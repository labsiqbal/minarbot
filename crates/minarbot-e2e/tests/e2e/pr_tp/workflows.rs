//! Canonical workflow execution: conditions, approvals, home tasks, retries.

use super::{
    fixture::{account, encode, locked, until, SETTLE},
    workflow_waits::WorkflowHome,
};
use minarbot_testkit::prelude::*;
use std::sync::{Arc, Mutex};

pub async fn create(w: &WorkflowHome, name: &str, graph: &Value) -> Result<()> {
    let (status, body) =
        w.tp.send(
            &w.list,
            Some(
                json!({"requestId": w.workflow, "id": w.workflow, "revision": 0, "name": name,
                "graph": graph}),
            ),
        )
        .await?;
    ensure!(status == 201, "workflow: {status} {body}");
    Ok(())
}

pub async fn start_run(w: &WorkflowHome, input: &str) -> Result<(String, String)> {
    let run = new_id();
    let (status, body) =
        w.tp.send(
            &format!("{}/run", w.detail),
            Some(json!({"requestId": run, "id": run, "revision": 1, "input": input})),
        )
        .await?;
    ensure!(status == 201, "run: {status} {body}");
    let path = format!("/api/projects/{}/workflow-runs/{run}", w.project);
    Ok((run, path))
}

pub async fn wait_run(
    w: &WorkflowHome,
    path: &str,
    done: impl Fn(&Value) -> bool,
) -> Result<Value> {
    minarbot_testkit::wait::until_within(format!("run {path}"), SETTLE, || async {
        let current = w.tp.data(path).await?;
        Ok(done(&current).then_some(current))
    })
    .await
}

/// Bind the workflow agent to a ready provider binding on its home.
pub async fn bind_provider(w: &WorkflowHome) -> Result<()> {
    let binding = new_id();
    let owner = account(&w.tp, "user_owner").await?;
    let db = w.tp.write()?;
    db.execute(
        "INSERT INTO provider_bindings VALUES (?,?,?,'openai-compatible','workflow-fixture','Fixture','ready',1)",
        [binding.as_str(), w.home["id"].as_str().unwrap_or_default(), owner.as_str()],
    )?;
    db.execute(
        "UPDATE agent_authority SET provider_binding=?,revision=revision+1 WHERE agent=?",
        [&binding, &w.agent],
    )?;
    Ok(())
}

/// Connect the test-control host.
pub async fn connect(w: &WorkflowHome) -> Result<minarbot_testkit::device_host::Connection> {
    let connection = w.host.connect().await?;
    until("home connected", SETTLE, || async {
        Ok(connection.phase() == "connected")
    })
    .await?;
    Ok(connection)
}

/// Provider that records the first failed check instead of panicking.
pub struct Checks(Arc<Mutex<Vec<String>>>);

impl Checks {
    pub fn new() -> Self {
        Self(Arc::default())
    }

    pub fn handle(&self) -> Arc<Mutex<Vec<String>>> {
        self.0.clone()
    }

    pub fn result(&self) -> Result<()> {
        let failures = locked(&self.0).clone();
        ensure!(failures.is_empty(), "provider checks failed: {failures:?}");
        Ok(())
    }
}

pub fn check(failures: &Mutex<Vec<String>>, ok: bool, label: &str) {
    if !ok {
        locked(failures).push(label.to_owned());
    }
}

