use super::{guide, Result};
use crate::{
    computer::{machines, vault},
    data::{error, one, string},
    web::Failure,
    AppState,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncWriteExt, net::UnixListener, process::Command};

pub const WRITES: &[&str] = &[
    "add_guide_machine",
    "install_guide_vps",
    "configure_guide_remote_access",
    "setup_guide_computer",
];
pub const READS: &[&str] = &[
    "list_guide_machines",
    "inspect_guide_remote_access",
    "read_guide_computer_setup",
];
const ORIGINS: &str = "MINARBOT_ALLOWED_ORIGINS";
const ENV_FILE: &str = "MINARBOT_LAUNCHER_ENV_FILE";
const ASKPASS_SOCKET: &str = "MINARBOT_GUIDE_ASKPASS_SOCKET";
pub const ASKPASS_COMMAND: &str = "--guide-ssh-askpass";

fn https_origin(value: &str) -> Result<String> {
    machines::machine_origin(value)
        .ok()
        .filter(|origin| origin.starts_with("https://"))
        .ok_or_else(|| error(400, "Enter an exact HTTPS origin."))
}

fn hostname(value: &str) -> bool {
    value.len() <= 253
        && value.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && label.as_bytes()[0].is_ascii_alphanumeric()
                && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn host_key(value: &str) -> bool {
    let Some(encoded) = value.strip_prefix("ssh-ed25519 ") else {
        return false;
    };
    STANDARD
        .decode(encoded)
        .is_ok_and(|key| key.len() == 51 && key.starts_with(b"\0\0\0\x0bssh-ed25519\0\0\0\x20"))
}

fn vault_revision(db: &Connection, entry: &str) -> Result<i64> {
    let row = one(db, "SELECT e.revision FROM vault_entries e JOIN vault_grants g ON g.entry=e.name WHERE e.name=? AND e.kind='secret' AND g.agent=?", [entry, guide::ID])?;
    row.and_then(|row| row["revision"].as_i64()).ok_or_else(|| {
        error(
            403,
            "Add an SSH password as a Vault secret and grant Minarbot Guide access.",
        )
    })
}

pub fn plan(state: &AppState, db: &Connection, name: &str, args: &Value) -> Result<Value> {
    let (target, settings) = match name {
        "add_guide_machine" => {
            let origin = machines::machine_origin(string(args, "origin"))?;
            let source = machines::machine_origin(machines::public_origin(state))?;
            let name = string(args, "name").trim();
            if name.is_empty() || name.chars().any(char::is_control) {
                return Err(error(400, "Enter a machine name."));
            }
            (
                json!({"origin":origin,"name":name}),
                json!({"source":source}),
            )
        }
        "install_guide_vps" => {
            let host = string(args, "host");
            let user = string(args, "user");
            let entry = string(args, "vaultEntry");
            let key = string(args, "hostKey");
            if !hostname(host)
                || !user
                    .as_bytes()
                    .first()
                    .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
                || !user
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                || !host_key(key)
                || !vault::valid_name(entry)
            {
                return Err(error(400, "Provide a valid SSH host, user, Vault entry name, and trusted Ed25519 host public key."));
            }
            let origin = https_origin(string(args, "origin"))?;
            let source = https_origin(machines::public_origin(state))?;
            let port = args["port"].as_u64().unwrap_or(22);
            let service_port = args["servicePort"].as_u64().unwrap_or(4317);
            let revision = vault_revision(db, entry)?;
            (
                json!({"host":host,"user":user,"port":port,"origin":origin}),
                json!({"vaultEntry":entry,"vaultRevision":revision,"hostKey":key,"source":source,"servicePort":service_port}),
            )
        }
        "configure_guide_remote_access" => (
            json!({"origin":https_origin(string(args, "origin"))?}),
            json!({"restartRequired":true}),
        ),
        "setup_guide_computer" => (Value::Null, json!({})),
        _ => return Err(Failure::storage()),
    };
    Ok(json!({"guideOperation":name,"target":target,"settings":settings}))
}

pub async fn read(state: &AppState, name: &str, args: &Value) -> Result<Value> {
    match name {
        "list_guide_machines" => {
            let origin = args["origin"]
                .as_str()
                .map(machines::machine_origin)
                .transpose()?;
            let machines = machines::list(state)?.into_iter()
                .filter(|machine| origin.as_ref().is_none_or(|origin| machine["origin"] == *origin))
                .take(30)
                .map(|machine| json!({"name":machine["name"],"origin":machine["origin"],"status":machine["status"],"pairingState":machine["pairingState"]}))
                .collect::<Vec<_>>();
            Ok(json!({"machines":machines}))
        }
        "read_guide_computer_setup" => crate::computer::guide_setup_status(state),
        "inspect_guide_remote_access" => {
            let mut result = tailscale(state).await?;
            if let Some(origin) = result["httpsOrigin"].as_str() {
                let allowed = state
                    .config
                    .allowed_origins
                    .iter()
                    .any(|value| value == origin);
                let reachable = allowed && probe_pair_page(origin).await;
                result["allowed"] = json!(allowed);
                result["reachableFromServer"] = json!(reachable);
            }
            Ok(result)
        }
        _ => Err(Failure::storage()),
    }
}

async fn tailscale_json(args: &[&str]) -> Result<Option<Value>> {
    use tokio::io::AsyncReadExt;
    let mut command = Command::new("tailscale");
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(error(409, "Could not start Tailscale.")),
    };
    let stdout = child.stdout.take().ok_or_else(Failure::storage)?;
    let output = tokio::time::timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        stdout.take(512 * 1024 + 1).read_to_end(&mut bytes).await?;
        if bytes.len() > 512 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Tailscale output exceeded limit.",
            ));
        }
        if !child.wait().await?.success() && bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(std::io::Error::other)
    })
    .await
    .map_err(|_| error(409, "Tailscale did not respond."))?
    .map_err(|_| error(409, "Could not read Tailscale status."))?;
    Ok(Some(output))
}

