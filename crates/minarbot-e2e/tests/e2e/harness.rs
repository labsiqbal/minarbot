//! Native Codex and Claude Code harness turns: streaming segments, MCP
//! session lifetime, approvals, crashes, cancellation and resume (port of
//! `app/harness.test.ts`). Every runtime case runs once per runtime.

use minarbot_testkit::{
    fakes::{self, Fake},
    http::SseReader,
    issuer::{SessionIssuer, SESSION_TOKEN},
    prelude::*,
    Server,
};
use std::time::Duration;

const PROFILE_NAME: &str = "Coder";
const PROFILE_MODEL: &str = "saved-api-model";
const PROFILE_INSTRUCTIONS: &str = "Help with coding.";

/// One agent bound to a workspace and to native fake peers (the Node
/// `fixture(runtime, write, room, realMcp)`).
struct Harness {
    runtime: &'static str,
    root: TestRoot,
    app: Server,
    workspace: std::path::PathBuf,
    agent: String,
    path: String,
    issuer: Option<SessionIssuer>,
}

struct Options {
    write: bool,
    real_mcp: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            write: true,
            real_mcp: false,
        }
    }
}

fn profile() -> Value {
    json!({"name": PROFILE_NAME, "model": PROFILE_MODEL, "instructions": PROFILE_INSTRUCTIONS})
}

fn with(mut value: Value, extra: Value) -> Value {
    if let (Some(target), Value::Object(extra)) = (value.as_object_mut(), extra) {
        target.extend(extra);
    }
    value
}

impl Harness {
    async fn new(runtime: &'static str, options: Options) -> Result<Self> {
        let root = TestRoot::new()?;
        let workspace = root.dir("workspace")?;
        let bin = root.dir("bin")?;
        let codex = fakes::install(Fake::Codex, &bin, "fake-codex")?;
        let claude = fakes::install(Fake::Claude, &bin, "fake-claude")?;
        let agent = new_id();
        let issuer = if options.real_mcp {
            None
        } else {
            Some(SessionIssuer::start().await?)
        };
        let mut config = ServerConfig::new()
            .option("timeout", 10_000)
            .workspace(&agent, &workspace, options.write)
            .executable("codex", &codex)
            .executable("claude", &claude);
        if let Some(issuer) = &issuer {
            config = config.issuer(issuer);
        }
        let app = root.start(config).await?;
        app.post(
            "/api/agents",
            with(json!({"id": agent, "runtime": "api"}), profile()),
        )
        .await?;
        let path = format!("/api/agents/{agent}");
        app.post(
            &format!("{path}/edit"),
            with(
                json!({"id": new_id(), "revision": 1, "runtime": runtime, "confirmRuntime": true}),
                profile(),
            ),
        )
        .await?;
        Ok(Self {
            runtime,
            root,
            app,
            workspace,
            agent,
            path,
            issuer,
        })
    }

    fn turns(&self) -> String {
        format!("{}/turns", self.path)
    }

    async fn send(&self, text: &str) -> Result<String> {
        let id = new_id();
        self.app
            .post(&self.turns(), json!({"id": id, "text": text}))
            .await?;
        Ok(id)
    }

    async fn until(&self, id: &str, done: impl Fn(&Value) -> bool) -> Result<Value> {
        self.app.wait_row(&self.turns(), id, done).await
    }

    async fn settled(&self, id: &str) -> Result<Value> {
        self.until(id, |row| ["done", "failed"].contains(&status(row)))
            .await
    }

    async fn row(&self, id: &str) -> Result<Value> {
        self.app
            .row(&self.turns(), id)
            .await?
            .ok_or_else(|| format!("turn {id} missing").into())
    }

    fn log(&self) -> Result<Vec<Value>> {
        fakes::native_log(&self.workspace)
    }

    fn release(&self) -> Result<()> {
        Ok(std::fs::write(
            self.workspace.join(minarbot_testkit::fake::native::RELEASE),
            "",
        )?)
    }

    fn minted(&self) -> usize {
        self.issuer
            .as_ref()
            .map_or(0, |issuer| issuer.minted().len())
    }

    fn revoked(&self) -> usize {
        self.issuer
            .as_ref()
            .map_or(0, |issuer| issuer.revoked().len())
    }

    async fn wait_revoked(&self, count: usize) -> Result<()> {
        if let Some(issuer) = &self.issuer {
            issuer.wait_revoked(count).await?;
        }
        Ok(())
    }

    async fn decide(&self, turn: &str, card: &Value, decision: &str, status: u16) -> Result<Value> {
        let body = json!({"id": new_id(), "revision": card["revision"], "decision": decision});
        self.post_decision(turn, card, body, status).await
    }

    async fn post_decision(
        &self,
        turn: &str,
        card: &Value,
        body: Value,
        status: u16,
    ) -> Result<Value> {
        self.app
            .post_expect(
                &format!(
                    "{}/{turn}/harness-approvals/{}",
                    self.turns(),
                    card["id"].as_str().unwrap_or_default()
                ),
                body,
                status,
            )
            .await
    }

    /// Native session resumes recorded by the fake (`thread/resume` or a
    /// Claude init with `--resume`).
    fn resumes(&self) -> Result<usize> {
        Ok(self
            .log()?
            .iter()
            .filter(|row| {
                if self.runtime == "codex" {
                    row["method"] == "thread/resume"
                } else {
                    row["type"] == "init" && row["resume"] == true
                }
            })
            .count())
    }
}

