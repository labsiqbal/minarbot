//! Programmable loopback HTTP endpoint for peer tests.
//!
//! A [`Relay`] accepts one request per connection and hands it to an async
//! handler as an [`Exchange`]. The handler can answer itself (a synthetic
//! remote machine or CDN), forward to a live server with
//! [`Exchange::forward`] and pass the reply through, rewrite it, stream it
//! slowly, cut it off mid-body, or drop the connection without answering.
//! This replaces the ad hoc `node:http` proxies of the Node peer suites
//! (throttled links, lost replies, origin rewriting, port takeover).

use crate::Result;
pub use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use serde_json::Value;
use std::{
    future::Future,
    net::Ipv4Addr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinSet,
};

pub use bytes::Bytes;
/// HTTP client type of [`crate::Server::client`], for tests without a
/// direct `reqwest` dependency.
pub use reqwest::Client as HttpClient;

/// Redirect policy that never follows, for [`HttpClient`] builders.
pub fn redirect_none() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::none()
}

/// One received request.
#[derive(Clone, Debug)]
pub struct Exchange {
    pub method: String,
    /// Path and query, for example `/api/environment`.
    pub path: String,
    /// Request headers in arrival order, names lowercased.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Exchange {
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    }

    /// Request body as JSON (`Null` when empty or invalid).
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// Send the same request to `origin` (for example a restarted server)
    /// and return the reply headers with a streaming body. `Host` follows
    /// the target, as the Node proxies set it.
    pub async fn forward(&self, origin: &str) -> Result<Upstream> {
        let method = reqwest::Method::from_bytes(self.method.as_bytes())?;
        let mut request = client()?.request(method, format!("{origin}{}", self.path));
        for (name, value) in &self.headers {
            if !matches!(
                name.as_str(),
                "host" | "connection" | "content-length" | "transfer-encoding" | "keep-alive"
            ) {
                request = request.header(name, value);
            }
        }
        if !self.body.is_empty() {
            request = request.body(self.body.clone());
        }
        let response = request.send().await?;
        let status = response.status().as_u16();
        let mut headers = Vec::new();
        for (name, value) in response.headers() {
            headers.push((name.as_str().to_owned(), value.to_str()?.to_owned()));
        }
        Ok(Upstream {
            status,
            headers,
            response,
        })
    }
}

fn client() -> Result<reqwest::Client> {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client.clone());
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .pool_max_idle_per_host(0)
        .timeout(Duration::from_secs(600))
        .build()?;
    Ok(CLIENT.get_or_init(|| client).clone())
}

/// Reply from a forwarded request; the body has not been read yet.
pub struct Upstream {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    response: reqwest::Response,
}

impl Upstream {
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    }

    /// Read the whole body.
    pub async fn bytes(self) -> Result<Vec<u8>> {
        Ok(self.response.bytes().await?.to_vec())
    }

    /// Body as a stream of chunks.
    pub fn stream(self) -> BoxStream<'static, Result<Bytes>> {
        futures_util::stream::unfold(Some(self.response), |response| async move {
            let mut response = response?;
            match response.chunk().await {
                Ok(Some(chunk)) => Some((Ok(chunk), Some(response))),
                Ok(None) => None,
                Err(error) => Some((Err(error.into()), None)),
            }
        })
        .boxed()
    }

    /// Pass the reply through unchanged (status, headers, streamed body).
    pub fn relay(self) -> Answer {
        let status = self.status;
        let headers = self.headers.clone();
        Answer::stream(status, headers, self.stream())
    }
}

/// What the relay does with a request.
pub enum Answer {
    /// Close the connection without writing anything: a lost reply.
    Drop,
    /// Write this status, headers and body. A body chunk that is an error
    /// cuts the connection at that point (a reply lost mid-body).
    Reply {
        status: u16,
        headers: Vec<(String, String)>,
        body: BoxStream<'static, Result<Bytes>>,
    },
}

