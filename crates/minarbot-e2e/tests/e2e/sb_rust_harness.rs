//! Mise shims, ACP and Pi runtimes, and MCP peer features (port of
//! `app/rust-harness.test.ts`).

use crate::sb_support::id;
use minarbot_testkit::{
    fakes::{self, Fake},
    prelude::*,
    server::server_binary,
    Server,
};
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::Path,
    time::{Duration, Instant},
};

fn script(path: &Path, source: &str) -> Result<()> {
    fs::write(path, source)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Approve every pending harness card of turn `id` until it settles.
async fn approve_until_settled(app: &Server, root: &str, id: &str) -> Result<Value> {
    until_value(
        format!("turn {id}"),
        || async {
            let row = app.row(root, id).await?.ok_or("turn")?;
            for card in row["harnessApprovals"]
                .as_array()
                .cloned()
                .unwrap_or_default()
            {
                if card["state"] == "approval" {
                    app.post(
                        &format!(
                            "{root}/{id}/harness-approvals/{}",
                            card["id"].as_str().unwrap_or_default()
                        ),
                        json!({"id": new_id(), "revision": card["revision"], "decision": "allow"}),
                    )
                    .await?;
                }
            }
            Ok(row)
        },
        |row| ["done", "failed", "cancelled"].contains(&row["status"].as_str().unwrap_or_default()),
    )
    .await
}

#[tokio::test]
async fn mise_shim_probes_and_launches_with_trusted_mise_paths_and_an_isolated_home() -> Result<()>
{
    let root = TestRoot::new()?;
    let dir = root.dir("mise")?;
    let home = dir.join("owner");
    let workspace = dir.join("workspace");
    let config = home.join(".config/mise");
    let state = home.join(".local/state/mise");
    let cache = home.join(".cache/mise");
    let data = dir.join("xdg-data/mise");
    for path in [
        &workspace,
        &config,
        &state.join("trusted-configs"),
        &cache,
        &data,
    ] {
        fs::create_dir_all(path)?;
    }
    fs::write(
        config.join("config.toml"),
        "[env]\nGEMINI_API_KEY = \"synthetic-config-secret\"\n",
    )?;
    fs::write(home.join(".profile-canary"), "synthetic-host-profile")?;
    symlink(&home, state.join("trusted-configs/fixture"))?;
    let fake = fakes::install(Fake::Acp, &dir, "fake-acp")?;
    let shim = dir.join("gemini");
    let direct = dir.join("codex");
    // Like Gemini CLI, the shim waits for stdin EOF before answering --version.
    script(
        &shim,
        "#!/bin/bash\n[ \"${1:-}\" = --version ] && cat >/dev/null\nexport MISE_MINIMUM_RELEASE_AGE=0; mise use -g --quiet \"gemini\" || exit 1; exec mise x \"gemini\" -- \"gemini\" \"$@\"\n",
    )?;
    let (config_s, data_s, cache_s, home_s, fake_s) = (
        config.display().to_string(),
        data.display().to_string(),
        cache.display().to_string(),
        home.display().to_string(),
        fake.display().to_string(),
    );
    script(
        &dir.join("mise"),
        &format!(
            r#"#!/bin/sh
set -eu
if [ ! -f "${{MISE_CONFIG_DIR:-}}/config.toml" ] || [ "${{MISE_TRUSTED_CONFIG_PATHS:-}}" != "$MISE_CONFIG_DIR" ]; then
  echo 'mise fixture: config is not trusted' >&2
  exit 1
fi
[ "$MISE_CONFIG_DIR" = '{config_s}' ]
case ":$PATH:" in *':{data_s}/'*) echo 'mise fixture: PATH lists mise data dirs' >&2; exit 1;; esac
[ "$MISE_STATE_DIR" = "$HOME/.local/state/mise" ]
mkdir -p "$MISE_STATE_DIR/trusted-configs" && ln -sfn "$HOME" "$MISE_STATE_DIR/trusted-configs/home-$$"
[ "$MISE_CACHE_DIR" = '{cache_s}' ]
[ "$MISE_DATA_DIR" = '{data_s}' ]
[ "$HOME" != '{home_s}' ]
[ ! -e "$HOME/.profile-canary" ]
[ -z "${{GEMINI_API_KEY:-}}" ]
[ "$MISE_NO_ENV" = 1 ]
[ "$MISE_NO_HOOKS" = 1 ]
case "$1" in
  use) exit 0;;
  x) shift 4; exec '{fake_s}' "$@";;
esac
exit 1
"#
        ),
    )?;
    script(
        &direct,
        "#!/bin/sh\n[ -z \"$MISE_CONFIG_DIR\" ] && [ -z \"$MISE_STATE_DIR\" ] && printf \"direct 1.0.0\\n\"\n",
    )?;
    let shell = dir.join("fixture-shell");
    script(&shell, "#!/bin/sh\nexec /bin/sh -c \"$2\"\n")?;
    let agent = new_id();
    let mut server = ServerConfig::new()
        .path_prefix(format!(
            "{}:{}",
            dir.display(),
            data.join("shims").display()
        ))
        .workspace(&agent, &workspace, true)
        .executable("gemini", &shim)
        .executable("codex", &direct);
    for (name, value) in [
        ("SHELL", shell.display().to_string()),
        ("HOME", home_s.clone()),
        ("GEMINI_API_KEY", "synthetic-host-secret".into()),
        ("MISE_CONFIG_DIR", String::new()),
        ("MISE_STATE_DIR", String::new()),
        ("MISE_CACHE_DIR", String::new()),
        ("MISE_DATA_DIR", data_s.clone()),
        (
            "XDG_CONFIG_HOME",
            home.join(".config").display().to_string(),
        ),
        (
            "XDG_STATE_HOME",
            home.join(".local/state").display().to_string(),
        ),
        ("XDG_CACHE_HOME", home.join(".cache").display().to_string()),
        ("XDG_DATA_HOME", dir.join("xdg-data").display().to_string()),
    ] {
        server = server.env(name, value);
    }
    let app = root.start(server).await?;
    let runtimes = app.get("/api/runtimes").await?;
    let choice = |runtime: &str| {
        runtimes["choices"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["runtime"] == runtime))
            .cloned()
            .unwrap_or(Value::Null)
    };
    let gemini = choice("gemini");
    ensure!(
        choice("codex")["version"] == "direct 1.0.0",
        "codex probe: {}",
        choice("codex")
    );
    let profile =
        json!({"name": "Fixture", "instructions": "Help.", "model": "fixture", "runtime": "api"});
    let path = format!("/api/agents/{agent}");
    let turns = format!("{path}/turns");
    let mut create = profile.clone();
    create["id"] = json!(agent);
    app.post("/api/agents", create).await?;
    let mut edit = profile.clone();
    edit["id"] = json!(new_id());
    edit["revision"] = json!(1);
    edit["runtime"] = json!("gemini");
    edit["confirmRuntime"] = json!(true);
    edit["harnessModel"] = json!("fixture-model");
    app.post(&format!("{path}/edit"), edit).await?;
    let id = new_id();
    app.post(&turns, json!({"id": id, "text": "hello"})).await?;
    let turn = approve_until_settled(&app, &turns, &id).await?;
    ensure!(
        [
            &gemini["version"],
            &gemini["available"],
            &turn["status"],
            &turn["reply"]
        ] == [
            &json!("fake-acp 1.0.0"),
            &json!(true),
            &json!("done"),
            &json!("Final answer.")
        ],
        "{} {} {}",
        gemini,
        turn["status"],
        turn["error"]
    );
    // Probes and turns record mise trust in their isolated homes.
    let trusted: Vec<String> = fs::read_dir(state.join("trusted-configs"))?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<_>>()?;
    ensure!(
        trusted == ["fixture"],
        "owner mise state changed: {trusted:?}"
    );
    Ok(())
}

