//! Agent delegation (port of `app/agent-delegation.test.ts`).

use crate::fx_support::{after_pause, id, pause_reply_for, row, rows, tool_json};
use minarbot_testkit::{
    db::Inspector,
    gated::{Answer, Gate, GatedModel},
    model::ModelRequest,
    prelude::*,
    server::Gateway,
    Server,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

/// Parsed tool results of the current request.
fn results(request: &ModelRequest) -> Vec<Value> {
    request.tool_results().iter().map(tool_json).collect()
}

fn wake(request: &ModelRequest) -> bool {
    request.last_user_text().starts_with("Delegated task completed")
}

/// One server and model per case (the Node `fixture`): concurrency 12,
/// optional agent limits, agents named after their model.
struct Fixture {
    app: Server,
    model: GatedModel,
    _root: TestRoot,
}

impl Fixture {
    async fn start(model: GatedModel, limits: Option<Value>) -> Result<Self> {
        let root = TestRoot::new()?;
        let mut config = ServerConfig::new()
            .gateway(Gateway::new(model.base_url()))
            .concurrency(12);
        if let Some(limits) = limits {
            config = config.option("agentLimits", limits);
        }
        let app = root.start(config).await?;
        Ok(Self {
            app,
            model,
            _root: root,
        })
    }

    async fn agent(&self, name: &str) -> Result<String> {
        id(&self
            .app
            .post_expect(
                "/api/agents",
                json!({"runtime": "api", "id": new_id(), "name": name, "model": name,
                    "instructions": format!("{name} role")}),
                201,
            )
            .await?)
    }

    /// Wait until the rows of `path` satisfy `done`.
    async fn until(&self, path: &str, done: impl Fn(&[Value]) -> bool) -> Result<Vec<Value>> {
        let rows = self
            .app
            .wait_get(path, |value| value.as_array().is_some_and(|rows| done(rows)))
            .await?;
        Ok(rows.as_array().cloned().unwrap_or_default())
    }

    async fn settled(&self, path: &str, turn: &str) -> Result<Value> {
        let rows = self
            .until(path, |rows| rows.iter().any(|row| row["id"] == turn && row["status"] == "done"))
            .await?;
        rows.into_iter()
            .find(|row| row["id"] == turn)
            .ok_or_else(|| "turn vanished".into())
    }

    fn calls(&self, model: &str) -> usize {
        self.model
            .requests()
            .iter()
            .filter(|request| request.model() == model && !after_pause(request))
            .count()
    }
}

fn reply(row: &Value) -> Result<Value> {
    Ok(serde_json::from_str(row["reply"].as_str().ok_or_else(|| format!("no reply: {row}"))?)?)
}

#[tokio::test]
async fn delegation_isolates_child_context_and_durably_wakes_idle_parent_exactly_once(
) -> Result<()> {
    let target: Arc<Mutex<String>> = Arc::default();
    let wakes = Arc::new(AtomicUsize::new(0));
    let failures: Arc<Mutex<Vec<String>>> = Arc::default();
    let held = Gate::new();
    let model = GatedModel::start().await?;
    let (child, woken, problems, gate) = (target.clone(), wakes.clone(), failures.clone(), held.clone());
    model.respond(move |request| {
        let fail = |message: String| {
            if let Ok(mut problems) = problems.lock() {
                problems.push(message);
            }
            Answer::Status(500, json!({}))
        };
        let results = results(request);
        if request.model() == "child" {
            if request.mentions("PRIVATE_HISTORY") {
                return fail("child saw the parent's history".into());
            }
            return Answer::after(&gate, Answer::text("Child final result"));
        }
        if request.mentions("Delegated task completed") {
            woken.fetch_add(1, Ordering::SeqCst);
            return Answer::text("Parent received result");
        }
        if results.is_empty() {
            if !request.tool_names().iter().any(|name| name == "delegate_task") {
                return fail("delegate_task not offered".into());
            }
            let agent = child.lock().map(|value| value.clone()).unwrap_or_default();
            return Answer::tool_id(
                "delegate",
                "delegate_task",
                json!({"agent": agent, "task": "Isolated work", "wait": false}),
            );
        }
        if results.last().is_some_and(|last| last["status"] != "queued") {
            return fail(format!("delegation result {:?}", results.last()));
        }
        Answer::text("Child launched")
    });
    let root = TestRoot::new()?;
    let mut app = root
        .start(ServerConfig::new().gateway(Gateway::new(model.base_url())))
        .await?;
    let child = id(&app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Child", "model": "child", "instructions": "Help."}),
        )
        .await?)?;
    if let Ok(mut target) = target.lock() {
        target.clone_from(&child);
    }
    let parent = id(&app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Parent", "model": "parent", "instructions": "Coordinate."}),
        )
        .await?)?;
    let path = format!("/api/agents/{parent}/turns");
    let turn = new_id();
    app.post(&path, json!({"id": turn, "text": "PRIVATE_HISTORY"})).await?;
    let done = app.wait_status(&path, &turn, "done").await?;
    ensure!(
        done["agentMessages"].as_array().map(Vec::len) == Some(1),
        "agent messages {}",
        done["agentMessages"]
    );
    held.open();
    app.wait_get(&path, |rows| {
        rows.as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["reply"] == "Parent received result"))
    })
    .await?;
    app.restart().await?;
    let received = rows(&app, &path)
        .await?
        .iter()
        .filter(|row| row["reply"] == "Parent received result")
        .count();
    ensure!(received == 1, "wake rows {received}");
    ensure!(wakes.load(Ordering::SeqCst) == 1, "wakes {}", wakes.load(Ordering::SeqCst));
    ensure!(failures.lock().map(|f| f.is_empty()).unwrap_or(false), "provider checks {:?}", failures.lock().map(|f| f.clone()));
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WaitMode {
    Complete,
    Timeout,
    RestartWait,
    RestartChild,
    DelegateWait,
}

