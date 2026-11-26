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
