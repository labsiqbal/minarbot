//! OpenAI-compatible model whose held requests the test answers later.
//!
//! [`crate::model::ScriptedModel`] answers every request at once (or holds it
//! until the model stops). Some Node cases held one provider response, changed
//! server state, and then finished that same response with a chosen body (for
//! example a stale compaction summary). [`ControlModel`] keeps that ability:
//! its responder returns [`Answer::Defer`] for such a request, and the test
//! takes it with [`ControlModel::deferred`] and answers it with
//! [`Deferred::answer`] whenever it wants.
//!
//! Like `ScriptedModel` it serves `POST /v1/chat/completions` and
//! `GET /v1/models`, records every completion request, and the kit's
//! `fake-gateway` peer reaches it through `ServerConfig::gateway`.

use crate::{model::ModelRequest, server::Gateway, Result};
use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio::{
    net::TcpListener,
    sync::{oneshot, watch},
    task::JoinHandle,
};

/// One completion answer.
#[derive(Clone, Debug)]
pub enum Answer {
    /// Assistant text, `finish_reason: stop`.
    Text(String),
    /// One tool call with JSON arguments, `finish_reason: tool_calls`.
    Tool {
        id: String,
        name: String,
        arguments: Value,
    },
    /// One tool call whose `arguments` string is sent verbatim (for
    /// arguments JSON cannot model, such as a lone UTF-16 surrogate escape).
    RawTool { name: String, arguments: String },
    /// HTTP status with a JSON body.
    Status(u16, Value),
    /// Raw body text with status 200 (for malformed provider output).
    Raw(String),
    /// `302 Found` to `location`.
    Redirect(String),
    /// Keep the request open until the test answers it.
    Defer,
    /// Stream one SSE content delta, then keep the response open with no
    /// further bytes (a provider that stalls mid-answer). Ends when the
    /// client disconnects or the model stops.
    Stall(String),
    /// Stream one content delta, then an SSE `error` event, then end.
    StreamError(String, String),
    /// Stream one content delta, then break the connection.
    StreamCut(String),
}

impl Answer {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    pub fn tool(name: &str, arguments: Value) -> Self {
        Self::Tool {
            id: format!("call-{name}"),
            name: name.into(),
            arguments,
        }
    }

    /// Server-sent events for a streamed request (`stream: true`).
    fn events(choice: &Value, usage: Option<&Value>) -> Response {
        let message = &choice["message"];
        let mut frames = Vec::new();
        if let Some(text) = message["content"].as_str() {
            frames.push(json!({"choices": [{"index": 0, "delta": {"role": "assistant", "content": text}, "finish_reason": null}]}));
        }
        for (index, call) in message["tool_calls"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let mut call = call.clone();
            call["index"] = json!(index);
            frames.push(json!({"choices": [{"index": 0, "delta": {"tool_calls": [call]}, "finish_reason": null}]}));
        }
        let mut last = json!({"choices": [{"index": 0, "delta": {}, "finish_reason": choice["finish_reason"]}]});
        if let Some(usage) = usage {
            last["usage"] = usage.clone();
        }
        frames.push(last);
        let mut body: String = frames
            .iter()
            .map(|frame| format!("data: {frame}\n\n"))
            .collect();
        body.push_str("data: [DONE]\n\n");
        ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
    }

    fn response(self, usage: Option<(u64, u64)>) -> Response {
        self.respond(usage, false)
    }