async fn durable_task_wait(mode: WaitMode) -> Result<()> {
    let target: Arc<Mutex<String>> = Arc::default();
    let child = Gate::new();
    let child_calls = Arc::new(AtomicUsize::new(0));
    let model = GatedModel::start().await?;
    let (agent, gate, calls) = (target.clone(), child.clone(), child_calls.clone());
    model.respond(move |request| {
        if after_pause(request) {
            return pause_reply_for(request);
        }
        let rs = results(request);
        if request.model() == "Child" {
            calls.fetch_add(1, Ordering::SeqCst);
            if mode == WaitMode::RestartWait && rs.is_empty() {
                return Answer::tool("ask_user", json!({"question": "Finish child?", "choices": ["Yes"]}));
            }
            return Answer::after(&gate, Answer::text("Child result"));
        }
        if wake(request) {
            return Answer::text("Late result received");
        }
        let Some(last) = rs.last() else {
            let agent = agent.lock().map(|value| value.clone()).unwrap_or_default();
            return Answer::tool(
                "delegate_task",
                json!({"agent": agent, "task": "Child work", "wait": mode == WaitMode::DelegateWait}),
            );
        };
        if last["taskId"].is_string() && last["status"] == "queued" {
            return Answer::tool(
                "wait_task",
                json!({"taskId": last["taskId"], "timeoutSeconds": if mode == WaitMode::Timeout { 1 } else { 600 }}),
            );
        }
        Answer::text(last.to_string())
    });
    let mut f = Fixture::start(model, None).await?;
    let child_id = f.agent("Child").await?;
    if let Ok(mut target) = target.lock() {
        target.clone_from(&child_id);
    }
    let parent = f.agent("Parent").await?;
    let path = format!("/api/agents/{parent}/turns");
    let turn = new_id();
    f.app.post(&path, json!({"id": turn, "text": "Start task"})).await?;
    let waiting_rows = f
        .until(&path, |rows| {
            rows.iter().any(|row| {
                row["id"] == turn.as_str()
                    && row["status"] == "queued"
                    && row["agentMessages"].as_array().is_some_and(|items| !items.is_empty())
            })
        })
        .await?;
    let waiting = waiting_rows
        .iter()
        .find(|row| row["id"] == turn.as_str())
        .cloned()
        .ok_or("waiting row")?;
    ensure!(waiting["reply"].is_null(), "reply while waiting: {}", waiting["reply"]);
    if mode == WaitMode::Timeout {
        let done = f.settled(&path, &turn).await?;
        ensure!(reply(&done)?["timedOut"] == true, "timeout reply {}", done["reply"]);
        child.open();
        f.until(&path, |rows| {
            rows.iter().any(|row| {
                row["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("Delegated task completed"))
                    && row["status"] == "done"
            })
        })
        .await?;
        return Ok(());
    }
    if mode == WaitMode::RestartWait {
        let item = &waiting["agentMessages"][0];
        let child_path = format!("/api/threads/{}/turns", item["thread"].as_str().ok_or("thread")?);
        let child_rows = f
            .until(&child_path, |rows| {
                rows.first()
                    .is_some_and(|row| row["questions"].as_array().is_some_and(|q| !q.is_empty()))
            })
            .await?;
        ensure!(
            row(&f.app, &path, &turn).await?["agentMessages"][0]["status"] == "waiting",
            "parent item not waiting"
        );
        f.app.restart().await?;
        child.open();
        f.app
            .post(
                &format!(
                    "{child_path}/{}/questions/{}/answer",
                    id(item)?,
                    id(&child_rows[0]["questions"][0])?
                ),
                json!({"id": new_id(), "text": "Yes"}),
            )
            .await?;
    } else {
        if mode == WaitMode::RestartChild {
            let child_path = format!(
                "/api/threads/{}/turns",
                waiting["agentMessages"][0]["thread"].as_str().ok_or("thread")?
            );
            f.until(&child_path, |rows| rows.first().is_some_and(|row| row["status"] == "running"))
                .await?;
            // The harness asks the model a moment after the turn starts; the
            // Node API runtime had sent that request by then.
            until("child model request", || async {
                Ok((child_calls.load(Ordering::SeqCst) == 1).then_some(()))
            })
            .await?;
            f.app.restart().await?;
        }
        child.open();
    }
    let done = f.settled(&path, &turn).await?;
    let result = reply(&done)?;
    ensure!(
        result["status"] == if mode == WaitMode::RestartChild { "uncertain" } else { "done" },
        "result {result}"
    );
    if mode != WaitMode::RestartChild {
        ensure!(result["result"] == "Child result", "child result {result}");
    }
    let expected = if mode == WaitMode::RestartWait { 2 } else { 1 };
    ensure!(
        child_calls.load(Ordering::SeqCst) == expected,
        "child calls {} (expected {expected})",
        child_calls.load(Ordering::SeqCst)
    );
    f.app.restart().await?;
    let wakes = rows(&f.app, &path)
        .await?
        .iter()
        .filter(|row| {
            row["text"]
                .as_str()
                .is_some_and(|text| text.starts_with("Delegated task completed"))
        })
        .count();
    ensure!(wakes == 0, "completion wake despite the wait: {wakes}");
    Ok(())
}

#[tokio::test]
async fn durable_task_wait_complete() -> Result<()> {
    durable_task_wait(WaitMode::Complete).await
}

#[tokio::test]
async fn durable_task_wait_timeout() -> Result<()> {
    durable_task_wait(WaitMode::Timeout).await
}

#[tokio::test]
async fn durable_task_wait_restart_wait() -> Result<()> {
    durable_task_wait(WaitMode::RestartWait).await
}

#[tokio::test]
async fn durable_task_wait_restart_child() -> Result<()> {
    durable_task_wait(WaitMode::RestartChild).await
}

#[tokio::test]
async fn durable_task_wait_delegate_wait() -> Result<()> {
    durable_task_wait(WaitMode::DelegateWait).await
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Limit {
    Depth,
    Children,
    Size,
}

async fn backend_delegation_limit(limit: Limit) -> Result<()> {
    let ids: Arc<Mutex<Vec<String>>> = Arc::default();
    let child = Gate::new();
    let model = GatedModel::start().await?;
    let (agents, gate) = (ids.clone(), child.clone());
    model.respond(move |request| {
        let rs = results(request);
        let ids = agents.lock().map(|ids| ids.clone()).unwrap_or_default();
        if limit == Limit::Depth {
            if let Some(last) = rs.last() {
                return Answer::text(last.to_string());
            }
            let next = request.model()[1..].parse::<usize>().unwrap_or(0) + 1;
            return Answer::tool(
                "delegate_task",
                json!({"agent": ids.get(next), "task": "Nested task", "wait": true}),
            );
        }
        if request.model() != "Parent" {
            return Answer::after(&gate, Answer::text("Child result"));
        }
        if limit == Limit::Size {
            return match rs.last() {
                Some(last) => Answer::text(last.to_string()),
                None => Answer::tool(
                    "delegate_task",
                    json!({"agent": ids[0], "task": "x".repeat(16001)}),
                ),
            };
        }
        if rs.len() >= 8 {
            return Answer::text(rs[rs.len() - 1].to_string());
        }
        Answer::tool(
            "send_message",
            json!({"recipients": ids, "text": "Child work", "expects_reply": false}),
        )
    });
    let f = Fixture::start(model, None).await?;
    if limit == Limit::Depth {
        for index in 0..5 {
            let agent = f.agent(&format!("A{index}")).await?;
            if let Ok(mut ids) = ids.lock() {
                ids.push(agent);
            }
        }
        let first = ids.lock().map_err(|_| "ids")?[0].clone();
        let path = format!("/api/agents/{first}/turns");
        f.app.post(&path, json!({"id": new_id(), "text": "Start"})).await?;
        let done = f
            .until(&path, |rows| rows.first().is_some_and(|row| row["status"] == "done"))
            .await?;
        ensure!(
            done[0]["reply"].as_str().is_some_and(|reply| reply.contains("depth")),
            "depth reply {}",
            done[0]["reply"]
        );
        ensure!(f.calls("A4") == 0, "A4 ran past the depth limit");
        return Ok(());
    }
    for index in 0..(if limit == Limit::Children { 8 } else { 1 }) {
        let agent = f.agent(&format!("Child{index}")).await?;
        if let Ok(mut ids) = ids.lock() {
            ids.push(agent);
        }
    }
    let parent = f.agent("Parent").await?;
    let path = format!("/api/agents/{parent}/turns");
    f.app.post(&path, json!({"id": new_id(), "text": "Start"})).await?;
    if limit == Limit::Children {
        // #87: the agent loop failed the turn on this refusal; over MCP the
        // model gets the 409 and ends the turn with it.
        let done = f
            .until(&path, |rows| rows.first().is_some_and(|row| row["status"] == "done"))
            .await?;
        ensure!(
            done[0]["reply"].as_str().is_some_and(|reply| reply.contains("Concurrent child")),
            "children error {}",
            done[0]["reply"]
        );
        ensure!(
            done[0]["agentMessages"].as_array().map(Vec::len) == Some(8),
            "agent messages {}",
            done[0]["agentMessages"]
        );
    } else {
        let done = f
            .until(&path, |rows| rows.first().is_some_and(|row| row["status"] == "done"))
            .await?;
        ensure!(
            done[0]["reply"].as_str().is_some_and(|reply| reply.contains("16,000")),
            "size reply {}",
            done[0]["reply"]
        );
        ensure!(
            done[0]["agentMessages"].as_array().map(Vec::len) == Some(0),
            "oversized task started"
        );
    }
    child.open();
    Ok(())
}

#[tokio::test]
async fn backend_delegation_limit_depth() -> Result<()> {
    backend_delegation_limit(Limit::Depth).await
}

#[tokio::test]
async fn backend_delegation_limit_children() -> Result<()> {
    backend_delegation_limit(Limit::Children).await
}

#[tokio::test]
async fn backend_delegation_limit_size() -> Result<()> {
    backend_delegation_limit(Limit::Size).await
}

#[derive(Default)]
struct Tree {
    beta: String,
    gamma: String,
    root_task: Value,
}

#[tokio::test]
async fn delegated_task_authority_denies_unrelated_caller_cancel_stops_entire_mixed_message_tree(
) -> Result<()> {
    let state = Arc::new(Mutex::new(Tree::default()));
    let held = Gate::new();
    let model = GatedModel::start().await?;
    let (shared, gate) = (state.clone(), held.clone());
    model.respond(move |request| {
        if after_pause(request) {
            return pause_reply_for(request);
        }
        let rs = results(request);
        let Ok(mut tree) = shared.lock() else {
            return Answer::Status(500, json!({}));
        };
        match request.model() {
            "Other" => {
                return if rs.len() >= 2 {
                    Answer::text(json!(rs).to_string())
                } else {
                    Answer::tool(
                        if rs.is_empty() { "task_status" } else { "cancel_task" },
                        json!({"taskId": tree.root_task}),
                    )
                }
            }
            "Gamma" => return Answer::after(&gate, Answer::text("Late result")),
            "Beta" => {
                return if rs.is_empty() {
                    Answer::tool(
                        "send_message",
                        json!({"recipients": [tree.gamma], "text": "Grandchild work", "expects_reply": true}),
                    )
                } else {
                    Answer::text("Beta result")
                }
            }
            _ => {}
        }
        if rs.is_empty() {
            return Answer::tool("delegate_task", json!({"agent": tree.beta, "task": "Child work"}));
        }
        tree.root_task = rs[0]["taskId"].clone();
        match rs.len() {
            1 => Answer::tool("ask_user", json!({"question": "Cancel tree?", "choices": ["Yes"]})),
            2 => Answer::tool("cancel_task", json!({"taskId": tree.root_task})),
            _ => Answer::text(rs[rs.len() - 1].to_string()),
        }
    });
    let f = Fixture::start(model, None).await?;
    let beta = f.agent("Beta").await?;
    let gamma = f.agent("Gamma").await?;
    if let Ok(mut tree) = state.lock() {
        tree.beta = beta;
        tree.gamma = gamma;
    }
    let parent = f.agent("Parent").await?;
    let other = f.agent("Other").await?;
    let path = format!("/api/agents/{parent}/turns");
    let turn = new_id();
    f.app.post(&path, json!({"id": turn, "text": "Start"})).await?;
    let waiting = f
        .until(&path, |rows| {
            rows.first()
                .is_some_and(|row| row["questions"].as_array().is_some_and(|q| !q.is_empty()))
        })
        .await?[0]
        .clone();
    let item = &waiting["agentMessages"][0];
    let child_path = format!("/api/threads/{}/turns", item["thread"].as_str().ok_or("thread")?);
    let nested = f
        .until(&child_path, |rows| {
            rows.first().is_some_and(|row| {
                row["agentMessages"]
                    .as_array()
                    .is_some_and(|items| items.iter().any(|item| item["status"] == "running"))
            })
        })
        .await?[0]
        .clone();
    let other_path = format!("/api/agents/{other}/turns");
    f.app
        .post(&other_path, json!({"id": new_id(), "text": "Try unrelated task"}))
        .await?;
    let denied = f
        .until(&other_path, |rows| rows.first().is_some_and(|row| row["status"] == "done"))
        .await?[0]
        .clone();
    let denied_results = reply(&denied)?;
    ensure!(
        denied_results.as_array().is_some_and(|results| {
            results.len() == 2 && results.iter().all(|result| result["status"] == 403)
        }),
        "unrelated caller results {denied_results}"
    );
    f.app
        .post(
            &format!("{path}/{turn}/questions/{}/answer", id(&waiting["questions"][0])?),
            json!({"id": new_id(), "text": "Yes"}),
        )
        .await?;
    let done = f.settled(&path, &turn).await?;
    ensure!(reply(&done)?["status"] == "cancelled", "cancel result {}", done["reply"]);
    ensure!(
        rows(&f.app, &child_path).await?[0]["status"] == "cancelled",
        "child not cancelled"
    );
    ensure!(
        rows(
            &f.app,
            &format!(
                "/api/threads/{}/turns",
                nested["agentMessages"][0]["thread"].as_str().ok_or("thread")?
            )
        )
        .await?[0]["status"]
            == "cancelled",
        "grandchild not cancelled"
    );
    held.open();
    Ok(())
}

#[tokio::test]
async fn team_awareness_refreshes_role_and_bounded_roster_on_next_provider_request(
) -> Result<()> {
    // #87 removed the provider client that rebuilt the system prompt with the
    // roster before every request. The roster now reaches harness turns
    // through `list_agents`, so the refresh is asserted on that tool's result
    // within the same turn.
    let roster: Arc<Mutex<Vec<Value>>> = Arc::default();
    let advance = Gate::new();
    let model = GatedModel::start().await?;
    let (seen, gate) = (roster.clone(), advance.clone());
    model.respond(move |request| {
        let Ok(mut seen) = seen.lock() else {
            return Answer::Status(500, json!({}));
        };
        let rs = results(request);
        match rs.len() {
            0 => Answer::tool("list_agents", json!({})),
            1 => {
                seen.push(rs[0].clone());
                Answer::after(&gate, Answer::tool("list_agents", json!({})))
            }
            _ => {
                seen.push(rs[rs.len() - 1].clone());
                Answer::text("Refreshed")
            }
        }
    });
    let f = Fixture::start(model, None).await?;
    let parent = f.agent("Parent").await?;
    let path = format!("/api/agents/{parent}/turns");
    f.app.post(&path, json!({"id": new_id(), "text": "Roster"})).await?;
    let first = until("first roster", || async {
        Ok(roster.lock().ok().and_then(|seen| seen.first().cloned()))
    })
    .await?;
    let teammate = f
        .app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "New teammate", "model": "New teammate",
                "instructions": "New teammate role"}),
            201,
        )
        .await?;
    f.app
        .post(
            &format!("/api/agents/{}/edit", id(&teammate)?),
            json!({"id": new_id(), "revision": teammate["revision"], "name": teammate["name"],
                "role": "Research agent", "model": teammate["model"], "instructions": teammate["instructions"]}),
        )
        .await?;
    advance.open();
    f.until(&path, |rows| rows.first().is_some_and(|row| row["status"] == "done"))
        .await?;
    let second = roster.lock().ok().and_then(|seen| seen.get(1).cloned()).ok_or("second roster")?;
    ensure!(!first.to_string().contains("New teammate"), "first roster already had the teammate");
    let listed = second["agents"]
        .as_array()
        .and_then(|agents| agents.iter().find(|agent| agent["id"] == teammate["id"]))
        .cloned()
        .ok_or_else(|| format!("teammate missing from {second}"))?;
    ensure!(listed["role"] == "Research agent", "teammate role {listed}");
    ensure!(second.to_string().len() < 5000, "roster {} bytes", second.to_string().len());
    Ok(())
}

