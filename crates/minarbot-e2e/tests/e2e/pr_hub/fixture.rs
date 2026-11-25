//! Hosted Hub plus enrolled node servers (the Node `fixture` of
//! `app/hub-link.test.ts`).

use crate::pr_support::send;
use minarbot_testkit::{
    async_model::{AsyncModel, Step},
    db::{Fixture as Writable, Inspector, Params},
    hosted_hub::{hub_command, HostedHub, HubError, HubTransport},
    http::Method,
    machine_bridge::{Clock, MachineBridge},
    prelude::*,
    relay::{self, HttpClient},
    server::Gateway,
    wait::until_within,
    Servers,
};
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::{
    collections::HashMap,
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

pub const HUB_WAIT: Duration = Duration::from_secs(12);
pub type Script =
    Arc<dyn Fn(Value) -> Pin<Box<dyn Future<Output = Result<Step>> + Send>> + Send + Sync>;

pub fn locked<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

pub fn script<F, Fut>(respond: F) -> Script
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Step>> + Send + 'static,
{
    Arc::new(move |body| Box::pin(respond(body)))
}

pub fn has_tool(body: &Value) -> bool {
    body["messages"]
        .as_array()
        .is_some_and(|messages| messages.iter().any(|message| message["role"] == "tool"))
}

/// Poll `check` until it holds, for the Node suite's 12 s.
pub async fn until<F, Fut>(label: &str, mut check: F) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<bool>>,
{
    until_within(label, HUB_WAIT, || {
        let next = check();
        async move { Ok(next.await?.then_some(())) }
    })
    .await
}

/// Browser session (cookies and CSRF token).
#[derive(Clone, Debug, Default)]
pub struct Session {
    pub data: Value,
    pub cookies: String,
    pub csrf: String,
}

pub struct Project {
    pub project_id: String,
    pub host: String,
    pub section_id: String,
}

impl Project {
    fn from(value: &Value) -> Self {
        let text = |name: &str| value[name].as_str().unwrap_or_default().to_owned();
        Self {
            project_id: text("projectId"),
            host: text("host"),
            section_id: text("sectionId"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Proposal {
    pub value: Value,
    pub participation_id: String,
    pub revision: i64,
}

impl Proposal {
    fn from(value: Value) -> Self {
        Self {
            participation_id: value["participationId"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            revision: value["revision"].as_i64().unwrap_or(0),
            value,
        }
    }
}

pub struct Hub {
    pub root: TestRoot,
    pub hub: HostedHub,
    pub clock: Clock,
    pub transport: HubTransport,
    pub accounts_host: String,
    pub origin: String,
    pub local_origin: String,
    pub hub_key: String,
    pub signer: Arc<Ed25519KeyPair>,
    pub data_path: PathBuf,
    pub alice: String,
    pub bob: String,
    pub eve: String,
    pub project: Project,
    pub model: AsyncModel,
    pub calls: Arc<AtomicUsize>,
    stream: bool,
    sessions: Mutex<HashMap<String, Session>>,
    subjects: HashMap<String, String>,
    pub servers: Servers,
}

pub struct Node {
    pub app: Server,
    pub agent: Value,
    pub agent_id: String,
    pub node_id: String,
    pub account: String,
    pub config: ServerConfig,
    pub token: String,
    transport: HubTransport,
}

pub fn who(subject: &str) -> Value {
    json!({"issuer": "https://identity.test", "subject": subject,
        "verifiedMailboxes": [format!("{subject}@example.test")]})
}

fn client() -> Result<HttpClient> {
    Ok(HttpClient::builder()
        .no_proxy()
        .redirect(relay::redirect_none())
        .timeout(Duration::from_secs(30))
        .build()?)
}

/// SPKI DER of an Ed25519 public key, base64url.
pub fn spki(public: &[u8]) -> String {
    use minarbot_testkit::base64::Engine;
    let mut der = vec![
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0,
    ];
    der.extend_from_slice(public);
    minarbot_testkit::base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(der)
}

impl Hub {
    /// `count` is the number of node servers the case runs at once.
    pub async fn start(count: u32, respond: Option<Script>, stream: bool) -> Result<Self> {
        use minarbot_testkit::base64::Engine;
        let servers = Servers::reserve(count).await?;
        let root = TestRoot::new()?;
        let dir = root.path().to_owned();
        let data_path = dir.join("hub.sqlite");
        let clock = Clock::now();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
            .map_err(|_| "key generation")?;
        let signer =
            Arc::new(Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| "key parsing")?);
        let hub_key = spki(signer.public_key().as_ref());
        let accounts_host = "accounts.minarbot.test".to_owned();
        let origin = format!("https://{accounts_host}");
        let mut identities = serde_json::Map::new();
        for subject in ["alice", "bob", "eve"] {
            let mut identity = who(subject);
            identity["sessionId"] = json!(format!("{subject}-session"));
            identity["expiresAt"] = json!(clock.get() + 3_600_000);
            identities.insert(subject.to_owned(), identity);
        }
        let private = |path: &PathBuf, bytes: &[u8]| -> Result<()> {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(path, bytes)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            Ok(())
        };
        let (mailbox, tokens, signing) = (
            dir.join("mailbox.key"),
            dir.join("tokens.json"),
            dir.join("signer.pem"),
        );
        private(&mailbox, &[8_u8; 32])?;
        private(
            &tokens,
            Value::Object(identities.clone()).to_string().as_bytes(),
        )?;
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            minarbot_testkit::base64::engine::general_purpose::STANDARD.encode(pkcs8.as_ref())
        );
        private(&signing, pem.as_bytes())?;
        private(&data_path, b"")?;
        Writable::open(&data_path)?.execute_batch("PRAGMA application_id=0x4d4e5231")?;
        let config = json!({"domain": "minarbot.test", "accountsHost": accounts_host,
            "dataPath": data_path, "mailboxKeyPath": mailbox,
            "identity": {"mode": "stub", "tokensPath": tokens},
            "nodeLink": {"signingKeyPath": signing}});
        let mut ids = HashMap::new();
        let mut subjects = HashMap::new();
        for subject in ["alice", "bob", "eve"] {
            let out = hub_command(&dir, &config, &["admit", "https://identity.test", subject])?;
            let id = serde_json::from_str::<Value>(&out)?["accountId"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            subjects.insert(id.clone(), subject.to_owned());
            ids.insert(subject, id);
        }
        let hub = HostedHub::start(&dir, &config).await?;
        hub.control(json!({"now": clock.get()})).await?;
        let local_origin = hub.origin();
        let transport = HubTransport::new(&origin, &accounts_host, &local_origin, &clock);
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let model = AsyncModel::start(move |body| {
            counter.fetch_add(1, Ordering::SeqCst);
            let respond = respond.clone();
            async move {
                match respond {
                    Some(respond) => respond(body).await,
                    None => Ok(Step::text(format!(
                        "Reply from {}",
                        body["model"].as_str().unwrap_or_default()
                    ))),
                }
            }
        })
        .await?;
        let mut hub = Self {
            root,
            hub,
            clock,
            transport,
            accounts_host,
            origin,
            local_origin,
            hub_key,
            signer,
            data_path,
            alice: ids["alice"].clone(),
            bob: ids["bob"].clone(),
            eve: ids["eve"].clone(),
            project: Project {
                project_id: String::new(),
                host: String::new(),
                section_id: String::new(),
            },
            model,
            calls,
            stream,
            sessions: Mutex::new(HashMap::new()),
            subjects,
            servers,
        };
        hub.project = hub
            .create_project(&hub.alice.clone(), "alpha", "Alpha")
            .await?;
        let alice = hub.alice.clone();
        let bob = hub.bob.clone();
        let session = hub.project_session(&alice, None).await?;
        let invitation = hub
            .browser(
                &hub.project.host.clone(),
                "/api/invitations",
                Some(json!({"mailbox": "bob@example.test", "role": "member"})),
                Some(&session),
                None,
                Some(200),
            )
            .await?;
        let proof = fragment(invitation.data["url"].as_str().unwrap_or_default());
        let session = hub.project_session(&bob, None).await?;
        hub.browser(
            &hub.project.host.clone(),
            "/api/invitations/accept",
            Some(Value::Object(proof)),
            Some(&session),
            None,
            Some(200),
        )
        .await?;
        Ok(hub)
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Hub database row (read-only).
    pub fn one(&self, sql: &str, params: impl Params) -> Result<Option<Value>> {
        let rows = Inspector::open(&self.data_path)?.rows(sql, params)?;
        Ok(rows.into_iter().next())
    }

    pub fn field(&self, sql: &str, params: impl Params, name: &str) -> Result<Value> {
        Ok(self
            .one(sql, params)?
            .map_or(Value::Null, |row| row[name].clone()))
    }

    pub fn turn(&self, turn: &str) -> Result<Value> {
        self.turn_in(turn, &self.project.project_id)
    }

    pub fn turn_in(&self, turn: &str, project: &str) -> Result<Value> {
        self.one(
            "SELECT * FROM turns WHERE project_id=? AND id=?",
            [project, turn],
        )?
        .ok_or_else(|| format!("hub turn {turn} missing").into())
    }

    pub fn turn_state(&self, turn: &str) -> Result<String> {
        Ok(self.turn(turn)?["state"]
            .as_str()
            .unwrap_or_default()
            .to_owned())
    }

    pub fn node_field(&self, node: &str, name: &str) -> Result<Value> {
        self.field(
            &format!("SELECT {name} FROM nodes WHERE id=?"),
            [node],
            name,
        )
    }

    pub fn participation(&self, id: &str, name: &str) -> Result<Value> {
        self.field(
            &format!("SELECT {name} FROM participations WHERE id=?"),
            [id],
            name,
        )
    }

    /// Every routed result frame of the main section, in order.
    pub fn results(&self) -> Result<Vec<Value>> {
        let rows = Inspector::open(&self.data_path)?.rows(
            "SELECT r.frame_json FROM node_results r JOIN turns t ON t.project_id=r.project_id AND t.id=r.turn_id WHERE t.project_id=? AND t.section_id=? ORDER BY t.rowid,r.sequence",
            [&self.project.project_id, &self.project.section_id],
        )?;
        rows.iter()
            .map(|row| {
                Ok(serde_json::from_str(
                    row["frame_json"].as_str().unwrap_or("null"),
                )?)
            })
            .collect()
    }

    /// The Node `browser` helper: `host` selects the hub virtual host.
    pub async fn browser(
        &self,
        host: &str,
        path: &str,
        body: Option<Value>,
        session: Option<&Session>,
        bearer: Option<&str>,
        expected: Option<u16>,
    ) -> Result<Session> {
        let method = if body.is_some() {
            Method::POST
        } else {
            Method::GET
        };
        let mut request = client()?
            .request(method, format!("{}{path}", self.local_origin))
            .header("Host", host)
            .header("Origin", format!("https://{host}"))
            .header("Content-Type", "application/json");
        if let Some(session) = session {
            request = request
                .header("Cookie", &session.cookies)
                .header("X-CSRF-Token", &session.csrf);
        }
        if let Some(bearer) = bearer {
            request = request.header("Authorization", format!("Bearer {bearer}"));
        }
        if let Some(body) = body {
            request = request.body(body.to_string());
        }
        let response = request.send().await?;
        let status = response.status().as_u16();
        let cookies = response
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .filter_map(|value| value.split(';').next())
            .collect::<Vec<_>>()
            .join("; ");
        let text = response.text().await?;
        let data: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        match expected {
            None if !(200..300).contains(&status) => {
                return Err(HubError {
                    status,
                    code: data["error"]["code"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    message: data["error"]["message"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                }
                .into())
            }
            Some(expected) if expected != status => {
                return Err(
                    format!("{host}{path}: expected {expected}, got {status}: {text}").into(),
                )
            }
            _ => {}
        }
        let csrf = data["csrf"].as_str().unwrap_or_default().to_owned();
        Ok(Session {
            data,
            cookies,
            csrf,
        })
    }

    /// Raw request to the hub listener (the Node `request`).
    pub async fn request(
        &self,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<String>,
    ) -> Result<u16> {
        let mut request = client()?.post(format!("{}{path}", self.local_origin));
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        Ok(request.send().await?.status().as_u16())
    }

    pub async fn login(&self, account: &str) -> Result<Session> {
        let subject = self.subjects.get(account).cloned().unwrap_or_default();
        let boot = self
            .browser(
                &self.accounts_host,
                "/auth/bootstrap",
                None,
                None,
                None,
                Some(200),
            )
            .await?;
        self.browser(
            &self.accounts_host,
            "/auth/session",
            Some(json!({})),
            Some(&boot),
            Some(&subject),
            Some(200),
        )
        .await
    }

    pub async fn central_session(&self, account: &str) -> Result<Session> {
        if let Some(prior) = locked(&self.sessions).get(account).cloned() {
            if prior.data["expiresAt"].as_i64().unwrap_or(0) > self.clock.get() {
                return Ok(prior);
            }
        }
        let session = self.login(account).await?;
        locked(&self.sessions).insert(account.to_owned(), session.clone());
        Ok(session)
    }

    pub async fn create_project(&self, account: &str, slug: &str, name: &str) -> Result<Project> {
        let session = self.central_session(account).await?;
        let created = self
            .browser(
                &self.accounts_host,
                "/api/projects",
                Some(json!({"slug": slug, "name": name})),
                Some(&session),
                None,
                Some(200),
            )
            .await?;
        Ok(Project::from(&created.data))
    }

    pub async fn project_session(
        &self,
        account: &str,
        target: Option<&Project>,
    ) -> Result<Session> {
        let target = target.unwrap_or(&self.project);
        let central = self.central_session(account).await?;
        let started = self
            .browser(&target.host, "/auth/start", None, None, None, Some(200))
            .await?;
        let handoff = self
            .browser(
                &self.accounts_host,
                "/auth/handoff",
                Some(json!({"state": started.data["state"]})),
                Some(&central),
                None,
                Some(200),
            )
            .await?;
        let fragment = fragment(handoff.data["callback"].as_str().unwrap_or_default());
        let state = fragment.get("state").cloned().unwrap_or(Value::Null);
        let redeem = Session {
            csrf: state.as_str().unwrap_or_default().to_owned(),
            ..started
        };
        self.browser(
            &target.host,
            "/auth/redeem",
            Some(json!({"state": state, "code": fragment.get("code")})),
            Some(&redeem),
            None,
            Some(200),
        )
        .await
    }

    pub async fn post(&self, account: &str, mentions: &[&str]) -> Result<Vec<String>> {
        self.post_in(account, mentions, None, "Explain transport")
            .await
    }

    pub async fn browser_post(&self, account: &str, mentions: &[&str]) -> Result<Vec<String>> {
        self.post_in(
            account,
            mentions,
            None,
            "Mention from authenticated project browser",
        )
        .await
    }

    pub async fn post_in(
        &self,
        account: &str,
        mentions: &[&str],
        target: Option<&Project>,
        text: &str,
    ) -> Result<Vec<String>> {
        let target = target.unwrap_or(&self.project);
        let session = self.project_session(account, Some(target)).await?;
        let posted = self
            .browser(
                &target.host,
                "/api/messages",
                Some(
                    json!({"requestId": new_id(), "sectionId": target.section_id,
                    "text": text, "mentions": mentions}),
                ),
                Some(&session),
                None,
                None,
            )
            .await?;
        Ok(strings(&posted.data["turnIds"]))
    }

    pub async fn register_agent(
        &self,
        account: &str,
        agent: &str,
        node: &str,
        name: &str,
    ) -> Result<()> {
        let session = self.central_session(account).await?;
        self.browser(
            &self.accounts_host,
            "/api/agents",
            Some(json!({"agentId": agent, "nodeId": node, "name": name})),
            Some(&session),
            None,
            Some(200),
        )
        .await?;
        Ok(())
    }

    pub async fn propose(
        &self,
        account: &str,
        agent: &str,
        policy: &Value,
        provider_max: i64,
        target: Option<&Project>,
    ) -> Result<Proposal> {
        let target = target.unwrap_or(&self.project);
        let session = self.central_session(account).await?;
        let proposal = self
            .browser(
                &self.accounts_host,
                "/api/participations",
                Some(
                    json!({"projectHost": target.host, "agentId": agent, "policy": policy,
                    "providerMaxCostMicros": provider_max}),
                ),
                Some(&session),
                None,
                None,
            )
            .await?;
        Ok(Proposal::from(proposal.data))
    }

    pub async fn consent(&self, proposal: &Proposal, target: Option<&Project>) -> Result<()> {
        let target = target.unwrap_or(&self.project);
        let alice = self.alice.clone();
        let session = self.project_session(&alice, Some(target)).await?;
        self.browser(
            &target.host,
            "/api/participants/consent",
            Some(proposal.value.clone()),
            Some(&session),
            None,
            Some(200),
        )
        .await?;
        Ok(())
    }

    pub async fn withdraw(&self, participation: &str) -> Result<()> {
        let alice = self.alice.clone();
        let session = self.project_session(&alice, None).await?;
        self.browser(
            &self.project.host,
            "/api/participants/withdraw",
            Some(json!({"participationId": participation})),
            Some(&session),
            None,
            Some(200),
        )
        .await?;
        Ok(())
    }

    pub async fn advance(&self, millis: i64) -> Result<()> {
        self.hub
            .control(json!({"now": self.clock.get() + millis}))
            .await?;
        self.clock.advance(millis);
        Ok(())
    }

    pub async fn queued_post(&self, input: &Value, advance: i64) -> Result<Value> {
        self.clock.advance(advance);
        self.hub
            .control(
                json!({"now": self.clock.get(), "post": {"projectId": self.project.project_id,
                "accountId": self.alice, "input": input}}),
            )
            .await
    }

    pub async fn withdraw_node(
        &self,
        principal: &Value,
        participation: &str,
        revision: i64,
    ) -> Result<Value> {
        self.hub
            .control(
                json!({"now": self.clock.get(), "withdraw_node": {"principal": principal,
                "input": {"projectId": self.project.project_id, "participationId": participation,
                    "revision": revision}}}),
            )
            .await
    }

    /// Wait for the hub to mark `turn` done; returns its routed frames.
    pub async fn done(&self, turn: &str, account: Option<&str>) -> Result<Vec<Value>> {
        let account = account.unwrap_or(&self.alice).to_owned();
        let session = self.project_session(&account, None).await?;
        until_within("hub final", HUB_WAIT, || async {
            let state = self
                .browser(
                    &self.project.host,
                    &format!("/api/turn?turnId={turn}"),
                    None,
                    Some(&session),
                    None,
                    None,
                )
                .await?
                .data["state"]
                .clone();
            ensure!(
                !["failed", "unknown", "revoked"].contains(&state.as_str().unwrap_or_default()),
                "Unexpected hub turn state: {state}"
            );
            Ok((state == "done").then_some(()))
        })
        .await?;
        let history = self
            .browser(
                &self.project.host,
                &format!("/api/history?sectionId={}", self.project.section_id),
                None,
                Some(&session),
                None,
                None,
            )
            .await?;
        Ok(history.data["results"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|frame| frame["turnId"] == turn)
            .cloned()
            .collect())
    }

    /// Start, enroll and register a node server running `name`.
    pub async fn node(&self, name: &str, account: &str) -> Result<Node> {
        self.node_with(name, account, |config| config).await
    }

    pub async fn node_with(
        &self,
        name: &str,
        account: &str,
        extra: impl FnOnce(ServerConfig) -> ServerConfig,
    ) -> Result<Node> {
        self.node_with_ops(name, account, None, extra).await
    }

    /// [`Hub::node_with`] plus extra computer bridge operations (desktop).
    pub async fn node_with_ops(
        &self,
        name: &str,
        account: &str,
        ops: Option<minarbot_testkit::machine_bridge::Extra>,
        extra: impl FnOnce(ServerConfig) -> ServerConfig,
    ) -> Result<Node> {
        let root = TestRoot::new()?;
        let transport = self.transport.for_node();
        let hub_ops = transport.extra();
        let combined: minarbot_testkit::machine_bridge::Extra = Arc::new(move |operation, args| {
            ops.as_ref()
                .and_then(|ops| ops(operation, args))
                .or_else(|| hub_ops(operation, args))
        });
        let bridge = MachineBridge::new().extra(combined).bridge().await?;
        let config = extra(
            ServerConfig::new()
                .real_clock()
                .gateway(Gateway {
                    stream: Some(self.stream),
                    ..Gateway::new(self.model.base_url())
                })
                .option(
                    "hubLink",
                    json!({"origin": self.origin, "hubSigningPublicKey": self.hub_key,
                        "reconnectMs": 10}),
                )
                .computer_bridge(&bridge),
        );
        let token = config
            .computer_bridge_token()
            .unwrap_or_default()
            .to_owned();
        let app = root.start_in(&self.servers, config.clone()).await?;
        let agent = call(
            &app,
            "/api/agents",
            Some(json!({"runtime": "api", "id": new_id(),
            "name": name, "model": name, "instructions": "Answer section requests."})),
            201,
        )
        .await?;
        let started = call(&app, "/api/hub-link/enrollment/start", Some(json!({})), 200).await?;
        let code = started["humanCode"].as_str().unwrap_or_default().to_owned();
        ensure!(
            code.len() == 12
                && code
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()),
            "human code: {started}"
        );
        ensure!(
            started.get("deviceCode").is_none(),
            "device code leaked: {started}"
        );
        ensure!(
            started.get("privateKey").is_none(),
            "private key leaked: {started}"
        );
        let session = self.central_session(account).await?;
        self.browser(
            &self.accounts_host,
            "/api/nodes/enrollment/approve",
            Some(json!({"humanCode": code, "fingerprint": "0".repeat(64)})),
            None,
            None,
            Some(401),
        )
        .await?;
        let inspected = self
            .browser(
                &self.accounts_host,
                &format!("/api/nodes/enrollment?humanCode={code}"),
                None,
                Some(&session),
                None,
                Some(200),
            )
            .await?
            .data;
        ensure!(
            inspected["nodeId"] == started["nodeId"],
            "node: {inspected}"
        );
        ensure!(
            inspected["fingerprint"] == started["fingerprint"],
            "fingerprint: {inspected}"
        );
        ensure!(inspected["accountId"] == account, "account: {inspected}");
        self.browser(
            &self.accounts_host,
            "/api/nodes/enrollment/approve",
            Some(json!({"humanCode": code, "fingerprint": inspected["fingerprint"]})),
            Some(&session),
            None,
            Some(200),
        )
        .await?;
        call(&app, "/api/hub-link/enrollment/poll", Some(json!({})), 200).await?;
        let node = Node {
            app,
            agent,
            agent_id: new_id(),
            node_id: started["nodeId"].as_str().unwrap_or_default().to_owned(),
            account: account.to_owned(),
            config,
            token,
            transport,
        };
        until("node online", || async {
            Ok(node.status()?["connected"] == true)
        })
        .await?;
        self.register_agent(account, &node.agent_id, &node.node_id, name)
            .await?;
        Ok(node)
    }

    /// Policy for `node` in `target` (the Node `participate` policy).
    pub async fn policy(
        &self,
        node: &Node,
        invocation: Value,
        target: Option<&Project>,
    ) -> Result<Value> {
        let target = target.unwrap_or(&self.project);
        let digest = call(
            &node.app,
            &format!("/api/hub-link/agents/{}/policy", node.local_agent()),
            None,
            200,
        )
        .await?["localPolicyDigest"]
            .clone();
        Ok(
            json!({"sectionIds": [target.section_id], "invocation": invocation,
            "scopes": ["read", "invoke"], "localPolicyDigest": digest,
            "payerBindingRef": "local_provider", "maxCostMicros": 10000}),
        )
    }

    /// Propose, consent and approve a grant; waits until it is active.
    pub async fn participate(
        &self,
        node: &Node,
        invocation: Value,
        target: Option<&Project>,
    ) -> Result<Proposal> {
        let policy = self.policy(node, invocation, target).await?;
        let proposal = self
            .propose(&node.account, &node.agent_id, &policy, 100, target)
            .await?;
        self.consent(&proposal, target).await?;
        let target = target.unwrap_or(&self.project);
        call(
            &node.app,
            "/api/hub-link/grants/approve",
            Some(json!({"projectId": target.project_id, "participationId": proposal.participation_id,
                "agentId": node.agent_id, "localAgentId": node.agent["id"], "revision": proposal.revision,
                "policy": policy, "providerMaxCostMicros": 100})),
            200,
        )
        .await?;
        let id = proposal.participation_id.clone();
        until("grant active", || async {
            Ok(self.participation(&id, "state")? == "active")
        })
        .await?;
        Ok(proposal)
    }

    pub async fn owner(&self, node: &Node) -> Result<Proposal> {
        self.participate(node, json!({"mode": "owner"}), None).await
    }
}

/// Hub link status of any node server (the Node `hubLink.status()`).
pub fn runtime_status(app: &Server) -> Result<Value> {
    let row = Inspector::open(app.database())?
        .rows("SELECT value FROM hub_link_runtime WHERE id=1", [])?;
    Ok(row
        .first()
        .and_then(|row| row["value"].as_str())
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .map_or(Value::Null, |value| value["status"].clone()))
}

impl Node {
    pub fn local_agent(&self) -> String {
        self.agent["id"].as_str().unwrap_or_default().to_owned()
    }

    fn runtime(&self) -> Result<Value> {
        let row = Inspector::open(self.app.database())?
            .rows("SELECT value FROM hub_link_runtime WHERE id=1", [])?;
        Ok(row
            .first()
            .and_then(|row| row["value"].as_str())
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or_else(|| json!({})))
    }

    pub fn status(&self) -> Result<Value> {
        Ok(self.runtime()?["status"].clone())
    }

    pub fn approvals(&self) -> Result<Vec<Value>> {
        Ok(self.runtime()?["approvals"]
            .as_array()
            .cloned()
            .unwrap_or_default())
    }

    pub fn requests(&self) -> Result<Vec<Value>> {
        Inspector::open(self.app.database())?
            .rows("SELECT value FROM hub_link_requests LIMIT 100", [])?
            .iter()
            .map(|row| {
                Ok(serde_json::from_str(
                    row["value"].as_str().unwrap_or("null"),
                )?)
            })
            .collect()
    }

    pub fn local_policy_digest(&self, agent: &str) -> Result<Value> {
        Ok(self.runtime()?["policies"][agent].clone())
    }

    /// Fails unless the local turn is admitted and the link connected.
    pub fn check(&self, turn: &str) -> Result<bool> {
        let saved = Inspector::open(self.app.database())?.rows(
            "SELECT state,frame FROM hub_link_turns WHERE turn_id=?",
            [turn],
        )?;
        let Some(saved) = saved.first() else {
            return Ok(false);
        };
        Ok(saved["state"] == "admitted" && self.status()?["connected"] == true)
    }

    async fn command(&self, operation: &str, value: Value) -> Result<(u16, Value)> {
        let response = self
            .app
            .client()?
            .post(self.app.url("/api/hub-link"))
            .header("Origin", self.app.origin())
            .header("Content-Type", "application/json")
            .header("X-Minarbot-Test-Hub", &self.token)
            .body(json!({"operation": operation, "value": value}).to_string())
            .send()
            .await?;
        let status = response.status().as_u16();
        Ok((status, response.json().await.unwrap_or(Value::Null)))
    }

    /// Close the hub link (the Node `hubLink.close()`).
    pub async fn close_link(&self) -> Result<()> {
        let (status, body) = self.command("close", json!({})).await?;
        ensure!((200..300).contains(&status), "close: {status} {body}");
        Ok(())
    }

    pub async fn before_effect(
        &self,
        turn: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(u16, Value)> {
        self.command(
            "beforeEffect",
            json!({"turn": turn, "kind": kind, "payload": payload}),
        )
        .await
    }

    /// Stop the server and close its hub transport (the Node `app.close()`).
    pub async fn stop(&mut self) -> Result<()> {
        self.app.shutdown().await?;
        self.transport.close_all();
        Ok(())
    }

    /// SIGKILL the server and close its transport (the Node `app.crash()`).
    pub async fn crash(&mut self) -> Result<()> {
        self.app.crash().await?;
        self.transport.close_all();
        Ok(())
    }

    pub async fn restart(&mut self) -> Result<()> {
        let _ = self.app.shutdown().await;
        self.transport.close_all();
        let config = self.config.clone();
        self.app.start_stopped(config).await?;
        until("reconnected", || async {
            Ok(self.status()?["connected"] == true)
        })
        .await
    }
}

/// Browser call that must answer `status` (the Node `api`).
pub async fn call(app: &Server, path: &str, body: Option<Value>, status: u16) -> Result<Value> {
    let (got, value) = send(app, path, body.as_ref(), None, true).await?;
    ensure!(
        got == status,
        "{path}: expected {status}, got {got}: {value}"
    );
    Ok(value)
}

pub fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(str::to_owned))
        .collect()
}

/// Fields of a URL `#fragment`.
pub fn fragment(url: &str) -> serde_json::Map<String, Value> {
    let hash = url.split_once('#').map_or("", |(_, hash)| hash);
    hash.split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (decode(key), json!(decode(value))))
        .collect()
}

fn decode(value: &str) -> String {
    let bytes = value.replace('+', " ").into_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(byte) =
                u8::from_str_radix(&String::from_utf8_lossy(&bytes[index + 1..index + 3]), 16)
            {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Is `error` a hub protocol error with this code (any code when `None`)?
pub fn hub_error(error: &minarbot_testkit::Error, code: Option<&str>) -> bool {
    error
        .downcast_ref::<HubError>()
        .is_some_and(|error| code.is_none_or(|code| error.code == code))
}

/// Node signature envelope (`signedNodeMessage`) with `key`.
pub fn signed(body: &Value, key: &Ed25519KeyPair, direction: &str) -> Value {
    use minarbot_testkit::base64::Engine;
    let message = format!(
        "minarbot.{direction}.v1\n{}",
        minarbot_testkit::protocol::canonical_json(body)
    );
    let mut der = vec![
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0,
    ];
    der.extend_from_slice(key.public_key().as_ref());
    json!({"v": 1, "keyId": minarbot_testkit::protocol::sha256_hex(&der), "body": body,
        "signature": minarbot_testkit::base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(key.sign(message.as_bytes()).as_ref())})
}
