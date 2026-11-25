//! Hostile external MCP output schemas (port of `app/mcp-isolation.test.ts`).

use minarbot_testkit::{
    mcp_server::McpServer,
    model::{Reply, ScriptedModel},
    prelude::*,
    wait::until_within,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

#[tokio::test]
async fn a_hostile_mcp_output_schema_cannot_block_http_exceed_the_worker_deadline_or_resist_cancellation(
) -> Result<()> {
    let external = McpServer::start(
        json!([{"name": "slow", "inputSchema": {"type": "object"},
            "outputSchema": {"type": "object",
                "properties": {"text": {"type": "string", "pattern": "^(a+)+$"}}}}]),
        json!({"content": [{"type": "text", "text": "done"}],
            "structuredContent": {"text": format!("{}!", "a".repeat(32))}}),
    )
    .await?;
    let connection = Arc::new(Mutex::new(String::new()));
    let chosen = connection.clone();
    let model = ScriptedModel::start().await?;
    model.respond(move |_| {
        let connection = chosen.lock().map(|id| id.clone()).unwrap_or_default();
        Reply::tool(
            "call_mcp_tool",
            json!({"connection": connection, "name": "slow", "arguments": {}}),
        )
    });
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new().model(&model)).await?;
    let agent = app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "A", "model": "fixture",
                "instructions": "Help"}),
        )
        .await?;
    let agent_id = agent["id"].as_str().ok_or("agent id")?.to_owned();
    let tested = app
        .post(
            "/api/mcp/test",
            json!({"id": new_id(), "name": "Hostile schema", "url": external.url(), "token": ""}),
        )
        .await?;
    let saved = app
        .post("/api/mcp", json!({"id": new_id(), "test": tested["id"]}))
        .await?;
    let saved_id = saved["id"].as_str().ok_or("connection id")?.to_owned();
    if let Ok(mut slot) = connection.lock() {
        slot.clone_from(&saved_id);
    }
    app.post(
        &format!("/api/agents/{agent_id}/mcp/{saved_id}"),
        json!({"id": new_id(), "revision": 0, "connectionRevision": 1, "tools": ["slow"]}),
    )
    .await?;
    let path = format!("/api/agents/{agent_id}/turns");

    // HTTP stays responsive while the tool call is in flight.
    let id = new_id();
    app.post(&path, json!({"id": id, "text": "Run tool"}))
        .await?;
    until("first MCP effect", || async {
        Ok((external.effects() == 1).then_some(()))
    })
    .await?;
    let health = tokio::time::timeout(Duration::from_secs(5), app.get("/api/agents")).await;
    ensure!(
        matches!(health, Ok(Ok(_))),
        "HTTP blocked during the MCP call: {health:?}"
    );
    let active = tokio::time::timeout(Duration::from_secs(5), app.row(&path, &id))
        .await??
        .ok_or("turn missing")?;
    ensure!(
        active["status"] == "running",
        "HTTP responds while the MCP call is in flight: {active}"
    );

    // The hostile result fails the turn within the worker deadline as uncertain.
    external.release(1);
    let failed = until_within("MCP tool failure", Duration::from_secs(5), || async {
        let row = app.row(&path, &id).await?;
        Ok(row.filter(|row| row["status"] == "failed"))
    })
    .await?;
    ensure!(
        failed["error"]
            .as_str()
            .is_some_and(|error| error.contains("uncertain")),
        "error: {}",
        failed["error"]
    );

    // A turn cancelled during the call stays cancelled when the call ends.
    let cancel_id = new_id();
    app.post(&path, json!({"id": cancel_id, "text": "Cancel this tool"}))
        .await?;
    until("cancelled MCP effect", || async {
        Ok((external.effects() == 2).then_some(()))
    })
    .await?;
    app.post(
        &format!("{path}/{cancel_id}/cancel"),
        json!({"id": new_id()}),
    )
    .await?;
    external.release(2);
    let rows = tokio::time::timeout(Duration::from_secs(5), app.row(&path, &cancel_id))
        .await??
        .ok_or("cancelled turn missing")?;
    ensure!(rows["status"] == "cancelled", "cancelled turn: {rows}");
    Ok(())
}
