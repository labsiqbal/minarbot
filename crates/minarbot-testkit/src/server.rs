//! Disposable `minarbot-server` processes.
//!
//! [`TestRoot`] owns one temporary directory (database, harness homes, server
//! logs). [`TestRoot::start`] spawns the real server binary with
//! `--test-config` and a JSON [`ServerConfig`]; [`Server::restart`] reuses the
//! same data, so restart and recovery tests read exactly what the previous
//! process stored.

use crate::{
    bridge::Bridge,
    fakes::{self, Fake},
    http::Reply,
    issuer::SessionIssuer,
    model::ScriptedModel,
    Error, Result,
};
use reqwest::{header, redirect::Policy, Client, Method, RequestBuilder};
use rustix::process::{kill_process, Pid, Signal};
use serde_json::{json, Map, Value};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    net::{Ipv4Addr, SocketAddrV4},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::time::{sleep, timeout, Instant};
use uuid::Uuid;

/// Default timeout for kit waits and HTTP calls.
pub const WAIT: Duration = Duration::from_secs(30);
const STARTUP_WAIT: Duration = Duration::from_secs(120);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// Default fixed clock: 2026-11-19T07:33:20Z.
pub const FIXED_CLOCK: i64 = 1_795_000_000_000;
/// Value of `MINARBOT_HARNESS_CANARY` in every server environment. Fake peers
/// fail when it reaches them, which proves harness environments are scrubbed.
pub const HARNESS_CANARY: &str = "never-inherit-this-canary";
/// Hermetic `PATH`: no user tool directories, so real `codex` or `claude`
/// installations on the host can never be discovered by a test server.
pub const HERMETIC_PATH: &str = "/usr/bin:/bin";

/// Every option `app/rust-test-server.ts` accepted. Options outside this list
/// are rejected before the server starts, like the Node bridge did.
pub const SUPPORTED_OPTIONS: &[&str] = &[
    "testMemoryExtraction",
    "pdfFault",
    "testPathPrefix",
    "testCanonicalBridge",
    "hubLink",
    "composio",
    "mailApiKey",
    "mailEndpoint",
    "agentLimits",
    "imageProvider",
    "transcriptionProvider",
    "scriptToken",
    "scriptDirectory",
    "installationId",
    "mailFrom",
    "mailOrigin",
    "testKeyring",
    "testKeyringLocked",
    "updates",
    "desktop",
    "machines",
    "database",
    "port",
    "host",
    "tls",
    "allowedOrigins",
    "dist",
    "auth",
    "fixedClock",
    "baseURL",
    "apiKey",
    "defaultModel",
    "provider",
    "accounts",
    "timeout",
    "concurrency",
    "workspaces",
    "projectRoot",
    "harness",
    "stream",
    "streamTermination",
    "maxTokens",
];

/// OpenAI-compatible endpoint that the Codex gateway peer (`fake-gateway`)
/// calls for every model step. `ServerConfig::gateway` mirrors the Node
/// bridge: it sets the top-level `baseURL` family of options, seeds
/// `harness.testGateway` and points the `codex` executable at the gateway.
#[derive(Clone, Debug, Default)]
pub struct Gateway {
    pub base_url: String,
    pub api_key: Option<String>,
    pub default_model: Option<String>,
    pub provider: Option<String>,
    pub stream: Option<bool>,
    pub stream_termination: Option<String>,
    pub max_tokens: Option<u64>,
}

impl Gateway {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            ..Self::default()
        }
    }
}

/// Options for one server start. Builders consume and return `self`.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    options: Map<String, Value>,
    binary: Option<PathBuf>,
    issuer: Option<SessionIssuer>,
    environment: Vec<(String, String)>,
    sync_key: Option<[u8; 32]>,
    /// Test bridges by server option name; kept alive with the config.
    bridges: Vec<(&'static str, Value, Bridge)>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        let mut options = Map::new();
        options.insert("fixedClock".into(), json!(FIXED_CLOCK));
        Self {
            options,
            binary: None,
            issuer: None,
            environment: Vec::new(),
            sync_key: None,
            bridges: Vec::new(),
        }
    }
}

