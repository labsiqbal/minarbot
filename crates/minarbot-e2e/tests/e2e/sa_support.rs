//! Helpers shared by the `sa_*` modules (ports of the stale-a Node suites).

use minarbot_testkit::{model::ModelRequest, prelude::*, Server};

/// String field `name` of `value`, or an error naming the field.
pub fn text(value: &Value, name: &str) -> Result<String> {
    value[name]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("missing string {name} in {value}").into())
}

/// `id` of a created row.
pub fn id(value: &Value) -> Result<String> {
    text(value, "id")
}

/// Number of entries in `value[name]` (0 when it is not an array).
pub fn count(value: &Value, name: &str) -> usize {
    value[name].as_array().map_or(0, Vec::len)
}

/// Create an agent bound to the gateway peer (`runtime: "api"`, test mode).
pub async fn api_agent(app: &Server, name: &str) -> Result<Value> {
    app.post_expect(
        "/api/agents",
        json!({"runtime": "api", "id": new_id(), "name": name, "model": "fixture",
            "instructions": "Help"}),
        201,
    )
    .await
}

/// Channel `name` led by `lead` with `members`.
pub async fn channel(app: &Server, name: &str, lead: &str, members: &[&str]) -> Result<Value> {
    app.post_expect(
        "/api/channels",
        json!({"id": new_id(), "name": name, "instructions": "Help", "lead": lead,
            "members": members}),
        201,
    )
    .await
}

/// Side thread of `agent`.
pub async fn side_thread(app: &Server, agent: &str, name: &str) -> Result<Value> {
    app.post(
        &format!("/api/agents/{agent}/threads"),
        json!({"id": new_id(), "name": name}),
    )
    .await
}

/// Wait until turn `turn` in `path` is `done`/`failed`/`cancelled` and no room
/// turn rooted at it is still queued or running; returns the turn row.
pub async fn settle(app: &Server, path: &str, turn: &str) -> Result<Value> {
    let rows = app
        .wait_get(path, |rows| {
            let Some(rows) = rows.as_array() else {
                return false;
            };
            let finished = rows.iter().any(|row| {
                row["id"] == turn
                    && ["done", "failed", "cancelled"]
                        .contains(&row["status"].as_str().unwrap_or(""))
            });
            finished
                && !rows.iter().any(|row| {
                    row["room"]["root"] == turn
                        && ["queued", "running"].contains(&row["status"].as_str().unwrap_or(""))
                })
        })
        .await?;
    rows.as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == turn))
        .cloned()
        .ok_or_else(|| format!("turn {turn} missing from {path}").into())
}

/// JSON content of the last `tool` message of a model request.
pub fn last_tool_result(request: &ModelRequest) -> Option<Value> {
    request
        .tool_results()
        .last()
        .and_then(|message| message["content"].as_str())
        .and_then(|content| serde_json::from_str(content).ok())
}

/// Whether `request` offers a tool called `name`.
pub fn offers(request: &ModelRequest, name: &str) -> bool {
    request.tool_names().iter().any(|tool| tool == name)
}

/// State of a [`tool_loop`] model: the first step of every turn calls `tool`
/// with `args`; the step after the tool result records it and answers `done`
/// (or holds when `hold` is set).
#[derive(Debug, Default)]
pub struct ToolLoop {
    pub call_id: String,
    pub tool: String,
    pub args: Value,
    pub done: String,
    pub hold: bool,
    pub held: bool,
    /// Last tool result the model saw.
    pub result: Value,
    /// System context of the last first step.
    pub initial: String,
    /// Requests that did not offer `tool` (the Node provider asserted it).
    pub missing: usize,
}

pub type Shared<T> = std::sync::Arc<std::sync::Mutex<T>>;

/// Lock `shared`, mapping poisoning to an error.
pub fn lock<T>(shared: &Shared<T>) -> Result<std::sync::MutexGuard<'_, T>> {
    shared.lock().map_err(|_| "test state poisoned".into())
}

/// Script `model` as a [`ToolLoop`].
pub fn tool_loop(
    model: &minarbot_testkit::model::ScriptedModel,
    call_id: &str,
    tool: &str,
    args: Value,
    done: &str,
) -> Shared<ToolLoop> {
    use minarbot_testkit::model::Reply;
    let state = std::sync::Arc::new(std::sync::Mutex::new(ToolLoop {
        call_id: call_id.into(),
        tool: tool.into(),
        args,
        done: done.into(),
        ..ToolLoop::default()
    }));
    let shared = state.clone();
    model.respond(move |request| {
        let Ok(mut state) = shared.lock() else {
            return Reply::status(500, json!({"error": "poisoned"}));
        };
        if !offers(request, &state.tool) {
            state.missing += 1;
        }
        if after_tool(request) {
            state.result = last_tool_result(request).unwrap_or(Value::Null);
            if state.hold {
                state.held = true;
                return Reply::hold();
            }
            return Reply::text(state.done.clone());
        }
        state.initial = request.system();
        Reply::tool_call(&state.call_id, &state.tool, state.args.clone())
    });
    state
}

