//! Phone push (#109): device registration, delivery through fake APNs and
//! FCM endpoints when an agent asks or finishes, and token removal.

use minarbot_testkit::{
    db::Inspector,
    http::Method,
    model::{ModelRequest, Reply, ScriptedModel},
    prelude::*,
    push::{self, FakePush, PushRequest},
    wait::never,
    Server,
};
use std::time::Duration;

const DEVICES: &str = "/api/push/devices";
const IOS_TOKEN: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

fn android(token: &str, name: &str) -> Value {
    json!({"id": new_id(), "platform": "android", "token": token, "name": name,
        "appId": "dev.minarbot.app"})
}

fn ios(token: &str) -> Value {
    json!({"id": new_id(), "platform": "ios", "token": token, "name": "iPhone",
        "appId": "dev.minarbot.app", "environment": "development"})
}

fn id(value: &Value) -> String {
    value["id"].as_str().unwrap_or_default().to_owned()
}

async fn agent(app: &Server, name: &str) -> Result<String> {
    let agent = app
        .post_expect(
            "/api/agents",
            json!({"id": new_id(), "runtime": "api", "name": name, "model": "fixture",
                "instructions": "Help."}),
            201,
        )
        .await?;
    Ok(id(&agent))
}

/// Asks one question on the first step, answers with text afterwards.
fn asking_model(model: &ScriptedModel) {
    model.respond(|request: &ModelRequest| {
        if request.last_user_text().contains("ASK") && !request.called("ask_user") {
            Reply::tool_call(
                "question",
                "ask_user",
                json!({"question": "Which path?", "choices": ["Yes", "No"]}),
            )
        } else {
            Reply::text("Done.")
        }
    });
}

async fn pushes_where(
    fake: &FakePush,
    count: usize,
    label: &str,
    keep: impl Fn(&PushRequest) -> bool,
) -> Result<Vec<PushRequest>> {
    until(label.to_owned(), || {
        let pushes: Vec<_> = fake.pushes().into_iter().filter(|p| keep(p)).collect();
        async move { Ok((pushes.len() >= count).then_some(pushes)) }
    })
    .await
}

fn kind(request: &PushRequest) -> String {
    let body = request.json();
    let data = if request.path.starts_with("/3/device/") {
        body
    } else {
        body["message"]["data"].clone()
    };
    data["kind"].as_str().unwrap_or_default().to_owned()
}

#[tokio::test]
async fn push_devices_register_update_validate_and_remove_without_configuration() -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|_| Reply::text("Done."));
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new().model(&model)).await?;
    let empty = app.get(DEVICES).await?;
    ensure!(
        empty == json!({"configured": {"ios": false, "android": false}, "devices": []}),
        "empty list: {empty}"
    );

    let token = "fcm-token:APA91b_registered-0123456789";
    let created = app
        .post_expect(DEVICES, android(token, "Pixel 8"), 201)
        .await?;
    let device = &created["device"];
    ensure!(
        device["platform"] == "android"
            && device["name"] == "Pixel 8"
            && device["appId"] == "dev.minarbot.app"
            && device["environment"] == "production"
            && device["token"] == "fcm-toke…"
            && device["lastSentAt"].is_null()
            && created["configured"] == json!({"ios": false, "android": false}),
        "created: {created}"
    );
    let again = app
        .post_expect(DEVICES, android(token, "  Work phone "), 200)
        .await?;
    ensure!(
        again["device"]["id"] == device["id"] && again["device"]["name"] == "Work phone",
        "re-register keeps the device: {again}"
    );
    let phone = app.post_expect(DEVICES, ios(IOS_TOKEN), 201).await?;
    ensure!(
        phone["device"]["environment"] == "development",
        "ios: {phone}"
    );
    let listed = app.get(DEVICES).await?;
    ensure!(
        listed["devices"].as_array().map(Vec::len) == Some(2)
            && !listed.to_string().contains(token)
            && !listed.to_string().contains(IOS_TOKEN),
        "list masks tokens: {listed}"
    );

    let mut invalid = vec![
        json!({"id": new_id(), "platform": "web", "token": "t", "name": "n", "appId": "a"}),
        android("has space", "n"),
        android("", "n"),
        android(&"a".repeat(4097), "n"),
        android("t", " "),
        android("t", &"n".repeat(81)),
        android("t", "a\nb"),
        ios("not-hex-token-not-hex-token-not-hex-token"),
        ios("abcd"),
    ];
    let mut bad_app = android("t", "n");
    bad_app["appId"] = json!("dev/minarbot");
    let mut bad_environment = android("t", "n");
    bad_environment["environment"] = json!("staging");
    let mut extra = android("t", "n");
    extra["secret"] = json!("x");
    let mut no_id = android("t", "n");
    no_id["id"] = json!("not-a-uuid");
    invalid.extend([bad_app, bad_environment, extra, no_id]);
    for body in invalid {
        let reply = app.call(Method::POST, DEVICES, Some(body.clone())).await?;
        ensure!(
            reply.status == 400 && reply.body["error"].is_string(),
            "{body}: HTTP {} {}",
            reply.status,
            reply.body
        );
    }
    let foreign = app
        .request(Method::GET, DEVICES)?
        .header("authorization", "Bearer token")
        .send()
        .await?;
    ensure!(
        foreign.status() == 403,
        "authorization header: {}",
        foreign.status()
    );
    for origin in [Some("https://example.invalid"), None] {
        let mut request = app
            .client()?
            .post(app.url(DEVICES))
            .header("sec-fetch-site", "same-origin")
            .header("content-type", "application/json")
            .body(android("t", "n").to_string());
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        let reply = request.send().await?;
        ensure!(
            reply.status() == 403,
            "origin {origin:?}: {}",
            reply.status()
        );
    }

    let android_id = id(device);
    let test = app
        .call(
            Method::POST,
            &format!("{DEVICES}/{android_id}/test"),
            Some(json!({"id": new_id()})),
        )
        .await?;
    ensure!(
        test.status == 409 && test.body["error"] == "Push is not configured for Android.",
        "test without configuration: HTTP {} {}",
        test.status,
        test.body
    );

    // Without configuration a finished turn records its event but queues no push.
    let agent = agent(&app, "Ada").await?;
    let turns = format!("/api/agents/{agent}/turns");
    let turn = new_id();
    app.post_expect(&turns, json!({"id": turn, "text": "hello"}), 202)
        .await?;
    app.wait_status(&turns, &turn, "done").await?;
    let db = Inspector::open(root.database())?;
    let key = format!("finished:{turn}");
    until("finished event recorded", || {
        let seen = db.scalar::<i64>("SELECT count(*) FROM push_events WHERE key=?", [&key]);
        async move { Ok((seen? == 1).then_some(())) }
    })
    .await?;
    ensure!(
        db.scalar::<i64>("SELECT count(*) FROM push_outbox", [])? == 0,
        "no queued pushes without configuration"
    );

    app.post_expect(
        &format!("{DEVICES}/{android_id}/delete"),
        json!({"id": new_id()}),
        200,
    )
    .await?;
    app.post_expect(
        &format!("{DEVICES}/{android_id}/delete"),
        json!({"id": new_id()}),
        404,
    )
    .await?;
    let left = app.get(DEVICES).await?;
    ensure!(
        left["devices"].as_array().map(Vec::len) == Some(1)
            && left["devices"][0]["platform"] == "ios",
        "after delete: {left}"
    );
    Ok(())
}

