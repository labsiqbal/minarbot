//! Sidebar sections and summaries (port of `app/sidebar.test.ts`).

use minarbot_testkit::{
    backup,
    db::Fixture,
    http::{Method, Reply as Exchange},
    model::{Reply, ScriptedModel},
    prelude::*,
    server::Gateway,
};

const GUIDE: &str = "00000000-0000-4000-8000-000000000005";

fn user_summaries(summary: &Value) -> Vec<Value> {
    summary["summaries"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| row["agent"] != GUIDE)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn sidebar_validates_unique_membership_names_ids_and_bounded_settings() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let agent = new_id();
    app.post(
        "/api/agents",
        json!({"id": agent, "runtime": "api", "name": "Fixture", "model": "fixture",
            "instructions": "Help"}),
    )
    .await?;
    let section = json!({"id": new_id(), "name": " Work ", "agents": [agent]});
    let save = |sections: Value, revision: i64| {
        app.call(
            Method::POST,
            "/api/sidebar",
            Some(json!({"id": new_id(), "revision": revision, "sections": sections})),
        )
    };
    let updated = save(json!([section]), 0).await?;
    ensure!(updated.status == 200, "save: {}", updated.body);
    ensure!(
        updated.body["sections"][0]["name"] == "Work",
        "name not trimmed: {}",
        updated.body
    );
    let with = |field: &str, value: Value| {
        let mut changed = section.clone();
        changed[field] = value;
        changed
    };
    let mut privileged = section.clone();
    privileged["privilege"] = json!("admin");
    let invalid = [
        Value::Null,
        json!({}),
        json!([null]),
        json!([with("name", json!(" "))]),
        json!([with("name", json!("a".repeat(81)))]),
        json!([with("name", json!("a\nb"))]),
        json!([with("id", json!("__proto__"))]),
        json!([section, section]),
        json!([with("agents", json!([agent, agent]))]),
        json!([section, with("id", json!(new_id()))]),
        json!([with("agents", json!(["missing"]))]),
        json!([privileged]),
        Value::Array(vec![section.clone(); 101]),
    ];
    for sections in invalid {
        let reply = save(sections.clone(), 1).await?;
        ensure!(
            reply.status == 400,
            "{sections}: HTTP {} {}",
            reply.status,
            reply.body
        );
    }
    Ok(())
}