impl Answer {
    /// Complete reply with a fixed body.
    pub fn bytes(status: u16, headers: Vec<(String, String)>, body: impl Into<Vec<u8>>) -> Self {
        let body = Bytes::from(body.into());
        let mut headers = without_length(headers);
        headers.push(("content-length".into(), body.len().to_string()));
        Self::Reply {
            status,
            headers,
            body: futures_util::stream::iter([Ok(body)]).boxed(),
        }
    }

    /// JSON reply.
    pub fn json(status: u16, value: &Value) -> Self {
        Self::bytes(
            status,
            vec![("content-type".into(), "application/json".into())],
            value.to_string(),
        )
    }

    /// Empty reply with headers (for example a redirect).
    pub fn empty(status: u16, headers: Vec<(String, String)>) -> Self {
        Self::bytes(status, headers, Vec::new())
    }

    /// Streamed reply; `headers` are written as given, so a `content-length`
    /// the body does not honour is possible on purpose.
    pub fn stream(
        status: u16,
        headers: Vec<(String, String)>,
        body: BoxStream<'static, Result<Bytes>>,
    ) -> Self {
        Self::Reply {
            status,
            headers,
            body,
        }
    }
}

/// Headers without `content-length` and hop-by-hop fields.
pub fn without_length(headers: Vec<(String, String)>) -> Vec<(String, String)> {
    headers
        .into_iter()
        .filter(|(name, _)| {
            !matches!(
                name.to_ascii_lowercase().as_str(),
                "content-length" | "transfer-encoding" | "connection" | "keep-alive"
            )
        })
        .collect()
}

/// A body that yields `bytes` and then fails, cutting the connection.
pub fn cut_after(bytes: impl Into<Bytes>) -> BoxStream<'static, Result<Bytes>> {
    let bytes = bytes.into();
    futures_util::stream::iter([Ok(bytes), Err("cut by relay".into())]).boxed()
}

/// A body that yields `chunks` with `gap` before each one (a slow link).
pub fn paced(chunks: Vec<Bytes>, gap: Duration) -> BoxStream<'static, Result<Bytes>> {
    futures_util::stream::iter(chunks)
        .then(move |chunk| async move {
            tokio::time::sleep(gap).await;
            Ok(chunk)
        })
        .boxed()
}

/// A body that never yields: the reply hangs until the client gives up.
pub fn hang() -> BoxStream<'static, Result<Bytes>> {
    futures_util::stream::pending().boxed()
}

type Handler =
    Arc<dyn Fn(Exchange) -> futures_util::future::BoxFuture<'static, Result<Answer>> + Send + Sync>;

/// Running relay; stops (and cuts open connections) on drop.
pub struct Relay {
    origin: String,
    port: u16,
    tasks: Arc<Mutex<JoinSet<()>>>,
}

impl Relay {
    /// Listen on a free loopback port.
    pub async fn start<F, Fut>(handler: F) -> Result<Self>
    where
        F: Fn(Exchange) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Answer>> + Send + 'static,
    {
        Self::bind(0, handler).await
    }

    /// Listen on `port` (for example the port a stopped server used, so a
    /// paired origin stays valid while the relay sits in front of it).
    pub async fn bind<F, Fut>(port: u16, handler: F) -> Result<Self>
    where
        F: Fn(Exchange) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Answer>> + Send + 'static,
    {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
        let port = listener.local_addr()?.port();
        let handler: Handler = Arc::new(move |exchange| Box::pin(handler(exchange)));
        let tasks = Arc::new(Mutex::new(JoinSet::new()));
        let shared = tasks.clone();
        let accept = async move {
            while let Ok((stream, _)) = listener.accept().await {
                let handler = handler.clone();
                if let Ok(mut set) = shared.lock() {
                    set.spawn(async move {
                        let _ = serve(stream, handler).await;
                    });
                }
            }
        };
        if let Ok(mut set) = tasks.lock() {
            set.spawn(accept);
        }
        Ok(Self {
            origin: format!("http://127.0.0.1:{port}"),
            port,
            tasks,
        })
    }

