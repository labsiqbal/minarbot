//! OpenAI-compatible model whose answers can wait on the test.
//!
//! [`crate::model::ScriptedModel`] answers each request at once. Many flows
//! need the model step to stay open while the test changes server state:
//! a child agent that finishes only after the parent waits, a tool result
//! held across a restart, a cancel sent before the tool call arrives. A
//! [`GatedModel`] answers with an [`Answer`], which can
//!
//! - wait for a [`Gate`] the test opens later ([`Answer::after`]),
//! - hand the request to the test, which answers it after doing something
//!   else ([`Answer::Park`] and [`GatedModel::parked`]),
//! - stay open until the model stops ([`Answer::Hang`]), or
//! - stream deltas with gates between them ([`Answer::Stream`]).
//!
//! It also serves extra JSON routes such as `/v1/images/generations`
//! ([`GatedModel::route`]) and records every completion request.

use crate::{
    model::{ModelRequest, ToolCall},
    Result,
};
use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    convert::Infallible,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};

/// A one-way latch: closed until [`Gate::open`], then open for good.
#[derive(Clone, Debug)]
pub struct Gate(Arc<watch::Sender<bool>>);

impl Default for Gate {
    fn default() -> Self {
        Self::new()
    }
}

impl Gate {
    pub fn new() -> Self {
        Self(Arc::new(watch::channel(false).0))
    }

    pub fn open(&self) {
        self.0.send_replace(true);
    }

    pub fn is_open(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolve once the gate is open.
    pub async fn wait(&self) {
        until_true(self.0.subscribe()).await;
    }
}

/// Resolve once the watched flag is `true` (or its sender is gone).
async fn until_true(mut receiver: watch::Receiver<bool>) {
    while !*receiver.borrow_and_update() {
        if receiver.changed().await.is_err() {
            return;
        }
    }
}

/// One piece of a streamed answer.
#[derive(Clone, Debug)]
pub enum Chunk {
    /// A content delta.
    Text(String),
    /// A tool-call delta (whole call in one frame).
    Tool(ToolCall),
    /// Pause the stream until the gate opens.
    Wait(Gate),
}

/// How the model answers one request.
#[derive(Clone, Debug)]
pub enum Answer {
    /// Final answer text.
    Text(String),
    /// Tool calls (`finish_reason: tool_calls`).
    Tools(Vec<ToolCall>),
    /// HTTP error with a JSON body.
    Status(u16, Value),
    /// Server-sent events: chunks, then a final frame with this finish reason.
    Stream(Vec<Chunk>, String),
    /// Hand the request to the test ([`GatedModel::parked`]).
    Park,
    /// Answer once the gate opens.
    After(Gate, Box<Answer>),
    /// Never answer; the request stays open until the model stops.
    Hang,
}

impl Answer {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    /// One tool call with a fresh id.
    pub fn tool(name: &str, arguments: Value) -> Self {
        Self::Tools(vec![ToolCall::new(crate::new_id(), name, arguments)])
    }

    /// One tool call with an explicit id.
    pub fn tool_id(id: &str, name: &str, arguments: Value) -> Self {
        Self::Tools(vec![ToolCall::new(id, name, arguments)])
    }

    /// One tool call whose arguments are sent verbatim (may be malformed).
    pub fn raw_tool(name: &str, arguments: &str) -> Self {
        Self::Tools(vec![ToolCall {
            id: crate::new_id(),
            name: name.into(),
            arguments: arguments.into(),
        }])
    }

    pub fn after(gate: &Gate, answer: Answer) -> Self {
        Self::After(gate.clone(), Box::new(answer))
    }

    pub fn error(status: u16) -> Self {
        Self::Status(status, json!({"error": {"message": "scripted failure"}}))
    }
}

/// A request handed to the test by [`Answer::Park`].
pub struct Parked {
    pub request: ModelRequest,
    reply: oneshot::Sender<Answer>,
}

impl Parked {
    /// Answer the parked request (ignored if the client already left).
    pub fn answer(self, answer: Answer) {
        let _ = self.reply.send(answer);
    }
}

impl std::fmt::Debug for Parked {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Parked")
            .field("request", &self.request.index)
            .finish()
    }
}

type Responder = Arc<dyn Fn(&ModelRequest) -> Answer + Send + Sync>;
type Route = Arc<dyn Fn(&Value) -> (u16, Value) + Send + Sync>;

