//! Rust device hosts driven over their test interfaces.
//!
//! Port of `app/test-support/rust-device-host.ts`. A [`DeviceHost`] owns a
//! device database and keyring and talks to `minarbot-server --device-host
//! --test-control` over JSON lines (`{id, op, ...}` -> `{id, value | error}`,
//! plus `{phase}` connection and `{recovery}` events). Enrollment, the
//! standalone connection process, `run-task` (optionally stopping at a crash
//! checkpoint) and `export-recovery` run the same binary as the Node helpers
//! did. [`provider`] serves the action provider that host actions call.

use crate::{
    relay::{Answer, Relay},
    Result,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{oneshot, Mutex as AsyncMutex},
};

fn locked<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// A device host or server protocol error (`ProtocolError` in Node).
#[derive(Clone, Debug)]
pub struct HostError {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl HostError {
    fn from_value(value: &Value) -> Self {
        Self {
            status: value["status"].as_u64().unwrap_or(500) as u16,
            code: value["code"].as_str().unwrap_or_default().to_owned(),
            message: value["message"].as_str().unwrap_or_default().to_owned(),
        }
    }
}

impl std::fmt::Display for HostError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} {}: {}", self.status, self.code, self.message)
    }
}

impl std::error::Error for HostError {}

/// Whether `error` is a [`HostError`] with `code`.
pub fn is_code(error: &crate::Error, code: &str) -> bool {
    error
        .downcast_ref::<HostError>()
        .is_some_and(|error| error.code == code)
}

/// Where the host stores its data and which server it talks to.
#[derive(Clone, Debug)]
pub struct HostOptions {
    pub database: PathBuf,
    pub server: String,
    pub keyring: PathBuf,
    /// Extra environment for every host process (for example `TZ`).
    pub env: Vec<(String, String)>,
}

impl HostOptions {
    pub fn new(database: &Path, server: &str) -> Result<Self> {
        let mut keyring = database.as_os_str().to_owned();
        keyring.push(".keys.json");
        let keyring = PathBuf::from(keyring);
        if !keyring.exists() {
            std::fs::write(&keyring, "{}")?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&keyring, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self {
            database: database.to_owned(),
            server: server.to_owned(),
            keyring,
            env: Vec::new(),
        })
    }

    fn command(&self, subcommand: &[&str]) -> Result<Command> {
        let mut command = Command::new(crate::server::server_binary(None)?);
        command
            .arg("--device-host")
            .args(subcommand)
            .arg("--server")
            .arg(&self.server)
            .arg("--test-keyring")
            .arg(&self.keyring)
            .env_clear()
            .env("PATH", crate::server::HERMETIC_PATH)
            .env(
                "HOME",
                self.database.parent().unwrap_or_else(|| Path::new("/tmp")),
            )
            .env("MINARBOT_TEST_SERVER", "rust")
            .env("MINARBOT_DEVICE_DB", &self.database)
            .env("MINARBOT_DEVELOPMENT", "1")
            .kill_on_drop(true);
        for (name, value) in &self.env {
            command.env(name, value);
        }
        Ok(command)
    }

    /// Keyring entries (`service/name -> value`).
    pub fn keys(&self) -> Result<HashMap<String, String>> {
        Ok(serde_json::from_slice(&std::fs::read(&self.keyring)?)?)
    }

    /// Device id from `<db>.host/identity.json`.
    pub fn device_id(&self) -> Result<String> {
        let mut path = self.database.as_os_str().to_owned();
        path.push(".host/identity.json");
        let identity: Value = serde_json::from_slice(&std::fs::read(PathBuf::from(path))?)?;
        Ok(identity["deviceId"].as_str().unwrap_or_default().to_owned())
    }
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<std::result::Result<Value, HostError>>>>>;

struct Worker {
    stdin: AsyncMutex<Option<ChildStdin>>,
    child: AsyncMutex<Child>,
    pending: Pending,
    sequence: AtomicU64,
    exited: Arc<Mutex<Option<String>>>,
}

/// Test-control device host process plus its last connection state.
pub struct DeviceHost {
    pub options: HostOptions,
    worker: AsyncMutex<Option<Arc<Worker>>>,
    phase: Arc<Mutex<Value>>,
    recovery: Arc<Mutex<Value>>,
}

impl DeviceHost {
    pub fn new(options: HostOptions) -> Self {
        Self {
            options,
            worker: AsyncMutex::new(None),
            phase: Arc::new(Mutex::new(json!({"phase": "offline", "generation": 0}))),
            recovery: Arc::new(Mutex::new(json!([]))),
        }
    }

