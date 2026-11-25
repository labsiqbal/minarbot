//! Recording MCP session issuer (the Node `harness.issuer` option).
//!
//! With [`crate::ServerConfig::issuer`], the server asks this loopback bridge
//! to mint and revoke the MCP session of every harness turn instead of its
//! own issuer. Each mint returns the fixed token [`SESSION_TOKEN`], so tests
//! can prove that the token is redacted from replies, snapshots and approval
//! cards. The recorder outlives server restarts.

use crate::Result;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    fmt,
    sync::{Arc, Mutex},
};
use tokio::{net::TcpListener, task::JoinHandle};
use uuid::Uuid;

/// Token returned for every minted session.
pub const SESSION_TOKEN: &str = "test-mcp-session-token";

/// One minted session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Minted {
    pub agent: String,
    pub turn: String,
    pub id: String,
}

#[derive(Default)]
struct Log {
    minted: Vec<Minted>,
    revoked: Vec<String>,
}

struct Bridge {
    token: String,
    log: Mutex<Log>,
}

struct Inner {
    origin: String,
    bridge: Arc<Bridge>,
    task: JoinHandle<()>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Cloneable handle; all clones share one bridge and one log.
#[derive(Clone)]
pub struct SessionIssuer(Arc<Inner>);

impl fmt::Debug for SessionIssuer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionIssuer")
            .field("origin", &self.0.origin)
            .finish()
    }
}

impl SessionIssuer {
    pub async fn start() -> Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let bridge = Arc::new(Bridge {
            token: crate::bridge::token(),
            log: Mutex::new(Log::default()),
        });
        let app = Router::new()
            .route("/", post(handle))
            .with_state(bridge.clone());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self(Arc::new(Inner {
            origin,
            bridge,
            task,
        })))
    }

    pub(crate) fn bridge_config(&self) -> Value {
        json!({"origin": self.0.origin, "token": self.0.bridge.token})
    }

    /// Sessions minted so far, in order.
    pub fn minted(&self) -> Vec<Minted> {
        self.0
            .bridge
            .log
            .lock()
            .map(|log| log.minted.clone())
            .unwrap_or_default()
    }

    /// Session ids revoked so far, in order.
    pub fn revoked(&self) -> Vec<String> {
        self.0
            .bridge
            .log
            .lock()
            .map(|log| log.revoked.clone())
            .unwrap_or_default()
    }

    /// Wait until `count` sessions are revoked (revocation is asynchronous).
    pub async fn wait_revoked(&self, count: usize) -> Result<Vec<String>> {
        crate::wait::until(format!("{count} revoked MCP sessions"), || async {
            let revoked = self.revoked();
            Ok((revoked.len() >= count).then_some(revoked))
        })
        .await
    }
}

async fn handle(
    State(bridge): State<Arc<Bridge>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let expected = format!("Bearer {}", bridge.token);
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some(&expected)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(mut log) = bridge.log.lock() else {
        return Json(json!({"error": "poisoned"})).into_response();
    };
    let args = &body["args"];
    match body["operation"].as_str() {
        Some("mint") => {
            let id = Uuid::new_v4().to_string();
            log.minted.push(Minted {
                agent: args["agent"].as_str().unwrap_or_default().to_owned(),
                turn: args["turn"].as_str().unwrap_or_default().to_owned(),
                id: id.clone(),
            });
            Json(json!({"value": {"id": id, "token": SESSION_TOKEN}})).into_response()
        }
        Some("revoke") => {
            log.revoked
                .push(args["id"].as_str().unwrap_or_default().to_owned());
            Json(json!({"value": null})).into_response()
        }
        _ => Json(json!({"error": "Harness fixture bridge failed."})).into_response(),
    }
}
