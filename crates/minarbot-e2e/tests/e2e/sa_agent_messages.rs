//! Teammate messages: grouped requests, isolated context, durable receipts
//! and bounded delivery (port of `app/agent-messages.test.ts`).

use crate::sa_support::{lock, offers, tool_json, turn_tool_results, Shared};
use minarbot_testkit::{
    gate::ModelGate,
    model::{Reply, ScriptedModel},
    prelude::*,
    Server,
};
use std::sync::Arc;

#[derive(Default)]
struct Script {
    mode: &'static str,
    sender: String,
    targets: Vec<String>,
    fail_continuation: bool,
    violations: Vec<String>,
    loop_refusals: usize,
}

async fn until(
    app: &Server,
    path: &str,
    id: &str,
    check: impl Fn(&Value) -> bool,
) -> Result<Value> {
    app.wait_row(path, id, check).await
}

fn messages(row: &Value) -> Vec<Value> {
    row["agentMessages"].as_array().cloned().unwrap_or_default()
}

#[tokio::test]
async fn teammate_messages_correlate_groups_isolate_context_persist_receipts_and_bound_delivery(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    let gate = ModelGate::start(&model).await?;
    let state: Shared<Script> = Arc::new(std::sync::Mutex::new(Script {
        mode: "group",
        ..Script::default()
    }));
    let script = state.clone();
    model.respond(move |request| {
        let Ok(mut state) = script.lock() else { return Reply::status(500, json!({})) };
        let results: Vec<Value> = turn_tool_results(request).iter().map(tool_json).collect();
        let prompt = request.last_user_text();
        let mode = state.mode;
        if request.model() == "alpha" {
            if prompt.contains("user bypass") {
                return Reply::text("independent reply");
            }
            match results.last() {
                None => return Reply::tool_call(&new_id(), "list_agents", json!({})),
                Some(last) if last["agents"].is_array() => {
                    let agents = last["agents"].as_array().cloned().unwrap_or_default();
                    if agents.iter().any(|agent| agent["id"] == state.sender.as_str()) {
                        state.violations.push("sender listed".into());
                    }
                    let beta = agents.iter().find(|agent| agent["name"] == "Beta").cloned().unwrap_or(Value::Null);
                    if beta["role"] != "Personal agent" || beta["description"] != "Help." {
                        state.violations.push(format!("beta listing: {beta}"));
                    }
                    let recipients = match mode {
                        "self" => json!([state.sender]),
                        "duplicate" => json!([state.targets[0], state.targets[0]]),
                        _ => json!(state.targets),
                    };
                    return Reply::tool_call(
                        &new_id(),
                        "send_message",
                        json!({"recipients": recipients, "text": if mode == "loop" { "loop" } else { "shared task" },
                            "expects_reply": mode != "notify"}),
                    );
                }
                Some(last) => {
                    if std::mem::take(&mut state.fail_continuation) {
                        return Reply::status(503, json!({}));
                    }
                    return Reply::text(last.to_string());
                }
            }
        }
        if request.system().contains("Message from teammate") && request.mentions("PRIVATE_MAIN_HISTORY") {
            state.violations.push("main history in a delivery".into());
        }
        match mode {
            "loop" => {
                state.loop_refusals += results.iter().filter(|r| r["error"].is_string()).count();
                Reply::tool_call(&new_id(), "send_message", json!({"recipients": [state.sender], "text": "ack", "expects_reply": true}))
            }
            "channel" => {
                if request.mentions("PRIVATE_MEMORY") {
                    state.violations.push("private memory in channel delivery".into());
                }
                if !request.system().contains("SHARED_MEMORY") {
                    state.violations.push("shared memory missing".into());
                }
                for tool in ["list_routines", "read_routine", "set_routine_flow", "list_skills", "read_skill"] {
                    if offers(request, tool) {
                        state.violations.push(format!("{tool} offered in a channel delivery"));
                    }
                }
                match results.first() {
                    None => Reply::tool_call(&new_id(), "search_memory", json!({})),
                    Some(first) => {
                        let texts: Vec<Value> = first["items"].as_array().map(|items| items.iter().map(|i| i["text"].clone()).collect()).unwrap_or_default();
                        if texts != [json!("SHARED_MEMORY")] {
                            state.violations.push(format!("channel search: {texts:?}"));
                        }
                        Reply::text("channel scoped")
                    }
                }
            }
            _ => {
                if mode == "notify" && offers(request, "send_message") {
                    state.violations.push("send_message offered to a no-reply delivery".into());
                }
                Reply::text(if mode == "long" { "x".repeat(20000) } else { format!("{} result", request.model()) })
            }
        }
    });
    let hold_modes = state.clone();
    gate.hold_when(move |body| {
        let mode = hold_modes.lock().map(|state| state.mode).unwrap_or("");
        let model = body["model"].as_str().unwrap_or_default();
        (model == "gamma" && ["hold", "restart", "cancel", "pause"].contains(&mode))
            || (mode == "archive" && model != "alpha")
    });
    let calls_for = |model: &ScriptedModel, name: &str| {
        model
            .requests()
            .iter()
            .filter(|r| r.model() == name)
            .count()
    };
    let root = TestRoot::new()?;
    let options = ServerConfig::new().gateway(gate.gateway()).concurrency(2);
    let mut app = root.start(options.clone()).await?;
    let agent = |name: &str, model: &str, role: Option<&str>| {
        let mut body = json!({"runtime": "api", "id": new_id(), "name": name, "model": model, "instructions": "Help."});
        if let Some(role) = role {
            body["role"] = json!(role);
            // Teammates need at most one tool step; a small budget ends the
            // refused reply loop quickly (H1).
            body["toolBudget"] = json!(10);
        }
        body
    };
    let alpha = app
        .post_expect("/api/agents", agent("Alpha", "alpha", None), 201)
        .await?;
    let beta = app
        .post_expect(
            "/api/agents",
            agent("Beta", "beta", Some("Personal agent")),
            201,
        )
        .await?;
    let gamma = app
        .post_expect(
            "/api/agents",
            agent("Gamma", "gamma", Some("Research agent")),
            201,
        )
        .await?;
    let (a, b, g) = (
        alpha["id"].as_str().unwrap_or_default().to_owned(),
        beta["id"].as_str().unwrap_or_default().to_owned(),
        gamma["id"].as_str().unwrap_or_default().to_owned(),
    );
    {
        let mut state = lock(&state)?;
        state.sender = a.clone();
        state.targets = vec![b.clone(), g.clone()];
    }
    let path = format!("/api/agents/{a}/turns");
    let beta_path = format!("/api/agents/{b}/turns");
    let prior = new_id();
    app.post_expect(
        &beta_path,
        json!({"id": prior, "text": "PRIVATE_MAIN_HISTORY"}),
        202,
    )
    .await?;
    until(&app, &beta_path, &prior, |row| row["status"] == "done").await?;
    lock(&state)?.mode = "hold";
    let request = json!({"id": new_id(), "text": "ask both"});
    let request_id = request["id"].as_str().unwrap_or_default().to_owned();
    app.post_expect(&path, request.clone(), 202).await?;
    let waiting = until(&app, &path, &request_id, |row| {
        let items = messages(row);
        items.len() == 2 && items.iter().any(|item| item["status"] == "done")
    })
    .await?;
    ensure!(waiting["reply"].is_null(), "reply while waiting: {waiting}");
    let beta_message = messages(&waiting)
        .into_iter()
        .find(|m| m["agent"] == b.as_str())
        .ok_or("beta message")?;
    let threads = app.get(&format!("/api/agents/{b}/threads")).await?;
    let thread = threads
        .as_array()
        .and_then(|t| {
            t.iter()
                .find(|t| t["id"] == beta_message["thread"])
                .cloned()
        })
        .ok_or("beta thread")?;
    ensure!(
        thread["name"] == "Alpha asked: shared task",
        "thread name: {}",
        thread["name"]
    );
    ensure!(
        calls_for(&model, "alpha") == 2,
        "alpha calls: {}",
        calls_for(&model, "alpha")
    );
    ensure!(
        app.get(&beta_path).await?.as_array().map(Vec::len) == Some(1),
        "delivery in beta main history"
    );
    // The harness keeps the parent run open while `send_message` waits, so a
    // later user message on the same thread queues behind it instead of
    // consuming the earlier reply (no `delivered_earlier`, see ledger).
    let bypass = new_id();
    app.post_expect(&path, json!({"id": bypass, "text": "user bypass"}), 202)
        .await?;
    until_held(&gate).await?;
    ensure!(
        app.row(&path, &bypass).await?.ok_or("bypass")?["status"] == "queued",
        "bypass ran while the parent waited"
    );
    gate.release();
    let done = until(&app, &path, &request_id, |row| row["status"] == "done").await?;
    let result: Value = serde_json::from_str(done["reply"].as_str().unwrap_or("null"))?;
    ensure!(result["status"] == "completed", "group result: {result}");
    let replies: Vec<Value> = result["replies"]
        .as_array()
        .map(|rows| rows.iter().map(|row| row["reply"].clone()).collect())
        .unwrap_or_default();
    ensure!(
        replies == [json!("beta result"), json!("gamma result")],
        "group replies: {replies:?}"
    );
    ensure!(
        result["delivered_earlier"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "delivered earlier: {result}"
    );
    let bypassed = until(&app, &path, &bypass, |row| row["status"] == "done").await?;
    ensure!(
        bypassed["reply"] == "independent reply",
        "bypass: {bypassed}"
    );
    ensure!(
        bypassed["receivedAgentReplies"]
            .as_array()
            .is_none_or(Vec::is_empty),
        "bypass received replies: {bypassed}"
    );
    ensure!(
        messages(&done).len() == 2,
        "messages: {}",
        messages(&done).len()
    );
    let before = model.calls();
    app.post_expect(&path, request.clone(), 202).await?;
    app.restart().await?;
    ensure!(
        messages(&app.row(&path, &request_id).await?.ok_or("request")?).len() == 2,
        "receipt after restart"
    );
    ensure!(model.calls() == before, "replay called the model");
    for invalid in ["self", "duplicate", "loop"] {
        lock(&state)?.mode = invalid;
        let id = new_id();
        app.post_expect(&path, json!({"id": id, "text": invalid}), 202)
            .await?;
        let row = until(&app, &path, &id, |row| {
            row["status"] == "done" || row["status"] == "failed"
        })
        .await?;
        if invalid == "loop" {
            ensure!(row["status"] == "done", "loop: {row}");
            // H1: each refused reply-to-sender reaches the model, which
            // retries until the tool budget stops the delivery.
            ensure!(
                messages(&row).iter().all(|m| m["status"] == "failed"),
                "loop messages: {row}"
            );
            ensure!(lock(&state)?.loop_refusals > 0, "loop never refused");
        } else {
            // H1: the refused call reaches the model as an error result.
            ensure!(messages(&row).is_empty(), "{invalid} messages: {row}");
            ensure!(
                row["reply"].as_str().unwrap_or_default().contains("error"),
                "{invalid} refusal: {row}"
            );
        }
    }
    {
        let mut state = lock(&state)?;
        state.mode = "loop";
        state.fail_continuation = true;
    }
    let retry = new_id();
    app.post_expect(
        &path,
        json!({"id": retry, "text": "failed parent continuation"}),
        202,
    )
    .await?;
    let failed = until(&app, &path, &retry, |row| row["status"] == "failed").await?;
    ensure!(
        messages(&failed)
            .iter()
            .all(|m| m["status"] == "failed" && m["settled"] == 1),
        "failed continuation messages: {failed}"
    );
    lock(&state)?.mode = "group";
    app.post(
        &format!("{path}/{retry}/resume"),
        json!({"id": new_id(), "revision": failed["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    let resumed = until(&app, &path, &retry, |row| row["status"] == "done").await?;
    ensure!(
        ids(&resumed) == ids(&failed),
        "resume resent messages: {:?} vs {:?}",
        ids(&resumed),
        ids(&failed)
    );
    lock(&state)?.mode = "notify";
    let notification = new_id();
    app.post_expect(&path, json!({"id": notification, "text": "FYI"}), 202)
        .await?;
    let notified = until(&app, &path, &notification, |row| {
        row["status"] == "done" && messages(row).iter().all(|m| m["status"] == "done")
    })
    .await?;
    let receipt: Value = serde_json::from_str(notified["reply"].as_str().unwrap_or("null"))?;
    ensure!(receipt["status"] == "sent", "notify receipt: {receipt}");
    lock(&state)?.mode = "restart";
    let restart = new_id();
    app.post_expect(&path, json!({"id": restart, "text": "restart group"}), 202)
        .await?;
    until_held(&gate).await?;
    until(&app, &path, &restart, |row| gamma_running(row, &g)).await?;
    let count = calls_for(&model, "gamma");
    lock(&state)?.mode = "group";
    app.shutdown().await?;
    gate.release();
    app.start_stopped(options.clone()).await?;
    let recovered = until(&app, &path, &restart, |row| row["status"] == "done").await?;
    let interrupted = messages(&recovered)
        .into_iter()
        .find(|m| m["agent"] == g.as_str())
        .ok_or("gamma message")?;
    ensure!(
        interrupted["status"] == "interrupted",
        "gamma after restart: {interrupted}"
    );
    ensure!(
        calls_for(&model, "gamma") == count,
        "gamma replayed after restart"
    );
    let child_path = format!(
        "/api/threads/{}/turns",
        interrupted["thread"].as_str().unwrap_or_default()
    );
    let child = app.get(&child_path).await?[0].clone();
    app.post_expect(
        &format!(
            "{child_path}/{}/resume",
            child["id"].as_str().unwrap_or_default()
        ),
        json!({"id": new_id(), "revision": child["revision"], "acknowledgeUncertain": true}),
        409,
    )
    .await?;
    lock(&state)?.mode = "cancel";
    let cancel = new_id();
    app.post_expect(&path, json!({"id": cancel, "text": "cancel group"}), 202)
        .await?;
    until_held(&gate).await?;
    until(&app, &path, &cancel, |row| gamma_running(row, &g)).await?;
    app.post(&format!("{path}/{cancel}/cancel"), json!({"id": new_id()}))
        .await?;
    let cancelled = until(&app, &path, &cancel, |row| {
        row["status"] == "cancelled"
            && messages(row)
                .iter()
                .all(|m| m["status"] == "done" || m["status"] == "cancelled")
    })
    .await?;
    ensure!(cancelled["status"] == "cancelled", "cancel: {cancelled}");
    gate.release();
    lock(&state)?.mode = "pause";
    let pause = new_id();
    app.post_expect(&path, json!({"id": pause, "text": "pause group"}), 202)
        .await?;
    until_held(&gate).await?;
    let pausing = until(&app, &path, &pause, |row| gamma_running(row, &g)).await?;
    app.post(
        &format!("{path}/{pause}/pause"),
        json!({"id": new_id(), "revision": pausing["revision"]}),
    )
    .await?;
    lock(&state)?.mode = "group";
    gate.release();
    app.shutdown().await?;
    app.start_stopped(options.clone().concurrency(1)).await?;
    let paused = app.row(&path, &pause).await?.ok_or("paused turn")?;
    ensure!(paused["paused"] == 1, "paused: {paused}");
    ensure!(
        messages(&paused)
            .iter()
            .any(|m| m["paused"] == 1 && m["settled"] != 1),
        "paused messages: {paused}"
    );
    app.post(
        &format!("{path}/{pause}/resume"),
        json!({"id": new_id(), "revision": paused["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    let unpaused = until(&app, &path, &pause, |row| row["status"] == "done").await?;
    ensure!(
        ids(&unpaused) == ids(&paused),
        "pause resume resent messages"
    );
    // A harness parent holds its slot while `send_message` waits, so the
    // remaining group requests need a second slot for the delivery.
    app.restart_with(options.clone()).await?;
    let room = app
        .post_expect(
            "/api/channels",
            json!({"id": new_id(), "name": "Restricted", "instructions": "Alpha and Beta only", "members": [a, b], "lead": a}),
            201,
        )
        .await?;
    let room_id = room["id"].as_str().unwrap_or_default().to_owned();
    let channel_path = format!("/api/channels/{room_id}/turns");
    lock(&state)?.mode = "group";
    lock(&state)?.targets = vec![g.clone()];
    let outside = new_id();
    app.post_expect(
        &channel_path,
        json!({"id": outside, "text": "outside", "recipient": a}),
        202,
    )
    .await?;
    let rejected = until(&app, &channel_path, &outside, |row| {
        row["status"] == "done" || row["status"] == "failed"
    })
    .await?;
    ensure!(
        messages(&rejected).is_empty(),
        "outside messages: {rejected}"
    );
    // H1: the refusal reaches the model, which repeats it as its answer.
    ensure!(
        rejected["reply"]
            .as_str()
            .unwrap_or_default()
            .contains("Recipient"),
        "outside refusal: {rejected}"
    );
    let memory = |text: &str| json!({"id": new_id(), "entry": new_id(), "revision": 0, "text": text, "essential": true, "remove": false});
    app.post(&format!("/api/agents/{b}/memory"), memory("PRIVATE_MEMORY"))
        .await?;
    app.post(
        &format!("/api/channels/{room_id}/memory"),
        memory("SHARED_MEMORY"),
    )
    .await?;
    {
        let mut state = lock(&state)?;
        state.mode = "channel";
        state.targets = vec![b.clone()];
    }
    let scoped = new_id();
    app.post_expect(
        &channel_path,
        json!({"id": scoped, "text": "channel context", "recipient": a}),
        202,
    )
    .await?;
    let scoped_result = until(&app, &channel_path, &scoped, |row| row["status"] == "done").await?;
    ensure!(
        messages(&scoped_result)[0]["reply"] == "channel scoped",
        "channel reply: {scoped_result}"
    );
    let delivery = app
        .get(&format!(
            "/api/threads/{}/turns",
            messages(&scoped_result)[0]["thread"]
                .as_str()
                .unwrap_or_default()
        ))
        .await?;
    ensure!(
        delivery[0]["messageOrigin"]["channel"] == room_id.as_str(),
        "delivery origin: {}",
        delivery[0]["messageOrigin"]
    );
    lock(&state)?.mode = "long";
    let long = new_id();
    app.post_expect(&path, json!({"id": long, "text": "long result"}), 202)
        .await?;
    let long_result = until(&app, &path, &long, |row| row["status"] == "done").await?;
    let reply: Value = serde_json::from_str(long_result["reply"].as_str().unwrap_or("null"))?;
    ensure!(
        reply["replies"][0]["reply"]
            .as_str()
            .map(|r| r.chars().count())
            == Some(16000),
        "bounded reply"
    );
    ensure!(
        reply["replies"][0]["truncated"] == true,
        "reply truncated flag"
    );
    ensure!(
        messages(&long_result)[0]["reply"]
            .as_str()
            .map(|r| r.chars().count())
            == Some(1000),
        "message preview"
    );
    ensure!(
        messages(&long_result)[0]["truncated"] == true,
        "message truncated flag"
    );
    lock(&state)?.mode = "archive";
    let archive = new_id();
    app.post_expect(
        &channel_path,
        json!({"id": archive, "text": "archive while waiting", "recipient": a}),
        202,
    )
    .await?;
    until_held(&gate).await?;
    until(&app, &channel_path, &archive, |row| {
        messages(row).iter().any(|m| m["status"] == "running")
    })
    .await?;
    app.post(
        &format!("/api/channels/{room_id}/edit"),
        json!({"id": new_id(), "revision": room["revision"], "name": room["name"], "instructions": room["instructions"],
            "members": [a, b], "lead": a, "archived": true}),
    )
    .await?;
    until(&app, &channel_path, &archive, |row| {
        row["status"] == "cancelled" && messages(row).iter().all(|m| m["status"] == "cancelled")
    })
    .await?;
    lock(&state)?.mode = "group";
    gate.release();
    let violations = lock(&state)?.violations.clone();
    ensure!(violations.is_empty(), "model checks: {violations:?}");
    Ok(())
}

fn ids(row: &Value) -> Vec<Value> {
    messages(row).iter().map(|m| m["id"].clone()).collect()
}

fn gamma_running(row: &Value, gamma: &str) -> bool {
    messages(row)
        .iter()
        .any(|m| m["agent"] == gamma && m["status"] == "running")
}

async fn until_held(gate: &ModelGate) -> Result<()> {
    minarbot_testkit::wait::until("held teammate reply", || async {
        Ok((gate.held() > 0).then_some(()))
    })
    .await
}
