//! Provider settings read and model-change snapshots (port of
//! `app/provider-settings-tools.test.ts`).

use crate::sb_profile::{get, profile, run, set, tool_model, Script, Shared};
use crate::sb_support::id;
use minarbot_testkit::{
    control::{Answer, ControlModel},
    prelude::*,
    server::Gateway,
};
use std::sync::{Arc, Mutex};

fn last(script: &Shared) -> Result<Value> {
    get(script, |script| {
        script.results.last().cloned().unwrap_or(Value::Null)
    })
}

/// #87 replaced `providerAccount`/`backupModel` with the provider instance
/// binding, so the result is `{revision, providerInstance, model,
/// configured}` (the shape the canonical executor already returned).
#[tokio::test]
async fn provider_settings_read_stays_private_and_excludes_endpoint_and_credentials() -> Result<()>
{
    let secret = new_id();
    let script: Shared = Arc::new(Mutex::new(Script {
        tool: "read_provider_settings".into(),
        args: json!({}),
        ..Script::default()
    }));
    let model = ControlModel::start().await?;
    tool_model(&model, &script, "Configuration read.");
    let base_url = model.base_url();
    let root = TestRoot::new()?;
    let gateway = Gateway {
        api_key: Some(secret.clone()),
        ..model.gateway()
    };
    let app = root.start(ServerConfig::new().gateway(gateway)).await?;
    let agent = id(&app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Reader", "model": "fixture",
                "backupModel": "backup", "instructions": "Help"}),
        )
        .await?)?;
    let path = format!("/api/agents/{agent}");
    let instance = profile(&app, &agent).await?["providerInstance"].clone();
    ensure!(
        instance.as_str().is_some_and(|id| !id.is_empty()),
        "no instance: {instance}"
    );
    let side = id(&app
        .post(
            &format!("{path}/threads"),
            json!({"id": new_id(), "name": "Side"}),
        )
        .await?)?;
    let room = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Room", "members": [agent], "lead": agent, "instructions": "Help"}),
        )
        .await?)?;
    for location in [path.clone(), format!("/api/threads/{side}")] {
        run(&app, &model, &location, "Read settings", "done").await?;
        ensure!(
            get(&script, |script| script.advertised)?,
            "{location}: not offered"
        );
        let result = last(&script)?;
        ensure!(
            result
                == json!({"revision": 1, "providerInstance": instance, "model": "fixture", "configured": true}),
            "{location}: {result}"
        );
        let text = result.to_string();
        ensure!(
            !text.contains(&secret) && !text.contains(&base_url),
            "{location} leaked provider endpoint or key: {text}"
        );
    }
    set(&script, |script| script.args = json!({"agent": new_id()}))?;
    run(&app, &model, &path, "Read settings", "done").await?;
    ensure!(
        last(&script)?
            == json!({"error": "Reading provider settings does not accept arguments.", "status": 400}),
        "arguments: {}",
        last(&script)?
    );
    // Harness turns get the refused tool as an MCP error and finish; the
    // Node direct client failed the turn.
    set(&script, |script| script.args = json!({}))?;
    run(
        &app,
        &model,
        &format!("/api/channels/{room}"),
        "Read settings",
        "done",
    )
    .await?;
    ensure!(
        !get(&script, |script| script.advertised)?,
        "offered in a channel"
    );
    ensure!(
        last(&script)?["error"].is_string(),
        "channel read: {}",
        last(&script)?
    );
    ensure!(
        app.get("/api/agents").await?["agents"][0]["revision"] == 1,
        "reading changed the profile"
    );
    Ok(())
}

/// The Node case drove `set_provider_model`, which #87 removed with backup
/// models and provider fallback. The model now changes only through the
/// profile edit; what carries over is the execution snapshot (a running turn
/// keeps the model it started with, a resumed turn uses the current one),
/// scope (only this agent changes), cancellation before an effect and
/// restart.
#[tokio::test]
async fn model_changes_preserve_execution_snapshot_scope_and_journal_across_restart() -> Result<()>
{
    let script: Shared = Arc::new(Mutex::new(Script {
        tool: "read_provider_settings".into(),
        args: json!({}),
        ..Script::default()
    }));
    let model = ControlModel::start().await?;
    tool_model(&model, &script, "Model settings checked.");
    let root = TestRoot::new()?;
    let mut app = root
        .start(ServerConfig::new().gateway(model.gateway()))
        .await?;
    let agent = id(&app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Original", "instructions": "Help",
                "model": "first", "backupModel": "backup"}),
        )
        .await?)?;
    let other = id(&app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Other", "instructions": "Help", "model": "other"}),
        )
        .await?)?;
    let path = format!("/api/agents/{agent}");
    let side = id(&app
        .post(
            &format!("{path}/threads"),
            json!({"id": new_id(), "name": "Side"}),
        )
        .await?)?;
    let models = |script: &Shared| get(script, |script| script.models.clone());

    run(&app, &model, &path, "Read model", "done").await?;
    let seen = models(&script)?;
    ensure!(
        seen[seen.len() - 2..] == ["first", "first"],
        "models: {seen:?}"
    );
    ensure!(
        last(&script)?["model"] == "first",
        "result: {}",
        last(&script)?
    );
    app.post(
        &format!("{path}/edit"),
        json!({"id": new_id(), "revision": 1, "name": "Original", "instructions": "Help", "model": "second"}),
    )
    .await?;
    set(&script, |script| script.answer_only = true)?;
    run(
        &app,
        &model,
        &format!("/api/threads/{side}"),
        "Use new model",
        "done",
    )
    .await?;
    set(&script, |script| script.answer_only = false)?;
    ensure!(
        models(&script)?
            .last()
            .is_some_and(|model| model == "second"),
        "side model"
    );

    set(&script, |script| script.hold = true)?;
    let interrupted = new_id();
    app.post_expect(
        &format!("{path}/turns"),
        json!({"id": interrupted, "text": "Read model then wait"}),
        202,
    )
    .await?;
    let held = model.deferred().await?;
    ensure!(
        held.request.model() == "second",
        "held step: {}",
        held.request.model()
    );
    ensure!(
        last(&script)?["revision"] == 2,
        "result: {}",
        last(&script)?
    );
    app.shutdown().await?;
    drop(held);
    set(&script, |script| script.hold = false)?;
    app.restart().await?;
    app.post(
        &format!("{path}/edit"),
        json!({"id": new_id(), "revision": 2, "name": "Original", "instructions": "Help", "model": "user-choice"}),
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
    ensure!(
        models(&script)?
            .last()
            .is_some_and(|model| model == "user-choice"),
        "resume did not use the current model: {:?}",
        models(&script)?
    );
    let current = profile(&app, &agent).await?;
    ensure!(
        current["revision"] == 3 && current["model"] == "user-choice",
        "profile: {current}"
    );

    set(&script, |script| script.defer_first = true)?;
    let cancelled = new_id();
    app.post_expect(
        &format!("{path}/turns"),
        json!({"id": cancelled, "text": "Cancel before the read"}),
        202,
    )
    .await?;
    let first = model.deferred().await?;
    app.post(
        &format!("{path}/turns/{cancelled}/cancel"),
        json!({"id": new_id()}),
    )
    .await?;
    first.answer(Answer::tool("read_provider_settings", json!({})));
    app.wait_status(&format!("{path}/turns"), &cancelled, "cancelled")
        .await?;
    ensure!(
        profile(&app, &agent).await?["revision"] == 3,
        "cancel changed the profile"
    );
    ensure!(
        profile(&app, &other).await?["model"] == "other",
        "other agent changed"
    );
    Ok(())
}
