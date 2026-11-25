//! Local effects behind hub turns: computer input, connected apps, MCP
//! tools and unsupported engines.

use super::fixture::{call, has_tool, hub_error, locked, script, until, Hub};
use crate::pr_support::pause;
use minarbot_testkit::{
    async_model::{Gate, Step},
    bridge::{Bridge, BridgeError},
    db::{Fixture as Writable, Inspector},
    machine_bridge::Extra,
    prelude::*,
    relay::{Answer, Relay},
};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};

/// Removes a scratch directory on drop.
struct DirGuard(std::path::PathBuf);

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Desktop runtime fixture answered through the computer bridge (the
/// Node `desktop.runtime`): launches succeed, input counts as an effect
/// and can be held, capture returns an empty frame.
fn desktop(effects: Arc<AtomicUsize>, hold: Arc<AtomicBool>, release: Gate) -> Extra {
    let exited: Arc<Mutex<std::collections::HashMap<String, bool>>> = Arc::default();
    Arc::new(move |operation: &str, args: &Value| {
        if !operation.starts_with("desktop-") {
            return None;
        }
        let id = args["id"].as_str().unwrap_or_default().to_owned();
        let (effects, hold, release, exited) = (
            effects.clone(),
            hold.clone(),
            release.clone(),
            exited.clone(),
        );
        let operation = operation.to_owned();
        Some(Box::pin(async move {
            match operation.as_str() {
                "desktop-launch" => {
                    locked(&exited).insert(id, false);
                    Ok(Value::Null)
                }
                "desktop-status" => Ok(json!(locked(&exited)
                    .get(&id)
                    .copied()
                    .ok_or_else(|| BridgeError::new(503, "Desktop fixture missing."))?)),
                "desktop-stop" => {
                    locked(&exited).remove(&id);
                    Ok(Value::Null)
                }
                "desktop-input" => {
                    effects.fetch_add(1, Ordering::SeqCst);
                    if hold.load(Ordering::SeqCst) {
                        release.wait().await;
                    }
                    Ok(json!(""))
                }
                "desktop-capture" => Ok(json!("")),
                _ => Err(BridgeError::new(503, "Unknown desktop fixture operation.")),
            }
        }))
    })
}

#[tokio::test]
async fn computer_input_requires_local_approval_and_action_lease_revoke_leaves_uncertain_input_without_replay(
) -> Result<()> {
    let effects = Arc::new(AtomicUsize::new(0));
    let hold = Arc::new(AtomicBool::new(false));
    let release = Gate::new();
    let click = json!({"id": "hub-computer-click", "type": "function",
        "function": {"name": "computer_click", "arguments": "{\"x\":12,\"y\":24}"}});
    let f = Hub::start(
        1,
        Some(script(move |body| {
            let click = click.clone();
            async move {
                Ok(if has_tool(&body) {
                    Step::text("Computer checked.")
                } else {
                    Step::RawTool(click)
                })
            }
        })),
        false,
    )
    .await?;
    // The lease directory holds a Unix socket; keep its path under the
    // 108-byte socket limit, as the Node suite's short temporary folder did.
    let short = std::env::temp_dir().join(format!("mb-hub-{}", &new_id()[..8]));
    std::fs::create_dir_all(&short)?;
    let _lease_guard = DirGuard(short.clone());
    let (computer, leases) = (f.root.dir("computer")?, short.join("leases"));
    let mut node = f
        .node_with_ops(
            "ComputerBoundary",
            &f.alice,
            Some(desktop(effects.clone(), hold.clone(), release.clone())),
            move |config| {
                config.option(
                    "desktop",
                    json!({"dataDir": computer, "leaseDir": leases, "enabled": true,
                        "testRuntime": true}),
                )
            },
        )
        .await?;
    call(
        &node.app,
        &format!("/api/agents/{}/computer", node.local_agent()),
        Some(json!({"id": new_id(), "revision": 0, "enabled": true})),
        200,
    )
    .await?;
    let p = f.owner(&node).await?;
    let first = f.browser_post(&f.alice, &[&p.participation_id]).await?;
    let result = async {
        until("computer owner approval", || async {
            Ok(node.approvals()?.len() == 1)
        })
        .await?;
        ensure!(effects.load(Ordering::SeqCst) == 0, "input before approval");
        let approval = node.approvals()?[0].clone();
        ensure!(
            approval["payload"]["function"]["name"] == "computer_click",
            "approval: {approval}"
        );
        approve(&node, &approval).await?;
        let results = f.done(&first[0], None).await?;
        ensure!(
            effects.load(Ordering::SeqCst) == 1,
            "effects: {} model {:?} tools {:?} stderr {}",
            effects.load(Ordering::SeqCst),
            f.model
                .requests()
                .last()
                .map(|r| r["messages"].as_array().and_then(|m| m.last()).cloned()),
            Inspector::open(node.app.database())?
                .rows("SELECT call,result FROM flow_tool_calls", [])?,
            node.app.output()?.1
        );
        ensure!(
            results
                .iter()
                .any(|result| result["kind"] == "turn.effect.receipt"
                    && result["payload"]["actionId"] == approval["actionId"]
                    && result["payload"]["outcome"] == "succeeded"),
            "receipt: {results:?}"
        );
        ensure!(
            f.one(
                "SELECT 1 FROM action_leases WHERE action_id=?",
                [approval["actionId"].as_str().unwrap_or_default()]
            )?
            .is_some(),
            "no action lease"
        );
        hold.store(true, Ordering::SeqCst);
        let second = f.post(&f.alice, &[&p.participation_id]).await?;
        until("second approval", || async {
            Ok(node.approvals()?.len() == 1)
        })
        .await?;
        let pending = node.approvals()?[0].clone();
        approve(&node, &pending).await?;
        until("computer effect dispatched", || async {
            Ok(effects.load(Ordering::SeqCst) == 2)
        })
        .await?;
        f.withdraw(&p.participation_id).await?;
        until("revoked input uncertain", || async {
            Ok(node.approvals()?.is_empty() && f.turn_state(&second[0])? == "unknown")
        })
        .await?;
        release.open();
        node.restart().await?;
        pause(200).await;
        ensure!(effects.load(Ordering::SeqCst) == 2, "input replayed");
        node.stop().await?;
        let state = Inspector::open(node.app.database())?.row(
            "SELECT state FROM hub_link_actions WHERE action_id=?",
            [pending["actionId"].as_str().unwrap_or_default()],
        )?;
        ensure!(state["state"] == "unknown", "action: {state}");
        let again = f.post(&f.alice, &[&p.participation_id]).await;
        ensure!(
            again.as_ref().is_err_and(|error| hub_error(error, None)),
            "post after revoke: {again:?}"
        );
        Ok(())
    }
    .await;
    release.open();
    result
}

async fn approve(node: &super::fixture::Node, approval: &Value) -> Result<()> {
    call(
        &node.app,
        "/api/hub-link/approvals",
        Some(
            json!({"actionId": approval["actionId"], "actionDigest": approval["actionDigest"],
            "approved": true}),
        ),
        200,
    )
    .await?;
    Ok(())
}

/// `composioToolName`: `composio__` plus 24 hex of SHA-256 of the JSON slug.
fn composio_tool_name(slug: &str) -> String {
    let digest = minarbot_testkit::protocol::sha256_hex(json!(slug).to_string());
    format!("composio__{}", &digest[..24])
}

#[tokio::test]
async fn connected_app_calls_require_local_approval_and_lease_revocation_after_dispatch_is_uncertain_and_never_replayed(
) -> Result<()> {
    let effects = Arc::new(AtomicUsize::new(0));
    let lose = Arc::new(AtomicBool::new(false));
    let release = Gate::new();
    let (count, losing, gate) = (effects.clone(), lose.clone(), release.clone());
    let remote = Relay::start(move |exchange| {
        let (count, losing, gate) = (count.clone(), losing.clone(), gate.clone());
        async move {
            let path = exchange
                .path
                .split('?')
                .next()
                .unwrap_or_default()
                .to_owned();
            Ok(if path.ends_with("/toolkits/categories") {
                Answer::json(200, &json!({"items": []}))
            } else if path.ends_with("/connected_accounts") {
                Answer::json(
                    200,
                    &json!({"items": [{"id": "fixture-account",
                    "toolkit": {"slug": "gmail"}, "status": "ACTIVE"}]}),
                )
            } else if path.ends_with("/tools") {
                Answer::json(
                    200,
                    &json!({"items": [{"slug": "GMAIL_SEND_EMAIL", "name": "Send email",
                    "description": "Synthetic write", "toolkit": {"slug": "gmail"},
                    "version": "20261113_00", "input_parameters": {"type": "object",
                        "properties": {"target": {"type": "string"}}, "required": ["target"],
                        "additionalProperties": false}, "tags": ["createHint"]}]}),
                )
            } else if path.ends_with("/tools/execute/GMAIL_SEND_EMAIL") {
                count.fetch_add(1, Ordering::SeqCst);
                if losing.load(Ordering::SeqCst) {
                    gate.wait().await;
                    Answer::Drop
                } else {
                    Answer::json(
                        200,
                        &json!({"successful": true, "data": {"fixture": "sent"}}),
                    )
                }
            } else {
                Answer::json(404, &json!({}))
            })
        }
    })
    .await?;
    let tool = json!({"id": "connected-write", "type": "function", "function": {
        "name": composio_tool_name("GMAIL_SEND_EMAIL"), "arguments": "{\"target\":\"synthetic@example.test\"}"}});
    let f = Hub::start(
        1,
        Some(script(move |body| {
            let tool = tool.clone();
            async move {
                Ok(if has_tool(&body) {
                    Step::text("Connected app completed.")
                } else {
                    Step::RawTool(tool)
                })
            }
        })),
        false,
    )
    .await?;
    let key = Bridge::composio_key("").await?;
    let base = format!("{}/api/v3.1", remote.origin());
    let mut node = f
        .node_with_ops("ConnectedBoundary", &f.alice, None, move |config| {
            config.composio(&base, &key)
        })
        .await?;
    call(
        &node.app,
        "/api/connected-apps/key",
        Some(json!({"id": new_id(), "revision": 0, "key": "synthetic-composio-fixture-only"})),
        200,
    )
    .await?;
    call(
        &node.app,
        &format!("/api/agents/{}/connected-apps/gmail", node.local_agent()),
        Some(json!({"id": new_id(), "revision": 0, "enabled": true})),
        200,
    )
    .await?;
    let p = f.owner(&node).await?;
    let result = async {
        let threads_path = format!("/api/agents/{}/threads", node.local_agent());
        let run = |turn: String| {
            let (node, threads_path, effects) = (&node, threads_path.clone(), effects.clone());
            async move {
                until("hub tool approval", || async { Ok(node.approvals()?.len() == 1) }).await?;
                let approval = node.approvals()?[0].clone();
                ensure!(effects.load(Ordering::SeqCst) < 2, "effect before approval");
                approve(node, &approval).await?;
                // Local app approval remains separate from hub action approval.
                let threads = call(&node.app, &threads_path, None, 200).await?;
                let turns = format!(
                    "/api/threads/{}/turns",
                    threads[0]["id"].as_str().unwrap_or_default()
                );
                let mut local = Value::Null;
                until("connected app approval", || async {
                    let rows = call(&node.app, &turns, None, 200).await?;
                    let row = rows
                        .as_array()
                        .and_then(|rows| rows.iter().find(|row| row["id"] == turn.as_str()).cloned());
                    let ready = row.as_ref().is_some_and(|row| {
                        row["approvals"]
                            .as_array()
                            .is_some_and(|approvals| approvals.iter().any(|a| a["state"] == "approval"))
                    });
                    Ok(ready)
                })
                .await?;
                let rows = call(&node.app, &turns, None, 200).await?;
                if let Some(row) = rows.as_array().and_then(|rows| rows.iter().find(|row| row["id"] == turn.as_str())) {
                    local = row.clone();
                }
                let app_approval = local["approvals"][0].clone();
                call(
                    &node.app,
                    &format!("{turns}/{turn}/app-approvals/{}", app_approval["id"].as_str().unwrap_or_default()),
                    Some(json!({"id": new_id(), "revision": app_approval["revision"], "decision": "allow"})),
                    200,
                )
                .await?;
                until("fresh resume lease approval", || async { Ok(node.approvals()?.len() == 1) }).await?;
                let resume = node.approvals()?[0].clone();
                approve(node, &resume).await?;
                Ok::<Value, minarbot_testkit::Error>(resume)
            }
        };
        let first = f.post(&f.alice, &[&p.participation_id]).await?;
        ensure!(effects.load(Ordering::SeqCst) == 0, "effect before any approval");
        let resume = run(first[0].clone()).await?;
        f.done(&first[0], None).await?;
        ensure!(effects.load(Ordering::SeqCst) == 1, "effects: {}", effects.load(Ordering::SeqCst));
        ensure!(
            f.one("SELECT 1 FROM action_leases WHERE action_id=?", [resume["actionId"].as_str().unwrap_or_default()])?
                .is_some(),
            "no resume lease"
        );
        lose.store(true, Ordering::SeqCst);
        let second = f.post(&f.alice, &[&p.participation_id]).await?;
        run(second[0].clone()).await?;
        until("second effect", || async { Ok(effects.load(Ordering::SeqCst) == 2) }).await?;
        f.withdraw(&p.participation_id).await?;
        release.open();
        until("uncertain after revoke", || async {
            Ok(f.turn_state(&second[0])? == "unknown")
        })
        .await?;
        node.restart().await?;
        pause(200).await;
        ensure!(effects.load(Ordering::SeqCst) == 2, "connected app call replayed");
        let again = f.post(&f.alice, &[&p.participation_id]).await;
        ensure!(again.as_ref().is_err_and(|error| hub_error(error, None)), "post after revoke: {again:?}");
        Ok(())
    }
    .await;
    release.open();
    result
}

#[tokio::test]
async fn mcp_async_discovery_cannot_dispatch_after_revoke_lost_result_remains_uncertain_across_restart(
) -> Result<()> {
    let effects = Arc::new(AtomicUsize::new(0));
    let hold_catalogue = Arc::new(AtomicBool::new(false));
    let held = Arc::new(AtomicBool::new(false));
    let lose_result = Arc::new(AtomicBool::new(false));
    let release = Gate::new();
    let (count, holding, was_held, losing, gate) = (
        effects.clone(),
        hold_catalogue.clone(),
        held.clone(),
        lose_result.clone(),
        release.clone(),
    );
    let remote = Relay::start(move |exchange| {
        let (count, holding, was_held, losing, gate) = (
            count.clone(),
            holding.clone(),
            was_held.clone(),
            losing.clone(),
            gate.clone(),
        );
        async move {
            match exchange.method.as_str() {
                "GET" => return Ok(Answer::empty(405, Vec::new())),
                "DELETE" => return Ok(Answer::empty(200, Vec::new())),
                _ => {}
            }
            let message = exchange.json();
            if message.get("id").is_none() {
                return Ok(Answer::empty(202, Vec::new()));
            }
            let result = match message["method"].as_str().unwrap_or_default() {
                "initialize" => {
                    json!({"protocolVersion": "2025-03-26", "capabilities": {"tools": {}},
                    "serverInfo": {"name": "hub-effect-fixture", "version": "1"}})
                }
                "tools/list" => {
                    if holding.load(Ordering::SeqCst) {
                        was_held.store(true, Ordering::SeqCst);
                        gate.wait().await;
                    }
                    json!({"tools": [{"name": "change", "inputSchema": {"type": "object",
                        "properties": {"value": {"type": "string"}}, "required": ["value"]}}]})
                }
                "tools/call" => {
                    count.fetch_add(1, Ordering::SeqCst);
                    if losing.load(Ordering::SeqCst) {
                        return Ok(Answer::Drop);
                    }
                    json!({"content": [{"type": "text", "text": "Saved synthetic change."}]})
                }
                _ => return Ok(Answer::empty(400, Vec::new())),
            };
            Ok(Answer::json(
                200,
                &json!({"jsonrpc": "2.0", "id": message["id"], "result": result}),
            ))
        }
    })
    .await?;
    let connection = Arc::new(Mutex::new(String::new()));
    let chosen = connection.clone();
    let f = Hub::start(
        1,
        Some(script(move |body| {
            let connection = locked(&chosen).clone();
            async move {
                Ok(if has_tool(&body) {
                    Step::text("MCP completed.")
                } else {
                    Step::RawTool(json!({"id": "mcp-write", "type": "function", "function": {
                        "name": "call_mcp_tool", "arguments": json!({"connection": connection,
                            "name": "change", "arguments": {"value": "synthetic"}}).to_string()}}))
                })
            }
        })),
        false,
    )
    .await?;
    let mut node = f.node("McpBoundary", &f.alice).await?;
    let tested = call(
        &node.app,
        "/api/mcp/test",
        Some(
            json!({"id": new_id(), "name": "Fixture", "url": format!("{}/mcp", remote.origin()),
            "token": ""}),
        ),
        200,
    )
    .await?;
    let id = new_id();
    *locked(&connection) = id.clone();
    call(
        &node.app,
        "/api/mcp",
        Some(json!({"id": id, "test": tested["id"]})),
        200,
    )
    .await?;
    call(
        &node.app,
        &format!("/api/agents/{}/mcp/{id}", node.local_agent()),
        Some(json!({"id": new_id(), "revision": 0, "connectionRevision": 1, "tools": ["change"]})),
        200,
    )
    .await?;
    let p = f.owner(&node).await?;
    let result = async {
        let first = f.post(&f.alice, &[&p.participation_id]).await?;
        until("approval", || async { Ok(node.approvals()?.len() == 1) }).await?;
        approve(&node, &node.approvals()?[0].clone()).await?;
        f.done(&first[0], None).await?;
        ensure!(
            effects.load(Ordering::SeqCst) == 1,
            "effects: {}",
            effects.load(Ordering::SeqCst)
        );
        hold_catalogue.store(true, Ordering::SeqCst);
        let second = f.post(&f.alice, &[&p.participation_id]).await?;
        until("approval", || async { Ok(node.approvals()?.len() == 1) }).await?;
        approve(&node, &node.approvals()?[0].clone()).await?;
        until("catalogue held", || async {
            Ok(held.load(Ordering::SeqCst))
        })
        .await?;
        f.withdraw(&p.participation_id).await?;
        until("turn unknown", || async {
            Ok(f.turn_state(&second[0])? == "unknown")
        })
        .await?;
        // Wait for the node's local cancellation, not just the hub state change.
        until("local cancellation", || async {
            Ok(!node.check(&second[0])?)
        })
        .await?;
        release.open();
        hold_catalogue.store(false, Ordering::SeqCst);
        pause(100).await;
        ensure!(
            effects.load(Ordering::SeqCst) == 1,
            "revoked call dispatched"
        );
        node.restart().await?;
        let renewed = f.owner(&node).await?;
        lose_result.store(true, Ordering::SeqCst);
        let third = f.post(&f.alice, &[&renewed.participation_id]).await?;
        until("approval", || async { Ok(node.approvals()?.len() == 1) }).await?;
        let action = node.approvals()?[0].clone();
        approve(&node, &action).await?;
        until("turn failed", || async {
            Ok(f.turn_state(&third[0])? == "failed")
        })
        .await?;
        ensure!(
            effects.load(Ordering::SeqCst) == 2,
            "effects: {}",
            effects.load(Ordering::SeqCst)
        );
        ensure!(
            f.results()?
                .iter()
                .any(|row| row["kind"] == "turn.effect.receipt"
                    && row["payload"]["actionId"] == action["actionId"]
                    && row["payload"]["outcome"] == "unknown"),
            "uncertain receipt"
        );
        node.restart().await?;
        pause(150).await;
        ensure!(effects.load(Ordering::SeqCst) == 2, "lost result replayed");
        Ok(())
    }
    .await;
    release.open();
    result
}

/// #87 removed `agents.runtime`, so no agent can be switched to a native
/// runtime any more. The surviving fence is the local engine refusal
/// (`local_engine_unsupported_or_unavailable`): the case hides the agent
/// locally after its grant (which leaves the policy digest unchanged) and
/// still checks the advertised runtime fence, that the hub turn fails and
/// that no provider call happens, also after restart.
async fn unsupported_effect_fencing_never_executes_hub_turns(runtime: &'static str) -> Result<()> {
    let f = Hub::start(1, None, false).await?;
    let mut node = f.node("NativeBoundary", &f.alice).await?;
    let p = f.owner(&node).await?;
    let agent = node.local_agent();
    node.stop().await?;
    Writable::open(node.app.database())?.execute(
        "INSERT INTO agent_visibility(agent,hidden) VALUES(?,1) ON CONFLICT(agent) DO UPDATE SET hidden=1",
        [agent.as_str()],
    )?;
    node.restart().await?;
    let status = call(&node.app, "/api/hub-link", None, 200).await?;
    ensure!(
        status["supportedRuntimes"] == json!(["api"]),
        "status: {status}"
    );
    ensure!(
        status["unsupportedRuntimes"][runtime] == "native_effect_fencing_unavailable",
        "status: {status}"
    );
    let post = f.post(&f.alice, &[&p.participation_id]).await?;
    until("turn failed", || async {
        Ok(f.turn_state(&post[0])? == "failed")
    })
    .await?;
    let results: Vec<Value> = f
        .results()?
        .into_iter()
        .filter(|row| row["turnId"] == post[0].as_str())
        .collect();
    ensure!(
        results
            .last()
            .is_some_and(|row| row["payload"]["code"] == "local_engine_unsupported_or_unavailable"),
        "results: {results:?}"
    );
    ensure!(f.calls() == 0, "provider calls: {}", f.calls());
    node.restart().await?;
    ensure!(
        f.calls() == 0,
        "provider calls after restart: {}",
        f.calls()
    );
    Ok(())
}

#[tokio::test]
async fn codex_unsupported_effect_fencing_never_executes_hub_turns() -> Result<()> {
    unsupported_effect_fencing_never_executes_hub_turns("codex").await
}

#[tokio::test]
async fn claude_unsupported_effect_fencing_never_executes_hub_turns() -> Result<()> {
    unsupported_effect_fencing_never_executes_hub_turns("claude").await
}

#[tokio::test]
async fn hermes_unsupported_effect_fencing_never_executes_hub_turns() -> Result<()> {
    unsupported_effect_fencing_never_executes_hub_turns("hermes").await
}

#[tokio::test]
async fn antigravity_unsupported_effect_fencing_never_executes_hub_turns() -> Result<()> {
    unsupported_effect_fencing_never_executes_hub_turns("antigravity").await
}

#[tokio::test]
async fn grok_unsupported_effect_fencing_never_executes_hub_turns() -> Result<()> {
    unsupported_effect_fencing_never_executes_hub_turns("grok").await
}

#[tokio::test]
async fn pi_unsupported_effect_fencing_never_executes_hub_turns() -> Result<()> {
    unsupported_effect_fencing_never_executes_hub_turns("pi").await
}

#[tokio::test]
async fn opencode_unsupported_effect_fencing_never_executes_hub_turns() -> Result<()> {
    unsupported_effect_fencing_never_executes_hub_turns("opencode").await
}

#[tokio::test]
async fn gemini_unsupported_effect_fencing_never_executes_hub_turns() -> Result<()> {
    unsupported_effect_fencing_never_executes_hub_turns("gemini").await
}

#[tokio::test]
async fn kiro_unsupported_effect_fencing_never_executes_hub_turns() -> Result<()> {
    unsupported_effect_fencing_never_executes_hub_turns("kiro").await
}
