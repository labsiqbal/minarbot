//! Canonical (hosted, Clerk-authenticated) servers.
//!
//! Port of `app/test-support/canonical-http.ts`: the database starts from the
//! canonical schema fixture, accounts are inserted directly, and requests
//! carry an RS256 session token signed with the test-only key in
//! `fixtures/clerk-test-key.pem` (see `fixtures/README.md`).

use crate::{
    db::Fixture,
    new_id,
    protocol::digest_command,
    server::{Server, ServerConfig, TestRoot},
    Result,
};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::{header, Method};
use rusqlite::functions::FunctionFlags;
use serde_json::{json, Value};

pub const ISSUER: &str = "https://fixture.clerk.accounts.dev";
const PRIVATE_KEY: &str = include_str!("../fixtures/clerk-test-key.pem");
const PUBLIC_KEY: &str = include_str!("../fixtures/clerk-test-key.pub.pem");
const SCHEMA: &str = include_str!("../fixtures/canonical-pre-collapse.sql");

/// A Clerk subject with its canonical account row.
#[derive(Clone, Debug)]
pub struct Account {
    pub account_id: String,
    pub subject: String,
}

/// Canonical reply: HTTP status and the `{ok, data | error}` envelope.
#[derive(Clone, Debug)]
pub struct CanonicalReply {
    pub status: u16,
    pub body: Value,
}

impl CanonicalReply {
    /// `error.code` of a failed request.
    pub fn code(&self) -> Option<&str> {
        self.body["error"]["code"].as_str()
    }

    /// Require success (status < 400 and `ok: true`).
    pub fn ok(self) -> Result<Self> {
        if self.status >= 400 || self.body["ok"] != true {
            return Err(
                format!("canonical request failed ({}): {}", self.status, self.body).into(),
            );
        }
        Ok(self)
    }

    /// Require a failure with `code`.
    pub fn error(self, code: &str) -> Result<()> {
        if self.status >= 400 && self.code() == Some(code) {
            Ok(())
        } else {
            Err(format!(
                "expected canonical error {code}, got {}: {}",
                self.status, self.body
            )
            .into())
        }
    }

    pub fn data(&self) -> &Value {
        &self.body["data"]
    }
}

/// Canonical database, accounts and (once started) server.
pub struct Canonical {
    pub root: TestRoot,
    pub db: Fixture,
    pub owner: Account,
    pub server: Option<Server>,
    clock: Option<i64>,
}

fn register_digest(db: &Fixture) -> Result<()> {
    db.connection().create_scalar_function(
        "minarbot_capability_digest",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |context| {
            let raw: String = context.get(0)?;
            let value: Value = serde_json::from_str(&raw)
                .map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))?;
            Ok(digest_command(&json!(["minarbot-capabilities-v1", value])))
        },
    )?;
    Ok(())
}

impl Canonical {
    /// Install the canonical schema and an owner account (not started yet).
    pub fn new() -> Result<Self> {
        let root = TestRoot::new()?;
        let path = root.database();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = Fixture::open(&path)?;
        register_digest(&db)?;
        db.execute_batch(&format!("BEGIN;{SCHEMA};COMMIT;"))?;
        db.execute(
            "INSERT INTO authority_state VALUES (1,?,'active',0,NULL,NULL,1)",
            [new_id()],
        )?;
        let mut fixture = Self {
            root,
            db,
            owner: Account {
                account_id: String::new(),
                subject: String::new(),
            },
            server: None,
            clock: None,
        };
        fixture.owner = fixture.add_account("user_owner")?;
        Ok(fixture)
    }

    /// Insert an account for a Clerk `subject`.
    pub fn add_account(&self, subject: &str) -> Result<Account> {
        let account_id = new_id();
        self.db.execute(
            "INSERT INTO accounts(id,issuer,clerk_sub,created) VALUES (?,?,?,?)",
            rusqlite::params![account_id, ISSUER, subject, crate::server::FIXED_CLOCK],
        )?;
        Ok(Account {
            account_id,
            subject: subject.to_owned(),
        })
    }

    /// Start (or restart) the canonical server.
    pub async fn start(&mut self, config: ServerConfig) -> Result<&Server> {
        if let Some(mut server) = self.server.take() {
            server.shutdown().await?;
        }
        self.clock = config.clock();
        let mut last = None;
        for _ in 0..5 {
            let port = free_port()?;
            let origin = format!("http://127.0.0.1:{port}");
            let publishable = {
                use base64::Engine;
                format!(
                    "pk_test_{}",
                    base64::engine::general_purpose::STANDARD.encode("fixture.clerk.accounts.dev$")
                )
            };
            let started = self
                .root
                .start(
                    config
                        .clone()
                        .option("port", port)
                        .option(
                            "auth",
                            json!({"jwtKey": PUBLIC_KEY, "issuer": ISSUER, "origin": origin,
                                "development": true, "secretKey": "sk_test_fixture",
                                "publishableKey": publishable}),
                        )
                        .sync_key([7; 32]),
                )
                .await;
            match started {
                Ok(server) => {
                    self.server = Some(server);
                    return self.server.as_ref().ok_or_else(|| "server missing".into());
                }
                Err(error) if error.to_string().contains("Address already in use") => {
                    last = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last.unwrap_or_else(|| "canonical server did not start".into()))
    }

    fn token(&self, actor: &Account, origin: &str) -> Result<String> {
        let now = self.clock.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_millis() as i64)
        }) / 1000;
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("fixture".into());
        let claims = json!({"iss": ISSUER, "sub": actor.subject,
            "sid": format!("sess_{}", actor.subject), "azp": origin,
            "nbf": now - 1, "exp": now + 600});
        Ok(jsonwebtoken::encode(
            &header,
            &claims,
            &EncodingKey::from_rsa_pem(PRIVATE_KEY.as_bytes())?,
        )?)
    }

    /// Clerk session token of `actor` for the running server.
    pub fn token_for(&self, actor: &Account) -> Result<String> {
        let server = self
            .server
            .as_ref()
            .ok_or("start the canonical server first")?;
        self.token(actor, server.origin())
    }

    /// Send an authenticated canonical request as `actor`.
    pub async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        actor: &Account,
    ) -> Result<CanonicalReply> {
        let server = self
            .server
            .as_ref()
            .ok_or("start the canonical server first")?;
        let token = self.token(actor, server.origin())?;
        let request_id = body
            .as_ref()
            .and_then(|body| body["requestId"].as_str().map(str::to_owned))
            .unwrap_or_else(new_id);
        let mut request = server
            .client()?
            .request(method, server.url(path))
            .header(header::ORIGIN, server.origin())
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header("X-Minarbot-Protocol", "1")
            .header("X-Minarbot-Request-Id", request_id);
        if let Some(body) = body {
            request = request.body(serde_json::to_vec(&body)?);
        }
        let response = request.send().await?;
        let status = response.status().as_u16();
        let text = response.text().await?;
        let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
        Ok(CanonicalReply { status, body })
    }

    pub async fn post(&self, path: &str, body: Value, actor: &Account) -> Result<CanonicalReply> {
        self.send(Method::POST, path, Some(body), actor).await
    }

    pub async fn put(&self, path: &str, body: Value, actor: &Account) -> Result<CanonicalReply> {
        self.send(Method::PUT, path, Some(body), actor).await
    }
}

fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    Ok(listener.local_addr()?.port())
}
