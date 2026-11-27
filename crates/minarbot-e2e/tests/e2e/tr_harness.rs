//! Harness discovery: the Hermes probe sandbox, runtime versions and
//! advertised models, and login-shell PATH hydration for a service launch
//! (ports of `app/rust-hermes-probe.test.ts`, `app/rust-rehearsal.test.ts`
//! and the server cases of `app/path-hydration.test.ts`, #89 triage).

use minarbot_testkit::{prelude::*, relay::HttpClient, server::server_binary};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

fn script(path: &Path, source: &str) -> Result<()> {
    fs::write(path, source)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn choice(runtimes: &Value, runtime: &str) -> Value {
    runtimes["choices"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["runtime"] == runtime))
        .cloned()
        .unwrap_or(Value::Null)
}

/// A Hermes stand-in that checks the probe sandbox: it reads the committed
/// install, tries to take the install lock and a generation lease, and
/// answers `initialize` only after 2.5 s.
const HERMES: &str = r#"#!/bin/sh
install="$HOME/.hermes/installs/fixture"
[ "$(cat "$install/facts.json")" = committed ] || exit 1
printf 'probe lock' > "$install/.install.lock" || exit 1
: > "$install/environments/fixture/.leases/probe" || exit 1
[ "${HERMES_YOLO_MODE:-}" = 0 ] && [ "${HERMES_ACP_SKIP_CONFIGURED_MCP:-}" = 1 ] && [ "${HERMES_DISABLE_LAZY_INSTALLS:-}" = 1 ] || exit 1
if [ "$#" = 1 ] && [ "$1" = --version ]; then printf 'hermes 0.22.2\n'; exit 0; fi
[ "$#" = 1 ] && [ "$1" = acp ] || exit 1
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      sleep 2.5
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"authMethods":[{"type":"agent"}],"models":[]}}\n' "$id";;
  esac
done
"#;

#[tokio::test]
async fn hermes_probes_read_the_host_install_without_changing_it() -> Result<()> {
    let root = TestRoot::new()?;
    let dir = root.dir("hermes")?;
    let home = dir.join("home");
    let install = home.join(".hermes/installs/fixture");
    let generation = install.join("environments/fixture");
    let leases = generation.join(".leases");
    let lock = install.join(".install.lock");
    fs::create_dir_all(&leases)?;
    fs::write(install.join("facts.json"), "committed")?;
    fs::write(generation.join(".lease-managed"), "")?;
    fs::write(&lock, "host lock")?;
    script(&dir.join("hermes"), HERMES)?;
    let app = root
        .start(
            ServerConfig::new()
                .path_prefix(&dir)
                .env("HOME", home.display().to_string()),
        )
        .await?;
    let runtimes = app.get("/api/runtimes").await?;
    let hermes = choice(&runtimes, "hermes");
    ensure!(
        hermes
            == json!({"runtime": "hermes", "available": true, "version": "hermes 0.22.2",
                "signedIn": true, "models": [], "reason": null, "label": "Hermes", "enabled": true}),
        "hermes choice: {hermes}"
    );
    ensure!(
        fs::read_to_string(&lock)? == "host lock",
        "the probe changed the host install lock"
    );
    let leased: Vec<_> = fs::read_dir(&leases)?.collect::<std::io::Result<_>>()?;
    ensure!(
        leased.is_empty(),
        "the probe left a lease in the host install: {leased:?}"
    );
    Ok(())
}

