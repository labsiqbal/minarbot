//! Computer bridges for paired machines (the Node `peerTokenIssuer` option
//! and the computer bridge's `peer-http`).
//!
//! With a computer bridge configured, the server sends peer HTTP and peer
//! token operations through it. [`PeerBridge::start`] answers them:
//!
//! - `clock` -> `null` (the server keeps its own clock),
//! - `issue`, `verify`, `revoke` -> a recording token issuer when the bridge
//!   was started with one ([`PeerBridge::issuer`]), else `null` so the server
//!   falls back to its own stored tokens,
//! - `peer-http` -> forwards the request to a loopback peer and returns
//!   `{status, headers, body(base64)}`, like the Node bridge.
//!
//! Combine it with [`crate::server::start_pair`] and
//! [`crate::Server::refresh_machines`]. Paired servers call each other's
//! bridges while one waits, so run such tests on a multi-thread runtime
//! (`#[tokio::test(flavor = "multi_thread")]`).

use crate::{
    bridge::{Bridge, BridgeError},
    Result,
};
use base64::Engine;
use serde_json::{json, Map, Value};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

/// One issued peer token.
#[derive(Clone, Debug)]
pub struct IssuedToken {
    pub id: String,
    pub token: String,
    pub scopes: Value,
}

/// A running computer bridge for one server.
#[derive(Clone)]
pub struct PeerBridge {
    bridge: Bridge,
    issued: Arc<Mutex<Vec<IssuedToken>>>,
    revoked: Arc<Mutex<Vec<String>>>,
}

impl PeerBridge {
    /// Bridge without its own issuer (the server stores peer tokens itself).
    pub async fn start() -> Result<Self> {
        Self::with_issuer(false).await
    }

    /// Bridge with a recording issuer (the Node `fakePeerTokenIssuer`).
    pub async fn issuer() -> Result<Self> {
        Self::with_issuer(true).await
    }

    async fn with_issuer(issue: bool) -> Result<Self> {
        let issued: Arc<Mutex<Vec<IssuedToken>>> = Arc::default();
        let revoked: Arc<Mutex<Vec<String>>> = Arc::default();
        let (tokens, revocations) = (issued.clone(), revoked.clone());
        let bridge = Bridge::operations(move |operation, args| {
            fn poisoned<T>(_: T) -> BridgeError {
                BridgeError::new(500, "peer bridge poisoned")
            }
            match operation {
                "clock" => Ok(Value::Null),
                "peer-http" => forward(args),
                "issue" if issue => {
                    let token = IssuedToken {
                        id: crate::new_id(),
                        token: format!("peer-canary-{}{}", uuid(), uuid()),
                        scopes: args["scopes"].clone(),
                    };
                    let value = json!({"id": token.id, "token": token.token});
                    tokens.lock().map_err(poisoned)?.push(token);
                    Ok(value)
                }
                "revoke" if issue => {
                    let id = args["id"].as_str().unwrap_or_default().to_owned();
                    revocations.lock().map_err(poisoned)?.push(id.clone());
                    tokens
                        .lock()
                        .map_err(poisoned)?
                        .retain(|token| token.id != id);
                    Ok(Value::Null)
                }
                "verify" if issue => Ok(tokens
                    .lock()
                    .map_err(poisoned)?
                    .iter()
                    .find(|token| args["token"] == token.token.as_str())
                    .map_or(
                        Value::Null,
                        |token| json!({"id": token.id, "scopes": token.scopes}),
                    )),
                "issue" | "revoke" | "verify" => Ok(Value::Null),
                other => Err(BridgeError::new(404, format!("peer bridge has no {other}"))),
            }
        })
        .await?;
        Ok(Self {
            bridge,
            issued,
            revoked,
        })
    }

    pub fn bridge(&self) -> &Bridge {
        &self.bridge
    }

    /// Tokens issued so far (still valid ones only).
    pub fn issued(&self) -> Vec<IssuedToken> {
        self.issued
            .lock()
            .map(|tokens| tokens.clone())
            .unwrap_or_default()
    }

    /// Token ids revoked so far.
    pub fn revoked(&self) -> Vec<String> {
        self.revoked
            .lock()
            .map(|ids| ids.clone())
            .unwrap_or_default()
    }
}

fn uuid() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Forward one `peer-http` request to a loopback peer. The bridge handler is
/// synchronous, so the request runs on its own thread and runtime.
fn forward(args: &Value) -> Result<Value, BridgeError> {
    let args = args.clone();
    let request = std::thread::spawn(move || -> Result<Value, BridgeError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| BridgeError::new(503, error.to_string()))?;
        runtime.block_on(send(&args))
    });
    let join = || {
        request
            .join()
            .map_err(|_| BridgeError::new(503, "peer request thread panicked"))?
    };
    // Bridges run on the test runtime; with several workers the others keep
    // serving bridge calls the peer makes while this request waits.
    match tokio::runtime::Handle::try_current().map(|handle| handle.runtime_flavor()) {
        Ok(tokio::runtime::RuntimeFlavor::MultiThread) => tokio::task::block_in_place(join),
        _ => join(),
    }
}

async fn send(args: &Value) -> Result<Value, BridgeError> {
    let failure = |error: &dyn std::fmt::Display| BridgeError::new(503, error.to_string());
    let url = reqwest::Url::parse(args["url"].as_str().unwrap_or_default())
        .map_err(|error| failure(&error))?;
    if !matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")) {
        return Err(BridgeError::new(400, "Peer fixture requires loopback."));
    }
    let timeout = if url.path() == "/api/peer/updates/apply" {
        Duration::from_secs(300)
    } else {
        Duration::from_secs(10)
    };
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|error| failure(&error))?;
    let method = reqwest::Method::from_bytes(args["method"].as_str().unwrap_or("GET").as_bytes())
        .map_err(|error| failure(&error))?;
    let mut request = client.request(method, url);
    for (name, value) in args["headers"].as_object().into_iter().flatten() {
        if let Some(value) = value.as_str() {
            request = request.header(name.as_str(), value);
        }
    }
    if let Some(body) = args["body"].as_str() {
        request = request.body(body.to_owned());
    }
    let response = request.send().await.map_err(|error| {
        let mut message = error.to_string();
        let mut source = std::error::Error::source(&error);
        while let Some(inner) = source {
            message.push_str(&format!(": {inner}"));
            source = inner.source();
        }
        BridgeError::new(503, message)
    })?;
    let status = response.status().as_u16();
    let mut headers = Map::new();
    for (name, value) in response.headers() {
        if let Ok(value) = value.to_str() {
            headers.insert(name.as_str().to_owned(), json!(value));
        }
    }
    let body = response.bytes().await.map_err(|error| failure(&error))?;
    if body.len() > 128 * 1024 {
        return Err(BridgeError::new(503, "Peer fixture response exceeded."));
    }
    Ok(json!({"status": status, "headers": headers,
        "body": base64::engine::general_purpose::STANDARD.encode(&body)}))
}
