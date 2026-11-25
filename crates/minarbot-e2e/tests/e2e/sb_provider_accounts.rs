//! Agents bound to isolated provider instances across channels, side threads,
//! edits and restart (port of `app/provider-accounts.test.ts`).
//!
//! #87 replaced provider accounts with provider instances. Each Node account
//! becomes a Codex instance created through `/api/provider-instances`: the
//! kit's gateway peer as binary, its own scripted model as
//! `MINARBOT_GATEWAY_BASE_URL` and its own key as the secret
//! `MINARBOT_PROVIDER_KEY`. Provider families (`x-api-key`,
//! `x-goog-api-key`) and `/api/models?account=` left with the accounts.

use crate::sb_support::{after_tool, id, settle};
use minarbot_testkit::{
    control::{Answer, ControlModel},
    fakes::{self, Fake},
    http::Method,
    prelude::*,
    Server,
};
use std::sync::{
    atomic::{AtomicBool, Ordering::SeqCst},
    Arc, Mutex,
};

struct Instance {
    name: &'static str,
    key: Option<String>,
    model: ControlModel,
}

async fn instance(
    name: &'static str,
    key: Option<String>,
    pause: &Arc<AtomicBool>,
    transfer: &Arc<Mutex<String>>,
) -> Result<Instance> {
    let model = ControlModel::start().await?;
    let pause = pause.clone();
    let transfer = transfer.clone();
    model.respond(move |request| {
        if !after_tool(request) && pause.load(SeqCst) {
            return Answer::tool(
                "ask_user",
                json!({"question": "Continue?", "choices": ["Yes"]}),
            );
        }
        let target = transfer.lock().map(|id| id.clone()).unwrap_or_default();
        if !after_tool(request) && !target.is_empty() && name == "openai" {
            return Answer::tool(
                "transfer_task",
                json!({"recipient": target, "task": "Finish handoff"}),
            );
        }
        Answer::text(name)
    });
    Ok(Instance { name, key, model })
}

async fn create(app: &Server, instance: &Instance) -> Result<Value> {
    let mut environment = vec![json!({"name": "MINARBOT_GATEWAY_BASE_URL",
        "value": instance.model.base_url(), "secret": false})];
    if let Some(key) = &instance.key {
        environment.push(json!({"name": "MINARBOT_PROVIDER_KEY", "value": key, "secret": true}));
    }
    app.post_expect(
        "/api/provider-instances",
        json!({"id": new_id(), "instanceId": instance.name, "kind": "codex", "displayName": instance.name,
            "enabled": true, "binaryPath": fakes::path(Fake::Gateway)?, "environment": environment}),
        201,
    )
    .await
}

async fn reply(app: &Server, path: &str, text: &str) -> Result<Value> {
    let id = new_id();
    app.post_expect(path, json!({"id": id, "text": text}), 202)
        .await?;
    app.wait_row(path, &id, |row| {
        row["paused"] == 1
            || ["done", "failed"].contains(&row["status"].as_str().unwrap_or_default())
    })
    .await
}

