//! Binary WebSocket + RFB 3.8 test client for the VNC relay (the former
//! `app/test-support/agent-desktop-test-client.ts`).

use crate::Result;
use base64::{engine::general_purpose::STANDARD, Engine};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{tcp::OwnedWriteHalf, TcpStream},
    sync::Notify,
};

#[derive(Default)]
struct Incoming {
    bytes: Vec<u8>,
    closed: bool,
}

pub struct VncClient {
    writer: tokio::sync::Mutex<Option<OwnedWriteHalf>>,
    incoming: Arc<(Mutex<Incoming>, Notify)>,
    pub width: u16,
    pub height: u16,
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    let mut filled = 0;
    while filled < N {
        for byte in uuid::Uuid::new_v4().as_bytes() {
            if filled < N {
                out[filled] = *byte;
                filled += 1;
            }
        }
    }
    out
}

fn masked_frame(data: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x82];
    let size = data.len();
    if size < 126 {
        frame.push(0x80 | u8::try_from(size).unwrap_or(125));
    } else if let Ok(size) = u16::try_from(size) {
        frame.push(0x80 | 126);
        frame.extend_from_slice(&size.to_be_bytes());
    } else {
        frame.push(0x80 | 127);
        frame.extend_from_slice(&(size as u64).to_be_bytes());
    }
    let mask: [u8; 4] = random_bytes();
    frame.extend_from_slice(&mask);
    frame.extend(
        data.iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % 4]),
    );
    frame
}

