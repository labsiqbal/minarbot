use super::*;
use nix::{
    sys::signal::{kill, Signal},
    unistd::Pid,
};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    os::unix::fs::symlink,
    process::{Child, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

pub fn free_port() -> Result<u16> {
    let high = fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .ok()
        .and_then(|range| range.split_whitespace().next()?.parse::<u16>().ok())
        .unwrap_or(32768)
        .min(32768);
    let low = high.saturating_sub(12000).max(1024);
    if high <= low {
        return Err("No test port range below the ephemeral range.".into());
    }
    // Keep the selected port outside the kernel's ephemeral range across server restarts.
    for _ in 0..200 {
        let port = low + (Uuid::new_v4().as_u128() % u128::from(high - low)) as u16;
        if ![4317, 4318].contains(&port) && TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }
    Err("No free test port below the ephemeral range.".into())
}

pub struct Process {
    child: Child,
    log: PathBuf,
}

impl Process {
    pub fn start(command: &mut Command, log: &Path) -> Result<Self> {
        let child = command.stderr(File::create(log)?).spawn()?;
        Ok(Self {
            child,
            log: log.to_owned(),
        })
    }

    fn log(&self) -> String {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .chars()
            .rev()
            .take(4096)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    fn running(&mut self, label: &str) -> Result<()> {
        if let Some(status) = self.child.try_wait()? {
            return Err(format!("{label} exited ({status}): {}", self.log()).into());
        }
        Ok(())
    }

    pub fn stop(&mut self, timeout: Duration) -> Result<()> {
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }
        kill(Pid::from_raw(self.child.id().try_into()?), Signal::SIGTERM)?;
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(50));
        }
        self.child.kill()?;
        self.child.wait()?;
        Ok(())
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.stop(Duration::from_secs(5));
    }
}

