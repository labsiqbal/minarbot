//! SIGKILL recovery without replay (port of `app/crash.test.ts`).
//!
//! The Node cases started the bridge in a child process and killed it; the
//! kit's [`minarbot_testkit::Server::crash`] sends SIGKILL to the server.

use crate::sb_support::id;
use minarbot_testkit::{
    control::{Answer, ControlModel},
    prelude::*,
    wait::never,
};
use std::time::Duration;

#[tokio::test]
async fn workflow_sigkill_preserves_uncertain_step_without_replay_across_two_restarts() -> Result<()>
{
    let model = ControlModel::start().await?;
    model.respond(|_| Answer::Defer);
    let root = TestRoot::new()?;
    let mut app = root
        .start(ServerConfig::new().gateway(model.gateway()))
        .await?;
    let agent = id(&app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Crash workflow", "model": "fixture",
                "instructions": "Execute."}),
        )
        .await?)?;
    let workflow = id(&app
        .post(
            &format!("/api/agents/{agent}/workflows"),
            json!({"id": new_id(), "name": "Crash probe", "revision": 0, "graph": {"entry": "work",
                "maxSteps": 3, "nodes": [
                    {"id": "work", "kind": "agent", "agent": agent, "prompt": "Hold until cancelled.", "next": "approve"},
                    {"id": "approve", "kind": "approval", "prompt": "Continue?", "yes": "out", "no": "out"},
                    {"id": "out", "kind": "output"}]}}),
        )
        .await?)?;
    let command = json!({"id": new_id(), "revision": 1, "input": "Synthetic input"});
    let run_path = format!("/api/workflow-runs/{}", command["id"].as_str().ok_or("id")?);
    let start = format!("/api/workflows/{workflow}/run");
    app.post(&start, command.clone()).await?;
    let accepted = model.deferred().await?;
    app.crash().await?;
    drop(accepted);
    let mut turn: Option<Value> = None;
    for restart in 0..2 {
        app.restart().await?;
        let run = app.get(&run_path).await?;
        ensure!(run["status"] == "waiting", "restart {restart}: {run}");
        let steps = run["steps"].as_array().ok_or("steps")?;
        ensure!(steps.len() == 1, "steps: {run}");
        ensure!(
            steps[0]["task_status"] == "interrupted",
            "task status: {run}"
        );
        ensure!(steps[0]["status"] == "waiting", "step status: {run}");
        match &turn {
            Some(turn) => ensure!(steps[0]["turn"] == *turn, "step turn changed: {run}"),
            None => turn = Some(steps[0]["turn"].clone()),
        }
        app.post(&start, command.clone()).await?;
        never(
            "workflow replays the provider call",
            Duration::from_millis(1100),
            || async { Ok(model.calls() > 1) },
        )
        .await?;
        ensure!(model.calls() == 1, "provider calls: {}", model.calls());
        let steps = app.get(&run_path).await?;
        ensure!(
            steps["steps"].as_array().map(Vec::len) == Some(1),
            "steps: {steps}"
        );
        ensure!(
            app.get(&format!("/api/agents/{agent}/turns")).await? == json!([]),
            "workflow step leaked into the direct chat"
        );
        let usage = app.get("/api/usage").await?;
        ensure!(usage["summary"]["calls"] == 1, "usage calls: {usage}");
        ensure!(usage["summary"]["interrupted"] == 1, "interrupted: {usage}");
        ensure!(
            usage["summary"]["total_tokens"].is_null(),
            "tokens: {usage}"
        );
        ensure!(
            Some(&usage["calls"][0]["turn"]) == turn.as_ref(),
            "usage turn: {usage}"
        );
        if restart == 1 {
            app.post(
                &format!("{run_path}/cancel"),
                json!({"id": new_id(), "revision": run["revision"]}),
            )
            .await?;
            ensure!(
                app.get(&run_path).await?["status"] == "cancelled",
                "run not cancelled"
            );
        }
        app.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn sigkill_after_delegated_provider_acceptance_does_not_replay_parent_or_child() -> Result<()>
{
    let model = ControlModel::start().await?;
    let recipient = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let target = recipient.clone();
    model.respond(move |request| {
        // The child step and the parent's step after the delegation
        // receipt stay in flight until the crash.
        if request.last_user_text() == "child" || crate::sb_support::after_tool(request) {
            return Answer::Defer;
        }
        let beta = target.lock().map(|id| id.clone()).unwrap_or_default();
        Answer::tool("delegate_task", json!({"recipient": beta, "task": "child"}))
    });
    let root = TestRoot::new()?;
    let mut app = root
        .start(ServerConfig::new().gateway(model.gateway()))
        .await?;
    let alpha = id(&app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Alpha", "model": "fixture",
                "instructions": "Delegate."}),
        )
        .await?)?;
    let beta = id(&app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Beta", "model": "fixture",
                "instructions": "Execute."}),
        )
        .await?)?;
    *recipient.lock().map_err(|_| "recipient")? = beta.clone();
    let channel = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Crash check", "instructions": "Coordinate.",
                "members": [alpha, beta], "lead": alpha}),
        )
        .await?)?;
    let path = format!("/api/channels/{channel}/turns");
    let parent_id = new_id();
    app.post(
        &path,
        json!({"id": parent_id, "text": "root", "recipient": alpha}),
    )
    .await?;
    let child_step = model.deferred().await?;
    let parent_step = model.deferred().await?;
    app.crash().await?;
    drop((child_step, parent_step));
    let accepted = model.calls();
    app.restart().await?;
    let rows = app.get(&path).await?;
    let rows = rows.as_array().ok_or("rows")?;
    ensure!(rows.len() == 2, "rows: {rows:?}");
    let parent = rows
        .iter()
        .find(|row| row["id"] == parent_id.as_str())
        .ok_or("parent")?;
    let child = rows
        .iter()
        .find(|row| row["parent"] == parent_id.as_str())
        .ok_or("delegated child")?;
    ensure!(parent["status"] == "interrupted", "parent: {parent}");
    ensure!(child["status"] == "interrupted", "child: {child}");
    ensure!(parent["agent"] == alpha.as_str(), "parent agent: {parent}");
    ensure!(child["agent"] == beta.as_str(), "child agent: {child}");
    let coordination = parent["coordination"].as_array().ok_or("coordination")?;
    ensure!(coordination.len() == 1, "coordination: {parent}");
    ensure!(
        coordination[0]["child"] == child["id"],
        "coordination child: {parent}"
    );
    // Node counted 2 requests (parent, child); the harness parent also takes
    // its step after the delegation receipt before the crash.
    ensure!(accepted == 3, "provider calls before the crash: {accepted}");
    never(
        "restart replays parent or child",
        Duration::from_millis(150),
        || async { Ok(model.calls() > accepted) },
    )
    .await?;
    Ok(())
}
