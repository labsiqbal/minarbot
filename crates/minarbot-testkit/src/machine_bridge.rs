//! Computer bridge for paired-machine tests.
//!
//! Every Node `startApp` server had a computer bridge (`app/rust-computer-test-bridge.ts`).
//! With a bridge configured, the server asks it for the machine clock, for
//! peer token issue/revoke/verify, and sends every peer HTTP request through
//! `peer-http`. [`MachineBridge`] serves those operations like the Node
//! bridge: a missing [`Issuer`] answers `null` (the server then uses its MCP
//! token registry), a missing manual clock answers wall-clock time, and
//! `peer-http` fetches loopback URLs without following redirects, with a
//! 10 s budget (300 s for peer update apply) and a 128 KiB reply cap.
//! [`ServerConfig::computer_bridge`](crate::ServerConfig::computer_bridge)
//! takes [`MachineBridge::bridge`], which also enables
//! [`Server::refresh_machines`](crate::Server::refresh_machines).

use crate::{
    bridge::{AsyncOperations, Bridge, BridgeError},
    Result,
};
use base64::Engine;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

/// `peerUpdateTimeoutMs` of the Node peer token issuer module.
pub const PEER_UPDATE_TIMEOUT: Duration = Duration::from_secs(300);
const PEER_HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const PEER_HTTP_LIMIT: usize = 128 * 1024;

/// One issued peer token.
#[derive(Clone, Debug)]
pub struct IssuedToken {
    pub id: String,
    pub scopes: Vec<String>,
}

#[derive(Default)]
struct IssuerState {
    tokens: HashMap<String, IssuedToken>,
    issued: Vec<String>,
    revoked: Vec<String>,
}

/// Test-only peer token issuer (`fakePeerTokenIssuer`): tokens are
/// `peer-canary-<base64url 32 bytes>`, revocation deletes by id.
#[derive(Clone, Default)]
pub struct Issuer(Arc<Mutex<IssuerState>>);

impl Issuer {
    pub fn new() -> Self {
        Self::default()
    }

    fn with<T>(&self, change: impl FnOnce(&mut IssuerState) -> T) -> T {
        match self.0.lock() {
            Ok(mut state) => change(&mut state),
            Err(poisoned) => change(&mut poisoned.into_inner()),
        }
    }

    /// Every token issued so far, in order (revoked ones included).
    pub fn issued(&self) -> Vec<String> {
        self.with(|state| state.issued.clone())
    }

    /// Ids passed to revoke, in order.
    pub fn revoked(&self) -> Vec<String> {
        self.with(|state| state.revoked.clone())
    }

    /// Number of live (unrevoked) tokens.
    pub fn live(&self) -> usize {
        self.with(|state| state.tokens.len())
    }

    /// Current authorization of `token`, if live.
    pub fn get(&self, token: &str) -> Option<IssuedToken> {
        self.with(|state| state.tokens.get(token).cloned())
    }

    /// Replace the authorization of a live token (scope downgrade tests).
    pub fn set(&self, token: &str, value: IssuedToken) {
        self.with(|state| {
            state.tokens.insert(token.to_owned(), value);
        });
    }

    fn create(&self, scopes: Vec<String>) -> Value {
        use rand_free::random_token;
        let id = crate::new_id();
        let token = format!("peer-canary-{}", random_token());
        self.with(|state| {
            state.tokens.insert(
                token.clone(),
                IssuedToken {
                    id: id.clone(),
                    scopes,
                },
            );
            state.issued.push(token.clone());
        });
        json!({"id": id, "token": token})
    }

    fn revoke(&self, id: &str) {
        self.with(|state| {
            state.revoked.push(id.to_owned());
            state.tokens.retain(|_, value| value.id != id);
        });
    }

    fn verify(&self, token: &str) -> Value {
        self.get(token).map_or(
            Value::Null,
            |auth| json!({"id": auth.id, "scopes": auth.scopes}),
        )
    }
}

mod rand_free {
    /// 32 random bytes, base64url without padding, from two v4 UUIDs.
    pub fn random_token() -> String {
        use base64::Engine;
        let mut bytes = Vec::with_capacity(32);
        bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }
}

/// Manual machine clock (`machines.now`), in Unix milliseconds.
#[derive(Clone)]
pub struct Clock(Arc<AtomicI64>);

impl Clock {
    pub fn new(millis: i64) -> Self {
        Self(Arc::new(AtomicI64::new(millis)))
    }

    /// Wall-clock start, like `let clock = Date.now()`.
    pub fn now() -> Self {
        Self::new(wall_clock())
    }

    pub fn get(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }

    pub fn advance(&self, millis: i64) {
        self.0.fetch_add(millis, Ordering::SeqCst);
    }
}

