//! Agent tools through harness turns (ports of `app/flow-tools.test.ts`,
//! `app/routine-tools.test.ts`, `app/artifacts.test.ts`,
//! `app/avatar-tools.test.ts`, `app/site-tools.test.ts` and
//! `app/base-instructions-scenario.test.ts`).

use crate::fx_support::{api_agent, fetch, gated, id, row, rows, settle, tool_json, upload};
use minarbot_testkit::{
    db::{Fixture, Inspector},
    gated::{Answer, GatedModel},
    http::Method,
    model::ModelRequest,
    prelude::*,
    Server, FIXED_CLOCK,
};
use std::sync::{Arc, Mutex};

fn lock<T>(state: &Arc<Mutex<T>>) -> std::sync::MutexGuard<'_, T> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Tool results of a request, parsed.
fn results(request: &ModelRequest) -> Vec<Value> {
    request.tool_results().iter().map(tool_json).collect()
}

/// The server's done check: after a turn that saved files without a recorded
/// check, it asks once more with a "One verification reminder" message. The
/// Node API runtime put that reminder in the system prompt and kept the tool
/// results, so the Node providers answered it with their final text.
fn reminder(request: &ModelRequest) -> bool {
    request
        .last_user_text()
        .starts_with("One verification reminder")
}

/// The last tool message of a request (any position), parsed.
fn last_result(request: &ModelRequest) -> Option<(String, Value)> {
    request.tool_results().last().map(|message| {
        (
            message["tool_call_id"].as_str().unwrap_or_default().to_owned(),
            tool_json(message),
        )
    })
}

#[derive(Default)]
struct Flow {
    args: Value,
    hold: bool,
    held: bool,
    park: bool,
    last: Value,
}

