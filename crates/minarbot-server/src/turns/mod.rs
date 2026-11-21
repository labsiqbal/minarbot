pub(crate) use admission::account as hub_account;
pub(crate) use admission::prepare as prepare_script;
pub(crate) use admission::route as script_route;
pub(crate) use history::{inspect, requested_files};
mod accounts;
pub(crate) mod admission;
mod compaction;
mod coordination;
pub(crate) mod diagnostics;
mod dispatch;
pub(crate) mod execution;
pub(crate) mod harness;
mod history;
pub(crate) use crate::media::images as media;
pub(crate) use memory::scoped as canonical_memory;
mod memory;
pub(crate) mod messaging;
pub mod options;
mod profile;
pub(crate) mod provider;
mod questions;
mod queue;
pub(crate) mod room;
mod shell;
pub(crate) mod stream;
pub(crate) mod tools;
pub(crate) use crate::media::transcription;
mod verification;
pub(crate) mod waits;
pub(crate) mod web;

use crate::{data, web::Failure, AppState};
use axum::{extract::Request, http::Method, response::Response, routing::any, Router};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::{watch, Notify};

use crate::data::{
    all, closed, error, exists, field, integer, iso, one, string, transaction, uuid,
};
type Result<T> = std::result::Result<T, Failure>;

struct Active {
    cancel: watch::Sender<bool>,
    started: watch::Sender<bool>,
}
#[derive(Clone, Default)]
pub struct Runtime {
    active: Arc<Mutex<HashMap<String, Active>>>,
    pub(crate) media: Arc<Mutex<HashMap<String, watch::Sender<bool>>>>,
    wake: Arc<Notify>,
    requested_pass: Arc<AtomicU64>,
    scheduled: watch::Sender<u64>,
    client: reqwest::Client,
    stopping: Arc<std::sync::atomic::AtomicBool>,
}
impl Runtime {
    pub fn new() -> std::result::Result<Self, String> {
        Ok(Self {
            active: Default::default(),
            media: Default::default(),
            wake: Default::default(),
            requested_pass: Default::default(),
            scheduled: Default::default(),
            stopping: Default::default(),
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| "Cannot initialize provider HTTP client.".to_owned())?,
        })
    }
    fn cancel(&self, turn: &str) {
        if let Ok(active) = self.active.lock() {
            if let Some(cancel) = active.get(turn) {
                cancel.cancel.send_replace(true);
            }
        }
    }
    pub(crate) fn mark_started(&self, turn: &str) {
        if let Ok(active) = self.active.lock() {
            if let Some(active) = active.get(turn) {
                active.started.send_replace(true);
            }
        }
    }
}

pub fn initialize_canonical(db: &Connection) -> Result<()> {
    db.execute_batch(include_str!("schema.sql"))?;
    verification::migrate(db)?;
    if !all(db, "PRAGMA table_info(queue_edits)", [])?
        .iter()
        .any(|column| column["name"] == "expires_at")
    {
        db.execute_batch(
            "ALTER TABLE queue_edits ADD COLUMN expires_at INTEGER NOT NULL DEFAULT 0",
        )?;
    }
    let columns = all(db, "PRAGMA table_info(transcriptions)", [])?;
    if !columns.iter().any(|column| column["name"] == "usage") {
        db.execute("ALTER TABLE transcriptions ADD COLUMN usage TEXT", [])?;
    }
    Ok(())
}