fn status(row: &Value) -> &str {
    row["status"].as_str().unwrap_or_default()
}

fn approvals(row: &Value) -> usize {
    row["harnessApprovals"].as_array().map_or(0, Vec::len)
}

/// Runs a case once per native runtime as `<case>::codex` and `<case>::claude`.
macro_rules! per_runtime {
    ($($case:ident),* $(,)?) => {$(
        mod $case {
            #[tokio::test]
            async fn codex() -> minarbot_testkit::Result<()> {
                super::$case("codex").await
            }
            #[tokio::test]
            async fn claude() -> minarbot_testkit::Result<()> {
                super::$case("claude").await
            }
        }
    )*};
}

per_runtime!(
    native_check_receipt_prevents_a_second_verification_turn,
    gated_segments_replace_only_final_answer_coalesce_writes_and_restore_without_replay,
    abort_flush_preserves_final_pending_segment_and_stays_unfinished_after_restart,
    error_flush_commits_buffered_tail_without_declaring_a_final_answer,
    authoritative_snapshot_and_reply_apply_identical_session_token_redaction,
    stream_tool_activity_mcp_lifetime_follow_up_and_restart_resume,
    steer_degrades_to_linked_restart_with_partial_context_and_fresh_native_session,
    addressed_room_steer_restarts_only_selected_member_and_room_stop_fences_restart,
    durable_approval_allow_deny_prefix_grant_and_scoped_receipt,
    workspace_write_policy_outside_file_and_network_consent,
    crash_fails_once_preserves_unfinished_text_and_never_replays,
    cancel_kills_descendants_and_revokes_mcp_mid_turn_runtime_switch_refused,
    pending_approval_expires_on_shutdown_restart_no_auto_approval,
    bounded_output_fails_without_replay,
    done_check_nudges_once_and_preserves_native_session,
);

