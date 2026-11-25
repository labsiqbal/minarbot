//! Script provider settings keep identity and credentials isolated (port of
//! `app/script-provider.test.ts`).
//!
//! The Node case stays in place for B2 (`GET /api/scripts/agents/{id}/provider`
//! deadlocks) and B3 (`script models` cannot reach the instance catalog);
//! this test covers every other step. `providerAccount` and `backupModel`
//! are accepted and ignored since #87, so the "missing account" refusal is
//! dropped.

use crate::sb_support::id;
use minarbot_testkit::{
    control::{Answer, ControlModel},
    http::{Method, SplitRequest},
    prelude::*,
    server::Gateway,
    Server,
};

fn token() -> String {
    format!("{}{}", new_id().replace('-', ""), new_id().replace('-', ""))
}

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
    ensure!(!text.contains(secret), "{path} leaked the provider key");
    Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
}

#[tokio::test]
async fn script_provider_settings_keep_identity_and_credentials_isolated_reject_busy_revoked_changes_and_preserve_receipts(
) -> Result<()> {
    let script_token = token();
    let secret = token();
    let model = ControlModel::start().await?;
    model.respond(|request| {
        if request.last_user_text() == "HOLD" {
            Answer::Defer
        } else {
            Answer::text("Done")
        }
    });
    let root = TestRoot::new()?;
    let gateway = Gateway {
        api_key: Some(secret.clone()),
        ..model.gateway()
    };
    let config = ServerConfig::new()
        .gateway(gateway)
        .option("scriptToken", script_token.clone());
    let mut app = root.start(config).await?;
    let bearer = format!("Bearer {script_token}");
    let auth: &[(&str, &str)] = &[("Authorization", bearer.as_str())];
    let a = app
        .post("/api/agents", json!({"runtime": "api", "id": new_id(), "name": "Keep name", "model": "old", "instructions": "Keep instructions"}))
        .await?;
    let agent = id(&a)?;
    let path = format!("/api/scripts/agents/{agent}/provider");
    let permission = format!("/api/agents/{agent}/scripts");
    let turns = format!("/api/scripts/agents/{agent}/turns");
    script(&app, Method::GET, &path, None, auth, 403, &secret).await?;
    app.post(
        &permission,
        json!({"id": new_id(), "revision": 0, "enabled": true}),
    )
    .await?;
    let command = json!({"id": new_id(), "revision": a["revision"], "model": "new", "providerAccount": "chosen", "backupModel": ""});
    let mut extra = command.clone();
    extra["enabled"] = json!(true);
    script(&app, Method::POST, &path, Some(&extra), auth, 400, &secret).await?;
    let origin = app.origin().to_owned();
    script(
        &app,
        Method::POST,
        &path,
        Some(&command),
        &[
            ("Authorization", bearer.as_str()),
            ("Origin", origin.as_str()),
        ],
        403,
        &secret,
    )
    .await?;
    let changed = script(
        &app,
        Method::POST,
        &path,
        Some(&command),
        auth,
        200,
        &secret,
    )
    .await?;
    ensure!(
        changed["name"] == a["name"] && changed["instructions"] == a["instructions"],
        "identity changed: {changed}"
    );
    ensure!(changed["model"] == "new", "model: {changed}");
    ensure!(
        script(
            &app,
            Method::POST,
            &path,
            Some(&command),
            auth,
            200,
            &secret
        )
        .await?
            == changed,
        "receipt"
    );
    let mut stale = command.clone();
    stale["id"] = json!(new_id());
    script(&app, Method::POST, &path, Some(&stale), auth, 409, &secret).await?;
    app.post(
        &permission,
        json!({"id": new_id(), "revision": 1, "enabled": false}),
    )
    .await?;
    app.post(
        &permission,
        json!({"id": new_id(), "revision": 2, "enabled": true}),
    )
    .await?;
    let message = json!({"id": new_id(), "text": "Ready"});
    script(
        &app,
        Method::POST,
        &turns,
        Some(&message),
        auth,
        202,
        &secret,
    )
    .await?;
    until_value(
        "script turn",
        || script(&app, Method::GET, &turns, None, auth, 200, &secret),
        |rows| rows[0]["status"] == "done",
    )
    .await?;
    let calls: Vec<(String, Option<String>)> = model
        .requests()
        .iter()
        .map(|request| (request.model().to_owned(), request.authorization.clone()))
        .collect();
    ensure!(
        calls == [("new".to_owned(), Some(format!("Bearer {secret}")))],
        "provider calls: {calls:?}"
    );
    let renamed = app
        .post(
            &format!("/api/agents/{agent}/edit"),
            json!({"id": new_id(), "revision": changed["revision"], "name": "Renamed", "model": changed["model"], "instructions": changed["instructions"]}),
        )
        .await?;
    ensure!(
        script(
            &app,
            Method::POST,
            &path,
            Some(&command),
            auth,
            200,
            &secret
        )
        .await?
            == changed,
        "receipt after rename"
    );
    ensure!(
        app.get("/api/agents").await?["agents"][0]["name"] == "Renamed",
        "rename lost"
    );
    let hold = json!({"id": new_id(), "text": "HOLD"});
    script(&app, Method::POST, &turns, Some(&hold), auth, 202, &secret).await?;
    let held = model.deferred().await?;
    let mut busy = command.clone();
    busy["id"] = json!(new_id());
    busy["revision"] = renamed["revision"].clone();
    busy["model"] = json!("busy");
    script(&app, Method::POST, &path, Some(&busy), auth, 409, &secret).await?;
    app.post(
        &format!(
            "/api/agents/{agent}/turns/{}/cancel",
            hold["id"].as_str().ok_or("id")?
        ),
        json!({"id": new_id()}),
    )
    .await?;
    drop(held);
    app.restart().await?;
    ensure!(
        script(
            &app,
            Method::POST,
            &path,
            Some(&command),
            auth,
            200,
            &secret
        )
        .await?
            == changed,
        "receipt after restart"
    );
    ensure!(model.calls() == 2, "restart replayed: {}", model.calls());
    let mut late = command.clone();
    late["id"] = json!(new_id());
    late["revision"] = renamed["revision"].clone();
    let raw = late.to_string();
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
    app.post(
        &permission,
        json!({"id": new_id(), "revision": 3, "enabled": false}),
    )
    .await?;
    ensure!(
        delayed.finish().await? == 403,
        "in-flight change outlived the revocation"
    );
    script(
        &app,
        Method::POST,
        &path,
        Some(&command),
        auth,
        403,
        &secret,
    )
    .await?;
    script(&app, Method::GET, &path, None, auth, 403, &secret).await?;
    Ok(())
}
