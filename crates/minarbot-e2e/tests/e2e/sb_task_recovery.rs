//! Canonical task recovery, controls, tools and context (port of
//! `app/task-recovery.test.ts`). The fixture is `task_bridge::TaskFixture`.

use minarbot_testkit::{
    prelude::*,
    task_bridge::{Envelope, TaskFixture},
};

fn agent_body(id: &str, name: &str, f: &TaskFixture) -> Value {
    json!({"requestId": id, "id": id, "name": name, "model": "fixture", "backupModel": "", "instructions": "Continue",
        "threadLimit": 2, "projectId": f.project, "homeDeviceId": f.home, "expectedProjectRevision": 1,
        "expectedHomeDeviceRevision": 1, "providerBinding": null})
}

fn data(reply: &Envelope, field: &str) -> Value {
    reply.data()[field].clone()
}

#[tokio::test]
async fn reassign_creates_one_immutable_successor_keeps_original_receipt_and_fences_source_controls(
) -> Result<()> {
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    let recipient = new_id();
    f.human(
        minarbot_testkit::http::Method::POST,
        "/api/agents",
        Some(agent_body(&recipient, "Recipient", &f)),
        &f.owner,
    )
    .await?
    .expect_ok()?;
    let command = f.command(json!({}));
    let accepted = f.admit(&f.owner, &command).await?.expect_ok()?;
    let task = command["requestId"].as_str().ok_or("task")?.to_owned();
    let mut reassign = json!({"requestId": new_id(), "expectedRevision": 1, "operation": "reassign", "recipientAgentId": recipient});
    f.control(&f.owner, &task, reassign.clone())
        .await?
        .expect_code("task_not_paused")?;
    let paused = f
        .control(
            &f.owner,
            &task,
            json!({"requestId": new_id(), "expectedRevision": 1, "operation": "pause"}),
        )
        .await?
        .expect_ok()?;
    reassign["expectedRevision"] = paused.receipt()["recordRevision"].clone();
    let transferred = f
        .control(&f.owner, &task, reassign.clone())
        .await?
        .expect_ok()?;
    let successor = data(&transferred, "taskId")
        .as_str()
        .ok_or("successor")?
        .to_owned();
    ensure!(successor != task, "successor reuses the task id");
    ensure!(
        data(&transferred, "logicalTurnId") == task.as_str(),
        "logical turn: {}",
        transferred.body
    );
    ensure!(
        data(&transferred, "previousTaskId") == task.as_str(),
        "previous task: {}",
        transferred.body
    );
    ensure!(
        data(&transferred, "homeDeviceId") == f.home.as_str(),
        "home: {}",
        transferred.body
    );
    ensure!(
        f.control(&f.owner, &task, reassign.clone())
            .await?
            .expect_ok()?
            .data()
            == transferred.data(),
        "reassign receipt"
    );
    let original = f.lookup(&f.owner, &task).await?.expect_ok()?;
    let next = f.lookup(&f.owner, &successor).await?.expect_ok()?;
    ensure!(
        data(&original, "currentTaskId") == successor.as_str(),
        "current task: {}",
        original.body
    );
    ensure!(
        data(&original, "state") == "transferred",
        "state: {}",
        original.body
    );
    ensure!(
        data(&original, "agentId") == f.agent.as_str(),
        "original agent: {}",
        original.body
    );
    ensure!(
        original.receipt() == accepted.receipt(),
        "original receipt changed"
    );
    ensure!(
        data(&next, "agentId") == recipient.as_str(),
        "successor agent: {}",
        next.body
    );
    ensure!(
        data(&next, "state") == "waiting_device",
        "successor state: {}",
        next.body
    );
    ensure!(
        data(&next, "logicalTurnId") == task.as_str(),
        "successor logical turn: {}",
        next.body
    );
    let mut again = reassign.clone();
    again["requestId"] = json!(new_id());
    f.control(&f.owner, &task, again)
        .await?
        .expect_code("task_transferred")?;
    f.control(
        &f.owner,
        &successor,
        json!({"requestId": new_id(), "expectedRevision": 1, "operation": "pause", "recipientAgentId": recipient}),
    )
    .await?
    .expect_code("recipient_forbidden")?;
    f.control(
        &f.foreign,
        &successor,
        json!({"requestId": new_id(), "expectedRevision": 1, "operation": "pause"}),
    )
    .await?
    .expect_code("resource_not_found")?;
    let immutable = f
        .db()
        .execute("UPDATE tasks SET agent=? WHERE id=?", [&recipient, &task]);
    ensure!(
        immutable
            .as_ref()
            .is_err_and(|error| error.to_string().contains("immutable task admission")),
        "task admission mutable: {immutable:?}"
    );
    ensure!(
        data(&f.admit(&f.owner, &command).await?.expect_ok()?, "taskId") == task.as_str(),
        "admission receipt"
    );
    Ok(())
}

use crate::sb_task_support::{field, routine_task, stage, start, task_revision};
use minarbot_testkit::protocol::sha256_hex;

#[tokio::test]
async fn canonical_channel_context_names_current_visible_members_and_lead_across_edits(
) -> Result<()> {
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    let peer = new_id();
    let channel = new_id();
    let mut body = agent_body(&peer, "Right Hand", &f);
    body["role"] = json!("Personal agent");
    body["instructions"] = json!("Help.");
    f.human(
        minarbot_testkit::http::Method::POST,
        "/api/agents",
        Some(body),
        &f.owner,
    )
    .await?
    .expect_ok()?;
    f.db().execute(
        "INSERT INTO channels(id,name,instructions,lead) VALUES (?,'Owner team','Discuss channel work.',?)",
        [&channel, &peer],
    )?;
    for agent in [&f.agent, &peer] {
        f.db().execute(
            "INSERT INTO channel_members VALUES (?,?)",
            [&channel, agent],
        )?;
    }
    f.db().execute(
        "INSERT INTO record_scopes VALUES ('channels',?,?,?,'project','channel',?)",
        [
            json!([channel]).to_string(),
            f.owner.account_id.clone(),
            f.project.clone(),
            channel.clone(),
        ],
    )?;
    let snapshot = |routing: &'static str| {
        let f = &f;
        let peer = peer.clone();
        let channel = channel.clone();
        async move {
            let agent = if routing == "automatic" {
                peer
            } else {
                f.agent.clone()
            };
            let revision = field(
                f,
                "SELECT revision FROM channels WHERE id=?",
                [&channel],
                "revision",
            )?;
            let command = f.command(json!({"agentId": agent,
                "payload": {"v": 1, "kind": "channel", "channelId": channel, "routing": routing,
                    "message": {"text": "Describe this channel.", "files": []}},
                "expectedRevisions": {"project": 1, "agent": 1, "homeBinding": 1, "membership": 1, "assignment": 1,
                    "capability": 1, "channel": revision}}));
            f.admit(&f.owner, &command).await?.expect_ok()?;
            let task = command["requestId"].as_str().ok_or("task")?.to_owned();
            let (token, ownership, revision) = f.acquire_task(&task).await?;
            let context = f
                .task_op("context", &task, json!({"requestId": new_id(), "ownership": ownership, "expectedTaskRevision": revision}), &token)
                .await?
                .expect_ok()?;
            Ok::<_, minarbot_testkit::Error>(
                context.data()["snapshot"]["conversation"]["channel"].clone(),
            )
        }
    };
    let expected = json!({"name": "Owner team", "instructions": "Discuss channel work.",
        "members": ["Agent", "Right Hand - Personal agent"], "lead": "Right Hand - Personal agent"});
    ensure!(
        snapshot("automatic").await? == expected,
        "automatic routing context"
    );
    ensure!(snapshot("assigned").await? == expected, "assigned context");
    f.db().execute(
        "UPDATE channels SET lead=?,revision=revision+1 WHERE id=?",
        [&f.agent, &channel],
    )?;
    let mut lead = expected.clone();
    lead["lead"] = json!("Agent");
    ensure!(snapshot("assigned").await? == lead, "lead change");
    f.db().execute(
        "UPDATE channels SET lead=?,revision=revision+1 WHERE id=?",
        [&peer, &channel],
    )?;
    f.db().execute_batch("CREATE TABLE IF NOT EXISTS agent_visibility(agent TEXT PRIMARY KEY,hidden INTEGER,revision INTEGER)")?;
    f.db()
        .execute("INSERT INTO agent_visibility VALUES (?,1,1)", [&peer])?;
    let mut hidden = expected.clone();
    hidden["members"] = json!(["Agent"]);
    hidden["lead"] = json!("No lead");
    ensure!(snapshot("assigned").await? == hidden, "hidden lead");
    f.db().execute(
        "DELETE FROM channel_members WHERE channel=? AND agent=?",
        [&channel, &peer],
    )?;
    f.db().execute(
        "UPDATE channels SET lead=NULL,revision=revision+1 WHERE id=?",
        [&channel],
    )?;
    ensure!(snapshot("assigned").await? == hidden, "removed member");
    Ok(())
}

async fn outcome_state(f: &TaskFixture, task: &str) -> Result<(Value, Value, Value)> {
    Ok((
        field(f, "SELECT state FROM tasks WHERE id=?", [task], "state")?,
        field(
            f,
            "SELECT no_update FROM turns WHERE id=?",
            [task],
            "no_update",
        )?,
        field(
            f,
            "SELECT stopped FROM execution_owners WHERE task=?",
            [task],
            "stopped",
        )?,
    ))
}

