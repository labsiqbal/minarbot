//! Canonical workflow runs: capacity, reservations, output, admission and
//! scope checks.

use super::{
    fixture::{account, direct, encode, task_body, until, Fault, SETTLE},
    workflow_waits::{connected, WorkflowHome},
    workflows::{create, start_run, wait_run},
};
use minarbot_testkit::prelude::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// Text of a string value, or the JSON of any other non-null value.
fn id_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn wait_graph(timeout: i64) -> Value {
    json!({"entry": "event", "maxSteps": 2, "nodes": [
        {"id": "event", "kind": "wait", "event": "ready", "timeoutSeconds": timeout, "next": "out"},
        {"id": "out", "kind": "output"}]})
}

async fn status(w: &WorkflowHome, path: &str) -> Result<Value> {
    Ok(w.tp.data(path).await?["status"].clone())
}

/// Let `subject` use the workflow agent in the project.
async fn collaborator(w: &WorkflowHome) -> Result<String> {
    let collaborator = account(&w.tp, "user_collaborator").await?;
    let db = w.tp.write()?;
    db.execute(
        "INSERT INTO project_members VALUES (?,?,'member',1,0)",
        [&w.project, &collaborator],
    )?;
    db.execute(
        "INSERT INTO collaborator_agents VALUES (?,?,?,1,1)",
        [&w.project, &collaborator, &w.agent],
    )?;
    Ok(collaborator)
}

