//! External streamable-HTTP MCP server for connection tests.
//!
//! It answers `initialize` and `tools/list` with fixed values and holds every
//! `tools/call` until the test releases that call by number, so a test can
//! observe the server while a tool call is in flight.

use crate::Result;
use axum::{
    body::Bytes,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::{net::TcpListener, sync::watch, task::JoinHandle};

struct Shared {
    tools: Value,
    result: Value,
    /// Number of `tools/call` requests received.
    effects: Mutex<usize>,
    /// Highest call number released so far.
    released: watch::Sender<usize>,
}

pub struct McpServer {
    origin: String,
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl McpServer {
    /// Serve `tools` from `tools/list` and `result` from every released call.
    pub async fn start(tools: Value, result: Value) -> Result<Self> {
        let (released, _) = watch::channel(0);
        let shared = Arc::new(Shared {
            tools,
            result,
            effects: Mutex::new(0),
            released,
        });
        let app = Router::new()
            .route(
                "/mcp",
                post(rpc).get(|| async { StatusCode::METHOD_NOT_ALLOWED }),
            )
            .with_state(shared.clone());
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            origin,
            shared,
            task,
        })
    }

    /// `<origin>/mcp`.
    pub fn url(&self) -> String {
        format!("{}/mcp", self.origin)
    }

    /// `tools/call` requests received so far.
    pub fn effects(&self) -> usize {
        self.shared.effects.lock().map_or(0, |effects| *effects)
    }

    /// Let calls up to number `call` (1-based) answer.
    pub fn release(&self, call: usize) {
        self.shared
            .released
            .send_modify(|released| *released = (*released).max(call));
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        self.release(usize::MAX);
        self.task.abort();
    }
}

async fn rpc(State(shared): State<Arc<Shared>>, body: Bytes) -> Response {
    let Ok(request) = serde_json::from_slice::<Value>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if request.get("id").is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    let result = match request["method"].as_str() {
        Some("initialize") => json!({"protocolVersion": "2025-03-26",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "hostile-schema-fixture", "version": "1"}}),
        Some("tools/list") => json!({"tools": shared.tools}),
        _ => {
            let number = match shared.effects.lock() {
                Ok(mut effects) => {
                    *effects += 1;
                    *effects
                }
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            };
            let mut released = shared.released.subscribe();
            let _ = released.wait_for(|released| *released >= number).await;
            shared.result.clone()
        }
    };
    Json(json!({"jsonrpc": "2.0", "id": request["id"], "result": result})).into_response()
}
