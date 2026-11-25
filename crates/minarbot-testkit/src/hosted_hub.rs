//! Hosted Hub process and the node side of its test transport.
//!
//! [`HostedHub`] runs `minarbot-server --hub test-fixture` with an isolated
//! `MINARBOT_HUB_CONFIG` and drives its stdin control channel (clock,
//! queued posts, node withdrawals), as `app/test-support/hosted-hub.ts` did.
//! [`HubTransport`] answers the computer bridge's `hub-*` operations for a
//! node server (`app/rust-hub-test-bridge.ts`): manual clock, in-memory key
//! entries, hub HTTP with the accounts `Host`, and real WebSockets to the
//! hub's `/node` endpoint. Tests can rewrite received frames, hold or drop
//! sent frames, close sockets and lose one enrollment poll reply.

use crate::{
    bridge::BridgeError,
    machine_bridge::{Clock, Extra},
    Result,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::mpsc,
};
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest,
    protocol::{frame::coding::CloseCode, CloseFrame, WebSocketConfig},
    Message,
};

fn locked<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn environment(directory: &Path, config: &Value, port: u16) -> Result<PathBuf> {
    let file = directory.join("hub-config.json");
    let mut value = config.clone();
    value["port"] = json!(port);
    std::fs::write(&file, value.to_string())?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

/// `minarbot-server --hub <args>` against `config` (for example `admit`).
pub fn hub_command(directory: &Path, config: &Value, args: &[&str]) -> Result<String> {
    let file = environment(directory, config, 0)?;
    let output = std::process::Command::new(crate::server::server_binary(None)?)
        .arg("--hub")
        .args(args)
        .env_clear()
        .env("PATH", crate::server::HERMETIC_PATH)
        .env("HOME", directory)
        .env("MINARBOT_HUB_CONFIG", file)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "hub {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

/// Running hosted Hub fixture; killed on drop.
pub struct HostedHub {
    pub port: u16,
    child: Child,
    stdin: tokio::sync::Mutex<(ChildStdin, mpsc::UnboundedReceiver<String>)>,
}

impl HostedHub {
    pub async fn start(directory: &Path, config: &Value) -> Result<Self> {
        if let Some(data) = config["dataPath"].as_str() {
            use std::os::unix::fs::PermissionsExt;
            if Path::new(data).exists() {
                std::fs::set_permissions(data, std::fs::Permissions::from_mode(0o600))?;
            }
        }
        let file = environment(directory, config, 0)?;
        let mut child = Command::new(crate::server::server_binary(None)?)
            .args(["--hub", "test-fixture"])
            .env_clear()
            .env("PATH", crate::server::HERMETIC_PATH)
            .env("HOME", directory)
            .env("MINARBOT_HUB_CONFIG", file)
            .env("MINARBOT_TEST_SERVER", "rust")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let stdout = child.stdout.take().ok_or("hub stdout")?;
        let stdin = child.stdin.take().ok_or("hub stdin")?;
        let (lines_tx, mut lines) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if lines_tx.send(line).is_err() {
                    break;
                }
            }
        });
        let port = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(line) = lines.recv().await {
                if let Some(port) = line.strip_prefix("Minarbot hub: http://127.0.0.1:") {
                    return port.trim().parse::<u16>().map_err(crate::Error::from);
                }
            }
            Err("Hosted hub exited before readiness".into())
        })
        .await
        .map_err(|_| "Hosted hub did not listen")??;
        Ok(Self {
            port,
            child,
            stdin: tokio::sync::Mutex::new((stdin, lines)),
        })
    }

    /// `http://127.0.0.1:<port>`.
    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// One control command; returns `result` or the hub's error as
    /// `HubError` text `status code message`.
    pub async fn control(&self, input: Value) -> Result<Value> {
        let mut guard = self.stdin.lock().await;
        let (stdin, lines) = &mut *guard;
        stdin.write_all(format!("{input}\n").as_bytes()).await?;
        stdin.flush().await?;
        let line = tokio::time::timeout(Duration::from_secs(10), lines.recv())
            .await
            .map_err(|_| "hub control timed out")?
            .ok_or("hub control closed")?;
        let reply: Value = serde_json::from_str(&line)?;
        if let Some(error) = reply.get("error").filter(|error| !error.is_null()) {
            return Err(HubError::from_value(error).into());
        }
        Ok(reply["result"].clone())
    }
}

