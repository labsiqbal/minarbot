//! Phone push notifications: device registration and delivery through APNs
//! and FCM when the owner is needed or work ends.
//!
//! The watcher reads the same attention sources as the web client
//! (`conversation_attention`, finished turns) plus workflow run changes,
//! records each event key once in `push_events` and queues one delivery per
//! device in the durable `push_outbox`, so restarts neither repeat nor lose
//! pushes. Delivery runs in this background task, never on a request or turn.

mod config;
mod send;

use crate::{
    data::{self, all, closed, error, iso, one, string, transaction},
    json_response,
    web::Failure,
    AppState,
};
use axum::{
    extract::Request,
    http::{HeaderMap, Method},
    response::Response,
    routing::any,
    Router,
};
use rusqlite::{params, Connection};
use send::{Outcome, Sender, Target};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    sync::LazyLock,
    time::{Duration, Instant},
};
use tokio::sync::broadcast::error::RecvError;

type Result<T> = std::result::Result<T, Failure>;

static SENDER: LazyLock<Sender> = LazyLock::new(Sender::new);
const MAX_ATTEMPTS: i64 = 5;
const BACKOFF_MS: [i64; 4] = [5_000, 30_000, 120_000, 600_000];
const EVENT_RETENTION_MS: i64 = 30 * 86_400_000;

pub fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(include_str!("schema.sql"))?;
    Ok(())
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/push/devices", any(crate::dispatch))
        .route("/api/push/devices/{id}/delete", any(crate::dispatch))
        .route("/api/push/devices/{id}/test", any(crate::dispatch))
}

pub fn handles(path: &str) -> bool {
    path == "/api/push/devices" || path.starts_with("/api/push/devices/")
}

pub async fn local(state: &AppState, request: Request) -> Result<Response> {
    let headers = request.headers().clone();
    if headers.contains_key("authorization")
        || headers.contains_key("origin") && !data::origin(state, &headers)
    {
        return Err(error(
            403,
            "Push devices require the local owner in Minarbot.",
        ));
    }
    if request.uri().query().is_some() {
        return Err(error(400, "Push devices take no query parameters."));
    }
    let path = request.uri().path().to_owned();
    let parts: Vec<_> = path.split('/').skip(1).collect();
    let method = request.method().clone();
    let known = matches!(
        parts.as_slice(),
        ["api", "push", "devices"] | ["api", "push", "devices", _, "delete" | "test"]
    );
    if !known {
        return Err(error(404, "Route not found."));
    }
    let get = method == Method::GET && parts.len() == 3;
    if !get && method != Method::POST {
        return Err(error(405, "Method not allowed."));
    }
    if get {
        return Ok(json_response(200, list(state)?));
    }
    let body = command(state, &headers, request).await?;
    match parts.as_slice() {
        ["api", "push", "devices"] => {
            let (status, value) = register(state, &body)?;
            Ok(json_response(status, value))
        }
        ["api", "push", "devices", id, "delete"] => {
            if !closed(&body, &["id"]) {
                return Err(error(400, "Unknown field."));
            }
            remove(state, id)?;
            Ok(json_response(200, json!({"deleted": true})))
        }
        ["api", "push", "devices", id, _] => {
            if !closed(&body, &["id"]) {
                return Err(error(400, "Unknown field."));
            }
            test(state, id).await
        }
        _ => Err(error(404, "Route not found.")),
    }
}

/// A command POST: JSON object from the Minarbot origin with a UUID `id`.
async fn command(state: &AppState, headers: &HeaderMap, request: Request) -> Result<Value> {
    if !data::origin(state, headers)
        || crate::text_header(headers, "content-type")
            .unwrap_or("")
            .split(';')
            .next()
            != Some("application/json")
    {
        return Err(error(403, "Request must originate from Minarbot."));
    }
    let bytes = axum::body::to_bytes(request.into_body(), 16_000)
        .await
        .map_err(|_| error(413, "Request is too large."))?;
    let body: Value = serde_json::from_slice(&bytes).map_err(|_| error(400, "Invalid JSON."))?;
    if !body.is_object() {
        return Err(error(400, "JSON object is required."));
    }
    if !body["id"].as_str().is_some_and(data::uuid) {
        return Err(error(400, "Invalid request ID."));
    }
    Ok(body)
}

fn configured(state: &AppState) -> Value {
    let config = config::load(&state.config.database);
    json!({"ios": config.configured("ios"), "android": config.configured("android")})
}

