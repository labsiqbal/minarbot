//! Slack connector: question answers, Socket Mode, status and stop commands,
//! offline recovery, cooldowns, interactive decisions and shared channels
//! (port of `app/slack.test.ts`).

use crate::{
    sa_support::{lock, RawProvider, Shared},
    support::{after_question, after_question_reply, work_calls},
};
use minarbot_testkit::{
    connector::{
        self, connector_app, slack_envelope, Connector, Fault, Platform, Reply as Answer,
        SocketScript,
    },
    db::Fixture,
    model::{Reply, ScriptedModel},
    prelude::*,
    Server,
};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::Notify;

fn users() -> Vec<Value> {
    vec![json!("UUSER"), json!("UOTHER")]
}

async fn slack(
    root: &TestRoot,
    agent: &str,
    config: ServerConfig,
    remote: Option<connector::Remote>,
    channels: Vec<String>,
) -> Result<(Server, Connector)> {
    let app = connector_app(root, agent, config).await?.app;
    let connector = Connector::new(
        Platform::Slack,
        root.path(),
        agent,
        app.origin(),
        remote,
        channels,
        users(),
    )
    .await?;
    Ok((app, connector))
}

async fn scripts(app: &Server, agent: &str, revision: u64, enabled: bool) -> Result<()> {
    app.post(
        &format!("/api/agents/{agent}/scripts"),
        json!({"id": new_id(), "revision": revision, "enabled": enabled}),
    )
    .await?;
    Ok(())
}

fn row(db: &Fixture, sql: &str, value: &str) -> Result<Option<Value>> {
    Ok(db.rows(sql, [value])?.into_iter().next())
}

fn posts_with(connector: &Connector, check: impl Fn(&Value) -> bool) -> bool {
    connector.json_posts("chat.postMessage").iter().any(check)
}

fn script_posts(
    connector: &Connector,
    suffix: &str,
    successful: bool,
) -> Vec<connector::ScriptRequest> {
    let requests = if successful {
        connector.successful()
    } else {
        connector.requests()
    };
    requests
        .into_iter()
        .filter(|request| request.method == "POST" && request.path.ends_with(suffix))
        .collect()
}

fn same(left: &connector::ScriptRequest, right: &connector::ScriptRequest) -> bool {
    left.path == right.path && left.bytes == right.bytes
}

fn fault(connector: &Connector, suffix: &str, after: bool) {
    connector.add_fault(Fault {
        method: Some("POST".into()),
        suffix: format!("/{suffix}"),
        after,
    });
}

