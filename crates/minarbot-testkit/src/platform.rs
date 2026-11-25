//! Fake Slack and Telegram for connector tests (port of
//! `app/test-support/rust-connector-fixture.ts`).
//!
//! [`ConnectorFixture::start`] runs:
//!
//! - a platform API stub: `auth.test`, `apps.connections.open` (Socket Mode
//!   WebSocket at `/socket`), `getMe`, `getUpdates` (from a queue), and a
//!   default answer for every other method, which a [`Hook`] can replace;
//! - a local script proxy between the connector and Minarbot that checks the
//!   script token and can drop a request before or after forwarding it
//!   ([`Fault`]);
//! - the connector process itself ([`crate::connector::Connector`]).

use crate::{
    connector::Connector,
    stub::{Socket, Stub, StubReply, StubRequest},
    Result,
};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub const TELEGRAM_TOKEN: &str = "123:xxxxxxxxxxxxxxxxxxxxxxxx";
pub const BOT_TOKEN: &str = "xoxb-bbbbbbbbbbbbbbbbbbbbbbbb";
pub const APP_TOKEN: &str = "xapp-aaaaaaaaaaaaaaaaaaaaaaaa";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Slack,
    Telegram,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Slack => "slack",
            Self::Telegram => "telegram",
        }
    }
}

/// One platform API call the connector made (`method` is the last path part).
#[derive(Clone, Debug)]
pub struct Post {
    pub method: String,
    pub request: StubRequest,
}

/// Answer of a [`Hook`]: a value merged into the platform envelope, or a raw reply.
pub enum HookReply {
    Value(Value),
    Raw(StubReply),
}

/// Replaces the default answer of a platform method.
pub type Hook = Arc<dyn Fn(&str, &StubRequest) -> Option<HookReply> + Send + Sync>;

/// A script-proxy fault: drop the matching request before forwarding it, or
/// after Minarbot handled it (the response is lost).
#[derive(Clone)]
pub struct Fault {
    pub method: Option<&'static str>,
    pub matches: fn(&str) -> bool,
    pub after: bool,
}

#[derive(Default)]
struct Shared {
    posts: Vec<Post>,
    updates: Vec<Value>,
    acks: Vec<String>,
    polls: Vec<i64>,
    faults: Vec<Fault>,
    sockets: Vec<tokio::sync::mpsc::UnboundedSender<String>>,
    problems: Vec<String>,
}

pub struct ConnectorFixture {
    pub kind: Kind,
    /// Connector state directory (`<platform>.sqlite`, `owner.lock`).
    pub directory: PathBuf,
    script_directory: PathBuf,
    agent: String,
    users: Vec<Value>,
    channels: Vec<String>,
    shared: Arc<Mutex<Shared>>,
    api: Stub,
    _local: Stub,
    connector: Option<Connector>,
}

fn lock(shared: &Arc<Mutex<Shared>>) -> std::sync::MutexGuard<'_, Shared> {
    match shared.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Forward a script request to Minarbot (`app_origin`) with its headers.
pub async fn forward(client: &reqwest::Client, app_origin: &str, request: &StubRequest) -> Result<StubReply> {
    let method = reqwest::Method::from_bytes(request.method.as_bytes())?;
    let mut builder = client.request(method.clone(), format!("{app_origin}{}", request.target));
    for name in ["authorization", "content-type", "x-file-name"] {
        if let Some(value) = request.header(name) {
            builder = builder.header(name, value);
        }
    }
    if method == reqwest::Method::POST {
        builder = builder.body(request.body.clone());
    }
    let response = builder.send().await?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();
    let bytes = response.bytes().await?.to_vec();
    Ok(StubReply::Raw(status, vec![("Content-Type".into(), content_type)], bytes))
}

