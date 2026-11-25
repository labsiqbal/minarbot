//! Remote agents across two paired servers, ported from
//! `app/remote-agents.test.ts` (#89).
//!
//! Both servers ask one [`AsyncModel`], like the Node fixture's shared
//! provider; agents bind it with `runtime: "api"` (the gateway peer). Each
//! server has a [`MachineBridge`], as every Node server did, so peer HTTP
//! (including the remote MCP calls) flows through the bridge. The Node
//! cases that patched `globalThis.fetch` hook the same requests with a
//! bridge [`Extra`] operation.

use crate::pr_support::{call, command, first_machine, leaks, pause, restart_same_port};
use minarbot_testkit::{
    async_model::{AsyncModel, Gate, Step},
    bridge::BridgeError,
    machine_bridge::{peer_http_json, peer_http_request, Extra, MachineBridge},
    prelude::*,
    wait::until_within,
    Servers,
};
use std::{
    future::Future,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

const REMOTE_WAIT: Duration = Duration::from_secs(20);

struct Fixture {
    _servers: Servers,
    root_a: TestRoot,
    root_b: TestRoot,
    a: Server,
    b: Server,
    alpha: Value,
    beta: Value,
    machine: Value,
    _model: AsyncModel,
}

/// JSON tool results of a model request, in order. A non-JSON result fails
/// the script, which answers HTTP 500 like the Node `JSON.parse` throw.
fn tools(body: &Value) -> Result<Vec<Value>> {
    body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "tool")
        .map(|message| {
            Ok(serde_json::from_str(
                message["content"].as_str().unwrap_or("null"),
            )?)
        })
        .collect()
}

/// Product bug B1 (`docs/test-ledger.md`): a blocking native tool
/// (`ask_user`, and here `delegate_task` with `wait: true`) pauses the turn
/// and returns `{"status": "waiting"}` at once, so the harness takes one
/// more model step before the turn resumes with the real result. The
/// scripts answer that step like any other (the Node script would have);
/// counts leave it out and the strict count stays in the retained Node
/// case.
fn after_pause(body: &Value) -> bool {
    body["messages"]
        .as_array()
        .and_then(|messages| messages.last())
        .filter(|message| message["role"] == "tool")
        .and_then(|message| message["content"].as_str())
        .and_then(|content| serde_json::from_str::<Value>(content).ok())
        .is_some_and(|result| result["status"] == "waiting")
}

fn last_json(results: &[Value]) -> Step {
    Step::text(results.last().cloned().unwrap_or(Value::Null).to_string())
}

async fn start<F, Fut>(script: F, limits: Value, hook_a: Option<Extra>) -> Result<Fixture>
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Step>> + Send + 'static,
{
    let model = AsyncModel::start(script).await?;
    let servers = Servers::reserve(2).await?;
    let root_a = TestRoot::new()?;
    let root_b = TestRoot::new()?;
    let mut bridge_a = MachineBridge::new();
    if let Some(hook) = hook_a {
        bridge_a = bridge_a.extra(hook);
    }
    let bridge_a = bridge_a.bridge().await?;
    let bridge_b = MachineBridge::new().bridge().await?;
    let common = |bridge| {
        ServerConfig::new()
            .real_clock()
            .gateway(minarbot_testkit::server::Gateway::new(model.base_url()))
            .option("agentLimits", limits.clone())
            .computer_bridge(bridge)
    };
    let a = root_a.start_in(&servers, common(&bridge_a)).await?;
    let b = root_b.start_in(&servers, common(&bridge_b)).await?;
    call(
        &a,
        "/api/environment",
        Some(json!({"id": new_id(), "name": "A"})),
        200,
    )
    .await?;
    call(
        &b,
        "/api/environment",
        Some(json!({"id": new_id(), "name": "B"})),
        200,
    )
    .await?;
    let alpha = call(
        &a,
        "/api/agents",
        Some(
            json!({"runtime": "api", "id": new_id(), "name": "Alpha", "model": "Alpha",
            "instructions": "Help."}),
        ),
        201,
    )
    .await?;
    let beta = call(
        &b,
        "/api/agents",
        Some(json!({"runtime": "api", "id": new_id(), "name": "Beta",
            "role": "Personal agent", "model": "Beta", "instructions": "Help."})),
        201,
    )
    .await?;
    let code = call(&b, "/api/pairing-codes", Some(command()), 200).await?;
    let machine = call(
        &a,
        "/api/machines",
        Some(json!({"id": new_id(), "origin": b.origin(), "code": code["code"]})),
        200,
    )
    .await?;
    Ok(Fixture {
        _servers: servers,
        root_a,
        root_b,
        a,
        b,
        alpha,
        beta,
        machine,
        _model: model,
    })
}

