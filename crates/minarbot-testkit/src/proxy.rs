//! Loopback HTTP proxy that can lose replies.
//!
//! The proxy forwards each request (method, path, body, `Authorization` and
//! `Content-Type`) to the current target, records it, and then either
//! returns the upstream reply or, when a one-shot drop rule matches, closes
//! the connection without answering, which is what a lost reply looks like
//! to a client. The target can change after a server restart. Each
//! connection carries one request (`Connection: close`).

use crate::Result;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

/// One forwarded request and what happened to its reply.
#[derive(Clone, Debug)]
pub struct Proxied {
    pub method: String,
    /// Path and query, for example `/api/scripts/agents?after=x`.
    pub path: String,
    pub body: String,
    /// Upstream status, or `None` when the upstream could not be reached.
    pub status: Option<u16>,
    pub dropped: bool,
}

impl Proxied {
    pub fn ok(&self) -> bool {
        self.status
            .is_some_and(|status| (200..300).contains(&status))
    }
}

type Rule = Box<dyn Fn(&Proxied) -> bool + Send + Sync>;

struct Inner {
    target: String,
    offline: bool,
    log: Vec<Proxied>,
    rules: Vec<Rule>,
}

/// Running proxy; stops on drop.
pub struct LossyProxy {
    origin: String,
    inner: Arc<Mutex<Inner>>,
    task: JoinHandle<()>,
}

impl LossyProxy {
    /// Proxy to `target` (an origin such as `http://127.0.0.1:1234`).
    pub async fn start(target: &str) -> Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let inner = Arc::new(Mutex::new(Inner {
            target: target.to_owned(),
            offline: false,
            log: Vec::new(),
            rules: Vec::new(),
        }));
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(120))
            .build()?;
        let shared = inner.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let shared = shared.clone();
                let client = client.clone();
                tokio::spawn(async move {
                    let _ = serve(stream, shared, client).await;
                });
            }
        });
        Ok(Self {
            origin,
            inner,
            task,
        })
    }

    fn with<T>(&self, change: impl FnOnce(&mut Inner) -> T) -> T {
        match self.inner.lock() {
            Ok(mut inner) => change(&mut inner),
            Err(poisoned) => change(&mut poisoned.into_inner()),
        }
    }

    /// `http://127.0.0.1:<port>` of the proxy.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Forward to a new upstream origin (for example after a restart).
    pub fn set_target(&self, target: &str) {
        self.with(|inner| inner.target = target.to_owned());
    }

    /// While offline, every request is closed unanswered without reaching
    /// the upstream: a deterministic "server unreachable" window.
    pub fn set_offline(&self, offline: bool) {
        self.with(|inner| inner.offline = offline);
    }

    /// Lose the reply of the next request that matches `rule` (one-shot).
    pub fn drop_next(&self, rule: impl Fn(&Proxied) -> bool + Send + Sync + 'static) {
        self.with(|inner| inner.rules.push(Box::new(rule)));
    }

    /// Remove pending drop rules.
    pub fn clear_rules(&self) {
        self.with(|inner| inner.rules.clear());
    }

    /// Whether a drop rule is still pending.
    pub fn pending_rules(&self) -> usize {
        self.with(|inner| inner.rules.len())
    }

    /// Every request so far.
    pub fn requests(&self) -> Vec<Proxied> {
        self.with(|inner| inner.log.clone())
    }

    /// Forget recorded requests.
    pub fn clear_log(&self) {
        self.with(|inner| inner.log.clear());
    }
}

impl Drop for LossyProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

async fn serve(
    mut stream: TcpStream,
    inner: Arc<Mutex<Inner>>,
    client: reqwest::Client,
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
    let mut length = 0;
    let mut authorization = None;
    let mut content_type = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim().to_owned();
            match name.trim().to_ascii_lowercase().as_str() {
                "content-length" => length = value.parse().unwrap_or(0),
                "authorization" => authorization = Some(value),
                "content-type" => content_type = Some(value),
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
    let (target, offline) = match inner.lock() {
        Ok(inner) => (inner.target.clone(), inner.offline),
        Err(poisoned) => {
            let inner = poisoned.into_inner();
            (inner.target.clone(), inner.offline)
        }
    };
    if offline {
        if let Ok(mut inner) = inner.lock() {
            inner.log.push(Proxied {
                method,
                path,
                body: String::from_utf8_lossy(&body).into_owned(),
                status: None,
                dropped: true,
            });
        }
        let _ = stream.shutdown().await;
        return Ok(());
    }
    let method_value = reqwest::Method::from_bytes(method.as_bytes())?;
    let mut request = client.request(method_value, format!("{target}{path}"));
    if let Some(value) = &authorization {
        request = request.header("Authorization", value);
    }
    if !body.is_empty() || content_type.is_some() {
        request = request.header("Content-Type", "application/json");
    }
    if !body.is_empty() {
        request = request.body(body.clone());
    }
    let upstream = match request.send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            response.bytes().await.ok().map(|bytes| (status, bytes))
        }
        Err(_) => None,
    };
    let mut record = Proxied {
        method,
        path,
        body: String::from_utf8_lossy(&body).into_owned(),
        status: upstream.as_ref().map(|(status, _)| *status),
        dropped: false,
    };
    let drop = {
        let mut inner = match inner.lock() {
            Ok(inner) => inner,
            Err(poisoned) => poisoned.into_inner(),
        };
        let matched = inner.rules.iter().position(|rule| rule(&record));
        if let Some(index) = matched {
            drop(inner.rules.remove(index));
        }
        record.dropped = matched.is_some() || upstream.is_none();
        inner.log.push(record.clone());
        record.dropped
    };
    let Some((status, bytes)) = upstream.filter(|_| !drop) else {
        // Close without a reply: the client sees a lost response.
        let _ = stream.shutdown().await;
        return Ok(());
    };
    let reason = reqwest::StatusCode::from_u16(status)
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or("Status");
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&bytes).await?;
    stream.shutdown().await?;
    Ok(())
}