impl ServerConfig {
    /// Fixed clock at [`FIXED_CLOCK`], loopback, random port.
    pub fn new() -> Self {
        Self::default()
    }

    /// Any supported option by its Node bridge name (see [`SUPPORTED_OPTIONS`]).
    pub fn option(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.options.insert(name.into(), value.into());
        self
    }

    /// Remove an option, for example to start without the gateway again.
    pub fn without(mut self, name: &str) -> Self {
        self.options.remove(name);
        self
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        self.options.get(name)
    }

    pub fn fixed_clock(self, millis: i64) -> Self {
        self.option("fixedClock", millis)
    }

    /// Use wall-clock time. Prefer the fixed clock; this exists for flows
    /// whose server-side deadlines must advance during the test.
    pub fn real_clock(self) -> Self {
        self.without("fixedClock")
    }

    pub fn clock(&self) -> Option<i64> {
        self.options.get("fixedClock").and_then(Value::as_i64)
    }

    /// Route `runtime: "api"` and Codex turns through `fake-gateway`, which
    /// asks `gateway.base_url` (usually a [`ScriptedModel`]) for each step.
    pub fn gateway(mut self, gateway: Gateway) -> Self {
        self.options
            .insert("baseURL".into(), json!(gateway.base_url));
        for (name, value) in [
            ("apiKey", gateway.api_key.clone().map(Value::from)),
            (
                "defaultModel",
                gateway.default_model.clone().map(Value::from),
            ),
            ("provider", gateway.provider.clone().map(Value::from)),
            ("stream", gateway.stream.map(Value::from)),
            (
                "streamTermination",
                gateway.stream_termination.clone().map(Value::from),
            ),
            ("maxTokens", gateway.max_tokens.map(Value::from)),
        ] {
            match value {
                Some(value) => self.options.insert(name.into(), value),
                None => self.options.remove(name),
            };
        }
        self
    }

    /// Shortcut for [`ServerConfig::gateway`] with a scripted model.
    pub fn model(self, model: &ScriptedModel) -> Self {
        self.gateway(Gateway::new(model.base_url()))
    }

    /// Same as [`ServerConfig::model`] with `defaultModel` and `stream` set.
    pub fn model_with(self, model: &ScriptedModel, default_model: &str, stream: bool) -> Self {
        self.gateway(Gateway {
            default_model: Some(default_model.into()),
            stream: Some(stream),
            ..Gateway::new(model.base_url())
        })
    }

    fn harness_mut(&mut self) -> &mut Map<String, Value> {
        let harness = self.options.entry("harness").or_insert_with(|| json!({}));
        if !harness.is_object() {
            *harness = json!({});
        }
        object(harness)
    }

    /// `harness.executables.<runtime> = path` (codex, claude, opencode, ...).
    pub fn executable(mut self, runtime: &str, path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_string_lossy().into_owned();
        let harness = self.harness_mut();
        let executables = harness.entry("executables").or_insert_with(|| json!({}));
        object(executables).insert(runtime.into(), json!(path));
        self
    }

    /// Record MCP session mints and revocations through `issuer` instead of
    /// the server's own issuer (the Node `harness.issuer` option).
    pub fn issuer(mut self, issuer: &SessionIssuer) -> Self {
        self.issuer = Some(issuer.clone());
        self
    }

    /// Bind `agent` to `directory` (the `workspaces` option).
    pub fn workspace(mut self, agent: &str, directory: impl AsRef<Path>, write: bool) -> Self {
        let entry = json!({"agent": agent, "directory": directory.as_ref(), "write": write});
        match self.options.get_mut("workspaces") {
            Some(Value::Array(rows)) => rows.push(entry),
            _ => {
                self.options.insert("workspaces".into(), json!([entry]));
            }
        }
        self
    }

    pub fn script_directory(self, directory: impl AsRef<Path>) -> Self {
        let path = directory.as_ref().to_string_lossy().into_owned();
        self.option("scriptDirectory", path)
    }