/// An ACP-style harness whose version spans lines and tabs and whose models
/// come from both `models` and nested, partly duplicate `configOptions`.
const REHEARSAL: &str = r#"#!/bin/sh
for arg; do [ "$arg" = --version ] && { printf 'fixture\nversion\t1\n'; exit 0; }; done
result='{"protocolVersion":1,"models":{"availableModels":[{"modelId":"first","name":"First"}]},"configOptions":[{"category":"model","options":[{"value":"second","name":"Second"},{"options":[{"value":"first","name":"Duplicate"},{"value":"third","name":"Third"}]}]}]}'
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/^{.*"id":\([0-9][0-9]*\|"[^"]*"\).*/\1/p')
  if [ -n "$id" ]; then
    printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$result"
  else
    printf '{"jsonrpc":"2.0","result":%s}\n' "$result"
  fi
done
"#;

/// The tightest finite cgroup v2 memory limit of this process, which the
/// server inherits, capped at the host total; `None` when unlimited.
fn cgroup_memory_limit(total: Option<u64>) -> Option<u64> {
    let group = fs::read_to_string("/proc/self/cgroup").ok()?;
    let group = group.lines().find_map(|line| line.strip_prefix("0::"))?;
    let path = Path::new("/sys/fs/cgroup").join(group.trim_start_matches('/'));
    let limit = ["memory.max", "memory.high"]
        .iter()
        .filter_map(|name| fs::read_to_string(path.join(name)).ok())
        .filter_map(|value| value.trim().parse::<u64>().ok())
        .min()?;
    Some(limit.min(total?))
}

#[tokio::test]
async fn folder_browser_and_runtime_discovery_retain_json_strings_and_advertised_models(
) -> Result<()> {
    let root = TestRoot::new()?;
    let dir = root.dir("rehearsal")?;
    let home = dir.join("home");
    fs::create_dir_all(home.join("Folder"))?;
    let executable = dir.join("fake-harness");
    script(&executable, REHEARSAL)?;
    let mut config = ServerConfig::new().env("HOME", home.display().to_string());
    for runtime in [
        "codex",
        "claude",
        "hermes",
        "antigravity",
        "grok",
        "pi",
        "opencode",
        "gemini",
        "kiro",
    ] {
        config = if runtime == "codex" || runtime == "grok" {
            config.executable(runtime, &executable)
        } else {
            config.executable(runtime, "/missing-harness")
        };
    }
    let app = root.start(config).await?;
    let folders = app.get("/api/project-folders").await?;
    ensure!(
        folders
            == json!({"path": home, "parent": null,
                "folders": [{"name": "Folder", "path": home.join("Folder")}]}),
        "folders: {folders}"
    );
    let runtimes = app.get("/api/runtimes").await?;
    ensure!(
        choice(&runtimes, "codex")["version"] == "fixture version 1",
        "codex version: {}",
        choice(&runtimes, "codex")
    );
    ensure!(
        choice(&runtimes, "grok")["models"]
            == json!([{"id": "first", "label": "First"}, {"id": "second", "label": "Second"},
                {"id": "third", "label": "Third"}]),
        "grok models: {}",
        choice(&runtimes, "grok")
    );
    let peer_tokens = app.get("/api/mcp-server/peer-tokens").await?;
    ensure!(
        peer_tokens["endpoint"] == format!("{}/mcp", app.origin()).as_str(),
        "peer token endpoint: {peer_tokens}"
    );
    let agent = app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Fixture", "model": "fixture",
                "instructions": "Help."}),
        )
        .await?;
    let channel = app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Fixture room", "members": [agent["id"]],
                "lead": agent["id"], "instructions": "Help."}),
        )
        .await?;
    let panel = app
        .get(&format!(
            "/api/channels/{}/panel",
            channel["id"].as_str().unwrap_or_default()
        ))
        .await?;
    ensure!(panel["computer"].is_null(), "room panel computer: {panel}");
    let metrics = app.get("/api/host-metrics").await?;
    let expected = cgroup_memory_limit(metrics["memory"]["total"].as_u64());
    ensure!(
        metrics["process"]["memoryLimit"] == json!(expected),
        "memory limit {} instead of {expected:?}",
        metrics["process"]["memoryLimit"]
    );
    let (process, host) = (
        metrics["process"]["availableMemory"].as_u64(),
        metrics["memory"]["available"].as_u64(),
    );
    ensure!(
        process
            .zip(host)
            .is_some_and(|(process, host)| process <= host)
            || (process.is_none() && expected.is_none()),
        "process available memory {process:?} exceeds host available {host:?}"
    );
    Ok(())
}

