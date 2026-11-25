//! Server lifecycle self-tests for `minarbot-testkit`.

use minarbot_testkit::{
    bridge::{Bridge, BridgeError},
    db::Inspector,
    fakes::Fake,
    prelude::*,
    providers::{self, AgentSpec},
    FIXED_CLOCK,
};

#[tokio::test]
async fn server_restarts_on_the_same_data_crashes_and_reports_startup_errors() -> Result<()> {
    let root = TestRoot::new()?;
    let error = root
        .start_error(ServerConfig::new().option("timeout", "slow"))
        .await?;
    ensure!(
        error.contains("Minarbot startup failed") && error.contains("stderr tail"),
        "startup error: {error}"
    );
    let mut app = root.start(ServerConfig::new()).await?;
    let agent = AgentSpec::new("Kit agent").create(&app).await?;
    ensure!(
        app.origin().starts_with("http://127.0.0.1:"),
        "origin {}",
        app.origin()
    );
    app.restart().await?;
    let agents = app.get("/api/agents").await?;
    ensure!(
        agents["agents"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"] == agent["id"])),
        "restart lost data"
    );
    app.crash().await?;
    ensure!(
        app.get("/api/agents").await.is_err(),
        "crashed server answered"
    );
    app.start_stopped(ServerConfig::new().fixed_clock(FIXED_CLOCK + 60_000))
        .await?;
    ensure!(
        app.config().clock() == Some(FIXED_CLOCK + 60_000),
        "restart clock"
    );
    let count = app
        .edit_database(|db| db.scalar::<i64>("SELECT count(*) FROM agents", []))
        .await?;
    ensure!(count >= 1, "edit_database saw {count} agents");
    let stored = Inspector::open(app.database())?.row(
        "SELECT name FROM agents WHERE id=?",
        [agent["id"].as_str().unwrap_or_default()],
    )?;
    ensure!(stored["name"] == "Kit agent", "inspector: {stored}");
    ensure!(
        app.shutdown().await?.is_some_and(|status| status.success()),
        "graceful shutdown status"
    );
    Ok(())
}

#[tokio::test]
async fn provider_helpers_bind_agents_to_fake_instances_and_bridges_reach_the_server() -> Result<()>
{
    let root = TestRoot::new()?;
    let computer = Bridge::operations(|operation, _| match operation {
        "clock" => Ok(json!(FIXED_CLOCK)),
        other => Err(BridgeError::new(404, format!("unexpected {other}"))),
    })
    .await?;
    let app = root
        .start(ServerConfig::new().computer_bridge(&computer))
        .await?;
    let instance = providers::fake_instance(&app, "claude-fixture", Fake::Claude).await?;
    ensure!(
        instance["kind"] == "claude" && instance["installed"] == true,
        "instance: {instance}"
    );
    let agent = AgentSpec::new("Bound")
        .instance("claude-fixture")
        .create(&app)
        .await?;
    ensure!(
        agent["providerInstance"] == "claude-fixture",
        "agent: {agent}"
    );
    let machines = app.refresh_machines().await?;
    ensure!(
        machines.is_object() || machines.is_array(),
        "machines: {machines}"
    );
    Ok(())
}

use minarbot_testkit::model::{Reply, ScriptedModel};

/// The worked example in `docs/testing.md`; keep the two identical.
#[tokio::test]
async fn worked_example_reply_survives_restart() -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|request| Reply::text(format!("reply:{}", request.last_text())));
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new().model(&model)).await?;
    let agent = app
        .post_expect(
            "/api/agents",
            json!({"id": new_id(), "runtime": "api", "name": "A",
            "model": "fixture", "instructions": "Help."}),
            201,
        )
        .await?;
    let turns = format!(
        "/api/agents/{}/turns",
        agent["id"].as_str().unwrap_or_default()
    );
    let turn = new_id();
    app.post_expect(&turns, json!({"id": turn, "text": "hello"}), 202)
        .await?;
    let done = app.wait_status(&turns, &turn, "done").await?;
    ensure!(done["reply"] == "reply:hello", "reply: {done}");
    app.restart().await?;
    ensure!(
        app.row(&turns, &turn).await?.ok_or("lost")?["reply"] == "reply:hello",
        "restart"
    );
    ensure!(model.calls() == 1, "restart replayed the turn");
    Ok(())
}