#[tokio::test]
async fn agent_flow_edits_preserve_ownership_revisions_and_ordered_receipts_across_restart(
) -> Result<()> {
    let state = Arc::new(Mutex::new(Flow::default()));
    let model = GatedModel::start().await?;
    let shared = state.clone();
    model.respond(move |request| {
        let mut flow = lock(&shared);
        if reminder(request) {
            return Answer::text("Saved flow.");
        }
        let results = results(request);
        let order: Vec<String> = request
            .messages()
            .iter()
            .filter_map(|message| message["tool_calls"][0]["function"]["name"].as_str())
            .map(str::to_owned)
            .collect();
        let step = results.len();
        if step == 1 && flow.park {
            flow.park = false;
            return Answer::Park;
        }
        if step == 3 {
            if order != ["create_text_file", "set_routine_flow", "create_text_file"] {
                return Answer::Status(500, json!({"error": format!("tool order {order:?}")}));
            }
            flow.last = results[1].clone();
            if flow.hold {
                flow.held = true;
                return Answer::Hang;
            }
            return Answer::text("Saved flow.");
        }
        if step == 1 {
            Answer::tool("set_routine_flow", flow.args.clone())
        } else {
            Answer::tool(
                "create_text_file",
                json!({"name": format!("step-{step}.txt"), "text": "Synthetic evidence"}),
            )
        }
    });
    let root = TestRoot::new()?;
    let mut app = root.start(gated(&model)).await?;
    let a = id(&api_agent(&app, "A", "fixture").await?)?;
    let b = id(&api_agent(&app, "B", "fixture").await?)?;
    let input = json!({"name": "Flow", "prompt": "Synthetic", "enabled": false,
        "nextAt": FIXED_CLOCK + 3_600_000, "intervalSeconds": null});
    let with_id = |mut value: Value| {
        value["id"] = json!(new_id());
        value
    };
    let own = id(&app
        .post(&format!("/api/agents/{a}/routines"), with_id(input.clone()))
        .await?)?;
    let foreign = id(&app
        .post(&format!("/api/agents/{b}/routines"), with_id(input.clone()))
        .await?)?;
    let settings = format!("/api/routines/{own}/flow");
    let turns = format!("/api/agents/{a}/turns");
    {
        let mut flow = lock(&state);
        flow.args = json!({"id": own, "revision": 0, "links": [{"from": a, "to": b}]});
        flow.hold = true;
    }
    let turn = new_id();
    app.post(&turns, json!({"id": turn, "text": "Update the flow"}))
        .await?;
    until("held flow result", || async { Ok(lock(&state).held.then_some(())) }).await?;
    ensure!(app.get(&settings).await?["revision"] == 1, "flow saved before hold");
    app.restart().await?;
    lock(&state).hold = false;
    let stopped = row(&app, &turns, &turn).await?;
    ensure!(stopped["status"] == "interrupted", "after restart: {stopped}");
    app.post(&settings, json!({"id": new_id(), "revision": 1, "links": []}))
        .await?;
    app.post(
        &format!("{turns}/{turn}/resume"),
        json!({"id": new_id(), "revision": stopped["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    let done = settle(&app, &turns, &turn, "done").await?;
    ensure!(lock(&state).last["revision"] == 1, "replayed receipt {}", lock(&state).last);
    ensure!(app.get(&settings).await?["revision"] == 2, "user edit lost");
    ensure!(
        done["generatedFiles"].as_array().map(Vec::len) == Some(2),
        "files {}",
        done["generatedFiles"]
    );
    for (change, status) in [
        (json!({"id": own, "revision": 0, "links": []}), 409),
        (json!({"id": foreign, "revision": 0, "links": []}), 404),
        (json!({"id": own, "revision": 2, "links": [{"from": a, "to": a}]}), 400),
    ] {
        lock(&state).args = change;
        let turn = new_id();
        app.post(&turns, json!({"id": turn, "text": "Attempt invalid edit"}))
            .await?;
        settle(&app, &turns, &turn, "done").await?;
        ensure!(lock(&state).last["status"] == status, "result {}", lock(&state).last);
        ensure!(app.get(&settings).await?["revision"] == 2, "invalid edit saved");
    }
    ensure!(
        rows(&app, &format!("/api/routines/{own}/runs")).await?.is_empty(),
        "flow edit ran the routine"
    );
    ensure!(
        app.get(&format!("/api/routines/{foreign}/flow")).await?["revision"] == 0,
        "foreign flow changed"
    );
    let cancelled = new_id();
    {
        let mut flow = lock(&state);
        flow.args = json!({"id": own, "revision": 2, "links": [{"from": a, "to": b}]});
        flow.park = true;
    }
    app.post(&turns, json!({"id": cancelled, "text": "Cancel before the edit arrives"}))
        .await?;
    let parked = model.parked().await?;
    app.post(&format!("{turns}/{cancelled}/cancel"), json!({"id": new_id()}))
        .await?;
    let args = lock(&state).args.clone();
    parked.answer(Answer::tool("set_routine_flow", args));
    settle(&app, &turns, &cancelled, "cancelled").await?;
    app.restart().await?;
    let flow = app.get(&settings).await?;
    ensure!(flow["revision"] == 2, "cancelled edit saved: {flow}");
    ensure!(flow["graph"]["links"] == json!([]), "links {}", flow["graph"]);
    ensure!(
        row(&app, &turns, &cancelled).await?["generatedFiles"]
            .as_array()
            .map(Vec::len)
            == Some(1),
        "cancelled turn files"
    );
    Ok(())
}

#[derive(Default)]
struct Routines {
    mode: String,
    owned: String,
    foreign: String,
    bad_after: Value,
    raw: String,
    last: Value,
    leaked: bool,
}

#[tokio::test]
async fn routine_tools_recover_from_invalid_arguments_preserve_owner_scope_and_keep_runtime_faults_fatal(
) -> Result<()> {
    let state = Arc::new(Mutex::new(Routines {
        mode: "list".into(),
        ..Routines::default()
    }));
    let model = GatedModel::start().await?;
    let shared = state.clone();
    model.respond(move |request| {
        let mut script = lock(&shared);
        let previous = results(request);
        let mode = script.mode.clone();
        if mode.starts_with("channel")
            && request
                .tool_names()
                .iter()
                .any(|name| ["list_routines", "read_routine", "set_routine_flow"].contains(&name.as_str()))
        {
            script.leaked = true;
        }
        let call: Option<(&str, Value)> = match mode.as_str() {
            "channel-transfer" => previous.is_empty().then(|| {
                ("transfer_task", json!({"recipient": new_id(), "task": "Help"}))
            }),
            "channel" => None,
            "channel-denied" => previous.is_empty().then(|| {
                (
                    "set_routine_flow",
                    json!({"id": script.owned, "revision": 1, "links": []}),
                )
            }),
            "list" => (previous.len() < 2).then(|| {
                (
                    "list_routines",
                    if previous.is_empty() {
                        json!({})
                    } else {
                        json!({"after": previous[0]["next_after"]})
                    },
                )
            }),
            "bad-after" => (previous.len() <= 1).then(|| {
                (
                    "list_routines",
                    if previous.is_empty() {
                        json!({"after": script.bad_after})
                    } else {
                        json!({})
                    },
                )
            }),
            "bad-json" | "runtime-fault" => {
                previous.is_empty().then(|| ("list_routines", json!({"after": ""})))
            }
            _ => previous.is_empty().then(|| {
                let target = if mode == "foreign" { &script.foreign } else { &script.owned };
                let mut args = json!({"id": target});
                if mode == "invalid" {
                    args["extra"] = json!(true);
                }
                ("read_routine", args)
            }),
        };
        if !previous.is_empty() {
            script.last = json!(previous);
        }
        if mode == "runtime-fault" && !previous.is_empty() {
            return Answer::Status(500, json!({}));
        }
        match call {
            Some((name, _)) if mode == "bad-json" => Answer::raw_tool(name, &script.raw),
            Some((name, args)) => Answer::tool(name, args),
            None => Answer::text("Checked."),
        }
    });
    let root = TestRoot::new()?;
    let app = root.start(gated(&model)).await?;
    let run = |path: String, recipient: Option<String>, expected: &'static str| {
        let app = &app;
        async move {
            let turn = new_id();
            let mut body = json!({"id": turn, "text": "Inspect saved routines"});
            if let Some(recipient) = recipient {
                body["recipient"] = json!(recipient);
            }
            app.post(&path, body).await?;
            let row = app
                .wait_row(&path, &turn, |row| row["status"] == "done" || row["status"] == "failed")
                .await?;
            ensure!(row["status"] == expected, "expected {expected}: {row}");
            Ok::<_, minarbot_testkit::Error>(())
        }
    };
    let last = |state: &Arc<Mutex<Routines>>| lock(state).last.clone();
    let a = id(&api_agent(&app, "A", "fixture").await?)?;
    let b = id(&api_agent(&app, "B", "fixture").await?)?;
    let input = json!({"name": "Routine", "prompt": "PRIVATE_PROMPT", "enabled": false,
        "nextAt": FIXED_CLOCK + 3_600_000, "intervalSeconds": null});
    let mut ids = Vec::new();
    for index in 0..11 {
        let mut body = input.clone();
        body["id"] = json!(new_id());
        body["trigger"] = json!(if index == 0 { "webhook" } else { "schedule" });
        ids.push(id(&app.post(&format!("/api/agents/{a}/routines"), body).await?)?);
    }
    let owned = ids[0].clone();
    let mut foreign_body = input.clone();
    foreign_body["id"] = json!(new_id());
    foreign_body["prompt"] = json!("FOREIGN_SECRET");
    let foreign = id(&app.post(&format!("/api/agents/{b}/routines"), foreign_body).await?)?;
    {
        let mut script = lock(&state);
        script.owned = owned.clone();
        script.foreign = foreign.clone();
    }
    let hook = app
        .post(
            &format!("/api/routines/{owned}/webhook"),
            json!({"id": new_id(), "revision": 0, "rotate": true, "eventName": "test.ready", "filters": []}),
        )
        .await?;
    let saved = app
        .post(
            &format!("/api/routines/{owned}/flow"),
            json!({"id": new_id(), "revision": 0, "links": [{"from": a, "to": b}]}),
        )
        .await?;
    let direct = format!("/api/agents/{a}/turns");
    run(direct.clone(), None, "done").await?;
    let pages = last(&state);
    ensure!(pages[0]["routines"].as_array().map(Vec::len) == Some(10), "page one {pages}");
    ensure!(pages[1]["routines"].as_array().map(Vec::len) == Some(1), "page two {pages}");
    ensure!(pages[1]["next_after"].is_null(), "next_after {}", pages[1]);
    let mut listed: Vec<String> = pages
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|page| page["routines"].as_array().cloned().unwrap_or_default())
        .filter_map(|routine| routine["id"].as_str().map(str::to_owned))
        .collect();
    let mut sorted = ids.clone();
    sorted.sort();
    ensure!(listed == sorted, "listing order {listed:?}");
    listed.clear();
    ensure!(!pages.to_string().contains("PRIVATE_PROMPT"), "list leaked prompts");
    lock(&state).mode = "bad-after".into();
    for bad in [json!(""), json!(0), json!("Daily sales"), Value::Null] {
        lock(&state).bad_after = bad.clone();
        run(direct.clone(), None, "done").await?;
        let result = last(&state);
        ensure!(
            result[0]["error"]
                .as_str()
                .is_some_and(|error| error.contains("Omit after for the first page, or pass the UUID from next_after")),
            "bad after {bad}: {result}"
        );
        ensure!(result[0]["status"] == 400, "bad after status {result}");
        ensure!(result[1]["routines"].as_array().map(Vec::len) == Some(10), "recovery {result}");
    }
    lock(&state).mode = "bad-json".into();
    // `{` is not JSON: the harness rejects it before Minarbot sees a call
    // (dropped iteration, see the ledger).
    for raw in ["null", "[]", "\"cursor\""] {
        lock(&state).raw = raw.into();
        run(direct.clone(), None, "done").await?;
        let result = last(&state);
        ensure!(
            result[0]["error"]
                .as_str()
                .is_some_and(|error| error.contains("Tool arguments must be")),
            "bad json {raw}: {result}"
        );
        ensure!(result[0]["status"] == 400, "bad json status {result}");
    }
    lock(&state).mode = "runtime-fault".into();
    run(direct.clone(), None, "failed").await?;
    ensure!(last(&state)[0]["status"] == 400, "fault result {}", last(&state));
    lock(&state).mode = "read".into();
    run(direct.clone(), None, "done").await?;
    let read = last(&state);
    ensure!(read[0]["prompt"] == input["prompt"], "read prompt {read}");
    ensure!(read[0]["flow"] == saved, "read flow {read}");
    for secret in [&hook["secret"], &hook["route"]] {
        let secret = secret.as_str().ok_or("hook field")?;
        ensure!(!read.to_string().contains(secret), "read leaked webhook {secret}");
    }
    let side = id(&app
        .post(&format!("/api/agents/{a}/threads"), json!({"id": new_id(), "name": "Private side"}))
        .await?)?;
    run(format!("/api/threads/{side}/turns"), None, "done").await?;
    ensure!(last(&state)[0]["flow"] == saved, "side read {}", last(&state));
    lock(&state).mode = "foreign".into();
    run(direct.clone(), None, "done").await?;
    // Over MCP a refusal carries its status (400) next to the Node error text.
    ensure!(
        last(&state) == json!([{"error": "Routine not found for this agent.", "status": 400}]),
        "foreign {}",
        last(&state)
    );
    lock(&state).mode = "channel".into();
    let room = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Shared", "instructions": "Help.", "lead": a, "members": [a, b]}),
        )
        .await?)?;
    let room_turns = format!("/api/channels/{room}/turns");
    run(room_turns.clone(), Some(a.clone()), "done").await?;
    lock(&state).mode = "channel-transfer".into();
    run(room_turns.clone(), Some(a.clone()), "done").await?;
    ensure!(
        last(&state)
            == json!([{"error": "Coordination recipient is not another channel member.", "status": 400}]),
        "transfer {}",
        last(&state)
    );
    lock(&state).mode = "channel-denied".into();
    // #87: over MCP an unoffered tool is refused (403); the turn goes on.
    run(room_turns, Some(a.clone()), "done").await?;
    ensure!(last(&state)[0]["status"] == 403, "channel set_routine_flow not refused: {}", last(&state));
    ensure!(!lock(&state).leaked, "channel turns offered routine tools");
    lock(&state).mode = "invalid".into();
    run(direct, None, "done").await?;
    ensure!(
        last(&state) == json!([{"error": "Invalid routine ID.", "status": 400}]),
        "invalid {}",
        last(&state)
    );
    ensure!(
        rows(&app, &format!("/api/routines/{owned}/runs")).await?.is_empty(),
        "routine tools ran the routine"
    );
    Ok(())
}