async fn rate_error(message_tool: &'static str) -> Result<()> {
    let rate: usize = if message_tool == "send_message" { 20 } else { 2 };
    let target: Arc<Mutex<String>> = Arc::default();
    let model = GatedModel::start().await?;
    let recipient = target.clone();
    model.respond(move |request| {
        let rs = results(request);
        if request.model() != "Parent" {
            return Answer::text("Child result");
        }
        if wake(request) {
            return Answer::text("Received");
        }
        if let Some(last) = rs.last() {
            return Answer::text(last.to_string());
        }
        if message_tool == "delegate_task" {
            Answer::tool(message_tool, json!({"agent": "Child", "task": "Work"}))
        } else {
            let child = recipient.lock().map(|value| value.clone()).unwrap_or_default();
            Answer::tool(
                message_tool,
                json!({"recipients": [child], "text": "Work", "expects_reply": false}),
            )
        }
    });
    let limits = (message_tool == "delegate_task")
        .then(|| json!({"rate": rate, "rateWindowMs": 300_000, "cooldownMs": 600_000}));
    let mut f = Fixture::start(model, limits).await?;
    let child = f.agent("Child").await?;
    if let Ok(mut target) = target.lock() {
        target.clone_from(&child);
    }
    let parent = f.agent("Parent").await?;
    let path = format!("/api/agents/{parent}/turns");
    let side = id(&f
        .app
        .post(
            &format!("/api/agents/{parent}/threads"),
            json!({"id": new_id(), "name": "Other conversation"}),
        )
        .await?)?;
    for index in 0..rate + 2 {
        let work_path = if index < rate {
            path.clone()
        } else {
            format!("/api/threads/{side}/turns")
        };
        let turn = new_id();
        f.app
            .post(&work_path, json!({"id": turn, "text": format!("Work {index}")}))
            .await?;
        let result = reply(&f.settled(&work_path, &turn).await?)?;
        if index < rate {
            let expected = if message_tool == "delegate_task" { "queued" } else { "sent" };
            ensure!(result["status"] == expected, "work {index}: {result}");
        } else {
            let expected = if index == rate { "rate limit reached" } else { "cooling down" };
            ensure!(
                result["error"].as_str().is_some_and(|error| error.contains(expected)),
                "work {index}: {result}"
            );
        }
        if index == rate {
            f.app.restart().await?;
        }
    }
    f.app
        .edit_database(|db| {
            db.execute_batch(
                "UPDATE agent_message_events SET created=0; UPDATE agent_message_cooldowns SET until=0",
            )
        })
        .await?;
    let turn = new_id();
    f.app
        .post(&path, json!({"id": turn, "text": "After cooldown"}))
        .await?;
    let result = reply(&f.settled(&path, &turn).await?)?;
    let expected = if message_tool == "delegate_task" { "queued" } else { "sent" };
    ensure!(result["status"] == expected, "after cooldown {result}");
    Ok(())
}