fn public(row: &Value) -> Value {
    let token: String = string(row, "token").chars().take(8).collect();
    json!({"id": row["id"], "platform": row["platform"], "name": row["name"],
        "appId": row["app_id"], "environment": row["environment"], "token": format!("{token}…"),
        "created": row["created"], "updated": row["updated"],
        "lastSentAt": row["last_sent"], "lastError": row["last_error"]})
}

fn list(state: &AppState) -> Result<Value> {
    let devices = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        all(
            &guard.connection,
            "SELECT * FROM push_devices ORDER BY created,rowid",
            [],
        )?
    };
    Ok(json!({"configured": configured(state),
        "devices": devices.iter().map(public).collect::<Vec<_>>()}))
}

fn token_valid(platform: &str, token: &str) -> bool {
    if platform == "ios" {
        (32..=200).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_hexdigit())
    } else {
        (1..=4096).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_graphic())
    }
}

fn register(state: &AppState, v: &Value) -> Result<(u16, Value)> {
    if !closed(
        v,
        &["id", "platform", "token", "name", "appId", "environment"],
    ) {
        return Err(error(400, "Unknown device field."));
    }
    let platform = match v["platform"].as_str() {
        Some(platform @ ("ios" | "android")) => platform,
        _ => return Err(error(400, "Platform must be ios or android.")),
    };
    let token = v["token"]
        .as_str()
        .filter(|token| token_valid(platform, token))
        .ok_or_else(|| error(400, "Push token is invalid."))?;
    let name = v["name"]
        .as_str()
        .map(str::trim)
        .filter(|name| {
            !name.is_empty() && name.chars().count() <= 80 && !name.chars().any(char::is_control)
        })
        .ok_or_else(|| error(400, "Device name must be 1 to 80 characters."))?;
    let app = v["appId"]
        .as_str()
        .filter(|app| {
            (1..=155).contains(&app.len())
                && app
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        })
        .ok_or_else(|| error(400, "App ID is invalid."))?;
    let environment = match &v["environment"] {
        Value::Null => "production",
        Value::String(value) if value == "production" || value == "development" => value,
        _ => return Err(error(400, "Environment must be production or development.")),
    };
    let (status, row) = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        transaction(db, || {
            let now = iso(db, state.config.now())?;
            let existing = one(db, "SELECT id FROM push_devices WHERE token=?", [token])?;
            let (status, id) = match existing {
                Some(row) => {
                    let id = string(&row, "id").to_owned();
                    db.execute(
                        "UPDATE push_devices SET platform=?,name=?,app_id=?,environment=?,updated=? WHERE id=?",
                        params![platform, name, app, environment, now, id],
                    )?;
                    (200, id)
                }
                None => {
                    let id = uuid::Uuid::new_v4().to_string();
                    db.execute(
                        "INSERT INTO push_devices(id,platform,token,name,app_id,environment,created,updated) VALUES(?,?,?,?,?,?,?,?)",
                        params![id, platform, token, name, app, environment, now, now],
                    )?;
                    (201, id)
                }
            };
            let row = one(db, "SELECT * FROM push_devices WHERE id=?", [&id])?
                .ok_or_else(Failure::storage)?;
            Ok((status, row))
        })?
    };
    Ok((
        status,
        json!({"device": public(&row), "configured": configured(state)}),
    ))
}

fn forget_device(db: &Connection, id: &str) -> Result<usize> {
    db.execute("DELETE FROM push_outbox WHERE device=?", [id])?;
    Ok(db.execute("DELETE FROM push_devices WHERE id=?", [id])?)
}

fn remove(state: &AppState, id: &str) -> Result<()> {
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    if transaction(db, || forget_device(db, id))? == 0 {
        return Err(error(404, "Device not found."));
    }
    Ok(())
}

fn platform_name(platform: &str) -> &'static str {
    if platform == "ios" {
        "iOS"
    } else {
        "Android"
    }
}

async fn test(state: &AppState, id: &str) -> Result<Response> {
    let device = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        one(
            &guard.connection,
            "SELECT * FROM push_devices WHERE id=?",
            [id],
        )?
    }
    .ok_or_else(|| error(404, "Device not found."))?;
    let platform = string(&device, "platform");
    let config = config::load(&state.config.database);
    if !config.configured(platform) {
        return Err(error(
            409,
            format!("Push is not configured for {}.", platform_name(platform)),
        ));
    }
    let payload = json!({"title": "Minarbot", "body": "Test notification", "tag": "test",
        "data": {"kind": "test", "path": "/"}});
    let outcome = SENDER.send(&config, &target(&device), &payload).await;
    {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        transaction(db, || record(state, db, id, &outcome))?;
    }
    match outcome {
        Outcome::Sent => Ok(json_response(200, json!({"result": "sent"}))),
        Outcome::Gone => Ok(json_response(200, json!({"result": "removed"}))),
        Outcome::Retry(message) | Outcome::Failed(message) => Err(error(502, message)),
        Outcome::NotConfigured => Err(error(
            409,
            format!("Push is not configured for {}.", platform_name(platform)),
        )),
    }
}

