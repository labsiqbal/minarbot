use minarbot_testkit::{http, server::ServerConfig, Result, TestRoot};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn question_answer_route_rejects_unknown_turn() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let agent = Uuid::new_v4().to_string();
    http::post_json::<serde_json::Value>(&app, "/api/agents", &json!({"id":agent,"runtime":"api","name":"Questions","model":"fixture","instructions":"Help"}), 201).await?;
    let _: serde_json::Value = http::post_json(
        &app,
        &format!("/api/agents/{agent}/turns/no-turn/questions/no-question/answer"),
        &json!({"id":Uuid::new_v4().to_string(),"text":"Yes"}),
        404,
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn question_answer_receipt_requires_text() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let agent = Uuid::new_v4().to_string();
    http::post_json::<serde_json::Value>(&app, "/api/agents", &json!({"id":agent,"runtime":"api","name":"Questions","model":"fixture","instructions":"Help"}), 201).await?;
    let _: serde_json::Value = http::post_json(
        &app,
        &format!("/api/agents/{agent}/turns/no-turn/questions/no-question/answer"),
        &json!({"id":Uuid::new_v4().to_string()}),
        404,
    )
    .await?;
    Ok(())
}
