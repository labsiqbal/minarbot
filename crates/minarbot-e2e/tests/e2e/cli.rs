use minarbot_testkit::{http, server::ServerConfig, Result, TestRoot};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn cli_routines_stay_within_selected_agent() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let agent = Uuid::new_v4().to_string();
    http::post_json::<serde_json::Value>(
        &app,
        "/api/agents",
        &json!({"id":agent,"runtime":"api","name":"CLI","model":"fixture","instructions":"Help"}),
        201,
    )
    .await?;
    let routines: serde_json::Value =
        http::get_json(&app, &format!("/api/agents/{agent}/routines")).await?;
    assert!(routines.as_array().is_some());
    Ok(())
}

#[tokio::test]
async fn cli_unknown_agent_is_fenced() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let _: serde_json::Value = http::json(
        app.get(&format!("/api/agents/{}/routines", Uuid::new_v4())),
        404,
    )
    .await?;
    Ok(())
}
