//! Dispatch: steer, restart, fork and queued dispatch (port of
//! `app/dispatch.test.ts`).

use crate::fx_support::{id, row, rows};
use minarbot_testkit::{
    db::{Fixture as Db, Inspector},
    gated::{Answer, Chunk, Gate, GatedModel, Parked},
    model::ModelRequest,
    prelude::*,
    server::Gateway,
    Server,
};
use std::collections::HashMap;

/// The first `LOOP:`/`HOLD:`/`PARTIAL:` user message not yet answered with
/// assistant text (the Node fixture's `source`).
fn source(request: &ModelRequest) -> Option<String> {
    let messages = request.messages();
    messages.iter().enumerate().find_map(|(index, message)| {
        let content = message["content"].as_str()?;
        let tagged = ["LOOP:", "HOLD:", "PARTIAL:"]
            .iter()
            .any(|prefix| content.starts_with(prefix));
        // A streamed tool step folds to `content: ""` (the Node API runtime
        // sent `null`), which is not an answer.
        let answered = messages.get(index + 1).is_some_and(|next| {
            next["role"] == "assistant"
                && next["content"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty())
        });
        (message["role"] == "user" && tagged && !answered).then(|| content.to_owned())
    })
}

fn last(request: &ModelRequest) -> String {
    request
        .messages()
        .last()
        .and_then(|message| message["content"].as_str())
        .unwrap_or_default()
        .to_owned()
}

/// Key of a parked request: its tagged source, or the `HOLD:` text.
fn key(request: &ModelRequest) -> String {
    source(request).unwrap_or_else(|| last(request))
}

fn responder(request: &ModelRequest) -> Answer {
    let last = last(request);
    let source = source(request);
    let tools = request.tool_results().len();
    if last.starts_with("NEW:") {
        return Answer::text("Restart finished");
    }
    if source
        .as_deref()
        .is_some_and(|text| text.starts_with("LOOP:"))
    {
        return match tools {
            0 => Answer::Park,
            1 => Answer::tool("read_provider_settings", json!({})),
            _ => Answer::text("Steered loop finished"),
        };
    }
    if source
        .as_deref()
        .is_some_and(|text| text.starts_with("PARTIAL:"))
    {
        if tools == 0 {
            return Answer::Park;
        }
        // Partial text, then the stream stays open (the Node `res.end()` was
        // never reached before the restart command).
        return Answer::Stream(
            vec![
                Chunk::Text("Partial answer before restart".into()),
                Chunk::Wait(Gate::new()),
            ],
            "stop".into(),
        );
    }
    if last.starts_with("HOLD:") {
        return Answer::Park;
    }
    Answer::text(format!("Answer: {last}"))
}

/// One dispatch server (the Node `fixture`).
struct Fixture {
    app: Server,
    model: GatedModel,
    agent: String,
    path: String,
    stash: HashMap<String, Parked>,
    _root: TestRoot,
}

impl Fixture {
    async fn start(stream: bool) -> Result<Self> {
        let model = GatedModel::start().await?;
        model.respond(responder);
        let root = TestRoot::new()?;
        let app = root
            .start(ServerConfig::new().gateway(Gateway {
                stream: stream.then_some(true),
                ..Gateway::new(model.base_url())
            }))
            .await?;
        let agent = id(&app
            .post(
                "/api/agents",
                json!({"runtime": "api", "id": new_id(), "name": "Dispatch agent", "model": "fake", "instructions": "Help."}),
            )
            .await?)?;
        let path = format!("/api/agents/{agent}/turns");
        Ok(Self {
            app,
            model,
            agent,
            path,
            stash: HashMap::new(),
            _root: root,
        })
    }

    /// The parked request for `text` (others are kept for later).
    async fn held(&mut self, text: &str) -> Result<Parked> {
        if let Some(parked) = self.stash.remove(text) {
            return Ok(parked);
        }
        loop {
            let parked = self.model.parked().await?;
            let found = key(&parked.request);
            if found == text {
                return Ok(parked);
            }
            self.stash.insert(found, parked);
        }
    }