pub fn initialize(db: &Connection) -> Result<()> {
    initialize_canonical(db)?;
    transaction(db, || queue::expire(db))?;
    db.execute_batch(include_str!("indexes.sql"))?;
    db.execute_batch(
        "CREATE INDEX IF NOT EXISTS turns_released_routing ON turns(routing) WHERE status='queued' AND routing='released';
         CREATE INDEX IF NOT EXISTS turns_waiting_parent ON turns(waiting_for) WHERE status='queued' AND waiting_for IS NOT NULL;
         CREATE INDEX IF NOT EXISTS turns_queued_steer ON turns(dispatch_target) WHERE status='queued' AND dispatch_mode='steer';
         CREATE INDEX IF NOT EXISTS room_unresolved_invitations ON room_invitations(root) WHERE resolved IS NULL;
         CREATE INDEX IF NOT EXISTS room_messages_root ON room_messages(root);",
    )?;
    waits::initialize(db)?;
    db.execute_batch("UPDATE compaction_attempts SET status='interrupted' WHERE status='pending';UPDATE harness_approvals SET state='expired',revision=revision+1 WHERE state='approval';UPDATE transcriptions SET status='uncertain' WHERE status='pending';")?;
    db.execute("UPDATE turns SET status='interrupted',error='Server stopped during this turn. Check the result before resuming.',revision=revision+1 WHERE status='running'", [])?;
    db.execute("UPDATE provider_calls SET status='interrupted',finished=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE status='pending'", [])?;
    Ok(())
}

pub fn pending(state: &AppState) -> bool {
    state.trial_pending.load(Ordering::Acquire) || state.turns.stopping.load(Ordering::Acquire)
}
pub fn stop(state: &AppState) -> Result<()> {
    state.turns.stopping.store(true, Ordering::Release);
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    if state.auth.is_none() {
        transaction(db, || {
            db.execute("UPDATE turns SET status='interrupted',error='Server stopped during this turn. Check the result before resuming.',revision=revision+1 WHERE status='running'",[])?;
            db.execute(
                "UPDATE provider_calls SET status='interrupted',finished=? WHERE status='pending'",
                [iso(db, state.config.now())?],
            )?;
            Ok(())
        })?;
    }
    if let Ok(active) = state.turns.media.lock() {
        for cancel in active.values() {
            cancel.send_replace(true);
        }
    }
    if let Ok(active) = state.turns.active.lock() {
        for cancel in active.values() {
            cancel.cancel.send_replace(true);
        }
    }
    Ok(())
}

pub fn start(state: AppState) -> Result<tokio::task::JoinHandle<()>> {
    let reader = if state.auth.is_none() {
        Some(Connection::open_with_flags(
            &state.config.database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?)
    } else {
        None
    };
    Ok(tokio::spawn(async move {
        let Some(reader) = reader else {
            return;
        };
        let mut changes = state.events.subscribe();
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            if std::env::var("MINARBOT_TEST_SERVER").as_deref() == Ok("rust") {
                if let Ok(parent) = std::env::var("MINARBOT_TEST_PARENT") {
                    if parent
                        .parse()
                        .is_ok_and(|pid| !crate::platform::process_alive(pid))
                    {
                        std::process::exit(0);
                    }
                }
            }
            let pass = state.turns.requested_pass.load(Ordering::Acquire);
            while changes.try_recv().is_ok() {}
            match schedule(&state, &reader) {
                Ok(()) => {
                    state.turns.scheduled.send_replace(pass);
                }
                Err(error) => eprintln!("Turn scheduling failed: {}", error.message),
            }
            tokio::select! { _ = changes.recv() => (), _ = state.turns.wake.notified() => (), _ = timer.tick() => () }
        }
    }))
}
fn maintain(state: &AppState) -> Result<()> {
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    if pending(state) {
        return Ok(());
    }
    queue::expire(db)?;
    refresh_routing(db)?;
    waits::expire(db, state.config.now())?;
    messaging::settle(state, db)?;
    coordination::settle(db)?;
    Ok(())
}

fn capacity(state: &AppState) -> usize {
    state
        .config
        .data
        .concurrency
        .as_ref()
        .and_then(Value::as_u64)
        .unwrap_or(4) as usize
}