    pub fn concurrency(self, value: impl Into<Value>) -> Self {
        self.option("concurrency", value)
    }

    /// Prepend a directory to the server's hermetic `PATH`.
    pub fn path_prefix(self, directory: impl AsRef<Path>) -> Self {
        let path = directory.as_ref().to_string_lossy().into_owned();
        self.option("testPathPrefix", path)
    }

    /// Extra environment for the server process.
    pub fn env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.environment.push((name.into(), value.into()));
        self
    }

    /// 32-byte sync key, stored as `testKeyring["sync-hmac/v1"]`.
    pub fn sync_key(mut self, key: [u8; 32]) -> Self {
        self.sync_key = Some(key);
        self
    }

    fn bridge(mut self, option: &'static str, config: Value, bridge: &Bridge) -> Self {
        self.bridges.retain(|(name, ..)| *name != option);
        self.bridges.push((option, config, bridge.clone()));
        self
    }

    /// Computer bridge (`testComputerBridge`): clock, peer tokens, peer HTTP,
    /// desktop runtime and hub transport operations.
    pub fn computer_bridge(self, bridge: &Bridge) -> Self {
        let config = bridge.config();
        self.bridge("testComputerBridge", config, bridge)
    }

    /// Canonical bridge (`testCanonicalBridge`) for Clerk, mail keys and
    /// routine webhook keys; the flags say which operations it serves.
    pub fn canonical_bridge(self, bridge: &Bridge, clerk: bool, mail: bool, webhook: bool) -> Self {
        let mut config = bridge.config();
        config["clerk"] = json!(clerk);
        config["mail"] = json!(mail);
        config["webhook"] = json!(webhook);
        self.bridge("testCanonicalBridge", config, bridge)
    }

    /// PDF fault bridge (`testPdfBridge`, the Node `pdfFault` option).
    pub fn pdf_bridge(self, bridge: &Bridge) -> Self {
        let config = bridge.config();
        self.bridge("testPdfBridge", config, bridge)
    }

    /// Composio endpoint plus the key bridge (`testConnectorBridge`).
    pub fn composio(self, base_url: &str, key: &Bridge) -> Self {
        let config = key.config();
        self.option("composio", json!({"baseURL": base_url}))
            .bridge("testConnectorBridge", config, key)
    }

    /// Token of the computer bridge, for `X-Minarbot-Test-Refresh`.
    pub fn computer_bridge_token(&self) -> Option<&str> {
        self.bridges
            .iter()
            .find(|(name, ..)| *name == "testComputerBridge")
            .map(|(_, _, bridge)| bridge.token())
    }

    /// Use a specific server binary instead of discovery.
    pub fn binary(mut self, path: impl Into<PathBuf>) -> Self {
        self.binary = Some(path.into());
        self
    }

    /// Final JSON for `--test-config`, exactly as the Node bridge serialized it.
    pub fn to_json(&self, database: &Path) -> Result<Value> {
        for name in self.options.keys() {
            if !SUPPORTED_OPTIONS.contains(&name.as_str()) {
                return Err(format!("not ported: option {name}").into());
            }
        }
        let mut options = self.options.clone();
        options.remove("testPathPrefix");
        options.remove("pdfFault");
        options.insert("database".into(), json!(database));
        options.entry("port").or_insert(json!(0));
        options.entry("host").or_insert(json!(Ipv4Addr::LOCALHOST));
        let legacy = options
            .get("accounts")
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .filter(|row| row.is_object())
            .cloned();
        if options.contains_key("baseURL") || legacy.is_some() {
            let pick = |name: &str| {
                options
                    .get(name)
                    .filter(|value| !value.is_null())
                    .cloned()
                    .or_else(|| legacy.as_ref().and_then(|row| row.get(name).cloned()))
                    .unwrap_or(Value::Null)
            };
            let gateway_binary = fakes::path(Fake::Gateway)?;
            let mut gateway = json!({
                "baseURL": pick("baseURL"),
                "apiKey": pick("apiKey"),
                "defaultModel": pick("defaultModel"),
                "provider": pick("provider"),
                "binary": gateway_binary,
                "stream": options.get("stream").cloned().unwrap_or(Value::Null),
                "streamTermination": options.get("streamTermination").cloned().unwrap_or(Value::Null),
                "maxTokens": options.get("maxTokens").filter(|value| value.is_number()).cloned().unwrap_or(Value::Null),
            });
            if let Value::Object(fields) = &mut gateway {
                fields.retain(|_, value| !value.is_null());
            }
            let harness = options.entry("harness").or_insert_with(|| json!({}));
            if !harness.is_object() {
                *harness = json!({});
            }
            harness["testGateway"] = gateway;
            if !harness["executables"].is_object() {
                harness["executables"] = json!({});
            }
            harness["executables"]["codex"] = json!(gateway_binary);
        }
        if let Some(key) = self.sync_key {
            use base64::Engine;
            let keyring = options.entry("testKeyring").or_insert_with(|| json!({}));
            keyring["sync-hmac/v1"] =
                json!(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key));
        }
        if let Some(issuer) = &self.issuer {
            options.insert("testHarnessBridge".into(), issuer.bridge_config());
        }
        for (name, config, _) in &self.bridges {
            options.insert((*name).into(), config.clone());
        }
        Ok(Value::Object(options))
    }

    fn path(&self) -> String {
        match self.options.get("testPathPrefix").and_then(Value::as_str) {
            Some(prefix) => format!("{prefix}:{HERMETIC_PATH}"),
            None => HERMETIC_PATH.to_owned(),
        }
    }
}

