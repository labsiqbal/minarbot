//! Slack connector fixture (port of `app/test-support/rust-connector-fixture.ts`
//! for Slack, and of `app/rust-connectors-runner.ts`).
//!
//! [`SlackFixture`] serves a fake Slack Web API (`/api/<method>`) and a Socket
//! Mode WebSocket (`/socket`), writes the connector's `connection.json` for a
//! test server, and runs `minarbot-server --connector-config` as a child
//! process. Web API calls other than `auth.test` and
//! `apps.connections.open` go to the test's closure; uploads to the URL the
//! closure hands out (`/upload/<n>`) do too. [`journal`] runs
//! `minarbot-server --connector-journal-config`.

use crate::{server::server_binary, Result};
use axum::{
    body::{Body, Bytes},
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    io::Write as _,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
};
use tokio::{
    io::AsyncReadExt,
    net::TcpListener,
    process::{Child, Command},
    sync::mpsc,
    task::JoinHandle,
};

pub const BOT_TOKEN: &str = "xoxb-bbbbbbbbbbbbbbbbbbbbbbbb";
pub const APP_TOKEN: &str = "xapp-aaaaaaaaaaaaaaaaaaaaaaaa";

/// One call to the fake Slack Web API or upload URL.
#[derive(Clone, Debug)]
pub struct SlackCall {
    /// Last path segment (`chat.postMessage`, or the upload number).
    pub method: String,
    pub path: String,
    pub authorization: Option<String>,
    pub host: String,
    pub body: Vec<u8>,
}

impl SlackCall {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// Reply of the test's Web API closure.
pub enum SlackReply {
    /// Default Slack reply (`{ok, channel, ts}`).
    Default,
    /// `{ok: true, ...value}`.
    Ok(Value),
    /// Plain body for upload URLs.
    Raw(&'static str),
    /// Headers, then the connection breaks: the reply is lost.
    Lost,
}

type Remote = Arc<dyn Fn(&SlackCall) -> SlackReply + Send + Sync>;

#[derive(Default)]
struct Log {
    calls: Vec<SlackCall>,
    acks: Vec<String>,
    socket: Option<mpsc::UnboundedSender<String>>,
}

#[derive(Clone)]
struct Shared {
    origin: String,
    remote: Remote,
    log: Arc<Mutex<Log>>,
}

impl Shared {
    fn with<T>(&self, change: impl FnOnce(&mut Log) -> T) -> T {
        match self.log.lock() {
            Ok(mut log) => change(&mut log),
            Err(poisoned) => change(&mut poisoned.into_inner()),
        }
    }
}

/// Running fake Slack plus the connector process.
pub struct SlackFixture {
    shared: Shared,
    task: JoinHandle<()>,
    pub directory: PathBuf,
    script_directory: PathBuf,
    agent: String,
    users: Vec<Value>,
    child: Option<Child>,
}

impl SlackFixture {
    /// Fake Slack for `agent`, connector state in `<root>/state`; the
    /// connector reaches the server at `server_origin` with `script_token`.
    pub async fn start(
        root: &Path,
        agent: &str,
        server_origin: &str,
        script_token: &str,
        remote: impl Fn(&SlackCall) -> SlackReply + Send + Sync + 'static,
    ) -> Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let shared = Shared {
            origin: origin.clone(),
            remote: Arc::new(remote),
            log: Arc::new(Mutex::new(Log::default())),
        };
        let app = Router::new()
            .route("/socket", get(socket))
            .fallback(web_api)
            .with_state(shared.clone());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let directory = root.join("state");
        let script_directory = root.join("connector-scripts");
        std::fs::create_dir_all(&script_directory)?;
        std::fs::set_permissions(&script_directory, std::fs::Permissions::from_mode(0o700))?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(script_directory.join("connection.json"))?;
        file.write_all(
            json!({"url": server_origin, "token": script_token})
                .to_string()
                .as_bytes(),
        )?;
        Ok(Self {
            shared,
            task,
            directory,
            script_directory,
            agent: agent.into(),
            users: vec![json!("UUSER"), json!("UOTHER")],
            child: None,
        })
    }

    /// Path of the connector journal database.
    pub fn journal(&self) -> PathBuf {
        self.directory.join("slack.sqlite")
    }

    pub fn calls(&self) -> Vec<SlackCall> {
        self.shared.with(|log| log.calls.clone())
    }

    /// Web API calls of `method`.
    pub fn calls_of(&self, method: &str) -> Vec<SlackCall> {
        self.calls()
            .into_iter()
            .filter(|call| call.method == method)
            .collect()
    }

    /// Start `minarbot-server --connector-config`.
    pub async fn launch(&mut self) -> Result<()> {
        if self.child.is_some() {
            return Err("connector already running".into());
        }
        let options = json!({"platform": "slack", "directory": self.directory,
            "scriptDirectory": self.script_directory, "agent": self.agent,
            "origin": self.shared.origin, "channels": [], "team": "TTEAM", "app": "AAPP",
            "botToken": BOT_TOKEN, "appToken": APP_TOKEN, "users": self.users});
        let mut child = Command::new(server_binary(None)?)
            .arg("--connector-config")
            .env("MINARBOT_TEST_SERVER", "rust")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        {
            use tokio::io::AsyncWriteExt;
            let mut stdin = child.stdin.take().ok_or("connector stdin")?;
            stdin.write_all(options.to_string().as_bytes()).await?;
        }
        self.child = Some(child);
        Ok(())
    }

    /// Fail when the connector exited on its own (`check` in Node).
    pub fn check(&mut self) -> Result<()> {
        if let Some(child) = &mut self.child {
            if let Some(status) = child.try_wait()? {
                return Err(format!("connector stopped: {status}").into());
            }
        }
        Ok(())
    }

