//! Goals and loops: judging, pauses, limits, routine occurrences and ACP
//! judge isolation (port of `app/goals.test.ts`).

use minarbot_testkit::{
    fakes::{self, Fake},
    model::{ModelRequest, Reply, ScriptedModel},
    prelude::*,
    providers::{self, AgentSpec},
    Server,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

/// 2026-11-22T10:00:00Z.
const BASE_TIME: i64 = 1_795_341_600_000;

fn answer(content: &str) -> Reply {
    Reply::text(content).usage(20, 10)
}

fn judge(met: bool, reason: &str) -> Reply {
    let evidence = if met { json!(["answer"]) } else { json!([]) };
    answer(&json!({"met": met, "reason": reason, "evidence": evidence}).to_string())
}

fn judging(request: &ModelRequest) -> bool {
    request
        .system()
        .starts_with("Judge whether the target is met.")
}

fn config(model: &ScriptedModel) -> ServerConfig {
    ServerConfig::new()
        .fixed_clock(BASE_TIME)
        .model_with(model, "work", false)
}

async fn goal_agent(app: &Server) -> Result<(String, i64)> {
    let agent = app
        .post(
            "/api/agents",
            json!({"id": new_id(), "name": "Goal fixture", "model": "work",
                "instructions": "Run synthetic tasks.", "runtime": "api"}),
        )
        .await?;
    Ok((
        agent["id"].as_str().ok_or("agent id")?.to_owned(),
        agent["revision"].as_i64().ok_or("agent revision")?,
    ))
}

async fn goal_until(app: &Server, path: &str, done: impl Fn(&Value) -> bool) -> Result<Value> {
    app.wait_get(&format!("{path}/goal"), done).await
}

#[tokio::test]
async fn goal_judges_work_separately_uses_the_configured_model_and_keeps_ordinary_requests_unchanged(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.catalog(["work", "judge"]);
    let judgments = Arc::new(AtomicUsize::new(0));
    let counter = judgments.clone();
    model.respond(move |request| {
        if judging(request) {
            return if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                judge(false, "Include exactly FIXTURE_READY.")
            } else {
                judge(true, "The answer is FIXTURE_READY.")
            };
        }
        answer(if request.last_text().contains("Continue the goal.") {
            "FIXTURE_READY"
        } else {
            "First answer."
        })
    });
    let root = TestRoot::new()?;
    let app = root.start(config(&model)).await?;
    let (id, revision) = goal_agent(&app).await?;
    let path = format!("/api/agents/{id}");
    app.post(
        &format!("{path}/edit"),
        json!({"id": new_id(), "revision": revision, "name": "Goal fixture", "model": "work",
            "instructions": "Run synthetic tasks.", "goalJudgeModel": "judge"}),
    )
    .await?;
    let ordinary = new_id();
    app.post(
        &format!("{path}/turns"),
        json!({"id": ordinary, "text": "Say hello."}),
    )
    .await?;
    app.wait_status(&format!("{path}/turns"), &ordinary, "done")
        .await?;
    let ordinary_calls = model.calls();
    ensure!(
        ordinary_calls == 1,
        "ordinary turn used {ordinary_calls} model calls"
    );
    let tools = model.requests()[0].tool_names();
    ensure!(
        !tools
            .iter()
            .any(|name| name == "set_goal" || name == "set_loop"),
        "ordinary turn offered goal tools: {tools:?}"
    );
    let command = json!({"id": new_id(), "target": "Reply with exactly FIXTURE_READY.", "maxTurns": 3, "maxTokens": 20000});
    app.post(&format!("{path}/goal"), command.clone()).await?;
    let goal = goal_until(&app, &path, |goal| goal["status"] == "met").await?;
    ensure!(goal["turn_count"] == 2, "turn count: {goal}");
    ensure!(goal["max_tokens"] == 20000, "max tokens: {goal}");
    ensure!(
        goal["reason"] == "The answer is FIXTURE_READY.",
        "reason: {goal}"
    );
    ensure!(
        app.post(&format!("{path}/goal"), command.clone()).await?["id"] == command["id"],
        "goal replay"
    );
    let mut changed = command.clone();
    changed["maxTurns"] = json!(2);
    app.post_expect(&format!("{path}/goal"), changed, 409)
        .await?;
    let models: Vec<String> = model.requests()[ordinary_calls..]
        .iter()
        .map(|request| request.model().to_owned())
        .collect();
    ensure!(
        models == ["work", "judge", "work", "judge"],
        "models: {models:?}"
    );
    let inspect = app
        .get(&format!(
            "/api/turns/{}/inspect",
            goal["current_turn"].as_str().ok_or("turn")?
        ))
        .await?;
    let judgments: Vec<Value> = inspect["judgments"]
        .as_array()
        .ok_or("judgments")?
        .iter()
        .map(|row| json!([row["number"], row["met"], row["reason"]]))
        .collect();
    ensure!(
        judgments
            == [
                json!([1, 0, "Include exactly FIXTURE_READY."]),
                json!([2, 1, "The answer is FIXTURE_READY."])
            ],
        "judgments: {judgments:?}"
    );
    Ok(())
}

