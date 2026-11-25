//! Scriptable loopback HTTP/1.1 server for third-party API fakes (Composio,
//! Slack, Telegram, external MCP).
//!
//! A [`Stub`] records every request and asks a closure for each answer. A
//! [`StubReply`] can be JSON, raw bytes with headers, a held-open request
//! ([`StubReply::Hang`]) or a dropped connection ([`StubReply::Drop`]), which
//! is how tests produce "the effect was accepted but the result was lost".
//! Each connection serves one request (`Connection: close`). Handlers may
//! be async ([`Stub::start_async`], for example to forward to a server), and
//! a stub can accept WebSocket upgrades ([`Stub::with_socket`], the Slack
//! Socket Mode fake).

use crate::Result;
use serde_json::Value;
use futures_util::{SinkExt, StreamExt};
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio_tungstenite::{
    tungstenite::{handshake::derive_accept_key, protocol::Role, Message},
    WebSocketStream,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinHandle,
};

/// One recorded request.
#[derive(Clone, Debug)]
pub struct StubRequest {
    pub method: String,
    /// Path with query string, as sent.
    pub target: String,
    /// Lowercase header names.
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl StubRequest {
    /// Path without the query string.
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or_default()
    }

    /// Value of query parameter `name`.
    pub fn query(&self, name: &str) -> Option<String> {
        let query = self.target.split_once('?')?.1;
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (key == name).then(|| value.to_owned())
        })
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_ascii_lowercase()).map(String::as_str)
    }

    /// The body as JSON (`Null` when it is not JSON).
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// How the stub answers one request.
#[derive(Clone, Debug)]
pub enum StubReply {
    Json(u16, Value),
    Raw(u16, Vec<(String, String)>, Vec<u8>),
    /// Keep the request open until the stub stops.
    Hang,
    /// Close the connection without an answer.
    Drop,
}

impl StubReply {
    pub fn ok(value: Value) -> Self {
        Self::Json(200, value)
    }
}

type Reply = Pin<Box<dyn Future<Output = StubReply> + Send>>;
type Handler = Arc<dyn Fn(StubRequest) -> Reply + Send + Sync>;

/// One accepted WebSocket: text frames in, text frames out.
pub struct Socket {
    /// Zero-based connection number.
    pub index: usize,
    pub incoming: tokio::sync::mpsc::UnboundedReceiver<String>,
    pub outgoing: tokio::sync::mpsc::UnboundedSender<String>,
}

type SocketHandler = Arc<dyn Fn(Socket) + Send + Sync>;

