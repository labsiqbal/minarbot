//! Workflow HTTP: versions, model and approval steps, webhooks, schedules,
//! directory, deletion, inbox, channel outputs and run pages (port of
//! `app/workflow-http.test.ts`).

use crate::sa_support::{api_agent, channel, id, lock, Shared};
use minarbot_testkit::{
    db::Fixture,
    hmac,
    model::{Reply, ScriptedModel},
    prelude::*,
    Server, FIXED_CLOCK,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn with(base: &Value, fields: Value) -> Value {
    let mut value = base.clone();
    if let (Some(value), Some(fields)) = (value.as_object_mut(), fields.as_object()) {
        value.extend(fields.clone());
    }
    value
}

fn len(value: &Value) -> usize {
    value.as_array().map_or(0, Vec::len)
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

async fn run_until(app: &Server, run: &str, done: impl Fn(&Value) -> bool) -> Result<Value> {
    app.wait_get(&format!("/api/workflow-runs/{run}"), done)
        .await
}

async fn settled(app: &Server, run: &str) -> Result<Value> {
    run_until(app, run, |run| {
        run["status"] == "done" || run["status"] == "failed"
    })
    .await
}

fn output_graph() -> Value {
    json!({"entry": "out", "maxSteps": 1, "nodes": [{"id": "out", "kind": "output"}]})
}

#[tokio::test]
async fn legacy_unsupported_workflow_steps_fail_once_without_producing_output() -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|_| Reply::status(500, json!({})));
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new().model(&model)).await?;
    let agent = id(&api_agent(&app, "Legacy").await?)?;
    let list = format!("/api/agents/{agent}/workflows");
    for kind in ["image", "asset", "file-output", "message-trigger"] {
        let workflow = new_id();
        let graph = json!({"entry": "legacy", "maxSteps": 2, "nodes": [
            {"id": "legacy", "kind": kind, "next": "out"}, {"id": "out", "kind": "output"}]});
        ensure!(
            app.post_expect(
                &list,
                json!({"id": workflow, "name": kind, "revision": 0, "graph": graph}),
                400
            )
            .await?
                == json!({"error": "Unsupported workflow step type."}),
            "{kind} accepted"
        );
        {
            // A saved version predating the current validation.
            let db = Fixture::open(app.database())?;
            db.execute(
                "INSERT INTO workflows(id,agent,name,revision) VALUES(?,?,?,1)",
                [&workflow, &agent, &kind.to_owned()],
            )?;
            db.execute(
                "INSERT INTO workflow_versions(workflow,revision,graph) VALUES(?,1,?)",
                [&workflow, &graph.to_string()],
            )?;
        }
        let path = format!("/api/workflows/{workflow}");
        ensure!(app.get(&path).await?["graph"] == graph, "{kind} graph");
        let launch = json!({"id": new_id(), "revision": 1, "input": "Preserve this input"});
        app.post(&format!("{path}/run"), launch.clone()).await?;
        let run = app
            .get(&format!("/api/workflow-runs/{}", id(&launch)?))
            .await?;
        let error = format!("Unsupported workflow step type: {kind}.");
        ensure!(run["status"] == "failed", "{kind} run: {run}");
        let steps: Vec<Value> = run["steps"]
            .as_array()
            .map(|steps| {
                steps
                    .iter()
                    .map(|step| json!([step["node"], step["status"], step["detail"]]))
                    .collect()
            })
            .unwrap_or_default();
        ensure!(
            steps == [json!(["legacy", "failed", error])],
            "{kind} steps: {run}"
        );
        ensure!(run["error"] == error.as_str(), "{kind} error: {run}");
        ensure!(run["last"] == "Preserve this input", "{kind} last: {run}");
    }
    ensure!(model.calls() == 0, "model called {} times", model.calls());
    ensure!(
        app.get("/api/workflow-notifications").await?["notifications"] == json!([]),
        "failed legacy runs notified"
    );
    Ok(())
}

#[tokio::test]
async fn model_conditions_choose_one_branch_without_tools_and_reject_ambiguous_answers(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    let answer: Shared<String> = std::sync::Arc::new(std::sync::Mutex::new("yes".into()));
    let current = answer.clone();
    model.respond(move |_| Reply::text(current.lock().map(|a| a.clone()).unwrap_or_default()));
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new().model(&model)).await?;
    let agent = app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Condition", "model": "owner",
                "instructions": "Evaluate."}),
            201,
        )
        .await?;
    let agent = id(&agent)?;
    let graph = json!({"entry": "choose", "maxSteps": 4, "nodes": [
        {"id": "choose", "kind": "condition", "operator": "model", "model": "decision-model",
            "value": "Is the input acceptable?", "yes": "accepted", "no": "declined"},
        {"id": "accepted", "kind": "output"}, {"id": "declined", "kind": "output"}]});
    let workflow = app
        .post(
            &format!("/api/agents/{agent}/workflows"),
            json!({"id": new_id(), "revision": 0, "name": "Condition", "graph": graph}),
        )
        .await?;
    let workflow = id(&workflow)?;
    let decisions = [
        (" yes\n", Some("accepted")),
        ("NO", Some("declined")),
        ("maybe", None),
        (" YES\n", Some("accepted")),
        ("no", Some("declined")),
        ("yes because it matches", None),
    ];
    let mut failed_run = Value::Null;
    for (value, expected) in decisions {
        *lock(&answer)? = value.into();
        let run = app
            .post(
                &format!("/api/workflows/{workflow}/run"),
                json!({"id": new_id(), "revision": 1, "input": "ORIGINAL_DATA"}),
            )
            .await?;
        let current = settled(&app, &id(&run)?).await?;
        ensure!(
            current["last"] == "ORIGINAL_DATA",
            "{value:?} last: {current}"
        );
        ensure!(
            current["status"] == if expected.is_some() { "done" } else { "failed" },
            "{value:?} status: {current}"
        );
        ensure!(
            len(&current["steps"]) == if expected.is_some() { 2 } else { 1 },
            "{value:?} steps: {current}"
        );
        ensure!(
            current["steps"][0]["status"] == if expected.is_some() { "done" } else { "failed" },
            "{value:?} first step: {current}"
        );
        match expected {
            Some(branch) => ensure!(current["cursor"] == branch, "{value:?} cursor: {current}"),
            None => {
                ensure!(
                    current["error"] == "Model condition must answer yes or no.",
                    "{value:?} error: {current}"
                );
                failed_run = current;
            }
        }
    }
    let requests = model.requests();
    ensure!(
        requests.len() == decisions.len(),
        "model calls: {}",
        requests.len()
    );
    for request in &requests {
        ensure!(
            request.model() == "decision-model",
            "model: {}",
            request.model()
        );
        ensure!(
            request.tool_names().is_empty(),
            "condition tools: {:?}",
            request.tool_names()
        );
    }
    let mut edited = graph.clone();
    edited["nodes"][0] = with(
        &graph["nodes"][0],
        json!({"model": "fixed-model", "value": "Fixed question", "yes": "declined", "no": "accepted"}),
    );
    app.post(
        &format!("/api/workflows/{workflow}/edit"),
        json!({"id": new_id(), "revision": 1, "name": "Fixed", "graph": edited}),
    )
    .await?;
    *lock(&answer)? = "yes".into();
    let retry = json!({"id": new_id(), "revision": failed_run["revision"]});
    let failed_id = id(&failed_run)?;
    let retry_path = format!("/api/workflow-runs/{failed_id}/retry");
    app.post(&retry_path, retry.clone()).await?;
    app.post(&retry_path, retry.clone()).await?;
    let recovered = run_until(&app, &failed_id, |run| run["status"] == "done").await?;
    ensure!(recovered["version"] == 1, "version: {recovered}");
    ensure!(recovered["graph"] == graph, "graph: {recovered}");
    ensure!(recovered["cursor"] == "accepted", "cursor: {recovered}");
    ensure!(len(&recovered["steps"]) == 3, "steps: {recovered}");
    ensure!(
        recovered["steps"][0]["status"] == "failed",
        "first: {recovered}"
    );
    ensure!(recovered["last"] == "ORIGINAL_DATA", "last: {recovered}");
    let requests = model.requests();
    ensure!(
        requests.len() == decisions.len() + 1,
        "retry calls: {}",
        requests.len()
    );
    let retried = &requests[decisions.len()];
    ensure!(
        retried.model() == "fixed-model",
        "retry model: {}",
        retried.model()
    );
    ensure!(
        retried.tool_names().is_empty(),
        "retry tools: {:?}",
        retried.tool_names()
    );
    ensure!(
        retried.mentions("Fixed question"),
        "retry prompt: {:?}",
        retried.texts()
    );
    let settings: Value =
        serde_json::from_str(recovered["steps"][1]["settings"].as_str().unwrap_or("null"))?;
    ensure!(settings["model"] == "fixed-model", "settings: {settings}");
    app.restart().await?;
    app.post(&retry_path, retry.clone()).await?;
    ensure!(
        app.get(&format!("/api/workflow-runs/{failed_id}")).await?["steps"] == recovered["steps"],
        "retry replay changed steps"
    );
    ensure!(
        model.calls() == decisions.len() + 1,
        "retry replayed the model"
    );
    app.post_expect(&retry_path, with(&retry, json!({"id": new_id()})), 409)
        .await?;
    ensure!(
        app.get(&format!("/api/agents/{agent}/turns")).await? == json!([]),
        "condition entered main history"
    );
    Ok(())
}

