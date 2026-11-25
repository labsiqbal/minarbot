//! Journal-backed host execution, SIGKILL recovery, offline exports,
//! request capacity, transfers and owner/disaster recovery.

use super::{
    execution::{act_on, loop_task, Binding},
    fixture::{
        agent_body, digest, direct, fresh_revisions, locked, task_body, until, workspace_snapshot,
        Fault, Tp, DEVICE_RECONNECT, SETTLE,
    },
    routines::text,
};
use minarbot_testkit::{
    device_host::{
        crash_task, enroll, export_recovery, is_code, provider, run_task_process,
        ConnectionProcess, DeviceHost, HostOptions,
    },
    prelude::*,
};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

async fn connected_home(
    tp: &Tp,
    name: &str,
) -> Result<(Arc<DeviceHost>, minarbot_testkit::device_host::Connection)> {
    let runtime = tp.device(name)?;
    runtime.enroll(&tp.jwt("user_owner")?, "new").await?;
    let connection = runtime.connect().await?;
    until("home", SETTLE, || async {
        Ok(connection.phase() == "connected")
    })
    .await?;
    Ok((runtime, connection))
}

/// Project and agent on the first device.
async fn project_agent(tp: &Tp, name: &str, instructions: &str) -> Result<(Value, String, String)> {
    let home = tp.data("/api/devices").await?[0].clone();
    let (project, agent) = (new_id(), new_id());
    tp.project(&project, name).await?;
    ensure!(
        tp.send(
            "/api/agents",
            Some(agent_body(&agent, "Agent", instructions, &project, &home))
        )
        .await?
        .0 == 201,
        "agent"
    );
    Ok((home, project, agent))
}

async fn admit(
    tp: &Tp,
    home: &Value,
    project: &str,
    agent: &str,
    text_body: &str,
) -> Result<String> {
    let task = new_id();
    let (status, body) = tp
        .send(
            "/api/tasks",
            Some(task_body(
                &task,
                project,
                agent,
                &text(&home["id"]),
                &direct(text_body),
            )),
        )
        .await?;
    ensure!(status == 202, "task: {status} {body}");
    Ok(task)
}

fn counted(
    calls: &Arc<AtomicUsize>,
    answer: &'static str,
) -> impl Fn(Value) -> std::future::Ready<Result<Value>> + Send + Sync + 'static {
    let calls = calls.clone();
    move |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        std::future::ready(Ok(json!({"text": answer})))
    }
}

/// Lose every request whose path ends with `suffix` before it reaches the
/// server (the Node `fetch` patches that threw).
fn drop_requests(tp: &Tp, suffix: &'static str) {
    tp.fault(Some(Arc::new(move |exchange| {
        if exchange.path.ends_with(suffix) {
            Fault::Drop
        } else {
            Fault::Pass
        }
    })));
}

