//! ACP and Pi harness runtimes (port of `app/acp-harness.test.ts`).

use crate::fx_support::id;
use minarbot_testkit::{
    fakes::{self, Fake},
    issuer::{SessionIssuer, SESSION_TOKEN},
    prelude::*,
    Server,
};
use std::{fs, os::unix::fs::symlink, path::PathBuf, process::Command, time::Duration};

const PROFILE: (&str, &str, &str) = ("ACP fixture", "saved-api-model", "Minarbot native context.");

/// One server with an agent bound to `runtime` on the fake ACP peer (the
/// Node `rustFixture`).
struct Acp {
    app: Server,
    root: TestRoot,
    workspace: PathBuf,
    path: String,
    _issuer: SessionIssuer,
}

impl Acp {
    async fn start(runtime: &str, write: bool, timeout: u64, model: &str) -> Result<Self> {
        let root = TestRoot::new()?;
        let workspace = root.dir("workspace")?;
        let bin = root.dir("bin")?;
        let executable = fakes::install(Fake::Acp, &bin, "fake-acp")?;
        let issuer = SessionIssuer::start().await?;
        let agent = new_id();
        let app = root
            .start(
                ServerConfig::new()
                    .option("timeout", timeout)
                    .workspace(&agent, &workspace, write)
                    .executable(runtime, &executable)
                    .issuer(&issuer),
            )
            .await?;
        let (name, saved, instructions) = PROFILE;
        app.post(
            "/api/agents",
            json!({"id": agent, "name": name, "model": saved, "instructions": instructions, "runtime": "api"}),
        )
        .await?;
        let path = format!("/api/agents/{agent}");
        app.post(
            &format!("{path}/edit"),
            json!({"id": new_id(), "revision": 1, "name": name, "model": saved, "instructions": instructions,
                "runtime": runtime, "harnessModel": model, "confirmRuntime": true}),
        )
        .await?;
        Ok(Self {
            app,
            root,
            workspace,
            path,
            _issuer: issuer,
        })
    }

    fn turns(&self) -> String {
        format!("{}/turns", self.path)
    }

    async fn send(&self, text: &str) -> Result<String> {
        let turn = new_id();
        self.app
            .post(&self.turns(), json!({"id": turn, "text": text}))
            .await?;
        Ok(turn)
    }

    async fn until(&self, turn: &str, done: impl Fn(&Value) -> bool) -> Result<Value> {
        self.app.wait_row(&self.turns(), turn, done).await
    }

    /// Answer every pending approval with `decision` until the turn ends.
    async fn complete(&self, turn: &str, decision: &str) -> Result<Value> {
        let terminal = |row: &Value| {
            ["done", "failed", "cancelled"].contains(&row["status"].as_str().unwrap_or(""))
        };
        loop {
            let row = self
                .until(turn, |row| {
                    terminal(row)
                        || row["harnessApprovals"].as_array().is_some_and(|cards| {
                            cards.iter().any(|card| card["state"] == "approval")
                        })
                })
                .await?;
            if terminal(&row) {
                return Ok(row);
            }
            for card in row["harnessApprovals"].as_array().into_iter().flatten() {
                if card["state"] == "approval" {
                    self.app
                        .post(
                            &format!("{}/{turn}/harness-approvals/{}", self.turns(), id(card)?),
                            json!({"id": new_id(), "revision": card["revision"], "decision": decision}),
                        )
                        .await?;
                }
            }
        }
    }

    fn log(&self) -> Result<Vec<Value>> {
        fakes::acp_log(&self.workspace)
    }
}

fn decisions(log: &[Value]) -> Vec<Value> {
    log.iter()
        .filter_map(|row| {
            row["result"]["outcome"]["optionId"]
                .as_str()
                .map(|id| json!(id))
        })
        .collect()
}