async fn question_flow(buttons: bool) -> Result<()> {
    let yes = if buttons {
        "Yes ".repeat(45).trim().to_owned()
    } else {
        "Yes".to_owned()
    };
    let model = ScriptedModel::start().await?;
    let choice = yes.clone();
    model.respond(move |request| {
        if after_question(request) {
            return after_question_reply();
        }
        if crate::sa_support::turn_tool_results(request).is_empty()
            && !request
                .tool_results()
                .iter()
                .any(|m| m["content"] != "null")
        {
            return Reply::tool_call(
                "question",
                "ask_user",
                json!({"question": "Proceed?", "choices": [choice, "No"]}),
            );
        }
        Reply::text("Confirmed")
    });
    let root = TestRoot::new()?;
    let agent = new_id();
    let (app, connector) = slack(
        &root,
        &agent,
        ServerConfig::new().model(&model),
        None,
        vec![],
    )
    .await?;
    let db = connector.open_journal()?;
    let mut sequence = 0_u32;
    let delivered = |id: &str| {
        let (db, connector) = (&db, &connector);
        let id = id.to_owned();
        async move {
            connector
                .until("outbox sent", || {
                    row(
                        db,
                        "SELECT * FROM slack_outbox WHERE id=? AND status='sent'",
                        &id,
                    )
                })
                .await
        }
    };
    macro_rules! incoming {
        ($text:expr, $user:expr, $root:expr, $channel:expr) => {{
            sequence += 1;
            connector
                .send(slack_envelope(sequence, $text, $user, $root, $channel))
                .await?;
            row(
                &db,
                "SELECT * FROM slack_inbox WHERE event=?",
                &format!("Ev{sequence}"),
            )?
            .ok_or("inbox row missing")?
        }};
    }
    fault(&connector, "turns", true);
    connector.start()?;
    let original = incoming!("Hello", "UUSER", None, "DCHAT");
    connector
        .expected_failure("Local agent request failed")
        .await?;
    connector.start()?;
    let turn = original["turn"].as_str().unwrap_or_default().to_owned();
    let q = connector
        .until("question stored", || {
            row(&db, "SELECT * FROM slack_questions WHERE turn=?", &turn)
        })
        .await?;
    let qid = q["id"].as_str().unwrap_or_default().to_owned();
    let qroot = q["root"].as_str().unwrap_or_default().to_owned();
    let notice = delivered(&qid).await?;
    let turns = script_posts(&connector, "/turns", false);
    ensure!(turns.len() == 2, "turn posts: {}", turns.len());
    ensure!(same(&turns[0], &turns[1]), "turn retry differs");
    ensure!(
        work_calls(&model) == 1,
        "model calls: {}",
        work_calls(&model)
    );
    let foreign = incoming!(
        &format!("!answer {qid} Yes"),
        "UOTHER",
        Some(qroot.as_str()),
        "DOTHER"
    );
    delivered(foreign["turn"].as_str().unwrap_or_default()).await?;
    ensure!(
        script_posts(&connector, "/answer", false).is_empty(),
        "foreign answer posted"
    );
    if buttons {
        let stamps: Value = serde_json::from_str(notice["timestamps"].as_str().unwrap_or("[]"))?;
        let ts = stamps
            .as_array()
            .and_then(|s| s.last())
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let posted = connector.json_posts("chat.postMessage");
        let elements = posted
            .iter()
            .find(|post| post["blocks"].is_array())
            .and_then(|post| {
                post["blocks"]
                    .as_array()
                    .and_then(|blocks| blocks.last())
                    .cloned()
            })
            .map(|block| block["elements"].clone())
            .ok_or("question buttons missing")?;
        ensure!(
            elements.as_array().map(Vec::len) == Some(2),
            "buttons: {elements}"
        );
        let label = format!("1. {}...", yes.chars().take(45).collect::<String>());
        ensure!(
            elements[0]["text"]["text"] == label.as_str(),
            "button label: {}",
            elements[0]["text"]["text"]
        );
        ensure!(
            posted
                .iter()
                .any(|post| post["text"].as_str().is_some_and(|t| t.contains(&yes))),
            "question text lacks the full choice"
        );
        let mut click = |index: usize, edit: &dyn Fn(&mut Value)| {
            sequence += 1;
            let mut envelope = json!({"type": "interactive", "envelope_id": format!("question-button-{sequence}"),
                "payload": {"type": "block_actions", "team": {"id": "TTEAM"}, "api_app_id": "AAPP",
                    "user": {"id": "UUSER"}, "channel": {"id": "DCHAT"},
                    "container": {"channel_id": "DCHAT", "message_ts": ts}, "message": {"user": "UBOT", "ts": ts},
                    "actions": [{"type": "button", "action_id": elements[index]["action_id"], "value": elements[index]["value"]}]}});
            edit(&mut envelope["payload"]);
            envelope
        };
        let callbacks = || db.scalar::<i64>("SELECT count(*) FROM slack_question_callbacks", []);
        let edits: [&dyn Fn(&mut Value); 4] = [
            &|p| p["user"]["id"] = json!("UOTHER"),
            &|p| p["message"]["ts"] = json!("1700000099.000001"),
            &|p| p["actions"][0] = json!({"type": "button", "action_id": "minarbot_answer_7", "value": format!("question:{qid}:7")}),
            &|p| p["actions"][0] = json!({"type": "button", "action_id": "minarbot_approve_yes", "value": format!("approval:{qid}:yes")}),
        ];
        for edit in edits {
            connector.send(click(0, edit)).await?;
            ensure!(callbacks()? == 0, "invalid click stored");
        }
        db.execute(
            "UPDATE slack_outbox SET status='sending' WHERE id=?",
            [&qid],
        )?;
        let early = click(0, &|_| {});
        let early_id = early["envelope_id"].as_str().unwrap_or_default().to_owned();
        connector.push(early).await?;
        connector
            .send(json!({"type": "probe", "envelope_id": "question-barrier"}))
            .await?;
        ensure!(
            !connector.acks().contains(&early_id),
            "click acknowledged before the notice was sent"
        );
        db.execute("UPDATE slack_outbox SET status='sent' WHERE id=?", [&qid])?;
        scripts(&app, &agent, 1, false).await?;
        connector.send(click(0, &|_| {})).await?;
        connector.expected_failure("403").await?;
        ensure!(
            db.scalar::<String>("SELECT status FROM slack_question_callbacks", [])? == "queued",
            "callback status"
        );
        ensure!(
            db.scalar::<String>("SELECT answer FROM slack_question_callbacks", [])? == yes,
            "callback answer"
        );
        ensure!(work_calls(&model) == 1, "revoked click reached the model");
        scripts(&app, &agent, 2, true).await?;
        fault(&connector, "answer", true);
        connector.start()?;
        connector
            .expected_failure("Local agent request failed")
            .await?;
        connector.start()?;
        connector.send(click(1, &|_| {})).await?;
        ensure!(
            db.scalar::<String>("SELECT answer FROM slack_question_callbacks", [])? == yes,
            "second click changed the answer"
        );
    } else {
        fault(&connector, "answer", true);
        let pending = incoming!(
            &format!("!answer {qid} Yes"),
            "UUSER",
            Some(qroot.as_str()),
            "DCHAT"
        );
        connector
            .expected_failure("Local agent request failed")
            .await?;
        sequence += 1;
        let stop = slack_envelope(
            sequence,
            &format!("!stop {}", pending["turn"].as_str().unwrap_or_default()),
            "UUSER",
            Some(qroot.as_str()),
            "DCHAT",
        );
        db.execute(
            "INSERT INTO slack_inbox(event,channel,sender,ts,root,text,thread,turn) VALUES(?,?,'UUSER',?,?,?,?,?)",
            [
                stop["payload"]["event_id"].as_str().unwrap_or_default().to_owned(),
                "DCHAT".to_owned(),
                stop["payload"]["event"]["ts"].as_str().unwrap_or_default().to_owned(),
                qroot.clone(),
                stop["payload"]["event"]["text"].as_str().unwrap_or_default().to_owned(),
                q["thread"].as_str().unwrap_or_default().to_owned(),
                new_id(),
            ],
        )?;
        connector.start()?;
    }
    delivered(&turn).await?;
    ensure!(
        posts_with(&connector, |post| post["text"] == "Confirmed"),
        "answer reply missing"
    );
    let answers = script_posts(&connector, "/answer", true);
    ensure!(answers.len() == 2, "answer posts: {}", answers.len());
    ensure!(same(&answers[0], &answers[1]), "answer retry differs");
    ensure!(
        work_calls(&model) == 2,
        "model calls: {}",
        work_calls(&model)
    );
    ensure!(
        app.get(&format!("/api/agents/{agent}/turns")).await? == json!([]),
        "main history"
    );
    scripts(&app, &agent, if buttons { 3 } else { 1 }, false).await?;
    let _ = incoming!("No longer authorized", "UUSER", None, "DCHAT");
    connector.expected_failure("403").await?;
    ensure!(work_calls(&model) == 2, "revoked message reached the model");
    Ok(())
}