#[tokio::test]
async fn journal_backed_host_dispatches_once_retries_publication_only_and_refuses_lost_or_late_commit_grants(
) -> Result<()> {
    let tp = Tp::start().await?;
    let result = async {
        let (runtime, _connection) = connected_home(&tp, "device.sqlite").await?;
        let (home, project, agent) = project_agent(&tp, "Executor", "Research").await?;
        let calls = Arc::new(AtomicUsize::new(0));
        let action = |lose_acquire: bool| {
            let (tp, runtime, home, project, agent) = (
                &tp,
                runtime.clone(),
                home.clone(),
                project.clone(),
                agent.clone(),
            );
            async move {
                let task = admit(tp, &home, &project, &agent, "Run once").await?;
                if lose_acquire {
                    tp.lose_next("/acquire");
                    ensure!(
                        runtime.start_task(&task).await.is_err(),
                        "lost acquire accepted"
                    );
                    tp.fault(None);
                }
                let acquired = runtime.start_task(&task).await?;
                let context = runtime
                    .context(&task, &acquired["ownership"], &acquired["revision"])
                    .await?;
                Ok::<Value, minarbot_testkit::Error>(
                    json!({"taskId": task, "actionId": new_id(), "sequence": 0,
                    "actionKind": "read", "ownership": acquired["ownership"],
                    "payload": {"operation": "synthetic"}, "context": context, "grantRevision": 1}),
                )
            }
        };
        let first = action(true).await?;
        tp.lose_next("/reconcile");
        ensure!(
            act_on(&runtime, &first, counted(&calls, "Durable result"))
                .await
                .is_err(),
            "lost result ACK accepted"
        );
        ensure!(calls.load(Ordering::SeqCst) == 1, "calls after lost ACK");
        tp.fault(None);
        for attempt in 0..2 {
            ensure!(
                act_on(&runtime, &first, counted(&calls, "Durable result")).await?["state"]
                    == "acknowledged",
                "republish {attempt}"
            );
        }
        ensure!(calls.load(Ordering::SeqCst) == 1, "calls after republish");
        let device = minarbot_testkit::db::Inspector::open(&runtime.options.database)?;
        let action_id = text(&first["actionId"]);
        ensure!(
            device.scalar::<String>(
                "SELECT state FROM device_outbox WHERE action_id=?",
                [&action_id]
            )? == "acknowledged",
            "outbox"
        );
        ensure!(
            device.scalar::<i64>(
                "SELECT send_marker FROM device_journal WHERE action_id=?",
                [&action_id]
            )? == 1,
            "send marker"
        );
        for mode in ["lost", "late"] {
            let input = action(false).await?;
            if mode == "lost" {
                tp.lose_next("/commit");
            } else {
                tp.fault(Some(Arc::new(|exchange| {
                    if exchange.path.ends_with("/commit") {
                        Fault::After(Box::pin(tokio::time::sleep(Duration::from_millis(2100))))
                    } else {
                        Fault::Pass
                    }
                })));
            }
            ensure!(
                act_on(&runtime, &input, counted(&calls, "Durable result"))
                    .await
                    .is_err(),
                "{mode} commit accepted"
            );
            tp.fault(None);
            let retry = act_on(&runtime, &input, counted(&calls, "Durable result")).await;
            ensure!(
                retry
                    .as_ref()
                    .is_err_and(|error| is_code(error, "recovery_required")),
                "{mode} retry: {retry:?}"
            );
            ensure!(calls.load(Ordering::SeqCst) == 1, "{mode} calls");
        }
        let failed = action(false).await?;
        let counter = calls.clone();
        let unavailable = act_on(&runtime, &failed, move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Err("Provider response unavailable".into()))
        })
        .await;
        ensure!(
            unavailable
                .as_ref()
                .is_err_and(|error| error.to_string().contains("Provider response unavailable")),
            "unavailable: {unavailable:?}"
        );
        let (task, failed_action) = (text(&failed["taskId"]), text(&failed["actionId"]));
        ensure!(
            runtime
                .call(
                    "recover-action",
                    json!({"taskId": task, "actionId": failed_action})
                )
                .await?["state"]
                == "uncertain",
            "recover action"
        );
        let detail = tp.task_detail(&task).await?;
        let decision = new_id();
        let (status, _) = tp
            .send(
                &format!("/api/tasks/{task}/control"),
                Some(
                    json!({"requestId": decision, "expectedRevision": detail["revision"],
                    "operation": "resolve_uncertain", "decision": {"kind": "retry_with_risk",
                        "actionId": failed_action, "acknowledgement": "effect_may_have_happened",
                        "reason": "Synthetic test with explicit owner decision"}}),
                ),
            )
            .await?;
        ensure!(status == 200, "decision: {status}");
        let resolved = tp.task_detail(&task).await?;
        let context = runtime
            .context(&task, &failed["ownership"], &resolved["revision"])
            .await?;
        let mut retry = failed.clone();
        retry["actionId"] = json!(decision);
        retry["sequence"] = json!(1);
        retry["resolutionRequestId"] = json!(decision);
        retry["context"] = context;
        ensure!(
            act_on(&runtime, &retry, counted(&calls, "Durable result")).await?["state"]
                == "acknowledged",
            "retry"
        );
        ensure!(calls.load(Ordering::SeqCst) == 3, "calls after retry");
        Ok(())
    }
    .await;
    tp.fault(None);
    tp.close().await?;
    result
}

