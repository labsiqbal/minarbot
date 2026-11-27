pub(crate) mod access;
pub(crate) mod agent_folders;
pub(crate) mod agents;
pub(crate) mod avatar;
pub(crate) mod conversations;
mod deletion;
pub(crate) mod delivery;
pub(crate) mod device_pairing;
pub(crate) mod environment;
mod folders;
pub(crate) mod history;
pub(crate) mod limits;
#[path = "harness/logins.rs"]
pub(crate) mod logins;
pub(crate) mod memory;
pub mod options;
mod panel;
pub(crate) mod projects;
pub(crate) mod provider_instances;
mod provider_sign_in;
mod provider_store;
pub(crate) mod providers;
mod schema;
pub(crate) mod sidebar;
pub(crate) mod sync;
pub(crate) use sync::append_device;
mod sync_projection;
pub(crate) mod wire;

use crate::{json_response, web::Failure, AppState};
use axum::{
    extract::Request,
    http::{HeaderMap, Method},
    response::Response,
    routing::any,
    Router,
};
pub(crate) use history::partial;
pub use providers::Options;
use rusqlite::{types::ValueRef, Connection, Params};
pub use schema::{initialize, initialize_canonical};
use serde_json::{json, Value};
pub(crate) use sync::append_mutation;

pub fn maintenance(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        eprintln!("data maintenance start");
        let period = std::time::Duration::from_secs(if state.auth.is_some() { 5 } else { 60 });
        let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        loop {
            timer.tick().await;
            if crate::updates::pending(&state) {
                continue;
            }
            if state.auth.is_some() {
                if sync::maintain(&state).is_err() {
                    eprintln!("Sync maintenance failed");
                }
                continue;
            }
            let result = state
                .database
                .lock()
                .map_err(|_| Failure::storage())
                .and_then(|db| {
                    transaction(&db.connection, || {
                        delivery::maintain(&db.connection, state.config.now())
                    })
                });
            if result.is_err() {
                eprintln!("Conversation maintenance failed");
            }
        }
    })
}