impl ConnectorFixture {
    /// Fixture for `agent` whose connector reaches Minarbot at `app_origin`
    /// with `script_token`.
    pub async fn start(
        kind: Kind,
        root: &Path,
        agent: &str,
        app_origin: &str,
        script_token: &str,
        hook: Option<Hook>,
        users: Vec<Value>,
        channels: Vec<String>,
    ) -> Result<Self> {
        let directory = root.join("state");
        let script_directory = root.join("connector-scripts");
        std::fs::create_dir_all(&script_directory)?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_directory, std::fs::Permissions::from_mode(0o700))?;
        }
        let shared: Arc<Mutex<Shared>> = Arc::default();
        let api_shared = shared.clone();
        let origin_cell: Arc<Mutex<String>> = Arc::default();
        let api_origin = origin_cell.clone();
        let socket_shared = shared.clone();
        let api = Stub::with_socket(
            move |request| {
                let reply = platform(kind, &api_shared, &api_origin, hook.as_ref(), &request);
                async move { reply }
            },
            move |socket: Socket| {
                let Socket { mut incoming, outgoing, .. } = socket;
                let _ = outgoing.send(json!({"type": "hello", "connection_info": {"app_id": "AAPP"}}).to_string());
                lock(&socket_shared).sockets.push(outgoing);
                let acks = socket_shared.clone();
                tokio::spawn(async move {
                    while let Some(text) = incoming.recv().await {
                        let envelope = serde_json::from_str::<Value>(&text)
                            .ok()
                            .and_then(|value| value["envelope_id"].as_str().map(str::to_owned));
                        if let Some(envelope) = envelope {
                            lock(&acks).acks.push(envelope);
                        }
                    }
                });
            },
        )
        .await?;
        if let Ok(mut origin) = origin_cell.lock() {
            *origin = api.origin().to_owned();
        }
        let local_shared = shared.clone();
        let (app, token) = (app_origin.to_owned(), format!("Bearer {script_token}"));
        let client = reqwest::Client::builder().no_proxy().build()?;
        let local = Stub::start_async(move |request| {
            let (shared, app, token, client) = (local_shared.clone(), app.clone(), token.clone(), client.clone());
            async move {
                let fault = {
                    let mut state = lock(&shared);
                    let index = state.faults.iter().position(|fault| {
                        fault.method.is_none_or(|method| method == request.method) && (fault.matches)(&request.target)
                    });
                    index.map(|index| state.faults.remove(index))
                };
                if fault.as_ref().is_some_and(|fault| !fault.after) {
                    return StubReply::Drop;
                }
                if request.header("authorization") != Some(token.as_str()) {
                    lock(&shared).problems.push(format!("script request without the token: {}", request.target));
                }
                let reply = forward(&client, &app, &request).await;
                if fault.is_some() {
                    return StubReply::Drop;
                }
                reply.unwrap_or(StubReply::Drop)
            }
        })
        .await?;
        std::fs::write(
            script_directory.join("connection.json"),
            json!({"url": local.origin(), "token": script_token}).to_string(),
        )?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(script_directory.join("connection.json"), std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self {
            kind,
            directory,
            script_directory,
            agent: agent.to_owned(),
            users,
            channels,
            shared,
            api,
            _local: local,
            connector: None,
        })
    }

    pub fn origin(&self) -> &str {
        self.api.origin()
    }

    fn options(&self) -> Value {
        let mut options = json!({"platform": self.kind.name(), "directory": self.directory,
            "scriptDirectory": self.script_directory, "agent": self.agent, "origin": self.api.origin(),
            "channels": self.channels, "users": self.users});
        match self.kind {
            Kind::Slack => {
                options["team"] = json!("TTEAM");
                options["app"] = json!("AAPP");
                options["botToken"] = json!(BOT_TOKEN);
                options["appToken"] = json!(APP_TOKEN);
            }
            Kind::Telegram => options["token"] = json!(TELEGRAM_TOKEN),
        }
        options
    }

    /// Start (or restart) the connector process.
    pub async fn start_connector(&mut self) -> Result<()> {
        if self.connector.is_some() {
            return Err("connector already running".into());
        }
        lock(&self.shared).sockets.clear();
        self.connector = Some(Connector::start(&self.options()).await?);
        Ok(())
    }

    /// Stop the connector (SIGTERM) and fail on fixture problems.
    pub async fn stop(&mut self) -> Result<()> {
        if let Some(mut connector) = self.connector.take() {
            connector.stop().await?;
        }
        self.problems()
    }

    /// Fail when the connector stopped by itself or the fakes saw a problem.
    pub fn check(&mut self) -> Result<()> {
        self.problems()?;
        match &mut self.connector {
            Some(connector) => connector.check(),
            None => Err("connector is not running".into()),
        }
    }

    fn problems(&self) -> Result<()> {
        let problems = lock(&self.shared).problems.clone();
        if problems.is_empty() {
            Ok(())
        } else {
            Err(format!("platform fixture: {problems:?}").into())
        }
    }

    /// Wait until the connector stops after an injected failure; returns stderr.
    pub async fn expected_failure(&mut self) -> Result<String> {
        let mut connector = self.connector.take().ok_or("connector is not running")?;
        connector.failure().await
    }

    pub fn push_fault(&self, fault: Fault) {
        lock(&self.shared).faults.push(fault);
    }

    pub fn posts(&self) -> Vec<Post> {
        lock(&self.shared).posts.clone()
    }

    /// JSON bodies of posts to `method`.
    pub fn json_posts(&self, method: &str) -> Vec<Value> {
        self.posts()
            .iter()
            .filter(|post| post.method == method)
            .map(|post| post.request.json())
            .collect()
    }

    pub fn acks(&self) -> Vec<String> {
        lock(&self.shared).acks.clone()
    }

    pub fn polls(&self) -> Vec<i64> {
        lock(&self.shared).polls.clone()
    }

    /// Send a Slack envelope over the open socket, or queue a Telegram update,
    /// then wait until the connector committed it (ack or binding offset).
    pub async fn send(&mut self, message: Value) -> Result<()> {
        match self.kind {
            Kind::Slack => {
                let envelope = message["envelope_id"].as_str().unwrap_or_default().to_owned();
                let before = self.acks().iter().filter(|id| **id == envelope).count();
                crate::wait::until("Slack socket open", || async {
                    self.problems()?;
                    Ok(lock(&self.shared).sockets.last().cloned())
                })
                .await?
                .send(message.to_string())
                .map_err(|_| "Slack socket closed")?;
                crate::wait::until("Slack event acknowledged", || async {
                    self.problems()?;
                    Ok((self.acks().iter().filter(|id| **id == envelope).count() > before).then_some(()))
                })
                .await
            }
            Kind::Telegram => {
                let update = message["update_id"].as_i64().unwrap_or(0);
                lock(&self.shared).updates.push(message);
                let database = self.directory.join("telegram.sqlite");
                crate::wait::until("Telegram update committed", || async {
                    self.problems()?;
                    let offset = crate::db::Inspector::open(&database)
                        .and_then(|db| db.scalar::<i64>("SELECT offset FROM telegram_binding WHERE id=1", []))
                        .unwrap_or(0);
                    Ok((offset > update).then_some(()))
                })
                .await
            }
        }
    }

    /// Push a raw frame to the current Slack socket without waiting.
    pub fn push_socket(&self, frame: Value) -> Result<()> {
        lock(&self.shared)
            .sockets
            .last()
            .ok_or("no Slack socket")?
            .send(frame.to_string())
            .map_err(|_| "Slack socket closed".into())
    }
}