#[tokio::test]
async fn sigkill_recovery_proves_no_send_before_marker_and_never_repeats_marked_effects_or_lost_result_acks(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let options = HostOptions::new(&tp.dir().join("device.sqlite"), &tp.origin())?;
        let calls: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
        let counter = calls.clone();
        let relay = provider(move |context| {
            *locked(&counter).entry(text(&context["snapshot"]["taskId"])).or_default() += 1;
            std::future::ready(Ok(json!({"text": "Saved effect"})))
        })
        .await?;
        let origin = relay.origin();
        enroll(&options, &tp.jwt("user_owner")?, "new").await?;
        let device = options.device_id()?;
        for (checkpoint, expected, marked) in [
            ("provider-not-sent", "not_sent", false),
            ("provider-prepare-reply", "not_sent", false),
            ("provider-commit-reply", "not_sent", false),
            ("provider-in-flight", "uncertain", true),
            ("provider-saved", "acknowledged", true),
            ("provider-report-reply", "acknowledged", true),
        ] {
            let task = loop_task(&tp, &device, false, Binding::New).await?.task;
            crash_task(&options, &task, &origin, checkpoint).await?;
            let db = minarbot_testkit::db::Inspector::open(&options.database)?;
            let action = db.row("SELECT action_id, send_marker FROM device_journal WHERE task_id=? AND sequence=0", [&task])?;
            ensure!(action["send_marker"] == i64::from(marked), "{checkpoint} marker");
            let action_id = text(&action["action_id"]);
            for restart in 0..2 {
                until("home not ready", DEVICE_RECONNECT, || async {
                    Ok(tp.data("/api/devices").await?[0]["status"] != "ready")
                })
                .await?;
                tp.restart().await?;
                let mut recovery = ConnectionProcess::start(&options)?;
                let checked = async {
                    until("recovery connected", SETTLE, || async { Ok(recovery.phase() == "connected") }).await?;
                    until("outbox acknowledged", SETTLE, || async {
                        Ok(db.scalar::<i64>("SELECT count(*) FROM device_outbox WHERE action_id=? AND state='acknowledged'", [&action_id])? > 0)
                    })
                    .await?;
                    ensure!(
                        db.scalar::<String>("SELECT state FROM device_journal WHERE action_id=?", [&action_id])? == expected,
                        "{checkpoint} journal state"
                    );
                    Ok::<(), minarbot_testkit::Error>(())
                }
                .await;
                recovery.stop().await?;
                checked?;
                let seen = locked(&calls).get(&task).copied().unwrap_or(0);
                ensure!(seen == usize::from(marked || restart > 0), "{checkpoint} calls: {seen}");
                let detail = tp.task_detail(&task).await?;
                if expected == "acknowledged" {
                    ensure!(detail["results"][0]["content"] == json!({"text": "Saved effect"}), "{checkpoint} result");
                }
                if expected == "uncertain" {
                    ensure!(detail["state"] == "uncertain", "{checkpoint} state");
                }
                if !marked && restart == 0 {
                    ensure!(detail["state"] == "running", "{checkpoint} running");
                    ensure!(run_task_process(&options, &task, &origin).await?["state"] == "completed", "{checkpoint} continued");
                    let continued = db.row("SELECT action_id,state,result_json FROM device_journal WHERE task_id=? AND sequence=1", [&task])?;
                    ensure!(continued["action_id"] != action["action_id"], "{checkpoint} new action");
                    ensure!(continued["state"] == "acknowledged", "{checkpoint} continued state");
                    ensure!(
                        serde_json::from_str::<Value>(&text(&continued["result_json"]))? == json!({"text": "Saved effect"}),
                        "{checkpoint} continued result"
                    );
                    ensure!(locked(&calls).get(&task).copied() == Some(1), "{checkpoint} continued calls");
                }
            }
        }
        drop(relay);
        Ok(())
    }
    .await;
    tp.close().await?;
    result
}

#[tokio::test]
async fn rust_exports_prepared_in_flight_and_reported_device_journal_evidence_offline() -> Result<()>
{
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../minarbot-testkit/fixtures/device-journal-v3");
    let synthetic: Value = serde_json::from_slice(&std::fs::read(source.join("synthetic.json"))?)?;
    let root = TestRoot::new()?;
    let database = root.path().join("home.sqlite");
    let db = minarbot_testkit::db::Fixture::open(&database)?;
    // The fixture is current-format device data, which carries the data marker.
    db.execute_batch("PRAGMA application_id=0x4d4e5231")?;
    db.execute_batch(&std::fs::read_to_string(source.join("device.sql"))?)?;
    drop(db);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600))?;
    let metadata = root.path().join("home.sqlite.host");
    std::fs::create_dir(&metadata)?;
    std::fs::set_permissions(&metadata, std::fs::Permissions::from_mode(0o700))?;
    std::fs::write(
        metadata.join("identity.json"),
        synthetic["identity"].to_string(),
    )?;
    std::fs::set_permissions(
        metadata.join("identity.json"),
        std::fs::Permissions::from_mode(0o600),
    )?;
    let mut options = HostOptions::new(
        &database,
        synthetic["identity"]["server"].as_str().unwrap_or_default(),
    )?;
    options.keyring = root.path().join("keys.json");
    std::fs::write(&options.keyring, synthetic["keyring"].to_string())?;
    std::fs::set_permissions(&options.keyring, std::fs::Permissions::from_mode(0o600))?;
    let generation = new_id();
    for sample in synthetic["samples"].as_array().into_iter().flatten() {
        let (task, action, state) = (
            text(&sample["taskId"]),
            text(&sample["actionId"]),
            text(&sample["recoveryState"]),
        );
        let with_content = state == "reported";
        let input = json!({"taskId": task, "actionIds": [action], "requestId": new_id(),
            "recoveryGeneration": generation, "withContent": with_content});
        let evidence = export_recovery(&options, &input).await?;
        let entries = evidence["envelope"]["entries"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        ensure!(entries.len() == 1, "{state} entries");
        let entry = &entries[0];
        ensure!(entry["actionId"] == action.as_str(), "{state} action");
        ensure!(entry["taskId"] == task.as_str(), "{state} task");
        ensure!(entry["state"] == state.as_str(), "{state} state");
        let effect = match state.as_str() {
            "not_sent" => "none",
            "uncertain" => "unknown",
            _ => "known",
        };
        ensure!(entry["effectStatus"] == effect, "{state} effect");
        ensure!(
            evidence.get("resultContent").cloned()
                == with_content.then(|| json!({"text": "Saved effect"})),
            "{state} content: {evidence}"
        );
        ensure!(
            export_recovery(&options, &input).await? == evidence,
            "{state} export stable"
        );
    }
    Ok(())
}

