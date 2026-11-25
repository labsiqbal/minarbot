//! Signed GitHub releases through a CDN, the real launcher trial and peer
//! fan-out transfer policy (ported from `app/github-updates.test.ts`, #89).

use minarbot_testkit::{
    base64::{engine::general_purpose::STANDARD, Engine},
    launcher_process::LauncherProcess,
    prelude::*,
    relay::{self, Answer, Bytes, Relay},
    release_fixture, Servers,
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

const BASE: &str = "/labsiqbal/minarbot/releases";
const CREATED: &str = "2026-11-19T09:00:00.000Z";
const VERSION: &str = "9.1.2";
const COMMIT: &str = "abcdef1234567890abcdef1234567890abcdef12";

fn archive() -> String {
    format!("minarbot-{VERSION}-linux-x86_64.tar.gz")
}

fn locked<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn sha256(bytes: &[u8]) -> String {
    minarbot_testkit::protocol::sha256_hex(bytes)
}

fn current_version() -> Result<String> {
    let package: Value = serde_json::from_slice(&fs::read(
        release_fixture::repository().join("app/package.json"),
    )?)?;
    Ok(package["version"]
        .as_str()
        .ok_or("package version")?
        .to_owned())
}

fn free_port() -> Result<u16> {
    Ok(
        std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?
            .local_addr()?
            .port(),
    )
}

fn private_file(path: &Path, contents: &[u8]) -> Result<()> {
    fs::write(path, contents)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// A release directory `<root>/<version>-<commit8>` with its `release.json`;
/// `binary` is a server script, otherwise the native contract fixture.
fn release(root: &Path, version: &str, binary: Option<&str>) -> Result<Value> {
    let id = format!("{version}-{}", &COMMIT[..8]);
    let directory = root.join(&id);
    fs::create_dir_all(directory.join("bin"))?;
    match binary {
        Some(script) => {
            let path = directory.join("bin/minarbot-server");
            fs::write(&path, script)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        }
        None => release_fixture::install_native(&directory, version, "healthy")?,
    }
    fs::write(
        directory.join("package.json"),
        json!({"version": version}).to_string(),
    )?;
    let info = json!({"id": id, "version": version, "commit": COMMIT, "created": CREATED,
        "notes": "Signed fixture", "node": "rust", "files": release_fixture::manifest(&directory)?});
    fs::write(directory.join("release.json"), info.to_string())?;
    Ok(info)
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Fault {
    Signature,
    MissingSignature,
    ArchiveCommit,
    Hash,
    Truncated,
    Redirect,
    Unknown,
    Commit,
    Version,
}

/// The fake GitHub release origin and its CDN (`github`).
struct Publisher {
    options: Value,
    ranges: Arc<Mutex<Vec<Option<String>>>>,
    size: usize,
    origin: Option<Relay>,
    _cdn: Relay,
    interrupt: Arc<tokio::sync::Notify>,
    wrong_path: Arc<AtomicUsize>,
}

impl Publisher {
    async fn start(
        root: &Path,
        target: &Value,
        fault: Option<Fault>,
        resume: bool,
    ) -> Result<Self> {
        let archive = archive();
        let file = root.join(&archive);
        let id = target["id"].as_str().unwrap_or_default();
        let status = std::process::Command::new("tar")
            .args(["--format=ustar", "-czf"])
            .arg(&file)
            .arg("-C")
            .arg(root.join(id))
            .arg(".")
            .status()?;
        ensure!(status.success(), "tar failed");
        let bytes = Bytes::from(fs::read(&file)?);
        let random = ring::rand::SystemRandom::new();
        let pkcs8 =
            ring::signature::Ed25519KeyPair::generate_pkcs8(&random).map_err(|_| "ed25519 key")?;
        let key = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
            .map_err(|_| "ed25519 key")?;
        let digest = sha256(&bytes);
        let mut manifest = json!({"id": id,
            "version": if fault == Some(Fault::Version) { "9.1.3" } else { VERSION },
            "commit": match fault {
                Some(Fault::Commit) => "invalid".to_owned(),
                Some(Fault::ArchiveCommit) => format!("{}0", &COMMIT[..COMMIT.len() - 1]),
                _ => COMMIT.to_owned(),
            },
            "archive": archive, "created": CREATED, "notes": target["notes"],
            "sha256": if fault == Some(Fault::Hash) { "0".repeat(64) } else { digest.clone() },
            "size": bytes.len()});
        if resume {
            manifest["assets"] = json!({archive.clone(): {"sha256": digest, "size": bytes.len()}});
        }
        if fault == Some(Fault::Unknown) {
            manifest["extra"] = json!(true);
        }
        let manifest = Bytes::from(format!("{}\n", serde_json::to_string_pretty(&manifest)?));
        let signed = if fault == Some(Fault::Signature) {
            &manifest[..manifest.len() - 1]
        } else {
            &manifest[..]
        };
        let signature = STANDARD.encode(key.sign(signed).as_ref());
        let ranges: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
        let interrupt = Arc::new(tokio::sync::Notify::new());
        let wrong_path = Arc::new(AtomicUsize::new(0));
        let (seen, cut, wrong, body) = (
            ranges.clone(),
            interrupt.clone(),
            wrong_path.clone(),
            bytes.clone(),
        );
        let cdn = Relay::start(move |exchange| {
            let (seen, cut, wrong, bytes) =
                (seen.clone(), cut.clone(), wrong.clone(), body.clone());
            async move {
                if exchange.path != "/asset" {
                    wrong.fetch_add(1, Ordering::SeqCst);
                }
                let range = exchange.header("range").map(str::to_owned);
                let count = {
                    let mut ranges = locked(&seen);
                    ranges.push(range.clone());
                    ranges.len()
                };
                if fault == Some(Fault::Truncated) {
                    return Ok(Answer::bytes(
                        200,
                        Vec::new(),
                        bytes.slice(..bytes.len() - 1).to_vec(),
                    ));
                }
                if resume && count == 1 {
                    return Ok(Answer::stream(
                        200,
                        vec![("content-length".into(), bytes.len().to_string())],
                        relay::cut_on(Some(bytes.slice(..64)), cut),
                    ));
                }
                let offset = range
                    .as_deref()
                    .and_then(|value| value.strip_prefix("bytes="))
                    .and_then(|value| value.strip_suffix('-'))
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                if offset > 0 {
                    return Ok(Answer::bytes(
                        206,
                        vec![(
                            "content-range".into(),
                            format!("bytes {offset}-{}/{}", bytes.len() - 1, bytes.len()),
                        )],
                        bytes.slice(offset..).to_vec(),
                    ));
                }
                Ok(Answer::bytes(200, Vec::new(), bytes.to_vec()))
            }
        })
        .await?;
        let cdn_origin = cdn.origin().to_owned();
        let download = format!("{BASE}/download/v{VERSION}/{archive}");
        let (manifest_body, signature_body) = (manifest.clone(), signature.clone());
        let origin = Relay::start(move |exchange| {
            let (manifest, signature, cdn, download) = (
                manifest_body.clone(),
                signature_body.clone(),
                cdn_origin.clone(),
                download.clone(),
            );
            async move {
                let path = exchange.path.as_str();
                if path == format!("{BASE}/latest/download/latest.json") {
                    return Ok(Answer::bytes(200, Vec::new(), manifest.to_vec()));
                }
                if path == format!("{BASE}/latest/download/latest.json.sig")
                    && fault != Some(Fault::MissingSignature)
                {
                    return Ok(Answer::bytes(200, Vec::new(), signature.into_bytes()));
                }
                if path == download {
                    let location = if fault == Some(Fault::Redirect) {
                        format!("{}/asset", cdn.replace("127.0.0.1", "127.0.0.2"))
                    } else {
                        format!("{cdn}/asset")
                    };
                    return Ok(Answer::bytes(
                        302,
                        vec![("location".into(), location)],
                        Vec::new(),
                    ));
                }
                Ok(Answer::empty(404, Vec::new()))
            }
        })
        .await?;
        let public = STANDARD.encode(ring::signature::KeyPair::public_key(&key).as_ref());
        Ok(Self {
            options: json!({"testReleaseBaseUrl": format!("{}{BASE}", origin.origin()), "testReleasePublicKey": public}),
            ranges,
            size: bytes.len(),
            origin: Some(origin),
            _cdn: cdn,
            interrupt,
            wrong_path,
        })
    }

    fn ranges(&self) -> Vec<Option<String>> {
        locked(&self.ranges).clone()
    }

    fn close(&mut self) {
        self.origin = None;
    }

    fn check(&self) -> Result<()> {
        ensure!(
            self.wrong_path.load(Ordering::SeqCst) == 0,
            "CDN asked for another path"
        );
        Ok(())
    }
}

/// A launcher-managed machine whose current release runs the real server
/// (`managed`).
struct Managed {
    origin: String,
    releases: PathBuf,
    data: PathBuf,
    current: Value,
    launcher: LauncherProcess,
}

impl Managed {
    async fn start(path: &Path, updates: &Value) -> Result<Self> {
        let (data, releases) = (path.join("data"), path.join("releases"));
        fs::create_dir_all(&data)?;
        fs::set_permissions(&data, fs::Permissions::from_mode(0o700))?;
        let port = free_port()?;
        let origin = format!("http://127.0.0.1:{port}");
        let (socket, database) = (data.join("launcher.sock"), data.join("minarbot.sqlite"));
        let config = path.join("server.json");
        let mut options = updates.clone();
        options["releasesDir"] = json!(releases);
        options["launcherSocket"] = json!(socket);
        private_file(
            &config,
            json!({"database": database, "port": port, "updates": options})
                .to_string()
                .as_bytes(),
        )?;
        let binary = minarbot_testkit::server::server_binary(None)?;
        let wrapper = format!(
            "#!/bin/sh\nexec '{}' --test-config < '{}'\n",
            binary.display(),
            config.display()
        );
        let current = release(&releases, &current_version()?, Some(&wrapper))?;
        release_fixture::select(
            &releases,
            "current",
            current["id"].as_str().unwrap_or_default(),
        )?;
        let launcher_config = path.join("launcher.json");
        private_file(
            &launcher_config,
            json!({"dataDir": data, "releasesDir": releases, "port": port, "trialTimeoutMs": 15_000}).to_string().as_bytes(),
        )?;
        let mut launcher = LauncherProcess::start(&launcher_config, path)?;
        let client = minarbot_testkit::relay::HttpClient::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = launcher.exited()? {
                return Err(format!("launcher exited ({status}): {}", launcher.logs()).into());
            }
            if socket.exists()
                && client
                    .get(format!("{origin}/api/environment"))
                    .send()
                    .await
                    .is_ok_and(|response| response.status().is_success())
            {
                break;
            }
            if std::time::Instant::now() > deadline {
                launcher.stop().await?;
                return Err(format!("launcher not ready: {}", launcher.logs()).into());
            }
            tokio::time::sleep(Duration::from_millis(70)).await;
        }
        Ok(Self {
            origin,
            releases,
            data,
            current,
            launcher,
        })
    }

    fn attempt(&self) -> Value {
        fs::read(self.data.join("update-state.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .map_or(Value::Null, |state| state["lastAttempt"].clone())
    }

    fn selected(&self) -> Result<Option<String>> {
        release_fixture::selection(&self.releases, "current")
    }

    /// `waitForTrial`.
    async fn trial(&self, id: &Value, target: &Value) -> Result<()> {
        minarbot_testkit::wait::until_within("update trial", Duration::from_secs(30), || async {
            let attempt = self.attempt();
            if attempt["id"] != *id {
                return Ok(None);
            }
            if attempt["status"] == "failed" || attempt["status"] == "rolled-back" {
                return Err(format!("{}: {}", attempt["reason"], self.launcher.logs()).into());
            }
            Ok((attempt["status"] == "done").then_some(()))
        })
        .await?;
        ensure!(
            self.selected()?.as_deref() == target.as_str(),
            "selected release"
        );
        Ok(())
    }
}

/// Plain JSON request; `body` makes it a POST with an `Origin`.
async fn api(
    origin: &str,
    path: &str,
    body: Option<Value>,
    status: u16,
    timeout: Duration,
) -> Result<Value> {
    let client = minarbot_testkit::relay::HttpClient::builder()
        .timeout(timeout)
        .build()?;
    let request = match body {
        Some(body) => client
            .post(format!("{origin}{path}"))
            .header("Origin", origin)
            .header("Content-Type", "application/json")
            .body(body.to_string()),
        None => client.get(format!("{origin}{path}")),
    };
    let response = request.send().await?;
    let code = response.status().as_u16();
    let value: Value = response.json().await.unwrap_or(Value::Null);
    ensure!(code == status, "{path}: {code} {value}");
    Ok(value)
}

const SHORT: Duration = Duration::from_secs(15);
const CHECK: &str = "/api/updates";
const APPLY: &str = "/api/updates/apply";

fn matches(text: &Value, alternatives: &[&str]) -> bool {
    let text = text.as_str().unwrap_or_default().to_lowercase();
    alternatives
        .iter()
        .any(|needle| text.contains(&needle.to_lowercase()))
}

#[tokio::test]
async fn signed_github_release_resumes_through_the_cdn_and_completes_a_real_launcher_trial(
) -> Result<()> {
    let _slots = Servers::reserve(1).await?;
    let dir = TestRoot::new()?;
    let staged = dir.dir("publish")?;
    let publisher =
        Publisher::start(&staged, &release(&staged, VERSION, None)?, None, true).await?;
    let mut app = Managed::start(&dir.dir("machine")?, &publisher.options).await?;
    let result = async {
        let state = api(&app.origin, CHECK, None, 200, SHORT).await?;
        ensure!(
            state["available"]["version"] == VERSION,
            "available: {state}"
        );
        ensure!(state["available"]["source"] == "GitHub", "source");
        let id = new_id();
        let command = json!({"id": id, "version": VERSION, "source": "GitHub"});
        ensure!(
            api(&app.origin, APPLY, Some(command.clone()), 202, SHORT).await?["id"] == id.as_str(),
            "apply"
        );
        ensure!(
            api(&app.origin, APPLY, Some(command), 202, SHORT).await?["id"] == id.as_str(),
            "apply replay"
        );
        let progress = minarbot_testkit::wait::until_within(
            "partial download",
            Duration::from_secs(30),
            || async {
                let attempt =
                    api(&app.origin, CHECK, None, 200, SHORT).await?["lastAttempt"].clone();
                Ok(
                    (attempt["id"] == id.as_str() && attempt["bytesTransferred"] == 64)
                        .then_some(attempt),
                )
            },
        )
        .await?;
        ensure!(progress["totalBytes"] == publisher.size, "total bytes");
        ensure!(progress["source"] == "GitHub", "progress source");
        publisher.interrupt.notify_one();
        app.trial(&json!(id), &state["available"]["id"]).await?;
        ensure!(
            publisher.ranges() == vec![None, Some("bytes=64-".to_owned())],
            "ranges: {:?}",
            publisher.ranges()
        );
        ensure!(
            api(&app.origin, "/api/environment", None, 200, SHORT).await?["version"] == VERSION,
            "running version"
        );
        ensure!(
            app.data
                .join(".updates")
                .join(&id)
                .join("minarbot.sqlite")
                .exists(),
            "trial database backup"
        );
        publisher.check()
    }
    .await;
    app.launcher.stop().await?;
    result
}

async fn github_fault_rejected(fault: Fault, message: &[&str], at_check: bool) -> Result<()> {
    let _slots = Servers::reserve(1).await?;
    let dir = TestRoot::new()?;
    let staged = dir.dir("publish")?;
    let target = release(&staged, VERSION, None)?;
    let publisher = Publisher::start(&staged, &target, Some(fault), false).await?;
    let mut app = Managed::start(&dir.dir("machine")?, &publisher.options).await?;
    let result = async {
        let state = api(&app.origin, CHECK, None, 200, SHORT).await?;
        if at_check {
            ensure!(state["available"].is_null(), "{fault:?} available: {state}");
            ensure!(
                matches(&state["githubError"], message),
                "{fault:?} error: {}",
                state["githubError"]
            );
        } else {
            ensure!(
                state["available"]["version"] == VERSION,
                "{fault:?} available: {state}"
            );
            let id = new_id();
            api(
                &app.origin,
                APPLY,
                Some(json!({"id": id, "version": VERSION, "source": "GitHub"})),
                202,
                SHORT,
            )
            .await?;
            let failure = minarbot_testkit::wait::until_within(
                "failed attempt",
                Duration::from_secs(30),
                || async {
                    let status =
                        api(&app.origin, CHECK, None, 200, SHORT).await?["lastAttempt"].clone();
                    Ok(
                        (status["id"] == id.as_str() && status["status"] == "failed")
                            .then_some(status),
                    )
                },
            )
            .await?;
            ensure!(
                matches(&failure["reason"], message),
                "{fault:?} reason: {}",
                failure["reason"]
            );
        }
        ensure!(
            api(&app.origin, "/api/environment", None, 200, SHORT).await?["version"]
                == app.current["version"],
            "current release replaced"
        );
        ensure!(
            app.selected()?.as_deref() == app.current["id"].as_str(),
            "selection changed"
        );
        ensure!(
            !app.releases
                .join(target["id"].as_str().unwrap_or_default())
                .exists(),
            "target installed"
        );
        if fault == Fault::Redirect {
            ensure!(publisher.ranges().is_empty(), "redirect reached the CDN");
        }
        publisher.check()
    }
    .await;
    app.launcher.stop().await?;
    result
}

#[tokio::test]
async fn github_signature_fixture_is_rejected_while_current_release_keeps_running() -> Result<()> {
    github_fault_rejected(Fault::Signature, &["signature"], true).await
}

#[tokio::test]
async fn github_missing_signature_fixture_is_rejected_while_current_release_keeps_running(
) -> Result<()> {
    github_fault_rejected(Fault::MissingSignature, &["signature is missing"], true).await
}

#[tokio::test]
async fn github_unknown_fixture_is_rejected_while_current_release_keeps_running() -> Result<()> {
    github_fault_rejected(Fault::Unknown, &["unknown field"], true).await
}

#[tokio::test]
async fn github_commit_fixture_is_rejected_while_current_release_keeps_running() -> Result<()> {
    github_fault_rejected(Fault::Commit, &["commit"], true).await
}

#[tokio::test]
async fn github_version_fixture_is_rejected_while_current_release_keeps_running() -> Result<()> {
    github_fault_rejected(Fault::Version, &["version"], true).await
}

#[tokio::test]
async fn github_archive_commit_fixture_is_rejected_while_current_release_keeps_running(
) -> Result<()> {
    github_fault_rejected(Fault::ArchiveCommit, &["does not match the archive"], false).await
}

#[tokio::test]
async fn github_hash_fixture_is_rejected_while_current_release_keeps_running() -> Result<()> {
    github_fault_rejected(Fault::Hash, &["sha-256"], false).await
}

#[tokio::test]
async fn github_truncated_fixture_is_rejected_while_current_release_keeps_running() -> Result<()> {
    github_fault_rejected(Fault::Truncated, &["truncated", "size mismatch"], false).await
}

#[tokio::test]
async fn github_redirect_fixture_is_rejected_while_current_release_keeps_running() -> Result<()> {
    github_fault_rejected(Fault::Redirect, &["redirect"], false).await
}

/// Holds apply replies until the receiver restarted into the new release, so
/// the sender's confirmation always comes from the restarted server, as it
/// does on a slow link (`lateReplies`).
async fn late_replies(receiver: &Managed) -> Result<Relay> {
    let own: Arc<Mutex<String>> = Arc::default();
    let (target, data, me) = (receiver.origin.clone(), receiver.data.clone(), own.clone());
    let relay = Relay::start(move |exchange| {
        let (target, data, me) = (target.clone(), data.clone(), locked(&me).clone());
        async move {
            let upstream = match exchange.forward(&target).await {
                Ok(upstream) => upstream,
                Err(_) => return Ok(Answer::Drop),
            };
            if exchange.path == "/api/environment" || exchange.path == "/api/pairing/redeem" {
                // The receiver identifies itself by the origin the sender paired with.
                let status = upstream.status;
                let body = String::from_utf8_lossy(&upstream.bytes().await?).into_owned();
                let rewritten = body.replace(&json!(target).to_string(), &json!(me).to_string());
                return Ok(Answer::bytes(
                    status,
                    vec![("content-type".into(), "application/json".into())],
                    rewritten.into_bytes(),
                ));
            }
            let committed = fs::read(data.join("update-state.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .is_some_and(|state| state["lastAttempt"]["status"] == "done");
            if exchange.path == "/api/peer/updates/apply" && !committed {
                let _ = upstream.bytes().await;
                return Ok(Answer::Drop);
            }
            Ok(upstream.relay())
        }
    })
    .await?;
    *locked(&own) = relay.origin().to_owned();
    Ok(relay)
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Route {
    Direct,
    LateReply,
    Fallback,
    Invalid,
}

async fn github_peer_fan_out(route: Route) -> Result<()> {
    let servers = Servers::reserve(2).await?;
    let dir = TestRoot::new()?;
    let staged = dir.dir("publish")?;
    let target = release(&staged, VERSION, None)?;
    let publisher = Publisher::start(&staged, &target, None, false).await?;
    let mut separate = match route {
        Route::Direct | Route::LateReply => None,
        Route::Fallback => Some(Publisher::start(&staged, &target, None, false).await?),
        Route::Invalid => {
            Some(Publisher::start(&staged, &target, Some(Fault::Signature), false).await?)
        }
    };
    if route == Route::Fallback {
        if let Some(publisher) = separate.as_mut() {
            publisher.close();
        }
    }
    let receiver_options = separate
        .as_ref()
        .map_or(&publisher.options, |publisher| &publisher.options)
        .clone();
    let mut receiver = Managed::start(&dir.dir("receiver")?, &receiver_options).await?;
    let result = async {
        let late = if route == Route::LateReply {
            Some(late_replies(&receiver).await?)
        } else {
            None
        };
        let receiver_origin = late.as_ref().map_or_else(
            || receiver.origin.clone(),
            |relay| relay.origin().to_owned(),
        );
        let releases = dir.dir("sender")?;
        let mut updates = publisher.options.clone();
        updates["releasesDir"] = json!(releases);
        let config = ServerConfig::new().real_clock().option("updates", updates);
        let sender_root = TestRoot::new()?;
        let mut initial = sender_root.start_in(&servers, config.clone()).await?;
        let initial_origin = initial.origin().to_owned();
        let code = api(
            &receiver.origin,
            "/api/pairing-codes",
            Some(json!({"id": new_id()})),
            200,
            SHORT,
        )
        .await?["code"]
            .clone();
        let machine = api(
            &initial_origin,
            "/api/machines",
            Some(json!({"id": new_id(), "origin": receiver_origin, "code": code})),
            200,
            SHORT,
        )
        .await?;
        let sender =
            api(&receiver.origin, "/api/machines", None, 200, SHORT).await?["machines"][0].clone();
        api(
            &initial_origin,
            "/api/machines/updates",
            Some(json!({"id": new_id(), "environmentId": machine["id"], "allowed": true})),
            200,
            SHORT,
        )
        .await?;
        api(
            &receiver.origin,
            "/api/machines/updates",
            Some(json!({"id": new_id(), "environmentId": sender["id"], "allowed": true})),
            200,
            SHORT,
        )
        .await?;
        initial.shutdown().await?;
        drop(initial);
        let (archive_requests, archive_bytes) =
            (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let backend: Arc<Mutex<String>> = Arc::default();
        let (requests, bytes_seen, upstream_origin, advertised) = (
            archive_requests.clone(),
            archive_bytes.clone(),
            backend.clone(),
            initial_origin.clone(),
        );
        let _front = Relay::bind(relay::port(&initial_origin)?, move |exchange| {
            let (requests, bytes_seen, backend, advertised) = (
                requests.clone(),
                bytes_seen.clone(),
                locked(&upstream_origin).clone(),
                advertised.clone(),
            );
            async move {
                let upstream = match exchange.forward(&backend).await {
                    Ok(upstream) => upstream,
                    Err(_) => return Ok(Answer::Drop),
                };
                if exchange.path == "/api/environment" {
                    let status = upstream.status;
                    let mut value: Value = serde_json::from_slice(&upstream.bytes().await?)?;
                    value["origin"] = json!(advertised);
                    return Ok(Answer::bytes(
                        status,
                        vec![("content-type".into(), "application/json".into())],
                        value.to_string().into_bytes(),
                    ));
                }
                if exchange.path.starts_with("/api/updates/releases/") {
                    requests.fetch_add(1, Ordering::SeqCst);
                    let status = upstream.status;
                    let headers = relay::without_length(upstream.headers.clone());
                    let body = upstream.bytes().await?;
                    bytes_seen.fetch_add(body.len(), Ordering::SeqCst);
                    return Ok(Answer::bytes(status, headers, body));
                }
                Ok(upstream.relay())
            }
        })
        .await?;
        let app = sender_root.start_in(&servers, config).await?;
        *locked(&backend) = app.origin().to_owned();
        let origin = app.origin().to_owned();
        if route == Route::Fallback {
            let feed = receiver.releases.join("feed");
            fs::create_dir(&feed)?;
            fs::write(
                feed.join("latest.json"),
                release(&receiver.releases, "8.0.0", None)?.to_string(),
            )?;
            let state = api(&receiver.origin, CHECK, None, 200, SHORT).await?;
            ensure!(
                matches(&state["githubError"], &["github is unreachable"]),
                "github error: {}",
                state["githubError"]
            );
            ensure!(state["available"]["source"] == "Local feed", "feed source");
            ensure!(state["available"]["version"] == "8.0.0", "feed version");
        }
        let id = new_id();
        let command =
            json!({"id": id, "machines": [machine["id"]], "version": VERSION, "source": "GitHub"});
        let path = "/api/updates/fan-out";
        // The fan-out reply waits for the peer's transfer and apply, which can
        // exceed 15 s under suite load.
        let result = api(
            &origin,
            path,
            Some(command.clone()),
            202,
            Duration::from_secs(60),
        )
        .await?["machines"][0]
            .clone();
        let last = api(&origin, "/api/updates/machines", None, 200, SHORT).await?["machines"][0]
            ["fanOutAttempt"]
            .clone();
        if route == Route::Invalid {
            ensure!(result["status"] == "failed", "invalid status: {result}");
            ensure!(
                matches(&result["reason"], &["github release signature"]),
                "invalid reason: {}",
                result["reason"]
            );
            ensure!(
                receiver.selected()?.as_deref() == receiver.current["id"].as_str(),
                "receiver selection"
            );
            ensure!(
                archive_requests.load(Ordering::SeqCst) == 0,
                "archive requests"
            );
            ensure!(archive_bytes.load(Ordering::SeqCst) == 0, "archive bytes");
            return Ok(());
        }
        ensure!(result["status"] == "accepted", "result: {result}");
        ensure!(
            last["source"]
                == if route == Route::Fallback {
                    "Local feed"
                } else {
                    "GitHub"
                },
            "attempt source: {last}"
        );
        ensure!(
            last["bytesTransferred"] == last["totalBytes"],
            "bytes transferred: {last}"
        );
        ensure!(
            api(&origin, path, Some(command), 202, SHORT).await?["machines"][0]["id"]
                == result["id"],
            "fan-out replay"
        );
        receiver.trial(&result["id"], &target["id"]).await?;
        ensure!(
            publisher.ranges() == vec![None],
            "ranges: {:?}",
            publisher.ranges()
        );
        let fallback = route == Route::Fallback;
        ensure!(
            archive_requests.load(Ordering::SeqCst) == usize::from(fallback),
            "archive requests"
        );
        ensure!(
            archive_bytes.load(Ordering::SeqCst)
                == if fallback {
                    last["totalBytes"].as_u64().unwrap_or(0) as usize
                } else {
                    0
                },
            "archive bytes"
        );
        ensure!(
            releases
                .join(target["id"].as_str().unwrap_or_default())
                .exists()
                == fallback,
            "sender release copy"
        );
        publisher.check()
    }
    .await;
    receiver.launcher.stop().await?;
    result
}

#[tokio::test]
async fn github_peer_fan_out_uses_direct_transfer_policy() -> Result<()> {
    github_peer_fan_out(Route::Direct).await
}

#[tokio::test]
async fn github_peer_fan_out_uses_late_reply_transfer_policy() -> Result<()> {
    github_peer_fan_out(Route::LateReply).await
}

#[tokio::test]
async fn github_peer_fan_out_uses_fallback_transfer_policy() -> Result<()> {
    github_peer_fan_out(Route::Fallback).await
}

#[tokio::test]
async fn github_peer_fan_out_uses_invalid_transfer_policy() -> Result<()> {
    github_peer_fan_out(Route::Invalid).await
}
