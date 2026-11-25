//! Image generation (port of `app/image-generation.test.ts`).

use minarbot_testkit::{
    base64::{engine::general_purpose::STANDARD, Engine},
    images::{ImageServer, Mode},
    model::{Reply, ScriptedModel},
    prelude::*,
    server::Gateway,
};
use std::sync::{Arc, Mutex};

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aZ84AAAAASUVORK5CYII=";

#[derive(Default)]
struct Chat {
    disabled: bool,
    hold: bool,
    continuation: bool,
    seen_image: bool,
    violations: Vec<String>,
    downloads: Vec<(String, String)>,
}

fn chat_model(model: &ScriptedModel, chat: Arc<Mutex<Chat>>) {
    model.respond(move |request| {
        let Ok(mut chat) = chat.lock() else {
            return Reply::status(500, json!({"error": "poisoned"}));
        };
        if request.authorization.as_deref() != Some("Bearer chat-secret") {
            let seen = format!("chat authorization: {:?}", request.authorization);
            chat.violations.push(seen);
        }
        if request.body.to_string().contains("image-secret") {
            chat.violations
                .push("image key reached the chat model".into());
        }
        let offered = request
            .tool_names()
            .iter()
            .any(|name| name == "generate_image");
        if chat.disabled {
            if offered {
                chat.violations
                    .push("generate_image offered while disabled".into());
            }
            return Reply::text("Image provider disabled.");
        }
        if !offered {
            chat.violations.push("generate_image not offered".into());
        }
        let results = request.tool_results();
        if !results.is_empty() {
            chat.continuation = true;
            if chat.hold {
                return Reply::hold();
            }
        }
        if request.messages().iter().any(|message| {
            message["content"]
                .as_array()
                .is_some_and(|parts| parts.iter().any(|part| part["type"] == "image_url"))
        }) {
            chat.seen_image = true;
        }
        let result: Option<Value> = results
            .first()
            .and_then(|result| serde_json::from_str(result["content"].as_str()?).ok());
        if let Some(id) = result.as_ref().and_then(|result| result["id"].as_str()) {
            let url = result
                .as_ref()
                .and_then(|result| result["downloadUrl"].as_str())
                .unwrap_or("")
                .to_owned();
            chat.downloads.push((id.to_owned(), url));
        }
        match (
            results.len(),
            result.as_ref().and_then(|result| result["id"].as_str()),
        ) {
            (0, _) => Reply::tool_call(
                "generate",
                "generate_image",
                json!({"prompt": "Synthetic landscape"}),
            ),
            (1, Some(id)) => Reply::tool_call("read", "read_attachment", json!({"id": id})),
            _ => Reply::text("Image complete or uncertain receipt checked."),
        }
    });
}

fn download_url(chat: &Arc<Mutex<Chat>>, id: &str) -> Option<String> {
    chat.lock().ok().and_then(|chat| {
        chat.downloads
            .iter()
            .rev()
            .find(|(seen, _)| seen == id)
            .map(|(_, url)| url.clone())
    })
}

/// `downloadUrl` is `/api/(agents|threads|channels)/<uuid>/files/<id>`.
fn valid_download(url: &str, id: &str) -> bool {
    let Some(rest) = url.strip_prefix("/api/") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('/').collect();
    parts.len() == 4
        && ["agents", "threads", "channels"].contains(&parts[0])
        && !parts[1].is_empty()
        && parts[1]
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase() || c == '-')
        && parts[2] == "files"
        && parts[3] == id
}