    /// SIGTERM the connector and wait for it.
    pub async fn stop(&mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
            if child.try_wait()?.is_none() {
                if let Some(pid) = child.id() {
                    let pid = rustix::process::Pid::from_raw(i32::try_from(pid)?)
                        .ok_or("connector pid")?;
                    rustix::process::kill_process(pid, rustix::process::Signal::TERM)?;
                }
            }
            child.wait().await?;
        }
        self.shared.with(|log| log.socket = None);
        Ok(())
    }

    /// Wait until the connector exits by itself; returns its stderr.
    pub async fn expected_failure(&mut self) -> Result<String> {
        let mut child = self.child.take().ok_or("connector is not running")?;
        let status = tokio::time::timeout(crate::WAIT, child.wait())
            .await
            .map_err(|_| "connector did not stop after the injected failure")??;
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            pipe.read_to_string(&mut stderr).await?;
        }
        if status.success() {
            return Err(format!("connector exited cleanly: {stderr}").into());
        }
        self.shared.with(|log| log.socket = None);
        Ok(stderr)
    }

    /// Push a Socket Mode envelope and wait for its acknowledgement.
    pub async fn send(&mut self, envelope: Value) -> Result<()> {
        let id = envelope["envelope_id"]
            .as_str()
            .ok_or("envelope id")?
            .to_owned();
        let before = self
            .shared
            .with(|log| log.acks.iter().filter(|ack| **ack == id).count());
        let sender = crate::wait::until("Slack socket open", || async {
            Ok(self.shared.with(|log| log.socket.clone()))
        })
        .await?;
        sender.send(envelope.to_string())?;
        crate::wait::until("Slack event acknowledged", || async {
            Ok((self
                .shared
                .with(|log| log.acks.iter().filter(|ack| **ack == id).count())
                > before)
                .then_some(()))
        })
        .await
    }
}

impl Drop for SlackFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Slack `message` envelope (`slackEnvelope` in Node).
pub fn envelope(n: u64, text: &str, root: Option<&str>) -> Value {
    let mut event = json!({"type": "message", "channel_type": "im", "channel": "DCHAT", "user": "UUSER",
        "text": text, "ts": format!("1700000000.{n:06}")});
    if let Some(root) = root {
        event["thread_ts"] = json!(root);
    }
    json!({"type": "events_api", "envelope_id": format!("envelope-{n}"),
        "payload": {"type": "event_callback", "team_id": "TTEAM", "api_app_id": "AAPP",
            "event_id": format!("Ev{n}"),
            "authorizations": [{"team_id": "TTEAM", "user_id": "UBOT", "is_bot": true}],
            "event": event}})
}

/// `minarbot-server --connector-journal-config` with `command` on stdin.
pub async fn journal(command: &Value) -> Result<Value> {
    let mut child = Command::new(server_binary(None)?)
        .arg("--connector-journal-config")
        .env("MINARBOT_TEST_SERVER", "rust")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = child.stdin.take().ok_or("journal stdin")?;
        stdin.write_all(command.to_string().as_bytes()).await?;
    }
    let output = child.wait_with_output().await?;
    if !output.status.success() {
        return Err(format!(
            "connector journal command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

async fn socket(State(shared): State<Shared>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| serve_socket(shared, socket))
}

async fn serve_socket(shared: Shared, mut socket: WebSocket) {
    let (sender, mut outgoing) = mpsc::unbounded_channel::<String>();
    if socket
        .send(Message::Text(
            json!({"type": "hello", "connection_info": {"app_id": "AAPP"}})
                .to_string()
                .into(),
        ))
        .await
        .is_err()
    {
        return;
    }
    shared.with(|log| log.socket = Some(sender));
    loop {
        tokio::select! {
            message = outgoing.recv() => match message {
                Some(text) => if socket.send(Message::Text(text.into())).await.is_err() { break },
                None => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                    if let Some(id) = value["envelope_id"].as_str() {
                        let id = id.to_owned();
                        shared.with(|log| log.acks.push(id));
                    }
                }
                Some(Ok(_)) => {}
                _ => break,
            },
        }
    }
}

async fn web_api(
    State(shared): State<Shared>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_owned();
    let method = path.rsplit('/').next().unwrap_or_default().to_owned();
    let call = SlackCall {
        method: method.clone(),
        path: path.clone(),
        authorization: headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        host: headers
            .get("host")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
        body: body.to_vec(),
    };
    let value = match method.as_str() {
        "auth.test" => json!({"team_id": "TTEAM", "user_id": "UBOT", "bot_id": "BBOT"}),
        "apps.connections.open" => {
            json!({"url": format!("{}/socket", shared.origin.replacen("http:", "ws:", 1))})
        }
        _ => {
            let count = shared.with(|log| {
                log.calls.push(call.clone());
                log.calls.len()
            });
            match (shared.remote)(&call) {
                SlackReply::Default => json!({"channel": call.json()["channel"],
                    "ts": format!("1700000001.{count:06}")}),
                SlackReply::Ok(value) => value,
                SlackReply::Raw(text) => return (StatusCode::OK, text).into_response(),
                SlackReply::Lost => {
                    let failure = futures_util::stream::once(async {
                        Err::<Bytes, std::io::Error>(std::io::Error::other("reply lost"))
                    });
                    return (StatusCode::OK, Body::from_stream(failure)).into_response();
                }
            }
        }
    };
    let mut reply = json!({"ok": true});
    if let (Some(reply), Some(fields)) = (reply.as_object_mut(), value.as_object()) {
        for (key, field) in fields {
            reply.insert(key.clone(), field.clone());
        }
    }
    Json(reply).into_response()
}