impl VncClient {
    /// Open the relay at `url` (`http://host:port/path?ticket=...`) with
    /// `Origin: origin` and `extra` headers (`Host` replaces the default).
    /// A refused upgrade fails with `WebSocket HTTP <status>`.
    pub async fn connect(url: &str, origin: &str, extra: &[(&str, &str)]) -> Result<Self> {
        let rest = url
            .strip_prefix("http://")
            .ok_or("VncClient needs an http:// URL")?;
        let (address, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        let stream = TcpStream::connect(address).await?;
        let key = STANDARD.encode(random_bytes::<16>());
        let host = extra
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("host"))
            .map_or(address, |(_, value)| *value);
        let mut head = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Protocol: binary\r\n"
        );
        for (name, value) in extra {
            if !name.eq_ignore_ascii_case("host") {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
        }
        head.push_str("\r\n");
        let (mut reader, mut writer) = stream.into_split();
        writer.write_all(head.as_bytes()).await?;
        let mut response = Vec::new();
        let mut chunk = [0u8; 4096];
        let body_start = loop {
            if let Some(index) = response.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
            let read = reader.read(&mut chunk).await?;
            if read == 0 {
                return Err("WebSocket connection closed during upgrade".into());
            }
            response.extend_from_slice(&chunk[..read]);
        };
        let status: u16 = String::from_utf8_lossy(&response[..body_start])
            .split_whitespace()
            .nth(1)
            .and_then(|status| status.parse().ok())
            .unwrap_or(0);
        if status != 101 {
            return Err(format!("WebSocket HTTP {status}").into());
        }
        let incoming: Arc<(Mutex<Incoming>, Notify)> = Arc::default();
        let feed = incoming.clone();
        let mut pending = response[body_start..].to_vec();
        tokio::spawn(async move {
            let mut chunk = [0u8; 65536];
            'frames: loop {
                // Parse every complete frame in `pending`.
                loop {
                    if pending.len() < 2 {
                        break;
                    }
                    let opcode = pending[0] & 15;
                    let mut length = usize::from(pending[1] & 127);
                    let mut offset = 2;
                    if length == 126 {
                        if pending.len() < 4 {
                            break;
                        }
                        length = usize::from(u16::from_be_bytes([pending[2], pending[3]]));
                        offset = 4;
                    } else if length == 127 {
                        if pending.len() < 10 {
                            break;
                        }
                        let mut size = [0u8; 8];
                        size.copy_from_slice(&pending[2..10]);
                        length = usize::try_from(u64::from_be_bytes(size)).unwrap_or(usize::MAX);
                        offset = 10;
                    }
                    if pending.len() < offset + length {
                        break;
                    }
                    let payload = pending[offset..offset + length].to_vec();
                    pending.drain(..offset + length);
                    if opcode == 2 {
                        if let Ok(mut state) = feed.0.lock() {
                            state.bytes.extend_from_slice(&payload);
                        }
                        feed.1.notify_waiters();
                    }
                    if opcode == 8 {
                        break 'frames;
                    }
                }
                match reader.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => pending.extend_from_slice(&chunk[..read]),
                }
            }
            if let Ok(mut state) = feed.0.lock() {
                state.closed = true;
            }
            feed.1.notify_waiters();
        });
        Ok(Self {
            writer: tokio::sync::Mutex::new(Some(writer)),
            incoming,
            width: 0,
            height: 0,
        })
    }

    /// Read exactly `size` relayed bytes (15 s limit).
    pub async fn read(&self, size: usize) -> Result<Vec<u8>> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let notified = self.incoming.1.notified();
            {
                let mut state = self
                    .incoming
                    .0
                    .lock()
                    .map_err(|_| "VNC client state poisoned")?;
                if state.bytes.len() >= size {
                    return Ok(state.bytes.drain(..size).collect());
                }
                if state.closed {
                    return Err("Test connection closed.".into());
                }
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Err("Test connection read timed out.".into());
            }
        }
    }

    pub async fn send(&self, bytes: &[u8]) -> Result<()> {
        let mut writer = self.writer.lock().await;
        let writer = writer.as_mut().ok_or("VNC client closed")?;
        writer.write_all(&masked_frame(bytes)).await?;
        Ok(())
    }

    /// RFB 3.8 handshake with no authentication and 32-bit raw pixels.
    pub async fn handshake(&mut self) -> Result<()> {
        let version = self.read(12).await?;
        if version != b"RFB 003.008\n" {
            return Err(format!("RFB version: {:?}", String::from_utf8_lossy(&version)).into());
        }
        self.send(b"RFB 003.008\n").await?;
        let count = usize::from(self.read(1).await?[0]);
        if !self.read(count).await?.contains(&1) {
            return Err("RFB server offers no None security".into());
        }
        self.send(&[1]).await?;
        if self.read(4).await? != [0, 0, 0, 0] {
            return Err("RFB security failed".into());
        }
        self.send(&[1]).await?;
        let init = self.read(24).await?;
        self.width = u16::from_be_bytes([init[0], init[1]]);
        self.height = u16::from_be_bytes([init[2], init[3]]);
        let name = u32::from_be_bytes([init[20], init[21], init[22], init[23]]);
        self.read(usize::try_from(name)?).await?;
        let mut pixel = [0u8; 20];
        pixel[4] = 32;
        pixel[5] = 24;
        pixel[7] = 1;
        pixel[8..10].copy_from_slice(&255u16.to_be_bytes());
        pixel[10..12].copy_from_slice(&255u16.to_be_bytes());
        pixel[12..14].copy_from_slice(&255u16.to_be_bytes());
        pixel[14] = 16;
        pixel[15] = 8;
        self.send(&pixel).await?;
        self.send(&[2, 0, 0, 1, 0, 0, 0, 0]).await
    }

    pub async fn key(&self, keysym: u32, down: bool) -> Result<()> {
        let mut message = vec![4, u8::from(down), 0, 0];
        message.extend_from_slice(&keysym.to_be_bytes());
        self.send(&message).await
    }

    pub async fn pointer(&self, x: u16, y: u16, buttons: u8) -> Result<()> {
        let mut message = vec![5, buttons];
        message.extend_from_slice(&x.to_be_bytes());
        message.extend_from_slice(&y.to_be_bytes());
        self.send(&message).await
    }

    pub fn closed(&self) -> bool {
        self.incoming.0.lock().map_or(true, |state| state.closed)
    }

    /// Wait up to 5 s for the relay to close the connection.
    pub async fn wait_closed(&self) -> Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let notified = self.incoming.1.notified();
            if self.closed() {
                return Ok(());
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Err("VNC connection did not close".into());
            }
        }
    }

    /// Drop the connection from the client side.
    pub async fn close(&self) {
        if let Some(mut writer) = self.writer.lock().await.take() {
            let _ = writer.shutdown().await;
        }
    }
}
