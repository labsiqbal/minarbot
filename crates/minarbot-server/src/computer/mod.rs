#[allow(dead_code)]
pub mod actions;
mod browser_actions;
mod browser_pipe;
#[allow(dead_code)]
pub mod control;
mod desktop;
pub(crate) mod desktop_supervisor;
pub mod devices;
pub mod host_metrics;
pub mod hub;
pub mod hub_config;
#[allow(dead_code)]
pub mod machines;
pub mod options;
#[allow(dead_code)]
pub mod remote;
#[allow(dead_code)]
pub mod resources;
mod vnc;
pub use desktop::Manager;
mod proxy;
pub(crate) use proxy::denied as denied_outbound_ip;

use crate::{local_error, web::Failure, AppState};
use axum::{
    extract::{Request, State},
    response::Response,
    routing::any,
    Router,
};

use serde_json::Value;

pub type Result<T> = std::result::Result<T, Failure>;
pub fn initialize(db: &rusqlite::Connection) -> crate::db::Result<()> {
    db.execute_batch(include_str!("local.sql"))?;
    initialize_canonical(db)
}
pub fn initialize_canonical(db: &rusqlite::Connection) -> crate::db::Result<()> {
    db.execute_batch(include_str!("schema.sql"))?;
    machines::initialize(db)?;
    hub::initialize(db)?;
    remote::initialize(db)
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.message.into() })?;
    resources::initialize(db)
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.message.into() })
}

fn transaction<T>(
    db: &rusqlite::Connection,
    work: impl FnOnce(&rusqlite::Connection) -> Result<T>,
) -> Result<T> {
    let tx = rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)?;
    let result = work(&tx)?;
    tx.commit()?;
    Ok(result)
}

fn error(status: u16, message: impl Into<String>) -> Failure {
    Failure::new(status, "computer_error", message)
}
async fn body(request: Request, limit: usize) -> Result<Value> {
    let bytes = axum::body::to_bytes(request.into_body(), limit)
        .await
        .map_err(|_| error(413, "Payload exceeds the limit."))?;
    serde_json::from_slice(&bytes).map_err(|_| error(400, "Invalid JSON."))
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .merge(devices::routes())
        .merge(hub::routes())
        .route("/api/agents/{id}/computer", any(entry))
        .route("/api/agents/{id}/computer/start", any(entry))
        .route("/api/agents/{id}/computer/stop", any(entry))
        .route("/api/agents/{id}/computer/screenshot", any(entry))
        .route("/api/agents/{id}/computer/control", any(entry))
        .route("/api/agents/{id}/computer/input", any(entry))
        .route("/api/agents/{id}/computer/open-url", any(entry))
        .route("/api/agents/{id}/computer/vnc-ticket", any(entry))
        .route("/api/agents/{id}/computer/vnc", any(entry))
        .route("/api/environment", any(entry))
        .route("/api/host-metrics", any(entry))
        .route("/api/remote-agents/messages", any(entry))
        .route("/api/machines", any(entry))
        .route("/api/machines/remove", any(entry))
        .route("/api/machines/reciprocal", any(entry))
        .route("/api/machines/agent-messaging", any(entry))
        .route("/api/pairing-codes", any(entry))
        .route("/api/pairing/redeem", any(entry))
        .route("/api/peer/agents", any(entry))
        .route("/api/peer/unpair", any(entry))
        .route("/api/mcp-server/peer-tokens", any(entry))
}
pub(crate) async fn feature(state: &AppState, request: Request) -> Result<Response> {
    if request.uri().path() == "/api/machines" {
        machines::handle(state, request).await
    } else {
        handle_computer(state, request).await
    }
}
async fn entry(State(state): State<AppState>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let transport = [
        "/api/environment",
        "/api/pairing/redeem",
        "/api/machines/reciprocal",
        "/api/peer/agents",
        "/api/peer/unpair",
    ]
    .contains(&path.as_str());
    let result = async {
        if state.auth.is_some() {
            return Err(error(404, "Route is not available yet."));
        }
        let host = crate::text_header(request.headers(), "host").unwrap_or("");
        if (!transport
            && format!("http://{host}") != state.origin
            && !state
                .config
                .allowed_origins
                .contains(&format!("https://{host}")))
            || crate::text_header(request.headers(), "sec-fetch-site") == Some("cross-site")
                && path != "/api/environment"
        {
            return Err(error(
                403,
                "Access is only allowed through the local Minarbot address.",
            ));
        }
        if crate::updates::pending(&state) && path != "/api/environment" {
            return Err(error(503, "Update is being verified. Retry after restart."));
        }
        if crate::text_header(request.headers(), "authorization")
            .is_some_and(|v| v.starts_with("Bearer bsi_"))
        {
            return Err(error(401, "Unauthorized request."));
        }
        if path == "/api/host-metrics" {
            return host_metrics::handle(&state, request).await;
        }
        if path.starts_with("/api/agents/") {
            return handle_computer(&state, request).await;
        }
        if path == "/api/remote-agents/messages" {
            return remote::handle(&state, request).await;
        }
        if crate::mcp::handles(&path) {
            return crate::mcp::local(&state, request).await;
        }
        machines::handle(&state, request).await
    }
    .await;
    let mut response = result.unwrap_or_else(|e| {
        if path.ends_with("/computer/vnc-ticket") {
            crate::json_response(
                e.status,
                serde_json::json!({"error":e.message,"code":e.code}),
            )
        } else {
            local_error(e.status, &e.message)
        }
    });
    crate::security_headers(&mut response);
    response
}
pub fn maintenance(state: AppState) -> Vec<tokio::task::JoinHandle<()>> {
    let desktop = state.clone();
    vec![
        tokio::spawn(remote::worker(state.clone())),
        hub::maintenance(state.clone()),
        tokio::spawn(async move {
            let interval = state
                .config
                .computer
                .machines
                .as_ref()
                .and_then(|m| m.health_interval_ms)
                .unwrap_or(30000)
                .max(1);
            let mut timer = tokio::time::interval_at(
                tokio::time::Instant::now() + std::time::Duration::from_millis(interval),
                std::time::Duration::from_millis(interval),
            );
            loop {
                timer.tick().await;
                if !crate::updates::pending(&state) {
                    let _ = machines::health(&state).await;
                }
            }
        }),
        tokio::spawn(async move {
            let interval = desktop
                .config
                .computer
                .desktop
                .as_ref()
                .and_then(|d| d.idle_ms)
                .unwrap_or(900000)
                .clamp(1, 10000) as u64;
            let mut timer = tokio::time::interval(std::time::Duration::from_millis(interval));
            loop {
                timer.tick().await;
                if !crate::updates::pending(&desktop) {
                    let _ = desktop.computer.maintain(desktop.config.now()).await;
                }
            }
        }),
    ]
}

