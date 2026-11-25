//! Canonical task protocol fixture (port of `app/rust-task-test-bridge.ts`
//! and the `fixture()` helper of `app/task-recovery.test.ts`).
//!
//! [`TaskFixture::new`] installs the canonical schema, adds owner, member and
//! foreign accounts, starts a canonical server, links one home device with
//! Ed25519 installation and device keys, and creates a project with one
//! agent homed on that device. Human requests carry a Clerk session token;
//! device requests carry `Authorization: Device <session>` plus a signed
//! `X-Minarbot-Proof`, like the Rust device host.

use crate::{
    canonical::{Account, Canonical},
    db::Fixture,
    new_id,
    protocol::{canonical_json, digest_command, sha256_hex},
    server::{Server, ServerConfig},
    Result,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use reqwest::{header, Method};
use ring::{
    agreement,
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use serde_json::{json, Value};
use std::{sync::Mutex, time::Duration};
use tokio::time::Instant;

const ED25519_SPKI: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];
const X25519_SPKI: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x6e, 0x03, 0x21, 0x00,
];

/// An Ed25519 key pair with its SPKI public key (base64url, no padding).
pub struct SigningKey {
    pair: Ed25519KeyPair,
}

impl SigningKey {
    pub fn generate() -> Result<Self> {
        let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .map_err(|_| "Ed25519 key generation failed")?;
        let pair = Ed25519KeyPair::from_pkcs8(document.as_ref()).map_err(|_| "Ed25519 key")?;
        Ok(Self { pair })
    }

    /// `key.export({type: 'spki', format: 'der'}).toString('base64url')`.
    pub fn public(&self) -> String {
        let mut der = ED25519_SPKI.to_vec();
        der.extend_from_slice(self.pair.public_key().as_ref());
        URL_SAFE_NO_PAD.encode(der)
    }

    /// Signature of `bytes`, base64url.
    pub fn sign(&self, bytes: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(self.pair.sign(bytes).as_ref())
    }

    /// Signature of `JSON.stringify(value)` (insertion order kept).
    pub fn sign_json(&self, value: &Value) -> String {
        self.sign(value.to_string().as_bytes())
    }
}

/// X25519 public key (SPKI, base64url); the private half is not needed.
pub fn x25519_public() -> Result<String> {
    let private =
        agreement::EphemeralPrivateKey::generate(&agreement::X25519, &SystemRandom::new())
            .map_err(|_| "X25519 key generation failed")?;
    let public = private
        .compute_public_key()
        .map_err(|_| "X25519 public key")?;
    let mut der = X25519_SPKI.to_vec();
    der.extend_from_slice(public.as_ref());
    Ok(URL_SAFE_NO_PAD.encode(der))
}

/// Canonical reply envelope: `{ok, data, receipt}` or `{ok: false, error}`.
#[derive(Clone, Debug)]
pub struct Envelope {
    pub status: u16,
    pub body: Value,
}

impl Envelope {
    pub fn ok(&self) -> bool {
        self.body["ok"] == true
    }

    pub fn data(&self) -> &Value {
        &self.body["data"]
    }

    pub fn receipt(&self) -> &Value {
        &self.body["receipt"]
    }

    /// `error.code` of a refused request.
    pub fn code(&self) -> Option<&str> {
        self.body["error"]["code"].as_str()
    }

    /// Require success.
    pub fn expect_ok(self) -> Result<Self> {
        if self.ok() {
            Ok(self)
        } else {
            Err(format!("canonical request failed ({}): {}", self.status, self.body).into())
        }
    }

    /// Require a refusal with `code` (the Node `assert.throws(..., code(x))`).
    pub fn expect_code(&self, code: &str) -> Result<()> {
        if !self.ok() && self.code() == Some(code) {
            Ok(())
        } else {
            Err(format!(
                "expected error {code}, got ({}): {}",
                self.status, self.body
            )
            .into())
        }
    }
}

/// The home device acting as executor.
#[derive(Clone, Debug)]
pub struct Principal {
    pub device_id: String,
    pub account_id: String,
    pub generation: i64,
    pub authority_generation: String,
}