async fn tailscale(state: &AppState) -> Result<Value> {
    let Some(status) = tailscale_json(&["status", "--json"]).await? else {
        return Ok(json!({"installed":false}));
    };
    let name = string(&status["Self"], "DNSName").trim_end_matches('.');
    let running = status["BackendState"] == "Running";
    let magic_dns = status["CurrentTailnet"]["MagicDNSEnabled"] == true;
    let https_name = status["CertDomains"]
        .as_array()
        .is_some_and(|domains| domains.iter().any(|domain| domain == name));
    let mut result =
        json!({"installed":true,"loggedIn":running,"magicDns":magic_dns,"httpsEnabled":https_name});
    if !running || !magic_dns || !https_name || !hostname(name) || !name.ends_with(".ts.net") {
        return Ok(result);
    }
    result["httpsName"] = json!(name);
    let serve = tailscale_json(&["serve", "status", "--json"])
        .await?
        .unwrap_or(Value::Null);
    let local_port = url::Url::parse(&state.origin)
        .ok()
        .and_then(|url| url.port_or_known_default());
    let web = serve["Web"].as_object().into_iter().flatten();
    for (address, site) in web {
        let Some(port) = address
            .strip_prefix(&format!("{name}:"))
            .and_then(|port| port.parse::<u16>().ok())
        else {
            continue;
        };
        let proxy = string(&site["Handlers"]["/"], "Proxy");
        let local = ["127.0.0.1", "localhost", "[::1]"]
            .iter()
            .any(|host| local_port.is_some_and(|port| proxy == format!("http://{host}:{port}")));
        let funnel = serve["AllowFunnel"][address] == true;
        let https = serve["TCP"][port.to_string()]["HTTPS"] == true;
        if local && https && !funnel {
            result["httpsOrigin"] = json!(if port == 443 {
                format!("https://{name}")
            } else {
                format!("https://{address}")
            });
            break;
        }
    }
    Ok(result)
}

async fn probe_pair_page(origin: &str) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
    else {
        return false;
    };
    client
        .get(format!("{origin}/pair"))
        .send()
        .await
        .is_ok_and(|response| {
            response.status().is_success()
                && response.headers().contains_key("content-security-policy")
        })
}

pub async fn apply(state: &AppState, id: &str, plan: &Value) -> Result<Value> {
    let target = &plan["target"];
    let settings = &plan["settings"];
    match string(plan, "guideOperation") {
        "add_guide_machine" => {
            check_source(state, target, settings)?;
            // The existing owner-only install endpoint delivers the command credential.
            Ok(json!({"installRequest":id}))
        }
        "setup_guide_computer" => Ok(json!({"setup":crate::computer::guide_setup(state)?})),
        "configure_guide_remote_access" => {
            let origin = string(target, "origin");
            if tailscale(state).await?["httpsOrigin"] != origin {
                return Err(error(409, "Configure tailnet-only HTTPS Serve for this Minarbot port, then check remote access again."));
            }
            if state
                .config
                .allowed_origins
                .iter()
                .any(|value| value == origin)
            {
                if !probe_pair_page(origin).await {
                    return Err(error(
                        409,
                        "The HTTPS pairing page is unreachable. Check Tailscale Serve.",
                    ));
                }
                return Ok(
                    json!({"message":"HTTPS pairing page is reachable. Pair your phone in Devices."}),
                );
            }
            let path = std::env::var_os(ENV_FILE)
                .filter(|path| Path::new(path).is_absolute())
                .ok_or_else(|| error(409, "This server has no launcher.env. Add the exact origin to MINARBOT_ALLOWED_ORIGINS in its service configuration."))?;
            write_allowed_origin(Path::new(&path), &state.config.allowed_origins, origin)?;
            Ok(
                json!({"message":"Origin saved. Restart Minarbot, then open Devices to pair your phone."}),
            )
        }
        "install_guide_vps" => install_vps(state, id, target, settings).await,
        _ => Err(Failure::storage()),
    }
}