    async fn worker(&self) -> Result<Arc<Worker>> {
        let mut slot = self.worker.lock().await;
        if let Some(worker) = slot.as_ref() {
            return Ok(worker.clone());
        }
        let mut child = self
            .options
            .command(&["--test-control"])?
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or("device host stdout")?;
        let stderr = child.stderr.take().ok_or("device host stderr")?;
        let pending: Pending = Arc::default();
        let exited = Arc::new(Mutex::new(None::<String>));
        let tail = Arc::new(Mutex::new(String::new()));
        let stderr_tail = tail.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut tail = locked(&stderr_tail);
                tail.push_str(&line);
                tail.push('\n');
                let excess = tail.len().saturating_sub(8192);
                tail.drain(..excess);
            }
        });
        let (phase, recovery, waiting, gone) = (
            self.phase.clone(),
            self.recovery.clone(),
            pending.clone(),
            exited.clone(),
        );
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if message.get("phase").is_some() {
                    *locked(&phase) = message;
                    continue;
                }
                if let Some(results) = message.get("recovery") {
                    *locked(&recovery) = results.clone();
                    continue;
                }
                let Some(id) = message["id"].as_u64() else {
                    continue;
                };
                if let Some(sender) = locked(&waiting).remove(&id) {
                    let _ = sender.send(match message.get("error").filter(|e| !e.is_null()) {
                        Some(error) => Err(HostError::from_value(error)),
                        None => Ok(message["value"].clone()),
                    });
                }
            }
            let reason = {
                let tail = locked(&tail).trim().to_owned();
                if tail.is_empty() {
                    "Rust device fixture stopped.".to_owned()
                } else {
                    tail
                }
            };
            *locked(&gone) = Some(reason.clone());
            for (_, sender) in locked(&waiting).drain() {
                let _ = sender.send(Err(HostError {
                    status: 503,
                    code: "fixture_stopped".into(),
                    message: reason.clone(),
                }));
            }
        });
        let worker = Arc::new(Worker {
            stdin: AsyncMutex::new(stdin),
            child: AsyncMutex::new(child),
            pending,
            sequence: AtomicU64::new(0),
            exited,
        });
        *slot = Some(worker.clone());
        Ok(worker)
    }

    /// One control operation (`{id, op, ...args}`).
    pub async fn call(&self, op: &str, args: Value) -> Result<Value> {
        let worker = self.worker().await?;
        if let Some(reason) = locked(&worker.exited).clone() {
            return Err(reason.into());
        }
        let id = worker.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        let (sender, receiver) = oneshot::channel();
        locked(&worker.pending).insert(id, sender);
        let mut message = json!({"id": id, "op": op});
        if let (Some(fields), Some(extra)) = (message.as_object_mut(), args.as_object()) {
            for (key, value) in extra {
                fields.insert(key.clone(), value.clone());
            }
        }
        {
            let mut stdin = worker.stdin.lock().await;
            let stdin = stdin.as_mut().ok_or("device host stdin closed")?;
            stdin.write_all(format!("{message}\n").as_bytes()).await?;
            stdin.flush().await?;
        }
        match receiver.await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(error.into()),
            Err(_) => Err("device host dropped the request".into()),
        }
    }

    /// Enroll this host with a Clerk session `token`.
    pub async fn enroll(&self, token: &str, mode: &str) -> Result<Value> {
        enroll(&self.options, token, mode).await
    }

    /// Start the outbound connection (resolves when the host accepted it).
    pub async fn connect(self: &Arc<Self>) -> Result<Connection> {
        *locked(&self.phase) = json!({"phase": "connecting",
            "generation": locked(&self.phase)["generation"].clone()});
        let host = self.clone();
        let started = tokio::spawn(async move { host.call("connect", json!({})).await });
        Ok(Connection {
            host: self.clone(),
            started: AsyncMutex::new(Some(started)),
        })
    }

    pub fn state(&self) -> Value {
        locked(&self.phase).clone()
    }

    pub fn recovery_status(&self) -> Value {
        locked(&self.recovery).clone()
    }

    pub fn keys(&self) -> Result<HashMap<String, String>> {
        self.options.keys()
    }

    pub fn device_id(&self) -> Result<String> {
        self.options.device_id()
    }

    pub async fn start_task(&self, task: &str) -> Result<Value> {
        self.call("start-task", json!({"taskId": task})).await
    }

    pub async fn context(&self, task: &str, ownership: &Value, revision: &Value) -> Result<Value> {
        self.call(
            "context",
            json!({"taskId": task, "ownership": ownership, "revision": revision}),
        )
        .await
    }

    /// Canonical action, or a host action whose provider answers `effect`.
    pub async fn action<F, Fut>(&self, input: &Value, effect: F) -> Result<Value>
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value>> + Send + 'static,
    {
        if input["actionKind"] == "canonical" {
            return self.call("action", json!({"input": input})).await;
        }
        let provider = provider(effect).await?;
        let result = self
            .call(
                "action",
                json!({"input": input, "provider": provider.origin()}),
            )
            .await;
        drop(provider);
        result
    }

    /// `run-task` in the control process with a provider.
    pub async fn run_task<F, Fut>(&self, task: &str, provide: F) -> Result<Value>
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value>> + Send + 'static,
    {
        let provider = provider(provide).await?;
        let result = self
            .call(
                "run-task",
                json!({"taskId": task, "provider": provider.origin()}),
            )
            .await;
        drop(provider);
        result
    }

    pub async fn recover(&self) -> Result<Value> {
        self.call("recover", json!({})).await
    }

    /// Stop the control process (stdin closed, SIGKILL after 5 s).
    pub async fn close(&self) -> Result<()> {
        let worker = self.worker.lock().await.take();
        if let Some(worker) = worker {
            worker.stdin.lock().await.take();
            let mut child = worker.child.lock().await;
            if tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .is_err()
            {
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
        }
        Ok(())
    }
}