fn health(
    client: &Client,
    process: &mut Process,
    port: u16,
    version: &str,
    timeout: u64,
    label: &str,
) -> Result<()> {
    let url = format!("http://127.0.0.1:{port}/api/environment");
    let deadline = Instant::now() + Duration::from_secs(timeout);
    while Instant::now() < deadline {
        process.running(label)?;
        if client
            .get(&url)
            .timeout(Duration::from_millis(500))
            .send()
            .and_then(|response| response.error_for_status()?.json::<Value>())
            .is_ok_and(|body| body["version"] == version)
        {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(format!("{label} timed out: {}", process.log()).into())
}

fn rss(process: &Process) -> Result<f64> {
    let status = fs::read_to_string(format!("/proc/{}/status", process.child.id()))?;
    let kib = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next())
        .ok_or("Cannot read Rust RSS from /proc")?
        .parse::<u64>()?;
    Ok(kib as f64 / 1024.0)
}

pub fn fixture_release(directory: &Path, server: &Path, version: &str) -> Result<PathBuf> {
    let releases = directory.join("releases");
    let id = format!("{version}-00000000");
    let release = releases.join(&id);
    let bin = release.join("bin");
    files::mkdir(&bin)?;
    let staged = bin.join("minarbot-server");
    copy(server, &staged)?;
    fs::set_permissions(staged, fs::Permissions::from_mode(0o700))?;
    fs::write(
        release.join("package.json"),
        serde_json::to_vec(&json!({"version": version, "type": "module"}))?,
    )?;
    fs::write(
        release.join("release.json"),
        serde_json::to_vec(
            &json!({"id": id, "version": version, "files": files::manifest(&release)?}),
        )?,
    )?;
    symlink(id, releases.join("current"))?;
    Ok(releases)
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerBudget {
    #[serde(rename = "idleRssMiB")]
    idle_rss_mi_b: f64,
    p95_latency_ms: f64,
    warmup_requests: usize,
    measured_requests: usize,
}
#[derive(Serialize, Deserialize)]
struct LauncherBudget {
    #[serde(rename = "idleRssMiB")]
    idle_rss_mi_b: f64,
}
#[derive(Deserialize)]
struct Budgets {
    rust: ServerBudget,
    #[serde(rename = "rustLauncher")]
    rust_launcher: LauncherBudget,
}

pub fn resource_command(args: &[String]) -> Result<()> {
    let [server, budgets, launcher] = args else {
        return Err("Usage: cargo xtask resource-gate <server> <budgets.json> <launcher>".into());
    };
    let budgets: Budgets = serde_json::from_slice(&fs::read(budgets)?)?;
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()?;
    let server = files::absolute(Path::new(server))?;
    let launcher = files::absolute(Path::new(launcher))?;
    if budgets.rust_launcher.idle_rss_mi_b <= 0.0 {
        return Err("Invalid Rust launcher budget: idleRssMiB".into());
    }
    let directory = scratch("minarbot-launcher-budget-")?;
    let releases = fixture_release(directory.path(), &server, "1.0.0")?;
    let port = free_port()?;
    let config = directory.path().join("launcher.json");
    files::durable(
        &config,
        &serde_json::to_vec(
            &json!({"dataDir": directory.path().join("data"), "releasesDir": releases, "port": port}),
        )?,
    )?;
    let mut child = Process::start(
        Command::new(launcher)
            .arg("--config")
            .arg(config)
            .stdout(Stdio::null()),
        &directory.path().join("launcher.log"),
    )?;
    health(
        &client,
        &mut child,
        port,
        "1.0.0",
        10,
        "Rust launcher gate readiness",
    )?;
    thread::sleep(Duration::from_millis(200));
    let ram = rss(&child)?;
    println!(
        "{}",
        json!({"rustLauncher": {"idleRssMiB": ram}, "budget": budgets.rust_launcher})
    );
    if ram > budgets.rust_launcher.idle_rss_mi_b {
        return Err("Rust launcher resource budget exceeded".into());
    }
    child.stop(Duration::from_secs(5))?;
    let budget = budgets.rust;
    if budget.idle_rss_mi_b <= 0.0
        || budget.p95_latency_ms <= 0.0
        || budget.warmup_requests == 0
        || budget.measured_requests == 0
    {
        return Err("Invalid Rust resource budget".into());
    }
    let directory = scratch("minarbot-rust-budget-")?;
    let mut child = Process::start(
        Command::new(server)
            .arg("--test-config")
            .env("MINARBOT_TEST_SERVER", "rust")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped()),
        &directory.path().join("server.log"),
    )?;
    write!(
        child
            .child
            .stdin
            .take()
            .ok_or("Missing resource gate stdin")?,
        "{}",
        json!({"database": directory.path().join("db.sqlite"), "port": 0})
    )?;
    let stdout = child
        .child
        .stdout
        .take()
        .ok_or("Missing resource gate stdout")?;
    let (sender, receiver) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let mut sent = false;
        for line in BufReader::new(stdout)
            .lines()
            .map_while(std::result::Result::ok)
        {
            if !sent {
                if let Some(origin) = line.strip_prefix("Minarbot: http://") {
                    let _ = sender.send(format!("http://{origin}"));
                    sent = true;
                }
            }
        }
    });
    let result = (|| -> Result<()> {
        let origin = receiver
            .recv_timeout(Duration::from_secs(10))
            .map_err(|error| {
                format!(
                    "Rust resource gate readiness failed ({error}): {}",
                    child.log()
                )
            })?;
        let url = format!("{origin}/api/agents");
        let request = || -> Result<()> {
            let response = client.get(&url).send()?;
            if response.status() != 200 {
                return Err(format!("Rust workload returned {}", response.status()).into());
            }
            if !response.json::<Value>()?["agents"].is_array() {
                return Err("Rust workload returned an invalid body".into());
            }
            Ok(())
        };
        for _ in 0..budget.warmup_requests {
            request()?;
        }
        let idle = rss(&child)?;
        let mut durations = Vec::new();
        for _ in 0..budget.measured_requests {
            let start = Instant::now();
            request()?;
            durations.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        durations.sort_by(f64::total_cmp);
        let p95 = durations[(durations.len() * 95).div_ceil(100) - 1];
        println!(
            "{}",
            json!({"rust": {"idleRssMiB": idle, "p95LatencyMs": p95, "requests": durations.len()}, "budget": budget})
        );
        if idle > budget.idle_rss_mi_b || p95 > budget.p95_latency_ms {
            return Err("Rust resource budget exceeded".into());
        }
        Ok(())
    })();
    child.stop(Duration::from_secs(5))?;
    reader
        .join()
        .map_err(|_| "Resource gate output reader panicked")?;
    result
}

