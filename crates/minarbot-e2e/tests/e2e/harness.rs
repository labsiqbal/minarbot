use minarbot_testkit::{http, server::ServerConfig, Result, TestRoot};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn codex_and_claude_provider_instances_are_stored() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    for (id, kind) in [("codex-fixture", "codex"), ("claude-fixture", "claude")] {
        let value: serde_json::Value = http::post_json(&app, "/api/provider-instances", &json!({"id":Uuid::new_v4().to_string(),"instanceId":id,"kind":kind,"displayName":id,"enabled":true,"binaryPath":"/tmp/fixture"}), 201).await?;
        assert_eq!(value["id"], id);
    }
    let list: serde_json::Value = http::get_json(&app, "/api/provider-instances").await?;
    assert!(list.to_string().contains("codex-fixture"));
    assert!(list.to_string().contains("claude-fixture"));
    Ok(())
}

#[tokio::test]
async fn harness_agent_turn_uses_test_workspace() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let id = Uuid::new_v4().to_string();
    let _: serde_json::Value = http::post_json(
        &app,
        "/api/agents",
        &json!({"id":id,"runtime":"api","name":"Harness","model":"fixture","instructions":"Help"}),
        201,
    )
    .await?;
    let turns: serde_json::Value = http::get_json(&app, &format!("/api/agents/{id}/turns")).await?;
    assert!(turns.as_array().is_some());
    Ok(())
}
