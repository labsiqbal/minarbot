//! Agent history tools (port of `app/history-tools.test.ts`).

use minarbot_testkit::{
    db::Fixture,
    model::ModelRequest,
    parked::{Answer, ParkedModel, Route},
    prelude::*,
    server::Gateway,
};
use std::sync::{Arc, Mutex};

const COMPACTION: &str = "Summarize conversation history";

#[derive(Clone, Default)]
struct Plan {
    mode: &'static str,
    oldest: String,
    before: i64,
    attachment: String,
    foreign: String,
    /// Park the next answer-phase request under `during`.
    during: bool,
}

fn compaction(request: &ModelRequest) -> bool {
    request.system().starts_with(COMPACTION)
}

fn disclosure(request: &ModelRequest) -> bool {
    request.last_text().starts_with("Future disclosure")
}

fn answer(plan: &Plan, request: &ModelRequest) -> Answer {
    if compaction(request) {
        return Answer::text("Earlier details remain available through history tools.");
    }
    if disclosure(request) {
        return Answer::text("Saved.");
    }
    let steps: Vec<(&str, Value)> = match plan.mode {
        "read" => vec![
            ("read_history", json!({"before": plan.before})),
            ("read_message", json!({"id": plan.oldest})),
            ("read_message", json!({"id": plan.oldest, "offset": 8000})),
        ],
        "attachment" => vec![
            ("read_attachment", json!({"id": plan.attachment})),
            (
                "read_attachment",
                json!({"id": plan.attachment, "offset": 8000}),
            ),
        ],
        "denied-attachment" => vec![("read_attachment", json!({"id": plan.attachment}))],
        "foreign" => vec![("read_message", json!({"id": plan.foreign}))],
        "invalid" => vec![("read_history", json!({"before": -1}))],
        _ => (0..9).map(|_| ("read_history", json!({}))).collect(),
    };
    match steps.into_iter().nth(request.tool_results().len()) {
        Some((name, arguments)) => Answer::tool(name, arguments),
        None => Answer::text("Read complete."),
    }
}

struct Run {
    app: Server,
    model: ParkedModel,
    plan: Arc<Mutex<Plan>>,
}

impl Run {
    fn change(&self, change: impl FnOnce(&mut Plan)) {
        if let Ok(mut plan) = self.plan.lock() {
            change(&mut plan);
        }
    }

    /// Run one turn in `path`; returns the parsed tool results in order.
    async fn run(&self, path: &str, during: Option<(String, Value)>) -> Result<Vec<Value>> {
        let first = self.model.calls();
        let id = new_id();
        self.app
            .post(path, json!({"id": id, "text": "Retrieve earlier evidence"}))
            .await?;
        if let Some((action, body)) = during {
            self.model.wait_parked(&["during"]).await?;
            self.app.post_expect(&action, body, 202).await?;
            let plan = self
                .plan
                .lock()
                .map(|plan| plan.clone())
                .unwrap_or_default();
            let parked = self
                .model
                .parked_request("during")
                .ok_or("parked request")?;
            self.model.finish("during", answer(&plan, &parked))?;
        }
        let turn = self
            .app
            .wait_row(path, &id, |row| {
                ["done", "failed"].contains(&row["status"].as_str().unwrap_or(""))
            })
            .await?;
        let mode = self.plan.lock().map(|plan| plan.mode).unwrap_or("");
        ensure!(
            turn["status"] == "done",
            "mode {mode}: {} {}",
            turn["status"],
            turn["error"]
        );
        let requests: Vec<ModelRequest> = self
            .model
            .requests()
            .split_off(first)
            .into_iter()
            .filter(|request| !compaction(request) && !disclosure(request))
            .collect();
        let mut results = Vec::new();
        for request in &requests {
            ensure!(
                request
                    .tool_names()
                    .iter()
                    .any(|name| name == "read_history"),
                "read_history not offered: {:?}",
                request.tool_names()
            );
            match request.tool_results().last() {
                None => ensure!(
                    !request.mentions("OLD_ORIGINAL_MARKER"),
                    "mode {mode}: the old original reached the context"
                ),
                Some(result) => results.push(serde_json::from_str(
                    result["content"].as_str().unwrap_or(""),
                )?),
            }
        }
        Ok(results)
    }
}

