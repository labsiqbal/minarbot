//! Slack and Telegram connector processes (`minarbot-server
//! --connector-config` and `--connector-journal-config`), the Rust form of
//! `app/rust-connectors-runner.ts`.

use crate::{server::server_binary, Result};
use serde_json::Value;
use std::{
    process::{ExitStatus, Stdio},
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
};

fn command(flag: &str) -> Result<Command> {
    let mut command = Command::new(server_binary(None)?);
    command
        .arg(flag)
        .env_clear()
        .env("PATH", crate::server::HERMETIC_PATH)
        .env("MINARBOT_TEST_SERVER", "rust")
        .env("LANG", "C.UTF-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    Ok(command)
}

/// A running connector. Dropping it kills the process.
pub struct Connector {
    child: Child,
    stderr: Arc<Mutex<String>>,
    status: Option<ExitStatus>,
}

impl Connector {
    /// Start a connector with `options` (`{platform, directory, scriptDirectory,
    /// agent, origin, users, ...}`) on stdin.
    pub async fn start(options: &Value) -> Result<Self> {
        let mut child = command("--connector-config")?.spawn()?;
        let mut stdin = child.stdin.take().ok_or("connector stdin")?;
        stdin.write_all(options.to_string().as_bytes()).await?;
        drop(stdin);
        let stderr = Arc::new(Mutex::new(String::new()));
        if let Some(mut pipe) = child.stderr.take() {
            let tail = stderr.clone();
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                while let Ok(read) = pipe.read(&mut buffer).await {
                    if read == 0 {
                        break;
                    }
                    if let Ok(mut tail) = tail.lock() {
                        tail.push_str(&String::from_utf8_lossy(&buffer[..read]));
                        let excess = tail.len().saturating_sub(4096);
                        tail.drain(..excess);
                    }
                }
            });
        }
        Ok(Self {
            child,
            stderr,
            status: None,
        })
    }

    pub fn stderr(&self) -> String {
        self.stderr
            .lock()
            .map(|tail| tail.clone())
            .unwrap_or_default()
    }

    /// Exit status if the process already stopped on its own.
    pub fn exited(&mut self) -> Result<Option<ExitStatus>> {
        if self.status.is_none() {
            self.status = self.child.try_wait()?;
        }
        Ok(self.status)
    }

    /// Fail when the connector stopped on its own (a fixture `check`).
    pub fn check(&mut self) -> Result<()> {
        match self.exited()? {
            Some(status) => Err(format!("connector stopped ({status}): {}", self.stderr()).into()),
            None => Ok(()),
        }
    }

    /// SIGTERM and wait; returns the exit status (the Node `abort`).
    pub async fn stop(&mut self) -> Result<ExitStatus> {
        if let Some(status) = self.exited()? {
            return Ok(status);
        }
        if let Some(pid) = self.child.id() {
            let pid = rustix::process::Pid::from_raw(i32::try_from(pid)?).ok_or("connector pid")?;
            rustix::process::kill_process(pid, rustix::process::Signal::TERM)?;
        }
        let status = tokio::time::timeout(crate::WAIT, self.child.wait()).await??;
        self.status = Some(status);
        Ok(status)
    }

    /// Wait until the connector exits by itself; returns its stderr.
    pub async fn failure(&mut self) -> Result<String> {
        let status = tokio::time::timeout(crate::WAIT, self.child.wait())
            .await
            .map_err(|_| "connector did not stop after the injected failure")??;
        self.status = Some(status);
        if status.success() {
            return Err("connector exited successfully instead of failing".into());
        }
        // Let the stderr reader drain.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        Ok(self.stderr())
    }
}

/// Run one journal command (`{directory, platform, command, ...}`); a non-zero
/// exit is an error carrying stderr.
pub async fn journal(command_input: &Value) -> Result<Value> {
    let mut child = command("--connector-journal-config")?.spawn()?;
    let mut stdin = child.stdin.take().ok_or("journal stdin")?;
    stdin
        .write_all(command_input.to_string().as_bytes())
        .await?;
    drop(stdin);
    let output = child.wait_with_output().await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if stderr.is_empty() {
            "Rust connector journal command failed.".into()
        } else {
            stderr.into()
        });
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}