impl Fixture {
    fn id(value: &Value) -> String {
        value["id"].as_str().unwrap_or_default().to_owned()
    }

    fn turns(&self) -> String {
        format!("/api/agents/{}/turns", Self::id(&self.alpha))
    }

    fn messages_path(&self) -> String {
        format!(
            "/api/remote-agents/messages?machine={}&agent={}",
            Self::id(&self.machine),
            Self::id(&self.beta)
        )
    }

    async fn messages(&self) -> Result<Vec<Value>> {
        Ok(
            call(&self.a, &self.messages_path(), None, 200).await?["messages"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
        )
    }

    async fn beta_threads(&self) -> Result<Vec<Value>> {
        let path = format!("/api/agents/{}/threads", Self::id(&self.beta));
        Ok(call(&self.b, &path, None, 200)
            .await?
            .as_array()
            .cloned()
            .unwrap_or_default())
    }

    /// First turn row matching `done` in A's Alpha turns.
    async fn wait_turn(&self, done: impl Fn(&Value) -> bool) -> Result<Value> {
        let path = self.turns();
        until_within("Alpha turn", REMOTE_WAIT, || async {
            let rows = call(&self.a, &path, None, 200).await?;
            Ok(rows
                .as_array()
                .and_then(|rows| rows.iter().find(|row| done(row)).cloned()))
        })
        .await
    }

    async fn wait_message(&self, done: impl Fn(&Value) -> bool) -> Result<Value> {
        until_within("remote message", REMOTE_WAIT, || async {
            Ok(self.messages().await?.into_iter().find(|row| done(row)))
        })
        .await
    }

    async fn send_message(&self, id: &str, text: &str) -> Result<Value> {
        call(
            &self.a,
            "/api/remote-agents/messages",
            Some(
                json!({"id": id, "machine": self.machine["id"], "agent": self.beta["id"],
                "text": text}),
            ),
            200,
        )
        .await
    }
}

async fn wait_count(label: &str, counter: &AtomicUsize, count: usize) -> Result<()> {
    until_within(label, REMOTE_WAIT, || async {
        Ok((counter.load(Ordering::SeqCst) == count).then_some(()))
    })
    .await
}

async fn paired_real_mcp_remote_agents(mode: &'static str) -> Result<()> {
    let held = Gate::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let wakes = Arc::new(AtomicUsize::new(0));
    let (gate, beta_calls, wake_count) = (held.clone(), calls.clone(), wakes.clone());
    let f = start(
        move |body| {
            let (gate, calls, wakes) = (gate.clone(), beta_calls.clone(), wake_count.clone());
            async move {
                if body["model"] == "Beta" {
                    calls.fetch_add(1, Ordering::SeqCst);
                    gate.wait().await;
                    return Ok(Step::text("Remote Beta reply"));
                }
                if body["messages"]
                    .to_string()
                    .contains("Delegated task completed")
                {
                    wakes.fetch_add(1, Ordering::SeqCst);
                    return Ok(Step::text("Completion received"));
                }
                let results = tools(&body)?;
                let Some(last) = results.last() else {
                    return Ok(if mode == "message" {
                        Step::tool(
                            "send_message",
                            json!({"recipients": ["Beta@B"], "text": "Remote work",
                                "expects_reply": true}),
                        )
                    } else {
                        Step::tool(
                            "delegate_task",
                            json!({"agent": "Beta@B", "task": "Remote work",
                                "wait": mode != "completion" && mode != "wait"}),
                        )
                    });
                };
                if mode == "wait" && last["status"] == "queued" {
                    return Ok(Step::tool(
                        "wait_task",
                        json!({"taskId": last["taskId"], "timeoutSeconds": 600}),
                    ));
                }
                Ok(last_json(&results))
            }
        },
        json!({}),
        None,
    )
    .await?;
    let mut f = f;
    let result = async {
        let id = new_id();
        let path = f.turns();
        call(&f.a, &path, Some(json!({"id": id, "text": "Start"})), 202).await?;
        wait_count("Beta model call", &calls, 1).await?;
        let inbound = until_within("inbound peer turn", REMOTE_WAIT, || async {
            let threads = f.beta_threads().await?;
            let Some(thread) = threads.first() else {
                return Ok(None);
            };
            let rows = call(
                &f.b,
                &format!("/api/threads/{}/turns", Fixture::id(thread)),
                None,
                200,
            )
            .await?;
            Ok(rows.as_array().and_then(|rows| {
                rows.iter()
                    .find(|row| row["origin"] == "peer:Minarbot: A")
                    .cloned()
            }))
        })
        .await?;
        ensure!(inbound["message_depth"] == 1, "inbound depth: {inbound}");
        let inbound_threads = f.beta_threads().await?;
        ensure!(
            inbound_threads[0]["delivery"] == true,
            "inbound thread: {}",
            inbound_threads[0]
        );
        ensure!(
            inbound_threads[0]["name"] == "Minarbot on A: Remote work",
            "inbound thread name: {}",
            inbound_threads[0]
        );
        let outbound = call(
            &f.a,
            &format!("/api/agents/{}/threads", Fixture::id(&f.alpha)),
            None,
            200,
        )
        .await?;
        ensure!(
            outbound[0]["delivery"] == true,
            "outbound thread: {outbound}"
        );
        ensure!(
            outbound[0]["name"] == "Beta on B: Remote work",
            "outbound thread name: {outbound}"
        );
        ensure!(
            f.machine["agents"][0]["role"] == "Personal agent",
            "remote role: {}",
            f.machine
        );
        f.wait_turn(|row| {
            row["id"] == id.as_str()
                && row["agentMessages"]
                    .as_array()
                    .is_some_and(|messages| !messages.is_empty())
        })
        .await?;
        if mode == "restart-a" {
            restart_same_port(&mut f.a).await?;
        }
        if mode == "restart-b" {
            restart_same_port(&mut f.b).await?;
        }
        held.open();
        let row = f
            .wait_turn(|row| {
                if mode == "completion" {
                    row["reply"] == "Completion received"
                } else {
                    row["id"] == id.as_str() && row["status"] == "done"
                }
            })
            .await?;
        let reply = row["reply"].as_str().unwrap_or_default();
        if mode == "completion" {
            ensure!(wakes.load(Ordering::SeqCst) == 1, "completion wakes");
            restart_same_port(&mut f.a).await?;
            let rows = call(&f.a, &path, None, 200).await?;
            let completions = rows
                .as_array()
                .into_iter()
                .flatten()
                .filter(|row| row["reply"] == "Completion received")
                .count();
            ensure!(completions == 1, "completion replayed: {completions}");
        } else if mode == "restart-b" {
            ensure!(reply.contains("uncertain"), "restart-b reply: {reply}");
        } else {
            ensure!(reply.contains("Remote Beta reply"), "reply: {reply}");
        }
        ensure!(
            calls.load(Ordering::SeqCst) == 1,
            "Beta ran {} times",
            calls.load(Ordering::SeqCst)
        );
        let name = row["agentMessages"][0]["name"]
            .as_str()
            .unwrap_or("Beta on B");
        ensure!(name == "Beta on B", "agent message name: {name}");
        Ok(())
    }
    .await;
    held.open();
    result
}

#[tokio::test]
async fn paired_real_mcp_remote_agents_message() -> Result<()> {
    paired_real_mcp_remote_agents("message").await
}

#[tokio::test]
async fn paired_real_mcp_remote_agents_wait() -> Result<()> {
    paired_real_mcp_remote_agents("wait").await
}

#[tokio::test]
async fn paired_real_mcp_remote_agents_completion() -> Result<()> {
    paired_real_mcp_remote_agents("completion").await
}

#[tokio::test]
async fn paired_real_mcp_remote_agents_restart_a() -> Result<()> {
    paired_real_mcp_remote_agents("restart-a").await
}

#[tokio::test]
async fn paired_real_mcp_remote_agents_restart_b() -> Result<()> {
    paired_real_mcp_remote_agents("restart-b").await
}

#[tokio::test]
async fn remote_cancel_stops_peer_delivery_is_peer_scoped_and_unpaired_addresses_are_unreachable(
) -> Result<()> {
    let held = Gate::new();
    let child_calls = Arc::new(AtomicUsize::new(0));
    let (gate, children) = (held.clone(), child_calls.clone());
    let f = start(
        move |body| {
            let (gate, children) = (gate.clone(), children.clone());
            async move {
                if body["model"] == "Beta" {
                    children.fetch_add(1, Ordering::SeqCst);
                    gate.wait().await;
                    return Ok(Step::text("Too late"));
                }
                let results = tools(&body)?;
                match results.len() {
                    0 => Ok(Step::tool(
                        "delegate_task",
                        json!({"agent": "Beta@B", "task": "Cancel work"}),
                    )),
                    1 => {
                        wait_count("Beta started", &children, 1).await?;
                        Ok(Step::tool(
                            "cancel_task",
                            json!({"taskId": results[0]["taskId"]}),
                        ))
                    }
                    _ => Ok(last_json(&results)),
                }
            }
        },
        json!({}),
        None,
    )
    .await?;
    let result = async {
        let id = new_id();
        call(
            &f.a,
            &f.turns(),
            Some(json!({"id": id, "text": "Cancel"})),
            202,
        )
        .await?;
        let row = f
            .wait_turn(|row| row["id"] == id.as_str() && row["status"] == "done")
            .await?;
        let reply = row["reply"].as_str().unwrap_or_default();
        ensure!(reply.contains("cancelled"), "cancel reply: {reply}");
        until_within("remote turn cancelled", REMOTE_WAIT, || async {
            let threads = f.beta_threads().await?;
            let Some(thread) = threads.first() else {
                return Ok(None);
            };
            let rows = call(
                &f.b,
                &format!("/api/threads/{}/turns", Fixture::id(thread)),
                None,
                200,
            )
            .await?;
            Ok((rows[0]["status"] == "cancelled").then_some(()))
        })
        .await?;
        call(
            &f.a,
            "/api/remote-agents/messages",
            Some(
                json!({"id": new_id(), "machine": new_id(), "agent": f.beta["id"],
                "text": "Unpaired"}),
            ),
            409,
        )
        .await?;
        Ok(())
    }
    .await;
    held.open();
    result
}

#[tokio::test]
async fn kill_switch_rejects_immediately_and_loop_limits_count_peer_and_agent_across_requests(
) -> Result<()> {
    let mut f = start(
        |_| async { Ok(Step::text("Reply")) },
        json!({"rate": 2, "depth": 2}),
        None,
    )
    .await?;
    let settled = |row: &Value| {
        ["done", "failed", "interrupted"]
            .iter()
            .any(|status| row["status"] == *status)
    };
    let back = first_machine(&f.b).await?;
    ensure!(
        back["allowAgentMessaging"] == true,
        "default policy: {back}"
    );
    call(
        &f.b,
        "/api/machines/agent-messaging",
        Some(json!({"id": new_id(), "environmentId": back["id"], "allowed": false})),
        200,
    )
    .await?;
    f.a.refresh_machines().await?;
    ensure!(
        first_machine(&f.a).await?["remoteAgentMessaging"] == false,
        "A did not learn the kill switch"
    );
    let send = |text: &'static str| {
        let f = &f;
        async move {
            let id = new_id();
            f.send_message(&id, text).await?;
            f.wait_message(|row| row["id"] == id.as_str() && settled(row))
                .await
        }
    };
    let blocked = send("Blocked").await?;
    ensure!(blocked["status"] == "failed", "blocked: {blocked}");
    ensure!(
        blocked["error"]
            .as_str()
            .unwrap_or_default()
            .contains("disabled"),
        "blocked error: {blocked}"
    );
    ensure!(
        f.beta_threads().await?.is_empty(),
        "blocked message delivered"
    );
    call(
        &f.b,
        "/api/machines/agent-messaging",
        Some(json!({"id": new_id(), "environmentId": back["id"], "allowed": true})),
        200,
    )
    .await?;
    f.a.refresh_machines().await?;
    ensure!(
        first_machine(&f.a).await?["remoteAgentMessaging"] == true,
        "A did not learn the re-enable"
    );
    ensure!(send("One").await?["status"] == "done", "first message");
    ensure!(send("Two").await?["status"] == "done", "second message");
    let rate = send("Three").await?;
    ensure!(rate["status"] == "failed", "rate: {rate}");
    ensure!(
        rate["error"]
            .as_str()
            .unwrap_or_default()
            .contains("rate limit"),
        "rate error: {rate}"
    );
    restart_same_port(&mut f.b).await?;
    let id = new_id();
    f.send_message(&id, "Four").await?;
    let cool = f
        .wait_message(|row| row["id"] == id.as_str() && settled(row))
        .await?;
    ensure!(cool["status"] == "failed", "cool-down: {cool}");
    ensure!(
        cool["error"]
            .as_str()
            .unwrap_or_default()
            .contains("rate limit"),
        "cool-down error: {cool}"
    );
    Ok(())
}