/// A running stub; dropping it stops it and releases held requests.
pub struct Stub {
    origin: String,
    requests: Arc<Mutex<Vec<StubRequest>>>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Stub {
    pub async fn start(
        handler: impl Fn(&StubRequest) -> StubReply + Send + Sync + 'static,
    ) -> Result<Self> {
        Self::start_async(move |request| {
            let reply = handler(&request);
            async move { reply }
        })
        .await
    }

    /// Stub whose handler is async.
    pub async fn start_async<F, Fut>(handler: F) -> Result<Self>
    where
        F: Fn(StubRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = StubReply> + Send + 'static,
    {
        Self::launch(Arc::new(move |request| Box::pin(handler(request))), None).await
    }

    /// Stub that also accepts WebSocket upgrades; `socket` runs for each one
    /// (call it from a task: it must not block).
    pub async fn with_socket<F, Fut>(
        handler: F,
        socket: impl Fn(Socket) + Send + Sync + 'static,
    ) -> Result<Self>
    where
        F: Fn(StubRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = StubReply> + Send + 'static,
    {
        Self::launch(
            Arc::new(move |request| Box::pin(handler(request))),
            Some(Arc::new(socket)),
        )
        .await
    }

    async fn launch(handler: Handler, socket: Option<SocketHandler>) -> Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let requests: Arc<Mutex<Vec<StubRequest>>> = Arc::default();
        let (stop, stopping) = watch::channel(false);
        let log = requests.clone();
        let sockets = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (handler, log, stopping, socket, sockets) =
                    (handler.clone(), log.clone(), stopping.clone(), socket.clone(), sockets.clone());
                tokio::spawn(async move {
                    let _ = serve(stream, handler, log, stopping, socket, sockets).await;
                });
            }
        });
        Ok(Self {
            origin,
            requests,
            stop,
            task,
        })
    }

    /// `http://127.0.0.1:<port>`.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn requests(&self) -> Vec<StubRequest> {
        self.requests.lock().map(|log| log.clone()).unwrap_or_default()
    }

    /// Requests whose path ends with `suffix`.
    pub fn count(&self, suffix: &str) -> usize {
        self.requests()
            .iter()
            .filter(|request| request.path().ends_with(suffix))
            .count()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.task.abort();
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

async fn serve(
    mut stream: TcpStream,
    handler: Handler,
    log: Arc<Mutex<Vec<StubRequest>>>,
    mut stopping: watch::Receiver<bool>,
    socket: Option<SocketHandler>,
    sockets: Arc<std::sync::atomic::AtomicUsize>,
) -> Result<()> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let end = loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split(' ');
    let method = first.next().unwrap_or_default().to_owned();
    let target = first.next().unwrap_or_default().to_owned();
    let headers: HashMap<String, String> = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        })
        .collect();
    if let (Some(socket), Some(key)) = (
        &socket,
        headers
            .get("upgrade")
            .filter(|value| value.eq_ignore_ascii_case("websocket"))
            .and_then(|_| headers.get("sec-websocket-key")),
    ) {
        let accept = derive_accept_key(key.as_bytes());
        stream
            .write_all(
                format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n")
                    .as_bytes(),
            )
            .await?;
        let index = sockets.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        return bridge_socket(stream, index, socket.clone(), stopping).await;
    }
    let length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buffer[end + 4..].to_vec();
    if headers
        .get("transfer-encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
    {
        body = read_chunked(&mut stream, body).await?;
    } else {
        while body.len() < length {
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read]);
        }
    }
    let request = StubRequest {
        method,
        target,
        headers,
        body,
    };
    if let Ok(mut log) = log.lock() {
        log.push(request.clone());
    }
    let request = request;
    let (status, headers, bytes) = match handler(request).await {
        StubReply::Json(status, value) => (
            status,
            vec![("Content-Type".to_owned(), "application/json".to_owned())],
            value.to_string().into_bytes(),
        ),
        StubReply::Raw(status, headers, bytes) => (status, headers, bytes),
        StubReply::Hang => {
            let _ = stopping.wait_for(|stop| *stop).await.map(|_| ());
            return Ok(());
        }
        StubReply::Drop => return Ok(()),
    };
    let mut response = format!("HTTP/1.1 {status} {}\r\n", reason(status));
    for (name, value) in &headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()));
    stream.write_all(response.as_bytes()).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

/// Decode a chunked body that starts with `body`.
async fn read_chunked(stream: &mut TcpStream, mut raw: Vec<u8>) -> Result<Vec<u8>> {
    let mut chunk = [0u8; 8192];
    loop {
        if raw.windows(5).any(|window| window == b"0\r\n\r\n") {
            break;
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..read]);
    }
    let mut body = Vec::new();
    let mut rest = raw.as_slice();
    while let Some(line_end) = rest.windows(2).position(|window| window == b"\r\n") {
        let size = usize::from_str_radix(String::from_utf8_lossy(&rest[..line_end]).trim(), 16).unwrap_or(0);
        rest = &rest[line_end + 2..];
        if size == 0 || rest.len() < size {
            break;
        }
        body.extend_from_slice(&rest[..size]);
        rest = rest.get(size + 2..).unwrap_or_default();
    }
    Ok(body)
}

/// Pump one WebSocket between the network and a [`Socket`] handle.
async fn bridge_socket(
    stream: TcpStream,
    index: usize,
    handler: SocketHandler,
    mut stopping: watch::Receiver<bool>,
) -> Result<()> {
    let (incoming_sender, incoming) = tokio::sync::mpsc::unbounded_channel();
    let (outgoing, mut outgoing_receiver) = tokio::sync::mpsc::unbounded_channel::<String>();
    handler(Socket {
        index,
        incoming,
        outgoing: outgoing.clone(),
    });
    let socket = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
    let (mut sink, mut source) = socket.split();
    loop {
        tokio::select! {
            frame = source.next() => match frame {
                Some(Ok(Message::Text(text))) => { let _ = incoming_sender.send(text.to_string()); }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                    let _ = sink.send(Message::Close(None)).await;
                    return Ok(());
                }
                Some(Ok(_)) => {}
            },
            text = outgoing_receiver.recv() => match text {
                Some(text) => sink.send(Message::Text(text.into())).await?,
                None => return Ok(()),
            },
            () = stopped(&mut stopping) => return Ok(()),
        }
    }
}

async fn stopped(stopping: &mut watch::Receiver<bool>) {
    while !*stopping.borrow_and_update() {
        if stopping.changed().await.is_err() {
            return;
        }
    }
}
