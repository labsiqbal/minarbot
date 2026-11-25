//! OpenAI-compatible model whose script is an async closure.
//!
//! [`crate::model::ScriptedModel`] answers synchronously. Peer suites need
//! scripts that wait (a remote agent's reply is held until the test
//! releases it) or that poll the test's own state before answering, as the
//! Node fixtures' `async body => ...` providers did. [`AsyncModel`] runs the
//! script per request and records every request body. A script error
//! answers HTTP 500, like the Node fixtures' `catch`.

use crate::Result;
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::{future::BoxFuture, StreamExt};
use serde_json::{json, Value};
use std::{
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::{net::TcpListener, task::JoinHandle};

/// One model step.
#[derive(Clone, Debug)]
pub enum Step {
    /// Final answer text.
    Text(String),
    /// One tool call with these JSON arguments.
    Tool(String, Value),
    /// HTTP error status.
    Status(u16),
    /// One `tool_calls` entry sent verbatim (may be malformed on purpose).
    RawTool(Value),
    /// Server-sent events: one content delta, then the final frame after
    /// `delay`, whatever the request asked for (the Node `streamText`).
    Stream(String, std::time::Duration),
}

impl Step {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    pub fn tool(name: &str, arguments: Value) -> Self {
        Self::Tool(name.to_owned(), arguments)
    }
}

type Script = Arc<dyn Fn(Value) -> BoxFuture<'static, Result<Step>> + Send + Sync>;

struct Shared {
    script: Script,
    requests: Mutex<Vec<Value>>,
}

/// Running model; stops on drop.
pub struct AsyncModel {
    origin: String,
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl AsyncModel {
    pub async fn start<F, Fut>(script: F) -> Result<Self>
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Step>> + Send + 'static,
    {
        let shared = Arc::new(Shared {
            script: Arc::new(move |body| Box::pin(script(body))),
            requests: Mutex::new(Vec::new()),
        });
        let app = Router::new()
            .route("/v1/chat/completions", post(completion))
            .route("/v1/models", get(models))
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

    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// OpenAI-compatible base URL (`<origin>/v1`).
    pub fn base_url(&self) -> String {
        format!("{}/v1", self.origin)
    }

    /// Every request body so far.
    pub fn requests(&self) -> Vec<Value> {
        match self.shared.requests.lock() {
            Ok(requests) => requests.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl Drop for AsyncModel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn models() -> Json<Value> {
    Json(json!({"object": "list", "data": [{"id": "fixture", "object": "model"}]}))
}

async fn completion(State(shared): State<Arc<Shared>>, body: Bytes) -> Response {
    let Ok(body) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, "invalid JSON").into_response();
    };
    if let Ok(mut requests) = shared.requests.lock() {
        requests.push(body.clone());
    }
    let stream = body["stream"] == true;
    let step = match (shared.script)(body).await {
        Ok(step) => step,
        Err(_) => Step::Status(500),
    };
    if let Step::Stream(text, delay) = &step {
        let first =
            json!({"choices": [{"index": 0, "delta": {"content": text}, "finish_reason": null}]});
        let last = json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]});
        let delay = *delay;
        let frames = futures_util::stream::iter([Ok::<_, std::convert::Infallible>(format!(
            "data: {first}\n\n"
        ))])
        .chain(futures_util::stream::once(async move {
            tokio::time::sleep(delay).await;
            Ok(format!("data: {last}\n\ndata: [DONE]\n\n"))
        }));
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from_stream(frames))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    let (message, finish, delta) = match &step {
        Step::Stream(..) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        Step::Status(status) => {
            return (
                StatusCode::from_u16(*status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({})),
            )
                .into_response()
        }
        Step::Text(text) => (
            json!({"role": "assistant", "content": text}),
            "stop",
            json!({"role": "assistant", "content": text}),
        ),
        Step::Tool(..) | Step::RawTool(..) => {
            let call = match &step {
                Step::Tool(name, arguments) => json!({"id": crate::new_id(), "type": "function",
                    "function": {"name": name, "arguments": arguments.to_string()}}),
                Step::RawTool(call) => call.clone(),
                _ => Value::Null,
            };
            let mut indexed = call.clone();
            if indexed.is_object() {
                indexed["index"] = json!(0);
            }
            (
                json!({"role": "assistant", "content": null, "tool_calls": [call]}),
                "tool_calls",
                json!({"tool_calls": [indexed]}),
            )
        }
    };
    if stream {
        let frames = [
            json!({"choices": [{"index": 0, "delta": delta, "finish_reason": null}]}),
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": finish}]}),
        ];
        let mut out: String = frames
            .iter()
            .map(|frame| format!("data: {frame}\n\n"))
            .collect();
        out.push_str("data: [DONE]\n\n");
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(out))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    Json(json!({"choices": [{"index": 0, "finish_reason": finish, "message": message}]}))
        .into_response()
}

/// A one-shot gate: tasks wait until the test opens it.
#[derive(Clone)]
pub struct Gate {
    sender: Arc<tokio::sync::watch::Sender<bool>>,
}

impl Default for Gate {
    fn default() -> Self {
        Self::new()
    }
}

impl Gate {
    pub fn new() -> Self {
        Self {
            sender: Arc::new(tokio::sync::watch::channel(false).0),
        }
    }

    /// Release every current and future waiter.
    pub fn open(&self) {
        self.sender.send_replace(true);
    }

    pub fn is_open(&self) -> bool {
        *self.sender.borrow()
    }

    /// Wait until [`Gate::open`].
    pub async fn wait(&self) {
        let mut receiver = self.sender.subscribe();
        let _ = receiver.wait_for(|open| *open).await;
    }
}
