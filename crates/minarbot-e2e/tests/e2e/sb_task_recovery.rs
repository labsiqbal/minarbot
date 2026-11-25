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

use crate::sb_task_support::{prepared, Prepared};
use minarbot_testkit::protocol::{canonical_json, digest_command};

/// `(resultDigest, resultRef)` of `content` for `action`.
fn result_ref(task: &str, action: &str, content: &Value) -> (String, Value) {
    let digest = sha256_hex(canonical_json(content));
    let reference = json!({"v": 1, "taskId": task, "actionId": action, "digest": digest,
        "size": canonical_json(content).len(), "encoding": "canonical-json-v1"});
    (digest, reference)
}

fn with(mut value: Value, change: Value) -> Value {
    if let (Some(target), Value::Object(fields)) = (value.as_object_mut(), change) {
        target.extend(fields);
    }
    value
}

/// `journalEntryBody(sourceAuthority, device, generation, entry)`.
fn journal_entry_body(authority: &Value, device: &str, generation: i64, entry: &Value) -> Value {
    json!([
        "minarbot-journal-entry-v1",
        authority,
        device,
        generation,
        entry["taskId"],
        entry["actionId"],
        entry["sequence"],
        entry["executorGeneration"],
        entry["actionKind"],
        entry["payloadDigest"],
        entry["intentDigest"],
        entry.get("bindingId").cloned().unwrap_or(Value::Null),
        entry.get("bindingRevision").cloned().unwrap_or(Value::Null),
        entry["grantRevision"],
        entry["state"],
        entry["effectStatus"],
        entry.get("resultDigest").cloned().unwrap_or(Value::Null),
        entry.get("resultRef").cloned().unwrap_or(Value::Null),
        entry["journalSequence"]
    ])
}

/// `stopEvidenceBody(stop)`.
fn stop_evidence_body(stop: &Value) -> Value {
    let children: Vec<Value> = stop["children"]
        .as_array()
        .map(|children| {
            children
                .iter()
                .map(|child| {
                    json!([
                        child["processId"],
                        child["processStart"],
                        child["processGroupId"]
                    ])
                })
                .collect()
        })
        .unwrap_or_default();
    json!([
        "minarbot-stop-evidence-v1",
        stop["sourceAuthority"],
        stop["taskId"],
        stop["actionId"],
        stop["sourceDeviceId"],
        stop["sourceDeviceGeneration"],
        stop["executorGeneration"],
        stop["bootId"],
        stop["processId"],
        stop["processStart"],
        stop["lockId"],
        stop["observationBootId"],
        stop["observation"],
        children,
        stop["observedAt"],
        stop["journalDigest"]
    ])
}

async fn commit(f: &TaskFixture, p: &Prepared) -> Result<Envelope> {
    f.commit(&p.task, &p.action, p.commit.clone(), &p.token)
        .await?
        .expect_ok()
}

async fn reconcile(f: &TaskFixture, p: &Prepared, body: Value) -> Result<Envelope> {
    f.task_op("reconcile", &p.task, body, &p.token).await
}

fn report(p: &Prepared, state: &str, effect: &str, sequence: i64) -> Value {
    json!({"requestId": new_id(), "actionId": p.action, "ownership": p.ownership, "payloadDigest": p.payload_digest,
        "state": state, "effectStatus": effect, "journalSequence": sequence})
}

