//! Caller interruption of teammate work (port of
//! `app/agent-interrupt.test.ts`): `interrupt_agent` cancels only the caller's
//! own deliveries, journals a notice and leaves other work running.
//!
//! The caller's requests and control turns run in side threads: a harness
//! parent holds its conversation while `send_message` waits for replies (M1),
//! so turns on one conversation would run one after another.

use crate::sa_support::{lock, offers, side_thread, tool_json, turn_tool_results, Shared};
use minarbot_testkit::{
    gate::ModelGate,
    model::{Reply, ScriptedModel},
    prelude::*,
    Server,
};
use std::sync::Arc;

#[derive(Default)]
struct Script {
    recipient: String,
    caller: String,
    nested: String,
    hold: bool,
    fail_interrupt_reply: bool,
    inject_failure: bool,
    violations: Vec<String>,
}

/// Text of the last user message of a raw model request body.
fn last_user(body: &Value) -> String {
    let content = body["messages"]
        .as_array()
        .and_then(|m| m.iter().rev().find(|m| m["role"] == "user"))
        .map(|m| m["content"].clone())
        .unwrap_or(Value::Null);
    match content {
        Value::String(text) => text,
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn messages(row: &Value) -> Vec<Value> {
    row["agentMessages"].as_array().cloned().unwrap_or_default()
}

fn first_message(row: &Value) -> Value {
    messages(row).first().cloned().unwrap_or(Value::Null)
}

fn reply(row: &Value) -> Result<Value> {
    Ok(serde_json::from_str(
        row["reply"].as_str().unwrap_or("null"),
    )?)
}

async fn send(app: &Server, path: &str, text: &str, extra: Value) -> Result<String> {
    let id = new_id();
    let mut body = json!({"id": id, "text": text});
    if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        body.extend(extra.clone());
    }
    app.post_expect(path, body, 202).await?;
    Ok(id)
}

/// Send `text` from a new side thread of `agent`; returns (turns path, id).
async fn own(app: &Server, agent: &str, text: &str) -> Result<(String, String)> {
    let thread = side_thread(app, agent, text).await?;
    let path = format!(
        "/api/threads/{}/turns",
        thread["id"].as_str().unwrap_or_default()
    );
    let id = send(app, &path, text, json!({})).await?;
    Ok((path, id))
}

async fn until(
    app: &Server,
    path: &str,
    id: &str,
    check: impl Fn(&Value) -> bool,
) -> Result<Value> {
    app.wait_row(path, id, check).await
}

fn status(row: &Value, status: &str) -> bool {
    row["status"] == status
}

#[tokio::test]
async fn caller_interruption_cancels_only_owned_deliveries_journals_notice_and_preserves_other_work(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    let gate = ModelGate::start(&model).await?;
    let state: Shared<Script> = Arc::new(std::sync::Mutex::new(Script {
        hold: true,
        fail_interrupt_reply: true,
        ..Script::default()
    }));
    let script = state.clone();
    model.respond(move |request| {
        let Ok(mut state) = script.lock() else { return Reply::status(500, json!({})) };
        let prompt = request.last_user_text();
        let results: Vec<Value> = turn_tool_results(request).iter().map(tool_json).collect();
        let interrupt = |recipient: &str, reason: &str| {
            Reply::tool_call(&new_id(), "interrupt_agent", json!({"recipient": recipient, "reason": reason}))
        };
        match request.model() {
            "gamma" if prompt == "NESTED_CHILD" => return Reply::text("Nested result"),
            "beta" => {
                if prompt == "NESTED_TASK" && results.is_empty() {
                    return Reply::tool_call(&new_id(), "send_message",
                        json!({"recipients": [state.nested], "text": "NESTED_CHILD", "expects_reply": true}));
                }
                if prompt == "ANCESTOR_TASK" {
                    return match results.last() {
                        None => interrupt(&state.caller.clone(), "Cannot stop requester"),
                        Some(last) => Reply::text(last.to_string()),
                    };
                }
                if prompt.starts_with("The work I asked for") {
                    if offers(request, "send_message") || offers(request, "interrupt_agent") {
                        state.violations.push("notice offered messaging tools".into());
                    }
                    return Reply::text("Notice recorded");
                }
                return Reply::text("Work finished");
            }
            _ => {}
        }
        let recipient = state.recipient.clone();
        match results.last() {
            None if prompt == "inspect" => Reply::tool_call(&new_id(), "list_agents", json!({})),
            None if prompt == "stop" => interrupt(
                &recipient,
                if state.inject_failure { "fixture failure check" } else { "User changed priorities" },
            ),
            None if prompt == "self" => interrupt(&state.caller.clone(), "Invalid target"),
            None => Reply::tool_call(&new_id(), "send_message",
                json!({"recipients": [recipient], "text": prompt, "expects_reply": true})),
            Some(last) if last["status"] == "interrupted" && state.fail_interrupt_reply => {
                state.fail_interrupt_reply = false;
                Reply::status(503, json!({}))
            }
            Some(last) => Reply::text(last.to_string()),
        }
    });
    let holds = state.clone();
    gate.hold_when(move |body| {
        if !holds.lock().map(|state| state.hold).unwrap_or(false) {
            return false;
        }
        let prompt = last_user(body);
        match body["model"].as_str().unwrap_or_default() {
            "gamma" => prompt == "NESTED_CHILD",
            "beta" => {
                !["NESTED_TASK", "ANCESTOR_TASK"].contains(&prompt.as_str())
                    && !prompt.starts_with("The work I asked for")
            }
            _ => false,
        }
    });
    let unhold = || -> Result<()> {
        lock(&state)?.hold = false;
        gate.release();
        Ok(())
    };
    let root = TestRoot::new()?;
    // M1: every waiting parent keeps its slot, so the Node suite's four slots
    // cannot hold its six concurrent requests.
    let options = ServerConfig::new().gateway(gate.gateway()).concurrency(10);
    let mut app = root.start(options.clone()).await?;
    app.edit_database(|db| {
        db.execute_batch("CREATE TRIGGER reject_interrupt_notice BEFORE INSERT ON turns WHEN NEW.text LIKE 'The work I asked for%fixture failure check%' BEGIN SELECT RAISE(ABORT,'fixture notice failure'); END")
    })
    .await?;
    let agent = |name: &str, model: &str, instructions: &str| json!({"runtime": "api", "id": new_id(), "name": name, "model": model, "instructions": instructions});
    // M1: Alpha's waiting side threads stay running, so it needs more than
    // the default two threads.
    let mut alpha_body = agent("Alpha", "alpha", "Coordinate");
    alpha_body["threadLimit"] = json!(8);
    let alpha = app.post_expect("/api/agents", alpha_body, 201).await?;
    let mut beta_body = agent("Beta", "beta", "Work");
    beta_body["threadLimit"] = json!(1);
    let beta = app.post_expect("/api/agents", beta_body, 201).await?;
    let gamma = app
        .post_expect("/api/agents", agent("Gamma", "gamma", "Coordinate"), 201)
        .await?;
    let (al, be, ga) = (
        alpha["id"].as_str().unwrap_or_default().to_owned(),
        beta["id"].as_str().unwrap_or_default().to_owned(),
        gamma["id"].as_str().unwrap_or_default().to_owned(),
    );
    {
        let mut state = lock(&state)?;
        state.caller = al.clone();
        state.recipient = be.clone();
        state.nested = ga.clone();
    }
    // Alpha's waiting channel turn also holds its main conversation (M1), so
    // its control turns run in a side thread.
    let control = side_thread(&app, &al, "Control").await?;
    let a = format!(
        "/api/threads/{}/turns",
        control["id"].as_str().unwrap_or_default()
    );
    let b = format!("/api/agents/{be}/turns");
    let g = format!("/api/agents/{ga}/turns");
    let user = send(&app, &b, "USER_TASK", json!({})).await?;
    until(&app, &b, &user, |r| status(r, "running")).await?;
    let (first_path, first) = own(&app, &al, "FIRST_OWN").await?;
    until(&app, &first_path, &first, |r| {
        messages(r).iter().any(|m| m["status"] == "running")
    })
    .await?;
    let (second_path, second) = own(&app, &al, "SECOND_OWN").await?;
    until(&app, &second_path, &second, |r| {
        messages(r).iter().any(|m| m["status"] == "queued")
    })
    .await?;
    let foreign = send(&app, &g, "FOREIGN_TASK", json!({})).await?;
    until(&app, &g, &foreign, |r| messages(r).len() == 1).await?;
    let channel = app
        .post_expect(
            "/api/channels",
            json!({"id": new_id(), "name": "Shared", "instructions": "Shared work", "members": [al, be], "lead": al}),
            201,
        )
        .await?;
    let c = format!(
        "/api/channels/{}/turns",
        channel["id"].as_str().unwrap_or_default()
    );
    let channel_task = send(&app, &c, "CHANNEL_TASK", json!({"recipient": al})).await?;
    until(&app, &c, &channel_task, |r| messages(r).len() == 1).await?;
    let inspect = send(&app, &a, "inspect", json!({})).await?;
    let inspected = until(&app, &a, &inspect, |r| status(r, "done")).await?;
    let listed = reply(&inspected)?["agents"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == be.as_str()).cloned())
        .ok_or("beta not listed")?;
    ensure!(listed["status"] == "working", "beta listing: {listed}");
    ensure!(listed["queuedMessages"] == 2, "beta listing: {listed}");
    let stop = send(&app, &a, "stop", json!({})).await?;
    let failed = until(&app, &a, &stop, |r| status(r, "failed")).await?;
    ensure!(messages(&failed).len() == 1, "stop messages: {failed}");
    let notice_id = first_message(&failed)["id"].clone();
    let first_done = until(&app, &first_path, &first, |r| status(r, "done")).await?;
    let second_done = until(&app, &second_path, &second, |r| status(r, "done")).await?;
    ensure!(
        first_message(&first_done)["status"] == "cancelled",
        "first: {first_done}"
    );
    ensure!(
        first_message(&second_done)["status"] == "cancelled",
        "second: {second_done}"
    );
    ensure!(
        app.row(&b, &user).await?.ok_or("user task")?["status"] == "running",
        "user task stopped"
    );
    let foreign_row = app.row(&g, &foreign).await?.ok_or("foreign")?;
    ensure!(
        ["queued", "running"]
            .contains(&first_message(&foreign_row)["status"].as_str().unwrap_or("")),
        "foreign work: {foreign_row}"
    );
    let channel_row = app.row(&c, &channel_task).await?.ok_or("channel task")?;
    ensure!(
        first_message(&channel_row)["status"] == "queued",
        "channel work: {channel_row}"
    );
    let beta_saw = |text: &str| {
        model
            .requests()
            .iter()
            .filter(|r| r.model() == "beta" && r.last_user_text().starts_with(text))
            .count()
    };
    ensure!(beta_saw("SECOND_OWN") == 0, "cancelled queued work ran");
    app.post_expect(&a, json!({"id": stop, "text": "stop"}), 202)
        .await?;
    unhold()?;
    until(&app, &g, &foreign, |r| status(r, "done")).await?;
    until(&app, &c, &channel_task, |r| status(r, "done")).await?;
    until(&app, &a, &stop, |r| {
        messages(r).iter().all(|m| m["status"] == "done")
    })
    .await?;
    app.restart().await?;
    app.post(
        &format!("{a}/{stop}/resume"),
        json!({"id": new_id(), "revision": failed["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    let retried = until(&app, &a, &stop, |r| status(r, "done")).await?;
    ensure!(
        first_message(&retried)["id"] == notice_id,
        "notice resent: {retried}"
    );
    let receipt = reply(&retried)?;
    ensure!(receipt["status"] == "interrupted", "receipt: {receipt}");
    ensure!(
        receipt["cancelled"].as_array().map(Vec::len) == Some(2),
        "receipt cancelled: {receipt}"
    );
    ensure!(
        beta_saw("The work I asked for") == 1,
        "notices: {}",
        beta_saw("The work I asked for")
    );
    ensure!(
        app.row(&b, &user).await?.ok_or("user task")?["status"] == "done",
        "user task after release"
    );
    // H1: refusals reach the model, which answers with the error.
    let no_own = send(&app, &a, "stop", json!({})).await?;
    let no_own = until(&app, &a, &no_own, |r| status(r, "done")).await?;
    ensure!(
        no_own["reply"]
            .as_str()
            .unwrap_or_default()
            .contains("no active work"),
        "no own work: {no_own}"
    );
    let own_stop = send(&app, &a, "self", json!({})).await?;
    let own_stop = until(&app, &a, &own_stop, |r| status(r, "done")).await?;
    ensure!(
        own_stop["reply"]
            .as_str()
            .unwrap_or_default()
            .contains("You can only stop"),
        "self stop: {own_stop}"
    );
    let forbidden = send(&app, &c, "stop", json!({"recipient": al})).await?;
    let forbidden = until(&app, &c, &forbidden, |r| status(r, "done")).await?;
    ensure!(
        forbidden["reply"]
            .as_str()
            .unwrap_or_default()
            .contains("error"),
        "channel stop: {forbidden}"
    );
    let channel_call = model
        .requests()
        .into_iter()
        .find(|r| {
            r.model() == "alpha" && r.system().contains("Shared") && r.last_user_text() == "stop"
        })
        .ok_or("channel stop call")?;
    ensure!(
        !offers(&channel_call, "interrupt_agent"),
        "interrupt_agent offered in a channel"
    );
    lock(&state)?.hold = true;
    let (rollback_path, rollback) = own(&app, &al, "ROLLBACK_WORK").await?;
    until(&app, &rollback_path, &rollback, |r| {
        messages(r).iter().any(|m| m["status"] == "running")
    })
    .await?;
    lock(&state)?.inject_failure = true;
    let rejected = send(&app, &a, "stop", json!({})).await?;
    let rejected_row = until(&app, &a, &rejected, |r| {
        status(r, "done") || status(r, "failed")
    })
    .await?;
    let still_running = app
        .row(&rollback_path, &rollback)
        .await?
        .ok_or("rollback")?;
    ensure!(
        first_message(&still_running)["status"] == "running",
        "rolled back interrupt cancelled work: {still_running}"
    );
    ensure!(
        messages(&rejected_row).is_empty(),
        "rolled back notice: {rejected_row}"
    );
    lock(&state)?.inject_failure = false;
    let child = first_message(&still_running);
    let child_path = format!(
        "/api/threads/{}/turns",
        child["thread"].as_str().unwrap_or_default()
    );
    let child_id = child["id"].as_str().unwrap_or_default().to_owned();
    let live_child = app.get(&child_path).await?[0].clone();
    app.post(
        &format!("{child_path}/{child_id}/pause"),
        json!({"id": new_id(), "revision": live_child["revision"]}),
    )
    .await?;
    until(&app, &child_path, &child_id, |r| r["paused"] == 1).await?;
    unhold()?;
    let stop_paused = send(&app, &a, "stop", json!({})).await?;
    let stopped = until(&app, &a, &stop_paused, |r| status(r, "done")).await?;
    ensure!(
        reply(&stopped)?["cancelled"]
            .as_array()
            .is_some_and(|ids| ids.contains(&json!(child_id))),
        "paused child not cancelled: {stopped}"
    );
    let rolled = until(&app, &rollback_path, &rollback, |r| status(r, "done")).await?;
    ensure!(
        first_message(&rolled)["status"] == "cancelled",
        "rollback: {rolled}"
    );
    let (ancestor_path, ancestor) = own(&app, &al, "ANCESTOR_TASK").await?;
    let ancestor_row = until(&app, &ancestor_path, &ancestor, |r| status(r, "done")).await?;
    // H1: the refusal reaches the delivery's model, which answers with it.
    let ancestor_message = first_message(&ancestor_row);
    ensure!(
        ancestor_message["status"] == "done"
            && ancestor_message["reply"]
                .as_str()
                .unwrap_or_default()
                .contains("You can only stop"),
        "ancestor: {ancestor_message}"
    );
    lock(&state)?.hold = true;
    let (nested_path, nested) = own(&app, &al, "NESTED_TASK").await?;
    let nesting = until(&app, &nested_path, &nested, |r| messages(r).len() == 1).await?;
    let nested_beta = first_message(&nesting);
    let beta_path = format!(
        "/api/threads/{}/turns",
        nested_beta["thread"].as_str().unwrap_or_default()
    );
    let nested_beta_id = nested_beta["id"].as_str().unwrap_or_default().to_owned();
    let nested_waiting = until(&app, &beta_path, &nested_beta_id, |r| {
        messages(r).iter().any(|m| m["status"] == "running")
    })
    .await?;
    let nested_gamma = first_message(&nested_waiting);
    let stop_tree = send(&app, &a, "stop", json!({})).await?;
    let tree = until(&app, &a, &stop_tree, |r| status(r, "done")).await?;
    let mut cancelled: Vec<String> = reply(&tree)?["cancelled"]
        .as_array()
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    cancelled.sort();
    let mut expected = vec![
        nested_beta_id.clone(),
        nested_gamma["id"].as_str().unwrap_or_default().to_owned(),
    ];
    expected.sort();
    ensure!(cancelled == expected, "tree: {cancelled:?} vs {expected:?}");
    let gamma_turns = app
        .get(&format!(
            "/api/threads/{}/turns",
            nested_gamma["thread"].as_str().unwrap_or_default()
        ))
        .await?;
    ensure!(
        gamma_turns[0]["status"] == "cancelled",
        "nested gamma: {}",
        gamma_turns[0]
    );
    unhold()?;
    let violations = lock(&state)?.violations.clone();
    ensure!(violations.is_empty(), "model checks: {violations:?}");
    Ok(())
}
