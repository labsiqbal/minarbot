use reqwest::{header, redirect::Policy, Client, RequestBuilder};
use rustix::process::{kill_process, Pid, Signal};
use serde_json::{json, Value};
use std::{
    ffi::OsStr,
    fs::{self, DirBuilder, File},
    io::Read,
    net::{Ipv4Addr, SocketAddrV4},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus},
    time::Duration,
};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub const WAIT: Duration = Duration::from_secs(15);
const STARTUP_WAIT: Duration = Duration::from_secs(60);

pub struct TestRoot(PathBuf);

impl TestRoot {
    pub fn new() -> Result<Self> {
        let path = std::env::temp_dir()
            .canonicalize()?
            .join(format!("minarbot-http-{}", Uuid::new_v4()));
        DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn database(&self) -> PathBuf {
        self.path().join("state.sqlite")
    }

    pub async fn start(&self, config: Value) -> Result<Server<'_>> {
        self.start_with_env(config, &[]).await
    }

    pub async fn start_with_env(
        &self,
        mut config: Value,
        environment: &[(&str, &OsStr)],
    ) -> Result<Server<'_>> {
        let tls = config.get("tls").is_some_and(|value| !value.is_null());
        let scheme = if tls { "https://" } else { "http://" };
        let fields = config
            .as_object_mut()
            .ok_or("fixture config must be JSON object")?;
        fields.insert("database".into(), json!(self.database()));
        fields.insert("host".into(), json!(Ipv4Addr::LOCALHOST));
        fields.entry("port").or_insert_with(|| json!(0));
        let mut client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .timeout(WAIT);
        if tls {
            let cert = config["tls"]["cert"]
                .as_str()
                .ok_or("missing fixture certificate")?;
            client = client.add_root_certificate(reqwest::Certificate::from_pem(cert.as_bytes())?);
        }
        let client = client.build()?;

        // A file on stdin cannot block the harness on a full pipe during startup.
        let run = Uuid::new_v4();
        let input = self.path().join(format!("{run}.json"));
        fs::write(&input, serde_json::to_vec(&config)?)?;
        let stdin = File::open(&input)?;
        fs::remove_file(input)?;
        let stdout = self.path().join(format!("{run}.stdout"));
        let stderr = self.path().join(format!("{run}.stderr"));
        let mut child = ReapChild(
            Command::new(env!("CARGO_BIN_EXE_minarbot-server"))
                .arg("--test-config")
                .env_clear()
                .env("MINARBOT_TEST_SERVER", "rust")
                .env("MINARBOT_TEST_PARENT", std::process::id().to_string())
                .env("HOME", self.path())
                .env("TMPDIR", self.path())
                .env("XDG_CONFIG_HOME", self.path().join("config"))
                .env("XDG_CACHE_HOME", self.path().join("cache"))
                .env("XDG_DATA_HOME", self.path().join("data"))
                .envs(environment.iter().copied())
                .current_dir(self.path())
                .stdin(stdin)
                .stdout(File::create(&stdout)?)
                .stderr(File::create(&stderr)?)
                .spawn()?,
        );
        // Database initialization and migrations can wait on shared disk I/O.
        let ready = timeout(STARTUP_WAIT, async {
            loop {
                if let Some(status) = child.0.try_wait()? {
                    return Err(format!("Minarbot exited during startup ({status})").into());
                }
                if let Some(origin) = output(&stdout)?
                    .split_inclusive('\n')
                    .find_map(|line| line.strip_suffix('\n')?.strip_prefix("Minarbot: "))
                {
                    let address: SocketAddrV4 = origin
                        .strip_prefix(scheme)
                        .ok_or("unexpected loopback origin scheme")?
                        .parse()?;
                    if *address.ip() != Ipv4Addr::LOCALHOST || address.port() == 0 {
                        return Err("expected a bound IPv4 loopback port".into());
                    }
                    // Serving a request proves the shutdown handler is ready too.
                    client
                        .get(format!("{origin}/api/test-readiness"))
                        .send()
                        .await?;
                    return Result::Ok(origin.to_owned());
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        let origin = ready
            .map_err(|error| startup_error(error, &stdout, &stderr))?
            .map_err(|error| startup_error(error, &stdout, &stderr))?;
        Ok(Server {
            _root: self,
            child,
            origin,
            client,
        })
    }
}

fn output(path: &Path) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    File::open(path)?.take(8192).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn startup_error(
    error: impl std::fmt::Display,
    stdout: &Path,
    stderr: &Path,
) -> Box<dyn std::error::Error + Send + Sync> {
    let stdout = output(stdout).unwrap_or_default();
    let stderr = output(stderr).unwrap_or_default();
    format!("Minarbot readiness failed: {error}\nstdout:\n{stdout}\nstderr:\n{stderr}").into()
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.path());
    }
}

struct ReapChild(Child);

impl Drop for ReapChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub struct Server<'root> {
    _root: &'root TestRoot,
    child: ReapChild,
    pub origin: String,
    pub client: Client,
}

impl Server<'_> {
    pub fn get(&self, path: &str) -> RequestBuilder {
        self.client.get(format!("{}{path}", self.origin))
    }

    pub fn post(&self, path: &str) -> RequestBuilder {
        self.client
            .post(format!("{}{path}", self.origin))
            .header(header::ORIGIN, &self.origin)
    }

    pub async fn terminate(mut self, signal: Signal) -> Result<ExitStatus> {
        if let Some(status) = self.child.0.try_wait()? {
            return Err(format!("Minarbot exited before shutdown ({status})").into());
        }
        let raw = i32::try_from(self.child.0.id())?;
        kill_process(Pid::from_raw(raw).ok_or("invalid child PID")?, signal)?;
        timeout(WAIT, async {
            loop {
                if let Some(status) = self.child.0.try_wait()? {
                    return Ok(status);
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await?
    }
}
