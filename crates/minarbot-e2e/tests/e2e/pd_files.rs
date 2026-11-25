//! Conversation files and text attachments (port of `app/files.test.ts`).

use minarbot_testkit::{
    http::SplitRequest,
    model::{ModelRequest, Reply, ScriptedModel},
    prelude::*,
    protocol::sha256_hex,
    upload::chunked_post,
};
use std::time::Duration;

fn id_of(value: &Value) -> Result<String> {
    Ok(value["id"].as_str().ok_or("missing id")?.to_owned())
}

/// `encodeURIComponent`.
fn encode_component(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => (byte as char).to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

struct Download {
    status: u16,
    disposition: String,
    content_type: String,
    bytes: Vec<u8>,
}

async fn fetch(app: &Server, path: &str) -> Result<Download> {
    let response = app.client()?.get(app.url(path)).send().await?;
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned()
    };
    let (disposition, content_type) = (header("content-disposition"), header("content-type"));
    let status = response.status().as_u16();
    let bytes = response.bytes().await?.to_vec();
    Ok(Download {
        status,
        disposition,
        content_type,
        bytes,
    })
}

async fn fetch_json(app: &Server, path: &str) -> Result<Value> {
    Ok(serde_json::from_slice(&fetch(app, path).await?.bytes)?)
}

async fn upload_from(
    app: &Server,
    path: &str,
    bytes: &[u8],
    name: &str,
    origin: &str,
) -> Result<(u16, Value)> {
    let response = app
        .client()?
        .post(app.url(path))
        .header("Origin", origin)
        .header("Content-Type", "application/octet-stream")
        .header("X-File-Name", encode_component(name))
        .body(bytes.to_vec())
        .send()
        .await?;
    let status = response.status().as_u16();
    let body = response.bytes().await?;
    let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    Ok((status, value))
}

async fn upload(app: &Server, path: &str, bytes: &[u8], name: &str) -> Result<(u16, Value)> {
    upload_from(app, path, bytes, name, app.origin()).await
}

/// Start an upload whose last body bytes arrive only on `finish`.
async fn delayed_upload(
    app: &Server,
    path: &str,
    name: &str,
    bytes: &[u8],
) -> Result<SplitRequest> {
    let origin = app.origin().to_owned();
    let request = SplitRequest::start(
        &origin,
        "POST",
        path,
        &[
            ("Origin", origin.as_str()),
            ("Content-Type", "application/octet-stream"),
            ("X-File-Name", name),
        ],
        bytes,
        bytes.len() - 1,
    )
    .await?;
    // Give the server the head and first byte before changing the scope.
    tokio::time::sleep(Duration::from_millis(20)).await;
    Ok(request)
}