fn schedule(state: &AppState, reader: &Connection) -> Result<()> {
    maintain(state)?;
    if pending(state) {
        return Ok(());
    }
    for root in room::settlements(reader)? {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        if pending(state) {
            return Ok(());
        }
        room::settle_root(&guard.connection, &root)?;
    }
    let capacity = capacity(state);
    let reserved = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        if pending(state) {
            return Ok(());
        }
        for steer in all(db,"SELECT DISTINCT dispatch_target FROM turns WHERE dispatch_mode='steer' AND status='queued' AND NOT EXISTS(SELECT 1 FROM turns t WHERE t.id=turns.dispatch_target AND t.status='running')",[])? {
            db.execute("UPDATE turns SET dispatch_mode='queue',dispatch_target=NULL,dispatch_label='Queued after active turn',revision=revision+1 WHERE dispatch_mode='steer' AND dispatch_target=? AND status='queued'",[string(&steer,"dispatch_target")])?;
        }
        for approval in all(db,"SELECT id FROM harness_approvals WHERE state='approval' AND EXISTS(SELECT 1 FROM turns WHERE id=harness_approvals.turn AND status<>'running')",[])? {
            db.execute("UPDATE harness_approvals SET state='expired',revision=revision+1 WHERE id=?",[string(&approval,"id")])?;
        }
        let active = state.turns.active.lock().map_err(|_| Failure::storage())?;
        for (id, cancel) in active.iter() {
            if !exists(
                db,
                "SELECT 1 FROM turns WHERE id=? AND status IN('queued','running') AND (paused=0 OR EXISTS(SELECT 1 FROM composio_calls WHERE turn=turns.id AND state='approval'))",
                [id],
            )? {
                cancel.cancel.send_replace(true);
            }
        }
        if active.len() >= capacity {
            return Ok(());
        }
        active.keys().cloned().collect::<Vec<_>>()
    };

    // WAL lets queue traversal run while requests write. Recheck before reserving from that view.
    let version = data_version(reader)?;
    let snapshot = reader.unchecked_transaction()?;
    let selection = select(&snapshot, &reserved, capacity)?;
    snapshot.commit()?;
    if selection.is_empty() || pending(state) {
        return Ok(());
    }
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let mut active = state.turns.active.lock().map_err(|_| Failure::storage())?;
    let selection = transaction(&guard.connection, || {
        if pending(state) {
            return Ok(Vec::new());
        }
        let unchanged = data_version(reader)? == version
            && active.len() == reserved.len()
            && reserved.iter().all(|id| active.contains_key(id));
        // Running turns write often, so a stale snapshot is selected again under the lock
        // instead of retried; every pass admits from a consistent view.
        let selection = if unchanged {
            selection
        } else {
            let current = active.keys().cloned().collect::<Vec<_>>();
            select(&guard.connection, &current, capacity)?
        };
        for id in &selection {
            room::admit(&guard.connection, id)?;
        }
        Ok(selection)
    })?;
    for id in selection {
        run_reserved(state, &mut active, id);
    }
    Ok(())
}

fn data_version(db: &Connection) -> Result<i64> {
    Ok(db.query_row("PRAGMA data_version", [], |row| row.get(0))?)
}

