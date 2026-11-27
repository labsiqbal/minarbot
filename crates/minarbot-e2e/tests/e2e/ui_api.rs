use minarbot_testkit::{model::ScriptedModel, prelude::*};

#[tokio::test]
async fn provider_model_picker_reads_the_instance_catalog() -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.catalog(["fixture", "alternate"]);
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new().model(&model)).await?;

    let catalog = app
        .get_expect("/api/provider-instances/codex-test-gateway/models", 200)
        .await?;
    assert_eq!(catalog["models"], json!(["fixture", "alternate"]));
    assert_eq!(catalog["catalog"][0]["id"], "fixture");
    assert_eq!(catalog["catalog"][0]["options"][0]["id"], "reasoningEffort");
    Ok(())
}

/// An instance's own binary path makes its agents runnable even when the
/// engine is not on `PATH` (the agent list once checked only `PATH`, so
/// agents on instances configured in Settings > Providers showed "Connect a
/// provider" and could not send).
#[tokio::test]
async fn agent_on_an_instance_binary_path_is_runnable_without_the_engine_on_path() -> Result<()> {
    use minarbot_testkit::{fakes::Fake, providers};
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    providers::fake_instance(&app, "codex-binary", Fake::Codex).await?;
    let agent = providers::AgentSpec::new("Bound")
        .instance("codex-binary")
        .create(&app)
        .await?;
    let listed = app.get("/api/agents").await?;
    let row = listed["agents"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == agent["id"]))
        .ok_or_else(|| format!("agent missing: {listed}"))?;
    ensure!(
        row["runnable"] == true && row["blocked_reason"].is_null(),
        "agent row: {row}"
    );
    Ok(())
}
