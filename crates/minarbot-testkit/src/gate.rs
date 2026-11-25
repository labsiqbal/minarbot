//! Model gate: holds one scripted model reply until the test releases it.
//!
//! [`ModelGate`] sits between `fake-gateway` and a [`ScriptedModel`]. It
//! forwards every request at once, but when armed it keeps the next reply
//! back after the model answered, until [`ModelGate::release`]. A test uses
//! it to act exactly between "the model chose a tool" and "the server runs
//! the tool" (a user edit, a cancellation), as the Node providers did with an
//! `await` before replying.
//!
//! ```no_run
//! # use minarbot_testkit::{gate::ModelGate, model::ScriptedModel, prelude::*};
//! # async fn example() -> Result<()> {
//! let model = ScriptedModel::start().await?;
//! let gate = ModelGate::start(&model).await?;
//! let app = TestRoot::new()?.start(ServerConfig::new().gateway(gate.gateway())).await?;
//! gate.arm();
//! // ... post a turn ...
//! gate.wait_held().await?;
//! // ... act while the tool call is held ...
//! gate.release();
//! # Ok(()) }
//! ```

use crate::{model::ScriptedModel, server::Gateway, Result};
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header, HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};

/// Which completion requests to hold (by request body).
type Filter = Arc<dyn Fn(&serde_json::Value) -> bool + Send + Sync>;

#[derive(Default)]
struct Inner {
    armed: bool,
    held: usize,
    /// Incremented by every release; a held reply waits for a newer one.
    generation: u64,
    filter: Option<Filter>,
}

#[derive(Clone)]
struct Shared {
    upstream: String,
    client: reqwest::Client,
    inner: Arc<Mutex<Inner>>,
    notify: Arc<Notify>,
}

/// Running gate; stops on drop.
pub struct ModelGate {
    origin: String,
    shared: Shared,
    task: JoinHandle<()>,
}

impl ModelGate {
    /// Gate in front of `model`.
    pub async fn start(model: &ScriptedModel) -> Result<Self> {
        let shared = Shared {
            upstream: model.origin().to_owned(),
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(300))
                .build()?,
            inner: Arc::new(Mutex::new(Inner::default())),
            notify: Arc::new(Notify::new()),
        };
        let app = Router::new()
            .fallback(any(forward))
            .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
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

    /// OpenAI-compatible base URL (`<origin>/v1`).
    pub fn base_url(&self) -> String {
        format!("{}/v1", self.origin)
    }

    /// Gateway options that send every model step through this gate.
    pub fn gateway(&self) -> Gateway {
        Gateway::new(self.base_url())
    }

    fn with<T>(&self, change: impl FnOnce(&mut Inner) -> T) -> T {
        match self.shared.inner.lock() {
            Ok(mut inner) => change(&mut inner),
            Err(poisoned) => change(&mut poisoned.into_inner()),
        }
    }

    /// Hold the reply of the next completion request until [`Self::release`].
    pub fn arm(&self) {
        self.with(|inner| inner.armed = true);
    }

    /// Hold the reply of every completion request whose JSON body matches
    /// `filter` until [`Self::release`] (in addition to [`Self::arm`]).
    pub fn hold_when(&self, filter: impl Fn(&serde_json::Value) -> bool + Send + Sync + 'static) {
        let filter: Filter = Arc::new(filter);
        self.with(|inner| inner.filter = Some(filter));
    }

    /// Stop holding new requests by filter (held ones stay held).
    pub fn clear_hold(&self) {
        self.with(|inner| inner.filter = None);
    }

    /// Replies currently held.
    pub fn held(&self) -> usize {
        self.with(|inner| inner.held)
    }

    /// Let every reply held so far through.
    pub fn release(&self) {
        self.with(|inner| inner.generation += 1);
        self.shared.notify.notify_waiters();
    }

    /// Wait until a reply is held.
    pub async fn wait_held(&self) -> Result<()> {
        crate::wait::until("a held model reply", || async {
            Ok((self.with(|inner| inner.held) > 0).then_some(()))
        })
        .await
    }
}

impl Drop for ModelGate {
    fn drop(&mut self) {
        self.release();
        self.task.abort();
    }
}

async fn forward(
    State(shared): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri
        .path_and_query()
        .map_or_else(|| uri.path().to_owned(), |path| path.as_str().to_owned());
    let mut request = shared
        .client
        .request(method.clone(), format!("{}{path}", shared.upstream))
        .body(body.to_vec());
    for name in [header::AUTHORIZATION, header::CONTENT_TYPE] {
        if let Some(value) = headers.get(&name) {
            request = request.header(name, value);
        }
    }
    let gated = method == Method::POST && path.ends_with("/chat/completions") && {
        let Ok(mut inner) = shared.inner.lock() else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        let matched = inner.filter.as_ref().is_some_and(|filter| {
            serde_json::from_slice::<serde_json::Value>(&body).is_ok_and(|value| filter(&value))
        });
        std::mem::take(&mut inner.armed) || matched
    };
    let reply = match request.send().await {
        Ok(reply) => reply,
        Err(error) => return (StatusCode::BAD_GATEWAY, error.to_string()).into_response(),
    };
    let status = reply.status().as_u16();
    let content_type = reply.headers().get(header::CONTENT_TYPE).cloned();
    let bytes = match reply.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => return (StatusCode::BAD_GATEWAY, error.to_string()).into_response(),
    };
    if gated {
        let generation = match shared.inner.lock() {
            Ok(mut inner) => {
                inner.held += 1;
                inner.generation
            }
            Err(_) => 0,
        };
        loop {
            let notified = shared.notify.notified();
            if shared
                .inner
                .lock()
                .map_or(true, |inner| inner.generation > generation)
            {
                break;
            }
            notified.await;
        }
        if let Ok(mut inner) = shared.inner.lock() {
            inner.held = inner.held.saturating_sub(1);
        }
    }
    let mut response =
        Response::builder().status(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY));
    if let Some(content_type) = content_type {
        response = response.header(header::CONTENT_TYPE, content_type);
    }
    response
        .body(Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
