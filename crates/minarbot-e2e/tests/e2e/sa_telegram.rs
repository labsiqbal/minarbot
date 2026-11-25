//! Telegram connector: question buttons, offline recovery, the side-thread
//! bridge and the runner (port of `app/telegram.test.ts`).

use crate::{
    sa_support::{lock, RawProvider, Shared},
    support::{after_question, after_question_reply},
};
use minarbot_testkit::{
    connector::{
        self, connector_app, telegram_update, Connector, Fault, Platform, Reply as Answer,
    },
    model::{Reply, ScriptedModel},
    prelude::*,
    Server,
};
use std::{sync::Arc, time::Duration};

async fn script_switch(app: &Server, agent: &str, revision: u64, enabled: bool) -> Result<()> {
    app.post(
        &format!("/api/agents/{agent}/scripts"),
        json!({"id": new_id(), "revision": revision, "enabled": enabled}),
    )
    .await?;
    Ok(())
}

struct Question {
    id: String,
    turn: String,
    thread: String,
    message: i64,
    notice: Value,
}

#[tokio::test]
async fn telegram_question_buttons_answer_the_owning_task_once_across_restart_and_lost_responses(
) -> Result<()> {
    let choices: Vec<String> = (0..8)
        .map(|i| format!("{} {}{i}", i + 1, "Long choice ".repeat(10)))
        .collect();
    let model = ScriptedModel::start().await?;
    let answers: Shared<Vec<String>> = Shared::default();
    let free = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (seen, offered, script_free) = (answers.clone(), choices.clone(), free.clone());
    model.respond(move |request| {
        if after_question(request) {
            return after_question_reply();
        }
        let answer = request
            .messages()
            .iter()
            .rev()
            .find(|message| message["role"] == "tool")
            .and_then(|message| serde_json::from_str::<Value>(message["content"].as_str()?).ok())
            .and_then(|result| result["answer"].as_str().map(str::to_owned));
        match answer {
            Some(answer) => {
                if let Ok(mut seen) = seen.lock() {
                    seen.push(answer.clone());
                }
                Reply::text(format!("Answered {answer}"))
            }
            None => {
                let choices = if script_free.load(std::sync::atomic::Ordering::SeqCst) {
                    json!([])
                } else {
                    json!(offered)
                };
                Reply::tool_call(
                    "question",
                    "ask_user",
                    json!({"question": "Choose", "choices": choices}),
                )
            }
        }
    });
    let root = TestRoot::new()?;
    let agent = new_id();
    let f = connector_app(&root, &agent, ServerConfig::new().model(&model)).await?;
    let connector = Connector::new(
        Platform::Telegram,
        root.path(),
        &agent,
        f.app.origin(),
        None,
        vec![],
        vec![json!(4), json!(5)],
    )
    .await?;
    let db = connector.open_journal()?;
    let sent = |c: &Connector| c.json_posts("sendMessage");
    let answer_requests = |c: &Connector| -> Vec<connector::ScriptRequest> {
        c.requests()
            .into_iter()
            .filter(|request| request.method == "POST" && request.path.ends_with("/answer"))
            .collect()
    };
    let callback = |id: &str| -> Result<Option<Value>> {
        Ok(db
            .rows("SELECT * FROM telegram_callbacks WHERE notice=?", [id])?
            .into_iter()
            .next())
    };
    let mut sequence = 0_i64;
    let mut next = || {
        sequence += 1;
        sequence
    };
    let click = |n: i64, question: &Question, index: usize, change: Value| {
        let mut query = json!({"id": format!("query-{n}"), "data": format!("q:{}:{index}", question.id),
            "from": {"id": 4, "is_bot": false},
            "message": {"message_id": question.message, "chat": {"id": 4, "type": "private"},
                "from": {"id": 123, "is_bot": true}}});
        if let (Some(query), Some(change)) = (query.as_object_mut(), change.as_object()) {
            query.extend(change.clone());
        }
        json!({"update_id": n, "callback_query": query})
    };
    async fn ask(
        connector: &Connector,
        db: &minarbot_testkit::db::Fixture,
        n: i64,
    ) -> Result<Question> {
        connector
            .send(telegram_update(n, &format!("Ask {n}")))
            .await?;
        let row = connector
            .until("question notice sent", || {
                Ok(db
                    .rows(
                        "SELECT q.*,o.message_ids FROM telegram_questions q JOIN telegram_inbox i ON i.turn=q.turn \
                         JOIN telegram_outbox o ON o.id=q.id WHERE i.update_id=? AND o.status='sent'",
                        [n],
                    )?
                    .into_iter()
                    .next())
            })
            .await?;
        let id = row["id"].as_str().ok_or("question id")?.to_owned();
        let ids: Value = serde_json::from_str(row["message_ids"].as_str().unwrap_or("[]"))?;
        let notice = connector
            .json_posts("sendMessage")
            .into_iter()
            .find(|post| post["text"].as_str().is_some_and(|text| text.contains(&id)))
            .ok_or("question notice missing")?;
        Ok(Question {
            id,
            turn: row["turn"].as_str().unwrap_or_default().into(),
            thread: row["thread"].as_str().unwrap_or_default().into(),
            message: ids
                .as_array()
                .and_then(|ids| ids.last())
                .and_then(Value::as_i64)
                .unwrap_or_default(),
            notice,
        })
    }
    connector.start()?;
    let q = ask(&connector, &db, next()).await?;
    let keyboard = q.notice["reply_markup"]["inline_keyboard"].clone();
    ensure!(
        keyboard.as_array().map(Vec::len) == Some(8),
        "keyboard: {keyboard}"
    );
    ensure!(
        q.notice["text"]
            .as_str()
            .is_some_and(|text| text.contains(&choices[7])),
        "notice text: {}",
        q.notice["text"]
    );
    ensure!(
        keyboard[7][0]["text"]
            .as_str()
            .is_some_and(|text| text.ends_with("...")),
        "button text: {}",
        keyboard[7][0]["text"]
    );
    ensure!(
        keyboard[7][0]["callback_data"]
            .as_str()
            .is_some_and(|data| data.len() <= 64),
        "callback data: {}",
        keyboard[7][0]["callback_data"]
    );
    connector.send(click(next(), &q, 8, json!({}))).await?;
    connector
        .send(click(
            next(),
            &q,
            0,
            json!({"from": {"id": 5, "is_bot": false}}),
        ))
        .await?;
    connector
        .send(click(
            next(),
            &q,
            0,
            json!({"data": format!("a:{}:y", q.id)}),
        ))
        .await?;
    connector
        .send(click(
            next(),
            &q,
            0,
            json!({"message": {"message_id": q.message + 1, "chat": {"id": 4, "type": "private"},
                "from": {"id": 123, "is_bot": true}}}),
        ))
        .await?;
    ensure!(
        answer_requests(&connector).is_empty(),
        "invalid clicks answered"
    );
    ensure!(callback(&q.id)?.is_none(), "invalid click stored");
    connector.add_fault(Fault {
        method: Some("POST".into()),
        suffix: "/answer".into(),
        after: true,
    });
    connector.send(click(next(), &q, 7, json!({}))).await?;
    ensure!(
        callback(&q.id)?.is_some_and(|row| row["selection"] == 7),
        "selection not stored"
    );
    connector
        .expected_failure("Local agent request failed")
        .await?;
    connector.start()?;
    connector.send(click(next(), &q, 0, json!({}))).await?;
    let expected = format!("Answered {}", choices[7]);
    connector
        .until("answer reply", || {
            Ok(sent(&connector)
                .iter()
                .any(|post| post["text"] == expected.as_str())
                .then_some(()))
        })
        .await?;
    ensure!(
        *lock(&answers)? == [choices[7].clone()],
        "model answers: {:?}",
        lock(&answers)?
    );
    let posted = answer_requests(&connector);
    ensure!(posted.len() == 2, "answer posts: {}", posted.len());
    ensure!(
        posted[0].bytes == posted[1].bytes && posted[0].path == posted[1].path,
        "answer retry differs"
    );
    let body: Value = serde_json::from_slice(&posted[0].bytes)?;
    ensure!(body["text"] == choices[7].as_str(), "answer body: {body}");
    ensure!(
        posted[0]
            .path
            .contains(&format!("/threads/{}/turns/{}/", q.thread, q.turn)),
        "answer path: {}",
        posted[0].path
    );
    connector.send(click(next(), &q, 7, json!({}))).await?;
    ensure!(
        answer_requests(&connector).len() == 2,
        "repeated click answered again"
    );

    let revoked = ask(&connector, &db, next()).await?;
    script_switch(&f.app, &agent, 1, false).await?;
    connector
        .send(click(next(), &revoked, 1, json!({})))
        .await?;
    connector.expected_failure("403").await?;
    ensure!(
        callback(&revoked.id)?.is_some_and(|row| row["status"] == "queued"),
        "revoked callback: {:?}",
        callback(&revoked.id)?
    );
    ensure!(
        lock(&answers)?.len() == 1,
        "revoked answer reached the model"
    );
    script_switch(&f.app, &agent, 2, true).await?;
    connector.start()?;
    connector
        .until("queued answer delivered", || {
            Ok((lock(&answers)?.len() == 2).then_some(()))
        })
        .await?;
    ensure!(
        lock(&answers)?[1] == choices[1],
        "second answer: {:?}",
        lock(&answers)?
    );
    let cancelled = ask(&connector, &db, next()).await?;
    f.app
        .post(
            &format!(
                "/api/threads/{}/turns/{}/cancel",
                cancelled.thread, cancelled.turn
            ),
            json!({"id": new_id()}),
        )
        .await?;
    connector
        .send(click(next(), &cancelled, 2, json!({})))
        .await?;
    connector
        .until("cancelled answer notice", || {
            Ok(sent(&connector)
                .iter()
                .any(|post| {
                    post["text"]
                        .as_str()
                        .is_some_and(|t| t.contains("Answer could not be applied"))
                })
                .then_some(()))
        })
        .await?;
    free.store(true, std::sync::atomic::Ordering::SeqCst);
    let custom = ask(&connector, &db, next()).await?;
    ensure!(
        custom.notice.get("reply_markup").is_none(),
        "free question has buttons: {}",
        custom.notice
    );
    connector.send(click(next(), &custom, 0, json!({}))).await?;
    ensure!(
        callback(&custom.id)?.is_none(),
        "free question click stored"
    );
    connector.stop().await?;
    Ok(())
}

