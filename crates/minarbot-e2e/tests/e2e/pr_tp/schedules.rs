//! Canonical workflow schedules on a home (offline coalescing, zones, cron).

use super::{
    fixture::{until, Tp, SETTLE},
    workflow_waits::{connected, WorkflowHome},
};
use minarbot_testkit::{prelude::*, Servers};

fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

async fn create_workflow(w: &WorkflowHome, name: &str) -> Result<()> {
    let (status, body) =
        w.tp.send(
            &w.list,
            Some(
                json!({"requestId": w.workflow, "id": w.workflow, "revision": 0, "name": name,
                "graph": w.graph}),
            ),
        )
        .await?;
    ensure!(status == 201, "workflow: {status} {body}");
    Ok(())
}

async fn schedule(w: &WorkflowHome) -> Result<Value> {
    w.tp.data(&format!("{}/schedule", w.detail)).await
}

async fn wait_schedule(w: &WorkflowHome, done: impl Fn(&Value) -> bool) -> Result<Value> {
    minarbot_testkit::wait::until_within("schedule", SETTLE, || async {
        let current = schedule(w).await?;
        Ok(done(&current).then_some(current))
    })
    .await
}

async fn run_done(w: &WorkflowHome, run: &Value) -> Result<()> {
    let path = format!(
        "/api/projects/{}/workflow-runs/{}",
        w.project,
        run.as_str().unwrap_or_default()
    );
    until("run done", SETTLE, || async {
        Ok(w.tp.data(&path).await?["status"] == "done")
    })
    .await
}

fn occurrences(value: &Value) -> usize {
    value["occurrences"].as_array().map_or(0, Vec::len)
}