fn descendants(pid: u32) -> Result<Vec<u32>> {
    let contents = fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))?;
    let mut children = Vec::new();
    for child in contents.split_whitespace() {
        let id = child.parse()?;
        children.push(id);
        children.extend(descendants(id)?);
    }
    Ok(children)
}

pub fn production_command(root: &Path, args: &[String]) -> Result<()> {
    if args.len() > 2 {
        return Err("Usage: cargo xtask production-env-gate [server] [launcher]".into());
    }
    let server = files::absolute(&args.first().map_or_else(
        || root.join("target/release/minarbot-server"),
        PathBuf::from,
    ))?;
    let launcher = files::absolute(&args.get(1).map_or_else(
        || root.join("target/release/minarbot-launcher"),
        PathBuf::from,
    ))?;
    let directory = private_stage("b102-", Path::new("/dev/shm"))?;
    let version = release::version(&root.join("app"))?;
    let releases = fixture_release(directory.path(), &server, &version)?;
    let client = Client::builder().no_proxy().build()?;
    for shape in ["NUC", "omarchy"] {
        let home = directory.path().join(shape);
        let data = home.join("data");
        let workspace = home.join("workspace");
        files::mkdir(&data)?;
        files::mkdir(&workspace)?;
        let port = free_port()?;
        let provider = format!("http://127.0.0.1:{}/v1", free_port()?);
        let workspaces = json!([{"agent": "00000000-0000-4000-8000-000000000001", "directory": workspace, "write": true}]).to_string();
        let shared = [
            (
                "MINARBOT_ALLOWED_ORIGINS",
                "https://minarbot.example.test:4317",
            ),
            ("MINARBOT_WORKSPACES", workspaces.as_str()),
        ];
        let mut config = json!({"dataDir": data, "releasesDir": releases, "port": port});
        let mut launcher = Command::new(&launcher);
        launcher
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .env("TMPDIR", &home)
            .stdout(Stdio::null());
        if shape == "NUC" {
            launcher
                .envs(shared)
                .env("OPENAI_BASE_URL", provider)
                .env("OPENAI_API_KEY", "dummy-production-gate")
                .env("OPENAI_MODEL", "fixture")
                .env("MINARBOT_DATA_DIR", &data)
                .env("PORT", port.to_string())
                .env("MINARBOT_STREAM", "1")
                .env("MINARBOT_STREAM_TERMINATION", "finish-reason")
                .env("MINARBOT_AGENT_DESKTOP", "1");
        } else {
            let env = home.join("launcher.env");
            let bytes = format!(
                "{}={}\n{}={}\nPATH=/usr/bin:/bin\n",
                shared[0].0, shared[0].1, shared[1].0, shared[1].1
            );
            files::durable(&env, bytes.as_bytes())?;
            config["envFile"] = json!(env);
        }
        let config_path = home.join("launcher.json");
        files::durable(&config_path, &serde_json::to_vec(&config)?)?;
        let mut child = Process::start(
            launcher.arg("--config").arg(config_path),
            &home.join("launcher.log"),
        )?;
        health(
            &client,
            &mut child,
            port,
            &version,
            15,
            "Production env startup",
        )?;
        let children = descendants(child.child.id())?;
        let result = (|| -> Result<()> {
            if children.is_empty() {
                return Err("Launcher must own a running server".into());
            }
            for pid in &children {
                let command = fs::read_to_string(format!("/proc/{pid}/cmdline"))?;
                let program = command.split('\0').next().unwrap_or_default();
                if !program.ends_with("minarbot-server") && !program.ends_with("minarbot-launcher")
                {
                    return Err(format!("Unexpected process started by launcher: {program}").into());
                }
            }
            let desktops = data.join("agent-desktops");
            if desktops.exists() && fs::read_dir(desktops)?.next().is_some() {
                return Err("Startup must not start desktops".into());
            }
            println!("{shape} production env: health OK; no desktops started");
            Ok(())
        })();
        child.stop(Duration::from_secs(8))?;
        for pid in children {
            if Path::new(&format!("/proc/{pid}")).exists() {
                return Err("Managed process survived launcher shutdown".into());
            }
        }
        result?;
    }
    Ok(())
}
