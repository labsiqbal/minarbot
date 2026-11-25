//! Default harness binding, device sign-ins, instance homes and native setup
//! (port of `app/harness-default.test.ts`).
//!
//! #87 removed the `runtime` field and the implicit default runtime on
//! create: a new agent is unbound until it gets a provider instance (in test
//! mode an edit with `runtime` and `confirmRuntime` binds the runtime's
//! default instance). Provider accounts became instances.

use crate::sb_support::{id, settle};
use minarbot_testkit::{
    fakes::{self, Fake},
    issuer::{SessionIssuer, SESSION_TOKEN},
    prelude::*,
    Server,
};
use std::{fs, path::Path};

fn entries(directory: &Path) -> Result<Vec<String>> {
    let mut names: Vec<String> = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<_>>()?;
    names.sort();
    Ok(names)
}

fn log(path: &Path) -> Result<Vec<Value>> {
    fakes::read_log(path)
}

/// Managed workspace of `agent` (beside the database).
fn managed(root: &TestRoot, agent: &str) -> Result<std::path::PathBuf> {
    Ok(root
        .database()
        .parent()
        .ok_or("database directory")?
        .join("agent-workspaces")
        .join(agent))
}

async fn bind(
    app: &Server,
    agent: &str,
    revision: i64,
    runtime: &str,
    model: &str,
) -> Result<Value> {
    app.post(
        &format!("/api/agents/{agent}/edit"),
        json!({"id": new_id(), "revision": revision, "name": "Coder", "model": model, "instructions": "Help.",
            "runtime": runtime, "confirmRuntime": true}),
    )
    .await
}

async fn hello(app: &Server, agent: &str, text: &str) -> Result<Value> {
    let path = format!("/api/agents/{agent}/turns");
    let id = new_id();
    app.post(&path, json!({"id": id, "text": text})).await?;
    let row = settle(app, &path, &id).await?;
    ensure!(row["status"] == "done", "{text}: {}", row["error"]);
    Ok(row)
}

fn choice(runtimes: &Value, runtime: &str) -> Value {
    runtimes["choices"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["runtime"] == runtime))
        .cloned()
        .unwrap_or(Value::Null)
}

#[tokio::test]
async fn new_agents_default_to_codex_with_a_managed_workspace_api_agents_keep_runtime_and_switch_in_one_saved_command(
) -> Result<()> {
    let root = TestRoot::new()?;
    let bin = root.dir("bin")?;
    let codex = fakes::install(Fake::Codex, &bin, "codex")?;
    let config = ServerConfig::new().executable("codex", &codex);
    let mut app = root.start(config).await?;
    let profile = json!({"name": "New agent", "model": "cx/gpt-6.1-sol", "instructions": "Help."});
    let mut create = profile.clone();
    create["id"] = json!(new_id());
    let created = app.post("/api/agents", create).await?;
    let created_id = id(&created)?;
    ensure!(
        created["providerInstance"] == "",
        "implicit binding: {created}"
    );
    let mut legacy = profile.clone();
    legacy["id"] = json!(new_id());
    legacy["runtime"] = json!("api");
    let legacy = app.post("/api/agents", legacy).await?;
    let legacy_id = id(&legacy)?;
    let mut replay = profile.clone();
    replay["id"] = json!(legacy_id);
    ensure!(
        app.post("/api/agents", replay).await? == legacy,
        "create receipt"
    );
    let choices = app
        .get(&format!("/api/agents/{legacy_id}/runtimes"))
        .await?;
    ensure!(
        choice(&choices, "codex")["enabled"] == true,
        "codex choice: {choices}"
    );
    let edit = json!({"id": new_id(), "revision": 1, "name": "New agent", "model": "cx/gpt-6.1-sol",
        "instructions": "Help.", "runtime": "codex", "confirmRuntime": true});
    let bound = app
        .post(&format!("/api/agents/{created_id}/edit"), edit.clone())
        .await?;
    ensure!(
        app.post(&format!("/api/agents/{created_id}/edit"), edit)
            .await?
            == bound,
        "edit receipt"
    );
    ensure!(
        bound["providerInstance"] == "codex" && bound["revision"] == 2,
        "binding: {bound}"
    );
    hello(&app, &created_id, "Hello").await?;
    ensure!(
        managed(&root, &created_id)?.is_dir(),
        "managed workspace missing"
    );
    app.restart().await?;
    let agents = app.get("/api/agents").await?["agents"].clone();
    let saved = agents
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == created_id.as_str()))
        .ok_or("agent")?;
    ensure!(
        saved["workspace_connected"] == true,
        "workspace after restart: {saved}"
    );
    ensure!(
        saved["providerInstance"] == "codex" && saved["revision"] == 2,
        "binding after restart: {saved}"
    );
    ensure!(
        app.get("/api/runtimes").await?["defaultRuntime"] == "codex",
        "default runtime"
    );
    Ok(())
}