#[tokio::test]
async fn sidebar_http_edits_retain_agents_history_through_delete_conflicts_retry_restart_and_backup(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|_| Reply::text("History retained."));
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new().model(&model)).await?;
    ensure!(
        app.get("/api/sidebar").await? == json!({"revision": 0, "sections": []}),
        "fresh sidebar"
    );
    let initial_summary = app.get("/api/sidebar/summary").await?;
    ensure!(
        user_summaries(&initial_summary).is_empty()
            && initial_summary.as_object().map(|fields| fields.len()) == Some(1),
        "fresh summary: {initial_summary}"
    );
    app.get_expect("/api/sidebar/summary?limit=1", 400).await?;
    let foreign = app
        .client()?
        .get(app.url("/api/sidebar/summary"))
        .header("Origin", "https://foreign.test")
        .send()
        .await?;
    ensure!(
        foreign.status().as_u16() == 403,
        "foreign summary: {}",
        foreign.status()
    );
    let a = app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "A", "model": "fixture",
                "instructions": "Help"}),
            201,
        )
        .await?;
    let b = app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "B", "model": "fixture",
                "instructions": "Help"}),
            201,
        )
        .await?;
    let (a_id, b_id) = (
        a["id"].as_str().ok_or("a id")?.to_owned(),
        b["id"].as_str().ok_or("b id")?.to_owned(),
    );
    let history_path = format!("/api/agents/{a_id}/turns");
    app.post_expect(
        &history_path,
        json!({"id": new_id(), "text": "Retain this conversation"}),
        202,
    )
    .await?;
    let history = app
        .wait_get(&history_path, |rows| rows[0]["status"] == "done")
        .await?;
    ensure!(history[0]["status"] == "done", "history: {history}");
    let summary = app.get("/api/sidebar/summary").await?;
    let row = summary["summaries"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["agent"] == a_id.as_str()))
        .cloned()
        .ok_or("summary for A")?;
    ensure!(
        row == json!({"agent": a_id, "preview": "History retained.", "time": history[0]["created"],
            "status": "idle", "needs_you": 0}),
        "summary row: {row}"
    );
    let plain = app
        .client()?
        .get(app.url("/api/sidebar/summary"))
        .send()
        .await?;
    ensure!(
        plain
            .headers()
            .get("cache-control")
            .and_then(|value| value.to_str().ok())
            == Some("no-store"),
        "summary cache-control: {:?}",
        plain.headers().get("cache-control")
    );

    let first = json!({"id": new_id(), "name": "Projects", "agents": [a_id, b_id]});
    let second = json!({"id": new_id(), "name": "Personal", "agents": []});
    let create = json!({"id": new_id(), "revision": 0, "sections": [first, second]});
    let initial = app.post("/api/sidebar", create.clone()).await?;
    ensure!(initial["revision"] == 1, "initial: {initial}");
    ensure!(
        app.post("/api/sidebar", create.clone()).await? == initial,
        "create replay differs"
    );
    let mut emptied = create.clone();
    emptied["sections"] = json!([]);
    app.post_expect("/api/sidebar", emptied, 409).await?;
    let mut reading = second.clone();
    reading["name"] = json!("Reading");
    reading["agents"] = json!([b_id]);
    let mut projects = first.clone();
    projects["agents"] = json!([a_id]);
    let edited = app
        .post(
            "/api/sidebar",
            json!({"id": new_id(), "revision": 1, "sections": [reading, projects]}),
        )
        .await?;
    ensure!(edited["revision"] == 2, "edited: {edited}");
    ensure!(
        edited["sections"][0]["name"] == "Reading",
        "edited: {edited}"
    );
    app.post_expect(
        "/api/sidebar",
        json!({"id": new_id(), "revision": 1, "sections": []}),
        409,
    )
    .await?;
    let mut unknown = first.clone();
    unknown["agents"] = json!([new_id()]);
    app.post_expect(
        "/api/sidebar",
        json!({"id": new_id(), "revision": 2, "sections": [unknown]}),
        404,
    )
    .await?;
    let mut duplicate = first.clone();
    duplicate["agents"] = json!([a_id, a_id]);
    app.post_expect(
        "/api/sidebar",
        json!({"id": new_id(), "revision": 2, "sections": [duplicate]}),
        400,
    )
    .await?;
    app.get_expect("/api/sidebar?path=other", 400).await?;
    let foreign_save = app
        .client()?
        .post(app.url("/api/sidebar"))
        .header("Origin", "https://foreign.test")
        .header("Content-Type", "application/json")
        .body(json!({"id": new_id(), "revision": 2, "sections": []}).to_string())
        .send()
        .await?;
    ensure!(
        foreign_save.status().as_u16() == 403,
        "foreign save: {}",
        foreign_save.status()
    );

    // A failed receipt write leaves the layout unchanged.
    app.edit_database(|db| {
        db.execute_batch(
            "CREATE TRIGGER reject_sidebar_receipt BEFORE INSERT ON sidebar_commands BEGIN SELECT RAISE(ABORT,'fixture'); END",
        )
    })
    .await?;
    let remove = json!({"id": new_id(), "revision": 2, "sections": []});
    app.post_expect("/api/sidebar", remove.clone(), 500).await?;
    ensure!(
        app.get("/api/sidebar").await? == edited,
        "failed save changed the layout"
    );
    app.edit_database(|db| db.execute_batch("DROP TRIGGER reject_sidebar_receipt"))
        .await?;
    let empty = app.post("/api/sidebar", remove).await?;
    ensure!(empty["revision"] == 3, "empty: {empty}");
    ensure!(
        app.post("/api/sidebar", create.clone()).await? == initial,
        "old receipt replay differs"
    );
    ensure!(
        app.get("/api/sidebar").await? == empty,
        "replay changed layout"
    );
    ensure!(
        app.get(&history_path).await? == history,
        "sidebar delete changed history"
    );
    ensure!(
        app.get("/api/agents").await?["agents"]
            .as_array()
            .map(Vec::len)
            == Some(2),
        "sidebar delete removed agents"
    );

    // Concurrent saves at the same revision: exactly one wins.
    let racing = |id: String| {
        app.call(
            Method::POST,
            "/api/sidebar",
            Some(json!({"id": id, "revision": 3, "sections": [first]})),
        )
    };
    let (one, two) = tokio::join!(racing(new_id()), racing(new_id()));
    let mut statuses = [one?.status, two?.status];
    statuses.sort_unstable();
    ensure!(statuses == [200, 409], "concurrent saves: {statuses:?}");
    let latest = app.get("/api/sidebar").await?;
    app.restart().await?;
    ensure!(app.get("/api/sidebar").await? == latest, "restart layout");
    ensure!(
        app.post("/api/sidebar", create.clone()).await? == initial,
        "restart replay"
    );
    ensure!(app.get(&history_path).await? == history, "restart history");
    ensure!(
        app.get("/api/sidebar/summary").await? == summary,
        "restart summary"
    );

    // A restored backup keeps the layout.
    let restored_root = backup::restore_into_new_root(&app).await?;
    app.shutdown().await?;
    drop(app);
    let restored = restored_root.start(ServerConfig::new()).await?;
    ensure!(
        restored.get("/api/sidebar").await? == latest,
        "restored layout"
    );
    Ok(())
}

