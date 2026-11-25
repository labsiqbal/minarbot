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