/// #87 replaced the `device` provider account with instances: this
/// machine's Codex sign-in is an instance whose home is the CLI folder
/// (`homePath`, which sets `CODEX_HOME`). Dropped with the account: the
/// isolated `HOME` for device turns (a8cfcac sets `isolated_home = false`)
/// and the 400 for a device account on the API runtime.
#[tokio::test]
async fn agents_on_this_machines_codex_sign_in_use_the_cli_folder_without_writing_to_it(
) -> Result<()> {
    let root = TestRoot::new()?;
    let bin = root.dir("bin")?;
    let codex = fakes::install(Fake::Codex, &bin, "codex")?;
    let device = root.dir("device-codex")?;
    fs::write(
        device.join("auth.json"),
        json!({"tokens": {"access_token": "synthetic-device-access", "refresh_token": "synthetic-device-refresh"}}).to_string(),
    )?;
    let app = root
        .start(
            ServerConfig::new()
                .executable("codex", &codex)
                .env("CODEX_HOME", device.display().to_string()),
        )
        .await?;
    ensure!(
        choice(&app.get("/api/runtimes").await?, "codex")["signedIn"] == true,
        "device sign-in"
    );
    app.post_expect(
        "/api/provider-instances",
        json!({"id": new_id(), "instanceId": "codex-device", "kind": "codex", "displayName": "This machine",
            "enabled": true, "binaryPath": codex, "homePath": device}),
        201,
    )
    .await?;
    let agent = id(&app
        .post(
            "/api/agents",
            json!({"id": new_id(), "name": "Coder", "model": "default", "instructions": "Help.", "providerInstance": "codex-device"}),
        )
        .await?)?;
    let listed = app.get("/api/agents").await?["agents"].clone();
    ensure!(
        listed.as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row["id"] == agent.as_str() && row["provider_available"] == true)),
        "provider unavailable: {listed}"
    );
    hello(&app, &agent, "Hello").await?;
    let start = log(&managed(&root, &agent)?.join("fake-log.jsonl"))?
        .into_iter()
        .find(|item| item["method"] == "thread/start")
        .ok_or("thread/start")?;
    ensure!(
        start["codexHome"] == device.display().to_string(),
        "CODEX_HOME: {start}"
    );
    ensure!(
        !start["home"]
            .as_str()
            .unwrap_or_default()
            .starts_with(&device.display().to_string()),
        "harness HOME inside the device folder: {start}"
    );
    ensure!(
        entries(&device)? == ["auth.json"],
        "device folder changed: {:?}",
        entries(&device)?
    );
    Ok(())
}

#[tokio::test]
async fn agents_on_this_machines_claude_code_sign_in_run_with_the_real_home_and_add_nothing_to_claude(
) -> Result<()> {
    let root = TestRoot::new()?;
    let bin = root.dir("bin")?;
    let claude = fakes::install(Fake::Claude, &bin, "claude")?;
    let home = root.dir("owner-home")?;
    fs::create_dir_all(home.join(".claude"))?;
    fs::write(
        home.join(".claude/.credentials.json"),
        json!({"claudeAiOauth": {"accessToken": "synthetic-device-access", "refreshToken": "synthetic-device-refresh"}}).to_string(),
    )?;
    let app = root
        .start(
            ServerConfig::new()
                .executable("claude", &claude)
                .env("HOME", home.display().to_string()),
        )
        .await?;
    ensure!(
        choice(&app.get("/api/runtimes").await?, "claude")["signedIn"] == true,
        "device sign-in"
    );
    let agent = id(&app
        .post(
            "/api/agents",
            json!({"id": new_id(), "name": "Clara", "model": "default", "instructions": "Help."}),
        )
        .await?)?;
    bind(&app, &agent, 1, "claude", "default").await?;
    hello(&app, &agent, "Hello").await?;
    let turn = log(&managed(&root, &agent)?.join("fake-log.jsonl"))?
        .into_iter()
        .find(|item| item["type"] == "turn")
        .ok_or("turn log")?;
    ensure!(turn["home"] == home.display().to_string(), "HOME: {turn}");
    ensure!(
        turn["claudeConfigDir"].is_null(),
        "CLAUDE_CONFIG_DIR set: {turn}"
    );
    ensure!(
        entries(&home.join(".claude"))? == [".credentials.json"],
        "~/.claude changed"
    );
    Ok(())
}

async fn rpc(app: &Server, name: &str, args: Value) -> Result<Value> {
    let response: Value = app
        .client()?
        .post(app.url("/mcp"))
        .bearer_auth(SESSION_TOKEN)
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "id": new_id(), "method": "tools/call", "params": {"name": name, "arguments": args}}))
        .send()
        .await?
        .json()
        .await?;
    Ok(response["result"].clone())
}