#[derive(Default)]
struct Steps {
    hold: bool,
    force_tool: bool,
    answer: String,
}

#[tokio::test]
async fn workflow_http_versions_model_steps_approval_receipts_and_cancellation_survive_restart(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    let state: Shared<Steps> = std::sync::Arc::new(std::sync::Mutex::new(Steps {
        answer: "RESULT".into(),
        ..Steps::default()
    }));
    let script = state.clone();
    model.respond(move |_| {
        let Ok(state) = script.lock() else {
            return Reply::status(500, json!({}));
        };
        if state.hold {
            Reply::hold()
        } else if state.force_tool {
            Reply::tool_call(
                "forced-file",
                "create_text_file",
                json!({"name": "forbidden.txt", "text": "Must not be saved"}),
            )
        } else {
            Reply::text(state.answer.clone())
        }
    });
    let models = |model: &ScriptedModel| -> Vec<String> {
        model
            .requests()
            .iter()
            .map(|r| r.model().to_owned())
            .collect()
    };
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new().model(&model)).await?;
    let agent = app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Owner", "model": "owner-model",
                "instructions": "Help."}),
            201,
        )
        .await?;
    let agent = id(&agent)?;
    let graph = json!({"entry": "model", "maxSteps": 4, "positions": {"model": {"x": 501, "y": 702}},
        "nodes": [
            {"id": "model", "kind": "model", "model": "override-model", "prompt": "Process", "next": "approve"},
            {"id": "approve", "kind": "approval", "prompt": "Accept?", "yes": "out", "no": "model"},
            {"id": "out", "kind": "output"}]});
    let create = json!({"id": new_id(), "revision": 0, "name": "Workflow", "graph": graph});
    let list = format!("/api/agents/{agent}/workflows");
    let workflow = app.post_expect(&list, create.clone(), 200).await?;
    ensure!(
        app.post_expect(&list, create.clone(), 200).await? == workflow,
        "create replay"
    );
    let workflow_id = id(&workflow)?;
    let path = format!("/api/workflows/{workflow_id}");
    let launch = json!({"id": new_id(), "revision": 1, "input": "INPUT"});
    let run_path = format!("/api/workflow-runs/{}", id(&launch)?);
    app.post_expect(&format!("{path}/run"), launch.clone(), 200)
        .await?;
    let mut run = app
        .wait_get(&run_path, |run| run["status"] == "approval")
        .await?;
    ensure!(
        models(&model) == ["override-model"],
        "models: {:?}",
        models(&model)
    );
    ensure!(run["last"] == "RESULT", "last: {run}");
    let first = model
        .requests()
        .first()
        .cloned()
        .ok_or("no model request")?;
    ensure!(
        first.tool_names().is_empty(),
        "Model workflow steps must not receive agent tools: {:?}",
        first.tool_names()
    );
    app.post_expect(
        &format!("{path}/edit"),
        with(&create, json!({"id": new_id(), "revision": 0})),
        409,
    )
    .await?;
    app.post_expect(
        &format!("{path}/edit"),
        with(
            &create,
            json!({"id": new_id(), "revision": 1, "graph": output_graph()}),
        ),
        200,
    )
    .await?;
    let latest = app.get_expect(&path, 200).await?;
    ensure!(latest["revision"] == 2, "revision: {latest}");
    ensure!(latest["graph"]["entry"] == "out", "entry: {latest}");
    app.get_expect(&format!("/api/workflows/{}", new_id()), 404)
        .await?;
    let versions = format!("{path}/versions");
    let revisions: Vec<Value> = app.get_expect(&versions, 200).await?["versions"]
        .as_array()
        .map(|rows| rows.iter().map(|row| row["revision"].clone()).collect())
        .unwrap_or_default();
    ensure!(revisions == [json!(2), json!(1)], "versions: {revisions:?}");
    ensure!(
        app.get_expect(&format!("{versions}?before=2"), 200).await?["versions"][0]["graph"]
            == graph,
        "older version graph"
    );
    for cursor in ["0", "-1", "1.5", "NaN", "9007199254740992"] {
        app.get_expect(&format!("{versions}?before={cursor}"), 400)
            .await?;
    }
    app.get_expect(&format!("{versions}?unknown=1"), 400)
        .await?;
    app.restart().await?;
    ensure!(
        app.get(&run_path).await?["status"] == "approval",
        "approval after restart"
    );
    app.post_expect(&format!("{path}/run"), launch.clone(), 200)
        .await?;
    ensure!(
        len(&app.get(&format!("{path}/runs")).await?) == 1,
        "run replay"
    );
    let approve = json!({"id": new_id(), "revision": run["revision"], "accepted": true});
    app.post_expect(
        &format!("{run_path}/approve"),
        with(&approve, json!({"accepted": "yes"})),
        400,
    )
    .await?;
    let mut missing = approve.clone();
    if let Some(fields) = missing.as_object_mut() {
        fields.remove("accepted");
    }
    app.post_expect(&format!("{run_path}/approve"), missing, 400)
        .await?;
    app.post_expect(&format!("{run_path}/approve"), approve.clone(), 200)
        .await?;
    let done = app
        .wait_get(&run_path, |run| run["status"] == "done")
        .await?;
    let nodes: Vec<Value> = done["steps"]
        .as_array()
        .map(|steps| steps.iter().map(|step| step["node"].clone()).collect())
        .unwrap_or_default();
    ensure!(
        nodes == [json!("model"), json!("approve"), json!("out")],
        "nodes: {done}"
    );
    ensure!(done["cursor"] == "out", "cursor: {done}");
    app.post_expect(&format!("{run_path}/approve"), approve.clone(), 200)
        .await?;
    app.post_expect(
        &format!("{run_path}/approve"),
        with(&approve, json!({"id": new_id()})),
        409,
    )
    .await?;
    ensure!(app.get(&run_path).await?["version"] == 1, "run version");
    ensure!(model.calls() == 1, "approval called the model");
    app.post_expect(
        &format!("{path}/edit"),
        with(&create, json!({"id": new_id(), "revision": 2})),
        200,
    )
    .await?;
    lock(&state)?.force_tool = true;
    let forced = app
        .post_expect(
            &format!("{path}/run"),
            json!({"id": new_id(), "revision": 3, "input": "FORCE TOOL"}),
            200,
        )
        .await?;
    let failed = run_until(&app, &id(&forced)?, |run| run["status"] == "failed").await?;
    ensure!(
        failed["steps"][0]["status"] == "failed",
        "forced step: {failed}"
    );
    let thread = failed["steps"][0]["thread"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    ensure!(
        len(&app
            .get_expect(&format!("/api/threads/{thread}/files"), 200)
            .await?)
            == 0,
        "model step saved a file"
    );
    lock(&state)?.force_tool = false;
    lock(&state)?.hold = true;
    let second = json!({"id": new_id(), "revision": 3, "input": "HOLD"});
    app.post_expect(&format!("{path}/run"), second.clone(), 200)
        .await?;
    let second_path = format!("/api/workflow-runs/{}", id(&second)?);
    app.wait_get(&second_path, |run| {
        run["steps"][0]["task_status"] == "running"
    })
    .await?;
    app.restart().await?;
    run = app.get(&second_path).await?;
    ensure!(
        run["steps"][0]["task_status"] == "interrupted",
        "after restart: {run}"
    );
    ensure!(run["status"] == "waiting", "status after restart: {run}");
    let count = model.calls();
    let cancel = json!({"id": new_id(), "revision": run["revision"]});
    app.post_expect(&format!("{second_path}/cancel"), cancel.clone(), 200)
        .await?;
    app.post_expect(&format!("{second_path}/cancel"), cancel, 200)
        .await?;
    let cancelled = app.get(&second_path).await?;
    ensure!(cancelled["status"] == "cancelled", "cancelled: {cancelled}");
    ensure!(
        cancelled["steps"][0]["status"] == "cancelled",
        "cancelled step: {cancelled}"
    );
    ensure!(model.calls() == count, "cancel replayed the model");
    lock(&state)?.hold = false;
    app.post_expect(
        &format!("{path}/edit"),
        with(
            &create,
            json!({"id": new_id(), "revision": 3, "graph": {"entry": "work", "maxSteps": 2, "nodes": [
                {"id": "work", "kind": "agent", "agent": agent, "prompt": "Agent tools remain available.", "next": "out"},
                {"id": "out", "kind": "output"}]}}),
        ),
        200,
    )
    .await?;
    let agent_run = app
        .post_expect(
            &format!("{path}/run"),
            json!({"id": new_id(), "revision": 4, "input": "AGENT"}),
            200,
        )
        .await?;
    let agent_run = id(&agent_run)?;
    let result = run_until(&app, &agent_run, |run| run["status"] == "done").await?;
    ensure!(result["last"] == "RESULT", "agent step: {result}");
    let statuses: Vec<Value> = result["steps"]
        .as_array()
        .map(|steps| steps.iter().map(|step| step["status"].clone()).collect())
        .unwrap_or_default();
    ensure!(
        statuses == [json!("done"), json!("done")],
        "agent steps: {result}"
    );
    let tools = model
        .requests()
        .last()
        .map(|r| r.tool_names())
        .unwrap_or_default();
    ensure!(
        tools.iter().any(|t| t == "create_text_file"),
        "agent tools: {tools:?}"
    );
    ensure!(
        tools.iter().any(|t| t == "set_routine_flow"),
        "agent tools: {tools:?}"
    );
    let restore_path = format!("{path}/restore");
    let restore = json!({"id": new_id(), "revision": 4, "version": 1});
    app.post_expect(&restore_path, with(&restore, json!({"version": "1"})), 400)
        .await?;
    app.post_expect(&restore_path, with(&restore, json!({"version": 999})), 404)
        .await?;
    app.post_expect(&restore_path, with(&restore, json!({"revision": 3})), 409)
        .await?;
    let restored = app.post_expect(&restore_path, restore.clone(), 200).await?;
    ensure!(restored["revision"] == 5, "restored revision: {restored}");
    ensure!(restored["graph"] == graph, "restored graph: {restored}");
    let agent_view = app.get(&format!("/api/workflow-runs/{agent_run}")).await?;
    ensure!(
        agent_view["version"] == 4,
        "run version changed: {agent_view}"
    );
    ensure!(
        agent_view["graph"]["entry"] == "work",
        "run graph changed: {agent_view}"
    );
    app.restart().await?;
    ensure!(
        app.post_expect(&restore_path, restore.clone(), 200).await? == restored,
        "restore receipt"
    );
    ensure!(
        len(&app.get(&versions).await?["versions"]) == 5,
        "version count"
    );
    app.post_expect(&restore_path, with(&restore, json!({"version": 2})), 409)
        .await?;
    let rejected_run = app
        .post_expect(
            &format!("{path}/run"),
            json!({"id": new_id(), "revision": 5, "input": "REJECT"}),
            200,
        )
        .await?;
    let rejected_path = format!("/api/workflow-runs/{}", id(&rejected_run)?);
    let awaiting = app
        .wait_get(&rejected_path, |run| run["status"] == "approval")
        .await?;
    let reject = json!({"id": new_id(), "revision": awaiting["revision"], "accepted": false});
    let before_reject = model.calls();
    app.post_expect(&format!("{rejected_path}/approve"), reject.clone(), 200)
        .await?;
    let rejected = app.get(&rejected_path).await?;
    ensure!(rejected["status"] == "failed", "rejected: {rejected}");
    ensure!(
        rejected["error"] == "Workflow approval denied.",
        "reject error: {rejected}"
    );
    ensure!(len(&rejected["steps"]) == 2, "reject steps: {rejected}");
    ensure!(
        rejected["steps"][1]["detail"] == "rejected",
        "reject detail: {rejected}"
    );
    ensure!(rejected["last"] == "RESULT", "reject last: {rejected}");
    app.restart().await?;
    app.post_expect(&format!("{rejected_path}/approve"), reject.clone(), 200)
        .await?;
    app.post_expect(
        &format!("{rejected_path}/approve"),
        with(&reject, json!({"id": new_id(), "accepted": true})),
        409,
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let rejected = app.get(&rejected_path).await?;
    ensure!(
        rejected["status"] == "failed",
        "rejected resumed: {rejected}"
    );
    ensure!(len(&rejected["steps"]) == 2, "rejected grew: {rejected}");
    ensure!(
        model.calls() == before_reject,
        "rejected run called the model"
    );
    ensure!(
        app.get(&format!("/api/agents/{agent}/turns")).await? == json!([]),
        "workflow entered main history"
    );

    let draft = json!({"id": "draft", "kind": "agent", "agent": agent, "prompt": "Draft", "next": "approve"});
    let retry_graph = json!({"entry": "draft", "maxSteps": 8, "nodes": [draft,
        {"id": "approve", "kind": "approval", "prompt": "Accept?", "yes": "out", "no": "draft"},
        {"id": "out", "kind": "output"}]});
    let retryable = app
        .post_expect(
            &list,
            json!({"id": new_id(), "revision": 0, "name": "Retry evidence", "graph": retry_graph}),
            200,
        )
        .await?;
    let retry_workflow = format!("/api/workflows/{}", id(&retryable)?);
    lock(&state)?.answer = "x".repeat(256 * 1024 + 1);
    let oversized = app
        .post_expect(
            &format!("{retry_workflow}/run"),
            json!({"id": new_id(), "revision": 1, "input": "INPUT"}),
            200,
        )
        .await?;
    let oversized = id(&oversized)?;
    let failed_path = format!("/api/workflow-runs/{oversized}");
    let too_large = run_until(&app, &oversized, |run| run["status"] == "failed").await?;
    ensure!(
        too_large["error"]
            .as_str()
            .is_some_and(|error| error.contains("Step result exceeds 256 KiB")),
        "oversized: {}",
        too_large["error"]
    );
    ensure!(
        too_large["steps"][0]["status"] == "failed",
        "oversized step"
    );
    app.post_expect(
        &format!("{retry_workflow}/edit"),
        json!({"id": new_id(), "revision": 1, "name": "Changed kind", "graph": {"entry": "draft",
            "maxSteps": 2, "nodes": [{"id": "draft", "kind": "condition", "operator": "equals",
                "value": "INPUT", "yes": "out", "no": "out"}, {"id": "out", "kind": "output"}]}}),
        200,
    )
    .await?;
    app.post_expect(
        &format!("{failed_path}/retry"),
        json!({"id": new_id(), "revision": too_large["revision"]}),
        200,
    )
    .await?;
    let failed_again = run_until(&app, &oversized, |run| {
        run["status"] == "failed" && len(&run["steps"]) == 2
    })
    .await?;
    let settings: Value = serde_json::from_str(
        failed_again["steps"][1]["settings"]
            .as_str()
            .unwrap_or("null"),
    )?;
    ensure!(settings == draft, "retry settings: {settings}");
    app.post_expect(
        &format!("{failed_path}/retry"),
        json!({"id": new_id(), "revision": too_large["revision"]}),
        409,
    )
    .await?;
    app.post_expect(
        &format!("{retry_workflow}/edit"),
        json!({"id": new_id(), "revision": 2, "name": "Removed node", "graph": output_graph()}),
        200,
    )
    .await?;
    lock(&state)?.answer = "RECOVERED".into();
    app.post_expect(
        &format!("{failed_path}/retry"),
        json!({"id": new_id(), "revision": failed_again["revision"]}),
        200,
    )
    .await?;
    let recovered = run_until(&app, &oversized, |run| run["status"] == "approval").await?;
    ensure!(recovered["version"] == 1, "recovered version: {recovered}");
    ensure!(
        recovered["last"] == "RECOVERED",
        "recovered last: {recovered}"
    );
    ensure!(
        recovered["graph"] == retry_graph,
        "recovered graph: {recovered}"
    );
    let settings: Value =
        serde_json::from_str(recovered["steps"][2]["settings"].as_str().unwrap_or("null"))?;
    ensure!(settings == draft, "recovered settings: {settings}");
    let statuses: Vec<Value> = recovered["steps"]
        .as_array()
        .map(|steps| steps.iter().map(|step| step["status"].clone()).collect())
        .unwrap_or_default();
    ensure!(
        statuses
            == [
                json!("failed"),
                json!("failed"),
                json!("done"),
                json!("approval")
            ],
        "recovered steps: {recovered}"
    );
    let current = app
        .post_expect(
            &format!("{retry_workflow}/run"),
            json!({"id": new_id(), "revision": 3, "input": "LATEST"}),
            200,
        )
        .await?;
    ensure!(
        run_until(&app, &id(&current)?, |run| run["status"] == "done").await?["last"] == "LATEST",
        "latest version run"
    );
    Ok(())
}

#[tokio::test]
async fn provider_free_workflows_continue_beyond_one_batch_and_configuration_listing_is_bounded(
) -> Result<()> {
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new()).await?;
    let agent = app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Local", "model": "unused",
                "instructions": "Help."}),
        )
        .await?;
    let list = format!("/api/agents/{}/workflows", id(&agent)?);
    let mut ids = Vec::new();
    for i in 0..21 {
        let saved = app
            .post(
                &list,
                json!({"id": new_id(), "revision": 0, "name": format!("Loop {i}"), "graph": {
                    "entry": "loop", "maxSteps": 40, "nodes": [
                        {"id": "loop", "kind": "condition", "operator": "equals", "value": "repeat",
                            "yes": "loop", "no": "out"}, {"id": "out", "kind": "output"}]}}),
            )
            .await?;
        ids.push(id(&saved)?);
    }
    let first = app.get(&list).await?;
    let after = first["next_after"]
        .as_str()
        .ok_or("no next_after")?
        .to_owned();
    let second = app.get(&format!("{list}?after={after}")).await?;
    ensure!(len(&first["workflows"]) == 20, "first page");
    ensure!(len(&second["workflows"]) == 1, "second page");
    ensure!(second["next_after"].is_null(), "next_after: {second}");
    let listed: Vec<String> = first["workflows"]
        .as_array()
        .into_iter()
        .chain(second["workflows"].as_array())
        .flatten()
        .filter_map(|row| row["id"].as_str().map(str::to_owned))
        .collect();
    let mut sorted = ids.clone();
    sorted.sort();
    ensure!(listed == sorted, "listing order");
    let versioned = app.get(&format!("/api/workflows/{}", ids[0])).await?;
    for revision in 1..=20 {
        app.post(
            &format!("/api/workflows/{}/edit", ids[0]),
            json!({"id": new_id(), "revision": revision, "name": versioned["name"],
                "graph": versioned["graph"]}),
        )
        .await?;
    }
    let versions = app
        .get(&format!("/api/workflows/{}/versions", ids[0]))
        .await?;
    let older = app
        .get(&format!(
            "/api/workflows/{}/versions?before={}",
            ids[0], versions["next_before"]
        ))
        .await?;
    ensure!(len(&versions["versions"]) == 20, "versions page");
    ensure!(len(&older["versions"]) == 1, "older page");
    ensure!(older["next_before"].is_null(), "next_before: {older}");
    let revisions: Vec<i64> = versions["versions"]
        .as_array()
        .into_iter()
        .chain(older["versions"].as_array())
        .flatten()
        .filter_map(|row| row["revision"].as_i64())
        .collect();
    ensure!(
        revisions == (1..=21).rev().collect::<Vec<_>>(),
        "revisions: {revisions:?}"
    );
    let run = app
        .post(
            &format!("/api/workflows/{}/run", ids[0]),
            json!({"id": new_id(), "revision": 21, "input": "repeat"}),
        )
        .await?;
    let current = run_until(&app, &id(&run)?, |run| run["status"] == "failed").await?;
    ensure!(
        len(&current["steps"]) == 40,
        "loop steps: {}",
        len(&current["steps"])
    );
    app.post_expect(
        &format!("/api/workflow-runs/{}/retry", id(&run)?),
        json!({"id": new_id(), "revision": current["revision"]}),
        409,
    )
    .await?;
    let schedule_path = format!("/api/workflows/{}/schedule", ids[1]);
    ensure!(
        app.get(&schedule_path).await?["schedule"].is_null(),
        "schedule before"
    );
    let command = json!({"id": new_id(), "revision": 0, "nextAt": FIXED_CLOCK - 1000,
        "intervalSeconds": null, "enabled": true, "input": "finish"});
    let saved = app.post(&schedule_path, command.clone()).await?;
    let current = app
        .wait_get(&schedule_path, |schedule| len(&schedule["occurrences"]) > 0)
        .await?;
    ensure!(len(&current["occurrences"]) == 1, "occurrences: {current}");
    ensure!(
        current["schedule"]["enabled"] == 0,
        "one-shot enabled: {current}"
    );
    let scheduled = app
        .get(&format!(
            "/api/workflow-runs/{}",
            current["occurrences"][0]["run"]
                .as_str()
                .unwrap_or_default()
        ))
        .await?;
    ensure!(scheduled["status"] == "done", "scheduled run: {scheduled}");
    ensure!(
        scheduled["input"] == "finish",
        "scheduled input: {scheduled}"
    );
    app.restart().await?;
    ensure!(
        app.post(&schedule_path, command).await? == saved,
        "schedule receipt"
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    ensure!(
        len(&app.get(&schedule_path).await?["occurrences"]) == 1,
        "occurrence repeated"
    );
    ensure!(
        len(&app.get(&format!("/api/workflows/{}/runs", ids[1])).await?) == 1,
        "scheduled run repeated"
    );
    Ok(())
}