fn target(device: &Value) -> Target<'_> {
    Target {
        platform: string(device, "platform"),
        token: string(device, "token"),
        app_id: string(device, "app_id"),
        development: device["environment"] == "development",
    }
}

/// Applies a final outcome to the device row.
fn record(state: &AppState, db: &Connection, device: &str, outcome: &Outcome) -> Result<()> {
    match outcome {
        Outcome::Sent => {
            db.execute(
                "UPDATE push_devices SET last_sent=?,last_error=NULL WHERE id=?",
                params![iso(db, state.config.now())?, device],
            )?;
        }
        Outcome::Gone => {
            forget_device(db, device)?;
        }
        Outcome::Retry(message) | Outcome::Failed(message) => {
            db.execute(
                "UPDATE push_devices SET last_error=? WHERE id=?",
                params![message, device],
            )?;
        }
        Outcome::NotConfigured => {
            db.execute(
                "UPDATE push_devices SET last_error='Push is not configured.' WHERE id=?",
                [device],
            )?;
        }
    }
    Ok(())
}

pub fn start(state: AppState) -> Option<tokio::task::JoinHandle<()>> {
    if state.auth.is_some() {
        return None;
    }
    Some(tokio::spawn(async move {
        let mut changes = state.events.subscribe();
        let mut timer = tokio::time::interval(Duration::from_secs(2));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut pruned: Option<Instant> = None;
        let mut changed = true;
        loop {
            if changed {
                if let Err(e) = collect(&state) {
                    eprintln!("Push event collection failed: {}", e.message);
                }
            }
            if pruned.is_none_or(|at| at.elapsed() > Duration::from_secs(3600)) {
                pruned = Some(Instant::now());
                if let Err(e) = prune(&state) {
                    eprintln!("Push event pruning failed: {}", e.message);
                }
            }
            if let Err(e) = deliver(&state).await {
                eprintln!("Push delivery failed: {}", e.message);
            }
            changed = tokio::select! {
                change = changes.recv() => match change {
                    Ok(scope) => scope == "database",
                    Err(RecvError::Lagged(_)) => true,
                    Err(RecvError::Closed) => break,
                },
                _ = timer.tick() => false,
            };
            if changed {
                tokio::time::sleep(Duration::from_millis(150)).await;
                while changes.try_recv().is_ok() {}
            }
        }
    }))
}

const PENDING: &str = "SELECT a.agent,a.thread,a.channel,a.turn,a.focus,a.kind,a.request,g.name
FROM conversation_attention a JOIN agents g ON g.id=a.agent
WHERE NOT EXISTS(SELECT 1 FROM push_events e WHERE e.key=a.kind||':'||a.turn||':'||a.request)
ORDER BY a.seq,a.kind,a.request LIMIT 200";

/// Finished owner-level turns (not delegated children or workflow steps),
/// with the same visibility rules as the sidebar's attention snapshot.
const FINISHED: &str = "SELECT coalesce(th.agent,t.agent) AS agent,t.thread,t.channel,t.id AS turn,
  coalesce(r.root,t.id) AS focus,t.status,g.name
FROM turns t LEFT JOIN threads th ON th.id=t.thread LEFT JOIN channels c ON c.id=t.channel
LEFT JOIN room_turns r ON r.turn=t.id
LEFT JOIN agent_visibility v ON v.agent=coalesce(th.agent,t.agent)
JOIN agents g ON g.id=coalesce(th.agent,t.agent)
WHERE t.attention_finished_at>=unixepoch()-120 AND t.status IN('done','failed')
  AND t.parent IS NULL AND t.workflow_run IS NULL
  AND coalesce(th.deleted,0)=0 AND coalesce(th.archived,0)=0 AND coalesce(c.archived,0)=0
  AND (t.channel IS NOT NULL OR coalesce(v.hidden,0)=0)
  AND NOT EXISTS(SELECT 1 FROM push_events e WHERE e.key='finished:'||t.id)
ORDER BY t.seq LIMIT 200";

const WORKFLOWS: &str = "SELECT c.seq,c.run,c.status,c.steps,r.agent,w.name
FROM push_workflow_changes c LEFT JOIN workflow_runs r ON r.id=c.run
LEFT JOIN workflows w ON w.id=r.workflow ORDER BY c.seq LIMIT 200";

