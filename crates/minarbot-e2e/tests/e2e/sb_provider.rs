//! One explicit provider: answers, tools, resume, routing, delegation,
//! cancellation and usage (port of `app/provider.test.ts`).
//!
//! The Node case drove the removed native Anthropic client (`/v1/messages`
//! shape, `max_tokens`, `tool_choice`, catalog pagination, redirects, the
//! `overloaded` fallback to `backupModel`, family `anthropic` on usage rows).
//! Those are dropped with #87. The behaviour around them runs through the
//! provider instance: replies, a durable artifact tool, failures without
//! replay, a question that survives restart, an empty workflow output,
//! automatic routing, delegation and transfer with coordination, cancel of a
//! held request, and usage without the key or the provider's error text.

use crate::sb_support::{after_tool, id, reminder, settle};
use minarbot_testkit::{
    control::{Answer, ControlModel},
    model::ModelRequest,
    prelude::*,
    server::Gateway,
    Server,
};
use std::sync::{Arc, Mutex};

const KEY: &str = "fixture-anthropic";
const ROUTER: &str = "Choose exactly one channel member";

fn respond(mode: &str, recipient: &str, request: &ModelRequest) -> Answer {
    if request.system().starts_with(ROUTER) {
        let input: Value = serde_json::from_str(&request.last_text()).unwrap_or(Value::Null);
        return Answer::text(
            json!({"action": "assign", "recipient": input["members"][0]["id"]}).to_string(),
        );
    }
    if after_tool(request) || reminder(request) {
        return Answer::text("Native\nanswer");
    }
    match mode {
        "hold" => Answer::Defer,
        "malformed" => Answer::Raw("{".into()),
        "overloaded" => Answer::Status(529, json!({"error": "secret failure"})),
        "unknown" => Answer::tool("shell", json!({"command": "no"})),
        "question" => Answer::tool(
            "ask_user",
            json!({"question": "Continue?", "choices": ["Yes"]}),
        ),
        "artifact" => Answer::tool(
            "create_text_file",
            json!({"name": "native.txt", "text": "Native artifact"}),
        ),
        "delegate" | "transfer" if request.model() == "primary" => {
            let name = if mode == "delegate" {
                "delegate_task"
            } else {
                "transfer_task"
            };
            Answer::tool(
                name,
                json!({"recipient": recipient, "task": "Native subtask"}),
            )
        }
        _ => Answer::text("Native\nanswer"),
    }
}

async fn turn(app: &Server, path: &str, body: Value) -> Result<Value> {
    let id = body["id"].as_str().ok_or("id")?.to_owned();
    app.post_expect(path, body, 202).await?;
    settle(app, path, &id).await
}