async fn native_check_receipt_prevents_a_second_verification_turn(
    runtime: &'static str,
) -> Result<()> {
    let f = Harness::new(runtime, Options::default()).await?;
    let id = f.send("Write a result NATIVE_CHECK").await?;
    let done = f.settled(&id).await?;
    ensure!(status(&done) == "done", "turn: {done}");
    let checks = done["activity"]["artifacts"].as_array().map_or(0, |rows| {
        rows.iter().filter(|row| row["kind"] == "check").count()
    });
    ensure!(
        checks == 1,
        "check artifacts: {}",
        done["activity"]["artifacts"]
    );
    let edit = if runtime == "codex" {
        "fileChange"
    } else {
        "Write"
    };
    let steps = done["activity"]["harness"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    ensure!(
        steps
            .iter()
            .any(|step| step["name"] == edit && step["state"] == "completed"),
        "harness steps: {steps:?}"
    );
    ensure!(
        !steps.iter().any(|step| step["name"] == "Verify result"),
        "second verification turn: {steps:?}"
    );
    let log = f.log()?;
    ensure!(fakes::turns(&log).len() == 1, "native turns: {log:?}");
    let contexts: Vec<String> = log
        .iter()
        .filter(|row| row.get("context").is_some_and(|context| !context.is_null()))
        .map(|row| row["context"].to_string())
        .collect();
    ensure!(!contexts.is_empty(), "no context logged");
    for context in &contexts {
        ensure!(
            context.contains(&format!("through the {runtime} runtime")),
            "runtime info: {context}"
        );
        ensure!(
            context.contains("read_routine to verify saved fields"),
            "routine guidance: {context}"
        );
    }
    Ok(())
}

async fn gated_segments_replace_only_final_answer_coalesce_writes_and_restore_without_replay(
    runtime: &'static str,
) -> Result<()> {
    let mut f = Harness::new(runtime, Options::default()).await?;
    f.app
        .edit_database(|db| {
            db.execute_batch(
                "CREATE TABLE snapshot_writes(at INTEGER);
                 CREATE TRIGGER count_snapshot_insert AFTER INSERT ON provider_streams BEGIN INSERT INTO snapshot_writes VALUES (1); END;
                 CREATE TRIGGER count_snapshot_update AFTER UPDATE ON provider_streams BEGIN INSERT INTO snapshot_writes VALUES (1); END;",
            )
        })
        .await?;
    let draft = format!("Draft {}", "x".repeat(100));
    let start = std::time::Instant::now();
    let id = f.send("STREAM_GATE").await?;
    let partial = f
        .until(&id, |row| row["partial"]["text"] == json!(draft))
        .await?;
    ensure!(
        status(&partial) == "running" && partial["partial"]["final"] == false,
        "partial: {}",
        partial["partial"]
    );
    let segments = partial["activity"]["stream"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let shape: Vec<Value> = segments
        .iter()
        .map(|item| json!([item["ordinal"], item["kind"]]))
        .collect();
    ensure!(
        shape == [json!([0, "text"]), json!([1, "tool"]), json!([2, "text"])],
        "segments: {shape:?}"
    );
    ensure!(
        segments[0]["text"] == json!(format!("Before tool. {}[REDACTED]", "🧪".repeat(100))),
        "first segment: {}",
        segments[0]["text"]
    );
    ensure!(
        !partial.to_string().contains(SESSION_TOKEN),
        "session token in partial"
    );
    ensure!(
        partial["partial"]["segment"] == segments[2]["id"],
        "partial segment"
    );
    ensure!(
        f.row(&id).await?["partial"] == partial["partial"],
        "partial not restorable"
    );
    ensure!(f.minted() == 1, "minted {}", f.minted());
    f.release()?;
    let done = f.until(&id, |row| status(row) == "done").await?;
    ensure!(
        done["reply"] == "Final answer." && done["partial"].is_null(),
        "done: {}",
        done["reply"]
    );
    let ids = |rows: &Value| -> Vec<Value> {
        rows.as_array()
            .into_iter()
            .flatten()
            .map(|item| json!([item["ordinal"], item["id"]]))
            .collect()
    };
    ensure!(
        ids(&done["activity"]["stream"]) == ids(&json!(segments)),
        "segment identity changed"
    );
    ensure!(
        done["activity"]["stream"][2]["text"] == done["reply"]
            && done["activity"]["stream"][2]["final"] == 1,
        "final segment"
    );
    let elapsed = start.elapsed().as_millis();
    let writes =
        minarbot_testkit::db::Inspector::open(f.app.database())?.count("snapshot_writes")?;
    let limit = i64::try_from(elapsed.div_ceil(40))? + 8;
    ensure!(
        writes <= limit && writes < 30,
        "token-rate snapshot writes: {writes} in {elapsed} ms"
    );
    f.app.restart().await?;
    let restored = f.row(&id).await?;
    ensure!(restored["reply"] == "Final answer.", "reply after restart");
    ensure!(
        restored["activity"]["stream"] == done["activity"]["stream"],
        "stream after restart"
    );
    ensure!(
        f.minted() == 1,
        "restart replayed the turn ({} sessions)",
        f.minted()
    );
    Ok(())
}

async fn abort_flush_preserves_final_pending_segment_and_stays_unfinished_after_restart(
    runtime: &'static str,
) -> Result<()> {
    let mut f = Harness::new(runtime, Options::default()).await?;
    let draft = format!("Draft {}", "x".repeat(100));
    let id = f.send("STREAM_GATE").await?;
    f.until(&id, |row| row["partial"]["text"] == json!(draft))
        .await?;
    f.app
        .post(
            &format!("{}/{id}/cancel", f.turns()),
            json!({"id": new_id()}),
        )
        .await?;
    f.until(&id, |row| status(row) == "cancelled").await?;
    f.wait_revoked(1).await?;
    f.app.restart().await?;
    let row = f.row(&id).await?;
    ensure!(
        row["partial"]["text"] == json!(draft) && row["partial"]["final"] == false,
        "partial: {}",
        row["partial"]
    );
    ensure!(
        row["partial"]["status"] == "failed",
        "partial status: {}",
        row["partial"]
    );
    ensure!(
        f.minted() == 1 && f.revoked() == 1,
        "sessions: {} minted, {} revoked",
        f.minted(),
        f.revoked()
    );
    Ok(())
}

async fn error_flush_commits_buffered_tail_without_declaring_a_final_answer(
    runtime: &'static str,
) -> Result<()> {
    let mut f = Harness::new(runtime, Options::default()).await?;
    let draft = format!("Draft {}", "x".repeat(100));
    let id = f.send("STREAM_CRASH").await?;
    f.until(&id, |row| row["partial"]["text"] == json!(draft))
        .await?;
    f.release()?;
    let failed = f.until(&id, |row| status(row) == "failed").await?;
    ensure!(
        failed["partial"]["text"] == json!(format!("{draft} buffered tail")),
        "partial: {}",
        failed["partial"]
    );
    ensure!(
        failed["partial"]["final"] == false && failed["partial"]["status"] == "failed",
        "partial: {}",
        failed["partial"]
    );
    let last = failed["activity"]["stream"]
        .as_array()
        .and_then(|rows| rows.last())
        .cloned()
        .unwrap_or_default();
    ensure!(last["final"] == 0, "last segment: {last}");
    f.app.restart().await?;
    ensure!(
        f.row(&id).await?["partial"] == failed["partial"],
        "partial changed on restart"
    );
    f.wait_revoked(1).await?;
    ensure!(
        f.minted() == 1 && f.revoked() == 1,
        "sessions: {} minted, {} revoked",
        f.minted(),
        f.revoked()
    );
    Ok(())
}

async fn authoritative_snapshot_and_reply_apply_identical_session_token_redaction(
    runtime: &'static str,
) -> Result<()> {
    let f = Harness::new(runtime, Options::default()).await?;
    let id = f.send("FINAL_SECRET").await?;
    let done = f.until(&id, |row| status(row) == "done").await?;
    ensure!(
        done["reply"] == "finished [REDACTED]",
        "reply: {}",
        done["reply"]
    );
    let last = done["activity"]["stream"]
        .as_array()
        .and_then(|rows| rows.last())
        .cloned()
        .unwrap_or_default();
    ensure!(
        last["text"] == done["reply"],
        "snapshot {} differs from reply",
        last["text"]
    );
    ensure!(
        !done.to_string().contains(SESSION_TOKEN),
        "session token leaked"
    );
    Ok(())
}

async fn stream_tool_activity_mcp_lifetime_follow_up_and_restart_resume(
    runtime: &'static str,
) -> Result<()> {
    let mut f = Harness::new(runtime, Options::default()).await?;
    let mut stream = SseReader::connect(&f.app, &format!("{}/stream", f.turns())).await?;
    let first = f.send("FIRST_PARTIAL_GATE").await?;
    let partial = f
        .until(&first, |row| {
            row["partial"]["text"]
                .as_str()
                .is_some_and(|text| !text.is_empty())
                || ["done", "failed", "cancelled"].contains(&status(row))
        })
        .await?;
    ensure!(status(&partial) == "running", "turn: {}", partial["error"]);
    ensure!(
        partial["partial"]["text"] == "Harness ",
        "partial: {}",
        partial["partial"]
    );
    let snapshot = stream.next_within(Duration::from_secs(10)).await?;
    ensure!(snapshot["id"] == json!(first), "first frame: {snapshot}");
    ensure!(
        snapshot["partial"]["text"] == ""
            && snapshot["partial"]["seq"] == partial["partial"]["seq"],
        "first frame: {snapshot}"
    );
    drop(stream);
    f.release()?;
    let done = f.settled(&first).await?;
    ensure!(
        status(&done) == "done" && done["reply"] == "finished",
        "first turn: {}",
        done["error"]
    );
    ensure!(
        done["activity"]["harness"][0]["state"] == "completed",
        "harness steps: {}",
        done["activity"]["harness"]
    );
    let second = f.send("SECOND").await?;
    ensure!(status(&f.settled(&second).await?) == "done", "second turn");
    f.app.restart().await?;
    let third = f.send("THIRD").await?;
    ensure!(
        status(&f.settled(&third).await?) == "done",
        "third turn after restart"
    );
    ensure!(f.resumes()? > 0, "no native resume after restart");
    f.wait_revoked(3).await?;
    let issuer = f.issuer.as_ref().ok_or("issuer")?;
    let minted: Vec<String> = issuer.minted().into_iter().map(|row| row.id).collect();
    ensure!(
        minted.len() == 3 && issuer.revoked() == minted,
        "sessions: {minted:?} vs {:?}",
        issuer.revoked()
    );
    let contexts: Vec<String> = f
        .log()?
        .iter()
        .filter_map(|row| match row.get("context") {
            Some(Value::String(text)) => Some(text.clone()),
            Some(Value::Object(entries)) => Some(
                entries
                    .values()
                    .filter_map(|entry| entry["value"].as_str())
                    .collect(),
            ),
            _ => None,
        })
        .collect();
    ensure!(contexts.len() >= 3, "contexts: {}", contexts.len());
    for context in &contexts {
        ensure!(
            context.contains("<runtime_info>") && context.contains("open_file_in_panel"),
            "context: {context}"
        );
        ensure!(
            !context.contains("computer_screenshot") && context.contains("run_shell"),
            "context tools: {context}"
        );
    }
    let usage = f.app.get("/api/usage").await?;
    ensure!(
        usage["calls"].as_array().is_some_and(|calls| calls
            .iter()
            .any(|call| call["input_tokens"] == 11 && call["output_tokens"] == 7)),
        "usage: {usage}"
    );
    Ok(())
}

async fn steer_degrades_to_linked_restart_with_partial_context_and_fresh_native_session(
    runtime: &'static str,
) -> Result<()> {
    let mut f = Harness::new(runtime, Options::default()).await?;
    let first = f.send("HOLD").await?;
    f.until(&first, |row| row["partial"]["text"] == "Harness ")
        .await?;
    let active = f.app.get(&format!("{}/dispatch", f.path)).await?;
    ensure!(
        active[0]["capabilities"]["steer"] == false,
        "dispatch: {active}"
    );
    let command = json!({
        "id": new_id(),
        "text": "RESTART_FINISH",
        "dispatchMode": "steer",
        "targetTurn": first
    });
    let receipt = f.app.post(&f.turns(), command.clone()).await?;
    ensure!(
        receipt["dispatch_mode"] == "restart"
            && receipt["dispatch_label"]
                .as_str()
                .is_some_and(|label| label.contains("cannot steer")),
        "steer receipt: {receipt}"
    );
    let command_id = command["id"].as_str().ok_or("command id")?;
    let done = f.settled(command_id).await?;
    ensure!(status(&done) == "done", "restart turn: {done}");
    ensure!(
        status(&f.row(&first).await?) == "cancelled",
        "active turn was not cancelled"
    );
    ensure!(
        done["activity"]["dispatch"][0]["source_turn"] == json!(first),
        "dispatch activity: {}",
        done["activity"]["dispatch"]
    );
    let log = f.log()?;
    let turns = fakes::turns(&log);
    let prompt = turns
        .last()
        .and_then(|row| row["prompt"].as_str())
        .ok_or("restart prompt")?;
    ensure!(
        prompt.contains(r#"\"partial\":\"Harness \""#)
            && prompt.contains("Never replay uncertain operations"),
        "restart prompt lost interrupted context"
    );
    ensure!(
        turns.len() == 2 && turns[0]["session"] != turns[1]["session"],
        "restart session"
    );
    ensure!(f.resumes()? == 0, "restart reused native session");
    let minted = f.minted();
    f.app.restart().await?;
    ensure!(
        f.app.post(&f.turns(), command.clone()).await? == receipt,
        "receipt replay"
    );
    ensure!(f.minted() == minted, "receipt replay minted a session");
    let follow = f.send("FOLLOW_RESTART_FINISH").await?;
    ensure!(status(&f.settled(&follow).await?) == "done", "follow-up");
    let turns = fakes::turns(&f.log()?);
    ensure!(turns.len() == 3, "native turns: {}", turns.len());
    ensure!(
        turns[2]["session"] != turns[1]["session"]
            && turns[2]["session"] != turns[0]["session"]
            && f.resumes()? == 0,
        "follow-up reused a restart session"
    );
    Ok(())
}

async fn addressed_room_steer_restarts_only_selected_member_and_room_stop_fences_restart(
    runtime: &'static str,
) -> Result<()> {
    let mut f = Harness::new(runtime, Options::default()).await?;
    let peer = f
        .app
        .post(
            "/api/agents",
            with(
                json!({"id": new_id(), "providerInstance": runtime}),
                profile(),
            ),
        )
        .await?;
    let peer_id = peer["id"].as_str().ok_or("peer id")?.to_owned();
    let folder = f.root.dir("home/room")?;
    let project = f
        .app
        .post(
            "/api/projects",
            json!({
                "id": new_id(),
                "name": "Harness room",
                "folders": [{"mode": "existing", "path": folder}],
                "members": [{"agent": f.agent, "write": true}, {"agent": peer_id, "write": true}],
                "confirmWrite": true
            }),
        )
        .await?;
    let channel_id = project["sections"][0]["id"].as_str().ok_or("section")?;
    let path = format!("/api/channels/{channel_id}/turns");
    let root = new_id();
    f.app
        .post(&path, json!({"id": root, "text": "APPROVE"}))
        .await?;
    let rows = f
        .app
        .wait_get(&path, |rows| {
            rows.as_array()
                .is_some_and(|rows| rows.iter().filter(|row| approvals(row) > 0).count() == 1)
        })
        .await?;
    let rows = rows.as_array().ok_or("room turns")?;
    let selected = rows
        .iter()
        .find(|row| status(row) == "running" && row["harnessApprovals"][0]["state"] == "approval")
        .ok_or("selected room member")?;
    let selected_id = selected["id"].as_str().ok_or("selected turn id")?;
    let other_id = rows
        .iter()
        .find(|row| status(row) == "queued")
        .and_then(|row| row["id"].as_str())
        .ok_or("queued room member")?;
    let command = json!({
        "id": new_id(),
        "text": "RESTART_FINISH",
        "dispatchMode": "steer",
        "targetTurn": selected["id"],
        "recipient": selected["agent"]
    });
    let receipt = f.app.post(&path, command.clone()).await?;
    ensure!(
        receipt["dispatch_mode"] == "restart",
        "room steer: {receipt}"
    );
    let command_id = command["id"].as_str().ok_or("command id")?;
    let restarted = f
        .app
        .wait_row(&path, command_id, |row| {
            row["harnessApprovals"][0]["state"] == "approval" || status(row) == "failed"
        })
        .await?;
    ensure!(
        status(&restarted) == "running",
        "restart failed: {}",
        restarted["error"]
    );
    ensure!(
        f.app
            .row(&path, selected_id)
            .await?
            .is_some_and(|row| status(&row) == "cancelled")
            && f.app
                .row(&path, other_id)
                .await?
                .is_some_and(|row| status(&row) == "queued"),
        "restart did not isolate the selected member"
    );
    let approval = &restarted["harnessApprovals"][0];
    let approval_id = approval["id"].as_str().ok_or("restart approval id")?;
    f.app
        .post(
            &format!("{path}/{command_id}/harness-approvals/{approval_id}"),
            json!({"id": new_id(), "revision": approval["revision"], "decision": "allow"}),
        )
        .await?;
    f.app.wait_status(&path, command_id, "done").await?;
    let other = f
        .app
        .wait_row(&path, other_id, |row| {
            status(row) == "running" && row["harnessApprovals"][0]["state"] == "approval"
        })
        .await?;
    let stop_path = format!("/api/channels/{channel_id}/discussions/{root}/stop");
    let stop_command = json!({"id": new_id(), "generation": root});
    let stopped = f.app.post(&stop_path, stop_command.clone()).await?;
    ensure!(
        stopped["cancelled"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id == other_id)),
        "stop missed active member: {stopped}"
    );
    let expired = f
        .app
        .wait_row(&path, other_id, |row| {
            row["harnessApprovals"][0]["state"] == "expired"
        })
        .await?;
    let card = &expired["harnessApprovals"][0];
    let other_card_id = other["harnessApprovals"][0]["id"]
        .as_str()
        .ok_or("other approval id")?;
    f.app
        .post_expect(
            &format!("{path}/{other_id}/harness-approvals/{other_card_id}"),
            json!({"id": new_id(), "revision": card["revision"], "decision": "allow"}),
            409,
        )
        .await?;
    f.app
        .post_expect(
            &path,
            json!({"id": new_id(), "text": "Restart stopped discussion", "recipient": other["agent"], "dispatchMode": "steer", "targetTurn": other_id}),
            409,
        )
        .await?;
    let minted = f.minted();
    f.app.restart().await?;
    ensure!(
        f.app.post(&stop_path, stop_command).await? == stopped,
        "stop replay"
    );
    ensure!(
        f.app.post(&path, command).await? == receipt,
        "room steer replay"
    );
    ensure!(f.minted() == minted, "room replay minted a session");
    Ok(())
}

#[tokio::test]
async fn queued_steer_promotion_restarts_the_linked_turn() -> Result<()> {
    let f = Harness::new("codex", Options::default()).await?;
    let first = f.send("HOLD").await?;
    f.until(&first, |row| row["partial"]["text"] == "Harness ")
        .await?;
    let queued = f.send("RESTART_FINISH").await?;
    let row = f.row(&queued).await?;
    ensure!(status(&row) == "queued", "queued turn: {row}");
    let path = format!("{}/{queued}/dispatch", f.turns());
    let command =
        json!({"id": new_id(), "revision": row["revision"], "mode": "steer", "targetTurn": first});
    let receipt = f.app.post(&path, command.clone()).await?;
    ensure!(
        receipt["dispatch_mode"] == "restart",
        "promotion: {receipt}"
    );
    let done = f.settled(&queued).await?;
    ensure!(status(&done) == "done", "promoted turn: {done}");
    ensure!(
        status(&f.row(&first).await?) == "cancelled",
        "source kept running"
    );
    ensure!(
        done["activity"]["dispatch"][0]["source_turn"] == first,
        "source link: {done}"
    );
    ensure!(
        f.app.post(&path, command).await? == receipt,
        "promotion replay"
    );
    ensure!(f.resumes()? == 0, "promotion reused native session");
    Ok(())
}

async fn durable_approval_allow_deny_prefix_grant_and_scoped_receipt(
    runtime: &'static str,
) -> Result<()> {
    let f = Harness::new(runtime, Options::default()).await?;
    for decision in ["deny", "allow", "always"] {
        let id = f.send("APPROVE").await?;
        let row = f
            .until(&id, |row| approvals(row) > 0 || status(row) == "failed")
            .await?;
        ensure!(status(&row) == "running", "{decision}: {}", row["error"]);
        let card = row["harnessApprovals"][0].clone();
        ensure!(card["prefix"] == "echo hello", "card: {card}");
        let body = json!({"id": new_id(), "revision": card["revision"], "decision": decision});
        f.post_decision(&id, &card, body.clone(), 200).await?;
        f.post_decision(&id, &card, body, 200).await?;
        let done = f.settled(&id).await?;
        ensure!(status(&done) == "done", "{decision}: {}", done["error"]);
        ensure!(
            done["reply"]
                == if decision == "deny" {
                    "denied"
                } else {
                    "finished"
                },
            "{decision}: {}",
            done["reply"]
        );
    }
    let fourth = f.send("APPROVE").await?;
    let fourth = f.settled(&fourth).await?;
    ensure!(
        status(&fourth) == "done" && approvals(&fourth) == 0,
        "saved grant not applied: {}",
        fourth["harnessApprovals"]
    );
    let peer = f
        .app
        .post(
            "/api/mcp-server/peer-tokens",
            json!({"id": new_id(), "name": "Peer", "scopes": ["agents:message"]}),
        )
        .await?;
    let receipt: Value = f
        .app
        .client()?
        .post(f.app.url("/mcp"))
        .bearer_auth(peer["token"].as_str().ok_or("peer token")?)
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "send_message",
            "arguments": {"requestId": new_id(), "recipient": f.agent, "text": "APPROVE peer work"}}}))
        .send()
        .await?
        .json()
        .await?;
    let delivery = &receipt["result"]["structuredContent"];
    let thread = format!(
        "/api/threads/{}/turns",
        delivery["thread"]
            .as_str()
            .ok_or_else(|| format!("delivery: {receipt}"))?
    );
    let peer_turn = until_value(
        "peer turn approval",
        || f.app.get(&thread),
        |rows| approvals(&rows[0]) > 0 || ["done", "failed"].contains(&status(&rows[0])),
    )
    .await?[0]
        .clone();
    ensure!(
        approvals(&peer_turn) > 0,
        "peer bypassed the saved command approval: {peer_turn}"
    );
    ensure!(
        peer_turn["harnessApprovals"][0]["state"] == "approval",
        "peer card: {}",
        peer_turn["harnessApprovals"]
    );
    let card = peer_turn["harnessApprovals"][0].clone();
    f.app
        .post(
            &format!(
                "{thread}/{}/harness-approvals/{}",
                delivery["turn"].as_str().ok_or("turn")?,
                card["id"].as_str().ok_or("card")?
            ),
            json!({"id": new_id(), "revision": card["revision"], "decision": "deny"}),
        )
        .await?;
    let denied = f
        .app
        .wait_get(&thread, |rows| status(&rows[0]) == "done")
        .await?;
    ensure!(
        denied[0]["reply"] == "denied",
        "peer reply: {}",
        denied[0]["reply"]
    );
    let secret = f.send("APPROVE_SECRET").await?;
    let row = f.until(&secret, |row| approvals(row) > 0).await?;
    let card = row["harnessApprovals"][0].clone();
    ensure!(
        card["prefix"].is_null() && !card.to_string().contains(SESSION_TOKEN),
        "secret card: {card}"
    );
    ensure!(
        card["command"]
            .as_str()
            .is_some_and(|command| command.contains("[redacted]")),
        "secret command: {card}"
    );
    f.decide(&secret, &card, "always", 400).await?;
    f.decide(&secret, &card, "deny", 200).await?;
    ensure!(
        f.settled(&secret).await?["reply"] == "denied",
        "secret approval reply"
    );
    f.wait_revoked(6).await?;
    ensure!(
        f.minted() == 6 && f.revoked() == 6,
        "sessions: {} minted, {} revoked",
        f.minted(),
        f.revoked()
    );
    Ok(())
}