/// An outbound connection started by [`DeviceHost::connect`].
pub struct Connection {
    host: Arc<DeviceHost>,
    started: AsyncMutex<Option<tokio::task::JoinHandle<Result<Value>>>>,
}

impl Connection {
    pub fn state(&self) -> Value {
        self.host.state()
    }

    pub fn phase(&self) -> String {
        self.state()["phase"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    pub fn generation(&self) -> i64 {
        self.state()["generation"].as_i64().unwrap_or(0)
    }

    /// Wait for the connect call, then disconnect.
    pub async fn stop(&self) -> Result<()> {
        if let Some(started) = self.started.lock().await.take() {
            let _ = started.await;
        }
        self.host.call("disconnect", json!({})).await?;
        Ok(())
    }
}

/// HTTP provider: every request body is an action context, the reply is
/// whatever `provide` returns (`withDeviceProvider`).
pub async fn provider<F, Fut>(provide: F) -> Result<Relay>
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value>> + Send + 'static,
{
    let provide = Arc::new(provide);
    Relay::start(move |exchange| {
        let provide = provide.clone();
        async move {
            Ok(match provide(exchange.json()).await {
                Ok(value) => Answer::json(200, &value),
                Err(_) => Answer::empty(502, Vec::new()),
            })
        }
    })
    .await
}

async fn line_of(child: &mut Child, select: impl Fn(&Value) -> bool) -> Result<Value> {
    let stdout = child.stdout.take().ok_or("device host stdout")?;
    let mut lines = BufReader::new(stdout).lines();
    let found = tokio::time::timeout(Duration::from_secs(15), async {
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                if select(&value) {
                    return Some(value);
                }
            }
        }
        None
    })
    .await
    .map_err(|_| "Rust device host readiness timed out.")?;
    tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
    found.ok_or_else(|| "Rust device host exited before readiness.".into())
}

