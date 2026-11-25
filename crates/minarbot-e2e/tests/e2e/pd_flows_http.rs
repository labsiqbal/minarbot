//! Routine flows over HTTP (port of `app/flows-http.test.ts`).

use minarbot_testkit::{
    db::{Fixture, Inspector},
    model::{ModelRequest, Reply, ScriptedModel},
    prelude::*,
    wait::never,
    FIXED_CLOCK,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Default)]
struct Modes {
    hold_c: bool,
    fail_c: bool,
    root: &'static str,
    hold_d: bool,
}

fn modes(shared: &Arc<Mutex<Modes>>, change: impl FnOnce(&mut Modes)) {
    if let Ok(mut modes) = shared.lock() {
        change(&mut modes);
    }
}

/// The Node provider: every agent's model is its name; `<model> result`
/// unless a mode holds, fails or reshapes that agent's step.
fn provider(model: &ScriptedModel, shared: Arc<Mutex<Modes>>) {
    model.respond(move |request| {
        let Ok(modes) = shared.lock() else {
            return Reply::status(500, json!({"error": "poisoned"}));
        };
        let name = request.model();
        if acknowledgement(request) {
            return Reply::text("Outcome recorded.");
        }
        if modes.hold_d && name == "d" {
            return Reply::hold();
        }
        if modes.root == "quiet" && name == "a" {
            return Reply::tool("no_update", json!({"reason": "No changes."}));
        }
        if modes.root == "large" && name == "a" {
            return Reply::text("x".repeat(256 * 1024));
        }
        if modes.hold_c && name == "c" {
            return Reply::hold();
        }
        if modes.fail_c && name == "c" {
            return Reply::tool(
                "report_failure",
                json!({"reason": "C cannot complete its step."}),
            );
        }
        let extra = if name == "d" {
            "x".repeat(4000)
        } else {
            String::new()
        };
        Reply::text(format!("{name} result{extra}"))
    });
}

/// The harness step after a terminal outcome tool (`no_update`,
/// `report_failure`): the model receives the tool result once more before
/// the server stops the turn. The Node API runtime ended the turn inside its
/// own provider loop instead, so step counts leave this step out.
fn acknowledgement(request: &ModelRequest) -> bool {
    request
        .messages()
        .last()
        .is_some_and(|message| message["role"] == "tool")
        && (request.called("no_update") || request.called("report_failure"))
}

/// Model steps that are agent work (every step except acknowledgements).
fn work_calls(model: &ScriptedModel) -> usize {
    model
        .requests()
        .iter()
        .filter(|request| !acknowledgement(request))
        .count()
}

fn calls_for(model: &ScriptedModel, name: &str) -> Vec<String> {
    model
        .requests()
        .iter()
        .filter(|request| request.model() == name)
        .map(|request| request.last_text())
        .collect()
}

fn id_of(value: &Value) -> Result<String> {
    Ok(value["id"].as_str().ok_or("missing id")?.to_owned())
}

fn steps_of(run: &Value) -> Vec<Value> {
    run["steps"].as_array().cloned().unwrap_or_default()
}

fn step_for(steps: &[Value], agent: &str) -> Result<Value> {
    steps
        .iter()
        .find(|step| step["agent"] == agent)
        .cloned()
        .ok_or_else(|| format!("no step for {agent}: {steps:?}").into())
}

fn settled(steps: &[Value]) -> bool {
    !steps.is_empty()
        && steps.iter().all(|step| {
            ["done", "failed", "skipped"].contains(&step["status"].as_str().unwrap_or(""))
        })
}

fn run_of(runs: &Value, root_turn: &str) -> Option<Value> {
    runs.as_array()?
        .iter()
        .find(|run| run["root_turn"] == root_turn)
        .cloned()
}

async fn wait_steps(
    app: &Server,
    runs: &str,
    root_turn: &str,
    done: impl Fn(&[Value]) -> bool,
) -> Result<Vec<Value>> {
    let runs_value = app
        .wait_get(runs, |data| {
            run_of(data, root_turn).is_some_and(|run| done(&steps_of(&run)))
        })
        .await?;
    Ok(run_of(&runs_value, root_turn)
        .map(|run| steps_of(&run))
        .unwrap_or_default())
}