/// Mutable view of a JSON object, replacing any other value with `{}`.
fn object(value: &mut Value) -> &mut Map<String, Value> {
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    match value {
        Value::Object(fields) => fields,
        _ => unreachable!("value was replaced by an object"),
    }
}

/// One temporary directory reused by every server start and restart.
#[derive(Clone)]
pub struct TestRoot(Arc<PathBuf>);

impl TestRoot {
    pub fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!("minarbot-test-{}", Uuid::new_v4()));
        fs::create_dir(&path)?;
        Ok(Self(Arc::new(path.canonicalize()?)))
    }

    pub fn path(&self) -> &Path {
        self.0.as_path()
    }

    /// Database used by every server in this root.
    pub fn database(&self) -> PathBuf {
        self.path().join("data").join("state.sqlite")
    }

    /// Create (if needed) and return a directory inside the root.
    pub fn dir(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        let path = self.path().join(relative);
        fs::create_dir_all(&path)?;
        Ok(path)
    }

    /// Start a server on this root's database.
    pub async fn start(&self, config: ServerConfig) -> Result<Server> {
        let slot = slots().acquire_owned().await?;
        let process = spawn(self, &config).await?;
        Ok(Server {
            root: self.clone(),
            config,
            process: Some(process),
            _slot: slot,
        })
    }

    /// Start and expect startup to fail; returns the server's error output.
    pub async fn start_error(&self, config: ServerConfig) -> Result<String> {
        let _slot = slots().acquire_owned().await?;
        match spawn(self, &config).await {
            Ok(_) => Err("server started although a startup failure was expected".into()),
            Err(error) => Ok(error.to_string()),
        }
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        if Arc::strong_count(&self.0) == 1 {
            crate::reap::kill_under(self.path());
            let _ = fs::remove_dir_all(self.path());
        }
    }
}

struct Process {
    child: Child,
    origin: String,
    client: Client,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Locate `minarbot-server`: explicit config, `MINARBOT_SERVER_BINARY`, then
/// the binary next to the running test executable's target directory.
pub fn server_binary(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_owned());
    }
    if let Some(path) = std::env::var_os("MINARBOT_SERVER_BINARY") {
        return Ok(PathBuf::from(path));
    }
    let binary = fakes::target_dir()?.join("minarbot-server");
    if binary.is_file() {
        return Ok(binary);
    }
    Err(format!(
        "{} is missing: run `cargo build -p minarbot-server` before integration tests \
         or set MINARBOT_SERVER_BINARY",
        binary.display()
    )
    .into())
}