/// Signed workflow webhook delivery; returns (status, body).
async fn deliver(
    app: &Server,
    route: &str,
    secret: &str,
    delivery: &str,
    payload: Value,
    browser: bool,
) -> Result<(u16, Value)> {
    let body = json!({"event": "build.done", "payload": payload}).to_string();
    deliver_raw(app, route, secret, delivery, &body, browser).await
}

async fn deliver_raw(
    app: &Server,
    route: &str,
    secret: &str,
    delivery: &str,
    body: &str,
    browser: bool,
) -> Result<(u16, Value)> {
    let time = (now() / 1000).to_string();
    let signature = hmac::sha256_hex(
        secret.as_bytes(),
        &[format!("{time}\n{delivery}\n").as_bytes(), body.as_bytes()],
    );
    let mut request = app
        .client()?
        .post(app.url(&format!("/api/workflow-webhooks/{route}")))
        .header("content-type", "application/json")
        .header("x-minarbot-time", time)
        .header("x-minarbot-id", delivery)
        .header("x-minarbot-signature", signature)
        .body(body.to_owned());
    if browser {
        request = request.header("origin", app.origin());
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    let text = response.text().await?;
    Ok((
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    ))
}

async fn expect_delivery(
    app: &Server,
    route: &str,
    secret: &str,
    delivery: &str,
    payload: Value,
    status: u16,
) -> Result<Value> {
    let (got, body) = deliver(app, route, secret, delivery, payload, false).await?;
    ensure!(
        got == status,
        "delivery {delivery}: expected {status}, got {got}: {body}"
    );
    Ok(body)
}

#[tokio::test]
async fn workflow_webhooks_authenticate_deduplicate_ignore_busy_runs_and_recover_rotation(
) -> Result<()> {
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new().real_clock()).await?;
    let owner = id(&api_agent(&app, "Hook").await?)?;
    let workflow = app
        .post(
            &format!("/api/agents/{owner}/workflows"),
            json!({"id": new_id(), "revision": 0, "name": "Hook", "graph": {"entry": "review",
                "maxSteps": 3, "nodes": [{"id": "review", "kind": "approval", "prompt": "Accept?", "yes": "out"},
                    {"id": "out", "kind": "output"}]}}),
        )
        .await?;
    let workflow = id(&workflow)?;
    let path = format!("/api/workflows/{workflow}/webhook");
    ensure!(
        app.get_expect(&path, 200).await?.is_null(),
        "webhook before setup"
    );
    let setup = json!({"id": new_id(), "revision": 0, "rotate": true, "enabled": true});
    let hook = app.post_expect(&path, setup.clone(), 200).await?;
    let (route, secret) = (
        hook["route"].as_str().ok_or("route")?.to_owned(),
        hook["secret"].as_str().ok_or("secret")?.to_owned(),
    );
    ensure!(
        app.post_expect(&path, setup.clone(), 200).await?["secret"] == secret.as_str(),
        "setup replay secret"
    );
    ensure!(
        app.get_expect(&path, 200).await?["secret"].is_null(),
        "secret listed"
    );
    expect_delivery(&app, &route, "wrong", "bad", json!({}), 401).await?;
    expect_delivery(
        &app,
        &route,
        &secret,
        "oversize",
        json!("x".repeat(64000)),
        413,
    )
    .await?;
    let (browser, body) = deliver(&app, &route, &secret, "browser", json!({}), true).await?;
    ensure!(browser == 403, "browser delivery: {browser} {body}");
    let marker = json!({"marker": "external"});
    let event = expect_delivery(&app, &route, &secret, "first", marker.clone(), 202).await?;
    ensure!(event["outcome"] == "queued", "first: {event}");
    ensure!(
        expect_delivery(&app, &route, &secret, "first", marker.clone(), 200).await? == event,
        "duplicate delivery"
    );
    expect_delivery(
        &app,
        &route,
        &secret,
        "first",
        json!({"changed": true}),
        409,
    )
    .await?;
    ensure!(
        expect_delivery(&app, &route, &secret, "busy", marker.clone(), 202).await?
            == json!({"outcome": "ignored", "reason": "busy"}),
        "busy"
    );
    let event_id = id(&event)?;
    let run = app
        .get_expect(&format!("/api/workflow-runs/{event_id}"), 200)
        .await?;
    let input = run["input"].as_str().unwrap_or_default();
    ensure!(
        input.contains("not instructions or permissions"),
        "input: {input}"
    );
    ensure!(input.contains("external"), "input: {input}");
    app.post_expect(
        &format!("/api/workflow-runs/{event_id}/cancel"),
        json!({"id": new_id(), "revision": run["revision"]}),
        200,
    )
    .await?;
    ensure!(
        expect_delivery(&app, &route, &secret, "busy", marker.clone(), 200).await?
            == json!({"outcome": "ignored", "reason": "busy"}),
        "busy receipt"
    );
    let rotation = json!({"id": new_id(), "revision": 1, "rotate": true, "enabled": false});
    let rotated = app.post_expect(&path, rotation.clone(), 200).await?;
    let rotated_secret = rotated["secret"]
        .as_str()
        .ok_or("rotated secret")?
        .to_owned();
    app.post_expect(&path, setup.clone(), 409).await?;
    expect_delivery(&app, &route, &secret, "old", json!({}), 401).await?;
    ensure!(
        expect_delivery(
            &app,
            &route,
            &rotated_secret,
            "disabled",
            marker.clone(),
            202
        )
        .await?["reason"]
            == "disabled",
        "disabled"
    );
    app.shutdown().await?;
    {
        let db = Fixture::open(app.database())?;
        db.execute(
            "UPDATE workflow_webhook_receipts SET received=? WHERE delivery=?",
            (now() - 8 * 86_400_000, "disabled"),
        )?;
    }
    let config = app.config().clone();
    app.start_stopped(config).await?;
    ensure!(
        app.post_expect(&path, rotation.clone(), 200).await?["secret"] == rotated_secret.as_str(),
        "rotation receipt"
    );
    ensure!(
        len(&app
            .get_expect(&format!("/api/workflows/{workflow}/webhook-events"), 200)
            .await?)
            == 3,
        "events after retention"
    );
    ensure!(
        len(&app
            .get_expect(&format!("/api/workflows/{workflow}/runs"), 200)
            .await?)
            == 1,
        "runs"
    );
    app.post_expect(
        &path,
        json!({"id": new_id(), "revision": 2, "rotate": false, "enabled": true}),
        200,
    )
    .await?;
    let active = expect_delivery(
        &app,
        &route,
        &rotated_secret,
        "disabled",
        marker.clone(),
        202,
    )
    .await?;
    ensure!(
        active["outcome"] == "queued",
        "expired receipt reused: {active}"
    );
    let removal = json!({"id": new_id(), "revision": 3, "remove": true, "route": route});
    ensure!(
        app.post_expect(&path, removal.clone(), 200).await? == json!({"removed": true}),
        "removal"
    );
    ensure!(
        app.get_expect(&path, 200).await? == json!({"route": null, "revision": 4, "enabled": 0}),
        "removed hook"
    );
    expect_delivery(
        &app,
        &route,
        &rotated_secret,
        "disabled",
        marker.clone(),
        404,
    )
    .await?;
    ensure!(
        app.get_expect(&format!("/api/workflow-runs/{}", id(&active)?), 200)
            .await?["id"]
            == active["id"],
        "run kept after removal"
    );
    app.restart().await?;
    ensure!(
        app.post_expect(&path, removal.clone(), 200).await? == json!({"removed": true}),
        "removal receipt"
    );
    app.post_expect(&path, rotation, 409).await?;
    app.post_expect(
        &path,
        json!({"id": new_id(), "revision": 0, "rotate": true, "enabled": true}),
        409,
    )
    .await?;
    app.post_expect(
        &path,
        json!({"id": new_id(), "revision": 3, "rotate": true, "enabled": true}),
        409,
    )
    .await?;
    let replacement = app
        .post_expect(
            &path,
            json!({"id": new_id(), "revision": 4, "rotate": true, "enabled": true}),
            200,
        )
        .await?;
    ensure!(replacement["route"] != route.as_str(), "route reused");
    ensure!(
        replacement["revision"] == 5,
        "replacement revision: {replacement}"
    );
    ensure!(
        app.post_expect(&path, removal.clone(), 200).await? == json!({"removed": true}),
        "old removal receipt"
    );
    ensure!(
        app.get_expect(&path, 200).await?["route"] == replacement["route"],
        "replacement removed"
    );
    app.post_expect(&path, with(&removal, json!({"id": new_id()})), 409)
        .await?;
    app.post_expect(
        &path,
        json!({"id": new_id(), "revision": 3, "rotate": false, "enabled": false}),
        409,
    )
    .await?;
    expect_delivery(
        &app,
        &route,
        &rotated_secret,
        "old-after-recreate",
        marker.clone(),
        404,
    )
    .await?;
    let replacement_route = replacement["route"].as_str().unwrap_or_default();
    let replacement_secret = replacement["secret"].as_str().unwrap_or_default();
    ensure!(
        expect_delivery(
            &app,
            replacement_route,
            replacement_secret,
            "disabled",
            marker,
            202
        )
        .await?["reason"]
            == "busy",
        "replacement delivery"
    );
    let events = app
        .get_expect(&format!("/api/workflows/{workflow}/webhook-events"), 200)
        .await?;
    let disabled: Vec<&Value> = events
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| row["delivery"] == "disabled")
                .collect()
        })
        .unwrap_or_default();
    ensure!(disabled.len() == 2, "disabled events: {events}");
    ensure!(
        disabled[0]["route"] != disabled[1]["route"],
        "routes: {events}"
    );
    Ok(())
}

