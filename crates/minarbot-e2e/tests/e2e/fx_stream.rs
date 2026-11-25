//! Room stream delivery (port of `app/turn-stream.test.ts`, runtime case).

use crate::fx_support::rows;
use minarbot_testkit::{
    db::Fixture,
    gated::{Answer, Chunk, Gate, GatedModel},
    http::SseReader,
    model::ToolCall,
    prelude::*,
    server::Gateway,
};
use std::sync::{Arc, Mutex};

/// `streamWork` from `app/src/ui/transcript.ts`: unfinished stream segments
/// that are tools or text not still shown as the live partial.
fn stream_work(turn: &Value) -> Vec<(String, String)> {
    turn["activity"]["stream"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|segment| {
            let live = turn["partial"]["seq"] == segment["seq"]
                && turn["partial"]["segment"] == segment["id"];
            segment["final"] != true
                && segment["final"] != 1
                && (segment["kind"] == "tool"
                    || (segment["text"].as_str().is_some_and(|text| !text.is_empty()) && !live))
        })
        .map(|segment| {
            (
                segment["kind"].as_str().unwrap_or_default().to_owned(),
                segment["text"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

/// Read snapshots until the root turn's partial text is `text`.
async fn wait_for_text(stream: &mut SseReader, root: &str, text: &str) -> Result<()> {
    loop {
        let snapshot = stream.next().await?.ok_or("stream ended before the snapshot")?;
        if snapshot["id"] == root && snapshot["partial"]["text"] == text {
            return Ok(());
        }
    }
}

#[tokio::test]
async fn room_polling_retains_active_members_beyond_full_history_and_orders_prose_tool_final_by_attempt(
) -> Result<()> {
    let (tool, draft_gate, answer) = (Gate::new(), Gate::new(), Gate::new());
    let (attempt_started, next_member_started) = (Gate::new(), Gate::new());
    let first_model: Arc<Mutex<Option<String>>> = Arc::default();
    let model = GatedModel::start().await?;
    let (tool2, draft2, answer2, attempt2, next2, first2) = (
        tool.clone(),
        draft_gate.clone(),
        answer.clone(),
        attempt_started.clone(),
        next_member_started.clone(),
        first_model.clone(),
    );
    model.respond(move |request| {
        let Ok(mut first) = first2.lock() else {
            return Answer::Status(500, json!({}));
        };
        let last = request.messages().last().cloned().unwrap_or(Value::Null);
        if first.is_none() && last["content"] == "Gated room" {
            *first = Some(request.model().to_owned());
            return Answer::Stream(
                vec![
                    Chunk::Text("Before tool.".into()),
                    Chunk::Wait(tool2.clone()),
                    Chunk::Tool(ToolCall::new(
                        "room-file",
                        "create_text_file",
                        json!({"name": "room.txt", "text": "Once"}),
                    )),
                ],
                "tool_calls".into(),
            );
        }
        if request.last_user_text().starts_with("One verification reminder") {
            return Answer::Stream(vec![Chunk::Text("Draft answer.".into())], "stop".into());
        }
        if last["role"] == "tool" {
            attempt2.open();
            return Answer::Stream(
                vec![
                    Chunk::Wait(draft2.clone()),
                    Chunk::Text("Draft".into()),
                    Chunk::Wait(answer2.clone()),
                    Chunk::Text(" answer.".into()),
                ],
                "stop".into(),
            );
        }
        if first.as_deref() != Some(request.model()) {
            next2.open();
        }
        Answer::Stream(vec![Chunk::Text("NO_REPLY".into())], "stop".into())
    });
    let root = TestRoot::new()?;
    let app = root
        .start(ServerConfig::new().gateway(Gateway {
            stream: Some(true),
            ..Gateway::new(model.base_url())
        }))
        .await?;
    let agents: Vec<String> = (0..3).map(|_| new_id()).collect();
    for (index, agent) in agents.iter().enumerate() {
        app.post(
            "/api/agents",
            json!({"runtime": "api", "id": agent, "name": format!("Member {index}"),
                "model": format!("fixture-{index}"), "instructions": "Help"}),
        )
        .await?;
    }
    let (channel, other) = (new_id(), new_id());
    for room in [&channel, &other] {
        app.post(
            "/api/channels",
            json!({"id": room, "name": room, "members": agents, "lead": null, "instructions": "Help"}),
        )
        .await?;
    }
    let path = format!("/api/channels/{channel}/turns");
    let root_turn = new_id();
    let mut stream = SseReader::connect(&app, &format!("{path}/stream")).await?;
    app.post(&path, json!({"id": root_turn, "text": "Gated room"})).await?;
    wait_for_text(&mut stream, &root_turn, "Before tool.").await?;
    let current = rows(&app, &path).await?;
    ensure!(current.len() == 3, "room rows {}", current.len());
    ensure!(
        current.iter().filter(|row| row["partial"]["text"] == "Before tool.").count() == 1,
        "partial rows"
    );
    ensure!(model.calls() == 1, "model calls {}", model.calls());
    ensure!(
        current.iter().filter(|row| row["status"] == "running").count() == 1,
        "running members"
    );
    ensure!(
        current
            .iter()
            .filter(|row| row["status"] == "queued" && row["partial"].is_null())
            .count()
            == 2,
        "queued members"
    );
    {
        let db = Fixture::open(root.database())?;
        db.execute_batch("BEGIN IMMEDIATE")?;
        for index in 0..105 {
            let agent = &agents[index % agents.len()];
            db.connection().execute(
                "INSERT INTO turns(id,agent,channel,recipient,text,status,paused,created) VALUES (?,?,?,?,?,'queued',1,?)",
                (new_id(), agent, &channel, agent, format!("Queued {index}"), "2026-01-01T00:00:00.000Z"),
            )?;
        }
        db.execute_batch("COMMIT")?;
    }
    let current = rows(&app, &path).await?;
    ensure!(current.len() == 108, "rows with history {}", current.len());
    ensure!(
        current.iter().filter(|row| row["room"]["root"] == root_turn.as_str()).count() == 3,
        "active room rows"
    );
    ensure!(
        current.iter().filter(|row| row["history_page"].as_i64().is_some_and(|page| page != 0)).count() == 100,
        "history page rows"
    );
    let page_seq = current
        .iter()
        .find(|row| row["history_page"].as_i64().is_some_and(|page| page != 0))
        .and_then(|row| row["seq"].as_i64())
        .ok_or("history row")?;
    ensure!(
        page_seq > current[0]["seq"].as_i64().unwrap_or(i64::MAX),
        "Active rows must not become the history cursor"
    );
    let oldest = current.last().and_then(|row| row["seq"].as_i64()).ok_or("oldest seq")?;
    ensure!(
        rows(&app, &format!("{path}?before={}", oldest + 1)).await?.len() == 100,
        "older page"
    );
    ensure!(rows(&app, &format!("{path}?q=unrelated")).await?.is_empty(), "search leak");
    ensure!(
        rows(&app, &format!("/api/channels/{other}/turns")).await?.is_empty(),
        "other room leak"
    );
    let member = current
        .iter()
        .find(|row| row["id"] == root_turn.as_str())
        .ok_or("root row")?;
    let before = member["partial"].clone();
    let discussion = format!("{path}?q=Gated%20room");
    let find = |rows: Vec<Value>| -> Result<Value> {
        rows.into_iter()
            .find(|row| row["id"] == root_turn.as_str())
            .ok_or_else(|| "root row missing".into())
    };
    tool.open();
    attempt_started.wait().await;
    let empty = find(rows(&app, &discussion).await?)?;
    // #87: a harness turn is one provider call record, so a new model step
    // shows as a new stream segment of the same `seq`, not a new attempt.
    ensure!(
        empty["partial"]["seq"] == before["seq"] && empty["partial"]["segment"] != before["segment"],
        "new step partial {} after {before}",
        empty["partial"]
    );
    ensure!(empty["partial"]["text"] == "", "empty attempt text {}", empty["partial"]);
    ensure!(empty["partial"]["status"] == "pending", "attempt status {}", empty["partial"]);
    ensure!(
        empty["partial"]["revision"].as_i64() > before["revision"].as_i64(),
        "attempt revision {}",
        empty["partial"]
    );
    ensure!(
        stream_work(&empty)
            == [("text".to_owned(), "Before tool.".to_owned()), ("tool".to_owned(), "create_text_file".to_owned())],
        "work before draft {:?}",
        stream_work(&empty)
    );
    draft_gate.open();
    wait_for_text(&mut stream, &root_turn, "Draft").await?;
    let draft = find(rows(&app, &discussion).await?)?;
    let count = model.calls();
    ensure!(draft["partial"]["text"] == "Draft", "draft {}", draft["partial"]);
    ensure!(draft["partial"]["final"] == false, "draft final {}", draft["partial"]);
    ensure!(
        find(rows(&app, &discussion).await?)?["partial"] == draft["partial"],
        "poll regressed the draft"
    );
    ensure!(model.calls() == count, "polling called the model");
    answer.open();
    // A second room member starts only after the first member's result commits.
    next_member_started.wait().await;
    let done = find(rows(&app, &discussion).await?)?;
    ensure!(done["status"] == "done", "status {}", done["status"]);
    ensure!(done["reply"] == "Draft answer.", "reply {}", done["reply"]);
    ensure!(done["partial"].is_null(), "partial after done {}", done["partial"]);
    ensure!(done["generatedFiles"].as_array().map(Vec::len) == Some(1), "files");
    let segments: Vec<Value> = done["activity"]["stream"]
        .as_array()
        .ok_or("stream")?
        .iter()
        .map(|item| json!([item["seq"], item["ordinal"], item["kind"], item["final"]]))
        .collect();
    let last_seq = done["activity"]["stream"]
        .as_array()
        .and_then(|items| items.last())
        .map(|item| item["seq"].clone())
        .unwrap_or(Value::Null);
    // #87: one harness call holds every step, so the order is by ordinal
    // within that call instead of by provider attempt.
    ensure!(
        segments
            == vec![
                json!([before["seq"], 0, "text", 0]),
                json!([before["seq"], 1, "tool", 0]),
                json!([draft["partial"]["seq"], 2, "text", 0]),
                json!([last_seq, 3, "text", 1]),
            ],
        "segments {segments:?}"
    );
    let work: Vec<String> = stream_work(&done).into_iter().map(|(_, text)| text).collect();
    ensure!(work == ["Before tool.", "create_text_file", "Draft answer."], "work {work:?}");
    ensure!(
        done["activity"]["stream"]
            .as_array()
            .and_then(|items| items.last())
            .is_some_and(|item| item["text"] == "Draft answer."),
        "final segment text"
    );
    let restored = find(rows(&app, &discussion).await?)?;
    ensure!(restored["activity"]["stream"] == done["activity"]["stream"], "stream changed");
    Ok(())
}