    fn respond(self, usage: Option<(u64, u64)>, stream: bool) -> Response {
        let usage = usage.map(|(prompt, completion)| {
            json!({"prompt_tokens": prompt, "completion_tokens": completion,
                "total_tokens": prompt + completion})
        });
        let choice = match self {
            Self::Text(text) => {
                json!({"index": 0, "finish_reason": "stop", "message": {"role": "assistant", "content": text}})
            }
            Self::Tool {
                id,
                name,
                arguments,
            } => json!({"index": 0, "finish_reason": "tool_calls", "message": {"role": "assistant",
                "content": null, "tool_calls": [{"id": id, "type": "function",
                "function": {"name": name, "arguments": arguments.to_string()}}]}}),
            Self::RawTool { name, arguments } => json!({"index": 0, "finish_reason": "tool_calls",
                "message": {"role": "assistant", "content": null, "tool_calls": [{"id": format!("call-{name}"),
                "type": "function", "function": {"name": name, "arguments": arguments}}]}}),
            Self::Status(status, body) => {
                let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
                return (status, Json(body)).into_response();
            }
            Self::Raw(body) => return (StatusCode::OK, body).into_response(),
            Self::Redirect(location) => {
                return (StatusCode::FOUND, [(header::LOCATION, location)], "").into_response()
            }
            Self::StreamError(text, message) => {
                let frame = json!({"choices": [{"index": 0, "delta": {"role": "assistant",
                    "content": text}, "finish_reason": null}]});
                let body = format!(
                    "data: {frame}\n\nevent: error\ndata: {}\n\n",
                    json!({"error": {"message": message}})
                );
                return ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response();
            }
            Self::StreamCut(text) => {
                let frame = json!({"choices": [{"index": 0, "delta": {"role": "assistant",
                    "content": text}, "finish_reason": null}]});
                let first = futures_util::stream::once(async move {
                    Ok::<_, std::io::Error>(format!("data: {frame}\n\n"))
                });
                let cut = futures_util::stream::once(async {
                    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                    Err::<String, std::io::Error>(std::io::Error::other("stream cut"))
                });
                let body = Body::from_stream(first.chain(cut));
                return ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response();
            }
            Self::Stall(text) => {
                let frame = json!({"choices": [{"index": 0, "delta": {"role": "assistant",
                    "content": text}, "finish_reason": null}]});
                let first = futures_util::stream::once(async move {
                    Ok::<_, std::convert::Infallible>(format!("data: {frame}\n\n"))
                });
                let body = Body::from_stream(first.chain(futures_util::stream::pending()));
                return ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response();
            }
            Self::Defer => {
                return (StatusCode::INTERNAL_SERVER_ERROR, "deferred twice").into_response()
            }
        };
        if stream {
            return Self::events(&choice, usage.as_ref());
        }
        let mut body = json!({"choices": [choice]});
        if let Some(usage) = usage {
            body["usage"] = usage;
        }
        Json(body).into_response()
    }
}

type Responder = Arc<dyn Fn(&ModelRequest) -> Answer + Send + Sync>;

/// A request the responder deferred. Dropping it without an answer fails the
/// request with HTTP 500, like a provider that closes the connection.
pub struct Deferred {
    pub request: ModelRequest,
    sender: oneshot::Sender<Answer>,
}

impl Deferred {
    /// Finish the held request with `answer`.
    pub fn answer(self, answer: Answer) {
        let _ = self.sender.send(answer);
    }
}

type CatalogResponder = Arc<dyn Fn() -> Answer + Send + Sync>;

struct Script {
    responder: Option<Responder>,
    catalog_responder: Option<CatalogResponder>,
    catalog_calls: usize,
    requests: Vec<ModelRequest>,
    deferred: VecDeque<Deferred>,
    catalog: Vec<String>,
    usage: Option<(u64, u64)>,
}

#[derive(Clone)]
struct Shared {
    state: Arc<Mutex<Script>>,
}

impl Shared {
    fn with<T>(&self, change: impl FnOnce(&mut Script) -> T) -> T {
        match self.state.lock() {
            Ok(mut state) => change(&mut state),
            Err(poisoned) => change(&mut poisoned.into_inner()),
        }
    }
}