#[tokio::test]
async fn goal_can_be_set_by_an_agent_pauses_for_you_and_resumes_on_answer() -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|request| {
        if judging(request) {
            judge(true, "The requested answer is present.")
        } else if request.called("ask_user") {
            answer("Blue")
        } else if request.called("set_goal") {
            Reply::tool(
                "ask_user",
                json!({"question": "Which color?", "choices": ["Blue", "Green"]}),
            )
        } else {
            Reply::tool(
                "set_goal",
                json!({"target": "Reply with the selected color."}),
            )
        }
    });
    let root = TestRoot::new()?;
    let mut app = root.start(config(&model)).await?;
    let (id, _) = goal_agent(&app).await?;
    let path = format!("/api/agents/{id}");
    let turn = new_id();
    app.post(
        &format!("{path}/turns"),
        json!({"id": turn, "text": "Set a goal to reply with my selected color."}),
    )
    .await?;
    goal_until(&app, &path, |goal| goal["status"] == "needs_you").await?;
    let row = app
        .row(&format!("{path}/turns"), &turn)
        .await?
        .ok_or("turn missing")?;
    let question = row["questions"][0]["id"]
        .as_str()
        .ok_or("question card missing")?
        .to_owned();
    let judged = |model: &ScriptedModel| model.requests().iter().filter(|r| judging(r)).count();
    ensure!(judged(&model) == 0, "judged before the answer");
    app.restart_with(config(&model).fixed_clock(BASE_TIME + 1000))
        .await?;
    ensure!(
        app.get(&format!("{path}/goal")).await?["status"] == "needs_you",
        "pause lost on restart"
    );
    app.post(
        &format!("{path}/turns/{turn}/questions/{question}/answer"),
        json!({"id": new_id(), "text": "Blue"}),
    )
    .await?;
    goal_until(&app, &path, |goal| goal["status"] == "met").await?;
    ensure!(judged(&model) == 1, "judgments: {}", judged(&model));
    Ok(())
}

/// The token-limit step of the Node case stays in `app/goals.test.ts` until
/// the product bug in `docs/test-ledger.md` (#89, goal allowance) is fixed.
#[tokio::test]
async fn stop_and_restart_fence_prevent_later_goal_work() -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|_| Reply::hold());
    let root = TestRoot::new()?;
    let mut app = root.start(config(&model)).await?;
    let (id, _) = goal_agent(&app).await?;
    let path = format!("/api/agents/{id}");
    let start = |target: &str| json!({"id": new_id(), "target": target});
    let first = app
        .post(&format!("{path}/goal"), start("Wait for stop."))
        .await?;
    model.wait_calls(1).await?;
    let stopped = app
        .post(
            &format!("{path}/goal/stop"),
            json!({"id": new_id(), "goalId": first["id"]}),
        )
        .await?;
    ensure!(stopped["status"] == "stopped", "stop: {stopped}");
    app.post(&format!("{path}/goal"), start("Wait for restart."))
        .await?;
    model.wait_calls(2).await?;
    app.crash().await?;
    app.start_stopped(config(&model)).await?;
    ensure!(
        app.get(&format!("{path}/goal")).await?["status"] == "interrupted",
        "crash fence"
    );
    Ok(())
}