#[tokio::test]
async fn canonical_workflow_schedule_coalesces_offline_intervals_with_one_durable_identity_across_restart(
) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        create_workflow(&w, "Scheduled").await?;
        let request = new_id();
        let anchor = wall_ms() - 960_000;
        let command = json!({"requestId": request, "id": request, "revision": 0,
            "spec": {"kind": "interval", "amount": 5, "unit": "minute", "anchorAt": anchor},
            "input": "Scheduled input", "enabled": true});
        let (status, accepted) =
            w.tp.send(&format!("{}/schedule", w.detail), Some(command.clone()))
                .await?;
        ensure!(status == 201, "schedule: {status} {accepted}");
        ensure!(
            accepted["data"]["homeDeviceId"] == w.home["id"],
            "home: {accepted}"
        );
        let current = wait_schedule(&w, |current| occurrences(current) == 1).await?;
        let occurrence = current["occurrences"][0].clone();
        ensure!(occurrence["state"] == "pending", "occurrence: {occurrence}");
        ensure!(occurrence["dueAt"] == anchor, "due: {occurrence}");
        ensure!(occurrence["missedCount"] == 3, "missed: {occurrence}");
        let id = occurrence["occurrenceId"].as_str().unwrap_or_default();
        ensure!(
            id.len() == 43
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "occurrence id: {id}"
        );
        ensure!(
            w.tp.data(&format!("{}/runs", w.detail)).await?["items"] == json!([]),
            "runs before the home connected"
        );
        w.tp.restart().await?;
        ensure!(
            schedule(&w).await?["occurrences"] == current["occurrences"],
            "occurrences changed across restart"
        );
        let mut connection = w.connect()?;
        connected(&connection).await?;
        let current = wait_schedule(&w, |current| {
            current["occurrences"][0]["state"] == "admitted"
        })
        .await?;
        ensure!(occurrences(&current) == 1, "occurrences: {current}");
        ensure!(
            current["occurrences"][0]["occurrenceId"] == occurrence["occurrenceId"],
            "identity changed"
        );
        ensure!(
            current["nextAt"].as_i64().unwrap_or(0) > wall_ms(),
            "next: {current}"
        );
        ensure!(current["revision"] == 1, "revision: {current}");
        let run = current["occurrences"][0]["runId"].clone();
        run_done(&w, &run).await?;
        let path = format!(
            "/api/projects/{}/workflow-runs/{}",
            w.project,
            run.as_str().unwrap_or_default()
        );
        ensure!(
            w.tp.data(&path).await?["input"] == command["input"],
            "run input"
        );
        ensure!(
            w.tp.data(&format!("{}/runs", w.detail)).await?["items"]
                .as_array()
                .map(Vec::len)
                == Some(1),
            "runs"
        );
        ensure!(
            w.tp.send(&format!("{}/schedule", w.detail), Some(command))
                .await?
                .1["receipt"]
                == accepted["receipt"],
            "receipt replay"
        );
        w.tp.restart().await?;
        ensure!(
            occurrences(&schedule(&w).await?) == 1,
            "occurrences after restart"
        );
        ensure!(
            w.tp.send(
                &format!(
                    "/api/projects/{}/workflows/{}/schedule",
                    w.other, w.workflow
                ),
                None
            )
            .await?
            .0 == 404,
            "foreign project schedule"
        );
        connection.stop().await?;
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_workflow_once_uses_its_home_zone_and_consumes_one_catch_up_after_offline_restart(
) -> Result<()> {
    let servers = Servers::reserve(1).await?;
    // Node set `process.env.TZ`, which the server and the home inherited.
    let zone = [("TZ", "America/New_York")];
    let mut w = WorkflowHome::start_env(Tp::start_env(&servers, &zone).await?, &zone).await?;
    let result = async {
        create_workflow(&w, "Wall clock").await?;
        let request = new_id();
        let command = json!({"requestId": request, "id": request, "revision": 0,
            "spec": {"kind": "once", "date": "2024-03-10", "time": "02:30"},
            "input": "Wall clock input", "enabled": true});
        let path = format!("{}/schedule", w.detail);
        let (status, saved) = w.tp.send(&path, Some(command.clone())).await?;
        ensure!(status == 201, "schedule: {status} {saved}");
        // 2024-03-10T07:00:00Z: 02:30 does not exist on that New York day.
        ensure!(
            saved["data"]["nextAt"] == 1_710_054_000_000_i64,
            "next: {saved}"
        );
        ensure!(saved["data"]["zone"] == "America/New_York", "zone: {saved}");
        let current = wait_schedule(&w, |current| occurrences(current) == 1).await?;
        let pending = current["occurrences"][0].clone();
        ensure!(pending["state"] == "pending", "pending: {pending}");
        ensure!(
            pending["dueAt"] == saved["data"]["nextAt"],
            "due: {pending}"
        );
        w.tp.restart().await?;
        ensure!(
            schedule(&w).await?["occurrences"] == json!([pending]),
            "occurrence changed across restart"
        );
        let mut connection = w.connect()?;
        connected(&connection).await?;
        let current = wait_schedule(&w, |current| {
            current["occurrences"][0]["state"] == "admitted"
        })
        .await?;
        ensure!(current["enabled"] == false, "enabled: {current}");
        ensure!(current["nextAt"].is_null(), "next: {current}");
        ensure!(occurrences(&current) == 1, "occurrences: {current}");
        ensure!(
            current["occurrences"][0]["occurrenceId"] == pending["occurrenceId"],
            "identity changed"
        );
        ensure!(
            current["occurrences"][0]["zoneRevision"] == 1,
            "zone revision"
        );
        run_done(&w, &current["occurrences"][0]["runId"]).await?;
        ensure!(
            w.tp.send(&path, Some(command.clone())).await?.1["receipt"] == saved["receipt"],
            "receipt replay"
        );
        let replay = new_id();
        let mut next = command.clone();
        next["requestId"] = json!(replay);
        next["id"] = json!(replay);
        next["revision"] = json!(1);
        let (status, denied) = w.tp.send(&path, Some(next)).await?;
        ensure!(status == 409, "processed once: {status}");
        ensure!(
            denied["error"]["code"] == "schedule_processed",
            "code: {denied}"
        );
        ensure!(
            w.tp.data(&format!("{}/runs", w.detail)).await?["items"]
                .as_array()
                .map(Vec::len)
                == Some(1),
            "runs"
        );
        connection.stop().await?;
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_workflow_cron_validates_http_input_and_admits_one_retained_overdue_occurrence_after_reconnect(
) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        create_workflow(&w, "Cron").await?;
        let path = format!("{}/schedule", w.detail);
        let request = new_id();
        let command = json!({"requestId": request, "id": request, "revision": 0,
            "spec": {"kind": "cron", "expression": "*/5 * * * *"}, "input": "Private cron input",
            "enabled": true});
        for expression in ["* * * * *", "0 0 31 2 *", "0 9 * * MON"] {
            let invalid = new_id();
            let mut body = command.clone();
            body["requestId"] = json!(invalid);
            body["id"] = json!(invalid);
            body["spec"] = json!({"kind": "cron", "expression": expression});
            let (status, response) = w.tp.send(&path, Some(body)).await?;
            ensure!(
                status == 400 || status == 422,
                "{expression}: {status} {response}"
            );
            ensure!(
                schedule(&w).await?["schedule"].is_null(),
                "{expression} saved"
            );
        }
        let before = wall_ms();
        let (status, saved) = w.tp.send(&path, Some(command.clone())).await?;
        ensure!(status == 201, "schedule: {status} {saved}");
        ensure!(
            saved["data"]["nextAt"].as_i64().unwrap_or(0) > before,
            "next: {saved}"
        );
        // A persisted cron that became overdue while canonical and home were offline.
        let due = wall_ms() / 300_000 * 300_000 - 900_000;
        w.tp.write()?.execute(
            "UPDATE schedules SET next_at=? WHERE id=?",
            minarbot_testkit::hosted::params![due, w.workflow],
        )?;
        w.tp.restart().await?;
        let current = wait_schedule(&w, |current| occurrences(current) == 1).await?;
        let pending = current["occurrences"][0].clone();
        ensure!(pending["state"] == "pending", "pending: {pending}");
        ensure!(pending["dueAt"] == due, "due: {pending}");
        ensure!(
            pending["missedCount"].as_i64().unwrap_or(0) >= 3,
            "missed: {pending}"
        );
        w.tp.restart().await?;
        ensure!(
            schedule(&w).await?["occurrences"][0]["occurrenceId"] == pending["occurrenceId"],
            "identity changed across restart"
        );
        let mut connection = w.connect()?;
        connected(&connection).await?;
        let current = wait_schedule(&w, |current| {
            current["occurrences"][0]["state"] == "admitted"
        })
        .await?;
        ensure!(current["enabled"] == true, "enabled: {current}");
        ensure!(
            current["nextAt"].as_i64().unwrap_or(0) > wall_ms(),
            "next: {current}"
        );
        ensure!(occurrences(&current) == 1, "occurrences: {current}");
        ensure!(
            current["occurrences"][0]["occurrenceId"] == pending["occurrenceId"],
            "identity changed"
        );
        run_done(&w, &current["occurrences"][0]["runId"]).await?;
        ensure!(
            w.tp.data(&format!("{}/runs", w.detail)).await?["items"]
                .as_array()
                .map(Vec::len)
                == Some(1),
            "runs"
        );
        ensure!(
            w.tp.send(&path, Some(command)).await?.1["receipt"] == saved["receipt"],
            "receipt replay"
        );
        ensure!(
            w.tp.send(
                &format!(
                    "/api/projects/{}/workflows/{}/schedule",
                    w.other, w.workflow
                ),
                None
            )
            .await?
            .0 == 404,
            "foreign project schedule"
        );
        connection.stop().await?;
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

fn schedule_events(data: &Value, prefix: &str) -> Vec<Value> {
    data["page"]["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|event| {
            event["recordKind"]
                .as_str()
                .is_some_and(|kind| kind.starts_with(prefix))
        })
        .cloned()
        .collect()
}

#[tokio::test]
async fn canonical_workflow_occurrence_attaches_its_first_recovery_task_atomically_on_the_original_home(
) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        let graph = json!({"entry": "model", "maxSteps": 2, "nodes": [
            {"id": "model", "kind": "model", "model": "fixture", "prompt": "Scheduled task", "next": "out"},
            {"id": "out", "kind": "output"}]});
        let (status, _) = w
            .tp
            .send(
                &w.list,
                Some(json!({"requestId": w.workflow, "id": w.workflow, "revision": 0,
                    "name": "Scheduled model", "graph": graph})),
            )
            .await?;
        ensure!(status == 201, "workflow: {status}");
        let mut connection = w.connect()?;
        connected(&connection).await?;
        let sync = format!("/api/sync?projectId={}", w.project);
        let before = w.tp.data(&sync).await?;
        let request = new_id();
        let command = json!({"requestId": request, "id": request, "revision": 0,
            "spec": {"kind": "instant", "at": wall_ms()}, "input": "Isolated input", "enabled": true});
        ensure!(
            w.tp.send(&format!("{}/schedule", w.detail), Some(command.clone())).await?.0 == 201,
            "schedule"
        );
        let (current, run) = minarbot_testkit::wait::until_within("run waiting", SETTLE, || async {
            let current = schedule(&w).await?;
            let Some(run) = current["occurrences"][0]["runId"].as_str() else {
                return Ok(None);
            };
            let run = w
                .tp
                .data(&format!("/api/projects/{}/workflow-runs/{run}", w.project))
                .await?;
            Ok((run["status"] == "waiting").then_some((current, run)))
        })
        .await?;
        let turn = run["steps"][0]["turn"].clone();
        ensure!(current["occurrences"][0]["taskId"] == turn, "occurrence task");
        let task = current["occurrences"][0]["taskId"].as_str().unwrap_or_default().to_owned();
        let (status, detail) = w.tp.send(&format!("/api/tasks/{task}"), None).await?;
        ensure!(status == 200, "task: {status} {detail}");
        ensure!(detail["data"]["homeDeviceId"] == w.home["id"], "task home");
        let changed = w
            .tp
            .data(&format!("{sync}&cursor={}", super::fixture::encode(before["nextCursor"].as_str().unwrap_or_default())))
            .await?;
        let events: Vec<Value> = schedule_events(&changed, "workflow_schedule_occurrence");
        ensure!(events.len() >= 2, "occurrence events: {events:?}");
        let last = events.last().cloned().unwrap_or(Value::Null);
        ensure!(last["body"]["taskId"] == turn, "last event task");
        ensure!(last["body"]["state"] == "admitted", "last event state");
        ensure!(
            last["revision"].as_i64() > events[0]["revision"].as_i64(),
            "event revisions"
        );
        ensure!(
            !json!(events).to_string().contains("Isolated input"),
            "sync leaked the input"
        );
        w.tp.restart().await?;
        ensure!(
            schedule(&w).await?["occurrences"][0]["taskId"] == turn,
            "task lost across restart"
        );
        connection.stop().await?;
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn workflow_schedule_sync_retains_scoped_metadata_and_exact_deltas_across_restart_without_replaying_commands(
) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        create_workflow(&w, "Schedule observations").await?;
        let sync = format!("/api/sync?projectId={}", w.project);
        let before = w.tp.data(&sync).await?;
        let cursor = |value: &Value| {
            format!("{sync}&cursor={}", super::fixture::encode(value["nextCursor"].as_str().unwrap_or_default()))
        };
        let request = new_id();
        let command = json!({"requestId": request, "id": request, "revision": 0,
            "spec": {"kind": "interval", "amount": 5, "unit": "minute", "anchorAt": wall_ms() - 960_000},
            "input": "PRIVATE_SCHEDULE_INPUT_CANARY", "enabled": true});
        let path = format!("{}/schedule", w.detail);
        let (status, accepted) = w.tp.send(&path, Some(command.clone())).await?;
        ensure!(status == 201, "schedule: {status} {accepted}");
        let current = wait_schedule(&w, |current| occurrences(current) == 1).await?;
        let changed = w.tp.data(&cursor(&before)).await?;
        let events = schedule_events(&changed, "workflow_schedule");
        let kinds: Vec<(Value, Value)> = events
            .iter()
            .map(|event| (event["recordKind"].clone(), event["revision"].clone()))
            .collect();
        ensure!(
            kinds == vec![(json!("workflow_schedule"), json!(1)), (json!("workflow_schedule_occurrence"), json!(1))],
            "events: {kinds:?}"
        );
        ensure!(events[0]["body"]["detailRef"] == path.as_str(), "detail ref");
        ensure!(events[0]["body"]["scheduleRevision"] == 1, "schedule revision");
        ensure!(events[1]["body"]["missedCount"] == 3, "missed count");
        ensure!(
            events[1]["recordId"].as_str()
                == Some(json!([w.workflow, current["occurrences"][0]["occurrenceId"]]).to_string().as_str()),
            "occurrence record id"
        );
        ensure!(
            !changed.to_string().contains("PRIVATE_SCHEDULE_INPUT_CANARY"),
            "sync leaked the input"
        );
        ensure!(events[0]["body"].get("spec").is_none(), "sync carries the spec");
        w.tp.restart().await?;
        ensure!(
            w.tp.send(&path, Some(command)).await?.1["receipt"] == accepted["receipt"],
            "receipt replay"
        );
        let retry = w.tp.data(&cursor(&before)).await?;
        ensure!(
            retry["page"]["events"] == changed["page"]["events"],
            "delta changed across restart"
        );
        ensure!(
            w.tp.data(&cursor(&changed)).await?["page"]["events"] == json!([]),
            "later delta not empty"
        );
        let snapshot = w.tp.data(&sync).await?;
        for event in &events {
            let found = snapshot["page"]["events"]
                .as_array()
                .and_then(|rows| rows.iter().find(|item| item["recordKind"] == event["recordKind"]))
                .map(|item| item["body"].clone());
            ensure!(found.as_ref() == Some(&event["body"]), "snapshot body differs");
        }
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn workflow_schedule_sync_rechecks_current_assignment_private_versions_and_root_visibility_for_every_human(
) -> Result<()> {
    let w = WorkflowHome::start().await?;
    let result = async {
        create_workflow(&w, "Private schedules").await?;
        let request = new_id();
        let command = json!({"requestId": request, "id": request, "revision": 0,
            "spec": {"kind": "instant", "at": wall_ms() - 1000}, "input": "PRIVATE_SCOPE_CANARY",
            "enabled": true});
        ensure!(
            w.tp.send(&format!("{}/schedule", w.detail), Some(command)).await?.0 == 201,
            "schedule"
        );
        wait_schedule(&w, |current| occurrences(current) == 1).await?;
        let account = super::fixture::account(&w.tp, "user_collaborator").await?;
        let sync = format!("/api/sync?projectId={}", w.project);
        let db = w.tp.write()?;
        db.execute("INSERT INTO project_members VALUES (?,?,'member',1,0)", [&w.project, &account])?;
        let read = |subject: &'static str| {
            let (tp, sync) = (&w.tp, sync.clone());
            async move { Ok::<Value, minarbot_testkit::Error>(tp.send_as(&sync, None, subject, None).await?.1["data"].clone()) }
        };
        let events = |data: &Value| schedule_events(data, "workflow_schedule");
        ensure!(events(&read("user_collaborator").await?).is_empty(), "unassigned member sees schedules");
        db.execute("INSERT INTO collaborator_agents VALUES (?,?,?,1,1)", [&w.project, &account, &w.agent])?;
        db.execute("INSERT INTO response_policies VALUES (?,?,?,0,1)", [&w.project, &account, &w.agent])?;
        ensure!(
            events(&read("user_collaborator").await?).len() == 2,
            "Response OFF preserves schedule visibility"
        );
        ensure!(
            events(&w.tp.data(&format!("/api/sync?projectId={}", w.other)).await?).is_empty(),
            "other project sees schedules"
        );
        for (table, key) in [("workflows", json!([w.workflow])), ("workflow_versions", json!([w.workflow, 1]))] {
            let before = read("user_collaborator").await?;
            let key = minarbot_testkit::protocol::canonical_json(&key);
            db.execute(
                "UPDATE record_scopes SET visibility='owner' WHERE table_name=? AND key_json=?",
                [table, key.as_str()],
            )?;
            let cursor = format!("{sync}&cursor={}", super::fixture::encode(before["nextCursor"].as_str().unwrap_or_default()));
            ensure!(
                w.tp.send_as(&cursor, None, "user_collaborator", None).await?.0 == 410,
                "{table}: stale cursor"
            );
            ensure!(events(&read("user_collaborator").await?).is_empty(), "{table}: private visible");
            ensure!(
                w.tp.send_as(&format!("{}/schedule", w.detail), None, "user_collaborator", None).await?.0 == 404,
                "{table}: schedule readable"
            );
            db.execute(
                "UPDATE record_scopes SET visibility='project' WHERE table_name=? AND key_json=?",
                [table, key.as_str()],
            )?;
        }
        let workflow_key = minarbot_testkit::protocol::canonical_json(&json!([w.workflow]));
        db.execute(
            "UPDATE record_scopes SET visibility='owner',account=? WHERE table_name='workflows' AND key_json=?",
            [account.as_str(), workflow_key.as_str()],
        )?;
        ensure!(events(&read("user_owner").await?).is_empty(), "owner sees another account's private root");
        ensure!(events(&read("user_collaborator").await?).len() == 2, "private root owner sees it");
        db.execute(
            "UPDATE record_scopes SET visibility='project' WHERE table_name='workflows' AND key_json=?",
            [workflow_key.as_str()],
        )?;
        let granted = read("user_collaborator").await?;
        db.execute(
            "UPDATE collaborator_agents SET enabled=0,revision=2 WHERE project=? AND account=? AND agent=?",
            [&w.project, &account, &w.agent],
        )?;
        let cursor = format!("{sync}&cursor={}", super::fixture::encode(granted["nextCursor"].as_str().unwrap_or_default()));
        ensure!(
            w.tp.send_as(&cursor, None, "user_collaborator", None).await?.0 == 410,
            "revoked assignment cursor"
        );
        ensure!(events(&read("user_collaborator").await?).is_empty(), "revoked assignment sees schedules");
        ensure!(
            w.tp.send_as(&sync, None, "user_stranger", None).await?.0 == 403,
            "stranger sync"
        );
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_workflow_schedule_rejects_foreign_scope_and_rechecks_current_assignment_before_receipt_replay(
) -> Result<()> {
    let w = WorkflowHome::start().await?;
    let result = async {
        create_workflow(&w, "Schedule authority").await?;
        let member = super::fixture::account(&w.tp, "user_member").await?;
        let db = w.tp.write()?;
        db.execute("INSERT INTO project_members VALUES (?,?,'member',1,0)", [&w.project, &member])?;
        db.execute("INSERT INTO collaborator_agents VALUES (?,?,?,1,1)", [&w.project, &member, &w.agent])?;
        let path = format!("{}/schedule", w.detail);
        let request = new_id();
        let command = json!({"requestId": request, "id": request, "revision": 0,
            "spec": {"kind": "instant", "at": wall_ms() + 600_000}, "input": "Scoped", "enabled": true});
        let (status, accepted) = w.tp.send_as(&path, Some(command.clone()), "user_member", None).await?;
        ensure!(status == 201, "member schedule: {status} {accepted}");
        let variant = |extra: Option<(&str, Value)>| {
            let mut body = command.clone();
            let id = new_id();
            body["requestId"] = json!(id);
            body["id"] = json!(id);
            if let Some((key, value)) = extra {
                body[key] = value;
            }
            body
        };
        ensure!(
            w.tp.send_as(&path, Some(variant(None)), "user_member", None).await?.0 == 409,
            "stale revision"
        );
        ensure!(
            w.tp.send_as(&path, Some(variant(Some(("projectId", json!(w.project))))), "user_member", None)
                .await?
                .0
                == 404,
            "extra field"
        );
        ensure!(w.tp.send(&format!("{path}?zone=UTC"), None).await?.0 == 404, "query parameter");
        ensure!(
            w.tp.send(&format!("/api/projects/{}/workflows/{}/schedule", w.other, w.workflow), None)
                .await?
                .0
                == 404,
            "foreign project"
        );
        db.execute(
            "UPDATE collaborator_agents SET enabled=0 WHERE project=? AND account=?",
            [&w.project, &member],
        )?;
        ensure!(
            w.tp.send_as(&path, Some(command.clone()), "user_member", None).await?.0 == 404,
            "receipt replay after assignment removal"
        );
        ensure!(
            w.tp.send_as(&path, None, "user_member", None).await?.0 == 404,
            "read after assignment removal"
        );
        db.execute(
            "UPDATE collaborator_agents SET enabled=1 WHERE project=? AND account=?",
            [&w.project, &member],
        )?;
        db.execute(
            "UPDATE record_scopes SET visibility='owner' WHERE root_kind='workflow' AND root_id=?",
            [&w.workflow],
        )?;
        ensure!(
            w.tp.send_as(&path, Some(command), "user_member", None).await?.0 == 404,
            "receipt replay of a private root"
        );
        ensure!(w.tp.send(&path, None).await?.0 == 200, "owner read");
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}
