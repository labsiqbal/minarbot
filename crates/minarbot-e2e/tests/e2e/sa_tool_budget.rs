//! Per-task tool budgets on provider instances (port of
//! `app/tool-budget.test.ts`). The gateway peer enforces the budget the
//! server passes (`MINARBOT_TOOL_BUDGET`, G8 in `docs/testing.md`).

use crate::sa_support::{lock, Shared};
use minarbot_testkit::{
    base64::{engine::general_purpose::STANDARD, Engine},
    bridge::{Bridge, BridgeError},
    model::{ModelRequest, Reply, ScriptedModel},
    prelude::*,
    Server,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

/// Tool results of the turn. Screenshots come back as an extra user message
/// after their tool result, so this counts every tool message (earlier turns
/// reach the harness as text only).
fn reads(request: &ModelRequest) -> usize {
    request.tool_results().len()
}

fn offered(request: &ModelRequest) -> bool {
    !request.tool_names().is_empty()
}

fn images(request: &ModelRequest) -> Vec<String> {
    request
        .messages()
        .iter()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter(|part| part["type"] == "image_url")
        .filter_map(|part| part["image_url"]["url"].as_str().map(str::to_owned))
        .collect()
}

/// Server with a scripted model, the fake desktop runtime and a budgeted
/// agent (`toolBudget: 10`), like the Node `fixture`.
struct Budget {
    model: ScriptedModel,
    app: Server,
    agent: Value,
    captures: Arc<AtomicUsize>,
    _bridge: Bridge,
    _root: TestRoot,
}

async fn fixture(
    answer: impl Fn(&ModelRequest) -> Reply + Send + Sync + 'static,
) -> Result<Budget> {
    let model = ScriptedModel::start().await?;
    model.respond(answer);
    let captures = Arc::new(AtomicUsize::new(0));
    let count = captures.clone();
    let bridge = Bridge::operations(move |operation, _| match operation {
        "desktop-launch" | "desktop-stop" => Ok(Value::Null),
        "desktop-status" => Ok(json!(false)),
        "desktop-run" | "desktop-click" | "desktop-type" | "desktop-key" | "desktop-scroll"
        | "desktop-move" => Ok(json!("")),
        "desktop-capture" | "desktop-screenshot" => {
            let n = count.fetch_add(1, Ordering::SeqCst) + 1;
            let color = [
                (n * 47 % 256) as u8,
                (n * 83 % 256) as u8,
                (n * 131 % 256) as u8,
                255,
            ];
            let png = minarbot_testkit::image::png(1280, 800, color)
                .map_err(|e| BridgeError::new(500, e.to_string()))?;
            Ok(json!(STANDARD.encode(png)))
        }
        "clock" => Ok(Value::Null),
        other => Err(BridgeError::new(404, format!("unexpected {other}"))),
    })
    .await?;
    let root = TestRoot::new()?;
    // Short directories: the desktop proxy socket path must fit sun_path.
    let data = root.dir("d")?;
    let leases = root.dir("l")?;
    let config = ServerConfig::new()
        .model(&model)
        .computer_bridge(&bridge)
        .option(
            "desktop",
            json!({"dataDir": data, "leaseDir": leases, "enabled": true, "testRuntime": true}),
        );
    let app = root.start(config).await?;
    let agent = app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Budget", "model": "fixture", "instructions": "Help.", "toolBudget": 10}),
        )
        .await?;
    Ok(Budget {
        model,
        app,
        agent,
        captures,
        _bridge: bridge,
        _root: root,
    })
}

impl Budget {
    fn id(&self) -> String {
        self.agent["id"].as_str().unwrap_or_default().to_owned()
    }

    async fn turn(&self, path: &str, id: &str) -> Result<Value> {
        self.app
            .wait_row(path, id, |row| {
                row["status"] == "done" || row["status"] == "failed"
            })
            .await
    }
}

fn read_history(request: &ModelRequest) -> Reply {
    Reply::tool_call(
        &format!("read-{}", reads(request)),
        "read_history",
        json!({}),
    )
}

