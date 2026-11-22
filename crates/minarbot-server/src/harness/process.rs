use super::*;
use crate::platform::{process_identity, processes, LINUX_SANDBOX};
use std::{
    collections::HashMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

pub(super) fn mise_shim(path: &Path) -> bool {
    if std::fs::canonicalize(path)
        .ok()
        .is_some_and(|target| target.file_name().is_some_and(|name| name == "mise"))
    {
        return true;
    }
    // ponytail: Match shell wrappers' direct mise calls. Add explicit config for indirect wrappers if needed.
    let mut prefix = Vec::new();
    path.is_file()
        && File::open(path)
            .and_then(|file| file.take(8192).read_to_end(&mut prefix))
            .is_ok()
        && prefix.starts_with(b"#!")
        && String::from_utf8_lossy(&prefix)
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .flat_map(|line| line.split(';'))
            .any(|command| {
                let mut words = command.split_ascii_whitespace();
                matches!(
                    (words.next(), words.next(), words.next()),
                    (Some("mise"), Some("x" | "exec"), _)
                        | (Some("exec"), Some("mise"), Some("x" | "exec"))
                )
            })
}

const MISE_DIRS: [(&str, &str, &str); 3] = [
    ("MISE_CONFIG_DIR", "XDG_CONFIG_HOME", ".config"),
    ("MISE_DATA_DIR", "XDG_DATA_HOME", ".local/share"),
    ("MISE_CACHE_DIR", "XDG_CACHE_HOME", ".cache"),
];

pub(super) fn configure_mise(env: &mut HashMap<String, String>) {
    let home = std::env::home_dir().filter(|path| path.is_absolute());
    let absolute = |key| {
        std::env::var_os(key)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    for (key, xdg, default) in MISE_DIRS {
        let path = absolute(key).or_else(|| {
            absolute(xdg)
                .or_else(|| home.as_ref().map(|home| home.join(default)))
                .map(|base| base.join("mise"))
        });
        if let Some(path) = path.and_then(|path| path.into_os_string().into_string().ok()) {
            env.insert(key.into(), path);
        }
    }
    // A login PATH can list mise's own install and shim dirs after the shim's dir; then `mise x`
    // resolves the tool back to the shim and loops. Let `mise x` supply tool paths instead.
    if let (Some(data), Some(path)) = (env.get("MISE_DATA_DIR").cloned(), env.get("PATH").cloned())
    {
        let data = Path::new(&data);
        let kept = path
            .split(':')
            .filter(|entry| !Path::new(entry).starts_with(data))
            .collect::<Vec<_>>()
            .join(":");
        env.insert("PATH".into(), kept);
    }
    // mise records trust for every new HOME in its state dir, so keep state in the isolated home
    // and trust only the owner's mise config.
    if let Some(home) = env.get("HOME").cloned() {
        env.insert("MISE_STATE_DIR".into(), format!("{home}/.local/state/mise"));
    }
    if let Some(config) = env.get("MISE_CONFIG_DIR").cloned() {
        env.insert("MISE_TRUSTED_CONFIG_PATHS".into(), config);
    }
    // Keep config-sourced env and hooks out of harness launches.
    for key in ["MISE_NO_ENV", "MISE_NO_HOOKS"] {
        env.insert(key.into(), "1".into());
    }
}

fn track(root: u32, birth: &str, owned: &mut HashMap<u32, String>) {
    let processes = processes();
    let mut parents = owned
        .iter()
        .filter(|(pid, start)| processes.get(pid).is_some_and(|v| v.1 == **start))
        .map(|(pid, _)| *pid)
        .collect::<std::collections::HashSet<_>>();
    if processes.get(&root).is_some_and(|v| v.1 == birth) {
        parents.insert(root);
    }
    loop {
        let mut changed = false;
        for (pid, (parent, start)) in &processes {
            if parents.contains(parent) && !parents.contains(pid) {
                parents.insert(*pid);
                owned.insert(*pid, start.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}
fn signal(pid: u32) {
    if let Some(pid) = rustix::process::Pid::from_raw(pid as i32) {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
    }
}
const STDERR_BYTES: usize = 16 * 1024;

pub struct Process {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    root: u32,
    birth: String,
    owned: Arc<Mutex<HashMap<u32, String>>>,
    watch: tokio::task::JoinHandle<()>,
    stderr: Arc<Mutex<Vec<u8>>>,
    stderr_reader: tokio::task::JoinHandle<()>,
    stderr_destination: Option<(AppState, i64, Vec<String>)>,
    bytes: usize,
    pending: Vec<u8>,
    pub(super) clock: Option<super::config::TurnClock>,
}
impl Process {
    pub fn launch(
        path: &std::path::Path,
        args: &[String],
        cwd: &std::path::Path,
        env: &HashMap<String, String>,
    ) -> Result<Self> {
        let mut command = Command::new(path);
        command
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        for key in ["PATH", "HOME", "LANG", "LC_ALL", "TMPDIR", "TERM"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command.envs(env);
        let mut child = command.spawn().map_err(|_| {
            error(
                502,
                "Coding harness could not start. Check its executable on PATH.",
            )
        })?;
        let root = child.id().ok_or_else(Failure::storage)?;
        let birth = process_identity(root).map(|v| v.1).unwrap_or_default();
        let stdin = child.stdin.take().ok_or_else(Failure::storage)?;
        let stdout = BufReader::new(child.stdout.take().ok_or_else(Failure::storage)?);
        let mut pipe = child.stderr.take().ok_or_else(Failure::storage)?;
        let stderr = Arc::new(Mutex::new(Vec::<u8>::new()));
        let tail = Arc::clone(&stderr);
        let stderr_reader = tokio::spawn(async move {
            let mut buffer = [0; 4096];
            while let Ok(size @ 1..) = pipe.read(&mut buffer).await {
                if let Ok(mut tail) = tail.lock() {
                    let excess = (tail.len() + size).saturating_sub(STDERR_BYTES);
                    tail.drain(..excess);
                    tail.extend_from_slice(&buffer[..size]);
                }
            }
        });
        let owned = Arc::new(Mutex::new(HashMap::new()));
        let descendants = owned.clone();
        let start = birth.clone();
        let watch = tokio::spawn(async move {
            loop {
                if let Ok(mut map) = descendants.lock() {
                    track(root, &start, &mut map);
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        });
        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout,
            root,
            birth,
            owned,
            watch,
            stderr,
            stderr_reader,
            stderr_destination: None,
            bytes: 0,
            pending: Vec::new(),
            clock: None,
        })
    }

    pub fn record_stderr(&mut self, state: &AppState, call: i64, secrets: &[String]) {
        self.stderr_destination = Some((state.clone(), call, secrets.to_vec()));
    }

    pub async fn send(&mut self, value: Value) -> Result<()> {
        let mut bytes = value.to_string().into_bytes();
        if bytes.len() > 2 * 1024 * 1024 {
            return Err(error(
                502,
                "Coding harness input exceeded its buffer limit.",
            ));
        }
        bytes.push(b'\n');
        self.write_input(&bytes).await
    }

    pub(crate) async fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        let stdin = self.stdin.as_mut().ok_or_else(stopped)?;
        tokio::time::timeout(Duration::from_secs(15), stdin.write_all(bytes))
            .await
            .map_err(|_| error(502, "Coding harness input timed out."))?
            .map_err(|_| stopped())
    }
    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }
    pub async fn cancel(mut self, notification: Value) {
        // Remember descendants before the notification lets the parent exit.
        if let Ok(mut owned) = self.owned.lock() {
            track(self.root, &self.birth, &mut owned);
        }
        let deadline = tokio::time::Instant::now() + Duration::from_millis(150);
        if tokio::time::timeout_at(deadline, self.send(notification))
            .await
            .is_ok_and(|result| result.is_ok())
        {
            tokio::time::sleep_until(deadline).await;
        }
    }
    pub async fn next(&mut self) -> Result<Value> {
        loop {
            let buf = self.stdout.fill_buf().await.map_err(|_| stopped())?;
            if buf.is_empty() {
                return Err(stopped());
            }
            let n = buf
                .iter()
                .position(|b| *b == b'\n')
                .map(|n| n + 1)
                .unwrap_or(buf.len());
            if self.pending.len() + n > 1024 * 1024 {
                return Err(error(502, "Coding harness output line exceeded 1 MiB."));
            }
            self.bytes += n;
            if self.bytes > 16 * 1024 * 1024 {
                return Err(error(502, "Coding harness output exceeded 16 MiB."));
            }
            self.pending.extend_from_slice(&buf[..n]);
            self.stdout.consume(n);
            if self.pending.last() == Some(&b'\n') {
                if self.pending.iter().all(u8::is_ascii_whitespace) {
                    self.pending.clear();
                    continue;
                }
                let value = serde_json::from_slice(&std::mem::take(&mut self.pending))
                    .map_err(|_| error(502, "Coding harness sent invalid protocol data."))?;
                if let Some(clock) = &self.clock {
                    clock.activity();
                }
                return Ok(value);
            }
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        self.watch.abort();
        self.stderr_reader.abort();
        if let Some((state, call, secrets)) = self.stderr_destination.take() {
            if let Ok(bytes) = self.stderr.lock() {
                let raw = if bytes.len() == STDERR_BYTES {
                    let start = bytes.iter().position(|byte| *byte == b'\n');
                    start.map_or(&[][..], |index| &bytes[index + 1..])
                } else {
                    &bytes
                };
                let raw = String::from_utf8_lossy(raw);
                let tail = raw.lines().rev().take(20).collect::<Vec<_>>();
                let tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
                let tail = crate::turns::diagnostics::text(&tail, &secrets);
                if !tail.is_empty() {
                    if let Ok(guard) = state.database.lock() {
                        let _ = guard.connection.execute(
                            "INSERT INTO harness_stderr(call,tail) VALUES(?,?)",
                            rusqlite::params![call, crate::turns::diagnostics::bounded(&tail)],
                        );
                    }
                }
            }
        }
        if let Ok(mut owned) = self.owned.lock() {
            track(self.root, &self.birth, &mut owned);
            for (pid, start) in owned.iter() {
                if process_identity(*pid).is_some_and(|v| v.1 == *start) {
                    signal(*pid);
                }
            }
        }
        if process_identity(self.root).is_some_and(|v| v.1 == self.birth) {
            if let Some(pid) = rustix::process::Pid::from_raw(self.root as i32) {
                let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
            }
        }
        let _ = self.child.start_kill();
    }
}
pub fn stopped() -> Failure {
    error(502,"Coding harness stopped before completion. Check workspace changes before retrying; this turn was not replayed.")
}
pub const HERMES_ENV: [(&str, &str); 3] = [
    ("HERMES_YOLO_MODE", "0"),
    ("HERMES_ACP_SKIP_CONFIGURED_MCP", "1"),
    ("HERMES_DISABLE_LAZY_INSTALLS", "1"),
];
pub const HERMES_STARTUP: Duration = Duration::from_secs(20);

pub struct ProbeHome(pub std::path::PathBuf);
impl ProbeHome {
    pub fn new() -> std::io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let path = std::env::temp_dir().join(format!("minarbot-probe-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }

    pub fn launch(
        &self,
        path: &Path,
        args: &[String],
        runtime: &str,
        hydrated_path: &str,
    ) -> Option<Process> {
        let scratch = self.0.to_str()?;
        let mut env = HashMap::from([
            ("HOME".into(), scratch.to_owned()),
            ("PATH".into(), hydrated_path.to_owned()),
        ]);
        let mise = mise_shim(path);
        if mise {
            configure_mise(&mut env);
        }
        if runtime != "hermes" {
            return Process::launch(path, args, &self.0, &env).ok();
        }
        if !LINUX_SANDBOX {
            return None;
        }
        env.insert("TMPDIR".into(), scratch.to_owned());
        env.extend(HERMES_ENV.map(|(key, value)| (key.into(), value.into())));

        // Hermes opens PM locks and generation leases even for metadata requests.
        let mut sandbox: Vec<String> = [
            "--die-with-parent",
            "--unshare-net",
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--ro-bind-try",
            "/dev/shm",
            "/dev/shm",
            "--bind",
            scratch,
            scratch,
        ]
        .map(str::to_owned)
        .into();
        if !mise {
            let home = std::env::home_dir().filter(|home| home.is_absolute())?;
            env.insert("HOME".into(), home.to_str()?.to_owned());
            let installs = home.join(".hermes/installs");
            for (index, install) in directories(&installs).enumerate() {
                let lock = install.join(".install.lock");
                if lock.is_file() {
                    let copy = self.0.join(format!("hermes-lock-{index}"));
                    File::create(&copy).ok()?;
                    sandbox.extend([
                        "--bind".into(),
                        copy.to_str()?.into(),
                        lock.to_str()?.into(),
                    ]);
                }
                for generation in directories(&install.join("environments")) {
                    let leases = generation.join(".leases");
                    if generation.join(".lease-managed").is_file() && leases.is_dir() {
                        sandbox.extend(["--tmpfs".into(), leases.to_str()?.into()]);
                    }
                }
            }
        }
        sandbox.extend(["--".into(), path.to_str()?.into()]);
        sandbox.extend_from_slice(args);
        Process::launch(Path::new("/usr/bin/bwrap"), &sandbox, &self.0, &env).ok()
    }
}

fn directories(path: &Path) -> impl Iterator<Item = std::path::PathBuf> {
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
}

impl Drop for ProbeHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub async fn probe(
    path: &Path,
    args: &[&str],
    runtime: &str,
    hydrated_path: &str,
) -> Option<String> {
    use tokio::io::AsyncReadExt;
    let home = ProbeHome::new().ok()?;
    let mut process = home.launch(
        path,
        &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        runtime,
        hydrated_path,
    )?;
    // Some CLIs (Gemini) read stdin before handling --version, so give them EOF.
    process.stdin = None;
    let budget = if runtime == "hermes" {
        HERMES_STARTUP
    } else {
        Duration::from_secs(10)
    };
    let result = tokio::time::timeout(budget, async {
        let mut bytes = Vec::new();
        (&mut process.stdout)
            .take(64001)
            .read_to_end(&mut bytes)
            .await
            .ok()?;
        if bytes.len() > 64000 || !process.child.wait().await.ok()?.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&bytes).into_owned())
    })
    .await
    .ok()
    .flatten();
    drop(process);
    result
}
pub async fn capture(
    path: &std::path::Path,
    args: &[String],
    env: &HashMap<String, String>,
    budget: Duration,
) -> Option<String> {
    use tokio::io::AsyncReadExt;
    let cwd = std::env::temp_dir();
    let mut process = Process::launch(path, args, &cwd, env).ok()?;
    let result = tokio::time::timeout(budget, async {
        let mut bytes = Vec::new();
        (&mut process.stdout)
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .ok()?;
        if bytes.len() > 1024 * 1024 || !process.child.wait().await.ok()?.success() {
            return None;
        }
        String::from_utf8(bytes).ok()
    })
    .await
    .ok()
    .flatten();
    drop(process);
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_read_preserves_partial_protocol_line() -> Result<()> {
        let home = ProbeHome::new().map_err(|_| Failure::storage())?;
        let mut process = Process::launch(
            std::path::Path::new("/bin/sh"),
            &[
                "-c".into(),
                r#"printf '{"ok":'; read -r reply; printf 'true}\n'"#.into(),
            ],
            &home.0,
            &HashMap::new(),
        )?;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), process.next())
                .await
                .is_err()
        );
        process.send(json!({})).await?;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), process.next())
                .await
                .map_err(|_| Failure::storage())??,
            json!({"ok":true})
        );
        Ok(())
    }
    #[tokio::test]
    async fn capture_bounds_output_and_cleans_up_hanging_children() -> crate::db::Result<()> {
        let home = ProbeHome::new()?;
        let env = HashMap::from([
            ("HOME".into(), home.0.to_string_lossy().into_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ]);
        let shell = std::path::Path::new("/bin/sh");
        assert_eq!(
            capture(
                shell,
                &["-c".into(), "printf fixture".into()],
                &env,
                Duration::from_secs(2)
            )
            .await
            .as_deref(),
            Some("fixture")
        );
        assert!(capture(
            shell,
            &["-c".into(), "head -c 1048577 /dev/zero".into()],
            &env,
            Duration::from_secs(2)
        )
        .await
        .is_none());
        assert!(capture(
            shell,
            &["-c".into(), "sleep 10 & wait".into()],
            &env,
            Duration::from_millis(50)
        )
        .await
        .is_none());
        Ok(())
    }
}