#[tokio::test]
async fn stalled_normal_requests_cannot_starve_control_reads_and_disconnected_requests_release_capacity(
) -> Result<()> {
    let tp = Tp::start().await?;
    let holding = Arc::new(AtomicBool::new(true));
    let answered: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut held = Vec::new();
    let result = async {
        let port = tp.app.port;
        let (origin, token) = (tp.origin(), tp.jwt("user_owner")?);
        // Probes can take a slot before a held request arrives; replace any
        // held request the server answers.
        for _ in 0..64 {
            let (holding, answered, origin, token) = (holding.clone(), answered.clone(), origin.clone(), token.clone());
            held.push(tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                while holding.load(Ordering::SeqCst) {
                    let Ok(mut stream) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await else { break };
                    let head = format!(
                        "POST /api/projects HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nOrigin: {origin}\r\nContent-Type: application/json\r\nContent-Length: 8\r\nX-Minarbot-Protocol: 1\r\nX-Minarbot-Request-Id: {}\r\n\r\n{{",
                        new_id()
                    );
                    if stream.write_all(head.as_bytes()).await.is_err() {
                        continue;
                    }
                    let mut reply = [0_u8; 64];
                    match stream.read(&mut reply).await {
                        Ok(read) if read > 0 => locked(&answered).push(String::from_utf8_lossy(&reply[..read.min(12)]).into_owned()),
                        _ => {}
                    }
                }
            }));
        }
        let busy = until("projects busy", SETTLE, || async { Ok(tp.send("/api/projects", None).await?.0 == 429) }).await;
        if let Err(error) = busy {
            return Err(format!("{error}; held requests answered: {:?}", locked(&answered)).into());
        }
        let (status, control) = tp.send("/api/session", None).await?;
        ensure!(status == 200, "session: {status} {control}");
        for path in [format!("/api/tasks/{}", new_id()), format!("/api/tasks/{}/results/{}", new_id(), new_id())] {
            let (status, receipt) = tp.send(&path, None).await?;
            ensure!(status == 404, "{path}: {status} {receipt}");
        }
        let (status, logout) = tp.send_as("/api/session/logout", Some(json!({"requestId": new_id()})), "user_logout", None).await?;
        ensure!([200, 202, 503].contains(&status), "logout: {status} {logout}");
        let (status, task_control) = tp
            .send(
                &format!("/api/tasks/{}/control", new_id()),
                Some(json!({"requestId": new_id(), "expectedRevision": 1, "operation": "cancel"})),
            )
            .await?;
        ensure!(status == 404, "task control: {status} {task_control}");
        holding.store(false, Ordering::SeqCst);
        for task in held.drain(..) {
            task.abort();
        }
        until("projects free", SETTLE, || async { Ok(tp.send("/api/projects", None).await?.0 == 200) }).await?;
        Ok(())
    }
    .await;
    holding.store(false, Ordering::SeqCst);
    for task in held {
        task.abort();
    }
    tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_transfer_survives_lost_rpc_reply_and_canonical_restart_without_duplicate_successor_or_caller_effects(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let home = tp.device("source.sqlite")?;
        let target = tp.device("target.sqlite")?;
        home.enroll(&tp.jwt("user_owner")?, "new").await?;
        target.enroll(&tp.jwt("user_owner")?, "new").await?;
        let connection = home.connect().await?;
        until("home", SETTLE, || async { Ok(connection.phase() == "connected") }).await?;
        let devices = tp.data("/api/devices").await?;
        let (project, source, recipient) = (new_id(), new_id(), new_id());
        tp.project(&project, "Canonical transfer").await?;
        let (home_id, target_id) = (home.device_id()?, target.device_id()?);
        for (agent, device) in [(&source, &home_id), (&recipient, &target_id)] {
            let row = devices
                .as_array()
                .and_then(|rows| rows.iter().find(|row| row["id"] == device.as_str()).cloned())
                .ok_or("device row")?;
            ensure!(tp.send("/api/agents", Some(agent_body(agent, agent, "Continue", &project, &row))).await?.0 == 201, "agent");
        }
        let channel = new_id();
        let seed = tp.write()?;
        let owner: String = seed.scalar("SELECT account FROM projects WHERE id=?", [&project])?;
        seed.execute("INSERT INTO channels(id,name,instructions,lead) VALUES (?,?,?,?)", [&channel, "Handoff", "Continue project work", &source])?;
        for agent in [&source, &recipient] {
            seed.execute("INSERT INTO channel_members VALUES (?,?)", [&channel, agent])?;
        }
        seed.execute(
            "INSERT INTO record_scopes VALUES ('channels',?,?,?,'project','channel',?)",
            [&json!([channel]).to_string(), &owner, &project, &channel],
        )?;
        drop(seed);
        let task = new_id();
        let payload = json!({"v": 1, "kind": "channel", "channelId": channel, "routing": "assigned",
            "message": {"text": "Original request", "files": []}});
        let mut revisions = fresh_revisions();
        revisions["channel"] = json!(1);
        let (status, _) = tp
            .send(
                "/api/tasks",
                Some(json!({"requestId": task, "projectId": project, "agentId": source, "homeDeviceId": home_id,
                    "payload": payload, "expectedRevisions": revisions,
                    "payloadDigest": digest(&json!(["minarbot-task-payload-v1", task, project, source, home_id, payload, revisions]))})),
            )
            .await?;
        ensure!(status == 202, "task: {status}");
        let owned = home.start_task(&task).await?;
        let context = home.context(&task, &owned["ownership"], &owned["revision"]).await?;
        let action = new_id();
        let schema = context["snapshot"]["tools"]
            .as_array()
            .and_then(|tools| tools.iter().find(|tool| tool["function"]["name"] == "transfer_task").cloned())
            .ok_or("transfer_task tool")?;
        ensure!(schema["function"]["parameters"]["properties"]["recipient"]["enum"] == json!([recipient]), "recipient enum: {schema}");
        let tool_call = json!({"id": "transfer", "function": {"name": "transfer_task",
            "arguments": json!({"recipient": recipient, "task": "Continue from saved evidence"}).to_string()}});
        let input = json!({"taskId": task, "actionId": action, "sequence": 0, "actionKind": "canonical",
            "ownership": owned["ownership"],
            "payload": {"toolCall": tool_call, "expectedRevision": owned["revision"].as_i64().unwrap_or(0) + 1},
            "context": context, "grantRevision": 1});
        let seen = tp.lose_next("/context-mutations");
        ensure!(home.action(&input, |_| async { Err("Must not execute".into()) }).await.is_err(), "lost ACK accepted");
        tp.fault(None);
        ensure!(*locked(&seen) == Some(200), "context mutation status: {:?}", locked(&seen));
        let transferred = tp.task_detail(&task).await?;
        let successor = text(&transferred["currentTaskId"]);
        ensure!(transferred["state"] == "transferred", "state");
        ensure!(successor != task, "successor id");
        tp.restart().await?;
        until("home reconnected", SETTLE, || async { Ok(connection.phase() == "connected") }).await?;
        until("action acknowledged", Duration::from_secs(15), || async {
            Ok(home.recovery_status().as_array().is_some_and(|rows| {
                rows.iter().any(|row| row["actionId"] == action.as_str() && row["state"] == "acknowledged")
            }))
        })
        .await?;
        ensure!(tp.task_detail(&task).await?["currentTaskId"] == successor.as_str(), "successor kept");
        let next = tp.task_detail(&successor).await?;
        ensure!(next["homeDeviceId"] == target_id.as_str(), "successor home");
        ensure!(next["state"] == "waiting_device", "successor state");
        let db = tp.inspect()?;
        ensure!(db.scalar::<i64>("SELECT count(*) FROM tasks WHERE logical_turn=?", [&task])? == 2, "tasks");
        ensure!(db.scalar::<i64>("SELECT count(*) FROM task_results WHERE action=?", [&action])? == 1, "results");
        Ok(())
    }
    .await;
    tp.fault(None);
    tp.close().await?;
    result
}