#[tokio::test]
async fn nine_distinct_tools_complete_before_per_task_budget_requests_a_final_answer_without_tools(
) -> Result<()> {
    let f = fixture(|request| {
        if offered(request) {
            read_history(request)
        } else {
            Reply::text("Checked history. More work remains. Reply \"continue\".")
        }
    })
    .await?;
    let path = format!("/api/agents/{}/turns", f.id());
    let id = new_id();
    f.app
        .post(
            &path,
            json!({"id": id, "text": "Perform more than nine steps."}),
        )
        .await?;
    let turn = f.turn(&path, &id).await?;
    ensure!(turn["status"] == "done", "turn: {}", turn["error"]);
    let requests = f.model.requests();
    ensure!(requests.len() == 11, "requests: {}", requests.len());
    let last = requests.last().ok_or("no request")?;
    ensure!(
        !offered(last),
        "final request offered tools: {:?}",
        last.tool_names()
    );
    ensure!(
        turn["activity"]["budget_stopped"] == 10,
        "budget: {}",
        turn["activity"]["budget_stopped"]
    );
    ensure!(
        turn["reply"]
            .as_str()
            .is_some_and(|r| r.contains("continue")),
        "reply: {}",
        turn["reply"]
    );
    let messages = last.body["messages"].to_string().to_lowercase();
    ensure!(
        messages.contains("remains") || (messages.contains("what") && messages.contains("left")),
        "final request lacks the summary instruction"
    );
    Ok(())
}

#[tokio::test]
async fn budget_setting_rejects_malformed_values_survives_restart_and_template_recovery(
) -> Result<()> {
    let mut f = fixture(|_| Reply::text("Done.")).await?;
    let profile = json!({"name": "Budget", "model": "fixture", "instructions": "Help."});
    let with = |fields: Value| {
        let mut value = profile.clone();
        if let (Some(value), Some(fields)) = (value.as_object_mut(), fields.as_object()) {
            value.extend(fields.clone());
        }
        value
    };
    for budget in [
        Value::Null,
        json!(true),
        json!("90"),
        json!(9),
        json!(201),
        json!(10.5),
    ] {
        f.app
            .post_expect(
                "/api/agents",
                with(json!({"runtime": "api", "id": new_id(), "toolBudget": budget})),
                400,
            )
            .await?;
        f.app
            .post_expect(
                &format!("/api/agents/{}/edit", f.id()),
                with(json!({"id": new_id(), "revision": 1, "toolBudget": budget})),
                400,
            )
            .await?;
    }
    let create = with(json!({"id": new_id(), "toolBudget": 200, "runtime": "api"}));
    let agent = f.app.post("/api/agents", create.clone()).await?;
    ensure!(agent["tool_budget"] == 200, "created: {agent}");
    ensure!(
        f.app.post("/api/agents", create.clone()).await? == agent,
        "create replay"
    );
    let mut changed = create.clone();
    changed["toolBudget"] = json!(199);
    f.app.post_expect("/api/agents", changed, 409).await?;
    let agent_id = agent["id"].as_str().unwrap_or_default().to_owned();
    let edit = with(json!({"id": new_id(), "revision": 1, "toolBudget": 15}));
    let edited = f
        .app
        .post(&format!("/api/agents/{agent_id}/edit"), edit.clone())
        .await?;
    ensure!(edited["tool_budget"] == 15, "edited: {edited}");
    let mut other = edit.clone();
    other["id"] = json!(new_id());
    f.app
        .post_expect(&format!("/api/agents/{agent_id}/edit"), other, 409)
        .await?;
    let exported = f
        .app
        .get(&format!("/api/agents/{agent_id}/template"))
        .await?;
    ensure!(exported["toolBudget"] == 15, "template: {exported}");
    // Server-side form of `templateCommand(exported, id)`.
    let mut import = json!({"id": new_id(), "runtime": "api", "name": exported["name"], "role": exported["role"].as_str().unwrap_or(""),
        "model": exported["model"], "instructions": exported["instructions"], "threadLimit": exported["threadLimit"],
        "toolBudget": exported["toolBudget"]});
    if !exported["avatar"].is_null() {
        import["avatar"] = exported["avatar"].clone();
    }
    let imported = f.app.post("/api/agents", import).await?;
    ensure!(imported["tool_budget"] == 15, "imported: {imported}");
    f.app.restart().await?;
    ensure!(
        f.app
            .post(&format!("/api/agents/{agent_id}/edit"), edit)
            .await?
            == edited,
        "edit receipt"
    );
    let agents = f.app.get("/api/agents").await?;
    let listed = agents["agents"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["id"] == agent_id.as_str())
                .cloned()
        })
        .ok_or("agent missing")?;
    ensure!(listed["tool_budget"] == 15, "listed: {listed}");
    Ok(())
}

