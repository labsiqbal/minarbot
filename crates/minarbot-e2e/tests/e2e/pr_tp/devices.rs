//! Canonical listener lock, device enrollment, keys, reassignment and signed
//! host admission.

use super::{
    fixture::{agent_body, digest, direct, locked, task_body, until, Tp, SETTLE},
    routines::text,
};
use minarbot_testkit::{
    device_host::{enroll, is_code, start_enrollment, ConnectionProcess, HostOptions},
    http::Method,
    prelude::*,
    protocol::{canonical_json, sha256_hex},
    TestRoot,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

const BUSY: &str = "Another canonical server is still running.";

fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?
        .local_addr()?
        .port())
}

async fn ready(connection: &ConnectionProcess) -> Result<()> {
    until("connection ready", SETTLE, || async { Ok(connection.phase() == "connected") }).await
}

fn identity(options: &HostOptions) -> Result<Value> {
    let mut path = options.database.as_os_str().to_owned();
    path.push(".host/identity.json");
    Ok(serde_json::from_slice(&std::fs::read(std::path::PathBuf::from(path))?)?)
}

async fn home_row(tp: &Tp, device: &str) -> Result<Value> {
    tp.data("/api/devices")
        .await?
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == device).cloned())
        .ok_or_else(|| format!("device {device} missing").into())
}

#[tokio::test]
async fn canonical_listener_rejects_a_second_server_on_the_same_database_and_releases_lock_on_close(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let duplicate = tp.app.root.start_error(tp.app.config_on(free_port()?)).await?;
        // The Node launcher mapped this startup error to `canonical_server_busy`.
        ensure!(duplicate.contains(BUSY), "duplicate: {duplicate}");
        let alias = TestRoot::new()?;
        std::os::unix::fs::symlink(tp.database(), alias.dir("data")?.join("state.sqlite"))?;
        let aliased = alias.start_error(tp.app.config_on(free_port()?)).await?;
        ensure!(aliased.contains(BUSY), "alias: {aliased}");
        ensure!(tp.send("/api/session", None).await?.0 == 200, "session");
        let failed = TestRoot::new()?;
        let in_use = failed.start_error(tp.app.config_on(tp.app.port)).await?;
        ensure!(in_use.contains("Address already in use") || in_use.contains("EADDRINUSE"), "port in use: {in_use}");
        let mut second = failed.start(tp.app.config_on(free_port()?)).await?;
        second.shutdown().await?;
        tp.restart().await?;
        ensure!(tp.send("/api/session", None).await?.0 == 200, "session after restart");
        Ok(())
    }
    .await;
    tp.close().await?;
    result
}

