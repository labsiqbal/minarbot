use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{unix::OwnedWriteHalf, UnixStream},
    process::Command,
    sync::mpsc,
    task::JoinHandle,
    time::{timeout, Instant},
};

const MAX_PACKET: u64 = 12 * 1024 * 1024;

pub(super) struct Events {
    pub frame: String,
    pub navigation: u64,
    pending: HashSet<String>,
    idle_since: Instant,
    overflow: bool,
}

impl Default for Events {
    fn default() -> Self {
        Self {
            frame: String::new(),
            navigation: 0,
            pending: HashSet::new(),
            idle_since: Instant::now(),
            overflow: false,
        }
    }
}

impl Events {
    pub fn idle(&self) -> bool {
        !self.overflow
            && self.pending.is_empty()
            && self.idle_since.elapsed() >= Duration::from_millis(500)
    }

    fn update(&mut self, event: &Value) {
        let p = &event["params"];
        match event["method"].as_str() {
            Some("Page.frameNavigated") if p["frame"].get("parentId").is_none() => {
                self.frame = p["frame"]["id"].as_str().unwrap_or_default().into();
                self.navigation += 1;
            }
            Some("Page.navigatedWithinDocument" | "Page.frameStartedNavigating")
                if p["frameId"] == self.frame =>
            {
                self.navigation += 1;
            }
            Some("Network.requestWillBeSent") => {
                if self.pending.len() >= 2048 {
                    self.overflow = true;
                } else if let Some(id) = p["requestId"].as_str() {
                    self.pending.insert(id.into());
                }
            }
            Some("Network.loadingFinished" | "Network.loadingFailed") => {
                if let Some(id) = p["requestId"].as_str() {
                    self.pending.remove(id);
                    if self.pending.is_empty() {
                        self.idle_since = Instant::now();
                    }
                }
            }
            _ => {}
        }
    }
}

#[derive(Debug)]
pub(crate) enum Error {
    Transport,
    Command,
    Rejected(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Browser command failed.")
    }
}

impl std::error::Error for Error {}

pub(crate) type Result<T> = std::result::Result<T, Error>;

pub(crate) struct Connection {
    send: OwnedWriteHalf,
    responses: mpsc::Receiver<Value>,
    events: Arc<Mutex<HashMap<String, Events>>>,
    reader: JoinHandle<()>,
    sequence: u64,
    broken: bool,
}

impl Connection {
    pub fn configure(command: &mut Command, binary: &str) -> std::io::Result<Self> {
        // Anonymous sockets stay private even where /proc is mounted.
        let (supervisor, browser) = std::os::unix::net::UnixStream::pair()?;
        supervisor.set_nonblocking(true)?;
        let stdin: std::os::fd::OwnedFd = browser.try_clone()?.into();
        let stdout: std::os::fd::OwnedFd = browser.into();
        command
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout))
            .args([
                "-c",
                r#"exec 3<&0 4>&1 </dev/null >/dev/null; exec "$@""#,
                "minarbot-chrome",
                binary,
                "--remote-debugging-pipe",
            ]);
        let (receive, send) = UnixStream::from_std(supervisor)?.into_split();
        let (deliver, responses) = mpsc::channel(4);
        let events = Arc::new(Mutex::new(HashMap::<String, Events>::new()));
        let state = events.clone();
        let reader = tokio::spawn(async move {
            let mut input = BufReader::new(receive);
            let mut packet = Vec::new();
            loop {
                packet.clear();
                let Ok(size) = (&mut input)
                    .take(MAX_PACKET + 1)
                    .read_until(0, &mut packet)
                    .await
                else {
                    break;
                };
                if size == 0 || size as u64 > MAX_PACKET || packet.pop() != Some(0) {
                    break;
                }
                let Ok(value) = serde_json::from_slice::<Value>(&packet) else {
                    break;
                };
                if value["id"].is_u64() {
                    if deliver.send(value).await.is_err() {
                        break;
                    }
                } else if let Some(session) = value["sessionId"].as_str() {
                    if let Ok(mut events) = state.lock() {
                        if let Some(frame) = events.get_mut(session) {
                            frame.update(&value);
                        }
                    }
                }
            }
        });
        Ok(Self {
            send,
            responses,
            events,
            reader,
            sequence: 0,
            broken: false,
        })
    }

    pub async fn call(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        if self.broken {
            return Err(Error::Transport);
        }
        self.sequence += 1;
        let id = self.sequence;
        let mut command = json!({"id":id, "method":method, "params":params});
        if let Some(session) = session {
            command["sessionId"] = json!(session);
        }
        let mut bytes = command.to_string().into_bytes();
        bytes.push(0);
        // A cancelled/partial write can corrupt framing. Never use that connection again.
        self.broken = true;
        timeout(Duration::from_secs(2), self.send.write_all(&bytes))
            .await
            .map_err(|_| Error::Transport)?
            .map_err(|_| Error::Transport)?;
        self.broken = false;
        // Late responses are retired by ID. A response timeout never repeats the command.
        let deadline = match method {
            "Page.navigate" => 8,
            "Target.getTargets" | "Page.setDocumentContent" => 5,
            _ => 2,
        };
        timeout(Duration::from_secs(deadline), async {
            loop {
                let reply = self.responses.recv().await.ok_or(Error::Transport)?;
                if reply["id"] == id {
                    return if reply.get("error").is_some() {
                        Err(Error::Command)
                    } else {
                        Ok(reply["result"].clone())
                    };
                }
            }
        })
        .await
        .unwrap_or(Err(Error::Transport))
    }

    pub fn watch(&self, session: &str) -> Result<()> {
        self.events
            .lock()
            .map_err(|_| Error::Transport)?
            .entry(session.into())
            .or_default();
        Ok(())
    }

    pub fn forget(&self, session: &str) {
        if let Ok(mut events) = self.events.lock() {
            events.remove(session);
        }
    }

    pub(super) fn events<T>(
        &self,
        session: &str,
        inspect: impl FnOnce(&mut Events) -> T,
    ) -> Result<T> {
        let mut events = self.events.lock().map_err(|_| Error::Transport)?;
        events.get_mut(session).map(inspect).ok_or(Error::Command)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.reader.abort();
    }
}
