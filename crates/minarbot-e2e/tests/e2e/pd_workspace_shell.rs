//! The workspace `run_shell` tool (port of `app/workspace-shell.test.ts`).

use minarbot_testkit::{
    model::{Reply, ScriptedModel},
    prelude::*,
    wait::never,
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

/// Each turn's text is its id; the model calls `run_shell` with the
/// arguments registered for that id, then records the tool result.
struct Shell {
    app: Server,
    _model: ScriptedModel,
    root: PathBuf,
    workspace: PathBuf,
    writable: String,
    readonly: String,
    args: Arc<Mutex<HashMap<String, Value>>>,
    results: Arc<Mutex<HashMap<String, Value>>>,
}

struct Turn<'a> {
    shell: &'a Shell,
    id: String,
    turns: String,
}

impl Shell {
    async fn start() -> Result<Self> {
        let args: Arc<Mutex<HashMap<String, Value>>> = Arc::default();
        let results: Arc<Mutex<HashMap<String, Value>>> = Arc::default();
        let model = ScriptedModel::start().await?;
        let (planned, seen) = (args.clone(), results.clone());
        model.respond(move |request| {
            let id = request.last_user_text();
            let last = request.messages().last().cloned().unwrap_or(Value::Null);
            if last["role"] == "tool" {
                let result = serde_json::from_str(last["content"].as_str().unwrap_or(""))
                    .unwrap_or(Value::Null);
                if let Ok(mut seen) = seen.lock() {
                    seen.insert(id, result);
                }
                return Reply::text("Checked shell result.");
            }
            let arguments = planned
                .lock()
                .ok()
                .and_then(|planned| planned.get(&id).cloned())
                .unwrap_or(Value::Null);
            Reply::tool_call(&id, "run_shell", arguments)
        });
        let root = TestRoot::new()?;
        let workspace = root.dir("workspace")?;
        let (writable, readonly) = (new_id(), new_id());
        let config = ServerConfig::new()
            .model(&model)
            .workspace(&writable, &workspace, true)
            .workspace(&readonly, &workspace, false)
            .env("MINARBOT_SHELL_TEST_CANARY", "shell-secret-fixture");
        let app = root.start(config).await?;
        for id in [&writable, &readonly] {
            app.post(
                "/api/agents",
                json!({"id": id, "runtime": "api", "name": "Shell", "model": "fixture",
                    "instructions": "Help."}),
            )
            .await?;
        }
        Ok(Self {
            app,
            _model: model,
            root: root.path().to_owned(),
            workspace,
            writable,
            readonly,
            args,
            results,
        })
    }

    async fn send(&self, input: Value, write: bool) -> Result<Turn<'_>> {
        let id = new_id();
        let agent = if write {
            &self.writable
        } else {
            &self.readonly
        };
        let turns = format!("/api/agents/{agent}/turns");
        if let Ok(mut args) = self.args.lock() {
            args.insert(id.clone(), input);
        }
        self.app.post(&turns, json!({"id": id, "text": id})).await?;
        Ok(Turn {
            shell: self,
            id,
            turns,
        })
    }
}

impl Turn<'_> {
    async fn until(&self, done: impl Fn(&Value) -> bool) -> Result<Value> {
        self.shell.app.wait_row(&self.turns, &self.id, done).await
    }

    async fn decide(&self, decision: &str) -> Result<()> {
        let row = self
            .until(|row| {
                row["status"] == "failed"
                    || row["harnessApprovals"]
                        .as_array()
                        .is_some_and(|cards| cards.iter().any(|card| card["state"] == "approval"))
            })
            .await?;
        ensure!(row["status"] != "failed", "turn failed: {}", row["error"]);
        let approval = row["harnessApprovals"]
            .as_array()
            .and_then(|cards| cards.iter().find(|card| card["state"] == "approval"))
            .cloned()
            .ok_or("approval card")?;
        self.shell
            .app
            .post(
                &format!(
                    "{}/{}/harness-approvals/{}",
                    self.turns,
                    self.id,
                    approval["id"].as_str().ok_or("approval id")?
                ),
                json!({"id": new_id(), "revision": approval["revision"], "decision": decision}),
            )
            .await?;
        Ok(())
    }

    /// B8: a writable binding runs a non-sensitive command without a card.
    async fn unapproved(&self) -> Result<()> {
        let row = self
            .until(|row| row["status"] != "running" && row["status"] != "queued")
            .await?;
        ensure!(
            row["harnessApprovals"] == json!([]),
            "approval requested: {}",
            row["harnessApprovals"]
        );
        Ok(())
    }

    async fn cancel(&self) -> Result<()> {
        self.shell
            .app
            .post(
                &format!("{}/{}/cancel", self.turns, self.id),
                json!({"id": new_id()}),
            )
            .await?;
        Ok(())
    }

    async fn result(&self) -> Result<Value> {
        let row = self
            .until(|row| ["done", "failed"].contains(&row["status"].as_str().unwrap_or("")))
            .await?;
        ensure!(row["status"] == "done", "turn failed: {}", row["error"]);
        self.shell
            .results
            .lock()
            .ok()
            .and_then(|results| results.get(&self.id).cloned())
            .ok_or_else(|| "no shell result recorded".into())
    }
}

