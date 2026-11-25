//! Canonical routines: manual runs, configuration, catch-up and flows.

use super::{
    fixture::{account, agent_body, encode, until, SETTLE},
    workflow_waits::{connected, WorkflowHome},
    workflows::{check, Checks},
};
use minarbot_testkit::{http::Method, prelude::*};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

pub fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

pub fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub fn routines_of(w: &WorkflowHome) -> String {
    format!("/api/projects/{}/agents/{}/routines", w.project, w.agent)
}

pub fn routine_path(w: &WorkflowHome, routine: &str) -> String {
    format!("/api/projects/{}/routines/{routine}", w.project)
}

pub fn routine_command(routine: &str, name: &str, prompt: &str, next_at: i64, interval: Option<i64>, enabled: bool) -> Value {
    json!({"requestId": routine, "id": routine, "revision": 0, "name": name, "prompt": prompt,
        "nextAt": next_at, "intervalSeconds": interval, "enabled": enabled})
}

/// Bind `agent` to a ready provider binding on `device`.
pub fn bind(w: &WorkflowHome, device: &str, agent: &str, owner: &str, model: &str) -> Result<()> {
    let binding = new_id();
    let db = w.tp.write()?;
    db.execute(
        "INSERT INTO provider_bindings VALUES (?,?,?,'openai-compatible',?,'Fixture','ready',1)",
        [&binding, device, owner, model],
    )?;
    db.execute(
        "UPDATE agent_authority SET provider_binding=?,revision=revision+1 WHERE agent=?",
        [&binding, agent],
    )?;
    Ok(())
}

pub async fn sync_after(w: &WorkflowHome, cursor: &Value) -> Result<Value> {
    w.tp.data(&format!("/api/sync?projectId={}&cursor={}", w.project, encode(&text(cursor))))
        .await
}

pub async fn first_run(w: &WorkflowHome, detail: &str) -> Result<Value> {
    Ok(w.tp.data(&format!("{detail}/runs")).await?["items"][0].clone())
}

pub async fn wait_first_run(
    w: &WorkflowHome,
    detail: &str,
    done: impl Fn(&Value) -> bool,
) -> Result<Value> {
    minarbot_testkit::wait::until_within(format!("routine run {detail}"), SETTLE, || async {
        let run = first_run(w, detail).await?;
        Ok(done(&run).then_some(run))
    })
    .await
}

fn events<'a>(page: &'a Value, kind: impl Fn(&str) -> bool + 'a) -> impl Iterator<Item = &'a Value> + 'a {
    page["page"]["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(move |event| kind(event["recordKind"].as_str().unwrap_or_default()))
}

