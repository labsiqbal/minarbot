//! Provider handoff keeps completed effects and question recovery (port of
//! `app/provider-handoff.test.ts`).
//!
//! The Node cases switched the server between provider families
//! (openai-compatible, anthropic, gemini) across a restart while a question
//! waited. #87 removed the families and their payload conversions (Gemini
//! thought signatures, `geminiParts`). What carries over is the handoff
//! itself: the agent's instance is re-pointed at a different model endpoint
//! across a restart, and the answered question resumes there with the full
//! scoped history, the saved artifact and no replay. One test per scope.

use crate::sb_support::{after_tool, id, settle};
use crate::support::after_question;
use minarbot_testkit::{
    control::{Answer, ControlModel},
    model::ModelRequest,
    prelude::*,
    Server,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering::SeqCst},
    Arc, Mutex,
};

#[derive(Clone, Copy)]
enum Scope {
    Direct,
    Channel,
    Side,
}

fn source(
    phase: &Arc<Mutex<&'static str>>,
    initial: &Arc<AtomicUsize>,
    request: &ModelRequest,
) -> Answer {
    // Product bug B1: one stray model step follows every question; it fails
    // at once (see `support::after_question_reply`).
    if after_question(request) {
        return Answer::Status(500, json!({"error": "no model step after a question (B1)"}));
    }
    let phase = phase.lock().map(|phase| *phase).unwrap_or("seed");
    if phase == "seed" {
        return Answer::text("Seed answer");
    }
    if after_tool(request) && request.called("create_text_file") && !request.called("ask_user") {
        initial.fetch_add(1, SeqCst);
        return Answer::tool(
            "ask_user",
            json!({"question": "Continue?", "choices": ["Yes"]}),
        );
    }
    if !after_tool(request) {
        initial.fetch_add(1, SeqCst);
        return Answer::tool(
            "create_text_file",
            json!({"name": "handoff.txt", "text": "Saved before switch"}),
        );
    }
    Answer::text("Seed answer")
}

async fn turn(app: &Server, path: &str, text: &str) -> Result<Value> {
    let id = new_id();
    app.post_expect(path, json!({"id": id, "text": text}), 202)
        .await?;
    settle(app, path, &id).await
}

async fn handoff(scope: Scope) -> Result<()> {
    let phase = Arc::new(Mutex::new("seed"));
    let initial = Arc::new(AtomicUsize::new(0));
    let first = ControlModel::start().await?;
    let (shared_phase, shared_initial) = (phase.clone(), initial.clone());
    first.respond(move |request| source(&shared_phase, &shared_initial, request));
    let target = ControlModel::start().await?;
    target.respond(|_| Answer::text("Resumed after switch"));
    let root = TestRoot::new()?;
    let mut app = root
        .start(ServerConfig::new().gateway(first.gateway()))
        .await?;
    let agent = id(&app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Stable identity", "model": "fixture", "instructions": "Help"}),
        )
        .await?)?;
    let side = id(&app
        .post(
            &format!("/api/agents/{agent}/threads"),
            json!({"id": new_id(), "name": "Private side"}),
        )
        .await?)?;
    let channel = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Shared", "instructions": "Help", "members": [agent], "lead": agent}),
        )
        .await?)?;
    let direct_path = format!("/api/agents/{agent}/turns");
    let side_path = format!("/api/threads/{side}/turns");
    let path = match scope {
        Scope::Channel => format!("/api/channels/{channel}/turns"),
        Scope::Side => side_path.clone(),
        Scope::Direct => direct_path.clone(),
    };
    let foreign_path = if matches!(scope, Scope::Side) {
        direct_path.clone()
    } else {
        side_path.clone()
    };
    let foreign = new_id();
    ensure!(
        turn(&app, &foreign_path, &foreign).await?["status"] == "done",
        "foreign turn"
    );
    ensure!(
        turn(&app, &path, "Keep original history").await?["status"] == "done",
        "seed turn"
    );
    *phase.lock().map_err(|_| "phase")? = "initial";
    let id_initial = new_id();
    app.post_expect(
        &path,
        json!({"id": id_initial, "text": "Save artifact and ask before continuing"}),
        202,
    )
    .await?;
    let waiting = app
        .wait_row(&path, &id_initial, |row| {
            row["paused"] == 1 || row["status"] == "failed"
        })
        .await?;
    ensure!(waiting["paused"] == 1, "question: {waiting}");
    ensure!(
        waiting["generatedFiles"].as_array().map(Vec::len) == Some(1),
        "artifact: {waiting}"
    );
    ensure!(
        initial.load(SeqCst) == 2,
        "initial steps: {}",
        initial.load(SeqCst)
    );
    let file = waiting["generatedFiles"][0].clone();
    app.shutdown().await?;
    app.start_stopped(ServerConfig::new().gateway(target.gateway()))
        .await?;
    ensure!(target.calls() == 0, "restart sent work to the new endpoint");
    let command = json!({"id": new_id(), "text": "Yes"});
    let answer_path = format!(
        "{path}/{id_initial}/questions/{}/answer",
        waiting["questions"][0]["id"].as_str().ok_or("question")?
    );
    app.post(&answer_path, command.clone()).await?;
    let done = settle(&app, &path, &id_initial).await?;
    ensure!(done["status"] == "done", "resumed: {done}");
    ensure!(done["reply"] == "Resumed after switch", "reply: {done}");
    ensure!(done["agent"] == agent.as_str(), "agent changed: {done}");
    ensure!(
        done["generatedFiles"] == json!([file]),
        "artifact changed: {done}"
    );
    ensure!(initial.load(SeqCst) == 2, "the old endpoint ran again");
    let sent_count = target.calls();
    ensure!(sent_count >= 1, "new endpoint requests: {sent_count}");
    let sent = target.requests()[0].body.to_string();
    for marker in [
        "Keep original history",
        "Seed answer",
        "handoff.txt",
        "Continue?",
        "Yes",
    ] {
        ensure!(sent.contains(marker), "handoff lost {marker}");
    }
    ensure!(
        !sent.contains(&foreign),
        "foreign history reached the new endpoint"
    );
    let scope_root = path.trim_end_matches("/turns");
    let download = app
        .request(
            minarbot_testkit::http::Method::GET,
            &format!("{scope_root}/files/{}", file["id"].as_str().ok_or("file")?),
        )?
        .send()
        .await?;
    ensure!(
        download.text().await? == "Saved before switch",
        "artifact bytes"
    );
    app.post(&answer_path, command).await?;
    ensure!(target.calls() == sent_count, "answer receipt replayed");
    ensure!(
        app.get(&path).await?.as_array().map(Vec::len) == Some(2),
        "scope turns"
    );
    ensure!(
        app.get(&foreign_path).await?.as_array().map(Vec::len) == Some(1),
        "foreign turns"
    );
    Ok(())
}

#[tokio::test]
async fn provider_handoff_in_direct_retains_completed_effects_and_question_recovery() -> Result<()>
{
    handoff(Scope::Direct).await
}

#[tokio::test]
async fn provider_handoff_in_channel_retains_completed_effects_and_question_recovery() -> Result<()>
{
    handoff(Scope::Channel).await
}

#[tokio::test]
async fn provider_handoff_in_side_retains_completed_effects_and_question_recovery() -> Result<()> {
    handoff(Scope::Side).await
}