#[tokio::test]
async fn a_question_and_a_finished_turn_push_once_to_each_device_across_restarts() -> Result<()> {
    let fake = FakePush::start().await?;
    let model = ScriptedModel::start().await?;
    asking_model(&model);
    let root = TestRoot::new()?;
    let config = fake.configure(&root, ServerConfig::new().model(&model))?;
    let mut app = root.start(config).await?;
    let fcm_token = "fcm-token:APA91b_phone";
    let registered = app
        .post_expect(DEVICES, android(fcm_token, "Pixel"), 201)
        .await?;
    ensure!(
        registered["configured"] == json!({"ios": true, "android": true}),
        "configured: {registered}"
    );
    app.post_expect(DEVICES, ios(IOS_TOKEN), 201).await?;
    let agent = agent(&app, "Ada").await?;
    let turns = format!("/api/agents/{agent}/turns");
    let turn = new_id();
    app.post_expect(&turns, json!({"id": turn, "text": "ASK please"}), 202)
        .await?;
    let asked = app
        .wait_row(&turns, &turn, |row| {
            row["questions"]
                .as_array()
                .is_some_and(|questions| !questions.is_empty())
        })
        .await?;

    let questions = pushes_where(&fake, 2, "question pushes", |p| kind(p) == "question").await?;
    let apns = questions
        .iter()
        .find(|p| p.path.starts_with("/3/device/"))
        .ok_or("no APNs push")?;
    ensure!(
        apns.path == format!("/3/device/{IOS_TOKEN}")
            && apns.http2
            && apns.header("apns-topic") == Some("dev.minarbot.app")
            && apns.header("apns-push-type") == Some("alert")
            && apns.header("apns-priority") == Some("10"),
        "APNs request: {apns:?}"
    );
    let claims = push::apns_claims(apns)?;
    ensure!(
        claims["iss"] == push::TEAM_ID && claims["iat"].is_i64(),
        "APNs claims: {claims}"
    );
    ensure!(
        apns.json()
            == json!({"aps": {"alert": {"title": "Ada", "body": "Has a question for you"},
                "sound": "default", "thread-id": agent}, "kind": "question", "agent": agent,
                "turn": turn, "focus": turn, "path": format!("/agents/{agent}")}),
        "APNs body: {}",
        apns.json()
    );
    let fcm = questions
        .iter()
        .find(|p| p.path.starts_with("/v1/"))
        .ok_or("no FCM push")?;
    let message = &fcm.json()["message"];
    ensure!(
        fcm.path == format!("/v1/projects/{}/messages:send", push::PROJECT)
            && fcm.header("authorization") == Some(&*format!("Bearer {}", push::ACCESS_TOKEN))
            && message["token"] == fcm_token
            && message["notification"] == json!({"title": "Ada", "body": "Has a question for you"})
            && message["data"]
                == json!({"kind": "question", "agent": agent, "turn": turn, "focus": turn,
                    "path": format!("/agents/{agent}")})
            && message["android"]["priority"] == "high",
        "FCM request: {}",
        fcm.json()
    );
    let token_request = fake
        .requests()
        .into_iter()
        .find(|p| p.path == "/token")
        .ok_or("no OAuth2 token request")?;
    let assertion = push::fcm_assertion_claims(&token_request)?;
    ensure!(
        assertion["iss"] == push::CLIENT_EMAIL
            && assertion["scope"] == "https://www.googleapis.com/auth/firebase.messaging"
            && assertion["aud"] == format!("{}/token", fake.origin()),
        "FCM assertion: {assertion}"
    );

    let question = asked["questions"][0]["id"].as_str().unwrap_or_default();
    app.post_expect(
        &format!("{turns}/{turn}/questions/{question}/answer"),
        json!({"id": new_id(), "text": "Yes"}),
        200,
    )
    .await?;
    app.wait_status(&turns, &turn, "done").await?;
    let finished = pushes_where(&fake, 2, "finished pushes", |p| kind(p) == "finished").await?;
    ensure!(
        finished
            .iter()
            .any(|p| p.json()["aps"]["alert"]["body"] == "Finished")
            && finished
                .iter()
                .any(|p| p.json()["message"]["notification"]["body"] == "Finished"),
        "finished bodies: {finished:?}"
    );

    app.restart().await?;
    never(
        "pushes repeated after restart",
        Duration::from_secs(4),
        || {
            let count = fake.pushes().len();
            async move { Ok(count != 4) }
        },
    )
    .await?;
    ensure!(
        fake.requests()
            .iter()
            .filter(|p| p.path == "/token")
            .count()
            == 1,
        "the FCM access token is cached"
    );
    let devices = app.get(DEVICES).await?;
    ensure!(
        devices["devices"].as_array().is_some_and(|rows| rows
            .iter()
            .all(|d| d["lastSentAt"].is_string() && d["lastError"].is_null())),
        "delivery recorded: {devices}"
    );
    Ok(())
}