/// Whether the last message of `request` is a tool result (the step after a
/// tool call), as opposed to the first step of a turn.
pub fn after_tool(request: &ModelRequest) -> bool {
    request
        .messages()
        .last()
        .is_some_and(|message| message["role"] == "tool")
}

/// Tool messages of the current turn: those after the last user message.
pub fn turn_tool_results(request: &ModelRequest) -> Vec<Value> {
    let messages = request.messages();
    let start = messages
        .iter()
        .rposition(|message| message["role"] == "user")
        .map_or(0, |index| index + 1);
    messages[start..]
        .iter()
        .filter(|message| message["role"] == "tool")
        .cloned()
        .collect()
}

/// JSON content of a tool message.
pub fn tool_json(message: &Value) -> Value {
    message["content"]
        .as_str()
        .and_then(|content| serde_json::from_str(content).ok())
        .unwrap_or(Value::Null)
}

/// Raw request with exactly `headers` (no implicit Origin).
pub async fn raw(
    app: &Server,
    method: minarbot_testkit::http::Method,
    path: &str,
    headers: &[(&str, &str)],
) -> Result<u16> {
    let mut request = app.client()?.request(method, app.url(path));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    Ok(request.send().await?.status().as_u16())
}

/// Hashed Leptos startup asset of [`web_dist`].
pub const DIST_SCRIPT: &str = "/_leptos/assets/app.0123456789abcdef.js";
/// Hashed icon of [`web_dist`].
pub const DIST_ICON: &str = "/_leptos/assets/icon.fedcba9876543210.svg";
/// Bytes of [`DIST_SCRIPT`]; not valid UTF-8, so a re-encoding server fails.
pub const DIST_SCRIPT_BYTES: &[u8] = b"export const fixture = 1;\n\xff\x00";

/// Minimal packaged web build (`dist` option): the documents the server
/// requires plus one hashed script and icon, so static serving is tested
/// without building the Leptos client.
pub fn web_dist(root: &minarbot_testkit::TestRoot) -> Result<std::path::PathBuf> {
    let dist = root.dir("dist")?;
    let assets = root.dir("dist/_leptos/assets")?;
    let page = format!(
        "<!doctype html><html><head><title>__SETTINGS_TITLE__</title>\
         <link rel=\"icon\" href=\"{DIST_ICON}\"><script type=\"module\" src=\"{DIST_SCRIPT}\"></script>\
         </head><body></body></html>"
    );
    for document in ["appearance.html", "workflows.html", "conversation.html"] {
        std::fs::write(dist.join("_leptos").join(document), &page)?;
    }
    std::fs::write(assets.join("app.0123456789abcdef.js"), DIST_SCRIPT_BYTES)?;
    std::fs::write(
        assets.join("icon.fedcba9876543210.svg"),
        "<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
    )?;
    Ok(dist)
}

/// One request a [`RawProvider`] received.
#[derive(Clone, Debug)]
pub struct RawRequest {
    pub method: String,
    pub path: String,
    pub authorization: Option<String>,
    pub body: Vec<u8>,
}

impl RawRequest {
    /// The body as JSON (`Null` when it is not JSON).
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// Reply of a [`RawProvider`]: status, extra headers and body bytes.
pub type RawReply = (u16, Vec<(String, String)>, Vec<u8>);

/// Loopback HTTP/1.1 provider whose replies a closure decides byte for byte
/// (redirects, oversized or malformed bodies), for upstream failure modes the
/// scripted model cannot produce.
pub struct RawProvider {
    origin: String,
    requests: Shared<Vec<RawRequest>>,
    task: tokio::task::JoinHandle<()>,
}

/// What a [`RawProvider`] does with one request.
pub enum RawAction {
    /// Answer with this reply.
    Reply(RawReply),
    /// Send the head and these first body bytes of a larger body, then keep
    /// the connection open until the client closes it; `closed` is notified
    /// then.
    Partial(RawReply, std::sync::Arc<tokio::sync::Notify>),
    /// Close the connection without a reply (a lost response).
    Lose,
}

/// Asynchronous responder of a [`RawProvider`].
pub type RawResponder = std::sync::Arc<
    dyn Fn(RawRequest) -> std::pin::Pin<Box<dyn std::future::Future<Output = RawAction> + Send>>
        + Send
        + Sync,
>;

impl RawProvider {
    pub async fn start(
        reply: impl Fn(&RawRequest) -> RawReply + Send + Sync + 'static,
    ) -> Result<Self> {
        let reply = std::sync::Arc::new(reply);
        Self::start_async(std::sync::Arc::new(move |request: RawRequest| {
            let action = RawAction::Reply(reply(&request));
            Box::pin(async move { action })
        }))
        .await
    }

