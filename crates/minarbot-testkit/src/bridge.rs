//! Loopback test bridges backed by Rust closures.
//!
//! The Node bridge passed JavaScript callbacks (desktop runtimes, peer token
//! issuers, hub transports, PDF faults, Clerk clients) to the server through
//! small HTTP bridges. Here a test supplies a closure instead, and
//! [`crate::ServerConfig`] wires the bridge into the server's test options:
//!
//! | Bridge | Server option | Protocol |
//! |---|---|---|
//! | [`Bridge::operations`] + `computer_bridge` | `testComputerBridge` | `POST /` `{operation, args}` -> `{value}` or `{error:{status,message}}` (clock, issue, revoke, verify, peer-http, desktop-*, hub-*) |
//! | [`Bridge::paths`] + `canonical_bridge` | `testCanonicalBridge` | `POST /<operation>` JSON args -> `{value}` (clerk-user, clerk-revoke, mail-key, webhook-read, webhook-create) |
//! | [`Bridge::pdf`] + `pdf_bridge` | `testPdfBridge` | `POST /` PDF bytes -> `204` or `{error}` with a status |
//! | [`Bridge::composio_key`] + `composio` | `testConnectorBridge` | `GET /composio-key` -> `{key}` |
//!
//! Every bridge checks its bearer token, like the Node bridges.

use crate::Result;
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::{fmt, sync::Arc};
use tokio::{net::TcpListener, task::JoinHandle};

/// 64 hex characters, the bridge token format the server accepts
/// (`randomBytes(32).toString('hex')` in the Node bridges).
pub fn token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Failure returned by a bridge handler (`{error:{status,message}}`).
#[derive(Clone, Debug)]
pub struct BridgeError {
    pub status: u16,
    pub message: String,
}

impl BridgeError {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

type Operations = dyn Fn(&str, &Value) -> Result<Value, BridgeError> + Send + Sync;
type Pdf = dyn Fn(&[u8]) -> Option<(u16, String)> + Send + Sync;

enum Kind {
    Operations(Arc<Operations>),
    Paths(Arc<Operations>),
    Pdf(Arc<Pdf>),
    Key(String),
}

struct Shared {
    token: String,
    kind: Kind,
}

struct Inner {
    origin: String,
    token: String,
    task: JoinHandle<()>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A running bridge; clones share it. It stops when the last clone drops.
#[derive(Clone)]
pub struct Bridge(Arc<Inner>);

impl fmt::Debug for Bridge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Bridge")
            .field("origin", &self.0.origin)
            .finish()
    }
}

fn authorized(shared: &Shared, headers: &HeaderMap) -> bool {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        == Some(format!("Bearer {}", shared.token).as_str())
}

fn answer(result: Result<Value, BridgeError>) -> Response {
    match result {
        Ok(value) => Json(json!({"value": value})).into_response(),
        Err(error) => Json(json!({"error": {"status": error.status, "message": error.message}}))
            .into_response(),
    }
}

async fn operation(State(shared): State<Arc<Shared>>, headers: HeaderMap, body: Bytes) -> Response {
    if !authorized(&shared, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match &shared.kind {
        Kind::Operations(handler) => {
            let Ok(body) = serde_json::from_slice::<Value>(&body) else {
                return answer(Err(BridgeError::new(400, "Invalid bridge request.")));
            };
            answer(handler(
                body["operation"].as_str().unwrap_or_default(),
                &body["args"],
            ))
        }
        Kind::Pdf(handler) => match handler(&body) {
            None => StatusCode::NO_CONTENT.into_response(),
            Some((status, error)) => (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"error": error})),
            )
                .into_response(),
        },
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn path(
    State(shared): State<Arc<Shared>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorized(&shared, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Kind::Paths(handler) = &shared.kind else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let args = serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
    match handler(&name, &args) {
        Ok(value) => Json(json!({"value": value})).into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Canonical fixture unavailable."})),
        )
            .into_response(),
    }
}

async fn key(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if !authorized(&shared, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match &shared.kind {
        Kind::Key(key) => Json(json!({"key": key})).into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

impl Bridge {
    async fn start(kind: Kind) -> Result<Self> {
        let token = crate::bridge::token();
        let shared = Arc::new(Shared {
            token: token.clone(),
            kind,
        });
        let app = Router::new()
            .route("/", post(operation))
            .route("/composio-key", get(key))
            .route("/{name}", post(path))
            .with_state(shared);
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self(Arc::new(Inner {
            origin,
            token,
            task,
        })))
    }

    /// `{operation, args}` bridge (the computer bridge).
    pub async fn operations(
        handler: impl Fn(&str, &Value) -> Result<Value, BridgeError> + Send + Sync + 'static,
    ) -> Result<Self> {
        Self::start(Kind::Operations(Arc::new(handler))).await
    }

    /// `POST /<operation>` bridge (the canonical bridge).
    pub async fn paths(
        handler: impl Fn(&str, &Value) -> Result<Value, BridgeError> + Send + Sync + 'static,
    ) -> Result<Self> {
        Self::start(Kind::Paths(Arc::new(handler))).await
    }

    /// PDF fault bridge: return `Some((status, error))` to fail a render.
    pub async fn pdf(
        handler: impl Fn(&[u8]) -> Option<(u16, String)> + Send + Sync + 'static,
    ) -> Result<Self> {
        Self::start(Kind::Pdf(Arc::new(handler))).await
    }

    /// Composio key bridge that serves `key`.
    pub async fn composio_key(key: impl Into<String>) -> Result<Self> {
        Self::start(Kind::Key(key.into())).await
    }

    pub fn origin(&self) -> &str {
        &self.0.origin
    }

    pub fn token(&self) -> &str {
        &self.0.token
    }

    /// `{origin, token}` as the server options expect.
    pub fn config(&self) -> Value {
        json!({"origin": self.0.origin, "token": self.0.token})
    }
}
