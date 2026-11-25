//! HTTP replies and Server-Sent Events.

use crate::{Error, Result, Server};
use reqwest::{header, Response};
pub use reqwest::{Client, Method};
use serde_json::Value;
use std::time::Duration;

/// Status and decoded body of one HTTP exchange. Non-JSON bodies become a
/// JSON string so assertions and error messages always have the payload.
#[derive(Clone, Debug)]
pub struct Reply {
    pub method: String,
    pub path: String,
    pub status: u16,
    pub body: Value,
}

impl Reply {
    pub async fn read(method: &Method, path: &str, response: Response) -> Result<Self> {
        let status = response.status().as_u16();
        let text = response.text().await?;
        let body = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        Ok(Self {
            method: method.to_string(),
            path: path.to_owned(),
            status,
            body,
        })
    }

    fn failure(&self, expected: &str) -> Error {
        format!(
            "{} {}: expected HTTP {expected}, got {}: {}",
            self.method, self.path, self.status, self.body
        )
        .into()
    }

    /// Body of a 2xx reply, or an error carrying status and body.
    pub fn ok(self) -> Result<Value> {
        if (200..300).contains(&self.status) {
            Ok(self.body)
        } else {
            Err(self.failure("2xx"))
        }
    }

    /// Body when the status is exactly `status`.
    pub fn expect(self, status: u16) -> Result<Value> {
        if self.status == status {
            Ok(self.body)
        } else {
            Err(self.failure(&status.to_string()))
        }
    }
}

/// Reader for `text/event-stream` responses. Each `next` returns the JSON of
/// one `data:` frame; comment and keep-alive frames are skipped.
pub struct SseReader {
    response: Response,
    pending: Vec<u8>,
}

impl SseReader {
    pub async fn connect(server: &Server, path: &str) -> Result<Self> {
        let response = server
            .request(Method::GET, path)?
            .header(header::ACCEPT, "text/event-stream")
            .timeout(Duration::from_secs(3600))
            .send()
            .await?;
        if response.status().as_u16() != 200 {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(format!("GET {path}: expected event stream, got {status}: {body}").into());
        }
        Ok(Self {
            response,
            pending: Vec::new(),
        })
    }

    /// Next JSON frame, or `None` when the stream ends.
    pub async fn next(&mut self) -> Result<Option<Value>> {
        loop {
            if let Some(index) = self.pending.windows(2).position(|pair| pair == b"\n\n") {
                let frame: Vec<u8> = self.pending.drain(..index + 2).collect();
                let text = String::from_utf8(frame)?;
                let data = text
                    .lines()
                    .filter_map(|line| {
                        line.strip_prefix("data: ")
                            .or_else(|| line.strip_prefix("data:"))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if data.is_empty() {
                    continue;
                }
                return Ok(Some(serde_json::from_str(&data)?));
            }
            match self.response.chunk().await? {
                Some(chunk) => self.pending.extend_from_slice(&chunk),
                None => return Ok(None),
            }
        }
    }

    /// Next frame within `limit`.
    pub async fn next_within(&mut self, limit: Duration) -> Result<Value> {
        tokio::time::timeout(limit, self.next())
            .await
            .map_err(|_| -> Error { format!("no event within {limit:?}").into() })??
            .ok_or_else(|| "event stream ended".into())
    }
}

/// An HTTP/1.1 request whose last body bytes are held back, so a test can
/// change server state between "request started" and "request finished".
pub struct SplitRequest {
    stream: tokio::net::TcpStream,
    rest: Vec<u8>,
}

impl SplitRequest {
    /// Send `method path` with `headers` and all but the last `held_back`
    /// bytes of `body` to `origin` (`http://127.0.0.1:<port>`).
    pub async fn start(
        origin: &str,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
        held_back: usize,
    ) -> Result<Self> {
        use tokio::io::AsyncWriteExt;
        let address = origin
            .strip_prefix("http://")
            .ok_or("SplitRequest needs an http:// origin")?;
        let mut stream = tokio::net::TcpStream::connect(address).await?;
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        let split = body.len().saturating_sub(held_back);
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(&body[..split]).await?;
        stream.flush().await?;
        Ok(Self {
            stream,
            rest: body[split..].to_vec(),
        })
    }

    /// Send the held-back bytes and return the response status.
    pub async fn finish(mut self) -> Result<u16> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        self.stream.write_all(&self.rest).await?;
        self.stream.flush().await?;
        let mut response = Vec::new();
        self.stream.read_to_end(&mut response).await?;
        let text = String::from_utf8_lossy(&response);
        let status = text
            .split_whitespace()
            .nth(1)
            .and_then(|status| status.parse().ok())
            .ok_or_else(|| format!("no HTTP status in response: {text}"))?;
        Ok(status)
    }
}
