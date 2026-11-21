mod archive;
mod attempts;
pub mod config;
mod github;
pub mod install;
mod peer_error;
mod peers;
mod release;
pub(crate) use github::remote_error as github_remote_error;
pub(crate) use peer_error::PeerError;
use peers::*;

use crate::{json_response, local_error, web::Failure, AppState};
use axum::{extract::Request, http::Method, response::Response, routing::any, Router};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

type Result<T> = std::result::Result<T, Failure>;
fn error(status: u16, message: impl Into<String>) -> Failure {
    Failure::new(status, "invalid_request", message)
}
fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
const LOCAL_SOURCE: &str = "Local feed";
static RECEIVING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn receive_lock() -> Result<tokio::sync::MutexGuard<'static, ()>> {
    RECEIVING
        .try_lock()
        .map_err(|_| error(409, "Another update is already running."))
}

const UNMANAGED: &str = "Server is not running under the Minarbot launcher. Install the launcher before applying updates.";

pub(crate) async fn computer_image(state: &AppState, cache: &std::path::Path) -> Result<String> {
    let unavailable = || error(503, "Computer image needs a published Minarbot release.");
    let id = std::env::var("MINARBOT_RELEASE_ID").map_err(|_| unavailable())?;
    let feed = state
        .config
        .update
        .options()
        .github
        .as_ref()
        .ok_or_else(unavailable)?;
    feed.computer_image(&id, cache).await?.ok_or_else(|| {
        error(
            503,
            "This Minarbot release has no Computer image. Update Minarbot and try again.",
        )
    })
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .merge(install::routes())
        .route("/api/machines/updates", any(crate::dispatch))
        .route("/api/updates", any(crate::dispatch))
        .route("/api/updates/apply", any(crate::dispatch))
        .route("/api/updates/fan-out", any(crate::dispatch))
        .route("/api/updates/machines", any(crate::dispatch))
        .route("/api/peer/updates", any(crate::dispatch))
        .route("/api/peer/updates/apply", any(crate::dispatch))
        .route("/api/updates/releases/{id}", any(crate::dispatch))
}
pub fn transport(path: &str) -> bool {
    path == "/api/environment"
        || path == "/api/peer/updates"
        || path == "/api/peer/updates/apply"
        || path.starts_with("/api/updates/releases/")
}
pub fn handles(path: &str) -> bool {
    path == "/api/machines/updates"
        || path == "/api/updates"
        || path.starts_with("/api/updates/")
        || path == "/api/peer/updates"
        || path.starts_with("/api/peer/updates/")
}
pub async fn launcher(options: &config::Options, request: Value) -> Result<Value> {
    let unavailable =
        |status, message| Failure::new(status, PeerError::LauncherUnavailable.code(), message);
    let path = options
        .launcher_socket
        .as_ref()
        .ok_or_else(|| unavailable(409, UNMANAGED))?;
    let metadata = std::fs::symlink_metadata(path).map_err(|_| {
        unavailable(
            409,
            "Launcher control socket must be private and owned by this user.",
        )
    })?;
    if !metadata.file_type().is_socket()
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(unavailable(
            409,
            "Launcher control socket must be private and owned by this user.",
        ));
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut socket = UnixStream::connect(path).await.map_err(|_| {
            unavailable(
                409,
                "Launcher is unavailable. Start Minarbot through its launcher.",
            )
        })?;
        socket
            .write_all(format!("{request}\n").as_bytes())
            .await
            .map_err(|_| {
                unavailable(
                    409,
                    "Launcher is unavailable. Start Minarbot through its launcher.",
                )
            })?;
        let mut buffer = Vec::new();
        loop {
            let mut bytes = [0; 4096];
            let n = socket.read(&mut bytes).await.map_err(|_| {
                unavailable(
                    409,
                    "Launcher is unavailable. Start Minarbot through its launcher.",
                )
            })?;
            if n == 0 {
                return Err(unavailable(
                    503,
                    "Launcher response was interrupted. Retry with the same request ID.",
                ));
            }
            buffer.extend_from_slice(&bytes[..n]);
            if buffer.len() > 65536 {
                return Err(unavailable(502, "Invalid launcher response."));
            }
            if let Some(end) = buffer.iter().position(|b| *b == b'\n') {
                let value: Value = serde_json::from_slice(&buffer[..end])
                    .map_err(|_| unavailable(502, "Invalid launcher response."))?;
                if let Some(message) = value.get("error").filter(|v| !v.is_null() && **v != false) {
                    return Err(error(
                        409,
                        message
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| message.to_string()),
                    ));
                }
                return Ok(value);
            }
        }
    })
    .await
    .map_err(|_| {
        unavailable(
            503,
            "Launcher response timed out. Retry with the same request ID.",
        )
    })?
}
pub fn pending(state: &AppState) -> bool {
    state.trial_pending.load(Ordering::Acquire)
}
pub fn trial(config: &crate::config::Config) -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(config.update.options().trial.is_some()))
}
pub fn watch_trial(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let Some(id) = state.config.update.options().trial.as_ref() else {
            return;
        };
        loop {
            if launcher(
                state.config.update.options(),
                json!({"type":"trial","id":id}),
            )
            .await
            .is_ok_and(|v| v["committed"] == true)
            {
                state.trial_pending.store(false, Ordering::Release);
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
}
pub fn initialize(db: &rusqlite::Connection) -> crate::db::Result<()> {
    install::initialize(db)?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS update_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;")?;
    db.execute(
        "UPDATE update_commands SET result='{\"interrupted\":true}'
         WHERE id LIKE 'peer-apply:%' AND json_extract(result,'$.pending')=1",
        [],
    )?;
    db.execute(
        "UPDATE update_commands SET result=json_set(result,'$.githubAttempt.status','failed',
             '$.githubAttempt.reason','Server restarted during the GitHub download.')
         WHERE json_extract(result,'$.githubAttempt.status')='downloading'",
        [],
    )?;
    attempts::initialize(db)?;
    Ok(())
}
pub fn version() -> String {
    let package = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent()?.parent().map(|root| root.join("package.json")));
    package
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v["version"].as_str().map(str::to_owned))
        .unwrap_or_else(|| {
            serde_json::from_str::<Value>(include_str!("../../../../app/package.json"))
                .ok()
                .and_then(|v| v["version"].as_str().map(str::to_owned))
                .unwrap_or_default()
        })
}
pub async fn status(state: &AppState) -> Result<Value> {
    let options = state.config.update.options();
    let control = launcher(options, json!({"type":"status"})).await.ok();
    let info = |id: &Value| {
        options
            .releases_dir
            .as_ref()
            .and_then(|p| id.as_str().and_then(|id| release::info(p, id).ok()))
    };
    let current = control
        .as_ref()
        .and_then(|c| info(&c["current"]))
        .unwrap_or_else(
            || json!({"id":"development","version":version(),"commit":"","created":"","notes":""}),
        );
    let mut result = json!({"current":current});
    if let Some(previous) = control.as_ref().and_then(|c| info(&c["previous"])) {
        result["previous"] = previous;
    }
    if let Some(feed) = options
        .releases_dir
        .as_ref()
        .map(|p| release::latest(p))
        .transpose()?
        .flatten()
    {
        let mut public = release::public(&feed);
        public["source"] = json!(LOCAL_SOURCE);
        result["feed"] = public.clone();
        if release::newer(string(&feed, "version"), string(&current, "version")) {
            result["available"] = public;
        }
    }
    if let Some(attempt) = control.as_ref().and_then(|c| c.get("lastAttempt")) {
        result["lastAttempt"] = attempt.clone();
    }
    if let Some(attempt) = local_download_status(state, &result["lastAttempt"])? {
        result["lastAttempt"] = attempt;
    }
    result["launcherAvailable"] = json!(control.is_some_and(|c| c["protocol"] == 1));
    result["runningTurns"] = json!(state
        .database
        .lock()
        .map_err(|_| Failure::storage())?
        .connection
        .query_row(
            "SELECT count(*) FROM turns WHERE status='running'",
            [],
            |r| r.get::<_, i64>(0)
        )?);
    Ok(result)
}
async fn check(state: &AppState) -> Result<Value> {
    let mut result = status(state).await?;
    if let Some(feed) = &state.config.update.options().github {
        match feed.check().await {
            Ok(manifest) => {
                result["github"] = manifest.public();
                if release::newer(&manifest.version, string(&result["current"], "version"))
                    && !release::newer(string(&result["available"], "version"), &manifest.version)
                {
                    result["available"] = manifest.public();
                }
            }
            Err(failure) => result["githubError"] = json!(failure.message),
        }
    }
    Ok(result)
}

