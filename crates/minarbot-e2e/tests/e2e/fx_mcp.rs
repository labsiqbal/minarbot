//! External MCP server that accepts an effect and loses its result (the Node
//! `uncertain-effect` fixture in `app/dispatch.test.ts`).

use minarbot_testkit::prelude::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::{net::TcpListener, sync::watch, task::JoinHandle};

pub struct LosingMcp {
    origin: String,
    effects: Arc<AtomicUsize>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl LosingMcp {
    pub async fn start() -> Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let effects = Arc::new(AtomicUsize::new(0));
        let (stop, stopping) = watch::channel(false);
        let counter = effects.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let (counter, stopping) = (counter.clone(), stopping.clone());
                tokio::spawn(async move {
                    let _ = serve(stream, counter, stopping).await;
                });
            }
        });
        Ok(Self {
            origin,
            effects,
            stop,
            task,
        })
    }

    pub fn url(&self) -> String {
        format!("{}/mcp", self.origin)
    }

    /// `tools/call` requests accepted so far.
    pub fn effects(&self) -> usize {
        self.effects.load(Ordering::SeqCst)
    }
}

impl Drop for LosingMcp {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.task.abort();
    }
}

/// Minimal HTTP/1.1 handling: one JSON-RPC request per connection.
async fn serve(
    mut stream: tokio::net::TcpStream,
    effects: Arc<AtomicUsize>,
    mut stopping: watch::Receiver<bool>,
) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buffer = Vec::new();
    let (head, body) = loop {
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            while buffer.len() < end + 4 + length {
                let read = stream.read(&mut chunk).await?;
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
            }
            break (head, buffer[end + 4..].to_vec());
        }
    };
    let reply = |status: &str, body: Option<Value>| {
        let text = body.map(|body| body.to_string()).unwrap_or_default();
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
            text.len()
        )
    };
    if head.starts_with("GET ") {
        stream
            .write_all(reply("405 Method Not Allowed", None).as_bytes())
            .await?;
        return Ok(());
    }
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if input.get("id").is_none() {
        stream
            .write_all(reply("202 Accepted", None).as_bytes())
            .await?;
        return Ok(());
    }
    let result = match input["method"].as_str() {
        Some("initialize") => {
            json!({"protocolVersion": "2025-03-26", "capabilities": {"tools": {}},
            "serverInfo": {"name": "uncertain-effect", "version": "1"}})
        }
        Some("tools/list") => {
            json!({"tools": [{"name": "effect", "inputSchema": {"type": "object"}}]})
        }
        _ => {
            // Effect accepted; deliberately lose the result.
            effects.fetch_add(1, Ordering::SeqCst);
            let _ = stopping.wait_for(|stop| *stop).await.map(|_| ());
            return Ok(());
        }
    };
    let response = json!({"jsonrpc": "2.0", "id": input["id"], "result": result});
    stream
        .write_all(reply("200 OK", Some(response)).as_bytes())
        .await?;
    Ok(())
}