/// Who sends a request.
pub enum Actor<'a> {
    Human(&'a Account),
    Device(&'a Principal),
}

pub struct TaskFixture {
    pub canonical: Canonical,
    pub owner: Account,
    pub member: Account,
    pub foreign: Account,
    pub project: String,
    pub agent: String,
    pub home: String,
    pub principal: Principal,
    pub signing: SigningKey,
    pub session: String,
    pacing: Mutex<Option<Instant>>,
}

impl TaskFixture {
    /// Canonical server with one linked home device and a project agent whose
    /// capabilities offer `tools`.
    pub async fn new(tools: &[&str], path_prefix: Option<&str>) -> Result<Self> {
        let mut canonical = Canonical::new()?;
        let owner = canonical.owner.clone();
        let member = canonical.add_account("member")?;
        let foreign = canonical.add_account("foreign")?;
        let mut config = ServerConfig::new()
            .real_clock()
            .option("testKeyring", json!({}));
        if let Some(prefix) = path_prefix {
            config = config.option("testPathPrefix", prefix);
        }
        canonical.start(config).await?;
        let signing = SigningKey::generate()?;
        let installation = SigningKey::generate()?;
        let mut fixture = Self {
            canonical,
            owner,
            member,
            foreign,
            project: new_id(),
            agent: new_id(),
            home: new_id(),
            principal: Principal {
                device_id: String::new(),
                account_id: String::new(),
                generation: 1,
                authority_generation: String::new(),
            },
            signing,
            session: String::new(),
            pacing: Mutex::new(None),
        };
        let candidate = json!({"requestId": new_id(), "installationId": new_id(), "deviceId": fixture.home,
            "mode": "new", "installationPublicKey": installation.public(),
            "deviceSigningKey": fixture.signing.public(), "deviceEncryptionKey": x25519_public()?,
            "deviceLabel": "Home", "requestedCapabilities": ["execution"], "zone": "Asia/Jakarta", "zoneRevision": 1});
        let challenge = fixture
            .request(
                Method::POST,
                "/api/devices/link-challenges",
                Some(candidate.clone()),
                Actor::Human(&fixture.owner),
                None,
                None,
            )
            .await?
            .expect_ok()?;
        let r = challenge.data().clone();
        let body = json!([
            "minarbot-link-v1",
            r["challengeId"],
            r["accountId"],
            r["clerkSub"],
            candidate["installationId"],
            candidate["deviceId"],
            r["installationThumbprint"],
            r["signingThumbprint"],
            r["encryptionThumbprint"],
            candidate["deviceLabel"],
            digest_command(&json!([
                "minarbot-device-capabilities-v1",
                candidate["requestedCapabilities"]
            ])),
            r["authorityGeneration"],
            r["deviceGeneration"],
            r["keyRevision"],
            candidate["zone"],
            candidate["zoneRevision"],
            candidate["mode"],
            null,
            null,
            r["nonce"],
            r["expiresAt"]
        ]);
        let descriptor = json!({"v": 1, "accountId": fixture.owner.account_id, "installationId": candidate["installationId"],
            "deviceId": fixture.home, "authorityGeneration": r["authorityGeneration"], "deviceGeneration": 1,
            "keyRevision": 1, "zone": candidate["zone"], "zoneRevision": 1,
            "signingPublicKey": candidate["deviceSigningKey"], "encryptionPublicKey": candidate["deviceEncryptionKey"]});
        let descriptor_body = json!([
            "minarbot-device-descriptor-v1",
            descriptor["accountId"],
            descriptor["installationId"],
            descriptor["deviceId"],
            descriptor["authorityGeneration"],
            descriptor["deviceGeneration"],
            descriptor["keyRevision"],
            descriptor["zone"],
            descriptor["zoneRevision"],
            descriptor["signingPublicKey"],
            descriptor["encryptionPublicKey"]
        ]);
        let mut signed = descriptor.clone();
        signed["signature"] = json!(fixture.signing.sign_json(&descriptor_body));
        let link = json!({"requestId": new_id(), "challengeId": r["challengeId"], "nonce": r["nonce"],
            "installationSignature": installation.sign_json(&body), "deviceSignature": fixture.signing.sign_json(&body),
            "descriptor": signed});
        let linked = fixture
            .request(
                Method::POST,
                "/api/devices/link",
                Some(link),
                Actor::Human(&fixture.owner),
                None,
                None,
            )
            .await?
            .expect_ok()?;
        fixture.session = linked.data()["deviceSession"]
            .as_str()
            .ok_or("Rust did not issue a device session.")?
            .to_owned();
        fixture.principal = Principal {
            device_id: fixture.home.clone(),
            account_id: fixture.owner.account_id.clone(),
            generation: 1,
            authority_generation: r["authorityGeneration"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        };
        fixture
            .human(
                Method::POST,
                "/api/projects",
                Some(json!({"requestId": new_id(), "id": fixture.project, "name": "A"})),
                &fixture.owner,
            )
            .await?
            .expect_ok()?;
        fixture
            .human(
                Method::POST,
                "/api/agents",
                Some(json!({"requestId": fixture.agent, "id": fixture.agent, "name": "Agent", "model": "fixture",
                    "backupModel": "", "instructions": "Research", "threadLimit": 2, "projectId": fixture.project,
                    "homeDeviceId": fixture.home, "expectedProjectRevision": 1, "expectedHomeDeviceRevision": 1,
                    "providerBinding": null})),
                &fixture.owner,
            )
            .await?
            .expect_ok()?;
        fixture.db().execute(
            "INSERT INTO project_members VALUES (?,?,'member',1,0)",
            [&fixture.project, &fixture.member.account_id],
        )?;
        fixture.db().execute(
            "UPDATE agent_projects SET capabilities_json=? WHERE project=? AND agent=?",
            [
                canonical_json(&json!({"v": 1, "tools": tools, "resources": []})),
                fixture.project.clone(),
                fixture.agent.clone(),
            ],
        )?;
        Ok(fixture)
    }

    pub fn db(&self) -> &Fixture {
        &self.canonical.db
    }

    pub fn server(&self) -> Result<&Server> {
        self.canonical
            .server
            .as_ref()
            .ok_or_else(|| "server stopped".into())
    }

    /// Admission command with its payload digest; `change` overrides fields.
    pub fn command(&self, change: Value) -> Value {
        let mut c = json!({"requestId": new_id(), "projectId": self.project, "agentId": self.agent,
            "homeDeviceId": self.home, "payload": {"v": 1, "kind": "direct", "message": {"text": "Research this", "files": []}},
            "expectedRevisions": {"project": 1, "agent": 1, "homeBinding": 1, "membership": 1, "assignment": 1, "capability": 1}});
        if let (Some(target), Value::Object(fields)) = (c.as_object_mut(), change) {
            target.extend(fields);
        }
        let digest = digest_command(&json!([
            "minarbot-task-payload-v1",
            c["requestId"],
            c["projectId"],
            c["agentId"],
            c["homeDeviceId"],
            c["payload"],
            c["expectedRevisions"]
        ]));
        c["payloadDigest"] = json!(digest);
        c
    }

    pub async fn human(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        actor: &Account,
    ) -> Result<Envelope> {
        self.request(method, path, body, Actor::Human(actor), None, None)
            .await
    }

    pub async fn device(&self, path: &str, body: Value, token: Option<&str>) -> Result<Envelope> {
        self.request(
            Method::POST,
            path,
            Some(body),
            Actor::Device(&self.principal),
            token,
            None,
        )
        .await
    }

    /// Raw canonical request (Node `request`). Requests are paced 51 ms
    /// apart to stay below the device proof limiter, like the Node bridge.
    pub async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        actor: Actor<'_>,
        token: Option<&str>,
        recovery: Option<&str>,
    ) -> Result<Envelope> {
        let wait = {
            let mut last = self.pacing.lock().map_err(|_| "pacing")?;
            let now = Instant::now();
            let due = last.map_or(now, |last| last + Duration::from_millis(51));
            *last = Some(due.max(now));
            due.saturating_duration_since(now)
        };
        tokio::time::sleep(wait).await;
        let raw = body.as_ref().map(Value::to_string).unwrap_or_default();
        let request_id = body
            .as_ref()
            .and_then(|body| body["requestId"].as_str().map(str::to_owned))
            .unwrap_or_else(new_id);
        let server = self.server()?;
        let mut request = server
            .client()?
            .request(method.clone(), server.url(path))
            .header(header::ORIGIN, server.origin())
            .header(header::CONTENT_TYPE, "application/json")
            .header("X-Minarbot-Protocol", "1")
            .header("X-Minarbot-Request-Id", &request_id)
            .timeout(Duration::from_secs(30));
        match actor {
            Actor::Human(account) => {
                request = request.header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", self.canonical.token_for(account)?),
                );
            }
            Actor::Device(principal) => {
                let at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_secs() as i64;
                let mut nonce = [0u8; 32];
                ring::rand::SecureRandom::fill(&SystemRandom::new(), &mut nonce)
                    .map_err(|_| "nonce")?;
                let mut proof = json!({"deviceId": principal.device_id, "deviceGeneration": principal.generation,
                    "authorityGeneration": principal.authority_generation, "requestId": request_id,
                    "nonce": URL_SAFE_NO_PAD.encode(nonce), "iat": at, "exp": at + 60, "method": method.as_str(),
                    "path": path, "bodyDigest": sha256_hex(&raw)});
                let body = json!([
                    "minarbot-device-request-v1",
                    proof["deviceId"],
                    proof["deviceGeneration"],
                    proof["authorityGeneration"],
                    proof["requestId"],
                    proof["nonce"],
                    proof["iat"],
                    proof["exp"],
                    proof["method"],
                    proof["path"],
                    proof["bodyDigest"]
                ]);
                proof["signature"] = json!(self.signing.sign_json(&body));
                request = request
                    .header(header::AUTHORIZATION, format!("Device {}", self.session))
                    .header("X-Minarbot-Proof", proof.to_string());
                if let Some(token) = token {
                    request = request.header("X-Minarbot-Executor", token);
                }
            }
        }
        if let Some(recovery) = recovery {
            request = request.header("X-Minarbot-Recovery", recovery);
        }
        if body.is_some() {
            request = request.body(raw);
        }
        let response = request.send().await?;
        let status = response.status().as_u16();
        let text = response.text().await?;
        let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
        Ok(Envelope { status, body })
    }

    // Task routes (Node `tasks.*`).

    pub async fn admit(&self, actor: &Account, body: &Value) -> Result<Envelope> {
        self.human(Method::POST, "/api/tasks", Some(body.clone()), actor)
            .await
    }

    pub async fn lookup(&self, actor: &Account, task: &str) -> Result<Envelope> {
        self.human(Method::GET, &format!("/api/tasks/{task}"), None, actor)
            .await
    }

    pub async fn lookup_after(&self, actor: &Account, task: &str, after: i64) -> Result<Envelope> {
        self.human(
            Method::GET,
            &format!("/api/tasks/{task}?afterResultSequence={after}"),
            None,
            actor,
        )
        .await
    }

    pub async fn control(&self, actor: &Account, task: &str, body: Value) -> Result<Envelope> {
        self.human(
            Method::POST,
            &format!("/api/tasks/{task}/control"),
            Some(body),
            actor,
        )
        .await
    }

    pub async fn answer(
        &self,
        actor: &Account,
        task: &str,
        question: &str,
        body: Value,
    ) -> Result<Envelope> {
        self.human(
            Method::POST,
            &format!("/api/tasks/{task}/questions/{question}/answer"),
            Some(body),
            actor,
        )
        .await
    }

    pub async fn recovery_session(
        &self,
        actor: &Account,
        task: &str,
        body: Value,
    ) -> Result<Envelope> {
        self.human(
            Method::POST,
            &format!("/api/tasks/{task}/recovery-session"),
            Some(body),
            actor,
        )
        .await
    }

    pub async fn recovery_import(
        &self,
        actor: &Account,
        task: &str,
        body: Value,
        proof: &str,
    ) -> Result<Envelope> {
        self.request(
            Method::POST,
            &format!("/api/tasks/{task}/recovery-import"),
            Some(body),
            Actor::Human(actor),
            None,
            Some(proof),
        )
        .await
    }

    pub async fn recovery_results(
        &self,
        actor: &Account,
        task: &str,
        body: Value,
        proof: &str,
    ) -> Result<Envelope> {
        self.request(
            Method::POST,
            &format!("/api/tasks/{task}/recovery-results"),
            Some(body),
            Actor::Human(actor),
            None,
            Some(proof),
        )
        .await
    }

    pub async fn acquire(&self, body: Value, token: &str) -> Result<Envelope> {
        self.device(
            &format!("/api/devices/{}/acquire", self.principal.device_id),
            body,
            Some(token),
        )
        .await
    }

    pub async fn prepare(
        &self,
        task: &str,
        action: &str,
        body: Value,
        token: &str,
    ) -> Result<Envelope> {
        self.device(
            &format!("/api/tasks/{task}/actions/{action}/prepare"),
            body,
            Some(token),
        )
        .await
    }

    pub async fn commit(
        &self,
        task: &str,
        action: &str,
        body: Value,
        token: &str,
    ) -> Result<Envelope> {
        self.device(
            &format!("/api/tasks/{task}/actions/{action}/commit"),
            body,
            Some(token),
        )
        .await
    }

    /// Device task operation `op` (`context`, `execution`, `reconcile`,
    /// `context-mutations`).
    pub async fn task_op(
        &self,
        op: &str,
        task: &str,
        body: Value,
        token: &str,
    ) -> Result<Envelope> {
        self.device(&format!("/api/tasks/{task}/{op}"), body, Some(token))
            .await
    }

    pub async fn wait_secret_result(&self, body: Value) -> Result<Envelope> {
        self.device(
            &format!(
                "/api/devices/{}/wait-secret-results",
                self.principal.device_id
            ),
            body,
            None,
        )
        .await
    }

    pub async fn revoke_device(
        &self,
        actor: &Account,
        device: &str,
        body: Value,
    ) -> Result<Envelope> {
        self.human(
            Method::POST,
            &format!("/api/devices/{device}/revoke"),
            Some(body),
            actor,
        )
        .await
    }

    /// `PUT /api/projects/{project}/agents/{agent}/rules`.
    pub async fn edit_agent_rules(
        &self,
        actor: &Account,
        project: &str,
        agent: &str,
        body: Value,
    ) -> Result<Envelope> {
        self.human(
            Method::PUT,
            &format!("/api/projects/{project}/agents/{agent}/rules"),
            Some(body),
            actor,
        )
        .await
    }

    /// Acquire `task` with a fresh executor token; returns `(token, ownership,
    /// acquire receipt revision)`.
    pub async fn acquire_task(&self, task: &str) -> Result<(String, Value, Value)> {
        let token = random_token()?;
        let acquired = self
            .acquire(
                json!({"requestId": new_id(), "taskId": task, "previousExecutorGeneration": 0,
                    "executorTokenHash": sha256_hex(&token), "lockProof": lock_proof()}),
                &token,
            )
            .await?
            .expect_ok()?;
        Ok((
            token,
            acquired.data()["ownership"].clone(),
            acquired.receipt()["recordRevision"].clone(),
        ))
    }
}

/// 32 random bytes, base64url (Node `randomBytes(32).toString('base64url')`).
pub fn random_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    ring::rand::SecureRandom::fill(&SystemRandom::new(), &mut bytes).map_err(|_| "random")?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// The lock proof the Node fixtures sent with a first acquire.
pub fn lock_proof() -> Value {
    json!({"bootId": new_id(), "processId": 1234, "processStart": "999", "lockId": new_id(),
        "previousOwnerStopped": true, "reconciledThrough": 0, "journalDigest": sha256_hex("journal")})
}

/// `canonicalToolDigest({toolCall, expectedRevision, fileInput?})`.
pub fn tool_digest(
    tool_call: &Value,
    expected_revision: &Value,
    file_input: Option<&Value>,
) -> String {
    digest_command(&json!([
        "minarbot-canonical-tool-v1",
        tool_call,
        expected_revision,
        file_input.cloned().unwrap_or(Value::Null)
    ]))
}