/// Bind `agent` to the test gateway instance.
async fn bind(app: &Server, agent: &str) -> Result<()> {
    let agents = app.get("/api/agents").await?;
    let current = agents["agents"]
        .as_array()
        .or_else(|| agents.as_array())
        .and_then(|rows| rows.iter().find(|row| row["id"] == agent).cloned())
        .ok_or("agent missing")?;
    app.post(
        &format!("/api/agents/{agent}/edit"),
        json!({"id": new_id(), "revision": current["revision"], "name": current["name"],
            "model": current["model"], "instructions": current["instructions"],
            "providerInstance": "codex-test-gateway"}),
    )
    .await?;
    Ok(())
}

/// Regression for the parallel-branch bug found by the port below: a branch
/// that cannot start (here: an agent without a provider instance) aborted the
/// whole automation tick every second, so the run stayed `ready` and other
/// workflows stalled. It now fails only that run.
#[tokio::test]
async fn parallel_branch_start_failure_fails_only_its_run() -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|_| Reply::text("ok"));
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new()).await?;
    let owner = id(&api_agent(&app, "Unbound").await?)?;
    let list = format!("/api/agents/{owner}/workflows");
    let parallel = app
        .post(
            &list,
            json!({"id": new_id(), "revision": 0, "name": "Parallel", "graph": {"entry": "fan",
                "maxSteps": 4, "nodes": [{"id": "fan", "kind": "parallel", "branches": [
                    {"kind": "model", "model": "fixture", "prompt": "A"},
                    {"kind": "model", "model": "fixture", "prompt": "B"}], "next": "out"},
                    {"id": "out", "kind": "output"}]}}),
        )
        .await?;
    let plain = app
        .post(
            &list,
            json!({"id": new_id(), "revision": 0, "name": "Plain", "graph": output_graph()}),
        )
        .await?;
    app.restart_with(ServerConfig::new().model(&model)).await?;
    let run = app
        .post(
            &format!("/api/workflows/{}/run", id(&parallel)?),
            json!({"id": new_id(), "revision": 1, "input": "x"}),
        )
        .await?;
    let failed = settled(&app, &id(&run)?).await?;
    ensure!(failed["status"] == "failed", "parallel run: {failed}");
    ensure!(
        failed["error"] == "Choose a provider instance before starting this task.",
        "error: {failed}"
    );
    ensure!(
        len(&failed["steps"]) == 0,
        "partial branches kept: {failed}"
    );
    let other = app
        .post(
            &format!("/api/workflows/{}/run", id(&plain)?),
            json!({"id": new_id(), "revision": 1, "input": "still runs"}),
        )
        .await?;
    let done = settled(&app, &id(&other)?).await?;
    ensure!(done["status"] == "done", "other workflow: {done}");
    ensure!(model.calls() == 0, "a failed branch reached the model");
    Ok(())
}

