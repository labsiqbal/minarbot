//! Protected workflow waits (`rustWorkflowConfigurationFixture`).

use super::{
    crypto::{decrypt_reveal, hmac_hex, keys, sorted, Browser},
    fixture::{agent_body, until, workspace_snapshot, Fault, Tp, DEVICE_RECONNECT, SETTLE},
};
use minarbot_testkit::{
    async_model::Gate,
    device_host::{enroll, ConnectionProcess, HostOptions},
    http::Method,
    prelude::*,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// Workflow configuration with an enrolled home (`prepareWorkflowConfiguration`).
pub struct WorkflowHome {
    pub tp: Tp,
    pub options: HostOptions,
    pub home: Value,
    pub project: String,
    pub other: String,
    pub agent: String,
    pub workflow: String,
    pub list: String,
    pub detail: String,
}

impl WorkflowHome {
    pub async fn start() -> Result<Self> {
        let tp = Tp::start().await?;
        let options = HostOptions::new(&tp.dir().join("workflow-home.sqlite"), tp.proxy.origin())?;
        enroll(&options, &tp.jwt("user_owner")?, "new").await?;
        let home = tp.data("/api/devices").await?[0].clone();
        let (project, other, agent, workflow) = (new_id(), new_id(), new_id(), new_id());
        for id in [&project, &other] {
            tp.project(id, "Workflows").await?;
        }
        let (status, body) = tp
            .send(
                "/api/agents",
                Some(agent_body(
                    &agent,
                    "Workflow agent",
                    "Help",
                    &project,
                    &home,
                )),
            )
            .await?;
        ensure!(status == 201, "agent: {status} {body}");
        Ok(Self {
            list: format!("/api/projects/{project}/agents/{agent}/workflows"),
            detail: format!("/api/projects/{project}/workflows/{workflow}"),
            tp,
            options,
            home,
            project,
            other,
            agent,
            workflow,
        })
    }

    pub fn connect(&self) -> Result<ConnectionProcess> {
        ConnectionProcess::start(&self.options)
    }

    pub fn secret(&self, except: Option<&str>) -> Result<String> {
        self.options
            .keys()?
            .into_iter()
            .filter(|(key, value)| key.contains("/event-wait/") && Some(value.as_str()) != except)
            .map(|(_, value)| value)
            .next()
            .ok_or_else(|| "event secret missing".into())
    }
}

pub async fn connected(connection: &ConnectionProcess) -> Result<()> {
    until("connected", SETTLE, || async {
        Ok(connection.phase() == "connected")
    })
    .await
}

/// `protectedWorkflowWaitFixture`.
pub struct WorkflowWait {
    pub w: WorkflowHome,
    pub connection: ConnectionProcess,
    pub initiator: String,
    pub run: String,
    pub path: String,
    pub wait_path: String,
}

pub async fn workflow_wait(subject: &str, timeout: i64) -> Result<WorkflowWait> {
    let w = WorkflowHome::start().await?;
    let connection = w.connect()?;
    connected(&connection).await?;
    let graph = json!({"entry": "event", "maxSteps": 2, "nodes": [
        {"id": "event", "kind": "wait", "event": "ready", "timeoutSeconds": timeout, "next": "out"},
        {"id": "out", "kind": "output"}]});
    let (status, body) =
        w.tp.send(
            &w.list,
            Some(
                json!({"requestId": w.workflow, "id": w.workflow, "revision": 0,
                "name": "Protected wait", "graph": graph}),
            ),
        )
        .await?;
    ensure!(status == 201, "workflow: {status} {body}");
    let initiator = w.tp.send_as("/api/session", None, subject, None).await?.1["data"]["accountId"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    if subject != "user_owner" {
        let db = w.tp.write()?;
        db.execute(
            "INSERT INTO project_members VALUES (?,?,'member',1,0)",
            [&w.project, &initiator],
        )?;
        db.execute(
            "INSERT INTO collaborator_agents VALUES (?,?,?,1,1)",
            [&w.project, &initiator, &w.agent],
        )?;
    }
    let run = new_id();
    let path = format!("/api/projects/{}/workflow-runs/{run}", w.project);
    let wait_path = format!("/api/workflow-waits/{run}/1");
    let (status, body) =
        w.tp.send_as(
            &format!("{}/run", w.detail),
            Some(json!({"requestId": run, "id": run, "revision": 1, "input": "Synthetic"})),
            subject,
            None,
        )
        .await?;
    ensure!(status == 201, "run: {status} {body}");
    until("wait waiting", SETTLE, || async {
        Ok(w.tp.send(&wait_path, None).await?.1["data"]["status"] == "waiting")
    })
    .await?;
    Ok(WorkflowWait {
        w,
        connection,
        initiator,
        run,
        path,
        wait_path,
    })
}

fn now_seconds() -> String {
    (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs()))
    .to_string()
}

pub async fn signed_event(
    tp: &Tp,
    path: &str,
    secret: &str,
    text: &str,
    event_id: &str,
    time: &str,
    extra: &[(&str, &str)],
) -> Result<(u16, Value)> {
    let mut request = tp
        .app
        .server
        .client()?
        .post(format!("{}{path}", tp.origin()))
        .header("Content-Type", "application/json")
        .header("X-Minarbot-Time", time)
        .header("X-Minarbot-Id", event_id);
    let mut signature = Some(hmac_hex(secret, &format!("{time}\n{event_id}\n{text}")));
    for (name, value) in extra {
        if name.eq_ignore_ascii_case("X-Minarbot-Signature") {
            signature = None;
        }
        request = request.header(*name, *value);
    }
    if let Some(signature) = signature {
        request = request.header("X-Minarbot-Signature", signature);
    }
    let response = request.body(text.to_owned()).send().await?;
    let status = response.status().as_u16();
    Ok((status, response.json().await.unwrap_or(Value::Null)))
}

fn device_key(device: &Value) -> Result<String> {
    let descriptor: Value =
        serde_json::from_str(device["descriptor_json"].as_str().unwrap_or("null"))?;
    Ok(descriptor["encryptionPublicKey"]
        .as_str()
        .unwrap_or_default()
        .to_owned())
}

fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

#[tokio::test]
async fn protected_workflow_connection_reveals_only_ciphertext_to_its_current_owner_browser(
) -> Result<()> {
    let mut f = workflow_wait("user_owner", 60).await?;
    let result = async {
        let mut browser = Browser::new()?;
        let path = format!("{}/connection", f.wait_path);
        let reveal = json!({"requestId": new_id(), "ephemeralPublicKey": browser.public,
            "expectedSecretGeneration": 1});
        let (status, encrypted) = f.w.tp.send(&path, Some(reveal.clone())).await?;
        ensure!(status == 200, "reveal: {status} {encrypted}");
        let value = encrypted["data"].clone();
        let owner = f.w.tp.data("/api/session").await?["accountId"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let device = f.w.tp.data("/api/devices").await?[0].clone();
        let secret = f.w.secret(None)?;
        ensure!(
            value["waitKey"] == json!(["workflow", f.run, 1]).to_string(),
            "wait key: {value}"
        );
        ensure!(value["sourceDeviceId"] == f.w.home["id"], "source: {value}");
        ensure!(
            keys(&value)
                == sorted(&[
                    "requestId",
                    "waitKey",
                    "sourceDeviceId",
                    "deviceGeneration",
                    "secretGeneration",
                    "expiresAt",
                    "iv",
                    "ciphertext",
                    "tag"
                ]),
            "fields: {value}"
        );
        let request = reveal["requestId"].as_str().unwrap_or_default().to_owned();
        let shared = browser.shared(&device_key(&device)?)?;
        ensure!(
            decrypt_reveal(&shared, &request, &value, &owner)? == secret,
            "secret"
        );
        ensure!(
            decrypt_reveal(&shared, &request, &value, &new_id()).is_err(),
            "another owner decrypted"
        );
        let other = Browser::new()?.shared(&device_key(&device)?)?;
        ensure!(
            decrypt_reveal(&other, &request, &value, &owner).is_err(),
            "another key decrypted"
        );
        let expires = value["expiresAt"].as_i64().unwrap_or(0);
        ensure!(
            expires > wall_ms() && expires <= wall_ms() + 60_000,
            "expiry: {expires}"
        );
        let (status, metadata) = f.w.tp.send(&path, None).await?;
        ensure!(status == 200, "metadata: {status}");
        ensure!(
            !metadata.to_string().contains(&secret),
            "metadata leaked the secret"
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
            f.w.tp
                .send(&path, Some(with(json!({"expectedSecretGeneration": 2}))))
                .await?
                .0
                == 409,
            "stale generation"
        );
        ensure!(
            f.w.tp
                .send(&path, Some(with(json!({"ephemeralPublicKey": "invalid"}))))
                .await?
                .0
                == 400,
            "invalid key"
        );
        let collaborator =
            f.w.tp
                .send_as("/api/session", None, "user_collaborator", None)
                .await?
                .1["data"]["accountId"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
        {
            let db = f.w.tp.write()?;
            db.execute(
                "INSERT INTO project_members VALUES (?,?,'member',1,0)",
                [&f.w.project, &collaborator],
            )?;
            db.execute(
                "INSERT INTO collaborator_agents VALUES (?,?,?,1,1)",
                [&f.w.project, &collaborator, &f.w.agent],
            )?;
        }
        ensure!(
            f.w.tp
                .send_as(&f.wait_path, None, "user_collaborator", None)
                .await?
                .0
                == 200,
            "collaborator reads the wait"
        );
        ensure!(
            f.w.tp
                .send_as(&path, Some(with(json!({}))), "user_collaborator", None)
                .await?
                .0
                == 403,
            "collaborator reveal"
        );
        let foreign =
            f.w.tp
                .app
                .server
                .client()?
                .post(format!("{}{path}", f.w.tp.origin()))
                .header(
                    "Authorization",
                    format!("Bearer {}", f.w.tp.jwt("user_owner")?),
                )
                .header("Origin", "https://foreign.invalid")
                .header("Content-Type", "application/json")
                .header("X-Minarbot-Protocol", "1")
                .header("X-Minarbot-Request-Id", request.as_str())
                .body(reveal.to_string())
                .send()
                .await?
                .status()
                .as_u16();
        ensure!(foreign == 403, "foreign origin: {foreign}");
        f.connection.stop().await?;
        ensure!(
            f.w.tp.send(&path, Some(with(json!({})))).await?.0 == 503,
            "offline reveal"
        );
        ensure!(f.w.tp.send(&path, None).await?.0 == 200, "offline metadata");
        Ok(())
    }
    .await;
    f.connection.stop().await?;
    f.w.tp.close().await?;
    result
}

#[tokio::test]
async fn protected_workflow_event_commits_one_transition_and_keeps_exact_retries_after_advancement_restart_and_restore(
) -> Result<()> {
    let mut f = workflow_wait("user_owner", 60).await?;
    let result = async {
        let secret = f.w.secret(None)?;
        let delivery = new_id();
        let timestamp = now_seconds();
        let small = r#"{ "event": "ready", "eventTime": 1234, "payload": {"message":"hello 🌏"} }"#;
        let body = format!("{small}{}", " ".repeat(65536 - small.len()));
        let events = format!("{}/events", f.wait_path);
        let tp = &f.w.tp;
        ensure!(
            signed_event(tp, &events, &secret, &body, &delivery, &timestamp, &[]).await?.0 == 200,
            "first event"
        );
        until("run done", SETTLE, || async {
            Ok(tp.send(&f.path, None).await?.1["data"]["status"] == "done")
        })
        .await?;
        let accepted = signed_event(tp, &events, &secret, &body, &delivery, &timestamp, &[]).await?;
        ensure!(accepted.0 == 200, "accepted: {accepted:?}");
        ensure!(accepted.1["data"]["delivery"] == delivery.as_str(), "delivery");
        let run = tp.data(&f.path).await?;
        ensure!(run["stepCount"] == 2, "steps: {run}");
        let last: Value = serde_json::from_str(run["last"].as_str().unwrap_or("null"))?;
        ensure!(
            last == json!({"event": "ready", "eventTime": 1234, "payload": {"message": "hello 🌏"}}),
            "last: {last}"
        );
        ensure!(
            signed_event(tp, &events, &secret, &body, &delivery, "1", &[]).await?.1 == accepted.1,
            "Accepted retry skips freshness only after verifying HMAC"
        );
        let zero = "0".repeat(64);
        ensure!(
            signed_event(tp, &events, &secret, &body, &delivery, "1",
                &[("X-Minarbot-Signature", zero.as_str())]).await?.0 == 401,
            "bad signature retry"
        );
        ensure!(
            signed_event(tp, &events, &secret, small, &delivery, &timestamp, &[]).await?.0 == 409,
            "Raw body whitespace changes delivery digest"
        );
        ensure!(
            signed_event(tp, &events, &secret, &body, &new_id(), &timestamp, &[]).await?.0 == 409,
            "second delivery"
        );
        let status = tp.data(&f.wait_path).await?;
        ensure!(status["status"] == "received", "wait: {status}");
        ensure!(status["receipt"] == accepted.1["receipt"], "receipt");
        Ok::<_, minarbot_testkit::Error>((secret, delivery, timestamp, body, accepted))
    }
    .await;
    let (secret, delivery, timestamp, body, accepted) = match result {
        Ok(values) => values,
        Err(error) => {
            f.connection.stop().await?;
            f.w.tp.close().await?;
            return Err(error);
        }
    };
    let result = async {
        let events = format!("{}/events", f.wait_path);
        let generation = f.connection.state()["generation"].as_i64().unwrap_or(0);
        f.w.tp.restart().await?;
        until("reconnected", SETTLE, || async {
            Ok(f.connection.phase() == "connected"
                && f.connection.state()["generation"].as_i64().unwrap_or(0) > generation)
        })
        .await?;
        let tp = &f.w.tp;
        ensure!(
            signed_event(tp, &events, &secret, &body, &delivery, &timestamp, &[]).await?.1 == accepted.1,
            "restart receipt"
        );
        ensure!(tp.data(&f.path).await?["stepCount"] == 2, "steps after restart");
        let db = tp.inspect()?;
        for table in ["wait_provisions", "workflow_waits", "workflow_runs", "workflow_steps", "receipts", "sync_events"] {
            ensure!(
                !json!(db.rows(&format!("SELECT * FROM {table}"), [])?).to_string().contains(&secret),
                "secret in {table}"
            );
        }
        let next = new_id();
        let next_wait = format!("/api/workflow-waits/{next}/1");
        let (status, _) = tp
            .send(
                &format!("{}/run", f.w.detail),
                Some(json!({"requestId": next, "id": next, "revision": 1, "input": ""})),
            )
            .await?;
        ensure!(status == 201, "second run: {status}");
        until("second wait", SETTLE, || async {
            Ok(tp.send(&next_wait, None).await?.1["data"]["status"] == "waiting")
        })
        .await?;
        let next_secret = f.w.secret(Some(&secret))?;
        let next_events = format!("{next_wait}/events");
        let fresh = |text: String, time: String, extra: Vec<(&'static str, String)>| {
            let (tp, path, secret) = (tp, next_events.clone(), next_secret.clone());
            async move {
                let extra: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
                signed_event(tp, &path, &secret, &text, &new_id(), &time, &extra).await
            }
        };
        ensure!(fresh(format!("{body} "), timestamp.clone(), vec![]).await?.0 == 413, "oversize");
        ensure!(
            fresh(body.clone(), timestamp.clone(), vec![("Origin", tp.origin())]).await?.0 == 403,
            "browser origin"
        );
        ensure!(fresh(body.clone(), "1".into(), vec![]).await?.0 == 401, "stale");
        for text in ["{", r#"{"event":"ready","payload":1e999}"#, r#"{"event":"ready","extra":true}"#] {
            ensure!(fresh(text.into(), timestamp.clone(), vec![]).await?.0 == 400, "{text}");
        }
        ensure!(
            fresh(r#"{"event":"wrong"}"#.into(), timestamp.clone(), vec![]).await?.0 == 409,
            "wrong event"
        );
        f.connection.stop().await?;
        ensure!(fresh(body.clone(), timestamp.clone(), vec![]).await?.0 == 503, "offline");
        let mut connection = f.w.connect()?;
        connected(&connection).await?;
        tp.write()?.execute_batch(
            "CREATE TRIGGER fail_workflow_event_receipt BEFORE INSERT ON receipts WHEN NEW.scope LIKE 'workflow-wait-event:%' BEGIN SELECT RAISE(ABORT,'fixture event failure'); END",
        )?;
        ensure!(fresh(body.clone(), timestamp.clone(), vec![]).await?.0 == 500, "receipt failure");
        ensure!(tp.data(&next_wait).await?["status"] == "waiting", "wait advanced");
        ensure!(
            tp.data(&format!("/api/projects/{}/workflow-runs/{next}", f.w.project)).await?["steps"][0]
                ["status"]
                == "waiting",
            "step advanced"
        );
        tp.write()?.execute_batch("DROP TRIGGER fail_workflow_event_receipt")?;
        let owner = tp.data("/api/session").await?["accountId"].as_str().unwrap_or_default().to_owned();
        let snapshot = workspace_snapshot(tp, Some(&owner))?;
        let backup = std::fs::read(&snapshot)?;
        for value in [&secret, &next_secret] {
            ensure!(
                !backup.windows(value.len()).any(|window| window == value.as_bytes()),
                "secret in backup"
            );
        }
        connection.stop().await?;
        Ok::<_, minarbot_testkit::Error>((snapshot, next_wait))
    }
    .await;
    let (snapshot, next_wait) = match result {
        Ok(values) => values,
        Err(error) => {
            f.connection.stop().await?;
            f.w.tp.close().await?;
            return Err(error);
        }
    };
    let result = async {
        f.w.tp.restore_from(&snapshot).await?;
        let tp = &f.w.tp;
        let events = format!("{}/events", f.wait_path);
        ensure!(
            signed_event(tp, &events, &secret, &body, &delivery, &timestamp, &[])
                .await?
                .0
                == 404,
            "Restore closes delivery admission"
        );
        ensure!(
            tp.data(&f.wait_path).await?["receipt"] == accepted.1["receipt"],
            "restored receipt"
        );
        ensure!(
            tp.data(&next_wait).await?["status"] == "closed",
            "next wait closed"
        );
        Ok(())
    }
    .await;
    f.w.tp.close().await?;
    result
}

async fn protected_workflow_wait_discards_delayed(
    kind: &'static str,
    change: &'static str,
) -> Result<()> {
    let mut f = workflow_wait("user_collaborator", if change == "expiry" { 2 } else { 60 }).await?;
    let gate = Gate::new();
    let held = Arc::new(AtomicBool::new(false));
    let WorkflowWait {
        w,
        connection,
        initiator,
        run,
        path,
        wait_path,
    } = &mut f;
    let result = async {
        let browser = Browser::new()?;
        let reveal = json!({"requestId": new_id(), "ephemeralPublicKey": browser.public,
            "expectedSecretGeneration": 1});
        let secret = w.secret(None)?;
        let delivery = new_id();
        let body = r#"{"event":"ready","payload":"once"}"#;
        let time = now_seconds();
        let tp = &w.tp;
        let fire = || {
            let (reveal, secret, delivery, time) =
                (reveal.clone(), secret.clone(), delivery.clone(), time.clone());
            let wait_path = wait_path.clone();
            async move {
                if kind == "reveal" {
                    let mut body = reveal;
                    body["requestId"] = json!(new_id());
                    return tp.send(&format!("{wait_path}/connection"), Some(body)).await;
                }
                signed_event(tp, &format!("{wait_path}/events"), &secret, body, &delivery, &time, &[])
                    .await
            }
        };
        let suffix = if kind == "reveal" {
            "/wait-reveal-results"
        } else {
            "/wait-event-results"
        };
        let (hold, flag) = (gate.clone(), held.clone());
        tp.fault(Some(Arc::new(move |exchange| {
            if exchange.path.ends_with(suffix) {
                flag.store(true, Ordering::SeqCst);
                let hold = hold.clone();
                return Fault::Hold(Box::pin(async move { hold.wait().await }));
            }
            Fault::Pass
        })));
        let control = async {
            until("held", SETTLE, || async { Ok(held.load(Ordering::SeqCst)) }).await?;
            ensure!(fire().await?.0 == 429, "One pending request per home");
            match change {
                "revision" => {
                    tp.write()?
                        .execute("UPDATE workflow_runs SET revision=revision+1 WHERE id=?", [&*run])?;
                }
                "rules" => {
                    let (status, _) = tp
                        .send_as(
                            &format!("/api/projects/{}/agents/{}/rules", w.project, w.agent),
                            Some(json!({"requestId": new_id(), "expectedParticipationRevision": 1,
                                "rules": "Changed rules"})),
                            "user_owner",
                            Some(Method::PUT),
                        )
                        .await?;
                    ensure!(status == 200, "rules: {status}");
                }
                "assignment" => {
                    let (status, _) = tp
                        .send_as(
                            &format!("/api/projects/{}/members/{initiator}", w.project),
                            Some(json!({"requestId": new_id(), "expectedRevision": 1,
                                "agentAssignments": [{"agentId": w.agent, "expectedAgentRevision": 1,
                                    "expectedParticipationRevision": 1, "expectedAssignmentRevision": 1,
                                    "enabled": false}]})),
                            "user_owner",
                            Some(Method::PATCH),
                        )
                        .await?;
                    ensure!(status == 200, "assignment: {status}");
                }
                "mapping" => {
                    tp.write()?.execute(
                        "DELETE FROM record_scopes WHERE table_name='workflow_waits' AND key_json=?",
                        [minarbot_testkit::protocol::canonical_json(&json!([run, 1]))],
                    )?;
                }
                "cancel" => {
                    let current = tp.data(path).await?;
                    let id = new_id();
                    let (status, _) = tp
                        .send(
                            &format!("{path}/cancel"),
                            Some(json!({"requestId": id, "id": id, "revision": current["revision"]})),
                        )
                        .await?;
                    ensure!(status == 200, "cancel: {status}");
                }
                "expiry" => {
                    tp.write()?.execute(
                        "UPDATE workflow_waits SET deadline=? WHERE run=?",
                        minarbot_testkit::hosted::params![wall_ms(), &*run],
                    )?;
                }
                "revoke" => {
                    let device = tp.data("/api/devices").await?[0].clone();
                    let (status, _) = tp
                        .send(
                            &format!("/api/devices/{}/revoke", device["id"].as_str().unwrap_or_default()),
                            Some(json!({"requestId": new_id(), "expectedRevision": device["revision"]})),
                        )
                        .await?;
                    ensure!(status == 200, "revoke: {status}");
                }
                _ => {}
            }
            if change == "disconnect" {
                connection.stop().await?;
                until("home offline", DEVICE_RECONNECT, || async {
                    Ok(tp.data("/api/devices").await?[0]["status"] != "ready")
                })
                .await?;
            }
            tp.fault(None);
            gate.open();
            Ok::<(), minarbot_testkit::Error>(())
        };
        let (denied, control) = tokio::join!(fire(), control);
        gate.open();
        tp.fault(None);
        control?;
        let denied = denied?;
        ensure!([403, 404, 409, 503].contains(&denied.0), "{kind}/{change}: {denied:?}");
        let db = tp.inspect()?;
        ensure!(
            db.row("SELECT delivery FROM workflow_waits WHERE run=?", [&*run])?["delivery"].is_null(),
            "delayed delivery stored"
        );
        ensure!(
            db.scalar::<i64>("SELECT count(*) FROM receipts WHERE scope LIKE 'workflow-wait-event:%'", [])?
                == 0,
            "delayed receipt stored"
        );
        if change == "rules" || change == "revision" {
            ensure!(fire().await?.0 == 200, "Fresh request uses changed authority");
        }
        if change == "expiry" {
            until("run failed", SETTLE, || async {
                Ok(tp.data(path).await?["status"] == "failed")
            })
            .await?;
            ensure!(tp.data(wait_path).await?["status"] == "expired", "wait not expired");
        }
        let _ = &held;
        Ok::<(), minarbot_testkit::Error>(())
    }
    .await;
    gate.open();
    w.tp.fault(None);
    connection.stop().await?;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_reveal_revision(
) -> Result<()> {
    protected_workflow_wait_discards_delayed("reveal", "revision").await
}

#[tokio::test]
async fn protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_reveal_rules(
) -> Result<()> {
    protected_workflow_wait_discards_delayed("reveal", "rules").await
}

#[tokio::test]
async fn protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_reveal_assignment(
) -> Result<()> {
    protected_workflow_wait_discards_delayed("reveal", "assignment").await
}

#[tokio::test]
async fn protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_reveal_mapping(
) -> Result<()> {
    protected_workflow_wait_discards_delayed("reveal", "mapping").await
}

#[tokio::test]
async fn protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_reveal_cancel(
) -> Result<()> {
    protected_workflow_wait_discards_delayed("reveal", "cancel").await
}

#[tokio::test]
async fn protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_reveal_expiry(
) -> Result<()> {
    protected_workflow_wait_discards_delayed("reveal", "expiry").await
}

#[tokio::test]
async fn protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_reveal_revoke(
) -> Result<()> {
    protected_workflow_wait_discards_delayed("reveal", "revoke").await
}

#[tokio::test]
async fn protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_reveal_disconnect(
) -> Result<()> {
    protected_workflow_wait_discards_delayed("reveal", "disconnect").await
}

#[tokio::test]
async fn protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_event_revision(
) -> Result<()> {
    protected_workflow_wait_discards_delayed("event", "revision").await
}

#[tokio::test]
async fn protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_event_rules(
) -> Result<()> {
    protected_workflow_wait_discards_delayed("event", "rules").await
}

#[tokio::test]
async fn protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_event_assignment(
) -> Result<()> {
    protected_workflow_wait_discards_delayed("event", "assignment").await
}

#[tokio::test]
async fn protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_event_mapping(
) -> Result<()> {
    protected_workflow_wait_discards_delayed("event", "mapping").await
}

#[tokio::test]
async fn protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_event_cancel(
) -> Result<()> {
    protected_workflow_wait_discards_delayed("event", "cancel").await
}

#[tokio::test]
async fn protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_event_expiry(
) -> Result<()> {
    protected_workflow_wait_discards_delayed("event", "expiry").await
}

#[tokio::test]
async fn protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_event_revoke(
) -> Result<()> {
    protected_workflow_wait_discards_delayed("event", "revoke").await
}

#[tokio::test]
async fn protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_event_disconnect(
) -> Result<()> {
    protected_workflow_wait_discards_delayed("event", "disconnect").await
}
