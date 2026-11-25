//! Enrollment, grants, routing, revocation and transport cases.

use super::fixture::{
    call, has_tool, hub_error, locked, script, signed, until, Hub, Session, HUB_WAIT,
};
use crate::pr_support::{leaks, pause};
use minarbot_testkit::{
    async_model::{Gate, Step},
    db::Inspector,
    hosted_hub::{Outbound, Receive},
    prelude::*,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

fn body_kind(frame: &str) -> Value {
    serde_json::from_str::<Value>(frame).unwrap_or(Value::Null)["body"]["kind"].clone()
}

fn owner() -> Value {
    json!({"mode": "owner"})
}

fn all_members() -> Value {
    json!({"mode": "all-members"})
}

fn last(results: &[Value]) -> Value {
    results.last().cloned().unwrap_or(Value::Null)
}

#[tokio::test]
async fn browser_approved_enrollment_two_real_local_engines_attribution_and_local_key_custody(
) -> Result<()> {
    let f = Hub::start(2, None, false).await?;
    let alice = f.node("AliceNode", &f.alice).await?;
    let bob = f.node("BobNode", &f.bob).await?;
    let pa = f.owner(&alice).await?;
    let pb = f.participate(&bob, all_members(), None).await?;
    let turns = f
        .browser_post(&f.alice, &[&pa.participation_id, &pb.participation_id])
        .await?;
    for turn in &turns {
        let results = f.done(turn, None).await?;
        let last = last(&results);
        ensure!(
            results[0]["kind"] == "turn.admitted",
            "first: {}",
            results[0]
        );
        ensure!(last["kind"] == "turn.final", "last: {last}");
        ensure!(
            results
                .iter()
                .any(|result| result["kind"] == "turn.effect.receipt"),
            "no effect receipt"
        );
        ensure!(last["requesterAccountId"] == f.alice, "requester: {last}");
        let from_alice = last["nodeId"] == alice.node_id.as_str();
        ensure!(
            last["payload"]["text"]
                == if from_alice {
                    "Reply from AliceNode"
                } else {
                    "Reply from BobNode"
                },
            "reply: {last}"
        );
        ensure!(
            last["payerAccountId"] == if from_alice { &f.alice } else { &f.bob }.as_str(),
            "payer: {last}"
        );
        let sequences: Vec<i64> = results
            .iter()
            .map(|result| result["sequence"].as_i64().unwrap_or(0))
            .collect();
        ensure!(
            sequences == (1..=results.len() as i64).collect::<Vec<_>>(),
            "sequences: {sequences:?}"
        );
    }
    ensure!(f.calls() == 2, "provider calls: {}", f.calls());
    use minarbot_testkit::base64::Engine;
    let mut secrets = f.transport.key_values();
    for value in f.transport.key_values() {
        if let Ok(raw) =
            minarbot_testkit::base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(&value)
        {
            secrets.push(String::from_utf8_lossy(&raw).into_owned());
        }
    }
    for root in [
        f.root.path(),
        alice.app.root().path(),
        bob.app.root().path(),
    ] {
        let found = leaks(root, &f.transport.key_values())?;
        ensure!(found.is_empty(), "key leaked into {found:?}");
        let raw = raw_leaks(root, &f.transport.key_values())?;
        ensure!(raw.is_empty(), "raw key leaked into {raw:?}");
    }
    ensure!(
        f.transport.key_count() == 6,
        "keys: {}",
        f.transport.key_count()
    );
    Ok(())
}

/// Files containing the base64url-decoded bytes of any secret.
fn raw_leaks(root: &std::path::Path, secrets: &[String]) -> Result<Vec<String>> {
    use minarbot_testkit::base64::Engine;
    let raw: Vec<Vec<u8>> = secrets
        .iter()
        .filter_map(|secret| {
            minarbot_testkit::base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(secret)
                .ok()
        })
        .filter(|bytes| bytes.len() >= 16)
        .collect();
    let mut found = Vec::new();
    for path in crate::pr_support::files(root)? {
        let bytes = std::fs::read(&path)?;
        if raw.iter().any(|secret| {
            bytes
                .windows(secret.len())
                .any(|window| window == secret.as_slice())
        }) {
            found.push(path.display().to_string());
        }
    }
    Ok(found)
}

#[tokio::test]
async fn owner_only_default_specific_member_and_all_member_grants_offline_refuses_without_hub_execution(
) -> Result<()> {
    let f = Hub::start(1, None, false).await?;
    let mut bob = f.node("Bob", &f.bob).await?;
    let p = f.owner(&bob).await?;
    let denied = f.post(&f.alice, &[&p.participation_id]).await;
    ensure!(
        denied
            .as_ref()
            .is_err_and(|error| hub_error(error, Some("authority_denied"))),
        "non-owner post: {denied:?}"
    );
    let post = f.post(&f.bob, &[&p.participation_id]).await?;
    f.done(&post[0], Some(&f.bob)).await?;
    let specific = f
        .participate(
            &bob,
            json!({"mode": "members", "accountIds": [f.alice]}),
            None,
        )
        .await?;
    let post = f.post(&f.alice, &[&specific.participation_id]).await?;
    f.done(&post[0], None).await?;
    let all = f.participate(&bob, all_members(), None).await?;
    let post = f.post(&f.alice, &[&all.participation_id]).await?;
    f.done(&post[0], None).await?;
    bob.stop().await?;
    until("node offline", || async {
        Ok(f.node_field(&bob.node_id, "status")? == "offline")
    })
    .await?;
    let offline = f.post(&f.alice, &[&all.participation_id]).await;
    ensure!(
        offline
            .as_ref()
            .is_err_and(|error| hub_error(error, Some("node_offline"))),
        "offline post: {offline:?}"
    );
    ensure!(f.calls() == 3, "provider calls: {}", f.calls());
    let outsider = f.post(&f.eve, &[]).await;
    ensure!(
        outsider.as_ref().is_err_and(|error| hub_error(error, None)),
        "outsider post: {outsider:?}"
    );
    let _ = &mut bob;
    Ok(())
}

#[tokio::test]
async fn dangerous_tool_waits_for_exact_local_owner_approval_hub_browser_roles_cannot_approve_it(
) -> Result<()> {
    let tool_calls = Arc::new(Mutex::new(0_usize));
    let counter = tool_calls.clone();
    let f = Hub::start(
        1,
        Some(script(move |body| {
            let counter = counter.clone();
            async move {
                if has_tool(&body) {
                    return Ok(Step::text("Tool finished"));
                }
                *locked(&counter) += 1;
                Ok(Step::RawTool(json!({"id": "write-once", "type": "function",
                    "function": {"name": "create_text_file", "arguments": json!({"name": "result.txt",
                        "text": "Private file created locally."}).to_string()}})))
            }
        })),
        false,
    )
    .await?;
    let node = f.node("Writer", &f.alice).await?;
    let p = f.owner(&node).await?;
    let post = f.post(&f.alice, &[&p.participation_id]).await?;
    until("owner approval wait", || async {
        Ok(node.approvals()?.len() == 1)
    })
    .await?;
    let prompt = node.approvals()?[0].clone();
    ensure!(f.calls() == 1, "provider calls: {}", f.calls());
    let threads = call(
        &node.app,
        &format!("/api/agents/{}/threads", node.local_agent()),
        None,
        200,
    )
    .await?;
    let before = call(
        &node.app,
        &format!(
            "/api/threads/{}/turns",
            threads[0]["id"].as_str().unwrap_or("none")
        ),
        None,
        200,
    )
    .await?;
    ensure!(
        !before
            .as_array()
            .into_iter()
            .flatten()
            .any(|turn| turn["generatedFiles"]
                .as_array()
                .is_some_and(|files| !files.is_empty())),
        "file created before approval"
    );
    call(
        &node.app,
        "/api/hub-link/approvals",
        Some(json!({"actionId": prompt["actionId"], "actionDigest": "0".repeat(64), "approved": true})),
        403,
    )
    .await?;
    let denied = node
        .app
        .client()?
        .post(node.app.url("/api/hub-link/approvals"))
        .header("Origin", "https://alpha.minarbot.test")
        .header("Content-Type", "application/json")
        .body(
            json!({"actionId": prompt["actionId"], "actionDigest": prompt["actionDigest"],
            "approved": true})
            .to_string(),
        )
        .send()
        .await?
        .status()
        .as_u16();
    ensure!(denied == 403, "hub browser approval: {denied}");
    call(
        &node.app,
        "/api/hub-link/approvals",
        Some(
            json!({"actionId": prompt["actionId"], "actionDigest": prompt["actionDigest"],
            "approved": true}),
        ),
        200,
    )
    .await?;
    let results = f.done(&post[0], None).await?;
    ensure!(
        results
            .iter()
            .any(|result| result["kind"] == "turn.approval.request"
                && result["payload"]["requiredApproverAccountId"] == f.alice.as_str()),
        "approval request frame"
    );
    ensure!(
        results
            .iter()
            .any(|result| result["kind"] == "turn.effect.receipt"),
        "effect receipt"
    );
    ensure!(
        last(&results)["payload"]["text"] == "Tool finished",
        "final: {}",
        last(&results)
    );
    ensure!(
        *locked(&tool_calls) == 1,
        "tool calls: {}",
        *locked(&tool_calls)
    );
    ensure!(f.calls() == 3, "provider calls: {}", f.calls());
    Ok(())
}

#[tokio::test]
async fn duplicate_delivery_final_ack_loss_and_restart_never_execute_twice() -> Result<()> {
    let f = Hub::start(1, None, false).await?;
    let mut node = f.node("Once", &f.alice).await?;
    let p = f.owner(&node).await?;
    let duplicated = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let (dup, drop_flag, data_path, project) = (
        duplicated.clone(),
        dropped.clone(),
        f.data_path.clone(),
        f.project.project_id.clone(),
    );
    let receive: Receive = Arc::new(move |data, socket| {
        let wire: Value = serde_json::from_str(data).unwrap_or(Value::Null);
        if wire["body"]["kind"] == "turn.request" && !dup.swap(true, Ordering::SeqCst) {
            return vec![data.to_owned(), data.to_owned()];
        }
        if wire["body"]["kind"] == "response"
            && wire["body"]["result"]["accepted"] == true
            && !drop_flag.load(Ordering::SeqCst)
        {
            let sequence = wire["body"]["result"]["sequence"].as_i64().unwrap_or(-1);
            let row = Inspector::open(&data_path).and_then(|db| {
                db.rows(
                    "SELECT frame_json FROM node_results WHERE project_id=? AND sequence=?",
                    (project.as_str(), sequence),
                )
            });
            let is_final = row
                .ok()
                .and_then(|rows| rows.into_iter().next())
                .is_some_and(|row| {
                    serde_json::from_str::<Value>(row["frame_json"].as_str().unwrap_or("null"))
                        .is_ok_and(|frame| frame["kind"] == "turn.final")
                });
            if is_final {
                drop_flag.store(true, Ordering::SeqCst);
                socket.close(4409, "Injected ACK loss");
                return Vec::new();
            }
        }
        vec![data.to_owned()]
    });
    f.transport.receive_frames(Some(receive));
    let post = f.browser_post(&f.alice, &[&p.participation_id]).await?;
    until("provider call", || async { Ok(f.calls() == 1) }).await?;
    until("result ACK dropped", || async {
        Ok(dropped.load(Ordering::SeqCst))
    })
    .await?;
    ensure!(f.turn_state(&post[0])? == "done", "turn state");
    until("generation fenced", || async {
        Ok(f.node_field(&node.node_id, "connection_generation")? != 1)
    })
    .await?;
    node.restart().await?;
    pause(400).await;
    ensure!(f.calls() == 1, "provider calls: {}", f.calls());
    ensure!(
        duplicated.load(Ordering::SeqCst),
        "request was not duplicated"
    );
    let state = f.turn_state(&post[0])?;
    ensure!(state == "done" || state == "unknown", "turn state: {state}");
    f.transport.receive_frames(None);
    Ok(())
}

#[tokio::test]
async fn revocation_mid_stream_stops_local_work_before_another_effect_and_does_not_execute_offline(
) -> Result<()> {
    let held = Gate::new();
    let gate = held.clone();
    let f = Hub::start(
        1,
        Some(script(move |_| {
            let gate = gate.clone();
            async move {
                gate.wait().await;
                Ok(Step::text("Late answer"))
            }
        })),
        false,
    )
    .await?;
    let node = f.node("Held", &f.alice).await?;
    let p = f.owner(&node).await?;
    let post = f.post(&f.alice, &[&p.participation_id]).await?;
    until("provider call", || async { Ok(f.calls() == 1) }).await?;
    f.withdraw(&p.participation_id).await?;
    until("stopped", || async {
        let status = call(&node.app, "/api/hub-link", None, 200).await?;
        Ok(status["connected"] != true || f.turn_state(&post[0])? == "unknown")
    })
    .await?;
    held.open();
    pause(300).await;
    ensure!(f.calls() == 1, "provider calls: {}", f.calls());
    ensure!(f.turn_state(&post[0])? == "unknown", "turn state");
    let again = f.post(&f.alice, &[&p.participation_id]).await;
    ensure!(again.is_err(), "post after withdrawal: {again:?}");
    Ok(())
}

#[tokio::test]
async fn foreign_project_and_forged_signed_hub_frames_never_reach_local_engine() -> Result<()> {
    let f = Hub::start(1, None, false).await?;
    let mut node = f.node("Isolated", &f.alice).await?;
    let p = f.owner(&node).await?;
    let forged = ring::signature::Ed25519KeyPair::from_pkcs8(
        ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
            .map_err(|_| "key")?
            .as_ref(),
    )
    .map_err(|_| "key")?;
    let forged = Arc::new(forged);
    for scenario in ["foreign project", "wrong signature"] {
        let injected = Arc::new(AtomicBool::new(false));
        let (flag, signer, forged) = (injected.clone(), f.signer.clone(), forged.clone());
        let receive: Receive = Arc::new(move |data, _| {
            let wire: Value = serde_json::from_str(data).unwrap_or(Value::Null);
            if wire["body"]["kind"] != "turn.request" || flag.load(Ordering::SeqCst) {
                return vec![data.to_owned()];
            }
            flag.store(true, Ordering::SeqCst);
            if scenario == "foreign project" {
                let mut foreign = wire["body"].clone();
                foreign["projectId"] = json!(new_id());
                return vec![minarbot_testkit::protocol::canonical_json(&signed(
                    &foreign, &signer, "hub",
                ))];
            }
            let mut wrong = wire.clone();
            wrong["signature"] = signed(&wire["body"], &forged, "hub")["signature"].clone();
            vec![minarbot_testkit::protocol::canonical_json(&wrong)]
        });
        f.transport.receive_frames(Some(receive));
        let post = f.browser_post(&f.alice, &[&p.participation_id]).await?;
        until(scenario, || async { Ok(injected.load(Ordering::SeqCst)) }).await?;
        until("turn unknown", || async {
            Ok(f.turn_state(&post[0])? == "unknown")
        })
        .await?;
        ensure!(f.calls() == 0, "{scenario}: provider calls {}", f.calls());
        f.transport.receive_frames(None);
        node.restart().await?;
    }
    Ok(())
}

#[tokio::test]
async fn local_owner_endpoints_require_exact_origin_and_refuse_bearer_callers_unenrolled_app_stays_idle(
) -> Result<()> {
    let f = Hub::start(1, None, false).await?;
    let node = f.node("Boundary", &f.alice).await?;
    let client = node.app.client()?;
    let foreign = client
        .get(node.app.url("/api/hub-link"))
        .header("Origin", "https://foreign.test")
        .send()
        .await?
        .status()
        .as_u16();
    ensure!(foreign == 403, "foreign origin: {foreign}");
    let bearer = client
        .get(node.app.url("/api/hub-link"))
        .header("Origin", node.app.origin())
        .header("Authorization", "Bearer rejected")
        .send()
        .await?
        .status()
        .as_u16();
    ensure!(bearer == 403, "bearer caller: {bearer}");
    ensure!(f.calls() == 0, "provider calls: {}", f.calls());
    call(&node.app, "/api/hub-link/revoke", Some(json!({})), 200).await?;
    until("node revoked", || async {
        Ok(f.node_field(&node.node_id, "status")? == "revoked")
    })
    .await?;
    ensure!(
        node.status()?["disabled"] == true,
        "status: {}",
        node.status()?
    );
    Ok(())
}

#[tokio::test]
async fn node_revocation_drains_its_signed_frame_before_closing_transport() -> Result<()> {
    let f = Hub::start(1, None, false).await?;
    let node = f.node("Delayed revoke", &f.alice).await?;
    let delayed = Arc::new(AtomicBool::new(false));
    let flag = delayed.clone();
    f.transport.intercept_sends(Some(Arc::new(move |frame| {
        if body_kind(frame) == "node.revoke" {
            flag.store(true, Ordering::SeqCst);
            return Outbound::After(Box::pin(tokio::time::sleep(Duration::from_millis(50))));
        }
        Outbound::Now
    })));
    call(&node.app, "/api/hub-link/revoke", Some(json!({})), 200).await?;
    ensure!(
        delayed.load(Ordering::SeqCst),
        "revoke frame not sent before reply"
    );
    until("node revoked", || async {
        Ok(f.node_field(&node.node_id, "status")? == "revoked")
    })
    .await?;
    ensure!(
        node.status()?["disabled"] == true,
        "status: {}",
        node.status()?
    );
    ensure!(f.calls() == 0, "provider calls: {}", f.calls());
    f.transport.intercept_sends(None);
    Ok(())
}

#[tokio::test]
async fn sigkill_after_dispatch_and_before_result_ack_keeps_durable_uncertainty_without_replay(
) -> Result<()> {
    let held = Gate::new();
    let gate = held.clone();
    let f = Hub::start(
        1,
        Some(script(move |_| {
            let gate = gate.clone();
            async move {
                gate.wait().await;
                Ok(Step::text("Effect already happened"))
            }
        })),
        false,
    )
    .await?;
    let mut node = f.node("Crash", &f.alice).await?;
    let p = f.owner(&node).await?;
    let post = f.browser_post(&f.alice, &[&p.participation_id]).await?;
    until("provider dispatched", || async { Ok(f.calls() == 1) }).await?;
    node.crash().await?;
    held.open();
    until("durable uncertainty", || async {
        Ok(f.turn_state(&post[0])? == "unknown")
    })
    .await?;
    node.restart().await?;
    pause(500).await;
    ensure!(f.calls() == 1, "provider calls: {}", f.calls());
    ensure!(f.turn_state(&post[0])? == "unknown", "turn state");
    Ok(())
}

#[tokio::test]
async fn provider_partial_snapshots_stream_over_real_websockets_before_final() -> Result<()> {
    let f = Hub::start(
        1,
        Some(script(|_| async {
            Ok(Step::Stream(
                "Streamed local answer".into(),
                Duration::from_millis(300),
            ))
        })),
        true,
    )
    .await?;
    let node = f.node("Stream", &f.alice).await?;
    let p = f.owner(&node).await?;
    let post = f.post(&f.alice, &[&p.participation_id]).await?;
    let results = f.done(&post[0], None).await?;
    ensure!(
        results.iter().any(|frame| frame["kind"] == "turn.partial"
            && frame["payload"]["text"] == "Streamed local answer"),
        "no partial: {results:?}"
    );
    ensure!(last(&results)["kind"] == "turn.final", "last frame");
    ensure!(
        last(&results)["payload"]["text"] == "Streamed local answer",
        "final text"
    );
    ensure!(f.calls() == 1, "provider calls: {}", f.calls());
    Ok(())
}

#[tokio::test]
async fn browser_code_alone_cannot_redeem_enrollment_node_upgrade_rejects_browser_credentials(
) -> Result<()> {
    let f = Hub::start(1, None, false).await?;
    for path in ["/api/nodes/enrollment/poll", "/api/nodes/session"] {
        let status = f
            .request(
                path,
                &[
                    ("Host", &f.accounts_host),
                    ("Content-Type", "application/json"),
                ],
                Some(json!({"deviceCode": "BADCODE"}).to_string()),
            )
            .await?;
        ensure!(status == 400 || status == 403, "{path}: {status}");
    }
    let refused = minarbot_testkit::hosted_hub::browser_upgrade(
        &f.local_origin,
        &[
            ("host", f.accounts_host.as_str()),
            ("origin", f.origin.as_str()),
            ("cookie", "session=forbidden"),
        ],
    )
    .await;
    ensure!(
        refused.is_err(),
        "browser credentials upgraded to a node socket"
    );
    Ok(())
}

#[tokio::test]
async fn reconnect_fences_old_generation_session_expiry_renews_with_node_key_and_never_replays_completed_work(
) -> Result<()> {
    let f = Hub::start(1, None, false).await?;
    let node = f.node("Renew", &f.alice).await?;
    let p = f.owner(&node).await?;
    let post = f.browser_post(&f.alice, &[&p.participation_id]).await?;
    f.done(&post[0], None).await?;
    let generation = || -> Result<i64> {
        Ok(f.node_field(&node.node_id, "connection_generation")?
            .as_i64()
            .unwrap_or(0))
    };
    let session = || -> Result<i64> {
        Ok(f.field(
            "SELECT expires_at FROM node_link_sessions WHERE node_id=?",
            [&node.node_id],
            "expires_at",
        )?
        .as_i64()
        .unwrap_or(0))
    };
    let (before, expires) = (generation()?, session()?);
    f.advance(900_001).await?;
    until("signed renewal", || async {
        Ok(generation()? > before && node.status()?["connected"] == true)
    })
    .await?;
    ensure!(session()? > expires, "session not renewed");
    ensure!(f.calls() == 1, "provider calls: {}", f.calls());
    ensure!(f.turn_state(&post[0])? == "done", "turn state");
    Ok(())
}

#[tokio::test]
async fn lost_enrollment_redemption_response_recovers_with_signed_node_proof() -> Result<()> {
    let f = Hub::start(1, None, false).await?;
    f.transport.lose_enrollment_response();
    let node = f.node("RecoveredEnrollment", &f.alice).await?;
    ensure!(
        node.status()?["enrolled"] == true,
        "status: {}",
        node.status()?
    );
    ensure!(f.calls() == 0, "provider calls: {}", f.calls());
    Ok(())
}

#[tokio::test]
async fn changed_local_agent_policy_rejects_an_existing_hub_grant_before_provider_dispatch(
) -> Result<()> {
    let f = Hub::start(1, None, false).await?;
    let node = f.node("Changed", &f.alice).await?;
    let p = f.owner(&node).await?;
    call(
        &node.app,
        &format!("/api/agents/{}/edit", node.local_agent()),
        Some(
            json!({"id": new_id(), "revision": node.agent["revision"], "name": "Changed config",
            "model": "Changed", "instructions": "Updated policy"}),
        ),
        200,
    )
    .await?;
    let post = f.post(&f.alice, &[&p.participation_id]).await?;
    until("turn unknown", || async {
        Ok(f.turn_state(&post[0])? == "unknown")
    })
    .await?;
    ensure!(f.calls() == 0, "provider calls: {}", f.calls());
    Ok(())
}

#[tokio::test]
async fn revoking_alpha_preserves_simultaneous_beta_work_on_the_same_node() -> Result<()> {
    let held: Arc<Mutex<Vec<Gate>>> = Arc::default();
    let replies: Arc<Mutex<Vec<String>>> = Arc::default();
    let (gates, texts) = (held.clone(), replies.clone());
    let f = Hub::start(
        1,
        Some(script(move |_| {
            let gate = Gate::new();
            let index = {
                let mut gates = locked(&gates);
                gates.push(gate.clone());
                gates.len() - 1
            };
            let texts = texts.clone();
            async move {
                gate.wait().await;
                Ok(Step::text(
                    locked(&texts).get(index).cloned().unwrap_or_default(),
                ))
            }
        })),
        false,
    )
    .await?;
    let node = f.node("TwoProjects", &f.alice).await?;
    let alpha = f.owner(&node).await?;
    let beta = f.create_project(&f.alice, "beta", "Beta").await?;
    let bp = f.participate(&node, owner(), Some(&beta)).await?;
    let a = f.post(&f.alice, &[&alpha.participation_id]).await?;
    until("alpha dispatched", || async {
        Ok(locked(&held).len() == 1)
    })
    .await?;
    let b = f
        .post_in(&f.alice, &[&bp.participation_id], Some(&beta), "Beta work")
        .await?;
    until("beta dispatched", || async { Ok(locked(&held).len() == 2) }).await?;
    let generation = f.node_field(&node.node_id, "connection_generation")?;
    // Revoke when Alpha's result is already in flight, before the hub accepts it.
    let (withdrawn, alpha_turn) = (Gate::new(), a[0].clone());
    let in_flight = Arc::new(AtomicBool::new(false));
    let (hold, flag) = (withdrawn.clone(), in_flight.clone());
    f.transport.intercept_sends(Some(Arc::new(move |frame| {
        let body = serde_json::from_str::<Value>(frame).unwrap_or(Value::Null)["body"].clone();
        if body["kind"] == "turn.result"
            && body["frame"]["kind"] == "turn.final"
            && body["frame"]["turnId"] == alpha_turn.as_str()
        {
            flag.store(true, Ordering::SeqCst);
            let hold = hold.clone();
            return Outbound::After(Box::pin(async move { hold.wait().await }));
        }
        Outbound::Now
    })));
    locked(&replies).extend(["Late alpha".to_owned(), "Beta stays connected".to_owned()]);
    let result = async {
        locked(&held)[0].open();
        until("Alpha final in flight", || async {
            Ok(in_flight.load(Ordering::SeqCst))
        })
        .await?;
        f.withdraw(&alpha.participation_id).await?;
        withdrawn.open();
        until("Alpha revoked during result delivery", || async {
            Ok(f.turn_state(&a[0])? == "unknown")
        })
        .await?;
        locked(&held)[1].open();
        until("Beta result after Alpha revocation", || async {
            let state = f.turn_in(&b[0], &beta.project_id)?["state"].clone();
            ensure!(
                state == "admitted" || state == "done",
                "Unexpected Beta turn state: {state}"
            );
            Ok(state == "done")
        })
        .await?;
        Ok::<(), minarbot_testkit::Error>(())
    }
    .await;
    f.transport.intercept_sends(None);
    withdrawn.open();
    for gate in locked(&held).iter() {
        gate.open();
    }
    result?;
    ensure!(
        !f.results()?
            .iter()
            .any(|frame| frame["kind"] == "turn.final"),
        "alpha final was routed"
    );
    ensure!(
        f.node_field(&node.node_id, "connection_generation")? == generation,
        "connection was replaced"
    );
    ensure!(node.status()?["connected"] == true, "node disconnected");
    ensure!(f.calls() == 2, "provider calls: {}", f.calls());
    Ok(())
}

#[tokio::test]
async fn offline_local_withdrawal_survives_restart_and_reaches_hub_before_reconnect_admits_work(
) -> Result<()> {
    let f = Hub::start(1, None, false).await?;
    let mut node = f.node("WithdrawOffline", &f.alice).await?;
    let p = f.owner(&node).await?;
    node.close_link().await?;
    until("node offline", || async {
        Ok(f.node_field(&node.node_id, "status")? == "offline")
    })
    .await?;
    call(
        &node.app,
        "/api/hub-link/grants/revoke",
        Some(json!({"projectId": f.project.project_id, "participationId": p.participation_id})),
        200,
    )
    .await?;
    node.restart().await?;
    until("durable withdrawal", || async {
        Ok(f.participation(&p.participation_id, "state")? == "revoked")
    })
    .await?;
    let again = f.post(&f.alice, &[&p.participation_id]).await;
    ensure!(
        again.as_ref().is_err_and(|error| hub_error(error, None)),
        "post after withdrawal: {again:?}"
    );
    ensure!(f.calls() == 0, "provider calls: {}", f.calls());
    Ok(())
}

#[tokio::test]
async fn hub_side_withdrawal_waits_offline_beyond_turn_deadline_and_disables_local_grant_before_new_work(
) -> Result<()> {
    let f = Hub::start(1, None, false).await?;
    let mut node = f.node("HubWithdrawOffline", &f.bob).await?;
    let p = f.owner(&node).await?;
    node.stop().await?;
    until("node offline", || async {
        Ok(f.node_field(&node.node_id, "status")? == "offline")
    })
    .await?;
    let session = f.project_session(&f.alice, None).await?;
    f.browser(
        &f.project.host,
        "/api/participants/withdraw",
        Some(json!({"participationId": p.participation_id})),
        Some(&session),
        None,
        Some(200),
    )
    .await?;
    f.advance(600_001).await?;
    node.restart().await?;
    until("durable control ACK", || async {
        Ok(f.field(
            "SELECT acked FROM outbox WHERE participation_id=? AND kind='grant.revoked'",
            [&p.participation_id],
            "acked",
        )? == 1)
    })
    .await?;
    node.stop().await?;
    let enabled = Inspector::open(node.app.database())?.row(
        "SELECT enabled FROM hub_link_grants WHERE participation_id=?",
        [&p.participation_id],
    )?;
    ensure!(
        enabled["enabled"] == 0,
        "local grant still enabled: {enabled}"
    );
    ensure!(f.calls() == 0, "provider calls: {}", f.calls());
    Ok(())
}

#[tokio::test]
async fn withdrawal_ack_loss_retries_same_revision_and_cannot_withdraw_another_node_or_newer_consent(
) -> Result<()> {
    let f = Hub::start(2, None, false).await?;
    let node = f.node("WithdrawReceipt", &f.alice).await?;
    let other = f.node("OtherOwner", &f.bob).await?;
    let p = f.owner(&node).await?;
    let foreign = f.owner(&other).await?;
    let generation = || -> Result<i64> {
        Ok(f.node_field(&node.node_id, "connection_generation")?
            .as_i64()
            .unwrap_or(0))
    };
    let before = generation()?;
    let dropped = Arc::new(AtomicBool::new(false));
    let flag = dropped.clone();
    f.transport.intercept_sends(Some(Arc::new(move |frame| {
        if body_kind(frame) == "participation.withdraw" && !flag.swap(true, Ordering::SeqCst) {
            return Outbound::ThenClose(4409, "Injected withdrawal ACK loss".into());
        }
        Outbound::Now
    })));
    call(
        &node.app,
        "/api/hub-link/grants/revoke",
        Some(json!({"projectId": f.project.project_id, "participationId": p.participation_id})),
        200,
    )
    .await?;
    until("withdrawal retried", || async {
        let status = node.status()?;
        Ok(status["connected"] == true
            && status["pendingWithdrawals"] == 0
            && generation()? > before)
    })
    .await?;
    f.transport.intercept_sends(None);
    ensure!(
        dropped.load(Ordering::SeqCst),
        "withdrawal ACK was not dropped"
    );
    ensure!(
        f.participation(&p.participation_id, "revision")? == p.revision + 1,
        "withdrawal revision"
    );
    let principal = json!({"nodeId": node.node_id, "accountId": f.alice, "nodeGeneration": 1,
        "connectionGeneration": generation()?});
    let foreign_result = f
        .withdraw_node(&principal, &foreign.participation_id, foreign.revision)
        .await;
    ensure!(
        foreign_result
            .as_ref()
            .is_err_and(|error| hub_error(error, Some("authority_denied"))),
        "foreign withdrawal: {foreign_result:?}"
    );
    let renewed = f.owner(&node).await?;
    f.withdraw_node(&principal, &renewed.participation_id, p.revision)
        .await?;
    ensure!(
        f.participation(&renewed.participation_id, "state")? == "active",
        "stale withdrawal revoked newer consent"
    );
    ensure!(f.calls() == 0, "provider calls: {}", f.calls());
    Ok(())
}

#[tokio::test]
async fn accounts_enrollment_routes_reject_project_cookies_bad_csrf_fingerprint_mismatch_and_account_overrides(
) -> Result<()> {
    let f = Hub::start(2, None, false).await?;
    let node = f.node("EnrollmentSession", &f.alice).await?;
    // A fresh candidate has no approving account until authenticated browser approval.
    let candidate_root = TestRoot::new()?;
    let candidate = candidate_root
        .start_in(&f.servers, node.config.clone())
        .await?;
    let started = call(
        &candidate,
        "/api/hub-link/enrollment/start",
        Some(json!({})),
        200,
    )
    .await?;
    let session = f.login(&f.alice).await?;
    let path = "/api/nodes/enrollment/approve";
    let inspected = f
        .browser(
            &f.accounts_host,
            &format!(
                "/api/nodes/enrollment?humanCode={}",
                started["humanCode"].as_str().unwrap_or_default()
            ),
            None,
            Some(&session),
            None,
            Some(200),
        )
        .await?;
    let value =
        json!({"humanCode": started["humanCode"], "fingerprint": inspected.data["fingerprint"]});
    let bad_csrf = Session {
        csrf: "bad".into(),
        ..session.clone()
    };
    f.browser(
        &f.accounts_host,
        path,
        Some(value.clone()),
        Some(&bad_csrf),
        None,
        Some(403),
    )
    .await?;
    let mut mismatch = value.clone();
    mismatch["fingerprint"] = json!("0".repeat(64));
    f.browser(
        &f.accounts_host,
        path,
        Some(mismatch),
        Some(&session),
        None,
        Some(404),
    )
    .await?;
    let mut override_account = value.clone();
    override_account["accountId"] = json!(f.bob);
    f.browser(
        &f.accounts_host,
        path,
        Some(override_account),
        Some(&session),
        None,
        Some(400),
    )
    .await?;
    let project = f.project_session(&f.alice, None).await?;
    f.browser(
        &f.accounts_host,
        path,
        Some(value.clone()),
        Some(&project),
        None,
        Some(401),
    )
    .await?;
    f.browser(
        &f.project.host,
        path,
        Some(value.clone()),
        Some(&project),
        None,
        Some(404),
    )
    .await?;
    let cross = f
        .request(
            path,
            &[
                ("Host", &f.accounts_host),
                ("Origin", "https://alpha.minarbot.test"),
                ("Cookie", &session.cookies),
                ("X-CSRF-Token", &session.csrf),
                ("Content-Type", "application/json"),
            ],
            Some(value.to_string()),
        )
        .await?;
    ensure!(cross == 403, "cross-origin approval: {cross}");
    f.browser(
        &f.accounts_host,
        path,
        Some(value.clone()),
        Some(&session),
        None,
        Some(200),
    )
    .await?;
    let bob = f.login(&f.bob).await?;
    f.browser(
        &f.accounts_host,
        path,
        Some(value.clone()),
        Some(&bob),
        None,
        Some(403),
    )
    .await?;
    f.browser(
        &f.accounts_host,
        "/auth/logout",
        Some(json!({"global": false})),
        Some(&session),
        None,
        Some(200),
    )
    .await?;
    f.browser(
        &f.accounts_host,
        path,
        Some(value),
        Some(&session),
        None,
        Some(401),
    )
    .await?;
    call(
        &candidate,
        "/api/hub-link/enrollment/poll",
        Some(json!({})),
        200,
    )
    .await?;
    let status = super::fixture::runtime_status(&candidate)?;
    ensure!(
        status["accountId"] == f.alice,
        "candidate account: {status}"
    );
    Ok(())
}

#[tokio::test]
async fn two_session_enrolled_nodes_answer_browser_mention_revoking_one_live_stream_preserves_other_node(
) -> Result<()> {
    let f = Hub::start(
        2,
        Some(script(|body| async move {
            let model = body["model"].as_str().unwrap_or_default().to_owned();
            let delay = if model == "HeldStream" { 2000 } else { 300 };
            Ok(Step::Stream(
                format!("Partial from {model}"),
                Duration::from_millis(delay),
            ))
        })),
        true,
    )
    .await?;
    let alice = f.node("HeldStream", &f.alice).await?;
    let bob = f.node("OtherStream", &f.bob).await?;
    let pa = f.owner(&alice).await?;
    let pb = f.participate(&bob, all_members(), None).await?;
    let post = f
        .browser_post(&f.alice, &[&pa.participation_id, &pb.participation_id])
        .await?;
    let mut revoked = None;
    for turn in &post {
        if f.turn(turn)?["node_id"] == alice.node_id.as_str() {
            revoked = Some(turn.clone());
        }
    }
    let revoked = revoked.ok_or("revoked turn missing")?;
    let other = post
        .iter()
        .find(|turn| **turn != revoked)
        .cloned()
        .ok_or("other turn missing")?;
    until("live partial before revoke", || async {
        Ok(f.results()?
            .iter()
            .any(|row| row["turnId"] == revoked.as_str() && row["kind"] == "turn.partial"))
    })
    .await?;
    f.withdraw(&pa.participation_id).await?;
    until("local cancellation", || async {
        Ok(!alice.check(&revoked)?)
    })
    .await?;
    let results = f.done(&other, None).await?;
    ensure!(
        last(&results)["nodeId"] == bob.node_id.as_str(),
        "other node"
    );
    ensure!(
        last(&results)["payload"]["text"] == "Partial from OtherStream",
        "other reply"
    );
    ensure!(f.turn_state(&revoked)? == "unknown", "revoked turn state");
    ensure!(
        !f.results()?
            .iter()
            .any(|row| row["turnId"] == revoked.as_str() && row["kind"] == "turn.final"),
        "revoked turn finished"
    );
    ensure!(f.calls() == 2, "provider calls: {}", f.calls());
    Ok(())
}

#[tokio::test]
async fn offline_withdrawal_is_acknowledged_even_after_owner_loses_project_membership() -> Result<()>
{
    let f = Hub::start(1, None, false).await?;
    let mut node = f.node("RemovedOwner", &f.bob).await?;
    let p = f.owner(&node).await?;
    node.close_link().await?;
    call(
        &node.app,
        "/api/hub-link/grants/revoke",
        Some(json!({"projectId": f.project.project_id, "participationId": p.participation_id})),
        200,
    )
    .await?;
    let session = f.project_session(&f.alice, None).await?;
    f.browser(
        &f.project.host,
        "/api/members",
        Some(json!({"accountId": f.bob, "revision": 1, "role": "revoked"})),
        Some(&session),
        None,
        Some(200),
    )
    .await?;
    node.restart().await?;
    ensure!(
        node.status()?["pendingWithdrawals"] == 0,
        "status: {}",
        node.status()?
    );
    ensure!(
        f.participation(&p.participation_id, "state")? == "revoked",
        "participation state"
    );
    ensure!(f.calls() == 0, "provider calls: {}", f.calls());
    Ok(())
}

#[tokio::test]
async fn project_queue_admission_is_atomic_idempotent_and_expires_without_executing_offline_work(
) -> Result<()> {
    let f = Hub::start(1, None, false).await?;
    let mut node = f.node("QueueBoundary", &f.alice).await?;
    let p = f.owner(&node).await?;
    let input = json!({"requestId": new_id(), "sectionId": f.project.section_id,
        "text": "Bounded queue", "mentions": [p.participation_id]});
    // Let the hosted queue fill while withholding delivery to the local engine.
    f.transport.receive_frames(Some(Arc::new(|frame, _| {
        let kind = body_kind(frame);
        if kind == "section.event" || kind == "turn.request" {
            Vec::new()
        } else {
            vec![frame.to_owned()]
        }
    })));
    let first = f.queued_post(&input, 0).await?;
    for _ in 1..100 {
        let mut next = input.clone();
        next["requestId"] = json!(new_id());
        f.queued_post(&next, 0).await?;
    }
    let mut overflow = input.clone();
    overflow["requestId"] = json!(new_id());
    let full = f.queued_post(&overflow, 0).await;
    ensure!(
        full.as_ref().is_err_and(|error| error
            .downcast_ref::<minarbot_testkit::hosted_hub::HubError>()
            .is_some_and(|error| error.status == 429 && error.code == "project_queue_full")),
        "queue overflow: {full:?}"
    );
    ensure!(f.queued_post(&input, 0).await? == first, "replay differs");
    ensure!(
        f.field(
            "SELECT count(*) AS n FROM turns WHERE project_id=?",
            [&f.project.project_id],
            "n"
        )? == 100,
        "queued turns"
    );
    let mut later = input.clone();
    later["requestId"] = json!(new_id());
    let expired = f.queued_post(&later, 600_001).await?;
    ensure!(
        expired["turnIds"].as_array().map(Vec::len) == Some(1),
        "post after expiry: {expired}"
    );
    node.restart().await?;
    f.transport.receive_frames(None);
    ensure!(f.calls() == 0, "provider calls: {}", f.calls());
    Ok(())
}

#[tokio::test]
async fn connected_app_resume_requires_exactly_one_ready_action_before_issuing_any_lease(
) -> Result<()> {
    let held = Gate::new();
    let gate = held.clone();
    let f = Hub::start(
        1,
        Some(script(move |_| {
            let gate = gate.clone();
            async move {
                gate.wait().await;
                Ok(Step::text("No connected app effects."))
            }
        })),
        false,
    )
    .await?;
    let node = f.node("BatchBoundary", &f.alice).await?;
    let p = f.owner(&node).await?;
    let post = f.post(&f.alice, &[&p.participation_id]).await?;
    until("provider call", || async { Ok(f.calls() == 1) }).await?;
    let (status, body) = node
        .before_effect(&post[0], "tool", json!({"kind": "composio.resume"}))
        .await?;
    ensure!(
        !(200..300).contains(&status) && body["code"] == "hub_effect_batch_unsupported",
        "batch resume: {status} {body}"
    );
    ensure!(node.approvals()?.is_empty(), "lease approval issued");
    held.open();
    f.done(&post[0], None).await?;
    ensure!(f.calls() == 1, "provider calls: {}", f.calls());
    Ok(())
}

#[tokio::test]
async fn human_post_rate_leaves_capacity_for_withdrawal_controls() -> Result<()> {
    let f = Hub::start(1, None, false).await?;
    let node = f.node("PostRate", &f.alice).await?;
    let p = f.owner(&node).await?;
    let session = f.project_session(&f.alice, None).await?;
    for _ in 0..60 {
        f.browser(
            &f.project.host,
            "/api/messages",
            Some(
                json!({"requestId": new_id(), "sectionId": f.project.section_id,
                "text": "Bounded human post", "mentions": []}),
            ),
            Some(&session),
            None,
            Some(200),
        )
        .await?;
    }
    f.browser(
        &f.project.host,
        "/api/messages",
        Some(
            json!({"requestId": new_id(), "sectionId": f.project.section_id,
            "text": "Over limit", "mentions": []}),
        ),
        Some(&session),
        None,
        Some(429),
    )
    .await?;
    call(
        &node.app,
        "/api/hub-link/grants/revoke",
        Some(json!({"projectId": f.project.project_id, "participationId": p.participation_id})),
        200,
    )
    .await?;
    ensure!(
        f.participation(&p.participation_id, "state")? == "revoked",
        "withdrawal under post limit"
    );
    ensure!(f.calls() == 0, "provider calls: {}", f.calls());
    Ok(())
}

#[tokio::test]
async fn withdrawal_drains_its_control_frame_before_queued_section_delivery_closes_transport(
) -> Result<()> {
    let f = Arc::new(Hub::start(1, None, false).await?);
    let node = f.node("Queued withdrawal", &f.alice).await?;
    let p = f.owner(&node).await?;
    let delayed = Arc::new(AtomicBool::new(false));
    let queued: Arc<Mutex<Option<tokio::task::JoinHandle<Result<Vec<String>>>>>> = Arc::default();
    let (flag, slot, hub) = (delayed.clone(), queued.clone(), f.clone());
    f.transport.intercept_sends(Some(Arc::new(move |frame| {
        if body_kind(frame) != "participation.withdraw" {
            return Outbound::Now;
        }
        flag.store(true, Ordering::SeqCst);
        let hub = hub.clone();
        let task = tokio::spawn(async move {
            let alice = hub.alice.clone();
            hub.post(&alice, &[]).await
        });
        let done = Gate::new();
        let signal = done.clone();
        *locked(&slot) = Some(task);
        let slot = slot.clone();
        Outbound::After(Box::pin(async move {
            loop {
                if locked(&slot).as_ref().is_none_or(|task| task.is_finished()) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            signal.open();
        }))
    })));
    call(
        &node.app,
        "/api/hub-link/grants/revoke",
        Some(json!({"projectId": f.project.project_id, "participationId": p.participation_id})),
        200,
    )
    .await?;
    ensure!(
        delayed.load(Ordering::SeqCst),
        "withdraw frame not intercepted"
    );
    let task = locked(&queued).take().ok_or("queued post missing")?;
    tokio::time::timeout(HUB_WAIT, task)
        .await
        .map_err(|_| "queued post timed out")???;
    f.transport.intercept_sends(None);
    ensure!(
        f.participation(&p.participation_id, "state")? == "revoked",
        "participation state"
    );
    ensure!(f.calls() == 0, "provider calls: {}", f.calls());
    Ok(())
}

#[tokio::test]
async fn signed_pending_requests_stay_node_scoped_approval_stale_revision_offline_decline_and_reconnect_preserve_consent(
) -> Result<()> {
    let f = Hub::start(2, None, false).await?;
    let mut bob = f.node("Pending Bob", &f.bob).await?;
    let alice = f.node("Other Alice", &f.alice).await?;
    f.register_agent(&f.bob, &bob.local_agent(), &bob.node_id, "Bob local")
        .await?;
    let policy = json!({"sectionIds": [f.project.section_id], "scopes": ["read", "invoke"],
        "invocation": {"mode": "all-members"}, "payerBindingRef": "local",
        "localPolicyDigest": bob.local_policy_digest(&bob.local_agent())?, "maxCostMicros": 10000});
    let local = bob.local_agent();
    let too_much = f.propose(&f.bob, &local, &policy, 10_001, None).await;
    ensure!(too_much.is_err(), "over-budget proposal accepted");
    let first = f.propose(&f.bob, &local, &policy, 100, None).await?;
    until("pending request delivered", || async {
        Ok(bob.requests()?.len() == 1)
    })
    .await?;
    let pending = bob.requests()?[0].clone();
    ensure!(pending["projectName"] == "Alpha", "pending: {pending}");
    ensure!(pending["requesterAccountId"] == f.bob, "pending: {pending}");
    ensure!(
        pending["sections"][0]["id"] == f.project.section_id.as_str(),
        "pending: {pending}"
    );
    ensure!(
        pending["grant"]["providerMaxCostMicros"] == 100,
        "pending: {pending}"
    );
    ensure!(
        alice.requests()?.is_empty(),
        "request leaked to another node"
    );
    ensure!(f.calls() == 0, "provider calls: {}", f.calls());
    let changed = f.propose(&f.bob, &local, &policy, 100, None).await?;
    ensure!(
        changed.revision > first.revision,
        "revision did not advance"
    );
    until("new revision delivered", || async {
        Ok(bob
            .requests()?
            .first()
            .is_some_and(|request| request["grant"]["revision"] == changed.revision))
    })
    .await?;
    call(
        &bob.app,
        "/api/hub-link/grants/approve",
        Some(pending["grant"].clone()),
        409,
    )
    .await?;
    let current = bob.requests()?[0]["grant"].clone();
    call(&bob.app, "/api/hub-link/grants/approve", Some(current), 200).await?;
    ensure!(
        f.participation(&changed.participation_id, "state")? == "proposed",
        "local approval skipped hub consent"
    );
    ensure!(bob.requests()?.is_empty(), "approved request still pending");
    ensure!(f.calls() == 0, "provider calls: {}", f.calls());
    f.consent(&changed, None).await?;
    ensure!(
        f.participation(&changed.participation_id, "state")? == "active",
        "consented participation"
    );
    let declined = f.propose(&f.bob, &local, &policy, 100, None).await?;
    until("declined revision delivered", || async {
        Ok(bob
            .requests()?
            .first()
            .is_some_and(|request| request["grant"]["revision"] == declined.revision))
    })
    .await?;
    bob.close_link().await?;
    call(
        &bob.app,
        "/api/hub-link/grants/revoke",
        Some(json!({"projectId": f.project.project_id, "participationId": declined.participation_id})),
        200,
    )
    .await?;
    ensure!(
        bob.status()?["pendingWithdrawals"] == 1,
        "status: {}",
        bob.status()?
    );
    ensure!(bob.requests()?.is_empty(), "declined request still pending");
    bob.restart().await?;
    until("offline decline reconciled", || async {
        Ok(bob.status()?["pendingWithdrawals"] == 0)
    })
    .await?;
    ensure!(
        f.participation(&declined.participation_id, "state")? == "revoked",
        "declined participation"
    );
    ensure!(bob.requests()?.is_empty(), "requests after decline");
    ensure!(f.calls() == 0, "provider calls: {}", f.calls());
    let chunked = Arc::new(AtomicBool::new(false));
    let oversized = Arc::new(AtomicBool::new(false));
    let (chunk_flag, size_flag) = (chunked.clone(), oversized.clone());
    f.transport.observe(Arc::new(move |frame| {
        let body = serde_json::from_str::<Value>(frame).unwrap_or(Value::Null)["body"].clone();
        if body["kind"] == "participation.requests" {
            if frame.len() > 65536 {
                size_flag.store(true, Ordering::SeqCst);
            }
            if body["complete"] != true {
                chunk_flag.store(true, Ordering::SeqCst);
            }
        }
    }));
    for index in 0..90 {
        if index % 25 == 0 {
            f.advance(60_001).await?;
        }
        let agent = new_id();
        f.register_agent(&f.bob, &agent, &bob.node_id, &format!("Pending {index}"))
            .await?;
        f.propose(&f.bob, &agent, &policy, 100, None).await?;
    }
    until("bounded multi-frame snapshot", || async {
        Ok(bob.requests()?.len() == 90)
    })
    .await?;
    ensure!(
        !oversized.load(Ordering::SeqCst),
        "request frame over 64 KiB"
    );
    ensure!(chunked.load(Ordering::SeqCst), "snapshot was not chunked");
    ensure!(bob.status()?["connected"] == true, "node disconnected");
    Ok(())
}

#[tokio::test]
async fn local_owner_deletes_linked_agent_withdraws_hub_participation_and_removes_local_transport_journals(
) -> Result<()> {
    let f = Hub::start(1, None, false).await?;
    let mut node = f.node("Delete linked", &f.alice).await?;
    let p = f.owner(&node).await?;
    call(
        &node.app,
        "/api/agents",
        Some(
            json!({"runtime": "api", "id": new_id(), "name": "Keep", "model": "fixture",
            "instructions": "Help"}),
        ),
        201,
    )
    .await?;
    let post = f.post(&f.alice, &[&p.participation_id]).await?;
    f.done(&post[0], None).await?;
    call(
        &node.app,
        &format!("/api/agents/{}/delete", node.local_agent()),
        Some(json!({"id": new_id()})),
        200,
    )
    .await?;
    let agents = call(&node.app, "/api/agents", None, 200).await?;
    ensure!(
        !agents["agents"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|agent| agent["id"] == node.agent["id"]),
        "deleted agent listed"
    );
    ensure!(
        call(&node.app, "/api/hub-link/history", None, 200).await? == json!([]),
        "hub history kept"
    );
    node.stop().await?;
    let db = Inspector::open(node.app.database())?;
    for table in [
        "hub_link_threads",
        "hub_link_grants",
        "hub_link_turns",
        "hub_link_events",
        "hub_link_results",
        "hub_link_actions",
    ] {
        ensure!(db.count(table)? == 0, "{table} rows remain");
    }
    ensure!(
        db.rows("PRAGMA foreign_key_check", [])?.is_empty(),
        "foreign key violations"
    );
    let _ = &mut node;
    Ok(())
}