#[tokio::test]
async fn delegation_depth_travels_across_machines_and_stops_ping_pong() -> Result<()> {
    let requests = Arc::new(AtomicUsize::new(0));
    let counter = requests.clone();
    let f = start(
        move |body| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let results = tools(&body)?;
                if !results.is_empty() {
                    return Ok(last_json(&results));
                }
                let agent = if body["model"] == "Alpha" {
                    "Beta@B"
                } else {
                    "Alpha@A"
                };
                Ok(Step::tool(
                    "delegate_task",
                    json!({"agent": agent, "task": "Continue", "wait": true}),
                ))
            }
        },
        json!({"depth": 2, "rate": 10}),
        None,
    )
    .await?;
    f.b.refresh_machines().await?;
    let id = new_id();
    call(
        &f.a,
        &f.turns(),
        Some(json!({"id": id, "text": "Start loop"})),
        202,
    )
    .await?;
    let row = f
        .wait_turn(|row| row["id"] == id.as_str() && row["status"] == "done")
        .await?;
    let reply = row["reply"].as_str().unwrap_or_default();
    ensure!(reply.contains("depth"), "depth reply: {reply}");
    let work = f
        ._model
        .requests()
        .iter()
        .filter(|body| !after_pause(body))
        .count();
    ensure!(work == 6, "model requests: {work}");
    Ok(())
}

