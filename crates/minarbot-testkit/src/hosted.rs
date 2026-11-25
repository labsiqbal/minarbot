//! Fresh hosted (canonical) server with Clerk session tokens.
//!
//! Port of the `fixture()` of `app/two-process.test.ts` and
//! `startCanonicalApp`: a canonical-mode server on a fixed loopback port
//! (its public origin must match the Clerk `azp`), a canonical bridge for
//! routine webhook keys (in memory, with fault switches) and Clerk lookups
//! (unavailable, as the Node fixture's offline Clerk client was), and RS256
//! session tokens signed with the kit's test-only key. Unlike
//! [`crate::canonical::Canonical`], the database starts empty and accounts
//! are provisioned by the server on first use.

use crate::{
    bridge::{Bridge, BridgeError},
    http::Method,
    server::{Server, ServerConfig, Servers, TestRoot},
    Result,
};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

/// SQL parameter macro for mixed text/blob statements in tests.
pub use rusqlite::params;

pub const ISSUER: &str = "https://fixture.clerk.accounts.dev";
const PRIVATE_KEY: &str = include_str!("../fixtures/clerk-test-key.pem");
const PUBLIC_KEY: &str = include_str!("../fixtures/clerk-test-key.pub.pem");

fn locked<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

/// Routine webhook keyring (`routineWebhookKeys` + `webhookFault`).
#[derive(Default)]
pub struct WebhookKeys {
    pub service: String,
    values: Mutex<HashMap<String, String>>,
    reads: Mutex<Vec<(String, String)>>,
    pub fail_read: AtomicBool,
    pub fail_write: AtomicBool,
}

impl WebhookKeys {
    /// Every `service/name -> secret` stored.
    pub fn values(&self) -> HashMap<String, String> {
        locked(&self.values).clone()
    }

    /// Every read as `(service, name)`, in order.
    pub fn reads(&self) -> Vec<(String, String)> {
        locked(&self.reads).clone()
    }

    pub fn clear_reads(&self) {
        locked(&self.reads).clear();
    }

    fn read(&self, secret_ref: Option<&str>) -> Result<Value, BridgeError> {
        let name = secret_ref
            .unwrap_or("routine-webhook/missing-target")
            .to_owned();
        locked(&self.reads).push((self.service.clone(), name.clone()));
        if self.fail_read.load(Ordering::SeqCst) {
            return Err(BridgeError::new(503, "PRIVATE_KEYRING_FAILURE"));
        }
        Ok(secret_ref
            .and_then(|_| {
                locked(&self.values)
                    .get(&format!("{}/{name}", self.service))
                    .cloned()
            })
            .map_or(Value::Null, Value::String))
    }

    fn create(&self, routine: &str, generation: &Value) -> Result<Value, BridgeError> {
        use base64::Engine;
        if self.fail_write.load(Ordering::SeqCst) {
            return Err(BridgeError::new(503, "Key write failed"));
        }
        let secret_ref = format!("routine-webhook/{routine}/{generation}/{}", crate::new_id());
        let mut bytes = Vec::new();
        bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        let secret = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        locked(&self.values).insert(format!("{}/{secret_ref}", self.service), secret.clone());
        if self.read(Some(&secret_ref))? != Value::String(secret.clone()) {
            return Err(BridgeError::new(503, "Key read-back failed"));
        }
        Ok(json!({"secretRef": secret_ref, "secret": secret}))
    }
}

/// A running hosted server.
pub struct HostedApp {
    pub root: TestRoot,
    pub server: Server,
    pub port: u16,
    pub keys: Arc<WebhookKeys>,
    config: ServerConfig,
    _bridge: Bridge,
}

fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    Ok(listener.local_addr()?.port())
}

/// RS256 Clerk session token for `subject` with `azp = origin`.
pub fn session_token(subject: &str, origin: &str) -> Result<String> {
    let now = wall_ms() / 1000;
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("fixture".into());
    header.typ = Some("JWT".into());
    let claims = json!({"iss": ISSUER, "sub": subject, "sid": format!("sess_{subject}"),
        "azp": origin, "exp": now + 300, "nbf": now - 1, "iat": now - 1});
    Ok(jsonwebtoken::encode(
        &header,
        &claims,
        &EncodingKey::from_rsa_pem(PRIVATE_KEY.as_bytes())?,
    )?)
}

impl HostedApp {
    /// Start on a fresh database in `root` (inside `servers`).
    pub async fn start(root: TestRoot, servers: &Servers) -> Result<Self> {
        Self::start_env(root, servers, &[]).await
    }

