use minarbot_testkit::{fixtures::SqliteInspector, http, server::ServerConfig, Result, TestRoot};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn agent_creation_persists_profile() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let id = Uuid::new_v4().to_string();
    let profile: serde_json::Value = http::post_json(&app, "/api/agents", &json!({"id":id,"runtime":"api","name":"Agent baru","model":"fixture","instructions":"Help with research","threadLimit":2}), 201).await?;
    assert_eq!(profile["id"], id);
    assert_eq!(profile["name"], "Agent baru");
    let restored: serde_json::Value = http::get_json(&app, "/api/agents").await?;
    assert!(restored["agents"]
        .as_array()
        .is_some_and(|rows| rows.iter().any(|row| row["id"] == id)));
    let db = SqliteInspector::open(root.database())?;
    assert!(db.table_count("agents")? >= 1);
    Ok(())
}

#[tokio::test]
async fn malformed_agent_creation_is_rejected() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let _: serde_json::Value = http::post_json(&app, "/api/agents", &json!({"id":Uuid::new_v4().to_string(),"name":"Invalid","model":"fixture","instructions":"Help","threadLimit":0}), 400).await?;
    Ok(())
}