#[tokio::test]
async fn slack_actual_agent_api_retries_one_task_and_isolates_requester_question_answers_buttons_false(
) -> Result<()> {
    question_flow(false).await
}

#[tokio::test]
async fn slack_actual_agent_api_retries_one_task_and_isolates_requester_question_answers_buttons_true(
) -> Result<()> {
    question_flow(true).await
}

#[tokio::test]
async fn slack_socket_mode_runner_acknowledges_journaled_event_and_delivers_once_after_reconnect(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|_| Reply::text("Socket reply"));
    let delivered = Arc::new(AtomicUsize::new(0));
    let violations: Shared<Vec<String>> = Shared::default();
    let (count, wrong) = (delivered.clone(), violations.clone());
    let remote: connector::Remote = Arc::new(move |post| {
        let (count, wrong) = (count.clone(), wrong.clone());
        Box::pin(async move {
            if post.method != "chat.postMessage" {
                return Answer::Default;
            }
            count.fetch_add(1, Ordering::SeqCst);
            let body: Value = serde_json::from_slice(&post.raw).unwrap_or(Value::Null);
            if body["thread_ts"] != "1700000000.000001"
                || body["channel"] != "DCHAT"
                || body["text"] != "Socket reply"
            {
                if let Ok(mut wrong) = wrong.lock() {
                    wrong.push(format!("delivery: {body}"));
                }
            }
            Answer::Value(json!({"channel": "DCHAT", "ts": "1700000001.000001"}))
        })
    });
    let root = TestRoot::new()?;
    let agent = new_id();
    let (_app, connector) = slack(
        &root,
        &agent,
        ServerConfig::new().model(&model),
        Some(remote),
        vec![],
    )
    .await?;
    let journal = connector.directory.join("slack.sqlite");
    let checks = violations.clone();
    connector.set_socket_script(SocketScript {
        rate_limit_once: vec!["auth.test".into(), "apps.connections.open".into()],
        on_connect: vec![slack_envelope(1, "Hello", "UUSER", None, "DCHAT")],
        disconnect_after_first_ack: true,
        on_ack: Some(Arc::new(move |id| {
            let inbox = minarbot_testkit::db::Inspector::open(&journal)
                .and_then(|db| db.scalar::<i64>("SELECT count(*) FROM slack_inbox", []));
            if let Ok(mut checks) = checks.lock() {
                if id != "envelope-1" {
                    checks.push(format!("ack {id}"));
                }
                if !matches!(inbox, Ok(1)) {
                    checks.push(format!("acknowledged before journaling: {inbox:?}"));
                }
            }
        })),
    });
    connector.start()?;
    connector
        .until("delivery and redelivered ack", || {
            Ok(
                (delivered.load(Ordering::SeqCst) >= 1 && connector.acks().len() >= 2)
                    .then_some(()),
            )
        })
        .await?;
    tokio::time::sleep(Duration::from_millis(20)).await;
    connector.stop().await?;
    ensure!(model.calls() == 1, "model calls: {}", model.calls());
    ensure!(
        delivered.load(Ordering::SeqCst) == 1,
        "deliveries: {}",
        delivered.load(Ordering::SeqCst)
    );
    ensure!(
        connector.connections() >= 2,
        "connections: {}",
        connector.connections()
    );
    ensure!(connector.acks().len() >= 2, "acks: {:?}", connector.acks());
    ensure!(
        !connector.directory.join("owner.lock").exists(),
        "journal lock kept"
    );
    let headers = connector.authorizations();
    ensure!(
        headers.iter().any(|h| h.starts_with("Bearer xapp-")),
        "app token unused"
    );
    ensure!(
        headers.iter().any(|h| h.starts_with("Bearer xoxb-")),
        "bot token unused"
    );
    let violations = lock(&violations)?.clone();
    ensure!(violations.is_empty(), "socket checks: {violations:?}");
    Ok(())
}