#[tokio::test]
async fn agents_bind_isolated_provider_accounts_across_channels_side_threads_edit_and_restart(
) -> Result<()> {
    let pause = Arc::new(AtomicBool::new(false));
    let transfer = Arc::new(Mutex::new(String::new()));
    let mut instances = Vec::new();
    for (name, key) in [
        ("openai", Some("key-openai")),
        ("anthropic", Some("key-anthropic")),
        ("gemini", Some("key-gemini")),
        ("public", None),
    ] {
        instances.push(instance(name, key.map(str::to_owned), &pause, &transfer).await?);
    }
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new()).await?;
    for instance in &instances {
        create(&app, instance).await?;
    }
    let listed = app.get("/api/provider-instances").await?;
    let text = listed.to_string();
    for instance in &instances {
        ensure!(
            listed["providers"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["id"] == instance.name)),
            "{} not listed: {listed}",
            instance.name
        );
        if let Some(key) = &instance.key {
            ensure!(!text.contains(key.as_str()), "instance list leaked {key}");
        }
    }
    for instance in &instances {
        let catalog = app
            .post(
                &format!("/api/provider-instances/{}/models", instance.name),
                json!({"id": new_id(), "instanceId": instance.name}),
            )
            .await?;
        ensure!(
            catalog["models"] == json!(["fixture"]),
            "{} catalog: {catalog}",
            instance.name
        );
    }
    app.post_expect(
        "/api/provider-instances/missing/models",
        json!({"id": new_id(), "instanceId": "missing"}),
        404,
    )
    .await?;
    let mut agents = Vec::new();
    for instance in &instances {
        let command = json!({"id": new_id(), "name": instance.name, "model": "fixture", "instructions": "Help",
            "providerInstance": instance.name});
        let agent = app.post("/api/agents", command.clone()).await?;
        ensure!(
            app.post("/api/agents", command.clone()).await? == agent,
            "create replay"
        );
        let mut moved = command.clone();
        moved["providerInstance"] = json!("missing");
        let conflict = app.call(Method::POST, "/api/agents", Some(moved)).await?;
        ensure!(
            conflict.status == 409 || conflict.status == 400,
            "changed replay: {}",
            conflict.status
        );
        let agent_id = id(&agent)?;
        let side = id(&app
            .post(
                &format!("/api/agents/{agent_id}/threads"),
                json!({"id": new_id(), "name": "Side"}),
            )
            .await?)?;
        for path in [
            format!("/api/agents/{agent_id}/turns"),
            format!("/api/threads/{side}/turns"),
        ] {
            let row = reply(&app, &path, &format!("Use {}", instance.name)).await?;
            ensure!(row["reply"] == instance.name, "{path}: {row}");
        }
        let authorization: Vec<Option<String>> = instance
            .model
            .requests()
            .iter()
            .map(|request| request.authorization.clone())
            .collect();
        let expected = instance.key.as_ref().map(|key| format!("Bearer {key}"));
        ensure!(
            authorization
                .iter()
                .all(|value| *value == expected || expected.is_none() && value.is_none()),
            "{} authorization: {authorization:?}",
            instance.name
        );
        agents.push(agent);
    }
    let invalid = app
        .call(
            Method::POST,
            "/api/agents",
            Some(json!({"id": new_id(), "name": "Invalid", "model": "fixture", "instructions": "Help",
                "providerInstance": "missing"})),
        )
        .await?;
    ensure!(
        invalid.status == 400,
        "missing instance: {} {}",
        invalid.status,
        invalid.body
    );
    let (a, c) = (id(&agents[0])?, id(&agents[2])?);
    let channel = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Mixed", "instructions": "Help", "lead": a,
                "members": agents.iter().map(|agent| agent["id"].clone()).collect::<Vec<_>>()}),
        )
        .await?)?;
    *transfer.lock().map_err(|_| "transfer")? = c.clone();
    let channel_path = format!("/api/channels/{channel}/turns");
    let transferred = new_id();
    app.post_expect(
        &channel_path,
        json!({"id": transferred, "text": "Transfer", "recipient": a}),
        202,
    )
    .await?;
    let done = until_value(
        "transferred turn",
        || async {
            let rows = app.get(&channel_path).await?;
            Ok(rows
                .as_array()
                .and_then(|rows| {
                    rows.iter()
                        .find(|row| {
                            row["id"] == transferred.as_str()
                                || row["parent"] == transferred.as_str()
                                    && row["agent"] == c.as_str()
                        })
                        .cloned()
                })
                .unwrap_or(Value::Null))
        },
        |row| row["agent"] == c.as_str() && row["status"] == "done",
    )
    .await?;
    ensure!(done["reply"] == "gemini", "transfer reply: {done}");
    transfer.lock().map_err(|_| "transfer")?.clear();
    let path = format!("/api/agents/{a}/turns");
    pause.store(true, SeqCst);
    let paused = reply(&app, &path, "Pause").await?;
    pause.store(false, SeqCst);
    ensure!(paused["paused"] == 1, "pause: {paused}");
    let edit = json!({"id": new_id(), "revision": agents[0]["revision"], "name": "openai", "model": "fixture",
        "instructions": "Help", "providerInstance": "anthropic"});
    let busy = app
        .call(
            Method::POST,
            &format!("/api/agents/{a}/edit"),
            Some(edit.clone()),
        )
        .await?;
    ensure!(
        busy.status == 409,
        "switch while a question waits: {} {}",
        busy.status,
        busy.body
    );
    app.post(
        &format!("{path}/{}/cancel", id(&paused)?),
        json!({"id": new_id()}),
    )
    .await?;
    let edited = app
        .post(&format!("/api/agents/{a}/edit"), edit.clone())
        .await?;
    ensure!(
        edited["providerInstance"] == "anthropic",
        "switch: {edited}"
    );
    ensure!(
        app.post(&format!("/api/agents/{a}/edit"), edit.clone())
            .await?
            == edited,
        "edit replay"
    );
    let next = reply(&app, &path, "After switch").await?;
    ensure!(next["reply"] == "anthropic", "after switch: {next}");
    let usage = app.get("/api/usage").await?;
    let rows = usage["calls"].as_array().cloned().unwrap_or_default();
    ensure!(
        rows.iter().filter(|row| row["agent"] == a.as_str()).count() >= 2,
        "usage rows for both instances: {usage}"
    );
    app.shutdown().await?;
    let database = std::fs::read(root.database())?;
    for instance in &instances {
        if let Some(key) = &instance.key {
            ensure!(
                !database
                    .windows(key.len())
                    .any(|window| window == key.as_bytes()),
                "database stores {key} in plain text"
            );
        }
    }
    app.start_stopped(ServerConfig::new()).await?;
    let public = format!("/api/agents/{}/turns", id(&agents[3])?);
    let public_row = reply(&app, &public, "No inherited default key").await?;
    ensure!(
        public_row["reply"] == "public",
        "public instance: {public_row}"
    );
    let public_auth = instances[3]
        .model
        .requests()
        .last()
        .and_then(|request| request.authorization.clone());
    ensure!(
        public_auth.is_none(),
        "public instance got a key: {public_auth:?}"
    );
    let listed = app.get("/api/provider-instances").await?;
    let anthropic = listed["providers"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == "anthropic"))
        .ok_or("anthropic instance")?
        .clone();
    app.post(
        "/api/provider-instances/anthropic/delete",
        json!({"id": new_id(), "instanceId": "anthropic", "revision": anthropic["revision"]}),
    )
    .await?;
    let before = instances[1].model.calls();
    let refused = app
        .call(
            Method::POST,
            &path,
            Some(json!({"id": new_id(), "text": "Must not fallback"})),
        )
        .await?;
    ensure!(
        refused.status == 503,
        "removed instance: {} {}",
        refused.status,
        refused.body
    );
    ensure!(
        instances[1].model.calls() == before,
        "removed instance reached its model"
    );
    let profile = app.get("/api/agents").await?["agents"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == a.as_str()).cloned())
        .ok_or("agent a")?;
    ensure!(
        profile["provider_available"] == false,
        "available: {profile}"
    );
    ensure!(
        app.get(&path).await?.as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row["id"] == next["id"] && row["reply"] == "anthropic")),
        "history lost"
    );
    let mut recover = edit.clone();
    recover["id"] = json!(new_id());
    recover["revision"] = edited["revision"].clone();
    recover["providerInstance"] = json!("openai");
    app.post(&format!("/api/agents/{a}/edit"), recover).await?;
    let recovered = reply(&app, &path, "Explicit default").await?;
    ensure!(recovered["reply"] == "openai", "recovered: {recovered}");
    pause.store(true, SeqCst);
    let waiting = reply(&app, &path, "Last instance disappears").await?;
    pause.store(false, SeqCst);
    ensure!(waiting["paused"] == 1, "waiting: {waiting}");
    let listed = app.get("/api/provider-instances").await?;
    let openai = listed["providers"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == "openai"))
        .ok_or("openai instance")?
        .clone();
    app.post(
        "/api/provider-instances/openai/edit",
        json!({"id": new_id(), "instanceId": "openai", "kind": "codex", "displayName": "openai",
            "enabled": false, "revision": openai["revision"]}),
    )
    .await?;
    app.restart().await?;
    let sent = instances[0].model.calls();
    app.post(
        &format!(
            "{path}/{}/questions/{}/answer",
            id(&waiting)?,
            waiting["questions"][0]["id"].as_str().ok_or("question")?
        ),
        json!({"id": new_id(), "text": "Yes"}),
    )
    .await?;
    let failed = settle(&app, &path, &id(&waiting)?).await?;
    ensure!(
        failed["status"] == "failed",
        "answered on a disabled instance: {failed}"
    );
    ensure!(
        failed["error"]
            .as_str()
            .is_some_and(|error| error.contains("provider instance")),
        "error: {failed}"
    );
    ensure!(
        instances[0].model.calls() == sent,
        "disabled instance reached its model"
    );
    Ok(())
}
