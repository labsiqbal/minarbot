//! Slack artifact uploads across uncertain completion and grant recovery
//! (port of `app/slack-output.test.ts`).

use crate::sb_support::{after_tool, reminder};
use minarbot_testkit::{
    connector_fixture::{envelope, journal, SlackCall, SlackFixture, SlackReply, BOT_TOKEN},
    control::{Answer, ControlModel},
    db::Inspector,
    prelude::*,
    Server,
};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering::SeqCst},
        Arc, Mutex,
    },
};

fn private_dir(path: &Path) -> Result<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(path.to_owned())
}

const TEXT: &str = "<h1>Exact Slack output</h1>\n";
const SCRIPT_TOKEN: &str = "fixture-output-script-token-01234567890";

#[derive(Default)]
struct Remote {
    lost: bool,
    allocated: Vec<Value>,
    uploaded: Vec<Vec<u8>>,
    completed: Vec<Value>,
    authorization_ok: bool,
}

fn remote(state: &Arc<Mutex<Remote>>, call: &SlackCall) -> SlackReply {
    let Ok(mut state) = state.lock() else {
        return SlackReply::Lost;
    };
    if call.path.starts_with("/upload/") {
        state.authorization_ok &= call.authorization.is_none();
        state.uploaded.push(call.body.clone());
        return SlackReply::Raw("OK");
    }
    state.authorization_ok &= call.authorization.as_deref() == Some(&format!("Bearer {BOT_TOKEN}"));
    let body = call.json();
    match call.method.as_str() {
        "files.getUploadURLExternal" => {
            state.allocated.push(body);
            let n = state.allocated.len();
            SlackReply::Ok(json!({"file_id": format!("FFILE{n}"),
                "upload_url": format!("http://{}/upload/{n}", call.host)}))
        }
        "files.completeUploadExternal" => {
            state.completed.push(body.clone());
            if state.lost {
                state.lost = false;
                return SlackReply::Lost;
            }
            SlackReply::Ok(json!({"files": body["files"]}))
        }
        _ => SlackReply::Default,
    }
}

fn status(journal_path: &Path, id: &Value) -> Result<Value> {
    let db = Inspector::open(journal_path)?;
    let rows = db.rows(
        "SELECT status FROM slack_outbox WHERE id=?",
        [id.as_str().unwrap_or_default()],
    )?;
    Ok(rows
        .first()
        .map(|row| row["status"].clone())
        .unwrap_or(Value::Null))
}

fn document(journal_path: &Path, turn: &Value) -> Result<Option<Value>> {
    if !journal_path.exists() {
        return Ok(None);
    }
    let db = Inspector::open(journal_path)?;
    Ok(db
        .rows(
            "SELECT * FROM slack_documents WHERE turn=?",
            [turn.as_str().unwrap_or_default()],
        )?
        .into_iter()
        .next())
}

async fn incoming(fixture: &mut SlackFixture, sequence: &mut u64, text: &str) -> Result<Value> {
    *sequence += 1;
    fixture.send(envelope(*sequence, text, None)).await?;
    let db = Inspector::open(fixture.journal())?;
    db.row("SELECT * FROM slack_inbox ORDER BY rowid DESC LIMIT 1", [])
}