fn wall_clock() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

/// Extra bridge operations (for example `hub-*`), answered before the
/// machine operations. Return `None` to fall through.
pub type Extra = Arc<
    dyn Fn(
            &str,
            &Value,
        ) -> Option<futures_util::future::BoxFuture<'static, Result<Value, BridgeError>>>
        + Send
        + Sync,
>;

/// Options of one [`MachineBridge`].
#[derive(Clone, Default)]
pub struct MachineBridge {
    pub issuer: Option<Issuer>,
    pub clock: Option<Clock>,
    pub extra: Option<Extra>,
}

impl MachineBridge {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn issuer(mut self, issuer: &Issuer) -> Self {
        self.issuer = Some(issuer.clone());
        self
    }

    pub fn clock(mut self, clock: &Clock) -> Self {
        self.clock = Some(clock.clone());
        self
    }

    pub fn extra(mut self, extra: Extra) -> Self {
        self.extra = Some(extra);
        self
    }

    /// Start the bridge; pass it to `ServerConfig::computer_bridge`.
    pub async fn bridge(self) -> Result<Bridge> {
        let options = Arc::new(self);
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let handler: Arc<AsyncOperations> = Arc::new(move |operation: String, args: Value| {
            let options = options.clone();
            let client = client.clone();
            Box::pin(async move { options.call(&client, &operation, &args).await })
        });
        Bridge::operations_async(handler).await
    }

    async fn call(
        &self,
        client: &reqwest::Client,
        operation: &str,
        args: &Value,
    ) -> Result<Value, BridgeError> {
        if let Some(extra) = &self.extra {
            if let Some(answer) = extra(operation, args) {
                return answer.await;
            }
        }
        match operation {
            "clock" => Ok(json!(self
                .clock
                .as_ref()
                .map_or_else(wall_clock, Clock::get))),
            "issue" => Ok(self.issuer.as_ref().map_or(Value::Null, |issuer| {
                issuer.create(
                    args["scopes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|scope| scope.as_str().map(str::to_owned))
                        .collect(),
                )
            })),
            "revoke" => {
                if let Some(issuer) = &self.issuer {
                    issuer.revoke(args["id"].as_str().unwrap_or_default());
                }
                Ok(Value::Null)
            }
            "verify" => Ok(self.issuer.as_ref().map_or(Value::Null, |issuer| {
                issuer.verify(args["token"].as_str().unwrap_or_default())
            })),
            "peer-http" => peer_http(client, args)
                .await
                .map_err(|error| BridgeError::new(503, error.to_string())),
            _ if operation.starts_with("desktop-") => {
                Err(BridgeError::new(503, "Desktop fixture missing."))
            }
            _ => Err(BridgeError::new(503, "Unknown test bridge operation.")),
        }
    }
}

async fn peer_http(client: &reqwest::Client, args: &Value) -> Result<Value> {
    let url = reqwest::Url::parse(args["url"].as_str().unwrap_or_default())?;
    if !matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")) {
        return Err("Peer fixture requires loopback.".into());
    }
    let budget = if url.path() == "/api/peer/updates/apply" {
        PEER_UPDATE_TIMEOUT
    } else {
        PEER_HTTP_TIMEOUT
    };
    let method = reqwest::Method::from_bytes(args["method"].as_str().unwrap_or("GET").as_bytes())?;
    let mut request = client.request(method, url).timeout(budget);
    for (name, value) in args["headers"].as_object().into_iter().flatten() {
        if let Some(value) = value.as_str() {
            request = request.header(name, value);
        }
    }
    if let Some(body) = args["body"].as_str() {
        request = request.body(body.to_owned());
    }
    let work = async {
        let response = request.send().await?;
        let status = response.status().as_u16();
        let mut headers = serde_json::Map::new();
        for (name, value) in response.headers() {
            let value = value.to_str()?.to_owned();
            match headers.get_mut(name.as_str()) {
                Some(Value::String(existing)) => {
                    existing.push_str(", ");
                    existing.push_str(&value);
                }
                _ => {
                    headers.insert(name.as_str().to_owned(), json!(value));
                }
            }
        }
        let mut body = Vec::new();
        let mut response = response;
        while let Some(chunk) = response.chunk().await? {
            body.extend_from_slice(&chunk);
            if body.len() > PEER_HTTP_LIMIT {
                return Err::<Value, crate::Error>("Peer fixture response exceeded.".into());
            }
        }
        Ok(json!({
            "status": status,
            "headers": headers,
            "body": base64::engine::general_purpose::STANDARD.encode(body),
        }))
    };
    tokio::time::timeout(budget, work)
        .await
        .map_err(|_| crate::Error::from("The operation was aborted due to timeout"))?
}
