//! Locating, installing and reading the logs of the fake harness peers.
//!
//! The peers are binaries of this crate (`src/bin/fake-*.rs`, logic in
//! [`crate::fake`]). Build them with the server before running e2e tests:
//! `cargo build -p minarbot-server -p minarbot-testkit`.

use crate::Result;
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

/// The four fake peers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fake {
    /// Codex `app-server` JSON-RPC peer with scripted, prompt-keyed turns.
    Codex,
    /// Claude Code `--input-format stream-json` peer with scripted turns.
    Claude,
    /// Agent Client Protocol peer (and Pi RPC peer).
    Acp,
    /// Codex `app-server` peer that asks an OpenAI-compatible model.
    Gateway,
}

impl Fake {
    pub fn binary_name(self) -> &'static str {
        match self {
            Self::Codex => "fake-codex",
            Self::Claude => "fake-claude",
            Self::Acp => "fake-acp",
            Self::Gateway => "fake-gateway",
        }
    }

    /// Native peer for a harness runtime name (`codex` or `claude`).
    pub fn native(runtime: &str) -> Result<Self> {
        match runtime {
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            other => Err(format!("no native fake peer for runtime {other}").into()),
        }
    }
}

/// `target/<profile>` of the running test executable (`target/debug/deps/x`).
pub fn target_dir() -> Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let mut directory = executable
        .parent()
        .ok_or("test executable has no parent directory")?;
    if directory.file_name().is_some_and(|name| name == "deps") {
        directory = directory.parent().ok_or("deps directory has no parent")?;
    }
    Ok(directory.to_owned())
}

/// Path of a built fake peer (`MINARBOT_FAKE_BIN_DIR` overrides discovery).
pub fn path(fake: Fake) -> Result<PathBuf> {
    let directory = match std::env::var_os("MINARBOT_FAKE_BIN_DIR") {
        Some(directory) => PathBuf::from(directory),
        None => target_dir()?,
    };
    let binary = directory.join(fake.binary_name());
    if binary.is_file() {
        Ok(binary)
    } else {
        Err(format!(
            "{} is missing: run `cargo build -p minarbot-testkit` (it builds the fake peers)",
            binary.display()
        )
        .into())
    }
}

/// Copy a fake into `directory` under `name`, so a test can delete or
/// replace that executable without touching the build output.
pub fn install(fake: Fake, directory: impl AsRef<Path>, name: &str) -> Result<PathBuf> {
    let source = path(fake)?;
    let target = directory.as_ref().join(name);
    fs::copy(&source, &target)?;
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700))?;
    Ok(target)
}

/// JSON-lines log a fake writes in its working directory.
pub fn read_log(path: impl AsRef<Path>) -> Result<Vec<Value>> {
    let path = path.as_ref();
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{}: {error}", path.display()).into()),
    };
    // A peer may be appending right now: ignore an unterminated last line.
    let complete = match text.rfind('\n') {
        Some(end) => &text[..=end],
        None => "",
    };
    complete
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| Ok(serde_json::from_str(line)?))
        .collect()
}

/// `fake-log.jsonl` of the native Codex/Claude peers.
pub fn native_log(workspace: impl AsRef<Path>) -> Result<Vec<Value>> {
    read_log(workspace.as_ref().join(crate::fake::native::LOG))
}

/// `fake-acp-log.jsonl` of the ACP peer.
pub fn acp_log(workspace: impl AsRef<Path>) -> Result<Vec<Value>> {
    read_log(workspace.as_ref().join(crate::fake::acp::LOG))
}

/// `fake-gateway-log.jsonl` of the gateway peer.
pub fn gateway_log(workspace: impl AsRef<Path>) -> Result<Vec<Value>> {
    read_log(workspace.as_ref().join(crate::fake::gateway::LOG))
}

/// Entries of a native log whose `type` is `turn`.
pub fn turns(log: &[Value]) -> Vec<Value> {
    log.iter()
        .filter(|row| row["type"] == "turn")
        .cloned()
        .collect()
}