/// At most this many servers run at once per test process
/// (`MINARBOT_TEST_SERVERS`, default 2), so a parallel test run on a shared
/// host keeps each server responsive enough for its wait budgets.
fn slots() -> Arc<tokio::sync::Semaphore> {
    static SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    SLOTS
        .get_or_init(|| {
            let count = std::env::var("MINARBOT_TEST_SERVERS")
                .ok()
                .and_then(|value| value.parse().ok())
                .filter(|count| *count > 0)
                .unwrap_or(2);
            Arc::new(tokio::sync::Semaphore::new(count))
        })
        .clone()
}

async fn spawn(root: &TestRoot, config: &ServerConfig) -> Result<Process> {
    let database = root.database();
    let input = config.to_json(&database)?;
    if let Some(parent) = database.parent() {
        fs::create_dir_all(parent)?;
    }
    let tls = input.get("tls").is_some_and(|value| !value.is_null());
    let scheme = if tls { "https://" } else { "http://" };
    let mut client = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .timeout(WAIT);
    if tls {
        let cert = input["tls"]["cert"]
            .as_str()
            .ok_or("missing TLS certificate")?;
        client = client.add_root_certificate(reqwest::Certificate::from_pem(cert.as_bytes())?);
    }
    let client = client.build()?;
    let run = Uuid::new_v4();
    let logs = root.dir("logs")?;
    let config_path = logs.join(format!("{run}.json"));
    fs::write(&config_path, serde_json::to_vec(&input)?)?;
    let stdin = File::open(&config_path)?;
    fs::remove_file(&config_path)?;
    let stdout = logs.join(format!("{run}.stdout"));
    let stderr = logs.join(format!("{run}.stderr"));
    let home = root.dir("home")?;
    let mut command = Command::new(server_binary(config.binary.as_deref())?);
    command
        .arg("--test-config")
        .env_clear()
        .env("MINARBOT_TEST_SERVER", "rust")
        .env("MINARBOT_TEST_PARENT", std::process::id().to_string())
        .env("MINARBOT_HARNESS_CANARY", HARNESS_CANARY)
        .env("PATH", config.path())
        .env("HOME", &home)
        .env("TMPDIR", root.dir("tmp")?)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("LANG", "C.UTF-8");
    for (name, value) in &config.environment {
        command.env(name, value);
    }
    let child = command
        .current_dir(root.path())
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(File::create(&stdout)?))
        .stderr(Stdio::from(File::create(&stderr)?))
        .spawn()?;
    let mut process = Process {
        child,
        origin: String::new(),
        client,
        stdout,
        stderr,
    };
    let deadline = Instant::now() + STARTUP_WAIT;
    loop {
        if let Some(status) = process.child.try_wait()? {
            return Err(startup_error(
                format!("Minarbot exited during startup ({status})"),
                &process,
            ));
        }
        let output = tail(&process.stdout, 64 * 1024)?;
        if let Some(origin) = output.lines().find_map(|line| {
            line.strip_prefix("Minarbot: ")
                .map(str::trim)
                .filter(|value| value.starts_with(scheme))
        }) {
            let address: SocketAddrV4 = origin
                .strip_prefix(scheme)
                .ok_or("unexpected origin scheme")?
                .parse()?;
            if *address.ip() != Ipv4Addr::LOCALHOST || address.port() == 0 {
                return Err(startup_error(
                    "server did not bind an IPv4 loopback port".into(),
                    &process,
                ));
            }
            process.origin = origin.to_owned();
            return Ok(process);
        }
        if Instant::now() > deadline {
            return Err(startup_error(
                format!("readiness timed out after {}s", STARTUP_WAIT.as_secs()),
                &process,
            ));
        }
        sleep(Duration::from_millis(20)).await;
    }
}

