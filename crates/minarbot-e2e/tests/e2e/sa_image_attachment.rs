//! Image attachments: scope, exact bytes, retry identity and bounded
//! rereading (port of `app/image-attachment.test.ts`, openai-compatible case;
//! #87 removed the Anthropic and Gemini wire formats).

use crate::sa_support::{lock, Shared};
use minarbot_testkit::{
    base64::{engine::general_purpose::STANDARD, Engine},
    http::Method,
    model::{ModelRequest, Reply, ScriptedModel},
    prelude::*,
    Server,
};

const IMAGE_LIMIT: usize = 4 * 1024 * 1024;
const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aZ84AAAAASUVORK5CYII=";

#[derive(Default)]
struct Script {
    read: bool,
    target: String,
    stage: usize,
}

fn images(request: &ModelRequest) -> Vec<Value> {
    request
        .messages()
        .iter()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter(|part| part["type"] == "image_url" || part["type"] == "image")
        .cloned()
        .collect()
}

fn exact(part: &Value) -> bool {
    part["image_url"]["url"] == format!("data:image/png;base64,{PNG}").as_str()
}

async fn upload(app: &Server, scope: &str, bytes: &[u8], name: &str) -> Result<String> {
    let id = new_id();
    let status = app
        .request(Method::POST, &format!("{scope}/files/{id}"))?
        .header("content-type", "application/octet-stream")
        .header("x-file-name", name)
        .body(bytes.to_vec())
        .send()
        .await?
        .status();
    ensure!(status == 200, "upload {name}: {status}");
    Ok(id)
}

async fn settle(app: &Server, scope: &str, id: &str) -> Result<Value> {
    app.wait_row(&format!("{scope}/turns"), id, |row| {
        row["status"] == "done" || row["status"] == "failed"
    })
    .await
}

fn file(row: &Value, id: &str) -> Value {
    row["files"]
        .as_array()
        .and_then(|files| files.iter().find(|file| file["id"] == id).cloned())
        .unwrap_or(Value::Null)
}