    async fn turn(&self, id: &str, done: impl Fn(&Value) -> bool) -> Result<Value> {
        self.app.wait_row(&self.path, id, done).await
    }

    fn calls(&self) -> Vec<ModelRequest> {
        self.model.requests()
    }

    fn calls_with_last(&self, text: &str) -> usize {
        self.calls()
            .iter()
            .filter(|request| last(request) == text)
            .count()
    }
}

fn mentions(request: Option<&ModelRequest>, text: &str) -> bool {
    request.is_some_and(|request| request.body["messages"].to_string().contains(text))
}

#[tokio::test]
async fn startup_failure_reports_child_output_and_releases_the_provider_and_directory() -> Result<()>
{
    let scripts = TempWorkspace::new()?;
    let binary = scripts.write(
        "failing-server",
        "#!/bin/sh\nprintf \"fixture stdout\\n\"\nprintf \"fixture stderr\\n\" >&2\nexit 17\n",
    )?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))?;
    }
    let model = GatedModel::start().await?;
    model.respond(|_| Answer::text("unused"));
    let root = TestRoot::new()?;
    let directory = root.path().to_owned();
    let error = root
        .start_error(
            ServerConfig::new()
                .gateway(Gateway::new(model.base_url()))
                .binary(&binary),
        )
        .await?;
    ensure!(
        error.contains("exited during startup") && error.contains("17"),
        "status: {error}"
    );
    ensure!(
        error.contains("stdout tail:\nfixture stdout\nstderr tail:\nfixture stderr"),
        "output tails: {error}"
    );
    ensure!(model.calls() == 0, "failed server reached the provider");
    drop(root);
    ensure!(
        !directory.exists(),
        "test directory kept after the failed start"
    );
    Ok(())
}

#[tokio::test]
async fn steer_is_injected_once_at_next_tool_loop_safe_point_durable_and_idempotent() -> Result<()>
{
    let mut f = Fixture::start(false).await?;
    let root = new_id();
    let text = format!("LOOP:{root}");
    f.app
        .post(&f.path, json!({"id": root, "text": text}))
        .await?;
    let held = f.held(&text).await?;
    let guidance = json!({"id": new_id(), "text": "Use the new constraints", "dispatchMode": "steer", "targetTurn": root});
    let receipt = f.app.post(&f.path, guidance.clone()).await?;
    ensure!(receipt["dispatch_mode"] == "steer", "receipt {receipt}");
    ensure!(
        f.app.post(&f.path, guidance.clone()).await? == receipt,
        "steer replay"
    );
    let mut restart = guidance.clone();
    restart["dispatchMode"] = json!("restart");
    f.app.post_expect(&f.path, restart, 409).await?;
    f.app
        .post_expect(
            &format!("{}/{}/edit", f.path, id(&guidance)?),
            json!({"id": new_id(), "editor": new_id(), "action": "hold", "revision": 1}),
            409,
        )
        .await?;
    ensure!(
        f.calls().len() == 1,
        "calls before release {}",
        f.calls().len()
    );
    held.answer(Answer::tool(
        "create_text_file",
        json!({"name": "saved.txt", "text": "Saved once"}),
    ));
    let done = f.turn(&root, |row| row["status"] == "done").await?;
    ensure!(
        done["reply"] == "Steered loop finished",
        "reply {}",
        done["reply"]
    );
    // B7: harness turns cannot take a steer, so the guidance is never
    // delivered ("Delivered" label, one copy per later request, history and
    // fork context); those assertions stay in the Node case.
    ensure!(
        done["generatedFiles"].as_array().map(Vec::len) == Some(1),
        "files"
    );
    let before = f.calls().len();
    f.app.restart().await?;
    f.app.post(&f.path, guidance.clone()).await?;
    ensure!(f.calls().len() == before, "replay ran the model");
    Ok(())
}