    /// Provider whose replies may wait (hold) or stream partially.
    pub async fn start_async(respond: RawResponder) -> Result<Self> {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let requests: Shared<Vec<RawRequest>> = Shared::default();
        let log = requests.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (log, respond) = (log.clone(), respond.clone());
                tokio::spawn(async move {
                    let mut stream = tokio::io::BufReader::new(stream);
                    let mut head = Vec::new();
                    while !head.ends_with(b"\r\n\r\n") && head.len() < 65_536 {
                        let mut byte = [0_u8; 1];
                        if stream.read(&mut byte).await.unwrap_or(0) == 0 {
                            return;
                        }
                        head.push(byte[0]);
                    }
                    let text = String::from_utf8_lossy(&head).into_owned();
                    let mut first = text.split("\r\n").next().unwrap_or_default().split(' ');
                    let (method, path) = (
                        first.next().unwrap_or_default().to_owned(),
                        first.next().unwrap_or_default().to_owned(),
                    );
                    let header = |wanted: &str| {
                        text.split("\r\n").skip(1).find_map(|line| {
                            line.split_once(':')
                                .filter(|(name, _)| name.trim().eq_ignore_ascii_case(wanted))
                                .map(|(_, value)| value.trim().to_owned())
                        })
                    };
                    let mut body = Vec::new();
                    if header("transfer-encoding")
                        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
                    {
                        loop {
                            let mut size = String::new();
                            if stream.read_line(&mut size).await.unwrap_or(0) == 0 {
                                return;
                            }
                            let Ok(size) = usize::from_str_radix(
                                size.trim().split(';').next().unwrap_or("0"),
                                16,
                            ) else {
                                return;
                            };
                            let mut chunk = vec![0_u8; size + 2];
                            if stream.read_exact(&mut chunk).await.is_err() {
                                return;
                            }
                            if size == 0 {
                                break;
                            }
                            body.extend_from_slice(&chunk[..size]);
                        }
                    } else {
                        let length: usize = header("content-length")
                            .and_then(|value| value.parse().ok())
                            .unwrap_or(0);
                        body = vec![0_u8; length];
                        if stream.read_exact(&mut body).await.is_err() {
                            return;
                        }
                    }
                    let request = RawRequest {
                        method,
                        path,
                        authorization: header("authorization"),
                        body,
                    };
                    if let Ok(mut log) = log.lock() {
                        log.push(request.clone());
                    }
                    let (reply, closed) = match respond(request).await {
                        RawAction::Reply(reply) => (reply, None),
                        RawAction::Partial(reply, closed) => (reply, Some(closed)),
                        RawAction::Lose => {
                            let _ = stream.get_mut().shutdown().await;
                            return;
                        }
                    };
                    let (status, headers, body) = reply;
                    let length = if closed.is_some() {
                        body.len() + 1_000_000
                    } else {
                        body.len()
                    };
                    let mut out = format!(
                        "HTTP/1.1 {status} X\r\ncontent-length: {length}\r\nconnection: close\r\n"
                    );
                    for (name, value) in headers {
                        out.push_str(&format!("{name}: {value}\r\n"));
                    }
                    out.push_str("\r\n");
                    let stream = stream.get_mut();
                    let _ = stream.write_all(out.as_bytes()).await;
                    let _ = stream.write_all(&body).await;
                    match closed {
                        Some(closed) => {
                            let mut rest = [0_u8; 1024];
                            while stream.read(&mut rest).await.unwrap_or(0) > 0 {}
                            closed.notify_one();
                        }
                        None => {
                            let _ = stream.shutdown().await;
                        }
                    }
                });
            }
        });
        Ok(Self {
            origin,
            requests,
            task,
        })
    }

    /// OpenAI-compatible base URL (`<origin>/v1`).
    pub fn base_url(&self) -> String {
        format!("{}/v1", self.origin)
    }

    /// `http://127.0.0.1:<port>`.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn requests(&self) -> Vec<RawRequest> {
        self.requests
            .lock()
            .map(|log| log.clone())
            .unwrap_or_default()
    }
}

impl Drop for RawProvider {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Names of the tool calls made since the last user message (this turn).
pub fn turn_tool_calls(request: &ModelRequest) -> Vec<String> {
    let messages = request.messages();
    let start = messages
        .iter()
        .rposition(|message| message["role"] == "user")
        .map_or(0, |index| index + 1);
    messages[start..]
        .iter()
        .filter_map(|message| message["tool_calls"].as_array())
        .flatten()
        .filter_map(|call| call["function"]["name"].as_str().map(str::to_owned))
        .collect()
}