/// Provider accounts became Codex instances with their own secret key.
/// Dropped with the accounts: per-binding harness home rotation and its
/// `config.toml` (a8cfcac sets `isolated_home = false`); harness turns now
/// also see the full offered tool set (5528da5), so the "not offered" list
/// left. The Node case's "workspace checks run without approval" is kept in
/// Node for B8 (`docs/test-ledger.md`): in test mode every `run_shell` asks.
#[tokio::test]
async fn provider_isolation_account_home_rotation_and_mcp_shell_approval_boundaries() -> Result<()>
{
    let root = TestRoot::new()?;
    let bin = root.dir("bin")?;
    let codex = fakes::install(Fake::Codex, &bin, "codex")?;
    let issuer = SessionIssuer::start().await?;
    let app = root
        .start(
            ServerConfig::new()
                .executable("codex", &codex)
                .issuer(&issuer),
        )
        .await?;
    let keys = ["synthetic-first-credential", "synthetic-second-credential"];
    for (index, key) in keys.iter().enumerate() {
        app.post_expect(
            "/api/provider-instances",
            json!({"id": new_id(), "instanceId": format!("codex-{index}"), "kind": "codex",
                "displayName": format!("Codex {index}"), "enabled": true, "binaryPath": codex,
                "environment": [{"name": "MINARBOT_PROVIDER_KEY", "value": key, "secret": true}]}),
            201,
        )
        .await?;
    }
    let agent = id(&app
        .post(
            "/api/agents",
            json!({"id": new_id(), "name": "Coder", "model": "router/model", "providerInstance": "codex-0", "instructions": "Help."}),
        )
        .await?)?;
    let path = format!("/api/agents/{agent}");
    let workspace = managed(&root, &agent)?;
    for (index, key) in keys.iter().enumerate() {
        if index > 0 {
            app.post(
                &format!("{path}/edit"),
                json!({"id": new_id(), "revision": 1, "name": "Coder", "model": "router/model",
                    "instructions": "Help.", "providerInstance": "codex-1"}),
            )
            .await?;
        }
        let row = hello(&app, &agent, "FINAL_PROVIDER_SECRET").await?;
        ensure!(!row.to_string().contains(key), "reply leaked {key}");
        ensure!(
            row["reply"]
                .as_str()
                .is_some_and(|reply| reply.contains("REDACTED")),
            "reply: {}",
            row["reply"]
        );
    }
    let turn = new_id();
    app.post(
        &format!("{path}/turns"),
        json!({"id": turn, "text": "HOLD"}),
    )
    .await?;
    app.wait_row(&format!("{path}/turns"), &turn, |row| {
        row["partial"]["text"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
    })
    .await?;
    let catalog: Value = app
        .client()?
        .post(app.url("/mcp"))
        .bearer_auth(SESSION_TOKEN)
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .send()
        .await?
        .json()
        .await?;
    let names: Vec<String> = catalog["result"]["tools"]
        .as_array()
        .ok_or_else(|| format!("catalog: {catalog}"))?
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect();
    for name in [
        "run_shell",
        "verify_result",
        "create_text_file",
        "read_workspace_file",
        "open_file_in_panel",
        "recall",
        "reflect",
        "remember",
        "search_history",
        "search_memory",
        "remember_memory",
        "select_memory",
    ] {
        ensure!(
            names.iter().any(|tool| tool == name),
            "{name} missing: {names:?}"
        );
    }
    let approve_next = || async {
        let card = until_value(
            "shell approval",
            || async {
                let row = app
                    .row(&format!("{path}/turns"), &turn)
                    .await?
                    .ok_or("turn")?;
                Ok(row["harnessApprovals"]
                    .as_array()
                    .and_then(|cards| {
                        cards
                            .iter()
                            .find(|card| card["state"] == "approval")
                            .cloned()
                    })
                    .unwrap_or(Value::Null))
            },
            |card| !card.is_null(),
        )
        .await?;
        Ok::<_, minarbot_testkit::Error>(card)
    };
    // B8: in test mode each workspace check asks; allow it so the check runs.
    let normal = {
        let call = rpc(
            &app,
            "run_shell",
            json!({"command": "echo hello > result; cat result"}),
        );
        let card = async {
            let card = approve_next().await?;
            app.post(
                &format!(
                    "{path}/turns/{turn}/harness-approvals/{}",
                    card["id"].as_str().unwrap_or_default()
                ),
                json!({"id": new_id(), "revision": card["revision"], "decision": "allow"}),
            )
            .await
        };
        let (result, approved) = tokio::join!(call, card);
        approved?;
        result?
    };
    ensure!(
        normal["structuredContent"]["exitCode"] == 0,
        "normal shell: {normal}"
    );
    ensure!(
        normal["structuredContent"]["verification"]
            == json!({"recorded": true, "evidenceInWorkLog": true}),
        "verification: {normal}"
    );
    ensure!(
        fs::read_to_string(workspace.join("result"))? == "hello\n",
        "shell output file"
    );
    let sensitive = rpc(&app, "run_shell", json!({"command": "rm result"}));
    let deny = async {
        let card = approve_next().await?;
        app.post(
            &format!(
                "{path}/turns/{turn}/harness-approvals/{}",
                card["id"].as_str().unwrap_or_default()
            ),
            json!({"id": new_id(), "revision": card["revision"], "decision": "deny"}),
        )
        .await
    };
    let (removed, denied) = tokio::join!(sensitive, deny);
    denied?;
    ensure!(removed?["isError"] == true, "denied command ran");
    ensure!(
        workspace.join("result").exists(),
        "denied command removed the file"
    );
    rpc(
        &app,
        "ask_user",
        json!({"question": "Which report?", "choices": ["Weekly", "Daily"]}),
    )
    .await?;
    let paused = app
        .row(&format!("{path}/turns"), &turn)
        .await?
        .ok_or("turn")?;
    ensure!(paused["paused"] == 1, "question did not pause: {paused}");
    ensure!(
        paused["questions"][0]["prompt"] == "Which report?",
        "question: {paused}"
    );
    app.post(
        &format!(
            "{path}/turns/{turn}/questions/{}/answer",
            paused["questions"][0]["id"].as_str().ok_or("question")?
        ),
        json!({"id": new_id(), "text": "Weekly"}),
    )
    .await?;
    app.post(
        &format!("{path}/turns/{turn}/cancel"),
        json!({"id": new_id()}),
    )
    .await?;
    Ok(())
}

async fn native_setup(runtime: &str) -> Result<()> {
    let root = TestRoot::new()?;
    let bin = root.dir("bin")?;
    let fake = fakes::install(Fake::native(runtime)?, &bin, runtime)?;
    let app = root
        .start(ServerConfig::new().executable(runtime, &fake))
        .await?;
    app.post_expect(
        "/api/provider-instances",
        json!({"id": new_id(), "instanceId": runtime, "kind": runtime, "displayName": runtime, "enabled": true}),
        201,
    )
    .await?;
    let agent = new_id();
    app.post(
        "/api/agents",
        json!({"id": agent, "name": "New agent", "model": "fixture", "instructions": "NATIVE_SETUP_FIXTURE",
            "setup": true, "providerInstance": runtime}),
    )
    .await?;
    let path = format!("/api/agents/{agent}/turns");
    let turn = until_value(
        "setup turn",
        || async { Ok(app.get(&path).await?[0].clone()) },
        |row| ["done", "failed"].contains(&row["status"].as_str().unwrap_or_default()),
    )
    .await?;
    ensure!(
        turn["status"] == "done",
        "{runtime} setup: {}",
        turn["error"]
    );
    let profile = app.get("/api/agents").await?["agents"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == agent.as_str()).cloned())
        .ok_or("agent")?;
    ensure!(
        profile["name"] == "Native assistant",
        "{runtime} name: {profile}"
    );
    ensure!(
        profile["instructions"] == "Native setup saved.",
        "{runtime} instructions: {profile}"
    );
    ensure!(profile["revision"] == 2, "{runtime} revision: {profile}");
    let edits = turn["activity"]["tools"].as_array().map_or(0, |tools| {
        tools
            .iter()
            .filter(|tool| tool["name"] == "edit_profile")
            .count()
    });
    ensure!(edits == 1, "{runtime} edit_profile calls: {edits}");
    Ok(())
}

#[tokio::test]
async fn default_harness_setup_saves_its_profile_through_mcp_and_cannot_use_unrelated_tools_codex(
) -> Result<()> {
    native_setup("codex").await
}

#[tokio::test]
async fn default_harness_setup_saves_its_profile_through_mcp_and_cannot_use_unrelated_tools_claude(
) -> Result<()> {
    native_setup("claude").await
}

#[tokio::test]
async fn new_agents_fall_back_to_the_api_runtime_when_codex_is_not_installed() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    ensure!(
        app.get("/api/runtimes").await?["defaultRuntime"] == "api",
        "default runtime"
    );
    let created = app
        .post("/api/agents", json!({"id": new_id(), "name": "No Codex", "model": "cx/gpt-6.1-sol", "instructions": "Help."}))
        .await?;
    // #87 removed the `api` runtime; without an engine the agent stays unbound.
    ensure!(
        created["providerInstance"] == "",
        "binding without an engine: {created}"
    );
    let listed = app.get("/api/agents").await?["agents"][0].clone();
    ensure!(
        listed["runnable"] == false,
        "unbound agent runnable: {listed}"
    );
    Ok(())
}
