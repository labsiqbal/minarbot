//! Telegram documents: one upload in the requester's thread, retried lost
//! uploads and turns with the original bytes, script file scope, revoked
//! uploads, archived threads and PDF faults (port of
//! `app/telegram-media.test.ts`).

use crate::rg_media::{
    arm_faults, capture, expected_failure, journal_row, pdf_switch, settle, Recorder,
};
use minarbot_testkit::{
    base64::{engine::general_purpose::STANDARD, Engine},
    http::{Method, SplitRequest},
    model::{ModelRequest, Reply, ScriptedModel},
    pdf::pdf_fixture,
    platform::{telegram_update, ConnectorFixture, Hook, HookReply, Kind},
    prelude::*,
    stub::StubReply,
    Server,
};
use std::{
    collections::HashMap,
    sync::{atomic::Ordering, Arc, Mutex},
};

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jkAAAAABJRU5ErkJggg==";
const TOKEN: &str = "local-script-test-token-with-32-characters";

type Files = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// The fake Bot API file endpoints: `getFile` and `/file/...` downloads.
fn telegram_files(payloads: Files, downloads: Arc<Mutex<Vec<String>>>) -> Hook {
    Arc::new(move |method, request| {
        if request.path().starts_with("/file/") {
            if let Ok(mut downloads) = downloads.lock() {
                downloads.push(method.to_owned());
            }
            let body = payloads
                .lock()
                .ok()
                .and_then(|files| files.get(method).cloned())
                .unwrap_or_default();
            return Some(HookReply::Raw(StubReply::Raw(200, Vec::new(), body)));
        }
        if method == "getFile" {
            let file = request.json()["file_id"].clone();
            return Some(HookReply::Value(json!({"file_id": file,
                "file_path": format!("documents/{}", file.as_str().unwrap_or_default())})));
        }
        None
    })
}

struct Media {
    app: Server,
    connector: ConnectorFixture,
    recorder: Recorder,
    model: ScriptedModel,
    sequence: i64,
}

impl Media {
    /// `incoming(file, user, caption, size)`: a document message; returns
    /// its inbox row.
    async fn incoming(
        &mut self,
        file: &str,
        user: i64,
        caption: &str,
        size: Option<u64>,
    ) -> Result<Value> {
        self.sequence += 1;
        let id = self.sequence;
        let mut update = telegram_update(id, "", user);
        update["message"]["caption"] = json!(caption);
        let mut document = json!({"file_id": file, "file_name": format!("{file}.dat")});
        if let Some(size) = size {
            document["file_size"] = json!(size);
        }
        update["message"]["document"] = document;
        self.connector.send(update).await?;
        journal_row(
            &self.connector,
            "SELECT * FROM telegram_inbox WHERE update_id=?",
            [id],
        )?
        .ok_or_else(|| "no inbox row".into())
    }

    async fn read(&mut self, file: &str) -> Result<Value> {
        self.incoming(file, 4, "Read attached", None).await
    }

    async fn lost(&mut self) -> Result<()> {
        expected_failure(&mut self.connector, "Local agent request failed").await
    }

    fn captures(&self) -> Vec<ModelRequest> {
        self.model.requests()
    }

    fn last_capture(&self) -> String {
        let captures = self.captures();
        capture(&captures, captures.len().saturating_sub(1))
    }

    async fn scripts(&self, agent: &str, revision: i64, enabled: bool) -> Result<()> {
        self.app
            .post(
                &format!("/api/agents/{agent}/scripts"),
                json!({"id": new_id(), "revision": revision, "enabled": enabled}),
            )
            .await?;
        Ok(())
    }

    async fn thread_files(&self, thread: &Value) -> Result<Vec<Value>> {
        Ok(self
            .app
            .get(&format!(
                "/api/threads/{}/files",
                thread.as_str().ok_or("thread")?
            ))
            .await?
            .as_array()
            .cloned()
            .unwrap_or_default())
    }
}

