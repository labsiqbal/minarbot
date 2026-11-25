//! Shared data tables over agent tools and HTTP (port of
//! `app/shared-data-http.test.ts`).

use minarbot_testkit::{
    backup,
    model::{Reply, ScriptedModel},
    prelude::*,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// Each user message is SQL for `query_data` (or `list` for
/// `list_data_tables`); the reply is the tool result. With `fail` set, the
/// step after the next tool result fails once with HTTP 503.
fn data_model(model: &ScriptedModel, fail: Arc<AtomicBool>) {
    model.respond(move |request| {
        let prompt = request.last_user_text();
        let result = request.tool_results().last().cloned();
        match result {
            Some(_) if fail.swap(false, Ordering::SeqCst) => Reply::status(503, json!({})),
            Some(result) => Reply::text(result["content"].as_str().unwrap_or("").to_owned()),
            None if prompt == "list" => Reply::tool_call("sql-call", "list_data_tables", json!({})),
            None => Reply::tool_call("sql-call", "query_data", json!({"sql": prompt})),
        }
    });
}

async fn query(app: &Server, path: &str, text: &str) -> Result<Value> {
    let id = new_id();
    app.post_expect(path, json!({"id": id, "text": text}), 202)
        .await?;
    let done = app.wait_status(path, &id, "done").await?;
    Ok(serde_json::from_str(
        done["reply"].as_str().ok_or("reply")?,
    )?)
}

#[tokio::test]
async fn shared_data_agent_tools_journal_once_across_resume_user_reads_drop_and_restored_workspace_retain_data(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    let fail = Arc::new(AtomicBool::new(false));
    data_model(&model, fail.clone());
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new().model(&model)).await?;
    let a = app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Creator", "model": "fixture",
                "instructions": "Use SQL"}),
            201,
        )
        .await?;
    let b = app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Writer", "model": "fixture",
                "instructions": "Use SQL"}),
            201,
        )
        .await?;
    let a_id = a["id"].as_str().ok_or("a id")?.to_owned();
    let ap = format!("/api/agents/{a_id}/turns");
    let bp = format!("/api/agents/{}/turns", b["id"].as_str().ok_or("b id")?);
    ensure!(
        query(
            &app,
            &ap,
            "CREATE TABLE things(id INTEGER PRIMARY KEY, text TEXT)"
        )
        .await?["revision"]
            == 1,
        "create revision"
    );

    // The insert runs once: the provider fails after it, and resume reuses
    // the journaled result after a restart.
    fail.store(true, Ordering::SeqCst);
    let insertion =
        json!({"id": new_id(), "text": "INSERT INTO things(text) VALUES ('saved once')"});
    let insertion_id = insertion["id"].as_str().ok_or("insertion id")?.to_owned();
    app.post_expect(&bp, insertion.clone(), 202).await?;
    let failed = app.wait_status(&bp, &insertion_id, "failed").await?;
    app.restart().await?;
    app.post_expect(&bp, insertion, 202).await?;
    app.post_expect(
        &format!("{bp}/{insertion_id}/resume"),
        json!({"id": new_id(), "revision": failed["revision"], "acknowledgeUncertain": true}),
        200,
    )
    .await?;
    let resumed = app.wait_status(&bp, &insertion_id, "done").await?;
    let changes: Value = serde_json::from_str(resumed["reply"].as_str().ok_or("reply")?)?;
    ensure!(changes["changes"] == 1, "insert result: {changes}");
    ensure!(
        query(&app, &ap, "SELECT * FROM things").await?["rows"] == json!([[1, "saved once"]]),
        "rows after resume"
    );
    let dropped = query(&app, &bp, "DROP TABLE things").await?;
    ensure!(
        dropped["error"]
            .as_str()
            .is_some_and(|error| error.contains("authorized")),
        "foreign drop: {dropped}"
    );
    ensure!(
        query(&app, &bp, "list").await?["tables"][0]["owner"] == a_id.as_str(),
        "table owner"
    );

    // The owner reads read-only SQL and drops with receipts.
    let listing = app.get("/api/data").await?;
    ensure!(
        listing["tables"][0]["name"] == "things",
        "listing: {listing}"
    );
    app.post_expect(
        "/api/data/query",
        json!({"id": new_id(), "revision": listing["revision"], "sql": "DELETE FROM things"}),
        400,
    )
    .await?;
    let read =
        json!({"id": new_id(), "revision": listing["revision"], "sql": "SELECT * FROM things"});
    let viewed = app.post("/api/data/query", read.clone()).await?;
    ensure!(
        viewed["rows"] == json!([[1, "saved once"]]),
        "viewed: {viewed}"
    );
    ensure!(
        app.post("/api/data/query", read).await? == viewed,
        "read replay differs"
    );
    // The backup includes the embedded database image and ownership metadata.
    let backup_bytes = backup::download(&app).await?;
    let drop_command = json!({"id": new_id(), "revision": listing["revision"], "table": "things"});
    let removed = app.post("/api/data/drop", drop_command.clone()).await?;
    ensure!(
        app.post("/api/data/drop", drop_command.clone()).await? == removed,
        "drop replay differs"
    );
    ensure!(
        app.get("/api/data").await?["tables"] == json!([]),
        "tables after drop"
    );
    let mut again = drop_command.clone();
    again["id"] = json!(new_id());
    app.post_expect("/api/data/drop", again, 409).await?;

    // A restored workspace keeps the table and its owner.
    app.shutdown().await?;
    drop(app);
    let restored_root = TestRoot::new()?;
    let file = restored_root.path().join("backup.sqlite");
    std::fs::write(&file, backup_bytes)?;
    backup::restore_workspace(&file, &restored_root.database()).await?;
    let restored = restored_root
        .start(ServerConfig::new().model(&model))
        .await?;
    let restored_list = restored.get("/api/data").await?;
    ensure!(
        restored_list["tables"][0]["owner"] == a_id.as_str(),
        "restored owner: {restored_list}"
    );
    ensure!(
        restored
            .post(
                "/api/data/query",
                json!({"id": new_id(), "revision": restored_list["revision"],
                    "sql": "SELECT * FROM things"}),
            )
            .await?["rows"]
            == json!([[1, "saved once"]]),
        "restored rows"
    );
    restored
        .post_expect(
            "/api/data/drop",
            json!({"id": new_id(), "revision": restored_list["revision"],
                "table": "minarbot_owners"}),
            400,
        )
        .await?;
    Ok(())
}
