#![forbid(unsafe_code)]
mod backup;
mod config;
mod runtime;
use minarbot_launcher::{files, DATABASE_NAME};
#[cfg(test)]
mod tests;

use config::Config;
use files::*;
use nix::{
    sys::{
        signal::{kill, Signal},
        statvfs::statvfs,
    },
    unistd::Pid,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    rc::Rc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    process::{Child, Command},
    signal::unix::{signal, SignalKind},
    sync::Notify,
    task::{JoinHandle, JoinSet, LocalSet},
    time::{sleep, timeout, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct State {
    protocol: u8,
    current: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous: Option<String>,
    receipts: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_attempt: Option<Attempt>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Attempt {
    id: String,
    from: String,
    target: String,
    version: String,
    status: String,
    started: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finished: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    snapshot_ready: Option<bool>,
}
struct Managed {
    process: Child,
    /// Last `Error: ...` line the server wrote to stderr, as its startup failure.
    failure: Rc<RefCell<Option<String>>>,
    stderr: Option<JoinHandle<()>>,
}
struct Launcher {
    config: Config,
    executable: PathBuf,
    state: RefCell<State>,
    child: RefCell<Option<Managed>>,
    busy: Cell<bool>,
    stopping: Cell<bool>,
    job: RefCell<Option<JoinHandle<()>>>,
    fatal: Notify,
}
fn timestamp() -> Result<String> {
    Ok(
        time::OffsetDateTime::now_utc().format(&time::format_description::parse_borrowed::<2>(
            "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z",
        )?)?,
    )
}
fn finished(status: &str) -> bool {
    ["done", "rolled-back", "failed"].contains(&status)
}
impl Launcher {
    fn save(&self) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(&*self.state.borrow())?;
        bytes.push(b'\n');
        durable(&self.config.data.join("update-state.json"), &bytes)
    }
    fn attempt(&self) -> Result<Attempt> {
        self.state
            .borrow()
            .last_attempt
            .clone()
            .ok_or_else(|| "Invalid launcher state.".into())
    }
    fn phase(&self, status: &str, reason: Option<&str>) -> Result<()> {
        {
            let mut state = self.state.borrow_mut();
            let attempt = state
                .last_attempt
                .as_mut()
                .ok_or("Invalid launcher state.")?;
            attempt.status = status.into();
            if let Some(reason) = reason {
                attempt.reason = Some(reason.into());
            }
            if finished(status) {
                attempt.finished = Some(timestamp()?);
            }
        }
        self.save()
    }
    fn start(&self, id: &str, trial: bool) -> Result<()> {
        if self.stopping.get() {
            return Ok(());
        }
        let path = path_for(&self.config.releases, id)?;
        let binary = path.join("bin/minarbot-server");
        if !binary.is_file() {
            return Err("Release is missing bin/minarbot-server.".into());
        }
        let mut command = Command::new(&self.executable);
        command
            .arg("--run-server")
            .arg(binary)
            .arg(std::process::id().to_string());
        command
            .current_dir(path)
            .env_clear()
            .envs(&self.config.env)
            .env("MINARBOT_RELEASE_ID", id)
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if trial {
            let id = self.attempt()?.id;
            command.env("MINARBOT_UPDATE_TRIAL", &id);
        }
        let mut process = command.spawn()?;
        let failure = Rc::new(RefCell::new(None));
        let stderr = process
            .stderr
            .take()
            .map(|stderr| tokio::task::spawn_local(forward_stderr(stderr, failure.clone())));
        *self.child.borrow_mut() = Some(Managed {
            process,
            failure,
            stderr,
        });
        Ok(())
    }
    async fn stop_child(&self) -> Result<()> {
        let child = self.child.borrow_mut().take();
        if let Some(mut child) = child {
            if child.process.try_wait()?.is_none() {
                if let Some(id) = child.process.id() {
                    match kill(Pid::from_raw(id as i32), Signal::SIGTERM) {
                        Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                match timeout(Duration::from_secs(5), child.process.wait()).await {
                    Ok(status) => {
                        status?;
                    }
                    Err(_) => child.process.kill().await?,
                }
            }
        }
        Ok(())
    }
    fn child_exited(&self) -> Result<bool> {
        let mut managed = self.child.borrow_mut();
        match managed.as_mut() {
            Some(child) => Ok(child.process.try_wait()?.is_some()),
            None => Ok(true),
        }
    }
    async fn exit_reason(&self) -> String {
        let (stderr, failure) = match self.child.borrow_mut().as_mut() {
            Some(child) => (child.stderr.take(), child.failure.clone()),
            None => (None, Rc::default()),
        };
        if let Some(stderr) = stderr {
            // The pipe closes once the server has exited, so this only waits for the tail.
            let _ = timeout(Duration::from_secs(1), stderr).await;
        }
        let failure = failure.borrow().clone();
        match failure {
            Some(message) => format!("Candidate exited before becoming healthy: {message}"),
            None => "Candidate exited before becoming healthy.".into(),
        }
    }
    async fn health(&self, id: &str) -> Result<()> {
        let version =
            json(&path_for(&self.config.releases, id)?.join("release.json"))?["version"].clone();
        let deadline = Instant::now() + self.config.timeout;
        while !self.stopping.get() && Instant::now() < deadline {
            if self.child_exited()? {
                return Err(self.exit_reason().await.into());
            }
            if timeout(
                Duration::from_millis(500),
                health_request(self.config.port, &version),
            )
            .await
            .is_ok_and(|v| v.is_ok())
            {
                return Ok(());
            }
            sleep(Duration::from_millis(100)).await;
        }
        Err(if self.stopping.get() {
            "Launcher stopped during update."
        } else {
            "Candidate health check timed out or reported a different version."
        }
        .into())
    }
    async fn worker(&self, operation: &str) -> Result<()> {
        let attempt = self.attempt()?;
        path_for(&self.config.releases, &attempt.from)?;
        let database = self.config.data.join(DATABASE_NAME);
        let destination = self
            .config
            .data
            .join(".updates")
            .join(&attempt.id)
            .join(DATABASE_NAME);
        let mut worker = Command::new(&self.executable)
            .arg("--backup")
            .arg(operation)
            .arg(database)
            .arg(destination)
            .env_clear()
            .env(
                "PATH",
                self.config
                    .env
                    .get("PATH")
                    .map(String::as_str)
                    .unwrap_or(""),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let status = timeout(self.config.timeout, worker.wait()).await;
        if !matches!(status, Ok(Ok(s)) if s.success()) {
            worker.kill().await?;
            return Err(if operation == "snapshot" {
                "Snapshot failed. Server remains stopped."
            } else {
                "Snapshot restore failed. Server remains stopped."
            }
            .into());
        }
        Ok(())
    }
    async fn rollback(&self, reason: &str) -> Result<()> {
        self.stop_child().await?;
        self.phase("restoring", Some(reason))?;
        let attempt = self.attempt()?;
        if attempt.snapshot_ready == Some(true) {
            self.worker("restore").await?;
        }
        select(&self.config.releases, "current", &attempt.from)?;
        remove(&self.config.releases.join("trial"))?;
        self.state.borrow_mut().current = attempt.from.clone();
        self.phase("rolled-back", Some(reason))?;
        if !self.stopping.get() {
            self.start(&attempt.from, false)?;
            self.health(&attempt.from).await?;
        }
        Ok(())
    }
    async fn trial(&self) -> Result<()> {
        self.stop_child().await?;
        self.phase("backing-up", None)?;
        self.worker("snapshot").await?;
        self.state
            .borrow_mut()
            .last_attempt
            .as_mut()
            .ok_or("Invalid launcher state.")?
            .snapshot_ready = Some(true);
        self.save()?;
        if self.stopping.get() {
            return Ok(());
        }
        let attempt = self.attempt()?;
        select(&self.config.releases, "trial", &attempt.target)?;
        select(&self.config.releases, "current", &attempt.target)?;
        self.phase("starting", None)?;
        self.start(&attempt.target, true)?;
        self.phase("verifying", None)?;
        self.health(&attempt.target).await?;
        {
            let mut state = self.state.borrow_mut();
            state.previous = Some(attempt.from.clone());
            state.current = attempt.target;
        }
        select(&self.config.releases, "previous", &attempt.from)?;
        self.phase("done", None)?;
        remove(&self.config.releases.join("trial"))?;
        Ok(())
    }
    fn accept(self: &Rc<Self>, request: &Value) -> Result<Value> {
        let id = request["id"].as_str().ok_or("Invalid update request.")?;
        let target = request["target"]
            .as_str()
            .ok_or("Invalid update request.")?;
        let version = request["version"]
            .as_str()
            .ok_or("Invalid update request.")?;
        if !valid_request_id(id) || !release_id(target) {
            return Err("Invalid update request.".into());
        }
        let fingerprint = serde_json::to_string(&[target, version])?;
        if let Some(prior) = self.state.borrow().receipts.get(id) {
            if prior != &fingerprint {
                return Err("Request ID was already used for another release.".into());
            }
            return Ok(json!({"accepted":true,"id":id}));
        }
        if self.busy.get() || self.stopping.get() {
            return Err("Another update is already running.".into());
        }
        let info = verify(&self.config.releases, target)?;
        if info["version"] != version || target == self.state.borrow().current {
            return Err("Release version is not an update.".into());
        }
        let disk = statvfs(&self.config.data)?;
        let mut bytes = 0;
        let database = self.config.data.join(DATABASE_NAME);
        for suffix in ["", "-wal", "-shm"] {
            if let Ok(s) = fs::metadata(sidecar(&database, suffix)) {
                bytes += u128::from(s.len());
            }
        }
        if u128::from(disk.blocks_available()) * u128::from(disk.block_size())
            < bytes * 3 + 16 * 1024 * 1024
        {
            return Err("Insufficient disk space for update snapshot and trial.".into());
        }
        {
            let mut state = self.state.borrow_mut();
            state.receipts.insert(id.into(), fingerprint);
            state.last_attempt = Some(Attempt {
                id: id.into(),
                from: state.current.clone(),
                target: target.into(),
                version: version.into(),
                status: "accepted".into(),
                started: timestamp()?,
                finished: None,
                reason: None,
                snapshot_ready: None,
            });
        }
        self.save()?;
        self.busy.set(true);
        let launcher = self.clone();
        *self.job.borrow_mut() = Some(tokio::task::spawn_local(async move {
            sleep(Duration::from_millis(300)).await;
            if let Err(error) = launcher.trial().await {
                if !launcher.stopping.get() {
                    if let Err(error) = launcher.rollback(&error.to_string()).await {
                        eprintln!("[minarbot-launcher] {error}");
                        launcher.fatal.notify_one();
                    }
                }
            }
            launcher.busy.set(false);
        }));
        Ok(json!({"accepted":true,"id":id}))
    }
    fn request(self: &Rc<Self>, request: Value) -> Result<Value> {
        let keys = if request["type"] == "apply" {
            &["type", "id", "target", "version"][..]
        } else if request["type"] == "trial" {
            &["type", "id"][..]
        } else {
            &["type"][..]
        };
        if request
            .as_object()
            .ok_or("Invalid control request.")?
            .keys()
            .any(|key| !keys.contains(&key.as_str()))
        {
            return Err("Invalid control request.".into());
        }
        match request["type"].as_str() {
            Some("apply") => self.accept(&request),
            Some("trial") => {
                let id = request["id"]
                    .as_str()
                    .filter(|id| valid_request_id(id))
                    .ok_or("Invalid update request.")?;
                let state = self.state.borrow();
                let attempt = state
                    .last_attempt
                    .as_ref()
                    .filter(|a| a.id == id)
                    .ok_or("Unknown update trial.")?;
                Ok(json!({"committed":attempt.status=="done" && state.current==attempt.target}))
            }
            Some("status") => {
                let mut value = serde_json::to_value(&*self.state.borrow())?;
                value
                    .as_object_mut()
                    .ok_or("Invalid launcher state.")?
                    .remove("receipts");
                value["releasePublicKey"] = json!(minarbot_launcher::RELEASE_PUBLIC_KEY);
                Ok(value)
            }
            _ => Err("Unknown control request.".into()),
        }
    }
}
fn valid_request_id(value: &str) -> bool {
    value.len() == 36
        && uuid::Uuid::parse_str(value)
            .is_ok_and(|id| id.get_version_num() == 4 && id.get_variant() == uuid::Variant::RFC4122)
}
async fn health_request(port: u16, version: &Value) -> Result<()> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(500))
        .build()?;
    let mut response = client
        .get(format!("http://127.0.0.1:{port}/api/environment"))
        .send()
        .await?;
    if response.status() != 200 || response.content_length().unwrap_or(0) >= 8192 {
        return Err("Invalid health response".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > 16384 {
            return Err("Health response too large".into());
        }
        body.extend_from_slice(&chunk);
    }
    if serde_json::from_slice::<Value>(&body)?["version"] != *version {
        return Err("Wrong health version".into());
    }
    Ok(())
}
async fn control(launcher: Rc<Launcher>, mut socket: UnixStream) {
    let result = timeout(Duration::from_secs(15), async {
        let mut buffer = Vec::new();
        loop {
            let mut bytes = [0; 4097];
            let n = socket.read(&mut bytes).await?;
            if n == 0 {
                return Ok::<_, Box<dyn std::error::Error>>(());
            }
            buffer.extend_from_slice(&bytes[..n]);
            if buffer.len() > 4096 {
                return Ok(());
            }
            if let Some(end) = buffer.iter().position(|b| *b == b'\n') {
                let response = serde_json::from_slice(&buffer[..end])
                    .map_err(Into::into)
                    .and_then(|request| launcher.request(request));
                let value = match response {
                    Ok(value) => value,
                    Err(error) => json!({"error":error.to_string()}),
                };
                socket.write_all(format!("{value}\n").as_bytes()).await?;
                socket.shutdown().await?;
                return Ok(());
            }
        }
    })
    .await;
    let _ = result;
}
async fn launch(mut config: Config) -> Result<()> {
    mkdir(&config.data)?;
    mkdir(&config.releases)?;
    let _lease = lease(
        &config.data.join(".launcher-lock.sqlite"),
        "Another launcher owns this data directory.",
    )?;
    let state_path = config.data.join("update-state.json");
    let state = if state_path.exists() {
        private_file(&state_path)?;
        serde_json::from_value::<State>(json(&state_path)?)
            .map_err(|_| "Invalid launcher state.")?
    } else {
        State {
            protocol: 1,
            current: selection(&config.releases, "current")?
                .ok_or("Select an installed release with the current symlink first.")?,
            previous: None,
            receipts: BTreeMap::new(),
            last_attempt: None,
        }
    };
    if state.protocol != 1
        || !release_id(&state.current)
        || state.previous.as_ref().is_some_and(|v| !release_id(v))
        || state.last_attempt.as_ref().is_some_and(|a| {
            !valid_request_id(&a.id) || !release_id(&a.from) || !release_id(&a.target)
        })
    {
        return Err("Invalid launcher state.".into());
    }
    let socket = runtime::ControlSocket::bind(&config.data)?;
    config.env.insert(
        "MINARBOT_LAUNCHER_SOCKET".into(),
        socket.path().to_string_lossy().into_owned(),
    );
    let launcher = Rc::new(Launcher {
        config,
        executable: socket.executable()?,
        state: RefCell::new(state),
        child: RefCell::new(None),
        busy: Cell::new(false),
        stopping: Cell::new(false),
        job: RefCell::new(None),
        fatal: Notify::new(),
    });
    launcher.save()?;
    let mut term = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let result = async {
        if launcher.state.borrow().last_attempt.as_ref().is_some_and(|a| !finished(&a.status)) {
            launcher.busy.set(true); launcher.rollback("Launcher restarted during trial.").await?; launcher.busy.set(false);
        } else {
            let current = launcher.state.borrow().current.clone(); verify(&launcher.config.releases, &current)?; select(&launcher.config.releases, "current", &current)?; launcher.start(&current, false)?;
        }
        let mut clients = JoinSet::new(); let mut tick = tokio::time::interval(Duration::from_millis(50));
        loop {
            tokio::select! {
                _ = term.recv() => break,
                _ = interrupt.recv() => break,
                _ = launcher.fatal.notified() => return Err("Update rollback failed. Server remains stopped.".into()),
                accepted = socket.listener.accept() => { let (socket, _) = accepted?; clients.spawn_local(control(launcher.clone(), socket)); },
                Some(_) = clients.join_next(), if !clients.is_empty() => {},
                _ = tick.tick() => { if !launcher.busy.get() && launcher.child_exited()? { return Err("Managed server exited.".into()); } },
            }
        }
        Ok(())
    }.await;
    launcher.stopping.set(true);
    let stopped = launcher.stop_child().await;
    let job = launcher.job.borrow_mut().take();
    if let Some(job) = job {
        job.await?;
    }
    stopped?;
    result
}
/// Copies the server's stderr to the launcher's own, so the journal keeps every line,
/// and remembers the last startup error. It reads until end of file even past invalid
/// UTF-8, because a full pipe would block the server.
async fn forward_stderr(stderr: tokio::process::ChildStderr, failure: Rc<RefCell<Option<String>>>) {
    use std::io::Write;
    let mut reader = tokio::io::BufReader::new(stderr);
    let mut line = Vec::new();
    while matches!(reader.read_until(b'\n', &mut line).await, Ok(n) if n > 0) {
        let _ = std::io::stderr().write_all(&line);
        if let Some(message) = startup_error(&String::from_utf8_lossy(&line)) {
            *failure.borrow_mut() = Some(message);
        }
        line.clear();
    }
}
/// Reads the line Rust prints when `main` returns an error. Only the server's own
/// error message reaches the rollback reason, cut to 300 characters.
fn startup_error(line: &str) -> Option<String> {
    let message = line.trim_end().strip_prefix("Error: ")?;
    let message = match message.strip_prefix('"').and_then(|m| m.strip_suffix('"')) {
        Some(quoted) => quoted.replace("\\\"", "\"").replace("\\\\", "\\"),
        None => message.to_owned(),
    };
    let message: String = message
        .chars()
        .filter(|c| !c.is_control())
        .take(300)
        .collect();
    (!message.is_empty()).then_some(message)
}
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let result = if args.len() == 3 && args[0] == "--run-server" {
        runtime::run_server(Path::new(&args[1]), &args[2].to_string_lossy())
    } else if args.len() == 4 && args[0] == "--backup" {
        backup::run(
            &args[1].to_string_lossy(),
            Path::new(&args[2]),
            Path::new(&args[3]),
        )
    } else if args.len() == 2 && args[0] == "--config" {
        match Config::load(Path::new(&args[1])) {
            Ok(config) => LocalSet::new().run_until(launch(config)).await,
            Err(error) => Err(error),
        }
    } else {
        Err("Usage: minarbot-launcher --config CONFIG.json".into())
    };
    if let Err(error) = result {
        eprintln!("[minarbot-launcher] {error}");
        std::process::exit(1);
    }
}
