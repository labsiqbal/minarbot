pub(crate) mod attachments;
mod backup;
pub(crate) mod git;
mod panel;
pub(crate) mod preview;
pub(crate) mod rich_html;
pub(crate) mod shared;
pub(crate) mod sites;
pub(crate) mod skills;
pub(crate) mod workspace;
// ponytail: turns lane connects workspace tool calls; HTTP routes do not expose host writes.
#[allow(dead_code)]
pub(crate) mod writes;
pub use workspace::validate_bindings as validate_workspaces;

use crate::{json_response, local_error, web::Failure, AppState};
use axum::{
    extract::{Request, State},
    http::{header, Method},
    response::Response,
    routing::any,
    Router,
};
use rusqlite::{types::ValueRef, Connection, Params};
use serde_json::{json, Map, Value};

pub type Result<T> = std::result::Result<T, Failure>;
pub fn error(status: u16, message: impl Into<String>) -> Failure {
    Failure::new(status, "data_error", message)
}
pub fn valid_id(value: &str) -> bool {
    crate::web::id(value).as_deref() == Some(value)
}
pub fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
pub fn keys(value: &Value, allowed: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|v| v.keys().all(|k| allowed.contains(&k.as_str())))
}
pub fn rows(db: &Connection, sql: &str, params: impl Params) -> Result<Vec<Value>> {
    let mut statement = db.prepare(sql)?;
    let names: Vec<_> = statement
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let values = statement
        .query_map(params, |row| {
            let mut value = Map::new();
            for (i, name) in names.iter().enumerate() {
                let item = match row.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(v) => json!(v),
                    ValueRef::Real(v) => json!(v),
                    ValueRef::Text(v) => json!(String::from_utf8_lossy(v)),
                    ValueRef::Blob(v) => Value::Array(v.iter().map(|b| json!(*b)).collect()),
                };
                value.insert(name.clone(), item);
            }
            Ok(Value::Object(value))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(values)
}
pub fn one(db: &Connection, sql: &str, params: impl Params) -> Result<Option<Value>> {
    Ok(rows(db, sql, params)?.into_iter().next())
}
pub fn transaction<T>(
    db: &mut Connection,
    work: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    let tx = db.transaction()?;
    let result = work(&tx)?;
    tx.commit()?;
    Ok(result)
}
pub fn paginate(mut rows: Vec<Value>, key: &str, cursor: &str) -> Value {
    let next = if rows.len() > 20 {
        rows[19][cursor].clone()
    } else {
        Value::Null
    };
    rows.truncate(20);
    json!({key:rows,"next_after":next})
}
pub fn query(req: &Request) -> Vec<(String, String)> {
    url::form_urlencoded::parse(req.uri().query().unwrap_or("").as_bytes())
        .into_owned()
        .collect()
}
pub fn parameter<'a>(q: &'a [(String, String)], key: &str) -> Option<&'a str> {
    q.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}