#[tokio::test]
async fn parallel_webhook_waits_for_provider_before_creating_a_receipt() -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|_| Reply::text("ok"));
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new().real_clock()).await?;
    let owner = id(&api_agent(&app, "Parallel hook").await?)?;
    let workflow = app
        .post(
            &format!("/api/agents/{owner}/workflows"),
            json!({"id": new_id(), "revision": 0, "name": "Parallel hook", "graph": {"entry": "fan",
                "maxSteps": 4, "nodes": [{"id": "fan", "kind": "parallel", "branches": [
                    {"kind": "agent", "agent": owner, "prompt": "A"},
                    {"kind": "model", "model": "fixture", "prompt": "B"}], "next": "out"},
                    {"id": "out", "kind": "output"}]}}),
        )
        .await?;
    let workflow = id(&workflow)?;
    let hook = app
        .post(
            &format!("/api/workflows/{workflow}/webhook"),
            json!({"id": new_id(), "revision": 0, "enabled": true, "rotate": true}),
        )
        .await?;
    let route = hook["route"].as_str().ok_or("route")?.to_owned();
    let secret = hook["secret"].as_str().ok_or("secret")?.to_owned();
    let body = json!({"event": "parallel", "payload": {}}).to_string();
    let delivery = new_id();
    let (status, reply) = deliver_raw(&app, &route, &secret, &delivery, &body, false).await?;
    ensure!(status == 503, "without provider: {status} {reply}");
    ensure!(
        app.get_expect(&format!("/api/workflows/{workflow}/runs"), 200)
            .await?
            == json!([]),
        "run created without provider"
    );
    ensure!(
        app.get_expect(&format!("/api/workflows/{workflow}/webhook-events"), 200)
            .await?
            == json!([]),
        "receipt created without provider"
    );
    app.restart_with(ServerConfig::new().real_clock().model(&model))
        .await?;
    // #87: the agent was created before any provider instance existed, so it
    // is bound to the new instance explicitly (the Node case relied on the
    // global provider).
    bind(&app, &owner).await?;
    let (status, receipt) = deliver_raw(&app, &route, &secret, &delivery, &body, false).await?;
    ensure!(status == 202, "with provider: {status} {receipt}");
    ensure!(receipt["outcome"] == "queued", "receipt: {receipt}");
    let (status, _) = deliver_raw(&app, &route, &secret, &delivery, &body, false).await?;
    ensure!(status == 200, "duplicate: {status}");
    ensure!(
        len(&app.get(&format!("/api/workflows/{workflow}/runs")).await?) == 1,
        "duplicate run"
    );
    let run = settled(&app, &id(&receipt)?).await?;
    ensure!(run["status"] == "done", "parallel run: {run}");
    ensure!(
        run["input"]
            .as_str()
            .is_some_and(|input| input.contains("\"event\":\"parallel\"")),
        "input: {run}"
    );
    let last: Value = serde_json::from_str(run["last"].as_str().unwrap_or("null"))?;
    ensure!(
        last == json!([{"branch": 1, "result": "ok"}, {"branch": 2, "result": "ok"}]),
        "branches: {last}"
    );
    Ok(())
}

