//! The done check: one verification nudge, checks before the final answer,
//! evidence kept out of the reply (port of `app/self-verification.test.ts`).

use crate::sa_support::{lock, tool_json, Shared};
use minarbot_testkit::{
    model::{Reply, ScriptedModel},
    prelude::*,
};

const REMINDER: &str = "One verification reminder";

#[derive(Default)]
struct Script {
    agent: String,
    calls: Vec<String>,
    errors: Vec<String>,
    file: String,
    checked: bool,
    reviewed: bool,
}

fn step(state: &mut Script, request: &minarbot_testkit::model::ModelRequest) -> Reply {
    let messages = request.messages();
    let last = messages.last().cloned().unwrap_or(Value::Null);
    // Harness turns get the reminder as the follow-up user prompt, not as
    // the system message the API runtime rewrote.
    if last["role"] == "user" && request.last_user_text().starts_with(REMINDER) && !state.checked {
        state.calls.push("nudge".into());
        state.checked = true;
        return Reply::tool("read_workspace_file", json!({"path": "result.txt"}));
    }
    if last["role"] == "tool" {
        let result = tool_json(&last);
        let name = messages
            .iter()
            .rev()
            .find_map(|message| message["tool_calls"][0]["function"]["name"].as_str())
            .unwrap_or_default()
            .to_owned();
        state.calls.push(name.clone());
        if !result["error"].is_null() {
            state.errors.push(format!("{name}: {result}"));
        }
        return match name.as_str() {
            "create_text_file" => {
                result["id"]
                    .as_str()
                    .unwrap_or_default()
                    .clone_into(&mut state.file);
                Reply::text("Done.")
            }
            "read_workspace_file" => {
                if result["text"] != "Requested result" {
                    state.errors.push(format!("read: {result}"));
                }
                state.checked = true;
                Reply::tool(
                    "verify_result",
                    json!({"kind": "file",
                        "review": "Reread request: result.txt contains the requested result."}),
                )
            }
            _ => {
                if result["verified"] != true {
                    state.errors.push(format!("verify_result: {result}"));
                }
                state.reviewed = true;
                Reply::text(format!(
                    "Saved [result.txt](/api/agents/{}/files/{}).",
                    state.agent, state.file
                ))
            }
        };
    }
    Reply::tool(
        "create_text_file",
        json!({"name": "result.txt", "text": "Requested result"}),
    )
}

#[tokio::test]
async fn synthetic_done_without_verification_gets_one_nudge_checks_precede_final_evidence_stays_outside_reply(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    let script: Shared<Script> = Shared::default();
    let state = script.clone();
    model.respond(move |request| match state.lock() {
        Ok(mut state) => step(&mut state, request),
        Err(_) => Reply::status(500, json!({})),
    });
    let root = TestRoot::new()?;
    let config = ServerConfig::new().model(&model);
    let mut app = root.start(config.clone()).await?;
    let agent = new_id();
    lock(&script)?.agent.clone_from(&agent);
    app.post(
        "/api/agents",
        json!({"id": agent, "runtime": "api", "name": "Verify", "model": "fixture",
            "instructions": "Work."}),
    )
    .await?;
    let path = format!("/api/agents/{agent}/turns");
    let turn = new_id();
    app.post(
        &path,
        json!({"id": turn, "text": "Create result.txt with the requested result."}),
    )
    .await?;
    let row = app
        .wait_row(&path, &turn, |row| {
            row["status"] == "done" || row["status"] == "failed"
        })
        .await?;
    ensure!(row["status"] == "done", "turn: {row}");
    {
        let state = lock(&script)?;
        ensure!(state.checked && state.reviewed, "calls: {:?}", state.calls);
        ensure!(state.errors.is_empty(), "tool errors: {:?}", state.errors);
        ensure!(
            state.calls
                == [
                    "create_text_file",
                    "nudge",
                    "read_workspace_file",
                    "verify_result"
                ],
            "calls: {:?}",
            state.calls
        );
    }
    let reply = row["reply"].as_str().ok_or("reply")?.to_owned();
    ensure!(reply.starts_with("Saved [result.txt]"), "reply: {reply}");
    ensure!(
        !reply.contains("test-summary"),
        "evidence in reply: {reply}"
    );
    app.restart_with(config).await?;
    let restored = app.row(&path, &turn).await?.ok_or("restored turn")?;
    ensure!(
        restored["generatedFiles"].as_array().map(Vec::len) == Some(1),
        "generated files: {restored}"
    );
    ensure!(restored["reply"] == reply.as_str(), "restored: {restored}");
    Ok(())
}
