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

pub fn routine_command(
    routine: &str,
    name: &str,
    prompt: &str,
    next_at: i64,
    interval: Option<i64>,
    enabled: bool,
) -> Value {
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
    w.tp.data(&format!(
        "/api/sync?projectId={}&cursor={}",
        w.project,
        encode(&text(cursor))
    ))
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

pub fn events<'a>(
    page: &'a Value,
    kind: impl Fn(&str) -> bool + 'a,
) -> impl Iterator<Item = &'a Value> + 'a {
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
    let (status, _, text) = raw_as(&w.tp, "user_owner", method, path, headers, body).await?;
    Ok((status, text))
}

/// `raw` as `subject`, with the response headers.
pub async fn raw_as(
    tp: &super::fixture::Tp,
    subject: &str,
    method: Method,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<String>,
) -> Result<(u16, std::collections::HashMap<String, String>, String)> {
    let mut request = tp
        .app
        .server
        .client()?
        .request(method, format!("{}{path}", tp.origin()))
        .header("Authorization", format!("Bearer {}", tp.jwt(subject)?))
        .header("Origin", tp.origin())
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
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                value.to_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    Ok((status, headers, response.text().await?))
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
        until("other home", SETTLE, || async {
            Ok(other_connection.phase() == "connected")
        })
        .await?;
        let command = routine_command(
            &routine,
            "Manual",
            "PRIVATE_MANUAL_PROMPT",
            wall_ms() + 600_000,
            Some(300),
            false,
        );
        let request = json!({"requestId": new_id()});
        let owner = account(&w.tp, "user_owner").await?;
        let home = text(&w.home["id"]);
        bind(&w, &home, &w.agent, &owner, "manual-fixture")?;
        ensure!(
            w.tp.send(&routines_of(&w), Some(command.clone())).await?.0 == 201,
            "routine"
        );
        let before = w.tp.data(&detail).await?;
        let cursor =
            w.tp.data(&format!("/api/sync?projectId={}", w.project))
                .await?["nextCursor"]
                .clone();
        let run_path = format!("{detail}/run");
        // The first reply is lost after the server accepted it.
        let (status, lost) = w.tp.send(&run_path, Some(request.clone())).await?;
        ensure!(status == 202, "lost admission: {status} {lost}");
        let (status, accepted) = w.tp.send(&run_path, Some(request.clone())).await?;
        ensure!(status == 200, "retry: {status} {accepted}");
        let run = first_run(&w, &detail).await?;
        ensure!(run["source"] == "manual", "source");
        ensure!(
            run["id"] == accepted["data"]["occurrenceId"],
            "occurrence id"
        );
        ensure!(run["taskId"] == accepted["data"]["taskId"], "task id");
        ensure!(run["state"] == "admitted", "state");
        let task = text(&run["taskId"]);
        ensure!(
            w.tp.task_detail(&task).await?["state"] == "waiting_device",
            "task waits for home"
        );
        ensure!(accepted["data"]["homeDeviceId"] == home.as_str(), "home");
        ensure!(
            w.tp.data("/api/devices")
                .await?
                .as_array()
                .is_some_and(|devices| devices
                    .iter()
                    .any(|device| device["id"] != home.as_str() && device["status"] == "ready")),
            "Another ready device never takes the offline home task"
        );
        ensure!(
            w.tp.data(&detail).await? == before,
            "Manual admission never changes configuration or catch-up"
        );
        let (status, overlap) =
            w.tp.send(&run_path, Some(json!({"requestId": new_id()})))
                .await?;
        ensure!(status == 409, "overlap: {status}");
        let message = text(&overlap["error"]["message"]);
        ensure!(
            message.contains("active run")
                && message[message.find("active run").unwrap_or(0)..]
                    .contains("Complete or cancel"),
            "overlap message: {message}"
        );
        let observations = sync_after(&w, &cursor).await?;
        let sources: Vec<Value> = events(&observations, |kind| kind == "routine_occurrence")
            .map(|event| event["body"]["source"].clone())
            .collect();
        ensure!(sources == vec![json!("manual")], "sources: {sources:?}");
        ensure!(
            !observations.to_string().contains("PRIVATE_"),
            "private text synced"
        );
        w.tp.restart().await?;
        ensure!(
            w.tp.send(&run_path, Some(request.clone())).await?.1 == accepted,
            "replay after restart"
        );
        let connection = w.host.connect().await?;
        until("home", SETTLE, || async {
            Ok(connection.phase() == "connected")
        })
        .await?;
        let calls = Arc::new(AtomicUsize::new(0));
        let checks = Checks::new();
        let (counter, failures, prompt) =
            (calls.clone(), checks.handle(), command["prompt"].clone());
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
        ensure!(
            w.tp.send(&run_path, Some(request)).await?.1 == accepted,
            "replay after done"
        );
        let snapshot =
            w.tp.data(&format!("/api/sync?projectId={}", w.project))
                .await?;
        ensure!(
            events(&snapshot, |kind| kind == "routine_occurrence")
                .next()
                .is_some_and(|event| event["body"]["source"] == "manual"),
            "snapshot source"
        );
        ensure!(
            !snapshot.to_string().contains("PRIVATE_"),
            "private text in snapshot"
        );
        ensure!(
            w.tp.data(&detail).await?["next_at"] == command["nextAt"],
            "schedule moved"
        );
        ensure!(
            w.tp.inspect()?.count("routine_occurrences")? == 1,
            "occurrences"
        );
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
        let command = routine_command(
            &routine,
            "Routine",
            "Private prompt\nCheck work",
            wall_ms() + 600_000,
            Some(300),
            true,
        );
        let (status, created) = w.tp.send(&list, Some(command.clone())).await?;
        ensure!(status == 201, "create: {status} {created}");
        ensure!(created["data"]["homeDeviceId"] == w.home["id"], "home");
        ensure!(
            created["data"]["projectId"] == w.project.as_str(),
            "project"
        );
        ensure!(
            w.tp.data(&detail).await?["prompt"] == command["prompt"],
            "prompt"
        );
        let (status, calendar) = raw(
            &w,
            Method::GET,
            &format!("{list}/calendar.ics"),
            &[
                ("Connection", "close"),
                ("X-Minarbot-Request-Id", &new_id()),
            ],
            None,
        )
        .await?;
        ensure!(status == 200, "calendar: {status}");
        ensure!(
            calendar.contains(&format!("UID:{routine}@minarbot.local")),
            "calendar uid"
        );
        ensure!(
            calendar.contains("RRULE:FREQ=SECONDLY;INTERVAL=300"),
            "calendar rule"
        );
        ensure!(
            !calendar.contains(command["prompt"].as_str().unwrap_or_default()),
            "calendar prompt"
        );
        ensure!(
            w.tp.send(
                &format!(
                    "/api/projects/{}/agents/{}/routines/calendar.ics",
                    w.other, w.agent
                ),
                None
            )
            .await?
            .0 == 404,
            "foreign calendar"
        );
        ensure!(
            w.tp.send(&list, Some(command.clone())).await?.1["receipt"] == created["receipt"],
            "create replay"
        );
        let mut renamed = command.clone();
        renamed["name"] = json!("Other");
        ensure!(
            w.tp.send(&list, Some(renamed)).await?.0 == 409,
            "changed create"
        );
        let (status, _) =
            w.tp.send_as(
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
            w.tp.data(&format!(
                "/api/projects/{}/agents/{}/routines",
                w.other, w.agent
            ))
            .await?["items"]
                == json!([]),
            "other project list"
        );
        ensure!(
            w.tp.send(
                &format!("/api/projects/{}/routines/{routine}", w.other),
                None
            )
            .await?
            .0 == 404,
            "other project read"
        );
        let next = new_id();
        let mut edited = command.clone();
        edited["requestId"] = json!(next);
        edited["id"] = json!(next);
        edited["revision"] = json!(1);
        edited["name"] = json!("Edited");
        let (status, saved) =
            w.tp.send(&format!("{detail}/edit"), Some(edited.clone()))
                .await?;
        ensure!(status == 200, "edit: {status} {saved}");
        ensure!(
            w.tp.send(&list, Some(command.clone())).await?.1["data"]["name"] == "Routine",
            "Creation receipt never overwrites newer configuration"
        );
        ensure!(w.tp.data(&detail).await?["name"] == "Edited", "edited name");
        let flow_id = new_id();
        let flow = json!({"requestId": flow_id, "id": flow_id, "revision": 0, "links": []});
        let (status, flow_saved) =
            w.tp.send(&format!("{detail}/flow"), Some(flow.clone()))
                .await?;
        ensure!(status == 200, "flow: {status} {flow_saved}");
        ensure!(
            flow_saved["data"]["graph"]
                == json!({"root": w.agent, "nodes": [w.agent], "links": []}),
            "flow graph: {flow_saved}"
        );
        w.tp.restart().await?;
        ensure!(
            w.tp.send(&format!("{detail}/edit"), Some(edited.clone()))
                .await?
                .1["receipt"]
                == saved["receipt"],
            "edit replay"
        );
        ensure!(
            w.tp.send(&format!("{detail}/flow"), Some(flow)).await?.1["receipt"]
                == flow_saved["receipt"],
            "flow replay"
        );
        ensure!(
            w.tp.data(&format!("{detail}/flow")).await?["revision"] == 1,
            "flow revision"
        );
        let mut stale = edited.clone();
        stale["requestId"] = json!(new_id());
        stale["id"] = json!(new_id());
        ensure!(
            w.tp.send(&format!("{detail}/edit"), Some(stale)).await?.0 == 400,
            "mismatched ids"
        );
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
        ensure!(
            w.tp.send(&format!("{detail}/edit"), Some(selector))
                .await?
                .0
                == 404,
            "projectId body"
        );
        let db = w.tp.inspect()?;
        ensure!(
            db.rows("SELECT 1 FROM sqlite_schema WHERE name='routine_runs'", [])?
                .is_empty(),
            "Configuration never starts legacy dispatch"
        );
        ensure!(
            db.rows("PRAGMA foreign_key_check", [])?.is_empty(),
            "foreign keys"
        );
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
        let command = routine_command(
            &routine,
            "Catch-up",
            "Scheduled isolated input",
            due,
            Some(300),
            true,
        );
        let before =
            w.tp.data(&format!("/api/sync?projectId={}", w.project))
                .await?;
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
                == vec![
                    (json!("routine"), json!(1)),
                    (json!("routine_occurrence"), json!(1))
                ],
            "observations: {:?}",
            routine_revisions(&observations)
        );
        ensure!(
            !observations
                .to_string()
                .contains("Scheduled isolated input"),
            "prompt synced"
        );
        ensure!(
            pending["id"] == occurrence_id(&routine, due).as_str(),
            "Elapsed occurrence identity follows approved schedule tuple"
        );
        w.tp.restart().await?;
        ensure!(
            w.tp.data(&runs_path).await? == runs,
            "runs changed across restart"
        );
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
                == vec![
                    (json!("routine"), json!(2)),
                    (json!("routine_occurrence"), json!(2))
                ],
            "admitted observations: {:?}",
            routine_revisions(&admitted)
        );
        ensure!(task["data"]["homeDeviceId"] == w.home["id"], "task home");
        ensure!(task["data"]["agentId"] == w.agent.as_str(), "task agent");
        ensure!(
            task["receipt"]["requestId"] == task_id.as_str(),
            "task receipt"
        );
        let current = w.tp.data(&detail).await?;
        ensure!(
            current["next_at"].as_i64().unwrap_or(0) > wall_ms(),
            "next_at advanced"
        );
        ensure!(current["revision"] == 2, "revision");
        ensure!(
            w.tp.send(&list, Some(command.clone())).await?.1["receipt"] == saved["receipt"],
            "create replay"
        );
        w.tp.restart().await?;
        ensure!(
            w.tp.data(&runs_path).await?["items"][0]["taskId"] == task_id.as_str(),
            "task after restart"
        );
        let edit = |revision: i64| {
            let mut edit = command.clone();
            let id = new_id();
            edit["requestId"] = json!(id);
            edit["id"] = json!(id);
            edit["revision"] = json!(revision);
            edit
        };
        ensure!(
            w.tp.send(&format!("{detail}/edit"), Some(edit(1))).await?.0 == 409,
            "stale edit"
        );
        ensure!(
            w.tp.send(&format!("{detail}/edit"), Some(edit(2))).await?.1["error"]["code"]
                == "schedule_processed",
            "processed schedule"
        );
        ensure!(
            w.tp.send(
                &format!("/api/projects/{}/routines/{routine}/runs", w.other),
                None
            )
            .await?
            .0 == 404,
            "foreign runs"
        );
        for suffix in [
            "?before=0".to_owned(),
            "?before=1&before=2".to_owned(),
            format!("?projectId={}", w.project),
        ] {
            ensure!(
                w.tp.send(&format!("{runs_path}{suffix}"), None).await?.0 == 404,
                "runs{suffix}"
            );
        }
        ensure!(
            w.tp.send(&runs_path, Some(json!({"requestId": new_id()})))
                .await?
                .0
                == 404,
            "POST runs"
        );
        let db = w.tp.inspect()?;
        ensure!(
            db.rows("SELECT 1 FROM sqlite_schema WHERE name='routine_runs'", [])?
                .is_empty(),
            "legacy table"
        );
        ensure!(
            db.scalar::<i64>("SELECT count(*) FROM tasks WHERE id=?", [&task_id])? == 1,
            "task row"
        );
        ensure!(
            db.rows("PRAGMA foreign_key_check", [])?.is_empty(),
            "foreign keys"
        );
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
        second_runtime
            .enroll(&w.tp.jwt("user_owner")?, "new")
            .await?;
        let connection = w.host.connect().await?;
        until("home", SETTLE, || async {
            Ok(connection.phase() == "connected")
        })
        .await?;
        let homes = w.tp.data("/api/devices").await?;
        let home = text(&w.home["id"]);
        let second_home = homes
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["id"] != home.as_str()).cloned())
            .ok_or("second home missing")?;
        let second = new_id();
        let owner = account(&w.tp, "user_owner").await?;
        let (status, body) =
            w.tp.send(
                "/api/agents",
                Some(agent_body(
                    &second,
                    "Next",
                    "Help",
                    &w.project,
                    &second_home,
                )),
            )
            .await?;
        ensure!(status == 201, "second agent: {status} {body}");
        bind(&w, &home, &w.agent, &owner, "routine-fixture")?;
        bind(
            &w,
            &text(&second_home["id"]),
            &second,
            &owner,
            "routine-fixture",
        )?;
        let create = routine_command(
            &routine,
            "Flow run",
            "Root work",
            wall_ms() + 600_000,
            None,
            true,
        );
        ensure!(
            w.tp.send(&routines_of(&w), Some(create.clone())).await?.0 == 201,
            "routine"
        );
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
        ensure!(
            w.tp.send(&format!("{detail}/edit"), Some(edited)).await?.0 == 200,
            "edit"
        );
        let run = wait_first_run(&w, &detail, |run| run["state"] == "admitted").await?;
        ensure!(
            run["steps"].as_array().map(Vec::len) == Some(2),
            "steps: {run}"
        );
        ensure!(
            w.tp.data(&detail).await?["enabled"] == 0,
            "one-shot disabled"
        );
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
                check(
                    &failures,
                    conversation["history"].as_array().map(Vec::len) == Some(0),
                    "root history",
                );
                check(
                    &failures,
                    conversation["message"]["text"] == "Root work",
                    "root message",
                );
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
        ensure!(
            step_task(&first_run(&w, &detail).await?) == Some(next.clone()),
            "next task after restart"
        );
        let second_connection = second_runtime.connect().await?;
        until("second home", SETTLE, || async {
            Ok(second_connection.phase() == "connected")
        })
        .await?;
        let failures = checks.handle();
        let ran = second_runtime
            .run_task(&next, move |context| {
                let conversation = &context["snapshot"]["conversation"];
                check(
                    &failures,
                    conversation["history"].as_array().map(Vec::len) == Some(0),
                    "next history",
                );
                check(
                    &failures,
                    conversation["message"]["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("Original root result")),
                    "next message",
                );
                async { Ok(json!({"text": "Next result"})) }
            })
            .await?;
        ensure!(ran["state"] == "completed", "next: {ran}");
        checks.result()?;
        let run = wait_first_run(&w, &detail, |run| run["state"] == "done").await?;
        let states: Vec<Value> = run["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|step| step["state"].clone())
            .collect();
        ensure!(
            states == vec![json!("done"), json!("done")],
            "step states: {states:?}"
        );
        ensure!(
            w.tp.data(&format!("{detail}/runs")).await?["items"]
                .as_array()
                .map(Vec::len)
                == Some(1),
            "one run"
        );
        ensure!(
            w.tp.inspect()?.scalar::<i64>(
                "SELECT count(*) FROM routine_steps WHERE occurrence=?",
                [&text(&run["id"])]
            )? == 2,
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

fn step<'a>(run: &'a Value, agent: &str) -> Option<&'a Value> {
    run["steps"]
        .as_array()
        .and_then(|steps| steps.iter().find(|step| step["agentId"] == agent))
}

fn step_task(run: &Value, agent: &str) -> Option<String> {
    step(run, agent).and_then(|step| step["taskId"].as_str().map(str::to_owned))
}

/// Agent body bound to an existing provider binding.
fn bound_agent(agent: &str, name: &str, project: &str, home: &Value, binding: &str) -> Value {
    let mut body = agent_body(agent, name, "Help", project, home);
    body["providerBinding"] = json!({"bindingId": binding, "expectedRevision": 1});
    body
}

/// The workflow home's current `/api/devices` row.
async fn current_home(w: &WorkflowHome) -> Result<Value> {
    w.tp.data("/api/devices")
        .await?
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == w.home["id"]).cloned())
        .ok_or_else(|| "home missing".into())
}