#[tokio::test]
async fn offline_send_remains_explicitly_uncertain_across_caller_restart_without_replay(
) -> Result<()> {
    let requests = Arc::new(AtomicUsize::new(0));
    let counter = requests.clone();
    let mut f = start(
        move |_| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(Step::text("Reply"))
            }
        },
        json!({}),
        None,
    )
    .await?;
    f.b.shutdown().await?;
    let id = new_id();
    f.send_message(&id, "Offline").await?;
    let uncertain = f
        .wait_message(|row| row["id"] == id.as_str() && row["state"] == "uncertain")
        .await?;
    ensure!(
        uncertain["error"]
            .as_str()
            .unwrap_or_default()
            .contains("uncertain"),
        "offline error: {uncertain}"
    );
    restart_same_port(&mut f.a).await?;
    f.send_message(&id, "Offline").await?;
    pause(500).await;
    ensure!(
        requests.load(Ordering::SeqCst) == 0,
        "offline message reached a model"
    );
    Ok(())
}

/// Bridge hook: the first `send_message` MCP call to `target` is sent and
/// answered, then reported to the server as a lost acknowledgement. With
/// `hold`, the loss is reported only after `release` opens.
fn lose_send_ack(
    target: Arc<Mutex<String>>,
    armed: Arc<Mutex<bool>>,
    accepted: Gate,
    release: Option<Gate>,
) -> Extra {
    Arc::new(move |operation: &str, args: &Value| {
        if operation != "peer-http" {
            return None;
        }
        let url = args["url"].as_str().unwrap_or_default().to_owned();
        let body: Value = args["body"]
            .as_str()
            .and_then(|body| serde_json::from_str(body).ok())
            .unwrap_or(Value::Null);
        let expected = format!("{}/mcp", locked(&target));
        let matches = url == expected
            && body["method"] == "tools/call"
            && body["params"]["name"] == "send_message";
        if !matches || !*locked(&armed) {
            return None;
        }
        *locked(&armed) = false;
        let (args, accepted, release) = (args.clone(), accepted.clone(), release.clone());
        Some(Box::pin(async move {
            let _ = peer_http_request(&args).await;
            accepted.open();
            if let Some(release) = release {
                release.wait().await;
            }
            Err(BridgeError::new(503, "Fixture lost acknowledgement"))
        }))
    })
}

