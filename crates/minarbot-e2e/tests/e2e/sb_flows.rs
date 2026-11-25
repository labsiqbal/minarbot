//! Routine flow graphs: validation and fan-in execution (port of
//! `app/flows.test.ts`).

use minarbot_testkit::{
    control::{Answer, ControlModel, Deferred},
    prelude::*,
    providers::AgentSpec,
    Server,
};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering::SeqCst},
        Arc,
    },
};

fn id_of(value: &Value) -> Result<String> {
    Ok(value["id"].as_str().ok_or("id")?.to_owned())
}

/// Routine owned by `owner`; returns `(routine id, revision)`.
async fn routine(app: &Server, owner: &str) -> Result<(String, Value)> {
    let routine = app
        .post(
            &format!("/api/agents/{owner}/routines"),
            json!({"id": new_id(), "name": "Fan-in", "prompt": "Root job",
                "nextAt": minarbot_testkit::FIXED_CLOCK + 3_600_000,
                "intervalSeconds": null, "enabled": false}),
        )
        .await?;
    Ok((id_of(&routine)?, routine["revision"].clone()))
}

/// The Node case called `validateFlow` from `app/src/flows.ts`; the Rust
/// server validates the same graph in `POST /api/routines/{id}/flow`.
#[tokio::test]
async fn routine_flow_graph_rejects_cycles_foreign_agents_and_disconnected_branches() -> Result<()>
{
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let mut agents = Vec::new();
    for name in ["a", "b", "c", "d"] {
        agents.push(id_of(&AgentSpec::new(name).create(&app).await?)?);
    }
    let [a, b, c, d] = [&agents[0], &agents[1], &agents[2], &agents[3]];
    let (routine, _) = routine(&app, a).await?;
    let flow = format!("/api/routines/{routine}/flow");
    let mut revision = 0;
    let empty = app
        .post(
            &flow,
            json!({"id": new_id(), "revision": revision, "links": []}),
        )
        .await?;
    ensure!(
        empty["graph"] == json!({"root": a, "nodes": [a], "links": []}),
        "empty graph: {empty}"
    );
    revision += 1;
    let diamond = json!([{"from": a, "to": b}, {"from": a, "to": c}, {"from": b, "to": d}, {"from": c, "to": d}]);
    let graph = app
        .post(
            &flow,
            json!({"id": new_id(), "revision": revision, "links": diamond}),
        )
        .await?;
    let mut middle = [b.clone(), c.clone()];
    middle.sort();
    ensure!(
        graph["graph"]["nodes"] == json!([a, middle[0], middle[1], d]),
        "diamond nodes: {graph}"
    );
    revision += 1;
    let foreign = new_id();
    let invalid = [
        json!(null),
        json!([{"from": a, "to": foreign}]),
        json!([{"from": a, "to": a}]),
        json!([{"from": b, "to": a}]),
        json!([{"from": a, "to": b}, {"from": a, "to": b}]),
        json!([{"from": c, "to": d}]),
        json!([{"from": a, "to": b}, {"from": b, "to": c}, {"from": c, "to": b}]),
        json!([{"from": a, "to": b, "unexpected": true}]),
    ];
    for links in invalid {
        app.post_expect(
            &flow,
            json!({"id": new_id(), "revision": revision, "links": links}),
            400,
        )
        .await?;
    }
    let mut many = Vec::new();
    for index in 1..33 {
        many.push(id_of(
            &AgentSpec::new(&format!("n{index}")).create(&app).await?,
        )?);
    }
    let links: Vec<Value> = many.iter().map(|to| json!({"from": a, "to": to})).collect();
    let rejected = app
        .post_expect(
            &flow,
            json!({"id": new_id(), "revision": revision, "links": links}),
            400,
        )
        .await?;
    ensure!(
        rejected["error"]
            .as_str()
            .is_some_and(|error| error.contains("32")),
        "33 agents: {rejected}"
    );
    let stored = app.get(&format!("/api/routines/{routine}/flow")).await?;
    ensure!(
        stored["revision"] == revision,
        "rejected graphs saved: {stored}"
    );
    Ok(())
}

