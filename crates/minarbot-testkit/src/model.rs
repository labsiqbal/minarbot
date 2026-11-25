//! Scripted OpenAI-compatible model on loopback.
//!
//! `fake-gateway` (the Codex peer used for `runtime: "api"` agents) sends
//! every model step here as `POST /v1/chat/completions`. A test scripts the
//! answers either as a queue ([`ScriptedModel::push`]) or with a responder
//! closure ([`ScriptedModel::respond`]) that sees each [`ModelRequest`].
//! Queued replies win over the responder. Every request is recorded.
//!
//! ```no_run
//! # use minarbot_testkit::model::{Reply, ScriptedModel};
//! # async fn example() -> minarbot_testkit::Result<()> {
//! let model = ScriptedModel::start().await?;
//! model.push(Reply::tool("remember_memory", serde_json::json!({"text": "x"})));
//! model.push(Reply::deltas(["Hel", "lo"]).usage(20, 10));
//! model.respond(|request| match request.last_text().as_str() {
//!     "hold" => Reply::hold(),
//!     text => Reply::text(format!("reply:{text}")),
//! });
//! # Ok(()) }
//! ```

use crate::Result;
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, sync::watch, task::JoinHandle};

/// One recorded request.
#[derive(Clone, Debug)]
pub struct ModelRequest {
    /// Zero-based arrival order.
    pub index: usize,
    pub body: Value,
    pub authorization: Option<String>,
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

impl ModelRequest {
    pub fn model(&self) -> &str {
        self.body["model"].as_str().unwrap_or_default()
    }

    pub fn messages(&self) -> &[Value] {
        self.body["messages"].as_array().map_or(&[], Vec::as_slice)
    }

    /// Text content of the last message (any role).
    pub fn last_text(&self) -> String {
        self.messages()
            .last()
            .map(|message| text_of(&message["content"]))
            .unwrap_or_default()
    }

    /// Text content of the last `user` message.
    pub fn last_user_text(&self) -> String {
        self.messages()
            .iter()
            .rev()
            .find(|message| message["role"] == "user")
            .map(|message| text_of(&message["content"]))
            .unwrap_or_default()
    }

    /// Text content of every message, in order.
    pub fn texts(&self) -> Vec<String> {
        self.messages()
            .iter()
            .map(|message| text_of(&message["content"]))
            .collect()
    }

    /// Text content of the first (system) message.
    pub fn system(&self) -> String {
        self.messages()
            .first()
            .map(|message| text_of(&message["content"]))
            .unwrap_or_default()
    }

    /// Names of the tools offered to the model, in request order.
    pub fn tool_names(&self) -> Vec<String> {
        self.body["tools"]
            .as_array()
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every tool call the conversation already contains (assistant messages).
    pub fn tool_calls(&self) -> Vec<Value> {
        self.messages()
            .iter()
            .filter_map(|message| message["tool_calls"].as_array())
            .flatten()
            .cloned()
            .collect()
    }

    /// Whether a previous assistant message already called `name`.
    pub fn called(&self, name: &str) -> bool {
        self.tool_calls()
            .iter()
            .any(|call| call["function"]["name"] == name)
    }

    /// Messages with role `tool`.
    pub fn tool_results(&self) -> Vec<Value> {
        self.messages()
            .iter()
            .filter(|message| message["role"] == "tool")
            .cloned()
            .collect()
    }

    /// Whether the serialized messages contain `needle`.
    pub fn mentions(&self, needle: &str) -> bool {
        self.body["messages"].to_string().contains(needle)
    }

    pub fn stream(&self) -> bool {
        self.body["stream"] == true
    }
}

/// One model tool call.
#[derive(Clone, Debug)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// JSON-encoded arguments, sent verbatim (may be deliberately malformed).
    pub arguments: String,
}

impl ToolCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: Value) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments: arguments.to_string(),
        }
    }
}

#[derive(Clone, Debug)]
enum Step {
    Text(Vec<String>),
    Tools(Vec<ToolCall>),
    Status(u16, Value),
    Hold,
}

/// One scripted answer.
#[derive(Clone, Debug)]
pub struct Reply {
    step: Step,
    usage: Option<(u64, u64)>,
    delay: Option<Duration>,
    finish_reason: Option<String>,
}

impl Reply {
    fn of(step: Step) -> Self {
        Self {
            step,
            usage: None,
            delay: None,
            finish_reason: None,
        }
    }

