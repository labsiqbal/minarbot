//! Telegram and Slack connector fixture (port of
//! `app/test-support/rust-connector-fixture.ts`, `rust-connectors-runner.ts`
//! and `connector-journal-command.ts`).
//!
//! [`Connector`] runs `minarbot-server --connector-config` against two
//! loopback servers it owns:
//!
//! - a fake platform API (Telegram Bot API or Slack Web API plus a Socket Mode
//!   WebSocket) that records every call and lets a test answer, delay or lose
//!   a reply through a [`Remote`] hook;
//! - a script-API proxy in front of the Minarbot server that records each
//!   request and can lose a reply after forwarding it ([`Fault`]).
//!
//! [`connector_app`] also starts the Minarbot server with a script token and
//! an enabled agent, as `connectorApp` did.

use crate::{
    server::{server_binary, Server, ServerConfig, TestRoot, HERMETIC_PATH},
    wait, Result,
};
use axum::{
    body::{Body, Bytes},
    extract::{ws::Message, State, WebSocketUpgrade},
    http::{header, HeaderMap, Uri},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::{
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::JoinHandle,
};

pub const TELEGRAM_TOKEN: &str = "123:xxxxxxxxxxxxxxxxxxxxxxxx";
pub const SLACK_BOT_TOKEN: &str = "xoxb-bbbbbbbbbbbbbbbbbbbbbbbb";
pub const SLACK_APP_TOKEN: &str = "xapp-aaaaaaaaaaaaaaaaaaaaaaaa";
/// Script token the fixture app accepts (`scriptToken`).
pub const SCRIPT_TOKEN: &str = "fixture-connector-app-token-01234567890";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Slack,
    Telegram,
}

impl Platform {
    pub fn name(self) -> &'static str {
        match self {
            Self::Slack => "slack",
            Self::Telegram => "telegram",
        }
    }
}

/// One request the connector sent to the script API.
#[derive(Clone, Debug)]
pub struct ScriptRequest {
    pub method: String,
    pub path: String,
    pub bytes: Vec<u8>,
    pub name: Option<String>,
    pub authorization: Option<String>,
}

/// One-shot fault on the script API proxy: the matching request is lost
/// before (`after == false`) or after it reached the server.
#[derive(Clone, Debug)]
pub struct Fault {
    pub method: Option<String>,
    /// Matches when the request path ends with this suffix.
    pub suffix: String,
    pub after: bool,
}

/// One platform API call.
#[derive(Clone, Debug)]
pub struct Post {
    pub method: String,
    pub raw: Vec<u8>,
    pub content_type: Option<String>,
    pub authorization: Option<String>,
    pub host: String,
}

/// What the fake platform does with a call.
pub enum Reply {
    /// The default result (a sent message).
    Default,
    /// This `result` (Telegram) or these extra fields (Slack).
    Value(Value),
    /// Close the connection without a usable reply (a lost response).
    Lose,
    /// A raw reply, for upload endpoints that do not speak the platform JSON.
    Raw(u16, Vec<u8>),
}

/// Hook for platform calls other than the identity, socket and poll calls.
pub type Remote = Arc<dyn Fn(Post) -> BoxFuture<'static, Reply> + Send + Sync>;

#[derive(Default)]
struct Record {
    requests: Vec<ScriptRequest>,
    successful: Vec<ScriptRequest>,
    faults: Vec<Fault>,
    updates: Vec<Value>,
    acks: Vec<String>,
    polls: Vec<i64>,
    posts: Vec<Post>,
    offset: i64,
    error: Option<String>,
    socket: Option<mpsc::UnboundedSender<String>>,
    connections: usize,
    authorizations: Vec<String>,
    limited: std::collections::HashMap<String, std::time::Instant>,
    script: SocketScript,
}

