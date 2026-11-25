//! `minarbot-server script ...`, the agent script CLI.
//!
//! The CLI discovers the server through `connection.json` in the script
//! directory (the server's `scriptDirectory` option). Point it at a
//! [`crate::proxy::LossyProxy`] with [`Script::use_url`] to lose replies.

use crate::{server::server_binary, Result};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncWriteExt, process::Command};

/// Result of one CLI run. `json` is the single JSON document on stdout;
/// its `exitCode` always equals the process exit status (checked).
#[derive(Clone, Debug)]
pub struct Run {
    pub code: i32,
    pub json: Value,
    pub stdout: String,
}

impl std::ops::Index<&str> for Run {
    type Output = Value;
    fn index(&self, key: &str) -> &Value {
        &self.json[key]
    }
}

/// CLI bound to one script directory.
#[derive(Clone, Debug)]
pub struct Script {
    directory: PathBuf,
    binary: PathBuf,
}

impl Script {
    pub fn new(directory: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            directory: directory.as_ref().to_owned(),
            binary: server_binary(None)?,
        })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn command<S: AsRef<str>>(&self, args: &[S]) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .arg("script")
            .args(args.iter().map(AsRef::as_ref))
            .env_clear()
            .env("PATH", crate::server::HERMETIC_PATH)
            .env("MINARBOT_SCRIPT_DIRECTORY", &self.directory)
            .env("LANG", "C.UTF-8")
            .current_dir(self.directory.parent().unwrap_or(&self.directory))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command
    }

    /// Run with stdin closed.
    pub async fn run<S: AsRef<str>>(&self, args: &[S]) -> Result<Run> {
        self.run_chunks(args, &[]).await
    }

    /// Run with `input` on stdin.
    pub async fn run_input<S: AsRef<str>>(&self, args: &[S], input: &[u8]) -> Result<Run> {
        self.run_chunks(args, &[(input, Duration::ZERO)]).await
    }

    /// Write each chunk after its delay, then close stdin.
    pub async fn run_chunks<S: AsRef<str>>(
        &self,
        args: &[S],
        chunks: &[(&[u8], Duration)],
    ) -> Result<Run> {
        let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
        let stdout = self.raw_chunks(&args, chunks).await?;
        let (status, stdout) = stdout;
        let json: Value = serde_json::from_str(&stdout)
            .map_err(|error| format!("script {args:?}: stdout is not JSON ({error}): {stdout}"))?;
        let code = status;
        if json["exitCode"].as_i64() != Some(i64::from(code)) {
            return Err(format!(
                "script {args:?}: exitCode {} differs from process status {code}",
                json["exitCode"]
            )
            .into());
        }
        Ok(Run { code, json, stdout })
    }

    /// Re-run `args` until `done(&run)` holds; the timeout shows the last run.
    pub async fn wait<S: AsRef<str>>(
        &self,
        args: &[S],
        done: impl Fn(&Run) -> bool,
    ) -> Result<Run> {
        let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
        let args = args.as_slice();
        crate::wait::until_value(format!("script {args:?}"), || self.run(args), done).await
    }

    /// Exit status and raw stdout.
    pub async fn raw<S: AsRef<str>>(&self, args: &[S]) -> Result<(i32, String)> {
        let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
        self.raw_chunks(&args, &[]).await
    }

    async fn raw_chunks(
        &self,
        args: &[&str],
        chunks: &[(&[u8], Duration)],
    ) -> Result<(i32, String)> {
        let mut command = self.command(args);
        command.stdin(if chunks.is_empty() {
            Stdio::null()
        } else {
            Stdio::piped()
        });
        let mut child = command.spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            for (bytes, delay) in chunks {
                tokio::time::sleep(*delay).await;
                stdin.write_all(bytes).await?;
                stdin.flush().await?;
            }
        }
        let output = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output())
            .await
            .map_err(|_| format!("script {args:?} did not exit within 60s"))??;
        let code = output.status.code().ok_or("script CLI killed by signal")?;
        Ok((code, String::from_utf8(output.stdout)?))
    }

    /// Parsed `connection.json` written by the server.
    pub fn connection(&self) -> Result<Value> {
        Ok(serde_json::from_slice(&fs::read(
            self.directory.join("connection.json"),
        )?)?)
    }

    /// Replace `connection.json`.
    pub fn write_connection(&self, value: &Value) -> Result<()> {
        Ok(fs::write(
            self.directory.join("connection.json"),
            serde_json::to_vec(value)?,
        )?)
    }

    /// Keep the token but send requests to `url` (for example a proxy).
    pub fn use_url(&self, url: &str) -> Result<()> {
        let mut connection = self.connection()?;
        connection["url"] = Value::String(url.to_owned());
        self.write_connection(&connection)
    }
}