struct Event {
    key: String,
    /// Several requests of one turn at once make one push.
    group: String,
    payload: Value,
}

fn title(name: &str) -> String {
    let clean: String = name.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim();
    if clean.chars().count() > 60 {
        format!("{}…", clean.chars().take(59).collect::<String>())
    } else if clean.is_empty() {
        "Minarbot".to_owned()
    } else {
        clean.to_owned()
    }
}

fn tag(key: &str) -> String {
    data::digest_bytes(key.as_bytes())[..32].to_owned()
}

/// A conversation event: agent name, a short reason and the ids that open it.
fn conversation(row: &Value, key: String, kind: &str, body: &str) -> Event {
    let agent = string(row, "agent");
    let turn = string(row, "turn");
    let mut data = json!({"kind": kind, "agent": agent, "turn": turn,
        "focus": string(row, "focus")});
    let path = if let Some(channel) = row["channel"].as_str() {
        data["channel"] = json!(channel);
        format!("/rooms/{channel}")
    } else if let Some(thread) = row["thread"].as_str() {
        data["thread"] = json!(thread);
        format!("/agents/{agent}/conversations/{thread}")
    } else {
        format!("/agents/{agent}")
    };
    data["path"] = json!(path);
    let group = row["channel"]
        .as_str()
        .or(row["thread"].as_str())
        .unwrap_or(agent);
    Event {
        payload: json!({"title": title(string(row, "name")), "body": body, "group": group,
            "tag": tag(&key), "data": data}),
        group: format!("turn:{turn}"),
        key,
    }
}

fn pending(row: &Value) -> Event {
    let key = format!(
        "{}:{}:{}",
        string(row, "kind"),
        string(row, "turn"),
        string(row, "request")
    );
    let (kind, body) = match string(row, "kind") {
        "question" => ("question", "Has a question for you"),
        "computer" => ("computer", "Needs you at its computer"),
        _ => ("approval", "Needs your approval"),
    };
    conversation(row, key, kind, body)
}

fn finished(row: &Value) -> Event {
    let key = format!("finished:{}", string(row, "turn"));
    let (kind, body) = if row["status"] == "failed" {
        ("failed", "Failed")
    } else {
        ("finished", "Finished")
    };
    conversation(row, key, kind, body)
}

fn workflow(row: &Value) -> Option<Event> {
    let run = string(row, "run");
    let (kind, body) = match string(row, "status") {
        "approval" => ("workflow-approval", "Workflow needs your approval"),
        "failed" => ("workflow-failed", "Workflow failed"),
        "done" => ("workflow-done", "Workflow finished"),
        _ => return None,
    };
    row["name"].as_str()?;
    let key = format!("{kind}:{run}:{}", row["steps"]);
    Some(Event {
        payload: json!({"title": title(string(row, "name")), "body": body, "group": run,
            "tag": tag(&key), "data": {"kind": kind, "agent": string(row, "agent"),
            "workflowRun": run, "path": "/workflows"}}),
        group: key.clone(),
        key,
    })
}

/// Records new attention events and queues one delivery per device whose
/// platform is configured. Without configuration the keys are still
/// recorded, so configuring push later does not replay old events.
fn collect(state: &AppState) -> Result<()> {
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    let mut events: Vec<_> = all(db, PENDING, [])?.iter().map(pending).collect();
    events.extend(all(db, FINISHED, [])?.iter().map(finished));
    let changes = all(db, WORKFLOWS, [])?;
    events.extend(changes.iter().filter_map(workflow));
    let last = changes.last().and_then(|row| row["seq"].as_i64());
    if events.is_empty() && last.is_none() {
        return Ok(());
    }
    let config = if events.is_empty() {
        config::Push::default()
    } else {
        config::load(&state.config.database)
    };
    let now = crate::config::now();
    transaction(db, || {
        if let Some(last) = last {
            db.execute("DELETE FROM push_workflow_changes WHERE seq<=?", [last])?;
        }
        let devices = all(db, "SELECT id,platform FROM push_devices", [])?;
        let mut groups = HashSet::new();
        for event in &events {
            let new = db.execute(
                "INSERT OR IGNORE INTO push_events(key,created) VALUES(?,?)",
                params![event.key, now],
            )? == 1;
            if !new || !groups.insert(event.group.clone()) {
                continue;
            }
            for device in &devices {
                if config.configured(string(device, "platform")) {
                    db.execute(
                        "INSERT OR IGNORE INTO push_outbox(event,device,payload,next_at) VALUES(?,?,?,?)",
                        params![event.key, string(device, "id"), event.payload.to_string(), now],
                    )?;
                }
            }
        }
        Ok(())
    })
}

