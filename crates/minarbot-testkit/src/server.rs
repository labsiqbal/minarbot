use reqwest::{header, redirect::Policy, Client, RequestBuilder};
use rustix::process::{kill_process, Pid, Signal};
use serde_json::{json, Value};
use std::{
    fs::{self, File},
    io::Read,
    net::{Ipv4Addr, SocketAddrV4},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub const WAIT: Duration = Duration::from_secs(30);
const STARTUP_WAIT: Duration = Duration::from_secs(60);

/// Option names accepted by the former Node bridge and forwarded unchanged.
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

/// JSON options passed to `minarbot-server --test-config`.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    value: Value,
    binary: Option<PathBuf>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            value: json!({"fixedClock": 1_795_000_000_000_i64}),
            binary: None,
        }
    }
}

impl From<Value> for ServerConfig {
    fn from(value: Value) -> Self {
        Self {
            value,
            binary: None,
        }
    }
}

impl ServerConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        if let Value::Object(fields) = &mut self.value {
            fields.insert(name.into(), value.into());
        }
        self
    }

    pub fn fixed_clock(self, millis: i64) -> Self {
        self.set("fixedClock", millis)
    }

    pub fn with_options(mut self, options: Value) -> Self {
        if let (Value::Object(current), Value::Object(extra)) = (&mut self.value, options) {
            current.extend(extra);
        }
        self
    }

    pub fn database(self, path: impl AsRef<Path>) -> Self {
        self.set("database", path.as_ref().to_string_lossy().to_string())
    }

    pub fn binary(mut self, path: impl Into<PathBuf>) -> Self {
        self.binary = Some(path.into());
        self
    }

    fn into_parts(self) -> (Value, Option<PathBuf>) {
        (self.value, self.binary)
    }
}

/// Isolated temporary root shared by server restarts and fixture files.
#[derive(Clone)]
pub struct TestRoot(Arc<PathBuf>);

impl TestRoot {
    pub fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!("minarbot-rust-test-{}", Uuid::new_v4()));
        fs::create_dir(&path)?;
        Ok(Self(Arc::new(path)))
    }

    pub fn path(&self) -> &Path {
        self.0.as_path()
    }

    pub fn database(&self) -> PathBuf {
        self.path().join("state.sqlite")
    }

    pub async fn start(&self, config: impl Into<ServerConfig>) -> Result<Server> {
        let config = config.into();
        let (value, binary) = config.into_parts();
        self.spawn(value, binary).await
    }

    async fn spawn(&self, mut config: Value, binary: Option<PathBuf>) -> Result<Server> {
        let fields = config
            .as_object_mut()
            .ok_or("fixture config must be a JSON object")?;
        fields.insert("database".into(), json!(self.database()));
        fields.insert("host".into(), json!(Ipv4Addr::LOCALHOST));
        fields.entry("port").or_insert_with(|| json!(0));
        let tls = fields.get("tls").is_some_and(|value| !value.is_null());
        let scheme = if tls { "https://" } else { "http://" };
        let mut client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .timeout(WAIT);
        if tls {
            let cert = fields
                .get("tls")
                .and_then(|value| value.get("cert"))
                .and_then(Value::as_str)
                .ok_or("missing fixture TLS certificate")?;
            client = client.add_root_certificate(reqwest::Certificate::from_pem(cert.as_bytes())?);
        }
        let client = client.build()?;
        let run = Uuid::new_v4();
        let input = self.path().join(format!("{run}.json"));
        fs::write(&input, serde_json::to_vec(&config)?)?;
        let stdout_path = self.path().join(format!("{run}.stdout"));
        let stderr_path = self.path().join(format!("{run}.stderr"));
        let stdin = File::open(&input)?;
        fs::remove_file(&input)?;
        let executable = binary
            .clone()
            .or_else(|| std::env::var_os("MINARBOT_SERVER_BINARY").map(PathBuf::from))
            .or_else(default_binary)
            .unwrap_or_else(|| PathBuf::from("target/debug/minarbot-server"));
        let child = Command::new(executable)
            .arg("--test-config")
            .env_clear()
            .env("MINARBOT_TEST_SERVER", "rust")
            .env("MINARBOT_TEST_PARENT", std::process::id().to_string())
            .env("HOME", self.path())
            .env("TMPDIR", self.path())
            .env("XDG_CONFIG_HOME", self.path().join("config"))
            .env("XDG_CACHE_HOME", self.path().join("cache"))
            .env("XDG_DATA_HOME", self.path().join("data"))
            .current_dir(self.path())
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(File::create(&stdout_path)?))
            .stderr(Stdio::from(File::create(&stderr_path)?))
            .spawn()?;
        let mut child = ReapChild(child);
        let ready: Result<String> = timeout(STARTUP_WAIT, async {
            loop {
                if let Some(status) = child.0.try_wait()? {
                    return Err::<String, Box<dyn std::error::Error + Send + Sync>>(
                        format!("Minarbot exited during startup ({status})").into(),
                    );
                }
                if let Some(origin) = read_output(&stdout_path)?.lines().find_map(|line| {
                    line.strip_prefix("Minarbot: ")
                        .map(str::trim)
                        .filter(|value| value.starts_with(scheme))
                }) {
                    let address: SocketAddrV4 = origin
                        .strip_prefix(scheme)
                        .ok_or::<Box<dyn std::error::Error + Send + Sync>>(
                            "unexpected fixture origin scheme".into(),
                        )?
                        .parse()?;
                    if *address.ip() != Ipv4Addr::LOCALHOST || address.port() == 0 {
                        return Err::<String, Box<dyn std::error::Error + Send + Sync>>(
                            "fixture did not bind an IPv4 loopback port".into(),
                        );
                    }
                    client
                        .get(format!("{origin}/api/test-readiness"))
                        .send()
                        .await?;
                    return Ok(origin.to_owned());
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|error| startup_error(error, &stdout_path, &stderr_path))?;
        let origin = ready.map_err(|error| startup_error(error, &stdout_path, &stderr_path))?;
        Ok(Server {
            root: self.clone(),
            child,
            origin,
            client,
            config,
            binary,
        })
    }
}

fn default_binary() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let target = executable.parent()?.parent()?;
    let binary = target.join("minarbot-server");
    binary.is_file().then_some(binary)
}