struct Script {
    responder: Option<Responder>,
    requests: Vec<ModelRequest>,
    routes: HashMap<String, Route>,
    route_calls: HashMap<String, usize>,
    catalog: Vec<String>,
}

#[derive(Clone)]
struct Shared {
    state: Arc<Mutex<Script>>,
    parked: mpsc::UnboundedSender<Parked>,
    stopping: watch::Receiver<bool>,
}

/// Running gated model; dropping it ends every waiting request.
pub struct GatedModel {
    origin: String,
    shared: Shared,
    parked: tokio::sync::Mutex<mpsc::UnboundedReceiver<Parked>>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl GatedModel {
    pub async fn start() -> Result<Self> {
        let (stop, stopping) = watch::channel(false);
        let (parked, parked_receiver) = mpsc::unbounded_channel();
        let shared = Shared {
            state: Arc::new(Mutex::new(Script {
                responder: None,
                requests: Vec::new(),
                routes: HashMap::new(),
                route_calls: HashMap::new(),
                catalog: vec!["fixture".into()],
            })),
            parked,
            stopping,
        };
        let app = Router::new()
            .fallback(dispatch)
            .with_state(shared.clone());
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let mut stopped = shared.stopping.clone();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stopped.wait_for(|stop| *stop).await;
                })
                .await;
        });
        Ok(Self {
            origin,
            shared,
            parked: tokio::sync::Mutex::new(parked_receiver),
            stop,
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

    fn with<T>(&self, change: impl FnOnce(&mut Script) -> T) -> T {
        match self.shared.state.lock() {
            Ok(mut state) => change(&mut state),
            Err(poisoned) => change(&mut poisoned.into_inner()),
        }
    }

    /// Answer every completion request with `responder` (replaces the last one).
    pub fn respond(&self, responder: impl Fn(&ModelRequest) -> Answer + Send + Sync + 'static) {
        let responder: Responder = Arc::new(responder);
        self.with(|state| state.responder = Some(responder));
    }

    /// Serve `POST path` (for example `/v1/images/generations`).
    pub fn route(
        &self,
        path: &str,
        handler: impl Fn(&Value) -> (u16, Value) + Send + Sync + 'static,
    ) {
        let handler: Route = Arc::new(handler);
        self.with(|state| state.routes.insert(path.into(), handler));
    }

    /// Requests served by an extra route so far.
    pub fn route_calls(&self, path: &str) -> usize {
        self.with(|state| state.route_calls.get(path).copied().unwrap_or(0))
    }

    pub fn catalog<I, S>(&self, ids: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let ids = ids.into_iter().map(Into::into).collect();
        self.with(|state| state.catalog = ids);
    }

    /// Every completion request so far.
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.with(|state| state.requests.clone())
    }

    pub fn calls(&self) -> usize {
        self.with(|state| state.requests.len())
    }

    /// Wait until at least `count` completion requests arrived.
    pub async fn wait_calls(&self, count: usize) -> Result<Vec<ModelRequest>> {
        crate::wait::until(format!("{count} model requests"), || async {
            let requests = self.requests();
            Ok((requests.len() >= count).then_some(requests))
        })
        .await
    }

    /// Next request the responder parked, within [`crate::WAIT`].
    pub async fn parked(&self) -> Result<Parked> {
        let mut receiver = self.parked.lock().await;
        tokio::time::timeout(crate::WAIT, receiver.recv())
            .await
            .map_err(|_| "timed out waiting for a parked model request")?
            .ok_or_else(|| "gated model stopped".into())
    }
}

impl Drop for GatedModel {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.task.abort();
    }
}

async fn dispatch(State(shared): State<Shared>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let authorization = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = match axum::body::to_bytes(request.into_body(), 64 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if path.ends_with("/models") {
        let ids = shared
            .state
            .lock()
            .map(|state| state.catalog.clone())
            .unwrap_or_default();
        let data: Vec<Value> = ids
            .iter()
            .map(|id| json!({"id": id, "object": "model"}))
            .collect();
        return Json(json!({"object": "list", "data": data})).into_response();
    }
    if path.ends_with("/chat/completions") {
        return completion(shared, authorization, bytes).await;
    }
    let route = shared.state.lock().ok().and_then(|mut state| {
        let route = state.routes.get(&path).cloned();
        if route.is_some() {
            *state.route_calls.entry(path.clone()).or_default() += 1;
        }
        route
    });
    match route {
        Some(route) => {
            let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            let (status, value) = route(&body);
            (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(value),
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn completion(shared: Shared, authorization: Option<String>, bytes: Bytes) -> Response {
    let body: Value = match serde_json::from_slice(&bytes) {
        Ok(body) => body,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, format!("invalid JSON: {error}")).into_response()
        }
    };
    let (request, responder) = {
        let Ok(mut state) = shared.state.lock() else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        let request = ModelRequest {
            index: state.requests.len(),
            body,
            authorization,
        };
        state.requests.push(request.clone());
        (request, state.responder.clone())
    };
    let Some(responder) = responder else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "GatedModel has no responder"}})),
        )
            .into_response();
    };
    let answer = responder(&request);
    render(shared, request, answer).await
}