#[derive(Default)]
struct Artifacts {
    hold: bool,
    unrelated: Option<String>,
    denied_later: bool,
    failures: Vec<String>,
}

#[tokio::test]
async fn generated_text_artifacts_persist_across_interruption_without_duplicate_creation(
) -> Result<()> {
    let state = Arc::new(Mutex::new(Artifacts::default()));
    let model = GatedModel::start().await?;
    let shared = state.clone();
    model.respond(move |request| {
        let mut script = lock(&shared);
        let task = request.last_user_text();
        let result = request
            .messages()
            .last()
            .filter(|message| message["role"] == "tool")
            .map(tool_json);
        if let (Some(result), true, Some(unrelated)) =
            (&result, task == "Earlier task", script.unrelated.clone())
        {
            if let Some(error) = result["error"].as_str() {
                script.denied_later =
                    error == "Attachment not found in sent messages in this conversation.";
                return Answer::text("Later unrelated file denied.");
            }
            return Answer::tool_id("read-unrelated", "read_attachment", json!({"id": unrelated}));
        }
        if let Some(result) = result {
            let expected = match task.as_str() {
                "bad-path" => Some(json!({"error": "Invalid text file name or content.", "status": 400})),
                "oversize" => Some(json!({"error": "Text file limit is 32 KiB.", "status": 413})),
                _ => None,
            };
            if let Some(expected) = expected {
                if result != expected {
                    script.failures.push(format!("{task}: {result}"));
                }
            }
            if script.hold {
                return Answer::Park;
            }
            return Answer::text(format!("Saved {}", result["name"].as_str().unwrap_or_default()));
        }
        if task == "Late file creation" {
            return Answer::Park;
        }
        let name = match task.as_str() {
            "bad-path" => "../escape.txt".to_owned(),
            "Write a file" => "result.txt".to_owned(),
            other => format!("{other}.txt"),
        };
        let text = if task == "oversize" {
            "界".repeat(12000)
        } else {
            "<script>literal text</script>\nSYNTHETIC OUTPUT".to_owned()
        };
        Answer::tool_id("artifact-call", "create_text_file", json!({"name": name, "text": text}))
    });
    let root = TestRoot::new()?;
    let mut app = root.start(gated(&model)).await?;
    let agent = id(&api_agent(&app, "Writer", "fixture").await?)?;
    let room = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Room", "instructions": "Help.", "members": [agent], "lead": agent}),
        )
        .await?)?;
    let side = id(&app
        .post(&format!("/api/agents/{agent}/threads"), json!({"id": new_id(), "name": "Side"}))
        .await?)?;
    let paths = [
        format!("/api/agents/{agent}"),
        format!("/api/channels/{room}"),
        format!("/api/threads/{side}"),
    ];
    for path in &paths {
        let request = json!({"id": new_id(), "text": "Write a file"});
        let turns = format!("{path}/turns");
        app.post(&turns, request.clone()).await?;
        let turn = settle(&app, &turns, &id(&request)?, "done").await?;
        let files = turn["generatedFiles"].as_array().ok_or("generatedFiles")?;
        ensure!(files.len() == 1, "generated {files:?}");
        let file = id(&files[0])?;
        let download = app
            .request(Method::GET, &format!("{path}/files/{file}"))?
            .send()
            .await?;
        let header = |name: &str| {
            download
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned()
        };
        ensure!(header("content-type") == "application/octet-stream", "type {}", header("content-type"));
        ensure!(header("content-disposition").contains("attachment"), "disposition {}", header("content-disposition"));
        ensure!(
            download.text().await? == "<script>literal text</script>\nSYNTHETIC OUTPUT",
            "download bytes"
        );
        app.post(&turns, request).await?;
        ensure!(rows(&app, &format!("{path}/files")).await?.len() == 1, "replay created a file");
        let foreign = paths.iter().find(|other| *other != path).ok_or("foreign path")?;
        ensure!(
            fetch(&app, &format!("{foreign}/files/{file}")).await?.0 == 404,
            "file visible from {foreign}"
        );
    }
    let path = &paths[0];
    let turns = format!("{path}/turns");
    for text in ["bad-path", "oversize"] {
        let turn = new_id();
        app.post(&turns, json!({"id": turn, "text": text})).await?;
        settle(&app, &turns, &turn, "done").await?;
    }
    ensure!(lock(&state).failures.is_empty(), "rejections {:?}", lock(&state).failures);
    ensure!(rows(&app, &format!("{path}/files")).await?.len() == 1, "rejected files stored");
    lock(&state).hold = true;
    let held_turn = new_id();
    app.post(&turns, json!({"id": held_turn, "text": "Retain on restart"}))
        .await?;
    let held = model.parked().await?;
    ensure!(held.request.last_user_text() == "Retain on restart", "held task");
    app.restart().await?;
    drop(held);
    let interrupted = settle(&app, &turns, &held_turn, "interrupted").await?;
    ensure!(
        interrupted["generatedFiles"].as_array().map(Vec::len) == Some(1),
        "interrupted files {}",
        interrupted["generatedFiles"]
    );
    lock(&state).hold = false;
    app.post(
        &format!("{turns}/{held_turn}/resume"),
        json!({"id": new_id(), "revision": interrupted["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    let resumed = settle(&app, &turns, &held_turn, "done").await?;
    ensure!(
        resumed["generatedFiles"] == interrupted["generatedFiles"],
        "resume changed files"
    );
    ensure!(rows(&app, &format!("{path}/files")).await?.len() == 2, "resume duplicated a file");
    lock(&state).hold = true;
    let cancelled = new_id();
    app.post(&turns, json!({"id": cancelled, "text": "Retain on cancellation"}))
        .await?;
    let held = model.parked().await?;
    ensure!(held.request.last_user_text() == "Retain on cancellation", "held task");
    app.post(&format!("{turns}/{cancelled}/cancel"), json!({"id": new_id()}))
        .await?;
    let stopped = settle(&app, &turns, &cancelled, "cancelled").await?;
    drop(held);
    ensure!(
        stopped["generatedFiles"].as_array().map(Vec::len) == Some(1),
        "cancelled files"
    );
    let late = new_id();
    app.post(&turns, json!({"id": late, "text": "Late file creation"}))
        .await?;
    let release = model.parked().await?;
    ensure!(release.request.last_user_text() == "Late file creation", "late task");
    app.post(&format!("{turns}/{late}/cancel"), json!({"id": new_id()}))
        .await?;
    release.answer(Answer::tool_id(
        "artifact-call",
        "create_text_file",
        json!({"name": "Late file creation.txt", "text": "<script>literal text</script>\nSYNTHETIC OUTPUT"}),
    ));
    lock(&state).hold = false;
    let sentinel = new_id();
    app.post(&turns, json!({"id": sentinel, "text": "Next task after cancellation"}))
        .await?;
    settle(&app, &turns, &sentinel, "done").await?;
    ensure!(
        settle(&app, &turns, &late, "cancelled").await?["generatedFiles"]
            .as_array()
            .map(Vec::len)
            == Some(0),
        "late cancelled call created a file"
    );
    ensure!(rows(&app, &format!("{path}/files")).await?.len() == 4, "file count");
    lock(&state).hold = true;
    let earlier = new_id();
    app.post(&turns, json!({"id": earlier, "text": "Earlier task"}))
        .await?;
    // Stays open, like the Node provider's unanswered response.
    let _held = model.parked().await?;
    let earlier_row = row(&app, &turns, &earlier).await?;
    app.post(
        &format!("{turns}/{earlier}/pause"),
        json!({"id": new_id(), "revision": earlier_row["revision"]}),
    )
    .await?;
    lock(&state).hold = false;
    let later = new_id();
    app.post(&turns, json!({"id": later, "text": "Unrelated later task"}))
        .await?;
    let later_row = settle(&app, &turns, &later, "done").await?;
    lock(&state).unrelated = Some(id(&later_row["generatedFiles"][0])?);
    let paused = row(&app, &turns, &earlier).await?;
    app.post(
        &format!("{turns}/{earlier}/resume"),
        json!({"id": new_id(), "revision": paused["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    settle(&app, &turns, &earlier, "done").await?;
    ensure!(lock(&state).denied_later, "unrelated later file was readable");
    Ok(())
}

#[derive(Default)]
struct Avatar {
    generate: bool,
    args: Value,
    hold: bool,
    park: bool,
    continued: bool,
    result: Value,
    advertised: bool,
}

#[tokio::test]
async fn avatar_tool_scopes_files_journals_atomic_changes_and_preserves_user_changes_on_restart(
) -> Result<()> {
    use minarbot_testkit::base64::{engine::general_purpose::STANDARD, Engine};
    let png = minarbot_testkit::image::png(32, 32, [0x44, 0x88, 0x66, 0xff])?;
    let generated = minarbot_testkit::noise::noise_png(512, 512, 7)?;
    ensure!(generated.len() > 512 * 1024, "generated image too small: {}", generated.len());
    let state = Arc::new(Mutex::new(Avatar::default()));
    let model = GatedModel::start().await?;
    let encoded = STANDARD.encode(&generated);
    model.route("/v1/images/generations", move |_| {
        (200, json!({"data": [{"b64_json": encoded}]}))
    });
    let shared = state.clone();
    model.respond(move |request| {
        let mut script = lock(&shared);
        script.advertised = request.tool_names().iter().any(|name| name == "set_avatar");
        let last = last_result(request);
        if script.generate && last.is_none() {
            return Answer::tool_id("generate", "generate_image", json!({"prompt": "Synthetic avatar"}));
        }
        if let (true, Some(("generate", file))) =
            (script.generate, last.as_ref().map(|(id, value)| (id.as_str(), value)))
        {
            return Answer::tool_id("avatar", "set_avatar", json!({"image": file["id"], "revision": 5}));
        }
        if let Some((_, result)) = last {
            script.result = result;
            script.continued = true;
            if script.hold {
                return Answer::Park;
            }
            return Answer::text("Avatar result checked.");
        }
        if script.park {
            script.park = false;
            return Answer::Park;
        }
        Answer::tool_id("avatar", "set_avatar", script.args.clone())
    });
    let root = TestRoot::new()?;
    let config = gated(&model).option(
        "imageProvider",
        json!({"baseURL": model.base_url(), "model": "fixture-image"}),
    );
    let mut app = root.start(config.clone()).await?;
    let agent = id(&api_agent(&app, "Artist", "fixture").await?)?;
    let other = id(&api_agent(&app, "Other", "fixture").await?)?;
    let side = id(&app
        .post(&format!("/api/agents/{agent}/threads"), json!({"id": new_id(), "name": "Side"}))
        .await?)?;
    let room = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Room", "instructions": "Help", "members": [agent], "lead": agent}),
        )
        .await?)?;
    let path = format!("/api/agents/{agent}");
    let avatar = format!("{path}/avatar");
    let side_path = format!("/api/threads/{side}");
    let send = |path: &str, files: Vec<String>| {
        let turn = new_id();
        let body = json!({"id": turn, "text": "Use this as your avatar", "files": files});
        (format!("{path}/turns"), turn, body)
    };
    async fn put(app: &Server, path: &str, bytes: &[u8]) -> Result<String> {
        let file = new_id();
        let status = upload(app, &format!("{path}/files/{file}"), "image.png", bytes.to_vec()).await?;
        ensure!(status == 200, "upload {status}");
        Ok(file)
    }
    async fn run(app: &Server, (turns, turn, body): (String, String, Value), status: &str) -> Result<String> {
        app.post(&turns, body).await?;
        settle(app, &turns, &turn, status).await?;
        Ok(turn)
    }
    let result = |state: &Arc<Mutex<Avatar>>| lock(state).result.clone();
    let set_args = |state: &Arc<Mutex<Avatar>>, args: Value| lock(state).args = args;
    let image = put(&app, &path, &png).await?;
    set_args(&state, json!({"image": image, "revision": 0}));
    run(&app, send(&path, vec![image.clone()]), "done").await?;
    ensure!(lock(&state).advertised, "set_avatar not offered");
    ensure!(result(&state)["revision"] == 1 && result(&state)["present"] == true, "first {}", result(&state));
    let (status, bytes) = fetch(&app, &format!("{avatar}/image")).await?;
    ensure!(status == 200, "avatar image {status}");
    ensure!(minarbot_testkit::image::dimensions(&bytes)?.0 == 256, "avatar not resized");
    let unsent = put(&app, &path, &png).await?;
    set_args(&state, json!({"image": unsent, "revision": 1}));
    run(&app, send(&path, vec![]), "done").await?;
    ensure!(result(&state)["status"] == 404, "unsent {}", result(&state));
    set_args(&state, json!({"image": image, "revision": 1}));
    run(&app, send(&side_path, vec![]), "done").await?;
    ensure!(result(&state)["status"] == 404, "side {}", result(&state));
    set_args(&state, json!({"image": image, "revision": 0}));
    run(&app, send(&format!("/api/agents/{other}"), vec![]), "done").await?;
    ensure!(result(&state)["status"] == 404, "other agent {}", result(&state));
    set_args(&state, json!({"image": image, "revision": 0}));
    run(&app, send(&path, vec![]), "done").await?;
    ensure!(result(&state)["status"] == 409, "stale {}", result(&state));
    set_args(&state, json!({"image": null, "revision": 1, "agent": other}));
    run(&app, send(&path, vec![]), "done").await?;
    ensure!(result(&state)["status"] == 400, "extra field {}", result(&state));
    set_args(&state, json!({"image": null, "revision": 1}));
    // #87: the API runtime failed the turn on an unoffered tool; over MCP the
    // server refuses the call and the turn goes on.
    run(&app, send(&format!("/api/channels/{room}"), vec![]), "done").await?;
    ensure!(!lock(&state).advertised, "set_avatar offered in a channel");
    ensure!(result(&state)["status"] == 403, "channel call not refused: {}", result(&state));
    ensure!(app.get(&avatar).await? == json!({"revision": 1, "present": true}), "avatar after denials");
    let side_image = put(&app, &side_path, &png).await?;
    set_args(&state, json!({"image": side_image, "revision": 1}));
    run(&app, send(&side_path, vec![side_image.clone()]), "done").await?;
    ensure!(result(&state)["revision"] == 2, "side avatar {}", result(&state));
    set_args(&state, json!({"image": null, "revision": 2}));
    lock(&state).park = true;
    let (turns, conflict, body) = send(&path, vec![]);
    app.post(&turns, body).await?;
    let parked = model.parked().await?;
    app.post(&avatar, json!({"id": new_id(), "revision": 2, "image": STANDARD.encode(&png)}))
        .await?;
    parked.answer(Answer::tool_id("avatar", "set_avatar", json!({"image": null, "revision": 2})));
    settle(&app, &turns, &conflict, "done").await?;
    ensure!(result(&state)["status"] == 409, "conflict {}", result(&state));
    ensure!(app.get(&avatar).await?["revision"] == 3, "user change lost");
    let journal: Vec<Value> = Inspector::open(root.database())?
        .rows("SELECT result FROM flow_tool_calls WHERE turn=?", [&conflict])?
        .iter()
        .map(|row| serde_json::from_str(row["result"].as_str().unwrap_or("null")).unwrap_or(Value::Null))
        .collect();
    ensure!(journal == vec![result(&state)], "journal {journal:?}");
    set_args(&state, json!({"image": null, "revision": 3}));
    {
        let mut script = lock(&state);
        script.hold = true;
        script.continued = false;
    }
    let interrupted = new_id();
    app.post(&format!("{path}/turns"), json!({"id": interrupted, "text": "Remove avatar"}))
        .await?;
    let held = model.parked().await?;
    ensure!(lock(&state).continued, "result not observed");
    ensure!(app.get(&avatar).await? == json!({"revision": 4, "present": false}), "removal");
    app.restart().await?;
    drop(held);
    lock(&state).hold = false;
    app.post(&avatar, json!({"id": new_id(), "revision": 4, "image": STANDARD.encode(&png)}))
        .await?;
    let stopped = settle(&app, &format!("{path}/turns"), &interrupted, "interrupted").await?;
    app.post(
        &format!("{path}/turns/{interrupted}/resume"),
        json!({"id": new_id(), "revision": stopped["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    settle(&app, &format!("{path}/turns"), &interrupted, "done").await?;
    ensure!(result(&state) == json!({"revision": 4, "present": false}), "replayed {}", result(&state));
    ensure!(app.get(&avatar).await? == json!({"revision": 5, "present": true}), "user upload lost");
    set_args(&state, json!({"image": null, "revision": 5}));
    lock(&state).park = true;
    let cancelled = new_id();
    app.post(&format!("{path}/turns"), json!({"id": cancelled, "text": "Cancel"}))
        .await?;
    let parked = model.parked().await?;
    app.post(&format!("{path}/turns/{cancelled}/cancel"), json!({"id": new_id()}))
        .await?;
    parked.answer(Answer::tool_id("avatar", "set_avatar", json!({"image": null, "revision": 5})));
    settle(&app, &format!("{path}/turns"), &cancelled, "cancelled").await?;
    ensure!(app.get(&avatar).await?["revision"] == 5, "cancelled turn changed the avatar");
    lock(&state).generate = true;
    run(&app, send(&path, vec![]), "done").await?;
    lock(&state).generate = false;
    ensure!(model.route_calls("/v1/images/generations") == 1, "image calls");
    ensure!(result(&state)["revision"] == 6, "generated avatar {}", result(&state));
    set_args(&state, json!({"image": null, "revision": 6}));
    app.shutdown().await?;
    Fixture::open(root.database())?.execute_batch(
        "CREATE TRIGGER reject_avatar_journal BEFORE INSERT ON flow_tool_calls BEGIN SELECT RAISE(ABORT, 'synthetic journal failure'); END",
    )?;
    app.start_stopped(config).await?;
    // #87: the agent loop failed the turn on this storage error; over MCP the
    // harness gets the error result (status 500) and the turn goes on.
    run(&app, send(&path, vec![]), "done").await?;
    ensure!(result(&state)["status"] == 500, "journal failure result {}", result(&state));
    ensure!(
        app.get(&avatar).await? == json!({"revision": 6, "present": true}),
        "journal failure changed the avatar"
    );
    Ok(())
}

#[derive(Default)]
struct Sites {
    tool: String,
    args: Value,
    result: Value,
    hold: bool,
    observed: bool,
    advertised: bool,
    generate: bool,
    park: bool,
}

#[tokio::test]
async fn site_tools_bind_sent_files_and_owner_journal_results_and_never_replay_old_mutation_on_restart(
) -> Result<()> {
    let state = Arc::new(Mutex::new(Sites {
        tool: "publish_site".into(),
        args: json!({}),
        ..Sites::default()
    }));
    let model = GatedModel::start().await?;
    let shared = state.clone();
    model.respond(move |request| {
        let mut script = lock(&shared);
        let last = last_result(request);
        script.advertised = request.tool_names().contains(&script.tool);
        if script.generate && last.as_ref().is_none_or(|(id, _)| id == "artifact") {
            return match last {
                Some((_, generated)) => Answer::tool_id(
                    "site-call",
                    "publish_site",
                    json!({"file": generated["id"], "name": "Generated site"}),
                ),
                None => Answer::tool_id(
                    "artifact",
                    "create_text_file",
                    json!({"name": "generated.html", "text": "<h1>Generated page</h1>"}),
                ),
            };
        }
        if let Some((_, result)) = last {
            script.result = result.clone();
            script.observed = true;
            if script.hold {
                return Answer::Park;
            }
            return Answer::text(match result["path"].as_str() {
                Some(path) => format!("[Open site]({path})"),
                None => "Result checked.".into(),
            });
        }
        if script.park {
            script.park = false;
            return Answer::Park;
        }
        Answer::tool_id("site-call", &script.tool, script.args.clone())
    });
    let root = TestRoot::new()?;
    let mut app = root.start(gated(&model)).await?;
    let set = |tool: &str, args: Value| {
        let mut script = lock(&state);
        script.tool = tool.into();
        script.args = args;
    };
    let result = || lock(&state).result.clone();
    /// Send, wait for `status`, then replay the same command (receipt).
    async fn run(app: &Server, path: &str, files: &[&str], status: &str) -> Result<String> {
        let turn = new_id();
        let command = json!({"id": turn, "text": "Manage local site", "files": files});
        let turns = format!("{path}/turns");
        app.post(&turns, command.clone()).await?;
        settle(app, &turns, &turn, status).await?;
        app.post(&turns, command).await?;
        Ok(turn)
    }
    async fn put(app: &Server, path: &str, source: impl Into<Vec<u8>>, name: &str) -> Result<String> {
        let file = new_id();
        let status = upload(app, &format!("{path}/files/{file}"), name, source).await?;
        ensure!(status == 200, "upload {name}: {status}");
        Ok(file)
    }
    let a = id(&api_agent(&app, "Publisher", "fixture").await?)?;
    let b = id(&api_agent(&app, "Other", "fixture").await?)?;
    let path = format!("/api/agents/{a}");
    let other = format!("/api/agents/{b}");
    let side = id(&app
        .post(&format!("{path}/threads"), json!({"id": new_id(), "name": "Side"}))
        .await?)?;
    let side_path = format!("/api/threads/{side}");
    let room = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Room", "members": [a], "lead": a, "instructions": "Help"}),
        )
        .await?)?;
    let file = put(&app, &path, "<h1>Published</h1><script>bad()</script>", "page.html").await?;
    set("publish_site", json!({"file": file, "name": "Report"}));
    run(&app, &path, &[], "done").await?;
    ensure!(result()["status"] == 404, "unsent upload published: {}", result());
    run(&app, &path, &[&file], "done").await?;
    ensure!(lock(&state).advertised, "publish_site not offered");
    let site = result();
    ensure!(site["revision"] == 1, "site {site}");
    ensure!(
        app.get(&format!("{path}/sites")).await?["sites"].as_array().map(Vec::len) == Some(1),
        "sites"
    );
    let site_path = site["path"].as_str().ok_or("site path")?.to_owned();
    let (_, page) = fetch(&app, &site_path).await?;
    ensure!(String::from_utf8_lossy(&page).contains("<h1>Published</h1>"), "published page");
    set("publish_site", json!({"file": file, "name": "Side cannot use main file"}));
    run(&app, &side_path, &[], "done").await?;
    ensure!(result()["status"] == 404, "side used main file: {}", result());
    let other_file = put(&app, &other, "<p>Foreign</p>", "page.html").await?;
    set("publish_site", json!({"file": other_file, "name": "Foreign"}));
    run(&app, &path, &[], "done").await?;
    ensure!(result()["status"] == 404, "foreign file: {}", result());
    for (bytes, name, status, message) in [
        (vec![255u8], "invalid.html", 415, "Message attachment must be UTF-8 text."),
        (vec![b'x'; 262_145], "large.html", 413, "Total text attachment limit is 32 KiB per message."),
    ] {
        let invalid = put(&app, &path, bytes, name).await?;
        let reply = app
            .call(
                Method::POST,
                &format!("{path}/turns"),
                Some(json!({"id": new_id(), "text": "Manage local site", "files": [invalid]})),
            )
            .await?;
        ensure!(reply.status == status, "{name}: {}", reply.status);
        ensure!(reply.body["error"] == message, "{name}: {}", reply.body);
    }
    let invalid_type = put(&app, &path, "plain", "not-html.txt").await?;
    set("publish_site", json!({"file": invalid_type, "name": "Invalid"}));
    run(&app, &path, &[&invalid_type], "done").await?;
    ensure!(result()["status"] == 400, "non-HTML published: {}", result());
    ensure!(
        app.get(&format!("{path}/sites")).await?["sites"].as_array().map(Vec::len) == Some(1),
        "invalid site stored"
    );
    lock(&state).generate = true;
    run(&app, &path, &[], "done").await?;
    lock(&state).generate = false;
    ensure!(result()["name"] == "Generated site", "generated {}", result());
    let (_, generated) = fetch(&app, result()["path"].as_str().ok_or("generated path")?).await?;
    ensure!(String::from_utf8_lossy(&generated).contains("Generated page"), "generated page");
    let site_id = id(&site)?;
    set("publish_site", json!({"file": file, "name": "Changed", "site": site_id, "revision": 1}));
    run(&app, &path, &[], "done").await?;
    ensure!(result()["revision"] == 2, "update {}", result());
    run(&app, &path, &[], "done").await?;
    ensure!(result()["status"] == 409, "stale update {}", result());
    set("delete_site", json!({"site": site_id, "revision": 2}));
    run(&app, &other, &[], "done").await?;
    ensure!(result()["status"] == 404, "foreign delete {}", result());
    set("list_sites", json!({}));
    run(&app, &other, &[], "done").await?;
    ensure!(result()["sites"] == json!([]), "foreign list {}", result());
    run(&app, &path, &[], "done").await?;
    ensure!(
        result()["sites"]
            .as_array()
            .and_then(|sites| sites.iter().find(|row| row["id"] == site_id.as_str()))
            .is_some_and(|row| row["revision"] == 2),
        "own list {}",
        result()
    );
    set("publish_site", json!({"file": file, "name": "Denied channel"}));
    // #87: over MCP an unoffered tool is refused (403); the turn goes on.
    run(&app, &format!("/api/channels/{room}"), &[], "done").await?;
    ensure!(!lock(&state).advertised, "publish_site offered in a channel");
    ensure!(result()["status"] == 403, "channel call not refused: {}", result());
    let side_file = put(&app, &side_path, "<h1>Side</h1>", "page.html").await?;
    set("publish_site", json!({"file": side_file, "name": "Side site"}));
    run(&app, &side_path, &[&side_file], "done").await?;
    ensure!(result()["revision"] == 1, "side site {}", result());
    let side_site = id(&result())?;
    let side_panel = app.get(&format!("{side_path}/panel")).await?;
    let ids: Vec<&Value> = side_panel["sites"].as_array().ok_or("panel sites")?.iter().map(|row| &row["id"]).collect();
    ensure!(ids == [&json!(side_site)], "side panel {ids:?}");
    ensure!(
        !app.get(&format!("{path}/panel")).await?["sites"]
            .as_array()
            .is_some_and(|sites| sites.iter().any(|row| row["id"] == side_site.as_str())),
        "side site in main panel"
    );
    let (status, scoped) = fetch(&app, &format!("{side_path}/panel/sites/{side_site}")).await?;
    ensure!(status == 200 && String::from_utf8_lossy(&scoped).contains("Side"), "scoped panel site {status}");
    ensure!(
        fetch(&app, &format!("{path}/panel/sites/{side_site}")).await?.0 == 404,
        "side site through main panel"
    );
    set("publish_site", json!({"file": file, "name": "Durable", "site": site_id, "revision": 2}));
    {
        let mut script = lock(&state);
        script.hold = true;
        script.observed = false;
    }
    let interrupted = new_id();
    app.post(&format!("{path}/turns"), json!({"id": interrupted, "text": "Hold result"}))
        .await?;
    let held = model.parked().await?;
    ensure!(lock(&state).observed && result()["revision"] == 3, "held result {}", result());
    app.restart().await?;
    drop(held);
    lock(&state).hold = false;
    app.post(
        &format!("{path}/sites"),
        json!({"id": new_id(), "site": site_id, "revision": 3, "name": "User latest", "source": "<h1>User latest</h1>"}),
    )
    .await?;
    let paused = settle(&app, &format!("{path}/turns"), &interrupted, "interrupted").await?;
    app.post(
        &format!("{path}/turns/{interrupted}/resume"),
        json!({"id": new_id(), "revision": paused["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    settle(&app, &format!("{path}/turns"), &interrupted, "done").await?;
    ensure!(result()["revision"] == 3, "replayed mutation {}", result());
    ensure!(
        app.get(&format!("{path}/sites/{site_id}")).await?["revision"] == 4,
        "user edit lost"
    );
    set("delete_site", json!({"site": site_id, "revision": 4}));
    lock(&state).park = true;
    let cancelled = new_id();
    app.post(&format!("{path}/turns"), json!({"id": cancelled, "text": "Cancel deletion"}))
        .await?;
    let parked = model.parked().await?;
    app.post(&format!("{path}/turns/{cancelled}/cancel"), json!({"id": new_id()}))
        .await?;
    parked.answer(Answer::tool_id("site-call", "delete_site", json!({"site": site_id, "revision": 4})));
    settle(&app, &format!("{path}/turns"), &cancelled, "cancelled").await?;
    ensure!(
        app.get(&format!("{path}/sites/{site_id}")).await?["deleted"] == 0,
        "cancelled turn deleted the site"
    );
    run(&app, &path, &[], "done").await?;
    ensure!(result()["deleted"] == 1, "delete {}", result());
    ensure!(fetch(&app, &site_path).await?.0 == 404, "deleted site still served");
    Ok(())
}

#[derive(Default)]
struct Scenario {
    calls: Vec<String>,
    prompts: Vec<String>,
    failures: Vec<String>,
    target: String,
    url: String,
}

fn scenario(request: &ModelRequest, script: &mut Scenario) -> std::result::Result<Answer, String> {
    let system = request.system();
    script.prompts.push(system.clone());
    let first_line = system.lines().next().unwrap_or_default();
    let check = |ok: bool, what: &str| if ok { Ok(()) } else { Err(what.to_owned()) };
    // #87: `runtime: "api"` agents run on the Codex (gateway) instance.
    check(
        first_line.starts_with("<runtime_info>")
            && first_line.contains("through the codex runtime, model fixture"),
        "runtime_info line",
    )?;
    check(system.contains("ordinary inline Markdown links"), "inline links guidance")?;
    check(system.contains("Only the user's own messages"), "user message guidance")?;
    check(
        !system.contains("computer_screenshot") && !system.contains("start_workflow"),
        "unavailable tools described",
    )?;
    let user = request.last_user_text();
    let tools = request.tool_names();
    let result = request.tool_results().last().map(tool_json);
    if user == "plain" {
        check(tools.iter().any(|tool| tool == "run_shell"), "plain run_shell")?;
        check(!system.contains("Project convention"), "plain agent got workspace context")?;
        return Ok(Answer::text("Ready."));
    }
    check(system.contains("Project convention: check native tests"), "workspace context")?;
    check(tools.iter().any(|tool| tool == "run_shell"), "run_shell offered")?;
    check(system.contains("run_shell:"), "run_shell guidance")?;
    if let Some(value) = result {
        check(value["error"].is_null(), &format!("tool error {value}"))?;
        if user == "run checks" {
            check(
                value["verification"] == json!({"recorded": true, "evidenceInWorkLog": true}),
                &format!("verification {value}"),
            )?;
        }
        if let Some(id) = value["id"].as_str() {
            script.target = id.to_owned();
            script.url = value["url"].as_str().unwrap_or_default().to_owned();
        }
        return Ok(Answer::text(if user == "run checks" {
            "Checks passed.".to_owned()
        } else {
            format!("Open [hello.html]({}).", script.url)
        }));
    }
    let name = match user.as_str() {
        "show me hello.html" => "open_file_in_panel",
        "run checks" => "run_shell",
        _ => "create_text_file",
    };
    if name == "open_file_in_panel" {
        check(system.contains("without recreating or rewriting"), "panel guidance")?;
    }
    script.calls.push(name.to_owned());
    let args = match name {
        "open_file_in_panel" => json!({"id": script.target}),
        "run_shell" => json!({"command": "echo verified > checked.txt && test \"$(cat checked.txt)\" = verified", "check": true}),
        _ => json!({"name": "hello.html", "text": "<h1>Hello</h1>"}),
    };
    Ok(Answer::tool(name, args))
}

#[tokio::test]
async fn synthetic_scenarios_record_panel_without_duplicate_writes_inline_links_workspace_context_and_approved_shell(
) -> Result<()> {
    use minarbot_testkit::base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let workspace = TempWorkspace::new()?;
    workspace.write("AGENTS.md", "Project convention: check native tests.")?;
    let agent = new_id();
    let state = Arc::new(Mutex::new(Scenario::default()));
    let model = GatedModel::start().await?;
    let shared = state.clone();
    model.respond(move |request| {
        let mut script = lock(&shared);
        match scenario(request, &mut script) {
            Ok(answer) => answer,
            Err(failure) => {
                script.failures.push(failure);
                Answer::Status(500, json!({}))
            }
        }
    });
    let root = TestRoot::new()?;
    let app = root
        .start(gated(&model).workspace(&agent, workspace.path(), true))
        .await?;
    let path = format!("/api/agents/{agent}");
    let turns = format!("{path}/turns");
    let send = |text: &str| {
        let turn = new_id();
        (turn.clone(), json!({"id": turn, "text": text}))
    };
    let settle_ok = |turn: String| {
        let app = &app;
        let turns = turns.clone();
        let state = state.clone();
        async move {
            let row = app
                .wait_row(&turns, &turn, |row| row["status"] == "done" || row["status"] == "failed")
                .await?;
            ensure!(row["status"] == "done", "turn failed: {} ({:?})", row["error"], lock(&state).failures);
            Ok::<_, minarbot_testkit::Error>(row)
        }
    };
    app.post_expect(
        "/api/agents",
        json!({"runtime": "api", "id": agent, "name": "Scenario", "model": "fixture",
            "instructions": "Keep agent-specific guidance."}),
        201,
    )
    .await?;
    let (first, body) = send("save hello.html");
    app.post(&turns, body).await?;
    let first = settle_ok(first).await?;
    let reply = first["reply"].as_str().unwrap_or_default();
    let link = reply
        .strip_prefix("Open [hello.html](")
        .and_then(|rest| rest.strip_suffix(")."))
        .ok_or_else(|| format!("reply {reply}"))?;
    let parts: Vec<&str> = link.split('/').collect();
    ensure!(
        parts.len() == 6
            && parts[..4] == ["", "api", "agents", agent.as_str()]
            && parts[4] == "files"
            && parts[5].len() == 36,
        "link {link}"
    );
    let url = lock(&state).url.clone();
    let (_, bytes) = fetch(&app, &url).await?;
    ensure!(bytes == b"<h1>Hello</h1>", "file contents");
    let (opened, body) = send("show me hello.html");
    app.post(&turns, body).await?;
    let opened = settle_ok(opened).await?;
    let target = lock(&state).target.clone();
    ensure!(opened["panelFile"]["id"] == target.as_str(), "panel {}", opened["panelFile"]);
    ensure!(opened["generatedFiles"] == json!([]), "panel rewrote the file");
    ensure!(rows(&app, &format!("{path}/files")).await?.len() == 1, "duplicate file");
    ensure!(
        lock(&state).calls == ["create_text_file", "open_file_in_panel"],
        "calls {:?}",
        lock(&state).calls
    );
    let (shell_turn, body) = send("run checks");
    app.post(&turns, body).await?;
    let waiting = app
        .wait_row(&turns, &shell_turn, |row| {
            row["harnessApprovals"]
                .as_array()
                .is_some_and(|cards| cards.iter().any(|card| card["state"] == "approval"))
        })
        .await?;
    let approval = waiting["harnessApprovals"][0].clone();
    let token = URL_SAFE_NO_PAD.encode(uuid_bytes());
    let session = new_id();
    Fixture::open(root.database())?.connection().execute(
        "INSERT INTO mcp_server_tokens(id,token_hash,kind,name,agent,turn,scopes,created,expires,request_digest) VALUES (?,?,'session','',?,?,?,?,?,'')",
        (
            &session,
            minarbot_testkit::protocol::sha256_hex(token.as_bytes()),
            &agent,
            &shell_turn,
            json!(["orchestration"]).to_string(),
            FIXED_CLOCK,
            FIXED_CLOCK + 60_000,
        ),
    )?;
    let native: Value = app
        .client()?
        .post(app.url("/mcp"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .body(
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "open_file_in_panel", "arguments": {"id": target}}})
            .to_string(),
        )
        .send()
        .await?
        .json()
        .await?;
    ensure!(
        native["result"]["structuredContent"]["id"] == target.as_str(),
        "native panel call {native}"
    );
    app.post(
        &format!("{turns}/{shell_turn}/harness-approvals/{}", id(&approval)?),
        json!({"id": new_id(), "revision": approval["revision"], "decision": "allow"}),
    )
    .await?;
    let checked = settle_ok(shell_turn).await?;
    ensure!(
        lock(&state).calls.iter().filter(|name| *name == "run_shell").count() == 1,
        "run_shell calls {:?}",
        lock(&state).calls
    );
    ensure!(
        checked["activity"]["artifacts"].as_array().map(Vec::len) == Some(1),
        "artifacts {}",
        checked["activity"]["artifacts"]
    );
    ensure!(workspace.read("checked.txt")? == "verified\n", "check output");
    let plain = id(&api_agent(&app, "Plain", "fixture").await?)?;
    let plain_turns = format!("/api/agents/{plain}/turns");
    let turn = new_id();
    app.post(&plain_turns, json!({"id": turn, "text": "plain"})).await?;
    let plain_row = app
        .wait_row(&plain_turns, &turn, |row| row["status"] == "done" || row["status"] == "failed")
        .await?;
    ensure!(plain_row["status"] == "done", "plain failed: {}", plain_row["error"]);
    let script = lock(&state);
    ensure!(
        script.prompts.iter().any(|prompt| !prompt.contains("Project convention")),
        "every prompt had workspace context"
    );
    ensure!(script.failures.is_empty(), "provider checks {:?}", script.failures);
    Ok(())
}

fn uuid_bytes() -> Vec<u8> {
    format!("{}{}", new_id(), new_id()).into_bytes()[..32].to_vec()
}