async fn first_turn(app: &Server, thread: &Value) -> Result<Value> {
    let thread = thread.as_str().ok_or("step thread")?;
    let turns = app.get(&format!("/api/threads/{thread}/turns")).await?;
    Ok(turns[0].clone())
}

fn routine_body(name: &str, prompt: &str, next_at: i64, enabled: bool) -> Value {
    json!({"id": new_id(), "name": name, "prompt": prompt, "nextAt": next_at,
        "intervalSeconds": null, "enabled": enabled})
}

#[tokio::test]
async fn routine_flow_dispatch_joins_once_snapshots_edits_and_recovers_interrupted_steps(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    let shared = Arc::new(Mutex::new(Modes {
        root: "normal",
        ..Modes::default()
    }));
    provider(&model, shared.clone());
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new().model(&model)).await?;
    let mut agents = Vec::new();
    for name in ["a", "b", "c", "d"] {
        let agent = app
            .post_expect(
                "/api/agents",
                json!({"runtime": "api", "id": new_id(), "name": name, "model": name,
                    "instructions": format!("{name} instruction")}),
                201,
            )
            .await?;
        agents.push(id_of(&agent)?);
    }
    let [a, b, c, d] = [&agents[0], &agents[1], &agents[2], &agents[3]];
    let routine = app
        .post_expect(
            &format!("/api/agents/{a}/routines"),
            routine_body("Fan-in", "Root job", FIXED_CLOCK + 3_600_000, false),
            200,
        )
        .await?;
    let routine_id = id_of(&routine)?;
    let mut routine_revision = routine["revision"].clone();
    let settings = format!("/api/routines/{routine_id}/flow");
    let runs = format!("/api/routines/{routine_id}/flow-runs");
    let routine_runs = format!("/api/routines/{routine_id}/runs");
    let test = format!("/api/routines/{routine_id}/test");
    let edit = format!("/api/routines/{routine_id}/edit");
    ensure!(
        app.get(&settings).await?["revision"] == 0,
        "fresh flow revision"
    );
    let links = json!([{"from": a, "to": b}, {"from": a, "to": c}, {"from": b, "to": d},
        {"from": c, "to": d}]);
    let command = json!({"id": new_id(), "revision": 0, "links": links});
    let saved = app.post_expect(&settings, command.clone(), 200).await?;
    ensure!(
        app.post_expect(&settings, command.clone(), 200).await? == saved,
        "flow save replay differs"
    );
    let mut changed = command.clone();
    changed["links"] = json!([]);
    app.post_expect(&settings, changed, 409).await?;
    app.post_expect(
        &settings,
        json!({"id": new_id(), "revision": 1, "links": [{"from": a, "to": "missing"}]}),
        400,
    )
    .await?;
    let mut cycle = links.as_array().cloned().unwrap_or_default();
    cycle.push(json!({"from": d, "to": b}));
    app.post_expect(
        &settings,
        json!({"id": new_id(), "revision": 1, "links": cycle}),
        400,
    )
    .await?;

    // A failed handoff keeps downstream steps waiting and creates no turns.
    modes(&shared, |modes| modes.fail_c = true);
    let run_request = json!({"id": new_id(), "revision": routine_revision});
    let fixture = Fixture::open(app.database())?;
    fixture.execute_batch(
        "CREATE TRIGGER fail_flow_handoff BEFORE UPDATE ON flow_steps WHEN OLD.status='waiting' AND NEW.status='queued' BEGIN SELECT RAISE(ABORT,'fixture handoff failure'); END",
    )?;
    let blocked = app.post_expect(&test, run_request.clone(), 200).await?;
    let blocked_turn = blocked["turn"].as_str().ok_or("blocked turn")?.to_owned();
    app.wait_get(&routine_runs, |data| data[0]["status"] == "done")
        .await?;
    let blocked_steps = run_of(&app.get(&runs).await?, &blocked_turn)
        .map(|run| steps_of(&run))
        .ok_or("blocked flow run")?;
    ensure!(
        blocked_steps
            .iter()
            .filter(|step| step["agent"] != a.as_str())
            .all(|step| step["status"] == "waiting"),
        "steps moved past a failed handoff: {blocked_steps:?}"
    );
    let inspector = Inspector::open(app.database())?;
    ensure!(
        inspector.count("turns")? == 1,
        "turns: {}",
        inspector.count("turns")?
    );
    fixture.execute_batch("DROP TRIGGER fail_flow_handoff")?;
    drop(fixture);

    // The flow then runs once: d merges b's result and c's failure reason.
    let completed = app
        .wait_get(&runs, |data| settled(&steps_of(&data[0])))
        .await?;
    ensure!(
        completed.as_array().map(Vec::len) == Some(1),
        "flow runs: {completed}"
    );
    let merge = calls_for(&model, "d");
    ensure!(merge.len() == 1, "d calls: {}", merge.len());
    ensure!(merge[0].contains("b result"), "merge prompt: {}", merge[0]);
    ensure!(
        merge[0].contains("C cannot complete its step."),
        "merge prompt: {}",
        merge[0]
    );
    let completed_steps = steps_of(&completed[0]);
    let c_step = step_for(&completed_steps, c)?;
    ensure!(c_step["status"] == "failed", "c step: {c_step}");
    let d_step = step_for(&completed_steps, d)?;
    ensure!(
        d_step["result"].as_str().map(|text| text.chars().count()) == Some(1000),
        "d result: {}",
        d_step["result"]
    );
    ensure!(
        d_step["result_length"]
            .as_i64()
            .is_some_and(|length| length > 4000),
        "d result_length: {}",
        d_step["result_length"]
    );
    ensure!(
        first_turn(&app, &d_step["thread"]).await?["reply"]
            .as_str()
            .is_some_and(|reply| reply.len() > 4000),
        "d reply truncated"
    );
    let c_turn = first_turn(&app, &c_step["thread"]).await?;
    let c_thread = c_step["thread"].as_str().ok_or("c thread")?;
    app.post_expect(
        &format!(
            "/api/threads/{c_thread}/turns/{}/resume",
            c_turn["id"].as_str().ok_or("c turn")?
        ),
        json!({"id": new_id(), "revision": c_turn["revision"], "acknowledgeUncertain": true}),
        409,
    )
    .await?;
    for agent in &agents {
        let turns = app.get(&format!("/api/agents/{agent}/turns")).await?;
        ensure!(
            turns.as_array().is_some_and(Vec::is_empty),
            "main turns of {agent}: {turns}"
        );
    }
    app.restart().await?;
    app.post_expect(&test, run_request, 200).await?;
    ensure!(
        app.get(&runs).await?.as_array().map(Vec::len) == Some(1),
        "test replay started another flow"
    );
    ensure!(work_calls(&model) == 4, "calls: {}", work_calls(&model));

    // A running downstream step blocks manual and scheduled runs.
    modes(&shared, |modes| {
        modes.fail_c = false;
        modes.hold_c = true;
    });
    let next = app
        .post_expect(
            &test,
            json!({"id": new_id(), "revision": routine_revision}),
            200,
        )
        .await?;
    let next_turn = next["turn"].as_str().ok_or("next turn")?.to_owned();
    wait_steps(&app, &runs, &next_turn, |steps| {
        step_for(steps, c).is_ok_and(|step| step["task_status"] == "running")
    })
    .await?;
    app.post_expect(
        &test,
        json!({"id": new_id(), "revision": routine_revision}),
        409,
    )
    .await?;
    let mut due_body = routine_body("Fan-in", "Root job", FIXED_CLOCK - 1, true);
    due_body["revision"] = routine_revision.clone();
    let due = app.post_expect(&edit, due_body, 200).await?;
    never(
        "a due schedule starts while its downstream step is active",
        Duration::from_millis(1200),
        || async { Ok(app.get(&routine_runs).await?.as_array().map(Vec::len) != Some(2)) },
    )
    .await?;
    ensure!(
        app.get(&routine_runs).await?.as_array().map(Vec::len) == Some(2),
        "A due schedule must wait for its active downstream step"
    );
    let mut disable = routine_body("Fan-in", "Root job", 0, false);
    disable["nextAt"] = due["next_at"].clone();
    disable["revision"] = due["revision"].clone();
    let disabled = app.post_expect(&edit, disable, 200).await?;
    routine_revision = disabled["revision"].clone();
    app.post_expect(
        &settings,
        json!({"id": new_id(), "revision": 1, "links": []}),
        200,
    )
    .await?;

    // A restart interrupts the held step; resuming finishes the snapshot flow.
    app.shutdown().await?;
    modes(&shared, |modes| modes.hold_c = false);
    let config = app.config().clone();
    app.start_stopped(config).await?;
    let interrupted = run_of(&app.get(&runs).await?, &next_turn).ok_or("interrupted run")?;
    let interrupted_steps = steps_of(&interrupted);
    let current_c = step_for(&interrupted_steps, c)?;
    ensure!(
        current_c["task_status"] == "interrupted",
        "c step: {current_c}"
    );
    ensure!(current_c["status"] == "queued", "c step: {current_c}");
    ensure!(
        step_for(&interrupted_steps, d)?["status"] == "waiting",
        "d step: {interrupted}"
    );
    let current_turn = first_turn(&app, &current_c["thread"]).await?;
    app.post_expect(
        &format!(
            "/api/threads/{}/turns/{}/resume",
            current_c["thread"].as_str().ok_or("c thread")?,
            current_turn["id"].as_str().ok_or("c turn")?
        ),
        json!({"id": new_id(), "revision": current_turn["revision"],
            "acknowledgeUncertain": true}),
        200,
    )
    .await?;
    wait_steps(&app, &runs, &next_turn, |steps| {
        steps.iter().all(|step| step["status"] == "done")
    })
    .await?;
    ensure!(
        calls_for(&model, "d").len() == 2,
        "d calls: {}",
        calls_for(&model, "d").len()
    );

    // With the links removed, a run starts no flow.
    app.post_expect(
        &test,
        json!({"id": new_id(), "revision": routine_revision}),
        200,
    )
    .await?;
    app.wait_get(&routine_runs, |data| data[0]["status"] == "done")
        .await?;
    ensure!(
        app.get(&runs).await?.as_array().map(Vec::len) == Some(2),
        "flow runs: {}",
        app.get(&runs).await?
    );
    ensure!(
        calls_for(&model, "d").len() == 2,
        "d calls: {}",
        calls_for(&model, "d").len()
    );
    app.post_expect(
        &settings,
        json!({"id": new_id(), "revision": 2, "links": links}),
        200,
    )
    .await?;
    let start_run = || async {
        let run = app
            .post_expect(
                &test,
                json!({"id": new_id(), "revision": routine_revision}),
                200,
            )
            .await?;
        Ok::<String, minarbot_testkit::Error>(run["turn"].as_str().ok_or("run turn")?.to_owned())
    };

    // Cancelling a downstream turn fails its step.
    modes(&shared, |modes| modes.hold_d = true);
    let cancelled = start_run().await?;
    let downstream = wait_steps(&app, &runs, &cancelled, |steps| {
        steps
            .iter()
            .any(|step| step["agent"] == d.as_str() && step["task_status"] == "running")
    })
    .await?;
    let last = step_for(&downstream, d)?;
    ensure!(last["thread"].is_string(), "d step thread: {last}");
    let last_turn = first_turn(&app, &last["thread"]).await?;
    app.post_expect(
        &format!(
            "/api/threads/{}/turns/{}/cancel",
            last["thread"].as_str().ok_or("d thread")?,
            last_turn["id"].as_str().ok_or("d turn")?
        ),
        json!({"id": new_id()}),
        200,
    )
    .await?;
    let cancelled_steps = wait_steps(&app, &runs, &cancelled, settled).await?;
    ensure!(
        step_for(&cancelled_steps, d)?["status"] == "failed",
        "d step: {cancelled_steps:?}"
    );

    // no_update skips every downstream step; an oversized root result fails
    // the direct children and skips the merge, without starting them.
    let count = work_calls(&model);
    modes(&shared, |modes| modes.root = "quiet");
    let quiet = start_run().await?;
    let quiet_steps = wait_steps(&app, &runs, &quiet, settled).await?;
    ensure!(
        quiet_steps.iter().all(|step| step["status"] == "skipped"),
        "quiet steps: {quiet_steps:?}"
    );
    modes(&shared, |modes| modes.root = "large");
    let large = start_run().await?;
    let large_steps = wait_steps(&app, &runs, &large, settled).await?;
    let statuses: Vec<(String, Value)> = large_steps
        .iter()
        .map(|step| {
            (
                step["agent"].as_str().unwrap_or("").to_owned(),
                step["status"].clone(),
            )
        })
        .collect();
    let expected = vec![
        (a.clone(), json!("done")),
        (b.clone(), json!("failed")),
        (c.clone(), json!("failed")),
        (d.clone(), json!("skipped")),
    ];
    let mut sorted = statuses.clone();
    sorted.sort_by(|left, right| left.0.cmp(&right.0));
    let mut expected_sorted = expected.clone();
    expected_sorted.sort_by(|left, right| left.0.cmp(&right.0));
    ensure!(sorted == expected_sorted, "large steps: {statuses:?}");
    ensure!(
        large_steps
            .iter()
            .filter(|step| step["agent"] != a.as_str())
            .all(|step| step["thread"].is_null()),
        "large steps started threads: {large_steps:?}"
    );
    ensure!(
        work_calls(&model) == count + 2,
        "calls: {} (before {count})",
        work_calls(&model)
    );

    // The flow directory pages through every configured flow.
    let mut configured = vec![routine_id.clone()];
    for index in 0..20 {
        let item = app
            .post_expect(
                &format!("/api/agents/{b}/routines"),
                routine_body(
                    &format!("Directory {index}"),
                    "Not executed",
                    FIXED_CLOCK + 3_600_000,
                    false,
                ),
                200,
            )
            .await?;
        let item_id = id_of(&item)?;
        app.post_expect(
            &format!("/api/routines/{item_id}/flow"),
            json!({"id": new_id(), "revision": 0, "links": [{"from": b, "to": c}]}),
            200,
        )
        .await?;
        configured.push(item_id);
    }
    let first = app.get("/api/flows").await?;
    let after = first["next_after"].as_str().ok_or("next_after")?;
    let second = app.get(&format!("/api/flows?after={after}")).await?;
    let first_rows = first["flows"].as_array().cloned().unwrap_or_default();
    let second_rows = second["flows"].as_array().cloned().unwrap_or_default();
    ensure!(first_rows.len() == 20, "first page: {}", first_rows.len());
    ensure!(second_rows.len() == 1, "second page: {}", second_rows.len());
    ensure!(
        second["next_after"].is_null(),
        "next_after: {}",
        second["next_after"]
    );
    configured.sort();
    let listed: Vec<String> = first_rows
        .iter()
        .chain(&second_rows)
        .map(|row| row["id"].as_str().unwrap_or("").to_owned())
        .collect();
    ensure!(listed == configured, "flows: {listed:?}");
    ensure!(
        first_rows.iter().all(|row| {
            row["owner_name"]
                .as_str()
                .is_some_and(|name| !name.is_empty())
                && row["graph"]["root"] == row["agent"]
                && row["flow_revision"]
                    .as_i64()
                    .is_some_and(|revision| revision >= 1)
        }),
        "flow rows: {first_rows:?}"
    );
    app.get_expect("/api/flows?after=invalid", 400).await?;
    app.get_expect("/api/flows?extra=1", 400).await?;
    Ok(())
}