#[tokio::test]
async fn sidebar_summary_bounds_entries_and_text_redacts_credentials_and_selects_latest_explicit_remote_messages(
) -> Result<()> {
    let root = TestRoot::new()?;
    let app = root
        .start(ServerConfig::new().gateway(Gateway {
            api_key: Some("saved-provider-credential".into()),
            ..Gateway::new("http://127.0.0.1:1/v1")
        }))
        .await?;
    let db = Fixture::open(app.database())?;
    let [first, second, room, third] = [new_id(), new_id(), new_id(), new_id()];
    let mut agents = vec![first.clone(), second.clone(), third.clone()];
    agents.extend((0..498).map(|_| new_id()));
    for agent in &agents {
        db.execute(
            "INSERT INTO agents(id,name,model,instructions) VALUES(?,?,'fixture','PRIVATE_INSTRUCTIONS')",
            [agent, agent],
        )?;
    }
    db.execute(
        "INSERT INTO channels(id,name,instructions) VALUES(?,'Room','Help')",
        [&room],
    )?;
    let turn = |id: &str,
                agent: &str,
                text: &str,
                reply: Option<&str>,
                created: &str,
                status: &str,
                error: Option<&str>,
                channel: Option<&str>| {
        db.execute(
            "INSERT INTO turns(id,agent,text,reply,created,status,error,channel) VALUES(?,?,?,?,?,?,?,?)",
            (id, agent, text, reply, created, status, error, channel),
        )
    };
    let credential = format!(
        "saved-provider-credential Bearer abcdefghijklmnop token=\"hide-me\" sk-abcdefghijklmnopqrstuv {}",
        "x".repeat(500)
    );
    turn(
        &new_id(),
        &first,
        "old",
        None,
        "2026-11-14T00:00:00.000Z",
        "running",
        Some("PRIVATE_ERROR"),
        None,
    )?;
    turn(
        &new_id(),
        &first,
        "unused prompt",
        Some(&credential),
        "2026-11-14T01:00:00.000Z",
        "done",
        Some("PRIVATE_ERROR"),
        None,
    )?;
    turn(
        &new_id(),
        &second,
        "Room-only secret",
        None,
        "2026-11-14T02:00:00.000Z",
        "running",
        None,
        Some(&room),
    )?;
    turn(
        &new_id(),
        &first,
        "Room newer than direct",
        None,
        "2026-11-14T03:00:00.000Z",
        "done",
        None,
        Some(&room),
    )?;
    let delivery = new_id();
    turn(
        &delivery,
        &third,
        "Delivery",
        None,
        "2026-11-14T03:00:00.000Z",
        "done",
        None,
        Some(&room),
    )?;
    db.execute_batch(
        "INSERT INTO paired_machines(environment_id,name,origin,token,local_token_id,state,status,version,agents) VALUES('machine','Peer','http://127.0.0.1:1','PRIVATE_PAIRING_TOKEN','fixture','paired','offline','fixture','[]')",
    )?;
    let remote = |id: &str,
                  machine: &str,
                  agent: &str,
                  text: &str,
                  reply: Option<&str>,
                  created: i64,
                  delivery: Option<&str>| {
        db.execute(
            "INSERT INTO remote_agent_calls(id,machine,agent,name,text,reply,created,status,delivery,depth,state) VALUES(?,?,?,'Fixture',?,?,?,'done',?,0,'result')",
            (id, machine, agent, text, reply, created, delivery),
        )
    };
    remote("a", "machine", "remote", "old", None, 1000, None)?;
    remote(
        "b",
        "machine",
        "remote",
        "latest user message",
        Some("Latest reply"),
        2000,
        None,
    )?;
    remote(
        "c",
        "machine",
        "remote",
        "AGENT_DELIVERY_SECRET",
        None,
        3000,
        Some(&delivery),
    )?;
    remote(
        "d",
        "removed-machine",
        "remote",
        "UNPAIRED_SECRET",
        None,
        3000,
        None,
    )?;
    let summary = || async {
        let reply: Exchange = app.call(Method::GET, "/api/sidebar/summary", None).await?;
        reply.expect(200)
    };
    let result = summary().await?;
    let users = user_summaries(&result);
    ensure!(users.len() == 501, "user summaries: {}", users.len());
    let listed: Vec<&str> = users[..500]
        .iter()
        .map(|row| row["agent"].as_str().unwrap_or(""))
        .collect();
    let expected: Vec<&str> = agents[..500].iter().map(String::as_str).collect();
    ensure!(listed == expected, "summary order differs");
    ensure!(
        result["summaries"][500]
            == json!({"agent": GUIDE, "preview": "", "time": null, "status": "idle", "needs_you": 0}),
        "guide row: {}",
        result["summaries"][500]
    );
    ensure!(
        users[1]
            == json!({"agent": second, "preview": "", "time": null, "status": "idle", "needs_you": 0}),
        "room-only agent: {}",
        users[1]
    );
    ensure!(users[0]["status"] == "running", "first: {}", users[0]);
    ensure!(
        users[0]["preview"]
            .as_str()
            .map(|text| text.chars().count())
            == Some(180),
        "preview length: {}",
        users[0]["preview"]
    );
    ensure!(
        users[0]["time"] == "2026-11-14T01:00:00.000Z",
        "first time: {}",
        users[0]
    );
    let all = result["summaries"].as_array().cloned().unwrap_or_default();
    ensure!(
        all.last()
            == Some(
                &json!({"agent": "remote@machine", "preview": "Latest reply",
                "time": "1970-01-01T00:00:02.000Z", "status": "done", "needs_you": 0})
            ),
        "remote row: {:?}",
        all.last()
    );
    let mut keys: Vec<&String> = users[0]
        .as_object()
        .map(|row| row.keys().collect())
        .unwrap_or_default();
    keys.sort();
    ensure!(
        keys == ["agent", "needs_you", "preview", "status", "time"],
        "summary keys: {keys:?}"
    );
    let text = result.to_string();
    for secret in [
        "PRIVATE_",
        "hide-me",
        "abcdefghijklmnop",
        "saved-provider-credential",
        "AGENT_DELIVERY_SECRET",
        "UNPAIRED_SECRET",
        "unused prompt",
    ] {
        ensure!(!text.contains(secret), "summary leaks {secret}");
    }
    for index in 0..501 {
        remote(
            &format!("remote-{index}"),
            "machine",
            &format!("r-{index}"),
            "hello",
            None,
            4000,
            None,
        )?;
    }
    ensure!(
        user_summaries(&summary().await?).len() == 1000,
        "summary is not bounded at 1000"
    );
    Ok(())
}
