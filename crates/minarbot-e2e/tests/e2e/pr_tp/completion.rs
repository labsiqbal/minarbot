//! Canonical completion recovery and controls through signed HTTP.

use super::{
    fixture::{direct, locked, task_body, until, Fault, Tp, DEVICE_RECONNECT, SETTLE},
    routines::text,
};
use minarbot_testkit::{
    device_host::{is_code, Connection, DeviceHost},
    prelude::*,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

/// Project, bound agent and (unless `setup`) one admitted task on `home`.
async fn bound_task(
    tp: &Tp,
    home: &Value,
    name: &str,
    model: &str,
    setup: bool,
) -> Result<(String, String, String, String)> {
    let (project, agent, binding) = (new_id(), new_id(), new_id());
    tp.project(&project, name).await?;
    let db = tp.write()?;
    let device = text(&home["id"]);
    let account: String = db.scalar("SELECT account FROM devices WHERE id=?", [&device])?;
    db.execute(
        "INSERT INTO provider_bindings VALUES (?,?,?,'openai-compatible',?,'Synthetic','ready',1)",
        [&binding, &device, &account, model],
    )?;
    let mut body = json!({"requestId": agent, "id": agent, "name": "Writer", "model": "fixture",
        "backupModel": "", "instructions": "Answer carefully", "threadLimit": 2, "projectId": project,
        "homeDeviceId": device, "expectedProjectRevision": 1, "expectedHomeDeviceRevision": home["revision"],
        "providerBinding": {"bindingId": binding, "expectedRevision": 1}});
    if setup {
        body["setup"] = json!(true);
    }
    let (status, created) = tp.send("/api/agents", Some(body)).await?;
    ensure!(status == 201, "agent: {status} {created}");
    let task = if setup { agent.clone() } else { new_id() };
    if !setup {
        let (status, admitted) = tp
            .send(
                "/api/tasks",
                Some(task_body(
                    &task,
                    &project,
                    &agent,
                    &device,
                    &direct("Answer once"),
                )),
            )
            .await?;
        ensure!(status == 202, "task: {status} {admitted}");
    }
    Ok((project, agent, binding, task))
}

async fn connect(host: &Arc<DeviceHost>) -> Result<Connection> {
    let connection = host.connect().await?;
    until("home", SETTLE, || async {
        Ok(connection.phase() == "connected")
    })
    .await?;
    Ok(connection)
}

#[derive(Clone, Copy, PartialEq)]
enum Lost {
    Request,
    Reply,
    Receipt,
    Revoked,
    Storage,
}

async fn completion_recovery(lost: Lost) -> Result<()> {
    let mut tp = Tp::start().await?;
    let name = "completion-home.sqlite";
    let result = async {
        let mut runtime = tp.device(name)?;
        runtime.enroll(&tp.jwt("user_owner")?, "new").await?;
        let _connection = connect(&runtime).await?;
        let home = tp.data("/api/devices").await?[0].clone();
        let (project, agent, binding, task) = bound_task(&tp, &home, "Completion", "synthetic-completion", false).await?;
        let calls = Arc::new(AtomicUsize::new(0));
        let owned = runtime.start_task(&task).await?;
        let context = runtime.context(&task, &owned["ownership"], &owned["revision"]).await?;
        let action = new_id();
        let counter = calls.clone();
        runtime
            .action(
                &json!({"taskId": task, "actionId": action, "sequence": 0, "actionKind": "provider",
                    "ownership": owned["ownership"], "payload": {"purpose": "answer"}, "context": context,
                    "grantRevision": 1, "bindingId": binding, "bindingRevision": 1}),
                move |_| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    async { Ok(json!({"text": "Saved final answer"})) }
                },
            )
            .await?;
        let status: Arc<Mutex<Option<u16>>> = Arc::default();
        let seen = status.clone();
        tp.fault(Some(Arc::new(move |exchange| {
            if !exchange.path.ends_with("/complete") {
                return Fault::Pass;
            }
            match lost {
                Lost::Request | Lost::Revoked | Lost::Storage => Fault::Drop,
                Lost::Reply => {
                    let seen = seen.clone();
                    Fault::LoseReply(Box::new(move |code, _| {
                        *locked(&seen) = Some(code);
                        true
                    }))
                }
                Lost::Receipt => {
                    let seen = seen.clone();
                    Fault::Rewrite(Box::new(move |code, body| {
                        *locked(&seen) = Some(code);
                        let mut value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                        value["receipt"]["payloadDigest"] = json!("0".repeat(64));
                        value.to_string().into_bytes()
                    }))
                }
            }
        })));
        let failed = runtime.complete(&task, &action).await;
        tp.fault(None);
        if lost == Lost::Receipt {
            ensure!(failed.as_ref().is_err_and(|error| is_code(error, "receipt_mismatch")), "receipt: {failed:?}");
        } else {
            ensure!(failed.is_err(), "completion was not lost: {failed:?}");
        }
        if matches!(lost, Lost::Reply | Lost::Receipt) {
            ensure!(*locked(&status) == Some(200), "completion status: {:?}", locked(&status));
        }
        let database = tp.database();
        let unassign = |deleted: i64| -> Result<()> {
            minarbot_testkit::db::Fixture::open(&database)?.execute(
                "UPDATE agent_projects SET deleted=? WHERE project=? AND agent=?",
                minarbot_testkit::hosted::params![deleted, project, agent],
            )?;
            Ok(())
        };
        if matches!(lost, Lost::Revoked | Lost::Storage) {
            unassign(1)?;
        }
        runtime.close().await?;
        // restart() waits for devices the server still lists as ready; let the
        // closed host's disconnect land first.
        until("home disconnected", DEVICE_RECONNECT, || async {
            Ok(tp
                .data("/api/devices")
                .await?
                .as_array()
                .is_some_and(|rows| rows.iter().all(|row| row["status"] != "ready")))
        })
        .await?;
        tp.restart().await?;
        let device_db = tp.dir().join(name);
        if lost == Lost::Storage {
            minarbot_testkit::db::Fixture::open(&device_db)?.execute_batch("CREATE TRIGGER fail_recovery_marker BEFORE UPDATE OF intent_json ON device_journal WHEN json_extract(NEW.intent_json,'$.completionError') IS NOT NULL BEGIN SELECT RAISE(ABORT,'completion marker write failed'); END")?;
        }
        runtime = tp.device(name)?;
        let _reconnected = connect(&runtime).await?;
        until("recovery result", SETTLE, || async {
            Ok(runtime.recovery_status().as_array().is_some_and(|rows| {
                rows.iter().any(|row| {
                    row["actionId"] == action.as_str()
                        && match lost {
                            Lost::Storage => row["error"] == "recovery_storage_unavailable",
                            Lost::Revoked => row["error"] == "execution_denied",
                            _ => row["state"] == "completed",
                        }
                })
            }))
        })
        .await?;
        if lost == Lost::Storage {
            minarbot_testkit::db::Fixture::open(&device_db)?.execute_batch("DROP TRIGGER fail_recovery_marker")?;
            ensure!(
                runtime.recover().await? == json!([{"taskId": task, "actionId": action, "error": "execution_denied"}]),
                "storage recovery"
            );
        }
        if matches!(lost, Lost::Revoked | Lost::Storage) {
            ensure!(runtime.recover().await? == json!([]), "recovery settled");
            runtime.close().await?;
            runtime = tp.device(name)?;
            let _restarted = connect(&runtime).await?;
            ensure!(runtime.recover().await? == json!([]), "recovery after restart");
            ensure!(
                tp.inspect()?.row("SELECT reply FROM turns WHERE id=?", [&task])?["reply"].is_null(),
                "revoked completion reply"
            );
            unassign(0)?;
        }
        let completed = runtime.complete(&task, &action).await?;
        ensure!(completed["data"]["state"] == "completed", "completed: {completed}");
        ensure!(runtime.complete(&task, &action).await?["receipt"] == completed["receipt"], "completion replay");
        let detail = tp.task_detail(&task).await?;
        ensure!(detail["state"] == "completed", "state");
        ensure!(detail["results"].as_array().map(Vec::len) == Some(1), "results");
        ensure!(calls.load(Ordering::SeqCst) == 1, "provider calls");
        ensure!(runtime.recover().await? == json!([]), "nothing pending");
        runtime.close().await?;
        runtime = tp.device(name)?;
        let _restarted = connect(&runtime).await?;
        ensure!(runtime.recover().await? == json!([]), "nothing pending after restart");
        let db = tp.inspect()?;
        ensure!(
            db.row("SELECT status,reply,paused FROM turns WHERE id=?", [&task])?
                == json!({"status": "done", "reply": "Saved final answer", "paused": 0}),
            "turn"
        );
        ensure!(
            db.scalar::<i64>("SELECT count(*) FROM receipts WHERE scope=?", [format!("tasks:complete:{task}")])? == 1,
            "completion receipts"
        );
        let (status, _) = tp
            .send(
                &format!("/api/tasks/{task}/control"),
                Some(json!({"requestId": new_id(), "expectedRevision": detail["revision"], "operation": "resume"})),
            )
            .await?;
        ensure!(status == 409, "resume completed: {status}");
        Ok(())
    }
    .await;
    tp.fault(None);
    tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_completion_recovery_handles_request_without_another_provider_call() -> Result<()>
{
    completion_recovery(Lost::Request).await
}

#[tokio::test]
async fn canonical_completion_recovery_handles_reply_without_another_provider_call() -> Result<()> {
    completion_recovery(Lost::Reply).await
}

#[tokio::test]
async fn canonical_completion_recovery_handles_receipt_without_another_provider_call() -> Result<()>
{
    completion_recovery(Lost::Receipt).await
}

#[tokio::test]
async fn canonical_completion_recovery_handles_revoked_without_another_provider_call() -> Result<()>
{
    completion_recovery(Lost::Revoked).await
}

#[tokio::test]
async fn canonical_completion_recovery_handles_storage_without_another_provider_call() -> Result<()>
{
    completion_recovery(Lost::Storage).await
}

#[derive(Clone, Copy, PartialEq)]
enum Scenario {
    Pause,
    Cancel,
    Latest,
    Invalid,
    Setup,
    Revoked,
}

async fn completion_enforces(scenario: Scenario) -> Result<()> {
    let tp = Tp::start().await?;
    let result = async {
        let runtime = tp.device("completion-controls.sqlite")?;
        runtime.enroll(&tp.jwt("user_owner")?, "new").await?;
        let _connection = connect(&runtime).await?;
        let home = tp.data("/api/devices").await?[0].clone();
        let (project, agent, binding, task) =
            bound_task(&tp, &home, "Completion controls", "completion-controls", scenario == Scenario::Setup).await?;
        let db = tp.write()?;
        let owned = runtime.start_task(&task).await?;
        let mut sequence = 0;
        let mut run = |content: Value, canonical: bool| {
            let (tp, runtime, task, owned, binding) = (&tp, runtime.clone(), task.clone(), owned.clone(), binding.clone());
            let current = sequence;
            sequence += 1;
            async move {
                let detail = tp.task_detail(&task).await?;
                let context = runtime.context(&task, &owned["ownership"], &detail["revision"]).await?;
                let action = new_id();
                let mut input = json!({"taskId": task, "actionId": action, "sequence": current,
                    "actionKind": if canonical { "canonical" } else { "provider" },
                    "ownership": owned["ownership"], "context": context, "grantRevision": 1});
                if canonical {
                    input["payload"] = json!({"toolCall": {"id": action, "function": {"name": "edit_profile",
                        "arguments": content.to_string()}},
                        "expectedRevision": context["snapshot"]["taskRevision"].as_i64().unwrap_or(0) + 1});
                } else {
                    input["payload"] = json!({"purpose": "answer"});
                    input["bindingId"] = json!(binding);
                    input["bindingRevision"] = json!(1);
                }
                runtime
                    .action(&input, move |_| {
                        let content = content.clone();
                        async move { if canonical { Err("canonical action reached the provider".into()) } else { Ok(content) } }
                    })
                    .await?;
                Ok::<String, minarbot_testkit::Error>(action)
            }
        };
        let first = if scenario == Scenario::Invalid {
            json!({"text": " ", "extra": true})
        } else {
            json!({"text": "Saved answer"})
        };
        let action = run(first, false).await?;
        let reply = || -> Result<Value> { Ok(db.row("SELECT reply FROM turns WHERE id=?", [&task])?["reply"].clone()) };
        let rejects = |result: Result<Value>, code: &str| -> Result<()> {
            ensure!(result.as_ref().is_err_and(|error| is_code(error, code)), "expected {code}: {result:?}");
            Ok(())
        };
        match scenario {
            Scenario::Pause | Scenario::Cancel => {
                let control = |operation: &'static str| {
                    let (tp, task) = (&tp, task.clone());
                    async move {
                        let detail = tp.task_detail(&task).await?;
                        let (status, _) = tp
                            .send(
                                &format!("/api/tasks/{task}/control"),
                                Some(json!({"requestId": new_id(), "expectedRevision": detail["revision"], "operation": operation})),
                            )
                            .await?;
                        ensure!(status == 200, "{operation}: {status}");
                        Ok::<(), minarbot_testkit::Error>(())
                    }
                };
                control(if scenario == Scenario::Pause { "pause" } else { "cancel" }).await?;
                rejects(
                    runtime.complete(&task, &action).await,
                    if scenario == Scenario::Pause { "task_not_runnable" } else { "ownership_conflict" },
                )?;
                ensure!(reply()?.is_null(), "reply written");
                if scenario == Scenario::Pause {
                    control("resume").await?;
                    runtime.complete(&task, &action).await?;
                }
            }
            Scenario::Invalid => rejects(runtime.complete(&task, &action).await, "result_invalid")?,
            Scenario::Revoked => {
                let unassign = |deleted: i64| {
                    db.execute(
                        "UPDATE agent_projects SET deleted=? WHERE project=? AND agent=?",
                        minarbot_testkit::hosted::params![deleted, project, agent],
                    )
                };
                unassign(1)?;
                rejects(runtime.complete(&task, &action).await, "execution_denied")?;
                unassign(0)?;
                let completed = runtime.complete(&task, &action).await?;
                unassign(1)?;
                rejects(runtime.complete(&task, &action).await, "execution_denied")?;
                ensure!(
                    db.row("SELECT state,revision FROM tasks WHERE id=?", [&task])?["revision"] == completed["data"]["revision"],
                    "revision changed"
                );
                ensure!(reply()? == "Saved answer", "reply");
            }
            Scenario::Setup => {
                rejects(runtime.complete(&task, &action).await, "setup_incomplete")?;
                run(json!({"revision": 1, "name": "Research partner", "instructions": "Cite evidence"}), true).await?;
                rejects(runtime.complete(&task, &action).await, "outcome_unresolved")?;
                let saved = run(json!({"text": "Profile saved"}), false).await?;
                runtime.complete(&task, &saved).await?;
                ensure!(db.scalar::<String>("SELECT status FROM turns WHERE id=?", [&task])? == "done", "setup done");
            }
            Scenario::Latest => {
                let latest = run(json!({"text": "Latest answer"}), false).await?;
                rejects(runtime.complete(&task, &action).await, "outcome_unresolved")?;
                db.execute_batch("CREATE TRIGGER fail_completion BEFORE UPDATE OF reply ON turns BEGIN SELECT RAISE(ABORT,'completion write failed'); END")?;
                ensure!(runtime.complete(&task, &latest).await.is_err(), "completion with failed write");
                ensure!(
                    db.scalar::<i64>("SELECT count(*) FROM receipts WHERE scope=?", [format!("tasks:complete:{task}")])? == 0,
                    "receipt kept"
                );
                ensure!(db.scalar::<String>("SELECT state FROM tasks WHERE id=?", [&task])? == "running", "state kept");
                db.execute_batch("DROP TRIGGER fail_completion")?;
                runtime.complete(&task, &latest).await?;
                ensure!(reply()? == "Latest answer", "latest reply");
            }
        }
        Ok(())
    }
    .await;
    tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_completion_enforces_pause_through_signed_http() -> Result<()> {
    completion_enforces(Scenario::Pause).await
}

#[tokio::test]
async fn canonical_completion_enforces_cancel_through_signed_http() -> Result<()> {
    completion_enforces(Scenario::Cancel).await
}

#[tokio::test]
async fn canonical_completion_enforces_latest_through_signed_http() -> Result<()> {
    completion_enforces(Scenario::Latest).await
}

#[tokio::test]
async fn canonical_completion_enforces_invalid_through_signed_http() -> Result<()> {
    completion_enforces(Scenario::Invalid).await
}

#[tokio::test]
async fn canonical_completion_enforces_setup_through_signed_http() -> Result<()> {
    completion_enforces(Scenario::Setup).await
}

#[tokio::test]
async fn canonical_completion_enforces_revoked_through_signed_http() -> Result<()> {
    completion_enforces(Scenario::Revoked).await
}