type Result<T> = std::result::Result<T, Failure>;
pub(crate) fn digest_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn normalize_email(v: &Value) -> Result<String> {
    use unicode_normalization::UnicodeNormalization;
    let fail = || Failure::new(400, "invalid_email", "Invalid email address.");
    let raw = v
        .as_str()
        .filter(|s| s.len() <= 254 && !s.chars().any(|c| c < ' ' || c == '\u{7f}'))
        .ok_or_else(fail)?;
    let normalized = raw.nfc().collect::<String>();
    let (local, domain) = normalized.trim().split_once('@').ok_or_else(fail)?;
    if local.is_empty()
        || local.len() > 64
        || local.starts_with('.')
        || local.ends_with('.')
        || local.contains("..")
        || local
            .chars()
            .any(|c| c.is_whitespace() || "<>(),;:\"\\".contains(c))
    {
        return Err(fail());
    }
    let domain = idna::domain_to_ascii(domain)
        .map_err(|_| fail())?
        .to_lowercase();
    if domain.is_empty()
        || domain.split('.').any(|part| {
            part.is_empty()
                || part.len() > 63
                || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                || part.starts_with('-')
                || part.ends_with('-')
        })
    {
        return Err(fail());
    }
    let result = format!("{}@{domain}", local.to_lowercase());
    if result.len() > 254 {
        return Err(fail());
    }
    Ok(result)
}
fn image_header(hex: &str) -> bool {
    hex.len() >= 48 && hex.starts_with("89504E470D0A1A0A") && hex.get(24..32) == Some("49484452")
        || hex.len() >= 8 && hex.starts_with("FFD8FF")
        || hex.len() >= 32
            && hex.starts_with("52494646")
            && hex.get(16..24) == Some("57454250")
            && matches!(hex.get(24..32), Some("56503820" | "5650384C" | "56503858"))
}
pub(crate) fn error(status: u16, message: impl Into<String>) -> Failure {
    Failure::new(status, "invalid_request", message)
}
pub(crate) fn string<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
pub(crate) fn field(v: &Value, key: &str, max: usize) -> Result<String> {
    let s = v[key]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.encode_utf16().count() <= max)
        .ok_or_else(|| {
            error(
                400,
                format!("Field {key} is invalid (maximum {max} characters)."),
            )
        })?;
    Ok(s.trim().to_owned())
}
pub(crate) fn uuid(value: &str) -> bool {
    crate::web::id(value).is_some()
}
pub(crate) fn integer(v: &Value, min: i64, max: i64) -> bool {
    v.as_i64().is_some_and(|n| (min..=max).contains(&n))
}
pub(crate) fn closed(v: &Value, keys: &[&str]) -> bool {
    v.as_object()
        .is_some_and(|m| m.keys().all(|k| keys.contains(&k.as_str())))
}
pub(crate) fn all(db: &Connection, sql: &str, params: impl Params) -> Result<Vec<Value>> {
    let mut statement = db.prepare_cached(sql)?;
    let names: Vec<_> = statement
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect();
    let rows = statement
        .query_map(params, |row| {
            let mut out = serde_json::Map::new();
            for (index, name) in names.iter().enumerate() {
                let value = match row.get_ref(index)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(v) => json!(v),
                    ValueRef::Real(v) => json!(v),
                    ValueRef::Text(v) => json!(String::from_utf8_lossy(v)),
                    ValueRef::Blob(_) => {
                        return Err(rusqlite::Error::InvalidColumnType(
                            index,
                            name.clone(),
                            rusqlite::types::Type::Blob,
                        ))
                    }
                };
                out.insert(name.clone(), value);
            }
            Ok(Value::Object(out))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}
pub(crate) fn one(db: &Connection, sql: &str, params: impl Params) -> Result<Option<Value>> {
    Ok(all(db, sql, params)?.into_iter().next())
}
pub(crate) fn exists(db: &Connection, sql: &str, params: impl Params) -> Result<bool> {
    Ok(one(db, sql, params)?.is_some())
}
pub(crate) fn json(raw: &Value) -> Result<Value> {
    serde_json::from_str(raw.as_str().unwrap_or("")).map_err(|_| Failure::storage())
}
pub(crate) fn receipt(
    db: &Connection,
    table: &str,
    id: &str,
    request: &Value,
    message: &str,
) -> Result<Option<Value>> {
    if let Some(row) = one(
        db,
        &format!("SELECT request,result FROM {table} WHERE id=?"),
        [id],
    )? {
        if json(&row["request"])? != *request {
            return Err(error(409, message));
        }
        return Ok(Some(json(&row["result"])?));
    }
    Ok(None)
}
pub(crate) fn save_receipt(
    db: &Connection,
    table: &str,
    id: &str,
    request: &Value,
    result: &Value,
) -> Result<()> {
    db.execute(
        &format!("INSERT INTO {table}(id,request,result) VALUES(?,?,?)"),
        rusqlite::params![id, request.to_string(), result.to_string()],
    )?;
    Ok(())
}
pub(crate) fn table(db: &Connection, name: &str) -> Result<bool> {
    exists(
        db,
        "SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?",
        [name],
    )
}
pub(crate) fn iso(db: &Connection, now: i64) -> Result<String> {
    db.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%fZ',?/1000.0,'unixepoch')",
        [now],
        |r| r.get(0),
    )
    .map_err(Into::into)
}
pub(crate) fn transaction<T>(db: &Connection, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let transaction =
        rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)?;
    let value = f()?;
    transaction.commit()?;
    Ok(value)
}
pub(crate) fn query(request: &Request) -> Vec<(String, String)> {
    url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
        .into_owned()
        .collect()
}
pub(crate) fn param<'a>(q: &'a [(String, String)], name: &str) -> Option<&'a str> {
    q.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}
pub(crate) fn origin(state: &AppState, headers: &HeaderMap) -> bool {
    crate::allowed(state, crate::text_header(headers, "origin"))
}