#[tokio::test]
async fn canonical_routine_outcomes_validate_reasons_and_atomically_retain_result_receipt_and_task_state(
) -> Result<()> {
    for name in ["no_update", "report_failure"] {
        let f = TaskFixture::new(&["no_update", "report_failure"], None).await?;
        let (mut task, occurrence) = routine_task(&f).await?;
        for reason in [
            json!({}),
            json!({"reason": null}),
            json!({"reason": 0}),
            json!({"reason": ""}),
            json!({"reason": "  "}),
            json!({"reason": "x".repeat(2001)}),
            json!({"reason": "null\u{0}byte"}),
        ] {
            let result = stage(&f, &mut task, &reason, name)
                .await?
                .run(&f)
                .await?
                .expect_ok()?;
            ensure!(
                result.data()["resultContent"]["status"] == 400,
                "{name} {reason}: {}",
                result.body
            );
            ensure!(
                outcome_state(&f, &task.id).await? == (json!("running"), json!(0), json!(0)),
                "{name} {reason} changed state"
            );
        }
        f.db().execute_batch(
            "CREATE TABLE IF NOT EXISTS files(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE,scope TEXT,name TEXT,digest TEXT,bytes BLOB,created TEXT);
             CREATE TABLE IF NOT EXISTS generated_files(seq INTEGER PRIMARY KEY AUTOINCREMENT,turn TEXT,file TEXT,call_id TEXT,call TEXT,after_action INTEGER);",
        )?;
        let file = new_id();
        let bytes = b"Evidence collected before terminal outcome".to_vec();
        f.db().execute(
            "INSERT INTO files(id,scope,name,digest,bytes,created) VALUES (?,'fixture','partial.txt',?,?,'fixture')",
            (file.clone(), sha256_hex(&bytes), bytes.clone()),
        )?;
        f.db().execute(
            "INSERT INTO generated_files(turn,file,call_id,call,after_action) VALUES (?,?,'partial',?,0)",
            [task.id.clone(), file.clone(), json!({"function": {"name": "create_text_file"}}).to_string()],
        )?;
        f.db().execute(
            "INSERT INTO record_scopes VALUES ('files',?,?,?,'owner','agent',?)",
            [
                json!([file]).to_string(),
                f.member.account_id.clone(),
                f.project.clone(),
                f.agent.clone(),
            ],
        )?;
        let reason = "r".repeat(2000);
        let command = stage(&f, &mut task, &json!({"reason": reason}), name).await?;
        let before = field(
            &f,
            "SELECT revision FROM tasks WHERE id=?",
            [&task.id],
            "revision",
        )?;
        f.db().execute_batch("CREATE TRIGGER fail_outcome_receipt BEFORE INSERT ON receipts WHEN NEW.scope LIKE 'tasks:context-mutations:%' BEGIN SELECT RAISE(ABORT,'outcome receipt failed'); END")?;
        command.run(&f).await?.expect_code("storage_failed")?;
        ensure!(
            outcome_state(&f, &task.id).await? == (json!("running"), json!(0), json!(0)),
            "{name}: failed receipt changed state"
        );
        ensure!(
            field(
                &f,
                "SELECT revision FROM tasks WHERE id=?",
                [&task.id],
                "revision"
            )? == before,
            "{name}: revision moved"
        );
        ensure!(
            field(
                &f,
                "SELECT 1 AS x FROM task_results WHERE action=?",
                [command.action()],
                "x"
            )?
            .is_null(),
            "{name}: result kept"
        );
        ensure!(
            field(
                &f,
                "SELECT 1 AS x FROM task_reports WHERE action=?",
                [command.action()],
                "x"
            )?
            .is_null(),
            "{name}: report kept"
        );
        f.db().execute_batch("DROP TRIGGER fail_outcome_receipt")?;
        let saved = command.run(&f).await?.expect_ok()?;
        ensure!(
            saved.data()["resultContent"]["reason"] == reason.as_str(),
            "{name}: saved reason"
        );
        ensure!(
            f.db().blob("SELECT bytes FROM files WHERE id=?", [&file])? == bytes,
            "{name}: evidence bytes"
        );
        ensure!(
            field(
                &f,
                "SELECT file FROM generated_files WHERE turn=?",
                [&task.id],
                "file"
            )? == file.as_str(),
            "{name}: evidence file"
        );
        ensure!(
            command.run(&f).await?.body == saved.body,
            "{name}: exact retry"
        );
        let (step, state) = if name == "no_update" {
            ("skipped", "done")
        } else {
            ("failed", "failed")
        };
        f.db().execute(
            "UPDATE routine_steps SET state=? WHERE occurrence=?",
            [step, occurrence.as_str()],
        )?;
        f.db().execute(
            "UPDATE routine_occurrences SET state=? WHERE id=?",
            [state, occurrence.as_str()],
        )?;
        ensure!(
            command.run(&f).await?.body == saved.body,
            "{name}: settlement blocked recovery"
        );
        let execution = f
            .task_op(
                "execution",
                &task.id,
                json!({"requestId": new_id(), "ownership": task.ownership}),
                &task.token,
            )
            .await?
            .expect_ok()?;
        let terminal = if name == "no_update" {
            "completed"
        } else {
            "failed"
        };
        ensure!(
            execution.data()["state"] == terminal,
            "{name}: execution {}",
            execution.body
        );
        f.db().execute(
            "UPDATE collaborator_agents SET enabled=0,revision=revision+1 WHERE project=? AND account=? AND agent=?",
            [&f.project, &f.member.account_id, &f.agent],
        )?;
        command.run(&f).await?.expect_code("execution_denied")?;
    }
    Ok(())
}