async fn supervised_session(runtime: &str) -> Result<()> {
    let f = Acp::start(runtime, true, 15_000, "fixture-model").await?;
    let done = f.complete(&f.send("hello").await?, "allow").await?;
    ensure!(done["status"] == "done", "{runtime}: {}", done["error"]);
    ensure!(done["reply"] == "Final answer.", "reply {}", done["reply"]);
    let stream: Vec<Value> = done["activity"]["stream"]
        .as_array()
        .ok_or("stream")?
        .iter()
        .map(|entry| json!([entry["kind"], entry["text"]]))
        .collect();
    ensure!(
        stream
            == vec![
                json!(["text", "Before tool. "]),
                json!(["tool", "echo fixture"]),
                json!(["text", "Final answer."])
            ],
        "stream {stream:?}"
    );
    ensure!(
        done["harnessApprovals"].as_array().map(Vec::len) == Some(1),
        "approvals"
    );
    ensure!(
        done["activity"]["harness"][0]["state"] == "completed",
        "tool state"
    );
    let log = f.log()?;
    let find = |method: &str| {
        log.iter()
            .find(|row| row["method"] == method)
            .cloned()
            .unwrap_or(Value::Null)
    };
    let init = find("initialize");
    let session = find("session/new");
    ensure!(
        init["params"]["protocolVersion"] == 1,
        "protocol {}",
        init["params"]
    );
    ensure!(
        init["params"]["clientCapabilities"]["terminal"] == false,
        "terminal capability"
    );
    ensure!(
        session["params"]["cwd"] == f.workspace.to_string_lossy().as_ref(),
        "cwd {}",
        session["params"]["cwd"]
    );
    ensure!(
        session.to_string().contains(SESSION_TOKEN),
        "session lacks the MCP token"
    );
    ensure!(
        !json!(log).to_string().contains("MINARBOT_HARNESS_CANARY"),
        "canary leaked to the peer"
    );
    ensure!(
        log.iter()
            .any(|row| row["method"] == "session/set_mode" && row["params"]["modeId"] == "default"),
        "no default mode"
    );
    ensure!(
        log.iter().any(|row| row["method"] == "session/set_model"
            && row["params"]["modelId"] == "fixture-model"),
        "no model selection"
    );
    ensure!(
        decisions(&log) == vec![json!("once")],
        "decisions {:?}",
        decisions(&log)
    );
    let prompt = find("session/prompt");
    let first = prompt["params"]["prompt"][0]["text"]
        .as_str()
        .unwrap_or_default();
    let second = prompt["params"]["prompt"][1]["text"]
        .as_str()
        .unwrap_or_default();
    ensure!(
        first.starts_with("<minarbot_context>\n<runtime_info>"),
        "context start"
    );
    ensure!(first.ends_with("</minarbot_context>"), "context end");
    ensure!(
        second.contains("Minarbot native context."),
        "instructions missing"
    );
    // The prompt now carries the transcript as `<minarbot-message>` tags; the
    // last one is the user's message (Node matched a trailing `user: hello`).
    let last_message = second
        .rsplit_once("<minarbot-message>")
        .and_then(|(_, tail)| tail.strip_suffix("</minarbot-message>"))
        .and_then(|tag| serde_json::from_str::<Value>(tag).ok())
        .unwrap_or(Value::Null);
    ensure!(
        last_message["role"] == "user" && last_message["content"] == "hello",
        "last prompt message {last_message} in {second:?}"
    );
    ensure!(
        prompt["params"]["sessionId"] == "fixture-session",
        "session id"
    );
    Ok(())
}

#[tokio::test]
async fn hermes_initialize_supervised_session_stream_segments_tool_one_use_permission_and_done(
) -> Result<()> {
    supervised_session("hermes").await
}

#[tokio::test]
async fn antigravity_initialize_supervised_session_stream_segments_tool_one_use_permission_and_done(
) -> Result<()> {
    supervised_session("antigravity").await
}

#[tokio::test]
async fn grok_initialize_supervised_session_stream_segments_tool_one_use_permission_and_done(
) -> Result<()> {
    supervised_session("grok").await
}

#[tokio::test]
async fn opencode_initialize_supervised_session_stream_segments_tool_one_use_permission_and_done(
) -> Result<()> {
    supervised_session("opencode").await
}

#[tokio::test]
async fn gemini_initialize_supervised_session_stream_segments_tool_one_use_permission_and_done(
) -> Result<()> {
    supervised_session("gemini").await
}

#[tokio::test]
async fn kiro_initialize_supervised_session_stream_segments_tool_one_use_permission_and_done(
) -> Result<()> {
    supervised_session("kiro").await
}

