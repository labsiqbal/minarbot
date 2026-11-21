use crate::{
    data::{all, closed, error, exists, field, iso, one, string, transaction, Result},
    json_response, operator_file,
    web::Failure,
    AppState,
};
use axum::{http::Method, response::Response};
use rusqlite::{params, Connection};
use rustix::fs::{mkdirat, Mode};
use serde_json::{json, Value};
use std::{
    ffi::OsStr,
    fs::{self, File},
    io::ErrorKind,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;

pub(crate) const PREFIX: &str = "login:";
/// Binds a Codex or Claude Code agent to the sign-in already set up on this
/// machine. The engine then reads its own config folder, shared with the CLI,
/// so token refreshes by either side stay in one file under one lock.
pub(crate) const DEVICE: &str = "device";
pub(crate) const DEVICE_RUNTIME_ONLY: &str =
    "This machine's sign-in works only with Codex or Claude Code.";
pub(crate) const CODEX_CONFIG: &[u8] = b"cli_auth_credentials_store = \"file\"\n";

const SELECT: &str = "SELECT id,runtime,name,created,checked_at,signed_in FROM engine_logins";

pub(crate) fn bound(db: &Connection, binding: &str) -> Result<Option<Value>> {
    match binding.strip_prefix(PREFIX) {
        Some(id) => one(db, &format!("{SELECT} WHERE id=?"), [id]),
        None => Ok(None),
    }
}

pub(crate) fn blocked_reason(runtime: &str, profile: Option<&Value>) -> Option<&'static str> {
    match profile {
        None => Some("This agent's engine login is unavailable."),
        Some(profile) if profile["runtime"] != runtime => {
            Some("Choose an engine login for this runtime.")
        }
        Some(profile) if profile["signed_in"] != 1 => {
            Some("Check this login in Settings > Engines.")
        }
        _ => None,
    }
}

fn data_root(state: &AppState) -> Result<PathBuf> {
    state
        .config
        .database
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .map_err(|_| Failure::storage())
}

fn child(parent: &File, name: &str, create: bool) -> std::io::Result<File> {
    if create {
        match mkdirat(parent, name, Mode::RWXU) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => (),
            Err(error) => return Err(error.into()),
        }
    }
    let directory = operator_file::directory_at(parent, name)?;
    if !operator_file::private(&directory) {
        return Err(ErrorKind::PermissionDenied.into());
    }
    Ok(directory)
}

fn runtime_directory(root: &Path, runtime: &str, create: bool) -> std::io::Result<File> {
    let data = operator_file::open_directory(root)?;
    child(&child(&data, "engine-logins", create)?, runtime, create)
}

fn runtime_file(runtime: &str) -> (&'static str, &'static str) {
    if runtime == "codex" {
        ("CODEX_HOME", "auth.json")
    } else {
        ("CLAUDE_CONFIG_DIR", ".credentials.json")
    }
}

pub(crate) fn credentials(state: &AppState, profile: &Value) -> Result<Option<Vec<u8>>> {
    let runtime = string(profile, "runtime");
    let id = string(profile, "id");
    let (_, name) = runtime_file(runtime);
    let root = data_root(state)?;
    let result = (|| {
        let parent = runtime_directory(&root, runtime, false)?;
        let directory = child(&parent, id, false)?;
        let file = operator_file::file_at(&directory, OsStr::new(name))?;
        operator_file::bounded_read(file, 1024 * 1024)
    })();
    match result {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(_) => Err(error(
            409,
            "Engine login credentials must be a regular file in the private profile directory.",
        )),
    }
}

pub(crate) fn device_runtime(runtime: &str) -> bool {
    matches!(runtime, "codex" | "claude")
}

