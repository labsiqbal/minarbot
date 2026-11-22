use minarbot_launcher::{files, Result, DATABASE_NAME};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use reqwest::{redirect::Policy, Client};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    fs,
    net::TcpListener,
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    process::{Child, Command},
    time::{sleep, timeout},
};
use uuid::Uuid;

const BIN: &str = env!("CARGO_BIN_EXE_minarbot-launcher");
const WAIT: Duration = Duration::from_secs(30);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir()
            .canonicalize()?
            .join(format!("minarbot-launcher-test-{}", Uuid::new_v4()));
        files::mkdir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    root: Scratch,
    home: PathBuf,
    data: PathBuf,
    releases: PathBuf,
    config: PathBuf,
    env: PathBuf,
    settings: Value,
    port: u16,
    reserved_port: RefCell<Option<TcpListener>>,
    client: Client,
}

struct Release {
    id: String,
    version: String,
    path: PathBuf,
}

impl Release {
    fn seal(&self) -> Result<()> {
        fs::write(
            self.path.join("release.json"),
            serde_json::to_vec(&json!({
                "id":self.id,"version":self.version,"files":files::manifest(&self.path)?
            }))?,
        )?;
        Ok(())
    }
}

impl Fixture {
    fn new(long_home: bool) -> Result<Self> {
        let root = Scratch::new()?;
        let home = if long_home {
            root.0.join("long-home-".repeat(18))
        } else {
            root.0.clone()
        };
        let data = home.join("data");
        let releases = home.join("releases");
        fs::create_dir_all(&data)?;
        fs::create_dir(&releases)?;
        let reserved_port = TcpListener::bind(("127.0.0.1", 0))?;
        let port = reserved_port.local_addr()?.port();
        let config = root.0.join("launcher.json");
        let env = root.0.join("env");
        let settings = json!({"dataDir":data,"releasesDir":releases,"port":port,"trialTimeoutMs":5000,"envFile":env});
        files::durable(&env, b"")?;
        Connection::open(data.join(DATABASE_NAME))?.execute_batch(
            "CREATE TABLE data(value TEXT); INSERT INTO data VALUES('preserved');",
        )?;
        let client = Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()?;
        let f = Self {
            root,
            home,
            data,
            releases,
            config,
            env,
            settings,
            port,
            reserved_port: RefCell::new(Some(reserved_port)),
            client,
        };
        f.save()?;
        Ok(f)
    }

    fn save(&self) -> Result<()> {
        files::durable(&self.config, &serde_json::to_vec(&self.settings)?)
    }

    fn release(&self, version: &str) -> Result<Release> {
        let id = format!("{version}-12345678");
        let path = self.releases.join(&id);
        fs::create_dir_all(path.join("bin"))?;
        fs::write(
            path.join("package.json"),
            serde_json::to_vec(&json!({"version":version,"type":"module"}))?,
        )?;
        Ok(Release {
            id,
            version: version.into(),
            path,
        })
    }

    fn publish(&self, version: &str, mode: &str) -> Result<Release> {
        let release = self.release(version)?;
        compile(
            "contract.rs",
            &release.path.join("bin/minarbot-server"),
            version,
            mode,
        )?;
        release.seal()?;
        Ok(release)
    }

    fn first(&self) -> Result<Release> {
        let first = self.publish("1.0.0", "healthy")?;
        files::select(&self.releases, "current", &first.id)?;
        Ok(first)
    }