    /// [`HostedApp::start`] with extra server environment (for example `TZ`).
    pub async fn start_env(
        root: TestRoot,
        servers: &Servers,
        env: &[(&str, &str)],
    ) -> Result<Self> {
        let keys = Arc::new(WebhookKeys {
            service: format!("minarbot/server/{}/routine-webhooks", crate::new_id()),
            ..WebhookKeys::default()
        });
        let shared = keys.clone();
        let bridge = Bridge::paths(move |operation, args| match operation {
            "webhook-read" => shared.read(args["name"].as_str()),
            "webhook-create" => shared.create(
                args["routine"].as_str().unwrap_or_default(),
                &args["generation"],
            ),
            // The Node fixture's Clerk client had no network: lookups failed.
            _ => Err(BridgeError::new(503, "Canonical fixture unavailable.")),
        })
        .await?;
        let mut last = None;
        for _ in 0..5 {
            let port = free_port()?;
            let mut config = Self::config(&bridge, port);
            for (name, value) in env {
                config = config.env(*name, *value);
            }
            match root.start_in(servers, config.clone()).await {
                Ok(server) => {
                    return Ok(Self {
                        root,
                        server,
                        port,
                        keys,
                        config,
                        _bridge: bridge,
                    })
                }
                Err(error) if error.to_string().contains("Address already in use") => {
                    last = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last.unwrap_or_else(|| "hosted server did not start".into()))
    }

    fn config(bridge: &Bridge, port: u16) -> ServerConfig {
        use base64::Engine;
        let origin = format!("http://127.0.0.1:{port}");
        let publishable = format!(
            "pk_test_{}",
            base64::engine::general_purpose::STANDARD.encode("fixture.clerk.accounts.dev$")
        );
        ServerConfig::new()
            .real_clock()
            .option("port", port)
            .option(
                "auth",
                json!({"jwtKey": PUBLIC_KEY, "secretKey": "sk_test_fixture",
                    "publishableKey": publishable, "issuer": ISSUER, "origin": origin,
                    "development": true}),
            )
            .sync_key([7; 32])
            .canonical_bridge(bridge, true, false, true)
    }

    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn database(&self) -> PathBuf {
        self.server.database()
    }

    pub fn token(&self, subject: &str) -> Result<String> {
        session_token(subject, &self.origin())
    }

    /// Authenticated request as `subject` (POST with a body, otherwise GET,
    /// unless `method` says otherwise). Returns status and JSON body.
    pub async fn send(
        &self,
        path: &str,
        body: Option<&Value>,
        subject: &str,
        method: Option<Method>,
    ) -> Result<(u16, Value)> {
        let method = method.unwrap_or(if body.is_some() {
            Method::POST
        } else {
            Method::GET
        });
        let request_id = body
            .and_then(|body| body["requestId"].as_str().map(str::to_owned))
            .unwrap_or_else(crate::new_id);
        let mut request = self
            .server
            .client()?
            .request(method, format!("{}{path}", self.origin()))
            .header("Authorization", format!("Bearer {}", self.token(subject)?))
            .header("Origin", self.origin())
            .header("Connection", "close")
            .header("Content-Type", "application/json")
            .header("X-Minarbot-Protocol", "1")
            .header("X-Minarbot-Request-Id", request_id);
        if let Some(body) = body {
            request = request.body(body.to_string());
        }
        let response = request.send().await?;
        let status = response.status().as_u16();
        let text = response.text().await?;
        Ok((
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        ))
    }

    /// Restart on the same port; waits until previously ready devices
    /// report ready again (devices back off up to 65 s between reconnects).
    pub async fn restart(&mut self) -> Result<()> {
        let (_, devices) = self.send("/api/devices", None, "user_owner", None).await?;
        let ready: Vec<Value> = devices["data"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|device| device["status"] == "ready")
            .map(|device| device["id"].clone())
            .collect();
        let config = self.config.clone();
        self.server.restart_with(config).await?;
        if !ready.is_empty() {
            crate::wait::until_within("devices reconnect", Duration::from_secs(65), || async {
                let (_, devices) = self.send("/api/devices", None, "user_owner", None).await?;
                let rows = devices["data"].as_array().cloned().unwrap_or_default();
                Ok(ready
                    .iter()
                    .all(|id| {
                        rows.iter()
                            .any(|device| device["id"] == *id && device["status"] == "ready")
                    })
                    .then_some(()))
            })
            .await?;
        }
        Ok(())
    }

    /// Stop the server.
    pub async fn stop(&mut self) -> Result<()> {
        self.server.shutdown().await?;
        Ok(())
    }

    /// Start a stopped server again on the same port.
    pub async fn start_again(&mut self) -> Result<()> {
        let config = self.config.clone();
        self.server.start_stopped(config).await
    }

    /// Restore `backup` into a fresh read-only recovery database and
    /// reconcile it (the Node `restoreFrom`); returns the reconcile data.
    pub async fn restore_from(&mut self, backup: &Path) -> Result<Value> {
        self.server.shutdown().await?;
        let database = self.database();
        let mut unavailable = database.clone().into_os_string();
        unavailable.push(".unavailable");
        std::fs::rename(&database, PathBuf::from(unavailable))?;
        let output = std::process::Command::new(crate::server::server_binary(None)?)
            .arg("--restore-read-only")
            .arg(backup)
            .arg(&database)
            .env_clear()
            .env("PATH", crate::server::HERMETIC_PATH)
            .env("HOME", self.root.path())
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "restore failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        let gate: Value = serde_json::from_slice(&output.stdout)?;
        self.start_again().await?;
        let (status, body) = self
            .send(
                "/api/restore/reconcile",
                Some(&json!({"requestId": crate::new_id(), "bootstrap": gate["bootstrap"]})),
                "user_owner",
                None,
            )
            .await?;
        if status != 200 {
            return Err(format!("reconcile failed ({status}): {body}").into());
        }
        Ok(body["data"].clone())
    }

    pub fn output(&self) -> Result<(String, String)> {
        self.server.output()
    }
}