    /// Final answer `text` (one delta when the request streams).
    pub fn text(text: impl Into<String>) -> Self {
        Self::of(Step::Text(vec![text.into()]))
    }

    /// Final answer streamed as these deltas (joined when not streaming).
    pub fn deltas<I, S>(deltas: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::of(Step::Text(deltas.into_iter().map(Into::into).collect()))
    }

    /// One tool call whose id equals the tool name.
    pub fn tool(name: &str, arguments: Value) -> Self {
        Self::tool_calls(vec![ToolCall::new(name, name, arguments)])
    }

    /// One tool call with an explicit id.
    pub fn tool_call(id: &str, name: &str, arguments: Value) -> Self {
        Self::tool_calls(vec![ToolCall::new(id, name, arguments)])
    }

    pub fn tool_calls(calls: Vec<ToolCall>) -> Self {
        Self::of(Step::Tools(calls))
    }

    /// HTTP error `status` with a JSON `body`.
    pub fn status(status: u16, body: Value) -> Self {
        Self::of(Step::Status(status, body))
    }

    /// Never answer; the request stays open until the client gives up or the
    /// model stops. Use it for "still running" states.
    pub fn hold() -> Self {
        Self::of(Step::Hold)
    }

    /// Report `prompt_tokens` and `completion_tokens` usage.
    pub fn usage(mut self, prompt: u64, completion: u64) -> Self {
        self.usage = Some((prompt, completion));
        self
    }

    /// Wait before answering.
    pub fn delay(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }

    /// Override `finish_reason` (default `stop`, or `tool_calls` for tools).
    pub fn finish_reason(mut self, reason: impl Into<String>) -> Self {
        self.finish_reason = Some(reason.into());
        self
    }

    fn finish(&self) -> String {
        self.finish_reason
            .clone()
            .unwrap_or_else(|| match self.step {
                Step::Tools(_) => "tool_calls".into(),
                _ => "stop".into(),
            })
    }

    fn usage_json(&self) -> Option<Value> {
        self.usage.map(|(prompt, completion)| {
            json!({"prompt_tokens": prompt, "completion_tokens": completion, "total_tokens": prompt + completion})
        })
    }

    fn tool_json(calls: &[ToolCall]) -> Vec<Value> {
        calls
            .iter()
            .map(|call| json!({"id": call.id, "type": "function", "function": {"name": call.name, "arguments": call.arguments}}))
            .collect()
    }

    fn completion(&self) -> Value {
        let message = match &self.step {
            Step::Text(deltas) => json!({"role": "assistant", "content": deltas.concat()}),
            Step::Tools(calls) => {
                json!({"role": "assistant", "content": null, "tool_calls": Self::tool_json(calls)})
            }
            Step::Status(..) | Step::Hold => json!({}),
        };
        let mut body =
            json!({"choices": [{"index": 0, "finish_reason": self.finish(), "message": message}]});
        if let Some(usage) = self.usage_json() {
            body["usage"] = usage;
        }
        body
    }

    fn events(&self) -> String {
        let mut frames = Vec::new();
        match &self.step {
            Step::Text(deltas) => {
                for delta in deltas {
                    frames.push(json!({"choices": [{"index": 0, "delta": {"role": "assistant", "content": delta}, "finish_reason": null}]}));
                }
            }
            Step::Tools(calls) => {
                for (index, call) in calls.iter().enumerate() {
                    frames.push(json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": index, "id": call.id, "type": "function", "function": {"name": call.name, "arguments": call.arguments}}]}, "finish_reason": null}]}));
                }
            }
            Step::Status(..) | Step::Hold => {}
        }
        let mut last =
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": self.finish()}]});
        if let Some(usage) = self.usage_json() {
            last["usage"] = usage;
        }
        frames.push(last);
        let mut out: String = frames
            .iter()
            .map(|frame| format!("data: {frame}\n\n"))
            .collect();
        out.push_str("data: [DONE]\n\n");
        out
    }
}

type Responder = Arc<dyn Fn(&ModelRequest) -> Reply + Send + Sync>;

struct Script {
    queue: VecDeque<Reply>,
    responder: Option<Responder>,
    requests: Vec<ModelRequest>,
    catalog: Vec<String>,
    catalog_status: u16,
    held: usize,
}

#[derive(Clone)]
struct Shared {
    state: Arc<Mutex<Script>>,
    stopping: watch::Receiver<bool>,
}

/// Running scripted model. Dropping it releases held requests and stops it.
pub struct ScriptedModel {
    origin: String,
    shared: Shared,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl ScriptedModel {
    pub async fn start() -> Result<Self> {
        let (stop, stopping) = watch::channel(false);
        let shared = Shared {
            state: Arc::new(Mutex::new(Script {
                queue: VecDeque::new(),
                responder: None,
                requests: Vec::new(),
                catalog: vec!["fixture".into()],
                catalog_status: 200,
                held: 0,
            })),
            stopping,
        };
        let app = Router::new()
            .route("/v1/chat/completions", post(completion))
            .route("/v1/models", get(models))
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
            stop,
            task,
        })
    }

