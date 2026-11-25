//! Script discovery and script messages (ports of
//! `app/script-discovery.test.ts` and `app/script-messages.test.ts`).

use crate::fx_support::{after_pause, api_agent, gated, id, pause_reply, statuses, terminal, work_calls};
use minarbot_testkit::{
    gated::{Answer, GatedModel},
    http::{Method, SplitRequest},
    prelude::*,
    wait::until_within,
    Server,
};
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
};

/// Bearer request without browser headers; returns status and JSON body.
async fn bearer(
    app: &Server,
    method: Method,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> Result<(u16, Value)> {
    let mut request = app
        .client()?
        .request(method, app.url(path))
        .header("Authorization", format!("Bearer {token}"));
    if let Some(body) = body {
        request = request
            .header("Content-Type", "application/json")
            .body(body.to_string());
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    let text = response.text().await?;
    Ok((status, serde_json::from_str(&text).unwrap_or(Value::String(text))))
}

async fn expect(
    app: &Server,
    method: Method,
    path: &str,
    token: &str,
    body: Option<Value>,
    status: u16,
) -> Result<Value> {
    let (actual, value) = bearer(app, method.clone(), path, token, body).await?;
    ensure!(actual == status, "{method} {path}: expected {status}, got {actual}: {value}");
    Ok(value)
}

fn mode(path: &Path) -> Result<u32> {
    Ok(fs::symlink_metadata(path)?.permissions().mode() & 0o777)
}

fn connection(file: &Path) -> Result<Value> {
    Ok(serde_json::from_str(&fs::read_to_string(file)?)?)
}

#[tokio::test]
async fn duplicate_discovery_startup_cannot_interrupt_the_owning_server_task() -> Result<()> {
    let model = GatedModel::start().await?;
    model.respond(|_| Answer::Hang);
    let root = TestRoot::new()?;
    let config = gated(&model).script_directory(root.path().join("scripts"));
    let app = root.start(config.clone()).await?;
    let agent = id(&api_agent(&app, "A", "test").await?)?;
    let path = format!("/api/agents/{agent}/turns");
    let turn = new_id();
    app.post(&path, json!({"id": turn, "text": "Hold"})).await?;
    model.wait_calls(1).await?;
    let error = root.start_error_beside(&app, config).await?;
    ensure!(error.contains("Minarbot startup failed"), "duplicate start: {error}");
    let rows = app.get(&path).await?;
    ensure!(rows[0]["status"] == "running", "owning task interrupted: {}", rows[0]);
    app.post(&format!("{path}/{turn}/cancel"), json!({"id": new_id()}))
        .await?;
    Ok(())
}

#[tokio::test]
async fn private_script_discovery_follows_live_grants_rotates_on_restart_and_preserves_foreign_files(
) -> Result<()> {
    let root = TestRoot::new()?;
    let directory = root.path().join("scripts");
    let file = directory.join("connection.json");
    let config = ServerConfig::new().script_directory(&directory);
    let mut app = root.start(config.clone()).await?;
    ensure!(mode(&directory)? == 0o700, "directory mode {:o}", mode(&directory)?);
    ensure!(!file.exists(), "discovery file before any grant");
    let a = id(&api_agent(&app, "A", "test").await?)?;
    let b = id(&api_agent(&app, "B", "test").await?)?;
    let path = format!("/api/agents/{a}/scripts");
    ensure!(app.get(&path).await?["configured"] == true, "not configured");
    let enable = json!({"id": new_id(), "revision": 0, "enabled": true});
    fs::write(&file, "preserve")?;
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
    app.post_expect(&path, enable.clone(), 500).await?;
    ensure!(fs::read_to_string(&file)? == "preserve", "foreign file replaced");
    fs::remove_file(&file)?;
    app.post(&path, enable.clone()).await?;
    let first = connection(&file)?;
    let token = first["token"].as_str().ok_or("token")?.to_owned();
    ensure!(first["url"] == app.origin(), "url {first}");
    ensure!(
        token.len() == 43
            && token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
        "token format {token}"
    );
    ensure!(mode(&file)? == 0o600, "file mode {:o}", mode(&file)?);
    // B2 (`docs/test-ledger.md`): `/api/scripts/agents/{id}/provider`
    // deadlocks the server, so the token is proven on the turns route.
    let (status, body) = bearer(
        &app,
        Method::GET,
        &format!("/api/scripts/agents/{a}/turns"),
        &token,
        None,
    )
    .await?;
    ensure!(status == 200, "script token rejected: {status}");
    ensure!(!body.to_string().contains(&token), "token echoed");
    app.post(
        &format!("/api/agents/{b}/scripts"),
        json!({"id": new_id(), "revision": 0, "enabled": true}),
    )
    .await?;
    app.post(&path, json!({"id": new_id(), "revision": 1, "enabled": false}))
        .await?;
    ensure!(connection(&file)? == first, "file changed while B is granted");
    app.post(
        &format!("/api/agents/{b}/scripts"),
        json!({"id": new_id(), "revision": 1, "enabled": false}),
    )
    .await?;
    ensure!(!file.exists(), "file kept without grants");
    app.post(&path, enable).await?;
    ensure!(
        !file.exists(),
        "historical enabled receipt must not republish revoked access"
    );
    app.post(&path, json!({"id": new_id(), "revision": 2, "enabled": true}))
        .await?;
    app.shutdown().await?;
    ensure!(!file.exists(), "file kept after shutdown");
    ensure!(!directory.join("owner.lock").exists(), "lock kept after shutdown");
    app.start_stopped(config.clone()).await?;
    let second = connection(&file)?;
    ensure!(second["token"] != first["token"], "token not rotated");
    ensure!(second["url"] == app.origin(), "url after restart {second}");
    let (status, _) = bearer(
        &app,
        Method::GET,
        &format!("/api/scripts/agents/{a}/provider"),
        &token,
        None,
    )
    .await?;
    ensure!(status == 403, "stale token accepted: {status}");
    let competing = TestRoot::new()?;
    competing.start_error_beside(&app, config.clone()).await?;
    ensure!(connection(&file)? == second, "competing server changed discovery");
    app.shutdown().await?;
    fs::write(&file, "foreign")?;
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
    root.start_error_beside(&app, config.clone()).await?;
    ensure!(fs::read_to_string(&file)? == "foreign", "foreign file replaced at startup");
    ensure!(!directory.join("owner.lock").exists(), "lock kept after refused startup");
    fs::remove_file(&file)?;
    app.start_stopped(config).await?;
    ensure!(connection(&file)?["url"] == app.origin(), "discovery after recovery");
    Ok(())
}

#[tokio::test]
async fn discovery_rejects_unsafe_directories_and_symlink_destinations_without_changing_targets(
) -> Result<()> {
    let root = TestRoot::new()?;
    let unsafe_dir = root.path().join("unsafe");
    fs::create_dir(&unsafe_dir)?;
    fs::set_permissions(&unsafe_dir, fs::Permissions::from_mode(0o755))?;
    let error = root
        .start_error(ServerConfig::new().script_directory(&unsafe_dir))
        .await?;
    ensure!(error.contains("must be private"), "unsafe directory: {error}");
    let link = root.path().join("link");
    symlink(&unsafe_dir, &link)?;
    let error = root
        .start_error(ServerConfig::new().script_directory(&link))
        .await?;
    ensure!(error.contains("must be private"), "linked directory: {error}");
    let directory: PathBuf = root.path().join("private");
    let target = root.path().join("target");
    let mut app = root
        .start(ServerConfig::new().script_directory(&directory))
        .await?;
    fs::write(&target, "preserve")?;
    symlink(&target, directory.join("connection.json"))?;
    let agent = new_id();
    app.post_expect(
        "/api/agents",
        json!({"id": agent, "runtime": "api", "name": "Fixture", "model": "test", "instructions": "Help"}),
        201,
    )
    .await?;
    app.post_expect(
        &format!("/api/agents/{agent}/scripts"),
        json!({"id": new_id(), "revision": 0, "enabled": true}),
        500,
    )
    .await?;
    ensure!(fs::read_to_string(&target)? == "preserve", "symlink target changed");
    app.shutdown().await?;
    ensure!(
        fs::symlink_metadata(directory.join("connection.json"))?
            .file_type()
            .is_symlink(),
        "symlink replaced"
    );
    Ok(())
}

fn work_count(model: &GatedModel) -> usize {
    work_calls(model).len()
}

#[tokio::test]
async fn script_messages_and_answers_share_grants_scope_receipts_redaction_and_rate_admission(
) -> Result<()> {
    let token = format!("{}{}", new_id().replace('-', ""), new_id().replace('-', ""));
    let model = GatedModel::start().await?;
    let reply = format!("Done {token}");
    model.respond(move |request| {
        if after_pause(request) {
            return pause_reply();
        }
        if request.last_text() == "ASK" {
            Answer::tool_id(
                "ask",
                "ask_user",
                json!({"question": "Proceed?", "choices": ["Yes", "No"]}),
            )
        } else {
            Answer::text(reply.clone())
        }
    });
    let root = TestRoot::new()?;
    let config = gated(&model).option("scriptToken", token.clone());
    let mut app = root.start(config).await?;
    let a = id(&api_agent(&app, "A", "fixture").await?)?;
    let b = id(&api_agent(&app, "B", "fixture").await?)?;
    let path = format!("/api/scripts/agents/{a}/turns");
    let ui_path = format!("/api/agents/{a}/turns");
    let permission = format!("/api/agents/{a}/scripts");
    let command = json!({"id": new_id(), "text": "ASK"});
    expect(&app, Method::POST, &path, &token, Some(command.clone()), 403).await?;
    app.post(&permission, json!({"id": new_id(), "revision": 0, "enabled": true}))
        .await?;
    let with_origin = app
        .client()?
        .post(app.url(&path))
        .header("Authorization", format!("Bearer {token}"))
        .header("Origin", app.origin())
        .header("Content-Type", "application/json")
        .body(command.to_string())
        .send()
        .await?
        .status()
        .as_u16();
    ensure!(with_origin == 403, "browser origin accepted: {with_origin}");
    expect(&app, Method::POST, &path, "wrong", Some(command.clone()), 403).await?;
    expect(&app, Method::POST, &path, &token, Some(command.clone()), 202).await?;
    let turn = until("script question", || async {
        let (_, rows) = bearer(&app, Method::GET, &path, &token, None).await?;
        Ok(rows[0]["questions"]
            .as_array()
            .is_some_and(|questions| !questions.is_empty())
            .then(|| rows[0].clone()))
    })
    .await?;
    let turn_id = id(&turn)?;
    let question = id(&turn["questions"][0])?;
    let answer_path = format!("{path}/{turn_id}/questions/{question}/answer");
    let answer = json!({"id": new_id(), "text": "Yes"});
    app.post(
        &format!("/api/agents/{b}/scripts"),
        json!({"id": new_id(), "revision": 0, "enabled": true}),
    )
    .await?;
    expect(
        &app,
        Method::POST,
        &format!("/api/scripts/agents/{b}/turns/{turn_id}/questions/{question}/answer"),
        &token,
        Some(answer.clone()),
        404,
    )
    .await?;
    app.post(&permission, json!({"id": new_id(), "revision": 1, "enabled": false}))
        .await?;
    expect(&app, Method::POST, &answer_path, &token, Some(answer.clone()), 403).await?;
    expect(&app, Method::GET, &path, &token, None, 403).await?;
    app.post(&permission, json!({"id": new_id(), "revision": 2, "enabled": true}))
        .await?;
    let accepted = expect(&app, Method::POST, &answer_path, &token, Some(answer.clone()), 200).await?;
    ensure!(
        expect(&app, Method::POST, &answer_path, &token, Some(answer.clone()), 200).await? == accepted,
        "answer replay"
    );
    let result = until("script answer", || async {
        let (_, rows) = bearer(&app, Method::GET, &path, &token, None).await?;
        Ok((rows[0]["status"] == "done").then(|| rows[0].clone()))
    })
    .await?;
    ensure!(result["reply"] == "Done [redacted]", "reply {}", result["reply"]);
    ensure!(work_count(&model) == 2, "model calls {}", work_count(&model));
    app.restart().await?;
    expect(&app, Method::POST, &path, &token, Some(command.clone()), 202).await?;
    ensure!(
        expect(&app, Method::POST, &answer_path, &token, Some(answer.clone()), 200).await? == accepted,
        "answer receipt after restart"
    );
    ensure!(work_count(&model) == 2, "replay ran the model");
    let mut changed = answer.clone();
    changed["text"] = json!("No");
    expect(&app, Method::POST, &answer_path, &token, Some(changed), 409).await?;
    app.post(
        &format!("/api/agents/{a}/visibility"),
        json!({"id": new_id(), "revision": 0, "hidden": true}),
    )
    .await?;
    expect(
        &app,
        Method::POST,
        &path,
        &token,
        Some(json!({"id": new_id(), "text": "Hidden"})),
        409,
    )
    .await?;
    app.post(
        &format!("/api/agents/{a}/visibility"),
        json!({"id": new_id(), "revision": 1, "hidden": false}),
    )
    .await?;
    let authorization = format!("Bearer {token}");
    let late = SplitRequest::start(
        app.origin(),
        "POST",
        &path,
        &[("Authorization", &authorization), ("Content-Type", "application/json")],
        json!({"id": new_id(), "text": "Too late"}).to_string().as_bytes(),
        "\"Too late\"}".len(),
    )
    .await?;
    app.post(&permission, json!({"id": new_id(), "revision": 3, "enabled": false}))
        .await?;
    ensure!(late.finish().await? == 403, "revocation mid-request admitted the message");
    app.post(&permission, json!({"id": new_id(), "revision": 4, "enabled": true}))
        .await?;
    let routine = app
        .post(
            &format!("/api/agents/{a}/routines"),
            json!({"id": new_id(), "name": "Script", "prompt": "RUN",
                "nextAt": minarbot_testkit::FIXED_CLOCK + 3_600_000, "intervalSeconds": null, "enabled": false}),
        )
        .await?;
    expect(
        &app,
        Method::POST,
        &format!("/api/scripts/routines/{}/run", id(&routine)?),
        &token,
        Some(json!({"id": new_id(), "revision": routine["revision"], "payload": ""})),
        200,
    )
    .await?;
    for n in 0..58 {
        expect(
            &app,
            Method::POST,
            &path,
            &token,
            Some(json!({"id": new_id(), "text": format!("Message {n}")})),
            202,
        )
        .await?;
    }
    expect(
        &app,
        Method::POST,
        &path,
        &token,
        Some(json!({"id": new_id(), "text": "Limited"})),
        429,
    )
    .await?;
    expect(&app, Method::POST, &path, &token, Some(command.clone()), 202).await?;
    let ui_rows = app.get(&ui_path).await?;
    ensure!(
        ui_rows.as_array().map_or(0, Vec::len) <= 59,
        "rate admission let {} rows in",
        ui_rows.as_array().map_or(0, Vec::len)
    );
    let threads_path = format!("/api/scripts/agents/{b}/threads");
    let create_side = json!({"id": new_id(), "name": "Script side"});
    let side = expect(&app, Method::POST, &threads_path, &token, Some(create_side.clone()), 200).await?;
    ensure!(
        expect(&app, Method::POST, &threads_path, &token, Some(create_side.clone()), 200).await? == side,
        "side replay"
    );
    let side_id = id(&side)?;
    let listed = expect(&app, Method::GET, &threads_path, &token, None, 200).await?;
    ensure!(
        listed
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"] == side_id.as_str())),
        "side not listed: {listed}"
    );
    let side_path = format!("{threads_path}/{side_id}/turns");
    let side_message = json!({"id": new_id(), "text": "ASK"});
    expect(
        &app,
        Method::POST,
        &format!("/api/scripts/agents/{a}/threads/{side_id}/turns"),
        &token,
        Some(side_message.clone()),
        404,
    )
    .await?;
    expect(&app, Method::POST, &side_path, &token, Some(side_message.clone()), 202).await?;
    let side_turn = until("side question", || async {
        let (_, rows) = bearer(&app, Method::GET, &side_path, &token, None).await?;
        Ok(rows[0]["questions"]
            .as_array()
            .is_some_and(|questions| !questions.is_empty())
            .then(|| rows[0].clone()))
    })
    .await?;
    let side_turn_id = id(&side_turn)?;
    let side_question = id(&side_turn["questions"][0])?;
    let side_answer = json!({"id": new_id(), "text": "No"});
    expect(
        &app,
        Method::POST,
        &format!("/api/scripts/agents/{b}/turns/{side_turn_id}/questions/{side_question}/answer"),
        &token,
        Some(side_answer.clone()),
        404,
    )
    .await?;
    let side_answer_path = format!("{side_path}/{side_turn_id}/questions/{side_question}/answer");
    expect(&app, Method::POST, &side_answer_path, &token, Some(side_answer.clone()), 200).await?;
    until("side answer", || async {
        let (_, rows) = bearer(&app, Method::GET, &side_path, &token, None).await?;
        Ok((rows[0]["status"] == "done").then_some(()))
    })
    .await?;
    ensure!(
        expect(
            &app,
            Method::GET,
            &format!("/api/scripts/agents/{b}/turns"),
            &token,
            None,
            200
        )
        .await?
            == json!([]),
        "side turn leaked into main chat"
    );
    // The 58 admitted messages run one harness turn each (a new gateway
    // process per turn, about a second on a loaded host), so the drain gets
    // a budget per queued turn instead of the default wait.
    let queued = statuses(&app, &ui_path).await?.len();
    let drained = until_within(
        "every main chat turn settles",
        std::time::Duration::from_secs(2 * u64::try_from(queued)?),
        || async {
            let rows = statuses(&app, &ui_path).await?;
            Ok(rows.iter().all(|(_, status)| terminal(status)).then_some(rows))
        },
    )
    .await;
    ensure!(drained.is_ok(), "drain: {:?}", statuses(&app, &ui_path).await?);
    let side_calls = work_count(&model);
    app.restart().await?;
    expect(&app, Method::POST, &side_path, &token, Some(side_message.clone()), 202).await?;
    expect(&app, Method::POST, &side_answer_path, &token, Some(side_answer), 200).await?;
    ensure!(work_count(&model) == side_calls, "side replay ran the model");
    let stop_turn = json!({"id": new_id(), "text": "ASK"});
    let stop_id = id(&stop_turn)?;
    expect(&app, Method::POST, &side_path, &token, Some(stop_turn), 202).await?;
    until("stop question", || async {
        let (_, rows) = bearer(&app, Method::GET, &side_path, &token, None).await?;
        Ok(rows
            .as_array()
            .is_some_and(|rows| {
                rows.iter().any(|row| {
                    row["id"] == stop_id.as_str()
                        && row["questions"].as_array().is_some_and(|q| !q.is_empty())
                })
            })
            .then_some(()))
    })
    .await?;
    let stop_path = format!("{side_path}/{stop_id}/cancel");
    let stop_body = json!({"id": new_id()});
    expect(&app, Method::GET, &stop_path, &token, None, 405).await?;
    expect(
        &app,
        Method::POST,
        &format!("/api/scripts/agents/{a}/threads/{side_id}/turns/{stop_id}/cancel"),
        &token,
        Some(stop_body.clone()),
        404,
    )
    .await?;
    for _ in 0..2 {
        let stopped = expect(&app, Method::POST, &stop_path, &token, Some(stop_body.clone()), 200).await?;
        ensure!(stopped["status"] == "cancelled", "cancel {stopped}");
    }
    let replay = SplitRequest::start(
        app.origin(),
        "POST",
        &side_path,
        &[("Authorization", &authorization), ("Content-Type", "application/json")],
        side_message.to_string().as_bytes(),
        "\"ASK\"}".len(),
    )
    .await?;
    app.post(
        &format!("/api/threads/{side_id}/edit"),
        json!({"id": new_id(), "revision": side["revision"], "name": side["name"], "archived": true}),
    )
    .await?;
    expect(
        &app,
        Method::POST,
        &side_path,
        &token,
        Some(json!({"id": new_id(), "text": "Archived"})),
        409,
    )
    .await?;
    app.post(
        &format!("/api/threads/{side_id}/delete"),
        json!({"id": new_id(), "revision": side["revision"].as_i64().unwrap_or(0) + 1}),
    )
    .await?;
    ensure!(replay.finish().await? == 404, "deleted thread accepted a replay");
    expect(&app, Method::GET, &side_path, &token, None, 404).await?;
    expect(&app, Method::POST, &side_path, &token, Some(side_message), 404).await?;
    app.post(
        &format!("/api/agents/{b}/scripts"),
        json!({"id": new_id(), "revision": 1, "enabled": false}),
    )
    .await?;
    expect(&app, Method::POST, &threads_path, &token, Some(create_side), 403).await?;
    expect(&app, Method::GET, &threads_path, &token, None, 403).await?;
    Ok(())
}
