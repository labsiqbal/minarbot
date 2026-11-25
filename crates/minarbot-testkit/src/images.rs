//! Fake OpenAI-compatible image provider (`POST /v1/images/generations`)
//! for the server's `imageProvider` option.
//!
//! It runs on raw TCP so it can also lose a request (close the connection
//! without a reply). Every request is checked against the expected path,
//! bearer key and JSON body; mismatches are recorded as violations instead
//! of failing inside the handler, and tests assert there are none.

use crate::Result;
use base64::Engine;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::JoinHandle,
};

/// How the next requests are answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The PNG as `b64_json`, with usage 5/6/11.
    Ok,
    /// The PNG padded with zero bytes to exactly 4 MiB.
    Large,
    /// Close the connection without a reply.
    Lost,
    /// HTTP 302 to `/stolen`.
    Redirect,
    /// A `url` result instead of bytes.
    Url,
    /// `b64_json` that is not an image.
    Invalid,
    /// Hold the reply until [`ImageServer::release`].
    Late,
}

struct State {
    mode: Mode,
    calls: usize,
    violations: Vec<String>,
    late: Option<oneshot::Sender<()>>,
}

#[derive(Clone)]
struct Shared {
    state: Arc<Mutex<State>>,
    key: String,
    body: Value,
    png: Arc<Vec<u8>>,
}

impl Shared {
    fn with<T>(&self, change: impl FnOnce(&mut State) -> T) -> T {
        match self.state.lock() {
            Ok(mut state) => change(&mut state),
            Err(poisoned) => change(&mut poisoned.into_inner()),
        }
    }
}

pub struct ImageServer {
    origin: String,
    shared: Shared,
    task: JoinHandle<()>,
}

impl ImageServer {
    /// Serve `png`, expecting `Authorization: Bearer <key>` and exactly `body`.
    pub async fn start(key: &str, body: Value, png: Vec<u8>) -> Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let shared = Shared {
            state: Arc::new(Mutex::new(State {
                mode: Mode::Ok,
                calls: 0,
                violations: Vec::new(),
                late: None,
            })),
            key: key.to_owned(),
            body,
            png: Arc::new(png),
        };
        let serving = shared.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let shared = serving.clone();
                tokio::spawn(async move {
                    let _ = handle(shared, stream).await;
                });
            }
        });
        Ok(Self {
            origin,
            shared,
            task,
        })
    }

    /// `<origin>/v1`, the `imageProvider.baseURL`.
    pub fn base_url(&self) -> String {
        format!("{}/v1", self.origin)
    }

    pub fn set_mode(&self, mode: Mode) {
        self.shared.with(|state| state.mode = mode);
    }

    /// Requests received so far.
    pub fn calls(&self) -> usize {
        self.shared.with(|state| state.calls)
    }

    /// Whether a [`Mode::Late`] reply is waiting for [`ImageServer::release`].
    pub fn holding(&self) -> bool {
        self.shared.with(|state| state.late.is_some())
    }

    /// Send the held [`Mode::Late`] reply.
    pub fn release(&self) {
        if let Some(late) = self.shared.with(|state| state.late.take()) {
            let _ = late.send(());
        }
    }

    /// Contract mismatches seen so far (path, key or body).
    pub fn violations(&self) -> Vec<String> {
        self.shared.with(|state| state.violations.clone())
    }
}

impl Drop for ImageServer {
    fn drop(&mut self) {
        self.release();
        self.task.abort();
    }
}

async fn read_request(stream: &mut TcpStream) -> Result<(String, Vec<u8>)> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err("connection closed before the request head".into());
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let mut body = buffer[head_end..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Ok((head, body))
}

async fn handle(shared: Shared, mut stream: TcpStream) -> Result<()> {
    let (head, body) = read_request(&mut stream).await?;
    let mut violations = Vec::new();
    let line = head.lines().next().unwrap_or("");
    if line.split_whitespace().nth(1) != Some("/v1/images/generations") {
        violations.push(format!("request line: {line}"));
    }
    let authorization = head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("authorization")
            .then(|| value.trim().to_owned())
    });
    if authorization.as_deref() != Some(format!("Bearer {}", shared.key).as_str()) {
        violations.push(format!("authorization: {authorization:?}"));
    }
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if parsed != shared.body {
        violations.push(format!("body: {parsed}"));
    }
    let mode = shared.with(|state| {
        state.calls += 1;
        state.violations.extend(violations);
        state.mode
    });
    let response = match mode {
        Mode::Lost => return Ok(()),
        Mode::Redirect => {
            "HTTP/1.1 302 Found\r\nLocation: /stolen\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_owned()
                .into_bytes()
        }
        _ => {
            let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
            let data = match mode {
                Mode::Url => json!([{"url": "http://127.0.0.1/private"}]),
                Mode::Invalid => json!([{"b64_json": "AA=="}]),
                Mode::Large => {
                    let mut image = shared.png.to_vec();
                    image.resize(4 * 1024 * 1024, 0);
                    json!([{"b64_json": encode(&image)}])
                }
                _ => json!([{"b64_json": encode(&shared.png)}]),
            };
            let body = json!({"data": data,
                "usage": {"input_tokens": 5, "output_tokens": 6, "total_tokens": 11}})
            .to_string();
            if mode == Mode::Late {
                let (send, wait) = oneshot::channel();
                shared.with(|state| state.late = Some(send));
                let _ = wait.await;
            }
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .into_bytes()
        }
    };
    stream.write_all(&response).await?;
    stream.flush().await?;
    Ok(())
}
