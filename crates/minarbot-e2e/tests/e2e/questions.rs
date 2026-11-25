//! Question cards: persistence, scoped answers and resume without repeated
//! effects (port of `app/questions.test.ts`).

use minarbot_testkit::{
    model::{ModelRequest, Reply, ScriptedModel},
    prelude::*,
    Server,
};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Script {
    free: bool,
    invalid: bool,
    asks: usize,
    replies: Vec<String>,
    prompts: Vec<String>,
    rejections: Vec<String>,
}

/// First step saves a memory, second asks a question, third reads the answer.
fn question_model(model: &ScriptedModel, script: Arc<Mutex<Script>>) {
    model.respond(move |request: &ModelRequest| {
        let Ok(mut script) = script.lock() else {
            return Reply::status(500, json!({"error": "script poisoned"}));
        };
        script.prompts.push(request.body["messages"].to_string());
        let calls = request.tool_calls();
        if calls.is_empty() {
            return Reply::tool_call(
                "effect",
                "remember_memory",
                json!({"text": format!("Saved before question {}", request.last_user_text())}),
            );
        }
        if !request.called("ask_user") {
            script.asks += 1;
            let choices = if script.invalid {
                json!(["duplicate", "duplicate"])
            } else if script.free {
                json!([])
            } else {
                json!(["Yes", "No"])
            };
            return Reply::tool_call(
                "question",
                "ask_user",
                json!({"question": "<script>Which path?</script>", "choices": choices}),
            );
        }
        if calls[0]["function"]["name"] != "remember_memory" {
            return Reply::status(500, json!({"error": "first call was not remember_memory"}));
        }
        let result = request
            .tool_results()
            .last()
            .and_then(|message| message["content"].as_str())
            .and_then(|content| serde_json::from_str::<Value>(content).ok())
            .unwrap_or(Value::Null);
        if let Some(error) = result["error"].as_str() {
            script.rejections.push(error.to_owned());
            return Reply::text("Question rejected.");
        }
        let answer = Some(result["answer"].clone());
        match answer.and_then(|answer| answer.as_str().map(str::to_owned)) {
            Some(answer) => {
                script.replies.push(answer.clone());
                Reply::text(format!("User chose {answer}"))
            }
            None => Reply::status(
                500,
                json!({"error": "ask_user result has no answer",
                "results": request.tool_results()}),
            ),
        }
    });
}

fn answer_path(path: &str, turn: &Value) -> String {
    format!(
        "{path}/{}/questions/{}/answer",
        turn["id"].as_str().unwrap_or_default(),
        turn["questions"][0]["id"].as_str().unwrap_or_default()
    )
}

/// Start a turn that saves a memory and asks; returns the paused turn.
async fn ask(app: &Server, path: &str) -> Result<Value> {
    let id = new_id();
    let suffix = if path.starts_with("/api/channels/") {
        " @A"
    } else {
        ""
    };
    app.post(path, json!({"id": id, "text": format!("{id}{suffix}")}))
        .await?;
    let turn = app
        .wait_row(path, &id, |row| {
            row["questions"]
                .as_array()
                .is_some_and(|questions| !questions.is_empty())
        })
        .await?;
    ensure!(
        turn["status"] == "queued" && turn["paused"] == 1,
        "question turn: {turn}"
    );
    Ok(turn)
}