#[tokio::test]
async fn telegram_offline_recovery_preserves_sent_parts_and_rejects_repeated_operator_decisions(
) -> Result<()> {
    let root = TestRoot::new()?;
    let calls: Shared<Vec<Value>> = Shared::default();
    let lost = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (log, losing) = (calls.clone(), lost.clone());
    let remote: connector::Remote = Arc::new(move |post| {
        let (log, losing) = (log.clone(), losing.clone());
        Box::pin(async move {
            let count = {
                let Ok(mut log) = log.lock() else {
                    return Answer::Lose;
                };
                log.push(serde_json::from_slice(&post.raw).unwrap_or(Value::Null));
                log.len()
            };
            if count > 1 && losing.load(std::sync::atomic::Ordering::SeqCst) {
                return Answer::Lose;
            }
            Answer::Value(json!({"message_id": count, "chat": {"id": 4}}))
        })
    });
    let agent = new_id();
    let _f = connector_app(&root, &agent, ServerConfig::new()).await?;
    let connector = Connector::new(
        Platform::Telegram,
        root.path(),
        &agent,
        _f.app.origin(),
        Some(remote),
        vec![],
        vec![json!(4), json!(5)],
    )
    .await?;
    let command =
        |command: &str, id: &str, revision: i64, action: &str, confirmation: Option<&str>| {
            connector.journal(
                command,
                Some(id),
                Some(revision),
                Some(action),
                confirmation,
            )
        };
    let inspect = |id: &str| -> Result<Value> {
        connector
            .journal("inspect", Some(id), None, None, None)?
            .json()
    };
    let db = connector.open_journal()?;
    let add = |id: &str, text: &str| {
        db.execute(
            "INSERT INTO telegram_outbox(id,chat,reply_to,text,status) VALUES(?,4,2,?,'queued')",
            [id, text],
        )
    };
    let settle = |id: String, status: &'static str| {
        let connector = &connector;
        let db = &db;
        async move {
            connector.start()?;
            connector
                .until(&format!("reply {status}"), || {
                    Ok(db
                        .rows("SELECT status FROM telegram_outbox WHERE id=?", [&id])?
                        .first()
                        .is_some_and(|row| row["status"] == status)
                        .then_some(()))
                })
                .await?;
            connector.stop().await
        }
    };
    let id = new_id();
    add(&id, &format!("{}tail", "a".repeat(4096)))?;
    settle(id.clone(), "uncertain").await?;
    let inspected = inspect(&id)?;
    ensure!(
        inspected["reply"]["status"] == "uncertain",
        "inspect: {inspected}"
    );
    ensure!(inspected["reply"]["part"] == 1, "part: {inspected}");
    ensure!(inspected["revision"] == 0, "revision: {inspected}");
    ensure!(
        command("recover", &id, 0, "retry", None)?.status == 1,
        "retry without risk flag"
    );
    ensure!(
        command("recover", &id, 1, "retry", Some("--accept-duplicate-risk"))?.status == 1,
        "stale revision accepted"
    );
    ensure!(
        command("recover", &id, 0, "retry", Some("--accept-duplicate-risk"))?.status == 0,
        "retry rejected"
    );
    settle(id.clone(), "uncertain").await?;
    {
        let calls = lock(&calls)?;
        ensure!(calls.len() == 3, "platform calls: {}", calls.len());
        ensure!(calls[2]["text"] == "tail", "resent part: {}", calls[2]);
    }
    ensure!(
        command("recover", &id, 0, "retry", Some("--accept-duplicate-risk"))?.status == 1,
        "repeated decision accepted"
    );
    let inspected = inspect(&id)?;
    ensure!(inspected["revision"] == 1, "revision: {inspected}");
    ensure!(
        inspected["decisions"][0]["status"] == "uncertain",
        "decision: {inspected}"
    );
    ensure!(
        inspected["decisions"][0]["part"] == 1,
        "decision part: {inspected}"
    );
    lost.store(false, std::sync::atomic::Ordering::SeqCst);
    ensure!(
        command("recover", &id, 1, "retry", Some("--accept-duplicate-risk"))?.status == 0,
        "second retry rejected"
    );
    settle(id.clone(), "sent").await?;
    let inspected = inspect(&id)?;
    ensure!(inspected["reply"]["status"] == "sent", "sent: {inspected}");
    ensure!(
        inspected["reply"]["message_ids"] == "[1,4]",
        "message ids: {inspected}"
    );
    ensure!(lock(&calls)?[3]["text"] == "tail", "final part");
    ensure!(
        command("recover", &id, 2, "retry", Some("--accept-duplicate-risk"))?.status == 1,
        "retry after sent accepted"
    );
    let rejected = new_id();
    add(&rejected, "Rejected")?;
    db.execute(
        "UPDATE telegram_outbox SET status='failed',error='Rejected' WHERE id=?",
        [&rejected],
    )?;
    ensure!(
        command("recover", &rejected, 0, "discard", None)?.status == 1,
        "discard without confirm"
    );
    ensure!(
        command("recover", &rejected, 0, "discard", Some("--confirm"))?.status == 0,
        "discard rejected"
    );
    let inspected = inspect(&rejected)?;
    ensure!(
        inspected["reply"]["status"] == "discarded",
        "discarded: {inspected}"
    );
    ensure!(
        inspected["decisions"][0]["error"] == "Rejected",
        "decision error: {inspected}"
    );
    connector.start()?;
    connector.send(telegram_update(1, "/status")).await?;
    connector
        .until("status probe", || {
            Ok(db
                .rows(
                    "SELECT 1 AS found FROM telegram_outbox WHERE id<>? AND id<>? AND status='sent'",
                    [&id, &rejected],
                )?
                .into_iter()
                .next())
        })
        .await?;
    connector.stop().await?;
    ensure!(
        !lock(&calls)?.iter().any(|call| call["text"] == "Rejected"),
        "discarded reply was sent"
    );
    ensure!(
        command(
            "recover",
            &rejected,
            1,
            "retry",
            Some("--accept-duplicate-risk")
        )?
        .status
            == 1,
        "retry after discard accepted"
    );
    let interrupted = new_id();
    add(&interrupted, "Interrupted")?;
    db.execute(
        "UPDATE telegram_outbox SET status='sending' WHERE id=?",
        [&interrupted],
    )?;
    let inspected = inspect(&interrupted)?;
    ensure!(
        inspected["reply"]["status"] == "uncertain",
        "interrupted: {inspected}"
    );
    ensure!(
        inspected["revision"] == 0,
        "interrupted revision: {inspected}"
    );
    ensure!(
        command("recover", &interrupted, 0, "discard", Some("--confirm"))?.status == 0,
        "discard interrupted"
    );
    Ok(())
}