fn select(db: &Connection, reserved: &[String], capacity: usize) -> Result<Vec<String>> {
    let mut reserved = reserved.to_vec();
    let mut selected = Vec::new();
    let queued = all(db, "SELECT t.id,t.agent,t.thread,t.channel,a.thread_limit FROM turns t JOIN agents a ON a.id=t.agent WHERE t.status='queued' AND t.paused=0 AND t.waiting_for IS NULL AND NOT EXISTS(SELECT 1 FROM turn_waits w WHERE w.turn=t.id AND w.status='waiting') AND NOT EXISTS(SELECT 1 FROM questions q WHERE q.turn=t.id AND q.answer IS NULL) AND NOT EXISTS(SELECT 1 FROM remote_agent_calls WHERE delivery=t.id) AND NOT EXISTS(SELECT 1 FROM agent_requests r WHERE r.turn=t.id AND r.expects_reply=1 AND r.settled=0 AND NOT EXISTS(SELECT 1 FROM delegation_tasks d WHERE d.request=r.id)) AND NOT EXISTS(SELECT 1 FROM delegation_waits w WHERE w.turn=t.id AND w.status='waiting') AND NOT EXISTS(SELECT 1 FROM delegation_tasks d WHERE d.creator_turn=t.id AND d.wait=1 AND d.wake_delivered=0) AND t.dispatch_mode<>'steer' AND coalesce(t.routing,'')<>'released' AND NOT EXISTS(SELECT 1 FROM agent_visibility v WHERE v.agent=t.agent AND v.hidden=1) AND NOT EXISTS(SELECT 1 FROM queue_edits e JOIN turns h ON h.id=e.turn WHERE e.state='held' AND e.expires_at>unixepoch('subsec')*1000 AND h.agent=t.agent AND h.thread IS t.thread AND coalesce((SELECT position FROM queue_positions WHERE turn=h.id),h.seq)<=coalesce((SELECT position FROM queue_positions WHERE turn=t.id),t.seq)) ORDER BY coalesce((SELECT position FROM queue_positions WHERE turn=t.id),t.seq)", [])?;
    let mut room_heads = HashMap::new();
    for row in queued {
        if reserved.len() >= capacity {
            break;
        }
        let id = string(&row, "id");
        if reserved.iter().any(|reserved| reserved == id) {
            continue;
        }
        if !room::can_run(db, id, row["channel"].as_str(), &mut room_heads)? {
            continue;
        }
        let agent = string(&row, "agent");
        let unavailable = json!(reserved).to_string();
        if exists(
            db,
            "SELECT 1 FROM turns WHERE agent=? AND thread IS ? AND(status='running' OR id IN(SELECT value FROM json_each(?)))",
            params![agent, row["thread"].as_str(), unavailable],
        )? {
            continue;
        }
        if !row["thread"].is_null() {
            let count:i64 = db.query_row("SELECT count(*) FROM turns WHERE agent=? AND thread IS NOT NULL AND(status='running' OR id IN(SELECT value FROM json_each(?)))", params![agent,unavailable], |r|r.get(0))?;
            if count >= row["thread_limit"].as_i64().unwrap_or(2) {
                continue;
            }
        }
        reserved.push(id.to_owned());
        selected.push(id.to_owned());
    }
    Ok(selected)
}

fn run_reserved(state: &AppState, active: &mut HashMap<String, Active>, id: String) {
    let (cancel, mut receiver) = watch::channel(false);
    let (started, _) = watch::channel(false);
    active.insert(id.clone(), Active { cancel, started });
    let task_state = state.clone();
    tokio::spawn(async move {
        let result = tokio::select! { result=execution::run(&task_state,&id) => result, _=receiver.changed()=>Err(error(409,"Turn stopped.")) };
        crate::harness::settle_process(&task_state, &id).await;
        if let Err(failure) = result {
            if let Ok(db) = task_state.database.lock() {
                if failure.message == "Turn stopped."
                    && exists(
                        &db.connection,
                        "SELECT 1 FROM computer_waits WHERE turn=?1
                         UNION ALL SELECT 1 FROM questions q JOIN turns t ON t.id=q.turn
                         WHERE q.turn=?1 AND t.status='queued'",
                        [&id],
                    )
                    .unwrap_or(false)
                {
                    if let Ok(mut active) = task_state.turns.active.lock() {
                        active.remove(&id);
                    }
                    task_state.turns.wake.notify_one();
                    return;
                }
                let status = if task_state.turns.stopping.load(Ordering::Acquire) {
                    "interrupted"
                } else {
                    "failed"
                };
                let _=db.connection.execute("UPDATE turns SET status=?,error=?,stopped_at=? WHERE id=? AND status IN('queued','running') AND paused=0 AND(status='running' OR ?='failed')",params![status,failure.message,iso(&db.connection,task_state.config.now()).ok(),id,status]);
                let _=db.connection.execute("UPDATE provider_calls SET status=?,finished=? WHERE turn=? AND status='pending'",params![status,iso(&db.connection,task_state.config.now()).ok(),id]);
            }
        }
        if let Ok(mut active) = task_state.turns.active.lock() {
            active.remove(&id);
        }
        task_state.turns.wake.notify_one();
    });
}