#[tokio::test]
async fn steer_before_final_answer_and_steer_after_completion_each_become_one_queued_turn(
) -> Result<()> {
    let mut f = Fixture::start(false).await?;
    let root = new_id();
    let text = format!("HOLD:{root}");
    f.app
        .post(&f.path, json!({"id": root, "text": text}))
        .await?;
    let held = f.held(&text).await?;
    let command = json!({"id": new_id(), "text": "Late safe point guidance", "dispatchMode": "steer", "targetTurn": root});
    f.app.post(&f.path, command.clone()).await?;
    held.answer(Answer::text("Held answer completed"));
    f.turn(&id(&command)?, |row| row["status"] == "done")
        .await?;
    ensure!(
        f.calls_with_last("Late safe point guidance") == 1,
        "guidance turns"
    );
    let mut late = command.clone();
    late["id"] = json!(new_id());
    late["text"] = json!("Already finished guidance");
    ensure!(
        f.app.post(&f.path, late.clone()).await?["dispatch_mode"] == "queue",
        "late steer not queued"
    );
    f.turn(&id(&late)?, |row| row["status"] == "done").await?;
    f.app.post(&f.path, late).await?;
    ensure!(
        f.calls_with_last("Already finished guidance") == 1,
        "late guidance turns"
    );
    let fork = f
        .app
        .post(&format!("{}/{root}/fork", f.path), json!({"id": new_id()}))
        .await?;
    let side = format!("/api/threads/{}/turns", id(&fork)?);
    let follow = new_id();
    f.app
        .post(
            &side,
            json!({"id": follow, "text": "Continue original answer"}),
        )
        .await?;
    f.app.wait_status(&side, &follow, "done").await?;
    ensure!(
        !mentions(f.calls().last(), "Late safe point guidance"),
        "fork saw the steer"
    );
    ensure!(
        !mentions(f.calls().last(), "Already finished guidance"),
        "fork saw the late steer"
    );
    Ok(())
}