#[tokio::test]
async fn image_generation_preserves_scope_uncertain_receipts_and_exact_artifacts_through_restart(
) -> Result<()> {
    let png = STANDARD.decode(PNG)?;
    let images = ImageServer::start(
        "image-secret",
        json!({"model": "fixture-image", "prompt": "Synthetic landscape", "n": 1,
            "size": "1024x1024", "output_format": "png"}),
        png.clone(),
    )
    .await?;
    let model = ScriptedModel::start().await?;
    let chat = Arc::new(Mutex::new(Chat::default()));
    chat_model(&model, chat.clone());
    let config = ServerConfig::new()
        .gateway(Gateway {
            api_key: Some("chat-secret".into()),
            ..Gateway::new(model.base_url())
        })
        .option(
            "imageProvider",
            json!({"baseURL": images.base_url(), "model": "fixture-image",
                "apiKey": "image-secret"}),
        );
    let root = TestRoot::new()?;
    let mut app = root.start(config.clone()).await?;
    let a = app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Artist", "model": "chat",
                "instructions": "Generate"}),
        )
        .await?;
    let a_id = a["id"].as_str().ok_or("agent id")?.to_owned();
    let side = app
        .post(
            &format!("/api/agents/{a_id}/threads"),
            json!({"id": new_id(), "name": "Side"}),
        )
        .await?;
    let room = app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Room", "members": [a_id], "lead": a_id,
                "instructions": "Generate"}),
        )
        .await?;
    let scopes = [
        format!("/api/agents/{a_id}"),
        format!("/api/threads/{}", side["id"].as_str().ok_or("side id")?),
        format!("/api/channels/{}", room["id"].as_str().ok_or("room id")?),
    ];
    let send = |path: &str, text: &str| {
        let mut body = json!({"id": new_id(), "text": text});
        if path.contains("channels") {
            body["recipient"] = json!(a_id);
        }
        body
    };

    // Each scope stores exactly one artifact, downloadable only in that scope.
    for path in &scopes {
        if let Ok(mut chat) = chat.lock() {
            chat.seen_image = false;
        }
        let body = send(path, "Image");
        let id = body["id"].as_str().ok_or("turn id")?.to_owned();
        app.post(&format!("{path}/turns"), body).await?;
        let row = app
            .wait_status(&format!("{path}/turns"), &id, "done")
            .await?;
        let generated = row["generatedFiles"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        ensure!(generated.len() == 1, "generated: {}", row["generatedFiles"]);
        ensure!(
            chat.lock().is_ok_and(|chat| chat.seen_image),
            "model did not receive the image pixels"
        );
        let file_id = generated[0]["id"].as_str().ok_or("file id")?;
        let url = download_url(&chat, file_id).ok_or("download url")?;
        ensure!(valid_download(&url, file_id), "downloadUrl: {url}");
        ensure!(
            url == format!("{path}/files/{file_id}"),
            "downloadUrl scope: {url}"
        );
        let response = app.client()?.get(app.url(&url)).send().await?;
        let disposition = response
            .headers()
            .get("content-disposition")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned();
        ensure!(
            response.bytes().await?.to_vec() == png,
            "artifact bytes differ"
        );
        ensure!(
            disposition.contains("attachment"),
            "disposition: {disposition}"
        );
        let other = scopes
            .iter()
            .find(|other| *other != path)
            .ok_or("other scope")?;
        let foreign = app
            .client()?
            .get(app.url(&format!("{other}/files/{file_id}")))
            .send()
            .await?;
        ensure!(
            foreign.status().as_u16() == 404,
            "artifact visible in another scope"
        );
    }
    let path = scopes[0].clone();
    let turns = format!("{path}/turns");

    // A 4 MiB image is the boundary that still succeeds.
    images.set_mode(Mode::Large);
    let large = send(&path, "Large image boundary");
    let large_id = large["id"].as_str().ok_or("id")?.to_owned();
    app.post(&turns, large).await?;
    let large_row = app.wait_status(&turns, &large_id, "done").await?;
    ensure!(
        large_row["generatedFiles"][0]["size"] == 4 * 1024 * 1024,
        "large size: {}",
        large_row["generatedFiles"]
    );

    // A restart after generation resumes without generating again.
    images.set_mode(Mode::Ok);
    if let Ok(mut chat) = chat.lock() {
        chat.hold = true;
        chat.continuation = false;
    }
    let restart = send(&path, "Restart");
    let restart_id = restart["id"].as_str().ok_or("id")?.to_owned();
    app.post(&turns, restart).await?;
    until("continuation after generate_image", || async {
        Ok(chat
            .lock()
            .is_ok_and(|chat| chat.continuation)
            .then_some(()))
    })
    .await?;
    let before = images.calls();
    app.restart().await?;
    if let Ok(mut chat) = chat.lock() {
        chat.hold = false;
    }
    let stopped = app.wait_status(&turns, &restart_id, "interrupted").await?;
    app.post(
        &format!("{turns}/{restart_id}/resume"),
        json!({"id": new_id(), "revision": stopped["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    let resumed = app.wait_status(&turns, &restart_id, "done").await?;
    ensure!(
        resumed["generatedFiles"] == stopped["generatedFiles"],
        "resume changed artifacts: {} vs {}",
        resumed["generatedFiles"],
        stopped["generatedFiles"]
    );
    ensure!(images.calls() == before, "resume generated again");

    // Lost, invalid, URL and redirected results fail; resume never retries them.
    for mode in [Mode::Lost, Mode::Invalid, Mode::Url, Mode::Redirect] {
        images.set_mode(mode);
        let body = send(&path, &format!("{mode:?}").to_lowercase());
        let id = body["id"].as_str().ok_or("id")?.to_owned();
        app.post(&turns, body).await?;
        let failed = app.wait_status(&turns, &id, "failed").await?;
        ensure!(
            failed["generatedFiles"]
                .as_array()
                .is_some_and(Vec::is_empty),
            "{mode:?}: {}",
            failed["generatedFiles"]
        );
        let count = images.calls();
        app.post(
            &format!("{turns}/{id}/resume"),
            json!({"id": new_id(), "revision": failed["revision"], "acknowledgeUncertain": true}),
        )
        .await?;
        app.wait_status(&turns, &id, "done").await?;
        ensure!(
            images.calls() == count,
            "{mode:?}: resume retried generation"
        );
    }

    // Cancelling while the image provider is answering stores nothing.
    images.set_mode(Mode::Late);
    let cancel = send(&path, "Cancel");
    let cancel_id = cancel["id"].as_str().ok_or("id")?.to_owned();
    app.post(&turns, cancel).await?;
    until("image request held", || async {
        Ok(images.holding().then_some(()))
    })
    .await?;
    app.post(
        &format!("{turns}/{cancel_id}/cancel"),
        json!({"id": new_id()}),
    )
    .await?;
    images.release();
    let cancelled = app.wait_status(&turns, &cancel_id, "cancelled").await?;
    ensure!(
        cancelled["generatedFiles"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "cancelled: {}",
        cancelled["generatedFiles"]
    );

    // Usage records received and failed image calls.
    let usage = app.get("/api/usage").await?;
    let rows: Vec<Value> = usage["calls"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| row["phase"] == "image")
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    ensure!(
        rows.iter()
            .any(|row| row["status"] == "received" && row["total_tokens"] == 11),
        "received image usage: {rows:?}"
    );
    ensure!(
        rows.iter().any(|row| row["status"] == "failed"),
        "failed image usage: {rows:?}"
    );

    // Without an image provider the tool is not offered.
    app.restart_with(config.without("imageProvider")).await?;
    if let Ok(mut chat) = chat.lock() {
        chat.disabled = true;
    }
    let disabled = send(&path, "No image provider");
    let disabled_id = disabled["id"].as_str().ok_or("id")?.to_owned();
    let before_disabled = images.calls();
    app.post(&turns, disabled).await?;
    app.wait_status(&turns, &disabled_id, "done").await?;
    ensure!(
        images.calls() == before_disabled,
        "disabled provider called"
    );
    ensure!(
        images.violations().is_empty(),
        "image contract: {:?}",
        images.violations()
    );
    let violations = chat
        .lock()
        .map(|chat| chat.violations.clone())
        .unwrap_or_default();
    ensure!(violations.is_empty(), "chat contract: {violations:?}");
    Ok(())
}
