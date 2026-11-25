//! Parallel workflow blocks (port of `app/workflow-parallel.test.ts`).

use minarbot_testkit::{
    parked::{Answer, ParkedModel, Route},
    prelude::*,
    server::Gateway,
    wait::never,
};
use std::time::Duration;

fn id_of(value: &Value) -> Result<String> {
    Ok(value["id"].as_str().ok_or("missing id")?.to_owned())
}

async fn get_run(app: &Server, run: &str) -> Result<Value> {
    app.get(&format!("/api/workflow-runs/{run}")).await
}

async fn wait_run(app: &Server, run: &str, done: impl Fn(&Value) -> bool) -> Result<Value> {
    app.wait_get(&format!("/api/workflow-runs/{run}"), done)
        .await
}

async fn wait_status(app: &Server, run: &str, status: &str) -> Result<Value> {
    wait_run(app, run, |row| row["status"] == status).await
}

fn steps(run: &Value) -> Vec<Value> {
    run["steps"].as_array().cloned().unwrap_or_default()
}

fn last(run: &Value) -> Result<Value> {
    Ok(serde_json::from_str(
        run["last"].as_str().ok_or("run.last")?,
    )?)
}

fn results(first: &str, second: &str) -> Value {
    json!([{"branch": 1, "result": first}, {"branch": 2, "result": second}])
}

/// The model branch (`model: "b"`) is a bare model call without tools.
fn no_tools_for_b(model: &ParkedModel) -> Result<()> {
    for request in model.requests() {
        if request.model() == "b" {
            ensure!(
                request.tool_names().is_empty(),
                "model branch was offered tools: {:?}",
                request.tool_names()
            );
        }
    }
    Ok(())
}

fn inputs(model: &ParkedModel) -> Vec<String> {
    model
        .requests()
        .iter()
        .map(|request| request.last_text())
        .collect()
}

async fn workflow(app: &Server, owner: &str, name: &str, graph: &Value) -> Result<String> {
    let saved = app
        .post_expect(
            &format!("/api/agents/{owner}/workflows"),
            json!({"id": new_id(), "revision": 0, "name": name, "graph": graph}),
            200,
        )
        .await?;
    id_of(&saved)
}

async fn start(app: &Server, workflow: &str, input: &str) -> Result<String> {
    let run = app
        .post_expect(
            &format!("/api/workflows/{workflow}/run"),
            json!({"id": new_id(), "revision": 1, "input": input}),
            200,
        )
        .await?;
    id_of(&run)
}

async fn task_of(app: &Server, step: &Value) -> Result<Value> {
    let thread = step["thread"].as_str().ok_or("step thread")?;
    let turn = step["turn"].as_str().ok_or("step turn")?;
    app.row(&format!("/api/threads/{thread}/turns"), turn)
        .await?
        .ok_or_else(|| format!("turn {turn} missing").into())
}

async fn resume(app: &Server, step: &Value, task: &Value) -> Result<()> {
    let thread = step["thread"].as_str().ok_or("step thread")?;
    let turn = step["turn"].as_str().ok_or("step turn")?;
    app.post_expect(
        &format!("/api/threads/{thread}/turns/{turn}/resume"),
        json!({"id": new_id(), "revision": task["revision"], "acknowledgeUncertain": true}),
        200,
    )
    .await?;
    Ok(())
}

fn step_done(run: &Value, agent: &str) -> bool {
    steps(run)
        .iter()
        .any(|step| step["agent"] == agent && step["status"] == "done")
}