fn not_found(results: &[Value]) -> bool {
    results
        .first()
        .and_then(|result| result["error"].as_str())
        .is_some_and(|error| error.contains("not found"))
}

#[tokio::test]
async fn agent_history_tools_retrieve_old_originals_and_reject_foreign_conversations() -> Result<()>
{
    let plan = Arc::new(Mutex::new(Plan {
        mode: "read",
        before: 1,
        ..Plan::default()
    }));
    let routed = plan.clone();
    let model = ParkedModel::start(move |request| {
        let Ok(mut plan) = routed.lock() else {
            return Route::Now(Answer::fail("poisoned"));
        };
        if plan.during && !compaction(request) && !disclosure(request) {
            plan.during = false;
            return Route::Park("during".into());
        }
        Route::Now(answer(&plan, request))
    })
    .await?;
    let root = TestRoot::new()?;
    let mut app = root
        .start(ServerConfig::new().gateway(Gateway::new(model.base_url())))
        .await?;
    let agent = app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Reader", "model": "fixture",
                "instructions": "Help."}),
        )
        .await?;
    let agent_id = agent["id"].as_str().ok_or("agent id")?.to_owned();
    let channel = app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Room", "instructions": "Help.", "lead": agent_id,
                "members": [agent_id]}),
        )
        .await?;
    let channel_id = channel["id"].as_str().ok_or("channel id")?.to_owned();
    let side_a = app
        .post(
            &format!("/api/agents/{agent_id}/threads"),
            json!({"id": new_id(), "name": "A"}),
        )
        .await?;
    let side_b = app
        .post(
            &format!("/api/agents/{agent_id}/threads"),
            json!({"id": new_id(), "name": "B"}),
        )
        .await?;
    let side_a = side_a["id"].as_str().ok_or("A")?.to_owned();
    let side_b = side_b["id"].as_str().ok_or("B")?.to_owned();
    // (turns path, channel, thread)
    let scopes: Vec<(String, Option<String>, Option<String>)> = vec![
        (format!("/api/agents/{agent_id}/turns"), None, None),
        (
            format!("/api/channels/{channel_id}/turns"),
            Some(channel_id.clone()),
            None,
        ),
        (
            format!("/api/threads/{side_a}/turns"),
            None,
            Some(side_a.clone()),
        ),
        (
            format!("/api/threads/{side_b}/turns"),
            None,
            Some(side_b.clone()),
        ),
    ];
    let upload = |path: String, body: Vec<u8>, name: &'static str| {
        let request = app.client().map(|client| {
            client
                .post(app.url(&path))
                .header("Origin", app.origin())
                .header("Content-Type", "application/octet-stream")
                .header("X-File-Name", name)
                .body(body)
        });
        async move { Ok::<u16, minarbot_testkit::Error>(request?.send().await?.status().as_u16()) }
    };
    let mut files = Vec::new();
    for (path, ..) in &scopes {
        let id = new_id();
        let mut body = vec![b'f'; 9000];
        body.extend_from_slice(b"OLD_ATTACHMENT_MARKER");
        let status = upload(
            path.replace("/turns", &format!("/files/{id}")),
            body,
            "old.txt",
        )
        .await?;
        ensure!(status == 200, "upload: {status}");
        files.push(id);
    }
    let unsent = new_id();
    let status = upload(
        scopes[0].0.replace("/turns", &format!("/files/{unsent}")),
        b"UNSENT_PRIVATE".to_vec(),
        "unsent.txt",
    )
    .await?;
    ensure!(status == 200, "unsent upload: {status}");

    // Seed one old original (with an attachment) and 42 newer turns per scope.
    let originals = app
        .edit_database(|db: &Fixture| {
            let mut originals = Vec::new();
            for (index, (_, channel, thread)) in scopes.iter().enumerate() {
                let id = new_id();
                let insert = "INSERT INTO turns(id,agent,channel,thread,text,reply,status,created) VALUES (?,?,?,?,?,?,'done','2026-11-14T00:00:00.000Z')";
                db.execute(
                    insert,
                    (
                        &id,
                        &agent_id,
                        channel,
                        thread,
                        format!("{}OLD_ORIGINAL_MARKER", "x".repeat(12000)),
                        "Saved original reply",
                    ),
                )?;
                let seq = db.connection().last_insert_rowid();
                db.execute(
                    "UPDATE turns SET attachments=? WHERE id=?",
                    (json!([files[index]]).to_string(), &id),
                )?;
                originals.push((id, seq));
                for n in 0..42 {
                    db.execute(
                        insert,
                        (
                            new_id(),
                            &agent_id,
                            channel,
                            thread,
                            format!("Recent {n}"),
                            "Recent reply",
                        ),
                    )?;
                }
            }
            Ok(originals)
        })
        .await?;
    let run = Run { app, model, plan };
    let count = scopes.len();
    for n in 0..count {
        let path = &scopes[n].0;
        run.change(|plan| {
            plan.mode = "read";
            plan.oldest.clone_from(&originals[n].0);
            plan.before = originals[n].1 + 1;
        });
        let results = run.run(path, None).await?;
        ensure!(
            results[0]["messages"][0]["id"] == originals[n].0.as_str(),
            "history page: {}",
            results[0]
        );
        ensure!(
            results[0]["messages"][0]["truncated"] == true,
            "history page: {}",
            results[0]
        );
        ensure!(
            results[1]["text"].as_str().map(str::len) == Some(8000),
            "first slice: {}",
            results[1]["text"].as_str().map_or(0, str::len)
        );
        ensure!(
            results[1]["next_offset"] == 8000,
            "first slice: {}",
            results[1]["next_offset"]
        );
        ensure!(
            results[2]["text"] == format!("{}OLD_ORIGINAL_MARKER", "x".repeat(4000)),
            "second slice"
        );
        ensure!(results[2]["next_offset"].is_null(), "second slice offset");

        run.change(|plan| {
            plan.mode = "attachment";
            plan.attachment.clone_from(&files[n]);
        });
        let results = run.run(path, None).await?;
        ensure!(
            results[0]["text"].as_str().map(str::len) == Some(8000),
            "attachment slice"
        );
        ensure!(results[0]["next_offset"] == 8000, "attachment offset");
        ensure!(
            results[1]["text"] == format!("{}OLD_ATTACHMENT_MARKER", "f".repeat(1000)),
            "attachment rest"
        );
        ensure!(
            results[1]["next_offset"].is_null(),
            "attachment rest offset"
        );

        run.change(|plan| {
            plan.mode = "denied-attachment";
            plan.attachment.clone_from(&files[(n + 1) % count]);
        });
        let results = run.run(path, None).await?;
        ensure!(not_found(&results), "foreign attachment: {results:?}");

        run.change(|plan| {
            plan.mode = "foreign";
            plan.foreign.clone_from(&originals[(n + 1) % count].0);
        });
        let results = run.run(path, None).await?;
        ensure!(not_found(&results), "foreign message: {results:?}");
    }

    // Unsent files stay private, also when a later turn attaches them mid-read.
    let direct = &scopes[0].0;
    run.change(|plan| {
        plan.mode = "denied-attachment";
        plan.attachment.clone_from(&unsent);
    });
    let results = run.run(direct, None).await?;
    ensure!(not_found(&results), "unsent attachment: {results:?}");
    run.change(|plan| plan.during = true);
    let results = run
        .run(
            direct,
            Some((
                direct.clone(),
                json!({"id": new_id(), "text": "Future disclosure", "files": [unsent]}),
            )),
        )
        .await?;
    ensure!(not_found(&results), "future attachment: {results:?}");
    run.change(|plan| plan.mode = "invalid");
    let results = run.run(direct, None).await?;
    ensure!(
        results == vec![json!({"error": "Invalid history position.", "status": 400})],
        "invalid: {results:?}"
    );
    run.change(|plan| plan.mode = "loop");
    let results = run.run(direct, None).await?;
    ensure!(results.len() == 9, "loop reads: {}", results.len());
    Ok(())
}
