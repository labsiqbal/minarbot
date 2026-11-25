//! Canonical routine webhooks: admission, concealment, keyring reads,
//! invalid deliveries, settings authority and failure atomicity.

use super::{
    fixture::{account, until, workspace_snapshot, SETTLE},
    routines::{routine_path, routines_of, text, wall_ms},
    workflow_waits::WorkflowHome,
    workflows::{check, Checks},
};
use minarbot_testkit::prelude::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

/// `routineWebhookFixture`.
struct Webhook {
    w: WorkflowHome,
    routine: String,
    path: String,
    configuration: Value,
    create: Value,
    hook: Value,
}

/// One signed delivery; `extra` sets (`Some`) or removes (`None`) headers.
struct Event<'a> {
    secret: Option<&'a str>,
    id: &'a str,
    body: Vec<u8>,
    time: Option<String>,
    extra: Vec<(&'a str, Option<String>)>,
    route: Option<String>,
}

impl Default for Event<'_> {
    fn default() -> Self {
        Self {
            secret: None,
            id: "fixture.event",
            body: br#"{"event":"ready","payload":{"count":4}}"#.to_vec(),
            time: None,
            extra: Vec::new(),
            route: None,
        }
    }
}

fn now_seconds() -> String {
    (wall_ms() / 1000).to_string()
}

impl Webhook {
    async fn start() -> Result<Self> {
        let w = WorkflowHome::start().await?;
        let routine = new_id();
        let path = routine_path(&w, &routine);
        let configuration = json!({"requestId": routine, "id": routine, "revision": 0, "name": "Webhook",
            "prompt": "PRIVATE_WEBHOOK_PROMPT", "nextAt": wall_ms() + 600_000, "intervalSeconds": 300,
            "enabled": true, "trigger": "webhook"});
        ensure!(
            w.tp.send(&routines_of(&w), Some(configuration.clone()))
                .await?
                .0
                == 201,
            "routine"
        );
        let create = json!({"requestId": new_id(), "revision": 0, "rotate": true,
            "filters": [{"pointer": "/count", "equals": 4}]});
        let (status, hook) =
            w.tp.send(&format!("{path}/webhook"), Some(create.clone()))
                .await?;
        ensure!(status == 201, "webhook: {status} {hook}");
        Ok(Self {
            w,
            routine,
            path,
            configuration,
            create,
            hook,
        })
    }

    fn secret(&self) -> String {
        text(&self.hook["data"]["secret"])
    }

    async fn event(&self, event: Event<'_>) -> Result<(u16, Value)> {
        let secret = event.secret.map_or_else(|| self.secret(), str::to_owned);
        let time = event.time.unwrap_or_else(now_seconds);
        let mut message = format!("{time}\n{}\n", event.id).into_bytes();
        message.extend_from_slice(&event.body);
        let signature = hmac_bytes(&secret, &message);
        let mut headers: Vec<(String, String)> = vec![
            ("Connection".into(), "close".into()),
            ("Content-Type".into(), "application/json".into()),
            ("X-Minarbot-Time".into(), time),
            ("X-Minarbot-Id".into(), event.id.to_owned()),
            ("X-Minarbot-Signature".into(), signature),
        ];
        for (name, value) in event.extra {
            headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
            if let Some(value) = value {
                headers.push((name.to_owned(), value));
            }
        }
        let route = event
            .route
            .unwrap_or_else(|| format!("{}/webhook-events", self.path));
        let mut request = self
            .w
            .tp
            .app
            .server
            .client()?
            .post(format!("{}{route}", self.w.tp.origin()));
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let response = request.body(event.body).send().await?;
        let status = response.status().as_u16();
        Ok((status, response.json().await.unwrap_or(Value::Null)))
    }

    async fn simple(&self) -> Result<(u16, Value)> {
        self.event(Event::default()).await
    }

    fn edit(&self, revision: i64, fields: Value) -> Value {
        let id = new_id();
        let mut body = self.configuration.clone();
        body["requestId"] = json!(id);
        body["id"] = json!(id);
        body["revision"] = json!(revision);
        for (key, value) in fields.as_object().into_iter().flatten() {
            body[key] = value.clone();
        }
        body
    }

    fn stderr(&self) -> Result<String> {
        Ok(self.w.tp.app.output()?.1)
    }
}

