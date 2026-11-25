//! Protected turn waits and events.

use super::{
    crypto::{decrypt_reveal, hmac_hex, keys, sorted, Browser},
    fixture::{agent_body, direct, task_body, until, workspace_snapshot, Fault, Tp, SETTLE},
};
use minarbot_testkit::{
    async_model::Gate,
    db::{Fixture as Writable, Inspector},
    device_host::{Connection, DeviceHost},
    http::Method,
    prelude::*,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

pub struct ProtectedWait {
    pub tp: Tp,
    pub runtime: Arc<DeviceHost>,
    pub connection: Connection,
    pub home: Value,
    pub project: String,
    pub agent: String,
    pub task: String,
    pub wait_id: String,
    pub ownership: Value,
}

async fn connected(connection: &Connection) -> Result<()> {
    until("connected", SETTLE, || async {
        Ok(connection.phase() == "connected")
    })
    .await
}

/// The Node `protectedWaitFixture`: an enrolled home, a task waiting on a
/// protected `ready` event.
pub async fn protected_wait() -> Result<ProtectedWait> {
    let tp = Tp::start().await?;
    let runtime = tp.device("event-home.sqlite")?;
    runtime.enroll(&tp.jwt("user_owner")?, "new").await?;
    let connection = runtime.connect().await?;
    connected(&connection).await?;
    let home = tp.data("/api/devices").await?[0].clone();
    let (project, agent) = (new_id(), new_id());
    let (status, _) = tp
        .send(
            "/api/projects",
            Some(json!({"requestId": new_id(), "id": project, "name": "Events"})),
        )
        .await?;
    ensure!(status == 201, "project: {status}");
    let (status, body) = tp
        .send(
            "/api/agents",
            Some(agent_body(&agent, "Waiter", "Wait", &project, &home)),
        )
        .await?;
    ensure!(status == 201, "agent: {status} {body}");
    let task = new_id();
    let home_id = home["id"].as_str().unwrap_or_default().to_owned();
    let (status, body) = tp
        .send(
            "/api/tasks",
            Some(task_body(
                &task,
                &project,
                &agent,
                &home_id,
                &direct("Wait"),
            )),
        )
        .await?;
    ensure!(status == 202, "task admission: {status} {body}");
    let owned = runtime.start_task(&task).await?;
    let context = runtime
        .context(&task, &owned["ownership"], &owned["revision"])
        .await?;
    let input = json!({"taskId": task, "actionId": new_id(), "sequence": 0, "actionKind": "canonical",
        "ownership": owned["ownership"], "payload": {"toolCall": {"id": "wait-ready", "function": {
            "name": "wait_for_event", "arguments": json!({"event": "ready", "timeoutSeconds": 60}).to_string()}},
            "expectedRevision": context["snapshot"]["taskRevision"].as_i64().unwrap_or(0) + 1},
        "context": context, "grantRevision": 1});
    let pending = runtime
        .action(&input, |_| async { Err("No host effect".into()) })
        .await;
    ensure!(pending.is_err(), "wait action completed instead of pausing");
    let wait_id = minarbot_testkit::wait::until_within("wait result", SETTLE, || async {
        let detail = tp.data(&format!("/api/tasks/{task}")).await?;
        Ok(detail["results"][0]["content"]["id"]
            .as_str()
            .map(str::to_owned))
    })
    .await?;
    runtime.recover().await?;
    Ok(ProtectedWait {
        tp,
        runtime,
        connection,
        home,
        project,
        agent,
        task,
        wait_id,
        ownership: owned["ownership"].clone(),
    })
}

fn event_secret(runtime: &DeviceHost) -> Result<String> {
    runtime
        .keys()?
        .into_iter()
        .find(|(key, _)| key.contains("/event-wait/"))
        .map(|(_, value)| value)
        .ok_or_else(|| "event secret missing".into())
}

fn now_seconds() -> String {
    (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs()))
    .to_string()
}