/// Running control model. Dropping it stops the listener.
pub struct ControlModel {
    origin: String,
    shared: Shared,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl ControlModel {
    pub async fn start() -> Result<Self> {
        let (stop, mut stopping) = watch::channel(false);
        let shared = Shared {
            state: Arc::new(Mutex::new(Script {
                responder: None,
                catalog_responder: None,
                catalog_calls: 0,
                requests: Vec::new(),
                deferred: VecDeque::new(),
                catalog: vec!["fixture".into()],
                usage: None,
            })),
        };
        let app = Router::new()
            .route("/v1/chat/completions", post(completion))
            .route("/v1/models", get(models))
            .with_state(shared.clone());
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stopping.wait_for(|stop| *stop).await;
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

    /// OpenAI-compatible base URL (`<origin>/v1`).
    pub fn base_url(&self) -> String {
        format!("{}/v1", self.origin)
    }

    /// Gateway options pointing `fake-gateway` at this model.
    pub fn gateway(&self) -> Gateway {
        Gateway::new(self.base_url())
    }

    /// Answer every completion request.
    pub fn respond(&self, responder: impl Fn(&ModelRequest) -> Answer + Send + Sync + 'static) {
        let responder: Responder = Arc::new(responder);
        self.shared.with(|state| state.responder = Some(responder));
    }

    /// Usage attached to every text and tool answer.
    pub fn usage(&self, prompt: u64, completion: u64) {
        self.set_usage(Some((prompt, completion)));
    }

    /// Usage attached to later answers; `None` omits the `usage` field.
    pub fn set_usage(&self, usage: Option<(u64, u64)>) {
        self.shared.with(|state| state.usage = usage);
    }

    /// Answer `GET /v1/models` with `responder` instead of the catalog:
    /// `Raw`, `Status`, `Redirect` and `Defer` (a held request whose
    /// [`Deferred::request`] body is `{"catalog": true}`); other answers
    /// fall back to the catalog.
    pub fn respond_catalog(&self, responder: impl Fn() -> Answer + Send + Sync + 'static) {
        let responder: CatalogResponder = Arc::new(responder);
        self.shared
            .with(|state| state.catalog_responder = Some(responder));
    }

    /// Number of `GET /v1/models` requests so far.
    pub fn catalog_calls(&self) -> usize {
        self.shared.with(|state| state.catalog_calls)
    }

    /// Model ids listed by `GET /v1/models`.
    pub fn catalog<I, S>(&self, ids: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let ids = ids.into_iter().map(Into::into).collect();
        self.shared.with(|state| state.catalog = ids);
    }

    /// Every completion request so far.
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.shared.with(|state| state.requests.clone())
    }

    pub fn calls(&self) -> usize {
        self.shared.with(|state| state.requests.len())
    }

    /// Requests currently deferred and not yet taken.
    pub fn pending(&self) -> usize {
        self.shared.with(|state| state.deferred.len())
    }

    /// Wait for the next deferred request and take it.
    pub async fn deferred(&self) -> Result<Deferred> {
        crate::wait::until("a deferred model request", || async {
            Ok(self.shared.with(|state| state.deferred.pop_front()))
        })
        .await
    }

    /// Wait until at least `count` completion requests arrived.
    pub async fn wait_calls(&self, count: usize) -> Result<Vec<ModelRequest>> {
        crate::wait::until(format!("{count} model requests"), || async {
            let requests = self.requests();
            Ok((requests.len() >= count).then_some(requests))
        })
        .await
    }
}

impl Drop for ControlModel {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.task.abort();
    }
}

async fn completion(
    State(shared): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let (request, responder, usage) = shared.with(|state| {
        let request = ModelRequest {
            index: state.requests.len(),
            body,
            authorization,
        };
        state.requests.push(request.clone());
        (request, state.responder.clone(), state.usage)
    });
    let answer = responder.map_or_else(|| Answer::text("ok"), |responder| responder(&request));
    let stream = request.body["stream"] == true;
    match answer {
        Answer::Defer => {
            let (sender, receiver) = oneshot::channel();
            shared.with(|state| state.deferred.push_back(Deferred { request, sender }));
            match receiver.await {
                Ok(answer) => answer.respond(usage, stream),
                Err(_) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "deferred request dropped",
                )
                    .into_response(),
            }
        }
        answer => answer.respond(usage, stream),
    }
}

async fn models(State(shared): State<Shared>, headers: HeaderMap) -> Response {
    let (responder, index) = shared.with(|state| {
        state.catalog_calls += 1;
        (state.catalog_responder.clone(), state.catalog_calls)
    });
    if let Some(responder) = responder {
        match responder() {
            Answer::Defer => {
                let (sender, receiver) = oneshot::channel();
                let request = ModelRequest {
                    index,
                    body: json!({"catalog": true}),
                    authorization: headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned),
                };
                shared.with(|state| state.deferred.push_back(Deferred { request, sender }));
                return match receiver.await {
                    Ok(answer) => answer.response(None),
                    Err(_) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "deferred request dropped",
                    )
                        .into_response(),
                };
            }
            answer @ (Answer::Raw(_) | Answer::Status(..) | Answer::Redirect(_)) => {
                return answer.response(None)
            }
            _ => {}
        }
    }
    let data: Vec<Value> = shared.with(|state| {
        state
            .catalog
            .iter()
            .map(|id| json!({"id": id, "object": "model"}))
            .collect()
    });
    Json(json!({"object": "list", "data": data})).into_response()
}