fn locked<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

async fn accepted_remote_send_is_never_replayed(mode: &'static str) -> Result<()> {
    let requests = Arc::new(AtomicUsize::new(0));
    let counter = requests.clone();
    let target = Arc::new(Mutex::new(String::new()));
    let armed = Arc::new(Mutex::new(false));
    let accepted = Gate::new();
    let held = Gate::new();
    let hook = lose_send_ack(
        target.clone(),
        armed.clone(),
        accepted.clone(),
        (mode == "restart-sending").then(|| held.clone()),
    );
    let mut f = start(
        move |_| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(Step::text("Accepted once"))
            }
        },
        json!({}),
        Some(hook),
    )
    .await?;
    *locked(&target) = f.b.origin().to_owned();
    *locked(&armed) = true;
    let result = async {
        let id = new_id();
        f.send_message(&id, "Once").await?;
        tokio::time::timeout(REMOTE_WAIT, accepted.wait())
            .await
            .map_err(|_| "send_message never reached B")?;
        if mode == "restart-sending" {
            let port = crate::pr_support::port(&f.a)?;
            let config = f.a.config().clone().option("port", port);
            let release = held.clone();
            let (restart, ()) = tokio::join!(f.a.restart_with(config), async move {
                pause(50).await;
                release.open();
            });
            restart?;
        } else {
            f.wait_message(|row| row["id"] == id.as_str() && row["state"] == "uncertain")
                .await?;
        }
        *locked(&armed) = false;
        restart_same_port(&mut f.a).await?;
        f.send_message(&id, "Once").await?;
        pause(500).await;
        ensure!(
            requests.load(Ordering::SeqCst) == 1,
            "remote send replayed: {} model calls",
            requests.load(Ordering::SeqCst)
        );
        ensure!(
            f.beta_threads().await?.len() == 1,
            "remote send delivered twice"
        );
        let messages = f.messages().await?;
        ensure!(
            messages[0]["state"] == "uncertain",
            "lost acknowledgement state: {}",
            messages[0]
        );
        Ok(())
    }
    .await;
    held.open();
    result
}