#[tokio::test]
async fn cross_device_reassign_survives_lost_reply_and_restart_preserves_old_results_and_waits_for_fixed_recipient_home(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let (first, second) = (tp.device("a.sqlite")?, tp.device("b.sqlite")?);
        first.enroll(&tp.jwt("user_owner")?, "new").await?;
        second.enroll(&tp.jwt("user_owner")?, "new").await?;
        let connection = first.connect().await?;
        until("first home", SETTLE, || async { Ok(connection.phase() == "connected") }).await?;
        let (project, source, recipient) = (new_id(), new_id(), new_id());
        tp.project(&project, "Transfer").await?;
        let (first_id, second_id) = (first.device_id()?, second.device_id()?);
        for (agent, home) in [(&source, &first_id), (&recipient, &second_id)] {
            let row = home_row(&tp, home).await?;
            ensure!(
                tp.send("/api/agents", Some(agent_body(agent, agent, "Continue safely", &project, &row))).await?.0 == 201,
                "agent {agent}"
            );
        }
        let task = new_id();
        let command = task_body(&task, &project, &source, &first_id, &direct("Keep earlier evidence"));
        let (status, accepted) = tp.send("/api/tasks", Some(command.clone())).await?;
        ensure!(status == 202, "task: {status}");
        let owned = first.start_task(&task).await?;
        let context = first.context(&task, &owned["ownership"], &owned["revision"]).await?;
        let effects = Arc::new(AtomicUsize::new(0));
        let counter = effects.clone();
        first
            .action(
                &json!({"taskId": task, "actionId": new_id(), "sequence": 0, "actionKind": "read",
                    "ownership": owned["ownership"], "payload": {"operation": "original"},
                    "context": context, "grantRevision": 1}),
                move |_| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    async { Ok(json!({"text": "Saved source output"})) }
                },
            )
            .await?;
        let detail = tp.task_detail(&task).await?;
        let control = format!("/api/tasks/{task}/control");
        let (status, paused) = tp
            .send(&control, Some(json!({"requestId": new_id(), "expectedRevision": detail["revision"], "operation": "pause"})))
            .await?;
        ensure!(status == 200, "pause: {status}");
        let reassign = json!({"requestId": new_id(), "expectedRevision": paused["data"]["revision"],
            "operation": "reassign", "recipientAgentId": recipient});
        // The first reply is lost after the server transferred the task.
        let (status, _) = tp.send(&control, Some(reassign.clone())).await?;
        ensure!(status == 200, "lost transfer: {status}");
        tp.restart().await?;
        let (status, retry) = tp.send(&control, Some(reassign.clone())).await?;
        ensure!(status == 200, "retry: {status} {retry}");
        let next = text(&retry["data"]["taskId"]);
        ensure!(next != task, "successor id");
        ensure!(retry["data"]["homeDeviceId"] == second_id.as_str(), "successor home");
        let (_, original) = tp.send(&format!("/api/tasks/{task}"), None).await?;
        let (_, successor) = tp.send(&format!("/api/tasks/{next}"), None).await?;
        ensure!(original["data"]["currentTaskId"] == next.as_str(), "current task");
        ensure!(original["data"]["state"] == "transferred", "original state");
        ensure!(original["receipt"] == accepted["receipt"], "original receipt");
        ensure!(successor["data"]["state"] == "waiting_device", "successor state");
        ensure!(successor["data"]["agentId"] == recipient.as_str(), "successor agent");
        let mut again = reassign.clone();
        again["requestId"] = json!(new_id());
        ensure!(tp.send(&control, Some(again)).await?.1["error"]["code"] == "task_transferred", "second reassign");
        let next_connection = second.connect().await?;
        until("second home", SETTLE, || async { Ok(next_connection.phase() == "connected") }).await?;
        let next_owner = second.start_task(&next).await?;
        let continued = second.context(&next, &next_owner["ownership"], &next_owner["revision"]).await?;
        ensure!(
            continued["snapshot"]["toolJournal"][0]["result"]["text"] == "Saved source output",
            "continued journal: {}",
            continued["snapshot"]["toolJournal"]
        );
        first.recover().await?;
        ensure!(effects.load(Ordering::SeqCst) == 1, "effects");
        ensure!(tp.send("/api/tasks", Some(command)).await?.1["data"]["taskId"] == task.as_str(), "admission replay");
        Ok(())
    }
    .await;
    tp.close().await?;
    result
}

async fn local(
    origin: &str,
    token: &str,
    headers: &[(&str, &str)],
    path: &str,
    body: &Value,
) -> Result<(u16, Value)> {
    let mut fields = vec![
        ("Origin".to_owned(), origin.to_owned()),
        ("Content-Type".to_owned(), "application/json".to_owned()),
        ("Authorization".to_owned(), format!("Bearer {token}")),
    ];
    for (name, value) in headers {
        fields.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
        fields.push(((*name).to_owned(), (*value).to_owned()));
    }
    let mut request = minarbot_testkit::relay::HttpClient::new().post(format!("{origin}{path}"));
    for (name, value) in fields {
        request = request.header(name, value);
    }
    let response = request.body(body.to_string()).send().await?;
    let status = response.status().as_u16();
    Ok((status, response.json().await.unwrap_or(Value::Null)))
}