fn statuses(steps: &Value) -> Vec<(String, String)> {
    steps
        .as_array()
        .into_iter()
        .flatten()
        .map(|step| {
            (
                step["node"].as_str().unwrap_or_default().to_owned(),
                step["status"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

#[tokio::test]
async fn canonical_workflow_executes_conditions_and_approval_once_across_restart() -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        let graph = json!({"entry": "check", "maxSteps": 3, "nodes": [
            {"id": "check", "kind": "condition", "operator": "equals", "value": "Original input",
                "yes": "approve", "no": "out"},
            {"id": "approve", "kind": "approval", "prompt": "Proceed?", "yes": "out"},
            {"id": "out", "kind": "output"}]});
        create(&w, "Execution", &graph).await?;
        let run = new_id();
        let command = json!({"requestId": run, "id": run, "revision": 1, "input": "Original input"});
        let path = format!("/api/projects/{}/workflow-runs/{run}", w.project);
        let (status, admission) = w.tp.send(&format!("{}/run", w.detail), Some(command.clone())).await?;
        ensure!(status == 201, "admission: {status}");
        let current = wait_run(&w, &path, |run| run["status"] == "approval").await?;
        ensure!(
            statuses(&current["steps"])
                == vec![("check".into(), "done".into()), ("approve".into(), "approval".into())],
            "steps: {}",
            current["steps"]
        );
        w.tp.restart().await?;
        ensure!(w.tp.data(&path).await?["steps"] == current["steps"], "steps changed across restart");
        let decision = new_id();
        let approval = json!({"requestId": decision, "id": decision, "revision": current["revision"],
            "accepted": true});
        let (status, accepted) = w.tp.send(&format!("{path}/approve"), Some(approval.clone())).await?;
        ensure!(status == 200, "approve: {status} {accepted}");
        let current = wait_run(&w, &path, |run| run["status"] == "done").await?;
        ensure!(current["last"] == "Original input", "last: {current}");
        ensure!(current["stepCount"] == 3, "steps: {current}");
        ensure!(
            w.tp.send(&format!("{path}/approve"), Some(approval)).await?.1 == accepted,
            "approval replay"
        );
        ensure!(
            w.tp.send(&format!("{}/run", w.detail), Some(command)).await?.1["receipt"]
                == admission["receipt"],
            "admission replay"
        );
        w.tp.restart().await?;
        ensure!(w.tp.data(&path).await?["stepCount"] == 3, "steps after restart");
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_workflow_admits_isolated_home_tasks_and_keeps_model_nodes_tool_free(
) -> Result<()> {
    let w = WorkflowHome::start().await?;
    let result = async {
        let connection = connect(&w).await?;
        let graph = json!({"entry": "agent", "maxSteps": 3, "nodes": [
            {"id": "agent", "kind": "agent", "agent": w.agent, "prompt": "Agent work", "next": "model"},
            {"id": "model", "kind": "model", "model": "workflow-model", "prompt": "Model work", "next": "out"},
            {"id": "out", "kind": "output"}]});
        create(&w, "Home tasks", &graph).await?;
        bind_provider(&w).await?;
        let owner = account(&w.tp, "user_owner").await?;
        let (_, path) = start_run(&w, "Synthetic input").await?;
        let current = wait_run(&w, &path, |run| run["status"] == "waiting").await?;
        let first = current["steps"][0]["turn"].as_str().unwrap_or_default().to_owned();
        let task = w.tp.task_detail(&first).await?;
        ensure!(task["homeDeviceId"] == w.home["id"], "task home");
        ensure!(task["agentId"] == w.agent.as_str(), "task agent");
        let owned = w.host.start_task(&first).await?;
        let context = w
            .host
            .context(&first, &owned["ownership"], &owned["revision"])
            .await?;
        let snapshot = &context["snapshot"];
        ensure!(
            snapshot["tools"].as_array().is_some_and(|tools| !tools.is_empty()),
            "agent tools"
        );
        ensure!(
            snapshot["admission"]["initiatorAccountId"] == owner.as_str(),
            "initiator"
        );
        ensure!(
            snapshot["conversation"]["history"].as_array().map(Vec::len) == Some(0),
            "history"
        );
        ensure!(
            snapshot["conversation"]["message"]["text"]
                .as_str()
                .unwrap_or_default()
                .contains("Agent work\n\nWorkflow input:\nSynthetic input"),
            "message: {}",
            snapshot["conversation"]["message"]
        );
        let cancel = new_id();
        ensure!(
            w.tp.send(
                &format!("{path}/cancel"),
                Some(json!({"requestId": cancel, "id": cancel, "revision": current["revision"]}))
            )
            .await?
            .0 == 200,
            "cancel"
        );
        ensure!(w.tp.task_detail(&first).await?["state"] == "cancelled", "task cancelled");
        ensure!(
            w.host
                .context(&first, &owned["ownership"], &owned["revision"])
                .await
                .is_err(),
            "cancelled task context"
        );
        let (_, next_path) = start_run(&w, "Synthetic input").await?;
        let current = wait_run(&w, &next_path, |run| run["status"] == "waiting").await?;
        let agent_task = current["steps"][0]["turn"].as_str().unwrap_or_default().to_owned();
        let ran = w
            .host
            .run_task(&agent_task, |_| async { Ok(json!({"text": "Agent result"})) })
            .await?;
        ensure!(ran["state"] == "completed", "agent step: {ran}");
        let current = wait_run(&w, &next_path, |run| {
            run["steps"].as_array().map(Vec::len) == Some(2)
        })
        .await?;
        let model_task = current["steps"][1]["turn"].as_str().unwrap_or_default().to_owned();
        let checks = Checks::new();
        let failures = checks.handle();
        let ran = w
            .host
            .run_task(&model_task, move |context| {
                let failures = failures.clone();
                async move {
                    let snapshot = &context["snapshot"];
                    check(&failures, snapshot["profile"]["model"] == "workflow-model", "model");
                    check(&failures, snapshot["profile"]["instructions"] == "Help", "instructions");
                    for field in ["tools", "memories", "skills", "skillAssets"] {
                        check(&failures, snapshot[field] == json!([]), field);
                    }
                    check(
                        &failures,
                        snapshot["conversation"]["history"].as_array().map(Vec::len) == Some(0),
                        "history",
                    );
                    check(
                        &failures,
                        snapshot["conversation"]["message"]["text"]
                            .as_str()
                            .is_some_and(|text| text.ends_with("Previous step result:\nAgent result")),
                        "message",
                    );
                    Ok(json!({"text": "Model result"}))
                }
            })
            .await?;
        checks.result()?;
        ensure!(ran["state"] == "completed", "model step: {ran}");
        let current = wait_run(&w, &next_path, |run| run["status"] == "done").await?;
        ensure!(current["last"] == "Model result", "last: {current}");
        ensure!(current["stepCount"] == 3, "steps: {current}");
        connection.stop().await?;
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_workflow_retry_preserves_topology_and_failed_evidence_while_using_current_node_settings_once(
) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        let connection = connect(&w).await?;
        bind_provider(&w).await?;
        let graph = json!({"entry": "check", "maxSteps": 5, "nodes": [
            {"id": "check", "kind": "condition", "operator": "model", "model": "old-model",
                "value": "Original question", "yes": "approve", "no": "out"},
            {"id": "approve", "kind": "approval", "prompt": "Keep original approval?", "yes": "out"},
            {"id": "out", "kind": "output"}]});
        create(&w, "Retry", &graph).await?;
        let sync = format!("/api/sync?projectId={}", w.project);
        let before = w.tp.data(&sync).await?;
        let (_, path) = start_run(&w, "Original input").await?;
        let current = wait_run(&w, &path, |run| run["status"] == "waiting").await?;
        let first = current["steps"][0]["turn"].as_str().unwrap_or_default().to_owned();
        let ran = w
            .host
            .run_task(&first, |_| async { Ok(json!({"text": "Invalid decision"})) })
            .await?;
        ensure!(ran["state"] == "completed", "first step: {ran}");
        let current = wait_run(&w, &path, |run| run["status"] == "failed").await?;
        let failed = current["steps"][0].clone();
        let id = new_id();
        let command = json!({"requestId": id, "id": id, "revision": current["revision"]});
        let mut changed = graph.clone();
        changed["nodes"][0]["model"] = json!("retry-model");
        changed["nodes"][0]["value"] = json!("New question");
        changed["nodes"][0]["yes"] = json!("out");
        changed["nodes"][0]["no"] = json!("approve");
        let edit = new_id();
        ensure!(
            w.tp.send(
                &format!("{}/edit", w.detail),
                Some(json!({"requestId": edit, "id": edit, "revision": 1, "name": "Changed", "graph": changed}))
            )
            .await?
            .0 == 200,
            "edit"
        );
        let (status, accepted) = w.tp.send(&format!("{path}/retry"), Some(command.clone())).await?;
        ensure!(status == 200, "retry: {status} {accepted}");
        w.tp.restart().await?;
        let current = wait_run(&w, &path, |run| run["status"] == "waiting" && run["stepCount"] == 2).await?;
        ensure!(
            w.tp.send(&format!("{path}/retry"), Some(command.clone())).await?.1 == accepted,
            "Lost-response retry reuses receipt after restart and advancement"
        );
        let mut conflict = command.clone();
        conflict["revision"] = json!(command["revision"].as_i64().unwrap_or(0) + 1);
        ensure!(
            w.tp.send(&format!("{path}/retry"), Some(conflict)).await?.1["error"]["code"] == "payload_conflict",
            "payload conflict"
        );
        let mut stale = command.clone();
        let stale_id = new_id();
        stale["requestId"] = json!(stale_id);
        stale["id"] = json!(stale_id);
        ensure!(
            w.tp.send(&format!("{path}/retry"), Some(stale)).await?.1["error"]["code"] == "stale_revision",
            "stale revision"
        );
        ensure!(current["steps"][0] == failed, "failed evidence changed");
        ensure!(current["steps"][1]["turn"] != first.as_str(), "retry reused the task");
        let retry_task = current["steps"][1]["turn"].as_str().unwrap_or_default().to_owned();
        let checks = Checks::new();
        let failures = checks.handle();
        let ran = w
            .host
            .run_task(&retry_task, move |context| {
                let failures = failures.clone();
                async move {
                    let snapshot = &context["snapshot"];
                    check(&failures, snapshot["profile"]["model"] == "retry-model", "model");
                    check(&failures, snapshot["tools"] == json!([]), "tools");
                    check(
                        &failures,
                        snapshot["conversation"]["message"]["text"]
                            .as_str()
                            .is_some_and(|text| text.starts_with("New question\n")),
                        "message",
                    );
                    Ok(json!({"text": "yes"}))
                }
            })
            .await?;
        checks.result()?;
        ensure!(ran["state"] == "completed", "retry step: {ran}");
        let current = wait_run(&w, &path, |run| run["status"] == "approval").await?;
        ensure!(
            current["cursor"] == "approve",
            "Retry keeps original branch despite changed latest edges"
        );
        ensure!(current["last"] == "Original input", "last");
        ensure!(current["version"] == 1, "version");
        ensure!(current["graph"] == graph, "graph");
        let steps: Vec<&str> = current["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|step| step["status"].as_str())
            .collect();
        ensure!(steps == ["failed", "done", "approval"], "steps: {steps:?}");
        ensure!(
            w.tp.send(&format!("{path}/retry"), Some(command)).await?.1 == accepted,
            "retry replay"
        );
        let (status, observed) = w
            .tp
            .send(&format!("{sync}&cursor={}", encode(before["nextCursor"].as_str().unwrap_or_default())), None)
            .await?;
        ensure!(status == 200, "sync: {status} {observed}");
        let observations: Vec<(Value, Value)> = observed["data"]["page"]["events"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|event| event["recordKind"] == "workflow_run")
            .map(|event| (event["revision"].clone(), event["body"]["status"].clone()))
            .collect();
        let expected: Vec<(Value, Value)> = [
            (1, "ready"), (2, "waiting"), (3, "failed"), (4, "ready"), (5, "waiting"), (6, "ready"), (7, "approval"),
        ]
        .into_iter()
        .map(|(revision, status)| (json!(revision), json!(status)))
        .collect();
        ensure!(observations == expected, "observations: {observations:?}");
        let raw = json!(observed["data"]["page"]["events"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|event| event["recordKind"] == "workflow_run")
            .collect::<Vec<_>>())
        .to_string();
        ensure!(!raw.contains("Original input"), "sync leaked the input");
        ensure!(!raw.contains("Invalid decision"), "sync leaked the result");
        connection.stop().await?;
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

async fn cancel_attempt(w: &WorkflowHome, path: &str) -> Result<Value> {
    let current = wait_run(w, path, |run| run["status"] == "waiting").await?;
    let turn = current["steps"]
        .as_array()
        .and_then(|steps| steps.last())
        .and_then(|step| step["turn"].as_str())
        .unwrap_or_default()
        .to_owned();
    let task = w.tp.task_detail(&turn).await?;
    let (status, _) = w
        .tp
        .send(
            &format!("/api/tasks/{}/control", task["taskId"].as_str().unwrap_or(&turn)),
            Some(json!({"requestId": new_id(), "expectedRevision": task["revision"], "operation": "cancel"})),
        )
        .await?;
    ensure!(status == 200, "cancel attempt: {status}");
    wait_run(w, path, |run| run["status"] == "failed").await
}

fn command(revision: &Value) -> Value {
    let id = new_id();
    json!({"requestId": id, "id": id, "revision": revision})
}

#[tokio::test]
async fn canonical_workflow_retry_changes_agent_per_attempt_and_falls_back_to_the_original_node_after_removal(
) -> Result<()> {
    let w = WorkflowHome::start().await?;
    let result = async {
        let connection = connect(&w).await?;
        let second = new_id();
        let home = w.tp.data("/api/devices").await?[0].clone();
        let (status, _) = w
            .tp
            .send("/api/agents", Some(super::fixture::agent_body(&second, "Retry agent", "Retry profile", &w.project, &home)))
            .await?;
        ensure!(status == 201, "second agent: {status}");
        let binding = new_id();
        let owner = account(&w.tp, "user_owner").await?;
        let db = w.tp.write()?;
        db.execute(
            "INSERT INTO provider_bindings VALUES (?,?,?,'openai-compatible','workflow-fixture','Fixture','ready',1)",
            [binding.as_str(), home["id"].as_str().unwrap_or_default(), owner.as_str()],
        )?;
        db.execute(
            "UPDATE agent_authority SET provider_binding=?,revision=revision+1 WHERE agent IN (?,?)",
            [&binding, &w.agent, &second],
        )?;
        let graph = json!({"entry": "work", "maxSteps": 4, "nodes": [
            {"id": "work", "kind": "agent", "agent": w.agent, "prompt": "Original work", "next": "out"},
            {"id": "out", "kind": "output"}]});
        create(&w, "Agent retry", &graph).await?;
        let (_, path) = start_run(&w, "Input").await?;
        let current = cancel_attempt(&w, &path).await?;
        let mut changed = graph.clone();
        changed["nodes"][0]["agent"] = json!(second);
        changed["nodes"][0]["prompt"] = json!("Changed work");
        let edit = new_id();
        ensure!(
            w.tp.send(&format!("{}/edit", w.detail), Some(json!({"requestId": edit, "id": edit, "revision": 1, "name": "Changed", "graph": changed}))).await?.0 == 200,
            "edit"
        );
        ensure!(
            w.tp.send(&format!("{path}/retry"), Some(command(&current["revision"]))).await?.0 == 200,
            "retry"
        );
        let current = wait_run(&w, &path, |run| run["status"] == "waiting" && run["stepCount"] == 2).await?;
        let second_task = current["steps"][1]["turn"].as_str().unwrap_or_default().to_owned();
        let owned = w.host.start_task(&second_task).await?;
        let context = w.host.context(&second_task, &owned["ownership"], &owned["revision"]).await?;
        ensure!(context["snapshot"]["profile"]["name"] == "Retry agent", "retry profile");
        ensure!(
            context["snapshot"]["conversation"]["message"]["text"]
                .as_str()
                .is_some_and(|text| text.starts_with("Changed work\n")),
            "retry prompt"
        );
        ensure!(w.tp.task_detail(&second_task).await?["agentId"] == second.as_str(), "retry agent");
        let remove = new_id();
        ensure!(
            w.tp.send(&format!("{}/edit", w.detail), Some(json!({"requestId": remove, "id": remove, "revision": 2, "name": "Removed", "graph": w.graph}))).await?.0 == 200,
            "remove node"
        );
        let current = cancel_attempt(&w, &path).await?;
        ensure!(
            w.tp.send(&format!("{path}/retry"), Some(command(&current["revision"]))).await?.0 == 200,
            "fallback retry"
        );
        let current = wait_run(&w, &path, |run| run["status"] == "waiting" && run["stepCount"] == 3).await?;
        let third = current["steps"][2]["turn"].as_str().unwrap_or_default().to_owned();
        ensure!(w.tp.task_detail(&third).await?["agentId"] == w.agent.as_str(), "fallback agent");
        let checks = Checks::new();
        let failures = checks.handle();
        let ran = w
            .host
            .run_task(&third, move |context| {
                let failures = failures.clone();
                async move {
                    check(
                        &failures,
                        context["snapshot"]["conversation"]["message"]["text"]
                            .as_str()
                            .is_some_and(|text| text.starts_with("Original work\n")),
                        "fallback prompt",
                    );
                    Ok(json!({"text": "Recovered once"}))
                }
            })
            .await?;
        checks.result()?;
        ensure!(ran["state"] == "completed", "fallback run: {ran}");
        let current = wait_run(&w, &path, |run| run["status"] == "done").await?;
        ensure!(current["last"] == "Recovered once", "last");
        ensure!(current["stepCount"] == 4, "steps");
        let steps: Vec<&str> = current["steps"].as_array().into_iter().flatten().filter_map(|step| step["status"].as_str()).collect();
        ensure!(steps == ["failed", "failed", "done", "done"], "steps: {steps:?}");
        ensure!(
            w.tp.send(&format!("{path}/retry"), Some(command(&current["revision"]))).await?.0 == 409,
            "retry after success"
        );
        connection.stop().await?;
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_workflow_retry_checks_capacity_scopes_and_original_authority_with_atomic_receipts(
) -> Result<()> {
    let w = WorkflowHome::start().await?;
    let result = async {
        let mut connection = w.connect()?;
        super::workflow_waits::connected(&connection).await?;
        let owner = account(&w.tp, "user_owner").await?;
        let collaborator = account(&w.tp, "user_collaborator").await?;
        let db = w.tp.write()?;
        db.execute("INSERT INTO project_members VALUES (?,?,'member',1,0)", [&w.project, &collaborator])?;
        db.execute("INSERT INTO collaborator_agents VALUES (?,?,?,1,1)", [&w.project, &collaborator, &w.agent])?;
        db.execute("INSERT INTO response_policies VALUES (?,?,?,0,1)", [&w.project, &collaborator, &w.agent])?;
        let graph = json!({"entry": "work", "maxSteps": 3, "nodes": [
            {"id": "work", "kind": "agent", "agent": w.agent, "prompt": "Work", "next": "out"},
            {"id": "out", "kind": "output"}]});
        create(&w, "Authorized retry", &graph).await?;
        let run = new_id();
        let path = format!("/api/projects/{}/workflow-runs/{run}", w.project);
        ensure!(
            w.tp.send_as(&format!("{}/run", w.detail), Some(json!({"requestId": run, "id": run, "revision": 1, "input": "Input"})), "user_collaborator", None).await?.0 == 201,
            "collaborator run"
        );
        let failed = cancel_attempt(&w, &path).await?;
        let retry = format!("{path}/retry");
        let body = command(&failed["revision"]);
        ensure!(
            w.tp.send(&format!("/api/projects/{}/workflow-runs/{run}/retry", w.other), Some(body.clone())).await?.0 == 404,
            "foreign project retry"
        );
        let mut extra = body.clone();
        extra["accepted"] = json!(true);
        ensure!(w.tp.send(&retry, Some(extra)).await?.0 == 400, "extra field");
        let run_key = minarbot_testkit::protocol::canonical_json(&json!([run]));
        let step_key = minarbot_testkit::protocol::canonical_json(&json!([run, 1]));
        db.execute(
            "UPDATE record_scopes SET visibility='owner',account=? WHERE table_name='workflow_runs' AND key_json=?",
            [owner.as_str(), run_key.as_str()],
        )?;
        ensure!(
            w.tp.send_as(&retry, Some(body.clone()), "user_collaborator", None).await?.0 == 404,
            "private run retry"
        );
        db.execute(
            "UPDATE record_scopes SET visibility='project',account=? WHERE table_name='workflow_runs' AND key_json=?",
            [collaborator.as_str(), run_key.as_str()],
        )?;
        db.execute(
            "UPDATE record_scopes SET project=? WHERE table_name='workflow_steps' AND key_json=?",
            [w.other.as_str(), step_key.as_str()],
        )?;
        ensure!(w.tp.send(&retry, Some(body.clone())).await?.0 == 404, "foreign step scope");
        db.execute(
            "UPDATE record_scopes SET project=? WHERE table_name='workflow_steps' AND key_json=?",
            [w.project.as_str(), step_key.as_str()],
        )?;
        let busy: Vec<String> = (0..100).map(|_| new_id()).collect();
        for other in &busy {
            db.execute(
                "INSERT INTO workflow_runs(id,workflow,version,agent,graph,input,last,cursor,status,created) SELECT ?,workflow,version,agent,graph,input,last,cursor,'approval',created FROM workflow_runs WHERE id=?",
                [other.as_str(), run.as_str()],
            )?;
        }
        let (status, full) = w.tp.send_as(&retry, Some(body.clone()), "user_collaborator", None).await?;
        ensure!(status == 429, "capacity: {status}");
        ensure!(full["error"]["code"] == "server_capacity", "capacity code");
        ensure!(full["error"]["retryable"] == true, "capacity retryable");
        ensure!(
            w.tp.data(&path).await? == failed,
            "Capacity denial leaves failed evidence and revision unchanged"
        );
        for other in &busy {
            db.execute("DELETE FROM workflow_runs WHERE id=?", [other])?;
        }
        db.execute_batch("CREATE TRIGGER fail_retry_receipt BEFORE INSERT ON receipts WHEN NEW.scope LIKE 'retained:%/workflow-runs/%/retry' BEGIN SELECT RAISE(ABORT,'retry receipt failure'); END")?;
        ensure!(
            w.tp.send_as(&retry, Some(body.clone()), "user_collaborator", None).await?.0 == 500,
            "receipt failure"
        );
        ensure!(w.tp.data(&path).await? == failed, "Receipt failure rolls back retry admission");
        db.execute_batch("DROP TRIGGER fail_retry_receipt")?;
        db.execute(
            "UPDATE collaborator_agents SET enabled=0,revision=revision+1 WHERE project=? AND account=?",
            [&w.project, &collaborator],
        )?;
        ensure!(
            w.tp.send_as(&retry, Some(body.clone()), "user_collaborator", None).await?.0 == 404,
            "revoked collaborator retry"
        );
        ensure!(
            w.tp.send(&retry, Some(body.clone())).await?.0 == 403,
            "Owner retry cannot revive revoked initiator authority"
        );
        db.execute(
            "UPDATE collaborator_agents SET enabled=1,revision=revision+1 WHERE project=? AND account=?",
            [&w.project, &collaborator],
        )?;
        let (status, accepted) = w.tp.send_as(&retry, Some(body.clone()), "user_collaborator", None).await?;
        ensure!(status == 200, "retry: {status} {accepted}");
        ensure!(accepted["receipt"]["actorId"] == collaborator.as_str(), "receipt actor");
        db.execute(
            "UPDATE collaborator_agents SET enabled=0,revision=revision+1 WHERE project=? AND account=?",
            [&w.project, &collaborator],
        )?;
        ensure!(
            w.tp.send_as(&retry, Some(body), "user_collaborator", None).await?.0 == 404,
            "Revoked caller cannot disclose retained receipt"
        );
        connection.stop().await?;
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_workflow_dispatch_retains_original_collaborator_and_owner_cancellation_fences_revoked_work(
) -> Result<()> {
    let w = WorkflowHome::start().await?;
    let result = async {
        let connection = connect(&w).await?;
        let owner = account(&w.tp, "user_owner").await?;
        let collaborator = account(&w.tp, "user_collaborator").await?;
        let db = w.tp.write()?;
        db.execute("INSERT INTO project_members VALUES (?,?,'member',1,0)", [&w.project, &collaborator])?;
        db.execute("INSERT INTO collaborator_agents VALUES (?,?,?,1,1)", [&w.project, &collaborator, &w.agent])?;
        db.execute("INSERT INTO response_policies VALUES (?,?,?,0,1)", [&w.project, &collaborator, &w.agent])?;
        let graph = json!({"entry": "work", "maxSteps": 2, "nodes": [
            {"id": "work", "kind": "agent", "agent": w.agent, "prompt": "Synthetic work", "next": "out"},
            {"id": "out", "kind": "output"}]});
        create(&w, "Collaborator", &graph).await?;
        let run = new_id();
        let path = format!("/api/projects/{}/workflow-runs/{run}", w.project);
        ensure!(
            w.tp.send_as(&format!("{}/run", w.detail), Some(json!({"requestId": run, "id": run, "revision": 1, "input": "Input"})), "user_collaborator", None).await?.0 == 201,
            "collaborator run"
        );
        db.execute(
            "UPDATE record_scopes SET account=? WHERE table_name='workflow_runs' AND key_json=?",
            [owner.as_str(), minarbot_testkit::protocol::canonical_json(&json!([run])).as_str()],
        )?;
        let current = wait_run(&w, &path, |run| run["status"] == "waiting").await?;
        let child = current["steps"][0]["turn"].as_str().unwrap_or_default().to_owned();
        let owned = w.host.start_task(&child).await?;
        let context = w.host.context(&child, &owned["ownership"], &owned["revision"]).await?;
        ensure!(
            context["snapshot"]["admission"]["initiatorAccountId"] == collaborator.as_str(),
            "initiator"
        );
        ensure!(
            w.tp.send_as(&format!("/api/tasks/{child}"), None, "user_collaborator", None).await?.0 == 200,
            "Response OFF preserves execution and project visibility"
        );
        db.execute(
            "UPDATE collaborator_agents SET enabled=0,revision=revision+1 WHERE project=? AND account=?",
            [&w.project, &collaborator],
        )?;
        let denied = w.host.context(&child, &owned["ownership"], &owned["revision"]).await;
        ensure!(
            denied.as_ref().is_err_and(|error| minarbot_testkit::device_host::is_code(error, "execution_denied")),
            "revoked context: {denied:?}"
        );
        let body = command(&current["revision"]);
        db.execute_batch("CREATE TRIGGER fail_workflow_cancel BEFORE INSERT ON receipts WHEN NEW.scope LIKE 'retained:%/workflow-runs/%/cancel' BEGIN SELECT RAISE(ABORT,'cancel receipt failure'); END")?;
        ensure!(w.tp.send(&format!("{path}/cancel"), Some(body.clone())).await?.0 == 500, "receipt failure");
        ensure!(
            w.tp.task_detail(&child).await?["state"] == "running",
            "Failed parent receipt rolls back child fencing"
        );
        db.execute_batch("DROP TRIGGER fail_workflow_cancel")?;
        ensure!(w.tp.send(&format!("{path}/cancel"), Some(body)).await?.0 == 200, "cancel");
        ensure!(w.tp.data(&path).await?["status"] == "cancelled", "run cancelled");
        ensure!(w.tp.task_detail(&child).await?["state"] == "cancelled", "child cancelled");
        ensure!(w.host.start_task(&child).await.is_err(), "cancelled child restarted");
        connection.stop().await?;
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}