#[tokio::test]
async fn accepted_remote_send_is_never_replayed_lost_ack() -> Result<()> {
    accepted_remote_send_is_never_replayed("lost-ack").await
}

#[tokio::test]
async fn accepted_remote_send_is_never_replayed_restart_sending() -> Result<()> {
    accepted_remote_send_is_never_replayed("restart-sending").await
}

#[tokio::test]
async fn peer_tokens_never_enter_database_bytes_logs_or_outbound_message_responses() -> Result<()> {
    let tokens = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = tokens.clone();
    let capture: Extra = Arc::new(move |operation: &str, args: &Value| {
        if operation != "peer-http" {
            return None;
        }
        let url = args["url"].as_str().unwrap_or_default().to_owned();
        if url.ends_with("/api/machines/reciprocal") {
            let body: Value = args["body"]
                .as_str()
                .and_then(|body| serde_json::from_str(body).ok())
                .unwrap_or(Value::Null);
            if let Some(token) = body["token"].as_str() {
                locked(&seen).push(token.to_owned());
            }
        }
        if !url.ends_with("/api/pairing/redeem") {
            return None;
        }
        let (args, seen) = (args.clone(), seen.clone());
        Some(Box::pin(async move {
            let reply = peer_http_request(&args)
                .await
                .map_err(|error| BridgeError::new(503, error.to_string()))?;
            if let Some(token) = peer_http_json(&reply)["token"].as_str() {
                locked(&seen).push(token.to_owned());
            }
            Ok(reply)
        }))
    });
    let mut f = start(
        |_| async { Ok(Step::text("Safe reply")) },
        json!({}),
        Some(capture),
    )
    .await?;
    let id = new_id();
    f.send_message(&id, "Canary").await?;
    let response = f.wait_message(|row| row["status"] == "done").await?;
    let tokens = locked(&tokens).clone();
    ensure!(tokens.len() == 2, "captured tokens: {}", tokens.len());
    for token in &tokens {
        ensure!(
            !response.to_string().contains(token.as_str()),
            "message response leaked a peer token"
        );
    }
    f.a.shutdown().await?;
    f.b.shutdown().await?;
    for root in [&f.root_a, &f.root_b] {
        let found = leaks(&root.path().join("logs"), &tokens)?;
        ensure!(found.is_empty(), "token in server logs: {found:?}");
        let found = leaks(root.path(), &tokens)?;
        ensure!(found.is_empty(), "Token leaked into {found:?}");
    }
    Ok(())
}

