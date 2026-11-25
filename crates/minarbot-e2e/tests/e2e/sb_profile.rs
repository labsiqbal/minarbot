//! `edit_profile` authority, revisions, execution snapshot and durable results
//! (port of `app/profile-tools.test.ts`).

use crate::sb_support::{after_tool, id, last_result, reminder, settle};
use minarbot_testkit::{
    control::{Answer, ControlModel},
    prelude::*,
    Server,
};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct Script {
    pub tool: String,
    pub args: Value,
    pub hold: bool,
    pub defer_first: bool,
    pub answer_only: bool,
    pub results: Vec<Value>,
    pub systems: Vec<String>,
    pub models: Vec<String>,
    pub advertised: bool,
}

pub type Shared = Arc<Mutex<Script>>;

/// Model that calls `script.tool` with `script.args` once per turn and then
/// answers. `hold` defers the step after the tool result; `defer_first`
/// defers the next first step (so the test can act before the tool runs).
pub fn tool_model(model: &ControlModel, script: &Shared, answer: &'static str) {
    let shared = script.clone();
    model.respond(move |request| {
        let Ok(mut script) = shared.lock() else {
            return Answer::Status(500, json!({"error": "script"}));
        };
        script.systems.push(request.system());
        script.models.push(request.model().to_owned());
        script.advertised = request.tool_names().iter().any(|name| *name == script.tool);
        if after_tool(request) {
            script.results.push(last_result(request));
            if script.hold {
                return Answer::Defer;
            }
            return Answer::text(answer);
        }
        if script.answer_only || reminder(request) {
            return Answer::text(answer);
        }
        if script.defer_first {
            script.defer_first = false;
            return Answer::Defer;
        }
        Answer::tool(&script.tool, script.args.clone())
    });
}

pub fn set(script: &Shared, change: impl FnOnce(&mut Script)) -> Result<()> {
    change(&mut *script.lock().map_err(|_| "script")?);
    Ok(())
}

pub fn get<T>(script: &Shared, read: impl FnOnce(&Script) -> T) -> Result<T> {
    Ok(read(&*script.lock().map_err(|_| "script")?))
}

/// Post a turn, wait for `status`, then post the same command again (it must
/// be accepted as a replay without new work). Returns the turn id.
pub async fn run(
    app: &Server,
    model: &ControlModel,
    path: &str,
    text: &str,
    status: &str,
) -> Result<String> {
    let id = new_id();
    let command = json!({"id": id, "text": text});
    app.post_expect(&format!("{path}/turns"), command.clone(), 202)
        .await?;
    let row = settle(app, &format!("{path}/turns"), &id).await?;
    ensure!(row["status"] == status, "{path} {text}: {row}");
    let calls = model.calls();
    app.post_expect(&format!("{path}/turns"), command, 202)
        .await?;
    ensure!(model.calls() == calls, "replayed turn ran again");
    Ok(id)
}

pub async fn profile(app: &Server, agent: &str) -> Result<Value> {
    let agents = app.get("/api/agents").await?;
    agents["agents"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == agent).cloned())
        .ok_or_else(|| format!("agent {agent} missing").into())
}

fn last(script: &Shared) -> Result<Value> {
    get(script, |script| {
        script.results.last().cloned().unwrap_or(Value::Null)
    })
}