#[tokio::test]
async fn native_anthropic_messages_catalog_tools_resume_cancellation_and_fallback_use_one_explicit_provider(
) -> Result<()> {
    let mode = Arc::new(Mutex::new(String::from("text")));
    let recipient = Arc::new(Mutex::new(String::new()));
    let model = ControlModel::start().await?;
    let shared = mode.clone();
    let other = recipient.clone();
    model.respond(move |request| {
        let mode = shared.lock().map(|mode| mode.clone()).unwrap_or_default();
        let other = other.lock().map(|id| id.clone()).unwrap_or_default();
        respond(&mode, &other, request)
    });
    model.catalog(["first", "second"]);
    let set = |value: &str| -> Result<()> {
        *mode.lock().map_err(|_| "mode")? = value.into();
        Ok(())
    };
    let root = TestRoot::new()?;
    let gateway = Gateway {
        api_key: Some(KEY.into()),
        max_tokens: Some(2048),
        ..model.gateway()
    };
    let config = ServerConfig::new().gateway(gateway);
    let mut app = root.start(config.clone()).await?;
    let catalog = app
        .post(
            "/api/provider-instances/codex-test-gateway/models",
            json!({"id": new_id(), "instanceId": "codex-test-gateway"}),
        )
        .await?;
    ensure!(
        catalog["models"] == json!(["first", "second"]),
        "catalog: {catalog}"
    );
    let a = id(&app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Native", "model": "primary",
                "backupModel": "backup", "instructions": "Use native tools"}),
        )
        .await?)?;
    let path = format!("/api/agents/{a}/turns");
    for scenario in ["text", "artifact", "unknown", "overloaded", "malformed"] {
        set(scenario)?;
        let start = model.calls();
        let body = json!({"id": new_id(), "text": scenario});
        let row = turn(&app, &path, body.clone()).await?;
        // A tool the turn does not know reaches the model as an MCP error;
        // provider failures fail the turn without a fallback model.
        let failed = ["overloaded", "malformed"].contains(&scenario);
        ensure!(
            row["status"] == if failed { "failed" } else { "done" },
            "{scenario}: {row}"
        );
        if row["status"] == "done" {
            ensure!(row["reply"] == "Native\nanswer", "{scenario} reply: {row}");
        }
        if scenario == "artifact" {
            ensure!(
                row["generatedFiles"].as_array().map(Vec::len) == Some(1),
                "artifact: {row}"
            );
            let requests = model.requests();
            let result = requests[start..]
                .iter()
                .find(|request| after_tool(request))
                .ok_or("artifact result request")?;
            let call = result.tool_calls().last().cloned().ok_or("artifact call")?;
            let answer = result
                .tool_results()
                .last()
                .cloned()
                .ok_or("artifact result")?;
            ensure!(
                answer["tool_call_id"] == call["id"],
                "result pairs with its call: {answer} {call}"
            );
        }
        if scenario == "overloaded" {
            let models: Vec<String> = model.requests()[start..]
                .iter()
                .map(|request| request.model().to_owned())
                .collect();
            ensure!(models == ["primary"], "no fallback model: {models:?}");
        }
        let count = model.calls();
        app.post_expect(&path, body, 202).await?;
        ensure!(model.calls() == count, "{scenario} receipt replayed");
    }
    set("question")?;
    let asked = new_id();
    app.post_expect(&path, json!({"id": asked, "text": "Ask"}), 202)
        .await?;
    let waiting = app
        .wait_row(&path, &asked, |row| row["paused"] == 1)
        .await?;
    app.restart().await?;
    set("text")?;
    app.post(
        &format!(
            "{path}/{asked}/questions/{}/answer",
            waiting["questions"][0]["id"].as_str().ok_or("question")?
        ),
        json!({"id": new_id(), "text": "Yes"}),
    )
    .await?;
    ensure!(
        settle(&app, &path, &asked).await?["status"] == "done",
        "answered question"
    );

    let b = id(&app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Other", "model": "other", "instructions": "Help"}),
        )
        .await?)?;
    *recipient.lock().map_err(|_| "recipient")? = b.clone();
    let channel = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Room", "instructions": "Route", "members": [a, b], "lead": a}),
        )
        .await?)?;
    let workflow = id(&app
        .post(
            &format!("/api/agents/{a}/workflows"),
            json!({"id": new_id(), "revision": 0, "name": "Empty output",
                "graph": {"entry": "out", "maxSteps": 1, "nodes": [{"id": "out", "kind": "output", "channel": channel}]}}),
        )
        .await?)?;
    let empty = app
        .post(
            &format!("/api/workflows/{workflow}/run"),
            json!({"id": new_id(), "revision": 1, "input": ""}),
        )
        .await?;
    let run_path = format!("/api/workflow-runs/{}", id(&empty)?);
    let run = until_value(
        "empty workflow run",
        || app.get(&run_path),
        |run| run["status"] == "done" || run["status"] == "failed",
    )
    .await?;
    ensure!(run["status"] == "done", "empty workflow output: {run}");
    let channel_path = format!("/api/channels/{channel}/turns");
    let after = turn(
        &app,
        &channel_path,
        json!({"id": new_id(), "recipient": a, "text": "Reply after empty workflow output"}),
    )
    .await?;
    ensure!(after["status"] == "done", "after empty output: {after}");
    let routed = turn(
        &app,
        &channel_path,
        json!({"id": new_id(), "text": "Route me", "automatic": true}),
    )
    .await?;
    ensure!(routed["status"] == "done", "routed: {routed}");
    for scenario in ["delegate", "transfer"] {
        set(scenario)?;
        let start = model.calls();
        let row = turn(
            &app,
            &channel_path,
            json!({"id": new_id(), "text": scenario, "recipient": a}),
        )
        .await?;
        ensure!(row["status"] == "done", "{scenario}: {row}");
        let models: Vec<String> = model.requests()[start..]
            .iter()
            .map(|request| request.model().to_owned())
            .collect();
        ensure!(
            models.first().is_some_and(|model| model == "primary")
                && models.iter().any(|model| model == "other"),
            "{scenario} models: {models:?}"
        );
        ensure!(
            row["coordination"].as_array().map(Vec::len) == Some(1),
            "{scenario} coordination: {row}"
        );
    }
    set("hold")?;
    let cancelled = new_id();
    app.post_expect(&path, json!({"id": cancelled, "text": "Cancel"}), 202)
        .await?;
    let held = model.deferred().await?;
    app.post(
        &format!("{path}/{cancelled}/cancel"),
        json!({"id": new_id()}),
    )
    .await?;
    ensure!(
        settle(&app, &path, &cancelled).await?["status"] == "cancelled",
        "cancel"
    );
    drop(held);
    let usage = app.get("/api/usage").await?;
    let received = usage["calls"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["status"] == "received"))
        .ok_or("received usage")?;
    ensure!(
        received["origin"].is_null() || received["origin"] != KEY,
        "usage origin: {received}"
    );
    let text = usage.to_string();
    ensure!(
        !text.contains(KEY) && !text.contains("secret failure"),
        "usage leaked provider detail: {usage}"
    );
    app.shutdown().await?;
    Ok(())
}