async fn handle_computer(state: &AppState, request: Request) -> Result<Response> {
    use axum::{
        http::{header, HeaderValue, Method},
        response::IntoResponse,
    };
    let path = request.uri().path().to_owned();
    let parts = path.split('/').collect::<Vec<_>>();
    let agent = parts
        .get(3)
        .copied()
        .filter(|v| crate::web::id(v).is_some())
        .ok_or_else(|| error(404, "Agent not found."))?;
    let operation = parts.get(5).copied().unwrap_or("");
    if request.headers().contains_key("authorization") {
        return Err(error(
            403,
            "Computer access requires the local owner in Minarbot.",
        ));
    }
    {
        let db = state.database.lock().map_err(|_| Failure::storage())?;
        if crate::files::one(&db.connection, "SELECT 1 FROM agents WHERE id=?", [agent])?.is_none()
        {
            return Err(error(404, "Agent not found."));
        }
    }
    if operation == "vnc" {
        return vnc::upgrade(state.clone(), request, agent.into()).await;
    }
    if request.uri().query().is_some() {
        return Err(error(400, "Computer routes do not accept parameters."));
    }
    crate::files::origin(state, &request)?;
    if request.method() == Method::GET && operation.is_empty() {
        return Ok(crate::json_response(200, control::read(state, agent)?));
    }
    if request.method() == Method::GET && operation == "screenshot" {
        control::assert_enabled(state, agent)?;
        let image = state.computer.screenshot(agent, true).await?;
        let mut response = (
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (
                    axum::http::HeaderName::from_static("cross-origin-resource-policy"),
                    "same-origin",
                ),
            ],
            image.clone(),
        )
            .into_response();
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from(image.len()));
        return Ok(response);
    }
    if request.method() != Method::POST || operation == "screenshot" {
        return Err(error(405, "Method not allowed."));
    }
    crate::files::mutation(state, &request)?;
    let origin = crate::text_header(request.headers(), "origin")
        .unwrap_or("")
        .to_owned();
    let data = body(request, if operation == "input" { 20000 } else { 2048 }).await?;
    let value = match operation {
        "" => control::configure(state, agent, &data).await?,
        "control" => control::transition(state, agent, &data).await?,
        "input" => control::input(state, agent, &data).await?,
        "open-url" => control::open_result(state, agent, &data).await?,
        "vnc-ticket" => state.computer.viewers.mint(state, agent, &origin, &data)?,
        "start" | "stop" => {
            if !data.as_object().is_some_and(|m| m.is_empty()) {
                return Err(error(400, "Computer controls do not accept arguments."));
            }
            let gate = state.computer.control_gate(agent)?;
            let _guard = gate.lock().await;
            if operation == "start" {
                control::assert_enabled(state, agent)?;
                state.computer.start(agent, state.config.now()).await?;
            } else {
                state.computer.viewers.revoke(agent)?;
                state.computer.stop(agent).await?;
            }
            control::read(state, agent)?
        }
        _ => return Err(error(404, "Route not found.")),
    };
    Ok(crate::json_response(200, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_commit_rolls_back_and_allows_next_command() -> Result<()> {
        let db = rusqlite::Connection::open_in_memory()?;
        db.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE parents(id INTEGER PRIMARY KEY); CREATE TABLE children(parent INTEGER REFERENCES parents(id) DEFERRABLE INITIALLY DEFERRED);")?;
        assert!(transaction(&db, |db| {
            db.execute("INSERT INTO children VALUES(1)", [])?;
            Ok(())
        })
        .is_err());
        assert!(db.is_autocommit());
        assert_eq!(
            db.query_row("SELECT count(*) FROM children", [], |r| r.get::<_, i64>(0))?,
            0
        );
        transaction(&db, |db| {
            db.execute("INSERT INTO parents VALUES(1)", [])?;
            db.execute("INSERT INTO children VALUES(1)", [])?;
            Ok(())
        })?;
        assert_eq!(
            db.query_row("SELECT count(*) FROM children", [], |r| r.get::<_, i64>(0))?,
            1
        );
        Ok(())
    }
}