#[tokio::test]
async fn loops_use_routine_occurrences_after_restart_and_stop_on_until_or_max_runs() -> Result<()> {
    let model = ScriptedModel::start().await?;
    let judgments = Arc::new(AtomicUsize::new(0));
    let counter = judgments.clone();
    model.respond(move |request| {
        if judging(request) {
            let count = counter.fetch_add(1, Ordering::SeqCst) + 1;
            judge(
                count == 2,
                if count == 2 {
                    "Condition met."
                } else {
                    "Keep checking."
                },
            )
        } else {
            answer("SYNTHETIC_READY")
        }
    });
    let root = TestRoot::new()?;
    let mut app = root.start(config(&model)).await?;
    let (id, _) = goal_agent(&app).await?;
    let path = format!("/api/agents/{id}");
    let command = json!({"id": new_id(), "prompt": "Check synthetic readiness.", "intervalSeconds": 60,
        "until": "Reply contains SYNTHETIC_READY", "maxRuns": 4});
    app.post_expect(&format!("{path}/loop"), command.clone(), 201)
        .await?;
    app.post_expect(&format!("{path}/loop"), command.clone(), 201)
        .await?;
    let second = json!({"id": new_id(), "prompt": "Repeat a synthetic status.", "intervalSeconds": 60, "maxRuns": 1});
    app.post_expect(&format!("{path}/loop"), second.clone(), 201)
        .await?;
    let first_id = command["id"].as_str().ok_or("id")?;
    let second_id = second["id"].as_str().ok_or("id")?;
    let routine = |id: &str| format!("/api/routines/{id}");
    let runs = |id: &str| format!("/api/routines/{id}/runs");
    ensure!(
        app.get(&routine(first_id)).await?["next_at"] == BASE_TIME + 60_000,
        "next occurrence"
    );
    app.restart_with(config(&model).fixed_clock(BASE_TIME + 60_000))
        .await?;
    for id in [first_id, second_id] {
        app.wait_get(&runs(id), |rows| rows[0]["status"] == "done")
            .await?;
    }
    ensure!(
        app.get(&routine(first_id)).await?["enabled"] == 1,
        "loop stopped before its until condition"
    );
    ensure!(
        app.get(&routine(second_id)).await?["enabled"] == 0,
        "maxRuns 1 loop still enabled"
    );
    app.restart_with(config(&model).fixed_clock(BASE_TIME + 120_000))
        .await?;
    app.wait_get(&routine(first_id), |row| row["enabled"] == 0)
        .await?;
    let count = |rows: Value| rows.as_array().map_or(0, Vec::len);
    ensure!(
        count(app.get(&runs(first_id)).await?) == 2,
        "until loop runs"
    );
    ensure!(
        count(app.get(&runs(second_id)).await?) == 1,
        "maxRuns loop runs"
    );
    ensure!(
        judgments.load(Ordering::SeqCst) == 2,
        "judgments: {}",
        judgments.load(Ordering::SeqCst)
    );
    Ok(())
}

#[tokio::test]
async fn acp_work_and_judge_use_distinct_sessions_with_tools_disabled_for_the_judge() -> Result<()>
{
    let root = TestRoot::new()?;
    let workspace = TempWorkspace::new()?;
    let agent = new_id();
    let app = root
        .start(
            ServerConfig::new()
                .option("defaultModel", "work")
                .workspace(&agent, workspace.path(), false),
        )
        .await?;
    providers::create_instance(
        &app,
        "opencode-fixture",
        "opencode",
        fakes::path(Fake::Acp)?,
    )
    .await?;
    AgentSpec::new("Goal fixture")
        .field("id", agent.as_str())
        .field("model", "work")
        .field("instructions", "Run synthetic tasks.")
        .instance("opencode-fixture")
        .create(&app)
        .await?;
    let path = format!("/api/agents/{agent}");
    let turn = new_id();
    app.post(
        &format!("{path}/goal"),
        json!({"id": turn, "target": "Reply with ACP_READY."}),
    )
    .await?;
    let goal = goal_until(&app, &path, |goal| {
        !["active", "judging"].contains(&goal["status"].as_str().unwrap_or_default())
    })
    .await?;
    ensure!(goal["status"] == "met", "goal: {goal}");
    let row = app
        .row(&format!("{path}/turns"), &turn)
        .await?
        .ok_or("turn missing")?;
    ensure!(row["reply"] == "ACP_READY", "work reply: {row}");
    // Only fake-acp's judge mode returns this verdict, and that mode fails
    // the peer when it receives MCP servers or the work session.
    ensure!(goal["reason"] == "Answer matches.", "judge verdict: {goal}");
    let log = fakes::acp_log(workspace.path())?;
    ensure!(
        log.iter().all(|row| row["method"] != "session/prompt"
            || row["params"]["sessionId"] == "fixture-session"),
        "judge prompt reached the work session: {log:?}"
    );
    Ok(())
}