#[tokio::test]
async fn acp_resume_suppresses_replay_and_keeps_cwd_and_credentials_scoped() -> Result<()> {
    let f = Acp::start("hermes", true, 15_000, "fixture-model").await?;
    ensure!(
        f.complete(&f.send("hello").await?, "allow").await?["status"] == "done",
        "first turn"
    );
    let resumed = f.complete(&f.send("hello again").await?, "allow").await?;
    ensure!(resumed["status"] == "done", "resumed: {}", resumed["error"]);
    ensure!(
        !resumed.to_string().contains("Historic"),
        "historic chunk replayed"
    );
    let log = f.log()?;
    let loaded = log
        .iter()
        .find(|row| row["method"] == "session/load")
        .ok_or("no session/load")?;
    ensure!(
        loaded["params"]["sessionId"] == "fixture-session",
        "loaded session"
    );
    ensure!(
        loaded["params"]["cwd"] == f.workspace.to_string_lossy().as_ref(),
        "loaded cwd"
    );
    ensure!(
        loaded["params"]["mcpServers"]
            .to_string()
            .contains(SESSION_TOKEN),
        "loaded session lacks the MCP token"
    );
    Ok(())
}

#[tokio::test]
async fn acp_file_requests_reject_outside_paths_and_symlinks_require_write_approval() -> Result<()>
{
    let f = Acp::start("hermes", true, 15_000, "fixture-model").await?;
    let turn = f.send("FILES").await?;
    let pending = f
        .until(&turn, |row| {
            row["harnessApprovals"]
                .as_array()
                .is_some_and(|cards| cards.iter().any(|card| card["state"] == "approval"))
        })
        .await?;
    // Add the link after the workspace checkpoint, before the file requests.
    let outside = f.root.database().parent().ok_or("data dir")?.to_owned();
    fs::write(outside.join("escape.txt"), "Outside workspace fixture")?;
    symlink(&outside, f.workspace.join("outside"))?;
    ensure!(
        pending["status"] == "running",
        "pending status {}",
        pending["status"]
    );
    let done = f.complete(&turn, "allow").await?;
    ensure!(done["status"] == "done", "files: {}", done["error"]);
    ensure!(
        fs::read_to_string(f.workspace.join("hello.txt"))? == "hello\n",
        "hello.txt"
    );
    ensure!(
        done["harnessApprovals"].as_array().map(Vec::len) == Some(2),
        "approvals"
    );
    ensure!(
        !f.workspace
            .parent()
            .ok_or("parent")?
            .join("escape.txt")
            .exists(),
        "wrote outside the workspace"
    );
    Ok(())
}

#[tokio::test]
async fn acp_read_only_workspace_rejects_file_writes_without_approval() -> Result<()> {
    let f = Acp::start("hermes", false, 15_000, "fixture-model").await?;
    let done = f.complete(&f.send("READONLY").await?, "allow").await?;
    ensure!(done["status"] == "done", "readonly: {}", done["error"]);
    ensure!(
        !f.workspace.join("readonly.txt").exists(),
        "read-only write succeeded"
    );
    ensure!(
        done["harnessApprovals"].as_array().map(Vec::len) == Some(1),
        "approvals"
    );
    Ok(())
}

#[tokio::test]
async fn pi_rpc_streams_requests_tool_approval_selects_model_and_cancels() -> Result<()> {
    let f = Acp::start("pi", true, 15_000, "fixture/model").await?;
    let done = f.complete(&f.send("hello").await?, "allow").await?;
    ensure!(done["status"] == "done", "pi: {}", done["error"]);
    ensure!(done["reply"] == "Pi answer.", "reply {}", done["reply"]);
    ensure!(
        done["activity"]["stream"]
            .as_array()
            .is_some_and(|entries| entries.iter().any(|entry| entry["kind"] == "tool")),
        "no tool segment"
    );
    ensure!(
        done["harnessApprovals"].as_array().map(Vec::len) == Some(1),
        "approvals"
    );
    let choices = f.app.get(&format!("{}/runtimes", f.path)).await?;
    ensure!(
        choices["choices"]
            .as_array()
            .and_then(|choices| choices.iter().find(|choice| choice["runtime"] == "pi"))
            .is_some_and(|choice| choice["capabilities"]["resume"] == false),
        "pi resume capability {choices}"
    );
    ensure!(
        f.log()?.iter().any(|row| row["type"] == "set_model"
            && row["provider"] == "fixture"
            && row["modelId"] == "model"),
        "no Pi model selection"
    );
    let pending = f.send("WAIT").await?;
    f.until(&pending, |row| row["partial"]["text"] == "Waiting.")
        .await?;
    f.app
        .post(
            &format!("{}/{pending}/cancel", f.turns()),
            json!({"id": new_id()}),
        )
        .await?;
    let cancelled = f
        .until(&pending, |row| row["status"] == "cancelled")
        .await?;
    ensure!(cancelled["status"] == "cancelled", "cancel");
    Ok(())
}