#[tokio::test]
async fn telegram_bridge_submits_once_through_granted_side_thread_api_and_isolates_main_history(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|_| Reply::text("Synthetic reply"));
    let root = TestRoot::new()?;
    let agent = new_id();
    let f = connector_app(&root, &agent, ServerConfig::new().model(&model)).await?;
    let parts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (counter, origin, owner, http) = (
        parts.clone(),
        f.app.origin().to_owned(),
        agent.clone(),
        f.app.client()?,
    );
    let remote: connector::Remote = Arc::new(move |post| {
        let (counter, origin, owner, client) =
            (counter.clone(), origin.clone(), owner.clone(), http.clone());
        Box::pin(async move {
            let body: Value = serde_json::from_slice(&post.raw).unwrap_or(Value::Null);
            if post.method != "sendMessage"
                || !body["text"].as_str().is_some_and(|t| t.starts_with("xxxx"))
            {
                return Answer::Default;
            }
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Revoke the script grant while the first part is being sent.
            let _ = client
                .post(format!("{origin}/api/agents/{owner}/scripts"))
                .header("origin", &origin)
                .header("content-type", "application/json")
                .body(json!({"id": new_id(), "revision": 1, "enabled": false}).to_string())
                .send()
                .await;
            Answer::Value(json!({"message_id": 100, "chat": {"id": 4}}))
        })
    });
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
    let mut update = telegram_update(1, "Hello");
    update["message"]["message_id"] = json!(2);
    let replies = |c: &Connector| {
        c.json_posts("sendMessage")
            .into_iter()
            .filter(|post| post["text"] == "Synthetic reply")
            .count()
    };
    let turns = |c: &Connector| -> Vec<connector::ScriptRequest> {
        c.requests()
            .into_iter()
            .filter(|request| request.method == "POST" && request.path.ends_with("/turns"))
            .collect()
    };
    connector.add_fault(Fault {
        method: Some("POST".into()),
        suffix: "/turns".into(),
        after: true,
    });
    connector.start()?;
    connector.send(update.clone()).await?;
    connector
        .expected_failure("Local agent request failed")
        .await?;
    connector.start()?;
    connector
        .until("Telegram reply delivered", || {
            Ok((replies(&connector) == 1).then_some(()))
        })
        .await?;
    ensure!(model.calls() == 1, "model calls: {}", model.calls());
    let posted = turns(&connector);
    ensure!(posted.len() == 2, "turn posts: {}", posted.len());
    ensure!(
        posted[0].bytes == posted[1].bytes && posted[0].path == posted[1].path,
        "turn retry differs"
    );
    ensure!(
        f.app.get(&format!("/api/agents/{agent}/turns")).await? == json!([]),
        "bridge turn entered main history"
    );
    connector.stop().await?;
    connector.start()?;
    connector.push(update).await?;
    connector.send(telegram_update(3, "/status")).await?;
    connector
        .until("status probe", || {
            Ok(db
                .rows(
                    "SELECT 1 AS found FROM telegram_outbox WHERE reply_to=3 AND text LIKE 'No active tasks%' AND status='sent'",
                    [],
                )?
                .into_iter()
                .next())
        })
        .await?;
    ensure!(replies(&connector) == 1, "duplicate update replied");
    ensure!(model.calls() == 1, "duplicate update reached the model");
    let pending = new_id();
    db.execute(
        "INSERT INTO telegram_outbox(id,chat,reply_to,text,status) VALUES(?,4,2,?,'queued')",
        [pending.clone(), "x".repeat(4097)],
    )?;
    connector.expected_failure("403").await?;
    ensure!(
        parts.load(std::sync::atomic::Ordering::SeqCst) == 1,
        "parts sent"
    );
    let paused = db.row(
        "SELECT status,part FROM telegram_outbox WHERE id=?",
        [&pending],
    )?;
    ensure!(paused["status"] == "queued", "paused reply: {paused}");
    ensure!(paused["part"] == 1, "paused part: {paused}");
    connector.start()?;
    connector.expected_failure("403").await?;
    ensure!(replies(&connector) == 1, "replies after revoke");
    ensure!(
        parts.load(std::sync::atomic::Ordering::SeqCst) == 1,
        "revoked reply resumed"
    );
    Ok(())
}

