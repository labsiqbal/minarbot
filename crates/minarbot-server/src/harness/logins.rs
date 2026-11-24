#![allow(dead_code)]

use crate::{harness, AppState};
use std::{
    fs::File,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;

pub(crate) const PREFIX: &str = "login:";
pub(crate) const DEVICE: &str = "device";
pub(crate) const DEVICE_RUNTIME_ONLY: &str =
    "This machine's sign-in works only with Codex or Claude Code.";
pub(crate) const CODEX_CONFIG: &[u8] = b"cli_auth_credentials_store = \"file\"\n";

pub(crate) fn device_runtime(runtime: &str) -> bool {
    matches!(runtime, "codex" | "claude")
}

pub(crate) fn device_folder(runtime: &str) -> Option<PathBuf> {
    if !device_runtime(runtime) {
        return None;
    }
    let variable = if runtime == "codex" {
        "CODEX_HOME"
    } else {
        "CLAUDE_CONFIG_DIR"
    };
    std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let folder = if runtime == "codex" {
                ".codex"
            } else {
                ".claude"
            };
            std::env::var_os("HOME").map(|home| Path::new(&home).join(folder))
        })
        .filter(|path| path.is_absolute())
}

pub(crate) fn device_environment(runtime: &str) -> Vec<(&'static str, PathBuf)> {
    let variable = if runtime == "codex" {
        "CODEX_HOME"
    } else {
        "CLAUDE_CONFIG_DIR"
    };
    device_folder(runtime)
        .map(|folder| vec![(variable, folder)])
        .unwrap_or_default()
}

pub(crate) fn device_credentials(runtime: &str) -> Option<Vec<u8>> {
    let name = credential_name(runtime);
    let file = File::open(device_folder(runtime)?.join(name)).ok()?;
    crate::operator_file::bounded_read(file, 1024 * 1024).ok()
}

pub(crate) fn credential_name(runtime: &str) -> &'static str {
    if runtime == "codex" {
        "auth.json"
    } else {
        ".credentials.json"
    }
}

pub(crate) fn secrets(_runtime: &str, _bytes: &[u8]) -> Vec<String> {
    Vec::new()
}

pub(crate) fn device_process_environment(
    runtime: &str,
    path: String,
) -> Option<(PathBuf, std::collections::HashMap<String, String>)> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let mut env = std::collections::HashMap::from([
        ("PATH".into(), path),
        ("HOME".into(), home.to_string_lossy().into_owned()),
    ]);
    env.extend(
        device_environment(runtime)
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value.to_string_lossy().into_owned())),
    );
    Some((home, env))
}

pub(crate) async fn device_signed_in(
    state: &AppState,
    executable: &Path,
    runtime: &str,
) -> Option<bool> {
    let (home, mut env) = device_process_environment(runtime, harness::path(state))?;
    harness::mise_environment(executable, &mut env);
    let args: &[&str] = if runtime == "codex" {
        &["login", "status"]
    } else {
        &["auth", "status", "--json"]
    };
    let mut child = Command::new(executable)
        .args(args)
        .env_clear()
        .envs(&env)
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
        Ok(Ok(status)) => match status.code() {
            Some(0) => Some(true),
            Some(1) => Some(false),
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn blocked_reason(
    _runtime: &str,
    _profile: Option<&serde_json::Value>,
) -> Option<&'static str> {
    Some("Choose a provider instance.")
}

pub(crate) fn bound(
    _db: &rusqlite::Connection,
    _binding: &str,
) -> crate::data::Result<Option<serde_json::Value>> {
    Ok(None)
}

pub(crate) fn credentials(
    _state: &AppState,
    _profile: &serde_json::Value,
) -> crate::data::Result<Option<Vec<u8>>> {
    Ok(None)
}

pub(crate) fn codex_config(_state: &AppState, _profile: &serde_json::Value) -> Option<Vec<u8>> {
    None
}