impl Drop for ConnectorFixture {
    fn drop(&mut self) {
        self.connector.take();
    }
}

fn platform(
    kind: Kind,
    shared: &Arc<Mutex<Shared>>,
    origin: &Arc<Mutex<String>>,
    hook: Option<&Hook>,
    request: &StubRequest,
) -> StubReply {
    let method = request.path().rsplit('/').next().unwrap_or_default().to_owned();
    let origin = origin.lock().map(|origin| origin.clone()).unwrap_or_default();
    let mut state = lock(shared);
    let value = match method.as_str() {
        "auth.test" => json!({"team_id": "TTEAM", "user_id": "UBOT", "bot_id": "BBOT"}),
        "apps.connections.open" => json!({"url": format!("{}/socket", origin.replacen("http:", "ws:", 1))}),
        "getMe" => json!({"id": 123, "is_bot": true}),
        "getUpdates" => {
            let poll = request.json();
            if poll["allowed_updates"] != json!(["message", "callback_query"]) {
                state.problems.push(format!("allowed_updates {}", poll["allowed_updates"]));
            }
            let offset = poll["offset"].as_i64().unwrap_or(0);
            state.polls.push(offset);
            json!(state
                .updates
                .iter()
                .filter(|update| update["update_id"].as_i64().unwrap_or(0) >= offset)
                .take(20)
                .cloned()
                .collect::<Vec<_>>())
        }
        _ => {
            state.posts.push(Post {
                method: method.clone(),
                request: request.clone(),
            });
            let count = state.posts.len();
            drop(state);
            match hook.and_then(|hook| hook(&method, request)) {
                Some(HookReply::Raw(reply)) => return reply,
                Some(HookReply::Value(value)) => value,
                None => {
                    let body = request.json();
                    match kind {
                        Kind::Slack => json!({"channel": body["channel"], "ts": format!("1700000001.{count:06}")}),
                        Kind::Telegram if method == "answerCallbackQuery" => json!(true),
                        Kind::Telegram => json!({"chat": {"id": if body["chat_id"].is_null() { json!(4) } else { body["chat_id"].clone() }},
                            "message_id": 100 + count}),
                    }
                }
            }
        }
    };
    StubReply::ok(match kind {
        Kind::Slack => {
            let mut envelope = json!({"ok": true});
            if let (Some(target), Some(source)) = (envelope.as_object_mut(), value.as_object()) {
                target.extend(source.clone());
            }
            envelope
        }
        Kind::Telegram => json!({"ok": true, "result": value}),
    })
}