/// Run `enroll` and approve it with `token` (`enrollRustDevice`).
pub async fn enroll(options: &HostOptions, token: &str, mode: &str) -> Result<Value> {
    let mut args = vec!["enroll"];
    let flag = format!("--{mode}");
    if mode != "new" {
        args.push(&flag);
    }
    let mut child = options
        .command(&args)?
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let enrollment = line_of(&mut child, |value| value.get("origin").is_some()).await?;
    let origin = enrollment["origin"].as_str().unwrap_or_default().to_owned();
    let bearer = std::fs::read_to_string(enrollment["tokenFile"].as_str().unwrap_or_default())?;
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()?
        .post(format!("{origin}/local/link-proof"))
        .header("Origin", &origin)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {bearer}"))
        .body(json!({"requestId": crate::new_id(), "clerkToken": token}).to_string())
        .send()
        .await?;
    let status = response.status().as_u16();
    let result: Value = response.json().await.unwrap_or(Value::Null);
    if status >= 400 || result["ok"] != true {
        let _ = child.start_kill();
        let _ = child.wait().await;
        return Err(HostError {
            status,
            code: result["error"]["code"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            message: format!(
                "Rust device enrollment denied: {status} {}",
                result["error"]["code"]
            ),
        }
        .into());
    }
    let exit = tokio::time::timeout(Duration::from_secs(15), child.wait()).await;
    match exit {
        Ok(Ok(status)) if status.success() => Ok(result["data"].clone()),
        _ => {
            let _ = child.start_kill();
            Err("Rust device enrollment did not finish".into())
        }
    }
}

/// Start `enroll` without approving it; returns the enrollment line and
/// the running process (`startRustDeviceEnrollment`).
pub async fn start_enrollment(options: &HostOptions, mode: &str) -> Result<(Value, Child)> {
    let mut args = vec!["enroll"];
    let flag = format!("--{mode}");
    if mode != "new" {
        args.push(&flag);
    }
    let mut child = options
        .command(&args)?
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let enrollment = line_of(&mut child, |value| value.get("origin").is_some()).await?;
    Ok((enrollment, child))
}

/// Standalone connection process (`startRustDeviceConnection`).
pub struct ConnectionProcess {
    child: Child,
    state: Arc<Mutex<Value>>,
    stderr: Arc<Mutex<String>>,
}

impl ConnectionProcess {
    pub fn start(options: &HostOptions) -> Result<Self> {
        let mut child = options
            .command(&[])?
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let state = Arc::new(Mutex::new(Value::Null));
        let stderr = Arc::new(Mutex::new(String::new()));
        let stdout = child.stdout.take().ok_or("connection stdout")?;
        let err = child.stderr.take().ok_or("connection stderr")?;
        let shared = state.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if value.get("phase").is_some() {
                        *locked(&shared) = value;
                    }
                }
            }
        });
        let tail = stderr.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                locked(&tail).push_str(&line);
            }
        });
        Ok(Self {
            child,
            state,
            stderr,
        })
    }

    pub fn state(&self) -> Value {
        locked(&self.state).clone()
    }

    pub fn phase(&self) -> String {
        self.state()["phase"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    pub fn stderr(&self) -> String {
        locked(&self.stderr).clone()
    }

    /// SIGTERM, then SIGKILL after 5 s.
    pub async fn stop(&mut self) -> Result<()> {
        if let Some(pid) = self.child.id() {
            if let Some(pid) = rustix::process::Pid::from_raw(pid as i32) {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
            }
        }
        if tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.start_kill();
            let _ = self.child.wait().await;
        }
        Ok(())
    }
}

/// `run-task <id> --test-provider <url>` as its own process
/// (`runRustDeviceTask`): the JSON result, or the host error it reported.
pub async fn run_task_process(options: &HostOptions, task: &str, provider: &str) -> Result<Value> {
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        options
            .command(&["run-task", task])?
            .arg("--test-provider")
            .arg(provider)
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| "run-task timed out")??;
    if output.status.success() {
        return Ok(serde_json::from_slice(&output.stdout)?);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if let Some(first) = stderr.lines().find(|line| line.starts_with('{')) {
        return Err(HostError::from_value(&serde_json::from_str(first)?).into());
    }
    Err(stderr.trim().to_owned().into())
}

/// Run a task until `checkpoint`, then SIGKILL it (`crashRustDeviceTask`).
pub async fn crash_task(
    options: &HostOptions,
    task: &str,
    provider: &str,
    checkpoint: &str,
) -> Result<()> {
    let mut child = options
        .command(&["run-task", task])?
        .arg("--test-provider")
        .arg(provider)
        .arg("--test-checkpoint")
        .arg(checkpoint)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let line = line_of(&mut child, |_| true).await?;
    if line["checkpoint"] != checkpoint {
        let _ = child.start_kill();
        return Err(format!("Expected Rust task checkpoint {checkpoint}, got {line}").into());
    }
    child.start_kill()?;
    let status = child.wait().await?;
    use std::os::unix::process::ExitStatusExt;
    if status.signal() != Some(9) {
        return Err("Rust task host did not stop at the crash checkpoint.".into());
    }
    Ok(())
}

/// `export-recovery` with `input` on stdin (`exportRustDeviceRecovery`).
pub async fn export_recovery(options: &HostOptions, input: &Value) -> Result<Value> {
    let mut child = options
        .command(&["export-recovery"])?
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.to_string().as_bytes()).await?;
    }
    let output = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .map_err(|_| "export-recovery timed out")??;
    if output.status.success() {
        return Ok(serde_json::from_slice(&output.stdout)?);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if let Some(first) = stderr.lines().next().filter(|line| line.starts_with('{')) {
        return Err(HostError::from_value(&serde_json::from_str(first)?).into());
    }
    Err(stderr.trim().to_owned().into())
}
