//! Manual and scheduled routine runs, the routine hub and deletion (port of
//! `app/routines.test.ts`).

use crate::sa_support::{api_agent, id, lock, offers, web_dist, Shared, DIST_ICON};
use minarbot_testkit::{
    db::Fixture,
    model::{Reply, ScriptedModel},
    prelude::*,
    Server, FIXED_CLOCK,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

fn with(base: &Value, fields: Value) -> Value {
    let mut value = base.clone();
    if let (Some(value), Some(fields)) = (value.as_object_mut(), fields.as_object()) {
        value.extend(fields.clone());
    }
    value
}

async fn runs(app: &Server, routine: &str) -> Result<Value> {
    app.get(&format!("/api/routines/{routine}/runs")).await
}

async fn first_run(app: &Server, routine: &str, status: &str) -> Result<Value> {
    Ok(app
        .wait_get(&format!("/api/routines/{routine}/runs"), |rows| {
            rows[0]["status"] == status
        })
        .await?[0]
        .clone())
}

fn find(rows: &Value, id: &str) -> Value {
    rows.as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == id).cloned())
        .unwrap_or(Value::Null)
}

#[derive(Default)]
struct Manual {
    quiet_without_tool: usize,
    /// (turn text, tool result) of every step after a tool call.
    results: Vec<(String, Value)>,
}

fn result_for(state: &Shared<Manual>, text: &str) -> Result<Value> {
    Ok(lock(state)?
        .results
        .iter()
        .rev()
        .find(|(turn, _)| turn == text)
        .map(|(_, result)| result.clone())
        .unwrap_or(Value::Null))
}

/// #87: an unoffered tool comes back to the harness as an MCP error.
fn unauthorized() -> Value {
    json!({"error": "MCP authorization failed.", "status": 403})
}