#[tokio::test]
async fn canonical_routine_outcomes_deny_forced_ordinary_chat_and_descendant_calls() -> Result<()> {
    for name in ["no_update", "report_failure"] {
        let f = TaskFixture::new(&["no_update", "report_failure"], None).await?;
        let (task, _) = routine_task(&f).await?;
        let direct = start(&f, &f.owner, None).await?;
        let descendant = start(&f, &f.member, None).await?;
        f.db().execute(
            "UPDATE turns SET parent=? WHERE id=?",
            [&task.id, &descendant.id],
        )?;
        let side = new_id();
        let channel = new_id();
        f.db().execute(
            "INSERT INTO threads(id,agent,name) VALUES (?,?,'Side')",
            [&side, &f.agent],
        )?;
        f.db().execute(
            "INSERT INTO record_scopes VALUES ('threads',?,?,?,'owner','thread',?)",
            [
                json!([side]).to_string(),
                f.owner.account_id.clone(),
                f.project.clone(),
                side.clone(),
            ],
        )?;
        f.db().execute(
            "INSERT INTO channels(id,name,instructions) VALUES (?,'Room','Help')",
            [&channel],
        )?;
        f.db().execute(
            "INSERT INTO channel_members VALUES (?,?)",
            [&channel, &f.agent],
        )?;
        f.db().execute(
            "INSERT INTO record_scopes VALUES ('channels',?,?,?,'project','channel',?)",
            [
                json!([channel]).to_string(),
                f.owner.account_id.clone(),
                f.project.clone(),
                channel.clone(),
            ],
        )?;
        let side_task = start(&f, &f.owner, Some(json!({"v": 1, "kind": "side", "threadId": side, "message": {"text": "Ordinary side chat", "files": []}}))).await?;
        let channel_task = start(&f, &f.owner, Some(json!({"v": 1, "kind": "channel", "channelId": channel, "routing": "assigned", "message": {"text": "Ordinary channel", "files": []}}))).await?;
        for mut task in [direct, descendant, side_task, channel_task] {
            let revision = task_revision(&f, &task).await?;
            let context = f
                .task_op("context", &task.id, json!({"requestId": new_id(), "ownership": task.ownership, "expectedTaskRevision": revision}), &task.token)
                .await?
                .expect_ok()?;
            let offered = context.data()["snapshot"]["tools"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            ensure!(
                !offered.iter().any(|tool| ["no_update", "report_failure"]
                    .contains(&tool["function"]["name"].as_str().unwrap_or_default())),
                "{name}: outcome tools offered to an ordinary task"
            );
            let command = stage(
                &f,
                &mut task,
                &json!({"reason": "Forced terminal call"}),
                name,
            )
            .await?;
            command.run(&f).await?.expect_code("execution_denied")?;
            ensure!(
                field(
                    &f,
                    "SELECT state FROM tasks WHERE id=?",
                    [&task.id],
                    "state"
                )? == "running",
                "{name}: state changed"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn canonical_routine_outcomes_reject_late_results_after_pause_cancel_revoke_or_context_change(
) -> Result<()> {
    for change in [
        "pause",
        "cancel",
        "revoke",
        "capability",
        "scope",
        "rules",
        "setup",
        "unknown-field",
    ] {
        let f = TaskFixture::new(&["no_update", "report_failure"], None).await?;
        let (mut task, _) = routine_task(&f).await?;
        let args = if change == "unknown-field" {
            json!({"reason": "No changes", "taskId": task.id})
        } else {
            json!({"reason": "No changes"})
        };
        let command = stage(&f, &mut task, &args, "no_update").await?;
        match change {
            "pause" | "cancel" => {
                f.control(&task.actor, &task.id, json!({"requestId": new_id(), "expectedRevision": command.request["expectedRevision"], "operation": change}))
                    .await?;
            }
            "revoke" => {
                f.db().execute(
                    "UPDATE collaborator_agents SET enabled=0,revision=revision+1 WHERE project=? AND account=? AND agent=?",
                    [&f.project, &f.member.account_id, &f.agent],
                )?;
            }
            "scope" => {
                f.db().execute(
                    "DELETE FROM record_scopes WHERE table_name='routine_occurrences'",
                    [],
                )?;
            }
            "capability" => {
                f.db().execute(
                    "UPDATE agent_projects SET capabilities_json=?,revision=revision+1 WHERE project=? AND agent=?",
                    [json!({"v": 1, "tools": [], "resources": []}).to_string(), f.project.clone(), f.agent.clone()],
                )?;
            }
            "rules" => {
                f.db().execute("UPDATE agent_projects SET rules='Changed',revision=revision+1 WHERE project=? AND agent=?", [&f.project, &f.agent])?;
            }
            "setup" => {
                f.db()
                    .execute("UPDATE turns SET profile_setup=1 WHERE id=?", [&task.id])?;
            }
            _ => {}
        }
        let refused = command.run(&f).await?;
        ensure!(
            !refused.ok()
                && [
                    "execution_denied",
                    "task_not_runnable",
                    "stale_revision",
                    "context_stale",
                    "unknown_field",
                    "ownership_conflict"
                ]
                .contains(&refused.code().unwrap_or_default()),
            "{change}: {}",
            refused.body
        );
        ensure!(
            field(
                &f,
                "SELECT no_update FROM turns WHERE id=?",
                [&task.id],
                "no_update"
            )? == 0,
            "{change}: no_update set"
        );
        ensure!(
            field(
                &f,
                "SELECT 1 AS x FROM task_results WHERE action=?",
                [command.action()],
                "x"
            )?
            .is_null(),
            "{change}: result saved"
        );
        ensure!(
            field(
                &f,
                "SELECT state FROM tasks WHERE id=?",
                [&task.id],
                "state"
            )? != "completed",
            "{change}: completed"
        );
    }
    Ok(())
}

use crate::sb_task_support::{canonical_snapshot, one, ready_result, stored_provision};

fn count(f: &TaskFixture, sql: &str) -> Result<Value> {
    field(f, sql, [], "n")
}

#[tokio::test]
async fn canonical_turn_wait_reserves_one_deadline_and_activates_only_after_matching_home_secret_result(
) -> Result<()> {
    let f = TaskFixture::new(&["wait_for_event"], None).await?;
    let mut task = start(&f, &f.owner, None).await?;
    let rpc = stage(
        &f,
        &mut task,
        &json!({"event": "ready", "timeoutSeconds": 60}),
        "wait_for_event",
    )
    .await?;
    rpc.run(&f).await?.expect_code("device_offline")?;
    let row = one(&f, "SELECT * FROM wait_provisions", [])?;
    ensure!(row["state"] == "pending", "provision: {row}");
    let provision = stored_provision(&row["request_json"])?;
    ensure!(
        provision["provisionId"] == rpc.request["requestId"],
        "provision id: {provision}"
    );
    ensure!(provision["target"]["kind"] == "turn", "target: {provision}");
    ensure!(
        count(&f, "SELECT count(*) n FROM turn_waits")? == 0,
        "wait activated early"
    );
    rpc.run(&f).await?.expect_code("device_offline")?;
    ensure!(
        field(&f, "SELECT deadline FROM wait_provisions", [], "deadline")? == row["deadline"],
        "deadline moved"
    );
    let result = ready_result(&provision);
    let mut mismatched = result.clone();
    mismatched["secretRef"] = json!("mismatched reference");
    f.wait_secret_result(mismatched)
        .await?
        .expect_code("wait_invalid")?;
    ensure!(
        count(&f, "SELECT count(*) n FROM turn_waits")? == 0,
        "mismatched result activated"
    );
    let ack = f.wait_secret_result(result.clone()).await?.expect_ok()?;
    ensure!(ack.data()["state"] == "activated", "ack: {}", ack.body);
    ensure!(
        count(&f, "SELECT count(*) n FROM turn_waits")? == 1,
        "wait not activated"
    );
    let original = rpc.run(&f).await?.expect_ok()?;
    ensure!(
        original.data()["resultContent"]["status"] == "waiting",
        "wait result: {}",
        original.body
    );
    ensure!(
        original.data()["resultContent"]["id"] == provision["waitKey"],
        "wait id: {}",
        original.body
    );
    ensure!(
        f.wait_secret_result(result.clone()).await?.body == ack.body,
        "ack receipt"
    );
    let mut failed = result.clone();
    failed["state"] = json!("failed");
    if let Some(fields) = failed.as_object_mut() {
        fields.remove("secretRef");
    }
    failed["errorCode"] = json!("wait_expired");
    f.wait_secret_result(failed)
        .await?
        .expect_code("payload_conflict")?;
    f.task_op(
        "context",
        &task.id,
        json!({"requestId": new_id(), "ownership": task.ownership, "expectedTaskRevision": original.receipt()["recordRevision"]}),
        &task.token,
    )
    .await?
    .expect_code("event_pending")?;
    let columns = f.db().rows("PRAGMA table_info(turn_waits)", [])?;
    ensure!(
        !columns.iter().any(|column| column["name"] == "secret"),
        "turn_waits stores the secret"
    );
    Ok(())
}

#[tokio::test]
async fn pending_turn_wait_closes_after_cancellation_stale_target_or_failed_keyring_result_and_rolls_back_failed_activation(
) -> Result<()> {
    for outcome in ["cancel", "pause", "capability", "keyring", "rollback"] {
        let f = TaskFixture::new(&["wait_for_event"], None).await?;
        let mut task = start(&f, &f.owner, None).await?;
        let rpc = stage(
            &f,
            &mut task,
            &json!({"event": "ready", "timeoutSeconds": 60}),
            "wait_for_event",
        )
        .await?;
        rpc.run(&f).await?.expect_code("device_offline")?;
        let provision = stored_provision(&field(
            &f,
            "SELECT request_json FROM wait_provisions",
            [],
            "request_json",
        )?)?;
        if outcome == "cancel" || outcome == "pause" {
            f.control(&f.owner, &task.id, json!({"requestId": new_id(), "expectedRevision": rpc.request["expectedRevision"], "operation": outcome}))
                .await?
                .expect_ok()?;
        }
        if outcome == "capability" {
            f.db()
                .execute("UPDATE agent_projects SET revision=revision+1", [])?;
        }
        let mut result = ready_result(&provision);
        if outcome == "keyring" {
            if let Some(fields) = result.as_object_mut() {
                fields.remove("secretRef");
            }
            result["state"] = json!("failed");
            result["errorCode"] = json!("keystore_unavailable");
        }
        if outcome == "rollback" {
            f.db().execute_batch("CREATE TRIGGER fail_wait_result BEFORE INSERT ON task_results BEGIN SELECT RAISE(ABORT,'fixture wait result failure'); END")?;
            f.wait_secret_result(result.clone())
                .await?
                .expect_code("storage_failed")?;
            ensure!(
                count(&f, "SELECT count(*) n FROM turn_waits")? == 0,
                "rollback kept a wait"
            );
            ensure!(
                field(&f, "SELECT state FROM wait_provisions", [], "state")? == "pending",
                "rollback closed the provision"
            );
            f.db().execute_batch("DROP TRIGGER fail_wait_result")?;
        }
        let ack = f.wait_secret_result(result.clone()).await?.expect_ok()?;
        let expected = if outcome == "rollback" {
            "activated"
        } else {
            "closed"
        };
        ensure!(ack.data()["state"] == expected, "{outcome}: {}", ack.body);
        ensure!(
            count(&f, "SELECT count(*) n FROM turn_waits")?
                == if outcome == "rollback" { 1 } else { 0 },
            "{outcome}: waits"
        );
        ensure!(
            f.wait_secret_result(result).await?.body == ack.body,
            "{outcome}: ack receipt"
        );
        if outcome == "cancel" || outcome == "pause" {
            let state = if outcome == "cancel" {
                "cancelled"
            } else {
                "paused"
            };
            ensure!(
                field(
                    &f,
                    "SELECT state FROM tasks WHERE id=?",
                    [&task.id],
                    "state"
                )? == state,
                "{outcome}: task state"
            );
        }
    }
    Ok(())
}

fn now_millis() -> Result<i64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as i64)
}

#[tokio::test]
async fn canonical_maintenance_expires_pending_and_activated_waits_once_without_reviving_closed_keys(
) -> Result<()> {
    for activated in [false, true] {
        let f = TaskFixture::new(&["wait_for_event"], None).await?;
        let mut task = start(&f, &f.owner, None).await?;
        let rpc = stage(
            &f,
            &mut task,
            &json!({"event": "ready", "timeoutSeconds": 2}),
            "wait_for_event",
        )
        .await?;
        rpc.run(&f).await?.expect_code("device_offline")?;
        let provision = stored_provision(&field(
            &f,
            "SELECT request_json FROM wait_provisions",
            [],
            "request_json",
        )?)?;
        let result = ready_result(&provision);
        if activated {
            f.wait_secret_result(result.clone()).await?.expect_ok()?;
        }
        let deadline = provision["deadline"].as_i64().ok_or("deadline")?;
        let until = deadline + 5000;
        while field(
            &f,
            "SELECT state FROM tasks WHERE id=?",
            [&task.id],
            "state",
        )? != "failed"
        {
            ensure!(
                now_millis()? < until,
                "Rust maintenance did not expire the wait"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        ensure!(now_millis()? >= deadline, "expired before the deadline");
        let revision = field(
            &f,
            "SELECT revision FROM tasks WHERE id=?",
            [&task.id],
            "revision",
        )?;
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        ensure!(
            field(
                &f,
                "SELECT revision FROM tasks WHERE id=?",
                [&task.id],
                "revision"
            )? == revision,
            "expired twice"
        );
        if activated {
            ensure!(
                field(&f, "SELECT status FROM turn_waits", [], "status")? == "expired",
                "wait status"
            );
            ensure!(
                f.wait_secret_result(result).await?.expect_ok()?.data()["state"] == "activated",
                "exact ACK changed after expiry"
            );
        } else {
            ensure!(
                f.wait_secret_result(result).await?.expect_ok()?.data()["state"] == "closed",
                "late result revived the key"
            );
            ensure!(
                count(&f, "SELECT count(*) n FROM turn_waits")? == 0,
                "late result activated"
            );
        }
        ensure!(
            count(&f, "SELECT count(*) n FROM task_results")? == 1,
            "task results"
        );
    }
    Ok(())
}

#[tokio::test]
async fn canonical_wait_maintenance_survives_sqlite_failure_and_retries_after_storage_recovers(
) -> Result<()> {
    let f = TaskFixture::new(&["wait_for_event"], None).await?;
    let mut task = start(&f, &f.owner, None).await?;
    let rpc = stage(
        &f,
        &mut task,
        &json!({"event": "ready", "timeoutSeconds": 60}),
        "wait_for_event",
    )
    .await?;
    rpc.run(&f).await?.expect_code("device_offline")?;
    let provision = stored_provision(&field(
        &f,
        "SELECT request_json FROM wait_provisions",
        [],
        "request_json",
    )?)?;
    f.wait_secret_result(ready_result(&provision))
        .await?
        .expect_ok()?;
    let other = TestRoot::new()?;
    let database = other.database();
    if let Some(parent) = database.parent() {
        std::fs::create_dir_all(parent)?;
    }
    canonical_snapshot(&f, &database, &f.owner.account_id)?;
    {
        let saved = minarbot_testkit::db::Fixture::open(&database)?;
        saved.execute_batch("UPDATE turn_waits SET deadline=0; CREATE TRIGGER fail_wait_maintenance BEFORE UPDATE ON turn_waits BEGIN SELECT RAISE(ABORT,'fixture wait maintenance failure'); END")?;
    }
    let publishable = {
        use minarbot_testkit::base64::Engine;
        format!(
            "pk_test_{}",
            minarbot_testkit::base64::engine::general_purpose::STANDARD
                .encode("fixture.clerk.accounts.dev$")
        )
    };
    let restarted = other
        .start(ServerConfig::new().real_clock().option(
            "auth",
            json!({"secretKey": "sk_test_fixture", "publishableKey": publishable,
                "issuer": "https://fixture.clerk.accounts.dev", "origin": "http://127.0.0.1", "development": true}),
        ))
        .await?;
    tokio::time::sleep(std::time::Duration::from_millis(1400)).await;
    let response = restarted
        .client()?
        .get(restarted.url("/api/session"))
        .header("X-Minarbot-Protocol", "1")
        .header("X-Minarbot-Request-Id", new_id())
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await?;
    let status = response.status().as_u16();
    ensure!(
        (400..500).contains(&status),
        "server unhealthy after maintenance failure: {status}"
    );
    let probe = minarbot_testkit::db::Fixture::open(&database)?;
    let status: String = probe.scalar("SELECT status FROM turn_waits", [])?;
    ensure!(
        status == "waiting",
        "failed maintenance changed the wait: {status}"
    );
    probe.execute_batch("DROP TRIGGER fail_wait_maintenance")?;
    let deadline = now_millis()? + 5000;
    loop {
        let status: String = probe.scalar("SELECT status FROM turn_waits", [])?;
        if status == "expired" {
            break;
        }
        ensure!(
            now_millis()? < deadline,
            "maintenance did not retry after the failure"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Ok(())
}

#[tokio::test]
async fn disaster_restore_closes_pending_provisions_and_activated_wait_references_while_keeping_receipts(
) -> Result<()> {
    let f = TaskFixture::new(&["wait_for_event"], None).await?;
    for activated in [false, true] {
        let mut task = start(&f, &f.owner, None).await?;
        let rpc = stage(
            &f,
            &mut task,
            &json!({"event": "ready", "timeoutSeconds": 60}),
            "wait_for_event",
        )
        .await?;
        rpc.run(&f).await?.expect_code("device_offline")?;
        let raw = field(
            &f,
            "SELECT request_json FROM wait_provisions WHERE id=?",
            [rpc.request["requestId"].as_str().unwrap_or_default()],
            "request_json",
        )?;
        let provision = stored_provision(&raw)?;
        if activated {
            f.wait_secret_result(ready_result(&provision))
                .await?
                .expect_ok()?;
        }
    }
    let receipts = f
        .db()
        .rows("SELECT * FROM receipts ORDER BY request_id", [])?;
    let directory = TempWorkspace::new()?;
    let backup = directory.join("backup.sqlite");
    canonical_snapshot(&f, &backup, &f.owner.account_id)?;
    let path = directory.join("restored.sqlite");
    let output = std::process::Command::new(minarbot_testkit::server::server_binary(None)?)
        .arg("--restore-read-only")
        .arg(&backup)
        .arg(&path)
        .output()?;
    ensure!(
        output.status.success(),
        "restore: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let restored = minarbot_testkit::db::Inspector::open(&path)?;
    ensure!(
        restored.rows("SELECT * FROM receipts ORDER BY request_id", [])? == receipts,
        "receipts changed"
    );
    ensure!(
        restored.scalar::<i64>("SELECT count(*) FROM wait_provisions", [])? == 2,
        "provisions"
    );
    ensure!(
        restored.scalar::<i64>(
            "SELECT count(*) FROM wait_provisions WHERE state='closed'",
            []
        )? == 2,
        "open provisions"
    );
    ensure!(
        restored.scalar::<String>("SELECT status FROM turn_waits", [])? == "closed",
        "activated wait"
    );
    ensure!(
        restored.scalar::<String>("SELECT serving_state FROM authority_state", [])?
            == "read_only_recovery",
        "serving state"
    );
    Ok(())
}

use crate::sb_task_support::Task;

async fn query(
    f: &TaskFixture,
    task: &mut Task,
    sql: &str,
    parameters: Option<Value>,
) -> Result<Envelope> {
    let mut args = json!({"sql": sql});
    if let Some(parameters) = parameters {
        args["parameters"] = parameters;
    }
    stage(f, task, &args, "query_data").await?.run(f).await
}

#[tokio::test]
async fn canonical_shared_data_keeps_schema_ownership_collaborator_scope_validation_and_atomic_receipts(
) -> Result<()> {
    let f = TaskFixture::new(&["query_data", "list_data_tables"], None).await?;
    let mut first = start(&f, &f.owner, None).await?;
    let created = query(&f, &mut first, "CREATE TABLE notes(text TEXT)", None)
        .await?
        .expect_ok()?;
    ensure!(
        created.data()["resultContent"]["revision"] == 1,
        "create: {}",
        created.body
    );
    let second_agent = new_id();
    let mut body = agent_body(&second_agent, "Other data agent", &f);
    body["instructions"] = json!("Use project data");
    f.human(
        minarbot_testkit::http::Method::POST,
        "/api/agents",
        Some(body),
        &f.owner,
    )
    .await?
    .expect_ok()?;
    f.db().execute(
        "UPDATE agent_projects SET capabilities_json=? WHERE agent=?",
        [
            json!({"v": 1, "tools": ["query_data", "list_data_tables"], "resources": []})
                .to_string(),
            second_agent.clone(),
        ],
    )?;
    f.db().execute(
        "INSERT INTO collaborator_agents VALUES (?,?,?,1,1)",
        [&f.project, &f.member.account_id, &f.agent],
    )?;
    let mut collaborator = start(&f, &f.member, None).await?;
    let inserted = query(
        &f,
        &mut collaborator,
        "INSERT INTO notes VALUES (?)",
        Some(json!(["Shared project row"])),
    )
    .await?
    .expect_ok()?;
    ensure!(
        inserted.data()["resultContent"]["revision"] == 2,
        "collaborator insert: {}",
        inserted.body
    );
    let command = f.command(json!({"agentId": second_agent}));
    let accepted = f.admit(&f.owner, &command).await?.expect_ok()?;
    ensure!(
        accepted.status == 202,
        "admission status: {}",
        accepted.status
    );
    let task_id = command["requestId"].as_str().ok_or("task")?.to_owned();
    let (token, ownership, _) = f.acquire_task(&task_id).await?;
    let mut other = Task {
        id: task_id,
        token,
        ownership,
        actor: f.owner.clone(),
        sequence: 0,
    };
    let dropped = query(&f, &mut other, "DROP TABLE notes", None)
        .await?
        .expect_ok()?;
    ensure!(
        dropped.data()["resultContent"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("authorized")),
        "foreign schema owner dropped the table: {}",
        dropped.body
    );
    let rows = query(&f, &mut other, "SELECT * FROM notes", None)
        .await?
        .expect_ok()?;
    ensure!(
        rows.data()["resultContent"]["rows"] == json!([["Shared project row"]]),
        "rows: {}",
        rows.body
    );
    for args in [
        json!({"sql": ""}),
        json!({"sql": "SELECT 1; SELECT 2"}),
        json!({"sql": "ATTACH DATABASE '/tmp/private' AS hidden"}),
        json!({"sql": "SELECT ?", "parameters": [true]}),
    ] {
        let result = stage(&f, &mut first, &args, "query_data")
            .await?
            .run(&f)
            .await?
            .expect_ok()?;
        ensure!(
            result.data()["resultContent"]["error"].is_string(),
            "{args}: {}",
            result.body
        );
    }
    let insert = stage(
        &f,
        &mut first,
        &json!({"sql": "INSERT INTO notes VALUES (?)", "parameters": ["Atomic row"]}),
        "query_data",
    )
    .await?;
    f.db().execute_batch("CREATE TRIGGER reject_data_result BEFORE INSERT ON task_results BEGIN SELECT RAISE(ABORT,'result journal failed'); END")?;
    insert.run(&f).await?.expect_code("storage_failed")?;
    ensure!(
        field(
            &f,
            "SELECT revision FROM shared_data WHERE project=?",
            [&f.project],
            "revision"
        )? == 2,
        "failed result changed data"
    );
    f.db().execute_batch("DROP TRIGGER reject_data_result")?;
    let saved = insert.run(&f).await?.expect_ok()?;
    ensure!(
        saved.data()["resultContent"]["revision"] == 3,
        "insert: {}",
        saved.body
    );
    ensure!(insert.run(&f).await?.body == saved.body, "insert receipt");
    let ordered = query(
        &f,
        &mut first,
        "SELECT text FROM notes ORDER BY rowid",
        None,
    )
    .await?
    .expect_ok()?;
    ensure!(
        ordered.data()["resultContent"]["rows"] == json!([["Shared project row"], ["Atomic row"]]),
        "rows: {}",
        ordered.body
    );
    let denied = stage(
        &f,
        &mut collaborator,
        &json!({"sql": "INSERT INTO notes VALUES ('Revoked')"}),
        "query_data",
    )
    .await?;
    let revoke = async {
        f.db().execute(
            "UPDATE collaborator_agents SET enabled=0,revision=2 WHERE project=? AND account=? AND agent=?",
            [&f.project, &f.member.account_id, &f.agent],
        )
    };
    let (denied_work, revoked) = tokio::join!(denied.run(&f), revoke);
    revoked?;
    denied_work?.expect_code("execution_denied")?;
    ensure!(
        field(
            &f,
            "SELECT revision FROM shared_data WHERE project=?",
            [&f.project],
            "revision"
        )? == 3,
        "revoked write applied"
    );
    stage(
        &f,
        &mut first,
        &json!({"sql": "SELECT 1", "project": f.project}),
        "query_data",
    )
    .await?
    .run(&f)
    .await?
    .expect_code("unknown_field")?;
    Ok(())
}

/// `sharedWorkerGate`: a `python3` on the server PATH that holds the shared
/// data worker while `hold` exists and records each start.
struct Gate {
    directory: TempWorkspace,
}

impl Gate {
    fn new() -> Result<Self> {
        let python = std::process::Command::new("python3")
            .args(["-I", "-c", "import sys; print(sys.executable)"])
            .output()?;
        let python = String::from_utf8(python.stdout)?.trim().to_owned();
        let directory = TempWorkspace::new()?;
        let script = format!(
            "#!{python}\nimport os, pathlib, sys, time\n\nroot = pathlib.Path(__file__).parent\nhold = root / 'hold'\nif len(sys.argv) > 2 and sys.argv[2].endswith('/shared-data-worker.py') and hold.exists():\n    (root / ('started-' + str(os.getpid()))).touch()\n    while hold.exists():\n        time.sleep(0.005)\n\nos.execv(sys.executable, [sys.executable, *sys.argv[1:]])\n"
        );
        let path = directory.write("python3", script)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        Ok(Self { directory })
    }

    fn path(&self) -> String {
        self.directory.path().display().to_string()
    }

    fn started_files(&self) -> Result<Vec<std::path::PathBuf>> {
        Ok(std::fs::read_dir(self.directory.path())?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("started-"))
            })
            .collect())
    }

    fn arm(&self) -> Result<()> {
        for path in self.started_files()? {
            std::fs::remove_file(path)?;
        }
        std::fs::write(self.directory.join("hold"), "")?;
        Ok(())
    }

    async fn started(&self, count: usize) -> Result<()> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while self.started_files()?.len() < count {
            ensure!(
                tokio::time::Instant::now() < deadline,
                "Rust did not start {count} shared query workers"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Ok(())
    }

    fn release(&self) {
        let _ = std::fs::remove_file(self.directory.join("hold"));
    }
}

#[tokio::test]
async fn canonical_shared_data_rechecks_cancellation_and_authority_across_worker_io_and_preserves_concurrent_conflicts(
) -> Result<()> {
    let gate = Gate::new()?;
    let f = TaskFixture::new(&["query_data", "list_data_tables"], Some(&gate.path())).await?;
    let result = async {
        let mut first = start(&f, &f.owner, None).await?;
        let mut second = start(&f, &f.owner, None).await?;
        stage(
            &f,
            &mut first,
            &json!({"sql": "CREATE TABLE notes(text TEXT)"}),
            "query_data",
        )
        .await?
        .run(&f)
        .await?
        .expect_ok()?;
        let pending = stage(
            &f,
            &mut first,
            &json!({"sql": "INSERT INTO notes VALUES ('Once')"}),
            "query_data",
        )
        .await?;
        gate.arm()?;
        let third = async {
            gate.started(2).await?;
            let busy = pending.run(&f).await?;
            gate.release();
            Ok::<_, minarbot_testkit::Error>(busy)
        };
        let (saved, duplicate, busy) = tokio::join!(pending.run(&f), pending.run(&f), third);
        busy?.expect_code("server_capacity")?;
        let (saved, duplicate) = (saved?.expect_ok()?, duplicate?.expect_ok()?);
        ensure!(duplicate.body == saved.body, "duplicate run differs");
        ensure!(
            saved.data()["resultContent"]["revision"] == 2,
            "once: {}",
            saved.body
        );
        let a = stage(
            &f,
            &mut first,
            &json!({"sql": "INSERT INTO notes VALUES ('First')"}),
            "query_data",
        )
        .await?;
        let b = stage(
            &f,
            &mut second,
            &json!({"sql": "INSERT INTO notes VALUES ('Second')"}),
            "query_data",
        )
        .await?;
        gate.arm()?;
        let release = async {
            gate.started(2).await?;
            gate.release();
            Ok::<_, minarbot_testkit::Error>(())
        };
        let (left, right, released) = tokio::join!(a.run(&f), b.run(&f), release);
        released?;
        let results = [left?.expect_ok()?, right?.expect_ok()?];
        let errors: Vec<&Envelope> = results
            .iter()
            .filter(|result| result.data()["resultContent"]["error"].is_string())
            .collect();
        ensure!(
            errors.len() == 1,
            "concurrent conflicts: {:?}",
            results
                .iter()
                .map(|result| &result.body)
                .collect::<Vec<_>>()
        );
        ensure!(
            errors[0].data()["resultContent"]["error"]
                .as_str()
                .is_some_and(|error| error.contains("Data changed")),
            "conflict: {}",
            errors[0].body
        );
        gate.arm()?;
        let stale = stage(
            &f,
            &mut first,
            &json!({"sql": "INSERT INTO notes VALUES ('Stale')"}),
            "query_data",
        )
        .await?;
        let change = async {
            gate.started(1).await?;
            f.db().execute(
                "UPDATE agents SET revision=revision+1 WHERE id=?",
                [&f.agent],
            )?;
            gate.release();
            Ok::<_, minarbot_testkit::Error>(())
        };
        let (stale_result, changed) = tokio::join!(stale.run(&f), change);
        changed?;
        stale_result?.expect_code("context_stale")?;
        ensure!(
            field(
                &f,
                "SELECT revision FROM shared_data WHERE project=?",
                [&f.project],
                "revision"
            )? == 3,
            "stale write applied"
        );
        gate.arm()?;
        let cancelled = stage(
            &f,
            &mut second,
            &json!({"sql": "INSERT INTO notes VALUES ('Cancelled')"}),
            "query_data",
        )
        .await?;
        let cancel = async {
            gate.started(1).await?;
            let revision = task_revision(&f, &second).await?;
            f.control(
                &f.owner,
                &second.id,
                json!({"requestId": new_id(), "expectedRevision": revision, "operation": "cancel"}),
            )
            .await?;
            Ok::<_, minarbot_testkit::Error>(())
        };
        let (cancelled_result, cancelled_task) = tokio::join!(cancelled.run(&f), cancel);
        cancelled_task?;
        cancelled_result?.expect_code("ownership_conflict")?;
        ensure!(
            field(
                &f,
                "SELECT revision FROM shared_data WHERE project=?",
                [&f.project],
                "revision"
            )? == 3,
            "cancelled write applied"
        );
        Ok(())
    }
    .await;
    gate.release();
    result
}

#[tokio::test]
async fn canonical_cancellation_kills_sql_workers_and_releases_query_capacity() -> Result<()> {
    let f = TaskFixture::new(&["query_data"], None).await?;
    let mut tasks = [
        start(&f, &f.owner, None).await?,
        start(&f, &f.owner, None).await?,
    ];
    let sql = "WITH RECURSIVE forever(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM forever) SELECT sum(n) FROM forever";
    let mut staged = Vec::new();
    for task in &mut tasks {
        staged.push(stage(&f, task, &json!({"sql": sql}), "query_data").await?);
    }
    let started = tokio::time::Instant::now();
    let cancel = async {
        for task in &tasks {
            let revision = task_revision(&f, task).await?;
            f.control(
                &f.owner,
                &task.id,
                json!({"requestId": new_id(), "expectedRevision": revision, "operation": "cancel"}),
            )
            .await?;
        }
        Ok::<_, minarbot_testkit::Error>(())
    };
    let (first, second, cancelled) = tokio::join!(staged[0].run(&f), staged[1].run(&f), cancel);
    cancelled?;
    ensure!(!first?.ok() && !second?.ok(), "cancelled queries completed");
    ensure!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "cancelled workers ran to their deadline: {:?}",
        started.elapsed()
    );
    let mut next = start(&f, &f.owner, None).await?;
    let result = query(&f, &mut next, "SELECT 42", None).await?.expect_ok()?;
    ensure!(
        result.data()["resultContent"]["rows"] == json!([[42]]),
        "capacity not released: {}",
        result.body
    );
    Ok(())
}

fn result_content(reply: &Envelope) -> Value {
    reply.data()["resultContent"].clone()
}

async fn status_of(f: &TaskFixture, task: &mut Task, args: Value, name: &str) -> Result<Value> {
    let reply = stage(f, task, &args, name)
        .await?
        .run(f)
        .await?
        .expect_ok()?;
    Ok(result_content(&reply)["status"].clone())
}

fn insert_thread(f: &TaskFixture, account: &str) -> Result<String> {
    let thread = new_id();
    f.db().execute(
        "INSERT INTO threads VALUES (?,?,'Side',1,0,0)",
        [&thread, &f.agent],
    )?;
    f.db().execute(
        "INSERT INTO record_scopes VALUES ('threads',?,?,?,'owner','thread',?)",
        [
            json!([thread]).to_string(),
            account.to_owned(),
            f.project.clone(),
            thread.clone(),
        ],
    )?;
    Ok(thread)
}

fn insert_channel(f: &TaskFixture) -> Result<String> {
    let channel = new_id();
    f.db().execute("INSERT INTO channels(id,name,instructions,lead,revision,archived) VALUES (?,'Room','',NULL,1,0)", [&channel])?;
    f.db().execute(
        "INSERT INTO channel_members VALUES (?,?)",
        [&channel, &f.agent],
    )?;
    f.db().execute(
        "INSERT INTO record_scopes VALUES ('channels',?,?,?,'project','channel',?)",
        [
            json!([channel]).to_string(),
            f.owner.account_id.clone(),
            f.project.clone(),
            channel.clone(),
        ],
    )?;
    Ok(channel)
}

fn channel_payload(channel: &str, text: &str) -> Value {
    json!({"v": 1, "kind": "channel", "channelId": channel, "routing": "assigned", "message": {"text": text, "files": []}})
}

fn side_payload(thread: &str, text: &str) -> Value {
    json!({"v": 1, "kind": "side", "threadId": thread, "message": {"text": text, "files": []}})
}

async fn lookup_data(
    f: &TaskFixture,
    actor: &minarbot_testkit::canonical::Account,
    task: &str,
) -> Result<Value> {
    Ok(f.lookup(actor, task).await?.expect_ok()?.data().clone())
}

#[tokio::test]
async fn canonical_questions_validate_limits_and_atomically_pause_answer_and_retain_cancellation(
) -> Result<()> {
    let f = TaskFixture::new(&["ask_user"], None).await?;
    let _stream = f.connect("Asia/Jakarta", 1).await?;
    let mut task = start(&f, &f.owner, None).await?;
    for args in [
        json!({"question": "?", "choices": ["Yes", " Yes "]}),
        json!({"question": "", "choices": []}),
        json!({"question": "?", "choices": vec!["x"; 9]}),
        json!({"question": "?", "choices": [2]}),
        json!({"question": "x".repeat(2001), "choices": []}),
    ] {
        ensure!(
            status_of(&f, &mut task, args.clone(), "ask_user").await? == 400,
            "{args} accepted"
        );
    }
    ensure!(
        lookup_data(&f, &f.owner, &task.id).await?["questions"] == json!([]),
        "invalid question stored"
    );
    let ask = stage(
        &f,
        &mut task,
        &json!({"question": " Continue? ", "choices": [" Yes ", "No"]}),
        "ask_user",
    )
    .await?;
    f.db().execute_batch("CREATE TRIGGER fail_question_result BEFORE INSERT ON task_results BEGIN SELECT RAISE(ABORT,'question result failed'); END")?;
    ask.run(&f).await?.expect_code("storage_failed")?;
    let unchanged = lookup_data(&f, &f.owner, &task.id).await?;
    ensure!(
        unchanged["questions"] == json!([]) && unchanged["state"] == "running",
        "failed question changed the task: {unchanged}"
    );
    f.db().execute_batch("DROP TRIGGER fail_question_result")?;
    let saved = ask.run(&f).await?.expect_ok()?;
    let question = result_content(&saved)["id"]
        .as_str()
        .ok_or("question id")?
        .to_owned();
    ensure!(ask.run(&f).await?.body == saved.body, "question receipt");
    ensure!(
        lookup_data(&f, &f.owner, &task.id).await?["state"] == "paused",
        "question did not pause"
    );
    f.control(
        &f.owner,
        &task.id,
        json!({"requestId": new_id(), "expectedRevision": saved.receipt()["recordRevision"],
        "operation": "reassign", "recipientAgentId": f.agent}),
    )
    .await?
    .expect_code("question_pending")?;
    let answer = json!({"requestId": new_id(), "expectedRevision": saved.receipt()["recordRevision"], "text": "Yes"});
    f.db().execute_batch("CREATE TRIGGER fail_question_answer BEFORE INSERT ON question_answers BEGIN SELECT RAISE(ABORT,'answer failed'); END")?;
    f.answer(&f.owner, &task.id, &question, answer.clone())
        .await?
        .expect_code("storage_failed")?;
    let unchanged = lookup_data(&f, &f.owner, &task.id).await?;
    ensure!(
        unchanged["state"] == "paused",
        "failed answer resumed: {unchanged}"
    );
    ensure!(
        unchanged["revision"] == answer["expectedRevision"],
        "failed answer revision: {unchanged}"
    );
    ensure!(
        unchanged["questions"][0]["answer"].is_null(),
        "failed answer stored: {unchanged}"
    );
    f.db().execute_batch("DROP TRIGGER fail_question_answer")?;
    let answered = f
        .answer(&f.owner, &task.id, &question, answer.clone())
        .await?
        .expect_ok()?;
    ensure!(
        answered.data()["state"] == "running",
        "answer: {}",
        answered.body
    );
    ensure!(
        f.answer(&f.owner, &task.id, &question, answer.clone())
            .await?
            .body
            == answered.body,
        "answer receipt"
    );
    let mut late = answer.clone();
    late["requestId"] = json!(new_id());
    late["expectedRevision"] = answered.receipt()["recordRevision"].clone();
    f.answer(&f.owner, &task.id, &question, late)
        .await?
        .expect_code("question_closed")?;
    for n in 1..8 {
        let asked = stage(
            &f,
            &mut task,
            &json!({"question": format!("Question {n}"), "choices": []}),
            "ask_user",
        )
        .await?
        .run(&f)
        .await?
        .expect_ok()?;
        let id = result_content(&asked)["id"]
            .as_str()
            .ok_or("question")?
            .to_owned();
        f.answer(&f.owner, &task.id, &id, json!({"requestId": new_id(), "expectedRevision": asked.receipt()["recordRevision"], "text": "Free text"}))
            .await?
            .expect_ok()?;
    }
    ensure!(
        status_of(
            &f,
            &mut task,
            json!({"question": "Ninth question", "choices": []}),
            "ask_user"
        )
        .await?
            == 409,
        "ninth question"
    );
    ensure!(
        lookup_data(&f, &f.owner, &task.id).await?["questions"]
            .as_array()
            .map(Vec::len)
            == Some(8),
        "question count"
    );
    let mut cancelled = start(&f, &f.owner, None).await?;
    let pending = stage(
        &f,
        &mut cancelled,
        &json!({"question": "Stop?", "choices": []}),
        "ask_user",
    )
    .await?;
    let pending_result = pending.run(&f).await?.expect_ok()?;
    let pending_id = result_content(&pending_result)["id"]
        .as_str()
        .ok_or("question")?
        .to_owned();
    let stopped = f
        .control(&f.owner, &cancelled.id, json!({"requestId": new_id(), "expectedRevision": pending_result.receipt()["recordRevision"], "operation": "cancel"}))
        .await?
        .expect_ok()?;
    ensure!(
        pending.run(&f).await?.body == pending_result.body,
        "question receipt after cancel"
    );
    f.answer(&f.owner, &cancelled.id, &pending_id, json!({"requestId": new_id(), "expectedRevision": stopped.receipt()["recordRevision"], "text": "Late"}))
        .await?
        .expect_code("question_closed")?;
    ensure!(
        lookup_data(&f, &f.owner, &cancelled.id).await?["state"] == "cancelled",
        "cancelled task resumed"
    );
    Ok(())
}

#[tokio::test]
async fn canonical_question_answers_enforce_private_human_conversation_project_and_current_collaborator_access(
) -> Result<()> {
    let f = TaskFixture::new(&["ask_user"], None).await?;
    f.db().execute(
        "INSERT INTO collaborator_agents VALUES (?,?,?,1,1)",
        [&f.project, &f.member.account_id, &f.agent],
    )?;
    f.db().execute(
        "INSERT INTO response_policies VALUES (?,?,?,1,1)",
        [&f.project, &f.member.account_id, &f.agent],
    )?;
    let mut task = start(&f, &f.member, None).await?;
    let asked = stage(
        &f,
        &mut task,
        &json!({"question": "Member private?", "choices": ["Yes"]}),
        "ask_user",
    )
    .await?
    .run(&f)
    .await?
    .expect_ok()?;
    let question = result_content(&asked)["id"]
        .as_str()
        .ok_or("question")?
        .to_owned();
    let answer = json!({"requestId": new_id(), "expectedRevision": asked.receipt()["recordRevision"], "text": "Yes"});
    f.answer(&f.owner, &task.id, &question, answer.clone())
        .await?
        .expect_code("resource_not_found")?;
    f.answer(&f.foreign, &task.id, &question, answer.clone())
        .await?
        .expect_code("resource_not_found")?;
    f.answer(&f.member, &task.id, &new_id(), answer.clone())
        .await?
        .expect_code("resource_not_found")?;
    let other = start(&f, &f.member, None).await?;
    f.answer(&f.member, &other.id, &question, answer.clone())
        .await?
        .expect_code("resource_not_found")?;
    let answered = f
        .answer(&f.member, &task.id, &question, answer.clone())
        .await?
        .expect_ok()?;
    ensure!(
        answered.receipt()["actorId"] == f.member.account_id.as_str(),
        "answer actor: {}",
        answered.body
    );
    f.db().execute(
        "UPDATE collaborator_agents SET enabled=0 WHERE account=?",
        [&f.member.account_id],
    )?;
    f.answer(&f.member, &task.id, &question, answer.clone())
        .await?
        .expect_code("execution_denied")?;
    f.db().execute(
        "UPDATE collaborator_agents SET enabled=1 WHERE account=?",
        [&f.member.account_id],
    )?;
    let thread = insert_thread(&f, &f.member.account_id)?;
    let mut side = start(&f, &f.member, Some(side_payload(&thread, "Side question"))).await?;
    let side_result = stage(
        &f,
        &mut side,
        &json!({"question": "Side?", "choices": []}),
        "ask_user",
    )
    .await?
    .run(&f)
    .await?
    .expect_ok()?;
    let side_question = result_content(&side_result)["id"]
        .as_str()
        .ok_or("question")?
        .to_owned();
    f.db().execute(
        "UPDATE threads SET archived=1,revision=revision+1 WHERE id=?",
        [&thread],
    )?;
    f.answer(&f.member, &side.id, &side_question, json!({"requestId": new_id(), "expectedRevision": side_result.receipt()["recordRevision"], "text": "Late"}))
        .await?
        .expect_code("execution_denied")?;
    let channel = insert_channel(&f)?;
    let mut room = start(
        &f,
        &f.owner,
        Some(channel_payload(&channel, "Shared question")),
    )
    .await?;
    let room_result = stage(
        &f,
        &mut room,
        &json!({"question": "Room?", "choices": []}),
        "ask_user",
    )
    .await?
    .run(&f)
    .await?
    .expect_ok()?;
    let room_question = result_content(&room_result)["id"]
        .as_str()
        .ok_or("question")?
        .to_owned();
    f.answer(&f.member, &side.id, &room_question, json!({"requestId": new_id(), "expectedRevision": side_result.receipt()["recordRevision"], "text": "Wrong conversation"}))
        .await?
        .expect_code("resource_not_found")?;
    let shared = f
        .answer(&f.member, &room.id, &room_question, json!({"requestId": new_id(), "expectedRevision": room_result.receipt()["recordRevision"], "text": "Shared answer"}))
        .await?
        .expect_ok()?;
    ensure!(
        shared.data()["answer"] == "Shared answer",
        "shared answer: {}",
        shared.body
    );
    f.db().execute(
        "UPDATE project_members SET deleted=1 WHERE account=?",
        [&f.member.account_id],
    )?;
    f.answer(&f.member, &task.id, &question, answer)
        .await?
        .expect_code("resource_not_found")?;
    Ok(())
}

async fn avatar_removal(side: bool) -> Result<()> {
    let f = TaskFixture::new(&["set_avatar"], None).await?;
    let payload = if side {
        Some(side_payload(
            &insert_thread(&f, &f.owner.account_id)?,
            "Remove avatar",
        ))
    } else {
        None
    };
    let mut task = start(&f, &f.owner, payload).await?;
    let snapshot = f
        .task_op(
            "context",
            &task.id,
            json!({"requestId": new_id(), "ownership": task.ownership, "expectedTaskRevision": 2}),
            &task.token,
        )
        .await?
        .expect_ok()?;
    ensure!(
        snapshot.data()["snapshot"]["profile"]["avatarRevision"] == 0,
        "avatar revision: {}",
        snapshot.body
    );
    let removed = stage(
        &f,
        &mut task,
        &json!({"image": null, "revision": 0}),
        "set_avatar",
    )
    .await?
    .run(&f)
    .await?
    .expect_ok()?;
    ensure!(
        result_content(&removed) == json!({"revision": 1, "present": false}),
        "first removal: {}",
        removed.body
    );
    for revision in [
        json!(-1),
        json!(1.5),
        json!("1"),
        json!(9_007_199_254_740_991_i64),
    ] {
        ensure!(
            status_of(
                &f,
                &mut task,
                json!({"image": null, "revision": revision}),
                "set_avatar"
            )
            .await?
                == 400,
            "revision {revision}"
        );
    }
    for image in [None, Some(json!(12)), Some(json!({})), Some(json!("bad"))] {
        let mut args = json!({"revision": 1});
        if let Some(image) = image.clone() {
            args["image"] = image;
        }
        ensure!(
            status_of(&f, &mut task, args, "set_avatar").await? == 400,
            "image {image:?}"
        );
    }
    ensure!(
        status_of(
            &f,
            &mut task,
            json!({"image": new_id(), "revision": 1}),
            "set_avatar"
        )
        .await?
            == 422,
        "unknown image"
    );
    ensure!(
        status_of(
            &f,
            &mut task,
            json!({"image": null, "revision": 0}),
            "set_avatar"
        )
        .await?
            == 409,
        "stale revision"
    );
    f.db().execute(
        "UPDATE agent_avatars SET image=?,revision=2 WHERE agent=?",
        (b"saved image".to_vec(), f.agent.clone()),
    )?;
    let deletion = stage(
        &f,
        &mut task,
        &json!({"image": null, "revision": 2}),
        "set_avatar",
    )
    .await?;
    f.db().execute_batch("CREATE TRIGGER fail_avatar_result BEFORE INSERT ON task_results BEGIN SELECT RAISE(ABORT,'avatar result failed'); END")?;
    deletion.run(&f).await?.expect_code("storage_failed")?;
    ensure!(
        field(
            &f,
            "SELECT revision FROM agent_avatars WHERE agent=?",
            [&f.agent],
            "revision"
        )? == 2,
        "failed removal changed the revision"
    );
    ensure!(
        f.db()
            .blob("SELECT image FROM agent_avatars WHERE agent=?", [&f.agent])?
            == b"saved image",
        "failed removal deleted the image"
    );
    f.db().execute_batch("DROP TRIGGER fail_avatar_result")?;
    let saved = deletion.run(&f).await?.expect_ok()?;
    ensure!(
        result_content(&saved) == json!({"revision": 3, "present": false}),
        "removal: {}",
        saved.body
    );
    ensure!(
        field(
            &f,
            "SELECT image FROM agent_avatars WHERE agent=?",
            [&f.agent],
            "image"
        )?
        .is_null(),
        "image kept"
    );
    f.db().execute(
        "UPDATE agent_avatars SET image=?,revision=4 WHERE agent=?",
        (b"later image".to_vec(), f.agent.clone()),
    )?;
    ensure!(
        deletion.run(&f).await?.body == saved.body,
        "removal receipt"
    );
    ensure!(
        field(
            &f,
            "SELECT revision FROM agent_avatars WHERE agent=?",
            [&f.agent],
            "revision"
        )? == 4,
        "receipt replay removed again"
    );
    let stale = stage(
        &f,
        &mut task,
        &json!({"image": null, "revision": 4}),
        "set_avatar",
    )
    .await?;
    f.db().execute(
        "UPDATE agent_avatars SET revision=5 WHERE agent=?",
        [&f.agent],
    )?;
    stale.run(&f).await?.expect_code("context_stale")?;
    let revision = lookup_data(&f, &f.owner, &task.id).await?["revision"].clone();
    f.control(
        &f.owner,
        &task.id,
        json!({"requestId": new_id(), "expectedRevision": revision, "operation": "cancel"}),
    )
    .await?
    .expect_ok()?;
    ensure!(
        deletion.run(&f).await?.body == saved.body,
        "receipt after cancel"
    );
    f.db().execute(
        "INSERT INTO collaborator_agents VALUES (?,?,?,1,1)",
        [&f.project, &f.member.account_id, &f.agent],
    )?;
    f.db().execute(
        "INSERT INTO response_policies VALUES (?,?,?,1,1)",
        [&f.project, &f.member.account_id, &f.agent],
    )?;
    let mut member = start(&f, &f.member, None).await?;
    let member_delete = stage(
        &f,
        &mut member,
        &json!({"image": null, "revision": 5}),
        "set_avatar",
    )
    .await?;
    ensure!(
        result_content(&member_delete.run(&f).await?.expect_ok()?)
            == json!({"revision": 6, "present": false}),
        "member removal"
    );
    f.db().execute(
        "UPDATE collaborator_agents SET enabled=0,revision=2 WHERE project=? AND account=? AND agent=?",
        [&f.project, &f.member.account_id, &f.agent],
    )?;
    member_delete
        .run(&f)
        .await?
        .expect_code("execution_denied")?;
    let channel = insert_channel(&f)?;
    let mut room = start(&f, &f.owner, Some(channel_payload(&channel, "Room avatar"))).await?;
    let context = f
        .task_op(
            "context",
            &room.id,
            json!({"requestId": new_id(), "ownership": room.ownership, "expectedTaskRevision": 2}),
            &room.token,
        )
        .await?
        .expect_ok()?;
    ensure!(
        context.data()["snapshot"]["tools"] == json!([]),
        "channel tools: {}",
        context.body
    );
    stage(
        &f,
        &mut room,
        &json!({"image": null, "revision": 6}),
        "set_avatar",
    )
    .await?
    .run(&f)
    .await?
    .expect_code("execution_denied")?;
    Ok(())
}

#[tokio::test]
async fn canonical_avatar_removal_in_direct_preserves_cas_rollback_scope_and_exact_results(
) -> Result<()> {
    avatar_removal(false).await
}

#[tokio::test]
async fn canonical_avatar_removal_in_side_preserves_cas_rollback_scope_and_exact_results(
) -> Result<()> {
    avatar_removal(true).await
}

async fn sites(side: bool) -> Result<()> {
    let f = TaskFixture::new(&["list_sites", "delete_site"], None).await?;
    let other = new_id();
    f.human(
        minarbot_testkit::http::Method::POST,
        "/api/projects",
        Some(json!({"requestId": new_id(), "id": other, "name": "Other"})),
        &f.owner,
    )
    .await?;
    let peer = new_id();
    let mut body = agent_body(&peer, "Peer", &f);
    body["instructions"] = json!("Peer");
    f.human(
        minarbot_testkit::http::Method::POST,
        "/api/agents",
        Some(body),
        &f.owner,
    )
    .await?
    .expect_ok()?;
    let seed = |id: String,
                project: &str,
                account: &str,
                agent: &str,
                visibility: &str,
                mapped: bool,
                root: &str|
     -> Result<String> {
        let source = "<p>PRIVATE_SITE_SOURCE</p>";
        f.db().execute(
            "INSERT INTO local_sites(id,agent,name,source,html,revision,updated) VALUES (?,?,'Scoped site',?,?,1,'2026-11-18T11:06:40.000Z')",
            [&id, agent, source, source],
        )?;
        if mapped {
            f.db().execute(
                "INSERT INTO record_scopes VALUES ('local_sites',?,?,?,?, 'agent',?)",
                [
                    json!([id]).to_string(),
                    account.to_owned(),
                    project.to_owned(),
                    visibility.to_owned(),
                    root.to_owned(),
                ],
            )?;
        }
        Ok(id)
    };
    let (owner, member, agent) = (
        f.owner.account_id.clone(),
        f.member.account_id.clone(),
        f.agent.clone(),
    );
    let mut sites = Vec::new();
    for n in 0..12 {
        sites.push(seed(
            format!("00000000-0000-4000-8000-{n:012}"),
            &f.project,
            &owner,
            &agent,
            "owner",
            true,
            &agent,
        )?);
    }
    let private_site = seed(new_id(), &f.project, &member, &agent, "owner", true, &agent)?;
    let foreign = seed(new_id(), &other, &owner, &agent, "owner", true, &agent)?;
    let foreign_agent = seed(new_id(), &f.project, &owner, &peer, "owner", true, &peer)?;
    let unmapped = seed(new_id(), &f.project, &owner, &agent, "owner", false, &agent)?;
    let wrong_root = seed(new_id(), &f.project, &owner, &agent, "owner", true, &peer)?;
    let deleted = seed(new_id(), &f.project, &owner, &agent, "owner", true, &agent)?;
    f.db()
        .execute("UPDATE local_sites SET deleted=1 WHERE id=?", [&deleted])?;
    let payload = if side {
        Some(side_payload(&insert_thread(&f, &owner)?, "Sites"))
    } else {
        None
    };
    let mut task = start(&f, &f.owner, payload).await?;
    let first = result_content(
        &stage(&f, &mut task, &json!({}), "list_sites")
            .await?
            .run(&f)
            .await?
            .expect_ok()?,
    );
    let ids = |value: &Value| -> Vec<Value> {
        value["sites"]
            .as_array()
            .map(|rows| rows.iter().map(|row| row["id"].clone()).collect())
            .unwrap_or_default()
    };
    ensure!(
        ids(&first) == sites[..10].iter().map(|id| json!(id)).collect::<Vec<_>>(),
        "first page: {first}"
    );
    ensure!(first["next_after"] == sites[9].as_str(), "cursor: {first}");
    ensure!(
        !first.to_string().contains("PRIVATE_SITE_SOURCE"),
        "site source listed"
    );
    let after = first["next_after"].as_str().ok_or("cursor")?.to_uppercase();
    let last = result_content(
        &stage(&f, &mut task, &json!({"after": after}), "list_sites")
            .await?
            .run(&f)
            .await?
            .expect_ok()?,
    );
    ensure!(
        ids(&last) == sites[10..].iter().map(|id| json!(id)).collect::<Vec<_>>(),
        "last page: {last}"
    );
    ensure!(last["next_after"].is_null(), "last cursor: {last}");
    for after in [json!(0), json!([]), json!({}), json!("bad")] {
        ensure!(
            status_of(&f, &mut task, json!({"after": after}), "list_sites").await? == 400,
            "cursor {after}"
        );
    }
    for site in [
        &private_site,
        &foreign,
        &foreign_agent,
        &unmapped,
        &wrong_root,
    ] {
        ensure!(
            status_of(
                &f,
                &mut task,
                json!({"site": site, "revision": 1}),
                "delete_site"
            )
            .await?
                == 404,
            "site {site} visible"
        );
    }
    for revision in [json!(0), json!(1.5), json!(9_007_199_254_740_991_i64)] {
        ensure!(
            status_of(
                &f,
                &mut task,
                json!({"site": sites[0], "revision": revision}),
                "delete_site"
            )
            .await?
                == 400,
            "revision {revision}"
        );
    }
    ensure!(
        status_of(
            &f,
            &mut task,
            json!({"site": sites[0], "revision": 2}),
            "delete_site"
        )
        .await?
            == 409,
        "stale revision"
    );
    let deletion = stage(
        &f,
        &mut task,
        &json!({"site": sites[0], "revision": 1}),
        "delete_site",
    )
    .await?;
    f.db().execute_batch("CREATE TRIGGER fail_site_result BEFORE INSERT ON task_results BEGIN SELECT RAISE(ABORT,'site result failed'); END")?;
    deletion.run(&f).await?.expect_code("storage_failed")?;
    ensure!(
        field(
            &f,
            "SELECT revision FROM local_sites WHERE id=?",
            [&sites[0]],
            "revision"
        )? == 1,
        "failed deletion revision"
    );
    ensure!(
        field(
            &f,
            "SELECT deleted FROM local_sites WHERE id=?",
            [&sites[0]],
            "deleted"
        )? == 0,
        "failed deletion deleted"
    );
    f.db().execute_batch("DROP TRIGGER fail_site_result")?;
    let saved = deletion.run(&f).await?.expect_ok()?;
    ensure!(
        result_content(&saved)
            == json!({"id": sites[0], "name": "Scoped site", "revision": 2, "deleted": 1, "path": null}),
        "deletion: {}",
        saved.body
    );
    ensure!(
        deletion.run(&f).await?.body == saved.body,
        "deletion receipt"
    );
    ensure!(
        field(
            &f,
            "SELECT source FROM local_sites WHERE id=?",
            [&sites[0]],
            "source"
        )? == "",
        "source kept"
    );
    ensure!(
        status_of(
            &f,
            &mut task,
            json!({"site": sites[0], "revision": 2}),
            "delete_site"
        )
        .await?
            == 410,
        "deleted site"
    );
    let revision = lookup_data(&f, &f.owner, &task.id).await?["revision"].clone();
    f.control(
        &f.owner,
        &task.id,
        json!({"requestId": new_id(), "expectedRevision": revision, "operation": "cancel"}),
    )
    .await?
    .expect_ok()?;
    ensure!(
        deletion.run(&f).await?.body == saved.body,
        "receipt after cancel"
    );
    f.db().execute(
        "INSERT INTO collaborator_agents VALUES (?,?,?,1,1)",
        [&f.project, &member, &agent],
    )?;
    f.db().execute(
        "INSERT INTO response_policies VALUES (?,?,?,1,1)",
        [&f.project, &member, &agent],
    )?;
    let mut member_task = start(&f, &f.member, None).await?;
    let shared = seed(
        new_id(),
        &f.project,
        &owner,
        &agent,
        "project",
        true,
        &agent,
    )?;
    let own = result_content(
        &stage(
            &f,
            &mut member_task,
            &json!({"site": private_site, "revision": 1}),
            "delete_site",
        )
        .await?
        .run(&f)
        .await?
        .expect_ok()?,
    );
    ensure!(own["deleted"] == 1, "member private site: {own}");
    ensure!(
        status_of(
            &f,
            &mut member_task,
            json!({"site": sites[1], "revision": 1}),
            "delete_site"
        )
        .await?
            == 404,
        "member deleted owner site"
    );
    let project_site = result_content(
        &stage(
            &f,
            &mut member_task,
            &json!({"site": shared, "revision": 1}),
            "delete_site",
        )
        .await?
        .run(&f)
        .await?
        .expect_ok()?,
    );
    ensure!(project_site["deleted"] == 1, "project site: {project_site}");
    let channel = insert_channel(&f)?;
    for name in ["list_sites", "delete_site"] {
        let mut room = start(&f, &f.owner, Some(channel_payload(&channel, "Room sites"))).await?;
        let args = if name == "list_sites" {
            json!({})
        } else {
            json!({"site": sites[1], "revision": 1})
        };
        stage(&f, &mut room, &args, name)
            .await?
            .run(&f)
            .await?
            .expect_code("execution_denied")?;
    }
    Ok(())
}

#[tokio::test]
async fn canonical_sites_in_direct_page_scoped_metadata_and_retain_atomic_deletion() -> Result<()> {
    sites(false).await
}

#[tokio::test]
async fn canonical_sites_in_side_page_scoped_metadata_and_retain_atomic_deletion() -> Result<()> {
    sites(true).await
}