fn tail(path: &Path, limit: u64) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(limit)))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn startup_error(reason: String, process: &Process) -> Error {
    let out = tail(&process.stdout, 8192).unwrap_or_default();
    let err = tail(&process.stderr, 8192).unwrap_or_default();
    format!(
        "Minarbot startup failed: {reason}\nstdout tail:\n{}\nstderr tail:\n{}",
        out.trim_end(),
        err.trim_end()
    )
    .into()
}

/// A running (or stopped) Minarbot server bound to a [`TestRoot`].
pub struct Server {
    root: TestRoot,
    config: ServerConfig,
    process: Option<Process>,
    /// Concurrency slot, kept across restarts and crashes.
    _slot: tokio::sync::OwnedSemaphorePermit,
}

impl Drop for Server {
    /// Stop the process before the root is released, so the root's cleanup
    /// sees (and kills) whatever the server left behind.
    fn drop(&mut self) {
        self.process.take();
    }
}

impl Server {
    fn process(&self) -> Result<&Process> {
        self.process
            .as_ref()
            .ok_or_else(|| "server is stopped; call restart() first".into())
    }

    /// `http://127.0.0.1:<port>` of the running process.
    pub fn origin(&self) -> &str {
        self.process.as_ref().map_or("", |process| &process.origin)
    }

    pub fn root(&self) -> &TestRoot {
        &self.root
    }

