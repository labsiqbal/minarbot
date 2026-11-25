//! Local script token, agent grants, payloads and retry identity (port of
//! `app/scripts.test.ts`).

use crate::sb_support::id;
use minarbot_testkit::{
    control::{Answer, ControlModel},
    http::{Method, SplitRequest},
    prelude::*,
    Server,
};

fn token() -> String {
    format!("{}{}", new_id().replace('-', ""), new_id().replace('-', ""))
}

/// Script-side request: bearer token, no browser headers unless given.
async fn script(
    app: &Server,
    method: Method,
    path: &str,
    body: Option<&Value>,
    headers: &[(&str, &str)],
    status: u16,
    secret: &str,
) -> Result<Value> {
    let mut request = app.client()?.request(method, app.url(path));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    if let Some(body) = body {
        request = request
            .header("Content-Type", "application/json")
            .body(body.to_string());
    }
    let response = request.send().await?;
    let code = response.status().as_u16();
    let text = response.text().await?;
    ensure!(
        code == status,
        "{path}: expected {status}, got {code}: {text}"
    );
    ensure!(!text.contains(secret), "{path} leaked the script token");
    Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
}

#[tokio::test]
async fn local_scripts_require_token_and_current_agent_grant_keep_payload_and_retry_identity(
) -> Result<()> {
    let secret = token();
    let model = ControlModel::start().await?;
    model.respond(|request| {
        if request.last_user_text().contains("HOLD") {
            Answer::Defer
        } else {
            Answer::text("Script result")
        }
    });
    let prompts = |model: &ControlModel| -> Vec<String> {
        model
            .requests()
            .iter()
            .map(|request| request.last_user_text())
            .collect()
    };
    let root = TestRoot::new()?;
    let config = ServerConfig::new()
        .gateway(model.gateway())
        .option("scriptToken", secret.clone());
    let refused = root
        .start_error(config.clone().option("scriptToken", "short"))
        .await?;
    ensure!(
        refused.contains("MINARBOT_SCRIPT_TOKEN"),
        "short token: {refused}"
    );
    let mut app = root.start(config.clone()).await?;
    let bearer = format!("Bearer {secret}");
    let auth: &[(&str, &str)] = &[("Authorization", bearer.as_str())];
    let agent = id(&app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Script agent", "model": "fixture", "instructions": "Help."}),
            201,
        )
        .await?)?;
    let permission = format!("/api/agents/{agent}/scripts");
    let list = format!("/api/agents/{agent}/routines");
    ensure!(
        app.get(&permission).await? == json!({"enabled": 0, "revision": 0, "configured": true}),
        "initial permission"
    );
    let routine = app
        .post(
            &list,
            json!({"id": new_id(), "name": "Script routine", "prompt": "Saved instruction",
                "nextAt": minarbot_testkit::FIXED_CLOCK + 3_600_000, "intervalSeconds": 60, "enabled": false}),
        )
        .await?;
    let routine_id = id(&routine)?;
    let path = format!("/api/scripts/routines/{routine_id}/run");
    let runs = format!("/api/scripts/routines/{routine_id}/runs");
    let body = json!({"id": new_id(), "revision": routine["revision"], "payload": "Event data"});
    script(&app, Method::POST, &path, Some(&body), auth, 403, &secret).await?;
    let grant = json!({"id": new_id(), "revision": 0, "enabled": true});
    script(
        &app,
        Method::POST,
        &permission,
        Some(&grant),
        auth,
        403,
        &secret,
    )
    .await?;
    ensure!(
        app.post(&permission, grant.clone()).await? == json!({"enabled": 1, "revision": 1}),
        "grant"
    );
    ensure!(
        app.post(&permission, grant.clone()).await? == json!({"enabled": 1, "revision": 1}),
        "grant replay"
    );
    let mut changed = grant.clone();
    changed["enabled"] = json!(false);
    app.post_expect(&permission, changed, 409).await?;
    let origin = app.origin().to_owned();
    for headers in [
        vec![],
        vec![("Authorization", "Bearer wrong")],
        vec![
            ("Authorization", bearer.as_str()),
            ("Origin", origin.as_str()),
        ],
        vec![("Authorization", bearer.as_str()), ("Origin", "null")],
        vec![("Authorization", bearer.as_str()), ("Host", "evil.example")],
        vec![
            ("Authorization", bearer.as_str()),
            ("Sec-Fetch-Site", "cross-site"),
        ],
    ] {
        script(
            &app,
            Method::POST,
            &path,
            Some(&body),
            &headers,
            403,
            &secret,
        )
        .await
        .map_err(|error| format!("{headers:?}: {error}"))?;
    }
    let mut large = body.clone();
    large["payload"] = json!("x".repeat(16001));
    script(&app, Method::POST, &path, Some(&large), auth, 400, &secret).await?;
    let mut stale = body.clone();
    stale["revision"] = json!(routine["revision"].as_i64().unwrap_or(0) + 1);
    script(&app, Method::POST, &path, Some(&stale), auth, 409, &secret).await?;
    let result = script(&app, Method::POST, &path, Some(&body), auth, 200, &secret).await?;
    ensure!(
        script(&app, Method::POST, &path, Some(&body), auth, 200, &secret).await? == result,
        "run replay differs"
    );
    let mut other_payload = body.clone();
    other_payload["payload"] = json!("Changed");
    script(
        &app,
        Method::POST,
        &path,
        Some(&other_payload),
        auth,
        409,
        &secret,
    )
    .await?;
    app.post_expect(
        &format!("/api/routines/{routine_id}/test"),
        json!({"id": body["id"], "revision": routine["revision"]}),
        409,
    )
    .await?;
    let rows = until_value(
        "script run",
        || script(&app, Method::GET, &runs, None, auth, 200, &secret),
        |rows| rows[0]["status"] == "done",
    )
    .await?;
    ensure!(rows[0]["source"] == "script", "source: {rows}");
    ensure!(rows[0]["scheduled_at"].is_null(), "scheduled_at: {rows}");
    ensure!(rows.as_array().map(Vec::len) == Some(1), "runs: {rows}");
    ensure!(
        prompts(&model) == ["Saved instruction\n\nScript input:\nEvent data"],
        "prompts: {:?}",
        prompts(&model)
    );
    ensure!(app.get(&list).await?[0] == routine, "routine changed");
    ensure!(
        app.get(&format!("/api/agents/{agent}/turns")).await? == json!([]),
        "script run leaked into the direct chat"
    );
    let granted = script(
        &app,
        Method::GET,
        &format!("/api/scripts/agents/{agent}/routines"),
        None,
        auth,
        200,
        &secret,
    )
    .await?;
    ensure!(
        granted.as_array().map(Vec::len) == Some(1),
        "granted routines: {granted}"
    );

    app.restart().await?;
    ensure!(
        script(&app, Method::POST, &path, Some(&body), auth, 200, &secret).await? == result,
        "replay after restart"
    );
    ensure!(prompts(&model).len() == 1, "restart replayed the run");
    let other = id(&app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "No script grant", "model": "fixture", "instructions": "Help."}),
            201,
        )
        .await?)?;
    let private = app
        .post(
            &format!("/api/agents/{other}/routines"),
            json!({"id": new_id(), "name": "Private routine", "prompt": "Never execute",
                "nextAt": minarbot_testkit::FIXED_CLOCK + 3_600_000, "intervalSeconds": null, "enabled": false}),
        )
        .await?;
    let private_id = id(&private)?;
    let foreign = json!({"id": new_id(), "revision": private["revision"], "payload": "Event data"});
    script(
        &app,
        Method::POST,
        &format!("/api/scripts/routines/{private_id}/run"),
        Some(&foreign),
        auth,
        403,
        &secret,
    )
    .await?;
    script(
        &app,
        Method::GET,
        &format!("/api/scripts/routines/{private_id}/runs"),
        None,
        auth,
        403,
        &secret,
    )
    .await?;
    script(
        &app,
        Method::GET,
        &format!("/api/scripts/agents/{other}/routines"),
        None,
        auth,
        403,
        &secret,
    )
    .await?;

    let rotated = token();
    app.restart_with(config.clone().option("scriptToken", rotated.clone()))
        .await?;
    script(&app, Method::POST, &path, Some(&body), auth, 403, &secret).await?;
    let rotated_bearer = format!("Bearer {rotated}");
    ensure!(
        script(
            &app,
            Method::POST,
            &path,
            Some(&body),
            &[("Authorization", rotated_bearer.as_str())],
            200,
            &rotated
        )
        .await?
            == result,
        "rotated token replay"
    );
    ensure!(prompts(&model).len() == 1, "rotation replayed the run");

    app.restart_with(config.clone()).await?;
    let mut hold = body.clone();
    hold["id"] = json!(new_id());
    hold["payload"] = json!("HOLD");
    let held = script(&app, Method::POST, &path, Some(&hold), auth, 200, &secret).await?;
    let held_step = model.deferred().await?;
    let mut busy = body.clone();
    busy["id"] = json!(new_id());
    script(&app, Method::POST, &path, Some(&busy), auth, 409, &secret).await?;
    let raw = body.to_string();
    let delayed = SplitRequest::start(
        app.origin(),
        "POST",
        &path,
        &[
            ("Authorization", bearer.as_str()),
            ("Content-Type", "application/json"),
        ],
        raw.as_bytes(),
        1,
    )
    .await?;
    ensure!(
        app.post(
            &permission,
            json!({"id": new_id(), "enabled": false, "revision": 1})
        )
        .await?
            == json!({"enabled": 0, "revision": 2}),
        "revoke"
    );
    ensure!(
        delayed.finish().await? == 403,
        "in-flight request outlived the revocation"
    );
    script(&app, Method::POST, &path, Some(&body), auth, 403, &secret).await?;
    script(&app, Method::GET, &runs, None, auth, 403, &secret).await?;
    script(
        &app,
        Method::GET,
        &format!("/api/scripts/agents/{agent}/routines"),
        None,
        auth,
        403,
        &secret,
    )
    .await?;
    let mut regrant = grant.clone();
    regrant["id"] = json!(new_id());
    app.post_expect(&permission, regrant, 409).await?;
    ensure!(
        app.get(&permission).await?["enabled"] == 0,
        "permission re-enabled"
    );
    app.post(
        &format!(
            "/api/threads/{}/turns/{}/cancel",
            held["thread"].as_str().ok_or("thread")?,
            held["turn"].as_str().ok_or("turn")?
        ),
        json!({"id": new_id()}),
    )
    .await?;
    drop(held_step);
    app.restart_with(config.clone().without("scriptToken"))
        .await?;
    ensure!(
        app.get(&permission).await?["configured"] == false,
        "unconfigured token"
    );
    let mut late = grant.clone();
    late["id"] = json!(new_id());
    late["revision"] = json!(2);
    app.post_expect(&permission, late, 409).await?;
    script(&app, Method::POST, &path, Some(&body), auth, 403, &secret).await?;
    Ok(())
}