#[tokio::test]
async fn delegate_task_receives_actionable_rate_error_across_conversations_restart_and_cooldown(
) -> Result<()> {
    rate_error("delegate_task").await
}

#[tokio::test]
async fn send_message_receives_actionable_rate_error_across_conversations_restart_and_cooldown(
) -> Result<()> {
    rate_error("send_message").await
}

async fn nested_async_children(cancel: bool) -> Result<()> {
    let ids: Arc<Mutex<(String, String)>> = Arc::default();
    let held = Gate::new();
    let model = GatedModel::start().await?;
    let (agents, gate) = (ids.clone(), held.clone());
    model.respond(move |request| {
        if after_pause(request) {
            return pause_reply_for(request);
        }
        let rs = results(request);
        let (child, grandchild) = agents.lock().map(|ids| ids.clone()).unwrap_or_default();
        match request.model() {
            "Grandchild" => return Answer::after(&gate, Answer::text("Grandchild final")),
            "Child" => {
                if wake(request) {
                    return Answer::text("Combined nested result");
                }
                return if rs.is_empty() {
                    Answer::tool("delegate_task", json!({"agent": grandchild, "task": "Nested task"}))
                } else {
                    Answer::text("Nested task launched")
                };
            }
            _ => {}
        }
        if cancel && rs.len() == 1 {
            return Answer::tool("ask_user", json!({"question": "Cancel nested work?", "choices": ["Yes"]}));
        }
        if cancel && rs.len() == 2 {
            return Answer::tool("cancel_task", json!({"taskId": rs[0]["taskId"]}));
        }
        match rs.last() {
            Some(last) => Answer::text(last.to_string()),
            None => Answer::tool(
                "delegate_task",
                json!({"agent": child, "task": "Gather nested result", "wait": !cancel}),
            ),
        }
    });
    let mut f = Fixture::start(model, None).await?;
    let child = f.agent("Child").await?;
    let grandchild = f.agent("Grandchild").await?;
    if let Ok(mut ids) = ids.lock() {
        *ids = (child, grandchild);
    }
    let parent = f.agent("Parent").await?;
    let path = format!("/api/agents/{parent}/turns");
    f.app
        .post(&path, json!({"id": new_id(), "text": "Nested request"}))
        .await?;
    let parent_row = f
        .until(&path, |rows| {
            rows.first()
                .is_some_and(|row| row["agentMessages"].as_array().is_some_and(|items| !items.is_empty()))
        })
        .await?[0]
        .clone();
    let child_path = format!(
        "/api/threads/{}/turns",
        parent_row["agentMessages"][0]["thread"].as_str().ok_or("thread")?
    );
    f.until(&child_path, |rows| rows.first().is_some_and(|row| row["status"] == "done"))
        .await?;
    ensure!(rows(&f.app, &path).await?[0]["status"] == "queued", "parent finished early");
    if cancel {
        let waiting = f
            .until(&path, |rows| {
                rows.first()
                    .is_some_and(|row| row["questions"].as_array().is_some_and(|q| !q.is_empty()))
            })
            .await?[0]
            .clone();
        ensure!(
            waiting["agentMessages"][0]["status"] == "waiting",
            "child item {}",
            waiting["agentMessages"][0]
        );
        f.app
            .post(
                &format!("{path}/{}/questions/{}/answer", id(&waiting)?, id(&waiting["questions"][0])?),
                json!({"id": new_id(), "text": "Yes"}),
            )
            .await?;
    } else {
        held.open();
    }
    let done = f
        .until(&path, |rows| rows.first().is_some_and(|row| row["status"] == "done"))
        .await?[0]
        .clone();
    if cancel {
        ensure!(reply(&done)?["status"] == "cancelled", "cancel result {}", done["reply"]);
        f.app.restart().await?;
        let child_rows = rows(&f.app, &child_path).await?;
        ensure!(child_rows.len() == 1, "child rows {}", child_rows.len());
        ensure!(child_rows[0]["status"] == "cancelled", "child status {}", child_rows[0]["status"]);
    } else {
        ensure!(
            reply(&done)?["result"] == "Combined nested result",
            "nested result {}",
            done["reply"]
        );
    }
    held.open();
    Ok(())
}

