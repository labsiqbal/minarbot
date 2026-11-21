pub mod config;
mod fs;
pub(crate) mod model_catalog;
pub(crate) mod pdf;
pub(crate) mod process;
mod protocol;
mod snapshot;
mod store;

use crate::{
    data::{self, all, error, one, string},
    web::Failure,
    AppState,
};
use axum::{extract::Request, http::Method, response::Response, routing::any, Router};
use futures_util::future::join_all;
use serde_json::{json, Value};
use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    time::{Duration, Instant},
};
pub type Result<T> = std::result::Result<T, Failure>;
pub const RUNTIMES: [&str; 9] = [
    "codex",
    "claude",
    "hermes",
    "antigravity",
    "grok",
    "pi",
    "opencode",
    "gemini",
    "kiro",
];
// Hermes can spend 20 seconds on each read and 15 on the protocol write.
const CATALOG_TIMEOUT: Duration = Duration::from_secs(55);
pub fn label(runtime: &str) -> &str {
    match runtime {
        "claude" => "Claude Code",
        "codex" => "Codex",
        "hermes" => "Hermes",
        "antigravity" => "Antigravity",
        "grok" => "Grok",
        "pi" => "Pi",
        "opencode" => "OpenCode",
        "gemini" => "Gemini",
        "kiro" => "Kiro",
        _ => runtime,
    }
}
pub fn capabilities(runtime: &str) -> Value {
    json!({"steer":false,"interrupt":true,"resume":runtime!="pi","images":false,"mcp":true,"approvals":true})
}
pub fn initialize(db: &rusqlite::Connection) -> Result<()> {
    db.execute_batch(include_str!("schema.sql"))?;
    Ok(())
}
pub async fn hydrate(config: &mut config::Config) {
    let inherited = std::env::var("PATH").unwrap_or_default();
    config.environment.insert("PATH".into(), inherited.clone());
    if config.test_mode {
        return;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let shells = [
        std::env::var("SHELL").unwrap_or_default(),
        "/bin/bash".into(),
    ];
    for shell in shells {
        let path = std::path::Path::new(&shell);
        if !path.is_absolute() {
            continue;
        }
        let budget = deadline.saturating_duration_since(tokio::time::Instant::now());
        if budget.is_zero() {
            break;
        }
        let command =
            "printf '__MINARBOT_PATH_START__\\n'; printenv PATH; printf '__MINARBOT_PATH_END__\\n'";
        if let Some(raw) = process::capture(
            path,
            &["-ilc".into(), command.into()],
            &config.environment,
            budget,
        )
        .await
        {
            if let Some(value) = raw
                .split_once("__MINARBOT_PATH_START__")
                .and_then(|(_, s)| s.split_once("__MINARBOT_PATH_END__"))
                .map(|(v, _)| v.trim())
                .filter(|v| !v.is_empty() && !v.contains('\0'))
            {
                let mut entries = Vec::new();
                for entry in value
                    .split(':')
                    .chain(inherited.split(':'))
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                {
                    if !entries.contains(&entry) {
                        entries.push(entry);
                    }
                }
                config.environment.insert("PATH".into(), entries.join(":"));
                return;
            }
        }
    }
    eprintln!("[server] Login environment hydration failed; keeping inherited environment.");
}
pub fn path(state: &AppState) -> String {
    state
        .config
        .harness
        .environment
        .get("PATH")
        .cloned()
        .unwrap_or_else(|| std::env::var("PATH").unwrap_or_default())
}
pub fn executable(state: &AppState, runtime: &str) -> Option<PathBuf> {
    if let Some(path) = state
        .config
        .harness
        .harness
        .as_ref()
        .and_then(|v| v["executables"][runtime].as_str())
    {
        return Some(path.into());
    }
    let names: &[&str] = match runtime {
        "hermes" => &["hermes-acp", "hermes"],
        "antigravity" => &["agy", "antigravity"],
        "kiro" => &["kiro-cli"],
        _ => std::slice::from_ref(&runtime),
    };
    for name in names {
        for dir in std::env::split_paths(&path(state)) {
            let path = dir.join(name);
            if !dir.is_absolute()
                || !path.is_file()
                || !path
                    .metadata()
                    .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
            {
                continue;
            }
            if state.config.harness.test_mode
                && !path.starts_with("/dev/shm")
                && !path.starts_with("/tmp")
            {
                continue;
            }
            return Some(path);
        }
    }
    None
}
/// Lets a mise shim run its tool outside a harness launch without resolving back to itself.
pub(crate) fn mise_environment(
    executable: &std::path::Path,
    env: &mut std::collections::HashMap<String, String>,
) {
    if process::mise_shim(executable) {
        process::configure_mise(env);
    }
}
async fn availability(state: &AppState, runtime: &str, deadline: tokio::time::Instant) -> Value {
    let missing = || json!({"runtime":runtime,"available":false,"version":null,"signedIn":null,"models":[],"reason":format!("{} is missing from PATH. Install it and sign in on this host.",label(runtime))});
    let Some(path) = executable(state, runtime) else {
        return missing();
    };
    let version = tokio::time::timeout_at(
        deadline,
        process::probe(&path, &["--version"], runtime, &self::path(state)),
    )
    .await
    .ok()
    .flatten()
    .map(|v| {
        v.chars()
            .map(|c| if c.is_ascii_control() { ' ' } else { c })
            .collect::<String>()
    })
    .map(|v| v.trim().chars().take(160).collect::<String>())
    .filter(|v| !v.is_empty());
    let Some(version) = version else {
        let mut v = missing();
        v["reason"] = json!(
            "Version check failed or timed out. Check the harness installation on this host."
        );
        return v;
    };
    if runtime == "antigravity"
        && tokio::time::timeout_at(
            deadline,
            process::probe(&path, &["--help"], runtime, &self::path(state)),
        )
        .await
        .ok()
        .flatten()
        .is_none_or(|v| !v.lines().any(|l| l.trim().starts_with("acp")))
    {
        return json!({"runtime":runtime,"available":false,"signedIn":null,"models":[],"version":version,"reason":"Installed Antigravity command has no ACP server. Install the standalone agy ACP CLI."});
    }
    let (available, signed, models) = if crate::data::logins::device_runtime(runtime) {
        let signed = tokio::time::timeout_at(
            deadline,
            crate::data::logins::device_signed_in(state, &path, runtime),
        )
        .await
        .ok()
        .flatten();
        (true, signed.map_or(Value::Null, Value::Bool), json!([]))
    } else {
        tokio::time::timeout_at(
            deadline,
            protocol::probe(&path, runtime, &self::path(state)),
        )
        .await
        .ok()
        .flatten()
        .unwrap_or((false, Value::Null, json!([])))
    };
    json!({"runtime":runtime,"available":available,"signedIn":signed,"models":models,"version":version,"reason":if available{Value::Null}else{json!("Installed command did not pass the harness protocol check. Check its ACP support and host login.")}})
}
pub async fn runtimes(state: &AppState) -> Value {
    let mut cache = state.config.harness.cache.lock().await;
    if let Some((at, value)) = &*cache {
        if at.elapsed() < Duration::from_secs(30) {
            return value.clone();
        }
    }
    let deadline = tokio::time::Instant::now() + CATALOG_TIMEOUT;
    let choices = join_all(RUNTIMES.into_iter().map(|runtime| async move {
        let mut value = availability(state, runtime, deadline).await;
        value["label"] = json!(label(runtime));
        value["enabled"] = json!(true);
        value
    }))
    .await;
    let value = json!({"defaultRuntime":if executable(state,"codex").is_some(){"codex"}else{"api"},"choices":choices});
    *cache = Some((Instant::now(), value.clone()));
    value
}
pub fn handles(path: &str, method: &Method) -> bool {
    *method == Method::GET
        && (path == "/api/runtimes"
            || path.starts_with("/api/agents/") && path.ends_with("/runtimes"))
}
pub async fn local(state: &AppState, request: Request) -> Result<Response> {
    let path = request.uri().path().to_owned();
    let mut value = runtimes(state).await;
    if path != "/api/runtimes" {
        let id = path
            .trim_start_matches("/api/agents/")
            .trim_end_matches("/runtimes");
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        let profile = data::agents::read(db, id)?;
        for choice in value["choices"]
            .as_array_mut()
            .ok_or_else(Failure::storage)?
        {
            let runtime = string(choice, "runtime").to_owned();
            if choice["models"].as_array().is_none_or(Vec::is_empty) {
                choice["models"] = one(
                    db,
                    "SELECT models FROM harness_catalog WHERE runtime=?",
                    [&runtime],
                )?
                .map(|v| data::json(&v["models"]))
                .transpose()?
                .unwrap_or(json!([]));
            }
            choice["model"] = one(
                db,
                "SELECT model FROM harness_models WHERE agent=? AND runtime=?",
                [id, &runtime],
            )?
            .map(|v| v["model"].clone())
            .unwrap_or(json!(""));
            choice["capabilities"] = capabilities(&runtime);
            choice["enabled"] = choice["available"].clone();
        }
        value = json!({"runtime":profile["runtime"],"revision":profile["revision"],"dispatchCapabilities":data::agents::capabilities(string(&profile,"runtime")),"choices":value["choices"]});
    }
    Ok(crate::json_response(200, value))
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/runtimes", any(crate::dispatch))
        .route("/api/agents/{id}/runtimes", any(crate::dispatch))
}
pub async fn run(
    state: &AppState,
    turn: &str,
    model: &str,
    messages: &[Value],
    phase: &str,
) -> Result<String> {
    store::run(state, turn, model, messages, phase).await
}
pub(crate) async fn settle_process(state: &AppState, turn: &str) {
    let cleanup = state
        .config
        .harness
        .stopping_processes
        .lock()
        .ok()
        .and_then(|mut processes| processes.remove(turn));
    if let Some(cleanup) = cleanup {
        let _ = cleanup.await;
    }
}
pub async fn stop(state: &AppState) {
    if let Err(error) = crate::turns::harness::drain(state).await {
        eprintln!("Harness shutdown failed: {}", error.message);
    }
    state.config.harness.homes.lock().await.clear();
    let tasks = state
        .config
        .harness
        .revocations
        .lock()
        .map(|mut v| std::mem::take(&mut *v))
        .unwrap_or_default();
    for task in tasks {
        let _ = task.await;
    }
}
pub async fn approve(
    state: &AppState,
    turn: &str,
    kind: &str,
    command: Option<&str>,
    description: &str,
    input: &Value,
    token: &str,
) -> Result<bool> {
    store::approve(state, turn, kind, command, description, input, token).await
}