#[tokio::test]
async fn kill_switch_cancels_an_active_peer_turn_before_any_more_remote_work_is_admitted(
) -> Result<()> {
    let held = Gate::new();
    let started = Gate::new();
    let (gate, start_gate) = (held.clone(), started.clone());
    let f = start(
        move |_| {
            let (gate, start_gate) = (gate.clone(), start_gate.clone());
            async move {
                start_gate.open();
                gate.wait().await;
                Ok(Step::text("Old work"))
            }
        },
        json!({}),
        None,
    )
    .await?;
    let result = async {
        let id = new_id();
        f.send_message(&id, "Running").await?;
        tokio::time::timeout(REMOTE_WAIT, started.wait())
            .await
            .map_err(|_| "peer turn never started")?;
        let peer = first_machine(&f.b).await?;
        call(
            &f.b,
            "/api/machines/agent-messaging",
            Some(json!({"id": new_id(), "environmentId": peer["id"], "allowed": false})),
            200,
        )
        .await?;
        let row = f
            .wait_message(|row| row["id"] == id.as_str() && row["status"] == "cancelled")
            .await?;
        ensure!(
            row["error"]
                .as_str()
                .unwrap_or_default()
                .contains("disabled"),
            "cancel reason: {row}"
        );
        Ok(())
    }
    .await;
    held.open();
    result
}

#[tokio::test]
async fn remote_role_reaches_local_discovery_and_team_prompt_through_paired_peer_summaries(
) -> Result<()> {
    let system = Arc::new(Mutex::new(String::new()));
    let seen = system.clone();
    let f = start(
        move |body| {
            let seen = seen.clone();
            async move {
                *locked(&seen) = body["messages"][0]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                let results = tools(&body)?;
                Ok(if results.is_empty() {
                    Step::tool("list_agents", json!({}))
                } else {
                    last_json(&results)
                })
            }
        },
        json!({}),
        None,
    )
    .await?;
    let id = new_id();
    call(
        &f.a,
        &f.turns(),
        Some(json!({"id": id, "text": "List teammates"})),
        202,
    )
    .await?;
    let row = f
        .wait_turn(|row| row["id"] == id.as_str() && row["status"] == "done")
        .await?;
    let reply: Value = serde_json::from_str(row["reply"].as_str().unwrap_or("null"))?;
    let remote_id = format!("{}@{}", Fixture::id(&f.beta), Fixture::id(&f.machine));
    let remote = reply["agents"]
        .as_array()
        .and_then(|agents| {
            agents
                .iter()
                .find(|agent| agent["id"] == remote_id.as_str())
        })
        .cloned()
        .ok_or_else(|| format!("remote agent missing from {reply}"))?;
    ensure!(remote["role"] == "Personal agent", "remote role: {remote}");
    ensure!(
        remote["description"]
            .as_str()
            .is_some_and(|description| !description.is_empty()),
        "remote description: {remote}"
    );
    let system = locked(&system).clone();
    ensure!(
        system.contains("Beta on B - Personal agent"),
        "team prompt: {system}"
    );
    Ok(())
}