#[tokio::test]
async fn acp_denied_permission_never_grants_persistent_option() -> Result<()> {
    let f = Acp::start("hermes", true, 15_000, "fixture-model").await?;
    let done = f.complete(&f.send("hello").await?, "deny").await?;
    ensure!(done["status"] == "done", "denied: {}", done["error"]);
    ensure!(
        done["activity"]["harness"][0]["state"] == "failed",
        "tool state {}",
        done["activity"]["harness"]
    );
    ensure!(
        decisions(&f.log()?) == vec![json!("deny")],
        "decisions {:?}",
        decisions(&f.log()?)
    );
    Ok(())
}

async fn bounded_failure(mode: &str) -> Result<()> {
    let f = Acp::start("hermes", true, 5_000, "fixture-model").await?;
    let turn = f.send(mode).await?;
    let failed = f
        .until(&turn, |row| {
            row["status"] == "done" || row["status"] == "failed"
        })
        .await?;
    ensure!(failed["status"] == "failed", "{mode}: {}", failed["status"]);
    let error = failed["error"].as_str().unwrap_or_default().to_lowercase();
    ensure!(
        ["stopped", "invalid", "timed out"]
            .iter()
            .any(|word| error.contains(word)),
        "{mode} error {error}"
    );
    let prompts = f
        .log()?
        .iter()
        .filter(|row| row["method"] == "session/prompt")
        .count();
    ensure!(prompts == 1, "{mode} prompts {prompts}");
    Ok(())
}

#[tokio::test]
async fn acp_crash_fail_bounded_no_replay() -> Result<()> {
    bounded_failure("CRASH").await
}

#[tokio::test]
async fn acp_invalid_fail_bounded_no_replay() -> Result<()> {
    bounded_failure("INVALID").await
}

#[tokio::test]
async fn acp_wait_fail_bounded_no_replay() -> Result<()> {
    bounded_failure("WAIT").await
}

/// Fields of `/proc/<pid>/stat` after the command name.
fn stat(pid: u32) -> Option<Vec<String>> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = text.rsplit_once(") ")?.1;
    Some(rest.split(' ').map(str::to_owned).collect())
}