async fn enable_computer(f: &Budget, agent: &str) -> Result<()> {
    f.app
        .post(
            &format!("/api/agents/{agent}/computer"),
            json!({"id": new_id(), "revision": 0, "enabled": true}),
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn resume_gets_fresh_budget_without_replay_desktop_images_retain_latest_three_across_restart(
) -> Result<()> {
    let summaries = Arc::new(AtomicUsize::new(0));
    let count = summaries.clone();
    let mut f = fixture(move |request| {
        if !offered(request) {
            return if count.fetch_add(1, Ordering::SeqCst) == 0 {
                Reply::status(503, json!({}))
            } else {
                Reply::text("Captured more. Reply \"continue\".")
            };
        }
        Reply::tool_call(
            &format!("shot-{}", reads(request)),
            "computer_screenshot",
            json!({}),
        )
    })
    .await?;
    enable_computer(&f, &f.id()).await?;
    let path = format!("/api/agents/{}/turns", f.id());
    let id = new_id();
    f.app
        .post(&path, json!({"id": id, "text": "Inspect desktop."}))
        .await?;
    let failed = f.turn(&path, &id).await?;
    ensure!(failed["status"] == "failed", "first attempt: {failed}");
    ensure!(
        failed["error"] == "Stopped after 10 tool steps. Resume to continue.",
        "error: {}",
        failed["error"]
    );
    ensure!(
        f.captures.load(Ordering::SeqCst) == 10,
        "captures: {}",
        f.captures.load(Ordering::SeqCst)
    );
    ensure!(
        !offered(f.model.requests().last().ok_or("no request")?),
        "summary offered tools"
    );
    f.app.restart().await?;
    let before = f.model.calls();
    let resume =
        json!({"id": new_id(), "revision": failed["revision"], "acknowledgeUncertain": true});
    f.app
        .post(&format!("{path}/{id}/resume"), resume.clone())
        .await?;
    f.app.post(&format!("{path}/{id}/resume"), resume).await?;
    let done = f.turn(&path, &id).await?;
    ensure!(done["status"] == "done", "resumed: {}", done["error"]);
    ensure!(
        f.captures.load(Ordering::SeqCst) == 20,
        "captures: {}",
        f.captures.load(Ordering::SeqCst)
    );
    let requests = f.model.requests();
    ensure!(
        requests.len() - before == 11,
        "resumed requests: {}",
        requests.len() - before
    );
    let tools = done["activity"]["tools"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    ensure!(tools.len() == 20, "tools: {}", tools.len());
    let mut ids: Vec<String> = tools.iter().map(|t| t["id"].to_string()).collect();
    ids.sort();
    ids.dedup();
    ensure!(ids.len() == 20, "distinct tool ids: {}", ids.len());
    ensure!(
        done["activity"]["budget_stopped"] == 10,
        "budget: {}",
        done["activity"]["budget_stopped"]
    );
    // Every screenshot reached the model as a distinct image.
    let mut distinct: Vec<String> = requests.iter().flat_map(images).collect();
    distinct.sort();
    distinct.dedup();
    ensure!(
        distinct.len() == 20,
        "distinct screenshots: {}",
        distinct.len()
    );
    Ok(())
}

#[tokio::test]
async fn legacy_channel_transfers_share_attempt_budget_beyond_eight_coordination_steps(
) -> Result<()> {
    let next: Shared<String> = Shared::default();
    let recipient = next.clone();
    let f = fixture(move |request| {
        // Like B1: after a transfer returns null the harness may take one
        // more step before the server stops it; never answer that step.
        if request
            .tool_results()
            .last()
            .is_some_and(|m| m["content"] == "null")
        {
            return Reply::hold();
        }
        if !offered(request) {
            return Reply::text("Transferred work. Reply \"continue\".");
        }
        let to = request.body["tools"]
            .as_array()
            .and_then(|tools| {
                tools
                    .iter()
                    .find(|t| t["function"]["name"] == "transfer_task")
            })
            .and_then(|t| {
                t["function"]["parameters"]["properties"]["recipient"]["enum"][0]
                    .as_str()
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| recipient.lock().map(|r| r.clone()).unwrap_or_default());
        Reply::tool_call(
            &format!("transfer-{}", reads(request)),
            "transfer_task",
            json!({"recipient": to, "task": "Continue work."}),
        )
    })
    .await?;
    let other = f
        .app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Next", "model": "fixture", "instructions": "Help.", "toolBudget": 10}),
        )
        .await?;
    *lock(&next)? = other["id"].as_str().unwrap_or_default().to_owned();
    let room = f
        .app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Transfers", "instructions": "Help.", "lead": f.id(), "members": [f.id(), other["id"]]}),
        )
        .await?;
    let path = format!(
        "/api/channels/{}/turns",
        room["id"].as_str().unwrap_or_default()
    );
    let id = new_id();
    f.app
        .post(
            &path,
            json!({"id": id, "text": "Transfer work.", "recipient": f.id()}),
        )
        .await?;
    let done = f.turn(&path, &id).await?;
    ensure!(done["status"] == "done", "transfers: {}", done["error"]);
    ensure!(
        done["coordination"].as_array().map(Vec::len) == Some(10),
        "coordination: {}",
        done["coordination"]
    );
    ensure!(
        done["activity"]["budget_stopped"] == 10,
        "budget: {}",
        done["activity"]["budget_stopped"]
    );
    let work = f
        .model
        .requests()
        .iter()
        .filter(|r| {
            !r.tool_results()
                .last()
                .is_some_and(|m| m["content"] == "null")
        })
        .count();
    ensure!(work == 11, "requests: {work}");
    ensure!(
        !offered(f.model.requests().last().ok_or("no request")?),
        "summary offered tools"
    );
    Ok(())
}

async fn shared_budget(kind: &str) -> Result<()> {
    let f = fixture(|request| {
        if offered(request) {
            read_history(request)
        } else {
            Reply::text("Checked. More remains. Reply \"continue\".")
        }
    })
    .await?;
    let (path, id) = if kind == "workflow" {
        let graph = json!({"entry": "work", "maxSteps": 3, "nodes": [
            {"id": "work", "kind": "agent", "agent": f.id(), "prompt": "Read.", "next": "out"},
            {"id": "out", "kind": "output"}]});
        let workflow = f
            .app
            .post(
                &format!("/api/agents/{}/workflows", f.id()),
                json!({"id": new_id(), "revision": 0, "name": "Budget", "graph": graph}),
            )
            .await?;
        let run = f
            .app
            .post(
                &format!(
                    "/api/workflows/{}/run",
                    workflow["id"].as_str().unwrap_or_default()
                ),
                json!({"id": new_id(), "revision": 1, "input": "Read."}),
            )
            .await?;
        let result = f
            .app
            .wait_get(
                &format!(
                    "/api/workflow-runs/{}",
                    run["id"].as_str().unwrap_or_default()
                ),
                |run| run["status"] == "done" || run["status"] == "failed",
            )
            .await?;
        ensure!(result["status"] == "done", "workflow: {result}");
        let step = result["steps"]
            .as_array()
            .and_then(|steps| steps.iter().find(|step| step["turn"].is_string()).cloned())
            .ok_or("agent step missing")?;
        (
            format!(
                "/api/threads/{}/turns",
                step["thread"].as_str().unwrap_or_default()
            ),
            step["turn"].as_str().unwrap_or_default().to_owned(),
        )
    } else {
        let routine = f
            .app
            .post(
                &format!("/api/agents/{}/routines", f.id()),
                json!({"id": new_id(), "name": "Budget", "prompt": "Read.", "nextAt": minarbot_testkit::FIXED_CLOCK + 3_600_000,
                    "intervalSeconds": 60, "enabled": false}),
            )
            .await?;
        let routine_id = routine["id"].as_str().unwrap_or_default().to_owned();
        if kind == "routine flow" {
            f.app
                .post(
                    &format!("/api/routines/{routine_id}/flow"),
                    json!({"id": new_id(), "revision": 0, "links": []}),
                )
                .await?;
        }
        let run = f
            .app
            .post(
                &format!("/api/routines/{routine_id}/test"),
                json!({"id": new_id(), "revision": routine["revision"]}),
            )
            .await?;
        let result = f
            .app
            .wait_get(&format!("/api/routines/{routine_id}/runs"), |rows| {
                rows[0]["status"] == "done" || rows[0]["status"] == "failed"
            })
            .await?;
        ensure!(result[0]["status"] == "done", "routine: {result}");
        (
            format!(
                "/api/threads/{}/turns",
                run["thread"].as_str().unwrap_or_default()
            ),
            run["turn"].as_str().unwrap_or_default().to_owned(),
        )
    };
    let row = f.turn(&path, &id).await?;
    ensure!(row["status"] == "done", "{kind} turn: {}", row["error"]);
    ensure!(
        row["activity"]["budget_stopped"] == 10,
        "{kind} budget: {}",
        row["activity"]["budget_stopped"]
    );
    ensure!(
        f.model.calls() == 11,
        "{kind} requests: {}",
        f.model.calls()
    );
    ensure!(
        !offered(f.model.requests().last().ok_or("no request")?),
        "{kind} summary offered tools"
    );
    Ok(())
}

#[tokio::test]
async fn workflow_uses_shared_per_task_budget_and_final_summary() -> Result<()> {
    shared_budget("workflow").await
}

#[tokio::test]
async fn routine_uses_shared_per_task_budget_and_final_summary() -> Result<()> {
    shared_budget("routine").await
}

#[tokio::test]
async fn routine_flow_uses_shared_per_task_budget_and_final_summary() -> Result<()> {
    shared_budget("routine flow").await
}

async fn invalid_final_summary(tool: bool) -> Result<()> {
    let f = fixture(move |request| {
        if offered(request) {
            read_history(request)
        } else if tool {
            Reply::tool_call("forbidden-summary-tool", "read_history", json!({}))
        } else {
            Reply::text("")
        }
    })
    .await?;
    let path = format!("/api/agents/{}/turns", f.id());
    let id = new_id();
    f.app
        .post(&path, json!({"id": id, "text": "Read."}))
        .await?;
    let row = f.turn(&path, &id).await?;
    ensure!(row["status"] == "failed", "summary turn: {row}");
    ensure!(
        row["error"] == "Stopped after 10 tool steps. Resume to continue.",
        "error: {}",
        row["error"]
    );
    ensure!(f.model.calls() == 11, "requests: {}", f.model.calls());
    ensure!(
        !offered(f.model.requests().last().ok_or("no request")?),
        "summary offered tools"
    );
    ensure!(
        row["activity"]["budget_stopped"] == 10,
        "budget: {}",
        row["activity"]["budget_stopped"]
    );
    Ok(())
}

#[tokio::test]
async fn invalid_final_summary_fails_clearly_and_never_executes_tools_string() -> Result<()> {
    invalid_final_summary(false).await
}

#[tokio::test]
async fn invalid_final_summary_fails_clearly_and_never_executes_tools_object() -> Result<()> {
    invalid_final_summary(true).await
}

#[tokio::test]
async fn reassign_gets_fresh_budget_while_retaining_recorded_computer_calls() -> Result<()> {
    let summaries = Arc::new(AtomicUsize::new(0));
    let count = summaries.clone();
    let f = fixture(move |request| {
        if !offered(request) {
            return if count.fetch_add(1, Ordering::SeqCst) == 0 {
                Reply::status(503, json!({}))
            } else {
                Reply::text("Checked. Reply \"continue\".")
            };
        }
        Reply::tool_call(
            &format!("shot-{}", reads(request)),
            "computer_screenshot",
            json!({}),
        )
    })
    .await?;
    let next = f
        .app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Next", "model": "fixture", "instructions": "Help.", "toolBudget": 10}),
        )
        .await?;
    let next_id = next["id"].as_str().unwrap_or_default().to_owned();
    for agent in [f.id(), next_id.clone()] {
        enable_computer(&f, &agent).await?;
    }
    let room = f
        .app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Budget room", "instructions": "Help.", "lead": f.id(), "members": [f.id(), next_id]}),
        )
        .await?;
    let path = format!(
        "/api/channels/{}/turns",
        room["id"].as_str().unwrap_or_default()
    );
    let id = new_id();
    f.app
        .post(
            &path,
            json!({"id": id, "text": "Inspect.", "recipient": f.id()}),
        )
        .await?;
    let failed = f.turn(&path, &id).await?;
    ensure!(
        failed["error"] == "Stopped after 10 tool steps. Resume to continue.",
        "error: {}",
        failed["error"]
    );
    ensure!(
        f.captures.load(Ordering::SeqCst) == 10,
        "captures: {}",
        f.captures.load(Ordering::SeqCst)
    );
    let command = json!({"id": new_id(), "revision": failed["revision"], "recipient": next_id, "acknowledgeUncertain": true});
    f.app
        .post(&format!("{path}/{id}/reassign"), command.clone())
        .await?;
    f.app
        .post(&format!("{path}/{id}/reassign"), command)
        .await?;
    let done = f.turn(&path, &id).await?;
    ensure!(done["status"] == "done", "reassigned: {}", done["error"]);
    ensure!(
        done["agent"] == next_id.as_str(),
        "agent: {}",
        done["agent"]
    );
    ensure!(
        f.captures.load(Ordering::SeqCst) == 20,
        "captures: {}",
        f.captures.load(Ordering::SeqCst)
    );
    ensure!(
        done["activity"]["tools"].as_array().map(Vec::len) == Some(20),
        "tools"
    );
    Ok(())
}