#[tokio::test]
async fn unregistered_tokens_remove_their_devices() -> Result<()> {
    let fake = FakePush::start().await?;
    let model = ScriptedModel::start().await?;
    model.respond(|_| Reply::text("Done."));
    let root = TestRoot::new()?;
    let config = fake.configure(&root, ServerConfig::new().model(&model))?;
    let app = root.start(config).await?;
    let good = app
        .post_expect(DEVICES, android("fcm-token:good", "Good"), 201)
        .await?;
    let test = app
        .post_expect(
            &format!("{DEVICES}/{}/test", id(&good["device"])),
            json!({"id": new_id()}),
            200,
        )
        .await?;
    ensure!(test == json!({"result": "sent"}), "test push: {test}");
    let sent = fake.pushes();
    ensure!(
        sent.len() == 1 && sent[0].json()["message"]["data"]["kind"] == "test",
        "test push request: {sent:?}"
    );

    let gone_fcm = "fcm-token:gone";
    fake.answer(
        gone_fcm,
        404,
        json!({"error": {"code": 404, "status": "NOT_FOUND", "message": "Requested entity was not found.",
            "details": [{"@type": "type.googleapis.com/google.firebase.fcm.v1.FcmError",
                "errorCode": "UNREGISTERED"}]}}),
    );
    let gone = app
        .post_expect(DEVICES, android(gone_fcm, "Old"), 201)
        .await?;
    let removed = app
        .post_expect(
            &format!("{DEVICES}/{}/test", id(&gone["device"])),
            json!({"id": new_id()}),
            200,
        )
        .await?;
    ensure!(
        removed == json!({"result": "removed"}),
        "test on a gone token: {removed}"
    );

    // An event push to an unregistered APNs token removes the device too.
    fake.answer(
        IOS_TOKEN,
        410,
        json!({"reason": "Unregistered", "timestamp": 1}),
    );
    app.post_expect(DEVICES, ios(IOS_TOKEN), 201).await?;
    let agent = agent(&app, "Ada").await?;
    let turns = format!("/api/agents/{agent}/turns");
    let turn = new_id();
    app.post_expect(&turns, json!({"id": turn, "text": "hello"}), 202)
        .await?;
    app.wait_status(&turns, &turn, "done").await?;
    let left = until_value(
        "unregistered iOS device removed",
        || app.get(DEVICES),
        |list| list["devices"].as_array().map(Vec::len) == Some(1),
    )
    .await?;
    ensure!(
        left["devices"][0]["id"] == good["device"]["id"],
        "only the good device stays: {left}"
    );
    pushes_where(&fake, 1, "finished push to the good device", |p| {
        kind(p) == "finished" && p.device_token().as_deref() == Some("fcm-token:good")
    })
    .await?;
    Ok(())
}
