use super::{control, error, Result};
use crate::{web::Failure, AppState};
use axum::{
    extract::{
        ws::{CloseFrame, Message, WebSocketUpgrade},
        FromRequestParts, Request,
    },
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::File,
    os::fd::AsRawFd,
    sync::{atomic::Ordering, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
const PROTOCOL: &str = "minarbot-vnc-v1";
const SAMPLE_BYTES: usize = 16 * 1024;
const WRITE_TIMEOUT: Duration = Duration::from_secs(15);
fn desktop_stopped() -> Failure {
    Failure::new(409, "desktop_stopped", "Computer is not running.")
}
fn viewer_limit() -> Failure {
    Failure::new(429, "viewer_limit", "Too many computer viewers.")
}
fn unavailable() -> Failure {
    error(503, "Desktop VNC socket is unavailable or unsafe.")
}
#[derive(Clone)]
pub(super) struct Ticket {
    agent: String,
    mode: String,
    revision: i64,
    origin: String,
    expires: i64,
}
#[derive(Default)]
pub struct Viewers {
    tickets: Mutex<HashMap<String, Ticket>>,
    active: Mutex<HashMap<String, usize>>,
}
impl Viewers {
    pub fn revoke(&self, agent: &str) -> Result<()> {
        self.tickets
            .lock()
            .map_err(|_| Failure::storage())?
            .retain(|_, v| v.agent != agent);
        Ok(())
    }
    pub(super) fn consume(
        &self,
        token: &str,
        agent: &str,
        origin: &str,
        now: i64,
    ) -> Result<Ticket> {
        let denied = || error(401, "Computer viewer ticket is invalid or expired.");
        let ticket = self
            .tickets
            .lock()
            .map_err(|_| Failure::storage())?
            .remove(token)
            .ok_or_else(denied)?;
        if ticket.agent != agent || ticket.origin != origin || ticket.expires <= now {
            return Err(denied());
        }
        Ok(ticket)
    }
    pub fn mint(&self, s: &AppState, agent: &str, origin: &str, v: &Value) -> Result<Value> {
        control::assert_enabled(s, agent)?;
        if !crate::files::keys(v, &["mode", "revision"])
            || !matches!(v["mode"].as_str(), Some("watch" | "control"))
            || v["revision"].as_i64().is_none()
        {
            return Err(error(400, "Invalid computer viewer mode."));
        }
        let current = control::read(s, agent)?;
        if current["control"]["revision"] != v["revision"] {
            return Err(error(409, "Control changed. Load latest status."));
        }
        if v["mode"] == "control" && current["control"]["mode"] != "user" {
            return Err(error(409, "Take control before sending computer input."));
        }
        if !matches!(current["state"].as_str(), Some("running" | "idle")) {
            return Err(desktop_stopped());
        }
        let mut tickets = self.tickets.lock().map_err(|_| Failure::storage())?;
        tickets.retain(|_, t| t.expires > s.config.now());
        if tickets.len() >= 1024 || tickets.values().filter(|t| t.agent == agent).count() >= 12 {
            return Err(error(429, "Too many pending viewer tickets."));
        }
        if self
            .active
            .lock()
            .map_err(|_| Failure::storage())?
            .get(agent)
            .copied()
            .unwrap_or(0)
            >= 3
        {
            return Err(viewer_limit());
        }
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let expires = s.config.now() + 30000;
        tickets.insert(
            token.clone(),
            Ticket {
                agent: agent.into(),
                origin: origin.into(),
                mode: v["mode"].as_str().unwrap_or("watch").into(),
                revision: v["revision"].as_i64().unwrap_or(0),
                expires,
            },
        );
        Ok(json!({"ticket":token,"expiresAt":expires}))
    }
}
#[cfg(target_os = "linux")]
fn socket(s: &AppState, agent: &str) -> Result<File> {
    use rustix::fs::{open, openat, Mode, OFlags};
    use std::os::unix::fs::FileTypeExt;
    let directory = open(
        s.computer.root.join(agent).join("x11"),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|_| unavailable())?;
    let fd = openat(
        &directory,
        "vnc.sock",
        OFlags::PATH | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|_| unavailable())?;
    let file = File::from(fd);
    if !file
        .metadata()
        .map_err(|_| Failure::storage())?
        .file_type()
        .is_socket()
    {
        return Err(unavailable());
    }
    Ok(file)
}
#[cfg(target_os = "macos")]
fn socket(_: &AppState, _: &str) -> Result<File> {
    Err(error(503, crate::platform::COMPUTER_UNAVAILABLE))
}
pub async fn upgrade(s: AppState, request: Request, agent: String) -> Result<Response> {
    let pairing = request
        .extensions()
        .get::<crate::auth::pairing::BrowserAccess>()
        .cloned();
    let q = crate::files::query(&request);
    let token = crate::files::parameter(&q, "ticket").ok_or_else(|| error(403, "Forbidden"))?;
    let origin = crate::text_header(request.headers(), "origin")
        .unwrap_or("")
        .to_owned();
    if q.len() != 1
        || !crate::allowed(&s, Some(&origin))
        || request.headers().contains_key("authorization")
    {
        return Err(error(403, "Forbidden"));
    }
    control::assert_enabled(&s, &agent)?;
    let (mut parts, _) = request.into_parts();
    let upgrade = WebSocketUpgrade::from_request_parts(&mut parts, &s)
        .await
        .map_err(|_| error(400, "Invalid WebSocket upgrade."))?
        .protocols([PROTOCOL, "binary"]);
    let measured = upgrade
        .selected_protocol()
        .is_some_and(|value| value == PROTOCOL);
    let ticket = s
        .computer
        .viewers
        .consume(token, &agent, &origin, s.config.now())?;
    let connection = async {
        let desktop = s.computer.get(&agent)?.ok_or_else(desktop_stopped)?;
        let revoked = desktop.disconnected.subscribe();
        if desktop.stopped.load(Ordering::SeqCst) {
            return Err(desktop_stopped());
        }
        let file = socket(&s, &agent)?;
        let backend =
            tokio::net::UnixStream::connect(format!("/proc/self/fd/{}", file.as_raw_fd()))
                .await
                .map_err(|_| unavailable())?;
        let mut active = s
            .computer
            .viewers
            .active
            .lock()
            .map_err(|_| Failure::storage())?;
        let count = active.entry(agent.clone()).or_default();
        if *count >= 3 {
            return Err(viewer_limit());
        }
        *count += 1;
        Ok((desktop, revoked, backend))
    };
    let (desktop, mut revoked, backend) = match connection.await {
        Ok(connection) => connection,
        Err(error) if measured => {
            let code = match error.code {
                "viewer_limit" => 4408,
                "desktop_stopped" => 4409,
                _ => 1011,
            };
            return Ok(upgrade
                .on_upgrade(move |mut ws| async move {
                    let _ = ws
                        .send(Message::Close(Some(CloseFrame {
                            code,
                            reason: "".into(),
                        })))
                        .await;
                })
                .into_response());
        }
        Err(error) => return Err(error),
    };
    let response = upgrade.max_message_size(262152).max_frame_size(262152)
        .on_upgrade(move |mut ws| async move {
            let (mut reader, mut writer) = backend.into_split();
            let mut filter = Filter::default();
            let mut buffer = vec![0; SAMPLE_BYTES];
            let mut heartbeat = tokio::time::interval(Duration::from_secs(30));
            let mut received = tokio::time::Instant::now();
            let signed_in = async {
                if let Some(pairing) = pairing { pairing.wait_until_denied(&s).await; }
                else { std::future::pending::<()>().await; }
            };
            tokio::pin!(signed_in);
            let sample_sent = !measured || matches!(
                tokio::time::timeout(WRITE_TIMEOUT, ws.send(Message::Binary(buffer.clone().into()))).await,
                Ok(Ok(()))
            );
            let code = if sample_sent {
                loop {
                    tokio::select! {
                        _ = &mut signed_in => break 4401,
                        _ = revoked.changed() => {
                            break if desktop.stopped.load(Ordering::SeqCst) { 4409 } else { 1000 };
                        }
                        _ = heartbeat.tick() => {
                            if received.elapsed() > Duration::from_secs(90) { break 4410; }
                            if ws.send(Message::Ping(Vec::new().into())).await.is_err() { break 1011; }
                        }
                        data = reader.read(&mut buffer) => {
                            let Ok(n) = data else { break 1011; };
                            if n == 0 { break 1011; }
                            match tokio::time::timeout(WRITE_TIMEOUT, ws.send(Message::Binary(buffer[..n].to_vec().into()))).await {
                                Ok(Ok(())) => {}
                                Ok(Err(_)) => break 1011,
                                Err(_) => break 4410,
                            }
                        }
                        message = ws.recv() => {
                            received = tokio::time::Instant::now();
                            match message {
                                Some(Ok(Message::Binary(bytes))) => {
                                    let can_input = ticket.mode == "control" && control::read(&s, &agent)
                                        .is_ok_and(|v| v["enabled"] == true && v["control"]["mode"] == "user" && v["control"]["revision"] == ticket.revision);
                                    let Ok((output, input)) = filter.feed(&bytes, can_input) else { break 1002; };
                                    if input { desktop.activity.store(s.config.now(), Ordering::SeqCst); }
                                    if !output.is_empty() && !matches!(tokio::time::timeout(Duration::from_secs(1), writer.write_all(&output)).await, Ok(Ok(()))) { break 1011; }
                                }
                                Some(Ok(Message::Ping(value))) => {
                                    if ws.send(Message::Pong(value)).await.is_err() { break 1011; }
                                }
                                Some(Ok(Message::Pong(_))) => {}
                                _ => break 1000,
                            }
                        }
                    }
                }
            } else {
                4410
            };
            let _ = tokio::time::timeout(Duration::from_secs(1), writer.write_all(&filter.release())).await;
            let _ = writer.shutdown().await;
            let close = Message::Close(Some(CloseFrame { code, reason: "".into() }));
            let _ = tokio::time::timeout(Duration::from_secs(1), ws.send(close)).await;
            if let Ok(mut active) = s.computer.viewers.active.lock() {
                if let Some(count) = active.get_mut(&agent) {
                    *count = count.saturating_sub(1);
                    if *count == 0 { active.remove(&agent); }
                }
            }
        });
    Ok(response.into_response())
}
#[derive(Default)]
struct Filter {
    pending: Vec<u8>,
    handshake: u8,
    keys: HashMap<(u8, u32), Vec<u8>>,
    pointer: Option<Vec<u8>>,
}
impl Filter {
    fn feed(&mut self, bytes: &[u8], can_input: bool) -> Result<(Vec<u8>, bool)> {
        if self.pending.len() + bytes.len() > 262144 + 65536 {
            return Err(error(400, "RFB input buffer exceeded."));
        }
        self.pending.extend_from_slice(bytes);
        let mut output = Vec::new();
        let mut input = false;
        loop {
            let b = &self.pending;
            if b.is_empty() {
                break;
            }
            let n = if self.handshake == 0 {
                12
            } else if self.handshake < 3 {
                1
            } else {
                match b[0] {
                    0 => 20,
                    2 => {
                        if b.len() < 4 {
                            0
                        } else {
                            4 + 4 * u16::from_be_bytes([b[2], b[3]]) as usize
                        }
                    }
                    3 => 10,
                    4 => 8,
                    5 => 6,
                    6 => {
                        if b.len() < 8 {
                            0
                        } else {
                            8 + i32::from_be_bytes([b[4], b[5], b[6], b[7]]).unsigned_abs() as usize
                        }
                    }
                    150 => 10,
                    248 => {
                        if b.len() < 9 {
                            0
                        } else {
                            9 + b[8] as usize
                        }
                    }
                    251 => {
                        if b.len() < 8 {
                            0
                        } else {
                            8 + 16 * b[6] as usize
                        }
                    }
                    255 => {
                        if b.len() > 1 && b[1] != 0 {
                            return Err(error(400, "Unsupported RFB message."));
                        }
                        12
                    }
                    _ => return Err(error(400, "Unsupported RFB message.")),
                }
            };
            if n > 262152 {
                return Err(error(400, "Oversized RFB message."));
            }
            if n == 0 || b.len() < n {
                break;
            }
            let mut message = self.pending.drain(..n).collect::<Vec<_>>();
            if self.handshake < 3 {
                if self.handshake == 0 && message != b"RFB 003.008\n"
                    || self.handshake > 0 && message[0] != 1
                {
                    return Err(error(400, "Unsupported RFB handshake."));
                }
                self.handshake += 1;
                output.extend(message);
                continue;
            }
            let kind = message[0];
            if kind == 251 {
                continue;
            }
            if kind == 2 {
                limit_continuous_updates(&mut message);
            }
            if [4, 5, 6, 255].contains(&kind) {
                if !can_input {
                    continue;
                }
                input = true;
                if kind == 4 || kind == 255 {
                    let offset = if kind == 4 { 4 } else { 8 };
                    let id = u32::from_be_bytes(
                        message[offset..offset + 4]
                            .try_into()
                            .map_err(|_| Failure::storage())?,
                    );
                    let down = if kind == 4 {
                        message[1] as u16
                    } else {
                        u16::from_be_bytes([message[2], message[3]])
                    };
                    if down != 0 {
                        if self.keys.len() >= 256 {
                            return Err(error(400, "Too many held keys."));
                        }
                        self.keys.insert((kind, id), message.clone());
                    } else {
                        self.keys.remove(&(kind, id));
                    }
                } else if kind == 5 {
                    self.pointer = Some(message.clone());
                }
            }
            output.extend(message);
        }
        Ok((output, input))
    }
    fn release(&mut self) -> Vec<u8> {
        let mut output = Vec::new();
        for (_, mut key) in self.keys.drain() {
            if key[0] == 4 {
                key[1] = 0;
            } else {
                key[2] = 0;
                key[3] = 0;
            }
            output.extend(key);
        }
        if let Some(mut pointer) = self.pointer.take() {
            pointer[1] = 0;
            output.extend(pointer);
        }
        output
    }
}
fn limit_continuous_updates(message: &mut Vec<u8>) {
    let encodings = message[4..].chunks_exact(4);
    if !encodings.clone().any(|code| code == (-32i32).to_be_bytes()) {
        return;
    }
    // Low-quality Tight uses one outstanding framebuffer request at a time.
    let retained = encodings
        .filter(|code| *code != (-313i32).to_be_bytes())
        .flatten()
        .copied()
        .collect::<Vec<_>>();
    message.truncate(4);
    message[2..4].copy_from_slice(&((retained.len() / 4) as u16).to_be_bytes());
    message.extend(retained);
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slow_encodings_keep_tight_and_disable_continuous_updates() -> Result<()> {
        let mut filter = Filter::default();
        filter.feed(b"RFB 003.008\n\x01\x01", false)?;
        let encodings = |quality: i32| {
            [
                vec![2, 0, 0, 4],
                [7, quality, -250, -313].map(i32::to_be_bytes).concat(),
            ]
            .concat()
        };
        let fast = encodings(-26);
        assert_eq!(filter.feed(&fast, false)?, (fast, false));
        let slow = encodings(-32);
        assert!(filter.feed(&slow[..7], false)?.0.is_empty());
        let mut expected = slow[..16].to_vec();
        expected[3] = 3;
        assert_eq!(filter.feed(&slow[7..], false)?, (expected, false));
        Ok(())
    }

    #[test]
    fn fragmented_handshake_and_live_input_fence() -> Result<()> {
        let mut f = Filter::default();
        assert!(f.feed(b"RFB 003.", false)?.0.is_empty());
        assert_eq!(f.feed(b"008\n\x01\x01", false)?.0, b"RFB 003.008\n\x01\x01");
        let key = [4, 1, 0, 0, 0, 0, 0, 97];
        let pointer = [5, 1, 0, 4, 0, 8];
        let clipboard = [6, 0, 0, 0, 0, 0, 0, 3, 97, 98, 99];
        let extended = [255, 0, 0, 1, 0, 0, 0, 97, 0, 0, 0, 30];
        let request = [3, 0, 0, 0, 0, 0, 5, 0, 3, 32];
        let mut resize = [0; 24];
        resize[0] = 251;
        resize[6] = 1;
        let frames: [&[u8]; 6] = [&key, &pointer, &clipboard, &extended, &resize, &request];
        let fragmented = frames.concat();
        let mut forwarded = Vec::new();
        for byte in fragmented {
            let (output, input) = f.feed(&[byte], false)?;
            assert!(!input);
            forwarded.extend(output);
        }
        assert_eq!(forwarded, request);
        let expected = [&key[..], &pointer, &clipboard, &extended, &request].concat();
        assert_eq!(f.feed(&frames.concat(), true)?, (expected, true));
        for frame in [&key[..], &pointer, &clipboard, &extended] {
            assert!(f.feed(frame, true)?.1);
        }
        assert_eq!(f.feed(&key, false)?, (vec![], false));
        let released = f.release();
        assert!(released
            .windows(8)
            .any(|part| part == [4, 0, 0, 0, 0, 0, 0, 97]));
        assert!(released
            .windows(12)
            .any(|part| part == [255, 0, 0, 0, 0, 0, 0, 97, 0, 0, 0, 30]));
        assert!(released.ends_with(&[5, 0, 0, 4, 0, 8]));
        assert!(f.feed(&[99], true).is_err());
        let mut oversized = Filter::default();
        oversized.feed(b"RFB 003.008\n\x01\x01", true)?;
        let mut header = [6, 0, 0, 0, 0, 0, 0, 0];
        header[4..].copy_from_slice(&(-262145i32).to_be_bytes());
        assert!(oversized.feed(&header, true).is_err());
        assert!(Filter::default().feed(b"RFB 003.003\n", true).is_err());
        Ok(())
    }
}