#[tokio::test]
async fn files_preserve_bytes_scope_retry_identity_and_archive_boundaries() -> Result<()> {
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new()).await?;
    let agent = app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Files", "model": "fixture",
                "instructions": "Help."}),
        )
        .await?;
    let agent_id = id_of(&agent)?;
    let channel = app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Files room", "instructions": "Help.",
                "members": [agent_id], "lead": agent_id}),
        )
        .await?;
    let channel_id = id_of(&channel)?;
    let thread = app
        .post(
            &format!("/api/agents/{agent_id}/threads"),
            json!({"id": new_id(), "name": "Files side"}),
        )
        .await?;
    let thread_id = id_of(&thread)?;
    let direct = format!("/api/agents/{agent_id}/files");
    let room = format!("/api/channels/{channel_id}/files");
    let side = format!("/api/threads/{thread_id}/files");
    let id = new_id();
    let bytes: &[u8] = &[0, 255, 128, 10, 13, 65];
    let path = format!("{room}/{id}");

    let (status, receipt) = upload(&app, &path, bytes, "report é.bin").await?;
    ensure!(status == 200, "upload status {status}: {receipt}");
    ensure!(receipt["size"] == bytes.len(), "receipt: {receipt}");
    let metadata = fetch_json(&app, &format!("{path}?metadata=1")).await?;
    ensure!(
        metadata["digest"] == sha256_hex(bytes),
        "metadata: {metadata}"
    );
    ensure!(metadata["size"] == receipt["size"], "metadata: {metadata}");
    ensure!(metadata["id"] == id.as_str(), "metadata: {metadata}");
    ensure!(
        metadata.get("bytes").is_none(),
        "metadata carries bytes: {metadata}"
    );
    let (_, replay) = upload(&app, &path, bytes, "report é.bin").await?;
    ensure!(replay == receipt, "replay receipt: {replay}");
    ensure!(
        upload(&app, &path, b"different", "report é.bin").await?.0 == 409,
        "changed bytes accepted"
    );
    ensure!(
        upload(&app, &format!("{direct}/{id}"), bytes, "report é.bin")
            .await?
            .0
            == 409,
        "same id accepted in another scope"
    );
    ensure!(
        fetch(&app, &format!("{direct}/{id}")).await?.status == 404,
        "file visible in another scope"
    );
    ensure!(
        fetch(&app, &format!("{direct}/{id}?metadata=1"))
            .await?
            .status
            == 404,
        "metadata visible in another scope"
    );
    ensure!(
        fetch_json(&app, &direct).await? == json!([]),
        "direct list not empty"
    );
    let download = fetch(&app, &path).await?;
    ensure!(
        download.disposition.contains("attachment"),
        "disposition: {}",
        download.disposition
    );
    ensure!(
        download.disposition.contains("%C3%A9"),
        "disposition: {}",
        download.disposition
    );
    ensure!(
        download.content_type == "application/octet-stream",
        "content type: {}",
        download.content_type
    );
    ensure!(download.bytes == bytes, "bytes: {:?}", download.bytes);
    ensure!(
        fetch(&app, &format!("{path}?preview=text")).await?.status == 415,
        "binary preview"
    );

    // Text previews return the stored text as JSON, never as HTML.
    let text_id = new_id();
    let html = "<script>window.injected = true</script>\nHello";
    ensure!(
        upload(
            &app,
            &format!("{direct}/{text_id}"),
            html.as_bytes(),
            "example.html"
        )
        .await?
        .0 == 200,
        "html upload"
    );
    let preview = fetch(&app, &format!("{direct}/{text_id}?preview=text")).await?;
    ensure!(
        preview.content_type == "application/json; charset=utf-8",
        "preview type: {}",
        preview.content_type
    );
    let preview_body: Value = serde_json::from_slice(&preview.bytes)?;
    ensure!(preview_body["text"] == html, "preview: {preview_body}");
    ensure!(
        fetch(&app, &format!("{room}/{text_id}?preview=text"))
            .await?
            .status
            == 404,
        "preview in another scope"
    );
    let large_text = new_id();
    upload(
        &app,
        &format!("{direct}/{large_text}"),
        &vec![b'A'; 256 * 1024 + 1],
        "large.txt",
    )
    .await?;
    ensure!(
        fetch(&app, &format!("{direct}/{large_text}?preview=text"))
            .await?
            .status
            == 413,
        "oversized preview"
    );

    // Names, origins and sizes are validated before storage.
    ensure!(
        upload(&app, &format!("{room}/{}", new_id()), bytes, "../escape")
            .await?
            .0
            == 400,
        "traversal name"
    );
    ensure!(
        upload(&app, &format!("{room}/{}", new_id()), bytes, "bad\nname")
            .await?
            .0
            == 400,
        "newline name"
    );
    ensure!(
        upload_from(
            &app,
            &format!("{room}/{}", new_id()),
            bytes,
            "safe",
            "https://outside.test"
        )
        .await?
        .0 == 403,
        "foreign origin"
    );
    ensure!(
        upload(
            &app,
            &format!("{room}/{}", new_id()),
            &vec![0; 10 * 1024 * 1024 + 1],
            "report.bin"
        )
        .await?
        .0 == 413,
        "oversized upload"
    );
    let oversized_id = new_id();
    let origin = app.origin().to_owned();
    let chunked = chunked_post(
        &origin,
        &format!("{room}/{oversized_id}"),
        &[
            ("Origin", origin.as_str()),
            ("Content-Type", "application/octet-stream"),
            ("X-File-Name", "large.bin"),
        ],
        (0..11).map(|_| vec![0; 1024 * 1024]),
    )
    .await?;
    ensure!(chunked == 413, "chunked oversized upload: {chunked}");
    ensure!(
        fetch(&app, &format!("{room}/{oversized_id}")).await?.status == 404,
        "oversized chunked upload stored"
    );
    ensure!(
        fetch(&app, &format!("{room}?before=-1")).await?.status == 400,
        "negative before"
    );
    ensure!(
        fetch_json(&app, &format!("{room}?before={}", receipt["seq"])).await? == json!([]),
        "before page"
    );

    // Archiving the room during an upload refuses it.
    let delayed_id = new_id();
    let delayed =
        delayed_upload(&app, &format!("{room}/{delayed_id}"), "delayed.bin", bytes).await?;
    let mut archive = channel.clone();
    archive["id"] = json!(new_id());
    archive["revision"] = json!(1);
    archive["archived"] = json!(true);
    app.post(&format!("/api/channels/{channel_id}/edit"), archive)
        .await?;
    let delayed_status = delayed.finish().await?;
    ensure!(
        delayed_status == 409,
        "upload into archived room: {delayed_status}"
    );
    ensure!(
        fetch(&app, &format!("{room}/{delayed_id}")).await?.status == 404,
        "refused upload stored"
    );
    ensure!(
        upload(&app, &format!("{room}/{}", new_id()), bytes, "report.bin")
            .await?
            .0
            == 409,
        "upload into archived room"
    );
    ensure!(
        upload(&app, &path, bytes, "report é.bin").await?.1 == receipt,
        "archived replay receipt"
    );

    // Bytes and lists survive a restart; deleted side threads refuse uploads.
    app.restart().await?;
    ensure!(
        fetch(&app, &path).await?.bytes == bytes,
        "bytes after restart"
    );
    ensure!(
        fetch_json(&app, &room).await?.as_array().map(Vec::len) == Some(1),
        "room list after restart"
    );
    let side_file = format!("{side}/{}", new_id());
    ensure!(
        upload(&app, &side_file, bytes, "report.bin").await?.0 == 200,
        "side upload"
    );
    let removed =
        delayed_upload(&app, &format!("{side}/{}", new_id()), "deleted.bin", bytes).await?;
    app.post(
        &format!("/api/threads/{thread_id}/edit"),
        json!({"id": new_id(), "revision": 1, "name": thread["name"], "archived": true}),
    )
    .await?;
    app.post(
        &format!("/api/threads/{thread_id}/delete"),
        json!({"id": new_id(), "revision": 2}),
    )
    .await?;
    let removed_status = removed.finish().await?;
    ensure!(
        removed_status == 404,
        "upload into deleted thread: {removed_status}"
    );
    ensure!(
        fetch(&app, &side_file).await?.status == 404,
        "deleted thread file readable"
    );
    ensure!(
        upload(&app, &side_file, bytes, "report.bin").await?.0 == 404,
        "upload into deleted thread"
    );
    ensure!(
        upload(&app, &format!("{direct}/{}", new_id()), &[], "empty.txt")
            .await?
            .0
            == 200,
        "empty upload"
    );
    Ok(())
}

