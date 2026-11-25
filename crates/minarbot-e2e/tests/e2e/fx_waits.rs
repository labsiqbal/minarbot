//! Event waits (port of `app/turn-waits.test.ts`).

use crate::fx_support::{
    after_pause, api_agent, gated, id, pause_reply_for, row, rows, settle, tool_json,
};
use minarbot_testkit::{
    db::{Fixture, Inspector},
    gated::{Answer, GatedModel},
    http::{Method, SplitRequest},
    model::ModelRequest,
    prelude::*,
    server::server_binary,
    signing::hmac_sha256_hex,
    Server, FIXED_CLOCK,
};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

/// Seconds of the server's fixed clock, for `X-Minarbot-Time`.
fn now_seconds() -> String {
    (FIXED_CLOCK / 1000).to_string()
}

fn path_of(url: &str) -> Result<String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("connection url {url}"))?;
    Ok(rest[rest.find('/').ok_or("url without path")?..].to_owned())
}

/// POST a signed event to a wait connection; returns status and body.
async fn deliver(
    app: &Server,
    connection: &Value,
    secret: &str,
    delivery: &str,
    body: &str,
) -> Result<(u16, Value)> {
    let time = now_seconds();
    let signature = hmac_sha256_hex(
        secret.as_bytes(),
        format!("{time}\n{delivery}\n{body}").as_bytes(),
    );
    let response = app
        .client()?
        .post(app.url(&path_of(connection["url"].as_str().unwrap_or_default())?))
        .header("Content-Type", "application/json")
        .header("X-Minarbot-Time", time)
        .header("X-Minarbot-Id", delivery)
        .header("X-Minarbot-Signature", signature)
        .body(body.to_owned())
        .send()
        .await?;
    let status = response.status().as_u16();
    let text = response.text().await?;
    Ok((
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    ))
}

async fn send(app: &Server, connection: &Value, delivery: &str, expected: u16) -> Result<Value> {
    let secret = connection["secret"].as_str().unwrap_or_default();
    let body = json!({"event": "ready", "payload": "synthetic"}).to_string();
    let (status, value) = deliver(app, connection, secret, delivery, &body).await?;
    ensure!(
        status == expected,
        "event delivery: expected {expected}, got {status}: {value}"
    );
    Ok(value)
}

/// Start a turn whose model asks to wait; returns the waiting row.
async fn waiting(app: &Server, path: &str) -> Result<Value> {
    let turn = new_id();
    app.post(path, json!({"id": turn, "text": turn})).await?;
    app.wait_row(path, &turn, |row| {
        row["events"]
            .as_array()
            .is_some_and(|events| !events.is_empty())
    })
    .await
}

/// Resume, retrying only while the server says the stopped harness turn has
/// not exited yet ("Try after the local stop completes"). With B1 the
/// harness is still finishing its extra step when the test pauses.
async fn resume_when_stopped(app: &Server, path: &str, body: Value) -> Result<Value> {
    until(format!("resume {path}"), || async {
        let reply = app.call(Method::POST, path, Some(body.clone())).await?;
        if reply.status == 409
            && reply.body["error"]
                .as_str()
                .is_some_and(|error| error.starts_with("Previous request has not stopped yet"))
        {
            return Ok(None);
        }
        reply.ok().map(Some)
    })
    .await
}

fn last_tool_status(request: &ModelRequest) -> Value {
    request
        .tool_results()
        .last()
        .map(tool_json)
        .map_or(Value::Null, |result| result["status"].clone())
}