#[tokio::test]
async fn reassign_denies_unresolved_effects_revoked_initiators_and_invalid_successor_provenance_atomically(
) -> Result<()> {
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    let p = prepared(&f).await?;
    let recipient = new_id();
    f.human(
        minarbot_testkit::http::Method::POST,
        "/api/agents",
        Some(agent_body(&recipient, "Recipient", &f)),
        &f.owner,
    )
    .await?
    .expect_ok()?;
    let committed = commit(&f, &p).await?;
    let paused = f
        .control(&f.owner, &p.task, json!({"requestId": new_id(), "expectedRevision": committed.receipt()["recordRevision"], "operation": "pause"}))
        .await?
        .expect_ok()?;
    let mut reassign = json!({"requestId": new_id(), "expectedRevision": paused.receipt()["recordRevision"], "operation": "reassign", "recipientAgentId": recipient});
    f.control(&f.owner, &p.task, reassign.clone())
        .await?
        .expect_code("outcome_unresolved")?;
    ensure!(
        lookup_data(&f, &f.owner, &p.task).await?["currentTaskId"] == p.task.as_str(),
        "unresolved task transferred"
    );
    let content = json!({"text": "Reconciled"});
    let (digest, reference) = result_ref(&p.task, &p.action, &content);
    let mut body = report(&p, "acknowledged", "known", 2);
    body["resultContent"] = content;
    body["resultDigest"] = json!(digest);
    body["resultRef"] = reference;
    let published = reconcile(&f, &p, body).await?.expect_ok()?;
    reassign["expectedRevision"] = published.receipt()["recordRevision"].clone();
    let transferred = f
        .control(&f.owner, &p.task, reassign.clone())
        .await?
        .expect_ok()?;
    let next = transferred.data()["taskId"]
        .as_str()
        .ok_or("successor")?
        .to_owned();
    f.task_op("context", &p.task, json!({"requestId": new_id(), "ownership": p.ownership, "expectedTaskRevision": reassign["expectedRevision"]}), &p.token)
        .await?
        .expect_code("ownership_conflict")?;
    let original = one(&f, "SELECT * FROM tasks WHERE id=?", [&next])?;
    let unused = f.command(json!({}));
    f.admit(&f.owner, &unused).await?.expect_ok()?;
    let unused_id = unused["requestId"].as_str().ok_or("unused")?.to_owned();
    f.db().execute(
        "UPDATE tasks SET state='transferred' WHERE id=?",
        [&unused_id],
    )?;
    let columns: Vec<String> = original
        .as_object()
        .ok_or("task row")?
        .keys()
        .cloned()
        .collect();
    // Copy the row through JSON so every column keeps its SQLite type.
    let insert = format!(
        "INSERT INTO tasks({}) SELECT {}",
        columns.join(","),
        columns
            .iter()
            .map(|column| format!("json_extract(?1,'$.\"{column}\"')"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut base = original.clone();
    base["id"] = json!(new_id());
    base["logical_turn"] = json!(unused_id);
    base["predecessor_task"] = json!(unused_id);
    let run = |row: &Value| f.db().execute(&insert, [row.to_string()]);
    let mut invalid = base.clone();
    invalid["initiator_account"] = json!(f.member.account_id);
    ensure!(
        run(&invalid).is_err_and(|error| error
            .to_string()
            .contains("invalid task successor provenance")),
        "foreign initiator accepted"
    );
    for key in ["capability_snapshot_json", "capability_digest"] {
        let mut forged = base.clone();
        forged[key] = if key == "capability_snapshot_json" {
            json!("{}")
        } else {
            json!("0".repeat(64))
        };
        ensure!(
            run(&forged).is_err_and(|error| error
                .to_string()
                .contains("invalid task successor provenance")),
            "forged {key} accepted"
        );
    }
    run(&base)?;
    f.db().execute(
        "UPDATE project_members SET deleted=1 WHERE project=? AND account=?",
        [&f.project, &f.owner.account_id],
    )?;
    let mut proof = p.lock_proof.clone();
    proof["lockId"] = json!(new_id());
    f.acquire(json!({"requestId": new_id(), "taskId": next, "previousExecutorGeneration": 0, "executorTokenHash": sha256_hex(&p.token), "lockProof": proof}), &p.token)
        .await?
        .expect_code("execution_denied")?;
    Ok(())
}

#[tokio::test]
async fn committed_result_survives_lost_ack_revocation_and_offline_history_without_granting_dispatch(
) -> Result<()> {
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    let p = prepared(&f).await?;
    commit(&f, &p).await?;
    let content = json!({"text": "Saved answer"});
    let (digest, reference) = result_ref(&p.task, &p.action, &content);
    let mut body = report(&p, "acknowledged", "known", 2);
    body["resultDigest"] = json!(digest);
    body["resultRef"] = reference;
    body["resultContent"] = content.clone();
    f.db().execute(
        "UPDATE project_members SET deleted=1 WHERE project=? AND account=?",
        [&f.project, &f.owner.account_id],
    )?;
    let saved = reconcile(&f, &p, body.clone()).await?.expect_ok()?;
    ensure!(
        saved.data()["actionState"] == "acknowledged",
        "action state: {}",
        saved.body
    );
    ensure!(
        saved.data()["dispatchable"] == false,
        "dispatchable: {}",
        saved.body
    );
    ensure!(
        reconcile(&f, &p, body.clone()).await?.body == saved.body,
        "reconcile receipt"
    );
    let mut changed = body.clone();
    changed["resultContent"] = json!({"text": "Changed"});
    reconcile(&f, &p, changed)
        .await?
        .expect_code("result_digest_mismatch")?;
    f.lookup(&f.owner, &p.task)
        .await?
        .expect_code("resource_not_found")?;
    f.db().execute(
        "UPDATE project_members SET deleted=0 WHERE project=? AND account=?",
        [&f.project, &f.owner.account_id],
    )?;
    let detail = lookup_data(&f, &f.owner, &p.task).await?;
    ensure!(
        detail["results"][0]["content"] == content,
        "result: {detail}"
    );
    let context = f
        .task_op("context", &p.task, json!({"requestId": new_id(), "ownership": p.ownership, "expectedTaskRevision": saved.receipt()["recordRevision"]}), &p.token)
        .await?
        .expect_ok()?;
    ensure!(
        context.data()["snapshot"]["toolJournal"]
            .as_array()
            .map(Vec::len)
            == Some(1),
        "tool journal: {}",
        context.body
    );
    Ok(())
}

#[tokio::test]
async fn not_sent_and_uncertain_reports_require_verifiable_evidence_and_cannot_resurrect_cancelled_work(
) -> Result<()> {
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    let p = prepared(&f).await?;
    commit(&f, &p).await?;
    let base = report(&p, "not_sent", "none", 2);
    reconcile(&f, &p, base.clone())
        .await?
        .expect_code("stop_evidence_required")?;
    reconcile(
        &f,
        &p,
        with(
            base.clone(),
            json!({"noMarkerEvidence": {}, "stopEvidence": {}}),
        ),
    )
    .await?
    .expect_code("invalid_stop_evidence")?;
    reconcile(
        &f,
        &p,
        with(
            base.clone(),
            json!({"state": "uncertain", "effectStatus": "unknown"}),
        ),
    )
    .await?
    .expect_code("uncertain_reason_required")?;
    f.db().execute(
        "UPDATE tasks SET state='cancelled',revision=revision+1 WHERE id=?",
        [&p.task],
    )?;
    let saved = reconcile(&f, &p, with(base, json!({"state": "uncertain", "effectStatus": "unknown", "uncertainReason": "Provider response lost"})))
        .await?
        .expect_ok()?;
    ensure!(
        saved.data()["taskState"] == "cancelled",
        "task state: {}",
        saved.body
    );
    ensure!(
        saved.data()["actionState"] == "uncertain",
        "action state: {}",
        saved.body
    );
    Ok(())
}

#[tokio::test]
async fn signed_no_marker_evidence_reconciles_original_owner_before_acquire_and_altered_proofs_fail_closed(
) -> Result<()> {
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    let p = prepared(&f).await?;
    commit(&f, &p).await?;
    let entry = json!({"taskId": p.task, "actionId": p.action, "sequence": 0, "executorGeneration": 1, "actionKind": "read",
        "payloadDigest": p.payload_digest, "intentDigest": p.commit["intentDigest"], "grantRevision": 1, "state": "not_sent",
        "effectStatus": "none", "journalSequence": 2});
    let entry_bytes =
        journal_entry_body(&p.ownership["authorityGeneration"], &f.home, 1, &entry).to_string();
    let no_marker = json!({"entry": entry, "signature": f.signing.sign(entry_bytes.as_bytes())});
    let stop = json!({"v": 1, "sourceAuthority": p.ownership["authorityGeneration"], "taskId": p.task, "actionId": p.action,
        "sourceDeviceId": f.home, "sourceDeviceGeneration": 1, "executorGeneration": 1, "bootId": p.lock_proof["bootId"],
        "processId": p.lock_proof["processId"], "processStart": p.lock_proof["processStart"], "lockId": p.lock_proof["lockId"],
        "observationBootId": p.lock_proof["bootId"], "observation": "process_exited", "children": [], "observedAt": now_millis()?,
        "journalDigest": sha256_hex(&entry_bytes)});
    let stop_evidence = with(
        stop.clone(),
        json!({"signature": f.signing.sign_json(&stop_evidence_body(&stop))}),
    );
    let body = with(
        report(&p, "not_sent", "none", 2),
        json!({"noMarkerEvidence": no_marker, "stopEvidence": stop_evidence}),
    );
    reconcile(
        &f,
        &p,
        with(
            body.clone(),
            json!({"stopEvidence": with(stop_evidence.clone(), json!({"processStart": "1000"}))}),
        ),
    )
    .await?
    .expect_code("stop_evidence_invalid")?;
    let altered = with(
        no_marker.clone(),
        json!({"entry": with(entry.clone(), json!({"intentDigest": "0".repeat(64)}))}),
    );
    reconcile(
        &f,
        &p,
        with(body.clone(), json!({"noMarkerEvidence": altered})),
    )
    .await?
    .expect_code("stop_evidence_invalid")?;
    let accepted = reconcile(&f, &p, body.clone()).await?.expect_ok()?;
    ensure!(
        accepted.data()["actionState"] == "not_sent",
        "action state: {}",
        accepted.body
    );
    ensure!(
        reconcile(&f, &p, body.clone()).await?.body == accepted.body,
        "evidence receipt"
    );
    reconcile(&f, &p, with(body, json!({"stopEvidence": with(stop_evidence, json!({"observedAt": now_millis()? + 1_000_000}))})))
        .await?
        .expect_code("payload_conflict")?;
    let acquire = json!({"requestId": new_id(), "taskId": p.task, "previousExecutorGeneration": 1, "executorTokenHash": sha256_hex(&p.token),
        "lockProof": with(p.lock_proof.clone(), json!({"lockId": new_id(), "reconciledThrough": 3}))});
    f.acquire(acquire.clone(), &p.token)
        .await?
        .expect_code("ownership_conflict")?;
    let mut next = acquire.clone();
    next["lockProof"]["reconciledThrough"] = json!(2);
    let next = f.acquire(next, &p.token).await?.expect_ok()?;
    ensure!(
        next.data()["ownership"]["executorGeneration"] == 2,
        "generation: {}",
        next.body
    );
    f.task_op("context", &p.task, json!({"requestId": new_id(), "ownership": p.ownership, "expectedTaskRevision": next.receipt()["recordRevision"]}), &p.token)
        .await?
        .expect_code("ownership_conflict")?;
    Ok(())
}

#[tokio::test]
async fn progress_reports_advance_monotonically_immutable_result_publishes_once_and_prepare_sees_changed_context(
) -> Result<()> {
    {
        let f = TaskFixture::new(&["transfer_task"], None).await?;
        let p = prepared(&f).await?;
        f.edit_agent_rules(&f.owner, &f.project, &f.agent, json!({"requestId": new_id(), "expectedParticipationRevision": 1, "rules": "New rules"}))
            .await?
            .expect_ok()?;
        f.commit(&p.task, &p.action, p.commit.clone(), &p.token)
            .await?
            .expect_code("context_stale")?;
    }
    let g = TaskFixture::new(&["transfer_task"], None).await?;
    let p = prepared(&g).await?;
    commit(&g, &p).await?;
    let progress = report(&p, "in_flight", "unknown", 2);
    let saved = reconcile(&g, &p, progress.clone()).await?.expect_ok()?;
    ensure!(
        saved.data()["actionState"] == "in_flight",
        "progress: {}",
        saved.body
    );
    let content = json!({"text": "Done"});
    let (digest, reference) = result_ref(&p.task, &p.action, &content);
    let published_body = with(
        progress.clone(),
        json!({"requestId": new_id(), "state": "reported", "effectStatus": "known",
        "journalSequence": 3, "resultContent": content, "resultDigest": digest, "resultRef": reference}),
    );
    let published = reconcile(&g, &p, published_body.clone())
        .await?
        .expect_ok()?;
    ensure!(
        reconcile(&g, &p, progress.clone()).await?.body == saved.body,
        "progress receipt"
    );
    ensure!(
        reconcile(&g, &p, published_body.clone()).await?.body == published.body,
        "result receipt"
    );
    let acknowledged = reconcile(
        &g,
        &p,
        with(
            published_body.clone(),
            json!({"requestId": new_id(), "state": "acknowledged", "journalSequence": 4}),
        ),
    )
    .await?
    .expect_ok()?;
    ensure!(
        acknowledged.data()["actionState"] == "acknowledged",
        "ack: {}",
        acknowledged.body
    );
    reconcile(
        &g,
        &p,
        with(
            published_body,
            json!({"requestId": new_id(), "journalSequence": 5}),
        ),
    )
    .await?
    .expect_code("action_conflict")?;
    reconcile(
        &g,
        &p,
        with(
            progress,
            json!({"requestId": new_id(), "journalSequence": 5}),
        ),
    )
    .await?
    .expect_code("payload_conflict")?;
    Ok(())
}

#[tokio::test]
async fn task_controls_use_current_scope_and_revisions_keep_cancellation_terminal_and_resolve_no_effects_implicitly(
) -> Result<()> {
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    let p = prepared(&f).await?;
    let pause = json!({"requestId": new_id(), "expectedRevision": p.commit["expectedTaskRevision"], "operation": "pause"});
    let paused = f
        .control(&f.owner, &p.task, pause.clone())
        .await?
        .expect_ok()?;
    ensure!(paused.data()["state"] == "paused", "pause: {}", paused.body);
    ensure!(
        f.control(&f.owner, &p.task, pause.clone())
            .await?
            .expect_ok()?
            .data()
            == paused.data(),
        "pause receipt"
    );
    f.commit(&p.task, &p.action, p.commit.clone(), &p.token)
        .await?
        .expect_code("task_not_runnable")?;
    f.control(
        &f.foreign,
        &p.task,
        with(pause, json!({"requestId": new_id()})),
    )
    .await?
    .expect_code("resource_not_found")?;
    let cancelled = f
        .control(&f.owner, &p.task, json!({"requestId": new_id(), "expectedRevision": paused.receipt()["recordRevision"], "operation": "cancel"}))
        .await?
        .expect_ok()?;
    ensure!(
        cancelled.data()["state"] == "cancelled",
        "cancel: {}",
        cancelled.body
    );
    f.control(&f.owner, &p.task, json!({"requestId": new_id(), "expectedRevision": cancelled.receipt()["recordRevision"], "operation": "resume"}))
        .await?
        .expect_code("task_terminal")?;
    Ok(())
}

#[tokio::test]
async fn owner_risk_resolution_reserves_one_replacement_and_cannot_erase_unknown_original_effect(
) -> Result<()> {
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    let p = prepared(&f).await?;
    commit(&f, &p).await?;
    let uncertain = reconcile(
        &f,
        &p,
        with(
            report(&p, "uncertain", "unknown", 2),
            json!({"uncertainReason": "Response lost"}),
        ),
    )
    .await?
    .expect_ok()?;
    let decision = json!({"requestId": new_id(), "expectedRevision": uncertain.receipt()["recordRevision"], "operation": "resolve_uncertain",
        "decision": {"kind": "retry_with_risk", "actionId": p.action, "acknowledgement": "effect_may_have_happened", "reason": "Owner checked service"}});
    let resolved = f
        .control(&f.owner, &p.task, decision.clone())
        .await?
        .expect_ok()?;
    ensure!(
        resolved.data()["replacementActionId"] == decision["requestId"],
        "replacement: {}",
        resolved.body
    );
    ensure!(
        f.control(&f.owner, &p.task, decision.clone())
            .await?
            .expect_ok()?
            .data()
            == resolved.data(),
        "resolution receipt"
    );
    f.control(
        &f.owner,
        &p.task,
        with(decision.clone(), json!({"requestId": new_id()})),
    )
    .await?
    .expect_code("resolution_conflict")?;
    ensure!(
        field(
            &f,
            "SELECT state FROM task_actions WHERE id=?",
            [&p.action],
            "state"
        )? == "uncertain",
        "original effect erased"
    );
    let deleted = f.db().execute(
        "DELETE FROM task_action_resolutions WHERE action=?",
        [&p.action],
    );
    ensure!(
        deleted.is_err_and(|error| error.to_string().contains("immutable task resolution")),
        "resolution deletable"
    );
    let acquired = f
        .acquire(json!({"requestId": new_id(), "taskId": p.task, "previousExecutorGeneration": 1, "executorTokenHash": sha256_hex(&p.token),
            "lockProof": with(p.lock_proof.clone(), json!({"lockId": new_id(), "reconciledThrough": 2}))}), &p.token)
        .await?
        .expect_ok()?;
    let ownership = acquired.data()["ownership"].clone();
    let revision = lookup_data(&f, &f.owner, &p.task).await?["revision"].clone();
    let context = f
        .task_op("context", &p.task, json!({"requestId": new_id(), "ownership": ownership, "expectedTaskRevision": revision}), &p.token)
        .await?
        .expect_ok()?;
    let reference = context.data()["snapshotRef"].clone();
    let action = decision["requestId"].as_str().ok_or("action")?.to_owned();
    let payload_digest = sha256_hex("new attempt");
    let intent = digest_command(&json!([
        p.task,
        action,
        1,
        ownership,
        payload_digest,
        "read",
        reference["snapshotId"],
        reference["digest"],
        null,
        null,
        1,
        null,
        null,
        decision["requestId"]
    ]));
    let prepared_action = f
        .prepare(&p.task, &action, json!({"requestId": new_id(), "sequence": 1, "ownership": ownership, "payloadDigest": payload_digest,
            "actionKind": "read", "contextSnapshotId": reference["snapshotId"], "contextDigest": reference["digest"], "grantRevision": 1,
            "expectedTaskRevision": revision, "journalSequence": 3, "resolutionRequestId": decision["requestId"], "intentDigest": intent}), &p.token)
        .await?
        .expect_ok()?;
    ensure!(
        prepared_action.data()["state"] == "prepared",
        "replacement prepare: {}",
        prepared_action.body
    );
    Ok(())
}

#[tokio::test]
async fn late_known_result_rejects_risk_retry_and_historical_accepted_receipt_stays_valid_after_a_later_ack(
) -> Result<()> {
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    let p = prepared(&f).await?;
    commit(&f, &p).await?;
    reconcile(
        &f,
        &p,
        with(
            report(&p, "uncertain", "unknown", 2),
            json!({"uncertainReason": "Response lost"}),
        ),
    )
    .await?
    .expect_ok()?;
    let content = json!({"text": "Late known output"});
    let (digest, reference) = result_ref(&p.task, &p.action, &content);
    let body = with(
        report(&p, "reported", "known", 3),
        json!({"resultContent": content, "resultDigest": digest, "resultRef": reference}),
    );
    let accepted = reconcile(&f, &p, body.clone()).await?.expect_ok()?;
    let acknowledged = reconcile(
        &f,
        &p,
        with(
            body,
            json!({"requestId": new_id(), "state": "acknowledged", "journalSequence": 4}),
        ),
    )
    .await?
    .expect_ok()?;
    f.control(&f.owner, &p.task, json!({"requestId": new_id(), "expectedRevision": acknowledged.receipt()["recordRevision"], "operation": "resolve_uncertain",
        "decision": {"kind": "retry_with_risk", "actionId": p.action, "acknowledgement": "effect_may_have_happened", "reason": "Should not replay known output"}}))
        .await?
        .expect_code("resolution_conflict")?;
    let resolved = f
        .control(&f.owner, &p.task, json!({"requestId": new_id(), "expectedRevision": acknowledged.receipt()["recordRevision"], "operation": "resolve_uncertain",
            "decision": {"kind": "known_result", "actionId": p.action, "evidenceRequestId": accepted.receipt()["requestId"], "resultRef": reference}}))
        .await?
        .expect_ok()?;
    ensure!(
        resolved.data()["state"] == "running",
        "resolution: {}",
        resolved.body
    );
    ensure!(
        resolved
            .data()
            .get("replacementActionId")
            .is_none_or(Value::is_null),
        "replacement reserved: {}",
        resolved.body
    );
    Ok(())
}

use minarbot_testkit::task_bridge::{lock_proof, random_token, tool_digest, Actor};

#[tokio::test]
async fn offline_task_admission_is_durable_scoped_immutable_and_exact_without_provider_binding(
) -> Result<()> {
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    let command = f.command(json!({}));
    let accepted = f.admit(&f.owner, &command).await?.expect_ok()?;
    ensure!(accepted.status == 202, "status: {}", accepted.status);
    ensure!(
        accepted.data()["state"] == "waiting_device",
        "state: {}",
        accepted.body
    );
    ensure!(
        accepted.data()["taskId"] == command["requestId"],
        "task id: {}",
        accepted.body
    );
    ensure!(
        f.admit(&f.owner, &command).await?.status == 200,
        "replay status"
    );
    f.admit(
        &f.owner,
        &with(command.clone(), json!({"payloadDigest": "0".repeat(64)})),
    )
    .await?
    .expect_code("payload_digest_mismatch")?;
    f.admit(&f.foreign, &command)
        .await?
        .expect_code("resource_not_found")?;
    f.admit(&f.member, &command)
        .await?
        .expect_code("execution_denied")?;
    let mut revisions = command["expectedRevisions"].clone();
    revisions["capability"] = json!(2);
    f.admit(
        &f.owner,
        &f.command(json!({"expectedRevisions": revisions})),
    )
    .await?
    .expect_code("stale_revision")?;
    f.admit(
        &f.owner,
        &with(command.clone(), json!({"secret": "forbidden"})),
    )
    .await?
    .expect_code("unknown_field")?;
    let mut payload = command["payload"].clone();
    payload["role"] = json!("admin");
    f.admit(
        &f.owner,
        &with(command.clone(), json!({"payload": payload})),
    )
    .await?
    .expect_code("unknown_field")?;
    f.db().execute(
        "UPDATE agent_projects SET revision=2 WHERE project=? AND agent=?",
        [&f.project, &f.agent],
    )?;
    ensure!(
        f.admit(&f.owner, &command).await?.status == 200,
        "replay after a revision change"
    );
    let task = command["requestId"].as_str().ok_or("task")?;
    ensure!(
        lookup_data(&f, &f.owner, task).await?["currentTaskId"] == task,
        "current task"
    );
    let mutated = f.db().execute(
        "UPDATE tasks SET home_device=? WHERE id=?",
        [&new_id(), &task.to_owned()],
    );
    ensure!(
        mutated.is_err_and(|error| error.to_string().contains("immutable task admission")),
        "admission mutable"
    );
    ensure!(
        field(&f, "SELECT count(*) AS n FROM turns", [], "n")? == 1,
        "turns"
    );
    ensure!(
        field(&f, "SELECT count(*) AS n FROM task_actions", [], "n")? == 0,
        "actions"
    );
    Ok(())
}

#[tokio::test]
async fn assigned_collaborator_executes_owner_home_binding_and_revoked_scope_blocks_receipt_replay(
) -> Result<()> {
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    f.db().execute(
        "INSERT INTO collaborator_agents VALUES (?,?,?,1,1)",
        [&f.project, &f.member.account_id, &f.agent],
    )?;
    f.db().execute(
        "INSERT INTO response_policies VALUES (?,?,?,1,1)",
        [&f.project, &f.member.account_id, &f.agent],
    )?;
    let command = f.command(json!({}));
    ensure!(
        f.admit(&f.member, &command).await?.expect_ok()?.status == 202,
        "collaborator admission"
    );
    f.db().execute(
        "UPDATE project_members SET deleted=1,grant_revision=2 WHERE project=? AND account=?",
        [&f.project, &f.member.account_id],
    )?;
    f.admit(&f.member, &command)
        .await?
        .expect_code("resource_not_found")?;
    f.lookup(&f.member, command["requestId"].as_str().ok_or("task")?)
        .await?
        .expect_code("resource_not_found")?;
    Ok(())
}

#[tokio::test]
async fn task_messages_require_files_preserve_line_breaks_and_closed_union_rejects_mixed_routing(
) -> Result<()> {
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    let command = f.command(json!({"payload": {"v": 1, "kind": "direct", "message": {"text": "First\nSecond\tIndented", "files": []}}}));
    ensure!(
        f.admit(&f.owner, &command).await?.expect_ok()?.status == 202,
        "line breaks"
    );
    f.admit(
        &f.owner,
        &f.command(json!({"payload": {"v": 1, "kind": "direct", "message": {"text": "No files"}}})),
    )
    .await?
    .expect_code("invalid_attachments")?;
    let mut revisions = command["expectedRevisions"].clone();
    revisions["channel"] = json!(1);
    f.admit(&f.owner, &f.command(json!({"payload": {"v": 1, "kind": "channel", "channelId": new_id(), "threadId": new_id(), "routing": "assigned",
        "message": {"text": "Mixed", "files": []}}, "expectedRevisions": revisions})))
        .await?
        .expect_code("invalid_task_payload")?;
    f.admit(&f.owner, &f.command(json!({"payload": {"v": 1, "kind": "direct", "message": {"text": "Foreign attachment", "files": [new_id()]}}})))
        .await?
        .expect_code("resource_not_found")?;
    ensure!(
        field(&f, "SELECT count(*) AS n FROM tasks", [], "n")? == 1,
        "refused admissions stored"
    );
    Ok(())
}

#[tokio::test]
async fn task_admission_requires_scoped_conversation_matching_revisions_and_exact_attachment_conversation(
) -> Result<()> {
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    let thread = new_id();
    let channel = new_id();
    f.db().execute(
        "INSERT INTO threads(id,agent,name) VALUES (?,?,?)",
        [&thread, &f.agent, &"Side".to_owned()],
    )?;
    f.db().execute(
        "INSERT INTO channels(id,name,instructions,lead) VALUES (?,?,?,?)",
        [
            &channel,
            &"Room".to_owned(),
            &"Room rules".to_owned(),
            &f.agent,
        ],
    )?;
    f.db().execute(
        "INSERT INTO channel_members VALUES (?,?)",
        [&channel, &f.agent],
    )?;
    let direct = f.command(json!({}));
    let mut thread_revisions = direct["expectedRevisions"].clone();
    thread_revisions["thread"] = json!(1);
    let side = f.command(
        json!({"payload": side_payload(&thread, "Private"), "expectedRevisions": thread_revisions}),
    );
    f.admit(&f.owner, &side)
        .await?
        .expect_code("resource_not_found")?;
    f.db().execute(
        "INSERT INTO record_scopes VALUES ('threads',?,?,?,'owner','thread',?)",
        [
            json!([thread]).to_string(),
            f.owner.account_id.clone(),
            f.project.clone(),
            thread.clone(),
        ],
    )?;
    ensure!(
        f.admit(&f.owner, &side).await?.expect_ok()?.status == 202,
        "scoped side admission"
    );
    ensure!(
        field(
            &f,
            "SELECT thread FROM turns WHERE id=?",
            [side["requestId"].as_str().unwrap_or_default()],
            "thread"
        )? == thread.as_str(),
        "turn thread"
    );
    let mut stale = side["expectedRevisions"].clone();
    stale["thread"] = json!(2);
    f.admit(
        &f.owner,
        &f.command(json!({"payload": side["payload"], "expectedRevisions": stale})),
    )
    .await?
    .expect_code("stale_revision")?;
    let mut channel_revisions = direct["expectedRevisions"].clone();
    channel_revisions["channel"] = json!(1);
    let room = f.command(json!({"payload": {"v": 1, "kind": "channel", "channelId": channel, "routing": "automatic", "message": {"text": "Room", "files": []}},
        "expectedRevisions": channel_revisions}));
    f.db().execute(
        "INSERT INTO record_scopes VALUES ('channels',?,?,?,'project','channel',?)",
        [
            json!([channel]).to_string(),
            f.owner.account_id.clone(),
            f.project.clone(),
            channel.clone(),
        ],
    )?;
    ensure!(
        f.admit(&f.owner, &room).await?.expect_ok()?.status == 202,
        "room admission"
    );
    f.db().execute(
        "UPDATE channels SET archived=1,revision=2 WHERE id=?",
        [&channel],
    )?;
    ensure!(
        f.admit(&f.owner, &room).await?.status == 200,
        "archived room replay"
    );
    f.admit(
        &f.owner,
        &f.command(
            json!({"payload": room["payload"], "expectedRevisions": room["expectedRevisions"]}),
        ),
    )
    .await?
    .expect_code("conversation_archived")?;
    Ok(())
}

#[tokio::test]
async fn provider_provenance_is_captured_from_owner_same_home_binding_and_never_caller_fields(
) -> Result<()> {
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    let binding = new_id();
    f.db().execute(
        "INSERT INTO provider_bindings VALUES (?,?,?,'openai-compatible','fixture','Fixture','ready',1)",
        [&binding, &f.home, &f.owner.account_id],
    )?;
    f.db().execute(
        "UPDATE agent_authority SET provider_binding=? WHERE agent=?",
        [&binding, &f.agent],
    )?;
    let command = f.command(json!({}));
    f.admit(&f.owner, &command).await?.expect_ok()?;
    let task = one(&f, "SELECT provider_binding_id,provider_binding_revision,provider_binding_owner FROM tasks WHERE id=?", [command["requestId"].as_str().unwrap_or_default()])?;
    ensure!(
        task["provider_binding_id"] == binding.as_str(),
        "binding: {task}"
    );
    ensure!(
        task["provider_binding_revision"] == 1,
        "binding revision: {task}"
    );
    ensure!(
        task["provider_binding_owner"] == f.owner.account_id.as_str(),
        "binding owner: {task}"
    );
    f.db().execute(
        "UPDATE provider_bindings SET owner_account=? WHERE id=?",
        [&f.foreign.account_id, &binding],
    )?;
    f.admit(&f.owner, &f.command(json!({})))
        .await?
        .expect_code("resource_not_found")?;
    ensure!(f.admit(&f.owner, &command).await?.status == 200, "receipt");
    Ok(())
}

#[tokio::test]
async fn acquire_requires_stopped_reconciled_owner_exact_token_and_fenced_generation() -> Result<()>
{
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    let command = f.command(json!({}));
    f.admit(&f.owner, &command).await?.expect_ok()?;
    let token = random_token()?;
    let proof = lock_proof();
    let acquire = json!({"requestId": new_id(), "taskId": command["requestId"], "previousExecutorGeneration": 0,
        "executorTokenHash": sha256_hex(&token), "lockProof": proof});
    let owned = f.acquire(acquire.clone(), &token).await?.expect_ok()?;
    ensure!(
        owned.data()["ownership"]["executorGeneration"] == 1,
        "generation: {}",
        owned.body
    );
    ensure!(
        f.acquire(acquire.clone(), &token)
            .await?
            .expect_ok()?
            .data()
            == owned.data(),
        "acquire receipt"
    );
    f.acquire(
        with(acquire.clone(), json!({"requestId": new_id()})),
        &token,
    )
    .await?
    .expect_code("ownership_conflict")?;
    f.acquire(
        with(
            acquire.clone(),
            json!({"requestId": new_id(), "previousExecutorGeneration": 1,
        "lockProof": with(proof.clone(), json!({"previousOwnerStopped": false}))}),
        ),
        &token,
    )
    .await?
    .expect_code("ownership_conflict")?;
    f.acquire(acquire.clone(), &random_token()?)
        .await?
        .expect_code("executor_invalid")?;
    let successor = random_token()?;
    let next = f
        .acquire(with(acquire, json!({"requestId": new_id(), "previousExecutorGeneration": 1, "executorTokenHash": sha256_hex(&successor),
            "lockProof": with(proof, json!({"lockId": new_id()}))})), &successor)
        .await?
        .expect_ok()?;
    ensure!(
        next.data()["ownership"]["executorGeneration"] == 2,
        "successor generation: {}",
        next.body
    );
    ensure!(
        !owned.body.to_string().contains(&token),
        "receipt leaked the executor token"
    );
    Ok(())
}

#[tokio::test]
async fn context_snapshot_is_immutable_exact_project_scoped_and_requires_current_executor_token(
) -> Result<()> {
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    let c = f.command(json!({}));
    f.admit(&f.owner, &c).await?.expect_ok()?;
    let task = c["requestId"].as_str().ok_or("task")?.to_owned();
    let (token, ownership, revision) = f.acquire_task(&task).await?;
    let command =
        json!({"requestId": new_id(), "ownership": ownership, "expectedTaskRevision": revision});
    let context = f
        .task_op("context", &task, command.clone(), &token)
        .await?
        .expect_ok()?;
    ensure!(
        f.task_op("context", &task, command.clone(), &token)
            .await?
            .expect_ok()?
            .data()
            == context.data(),
        "context receipt"
    );
    let snapshot = &context.data()["snapshot"];
    ensure!(
        snapshot["projectId"] == f.project.as_str(),
        "project: {snapshot}"
    );
    ensure!(
        snapshot["profile"]["instructions"] == "Research",
        "instructions: {snapshot}"
    );
    ensure!(
        context.data()["snapshotRef"]["digest"] == sha256_hex(canonical_json(snapshot)).as_str(),
        "snapshot digest"
    );
    f.task_op(
        "context",
        &task,
        with(command, json!({"requestId": new_id()})),
        &random_token()?,
    )
    .await?
    .expect_code("executor_invalid")?;
    let mutated = f
        .db()
        .execute("UPDATE task_context_snapshots SET snapshot_json='{}'", []);
    ensure!(
        mutated.is_err_and(|error| error.to_string().contains("immutable task context")),
        "snapshot mutable"
    );
    Ok(())
}

#[tokio::test]
async fn action_prepare_grants_no_dispatch_commit_has_one_live_grant_and_stale_context_blocks_new_work(
) -> Result<()> {
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    let c = f.command(json!({}));
    f.admit(&f.owner, &c).await?.expect_ok()?;
    let task = c["requestId"].as_str().ok_or("task")?.to_owned();
    let (token, ownership, revision) = f.acquire_task(&task).await?;
    let context = f
        .task_op("context", &task, json!({"requestId": new_id(), "ownership": ownership, "expectedTaskRevision": revision}), &token)
        .await?
        .expect_ok()?;
    let reference = context.data()["snapshotRef"].clone();
    let action = new_id();
    let digest = sha256_hex("compute");
    let intent = digest_command(&json!([
        task,
        action,
        0,
        ownership,
        digest,
        "compute",
        reference["snapshotId"],
        reference["digest"],
        null,
        null,
        1,
        null,
        null,
        null
    ]));
    let prepare = json!({"requestId": new_id(), "sequence": 0, "ownership": ownership, "payloadDigest": digest, "actionKind": "compute",
        "contextSnapshotId": reference["snapshotId"], "contextDigest": reference["digest"], "grantRevision": 1,
        "expectedTaskRevision": revision, "journalSequence": 1, "intentDigest": intent});
    let prepared_action = f
        .prepare(&task, &action, prepare.clone(), &token)
        .await?
        .expect_ok()?;
    ensure!(
        prepared_action.data()["state"] == "prepared",
        "prepare: {}",
        prepared_action.body
    );
    ensure!(
        prepared_action.data().get("dispatchable").is_none(),
        "prepare granted dispatch: {}",
        prepared_action.body
    );
    ensure!(
        f.prepare(&task, &action, prepare, &token)
            .await?
            .expect_ok()?
            .data()
            == prepared_action.data(),
        "prepare receipt"
    );
    let reacquire = || json!({"requestId": new_id(), "taskId": task, "previousExecutorGeneration": 1, "executorTokenHash": sha256_hex(&token), "lockProof": lock_proof()});
    f.acquire(reacquire(), &token)
        .await?
        .expect_code("ownership_conflict")?;
    let commit = json!({"requestId": new_id(), "ownership": ownership, "intentDigest": intent, "expectedTaskRevision": revision, "journalSequence": 1});
    let committed = f
        .commit(&task, &action, commit.clone(), &token)
        .await?
        .expect_ok()?;
    ensure!(
        committed.data()["dispatchable"] == true,
        "commit: {}",
        committed.body
    );
    ensure!(
        f.commit(&task, &action, commit.clone(), &token)
            .await?
            .expect_ok()?
            .data()["dispatchable"]
            == false,
        "commit replay dispatchable"
    );
    let stored: Value = serde_json::from_str(
        field(
            &f,
            "SELECT result_json FROM receipts WHERE request_id=?",
            [commit["requestId"].as_str().unwrap_or_default()],
            "result_json",
        )?
        .as_str()
        .ok_or("receipt")?,
    )?;
    ensure!(
        stored["data"]["dispatchable"] == false,
        "stored receipt dispatchable: {stored}"
    );
    f.commit(
        &task,
        &action,
        with(commit, json!({"requestId": new_id()})),
        &token,
    )
    .await?
    .expect_code("action_conflict")?;
    f.acquire(reacquire(), &token)
        .await?
        .expect_code("ownership_conflict")?;
    Ok(())
}

struct Transfer {
    task: String,
    recipient: String,
    token: String,
    ownership: Value,
    action: String,
    commit: Value,
    mutation: Value,
}

async fn canonical_transfer(f: &TaskFixture) -> Result<Transfer> {
    let recipient = new_id();
    f.human(
        minarbot_testkit::http::Method::POST,
        "/api/agents",
        Some(agent_body(&recipient, "Recipient", f)),
        &f.owner,
    )
    .await?
    .expect_ok()?;
    let channel = new_id();
    f.db().execute(
        "INSERT INTO channels(id,name,instructions,lead) VALUES (?,?,?,?)",
        [
            &channel,
            &"Transfer".to_owned(),
            &"Project work".to_owned(),
            &f.agent,
        ],
    )?;
    for agent in [&f.agent, &recipient] {
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
    let command = f.command(json!({"payload": channel_payload(&channel, "Research this"),
        "expectedRevisions": {"project": 1, "agent": 1, "homeBinding": 1, "membership": 1, "assignment": 1, "capability": 1, "channel": 1}}));
    f.admit(&f.owner, &command).await?.expect_ok()?;
    let task = command["requestId"].as_str().ok_or("task")?.to_owned();
    let (token, ownership, revision) = f.acquire_task(&task).await?;
    let context = f
        .task_op("context", &task, json!({"requestId": new_id(), "ownership": ownership, "expectedTaskRevision": revision}), &token)
        .await?
        .expect_ok()?;
    let reference = context.data()["snapshotRef"].clone();
    let action = new_id();
    let tool_call = json!({"id": "transfer", "function": {"name": "transfer_task",
        "arguments": json!({"recipient": recipient, "task": "Continue from saved evidence"}).to_string()}});
    let expected = json!(revision.as_i64().unwrap_or(0) + 1);
    let payload_digest = tool_digest(&tool_call, &expected, None);
    let intent = digest_command(&json!([
        task,
        action,
        0,
        ownership,
        payload_digest,
        "canonical",
        reference["snapshotId"],
        reference["digest"],
        null,
        null,
        1,
        null,
        null,
        null
    ]));
    f.prepare(&task, &action, json!({"requestId": new_id(), "sequence": 0, "ownership": ownership, "payloadDigest": payload_digest,
        "actionKind": "canonical", "contextSnapshotId": reference["snapshotId"], "contextDigest": reference["digest"], "grantRevision": 1,
        "expectedTaskRevision": revision, "journalSequence": 1, "intentDigest": intent}), &token)
        .await?
        .expect_ok()?;
    let commit = json!({"requestId": new_id(), "ownership": ownership, "intentDigest": intent, "expectedTaskRevision": revision, "journalSequence": 1});
    let mutation = json!({"toolCall": tool_call, "expectedRevision": expected, "requestId": new_id(), "actionId": action,
        "ownership": ownership, "payloadDigest": payload_digest});
    Ok(Transfer {
        task,
        recipient,
        token,
        ownership,
        action,
        commit,
        mutation,
    })
}

#[tokio::test]
async fn canonical_transfer_commits_one_successor_and_immutable_result_fences_old_executor_and_retries_without_mutation(
) -> Result<()> {
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    let t = canonical_transfer(&f).await?;
    let wrong = with(
        t.mutation.clone(),
        json!({"expectedRevision": 2, "payloadDigest": tool_digest(&t.mutation["toolCall"], &json!(2), None)}),
    );
    f.task_op("context-mutations", &t.task, wrong, &t.token)
        .await?
        .expect_code("action_conflict")?;
    f.commit(&t.task, &t.action, t.commit.clone(), &t.token)
        .await?
        .expect_ok()?;
    let result = f
        .task_op("context-mutations", &t.task, t.mutation.clone(), &t.token)
        .await?
        .expect_ok()?;
    let content = result_content(&result);
    let successor = content["taskId"].as_str().ok_or("successor")?.to_owned();
    ensure!(successor != t.task, "successor reuses the task");
    ensure!(
        content["homeDeviceId"] == f.home.as_str(),
        "home: {content}"
    );
    let original = lookup_data(&f, &f.owner, &t.task).await?;
    ensure!(
        original["state"] == "transferred" && original["currentTaskId"] == successor.as_str(),
        "original: {original}"
    );
    ensure!(
        lookup_data(&f, &f.owner, &successor).await?["agentId"] == t.recipient.as_str(),
        "successor agent"
    );
    let payload: Value = serde_json::from_str(
        field(
            &f,
            "SELECT payload_json FROM tasks WHERE id=?",
            [&successor],
            "payload_json",
        )?
        .as_str()
        .ok_or("payload")?,
    )?;
    ensure!(
        payload["message"]["text"] == "Continue from saved evidence",
        "successor payload: {payload}"
    );
    ensure!(
        f.task_op("context-mutations", &t.task, t.mutation.clone(), &t.token)
            .await?
            .body
            == result.body,
        "transfer receipt"
    );
    ensure!(
        field(
            &f,
            "SELECT count(*) n FROM tasks WHERE logical_turn=?",
            [&t.task],
            "n"
        )? == 2,
        "logical turn tasks"
    );
    ensure!(
        field(
            &f,
            "SELECT count(*) n FROM task_results WHERE action=?",
            [&t.action],
            "n"
        )? == 1,
        "results"
    );
    f.task_op(
        "context-mutations",
        &t.task,
        with(t.mutation.clone(), json!({"actionId": new_id()})),
        &t.token,
    )
    .await?
    .expect_code("payload_conflict")?;
    f.task_op(
        "context-mutations",
        &t.task,
        t.mutation.clone(),
        &"x".repeat(43),
    )
    .await?
    .expect_code("executor_invalid")?;
    let mut stranger = f.principal.clone();
    stranger.device_id = new_id();
    let forged = f
        .request(
            minarbot_testkit::http::Method::POST,
            &format!("/api/tasks/{}/context-mutations", t.task),
            Some(t.mutation.clone()),
            Actor::Device(&stranger),
            Some(&t.token),
            None,
        )
        .await?;
    ensure!(!forged.ok(), "foreign device accepted: {}", forged.body);
    f.task_op(
        "context-mutations",
        &t.task,
        with(t.mutation.clone(), json!({"requestId": new_id()})),
        &t.token,
    )
    .await?
    .expect_code("ownership_conflict")?;
    f.db().execute(
        "UPDATE project_members SET deleted=1 WHERE project=? AND account=?",
        [&f.project, &f.owner.account_id],
    )?;
    f.task_op("context-mutations", &t.task, t.mutation.clone(), &t.token)
        .await?
        .expect_code("execution_denied")?;
    Ok(())
}

#[tokio::test]
async fn canonical_rpc_rejects_changed_input_stale_grants_and_fabricated_results_without_partial_transfer(
) -> Result<()> {
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    let t = canonical_transfer(&f).await?;
    let committed = f
        .commit(&t.task, &t.action, t.commit.clone(), &t.token)
        .await?
        .expect_ok()?;
    let mut changed_call = t.mutation["toolCall"].clone();
    changed_call["function"]["arguments"] =
        json!(json!({"recipient": t.recipient, "task": "Changed"}).to_string());
    f.task_op(
        "context-mutations",
        &t.task,
        with(t.mutation.clone(), json!({"toolCall": changed_call})),
        &t.token,
    )
    .await?
    .expect_code("payload_digest_mismatch")?;
    let digest = tool_digest(&changed_call, &t.mutation["expectedRevision"], None);
    f.task_op(
        "context-mutations",
        &t.task,
        with(
            t.mutation.clone(),
            json!({"toolCall": changed_call, "payloadDigest": digest}),
        ),
        &t.token,
    )
    .await?
    .expect_code("action_conflict")?;
    let content = json!({"text": "Forged"});
    let (digest, reference) = result_ref(&t.task, &t.action, &content);
    f.task_op("reconcile", &t.task, json!({"requestId": new_id(), "actionId": t.action, "ownership": t.ownership,
        "payloadDigest": t.mutation["payloadDigest"], "state": "reported", "effectStatus": "known", "journalSequence": 2,
        "resultContent": content, "resultDigest": digest, "resultRef": reference}), &t.token)
        .await?
        .expect_code("canonical_result_required")?;
    f.edit_agent_rules(&f.owner, &f.project, &f.agent, json!({"requestId": new_id(), "expectedParticipationRevision": 1, "rules": "Changed after commit"}))
        .await?
        .expect_ok()?;
    f.task_op("context-mutations", &t.task, t.mutation.clone(), &t.token)
        .await?
        .expect_code("context_stale")?;
    ensure!(
        lookup_data(&f, &f.owner, &t.task).await?["revision"]
            == committed.receipt()["recordRevision"],
        "task revision moved"
    );
    ensure!(
        field(&f, "SELECT count(*) n FROM task_results", [], "n")? == 0,
        "partial result"
    );
    ensure!(
        field(&f, "SELECT count(*) n FROM tasks", [], "n")? == 1,
        "partial transfer"
    );
    ensure!(
        field(
            &f,
            "SELECT stopped FROM execution_owners WHERE task=?",
            [&t.task],
            "stopped"
        )? == 0,
        "executor stopped"
    );
    Ok(())
}

#[tokio::test]
async fn canonical_transfer_keeps_channel_only_tool_boundary_for_private_tasks() -> Result<()> {
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    let p = prepared(&f).await?;
    let tool_call = json!({"id": "transfer", "function": {"name": "transfer_task", "arguments": json!({"recipient": new_id(), "task": "Handoff"}).to_string()}});
    let digest = tool_digest(&tool_call, &json!(2), None);
    f.task_op("context-mutations", &p.task, json!({"toolCall": tool_call, "expectedRevision": 2, "requestId": new_id(), "actionId": new_id(),
        "ownership": p.ownership, "payloadDigest": digest}), &p.token)
        .await?
        .expect_code("execution_denied")?;
    ensure!(
        field(&f, "SELECT count(*) n FROM tasks", [], "n")? == 1,
        "private transfer created a task"
    );
    Ok(())
}

/// `initializeMemory` + `rememberMemory(db, scope, {text})`: a new automatic
/// note; returns its id.
fn remember(f: &TaskFixture, scope: &str, text: &str) -> Result<String> {
    f.db().execute_batch(
        "CREATE TABLE IF NOT EXISTS memories(scope TEXT NOT NULL,id TEXT NOT NULL,text TEXT NOT NULL,essential INTEGER NOT NULL,revision INTEGER NOT NULL,removed INTEGER NOT NULL,PRIMARY KEY(scope,id)) STRICT;
         CREATE TABLE IF NOT EXISTS memory_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;",
    )?;
    let columns = f.db().rows("PRAGMA table_info(memories)", [])?;
    if !columns.iter().any(|column| column["name"] == "selection_locked") {
        f.db().execute_batch("ALTER TABLE memories ADD COLUMN selection_locked INTEGER NOT NULL DEFAULT 1 CHECK(selection_locked IN (0,1))")?;
    }
    let id = new_id();
    f.db().execute(
        "INSERT INTO memories(scope,id,text,essential,revision,removed,selection_locked) VALUES (?,?,?,0,1,0,0)",
        [scope, &id, text],
    )?;
    Ok(id)
}

/// `seedSkillVersion(db, id, {revision, name, description, content})`.
fn save_skill(f: &TaskFixture, id: &str, revision: i64, name: &str, description: &str, content: &str) -> Result<()> {
    let next = revision + 1;
    f.db().execute(
        "INSERT INTO skills VALUES (?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,revision=excluded.revision",
        (id, name, next),
    )?;
    f.db().execute("INSERT INTO skill_versions VALUES (?,?,?,?,?)", (id, next, name, description, content))?;
    Ok(())
}

fn scope_row(f: &TaskFixture, table: &str, key: Value, account: &str, project: &str, visibility: &str, root: &str) -> Result<()> {
    f.db().execute(
        "INSERT INTO record_scopes VALUES (?,?,?,?,?,?,?)",
        [table, &key.to_string(), account, project, visibility, "agent", root],
    )?;
    Ok(())
}

fn read_intent(task: &str, action: &str, sequence: i64, ownership: &Value, digest: &str, reference: &Value) -> String {
    digest_command(&json!([task, action, sequence, ownership, digest, "read", reference["snapshotId"], reference["digest"], null, null, 1, null, null, null]))
}

#[tokio::test]
async fn context_projects_authorized_memory_pinned_skills_and_byte_free_asset_refs_invalidating_changed_sources(
) -> Result<()> {
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    let p = prepared(&f).await?;
    let scope = format!("project:{}/agents:{}/human:{}", f.project, f.agent, f.owner.account_id);
    let memory = remember(&f, &scope, "Project A decision")?;
    f.db().execute("UPDATE memories SET essential=1 WHERE scope=? AND id=?", [&scope, &memory])?;
    scope_row(&f, "memories", json!([scope, memory]), &f.owner.account_id, &f.project, "owner", &f.agent)?;
    let private_scope = format!("private:{}", f.member.account_id);
    let private_memory = remember(&f, &private_scope, "PRIVATE_OTHER_HUMAN")?;
    f.db().execute("UPDATE memories SET essential=1 WHERE scope=? AND id=?", [&private_scope, &private_memory])?;
    scope_row(&f, "memories", json!([private_scope, private_memory]), &f.member.account_id, &f.project, "owner", &f.agent)?;
    remember(&f, &format!("agents:{}", f.agent), "UNMAPPED_LEGACY_PRIVATE")?;
    let skill = new_id();
    save_skill(&f, &skill, 0, "Pinned skill", "Reference configuration", "Use project rules")?;
    save_skill(&f, &skill, 1, "Latest skill", "Must not substitute", "UNINSTALLED_LATEST_VERSION")?;
    f.db().execute("INSERT INTO agent_skills(agent,skill,version,enabled,revision,removed) VALUES (?,?,1,1,7,0)", [&f.agent, &skill])?;
    let raw = b"PRIVATE_ASSET_BYTES";
    let encoded_raw = minarbot_testkit::base64::Engine::encode(&minarbot_testkit::base64::engine::general_purpose::STANDARD, raw);
    f.db().execute(
        "INSERT INTO skill_files VALUES (?,1,?)",
        [skill.clone(), json!([{"path": "asset.bin", "content": encoded_raw, "encoding": "base64"}]).to_string()],
    )?;
    for (table, key) in [("skills", json!([skill])), ("skill_versions", json!([skill, 1])), ("agent_skills", json!([f.agent, skill])), ("skill_files", json!([skill, 1]))] {
        scope_row(&f, table, key, &f.owner.account_id, &f.project, "owner", &f.agent)?;
    }
    f.db().execute("UPDATE task_actions SET state='not_sent',effect_status='none' WHERE id=?", [&p.action])?;
    let context = f
        .task_op("context", &p.task, json!({"requestId": new_id(), "ownership": p.ownership, "expectedTaskRevision": 2}), &p.token)
        .await?
        .expect_ok()?;
    let snapshot = context.data()["snapshot"].clone();
    ensure!(snapshot["memories"][0]["text"] == "Project A decision", "memories: {}", snapshot["memories"]);
    ensure!(snapshot["memories"].as_array().map(Vec::len) == Some(1), "memory count: {}", snapshot["memories"]);
    ensure!(snapshot["skills"][0]["id"] == skill.as_str(), "skill: {}", snapshot["skills"]);
    ensure!(snapshot["skills"][0]["revision"] == 1, "pinned revision: {}", snapshot["skills"]);
    ensure!(snapshot["skills"][0]["installationRevision"] == 7, "installation revision: {}", snapshot["skills"]);
    ensure!(
        snapshot["skillAssets"] == json!([{"skillId": skill, "skillRevision": 1, "installationRevision": 7, "source": "asset.bin",
            "digest": sha256_hex(raw), "size": raw.len()}]),
        "assets: {}",
        snapshot["skillAssets"]
    );
    ensure!(snapshot["tools"] == json!([]), "tools: {}", snapshot["tools"]);
    let encoded = canonical_json(&snapshot);
    for secret in ["PRIVATE_OTHER_HUMAN", "UNMAPPED_LEGACY_PRIVATE", "UNINSTALLED_LATEST_VERSION", "PRIVATE_ASSET_BYTES", encoded_raw.as_str()] {
        ensure!(!encoded.contains(secret), "snapshot contains {secret}");
    }
    ensure!(context.data()["snapshotRef"]["digest"] == sha256_hex(&encoded).as_str(), "snapshot digest");
    f.db().execute("UPDATE memories SET text=?,revision=revision+1 WHERE scope=? AND id=?", ["Changed decision", &scope, &memory])?;
    let action = new_id();
    let digest = sha256_hex("next");
    let reference = context.data()["snapshotRef"].clone();
    let prepare = json!({"requestId": new_id(), "sequence": 1, "ownership": p.ownership, "payloadDigest": digest, "actionKind": "read",
        "contextSnapshotId": reference["snapshotId"], "contextDigest": reference["digest"], "grantRevision": 1, "expectedTaskRevision": 2,
        "journalSequence": 2, "intentDigest": read_intent(&p.task, &action, 1, &p.ownership, &digest, &reference)});
    f.prepare(&p.task, &action, prepare.clone(), &p.token).await?.expect_code("context_stale")?;
    let next = f
        .task_op("context", &p.task, json!({"requestId": new_id(), "ownership": p.ownership, "expectedTaskRevision": 2}), &p.token)
        .await?
        .expect_ok()?;
    let next_ref = next.data()["snapshotRef"].clone();
    f.db().execute("UPDATE agent_skills SET enabled=0,revision=revision+1 WHERE agent=? AND skill=?", [&f.agent, &skill])?;
    f.prepare(&p.task, &action, with(prepare, json!({"contextSnapshotId": next_ref["snapshotId"], "contextDigest": next_ref["digest"],
        "intentDigest": read_intent(&p.task, &action, 1, &p.ownership, &digest, &next_ref)})), &p.token)
        .await?
        .expect_code("context_stale")?;
    Ok(())
}

fn context_tables(f: &TaskFixture) -> Result<()> {
    f.db().execute_batch(
        "CREATE TABLE IF NOT EXISTS questions(id TEXT PRIMARY KEY,turn TEXT NOT NULL REFERENCES turns(id),call_id TEXT NOT NULL,call TEXT NOT NULL,prompt TEXT NOT NULL,choices TEXT NOT NULL,answer TEXT,after_output INTEGER NOT NULL,after_action INTEGER NOT NULL,UNIQUE(turn,call_id)) STRICT;
         CREATE TABLE IF NOT EXISTS question_answers(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
         CREATE TABLE IF NOT EXISTS queue_positions(turn TEXT PRIMARY KEY,position INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS compactions(scope TEXT PRIMARY KEY,version TEXT NOT NULL,through INTEGER NOT NULL,summary TEXT NOT NULL) STRICT;
         CREATE TABLE IF NOT EXISTS compaction_sources(scope TEXT NOT NULL,seq INTEGER NOT NULL REFERENCES turns(seq),revision INTEGER NOT NULL,status TEXT NOT NULL,PRIMARY KEY(scope,seq)) STRICT;
         CREATE TABLE IF NOT EXISTS compaction_attempts(turn TEXT PRIMARY KEY REFERENCES turns(id),status TEXT NOT NULL,created TEXT NOT NULL) STRICT;
         CREATE TABLE IF NOT EXISTS routines(id TEXT PRIMARY KEY,agent TEXT);
         CREATE TABLE IF NOT EXISTS coordination(seq INTEGER PRIMARY KEY AUTOINCREMENT,turn TEXT,owner TEXT,target TEXT,kind TEXT,prompt TEXT,call TEXT,child TEXT);
         CREATE TABLE IF NOT EXISTS files(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE,scope TEXT,name TEXT,digest TEXT,bytes BLOB,created TEXT);
         CREATE TABLE IF NOT EXISTS generated_files(seq INTEGER PRIMARY KEY AUTOINCREMENT,turn TEXT,file TEXT,call_id TEXT,call TEXT,after_action INTEGER);",
    )
}

/// A done turn mapped to `(account, project, root)` (or unmapped); returns
/// `(id, seq)`.
fn seed_turn(f: &TaskFixture, text: &str, reply: &str, project: &str, account: &str, root: &str, mapped: bool) -> Result<(String, i64)> {
    let id = new_id();
    f.db().execute(
        "INSERT INTO turns(id,agent,text,reply,status,created) VALUES (?,?,?,?,'done','fixture')",
        [&id, &f.agent, text, reply],
    )?;
    let seq = f.db().connection().last_insert_rowid();
    if mapped {
        f.db().execute(
            "INSERT INTO record_scopes VALUES ('turns',?,?,?,'owner','agent',?)",
            [json!([id]).to_string(), account.to_owned(), project.to_owned(), root.to_owned()],
        )?;
    }
    Ok((id, seq))
}

#[tokio::test]
async fn context_preserves_scoped_history_questions_compaction_and_child_evidence_without_foreign_records_or_file_bytes(
) -> Result<()> {
    let f = TaskFixture::new(&["transfer_task"], None).await?;
    context_tables(&f)?;
    let other_project = new_id();
    f.human(minarbot_testkit::http::Method::POST, "/api/projects", Some(json!({"requestId": new_id(), "id": other_project, "name": "Other"})), &f.owner)
        .await?;
    let (owner, member, agent, project) = (f.owner.account_id.clone(), f.member.account_id.clone(), f.agent.clone(), f.project.clone());
    let turn = |text: &str, project: &str, account: &str, root: &str| -> Result<String> {
        Ok(seed_turn(&f, text, &format!("{text} reply"), project, account, root, true)?.0)
    };
    let mut old = Vec::new();
    for index in 0..42 {
        old.push(turn(&format!("History {index}"), &project, &owner, &agent)?);
    }
    let private_turn = turn("PRIVATE_HUMAN", &project, &member, &agent)?;
    let foreign_turn = turn("FOREIGN_PROJECT", &other_project, &owner, &agent)?;
    turn("OTHER_CONVERSATION", &project, &owner, &new_id())?;
    let scope = format!("project:{project}/agent:{agent}/human:{owner}");
    let through = field(&f, "SELECT seq FROM turns WHERE id=?", [&old[0]], "seq")?;
    f.db().execute("INSERT INTO compactions VALUES (?,?,?,?)", (scope.clone(), new_id(), through.as_i64(), "Earlier summary"))?;
    f.db().execute("INSERT INTO compaction_sources VALUES (?,?,1,'done')", (scope.clone(), through.as_i64()))?;
    scope_row(&f, "compactions", json!([scope]), &owner, &project, "owner", &agent)?;
    let command = f.command(json!({}));
    let task = command["requestId"].as_str().ok_or("task")?.to_owned();
    f.admit(&f.owner, &command).await?.expect_ok()?;
    let child = turn("Visible child", &project, &owner, &new_id())?;
    let call = |id: &str| json!({"id": id, "function": {"name": "search_memory", "arguments": "{}"}});
    for target in [&child, &private_turn, &foreign_turn] {
        f.db().execute(
            "INSERT INTO coordination(turn,owner,target,kind,prompt,call,child) VALUES (?,?,?,?,?,?,?)",
            [task.as_str(), &agent, &agent, "delegate_task", "Subtask", &call(target).to_string(), target],
        )?;
    }
    for (id, result) in [(&task, "Parent result"), (&child, "Child result"), (&private_turn, "PRIVATE_TOOL"), (&foreign_turn, "FOREIGN_TOOL")] {
        f.db().execute(
            "INSERT INTO flow_tool_calls(turn,call_id,call,result,after_output,after_action) VALUES (?,?,?,?,0,0)",
            [id.as_str(), id, &call(id).to_string(), &json!({"text": result}).to_string()],
        )?;
    }
    let file = new_id();
    let raw = b"ATTACHMENT_BYTES".to_vec();
    f.db().execute(
        "INSERT INTO files(id,scope,name,digest,bytes,created) VALUES (?,'legacy-private','result.txt',?,?,'fixture')",
        (file.clone(), sha256_hex(&raw), raw.clone()),
    )?;
    f.db().execute(
        "INSERT INTO record_scopes VALUES ('files',?,?,?,'owner','agent',?)",
        [json!([file]).to_string(), owner.clone(), project.clone(), agent.clone()],
    )?;
    f.db().execute(
        "INSERT INTO generated_files(turn,file,call_id,call,after_action) VALUES (?,?,?,?,0)",
        [child.as_str(), &file, "output", &call("output").to_string()],
    )?;
    let question = new_id();
    f.db().execute(
        "INSERT INTO questions VALUES (?,?,?,?,?,?,?,0,0)",
        [question.as_str(), &old[5], "question", &call("question").to_string(), "Keep scope?", &json!(["Yes"]).to_string(), "Yes"],
    )?;
    let (token, ownership, _) = f.acquire_task(&task).await?;
    let request = json!({"requestId": new_id(), "ownership": ownership, "expectedTaskRevision": 2});
    let context = f.task_op("context", &task, request.clone(), &token).await?.expect_ok()?;
    let snapshot = context.data()["snapshot"].clone();
    let history = snapshot["conversation"]["history"].as_array().cloned().unwrap_or_default();
    ensure!(history.len() == 40, "history length: {}", history.len());
    ensure!(history[0]["text"] == "History 2", "oldest history: {}", history[0]);
    ensure!(snapshot["conversation"]["compaction"]["summary"] == "Earlier summary", "compaction: {}", snapshot["conversation"]["compaction"]);
    ensure!(snapshot["conversation"]["questions"][0]["id"] == question.as_str(), "questions: {}", snapshot["conversation"]["questions"]);
    let coordination = snapshot["coordination"].as_array().cloned().unwrap_or_default();
    ensure!(coordination.len() == 1 && coordination.iter().filter(|row| !row["child"].is_null()).count() == 1, "coordination: {coordination:?}");
    ensure!(coordination[0]["child"]["reply"] == "Visible child reply", "child reply: {}", coordination[0]);
    let journal = snapshot["toolJournal"].as_array().cloned().unwrap_or_default();
    let texts: Vec<Value> = journal.iter().filter_map(|row| row["result"]["text"].as_str().map(|text| json!(text))).collect();
    ensure!(texts == [json!("Parent result"), json!("Child result")], "tool journal: {texts:?}");
    ensure!(journal.last().is_some_and(|row| row["kind"] == "generated_output"), "last journal entry: {:?}", journal.last());
    ensure!(
        snapshot["artifacts"] == json!([{"id": file, "name": "result.txt", "digest": sha256_hex(&raw), "size": raw.len()}]),
        "artifacts: {}",
        snapshot["artifacts"]
    );
    let encoded = canonical_json(&snapshot);
    for secret in ["PRIVATE_HUMAN", "FOREIGN_PROJECT", "OTHER_CONVERSATION", "PRIVATE_TOOL", "FOREIGN_TOOL", "ATTACHMENT_BYTES", "legacy-private"] {
        ensure!(!encoded.contains(secret), "snapshot contains {secret}");
    }
    f.db().execute("UPDATE flow_tool_calls SET result=? WHERE turn=?", [json!({"text": "Changed child"}).to_string(), child.clone()])?;
    let action = new_id();
    let digest = sha256_hex("read");
    let reference = context.data()["snapshotRef"].clone();
    f.prepare(&task, &action, json!({"requestId": new_id(), "sequence": 0, "ownership": ownership, "payloadDigest": digest, "actionKind": "read",
        "contextSnapshotId": reference["snapshotId"], "contextDigest": reference["digest"], "grantRevision": 1, "expectedTaskRevision": 2,
        "journalSequence": 1, "intentDigest": read_intent(&task, &action, 0, &ownership, &digest, &reference)}), &token)
        .await?
        .expect_code("context_stale")?;
    ensure!(
        canonical_json(f.task_op("context", &task, request.clone(), &token).await?.expect_ok()?.data()) == canonical_json(context.data()),
        "context receipt changed"
    );
    let private_seq = field(&f, "SELECT seq FROM turns WHERE id=?", [&private_turn], "seq")?;
    f.db().execute("INSERT INTO compaction_sources VALUES (?,?,1,'done')", (scope.clone(), private_seq.as_i64()))?;
    let fresh = f.task_op("context", &task, with(request.clone(), json!({"requestId": new_id()})), &token).await?.expect_ok()?;
    ensure!(fresh.data()["snapshot"]["conversation"]["compaction"].is_null(), "foreign-sourced compaction kept");
    let snapshots = field(&f, "SELECT count(*) n FROM task_context_snapshots", [], "n")?;
    f.db().execute("UPDATE flow_tool_calls SET result=? WHERE turn=?", [json!({"text": "x".repeat(9 * 1024 * 1024)}).to_string(), child.clone()])?;
    f.task_op("context", &task, with(request.clone(), json!({"requestId": new_id()})), &token).await?.expect_code("context_too_large")?;
    ensure!(
        canonical_json(f.task_op("context", &task, request, &token).await?.expect_ok()?.data()) == canonical_json(context.data()),
        "context receipt after a too-large snapshot"
    );
    ensure!(field(&f, "SELECT count(*) n FROM task_context_snapshots", [], "n")? == snapshots, "too-large snapshot saved");
    ensure!(field(&f, "SELECT count(*) n FROM task_actions", [], "n")? == 0, "actions");
    Ok(())
}

#[tokio::test]
async fn canonical_history_retains_sequence_cursors_and_excludes_unmapped_foreign_private_and_future_originals(
) -> Result<()> {
    let f = TaskFixture::new(&["read_history", "read_message"], None).await?;
    context_tables(&f)?;
    let other = new_id();
    f.human(minarbot_testkit::http::Method::POST, "/api/projects", Some(json!({"requestId": new_id(), "id": other, "name": "Other"})), &f.owner)
        .await?;
    let (owner, member, agent, project) = (f.owner.account_id.clone(), f.member.account_id.clone(), f.agent.clone(), f.project.clone());
    let seed = |text: &str, project: &str, account: &str, root: &str, mapped: bool| seed_turn(&f, text, "Saved original reply", project, account, root, mapped);
    let mut originals = Vec::new();
    for index in 0..8 {
        originals.push(seed(&format!("Original {index}"), &project, &owner, &agent, true)?);
    }
    let private_turn = seed("PRIVATE", &project, &member, &agent, true)?;
    let foreign = seed("FOREIGN", &other, &owner, &agent, true)?;
    let other_root = seed("OTHER_ROOT", &project, &owner, &new_id(), true)?;
    let unmapped = seed("UNMAPPED", &project, &owner, &agent, false)?;
    f.db().execute("INSERT INTO queue_positions VALUES (?,?)", (originals[5].0.clone(), originals[4].1))?;
    f.db().execute("INSERT INTO queue_positions VALUES (?,?)", (originals[4].0.clone(), originals[5].1))?;
    f.db().execute("UPDATE turns SET status='failed' WHERE id=?", [&originals[0].0])?;
    let file = new_id();
    let blocked = new_id();
    let raw = b"SOURCE_DEVICE_BYTES".to_vec();
    for (id, account) in [(&file, &owner), (&blocked, &member)] {
        f.db().execute(
            "INSERT INTO files(id,scope,name,digest,bytes,created) VALUES (?,'legacy-private','original.txt',?,?,'fixture')",
            (id.clone(), sha256_hex(&raw), raw.clone()),
        )?;
        f.db().execute(
            "INSERT INTO record_scopes VALUES ('files',?,?,?,'owner','agent',?)",
            [json!([id]).to_string(), account.clone(), project.clone(), agent.clone()],
        )?;
    }
    f.db().execute("UPDATE turns SET attachments=? WHERE id=?", [json!([file, blocked]).to_string(), originals[0].0.clone()])?;
    f.db().execute(
        "INSERT INTO generated_files(turn,file,call_id,call,after_action) VALUES (?,?,?,?,0)",
        [originals[0].0.as_str(), &file, "file", &json!({"function": {"name": "create_text_file"}}).to_string()],
    )?;
    f.db().execute(
        "INSERT INTO questions VALUES (?,?,?,?,?,?,?,0,0)",
        [new_id().as_str(), &originals[0].0, "question", "{}", "Keep original?", &json!(["Yes"]).to_string(), "Yes"],
    )?;
    let command = f.command(json!({}));
    f.admit(&f.owner, &command).await?.expect_ok()?;
    let task_id = command["requestId"].as_str().ok_or("task")?.to_owned();
    let (token, ownership, _) = f.acquire_task(&task_id).await?;
    let future = seed("FUTURE", &project, &owner, &agent, true)?;
    let mut task = Task { id: task_id.clone(), token, ownership, actor: f.owner.clone(), sequence: 0 };
    let ids = |rows: &Value| -> Vec<Value> { rows.as_array().map(|rows| rows.iter().map(|row| row["id"].clone()).collect()).unwrap_or_default() };
    let page = result_content(&stage(&f, &mut task, &json!({}), "read_history").await?.run(&f).await?.expect_ok()?);
    let expected: Vec<Value> = originals[3..].iter().rev().map(|row| json!(row.0)).collect();
    ensure!(ids(&page["messages"]) == expected, "first page: {page}");
    ensure!(page["next_before"] == originals[3].1, "cursor: {page}");
    let next = result_content(&stage(&f, &mut task, &json!({"before": page["next_before"]}), "read_history").await?.run(&f).await?.expect_ok()?);
    let expected: Vec<Value> = originals[..3].iter().rev().map(|row| json!(row.0)).collect();
    ensure!(ids(&next["messages"]) == expected, "second page: {next}");
    ensure!(next["next_before"].is_null(), "last cursor: {next}");
    let read = stage(&f, &mut task, &json!({"id": originals[0].0}), "read_message").await?;
    let read_result = read.run(&f).await?.expect_ok()?;
    let content = result_content(&read_result);
    ensure!(ids(&content["attachments"]) == [json!(file)], "attachments: {content}");
    ensure!(ids(&content["generatedFiles"]) == [json!(file)], "generated files: {content}");
    ensure!(content["questions"][0]["answer"] == "Yes", "questions: {content}");
    ensure!(content["status"] == "failed", "status: {content}");
    ensure!(!content.to_string().contains("SOURCE_DEVICE_BYTES"), "file bytes in the message");
    for id in [&private_turn.0, &foreign.0, &other_root.0, &unmapped.0, &future.0, &task_id] {
        let result = result_content(&stage(&f, &mut task, &json!({"id": id}), "read_message").await?.run(&f).await?.expect_ok()?);
        ensure!(result["error"].as_str().is_some_and(|error| error.contains("not found")), "{id} readable: {result}");
    }
    ensure!(status_of(&f, &mut task, json!({"before": 0}), "read_history").await? == 400, "zero cursor");
    ensure!(status_of(&f, &mut task, json!({"id": originals[0].0, "offset": -1}), "read_message").await? == 400, "negative offset");
    f.db().execute("UPDATE turns SET text=?,revision=revision+1 WHERE id=?", ["Changed original", &originals[0].0])?;
    let replay = read.run(&f).await?.expect_ok()?;
    ensure!(canonical_json(&replay.body) == canonical_json(&read_result.body), "read receipt changed");
    Ok(())
}
