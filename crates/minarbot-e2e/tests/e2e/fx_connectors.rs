//! Slack and Telegram connectors (ports of `app/connector-task-files.test.ts`,
//! `app/workflow-files.test.ts` and the journal and runner cases of
//! `app/rust-connectors.test.ts`).

use crate::fx_support::{after_pause, id, pause_reply_for, rows};
use minarbot_testkit::{
    connector,
    db::{Fixture, Inspector},
    gated::{Answer, GatedModel},
    http::Method,
    platform::{artifact_receiver, slack_envelope, telegram_update, ConnectorFixture, Fault, Kind},
    prelude::*,
    server::Gateway,
    Server,
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

const SCRIPT_TOKEN: &str = "connector-artifact-test-token-0123456789";

/// The Node `localScript` client: bearer requests to the app's discovery file.
struct Script {
    directory: PathBuf,
}

impl Script {
    fn connection(&self) -> Result<(String, String)> {
        let value: Value = serde_json::from_str(&std::fs::read_to_string(self.directory.join("connection.json"))?)?;
        Ok((
            value["url"].as_str().ok_or("discovery url")?.to_owned(),
            value["token"].as_str().ok_or("discovery token")?.to_owned(),
        ))
    }

    /// Status and raw body of `GET path` (or POST with `body`).
    async fn call(&self, path: &str, body: Option<Value>) -> Result<(u16, Vec<u8>)> {
        let (url, token) = self.connection()?;
        let client = minarbot_testkit::http::Client::builder().no_proxy().build()?;
        let mut request = client
            .request(if body.is_some() { Method::POST } else { Method::GET }, format!("{url}{path}"))
            .bearer_auth(token);
        if let Some(body) = body {
            request = request.header("Content-Type", "application/json").body(body.to_string());
        }
        let response = request.send().await?;
        Ok((response.status().as_u16(), response.bytes().await?.to_vec()))
    }

    async fn json(&self, path: &str) -> Result<Value> {
        let (status, bytes) = self.call(path, None).await?;
        ensure!(status == 200, "GET {path}: {status} {}", String::from_utf8_lossy(&bytes));
        Ok(serde_json::from_slice(&bytes)?)
    }

    async fn status(&self, path: &str, body: Option<Value>) -> Result<u16> {
        Ok(self.call(path, body).await?.0)
    }
}

fn inbox(kind: Kind) -> &'static str {
    match kind {
        Kind::Slack => "slack_inbox",
        Kind::Telegram => "telegram_inbox",
    }
}

fn table(kind: Kind, name: &str) -> String {
    format!("{}_{name}", kind.name())
}

fn journal(fixture: &ConnectorFixture) -> PathBuf {
    fixture.directory.join(format!("{}.sqlite", fixture.kind.name()))
}

/// Deliver an incoming chat message and return the newest inbox row.
async fn incoming(fixture: &mut ConnectorFixture, sequence: &mut u64, text: &str) -> Result<Value> {
    *sequence += 1;
    let message = match fixture.kind {
        Kind::Slack => slack_envelope(*sequence, text, "UUSER", None, "DCHAT"),
        Kind::Telegram => telegram_update(i64::try_from(*sequence)?, text, 4),
    };
    fixture.send(message).await?;
    Inspector::open(journal(fixture))?.row(
        &format!("SELECT * FROM {} ORDER BY rowid DESC LIMIT 1", inbox(fixture.kind)),
        [],
    )
}

async fn wait_done(fixture: &mut ConnectorFixture, turn: &str) -> Result<()> {
    let database = journal(fixture);
    let kind = fixture.kind;
    poll("connector intake settles", || {
        fixture.check()?;
        let status: Option<String> = Inspector::open(&database)?
            .rows(&format!("SELECT status FROM {} WHERE turn=?", inbox(kind)), [turn])?
            .first()
            .and_then(|row| row["status"].as_str().map(str::to_owned));
        Ok((status.as_deref() == Some("done")).then_some(()))
    })
    .await
}

async fn deliver(fixture: &mut ConnectorFixture) -> Result<()> {
    let database = journal(fixture);
    let outbox = table(fixture.kind, "outbox");
    poll("Artifacts and text were delivered", || {
        fixture.check()?;
        let rows = Inspector::open(&database)?.rows(&format!("SELECT status FROM {outbox} ORDER BY rowid"), [])?;
        Ok(rows.iter().all(|row| row["status"] == "sent").then_some(()))
    })
    .await
}

