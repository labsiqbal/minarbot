//! Teammate reply capture into new user turns (port of
//! `app/agent-reply-capture.test.ts`).

use minarbot_testkit::{
    db::Inspector,
    model::ModelRequest,
    parked::{Answer, ParkedModel, Route},
    prelude::*,
    server::Gateway,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

const ROUTING: &str = "Choose exactly one channel member";
const TEAMMATES: &str = "Teammate results available";
const ATTACHED: &str = "Some teammate results were attached";

struct Script {
    targets: Mutex<Vec<String>>,
    hold_gamma: AtomicBool,
    fail_consumer: AtomicBool,
}

fn text(message: &Value) -> Option<&str> {
    message["content"].as_str()
}

fn route(script: &Script, request: &ModelRequest) -> Route {
    let prompt = request.last_user_text();
    let has_tool = !request.tool_results().is_empty();
    let targets = script.targets.lock().map(|t| t.clone()).unwrap_or_default();
    match request.model() {
        "gamma" if script.hold_gamma.load(Ordering::SeqCst) => return Route::Park("gamma".into()),
        "gamma" => return Route::Now(Answer::text("LATE_GAMMA")),
        "beta" if !has_tool => {
            return Route::Now(Answer::tool(
                "create_text_file",
                json!({"name": "ready.txt", "text": "READY_ARTIFACT"}),
            ))
        }
        "beta" => return Route::Now(Answer::text("READY_BETA")),
        _ => {}
    }
    if request.system().starts_with(ROUTING) {
        return Route::Now(Answer::text(
            json!({"action": "assign", "recipient": targets.get(2)}).to_string(),
        ));
    }
    if prompt == "start group" && !has_tool {
        return Route::Now(Answer::tool(
            "send_message",
            json!({"recipients": targets, "text": "group request", "expects_reply": true}),
        ));
    }
    if prompt == "use ready results" && script.fail_consumer.swap(false, Ordering::SeqCst) {
        return Route::Now(Answer::fail("unavailable"));
    }
    if prompt == "use ready results" && !has_tool {
        let reference = request
            .messages()
            .iter()
            .filter_map(text)
            .find(|content| content.starts_with(TEAMMATES))
            .unwrap_or("");
        let data: Value = reference
            .find(": ")
            .and_then(|index| serde_json::from_str(&reference[index + 2..]).ok())
            .unwrap_or(Value::Null);
        return Route::Now(Answer::tool(
            "read_attachment",
            json!({"id": data[0]["generatedFiles"][0]["id"]}),
        ));
    }
    if prompt == "WITHDRAWN_REQUEST_DO_NOT_EXECUTE" {
        return Route::Park("withdrawn".into());
    }
    Route::Now(Answer::text("Caller complete"))
}

fn release_all(model: &ParkedModel) -> Result<()> {
    while model.parked("gamma") {
        model.finish("gamma", Answer::text("LATE_GAMMA"))?;
    }
    while model.parked("withdrawn") {
        model.finish("withdrawn", Answer::text("Withdrawn reply"))?;
    }
    Ok(())
}

async fn submit(app: &Server, path: &str, text: &str, extra: Value) -> Result<String> {
    let id = new_id();
    let mut body = json!({"id": id, "text": text});
    if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            body.insert(key.clone(), value.clone());
        }
    }
    app.post_expect(path, body, 202).await?;
    Ok(id)
}

fn has_done_message(row: &Value) -> bool {
    row["agentMessages"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item["status"] == "done"))
}

fn received(row: &Value) -> usize {
    row["receivedAgentReplies"].as_array().map_or(0, Vec::len)
}

fn teammates_message(request: &ModelRequest) -> Option<Value> {
    request
        .messages()
        .iter()
        .find(|message| text(message).is_some_and(|content| content.starts_with(TEAMMATES)))
        .cloned()
}