fn render(
    shared: Shared,
    request: ModelRequest,
    answer: Answer,
) -> Pin<Box<dyn Future<Output = Response> + Send>> {
    Box::pin(async move {
        let stopping = shared.stopping.clone();
        match answer {
            Answer::Hang => {
                until_true(stopping).await;
                StatusCode::SERVICE_UNAVAILABLE.into_response()
            }
            Answer::After(gate, next) => {
                tokio::select! {
                    () = gate.wait() => render(shared, request, *next).await,
                    () = until_true(stopping) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
                }
            }
            Answer::Park => {
                let (reply, receiver) = oneshot::channel();
                if shared
                    .parked
                    .send(Parked {
                        request: request.clone(),
                        reply,
                    })
                    .is_err()
                {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                tokio::select! {
                    answer = receiver => match answer {
                        Ok(answer) => render(shared, request, answer).await,
                        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
                    },
                    () = until_true(stopping) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
                }
            }
            Answer::Status(status, body) => (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(body),
            )
                .into_response(),
            Answer::Text(text) if request.stream() => {
                stream(shared, vec![Chunk::Text(text)], "stop".into())
            }
            Answer::Tools(calls) if request.stream() => stream(
                shared,
                calls.into_iter().map(Chunk::Tool).collect(),
                "tool_calls".into(),
            ),
            Answer::Text(text) => Json(json!({"choices": [{"index": 0, "finish_reason": "stop",
                "message": {"role": "assistant", "content": text}}]}))
            .into_response(),
            Answer::Tools(calls) => Json(json!({"choices": [{"index": 0, "finish_reason": "tool_calls",
                "message": {"role": "assistant", "content": null, "tool_calls": tool_json(&calls)}}]}))
            .into_response(),
            Answer::Stream(chunks, finish) => stream(shared, chunks, finish),
        }
    })
}

fn tool_json(calls: &[ToolCall]) -> Vec<Value> {
    calls
        .iter()
        .map(|call| json!({"id": call.id, "type": "function", "function": {"name": call.name, "arguments": call.arguments}}))
        .collect()
}

/// Server-sent events written by a task, so gates can pause between frames.
fn stream(shared: Shared, chunks: Vec<Chunk>, finish: String) -> Response {
    let (sender, receiver) = mpsc::channel::<Bytes>(16);
    let stopping = shared.stopping.clone();
    tokio::spawn(async move {
        let frame = |value: Value| Bytes::from(format!("data: {value}\n\n"));
        let mut tool_index = 0;
        for chunk in chunks {
            let value = match chunk {
                Chunk::Text(text) => {
                    json!({"choices": [{"index": 0, "delta": {"role": "assistant", "content": text}, "finish_reason": null}]})
                }
                Chunk::Tool(call) => {
                    let value = json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": tool_index,
                        "id": call.id, "type": "function", "function": {"name": call.name, "arguments": call.arguments}}]},
                        "finish_reason": null}]});
                    tool_index += 1;
                    value
                }
                Chunk::Wait(gate) => {
                    tokio::select! {
                        () = gate.wait() => continue,
                        () = until_true(stopping.clone()) => return,
                    }
                }
            };
            if sender.send(frame(value)).await.is_err() {
                return;
            }
        }
        let last = json!({"choices": [{"index": 0, "delta": {}, "finish_reason": finish}]});
        let _ = sender.send(frame(last)).await;
        let _ = sender.send(Bytes::from_static(b"data: [DONE]\n\n")).await;
    });
    let body = futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver
            .recv()
            .await
            .map(|bytes| (Ok::<_, Infallible>(bytes), receiver))
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