/// Poll a synchronous check (it may borrow the fixture mutably) for up to
/// the kit's wait budget.
async fn poll<T>(label: &str, mut check: impl FnMut() -> Result<Option<T>>) -> Result<T> {
    let deadline = tokio::time::Instant::now() + minarbot_testkit::WAIT;
    loop {
        if let Some(value) = check()? {
            return Ok(value);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("timed out waiting for {label}").into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

fn read_fault() -> Fault {
    fn matches(path: &str) -> bool {
        path.rsplit_once("/files/").is_some_and(|(_, file)| !file.is_empty() && !file.contains(['/', '?']))
    }
    Fault {
        method: Some("GET"),
        matches,
        after: true,
    }
}

fn metadata_fault() -> Fault {
    fn matches(path: &str) -> bool {
        path.rsplit_once("/files/")
            .and_then(|(_, file)| file.strip_suffix("?metadata=1"))
            .is_some_and(|file| !file.is_empty() && !file.contains(['/', '?']))
    }
    Fault {
        method: Some("GET"),
        matches,
        after: false,
    }
}

/// Two injected script failures: the read reply is lost, then the metadata
/// request never arrives; each stops the connector, which then restarts.
async fn survive_faults(fixture: &mut ConnectorFixture, uploads: &Arc<Mutex<Vec<Vec<u8>>>>, documents: &str) -> Result<()> {
    for round in 0..2 {
        let stderr = fixture.expected_failure().await?;
        ensure!(stderr.contains("Local agent request failed"), "fault {round}: {stderr}");
        ensure!(uploads.lock().map(|u| u.is_empty()).unwrap_or(false), "upload before recovery");
        if round == 1 {
            let count: i64 = Inspector::open(journal(fixture))?.scalar(&format!("SELECT count(*) FROM {documents}"), [])?;
            ensure!(count == 1, "documents {count}");
        }
        fixture.start_connector().await?;
    }
    Ok(())
}

struct App {
    app: Server,
    root: TestRoot,
    _model: GatedModel,
}

async fn app_with(model: GatedModel) -> Result<App> {
    let root = TestRoot::new()?;
    let app = root
        .start(
            ServerConfig::new()
                .gateway(Gateway::new(model.base_url()))
                .option("scriptToken", SCRIPT_TOKEN)
                .script_directory(root.path().join("scripts")),
        )
        .await?;
    Ok(App { app, root, _model: model })
}

fn users(kind: Kind) -> Vec<Value> {
    match kind {
        Kind::Slack => vec![json!("UUSER"), json!("UOTHER")],
        Kind::Telegram => vec![json!(4), json!(5)],
    }
}

async fn task_files(kind: Kind, mode: &'static str) -> Result<()> {
    let expects_reply = mode != "notice";
    let (owner, worker, leaf, foreign) = (new_id(), new_id(), new_id(), new_id());
    let bytes = "exact teammate bytes";
    let partial = Arc::new(Mutex::new(false));
    let model = GatedModel::start().await?;
    let (recipient, nested_leaf, ask) = (worker.clone(), leaf.clone(), partial.clone());
    model.respond(move |request| {
        if after_pause(request) {
            return pause_reply_for(request);
        }
        let calls = request.tool_calls();
        let model = request.model();
        if calls.is_empty() {
            return if ["owner", "nested"].contains(&model) {
                Answer::tool_id(
                    "send_message",
                    "send_message",
                    json!({"recipients": [if model == "nested" { &nested_leaf } else { &recipient }],
                        "text": "Make artifact", "expects_reply": expects_reply}),
                )
            } else {
                Answer::tool_id("create_text_file", "create_text_file", json!({"name": "teammate.txt", "text": bytes}))
            };
        }
        let asked = calls.iter().any(|call| call["function"]["name"] == "ask_user");
        if ask.lock().map(|p| *p).unwrap_or(false) && model == "owner" && !asked {
            return Answer::tool_id("ask_user", "ask_user", json!({"question": "Continue?", "choices": []}));
        }
        Answer::text("Done")
    });
    let App { app, root, _model } = app_with(model).await?;
    let script = Script { directory: root.path().join("scripts") };
    let uploads: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
    let mut fixture = ConnectorFixture::start(
        kind,
        root.path(),
        &owner,
        app.origin(),
        SCRIPT_TOKEN,
        Some(artifact_receiver(kind, uploads.clone())),
        users(kind),
        Vec::new(),
    )
    .await?;
    for agent in [&owner, &worker, &leaf, &foreign] {
        let model = if *agent == owner {
            "owner"
        } else if mode == "nested" && *agent == worker {
            "nested"
        } else {
            "worker"
        };
        app.post(
            "/api/agents",
            json!({"runtime": "api", "id": agent, "name": agent, "model": model, "instructions": "Help"}),
        )
        .await?;
    }
    for agent in [&owner, &foreign] {
        app.post(&format!("/api/agents/{agent}/scripts"), json!({"id": new_id(), "revision": 0, "enabled": true}))
            .await?;
    }
    fixture.start_connector().await?;
    let documents = table(kind, "documents");
    let mut sequence = 0;
    let mut previous_file = String::new();
    for cancelled in if expects_reply { vec![false, true] } else { vec![false] } {
        if let Ok(mut partial) = partial.lock() {
            *partial = cancelled;
        }
        if expects_reply && !cancelled {
            fixture.push_fault(read_fault());
            fixture.push_fault(metadata_fault());
        }
        let row = incoming(&mut fixture, &mut sequence, "Make file").await?;
        let (thread, turn) = (row["thread"].as_str().ok_or("thread")?.to_owned(), row["turn"].as_str().ok_or("turn")?.to_owned());
        let to_parent = format!("/api/threads/{thread}/turns?focus={turn}");
        if cancelled {
            until("Task asked for input", || async {
                let reply = app.call(Method::GET, &to_parent, None).await?;
                if reply.status == 404 {
                    return Ok(None);
                }
                let first = reply.ok()?[0].clone();
                Ok(first["questions"]
                    .as_array()
                    .is_some_and(|questions| questions.iter().any(|question| question["answer"].is_null()))
                    .then_some(()))
            })
            .await?;
            app.post(&format!("/api/threads/{thread}/turns/{turn}/cancel"), json!({"id": new_id()}))
                .await?;
        }
        if expects_reply && !cancelled {
            survive_faults(&mut fixture, &uploads, &documents).await?;
        }
        wait_done(&mut fixture, &turn).await?;
        deliver(&mut fixture).await?;
        let parent = rows(&app, &to_parent).await?[0].clone();
        ensure!(
            parent["agentMessages"][0]["generatedFiles"].as_array().map_or(0, Vec::len) == usize::from(expects_reply),
            "visible files {}",
            parent["agentMessages"][0]
        );
        let child = parent["agentMessages"][0].clone();
        let child_turns = format!("/api/threads/{}/turns", child["thread"].as_str().ok_or("child thread")?);
        let child_turn = app
            .wait_get(&child_turns, |rows| rows[0]["status"] == "done")
            .await?[0]
            .clone();
        let file = if mode == "nested" {
            rows(&app, &format!("/api/threads/{}/turns", child_turn["agentMessages"][0]["thread"].as_str().ok_or("leaf thread")?))
                .await?[0]["generatedFiles"][0]
                .clone()
        } else {
            child_turn["generatedFiles"][0].clone()
        };
        let file_id = id(&file)?;
        let base = format!("/api/scripts/agents/{owner}/threads/{thread}");
        let path = format!("{base}/turns/{turn}/files");
        let focused = script.json(&format!("{base}/turns?focus={turn}")).await?;
        ensure!(
            focused[0]["generatedFiles"].as_array().map_or(0, Vec::len) == usize::from(expects_reply),
            "script files {}",
            focused[0]["generatedFiles"]
        );
        if !expects_reply {
            ensure!(uploads.lock().map(|u| u.is_empty()).unwrap_or(false), "notice uploaded a file");
            ensure!(script.json(&path).await? == json!([]), "notice listed files");
            ensure!(script.status(&format!("{path}/{file_id}"), None).await? == 404, "notice file readable");
            continue;
        }
        let visible = &parent["agentMessages"][0]["generatedFiles"][0];
        ensure!(visible["id"] == file_id.as_str(), "visible file {visible}");
        let (status, download) = crate::fx_support::fetch(
            &app,
            &format!("/api/threads/{}/files/{file_id}", visible["thread"].as_str().unwrap_or_default()),
        )
        .await?;
        ensure!(status == 200 && download == bytes.as_bytes(), "UI download {status}");
        ensure!(uploads.lock().ok().and_then(|u| u.last().cloned()) == Some(bytes.as_bytes().to_vec()), "uploaded bytes");
        ensure!(script.call(&format!("{path}/{file_id}"), None).await? == (200, bytes.as_bytes().to_vec()), "script download");
        ensure!(script.status(&path.replace(&owner, &foreign), None).await? == 404, "foreign agent read files");
        ensure!(script.status(&format!("{base}/files/{file_id}"), None).await? == 404, "thread file route");
        ensure!(script.status(&path, Some(json!({"id": new_id()}))).await? == 405, "POST files");
        let other = app
            .post(&format!("/api/agents/{owner}/threads"), json!({"id": new_id(), "name": "Unrelated"}))
            .await?;
        ensure!(script.status(&path.replace(&thread, &id(&other)?), None).await? == 404, "unrelated thread");
        ensure!(
            script.status(&format!("{base}/turns/{}/files/{file_id}", new_id()), None).await? == 404,
            "unrelated turn"
        );
        if !previous_file.is_empty() {
            ensure!(script.status(&format!("{path}/{previous_file}"), None).await? == 404, "previous file visible");
        }
        previous_file.clone_from(&file_id);
        if cancelled {
            let method = if kind == Kind::Slack { "files.completeUploadExternal" } else { "sendDocument" };
            ensure!(
                fixture
                    .posts()
                    .iter()
                    .rev()
                    .find(|post| post.method == method)
                    .is_some_and(|post| post.request.text().contains("Partial result")),
                "partial result not marked"
            );
        }
        let count = uploads.lock().map(|u| u.len()).unwrap_or(0);
        fixture.stop().await?;
        fixture.start_connector().await?;
        let probe = incoming(&mut fixture, &mut sequence, if kind == Kind::Slack { "!status" } else { "/status" }).await?;
        wait_done(&mut fixture, probe["turn"].as_str().ok_or("probe turn")?).await?;
        deliver(&mut fixture).await?;
        ensure!(uploads.lock().map(|u| u.len()).unwrap_or(0) == count, "restart uploaded again");
        if !cancelled {
            continue;
        }
        let child_thread = child["thread"].as_str().ok_or("child thread")?.to_owned();
        let side = rows(&app, &format!("/api/agents/{worker}/threads"))
            .await?
            .into_iter()
            .find(|thread| thread["id"] == child_thread.as_str())
            .ok_or("worker side thread")?;
        let archived = app
            .post(
                &format!("/api/threads/{child_thread}/edit"),
                json!({"id": new_id(), "revision": side["revision"], "name": side["name"], "archived": true}),
            )
            .await?;
        app.post(&format!("/api/threads/{child_thread}/delete"), json!({"id": new_id(), "revision": archived["revision"]}))
            .await?;
        ensure!(script.json(&path).await? == json!([]), "deleted child files listed");
        ensure!(script.status(&format!("{path}/{file_id}"), None).await? == 404, "deleted child file readable");
        ensure!(
            rows(&app, &to_parent).await?[0]["agentMessages"][0]["generatedFiles"] == json!([]),
            "deleted file still visible"
        );
        let journal_path = journal(&fixture);
        let document: String = Inspector::open(&journal_path)?.scalar(&format!("SELECT id FROM {documents} WHERE turn=?"), [&turn])?;
        {
            let db = Fixture::open(&journal_path)?;
            if kind == Kind::Slack {
                db.execute("UPDATE slack_documents SET stage=2 WHERE id=?", [&document])?;
            }
            db.execute(
                &format!(
                    "UPDATE {} SET status='queued'{} WHERE id=?",
                    table(kind, "outbox"),
                    if kind == Kind::Telegram { ",part=0" } else { "" }
                ),
                [&document],
            )?;
        }
        let sent = fixture.posts().len();
        let outbox = table(kind, "outbox");
        poll("Deleted artifact was revoked", || {
            fixture.check()?;
            let status: String = Inspector::open(&journal_path)?.scalar(&format!("SELECT status FROM {outbox} WHERE id=?"), [&document])?;
            Ok((status == "revoked").then_some(()))
        })
        .await?;
        ensure!(fixture.posts().len() == sent, "revoked artifact posted");
    }
    ensure!(rows(&app, &format!("/api/agents/{owner}/turns")).await?.is_empty(), "main chat used");
    app.post(&format!("/api/agents/{owner}/scripts"), json!({"id": new_id(), "revision": 1, "enabled": false}))
        .await?;
    ensure!(
        script.status(&format!("/api/scripts/agents/{owner}/threads/{}/turns/{}/files", new_id(), new_id()), None).await? == 403,
        "revoked script access"
    );
    fixture.stop().await
}

#[tokio::test]
async fn slack_ordinary_chat_collects_only_requested_teammate_files_reply() -> Result<()> {
    task_files(Kind::Slack, "reply").await
}

#[tokio::test]
async fn slack_ordinary_chat_collects_only_requested_teammate_files_notice() -> Result<()> {
    task_files(Kind::Slack, "notice").await
}

#[tokio::test]
async fn slack_ordinary_chat_collects_only_requested_teammate_files_nested() -> Result<()> {
    task_files(Kind::Slack, "nested").await
}

#[tokio::test]
async fn telegram_ordinary_chat_collects_only_requested_teammate_files_reply() -> Result<()> {
    task_files(Kind::Telegram, "reply").await
}

#[tokio::test]
async fn telegram_ordinary_chat_collects_only_requested_teammate_files_notice() -> Result<()> {
    task_files(Kind::Telegram, "notice").await
}

#[tokio::test]
async fn telegram_ordinary_chat_collects_only_requested_teammate_files_nested() -> Result<()> {
    task_files(Kind::Telegram, "nested").await
}

async fn workflow_files(kind: Kind, mode: &'static str) -> Result<()> {
    let (owner, worker, teammate, grandchild, foreign) = (new_id(), new_id(), new_id(), new_id(), new_id());
    let text = "workflow artifact exact";
    let model = GatedModel::start().await?;
    let (team, grand) = (teammate.clone(), grandchild.clone());
    model.respond(move |request| {
        if after_pause(request) {
            return pause_reply_for(request);
        }
        if request.messages().iter().any(|message| message["role"] == "tool") {
            return Answer::text("Done");
        }
        let model = request.model();
        if ["delegate", "nested", "notice"].contains(&model) {
            Answer::tool_id(
                "file",
                "send_message",
                json!({"recipients": [if model == "nested" { &grand } else { &team }], "text": "Make file",
                    "expects_reply": model != "notice"}),
            )
        } else {
            Answer::tool_id("file", "create_text_file", json!({"name": "workflow.txt", "text": text}))
        }
    });
    let App { app, root, _model } = app_with(model).await?;
    let script = Script { directory: root.path().join("scripts") };
    let uploads: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
    let mut fixture = ConnectorFixture::start(
        kind,
        root.path(),
        &owner,
        app.origin(),
        SCRIPT_TOKEN,
        Some(artifact_receiver(kind, uploads.clone())),
        users(kind),
        Vec::new(),
    )
    .await?;
    for agent in [&owner, &worker, &teammate, &grandchild, &foreign] {
        let model = if mode != "step" && *agent == worker {
            if mode == "notice" { "notice" } else { "delegate" }
        } else if mode == "nested" && *agent == teammate {
            "nested"
        } else {
            "fixture"
        };
        app.post(
            "/api/agents",
            json!({"runtime": "api", "id": agent, "name": agent, "model": model, "instructions": "Help"}),
        )
        .await?;
    }
    for agent in [&owner, &foreign] {
        app.post(&format!("/api/agents/{agent}/scripts"), json!({"id": new_id(), "revision": 0, "enabled": true}))
            .await?;
    }
    fixture.start_connector().await?;
    let mut sequence = 0;
    let mut previous_file = String::new();
    let prefix = if kind == Kind::Slack { "!" } else { "/" };
    for partial in if mode == "notice" { vec![false] } else { vec![false, true] } {
        let mut nodes = vec![json!({"id": "work", "kind": "agent", "agent": worker, "prompt": "Make file",
            "next": if partial { "ask" } else { "out" }})];
        if partial {
            nodes.push(json!({"id": "ask", "kind": "approval", "prompt": "Continue?", "yes": "out"}));
        }
        nodes.push(json!({"id": "out", "kind": "output"}));
        let workflow = app
            .post(
                &format!("/api/agents/{owner}/workflows"),
                json!({"id": new_id(), "revision": 0, "name": "Files", "graph": {"entry": "work", "maxSteps": 3, "nodes": nodes}}),
            )
            .await?;
        let workflow_id = id(&workflow)?;
        if mode != "notice" && !partial {
            fixture.push_fault(read_fault());
            fixture.push_fault(metadata_fault());
        }
        let row = incoming(&mut fixture, &mut sequence, &format!("{prefix}workflow {workflow_id} 1 Work")).await?;
        let run_id = row["turn"].as_str().ok_or("run")?.to_owned();
        if partial {
            let run = until("Workflow reached approval", || async {
                let reply = app.call(Method::GET, &format!("/api/workflow-runs/{run_id}"), None).await?;
                if reply.status == 404 {
                    return Ok(None);
                }
                let run = reply.ok()?;
                Ok((run["status"] == "approval").then_some(run))
            })
            .await?;
            app.post(&format!("/api/workflow-runs/{run_id}/cancel"), json!({"id": new_id(), "revision": run["revision"]}))
                .await?;
        }
        if mode != "notice" && !partial {
            survive_faults(&mut fixture, &uploads, &table(kind, "document_workflows")).await?;
        }
        wait_done(&mut fixture, &run_id).await?;
        deliver(&mut fixture).await?;
        let path = format!("/api/scripts/agents/{owner}/workflow-runs/{run_id}");
        let run = script.json(&path).await?;
        if mode == "step" && !partial {
            ensure!(run["status"] == "done" && run["result"] == "Done", "run {run}");
            let method = if kind == Kind::Slack { "chat.postMessage" } else { "sendMessage" };
            ensure!(fixture.json_posts(method).iter().any(|post| post["text"] == "Done"), "result not posted");
        }
        if mode == "notice" {
            let detail = app.get(&format!("/api/workflow-runs/{run_id}")).await?;
            let step = detail["steps"].as_array().and_then(|steps| steps.iter().find(|step| step["thread"].is_string())).ok_or("step thread")?;
            let parent = rows(&app, &format!("/api/threads/{}/turns", step["thread"].as_str().unwrap_or_default())).await?[0].clone();
            let child_turns = format!("/api/threads/{}/turns", parent["agentMessages"][0]["thread"].as_str().ok_or("child")?);
            let child_turn = app.wait_get(&child_turns, |rows| rows[0]["status"] == "done").await?[0].clone();
            ensure!(child_turn["generatedFiles"].as_array().map(Vec::len) == Some(1), "teammate file");
            ensure!(script.json(&path).await?["generatedFiles"] == json!([]), "notice file collected");
            ensure!(uploads.lock().map(|u| u.is_empty()).unwrap_or(false), "notice file uploaded");
            ensure!(
                script.status(&format!("{path}/files/{}", id(&child_turn["generatedFiles"][0])?), None).await? == 404,
                "notice file readable"
            );
            app.post(&format!("/api/workflows/{workflow_id}/delete"), json!({"id": new_id(), "revision": 1})).await?;
            continue;
        }
        ensure!(run["generatedFiles"].as_array().map(Vec::len) == Some(1), "run files {}", run["generatedFiles"]);
        ensure!(uploads.lock().ok().and_then(|u| u.last().cloned()) == Some(text.as_bytes().to_vec()), "uploaded bytes");
        let file = id(&run["generatedFiles"][0])?;
        ensure!(script.call(&format!("{path}/files/{file}"), None).await? == (200, text.as_bytes().to_vec()), "script download");
        ensure!(
            script.status(&format!("/api/scripts/agents/{foreign}/workflow-runs/{run_id}/files/{file}"), None).await? == 404,
            "foreign agent read the file"
        );
        ensure!(script.status(&format!("{path}/files/{}", new_id()), None).await? == 404, "unknown file");
        if !previous_file.is_empty() {
            ensure!(script.status(&format!("{path}/files/{previous_file}"), None).await? == 404, "previous file visible");
        }
        previous_file.clone_from(&file);
        if partial {
            let method = if kind == Kind::Slack { "files.completeUploadExternal" } else { "sendDocument" };
            ensure!(
                fixture.posts().iter().rev().find(|post| post.method == method).is_some_and(|post| post.request.text().contains("Partial result")),
                "partial result not marked"
            );
        }
        let count = uploads.lock().map(|u| u.len()).unwrap_or(0);
        fixture.stop().await?;
        fixture.start_connector().await?;
        let probe = incoming(&mut fixture, &mut sequence, &format!("{prefix}status")).await?;
        wait_done(&mut fixture, probe["turn"].as_str().ok_or("probe")?).await?;
        deliver(&mut fixture).await?;
        ensure!(uploads.lock().map(|u| u.len()).unwrap_or(0) == count, "restart uploaded again");
        let delete_thread = |thread: String, owner_agent: String| {
            let app = &app;
            async move {
                let side = rows(app, &format!("/api/agents/{owner_agent}/threads"))
                    .await?
                    .into_iter()
                    .find(|row| row["id"] == thread.as_str())
                    .ok_or("side thread")?;
                let archived = app
                    .post(&format!("/api/threads/{thread}/edit"), json!({"id": new_id(), "revision": side["revision"], "name": side["name"], "archived": true}))
                    .await?;
                app.post(&format!("/api/threads/{thread}/delete"), json!({"id": new_id(), "revision": archived["revision"]})).await?;
                Ok::<_, minarbot_testkit::Error>(())
            }
        };
        let detail = app.get(&format!("/api/workflow-runs/{run_id}")).await?;
        let step_thread = detail["steps"]
            .as_array()
            .and_then(|steps| steps.iter().find_map(|step| step["thread"].as_str()))
            .ok_or("step thread")?
            .to_owned();
        if !partial && ["reply", "nested"].contains(&mode) {
            let parent = rows(&app, &format!("/api/threads/{step_thread}/turns")).await?[0].clone();
            let child = parent["agentMessages"][0]["thread"].as_str().ok_or("child thread")?.to_owned();
            delete_thread(child, teammate.clone()).await?;
            ensure!(script.status(&format!("{path}/files/{file}"), None).await? == 404, "deleted teammate file readable");
            ensure!(script.json(&path).await?["generatedFiles"] == json!([]), "deleted teammate file listed");
        }
        if partial {
            delete_thread(step_thread, worker.clone()).await?;
            ensure!(script.status(&format!("{path}/files/{file}"), None).await? == 404, "deleted step file readable");
            ensure!(script.json(&path).await?["generatedFiles"] == json!([]), "deleted step file listed");
        }
        app.post(&format!("/api/workflows/{workflow_id}/delete"), json!({"id": new_id(), "revision": 1})).await?;
        ensure!(script.status(&format!("{path}/files/{file}"), None).await? == 404, "deleted workflow file readable");
    }
    ensure!(rows(&app, &format!("/api/agents/{owner}/turns")).await?.is_empty(), "main chat used");
    app.post(&format!("/api/agents/{owner}/scripts"), json!({"id": new_id(), "revision": 1, "enabled": false}))
        .await?;
    ensure!(
        script.status(&format!("/api/scripts/agents/{owner}/workflow-runs/{}", new_id()), None).await? == 403,
        "revoked script access"
    );
    fixture.stop().await
}

#[tokio::test]
async fn slack_step_workflow_artifacts_are_scoped_to_owned_run_and_retain_partial_bytes() -> Result<()> {
    workflow_files(Kind::Slack, "step").await
}

#[tokio::test]
async fn slack_reply_workflow_artifacts_are_scoped_to_owned_run_and_retain_partial_bytes() -> Result<()> {
    workflow_files(Kind::Slack, "reply").await
}

#[tokio::test]
async fn slack_notice_workflow_artifacts_are_scoped_to_owned_run_and_retain_partial_bytes() -> Result<()> {
    workflow_files(Kind::Slack, "notice").await
}

#[tokio::test]
async fn slack_nested_workflow_artifacts_are_scoped_to_owned_run_and_retain_partial_bytes() -> Result<()> {
    workflow_files(Kind::Slack, "nested").await
}

#[tokio::test]
async fn telegram_step_workflow_artifacts_are_scoped_to_owned_run_and_retain_partial_bytes() -> Result<()> {
    workflow_files(Kind::Telegram, "step").await
}

#[tokio::test]
async fn telegram_reply_workflow_artifacts_are_scoped_to_owned_run_and_retain_partial_bytes() -> Result<()> {
    workflow_files(Kind::Telegram, "reply").await
}

#[tokio::test]
async fn telegram_notice_workflow_artifacts_are_scoped_to_owned_run_and_retain_partial_bytes() -> Result<()> {
    workflow_files(Kind::Telegram, "notice").await
}

#[tokio::test]
async fn telegram_nested_workflow_artifacts_are_scoped_to_owned_run_and_retain_partial_bytes() -> Result<()> {
    workflow_files(Kind::Telegram, "nested").await
}

async fn offline_recovery(kind: Kind) -> Result<()> {
    let root = TempWorkspace::new()?;
    let directory = root.join("state");
    let id = new_id();
    let command = |name: &str, extra: Value| {
        let mut input = json!({"directory": directory, "platform": kind.name(), "command": name});
        if let (Some(target), Value::Object(extra)) = (input.as_object_mut(), extra) {
            target.extend(extra);
        }
        input
    };
    let initial = connector::journal(&command("status", json!({}))).await?;
    ensure!(initial["inbox"] == json!([]) && initial["outbox"] == json!([]), "initial {initial}");
    let database = directory.join(format!("{}.sqlite", kind.name()));
    let db = Fixture::open(&database)?;
    match kind {
        Kind::Slack => db.execute("INSERT INTO slack_outbox(id,channel,sender,root,text,status,part,timestamps) VALUES(?,'DCHAT','UUSER','1700000000.000001','Exact output','uncertain',1,'[\"1700000001.000001\"]')", [&id])?,
        Kind::Telegram => db.execute("INSERT INTO telegram_outbox(id,chat,reply_to,text,status,part,message_ids,error) VALUES(?,4,1,'Exact output','uncertain',1,'[42]','Lost response')", [&id])?,
    };
    let original = connector::journal(&command("inspect", json!({"id": id}))).await?["reply"].clone();
    let error = connector::journal(&command("recover", json!({"id": id, "revision": 0, "action": "retry"})))
        .await
        .err()
        .ok_or("retry without confirmation succeeded")?;
    ensure!(error.to_string().contains("confirmation"), "error {error}");
    ensure!(connector::journal(&command("inspect", json!({"id": id}))).await?["reply"] == original, "refused retry changed the reply");
    let confirmed = connector::journal(&command("recover", json!({"id": id, "revision": 0, "action": "retry", "confirmation": "--accept-duplicate-risk"}))).await?;
    ensure!(confirmed == json!({"id": id, "status": "queued", "revision": 1}), "retry {confirmed}");
    let retry = connector::journal(&command("inspect", json!({"id": id}))).await?;
    ensure!(retry["reply"]["part"] == 1 && retry["reply"]["text"] == "Exact output", "retry reply {retry}");
    ensure!(retry["decisions"][0]["status"] == "uncertain", "decision {}", retry["decisions"]);
    let ids = if kind == Kind::Slack { "timestamps" } else { "message_ids" };
    ensure!(retry["reply"][ids] == original[ids], "delivered ids changed");
    let stale = connector::journal(&command("recover", json!({"id": id, "revision": 0, "action": "retry", "confirmation": "--accept-duplicate-risk"})))
        .await
        .err()
        .ok_or("stale retry succeeded")?;
    ensure!(stale.to_string().contains("changed"), "stale error {stale}");
    db.execute(&format!("UPDATE {}_outbox SET status='failed' WHERE id=?", kind.name()), [&id])?;
    let discarded = connector::journal(&command("recover", json!({"id": id, "revision": 1, "action": "discard", "confirmation": "--confirm"}))).await?;
    ensure!(discarded == json!({"id": id, "status": "discarded", "revision": 2}), "discard {discarded}");
    ensure!(connector::journal(&command("status", json!({}))).await?["outbox"][0]["revision"] == 2, "status revision");
    drop(db);
    ensure!(!directory.join("owner.lock").exists(), "journal left the owner lock");
    Ok(())
}

#[tokio::test]
async fn rust_slack_offline_recovery_preserves_confirmed_delivery_and_requires_confirmation() -> Result<()> {
    offline_recovery(Kind::Slack).await
}

#[tokio::test]
async fn rust_telegram_offline_recovery_preserves_confirmed_delivery_and_requires_confirmation() -> Result<()> {
    offline_recovery(Kind::Telegram).await
}

#[derive(Default)]
struct Runner {
    deliveries: usize,
    answer: Option<String>,
    offset: i64,
    acknowledgements: usize,
    connections: usize,
    uploaded: usize,
    downloaded: usize,
    workflow_run: Value,
    requests: Vec<String>,
    abort: bool,
    sockets: Vec<tokio::sync::mpsc::UnboundedSender<String>>,
    problems: Vec<String>,
}

fn runner_lock(state: &Arc<Mutex<Runner>>) -> std::sync::MutexGuard<'_, Runner> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// `/approve <uuid> 1 yes` (with the given command prefix) inside `text`.
fn approve_command(text: &str, prefix: char) -> Option<String> {
    let start = text.find(&format!("{prefix}approve "))?;
    let rest = &text[start..];
    let command: String = rest.chars().take(1 + "approve ".len() + 36 + " 1 yes".len()).collect();
    (command.ends_with(" 1 yes") && command.len() == 1 + 8 + 36 + 6).then_some(command)
}

/// The question id after `<prefix>answer `.
fn answer_id(text: &str, prefix: char) -> Option<String> {
    let start = text.find(&format!("{prefix}answer "))? + 1 + "answer ".len();
    let id: String = text[start..].chars().take(36).collect();
    (id.len() == 36 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')).then_some(id)
}

fn runner_event(id: u64, text: &str, media: bool) -> Value {
    let mut event = json!({"type": "message", "channel_type": "im", "channel": "DCHAT", "user": "UUSER", "text": text,
        "ts": format!("1700000000.{id:06}")});
    if media {
        event["files"] = json!([{"id": "FTEXT"}]);
        event["subtype"] = json!("file_share");
    }
    if id > 1 {
        event["thread_ts"] = json!("1700000000.000001");
    }
    json!({"type": "events_api", "envelope_id": format!("e{id}"), "payload": {"type": "event_callback", "team_id": "TTEAM",
        "api_app_id": "AAPP", "event_id": format!("Ev{id}"), "authorizations": [{"team_id": "TTEAM", "user_id": "UBOT", "is_bot": true}],
        "event": event}})
}

async fn runner(kind: Kind, mode: &'static str) -> Result<()> {
    let (question, file, workflow, media) = (mode == "question", mode == "file", mode == "workflow", mode == "media");
    let agent = new_id();
    let workflow_id = new_id();
    let model = GatedModel::start().await?;
    model.respond(move |request| {
        if after_pause(request) {
            return pause_reply_for(request);
        }
        let tool = (question || file) && !request.messages().iter().any(|message| message["role"] == "tool");
        if !tool {
            return Answer::text("Native connector reply");
        }
        if file {
            Answer::tool_id("effect", "create_text_file", json!({"name": "fixture.txt", "text": "Exact native file bytes"}))
        } else {
            Answer::tool_id("effect", "ask_user", json!({"question": "Proceed?", "choices": ["Yes", "No"]}))
        }
    });
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new().gateway(Gateway::new(model.base_url()))).await?;
    let state: Arc<Mutex<Runner>> = Arc::default();
    let token = format!("{}{}", new_id().replace('-', ""), new_id().replace('-', ""));
    let (local_state, app_origin, local_token, owner) = (state.clone(), app.origin().to_owned(), token.clone(), agent.clone());
    let client = minarbot_testkit::http::Client::builder().no_proxy().build()?;
    let local = minarbot_testkit::stub::Stub::start_async(move |request| {
        let (state, app_origin, token, owner, client) =
            (local_state.clone(), app_origin.clone(), local_token.clone(), owner.clone(), client.clone());
        async move {
            use minarbot_testkit::stub::StubReply;
            if request.header("authorization") != Some(format!("Bearer {token}").as_str()) {
                return StubReply::Json(403, json!({}));
            }
            runner_lock(&state).requests.push(request.target.clone());
            let base = format!("/api/scripts/agents/{owner}");
            if workflow && request.target.contains("/workflow") {
                let body = request.json();
                let mut runner = runner_lock(&state);
                if request.target.ends_with("/run") {
                    if body["revision"] != 1 || body["input"] != "Exact input" {
                        runner.problems.push(format!("workflow run {body}"));
                    }
                    runner.workflow_run = json!({"id": body["id"], "status": "approval", "revision": 1, "prompt": "Proceed?", "generatedFiles": []});
                } else if request.target.ends_with("/approve") {
                    if body["revision"] != 1 || body["accepted"] != true || body["id"] == runner.workflow_run["id"] {
                        runner.problems.push(format!("workflow approval {body}"));
                    }
                    runner.workflow_run["status"] = json!("done");
                    runner.workflow_run["revision"] = json!(2);
                    runner.workflow_run["result"] = json!("Exact input");
                }
                return StubReply::ok(runner.workflow_run.clone());
            }
            let mut path = match request.target.strip_prefix(&format!("{base}/threads/")) {
                Some(rest) => format!("/api/threads/{rest}"),
                None => request.target.replacen(&base, &format!("/api/agents/{owner}"), 1),
            };
            if let Some((head, tail)) = path.split_once("/turns/") {
                if let Some((_, file)) = tail.split_once("/files/") {
                    path = format!("{head}/files/{file}");
                }
            }
            if media && request.header("content-type") == Some("application/octet-stream") {
                let mut runner = runner_lock(&state);
                if request.body != b"Native media bytes" {
                    runner.problems.push("media upload bytes".into());
                }
                runner.uploaded += 1;
            }
            let method = minarbot_testkit::http::Method::from_bytes(request.method.as_bytes()).unwrap_or(minarbot_testkit::http::Method::GET);
            let mut forwarded = client
                .request(method, format!("{app_origin}{path}"))
                .header("Origin", &app_origin)
                .header("Content-Type", request.header("content-type").unwrap_or("application/json"));
            if let Some(name) = request.header("x-file-name") {
                forwarded = forwarded.header("X-File-Name", name);
            }
            if !request.body.is_empty() {
                forwarded = forwarded.body(request.body.clone());
            }
            match forwarded.send().await {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let content_type = response
                        .headers()
                        .get("content-type")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("application/json")
                        .to_owned();
                    let bytes = response.bytes().await.map(|bytes| bytes.to_vec()).unwrap_or_default();
                    StubReply::Raw(status, vec![("Content-Type".into(), content_type)], bytes)
                }
                Err(_) => StubReply::Drop,
            }
        }
    })
    .await?;
    let scripts = root.dir("scripts")?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&scripts, std::fs::Permissions::from_mode(0o700))?;
        std::fs::write(scripts.join("connection.json"), json!({"url": local.origin(), "token": token}).to_string())?;
        std::fs::set_permissions(scripts.join("connection.json"), std::fs::Permissions::from_mode(0o600))?;
    }
    let first_text = if workflow { format!("!workflow {workflow_id} 1 Exact input") } else { "Native request".to_owned() };
    let origin_cell: Arc<Mutex<String>> = Arc::default();
    let (remote_state, remote_origin) = (state.clone(), origin_cell.clone());
    let final_text = if workflow { "Exact input" } else { "Native connector reply" };
    let socket_state = state.clone();
    let socket_first = first_text.clone();
    let remote = minarbot_testkit::stub::Stub::with_socket(
        move |request| {
            let reply = runner_remote(kind, &remote_state, &remote_origin, &request, workflow, file, media, final_text, workflow_id.as_str());
            async move { reply }
        },
        move |socket| {
            let minarbot_testkit::stub::Socket { index, mut incoming, outgoing } = socket;
            let _ = outgoing.send(json!({"type": "hello", "connection_info": {"app_id": "AAPP"}}).to_string());
            let _ = outgoing.send(runner_event(1, &socket_first, media).to_string());
            {
                let mut runner = runner_lock(&socket_state);
                runner.connections += 1;
                if workflow {
                    if let Some(answer) = runner.answer.clone() {
                        let _ = outgoing.send(runner_event(2, &answer, false).to_string());
                    }
                }
                runner.sockets.push(outgoing.clone());
            }
            let acks = socket_state.clone();
            tokio::spawn(async move {
                while let Some(text) = incoming.recv().await {
                    let ack: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                    let envelope = ack["envelope_id"].as_str().unwrap_or_default().to_owned();
                    let mut runner = runner_lock(&acks);
                    if !["e1", "e2"].contains(&envelope.as_str()) {
                        runner.problems.push(format!("ack {ack}"));
                    }
                    runner.acknowledgements += 1;
                    if index == 0 && envelope == "e1" {
                        let _ = outgoing.send(json!({"type": "disconnect", "reason": "refresh_requested"}).to_string());
                    }
                }
            });
        },
    )
    .await?;
    if let Ok(mut origin) = origin_cell.lock() {
        *origin = remote.origin().to_owned();
    }
    app.post(
        "/api/agents",
        json!({"id": agent, "runtime": "api", "name": "Native", "model": "fixture", "instructions": "Help"}),
    )
    .await?;
    let directory = root.path().join("state");
    let mut options = json!({"platform": kind.name(), "directory": directory, "scriptDirectory": scripts, "agent": agent,
        "origin": remote.origin(), "users": if kind == Kind::Slack { json!(["UUSER"]) } else { json!([4]) }});
    match kind {
        Kind::Slack => {
            options["team"] = json!("TTEAM");
            options["app"] = json!("AAPP");
            options["botToken"] = json!(minarbot_testkit::platform::BOT_TOKEN);
            options["appToken"] = json!(minarbot_testkit::platform::APP_TOKEN);
        }
        Kind::Telegram => options["token"] = json!(minarbot_testkit::platform::TELEGRAM_TOKEN),
    }
    let mut process = connector::Connector::start(&options).await?;
    // The Node controller aborted 100 ms after the last expected delivery,
    // or after 12 s.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(12);
    while !runner_lock(&state).abort && tokio::time::Instant::now() < deadline {
        if let Some(status) = process.exited()? {
            return Err(format!("connector stopped ({status}): {}", process.stderr()).into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    process.stop().await?;
    let runner = std::mem::take(&mut *runner_lock(&state));
    ensure!(runner.problems.is_empty(), "fake checks {:?}", runner.problems);
    let calls = crate::fx_support::work_calls(&model).len();
    let expected_calls = if workflow { 0 } else if file { 3 } else if question { 2 } else { 1 };
    ensure!(calls == expected_calls, "{mode} model calls {calls}");
    let expected_deliveries = if question || workflow { 3 } else if file { 2 } else { 1 };
    ensure!(runner.deliveries == expected_deliveries, "{mode} deliveries {}", runner.deliveries);
    if media {
        ensure!(runner.downloaded == 1 && runner.uploaded == 1, "media {} {}", runner.downloaded, runner.uploaded);
    }
    if workflow {
        ensure!(runner.workflow_run["status"] == "done", "workflow {}", runner.workflow_run);
    }
    let marker = if workflow { "/workflows/" } else { "/turns" };
    ensure!(runner.requests.iter().any(|path| path.contains(marker)), "no {marker} request: {:?}", runner.requests);
    ensure!(!directory.join("owner.lock").exists(), "owner lock kept");
    let outbox = Inspector::open(directory.join(format!("{}.sqlite", kind.name())))?
        .rows(&format!("SELECT status FROM {}_outbox", kind.name()), [])?;
    ensure!(outbox.iter().all(|row| row["status"] == "sent"), "outbox {outbox:?}");
    if kind == Kind::Slack {
        ensure!(runner.connections >= 2 && runner.acknowledgements >= 2, "socket {} {}", runner.connections, runner.acknowledgements);
    } else {
        ensure!(runner.offset >= 8, "offset {}", runner.offset);
    }
    ensure!(rows(&app, &format!("/api/agents/{agent}/turns")).await?.is_empty(), "main chat used");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn runner_remote(
    kind: Kind,
    state: &Arc<Mutex<Runner>>,
    origin: &Arc<Mutex<String>>,
    request: &minarbot_testkit::stub::StubRequest,
    workflow: bool,
    file: bool,
    media: bool,
    final_text: &str,
    workflow_id: &str,
) -> minarbot_testkit::stub::StubReply {
    use minarbot_testkit::stub::StubReply;
    let origin = origin.lock().map(|origin| origin.clone()).unwrap_or_default();
    let mut runner = runner_lock(state);
    let target = request.target.clone();
    if target == "/download" || target.starts_with("/file/") {
        if kind == Kind::Slack && request.header("authorization") != Some(format!("Bearer {}", minarbot_testkit::platform::BOT_TOKEN).as_str()) {
            runner.problems.push("download without the bot token".into());
        }
        runner.downloaded += 1;
        return StubReply::Raw(200, vec![], b"Native media bytes".to_vec());
    }
    if target == "/upload" || target.ends_with("/sendDocument") {
        if !request.text().contains("Exact native file bytes") {
            runner.problems.push("uploaded document bytes".into());
        }
        runner.deliveries += 1;
        if kind == Kind::Telegram {
            runner.abort = true;
        }
        return StubReply::ok(json!({"ok": true, "result": {"message_id": 40, "chat": {"id": 4}}}));
    }
    let body = request.json();
    let text = body["text"].as_str().unwrap_or_default().to_owned();
    match kind {
        Kind::Telegram => {
            let result = if target.ends_with("/getMe") {
                json!({"id": 123, "is_bot": true})
            } else if target.ends_with("/getFile") {
                json!({"file_id": "FTEXT", "file_path": "fixture.txt", "file_size": 18})
            } else if target.ends_with("/getUpdates") {
                if body["timeout"] != 25 || body["limit"] != 20 {
                    runner.problems.push(format!("poll {body}"));
                }
                let offset = body["offset"].as_i64().unwrap_or(0);
                runner.offset = runner.offset.max(offset);
                if offset < 8 {
                    let mut message = json!({"message_id": 8, "chat": {"id": 4, "type": "private"}, "from": {"id": 4, "is_bot": false},
                        "text": if workflow { format!("/workflow {workflow_id} 1 Exact input") } else { "Native request".into() }});
                    if media {
                        message["document"] = json!({"file_id": "FTEXT", "file_name": "fixture.txt", "file_size": 18});
                        message["caption"] = json!("Native request");
                    }
                    json!([{"update_id": 7, "message": message}])
                } else if let (Some(answer), true) = (runner.answer.clone(), offset < 9) {
                    json!([{"update_id": 8, "message": {"message_id": 9, "chat": {"id": 4, "type": "private"}, "from": {"id": 4, "is_bot": false},
                        "text": if workflow { answer } else { format!("/answer {answer} Yes") }}}])
                } else {
                    json!([])
                }
            } else {
                if body["chat_id"] != 4 {
                    runner.problems.push(format!("chat {body}"));
                }
                runner.deliveries += 1;
                let result = json!({"message_id": 20 + runner.deliveries, "chat": {"id": 4}});
                if workflow && text.contains("/approve ") {
                    runner.answer = approve_command(&text, '/');
                }
                if text.contains("/answer ") {
                    runner.answer = answer_id(&text, '/');
                }
                if text == final_text && !file {
                    runner.abort = true;
                }
                result
            };
            StubReply::ok(json!({"ok": true, "result": result}))
        }
        Kind::Slack => {
            let mut result = if target.ends_with("/auth.test") {
                json!({"team_id": "TTEAM", "user_id": "UBOT", "bot_id": "BBOT"})
            } else if target.ends_with("/files.info") {
                json!({"file": {"id": "FTEXT", "name": "fixture.txt", "mode": "hosted", "is_external": false, "size": 18,
                    "url_private_download": format!("{origin}/download")}})
            } else if target.ends_with("/apps.connections.open") {
                json!({"url": format!("{}/socket", origin.replacen("http:", "ws:", 1))})
            } else if target.ends_with("/files.getUploadURLExternal") {
                json!({"file_id": "FFILE", "upload_url": format!("{origin}/upload")})
            } else if target.ends_with("/files.completeUploadExternal") {
                runner.abort = true;
                json!({"files": body["files"]})
            } else {
                if body["channel"] != "DCHAT" || body["thread_ts"] != "1700000000.000001" {
                    runner.problems.push(format!("post {body}"));
                }
                runner.deliveries += 1;
                let result = json!({"channel": "DCHAT", "ts": format!("1700000001.{:06}", runner.deliveries)});
                let push = |runner: &Runner, frame: Value| {
                    for socket in &runner.sockets {
                        let _ = socket.send(frame.to_string());
                    }
                };
                if workflow && text.contains("!approve ") {
                    runner.answer = approve_command(&text, '!');
                    if let Some(answer) = runner.answer.clone() {
                        push(&runner, runner_event(2, &answer, false));
                    }
                }
                if text.contains("!answer ") {
                    runner.answer = answer_id(&text, '!');
                    if let Some(answer) = runner.answer.clone() {
                        push(&runner, runner_event(2, &format!("!answer {answer} Yes"), false));
                    }
                }
                if text == final_text && !file {
                    runner.abort = true;
                }
                result
            };
            if let Some(fields) = result.as_object_mut() {
                fields.insert("ok".into(), json!(true));
            }
            StubReply::ok(result)
        }
    }
}

macro_rules! runner_case {
    ($name:ident, $kind:expr, $mode:literal) => {
        #[tokio::test]
        async fn $name() -> Result<()> {
            runner($kind, $mode).await
        }
    };
}

runner_case!(rust_telegram_runner_reply_through_native_turns, Kind::Telegram, "reply");
runner_case!(rust_telegram_runner_question_through_native_turns, Kind::Telegram, "question");
runner_case!(rust_telegram_runner_file_through_native_turns, Kind::Telegram, "file");
runner_case!(rust_telegram_runner_workflow_through_native_turns, Kind::Telegram, "workflow");
runner_case!(rust_telegram_runner_media_through_native_turns, Kind::Telegram, "media");
runner_case!(rust_slack_runner_reply_through_native_turns, Kind::Slack, "reply");
runner_case!(rust_slack_runner_question_through_native_turns, Kind::Slack, "question");
runner_case!(rust_slack_runner_file_through_native_turns, Kind::Slack, "file");
runner_case!(rust_slack_runner_workflow_through_native_turns, Kind::Slack, "workflow");
runner_case!(rust_slack_runner_media_through_native_turns, Kind::Slack, "media");
