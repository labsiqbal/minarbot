//! Telegram generated-artifact delivery (port of `app/telegram-output.test.ts`).

use crate::sa_support::{lock, Shared};
use minarbot_testkit::{
    chat_connector::{self, connector_app, telegram_update, Connector, Platform, Reply as Answer},
    model::{ModelRequest, Reply, ScriptedModel},
    prelude::*,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// The server's one-time done check after a deliverable (H2 in the ledger).
fn reminder(request: &ModelRequest) -> bool {
    request
        .last_user_text()
        .starts_with("One verification reminder")
}

/// One multipart field: name, file name, content type, bytes.
type Part = (String, Option<String>, Option<String>, Vec<u8>);

fn multipart(content_type: &str, raw: &[u8]) -> Vec<Part> {
    let Some(boundary) = content_type.split("boundary=").nth(1) else {
        return Vec::new();
    };
    let delimiter = format!("--{}", boundary.trim_matches('"'));
    let text = raw.iter().map(|byte| char::from(*byte)).collect::<String>();
    text.split(&delimiter)
        .filter_map(|part| {
            let (head, body) = part.split_once("\r\n\r\n")?;
            let body = body.strip_suffix("\r\n").unwrap_or(body);
            let field = |key: &str| {
                head.split(&format!("{key}=\""))
                    .nth(1)
                    .and_then(|rest| rest.split('"').next())
                    .map(str::to_owned)
            };
            let kind = head
                .lines()
                .find_map(|line| {
                    line.strip_prefix("Content-Type:")
                        .or_else(|| line.strip_prefix("content-type:"))
                })
                .map(|value| value.trim().to_owned());
            Some((
                field(" name")?,
                field("filename"),
                kind,
                body.chars().map(|c| c as u8).collect(),
            ))
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[tokio::test]
async fn telegram_sends_exact_generated_artifacts_retains_uncertain_delivery_and_rechecks_grant_before_recovery(
) -> Result<()> {
    let text = "<h1>Exact generated output</h1>\n";
    let model = ScriptedModel::start().await?;
    let question = Arc::new(AtomicBool::new(false));
    let asking = question.clone();
    model.respond(move |request| {
        if reminder(request) {
            return Reply::text("File ready");
        }
        let calls = crate::sa_support::turn_tool_calls(request);
        if calls.is_empty() {
            return Reply::tool_call(
                "create_text_file",
                "create_text_file",
                json!({"name": "result.html", "text": text}),
            );
        }
        if asking.load(Ordering::SeqCst) && !calls.iter().any(|name| name == "ask_user") {
            return Reply::tool_call(
                "ask_user",
                "ask_user",
                json!({"question": "Continue?", "choices": ["Yes", "No"]}),
            );
        }
        Reply::text("File ready")
    });
    // Model steps, the done-check reminder included (it was a step in the
    // API runtime too).
    let work = |model: &ScriptedModel| model.calls();
    let deliveries: Shared<Vec<Value>> = Shared::default();
    let messages: Shared<Vec<Value>> = Shared::default();
    let lost = Arc::new(AtomicBool::new(true));
    let (documents, texts, losing) = (deliveries.clone(), messages.clone(), lost.clone());
    let remote: chat_connector::Remote = Arc::new(move |post| {
        let (documents, texts, losing) = (documents.clone(), texts.clone(), losing.clone());
        Box::pin(async move {
            if post.method == "sendDocument" {
                let parts = multipart(post.content_type.as_deref().unwrap_or_default(), &post.raw);
                let field = |name: &str| {
                    parts
                        .iter()
                        .find(|part| part.0 == name)
                        .map(|part| String::from_utf8_lossy(&part.3).into_owned())
                };
                let document = parts.iter().find(|part| part.0 == "document");
                let delivery = json!({"chat": field("chat_id"),
                    "reply": field("reply_parameters").and_then(|v| serde_json::from_str::<Value>(&v).ok()),
                    "name": document.and_then(|d| d.1.clone()), "type": document.and_then(|d| d.2.clone()),
                    "bytes": document.map(|d| hex(&d.3)), "detect": field("disable_content_type_detection"),
                    "caption": field("caption")});
                let count = {
                    let Ok(mut documents) = documents.lock() else {
                        return Answer::Lose;
                    };
                    documents.push(delivery);
                    documents.len()
                };
                if losing.swap(false, Ordering::SeqCst) {
                    return Answer::Lose;
                }
                return Answer::Value(json!({"message_id": 100 + count, "chat": {"id": 4}}));
            }
            if post.method == "sendMessage" {
                if let Ok(mut texts) = texts.lock() {
                    texts.push(serde_json::from_slice(&post.raw).unwrap_or(Value::Null));
                }
            }
            Answer::Default
        })
    });
    let root = TestRoot::new()?;
    let agent = new_id();
    let f = connector_app(
        &root,
        &agent,
        ServerConfig::new()
            .model(&model)
            .script_directory(root.path().join("scripts")),
    )
    .await?;
    let connector = Connector::new(
        Platform::Telegram,
        root.path(),
        &agent,
        f.app.origin(),
        Some(remote),
        vec![],
        vec![json!(4), json!(5)],
    )
    .await?;
    let db = connector.open_journal()?;
    let state = |id: &str| -> Result<Option<String>> {
        Ok(db
            .rows("SELECT status FROM telegram_outbox WHERE id=?", [id])?
            .first()
            .and_then(|row| row["status"].as_str().map(str::to_owned)))
    };
    let document_of = |turn: &str| -> Result<Option<String>> {
        Ok(db
            .rows("SELECT id FROM telegram_documents WHERE turn=?", [turn])?
            .first()
            .and_then(|row| row["id"].as_str().map(str::to_owned)))
    };
    let recover = |id: &str| -> Result<()> {
        let output = connector.journal(
            "recover",
            Some(id),
            Some(0),
            Some("retry"),
            Some("--accept-duplicate-risk"),
        )?;
        ensure!(output.status == 0, "recover {id}: {}", output.stderr);
        Ok(())
    };
    let mut sequence = 0;
    let mut next = || {
        sequence += 1;
        sequence
    };
    let latest_inbox = || {
        db.row(
            "SELECT * FROM telegram_inbox ORDER BY rowid DESC LIMIT 1",
            [],
        )
    };
    connector.start()?;
    connector
        .send(telegram_update(next(), "Create file"))
        .await?;
    let first = latest_inbox()?;
    let first_turn = first["turn"].as_str().ok_or("turn")?.to_owned();
    connector
        .until("first document uncertain", || {
            Ok(match document_of(&first_turn)? {
                Some(document) => (state(&document)?.as_deref() == Some("uncertain")
                    && state(&first_turn)?.as_deref() == Some("sent"))
                .then_some(()),
                None => None,
            })
        })
        .await?;
    ensure!(work(&model) == 3, "model calls: {}", work(&model));
    {
        let deliveries = lock(&deliveries)?;
        ensure!(deliveries.len() == 1, "deliveries: {deliveries:?}");
        let delivery = &deliveries[0];
        ensure!(
            delivery["bytes"] == hex(text.as_bytes()),
            "bytes: {delivery}"
        );
        ensure!(delivery["name"] == "result.html", "name: {delivery}");
        ensure!(delivery["chat"] == "4", "chat: {delivery}");
        ensure!(
            delivery["reply"] == json!({"message_id": 1, "allow_sending_without_reply": false}),
            "reply: {delivery}"
        );
        ensure!(
            delivery["type"] == "application/octet-stream",
            "type: {delivery}"
        );
        ensure!(delivery["detect"] == "true", "detect: {delivery}");
    }
    ensure!(
        lock(&messages)?[0]["text"] == "File ready",
        "message: {:?}",
        lock(&messages)?
    );
    let document = document_of(&first_turn)?.ok_or("document")?;
    ensure!(
        state(&document)?.as_deref() == Some("uncertain"),
        "document state"
    );
    connector.stop().await?;
    connector.start()?;
    connector.send(telegram_update(next(), "/status")).await?;
    ensure!(lock(&deliveries)?.len() == 1, "uncertain document resent");
    connector.stop().await?;
    recover(&document)?;
    f.app
        .post(
            &format!("/api/agents/{agent}/scripts"),
            json!({"id": new_id(), "revision": 1, "enabled": false}),
        )
        .await?;
    connector.start()?;
    connector.expected_failure("403").await?;
    ensure!(lock(&deliveries)?.len() == 1, "revoked recovery resent");
    f.app
        .post(
            &format!("/api/agents/{agent}/scripts"),
            json!({"id": new_id(), "revision": 2, "enabled": true}),
        )
        .await?;
    connector.start()?;
    connector
        .until("recovered document sent", || {
            Ok((state(&document)?.as_deref() == Some("sent")).then_some(()))
        })
        .await?;
    {
        let deliveries = lock(&deliveries)?;
        ensure!(deliveries.len() == 2, "deliveries: {}", deliveries.len());
        ensure!(
            deliveries[0] == deliveries[1],
            "recovery changed the document"
        );
    }
    ensure!(work(&model) == 3, "recovery called the model");
    question.store(true, Ordering::SeqCst);
    connector
        .send(telegram_update(next(), "Create file"))
        .await?;
    let partial = latest_inbox()?;
    let (turn, thread) = (
        partial["turn"].as_str().ok_or("turn")?.to_owned(),
        partial["thread"].as_str().ok_or("thread")?.to_owned(),
    );
    connector
        .until("question stored", || {
            Ok(db
                .rows(
                    "SELECT 1 AS found FROM telegram_questions WHERE turn=?",
                    [&turn],
                )?
                .into_iter()
                .next())
        })
        .await?;
    f.app
        .post(
            &format!("/api/threads/{thread}/turns/{turn}/cancel"),
            json!({"id": new_id()}),
        )
        .await?;
    connector
        .until("partial document sent", || {
            Ok(match document_of(&turn)? {
                Some(document) => (state(&document)?.as_deref() == Some("sent")
                    && state(&turn)?.as_deref() == Some("sent"))
                .then_some(()),
                None => None,
            })
        })
        .await?;
    {
        let deliveries = lock(&deliveries)?;
        ensure!(deliveries.len() == 3, "deliveries: {}", deliveries.len());
        ensure!(
            deliveries[2]["caption"]
                .as_str()
                .is_some_and(|caption| caption.contains("Partial result")),
            "caption: {}",
            deliveries[2]
        );
    }
    ensure!(
        lock(&messages)?.iter().any(|message| message["text"]
            .as_str()
            .is_some_and(|t| t.contains("partial result"))),
        "partial notice missing"
    );
    connector.stop().await?;
    let partial_document = document_of(&turn)?.ok_or("partial document")?;
    db.execute(
        "UPDATE telegram_outbox SET status='uncertain',part=0,message_ids='[]' WHERE id=?",
        [&partial_document],
    )?;
    recover(&partial_document)?;
    f.app
        .post(
            &format!("/api/threads/{thread}/edit"),
            json!({"id": new_id(), "revision": 1, "name": "Archived", "archived": true}),
        )
        .await?;
    f.app
        .post(
            &format!("/api/threads/{thread}/delete"),
            json!({"id": new_id(), "revision": 2}),
        )
        .await?;
    let count = lock(&deliveries)?.len();
    connector.start()?;
    connector
        .until("deleted source revoked", || {
            Ok((state(&partial_document)?.as_deref() == Some("revoked")).then_some(()))
        })
        .await?;
    ensure!(lock(&deliveries)?.len() == count, "revoked document sent");
    connector.stop().await?;
    Ok(())
}