/// `POST /api/tasks/{task}/recovery-{operation}` with a recovery proof.
async fn recovery_send(
    tp: &Tp,
    task: &str,
    operation: &str,
    body: &Value,
    session: &str,
    origin: &str,
    subject: &str,
) -> Result<(u16, Value)> {
    let response = tp
        .app
        .server
        .client()?
        .post(format!(
            "{}/api/tasks/{task}/recovery-{operation}",
            tp.origin()
        ))
        .header("Authorization", format!("Bearer {}", tp.jwt(subject)?))
        .header("Origin", origin)
        .header("Content-Type", "application/json")
        .header("X-Minarbot-Protocol", "1")
        .header("X-Minarbot-Request-Id", text(&body["requestId"]))
        .header("X-Minarbot-Recovery", session)
        .header("Connection", "close")
        .body(body.to_string())
        .send()
        .await?;
    let status = response.status().as_u16();
    Ok((status, response.json().await.unwrap_or(Value::Null)))
}

fn export_input(task: &str, action: &str, generation: &Value, with_content: bool) -> Value {
    json!({"taskId": task, "actionIds": [action], "requestId": new_id(), "recoveryGeneration": generation,
        "withContent": with_content})
}

/// One `read` action whose result report never reaches the server.
async fn unreported_read(
    tp: &Tp,
    runtime: &DeviceHost,
    task: &str,
    operation: &str,
    answer: &'static str,
    calls: &Arc<AtomicUsize>,
) -> Result<(Value, String)> {
    let owned = runtime.start_task(task).await?;
    let context = runtime
        .context(task, &owned["ownership"], &owned["revision"])
        .await?;
    let action = new_id();
    drop_requests(tp, "/reconcile");
    let reported = act_on(
        runtime,
        &json!({"taskId": task, "actionId": action, "sequence": 0, "actionKind": "read",
            "ownership": owned["ownership"], "payload": {"operation": operation}, "context": context,
            "grantRevision": 1}),
        counted(calls, answer),
    )
    .await;
    ensure!(reported.is_err(), "report reached the server: {reported:?}");
    Ok((owned, action))
}