fn count(list: &Arc<Mutex<Vec<String>>>, id: &str) -> usize {
    list.lock()
        .map(|list| list.iter().filter(|item| *item == id).count())
        .unwrap_or(0)
}

fn total(list: &Arc<Mutex<Vec<String>>>) -> usize {
    list.lock().map(|list| list.len()).unwrap_or(0)
}

fn set(payloads: &Files, id: &str, value: Vec<u8>) {
    if let Ok(mut files) = payloads.lock() {
        files.insert(id.to_owned(), value);
    }
}

/// `GET` with the script bearer token: status and body.
async fn script_get(app: &Server, path: &str) -> Result<(u16, Vec<u8>)> {
    let response = app
        .client()?
        .get(app.url(path))
        .bearer_auth(TOKEN)
        .send()
        .await?;
    Ok((response.status().as_u16(), response.bytes().await?.to_vec()))
}

#[tokio::test]
async fn telegram_media_uploads_once_in_requester_thread_and_retries_lost_uploads_messages_with_original_bytes(
) -> Result<()> {
    let png = STANDARD.decode(PNG)?;
    let payloads: Files = Arc::new(Mutex::new(HashMap::from([
        ("text".to_owned(), b"Exact attachment contents".to_vec()),
        ("image".to_owned(), png.clone()),
        ("binary".to_owned(), vec![0, 255]),
        ("oversize".to_owned(), vec![0; 4 * 1024 * 1024 + 1]),
    ])));
    let downloads: Arc<Mutex<Vec<String>>> = Arc::default();
    let model = ScriptedModel::start().await?;
    model.respond(|_| Reply::text("Received"));
    let (pdf, pdf_unavailable) = pdf_switch().await?;
    let root = TestRoot::new()?;
    let (agent, other) = (new_id(), new_id());
    let app = root
        .start(
            ServerConfig::new()
                .model(&model)
                .option("scriptToken", TOKEN)
                .script_directory(root.path().join("scripts"))
                .pdf_bridge(&pdf),
        )
        .await?;
    let recorder = Recorder::start(app.origin()).await?;
    let connector = ConnectorFixture::start(
        Kind::Telegram,
        root.path(),
        &agent,
        recorder.origin(),
        TOKEN,
        Some(telegram_files(payloads.clone(), downloads.clone())),
        vec![json!(4), json!(5)],
        Vec::new(),
    )
    .await?;
    let mut m = Media {
        app,
        connector,
        recorder,
        model,
        sequence: 0,
    };
    for id in [&agent, &other] {
        m.app
            .post(
                "/api/agents",
                json!({"runtime": "api", "id": id,
                    "name": if *id == agent { "Media" } else { "Other" },
                    "model": "fixture", "instructions": "Help"}),
            )
            .await?;
    }
    for id in [&agent, &other] {
        m.scripts(id, 0, true).await?;
    }
    m.connector.start_connector().await?;
    arm_faults(&m.connector);

    let text = m.read("text").await?;
    m.lost().await?;
    set(&payloads, "text", b"CHANGED REMOTE".to_vec());
    m.connector.start_connector().await?;
    m.lost().await?;
    m.connector.start_connector().await?;
    settle(&mut m.connector, &text["turn"]).await?;
    ensure!(count(&downloads, "text") == 1, "text downloads");
    let uploads = m.recorder.uploads();
    ensure!(
        uploads.len() >= 2 && uploads[0] == uploads[1],
        "upload retry changed: {uploads:?}"
    );
    ensure!(m.captures().len() == 1, "captures: {}", m.captures().len());
    ensure!(
        capture(&m.captures(), 0).contains("Exact attachment contents"),
        "snapshot bytes"
    );
    let files = m.thread_files(&text["thread"]).await?;
    ensure!(files.len() == 1, "files: {files:?}");
    ensure!(
        m.app.get(&format!("/api/agents/{agent}/turns")).await? == json!([]),
        "direct turns"
    );
    let thread = text["thread"].as_str().ok_or("thread")?.to_owned();
    let (status, _) = script_get(
        &m.app,
        &format!("/api/scripts/agents/{other}/threads/{thread}/files"),
    )
    .await?;
    ensure!(status == 404, "other agent's files: {status}");
    let (status, body) = script_get(
        &m.app,
        &format!(
            "/api/scripts/agents/{agent}/threads/{thread}/files/{}",
            files[0]["id"].as_str().ok_or("file id")?
        ),
    )
    .await?;
    ensure!(
        status == 200 && body == b"Exact attachment contents",
        "script file read: {status} {}",
        String::from_utf8_lossy(&body)
    );

    let photo = m.incoming("image", 5, "/stop not-a-command", None).await?;
    settle(&mut m.connector, &photo["turn"]).await?;
    ensure!(photo["thread"] != text["thread"], "requester thread");
    ensure!(m.captures().len() == 2, "captures: {}", m.captures().len());
    ensure!(
        capture(&m.captures(), 1).contains("data:image/png;base64,"),
        "image not forwarded"
    );
    ensure!(
        m.thread_files(&photo["thread"]).await?.len() == 1,
        "photo files"
    );

    let unsupported = m.read("binary").await?;
    settle(&mut m.connector, &unsupported["turn"]).await?;
    let too_large = m
        .incoming("image", 4, "Too big", Some(10 * 1024 * 1024 + 1))
        .await?;
    settle(&mut m.connector, &too_large["turn"]).await?;
    let streamed_large = m.read("oversize").await?;
    settle(&mut m.connector, &streamed_large["turn"]).await?;
    ensure!(m.captures().len() == 2, "captures: {}", m.captures().len());
    ensure!(count(&downloads, "image") == 1, "image downloads");
    ensure!(
        m.connector
            .json_posts("sendMessage")
            .iter()
            .any(|post| post["text"]
                .as_str()
                .is_some_and(|text| text.contains("Attachment cannot be processed"))),
        "refusal reply"
    );

    let file = new_id();
    let upload_path = format!("/api/scripts/agents/{agent}/threads/{thread}/files/{file}");
    let bearer = format!("Bearer {TOKEN}");
    let streamed = SplitRequest::start(
        m.app.origin(),
        "POST",
        &upload_path,
        &[
            ("Authorization", bearer.as_str()),
            ("Content-Type", "application/octet-stream"),
            ("X-File-Name", "late.txt"),
        ],
        b"firstlast",
        4,
    )
    .await?;
    m.scripts(&agent, 1, false).await?;
    ensure!(
        streamed.finish().await? == 403,
        "upload finished after revocation"
    );
    ensure!(
        !m.thread_files(&text["thread"])
            .await?
            .iter()
            .any(|row| row["id"] == file.as_str()),
        "revoked upload stored"
    );
    let revoked = m
        .incoming("text", 4, "/stop ordinary caption", None)
        .await?;
    expected_failure(&mut m.connector, "403").await?;
    ensure!(count(&downloads, "text") == 1, "downloaded while revoked");
    m.scripts(&agent, 2, true).await?;
    m.app
        .post(
            &format!("/api/threads/{thread}/edit"),
            json!({"id": new_id(), "revision": 1, "name": "Archived", "archived": true}),
        )
        .await?;
    m.connector.start_connector().await?;
    settle(&mut m.connector, &revoked["turn"]).await?;
    ensure!(m.captures().len() == 2, "captures: {}", m.captures().len());
    ensure!(
        journal_row(
            &m.connector,
            "SELECT 1 FROM telegram_closed_threads WHERE id=?",
            [&thread],
        )?
        .is_some(),
        "Media caption failure must close archived thread mapping"
    );
    let fresh = m.read("text").await?;
    ensure!(fresh["thread"] != text["thread"], "fresh thread");
    settle(&mut m.connector, &fresh["turn"]).await?;
    ensure!(m.captures().len() == 3, "captures: {}", m.captures().len());

    let mut original_pdf = pdf_fixture("TELEGRAM_PDF_TEXT", 1, true);
    original_pdf.extend(vec![32; 5 * 1024 * 1024]);
    set(&payloads, "pdf", original_pdf.clone());
    arm_faults(&m.connector);
    let pdf_row = m.read("pdf").await?;
    m.lost().await?;
    set(&payloads, "pdf", pdf_fixture("CHANGED_PDF", 1, true));
    m.connector.start_connector().await?;
    m.lost().await?;
    m.connector.start_connector().await?;
    settle(&mut m.connector, &pdf_row["turn"]).await?;
    ensure!(m.captures().len() == 4, "captures: {}", m.captures().len());
    ensure!(
        m.last_capture().contains("TELEGRAM_PDF_TEXT"),
        "PDF snapshot"
    );
    ensure!(count(&downloads, "pdf") == 1, "pdf downloads");
    let upload = journal_row(
        &m.connector,
        "SELECT upload FROM telegram_media WHERE turn=?",
        [pdf_row["turn"].as_str().ok_or("turn")?],
    )?
    .ok_or("telegram_media row")?;
    let stored = m
        .app
        .request(
            Method::GET,
            &format!(
                "/api/threads/{}/files/{}",
                pdf_row["thread"].as_str().ok_or("thread")?,
                upload["upload"].as_str().ok_or("upload")?
            ),
        )?
        .send()
        .await?
        .bytes()
        .await?;
    ensure!(
        stored.as_ref() == original_pdf.as_slice(),
        "stored PDF differs from the first download"
    );

    set(&payloads, "badPdf", b"%PDF-malformed".to_vec());
    set(
        &payloads,
        "largePdf",
        pdf_fixture(&"X".repeat(500), 70, true),
    );
    set(&payloads, "huge", vec![0; 10 * 1024 * 1024 + 1]);
    let mut large_image = png.clone();
    large_image.extend(vec![0; 4 * 1024 * 1024]);
    set(&payloads, "largeImage", large_image);
    for file in ["badPdf", "largePdf", "huge", "largeImage"] {
        let row = m.read(file).await?;
        settle(&mut m.connector, &row["turn"]).await?;
    }
    ensure!(m.captures().len() == 4, "refused files reached the model");
    let after_failure = m.read("pdf").await?;
    settle(&mut m.connector, &after_failure["turn"]).await?;
    ensure!(m.captures().len() == 5, "captures: {}", m.captures().len());
    ensure!(m.last_capture().contains("CHANGED_PDF"), "new PDF bytes");

    let (retry_count, download_count) = (m.captures().len(), total(&downloads));
    pdf_unavailable.store(true, Ordering::SeqCst);
    let retry = m.read("pdf").await;
    let failed = match &retry {
        Ok(_) => expected_failure(&mut m.connector, "503").await,
        Err(_) => Ok(()),
    };
    pdf_unavailable.store(false, Ordering::SeqCst);
    let retry = retry?;
    failed?;
    let status = journal_row(
        &m.connector,
        "SELECT status FROM telegram_inbox WHERE turn=?",
        [retry["turn"].as_str().ok_or("turn")?],
    )?
    .ok_or("retry inbox row")?;
    ensure!(status["status"] == "submitting", "retry: {status}");
    ensure!(
        m.captures().len() == retry_count,
        "submitted while PDF was down"
    );
    m.connector.start_connector().await?;
    settle(&mut m.connector, &retry["turn"]).await?;
    ensure!(m.captures().len() == retry_count + 1, "retry turn");
    ensure!(total(&downloads) == download_count + 1, "retry downloads");
    m.connector.stop().await?;
    Ok(())
}