#[tokio::test]
async fn nested_async_children_wait_includes_completion_follow_up() -> Result<()> {
    nested_async_children(false).await
}

#[tokio::test]
async fn nested_async_children_cancel_completed_root_with_running_descendants() -> Result<()> {
    nested_async_children(true).await
}

#[tokio::test]
async fn owner_later_turns_can_inspect_and_wait_without_a_duplicate_completion_wake(
) -> Result<()> {
    let state: Arc<Mutex<(String, Value)>> = Arc::default();
    let held = Gate::new();
    let model = GatedModel::start().await?;
    let (shared, gate) = (state.clone(), held.clone());
    model.respond(move |request| {
        let Ok(mut state) = shared.lock() else {
            return Answer::Status(500, json!({}));
        };
        if request.model() == "Child" {
            return Answer::after(&gate, Answer::text("Later-turn result"));
        }
        let rs = results(request);
        let input = request.last_user_text();
        if input.starts_with("Delegated task completed") {
            return Answer::text("Unexpected wake");
        }
        if let Some(last) = rs.last() {
            if input == "Start" {
                state.1 = rs[0]["taskId"].clone();
            }
            return Answer::text(last.to_string());
        }
        match input.as_str() {
            "Inspect" => Answer::tool("task_status", json!({"taskId": state.1})),
            "Wait" => Answer::tool("wait_task", json!({"taskId": state.1, "timeoutSeconds": 600})),
            _ => Answer::tool("delegate_task", json!({"agent": state.0, "task": "Work"})),
        }
    });
    let mut f = Fixture::start(model, None).await?;
    let child = f.agent("Child").await?;
    if let Ok(mut state) = state.lock() {
        state.0 = child;
    }
    let parent = f.agent("Parent").await?;
    let path = format!("/api/agents/{parent}/turns");
    for text in ["Start", "Inspect"] {
        let turn = new_id();
        f.app.post(&path, json!({"id": turn, "text": text})).await?;
        let result = reply(&f.settled(&path, &turn).await?)?;
        let expected = if text == "Start" { "queued" } else { "running" };
        ensure!(result["status"] == expected, "{text}: {result}");
    }
    let turn = new_id();
    f.app.post(&path, json!({"id": turn, "text": "Wait"})).await?;
    f.until(&path, |rows| {
        rows.iter()
            .any(|row| row["id"] == turn.as_str() && !row["waiting_for"].is_null())
    })
    .await?;
    held.open();
    let result = reply(&f.settled(&path, &turn).await?)?;
    ensure!(result["result"] == "Later-turn result", "wait result {result}");
    f.app.restart().await?;
    let all = rows(&f.app, &path).await?;
    ensure!(all.len() == 3, "duplicate completion wake: {} rows", all.len());
    Ok(())
}