    fn start(&self, binary: impl AsRef<Path>) -> Result<Child> {
        drop(self.reserved_port.borrow_mut().take());
        Ok(Command::new(binary.as_ref())
            .args(["--config"])
            .arg(&self.config)
            .env_clear()
            .env("HOME", &self.home)
            .env("TMPDIR", &self.root.0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?)
    }

    fn socket(&self) -> PathBuf {
        self.data.join("launcher.sock")
    }

    async fn read(&self) -> Result<Value> {
        Ok(self
            .client
            .get(format!("http://127.0.0.1:{}/api/environment", self.port))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    async fn ready(&self, version: &str) -> Result<Value> {
        until(async || match self.read().await {
            Ok(value) if value["version"] == version => Ok(Some(value)),
            _ => Ok(None),
        })
        .await
    }

    async fn outcome(&self, status: &str) -> Result<Value> {
        until(async || {
            let Ok(value) = control(&self.socket(), &json!({"type":"status"})).await else {
                return Ok(None);
            };
            let actual = value["lastAttempt"]["status"].as_str();
            match actual {
                Some(actual) if actual == status => Ok(Some(value)),
                Some("done" | "rolled-back" | "failed") => {
                    Err(format!("expected launcher {status}: {value}").into())
                }
                _ => Ok(None),
            }
        })
        .await
    }

    async fn rejects_start(&self, expected: &str) -> Result<()> {
        let mut child = self.start(BIN)?;
        let mut log = String::new();
        let mut stderr = child.stderr.take().ok_or("missing launcher stderr")?;
        let status = timeout(WAIT, async {
            stderr.read_to_string(&mut log).await?;
            child.wait().await
        })
        .await??;
        assert!(!status.success());
        assert!(log.contains(expected), "{log}");
        Ok(())
    }
}

fn compile(source: &str, binary: &Path, version: &str, mode: &str) -> Result<()> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(source);
    let output = std::process::Command::new("rustc")
        .arg(source)
        .arg("-O")
        .arg("-o")
        .arg(binary)
        .env("MINARBOT_FIXTURE_VERSION", version)
        .env("MINARBOT_FIXTURE_MODE", mode)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "fixture compilation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}

fn chunked_candidate(release: &Release) -> Result<()> {
    compile(
        "server.rs",
        &release.path.join("bin/minarbot-server"),
        &release.version,
        "healthy",
    )?;
    fs::write(release.path.join("version.txt"), &release.version)?;
    release.seal()
}

async fn until<T>(mut check: impl AsyncFnMut() -> Result<Option<T>>) -> Result<T> {
    timeout(WAIT, async {
        loop {
            if let Some(value) = check().await? {
                return Ok(value);
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await?
}

async fn control(socket: &Path, request: &Value) -> Result<Value> {
    timeout(Duration::from_secs(5), async {
        let mut client = UnixStream::connect(socket).await?;
        client.write_all(format!("{request}\n").as_bytes()).await?;
        client.shutdown().await?;
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).await?;
        let response: Value = serde_json::from_slice(&bytes)?;
        if let Some(error) = response["error"].as_str() {
            return Err(error.to_owned().into());
        }
        Ok(response)
    })
    .await?
}

fn apply(release: &Release) -> Value {
    json!({"type":"apply","id":Uuid::new_v4().to_string(),"target":release.id,"version":release.version})
}

async fn terminate(mut child: Child, signal: Signal) -> Result<ExitStatus> {
    let id = child.id().ok_or("launcher already exited")?;
    assert!(child.try_wait()?.is_none());
    kill(Pid::from_raw(i32::try_from(id)?), signal)?;
    Ok(timeout(WAIT, child.wait()).await??)
}

fn mode(path: impl AsRef<Path>) -> Result<u32> {
    Ok(fs::metadata(path)?.permissions().mode() & 0o777)
}

#[tokio::test]
async fn long_home_uses_private_control_socket_and_cleans_it_up() -> Result<()> {
    let f = Fixture::new(true)?;
    let first = f.first()?;
    let running = f.start(BIN)?;
    let ready = f.ready(&first.version).await?;
    let socket = Path::new(
        ready["env"]["MINARBOT_LAUNCHER_SOCKET"]
            .as_str()
            .ok_or("no launcher socket")?,
    );
    assert_eq!(mode(socket)?, 0o600);
    assert_eq!(mode(socket.parent().ok_or("no socket directory")?)?, 0o700);
    assert_eq!(
        control(socket, &json!({"type":"status"})).await?["current"],
        first.id
    );
    assert!(terminate(running, Signal::SIGTERM).await?.success());
    assert!(!socket.exists());
    Ok(())
}

#[tokio::test]
async fn config_rejects_invalid_shapes_and_untrusted_files() -> Result<()> {
    let f = Fixture::new(false)?;
    for invalid in [Value::Null, json!([])] {
        fs::write(&f.config, serde_json::to_vec(&invalid)?)?;
        f.rejects_start("Invalid launcher").await?;
    }
    for (field, value) in [
        ("unknown", json!(true)),
        ("dataDir", json!("relative")),
        ("envFile", Value::Null),
        ("port", json!(0)),
        ("port", json!(1.5)),
        ("trialTimeoutMs", json!(499)),
        ("trialTimeoutMs", json!(1800001)),
    ] {
        let mut invalid = f.settings.clone();
        invalid[field] = value;
        fs::write(&f.config, serde_json::to_vec(&invalid)?)?;
        f.rejects_start("Invalid launcher").await?;
    }
    f.save()?;
    fs::set_permissions(&f.config, fs::Permissions::from_mode(0o640))?;
    f.rejects_start("private files").await?;
    fs::set_permissions(&f.config, fs::Permissions::from_mode(0o600))?;
    let copy = f.root.0.join("config-copy");
    fs::hard_link(&f.config, &copy)?;
    f.rejects_start("private files").await?;
    fs::remove_file(&copy)?;
    fs::rename(&f.config, &copy)?;
    symlink(&copy, &f.config)?;
    f.rejects_start("private files").await?;
    fs::remove_file(&f.config)?;
    f.save()?;
    fs::set_permissions(&f.env, fs::Permissions::from_mode(0o644))?;
    f.rejects_start("private files").await?;
    Ok(())
}

#[tokio::test]
async fn literal_environment_and_socket_commit_persist_receipts() -> Result<()> {
    let f = Fixture::new(false)?;
    let first = f.first()?;
    let canary = f.root.0.join("must-not-exist");
    let literal = format!("$(touch {})", canary.display());
    fs::write(&f.env, format!("export LAUNCHER_QUOTED=\"line one\nline two\"\nLAUNCHER_LITERAL={literal}\nLAUNCHER_SINGLE='literal # value'\nLAUNCHER_EMPTY=\nPORT=1\nMINARBOT_DATA_DIR=/wrong\nMINARBOT_UPDATE_TRIAL=untrusted\n"))?;
    let running = f.start(BIN)?;
    let initial = f.ready(&first.version).await?;
    assert_eq!(initial["connected"], true);
    assert_eq!(initial["env"]["LAUNCHER_QUOTED"], "line one\nline two");
    assert_eq!(initial["env"]["LAUNCHER_SINGLE"], "literal # value");
    assert_eq!(initial["env"]["LAUNCHER_EMPTY"], "");
    assert_eq!(initial["env"]["LAUNCHER_LITERAL"], literal);
    assert!(!canary.exists());
    assert_eq!(initial["env"]["PORT"], f.port.to_string());
    assert_eq!(initial["env"]["MINARBOT_DATA_DIR"], json!(f.data));
    assert!(initial["env"].get("MINARBOT_UPDATE_TRIAL").is_none());

    let second = f.publish("1.0.1", "healthy")?;
    let request = apply(&second);
    control(&f.socket(), &request).await?;
    let id = request["id"].as_str().ok_or("no update ID")?;
    until(async || Ok(f.read().await.ok().filter(|value| value["committed"] == id))).await?;
    assert_eq!(
        mode(f.data.join(".updates").join(id).join(DATABASE_NAME))?,
        0o600
    );
    assert_eq!(mode(f.data.join("update-state.json"))?, 0o600);
    assert_eq!(mode(f.socket())?, 0o600);

    assert!(terminate(running, Signal::SIGTERM).await?.success());
    let restarted = f.start(BIN)?;
    f.ready(&second.version).await?;
    assert_eq!(control(&f.socket(), &request).await?["accepted"], true);
    let mut changed = request;
    changed["version"] = json!("9.0.0");
    let error = control(&f.socket(), &changed)
        .await
        .err()
        .ok_or("changed receipt accepted")?;
    assert!(error.to_string().contains("already used"));
    assert!(terminate(restarted, Signal::SIGTERM).await?.success());
    Ok(())
}

#[tokio::test]
async fn corrupt_candidate_and_bad_control_messages_keep_current_server() -> Result<()> {
    let f = Fixture::new(false)?;
    let first = f.first()?;
    let running = f.start(BIN)?;
    f.ready(&first.version).await?;
    for request in [
        json!({"type":"status","extra":1}),
        json!({"type":"other"}),
        json!({"type":"apply","id":"bad","target":first.id,"version":first.version}),
    ] {
        let error = control(&f.socket(), &request)
            .await
            .err()
            .ok_or("invalid control request accepted")?
            .to_string();
        assert!(
            error.contains("control request") || error.contains("update request"),
            "{error}"
        );
    }
    let bad = f.publish("1.0.1", "healthy")?;
    fs::write(bad.path.join("bin/minarbot-server"), "tampered")?;
    let error = control(&f.socket(), &apply(&bad))
        .await
        .err()
        .ok_or("corrupt release accepted")?;
    assert!(error.to_string().contains("manifest mismatch"));

    let mut oversized = UnixStream::connect(f.socket()).await?;
    oversized.write_all(&vec![b'x'; 4097]).await?;
    let mut reply = Vec::new();
    match timeout(WAIT, oversized.read_to_end(&mut reply)).await? {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
        Err(error) => return Err(error.into()),
    }
    assert_eq!(
        control(&f.socket(), &json!({"type":"status"})).await?["current"],
        first.id
    );
    assert_eq!(f.read().await?["version"], first.version);
    assert!(terminate(running, Signal::SIGTERM).await?.success());
    Ok(())
}

#[tokio::test]
async fn startup_rejects_tampering_and_public_persisted_state() -> Result<()> {
    let f = Fixture::new(false)?;
    let first = f.first()?;
    fs::write(first.path.join("bin/minarbot-server"), "tampered")?;
    f.rejects_start("manifest mismatch").await?;
    fs::set_permissions(
        f.data.join("update-state.json"),
        fs::Permissions::from_mode(0o644),
    )?;
    f.rejects_start("private files").await
}

#[tokio::test]
async fn next_probe_commits_after_first_probe_disconnects() -> Result<()> {
    let mut f = Fixture::new(false)?;
    f.settings["trialTimeoutMs"] = json!(15000);
    f.save()?;
    let first = f.first()?;
    let running = f.start(BIN)?;
    f.ready(&first.version).await?;
    let second = f.publish("1.0.1", "expired-probe")?;
    let request = apply(&second);
    control(&f.socket(), &request).await?;
    f.outcome("done").await?;
    let candidate = f.read().await?;
    assert_eq!(candidate["version"], second.version);
    assert_eq!(candidate["committed"], request["id"]);
    assert!(terminate(running, Signal::SIGTERM).await?.success());
    Ok(())
}

#[tokio::test]
async fn native_chunked_candidate_stops_after_launcher_sigkill() -> Result<()> {
    let f = Fixture::new(false)?;
    let first = f.first()?;
    let running = f.start(BIN)?;
    f.ready(&first.version).await?;
    let second = f.publish("1.0.1", "healthy")?;
    chunked_candidate(&second)?;
    fs::write(
        second.path.join("server.ts"),
        "throw new Error('unexpected legacy entrypoint')",
    )?;
    second.seal()?;
    control(&f.socket(), &apply(&second)).await?;
    f.outcome("done").await?;
    assert_eq!(f.read().await?["native"], true);
    assert!(!terminate(running, Signal::SIGKILL).await?.success());
    until(async || Ok(f.read().await.is_err().then_some(()))).await
}

#[tokio::test]
async fn launcher_updates_after_its_own_binary_is_unlinked() -> Result<()> {
    let f = Fixture::new(false)?;
    let executable = f.root.0.join("launcher-copy");
    fs::copy(BIN, &executable)?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
    let first = f.first()?;
    let running = f.start(&executable)?;
    f.ready(&first.version).await?;
    fs::remove_file(executable)?;
    let next = f.publish("1.0.1", "healthy")?;
    chunked_candidate(&next)?;
    let request = apply(&next);
    control(&f.socket(), &request).await?;
    f.outcome("done").await?;
    assert_eq!(f.read().await?["native"], true);
    assert!(f
        .data
        .join(".updates")
        .join(request["id"].as_str().ok_or("missing update ID")?)
        .join(DATABASE_NAME)
        .exists());
    assert!(terminate(running, Signal::SIGTERM).await?.success());
    Ok(())
}

#[tokio::test]
async fn healthy_trial_commits_once_and_refuses_a_competing_launcher() -> Result<()> {
    let f = Fixture::new(false)?;
    let first = f.first()?;
    let running = f.start(BIN)?;
    f.ready(&first.version).await?;
    let second = f.publish("1.0.1", "healthy")?;
    let request = apply(&second);
    for _ in 0..2 {
        assert_eq!(control(&f.socket(), &request).await?["accepted"], true);
    }
    let mut changed = request;
    changed["target"] = json!(first.id);
    changed["version"] = json!(first.version);
    let error = control(&f.socket(), &changed)
        .await
        .err()
        .ok_or("changed receipt accepted")?;
    assert!(error.to_string().contains("already used"));
    let outcome = f.outcome("done").await?;
    assert_eq!(outcome["current"], second.id);
    assert_eq!(
        files::selection(&f.releases, "previous")?.as_deref(),
        Some(first.id.as_str())
    );
    assert_eq!(
        files::json(&f.data.join("update-state.json"))?["lastAttempt"]["status"],
        "done"
    );
    let mut rival = f.start(BIN)?;
    assert!(!timeout(WAIT, rival.wait()).await??.success());
    assert_eq!(
        control(&f.socket(), &json!({"type":"status"})).await?["current"],
        second.id
    );
    assert_eq!(data_value(&f)?, "preserved");
    assert!(terminate(running, Signal::SIGTERM).await?.success());
    Ok(())
}

fn data_value(f: &Fixture) -> Result<String> {
    Ok(Connection::open(f.data.join(DATABASE_NAME))?.query_row(
        "SELECT value FROM data",
        [],
        |row| row.get(0),
    )?)
}

async fn assert_candidate_rolls_back(mode: &str) -> Result<()> {
    let f = Fixture::new(false)?;
    let first = f.first()?;
    let running = f.start(BIN)?;
    f.ready(&first.version).await?;
    let bad = f.publish("1.0.1", mode)?;
    control(&f.socket(), &apply(&bad)).await?;
    let state = f.outcome("rolled-back").await?;
    assert_eq!(state["current"], first.id);
    f.ready(&first.version).await?;
    assert_eq!(data_value(&f)?, "preserved");
    assert_eq!(
        files::selection(&f.releases, "current")?.as_deref(),
        Some(first.id.as_str())
    );
    let reason = state["lastAttempt"]["reason"]
        .as_str()
        .ok_or("missing rollback reason")?;
    assert!(
        reason.to_ascii_lowercase().contains("health")
            || reason.to_ascii_lowercase().contains("exited"),
        "{reason}"
    );
    if mode == "refuse" {
        assert!(reason.ends_with("exited before becoming healthy: This database does not carry the Minarbot data marker. Install fresh."), "{reason}");
    }
    assert!(terminate(running, Signal::SIGTERM).await?.success());
    Ok(())
}

#[tokio::test]
async fn wrong_version_candidate_rolls_back() -> Result<()> {
    assert_candidate_rolls_back("wrong").await
}

#[tokio::test]
async fn exited_candidate_rolls_back() -> Result<()> {
    assert_candidate_rolls_back("exit").await
}

#[tokio::test]
async fn refused_candidate_rolls_back_with_diagnostic() -> Result<()> {
    assert_candidate_rolls_back("refuse").await
}

async fn apply_native(f: &Fixture, release: &Release) -> Result<String> {
    let mut feed = files::json(&release.path.join("release.json"))?;
    feed["commit"] = json!("1234567812345678123456781234567812345678");
    feed["created"] = json!("2026-01-01T00:00:00Z");
    feed["node"] = json!("fixture");
    feed["notes"] = json!("Native launcher trial");
    fs::create_dir_all(f.releases.join("feed"))?;
    fs::write(
        f.releases.join("feed/latest.json"),
        serde_json::to_vec(&feed)?,
    )?;
    let id = Uuid::new_v4().to_string();
    let origin = format!("http://127.0.0.1:{}", f.port);
    let response = f
        .client
        .post(format!("{origin}/api/updates/apply"))
        .header("Origin", &origin)
        .timeout(WAIT)
        .json(&json!({"id":id,"version":release.version,"source":"Local feed"}))
        .send()
        .await?;
    let status = response.status();
    let body: Value = response.json().await?;
    assert_eq!(status, reqwest::StatusCode::ACCEPTED, "{body}");
    assert_eq!(body, json!({"accepted":true,"id":id}));
    Ok(id)
}

fn assert_foreign_keys(db: &Connection) -> Result<()> {
    let mut check = db.prepare("PRAGMA foreign_key_check")?;
    let mut rows = check.query([])?;
    if let Some(row) = rows.next()? {
        return Err(format!(
            "FK violation in {} at row {:?} referencing {}",
            row.get::<_, String>(0)?,
            row.get::<_, Option<i64>>(1)?,
            row.get::<_, String>(2)?
        )
        .into());
    }
    Ok(())
}

#[tokio::test]
async fn native_trial_commits_and_restores_feature_data() -> Result<()> {
    // Workspace tests build both binaries. The launcher unit tests cover
    // FTS5 snapshot validation when only the launcher package is built.
    let server = Path::new(BIN).with_file_name("minarbot-server");
    if !server.is_file() {
        eprintln!("Build minarbot-server or run workspace tests for native trial coverage.");
        return Ok(());
    }
    let f = Fixture::new(false)?;
    let database = f.data.join(DATABASE_NAME);
    minarbot_launcher::data_format::mark(&Connection::open(&database)?)?;

    let first = f.release("1.0.0")?;
    let first_binary = first.path.join("bin/minarbot-server");
    let strip_debug = if cfg!(target_os = "macos") {
        "-S"
    } else {
        "--strip-debug"
    };
    let stripped = std::process::Command::new("strip")
        .args([strip_debug, "-o"])
        .arg(&first_binary)
        .arg(server)
        .output()?;
    assert!(
        stripped.status.success(),
        "{}",
        String::from_utf8_lossy(&stripped.stderr)
    );
    first.seal()?;
    files::select(&f.releases, "current", &first.id)?;
    let mut running = f.start(BIN)?;
    f.ready(&first.version).await?;

    let second = f.release("1.0.1")?;
    fs::copy(&first_binary, second.path.join("bin/minarbot-server"))?;
    second.seal()?;
    let committed = apply_native(&f, &second).await?;
    let updated = f.outcome("done").await?;
    assert_eq!(updated["current"], second.id);
    assert_eq!(updated["lastAttempt"]["id"], committed);
    let trial = control(&f.socket(), &json!({"type":"trial","id":committed})).await?;
    assert_eq!(trial["committed"], true);
    f.ready(&second.version).await?;
    until(async || {
        let response = f
            .client
            .get(format!("http://127.0.0.1:{}/api/agents", f.port))
            .send()
            .await;
        Ok(response
            .is_ok_and(|response| response.status().is_success())
            .then_some(()))
    })
    .await?;

    {
        let mut db = Connection::open(&database)?;
        db.pragma_update(None, "foreign_keys", true)?;
        let workload = db.transaction()?;
        workload.execute_batch(include_str!("fixtures/rollback.sql"))?;
        assert_foreign_keys(&workload)?;
        workload.commit()?;
    }
    let broken = f.publish("1.0.2", "exit")?;
    let failed = apply_native(&f, &broken).await?;
    let rollback = tokio::select! {
        result = f.outcome("rolled-back") => result?,
        status = running.wait() => {
            let mut log = String::new();
            running.stderr.as_mut().ok_or("missing launcher stderr")?.read_to_string(&mut log).await?;
            return Err(format!("launcher exited ({})\n{log}", status?).into());
        }
    };
    assert_eq!(rollback["lastAttempt"]["id"], failed);
    assert_eq!(rollback["lastAttempt"]["snapshotReady"], true);
    assert_eq!(
        rollback["lastAttempt"]["reason"],
        "Candidate exited before becoming healthy."
    );
    assert_eq!(rollback["current"], second.id);
    assert_eq!(
        files::selection(&f.releases, "current")?,
        Some(second.id.clone())
    );
    f.ready(&second.version).await?;
    assert_eq!(data_value(&f)?, "preserved");
    let snapshot = Connection::open_with_flags(
        f.data.join(".updates").join(failed).join(DATABASE_NAME),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    assert_foreign_keys(&snapshot)?;
    assert_eq!(
        snapshot.pragma_query_value(None, "integrity_check", |row| row.get::<_, String>(0))?,
        "ok"
    );
    let restored = Connection::open(&database)?;
    assert_foreign_keys(&restored)?;
    let indexed_notes: i64 = restored.query_row(
        "SELECT count(*) FROM memory_fts WHERE memory_fts MATCH 'recallable'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(indexed_notes, 1);
    assert!(terminate(running, Signal::SIGTERM).await?.success());
    Ok(())
}

#[tokio::test]
async fn interrupted_trial_restores_snapshot_before_relaunch() -> Result<()> {
    let mut f = Fixture::new(false)?;
    f.settings["trialTimeoutMs"] = json!(15000);
    f.save()?;
    let first = f.first()?;
    let running = f.start(BIN)?;
    f.ready(&first.version).await?;
    let second = f.publish("1.0.1", "wrong")?;
    let request = apply(&second);
    control(&f.socket(), &request).await?;
    until(async || {
        let status = control(&f.socket(), &json!({"type":"status"})).await?;
        Ok((status["lastAttempt"]["status"] == "verifying"
            && data_value(&f).is_ok_and(|value| value == "trial damage"))
        .then_some(()))
    })
    .await?;
    assert!(!terminate(running, Signal::SIGKILL).await?.success());
    until(async || Ok(f.read().await.is_err().then_some(()))).await?;

    let restarted = f.start(BIN)?;
    f.outcome("rolled-back").await?;
    f.ready(&first.version).await?;
    assert_eq!(data_value(&f)?, "preserved");
    assert!(terminate(restarted, Signal::SIGTERM).await?.success());
    Ok(())
}