fn read_output(path: &Path) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    File::open(path)?.take(16_384).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn startup_error(
    error: impl std::fmt::Display,
    stdout: &Path,
    stderr: &Path,
) -> Box<dyn std::error::Error + Send + Sync> {
    let out =
        read_output(stdout).unwrap_or_else(|read_error| format!("<read failed: {read_error}>"));
    let err =
        read_output(stderr).unwrap_or_else(|read_error| format!("<read failed: {read_error}>"));
    format!("Minarbot readiness failed: {error}\nstdout:\n{out}\nstderr:\n{err}").into()
}

/// Running Minarbot process and same-origin HTTP client.
pub struct Server {
    root: TestRoot,
    child: ReapChild,
    pub origin: String,
    pub client: Client,
    config: Value,
    binary: Option<PathBuf>,
}

impl Server {
    pub fn get(&self, path: &str) -> RequestBuilder {
        self.request(reqwest::Method::GET, path)
    }

    pub fn post(&self, path: &str) -> RequestBuilder {
        self.request(reqwest::Method::POST, path)
    }

    pub fn put(&self, path: &str) -> RequestBuilder {
        self.request(reqwest::Method::PUT, path)
    }

    pub fn delete(&self, path: &str) -> RequestBuilder {
        self.request(reqwest::Method::DELETE, path)
    }

    fn request(&self, method: reqwest::Method, path: &str) -> RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.origin))
            .header(header::ORIGIN, &self.origin)
            .header("sec-fetch-site", "same-origin")
    }

    pub async fn restart(&mut self, config: impl Into<ServerConfig>) -> Result<()> {
        self.shutdown().await?;
        let config = config.into();
        let (mut value, binary) = config.into_parts();
        if !value.is_object() {
            return Err("fixture config must be a JSON object".into());
        }
        if let Value::Object(fields) = &mut value {
            if fields.get("fixedClock").is_none() {
                if let Some(clock) = self.config.get("fixedClock") {
                    fields.insert("fixedClock".into(), clock.clone());
                }
            }
        }
        let next = self
            .root
            .spawn(value, binary.or_else(|| self.binary.clone()))
            .await?;
        self.child = next.child;
        self.origin = next.origin;
        self.client = next.client;
        self.config = next.config;
        self.binary = next.binary;
        Ok(())
    }

    pub async fn shutdown(&mut self) -> Result<ExitStatus> {
        if let Some(status) = self.child.0.try_wait()? {
            return Ok(status);
        }
        let pid = Pid::from_raw(i32::try_from(self.child.0.id())?).ok_or("invalid child PID")?;
        kill_process(pid, Signal::TERM)?;
        let waited: Result<ExitStatus> = timeout(WAIT, async {
            loop {
                if let Some(status) = self.child.0.try_wait()? {
                    return Ok::<ExitStatus, Box<dyn std::error::Error + Send + Sync>>(status);
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|_| "timed out waiting for Minarbot shutdown")?;
        waited
    }

    pub async fn clean_shutdown(&mut self) -> Result<ExitStatus> {
        self.shutdown().await
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        if Arc::strong_count(&self.0) == 1 {
            let _ = fs::remove_dir_all(self.path());
        }
    }
}

struct ReapChild(Child);

impl Drop for ReapChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
