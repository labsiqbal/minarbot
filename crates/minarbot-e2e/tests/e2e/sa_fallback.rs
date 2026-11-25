//! Model failure handling on a provider instance: failures are bounded,
//! durable and never replayed after activity or uncertainty (port of
//! `app/fallback.test.ts`). #87 accepts and ignores `backupModel` (owner
//! decision), so every scenario must make exactly one attempt with the
//! agent's model and never switch to the backup.

use crate::sa_support::{lock, turn_tool_results, RawAction, RawProvider, Shared};
use minarbot_testkit::{prelude::*, server::Gateway, Server};
use std::sync::Arc;
use tokio::sync::Notify;

#[derive(Default)]
struct Upstream {
    mode: &'static str,
    calls: Vec<(String, &'static str)>,
}

fn completion(body: Value) -> RawAction {
    RawAction::Reply((
        200,
        vec![("content-type".into(), "application/json".into())],
        body.to_string().into_bytes(),
    ))
}

fn reject(status: u16) -> RawAction {
    RawAction::Reply((status, vec![], b"private upstream detail".to_vec()))
}

/// #87: no backup model is kept (the field is absent or empty).
fn no_backup(agent: &Value) -> bool {
    agent["backup_model"].is_null() || agent["backup_model"] == ""
}

async fn settle(app: &Server, path: &str, id: &str) -> Result<Value> {
    app.wait_row(path, id, |row| {
        ["done", "failed", "cancelled"].contains(&row["status"].as_str().unwrap_or(""))
    })
    .await
}

fn models(state: &Shared<Upstream>, from: usize) -> Result<Vec<String>> {
    Ok(lock(state)?
        .calls
        .iter()
        .skip(from)
        .map(|(model, _)| model.clone())
        .collect())
}

#[tokio::test]
async fn backup_model_is_opt_in_bounded_durable_and_never_replays_after_activity_or_uncertainty(
) -> Result<()> {
    let state: Shared<Upstream> = Shared::default();
    lock(&state)?.mode = "success";
    let hold = Arc::new(Notify::new());
    let (script, held) = (state.clone(), hold.clone());
    let provider = RawProvider::start_async(Arc::new(move |request| {
        let (script, held) = (script.clone(), held.clone());
        Box::pin(async move {
            let body = request.json();
            let model = body["model"].as_str().unwrap_or_default().to_owned();
            let mode = {
                let Ok(mut state) = script.lock() else { return reject(500) };
                let mode = state.mode;
                state.calls.push((model.clone(), mode));
                mode
            };
            let last_is_tool = body["messages"].as_array().and_then(|m| m.last()).is_some_and(|m| m["role"] == "tool");
            let tool = |name: &str, args: Value| {
                completion(json!({"choices": [{"finish_reason": "tool_calls", "message": {"tool_calls": [
                    {"id": new_id(), "type": "function", "function": {"name": name, "arguments": args.to_string()}}]}}]}))
            };
            match mode {
                "after-tool" if last_is_tool => reject(503),
                "after-tool" => tool("create_text_file", json!({"name": "saved.txt", "text": "Saved once"})),
                "truncated" => completion(json!({"choices": [{"finish_reason": "length", "message": {"content": "Partial"}}]})),
                "malformed" => RawAction::Reply((200, vec![("content-type".into(), "application/json".into())], b"{".to_vec())),
                "disconnect" => RawAction::Lose,
                "hold" => {
                    held.notified().await;
                    reject(503)
                }
                "forbidden" => reject(403),
                "double-failure" => reject(503),
                _ if model == "primary" => reject(if mode == "rate-limit" { 429 } else { 503 }),
                "backup-tools" if !last_is_tool => tool("read_history", json!({})),
                _ => completion(json!({"choices": [{"finish_reason": "stop", "message": {"content": format!("Answer from {model}")}}],
                    "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}})),
            }
        })
    }))
    .await?;
    let root = TestRoot::new()?;
    let options = ServerConfig::new().gateway(Gateway::new(provider.base_url()));
    let mut app = root.start(options).await?;
    let create = json!({"id": new_id(), "name": "Fallback", "model": "primary", "backupModel": "backup",
        "instructions": "Test", "runtime": "api"});
    let agent = app.post("/api/agents", create.clone()).await?;
    // #87: `backupModel` is accepted and ignored.
    ensure!(no_backup(&agent), "backup model stored: {agent}");
    ensure!(
        app.post("/api/agents", create.clone()).await?["id"] == agent["id"],
        "create replay"
    );
    let agent_id = agent["id"].as_str().unwrap_or_default().to_owned();
    let path = format!("/api/agents/{agent_id}/turns");
    for scenario in [
        "success",
        "rate-limit",
        "backup-tools",
        "double-failure",
        "forbidden",
        // "truncated" (finish_reason `length`) is dropped: since #87 the
        // harness reads provider responses and decides how to finish.
        "malformed",
        "disconnect",
        "after-tool",
    ] {
        lock(&state)?.mode = scenario;
        let start = lock(&state)?.calls.len();
        let id = new_id();
        let body = json!({"id": id, "text": scenario});
        app.post(&path, body.clone()).await?;
        let row = settle(&app, &path, &id).await?;
        ensure!(row["status"] == "failed", "{scenario}: {row}");
        let expected: &[&str] = if scenario == "after-tool" {
            &["primary", "primary"]
        } else {
            &["primary"]
        };
        let used = models(&state, start)?;
        ensure!(used == expected, "{scenario} models: {used:?}");
        ensure!(
            row["answer_model"] != "backup",
            "{scenario} switched to the backup"
        );
        if scenario == "after-tool" {
            ensure!(
                row["generatedFiles"].as_array().map(Vec::len) == Some(1),
                "after-tool files: {row}"
            );
        }
        app.post(&path, body).await?;
        ensure!(
            lock(&state)?.calls.len() == start + used.len(),
            "{scenario} replay called the model"
        );
        let usage = app.get("/api/usage").await?;
        let rows: Vec<Value> = usage["calls"]
            .as_array()
            .map(|calls| {
                calls
                    .iter()
                    .filter(|call| call["turn"] == id.as_str())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        ensure!(!rows.is_empty(), "{scenario} usage rows missing");
        ensure!(
            !json!(rows).to_string().contains("private upstream detail"),
            "{scenario} usage leaked the upstream detail"
        );
        ensure!(
            !row.to_string().contains("private upstream detail"),
            "{scenario} turn leaked the upstream detail"
        );
    }
    app.restart().await?;
    let agents = app.get("/api/agents").await?;
    ensure!(
        agents["agents"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["id"] == agent_id.as_str()))
            .is_some_and(no_backup),
        "backup model after restart"
    );
    lock(&state)?.mode = "success";
    let channel = app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Room", "instructions": "Test", "members": [agent_id], "lead": agent_id}),
        )
        .await?;
    let thread = app
        .post(
            &format!("/api/agents/{agent_id}/threads"),
            json!({"id": new_id(), "name": "Side"}),
        )
        .await?;
    for scope in [
        format!(
            "/api/channels/{}/turns",
            channel["id"].as_str().unwrap_or_default()
        ),
        format!(
            "/api/threads/{}/turns",
            thread["id"].as_str().unwrap_or_default()
        ),
    ] {
        let id = new_id();
        let start = lock(&state)?.calls.len();
        let mut body = json!({"id": id, "text": "Scoped fallback"});
        if scope.contains("channels") {
            body["recipient"] = json!(agent_id);
        }
        app.post(&scope, body).await?;
        ensure!(
            settle(&app, &scope, &id).await?["status"] == "failed",
            "{scope} turn"
        );
        let used = models(&state, start)?;
        ensure!(used == ["primary"], "{scope} models: {used:?}");
        ensure!(
            app.row(&path, &id).await?.is_none(),
            "{scope} turn in main history"
        );
    }
    let graph = json!({"entry": "choose", "maxSteps": 4, "nodes": [
        {"id": "choose", "kind": "condition", "operator": "model", "model": "primary", "value": "Accept?", "yes": "end", "no": "end"},
        {"id": "end", "kind": "output"}]});
    let workflow = app
        .post(
            &format!("/api/agents/{agent_id}/workflows"),
            json!({"id": new_id(), "revision": 0, "name": "Explicit model", "graph": graph}),
        )
        .await?;
    let start = lock(&state)?.calls.len();
    let run = app
        .post(
            &format!(
                "/api/workflows/{}/run",
                workflow["id"].as_str().unwrap_or_default()
            ),
            json!({"id": new_id(), "revision": 1, "input": "Test"}),
        )
        .await?;
    let current = app
        .wait_get(
            &format!(
                "/api/workflow-runs/{}",
                run["id"].as_str().unwrap_or_default()
            ),
            |run| run["status"] == "failed",
        )
        .await?;
    ensure!(current["status"] == "failed", "workflow: {current}");
    ensure!(
        models(&state, start)? == ["primary"],
        "workflow models: {:?}",
        models(&state, start)?
    );
    lock(&state)?.mode = "hold";
    let cancelled = new_id();
    let start = lock(&state)?.calls.len();
    app.post(&path, json!({"id": cancelled, "text": "Cancel active"}))
        .await?;
    until("held model call", || async {
        Ok((lock(&state)?.calls.len() > start).then_some(()))
    })
    .await?;
    app.post(
        &format!("{path}/{cancelled}/cancel"),
        json!({"id": new_id()}),
    )
    .await?;
    ensure!(
        settle(&app, &path, &cancelled).await?["status"] == "cancelled",
        "cancel"
    );
    ensure!(lock(&state)?.calls.len() == start + 1, "cancel replayed");
    hold.notify_waiters();
    lock(&state)?.mode = "disconnect";
    let uncertain = new_id();
    app.post(&path, json!({"id": uncertain, "text": "Uncertain"}))
        .await?;
    settle(&app, &path, &uncertain).await?;
    app.restart().await?;
    lock(&state)?.mode = "success";
    let start = lock(&state)?.calls.len();
    let failed = app
        .row(&path, &uncertain)
        .await?
        .ok_or("uncertain turn missing")?;
    app.post(
        &format!("{path}/{uncertain}/resume"),
        json!({"id": new_id(), "revision": failed["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    ensure!(
        settle(&app, &path, &uncertain).await?["status"] == "failed",
        "resumed uncertain turn"
    );
    ensure!(
        models(&state, start)? == ["primary"],
        "resume models: {:?}",
        models(&state, start)?
    );
    let template = app.get(&format!("/api/agents/{agent_id}/template")).await?;
    ensure!(
        template["backupModel"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "template exports a backup model: {template}"
    );
    let current = app.get("/api/agents").await?["agents"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["id"] == agent_id.as_str())
                .cloned()
        })
        .ok_or("agent missing")?;
    let edit = json!({"id": new_id(), "revision": current["revision"], "name": current["name"], "model": current["model"],
        "instructions": current["instructions"], "backupModel": ""});
    let edited = app
        .post(&format!("/api/agents/{agent_id}/edit"), edit.clone())
        .await?;
    ensure!(no_backup(&edited), "edited: {edited}");
    ensure!(
        app.post(&format!("/api/agents/{agent_id}/edit"), edit)
            .await?
            == edited,
        "edit replay"
    );
    let disabled = new_id();
    let before = lock(&state)?.calls.len();
    app.post(&path, json!({"id": disabled, "text": "No fallback"}))
        .await?;
    ensure!(
        settle(&app, &path, &disabled).await?["status"] == "failed",
        "no fallback turn"
    );
    ensure!(lock(&state)?.calls.len() == before + 1, "no fallback calls");
    Ok(())
}