#[tokio::test]
async fn local_possession_gate_enrolls_only_configured_server_and_retains_identity_across_reconnect_revoke(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let options = HostOptions::new(&tp.dir().join("device.sqlite"), &tp.origin())?;
        let (listener, mut child) = start_enrollment(&options, "new").await?;
        let token_file = std::path::PathBuf::from(text(&listener["tokenFile"]));
        let origin = text(&listener["origin"]);
        let token = std::fs::read_to_string(&token_file)?;
        use std::os::unix::fs::PermissionsExt;
        ensure!(std::fs::metadata(&token_file)?.permissions().mode() & 0o777 == 0o600, "token mode");
        let proof = json!({"requestId": new_id(), "clerkToken": tp.jwt("user_owner")?});
        ensure!(local(&origin, &token, &[("Origin", "https://evil.example")], "/local/link-proof", &proof).await?.0 == 403, "foreign origin");
        ensure!(local(&origin, &token, &[("Authorization", "Bearer wrong")], "/local/link-proof", &proof).await?.0 == 403, "wrong token");
        ensure!(local(&origin, &token, &[("X-Forwarded-Host", "evil.example")], "/local/link-proof", &proof).await?.0 == 403, "forwarded host");
        ensure!(local(&origin, &token, &[], "/local/sign-arbitrary", &proof).await?.0 == 403, "arbitrary signing");
        let (status, linked) = local(&origin, &token, &[], "/local/link-proof", &json!({"requestId": new_id(), "clerkToken": tp.jwt("user_owner")?})).await?;
        ensure!(status == 200, "link: {status} {linked}");
        tokio::time::timeout(std::time::Duration::from_secs(15), child.wait()).await??;
        ensure!(!token_file.exists(), "token file kept");
        let device = text(&linked["data"]["deviceId"]);
        let mut connection = ConnectionProcess::start(&options)?;
        ready(&connection).await?;
        let devices = tp.data("/api/devices").await?;
        ensure!(devices.as_array().map(Vec::len) == Some(1), "devices: {devices}");
        ensure!(devices[0]["id"] == device.as_str(), "device id");
        ensure!(devices[0]["status"] == "ready", "device status");
        ensure!(
            tp.send_as("/api/devices", None, "user_foreign", None).await?.1["data"].as_array().map(Vec::len) == Some(0),
            "foreign devices"
        );
        let project = new_id();
        tp.project(&project, "A").await?;
        let id = new_id();
        let mut create = agent_body(&id, "Remote", "Help with research", &project, &devices[0]);
        create["setup"] = json!(false);
        ensure!(tp.send("/api/agents", Some(create.clone())).await?.0 == 201, "agent");
        ensure!(tp.send("/api/agents", Some(create.clone())).await?.0 == 200, "agent replay");
        use minarbot_testkit::base64::Engine;
        let avatar = minarbot_testkit::noise_image::noise_png(256, 256, 0x5eed)?;
        let avatar_id = new_id();
        let mut avatar_command = create.clone();
        avatar_command["requestId"] = json!(avatar_id);
        avatar_command["id"] = json!(avatar_id);
        avatar_command["avatar"] = json!(minarbot_testkit::base64::engine::general_purpose::STANDARD.encode(&avatar));
        ensure!(avatar_command.to_string().len() > 65536, "avatar body size");
        ensure!(tp.send("/api/agents", Some(avatar_command.clone())).await?.0 == 201, "avatar agent");
        avatar_command["avatar"] = json!("a".repeat(750_000));
        ensure!(tp.send("/api/agents", Some(avatar_command)).await?.0 == 413, "oversized avatar");
        ensure!(
            tp.send_as(
                &format!("/api/projects/{project}/agents/{id}/rules"),
                Some(json!({"requestId": new_id(), "expectedParticipationRevision": 1, "rules": "A only"})),
                "user_owner",
                Some(Method::PUT)
            )
            .await?
            .0 == 200,
            "rules"
        );
        ensure!(
            tp.send_as(
                &format!("/api/projects/{project}/agents/{id}/capabilities"),
                Some(json!({"requestId": new_id(), "expectedRevision": 2,
                    "capabilities": {"v": 1, "tools": [], "resources": []}})),
                "user_owner",
                Some(Method::PUT)
            )
            .await?
            .0 == 200,
            "capabilities"
        );
        connection.stop().await?;
        tp.restart().await?;
        let mut resumed = ConnectionProcess::start(&options)?;
        ready(&resumed).await?;
        ensure!(tp.data("/api/devices").await?[0]["id"] == device.as_str(), "identity kept");
        let current = tp.data("/api/devices").await?[0].clone();
        ensure!(
            tp.send(&format!("/api/devices/{device}/revoke"), Some(json!({"requestId": new_id(), "expectedRevision": current["revision"]})))
                .await?
                .0
                == 200,
            "revoke"
        );
        until("blocked", SETTLE, || async { Ok(resumed.phase() == "blocked") }).await?;
        resumed.wait().await?;
        let device_db = minarbot_testkit::db::Inspector::open(&options.database)?;
        ensure!(device_db.rows("SELECT name FROM sqlite_schema WHERE name='accounts'", [])?.is_empty(), "device accounts table");
        let canonical = tp.inspect()?;
        ensure!(canonical.rows("SELECT name FROM sqlite_schema WHERE name='device_state'", [])?.is_empty(), "canonical device_state");
        ensure!(canonical.scalar::<i64>("SELECT count(*) FROM device_sessions WHERE revoked=0", [])? == 0, "live sessions");
        let mut raw = options.database.as_os_str().to_owned();
        raw.push(".host/identity.json");
        let metadata = std::fs::read_to_string(std::path::PathBuf::from(raw))?;
        for secret in options.keys()?.values() {
            ensure!(!metadata.contains(secret.as_str()), "secret in identity metadata");
        }
        Ok(())
    }
    .await;
    tp.close().await?;
    result
}