/// Removes a login-path HOME after clearing permissions: the server's
/// container probe may let rootless Podman create storage there with
/// folders that have no permissions.
struct LoginHome(PathBuf);

impl Drop for LoginHome {
    fn drop(&mut self) {
        let _ = std::process::Command::new("chmod")
            .args(["-R", "u+rwX"])
            .arg(&self.0)
            .status();
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Environment keys a dumped `env` listing holds, without the `PWD` that
/// the fixture's `/bin/sh` (dash) exports on its own.
fn env_keys(listing: &str) -> Vec<String> {
    let mut keys: Vec<String> = listing
        .lines()
        .filter_map(|line| line.split_once('=').map(|(key, _)| key.to_owned()))
        .filter(|key| key != "PWD")
        .collect();
    keys.sort();
    keys
}

fn env_value(listing: &str, key: &str) -> Option<String> {
    listing
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")).map(str::to_owned))
}

async fn login_shell_service(broken: bool) -> Result<()> {
    let root = TestRoot::new()?;
    let directory = LoginHome(root.dir("login-path")?);
    let directory = &directory.0;
    let bin = directory.join("bin");
    let inherited = directory.join("service-bin");
    let shell = directory.join("login-shell");
    let probes = directory.join("probes");
    let environment = directory.join("harness-env");
    let status_environment = directory.join("status-env");
    fs::create_dir_all(&bin)?;
    fs::create_dir_all(&inherited)?;
    script(
        &inherited.join("printenv"),
        "#!/bin/sh\n[ \"$1\" = PATH ] && printf \"%s\\n\" \"$PATH\"\n",
    )?;
    // The fallback Bash login shell also has a fixture-only PATH.
    fs::write(
        directory.join(".bash_profile"),
        format!("export PATH='{}'\n", inherited.display()),
    )?;
    script(
        &bin.join("codex"),
        &format!(
            "#!/bin/sh\nif [ \"$1\" = login ] && [ \"$2\" = status ]; then /usr/bin/env > '{}'; exit 1; fi\n/usr/bin/env > '{}'\nprintf 'codex fixture 1.0\\n'\n",
            status_environment.display(),
            environment.display()
        ),
    )?;
    script(
        &shell,
        &if broken {
            "#!/bin/sh\necho synthetic-secret >&2\nexit 7\n".to_owned()
        } else {
            format!(
                "#!/bin/sh\nprintf x >> '{}'\nprintf 'profile noise\\n'\nexport PATH='{}:{}'\nexport SHELL_SECRET=synthetic-secret\nexec /bin/sh -c \"$2\"\n",
                probes.display(),
                bin.display(),
                inherited.display()
            )
        },
    )?;
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let origin = format!("http://127.0.0.1:{port}");
    let mut child = tokio::process::Command::new(server_binary(None)?)
        .env_clear()
        .env("HOME", directory)
        .env("PATH", &inherited)
        .env("SHELL", &shell)
        .env("TMPDIR", directory)
        .env("PORT", port.to_string())
        .env("MINARBOT_DATA_DIR", directory)
        .env("AMBIENT_SECRET", "synthetic-secret")
        .env("LANG", "C")
        .env("OPENAI_API_KEY", "canary")
        .env("COMPOSIO_API_KEY", "canary")
        .env("NODE_OPTIONS", "--trace-warnings")
        .env("__MISE_DIFF", "canary")
        .env("JAVA_HOME", "canary")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = child.stdout.take().ok_or("server stdout")?;
    let mut stderr = child.stderr.take().ok_or("server stderr")?;
    let errors = tokio::spawn(async move {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text).await;
        text
    });
    let ready = format!("Minarbot: {origin}");
    let mut lines = BufReader::new(stdout).lines();
    let started = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(line) = lines.next_line().await? {
            if line == ready {
                return Ok::<bool, std::io::Error>(true);
            }
        }
        Ok(false)
    })
    .await;
    // Keep draining stdout so the server never blocks on a full pipe.
    tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
    let checked: Result<()> = async {
        ensure!(
            matches!(started, Ok(Ok(true))),
            "server did not report readiness: {started:?}"
        );
        let client = HttpClient::builder()
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .build()?;
        let environment_status = client
            .get(format!("{origin}/api/environment"))
            .send()
            .await?
            .status();
        ensure!(
            environment_status == 200,
            "environment: {environment_status}"
        );
        let catalog = client.get(format!("{origin}/api/runtimes")).send().await?;
        ensure!(catalog.status() == 200, "runtimes: {}", catalog.status());
        let runtimes: Value = catalog.json().await?;
        let discovered: Vec<&str> = runtimes["choices"]
            .as_array()
            .ok_or("choices")?
            .iter()
            .filter(|row| {
                !row["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("is missing from PATH."))
            })
            .filter_map(|row| row["runtime"].as_str())
            .collect();
        let codex = choice(&runtimes, "codex");
        if broken {
            ensure!(discovered.is_empty(), "discovered: {discovered:?}");
            ensure!(codex["version"].is_null(), "codex: {codex}");
            ensure!(
                runtimes["defaultRuntime"] == "api",
                "default: {}",
                runtimes["defaultRuntime"]
            );
            return Ok(());
        }
        ensure!(discovered == ["codex"], "discovered: {discovered:?}");
        ensure!(codex["version"] == "codex fixture 1.0", "codex: {codex}");
        ensure!(
            runtimes["defaultRuntime"] == "codex",
            "default: {}",
            runtimes["defaultRuntime"]
        );
        client
            .get(format!("{origin}/api/runtimes"))
            .send()
            .await?
            .bytes()
            .await?;
        ensure!(
            fs::read_to_string(&probes)? == "x",
            "the login shell ran more than once"
        );
        let probe = fs::read_to_string(&environment)?;
        ensure!(
            env_value(&probe, "PATH") == Some(format!("{}:{}", bin.display(), inherited.display())),
            "probe PATH: {probe}"
        );
        ensure!(
            env_keys(&probe) == ["HOME", "LANG", "PATH", "TMPDIR"],
            "probe environment: {probe}"
        );
        ensure!(
            env_value(&probe, "HOME")
                .is_some_and(|home| home
                    .starts_with(&directory.join("minarbot-probe-").display().to_string())),
            "probe HOME: {probe}"
        );
        ensure!(
            env_value(&probe, "LANG").as_deref() == Some("C"),
            "probe LANG: {probe}"
        );
        ensure!(
            env_value(&probe, "TMPDIR") == Some(directory.display().to_string()),
            "probe TMPDIR: {probe}"
        );
        // The device sign-in check sees the service's own HOME and nothing
        // else from its environment.
        let status = fs::read_to_string(&status_environment)?;
        ensure!(
            env_keys(&status) == ["CODEX_HOME", "HOME", "PATH"],
            "status environment: {status}"
        );
        ensure!(
            env_value(&status, "HOME") == Some(directory.display().to_string())
                && env_value(&status, "CODEX_HOME")
                    == Some(directory.join(".codex").display().to_string()),
            "status homes: {status}"
        );
        ensure!(codex["signedIn"] == false, "codex sign-in: {codex}");
        Ok(())
    }
    .await;
    child.start_kill()?;
    let _ = child.wait().await;
    let stderr = errors.await?;
    checked?;
    ensure!(
        !stderr.contains("synthetic-secret"),
        "a login-shell secret reached the server log: {stderr}"
    );
    Ok(())
}

#[tokio::test]
async fn service_launch_discovers_codex_exported_only_by_the_login_shell() -> Result<()> {
    login_shell_service(false).await
}

#[tokio::test]
async fn broken_login_shell_keeps_service_startup_working() -> Result<()> {
    login_shell_service(true).await
}