#[tokio::test]
async fn restart_cancels_and_links_a_new_turn_carries_partial_text_and_saved_results_no_effect_replay(
) -> Result<()> {
    let mut f = Fixture::start(true).await?;
    let root = new_id();
    let text = format!("PARTIAL:{root}");
    f.app
        .post(&f.path, json!({"id": root, "text": text}))
        .await?;
    let held = f.held(&text).await?;
    let guidance = json!({"id": new_id(), "text": "Keep the delivered constraint", "dispatchMode": "steer", "targetTurn": root});
    f.app.post(&f.path, guidance).await?;
    held.answer(Answer::tool(
        "create_text_file",
        json!({"name": "effect.txt", "text": "Do not repeat"}),
    ));
    f.turn(&root, |row| {
        row["partial"]["text"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
    })
    .await?;
    let queued = new_id();
    f.app
        .post(
            &f.path,
            json!({"id": queued, "text": "Ordinary queued message"}),
        )
        .await?;
    let command = json!({"id": new_id(), "text": format!("NEW:{root}"), "dispatchMode": "restart", "targetTurn": root});
    let receipt = f.app.post(&f.path, command.clone()).await?;
    ensure!(
        receipt["dispatch_target"] == root.as_str(),
        "receipt mode {} target {} label {}",
        receipt["dispatch_mode"],
        receipt["dispatch_target"],
        receipt["dispatch_label"]
    );
    let done = f
        .turn(&id(&command)?, |row| row["status"] == "done")
        .await?;
    ensure!(
        done["reply"] == "Restart finished",
        "reply {}",
        done["reply"]
    );
    ensure!(
        done["activity"]["dispatch"][0]["source_turn"] == root.as_str(),
        "dispatch link {}",
        done["activity"]["dispatch"]
    );
    let cancelled = f.turn(&root, |row| row["status"] == "cancelled").await?;
    ensure!(
        cancelled["generatedFiles"].as_array().map(Vec::len) == Some(1),
        "files {}",
        cancelled["generatedFiles"]
    );
    let calls = f.calls();
    let position = calls
        .iter()
        .position(|request| last(request) == command["text"])
        .ok_or("no restart call")?;
    let restart = &calls[position];
    let transcript = restart.body["messages"].to_string();
    ensure!(
        transcript.contains("Partial answer before restart"),
        "partial text not carried: {}",
        Inspector::open(f.app.database())?
            .rows("SELECT context FROM dispatch_links", [])?
            .iter()
            .map(|row| row.to_string())
            .collect::<Vec<_>>()
            .join(" | ")
    );
    ensure!(
        transcript.contains("Do not repeat"),
        "saved result not carried"
    );
    // B7: the steer is stored but never delivered to a harness turn, so the
    // restart context cannot carry it; that assertion stays in Node.
    ensure!(
        transcript.contains("Never replay uncertain operations"),
        "no replay warning"
    );
    ensure!(
        !restart
            .messages()
            .iter()
            .any(|message| message["role"] == "tool"),
        "restart replayed tool messages"
    );
    f.turn(&queued, |row| row["status"] == "done").await?;
    let ordinary = f
        .calls()
        .iter()
        .position(|request| last(request) == "Ordinary queued message")
        .ok_or("no queued call")?;
    ensure!(position < ordinary, "restart ran after the queued message");
    let count = f.calls().len();
    f.app.restart().await?;
    ensure!(
        f.app.post(&f.path, command).await? == receipt,
        "restart receipt"
    );
    ensure!(f.calls().len() == count, "receipt replay ran the model");
    Ok(())
}

#[tokio::test]
async fn server_restart_mid_steer_releases_guidance_once_and_never_replays_uncertain_source(
) -> Result<()> {
    let mut f = Fixture::start(false).await?;
    let root = new_id();
    let text = format!("HOLD:{root}");
    f.app
        .post(&f.path, json!({"id": root, "text": text}))
        .await?;
    let _held = f.held(&text).await?;
    let command = json!({"id": new_id(), "text": "Survive server restart", "dispatchMode": "steer", "targetTurn": root});
    f.app.post(&f.path, command.clone()).await?;
    f.app.restart().await?;
    f.turn(&id(&command)?, |row| row["status"] == "done")
        .await?;
    f.turn(&root, |row| row["status"] == "interrupted").await?;
    f.app.post(&f.path, command).await?;
    f.app.restart().await?;
    ensure!(
        f.calls_with_last("Survive server restart") == 1,
        "guidance turns"
    );
    ensure!(f.calls_with_last(&text) == 1, "uncertain source replayed");
    Ok(())
}

#[tokio::test]
async fn fork_stores_bounded_immutable_cutoff_same_agent_idempotency_and_channel_lineage(
) -> Result<()> {
    let mut f = Fixture::start(false).await?;
    let (first, second) = (new_id(), new_id());
    f.app
        .post(&f.path, json!({"id": first, "text": "Fork source"}))
        .await?;
    f.turn(&first, |row| row["status"] == "done").await?;
    f.app
        .post(&f.path, json!({"id": second, "text": "Beyond cutoff"}))
        .await?;
    f.turn(&second, |row| row["status"] == "done").await?;
    let fork_path = format!("{}/{first}/fork", f.path);
    let command = json!({"id": new_id()});
    let fork = f.app.post(&fork_path, command.clone()).await?;
    let fork_id = id(&fork)?;
    ensure!(
        fork["agent"] == f.agent.as_str(),
        "fork agent {}",
        fork["agent"]
    );
    ensure!(
        fork["fork"]["cutoff_turn"] == first.as_str(),
        "cutoff {}",
        fork["fork"]
    );
    ensure!(fork["name"] == "New conversation", "name {}", fork["name"]);
    ensure!(
        fork["fork"]["source_name"] == "Main chat",
        "source {}",
        fork["fork"]
    );
    let inherited = f
        .app
        .get(&format!("/api/threads/{fork_id}/fork-context"))
        .await?;
    let turns: Vec<&Value> = inherited["turns"]
        .as_array()
        .ok_or("turns")?
        .iter()
        .map(|turn| &turn["id"])
        .collect();
    ensure!(turns == [&json!(first)], "inherited {turns:?}");
    ensure!(
        inherited["turns"][0]["text"] == "Fork source",
        "inherited text"
    );
    f.app
        .get_expect(&format!("/api/threads/{}/fork-context", new_id()), 404)
        .await?;
    ensure!(
        f.app.post(&fork_path, command.clone()).await? == fork,
        "fork replay"
    );
    f.app
        .post_expect(&format!("{}/{second}/fork", f.path), command.clone(), 409)
        .await?;
    let side = format!("/api/threads/{fork_id}/turns");
    let follow = new_id();
    f.app
        .post(&side, json!({"id": follow, "text": "Continue fork"}))
        .await?;
    f.app.wait_status(&side, &follow, "done").await?;
    ensure!(
        mentions(f.calls().last(), "Answer: Fork source"),
        "fork lost the source answer"
    );
    ensure!(
        !mentions(f.calls().last(), "Beyond cutoff"),
        "fork saw past the cutoff"
    );
    let threads = f
        .app
        .get(&format!("/api/agents/{}/threads", f.agent))
        .await?;
    ensure!(
        threads
            .as_array()
            .and_then(|threads| threads
                .iter()
                .find(|thread| thread["id"] == fork_id.as_str()))
            .is_some_and(|thread| thread["name"] == "Continue fork"),
        "fork not named from its first message"
    );
    let nested = f
        .app
        .post(&format!("{side}/{follow}/fork"), json!({"id": new_id()}))
        .await?;
    ensure!(
        nested["fork"]["source_name"] == "Continue fork",
        "nested source {}",
        nested["fork"]
    );
    f.app.restart().await?;
    ensure!(
        f.app.post(&fork_path, command).await? == fork,
        "fork receipt after restart"
    );
    ensure!(
        f.app
            .get(&format!("/api/threads/{fork_id}/fork-context"))
            .await?
            == inherited,
        "fork context changed"
    );
    let channel = id(&f
        .app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Channel fork", "instructions": "Coordinate.", "members": [f.agent], "lead": f.agent}),
        )
        .await?)?;
    let at = format!("/api/channels/{channel}/turns");
    let channel_turn = new_id();
    f.app
        .post(
            &at,
            json!({"id": channel_turn, "text": "Channel answer", "recipient": f.agent}),
        )
        .await?;
    f.app.wait_status(&at, &channel_turn, "done").await?;
    let channel_command = json!({"id": new_id()});
    let channel_fork = f
        .app
        .post(
            &format!("{at}/{channel_turn}/fork"),
            channel_command.clone(),
        )
        .await?;
    ensure!(
        channel_fork["fork"]["source_name"] == "Channel fork",
        "channel fork {}",
        channel_fork["fork"]
    );
    ensure!(
        channel_fork["agent"] == f.agent.as_str(),
        "channel fork agent"
    );
    ensure!(
        channel_fork["fork"]["source_kind"] == "channel",
        "source kind"
    );
    ensure!(
        channel_fork["fork"]["source_id"] == channel.as_str(),
        "source id"
    );
    ensure!(
        f.app
            .post(&format!("{at}/{channel_turn}/fork"), channel_command)
            .await?
            == channel_fork,
        "channel fork replay"
    );
    f.app
        .post_expect(
            &format!("{side}/{channel_turn}/fork"),
            json!({"id": new_id()}),
            404,
        )
        .await?;
    let cutoff = new_id();
    let (agent, cutoff_id) = (f.agent.clone(), cutoff.clone());
    f.app
        .edit_database(move |db: &Db| {
            for index in 0..45 {
                let turn = if index == 44 { cutoff_id.clone() } else { new_id() };
                db.connection().execute(
                    "INSERT INTO turns(id,agent,text,reply,status,created) VALUES (?,?,?,?,'done',?)",
                    (
                        turn,
                        &agent,
                        format!("Older {index}{}", "漢".repeat(5000)),
                        "漢".repeat(30000),
                        "2026-11-19T07:33:20.000Z",
                    ),
                )?;
            }
            Ok(())
        })
        .await?;
    let bounded = f
        .app
        .post(
            &format!("{}/{cutoff}/fork", f.path),
            json!({"id": new_id()}),
        )
        .await?;
    let transcript: String = Inspector::open(f.app.database())?.scalar(
        "SELECT transcript FROM context_transfers WHERE target_thread=?",
        [&id(&bounded)?],
    )?;
    ensure!(
        transcript.len() <= 128 * 1024,
        "transfer {} bytes",
        transcript.len()
    );
    let snapshot: Value = serde_json::from_str(&transcript)?;
    let snapshot_turns = snapshot["turns"].as_array().ok_or("snapshot turns")?;
    ensure!(
        snapshot_turns
            .last()
            .is_some_and(|turn| turn["id"] == cutoff.as_str()),
        "cutoff not last"
    );
    ensure!(
        snapshot_turns.len() <= 40,
        "snapshot turns {}",
        snapshot_turns.len()
    );
    ensure!(
        snapshot["truncated"] == true,
        "snapshot not marked truncated"
    );
    let _ = &mut f;
    Ok(())
}