#[tokio::test]
async fn device_key_loss_fails_closed_and_concurrent_stream_replacement_fences_old_heartbeat(
) -> Result<()> {
    let tp = Tp::start().await?;
    let result = async {
        let options = HostOptions::new(&tp.dir().join("device.sqlite"), &tp.origin())?;
        enroll(&options, &tp.jwt("user_owner")?, "new").await?;
        let empty = tp.dir().join("empty-keys.json");
        std::fs::write(&empty, "{}")?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&empty, std::fs::Permissions::from_mode(0o600))?;
        let mut keyless = options.clone();
        keyless.keyring = empty.clone();
        let mut missing = ConnectionProcess::start(&keyless)?;
        let status = missing.wait().await?;
        ensure!(!status.success(), "keyless host succeeded");
        until("key error", SETTLE, || async { Ok(missing.stderr().contains("Device key is unavailable")) }).await?;
        ensure!(std::fs::read_to_string(&empty)? == "{}", "Must not replace missing identity key");
        let mut first = ConnectionProcess::start(&options)?;
        ready(&first).await?;
        let mut second = ConnectionProcess::start(&options)?;
        ready(&second).await?;
        until("first fenced", SETTLE, || async { Ok(first.phase() != "connected") }).await?;
        first.stop().await?;
        let current = tp.data("/api/devices").await?[0].clone();
        ensure!(current["status"] == "ready", "status");
        ensure!(current["connection_generation"].as_i64().unwrap_or(0) >= 2, "generation: {current}");
        second.stop().await?;
        Ok(())
    }
    .await;
    tp.close().await?;
    result
}