#[tokio::test]
async fn slack_status_and_stop_scope_to_requester_root_and_preserve_cancellation_identity_after_lost_responses(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|request| {
        if after_question(request) {
            return after_question_reply();
        }
        Reply::tool_call(
            "ask",
            "ask_user",
            json!({"question": "Wait", "choices": ["Yes", "No"]}),
        )
    });
    let root = TestRoot::new()?;
    let agent = new_id();
    let (app, connector) = slack(
        &root,
        &agent,
        ServerConfig::new().model(&model),
        None,
        vec![],
    )
    .await?;
    let db = connector.open_journal()?;
    let status = |turn: &str| -> Result<Option<String>> {
        Ok(
            row(&db, "SELECT status FROM slack_inbox WHERE turn=?", turn)?
                .and_then(|r| r["status"].as_str().map(str::to_owned)),
        )
    };
    let until = |id: &str| {
        let (db, connector, id) = (&db, &connector, id.to_owned());
        async move {
            connector
                .until("command reply sent", || {
                    row(
                        db,
                        "SELECT 1 AS sent FROM slack_outbox WHERE id=? AND status='sent'",
                        &id,
                    )
                })
                .await
                .map(|_| ())
        }
    };
    let mut sequence = 0_u32;
    let mut seed = |text: &str, parent: Option<&str>, thread: Option<&str>| -> Result<Value> {
        sequence += 1;
        let sample = slack_envelope(sequence, text, "UUSER", None, "DCHAT");
        let ts = sample["payload"]["event"]["ts"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        db.execute(
            "INSERT INTO slack_inbox(event,channel,sender,ts,root,text,thread,turn) VALUES(?,?,?,?,?,?,?,?)",
            [
                format!("Ev{sequence}"),
                "DCHAT".into(),
                "UUSER".into(),
                ts.clone(),
                parent.map_or(ts, str::to_owned),
                text.to_owned(),
                thread.map_or_else(new_id, str::to_owned),
                new_id(),
            ],
        )?;
        row(
            &db,
            "SELECT * FROM slack_inbox WHERE event=?",
            &format!("Ev{sequence}"),
        )?
        .ok_or_else(|| "seeded row missing".into())
    };
    let unsent = seed("Never execute", None, None)?;
    let stop = seed(
        &format!("!stop {}", unsent["turn"].as_str().unwrap_or_default()),
        unsent["root"].as_str(),
        unsent["thread"].as_str(),
    )?;
    connector.start()?;
    until(stop["turn"].as_str().unwrap_or_default()).await?;
    ensure!(work_calls(&model) == 0, "unsent task reached the model");
    ensure!(
        script_posts(&connector, "/turns", false).is_empty(),
        "unsent task submitted"
    );
    ensure!(
        status(unsent["turn"].as_str().unwrap_or_default())?.as_deref() == Some("cancelled"),
        "unsent status"
    );
    let mut events = 100_u32;
    let mut incoming = |text: &str, parent: Option<&str>, user: &str, channel: &str| {
        events += 1;
        (events, slack_envelope(events, text, user, parent, channel))
    };
    async fn deliver(
        connector: &Connector,
        db: &Fixture,
        (n, envelope): (u32, Value),
    ) -> Result<Value> {
        connector.send(envelope).await?;
        row(
            db,
            "SELECT * FROM slack_inbox WHERE event=?",
            &format!("Ev{n}"),
        )?
        .ok_or_else(|| "inbox row missing".into())
    }
    fault(&connector, "turns", true);
    let active = deliver(
        &connector,
        &db,
        incoming("!ANSWER ordinary task", None, "UUSER", "DCHAT"),
    )
    .await?;
    connector
        .expected_failure("Local agent request failed")
        .await?;
    let active_turn = active["turn"].as_str().unwrap_or_default().to_owned();
    let active_root = active["root"].as_str().unwrap_or_default().to_owned();
    ensure!(
        status(&active_turn)?.as_deref() == Some("submitting"),
        "submitting status"
    );
    db.execute(
        "UPDATE slack_inbox SET status='queued' WHERE event=?",
        [active["event"].as_str().unwrap_or_default()],
    )?;
    db.execute_batch("PRAGMA user_version=0")?;
    ensure!(
        connector.journal("status", None, None, None, None)?.status == 0,
        "journal status"
    );
    ensure!(
        status(&active_turn)?.as_deref() == Some("submitting"),
        "journal repair changed the status"
    );
    connector.start()?;
    let first = deliver(
        &connector,
        &db,
        incoming(&format!("!stop {active_turn}"), None, "UUSER", "DCHAT"),
    )
    .await?;
    until(first["turn"].as_str().unwrap_or_default()).await?;
    let other = deliver(
        &connector,
        &db,
        incoming(
            &format!("!stop {active_turn}"),
            Some(&active_root),
            "UOTHER",
            "DOTHER",
        ),
    )
    .await?;
    until(other["turn"].as_str().unwrap_or_default()).await?;
    let probe = deliver(
        &connector,
        &db,
        incoming("!status", Some(&active_root), "UUSER", "DCHAT"),
    )
    .await?;
    until(probe["turn"].as_str().unwrap_or_default()).await?;
    ensure!(
        script_posts(&connector, "/cancel", false).is_empty(),
        "foreign stop cancelled"
    );
    ensure!(
        posts_with(&connector, |post| post["text"]
            .as_str()
            .is_some_and(|t| t.contains(&active_turn) && t.contains("!stop"))),
        "status lacks the stop hint"
    );
    fault(&connector, "cancel", true);
    let cancellation = deliver(
        &connector,
        &db,
        incoming(
            &format!("!stop {active_turn}"),
            Some(&active_root),
            "UUSER",
            "DCHAT",
        ),
    )
    .await?;
    connector
        .expected_failure("Local agent request failed")
        .await?;
    connector.start()?;
    let cancellation_turn = cancellation["turn"].as_str().unwrap_or_default().to_owned();
    until(&cancellation_turn).await?;
    let cancels = script_posts(&connector, "/cancel", false);
    ensure!(cancels.len() == 2, "cancel posts: {}", cancels.len());
    ensure!(same(&cancels[0], &cancels[1]), "cancel retry differs");
    let body: Value = serde_json::from_slice(&cancels[0].bytes)?;
    ensure!(
        body["id"] == cancellation_turn.as_str(),
        "cancel id: {body}"
    );
    let thread = active["thread"].as_str().unwrap_or_default();
    ensure!(
        app.get(&format!("/api/threads/{thread}/turns")).await?[0]["status"] == "cancelled",
        "task not cancelled"
    );
    fault(&connector, "turns", false);
    let before = deliver(
        &connector,
        &db,
        incoming("Do not create", None, "UUSER", "DCHAT"),
    )
    .await?;
    connector
        .expected_failure("Local agent request failed")
        .await?;
    let prior = work_calls(&model);
    let before_turn = before["turn"].as_str().unwrap_or_default().to_owned();
    let unavailable = seed(
        &format!("!stop {before_turn}"),
        before["root"].as_str(),
        before["thread"].as_str(),
    )?;
    connector.start()?;
    until(unavailable["turn"].as_str().unwrap_or_default()).await?;
    ensure!(
        status(&before_turn)?.as_deref() == Some("unavailable"),
        "lost submission status"
    );
    ensure!(
        work_calls(&model) == prior,
        "lost submission reached the model"
    );
    ensure!(
        app.get(&format!(
            "/api/threads/{}/turns",
            before["thread"].as_str().unwrap_or_default()
        ))
        .await?
            == json!([]),
        "lost submission created a turn"
    );
    let probe = deliver(
        &connector,
        &db,
        incoming("!status", before["root"].as_str(), "UUSER", "DCHAT"),
    )
    .await?;
    until(probe["turn"].as_str().unwrap_or_default()).await?;
    ensure!(
        posts_with(&connector, |post| post["text"]
            == "No active tasks in this thread."),
        "empty status"
    );
    connector.stop().await?;
    Ok(())
}

