mod jobs;
mod protocol;
mod store;

use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use rusqlite::OptionalExtension;
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::sync::Notify;

const MAX_MEDIA: usize = 10 * 1024 * 1024;
const PAIR_TIMEOUT: Duration = Duration::from_secs(180);
const TICK: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(crate) struct Runtime {
    wake: Notify,
    view: Mutex<View>,
}

#[derive(Default)]
struct View {
    generation: String,
    connected: bool,
    qr: Option<(String, Instant)>,
}

pub(super) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(include_str!("whatsapp/schema.sql"))?;
    db.execute("UPDATE whatsapp_outbox SET status='uncertain',error='Server restarted during send.' WHERE status='sending'", [])?;
    Ok(())
}

fn lock(state: &AppState) -> Result<std::sync::MutexGuard<'_, crate::db::Database>> {
    state.database.lock().map_err(|_| Failure::storage())
}

fn epoch_active(db: &Connection, generation: &str) -> Result<bool> {
    exists(db, "SELECT 1 FROM whatsapp_link WHERE singleton=1 AND generation=? AND state IN ('pairing','linked')", [generation])
}

fn link_row(db: &Connection) -> Result<Value> {
    one(
        db,
        "SELECT generation,state,started FROM whatsapp_link WHERE singleton=1",
        [],
    )?
    .ok_or_else(Failure::storage)
}

fn link_view(state: &AppState, db: &Connection) -> Result<Value> {
    let row = link_row(db)?;
    let mut view = state.whatsapp.view.lock().map_err(|_| Failure::storage())?;
    let same = view.generation == row["generation"];
    if view
        .qr
        .as_ref()
        .is_some_and(|(_, expires)| *expires <= Instant::now())
    {
        view.qr = None;
    }
    let qr = (same && row["state"] == "pairing")
        .then_some(view.qr.as_ref())
        .flatten()
        .map(|(image, _)| image);
    let chats = all(db, "SELECT chat,label,kind,target,transcribe_provider AS transcribeProvider FROM whatsapp_chats ORDER BY kind IS NULL,seen DESC LIMIT 100", [])?;
    let problems = all(db, "SELECT i.chat,o.status,o.error FROM whatsapp_outbox o JOIN whatsapp_inbox i ON i.id=o.input WHERE o.status IN ('failed','uncertain') ORDER BY o.rowid DESC LIMIT 10", [])?;
    let voice = crate::media::transcription::destination(state)?;
    Ok(
        json!({"state":row["state"],"connected":same && view.connected,"qr":qr,"chats":chats,"deliveryProblems":problems,"transcription":voice}),
    )
}

fn set_view(state: &AppState, generation: &str, change: impl FnOnce(&mut View)) -> Result<()> {
    let db = lock(state)?;
    if epoch_active(&db.connection, generation)? {
        let mut view = state.whatsapp.view.lock().map_err(|_| Failure::storage())?;
        if view.generation != generation {
            *view = View {
                generation: generation.into(),
                ..View::default()
            };
        }
        change(&mut view);
    }
    Ok(())
}

fn render_qr(code: &str) -> Result<String> {
    let image = qrcode::QrCode::new(code.as_bytes())
        .map_err(|_| Failure::storage())?
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(258, 258)
        .build();
    Ok(format!(
        "data:image/svg+xml;base64,{}",
        STANDARD.encode(image)
    ))
}

fn clear_link(db: &Connection, status: &str, generation: &str, now: i64) -> Result<()> {
    transaction(db, || {
        db.execute_batch("DELETE FROM whatsapp_prompts; DELETE FROM whatsapp_outbox; DELETE FROM whatsapp_inbox; DELETE FROM whatsapp_chats; DELETE FROM whatsapp_session;")?;
        db.execute(
            "UPDATE whatsapp_link SET generation=?,state=?,started=?,next_send=0 WHERE singleton=1",
            params![generation, status, now],
        )?;
        Ok(())
    })
}

pub(super) fn route(state: &AppState, path: &str, method: &Method, body: &Value) -> Result<Value> {
    let db = lock(state)?;
    let db = &db.connection;
    match (method, path) {
        (&Method::GET, "/api/whatsapp") => (),
        (&Method::POST, "/api/whatsapp/link") if closed(body, &["id"]) => {
            if !matches!(string(&link_row(db)?, "state"), "linked" | "pairing") {
                clear_link(db, "pairing", string(body, "id"), state.config.now())?;
                state.whatsapp.wake.notify_one();
            }
        }
        (&Method::POST, "/api/whatsapp/unlink") if closed(body, &["id"]) => {
            clear_link(db, "disconnected", "", 0)?;
            *state.whatsapp.view.lock().map_err(|_| Failure::storage())? = View::default();
            state.whatsapp.wake.notify_one();
            // Rotate the wrapping key so old database pages cannot restore a session.
            let key = minarbot_launcher::files::sidecar(&state.config.database, ".whatsapp-key");
            minarbot_launcher::files::remove(&key).map_err(|_| Failure::storage())?;
        }
        (&Method::POST, "/api/whatsapp/map")
            if closed(body, &["id", "chat", "kind", "target", "transcribe"]) =>
        {
            map_chat(state, db, body)?;
            state.whatsapp.wake.notify_one();
        }
        _ => return Err(error(400, "Invalid WhatsApp request.")),
    }
    link_view(state, db)
}