/// POST a signed event delivery (no session, as an external sender).
async fn send_event(
    tp: &Tp,
    wait: &str,
    secret: &str,
    text: &str,
    event_id: &str,
    time: &str,
    extra: &[(&str, &str)],
) -> Result<(u16, Value)> {
    let signature = hmac_hex(secret, &format!("{time}\n{event_id}\n{text}"));
    let mut headers = vec![
        ("Content-Type", "application/json".to_owned()),
        ("X-Minarbot-Time", time.to_owned()),
        ("X-Minarbot-Id", event_id.to_owned()),
        ("X-Minarbot-Signature", signature),
    ];
    for (name, value) in extra {
        headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
        headers.push((name, (*value).to_owned()));
    }
    let mut request = tp
        .app
        .server
        .client()?
        .post(format!("{}/api/turn-waits/{wait}/events", tp.origin()));
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request.body(text.to_owned()).send().await?;
    let status = response.status().as_u16();
    Ok((status, response.json().await.unwrap_or(Value::Null)))
}

#[tokio::test]
async fn protected_turn_event_verifies_on_home_saves_one_receipt_and_resumes_with_exact_raw_payload(
) -> Result<()> {
    let mut f = protected_wait().await?;
    let result = async {
        let secret = event_secret(&f.runtime)?;
        let delivery = new_id();
        let small = r#"{ "event": "ready", "payload": {"message":"hello 🌏"} }"#;
        let body = format!("{small}{}", " ".repeat(65536 - small.len()));
        let timestamp = now_seconds();
        let wait = f.wait_id.clone();
        macro_rules! event {
            ($text:expr, $id:expr, $time:expr, $extra:expr) => {
                send_event(&f.tp, &wait, &secret, $text, $id, $time, $extra)
            };
            () => {
                send_event(&f.tp, &wait, &secret, &body, &delivery, &timestamp, &[])
            };
        }
        let oversize = event!(&format!("{body} "), &delivery, &timestamp, &[]).await?;
        ensure!(oversize.0 == 413, "oversize: {:?}", oversize);
        connected(&f.connection).await?;
        let origin = f.tp.origin();
        ensure!(
            event!(&body, &delivery, &timestamp, &[("Origin", origin.as_str())]).await?.0 == 403,
            "browser origin accepted"
        );
        let zero = "0".repeat(64);
        let wrong = event!(&body, &delivery, &timestamp, &[("X-Minarbot-Signature", zero.as_str())])
            .await?;
        ensure!(wrong.0 == 401, "wrong signature: {wrong:?}");
        ensure!(
            event!(&body, &delivery, "1", &[]).await?.0 == 401,
            "Stale unaccepted delivery denies"
        );
        ensure!(event!("{", &delivery, &timestamp, &[]).await?.0 == 400, "malformed JSON");
        ensure!(
            event!(r#"{"event":"other"}"#, &delivery, &timestamp, &[]).await?.0 == 409,
            "other event"
        );
        ensure!(
            event!(r#"{"event":"ready","payload":1e999}"#, &delivery, &timestamp, &[]).await?.0 == 400,
            "non-finite payload"
        );
        ensure!(
            event!(r#"{"event":"ready","extra":true}"#, &delivery, &timestamp, &[]).await?.0 == 400,
            "extra field"
        );
        f.connection.stop().await?;
        ensure!(
            event!().await?.1["error"]["code"] == "device_offline",
            "offline home accepted"
        );
        let reconnected = f.runtime.connect().await?;
        connected(&reconnected).await?;
        Writable::open(f.tp.database())?.execute_batch(
            "CREATE TRIGGER fail_event_receipt BEFORE INSERT ON receipts WHEN NEW.scope LIKE 'turn-wait-event:%' BEGIN SELECT RAISE(ABORT,'fixture event failure'); END",
        )?;
        ensure!(event!().await?.0 == 500, "receipt failure");
        ensure!(
            f.tp.data(&format!("/api/turn-waits/{wait}")).await?["status"] == "waiting",
            "Receipt storage failure rolls back delivery and tool result"
        );
        Writable::open(f.tp.database())?.execute_batch("DROP TRIGGER fail_event_receipt")?;
        let accepted = event!().await?;
        ensure!(accepted.0 == 200, "accepted: {accepted:?}");
        ensure!(accepted.1["data"]["accepted"] == true, "accepted: {accepted:?}");
        ensure!(accepted.1["data"]["delivery"] == delivery.as_str(), "delivery");
        ensure!(event!().await?.1 == accepted.1, "retry differs");
        ensure!(
            event!(&body, &delivery, "1", &[]).await?.1 == accepted.1,
            "Accepted retry skips freshness after verifying HMAC"
        );
        ensure!(
            event!(&body, &delivery, "1", &[("X-Minarbot-Signature", zero.as_str())]).await?.0 == 401,
            "accepted retry with a bad signature"
        );
        ensure!(
            event!(small, &delivery, &timestamp, &[]).await?.0 == 409,
            "Whitespace is part of exact raw body digest"
        );
        ensure!(
            event!(&body, &new_id(), &timestamp, &[]).await?.0 == 409,
            "second delivery"
        );
        ensure!(
            f.tp.data(&format!("/api/turn-waits/{wait}")).await?["status"] == "received",
            "wait not received"
        );
        let detail = f.tp.data(&format!("/api/tasks/{}", f.task)).await?;
        let next = f
            .runtime
            .context(&f.task, &f.ownership, &detail["revision"])
            .await?;
        let journal = next["snapshot"]["toolJournal"].as_array().cloned().unwrap_or_default();
        ensure!(
            journal
                .iter()
                .filter(|row| row["kind"] == "flow_tool"
                    && row["callId"] == "wait-ready"
                    && row["result"]["payload"]["message"] == "hello 🌏")
                .count()
                == 1,
            "journal: {journal:?}"
        );
        let generation = reconnected.generation();
        f.tp.restart().await?;
        until("reconnected after restart", SETTLE, || async {
            Ok(reconnected.phase() == "connected" && reconnected.generation() > generation)
        })
        .await?;
        ensure!(
            event!().await?.1 == accepted.1,
            "Canonical restart retains exact receipt"
        );
        let after = f.tp.data(&format!("/api/tasks/{}", f.task)).await?;
        ensure!(
            after["revision"] == detail["revision"],
            "Retry cannot advance task revision"
        );
        let (status, _) = f
            .tp
            .send(
                &format!("/api/tasks/{}/control", f.task),
                Some(json!({"requestId": new_id(), "expectedRevision": after["revision"],
                    "operation": "cancel"})),
            )
            .await?;
        ensure!(status == 200, "cancel: {status}");
        ensure!(
            event!().await?.1 == accepted.1,
            "Accepted delivery remains evidence after task ends"
        );
        ensure!(
            f.runtime
                .keys()?
                .keys()
                .filter(|key| key.contains("/event-wait/"))
                .count()
                == 1,
            "event secrets"
        );
        let owner = f.tp.data("/api/session").await?["accountId"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let snapshot = workspace_snapshot(&f.tp, Some(&owner))?;
        reconnected.stop().await?;
        f.tp.restore_from(&snapshot).await?;
        ensure!(
            event!().await?.0 == 404,
            "Restore disables old event admissions"
        );
        let (status, restored) = f.tp.send(&format!("/api/turn-waits/{wait}"), None).await?;
        ensure!(status == 200, "restored wait: {status}");
        ensure!(restored["data"]["status"] == "received", "restored: {restored}");
        ensure!(
            restored["data"]["receipt"] == accepted.1["receipt"],
            "Owner retains delivered evidence while recovery is read-only"
        );
        let (status, _) = f
            .tp
            .send_as(&format!("/api/turn-waits/{wait}"), None, "user_collaborator", None)
            .await?;
        ensure!(status == 404, "collaborator read the wait: {status}");
        Ok(())
    }
    .await;
    f.runtime.close().await?;
    f.tp.close().await?;
    result
}

async fn protected_turn_event_rejects_delayed_verification(change: &'static str) -> Result<()> {
    let f = protected_wait().await?;
    let gate = Gate::new();
    let held = Arc::new(AtomicBool::new(false));
    let result = async {
        let secret = event_secret(&f.runtime)?;
        let delivery = new_id();
        let body = r#"{"event":"ready","payload":{"ok":true}}"#;
        let time = now_seconds();
        let (hold, flag) = (gate.clone(), held.clone());
        f.tp.fault(Some(Arc::new(move |exchange| {
            if exchange.path.ends_with("/wait-event-results") {
                flag.store(true, Ordering::SeqCst);
                let hold = hold.clone();
                return Fault::Hold(Box::pin(async move { hold.wait().await }));
            }
            Fault::Pass
        })));
        let (tp_origin, client) = (f.tp.origin(), f.tp.app.server.client()?);
        let wait = f.wait_id.clone();
        let fire = move || {
            let (origin, client, secret, wait, delivery, time) = (
                tp_origin.clone(),
                client.clone(),
                secret.clone(),
                wait.clone(),
                delivery.clone(),
                time.clone(),
            );
            async move {
                let response = client
                    .post(format!("{origin}/api/turn-waits/{wait}/events"))
                    .header("Content-Type", "application/json")
                    .header("X-Minarbot-Time", &time)
                    .header("X-Minarbot-Id", &delivery)
                    .header(
                        "X-Minarbot-Signature",
                        hmac_hex(&secret, &format!("{time}\n{delivery}\n{body}")),
                    )
                    .body(body)
                    .send()
                    .await?;
                let status = response.status().as_u16();
                Ok::<(u16, Value), minarbot_testkit::Error>((
                    status,
                    response.json().await.unwrap_or(Value::Null),
                ))
            }
        };
        let pending = tokio::spawn(fire());
        until("event held", SETTLE, || async {
            Ok(held.load(Ordering::SeqCst))
        })
        .await?;
        ensure!(fire().await?.0 == 429, "At most one pending event per home");
        ensure!(
            f.tp.data(&format!("/api/turn-waits/{}", f.wait_id)).await?["status"] == "waiting",
            "wait advanced while verification is pending"
        );
        match change {
            "rules" => {
                let (status, _) =
                    f.tp.send_as(
                        &format!("/api/projects/{}/agents/{}/rules", f.project, f.agent),
                        Some(
                            json!({"requestId": new_id(), "expectedParticipationRevision": 1,
                            "rules": "Changed context"}),
                        ),
                        "user_owner",
                        Some(Method::PUT),
                    )
                    .await?;
                ensure!(status == 200, "rules: {status}");
            }
            "revoke" => {
                let device = f.tp.data("/api/devices").await?[0].clone();
                let (status, _) =
                    f.tp.send(
                        &format!(
                            "/api/devices/{}/revoke",
                            device["id"].as_str().unwrap_or_default()
                        ),
                        Some(
                            json!({"requestId": new_id(), "expectedRevision": device["revision"]}),
                        ),
                    )
                    .await?;
                ensure!(status == 200, "revoke: {status}");
            }
            "timeout" => {}
            _ => {
                let task = f.tp.data(&format!("/api/tasks/{}", f.task)).await?;
                let (status, _) =
                    f.tp.send(
                        &format!("/api/tasks/{}/control", f.task),
                        Some(
                            json!({"requestId": new_id(), "expectedRevision": task["revision"],
                            "operation": change}),
                        ),
                    )
                    .await?;
                ensure!(status == 200, "{change}: {status}");
            }
        }
        let denied = if change == "timeout" {
            let value = pending.await??;
            gate.open();
            f.tp.fault(None);
            value
        } else {
            gate.open();
            f.tp.fault(None);
            pending.await??
        };
        let expected = if change == "revoke" || change == "timeout" {
            503
        } else {
            409
        };
        ensure!(denied.0 == expected, "{change}: {denied:?}");
        if ["rules", "pause", "timeout"].contains(&change) {
            ensure!(
                fire().await?.0 == 200,
                "Fresh verification may accept without replaying an effect"
            );
            if change == "pause" {
                ensure!(
                    f.tp.data(&format!("/api/tasks/{}", f.task)).await?["state"] == "paused",
                    "Event cannot undo manual pause"
                );
            }
        }
        Ok(())
    }
    .await;
    gate.open();
    f.tp.fault(None);
    f.runtime.close().await?;
    f.tp.close().await?;
    result
}

#[tokio::test]
async fn protected_turn_event_rejects_delayed_verification_after_scope_or_connection_changes_pause(
) -> Result<()> {
    protected_turn_event_rejects_delayed_verification("pause").await
}

#[tokio::test]
async fn protected_turn_event_rejects_delayed_verification_after_scope_or_connection_changes_rules(
) -> Result<()> {
    protected_turn_event_rejects_delayed_verification("rules").await
}

#[tokio::test]
async fn protected_turn_event_rejects_delayed_verification_after_scope_or_connection_changes_cancel(
) -> Result<()> {
    protected_turn_event_rejects_delayed_verification("cancel").await
}

#[tokio::test]
async fn protected_turn_event_rejects_delayed_verification_after_scope_or_connection_changes_revoke(
) -> Result<()> {
    protected_turn_event_rejects_delayed_verification("revoke").await
}

#[tokio::test]
async fn protected_turn_event_rejects_delayed_verification_after_scope_or_connection_changes_timeout(
) -> Result<()> {
    protected_turn_event_rejects_delayed_verification("timeout").await
}

fn device_key(device: &Value) -> Result<String> {
    let descriptor: Value =
        serde_json::from_str(device["descriptor_json"].as_str().unwrap_or("null"))?;
    Ok(descriptor["encryptionPublicKey"]
        .as_str()
        .unwrap_or_default()
        .to_owned())
}

fn holds_secret(tp: &Tp, tables: &[&str], secret: &str) -> Result<Option<String>> {
    let db = Inspector::open(tp.database())?;
    for table in tables {
        if json!(db.rows(&format!("SELECT * FROM {table}"), [])?)
            .to_string()
            .contains(secret)
        {
            return Ok(Some((*table).to_owned()));
        }
    }
    Ok(None)
}

/// Hold `/wait-reveal-results` until `gate` opens; `held` turns true.
fn hold_reveals(tp: &Tp, gate: &Gate, held: &Arc<AtomicBool>) {
    let (gate, held) = (gate.clone(), held.clone());
    tp.fault(Some(Arc::new(move |exchange| {
        if exchange.path.ends_with("/wait-reveal-results") {
            held.store(true, Ordering::SeqCst);
            let gate = gate.clone();
            return Fault::Hold(Box::pin(async move { gate.wait().await }));
        }
        Fault::Pass
    })));
}

#[tokio::test]
async fn protected_turn_wait_uses_outbound_stream_and_recovers_lost_host_ack_without_another_key_or_wait(
) -> Result<()> {
    let mut f = Tp::start().await?;
    let database = f.dir().join("wait-home.sqlite");
    let result = async {
        let mut runtime = f.device("wait-home.sqlite")?;
        runtime.enroll(&f.jwt("user_owner")?, "new").await?;
        let connection = runtime.connect().await?;
        connected(&connection).await?;
        let home = f.data("/api/devices").await?[0].clone();
        let (project, agent) = (new_id(), new_id());
        let (status, _) = f
            .send("/api/projects", Some(json!({"requestId": new_id(), "id": project, "name": "Waits"})))
            .await?;
        ensure!(status == 201, "project: {status}");
        let (status, _) = f
            .send("/api/agents", Some(agent_body(&agent, "Waiter", "Wait", &project, &home)))
            .await?;
        ensure!(status == 201, "agent: {status}");
        let task = new_id();
        let home_id = home["id"].as_str().unwrap_or_default().to_owned();
        let (status, body) = f
            .send(
                "/api/tasks",
                Some(task_body(&task, &project, &agent, &home_id, &direct("Wait for ready"))),
            )
            .await?;
        ensure!(status == 202, "task: {status} {body}");
        let owned = runtime.start_task(&task).await?;
        let context = runtime
            .context(&task, &owned["ownership"], &owned["revision"])
            .await?;
        let input = json!({"taskId": task, "actionId": new_id(), "sequence": 0,
            "actionKind": "canonical", "ownership": owned["ownership"], "payload": {"toolCall": {
                "id": "wait-ready", "function": {"name": "wait_for_event", "arguments":
                    json!({"event": "ready", "timeoutSeconds": 60}).to_string()}},
                "expectedRevision": context["snapshot"]["taskRevision"].as_i64().unwrap_or(0) + 1},
            "context": context, "grantRevision": 1});
        let lost = Arc::new(AtomicBool::new(false));
        let acked = Arc::new(std::sync::Mutex::new(None::<u16>));
        let (flag, status_seen) = (lost.clone(), acked.clone());
        f.fault(Some(Arc::new(move |exchange| {
            if exchange.path.ends_with("/wait-secret-results") && !flag.load(Ordering::SeqCst) {
                let (flag, status_seen) = (flag.clone(), status_seen.clone());
                return Fault::LoseReply(Box::new(move |status| {
                    *super::fixture::locked(&status_seen) = Some(status);
                    flag.store(true, Ordering::SeqCst);
                    true
                }));
            }
            Fault::Pass
        })));
        let action = runtime
            .action(&input, |_| async { Err("Wait cannot execute a host effect".into()) })
            .await;
        ensure!(
            action
                .as_ref()
                .is_err_and(|error| minarbot_testkit::device_host::is_code(error, "device_offline")),
            "lost ACK: {action:?}"
        );
        until("ACK lost", SETTLE, || async { Ok(lost.load(Ordering::SeqCst)) }).await?;
        ensure!(
            *super::fixture::locked(&acked) == Some(200),
            "lost ACK status {:?}",
            *super::fixture::locked(&acked)
        );
        f.fault(None);
        connection.stop().await?;
        let posts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = posts.clone();
        f.fault(Some(Arc::new(move |exchange| {
            if exchange.path.ends_with("/wait-secret-results") {
                counter.fetch_add(1, Ordering::SeqCst);
            }
            Fault::Pass
        })));
        let probe = f.device("wait-home.sqlite")?;
        let probe_connection = probe.connect().await?;
        connected(&probe_connection).await?;
        ensure!(probe.recover().await? == json!([]), "probe recovered work");
        let state = Inspector::open(&database)?
            .row("SELECT state FROM device_wait_secrets", [])?;
        ensure!(
            posts.load(Ordering::SeqCst) == 0 && state["state"] == "ready",
            "Second host must not ACK or change wait evidence while original runner holds lock: posts {} {state}",
            posts.load(Ordering::SeqCst)
        );
        probe.close().await?;
        f.fault(None);
        runtime.close().await?;
        f.restart().await?;
        runtime = f.device("wait-home.sqlite")?;
        let connection = runtime.connect().await?;
        connected(&connection).await?;
        until("wait secret acknowledged", SETTLE, || async {
            Ok(Inspector::open(&database)?
                .row("SELECT state FROM device_wait_secrets", [])?["state"]
                == "acknowledged")
        })
        .await?;
        let canonical = Inspector::open(f.database())?;
        ensure!(
            canonical.scalar::<i64>("SELECT count(*) FROM wait_provisions", [])? == 1,
            "provisions"
        );
        ensure!(
            canonical.scalar::<i64>("SELECT count(*) FROM turn_waits", [])? == 1,
            "waits"
        );
        ensure!(
            canonical.scalar::<String>("SELECT state FROM wait_provisions", [])? == "activated",
            "provision state"
        );
        ensure!(
            canonical.scalar::<i64>("SELECT count(*) FROM receipts WHERE scope='wait-secret-results'", [])?
                == 1,
            "secret receipts"
        );
        let secrets: Vec<String> = runtime
            .keys()?
            .into_iter()
            .filter(|(key, _)| key.contains("/event-wait/"))
            .map(|(_, value)| value)
            .collect();
        ensure!(secrets.len() == 1, "secrets: {}", secrets.len());
        let secret = secrets[0].clone();
        let leaked = holds_secret(&f, &["wait_provisions", "turn_waits", "receipts", "task_results"], &secret)?;
        ensure!(leaked.is_none(), "secret stored in {leaked:?}");
        runtime.recover().await?;
        let detail = f.data(&format!("/api/tasks/{task}")).await?;
        ensure!(detail["results"].as_array().map(Vec::len) == Some(1), "results: {detail}");
        ensure!(
            detail["results"][0]["content"]["status"] == "waiting",
            "result: {detail}"
        );
        let ran = runtime
            .run_task(&task, |_| async { Err("Waiting task must not call provider".into()) })
            .await?;
        ensure!(ran["state"] == "event_pending", "run: {ran}");
        let wait = detail["results"][0]["content"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let path = format!("/api/turn-waits/{wait}/connection");
        let (status, metadata) = f.send(&path, None).await?;
        ensure!(status == 200, "connection metadata: {status}");
        ensure!(
            keys(&metadata["data"])
                == sorted(&["id", "event", "deadline", "status", "secretGeneration", "sourceDeviceId"]),
            "metadata: {metadata}"
        );
        let mut browser = Browser::new()?;
        let reveal = json!({"requestId": new_id(), "ephemeralPublicKey": browser.public,
            "expectedSecretGeneration": 1});
        let (status, encrypted) = f.send(&path, Some(reveal.clone())).await?;
        ensure!(status == 200, "reveal: {status} {encrypted}");
        let value = encrypted["data"].clone();
        let devices = f.data("/api/devices").await?;
        let device = devices
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["id"] == home["id"]).cloned())
            .ok_or("home device missing")?;
        let owner = f.data("/api/session").await?["accountId"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        ensure!(
            keys(&value)
                == sorted(&["requestId", "waitKey", "sourceDeviceId", "deviceGeneration",
                    "secretGeneration", "expiresAt", "iv", "ciphertext", "tag"]),
            "reveal fields: {value}"
        );
        ensure!(!encrypted.to_string().contains(&secret), "reveal leaked the secret");
        let request = reveal["requestId"].as_str().unwrap_or_default().to_owned();
        let shared = browser.shared(&device_key(&device)?)?;
        ensure!(
            decrypt_reveal(&shared, &request, &value, &owner)? == secret,
            "decrypted secret differs"
        );
        let other = Browser::new()?.shared(&device_key(&device)?)?;
        ensure!(
            decrypt_reveal(&other, &request, &value, &owner).is_err(),
            "another browser key decrypted the reveal"
        );
        ensure!(
            decrypt_reveal(&shared, &request, &value, &new_id()).is_err(),
            "another owner decrypted the reveal"
        );
        let with = |changes: Value| {
            let mut body = reveal.clone();
            body["requestId"] = json!(new_id());
            if let (Some(body), Some(changes)) = (body.as_object_mut(), changes.as_object()) {
                for (key, value) in changes {
                    body.insert(key.clone(), value.clone());
                }
            }
            body
        };
        ensure!(
            f.send(&path, Some(with(json!({"expectedSecretGeneration": 2})))).await?.0 == 409,
            "stale secret generation"
        );
        ensure!(
            f.send(&path, Some(with(json!({"ephemeralPublicKey": "invalid"})))).await?.0 == 400,
            "invalid key"
        );
        ensure!(
            f.send_as(&path, Some(with(json!({}))), "user_collaborator", None).await?.0 == 404,
            "collaborator reveal"
        );
        let foreign = f
            .app
            .server
            .client()?
            .post(format!("{}{path}", f.origin()))
            .header("Authorization", format!("Bearer {}", f.jwt("user_owner")?))
            .header("Origin", "https://foreign.invalid")
            .header("Content-Type", "application/json")
            .header("X-Minarbot-Protocol", "1")
            .header("X-Minarbot-Request-Id", request.as_str())
            .body(reveal.to_string())
            .send()
            .await?
            .status()
            .as_u16();
        ensure!(foreign == 403, "foreign origin reveal: {foreign}");
        let gate = Gate::new();
        let held = Arc::new(AtomicBool::new(false));
        hold_reveals(&f, &gate, &held);
        let pending = f.send(&path, Some(with(json!({}))));
        let scope = async {
            until("reveal held", SETTLE, || async { Ok(held.load(Ordering::SeqCst)) }).await?;
            ensure!(f.send(&path, Some(with(json!({})))).await?.0 == 429, "second pending reveal");
            let latest = f.data(&format!("/api/tasks/{task}")).await?;
            let (status, _) = f
                .send(
                    &format!("/api/tasks/{task}/control"),
                    Some(json!({"requestId": new_id(), "expectedRevision": latest["revision"],
                        "operation": "pause"})),
                )
                .await?;
            ensure!(status == 200, "pause: {status}");
            gate.open();
            Ok::<(), minarbot_testkit::Error>(())
        };
        let (delayed, scope) = tokio::join!(pending, scope);
        gate.open();
        f.fault(None);
        scope?;
        ensure!(
            delayed?.0 == 409,
            "Delayed reveal cannot cross a task scope change"
        );
        let (status, fresh) = f.send(&path, Some(with(json!({})))).await?;
        ensure!(status == 200, "Paused wait can obtain a fresh owner connection");
        ensure!(fresh["data"]["iv"] != value["iv"], "reveal IV reused");
        let gate = Gate::new();
        let held = Arc::new(AtomicBool::new(false));
        hold_reveals(&f, &gate, &held);
        let pending = f.send(&path, Some(with(json!({}))));
        let rules = async {
            until("reveal held", SETTLE, || async { Ok(held.load(Ordering::SeqCst)) }).await?;
            let (status, _) = f
                .send_as(
                    &format!("/api/projects/{project}/agents/{agent}/rules"),
                    Some(json!({"requestId": new_id(), "expectedParticipationRevision": 1,
                        "rules": "Updated project context"})),
                    "user_owner",
                    Some(Method::PUT),
                )
                .await?;
            ensure!(status == 200, "rules: {status}");
            gate.open();
            Ok::<(), minarbot_testkit::Error>(())
        };
        let (delayed, rules) = tokio::join!(pending, rules);
        gate.open();
        f.fault(None);
        rules?;
        ensure!(
            delayed?.0 == 409,
            "Delayed reveal cannot cross project rules/capability revisions"
        );
        let retained: Vec<String> = runtime
            .keys()?
            .into_iter()
            .filter(|(key, _)| key.contains("/event-wait/"))
            .map(|(_, value)| value)
            .collect();
        ensure!(retained.len() == 1, "retained secrets");
        ensure!(retained[0] == secret, "secret rotated");
        let leaked = holds_secret(
            &f,
            &["wait_provisions", "turn_waits", "receipts", "task_results", "sync_events"],
            &secret,
        )?;
        ensure!(leaked.is_none(), "secret stored in {leaked:?}");
        connection.stop().await?;
        ensure!(
            f.send(&path, Some(with(json!({})))).await?.0 == 503,
            "offline reveal"
        );
        ensure!(
            f.send(&path, None).await?.0 == 200,
            "Offline wait metadata remains owner-readable"
        );
        Ok(())
    }
    .await;
    f.fault(None);
    f.close().await?;
    result
}