fn prune(state: &AppState) -> Result<()> {
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    guard.connection.execute(
        "DELETE FROM push_events WHERE created<?",
        [crate::config::now() - EVENT_RETENTION_MS],
    )?;
    Ok(())
}

/// Sends due deliveries outside the database lock and settles each one.
async fn deliver(state: &AppState) -> Result<()> {
    for _ in 0..10 {
        let due = {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            all(
                &guard.connection,
                "SELECT o.seq,o.payload,o.attempts,d.id,d.platform,d.token,d.app_id,d.environment
                 FROM push_outbox o JOIN push_devices d ON d.id=o.device
                 WHERE o.next_at<=? ORDER BY o.next_at,o.seq LIMIT 20",
                [crate::config::now()],
            )?
        };
        if due.is_empty() {
            return Ok(());
        }
        let config = config::load(&state.config.database);
        for row in &due {
            let outcome = match data::json(&row["payload"]) {
                Ok(payload) => SENDER.send(&config, &target(row), &payload).await,
                Err(_) => Outcome::Failed("Stored push payload is invalid.".into()),
            };
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            let db = &guard.connection;
            transaction(db, || settle(state, db, row, &outcome))?;
        }
    }
    Ok(())
}

fn settle(state: &AppState, db: &Connection, row: &Value, outcome: &Outcome) -> Result<()> {
    let seq = row["seq"].as_i64().unwrap_or(0);
    let attempts = row["attempts"].as_i64().unwrap_or(0) + 1;
    let device = string(row, "id");
    if let Outcome::Retry(message) = outcome {
        if attempts < MAX_ATTEMPTS {
            let delay = BACKOFF_MS[(attempts as usize - 1).min(BACKOFF_MS.len() - 1)];
            db.execute(
                "UPDATE push_outbox SET attempts=?,next_at=? WHERE seq=?",
                params![attempts, crate::config::now() + delay, seq],
            )?;
            db.execute(
                "UPDATE push_devices SET last_error=? WHERE id=?",
                params![message, device],
            )?;
            return Ok(());
        }
    }
    db.execute("DELETE FROM push_outbox WHERE seq=?", [seq])?;
    record(state, db, device, outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_payloads_carry_ids_and_short_reasons_only() {
        let row = json!({"agent": "a", "thread": "t", "channel": null, "turn": "u", "focus": "u",
            "kind": "question", "request": "q", "name": "Ada\u{7}"});
        let event = pending(&row);
        assert_eq!(event.key, "question:u:q");
        assert_eq!(event.group, "turn:u");
        assert_eq!(
            event.payload,
            json!({"title": "Ada", "body": "Has a question for you", "group": "t",
                "tag": tag("question:u:q"), "data": {"kind": "question", "agent": "a",
                "turn": "u", "focus": "u", "thread": "t", "path": "/agents/a/conversations/t"}})
        );
        let room = json!({"agent": "a", "thread": null, "channel": "c", "turn": "u",
            "focus": "r", "status": "failed", "name": "x".repeat(80)});
        let event = finished(&room);
        assert_eq!(event.key, "finished:u");
        assert_eq!(event.payload["body"], "Failed");
        assert_eq!(event.payload["data"]["path"], "/rooms/c");
        assert_eq!(
            event.payload["title"].as_str().map(|t| t.chars().count()),
            Some(60)
        );
        let run = json!({"seq": 1, "run": "r", "status": "approval", "steps": 3, "agent": "a",
            "name": "Daily"});
        let event = workflow(&run);
        assert_eq!(
            event.as_ref().map(|e| e.key.as_str()),
            Some("workflow-approval:r:3")
        );
        assert_eq!(
            event.map(|e| e.payload["data"].clone()),
            Some(
                json!({"kind": "workflow-approval", "agent": "a", "workflowRun": "r",
                "path": "/workflows"})
            )
        );
        assert!(workflow(&json!({"run": "r", "status": "done", "name": null})).is_none());
    }

    #[test]
    fn tokens_are_validated_per_platform() {
        assert!(token_valid("ios", &"ab".repeat(32)));
        assert!(!token_valid("ios", "zz"));
        assert!(!token_valid("ios", &"a".repeat(201)));
        assert!(token_valid("android", "fcm:APA91b-x_y"));
        assert!(!token_valid("android", "has space"));
        assert!(!token_valid("android", &"a".repeat(4097)));
        assert!(!token_valid("android", ""));
    }
}
