use crate::connection::{origin, Result};
#[cfg(windows)]
use std::path::Path;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use url::Url;

pub const DEFAULT_PORT: u16 = 4317;
const RUNTIME_SHA256: Option<&str> = option_env!("MINARBOT_DESKTOP_RUNTIME_SHA256");

#[derive(Default)]
pub struct Local {
    startup: tokio::sync::Mutex<()>,
    #[cfg(unix)]
    child: std::sync::Mutex<Option<std::process::Child>>,
}

impl Local {
    pub async fn start(&self, app: &AppHandle) -> Result<Url> {
        let _startup = self.startup.lock().await;
        let resource = app.path().resource_dir()?;
        #[cfg(unix)]
        {
            native::start(self, resource).await
        }
        #[cfg(windows)]
        {
            start_wsl(&resource, &app.package_info().version.to_string()).await
        }
    }

    pub fn is_managed(url: &Url) -> bool {
        if url.scheme() != "http" || !crate::connection::loopback(url) {
            return false;
        }
        #[cfg(unix)]
        let port = native::installed_port();
        #[cfg(windows)]
        let port = Some(DEFAULT_PORT);
        port.is_some_and(|port| url.port_or_known_default() == Some(port))
    }

    pub fn stop(&self) {
        #[cfg(unix)]
        native::stop(self);
    }
}

fn local(port: u16) -> Result<Url> {
    origin(&format!("http://127.0.0.1:{port}/"))
}

#[cfg(windows)]
async fn start_wsl(resources: &Path, version: &str) -> Result<Url> {
    use std::os::windows::process::CommandExt;

    let windows = std::env::var_os("SystemRoot").ok_or("SystemRoot is not set.")?;
    let powershell =
        std::path::PathBuf::from(windows).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let installer = resources.join("install.ps1");
    let shell = resources.join("install.sh");
    let archive = resources.join("runtime.tar.gz");
    let digest = RUNTIME_SHA256.ok_or("Bundled runtime is unavailable.")?;
    if !installer.is_file() || !shell.is_file() || !archive.is_file() {
        return Err("Bundled WSL installer is missing.".into());
    }
    let mut command = std::process::Command::new(powershell);
    command.creation_flags(0x08000000);
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(installer)
        .args(["-NoOpen", "-InstallerPath"])
        .arg(shell)
        .arg("-ArchivePath")
        .arg(archive)
        .args(["-Tag", &format!("v{version}"), "-ArchiveSha256", digest]);
    let output = tokio::time::timeout(
        Duration::from_secs(195),
        tokio::process::Command::from(command)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "WSL startup timed out. Check Ubuntu and port 4317.")??;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("wsl --install -d Ubuntu") {
            return Err("Install WSL 2 and open Ubuntu once to create your Linux user.".into());
        }
        if stderr.contains("sudo apt install curl python3 openssl") {
            return Err(
                "In Ubuntu, run sudo apt update and sudo apt install curl python3 openssl.".into(),
            );
        }
        return Err(
            "Could not start Minarbot in WSL. Check Ubuntu network access and port 4317.".into(),
        );
    }
    local(DEFAULT_PORT)
}

#[cfg(unix)]
mod native {
    use super::*;
    use flate2::read::GzDecoder;
    use minarbot_launcher::files;
    use nix::{
        libc::O_NOFOLLOW,
        sys::signal::{kill, Signal},
        unistd::Pid,
    };
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::path::Path;
    use std::{
        fs::{self, File, OpenOptions},
        io::{self, Read, Seek},
        net::{Ipv4Addr, TcpListener},
        os::unix::fs::OpenOptionsExt,
        path::PathBuf,
        process::{Command, Stdio},
    };

    const LIMIT: u64 = 1 << 30;

    struct Install {
        config: PathBuf,
        launcher: PathBuf,
        version: String,
        port: u16,
        existed: bool,
    }

    fn home() -> Result<PathBuf> {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .ok_or_else(|| "HOME must be absolute.".into())
    }

    fn config_port(settings: &serde_json::Value, default: u16) -> Result<u16> {
        if settings["port"].is_null() {
            return Ok(default);
        }
        settings["port"]
            .as_f64()
            .filter(|value| value.fract() == 0.0 && (1.0..=65535.0).contains(value))
            .map(|value| value as u16)
            .ok_or_else(|| "Invalid launcher port.".into())
    }

