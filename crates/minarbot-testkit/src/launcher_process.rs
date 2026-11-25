//! The real `minarbot-launcher` as a child process (the Node
//! `launcherCommand` spawn in `app/github-updates.test.ts`).

use crate::Result;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};

/// Locate `minarbot-launcher`: `MINARBOT_LAUNCHER_BINARY`, then the binary
/// next to the running test executable's target directory.
pub fn launcher_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("MINARBOT_LAUNCHER_BINARY") {
        return Ok(PathBuf::from(path));
    }
    let binary = crate::fakes::target_dir()?.join("minarbot-launcher");
    if binary.is_file() {
        return Ok(binary);
    }
    Err(format!(
        "{} is missing: run `cargo build -p minarbot-launcher` before integration tests \
         or set MINARBOT_LAUNCHER_BINARY",
        binary.display()
    )
    .into())
}

/// A running launcher; stopped with SIGTERM (its server child follows
/// through the launcher's parent-death signal).
pub struct LauncherProcess {
    child: Child,
    logs: Arc<Mutex<String>>,
}

impl LauncherProcess {
    /// `minarbot-launcher --config <config>` with `HOME` and test markers.
    pub fn start(config: &Path, home: &Path) -> Result<Self> {
        let mut child = Command::new(launcher_binary()?)
            .arg("--config")
            .arg(config)
            .env("HOME", home)
            .env("MINARBOT_TEST_SERVER", "rust")
            .env("MINARBOT_TEST_PARENT", std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let logs = Arc::new(Mutex::new(String::new()));
        let stderr = child.stderr.take().ok_or("launcher stderr")?;
        let tail = logs.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut logs = match tail.lock() {
                    Ok(logs) => logs,
                    Err(poisoned) => poisoned.into_inner(),
                };
                logs.push_str(&line);
                logs.push('\n');
            }
        });
        Ok(Self { child, logs })
    }

    pub fn logs(&self) -> String {
        match self.logs.lock() {
            Ok(logs) => logs.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Exit status if the launcher already exited.
    pub fn exited(&mut self) -> Result<Option<std::process::ExitStatus>> {
        Ok(self.child.try_wait()?)
    }

    /// SIGTERM, then SIGKILL after 10 s.
    pub async fn stop(&mut self) -> Result<()> {
        if let Some(pid) = self.child.id().and_then(|pid| rustix::process::Pid::from_raw(pid as i32)) {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
        }
        if tokio::time::timeout(Duration::from_secs(10), self.child.wait()).await.is_err() {
            let _ = self.child.start_kill();
            let _ = self.child.wait().await;
        }
        Ok(())
    }
}