/// A Slack Socket Mode message envelope.
pub fn slack_envelope(n: u64, text: &str, user: &str, root: Option<&str>, channel: &str) -> Value {
    let mut event = json!({"type": "message", "channel_type": if channel.starts_with('D') { "im" } else { "channel" },
        "channel": channel, "user": user, "text": text, "ts": format!("1700000000.{n:06}")});
    if let Some(root) = root {
        event["thread_ts"] = json!(root);
    }
    json!({"type": "events_api", "envelope_id": format!("envelope-{n}"), "payload": {"type": "event_callback",
        "team_id": "TTEAM", "api_app_id": "AAPP", "event_id": format!("Ev{n}"),
        "authorizations": [{"team_id": "TTEAM", "user_id": "UBOT", "is_bot": true}], "event": event}})
}

/// A Telegram private-chat update.
pub fn telegram_update(n: i64, text: &str, chat: i64) -> Value {
    json!({"update_id": n, "message": {"message_id": n, "chat": {"id": chat, "type": "private"},
        "from": {"id": chat, "is_bot": false}, "text": text}})
}

/// Hook that receives uploaded artifacts (the Node `artifactReceiver`).
pub fn artifact_receiver(kind: Kind, uploads: Arc<Mutex<Vec<Vec<u8>>>>) -> Hook {
    Arc::new(move |method, request| {
        let push = |bytes: Vec<u8>| {
            if let Ok(mut uploads) = uploads.lock() {
                uploads.push(bytes);
            }
        };
        match method {
            "files.getUploadURLExternal" => Some(HookReply::Value(json!({"file_id": "FFILE",
                "upload_url": format!("http://{}/uploadFile", request.header("host").unwrap_or_default())}))),
            "uploadFile" => {
                if request.header("authorization").is_some() {
                    return Some(HookReply::Raw(StubReply::Json(400, json!({"error": "upload carried credentials"}))));
                }
                push(request.body.clone());
                Some(HookReply::Raw(StubReply::Raw(200, vec![], b"OK".to_vec())))
            }
            "files.completeUploadExternal" => Some(HookReply::Value(json!({"files": request.json()["files"]}))),
            "sendDocument" if kind == Kind::Telegram => {
                let boundary = request
                    .header("content-type")
                    .and_then(|value| value.split("boundary=").nth(1))
                    .map(|value| value.trim_matches('"').to_owned())?;
                let fields = split_multipart(&request.body, &boundary);
                let document = fields.iter().find(|(head, _)| head.contains("name=\"document\""))?;
                push(document.1.clone());
                let chat = fields
                    .iter()
                    .find(|(head, _)| head.contains("name=\"chat_id\""))
                    .and_then(|(_, value)| String::from_utf8_lossy(value).trim().parse::<i64>().ok())?;
                let count = uploads.lock().map(|uploads| uploads.len()).unwrap_or(0);
                Some(HookReply::Value(json!({"chat": {"id": chat}, "message_id": 1000 + count})))
            }
            _ => None,
        }
    })
}

/// Parts of a `multipart/form-data` body as (headers, content).
pub fn split_multipart(body: &[u8], boundary: &str) -> Vec<(String, Vec<u8>)> {
    let marker = format!("--{boundary}").into_bytes();
    let mut parts = Vec::new();
    let mut positions = Vec::new();
    let mut index = 0;
    while index + marker.len() <= body.len() {
        if body[index..index + marker.len()] == marker[..] {
            positions.push(index);
            index += marker.len();
        } else {
            index += 1;
        }
    }
    for pair in positions.windows(2) {
        let part = &body[pair[0] + marker.len()..pair[1]];
        let part = part.strip_prefix(b"\r\n").unwrap_or(part);
        let Some(split) = part.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&part[..split]).into_owned();
        let mut content = part[split + 4..].to_vec();
        if content.ends_with(b"\r\n") {
            content.truncate(content.len() - 2);
        }
        parts.push((head, content));
    }
    parts
}