fn write_allowed_origin(path: &Path, active: &[String], desired: &str) -> Result<()> {
    let error = || error(409, "Could not update the private launcher.env file.");
    let lock_path = path.with_extension("env.guide-lock");
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(&lock_path)
        .map_err(|_| error())?;
    if !lock.metadata().map_err(|_| error())?.is_file() {
        return Err(error());
    }
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .map_err(|_| error())?;
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)
        .map_err(|_| error())?;
    let metadata = file.metadata().map_err(|_| error())?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
    {
        return Err(error());
    }
    let mut source = String::new();
    use std::io::Read;
    (&mut file)
        .take(1024 * 1024 + 1)
        .read_to_string(&mut source)
        .map_err(|_| error())?;
    if source.len() > 1024 * 1024 {
        return Err(error());
    }
    let configured = minarbot_launcher::config::parse_env(&source);
    let mut origins = active.to_vec();
    for value in configured
        .get(ORIGINS)
        .into_iter()
        .flat_map(|raw| raw.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .chain([desired])
    {
        let value = https_origin(value)?;
        if !origins.contains(&value) {
            origins.push(value);
        }
    }
    let origins = origins.join(",");
    if configured.get(ORIGINS) == Some(&origins) {
        return Ok(());
    }
    // Appending preserves multiline dotenv values and unrelated comments.
    source.push_str(&format!("\n{ORIGINS}={origins}\n"));
    if minarbot_launcher::config::parse_env(&source).get(ORIGINS) != Some(&origins) {
        return Err(error());
    }
    minarbot_launcher::files::durable(path, source.as_bytes()).map_err(|_| error())
}

fn check_source(state: &AppState, target: &Value, settings: &Value) -> Result<()> {
    let source = string(settings, "source");
    let origin = string(target, "origin");
    if source != machines::public_origin(state) {
        return Err(error(
            409,
            "This Minarbot address changed. Request a fresh proposal.",
        ));
    }
    if source == origin
        || machines::list(state)?
            .iter()
            .any(|machine| machine["origin"] == origin)
    {
        return Err(error(
            409,
            "This address belongs to this server or an existing pairing.",
        ));
    }
    Ok(())
}

