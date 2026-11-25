//! Release directories and launcher sockets for update tests.
//!
//! [`native_server`] compiles the launcher's contract fixture
//! (`crates/minarbot-launcher/tests/fixtures/contract.rs`) for a version and
//! mode, as the former `app/test-support/native-release-fixture.ts` did. [`publish`]
//! writes an immutable release directory with its `release.json` and the
//! `feed/latest.json` pointer, hashing files with the launcher's own
//! manifest code. [`MockLauncher`] answers the launcher control socket with
//! a test closure.

use crate::Result;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixListener,
    task::JoinHandle,
};

pub use minarbot_launcher::files;

/// Repository root (two levels above this crate).
pub fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Compiled contract fixture for `version` and `mode` (cached per process).
pub fn native_server(version: &str, mode: &str) -> Result<PathBuf> {
    static CACHE: OnceLock<Mutex<HashMap<(String, String), PathBuf>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (version.to_owned(), mode.to_owned());
    let mut cache = match cache.lock() {
        Ok(cache) => cache,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(path) = cache.get(&key) {
        if path.is_file() {
            return Ok(path.clone());
        }
    }
    let directory =
        std::env::temp_dir().join(format!("minarbot-native-fixture-{}", crate::new_id()));
    fs::create_dir_all(&directory)?;
    let binary = directory.join("server");
    let source = repository().join("crates/minarbot-launcher/tests/fixtures/contract.rs");
    let output = std::process::Command::new("rustc")
        .arg(&source)
        .args(["-O", "-o"])
        .arg(&binary)
        .env("MINARBOT_FIXTURE_VERSION", version)
        .env("MINARBOT_FIXTURE_MODE", mode)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "rustc contract fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    cache.insert(key, binary.clone());
    Ok(binary)
}

/// Copy the contract fixture to `<directory>/bin/minarbot-server` (0700).
pub fn install_native(directory: &Path, version: &str, mode: &str) -> Result<()> {
    let binary = native_server(version, mode)?;
    fs::create_dir_all(directory.join("bin"))?;
    let target = directory.join("bin/minarbot-server");
    fs::copy(binary, &target)?;
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// SHA-256 manifest of every file below `directory` (launcher format).
pub fn manifest(directory: &Path) -> Result<Value> {
    Ok(serde_json::to_value(
        files::manifest(directory).map_err(|error| error.to_string())?,
    )?)
}

/// Release metadata for an existing directory `releases/<id>`.
pub fn info(releases: &Path, id: &str, version: &str, commit: &str, notes: &str) -> Result<Value> {
    Ok(json!({
        "id": id,
        "version": version,
        "commit": commit,
        "created": "2026-11-19T09:00:00.000Z",
        "node": "rust",
        "notes": notes,
        "files": manifest(&releases.join(id))?,
    }))
}

/// Write `release.json` into the release and, with `feed`, `feed/latest.json`.
pub fn write_metadata(releases: &Path, info: &Value, feed: bool) -> Result<()> {
    let id = info["id"].as_str().ok_or("release id")?;
    fs::write(releases.join(id).join("release.json"), info.to_string())?;
    if feed {
        fs::create_dir_all(releases.join("feed"))?;
        fs::write(releases.join("feed/latest.json"), info.to_string())?;
    }
    Ok(())
}

/// Publish a contract-fixture release `version` (id `<version>-<commit8>`)
/// into `releases`, with `package.json`, a Leptos page and the native server.
pub fn publish(releases: &Path, version: &str, commit: &str, notes: &str) -> Result<Value> {
    let id = format!("{version}-{}", &commit[..8]);
    let directory = releases.join(&id);
    fs::create_dir_all(directory.join("dist/_leptos"))?;
    fs::write(
        directory.join("dist/_leptos/conversation.html"),
        "<title>Minarbot fixture</title>",
    )?;
    fs::write(
        directory.join("package.json"),
        json!({"name": "fixture", "version": version, "type": "module"}).to_string(),
    )?;
    install_native(&directory, version, "healthy")?;
    let info = info(releases, &id, version, commit, notes)?;
    write_metadata(releases, &info, true)?;
    Ok(info)
}

/// Verify `releases/<id>` against its `release.json` (launcher check).
pub fn verify(releases: &Path, id: &str) -> Result<Value> {
    files::verify(releases, id).map_err(|error| error.to_string().into())
}

/// Release selected as `name` (for example `current`).
pub fn selection(releases: &Path, name: &str) -> Result<Option<String>> {
    files::selection(releases, name).map_err(|error| error.to_string().into())
}

/// Select `id` as `name`.
pub fn select(releases: &Path, name: &str, id: &str) -> Result<()> {
    files::select(releases, name, id).map_err(|error| error.to_string().into())
}

type Answer = Arc<dyn Fn(&Value) -> Value + Send + Sync>;

/// Launcher control socket answering one JSON line per connection.
pub struct MockLauncher {
    path: PathBuf,
    task: JoinHandle<()>,
}

impl MockLauncher {
    /// Listen on `path` (mode 0600); `answer` maps each command to a reply.
    pub async fn start(
        path: &Path,
        answer: impl Fn(&Value) -> Value + Send + Sync + 'static,
    ) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let listener = UnixListener::bind(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        let answer: Answer = Arc::new(answer);
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let answer = answer.clone();
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let mut line = String::new();
                    if BufReader::new(read).read_line(&mut line).await.is_err() {
                        return;
                    }
                    let command: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
                    let reply = answer(&command);
                    let _ = write.write_all(format!("{reply}\n").as_bytes()).await;
                    let _ = write.shutdown().await;
                });
            }
        });
        Ok(Self {
            path: path.to_owned(),
            task,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for MockLauncher {
    fn drop(&mut self) {
        self.task.abort();
        let _ = fs::remove_file(&self.path);
    }
}
