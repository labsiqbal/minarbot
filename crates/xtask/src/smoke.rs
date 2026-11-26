use super::*;
use axum::{
    body::Body,
    extract::State,
    http::{header::CONTENT_TYPE, Request, StatusCode},
    response::{IntoResponse, Response},
    Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use nix::{
    sys::signal::{kill, Signal},
    unistd::Pid,
};
use regex::Regex;
use reqwest::blocking::Client;
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use serde_json::{json, Value};
use std::{
    net::TcpListener,
    process::Stdio,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};
use uuid::Uuid;

struct Fixture {
    archive: Vec<u8>,
    archive_url: String,
    manifest: String,
    signature: String,
    mode: AtomicU8,
}

async fn respond(State(fixture): State<Arc<Fixture>>, request: Request<Body>) -> Response {
    let mode = fixture.mode.load(Ordering::Relaxed);
    let (status, body) = match request.uri().path() {
        "/latest/download/latest.json" => (200, fixture.manifest.clone().into_bytes()),
        "/latest/download/latest.json.sig" if mode == 1 => {
            (200, STANDARD.encode([0; 64]).into_bytes())
        }
        "/latest/download/latest.json.sig" => (200, fixture.signature.clone().into_bytes()),
        path if path == fixture.archive_url && mode == 2 => (200, vec![0; fixture.archive.len()]),
        path if path == fixture.archive_url => (200, fixture.archive.clone()),
        _ => (404, Vec::new()),
    };
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        [(CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

struct Service {
    home: PathBuf,
    launch_domain: Option<String>,
}

impl Drop for Service {
    fn drop(&mut self) {
        if let Some(domain) = &self.launch_domain {
            let _ = Command::new("launchctl")
                .env("HOME", &self.home)
                .args(["bootout", &format!("{domain}/com.minarbot.server")])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            return;
        }
        let pid = fs::read_to_string(self.home.join("launcher.pid"))
            .ok()
            .and_then(|pid| pid.trim().parse::<i32>().ok())
            .filter(|pid| *pid > 0);
        if let Some(pid) = pid {
            let expected = self
                .home
                .join(".local/share/minarbot/releases/current/bin/minarbot-launcher");
            let path = format!("/proc/{pid}/cmdline");
            if fs::read_to_string(&path).is_ok_and(|args| {
                args.split('\0')
                    .next()
                    .is_some_and(|program| Path::new(program) == expected)
            }) {
                let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
                for _ in 0..30 {
                    if fs::read_to_string(&path).map_or(true, |args| args.is_empty()) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
}

fn request(client: &Client, origin: &str, path: &str, body: Option<Value>) -> Result<Value> {
    let url = format!("{origin}{path}");
    let request = match body {
        Some(body) => client.post(url).header("Origin", origin).json(&body),
        None => client.get(url),
    };
    let response = request.send()?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(format!("{path}: HTTP {status}: {body}").into());
    }
    Ok(response.json()?)
}

pub fn command(root: &Path, args: &[String]) -> Result<()> {
    if args.len() > 1 {
        return Err("Usage: cargo xtask install-smoke [binary-dir]".into());
    }
    let binaries = args
        .first()
        .map_or_else(|| root.join("target/debug"), PathBuf::from);
    let scratch = scratch("minarbot-install-")?;
    let scratch = scratch.path();
    let stage = scratch.join("content");
    fs::create_dir_all(stage.join("bin"))?;
    for name in ["minarbot-server", "minarbot-launcher"] {
        copy(&binaries.join(name), &stage.join("bin").join(name))?;
    }
    let codex = fs::canonicalize(binaries.join("fake-codex"))
        .map_err(|_| "Missing fake-codex beside the server: build minarbot-testkit first")?;
    let app = root.join("app");
    let version = release::version(&app)?;
    crate::leptos::require_build(&app)?;
    release::stage_runtime(&app, &stage)?;
    let commit = "1234567890abcdef1234567890abcdef12345678";
    let id = format!("{version}-{}", &commit[..8]);
    fs::write(
        stage.join("release.json"),
        serde_json::to_vec(
            &json!({"id": id, "version": version, "commit": commit, "files": files::manifest(&stage)?}),
        )?,
    )?;
    let name = format!(
        "minarbot-{version}-{}-{}.tar.gz",
        env::consts::OS,
        env::consts::ARCH
    );
    let archive_file = scratch.join(&name);
    archive(&stage, &archive_file)?;
    let archive = fs::read(&archive_file)?;
    let manifest = json!({"id": id, "version": version, "commit": commit, "assets": { &name: {"sha256": sha256(&archive_file)?, "size": archive.len() }}}).to_string();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| "Cannot generate smoke-test Ed25519 key")?;
    let key =
        Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| "Invalid smoke-test Ed25519 key")?;
    let signature = STANDARD.encode(key.sign(manifest.as_bytes()).as_ref());
    let public_key = STANDARD.encode(key.public_key().as_ref());
    let script = release::pattern(r"(?m)^PUBLIC_KEY = '.*'$");
    let original = fs::read_to_string(root.join("scripts/install.sh"))?;
    if !script.is_match(&original) {
        return Err("Installer is missing its release public key".into());
    }
    let installer = scratch.join("install.sh");
    fs::write(
        &installer,
        script
            .replace(&original, format!("PUBLIC_KEY = '{public_key}'"))
            .as_bytes(),
    )?;
    let fixture = Arc::new(Fixture {
        archive,
        archive_url: format!("/download/v{version}/{name}"),
        manifest,
        signature,
        mode: AtomicU8::new(0),
    });
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let base = format!("http://{}", listener.local_addr()?);
    let router = Router::new().fallback(respond).with_state(fixture.clone());
    let (shutdown, stop) = tokio::sync::oneshot::channel::<()>();
    let server = thread::spawn(move || -> std::io::Result<()> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let listener = tokio::net::TcpListener::from_std(listener)?;
                axum::serve(listener, router)
                    .with_graceful_shutdown(async {
                        let _ = stop.await;
                    })
                    .await
            })
    });
    let result = exercise(
        root, scratch, &installer, &base, &fixture, &version, &id, &codex,
    );
    let _ = shutdown.send(());
    server
        .join()
        .map_err(|_| "Installer fixture HTTP server panicked")??;
    result
}

#[allow(clippy::too_many_arguments)]
fn exercise(
    root: &Path,
    scratch: &Path,
    installer: &Path,
    base: &str,
    fixture: &Fixture,
    version: &str,
    id: &str,
    codex: &Path,
) -> Result<()> {
    let mac = cfg!(target_os = "macos");
    let launch_domain = if mac {
        let user = output(Command::new("id").arg("-u"))?;
        let mut domain = format!("gui/{}", user.trim());
        if output(Command::new("launchctl").args(["print", &domain])).is_err() {
            domain = format!("user/{}", user.trim());
        }
        if output(
            Command::new("launchctl").args(["print", &format!("{domain}/com.minarbot.server")]),
        )
        .is_ok()
        {
            return Err("Smoke test requires an unused Minarbot launchd label.".into());
        }
        Some(domain)
    } else {
        None
    };
    let bin = scratch.join("path");
    files::mkdir(&bin)?;
    durable(
        &bin.join(if mac { "open" } else { "xdg-open" }),
        b"#!/bin/sh\nprintf '%s\\n' \"$1\" > \"$HOME/opened-url\"\n",
        0o700,
    )?;
    durable(&bin.join("systemctl"), br#"#!/bin/sh
set -eu
[ "$*" = '--user daemon-reload' ] && exit 0
[ "$*" = '--user enable --now minarbot.service' ]
test -s "$HOME/.config/systemd/user/minarbot.service"
[ ! -f "$HOME/launcher.pid" ] || exit 0
nohup "$HOME/.local/share/minarbot/releases/current/bin/minarbot-launcher" --config "$HOME/.config/minarbot/launcher.json" >"$HOME/server.log" 2>&1 </dev/null &
echo "$!" > "$HOME/launcher.pid"
"#, 0o700)?;
    let path = env::join_paths(
        std::iter::once(bin).chain(
            env::var_os("PATH")
                .as_deref()
                .into_iter()
                .flat_map(env::split_paths),
        ),
    )?;
    let home = scratch.join("home");
    let service = Service {
        home: home.clone(),
        launch_domain,
    };
    let config = home.join(".config/minarbot");
    files::mkdir(&config)?;
    let env_file = config.join("launcher.env");
    let config_file = config.join("launcher.json");
    let current = home.join(".local/share/minarbot/releases/current");
    let port = gates::free_port()?;
    let launch = |home: &Path, port: u16| {
        let mut command = Command::new("sh");
        command
            .env_clear()
            .env("PATH", &path)
            .env("HOME", home)
            .env("MINARBOT_RELEASE_BASE", base)
            .arg(installer)
            .args(["--port", &port.to_string()]);
        command
    };
    for (mode, expected) in [
        (1, "Invalid GitHub release signature"),
        (2, "SHA-256 mismatch"),
    ] {
        fixture.mode.store(mode, Ordering::Relaxed);
        let result = launch(&home, port).output()?;
        if result.status.success()
            || !String::from_utf8_lossy(&result.stderr).contains(expected)
            || config_file.exists()
            || current.exists()
        {
            return Err(format!(
                "Installer did not reject invalid release ({expected}): {}",
                String::from_utf8_lossy(&result.stderr)
            )
            .into());
        }
    }
    fixture.mode.store(0, Ordering::Relaxed);
    let environment = "MINARBOT_SMOKE_CONFIG=private\n";
    durable(&env_file, environment.as_bytes(), 0o600)?;
    let stdout = output(&mut launch(&home, port))?;
    if !stdout.contains("is ready at") {
        return Err("Installer did not report readiness".into());
    }
    let origin = format!("http://127.0.0.1:{port}");
    let opened = home.join("opened-url");
    for _ in 0..30 {
        if opened.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    if fs::read_to_string(&opened)?.trim() != origin
        || fs::read_link(&current)? != Path::new(id)
        || fs::metadata(&config_file)?.permissions().mode() & 0o777 != 0o600
        || fs::read_to_string(&env_file)? != environment
    {
        return Err(
            "Installer did not retain private config, select the release and open the origin"
                .into(),
        );
    }
    if !mac
        && fs::read(home.join(".config/systemd/user/minarbot.service"))?
            != fs::read(root.join("scripts/minarbot.service"))?
    {
        return Err("Installer wrote a different systemd unit".into());
    }
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()?;
    if request(&client, &origin, "/api/environment", None)?["version"] != version {
        return Err("Installed version mismatch".into());
    }
    let html = client.get(&origin).send()?.error_for_status()?.text()?;
    if !html.contains("id=\"leptos-root\"") {
        return Err("Installed UI did not select Leptos by default".into());
    }
    let legacy_query = client
        .get(format!("{origin}/?ui=react"))
        .send()?
        .error_for_status()?
        .text()?;
    if !legacy_query.contains("id=\"leptos-root\"") {
        return Err("Installed UI did not ignore the retired query override".into());
    }
    let pattern: Regex = release::pattern(r#"<script[^>]+src="([^"\s]+)""#);
    let bundle = pattern
        .captures(&html)
        .ok_or("Missing installed UI bundle")?;
    let url = reqwest::Url::parse(&origin)?.join(&bundle[1])?;
    client.get(url).send()?.error_for_status()?;
    // Turns run through a provider CLI (#87). The kit's fake Codex is signed
    // in when its home holds auth.json and replies "finished".
    let codex_home = scratch.join("codex-home");
    files::mkdir(&codex_home)?;
    durable(&codex_home.join("auth.json"), b"{}", 0o600)?;
    request(
        &client,
        &origin,
        "/api/provider-instances",
        Some(
            json!({"id": Uuid::new_v4(), "instanceId": "smoke-codex", "kind": "codex",
            "displayName": "Smoke Codex", "enabled": true, "binaryPath": codex,
            "homePath": codex_home}),
        ),
    )?;
    let agent = Uuid::new_v4();
    let turn = Uuid::new_v4().to_string();
    let turns = format!("/api/agents/{agent}/turns");
    request(
        &client,
        &origin,
        "/api/agents",
        Some(
            json!({"id": agent, "name": "Smoke", "instructions": "Answer.", "model": "fixture", "providerInstance": "smoke-codex"}),
        ),
    )?;
    if mac {
        let result = request(
            &client,
            &origin,
            &format!("/api/agents/{agent}/computer"),
            None,
        )?;
        if result["available"] != false
            || result["reason"] != "Computer is unavailable on this platform."
        {
            return Err("Installed macOS runtime should report Computer unavailable".into());
        }
    }
    request(
        &client,
        &origin,
        &turns,
        Some(json!({"id": turn, "text": "Reply."})),
    )?;
    let mut ready = false;
    let mut last = Value::Null;
    for _ in 0..300 {
        let rows = request(&client, &origin, &turns, None)?;
        if let Some(row) = rows
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["id"] == turn))
        {
            if row["status"] == "failed" {
                return Err(format!("Installed provider turn failed: {}", row["error"]).into());
            }
            last = row.clone();
            if row["status"] == "done" {
                ready = row["reply"] == "finished";
                break;
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    if !ready {
        return Err(
            format!("Installed provider turn did not return the fixture reply: {last}").into(),
        );
    }
    output(&mut launch(&home, port))?;
    if !mac {
        let response = request(
            &client,
            &origin,
            "/api/install/tokens",
            Some(json!({"id": Uuid::new_v4()})),
        )?;
        let token = response["token"]
            .as_str()
            .ok_or("Missing smoke-test pairing token")?;
        let peer = Service {
            home: scratch.join("peer"),
            launch_domain: None,
        };
        files::mkdir(&peer.home)?;
        let port = gates::free_port()?;
        output(launch(&peer.home, port).args([
            "--no-open",
            "--source",
            &origin,
            "--token",
            token,
        ]))?;
        let peer_origin = format!("http://127.0.0.1:{port}");
        for (local, other) in [(&origin, &peer_origin), (&peer_origin, &origin)] {
            let response = request(&client, local, "/api/machines", None)?;
            if !response["machines"].as_array().is_some_and(|machines| {
                machines.iter().any(|machine| {
                    machine["origin"] == *other && machine["pairingState"] == "paired"
                })
            }) {
                return Err("Installer peer did not pair on both machines".into());
            }
        }
    }
    drop(service);
    println!("Installer smoke passed: signature, digest, private config, service, UI, fake provider turn, rerun{}",
        if mac { "." } else { ", pairing." });
    Ok(())
}
