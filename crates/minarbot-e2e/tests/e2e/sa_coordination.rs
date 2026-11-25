//! Durable delegation, transfer, cancellation, rejection and recovery in
//! channels (port of `app/coordination.test.ts`).

use crate::sa_support::{after_tool, last_tool_result, lock, Shared};
use minarbot_testkit::{
    gate::ModelGate,
    model::{Reply, ScriptedModel},
    prelude::*,
    Server,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

const CALLING: &[&str] = &[
    "pause-tree",
    "save-result",
    "parallel-delegate",
    "nested",
    "nested-child",
    "delegate",
    "transfer",
    "cancel-tree",
    "restart-tree",
    "outsider",
    "cycle",
];

fn row_of(rows: &Value, check: impl Fn(&Value) -> bool) -> Option<Value> {
    rows.as_array()
        .and_then(|rows| rows.iter().find(|row| check(row)).cloned())
}

async fn status(app: &Server, path: &str, id: &str, status: &str) -> Result<Value> {
    app.wait_status(path, id, status).await
}

async fn running_child(app: &Server, path: &str, parent: &str) -> Result<Value> {
    let rows = app
        .wait_get(path, |rows| {
            row_of(rows, |row| {
                row["parent"] == parent && row["status"] == "running"
            })
            .is_some()
        })
        .await?;
    row_of(&rows, |row| row["parent"] == parent).ok_or_else(|| "child missing".into())
}

#[tokio::test]
async fn coordination_durable_delegation_transfer_cancellation_rejection_and_recovery() -> Result<()>
{
    let model = ScriptedModel::start().await?;
    let gate = ModelGate::start(&model).await?;
    let calls: Shared<Vec<(String, String)>> = Shared::default();
    let allow_paused = Arc::new(AtomicBool::new(false));
    let (log, paused) = (calls.clone(), allow_paused.clone());
    model.respond(move |request| {
        // Like B1: delegate and transfer return null and the harness takes
        // one more step before the server parks or stops the turn. End that
        // step with empty text (the server keeps the turn waiting) and leave
        // it out of the call log.
        if request
            .tool_results()
            .last()
            .is_some_and(|m| m["content"] == "null")
        {
            return Reply::text("");
        }
        let task = request.last_user_text();
        if let Ok(mut log) = log.lock() {
            log.push((request.model().to_owned(), task.clone()));
        }
        let result = if after_tool(request) {
            last_tool_result(request)
        } else {
            None
        };
        if task == "child-hang"
            || task == "block-parent"
            || (task == "paused-child" && !paused.load(Ordering::SeqCst))
        {
            return Reply::hold();
        }
        let should_call = (result.is_none() && CALLING.contains(&task.as_str()))
            || (task == "cycle" && result.as_ref().is_none_or(|r| r["error"].is_null()));
        if should_call {
            let recipient = if task == "outsider" {
                json!(new_id())
            } else {
                request.body["tools"]
                    .as_array()
                    .and_then(|tools| {
                        tools.iter().find(|t| {
                            t["function"]["name"] == "delegate_task"
                                || t["function"]["name"] == "transfer_task"
                        })
                    })
                    .map(|t| {
                        t["function"]["parameters"]["properties"]["recipient"]["enum"][0].clone()
                    })
                    .unwrap_or(Value::Null)
            };
            let name = if task == "transfer" || task == "cycle" {
                "transfer_task"
            } else {
                "delegate_task"
            };
            let child = match task.as_str() {
                "cancel-tree" | "restart-tree" => "child-hang",
                "pause-tree" => "paused-child",
                "save-result" => "saved-child",
                "nested" => "nested-child",
                _ => "child-work",
            };
            return Reply::tool_call(
                &new_id(),
                name,
                json!({"recipient": recipient, "task": child}),
            );
        }
        Reply::text(match result {
            Some(result) => format!(
                "parent:{}",
                result["error"]
                    .as_str()
                    .or(result["reply"].as_str())
                    .or(result["status"].as_str())
                    .unwrap_or_default()
            ),
            None => "child-result".into(),
        })
    });
    let tasks = |task: &str| {
        lock(&calls)
            .map(|calls| calls.iter().filter(|(_, t)| t == task).count())
            .unwrap_or(0)
    };
    let root = TestRoot::new()?;
    let options = ServerConfig::new().gateway(gate.gateway());
    let mut app = root.start(options).await?;
    let agent = |name: &str, model: &str| json!({"runtime": "api", "id": new_id(), "name": name, "model": model, "instructions": "Coordinate."});
    let alpha = app.post("/api/agents", agent("Alpha", "alpha")).await?;
    let beta = app.post("/api/agents", agent("Beta", "beta")).await?;
    let (a, b) = (
        alpha["id"].as_str().unwrap_or_default().to_owned(),
        beta["id"].as_str().unwrap_or_default().to_owned(),
    );
    let channel = app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Team", "instructions": "Collaborate.", "members": [a, b], "lead": a}),
        )
        .await?;
    let room = channel["id"].as_str().unwrap_or_default().to_owned();
    let path = format!("/api/channels/{room}/turns");
    let delegated = json!({"id": new_id(), "text": "delegate", "recipient": a});
    app.post(&path, delegated.clone()).await?;
    app.post(&path, delegated.clone()).await?;
    let parent_id = delegated["id"].as_str().unwrap_or_default().to_owned();
    let parent = status(&app, &path, &parent_id, "done").await?;
    ensure!(parent["agent"] == a.as_str(), "parent agent: {parent}");
    ensure!(
        parent["reply"] == "parent:child-result",
        "parent reply: {}",
        parent["reply"]
    );
    let rows = app.get(&path).await?;
    let child = row_of(&rows, |row| row["parent"] == parent_id.as_str()).ok_or("child missing")?;
    ensure!(child["agent"] == b.as_str(), "child agent: {child}");
    ensure!(child["status"] == "done", "child status: {child}");
    ensure!(
        rows.as_array().map(Vec::len) == Some(2),
        "rows: {}",
        rows.as_array().map_or(0, Vec::len)
    );
    ensure!(
        tasks("child-work") == 1,
        "child work calls: {}",
        tasks("child-work")
    );
    ensure!(
        app.get(&format!("/api/agents/{b}/turns")).await? == json!([]),
        "child in main history"
    );
    let transfer = json!({"id": new_id(), "text": "transfer", "recipient": a});
    app.post(&path, transfer.clone()).await?;
    let moved = status(
        &app,
        &path,
        transfer["id"].as_str().unwrap_or_default(),
        "done",
    )
    .await?;
    ensure!(moved["agent"] == b.as_str(), "transferred agent: {moved}");
    ensure!(
        moved["reply"] == "parent:transferred",
        "transfer reply: {}",
        moved["reply"]
    );
    ensure!(
        app.get(&path).await?.as_array().map(Vec::len) == Some(3),
        "rows after transfer"
    );
    app.post(&path, transfer).await?;
    ensure!(
        app.get(&path).await?.as_array().map(Vec::len) == Some(3),
        "transfer replay added a row"
    );
    let outsider = json!({"id": new_id(), "text": "outsider", "recipient": a});
    let before = lock(&calls)?.len();
    app.post(&path, outsider.clone()).await?;
    let rejected = status(
        &app,
        &path,
        outsider["id"].as_str().unwrap_or_default(),
        "done",
    )
    .await?;
    ensure!(
        rejected["reply"]
            .as_str()
            .is_some_and(|r| r.contains("member")),
        "outsider: {}",
        rejected["reply"]
    );
    ensure!(
        lock(&calls)?.len() == before + 2,
        "outsider calls: {}",
        lock(&calls)?.len() - before
    );
    let cycle_a = app.post("/api/agents", agent("Cycle A", "cycle")).await?;
    let cycle_b = app.post("/api/agents", agent("Cycle B", "cycle")).await?;
    let cycle_room = app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Cycle", "instructions": "Coordinate.",
                "members": [cycle_a["id"], cycle_b["id"]], "lead": cycle_a["id"]}),
        )
        .await?;
    let cycle_path = format!(
        "/api/channels/{}/turns",
        cycle_room["id"].as_str().unwrap_or_default()
    );
    let cycle = new_id();
    app.post(
        &cycle_path,
        json!({"id": cycle, "text": "cycle", "recipient": cycle_a["id"]}),
    )
    .await?;
    let cycled = minarbot_testkit::wait::until_within(
        "cycle limit",
        std::time::Duration::from_secs(120),
        || async {
            Ok(app
                .row(&cycle_path, &cycle)
                .await?
                .filter(|row| row["status"] == "done"))
        },
    )
    .await?;
    ensure!(
        cycled["reply"]
            .as_str()
            .is_some_and(|r| r.contains("Coordination limit")),
        "cycle: {}",
        cycled["reply"]
    );
    ensure!(
        cycled["coordination"].as_array().map(Vec::len) == Some(32),
        "cycle steps: {}",
        cycled["coordination"]
    );
    let cancellation = new_id();
    app.post(
        &path,
        json!({"id": cancellation, "text": "cancel-tree", "recipient": a}),
    )
    .await?;
    let child = running_child(&app, &path, &cancellation).await?;
    until("hanging child model call", || async {
        Ok((tasks("child-hang") == 1).then_some(()))
    })
    .await?;
    app.post(
        &format!("{path}/{cancellation}/cancel"),
        json!({"id": new_id()}),
    )
    .await?;
    let cancelled = status(&app, &path, &cancellation, "cancelled").await?;
    let error = cancelled["error"].as_str().unwrap_or_default();
    ensure!(error.contains("already sent"), "cancel error: {error}");
    ensure!(!error.contains("before sending"), "cancel error: {error}");
    status(
        &app,
        &path,
        child["id"].as_str().unwrap_or_default(),
        "cancelled",
    )
    .await?;
    ensure!(
        status(&app, &path, &parent_id, "done").await?["reply"] == "parent:child-result",
        "earlier parent changed"
    );
    let pausing = new_id();
    app.post(
        &path,
        json!({"id": pausing, "text": "pause-tree", "recipient": a}),
    )
    .await?;
    let child = running_child(&app, &path, &pausing).await?;
    let child_id = child["id"].as_str().unwrap_or_default().to_owned();
    // The harness reaches the model shortly after the child starts running.
    until("paused child model call", || async {
        Ok((tasks("paused-child") == 1).then_some(()))
    })
    .await?;
    app.post(
        &format!("{path}/{pausing}/pause"),
        json!({"id": new_id(), "revision": 1}),
    )
    .await?;
    ensure!(
        status(&app, &path, &pausing, "interrupted").await?["paused"] == 1,
        "paused parent"
    );
    ensure!(
        status(&app, &path, &child_id, "interrupted").await?["paused"] == 1,
        "paused child"
    );
    app.restart().await?;
    ensure!(
        tasks("paused-child") == 1,
        "paused child calls: {}",
        tasks("paused-child")
    );
    let resume = app
        .call(
            minarbot_testkit::http::Method::POST,
            &format!("{path}/{child_id}/resume"),
            Some(json!({"id": new_id(), "revision": 2, "acknowledgeUncertain": true})),
        )
        .await?;
    ensure!(
        resume.status == 409,
        "child resume: {} {}",
        resume.status,
        resume.body
    );
    ensure!(
        resume.body["error"]
            .as_str()
            .is_some_and(|e| e.contains("parent")),
        "child resume error: {}",
        resume.body
    );
    ensure!(
        status(&app, &path, &child_id, "interrupted").await?["paused"] == 1,
        "child resumed alone"
    );
    allow_paused.store(true, Ordering::SeqCst);
    app.post(
        &format!("{path}/{pausing}/resume"),
        json!({"id": new_id(), "revision": 2, "acknowledgeUncertain": true}),
    )
    .await?;
    ensure!(
        status(&app, &path, &pausing, "done").await?["reply"] == "parent:child-result",
        "resumed tree"
    );
    ensure!(
        tasks("paused-child") == 2,
        "paused child calls: {}",
        tasks("paused-child")
    );
    ensure!(
        app.get(&path).await?.as_array().map_or(0, |rows| rows
            .iter()
            .filter(|r| r["parent"] == pausing.as_str())
            .count())
            == 1,
        "resume created another child"
    );
    let archive_parent = new_id();
    app.post(
        &path,
        json!({"id": archive_parent, "text": "cancel-tree", "recipient": a}),
    )
    .await?;
    let child = running_child(&app, &path, &archive_parent).await?;
    until("hanging child model call", || async {
        Ok((tasks("child-hang") == 2).then_some(()))
    })
    .await?;
    let edit = json!({"id": new_id(), "revision": 1, "name": "Team", "instructions": "Collaborate.", "members": [a, b], "lead": a, "archived": true});
    app.post(&format!("/api/channels/{room}/edit"), edit.clone())
        .await?;
    status(&app, &path, &archive_parent, "cancelled").await?;
    status(
        &app,
        &path,
        child["id"].as_str().unwrap_or_default(),
        "cancelled",
    )
    .await?;
    let mut unarchive = edit.clone();
    unarchive["id"] = json!(new_id());
    unarchive["revision"] = json!(2);
    unarchive["archived"] = json!(false);
    app.post(&format!("/api/channels/{room}/edit"), unarchive)
        .await?;
    ensure!(
        status(&app, &path, &parent_id, "done").await?["reply"] == "parent:child-result",
        "parent after archive"
    );
    let restart = new_id();
    app.post(
        &path,
        json!({"id": restart, "text": "restart-tree", "recipient": a}),
    )
    .await?;
    let child = running_child(&app, &path, &restart).await?;
    until("hanging child model call", || async {
        Ok((tasks("child-hang") == 3).then_some(()))
    })
    .await?;
    app.restart().await?;
    status(&app, &path, &restart, "interrupted").await?;
    status(
        &app,
        &path,
        child["id"].as_str().unwrap_or_default(),
        "interrupted",
    )
    .await?;
    ensure!(
        tasks("restart-tree") == 1,
        "restart parent calls: {}",
        tasks("restart-tree")
    );
    ensure!(
        tasks("child-hang") == 3,
        "hanging child calls: {}",
        tasks("child-hang")
    );
    ensure!(
        status(&app, &path, &parent_id, "done").await?["reply"] == "parent:child-result",
        "parent after restart"
    );
    let nested = new_id();
    app.post(
        &path,
        json!({"id": nested, "text": "nested", "recipient": a}),
    )
    .await?;
    ensure!(
        status(&app, &path, &nested, "done").await?["reply"] == "parent:parent:child-result",
        "nested reply"
    );
    let rows = app.get(&path).await?;
    let nested_child =
        row_of(&rows, |row| row["parent"] == nested.as_str()).ok_or("nested child")?;
    let grandchild =
        row_of(&rows, |row| row["parent"] == nested_child["id"]).ok_or("grandchild")?;
    ensure!(
        grandchild["agent"] == a.as_str(),
        "grandchild agent: {grandchild}"
    );
    ensure!(
        grandchild["status"] == "done",
        "grandchild status: {grandchild}"
    );
    gate.hold_when(|body| {
        let messages = body["messages"].as_array().cloned().unwrap_or_default();
        let last_user = messages
            .iter()
            .rev()
            .find(|m| m["role"] == "user")
            .and_then(|m| m["content"].as_str())
            .unwrap_or("");
        last_user == "parallel-delegate" && messages.last().is_some_and(|m| m["role"] != "tool")
    });
    let mut parallel = Vec::new();
    for i in 0..4 {
        let lead = app
            .post(
                "/api/agents",
                agent(&format!("Parallel {i}"), &format!("parallel-{i}")),
            )
            .await?;
        let room = app
            .post(
                "/api/channels",
                json!({"id": new_id(), "name": format!("Parallel {i}"), "instructions": "Work.", "members": [lead["id"], b], "lead": lead["id"]}),
            )
            .await?;
        let target = format!(
            "/api/channels/{}/turns",
            room["id"].as_str().unwrap_or_default()
        );
        let id = new_id();
        app.post(
            &target,
            json!({"id": id, "text": "parallel-delegate", "recipient": lead["id"]}),
        )
        .await?;
        parallel.push((target, id));
    }
    until("four parallel first steps held", || async {
        Ok((gate.held() == 4).then_some(()))
    })
    .await?;
    gate.clear_hold();
    gate.release();
    for (target, id) in &parallel {
        ensure!(
            status(&app, target, id, "done").await?["reply"] == "parent:child-result",
            "parallel reply"
        );
    }
    ensure!(tasks("parallel-delegate") >= 4, "parallel first steps");
    gate.hold_when(|body| {
        body["messages"]
            .as_array()
            .and_then(|m| m.iter().rev().find(|m| m["role"] == "user"))
            .and_then(|m| m["content"].as_str())
            == Some("saved-child")
    });
    let saved = new_id();
    app.post(
        &path,
        json!({"id": saved, "text": "save-result", "recipient": a}),
    )
    .await?;
    until("saved child held", || async {
        Ok((gate.held() > 0).then_some(()))
    })
    .await?;
    gate.clear_hold();
    let alpha_path = format!("/api/agents/{a}/turns");
    let blocked = new_id();
    app.post(&alpha_path, json!({"id": blocked, "text": "block-parent"}))
        .await?;
    status(&app, &alpha_path, &blocked, "running").await?;
    gate.release();
    let rows = app.get(&path).await?;
    let saved_child = row_of(&rows, |row| row["parent"] == saved.as_str()).ok_or("saved child")?;
    status(
        &app,
        &path,
        saved_child["id"].as_str().unwrap_or_default(),
        "done",
    )
    .await?;
    ensure!(
        status(&app, &path, &saved, "queued").await?["waiting_for"].is_null(),
        "saved parent waits"
    );
    app.restart().await?;
    ensure!(
        status(&app, &path, &saved, "done").await?["reply"] == "parent:child-result",
        "saved result after restart"
    );
    ensure!(
        tasks("saved-child") == 1,
        "saved child calls: {}",
        tasks("saved-child")
    );
    ensure!(
        tasks("save-result") == 2,
        "save-result calls: {}",
        tasks("save-result")
    );
    for i in 0..101 {
        app.post(
            &path,
            json!({"id": new_id(), "text": format!("later-{i}"), "recipient": b}),
        )
        .await?;
    }
    let latest = app.get(&path).await?;
    ensure!(
        latest.as_array().map(Vec::len) == Some(100),
        "latest page: {}",
        latest.as_array().map_or(0, Vec::len)
    );
    ensure!(
        row_of(&latest, |row| row["id"] == parent_id.as_str()).is_none(),
        "old parent in the latest page"
    );
    let family = app.get(&format!("{path}?focus={parent_id}")).await?;
    ensure!(
        family.as_array().map(Vec::len) == Some(2),
        "family: {family}"
    );
    ensure!(family[0]["id"] == parent_id.as_str(), "family root");
    ensure!(family[1]["parent"] == parent_id.as_str(), "family child");
    ensure!(
        app.get(&format!(
            "{path}?focus={}",
            family[1]["id"].as_str().unwrap_or_default()
        ))
        .await?[0]["id"]
            == parent_id.as_str(),
        "focus on child"
    );
    ensure!(
        app.call(
            minarbot_testkit::http::Method::GET,
            &format!("{path}?focus={}", parallel[0].1),
            None
        )
        .await?
        .status
            == 404,
        "foreign focus"
    );
    Ok(())
}