fn contents(request: &ModelRequest) -> Vec<String> {
    request.texts()
}

#[tokio::test]
async fn text_attachments_reach_only_selected_conversation_and_retain_retry_identity() -> Result<()>
{
    let model = ScriptedModel::start().await?;
    let worker = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let routed_to = worker.clone();
    model.respond(move |request| {
        if request
            .system()
            .starts_with("Choose exactly one channel member")
        {
            let recipient = routed_to.lock().map(|id| id.clone()).unwrap_or_default();
            Reply::text(json!({"action": "assign", "recipient": recipient}).to_string())
        } else {
            Reply::text("Read attachment.")
        }
    });
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new().model(&model)).await?;
    let agent = app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Reader", "model": "fixture",
                "instructions": "Help."}),
            201,
        )
        .await?;
    let agent_id = id_of(&agent)?;
    let channel = app
        .post_expect(
            "/api/channels",
            json!({"id": new_id(), "name": "Room", "instructions": "Help.", "lead": agent_id,
                "members": [agent_id]}),
            201,
        )
        .await?;
    let channel_id = id_of(&channel)?;
    let path = format!("/api/channels/{channel_id}/turns");
    let main = format!("/api/agents/{agent_id}/turns");
    let mut ids = Vec::new();
    for (name, bytes) in [
        ("note.txt", b"PRIVATE_ROOM_NOTE".to_vec()),
        ("binary.bin", vec![0, 255]),
        ("large.txt", vec![b'A'; 32769]),
    ] {
        let id = new_id();
        let (status, body) = upload(
            &app,
            &format!("/api/channels/{channel_id}/files/{id}"),
            &bytes,
            name,
        )
        .await?;
        ensure!(status == 200, "upload {name}: {status} {body}");
        ids.push(id);
    }
    app.post_expect(
        &main,
        json!({"id": new_id(), "text": "Read", "files": [ids[0]]}),
        404,
    )
    .await?;
    app.post_expect(
        &path,
        json!({"id": new_id(), "text": "Read", "files": [ids[1]]}),
        415,
    )
    .await?;
    app.post_expect(
        &path,
        json!({"id": new_id(), "text": "Read", "files": [ids[2]]}),
        413,
    )
    .await?;
    app.post_expect(
        &path,
        json!({"id": new_id(), "text": "Read", "files": [ids[0], ids[0]]}),
        400,
    )
    .await?;
    app.post_expect(
        &path,
        json!({"id": new_id(), "text": "Read", "files": null}),
        400,
    )
    .await?;
    ensure!(model.calls() == 0, "rejected sends reached the model");

    let send = json!({"id": new_id(), "text": "Read", "files": [ids[0]]});
    let send_id = id_of(&send)?;
    app.post_expect(&path, send.clone(), 202).await?;
    let done = app.wait_status(&path, &send_id, "done").await?;
    ensure!(
        done["files"][0]["name"] == "note.txt",
        "files: {}",
        done["files"]
    );
    let calls = model.requests();
    ensure!(
        calls[0].last_text().contains("PRIVATE_ROOM_NOTE"),
        "attachment missing: {:?}",
        contents(&calls[0])
    );
    app.post_expect(&path, send.clone(), 202).await?;
    ensure!(model.calls() == 1, "replay reached the model");
    let mut changed = send.clone();
    changed["files"] = json!([]);
    app.post_expect(&path, changed, 409).await?;

    // The attachment stays in the room's history, not in direct chat.
    app.restart().await?;
    let follow = new_id();
    app.post_expect(&path, json!({"id": follow, "text": "Follow up"}), 202)
        .await?;
    app.wait_status(&path, &follow, "done").await?;
    let direct = new_id();
    app.post_expect(&main, json!({"id": direct, "text": "Direct chat"}), 202)
        .await?;
    app.wait_status(&main, &direct, "done").await?;
    let calls = model.requests();
    ensure!(
        contents(&calls[1])
            .iter()
            .any(|text| text.contains("PRIVATE_ROOM_NOTE")),
        "room history lost the attachment: {:?}",
        contents(&calls[1])
    );
    ensure!(
        !contents(&calls[2])
            .iter()
            .any(|text| text.contains("PRIVATE_ROOM_NOTE")),
        "direct chat saw the room attachment"
    );

    // Automatic routing sees the attachment and so does the chosen member.
    let worker_agent = app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Worker", "model": "fixture",
                "instructions": "Help."}),
            201,
        )
        .await?;
    let worker_id = id_of(&worker_agent)?;
    if let Ok(mut slot) = worker.lock() {
        slot.clone_from(&worker_id);
    }
    app.post(
        &format!("/api/channels/{channel_id}/edit"),
        json!({"id": new_id(), "revision": channel["revision"], "name": channel["name"],
            "instructions": "Help.", "lead": agent_id, "members": [agent_id, worker_id],
            "archived": false}),
    )
    .await?;
    let routed = new_id();
    app.post_expect(
        &path,
        json!({"id": routed, "text": "Route this file", "automatic": true, "files": [ids[0]]}),
        202,
    )
    .await?;
    let routed_done = app.wait_status(&path, &routed, "done").await?;
    ensure!(
        routed_done["agent"] == worker_id.as_str(),
        "routed: {routed_done}"
    );
    ensure!(
        routed_done["routing"] == "assigned",
        "routed: {routed_done}"
    );
    ensure!(model.calls() == 5, "calls: {}", model.calls());
    let calls = model.requests();
    let routing: Value = serde_json::from_str(&calls[3].last_text())?;
    ensure!(
        routing["request"]
            .as_str()
            .is_some_and(|text| text.contains("PRIVATE_ROOM_NOTE")),
        "routing request: {routing}"
    );
    ensure!(
        calls[4].last_text().contains("PRIVATE_ROOM_NOTE"),
        "worker prompt: {}",
        calls[4].last_text()
    );

    // Side-thread files stay in their thread.
    let side_a = app
        .post(
            &format!("/api/agents/{agent_id}/threads"),
            json!({"id": new_id(), "name": "Side A"}),
        )
        .await?;
    let side_b = app
        .post(
            &format!("/api/agents/{agent_id}/threads"),
            json!({"id": new_id(), "name": "Side B"}),
        )
        .await?;
    let (side_a, side_b) = (id_of(&side_a)?, id_of(&side_b)?);
    let file_id = new_id();
    let (status, _) = upload(
        &app,
        &format!("/api/threads/{side_a}/files/{file_id}"),
        b"PRIVATE_SIDE_NOTE",
        "side.txt",
    )
    .await?;
    ensure!(status == 200, "side upload: {status}");
    let path_a = format!("/api/threads/{side_a}/turns");
    let path_b = format!("/api/threads/{side_b}/turns");
    let side_send = json!({"id": new_id(), "text": "Read side file", "files": [file_id]});
    app.post_expect(&path_b, side_send.clone(), 404).await?;
    app.post_expect(&path_a, side_send.clone(), 202).await?;
    app.wait_status(&path_a, &id_of(&side_send)?, "done")
        .await?;
    ensure!(
        model.requests()[5]
            .last_text()
            .contains("PRIVATE_SIDE_NOTE"),
        "side prompt: {}",
        model.requests()[5].last_text()
    );
    let other = new_id();
    app.post_expect(&path_b, json!({"id": other, "text": "Other side"}), 202)
        .await?;
    app.wait_status(&path_b, &other, "done").await?;
    ensure!(
        !contents(&model.requests()[6])
            .iter()
            .any(|text| text.contains("PRIVATE_SIDE_NOTE") || text.contains("PRIVATE_ROOM_NOTE")),
        "other side saw private files"
    );
    Ok(())
}