impl Drop for HostedHub {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

/// A Hub or node protocol error (`status`, `code`, `message`).
#[derive(Clone, Debug)]
pub struct HubError {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl HubError {
    pub fn from_value(value: &Value) -> Self {
        Self {
            status: value["status"].as_u64().unwrap_or(500) as u16,
            code: value["code"].as_str().unwrap_or_default().to_owned(),
            message: value["message"].as_str().unwrap_or_default().to_owned(),
        }
    }
}

impl std::fmt::Display for HubError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} {} {}", self.status, self.code, self.message)
    }
}

impl std::error::Error for HubError {}

/// Handle a frame interceptor uses to close the socket it came from.
#[derive(Clone)]
pub struct SocketHandle {
    close: mpsc::UnboundedSender<(u16, String)>,
}

impl SocketHandle {
    pub fn close(&self, code: u16, reason: &str) {
        let _ = self.close.send((code, reason.to_owned()));
    }
}

/// Received-frame interceptor: returns the frames the node should see.
pub type Receive = Arc<dyn Fn(&str, &SocketHandle) -> Vec<String> + Send + Sync>;

/// What to do with one outgoing node frame.
pub enum Outbound {
    /// Send it now.
    Now,
    /// Drop it.
    Drop,
    /// Send it after this future completes (if the socket is still open).
    After(futures_util::future::BoxFuture<'static, ()>),
    /// Send it, then close the socket with this code and reason.
    ThenClose(u16, String),
}

/// Sent-frame interceptor (the Node `WebSocket.prototype.send` patches).
pub type Outgoing = Arc<dyn Fn(&str) -> Outbound + Send + Sync>;

struct Socket {
    events: Vec<String>,
    closed: bool,
    sender: mpsc::UnboundedSender<Control>,
}

enum Control {
    Frame(String),
    Close(u16, String),
}

type Observer = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Default)]
struct Hooks {
    receive: Option<Receive>,
    outgoing: Option<Outgoing>,
    lose_enrollment: bool,
    observers: Vec<Observer>,
}

/// Node-side hub transport for one or more node servers.
#[derive(Clone)]
pub struct HubTransport {
    /// Public Hub origin (`https://<accounts host>`), as the node sees it.
    pub origin: String,
    pub accounts_host: String,
    local: String,
    clock: Clock,
    keys: Arc<Mutex<HashMap<String, String>>>,
    hooks: Arc<Mutex<Hooks>>,
    sockets: Arc<Mutex<HashMap<String, Socket>>>,
}

impl HubTransport {
    pub fn new(origin: &str, accounts_host: &str, local: &str, clock: &Clock) -> Self {
        Self {
            origin: origin.to_owned(),
            accounts_host: accounts_host.to_owned(),
            local: local.to_owned(),
            clock: clock.clone(),
            keys: Arc::default(),
            hooks: Arc::default(),
            sockets: Arc::default(),
        }
    }

    /// Same hub, clock, keys and hooks with a separate socket set, so each
    /// node server's transport closes with it (one Node bridge per node).
    pub fn for_node(&self) -> Self {
        Self {
            sockets: Arc::default(),
            ..self.clone()
        }
    }

    /// Close every socket of this transport (the node's bridge closing).
    pub fn close_all(&self) {
        for (_, socket) in locked(&self.sockets).drain() {
            let _ = socket.sender.send(Control::Close(1001, String::new()));
        }
    }

    /// Every key entry value stored by nodes.
    pub fn key_values(&self) -> Vec<String> {
        locked(&self.keys).values().cloned().collect()
    }

    pub fn key_count(&self) -> usize {
        locked(&self.keys).len()
    }

    /// Rewrite received frames (`None` restores delivery).
    pub fn receive_frames(&self, receive: Option<Receive>) {
        locked(&self.hooks).receive = receive;
    }