#[tokio::test]
async fn owner_recovers_revoked_device_journal_through_http_without_device_session_or_repeated_effect(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let (runtime, connection) = connected_home(&tp, "recover-device.sqlite").await?;
        let options = runtime.options.clone();
        let (home, project, agent) = project_agent(&tp, "Recovery", "Recover").await?;
        let task = admit(&tp, &home, &project, &agent, "One effect").await?;
        let calls = Arc::new(AtomicUsize::new(0));
        let (owned, action) = unreported_read(&tp, &runtime, &task, "one effect", "Original output", &calls).await?;
        tp.fault(None);
        let device = text(&home["id"]);
        let current = tp.data("/api/devices").await?[0].clone();
        ensure!(
            tp.send(&format!("/api/devices/{device}/revoke"), Some(json!({"requestId": new_id(), "expectedRevision": current["revision"]})))
                .await?
                .0
                == 200,
            "revoke"
        );
        connection.stop().await?;
        runtime.close().await?;
        let start = json!({"requestId": new_id(), "expectedAuthorityGeneration": owned["ownership"]["authorityGeneration"],
            "sourceDeviceId": device, "sourceDeviceGeneration": owned["ownership"]["deviceGeneration"]});
        let session_path = format!("/api/tasks/{task}/recovery-session");
        let (status, session) = tp.send(&session_path, Some(start.clone())).await?;
        ensure!(status == 201, "session: {status} {session}");
        ensure!(tp.send(&session_path, Some(start)).await?.1["data"].get("session").is_none(), "session replayed");
        let generation = session["data"]["recoveryGeneration"].clone();
        let proof = text(&session["data"]["session"]);
        let origin = tp.origin();
        let send = |operation: &'static str, body: Value, origin: String, subject: &'static str| {
            let (tp, task, proof) = (&tp, task.clone(), proof.clone());
            async move { recovery_send(tp, &task, operation, &body, &proof, &origin, subject).await }
        };
        let metadata = export_recovery(&options, &export_input(&task, &action, &generation, false)).await?;
        ensure!(send("import", metadata.clone(), "https://evil.example".into(), "user_owner").await?.0 == 403, "foreign origin");
        ensure!(send("import", metadata.clone(), origin.clone(), "user_other").await?.0 == 404, "other user");
        let (status, imported) = send("import", metadata.clone(), origin.clone(), "user_owner").await?;
        ensure!(status == 200, "import: {status} {imported}");
        ensure!(imported["data"]["state"] == "result_missing", "import state");
        tp.restart().await?;
        let send = |operation: &'static str, body: Value| {
            let (tp, task, proof, origin) = (&tp, task.clone(), proof.clone(), origin.clone());
            async move { recovery_send(tp, &task, operation, &body, &proof, &origin, "user_owner").await }
        };
        let result_export = export_recovery(&options, &export_input(&task, &action, &generation, true)).await?;
        ensure!(result_export["envelope"]["entries"] == metadata["envelope"]["entries"], "entries");
        let (status, published) = send("results", result_export.clone()).await?;
        ensure!(status == 200, "results: {status} {published}");
        ensure!(send("results", result_export).await?.0 == 200, "results replay");
        ensure!(tp.task_detail(&task).await?["results"][0]["content"] == json!({"text": "Original output"}), "result");
        ensure!(tp.data("/api/devices").await?[0]["status"] == "revoked", "device revoked");
        ensure!(calls.load(Ordering::SeqCst) == 1, "calls");
        let rotated = enroll(&options, &tp.jwt("user_owner")?, "rotate").await?;
        ensure!(
            rotated["generation"].as_i64().unwrap_or(0) > owned["ownership"]["deviceGeneration"].as_i64().unwrap_or(0),
            "rotated generation"
        );
        let historical = export_recovery(&options, &export_input(&task, &action, &generation, true)).await?;
        ensure!(historical["envelope"]["entries"] == metadata["envelope"]["entries"], "historical entries");
        ensure!(send("results", historical).await?.0 == 200, "historical results");
        let signing: String = minarbot_testkit::db::Inspector::open(&options.database)?.scalar(
            "SELECT signing_ref FROM device_enrollment_keys WHERE device_generation=?",
            [&owned["ownership"]["deviceGeneration"].as_i64().unwrap_or(0)],
        )?;
        let mut keys = options.keys()?;
        let name = keys.keys().find(|name| name.ends_with(&format!("/{signing}"))).cloned().ok_or("signing key")?;
        let private = keys.get(&name).cloned().ok_or("private key")?;
        let rotated_keys: Value = serde_json::from_slice(&std::fs::read(&options.keyring)?)?;
        ensure!(rotated_keys[&name] == private.as_str(), "keyring entry");
        keys.remove(&name);
        std::fs::write(&options.keyring, serde_json::to_vec(&keys)?)?;
        let unavailable = export_recovery(&options, &export_input(&task, &action, &generation, true)).await;
        ensure!(
            unavailable.as_ref().is_err_and(|error| is_code(error, "device_key_unavailable")),
            "export without key: {unavailable:?}"
        );
        let db = tp.inspect()?;
        ensure!(db.count("task_results")? == 1, "task results");
        ensure!(db.count("recovery_imports")? == 1, "recovery imports");
        ensure!(!json!(db.rows("SELECT * FROM receipts", [])?).to_string().contains(&proof), "proof in receipts");
        Ok(())
    }
    .await;
    tp.fault(None);
    tp.close().await?;
    result
}