    pub fn database(&self) -> PathBuf {
        self.root.database()
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin())
    }

    /// Raw same-origin request (Origin header plus `sec-fetch-site`).
    pub fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        let process = self.process()?;
        Ok(process
            .client
            .request(method, format!("{}{path}", process.origin))
            .header(header::ORIGIN, &process.origin)
            .header("sec-fetch-site", "same-origin"))
    }

    /// Plain client without browser headers (scripts, peers, MCP callers).
    pub fn client(&self) -> Result<Client> {
        Ok(self.process()?.client.clone())
    }

    /// Send `method path` with an optional JSON body; never fails on status.
    pub async fn call(&self, method: Method, path: &str, body: Option<Value>) -> Result<Reply> {
        let mut request = self.request(method.clone(), path)?;
        if let Some(body) = body {
            request = request
                .header(header::CONTENT_TYPE, "application/json")
                .body(serde_json::to_vec(&body)?);
        }
        Reply::read(&method, path, request.send().await?).await
    }

    /// GET and require a 2xx reply.
    pub async fn get(&self, path: &str) -> Result<Value> {
        self.call(Method::GET, path, None).await?.ok()
    }

    /// GET and require exactly `status`.
    pub async fn get_expect(&self, path: &str, status: u16) -> Result<Value> {
        self.call(Method::GET, path, None).await?.expect(status)
    }

    /// POST JSON and require a 2xx reply.
    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.call(Method::POST, path, Some(body)).await?.ok()
    }

    /// POST JSON and require exactly `status`.
    pub async fn post_expect(&self, path: &str, body: Value, status: u16) -> Result<Value> {
        self.call(Method::POST, path, Some(body))
            .await?
            .expect(status)
    }

    /// Poll `GET path` until `done(&body)` holds; the timeout shows the last body.
    pub async fn wait_get(&self, path: &str, done: impl Fn(&Value) -> bool) -> Result<Value> {
        crate::wait::until_value(format!("GET {path}"), || self.get(path), done).await
    }

    /// Poll `list_path` until the row with `id` satisfies `done`.
    pub async fn wait_row(
        &self,
        list_path: &str,
        id: &str,
        done: impl Fn(&Value) -> bool,
    ) -> Result<Value> {
        crate::wait::until_value(
            format!("row {id} in {list_path}"),
            || async {
                let rows = self.get(list_path).await?;
                Ok(find_row(&rows, id))
            },
            |row| row.as_ref().is_some_and(&done),
        )
        .await?
        .ok_or_else(|| format!("row {id} disappeared from {list_path}").into())
    }

    /// Poll until the row with `id` reaches `status`.
    pub async fn wait_status(&self, list_path: &str, id: &str, status: &str) -> Result<Value> {
        self.wait_row(list_path, id, |row| row["status"] == status)
            .await
    }

    /// Current row with `id` in `list_path`, if present.
    pub async fn row(&self, list_path: &str, id: &str) -> Result<Option<Value>> {
        Ok(find_row(&self.get(list_path).await?, id))
    }

    /// SIGTERM, wait up to 5 s, then SIGKILL (the Node bridge's `close`).
    pub async fn shutdown(&mut self) -> Result<Option<ExitStatus>> {
        let Some(mut process) = self.process.take() else {
            return Ok(None);
        };
        if let Some(status) = process.child.try_wait()? {
            return Ok(Some(status));
        }
        let pid = Pid::from_raw(i32::try_from(process.child.id())?).ok_or("invalid server PID")?;
        kill_process(pid, Signal::TERM)?;
        let graceful = timeout(SHUTDOWN_GRACE, async {
            loop {
                if let Some(status) = process.child.try_wait()? {
                    return Ok::<_, Error>(status);
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        let status = match graceful {
            Ok(status) => status?,
            Err(_) => {
                process.child.kill()?;
                process.child.wait()?
            }
        };
        Ok(Some(status))
    }

    /// SIGKILL without cleanup (the Node bridge's `crash`).
    pub async fn crash(&mut self) -> Result<()> {
        let mut process = self.process.take().ok_or("server is not running")?;
        if process.child.try_wait()?.is_some() {
            return Err("server already exited before crash()".into());
        }
        process.child.kill()?;
        let status = process.child.wait()?;
        ensure_killed(status)
    }

    /// Stop gracefully and start again on the same data and config.
    pub async fn restart(&mut self) -> Result<()> {
        let config = self.config.clone();
        self.restart_with(config).await
    }

    /// Stop gracefully and start again with `config` on the same data.
    pub async fn restart_with(&mut self, config: ServerConfig) -> Result<()> {
        self.shutdown().await?;
        self.start_stopped(config).await
    }

    /// Start a server that was stopped with [`Server::shutdown`] or [`Server::crash`].
    pub async fn start_stopped(&mut self, config: ServerConfig) -> Result<()> {
        if self.process.is_some() {
            return Err("server is already running".into());
        }
        self.process = Some(spawn(&self.root, &config).await?);
        self.config = config;
        Ok(())
    }

    /// Run `edit` against the database while the server is stopped, then
    /// start it again (the Node harness fixture's `editDatabase`).
    pub async fn edit_database<T>(
        &mut self,
        edit: impl FnOnce(&crate::db::Fixture) -> Result<T>,
    ) -> Result<T> {
        self.shutdown().await?;
        let value = {
            let fixture = crate::db::Fixture::open(self.database())?;
            edit(&fixture)
        };
        let config = self.config.clone();
        self.start_stopped(config).await?;
        value
    }

    /// Re-run machine health checks now (the Node `refreshMachines`); needs
    /// [`ServerConfig::computer_bridge`].
    pub async fn refresh_machines(&self) -> Result<Value> {
        let token = self
            .config
            .computer_bridge_token()
            .ok_or("refresh_machines needs a computer bridge")?
            .to_owned();
        let response = self
            .client()?
            .get(self.url("/api/machines"))
            .header("X-Minarbot-Test-Refresh", token)
            .send()
            .await?;
        Reply::read(&Method::GET, "/api/machines", response)
            .await?
            .ok()
    }

    /// Tail of the server's stdout and stderr.
    pub fn output(&self) -> Result<(String, String)> {
        let process = self.process()?;
        Ok((
            tail(&process.stdout, 64 * 1024)?,
            tail(&process.stderr, 64 * 1024)?,
        ))
    }
}

fn ensure_killed(status: ExitStatus) -> Result<()> {
    use std::os::unix::process::ExitStatusExt;
    if status.signal() == Some(9) {
        Ok(())
    } else {
        Err(format!("server did not exit with SIGKILL ({status})").into())
    }
}

fn find_row(rows: &Value, id: &str) -> Option<Value> {
    rows.as_array()?.iter().find(|row| row["id"] == id).cloned()
}