#[tokio::test]
async fn acp_cancel_reaches_child_kills_detached_tool_process_and_preserves_inspect_diagnostics(
) -> Result<()> {
    let f = Acp::start("hermes", true, 15_000, "fixture-model").await?;
    let turn = f.send("TREE").await?;
    f.until(&turn, |row| row["partial"]["text"] == "Waiting.")
        .await?;
    let pid: u32 = fs::read_to_string(f.workspace.join("child-pid"))?
        .trim()
        .parse()?;
    let start = stat(pid)
        .and_then(|fields| fields.get(19).cloned())
        .ok_or("Detached tool did not start")?;
    let alive =
        || stat(pid).is_some_and(|fields| fields.get(19) == Some(&start) && fields[0] != "Z");
    ensure!(alive(), "detached tool not running");
    f.app
        .post(
            &format!("{}/{turn}/cancel", f.turns()),
            json!({"id": new_id()}),
        )
        .await?;
    let stopped = f.until(&turn, |row| row["status"] == "cancelled").await?;
    ensure!(
        stopped["reply"].is_null(),
        "reply after cancel {}",
        stopped["reply"]
    );
    let received = || f.workspace.join("cancel-received").exists();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while (alive() || !received()) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    ensure!(!alive(), "Detached tool survived cancellation");
    ensure!(received(), "Child had no time to handle session/cancel");
    let notices: Vec<Value> = f
        .log()?
        .into_iter()
        .filter(|row| row["method"] == "session/cancel")
        .map(|mut row| {
            if let Some(fields) = row.as_object_mut() {
                fields.retain(|key, _| ["jsonrpc", "method", "params"].contains(&key.as_str()));
            }
            row
        })
        .collect();
    ensure!(
        notices
            == vec![
                json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": "fixture-session"}})
            ],
        "cancel notices {notices:?}"
    );
    let inspection = f.app.get(&format!("/api/turns/{turn}/inspect")).await?;
    let step = &inspection["harness"][0];
    ensure!(
        inspection["status"] == "cancelled",
        "inspect status {}",
        inspection["status"]
    );
    ensure!(step["id"] == "worker", "step {step}");
    ensure!(step["state"] == "in_progress", "step state {step}");
    ensure!(
        !step["started"].is_null() && step["started"] != false,
        "step start"
    );
    let arguments: Value = serde_json::from_str(step["arguments"].as_str().unwrap_or("null"))?;
    ensure!(
        arguments == json!({"pid": pid}),
        "step arguments {arguments}"
    );
    ensure!(
        step["result"] == "Worker started",
        "step result {}",
        step["result"]
    );
    ensure!(
        inspection["engineStderr"] == "ACP cancelled [REDACTED]",
        "stderr {}",
        inspection["engineStderr"]
    );
    Ok(())
}

#[tokio::test]
async fn acp_runtimes_work_through_server_approvals_segments_and_runtime_picker() -> Result<()> {
    let root = TestRoot::new()?;
    let workspace = root.dir("workspace")?;
    let bin = root.dir("bin")?;
    let executable = fakes::install(Fake::Acp, &bin, "fake-acp")?;
    let issuer = SessionIssuer::start().await?;
    let agent = new_id();
    let mut config = ServerConfig::new()
        .option("timeout", 15_000)
        .workspace(&agent, &workspace, true)
        .issuer(&issuer);
    for runtime in [
        "hermes",
        "antigravity",
        "grok",
        "opencode",
        "gemini",
        "kiro",
        "pi",
    ] {
        config = config.executable(runtime, &executable);
    }
    let app = root.start(config).await?;
    app.post(
        "/api/agents",
        json!({"runtime": "api", "id": agent, "name": "ACP", "model": "saved-api-model", "instructions": "Help."}),
    )
    .await?;
    let turns = format!("/api/agents/{agent}/turns");
    for (revision, runtime) in ["hermes", "grok", "antigravity"].into_iter().enumerate() {
        app.post(
            &format!("/api/agents/{agent}/edit"),
            json!({"id": new_id(), "revision": revision + 1, "name": "ACP", "model": "saved-api-model",
                "instructions": "Help.", "runtime": runtime, "confirmRuntime": true}),
        )
        .await?;
        let turn = new_id();
        app.post(&turns, json!({"id": turn, "text": "hello"}))
            .await?;
        let done = until(format!("{runtime} turn"), || async {
            let row = app.row(&turns, &turn).await?.ok_or("turn missing")?;
            for card in row["harnessApprovals"].as_array().into_iter().flatten() {
                if card["state"] == "approval" {
                    app.post(
                        &format!("{turns}/{turn}/harness-approvals/{}", id(card)?),
                        json!({"id": new_id(), "revision": card["revision"], "decision": "allow"}),
                    )
                    .await?;
                }
            }
            ensure!(
                !["failed", "cancelled"].contains(&row["status"].as_str().unwrap_or("")),
                "{runtime} turn ended {row}"
            );
            Ok((row["status"] == "done").then_some(row))
        })
        .await?;
        ensure!(
            done["reply"] == "Final answer.",
            "{runtime} reply {}",
            done["reply"]
        );
        ensure!(
            done["activity"]["stream"].as_array().map(Vec::len) == Some(3),
            "{runtime} stream"
        );
    }
    let choices = app.get(&format!("/api/agents/{agent}/runtimes")).await?;
    ensure!(
        choices["choices"].as_array().is_some_and(|choices| choices
            .iter()
            .any(|choice| choice["runtime"] == "hermes" && choice["available"] == true)),
        "hermes not available: {choices}"
    );
    Ok(())
}