fn command(value: &Value, keys: &[&str]) -> Result<()> {
    if !value
        .as_object()
        .is_some_and(|m| m.keys().all(|k| keys.contains(&k.as_str())))
    {
        return Err(error(400, "Invalid update request."));
    }
    if crate::web::id(string(value, "id")).is_none() || !release::version(string(value, "version"))
    {
        return Err(error(
            400,
            "Update requires a request ID and exact version.",
        ));
    }
    Ok(())
}
fn prior(state: &AppState, id: &str, digest: &str) -> Result<Option<Value>> {
    let db = state.database.lock().map_err(|_| Failure::storage())?;
    let saved: Option<(String, String)> = db
        .connection
        .query_row(
            "SELECT request,result FROM update_commands WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    saved
        .map(|(request, result)| {
            if request != digest {
                return Err(error(
                    409,
                    "Request ID was already used for another update.",
                ));
            }
            serde_json::from_str(&result).map_err(|_| Failure::storage())
        })
        .transpose()
}
fn save(state: &AppState, id: &str, digest: &str, result: &Value) -> Result<()> {
    state
        .database
        .lock()
        .map_err(|_| Failure::storage())?
        .connection
        .execute(
            "INSERT INTO update_commands VALUES(?,?,?)",
            rusqlite::params![id, digest, result.to_string()],
        )?;
    Ok(())
}
fn target(state: &AppState, version: &str) -> Result<Value> {
    let options = state.config.update.options();
    let releases = options
        .releases_dir
        .as_ref()
        .ok_or_else(|| error(409, "Release feed is not configured."))?;
    let feed = release::latest(releases)?
        .filter(|f| f["version"] == version)
        .ok_or_else(|| {
            error(
                409,
                "Requested version is not available in the local release feed.",
            )
        })?;
    release::verify(releases, string(&feed, "id"))?;
    Ok(feed)
}
enum Target {
    Local(Value),
    GitHub(github::Manifest),
}
impl Target {
    fn public(&self) -> Value {
        match self {
            Self::Local(value) => release::public(value),
            Self::GitHub(manifest) => manifest.public(),
        }
    }
}

fn source(value: &Value) -> Result<Option<&str>> {
    match value.get("source") {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .filter(|source| [github::SOURCE, LOCAL_SOURCE].contains(source))
            .map(Some)
            .ok_or_else(|| error(400, "Invalid update source.")),
    }
}

async fn selected_target(state: &AppState, version: &str, source: Option<&str>) -> Result<Target> {
    if source != Some(LOCAL_SOURCE) {
        let result = match &state.config.update.options().github {
            Some(feed) => feed.check().await,
            None => Err(github::unreachable()),
        };
        match result {
            Ok(manifest) if manifest.version == version => return Ok(Target::GitHub(manifest)),
            Err(failure) if source == Some(github::SOURCE) => return Err(failure),
            Ok(_) if source == Some(github::SOURCE) => {
                return Err(error(409, "Requested version is not available on GitHub."));
            }
            _ => {}
        }
    }
    target(state, version).map(Target::Local)
}

fn local_download_status(state: &AppState, launcher_attempt: &Value) -> Result<Option<Value>> {
    let saved: Option<String> = state
        .database
        .lock()
        .map_err(|_| Failure::storage())?
        .connection
        .query_row(
            "SELECT json_extract(result,'$.githubAttempt') FROM update_commands
             WHERE json_type(result,'$.githubAttempt')='object' ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(saved) = saved else { return Ok(None) };
    let mut attempt: Value = serde_json::from_str(&saved).map_err(|_| Failure::storage())?;
    if launcher_attempt["id"] == attempt["id"] {
        let mut launcher_attempt = launcher_attempt.clone();
        launcher_attempt["source"] = json!(github::SOURCE);
        return Ok(Some(launcher_attempt));
    }
    if string(launcher_attempt, "started") > string(&attempt, "started") {
        return Ok(None);
    }
    if attempt["status"] == "downloading" {
        attempt["bytesTransferred"] = json!(downloaded(
            state,
            string(&attempt, "target"),
            string(&attempt, "sha256")
        ));
    }
    if let Some(fields) = attempt.as_object_mut() {
        fields.remove("sha256");
    }
    Ok(Some(attempt))
}

fn downloaded(state: &AppState, target: &str, sha256: &str) -> u64 {
    state
        .config
        .update
        .options()
        .releases_dir
        .as_ref()
        .and_then(|root| {
            std::fs::symlink_metadata(archive::partial_file(root, target, sha256)).ok()
        })
        .filter(std::fs::Metadata::is_file)
        .map_or(0, |metadata| metadata.len())
}

async fn apply(state: &AppState, value: &Value) -> Result<Value> {
    command(value, &["id", "version", "source"])?;
    let source = source(value)?;
    let id = string(value, "id");
    let version = string(value, "version");
    let mut digest = json!({"apply":version});
    if let Some(source) = source {
        digest["source"] = json!(source);
    }
    let digest = digest.to_string();
    let options = state.config.update.options();
    let apply = |release: Value| json!({"type":"apply","id":id,"version":release["version"],"target":release["id"]});
    if let Some(saved) = prior(state, id, &digest)? {
        if saved.get("githubAttempt").is_some() {
            return Ok(json!({"accepted":true,"id":id}));
        }
        return launcher(options, apply(saved)).await;
    }
    let _receiving = receive_lock()?;
    let current = status(state).await?;
    if current["launcherAvailable"] != true {
        return Err(error(409, UNMANAGED));
    }
    match selected_target(state, version, source).await? {
        Target::Local(release) => {
            save(
                state,
                id,
                &digest,
                &json!({"id":release["id"],"version":version}),
            )?;
            launcher(options, apply(release)).await
        }
        Target::GitHub(manifest) => {
            if !release::newer(version, string(&current["current"], "version")) {
                return Err(error(409, "Release version is not an update."));
            }
            let attempt = json!({"id":id,"version":version,"target":manifest.id,
                "status":"downloading","source":github::SOURCE,"totalBytes":manifest.size,
                "sha256":manifest.sha256,"started":chrono::DateTime::from_timestamp_millis(crate::config::now())
                    .ok_or_else(Failure::storage)?.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)});
            save(state, id, &digest, &json!({"githubAttempt":attempt}))?;
            let (state, id) = (state.clone(), id.to_owned());
            tokio::spawn(apply_github(state, id, manifest, attempt, _receiving));
            Ok(json!({"accepted":true,"id":value["id"]}))
        }
    }
}

async fn apply_github(
    state: AppState,
    id: String,
    manifest: github::Manifest,
    mut attempt: Value,
    _receiving: tokio::sync::MutexGuard<'static, ()>,
) {
    let options = state.config.update.options();
    let outcome = async {
        let feed = options.github.as_ref().ok_or_else(github::unreachable)?;
        let root = options.releases_dir.as_ref().ok_or_else(Failure::storage)?;
        feed.stage(root, &manifest, || async { Ok(()) }).await?;
        launcher(
            options,
            json!({"type":"apply","id":id,"target":manifest.id,"version":manifest.version}),
        )
        .await?;
        Ok::<_, Failure>(())
    }
    .await;
    if let Err(failure) = outcome {
        attempt["status"] = json!("failed");
        attempt["reason"] = json!(failure.message);
        if let Err(error) = save_update_result(&state, &id, &json!({"githubAttempt":attempt})) {
            eprintln!("GitHub release apply receipt {id}: {}", error.code);
        }
    }
}

fn save_update_result(state: &AppState, key: &str, result: &Value) -> Result<()> {
    state
        .database
        .lock()
        .map_err(|_| Failure::storage())?
        .connection
        .execute(
            "UPDATE update_commands SET result=? WHERE id=?",
            rusqlite::params![result.to_string(), key],
        )?;
    Ok(())
}
pub async fn local(state: &AppState, request: Request) -> Result<Response> {
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    if path.starts_with("/api/peer/") || path.starts_with("/api/updates/releases/") {
        return peer(state, request).await;
    }
    if request.uri().query().is_some()
        || request.headers().contains_key("authorization")
        || request.headers().contains_key("origin")
            && !crate::allowed(state, crate::text_header(request.headers(), "origin"))
    {
        return Err(error(403, "Updates require the local owner in Minarbot."));
    }
    if method == Method::GET {
        if path == "/api/updates" {
            return Ok(json_response(200, check(state).await?));
        }
        if path == "/api/updates/machines" {
            return Ok(json_response(200, peer_statuses(state).await?));
        }
    }
    if method == Method::POST {
        if !crate::allowed(state, crate::text_header(request.headers(), "origin"))
            || crate::text_header(request.headers(), "content-type")
                .and_then(|v| v.split(';').next())
                != Some("application/json")
        {
            return Err(error(403, "Request must originate from Minarbot."));
        }
        let bytes = axum::body::to_bytes(request.into_body(), 64000)
            .await
            .map_err(|_| error(413, "Data is too large."))?;
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| error(400, "Invalid JSON."))?;
        if !value.is_object() {
            return Err(error(400, "JSON object is required."));
        }
        let id = string(&value, "id");
        if id.trim().is_empty() || id.encode_utf16().count() > 36 {
            return Err(error(400, "Field id is invalid (maximum 36 characters)."));
        }
        if crate::web::id(id.trim()).is_none() {
            return Err(error(400, "Invalid request ID."));
        }
        if path == "/api/machines/updates" {
            return Ok(json_response(
                200,
                crate::computer::machines::set_updates(state, &value)?,
            ));
        }
        if path == "/api/updates/apply" {
            return Ok(json_response(202, apply(state, &value).await?));
        }
        if path == "/api/updates/fan-out" {
            return Ok(json_response(202, fan_out(state, &value).await?));
        }
    }
    Ok(local_error(404, "Route not found."))
}