#[tokio::test]
async fn manual_routine_runs_keep_schedule_deduplicate_across_restart_and_enforce_revision_and_overlap(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    let state: Shared<Manual> = Shared::default();
    let script = state.clone();
    model.respond(move |request| {
        let last = request.messages().last().cloned().unwrap_or(Value::Null);
        let text = request.last_text();
        if text == "HOLD" {
            return Reply::hold();
        }
        if text == "QUIET" {
            if !offers(request, "no_update") {
                if let Ok(mut state) = script.lock() {
                    state.quiet_without_tool += 1;
                }
            }
            return Reply::tool_call(
                "manual-quiet",
                "no_update",
                json!({"reason": "Checks finished without updates."}),
            );
        }
        if request
            .messages()
            .iter()
            .any(|message| message["role"] == "user" && message["content"] == "TASK_FAILURE")
        {
            let saved = request
                .messages()
                .iter()
                .any(|message| message["role"] == "tool");
            return if saved {
                Reply::tool_call(
                    "failed-outcome",
                    "report_failure",
                    json!({"reason": "Required service access is missing."}),
                )
            } else {
                Reply::tool_call(
                    "partial-output",
                    "create_text_file",
                    json!({"name": "partial.txt", "text": "Collected evidence before failure."}),
                )
            };
        }
        if text == "DIRECT_FAILURE" || text == "INVALID_FAILURE" {
            return Reply::tool_call("invalid-outcome", "report_failure", json!({"reason": ""}));
        }
        if last["role"] == "tool" {
            let result: Value = serde_json::from_str(last["content"].as_str().unwrap_or("null"))
                .unwrap_or_default();
            if let Ok(mut state) = script.lock() {
                state.results.push((request.last_user_text(), result));
            }
        }
        Reply::text("Test complete")
    });
    let root = TestRoot::new()?;
    let options = ServerConfig::new().model(&model).concurrency(1);
    let mut app = root.start(options.clone()).await?;
    let agent = id(&api_agent(&app, "Tester").await?)?;
    let list = format!("/api/agents/{agent}/routines");
    let create = json!({"id": new_id(), "name": "Manual", "prompt": "TEST",
        "nextAt": FIXED_CLOCK + 3_600_000, "intervalSeconds": 60, "enabled": false});
    let routine = app.post_expect(&list, create.clone(), 200).await?;
    let routine_id = id(&routine)?;
    let path = format!("/api/routines/{routine_id}/test");
    let request = json!({"id": new_id(), "revision": routine["revision"]});
    app.post_expect(&path, with(&request, json!({"revision": 0})), 400)
        .await?;
    app.post_expect(&path, with(&request, json!({"revision": 2})), 409)
        .await?;
    let result = app.post_expect(&path, request.clone(), 200).await?;
    ensure!(
        app.post_expect(&path, request.clone(), 200).await? == result,
        "manual run replay"
    );
    app.post_expect(&path, with(&request, json!({"revision": 2})), 409)
        .await?;
    let rows = app
        .wait_get(&format!("/api/routines/{routine_id}/runs"), |rows| {
            rows[0]["status"] == "done"
        })
        .await?;
    ensure!(rows[0]["status"] == "done", "run: {rows}");
    ensure!(
        rows[0]["scheduled_at"].is_null(),
        "manual run scheduled: {rows}"
    );
    ensure!(rows.as_array().map(Vec::len) == Some(1), "runs: {rows}");
    ensure!(
        app.get(&list).await?[0] == routine,
        "manual run changed the routine"
    );
    ensure!(model.calls() == 1, "model calls: {}", model.calls());
    ensure!(
        app.get(&format!("/api/agents/{agent}/turns")).await? == json!([]),
        "manual run entered main history"
    );
    app.restart().await?;
    ensure!(
        app.post_expect(&path, request.clone(), 200).await? == result,
        "receipt after restart"
    );
    ensure!(model.calls() == 1, "restart replayed: {}", model.calls());

    let quiet = app
        .post_expect(
            &list,
            with(&create, json!({"id": new_id(), "prompt": "QUIET"})),
            200,
        )
        .await?;
    let quiet_id = id(&quiet)?;
    app.post_expect(
        &format!("/api/routines/{quiet_id}/test"),
        json!({"id": new_id(), "revision": quiet["revision"]}),
        200,
    )
    .await?;
    let row = first_run(&app, &quiet_id, "done").await?;
    ensure!(row["no_update"] == 1, "quiet run: {row}");
    ensure!(row["error"].is_null(), "quiet error: {row}");
    ensure!(
        find(&app.get(&list).await?, &quiet_id) == quiet,
        "quiet routine changed"
    );

    let failed = app
        .post_expect(
            &list,
            with(&create, json!({"id": new_id(), "prompt": "TASK_FAILURE"})),
            200,
        )
        .await?;
    let failed_id = id(&failed)?;
    let failed_request = json!({"id": new_id(), "revision": failed["revision"]});
    let failed_run = app
        .post_expect(
            &format!("/api/routines/{failed_id}/test"),
            failed_request.clone(),
            200,
        )
        .await?;
    let row = first_run(&app, &failed_id, "failed").await?;
    ensure!(row["status"] == "failed", "failure run: {row}");
    ensure!(row["no_update"] == 0, "failure no_update: {row}");
    ensure!(
        row["error"] == "Required service access is missing.",
        "failure: {row}"
    );
    let thread = failed_run["thread"]
        .as_str()
        .ok_or("run thread")?
        .to_owned();
    let partial =
        app.get(&format!("/api/threads/{thread}/turns")).await?[0]["generatedFiles"].clone();
    ensure!(
        partial.as_array().map(Vec::len) == Some(1),
        "partial files: {partial}"
    );
    let file = app
        .request(
            minarbot_testkit::http::Method::GET,
            &format!(
                "/api/threads/{thread}/files/{}",
                partial[0]["id"].as_str().unwrap_or_default()
            ),
        )?
        .send()
        .await?
        .text()
        .await?;
    ensure!(
        file == "Collected evidence before failure.",
        "partial file: {file}"
    );
    app.restart().await?;
    ensure!(
        app.post_expect(
            &format!("/api/routines/{failed_id}/test"),
            failed_request,
            200
        )
        .await?
            == failed_run,
        "failure receipt"
    );
    ensure!(
        runs(&app, &failed_id).await?[0]["error"] == "Required service access is missing.",
        "failure after restart"
    );

    let invalid = app
        .post_expect(
            &list,
            with(
                &create,
                json!({"id": new_id(), "prompt": "INVALID_FAILURE"}),
            ),
            200,
        )
        .await?;
    let invalid_id = id(&invalid)?;
    app.post_expect(
        &format!("/api/routines/{invalid_id}/test"),
        json!({"id": new_id(), "revision": invalid["revision"]}),
        200,
    )
    .await?;
    let row = first_run(&app, &invalid_id, "done").await?;
    ensure!(
        row["no_update"] == 0 && row["error"].is_null(),
        "invalid failure: {row}"
    );
    ensure!(
        result_for(&state, "INVALID_FAILURE")?
            == json!({"error": "Field reason is invalid (maximum 2000 characters).", "status": 400}),
        "invalid outcome result: {}",
        result_for(&state, "INVALID_FAILURE")?
    );

    let direct = app
        .post_expect(
            &format!("/api/agents/{agent}/turns"),
            json!({"id": new_id(), "text": "DIRECT_FAILURE"}),
            202,
        )
        .await?;
    // #87: outcome tools exist only in routine runs; a direct turn's call is
    // refused over MCP and records no outcome (was a failed turn).
    let direct_turn = app
        .wait_status(&format!("/api/agents/{agent}/turns"), &id(&direct)?, "done")
        .await?;
    ensure!(direct_turn["no_update"] == 0, "direct: {direct_turn}");
    ensure!(
        direct_turn["error"].is_null(),
        "direct error: {direct_turn}"
    );
    ensure!(
        result_for(&state, "DIRECT_FAILURE")? == unauthorized(),
        "direct outcome result: {}",
        result_for(&state, "DIRECT_FAILURE")?
    );
    ensure!(
        !model
            .requests()
            .iter()
            .filter(|request| request.last_user_text() == "DIRECT_FAILURE")
            .any(|request| offers(request, "report_failure")),
        "report_failure offered outside a routine"
    );

    let hold = app
        .post_expect(
            &list,
            with(
                &create,
                json!({"id": new_id(), "prompt": "HOLD", "enabled": true}),
            ),
            200,
        )
        .await?;
    let hold_path = format!("/api/routines/{}/test", id(&hold)?);
    let held = app
        .post_expect(
            &hold_path,
            json!({"id": new_id(), "revision": hold["revision"]}),
            200,
        )
        .await?;
    app.post_expect(
        &hold_path,
        json!({"id": new_id(), "revision": hold["revision"]}),
        409,
    )
    .await?;
    ensure!(
        find(&app.get(&list).await?, &id(&hold)?) == hold,
        "held routine changed"
    );
    let held_turn = format!(
        "/api/threads/{}/turns/{}",
        held["thread"].as_str().unwrap_or_default(),
        held["turn"].as_str().unwrap_or_default()
    );
    app.post(
        &format!("{held_turn}/pause"),
        json!({"id": new_id(), "revision": 1}),
    )
    .await?;
    ensure!(
        runs(&app, &id(&hold)?).await?[0]["paused"] == 1,
        "paused run"
    );
    app.post_expect(
        &hold_path,
        json!({"id": new_id(), "revision": hold["revision"]}),
        409,
    )
    .await?;
    app.post(&format!("{held_turn}/cancel"), json!({"id": new_id()}))
        .await?;
    let again = app
        .post_expect(
            &hold_path,
            json!({"id": new_id(), "revision": hold["revision"]}),
            200,
        )
        .await?;
    ensure!(again["turn"] != held["turn"], "cancelled run reused");
    app.post(
        &format!(
            "/api/threads/{}/turns/{}/cancel",
            again["thread"].as_str().unwrap_or_default(),
            again["turn"].as_str().unwrap_or_default()
        ),
        json!({"id": new_id()}),
    )
    .await?;
    let full = id(&api_agent(&app, "Full queue").await?)?;
    let full_routine = app
        .post_expect(
            &format!("/api/agents/{full}/routines"),
            with(&create, json!({"id": new_id()})),
            200,
        )
        .await?;
    for _ in 0..100 {
        app.post_expect(
            &format!("/api/agents/{full}/turns"),
            json!({"id": new_id(), "text": "HOLD"}),
            202,
        )
        .await?;
    }
    app.post_expect(
        &format!("/api/routines/{}/test", id(&full_routine)?),
        json!({"id": new_id(), "revision": full_routine["revision"]}),
        409,
    )
    .await?;
    ensure!(
        runs(&app, &id(&full_routine)?).await? == json!([]),
        "full queue ran"
    );

    app.restart_with(ServerConfig::new()).await?;
    ensure!(
        app.post_expect(&path, request.clone(), 200).await? == result,
        "receipt without provider"
    );
    app.shutdown().await?;
    {
        let db = Fixture::open(app.database())?;
        db.execute_batch(
            "DELETE FROM routine_runs; DROP TABLE routine_runs;
             CREATE TABLE routine_runs(routine TEXT NOT NULL REFERENCES routines(id), scheduled_at INTEGER NOT NULL,
               turn TEXT NOT NULL UNIQUE REFERENCES turns(id), missed INTEGER NOT NULL, PRIMARY KEY(routine,scheduled_at)) STRICT;",
        )?;
        db.execute(
            "INSERT INTO routine_runs VALUES (?,?,?,0)",
            rusqlite_params(
                &routine_id,
                create["nextAt"].as_i64().unwrap_or_default(),
                &result,
            ),
        )?;
    }
    app.start_stopped(options).await?;
    ensure!(
        runs(&app, &routine_id).await?[0]["scheduled_at"] == create["nextAt"],
        "migrated scheduled receipt"
    );
    app.post(
        &path,
        json!({"id": new_id(), "revision": routine["revision"]}),
    )
    .await?;
    ensure!(
        runs(&app, &routine_id).await?.as_array().map(Vec::len) == Some(2),
        "run after migration"
    );
    ensure!(
        lock(&state)?.quiet_without_tool == 0,
        "no_update not offered"
    );
    Ok(())
}