/// Slack Socket Mode behaviour of the fake platform.
#[derive(Clone, Default)]
pub struct SocketScript {
    /// Methods whose first call is answered 429 with `Retry-After: 1`; a
    /// retry sooner than one second is a fixture error.
    pub rate_limit_once: Vec<String>,
    /// Envelopes pushed right after `hello` on every connection.
    pub on_connect: Vec<Value>,
    /// After the first acknowledgement on the first connection, ask the
    /// client to reconnect (`disconnect`, `refresh_requested`).
    pub disconnect_after_first_ack: bool,
    /// Called with each acknowledged envelope id.
    pub on_ack: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

type Shared = Arc<Mutex<Record>>;

fn lock(state: &Shared) -> std::sync::MutexGuard<'_, Record> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Clone)]
struct Api {
    platform: Platform,
    origin: String,
    state: Shared,
    remote: Option<Remote>,
}

struct Process {
    child: Child,
    stderr: PathBuf,
}

/// A running (or stopped) connector with its fake platform and proxy.
pub struct Connector {
    pub platform: Platform,
    pub directory: PathBuf,
    pub script_directory: PathBuf,
    /// Fake platform API origin.
    pub origin: String,
    agent: String,
    channels: Mutex<Vec<String>>,
    users: Vec<Value>,
    state: Shared,
    process: Mutex<Option<Process>>,
    logs: PathBuf,
    runs: Mutex<usize>,
    tasks: Vec<JoinHandle<()>>,
}

impl Connector {
    /// Fixture for `agent` with the platform API, the script-API proxy in
    /// front of `app_origin` and `connection.json` in the script directory.
    pub async fn new(
        platform: Platform,
        root: &Path,
        agent: &str,
        app_origin: &str,
        remote: Option<Remote>,
        channels: Vec<String>,
        users: Vec<Value>,
    ) -> Result<Self> {
        let directory = root.join("state");
        let script_directory = root.join("connector-scripts");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&script_directory)?;
        let logs = root.join("connector-logs");
        fs::create_dir_all(&logs)?;
        let state: Shared = Arc::default();
        let api_listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let origin = format!("http://{}", api_listener.local_addr()?);
        let api = Api {
            platform,
            origin: origin.clone(),
            state: state.clone(),
            remote,
        };
        let router = Router::new()
            .route("/socket", any(socket))
            .fallback(any(platform_call))
            .with_state(api);
        let api_task = tokio::spawn(async move {
            let _ = axum::serve(api_listener, router).await;
        });
        let proxy_listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let url = format!("http://{}", proxy_listener.local_addr()?);
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(120))
            .build()?;
        let (proxy_state, target) = (state.clone(), app_origin.to_owned());
        let proxy_task = tokio::spawn(async move {
            while let Ok((stream, _)) = proxy_listener.accept().await {
                let (state, client, target) = (proxy_state.clone(), client.clone(), target.clone());
                tokio::spawn(async move {
                    if let Err(error) = proxy(stream, &state, &client, &target).await {
                        lock(&state).error.get_or_insert(error.to_string());
                    }
                });
            }
        });
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(script_directory.join("connection.json"))?;
        file.write_all(
            json!({"url": url, "token": SCRIPT_TOKEN})
                .to_string()
                .as_bytes(),
        )?;
        Ok(Self {
            platform,
            directory,
            script_directory,
            origin,
            agent: agent.into(),
            channels: Mutex::new(channels),
            users,
            state,
            process: Mutex::new(None),
            logs,
            runs: Mutex::new(0),
            tasks: vec![api_task, proxy_task],
        })
    }

    /// The connector options (`--connector-config` stdin).
    pub fn options(&self) -> Value {
        let mut options = json!({"platform": self.platform.name(), "directory": self.directory,
            "scriptDirectory": self.script_directory, "agent": self.agent,
            "origin": self.origin, "channels": self.channels(), "users": self.users});
        match self.platform {
            Platform::Slack => {
                options["team"] = json!("TTEAM");
                options["app"] = json!("AAPP");
                options["botToken"] = json!(SLACK_BOT_TOKEN);
                options["appToken"] = json!(SLACK_APP_TOKEN);
            }
            Platform::Telegram => options["token"] = json!(TELEGRAM_TOKEN),
        }
        options
    }

    fn process(&self) -> std::sync::MutexGuard<'_, Option<Process>> {
        match self.process.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Channels the next [`Self::start`] passes to the connector.
    pub fn channels(&self) -> Vec<String> {
        self.channels
            .lock()
            .map(|channels| channels.clone())
            .unwrap_or_default()
    }

    pub fn set_channels(&self, channels: Vec<String>) {
        if let Ok(mut current) = self.channels.lock() {
            *current = channels;
        }
    }

    /// Socket Mode behaviour for the following connections.
    pub fn set_socket_script(&self, script: SocketScript) {
        lock(&self.state).script = script;
    }

    /// Socket Mode connections so far.
    pub fn connections(&self) -> usize {
        lock(&self.state).connections
    }

    /// `Authorization` headers of every platform call.
    pub fn authorizations(&self) -> Vec<String> {
        lock(&self.state).authorizations.clone()
    }

    /// Start the connector process.
    pub fn start(&self) -> Result<()> {
        let mut process = self.process();
        if process.is_some() {
            return Err("connector already running".into());
        }
        let run = {
            let mut runs = self.runs.lock().map_err(|_| "poisoned")?;
            *runs += 1;
            *runs
        };
        let stderr = self.logs.join(format!("run-{run}.stderr"));
        *process = Some(spawn(&self.options(), &stderr, &self.logs)?);
        Ok(())
    }

    /// SIGTERM and wait. Fixture errors surface here.
    pub async fn stop(&self) -> Result<()> {
        let taken = self.process().take();
        if let Some(mut process) = taken {
            if process.child.try_wait()?.is_none() {
                terminate(&mut process.child)?;
                for _ in 0..500 {
                    if process.child.try_wait()?.is_some() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                if process.child.try_wait()?.is_none() {
                    process.child.kill()?;
                    process.child.wait()?;
                }
            }
        }
        lock(&self.state).socket = None;
        match lock(&self.state).error.take() {
            Some(error) => Err(format!("connector fixture failed: {error}").into()),
            None => Ok(()),
        }
    }

    /// Fail if the fixture saw a protocol error or the connector exited.
    pub fn check(&self) -> Result<()> {
        if let Some(error) = lock(&self.state).error.clone() {
            return Err(format!("connector fixture failed: {error}").into());
        }
        if let Some(process) = self.process().as_mut() {
            if let Some(status) = process.child.try_wait()? {
                let stderr = fs::read_to_string(&process.stderr).unwrap_or_default();
                return Err(format!("connector stopped ({status}): {}", stderr.trim()).into());
            }
        }
        Ok(())
    }

    /// Wait for the connector to exit on its own (after an injected fault)
    /// and require its error output to contain `message`.
    pub async fn expected_failure(&self, message: &str) -> Result<()> {
        let stderr = wait::until("connector stopped after an injected failure", || async {
            if let Some(error) = lock(&self.state).error.clone() {
                return Err(format!("connector fixture failed: {error}").into());
            }
            let mut process = self.process();
            let Some(process) = process.as_mut() else {
                return Err("connector is not running".into());
            };
            Ok(match process.child.try_wait()? {
                Some(_) => Some(fs::read_to_string(&process.stderr).unwrap_or_default()),
                None => None,
            })
        })
        .await?;
        self.stop().await?;
        if !stderr.contains(message) {
            return Err(format!("connector error {stderr:?} lacks {message:?}").into());
        }
        Ok(())
    }

    /// Queue a Telegram update or push a Slack envelope over the socket.
    pub async fn push(&self, message: Value) -> Result<()> {
        if message.get("envelope_id").is_some() {
            let sender = wait::until("Slack socket to open", || async {
                self.check()?;
                Ok(lock(&self.state).socket.clone())
            })
            .await?;
            sender
                .send(message.to_string())
                .map_err(|_| "Slack socket closed")?;
        } else {
            lock(&self.state).updates.push(message);
        }
        Ok(())
    }

    /// Push `message` and wait until the connector committed it (Telegram
    /// offset) or acknowledged it (Slack envelope).
    pub async fn send(&self, message: Value) -> Result<()> {
        if let Some(update) = message["update_id"].as_i64() {
            self.push(message).await?;
            let journal = self.directory.join("telegram.sqlite");
            wait::until("Telegram update to be committed", || async {
                if let Ok(db) = crate::db::Inspector::open(&journal) {
                    if let Ok(offset) =
                        db.scalar::<i64>("SELECT offset FROM telegram_binding WHERE id=1", [])
                    {
                        if offset > update {
                            return Ok(Some(()));
                        }
                    }
                }
                self.check()?;
                Ok(None)
            })
            .await?;
            return Ok(());
        }
        let envelope = message["envelope_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let received = |state: &Shared| {
            lock(state)
                .acks
                .iter()
                .filter(|id| **id == envelope)
                .count()
        };
        let before = received(&self.state);
        self.push(message).await?;
        wait::until("Slack event to be acknowledged", || async {
            if received(&self.state) > before {
                return Ok(Some(()));
            }
            self.check()?;
            Ok(None)
        })
        .await
    }

    /// Poll `read` until it returns `Some`, failing fast when the connector
    /// stops or the fixture sees a protocol error.
    pub async fn until<T>(&self, label: &str, read: impl Fn() -> Result<Option<T>>) -> Result<T> {
        wait::until(label, || async {
            self.check()?;
            read()
        })
        .await
    }

    pub fn add_fault(&self, fault: Fault) {
        lock(&self.state).faults.push(fault);
    }

    /// Script API requests so far.
    pub fn requests(&self) -> Vec<ScriptRequest> {
        lock(&self.state).requests.clone()
    }

    /// Script API requests the server answered with 2xx.
    pub fn successful(&self) -> Vec<ScriptRequest> {
        lock(&self.state).successful.clone()
    }

    /// Platform calls other than identity, socket and polling.
    pub fn posts(&self) -> Vec<Post> {
        lock(&self.state).posts.clone()
    }

    /// JSON bodies of the platform calls named `method`.
    pub fn json_posts(&self, method: &str) -> Vec<Value> {
        self.posts()
            .iter()
            .filter(|post| post.method == method)
            .filter_map(|post| serde_json::from_slice(&post.raw).ok())
            .collect()
    }

    pub fn acks(&self) -> Vec<String> {
        lock(&self.state).acks.clone()
    }

    pub fn polls(&self) -> Vec<i64> {
        lock(&self.state).polls.clone()
    }

    pub fn offset(&self) -> i64 {
        lock(&self.state).offset
    }

    /// Run a journal command (`--connector-journal-config`).
    pub fn journal(
        &self,
        command: &str,
        id: Option<&str>,
        revision: Option<i64>,
        action: Option<&str>,
        confirmation: Option<&str>,
    ) -> Result<JournalOutput> {
        journal(
            self.platform,
            &self.directory,
            command,
            id,
            revision,
            action,
            confirmation,
        )
    }

    /// Initialize the journal (`status`) and open it for seeding.
    pub fn open_journal(&self) -> Result<crate::db::Fixture> {
        let initialized = self.journal("status", None, None, None, None)?;
        if initialized.status != 0 {
            return Err(format!("journal status failed: {}", initialized.stderr).into());
        }
        crate::db::Fixture::open(
            self.directory
                .join(format!("{}.sqlite", self.platform.name())),
        )
    }
}