#[tokio::test]
async fn slack_offline_recovery_pins_revisions_retains_confirmed_chunks_and_requires_explicit_cli_confirmation(
) -> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let mode: Shared<&'static str> = Arc::new(std::sync::Mutex::new("lost"));
    let violations: Shared<Vec<String>> = Shared::default();
    let (count, current, wrong) = (calls.clone(), mode.clone(), violations.clone());
    let remote: connector::Remote = Arc::new(move |post| {
        let (count, current, wrong) = (count.clone(), current.clone(), wrong.clone());
        Box::pin(async move {
            let calls = count.fetch_add(1, Ordering::SeqCst) + 1;
            let mode = current.lock().map(|mode| *mode).unwrap_or("lost");
            if mode == "rejected" {
                return Answer::Raw(
                    200,
                    json!({"ok": false, "error": "channel_not_found"})
                        .to_string()
                        .into_bytes(),
                );
            }
            if calls > 1 {
                let body: Value = serde_json::from_slice(&post.raw).unwrap_or(Value::Null);
                if body["text"] != "x".repeat(1000).as_str() {
                    if let Ok(mut wrong) = wrong.lock() {
                        wrong.push(format!("chunk {calls}"));
                    }
                }
                if mode == "lost" {
                    return Answer::Lose;
                }
            }
            Answer::Value(json!({"channel": "DCHAT", "ts": format!("1700000001.{calls:06}")}))
        })
    });
    let root = TestRoot::new()?;
    let agent = new_id();
    let (app, connector) = slack(&root, &agent, ServerConfig::new(), Some(remote), vec![]).await?;
    let db = connector.open_journal()?;
    let id = new_id();
    let add = |id: &str, text: &str| {
        db.execute(
            "INSERT INTO slack_outbox(id,channel,sender,root,text) VALUES (?,?,?,?,?)",
            [id, "DCHAT", "UUSER", "1700000000.000001", text],
        )
    };
    let read = |id: &str| -> Result<Value> {
        connector
            .journal("inspect", Some(id), None, None, None)?
            .json()
    };
    let cli = |id: &str, revision: i64, action: &str, confirmation: Option<&str>| -> Result<i32> {
        Ok(connector
            .journal(
                "recover",
                Some(id),
                Some(revision),
                Some(action),
                confirmation,
            )?
            .status)
    };
    let settle = |id: String, status: &'static str| {
        let (connector, db) = (&connector, &db);
        async move {
            connector.start()?;
            connector
                .until(&format!("reply {status}"), || {
                    Ok(row(db, "SELECT status FROM slack_outbox WHERE id=?", &id)?
                        .is_some_and(|row| row["status"] == status)
                        .then_some(()))
                })
                .await?;
            connector.stop().await
        }
    };
    add(&id, &"x".repeat(5000))?;
    settle(id.clone(), "uncertain").await?;
    ensure!(
        calls.load(Ordering::SeqCst) == 2,
        "calls: {}",
        calls.load(Ordering::SeqCst)
    );
    let inspected = read(&id)?;
    ensure!(
        inspected["reply"]["status"] == "uncertain",
        "inspect: {inspected}"
    );
    ensure!(inspected["reply"]["part"] == 1, "part: {inspected}");
    ensure!(inspected["revision"] == 0, "revision: {inspected}");
    ensure!(cli(&id, 0, "retry", None)? != 0, "retry without risk flag");
    ensure!(
        cli(&id, 0, "retry", Some("--accept-duplicate-risk"))? == 0,
        "retry rejected"
    );
    settle(id.clone(), "uncertain").await?;
    ensure!(calls.load(Ordering::SeqCst) == 3, "calls after retry");
    ensure!(
        cli(&id, 0, "retry", Some("--accept-duplicate-risk"))? != 0,
        "stale revision accepted"
    );
    let inspected = read(&id)?;
    ensure!(inspected["reply"]["part"] == 1, "part: {inspected}");
    let stamps: Value =
        serde_json::from_str(inspected["reply"]["timestamps"].as_str().unwrap_or("[]"))?;
    ensure!(
        stamps.as_array().map(Vec::len) == Some(1),
        "timestamps: {inspected}"
    );
    ensure!(inspected["revision"] == 1, "revision: {inspected}");
    ensure!(
        cli(&id, 1, "retry", Some("--accept-duplicate-risk"))? == 0,
        "second retry rejected"
    );
    scripts(&app, &agent, 1, false).await?;
    connector.start()?;
    connector.expected_failure("403").await?;
    ensure!(calls.load(Ordering::SeqCst) == 3, "revoked retry sent");
    scripts(&app, &agent, 2, true).await?;
    *lock(&mode)? = "ok";
    settle(id.clone(), "sent").await?;
    ensure!(calls.load(Ordering::SeqCst) == 4, "calls after recovery");
    db.execute("UPDATE slack_outbox SET status='sending' WHERE id=?", [&id])?;
    let inspected = read(&id)?;
    ensure!(
        inspected["reply"]["status"] == "uncertain",
        "interrupted: {inspected}"
    );
    ensure!(inspected["revision"] == 2, "revision: {inspected}");
    ensure!(
        cli(&id, 2, "discard", None)? != 0,
        "discard without confirm"
    );
    ensure!(
        cli(&id, 2, "discard", Some("--confirm"))? == 0,
        "discard rejected"
    );
    ensure!(
        cli(&id, 3, "retry", Some("--accept-duplicate-risk"))? != 0,
        "retry after discard"
    );
    let inspected = read(&id)?;
    ensure!(
        inspected["reply"]["status"] == "discarded",
        "discarded: {inspected}"
    );
    ensure!(
        inspected["decisions"].as_array().map(Vec::len) == Some(3),
        "decisions: {inspected}"
    );
    ensure!(inspected["reply"]["part"] == 2, "part: {inspected}");
    let rejected = new_id();
    *lock(&mode)? = "rejected";
    add(&rejected, "Rejected")?;
    settle(rejected.clone(), "failed").await?;
    let inspected = read(&rejected)?;
    ensure!(
        inspected["reply"]["status"] == "failed",
        "rejected: {inspected}"
    );
    ensure!(
        inspected["reply"]["part"] == 0,
        "rejected part: {inspected}"
    );
    ensure!(
        cli(&rejected, 0, "retry", Some("--accept-duplicate-risk"))? == 0,
        "retry rejected reply"
    );
    ensure!(read(&rejected)?["reply"]["status"] == "queued", "requeued");
    let violations = lock(&violations)?.clone();
    ensure!(violations.is_empty(), "platform checks: {violations:?}");
    Ok(())
}

