use super::{id, json};
use crate::server::{Result, TestRoot};
use reqwest::header;
use rusqlite::{params, Connection};
use rustix::process::Signal;
use serde_json::json;
use std::os::unix::fs::symlink;
use std::time::Duration;

#[tokio::test]
async fn rejects_second_server_on_same_database() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(json!({})).await?;
    let error = root
        .start(json!({}))
        .await
        .err()
        .ok_or("second server started")?;
    assert!(error.to_string().contains("Another canonical server"));
    app.get("/api/agents").send().await?.error_for_status()?;
    assert!(app.terminate(Signal::TERM).await?.success());
    Ok(())
}

#[tokio::test]
async fn create_retries_return_current_records_and_persist_edit_receipts() -> Result<()> {
    let root = TestRoot::new()?;
    let agent = id();
    let room = id();
    let original = json!({"id":agent,"runtime":"api","name":"Original agent","model":"fixture","instructions":"Original instructions."});
    let channel = json!({"id":room,"name":"Original room","instructions":"Original purpose.","lead":agent,"members":[agent]});
    let app = root.start(json!({})).await?;
    assert_eq!(
        json(app.post("/api/agents").json(&original), 201).await?["revision"],
        1
    );
    let created = json(app.post("/api/channels").json(&channel), 201).await?;
    assert_eq!(created["revision"], 1);
    assert_eq!(created["archived"], 0);
    assert!(app.terminate(Signal::TERM).await?.success());

    Connection::open(root.database())?.execute(
        "INSERT INTO turns(id,agent,channel,text,reply,status,created) VALUES (?,?,?,'Request','Result','done','2026-01-02T03:04:05.000Z')",
        params![id(), agent, room],
    )?;
    let app = root.start(json!({})).await?;
    let saved = json(app.post("/api/channels").json(&channel), 200).await?;
    assert_eq!(saved["activity"], 1);
    assert_eq!(saved["read_position"], 0);
    json(
        app.post(&format!("/api/channels/{room}/read"))
            .json(&json!({"id":id(),"position":1})),
        200,
    )
    .await?;
    let mut archive = channel.clone();
    archive["id"] = json!(id());
    archive["revision"] = json!(1);
    archive["archived"] = json!(true);
    archive["name"] = json!("Archived room");
    json(
        app.post(&format!("/api/channels/{room}/edit"))
            .json(&archive),
        200,
    )
    .await?;
    let saved = json(app.post("/api/channels").json(&channel), 200).await?;
    assert_eq!(saved["archived"], 1);
    assert_eq!(saved["name"], "Archived room");

    let path = format!("/api/agents/{agent}/edit");
    let mut first_edit = original.clone();
    first_edit
        .as_object_mut()
        .ok_or("missing edit fields")?
        .remove("runtime");
    first_edit["id"] = json!(id());
    first_edit["revision"] = json!(1);
    first_edit["name"] = json!("First edit");
    let receipt = json(app.post(&path).json(&first_edit), 200).await?;
    let mut second_edit = first_edit.clone();
    second_edit["id"] = json!(id());
    second_edit["revision"] = json!(2);
    second_edit["name"] = json!("Second edit");
    json(app.post(&path).json(&second_edit), 200).await?;
    assert!(app.terminate(Signal::TERM).await?.success());

    let app = root.start(json!({})).await?;
    let read = json(app.post("/api/channels").json(&channel), 200).await?;
    assert_eq!(read["activity"], 1);
    assert_eq!(read["read_position"], 1);
    assert_eq!(json(app.post(&path).json(&first_edit), 200).await?, receipt);
    let retried = json(app.post("/api/agents").json(&original), 200).await?;
    assert_eq!(retried["id"], agent);
    assert_eq!(retried["name"], "Second edit");
    assert_eq!(retried["revision"], 3);
    assert_eq!(
        json(app.get("/api/agents"), 200).await?["agents"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    Ok(())
}

#[tokio::test]
async fn fork_context_preserves_bounded_transcript_and_receipt_after_restart() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(json!({})).await?;
    drop(app);
    let agent = id();
    let cutoff = id();
    let created_at = "2026-01-02T03:04:05.000Z";
    let mut rows = Vec::new();
    {
        let db = Connection::open(root.database())?;
        db.execute("INSERT INTO agents(id,name,model,instructions) VALUES(?,'Fork fixture','fixture','Help')", [&agent])?;
        let mut insert = db.prepare(
            "INSERT INTO turns(id,agent,text,reply,status,created) VALUES(?,?,?,?,'done',?)",
        )?;
        for index in 0..42 {
            let turn = if index == 41 { cutoff.clone() } else { id() };
            let text = format!("Question {index}{}", "x".repeat(17000));
            let reply = format!("Answer {index}{}", "y".repeat(25000));
            insert.execute(params![turn, agent, text, reply, created_at])?;
            rows.push(json!({"id":turn,"agent":agent,"text":&text[..8505],"reply":&reply[..12504],"status":"done","created":created_at,"guidance":"[]"}));
        }
        db.execute(
            "INSERT INTO compactions VALUES(?,?,41,'Prior summary')",
            params![format!("agents:{agent}"), id()],
        )?;
    }
    let app = root.start(json!({})).await?;
    let path = format!("/api/agents/{agent}/turns/{cutoff}/fork");
    let command = json!({"id":id()});
    let created = json(app.post(&path).json(&command), 200).await?;
    let fork = created["id"].as_str().ok_or("fork ID missing")?;
    let context = format!("/api/threads/{fork}/fork-context");
    assert_eq!(
        json(app.get(&context), 200).await?,
        json!({"summary":"Prior summary","inherited":null,"turns":&rows[36..],"truncated":true})
    );
    assert_eq!(json(app.post(&path).json(&command), 200).await?, created);
    assert!(app.terminate(Signal::TERM).await?.success());
    let app = root.start(json!({})).await?;
    assert_eq!(json(app.post(&path).json(&command), 200).await?, created);
    assert_eq!(
        app.get(&context)
            .bearer_auth("fixture")
            .send()
            .await?
            .status(),
        403
    );
    Ok(())
}

#[tokio::test]
async fn host_metrics_cache_and_redact_storage_paths() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(json!({})).await?;
    Connection::open(root.database())?.execute_batch(
        "CREATE TABLE fixture(bytes BLOB); INSERT INTO fixture VALUES (zeroblob(32768)); DELETE FROM fixture;",
    )?;
    symlink(
        "/missing-private-key-target",
        root.path().join("state.sqlite.mcp-key"),
    )?;
    let body = json(app.get("/api/host-metrics"), 200).await?;
    assert_eq!(body["cpu"]["scope"], "host");
    assert!(body["cpu"]["percent"].is_null());
    assert!(body["process"]["cpuPercent"].is_null());
    assert!(
        body["process"]["rss"]
            .as_u64()
            .ok_or("missing process RSS")?
            > 0
    );
    let files = body["storage"]["files"]
        .as_array()
        .ok_or("missing storage files")?;
    assert!(files
        .iter()
        .any(|file| file["kind"] == "mcp-key" && file["status"] == "not-regular"));
    assert!(files
        .iter()
        .any(|file| file["kind"] == "rollback-journal" && file["status"] == "missing"));
    let size = |key: &str| -> Result<u64> {
        Ok(body["storage"][key]
            .as_str()
            .ok_or("missing storage size")?
            .parse()?)
    };
    let reusable = size("reusableBytes")?;
    assert!(reusable > 0);
    assert!(size("databaseBytes")? >= reusable);
    let encoded = body.to_string();
    assert!(!encoded.contains(root.path().to_str().ok_or("invalid fixture path")?));
    assert!(!encoded.contains("/missing"));
    assert_eq!(json(app.get("/api/host-metrics"), 200).await?, body);
    assert_eq!(
        app.get("/api/host-metrics?path=/etc/passwd")
            .send()
            .await?
            .status(),
        400
    );
    assert_eq!(
        app.get("/api/host-metrics")
            .header(header::ORIGIN, "https://untrusted.example")
            .send()
            .await?
            .status(),
        403
    );

    tokio::time::sleep(Duration::from_millis(2100)).await;
    let second = json(app.get("/api/host-metrics"), 200).await?;
    assert!(
        second["cpu"]["intervalMs"]
            .as_u64()
            .ok_or("missing sample interval")?
            >= 2000
    );
    let percent = &second["cpu"]["percent"];
    assert!(
        percent.is_null()
            || percent
                .as_f64()
                .is_some_and(|value| (0.0..=100.0).contains(&value))
    );
    #[cfg(target_os = "linux")]
    assert!(
        second["process"]["cpuPercent"]
            .as_f64()
            .ok_or("missing process CPU percentage")?
            >= 0.0
    );
    #[cfg(not(target_os = "linux"))]
    assert!(second["process"]["cpuPercent"].is_null());
    Ok(())
}