#[tokio::test]
async fn lost_enrollment_ack_resumes_same_request_and_explicit_reenroll_rotation_preserve_home_identity(
) -> Result<()> {
    let tp = Tp::start().await?;
    let result = async {
        let mut runtime = tp.device("device.sqlite")?;
        let seen = tp.lose_next("/api/devices/link");
        ensure!(runtime.enroll(&tp.jwt("user_owner")?, "new").await.is_err(), "lost enrollment accepted");
        tp.fault(None);
        ensure!(locked(&seen).is_some(), "enrollment reply was not lost");
        let pending = identity(&runtime.options)?["pending"].clone();
        let linked = runtime.enroll(&tp.jwt("user_owner")?, "new").await?;
        ensure!(linked["generation"] == 1, "generation: {linked}");
        let canonical = tp.inspect()?;
        ensure!(canonical.count("devices")? == 1, "devices");
        ensure!(
            canonical.scalar::<i64>("SELECT count(*) FROM receipts WHERE scope='device_enrollment'", [])? == 1,
            "enrollment receipts"
        );
        let request = text(&pending["request"]["requestId"]);
        ensure!(
            canonical.scalar::<String>("SELECT request_id FROM receipts WHERE request_id=?", [&request])? == request,
            "original request"
        );
        let home = runtime.device_id()?;
        let reenrolled = runtime.enroll(&tp.jwt("user_owner")?, "reenroll").await?;
        ensure!(reenrolled["deviceId"] == home.as_str(), "reenroll id");
        ensure!(reenrolled["generation"] == 2, "reenroll generation");
        let before = identity(&runtime.options)?;
        let rotated = runtime.enroll(&tp.jwt("user_owner")?, "rotate").await?;
        ensure!(rotated["deviceId"] == home.as_str(), "rotate id");
        ensure!(rotated["generation"] == 3, "rotate generation");
        let after = identity(&runtime.options)?;
        ensure!(after["signingRef"] != before["signingRef"], "signing key rotated");
        ensure!(after["descriptor"]["keyRevision"] == 2, "key revision");
        let old = format!("/{}", text(&before["signingRef"]));
        ensure!(runtime.keys()?.keys().any(|key| key.ends_with(&old)), "old signing key kept");
        runtime.close().await?;
        runtime = tp.device("device.sqlite")?;
        let connection = runtime.connect().await?;
        until("connected", SETTLE, || async { Ok(connection.phase() == "connected") }).await?;
        Ok(())
    }
    .await;
    tp.fault(None);
    tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_task_http_survives_lost_acceptance_response_restart_without_dispatch_and_hides_private_task(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let options = HostOptions::new(&tp.dir().join("device.sqlite"), &tp.origin())?;
        enroll(&options, &tp.jwt("user_owner")?, "new").await?;
        let home = tp.data("/api/devices").await?[0].clone();
        let (project, agent) = (new_id(), new_id());
        tp.project(&project, "Task project").await?;
        ensure!(tp.send("/api/agents", Some(agent_body(&agent, "Agent", "Research", &project, &home))).await?.0 == 201, "agent");
        let task = new_id();
        let command = task_body(&task, &project, &agent, &text(&home["id"]), &direct("Persistent task"));
        ensure!(tp.send("/api/tasks", Some(command.clone())).await?.0 == 202, "task");
        tp.restart().await?;
        let (status, detail) = tp.send(&format!("/api/tasks/{task}"), None).await?;
        ensure!(status == 200, "detail: {status}");
        ensure!(detail["data"]["state"] == "waiting_device", "state");
        ensure!(tp.send("/api/tasks", Some(command)).await?.0 == 200, "replay");
        ensure!(tp.send_as(&format!("/api/tasks/{task}"), None, "user_foreign", None).await?.0 == 404, "foreign read");
        let db = tp.inspect()?;
        ensure!(db.count("tasks")? == 1, "tasks");
        ensure!(db.count("task_actions")? == 0, "actions");
        Ok(())
    }
    .await;
    tp.close().await?;
    result
}