#[tokio::test]
async fn rust_acp_and_pi_execute_fake_protocols_with_model_selection_and_approvals() -> Result<()> {
    let root = TestRoot::new()?;
    let dir = root.dir("acp")?;
    let executable = fakes::install(Fake::Acp, &dir, "fake-acp")?;
    let agent = new_id();
    let runtimes = [
        "hermes",
        "grok",
        "opencode",
        "gemini",
        "kiro",
        "antigravity",
        "pi",
    ];
    let mut config = ServerConfig::new().workspace(&agent, &dir, true);
    for runtime in runtimes {
        config = config.executable(runtime, &executable);
    }
    let app = root.start(config).await?;
    let profile =
        json!({"name": "Fixture", "instructions": "Help.", "model": "fixture", "runtime": "api"});
    let mut create = profile.clone();
    create["id"] = json!(agent);
    app.post("/api/agents", create).await?;
    let turns = format!("/api/agents/{agent}/turns");
    for (index, runtime) in runtimes.iter().enumerate() {
        let mut edit = profile.clone();
        edit["id"] = json!(new_id());
        edit["revision"] = json!(index + 1);
        edit["runtime"] = json!(runtime);
        edit["confirmRuntime"] = json!(true);
        edit["harnessModel"] = json!(if *runtime == "pi" {
            "fixture/model"
        } else {
            "fixture-model"
        });
        app.post(&format!("/api/agents/{agent}/edit"), edit).await?;
        let id = new_id();
        app.post(&turns, json!({"id": id, "text": "hello"})).await?;
        let turn = approve_until_settled(&app, &turns, &id).await?;
        ensure!(turn["status"] == "done", "{runtime}: {turn}");
        let expected = if *runtime == "pi" {
            "Pi answer."
        } else {
            "Final answer."
        };
        ensure!(
            turn["reply"] == expected,
            "{runtime} reply: {}",
            turn["reply"]
        );
    }
    let log = fs::read_to_string(dir.join("fake-acp-log.jsonl"))?;
    ensure!(
        log.contains("set_model"),
        "no model selection in the ACP log"
    );
    ensure!(
        log.contains("session/set_model"),
        "no session/set_model in the ACP log"
    );
    Ok(())
}

