//! Closure-backed HTTP server on loopback for external services.
//!
//! Node cases stood up ad-hoc `http.createServer` fakes for remote MCP
//! servers, Composio and similar services. [`HttpStub`] is the Rust
//! equivalent: a closure answers every request ([`StubRequest`] in,
//! [`StubResponse`] out), and every request is recorded.

use crate::Result;
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    Router,
};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::{net::TcpListener, task::JoinHandle};

/// One recorded request.
#[derive(Clone, Debug)]
pub struct StubRequest {
    pub method: String,
    /// Path and query, for example `/tools?toolkits=gmail`.
    pub uri: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl StubRequest {
    pub fn path(&self) -> &str {
        self.uri.split('?').next().unwrap_or_default()
    }

    /// Value of query parameter `name`, percent-decoding `%XX` and `+`.
    pub fn query(&self, name: &str) -> Option<String> {
        let query = self.uri.split_once('?')?.1;
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (key == name).then(|| decode(value))
        })
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Body parsed as JSON (`Null` when empty or invalid).
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => out.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                match text
                    .get(index + 1..index + 3)
                    .map(|hex| u8::from_str_radix(hex, 16))
                {
                    Some(Ok(byte)) => {
                        out.push(byte);
                        index += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Reply to one stub request.
#[derive(Clone, Debug)]
pub enum StubResponse {
    /// Status, content type and body.
    Body(u16, String, Vec<u8>),
    /// Send the status line and headers, then abort the body: the client
    /// loses the reply (the Node fixtures' `res.destroy()`).
    Lost,
}

impl StubResponse {
    pub fn json(value: &Value) -> Self {
        Self::status_json(200, value)
    }

    pub fn status_json(status: u16, value: &Value) -> Self {
        Self::Body(
            status,
            "application/json".into(),
            value.to_string().into_bytes(),
        )
    }

    pub fn status(status: u16) -> Self {
        Self::Body(status, "text/plain".into(), Vec::new())
    }

    pub fn text(status: u16, text: impl Into<String>) -> Self {
        Self::Body(status, "text/plain".into(), text.into().into_bytes())
    }
}

type Handler = Arc<dyn Fn(&StubRequest) -> StubResponse + Send + Sync>;

#[derive(Clone)]
struct Shared {
    handler: Handler,
    requests: Arc<Mutex<Vec<StubRequest>>>,
}

/// Running stub. Dropping it stops the listener.
pub struct HttpStub {
    origin: String,
    requests: Arc<Mutex<Vec<StubRequest>>>,
    task: JoinHandle<()>,
}

impl HttpStub {
    pub async fn start(
        handler: impl Fn(&StubRequest) -> StubResponse + Send + Sync + 'static,
    ) -> Result<Self> {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let shared = Shared {
            handler: Arc::new(handler),
            requests: requests.clone(),
        };
        let app = Router::new().fallback(handle).with_state(shared);
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            origin,
            requests,
            task,
        })
    }

    /// `http://127.0.0.1:<port>`.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn requests(&self) -> Vec<StubRequest> {
        match self.requests.lock() {
            Ok(requests) => requests.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl Drop for HttpStub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle(
    State(shared): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request = StubRequest {
        method: method.as_str().to_owned(),
        uri: uri
            .path_and_query()
            .map(|value| value.as_str().to_owned())
            .unwrap_or_default(),
        headers: headers
            .iter()
            .map(|(key, value)| {
                (
                    key.as_str().to_owned(),
                    value.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect(),
        body: body.to_vec(),
    };
    match shared.requests.lock() {
        Ok(mut requests) => requests.push(request.clone()),
        Err(poisoned) => poisoned.into_inner().push(request.clone()),
    }
    match (shared.handler)(&request) {
        StubResponse::Body(status, content_type, body) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            [("content-type", content_type)],
            body,
        )
            .into_response(),
        StubResponse::Lost => {
            let failure = futures_util::stream::once(async {
                Err::<Bytes, std::io::Error>(std::io::Error::other("reply lost"))
            });
            (StatusCode::OK, Body::from_stream(failure)).into_response()
        }
    }
}