#[tokio::test]
async fn openai_compatible_image_attachments_retain_scope_bytes_retry_identity_and_bounded_rereading(
) -> Result<()> {
    let png = STANDARD.decode(PNG)?;
    let model = ScriptedModel::start().await?;
    let script: Shared<Script> = Shared::default();
    let state = script.clone();
    model.respond(move |request| {
        let Ok(mut state) = state.lock() else {
            return Reply::status(500, json!({}));
        };
        state.stage += 1;
        if request
            .system()
            .starts_with("Choose exactly one channel member")
        {
            let last = request.messages().last().cloned().unwrap_or(Value::Null);
            let text = last["content"][0]["text"]
                .as_str()
                .or_else(|| last["content"].as_str())
                .unwrap_or("{}");
            let input: Value = serde_json::from_str(text).unwrap_or(Value::Null);
            return Reply::text(
                json!({"action": "assign", "recipient": input["members"][0]["id"]}).to_string(),
            );
        }
        if state.read && state.stage == 1 {
            return Reply::tool_call(&new_id(), "read_attachment", json!({"id": state.target}));
        }
        Reply::text("Image received")
    });
    let read = |target: &str| -> Result<()> {
        let mut state = lock(&script)?;
        state.read = true;
        state.target = target.into();
        state.stage = 0;
        Ok(())
    };
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new().model(&model)).await?;
    let agent = |name: &str| json!({"runtime": "api", "id": new_id(), "name": name, "model": "vision", "instructions": "Inspect images"});
    let a = app.post("/api/agents", agent("A")).await?;
    let b = app.post("/api/agents", agent("B")).await?;
    let (a, b) = (
        a["id"].as_str().unwrap_or_default().to_owned(),
        b["id"].as_str().unwrap_or_default().to_owned(),
    );
    let room = app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Room", "instructions": "Route images", "lead": a, "members": [a, b]}),
        )
        .await?;
    let thread = app
        .post(
            &format!("/api/agents/{a}/threads"),
            json!({"id": new_id(), "name": "Side"}),
        )
        .await?;
    let scopes = [
        format!("/api/agents/{a}"),
        format!("/api/channels/{}", room["id"].as_str().unwrap_or_default()),
        format!("/api/threads/{}", thread["id"].as_str().unwrap_or_default()),
    ];
    let mut files = Vec::new();
    for scope in &scopes {
        let channel = scope.contains("channels");
        let image = upload(&app, scope, &png, "renamed.bin").await?;
        let note = upload(&app, scope, b"NOTE_DATA", "note.txt").await?;
        files.push(image.clone());
        ensure!(
            app.get(&format!("{scope}/files/{image}?metadata=1"))
                .await?["image"]
                == true,
            "image metadata"
        );
        ensure!(
            app.get(&format!("{scope}/files/{note}?metadata=1")).await?["image"] == false,
            "note metadata"
        );
        let listed = app.get(&format!("{scope}/files")).await?;
        ensure!(
            listed
                .as_array()
                .and_then(|rows| rows.iter().find(|row| row["id"] == image.as_str()))
                .is_some_and(|row| row["image"] == true),
            "files listing: {listed}"
        );
        let panel = app.get(&format!("{scope}/panel/files")).await?;
        ensure!(
            panel
                .as_array()
                .and_then(|rows| rows.iter().find(|row| row["id"] == image.as_str()))
                .is_some_and(|row| row["image"] == true),
            "panel listing: {panel}"
        );
        let id = new_id();
        let mut command =
            json!({"id": id, "text": "Describe selected image", "files": [image, note]});
        if channel {
            command["automatic"] = json!(true);
        }
        let start = model.calls();
        app.post(&format!("{scope}/turns"), command.clone()).await?;
        let settled = settle(&app, scope, &id).await?;
        ensure!(settled["status"] == "done", "{scope} turn: {settled}");
        ensure!(file(&settled, &image)["image"] == true, "turn image flag");
        ensure!(file(&settled, &note)["image"] == false, "turn note flag");
        for request in model.requests().into_iter().skip(start) {
            if request
                .system()
                .starts_with("Choose exactly one channel member")
            {
                continue;
            }
            let parts = images(&request);
            ensure!(parts.len() == 1, "{scope} images: {}", parts.len());
            ensure!(exact(&parts[0]), "{scope} image bytes");
            ensure!(request.mentions("NOTE_DATA"), "{scope} note missing");
        }
        let count = model.calls();
        app.post(&format!("{scope}/turns"), command.clone()).await?;
        ensure!(model.calls() == count, "turn replay called the model");
        let mut changed = command.clone();
        changed["files"] = json!([image]);
        app.post_expect(&format!("{scope}/turns"), changed, 409)
            .await?;
        let bytes = app
            .request(Method::GET, &format!("{scope}/files/{image}"))?
            .send()
            .await?
            .bytes()
            .await?;
        ensure!(bytes.as_ref() == png.as_slice(), "stored bytes changed");
        read(&image)?;
        let turn = new_id();
        let before = model.calls();
        let mut body = json!({"id": turn, "text": "Inspect previous image"});
        if channel {
            body["recipient"] = json!(a);
        }
        app.post(&format!("{scope}/turns"), body).await?;
        ensure!(
            settle(&app, scope, &turn).await?["status"] == "done",
            "read turn"
        );
        let requests = model.requests();
        ensure!(images(&requests[before]).is_empty(), "image before reading");
        let last = requests.last().ok_or("no request")?;
        ensure!(
            images(last).len() == 1 && exact(&images(last)[0]),
            "reread image"
        );
        lock(&script)?.read = false;
    }
    let main = &scopes[0];
    app.post_expect(
        &format!("{main}/turns"),
        json!({"id": new_id(), "text": "Foreign", "files": [files[1]]}),
        404,
    )
    .await?;
    read(&files[1])?;
    let foreign = new_id();
    app.post(
        &format!("{main}/turns"),
        json!({"id": foreign, "text": "Read foreign image"}),
    )
    .await?;
    ensure!(
        settle(&app, main, &foreign).await?["status"] == "done",
        "foreign read turn"
    );
    let last = model.requests().last().cloned().ok_or("no request")?;
    ensure!(images(&last).is_empty(), "foreign image read");
    ensure!(last.mentions("Attachment not found"), "foreign read error");
    let unsent_target = upload(&app, main, &png, "renamed.bin").await?;
    read(&unsent_target)?;
    let unsent = new_id();
    app.post(
        &format!("{main}/turns"),
        json!({"id": unsent, "text": "Read unsent image"}),
    )
    .await?;
    settle(&app, main, &unsent).await?;
    ensure!(
        images(&model.requests().last().cloned().ok_or("no request")?).is_empty(),
        "unsent image read"
    );
    let large = upload(
        &app,
        main,
        &[png.clone(), vec![0; IMAGE_LIMIT]].concat(),
        "renamed.bin",
    )
    .await?;
    app.post_expect(
        &format!("{main}/turns"),
        json!({"id": new_id(), "text": "Large", "files": [large]}),
        413,
    )
    .await?;
    let binary = upload(&app, main, &[0, 255], "fake.png").await?;
    app.post_expect(
        &format!("{main}/turns"),
        json!({"id": new_id(), "text": "Invalid", "files": [binary]}),
        415,
    )
    .await?;
    let mut five = Vec::new();
    for _ in 0..5 {
        five.push(upload(&app, main, &png, "renamed.bin").await?);
    }
    app.post_expect(
        &format!("{main}/turns"),
        json!({"id": new_id(), "text": "Five", "files": five}),
        400,
    )
    .await?;
    let half = [png.clone(), vec![0; IMAGE_LIMIT / 2]].concat();
    let halves = [
        upload(&app, main, &half, "renamed.bin").await?,
        upload(&app, main, &half, "renamed.bin").await?,
    ];
    app.post_expect(
        &format!("{main}/turns"),
        json!({"id": new_id(), "text": "Total", "files": halves}),
        413,
    )
    .await?;
    lock(&script)?.read = false;
    let older = new_id();
    app.post(
        &format!("{main}/turns"),
        json!({"id": older, "text": "Older image", "files": [halves[0]]}),
    )
    .await?;
    settle(&app, main, &older).await?;
    read(&halves[0])?;
    let over = new_id();
    app.post(
        &format!("{main}/turns"),
        json!({"id": over, "text": "Current plus older image", "files": [halves[1]]}),
    )
    .await?;
    let budget = settle(&app, main, &over).await?;
    ensure!(
        budget["status"] == "done",
        "budget turn: {}",
        budget["error"]
    );
    let last = model.requests().last().cloned().ok_or("no request")?;
    ensure!(
        images(&last).len() == 1,
        "budget images: {}",
        images(&last).len()
    );
    ensure!(
        last.mentions("Image context exceeds 4 MiB"),
        "budget error missing"
    );
    read(&files[0])?;
    let duplicate = new_id();
    app.post(
        &format!("{main}/turns"),
        json!({"id": duplicate, "text": "Already attached", "files": [files[0]]}),
    )
    .await?;
    settle(&app, main, &duplicate).await?;
    let last = model.requests().last().cloned().ok_or("no request")?;
    ensure!(
        images(&last).len() == 1,
        "duplicate images: {}",
        images(&last).len()
    );
    ensure!(
        last.mentions("already included"),
        "duplicate notice missing"
    );
    app.restart().await?;
    read(&files[0])?;
    let after = new_id();
    app.post(
        &format!("{main}/turns"),
        json!({"id": after, "text": "Read after restart"}),
    )
    .await?;
    ensure!(
        settle(&app, main, &after).await?["status"] == "done",
        "restart read turn"
    );
    let last = model.requests().last().cloned().ok_or("no request")?;
    ensure!(
        images(&last).first().is_some_and(exact),
        "image after restart"
    );
    Ok(())
}