#[tokio::test]
async fn automatic_question_continuation_retains_attempt_budget_across_restart() -> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut f = fixture(move |request| {
        if crate::support::after_question(request) {
            return crate::support::after_question_reply();
        }
        if !offered(request) {
            return Reply::text("Progress saved. Reply \"continue\".");
        }
        let n = count.fetch_add(1, Ordering::SeqCst) + 1;
        if n == 10 {
            Reply::tool_call(
                "ask",
                "ask_user",
                json!({"question": "Ready?", "choices": []}),
            )
        } else {
            Reply::tool_call(&format!("read-{n}"), "read_history", json!({}))
        }
    })
    .await?;
    let path = format!("/api/agents/{}/turns", f.id());
    let id = new_id();
    f.app
        .post(&path, json!({"id": id, "text": "Read then ask."}))
        .await?;
    let waiting = f
        .app
        .wait_row(&path, &id, |row| {
            row["questions"].as_array().map(Vec::len) == Some(1)
        })
        .await?;
    ensure!(
        calls.load(Ordering::SeqCst) == 10,
        "tool calls before the question: {}",
        calls.load(Ordering::SeqCst)
    );
    f.app.restart().await?;
    f.app
        .post(
            &format!(
                "{path}/{id}/questions/{}/answer",
                waiting["questions"][0]["id"].as_str().unwrap_or_default()
            ),
            json!({"id": new_id(), "text": "Ready."}),
        )
        .await?;
    let done = f.turn(&path, &id).await?;
    ensure!(done["status"] == "done", "answered: {}", done["error"]);
    ensure!(
        done["activity"]["budget_stopped"] == 10,
        "budget: {}",
        done["activity"]["budget_stopped"]
    );
    // B1: the harness takes one more step after `ask_user` returns null;
    // here it is the budget summary of the first run.
    let work = f
        .model
        .requests()
        .iter()
        .filter(|r| {
            !r.tool_results()
                .last()
                .is_some_and(|m| m["content"] == "null")
        })
        .count();
    ensure!(work == 11, "requests: {work}");
    ensure!(
        !offered(f.model.requests().last().ok_or("no request")?),
        "summary offered tools"
    );
    Ok(())
}