fn binding_of(w: &WorkflowHome, agent: &str) -> Result<String> {
    w.tp.inspect()?.scalar(
        "SELECT provider_binding FROM agent_authority WHERE agent=?",
        [agent],
    )
}

/// Make the routine's schedule due now (an edit to revision 1).
async fn due_now(w: &WorkflowHome, detail: &str, create: &Value) -> Result<()> {
    let edit = new_id();
    let mut edited = create.clone();
    edited["requestId"] = json!(edit);
    edited["id"] = json!(edit);
    edited["revision"] = json!(1);
    edited["nextAt"] = json!(wall_ms() - 1000);
    ensure!(
        w.tp.send(&format!("{detail}/edit"), Some(edited)).await?.0 == 200,
        "due edit"
    );
    Ok(())
}

async fn outcome_recovers_terminal_result_after_home_sigkill(name: &'static str) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        let routine = new_id();
        let detail = routine_path(&w, &routine);
        let reason = if name == "no_update" {
            "Checked all sources; nothing changed."
        } else {
            "Required service access is missing."
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let checks = Checks::new();
        let (counter, failures) = (calls.clone(), checks.handle());
        let provider = minarbot_testkit::device_host::provider(move |context| {
            check(
                &failures,
                context["snapshot"]["tools"].as_array().is_some_and(|tools| {
                    tools.iter().any(|tool| tool["function"]["name"] == name)
                }),
                "Actual routine step offers outcome tools",
            );
            counter.fetch_add(1, Ordering::SeqCst);
            let arguments = json!({"reason": format!("  {reason}  ")}).to_string();
            async move {
                Ok(json!({"toolCall": {"id": "outcome", "function": {"name": name,
                    "arguments": arguments}}}))
            }
        })
        .await?;
        let mut connection = w.connect()?;
        connected(&connection).await?;
        let home = current_home(&w).await?;
        let (peer, owner) = (new_id(), account(&w.tp, "user_owner").await?);
        bind(&w, &text(&home["id"]), &w.agent, &owner, "routine-outcome")?;
        let binding = binding_of(&w, &w.agent)?;
        let (status, body) = w
            .tp
            .send("/api/agents", Some(bound_agent(&peer, "Downstream", &w.project, &home, &binding)))
            .await?;
        ensure!(status == 201, "peer: {status} {body}");
        let create = routine_command(&routine, "Outcome flow", "Check for changes", wall_ms() + 600_000, None, true);
        ensure!(w.tp.send(&routines_of(&w), Some(create.clone())).await?.0 == 201, "routine");
        let flow = new_id();
        ensure!(
            w.tp.send(
                &format!("{detail}/flow"),
                Some(json!({"requestId": flow, "id": flow, "revision": 0,
                    "links": [{"from": w.agent, "to": peer}]}))
            )
            .await?
            .0 == 200,
            "flow"
        );
        due_now(&w, &detail, &create).await?;
        let run = wait_first_run(&w, &detail, |run| run["state"] == "admitted").await?;
        let task = text(&run["taskId"]);
        connection.stop().await?;
        minarbot_testkit::device_host::crash_task(&w.options, &task, provider.origin(), "tool-reply").await?;
        ensure!(calls.load(Ordering::SeqCst) == 1, "provider calls before crash recovery");
        let terminal = if name == "no_update" { "done" } else { "failed" };
        let run = wait_first_run(&w, &detail, |run| run["state"] == terminal).await?;
        let mut steps: Vec<(Value, Value, Value)> = run["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|step| (step["agentId"].clone(), step["state"].clone(), step["taskId"].clone()))
            .collect();
        let mut expected = vec![
            (json!(w.agent), json!(if name == "no_update" { "skipped" } else { "failed" }), json!(task)),
            (json!(peer), json!("skipped"), Value::Null),
        ];
        let key = |row: &(Value, Value, Value)| row.0.to_string();
        steps.sort_by_key(key);
        expected.sort_by_key(key);
        ensure!(steps == expected, "steps: {steps:?}");
        ensure!(step(&run, &w.agent).is_some_and(|step| step["preview"] == reason), "preview");
        let db = w.tp.inspect()?;
        let turn = db.row("SELECT status,no_update,reply,error FROM turns WHERE id=?", [&task])?;
        let expected_turn = if name == "no_update" {
            json!({"status": "done", "no_update": 1, "reply": reason, "error": null})
        } else {
            json!({"status": "failed", "no_update": 0, "reply": null, "error": reason})
        };
        ensure!(turn == expected_turn, "turn: {turn}");
        ensure!(db.scalar::<i64>("SELECT stopped FROM execution_owners WHERE task=?", [&task])? == 1, "owner stopped");
        let saved = db.row(
            "SELECT r.action,r.content_json FROM task_results r JOIN task_actions a ON a.id=r.action WHERE r.task=? AND a.action_kind='canonical'",
            [&task],
        )?;
        drop(db);
        w.tp.restart().await?;
        for _ in 0..2 {
            let resumed = minarbot_testkit::device_host::run_task_process(&w.options, &task, provider.origin()).await?;
            ensure!(
                resumed["state"] == if name == "no_update" { "completed" } else { "failed" },
                "resumed: {resumed}"
            );
            ensure!(calls.load(Ordering::SeqCst) == 1, "provider calls after resume");
        }
        let db = w.tp.inspect()?;
        ensure!(
            db.scalar::<String>("SELECT content_json FROM task_results WHERE action=?", [&text(&saved["action"])])?
                == text(&saved["content_json"]),
            "saved result changed"
        );
        ensure!(db.scalar::<i64>("SELECT count(*) FROM task_actions WHERE task=?", [&task])? == 2, "actions");
        ensure!(db.scalar::<i64>("SELECT count(*) FROM task_results WHERE task=?", [&task])? == 2, "results");
        ensure!(db.scalar::<i64>("SELECT count(*) FROM tasks WHERE project=?", [&w.project])? == 1, "no fan-out");
        ensure!(first_run(&w, &detail).await? == run, "run changed");
        checks.result()?;
        drop(provider);
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_routine_no_update_recovers_terminal_result_after_home_sigkill_and_server_restart_without_fan_out(
) -> Result<()> {
    outcome_recovers_terminal_result_after_home_sigkill("no_update").await
}

#[tokio::test]
async fn canonical_routine_report_failure_recovers_terminal_result_after_home_sigkill_and_server_restart_without_fan_out(
) -> Result<()> {
    outcome_recovers_terminal_result_after_home_sigkill("report_failure").await
}

#[tokio::test]
async fn canonical_routine_mixed_fan_in_waits_for_success_and_keeps_quiet_and_failed_outcomes_distinct(
) -> Result<()> {
    let w = WorkflowHome::start().await?;
    let result = async {
        let routine = new_id();
        let detail = routine_path(&w, &routine);
        let connection = w.host.connect().await?;
        until("home", SETTLE, || async { Ok(connection.phase() == "connected") }).await?;
        let owner = account(&w.tp, "user_owner").await?;
        bind(&w, &text(&w.home["id"]), &w.agent, &owner, "routine-fan-in")?;
        let binding = binding_of(&w, &w.agent)?;
        let (quiet, failed, good, joined) = (new_id(), new_id(), new_id(), new_id());
        let home = current_home(&w).await?;
        for agent in [&quiet, &failed, &good, &joined] {
            let (status, _) = w
                .tp
                .send("/api/agents", Some(bound_agent(agent, "Flow member", &w.project, &home, &binding)))
                .await?;
            ensure!(status == 201, "flow member: {status}");
        }
        let create = routine_command(&routine, "Mixed flow", "Root work", wall_ms() + 600_000, None, true);
        ensure!(w.tp.send(&routines_of(&w), Some(create.clone())).await?.0 == 201, "routine");
        let links: Vec<Value> = [&quiet, &failed, &good]
            .iter()
            .flat_map(|agent| [json!({"from": w.agent, "to": agent}), json!({"from": agent, "to": joined})])
            .collect();
        let flow = new_id();
        ensure!(
            w.tp.send(&format!("{detail}/flow"), Some(json!({"requestId": flow, "id": flow, "revision": 0, "links": links})))
                .await?
                .0
                == 200,
            "flow"
        );
        due_now(&w, &detail, &create).await?;
        let run = wait_first_run(&w, &detail, |run| run["state"] == "admitted").await?;
        let ran = w.host.run_task(&text(&run["taskId"]), |_| async { Ok(json!({"text": "Root evidence"})) }).await?;
        ensure!(ran["state"] == "completed", "root: {ran}");
        let run = wait_first_run(&w, &detail, |run| {
            [&quiet, &failed, &good].iter().all(|agent| step_task(run, agent).is_some())
        })
        .await?;
        let checks = Checks::new();
        for (agent, name, reason) in [
            (&quiet, "no_update", "Quiet check found no change"),
            (&failed, "report_failure", "One source is unavailable"),
        ] {
            let task = step_task(&run, agent).ok_or("step task")?;
            let failures = checks.handle();
            let ran = w
                .host
                .run_task(&task, move |context| {
                    check(
                        &failures,
                        context["snapshot"]["tools"].as_array().is_some_and(|tools| {
                            tools.iter().any(|tool| tool["function"]["name"] == name)
                        }),
                        "Downstream step offers outcome tools",
                    );
                    let arguments = json!({"reason": reason}).to_string();
                    async move {
                        Ok(json!({"toolCall": {"id": name, "function": {"name": name, "arguments": arguments}}}))
                    }
                })
                .await?;
            ensure!(
                ran["state"] == if name == "no_update" { "completed" } else { "failed" },
                "{name}: {ran}"
            );
        }
        let run = wait_first_run(&w, &detail, |run| {
            step(run, &failed).is_some_and(|step| step["state"] == "failed")
        })
        .await?;
        ensure!(step(&run, &quiet).is_some_and(|step| step["state"] == "skipped"), "quiet skipped");
        ensure!(
            step(&run, &joined).is_some_and(|step| step["taskId"].is_null()),
            "Fan-in waits for every upstream result"
        );
        let good_task = step_task(&run, &good).ok_or("good task")?;
        let ran = w.host.run_task(&good_task, |_| async { Ok(json!({"text": "Actionable result"})) }).await?;
        ensure!(ran["state"] == "completed", "good: {ran}");
        let run = wait_first_run(&w, &detail, |run| step_task(run, &joined).is_some()).await?;
        let joined_task = step_task(&run, &joined).ok_or("joined task")?;
        let failures = checks.handle();
        let ran = w
            .host
            .run_task(&joined_task, move |context| {
                let prompt = context["snapshot"]["conversation"]["message"]["text"].as_str().unwrap_or_default().to_owned();
                check(&failures, prompt.contains("Actionable result"), "good result in prompt");
                check(&failures, prompt.contains("\"status\":\"skipped\""), "skipped status in prompt");
                check(&failures, prompt.contains("One source is unavailable"), "failure in prompt");
                async { Ok(json!({"text": "Merged actionable result"})) }
            })
            .await?;
        ensure!(ran["state"] == "completed", "joined: {ran}");
        checks.result()?;
        let run = wait_first_run(&w, &detail, |run| run["state"] == "failed").await?;
        ensure!(step(&run, &joined).is_some_and(|step| step["state"] == "done"), "joined done");
        ensure!(
            w.tp.inspect()?.scalar::<i64>("SELECT count(*) FROM tasks WHERE project=? AND agent=?", [&w.project, &joined])? == 1,
            "joined tasks"
        );
        connection.stop().await?;
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_routine_keeps_no_reply_descendants_executable_after_completion_and_fences_revoked_root_authority(
) -> Result<()> {
    let w = WorkflowHome::start().await?;
    let result = async {
        let routine = new_id();
        let detail = routine_path(&w, &routine);
        let connection = w.host.connect().await?;
        until("home", SETTLE, || async { Ok(connection.phase() == "connected") }).await?;
        let (peer, owner) = (new_id(), account(&w.tp, "user_owner").await?);
        bind(&w, &text(&w.home["id"]), &w.agent, &owner, "routine-fixture")?;
        let home = w.tp.data("/api/devices").await?[0].clone();
        ensure!(
            w.tp.send("/api/agents", Some(agent_body(&peer, "Teammate", "Help", &w.project, &home))).await?.0 == 201,
            "teammate"
        );
        let create = routine_command(&routine, "Background work", "Delegate without waiting", wall_ms() - 1000, None, true);
        ensure!(w.tp.send(&routines_of(&w), Some(create)).await?.0 == 201, "routine");
        let run = wait_first_run(&w, &detail, |run| run["state"] == "admitted").await?;
        let task = text(&run["taskId"]);
        let calls = Arc::new(AtomicUsize::new(0));
        let (counter, recipient) = (calls.clone(), peer.clone());
        let ran = w
            .host
            .run_task(&task, move |_| {
                let first = counter.fetch_add(1, Ordering::SeqCst) == 0;
                let recipient = recipient.clone();
                async move {
                    Ok(if first {
                        json!({"toolCall": {"id": "delegate", "function": {"name": "send_message",
                            "arguments": json!({"recipients": [recipient], "text": "Background work",
                                "expects_reply": false}).to_string()}}})
                    } else {
                        json!({"text": "Parent done"})
                    })
                }
            })
            .await?;
        ensure!(ran["state"] == "completed", "parent: {ran}");
        wait_first_run(&w, &detail, |run| run["state"] == "done").await?;
        let child: String = w.tp.inspect()?.scalar("SELECT id FROM turns WHERE parent=?", [&task])?;
        let owned = w.host.start_task(&child).await?;
        w.host.context(&child, &owned["ownership"], &owned["revision"]).await?;
        w.tp.write()?.execute(
            "UPDATE agent_projects SET deleted=1 WHERE project=? AND agent=?",
            [&w.project, &w.agent],
        )?;
        let denied = w.host.context(&child, &owned["ownership"], &owned["revision"]).await;
        ensure!(
            denied.as_ref().is_err_and(|error| minarbot_testkit::device_host::is_code(error, "execution_denied")),
            "revoked root context: {denied:?}"
        );
        ensure!(
            w.tp.inspect()?.scalar::<String>("SELECT reply FROM turns WHERE id=?", [&task])? == "Parent done",
            "parent reply"
        );
        connection.stop().await?;
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_routine_runtime_rechecks_grants_and_rolls_back_failed_task_admissions_without_consuming_occurrence(
) -> Result<()> {
    let w = WorkflowHome::start().await?;
    let result = async {
        let routine = new_id();
        let detail = routine_path(&w, &routine);
        let collaborator = account(&w.tp, "user_collaborator").await?;
        let db = w.tp.write()?;
        db.execute("INSERT INTO project_members VALUES (?,?,'member',1,0)", [&w.project, &collaborator])?;
        db.execute("INSERT INTO collaborator_agents VALUES (?,?,?,1,1)", [&w.project, &collaborator, &w.agent])?;
        let command = routine_command(&routine, "Revocable", "Check", wall_ms() - 1000, None, true);
        let as_member = |path: String, body: Option<Value>| {
            let tp = &w.tp;
            async move { tp.send_as(&path, body, "user_collaborator", None).await }
        };
        ensure!(as_member(routines_of(&w), Some(command.clone())).await?.0 == 201, "routine");
        let runs = format!("{detail}/runs");
        let run = minarbot_testkit::wait::until_within("pending", SETTLE, || async {
            let run = as_member(runs.clone(), None).await?.1["data"]["items"][0].clone();
            Ok((run["state"] == "pending").then_some(run))
        })
        .await?;
        let occurrence = text(&run["id"]);
        db.execute(
            "UPDATE collaborator_agents SET enabled=0,revision=2 WHERE project=? AND account=?",
            [&w.project, &collaborator],
        )?;
        let connection = w.host.connect().await?;
        until("home", SETTLE, || async { Ok(connection.phase() == "connected") }).await?;
        let occurrence_task = || -> Result<Value> {
            Ok(minarbot_testkit::db::Inspector::open(w.tp.database())?
                .row("SELECT task FROM routine_occurrences WHERE id=?", [&occurrence])?["task"]
                .clone())
        };
        ensure!(occurrence_task()?.is_null(), "revoked occurrence admitted");
        ensure!(as_member(runs.clone(), None).await?.0 == 404, "revoked runs");
        db.execute(
            "UPDATE collaborator_agents SET enabled=1,revision=3 WHERE project=? AND account=?",
            [&w.project, &collaborator],
        )?;
        db.execute_batch("CREATE TRIGGER fail_routine_admission BEFORE INSERT ON receipts WHEN NEW.scope LIKE 'tasks:routine-step:%' BEGIN SELECT RAISE(ABORT,'synthetic routine receipt failure'); END")?;
        let rebind = new_id();
        let mut edit = command.clone();
        edit["requestId"] = json!(rebind);
        edit["id"] = json!(rebind);
        edit["revision"] = json!(1);
        ensure!(as_member(format!("{detail}/edit"), Some(edit)).await?.0 == 200, "rebind edit");
        until("automation failure", SETTLE, || async {
            Ok(w.tp.app.output()?.1.contains("Project automation failed:"))
        })
        .await?;
        ensure!(occurrence_task()?.is_null(), "Regrant never revives old schedule authority");
        ensure!(db.scalar::<i64>("SELECT count(*) FROM routine_steps", [])? == 0, "routine steps");
        ensure!(db.scalar::<i64>("SELECT count(*) FROM tasks", [])? == 0, "tasks");
        ensure!(w.tp.data(&detail).await?["enabled"] == 1, "enabled");
        db.execute_batch("DROP TRIGGER fail_routine_admission")?;
        let accepted = minarbot_testkit::wait::until_within("admitted", SETTLE, || async {
            let items = as_member(runs.clone(), None).await?.1["data"]["items"].clone();
            Ok(items.as_array().and_then(|rows| rows.iter().find(|row| row["state"] == "admitted").cloned()))
        })
        .await?;
        let task = text(&accepted["taskId"]);
        ensure!(
            as_member(format!("/api/tasks/{task}"), None).await?.1["receipt"]["actorId"] == collaborator.as_str(),
            "task actor"
        );
        db.execute(
            "UPDATE collaborator_agents SET enabled=0,revision=4 WHERE project=? AND account=?",
            [&w.project, &collaborator],
        )?;
        let denied = w.host.start_task(&task).await;
        ensure!(
            denied.as_ref().is_err_and(|error| minarbot_testkit::device_host::is_code(error, "execution_denied")),
            "revoked start: {denied:?}"
        );
        ensure!(db.scalar::<i64>("SELECT count(*) FROM task_actions", [])? == 0, "task actions");
        ensure!(db.rows("PRAGMA foreign_key_check", [])?.is_empty(), "foreign keys");
        connection.stop().await?;
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

async fn admitted_routine(
    w: &WorkflowHome,
    name: &str,
    prompt: &str,
) -> Result<(String, String, Value, Value)> {
    let routine = new_id();
    let detail = routine_path(w, &routine);
    let command = routine_command(&routine, name, prompt, wall_ms() - 1000, Some(300), true);
    ensure!(
        w.tp.send(&routines_of(w), Some(command.clone())).await?.0 == 201,
        "routine"
    );
    let run = wait_first_run(w, &detail, |run| run["state"] == "admitted").await?;
    Ok((routine, detail, command, run))
}

/// Disable the routine at its current revision and schedule.
async fn disable(w: &WorkflowHome, detail: &str, command: &Value) -> Result<(u16, Value)> {
    let current = w.tp.data(detail).await?;
    let edit = new_id();
    let mut body = command.clone();
    body["requestId"] = json!(edit);
    body["id"] = json!(edit);
    body["revision"] = current["revision"].clone();
    body["nextAt"] = current["next_at"].clone();
    body["enabled"] = json!(false);
    w.tp.send(&format!("{detail}/edit"), Some(body)).await
}

#[tokio::test]
async fn canonical_routine_edit_cancels_unstarted_scheduled_work_and_retains_accepted_evidence(
) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        let connection = w.connect()?;
        connected(&connection).await?;
        let (_, detail, command, run) =
            admitted_routine(&w, "Disable queued", "Never start after disable").await?;
        let task_path = format!("/api/tasks/{}", text(&run["taskId"]));
        let (_, task) = w.tp.send(&task_path, None).await?;
        ensure!(task["data"]["state"] == "queued", "queued: {task}");
        let (status, saved) = disable(&w, &detail, &command).await?;
        ensure!(status == 200, "disable: {status} {saved}");
        let (_, after) = w.tp.send(&task_path, None).await?;
        ensure!(after["data"]["state"] == "cancelled", "cancelled");
        ensure!(after["receipt"] == task["receipt"], "receipt kept");
        w.tp.restart().await?;
        ensure!(
            w.tp.data(&task_path).await?["state"] == "cancelled",
            "cancelled after restart"
        );
        drop(connection);
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_routine_runtime_preserves_started_snapshots_through_edits_and_restore_holds_unfinished_occurrences(
) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        let connection = w.host.connect().await?;
        until("home", SETTLE, || async {
            Ok(connection.phase() == "connected")
        })
        .await?;
        let (routine, detail, command, run) =
            admitted_routine(&w, "Started", "Retain started work").await?;
        let task = text(&run["taskId"]);
        let owned = w.host.start_task(&task).await?;
        ensure!(disable(&w, &detail, &command).await?.0 == 200, "disable");
        ensure!(
            w.tp.task_detail(&task).await?["state"] == "running",
            "Configuration edits keep started snapshot under task controls"
        );
        let backup = super::fixture::workspace_snapshot(&w.tp, None)?;
        connection.stop().await?;
        w.tp.restore_from(&backup).await?;
        let check_restored = || -> Result<()> {
            let db = minarbot_testkit::db::Inspector::open(w.tp.database())?;
            let row = db.row(
                "SELECT * FROM routine_occurrences WHERE id=?",
                [&text(&run["id"])],
            )?;
            ensure!(row["state"] == "blocked", "occurrence state: {row}");
            ensure!(row["task"] == task.as_str(), "occurrence task");
            ensure!(row["due_at"] == run["dueAt"], "due");
            ensure!(
                row["schedule_revision"] == run["scheduleRevision"],
                "schedule revision"
            );
            ensure!(
                db.scalar::<i64>("SELECT enabled FROM routines WHERE id=?", [&routine])? == 0,
                "routine enabled"
            );
            ensure!(
                db.scalar::<String>("SELECT state FROM tasks WHERE id=?", [&task])? == "uncertain",
                "task state"
            );
            Ok(())
        };
        check_restored()?;
        w.tp.restart().await?;
        let db = w.tp.inspect()?;
        ensure!(db.count("routine_occurrences")? == 1, "occurrences");
        ensure!(
            db.scalar::<String>(
                "SELECT state FROM routine_occurrences WHERE id=?",
                [&text(&run["id"])]
            )? == "blocked",
            "blocked after restart"
        );
        ensure!(
            db.rows("PRAGMA foreign_key_check", [])?.is_empty(),
            "foreign keys"
        );
        ensure!(
            w.host
                .context(&task, &owned["ownership"], &owned["revision"])
                .await
                .is_err(),
            "restored task context"
        );
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_routine_cancellation_settles_terminal_occurrence_after_task_cancel() -> Result<()>
{
    let mut w = WorkflowHome::start().await?;
    let result = async {
        let connection = w.connect()?;
        connected(&connection).await?;
        let (_, detail, _, run) = admitted_routine(&w, "Cancel", "Cancel me").await?;
        let task = text(&run["taskId"]);
        let current = w.tp.task_detail(&task).await?;
        let (status, _) =
            w.tp.send(
                &format!("/api/tasks/{task}/control"),
                Some(
                    json!({"requestId": new_id(), "expectedRevision": current["revision"],
                    "operation": "cancel"}),
                ),
            )
            .await?;
        ensure!(status == 200, "cancel: {status}");
        let run = wait_first_run(&w, &detail, |run| run["state"] == "failed").await?;
        ensure!(
            w.tp.task_detail(&task).await?["state"] == "cancelled",
            "task cancelled"
        );
        ensure!(
            w.tp.inspect()?.scalar::<String>(
                "SELECT state FROM routine_steps WHERE occurrence=?",
                [&text(&run["id"])]
            )? == "failed",
            "step failed"
        );
        w.tp.restart().await?;
        ensure!(
            first_run(&w, &detail).await?["state"] == "failed",
            "failed after restart"
        );
        drop(connection);
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_routines_recheck_assignments_private_mappings_flow_agents_and_receipt_rollback(
) -> Result<()> {
    let w = WorkflowHome::start().await?;
    let result = async {
        let list = routines_of(&w);
        let routine = new_id();
        let detail = routine_path(&w, &routine);
        let command = routine_command(&routine, "Routine", "Check", wall_ms() + 600_000, None, false);
        for fields in [
            json!({"trigger": ["webhook"]}),
            json!({"trigger": {}}),
            json!({"nextAt": -1}),
            json!({"intervalSeconds": 1.5}),
            json!({"prompt": ""}),
        ] {
            let invalid = new_id();
            let mut body = command.clone();
            for (key, value) in fields.as_object().into_iter().flatten() {
                body[key] = value.clone();
            }
            body["requestId"] = json!(invalid);
            body["id"] = json!(invalid);
            ensure!(w.tp.send(&list, Some(body)).await?.0 == 400, "invalid {fields}");
        }
        ensure!(w.tp.data(&list).await?["items"] == json!([]), "Invalid bodies write no configuration");
        ensure!(w.tp.send(&list, Some(command.clone())).await?.0 == 201, "routine");
        let collaborator = account(&w.tp, "user_collaborator").await?;
        let db = w.tp.write()?;
        let as_member = |path: String, body: Option<Value>| {
            let tp = &w.tp;
            async move { tp.send_as(&path, body, "user_collaborator", None).await }
        };
        db.execute("INSERT INTO project_members VALUES (?,?,'member',1,0)", [&w.project, &collaborator])?;
        ensure!(as_member(detail.clone(), None).await?.0 == 404, "unassigned read");
        db.execute("INSERT INTO collaborator_agents VALUES (?,?,?,1,1)", [&w.project, &collaborator, &w.agent])?;
        db.execute("INSERT INTO response_policies VALUES (?,?,?,0,1)", [&w.project, &collaborator, &w.agent])?;
        ensure!(as_member(detail.clone(), None).await?.0 == 200, "Response OFF retains assignment");
        let edit_id = new_id();
        let mut edit = command.clone();
        edit["requestId"] = json!(edit_id);
        edit["id"] = json!(edit_id);
        edit["revision"] = json!(1);
        edit["name"] = json!("Collaborator");
        ensure!(as_member(format!("{detail}/edit"), Some(edit.clone())).await?.0 == 200, "member edit");
        db.execute(
            "UPDATE collaborator_agents SET enabled=0,revision=revision+1 WHERE project=? AND account=?",
            [&w.project, &collaborator],
        )?;
        ensure!(
            as_member(format!("{detail}/edit"), Some(edit.clone())).await?.0 == 404,
            "Revoked grants cannot read old receipts"
        );
        db.execute(
            "UPDATE collaborator_agents SET enabled=1,revision=revision+1 WHERE project=? AND account=?",
            [&w.project, &collaborator],
        )?;
        let owner = account(&w.tp, "user_owner").await?;
        let key = json!([routine]).to_string();
        db.execute(
            "UPDATE record_scopes SET account=?,visibility='owner' WHERE table_name='routines' AND key_json=?",
            [&collaborator, &key],
        )?;
        ensure!(w.tp.send(&detail, None).await?.0 == 404, "Owner cannot read another human private root");
        ensure!(as_member(detail.clone(), None).await?.0 == 200, "private owner read");
        db.execute(
            "UPDATE record_scopes SET account=?,visibility='project' WHERE table_name='routines' AND key_json=?",
            [&owner, &key],
        )?;
        let teammate = new_id();
        ensure!(
            w.tp.send("/api/agents", Some(agent_body(&teammate, "Teammate", "Help", &w.project, &w.home))).await?.0 == 201,
            "teammate"
        );
        let flow_id = new_id();
        let flow = json!({"requestId": flow_id, "id": flow_id, "revision": 0,
            "links": [{"from": w.agent, "to": teammate}]});
        let flow_path = format!("{detail}/flow");
        ensure!(as_member(flow_path.clone(), Some(flow.clone())).await?.0 == 404, "Every flow agent needs assignment");
        let mut bad = flow.clone();
        bad["links"] = json!([{"from": w.agent, "to": new_id()}]);
        ensure!(w.tp.send(&flow_path, Some(bad)).await?.0 == 400, "unknown flow agent");
        ensure!(w.tp.send(&flow_path, Some(flow.clone())).await?.0 == 200, "flow");
        db.execute(
            "UPDATE record_scopes SET project=? WHERE table_name='routine_flows' AND key_json=?",
            [&w.other, &key],
        )?;
        ensure!(w.tp.send(&flow_path, Some(flow.clone())).await?.0 == 404, "Receipt cannot expose a foreign flow");
        db.execute(
            "UPDATE record_scopes SET project=? WHERE table_name='routine_flows' AND key_json=?",
            [&w.project, &key],
        )?;
        db.execute_batch("CREATE TRIGGER fail_routine_receipt BEFORE INSERT ON receipts WHEN NEW.scope LIKE '%/edit' BEGIN SELECT RAISE(ABORT,'synthetic routine receipt failure'); END")?;
        let change = new_id();
        let mut atomic = edit.clone();
        atomic["requestId"] = json!(change);
        atomic["id"] = json!(change);
        atomic["revision"] = json!(2);
        atomic["name"] = json!("Atomic");
        ensure!(w.tp.send(&format!("{detail}/edit"), Some(atomic.clone())).await?.0 == 500, "receipt failure");
        ensure!(w.tp.data(&detail).await?["name"] == "Collaborator", "edit rolled back");
        db.execute_batch("DROP TRIGGER fail_routine_receipt")?;
        ensure!(w.tp.send(&format!("{detail}/edit"), Some(atomic.clone())).await?.0 == 200, "edit");
        let short = new_id();
        let mut short_edit = atomic.clone();
        short_edit["requestId"] = json!(short);
        short_edit["id"] = json!(short);
        short_edit["revision"] = json!(3);
        short_edit["intervalSeconds"] = json!(60);
        ensure!(w.tp.send(&format!("{detail}/edit"), Some(short_edit)).await?.0 == 400, "short interval");
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

async fn calendar(
    w: &WorkflowHome,
    path: &str,
    subject: &str,
    suffix: String,
    extra: Vec<(&'static str, String)>,
) -> Result<(u16, std::collections::HashMap<String, String>, String)> {
    let request = new_id();
    let mut headers: Vec<(&str, &str)> =
        vec![("Connection", "close"), ("X-Minarbot-Request-Id", &request)];
    headers.extend(extra.iter().map(|(name, value)| (*name, value.as_str())));
    raw_as(
        &w.tp,
        subject,
        Method::GET,
        &format!("{path}{suffix}"),
        &headers,
        None,
    )
    .await
}

/// The wall-clock case runs its server and home in a fixed zone without
/// daylight saving (Node used the ambient `TZ` and `onceCandidate`).
const WALL_ZONE: &str = "Asia/Jakarta";

/// `onceCandidate` in `WALL_ZONE` (UTC+07:00).
fn wall_candidate(date: &str, time: &str) -> Result<i64> {
    let utc = chrono_like_parse(date, time)?;
    Ok(utc - 7 * 3_600_000)
}

/// Milliseconds of `<date>T<time>:00Z`.
fn chrono_like_parse(date: &str, time: &str) -> Result<i64> {
    let parts: Vec<i64> = date
        .split('-')
        .chain(time.split(':'))
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    let [year, month, day, hour, minute] = parts[..] else {
        return Err("bad wall time".into());
    };
    // Days from civil (Howard Hinnant).
    let (y, m) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Ok(((days * 24 + hour) * 60 + minute) * 60_000)
}

#[tokio::test]
async fn canonical_routine_wall_clock_http_keeps_exact_retries_and_pending_identities_across_restart_and_catch_up_admission(
) -> Result<()> {
    let servers = minarbot_testkit::Servers::reserve(1).await?;
    let zone = [("TZ", WALL_ZONE)];
    let mut w =
        WorkflowHome::start_env(super::fixture::Tp::start_env(&servers, &zone).await?, &zone)
            .await?;
    let result = async {
        ensure!(w.home["zone"] == WALL_ZONE, "home zone: {}", w.home);
        let list = routines_of(&w);
        let (once, cron) = (new_id(), new_id());
        let (once_path, cron_path) = (routine_path(&w, &once), routine_path(&w, &cron));
        let schedule = json!({"kind": "once", "date": "2024-03-10", "time": "02:30"});
        let command = json!({"requestId": once, "id": once, "revision": 0, "name": "Once",
            "prompt": "PRIVATE_WALL_CLOCK_PROMPT", "schedule": schedule, "enabled": true});
        for extra in [
            json!({"nextAt": 0}),
            json!({"intervalSeconds": null}),
            json!({"nextAt": 0, "intervalSeconds": 300}),
        ] {
            let mut body = command.clone();
            for (key, value) in extra.as_object().into_iter().flatten() {
                body[key] = value.clone();
            }
            ensure!(
                w.tp.send(&list, Some(body)).await?.0 == 400,
                "legacy timing {extra}"
            );
        }
        // The first reply is lost after the server accepted it.
        let (status, lost) = w.tp.send(&list, Some(command.clone())).await?;
        ensure!(status == 201, "lost create: {status} {lost}");
        let (status, saved) = w.tp.send(&list, Some(command.clone())).await?;
        ensure!(status == 200, "retry: {status}");
        ensure!(saved["data"]["schedule"] == schedule, "schedule");
        ensure!(
            saved["data"]["next_at"] == wall_candidate("2024-03-10", "02:30")?,
            "next_at: {saved}"
        );
        let mut cron_command = command.clone();
        cron_command["requestId"] = json!(cron);
        cron_command["id"] = json!(cron);
        cron_command["name"] = json!("Daily cron");
        cron_command["schedule"] = json!({"kind": "cron", "expression": "0 9 * * *"});
        ensure!(
            w.tp.send(&list, Some(cron_command)).await?.0 == 201,
            "cron routine"
        );
        // A durable due time retained while the canonical process was down.
        w.tp.write()?.execute(
            "UPDATE routines SET next_at=? WHERE id=?",
            minarbot_testkit::hosted::params![wall_candidate("2024-01-01", "09:00")?, cron],
        )?;
        let (pending_once, pending_cron) =
            minarbot_testkit::wait::until_within("pending", SETTLE, || async {
                let once = first_run(&w, &once_path).await?;
                let cron = first_run(&w, &cron_path).await?;
                Ok((once["state"] == "pending" && cron["state"] == "pending")
                    .then_some((once, cron)))
            })
            .await?;
        ensure!(pending_once["scheduleKind"] == "once", "once kind");
        ensure!(pending_cron["scheduleKind"] == "cron", "cron kind");
        ensure!(
            pending_cron["missedCount"].as_i64().unwrap_or(0) > 1,
            "missed"
        );
        let tuple = json!([cron, 1, w.home["zone_revision"], pending_cron["dueAt"], 0]).to_string();
        let digest = ring::digest::digest(&ring::digest::SHA256, tuple.as_bytes());
        use minarbot_testkit::base64::Engine;
        ensure!(
            pending_cron["id"]
                == minarbot_testkit::base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(digest.as_ref())
                    .as_str(),
            "cron identity"
        );
        let (status, calendar) = raw(
            &w,
            Method::GET,
            &format!("{list}/calendar.ics"),
            &[
                ("Connection", "close"),
                ("X-Minarbot-Request-Id", &new_id()),
            ],
            None,
        )
        .await?;
        let calendar = calendar.replace("\r\n ", "");
        ensure!(status == 200, "calendar: {status}");
        ensure!(
            calendar.contains(&format!("UID:{once}@minarbot.local")),
            "once in calendar"
        );
        ensure!(
            !calendar.contains(&format!("UID:{cron}@minarbot.local")),
            "cron in calendar"
        );
        ensure!(
            calendar.contains("Omitted cron routine \"Daily cron\""),
            "omitted note"
        );
        ensure!(
            !calendar.contains("PRIVATE_WALL_CLOCK_PROMPT"),
            "prompt in calendar"
        );
        let snapshot =
            w.tp.data(&format!("/api/sync?projectId={}", w.project))
                .await?;
        let observed: Vec<&Value> = events(&snapshot, |kind| kind.starts_with("routine")).collect();
        ensure!(
            observed
                .iter()
                .all(|event| event["body"]["scheduleKind"] == "once"
                    || event["body"]["scheduleKind"] == "cron"),
            "schedule kinds"
        );
        ensure!(
            !json!(observed).to_string().contains("PRIVATE_"),
            "private text synced"
        );
        w.tp.restart().await?;
        ensure!(
            w.tp.send(&list, Some(command.clone())).await?.1["receipt"] == saved["receipt"],
            "receipt replay"
        );
        ensure!(
            w.tp.data(&format!("{once_path}/runs")).await?["items"] == json!([pending_once]),
            "once runs"
        );
        ensure!(
            w.tp.data(&format!("{cron_path}/runs")).await?["items"] == json!([pending_cron]),
            "cron runs"
        );
        let connection = w.connect()?;
        connected(&connection).await?;
        let (admitted_once, admitted_cron) =
            minarbot_testkit::wait::until_within("admitted", SETTLE, || async {
                let once = first_run(&w, &once_path).await?;
                let cron = first_run(&w, &cron_path).await?;
                Ok((once["state"] == "admitted" && cron["state"] == "admitted")
                    .then_some((once, cron)))
            })
            .await?;
        ensure!(admitted_once["id"] == pending_once["id"], "once identity");
        ensure!(admitted_cron["id"] == pending_cron["id"], "cron identity");
        ensure!(
            w.tp.data(&once_path).await?["enabled"] == 0,
            "once disabled"
        );
        ensure!(
            w.tp.data(&cron_path).await?["next_at"]
                .as_i64()
                .unwrap_or(0)
                > wall_ms(),
            "cron advanced"
        );
        let enable = new_id();
        let mut again = command.clone();
        again["requestId"] = json!(enable);
        again["id"] = json!(enable);
        again["revision"] = json!(2);
        let (status, rejected) = w.tp.send(&format!("{once_path}/edit"), Some(again)).await?;
        ensure!(status == 409, "re-enable: {status}");
        ensure!(rejected["error"]["code"] == "schedule_processed", "code");
        w.tp.restart().await?;
        ensure!(
            w.tp.send(&list, Some(command)).await?.1["receipt"] == saved["receipt"],
            "receipt after restart"
        );
        ensure!(
            first_run(&w, &once_path).await?["taskId"] == admitted_once["taskId"],
            "once task"
        );
        ensure!(
            first_run(&w, &cron_path).await?["taskId"] == admitted_cron["taskId"],
            "cron task"
        );
        let db = w.tp.inspect()?;
        ensure!(db.count("routine_occurrences")? == 2, "occurrences");
        ensure!(db.count("tasks")? == 2, "tasks");
        ensure!(
            db.rows("PRAGMA foreign_key_check", [])?.is_empty(),
            "foreign keys"
        );
        drop(connection);
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_routine_calendar_exports_only_current_project_and_reader_schedules_without_prompts(
) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        let list = routines_of(&w);
        let path = format!("{list}/calendar.ics");
        let at = chrono_like_parse("2027-01-01", "09:00")?;
        let collaborator = account(&w.tp, "user_collaborator").await?;
        let db = w.tp.write()?;
        db.execute("INSERT INTO project_members VALUES (?,?,'member',1,0)", [&w.project, &collaborator])?;
        let mut entries = std::collections::HashMap::new();
        for name in ["shared", "private", "disabled", "webhook", "foreign"] {
            let id = new_id();
            entries.insert(name, id.clone());
            let body = json!({"requestId": id, "id": id, "revision": 0, "name": name,
                "prompt": "CALENDAR_PRIVATE_PROMPT", "nextAt": at,
                "intervalSeconds": if name == "shared" { json!(300) } else { Value::Null },
                "enabled": name != "disabled",
                "trigger": if name == "webhook" { "webhook" } else { "schedule" }});
            ensure!(w.tp.send(&list, Some(body)).await?.0 == 201, "routine {name}");
        }
        db.execute(
            "UPDATE record_scopes SET visibility='owner' WHERE table_name='routines' AND key_json=?",
            [&json!([entries["private"]]).to_string()],
        )?;
        db.execute(
            "UPDATE record_scopes SET project=? WHERE table_name='routines' AND key_json=?",
            [&w.other, &json!([entries["foreign"]]).to_string()],
        )?;
        let (status, headers, owned) = calendar(&w, &path, "user_owner", String::new(), vec![]).await?;
        ensure!(status == 200, "owner calendar: {status}");
        ensure!(headers.get("content-type").map(String::as_str) == Some("text/calendar; charset=utf-8"), "content type");
        ensure!(headers.get("x-minarbot-protocol").map(String::as_str) == Some("1"), "protocol header");
        ensure!(headers.get("cache-control").map(String::as_str) == Some("no-store"), "cache control");
        ensure!(owned.contains(&format!("UID:{}@minarbot.local", entries["shared"])), "shared entry");
        ensure!(owned.contains(&format!("UID:{}@minarbot.local", entries["private"])), "private entry");
        for name in ["disabled", "webhook", "foreign"] {
            ensure!(!owned.contains(&entries[name]), "{name}");
        }
        ensure!(!owned.contains("CALENDAR_PRIVATE_PROMPT"), "prompt");
        ensure!(owned.contains("DTSTART:20270101T090000Z"), "start");
        ensure!(owned.contains("RRULE:FREQ=SECONDLY;INTERVAL=300"), "rule");
        ensure!(calendar(&w, &path, "user_collaborator", String::new(), vec![]).await?.0 == 404, "unassigned reader");
        db.execute("INSERT INTO collaborator_agents VALUES (?,?,?,1,1)", [&w.project, &collaborator, &w.agent])?;
        db.execute("INSERT INTO response_policies VALUES (?,?,?,0,1)", [&w.project, &collaborator, &w.agent])?;
        let (status, _, shared) = calendar(&w, &path, "user_collaborator", String::new(), vec![]).await?;
        ensure!(status == 200, "collaborator calendar: {status}");
        ensure!(shared.contains(&entries["shared"]), "shared for collaborator");
        ensure!(!shared.contains(&entries["private"]), "private for collaborator");
        db.execute(
            "UPDATE devices SET zone='Pacific/Honolulu',zone_revision=zone_revision+1 WHERE id=?",
            [&text(&w.home["id"])],
        )?;
        drop(db);
        w.tp.restart().await?;
        ensure!(
            calendar(&w, &path, "user_collaborator", String::new(), vec![]).await?.2.contains("DTSTART:20270101T090000Z"),
            "Viewer/home-zone changes cannot rewrite absolute timing"
        );
        for suffix in [format!("?projectId={}", w.project), format!("?after={}", entries["shared"])] {
            ensure!(calendar(&w, &path, "user_collaborator", suffix.clone(), vec![]).await?.0 == 404, "{suffix}");
        }
        ensure!(
            calendar(&w, &path, "user_collaborator", String::new(), vec![("X-Minarbot-Project-Id", w.project.clone())]).await?.0 == 404,
            "project selector header"
        );
        w.tp.write()?.execute(
            "UPDATE collaborator_agents SET enabled=0,revision=revision+1 WHERE project=? AND account=?",
            [&w.project, &collaborator],
        )?;
        let (status, _, denied) = calendar(&w, &path, "user_collaborator", String::new(), vec![]).await?;
        ensure!(status == 404, "revoked: {status}");
        ensure!(!denied.contains(&entries["shared"]), "revoked text");
        ensure!(
            w.tp.send(&format!("/api/agents/{}/routines/calendar.ics", w.agent), None).await?.0 == 404,
            "unscoped calendar"
        );
        ensure!(
            w.tp.inspect()?.scalar::<i64>("SELECT enabled FROM routines WHERE id=?", [&entries["shared"]])? == 1,
            "Export cannot mutate a schedule"
        );
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}
