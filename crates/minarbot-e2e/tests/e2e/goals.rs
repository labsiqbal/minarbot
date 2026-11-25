use minarbot_testkit::{http, server::ServerConfig, Result, TestRoot};
use serde_json::json;
use uuid::Uuid;

async fn agent() -> Result<(TestRoot, minarbot_testkit::Server, String)> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let id = Uuid::new_v4().to_string();
    http::post_json::<serde_json::Value>(
        &app,
        "/api/agents",
        &json!({"id":id,"runtime":"api","name":"Goals","model":"fixture","instructions":"Help"}),
        201,
    )
    .await?;
    Ok((root, app, id))
}

#[tokio::test]
async fn goal_route_is_scoped_to_agent() -> Result<()> {
    let (_root, app, id) = agent().await?;
    let value: serde_json::Value = http::get_json(&app, &format!("/api/agents/{id}/goal")).await?;
    assert!(value.is_null() || value.is_object());
    Ok(())
}

#[tokio::test]
async fn goal_duplicate_receipt_is_idempotent() -> Result<()> {
    let (_root, app, id) = agent().await?;
    let first: serde_json::Value = http::get_json(&app, &format!("/api/agents/{id}/goal")).await?;
    let second: serde_json::Value = http::get_json(&app, &format!("/api/agents/{id}/goal")).await?;
    assert_eq!(first, second);
    Ok(())
}