    /// Intercept sent frames (`None` restores direct sends).
    pub fn intercept_sends(&self, outgoing: Option<Outgoing>) {
        locked(&self.hooks).outgoing = outgoing;
    }

    /// Observe every received frame (the Node `frames` emitter).
    pub fn observe(&self, observer: Arc<dyn Fn(&str) + Send + Sync>) {
        locked(&self.hooks).observers.push(observer);
    }

    /// Lose the reply of the next enrollment poll.
    pub fn lose_enrollment_response(&self) {
        locked(&self.hooks).lose_enrollment = true;
    }

    /// Bridge operations for `MachineBridge::extra`.
    pub fn extra(&self) -> Extra {
        let transport = self.clone();
        Arc::new(move |operation: &str, args: &Value| {
            if !operation.starts_with("hub-") {
                return None;
            }
            let (transport, operation, args) =
                (transport.clone(), operation.to_owned(), args.clone());
            Some(Box::pin(async move {
                transport
                    .call(&operation, &args)
                    .await
                    .map_err(|error| BridgeError::new(503, error.to_string()))
            }))
        })
    }

    async fn call(&self, operation: &str, args: &Value) -> Result<Value> {
        let text = |name: &str| args[name].as_str().unwrap_or_default().to_owned();
        match operation {
            "hub-clock" => Ok(json!(self.clock.get())),
            "hub-key-get" => Ok(locked(&self.keys)
                .get(&format!("{}/{}", text("service"), text("name")))
                .map_or(Value::Null, |value| json!(value))),
            "hub-key-set" => {
                locked(&self.keys).insert(
                    format!("{}/{}", text("service"), text("name")),
                    text("value"),
                );
                Ok(Value::Null)
            }
            "hub-http" => {
                let path = text("path");
                let response = reqwest::Client::builder()
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(Duration::from_secs(10))
                    .build()?
                    .post(format!("{}{path}", self.local))
                    .header("Host", &self.accounts_host)
                    .header("Content-Type", "application/json")
                    .body(text("body"))
                    .send()
                    .await?;
                let status = response.status().as_u16();
                let body = response.text().await?;
                let lose = path.ends_with("/enrollment/poll")
                    && std::mem::take(&mut locked(&self.hooks).lose_enrollment);
                if lose {
                    return Err("Injected response loss".into());
                }
                if body.len() > 65536 {
                    return Err("Hub fixture response exceeded.".into());
                }
                Ok(json!({"status": status, "body": body}))
            }
            "hub-connect" => {
                self.connect(&text("id"), &args["headers"]).await?;
                Ok(Value::Null)
            }
            "hub-poll" => {
                let mut sockets = locked(&self.sockets);
                let socket = sockets
                    .get_mut(&text("id"))
                    .ok_or("Hub fixture socket missing.")?;
                Ok(json!({"frames": std::mem::take(&mut socket.events), "closed": socket.closed}))
            }
            "hub-send" => {
                let id = text("id");
                let frame = text("frame");
                let sender = {
                    let sockets = locked(&self.sockets);
                    let socket = sockets.get(&id).ok_or("Hub fixture socket missing.")?;
                    if socket.closed {
                        return Err("Hub fixture unavailable.".into());
                    }
                    socket.sender.clone()
                };
                let outgoing = locked(&self.hooks).outgoing.clone();
                match outgoing.map_or(Outbound::Now, |hook| hook(&frame)) {
                    Outbound::Now => {
                        sender
                            .send(Control::Frame(frame))
                            .map_err(|_| "Hub fixture unavailable.")?;
                    }
                    Outbound::Drop => {}
                    Outbound::ThenClose(code, reason) => {
                        sender
                            .send(Control::Frame(frame))
                            .map_err(|_| "Hub fixture unavailable.")?;
                        let _ = sender.send(Control::Close(code, reason));
                    }
                    Outbound::After(wait) => {
                        tokio::spawn(async move {
                            wait.await;
                            let _ = sender.send(Control::Frame(frame));
                        });
                    }
                }
                Ok(Value::Null)
            }
            "hub-close" => {
                let socket = locked(&self.sockets).remove(&text("id"));
                if let Some(socket) = socket {
                    let _ = socket.sender.send(Control::Close(1000, String::new()));
                }
                Ok(Value::Null)
            }
            _ => Err("Unknown hub fixture operation.".into()),
        }
    }