    /// `http://127.0.0.1:<port>`.
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

    /// Queue one reply; queued replies are used before the responder.
    pub fn push(&self, reply: Reply) {
        self.with(|state| state.queue.push_back(reply));
    }

    /// Answer every request that finds the queue empty.
    pub fn respond(&self, responder: impl Fn(&ModelRequest) -> Reply + Send + Sync + 'static) {
        let responder: Responder = Arc::new(responder);
        self.with(|state| state.responder = Some(responder));
    }

    /// Model ids listed by `GET /v1/models`.
    pub fn catalog<I, S>(&self, ids: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let ids = ids.into_iter().map(Into::into).collect();
        self.with(|state| state.catalog = ids);
    }

    /// Make `GET /v1/models` fail with HTTP 502 (`true`) or succeed again.
    pub fn fail_catalog(&self, fail: bool) {
        self.with(|state| state.catalog_status = if fail { 502 } else { 200 });
    }

    /// Every completion request so far.
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.with(|state| state.requests.clone())
    }

    /// Number of completion requests so far.
    pub fn calls(&self) -> usize {
        self.with(|state| state.requests.len())
    }

    /// Requests currently held open by [`Reply::hold`].
    pub fn held(&self) -> usize {
        self.with(|state| state.held)
    }

    /// Wait until at least `count` requests arrived; returns them all.
    pub async fn wait_calls(&self, count: usize) -> Result<Vec<ModelRequest>> {
        crate::wait::until(format!("{count} model requests"), || async {
            let requests = self.requests();
            Ok((requests.len() >= count).then_some(requests))
        })
        .await
    }
}

impl Drop for ScriptedModel {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.task.abort();
    }
}

/// Decrements the held count when a held request ends, including when the
/// client disconnects and axum drops the handler.
struct Held(Arc<Mutex<Script>>);

impl Drop for Held {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.lock() {
            state.held = state.held.saturating_sub(1);
        }
    }
}

async fn models(State(shared): State<Shared>) -> Response {
    let (status, ids) = match shared.state.lock() {
        Ok(state) => (state.catalog_status, state.catalog.clone()),
        Err(_) => (500, Vec::new()),
    };
    let data: Vec<Value> = ids
        .iter()
        .map(|id| json!({"id": id, "object": "model"}))
        .collect();
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(json!({"object": "list", "data": data})),
    )
        .into_response()
}

async fn completion(State(shared): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    let body: Value = match serde_json::from_slice(&body) {
        Ok(body) => body,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, format!("invalid JSON: {error}")).into_response()
        }
    };
    let (request, reply) = {
        let Ok(mut state) = shared.state.lock() else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        let request = ModelRequest {
            index: state.requests.len(),
            body,
            authorization: headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
        };
        state.requests.push(request.clone());
        let reply = state
            .queue
            .pop_front()
            .or_else(|| state.responder.clone().map(|respond| respond(&request)));
        if matches!(
            reply,
            Some(Reply {
                step: Step::Hold,
                ..
            })
        ) {
            state.held += 1;
        }
        (request, reply)
    };
    let Some(reply) = reply else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": format!("ScriptedModel has no reply for request #{}", request.index)}})),
        )
            .into_response();
    };
    if let Some(delay) = reply.delay {
        tokio::time::sleep(delay).await;
    }
    match &reply.step {
        Step::Hold => {
            let _held = Held(shared.state.clone());
            let mut stopping = shared.stopping.clone();
            let _ = stopping.wait_for(|stop| *stop).await;
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
        Step::Status(status, body) => (
            StatusCode::from_u16(*status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(body.clone()),
        )
            .into_response(),
        _ if request.stream() => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(reply.events()))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        _ => Json(reply.completion()).into_response(),
    }
}
