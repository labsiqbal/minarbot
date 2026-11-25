use minarbot_testkit::{http, server::ServerConfig, Result, TestRoot};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn stored_threads_are_created_and_listed_without_agent_turn() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let agent = Uuid::new_v4().to_string();
    http::post_json::<serde_json::Value>(&app, "/api/agents", &json!({"id":agent,"runtime":"api","name":"Thread agent","model":"fixture","instructions":"Help"}), 201).await?;
    let thread = http::post_json::<serde_json::Value>(
        &app,
        &format!("/api/agents/{agent}/threads"),
        &json!({"id":Uuid::new_v4().to_string(),"name":"Side"}),
        200,
    )
    .await?;
    let list: Vec<serde_json::Value> =
        http::get_json(&app, &format!("/api/agents/{agent}/threads")).await?;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], thread["id"]);
    Ok(())
}

#[tokio::test]
async fn duplicate_thread_command_is_idempotent() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let agent = Uuid::new_v4().to_string();
    http::post_json::<serde_json::Value>(&app, "/api/agents", &json!({"id":agent,"runtime":"api","name":"Thread agent","model":"fixture","instructions":"Help"}), 201).await?;
    let body = json!({"id":Uuid::new_v4().to_string(),"name":"Side"});
    let first: serde_json::Value =
        http::post_json(&app, &format!("/api/agents/{agent}/threads"), &body, 200).await?;
    let second: serde_json::Value =
        http::post_json(&app, &format!("/api/agents/{agent}/threads"), &body, 200).await?;
    assert_eq!(first, second);
    Ok(())
}