#[tokio::test]
async fn completion_wakes_a_parent_waiting_for_user_input_and_preserves_its_pending_question(
) -> Result<()> {
    let target: Arc<Mutex<String>> = Arc::default();
    let held = Gate::new();
    let model = GatedModel::start().await?;
    let (agent, gate) = (target.clone(), held.clone());
    model.respond(move |request| {
        if after_pause(request) {
            return pause_reply_for(request);
        }
        if request.model() == "Child" {
            return Answer::after(&gate, Answer::text("Result while waiting"));
        }
        if wake(request) {
            return Answer::text("Completion received");
        }
        if results(request).is_empty() {
            let agent = agent.lock().map(|value| value.clone()).unwrap_or_default();
            Answer::tool("delegate_task", json!({"agent": agent, "task": "Work"}))
        } else {
            Answer::tool("ask_user", json!({"question": "Choose next work?", "choices": ["Later"]}))
        }
    });
    let mut f = Fixture::start(model, None).await?;
    let child = f.agent("Child").await?;
    if let Ok(mut target) = target.lock() {
        target.clone_from(&child);
    }
    let parent = f.agent("Parent").await?;
    let path = format!("/api/agents/{parent}/turns");
    let turn = new_id();
    f.app.post(&path, json!({"id": turn, "text": "Start"})).await?;
    f.until(&path, |rows| {
        rows.first()
            .is_some_and(|row| row["questions"].as_array().is_some_and(|q| !q.is_empty()))
    })
    .await?;
    held.open();
    f.until(&path, |rows| rows.iter().any(|row| row["reply"] == "Completion received"))
        .await?;
    f.app.restart().await?;
    let all = rows(&f.app, &path).await?;
    let wakes = all
        .iter()
        .filter(|row| {
            row["text"]
                .as_str()
                .is_some_and(|text| text.starts_with("Delegated task completed"))
        })
        .count();
    ensure!(wakes == 1, "wakes {wakes}");
    let waiting = all.iter().find(|row| row["id"] == turn.as_str()).ok_or("parent row")?;
    ensure!(waiting["paused"] == 1, "question no longer pauses the parent: {}", waiting["paused"]);
    ensure!(waiting["questions"][0]["answer"].is_null(), "question answered");
    Ok(())
}