#[tokio::test]
async fn parallel_workflow_joins_once_in_branch_order_retains_failures_and_cancels_all_branches(
) -> Result<()> {
    let model = ParkedModel::start(|request| Route::Park(request.model().to_owned())).await?;
    let root = TestRoot::new()?;
    let config = ServerConfig::new()
        .gateway(Gateway::new(model.base_url()))
        .concurrency(4);
    let mut app = root.start(config).await?;
    let a = app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "A", "model": "a",
                "instructions": "Help.", "threadLimit": 1}),
            201,
        )
        .await?;
    let b = app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "B", "model": "b",
                "instructions": "Help."}),
            201,
        )
        .await?;
    let (a_id, b_id) = (id_of(&a)?, id_of(&b)?);
    let parallel = json!({"id": "parallel", "kind": "parallel", "branches": [
        {"kind": "agent", "agent": a_id, "prompt": "A"},
        {"kind": "model", "model": "b", "prompt": "B"}], "next": "out"});
    let out = json!({"id": "out", "kind": "output"});
    let graph = json!({"entry": "parallel", "maxSteps": 4, "nodes": [parallel, out]});
    let graph_with = |max: i64, nodes: Value| {
        let mut changed = graph.clone();
        changed["maxSteps"] = json!(max);
        changed["nodes"] = nodes;
        changed
    };

    // validateWorkflow rejects a parallel block without branches.
    let mut empty = parallel.clone();
    empty["branches"] = json!([]);
    app.post_expect(
        &format!("/api/agents/{b_id}/workflows"),
        json!({"id": new_id(), "revision": 0, "name": "Empty",
            "graph": graph_with(4, json!([empty, out]))}),
        400,
    )
    .await?;
    let main = workflow(&app, &b_id, "Parallel", &graph).await?;

    // Both branches start; B finishing first leaves the join waiting.
    let run = start(&app, &main, "Shared input").await?;
    model.wait_parked(&["a", "b"]).await?;
    ensure!(model.calls() == 2, "calls: {}", model.calls());
    model.finish("b", Answer::text("B result"))?;
    wait_run(&app, &run, |row| step_done(row, &b_id)).await?;
    let waiting = get_run(&app, &run).await?;
    ensure!(waiting["status"] == "waiting", "run: {waiting}");
    model.finish("a", Answer::text("A result"))?;
    let result = wait_status(&app, &run, "done").await?;
    ensure!(
        last(&result)? == results("A result", "B result"),
        "joined: {}",
        result["last"]
    );
    ensure!(steps(&result).len() == 4, "steps: {}", result["steps"]);
    app.restart().await?;
    ensure!(
        get_run(&app, &run).await?["last"] == result["last"],
        "restart changed the joined result"
    );
    ensure!(model.calls() == 2, "restart replayed: {}", model.calls());

    // A failed branch fails the run and keeps the other branch's result.
    let failed = start(&app, &main, "Shared input").await?;
    model.wait_parked(&["a", "b"]).await?;
    model.finish("a", Answer::fail("failure"))?;
    model.finish("b", Answer::text("Partial result"))?;
    let failure = wait_status(&app, &failed, "failed").await?;
    ensure!(step_done(&failure, &b_id), "partial result lost: {failure}");
    app.post_expect(
        &format!("/api/workflow-runs/{failed}/retry"),
        json!({"id": new_id(), "revision": failure["revision"]}),
        409,
    )
    .await?;

    // Cancel cancels every branch.
    let cancelling = start(&app, &main, "Shared input").await?;
    model.wait_parked(&["a", "b"]).await?;
    let current = get_run(&app, &cancelling).await?;
    app.post_expect(
        &format!("/api/workflow-runs/{cancelling}/cancel"),
        json!({"id": new_id(), "revision": current["revision"]}),
        200,
    )
    .await?;
    let cancelled = get_run(&app, &cancelling).await?;
    ensure!(cancelled["status"] == "cancelled", "run: {cancelled}");
    ensure!(
        steps(&cancelled)
            .iter()
            .all(|step| step["status"] == "cancelled"),
        "steps: {}",
        cancelled["steps"]
    );
    ensure!(model.calls() == 6, "calls: {}", model.calls());

    // A restart interrupts the open branch; resuming it runs only that branch.
    model.clear();
    let interrupted = start(&app, &main, "Shared input").await?;
    model.wait_parked(&["a", "b"]).await?;
    model.finish("b", Answer::text("Saved B"))?;
    wait_run(&app, &interrupted, |row| step_done(row, &b_id)).await?;
    app.shutdown().await?;
    model.clear();
    let config = app.config().clone();
    app.start_stopped(config).await?;
    let restored = get_run(&app, &interrupted).await?;
    ensure!(restored["status"] == "waiting", "run: {restored}");
    ensure!(model.calls() == 8, "calls: {}", model.calls());
    let branch = steps(&restored)
        .into_iter()
        .find(|step| step["agent"] == a_id.as_str())
        .ok_or("branch A step")?;
    let task = task_of(&app, &branch).await?;
    ensure!(task["status"] == "interrupted", "task: {task}");
    resume(&app, &branch, &task).await?;
    model.wait_parked(&["a"]).await?;
    ensure!(!model.parked("b"), "resume replayed branch B");
    model.finish("a", Answer::text("Resumed A"))?;
    let resumed = wait_status(&app, &interrupted, "done").await?;
    ensure!(
        last(&resumed)? == results("Resumed A", "Saved B"),
        "resumed: {}",
        resumed["last"]
    );
    ensure!(model.calls() == 9, "calls: {}", model.calls());

    // Too few steps for the branches and the merge fails before any work.
    let short = workflow(
        &app,
        &b_id,
        "Bounded",
        &graph_with(2, graph["nodes"].clone()),
    )
    .await?;
    let bounded = start(&app, &short, "Bounded").await?;
    let bounded_run = wait_status(&app, &bounded, "failed").await?;
    ensure!(
        steps(&bounded_run).is_empty(),
        "steps: {}",
        bounded_run["steps"]
    );
    ensure!(model.calls() == 9, "calls: {}", model.calls());

    // Two branches on the same single-thread agent run one after the other.
    let mut serial_block = parallel.clone();
    serial_block["branches"] = json!([
        {"kind": "agent", "agent": a_id, "prompt": "First"},
        {"kind": "agent", "agent": a_id, "prompt": "Second"}]);
    let same = workflow(
        &app,
        &b_id,
        "Same agent",
        &graph_with(4, json!([serial_block, out])),
    )
    .await?;
    let serial = start(&app, &same, "Shared").await?;
    model.wait_parked(&["a"]).await?;
    ensure!(model.calls() == 10, "calls: {}", model.calls());
    let queued = get_run(&app, &serial).await?;
    let task_status = |status: &str| {
        steps(&queued)
            .iter()
            .filter(|step| step["task_status"] == status)
            .count()
    };
    ensure!(task_status("running") == 1, "steps: {}", queued["steps"]);
    ensure!(task_status("queued") == 1, "steps: {}", queued["steps"]);
    model.finish("a", Answer::text("First result"))?;
    model.wait_parked(&["a"]).await?;
    ensure!(model.calls() == 11, "calls: {}", model.calls());
    model.finish("a", Answer::text("Second result"))?;
    let serial_done = wait_status(&app, &serial, "done").await?;
    ensure!(
        last(&serial_done)? == results("First result", "Second result"),
        "serial: {}",
        serial_done["last"]
    );

    // A second block receives the first block's joined result.
    let mut chained_first = parallel.clone();
    chained_first["next"] = json!("second");
    let mut chained_second = parallel.clone();
    chained_second["id"] = json!("second");
    let chain = workflow(
        &app,
        &b_id,
        "Chained blocks",
        &graph_with(7, json!([chained_first, chained_second, out])),
    )
    .await?;
    let chained = start(&app, &chain, "Root input").await?;
    model.wait_parked(&["a", "b"]).await?;
    model.finish("a", Answer::text("First A"))?;
    model.finish("b", Answer::text("First B"))?;
    model.wait_parked(&["a", "b"]).await?;
    let previous = results("First A", "First B").to_string();
    let mut seen = inputs(&model).split_off(model.calls() - 2);
    seen.sort();
    let expected: Vec<String> = ["A", "B"]
        .iter()
        .map(|instruction| {
            format!(
                "{instruction}\n\nWorkflow input:\nRoot input\n\nPrevious step result:\n{previous}"
            )
        })
        .collect();
    ensure!(seen == expected, "chained inputs: {seen:?}");
    model.finish("b", Answer::text("Second B"))?;
    model.finish("a", Answer::text("Second A"))?;
    let chain_result = wait_status(&app, &chained, "done").await?;
    ensure!(
        steps(&chain_result).len() == 7,
        "steps: {}",
        chain_result["steps"]
    );
    ensure!(
        last(&chain_result)? == results("Second A", "Second B"),
        "chained: {}",
        chain_result["last"]
    );
    ensure!(model.calls() == 15, "calls: {}", model.calls());

    // An oversized join fails without running the output node, also after restart.
    let oversized = start(&app, &main, "Shared input").await?;
    model.wait_parked(&["a", "b"]).await?;
    model.finish("a", Answer::text("a".repeat(140 * 1024)))?;
    model.finish("b", Answer::text("b".repeat(140 * 1024)))?;
    let too_large = wait_status(&app, &oversized, "failed").await?;
    ensure!(
        too_large["error"]
            .as_str()
            .is_some_and(|error| error.contains("Combined result exceeds")),
        "error: {}",
        too_large["error"]
    );
    ensure!(
        too_large["last"] == "Shared input",
        "last: {}",
        too_large["last"]
    );
    let large_steps = steps(&too_large);
    ensure!(large_steps.len() == 3, "steps: {}", too_large["steps"]);
    ensure!(
        large_steps
            .iter()
            .filter(|step| step["status"] == "done")
            .count()
            == 2,
        "steps: {}",
        too_large["steps"]
    );
    ensure!(
        large_steps
            .iter()
            .find(|step| step["parallel_group"].is_null())
            .is_some_and(|step| step["status"] == "failed"),
        "merge step: {}",
        too_large["steps"]
    );
    ensure!(
        !large_steps.iter().any(|step| step["node"] == "out"),
        "output ran: {}",
        too_large["steps"]
    );
    ensure!(model.calls() == 17, "calls: {}", model.calls());
    app.restart().await?;
    never(
        "oversized run changes after restart",
        Duration::from_millis(1200),
        || async {
            Ok(get_run(&app, &oversized).await?["status"] != "failed" || model.calls() != 17)
        },
    )
    .await?;
    ensure!(
        get_run(&app, &oversized).await?["status"] == "failed",
        "oversized run revived"
    );
    ensure!(model.calls() == 17, "calls: {}", model.calls());

    // Retry reruns only the failed branch from its snapshot, across the
    // old-schema backfill, a workflow edit and an agent profile edit.
    let retryable = workflow(
        &app,
        &b_id,
        "Selective retry",
        &graph_with(12, graph["nodes"].clone()),
    )
    .await?;
    let retry_run = start(&app, &retryable, "Keep snapshot").await?;
    model.wait_parked(&["a", "b"]).await?;
    model.finish("a", Answer::fail("failure"))?;
    model.finish("b", Answer::text("Reuse B"))?;
    let original = wait_status(&app, &retry_run, "failed").await?;
    let original_prompt =
        "A\n\nWorkflow input:\nKeep snapshot\n\nPrevious step result:\nKeep snapshot";
    let mut changed = parallel.clone();
    changed["branches"] = json!([
        {"kind": "model", "model": "changed", "prompt": "Changed A"},
        {"kind": "model", "model": "changed", "prompt": "Changed B"}]);
    app.post_expect(
        &format!("/api/workflows/{retryable}/edit"),
        json!({"id": new_id(), "revision": 1, "name": "Changed",
            "graph": graph_with(12, json!([changed, out]))}),
        200,
    )
    .await?;
    app.edit_database(|db| {
        db.execute_batch(
            "DROP TABLE workflow_parallel_members; UPDATE workflow_steps SET parallel_branch=NULL",
        )
    })
    .await?;
    app.restart().await?;
    let legacy = get_run(&app, &retry_run).await?;
    app.post_expect(
        &format!("/api/agents/{a_id}/edit"),
        json!({"id": new_id(), "revision": 1, "name": "A updated", "model": "a-new",
            "instructions": "Updated profile.", "threadLimit": 1}),
        200,
    )
    .await?;
    let retry_body = json!({"id": new_id(), "revision": original["revision"]});
    let retry = format!("/api/workflow-runs/{retry_run}/retry");
    app.post_expect(&retry, retry_body.clone(), 200).await?;
    app.post_expect(&retry, retry_body, 200).await?;
    model.wait_parked(&["a-new"]).await?;
    ensure!(!model.parked("b"), "retry reran branch B");
    ensure!(model.calls() == 20, "calls: {}", model.calls());
    let requests = model.requests();
    let newest = requests.last().ok_or("no request")?;
    ensure!(
        newest.last_text() == original_prompt,
        "retry prompt: {}",
        newest.last_text()
    );
    ensure!(
        newest.system().contains("\n\nUpdated profile."),
        "retry system: {}",
        newest.system()
    );
    model.finish("a-new", Answer::fail("failure again"))?;
    let second = wait_status(&app, &retry_run, "failed").await?;
    app.post_expect(
        &retry,
        json!({"id": new_id(), "revision": second["revision"]}),
        200,
    )
    .await?;
    model.wait_parked(&["a-new"]).await?;
    ensure!(!model.parked("b"), "second retry reran branch B");
    ensure!(model.calls() == 21, "calls: {}", model.calls());
    app.shutdown().await?;
    model.clear();
    let config = app.config().clone();
    app.start_stopped(config).await?;
    let recovery = get_run(&app, &retry_run).await?;
    let active = steps(&recovery)
        .into_iter()
        .rev()
        .find(|step| step["turn"].is_string())
        .ok_or("active step")?;
    let active_task = task_of(&app, &active).await?;
    resume(&app, &active, &active_task).await?;
    model.wait_parked(&["a-new"]).await?;
    ensure!(!model.parked("b"), "recovery reran branch B");
    model.finish("a-new", Answer::text("Recovered A"))?;
    let recovered = wait_status(&app, &retry_run, "done").await?;
    ensure!(
        last(&recovered)? == results("Recovered A", "Reuse B"),
        "recovered: {}",
        recovered["last"]
    );
    ensure!(
        steps(&recovered).get(..3) == Some(&steps(&legacy)[..]),
        "legacy steps changed: {} vs {}",
        recovered["steps"],
        legacy["steps"]
    );
    ensure!(model.calls() == 22, "calls: {}", model.calls());
    ensure!(
        steps(&recovered)
            .iter()
            .filter(|step| step["agent"] == b_id.as_str())
            .count()
            == 1,
        "steps: {}",
        recovered["steps"]
    );

    // Retrying both failed branches, then cancelling, keeps the failed history.
    let both_workflow = workflow(
        &app,
        &b_id,
        "Both retry",
        &graph_with(12, graph["nodes"].clone()),
    )
    .await?;
    let both = start(&app, &both_workflow, "Both").await?;
    model.wait_parked(&["a-new", "b"]).await?;
    model.finish("a-new", Answer::fail("fail"))?;
    model.finish("b", Answer::fail("fail"))?;
    let both_failed = wait_status(&app, &both, "failed").await?;
    let both_command = json!({"id": new_id(), "revision": both_failed["revision"]});
    let both_retry = format!("/api/workflow-runs/{both}/retry");
    app.post_expect(&both_retry, both_command.clone(), 200)
        .await?;
    app.post_expect(&both_retry, both_command, 200).await?;
    model.wait_parked(&["a-new", "b"]).await?;
    ensure!(model.calls() == 26, "calls: {}", model.calls());
    let both_active = get_run(&app, &both).await?;
    app.post_expect(
        &format!("/api/workflow-runs/{both}/cancel"),
        json!({"id": new_id(), "revision": both_active["revision"]}),
        200,
    )
    .await?;
    let both_cancelled = get_run(&app, &both).await?;
    ensure!(
        both_cancelled["status"] == "cancelled",
        "run: {both_cancelled}"
    );
    ensure!(
        steps(&both_cancelled).get(..3) == Some(&steps(&both_failed)[..]),
        "failed steps changed: {} vs {}",
        both_cancelled["steps"],
        both_failed["steps"]
    );
    ensure!(
        steps(&both_cancelled)
            .iter()
            .skip(3)
            .all(|step| step["status"] == "cancelled"),
        "steps: {}",
        both_cancelled["steps"]
    );
    model.clear();

    // An oversized join cannot be retried and the run stays unchanged.
    let big = start(&app, &both_workflow, "Big").await?;
    model.wait_parked(&["a-new", "b"]).await?;
    model.finish("a-new", Answer::text("a".repeat(140 * 1024)))?;
    model.finish("b", Answer::text("b".repeat(140 * 1024)))?;
    let big_failed = wait_status(&app, &big, "failed").await?;
    app.post_expect(
        &format!("/api/workflow-runs/{big}/retry"),
        json!({"id": new_id(), "revision": big_failed["revision"]}),
        409,
    )
    .await?;
    ensure!(
        get_run(&app, &big).await? == big_failed,
        "refused retry changed the run"
    );
    ensure!(model.calls() == 28, "calls: {}", model.calls());
    no_tools_for_b(&model)?;
    Ok(())
}