#[tokio::test]
async fn signed_host_http_admission_commits_once_and_result_ack_survives_canonical_restart(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let runtime = tp.device("device.sqlite")?;
        runtime.enroll(&tp.jwt("user_owner")?, "new").await?;
        let connection = runtime.connect().await?;
        until("connected", SETTLE, || async { Ok(connection.phase() == "connected") }).await?;
        let home = tp.data("/api/devices").await?[0].clone();
        let (project, agent) = (new_id(), new_id());
        tp.project(&project, "Recovery").await?;
        ensure!(tp.send("/api/agents", Some(agent_body(&agent, "Agent", "Research", &project, &home))).await?.0 == 201, "agent");
        let task = new_id();
        ensure!(
            tp.send("/api/tasks", Some(task_body(&task, &project, &agent, &text(&home["id"]), &direct("Persistent result"))))
                .await?
                .0
                == 202,
            "task"
        );
        let acquire_id = new_id();
        let lock_proof = json!({"bootId": new_id(), "processId": 1234, "processStart": "999", "lockId": new_id(),
            "previousOwnerStopped": true, "reconciledThrough": 0, "journalDigest": sha256_hex("journal")});
        let acquire = |request: &str| {
            json!({"taskId": task, "requestId": request, "lockProof": lock_proof, "previousExecutorGeneration": 0})
        };
        let seen = tp.lose_next("/acquire");
        ensure!(runtime.call("acquire", acquire(&acquire_id)).await.is_err(), "lost acquire accepted");
        ensure!(*locked(&seen) == Some(200), "acquire status");
        let acquired = runtime.call("acquire", acquire(&acquire_id)).await?;
        let conflict = runtime.call("acquire", acquire(&new_id())).await;
        ensure!(conflict.as_ref().is_err_and(|error| is_code(error, "ownership_conflict")), "conflict: {conflict:?}");
        tp.fault(None);
        let ownership = acquired["ownership"].clone();
        let context = runtime.context(&task, &ownership, &acquired["revision"]).await?;
        ensure!(
            context["snapshotRef"]["digest"] == sha256_hex(canonical_json(&context["snapshot"])).as_str(),
            "snapshot digest"
        );
        let action = new_id();
        let payload_digest = sha256_hex("synthetic read");
        let snapshot = &context["snapshotRef"];
        let intent = digest(&json!([task, action, 0, ownership, payload_digest, "read", snapshot["snapshotId"],
            snapshot["digest"], null, null, 1, null, null, null]));
        runtime
            .call(
                "prepare",
                json!({"taskId": task, "actionId": action, "body": {"requestId": new_id(), "sequence": 0,
                    "ownership": ownership, "intentDigest": intent, "payloadDigest": payload_digest,
                    "actionKind": "read", "contextSnapshotId": snapshot["snapshotId"],
                    "contextDigest": snapshot["digest"], "grantRevision": 1,
                    "expectedTaskRevision": acquired["revision"], "journalSequence": 1}}),
            )
            .await?;
        let commit = json!({"taskId": task, "actionId": action, "body": {"requestId": new_id(),
            "ownership": ownership, "intentDigest": intent, "expectedTaskRevision": acquired["revision"],
            "journalSequence": 1}});
        ensure!(runtime.call("commit", commit.clone()).await?["dispatchable"] == true, "first commit");
        ensure!(runtime.call("commit", commit).await?["dispatchable"] == false, "second commit");
        let content = json!({"text": "Saved over signed HTTP"});
        let encoded = canonical_json(&content);
        let result_digest = sha256_hex(&encoded);
        let report = json!({"requestId": new_id(), "actionId": action, "ownership": ownership,
            "payloadDigest": payload_digest, "state": "acknowledged", "effectStatus": "known",
            "journalSequence": 2, "resultContent": content, "resultDigest": result_digest,
            "resultRef": {"v": 1, "taskId": task, "actionId": action, "digest": result_digest,
                "size": encoded.len(), "encoding": "canonical-json-v1"}});
        let reconcile = json!({"taskId": task, "body": report});
        let published = runtime.call("reconcile", reconcile.clone()).await?;
        let generation = connection.generation();
        tp.restart().await?;
        until("reconnected", SETTLE, || async {
            Ok(connection.phase() == "connected" && connection.generation() > generation)
        })
        .await?;
        ensure!(runtime.call("reconcile", reconcile).await? == published, "reconcile replay");
        ensure!(tp.task_detail(&task).await?["results"][0]["content"] == content, "result content");
        let refs: String = minarbot_testkit::db::Inspector::open(&runtime.options.database)?
            .scalar("SELECT executor_refs_json FROM device_state WHERE id=1", [])?;
        let canonical = tp.inspect()?;
        let receipts = json!(canonical.rows("SELECT result_json FROM receipts", [])?).to_string();
        for (name, token) in runtime.keys()? {
            if name.contains("/executor/") {
                ensure!(!refs.contains(&token), "executor token in refs");
                ensure!(!receipts.contains(&token), "executor token in receipts");
            }
        }
        ensure!(canonical.count("task_results")? == 1, "task results");
        Ok(())
    }
    .await;
    tp.fault(None);
    tp.close().await?;
    result
}