pub fn handles(path: &str) -> bool {
    let parts: Vec<_> = path.split('/').skip(1).collect();
    matches!(
        parts.as_slice(),
        [
            "api",
            "agents"
                | "projects"
                | "channels"
                | "project-folders"
                | "sidebar"
                | "usage"
                | "provider-instances"
        ] | [
            "api",
            "provider-instances",
            _,
            "edit"
                | "update"
                | "delete"
                | "refresh"
                | "sign-in"
                | "signin"
                | "sign-in-code"
                | "models"
        ] | ["api", "provider-instances", "detected"]
            | ["api", "sidebar", "summary"]
            | ["api", "conversations", "settings"]
            | ["api", "settings", "library-docs"]
            | ["api", "settings", "web-search"]
            | ["api", "settings", "web-search", "test"]
            | ["api", "threads", _]
            | ["api", "projects", _]
            | [
                "api",
                "projects",
                _,
                "edit" | "archive" | "restore" | "delete" | "thread" | "sections"
            ]
            | ["api", "projects", _, "environment"]
            | ["api", "projects", _, "environment", "remove" | "settings"]
            | ["api", "projects", _, "sections", _, "edit"]
            | [
                "api",
                "agents",
                _,
                "edit"
                    | "delete"
                    | "folder"
                    | "visibility"
                    | "template"
                    | "avatar"
                    | "threads"
                    | "memory"
                    | "panel"
                    | "scripts"
            ]
            | ["api", "threads", _, "panel"]
            | ["api", "turns", _, "inspect"]
            | ["api", "channels", _, "panel"]
            | [
                "api",
                "agents" | "threads" | "channels",
                _,
                "panel",
                "files"
            ]
            | ["api", "agents", _, "avatar", "image"]
            | ["api", "agents", _, "folder", "reset"]
            | ["api", "threads", _, "edit" | "delete" | "fork-context"]
            | ["api", "channels", _, "memory" | "edit" | "read"]
            | ["api", "agents" | "channels", _, "history", "search"]
            | [
                "api",
                "agents" | "channels",
                _,
                "memory",
                "knowledge" | "questions"
            ]
            | ["api", "channels", _, "turns"]
            | [
                "api",
                "agents" | "threads" | "channels",
                _,
                "turns" | "dispatch"
            ]
            | [
                "api",
                "agents" | "threads" | "channels",
                _,
                "turns",
                _,
                "fork"
            ]
    )
}
pub async fn local(state: &AppState, request: Request) -> Result<Response> {
    let path = request.uri().path().to_owned();
    let parts: Vec<_> = path.split('/').skip(1).collect();
    let method = request.method().clone();
    let q = query(&request);
    let headers = request.headers().clone();
    if let ["api", kind @ ("agents" | "channels"), id, "history", "search"] = parts.as_slice() {
        if headers.contains_key("authorization")
            || headers.contains_key("origin") && !origin(state, &headers)
        {
            return Err(error(
                403,
                "History search requires the local owner in Minarbot.",
            ));
        }
        if method != Method::GET {
            return Err(error(405, "Use GET for history search."));
        }
        return history::search_sessions(state, kind, id, &q).await;
    }
    if let ["api", "turns", id, "inspect"] = parts.as_slice() {
        return crate::turns::inspect(state, id, &method, &headers, &q);
    }
    if parts.get(3) == Some(&"panel") {
        if headers.contains_key("authorization")
            || headers.contains_key("origin") && !origin(state, &headers)
        {
            return Err(error(
                403,
                "Conversation panel requires the local owner in Minarbot.",
            ));
        }
        if method != Method::GET {
            return Err(error(405, "Use GET for panel reads."));
        }
    }
    let mut body = json!({});
    if method == Method::POST {
        if !origin(state, &headers)
            || crate::text_header(&headers, "content-type")
                .unwrap_or("")
                .split(';')
                .next()
                != Some("application/json")
        {
            return Err(error(403, "Request must originate from Minarbot."));
        }
        let limit = if path == "/api/agents" {
            750000
        } else if path.ends_with("/avatar") {
            701000
        } else {
            64000
        };
        let bytes = axum::body::to_bytes(request.into_body(), limit)
            .await
            .map_err(|_| error(413, "Request is too large."))?;
        body = serde_json::from_slice(&bytes).map_err(|_| error(400, "Invalid JSON."))?;
        if !body.is_object() {
            return Err(error(400, "JSON object is required."));
        }
        let id = field(&body, "id", 36)?;
        if !uuid(&id) {
            return Err(error(400, "Invalid request ID."));
        }
    }
    if parts.get(1) == Some(&"provider-instances") {
        if headers.contains_key("authorization")
            || headers.contains_key("origin") && !origin(state, &headers)
        {
            return Err(error(
                403,
                "Provider instances require the local owner in Minarbot.",
            ));
        }
        return provider_instances::route(state, &parts, &method, &q, &body).await;
    }
    if path == "/api/settings/library-docs" {
        if headers.contains_key("authorization")
            || headers.contains_key("origin") && !origin(state, &headers)
        {
            return Err(error(
                403,
                "Library docs settings require the local owner in Minarbot.",
            ));
        }
        if method != Method::GET || !q.is_empty() {
            return Err(error(
                400,
                "Use GET with no parameters for library docs status.",
            ));
        }
        return crate::turns::library_docs::settings(state)
            .await
            .map(|value| json_response(200, value));
    }
    if matches!(
        parts.as_slice(),
        ["api", "settings", "web-search"] | ["api", "settings", "web-search", "test"]
    ) {
        if headers.contains_key("authorization")
            || headers.contains_key("origin") && !origin(state, &headers)
        {
            return Err(error(
                403,
                "Web search settings require the local owner in Minarbot.",
            ));
        }
        if !q.is_empty() {
            return Err(error(
                400,
                "Web search settings do not accept query parameters.",
            ));
        }
        let test = parts.last() == Some(&"test");
        return crate::turns::web::settings(state, method, test, &body)
            .await
            .map(|value| json_response(200, value));
    }
    let write = method == Method::POST;
    let project = parts
        .get(1)
        .is_some_and(|v| matches!(*v, "projects" | "project-folders"));
    let result = avatar::run(state, |db, avatar| {
        if write && !project {
            transaction(db, || {
                route(state, db, &parts, &method, &q, &headers, &body, avatar)
            })
        } else {
            route(state, db, &parts, &method, &q, &headers, &body, avatar)
        }
    })
    .await?;
    if write && matches!(parts.as_slice(), ["api", "projects", _, "delete"]) {
        crate::connectors::composio::remove_project(state, parts[2]).await?;
    }
    if write && matches!(parts.as_slice(), ["api", "agents", _, "scripts" | "delete"]) {
        crate::automation::scripts::sync(state)?;
    }
    if write && matches!(parts.as_slice(), ["api", "agents", _, "delete"]) {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        deletion::cleanup(db, parts[2])?;
        let storage = state
            .config
            .database
            .parent()
            .ok_or_else(Failure::storage)?
            .join("agent-desktops");
        match std::fs::symlink_metadata(&storage) {
            Ok(meta) if !meta.is_dir() => {
                return Err(error(409, "Desktop storage must use real directories."))
            }
            Ok(_) => {
                let target = storage.join(parts[2]);
                match std::fs::remove_dir_all(&target) {
                    Ok(()) => (),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                    Err(_) => return Err(Failure::storage()),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(Failure::storage()),
        }
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn route(
    state: &AppState,
    db: &Connection,
    p: &[&str],
    method: &Method,
    q: &[(String, String)],
    headers: &HeaderMap,
    v: &Value,
    avatar: &mut avatar::Prepared,
) -> Result<Response> {
    let get = *method == Method::GET;
    let post = *method == Method::POST;
    if post && matches!(p, ["api", "projects", _, "sections"]) {
        return Ok(json_response(
            201,
            projects::route(state, db, p, v, q, headers, get)?,
        ));
    }
    let value = match p {
        ["api", "projects", project, "environment", ..] => environment::route(
            state,
            db,
            project,
            p.last().copied().unwrap_or(""),
            v,
            headers,
            get,
        )?,
        ["api", "agents"] if get => {
            let mut value = agents::list(state, db)?;
            if let Some(items) = value["agents"].as_array_mut() {
                for item in items {
                    *item = agents::public_profile(item.clone());
                }
            }
            if value["guide"].is_object() {
                value["guide"] = agents::public_profile(value["guide"].clone());
            }
            value
        }
        ["api", "agents"] if post => {
            let (status, v) = agents::create(state, db, v, avatar)?;
            return Ok(json_response(status, agents::public_profile(v)));
        }
        ["api", "agents", id, "folder"] if get || post => {
            agent_folders::route(state, db, id, false, get, v, q, headers)?
        }
        ["api", "agents", id, "folder", "reset"] if post => {
            agent_folders::route(state, db, id, true, false, v, q, headers)?
        }
        ["api", "agents", id, "template"] if get => agents::template(db, id)?,
        ["api", "agents", id, "edit"] if post => {
            if *id == crate::mcp::guide::ID && headers.contains_key("authorization") {
                return Err(error(
                    403,
                    "Guide connection requires the local Owner in Minarbot.",
                ));
            }
            agents::public_profile(agents::edit(state, db, id, v)?)
        }
        ["api", "agents", id, "visibility"] if post => {
            let value = agents::visibility(state, db, id, v)?;
            crate::turns::refresh_routing(db)?;
            value
        }
        ["api", "agents", id, "scripts"] if get || post => agents::scripts(state, db, id, v, get)?,
        ["api", "agents", id, "delete"] if post => agents::delete(state, db, id, v, q, headers)?,
        ["api", "agents", id, "avatar"] if get => agents::avatar_read(db, id)?,
        ["api", "agents", id, "avatar"] if post => agents::avatar_save(db, id, v, avatar)?,
        ["api", "agents", id, "avatar", "image"] if get => {
            return agents::avatar_image(state, db, id, headers)
        }
        ["api", kind, id, "memory"] if ["agents", "channels"].contains(kind) => {
            memory::route(db, kind, id, v, q, get)?
        }
        ["api", kind @ ("agents" | "channels"), id, "memory", view @ ("knowledge" | "questions")] =>
        {
            if state.auth.is_some()
                || headers.contains_key("authorization")
                || headers.contains_key("origin") && !origin(state, headers)
            {
                return Err(error(403, "Memory requires the local owner in Minarbot."));
            }
            memory::knowledge::route(db, kind, id, *view == "questions", v, q, get)?
        }
        ["api", kind, id, "panel"] => {
            if headers.contains_key("authorization")
                || headers.contains_key("origin") && !origin(state, headers)
            {
                return Err(error(
                    403,
                    "Conversation panel requires the local owner in Minarbot.",
                ));
            }
            if !get {
                return Err(error(405, "Use GET for panel reads."));
            }
            if !q.is_empty() {
                return Err(error(400, "Invalid panel parameters."));
            }
            panel::read(state, db, kind, id)?
        }
        ["api", kind, id, "panel", "files"] => {
            if headers.contains_key("authorization")
                || headers.contains_key("origin") && !origin(state, headers)
            {
                return Err(error(
                    403,
                    "Conversation panel requires the local owner in Minarbot.",
                ));
            }
            if !get {
                return Err(error(405, "Use GET for panel reads."));
            }
            panel::context(state, db, kind, id)?;
            if q.iter().any(|(key, _)| key != "before") || q.len() > 1 {
                return Err(error(400, "Invalid panel parameters."));
            }
            let before = param(q, "before")
                .unwrap_or("9007199254740991")
                .parse::<i64>()
                .map_err(|_| error(400, "Invalid file list position."))?;
            if !(1..=9_007_199_254_740_991).contains(&before) {
                return Err(error(400, "Invalid file list position."));
            }
            panel::files(db, kind, id, before, false)?
        }
        ["api", "sidebar"] if get => sidebar::read(db, q)?,
        ["api", "sidebar"] if post => sidebar::save(db, v, q)?,
        ["api", "sidebar", "summary"] if get => sidebar::summary(state, db, q, headers)?,
        ["api", "usage"] if get => providers::usage(db, q)?,
        ["api", "conversations", "settings"] if get || post => conversations::settings(db, v, get)?,
        ["api", "agents", id, "threads"] if get || post => conversations::threads(db, id, v, get)?,
        ["api", "threads", id] if get => conversations::thread_info(db, id)?,
        ["api", "threads", id, "edit" | "delete"] if post => {
            conversations::edit_thread(db, id, p[3], v)?
        }
        ["api", "threads", id, "fork-context"] if get => {
            return conversations::fork_context(state, db, id, headers);
        }
        ["api", "channels"] if get => conversations::channels(db)?,
        ["api", "channels"] if post => {
            let (s, v) = conversations::save_channel(state, db, None, v)?;
            return Ok(json_response(s, v));
        }
        ["api", "channels", id, "edit"] if post => {
            conversations::save_channel(state, db, Some(id), v)?.1
        }
        ["api", "channels", id, "read"] if post => conversations::read_channel(db, id, v)?,
        ["api", kind, id, "turns"] if get => history::read(state, db, kind, id, q)?,
        ["api", kind, id, "dispatch"] if get => conversations::dispatch(db, kind, id)?,
        ["api", kind, id, "turns", turn, "fork"] if post => {
            conversations::fork(state, db, kind, id, turn, v)?
        }
        ["api", "projects", ..] | ["api", "project-folders"] if get || post => {
            projects::route(state, db, p, v, q, headers, get)?
        }
        _ => return Err(error(404, "Route not found.")),
    };
    if p == ["api", "sidebar", "summary"] {
        return Ok(wire::sidebar(&value));
    }
    if matches!(p, ["api", _, _, "turns"]) && get {
        return Ok(wire::history(&value));
    }
    Ok(json_response(200, value))
}
pub async fn canonical(
    state: &AppState,
    path: &str,
    method: &Method,
    body: &Value,
    actor: &crate::auth::Actor,
    q: &[(String, String)],
) -> Result<Option<Response>> {
    if path == "/api/sync" && *method == Method::GET {
        return sync::read(state, &actor.account, q).map(Some);
    }
    let parts = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    if let ["api", "invitations", invitation, "accept"] = parts.as_slice() {
        if *method == Method::POST {
            return access::accept(state, actor, &access::id(&json!(invitation))?, body)
                .await
                .map(Some);
        }
    }
    access::route(state, path, method, body, &actor.account).await
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/sync", any(crate::dispatch))
        .route("/api/turns/{id}/inspect", any(crate::dispatch))
        .route("/api/projects/{id}/invitations", any(crate::dispatch))
        .route("/api/invitations/{id}/accept", any(crate::dispatch))
        .route("/api/invitations/{id}/revoke", any(crate::dispatch))
        .route("/api/projects", any(crate::dispatch))
        .route("/api/projects/{id}", any(crate::dispatch))
        .route("/api/projects/{id}/members/{account}", any(crate::dispatch))
        .route("/api/projects/{id}/agents/{agent}", any(crate::dispatch))
        .route(
            "/api/projects/{id}/agents/{agent}/rules",
            any(crate::dispatch),
        )
        .route(
            "/api/projects/{id}/agents/{agent}/capabilities",
            any(crate::dispatch),
        )
        .route(
            "/api/projects/{id}/agents/{agent}/response-policy",
            any(crate::dispatch),
        )
        .route("/api/projects/{id}/environment", any(crate::dispatch))
        .route(
            "/api/projects/{id}/environment/remove",
            any(crate::dispatch),
        )
        .route(
            "/api/projects/{id}/environment/settings",
            any(crate::dispatch),
        )
        .route("/api/projects/{id}/edit", any(crate::dispatch))
        .route("/api/projects/{id}/archive", any(crate::dispatch))
        .route("/api/projects/{id}/restore", any(crate::dispatch))
        .route("/api/projects/{id}/delete", any(crate::dispatch))
        .route("/api/projects/{id}/thread", any(crate::dispatch))
        .route("/api/projects/{id}/sections", any(crate::dispatch))
        .route(
            "/api/projects/{id}/sections/{section}/edit",
            any(crate::dispatch),
        )
        .route("/api/project-folders", any(crate::dispatch))
        .route("/api/agents/{id}/edit", any(crate::dispatch))
        .route("/api/agents/{id}/folder", any(crate::dispatch))
        .route("/api/agents/{id}/folder/reset", any(crate::dispatch))
        .route("/api/agents/{id}/delete", any(crate::dispatch))
        .route("/api/agents/{id}/visibility", any(crate::dispatch))
        .route("/api/agents/{id}/scripts", any(crate::dispatch))
        .route("/api/agents/{id}/avatar", any(crate::dispatch))
        .route("/api/agents/{id}/avatar/image", any(crate::dispatch))
        .route("/api/agents/{id}/template", any(crate::dispatch))
        .route("/api/agents/{id}/memory", any(crate::dispatch))
        .route("/api/channels/{id}/memory", any(crate::dispatch))
        .route("/api/agents/{id}/memory/knowledge", any(crate::dispatch))
        .route("/api/channels/{id}/memory/knowledge", any(crate::dispatch))
        .route("/api/agents/{id}/memory/questions", any(crate::dispatch))
        .route("/api/channels/{id}/memory/questions", any(crate::dispatch))
        .route("/api/agents/{id}/history/search", any(crate::dispatch))
        .route("/api/channels/{id}/history/search", any(crate::dispatch))
        .route("/api/agents/{id}/threads", any(crate::dispatch))
        .route("/api/threads/{id}", any(crate::dispatch))
        .route("/api/threads/{id}/edit", any(crate::dispatch))
        .route("/api/threads/{id}/delete", any(crate::dispatch))
        .route("/api/threads/{id}/fork-context", any(crate::dispatch))
        .route("/api/channels", any(crate::dispatch))
        .route("/api/channels/{id}/edit", any(crate::dispatch))
        .route("/api/channels/{id}/read", any(crate::dispatch))
        .route("/api/agents/{id}/dispatch", any(crate::dispatch))
        .route("/api/threads/{id}/dispatch", any(crate::dispatch))
        .route("/api/channels/{id}/dispatch", any(crate::dispatch))
        .route("/api/agents/{id}/turns/{turn}/fork", any(crate::dispatch))
        .route("/api/threads/{id}/turns/{turn}/fork", any(crate::dispatch))
        .route("/api/channels/{id}/turns/{turn}/fork", any(crate::dispatch))
        .route("/api/sidebar", any(crate::dispatch))
        .route("/api/sidebar/summary", any(crate::dispatch))
        .route("/api/usage", any(crate::dispatch))
        .route("/api/models", any(crate::dispatch))
        .route("/api/engine-logins", any(crate::dispatch))
        .route("/api/engine-logins/{id}/check", any(crate::dispatch))
        .route("/api/engine-logins/{id}/remove", any(crate::dispatch))
        .route("/api/provider-accounts", any(crate::dispatch))
        .route("/api/provider-accounts/test", any(crate::dispatch))
        .route("/api/provider-accounts/{id}/test", any(crate::dispatch))
        .route("/api/provider-accounts/{id}/edit", any(crate::dispatch))
        .route("/api/provider-accounts/{id}/delete", any(crate::dispatch))
        .route("/api/provider-instances", any(crate::dispatch))
        .route("/api/provider-instances/{id}/edit", any(crate::dispatch))
        .route("/api/provider-instances/{id}/update", any(crate::dispatch))
        .route("/api/provider-instances/{id}/delete", any(crate::dispatch))
        .route("/api/provider-instances/{id}/refresh", any(crate::dispatch))
        .route("/api/provider-instances/{id}/sign-in", any(crate::dispatch))
        .route(
            "/api/provider-instances/{id}/sign-in-code",
            any(crate::dispatch),
        )
        .route("/api/provider-instances/detected", any(crate::dispatch))
        .route("/api/provider-instances/{id}/models", any(crate::dispatch))
        .route("/api/conversations/settings", any(crate::dispatch))
        .route("/api/settings/library-docs", any(crate::dispatch))
        .route("/api/settings/web-search", any(crate::dispatch))
        .route("/api/settings/web-search/test", any(crate::dispatch))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commit_failure_rolls_back_and_releases_transaction() -> Result<()> {
        let db = Connection::open_in_memory()?;
        db.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE parents(id INTEGER PRIMARY KEY); CREATE TABLE children(parent INTEGER REFERENCES parents(id) DEFERRABLE INITIALLY DEFERRED)")?;
        assert!(transaction(&db, || {
            db.execute("INSERT INTO children VALUES(1)", [])?;
            Ok(())
        })
        .is_err());
        assert!(db.is_autocommit());
        assert!(!exists(&db, "SELECT 1 FROM children", [])?);
        transaction(&db, || {
            db.execute("INSERT INTO parents VALUES(1)", [])?;
            Ok(())
        })?;
        Ok(())
    }
}