async fn install_vps(
    state: &AppState,
    id: &str,
    target: &Value,
    settings: &Value,
) -> Result<Value> {
    check_source(state, target, settings)?;
    let host = string(target, "host");
    let port = target["port"].as_u64().ok_or_else(Failure::storage)?;
    let user = string(target, "user");
    let entry = string(settings, "vaultEntry");
    let origin = string(target, "origin");
    let (vault_origin, password) = {
        let lock = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &lock.connection;
        let revision = vault_revision(db, entry)?;
        if Some(revision) != settings["vaultRevision"].as_i64() {
            return Err(error(409, "Vault entry changed. Request a fresh proposal."));
        }
        vault::resolve(state, db, guide::ID, entry, "value", state.config.now())?
    };
    let expected = format!("https://{host}");
    if !vault_origin.eq_ignore_ascii_case(&expected) {
        return Err(error(403, "Vault secret URL must match the SSH host."));
    }
    let disk = SshFiles::new(host, port, string(settings, "hostKey"))?;
    let install = crate::updates::install::issue(state, id, Some(origin)).await?;
    let token = string(&install, "token");
    let service_port = settings["servicePort"]
        .as_u64()
        .ok_or_else(Failure::storage)?;
    let source = string(settings, "source");
    let remote = format!(
        "sh -c {} guide {} {} {} {}",
        quote(include_str!("guide_vps.sh")),
        quote(id),
        quote(source),
        quote(origin),
        service_port,
    );
    let status = disk
        .ssh(host, user, port, &remote, &password, token.as_bytes())
        .await;
    let retired = crate::updates::install::retire(state, id).is_ok();
    if status == Some(20) {
        return Err(error(409, "VPS already has a Minarbot installation or another setup is in progress. Check it before retrying."));
    }
    if status == Some(0) {
        let online = machines::list(state).is_ok_and(|machines| {
            machines.iter().any(|machine| {
                machine["origin"] == origin
                    && machine["pairingState"] == "paired"
                    && machine["status"] == "online"
            })
        });
        if online && retired {
            let finalize = format!(
                "sh -c {} guide {} commit",
                quote(include_str!("guide_vps_cleanup.sh")),
                quote(id)
            );
            let finalized = disk.ssh(host, user, port, &finalize, &password, &[]).await == Some(0);
            return Ok(json!({"message": if finalized { "VPS paired and online." }
                else { "VPS paired and online. Check the VPS for its leftover .minarbot-guide-install directory." }}));
        }
    }

    let rollback = format!(
        "sh -c {} guide {} rollback",
        quote(include_str!("guide_vps_cleanup.sh")),
        quote(id)
    );
    let rolled_back = disk.ssh(host, user, port, &rollback, &password, &[]).await == Some(0);
    Err(error(
        if rolled_back && retired { 409 } else { 503 },
        if rolled_back {
            "VPS did not pair and become healthy. Any new VPS service was stopped. Check Machines for a leftover pairing and inspect the VPS before retrying."
        } else {
            "VPS setup outcome is uncertain. On the VPS, check the Minarbot user service and stop it if this attempt started it. Check Machines for an incomplete pairing."
        },
    ))
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

struct SshFiles {
    dir: PathBuf,
    helper: PathBuf,
    known_hosts: PathBuf,
}

impl SshFiles {
    fn new(host: &str, port: u64, key: &str) -> Result<Self> {
        let dir =
            std::env::temp_dir().join(format!("minarbot-ssh-{}", uuid::Uuid::new_v4().simple()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|_| Failure::storage())?;
        let files = Self {
            helper: dir.join("askpass"),
            known_hosts: dir.join("known_hosts"),
            dir,
        };
        let host = if port == 22 {
            host.to_owned()
        } else {
            format!("[{host}]:{port}")
        };
        fs::write(&files.known_hosts, format!("{host} {key}\n")).map_err(|_| Failure::storage())?;
        let mut helper = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o700)
            .open(&files.helper)
            .map_err(|_| Failure::storage())?;
        use std::io::Write;
        helper
            .write_all(b"#!/bin/sh\nexec \"$MINARBOT_GUIDE_EXECUTABLE\" --guide-ssh-askpass\n")
            .map_err(|_| Failure::storage())?;
        Ok(files)
    }

    async fn ssh(
        &self,
        host: &str,
        user: &str,
        port: u64,
        remote: &str,
        password: &str,
        input: &[u8],
    ) -> Option<i32> {
        let socket = self.dir.join("password");
        let _ = fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).ok()?;
        let executable = std::env::current_exe().ok()?;
        let mut child = Command::new("ssh")
            .arg("-F")
            .arg("/dev/null")
            .args([
                "-T",
                "-o",
                "BatchMode=no",
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                "GlobalKnownHostsFile=/dev/null",
                "-o",
                "UpdateHostKeys=no",
                "-o",
                "HostKeyAlgorithms=ssh-ed25519",
                "-o",
                "PreferredAuthentications=password",
                "-o",
                "PubkeyAuthentication=no",
                "-o",
                "IdentityFile=none",
                "-o",
                "IdentityAgent=none",
                "-o",
                "NumberOfPasswordPrompts=1",
                "-o",
                "ClearAllForwardings=yes",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ServerAliveInterval=5",
                "-o",
                "ServerAliveCountMax=2",
            ])
            .arg("-o")
            .arg(format!("UserKnownHostsFile={}", self.known_hosts.display()))
            .arg("-p")
            .arg(port.to_string())
            .arg("-l")
            .arg(user)
            .arg("--")
            .arg(host)
            .arg(remote)
            .env_clear()
            .env(
                "PATH",
                std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
            )
            .env("SSH_ASKPASS", &self.helper)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("MINARBOT_GUIDE_EXECUTABLE", executable)
            .env(ASKPASS_SOCKET, &socket)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .ok()?;
        let mut stdin = child.stdin.take()?;
        tokio::time::timeout(Duration::from_secs(540), async {
            let send_input = async {
                let _ = stdin.write_all(input).await;
                drop(stdin);
            };
            let provide_password = async {
                if let Ok((mut stream, _)) = listener.accept().await {
                    let _ = stream.write_all(password.as_bytes()).await;
                }
            };
            tokio::select! {
                status = child.wait() => status.ok()?.code(),
                () = async { tokio::join!(send_input, provide_password); } => child.wait().await.ok()?.code(),
            }
        }).await.ok().flatten()
    }
}

impl Drop for SshFiles {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

pub fn askpass() -> crate::db::Result<()> {
    use std::io::{self, Write};
    use std::os::unix::net::UnixStream;
    let path = std::env::var_os(ASKPASS_SOCKET).ok_or("Missing SSH password socket.")?;
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut output = io::stdout().lock();
    io::copy(&mut stream, &mut output)?;
    output.write_all(b"\n")?;
    Ok(())
}