fn map_chat(state: &AppState, db: &Connection, body: &Value) -> Result<()> {
    let chat = string(body, "chat");
    let kind = string(body, "kind");
    let target = string(body, "target");
    if link_row(db)?["state"] != "linked"
        || !exists(db, "SELECT 1 FROM whatsapp_chats WHERE chat=?", [chat])?
    {
        return Err(error(409, "Choose a chat seen on the linked account."));
    }
    if body.get("kind") == Some(&Value::Null) && body.get("target") == Some(&Value::Null) {
        db.execute("UPDATE whatsapp_chats SET kind=NULL,target=NULL,revision=NULL,transcribe_provider=NULL WHERE chat=?", [chat])?;
        return Ok(());
    }
    if !uuid(target) || !matches!(kind, "agents" | "channels") || !body["transcribe"].is_boolean() {
        return Err(error(400, "Choose an agent or project section."));
    }
    jobs::target_agent(db, kind, target)?;
    let provider = if body["transcribe"] == true {
        Some(
            crate::media::transcription::destination(state)?["id"]
                .as_str()
                .ok_or_else(|| error(409, "Set up a transcription provider first."))?
                .to_owned(),
        )
    } else {
        None
    };
    let prior = one(
        db,
        "SELECT kind,target,transcribe_provider FROM whatsapp_chats WHERE chat=?",
        [chat],
    )?
    .ok_or_else(Failure::storage)?;
    if prior["kind"] != kind
        || prior["target"] != target
        || prior["transcribe_provider"] != json!(provider)
    {
        db.execute("UPDATE whatsapp_chats SET kind=?,target=?,revision=?,transcribe_provider=? WHERE chat=?", params![kind,target,body["id"].as_str(),provider,chat])?;
    }
    Ok(())
}

pub(crate) fn start(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if state.auth.is_none() {
            supervise(state, &protocol::Native).await;
        }
    })
}

async fn supervise(state: AppState, endpoint: &impl protocol::Endpoint) {
    loop {
        let row = lock(&state).and_then(|db| link_row(&db.connection));
        if let Ok(row) = row {
            let generation = string(&row, "generation");
            if matches!(string(&row, "state"), "pairing" | "linked")
                && !crate::updates::pending(&state)
            {
                match endpoint.open(state.clone(), generation).await {
                    Ok(mut session) => {
                        let mut timer = tokio::time::interval(TICK);
                        let unlink = loop {
                            let current = lock(&state).and_then(|db| link_row(&db.connection));
                            let Ok(current) = current else { break false };
                            if current["generation"] != generation
                                || current["state"] == "disconnected"
                            {
                                break true;
                            }
                            if current["state"] == "needs_relink" || crate::updates::pending(&state)
                            {
                                break false;
                            }
                            if current["state"] == "pairing"
                                && state
                                    .config
                                    .now()
                                    .saturating_sub(current["started"].as_i64().unwrap_or(0))
                                    >= PAIR_TIMEOUT.as_millis() as i64
                            {
                                let _ = invalidate(&state, generation);
                                break false;
                            }
                            if session.finished() {
                                break false;
                            }
                            if current["state"] == "linked" && session.connected() {
                                let _ = jobs::tick(&state, generation, session.as_ref()).await;
                            }
                            tokio::select! {
                                _ = state.whatsapp.wake.notified() => (),
                                _ = timer.tick() => (),
                            }
                        };
                        let _ = set_view(&state, generation, |view| {
                            view.connected = false;
                            view.qr = None;
                        });
                        session.close(unlink).await;
                    }
                    Err(_) => {
                        let _ = invalidate(&state, generation);
                    }
                }
            }
        }
        tokio::select! {
            _ = state.whatsapp.wake.notified() => (),
            _ = tokio::time::sleep(Duration::from_secs(5)) => (),
        }
    }
}

fn invalidate(state: &AppState, generation: &str) -> Result<()> {
    let db = lock(state)?;
    if epoch_active(&db.connection, generation)? {
        transaction(&db.connection, || {
            db.connection.execute(
                "UPDATE whatsapp_link SET state='needs_relink' WHERE singleton=1",
                [],
            )?;
            db.connection.execute("DELETE FROM whatsapp_session", [])?;
            db.connection.execute(
                "UPDATE whatsapp_inbox SET media=NULL WHERE media IS NOT NULL",
                [],
            )?;
            Ok(())
        })?;
        if let Ok(mut view) = state.whatsapp.view.lock() {
            *view = View::default();
        }
    }
    state.whatsapp.wake.notify_one();
    Ok(())
}