#[tokio::test]
async fn restart_during_uncertain_mcp_effect_keeps_its_receipt_and_never_replays_external_call(
) -> Result<()> {
    let external = crate::fx_mcp::LosingMcp::start().await?;
    let model = GatedModel::start().await?;
    let connection = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let (target, failures) = (
        connection.clone(),
        std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new())),
    );
    let problems = failures.clone();
    model.respond(move |request| {
        let restart = last(request) == "Inspect uncertain result";
        if restart {
            let transcript = request.body["messages"].to_string().to_lowercase();
            if !transcript.contains("uncertain") && !transcript.contains("not confirmed") {
                if let Ok(mut problems) = problems.lock() {
                    problems.push("restart prompt does not mention the uncertain effect".into());
                }
            }
            return Answer::text("Stopped without replay");
        }
        let connection = target.lock().map(|value| value.clone()).unwrap_or_default();
        Answer::tool_id(
            "uncertain-effect",
            "call_mcp_tool",
            json!({"connection": connection, "name": "effect", "arguments": {}}),
        )
    });
    let root = TestRoot::new()?;
    let mut app = root
        .start(ServerConfig::new().gateway(Gateway::new(model.base_url())))
        .await?;
    let agent = id(&app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Uncertain restart", "model": "fake", "instructions": "Help."}),
        )
        .await?)?;
    let tested = app
        .post(
            "/api/mcp/test",
            json!({"id": new_id(), "name": "Effect fixture", "url": external.url(), "token": ""}),
        )
        .await?;
    let connection_id = id(&app
        .post("/api/mcp", json!({"id": new_id(), "test": tested["id"]}))
        .await?)?;
    if let Ok(mut target) = connection.lock() {
        target.clone_from(&connection_id);
    }
    app.post(
        &format!("/api/agents/{agent}/mcp/{connection_id}"),
        json!({"id": new_id(), "revision": 0, "connectionRevision": 1, "tools": ["effect"]}),
    )
    .await?;
    let path = format!("/api/agents/{agent}/turns");
    let root_turn = new_id();
    app.post(&path, json!({"id": root_turn, "text": "Begin effect"}))
        .await?;
    until("effect accepted", || async {
        Ok((external.effects() == 1).then_some(()))
    })
    .await?;
    let command = json!({"id": new_id(), "text": "Inspect uncertain result", "dispatchMode": "restart", "targetTurn": root_turn});
    app.post(&path, command.clone()).await?;
    app.wait_status(&path, &id(&command)?, "done").await?;
    app.restart().await?;
    app.post(&path, command.clone()).await?;
    ensure!(
        external.effects() == 1,
        "external effect replayed: {}",
        external.effects()
    );
    app.shutdown().await?;
    let db = Inspector::open(root.database())?;
    ensure!(
        db.scalar::<i64>("SELECT uncertain FROM mcp_calls WHERE turn=?", [&root_turn])? == 1,
        "call not marked uncertain"
    );
    ensure!(
        db.scalar::<String>(
            "SELECT context FROM dispatch_links WHERE target_turn=?",
            [&id(&command)?]
        )?
        .contains("uncertain"),
        "dispatch context lacks the uncertain effect"
    );
    ensure!(
        failures.lock().map(|f| f.is_empty()).unwrap_or(false),
        "{:?}",
        failures.lock().map(|f| f.clone())
    );
    Ok(())
}