#[tokio::test]
async fn workspace_workflow_directory_pages_across_owners_and_keeps_per_agent_lists_scoped(
) -> Result<()> {
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new()).await?;
    ensure!(
        app.get_expect("/api/workflows", 200).await?
            == json!({"workflows": [], "next_after": null}),
        "empty directory"
    );
    let mut owners = Vec::new();
    for name in ["Alpha", "Beta"] {
        owners.push(api_agent(&app, name).await?);
    }
    let mut workflows = Vec::new();
    for index in 0..21 {
        let agent = id(&owners[index % 2])?;
        workflows.push(
            app.post_expect(
                &format!("/api/agents/{agent}/workflows"),
                json!({"id": new_id(), "revision": 0, "name": format!("Workflow {index}"),
                    "graph": {"entry": "out", "maxSteps": 1, "nodes": [{"id": "out", "kind": "output"}],
                        "positions": {"out": {"x": 100 + index, "y": 200 + index}}}}),
                200,
            )
            .await?,
        );
    }
    let first = app.get_expect("/api/workflows", 200).await?;
    ensure!(len(&first["workflows"]) == 20, "first page");
    let after = first["next_after"]
        .as_str()
        .ok_or("no next_after")?
        .to_owned();
    let second = app
        .get_expect(&format!("/api/workflows?after={after}"), 200)
        .await?;
    ensure!(len(&second["workflows"]) == 1, "second page");
    ensure!(second["next_after"].is_null(), "next_after");
    let rows: Vec<Value> = first["workflows"]
        .as_array()
        .into_iter()
        .chain(second["workflows"].as_array())
        .flatten()
        .cloned()
        .collect();
    let mut expected: Vec<Value> = workflows.iter().map(|row| row["id"].clone()).collect();
    expected.sort_by_key(|value| value.as_str().unwrap_or_default().to_owned());
    ensure!(
        rows.iter().map(|row| row["id"].clone()).collect::<Vec<_>>() == expected,
        "directory order"
    );
    for row in &rows {
        let owner = owners
            .iter()
            .find(|owner| owner["id"] == row["agent"])
            .ok_or("owner")?;
        ensure!(row["owner_name"] == owner["name"], "owner name: {row}");
        let saved = workflows
            .iter()
            .find(|w| w["id"] == row["id"])
            .ok_or("workflow")?;
        ensure!(
            row["graph"]["positions"] == saved["graph"]["positions"],
            "positions: {row}"
        );
        let detail = app
            .get_expect(&format!("/api/workflows/{}", id(row)?), 200)
            .await?;
        ensure!(
            detail["graph"]["positions"] == row["graph"]["positions"],
            "detail positions"
        );
    }
    for owner in &owners {
        let scoped = app
            .get_expect(&format!("/api/agents/{}/workflows", id(owner)?), 200)
            .await?;
        let listed: Vec<Value> = scoped["workflows"]
            .as_array()
            .map(|rows| rows.iter().map(|row| row["id"].clone()).collect())
            .unwrap_or_default();
        let own: Vec<Value> = rows
            .iter()
            .filter(|row| row["agent"] == owner["id"])
            .map(|row| row["id"].clone())
            .collect();
        ensure!(listed == own, "scoped list");
    }
    let workflow = rows[0].clone();
    let workflow_id = id(&workflow)?;
    app.post_expect(
        &format!("/api/workflows/{workflow_id}/edit"),
        json!({"id": new_id(), "revision": 1, "name": "Changed from directory", "graph": workflow["graph"]}),
        200,
    )
    .await?;
    ensure!(
        app.get_expect("/api/workflows", 200).await?["workflows"][0]["revision"] == 2,
        "directory revision"
    );
    let run = app
        .post_expect(
            &format!("/api/workflows/{workflow_id}/run"),
            json!({"id": new_id(), "revision": 2, "input": ""}),
            200,
        )
        .await?;
    let mut moved = workflow["graph"].clone();
    moved["positions"] = json!({"out": {"x": 800, "y": 900}});
    app.post_expect(
        &format!("/api/workflows/{workflow_id}/edit"),
        json!({"id": new_id(), "revision": 2, "name": "Moved", "graph": moved}),
        200,
    )
    .await?;
    ensure!(
        app.get_expect(&format!("/api/workflow-runs/{}", id(&run)?), 200)
            .await?["graph"]["positions"]
            == workflow["graph"]["positions"],
        "run positions changed"
    );
    app.restart().await?;
    ensure!(
        app.get_expect(&format!("/api/workflows/{workflow_id}"), 200)
            .await?["graph"]["positions"]
            == json!({"out": {"x": 800, "y": 900}}),
        "moved positions after restart"
    );
    let restored = app
        .post_expect(
            &format!("/api/workflows/{workflow_id}/restore"),
            json!({"id": new_id(), "revision": 3, "version": 1}),
            200,
        )
        .await?;
    ensure!(
        restored["graph"]["positions"] == workflow["graph"]["positions"],
        "restored positions"
    );
    for query in ["after=bad", "before=1", "owner=anything"] {
        app.get_expect(&format!("/api/workflows?{query}"), 400)
            .await?;
    }
    Ok(())
}

#[tokio::test]
async fn autosave_stops_after_server_canonicalizes_condition_property_order() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let agent = id(&api_agent(&app, "Autosave").await?)?;
    let graph = json!({"entry": "check", "maxSteps": 2, "nodes": [
        {"id": "check", "kind": "condition", "operator": "equals", "value": "yes", "yes": "out", "no": "out"},
        {"id": "out", "kind": "output"}]});
    let saved = app
        .post(
            &format!("/api/agents/{agent}/workflows"),
            json!({"id": new_id(), "revision": 0, "name": "Autosave", "graph": graph}),
        )
        .await?;
    // The draft the editor sends: same graph with condition fields reordered
    // and a changed operator.
    let local = json!({"entry": "check", "maxSteps": 2, "nodes": [
        {"id": "check", "kind": "condition", "value": "yes", "yes": "out", "no": "out", "operator": "contains"},
        {"id": "out", "kind": "output"}]});
    let ack = app
        .post(
            &format!("/api/workflows/{}/edit", id(&saved)?),
            json!({"id": new_id(), "name": "Autosave", "graph": local, "revision": saved["revision"]}),
        )
        .await?;
    ensure!(ack["revision"] == 2, "ack: {ack}");
    ensure!(ack["name"] == "Autosave", "ack name: {ack}");
    ensure!(
        ack["graph"] == local,
        "canonical graph differs from the draft: {ack}"
    );
    Ok(())
}

