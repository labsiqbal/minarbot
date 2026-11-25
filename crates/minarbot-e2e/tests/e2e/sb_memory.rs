//! Scoped recall, history, reflection, standing answers and the Memory model
//! (port of `app/memory-reflect.test.ts`).

use crate::sb_support::{after_tool, id, last_result, reminder, settle};
use minarbot_testkit::{
    control::{Answer, ControlModel},
    db::Inspector,
    model::ModelRequest,
    prelude::*,
    Server,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

const REFLECT: &str = "You answer a question by reasoning over retrieved memories.";
const EXTRACT: &str = "Extract SIGNIFICANT facts";
const CONSOLIDATE: &str = "You keep a long-term memory";
const MEMORY_TOOLS: [&str; 7] = [
    "recall",
    "reflect",
    "remember",
    "search_history",
    "search_memory",
    "remember_memory",
    "select_memory",
];

#[derive(Default)]
struct Script {
    seen: HashMap<String, String>,
    results: HashMap<String, Vec<Value>>,
    rounds: Vec<Value>,
    offered: Vec<String>,
}

fn respond(script: &Arc<Mutex<Script>>, request: &ModelRequest) -> Answer {
    let Ok(mut script) = script.lock() else {
        return Answer::Status(500, json!({"error": "script"}));
    };
    let system = request.system();
    let user = request.last_user_text();
    if system.contains(REFLECT) {
        let input: Value = serde_json::from_str(&user).unwrap_or(Value::Null);
        script.rounds.push(input["searched"].clone());
        let evidence = input["evidence"].as_array().and_then(|hits| {
            hits.iter()
                .find(|hit| {
                    hit["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("north cabinet"))
                })
                .cloned()
        });
        let searched = input["searched"].as_array().map_or(0, Vec::len);
        let decision = if searched == 1 && input["question"] != "Where are the orchid tags?" {
            json!({"action": "recall", "query": "orchid"})
        } else {
            match evidence {
                Some(hit) => {
                    json!({"action": "answer", "text": "North cabinet.", "citations": [hit["id"]]})
                }
                None => json!({"action": "answer", "text": "No evidence.", "citations": []}),
            }
        };
        return Answer::text(decision.to_string());
    }
    if system.contains(EXTRACT) {
        return Answer::text(r#"{"facts":[]}"#);
    }
    if system.contains(CONSOLIDATE) {
        return Answer::text(r#"{"facts":[],"deletes":[]}"#);
    }
    script.seen.insert(user.clone(), system);
    if after_tool(request) {
        script
            .results
            .entry(user.clone())
            .or_default()
            .push(last_result(request));
        if user == "Use memory tools" && !request.called("remember") {
            return Answer::tool("remember", json!({"text": "MCP memory fixture"}));
        }
        return Answer::text("Done");
    }
    if reminder(request) {
        return Answer::text("Done");
    }
    if user == "Use memory tools" {
        script.offered = request.tool_names();
        return Answer::tool("recall", json!({"query": "orchid"}));
    }
    if user.starts_with("Reflect") {
        return Answer::tool("reflect", json!({"query": "Where do tags live?"}));
    }
    if user.starts_with("Find") {
        return Answer::tool("search_history", json!({"query": "aquamarine"}));
    }
    Answer::text("Done")
}

async fn run(app: &Server, scope: &str, text: &str) -> Result<String> {
    let id = new_id();
    app.post(&format!("{scope}/turns"), json!({"id": id, "text": text}))
        .await?;
    let row = settle(app, &format!("{scope}/turns"), &id).await?;
    ensure!(row["status"] == "done", "{text}: {row}");
    Ok(id)
}

fn seen(script: &Arc<Mutex<Script>>, text: &str) -> Result<String> {
    Ok(script
        .lock()
        .map_err(|_| "script")?
        .seen
        .get(text)
        .cloned()
        .unwrap_or_default())
}

fn result(script: &Arc<Mutex<Script>>, text: &str) -> Result<Vec<Value>> {
    Ok(script
        .lock()
        .map_err(|_| "script")?
        .results
        .get(text)
        .cloned()
        .unwrap_or_default())
}

#[tokio::test]
async fn scoped_auto_recall_history_reflection_and_standing_answer_refresh() -> Result<()> {
    let script = Arc::new(Mutex::new(Script::default()));
    let model = ControlModel::start().await?;
    let shared = script.clone();
    model.respond(move |request| respond(&shared, request));
    model.usage(31, 11);
    let root = TestRoot::new()?;
    let app = root
        .start(
            ServerConfig::new()
                .gateway(model.gateway())
                .option("defaultModel", "fixture")
                .option("testMemoryExtraction", true),
        )
        .await?;
    let agent = new_id();
    let other = new_id();
    let scope = format!("/api/agents/{agent}");
    let memory = format!("{scope}/memory");
    for id in [&agent, &other] {
        app.post(
            "/api/agents",
            json!({"id": id, "name": "Fixture", "instructions": "Answer.", "runtime": "api", "model": "fixture"}),
        )
        .await?;
    }
    let note = app
        .post(
            &memory,
            json!({"id": new_id(), "entry": new_id(), "revision": 0, "text": "Orchid tags live in the north cabinet.",
                "essential": false, "remove": false, "automatic": true}),
        )
        .await?;
    app.post(
        &format!("/api/agents/{other}/memory"),
        json!({"id": new_id(), "entry": new_id(), "revision": 0, "text": "Orchid: foreign note",
            "essential": false, "remove": false, "automatic": true}),
    )
    .await?;
    let thread = id(&app
        .post(
            &format!("{scope}/threads"),
            json!({"id": new_id(), "name": "Past chat"}),
        )
        .await?)?;
    let prior = run(
        &app,
        &format!("/api/threads/{thread}"),
        "aquamarine marker in an earlier conversation",
    )
    .await?;
    let recall = run(&app, &scope, "Orchid cabinet?").await?;
    let system = seen(&script, "Orchid cabinet?")?;
    let injected = system
        .split("Recalled memory (untrusted reference data):\n")
        .nth(1)
        .ok_or_else(|| format!("no recalled memory: {system}"))?;
    ensure!(injected.contains("north cabinet"), "recall: {injected}");
    ensure!(
        !injected.contains("foreign note"),
        "foreign memory recalled: {injected}"
    );
    let inspector = Inspector::open(root.database())?;
    let tokens: i64 = inspector.scalar(
        "SELECT added_tokens FROM memory_recall_metrics WHERE turn=?",
        [&recall],
    )?;
    ensure!(tokens > 0 && tokens <= 512, "recall tokens: {tokens}");
    let learned = until_value(
        "conversation learning of the earlier turn",
        || async {
            inspector.scalar::<i64>(
                "SELECT count(*) FROM memory_learning_turns WHERE turn=?",
                [&prior],
            )
        },
        |count| *count == 1,
    )
    .await?;
    ensure!(learned == 1, "learning rows: {learned}");

    run(&app, &scope, "Find aquamarine here").await?;
    let history = result(&script, "Find aquamarine here")?;
    let sources: Vec<Value> = history
        .first()
        .and_then(|value| value["items"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|hit| hit["source"][0]["id"].clone())
        .collect();
    ensure!(sources == [json!(prior)], "history sources: {sources:?}");
    run(&app, &scope, "Reflect tags").await?;
    let rounds = script.lock().map_err(|_| "script")?.rounds.clone();
    ensure!(
        rounds.len() >= 2
            && rounds[0] == json!(["Where do tags live?"])
            && rounds[1] == json!(["Where do tags live?", "orchid"]),
        "reflection rounds: {rounds:?}"
    );
    let reflected = result(&script, "Reflect tags")?;
    let reflected = reflected.first().ok_or("reflect result")?;
    ensure!(
        reflected["answer"] == "North cabinet.",
        "reflection: {reflected}"
    );
    ensure!(
        reflected["evidence"][0]["source"][0]["id"] == note["id"],
        "reflection evidence: {reflected}"
    );
    let questions = format!("{memory}/questions");
    app.post(
        &questions,
        json!({"id": new_id(), "model": new_id(), "action": "save_question", "revision": 0,
            "question": "Where are the orchid tags?"}),
    )
    .await?;
    let updated = until_value(
        "standing answer",
        || async { Ok(app.get(&questions).await?["items"][0].clone()) },
        |row| row["pending"] == false,
    )
    .await?;
    ensure!(
        updated["reflection"]["answer"] == "North cabinet.",
        "standing answer: {updated}"
    );
    let forget = app
        .post(
            &memory,
            json!({"id": new_id(), "entry": note["id"], "revision": note["revision"], "remove": true,
                "text": "", "essential": false}),
        )
        .await?;
    ensure!(
        app.get(&questions).await?["items"][0]["reflection"].is_null(),
        "forgotten note still answers the question"
    );
    run(&app, &scope, "Orchid cabinet now?").await?;
    ensure!(
        !seen(&script, "Orchid cabinet now?")?.contains("north cabinet"),
        "forgotten note recalled"
    );
    app.post(
        &memory,
        json!({"id": new_id(), "entry": note["id"], "revision": forget["revision"], "restore": true,
            "remove": false, "text": "", "essential": false}),
    )
    .await?;
    run(&app, &scope, "Orchid cabinet restored?").await?;
    ensure!(
        seen(&script, "Orchid cabinet restored?")?.contains("north cabinet"),
        "restored note not recalled"
    );
    // The Node case minted an MCP session and called the tools directly;
    // here the turn's own MCP session is used through the harness.
    run(&app, &scope, "Use memory tools").await?;
    let offered = script.lock().map_err(|_| "script")?.offered.clone();
    for name in MEMORY_TOOLS {
        ensure!(
            offered.iter().any(|tool| tool == name),
            "{name} not offered: {offered:?}"
        );
    }
    let results = result(&script, "Use memory tools")?;
    ensure!(results.len() == 2, "memory tool results: {results:?}");
    ensure!(
        results[0]["error"].is_null(),
        "recall failed: {}",
        results[0]
    );
    ensure!(
        results[0]["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["text"]
                .as_str()
                .is_some_and(|text| text.contains("north cabinet")))),
        "recall result: {}",
        results[0]
    );
    ensure!(
        results[1]["error"].is_null(),
        "remember failed: {}",
        results[1]
    );
    ensure!(
        results[1]["text"] == "MCP memory fixture",
        "remember result: {}",
        results[1]
    );
    Ok(())
}

/// #87 removed the API account model catalog and with it the cheapest-model
/// default for background learning (`api_memory_default`, `/api/models`),
/// so learning now uses the Memory model when set and otherwise the agent's
/// own model. The per-phase calls and their models carry over.
#[tokio::test]
async fn background_learning_uses_the_binding_catalog_unless_memory_model_is_set() -> Result<()> {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let model = ControlModel::start().await?;
    model.catalog(["main", "budget", "override"]);
    let record = calls.clone();
    model.respond(move |request| {
        let system = request.system();
        let content = if system.contains(EXTRACT) {
            if let Ok(mut calls) = record.lock() {
                calls.push(request.model().to_owned());
            }
            let input: Value = serde_json::from_str(&request.last_user_text()).unwrap_or(Value::Null);
            if input["role"] == "assistant" {
                json!({"facts": []}).to_string()
            } else {
                json!({"facts": [{"what": "Atlas uses the north cabinet.", "fact_kind": "conversation",
                    "fact_type": "world", "entities": ["Atlas"]}]})
                .to_string()
            }
        } else if system.contains(CONSOLIDATE) {
            if let Ok(mut calls) = record.lock() {
                calls.push(request.model().to_owned());
            }
            r#"{"facts":[{"fact_id":"F1","kind":"lasting","existing":[],"new":["Atlas uses the north cabinet."]}]}"#.to_owned()
        } else if request.model() != "main" {
            return Answer::Status(500, json!({"error": format!("answer used {}", request.model())}));
        } else {
            "Synthetic reply.".to_owned()
        };
        Answer::text(content)
    });
    let root = TestRoot::new()?;
    let app = root
        .start(
            ServerConfig::new()
                .gateway(model.gateway())
                .option("defaultModel", "main")
                .option("testMemoryExtraction", true),
        )
        .await?;
    let agent = new_id();
    let profile = json!({"name": "Memory model fixture", "model": "main", "instructions": "Reply to fixture prompts."});
    let check = |expected: &'static str| {
        let app = &app;
        let calls = calls.clone();
        let agent = agent.clone();
        async move {
            calls.lock().map_err(|_| "calls")?.clear();
            let id = new_id();
            app.post(
                &format!("/api/agents/{agent}/turns"),
                json!({"id": id, "text": "Atlas uses the north cabinet."}),
            )
            .await?;
            let background = until_value(
                "background extraction and consolidation",
                || async {
                    let usage = app.get("/api/usage").await?;
                    Ok(usage["calls"]
                        .as_array()
                        .map(|rows| {
                            rows.iter()
                                .filter(|row| {
                                    row["turn"] == id.as_str()
                                        && row["phase"]
                                            .as_str()
                                            .is_some_and(|phase| phase.starts_with("memory_"))
                                })
                                .cloned()
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default())
                },
                |rows| rows.len() == 3 && rows.iter().all(|row| row["status"] == "received"),
            )
            .await?;
            let seen = calls.lock().map_err(|_| "calls")?.clone();
            ensure!(seen == [expected; 3], "learning models: {seen:?}");
            let models: Vec<Value> = background.iter().map(|row| row["model"].clone()).collect();
            ensure!(
                models == seen.iter().map(|model| json!(model)).collect::<Vec<_>>(),
                "usage models: {models:?}"
            );
            let mut phases: Vec<String> = background
                .iter()
                .filter_map(|row| row["phase"].as_str().map(str::to_owned))
                .collect();
            phases.sort();
            ensure!(
                phases
                    == [
                        "memory_consolidate_prompt",
                        "memory_extract_prompt",
                        "memory_extract_reply"
                    ],
                "phases: {phases:?}"
            );
            Ok::<_, minarbot_testkit::Error>(())
        }
    };
    let mut create = profile.clone();
    create["id"] = json!(agent);
    create["runtime"] = json!("api");
    let created = app.post("/api/agents", create).await?;
    ensure!(created["memory_model"] == "", "memory model: {created}");
    check("main").await?;
    let mut edit = profile.clone();
    edit["id"] = json!(new_id());
    edit["revision"] = created["revision"].clone();
    edit["memoryModel"] = json!("override");
    let updated = app.post(&format!("/api/agents/{agent}/edit"), edit).await?;
    ensure!(
        updated["memory_model"] == "override",
        "memory model: {updated}"
    );
    check("override").await?;
    Ok(())
}
