//! Slack file shares: exact byte snapshots, retried uploads and turns
//! without duplicate work, refusals, captions with commands, PDF faults
//! (port of `app/slack-media.test.ts`).

use crate::rg_media::{
    arm_faults, capture, expected_failure, journal, journal_row, pdf_switch, settle, Recorder,
};
use minarbot_testkit::{
    base64::{engine::general_purpose::STANDARD, Engine},
    db::Fixture,
    model::{Reply, ScriptedModel},
    pdf::pdf_fixture,
    platform::{slack_envelope, ConnectorFixture, Hook, HookReply, Kind, BOT_TOKEN},
    prelude::*,
    stub::StubReply,
    Server,
};
use std::{
    collections::HashMap,
    path::Path,
    sync::{atomic::Ordering, Arc, Mutex},
};

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jkAAAAABJRU5ErkJggg==";
const SCRIPT_TOKEN: &str = "fixture-media-script-token-01234567890";
const REFUSED: &str = "Attachment cannot be processed. Use at most four files: UTF-8 text or combined PDF text up to 32 KiB (PDF up to 10 MiB), or PNG/JPEG/WebP images up to 4 MiB each.";
const INVALID: &str = "Invalid attachment. Send at most four different files.";

type Files = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// The fake Slack file API: `files.info` metadata and private downloads,
/// each checked for the bot token.
fn slack_files(
    bytes: Files,
    downloads: Arc<Mutex<Vec<String>>>,
    problems: Arc<Mutex<Vec<String>>>,
) -> Hook {
    Arc::new(move |method, request| {
        if request.header("authorization") != Some(format!("Bearer {BOT_TOKEN}").as_str()) {
            if let Ok(mut problems) = problems.lock() {
                problems.push(format!("{method} without the bot token"));
            }
        }
        let files = bytes.lock().ok()?;
        if request.path().starts_with("/download/") {
            if let Ok(mut downloads) = downloads.lock() {
                downloads.push(method.to_owned());
            }
            let body = files.get(method).cloned().unwrap_or_default();
            return Some(HookReply::Raw(StubReply::Raw(200, Vec::new(), body)));
        }
        if method == "files.info" {
            let file = request.json()["file"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let size = if file == "FLARGE" {
                1
            } else {
                files.get(&file).map_or(0, Vec::len)
            };
            let host = request.header("host").unwrap_or_default();
            return Some(HookReply::Value(json!({"file": {"id": file,
                "name": format!("{file}.dat"), "size": size,
                "mode": if file == "FEXTERNAL" { "external" } else { "hosted" },
                "is_external": file == "FEXTERNAL",
                "url_private": format!("http://{host}/download/{file}")}})));
        }
        None
    })
}

/// The Node `localScript`: bearer requests to the app's discovery file.
async fn script_post(directory: &Path, path: &str, body: Value) -> Result<Value> {
    let connection: Value =
        serde_json::from_str(&std::fs::read_to_string(directory.join("connection.json"))?)?;
    let response = minarbot_testkit::http::Client::builder()
        .no_proxy()
        .build()?
        .post(format!(
            "{}{path}",
            connection["url"].as_str().ok_or("discovery url")?
        ))
        .bearer_auth(connection["token"].as_str().ok_or("discovery token")?)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;
    let status = response.status().as_u16();
    let value: Value = response.json().await?;
    ensure!(
        (200..300).contains(&status),
        "POST {path}: {status} {value}"
    );
    Ok(value)
}

struct Media {
    app: Server,
    connector: ConnectorFixture,
    recorder: Recorder,
    model: ScriptedModel,
    sequence: u64,
}

impl Media {
    /// `incoming(files, text, sender)`: a `file_share` message; returns its
    /// inbox row (`None` when the connector ignored the sender).
    async fn incoming(
        &mut self,
        files: &[&str],
        text: &str,
        sender: &str,
    ) -> Result<Option<Value>> {
        self.sequence += 1;
        let id = self.sequence;
        let mut envelope = slack_envelope(id, text, sender, None, "DCHAT");
        let event = &mut envelope["payload"]["event"];
        event["subtype"] = json!("file_share");
        event["bot_id"] = Value::Null;
        event["files"] = json!(files.iter().map(|id| json!({"id": id})).collect::<Vec<_>>());
        self.connector.send(envelope).await?;
        journal_row(
            &self.connector,
            "SELECT * FROM slack_inbox WHERE event=?",
            [format!("Ev{id}")],
        )
    }

    async fn row(&mut self, files: &[&str], text: &str) -> Result<Value> {
        self.incoming(files, text, "UUSER")
            .await?
            .ok_or_else(|| "no inbox row".into())
    }

    async fn lost(&mut self) -> Result<()> {
        expected_failure(&mut self.connector, "Local agent request failed").await
    }

    fn captures(&self) -> Vec<minarbot_testkit::model::ModelRequest> {
        self.model.requests()
    }

    fn last_capture(&self) -> String {
        let captures = self.captures();
        capture(&captures, captures.len().saturating_sub(1))
    }

    fn last_sent(&self) -> String {
        self.connector
            .json_posts("chat.postMessage")
            .last()
            .and_then(|post| post["text"].as_str().map(str::to_owned))
            .unwrap_or_default()
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

fn set(bytes: &Files, id: &str, value: Vec<u8>) {
    if let Ok(mut files) = bytes.lock() {
        files.insert(id.to_owned(), value);
    }
}

#[tokio::test]
async fn slack_files_snapshot_bytes_and_retry_uploads_and_turns_without_duplicate_work(
) -> Result<()> {
    let png = STANDARD.decode(PNG)?;
    let bytes: Files = Arc::new(Mutex::new(HashMap::from([
        ("FTEXT".to_owned(), b"Exact Slack file content".to_vec()),
        ("FIMAGE".to_owned(), png.clone()),
        ("FBINARY".to_owned(), vec![0, 255]),
        ("FLARGE".to_owned(), vec![0; 4 * 1024 * 1024 + 1]),
        ("FEXTERNAL".to_owned(), b"secret".to_vec()),
    ])));
    let downloads: Arc<Mutex<Vec<String>>> = Arc::default();
    let problems: Arc<Mutex<Vec<String>>> = Arc::default();
    let model = ScriptedModel::start().await?;
    model.respond(|_| Reply::text("Read"));
    let (pdf, pdf_unavailable) = pdf_switch().await?;
    let root = TestRoot::new()?;
    let scripts = root.path().join("scripts");
    let agent = new_id();
    let app = root
        .start(
            ServerConfig::new()
                .model(&model)
                .option("scriptToken", SCRIPT_TOKEN)
                .script_directory(&scripts)
                .pdf_bridge(&pdf),
        )
        .await?;
    let recorder = Recorder::start(app.origin()).await?;
    let connector = ConnectorFixture::start(
        Kind::Slack,
        root.path(),
        &agent,
        recorder.origin(),
        SCRIPT_TOKEN,
        Some(slack_files(
            bytes.clone(),
            downloads.clone(),
            problems.clone(),
        )),
        vec![json!("UUSER"), json!("UOTHER")],
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
    let scripts_path = format!("/api/agents/{agent}/scripts");
    m.app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": agent, "name": "Files", "model": "fixture",
                "instructions": "Help"}),
        )
        .await?;
    m.app
        .post(
            &scripts_path,
            json!({"id": new_id(), "revision": 0, "enabled": true}),
        )
        .await?;
    m.connector.start_connector().await?;
    arm_faults(&m.connector);

    ensure!(
        m.incoming(&["FTEXT"], "ignored", "UNOT").await?.is_none(),
        "an unlisted sender was accepted"
    );
    let row = m.row(&["FTEXT"], "Read files").await?;
    m.lost().await?;
    set(&bytes, "FTEXT", b"Changed remote".to_vec());
    m.connector.start_connector().await?;
    m.lost().await?;
    m.connector.start_connector().await?;
    settle(&mut m.connector, &row["turn"]).await?;
    ensure!(count(&downloads, "FTEXT") == 1, "FTEXT downloads");
    let uploads = m.recorder.uploads();
    ensure!(
        uploads.len() >= 2 && uploads[0] == uploads[1],
        "upload retry changed: {uploads:?}"
    );
    ensure!(m.captures().len() == 1, "captures: {}", m.captures().len());
    ensure!(
        capture(&m.captures(), 0).contains("Exact Slack file content"),
        "snapshot bytes"
    );
    let thread = row["thread"].as_str().ok_or("thread")?.to_owned();
    let files = m.app.get(&format!("/api/threads/{thread}/files")).await?;
    ensure!(files.as_array().map(Vec::len) == Some(1), "files: {files}");
    ensure!(
        m.app.get(&format!("/api/agents/{agent}/turns")).await? == json!([]),
        "direct turns"
    );

    let image = m.row(&["FIMAGE"], "").await?;
    settle(&mut m.connector, &image["turn"]).await?;
    ensure!(m.last_capture().contains(PNG), "image bytes not forwarded");

    let many: Vec<String> = (0..5).map(|index| format!("FFILE{index}")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let refused: [&[&str]; 5] = [
        &["FBINARY"],
        &["FLARGE"],
        &["FEXTERNAL"],
        &["FTEXT", "FTEXT"],
        &many,
    ];
    for ids in refused {
        let before = m.captures().len();
        let bad = m.row(ids, "Read files").await?;
        settle(&mut m.connector, &bad["turn"]).await?;
        ensure!(m.captures().len() == before, "{ids:?} reached the model");
        let unique: std::collections::HashSet<&&str> = ids.iter().collect();
        let expected = if ids.len() > 4 || unique.len() != ids.len() {
            INVALID
        } else {
            REFUSED
        };
        ensure!(m.last_sent() == expected, "{ids:?}: {}", m.last_sent());
    }
    ensure!(
        count(&downloads, "FEXTERNAL") == 0,
        "external file downloaded"
    );

    let command = m.row(&["FTEXT"], "!status").await?;
    settle(&mut m.connector, &command["turn"]).await?;
    ensure!(m.captures().len() == 3, "captures: {}", m.captures().len());
    ensure!(m.last_capture().contains("!status"), "caption command");

    let workflow = m
        .app
        .post(
            &format!("/api/agents/{agent}/workflows"),
            json!({"id": new_id(), "revision": 0, "name": "Wait", "graph": {"entry": "ask",
                "maxSteps": 2, "nodes": [{"id": "ask", "kind": "approval", "prompt": "Proceed?",
                "yes": "out"}, {"id": "out", "kind": "output"}]}}),
        )
        .await?;
    let workflow = workflow["id"].as_str().ok_or("workflow id")?.to_owned();
    let run = new_id();
    script_post(
        &scripts,
        &format!("/api/scripts/agents/{agent}/workflows/{workflow}/run"),
        json!({"id": run, "revision": 1, "input": "Wait"}),
    )
    .await?;
    let root_ts = slack_envelope(m.sequence + 1, "", "UUSER", None, "DCHAT")["payload"]["event"]
        ["ts"]
        .as_str()
        .ok_or("ts")?
        .to_owned();
    Fixture::open(journal(&m.connector))?.execute(
        "INSERT INTO slack_workflows VALUES (?,?,?,?,?)",
        (&run, "DCHAT", "UUSER", &root_ts, &workflow),
    )?;
    let stop_caption = m.row(&["FTEXT"], &format!("!stop {run}")).await?;
    settle(&mut m.connector, &stop_caption["turn"]).await?;
    let status = m.app.get(&format!("/api/workflow-runs/{run}")).await?;
    ensure!(status["status"] == "approval", "workflow run: {status}");
    ensure!(m.captures().len() == 4, "captures: {}", m.captures().len());
    ensure!(
        m.last_capture().contains(&format!("!stop {run}")),
        "caption stop"
    );

    m.app
        .post(
            &scripts_path,
            json!({"id": new_id(), "revision": 1, "enabled": false}),
        )
        .await?;
    let before = total(&downloads);
    let revoked = m.row(&["FTEXT"], "Read files").await?;
    expected_failure(&mut m.connector, "403").await?;
    ensure!(total(&downloads) == before, "downloaded while revoked");
    ensure!(m.captures().len() == 4, "captures: {}", m.captures().len());
    m.app
        .post(
            &scripts_path,
            json!({"id": new_id(), "revision": 2, "enabled": true}),
        )
        .await?;
    m.connector.start_connector().await?;
    settle(&mut m.connector, &revoked["turn"]).await?;

    let mut original_pdf = pdf_fixture("SLACK_PDF_TEXT", 1, true);
    original_pdf.extend(vec![32; 5 * 1024 * 1024]);
    set(&bytes, "FPDF", original_pdf.clone());
    arm_faults(&m.connector);
    let pdf_row = m.row(&["FPDF"], "Read files").await?;
    m.lost().await?;
    set(&bytes, "FPDF", pdf_fixture("CHANGED_PDF", 1, true));
    m.connector.start_connector().await?;
    m.lost().await?;
    m.connector.start_connector().await?;
    settle(&mut m.connector, &pdf_row["turn"]).await?;
    ensure!(m.last_capture().contains("SLACK_PDF_TEXT"), "PDF snapshot");
    ensure!(count(&downloads, "FPDF") == 1, "FPDF downloads");
    let upload = journal_row(
        &m.connector,
        "SELECT upload FROM slack_media WHERE turn=?",
        [pdf_row["turn"].as_str().ok_or("turn")?],
    )?
    .ok_or("slack_media row")?;
    let response = m
        .app
        .request(
            minarbot_testkit::http::Method::GET,
            &format!(
                "/api/threads/{}/files/{}",
                pdf_row["thread"].as_str().ok_or("thread")?,
                upload["upload"].as_str().ok_or("upload")?
            ),
        )?
        .send()
        .await?;
    ensure!(
        response.bytes().await?.as_ref() == original_pdf.as_slice(),
        "stored PDF differs from the first download"
    );

    let count_before = m.captures().len();
    set(&bytes, "FBADPDF", b"%PDF-malformed".to_vec());
    set(&bytes, "FBIGPDF", pdf_fixture(&"X".repeat(500), 70, true));
    set(&bytes, "FHUGE", vec![0; 10 * 1024 * 1024 + 1]);
    let mut large_image = png.clone();
    large_image.extend(vec![0; 4 * 1024 * 1024]);
    set(&bytes, "FLARGEIMAGE", large_image);
    for file in ["FBADPDF", "FBIGPDF", "FHUGE", "FLARGEIMAGE"] {
        let row = m.row(&[file], "Read files").await?;
        settle(&mut m.connector, &row["turn"]).await?;
    }
    ensure!(
        m.captures().len() == count_before,
        "refused files reached the model"
    );
    set(&bytes, "FHALF1", pdf_fixture(&"X".repeat(500), 34, true));
    set(&bytes, "FHALF2", pdf_fixture(&"Y".repeat(500), 34, true));
    let halves = m.row(&["FHALF1", "FHALF2"], "Read files").await?;
    settle(&mut m.connector, &halves["turn"]).await?;
    ensure!(
        m.captures().len() == count_before,
        "combined PDF text limit"
    );
    let after_failure = m.row(&["FPDF"], "Read files").await?;
    settle(&mut m.connector, &after_failure["turn"]).await?;
    ensure!(m.captures().len() == count_before + 1, "PDF after failures");
    ensure!(m.last_capture().contains("CHANGED_PDF"), "new PDF bytes");

    let (retry_count, download_count) = (m.captures().len(), total(&downloads));
    pdf_unavailable.store(true, Ordering::SeqCst);
    let retry = m.row(&["FPDF"], "Read files").await;
    let failed = match &retry {
        Ok(_) => expected_failure(&mut m.connector, "503").await,
        Err(_) => Ok(()),
    };
    pdf_unavailable.store(false, Ordering::SeqCst);
    let retry = retry?;
    failed?;
    let status = journal_row(
        &m.connector,
        "SELECT status FROM slack_inbox WHERE turn=?",
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
    let problems = problems.lock().map(|p| p.clone()).unwrap_or_default();
    ensure!(problems.is_empty(), "Slack fixture: {problems:?}");
    m.connector.stop().await?;
    Ok(())
}