#[tokio::test]
async fn queued_dispatch_rejects_stale_revision_held_editor_cross_conversation_target_and_started_message(
) -> Result<()> {
    let mut f = Fixture::start(false).await?;
    let root = new_id();
    let text = format!("HOLD:{root}");
    f.app
        .post(&f.path, json!({"id": root, "text": text}))
        .await?;
    let _held = f.held(&text).await?;
    let queued = new_id();
    f.app
        .post(
            &f.path,
            json!({"id": queued, "text": "Queue remains unchanged"}),
        )
        .await?;
    let command = || json!({"id": new_id(), "revision": 1, "mode": "steer", "targetTurn": root});
    let dispatch = format!("{}/{queued}/dispatch", f.path);
    let mut stale = command();
    stale["revision"] = json!(99);
    f.app.post_expect(&dispatch, stale, 409).await?;
    let other = id(&f
        .app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Other", "instructions": "Help.", "model": "fake"}),
        )
        .await?)?;
    let wrong = new_id();
    f.app
        .post(
            &format!("/api/agents/{other}/turns"),
            json!({"id": wrong, "text": format!("HOLD:{wrong}")}),
        )
        .await?;
    let mut foreign = command();
    foreign["targetTurn"] = json!(wrong);
    f.app.post_expect(&dispatch, foreign, 404).await?;
    let held = f
        .app
        .post(
            &format!("{}/{queued}/edit", f.path),
            json!({"id": new_id(), "editor": new_id(), "action": "hold", "revision": 1}),
        )
        .await?;
    let mut edited = command();
    edited["revision"] = held["revision"].clone();
    f.app.post_expect(&dispatch, edited, 409).await?;
    f.app
        .post_expect(&format!("{}/{root}/dispatch", f.path), command(), 409)
        .await?;
    ensure!(
        row(&f.app, &f.path, &queued).await?["dispatch_mode"] == "queue",
        "queued message changed"
    );
    Ok(())
}

