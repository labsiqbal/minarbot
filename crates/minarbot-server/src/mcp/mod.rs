pub(crate) mod client;
pub(crate) mod codegraph;
mod connections;
pub(crate) mod execution;
mod features;
pub(crate) mod guide;
mod registry;
pub(crate) mod secrets;
mod server;
mod stdio;
pub use stdio::run as stdio;
mod template;
mod validate;
use crate::{
    data::{self, all, closed, error, field, one, string, transaction, uuid},
    web::Failure,
    AppState,
};
use axum::{extract::Request, http::Method, response::Response, routing::any, Router};
pub(crate) use registry::hash_bytes;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
pub type Result<T> = std::result::Result<T, Failure>;
pub fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(include_str!("schema.sql"))?;
    let columns = all(db, "PRAGMA table_info(mcp_tests)", [])?;
    for (name, kind) in [("connection", "TEXT"), ("revision", "INTEGER")] {
        if !columns.iter().any(|v| v["name"] == name) {
            db.execute_batch(&format!("ALTER TABLE mcp_tests ADD COLUMN {name} {kind}"))?;
        }
    }
    Ok(())
}
pub fn url(state: &AppState) -> String {
    format!("{}/mcp", state.origin)
}
pub fn mint_session(state: &AppState, agent: &str, turn: &str) -> Result<(String, String)> {
    registry::mint(state, agent, turn)
}
pub fn revoke_session(state: &AppState, id: &str) {
    if let Ok(db) = state.database.lock() {
        let _=db.connection.execute("UPDATE mcp_server_tokens SET revoked=coalesce(revoked,?) WHERE id=? AND kind='session'",params![state.config.now(),id]);
    }
}
pub fn handles(path: &str) -> bool {
    path == "/mcp"
        || path.starts_with("/api/mcp")
        || path.contains("/mcp/")
        || path.starts_with("/api/projects/") && path.ends_with("/mcp")
        || guide::handles(path)
}
pub async fn local(state: &AppState, request: Request) -> Result<Response> {
    let path = request.uri().path().to_owned();
    if guide::handles(&path) {
        return guide::local(state, request).await;
    }
    if path == "/mcp" {
        let host = crate::text_header(request.headers(), "host").unwrap_or("");
        if format!("http://{host}") != state.origin
            && !state
                .config
                .allowed_origins
                .iter()
                .any(|v| v == &format!("https://{host}"))
        {
            return Err(error(
                403,
                "Access is only allowed through the local Minarbot address.",
            ));
        }
        if let Some(origin) = crate::text_header(request.headers(), "origin") {
            if !crate::allowed(state, Some(origin)) {
                return Err(error(
                    403,
                    "Access is only allowed through the local Minarbot address.",
                ));
            }
        }
        return server::rpc(state, request).await;
    }
    if request.headers().contains_key("authorization")
        || request.method() != Method::GET
            && (!data::origin(state, request.headers())
                || crate::text_header(request.headers(), "content-type")
                    .unwrap_or("")
                    .split(';')
                    .next()
                    != Some("application/json"))
    {
        return Err(error(403, "Request must originate from Minarbot."));
    }
    if path.starts_with("/api/mcp") && !path.starts_with("/api/mcp-server")
        || path.contains("/mcp/")
        || path.starts_with("/api/projects/")
    {
        return connections::route(state, request).await;
    }
    let get = request.method() == Method::GET;
    let value = if get {
        json!({})
    } else {
        let bytes = axum::body::to_bytes(request.into_body(), 64000)
            .await
            .map_err(|_| error(413, "Request is too large."))?;
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| error(400, "Invalid JSON."))?;
        if !value.is_object() || !uuid(string(&value, "id")) {
            return Err(error(400, "Invalid request ID."));
        }
        value
    };
    let db = state.database.lock().map_err(|_| Failure::storage())?;
    let result = match path.split('/').skip(1).collect::<Vec<_>>().as_slice() {
        ["api", "mcp-server", "peer-tokens"] => {
            let mut peers = registry::peers(state, &db.connection, &value, get)?;
            if get {
                peers["endpoint"] = json!(url(state));
            }
            peers
        }
        ["api", "mcp-server", "peer-tokens", id, "revoke"] => {
            registry::revoke_peer(state, &db.connection, id, &value)?
        }
        _ => return Err(error(404, "Route not found.")),
    };
    Ok(crate::json_response(200, result))
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/mcp", any(crate::dispatch))
        .route("/api/mcp", any(crate::dispatch))
        .route("/api/mcp/test", any(crate::dispatch))
        .route("/api/mcp/{id}", any(crate::dispatch))
        .route("/api/mcp/{id}/remove", any(crate::dispatch))
        .route("/api/agents/{agent}/mcp/{id}", any(crate::dispatch))
        .route("/api/projects/{project}/mcp", any(crate::dispatch))
        .route("/api/projects/{project}/mcp/test", any(crate::dispatch))
        .route("/api/projects/{project}/mcp/{id}", any(crate::dispatch))
        .route(
            "/api/projects/{project}/mcp/{id}/remove",
            any(crate::dispatch),
        )
        .route(
            "/api/projects/{project}/agents/{agent}/mcp/{id}",
            any(crate::dispatch),
        )
        .route(
            "/api/mcp-server/peer-tokens/{id}/revoke",
            any(crate::dispatch),
        )
}
