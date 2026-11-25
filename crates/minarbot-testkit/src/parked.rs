//! OpenAI-compatible model whose requests a test parks and answers later.
//!
//! [`crate::model::ScriptedModel`] answers each request when it arrives (or
//! holds it until the model stops). Some flows need a request to stay open
//! while the test changes server state, and then a chosen answer: parallel
//! workflow branches finished in a given order, a step held across a server
//! restart, or a step cancelled while its provider call is open. The routing
//! closure answers a request at once ([`Route::Now`]) or parks it under a key
//! ([`Route::Park`]); [`ParkedModel::finish`] answers the newest live request
//! parked under that key. A parked request whose client went away (for
//! example the gateway peer of a stopped server) no longer counts as parked.
//!
//! ```no_run
//! # use minarbot_testkit::parked::{Answer, ParkedModel, Route};
//! # async fn example() -> minarbot_testkit::Result<()> {
//! let model = ParkedModel::start(|request| Route::Park(request.model().to_owned())).await?;
//! model.wait_parked(&["a", "b"]).await?;
//! model.finish("b", Answer::text("B result"))?;
//! # Ok(()) }
//! ```

use crate::{model::ModelRequest, Result};
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
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::{
    net::TcpListener,
    sync::{oneshot, watch},
    task::JoinHandle,
};

/// The answer to one model request.
#[derive(Clone, Debug)]
pub enum Answer {
    /// HTTP 200 with assistant text and `finish_reason: stop`.
    Text(String),
    /// HTTP 500 carrying the same completion body (a provider failure).
    Fail(String),
    /// One tool call (`finish_reason: tool_calls`) with JSON arguments.
    Tool(String, Value),
}

impl Answer {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    pub fn fail(text: impl Into<String>) -> Self {
        Self::Fail(text.into())
    }

    pub fn tool(name: impl Into<String>, arguments: Value) -> Self {
        Self::Tool(name.into(), arguments)
    }

    fn message(&self) -> (Value, &'static str) {
        match self {
            Self::Text(text) | Self::Fail(text) => {
                (json!({"role": "assistant", "content": text}), "stop")
            }
            Self::Tool(name, arguments) => (
                json!({"role": "assistant", "content": null, "tool_calls": [{
                    "id": crate::new_id(), "type": "function",
                    "function": {"name": name, "arguments": arguments.to_string()}}]}),
                "tool_calls",
            ),
        }
    }

    fn response(&self, stream: bool) -> Response {
        let (message, finish) = self.message();
        let status = match self {
            Self::Fail(_) => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::OK,
        };
        if !stream || status != StatusCode::OK {
            let body =
                json!({"choices": [{"index": 0, "finish_reason": finish, "message": message}]});
            return (status, Json(body)).into_response();
        }
        let mut delta = message;
        if let Some(calls) = delta["tool_calls"].as_array_mut() {
            for (index, call) in calls.iter_mut().enumerate() {
                call["index"] = json!(index);
            }
        }
        let frames = [
            json!({"choices": [{"index": 0, "delta": delta, "finish_reason": null}]}),
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": finish}]}),
        ];
        let mut text: String = frames
            .iter()
            .map(|frame| format!("data: {frame}\n\n"))
            .collect();
        text.push_str("data: [DONE]\n\n");
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(text))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    }
}

/// What the routing closure does with a request.
#[derive(Clone, Debug)]
pub enum Route {
    Now(Answer),
    Park(String),
}

type RouteFn = Arc<dyn Fn(&ModelRequest) -> Route + Send + Sync>;

struct Parked {
    index: usize,
    reply: oneshot::Sender<Answer>,
}

struct Script {
    route: RouteFn,
    requests: Vec<ModelRequest>,
    parked: HashMap<String, Vec<Parked>>,
}

impl Script {
    /// Live parked requests under `key`, dropping those whose client left.
    fn live(&mut self, key: &str) -> Option<&mut Vec<Parked>> {
        let list = self.parked.get_mut(key)?;
        list.retain(|parked| !parked.reply.is_closed());
        Some(list).filter(|list| !list.is_empty())
    }
}

#[derive(Clone)]
struct Shared {
    state: Arc<Mutex<Script>>,
    stopping: watch::Receiver<bool>,
}

/// Running parked model. Dropping it fails every parked request and stops it.
pub struct ParkedModel {
    origin: String,
    shared: Shared,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl ParkedModel {
    pub async fn start(
        route: impl Fn(&ModelRequest) -> Route + Send + Sync + 'static,
    ) -> Result<Self> {
        let (stop, stopping) = watch::channel(false);
        let shared = Shared {
            state: Arc::new(Mutex::new(Script {
                route: Arc::new(route),
                requests: Vec::new(),
                parked: HashMap::new(),
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

    /// Replace the routing closure.
    pub fn route(&self, route: impl Fn(&ModelRequest) -> Route + Send + Sync + 'static) {
        let route: RouteFn = Arc::new(route);
        self.with(|state| state.route = route);
    }

    /// Every completion request so far, in arrival order.
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.with(|state| state.requests.clone())
    }

    pub fn calls(&self) -> usize {
        self.with(|state| state.requests.len())
    }

    /// Whether a live request is parked under `key`.
    pub fn parked(&self, key: &str) -> bool {
        self.with(|state| state.live(key).is_some())
    }

    /// The newest live request parked under `key`.
    pub fn parked_request(&self, key: &str) -> Option<ModelRequest> {
        self.with(|state| {
            let index = state.live(key)?.last()?.index;
            state.requests.get(index).cloned()
        })
    }

    /// Wait until a live request is parked under every key.
    pub async fn wait_parked(&self, keys: &[&str]) -> Result<()> {
        crate::wait::until(format!("parked {keys:?}"), || async {
            Ok(keys.iter().all(|key| self.parked(key)).then_some(()))
        })
        .await
    }

    /// Answer the newest live request parked under `key`.
    pub fn finish(&self, key: &str, answer: Answer) -> Result<()> {
        let parked = self
            .with(|state| state.live(key).and_then(Vec::pop))
            .ok_or_else(|| format!("no request parked under {key}"))?;
        parked
            .reply
            .send(answer)
            .map_err(|_| format!("parked request {key} went away").into())
    }

    /// Forget every parked request; their clients get HTTP 503.
    pub fn clear(&self) {
        self.with(|state| state.parked.clear());
    }
}

impl Drop for ParkedModel {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.task.abort();
    }
}

async fn models() -> Response {
    Json(json!({"object": "list", "data": [{"id": "fixture", "object": "model"}]})).into_response()
}

async fn completion(State(shared): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    let body: Value = match serde_json::from_slice(&body) {
        Ok(body) => body,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, format!("invalid JSON: {error}")).into_response()
        }
    };
    let (request, waiting) = {
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
        let route = state.route.clone();
        match route(&request) {
            Route::Now(answer) => return answer.response(request.stream()),
            Route::Park(key) => {
                let (reply, waiting) = oneshot::channel();
                state.parked.entry(key).or_default().push(Parked {
                    index: request.index,
                    reply,
                });
                (request, waiting)
            }
        }
    };
    let mut stopping = shared.stopping.clone();
    tokio::select! {
        answer = waiting => match answer {
            Ok(answer) => answer.response(request.stream()),
            Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        },
        _ = stopping.wait_for(|stop| *stop) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