#[tokio::test]
async fn queued_restart_cancels_captured_generation_late_steering_stays_queued_without_duplicate_turn(
) -> Result<()> {
    let mut f = Fixture::start(false).await?;
    let root = new_id();
    let text = format!("HOLD:{root}");
    f.app
        .post(&f.path, json!({"id": root, "text": text}))
        .await?;
    let _held = f.held(&text).await?;
    let queued = new_id();
    f.app
        .post(
            &f.path,
            json!({"id": queued, "text": "NEW:promoted restart"}),
        )
        .await?;
    let command = json!({"id": new_id(), "revision": 1, "mode": "restart", "targetTurn": root});
    let dispatch = format!("{}/{queued}/dispatch", f.path);
    f.app.post(&dispatch, command.clone()).await?;
    f.turn(&root, |row| row["status"] == "cancelled").await?;
    f.turn(&queued, |row| row["status"] == "done").await?;
    f.app.post(&dispatch, command).await?;
    ensure!(
        rows(&f.app, &f.path).await?.len() == 2,
        "restart replay created a turn"
    );
    let next = new_id();
    let hold = format!("HOLD:{next}");
    f.app
        .post(&f.path, json!({"id": next, "text": hold}))
        .await?;
    f.held(&hold)
        .await?
        .answer(Answer::text("Held answer completed"));
    f.turn(&next, |row| row["status"] == "done").await?;
    let barrier = new_id();
    let barrier_text = format!("HOLD:{barrier}");
    f.app
        .post(&f.path, json!({"id": barrier, "text": barrier_text}))
        .await?;
    let barrier_held = f.held(&barrier_text).await?;
    let late = new_id();
    f.app
        .post(&f.path, json!({"id": late, "text": "Late guidance"}))
        .await?;
    let response = f
        .app
        .post(
            &format!("{}/{late}/dispatch", f.path),
            json!({"id": new_id(), "revision": 1, "mode": "steer", "targetTurn": next}),
        )
        .await?;
    ensure!(
        response["dispatch_mode"] == "queue",
        "late steer {response}"
    );
    ensure!(
        response["dispatch_label"] == "Queued after active turn",
        "label {}",
        response["dispatch_label"]
    );
    barrier_held.answer(Answer::text("Held answer completed"));
    f.turn(&late, |row| row["status"] == "done").await?;
    ensure!(
        f.calls_with_last("Late guidance") == 1,
        "late guidance turns"
    );
    Ok(())
}