#[tokio::test]
async fn question_cards_persist_answer_once_in_scope_and_resume_without_repeating_effects(
) -> Result<()> {
    let script = Arc::new(Mutex::new(Script::default()));
    let state = |script: &Arc<Mutex<Script>>| -> Result<(usize, Option<String>, Option<String>)> {
        let script = script.lock().map_err(|_| "script poisoned")?;
        Ok((
            script.asks,
            script.replies.last().cloned(),
            script.prompts.last().cloned(),
        ))
    };
    let model = ScriptedModel::start().await?;
    question_model(&model, script.clone());
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new().model(&model)).await?;
    let agent = |name: &str| json!({"runtime": "api", "id": new_id(), "name": name, "model": "fixture", "instructions": "Help"});
    let a = app.post("/api/agents", agent("A")).await?;
    let b = app.post("/api/agents", agent("B")).await?;
    let (a_id, b_id) = (a["id"].as_str().ok_or("a")?, b["id"].as_str().ok_or("b")?);
    let c = app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "C", "instructions": "Help", "lead": a_id, "members": [a_id, b_id]}),
        )
        .await?;
    let c_id = c["id"].as_str().ok_or("c")?;
    let side = app
        .post(
            &format!("/api/agents/{a_id}/threads"),
            json!({"id": new_id(), "name": "Side"}),
        )
        .await?;
    let direct = format!("/api/agents/{a_id}/turns");
    let channel = format!("/api/channels/{c_id}/turns");
    let thread = format!("/api/threads/{}/turns", side["id"].as_str().ok_or("side")?);

    let first = ask(&app, &direct).await?;
    let first_id = first["id"].as_str().ok_or("first")?;
    let (original_asks, _, _) = state(&script)?;
    app.post_expect(
        &format!("{direct}/{first_id}/resume"),
        json!({"id": new_id(), "revision": first["revision"], "acknowledgeUncertain": true}),
        409,
    )
    .await?;
    app.post_expect(
        &answer_path(&format!("/api/agents/{b_id}/turns"), &first),
        json!({"id": new_id(), "text": "Yes"}),
        404,
    )
    .await?;
    app.post_expect(
        &answer_path(&channel, &first),
        json!({"id": new_id(), "text": "Yes"}),
        404,
    )
    .await?;
    app.post_expect(
        &answer_path(&direct, &first),
        json!({"id": new_id(), "text": "Wrong choice"}),
        400,
    )
    .await?;
    app.restart().await?;
    let waiting = app
        .row(&direct, first_id)
        .await?
        .ok_or("first turn missing")?;
    ensure!(
        waiting["questions"][0]["answer"].is_null(),
        "answer before reply: {waiting}"
    );
    ensure!(state(&script)?.0 == original_asks, "restart asked again");
    let command = json!({"id": new_id(), "text": "Yes"});
    let endpoint = answer_path(&direct, &first);
    let receipt = app.post(&endpoint, command.clone()).await?;
    ensure!(
        app.post(&endpoint, command.clone()).await? == receipt,
        "answer replay differs"
    );
    app.wait_status(&direct, first_id, "done").await?;
    let (asks, reply, _) = state(&script)?;
    ensure!(
        reply.as_deref() == Some("Yes") && asks == original_asks,
        "resume: asks {asks}, reply {reply:?}"
    );
    let memory = app.get(&format!("/api/agents/{a_id}/memory")).await?;
    ensure!(
        memory["items"].as_array().map(Vec::len) == Some(1),
        "memory effect repeated: {memory}"
    );
    app.post_expect(&endpoint, json!({"id": new_id(), "text": "No"}), 409)
        .await?;
    app.post_expect(&endpoint, json!({"id": command["id"], "text": "No"}), 409)
        .await?;
    app.restart().await?;
    ensure!(
        app.post(&endpoint, command).await? == receipt,
        "answer receipt lost on restart"
    );

    for path in [&channel, &thread] {
        let turn = ask(&app, path).await?;
        app.post_expect(
            &answer_path(&direct, &turn),
            json!({"id": new_id(), "text": "No"}),
            404,
        )
        .await?;
        app.post(
            &answer_path(path, &turn),
            json!({"id": new_id(), "text": "No"}),
        )
        .await?;
        app.wait_status(path, turn["id"].as_str().ok_or("turn")?, "done")
            .await?;
    }

    script.lock().map_err(|_| "poisoned")?.free = true;
    let text_turn = ask(&app, &direct).await?;
    let (_, _, prompt) = state(&script)?;
    let prompt = prompt.unwrap_or_default();
    ensure!(
        prompt.contains("User question cards (reference data)") && prompt.contains("Yes"),
        "free-text prompt: {prompt}"
    );
    app.post(
        &answer_path(&direct, &text_turn),
        json!({"id": new_id(), "text": "Custom answer"}),
    )
    .await?;
    app.wait_status(&direct, text_turn["id"].as_str().ok_or("turn")?, "done")
        .await?;
    ensure!(
        state(&script)?.1.as_deref() == Some("Custom answer"),
        "custom answer not delivered"
    );

    let cancelled = ask(&app, &direct).await?;
    app.post(
        &format!(
            "{direct}/{}/cancel",
            cancelled["id"].as_str().ok_or("turn")?
        ),
        json!({"id": new_id()}),
    )
    .await?;
    app.post_expect(
        &answer_path(&direct, &cancelled),
        json!({"id": new_id(), "text": "Too late"}),
        409,
    )
    .await?;
    let archived = ask(&app, &channel).await?;
    app.post(
        &format!("/api/channels/{c_id}/edit"),
        json!({"id": new_id(), "revision": 1, "name": "C", "instructions": "Help", "members": [a_id, b_id], "lead": a_id, "archived": true}),
    )
    .await?;
    app.post_expect(
        &answer_path(&channel, &archived),
        json!({"id": new_id(), "text": "Late"}),
        409,
    )
    .await?;

    let restored = ask(&app, &direct).await?;
    let restored_id = restored["id"].as_str().ok_or("turn")?.to_owned();
    app.edit_database(|db| {
        db.execute(
            "UPDATE turns SET status='interrupted',revision=revision+1 WHERE id=?",
            [&restored_id],
        )?;
        Ok(())
    })
    .await?;
    app.post_expect(
        &answer_path(&direct, &restored),
        json!({"id": new_id(), "text": "Restored answer"}),
        409,
    )
    .await?;
    app.post(
        &answer_path(&direct, &restored),
        json!({"id": new_id(), "text": "Restored answer", "acknowledgeUncertain": true}),
    )
    .await?;
    app.wait_status(&direct, &restored_id, "done").await?;

    let atomic = ask(&app, &direct).await?;
    let atomic_id = atomic["id"].as_str().ok_or("turn")?;
    app.edit_database(|db| {
        db.execute_batch("CREATE TRIGGER reject_answer BEFORE INSERT ON question_answers BEGIN SELECT RAISE(ABORT, 'synthetic answer receipt failure'); END")
    })
    .await?;
    let retry = json!({"id": new_id(), "text": "Atomic answer"});
    app.post_expect(&answer_path(&direct, &atomic), retry.clone(), 500)
        .await?;
    let untouched = app
        .row(&direct, atomic_id)
        .await?
        .ok_or("atomic turn missing")?;
    ensure!(
        untouched["paused"] == 1 && untouched["questions"][0]["answer"].is_null(),
        "failed answer changed the turn: {untouched}"
    );
    app.edit_database(|db| db.execute_batch("DROP TRIGGER reject_answer"))
        .await?;
    app.post(&answer_path(&direct, &atomic), retry).await?;
    app.wait_status(&direct, atomic_id, "done").await?;
    ensure!(
        state(&script)?.1.as_deref() == Some("Atomic answer"),
        "atomic retry answer"
    );

    // Before #87 the API runtime failed the turn on an invalid question.
    // Harness turns get the rejection as an MCP tool error instead; the
    // question card must still never be stored.
    script.lock().map_err(|_| "poisoned")?.invalid = true;
    let bad = new_id();
    app.post(&direct, json!({"id": bad, "text": bad})).await?;
    let finished = app
        .wait_row(&direct, &bad, |row| {
            ["done", "failed"].contains(&row["status"].as_str().unwrap_or_default())
        })
        .await?;
    ensure!(
        finished["questions"] == json!([]),
        "invalid question stored: {finished}"
    );
    let rejections = script.lock().map_err(|_| "poisoned")?.rejections.clone();
    ensure!(
        rejections.len() == 1 && rejections[0].to_lowercase().contains("question"),
        "invalid question rejection: {rejections:?}"
    );
    Ok(())
}