fn hmac_bytes(secret: &str, message: &[u8]) -> String {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_bytes());
    ring::hmac::sign(&key, message)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn occurrence_id(routine: &str, event: &str) -> String {
    use minarbot_testkit::base64::Engine;
    let tuple = json!([routine, "webhook", event]).to_string();
    let digest = ring::digest::digest(&ring::digest::SHA256, tuple.as_bytes());
    minarbot_testkit::base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest.as_ref())
}

#[tokio::test]
async fn canonical_routine_webhook_admits_once_offline_and_recovers_lost_response_restart_edits_and_rotation(
) -> Result<()> {
    let mut f = Webhook::start().await?;
    let result = async {
        let secret = f.secret();
        let keys = f.w.tp.app.keys.values();
        ensure!(keys.len() == 1, "keys: {keys:?}");
        ensure!(keys.keys().next().is_some_and(|key| key.contains("/routine-webhooks/")), "key service");
        ensure!(secret.len() == 43, "secret length");
        ensure!(f.w.tp.send(&format!("{}/webhook", f.path), Some(f.create.clone())).await?.1 == f.hook, "create replay");
        let owner = account(&f.w.tp, "user_owner").await?;
        let binding = new_id();
        let db = f.w.tp.write()?;
        db.execute(
            "INSERT INTO provider_bindings VALUES (?,?,?,'openai-compatible','webhook-fixture','Fixture','ready',1)",
            [&binding, &text(&f.w.home["id"]), &owner],
        )?;
        db.execute("UPDATE agent_authority SET provider_binding=?,revision=revision+1 WHERE agent=?", [&binding, &f.w.agent])?;
        let other = f.w.tp.device("webhook-other.sqlite")?;
        other.enroll(&f.w.tp.jwt("user_owner")?, "new").await?;
        let other_connection = other.connect().await?;
        until("other home", SETTLE, || async { Ok(other_connection.phase() == "connected") }).await?;
        let before = f.w.tp.data(&f.path).await?;
        let sync = format!("/api/sync?projectId={}", f.w.project);
        let cursor = f.w.tp.data(&sync).await?["nextCursor"].clone();
        // The first reply is lost after the server admitted the delivery.
        let (status, lost) = f.simple().await?;
        ensure!(status == 202, "lost delivery: {status} {lost}");
        let (status, accepted) = f.simple().await?;
        ensure!(status == 200, "replay: {status} {accepted}");
        let task = text(&accepted["data"]["taskId"]);
        ensure!(accepted["data"]["outcome"] == "queued", "outcome");
        ensure!(accepted["data"]["source"] == "webhook", "source");
        ensure!(accepted["data"]["homeDeviceId"] == f.w.home["id"], "home");
        ensure!(accepted["data"]["occurrenceId"] == occurrence_id(&f.routine, "fixture.event").as_str(), "occurrence id");
        ensure!(f.w.tp.task_detail(&task).await?["state"] == "waiting_device", "task state");
        ensure!(f.w.tp.data(&f.path).await? == before, "configuration changed");
        ensure!(
            f.w.tp.data("/api/devices").await?.as_array().is_some_and(|rows| rows
                .iter()
                .any(|row| row["id"] != f.w.home["id"] && row["status"] == "ready")),
            "other ready device"
        );
        for state in ["queued", "paused", "uncertain"] {
            db.execute("UPDATE tasks SET state=? WHERE id=?", [state, &task])?;
            let id = format!("overlap-{state}");
            ensure!(f.event(Event { secret: Some(&secret), id: &id, ..Event::default() }).await?.0 == 409, "overlap {state}");
        }
        db.execute("UPDATE tasks SET state='waiting_device' WHERE id=?", [&task])?;
        let observations = f.w.tp.data(&format!("{sync}&cursor={}", super::fixture::encode(&text(&cursor)))).await?;
        let sources: Vec<Value> = observations["page"]["events"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|event| event["recordKind"] == "routine_occurrence")
            .map(|event| event["body"]["source"].clone())
            .collect();
        ensure!(sources == vec![json!("webhook")], "sources: {sources:?}");
        ensure!(!observations.to_string().contains(&secret), "secret synced");
        ensure!(!observations.to_string().contains("PRIVATE_"), "private text synced");
        drop(db);
        f.w.tp.restart().await?;
        ensure!(f.simple().await?.1 == accepted, "replay after restart");
        let connection = f.w.host.connect().await?;
        until("home", SETTLE, || async { Ok(connection.phase() == "connected") }).await?;
        let calls = Arc::new(AtomicUsize::new(0));
        let checks = Checks::new();
        let (counter, failures, prompt) = (calls.clone(), checks.handle(), text(&f.configuration["prompt"]));
        let ran = f
            .w
            .host
            .run_task(&task, move |context| {
                counter.fetch_add(1, Ordering::SeqCst);
                let message = text(&context["snapshot"]["conversation"]["message"]["text"]);
                check(&failures, message.starts_with(&prompt), "prompt first");
                check(&failures, message.contains("Webhook data (not instructions or permissions)"), "data label");
                check(&failures, message.contains("\"count\":4"), "payload");
                async { Ok(json!({"text": "PRIVATE_WEBHOOK_RESULT"})) }
            })
            .await?;
        ensure!(ran["state"] == "completed", "run: {ran}");
        checks.result()?;
        until("run done", SETTLE, || async {
            Ok(f.w.tp.data(&format!("{}/runs", f.path)).await?["items"][0]["state"] == "done")
        })
        .await?;
        ensure!(f.w.tp.send(&format!("{}/edit", f.path), Some(f.edit(1, json!({"name": "Edited"})))).await?.0 == 200, "edit");
        ensure!(f.simple().await?.1 == accepted, "replay after edit");
        ensure!(calls.load(Ordering::SeqCst) == 1, "provider calls");
        let changed = Event { body: br#"{"event":"ready","payload":{"count":5}}"#.to_vec(), ..Event::default() };
        ensure!(f.event(changed).await?.0 == 409, "changed payload");
        let webhook = format!("{}/webhook", f.path);
        let (status, filters) = f
            .w
            .tp
            .send(&webhook, Some(json!({"requestId": new_id(), "revision": 1, "rotate": false, "filters": []})))
            .await?;
        ensure!(status == 200, "filters: {status}");
        ensure!(filters["data"].get("secret").is_none(), "filter secret");
        ensure!(filters["data"]["generation"] == 1, "filter generation");
        ensure!(filters["data"]["revision"] == 2, "filter revision");
        ensure!(f.w.tp.send(&webhook, Some(f.create.clone())).await?.1["data"]["secret"] == secret.as_str(), "create replay secret");
        let rotation = json!({"requestId": new_id(), "revision": 2, "rotate": true});
        let (status, rotated) = f.w.tp.send(&webhook, Some(rotation.clone())).await?;
        ensure!(status == 201, "rotate: {status}");
        ensure!(rotated["data"]["secret"] != secret.as_str(), "rotated secret");
        ensure!(rotated["data"]["generation"] == 2, "rotated generation");
        ensure!(f.event(Event { secret: Some(&secret), id: "old-secret", ..Event::default() }).await?.0 == 401, "old secret");
        ensure!(f.event(Event { secret: Some(&secret), ..Event::default() }).await?.0 == 401, "old secret replay");
        let (status, old_retry) = f.w.tp.send(&webhook, Some(f.create.clone())).await?;
        ensure!(status == 200, "old retry: {status}");
        ensure!(old_retry["data"]["rotated"] == true, "rotated flag");
        ensure!(text(&old_retry["data"]["message"]).contains("rotated"), "rotated message");
        ensure!(old_retry["data"].get("secret").is_none(), "old retry secret");
        f.w.tp.restart().await?;
        ensure!(f.w.tp.send(&webhook, Some(rotation)).await?.1 == rotated, "rotation replay");
        let current = text(&rotated["data"]["secret"]);
        ensure!(f.event(Event { secret: Some(&current), ..Event::default() }).await?.1 == accepted, "new secret replay");
        let member = account(&f.w.tp, "user_collaborator").await?;
        let db = f.w.tp.write()?;
        db.execute("UPDATE record_scopes SET account=?,visibility='owner' WHERE table_name='routine_occurrences'", [&member])?;
        ensure!(f.w.tp.data(&webhook).await?["receipts"].as_array().map(Vec::len) == Some(0), "hidden receipts");
        ensure!(f.event(Event { secret: Some(&current), ..Event::default() }).await?.0 == 403, "hidden occurrence");
        db.execute_batch("UPDATE record_scopes SET visibility='project' WHERE table_name='routine_occurrences'")?;
        ensure!(db.scalar::<i64>("SELECT count(*) FROM routine_occurrences", [])? == 1, "occurrences");
        ensure!(db.rows("PRAGMA foreign_key_check", [])?.is_empty(), "foreign keys");
        connection.stop().await?;
        other_connection.stop().await?;
        Ok(())
    }
    .await;
    f.w.tp.close().await?;
    result
}

fn denied(response: &(u16, Value)) -> Result<()> {
    let (status, body) = response;
    ensure!(*status == 401, "status: {status} {body}");
    let request = text(&body["error"]["requestId"]);
    ensure!(
        request.len() == 36 && request.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
        "requestId: {request}"
    );
    let mut error = body["error"].clone();
    if let Some(map) = error.as_object_mut() {
        map.remove("requestId");
    }
    ensure!(
        body["ok"] == false
            && error
                == json!({"code": "invalid_webhook", "message": "Webhook signature does not match.", "retryable": false})
            && body.as_object().map(|map| map.len()) == Some(2),
        "denied body: {body}"
    );
    Ok(())
}

#[tokio::test]
async fn canonical_routine_webhook_conceals_missing_and_inactive_targets_until_signature_verification(
) -> Result<()> {
    let f = Webhook::start().await?;
    let result = async {
        let bad = || {
            f.event(Event {
                secret: Some("wrong-secret"),
                ..Event::default()
            })
        };
        denied(&bad().await?)?;
        for header in ["X-Minarbot-Time", "X-Minarbot-Id", "X-Minarbot-Signature"] {
            for value in [Some(String::new()), Some("invalid!".to_owned()), None] {
                denied(
                    &f.event(Event {
                        extra: vec![(header, value)],
                        ..Event::default()
                    })
                    .await?,
                )?;
            }
        }
        for offset in [-301, 301] {
            denied(
                &f.event(Event {
                    time: Some((wall_ms() / 1000 + offset).to_string()),
                    ..Event::default()
                })
                .await?,
            )?;
        }
        for route in [
            f.path.replace(&f.routine, &new_id()),
            f.path.replace(&f.w.project, &new_id()),
            f.path.replace(&f.w.project, &f.w.other),
        ] {
            denied(
                &f.event(Event {
                    route: Some(format!("{route}/webhook-events")),
                    ..Event::default()
                })
                .await?,
            )?;
        }
        let routine = new_id();
        let no_hook = format!(
            "/api/projects/{}/routines/{routine}/webhook-events",
            f.w.project
        );
        let mut configuration = f.configuration.clone();
        configuration["requestId"] = json!(routine);
        configuration["id"] = json!(routine);
        ensure!(
            f.w.tp
                .send(&routines_of(&f.w), Some(configuration))
                .await?
                .0
                == 201,
            "second routine"
        );
        denied(
            &f.event(Event {
                route: Some(no_hook.clone()),
                ..Event::default()
            })
            .await?,
        )?;
        denied(
            &f.event(Event {
                secret: Some("minarbot-routine-webhook-dummy-secret-v1"),
                body: b"{".to_vec(),
                route: Some(no_hook.clone()),
                ..Event::default()
            })
            .await?,
        )?;
        let keys = f.w.tp.app.keys.clone();
        keys.fail_read.store(true, Ordering::SeqCst);
        denied(
            &f.event(Event {
                route: Some(no_hook),
                ..Event::default()
            })
            .await?,
        )?;
        for extra in [
            vec![],
            vec![("X-Minarbot-Signature", Some(String::new()))],
            vec![("X-Minarbot-Signature", Some("0".repeat(64)))],
        ] {
            let before = f.stderr()?.len();
            denied(
                &f.event(Event {
                    extra,
                    ..Event::default()
                })
                .await?,
            )?;
            until("keystore error logged", SETTLE, || async {
                Ok(f.stderr()?.len() > before)
            })
            .await?;
            let logged = f.stderr()?;
            ensure!(
                logged[before..].trim()
                    == format!("Webhook keystore is unavailable for routine {}.", f.routine),
                "logged: {}",
                &logged[before..]
            );
        }
        let webhook = format!("{}/webhook", f.path);
        ensure!(
            f.w.tp.data(&webhook).await?["keystore"] == "unavailable",
            "keystore unavailable"
        );
        ensure!(
            f.w.tp.send(&webhook, Some(f.create.clone())).await?.0 == 503,
            "create while unavailable"
        );
        let logged = f.stderr()?;
        ensure!(!logged.contains(&f.secret()), "secret logged");
        ensure!(!logged.contains("PRIVATE_"), "private text logged");
        keys.fail_read.store(false, Ordering::SeqCst);
        ensure!(
            f.w.tp.data(&webhook).await?["keystore"] == "ok",
            "keystore ok"
        );
        for (revision, enabled, trigger) in [(1, false, "webhook"), (2, true, "schedule")] {
            ensure!(
                f.w.tp
                    .send(
                        &format!("{}/edit", f.path),
                        Some(f.edit(revision, json!({"enabled": enabled, "trigger": trigger})))
                    )
                    .await?
                    .0
                    == 200,
                "edit {revision}"
            );
            denied(&bad().await?)?;
            let (status, signed) = f.simple().await?;
            ensure!(status == 409, "inactive {revision}: {status}");
            ensure!(
                signed["error"]["code"] == "webhook_inactive",
                "inactive code"
            );
            ensure!(
                signed["error"]["message"] == "Webhook routine is inactive.",
                "inactive message"
            );
        }
        Ok(())
    }
    .await;
    f.w.tp.app.keys.fail_read.store(false, Ordering::SeqCst);
    f.w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_routine_webhook_performs_one_keyring_read_for_configured_and_missing_targets(
) -> Result<()> {
    let f = Webhook::start().await?;
    let result = async {
        let keys = f.w.tp.app.keys.clone();
        let read = |route: String, extra: Vec<(&'static str, Option<String>)>| {
            let (f, keys) = (&f, keys.clone());
            async move {
                keys.clear_reads();
                let (status, _) = f
                    .event(Event {
                        secret: Some("wrong-secret"),
                        extra,
                        route: Some(route),
                        ..Event::default()
                    })
                    .await?;
                ensure!(status == 401, "status: {status}");
                let reads = keys.reads();
                ensure!(reads.len() == 1, "reads: {reads:?}");
                Ok::<(String, String), minarbot_testkit::Error>(reads[0].clone())
            }
        };
        let events = |path: String| format!("{path}/webhook-events");
        let configured = read(events(f.path.clone()), vec![]).await?;
        let missing = read(events(f.path.replace(&f.routine, &new_id())), vec![]).await?;
        let other_missing = read(events(f.path.replace(&f.w.project, &new_id())), vec![]).await?;
        ensure!(missing.0 == configured.0, "service");
        ensure!(missing.1 != configured.1, "name");
        ensure!(other_missing == missing, "other missing");
        ensure!(
            !keys
                .values()
                .contains_key(&format!("{}/{}", missing.0, missing.1)),
            "missing key stored"
        );
        for extra in [
            vec![("X-Minarbot-Signature", Some(String::new()))],
            vec![("X-Minarbot-Id", Some("invalid!".to_owned()))],
            vec![("X-Minarbot-Time", None)],
        ] {
            ensure!(
                read(events(f.path.clone()), extra.clone()).await? == configured,
                "configured {extra:?}"
            );
            ensure!(
                read(events(f.path.replace(&f.routine, &new_id())), extra.clone()).await?
                    == missing,
                "missing {extra:?}"
            );
        }
        Ok(())
    }
    .await;
    f.w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_routine_webhook_rejects_invalid_deliveries_and_records_ignored_receipts_across_filter_changes(
) -> Result<()> {
    let f = Webhook::start().await?;
    let result = async {
        let secret = f.secret();
        let body = br#"{"event":"ready","payload":{"count":4}}"#.to_vec();
        let time = now_seconds();
        let fixed = |id: &'static str, extra: Vec<(&'static str, Option<String>)>| Event {
            secret: Some(&secret),
            id,
            body: body.clone(),
            time: Some(time.clone()),
            extra,
            route: None,
        };
        for header in ["X-Minarbot-Time", "X-Minarbot-Id", "X-Minarbot-Signature"] {
            ensure!(f.event(fixed("bad", vec![(header, Some(String::new()))])).await?.0 == 401, "empty {header}");
        }
        ensure!(f.event(fixed("bad", vec![("X-Minarbot-Signature", Some("0".repeat(64)))])).await?.0 == 401, "zero signature");
        // Fresh whole seconds per request; ±302 keeps a margin over the
        // server's fractional 300 s window.
        for offset in [-302, 302] {
            let time = (wall_ms() / 1000 + offset).to_string();
            ensure!(f.event(Event { secret: Some(&secret), id: "expired", body: body.clone(), time: Some(time), ..Event::default() }).await?.0 == 401, "expired {offset}");
        }
        ensure!(f.event(Event { secret: Some(&secret), id: "large", body: vec![b' '; 65537], ..Event::default() }).await?.0 == 413, "large");
        ensure!(f.event(Event { secret: Some(&secret), id: "json", body: b"{".to_vec(), ..Event::default() }).await?.0 == 400, "json");
        ensure!(f.event(Event { secret: Some(&secret), id: "utf8", body: vec![0xff], ..Event::default() }).await?.0 == 400, "utf8");
        ensure!(f.event(fixed("content", vec![("Content-Type", Some("text/plain".into()))])).await?.0 == 415, "content type");
        ensure!(f.event(fixed("origin", vec![("Origin", Some(f.w.tp.origin()))])).await?.0 == 403, "origin");
        ensure!(f.event(fixed("selector", vec![("X-Minarbot-Project", Some(f.w.project.clone()))])).await?.0 == 404, "selector");
        let changed = br#"{"event":"ready","payload":{"count":5}}"#.to_vec();
        let (status, ignored) = f.event(Event { secret: Some(&secret), id: "ignored", body: changed.clone(), ..Event::default() }).await?;
        ensure!(status == 202, "ignored: {status}");
        ensure!(ignored["data"]["outcome"] == "ignored", "ignored outcome");
        let db = f.w.tp.inspect()?;
        ensure!(db.count("routine_occurrences")? == 0, "occurrences");
        let webhook = format!("{}/webhook", f.path);
        ensure!(f.w.tp.data(&webhook).await?["receipts"][0]["outcome"] == "ignored", "receipt outcome");
        ensure!(
            f.w.tp.send(&webhook, Some(json!({"requestId": new_id(), "revision": 1, "rotate": false, "filters": []}))).await?.0 == 200,
            "filters"
        );
        ensure!(
            f.event(Event { secret: Some(&secret), id: "ignored", body: changed, ..Event::default() }).await?.1 == ignored,
            "ignored replay"
        );
        ensure!(f.event(Event { secret: Some(&secret), id: "ignored", body: body.clone(), ..Event::default() }).await?.0 == 409, "ignored conflict");
        for (revision, enabled, trigger) in [(1, false, "webhook"), (2, true, "schedule")] {
            ensure!(
                f.w.tp.send(&format!("{}/edit", f.path), Some(f.edit(revision, json!({"enabled": enabled, "trigger": trigger})))).await?.0 == 200,
                "edit {revision}"
            );
            let id = format!("inactive-{revision}");
            ensure!(f.event(Event { secret: Some(&secret), id: &id, ..Event::default() }).await?.0 == 409, "inactive {revision}");
            ensure!(f.w.tp.data(&webhook).await?["enabled"] == false, "enabled {revision}");
        }
        ensure!(
            f.w.tp.send(&webhook, Some(f.create.clone())).await?.1["data"]["secret"] == secret.as_str(),
            "Current-generation exact retry survives trigger edits"
        );
        ensure!(db.count("routine_occurrences")? == 0, "occurrences after edits");
        ensure!(db.count("routine_webhook_receipts")? == 1, "webhook receipts");
        Ok(())
    }
    .await;
    f.w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_routine_webhook_settings_require_current_owner_while_assigned_collaborator_can_edit(
) -> Result<()> {
    let f = Webhook::start().await?;
    let result = async {
        let (member, owner) = (account(&f.w.tp, "user_collaborator").await?, account(&f.w.tp, "user_owner").await?);
        let db = f.w.tp.write()?;
        db.execute("INSERT INTO project_members VALUES (?,?,'member',1,0)", [&f.w.project, &member])?;
        db.execute("INSERT INTO collaborator_agents VALUES (?,?,?,1,1)", [&f.w.project, &member, &f.w.agent])?;
        let as_member = |path: String, body: Option<Value>| {
            let tp = &f.w.tp;
            async move { tp.send_as(&path, body, "user_collaborator", None).await }
        };
        ensure!(as_member(format!("{}/edit", f.path), Some(f.edit(1, json!({})))).await?.0 == 200, "member edit");
        let webhook = format!("{}/webhook", f.path);
        ensure!(as_member(webhook.clone(), None).await?.0 == 403, "member settings read");
        for command in [f.create.clone(), json!({"requestId": new_id(), "revision": 1, "rotate": true})] {
            ensure!(as_member(webhook.clone(), Some(command)).await?.0 == 403, "member settings write");
        }
        let foreign = f.path.replace(&f.w.project, &f.w.other);
        ensure!(f.w.tp.send(&format!("{foreign}/webhook"), None).await?.0 == 404, "foreign read");
        ensure!(
            f.w.tp.send(&format!("{foreign}/webhook"), Some(json!({"requestId": new_id(), "revision": 1, "rotate": true}))).await?.0 == 404,
            "foreign rotate"
        );
        let secret = f.secret();
        ensure!(
            f.event(Event { secret: Some(&secret), id: "foreign", route: Some(format!("{foreign}/webhook-events")), ..Event::default() }).await?.0 == 401,
            "foreign delivery"
        );
        for extra in [json!({"unexpected": true}), json!({"rotate": "yes"}), json!({"revision": -1}), json!({"filters": [{}]})] {
            let mut body = f.create.clone();
            body["requestId"] = json!(new_id());
            for (key, value) in extra.as_object().into_iter().flatten() {
                body[key] = value.clone();
            }
            ensure!(f.w.tp.send(&webhook, Some(body)).await?.0 == 400, "invalid {extra}");
        }
        let mut conflict_body = f.create.clone();
        conflict_body["rotate"] = json!(false);
        let (status, conflict) = f.w.tp.send(&webhook, Some(conflict_body)).await?;
        ensure!(status == 409, "conflict: {status}");
        ensure!(conflict["error"]["code"] == "payload_conflict", "conflict code");
        db.execute(
            "UPDATE project_members SET deleted=1,grant_revision=grant_revision+1 WHERE project=? AND account=?",
            [&f.w.project, &owner],
        )?;
        ensure!(f.w.tp.send(&webhook, None).await?.0 == 404, "removed owner read");
        ensure!(f.w.tp.send(&webhook, Some(f.create.clone())).await?.0 == 404, "removed owner create");
        ensure!(f.simple().await?.0 == 403, "removed owner delivery");
        db.execute("UPDATE project_members SET deleted=0 WHERE project=? AND account=?", [&f.w.project, &owner])?;
        db.execute("UPDATE accounts SET blocked=1 WHERE id=?", [&owner])?;
        ensure!(f.w.tp.send(&webhook, None).await?.0 == 401, "blocked owner read");
        ensure!(f.simple().await?.0 == 403, "blocked owner delivery");
        ensure!(db.scalar::<i64>("SELECT count(*) FROM routine_occurrences", [])? == 0, "occurrences");
        Ok(())
    }
    .await;
    f.w.tp.close().await?;
    result
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

#[tokio::test]
async fn canonical_routine_webhook_keyring_and_db_failures_leave_no_active_reference_or_partial_admission_and_exclude_secrets(
) -> Result<()> {
    let f = Webhook::start().await?;
    let result = async {
        let keys = f.w.tp.app.keys.clone();
        let db = f.w.tp.write()?;
        let row = db.row("SELECT * FROM routine_webhooks", [])?;
        let webhook = format!("{}/webhook", f.path);
        let rotate = json!({"requestId": new_id(), "revision": 1, "rotate": true});
        keys.fail_write.store(true, Ordering::SeqCst);
        ensure!(f.w.tp.send(&webhook, Some(rotate.clone())).await?.0 == 503, "write failure");
        keys.fail_write.store(false, Ordering::SeqCst);
        keys.fail_read.store(true, Ordering::SeqCst);
        ensure!(f.w.tp.send(&webhook, Some(rotate.clone())).await?.0 == 503, "read failure");
        keys.fail_read.store(false, Ordering::SeqCst);
        ensure!(db.row("SELECT * FROM routine_webhooks", [])? == row, "webhook changed");
        ensure!(db.rows("SELECT 1 FROM receipts WHERE request_id=?", [&text(&rotate["requestId"])])?.is_empty(), "rotation receipt");
        let new_routine = new_id();
        let detail = routine_path(&f.w, &new_routine);
        let mut configuration = f.configuration.clone();
        configuration["requestId"] = json!(new_routine);
        configuration["id"] = json!(new_routine);
        ensure!(f.w.tp.send(&routines_of(&f.w), Some(configuration)).await?.0 == 201, "second routine");
        let create = json!({"requestId": new_id(), "revision": 0, "rotate": true});
        keys.fail_read.store(true, Ordering::SeqCst);
        ensure!(f.w.tp.send(&format!("{detail}/webhook"), Some(create.clone())).await?.0 == 503, "create read failure");
        keys.fail_read.store(false, Ordering::SeqCst);
        ensure!(db.rows("SELECT 1 FROM routine_webhooks WHERE routine=?", [&new_routine])?.is_empty(), "partial webhook");
        ensure!(f.w.tp.data(&format!("{detail}/webhook")).await?["generation"] == 0, "generation");
        db.execute_batch("CREATE TRIGGER fail_webhook_command BEFORE INSERT ON receipts WHEN NEW.scope LIKE 'retained:%/webhook' BEGIN SELECT RAISE(ABORT,'webhook DB failure'); END")?;
        ensure!(f.w.tp.send(&format!("{detail}/webhook"), Some(create.clone())).await?.0 == 500, "create DB failure");
        ensure!(db.rows("SELECT 1 FROM routine_webhooks WHERE routine=?", [&new_routine])?.is_empty(), "partial webhook after DB failure");
        ensure!(f.w.tp.send(&webhook, Some(rotate)).await?.0 == 500, "rotate DB failure");
        ensure!(db.row("SELECT * FROM routine_webhooks WHERE routine=?", [&f.routine])? == row, "webhook after DB failure");
        db.execute_batch("DROP TRIGGER fail_webhook_command")?;
        let tables = ["receipts", "tasks", "turns", "threads", "record_scopes", "sync_events", "routine_occurrences", "routine_steps"];
        let counts = || -> Result<Vec<i64>> {
            tables.iter().map(|table| db.scalar::<i64>(&format!("SELECT count(*) FROM {table}"), [])).collect()
        };
        for failure in [
            "CREATE TRIGGER fail_webhook_event BEFORE INSERT ON routine_webhook_receipts BEGIN SELECT RAISE(ABORT,'event failure'); END",
            "CREATE TRIGGER fail_webhook_event BEFORE INSERT ON sync_events WHEN NEW.record_kind='routine_occurrence' BEGIN SELECT RAISE(ABORT,'event failure'); END",
        ] {
            let before = counts()?;
            db.execute_batch(failure)?;
            ensure!(f.simple().await?.0 == 500, "event failure");
            db.execute_batch("DROP TRIGGER fail_webhook_event")?;
            ensure!(counts()? == before, "partial admission");
            ensure!(db.scalar::<i64>("SELECT count(*) FROM routine_webhook_receipts", [])? == 0, "webhook receipts");
        }
        ensure!(f.simple().await?.0 == 202, "delivery");
        let reads = json!([
            f.w.tp.send(&f.path, None).await?.1,
            f.w.tp.send(&webhook, None).await?.1,
            f.w.tp.send(&format!("{}/runs", f.path), None).await?.1,
            f.w.tp.send(&format!("/api/sync?projectId={}", f.w.project), None).await?.1,
        ]);
        let snapshot = workspace_snapshot(&f.w.tp, None)?;
        let snapshot_bytes = std::fs::read(&snapshot)?;
        let database_bytes = std::fs::read(f.w.tp.database())?;
        let (stdout, stderr) = f.w.tp.app.output()?;
        let logs = json!([reads, stdout, stderr]).to_string();
        let names: Vec<String> = db
            .rows("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'", [])?
            .iter()
            .map(|row| text(&row["name"]))
            .collect();
        for value in keys.values().values() {
            ensure!(!contains(&snapshot_bytes, value.as_bytes()), "secret in snapshot");
            ensure!(!logs.contains(value.as_str()), "secret in reads or logs");
            ensure!(!contains(&database_bytes, value.as_bytes()), "secret in database file");
            for name in &names {
                ensure!(
                    !json!(db.rows(&format!("SELECT * FROM \"{name}\""), [])?).to_string().contains(value.as_str()),
                    "secret in {name}"
                );
            }
        }
        ensure!(f.w.tp.send(&webhook, Some(f.create.clone())).await?.1["data"]["secret"] == f.hook["data"]["secret"], "create replay");
        ensure!(f.w.tp.send(&format!("{detail}/webhook"), Some(create)).await?.0 == 201, "create after failures");
        ensure!(db.rows("PRAGMA foreign_key_check", [])?.is_empty(), "foreign keys");
        Ok(())
    }
    .await;
    f.w.tp.app.keys.fail_read.store(false, Ordering::SeqCst);
    f.w.tp.app.keys.fail_write.store(false, Ordering::SeqCst);
    f.w.tp.close().await?;
    result
}