/// Run the routine once through `/test`; returns the root turn.
async fn start(app: &Server, routine: &str, revision: &Value) -> Result<String> {
    let run = app
        .post(
            &format!("/api/routines/{routine}/test"),
            json!({"id": new_id(), "revision": revision}),
        )
        .await?;
    Ok(run["turn"].as_str().ok_or("turn")?.to_owned())
}

#[derive(Debug, PartialEq)]
struct Input {
    agent: String,
    status: String,
    text: String,
}

/// Flow inputs embedded as a JSON line in the step prompt.
fn inputs(model: &ControlModel, name: &str) -> Result<Vec<Input>> {
    let request = model
        .requests()
        .into_iter()
        .find(|request| request.model() == name)
        .ok_or_else(|| format!("no request for {name}"))?;
    let prompt = request.last_user_text();
    let line = prompt
        .lines()
        .find(|line| line.starts_with("[{"))
        .ok_or_else(|| format!("no input line for {name}: {prompt}"))?;
    let rows: Vec<Value> = serde_json::from_str(line)?;
    Ok(rows
        .iter()
        .map(|row| Input {
            agent: row["agent"].as_str().unwrap_or_default().into(),
            status: row["status"].as_str().unwrap_or_default().into(),
            text: row["text"].as_str().unwrap_or_default().into(),
        })
        .collect())
}

fn step<'a>(steps: &'a Value, agent: &str) -> Option<&'a Value> {
    steps
        .as_array()
        .and_then(|steps| steps.iter().find(|step| step["agent"] == agent))
}

async fn steps(
    app: &Server,
    path: &str,
    root: &str,
    ready: impl Fn(&Value) -> bool,
) -> Result<Value> {
    until_value(
        format!("flow run {root}"),
        || async {
            let runs = app.get(path).await?;
            Ok(runs
                .as_array()
                .and_then(|runs| runs.iter().find(|run| run["root_turn"] == root))
                .map(|run| run["steps"].clone())
                .unwrap_or(Value::Null))
        },
        |steps| !steps.is_null() && ready(steps),
    )
    .await
}

