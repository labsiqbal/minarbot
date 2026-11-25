//! Incremental history compaction and its fences (port of
//! `app/compaction.test.ts`).
//!
//! Since #87 the summary request runs through the agent's provider instance
//! (`harness::run` with phase `compaction`); the gateway peer hands the
//! server's messages to the model unchanged, so the model sees the same
//! `Summarize conversation history` request the Node provider saw. The Node
//! check that the summary request carried no `tools` was a direct-client
//! property: a harness CLI always exposes its MCP server, and the request's
//! system message forbids tool use.

use crate::sb_support::{id, settle};
use minarbot_testkit::{
    control::{Answer, ControlModel},
    db::{Fixture, Inspector},
    model::ModelRequest,
    prelude::*,
    Server,
};
use std::sync::{Arc, Mutex};

const SUMMARY: &str = "Summarize conversation history";

fn summary_request(request: &ModelRequest) -> bool {
    request.system().starts_with(SUMMARY)
}

/// JSON input of a summary request (`{previous, messages}`).
fn summary_input(request: &ModelRequest) -> Value {
    serde_json::from_str(&request.last_user_text()).unwrap_or(Value::Null)
}

#[derive(Default)]
struct Script {
    fail: bool,
    hold: bool,
}

struct Scope {
    path: String,
    agent: String,
    channel: Option<String>,
    thread: Option<String>,
    marker: &'static str,
}

async fn run(app: &Server, scope: &Scope, recipient: &str) -> Result<String> {
    let id = new_id();
    let mut body = json!({"id": id, "text": "Continue"});
    if scope.channel.is_some() {
        body["recipient"] = json!(recipient);
    }
    app.post_expect(&format!("{}/turns", scope.path), body, 202)
        .await?;
    let row = settle(app, &format!("{}/turns", scope.path), &id).await?;
    ensure!(row["status"] == "done", "{}: {row}", scope.path);
    Ok(id)
}

fn last<'a>(requests: &'a [ModelRequest], summary: bool) -> Result<&'a ModelRequest> {
    requests
        .iter()
        .rev()
        .find(|request| summary_request(request) == summary)
        .ok_or_else(|| format!("no {} request", if summary { "summary" } else { "answer" }).into())
}