#[tokio::test]
async fn disaster_recovery_imports_original_host_result_without_original_server_and_never_enables_dispatch(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let (runtime, connection) = connected_home(&tp, "home.sqlite").await?;
        let options = runtime.options.clone();
        let (home, project, agent) = project_agent(&tp, "Disaster recovery", "Recover").await?;
        let task = admit(&tp, &home, &project, &agent, "Original action").await?;
        let calls = Arc::new(AtomicUsize::new(0));
        let (owned, action) = unreported_read(
            &tp,
            &runtime,
            &task,
            "one call",
            "Saved on original host",
            &calls,
        )
        .await?;
        connection.stop().await?;
        let offline = runtime.start_task(&task).await;
        ensure!(
            offline
                .as_ref()
                .is_err_and(|error| is_code(error, "device_offline")),
            "offline start: {offline:?}"
        );
        runtime.close().await?;
        tp.fault(None);
        let backup = workspace_snapshot(&tp, None)?;
        let proof = tp.restore_from(&backup).await?;
        ensure!(
            proof["recoveryGeneration"] != owned["ownership"]["authorityGeneration"],
            "recovery generation"
        );
        let generation = proof["recoveryGeneration"].clone();
        let session = text(&proof["session"]);
        let origin = tp.origin();
        let bogus = "A".repeat(43);
        let metadata =
            export_recovery(&options, &export_input(&task, &action, &generation, false)).await?;
        ensure!(
            recovery_send(
                &tp,
                &task,
                "import",
                &metadata,
                &bogus,
                &origin,
                "user_owner"
            )
            .await?
            .0 == 403,
            "bogus session"
        );
        ensure!(
            recovery_send(
                &tp,
                &task,
                "import",
                &metadata,
                &session,
                &origin,
                "user_other"
            )
            .await?
            .0 == 403,
            "other user"
        );
        let (status, imported) = recovery_send(
            &tp,
            &task,
            "import",
            &metadata,
            &session,
            &origin,
            "user_owner",
        )
        .await?;
        ensure!(status == 200, "import: {status} {imported}");
        ensure!(
            imported["data"]["state"] == "result_missing",
            "import state"
        );
        tp.restart().await?;
        let content =
            export_recovery(&options, &export_input(&task, &action, &generation, true)).await?;
        let (status, published) = recovery_send(
            &tp,
            &task,
            "results",
            &content,
            &session,
            &origin,
            "user_owner",
        )
        .await?;
        ensure!(status == 200, "results: {status} {published}");
        ensure!(
            recovery_send(
                &tp,
                &task,
                "results",
                &content,
                &session,
                &origin,
                "user_owner"
            )
            .await?
            .0 == 200,
            "results replay"
        );
        ensure!(
            tp.send(&format!("/api/tasks/{task}"), None).await?.0 == 503,
            "read-only task read"
        );
        let restored = tp.write()?;
        ensure!(
            restored.scalar::<String>("SELECT serving_state FROM authority_state", [])?
                == "read_only_recovery",
            "serving state"
        );
        ensure!(
            serde_json::from_str::<Value>(&restored.scalar::<String>(
                "SELECT content_json FROM task_results WHERE action=?",
                [&action]
            )?)? == json!({"text": "Saved on original host"}),
            "imported result"
        );
        ensure!(
            restored.scalar::<i64>("SELECT count(*) FROM task_results", [])? == 1,
            "task results"
        );
        ensure!(
            restored.scalar::<i64>("SELECT stopped FROM execution_owners", [])? == 1,
            "owner stopped"
        );
        ensure!(
            restored.scalar::<String>("SELECT status FROM devices", [])? == "revoked",
            "device revoked"
        );
        restored.execute_batch("UPDATE authority_state SET recovery_expires_at=0")?;
        ensure!(
            recovery_send(
                &tp,
                &task,
                "results",
                &content,
                &session,
                &origin,
                "user_owner"
            )
            .await?
            .0 == 403,
            "expired session"
        );
        ensure!(calls.load(Ordering::SeqCst) == 1, "calls");
        Ok(())
    }
    .await;
    tp.fault(None);
    tp.close().await?;
    result
}