#[tokio::test]
async fn routine_flow_fan_in_waits_for_every_input_and_settles_all_failed_branches() -> Result<()> {
    let model = ControlModel::start().await?;
    let fail_both = Arc::new(AtomicBool::new(false));
    let gates = [AtomicBool::new(true), AtomicBool::new(true)];
    let flag = fail_both.clone();
    model.respond(move |request| {
        let name = request.model().to_owned();
        let answered = request
            .messages()
            .last()
            .is_some_and(|message| message["role"] == "tool");
        if answered {
            return Answer::text(format!("{name} reported"));
        }
        let fails = name == "c" || (name == "b" && flag.load(SeqCst));
        // The first root step and the first `c` step wait for the test.
        let gate = match name.as_str() {
            "a" => Some(&gates[0]),
            "c" => Some(&gates[1]),
            _ => None,
        };
        if gate.is_some_and(|gate| gate.swap(false, SeqCst)) {
            return Answer::Defer;
        }
        if fails {
            return Answer::tool(
                "report_failure",
                json!({"reason": format!("{name} failed")}),
            );
        }
        Answer::text(format!("{name} result"))
    });
    let root = TestRoot::new()?;
    let mut app = root
        .start(ServerConfig::new().gateway(model.gateway()))
        .await?;
    let mut agents = Vec::new();
    for name in ["a", "b", "c", "d"] {
        agents.push(id_of(
            &AgentSpec::new(name)
                .runtime("api")
                .field("model", name)
                .field("instructions", "Run a flow step.")
                .create(&app)
                .await?,
        )?);
    }
    let [a, b, c, d] = [&agents[0], &agents[1], &agents[2], &agents[3]];
    let (routine, revision) = routine(&app, a).await?;
    app.post(
        &format!("/api/routines/{routine}/flow"),
        json!({"id": new_id(), "revision": 0, "links": [{"from": a, "to": b}, {"from": a, "to": c}, {"from": b, "to": d}, {"from": c, "to": d}]}),
    )
    .await?;
    let path = format!("/api/routines/{routine}/flow-runs");
    let first = start(&app, &routine, &revision).await?;
    let root_step: Deferred = model.deferred().await?;
    ensure!(
        root_step.request.model() == "a",
        "first step: {}",
        root_step.request.model()
    );
    let initial = steps(&app, &path, &first, |steps| {
        step(steps, a).is_some_and(|step| step["task_status"] == "running")
    })
    .await?;
    let downstream: Vec<&Value> = initial
        .as_array()
        .ok_or("steps")?
        .iter()
        .filter(|step| step["agent"] != a.as_str())
        .collect();
    let mut branch = vec![b.clone(), c.clone()];
    branch.sort();
    branch.push(d.clone());
    ensure!(
        downstream
            .iter()
            .map(|step| step["agent"].clone())
            .collect::<Vec<_>>()
            == branch.iter().map(|id| json!(id)).collect::<Vec<_>>(),
        "downstream order: {initial}"
    );
    ensure!(
        downstream
            .iter()
            .all(|step| step["status"] == "waiting" && step["thread"].is_null()),
        "downstream not waiting: {initial}"
    );
    ensure!(
        model
            .requests()
            .iter()
            .all(|request| request.model() == "a"),
        "a branch started before the root finished"
    );
    root_step.answer(Answer::text("a result"));
    let branch_c: Deferred = model.deferred().await?;
    ensure!(
        branch_c.request.model() == "c",
        "held branch: {}",
        branch_c.request.model()
    );
    let active = steps(&app, &path, &first, |steps| {
        step(steps, b).is_some_and(|step| step["status"] == "done")
            && step(steps, c).is_some_and(|step| step["task_status"] == "running")
    })
    .await?;
    for name in ["b", "c"] {
        ensure!(
            inputs(&model, name)?
                == vec![Input {
                    agent: a.clone(),
                    status: "done".into(),
                    text: "a result".into()
                }],
            "{name} inputs"
        );
    }
    ensure!(
        step(&active, d).is_some_and(|step| step["status"] == "waiting"),
        "d did not wait: {active}"
    );
    ensure!(
        !model
            .requests()
            .iter()
            .any(|request| request.model() == "d"),
        "d ran before every input settled"
    );
    branch_c.answer(Answer::tool(
        "report_failure",
        json!({"reason": "c failed"}),
    ));
    steps(&app, &path, &first, |steps| {
        step(steps, d).is_some_and(|step| step["status"] == "done")
    })
    .await?;
    ensure!(
        inputs(&model, "d")?
            == vec![
                Input {
                    agent: b.clone(),
                    status: "done".into(),
                    text: "b result".into()
                },
                Input {
                    agent: c.clone(),
                    status: "failed".into(),
                    text: "c failed".into()
                },
            ],
        "d inputs"
    );

    fail_both.store(true, SeqCst);
    let second = start(&app, &routine, &revision).await?;
    let skipped = steps(&app, &path, &second, |steps| {
        step(steps, d).is_some_and(|step| step["status"] == "skipped")
    })
    .await?;
    let statuses: BTreeMap<String, Value> = skipped
        .as_array()
        .ok_or("steps")?
        .iter()
        .map(|step| {
            (
                step["agent"].as_str().unwrap_or_default().to_owned(),
                step["status"].clone(),
            )
        })
        .collect();
    let expected: BTreeMap<String, Value> = [
        (a.clone(), json!("done")),
        (b.clone(), json!("failed")),
        (c.clone(), json!("failed")),
        (d.clone(), json!("skipped")),
    ]
    .into();
    ensure!(statuses == expected, "second run: {skipped}");
    let skipped_d = step(&skipped, d).ok_or("d")?;
    ensure!(
        skipped_d["result"]
            .as_str()
            .is_some_and(|result| result.contains("No successful input")),
        "skip reason: {skipped_d}"
    );
    ensure!(skipped_d["thread"].is_null(), "skipped step has a thread");
    app.restart().await?;
    ensure!(
        steps(&app, &path, &second, |_| true).await? == skipped,
        "restart changed the settled run"
    );
    let mut models: Vec<String> = model
        .requests()
        .iter()
        .filter(|request| {
            !request
                .messages()
                .last()
                .is_some_and(|message| message["role"] == "tool")
        })
        .map(|request| request.model().to_owned())
        .collect();
    models.sort();
    ensure!(
        models == ["a", "a", "b", "b", "c", "c", "d"],
        "step models: {models:?}"
    );
    Ok(())
}