#[tokio::test]
async fn terminal_forks_preserve_partial_answers_untitled_chats_name_from_first_text_and_keep_manual_names(
) -> Result<()> {
    let mut f = Fixture::start(false).await?;
    for status in ["cancelled", "failed", "interrupted"] {
        let turn = new_id();
        let text = format!("Partial {status}");
        let (agent, turn_id, partial) = (f.agent.clone(), turn.clone(), text.clone());
        f.app
            .edit_database(move |db: &Db| {
                let created = "2026-11-19T07:33:20.000Z";
                db.connection().execute(
                    "INSERT INTO turns(id,agent,text,status,created) VALUES (?,?,?,?,?)",
                    (&turn_id, &agent, "Synthetic stopped task", status, created),
                )?;
                db.connection().execute(
                    "INSERT INTO provider_calls(turn,agent,model,origin,purpose,phase,status,created) VALUES (?,?,?,'http://synthetic.test','completion','answer',?,?)",
                    (&turn_id, &agent, "fake", if status == "cancelled" { "interrupted" } else { status }, created),
                )?;
                let call = db.connection().last_insert_rowid();
                db.connection().execute("INSERT INTO provider_streams(call,text) VALUES (?,?)", (call, &partial))?;
                Ok(())
            })
            .await?;
        let fork = f
            .app
            .post(&format!("{}/{turn}/fork", f.path), json!({"id": new_id()}))
            .await?;
        let context = f
            .app
            .get(&format!("/api/threads/{}/fork-context", id(&fork)?))
            .await?;
        ensure!(
            context["turns"]
                .as_array()
                .and_then(|turns| turns.last())
                .is_some_and(|turn| turn["reply"] == text.as_str()),
            "{status} fork context {context}"
        );
    }
    let thread = f
        .app
        .post(
            &format!("/api/agents/{}/threads", f.agent),
            json!({"id": new_id(), "name": "New conversation"}),
        )
        .await?;
    let thread_id = id(&thread)?;
    let path = format!("/api/threads/{thread_id}/turns");
    let first = new_id();
    f.app
        .post(&path, json!({"id": first, "text": "Plan launch"}))
        .await?;
    f.app.wait_status(&path, &first, "done").await?;
    let named = f.app.get(&format!("/api/threads/{thread_id}")).await?;
    ensure!(
        named["name"] == "Plan launch",
        "auto name {}",
        named["name"]
    );
    f.app
        .post(
            &format!("/api/threads/{thread_id}/edit"),
            json!({"id": new_id(), "revision": named["revision"], "archived": false, "name": "Manual name"}),
        )
        .await?;
    let second = new_id();
    f.app
        .post(&path, json!({"id": second, "text": "Another message"}))
        .await?;
    f.app.wait_status(&path, &second, "done").await?;
    ensure!(
        f.app.get(&format!("/api/threads/{thread_id}")).await?["name"] == "Manual name",
        "manual name replaced"
    );
    let _ = &mut f;
    Ok(())
}