pub fn query_keys(q: &[(String, String)], allowed: &[&str]) -> bool {
    q.iter()
        .enumerate()
        .all(|(i, (k, _))| allowed.contains(&k.as_str()) && !q[..i].iter().any(|(p, _)| p == k))
}
pub fn origin(state: &AppState, request: &Request) -> Result<()> {
    if request.headers().contains_key(header::ORIGIN)
        && !crate::allowed(state, crate::text_header(request.headers(), "origin"))
    {
        return Err(error(403, "Origin is not allowed."));
    }
    Ok(())
}
pub fn mutation(state: &AppState, req: &Request) -> Result<()> {
    if !crate::allowed(state, crate::text_header(req.headers(), "origin"))
        || crate::text_header(req.headers(), "content-type")
            .unwrap_or("")
            .split(';')
            .next()
            != Some("application/json")
    {
        return Err(error(403, "Request must originate from Minarbot."));
    }
    Ok(())
}
pub async fn body(req: Request, limit: usize) -> Result<Value> {
    let bytes = axum::body::to_bytes(req.into_body(), limit)
        .await
        .map_err(|_| error(413, "Data is too large."))?;
    serde_json::from_slice(&bytes).map_err(|_| error(400, "Invalid JSON."))
}
pub fn timestamp(db: &Connection, now: i64) -> Result<String> {
    Ok(db.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%fZ',? / 1000.0,'unixepoch')",
        [now],
        |r| r.get(0),
    )?)
}
pub fn context(db: &Connection, kind: &str, id: &str) -> Result<Value> {
    let sql = match kind {
        "agents" => "SELECT id FROM agents WHERE id=?",
        "channels" => "SELECT id,archived FROM channels WHERE id=?",
        "threads" => "SELECT id,agent,archived FROM threads WHERE id=? AND deleted=0",
        _ => return Err(error(404, "Conversation not found.")),
    };
    one(db, sql, [id])?.ok_or_else(|| error(404, "Conversation not found."))
}
pub fn init(db: &Connection) -> crate::db::Result<()> {
    init_schema(db, false)?;
    db.execute_batch(include_str!("indexes.sql"))?;
    recover(db)
}
pub fn init_canonical(db: &Connection) -> crate::db::Result<()> {
    init_canonical_schema(db)?;
    recover(db)
}
pub(crate) fn init_canonical_schema(db: &Connection) -> crate::db::Result<()> {
    init_schema(db, true)
}
fn init_schema(db: &Connection, canonical: bool) -> crate::db::Result<()> {
    if canonical {
        db.execute_batch("CREATE TABLE IF NOT EXISTS shared_data(project TEXT NOT NULL REFERENCES projects(id),id INTEGER NOT NULL CHECK(id=1),revision INTEGER NOT NULL CHECK(revision>=0),image BLOB,PRIMARY KEY(project,id)) STRICT;")?;
    } else {
        db.execute_batch("CREATE TABLE IF NOT EXISTS shared_data(id INTEGER PRIMARY KEY CHECK(id=1),revision INTEGER NOT NULL,image BLOB) STRICT; INSERT OR IGNORE INTO shared_data VALUES(1,0,NULL); CREATE TABLE IF NOT EXISTS shared_data_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;")?;
    }
    db.execute_batch(include_str!("schema.sql"))?;
    for (table, name, definition) in [
        ("files", "deleted", "INTEGER NOT NULL DEFAULT 0"),
        ("generated_files", "workspace", "TEXT"),
        (
            "agent_skills",
            "removed",
            "INTEGER NOT NULL DEFAULT 0 CHECK(removed IN (0,1))",
        ),
        ("agent_skills", "pack", "TEXT REFERENCES skill_packs(id)"),
    ] {
        let columns =
            rows(db, &format!("PRAGMA table_info({table})"), []).map_err(|e| e.message)?;
        if !columns.iter().any(|v| v["name"] == name) {
            db.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {name} {definition}"
            ))?;
        }
    }
    Ok(())
}
fn recover(db: &Connection) -> crate::db::Result<()> {
    writes::initialize(db)?;
    git::writes::initialize(db)
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/agents/{id}/files", any(entry))
        .route("/api/agents/{id}/files/{file}", any(entry))
        .route("/api/channels/{id}/files", any(entry))
        .route("/api/channels/{id}/files/{file}", any(entry))
        .route("/api/threads/{id}/files", any(entry))
        .route("/api/threads/{id}/files/{file}", any(entry))
        .route("/api/{kind}/{id}/html-replies/{reply}", any(entry))
        .route("/api/{kind}/{id}/mcp-apps/{app}", any(entry))
        .route("/api/{kind}/{id}/mcp-apps/{app}/{action}", any(entry))
        .route("/api/agents/{id}/sites", any(entry))
        .route("/api/agents/{id}/sites/{site}", any(entry))
        .route("/api/skills", any(entry))
        .route("/api/backup", any(entry))
        .route("/api/data", any(entry))
        .route("/api/data/query", any(entry))
        .route("/api/data/drop", any(entry))
        .route("/api/skills/{id}", any(entry))
        .route("/api/skill-packs", any(entry))
        .route("/api/skill-packs/{id}", any(entry))
        .route("/api/agents/{id}/skills", any(entry))
        .route("/api/agents/{id}/skills/{skill}", any(entry))
        .route("/api/agents/{id}/skill-packs/{pack}", any(entry))
        .route("/api/agents/{id}/workspace/entries", any(entry))
        .route("/api/agents/{id}/workspace/file", any(entry))
        .route("/api/agents/{id}/workspace/download", any(entry))
        .route("/api/projects/{id}/git", any(entry))
        .route("/api/projects/{id}/git/diff", any(entry))
        .route("/api/projects/{id}/git/log", any(entry))
        .route("/api/agents/{id}/git", any(entry))
        .route("/api/agents/{id}/git/status", any(entry))
        .route("/api/agents/{id}/git/diff", any(entry))
        .route("/api/agents/{id}/git/log", any(entry))
        .route("/api/agents/{id}/git/checkpoints/{turn}", any(entry))
        .route(
            "/api/agents/{id}/git/checkpoints/{turn}/restore",
            any(entry),
        )
        .route("/api/agents/{id}/panel", any(entry))
        .route("/api/agents/{id}/panel/files", any(entry))
        .route("/api/agents/{id}/panel/files/{file}", any(entry))
        .route("/api/agents/{id}/panel/workspace/entries", any(entry))
        .route("/api/agents/{id}/panel/workspace/file", any(entry))
        .route("/api/agents/{id}/panel/workspace/download", any(entry))
        .route("/api/agents/{id}/panel/git", any(entry))
        .route("/api/agents/{id}/panel/git/diff", any(entry))
        .route("/api/agents/{id}/panel/sites/{site}", any(entry))
        .route("/api/threads/{id}/panel", any(entry))
        .route("/api/threads/{id}/panel/files", any(entry))
        .route("/api/threads/{id}/panel/files/{file}", any(entry))
        .route("/api/threads/{id}/panel/workspace/entries", any(entry))
        .route("/api/threads/{id}/panel/workspace/file", any(entry))
        .route("/api/threads/{id}/panel/workspace/download", any(entry))
        .route("/api/threads/{id}/panel/git", any(entry))
        .route("/api/threads/{id}/panel/git/diff", any(entry))
        .route("/api/threads/{id}/panel/sites/{site}", any(entry))
        .route("/api/channels/{id}/panel", any(entry))
        .route("/api/channels/{id}/panel/files", any(entry))
        .route("/api/channels/{id}/panel/files/{file}", any(entry))
        .route("/api/channels/{id}/panel/workspace/entries", any(entry))
        .route("/api/channels/{id}/panel/workspace/file", any(entry))
        .route("/api/channels/{id}/panel/workspace/download", any(entry))
        .route("/api/channels/{id}/panel/git", any(entry))
        .route("/api/channels/{id}/panel/git/diff", any(entry))
        .route("/api/channels/{id}/panel/sites/{site}", any(entry))
        .route("/sites/{id}", any(entry))
}
pub(crate) async fn entry(State(state): State<AppState>, req: Request) -> Response {
    if state.auth.is_some() {
        return crate::canonical(&state, req).await;
    }
    let result = async {
        let host = crate::text_header(req.headers(), "host").unwrap_or("");
        if (format!("http://{host}") != state.origin
            && !state
                .config
                .allowed_origins
                .contains(&format!("https://{host}")))
            || crate::text_header(req.headers(), "sec-fetch-site") == Some("cross-site")
        {
            return Err(error(
                403,
                "Access is only allowed through the local Minarbot address.",
            ));
        }
        if crate::text_header(req.headers(), "authorization")
            .is_some_and(|v| v.starts_with("Bearer bsi_"))
        {
            return Err(error(401, "Unauthorized request."));
        }
        let path = req.uri().path().to_owned();
        let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
        if parts.get(3) == Some(&"panel") {
            return panel::handle(&state, req, &parts).await;
        }
        if let ["api", kind, owner, "html-replies", id] = parts.as_slice() {
            return rich_html::serve(&state, &req, kind, owner, id);
        }
        if let ["api", kind, owner, "mcp-apps", id] = parts.as_slice() {
            return crate::mcp::apps::route(&state, req, kind, owner, id, None).await;
        }
        if let ["api", kind, owner, "mcp-apps", id, action] = parts.as_slice() {
            return crate::mcp::apps::route(&state, req, kind, owner, id, Some(action)).await;
        }
        if path == "/api/backup" {
            return backup::handle(&state, req).await;
        }
        if path == "/api/data" || path == "/api/data/query" || path == "/api/data/drop" {
            return shared::handle(&state, req).await;
        }
        if parts.first() == Some(&"sites") || parts.get(3) == Some(&"sites") {
            return sites::handle(&state, req, &parts).await;
        }
        if parts.get(3) == Some(&"files")
            && parameter(&query(&req), "transcription") == Some("1")
            && parts.len() == 5
        {
            return crate::turns::transcription::handle(&state, req, &parts).await;
        }
        if parts.get(3) == Some(&"files") {
            return attachments::handle(&state, req, &parts).await;
        }
        if parts.get(3) == Some(&"workspace") {
            return workspace::handle(&state, req, &parts).await;
        }
        if parts.get(3) == Some(&"git") {
            if parts.get(1) == Some(&"projects") {
                return git::project(&state, req, &parts).await;
            }
            return git::handle(&state, req, &parts).await;
        }
        skills::handle(&state, req, &parts).await
    }
    .await;
    let mut response = result.unwrap_or_else(|e| local_error(e.status, &e.message));
    if response.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE {
        response.headers_mut().insert(
            header::CONNECTION,
            axum::http::HeaderValue::from_static("close"),
        );
    }
    crate::security_headers(&mut response);
    response
}