    /// `http://127.0.0.1:<port>`.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.abort_all();
        }
    }
}

/// Port of an `http://127.0.0.1:<port>` origin.
pub fn port(origin: &str) -> Result<u16> {
    Ok(origin
        .rsplit(':')
        .next()
        .ok_or("origin without port")?
        .trim_end_matches('/')
        .parse()?)
}

fn header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

async fn read_more(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> Result<bool> {
    let mut chunk = [0_u8; 16384];
    let read = stream.read(&mut chunk).await?;
    buffer.extend_from_slice(&chunk[..read]);
    Ok(read > 0)
}

async fn chunked(stream: &mut TcpStream, mut buffer: Vec<u8>) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let line = loop {
            if let Some(end) = buffer.windows(2).position(|window| window == b"\r\n") {
                break end;
            }
            if !read_more(stream, &mut buffer).await? {
                return Err("chunked body ended early".into());
            }
        };
        let size = usize::from_str_radix(
            String::from_utf8_lossy(&buffer[..line])
                .split(';')
                .next()
                .unwrap_or("0")
                .trim(),
            16,
        )?;
        buffer.drain(..line + 2);
        while buffer.len() < size + 2 {
            if !read_more(stream, &mut buffer).await? {
                return Err("chunked body ended early".into());
            }
        }
        if size == 0 {
            return Ok(body);
        }
        body.extend_from_slice(&buffer[..size]);
        buffer.drain(..size + 2);
    }
}

async fn serve(mut stream: TcpStream, handler: Handler) -> Result<()> {
    let mut buffer = Vec::new();
    let head_end = loop {
        if let Some(end) = header_end(&buffer) {
            break end;
        }
        if !read_more(&mut stream, &mut buffer).await? {
            return Ok(());
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split(' ');
    let method = request_line.next().unwrap_or("GET").to_owned();
    let path = request_line.next().unwrap_or("/").to_owned();
    let mut headers = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    let rest = buffer[head_end + 4..].to_vec();
    let find = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value): &(String, String)| value.clone())
    };
    let body = if find("transfer-encoding").is_some_and(|value| value.contains("chunked")) {
        chunked(&mut stream, rest).await?
    } else {
        let length: usize = find("content-length")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let mut body = rest;
        while body.len() < length {
            if !read_more(&mut stream, &mut body).await? {
                return Ok(());
            }
        }
        body.truncate(length);
        body
    };
    let exchange = Exchange {
        method,
        path,
        headers,
        body,
    };
    let answer = match handler(exchange).await {
        Ok(answer) => answer,
        Err(_) => Answer::json(502, &serde_json::json!({"error": "relay handler failed"})),
    };
    let Answer::Reply {
        status,
        headers,
        mut body,
    } = answer
    else {
        // Lost reply: close without a status line.
        drop(stream);
        return Ok(());
    };
    let reason = reqwest::StatusCode::from_u16(status)
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or("Status");
    let mut head = format!("HTTP/1.1 {status} {reason}\r\n");
    for (name, value) in &headers {
        if !matches!(
            name.to_ascii_lowercase().as_str(),
            "connection" | "transfer-encoding" | "keep-alive"
        ) {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    head.push_str("connection: close\r\n\r\n");
    stream.write_all(head.as_bytes()).await?;
    stream.flush().await?;
    while let Some(chunk) = body.next().await {
        match chunk {
            Ok(bytes) => {
                if stream.write_all(&bytes).await.is_err() {
                    return Ok(());
                }
                let _ = stream.flush().await;
            }
            Err(_) => {
                // Cut mid-body: close before the declared end.
                drop(stream);
                return Ok(());
            }
        }
    }
    let _ = stream.shutdown().await;
    Ok(())
}
