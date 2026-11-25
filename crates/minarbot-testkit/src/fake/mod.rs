//! Fake harness peers (library side of the `fake-*` binaries).
//!
//! Each peer is a faithful Rust port of a former Node fixture and keeps its
//! wire behaviour, prompt keywords, assertions and log files. The behaviour
//! checklist lives in `docs/testing.md`; `tests/fakes.rs` proves each item by
//! driving the binaries directly over stdio.

pub mod acp;
pub mod gateway;
pub mod native;

use serde_json::Value;
use std::{
    fs::OpenOptions,
    io::Write,
    process::ExitCode,
    sync::{Arc, Mutex},
};

/// Failure inside a fake. The binaries print it and exit with status 1, the
/// way an assertion failure crashed the Node fixtures.
pub type Result<T, E = Box<dyn std::error::Error + Send + Sync>> = std::result::Result<T, E>;

/// Fail with `message` unless `condition` holds.
pub fn check(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into().into())
    }
}

/// Serialized writer for newline-delimited JSON on stdout.
#[derive(Clone)]
pub struct Out(Arc<Mutex<std::io::Stdout>>);

impl Default for Out {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(std::io::stdout())))
    }
}

impl Out {
    /// Write one JSON line and flush.
    pub fn send(&self, value: &Value) {
        self.raw(format!("{value}\n").as_bytes());
    }

    /// Write raw bytes and flush (malformed or oversized output).
    pub fn raw(&self, bytes: &[u8]) {
        let stdout = match self.0.lock() {
            Ok(stdout) => stdout,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut handle = stdout.lock();
        if handle
            .write_all(bytes)
            .and_then(|()| handle.flush())
            .is_err()
        {
            // The harness closed our stdout: the process is being stopped.
            std::process::exit(0);
        }
    }
}

/// Append one JSON line to `file` in the working directory with a single
/// `write`, so concurrent writers and readers never see a split line.
pub fn log(file: &str, value: &Value) {
    if let Ok(mut handle) = OpenOptions::new().create(true).append(true).open(file) {
        let _ = handle.write_all(format!("{value}\n").as_bytes());
    }
}

/// Command-line access with the Node fixtures' `arg(flag)` semantics:
/// `--flag value` first, then `--flag=value`.
#[derive(Clone, Debug)]
pub struct Args(pub Vec<String>);

impl Args {
    pub fn current() -> Self {
        Self(std::env::args().collect())
    }

    pub fn has(&self, flag: &str) -> bool {
        self.0.iter().any(|value| value == flag)
    }

    pub fn value(&self, flag: &str) -> Option<String> {
        if let Some(index) = self.0.iter().position(|value| value == flag) {
            return self.0.get(index + 1).cloned();
        }
        let prefix = format!("{flag}=");
        self.0
            .iter()
            .find_map(|value| value.strip_prefix(&prefix).map(str::to_owned))
    }

    pub fn nth(&self, index: usize) -> Option<&str> {
        self.0.get(index).map(String::as_str)
    }
}

const IDLE_CHILD: &str = "--fake-idle-child";
const STUBBORN_CHILD: &str = "--fake-stubborn-child";

/// Spawn a copy of this executable that idles until killed.
pub fn spawn_idle_child(stubborn: bool, detached: bool) -> std::io::Result<u32> {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .arg(if stubborn { STUBBORN_CHILD } else { IDLE_CHILD })
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if detached {
        command.process_group(0);
    }
    let child = command.spawn()?;
    Ok(child.id())
}

static TASKS: Mutex<Vec<tokio::task::JoinHandle<()>>> = Mutex::new(Vec::new());

/// Spawn peer work that keeps the process alive after stdin closes, the way
/// pending timers and requests kept the Node fixtures alive.
pub fn spawn(
    task: impl std::future::Future<Output = ()> + Send + 'static,
) -> tokio::task::AbortHandle {
    let handle = tokio::spawn(task);
    let abort = handle.abort_handle();
    match TASKS.lock() {
        Ok(mut tasks) => tasks.push(handle),
        Err(poisoned) => poisoned.into_inner().push(handle),
    }
    abort
}

async fn drain() {
    loop {
        let pending: Vec<_> = match TASKS.lock() {
            Ok(mut tasks) => tasks.drain(..).collect(),
            Err(poisoned) => poisoned.into_inner().drain(..).collect(),
        };
        if pending.is_empty() {
            return;
        }
        for task in pending {
            let _ = task.await;
        }
    }
}

/// Run a fake: helper child modes first, then `body` on a Tokio runtime.
pub fn main<F, Fut>(body: F) -> ExitCode
where
    F: FnOnce(Args) -> Fut,
    Fut: std::future::Future<Output = Result<ExitCode>>,
{
    let args = Args::current();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("fake runtime failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    if args.nth(1) == Some(IDLE_CHILD) || args.nth(1) == Some(STUBBORN_CHILD) {
        let stubborn = args.nth(1) == Some(STUBBORN_CHILD);
        runtime.block_on(async move {
            // Registering a SIGTERM stream replaces the default action, so a
            // stubborn child survives TERM and only dies to KILL.
            let _term = if stubborn {
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok()
            } else {
                None
            };
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            }
        })
    }
    match runtime.block_on(async move {
        let code = body(args).await;
        drain().await;
        code
    }) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("fake peer failed: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Next stdin line, or `None` at end of input.
pub async fn next_line(
    lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::io::Stdin>>,
) -> Result<Option<String>> {
    Ok(lines.next_line().await?)
}

/// Line reader over stdin.
pub fn stdin_lines() -> tokio::io::Lines<tokio::io::BufReader<tokio::io::Stdin>> {
    use tokio::io::AsyncBufReadExt;
    tokio::io::BufReader::new(tokio::io::stdin()).lines()
}

/// `process.env[name]` with JavaScript truthiness (unset and empty are absent).
pub fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// Working directory as `process.cwd()` reports it.
pub fn cwd() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}