pub(crate) async fn start_queued(state: &AppState, id: &str) -> Result<()> {
    let mut scheduled = state.turns.scheduled.subscribe();
    let pass = state.turns.requested_pass.fetch_add(1, Ordering::AcqRel) + 1;
    state.turns.wake.notify_one();
    let initial = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let active = state.turns.active.lock().map_err(|_| Failure::storage())?;
        let started = active.get(id).map(|turn| turn.started.subscribe());
        if started.is_none()
            && (pending(state)
                || active.len() >= capacity(state)
                || !handoff_possible(&guard.connection, id, &active)?)
        {
            return Ok(());
        }
        started
    };

    // Wait for an eligible turn's existing one-second provider handoff.
    if let Ok(result) = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        let started = if initial.is_some() {
            initial
        } else {
            let _ = scheduled.wait_for(|completed| *completed >= pass).await;
            state
                .turns
                .active
                .lock()
                .map_err(|_| Failure::storage())?
                .get(id)
                .map(|turn| turn.started.subscribe())
        };
        if let Some(mut started) = started {
            if !*started.borrow_and_update() {
                let _ = started.changed().await;
            }
        }
        Ok::<_, Failure>(())
    })
    .await
    {
        result?;
    }
    Ok(())
}

fn handoff_possible(db: &Connection, id: &str, active: &HashMap<String, Active>) -> Result<bool> {
    let reserved = json!(active.keys().collect::<Vec<_>>()).to_string();
    exists(
        db,
        "SELECT 1 FROM turns t
        WHERE t.id=? AND t.status='queued' AND t.paused=0
        AND NOT EXISTS(
            SELECT 1 FROM json_each(?) slot JOIN turns running ON running.id=slot.value
            WHERE (running.agent=t.agent AND running.thread IS t.thread)
               OR (t.routing='room' AND running.channel=t.channel AND running.status='running'
                   AND running.dispatch_mode<>'steer'
                   AND (EXISTS(SELECT 1 FROM room_turns WHERE turn=running.id)
                        OR EXISTS(SELECT 1 FROM room_dispatches WHERE turn=running.id))
                   AND NOT EXISTS(SELECT 1 FROM agent_deliveries WHERE turn=running.id))
        )",
        params![id, reserved],
    )
}

pub(crate) fn refresh_routing(db: &Connection) -> Result<()> {
    for turn in all(db, "SELECT id FROM turns WHERE agent IN(SELECT agent FROM agent_visibility WHERE hidden=1) AND coalesce(routing,'') NOT IN('released','room') AND id NOT IN(SELECT turn FROM queue_edits WHERE state='held') AND channel IS NOT NULL AND parent IS NULL AND status='queued' AND paused=0 AND waiting_for IS NULL AND NOT EXISTS(SELECT 1 FROM provider_calls p WHERE p.turn=turns.id AND p.purpose='completion')", [])? {
        db.execute("UPDATE turns SET routing='released',revision=revision+1 WHERE id=?", [string(&turn,"id")])?;
    }
    for turn in all(db, "SELECT id,channel FROM turns WHERE routing='released' AND status='queued' AND id NOT IN(SELECT turn FROM queue_edits WHERE state='held')", [])? {
        let channel = data::conversations::channel(db, string(&turn, "channel"))?;
        if channel["archived"] == 1 {
            continue;
        }
        let available = all(db, "SELECT m.agent FROM channel_members m WHERE channel=? AND NOT EXISTS(SELECT 1 FROM agent_visibility v WHERE v.agent=m.agent AND v.hidden=1)", [string(&turn, "channel")])?;
        let owner = available.iter().find(|a| a["agent"] == channel["lead"]).or_else(|| (available.len() == 1).then(|| &available[0]));
        if let Some(owner) = owner {
            let owner = string(owner, "agent");
            let count: i64 = db.query_row("SELECT count(*) FROM turns WHERE agent=? AND id<>? AND status IN('queued','running')", [owner, string(&turn,"id")], |r| r.get(0))?;
            if count < 100 {
                db.execute("UPDATE turns SET agent=?,routing=?,revision=revision+1 WHERE id=?", params![owner, if available.len()>1 {"pending"}else{"assigned"},string(&turn,"id")])?;
            }
        }
    }
    Ok(())
}