#[tokio::test]
async fn channel_read_positions_survive_restart_and_do_not_hide_late_replies() -> Result<()> {
    use super::until;
    use crate::loopback::Loopback;
    use axum::{routing::post, Json, Router};
    use serde_json::Value;
    use std::sync::Arc;
    use tokio::sync::Notify;

    let root = TestRoot::new()?;
    let arrived = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider = Loopback::start(Router::new().route("/v1/chat/completions", post({
        let arrived = arrived.clone();
        let release = release.clone();
        move |Json(request): Json<Value>| {
            let arrived = arrived.clone();
            let release = release.clone();
            async move {
                let late = request["messages"].as_array().and_then(|messages| messages.last())
                    .is_some_and(|message| message["content"] == "Reply after read");
                if !late {
                    std::future::pending::<()>().await;
                }
                arrived.notify_one();
                release.notified().await;
                Json(json!({"choices":[{"finish_reason":"stop","message":{"content":"Late reply"}}]}))
            }
        }
    }))).await?;
    let options = json!({"baseURL":provider.base_url()});
    let app = root.start(options.clone()).await?;
    let agent = id();
    let room = id();
    json(app.post("/api/agents").json(&json!({"runtime":"api","id":agent,"name":"Reader","model":"fixture","instructions":"Help."})), 201).await?;
    let created = json(app.post("/api/channels").json(&json!({"id":room,"name":"Read state","instructions":"Help.","members":[agent],"lead":agent})), 201).await?;
    assert_eq!(created["activity"], 0);
    assert_eq!(created["read_position"], 0);
    let path = format!("/api/channels/{room}");
    let read_path = format!("{path}/read");
    let turns = format!("{path}/turns");
    let turn = id();
    json(
        app.post(&turns)
            .json(&json!({"id":turn,"text":"Queued work"})),
        202,
    )
    .await?;

    let state = async |app: &crate::server::Server<'_>| -> Result<Value> {
        let rows = json(app.get("/api/channels"), 200).await?;
        Ok(rows
            .as_array()
            .ok_or("expected channels")?
            .iter()
            .find(|row| row["id"] == room)
            .ok_or("room missing")?
            .clone())
    };
    let observed = state(&app).await?;
    let activity = observed["activity"].as_i64().ok_or("missing activity")?;
    assert!(activity > 0);
    let read = json!({"id":id(),"position":activity});
    assert_eq!(
        json(app.post(&read_path).json(&read), 200).await?["read_position"],
        activity
    );
    json(
        app.post(&format!("{turns}/{turn}/cancel"))
            .json(&json!({"id":id()})),
        200,
    )
    .await?;
    let changed = state(&app).await?;
    let changed_activity = changed["activity"]
        .as_i64()
        .ok_or("missing changed activity")?;
    assert!(
        changed_activity
            > changed["read_position"]
                .as_i64()
                .ok_or("missing read position")?
    );
    assert_eq!(
        json(app.post(&read_path).json(&read), 200).await?["read_position"],
        activity
    );
    for invalid in [json!(changed_activity + 1), json!(-1), json!(1.5)] {
        json(
            app.post(&read_path)
                .json(&json!({"id":id(),"position":invalid})),
            400,
        )
        .await?;
    }
    for position in [changed_activity, 0] {
        json(
            app.post(&read_path)
                .json(&json!({"id":id(),"position":position})),
            200,
        )
        .await?;
    }
    assert_eq!(state(&app).await?["read_position"], changed_activity);
    json(
        app.post(&format!("/api/agents/{agent}/turns"))
            .json(&json!({"id":id(),"text":"Direct work"})),
        202,
    )
    .await?;
    assert_eq!(state(&app).await?["activity"], changed_activity);
    assert!(app.terminate(Signal::TERM).await?.success());

    let app = root.start(options).await?;
    let reopened = state(&app).await?;
    assert_eq!(reopened["read_position"], changed_activity);
    assert_eq!(reopened["activity"], changed_activity);
    json(
        app.post(&format!("/api/channels/{}/read", id()))
            .json(&json!({"id":id(),"position":0})),
        404,
    )
    .await?;
    let late = id();
    json(
        app.post(&turns)
            .json(&json!({"id":late,"text":"Reply after read"})),
        202,
    )
    .await?;
    tokio::time::timeout(crate::server::WAIT, arrived.notified()).await?;
    let before = state(&app).await?;
    let read = json!({"id":id(),"position":before["activity"]});
    json(app.post(&read_path).json(&read), 200).await?;
    release.notify_one();
    let done = until(async || {
        let rows = json(app.get(&turns), 200).await?;
        let row = rows
            .as_array()
            .ok_or("missing turns")?
            .iter()
            .find(|row| row["id"] == late && row["status"] == "done")
            .cloned();
        Ok(row)
    })
    .await?;
    assert_eq!(done["reply"], "Late reply");
    json(app.post(&read_path).json(&read), 200).await?;
    let after = state(&app).await?;
    assert!(
        after["activity"].as_i64().ok_or("missing activity")?
            > after["read_position"]
                .as_i64()
                .ok_or("missing read position")?
    );
    assert_eq!(after["read_position"], before["activity"]);
    Ok(())
}