#[tokio::test]
async fn acp_room_queue_checkpoints_and_stop_preserve_peer_approvals() -> Result<()> {
    let root = TestRoot::new()?;
    // Project folders must sit inside the server's home.
    let workspace = root.dir("home/acp-room")?;
    let bin = root.dir("bin")?;
    let executable = fakes::install(Fake::Acp, &bin, "fake-acp")?;
    let git = |args: &[&str]| -> Result<()> {
        let output = Command::new("git")
            .args(["-c", "core.hooksPath=/dev/null"])
            .args(args)
            .current_dir(&workspace)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()?;
        ensure!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    };
    git(&["init", "-q"])?;
    git(&[
        "-c",
        "user.name=ACP Test",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "--allow-empty",
        "-qm",
        "Fixture",
    ])?;
    let issuer = SessionIssuer::start().await?;
    let app = root
        .start(
            ServerConfig::new()
                .option("timeout", 20_000)
                .executable("hermes", &executable)
                .issuer(&issuer),
        )
        .await?;
    let mut agents = Vec::new();
    for name in ["First", "Peer"] {
        agents.push(
            app.post(
                "/api/agents",
                json!({"runtime": "api", "id": new_id(), "name": name, "model": "api-model", "instructions": "Help."}),
            )
            .await?,
        );
    }
    let project = app
        .post(
            "/api/projects",
            json!({"id": new_id(), "name": "ACP room", "folders": [{"mode": "existing", "path": workspace}],
                "members": agents.iter().map(|agent| json!({"agent": agent["id"], "write": true})).collect::<Vec<_>>(),
                "confirmWrite": true}),
        )
        .await?;
    for agent in &agents {
        app.post(
            &format!("/api/agents/{}/edit", id(agent)?),
            json!({"id": new_id(), "revision": 1, "name": agent["name"], "model": "api-model",
                "instructions": "Help.", "runtime": "hermes", "confirmRuntime": true}),
        )
        .await?;
    }
    let path = format!("/api/channels/{}/turns", id(&project["sections"][0])?);
    let root_turn = new_id();
    app.post(&path, json!({"id": root_turn, "text": "hello"}))
        .await?;
    let rows = app
        .wait_get(&path, |rows| {
            rows.as_array().is_some_and(|rows| {
                rows.iter().any(|row| {
                    row["harnessApprovals"]
                        .as_array()
                        .is_some_and(|cards| !cards.is_empty())
                })
            })
        })
        .await?;
    let rows = rows.as_array().ok_or("rows")?;
    let active = rows
        .iter()
        .find(|row| row["status"] == "running")
        .ok_or("no running member")?
        .clone();
    let queued = rows
        .iter()
        .find(|row| row["status"] == "queued")
        .ok_or("Room peer waits in serialized queue")?
        .clone();
    let (active_id, queued_id) = (id(&active)?, id(&queued)?);
    let approval = &active["harnessApprovals"][0];
    app.post(
        &format!("{path}/{active_id}/harness-approvals/{}", id(approval)?),
        json!({"id": new_id(), "revision": approval["revision"], "decision": "allow"}),
    )
    .await?;
    app.wait_row(&path, &queued_id, |row| {
        row["harnessApprovals"]
            .as_array()
            .is_some_and(|cards| !cards.is_empty())
    })
    .await?;
    let first = app.row(&path, &active_id).await?.ok_or("active row")?;
    ensure!(
        first["status"] == "done",
        "first member {}",
        first["status"]
    );
    ensure!(
        first["checkpoint"]["restorable"] == true,
        "checkpoint {}",
        first["checkpoint"]
    );
    app.post(
        &path.replace("/turns", &format!("/discussions/{root_turn}/stop")),
        json!({"id": new_id(), "generation": root_turn}),
    )
    .await?;
    let stopped = app
        .wait_row(&path, &queued_id, |row| {
            row["harnessApprovals"][0]["state"] == "expired"
        })
        .await?;
    ensure!(
        stopped["harnessApprovals"][0]["state"] == "expired",
        "peer approval kept"
    );
    Ok(())
}
