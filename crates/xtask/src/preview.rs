//! `cargo xtask preview`: run this checkout as a second Minarbot on this
//! machine, beside the installed one, to try a branch without a release.
//!
//! The preview has its own port, data directory and systemd user unit. Its
//! runtime is staged outside the checkout, so later builds or a removed
//! worktree leave the running preview alone. No lint, test or signing gate
//! runs: releases keep those.

use super::*;
use crate::leptos;
use std::time::{Duration, Instant};

const UNIT: &str = "minarbot-preview.service";
const DEFAULT_PORT: u16 = 4327;
const USAGE: &str = "cargo xtask preview [--port <port>] [--stop]";

struct Paths {
    base: PathBuf,
    runtime: PathBuf,
    data: PathBuf,
    unit: PathBuf,
}

impl Paths {
    fn new() -> Result<Self> {
        let home = PathBuf::from(env::var_os("HOME").ok_or("HOME is not set")?);
        let base = home.join(".local/share/minarbot-preview");
        Ok(Self {
            runtime: base.join("runtime"),
            data: base.join("data"),
            unit: home.join(".config/systemd/user").join(UNIT),
            base,
        })
    }
}

pub fn command(root: &Path, args: &[String]) -> Result<()> {
    let options = options(args, &["--stop"], &["--port"])
        .map_err(|error| format!("{error}. Usage: {USAGE}"))?;
    let paths = Paths::new()?;
    if options.contains_key("--stop") {
        systemctl(&["disable", "--now", UNIT])?;
        println!(
            "Stopped the preview. Data stays in {}.",
            paths.data.display()
        );
        return Ok(());
    }
    let port = match options.get("--port") {
        Some(value) => value
            .parse::<u16>()
            .ok()
            .filter(|port| *port > 0)
            .ok_or("--port must be 1..65535")?,
        None => DEFAULT_PORT,
    };
    build(root)?;
    let tailnet = tailnet_origin(port);
    install(root, &paths, port, tailnet.as_deref())?;
    wait_ready(port)?;
    println!(
        "Preview of {} is running.",
        git(root, &["rev-parse", "--short", "HEAD"])?
    );
    println!("  Local:   http://127.0.0.1:{port}");
    if let Some(origin) = &tailnet {
        println!("  Tailnet: {origin}");
    }
    println!("  Data:    {}", paths.data.display());
    println!("  Logs:    journalctl --user -u {UNIT} -f");
    Ok(())
}

fn build(root: &Path) -> Result<()> {
    let app = root.join("app");
    if !app.join("node_modules").is_dir() {
        run(Command::new("npm").current_dir(&app).arg("ci"))?;
    }
    leptos::build(root)?;
    // Pin the target directory: staging reads root/target/release.
    run(Command::new("cargo")
        .current_dir(root)
        .env("CARGO_TARGET_DIR", root.join("target"))
        .args(["build", "--release", "-p", "minarbot-server"]))
}

fn install(root: &Path, paths: &Paths, port: u16, tailnet: Option<&str>) -> Result<()> {
    files::mkdir(&paths.base)?;
    files::mkdir(&paths.data)?;
    let stage = private_stage(".runtime-", &paths.base)?;
    release::stage_runtime(&root.join("app"), stage.path())?;
    let bin = stage.path().join("bin");
    files::mkdir(&bin)?;
    copy(
        &root.join("target/release/minarbot-server"),
        &bin.join("minarbot-server"),
    )?;
    // The server reads dist next to its bin directory; swap only while stopped.
    systemctl(&["stop", UNIT]).ok();
    if paths.runtime.symlink_metadata().is_ok() {
        fs::remove_dir_all(&paths.runtime)?;
    }
    fs::rename(stage.keep(), &paths.runtime)?;
    fs::set_permissions(&paths.runtime, fs::Permissions::from_mode(0o755))?;
    if let Some(parent) = paths.unit.parent() {
        fs::create_dir_all(parent)?;
    }
    durable(&paths.unit, unit(paths, port, tailnet).as_bytes(), 0o644)?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", UNIT])?;
    systemctl(&["restart", UNIT])
}

fn unit(paths: &Paths, port: u16, tailnet: Option<&str>) -> String {
    let mut unit = format!(
        "[Unit]\n\
         Description=Minarbot preview (cargo xtask preview)\n\
         \n\
         [Service]\n\
         ExecStart={}\n\
         Environment=PORT={port}\n\
         Environment=\"MINARBOT_DATA_DIR={}\"\n",
        paths.runtime.join("bin/minarbot-server").display(),
        paths.data.display(),
    );
    if let Some(origin) = tailnet {
        unit.push_str(&format!("Environment=MINARBOT_ALLOWED_ORIGINS={origin}\n"));
    }
    unit.push_str("Restart=on-failure\n\n[Install]\nWantedBy=default.target\n");
    unit
}

fn systemctl(args: &[&str]) -> Result<()> {
    run(Command::new("systemctl").arg("--user").args(args))
}

/// Serves the port over HTTPS on the tailnet and returns its origin, or
/// `None` when Tailscale is missing or refuses.
fn tailnet_origin(port: u16) -> Option<String> {
    let status = output(Command::new("tailscale").args(["status", "--json"])).ok()?;
    let status: serde_json::Value = serde_json::from_str(&status).ok()?;
    let name = status["Self"]["DNSName"].as_str()?.trim_end_matches('.');
    if name.is_empty() {
        return None;
    }
    let serve = output(Command::new("tailscale").args([
        "serve",
        "--bg",
        &format!("--https={port}"),
        &format!("http://127.0.0.1:{port}"),
    ]));
    match serve {
        Ok(_) => Some(format!("https://{name}:{port}")),
        Err(error) => {
            eprintln!("Tailscale did not serve the preview: {error}");
            None
        }
    }
}

fn wait_ready(port: u16) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(60) {
        if client
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .is_ok_and(|response| response.status().is_success())
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(
        format!("The preview did not answer on port {port}. See journalctl --user -u {UNIT}.")
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Paths {
        Paths {
            base: PathBuf::from("/home/me/.local/share/minarbot-preview"),
            runtime: PathBuf::from("/home/me/.local/share/minarbot-preview/runtime"),
            data: PathBuf::from("/home/me/.local/share/minarbot-preview/data"),
            unit: PathBuf::from("/home/me/.config/systemd/user/minarbot-preview.service"),
        }
    }

    #[test]
    fn unit_runs_the_staged_server_on_its_own_port_and_data() -> Result<()> {
        let text = unit(&paths(), 4400, Some("https://box.example.ts.net:4400"));
        for line in [
            "ExecStart=/home/me/.local/share/minarbot-preview/runtime/bin/minarbot-server",
            "Environment=PORT=4400",
            "Environment=\"MINARBOT_DATA_DIR=/home/me/.local/share/minarbot-preview/data\"",
            "Environment=MINARBOT_ALLOWED_ORIGINS=https://box.example.ts.net:4400",
            "WantedBy=default.target",
        ] {
            if !text.lines().any(|candidate| candidate == line) {
                return Err(format!("missing {line:?} in:\n{text}").into());
            }
        }
        Ok(())
    }

    #[test]
    fn unit_without_tailnet_allows_no_extra_origin() -> Result<()> {
        let text = unit(&paths(), DEFAULT_PORT, None);
        if text.contains("MINARBOT_ALLOWED_ORIGINS") {
            return Err(format!("unexpected origin in:\n{text}").into());
        }
        Ok(())
    }
}