#[tokio::test]
async fn agent_event_waits_release_workers_survive_restart_honor_pause_and_accept_once(
) -> Result<()> {
    let continuations = Arc::new(AtomicUsize::new(0));
    let invalid = Arc::new(AtomicBool::new(false));
    let model = GatedModel::start().await?;
    let (seen, bad) = (continuations.clone(), invalid.clone());
    model.respond(move |request| {
        if after_pause(request) {
            return pause_reply_for(request);
        }
        let received = !request.tool_results().is_empty();
        let ordinary = request.last_user_text() == "ordinary";
        if received {
            seen.fetch_add(1, Ordering::SeqCst);
        }
        if received || ordinary {
            Answer::text("Finished")
        } else {
            Answer::tool_id(
                "wait",
                "wait_for_event",
                json!({"event": "ready", "timeoutSeconds": if bad.load(Ordering::SeqCst) { 0 } else { 3600 }}),
            )
        }
    });
    let continued = || continuations.load(Ordering::SeqCst);
    let first_root = TestRoot::new()?;
    let config = gated(&model);
    let mut app = first_root.start(config.clone()).await?;
    let a = id(&api_agent(&app, "A", "fixture").await?)?;
    let direct = format!("/api/agents/{a}/turns");
    let first = waiting(&app, &direct).await?;
    let first_id = id(&first)?;
    let wait_path = format!("/api/turn-waits/{}", id(&first["events"][0])?);
    let connection = app.get(&wait_path).await?;
    let secret = connection["secret"].as_str().ok_or("secret")?.to_owned();
    for (name, value) in [
        ("Origin", "https://foreign.test"),
        ("Sec-Fetch-Site", "cross-site"),
    ] {
        let denied = app
            .client()?
            .get(app.url(&path_of(connection["url"].as_str().unwrap_or_default())?))
            .header(name, value)
            .send()
            .await?;
        ensure!(
            denied.status().as_u16() == 403,
            "{name}: {}",
            denied.status()
        );
        ensure!(
            !denied.text().await?.contains(&secret),
            "{name} leaked the secret"
        );
    }
    let mut wrong = connection.clone();
    wrong["secret"] = json!("wrong");
    send(&app, &wrong, &new_id(), 401).await?;
    ensure!(
        first["status"] == "queued" && first["paused"] == 0,
        "waiting row {first}"
    );
    ensure!(
        !first.to_string().contains(&secret),
        "turn row leaked the secret"
    );
    let other = new_id();
    app.post(&direct, json!({"id": other, "text": "ordinary"}))
        .await?;
    settle(&app, &direct, &other, "done").await?;
    app.restart().await?;
    let mut moved = connection.clone();
    moved["url"] = json!(app.url(&path_of(connection["url"].as_str().unwrap_or_default())?));
    ensure!(
        app.get(&wait_path).await? == moved,
        "connection after restart"
    );
    let connection = moved;
    app.post(
        &format!("{direct}/{first_id}/pause"),
        json!({"id": new_id(), "revision": first["revision"]}),
    )
    .await?;
    let delivery = new_id();
    let receipt = send(&app, &connection, &delivery, 200).await?;
    ensure!(
        send(&app, &connection, &delivery, 200).await? == receipt,
        "delivery replay"
    );
    let still = row(&app, &direct, &first_id).await?;
    ensure!(still["paused"] == 1, "pause lost: {still}");
    ensure!(continued() == 0, "paused turn continued");
    app.post(
        &format!("{direct}/{first_id}/resume"),
        json!({"id": new_id(), "revision": still["revision"]}),
    )
    .await?;
    settle(&app, &direct, &first_id, "done").await?;
    ensure!(continued() == 1, "continuations {}", continued());
    ensure!(
        !model
            .requests()
            .iter()
            .any(|request| request.body.to_string().contains(&secret)),
        "secret reached the model"
    );
    app.restart().await?;
    ensure!(
        send(&app, &connection, &delivery, 200).await? == receipt,
        "receipt after restart"
    );
    ensure!(continued() == 1, "replay continued the turn");
    let side = id(&app
        .post(
            &format!("/api/agents/{a}/threads"),
            json!({"id": new_id(), "name": "Side"}),
        )
        .await?)?;
    let side_path = format!("/api/threads/{side}/turns");
    let side_turn = waiting(&app, &side_path).await?;
    let side_id = id(&side_turn)?;
    ensure!(
        !rows(&app, &direct)
            .await?
            .iter()
            .any(|row| row["id"] == side_id.as_str()),
        "side turn in main chat"
    );
    let side_connection = app
        .get(&format!("/api/turn-waits/{}", id(&side_turn["events"][0])?))
        .await?;
    app.post(
        &format!("{side_path}/{side_id}/cancel"),
        json!({"id": new_id()}),
    )
    .await?;
    send(&app, &side_connection, &new_id(), 409).await?;
    let channel = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Events", "instructions": "Help", "lead": a, "members": [a]}),
        )
        .await?)?;
    let channel_path = format!("/api/channels/{channel}/turns");
    let channel_turn = waiting(&app, &channel_path).await?;
    let channel_connection = app
        .get(&format!(
            "/api/turn-waits/{}",
            id(&channel_turn["events"][0])?
        ))
        .await?;
    app.post(
        &format!("/api/channels/{channel}/edit"),
        json!({"id": new_id(), "revision": 1, "name": "Events", "instructions": "Help", "members": [a],
            "lead": a, "archived": true}),
    )
    .await?;
    send(&app, &channel_connection, &new_id(), 409).await?;
    app.get_expect(
        &path_of(channel_connection["url"].as_str().unwrap_or_default())?,
        404,
    )
    .await?;
    let pending = waiting(&app, &direct).await?;
    let pending_id = id(&pending)?;
    let pending_connection = app
        .get(&format!("/api/turn-waits/{}", id(&pending["events"][0])?))
        .await?;
    let paused = app
        .post(
            &format!("{direct}/{pending_id}/pause"),
            json!({"id": new_id(), "revision": pending["revision"]}),
        )
        .await?;
    resume_when_stopped(
        &app,
        &format!("{direct}/{pending_id}/resume"),
        json!({"id": new_id(), "revision": paused["revision"]}),
    )
    .await?;
    app.post_expect(
        &format!("{direct}/{pending_id}/reassign"),
        json!({"id": new_id(), "revision": paused["revision"].as_i64().unwrap_or(0) + 1, "recipient": a}),
        409,
    )
    .await?;
    let backup = app
        .request(Method::POST, "/api/backup")?
        .header("Content-Type", "application/json")
        .body(json!({"id": new_id(), "confirmed": true}).to_string())
        .send()
        .await?;
    ensure!(
        backup.status().as_u16() == 200,
        "backup {}",
        backup.status()
    );
    let archive = first_root.path().join("backup.sqlite");
    std::fs::write(&archive, backup.bytes().await?)?;
    app.shutdown().await?;
    drop(app);
    let restored_root = TestRoot::new()?;
    restore_workspace(&first_root, &archive, &restored_root.database())?;
    let restored = restored_root.start(config.clone()).await?;
    restored
        .get_expect(
            &path_of(pending_connection["url"].as_str().unwrap_or_default())?,
            404,
        )
        .await?;
    send(&restored, &pending_connection, &new_id(), 409).await?;
    let recovered = row(&restored, &direct, &pending_id).await?;
    ensure!(
        recovered["events"][0]["status"] == "closed",
        "restored wait {}",
        recovered["events"]
    );
    restored
        .post(
            &format!("{direct}/{pending_id}/resume"),
            json!({"id": new_id(), "revision": recovered["revision"], "acknowledgeUncertain": true}),
        )
        .await?;
    let resumed = restored
        .wait_row(&direct, &pending_id, |row| {
            row["status"] == "done" || row["status"] == "failed"
        })
        .await?;
    ensure!(
        resumed["status"] == "done",
        "resumed restore: {}",
        resumed["error"]
    );
    let last = model.requests().last().cloned().ok_or("no model request")?;
    ensure!(
        last_tool_status(&last) == "closed",
        "restored wait result {}",
        last.body["messages"]
    );
    send(&restored, &pending_connection, &new_id(), 409).await?;
    drop(restored);
    let mut app = first_root.start(config.clone()).await?;
    let expiring = waiting(&app, &direct).await?;
    let expiring_id = id(&expiring)?;
    let target = expiring_id.clone();
    app.edit_database(move |db| {
        db.execute("UPDATE turn_waits SET deadline=0 WHERE turn=?", [&target])?;
        Ok(())
    })
    .await?;
    let expired = settle(&app, &direct, &expiring_id, "failed").await?;
    ensure!(
        expired["events"][0]["status"] == "expired",
        "expired wait {}",
        expired["events"]
    );
    let target = expiring_id.clone();
    app.edit_database(move |db| {
        db.execute("DELETE FROM flow_tool_calls WHERE turn=?", [&target])?;
        Ok(())
    })
    .await?;
    app.restart().await?;
    app.shutdown().await?;
    let repaired: i64 = Inspector::open(first_root.database())?.scalar(
        "SELECT count(*) FROM flow_tool_calls WHERE turn=?",
        [&expiring_id],
    )?;
    ensure!(repaired == 1, "repaired journal rows {repaired}");
    app.start_stopped(config).await?;
    app.post(
        &format!("{direct}/{expiring_id}/resume"),
        json!({"id": new_id(), "revision": expired["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    let resumed = app
        .wait_row(&direct, &expiring_id, |row| {
            row["status"] == "done" || row["status"] == "failed"
        })
        .await?;
    ensure!(
        resumed["status"] == "done",
        "resumed expiry: {}",
        resumed["error"]
    );
    let last = model.requests().last().cloned().ok_or("no model request")?;
    ensure!(
        last_tool_status(&last) == "expired",
        "expired wait result {}",
        last.body["messages"]
    );
    invalid.store(true, Ordering::SeqCst);
    let bad = new_id();
    app.post(&direct, json!({"id": bad, "text": bad})).await?;
    // #87: the agent loop failed the turn on a rejected wait; over MCP the
    // harness gets the 400 and the turn goes on.
    settle(&app, &direct, &bad, "done").await?;
    let rejected = model
        .requests()
        .iter()
        .rev()
        .find(|request| request.last_user_text() == bad)
        .and_then(|request| request.tool_results().last().map(tool_json))
        .ok_or("no rejected wait result")?;
    ensure!(
        rejected["error"]
            .as_str()
            .is_some_and(|error| error.contains("Invalid event name or wait limit"))
            && rejected["status"] == 400,
        "rejected wait {rejected}"
    );
    Ok(())
}

/// `minarbot-server restore workspace <archive> --db <target>`.
fn restore_workspace(
    root: &TestRoot,
    archive: &std::path::Path,
    target: &std::path::Path,
) -> Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let home = root.dir("home")?;
    let output = std::process::Command::new(server_binary(None)?)
        .args(["restore", "workspace"])
        .arg(archive)
        .arg("--db")
        .arg(target)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .env("MINARBOT_TEST_SERVER", "rust")
        .output()?;
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    ensure!(
        value["exitCode"] == 0 && output.status.success(),
        "restore failed: {value} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[tokio::test]
async fn routine_waits_preserve_artifacts_and_reject_events_cancelled_before_upload_completes(
) -> Result<()> {
    let files_requested = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let failures: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let model = GatedModel::start().await?;
    let (files, done_count, problems) =
        (files_requested.clone(), completed.clone(), failures.clone());
    model.respond(move |request| {
        if after_pause(request) {
            return pause_reply_for(request);
        }
        let fail = |message: String| {
            if let Ok(mut problems) = problems.lock() {
                problems.push(message);
            }
            Answer::Status(500, json!({}))
        };
        // The one-time verification reminder follows the first final answer;
        // with B1 that can be the extra step's answer before the event, so it
        // is answered without counting (see the ledger).
        if request
            .last_user_text()
            .starts_with("One verification reminder")
        {
            return Answer::text("Routine resumed.");
        }
        if !request
            .tool_names()
            .iter()
            .any(|name| name == "wait_for_event")
        {
            return fail(format!(
                "wait_for_event not offered ({} tools, user {:?}, last {})",
                request.tool_names().len(),
                request
                    .last_user_text()
                    .chars()
                    .take(60)
                    .collect::<String>(),
                request
                    .messages()
                    .last()
                    .map(|m| m.to_string().chars().take(200).collect::<String>())
                    .unwrap_or_default()
            ));
        }
        let calls: Vec<String> = request
            .tool_calls()
            .iter()
            .filter_map(|call| call["function"]["name"].as_str().map(str::to_owned))
            .collect();
        if calls.is_empty() {
            files.fetch_add(1, Ordering::SeqCst);
            return Answer::tool_id(
                "artifact",
                "create_text_file",
                json!({"name": "before.txt", "text": "Evidence before event."}),
            );
        }
        if calls.iter().any(|name| name == "wait_for_event") {
            done_count.fetch_add(1, Ordering::SeqCst);
            if calls != ["create_text_file", "wait_for_event"] {
                return fail(format!("calls {calls:?}"));
            }
            let payload = request
                .tool_results()
                .last()
                .map(tool_json)
                .unwrap_or(Value::Null);
            if payload["payload"] != "ready-data" {
                return fail(format!("payload {payload}"));
            }
            return Answer::text("Routine resumed.");
        }
        Answer::tool_id(
            "wait",
            "wait_for_event",
            json!({"event": "ready", "timeoutSeconds": 3600}),
        )
    });
    let root = TestRoot::new()?;
    let mut app = root.start(gated(&model)).await?;
    let agent = id(&api_agent(&app, "Routine", "fixture").await?)?;
    let routine = app
        .post(
            &format!("/api/agents/{agent}/routines"),
            json!({"id": new_id(), "name": "Event routine", "prompt": "Wait for event",
                "nextAt": FIXED_CLOCK + 3_600_000, "intervalSeconds": null, "enabled": false}),
        )
        .await?;
    let routine_id = id(&routine)?;
    let test = format!("/api/routines/{routine_id}/test");
    app.post(
        &test,
        json!({"id": new_id(), "revision": routine["revision"]}),
    )
    .await?;
    let runs_path = format!("/api/routines/{routine_id}/runs");
    let runs = app
        .wait_get(&runs_path, |rows| rows.as_array().map(Vec::len) == Some(1))
        .await?;
    let thread_turns = format!(
        "/api/threads/{}/turns",
        runs[0]["thread"].as_str().ok_or("thread")?
    );
    let waiting_rows = app
        .wait_get(&thread_turns, |rows| {
            rows[0]["events"].as_array().map(Vec::len) == Some(1)
        })
        .await?;
    let waiting = waiting_rows[0].clone();
    ensure!(
        waiting["generatedFiles"].as_array().map(Vec::len) == Some(1),
        "evidence before wait {}",
        waiting["generatedFiles"]
    );
    ensure!(
        files_requested.load(Ordering::SeqCst) == 1,
        "files requested"
    );
    let connection_path = format!("/api/turn-waits/{}", id(&waiting["events"][0])?);
    let connection = app.get(&connection_path).await?;
    app.post_expect(
        &test,
        json!({"id": new_id(), "revision": routine["revision"]}),
        409,
    )
    .await?;
    ensure!(
        rows(&app, &format!("/api/agents/{agent}/turns"))
            .await?
            .is_empty(),
        "routine run leaked into main chat"
    );
    app.restart().await?;
    let delivery = new_id();
    let body = json!({"event": "ready", "payload": "ready-data"}).to_string();
    let secret = connection["secret"].as_str().unwrap_or_default().to_owned();
    let target = json!({"url": app.url(&connection_path)});
    let (first, second) = tokio::join!(
        deliver(&app, &target, &secret, &delivery, &body),
        deliver(&app, &target, &secret, &delivery, &body),
    );
    let (first, second) = (first?, second?);
    ensure!(
        first.0 == 200 && second.0 == 200,
        "concurrent deliveries {} {}",
        first.0,
        second.0
    );
    ensure!(first.1 == second.1, "concurrent receipts differ");
    let done_rows = app
        .wait_get(&thread_turns, |rows| rows[0]["status"] == "done")
        .await?;
    let done = done_rows[0].clone();
    ensure!(
        failures.lock().map(|f| f.is_empty()).unwrap_or(false),
        "provider checks {:?}",
        failures.lock().map(|f| f.clone())
    );
    // Node counts 2: the event continuation and the one-time verification
    // reminder. With B1 the reminder can follow the extra step instead, so
    // reminders are not counted and only the continuation remains.
    ensure!(
        completed.load(Ordering::SeqCst) == 1,
        "completed {}",
        completed.load(Ordering::SeqCst)
    );
    ensure!(
        files_requested.load(Ordering::SeqCst) == 1,
        "evidence recreated"
    );
    ensure!(
        done["generatedFiles"] == waiting["generatedFiles"],
        "files changed"
    );
    let (_, evidence) = crate::fx_support::fetch(
        &app,
        &format!(
            "/api/threads/{}/files/{}",
            runs[0]["thread"].as_str().unwrap_or_default(),
            id(&done["generatedFiles"][0])?
        ),
    )
    .await?;
    ensure!(evidence == b"Evidence before event.", "evidence bytes");
    ensure!(
        rows(&app, &runs_path).await?[0]["status"] == "done",
        "run status"
    );
    app.post(
        &test,
        json!({"id": new_id(), "revision": routine["revision"]}),
    )
    .await?;
    let runs = app
        .wait_get(&runs_path, |rows| rows.as_array().map(Vec::len) == Some(2))
        .await?;
    let next_turns = format!(
        "/api/threads/{}/turns",
        runs[0]["thread"].as_str().ok_or("thread")?
    );
    let next_rows = app
        .wait_get(&next_turns, |rows| {
            rows[0]["events"].as_array().map(Vec::len) == Some(1)
        })
        .await?;
    let next_waiting = next_rows[0].clone();
    let next_connection = app
        .get(&format!(
            "/api/turn-waits/{}",
            id(&next_waiting["events"][0])?
        ))
        .await?;
    let next_secret = next_connection["secret"].as_str().unwrap_or_default();
    let next_delivery = new_id();
    let time = now_seconds();
    let signature = hmac_sha256_hex(
        next_secret.as_bytes(),
        format!("{time}\n{next_delivery}\n{body}").as_bytes(),
    );
    let upload = SplitRequest::start(
        app.origin(),
        "POST",
        &path_of(next_connection["url"].as_str().unwrap_or_default())?,
        &[
            ("Content-Type", "application/json"),
            ("X-Minarbot-Time", &time),
            ("X-Minarbot-Id", &next_delivery),
            ("X-Minarbot-Signature", &signature),
        ],
        body.as_bytes(),
        body.len() - 10,
    )
    .await?;
    app.post(
        &format!("{next_turns}/{}/cancel", id(&next_waiting)?),
        json!({"id": new_id()}),
    )
    .await?;
    ensure!(
        upload.finish().await? == 409,
        "event accepted after cancellation"
    );
    ensure!(
        completed.load(Ordering::SeqCst) == 1,
        "cancelled wait continued"
    );
    ensure!(
        rows(&app, &runs_path).await?[0]["status"] == "cancelled",
        "run not cancelled"
    );
    ensure!(
        rows(&app, &next_turns).await?[0]["events"][0]["status"] == "closed",
        "wait not closed"
    );
    let scheduled = app
        .post(
            &format!("/api/agents/{agent}/routines"),
            json!({"id": new_id(), "name": "Scheduled wait", "prompt": "Wait from schedule",
                "nextAt": FIXED_CLOCK - 1, "intervalSeconds": null, "enabled": true}),
        )
        .await?;
    let scheduled_runs = format!("/api/routines/{}/runs", id(&scheduled)?);
    let scheduled_rows = app
        .wait_get(&scheduled_runs, |rows| {
            rows.as_array().map(Vec::len) == Some(1)
        })
        .await?;
    let scheduled_turns = format!(
        "/api/threads/{}/turns",
        scheduled_rows[0]["thread"].as_str().ok_or("thread")?
    );
    let scheduled_wait = app
        .wait_get(&scheduled_turns, |rows| {
            rows[0]["events"].as_array().map(Vec::len) == Some(1)
        })
        .await?[0]
        .clone();
    let target = id(&scheduled_wait)?;
    app.edit_database(move |db: &Fixture| {
        db.execute("UPDATE turn_waits SET deadline=0 WHERE turn=?", [&target])?;
        Ok(())
    })
    .await?;
    let expired = app
        .wait_get(&scheduled_runs, |rows| rows[0]["status"] == "failed")
        .await?;
    ensure!(
        expired[0]["error"]
            .as_str()
            .is_some_and(|error| error.contains("Event wait timed out")),
        "expired run {}",
        expired[0]
    );
    ensure!(
        completed.load(Ordering::SeqCst) == 1,
        "expired wait continued"
    );
    ensure!(
        rows(&app, &scheduled_turns).await?[0]["generatedFiles"]
            .as_array()
            .map(Vec::len)
            == Some(1),
        "expired run lost its evidence"
    );
    Ok(())
}

#[derive(Clone, Copy)]
enum Boundary {
    Workflow,
    Channel,
    Thread,
}

/// Cancel request builder, root to check (path, turn, unused) and an
/// optional restore request.
type Boundaries = (
    Box<dyn Fn() -> (String, Value)>,
    (String, String, Option<String>),
    Option<(String, Value)>,
);

async fn cancellation_closes_waiting_descendants(boundary: Boundary) -> Result<()> {
    let recipient = Arc::new(std::sync::Mutex::new(String::new()));
    let child_continuations = Arc::new(AtomicUsize::new(0));
    let model = GatedModel::start().await?;
    let (target, continued) = (recipient.clone(), child_continuations.clone());
    model.respond(move |request| {
        if after_pause(request) {
            return pause_reply_for(request);
        }
        let resumed = !request.tool_results().is_empty();
        let source = request.model() == "source";
        if !source && resumed {
            continued.fetch_add(1, Ordering::SeqCst);
        }
        if resumed {
            Answer::text("Finished")
        } else if source {
            let child = target.lock().map(|value| value.clone()).unwrap_or_default();
            Answer::tool_id(
                "call",
                "send_message",
                json!({"recipients": [child], "text": "Wait for external event", "expects_reply": false}),
            )
        } else {
            Answer::tool_id("call", "wait_for_event", json!({"event": "ready", "timeoutSeconds": 3600}))
        }
    });
    let root = TestRoot::new()?;
    let mut app = root.start(gated(&model)).await?;
    let owner = id(&api_agent(&app, "Owner", "source").await?)?;
    let child = id(&api_agent(&app, "Child", "child").await?)?;
    if let Ok(mut target) = recipient.lock() {
        target.clone_from(&child);
    }
    let (cancel, verify, restore): Boundaries = match boundary {
        Boundary::Workflow => {
            let graph = json!({"entry": "agent", "maxSteps": 4, "nodes": [
                    {"id": "agent", "kind": "agent", "agent": owner, "prompt": "Notify child", "next": "wait"},
                    {"id": "wait", "kind": "wait", "event": "workflow-ready", "timeoutSeconds": 3600, "next": "out"},
                    {"id": "out", "kind": "output"}]});
            let workflow = app
                .post(
                    &format!("/api/agents/{owner}/workflows"),
                    json!({"id": new_id(), "name": "Child wait", "revision": 0, "graph": graph}),
                )
                .await?;
            let run = app
                .post(
                    &format!("/api/workflows/{}/run", id(&workflow)?),
                    json!({"id": new_id(), "revision": workflow["revision"], "input": "Start"}),
                )
                .await?;
            let run_path = format!("/api/workflow-runs/{}", id(&run)?);
            let state = app
                .wait_get(&run_path, |value| {
                    value["cursor"] == "wait" && value["status"] == "waiting"
                })
                .await?;
            let revision = state["revision"].clone();
            let cancel_path = format!("{run_path}/cancel");
            (
                Box::new(move || {
                    (
                        cancel_path.clone(),
                        json!({"id": new_id(), "revision": revision}),
                    )
                }),
                (run_path, String::new(), None),
                None,
            )
        }
        Boundary::Channel => {
            let channel = app
                    .post(
                        "/api/channels",
                        json!({"id": new_id(), "name": "Team", "instructions": "Help", "members": [owner, child], "lead": owner}),
                    )
                    .await?;
            let channel_id = id(&channel)?;
            let path = format!("/api/channels/{channel_id}/turns");
            let root_turn = new_id();
            app.post(
                &path,
                json!({"id": root_turn, "text": "Notify child", "recipient": owner}),
            )
            .await?;
            settle(&app, &path, &root_turn, "done").await?;
            let edit = format!("/api/channels/{channel_id}/edit");
            let body = |revision: i64, archived: bool| {
                json!({"id": new_id(), "revision": revision, "name": channel["name"], "instructions": channel["instructions"],
                        "members": channel["members"], "lead": channel["lead"], "archived": archived})
            };
            let revision = channel["revision"].as_i64().unwrap_or(0);
            let (cancel_body, restore_body) = (body(revision, true), body(revision + 1, false));
            let cancel_edit = edit.clone();
            (
                Box::new(move || (cancel_edit.clone(), cancel_body.clone())),
                (path, root_turn, None),
                Some((edit, restore_body)),
            )
        }
        Boundary::Thread => {
            let side = app
                .post(
                    &format!("/api/agents/{owner}/threads"),
                    json!({"id": new_id(), "name": "Source"}),
                )
                .await?;
            let side_id = id(&side)?;
            let path = format!("/api/threads/{side_id}/turns");
            let root_turn = new_id();
            app.post(&path, json!({"id": root_turn, "text": "Notify child"}))
                .await?;
            settle(&app, &path, &root_turn, "done").await?;
            let edit = format!("/api/threads/{side_id}/edit");
            let revision = side["revision"].as_i64().unwrap_or(0);
            let cancel_body = json!({"id": new_id(), "revision": revision, "name": side["name"], "archived": true});
            let restore_body = json!({"id": new_id(), "revision": revision + 1, "name": side["name"], "archived": false});
            let cancel_edit = edit.clone();
            (
                Box::new(move || (cancel_edit.clone(), cancel_body.clone())),
                (path, root_turn, None),
                Some((edit, restore_body)),
            )
        }
    };
    let threads = app
        .wait_get(&format!("/api/agents/{child}/threads"), |value| {
            value.as_array().is_some_and(|rows| !rows.is_empty())
        })
        .await?;
    let turns = format!("/api/threads/{}/turns", id(&threads[0])?);
    let waiting_rows = app
        .wait_get(&turns, |value| {
            value[0]["events"]
                .as_array()
                .is_some_and(|events| !events.is_empty())
        })
        .await?;
    let waiting = waiting_rows[0].clone();
    let connection = app
        .get(&format!("/api/turn-waits/{}", id(&waiting["events"][0])?))
        .await?;
    let unrelated = new_id();
    let direct = format!("/api/agents/{child}/turns");
    app.post(
        &direct,
        json!({"id": unrelated, "text": "Independent task"}),
    )
    .await?;
    app.wait_row(&direct, &unrelated, |row| {
        row["events"]
            .as_array()
            .is_some_and(|events| !events.is_empty())
    })
    .await?;
    let (cancel_path, cancel_body) = cancel();
    app.post(&cancel_path, cancel_body).await?;
    let stopped = rows(&app, &turns).await?[0].clone();
    ensure!(
        stopped["status"] == "cancelled",
        "descendant not cancelled: {}",
        stopped["status"]
    );
    ensure!(
        stopped["events"][0]["status"] == "closed",
        "wait not closed: {}",
        stopped["events"]
    );
    let (root_path, root_turn, _) = verify.clone();
    match boundary {
        Boundary::Workflow => {
            ensure!(
                app.get(&root_path).await?["steps"][0]["status"] == "done",
                "workflow step not done"
            );
        }
        _ => {
            ensure!(
                row(&app, &root_path, &root_turn).await?["status"] == "done",
                "root turn changed"
            );
        }
    }
    let body = json!({"event": "ready"}).to_string();
    let delivery = new_id();
    let secret = connection["secret"].as_str().unwrap_or_default().to_owned();
    let (status, _) = deliver(&app, &connection, &secret, &delivery, &body).await?;
    ensure!(status == 409, "event accepted after cancellation: {status}");
    app.restart().await?;
    if let Some((path, body)) = &restore {
        app.post(path, body.clone()).await?;
    }
    ensure!(
        rows(&app, &turns).await?[0]["status"] == "cancelled",
        "restart revived the descendant"
    );
    let (status, _) = deliver(&app, &connection, &secret, &delivery, &body).await?;
    ensure!(status == 409, "event accepted after restart: {status}");
    ensure!(
        child_continuations.load(Ordering::SeqCst) == 0,
        "child continued"
    );
    ensure!(
        row(&app, &direct, &unrelated).await?["status"] == "queued",
        "unrelated wait was cancelled"
    );
    app.post_expect(
        &format!("{turns}/{}/resume", id(&waiting)?),
        json!({"id": new_id(), "revision": stopped["revision"], "acknowledgeUncertain": true}),
        409,
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn workflow_cancellation_closes_waiting_descendants_of_completed_steps() -> Result<()> {
    cancellation_closes_waiting_descendants(Boundary::Workflow).await
}

#[tokio::test]
async fn channel_cancellation_closes_waiting_descendants_of_completed_steps() -> Result<()> {
    cancellation_closes_waiting_descendants(Boundary::Channel).await
}

#[tokio::test]
async fn thread_cancellation_closes_waiting_descendants_of_completed_steps() -> Result<()> {
    cancellation_closes_waiting_descendants(Boundary::Thread).await
}