    async fn connect(&self, id: &str, headers: &Value) -> Result<()> {
        let url = format!("{}/node", self.local.replacen("http", "ws", 1));
        let mut request = url.into_client_request()?;
        for (name, value) in headers.as_object().into_iter().flatten() {
            if let Some(value) = value.as_str() {
                request.headers_mut().insert(
                    reqwest::header::HeaderName::from_bytes(name.as_bytes())?,
                    value.parse()?,
                );
            }
        }
        request
            .headers_mut()
            .insert("host", self.accounts_host.parse()?);
        let mut config = WebSocketConfig::default();
        config.max_message_size = Some(65536);
        config.max_frame_size = Some(65536);
        let (stream, _) = tokio::time::timeout(
            Duration::from_secs(10),
            tokio_tungstenite::connect_async_with_config(request, Some(config), false),
        )
        .await
        .map_err(|_| "Hub fixture handshake timed out")??;
        let (mut write, mut read) = stream.split();
        let (sender, mut control) = mpsc::unbounded_channel::<Control>();
        let (close_tx, mut close_rx) = mpsc::unbounded_channel::<(u16, String)>();
        locked(&self.sockets).insert(
            id.to_owned(),
            Socket {
                events: Vec::new(),
                closed: false,
                sender: sender.clone(),
            },
        );
        let forward = sender.clone();
        tokio::spawn(async move {
            while let Some((code, reason)) = close_rx.recv().await {
                let _ = forward.send(Control::Close(code, reason));
            }
        });
        let sockets = self.sockets.clone();
        let writer_id = id.to_owned();
        tokio::spawn(async move {
            while let Some(next) = control.recv().await {
                let done = match next {
                    Control::Frame(frame) => write.send(Message::Text(frame.into())).await.is_err(),
                    Control::Close(code, reason) => {
                        let _ = write
                            .send(Message::Close(Some(CloseFrame {
                                code: CloseCode::from(code),
                                reason: reason.into(),
                            })))
                            .await;
                        true
                    }
                };
                if done {
                    if let Some(socket) = locked(&sockets).get_mut(&writer_id) {
                        socket.closed = true;
                    }
                    break;
                }
            }
        });
        let (sockets, hooks, reader_id) = (self.sockets.clone(), self.hooks.clone(), id.to_owned());
        let handle = SocketHandle { close: close_tx };
        tokio::spawn(async move {
            while let Some(message) = read.next().await {
                let frame = match message {
                    Ok(Message::Text(text)) => text.to_string(),
                    Ok(Message::Ping(_) | Message::Pong(_)) => continue,
                    _ => break,
                };
                let (receive, observers) = {
                    let hooks = locked(&hooks);
                    (hooks.receive.clone(), hooks.observers.clone())
                };
                for observer in &observers {
                    observer(&frame);
                }
                let frames =
                    receive.map_or_else(|| vec![frame.clone()], |hook| hook(&frame, &handle));
                let mut sockets = locked(&sockets);
                let Some(socket) = sockets.get_mut(&reader_id) else {
                    break;
                };
                if socket.events.len() + frames.len() > 128 {
                    socket.closed = true;
                    break;
                }
                socket.events.extend(frames);
            }
            if let Some(socket) = locked(&sockets).get_mut(&reader_id) {
                socket.closed = true;
            }
        });
        Ok(())
    }
}

/// Try a WebSocket upgrade to the hub's `/node` endpoint with these headers
/// (browser credentials must be refused).
pub async fn browser_upgrade(local: &str, headers: &[(&str, &str)]) -> Result<()> {
    let url = format!("{}/node", local.replacen("http", "ws", 1));
    let mut request = url.into_client_request()?;
    for (name, value) in headers {
        request.headers_mut().insert(
            reqwest::header::HeaderName::from_bytes(name.as_bytes())?,
            value.parse()?,
        );
    }
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .map_err(|_| "upgrade timed out")??;
    Ok(())
}