/// A raw request with the protocol headers and an extra header set.
async fn raw(
    w: &WorkflowHome,
    method: Method,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<String>,
) -> Result<(u16, String)> {
    let mut request = w
        .tp
        .app
        .server
        .client()?
        .request(method, format!("{}{path}", w.tp.origin()))
        .header("Authorization", format!("Bearer {}", w.tp.jwt("user_owner")?))
        .header("Origin", w.tp.origin())
        .header("Content-Type", "application/json")
        .header("X-Minarbot-Protocol", "1");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    if let Some(body) = body {
        request = request.body(body);
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    Ok((status, response.text().await?))
}

#[tokio::test]
async fn canonical_routine_manual_run_waits_for_its_home_and_recovers_lost_replies_and_restart_without_moving_schedule(
) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        let routine = new_id();
        let detail = routine_path(&w, &routine);
        let other = w.tp.device("manual-other-home.sqlite")?;
        other.enroll(&w.tp.jwt("user_owner")?, "new").await?;
        let other_connection = other.connect().await?;
        until("other home", SETTLE, || async { Ok(other_connection.phase() == "connected") }).await?;
        let command = routine_command(&routine, "Manual", "PRIVATE_MANUAL_PROMPT", wall_ms() + 600_000, Some(300), false);
        let request = json!({"requestId": new_id()});
        let owner = account(&w.tp, "user_owner").await?;
        let home = text(&w.home["id"]);
        bind(&w, &home, &w.agent, &owner, "manual-fixture")?;
        ensure!(w.tp.send(&routines_of(&w), Some(command.clone())).await?.0 == 201, "routine");
        let before = w.tp.data(&detail).await?;
        let cursor = w.tp.data(&format!("/api/sync?projectId={}", w.project)).await?["nextCursor"].clone();
        let run_path = format!("{detail}/run");
        // The first reply is lost after the server accepted it.
        let (status, lost) = w.tp.send(&run_path, Some(request.clone())).await?;
        ensure!(status == 202, "lost admission: {status} {lost}");
        let (status, accepted) = w.tp.send(&run_path, Some(request.clone())).await?;
        ensure!(status == 200, "retry: {status} {accepted}");
        let run = first_run(&w, &detail).await?;
        ensure!(run["source"] == "manual", "source");
        ensure!(run["id"] == accepted["data"]["occurrenceId"], "occurrence id");
        ensure!(run["taskId"] == accepted["data"]["taskId"], "task id");
        ensure!(run["state"] == "admitted", "state");
        let task = text(&run["taskId"]);
        ensure!(w.tp.task_detail(&task).await?["state"] == "waiting_device", "task waits for home");
        ensure!(accepted["data"]["homeDeviceId"] == home.as_str(), "home");
        ensure!(
            w.tp.data("/api/devices").await?.as_array().is_some_and(|devices| devices
                .iter()
                .any(|device| device["id"] != home.as_str() && device["status"] == "ready")),
            "Another ready device never takes the offline home task"
        );
        ensure!(
            w.tp.data(&detail).await? == before,
            "Manual admission never changes configuration or catch-up"
        );
        let (status, overlap) = w.tp.send(&run_path, Some(json!({"requestId": new_id()}))).await?;
        ensure!(status == 409, "overlap: {status}");
        let message = text(&overlap["error"]["message"]);
        ensure!(
            message.contains("active run")
                && message[message.find("active run").unwrap_or(0)..].contains("Complete or cancel"),
            "overlap message: {message}"
        );
        let observations = sync_after(&w, &cursor).await?;
        let sources: Vec<Value> = events(&observations, |kind| kind == "routine_occurrence")
            .map(|event| event["body"]["source"].clone())
            .collect();
        ensure!(sources == vec![json!("manual")], "sources: {sources:?}");
        ensure!(!observations.to_string().contains("PRIVATE_"), "private text synced");
        w.tp.restart().await?;
        ensure!(w.tp.send(&run_path, Some(request.clone())).await?.1 == accepted, "replay after restart");
        let connection = w.host.connect().await?;
        until("home", SETTLE, || async { Ok(connection.phase() == "connected") }).await?;
        let calls = Arc::new(AtomicUsize::new(0));
        let checks = Checks::new();
        let (counter, failures, prompt) = (calls.clone(), checks.handle(), command["prompt"].clone());
        let ran = w
            .host
            .run_task(&task, move |context| {
                counter.fetch_add(1, Ordering::SeqCst);
                check(
                    &failures,
                    context["snapshot"]["conversation"]["message"]["text"] == prompt,
                    "routine prompt",
                );
                async { Ok(json!({"text": "PRIVATE_MANUAL_RESULT"})) }
            })
            .await?;
        ensure!(ran["state"] == "completed", "run task: {ran}");
        checks.result()?;
        wait_first_run(&w, &detail, |run| run["state"] == "done").await?;
        ensure!(calls.load(Ordering::SeqCst) == 1, "provider calls");
        ensure!(w.tp.send(&run_path, Some(request)).await?.1 == accepted, "replay after done");
        let snapshot = w.tp.data(&format!("/api/sync?projectId={}", w.project)).await?;
        ensure!(
            events(&snapshot, |kind| kind == "routine_occurrence")
                .next()
                .is_some_and(|event| event["body"]["source"] == "manual"),
            "snapshot source"
        );
        ensure!(!snapshot.to_string().contains("PRIVATE_"), "private text in snapshot");
        ensure!(w.tp.data(&detail).await?["next_at"] == command["nextAt"], "schedule moved");
        ensure!(w.tp.inspect()?.count("routine_occurrences")? == 1, "occurrences");
        connection.stop().await?;
        other_connection.stop().await?;
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_routine_manual_admission_enforces_current_authority_private_scopes_closed_requests_and_receipt_conflicts(
) -> Result<()> {
    let w = WorkflowHome::start().await?;
    let result = async {
        let (routine, other_routine) = (new_id(), new_id());
        let (detail, other_detail) = (routine_path(&w, &routine), routine_path(&w, &other_routine));
        for id in [&routine, &other_routine] {
            let command = routine_command(id, "Manual", "Private work", wall_ms() + 600_000, None, true);
            ensure!(w.tp.send(&routines_of(&w), Some(command)).await?.0 == 201, "routine");
        }
        let owner = account(&w.tp, "user_owner").await?;
        let member = account(&w.tp, "user_collaborator").await?;
        let db = w.tp.write()?;
        db.execute("INSERT INTO project_members VALUES (?,?,'member',1,0)", [&w.project, &member])?;
        db.execute("INSERT INTO collaborator_agents VALUES (?,?,?,1,1)", [&w.project, &member, &w.agent])?;
        db.execute("INSERT INTO response_policies VALUES (?,?,?,0,1)", [&w.project, &member, &w.agent])?;
        let as_member = |path: String, body: Value| {
            let tp = &w.tp;
            async move { tp.send_as(&path, Some(body), "user_collaborator", None).await }
        };
        let run = format!("{detail}/run");
        let other_run = format!("{other_detail}/run");
        let request = json!({"requestId": new_id()});
        let (status, accepted) = as_member(run.clone(), request.clone()).await?;
        ensure!(status == 202, "admission: {status} {accepted}");
        let (status, conflict) = as_member(other_run.clone(), request.clone()).await?;
        ensure!(status == 409, "conflict: {status}");
        ensure!(conflict["error"]["code"] == "payload_conflict", "conflict code");
        db.execute(
            "UPDATE collaborator_agents SET enabled=0,revision=revision+1 WHERE project=? AND account=?",
            [&w.project, &member],
        )?;
        for payload in [request.clone(), json!({"requestId": new_id()})] {
            ensure!(as_member(run.clone(), payload).await?.0 == 404, "revoked assignment");
        }
        db.execute(
            "UPDATE collaborator_agents SET enabled=1,revision=revision+1 WHERE project=? AND account=?",
            [&w.project, &member],
        )?;
        db.execute(
            "UPDATE project_members SET deleted=1,grant_revision=grant_revision+1 WHERE project=? AND account=?",
            [&w.project, &member],
        )?;
        ensure!(as_member(run.clone(), request.clone()).await?.0 == 404, "removed member");
        db.execute(
            "UPDATE project_members SET deleted=0,grant_revision=grant_revision+1 WHERE project=? AND account=?",
            [&w.project, &member],
        )?;
        db.execute(
            "UPDATE record_scopes SET account=?,visibility='owner' WHERE table_name='routines' AND key_json=?",
            [&member, &json!([routine]).to_string()],
        )?;
        ensure!(
            w.tp.send(&run, Some(json!({"requestId": new_id()}))).await?.0 == 404,
            "Project owner cannot run another human private routine"
        );
        ensure!(as_member(run.clone(), request.clone()).await?.0 == 200, "private replay");
        db.execute(
            "UPDATE record_scopes SET account=?,visibility='owner' WHERE table_name='routine_occurrences'",
            [&owner],
        )?;
        ensure!(
            as_member(run.clone(), request.clone()).await?.0 == 404,
            "Retry must still see its occurrence"
        );
        for path in [
            format!("/api/projects/{}/routines/{routine}/run", w.other),
            format!("/api/routines/{routine}/run"),
            format!("{other_run}?projectId={}", w.project),
            format!("{other_run}?after=1"),
        ] {
            ensure!(
                w.tp.send(&path, Some(json!({"requestId": new_id()}))).await?.0 == 404,
                "{path}"
            );
        }
        ensure!(
            w.tp.send(&other_run, Some(json!({"requestId": new_id(), "projectId": w.project})))
                .await?
                .0
                == 404,
            "projectId body"
        );
        ensure!(w.tp.send(&other_run, Some(json!({"requestId": "bad"}))).await?.0 == 400, "bad id");
        ensure!(
            w.tp.send(&other_run, Some(json!({"requestId": new_id(), "prompt": "Extra"})))
                .await?
                .0
                == 400,
            "extra field"
        );
        ensure!(w.tp.send(&other_run, None).await?.0 == 404, "GET run");
        let selector = new_id();
        let (status, _) = raw(
            &w,
            Method::POST,
            &other_run,
            &[("X-Minarbot-Request-Id", &selector), ("X-Minarbot-Project", &w.other)],
            Some(json!({"requestId": selector}).to_string()),
        )
        .await?;
        ensure!(status == 404, "project selector header: {status}");
        ensure!(w.tp.inspect()?.count("routine_occurrences")? == 1, "occurrences");
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_routine_configuration_and_flow_stay_scoped_across_projects_restart_and_lost_replies(
) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        let list = routines_of(&w);
        let routine = new_id();
        let detail = routine_path(&w, &routine);
        let command = routine_command(&routine, "Routine", "Private prompt\nCheck work", wall_ms() + 600_000, Some(300), true);
        let (status, created) = w.tp.send(&list, Some(command.clone())).await?;
        ensure!(status == 201, "create: {status} {created}");
        ensure!(created["data"]["homeDeviceId"] == w.home["id"], "home");
        ensure!(created["data"]["projectId"] == w.project.as_str(), "project");
        ensure!(w.tp.data(&detail).await?["prompt"] == command["prompt"], "prompt");
        let (status, calendar) = raw(
            &w,
            Method::GET,
            &format!("{list}/calendar.ics"),
            &[("Connection", "close"), ("X-Minarbot-Request-Id", &new_id())],
            None,
        )
        .await?;
        ensure!(status == 200, "calendar: {status}");
        ensure!(calendar.contains(&format!("UID:{routine}@minarbot.local")), "calendar uid");
        ensure!(calendar.contains("RRULE:FREQ=SECONDLY;INTERVAL=300"), "calendar rule");
        ensure!(!calendar.contains(command["prompt"].as_str().unwrap_or_default()), "calendar prompt");
        ensure!(
            w.tp.send(&format!("/api/projects/{}/agents/{}/routines/calendar.ics", w.other, w.agent), None)
                .await?
                .0
                == 404,
            "foreign calendar"
        );
        ensure!(w.tp.send(&list, Some(command.clone())).await?.1["receipt"] == created["receipt"], "create replay");
        let mut renamed = command.clone();
        renamed["name"] = json!("Other");
        ensure!(w.tp.send(&list, Some(renamed)).await?.0 == 409, "changed create");
        let (status, _) = w
            .tp
            .send_as(
                &format!("/api/projects/{}/agents/{}", w.other, w.agent),
                Some(json!({"requestId": new_id(), "expectedProjectRevision": 1,
                    "expectedAgentRevision": 1, "expectedAuthorityRevision": 1,
                    "expectedParticipationRevision": 0, "rules": "Separate",
                    "capabilities": {"v": 1, "tools": [], "resources": []}})),
                "user_owner",
                Some(Method::PUT),
            )
            .await?;
        ensure!(status == 201, "participation: {status}");
        ensure!(
            w.tp.data(&format!("/api/projects/{}/agents/{}/routines", w.other, w.agent)).await?["items"]
                == json!([]),
            "other project list"
        );
        ensure!(
            w.tp.send(&format!("/api/projects/{}/routines/{routine}", w.other), None).await?.0 == 404,
            "other project read"
        );
        let next = new_id();
        let mut edited = command.clone();
        edited["requestId"] = json!(next);
        edited["id"] = json!(next);
        edited["revision"] = json!(1);
        edited["name"] = json!("Edited");
        let (status, saved) = w.tp.send(&format!("{detail}/edit"), Some(edited.clone())).await?;
        ensure!(status == 200, "edit: {status} {saved}");
        ensure!(
            w.tp.send(&list, Some(command.clone())).await?.1["data"]["name"] == "Routine",
            "Creation receipt never overwrites newer configuration"
        );
        ensure!(w.tp.data(&detail).await?["name"] == "Edited", "edited name");
        let flow_id = new_id();
        let flow = json!({"requestId": flow_id, "id": flow_id, "revision": 0, "links": []});
        let (status, flow_saved) = w.tp.send(&format!("{detail}/flow"), Some(flow.clone())).await?;
        ensure!(status == 200, "flow: {status} {flow_saved}");
        ensure!(
            flow_saved["data"]["graph"] == json!({"root": w.agent, "nodes": [w.agent], "links": []}),
            "flow graph: {flow_saved}"
        );
        w.tp.restart().await?;
        ensure!(
            w.tp.send(&format!("{detail}/edit"), Some(edited.clone())).await?.1["receipt"] == saved["receipt"],
            "edit replay"
        );
        ensure!(
            w.tp.send(&format!("{detail}/flow"), Some(flow)).await?.1["receipt"] == flow_saved["receipt"],
            "flow replay"
        );
        ensure!(w.tp.data(&format!("{detail}/flow")).await?["revision"] == 1, "flow revision");
        let mut stale = edited.clone();
        stale["requestId"] = json!(new_id());
        stale["id"] = json!(new_id());
        ensure!(w.tp.send(&format!("{detail}/edit"), Some(stale)).await?.0 == 400, "mismatched ids");
        for path in [
            format!("/api/routines/{routine}"),
            format!("/api/agents/{}/routines?after={routine}", w.agent),
            format!("{list}?projectId={}", w.project),
            format!("{list}?after=x"),
            format!("{list}?after=&after="),
            format!("{detail}?after={routine}"),
        ] {
            ensure!(w.tp.send(&path, None).await?.0 == 404, "{path}");
        }
        let mut selector = edited.clone();
        selector["projectId"] = json!(w.project);
        ensure!(w.tp.send(&format!("{detail}/edit"), Some(selector)).await?.0 == 404, "projectId body");
        let db = w.tp.inspect()?;
        ensure!(
            db.rows("SELECT 1 FROM sqlite_schema WHERE name='routine_runs'", [])?.is_empty(),
            "Configuration never starts legacy dispatch"
        );
        ensure!(db.rows("PRAGMA foreign_key_check", [])?.is_empty(), "foreign keys");
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

fn occurrence_id(routine: &str, due: i64) -> String {
    use minarbot_testkit::base64::Engine;
    let tuple = json!([routine, 1, 1, due, 0]).to_string();
    let digest = ring::digest::digest(&ring::digest::SHA256, tuple.as_bytes());
    minarbot_testkit::base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest.as_ref())
}

fn routine_revisions(page: &Value) -> Vec<(Value, Value)> {
    events(page, |kind| kind.starts_with("routine"))
        .map(|event| (event["recordKind"].clone(), event["revision"].clone()))
        .collect()
}

#[tokio::test]
async fn canonical_routine_runtime_coalesces_offline_intervals_once_and_retains_task_receipts_across_restart(
) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        let routine = new_id();
        let (list, detail) = (routines_of(&w), routine_path(&w, &routine));
        let due = wall_ms() - 960_000;
        let command = routine_command(&routine, "Catch-up", "Scheduled isolated input", due, Some(300), true);
        let before = w.tp.data(&format!("/api/sync?projectId={}", w.project)).await?;
        let (status, saved) = w.tp.send(&list, Some(command.clone())).await?;
        ensure!(status == 201, "routine: {status} {saved}");
        let runs_path = format!("{detail}/runs");
        let runs = minarbot_testkit::wait::until_within("pending run", SETTLE, || async {
            let runs = w.tp.data(&runs_path).await?;
            Ok((runs["items"].as_array().map(Vec::len) == Some(1)).then_some(runs))
        })
        .await?;
        let pending = runs["items"][0].clone();
        ensure!(pending["state"] == "pending", "state");
        ensure!(pending["dueAt"] == due, "due");
        ensure!(pending["missedCount"] == 3, "missed");
        ensure!(pending["taskId"].is_null(), "task");
        let observations = sync_after(&w, &before["nextCursor"]).await?;
        ensure!(
            routine_revisions(&observations)
                == vec![(json!("routine"), json!(1)), (json!("routine_occurrence"), json!(1))],
            "observations: {:?}",
            routine_revisions(&observations)
        );
        ensure!(!observations.to_string().contains("Scheduled isolated input"), "prompt synced");
        ensure!(
            pending["id"] == occurrence_id(&routine, due).as_str(),
            "Elapsed occurrence identity follows approved schedule tuple"
        );
        w.tp.restart().await?;
        ensure!(w.tp.data(&runs_path).await? == runs, "runs changed across restart");
        let connection = w.connect()?;
        connected(&connection).await?;
        let runs = minarbot_testkit::wait::until_within("admitted run", SETTLE, || async {
            let runs = w.tp.data(&runs_path).await?;
            Ok((runs["items"][0]["state"] == "admitted").then_some(runs))
        })
        .await?;
        ensure!(runs["items"].as_array().map(Vec::len) == Some(1), "one run");
        ensure!(runs["items"][0]["id"] == pending["id"], "same occurrence");
        let task_id = text(&runs["items"][0]["taskId"]);
        let (_, task) = w.tp.send(&format!("/api/tasks/{task_id}"), None).await?;
        let admitted = sync_after(&w, &observations["nextCursor"]).await?;
        ensure!(
            routine_revisions(&admitted)
                == vec![(json!("routine"), json!(2)), (json!("routine_occurrence"), json!(2))],
            "admitted observations: {:?}",
            routine_revisions(&admitted)
        );
        ensure!(task["data"]["homeDeviceId"] == w.home["id"], "task home");
        ensure!(task["data"]["agentId"] == w.agent.as_str(), "task agent");
        ensure!(task["receipt"]["requestId"] == task_id.as_str(), "task receipt");
        let current = w.tp.data(&detail).await?;
        ensure!(current["next_at"].as_i64().unwrap_or(0) > wall_ms(), "next_at advanced");
        ensure!(current["revision"] == 2, "revision");
        ensure!(w.tp.send(&list, Some(command.clone())).await?.1["receipt"] == saved["receipt"], "create replay");
        w.tp.restart().await?;
        ensure!(w.tp.data(&runs_path).await?["items"][0]["taskId"] == task_id.as_str(), "task after restart");
        let edit = |revision: i64| {
            let mut edit = command.clone();
            let id = new_id();
            edit["requestId"] = json!(id);
            edit["id"] = json!(id);
            edit["revision"] = json!(revision);
            edit
        };
        ensure!(w.tp.send(&format!("{detail}/edit"), Some(edit(1))).await?.0 == 409, "stale edit");
        ensure!(
            w.tp.send(&format!("{detail}/edit"), Some(edit(2))).await?.1["error"]["code"] == "schedule_processed",
            "processed schedule"
        );
        ensure!(
            w.tp.send(&format!("/api/projects/{}/routines/{routine}/runs", w.other), None).await?.0 == 404,
            "foreign runs"
        );
        for suffix in ["?before=0".to_owned(), "?before=1&before=2".to_owned(), format!("?projectId={}", w.project)] {
            ensure!(w.tp.send(&format!("{runs_path}{suffix}"), None).await?.0 == 404, "runs{suffix}");
        }
        ensure!(w.tp.send(&runs_path, Some(json!({"requestId": new_id()}))).await?.0 == 404, "POST runs");
        let db = w.tp.inspect()?;
        ensure!(db.rows("SELECT 1 FROM sqlite_schema WHERE name='routine_runs'", [])?.is_empty(), "legacy table");
        ensure!(db.scalar::<i64>("SELECT count(*) FROM tasks WHERE id=?", [&task_id])? == 1, "task row");
        ensure!(db.rows("PRAGMA foreign_key_check", [])?.is_empty(), "foreign keys");
        drop(connection);
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_routine_runtime_snapshots_flows_and_hands_off_once_using_current_home_authority(
) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        let routine = new_id();
        let detail = routine_path(&w, &routine);
        let second_runtime = w.tp.device("routine-second-home.sqlite")?;
        second_runtime.enroll(&w.tp.jwt("user_owner")?, "new").await?;
        let connection = w.host.connect().await?;
        until("home", SETTLE, || async { Ok(connection.phase() == "connected") }).await?;
        let homes = w.tp.data("/api/devices").await?;
        let home = text(&w.home["id"]);
        let second_home = homes
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["id"] != home.as_str()).cloned())
            .ok_or("second home missing")?;
        let second = new_id();
        let owner = account(&w.tp, "user_owner").await?;
        let (status, body) = w
            .tp
            .send("/api/agents", Some(agent_body(&second, "Next", "Help", &w.project, &second_home)))
            .await?;
        ensure!(status == 201, "second agent: {status} {body}");
        bind(&w, &home, &w.agent, &owner, "routine-fixture")?;
        bind(&w, &text(&second_home["id"]), &second, &owner, "routine-fixture")?;
        let create = routine_command(&routine, "Flow run", "Root work", wall_ms() + 600_000, None, true);
        ensure!(w.tp.send(&routines_of(&w), Some(create.clone())).await?.0 == 201, "routine");
        let flow = new_id();
        ensure!(
            w.tp.send(
                &format!("{detail}/flow"),
                Some(json!({"requestId": flow, "id": flow, "revision": 0,
                    "links": [{"from": w.agent, "to": second}]}))
            )
            .await?
            .0 == 200,
            "flow"
        );
        let edit = new_id();
        let mut edited = create.clone();
        edited["requestId"] = json!(edit);
        edited["id"] = json!(edit);
        edited["revision"] = json!(1);
        edited["nextAt"] = json!(wall_ms() - 1000);
        ensure!(w.tp.send(&format!("{detail}/edit"), Some(edited)).await?.0 == 200, "edit");
        let run = wait_first_run(&w, &detail, |run| run["state"] == "admitted").await?;
        ensure!(run["steps"].as_array().map(Vec::len) == Some(2), "steps: {run}");
        ensure!(w.tp.data(&detail).await?["enabled"] == 0, "one-shot disabled");
        let root = text(&run["taskId"]);
        let change = new_id();
        ensure!(
            w.tp.send(
                &format!("{detail}/flow"),
                Some(json!({"requestId": change, "id": change, "revision": 1, "links": []}))
            )
            .await?
            .0 == 200,
            "flow change"
        );
        let checks = Checks::new();
        let failures = checks.handle();
        let ran = w
            .host
            .run_task(&root, move |context| {
                let conversation = &context["snapshot"]["conversation"];
                check(&failures, conversation["history"].as_array().map(Vec::len) == Some(0), "root history");
                check(&failures, conversation["message"]["text"] == "Root work", "root message");
                async { Ok(json!({"text": "Original root result"})) }
            })
            .await?;
        ensure!(ran["state"] == "completed", "root: {ran}");
        let step_task = |run: &Value| {
            run["steps"].as_array().and_then(|steps| {
                steps
                    .iter()
                    .find(|step| step["agentId"] == second.as_str())
                    .and_then(|step| step["taskId"].as_str().map(str::to_owned))
            })
        };
        let run = wait_first_run(&w, &detail, |run| step_task(run).is_some()).await?;
        let next = step_task(&run).ok_or("next task")?;
        let next_task = w.tp.task_detail(&next).await?;
        ensure!(next_task["homeDeviceId"] == second_home["id"], "next home");
        ensure!(next_task["state"] == "waiting_device", "next state");
        let generation = connection.generation();
        w.tp.restart().await?;
        until("home reconnected", SETTLE, || async {
            Ok(connection.phase() == "connected" && connection.generation() > generation)
        })
        .await?;
        ensure!(step_task(&first_run(&w, &detail).await?) == Some(next.clone()), "next task after restart");
        let second_connection = second_runtime.connect().await?;
        until("second home", SETTLE, || async { Ok(second_connection.phase() == "connected") }).await?;
        let failures = checks.handle();
        let ran = second_runtime
            .run_task(&next, move |context| {
                let conversation = &context["snapshot"]["conversation"];
                check(&failures, conversation["history"].as_array().map(Vec::len) == Some(0), "next history");
                check(
                    &failures,
                    conversation["message"]["text"].as_str().is_some_and(|text| text.contains("Original root result")),
                    "next message",
                );
                async { Ok(json!({"text": "Next result"})) }
            })
            .await?;
        ensure!(ran["state"] == "completed", "next: {ran}");
        checks.result()?;
        let run = wait_first_run(&w, &detail, |run| run["state"] == "done").await?;
        let states: Vec<Value> = run["steps"].as_array().into_iter().flatten().map(|step| step["state"].clone()).collect();
        ensure!(states == vec![json!("done"), json!("done")], "step states: {states:?}");
        ensure!(
            w.tp.data(&format!("{detail}/runs")).await?["items"].as_array().map(Vec::len) == Some(1),
            "one run"
        );
        ensure!(
            w.tp.inspect()?.scalar::<i64>("SELECT count(*) FROM routine_steps WHERE occurrence=?", [&text(&run["id"])])? == 2,
            "routine steps"
        );
        connection.stop().await?;
        second_connection.stop().await?;
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}
