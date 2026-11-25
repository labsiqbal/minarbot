//! Streaming, tool recovery, models and cancellation through one provider
//! instance (port of `app/gemini.test.ts`).
//!
//! #87 removed the Gemini API client: `x-goog-api-key`, `generateContent`
//! payloads, `thoughtSignature` replay, `maxOutputTokens` and the
//! `MAX_TOKENS` finish rule are gone. The behaviour the case wrapped around
//! them carries over against the instance: the model catalog, a tool
//! artifact, a held tool step that resumes after restart without running the
//! tool again, cancellation of a held request, and streamed and unary
//! answers.

use crate::sb_support::{after_tool, id, reminder, settle};
use minarbot_testkit::{
    control::{Answer, ControlModel},
    prelude::*,
    server::Gateway,
};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn gemini_http_streaming_signed_tool_recovery_models_and_cancellation() -> Result<()> {
    let mode = Arc::new(Mutex::new(String::from("text")));
    let model = ControlModel::start().await?;
    model.catalog(["fixture", "second"]);
    let shared = mode.clone();
    model.respond(move |request| {
        let mode = shared.lock().map(|mode| mode.clone()).unwrap_or_default();
        let result = after_tool(request);
        if mode == "hold" || mode == "tool-hold" && result {
            return Answer::Defer;
        }
        if mode.starts_with("tool") && !result && !reminder(request) {
            return Answer::tool(
                "create_text_file",
                json!({"name": "native.txt", "text": "Gemini artifact"}),
            );
        }
        Answer::text("Gemini answer")
    });
    let set = |value: &str| -> Result<()> {
        *mode.lock().map_err(|_| "mode")? = value.into();
        Ok(())
    };
    let tool_steps = |model: &ControlModel| {
        model
            .requests()
            .iter()
            .filter(|request| !after_tool(request) && !reminder(request))
            .count()
    };
    let root = TestRoot::new()?;
    let gateway = |stream: bool| Gateway {
        api_key: Some("synthetic".into()),
        stream: Some(stream),
        ..model.gateway()
    };
    let mut app = root
        .start(ServerConfig::new().gateway(gateway(true)))
        .await?;
    let catalog = app
        .post(
            "/api/provider-instances/codex-test-gateway/models",
            json!({"id": new_id(), "instanceId": "codex-test-gateway"}),
        )
        .await?;
    ensure!(
        catalog["models"] == json!(["fixture", "second"]),
        "catalog: {catalog}"
    );
    let agent = id(&app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Gemini", "model": "fixture", "instructions": "Help"}),
        )
        .await?)?;
    let path = format!("/api/agents/{agent}/turns");
    for scenario in ["text", "tool"] {
        set(scenario)?;
        let id = new_id();
        app.post_expect(&path, json!({"id": id, "text": scenario}), 202)
            .await?;
        let row = settle(&app, &path, &id).await?;
        ensure!(row["status"] == "done", "{scenario}: {row}");
        ensure!(row["reply"] == "Gemini answer", "{scenario} reply: {row}");
        if scenario == "tool" {
            ensure!(
                row["generatedFiles"].as_array().map(Vec::len) == Some(1),
                "tool artifact: {row}"
            );
        }
    }
    set("tool-hold")?;
    let resumed = new_id();
    app.post_expect(
        &path,
        json!({"id": resumed, "text": "Resume signed tool"}),
        202,
    )
    .await?;
    let held = model.deferred().await?;
    let before = tool_steps(&model);
    app.shutdown().await?;
    drop(held);
    set("tool")?;
    app.restart().await?;
    let interrupted = app.wait_status(&path, &resumed, "interrupted").await?;
    app.post(
        &format!("{path}/{resumed}/resume"),
        json!({"id": new_id(), "revision": interrupted["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    let done = app.wait_status(&path, &resumed, "done").await?;
    ensure!(
        done["generatedFiles"].as_array().map(Vec::len) == Some(1),
        "resumed artifact: {done}"
    );
    ensure!(tool_steps(&model) == before, "resume ran the tool again");
    set("hold")?;
    let cancel = new_id();
    app.post_expect(&path, json!({"id": cancel, "text": "Cancel"}), 202)
        .await?;
    let held = model.deferred().await?;
    app.post(&format!("{path}/{cancel}/cancel"), json!({"id": new_id()}))
        .await?;
    app.wait_status(&path, &cancel, "cancelled").await?;
    drop(held);
    ensure!(
        model.requests().iter().all(|request| request.stream()),
        "streamed instance sent a unary request"
    );
    app.restart_with(ServerConfig::new().gateway(gateway(false)))
        .await?;
    set("text")?;
    let unary = new_id();
    let start = model.calls();
    app.post_expect(&path, json!({"id": unary, "text": "Unary"}), 202)
        .await?;
    ensure!(
        app.wait_status(&path, &unary, "done").await?["reply"] == "Gemini answer",
        "unary reply"
    );
    ensure!(
        model.requests()[start..]
            .iter()
            .all(|request| !request.stream()),
        "unary instance streamed"
    );
    ensure!(model.calls() >= 8, "requests: {}", model.calls());
    Ok(())
}