    pub fn installed_port() -> Option<u16> {
        let path = home().ok()?.join(".config/minarbot/launcher.json");
        files::private_file(&path).ok()?;
        config_port(&files::json(&path).ok()?, DEFAULT_PORT).ok()
    }

    fn file_error(error: Box<dyn std::error::Error>) -> io::Error {
        io::Error::other(error.to_string())
    }

    fn install(
        home: &Path,
        archive: &Path,
        digest: Option<&str>,
        default_port: u16,
    ) -> Result<Install> {
        let root = home.join(".local/share/minarbot");
        let config = home.join(".config/minarbot/launcher.json");
        let env = config.with_file_name("launcher.env");
        let parent = config
            .parent()
            .ok_or("Missing launcher configuration directory.")?;
        let mut releases = root.join("releases");
        let mut port = default_port;
        let existing_config = config.symlink_metadata().is_ok();
        if existing_config {
            files::private_file(&config).map_err(file_error)?;
            let settings = files::json(&config).map_err(file_error)?;
            if !settings.is_object() {
                return Err("Invalid launcher.json.".into());
            }
            port = config_port(&settings, DEFAULT_PORT)?;
            if !settings["releasesDir"].is_null() {
                releases = PathBuf::from(
                    settings["releasesDir"]
                        .as_str()
                        .ok_or("Invalid releases directory.")?,
                );
                if !releases.is_absolute() {
                    return Err("Launcher releases directory must be absolute.".into());
                }
            }
        }
        files::mkdir(&releases).map_err(file_error)?;
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(O_NOFOLLOW)
            .open(releases.join(".install.lock"))?;
        lock.lock()?;
        let existing = files::selection(&releases, "current").map_err(file_error)?;
        let existed = existing.is_some();
        let id = if let Some(id) = existing {
            id
        } else {
            // Check the port before a fresh install so another process cannot be mistaken for our server.
            TcpListener::bind((Ipv4Addr::LOCALHOST, port)).map_err(|_| {
                format!("Port {port} is in use. Free it or connect to a server manually.")
            })?;
            extract(
                &releases,
                archive,
                digest.ok_or("Bundled runtime is unavailable.")?,
            )?
        };
        if !existing_config {
            files::mkdir(parent).map_err(file_error)?;
            if env
                .symlink_metadata()
                .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
            {
                files::durable(
                    &env,
                    b"# Optional literal environment values for Minarbot.\n",
                )
                .map_err(file_error)?;
            }
            files::private_file(&env).map_err(file_error)?;
            files::durable(&config, &serde_json::to_vec(&json!({
                "dataDir": root.join("data"), "releasesDir": releases, "envFile": env, "port": port
            }))?).map_err(file_error)?;
        }
        let version = id
            .rsplit_once('-')
            .ok_or("Invalid release ID.")?
            .0
            .to_owned();
        Ok(Install {
            config,
            launcher: releases.join("current/bin/minarbot-launcher"),
            version,
            port,
            existed,
        })
    }

    fn extract(releases: &Path, source: &Path, digest: &str) -> Result<String> {
        let mut archive = File::open(source)?;
        let size = archive.metadata()?.len();
        if size == 0 || size > LIMIT {
            return Err("Invalid bundled runtime size.".into());
        }
        let mut hasher = Sha256::new();
        io::copy(&mut (&mut archive).take(LIMIT + 1), &mut hasher)?;
        if format!("{:x}", hasher.finalize()) != digest {
            return Err("Bundled runtime SHA-256 mismatch.".into());
        }
        archive.rewind()?;
        let stage = tempfile::tempdir_in(releases)?;
        let unpacked = stage.path().join("runtime");
        fs::create_dir(&unpacked)?;
        let mut total = 0_u64;
        for entry in tar::Archive::new(GzDecoder::new(archive)).entries()? {
            let mut entry = entry?;
            let kind = entry.header().entry_type();
            total = total.saturating_add(entry.size());
            if !(kind.is_file() || kind.is_dir())
                || total > 2 * LIMIT
                || !entry.unpack_in(&unpacked)?
            {
                return Err("Invalid bundled runtime archive.".into());
            }
        }
        let info = files::json(&unpacked.join("release.json")).map_err(file_error)?;
        let id = info["id"]
            .as_str()
            .filter(|id| files::release_id(id))
            .ok_or("Invalid bundled release ID.")?;
        let verified = stage.path().join(id);
        fs::rename(&unpacked, &verified)?;
        files::verify(stage.path(), id).map_err(file_error)?;
        let destination = releases.join(id);
        if destination.symlink_metadata().is_ok() {
            if files::verify(releases, id).map_err(file_error)? != info {
                return Err("Existing runtime differs from bundled release.".into());
            }
        } else {
            fs::rename(verified, destination)?;
        }
        files::select(releases, "current", id).map_err(file_error)?;
        Ok(id.to_owned())
    }