#[tokio::test]
async fn incremental_compaction_keeps_source_scopes_originals_provenance_and_failure_fallback(
) -> Result<()> {
    let script = Arc::new(Mutex::new(Script::default()));
    let model = ControlModel::start().await?;
    let shared = script.clone();
    model.respond(move |request| {
        if !summary_request(request) {
            return Answer::text("Done");
        }
        let Ok(script) = shared.lock() else {
            return Answer::Status(500, json!({"error": "script"}));
        };
        if script.hold {
            return Answer::Defer;
        }
        if script.fail {
            return Answer::Status(503, json!({}));
        }
        let input = summary_input(request);
        let mut text = input["previous"].as_str().unwrap_or_default().to_owned();
        let lines: Vec<String> = input["messages"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| {
                        format!(
                            "{}:{}",
                            row["text"].as_str().unwrap_or_default(),
                            row["id"].as_str().unwrap_or_default()
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        text.push_str(&lines.join("\n"));
        Answer::text(text)
    });
    let root = TestRoot::new()?;
    // As in the Node case, a 1 s `timeout` bounds each harness call, so a
    // stalled summary falls back to full history.
    let config = ServerConfig::new()
        .gateway(model.gateway())
        .option("timeout", 1000);
    let mut app = root.start(config).await?;
    let agent = |name: &str| json!({"runtime": "api", "id": new_id(), "name": name, "model": "fixture", "instructions": "Help"});
    let a = id(&app.post("/api/agents", agent("A")).await?)?;
    let b = id(&app.post("/api/agents", agent("B")).await?)?;
    let c = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Room", "instructions": "Help", "lead": a, "members": [a, b]}),
        )
        .await?)?;
    let thread = id(&app
        .post(
            &format!("/api/agents/{a}/threads"),
            json!({"id": new_id(), "name": "Side"}),
        )
        .await?)?;
    let scopes = [
        Scope {
            path: format!("/api/agents/{a}"),
            agent: a.clone(),
            channel: None,
            thread: None,
            marker: "DIRECT_A",
        },
        Scope {
            path: format!("/api/agents/{b}"),
            agent: b.clone(),
            channel: None,
            thread: None,
            marker: "DIRECT_B",
        },
        Scope {
            path: format!("/api/channels/{c}"),
            agent: a.clone(),
            channel: Some(c.clone()),
            thread: None,
            marker: "SHARED_C",
        },
        Scope {
            path: format!("/api/threads/{thread}"),
            agent: a.clone(),
            channel: None,
            thread: Some(thread.clone()),
            marker: "SIDE_A",
        },
    ];
    app.edit_database(|db| {
        for scope in &scopes {
            for n in 0..65 {
                db.execute(
                    "INSERT INTO turns(id,agent,channel,thread,text,reply,status,created) VALUES (?,?,?,?,?,?,'done',?)",
                    (new_id(), &scope.agent, &scope.channel, &scope.thread,
                        format!("{}_{n}", scope.marker), format!("Reply {n}"), "2026-01-01T00:00:00.000Z"),
                )?;
            }
        }
        let first: String = db.scalar("SELECT id FROM turns ORDER BY seq LIMIT 1", [])?;
        db.execute(
            "INSERT INTO questions VALUES (?,?,?,?,?,?,?,0,0)",
            (new_id(), first, "question", "{}", "Decision?", "[]", "IMPORTANT_DECISION"),
        )?;
        Ok(())
    })
    .await?;
    for (index, scope) in scopes.iter().enumerate() {
        run(&app, scope, &a).await?;
        let first = app.get(&format!("{}/compaction", scope.path)).await?;
        ensure!(first["source_count"] == 20, "{}: {first}", scope.path);
        ensure!(
            first["sources"].as_array().map(Vec::len) == Some(20),
            "sources: {first}"
        );
        ensure!(first["next_after"].is_null(), "next_after: {first}");
        let requests = model.requests();
        let summary = last(&requests, true)?;
        let input = summary_input(summary);
        ensure!(
            input["messages"].as_array().map(Vec::len) == Some(20),
            "summary input: {input}"
        );
        let summary_text = input.to_string();
        if index == 0 {
            ensure!(
                summary_text.contains("IMPORTANT_DECISION"),
                "question card missing"
            );
        }
        let prompt = serde_json::to_string(last(&requests, false)?.messages())?;
        ensure!(
            prompt.contains("Earlier conversation summary"),
            "{}: answer prompt has no summary",
            scope.path
        );
        for other in scopes.iter().filter(|other| other.marker != scope.marker) {
            ensure!(
                !summary_text.contains(other.marker) && !prompt.contains(other.marker),
                "{} leaked into {}",
                other.marker,
                scope.marker
            );
        }
        run(&app, scope, &a).await?;
        let second = app.get(&format!("{}/compaction", scope.path)).await?;
        ensure!(second["source_count"] == 26, "{}: {second}", scope.path);
        let page = app
            .get(&format!(
                "{}/compaction?after={}",
                scope.path, second["next_after"]
            ))
            .await?;
        ensure!(
            page["sources"].as_array().map(Vec::len) == Some(6),
            "page: {page}"
        );
        let turns = app.get(&format!("{}/turns", scope.path)).await?;
        let turns = turns.as_array().ok_or("turns")?;
        ensure!(turns.len() == 67, "{}: {} turns", scope.path, turns.len());
        let original = format!("{}_0", scope.marker);
        ensure!(
            turns.iter().any(|row| row["text"] == original.as_str()),
            "original turn hidden"
        );
    }
    let usage = app.get("/api/usage").await?;
    let compactions = usage["calls"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| row["phase"] == "compaction")
                .count()
        })
        .unwrap_or(0);
    ensure!(compactions == 8, "compaction calls: {compactions}");

    script.lock().map_err(|_| "script")?.fail = true;
    let before = app.get(&format!("{}/compaction", scopes[0].path)).await?;
    run(&app, &scopes[0], &a).await?;
    ensure!(
        app.get(&format!("{}/compaction", scopes[0].path)).await? == before,
        "failed summary changed the compaction"
    );
    let prompt = serde_json::to_string(last(&model.requests(), false)?.messages())?;
    ensure!(
        prompt.contains("DIRECT_A_0"),
        "failure did not fall back to history"
    );
    script.lock().map_err(|_| "script")?.fail = false;

    script.lock().map_err(|_| "script")?.hold = true;
    run(&app, &scopes[0], &a).await?;
    let stalled = model.deferred().await?;
    let prompt = serde_json::to_string(last(&model.requests(), false)?.messages())?;
    ensure!(
        prompt.contains("DIRECT_A_0"),
        "stalled summary did not fall back"
    );
    drop(stalled);

    let id = new_id();
    let path = format!("{}/turns", scopes[1].path);
    app.post_expect(&path, json!({"id": id, "text": "Interrupted summary"}), 202)
        .await?;
    let held = model.deferred().await?;
    app.shutdown().await?;
    drop(held);
    script.lock().map_err(|_| "script")?.hold = false;
    app.restart().await?;
    let stopped = app.row(&path, &id).await?.ok_or("stopped turn")?;
    let calls = model
        .requests()
        .iter()
        .filter(|request| summary_request(request))
        .count();
    app.post(
        &format!("{path}/{id}/resume"),
        json!({"id": new_id(), "revision": stopped["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    app.wait_status(&path, &id, "done").await?;
    let after = model
        .requests()
        .iter()
        .filter(|request| summary_request(request))
        .count();
    ensure!(
        after == calls,
        "resume repeated the summary: {calls} -> {after}"
    );

    let source = before["sources"][0]["id"]
        .as_str()
        .ok_or("source")?
        .to_owned();
    app.edit_database(|db| {
        db.execute("UPDATE turns SET revision=revision+1 WHERE id=?", [&source])?;
        Ok(())
    })
    .await?;
    ensure!(
        app.get(&format!("{}/compaction", scopes[0].path))
            .await?
            .is_null(),
        "changed source still served"
    );
    run(&app, &scopes[0], &a).await?;
    ensure!(
        app.get(&format!("{}/compaction", scopes[0].path)).await?["source_count"] == 20,
        "rebuilt compaction"
    );
    Ok(())
}

#[derive(Default)]
struct Fence {
    defer: bool,
    summaries: usize,
    last_prompt: String,
}

#[tokio::test]
async fn rust_compaction_fences_stale_responses_changed_sources_cancelled_turns_and_future_watermarks(
) -> Result<()> {
    let fence = Arc::new(Mutex::new(Fence::default()));
    let model = ControlModel::start().await?;
    let shared = fence.clone();
    model.respond(move |request| {
        let Ok(mut fence) = shared.lock() else {
            return Answer::Status(500, json!({"error": "script"}));
        };
        if summary_request(request) {
            fence.summaries += 1;
            if fence.defer {
                return Answer::Defer;
            }
            return Answer::text("Original summary");
        }
        fence.last_prompt = request.system();
        Answer::text("Done")
    });
    let root = TestRoot::new()?;
    let mut app = root
        .start(ServerConfig::new().gateway(model.gateway()))
        .await?;
    let agent = id(&app
        .post(
            "/api/agents",
            json!({"id": new_id(), "runtime": "api", "name": "Compaction", "model": "fixture", "instructions": "Help."}),
        )
        .await?)?;
    let path = format!("/api/agents/{agent}");
    let turns = format!("{path}/turns");
    let scope = format!("agents:{agent}");
    let db = Fixture::open(root.database())?;
    db.execute_batch("BEGIN IMMEDIATE")?;
    for index in 1..=70 {
        db.execute(
            "INSERT INTO turns(id,agent,text,status,created) VALUES (?,?,?,?,'2026-01-01')",
            (new_id(), &agent, format!("Message {index}"), "done"),
        )?;
    }
    db.execute_batch("COMMIT")?;
    let attempt = |turn: &str| -> Result<Value> {
        let inspector = Inspector::open(root.database())?;
        let rows = inspector.rows(
            "SELECT status FROM compaction_attempts WHERE turn=?",
            [turn],
        )?;
        Ok(rows
            .first()
            .map(|row| row["status"].clone())
            .unwrap_or(Value::Null))
    };
    let first_turn = submit_turn(&app, &turns).await?;
    app.wait_status(&turns, &first_turn, "done").await?;
    let first = app.get(&format!("{path}/compaction")).await?;
    ensure!(first["summary"] == "Original summary", "first: {first}");

    fence.lock().map_err(|_| "fence")?.defer = true;
    let stale = submit_turn(&app, &turns).await?;
    let held = model.deferred().await?;
    db.execute(
        "UPDATE compactions SET version=?,summary='Newer summary' WHERE scope=?",
        (new_id(), &scope),
    )?;
    held.answer(Answer::text("Stale summary"));
    app.wait_status(&turns, &stale, "done").await?;
    ensure!(
        attempt(&stale)? == "superseded",
        "stale: {}",
        attempt(&stale)?
    );
    ensure!(
        app.get(&format!("{path}/compaction")).await?["summary"] == "Newer summary",
        "stale summary replaced the newer one"
    );

    let changed = submit_turn(&app, &turns).await?;
    let held = model.deferred().await?;
    db.execute(
        "UPDATE turns SET revision=revision+1 WHERE seq=?",
        [first["through"].as_i64().ok_or("through")? + 1],
    )?;
    held.answer(Answer::text("Invalid source summary"));
    app.wait_status(&turns, &changed, "done").await?;
    ensure!(
        attempt(&changed)? == "superseded",
        "changed: {}",
        attempt(&changed)?
    );

    let oversized = submit_turn(&app, &turns).await?;
    let held = model.deferred().await?;
    held.answer(Answer::text("x".repeat(6001)));
    app.wait_status(&turns, &oversized, "done").await?;
    ensure!(
        attempt(&oversized)? == "failed",
        "oversized: {}",
        attempt(&oversized)?
    );

    let cancelled = submit_turn(&app, &turns).await?;
    let held = model.deferred().await?;
    app.post(
        &format!("{turns}/{cancelled}/cancel"),
        json!({"id": new_id()}),
    )
    .await?;
    held.answer(Answer::text("Cancelled summary"));
    app.wait_status(&turns, &cancelled, "cancelled").await?;
    ensure!(
        app.get(&format!("{path}/compaction")).await?["summary"] == "Newer summary",
        "cancelled summary saved"
    );

    app.shutdown().await?;
    let future = new_id();
    db.execute(
        "INSERT INTO turns(id,agent,text,status,created) VALUES (?,?,?,?,'2026-01-01')",
        (&future, &agent, "Position before the snapshot", "queued"),
    )?;
    db.execute("INSERT INTO queue_positions VALUES (?,10)", [&future])?;
    let before = fence.lock().map_err(|_| "fence")?.summaries;
    app.restart().await?;
    app.wait_status(&turns, &future, "done").await?;
    let fence_state = fence.lock().map_err(|_| "fence")?;
    ensure!(fence_state.summaries == before, "future turn compacted");
    ensure!(
        attempt(&future)?.is_null(),
        "future attempt: {}",
        attempt(&future)?
    );
    ensure!(
        !fence_state.last_prompt.contains("Newer summary"),
        "future turn saw a later summary"
    );
    drop(fence_state);
    db.execute("UPDATE turns SET revision=revision+1 WHERE seq=1", [])?;
    ensure!(
        app.get(&format!("{path}/compaction")).await?.is_null(),
        "changed first source still served"
    );
    Ok(())
}

async fn submit_turn(app: &Server, turns: &str) -> Result<String> {
    let id = new_id();
    app.post_expect(turns, json!({"id": id, "text": "Continue"}), 202)
        .await?;
    Ok(id)
}