#[tokio::test]
async fn new_user_turns_capture_only_ready_same_scope_group_replies_once_across_retries_and_restart(
) -> Result<()> {
    let script = Arc::new(Script {
        targets: Mutex::new(Vec::new()),
        hold_gamma: AtomicBool::new(true),
        fail_consumer: AtomicBool::new(true),
    });
    let routed = script.clone();
    let model = ParkedModel::start(move |request| route(&routed, request)).await?;
    let root = TestRoot::new()?;
    let mut app = root
        .start(ServerConfig::new().gateway(Gateway::new(model.base_url())))
        .await?;
    let mut ids = Vec::new();
    for (name, model_name, instructions) in [
        ("Alpha", "alpha", "Coordinate"),
        ("Beta", "beta", "Help"),
        ("Gamma", "gamma", "Help"),
    ] {
        let agent = app
            .post_expect(
                "/api/agents",
                json!({"runtime": "api", "id": new_id(), "name": name, "model": model_name,
                    "instructions": instructions}),
                201,
            )
            .await?;
        ids.push(agent["id"].as_str().ok_or("agent id")?.to_owned());
    }
    let (alpha, beta, gamma) = (ids[0].clone(), ids[1].clone(), ids[2].clone());
    let set_targets = |targets: Vec<String>| {
        if let Ok(mut slot) = script.targets.lock() {
            *slot = targets;
        }
    };
    set_targets(vec![beta.clone(), gamma.clone()]);
    let path = format!("/api/agents/{alpha}/turns");

    // A group request with one ready reply (Beta) and one held (Gamma).
    let group = submit(&app, &path, "start group", json!({})).await?;
    let waiting = app.wait_row(&path, &group, has_done_message).await?;
    let side = app
        .post(
            &format!("/api/agents/{alpha}/threads"),
            json!({"id": new_id(), "name": "Other scope"}),
        )
        .await?;
    let side_path = format!(
        "/api/threads/{}/turns",
        side["id"].as_str().ok_or("side id")?
    );
    let separate = submit(&app, &side_path, "other user message", json!({})).await?;
    let separate_row = app.wait_status(&side_path, &separate, "done").await?;
    ensure!(
        received(&separate_row) == 0,
        "other scope captured: {separate_row}"
    );

    // The next user turn captures the ready reply and its artifact once.
    let consumer = json!({"id": new_id(), "text": "use ready results"});
    let consumer_id = consumer["id"].as_str().ok_or("consumer id")?.to_owned();
    app.post_expect(&path, consumer.clone(), 202).await?;
    let failed = app.wait_status(&path, &consumer_id, "failed").await?;
    let replies = failed["receivedAgentReplies"].clone();
    ensure!(received(&failed) == 1, "captured: {replies}");
    ensure!(replies[0]["reply"] == "READY_BETA", "captured: {replies}");
    ensure!(
        replies[0]["source"] == group.as_str(),
        "captured: {replies}"
    );
    ensure!(
        replies[0]["generatedFiles"].as_array().map(Vec::len) == Some(1),
        "captured files: {replies}"
    );
    let artifact = replies[0]["generatedFiles"][0].clone();
    let artifact_thread = artifact["thread"]
        .as_str()
        .ok_or("artifact thread")?
        .to_owned();
    let artifact_id = artifact["id"].as_str().ok_or("artifact id")?.to_owned();
    let artifact_path = format!("/api/threads/{artifact_thread}/files/{artifact_id}");
    let download = app.client()?.get(app.url(&artifact_path)).send().await?;
    ensure!(download.text().await? == "READY_ARTIFACT", "artifact bytes");
    let first_attempt = model
        .requests()
        .into_iter()
        .find(|request| request.model() == "alpha" && request.last_text() == "use ready results")
        .ok_or("first consumer attempt")?;
    let reference = teammates_message(&first_attempt).ok_or("teammate results message")?;
    let reference_text = text(&reference).unwrap_or("");
    ensure!(
        reference_text.contains("READY_BETA"),
        "reference: {reference_text}"
    );
    ensure!(
        reference_text.contains(&artifact_id),
        "reference: {reference_text}"
    );
    app.post_expect(&path, consumer.clone(), 202).await?;
    let second = submit(&app, &path, "another message", json!({})).await?;
    let second_row = app.wait_status(&path, &second, "done").await?;
    ensure!(received(&second_row) == 0, "captured twice: {second_row}");

    // After a restart the receipt keeps the same capture; resume uses it.
    app.shutdown().await?;
    release_all(&model)?;
    script.hold_gamma.store(false, Ordering::SeqCst);
    let config = app.config().clone();
    app.start_stopped(config).await?;
    app.post_expect(&path, consumer.clone(), 202).await?;
    let replayed = app.row(&path, &consumer_id).await?.ok_or("consumer")?;
    ensure!(
        replayed["receivedAgentReplies"] == failed["receivedAgentReplies"],
        "restart changed the capture"
    );
    app.post(
        &format!("{path}/{consumer_id}/resume"),
        json!({"id": new_id(), "revision": failed["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    app.wait_status(&path, &consumer_id, "done").await?;
    let attempts: Vec<ModelRequest> = model
        .requests()
        .into_iter()
        .filter(|request| {
            request.model() == "alpha" && request.last_user_text() == "use ready results"
        })
        .collect();
    ensure!(attempts.len() == 3, "consumer attempts: {}", attempts.len());
    ensure!(
        attempts
            .iter()
            .any(|request| request.tool_results().iter().any(|result| {
                serde_json::from_str::<Value>(result["content"].as_str().unwrap_or(""))
                    .is_ok_and(|value| value["text"] == "READY_ARTIFACT")
            })),
        "artifact not read"
    );
    ensure!(
        teammates_message(&attempts[0]) == teammates_message(&attempts[1]),
        "retry changed the teammate reference"
    );
    app.wait_status(&path, &group, "done").await?;
    let continuation = model
        .requests()
        .into_iter()
        .find(|request| request.model() == "alpha" && request.last_text().starts_with(ATTACHED))
        .ok_or("group continuation")?;
    ensure!(
        continuation.last_text().contains("use ready results"),
        "continuation: {}",
        continuation.last_text()
    );
    // The send_message receipt; the gateway peer replays tool results before
    // the current user message as assistant messages.
    let receipt: Value = continuation
        .messages()
        .iter()
        .filter_map(|message| serde_json::from_str::<Value>(text(message)?).ok())
        .find(|value| value.get("delivered_earlier").is_some())
        .ok_or("group receipt")?;
    let done_item = waiting["agentMessages"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["status"] == "done"))
        .map(|item| item["id"].clone())
        .ok_or("done item")?;
    ensure!(
        !receipt["replies"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["id"] == done_item)),
        "delivered reply repeated: {receipt}"
    );
    ensure!(
        receipt["delivered_earlier"][0]["consumer"] == consumer_id.as_str(),
        "receipt: {receipt}"
    );
    ensure!(
        model
            .requests()
            .iter()
            .filter(|request| request.model() == "gamma")
            .count()
            == 1,
        "gamma calls"
    );

    // Empty captures stay empty even if more results become ready before a retry.
    script.hold_gamma.store(true, Ordering::SeqCst);
    let empty = submit(&app, &path, "empty snapshot", json!({})).await?;
    app.wait_status(&path, &empty, "done").await?;
    let next_group = submit(&app, &path, "start group", json!({})).await?;
    app.wait_row(&path, &next_group, has_done_message).await?;
    app.post_expect(&path, json!({"id": empty, "text": "empty snapshot"}), 202)
        .await?;
    ensure!(
        received(&app.row(&path, &empty).await?.ok_or("empty")?) == 0,
        "empty capture filled"
    );

    // Channels capture only for the owner of the group request.
    let channel = app
        .post_expect(
            "/api/channels",
            json!({"id": new_id(), "name": "Other channel", "instructions": "Coordinate",
                "members": [alpha, beta, gamma], "lead": alpha}),
            201,
        )
        .await?;
    let channel_path = format!(
        "/api/channels/{}/turns",
        channel["id"].as_str().ok_or("id")?
    );
    let isolated = submit(
        &app,
        &channel_path,
        "channel isolated",
        json!({"recipient": alpha}),
    )
    .await?;
    ensure!(
        received(&app.wait_status(&channel_path, &isolated, "done").await?) == 0,
        "channel captured direct replies"
    );
    let channel_group = submit(
        &app,
        &channel_path,
        "start group",
        json!({"recipient": alpha}),
    )
    .await?;
    app.wait_row(&channel_path, &channel_group, has_done_message)
        .await?;
    set_targets(vec![beta.clone(), gamma.clone(), alpha.clone()]);
    let routed = submit(
        &app,
        &channel_path,
        "automatic followup",
        json!({"automatic": true}),
    )
    .await?;
    let routed_row = app.wait_status(&channel_path, &routed, "done").await?;
    ensure!(received(&routed_row) == 1, "routed capture: {routed_row}");
    ensure!(
        routed_row["receivedAgentReplies"][0]["source"] == channel_group.as_str(),
        "routed capture: {routed_row}"
    );
    let other_owner = submit(
        &app,
        &channel_path,
        "different owner",
        json!({"recipient": beta}),
    )
    .await?;
    ensure!(
        received(&app.wait_status(&channel_path, &other_owner, "done").await?) == 0,
        "another member captured the replies"
    );
    app.post(
        &format!("{path}/{next_group}/cancel"),
        json!({"id": new_id()}),
    )
    .await?;
    app.post(
        &format!("{channel_path}/{channel_group}/cancel"),
        json!({"id": new_id()}),
    )
    .await?;
    release_all(&model)?;

    // A cancelled capturing turn forwards its recovery result, not its text.
    set_targets(vec![beta.clone(), gamma.clone()]);
    script.hold_gamma.store(true, Ordering::SeqCst);
    let cancel_group = submit(&app, &path, "start group", json!({})).await?;
    app.wait_row(&path, &cancel_group, has_done_message).await?;
    let withdrawn = submit(&app, &path, "WITHDRAWN_REQUEST_DO_NOT_EXECUTE", json!({})).await?;
    app.wait_status(&path, &withdrawn, "running").await?;
    app.post(
        &format!("{path}/{withdrawn}/cancel"),
        json!({"id": new_id()}),
    )
    .await?;
    let before_finish = model.calls();
    script.hold_gamma.store(false, Ordering::SeqCst);
    release_all(&model)?;
    app.wait_status(&path, &cancel_group, "done").await?;
    let cancelled_context = model
        .requests()
        .split_off(before_finish)
        .into_iter()
        .find(|request| request.model() == "alpha" && request.last_text().starts_with(ATTACHED))
        .ok_or("cancelled context")?;
    ensure!(
        !cancelled_context.mentions("WITHDRAWN_REQUEST_DO_NOT_EXECUTE"),
        "withdrawn text forwarded"
    );
    let last = cancelled_context.last_text();
    let forwarded: Value = serde_json::from_str(
        last.split("Reference data: ")
            .nth(1)
            .ok_or("reference data")?,
    )?;
    ensure!(
        forwarded[0]["status"] == "cancelled",
        "forwarded: {forwarded}"
    );
    ensure!(
        forwarded[0]["user_message"].is_null(),
        "forwarded: {forwarded}"
    );
    ensure!(
        forwarded[0]["recovery_result"]["reply"] == "READY_BETA",
        "forwarded: {forwarded}"
    );
    ensure!(
        app.row(&path, &withdrawn).await?.ok_or("withdrawn")?["receivedAgentReplies"][0]["reply"]
            == "READY_BETA",
        "withdrawn capture"
    );

    // Deleting the source thread hides the artifact but keeps the snapshot.
    app.shutdown().await?;
    let snapshot_sql = "SELECT snapshot FROM agent_reply_claims WHERE consumer=?";
    let snapshot: String = Inspector::open(app.database())?.scalar(snapshot_sql, [&consumer_id])?;
    let config = app.config().clone();
    app.start_stopped(config).await?;
    let threads = app.get(&format!("/api/agents/{beta}/threads")).await?;
    let source_thread = threads
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["id"] == artifact_thread.as_str())
        })
        .cloned()
        .ok_or("source thread")?;
    let archived = app
        .post(
            &format!("/api/threads/{artifact_thread}/edit"),
            json!({"id": new_id(), "revision": source_thread["revision"],
                "name": source_thread["name"], "archived": true}),
        )
        .await?;
    app.post(
        &format!("/api/threads/{artifact_thread}/delete"),
        json!({"id": new_id(), "revision": archived["revision"]}),
    )
    .await?;
    ensure!(
        app.row(&path, &consumer_id).await?.ok_or("consumer")?["receivedAgentReplies"][0]
            ["generatedFiles"]
            == json!([]),
        "deleted artifact still listed"
    );
    let gone = app.client()?.get(app.url(&artifact_path)).send().await?;
    ensure!(gone.status().as_u16() == 404, "deleted artifact readable");
    app.shutdown().await?;
    let after: String = Inspector::open(app.database())?.scalar(snapshot_sql, [&consumer_id])?;
    ensure!(after == snapshot, "claim snapshot changed");
    let config = app.config().clone();
    app.start_stopped(config).await?;
    Ok(())
}