pub fn handles(path: &str, method: &Method) -> bool {
    let p: Vec<_> = path.split('/').skip(1).collect();
    matches!(p.as_slice(), ["api", "turn-waits", _])
        || matches!(p.as_slice(),["api","agents"|"threads"|"channels",_,"turns"] if *method==Method::POST)
        || matches!(p.as_slice(),["api","channels",_,"discussions",_,"stop"] if *method==Method::POST)
        || matches!(p.as_slice(),["api","agents"|"threads"|"channels",_,"compaction"] if *method==Method::GET)
        || matches!(
            p.as_slice(),
            [
                "api",
                "agents" | "threads" | "channels",
                _,
                "turns",
                _,
                "cancel" | "pause" | "resume" | "reassign" | "dispatch" | "edit"
            ] | ["api", "agents" | "threads" | "channels", _, "queue"]
                | [
                    "api",
                    "agents" | "threads" | "channels",
                    _,
                    "turns",
                    _,
                    "harness-approvals",
                    _
                ]
                | [
                    "api",
                    "agents" | "threads" | "channels",
                    _,
                    "turns",
                    _,
                    "questions",
                    _,
                    "answer"
                ]
        )
}
pub async fn local(state: &AppState, request: Request) -> Result<Response> {
    if request.uri().path().starts_with("/api/turn-waits/") {
        return waits::local(state, request).await;
    }
    let path = request.uri().path().to_owned();
    let p: Vec<_> = path.split('/').skip(1).collect();
    let method = request.method().clone();
    let q = data::query(&request);
    let room_stop = matches!(
        p.as_slice(),
        ["api", "channels", _, "discussions", _, "stop"]
    );
    if room_stop
        && (request.headers().contains_key("authorization") || request.uri().query().is_some())
    {
        return Err(error(
            403,
            "Room controls require the local Owner in Minarbot.",
        ));
    }
    let v = if method == Method::POST {
        if !data::origin(state, request.headers())
            || crate::text_header(request.headers(), "content-type")
                .unwrap_or("")
                .split(';')
                .next()
                != Some("application/json")
        {
            return Err(error(403, "Request must originate from Minarbot."));
        }
        let bytes = axum::body::to_bytes(request.into_body(), 64000)
            .await
            .map_err(|_| error(413, "Request is too large."))?;
        let v: Value = serde_json::from_slice(&bytes).map_err(|_| error(400, "Invalid JSON."))?;
        if !v.is_object() {
            return Err(error(400, "JSON object is required."));
        }
        if !uuid(&field(&v, "id", 36)?) {
            return Err(error(400, "Invalid request ID."));
        }
        v
    } else {
        json!({})
    };
    admission::prepare(state, &p, &method, &v).await?;
    let (status, value) = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        transaction(db, || {
            if room_stop {
                if !closed(&v, &["id", "generation"]) || !v["generation"].as_str().is_some_and(uuid)
                {
                    return Err(error(400, "Invalid discussion generation."));
                }
                let channel = one(db, "SELECT archived FROM channels WHERE id=?", [p[2]])?
                    .ok_or_else(|| error(404, "Channel not found."))?;
                if channel["archived"] == 1 {
                    return Err(error(
                        409,
                        "Restore the channel before changing its discussion.",
                    ));
                }
                return Ok((
                    200,
                    room::stop(db, p[2], p[4], string(&v, "id"), string(&v, "generation"))?,
                ));
            }
            admission::route(state, db, &path, &p, &method, &q, &v)
        })?
    };
    if room_stop {
        if let Some(cancelled) = value["cancelled"].as_array() {
            for id in cancelled.iter().filter_map(Value::as_str) {
                state.turns.cancel(id);
            }
        }
    }
    if method == Method::POST {
        start_queued(state, string(&v, "id")).await?;
    }
    Ok(crate::json_response(status, value))
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/turn-waits/{id}", any(crate::dispatch))
        .route("/api/turn-waits/{id}/events", any(crate::dispatch))
        .route("/api/agents/{id}/compaction", any(crate::dispatch))
        .route("/api/threads/{id}/compaction", any(crate::dispatch))
        .route("/api/channels/{id}/compaction", any(crate::dispatch))
        .route(
            "/api/agents/{id}/turns/{turn}/harness-approvals/{approval}",
            any(crate::dispatch),
        )
        .route(
            "/api/threads/{id}/turns/{turn}/harness-approvals/{approval}",
            any(crate::dispatch),
        )
        .route(
            "/api/channels/{id}/turns/{turn}/harness-approvals/{approval}",
            any(crate::dispatch),
        )
        .route("/api/agents/{id}/turns", any(crate::dispatch))
        .route("/api/threads/{id}/turns", any(crate::dispatch))
        .route("/api/channels/{id}/turns", any(crate::dispatch))
        .route(
            "/api/channels/{id}/discussions/{root}/stop",
            any(crate::dispatch),
        )
        .route("/api/agents/{id}/turns/{turn}/cancel", any(crate::dispatch))
        .route(
            "/api/threads/{id}/turns/{turn}/cancel",
            any(crate::dispatch),
        )
        .route(
            "/api/channels/{id}/turns/{turn}/cancel",
            any(crate::dispatch),
        )
        .route("/api/agents/{id}/turns/{turn}/pause", any(crate::dispatch))
        .route("/api/threads/{id}/turns/{turn}/pause", any(crate::dispatch))
        .route(
            "/api/channels/{id}/turns/{turn}/pause",
            any(crate::dispatch),
        )
        .route("/api/agents/{id}/turns/{turn}/resume", any(crate::dispatch))
        .route(
            "/api/threads/{id}/turns/{turn}/resume",
            any(crate::dispatch),
        )
        .route(
            "/api/channels/{id}/turns/{turn}/resume",
            any(crate::dispatch),
        )
        .route(
            "/api/agents/{id}/turns/{turn}/reassign",
            any(crate::dispatch),
        )
        .route(
            "/api/threads/{id}/turns/{turn}/reassign",
            any(crate::dispatch),
        )
        .route(
            "/api/channels/{id}/turns/{turn}/reassign",
            any(crate::dispatch),
        )
        .route(
            "/api/agents/{id}/turns/{turn}/dispatch",
            any(crate::dispatch),
        )
        .route(
            "/api/threads/{id}/turns/{turn}/dispatch",
            any(crate::dispatch),
        )
        .route("/api/agents/{id}/turns/{turn}/edit", any(crate::dispatch))
        .route("/api/threads/{id}/turns/{turn}/edit", any(crate::dispatch))
        .route("/api/channels/{id}/turns/{turn}/edit", any(crate::dispatch))
        .route("/api/agents/{id}/queue", any(crate::dispatch))
        .route("/api/threads/{id}/queue", any(crate::dispatch))
        .route("/api/channels/{id}/queue", any(crate::dispatch))
        .route(
            "/api/agents/{id}/turns/{turn}/questions/{question}/answer",
            any(crate::dispatch),
        )
        .route(
            "/api/threads/{id}/turns/{turn}/questions/{question}/answer",
            any(crate::dispatch),
        )
        .route(
            "/api/channels/{id}/turns/{turn}/questions/{question}/answer",
            any(crate::dispatch),
        )
}

pub(crate) fn enqueue_automation(
    state: &AppState,
    db: &Connection,
    agent: &str,
    thread: &str,
    id: &str,
    text: &str,
) -> Result<()> {
    admission::account(state, db, agent)?;
    db.execute(
        "INSERT INTO turns(id,agent,thread,text,status,created) VALUES(?,?,?,?,'queued',?)",
        params![id, agent, thread, text, iso(db, state.config.now())?],
    )?;
    Ok(())
}