#[tokio::test]
async fn workflow_deletion_revokes_triggers_rejects_stale_commands_and_retains_terminal_results(
) -> Result<()> {
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new()).await?;
    let agent = id(&api_agent(&app, "Delete").await?)?;
    let list = format!("/api/agents/{agent}/workflows");
    let graph = json!({"entry": "review", "maxSteps": 2, "nodes": [
        {"id": "review", "kind": "approval", "prompt": "Accept?", "yes": "out"}, {"id": "out", "kind": "output"}]});
    let create = json!({"id": new_id(), "revision": 0, "name": "Delete me", "graph": graph});
    let workflow = app.post_expect(&list, create.clone(), 200).await?;
    let path = format!("/api/workflows/{}", id(&workflow)?);
    let schedule = json!({"id": new_id(), "revision": 0, "enabled": true, "nextAt": FIXED_CLOCK + 3_600_000,
        "intervalSeconds": 60, "input": "scheduled"});
    app.post_expect(&format!("{path}/schedule"), schedule.clone(), 200)
        .await?;
    let hook_command = json!({"id": new_id(), "revision": 0, "rotate": true, "enabled": true});
    let hook = app
        .post_expect(&format!("{path}/webhook"), hook_command.clone(), 200)
        .await?;
    let run_command = json!({"id": new_id(), "revision": 1, "input": "retained marker"});
    let run = app
        .post_expect(&format!("{path}/run"), run_command.clone(), 200)
        .await?;
    let run_id = id(&run)?;
    app.post_expect(
        &format!("{path}/delete"),
        json!({"id": new_id(), "revision": 1}),
        409,
    )
    .await?;
    let current = app
        .get_expect(&format!("/api/workflow-runs/{run_id}"), 200)
        .await?;
    app.post_expect(
        &format!("/api/workflow-runs/{run_id}/cancel"),
        json!({"id": new_id(), "revision": current["revision"]}),
        200,
    )
    .await?;
    let deletion = json!({"id": new_id(), "revision": 1});
    app.post_expect(
        &format!("{path}/delete"),
        with(&deletion, json!({"revision": 99})),
        409,
    )
    .await?;
    ensure!(
        app.post_expect(&format!("{path}/delete"), deletion.clone(), 200)
            .await?
            == json!({"id": workflow["id"], "deleted": 1}),
        "deletion"
    );
    ensure!(
        app.get_expect(&path, 200).await?["deleted"] == 1,
        "deleted flag"
    );
    ensure!(
        len(&app.get_expect("/api/workflows", 200).await?["workflows"]) == 0,
        "directory"
    );
    ensure!(
        len(&app.get_expect(&list, 200).await?["workflows"]) == 0,
        "agent list"
    );
    ensure!(
        app.get_expect("/api/workflows?deleted=1", 200).await?["workflows"][0]["id"]
            == workflow["id"],
        "deleted listing"
    );
    ensure!(
        app.get_expect(&format!("{path}/schedule"), 200).await?["schedule"]["enabled"] == 0,
        "schedule still enabled"
    );
    let revision = current["revision"].as_i64().unwrap_or_default() + 1;
    for (endpoint, command) in [
        (list.clone(), create.clone()),
        (format!("{path}/run"), run_command.clone()),
        (format!("{path}/schedule"), schedule.clone()),
        (format!("{path}/webhook"), hook_command.clone()),
        (
            format!("{path}/edit"),
            json!({"id": new_id(), "revision": 1, "name": "Resurrect", "graph": graph}),
        ),
        (
            format!("{path}/restore"),
            json!({"id": new_id(), "revision": 1, "version": 1}),
        ),
        (
            format!("/api/workflow-runs/{run_id}/retry"),
            json!({"id": new_id(), "revision": revision}),
        ),
    ] {
        app.post_expect(&endpoint, command, 404).await?;
    }
    let status = app
        .client()?
        .post(app.url(&format!(
            "/api/workflow-webhooks/{}",
            hook["route"].as_str().unwrap_or_default()
        )))
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await?
        .status();
    ensure!(status == 404, "revoked webhook: {status}");
    app.restart().await?;
    ensure!(
        app.post_expect(&format!("{path}/delete"), deletion, 200)
            .await?
            == json!({"id": workflow["id"], "deleted": 1}),
        "deletion receipt"
    );
    let preserved = app
        .get_expect(&format!("/api/workflow-runs/{run_id}"), 200)
        .await?;
    ensure!(
        preserved["input"] == "retained marker",
        "input: {preserved}"
    );
    ensure!(preserved["status"] == "cancelled", "status: {preserved}");
    ensure!(preserved["graph"] == graph, "graph: {preserved}");
    ensure!(
        len(&app.get_expect(&format!("{path}/runs"), 200).await?) == 1,
        "runs kept"
    );
    app.post_expect(
        &format!("{path}/delete"),
        json!({"id": new_id(), "revision": 1}),
        404,
    )
    .await?;
    app.get_expect("/api/workflows?deleted=invalid", 400)
        .await?;
    Ok(())
}

#[tokio::test]
async fn workflow_output_inbox_survives_restart_with_stable_paging_and_idempotent_read(
) -> Result<()> {
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new()).await?;
    let agent = id(&api_agent(&app, "Inbox").await?)?;
    let workflow = app
        .post_expect(
            &format!("/api/agents/{agent}/workflows"),
            json!({"id": new_id(), "name": "Output", "revision": 0, "graph": output_graph()}),
            200,
        )
        .await?;
    let workflow = id(&workflow)?;
    for index in 0..21 {
        let body = json!({"id": new_id(), "revision": 1, "input": format!("Result {index}")});
        let run = app
            .post_expect(&format!("/api/workflows/{workflow}/run"), body.clone(), 200)
            .await?;
        ensure!(
            app.post_expect(&format!("/api/workflows/{workflow}/run"), body, 200)
                .await?
                == run,
            "run replay {index}"
        );
    }
    let first = app
        .wait_get("/api/workflow-notifications", |page| page["unread"] == 21)
        .await?;
    ensure!(first["unread"] == 21, "unread: {first}");
    ensure!(len(&first["notifications"]) == 20, "first page");
    let second = app
        .get_expect(
            &format!(
                "/api/workflow-notifications?before={}",
                first["next_before"]
            ),
            200,
        )
        .await?;
    ensure!(len(&second["notifications"]) == 1, "second page");
    ensure!(second["next_before"].is_null(), "next_before");
    let mut runs: Vec<String> = first["notifications"]
        .as_array()
        .into_iter()
        .chain(second["notifications"].as_array())
        .flatten()
        .map(|item| item["run"].to_string())
        .collect();
    runs.sort();
    runs.dedup();
    ensure!(runs.len() == 21, "distinct runs: {}", runs.len());
    let selected = first["notifications"][0]["run"]
        .as_str()
        .ok_or("run")?
        .to_owned();
    ensure!(
        app.get_expect(&format!("/api/workflow-runs/{selected}"), 200)
            .await?["last"]
            == "Result 20",
        "newest first"
    );
    let body = json!({"id": new_id()});
    let read = format!("/api/workflow-notifications/{selected}/read");
    app.post_expect(&read, body.clone(), 200).await?;
    app.post_expect(&read, body.clone(), 200).await?;
    app.get_expect("/api/workflow-notifications?before=0", 400)
        .await?;
    app.get_expect("/api/workflow-notifications?before=1e2", 400)
        .await?;
    app.post_expect(
        &format!("/api/workflow-notifications/{}/read", new_id()),
        body,
        404,
    )
    .await?;
    app.restart().await?;
    let reopened = app.get_expect("/api/workflow-notifications", 200).await?;
    ensure!(reopened["unread"] == 20, "unread after read: {reopened}");
    ensure!(reopened["notifications"][0]["read"] == 1, "read flag");
    let order = |page: &Value| -> Vec<Value> {
        page["notifications"]
            .as_array()
            .map(|items| items.iter().map(|item| item["run"].clone()).collect())
            .unwrap_or_default()
    };
    ensure!(order(&reopened) == order(&first), "paging order changed");
    Ok(())
}