#[tokio::test]
async fn concurrent_child_limit_counts_tasks_still_waiting_on_their_async_descendants(
) -> Result<()> {
    let ids: Arc<Mutex<(String, String, String)>> = Arc::default();
    let spare_started = Arc::new(AtomicUsize::new(0));
    let held = Gate::new();
    let model = GatedModel::start().await?;
    let (agents, started, gate) = (ids.clone(), spare_started.clone(), held.clone());
    model.respond(move |request| {
        if after_pause(request) {
            return pause_reply_for(request);
        }
        let rs = results(request);
        let (child, spare, grandchild) = agents.lock().map(|ids| ids.clone()).unwrap_or_default();
        match request.model() {
            "Grandchild" => return Answer::after(&gate, Answer::text("Grandchild result")),
            "Child" => {
                return if rs.is_empty() {
                    Answer::tool("delegate_task", json!({"agent": grandchild, "task": "Nested work"}))
                } else {
                    Answer::text("Waiting for nested result")
                }
            }
            "Spare" => {
                started.fetch_add(1, Ordering::SeqCst);
                return Answer::Status(500, json!({"error": "Concurrent task must not start"}));
            }
            _ => {}
        }
        match rs.len() {
            0 => Answer::tool("delegate_task", json!({"agent": child, "task": "Work"})),
            1 => Answer::tool("ask_user", json!({"question": "Start second child?", "choices": ["Yes"]})),
            2 => Answer::tool("delegate_task", json!({"agent": spare, "task": "More work"})),
            _ => Answer::text(rs[rs.len() - 1].to_string()),
        }
    });
    let f = Fixture::start(model, Some(json!({"concurrentChildren": 1}))).await?;
    let child = f.agent("Child").await?;
    let spare = f.agent("Spare").await?;
    let grandchild = f.agent("Grandchild").await?;
    if let Ok(mut ids) = ids.lock() {
        *ids = (child, spare, grandchild);
    }
    let parent = f.agent("Parent").await?;
    let path = format!("/api/agents/{parent}/turns");
    let turn = new_id();
    f.app.post(&path, json!({"id": turn, "text": "Start"})).await?;
    let parent_row = f
        .until(&path, |rows| {
            rows.first()
                .is_some_and(|row| row["questions"].as_array().is_some_and(|q| !q.is_empty()))
        })
        .await?[0]
        .clone();
    f.until(
        &format!(
            "/api/threads/{}/turns",
            parent_row["agentMessages"][0]["thread"].as_str().ok_or("thread")?
        ),
        |rows| rows.first().is_some_and(|row| row["status"] == "done"),
    )
    .await?;
    ensure!(
        row(&f.app, &path, &turn).await?["agentMessages"][0]["status"] == "waiting",
        "child item no longer waiting"
    );
    f.app
        .post(
            &format!("{path}/{turn}/questions/{}/answer", id(&parent_row["questions"][0])?),
            json!({"id": new_id(), "text": "Yes"}),
        )
        .await?;
    let done = f.settled(&path, &turn).await?;
    let result = reply(&done)?;
    ensure!(
        result["error"]
            .as_str()
            .is_some_and(|error| error.contains("Concurrent child limit") && error.contains("1 children")),
        "limit error {result}"
    );
    ensure!(
        done["agentMessages"].as_array().map(Vec::len) == Some(1),
        "agent messages {}",
        done["agentMessages"]
    );
    ensure!(spare_started.load(Ordering::SeqCst) == 0, "denied child started");
    held.open();
    Ok(())
}