async fn workspace_write_policy_outside_file_and_network_consent(
    runtime: &'static str,
) -> Result<()> {
    {
        let f = Harness::new(runtime, Options::default()).await?;
        let inside = f.send("WORKSPACE_WRITE").await?;
        let done = f.settled(&inside).await?;
        ensure!(
            status(&done) == "done" && approvals(&done) == 0,
            "workspace write: {}",
            done["harnessApprovals"]
        );
        for (text, kind) in [("OUTSIDE_FILE", "file"), ("NETWORK", "network")] {
            let id = f.send(text).await?;
            let row = f
                .until(&id, |row| approvals(row) > 0 || status(row) == "failed")
                .await?;
            ensure!(status(&row) == "running", "{text}: {}", row["error"]);
            let card = row["harnessApprovals"][0].clone();
            ensure!(
                card["kind"] == kind && card["prefix"].is_null(),
                "{text} card: {card}"
            );
            f.decide(&id, &card, "always", 400).await?;
            f.decide(&id, &card, "deny", 200).await?;
            ensure!(f.settled(&id).await?["reply"] == "denied", "{text} reply");
        }
    }
    let read = Harness::new(
        runtime,
        Options {
            write: false,
            ..Options::default()
        },
    )
    .await?;
    let id = read.send("READ_ONLY_WRITE").await?;
    let row = read
        .until(&id, |row| approvals(row) > 0 || status(row) == "failed")
        .await?;
    ensure!(
        status(&row) == "running",
        "read-only write: {}",
        row["error"]
    );
    let card = row["harnessApprovals"][0].clone();
    ensure!(card["kind"] == "file", "read-only card: {card}");
    read.decide(&id, &card, "allow", 200).await?;
    ensure!(
        status(&read.settled(&id).await?) == "done",
        "allowed read-only write"
    );
    Ok(())
}