#[tokio::test]
async fn workflow_channel_outputs_deliver_once_and_recheck_channel_availability_at_completion(
) -> Result<()> {
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new()).await?;
    let owner = app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Owner", "model": "unused", "instructions": "Test"}),
            201,
        )
        .await?;
    let other = app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Other", "model": "unused", "instructions": "Test"}),
            201,
        )
        .await?;
    let (owner, other) = (id(&owner)?, id(&other)?);
    let mut room = channel(&app, "Results", &owner, &[&owner, &other]).await?;
    let room_id = id(&room)?;
    let room_turns = format!("/api/channels/{room_id}/turns");
    let graph = json!({"entry": "review", "maxSteps": 2, "nodes": [
        {"id": "review", "kind": "approval", "prompt": "Publish?", "yes": "out"},
        {"id": "out", "kind": "output", "channel": room_id}]});
    let workflow = app
        .post_expect(
            &format!("/api/agents/{owner}/workflows"),
            json!({"id": new_id(), "revision": 0, "name": "Publish", "graph": graph}),
            200,
        )
        .await?;
    let workflow = id(&workflow)?;
    let mut foreign = graph.clone();
    foreign["nodes"][1]["channel"] = json!(new_id());
    app.post_expect(
        &format!("/api/agents/{other}/workflows"),
        json!({"id": new_id(), "revision": 0, "name": "Invalid", "graph": foreign}),
        409,
    )
    .await?;
    let mut empty = graph.clone();
    empty["nodes"][1]["channel"] = json!("");
    app.post_expect(
        &format!("/api/agents/{owner}/workflows"),
        json!({"id": new_id(), "revision": 0, "name": "Invalid", "graph": empty}),
        400,
    )
    .await?;
    let launch = json!({"id": new_id(), "revision": 1, "input": "<b>Exact result</b>"});
    let run = app
        .post_expect(
            &format!("/api/workflows/{workflow}/run"),
            launch.clone(),
            200,
        )
        .await?;
    let run_id = id(&run)?;
    let approval = run_until(&app, &run_id, |run| run["status"] == "approval").await?;
    let accept = json!({"id": new_id(), "revision": approval["revision"], "accepted": true});
    let db = Fixture::open(app.database())?;
    db.execute_batch(
        "CREATE TRIGGER fail_notice_fixture BEFORE INSERT ON workflow_notifications \
         BEGIN SELECT RAISE(ABORT,'injected notification failure'); END",
    )?;
    let checks = async {
        app.post_expect(
            &format!("/api/workflow-runs/{run_id}/approve"),
            accept.clone(),
            200,
        )
        .await?;
        let rolled_back = app
            .get_expect(&format!("/api/workflow-runs/{run_id}"), 200)
            .await?;
        ensure!(
            rolled_back["status"] == "ready",
            "rolled back: {rolled_back}"
        );
        ensure!(
            len(&rolled_back["steps"]) == 1,
            "rolled back steps: {rolled_back}"
        );
        ensure!(
            app.get_expect("/api/workflow-notifications", 200).await?["notifications"] == json!([]),
            "notification kept"
        );
        ensure!(
            app.get_expect(&room_turns, 200).await? == json!([]),
            "message kept"
        );
        Ok::<_, minarbot_testkit::Error>(())
    }
    .await;
    db.execute_batch("DROP TRIGGER IF EXISTS fail_notice_fixture")?;
    drop(db);
    checks?;
    run_until(&app, &run_id, |run| run["status"] == "done").await?;
    app.post_expect(&format!("/api/workflow-runs/{run_id}/approve"), accept, 200)
        .await?;
    app.post_expect(
        &format!("/api/workflows/{workflow}/run"),
        launch.clone(),
        200,
    )
    .await?;
    let messages = app.get_expect(&room_turns, 200).await?;
    ensure!(len(&messages) == 1, "messages: {messages}");
    ensure!(messages[0]["reply"] == launch["input"], "reply: {messages}");
    ensure!(
        messages[0]["workflow_run"] == run_id.as_str(),
        "workflow_run: {messages}"
    );
    ensure!(messages[0]["agent"] == owner.as_str(), "agent: {messages}");
    ensure!(
        len(&app
            .get_expect(&format!("/api/agents/{owner}/turns"), 200)
            .await?)
            == 0,
        "output in main history"
    );
    ensure!(
        app.get_expect("/api/channels", 200).await?[0]["activity"] == 1,
        "channel activity"
    );
    app.restart().await?;
    ensure!(
        app.get_expect(&room_turns, 200).await? == messages,
        "messages after restart"
    );
    for mode in ["archive", "remove"] {
        let next = app
            .post_expect(
                &format!("/api/workflows/{workflow}/run"),
                json!({"id": new_id(), "revision": 1, "input": mode}),
                200,
            )
            .await?;
        let next_id = id(&next)?;
        let pending = run_until(&app, &next_id, |run| run["status"] == "approval").await?;
        let members = if mode == "remove" {
            json!([other])
        } else {
            json!([owner, other])
        };
        room = app
            .post_expect(
                &format!("/api/channels/{room_id}/edit"),
                json!({"id": new_id(), "revision": room["revision"], "name": room["name"],
                    "instructions": room["instructions"], "archived": mode == "archive",
                    "members": members, "lead": other}),
                200,
            )
            .await?;
        app.post_expect(
            &format!("/api/workflow-runs/{next_id}/approve"),
            json!({"id": new_id(), "revision": pending["revision"], "accepted": true}),
            200,
        )
        .await?;
        let failed = run_until(&app, &next_id, |run| run["status"] == "failed").await?;
        ensure!(failed["last"] == mode, "{mode} last: {failed}");
        let steps = failed["steps"].as_array().cloned().unwrap_or_default();
        ensure!(
            steps.last().is_some_and(|step| step["status"] == "failed"),
            "{mode} last step: {failed}"
        );
        ensure!(
            failed["error"]
                .as_str()
                .is_some_and(|error| error.contains("Output channel")),
            "{mode} error: {failed}"
        );
        ensure!(
            len(&app.get_expect(&room_turns, 200).await?) == 1,
            "{mode} delivered"
        );
        app.post_expect(
            &format!("/api/workflows/{workflow}/edit"),
            json!({"id": new_id(), "revision": 1, "name": "Publish", "graph": graph}),
            409,
        )
        .await?;
        room = app
            .post_expect(
                &format!("/api/channels/{room_id}/edit"),
                json!({"id": new_id(), "revision": room["revision"], "name": room["name"],
                    "instructions": room["instructions"], "archived": false,
                    "members": [owner, other], "lead": owner}),
                200,
            )
            .await?;
    }
    ensure!(
        len(&app.get_expect("/api/workflow-notifications", 200).await?["notifications"]) == 1,
        "failed outputs notified"
    );
    let snapshot = app
        .post_expect(
            &format!("/api/workflows/{workflow}/run"),
            json!({"id": new_id(), "revision": 1, "input": "Snapshot output"}),
            200,
        )
        .await?;
    let snapshot_id = id(&snapshot)?;
    let snapshot_approval =
        run_until(&app, &snapshot_id, |run| run["status"] == "approval").await?;
    let mut inbox_only = graph.clone();
    inbox_only["nodes"][1] = json!({"id": "out", "kind": "output"});
    app.post_expect(
        &format!("/api/workflows/{workflow}/edit"),
        json!({"id": new_id(), "revision": 1, "name": "Inbox only", "graph": inbox_only}),
        200,
    )
    .await?;
    app.post_expect(
        &format!("/api/workflow-runs/{snapshot_id}/approve"),
        json!({"id": new_id(), "revision": snapshot_approval["revision"], "accepted": true}),
        200,
    )
    .await?;
    run_until(&app, &snapshot_id, |run| run["status"] == "done").await?;
    let snapshot_messages = app.get_expect(&room_turns, 200).await?;
    ensure!(
        len(&snapshot_messages) == 2,
        "snapshot messages: {snapshot_messages}"
    );
    ensure!(
        snapshot_messages[1]["reply"] == "Snapshot output",
        "snapshot reply"
    );
    ensure!(
        app.get_expect(&format!("/api/workflow-runs/{snapshot_id}"), 200)
            .await?["version"]
            == 1,
        "snapshot version"
    );
    Ok(())
}

#[tokio::test]
async fn workflow_run_pages_retain_older_approvals_after_restart_and_validate_cursors() -> Result<()>
{
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new()).await?;
    let agent = id(&api_agent(&app, "Approvals").await?)?;
    let workflow = app
        .post_expect(
            &format!("/api/agents/{agent}/workflows"),
            json!({"id": new_id(), "revision": 0, "name": "Review", "graph": {"entry": "review",
                "maxSteps": 2, "nodes": [{"id": "review", "kind": "approval", "prompt": "Accept?",
                    "yes": "out", "no": "out"}, {"id": "out", "kind": "output"}]}}),
            200,
        )
        .await?;
    let workflow = id(&workflow)?;
    let mut ids = Vec::new();
    for i in 0..22 {
        let run = app
            .post_expect(
                &format!("/api/workflows/{workflow}/run"),
                json!({"id": new_id(), "revision": 1, "input": i.to_string()}),
                200,
            )
            .await?;
        ids.push(json!(id(&run)?));
    }
    app.restart().await?;
    let path = format!("/api/workflows/{workflow}/runs");
    let latest = app.get_expect(&path, 200).await?;
    let order = |rows: &Value| -> Vec<Value> {
        rows.as_array()
            .map(|rows| rows.iter().map(|row| row["id"].clone()).collect())
            .unwrap_or_default()
    };
    ensure!(len(&latest) == 20, "latest page");
    let newest: Vec<Value> = ids[2..].iter().rev().cloned().collect();
    ensure!(order(&latest) == newest, "latest order");
    let last_seq = latest
        .as_array()
        .and_then(|rows| rows.last())
        .map(|row| row["seq"].clone())
        .ok_or("no seq")?;
    let older = app
        .get_expect(&format!("{path}?before={last_seq}"), 200)
        .await?;
    let oldest: Vec<Value> = ids[..2].iter().rev().cloned().collect();
    ensure!(order(&older) == oldest, "older order");
    let oldest_row = older
        .as_array()
        .and_then(|rows| rows.last())
        .cloned()
        .ok_or("no older row")?;
    ensure!(
        app.get_expect(&format!("/api/workflow-runs/{}", id(&oldest_row)?), 200)
            .await?["status"]
            == "approval",
        "oldest approval"
    );
    ensure!(
        len(&app
            .get_expect(&format!("{path}?before={}", oldest_row["seq"]), 200)
            .await?)
            == 0,
        "past the end"
    );
    for query in [
        "before=0",
        "before=-1",
        "before=no",
        "before=1.5",
        "before=9007199254740992",
        "before=",
        "before=1&before=2",
        "other=1",
    ] {
        app.get_expect(&format!("{path}?{query}"), 400).await?;
    }
    Ok(())
}