#[tokio::test]
async fn rust_counts_active_children_of_an_owner_completion_turn_against_its_inherited_budget(
) -> Result<()> {
    let spare_target: Arc<Mutex<String>> = Arc::default();
    let spare_started = Arc::new(AtomicUsize::new(0));
    let held = Gate::new();
    let model = GatedModel::start().await?;
    let (spare, started, gate) = (spare_target.clone(), spare_started.clone(), held.clone());
    model.respond(move |request| match request.model() {
        "Child" => Answer::after(&gate, Answer::text("Child finished")),
        "Spare" => {
            started.fetch_add(1, Ordering::SeqCst);
            Answer::Status(500, json!({"error": "A denied child must never start"}))
        }
        _ => match request.tool_results().last() {
            Some(result) => Answer::text(tool_json(result).to_string()),
            None => {
                let spare = spare.lock().map(|value| value.clone()).unwrap_or_default();
                Answer::tool(
                    "send_message",
                    json!({"recipients": [spare], "text": "More work", "expects_reply": false}),
                )
            }
        },
    });
    let mut f = Fixture::start(model, Some(json!({"concurrentChildren": 1}))).await?;
    let parent = f.agent("Parent").await?;
    let child = f.agent("Child").await?;
    let spare = f.agent("Spare").await?;
    if let Ok(mut target) = spare_target.lock() {
        target.clone_from(&spare);
    }
    let (root, active, continuation) = (new_id(), new_id(), new_id());
    let (parent2, child2, root2, active2, continuation2) =
        (parent.clone(), child.clone(), root.clone(), active.clone(), continuation.clone());
    f.app
        .edit_database(move |db| {
            let created = "2026-11-19T07:33:20.000Z";
            let insert = "INSERT INTO turns(id,agent,parent,text,status,created) VALUES (?,?,?,?,?,?)";
            db.connection().execute(insert, (&root2, &parent2, None::<String>, "Original task", "done", created))?;
            db.connection().execute(insert, (&active2, &child2, Some(&root2), "Still working", "queued", created))?;
            db.connection().execute(insert, (&continuation2, &parent2, Some(&root2), "Completion input", "queued", created))?;
            Ok(())
        })
        .await?;
    let path = format!("/api/agents/{parent}/turns");
    // #87: over MCP the refusal returns to the model, which ends the turn.
    let done = f.settled(&path, &continuation).await?;
    ensure!(
        done["reply"]
            .as_str()
            .is_some_and(|reply| reply.contains("Concurrent child limit") && reply.contains("1 children")),
        "limit error {}",
        done["reply"]
    );
    ensure!(
        Inspector::open(f.app.database())?.count("turns")? == 3,
        "a denied child turn was stored"
    );
    ensure!(spare_started.load(Ordering::SeqCst) == 0, "denied child started");
    held.open();
    Ok(())
}