fn rusqlite_params(routine: &str, at: i64, result: &Value) -> (String, i64, String) {
    (
        routine.to_owned(),
        at,
        result["turn"].as_str().unwrap_or_default().to_owned(),
    )
}

#[tokio::test]
async fn routine_occurrences_persist_once_and_isolate_each_run_from_main_history() -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|request| match request.last_text().as_str() {
        "HOLD" => Reply::hold(),
        "BROKEN_CHECK" => Reply::status(503, json!({})),
        "NO_UPDATE" => Reply::tool_call(
            "quiet-result",
            "no_update",
            json!({"reason": "Synthetic checks finished; nothing changed."}),
        ),
        _ => Reply::text("Routine complete"),
    });
    let root = TestRoot::new()?;
    let dist = web_dist(&root)?;
    // Scheduled occurrences need the server clock to advance.
    let options = ServerConfig::new()
        .real_clock()
        .model(&model)
        .concurrency(1)
        .option("dist", json!(dist));
    let mut app = root.start(options).await?;
    let html = app.client()?.get(app.url("/")).send().await?.text().await?;
    let icon = html
        .split("rel=\"icon\"")
        .nth(1)
        .and_then(|rest| rest.split("href=\"").nth(1))
        .and_then(|rest| rest.split('"').next())
        .ok_or_else(|| format!("no icon in {html}"))?
        .to_owned();
    ensure!(icon == DIST_ICON, "icon {icon}");
    let response = app.client()?.get(app.url(&icon)).send().await?;
    ensure!(
        response.status() == 200,
        "icon status {}",
        response.status()
    );
    ensure!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            == Some("image/svg+xml"),
        "icon type {:?}",
        response.headers().get("content-type")
    );
    let agent = id(&api_agent(&app, "Scheduler").await?)?;
    let list = format!("/api/agents/{agent}/routines");
    let create = json!({"id": new_id(), "name": "Daily check", "prompt": "ROUTINE_ONLY",
        "nextAt": now() - 120_001, "intervalSeconds": 60, "enabled": true});
    app.post_expect(&list, with(&create, json!({"intervalSeconds": 0})), 400)
        .await?;
    let routine = app.post_expect(&list, create.clone(), 200).await?;
    let routine_id = id(&routine)?;
    ensure!(
        app.post_expect(&list, create.clone(), 200).await? == routine,
        "create replay"
    );
    app.post_expect(&list, with(&create, json!({"prompt": "different"})), 409)
        .await?;
    let first = first_run(&app, &routine_id, "done").await?;
    ensure!(
        first["missed"].as_i64().is_some_and(|missed| missed >= 2),
        "missed: {first}"
    );
    let requests = model.requests();
    ensure!(requests.len() == 1, "calls: {}", requests.len());
    ensure!(
        requests[0]
            .texts()
            .iter()
            .filter(|text| *text == "ROUTINE_ONLY")
            .count()
            == 1,
        "prompt count: {:?}",
        requests[0].texts()
    );
    ensure!(
        app.get(&format!("/api/agents/{agent}/turns")).await? == json!([]),
        "routine entered main history"
    );
    let current = app.get(&list).await?[0].clone();
    ensure!(
        current["next_at"].as_i64().is_some_and(|next| next > now()),
        "next: {current}"
    );
    ensure!(current["revision"] == 2, "revision: {current}");
    let edit = json!({"id": new_id(), "name": current["name"], "prompt": "SECOND_RUN",
        "nextAt": now() - 1, "intervalSeconds": null, "enabled": true, "revision": current["revision"]});
    let edit_path = format!("/api/routines/{routine_id}/edit");
    app.post_expect(&edit_path, with(&edit, json!({"revision": 1})), 409)
        .await?;
    let saved = app.post_expect(&edit_path, edit.clone(), 200).await?;
    ensure!(
        app.post_expect(&edit_path, edit.clone(), 200).await? == saved,
        "edit replay"
    );
    let rows = app
        .wait_get(&format!("/api/routines/{routine_id}/runs"), |rows| {
            rows.as_array().map(Vec::len) == Some(2) && rows[0]["status"] == "done"
        })
        .await?;
    let second = rows[0].clone();
    ensure!(second["thread"] != first["thread"], "runs share a thread");
    let requests = model.requests();
    ensure!(
        !requests
            .get(1)
            .is_some_and(|request| request.texts().iter().any(|text| text == "ROUTINE_ONLY")),
        "second run saw the first prompt"
    );
    ensure!(
        app.get(&list).await?[0]["enabled"] == 0,
        "one-shot stayed enabled"
    );
    app.restart().await?;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    ensure!(model.calls() == 2, "restart reran: {}", model.calls());
    ensure!(
        runs(&app, &routine_id).await?.as_array().map(Vec::len) == Some(2),
        "runs after restart"
    );
    let latest = app.get(&list).await?[0].clone();
    app.post_expect(
        &edit_path,
        with(
            &edit,
            json!({"id": new_id(), "revision": latest["revision"]}),
        ),
        409,
    )
    .await?;
    let paused = app
        .post_expect(
            &list,
            with(&create, json!({"id": new_id(), "enabled": false})),
            200,
        )
        .await?;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    ensure!(
        runs(&app, &id(&paused)?).await? == json!([]),
        "paused routine ran"
    );
    let main = new_id();
    app.post_expect(
        &format!("/api/agents/{agent}/turns"),
        json!({"id": main, "text": "HOLD"}),
        202,
    )
    .await?;
    let queued = app
        .post_expect(
            &list,
            with(
                &create,
                json!({"id": new_id(), "prompt": "MUST_NOT_RUN", "nextAt": now() - 1, "intervalSeconds": null}),
            ),
            200,
        )
        .await?;
    let queued_id = id(&queued)?;
    let queued_run = app
        .wait_get(&format!("/api/routines/{queued_id}/runs"), |rows| {
            rows.as_array().is_some_and(|rows| !rows.is_empty())
        })
        .await?[0]
        .clone();
    ensure!(queued_run["status"] == "queued", "queued run: {queued_run}");
    let latest_queued = find(&app.get(&list).await?, &queued_id);
    app.post(
        &format!("/api/routines/{queued_id}/edit"),
        json!({"id": new_id(), "revision": latest_queued["revision"], "name": queued["name"],
            "prompt": queued["prompt"], "nextAt": now() + 60_000, "intervalSeconds": null, "enabled": false}),
    )
    .await?;
    ensure!(
        runs(&app, &queued_id).await?[0]["status"] == "cancelled",
        "disabled run not cancelled"
    );
    app.post(
        &format!("/api/agents/{agent}/turns/{main}/cancel"),
        json!({"id": new_id()}),
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    ensure!(
        !model
            .requests()
            .iter()
            .any(|request| request.texts().iter().any(|text| text == "MUST_NOT_RUN")),
        "cancelled run reached the model"
    );
    let quiet = app
        .post_expect(
            &list,
            with(
                &create,
                json!({"id": new_id(), "name": "Quiet check", "prompt": "NO_UPDATE",
                    "nextAt": now() - 1, "intervalSeconds": null}),
            ),
            200,
        )
        .await?;
    let quiet_id = id(&quiet)?;
    let quiet_run = first_run(&app, &quiet_id, "done").await?;
    ensure!(quiet_run["no_update"] == 1, "quiet: {quiet_run}");
    ensure!(quiet_run["error"].is_null(), "quiet error: {quiet_run}");
    ensure!(
        quiet_run["reply"] == "Synthetic checks finished; nothing changed.",
        "quiet reply: {quiet_run}"
    );
    let broken = app
        .post_expect(
            &list,
            with(
                &create,
                json!({"id": new_id(), "prompt": "BROKEN_CHECK", "nextAt": now() - 1, "intervalSeconds": null}),
            ),
            200,
        )
        .await?;
    let failure = first_run(&app, &id(&broken)?, "failed").await?;
    ensure!(failure["no_update"] == 0, "broken: {failure}");
    let direct = new_id();
    app.post_expect(
        &format!("/api/agents/{agent}/turns"),
        json!({"id": direct, "text": "NO_UPDATE"}),
        202,
    )
    .await?;
    // #87: refused over MCP outside routine runs (was a failed turn).
    let direct_turn = app
        .wait_status(&format!("/api/agents/{agent}/turns"), &direct, "done")
        .await?;
    ensure!(
        direct_turn["no_update"] == 0,
        "direct no_update: {direct_turn}"
    );
    let refusal = model
        .requests()
        .iter()
        .rev()
        .find(|request| {
            request.last_user_text() == "NO_UPDATE"
                && request
                    .messages()
                    .last()
                    .is_some_and(|message| message["role"] == "tool")
        })
        .and_then(crate::sa_support::last_tool_result)
        .unwrap_or(Value::Null);
    ensure!(
        refusal == unauthorized(),
        "direct no_update result: {refusal}"
    );
    app.restart().await?;
    ensure!(
        runs(&app, &quiet_id).await?[0]["no_update"] == 1,
        "quiet after restart"
    );
    let future = app
        .post_expect(
            &list,
            with(
                &create,
                json!({"id": new_id(), "name": "Review, reports; planning",
                    "prompt": "PRIVATE_CALENDAR_PROMPT", "nextAt": 1_893_553_440_000_i64,
                    "intervalSeconds": 3600}),
            ),
            200,
        )
        .await?;
    let calendar = app
        .request(
            minarbot_testkit::http::Method::GET,
            &format!("{list}/calendar.ics"),
        )?
        .send()
        .await?;
    ensure!(
        calendar
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            == Some("text/calendar; charset=utf-8"),
        "calendar type {:?}",
        calendar.headers().get("content-type")
    );
    let text = calendar.text().await?;
    ensure!(
        text.contains(&format!("UID:{}@minarbot.local", id(&future)?)),
        "uid: {text}"
    );
    ensure!(text.contains("DTSTART:20300102T030400Z"), "start: {text}");
    ensure!(
        text.contains("RRULE:FREQ=SECONDLY;INTERVAL=3600"),
        "rule: {text}"
    );
    ensure!(!text.contains("PRIVATE_CALENDAR_PROMPT"), "prompt leaked");
    ensure!(
        !text.contains(&format!("UID:{}@minarbot.local", id(&paused)?)),
        "paused routine listed"
    );
    Ok(())
}