async fn crash_fails_once_preserves_unfinished_text_and_never_replays(
    runtime: &'static str,
) -> Result<()> {
    let mut f = Harness::new(runtime, Options::default()).await?;
    let id = f.send("CRASH").await?;
    let row = f.until(&id, |row| status(row) == "failed").await?;
    ensure!(
        row["error"]
            .as_str()
            .is_some_and(|error| error.contains("stopped before completion")
                || error.contains("could not start")),
        "crash error: {}",
        row["error"]
    );
    ensure!(
        row["partial"]["text"] == "Harness ",
        "partial: {}",
        row["partial"]
    );
    f.app.restart().await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    f.wait_revoked(1).await?;
    ensure!(
        f.minted() == 1 && f.revoked() == 1,
        "sessions: {} minted, {} revoked",
        f.minted(),
        f.revoked()
    );
    Ok(())
}

async fn cancel_kills_descendants_and_revokes_mcp_mid_turn_runtime_switch_refused(
    runtime: &'static str,
) -> Result<()> {
    let f = Harness::new(runtime, Options::default()).await?;
    let id = f.send("HOLD").await?;
    f.until(&id, |row| {
        row["partial"]["text"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
    })
    .await?;
    f.app
        .post_expect(
            &format!("{}/edit", f.path),
            with(
                json!({"id": new_id(), "revision": 2, "runtime": "api", "confirmRuntime": true}),
                profile(),
            ),
            409,
        )
        .await?;
    f.app
        .post(
            &format!("{}/{id}/cancel", f.turns()),
            json!({"id": new_id()}),
        )
        .await?;
    f.until(&id, |row| status(row) == "cancelled").await?;
    f.wait_revoked(1).await?;
    ensure!(f.revoked() == 1, "revoked {}", f.revoked());
    let log = f.log()?;
    let pid = log
        .iter()
        .find(|row| row["type"] == "descendant")
        .and_then(|row| row["pid"].as_u64())
        .ok_or("no descendant logged")?;
    until("descendant exits", || async {
        let state = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        Ok((state.is_empty() || state.contains(") Z ")).then_some(()))
    })
    .await
    .map_err(|_| format!("harness descendant {pid} survived cancel"))?;
    Ok(())
}

async fn pending_approval_expires_on_shutdown_restart_no_auto_approval(
    runtime: &'static str,
) -> Result<()> {
    let mut f = Harness::new(runtime, Options::default()).await?;
    let id = f.send("APPROVE").await?;
    let row = f.until(&id, |row| approvals(row) > 0).await?;
    let card = row["harnessApprovals"][0].clone();
    f.app.restart().await?;
    let expired = f.row(&id).await?;
    ensure!(
        expired["harnessApprovals"][0]["state"] == "expired",
        "card after restart: {}",
        expired["harnessApprovals"]
    );
    f.wait_revoked(1).await?;
    ensure!(f.revoked() == 1, "revoked {}", f.revoked());
    f.decide(&id, &card, "allow", 409).await?;
    ensure!(f.minted() == 1, "expired approval restarted the turn");
    Ok(())
}

async fn bounded_output_fails_without_replay(runtime: &'static str) -> Result<()> {
    let f = Harness::new(runtime, Options::default()).await?;
    let id = f.send("OVERSIZE").await?;
    let row = f.until(&id, |row| status(row) == "failed").await?;
    ensure!(
        row["error"]
            .as_str()
            .is_some_and(|error| error.contains("output line exceeded")),
        "error: {}",
        row["error"]
    );
    f.wait_revoked(1).await?;
    ensure!(
        f.minted() == 1 && f.revoked() == 1,
        "sessions: {} minted, {} revoked",
        f.minted(),
        f.revoked()
    );
    Ok(())
}

async fn done_check_nudges_once_and_preserves_native_session(runtime: &'static str) -> Result<()> {
    let f = Harness::new(runtime, Options::default()).await?;
    let id = f.send("Create a deliverable.").await?;
    f.until(&id, |row| status(row) == "done").await?;
    let log = f.log()?;
    let turns = fakes::turns(&log);
    ensure!(turns.len() == 2, "native turns: {turns:?}");
    ensure!(
        turns[1]["prompt"]
            .as_str()
            .is_some_and(|prompt| prompt.contains("One verification reminder")),
        "reminder: {}",
        turns[1]["prompt"]
    );
    ensure!(
        turns[0]["session"] == turns[1]["session"],
        "reminder used a new session"
    );
    if runtime == "claude" {
        let hook = log
            .iter()
            .find(|row| row["type"] == "stop-hook")
            .ok_or("no stop hook")?;
        ensure!(hook["decision"] == "block", "stop hook: {hook}");
    } else {
        let usage = f.app.get("/api/usage").await?;
        ensure!(
            usage["calls"].as_array().is_some_and(|calls| calls
                .iter()
                .any(|call| call["input_tokens"] == 22 && call["output_tokens"] == 14)),
            "usage: {usage}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn runtime_changes_require_confirmation_and_revision_missing_workspaces_are_provisioned(
) -> Result<()> {
    // #87 removed runtime switching: provider-instance changes need no
    // `confirmRuntime` and agents no longer report a `runtime`, so the Node
    // steps for the unconfirmed switch and `runtime == "api"` are dropped.
    let mut f = Harness::new("codex", Options::default()).await?;
    let edit = format!("{}/edit", f.path);
    f.app
        .post_expect(
            &edit,
            with(
                json!({"id": new_id(), "revision": 1, "runtime": "api", "confirmRuntime": true}),
                profile(),
            ),
            409,
        )
        .await?;
    let command = with(
        json!({"id": new_id(), "revision": 2, "runtime": "api", "confirmRuntime": true}),
        profile(),
    );
    f.app.post(&edit, command.clone()).await?;
    f.app.post(&edit, command).await?;
    let directory = f.app.get("/api/agents").await?;
    ensure!(
        directory["agents"][0]["revision"] == 3,
        "agent: {}",
        directory["agents"][0]
    );
    let other = f
        .app
        .post("/api/agents", with(json!({"id": new_id()}), profile()))
        .await?;
    let other_id = other["id"].as_str().ok_or("id")?;
    let choices = f
        .app
        .get(&format!("/api/agents/{other_id}/runtimes"))
        .await?;
    ensure!(
        choices["choices"]
            .as_array()
            .is_some_and(|rows| rows.iter().all(|row| row["enabled"] == row["available"])),
        "runtime choices: {choices}"
    );
    f.app
        .post(
            &format!("/api/agents/{other_id}/edit"),
            with(
                json!({"id": new_id(), "revision": 1, "runtime": "codex", "confirmRuntime": true}),
                profile(),
            ),
        )
        .await?;
    std::fs::remove_file(f.root.path().join("bin/fake-codex"))?;
    f.app.restart().await?;
    let runtimes = f.app.get("/api/runtimes").await?;
    let missing = runtimes["choices"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["runtime"] == "codex"))
        .ok_or_else(|| format!("runtimes: {runtimes}"))?;
    ensure!(
        missing["available"] == false && missing["version"].is_null(),
        "missing codex: {missing}"
    );
    Ok(())
}

#[tokio::test]
async fn claude_workspace_shell_inspection_and_checks_finish_without_approval() -> Result<()> {
    let f = Harness::new(
        "claude",
        Options {
            real_mcp: true,
            ..Options::default()
        },
    )
    .await?;
    let host_file = f.root.path().join("host-only.txt");
    std::fs::write(&host_file, "private fixture")?;
    let port = f.app.origin().rsplit(':').next().ok_or("port")?.to_owned();
    std::fs::write(
        f.workspace.join("fixture.test.sh"),
        format!(
            "set -e\n\
             [ \"$PWD\" = /workspace ] || {{ echo \"cwd $PWD\"; exit 1; }}\n\
             [ -z \"${{MINARBOT_MCP_TOKEN+x}}\" ] || {{ echo 'MCP token leaked'; exit 1; }}\n\
             [ ! -e '{}' ] || {{ echo 'host file visible'; exit 1; }}\n\
             command -v bash >/dev/null || {{ echo 'bash missing'; exit 1; }}\n\
             if bash -c 'exec 3<>/dev/tcp/127.0.0.1/{port}' 2>/dev/null; then echo 'network reachable'; exit 1; fi\n",
            host_file.display()
        ),
    )?;
    let id = f.send("WORKSPACE_SHELL_FIXTURE").await?;
    let row = f
        .until(&id, |row| {
            approvals(row) > 0 || ["done", "failed"].contains(&status(row))
        })
        .await?;
    ensure!(
        approvals(&row) == 0,
        "workspace shell requested approval: {row}"
    );
    let row = f.settled(&id).await?;
    ensure!(
        status(&row) == "done" && row["reply"] == "finished",
        "turn: {} {}",
        row["status"],
        row["error"]
    );
    ensure!(
        std::fs::read_to_string(f.workspace.join("result.txt"))? == "checked",
        "check did not run"
    );
    Ok(())
}