async fn settled(
    fixture: &mut SlackFixture,
    label: &str,
    mut ready: impl FnMut(&Path) -> Result<bool>,
) -> Result<()> {
    let path = fixture.journal();
    let deadline = tokio::time::Instant::now() + minarbot_testkit::WAIT;
    loop {
        fixture.check()?;
        if ready(&path)? {
            return Ok(());
        }
        if tokio::time::Instant::now() > deadline {
            return Err(format!("connector did not settle: {label}").into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

async fn recover(fixture: &SlackFixture, id: &Value) -> Result<Value> {
    journal(
        &json!({"platform": "slack", "directory": fixture.directory, "command": "recover", "id": id,
        "revision": 0, "action": "retry", "confirmation": "--accept-duplicate-risk"}),
    )
    .await
}

async fn grant(app: &Server, agent: &str, revision: i64, enabled: bool) -> Result<()> {
    app.post(
        &format!("/api/agents/{agent}/scripts"),
        json!({"id": new_id(), "revision": revision, "enabled": enabled}),
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn slack_artifacts_retain_exact_bytes_and_upload_stages_across_uncertain_completion_and_grant_recovery(
) -> Result<()> {
    let question = Arc::new(AtomicBool::new(false));
    let asking = question.clone();
    let model = ControlModel::start().await?;
    model.respond(move |request| {
        let calls = request.tool_calls();
        if calls.is_empty() && !reminder(request) {
            return Answer::tool(
                "create_text_file",
                json!({"name": "result.html", "text": TEXT}),
            );
        }
        if asking.load(SeqCst) && after_tool(request) && !request.called("ask_user") {
            return Answer::tool(
                "ask_user",
                json!({"question": "Continue?", "choices": ["Yes", "No"]}),
            );
        }
        Answer::text("File ready")
    });
    let root = TestRoot::new()?;
    let agent = new_id();
    let app = root
        .start(
            ServerConfig::new()
                .gateway(model.gateway())
                .option("scriptToken", SCRIPT_TOKEN)
                .script_directory(private_dir(&root.dir("scripts")?)?),
        )
        .await?;
    app.post(
        "/api/agents",
        json!({"runtime": "api", "id": agent, "name": "Output", "model": "fixture", "instructions": "Help"}),
    )
    .await?;
    grant(&app, &agent, 0, true).await?;
    let state = Arc::new(Mutex::new(Remote {
        lost: true,
        authorization_ok: true,
        ..Remote::default()
    }));
    let shared = state.clone();
    let mut fixture = SlackFixture::start(
        root.path(),
        &agent,
        app.origin(),
        SCRIPT_TOKEN,
        move |call| remote(&shared, call),
    )
    .await?;
    let remote_state = |read: &dyn Fn(&Remote) -> Value| -> Result<Value> {
        Ok(read(&*state.lock().map_err(|_| "remote")?))
    };
    let mut sequence = 0;
    fixture.launch().await?;
    let first = incoming(&mut fixture, &mut sequence, "Create file").await?;
    let turn = first["turn"].clone();
    settled(&mut fixture, "first document", |path| {
        Ok(match document(path, &turn)? {
            Some(document) => {
                status(path, &document["id"])? == "uncertain" && status(path, &turn)? == "sent"
            }
            None => false,
        })
    })
    .await?;
    let saved = document(&fixture.journal(), &turn)?.ok_or("document")?;
    ensure!(saved["stage"] == 2, "upload stage: {saved}");
    ensure!(
        status(&fixture.journal(), &saved["id"])? == "uncertain",
        "document status"
    );
    let first_calls = model.calls();
    ensure!(
        remote_state(&|remote| json!(remote.allocated.len()))? == 1,
        "allocations"
    );
    ensure!(
        remote_state(&|remote| json!(remote.uploaded.len()))? == 1,
        "uploads"
    );
    ensure!(
        remote_state(&|remote| json!(remote.uploaded[0] == TEXT.as_bytes()))? == true,
        "uploaded bytes"
    );
    ensure!(
        remote_state(&|remote| remote.allocated[0].clone())?
            == json!({"filename": "result.html", "length": TEXT.len()}),
        "allocation: {}",
        remote_state(&|remote| remote.allocated[0].clone())?
    );
    let completed = remote_state(&|remote| remote.completed[0].clone())?;
    ensure!(
        completed["channel_id"] == "DCHAT",
        "completion channel: {completed}"
    );
    ensure!(
        completed["thread_ts"] == first["root"],
        "completion thread: {completed}"
    );

    fixture.stop().await?;
    fixture.launch().await?;
    incoming(&mut fixture, &mut sequence, "!status").await?;
    ensure!(
        remote_state(&|remote| json!(remote.completed.len()))? == 1,
        "restart repeated the completion"
    );
    fixture.stop().await?;
    recover(&fixture, &saved["id"]).await?;
    grant(&app, &agent, 1, false).await?;
    fixture.launch().await?;
    let failure = fixture.expected_failure().await?;
    ensure!(failure.contains("403"), "revoked grant: {failure}");
    ensure!(
        remote_state(&|remote| json!(remote.completed.len()))? == 1,
        "revoked connector completed"
    );
    grant(&app, &agent, 2, true).await?;
    fixture.launch().await?;
    let id = saved["id"].clone();
    settled(&mut fixture, "recovered document", |path| {
        Ok(status(path, &id)? == "sent")
    })
    .await?;
    ensure!(
        remote_state(&|remote| json!(remote.completed.len()))? == 2,
        "recovered completion"
    );
    ensure!(
        remote_state(&|remote| json!(remote.completed[0] == remote.completed[1]))? == true,
        "retried completion differs"
    );
    ensure!(model.calls() == first_calls, "recovery ran the agent again");
    ensure!(
        remote_state(&|remote| json!(remote.uploaded.len()))? == 1,
        "recovery uploaded again"
    );
    ensure!(
        remote_state(&|remote| json!(remote.allocated.len()))? == 1,
        "recovery allocated again"
    );

    question.store(true, SeqCst);
    let partial = incoming(&mut fixture, &mut sequence, "Create file").await?;
    let partial_turn = partial["turn"].clone();
    settled(&mut fixture, "question", |path| {
        let db = Inspector::open(path)?;
        Ok(!db
            .rows(
                "SELECT 1 FROM slack_questions WHERE turn=?",
                [partial_turn.as_str().unwrap_or_default()],
            )?
            .is_empty())
    })
    .await?;
    app.post(
        &format!(
            "/api/threads/{}/turns/{}/cancel",
            partial["thread"].as_str().ok_or("thread")?,
            partial_turn.as_str().ok_or("turn")?
        ),
        json!({"id": new_id()}),
    )
    .await?;
    settled(&mut fixture, "partial document", |path| {
        Ok(match document(path, &partial_turn)? {
            Some(document) => {
                status(path, &document["id"])? == "sent" && status(path, &partial_turn)? == "sent"
            }
            None => false,
        })
    })
    .await?;
    let last = remote_state(&|remote| remote.completed.last().cloned().unwrap_or(Value::Null))?;
    ensure!(
        last["initial_comment"]
            .as_str()
            .is_some_and(|comment| comment.contains("Partial result")),
        "partial comment: {last}"
    );
    ensure!(
        remote_state(&|remote| json!(remote
            .uploaded
            .last()
            .is_some_and(|bytes| bytes == TEXT.as_bytes())))?
            == true,
        "partial bytes"
    );
    ensure!(
        app.get(&format!("/api/agents/{agent}/turns")).await? == json!([]),
        "direct chat used"
    );
    fixture.stop().await?;
    let partial_document =
        document(&fixture.journal(), &partial_turn)?.ok_or("partial document")?;
    {
        let db = minarbot_testkit::db::Fixture::open(fixture.journal())?;
        db.execute(
            "UPDATE slack_outbox SET status='uncertain' WHERE id=?",
            [partial_document["id"].as_str().unwrap_or_default()],
        )?;
        db.execute(
            "UPDATE slack_documents SET stage=2 WHERE id=?",
            [partial_document["id"].as_str().unwrap_or_default()],
        )?;
    }
    recover(&fixture, &partial_document["id"]).await?;
    let thread = partial["thread"].as_str().ok_or("thread")?;
    app.post(
        &format!("/api/threads/{thread}/edit"),
        json!({"id": new_id(), "revision": 1, "name": "Archived", "archived": true}),
    )
    .await?;
    app.post(
        &format!("/api/threads/{thread}/delete"),
        json!({"id": new_id(), "revision": 2}),
    )
    .await?;
    let count = remote_state(&|remote| json!(remote.completed.len()))?;
    fixture.launch().await?;
    let partial_id = partial_document["id"].clone();
    settled(&mut fixture, "revoked document", |path| {
        Ok(status(path, &partial_id)? == "revoked")
    })
    .await?;
    ensure!(
        remote_state(&|remote| json!(remote.completed.len()))? == count,
        "deleted thread completed again"
    );
    ensure!(
        remote_state(&|remote| json!(remote.authorization_ok))? == true,
        "Slack authorization headers"
    );
    fixture.stop().await?;
    Ok(())
}