const SCOPES: [&str; 21] = [
    "agents:read",
    "agents:profile",
    "history:read",
    "conversations:read",
    "projects:read",
    "routines:read",
    "workflows:read",
    "machines:read",
    "settings:read",
    "computer:read",
    "usage:read",
    "skills:read",
    "agents:write",
    "conversations:write",
    "projects:write",
    "routines:write",
    "agents:message",
    "workflows:run",
    "computer:run",
    "orchestration",
    "machine:update",
];

async fn rpc(app: &Server, token: &str, name: &str, args: Value) -> Result<(u16, Value)> {
    let response = app
        .client()?
        .post(app.url("/mcp"))
        .bearer_auth(token)
        .header("Content-Type", "application/json")
        .body(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}}).to_string())
        .send()
        .await?;
    let status = response.status().as_u16();
    Ok((status, response.json().await.unwrap_or(Value::Null)))
}

async fn call(app: &Server, token: &str, name: &str, args: Value) -> Result<Value> {
    let (status, body) = rpc(app, token, name, args).await?;
    ensure!(status == 200, "{name}: {status} {body}");
    ensure!(body["result"]["isError"] != true, "{name}: {body}");
    Ok(body["result"]["structuredContent"].clone())
}

#[tokio::test]
async fn rust_mcp_feature_receipts_scopes_cursors_and_stdio_forwarding() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let peer = app
        .post(
            "/api/mcp-server/peer-tokens",
            json!({"id": new_id(), "name": "Owner", "scopes": SCOPES}),
        )
        .await?;
    let limited = app
        .post(
            "/api/mcp-server/peer-tokens",
            json!({"id": new_id(), "name": "Limited", "scopes": ["agents:read"]}),
        )
        .await?;
    let token = peer["token"].as_str().ok_or("peer token")?.to_owned();
    let limited_token = limited["token"].as_str().ok_or("limited token")?;
    ensure!(
        rpc(&app, limited_token, "create_agent", json!({})).await?.0 == 403,
        "limited scope"
    );
    let agent = call(&app, &token, "create_agent", json!({"requestId": new_id(), "name": "Fixture", "model": "fixture", "instructions": "Help."})).await?;
    let agent_id = id(&agent)?;
    ensure!(
        call(&app, &token, "read_agent", json!({"agent": agent_id})).await?["name"] == "Fixture",
        "read_agent"
    );
    let input = json!({"requestId": new_id(), "agent": agent_id, "name": "Side"});
    let side = call(&app, &token, "create_conversation", input.clone()).await?;
    ensure!(
        call(&app, &token, "create_conversation", input.clone()).await?["id"] == side["id"],
        "conversation receipt"
    );
    let mut changed = input.clone();
    changed["name"] = json!("Changed");
    let (_, conflict) = rpc(&app, &token, "create_conversation", changed).await?;
    ensure!(
        conflict["result"]["isError"] == true,
        "conflict: {conflict}"
    );
    ensure!(
        conflict.to_string().contains("409"),
        "conflict status: {conflict}"
    );
    call(
        &app,
        &token,
        "rename_conversation",
        json!({"requestId": new_id(), "thread": side["id"], "revision": 1, "name": "Renamed"}),
    )
    .await?;
    ensure!(
        call(
            &app,
            &token,
            "list_conversations",
            json!({"agent": agent_id})
        )
        .await?["items"]
            .as_array()
            .map(Vec::len)
            == Some(2),
        "conversations"
    );
    ensure!(
        rpc(&app, &token, "list_projects", json!({"limit": 9}))
            .await?
            .0
            == 400,
        "page size"
    );
    let project = call(
        &app,
        &token,
        "create_project",
        json!({"requestId": new_id(), "name": "Fixture project", "members": [{"agent": agent_id, "write": false}]}),
    )
    .await?;
    let read = call(
        &app,
        &token,
        "read_project",
        json!({"project": project["id"]}),
    )
    .await?;
    ensure!(
        read["members"]["items"][0]["agent"] == agent_id.as_str(),
        "project members: {read}"
    );
    let section = call(
        &app,
        &token,
        "add_project_section",
        json!({"requestId": new_id(), "project": project["id"], "revision": 1, "name": "Research"}),
    )
    .await?;
    ensure!(
        section["sections"].as_array().map(Vec::len) == Some(2),
        "sections: {section}"
    );
    let mut child = tokio::process::Command::new(server_binary(None)?)
        .arg("--mcp-stdio")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("MINARBOT_MCP_URL", app.url("/mcp"))
        .env("MINARBOT_MCP_TOKEN", &token)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = child.stdin.take().ok_or("stdin")?;
        let lines = [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
        stdin.write_all(format!("{lines}\n").as_bytes()).await?;
    }
    let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
        .await
        .map_err(|_| "stdio bridge did not exit")??;
    ensure!(
        output.status.success(),
        "stdio exit: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let replies: Vec<Value> = stdout
        .trim()
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()?;
    ensure!(replies.len() == 2, "stdio replies: {stdout}");
    ensure!(
        replies[1]["result"]["tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "create_agent")),
        "stdio tools: {}",
        replies[1]
    );
    ensure!(!stdout.contains(&token), "stdio leaked the token");
    app.get("/api/runtimes").await?;
    let mut times = Vec::new();
    for _ in 0..30 {
        let start = Instant::now();
        let status = app
            .call(minarbot_testkit::http::Method::GET, "/api/runtimes", None)
            .await?
            .status;
        ensure!(status == 200, "runtimes: {status}");
        times.push(start.elapsed());
    }
    times.sort();
    ensure!(
        times[28] < Duration::from_millis(50),
        "/api/runtimes warm p95: {:?}",
        times[28]
    );
    Ok(())
}