    async fn healthy(client: &reqwest::Client, url: &Url, version: &str) -> bool {
        let Ok(endpoint) = url.join("/api/environment") else {
            return false;
        };
        let Ok(response) = client.get(endpoint).send().await else {
            return false;
        };
        if !response.status().is_success() {
            return false;
        }
        super::super::connection::identity(response)
            .await
            .is_ok_and(|value| {
                value["version"] == version && value["id"].as_str().is_some_and(|id| !id.is_empty())
            })
    }

    pub async fn start(owner: &Local, resource: PathBuf) -> Result<Url> {
        let home = home()?;
        let port = option_env!("MINARBOT_DESKTOP_FIXTURE_PORT")
            .map(str::parse)
            .transpose()?
            .unwrap_or(DEFAULT_PORT);
        let install = tokio::task::spawn_blocking(move || {
            install(
                &home,
                &resource.join("runtime.tar.gz"),
                RUNTIME_SHA256,
                port,
            )
        })
        .await??;
        let url = local(install.port)?;
        let client = super::super::connection::client(Duration::from_millis(800))?;
        if install.existed && healthy(&client, &url, &install.version).await {
            return Ok(url);
        }
        TcpListener::bind((Ipv4Addr::LOCALHOST, install.port)).map_err(|_| {
            format!(
                "Port {} is in use. Free it or connect to a server manually.",
                install.port
            )
        })?;
        let output = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(O_NOFOLLOW)
            .open(install.config.with_file_name("desktop-server.log"))?;
        {
            let mut child = crate::lock(&owner.child)?;
            if child
                .as_mut()
                .is_some_and(|child| child.try_wait().is_ok_and(|status| status.is_none()))
            {
                return Err("Minarbot is still starting. Try again shortly.".into());
            }
            let mut command = Command::new(install.launcher);
            command
                .arg("--config")
                .arg(install.config)
                .stdin(Stdio::null())
                .stdout(output.try_clone()?)
                .stderr(output);
            // AppImage's library search path belongs to its GUI runtime.
            command.env_remove("LD_LIBRARY_PATH");
            *child = Some(command.spawn()?);
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        while tokio::time::Instant::now() < deadline {
            if healthy(&client, &url, &install.version).await {
                return Ok(url);
            }
            let running = crate::lock(&owner.child)?
                .as_mut()
                .is_some_and(|child| child.try_wait().is_ok_and(|status| status.is_none()));
            if !running {
                return Err("Minarbot launcher exited. Check desktop-server.log in the launcher configuration folder.".into());
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        stop(owner);
        Err("Minarbot server did not become ready within two minutes.".into())
    }

    pub fn stop(owner: &Local) {
        let Ok(mut guard) = owner.child.lock() else {
            return;
        };
        let Some(mut child) = guard.take() else {
            return;
        };
        if !child.try_wait().is_ok_and(|status| status.is_none()) {
            return;
        }
        let Ok(pid) = i32::try_from(child.id()) else {
            return;
        };
        let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
        for _ in 0..30 {
            if child.try_wait().is_ok_and(|status| status.is_some()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(test)]
    #[test]
    fn rejects_tampered_bundle_without_selecting() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let archive = temp.path().join("runtime");
        fs::write(&archive, b"invalid package")?;
        assert!(extract(temp.path(), &archive, &"0".repeat(64)).is_err());
        assert!(files::selection(temp.path(), "current")
            .map_err(file_error)?
            .is_none());
        Ok(())
    }
}