#[tokio::test]
async fn canonical_workflow_capacity_retries_leave_no_empty_threads_before_admission(
) -> Result<()> {
    let w = WorkflowHome::start().await?;
    let result = async {
        let home = id_of(&w.home["id"]);
        for _ in 0..100 {
            let task = new_id();
            let (status, body) = w
                .tp
                .send(
                    "/api/tasks",
                    Some(task_body(&task, &w.project, &w.agent, &home, &direct("Fill queue"))),
                )
                .await?;
            ensure!(status == 202, "task: {status} {body}");
        }
        let graph = json!({"entry": "work", "maxSteps": 2, "nodes": [
            {"id": "work", "kind": "agent", "agent": w.agent, "prompt": "Wait for capacity", "next": "out"},
            {"id": "out", "kind": "output"}]});
        create(&w, "Capacity", &graph).await?;
        let (_, path) = start_run(&w, "").await?;
        let edit = new_id();
        let (status, _) = w
            .tp
            .send(
                &format!("{}/edit", w.detail),
                Some(json!({"requestId": edit, "id": edit, "revision": 1, "name": "Output",
                    "graph": w.graph})),
            )
            .await?;
        ensure!(status == 200, "edit: {status}");
        let later = new_id();
        let later_path = format!("/api/projects/{}/workflow-runs/{later}", w.project);
        let (status, _) = w
            .tp
            .send(
                &format!("{}/run", w.detail),
                Some(json!({"requestId": later, "id": later, "revision": 2, "input": "Later"})),
            )
            .await?;
        ensure!(status == 201, "later run: {status}");
        wait_run(&w, &later_path, |run| run["status"] == "done").await?;
        ensure!(status_of(&w, &path).await? == "ready", "blocked run status");
        ensure!(
            w.tp.inspect()?.count("threads")? == 0,
            "Queue backpressure must not persist empty workflow threads"
        );
        let task = w
            .tp
            .inspect()?
            .row("SELECT id,revision FROM tasks ORDER BY created LIMIT 1", [])?;
        let (status, _) = w
            .tp
            .send(
                &format!("/api/tasks/{}/control", id_of(&task["id"])),
                Some(json!({"requestId": new_id(), "expectedRevision": task["revision"],
                    "operation": "cancel"})),
            )
            .await?;
        ensure!(status == 200, "cancel: {status}");
        wait_run(&w, &path, |run| run["status"] == "waiting").await?;
        ensure!(w.tp.inspect()?.count("threads")? == 1, "threads after admission");
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

async fn status_of(w: &WorkflowHome, path: &str) -> Result<String> {
    Ok(id_of(&status(w, path).await?))
}

#[tokio::test]
async fn canonical_workflow_revoked_assignment_before_host_ack_closes_the_original_run(
) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        let collaborator = collaborator(&w).await?;
        create(&w, "Revoked reservation", &wait_graph(60)).await?;
        let run = new_id();
        let path = format!("/api/projects/{}/workflow-runs/{run}", w.project);
        let (status, body) = w
            .tp
            .send_as(
                &format!("{}/run", w.detail),
                Some(json!({"requestId": run, "id": run, "revision": 1, "input": ""})),
                "user_collaborator",
                None,
            )
            .await?;
        ensure!(status == 201, "run: {status} {body}");
        let target = json!(["workflow", run, 1]).to_string();
        until("reservation", SETTLE, || async {
            Ok(w.tp.inspect()?.scalar::<i64>(
                "SELECT count(*) FROM wait_provisions WHERE target_key=?",
                [&target],
            )? == 1)
        })
        .await?;
        let revoked = Arc::new(AtomicBool::new(false));
        let (database, project, account) =
            (w.tp.database(), w.project.clone(), collaborator.clone());
        w.tp.fault(Some(Arc::new(move |exchange| {
            if exchange.path.ends_with("/wait-secret-results") && !revoked.swap(true, Ordering::SeqCst)
            {
                if let Ok(db) = minarbot_testkit::db::Fixture::open(&database) {
                    let _ = db.execute(
                        "UPDATE collaborator_agents SET enabled=0,revision=revision+1 WHERE project=? AND account=?",
                        [&project, &account],
                    );
                }
            }
            Fault::Pass
        })));
        let connection = w.host.connect().await?;
        until("connected", SETTLE, || async { Ok(connection.phase() == "connected") }).await?;
        until("reservation closed", SETTLE, || async {
            Ok(w.tp.inspect()?.scalar::<i64>(
                "SELECT count(*) FROM wait_provisions WHERE state='closed' AND result_request_id IS NOT NULL",
                [],
            )? > 0)
        })
        .await?;
        w.tp.fault(None);
        w.tp.write()?.execute(
            "UPDATE collaborator_agents SET enabled=1,revision=revision+1 WHERE project=? AND account=?",
            [&w.project, &collaborator],
        )?;
        ensure!(status_of(&w, &path).await? == "failed", "run status");
        ensure!(w.tp.data(&path).await?["error"] == "wait_invalid", "run error");
        ensure!(
            w.tp.send(&format!("/api/workflow-waits/{run}/1"), None).await?.0 == 404,
            "wait still readable"
        );
        w.tp.restart().await?;
        ensure!(status_of(&w, &path).await? == "failed", "status after restart");
        ensure!(w.tp.inspect()?.count("wait_provisions")? == 1, "provisions");
        connection.stop().await?;
        Ok(())
    }
    .await;
    w.tp.fault(None);
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_workflow_wait_reserves_once_offline_and_activates_only_after_home_keyring_acknowledgement(
) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        create(&w, "Protected wait", &wait_graph(60)).await?;
        let (run, path) = start_run(&w, "Synthetic").await?;
        let wait_path = format!("/api/workflow-waits/{run}/1");
        let target = json!(["workflow", run, 1]).to_string();
        until("reservation", SETTLE, || async {
            Ok(w.tp.inspect()?.scalar::<i64>(
                "SELECT count(*) FROM wait_provisions WHERE target_key=?",
                [&target],
            )? == 1)
        })
        .await?;
        let reservation = w
            .tp
            .inspect()?
            .row("SELECT * FROM wait_provisions WHERE target_key=?", [&target])?;
        ensure!(status_of(&w, &path).await? == "ready", "offline status");
        ensure!(w.tp.data(&path).await?["stepCount"] == 0, "offline steps");
        w.tp.restart().await?;
        ensure!(
            w.tp.inspect()?.row("SELECT * FROM wait_provisions WHERE id=?", [id_of(&reservation["id"])])?
                == reservation,
            "reservation changed across restart"
        );
        let connection = w.connect()?;
        connected(&connection).await?;
        until("wait waiting", SETTLE, || async {
            Ok(w.tp.send(&wait_path, None).await?.1["data"]["status"] == "waiting")
        })
        .await?;
        let current = w.tp.data(&path).await?;
        let wait = w.tp.data(&wait_path).await?;
        ensure!(current["status"] == "waiting", "run status");
        ensure!(current["stepCount"] == 1, "step count");
        ensure!(current["steps"][0]["node"] == "event", "step node");
        ensure!(wait["deadline"] == reservation["deadline"], "deadline");
        ensure!(wait["sourceDeviceId"] == w.home["id"], "source device");
        ensure!(wait["secretGeneration"] == 1, "secret generation");
        ensure!(wait.get("secret").is_none(), "secret exposed");
        ensure!(wait.get("secretRef").is_none(), "secret reference exposed");
        ensure!(
            !w.tp
                .inspect()?
                .rows("PRAGMA table_info(workflow_waits)", [])?
                .iter()
                .any(|row| row["name"] == "secret"),
            "secret column"
        );
        let generation = connection.state()["generation"].as_i64().unwrap_or(0);
        w.tp.restart().await?;
        until("reconnected", SETTLE, || async {
            let state = connection.state();
            Ok(state["phase"] == "connected" && state["generation"].as_i64().unwrap_or(0) > generation)
        })
        .await?;
        ensure!(w.tp.data(&path).await?["stepCount"] == 1, "steps after restart");
        ensure!(w.tp.data(&wait_path).await? == wait, "wait changed across restart");
        let cancel = new_id();
        let (status, _) = w
            .tp
            .send(
                &format!("{path}/cancel"),
                Some(json!({"requestId": cancel, "id": cancel, "revision": current["revision"]})),
            )
            .await?;
        ensure!(status == 200, "cancel: {status}");
        ensure!(w.tp.data(&wait_path).await?["status"] == "closed", "wait closed");
        drop(connection);
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_workflow_timeout_fails_offline_reservation_without_generating_a_second_wait(
) -> Result<()> {
    let w = WorkflowHome::start().await?;
    let result = async {
        create(&w, "Timeout", &wait_graph(1)).await?;
        let sync = format!("/api/sync?projectId={}", w.project);
        let before = w.tp.data(&sync).await?;
        let (run, path) = start_run(&w, "Offline").await?;
        let current = wait_run(&w, &path, |run| run["status"] == "failed").await?;
        ensure!(current["error"] == "wait_expired", "error: {current}");
        ensure!(current["stepCount"] == 1, "step count");
        ensure!(current["steps"][0]["status"] == "failed", "step status");
        let provisions = w.tp.inspect()?.rows("SELECT * FROM wait_provisions", [])?;
        ensure!(provisions.len() == 1, "provisions: {provisions:?}");
        ensure!(provisions[0]["state"] == "closed", "provision state");
        let connection = w.connect()?;
        connected(&connection).await?;
        ensure!(status_of(&w, &path).await? == "failed", "status after connect");
        ensure!(
            w.tp.inspect()?.rows("SELECT * FROM wait_provisions", [])? == provisions,
            "second wait generated"
        );
        ensure!(
            w.tp.send(&format!("/api/workflow-waits/{run}/1"), None).await?.0 == 404,
            "wait readable"
        );
        let cursor = id_of(&before["nextCursor"]);
        let page = w.tp.data(&format!("{sync}&cursor={}", encode(&cursor))).await?;
        let observations: Vec<(Value, Value)> = page["page"]["events"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|event| event["recordKind"] == "workflow_run")
            .map(|event| (event["revision"].clone(), event["body"]["status"].clone()))
            .collect();
        ensure!(
            observations == vec![(json!(1), json!("ready")), (json!(2), json!("failed"))],
            "observations: {observations:?}"
        );
        drop(connection);
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_workflow_channel_output_is_atomic_and_does_not_duplicate_after_restart(
) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        let owner = account(&w.tp, "user_owner").await?;
        let channel = new_id();
        let db = w.tp.write()?;
        db.execute(
            "INSERT INTO channels(id,name,instructions,lead,revision,archived) VALUES (?,?,'',?,1,0)",
            [&channel, "Workflow output", &w.agent],
        )?;
        db.execute("INSERT INTO channel_members VALUES (?,?)", [&channel, &w.agent])?;
        db.execute(
            "INSERT INTO record_scopes VALUES ('channels',?,?,?,'project','channel',?)",
            [&json!([channel]).to_string(), &owner, &w.project, &channel],
        )?;
        let graph = json!({"entry": "out", "maxSteps": 1, "nodes": [
            {"id": "out", "kind": "output", "channel": channel}]});
        create(&w, "Output", &graph).await?;
        db.execute_batch("CREATE TRIGGER fail_workflow_output BEFORE INSERT ON record_scopes WHEN NEW.table_name='workflow_steps' BEGIN SELECT RAISE(ABORT,'output mapping failure'); END")?;
        let (_, path) = start_run(&w, "Exact result").await?;
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        let database = w.tp.database();
        let channel_turns = || -> Result<i64> {
            minarbot_testkit::db::Inspector::open(&database)?
                .scalar("SELECT count(*) FROM turns WHERE channel=?", [&channel])
        };
        ensure!(status_of(&w, &path).await? == "ready", "status under failure");
        ensure!(channel_turns()? == 0, "Failed step mapping rolls back channel output");
        db.execute_batch("DROP TRIGGER fail_workflow_output")?;
        let done = wait_run(&w, &path, |run| run["status"] == "done").await?;
        ensure!(done["last"] == "Exact result", "last: {done}");
        let turn = id_of(&done["steps"][0]["turn"]);
        ensure!(!turn.is_empty(), "output turn");
        ensure!(
            w.tp.inspect()?.scalar::<String>("SELECT reply FROM turns WHERE id=?", [&turn])?
                == "Exact result",
            "reply"
        );
        drop(db);
        w.tp.restart().await?;
        ensure!(w.tp.data(&path).await?["stepCount"] == 1, "steps after restart");
        ensure!(channel_turns()? == 1, "channel turns after restart");
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_workflow_run_admission_keeps_its_graph_and_receipt_across_restart_and_revision_changes(
) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        let graph = w.graph.clone();
        create(&w, "Run", &graph).await?;
        w.tp.write()?.execute_batch(HOLD_STEPS)?;
        let request = new_id();
        let command = json!({"requestId": request, "id": request, "revision": 1,
            "input": "Original input"});
        let path = format!("/api/projects/{}/workflow-runs/{request}", w.project);
        let run = format!("{}/run", w.detail);
        let (status, accepted) = w.tp.send(&run, Some(command.clone())).await?;
        ensure!(status == 201, "admission: {status} {accepted}");
        ensure!(accepted["data"]["id"] == request.as_str(), "run id");
        ensure!(accepted["data"]["homeDeviceId"] == w.home["id"], "run home");
        let (status, read) = w.tp.send(&path, None).await?;
        ensure!(status == 200, "read: {status}");
        ensure!(read["data"]["graph"] == graph, "graph");
        ensure!(read["data"]["input"] == command["input"], "input");
        let edit = new_id();
        let mut changed = graph.clone();
        changed["maxSteps"] = json!(2);
        let (status, _) = w
            .tp
            .send(
                &format!("{}/edit", w.detail),
                Some(json!({"requestId": edit, "id": edit, "revision": 1, "name": "Changed",
                    "graph": changed})),
            )
            .await?;
        ensure!(status == 200, "edit: {status}");
        w.tp.restart().await?;
        let (status, replay) = w.tp.send(&run, Some(command.clone())).await?;
        ensure!(status == 200, "replay: {status}");
        ensure!(replay["receipt"] == accepted["receipt"], "replay receipt");
        ensure!(w.tp.data(&path).await?["graph"] == graph, "graph after edit");
        ensure!(w.tp.data(&path).await?["version"] == 1, "version");
        ensure!(
            w.tp.send(&format!("/api/projects/{}/workflow-runs/{request}", w.other), None)
                .await?
                .0
                == 404,
            "foreign project read"
        );
        let listed = w.tp.data(&format!("{}/runs", w.detail)).await?;
        let ids: Vec<Value> = listed["items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|row| row["id"].clone())
            .collect();
        ensure!(ids == vec![json!(request)], "listed: {listed}");
        ensure!(listed["nextCursor"].is_null(), "next cursor");
        let mut changed_payload = command.clone();
        changed_payload["input"] = json!("Changed payload");
        ensure!(w.tp.send(&run, Some(changed_payload)).await?.0 == 409, "changed payload");
        let stale = new_id();
        let mut stale_command = command.clone();
        stale_command["requestId"] = json!(stale);
        stale_command["id"] = json!(stale);
        ensure!(w.tp.send(&run, Some(stale_command)).await?.0 == 409, "stale revision");
        let cancel_id = new_id();
        let cancel = json!({"requestId": cancel_id, "id": cancel_id, "revision": read["data"]["revision"]});
        let (status, cancelled) = w.tp.send(&format!("{path}/cancel"), Some(cancel.clone())).await?;
        ensure!(status == 200, "cancel: {status} {cancelled}");
        ensure!(status_of(&w, &path).await? == "cancelled", "cancelled");
        ensure!(
            w.tp.send(&format!("{path}/cancel"), Some(cancel.clone())).await?.1 == cancelled,
            "cancel replay"
        );
        let next = new_id();
        let mut again = cancel.clone();
        again["requestId"] = json!(next);
        again["id"] = json!(next);
        ensure!(w.tp.send(&format!("{path}/cancel"), Some(again)).await?.0 == 409, "second cancel");
        ensure!(
            w.tp.send(&format!("/api/workflow-runs/{request}?before=1"), None).await?.0 == 404,
            "unscoped run path"
        );
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

/// Keep the scheduler from executing steps: it settles a run as soon as it
/// is admitted, and the step mapping insert fails while this trigger exists.
/// The Node cases raced that settle and failed at base.
const HOLD_STEPS: &str = "CREATE TRIGGER hold_workflow_steps BEFORE INSERT ON record_scopes WHEN NEW.table_name='workflow_steps' BEGIN SELECT RAISE(ABORT,'held workflow step'); END";

fn with_id(command: &Value, id: &str) -> Value {
    let mut command = command.clone();
    command["requestId"] = json!(id);
    command["id"] = json!(id);
    command
}

#[tokio::test]
async fn canonical_workflow_runs_deny_revoked_private_and_unmapped_state_and_roll_back_controls_with_receipts(
) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        let graph = json!({"entry": "approve", "maxSteps": 2, "nodes": [
            {"id": "approve", "kind": "approval", "prompt": "Accept?", "yes": "out"},
            {"id": "out", "kind": "output"}]});
        create(&w, "Approval", &graph).await?;
        let owner = account(&w.tp, "user_owner").await?;
        let collaborator = collaborator(&w).await?;
        let db = w.tp.write()?;
        db.execute_batch(HOLD_STEPS)?;
        db.execute(
            "INSERT INTO response_policies VALUES (?,?,?,0,1)",
            [&w.project, &collaborator, &w.agent],
        )?;
        let run = new_id();
        let command = json!({"requestId": run, "id": run, "revision": 1, "input": "Approval input"});
        let path = format!("/api/projects/{}/workflow-runs/{run}", w.project);
        let admit = format!("{}/run", w.detail);
        let runs = format!("{}/runs", w.detail);
        let as_collaborator = |path: String, body: Option<Value>| {
            let tp = &w.tp;
            async move { tp.send_as(&path, body, "user_collaborator", None).await }
        };
        let (status, accepted) = as_collaborator(admit.clone(), Some(command.clone())).await?;
        ensure!(status == 201, "admission: {status} {accepted}");
        db.execute(
            "UPDATE collaborator_agents SET enabled=0,revision=revision+1 WHERE project=? AND account=?",
            [&w.project, &collaborator],
        )?;
        ensure!(as_collaborator(path.clone(), None).await?.0 == 404, "revoked read");
        ensure!(
            as_collaborator(admit.clone(), Some(command.clone())).await?.0 == 404,
            "revoked admission replay"
        );
        ensure!(as_collaborator(runs.clone(), None).await?.0 == 404, "revoked list");
        db.execute(
            "UPDATE collaborator_agents SET enabled=1,revision=revision+1 WHERE project=? AND account=?",
            [&w.project, &collaborator],
        )?;
        let key = json!([run]).to_string();
        db.execute(
            "UPDATE record_scopes SET visibility='owner',account=? WHERE table_name='workflow_runs' AND key_json=?",
            [&owner, &key],
        )?;
        ensure!(as_collaborator(path.clone(), None).await?.0 == 404, "private read");
        ensure!(
            as_collaborator(runs.clone(), None).await?.1["data"]["items"] == json!([]),
            "private list"
        );
        db.execute(
            "UPDATE record_scopes SET visibility='project' WHERE table_name='workflow_runs' AND key_json=?",
            [&key],
        )?;
        let version = json!([w.workflow, 1]).to_string();
        db.execute(
            "UPDATE record_scopes SET project=? WHERE table_name='workflow_versions' AND key_json=?",
            [&w.other, &version],
        )?;
        ensure!(w.tp.send(&path, None).await?.0 == 404, "unmapped version read");
        ensure!(
            as_collaborator(admit.clone(), Some(command.clone())).await?.0 == 404,
            "unmapped version admission"
        );
        db.execute(
            "UPDATE record_scopes SET project=? WHERE table_name='workflow_versions' AND key_json=?",
            [&w.project, &version],
        )?;
        db.execute(
            "INSERT INTO workflow_steps(run,seq,node,status,settings) VALUES (?,1,'approve','approval',?)",
            [&run, &graph["nodes"][0].to_string()],
        )?;
        db.execute(
            "UPDATE workflow_runs SET steps=1,status='approval',revision=2 WHERE id=?",
            [&run],
        )?;
        let approval_id = new_id();
        let approval = json!({"requestId": approval_id, "id": approval_id, "revision": 2,
            "accepted": true});
        let approve = format!("{path}/approve");
        ensure!(
            w.tp.send(&approve, Some(approval.clone())).await?.0 == 404,
            "Unmapped step cannot be approved"
        );
        let cancellation = new_id();
        ensure!(
            w.tp.send(
                &format!("{path}/cancel"),
                Some(json!({"requestId": cancellation, "id": cancellation, "revision": 2}))
            )
            .await?
            .0 == 404,
            "Unmapped step cannot be cancelled"
        );
        db.execute_batch("DROP TRIGGER hold_workflow_steps")?;
        db.execute(
            "INSERT INTO record_scopes VALUES ('workflow_steps',?,?,?,'project','workflow',?)",
            [&json!([run, 1]).to_string(), &collaborator, &w.project, &w.workflow],
        )?;
        db.execute_batch(HOLD_STEPS)?;
        db.execute_batch("CREATE TRIGGER fail_run_receipt BEFORE INSERT ON receipts WHEN NEW.scope LIKE 'retained:%/workflow-runs/%/approve' BEGIN SELECT RAISE(ABORT,'run receipt failure'); END")?;
        ensure!(w.tp.send(&approve, Some(approval.clone())).await?.0 == 500, "receipt failure");
        ensure!(status_of(&w, &path).await? == "approval", "run rolled back");
        ensure!(w.tp.data(&path).await?["steps"][0]["status"] == "approval", "step rolled back");
        db.execute_batch("DROP TRIGGER fail_run_receipt")?;
        let (status, approved) = w.tp.send(&approve, Some(approval.clone())).await?;
        ensure!(status == 200, "approve: {status} {approved}");
        ensure!(status_of(&w, &path).await? == "ready", "approved status");
        ensure!(w.tp.data(&path).await?["cursor"] == "out", "cursor");
        drop(db);
        w.tp.restart().await?;
        let db = w.tp.write()?;
        ensure!(
            w.tp.send(&approve, Some(approval.clone())).await?.1 == approved,
            "approval replay"
        );
        ensure!(
            w.tp.send(&approve, Some(with_id(&approval, &new_id()))).await?.0 == 409,
            "stale approval"
        );
        let failed_id = new_id();
        let failed = with_id(&command, &failed_id);
        db.execute_batch("CREATE TRIGGER fail_run_mapping BEFORE INSERT ON record_scopes WHEN NEW.table_name='workflow_runs' BEGIN SELECT RAISE(ABORT,'run mapping failure'); END")?;
        ensure!(w.tp.send(&admit, Some(failed.clone())).await?.0 == 500, "mapping failure");
        ensure!(
            db.scalar::<i64>("SELECT count(*) FROM workflow_runs WHERE id=?", [&failed_id])? == 0,
            "failed run kept"
        );
        ensure!(
            db.scalar::<i64>("SELECT count(*) FROM receipts WHERE request_id=?", [&failed_id])? == 0,
            "failed receipt kept"
        );
        db.execute_batch("DROP TRIGGER fail_run_mapping")?;
        ensure!(w.tp.send(&admit, Some(failed)).await?.0 == 201, "admission after failure");
        for query in [
            format!("projectId={}", w.project),
            "before=0".into(),
            "before=1&before=2".into(),
            "before=x".into(),
        ] {
            ensure!(
                w.tp.send(&format!("{runs}?{query}"), None).await?.0 == 404,
                "runs query {query}"
            );
        }
        ensure!(w.tp.send(&format!("{path}?after=0"), None).await?.0 == 404, "after=0");
        ensure!(
            w.tp.send(
                &format!("{path}/cancel"),
                Some(json!({"requestId": new_id(), "id": new_id(), "revision": 3,
                    "projectId": w.project}))
            )
            .await?
            .0 == 404,
            "cancel with projectId"
        );
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_workflow_run_and_step_pages_preserve_old_approvals_and_admission_rolls_back_failed_receipts(
) -> Result<()> {
    let w = WorkflowHome::start().await?;
    let result = async {
        let graph = w.graph.clone();
        create(&w, "Pages", &graph).await?;
        let admit = format!("{}/run", w.detail);
        let mut runs = Vec::new();
        for index in 0..23 {
            let request = new_id();
            runs.push(json!(request));
            let (status, _) = w
                .tp
                .send(
                    &admit,
                    Some(json!({"requestId": request, "id": request, "revision": 1,
                        "input": index.to_string()})),
                )
                .await?;
            ensure!(status == 201, "run {index}: {status}");
        }
        let first = w.tp.data(&format!("{}/runs", w.detail)).await?;
        let second = w
            .tp
            .data(&format!("{}/runs?before={}", w.detail, id_of(&first["nextCursor"])))
            .await?;
        let items = |page: &Value| page["items"].as_array().cloned().unwrap_or_default();
        ensure!(items(&first).len() == 20, "first page");
        ensure!(items(&second).len() == 3, "second page");
        ensure!(second["nextCursor"].is_null(), "second cursor");
        let listed: Vec<Value> = items(&first)
            .into_iter()
            .chain(items(&second))
            .map(|row| row["id"].clone())
            .collect();
        let mut reversed = runs.clone();
        reversed.reverse();
        ensure!(listed == reversed, "run order");
        let run = id_of(&runs[0]);
        let path = format!("/api/projects/{}/workflow-runs/{run}", w.project);
        let owner = account(&w.tp, "user_owner").await?;
        let db = w.tp.write()?;
        db.execute_batch("BEGIN IMMEDIATE")?;
        db.execute("UPDATE workflow_runs SET status='waiting',steps=0 WHERE id=?", [&run])?;
        db.execute(
            "DELETE FROM record_scopes WHERE table_name='workflow_steps' AND json_extract(key_json,'$[0]')=?",
            [&run],
        )?;
        db.execute("DELETE FROM workflow_steps WHERE run=?", [&run])?;
        for seq in 1..=21 {
            db.execute(
                "INSERT INTO workflow_steps(run,seq,node,status) VALUES (?,?,?,'approval')",
                minarbot_testkit::hosted::params![run, seq, "out"],
            )?;
            db.execute(
                "INSERT INTO record_scopes VALUES ('workflow_steps',?,?,?,'project','workflow',?)",
                [&json!([run, seq]).to_string(), &owner, &w.project, &w.workflow],
            )?;
        }
        db.execute_batch("COMMIT")?;
        let steps = w.tp.data(&path).await?;
        let tail = w
            .tp
            .data(&format!("{path}?after={}", id_of(&steps["nextCursor"])))
            .await?;
        ensure!(steps["steps"].as_array().map(Vec::len) == Some(20), "step page");
        ensure!(tail["steps"].as_array().map(Vec::len) == Some(1), "step tail");
        ensure!(tail["nextCursor"].is_null(), "tail cursor");
        ensure!(tail["steps"][0]["seq"] == 21, "tail seq");
        let request = new_id();
        let command = json!({"requestId": request, "id": request, "revision": 1, "input": "Atomic"});
        db.execute_batch("CREATE TRIGGER fail_run_receipt BEFORE INSERT ON receipts WHEN NEW.scope LIKE 'retained:%/workflows/%/run' BEGIN SELECT RAISE(ABORT,'run receipt failure'); END")?;
        ensure!(w.tp.send(&admit, Some(command.clone())).await?.0 == 500, "receipt failure");
        ensure!(
            db.scalar::<i64>("SELECT count(*) FROM workflow_runs WHERE id=?", [&request])? == 0,
            "run kept"
        );
        ensure!(
            db.scalar::<i64>(
                "SELECT count(*) FROM record_scopes WHERE table_name='workflow_runs' AND key_json=?",
                [&json!([request]).to_string()]
            )? == 0,
            "scope kept"
        );
        db.execute_batch("DROP TRIGGER fail_run_receipt")?;
        ensure!(w.tp.send(&admit, Some(command.clone())).await?.0 == 201, "admission");
        let foreign = account(&w.tp, "user_stranger").await?;
        let home = id_of(&w.home["id"]);
        db.execute(
            "UPDATE installations SET account=? WHERE id=(SELECT installation FROM devices WHERE id=?)",
            [&foreign, &home],
        )?;
        let foreign_id = new_id();
        ensure!(
            w.tp.send(&admit, Some(with_id(&command, &foreign_id))).await?.0 == 404,
            "Device installation must belong to its account"
        );
        ensure!(
            db.scalar::<i64>("SELECT count(*) FROM workflow_runs WHERE id=?", [&foreign_id])? == 0,
            "foreign run kept"
        );
        ensure!(
            db.scalar::<i64>("SELECT count(*) FROM receipts WHERE request_id=?", [&foreign_id])? == 0,
            "foreign receipt kept"
        );
        db.execute(
            "UPDATE installations SET account=? WHERE id=(SELECT installation FROM devices WHERE id=?)",
            [&owner, &home],
        )?;
        db.execute("UPDATE devices SET status='revoked' WHERE id=?", [&home])?;
        ensure!(
            w.tp.send(&admit, Some(with_id(&command, &new_id()))).await?.0 == 404,
            "revoked device admission"
        );
        ensure!(
            w.tp.send(&admit, Some(command)).await?.0 == 200,
            "Revoked device never re-dispatches an already accepted command"
        );
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}