#[tokio::test]
async fn rust_shell_confines_cwd_filesystem_and_environment_and_enforces_read_only_and_approval(
) -> Result<()> {
    let shell = Shell::start().await?;
    std::fs::write(shell.root.join("outside"), "OUTSIDE_SHELL_CANARY")?;
    std::os::unix::fs::symlink(&shell.root, shell.workspace.join("escape"))?;
    for (args, status) in [
        (json!({"command": "pwd", "cwd": ".."}), 400),
        (json!({"command": "pwd", "check": "yes"}), 400),
        (json!({"command": "pwd", "cwd": "escape"}), 403),
    ] {
        let turn = shell.send(args.clone(), true).await?;
        let result = turn.result().await?;
        let error = result["error"].as_str().unwrap_or("").to_lowercase();
        ensure!(
            error.contains("shell command") || error.contains("workspace"),
            "{args}: {result}"
        );
        ensure!(result["status"] == status, "{args}: {result}");
        let row = turn.until(|row| row["status"] == "done").await?;
        ensure!(
            row["harnessApprovals"] == json!([]),
            "{args}: approvals {}",
            row["harnessApprovals"]
        );
    }
    let checked = shell
        .send(
            json!({"command": "python3 -c \"assert 2 + 2 == 4\"", "check": true}),
            true,
        )
        .await?;
    let checked_result = checked.result().await?;
    checked.unapproved().await?;
    ensure!(checked_result["exitCode"] == 0, "check: {checked_result}");
    ensure!(checked_result["check"] == true, "check: {checked_result}");

    // `chmod` still needs approval on a writable binding.
    let denied = shell
        .send(json!({"command": "touch denied; chmod 600 denied"}), true)
        .await?;
    denied.decide("deny").await?;
    let denied_result = denied.result().await?;
    ensure!(
        denied_result["error"]
            .as_str()
            .is_some_and(|error| error.contains("denied")),
        "denied: {denied_result}"
    );
    ensure!(
        !shell.workspace.join("denied").exists(),
        "denied command ran"
    );

    let allowed = shell
        .send(
            json!({"command": "pwd; env; cat escape/outside; echo ok > result"}),
            true,
        )
        .await?;
    allowed.decide("allow").await?;
    let result = allowed.result().await?;
    let output = result["output"].as_str().unwrap_or("");
    ensure!(output.contains("/workspace"), "output: {output}");
    for leaked in [
        "shell-secret-fixture",
        "NODE_OPTIONS",
        "OUTSIDE_SHELL_CANARY",
    ] {
        ensure!(!output.contains(leaked), "output leaks {leaked}: {output}");
    }
    ensure!(
        std::fs::read_to_string(shell.workspace.join("result"))? == "ok\n",
        "result file"
    );

    let readonly = shell
        .send(json!({"command": "echo changed > result"}), false)
        .await?;
    readonly.decide("allow").await?;
    let readonly_result = readonly.result().await?;
    ensure!(
        readonly_result["exitCode"] != 0,
        "read-only write succeeded: {readonly_result}"
    );
    ensure!(
        std::fs::read_to_string(shell.workspace.join("result"))? == "ok\n",
        "read-only agent changed the file"
    );
    Ok(())
}

#[tokio::test]
async fn rust_shell_caps_output_times_out_and_stops_descendants_after_exit_and_cancellation(
) -> Result<()> {
    let shell = Shell::start().await?;
    let run = |command: &'static str, timeout: u64| {
        let shell = &shell;
        async move {
            let turn = shell
                .send(json!({"command": command, "timeoutMs": timeout}), true)
                .await?;
            let result = turn.result().await?;
            turn.unapproved().await?;
            Result::<Value>::Ok(result)
        }
    };
    let bounded = run("yes x", 2000).await?;
    ensure!(
        bounded["truncated"] == true,
        "bounded: {}",
        bounded["truncated"]
    );
    ensure!(
        bounded["output"].as_str().map_or(0, str::len) <= 256 * 1024,
        "output size: {}",
        bounded["output"].as_str().map_or(0, str::len)
    );
    let timed = run("sleep 30", 100).await?;
    ensure!(timed["timedOut"] == true, "timeout: {timed}");
    run(
        "(sleep 1; echo escaped > orphan) >/dev/null 2>&1 & exit 0",
        2000,
    )
    .await?;

    let cancelled = shell
        .send(
            json!({"command": "touch started; (sleep 2; echo escaped > aborted) & wait"}),
            true,
        )
        .await?;
    let started = shell.workspace.join("started");
    until("shell command started", || async {
        Ok(started.exists().then_some(()))
    })
    .await?;
    cancelled.cancel().await?;
    cancelled.until(|row| row["status"] == "cancelled").await?;
    cancelled.unapproved().await?;
    let (orphan, aborted) = (
        shell.workspace.join("orphan"),
        shell.workspace.join("aborted"),
    );
    never(
        "a descendant outlives its command",
        Duration::from_millis(2100),
        || async { Ok(orphan.exists() || aborted.exists()) },
    )
    .await?;
    ensure!(!orphan.exists(), "orphan written");
    ensure!(!aborted.exists(), "aborted written");
    Ok(())
}