#[tokio::test]
async fn profile_tool_preserves_authority_revision_conflicts_execution_snapshot_and_durable_results(
) -> Result<()> {
    let script: Shared = Arc::new(Mutex::new(Script {
        tool: "edit_profile".into(),
        ..Script::default()
    }));
    let model = ControlModel::start().await?;
    tool_model(&model, &script, "Profile result checked.");
    let root = TestRoot::new()?;
    let mut app = root
        .start(ServerConfig::new().gateway(model.gateway()))
        .await?;
    let agent = id(&app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Original", "model": "fixture",
                "instructions": "Original instructions."}),
        )
        .await?)?;
    let other = id(&app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Other", "model": "fixture",
                "instructions": "Other instructions."}),
        )
        .await?)?;
    let path = format!("/api/agents/{agent}");
    let side = id(&app
        .post(
            &format!("{path}/threads"),
            json!({"id": new_id(), "name": "Side"}),
        )
        .await?)?;
    let room = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Room", "instructions": "Help", "members": [agent], "lead": agent}),
        )
        .await?)?;

    set(&script, |script| {
        script.args = json!({"revision": 1, "name": "Renamed", "role": " Orchestration agent ",
            "instructions": "Updated instructions."});
    })?;
    run(&app, &model, &path, "Change your profile", "done").await?;
    let result = last(&script)?;
    ensure!(
        get(&script, |script| script.advertised)?,
        "edit_profile not offered"
    );
    ensure!(result["revision"] == 2, "result: {result}");
    ensure!(result["name"] == "Renamed", "result: {result}");
    ensure!(
        result["role"] == "Orchestration agent",
        "role not trimmed: {result}"
    );
    let systems = get(&script, |script| script.systems.clone())?;
    ensure!(
        systems[systems.len().saturating_sub(2)..]
            .iter()
            .all(|text| {
                text.starts_with("<runtime_info>") && text.contains("\n\nOriginal instructions.")
            }),
        "the running turn lost its instruction snapshot"
    );
    ensure!(
        profile(&app, &agent).await?["instructions"] == "Updated instructions.",
        "instructions not saved"
    );
    set(&script, |script| script.answer_only = true)?;
    run(&app, &model, &path, "Change your profile", "done").await?;
    set(&script, |script| script.answer_only = false)?;
    ensure!(
        get(&script, |script| script.systems.last().cloned())?
            .is_some_and(|text| text.contains("\n\nUpdated instructions.")),
        "the next turn did not use the new instructions"
    );
    set(&script, |script| {
        script.args = json!({"revision": 2, "role": ""})
    })?;
    run(
        &app,
        &model,
        &format!("/api/threads/{side}"),
        "Change your profile",
        "done",
    )
    .await?;
    ensure!(
        last(&script)?["revision"] == 3,
        "side edit: {}",
        last(&script)?
    );
    let current = profile(&app, &agent).await?;
    ensure!(current["role"] == "", "role: {current}");
    ensure!(
        current["instructions"] == "Updated instructions.",
        "instructions: {current}"
    );

    let mut bad = vec![
        json!({"revision": 2, "name": "stale"}),
        json!({"revision": 3}),
        json!({"revision": 3, "name": ""}),
    ];
    for role in [
        json!(null),
        json!(1),
        json!("x".repeat(61)),
        json!("bad\nrole"),
        json!("bad\u{0085}role"),
        json!("bad\u{2028}role"),
    ] {
        bad.push(json!({"revision": 3, "role": role}));
    }
    bad.extend([
        json!({"revision": 3, "instructions": "x".repeat(8001)}),
        json!({"revision": 3, "name": "x", "model": "elsewhere"}),
        json!({"revision": 3, "name": "x", "providerAccount": "secret"}),
        json!({"revision": 3, "name": "x", "agent": other}),
    ]);
    for args in bad {
        set(&script, |script| script.args = args.clone())?;
        run(&app, &model, &path, "Change your profile", "done").await?;
        let result = last(&script)?;
        ensure!(
            result["status"] == 400 || result["status"] == 409,
            "{args}: {result}"
        );
        ensure!(
            profile(&app, &agent).await?["revision"] == 3,
            "{args} changed the profile"
        );
    }

    // Harness turns get the refused tool as an MCP error and finish; the
    // Node direct client failed the turn.
    set(&script, |script| {
        script.args = json!({"revision": 3, "instructions": "Channel cannot edit"});
        script.results.clear();
    })?;
    run(
        &app,
        &model,
        &format!("/api/channels/{room}"),
        "Change your profile",
        "done",
    )
    .await?;
    ensure!(
        !get(&script, |script| script.advertised)?,
        "edit_profile offered in a channel"
    );
    ensure!(
        last(&script)?["error"].is_string(),
        "channel edit result: {}",
        last(&script)?
    );
    ensure!(
        profile(&app, &agent).await?["revision"] == 3,
        "channel edited the profile"
    );

    set(&script, |script| {
        script.args = json!({"revision": 3, "instructions": "Persisted before restart."});
        script.hold = true;
    })?;
    let interrupted = new_id();
    app.post_expect(
        &format!("{path}/turns"),
        json!({"id": interrupted, "text": "Change instructions"}),
        202,
    )
    .await?;
    let held = model.deferred().await?;
    ensure!(
        profile(&app, &agent).await?["revision"] == 4,
        "edit not durable before restart"
    );
    app.shutdown().await?;
    drop(held);
    set(&script, |script| script.hold = false)?;
    app.restart().await?;
    app.post(
        &format!("{path}/edit"),
        json!({"id": new_id(), "revision": 4, "name": "User choice", "model": "fixture",
            "instructions": "User instructions."}),
    )
    .await?;
    let stopped = app
        .wait_status(&format!("{path}/turns"), &interrupted, "interrupted")
        .await?;
    app.post(
        &format!("{path}/turns/{interrupted}/resume"),
        json!({"id": new_id(), "revision": stopped["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    app.wait_status(&format!("{path}/turns"), &interrupted, "done")
        .await?;
    let result = last(&script)?;
    ensure!(result["revision"] == 4, "journaled result: {result}");
    ensure!(
        result["instructions"] == "Persisted before restart.",
        "journaled result: {result}"
    );
    let current = profile(&app, &agent).await?;
    ensure!(
        current["name"] == "User choice",
        "user edit lost: {current}"
    );
    ensure!(current["revision"] == 5, "revision: {current}");

    set(&script, |script| {
        script.args = json!({"revision": 5, "name": "Cancelled"});
        script.defer_first = true;
    })?;
    let cancelled = new_id();
    app.post_expect(
        &format!("{path}/turns"),
        json!({"id": cancelled, "text": "Cancel"}),
        202,
    )
    .await?;
    let first = model.deferred().await?;
    app.post(
        &format!("{path}/turns/{cancelled}/cancel"),
        json!({"id": new_id()}),
    )
    .await?;
    first.answer(Answer::tool(
        "edit_profile",
        json!({"revision": 5, "name": "Cancelled"}),
    ));
    app.wait_status(&format!("{path}/turns"), &cancelled, "cancelled")
        .await?;
    ensure!(
        profile(&app, &agent).await?["revision"] == 5,
        "cancelled turn edited the profile"
    );
    ensure!(
        profile(&app, &other).await?["revision"] == 1,
        "other agent changed"
    );

    app.edit_database(|db| {
        db.execute_batch("CREATE TRIGGER reject_profile_journal BEFORE INSERT ON flow_tool_calls BEGIN SELECT RAISE(ABORT, 'synthetic journal failure'); END")
    })
    .await?;
    // The rolled-back edit reaches the harness model as a tool error, so the
    // turn finishes; the Node direct client failed the turn.
    set(&script, |script| {
        script.args = json!({"revision": 5, "name": "Rolled back"})
    })?;
    run(&app, &model, &path, "Change your profile", "done").await?;
    ensure!(
        last(&script)?["error"].is_string(),
        "journal failure result: {}",
        last(&script)?
    );
    let current = profile(&app, &agent).await?;
    ensure!(
        current["revision"] == 5,
        "journal failure kept the edit: {current}"
    );
    ensure!(
        current["name"] == "User choice",
        "journal failure kept the edit: {current}"
    );
    Ok(())
}