#[tokio::test]
async fn rust_telegram_runner_rejects_a_mismatched_bot_identity_and_releases_its_journal_lock(
) -> Result<()> {
    let server = RawProvider::start(|_| {
        (
            200,
            vec![("content-type".into(), "application/json".into())],
            json!({"ok": true, "result": {"id": 124, "is_bot": true}})
                .to_string()
                .into_bytes(),
        )
    })
    .await?;
    let root = TestRoot::new()?;
    let directory = root.path().join("mismatch");
    let options = json!({"platform": "telegram", "directory": directory,
        "scriptDirectory": root.path().join("absent"), "agent": new_id(),
        "token": connector::TELEGRAM_TOKEN, "users": [4], "origin": server.origin()});
    let result = connector::run(&options, &root.path().join("logs"), std::future::pending()).await;
    let error = result
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    ensure!(error.contains("identity mismatch"), "runner error: {error}");
    ensure!(!directory.join("owner.lock").exists(), "journal lock kept");
    Ok(())
}

#[derive(Default)]
struct Runner {
    offsets: Vec<i64>,
    delivered: usize,
    answer: Option<Value>,
    errors: Vec<String>,
}

async fn telegram_runner_releases_owned_journal_on_shutdown(ask: bool) -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(move |request| {
        if after_question(request) {
            return after_question_reply();
        }
        if ask
            && !request
                .messages()
                .iter()
                .any(|message| message["role"] == "tool")
        {
            Reply::tool_call(
                "question",
                "ask_user",
                json!({"question": "Proceed?", "choices": ["Yes", "No"]}),
            )
        } else {
            Reply::text("Runner reply")
        }
    });
    let state: Shared<Runner> = Shared::default();
    let done = Arc::new(tokio::sync::Notify::new());
    let (script, finished) = (state.clone(), done.clone());
    let telegram = RawProvider::start(move |request| {
        let Ok(mut state) = script.lock() else {
            return (500, vec![], Vec::new());
        };
        let body = request.json();
        let result = if request.path.ends_with("/getMe") {
            json!({"id": 123, "is_bot": true})
        } else if request.path.ends_with("/getUpdates") {
            let offset = body["offset"].as_i64().unwrap_or(0);
            state.offsets.push(offset);
            if offset < 8 {
                json!([{"update_id": 7, "message": {"message_id": 8, "chat": {"id": 4, "type": "private"},
                    "from": {"id": 4, "is_bot": false}, "text": "Runner request"}}])
            } else if offset < 9 {
                state.answer.clone().map_or(json!([]), |answer| json!([answer]))
            } else {
                json!([])
            }
        } else {
            if body["chat_id"] != 4 {
                state.errors.push(format!("chat_id: {body}"));
            }
            state.delivered += 1;
            let text = body["text"].as_str().unwrap_or_default();
            if let Some(index) = text.find("/answer ") {
                let id: String = text[index + 8..].chars().take(36).collect();
                state.answer = Some(json!({"update_id": 8, "message": {"message_id": 10,
                    "chat": {"id": 4, "type": "private"}, "from": {"id": 4, "is_bot": false},
                    "text": format!("/answer {id} Yes")}}));
            } else if text == "Runner reply" {
                finished.notify_one();
            } else if text != "Answer accepted." {
                state.errors.push(format!("unexpected message: {text}"));
            }
            json!({"message_id": 20 + state.delivered, "chat": {"id": 4}})
        };
        (
            200,
            vec![("content-type".into(), "application/json".into())],
            json!({"ok": true, "result": result}).to_string().into_bytes(),
        )
    })
    .await?;
    let root = TestRoot::new()?;
    let scripts = root.path().join("scripts");
    let app = root
        .start(ServerConfig::new().model(&model).script_directory(&scripts))
        .await?;
    let agent = new_id();
    app.post(
        "/api/agents",
        json!({"runtime": "api", "id": agent, "name": "Runner", "model": "fixture", "instructions": "Help"}),
    )
    .await?;
    script_switch(&app, &agent, 0, true).await?;
    let options = json!({"platform": "telegram", "directory": root.path().join("state"),
        "scriptDirectory": scripts, "agent": agent, "token": connector::TELEGRAM_TOKEN,
        "users": [4], "origin": telegram.origin()});
    let stop = async {
        tokio::select! {
            () = done.notified() => tokio::time::sleep(Duration::from_millis(30)).await,
            () = tokio::time::sleep(Duration::from_secs(10)) => {}
        }
    };
    connector::run(&options, &root.path().join("logs"), stop).await?;
    let state = lock(&state)?;
    ensure!(state.errors.is_empty(), "telegram fake: {:?}", state.errors);
    let work = crate::support::work_calls(&model);
    ensure!(work == if ask { 2 } else { 1 }, "model calls: {work}");
    ensure!(
        state.delivered == if ask { 3 } else { 1 },
        "delivered: {}",
        state.delivered
    );
    ensure!(state.offsets.contains(&8), "offsets: {:?}", state.offsets);
    ensure!(
        !root.path().join("state/owner.lock").exists(),
        "journal lock kept"
    );
    let journal = minarbot_testkit::db::Inspector::open(root.path().join("state/telegram.sqlite"))?;
    ensure!(
        journal.scalar::<String>("SELECT status FROM telegram_outbox", [])? == "sent",
        "journal status"
    );
    Ok(())
}

#[tokio::test]
async fn telegram_runner_replies_directly_and_releases_owned_journal_on_shutdown() -> Result<()> {
    telegram_runner_releases_owned_journal_on_shutdown(false).await
}

#[tokio::test]
async fn telegram_runner_answers_a_question_and_releases_owned_journal_on_shutdown() -> Result<()> {
    telegram_runner_releases_owned_journal_on_shutdown(true).await
}