/// The engine's own config folder on this machine, where the CLI keeps its sign-in.
pub(crate) fn device_folder(runtime: &str) -> Option<PathBuf> {
    if !device_runtime(runtime) {
        return None;
    }
    let (variable, _) = runtime_file(runtime);
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

/// Environment that makes the engine read its sign-in from the device folder.
/// Claude Code keeps its global state in `~/.claude.json` beside `~/.claude`
/// and moves it under `CLAUDE_CONFIG_DIR` once that is set, so Claude runs with
/// the real HOME and inherits `CLAUDE_CONFIG_DIR` only when the user set it.
pub(crate) fn device_environment(runtime: &str) -> Vec<(&'static str, PathBuf)> {
    let (variable, _) = runtime_file(runtime);
    match runtime {
        "codex" => device_folder(runtime)
            .map(|folder| vec![(variable, folder)])
            .unwrap_or_default(),
        "claude" => std::env::var_os(variable)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .map(|folder| vec![(variable, folder)])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Reads the device sign-in so harness output can redact its tokens.
pub(crate) fn device_credentials(runtime: &str) -> Option<Vec<u8>> {
    let path = device_folder(runtime)?.join(credential_name(runtime));
    let file = File::open(path).ok()?;
    operator_file::bounded_read(file, 1024 * 1024).ok()
}

/// Whether Codex or Claude Code is signed in on this machine, using the CLI's own
/// status command. `None` when the check cannot run or gives no clear answer.
pub(crate) async fn device_signed_in(
    state: &AppState,
    executable: &Path,
    runtime: &str,
) -> Option<bool> {
    let (home, mut env) = device_process_environment(runtime, crate::harness::path(state))?;
    crate::harness::mise_environment(executable, &mut env);
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
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let status = match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
        Ok(status) => status.ok()?,
        Err(_) => {
            if let Some(pid) = child
                .id()
                .and_then(|pid| rustix::process::Pid::from_raw(pid as i32))
            {
                let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
            }
            return None;
        }
    };
    match status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

pub(crate) fn device_process_environment(
    runtime: &str,
    path: String,
) -> Option<(PathBuf, std::collections::HashMap<String, String>)> {
    device_folder(runtime)?;
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

pub(crate) fn credential_name(runtime: &str) -> &'static str {
    runtime_file(runtime).1
}

pub(crate) fn secrets(runtime: &str, bytes: &[u8]) -> Vec<String> {
    let Ok(auth) = serde_json::from_slice::<Value>(bytes) else {
        return Vec::new();
    };
    let (tokens, fields): (&Value, &[&str]) = if runtime == "codex" {
        (
            &auth["tokens"],
            &["access_token", "refresh_token", "id_token"],
        )
    } else {
        (&auth["claudeAiOauth"], &["accessToken", "refreshToken"])
    };
    fields
        .iter()
        .map(|field| &tokens[*field])
        .chain(auth.get("OPENAI_API_KEY"))
        .filter_map(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn profile_path(root: &Path, profile: &Value) -> PathBuf {
    root.join("engine-logins")
        .join(string(profile, "runtime"))
        .join(string(profile, "id"))
}

pub(crate) fn codex_config(state: &AppState, profile: &Value) -> Option<Vec<u8>> {
    let root = data_root(state).ok()?;
    let parent = runtime_directory(&root, "codex", false).ok()?;
    let directory = child(&parent, string(profile, "id"), false).ok()?;
    let file = operator_file::file_at(&directory, OsStr::new("config.toml")).ok()?;
    operator_file::bounded_read(file, 1024 * 1024).ok()
}

fn public(root: &Path, profile: &Value) -> Result<Value> {
    let id = string(profile, "id");
    let runtime = string(profile, "runtime");
    let path = profile_path(root, profile);
    let path = path
        .to_str()
        .filter(|path| !path.contains(['\r', '\n']))
        .ok_or_else(|| {
            error(
                409,
                "Engine login path cannot be used in a terminal command.",
            )
        })?;
    let quoted = format!("'{}'", path.replace('\'', "'\\''"));
    let (variable, _) = runtime_file(runtime);
    let login = if runtime == "codex" {
        "codex login"
    } else {
        "claude auth login"
    };
    Ok(json!({
        "id":id, "runtime":runtime, "name":profile["name"], "created":profile["created"],
        "checkedAt":profile["checked_at"], "signedIn":profile["signed_in"] == 1,
        "command":format!("{variable}={quoted} {login}")
    }))
}

fn add(state: &AppState, root: &Path, body: &Value) -> Result<Response> {
    let runtime = string(body, "runtime");
    let name = field(body, "name", 80)?;
    if !closed(body, &["id", "runtime", "name"])
        || !["claude", "codex"].contains(&runtime)
        || name.chars().any(char::is_control)
    {
        return Err(error(400, "Invalid engine login profile."));
    }
    let id = string(body, "id");
    if crate::web::id(id).is_none() {
        return Err(error(400, "Invalid engine login request ID."));
    }
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    if let Some(profile) = one(db, &format!("{SELECT} WHERE id=?"), [id])? {
        if profile["runtime"] != runtime || profile["name"] != name {
            return Err(error(
                409,
                "Request ID was already used for another engine login.",
            ));
        }
        return Ok(json_response(200, public(root, &profile)?));
    }
    let profile =
        json!({"id":id,"runtime":runtime,"name":name,"created":iso(db,state.config.now())?});
    let result = public(root, &profile)?;
    let parent = runtime_directory(root, runtime, true).map_err(|_| Failure::storage())?;
    mkdirat(&parent, id, Mode::RWXU).map_err(|_| Failure::storage())?;
    let saved = (|| {
        if runtime == "codex" {
            let dir = child(&parent, id, false).map_err(|_| Failure::storage())?;
            operator_file::publish(&dir, OsStr::new("config.toml"), CODEX_CONFIG)
                .map_err(|_| Failure::storage())?;
        }
        db.execute(
            "INSERT INTO engine_logins(id,runtime,name,created) VALUES(?,?,?,?)",
            params![id, runtime, name, profile["created"].as_str()],
        )?;
        Ok(json_response(201, result))
    })();
    if saved.is_err() {
        let _ = fs::remove_dir_all(profile_path(root, &profile));
    }
    saved
}

async fn check(state: &AppState, root: &Path, profile: &Value) -> Result<Response> {
    let runtime = string(profile, "runtime");
    let id = string(profile, "id");
    let has_credential = credentials(state, profile)?.is_some();
    let executable = crate::harness::executable(state, runtime)
        .ok_or_else(|| error(503, "This engine is unavailable on this host."))?;
    let path = profile_path(root, profile);
    let (variable, _) = runtime_file(runtime);
    let args: &[&str] = if runtime == "codex" {
        &["login", "status"]
    } else {
        &["auth", "status", "--json"]
    };
    let mut child = Command::new(executable)
        .args(args)
        .env_clear()
        .env("PATH", crate::harness::path(state))
        .env("HOME", &path)
        .env(variable, &path)
        .current_dir(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| error(502, "Engine sign-in check could not start."))?;
    let status = match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
        Ok(status) => status.map_err(|_| error(502, "Engine sign-in check failed."))?,
        Err(_) => {
            if let Some(pid) = child
                .id()
                .and_then(|pid| rustix::process::Pid::from_raw(pid as i32))
            {
                let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
            }
            return Err(error(504, "Engine sign-in check timed out."));
        }
    };
    let signed_in = match status.code() {
        Some(0) => has_credential,
        Some(1) => false,
        _ => return Err(error(502, "Engine sign-in check failed.")),
    };
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    let checked_at = iso(db, state.config.now())?;
    db.execute(
        "UPDATE engine_logins SET checked_at=?,signed_in=? WHERE id=? AND created=? AND checked_at IS ?",
        params![checked_at, signed_in, id, profile["created"].as_str(), profile["checked_at"].as_str()],
    )?;
    let current = one(db, &format!("{SELECT} WHERE id=?"), [id])?
        .ok_or_else(|| error(404, "Engine login profile not found."))?;
    Ok(json_response(200, public(root, &current)?))
}

fn remove(root: &Path, db: &Connection, profile: &Value) -> Result<()> {
    let id = string(profile, "id");
    let runtime = string(profile, "runtime");
    let binding = format!("{PREFIX}{id}");
    if exists(
        db,
        "SELECT 1 FROM agents WHERE provider_account=? AND id NOT IN (SELECT id FROM deleted_agents)
         AND (id<>? OR EXISTS(SELECT 1 FROM turns WHERE agent=agents.id AND status IN('queued','running'))) LIMIT 1",
        [&binding, crate::mcp::guide::ID],
    )? {
        return Err(error(409, "Unbind this login from its agents before removing it."));
    }
    match runtime_directory(root, runtime, false) {
        Ok(_) => match fs::remove_dir_all(profile_path(root, profile)) {
            Ok(()) => (),
            Err(error) if error.kind() == ErrorKind::NotFound => (),
            Err(_) => return Err(Failure::storage()),
        },
        Err(error) if error.kind() == ErrorKind::NotFound => (),
        Err(_) => return Err(Failure::storage()),
    }
    db.execute("DELETE FROM engine_logins WHERE id=?", [id])?;
    crate::mcp::guide::detach_connection(db, &binding)?;
    Ok(())
}

pub(crate) async fn route(
    state: &AppState,
    parts: &[&str],
    method: &Method,
    query: &[(String, String)],
    body: &Value,
) -> Result<Response> {
    if !query.is_empty() {
        return Err(error(400, "Invalid engine login parameters."));
    }
    let root = data_root(state)?;
    match (parts, method) {
        (["api", "engine-logins"], &Method::GET) => {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            let profiles = all(
                &guard.connection,
                &format!("{SELECT} ORDER BY created,id"),
                [],
            )?
            .iter()
            .map(|profile| public(&root, profile))
            .collect::<Result<Vec<_>>>()?;
            Ok(json_response(200, json!(profiles)))
        }
        (["api", "engine-logins"], &Method::POST) => add(state, &root, body),
        (["api", "engine-logins", id, action @ ("check" | "remove")], &Method::POST) => {
            if !closed(body, &["id"]) || !crate::data::uuid(id) {
                return Err(error(400, "Invalid engine login request."));
            }
            let profile = {
                let guard = state.database.lock().map_err(|_| Failure::storage())?;
                let db = &guard.connection;
                let profile = one(db, &format!("{SELECT} WHERE id=?"), [id])?
                    .ok_or_else(|| error(404, "Engine login profile not found."))?;
                if *action == "remove" {
                    transaction(db, || remove(&root, db, &profile))?;
                }
                profile
            };
            if *action == "check" {
                return check(state, &root, &profile).await;
            }
            state
                .config
                .harness
                .homes
                .lock()
                .await
                .retain(|_, home| home.login_id.as_deref() != Some(id));
            Ok(json_response(200, json!({"removed":true})))
        }
        _ => Err(error(405, "Use GET or POST for engine logins.")),
    }
}