#[tokio::test]
async fn slack_startup_cancellation_releases_lock_and_status_retains_authentication_cooldown(
) -> Result<()> {
    let requests = Arc::new(AtomicUsize::new(0));
    let count = requests.clone();
    let server = RawProvider::start(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
        (
            429,
            vec![("retry-after".into(), "60".into())],
            b"Wait".to_vec(),
        )
    })
    .await?;
    let root = TestRoot::new()?;
    let directory = root.path().join("state");
    let options = json!({"platform": "slack", "directory": directory, "scriptDirectory": root.path().join("unused"),
        "agent": new_id(), "team": "TTEAM", "app": "AAPP", "botToken": connector::SLACK_BOT_TOKEN,
        "appToken": connector::SLACK_APP_TOKEN, "users": ["UUSER"], "origin": server.origin()});
    let journal = directory.join("slack.sqlite");
    let recorded = Arc::new(AtomicBool::new(false));
    let seen = recorded.clone();
    let stop = async move {
        for _ in 0..500 {
            if let Ok(db) = minarbot_testkit::db::Inspector::open(&journal) {
                if db
                    .scalar::<i64>(
                        "SELECT count(*) FROM slack_session_cooldowns WHERE method='auth.test'",
                        [],
                    )
                    .is_ok_and(|n| n > 0)
                {
                    seen.store(true, Ordering::SeqCst);
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    connector::run(&options, &root.path().join("logs"), stop).await?;
    ensure!(recorded.load(Ordering::SeqCst), "cooldown not recorded");
    ensure!(
        requests.load(Ordering::SeqCst) == 1,
        "requests: {}",
        requests.load(Ordering::SeqCst)
    );
    ensure!(!directory.join("owner.lock").exists(), "journal lock kept");
    let status = connector::journal(
        Platform::Slack,
        &directory,
        "status",
        None,
        None,
        None,
        None,
    )?;
    ensure!(status.status == 0, "status: {}", status.stderr);
    let rows = status.json()?["sessionCooldowns"].clone();
    ensure!(
        rows.as_array().map(Vec::len) == Some(1),
        "cooldowns: {rows}"
    );
    ensure!(rows[0]["method"] == "auth.test", "cooldown method: {rows}");
    ensure!(
        rows[0]["until"]
            .as_i64()
            .is_some_and(|until| until > minarbot_testkit::restore::now()),
        "cooldown until: {rows}"
    );
    Ok(())
}

#[tokio::test]
async fn slack_interactive_socket_mode_withholds_early_ack_and_commits_one_decision_before_redelivery_ack(
) -> Result<()> {
    let (notice, run) = (new_id(), new_id());
    let exposed = Arc::new(AtomicBool::new(false));
    let gate = Arc::new(Notify::new());
    let (shown, released) = (exposed.clone(), gate.clone());
    let remote: connector::Remote = Arc::new(move |post| {
        let (shown, released) = (shown.clone(), released.clone());
        Box::pin(async move {
            if post.method != "chat.postMessage" {
                return Answer::Default;
            }
            let body: Value = serde_json::from_slice(&post.raw).unwrap_or(Value::Null);
            if body["blocks"].is_null() {
                return Answer::Default;
            }
            let wait = released.notified();
            shown.store(true, Ordering::SeqCst);
            wait.await;
            Answer::Value(json!({"channel": "DCHAT", "ts": "1700000001.000001"}))
        })
    });
    let root = TestRoot::new()?;
    let agent = new_id();
    let (_app, connector) = slack(&root, &agent, ServerConfig::new(), Some(remote), vec![]).await?;
    let db = connector.open_journal()?;
    let (ts, root_ts) = ("1700000001.000001", "1700000000.000001");
    let click = |id: &str, choice: &str| {
        json!({"type": "interactive", "envelope_id": id, "payload": {"type": "block_actions", "team": {"id": "TTEAM"},
            "api_app_id": "AAPP", "user": {"id": "UUSER"}, "channel": {"id": "DCHAT"},
            "container": {"channel_id": "DCHAT", "message_ts": ts}, "message": {"user": "UBOT", "ts": ts},
            "actions": [{"type": "button", "action_id": format!("minarbot_approve_{choice}"),
                "value": format!("approval:{notice}:{choice}")}]}})
    };
    let decision = || -> Result<Option<Value>> {
        Ok(db
            .rows("SELECT id,accepted FROM slack_callbacks", [])?
            .into_iter()
            .next())
    };
    db.execute(
        "INSERT INTO slack_workflows VALUES (?,'DCHAT','UUSER',?,?)",
        [run.clone(), root_ts.to_owned(), new_id()],
    )?;
    db.execute(
        "INSERT INTO slack_approvals VALUES (?,1,?)",
        [&run, &notice],
    )?;
    db.execute(
        "INSERT INTO slack_outbox(id,channel,sender,root,text) VALUES (?,'DCHAT','UUSER',?,'Proceed?')",
        [&notice, &root_ts.to_owned()],
    )?;
    connector.start()?;
    connector
        .until("approval send exposed", || {
            Ok(exposed.load(Ordering::SeqCst).then_some(()))
        })
        .await?;
    connector.push(click("early", "yes")).await?;
    connector
        .send(json!({"type": "probe", "envelope_id": "barrier"}))
        .await?;
    ensure!(
        !connector.acks().contains(&"early".to_owned()),
        "early click acknowledged"
    );
    ensure!(decision()?.is_none(), "early click decided");
    gate.notify_one();
    connector
        .until("approval sent", || {
            row(
                &db,
                "SELECT 1 AS sent FROM slack_outbox WHERE id=? AND status='sent'",
                &notice,
            )
        })
        .await?;
    connector.send(click("retry", "yes")).await?;
    let recorded = decision()?.ok_or("decision missing")?;
    ensure!(recorded["accepted"] == 1, "decision: {recorded}");
    connector.send(click("retry", "yes")).await?;
    ensure!(
        decision()? == Some(recorded.clone()),
        "redelivery changed the decision"
    );
    connector.send(click("duplicate", "no")).await?;
    ensure!(decision()? == Some(recorded), "second decision accepted");
    connector.stop().await?;
    Ok(())
}

#[tokio::test]
async fn slack_shared_channels_require_opt_in_mention_and_retain_one_scoped_multi_user_thread(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    model.respond(move |_| {
        Reply::text(format!(
            "Reply {}",
            count.fetch_add(1, Ordering::SeqCst) + 1
        ))
    });
    let root = TestRoot::new()?;
    let agent = new_id();
    let (app, connector) = slack(
        &root,
        &agent,
        ServerConfig::new().model(&model),
        None,
        vec!["CROOM".into(), "GROOM".into()],
    )
    .await?;
    let db = connector.open_journal()?;
    let rows = || db.scalar::<i64>("SELECT count(*) FROM slack_inbox", []);
    let row_of = |n: &str| -> Result<Value> {
        row(&db, "SELECT * FROM slack_inbox WHERE event=?", n)?
            .ok_or_else(|| format!("inbox {n} missing").into())
    };
    let incoming =
        |n: u32, text: &str, user: &str, thread: Option<&str>, channel: &str, mention: bool| {
            let mut event = slack_envelope(n, text, user, thread, channel);
            if channel.starts_with('G') {
                event["payload"]["event"]["channel_type"] = json!("group");
            }
            if mention {
                event["payload"]["event"]["type"] = json!("app_mention");
                if let Some(fields) = event["payload"]["event"].as_object_mut() {
                    fields.remove("channel_type");
                }
            }
            event
        };
    let settle = |n: u32| {
        let (connector, db) = (&connector, &db);
        async move {
            connector
                .until(&format!("event {n} settled"), || {
                    let Some(change) = row(
                        db,
                        "SELECT * FROM slack_inbox WHERE event=?",
                        &format!("Ev{n}"),
                    )?
                    else {
                        return Ok(None);
                    };
                    row(
                        db,
                        "SELECT 1 AS sent FROM slack_outbox WHERE id=? AND status='sent'",
                        change["turn"].as_str().unwrap_or_default(),
                    )
                })
                .await
                .map(|_| ())
        }
    };
    let sent = || connector.json_posts("chat.postMessage");
    connector.set_channels(vec![]);
    connector.start()?;
    connector
        .send(incoming(
            4,
            "<@UBOT> disabled",
            "UUSER",
            None,
            "CROOM",
            false,
        ))
        .await?;
    ensure!(rows()? == 0, "disabled channel stored");
    connector.stop().await?;
    connector.set_channels(vec!["CROOM".into(), "GROOM".into()]);
    connector.start()?;
    connector
        .send(incoming(1, "Ambient", "UUSER", None, "CROOM", false))
        .await?;
    connector
        .send(incoming(
            2,
            "<@UBOT> foreign",
            "UUSER",
            None,
            "CFOREIGN",
            false,
        ))
        .await?;
    connector
        .send(incoming(
            3,
            "<@UBOT> disallowed",
            "UNOT",
            None,
            "CROOM",
            false,
        ))
        .await?;
    ensure!(rows()? == 0, "ignored events stored");
    connector
        .send(incoming(5, "<@UBOT> First", "UUSER", None, "CROOM", true))
        .await?;
    let mut alias = incoming(5, "<@UBOT> First", "UUSER", None, "CROOM", false);
    alias["envelope_id"] = json!("alias-5");
    alias["payload"]["event_id"] = json!("EvAlias");
    connector.send(alias).await?;
    ensure!(rows()? == 1, "alias stored twice");
    settle(5).await?;
    let first = row_of("Ev5")?;
    let first_root = first["root"].as_str().unwrap_or_default().to_owned();
    connector
        .send(incoming(
            6,
            "Second",
            "UOTHER",
            Some(&first_root),
            "CROOM",
            false,
        ))
        .await?;
    settle(6).await?;
    ensure!(
        row_of("Ev6")?["thread"] == first["thread"],
        "second message thread"
    );
    ensure!(
        calls.load(Ordering::SeqCst) == 2,
        "calls: {}",
        calls.load(Ordering::SeqCst)
    );
    ensure!(
        sent()
            .iter()
            .all(|post| post["channel"] == "CROOM" && post["thread_ts"] == first_root.as_str()),
        "replies left the thread: {:?}",
        sent()
    );
    let second = model.requests().get(1).cloned().ok_or("second request")?;
    let texts = second.texts().join("\n");
    ensure!(
        texts.contains("Slack UUSER: First"),
        "first speaker missing: {texts}"
    );
    ensure!(
        texts.contains("Slack UOTHER: Second"),
        "second speaker missing: {texts}"
    );
    ensure!(
        app.get(&format!("/api/agents/{agent}/turns")).await? == json!([]),
        "main history"
    );
    connector
        .send(incoming(7, "<@UBOT> Private", "UUSER", None, "GROOM", true))
        .await?;
    settle(7).await?;
    ensure!(
        row_of("Ev7")?["thread"] != first["thread"],
        "private channel shared a thread"
    );
    let before_long = calls.load(Ordering::SeqCst);
    connector
        .send(incoming(
            9,
            &format!("<@UBOT> {}", "x".repeat(15992)),
            "UUSER",
            None,
            "CROOM",
            false,
        ))
        .await?;
    settle(9).await?;
    ensure!(
        calls.load(Ordering::SeqCst) == before_long,
        "long message reached the model"
    );
    ensure!(
        sent()
            .last()
            .and_then(|post| post["text"].as_str())
            .is_some_and(|t| t.starts_with("Message is too long. Maximum 15987")),
        "long message notice"
    );
    ensure!(
        row_of("Ev9")?["text"] == "x".repeat(15992).as_str(),
        "long text stored"
    );
    connector
        .send(incoming(
            10,
            &format!("<@UBOT> {}", "x".repeat(15987)),
            "UUSER",
            None,
            "CROOM",
            false,
        ))
        .await?;
    settle(10).await?;
    ensure!(
        calls.load(Ordering::SeqCst) == before_long + 1,
        "maximum message rejected"
    );
    let last = model.requests().last().cloned().ok_or("no request")?;
    ensure!(
        last.messages().iter().any(|m| m["role"] == "user"
            && m["content"]
                .as_str()
                .is_some_and(|c| c.chars().count() == 16000)),
        "maximum message not forwarded whole"
    );
    connector.stop().await?;
    db.execute(
        "INSERT INTO slack_inbox(event,channel,sender,ts,root,text,thread,turn) VALUES('Ev8','CROOM','UUSER','1700000000.000008',?,'Never send',?,?)",
        [first_root.clone(), first["thread"].as_str().unwrap_or_default().to_owned(), new_id()],
    )?;
    db.execute_batch("UPDATE slack_outbox SET status='queued' WHERE channel='CROOM'")?;
    connector.set_channels(vec![]);
    connector.start()?;
    connector
        .until("disabled channel revoked", || {
            let revoked = db.scalar::<i64>(
                "SELECT count(*) FROM slack_outbox WHERE channel='CROOM' AND status='revoked'",
                [],
            )?;
            Ok((row_of("Ev8")?["status"] == "revoked" && revoked == 4).then_some(()))
        })
        .await?;
    ensure!(
        calls.load(Ordering::SeqCst) == 4,
        "calls: {}",
        calls.load(Ordering::SeqCst)
    );
    ensure!(sent().len() == 5, "sent: {}", sent().len());
    connector.stop().await?;
    Ok(())
}