#[tokio::test]
async fn routine_hub_pages_visible_local_owners_filters_safely_and_never_returns_secrets(
) -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let a = id(&api_agent(&app, "Ada").await?)?;
    let b = id(&api_agent(&app, "Grace").await?)?;
    let hidden = id(&api_agent(&app, "Hidden").await?)?;
    let base = json!({"name": "Review", "prompt": "Saved instructions",
        "nextAt": FIXED_CLOCK + 3_600_000, "intervalSeconds": null, "enabled": true});
    let first = app
        .post(
            &format!("/api/agents/{a}/routines"),
            with(&base, json!({"id": new_id(), "trigger": "webhook"})),
        )
        .await?;
    let first_id = id(&first)?;
    let hook = app
        .post(
            &format!("/api/routines/{first_id}/webhook"),
            json!({"id": new_id(), "revision": 0, "rotate": true, "eventName": "", "filters": []}),
        )
        .await?;
    app.post(
        &format!("/api/routines/{first_id}/flow"),
        json!({"id": new_id(), "revision": 0, "links": [{"from": a, "to": b}]}),
    )
    .await?;
    for i in 0..22 {
        let owner = if i % 2 == 1 { &a } else { &b };
        let trigger = match i {
            21 => "manual",
            20 => "script",
            _ => "schedule",
        };
        app.post(
            &format!("/api/agents/{owner}/routines"),
            with(
                &base,
                json!({"id": new_id(), "name": format!("Review {i}"),
                    "intervalSeconds": if i % 2 == 1 { json!(600) } else { Value::Null },
                    "enabled": i % 3 == 0, "trigger": trigger}),
            ),
        )
        .await?;
    }
    app.post(
        &format!("/api/agents/{hidden}/routines"),
        with(&base, json!({"id": new_id(), "name": "Hidden routine"})),
    )
    .await?;
    app.post(
        &format!("/api/agents/{hidden}/visibility"),
        json!({"id": new_id(), "revision": 0, "hidden": true}),
    )
    .await?;
    let page = app.get_expect("/api/routines", 200).await?;
    ensure!(
        page["routines"].as_array().map(Vec::len) == Some(20),
        "page size"
    );
    let after = page["next_after"]
        .as_str()
        .ok_or("no next_after")?
        .to_owned();
    let next = app
        .get_expect(&format!("/api/routines?after={after}"), 200)
        .await?;
    ensure!(
        next["routines"].as_array().map(Vec::len) == Some(3),
        "second page"
    );
    ensure!(
        next["next_after"].is_null(),
        "next_after: {}",
        next["next_after"]
    );
    let all: Vec<Value> = page["routines"]
        .as_array()
        .into_iter()
        .chain(next["routines"].as_array())
        .flatten()
        .cloned()
        .collect();
    let mut ids: Vec<String> = all.iter().map(|row| row["id"].to_string()).collect();
    ids.sort();
    ids.dedup();
    ensure!(ids.len() == 23, "distinct routines: {}", ids.len());
    ensure!(
        all.iter()
            .all(|row| row["agent"] == a.as_str() || row["agent"] == b.as_str())
            && all.iter().any(|row| row["agent"] == a.as_str())
            && all.iter().any(|row| row["agent"] == b.as_str()),
        "owners"
    );
    ensure!(
        all.iter()
            .all(|row| row["owner_name"] == "Ada" || row["owner_name"] == "Grace"),
        "owner names"
    );
    ensure!(
        find(&json!(all), &first_id)["flow_agents"] == 2,
        "flow agents"
    );
    let secret = hook["secret"].as_str().ok_or("no webhook secret")?;
    ensure!(
        !json!(all).to_string().contains(secret),
        "webhook secret listed"
    );
    ensure!(
        all.iter()
            .all(|row| row["last_created"].is_null() && row["last_thread"].is_null()),
        "last run fields"
    );
    let own = app
        .get_expect(&format!("/api/routines?agent={b}"), 200)
        .await?;
    ensure!(
        own["routines"].as_array().is_some_and(
            |rows| !rows.is_empty() && rows.iter().all(|row| row["agent"] == b.as_str())
        ),
        "agent filter: {own}"
    );
    ensure!(
        app.get_expect(&format!("/api/routines?agent={hidden}"), 200)
            .await?["routines"]
            == json!([]),
        "hidden agent listed"
    );
    for enabled in [0, 1] {
        let rows = app
            .get_expect(&format!("/api/routines?enabled={enabled}"), 200)
            .await?;
        ensure!(
            rows["routines"]
                .as_array()
                .is_some_and(|rows| rows.iter().all(|row| row["enabled"] == enabled)),
            "enabled={enabled}: {rows}"
        );
    }
    ensure!(
        app.get_expect("/api/routines?trigger=webhook", 200).await?["routines"][0]["id"]
            == first_id.as_str(),
        "webhook filter"
    );
    for trigger in ["manual", "script"] {
        ensure!(
            app.get_expect(&format!("/api/routines?trigger={trigger}"), 200)
                .await?["routines"][0]["trigger"]
                == trigger,
            "{trigger} filter"
        );
    }
    let recurring = app
        .get_expect("/api/routines?trigger=recurring", 200)
        .await?;
    ensure!(
        recurring["routines"]
            .as_array()
            .is_some_and(|rows| rows.iter().all(|row| row["trigger"] == "schedule"
                && row["interval_ms"].as_i64().is_some_and(|ms| ms != 0))),
        "recurring: {recurring}"
    );
    let once = app.get_expect("/api/routines?trigger=once", 200).await?;
    ensure!(
        once["routines"].as_array().is_some_and(|rows| rows
            .iter()
            .all(|row| row["trigger"] == "schedule" && row["interval_ms"].is_null())),
        "once: {once}"
    );
    ensure!(
        app.get_expect("/api/routines?search=REVIEW%2021", 200)
            .await?["routines"]
            .as_array()
            .map(Vec::len)
            == Some(1),
        "search"
    );
    ensure!(
        app.get_expect("/api/routines?search=%25", 200).await?["routines"] == json!([]),
        "wildcard search"
    );
    let long = format!("search={}", "x".repeat(81));
    for query in [
        "after=bad",
        "agent=bad",
        "enabled=true",
        "trigger=schedule",
        "foo=x",
        "enabled=0&enabled=1",
        long.as_str(),
    ] {
        app.get_expect(&format!("/api/routines?{query}"), 400)
            .await?;
    }
    Ok(())
}