impl Drop for Connector {
    fn drop(&mut self) {
        if let Some(mut process) = self.process().take() {
            let _ = process.child.kill();
            let _ = process.child.wait();
        }
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Exit status and output of one journal command.
#[derive(Clone, Debug)]
pub struct JournalOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl JournalOutput {
    pub fn json(&self) -> Result<Value> {
        Ok(serde_json::from_str(&self.stdout)?)
    }
}

pub fn journal(
    platform: Platform,
    directory: &Path,
    command: &str,
    id: Option<&str>,
    revision: Option<i64>,
    action: Option<&str>,
    confirmation: Option<&str>,
) -> Result<JournalOutput> {
    let input = json!({"platform": platform.name(), "directory": directory, "command": command,
        "id": id, "revision": revision, "action": action, "confirmation": confirmation});
    let mut child = Command::new(server_binary(None)?)
        .arg("--connector-journal-config")
        .env_clear()
        .env("PATH", HERMETIC_PATH)
        .env("MINARBOT_TEST_SERVER", "rust")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.to_string().as_bytes())?;
    }
    let output = child.wait_with_output()?;
    Ok(JournalOutput {
        status: output.status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn spawn(options: &Value, stderr: &Path, home: &Path) -> Result<Process> {
    let mut child = Command::new(server_binary(None)?)
        .arg("--connector-config")
        .env_clear()
        .env("PATH", HERMETIC_PATH)
        .env("HOME", home)
        .env("MINARBOT_TEST_SERVER", "rust")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::from(File::create(stderr)?))
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(options.to_string().as_bytes())?;
    }
    Ok(Process {
        child,
        stderr: stderr.to_owned(),
    })
}

fn terminate(child: &mut Child) -> Result<()> {
    use rustix::process::{kill_process, Pid, Signal};
    let pid = Pid::from_raw(i32::try_from(child.id())?).ok_or("invalid connector PID")?;
    kill_process(pid, Signal::TERM)?;
    Ok(())
}

/// Run a connector to completion with `options` (the Node
/// `runRustConnector`), stopping it with SIGTERM when `stop` resolves.
/// Returns `Err` with the connector's error output when it fails.
pub async fn run(
    options: &Value,
    logs: &Path,
    stop: impl std::future::Future<Output = ()>,
) -> Result<()> {
    fs::create_dir_all(logs)?;
    let stderr = logs.join(format!("run-{}.stderr", uuid::Uuid::new_v4()));
    let mut process = spawn(options, &stderr, logs)?;
    tokio::pin!(stop);
    let mut stopped = false;
    loop {
        if let Some(status) = process.child.try_wait()? {
            if stopped || status.success() {
                return Ok(());
            }
            let mut text = String::new();
            File::open(&process.stderr)?.read_to_string(&mut text)?;
            let text = text.trim();
            return Err(if text.is_empty() {
                "Rust connector stopped.".into()
            } else {
                text.to_owned().into()
            });
        }
        if !stopped {
            tokio::select! {
                () = &mut stop => {
                    stopped = true;
                    terminate(&mut process.child)?;
                }
                () = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
        } else {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

/// Minarbot server with a script token, plus an agent with scripts enabled.
pub struct ConnectorApp {
    pub app: Server,
    pub agent: String,
}

/// `connectorApp`: start the server (`config` may add a model) and create the
/// connector agent with scripts enabled.
pub async fn connector_app(
    root: &TestRoot,
    agent: &str,
    config: ServerConfig,
) -> Result<ConnectorApp> {
    let app = root
        .start(config.option("scriptToken", SCRIPT_TOKEN))
        .await?;
    app.post(
        "/api/agents",
        json!({"runtime": "api", "id": agent, "name": "Connector", "model": "fixture",
            "instructions": "Help"}),
    )
    .await?;
    app.post(
        &format!("/api/agents/{agent}/scripts"),
        json!({"id": crate::new_id(), "revision": 0, "enabled": true}),
    )
    .await?;
    Ok(ConnectorApp {
        app,
        agent: agent.into(),
    })
}

/// Telegram private message update.
pub fn telegram_update(n: i64, text: &str) -> Value {
    telegram_update_in(n, text, 4)
}

pub fn telegram_update_in(n: i64, text: &str, chat: i64) -> Value {
    json!({"update_id": n, "message": {"message_id": n, "chat": {"id": chat, "type": "private"},
        "from": {"id": chat, "is_bot": false}, "text": text}})
}

/// Slack Socket Mode `events_api` envelope for a message event.
pub fn slack_envelope(n: u32, text: &str, user: &str, root: Option<&str>, channel: &str) -> Value {
    let mut event = json!({"type": "message",
        "channel_type": if channel.starts_with('D') { "im" } else { "channel" },
        "channel": channel, "user": user, "text": text, "ts": format!("1700000000.{n:06}")});
    if let Some(root) = root {
        event["thread_ts"] = json!(root);
    }
    json!({"type": "events_api", "envelope_id": format!("envelope-{n}"), "payload": {
        "type": "event_callback", "team_id": "TTEAM", "api_app_id": "AAPP", "event_id": format!("Ev{n}"),
        "authorizations": [{"team_id": "TTEAM", "user_id": "UBOT", "is_bot": true}], "event": event}})
}

async fn socket(State(api): State<Api>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |mut socket| async move {
        let (sender, mut receiver) = mpsc::unbounded_channel::<String>();
        let (connection, script) = {
            let mut state = lock(&api.state);
            state.socket = Some(sender);
            state.connections += 1;
            (state.connections, state.script.clone())
        };
        let hello = json!({"type": "hello", "connection_info": {"app_id": "AAPP"}}).to_string();
        if socket.send(Message::Text(hello.into())).await.is_err() {
            return;
        }
        for envelope in &script.on_connect {
            if socket.send(Message::Text(envelope.to_string().into())).await.is_err() {
                return;
            }
        }
        let mut acknowledged = false;
        loop {
            tokio::select! {
                outgoing = receiver.recv() => match outgoing {
                    Some(text) => if socket.send(Message::Text(text.into())).await.is_err() { break },
                    None => break,
                },
                incoming = socket.recv() => match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(value) = serde_json::from_str::<Value>(&text) {
                            if let Some(id) = value["envelope_id"].as_str() {
                                lock(&api.state).acks.push(id.to_owned());
                                if let Some(hook) = &script.on_ack {
                                    hook(id);
                                }
                                if script.disconnect_after_first_ack && connection == 1 && !acknowledged {
                                    let notice = json!({"type": "disconnect", "reason": "refresh_requested"});
                                    if socket.send(Message::Text(notice.to_string().into())).await.is_err() {
                                        break;
                                    }
                                }
                                acknowledged = true;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                },
            }
        }
    })
}

fn platform_json(platform: Platform, value: Value) -> Response {
    let body = match platform {
        Platform::Slack => {
            let mut body = json!({"ok": true});
            if let (Some(body), Some(fields)) = (body.as_object_mut(), value.as_object()) {
                body.extend(fields.clone());
            }
            body
        }
        Platform::Telegram => json!({"ok": true, "result": value}),
    };
    (
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

/// A reply whose body fails mid-stream, so the client sees a lost response.
fn lost() -> Response {
    let stream = futures_util::stream::once(async {
        Err::<Bytes, std::io::Error>(std::io::Error::other("lost reply"))
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| ().into_response())
}

async fn platform_call(
    State(api): State<Api>,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let path = uri.path().to_owned();
    let method = path.rsplit('/').next().unwrap_or_default().to_owned();
    let text = |name: header::HeaderName| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    {
        let mut state = lock(&api.state);
        if let Some(authorization) = text(header::AUTHORIZATION) {
            state.authorizations.push(authorization);
        }
        if state.script.rate_limit_once.contains(&method) {
            match state.limited.get(&method).copied() {
                None => {
                    state
                        .limited
                        .insert(method.clone(), std::time::Instant::now());
                    return (
                        axum::http::StatusCode::TOO_MANY_REQUESTS,
                        [(header::RETRY_AFTER, "1")],
                        "Wait",
                    )
                        .into_response();
                }
                Some(first) if first.elapsed() < Duration::from_secs(1) => {
                    state
                        .error
                        .get_or_insert(format!("{method} retried before Retry-After"));
                }
                Some(_) => {}
            }
        }
    }
    match method.as_str() {
        "auth.test" => {
            return platform_json(
                api.platform,
                json!({"team_id": "TTEAM", "user_id": "UBOT", "bot_id": "BBOT"}),
            )
        }
        "apps.connections.open" => {
            return platform_json(
                api.platform,
                json!({"url": format!("{}/socket", api.origin.replacen("http:", "ws:", 1))}),
            )
        }
        "getMe" => return platform_json(api.platform, json!({"id": 123, "is_bot": true})),
        "getUpdates" => {
            let poll: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
            let mut state = lock(&api.state);
            if poll["allowed_updates"] != json!(["message", "callback_query"]) {
                state
                    .error
                    .get_or_insert(format!("getUpdates allowed_updates: {poll}"));
            }
            let offset = poll["offset"].as_i64().unwrap_or(0);
            state.offset = offset;
            state.polls.push(offset);
            let updates: Vec<Value> = state
                .updates
                .iter()
                .filter(|update| update["update_id"].as_i64().unwrap_or(0) >= offset)
                .take(20)
                .cloned()
                .collect();
            return platform_json(api.platform, json!(updates));
        }
        _ => {}
    }
    let post = Post {
        method: method.clone(),
        raw: raw.to_vec(),
        content_type: text(header::CONTENT_TYPE),
        authorization: text(header::AUTHORIZATION),
        host: text(header::HOST).unwrap_or_default(),
    };
    let count = {
        let mut state = lock(&api.state);
        state.posts.push(post.clone());
        state.posts.len()
    };
    let body: Value = if post
        .content_type
        .as_deref()
        .is_some_and(|kind| kind.starts_with("application/json"))
    {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    let reply = match &api.remote {
        Some(remote) => remote(post).await,
        None => Reply::Default,
    };
    match reply {
        Reply::Lose => lost(),
        Reply::Raw(status, bytes) => (
            axum::http::StatusCode::from_u16(status).unwrap_or(axum::http::StatusCode::OK),
            bytes,
        )
            .into_response(),
        Reply::Value(value) => platform_json(api.platform, value),
        Reply::Default => platform_json(
            api.platform,
            match api.platform {
                Platform::Slack => {
                    json!({"channel": body["channel"], "ts": format!("1700000001.{count:06}")})
                }
                Platform::Telegram if method == "answerCallbackQuery" => json!(true),
                Platform::Telegram => json!({"chat": {"id": body["chat_id"].as_i64().unwrap_or(4)},
                    "message_id": 100 + count}),
            },
        ),
    }
}

fn header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

/// Script-API proxy: one request per connection (`Connection: close`).
async fn proxy(
    mut stream: TcpStream,
    state: &Shared,
    client: &reqwest::Client,
    target: &str,
) -> Result<()> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 8192];
    let head_end = loop {
        if let Some(end) = header_end(&buffer) {
            break end;
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split(' ');
    let method = request_line.next().unwrap_or("GET").to_owned();
    let path = request_line.next().unwrap_or("/").to_owned();
    let (mut length, mut authorization, mut content_type, mut name) = (0, None, None, None);
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim().to_owned();
            match key.trim().to_ascii_lowercase().as_str() {
                "content-length" => length = value.parse().unwrap_or(0),
                "authorization" => authorization = Some(value),
                "content-type" => content_type = Some(value),
                "x-file-name" => name = Some(value),
                _ => {}
            }
        }
    }
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);
    let request = ScriptRequest {
        method: method.clone(),
        path: path.clone(),
        bytes: body.clone(),
        name: name.clone(),
        authorization: authorization.clone(),
    };
    let fault = {
        let mut state = lock(state);
        state.requests.push(request.clone());
        let index = state.faults.iter().position(|fault| {
            fault.method.as_ref().is_none_or(|wanted| *wanted == method)
                && path.ends_with(&fault.suffix)
        });
        index.map(|index| state.faults.remove(index))
    };
    if fault.as_ref().is_some_and(|fault| !fault.after) {
        let _ = stream.shutdown().await;
        return Ok(());
    }
    if authorization.as_deref() != Some(&format!("Bearer {SCRIPT_TOKEN}")) {
        return Err(format!("script request without the script token: {path}").into());
    }
    let mut upstream = client
        .request(
            reqwest::Method::from_bytes(method.as_bytes())?,
            format!("{target}{path}"),
        )
        .header(header::AUTHORIZATION, authorization.unwrap_or_default());
    if let Some(kind) = &content_type {
        upstream = upstream.header(header::CONTENT_TYPE, kind);
    }
    if let Some(name) = &name {
        upstream = upstream.header("X-File-Name", name);
    }
    if method == "POST" {
        upstream = upstream.body(body);
    }
    let response = upstream.send().await?;
    let status = response.status();
    let kind = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();
    let bytes = response.bytes().await?;
    if status.is_success() {
        lock(state).successful.push(request);
    }
    if fault.is_some() {
        let _ = stream.shutdown().await;
        return Ok(());
    }
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or("Status"),
        bytes.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&bytes).await?;
    stream.shutdown().await?;
    Ok(())
}