#[tokio::test]
async fn disaster_recovery_quarantines_signed_post_backup_actions_and_tasks_without_publishing_or_admitting_them(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let (runtime, connection) = connected_home(&tp, "home.sqlite").await?;
        let options = runtime.options.clone();
        let (home, project, agent) = project_agent(&tp, "Quarantine", "Recover").await?;
        let known = admit(&tp, &home, &project, &agent, "Post-backup action").await?;
        let backup = workspace_snapshot(&tp, None)?;
        let unknown = admit(&tp, &home, &project, &agent, "Post-backup action").await?;
        let calls = Arc::new(AtomicUsize::new(0));
        let mut actions = Vec::new();
        for task in [&known, &unknown] {
            let (_, action) = unreported_read(
                &tp,
                &runtime,
                task,
                "one call",
                "Keep result on original host",
                &calls,
            )
            .await?;
            actions.push((task.clone(), action));
        }
        connection.stop().await?;
        runtime.close().await?;
        tp.fault(None);
        let proof = tp.restore_from(&backup).await?;
        let (generation, session, origin) = (
            proof["recoveryGeneration"].clone(),
            text(&proof["session"]),
            tp.origin(),
        );
        for (task, action) in &actions {
            let metadata =
                export_recovery(&options, &export_input(task, action, &generation, false)).await?;
            ensure!(
                recovery_send(
                    &tp,
                    task,
                    "import",
                    &metadata,
                    &"A".repeat(43),
                    &origin,
                    "user_owner"
                )
                .await?
                .0 == 403,
                "bogus session"
            );
            ensure!(
                recovery_send(
                    &tp,
                    task,
                    "import",
                    &metadata,
                    &session,
                    &origin,
                    "user_other"
                )
                .await?
                .0 == 403,
                "other user"
            );
            let mut altered = metadata.clone();
            altered["signature"] = json!("A".repeat(86));
            ensure!(
                recovery_send(
                    &tp,
                    task,
                    "import",
                    &altered,
                    &session,
                    &origin,
                    "user_owner"
                )
                .await?
                .0 == 403,
                "altered signature"
            );
            let (status, imported) = recovery_send(
                &tp,
                task,
                "import",
                &metadata,
                &session,
                &origin,
                "user_owner",
            )
            .await?;
            ensure!(status == 200, "import: {status} {imported}");
            ensure!(
                imported["data"]["quarantinedActionIds"] == json!([action]),
                "quarantined"
            );
            ensure!(
                imported["data"]["importedActionIds"] == json!([]),
                "imported"
            );
            ensure!(imported["data"]["dispatchable"] == false, "dispatchable");
            ensure!(
                recovery_send(
                    &tp,
                    task,
                    "import",
                    &metadata,
                    &session,
                    &origin,
                    "user_owner"
                )
                .await?
                .1["receipt"]["requestId"]
                    == imported["receipt"]["requestId"],
                "import replay"
            );
        }
        tp.restart().await?;
        let restored = tp.inspect()?;
        ensure!(restored.count("tasks")? == 1, "tasks");
        ensure!(restored.count("task_actions")? == 0, "actions");
        ensure!(restored.count("task_results")? == 0, "results");
        ensure!(restored.count("recovery_imports")? == 2, "imports");
        let private = restored.row(
            "SELECT project,result_json FROM receipts WHERE scope=?",
            [format!("tasks:recovery-import:{unknown}")],
        )?;
        ensure!(private["project"].is_null(), "private receipt project");
        ensure!(
            serde_json::from_str::<Value>(&text(&private["result_json"]))?["receipt"]
                .get("projectId")
                .is_none(),
            "private receipt projectId"
        );
        ensure!(
            restored.scalar::<String>("SELECT serving_state FROM authority_state", [])?
                == "read_only_recovery",
            "serving state"
        );
        ensure!(calls.load(Ordering::SeqCst) == 2, "calls");
        ensure!(
            tp.send(&format!("/api/tasks/{unknown}"), None).await?.0 == 404,
            "unknown task"
        );
        Ok(())
    }
    .await;
    tp.fault(None);
    tp.close().await?;
    result
}