#[tokio::test]
async fn routine_hub_delete_is_revisioned_and_recoverable_retains_runs_revokes_webhook_and_prevents_resurrection(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|_| Reply::text("Done"));
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new().model(&model)).await?;
    let agent = id(&api_agent(&app, "Runner").await?)?;
    let input = json!({"id": new_id(), "name": "Webhook", "prompt": "Run", "trigger": "webhook",
        "nextAt": FIXED_CLOCK + 3_600_000, "intervalSeconds": null, "enabled": true});
    let routine = app
        .post(&format!("/api/agents/{agent}/routines"), input.clone())
        .await?;
    let routine_id = id(&routine)?;
    let hook = app
        .post(
            &format!("/api/routines/{routine_id}/webhook"),
            json!({"id": new_id(), "revision": 0, "rotate": true, "eventName": "", "filters": []}),
        )
        .await?;
    let run_request = json!({"id": new_id(), "revision": routine["revision"]});
    let run = app
        .post(
            &format!("/api/routines/{routine_id}/test"),
            run_request.clone(),
        )
        .await?;
    let request = json!({"id": new_id(), "revision": routine["revision"]});
    let delete = format!("/api/routines/{routine_id}/delete");
    app.post_expect(&delete, request.clone(), 409).await?;
    first_run(&app, &routine_id, "done").await?;
    let summary = app.get("/api/routines").await?["routines"][0].clone();
    ensure!(
        summary["last_thread"] == run["thread"],
        "summary thread: {summary}"
    );
    ensure!(
        summary["last_status"] == "done",
        "summary status: {summary}"
    );
    app.post_expect(&delete, with(&request, json!({"revision": 99})), 409)
        .await?;
    app.post_expect(&delete, with(&request, json!({"revision": 0})), 400)
        .await?;
    let deleted = app.post_expect(&delete, request.clone(), 200).await?;
    ensure!(deleted["deleted"] == true, "deleted: {deleted}");
    ensure!(
        app.post_expect(&delete, request.clone(), 200).await? == deleted,
        "delete replay"
    );
    ensure!(
        app.get("/api/routines").await?["routines"] == json!([]),
        "hub lists deleted"
    );
    ensure!(
        app.get(&format!("/api/agents/{agent}/routines")).await? == json!([]),
        "agent lists deleted"
    );
    ensure!(
        runs(&app, &routine_id).await?.as_array().map(Vec::len) == Some(1),
        "runs not retained"
    );
    ensure!(
        app.get(&format!(
            "/api/threads/{}/turns",
            run["thread"].as_str().unwrap_or_default()
        ))
        .await?[0]["reply"]
            == "Done",
        "run history"
    );
    app.post_expect(
        &format!("/api/routines/{routine_id}/test"),
        json!({"id": new_id(), "revision": routine["revision"]}),
        404,
    )
    .await?;
    app.post_expect(
        &format!("/api/routines/{routine_id}/edit"),
        with(
            &input,
            json!({"id": new_id(), "revision": routine["revision"]}),
        ),
        404,
    )
    .await?;
    app.get_expect(&format!("/api/routines/{routine_id}/flow"), 404)
        .await?;
    let url = hook["url"].as_str().ok_or("no webhook url")?;
    let status = app
        .client()?
        .post(url)
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await?
        .status();
    ensure!(status == 404, "revoked webhook: {status}");
    app.restart().await?;
    ensure!(
        app.post_expect(&delete, request, 200).await? == deleted,
        "delete receipt"
    );
    ensure!(
        app.post_expect(
            &format!("/api/routines/{routine_id}/test"),
            run_request,
            200
        )
        .await?
            == run,
        "run receipt"
    );
    ensure!(
        app.get("/api/routines").await?["routines"] == json!([]),
        "resurrected"
    );
    Ok(())
}
