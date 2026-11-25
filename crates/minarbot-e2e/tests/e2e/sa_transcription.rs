//! Audio transcription consent, isolation, uncertainty and usage (port of
//! `app/transcription.test.ts`). The transcription provider is a direct HTTP
//! integration (`transcriptionProvider`), not an agent harness.

use crate::sa_support::{api_agent, id, lock, RawAction, RawProvider, RawRequest, Shared};
use minarbot_testkit::{db::Fixture, http::Method, prelude::*, Server};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::Notify;

fn json_reply(body: Value) -> (u16, Vec<(String, String)>, Vec<u8>) {
    (
        200,
        vec![("content-type".into(), "application/json".into())],
        body.to_string().into_bytes(),
    )
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn mp4_header(brand: &str) -> Vec<u8> {
    let mut bytes = vec![0_u8; 24];
    bytes[..4].copy_from_slice(&24_u32.to_be_bytes());
    bytes[4..8].copy_from_slice(b"ftyp");
    bytes[8..12].copy_from_slice(brand.as_bytes());
    bytes[16..20].copy_from_slice(b"isom");
    bytes[20..24].copy_from_slice(b"mp42");
    bytes
}

async fn upload(app: &Server, path: &str, name: &str, bytes: Vec<u8>) -> Result<u16> {
    Ok(app
        .request(Method::POST, path)?
        .header("content-type", "application/octet-stream")
        .header("x-file-name", name)
        .body(bytes)
        .send()
        .await?
        .status()
        .as_u16())
}

async fn status(app: &Server, path: &str, body: Value) -> Result<u16> {
    Ok(app.call(Method::POST, path, Some(body)).await?.status)
}

async fn usage(app: &Server) -> Result<Value> {
    Ok(app.get("/api/usage").await?["transcription"].clone())
}

#[derive(Default)]
struct Consent {
    invalid: &'static str,
    violations: Vec<String>,
}

#[tokio::test]
async fn http_transcription_requires_consent_isolates_files_and_never_silently_replays_uncertain_requests(
) -> Result<()> {
    let mut bytes = mp4_header("M4A ");
    bytes.extend_from_slice(b"synthetic audio transport bytes");
    let calls = Arc::new(AtomicUsize::new(0));
    let state: Shared<Consent> = Shared::default();
    let (counter, script, audio) = (calls.clone(), state.clone(), bytes.clone());
    let provider = RawProvider::start(move |request: &RawRequest| {
        counter.fetch_add(1, Ordering::SeqCst);
        let Ok(mut state) = script.lock() else {
            return (500, vec![], Vec::new());
        };
        if request.path != "/v1/audio/transcriptions" {
            state.violations.push(format!("path {}", request.path));
        }
        if request.authorization.as_deref() != Some("Bearer synthetic-secret") {
            state.violations.push(format!("authorization {:?}", request.authorization));
        }
        for needle in [&audio[..], b"fixture-transcribe", b"filename=\"audio.mp4\""] {
            if !contains(&request.body, needle) {
                state.violations.push(format!("body lacks {}", String::from_utf8_lossy(needle)));
            }
        }
        match state.invalid {
            "utf8" => (
                200,
                vec![("content-type".into(), "application/json".into())],
                [&b"{\"text\":\""[..], &[255], &b"\"}"[..]].concat(),
            ),
            // A lone surrogate escape, as JSON.stringify('\ud800') writes it.
            "surrogate" => (
                200,
                vec![("content-type".into(), "application/json".into())],
                br#"{"usage":{"type":"tokens","input_tokens":0,"output_tokens":2,"total_tokens":2},"text":"\ud800"}"#
                    .to_vec(),
            ),
            mode => json_reply(json!({"usage": {"type": "tokens", "input_tokens": 0, "output_tokens": 2,
                "total_tokens": 2}, "text": if mode == "secret" {
                    "synthetic-secret"
                } else {
                    "Result transcription.\nBaris dua."
                }})),
        }
    })
    .await?;
    let options = ServerConfig::new().option(
        "transcriptionProvider",
        json!({"baseURL": provider.base_url(), "model": "fixture-transcribe", "apiKey": "synthetic-secret"}),
    );
    let root = TestRoot::new()?;
    let mut app = root.start(options).await?;
    let agent = id(&api_agent(&app, "Audio").await?)?;
    let room = id(&crate::sa_support::channel(&app, "Audio room", &agent, &[&agent]).await?)?;
    let thread = id(&crate::sa_support::side_thread(&app, &agent, "Audio side").await?)?;
    let unsupported = format!("/api/agents/{agent}/files/{}", new_id());
    upload(&app, &unsupported, "fake.wav", b"not audio".to_vec()).await?;
    let info = app.get(&format!("{unsupported}?transcription=1")).await?;
    ensure!(info["format"].is_null(), "unsupported format: {info}");
    ensure!(
        status(
            &app,
            &format!("{unsupported}?transcription=1"),
            json!({"id": new_id(), "provider": info["provider"]["id"], "consent": true}),
        )
        .await?
            == 415,
        "unsupported audio accepted"
    );
    ensure!(
        calls.load(Ordering::SeqCst) == 0,
        "provider called for unsupported audio"
    );
    for scope in [
        format!("agents/{agent}"),
        format!("channels/{room}"),
        format!("threads/{thread}"),
    ] {
        let path = format!("/api/{scope}/files/{}", new_id());
        ensure!(
            upload(&app, &path, "private.wav", bytes.clone()).await? == 200,
            "upload in {scope}"
        );
        let endpoint = format!("{path}?transcription=1");
        let info = app.get(&endpoint).await?;
        let request = json!({"id": new_id(), "provider": info["provider"]["id"], "consent": true});
        let mut refused = request.clone();
        refused["consent"] = json!(false);
        ensure!(
            status(&app, &endpoint, refused).await? == 400,
            "consent false accepted"
        );
        let outside = app
            .client()?
            .post(app.url(&endpoint))
            .header("origin", "https://outside.test")
            .header("content-type", "application/json")
            .body(request.to_string())
            .send()
            .await?
            .status();
        ensure!(outside == 403, "outside origin: {outside}");
        let mut stale = request.clone();
        stale["provider"] = json!("stale");
        ensure!(
            status(&app, &endpoint, stale).await? == 409,
            "stale provider accepted"
        );
        let before = calls.load(Ordering::SeqCst);
        let result = app.post(&endpoint, request.clone()).await?;
        ensure!(result["status"] == "done", "{scope} result: {result}");
        ensure!(
            result["usage"]
                == json!({"type": "tokens", "input_tokens": 0, "output_tokens": 2, "total_tokens": 2}),
            "usage: {result}"
        );
        ensure!(
            app.post(&endpoint, request.clone()).await? == result,
            "replay differs"
        );
        ensure!(
            calls.load(Ordering::SeqCst) == before + 1,
            "replay called the provider"
        );
        ensure!(result.get("request").is_none(), "request echoed: {result}");
        let output = format!(
            "/api/{scope}/files/{}",
            result["result"]["id"].as_str().unwrap_or_default()
        );
        let text = app
            .request(Method::GET, &output)?
            .send()
            .await?
            .text()
            .await?;
        ensure!(
            text == "Result transcription.\nBaris dua.",
            "output text: {text}"
        );
        let foreign = app
            .request(
                Method::GET,
                &format!(
                    "/api/agents/{}/files/{}",
                    new_id(),
                    result["result"]["id"].as_str().unwrap_or_default()
                ),
            )?
            .send()
            .await?
            .status();
        ensure!(foreign == 404, "foreign scope served the result: {foreign}");
        let mut other = request.clone();
        other["id"] = json!(new_id());
        ensure!(
            status(&app, &endpoint, other).await? == 409,
            "second request accepted"
        );
    }
    let path = format!("/api/agents/{agent}/files/{}", new_id());
    upload(&app, &path, "retry.wav", bytes.clone()).await?;
    let endpoint = format!("{path}?transcription=1");
    let info = app.get(&endpoint).await?;
    let request = json!({"id": new_id(), "provider": info["provider"]["id"], "consent": true});
    lock(&state)?.invalid = "secret";
    ensure!(
        app.post(&endpoint, request.clone()).await?["status"] == "uncertain",
        "secret output accepted"
    );
    let count = calls.load(Ordering::SeqCst);
    app.restart().await?;
    ensure!(
        app.get(&endpoint).await?["operation"]["status"] == "uncertain",
        "uncertain after restart"
    );
    ensure!(
        app.post(&endpoint, request.clone()).await?["status"] == "uncertain",
        "uncertain replay"
    );
    ensure!(
        calls.load(Ordering::SeqCst) == count,
        "uncertain request replayed"
    );
    let mut other = request.clone();
    other["id"] = json!(new_id());
    ensure!(
        status(&app, &endpoint, other).await? == 409,
        "new request without retryOf accepted"
    );
    for bad in ["surrogate", "utf8"] {
        lock(&state)?.invalid = bad;
        let mut attempt = request.clone();
        attempt["id"] = json!(new_id());
        attempt["retryOf"] = app.get(&endpoint).await?["operation"]["id"].clone();
        ensure!(
            app.post(&endpoint, attempt).await?["status"] == "uncertain",
            "{bad} output accepted"
        );
    }
    lock(&state)?.invalid = "";
    let mut last = request.clone();
    last["id"] = json!(new_id());
    last["retryOf"] = app.get(&endpoint).await?["operation"]["id"].clone();
    ensure!(
        app.post(&endpoint, last).await?["status"] == "done",
        "valid retry failed"
    );
    ensure!(calls.load(Ordering::SeqCst) == count + 3, "retry count");
    let summary = usage(&app).await?;
    ensure!(
        summary["calls"] == calls.load(Ordering::SeqCst),
        "usage calls: {summary}"
    );
    ensure!(summary["total_tokens"] == "12", "usage total: {summary}");
    ensure!(summary["input_tokens"] == "0", "usage input: {summary}");
    ensure!(summary["unknown"] == 1, "usage unknown: {summary}");
    app.restart().await?;
    ensure!(usage(&app).await? == summary, "usage after restart");
    let violations = lock(&state)?.violations.clone();
    ensure!(violations.is_empty(), "provider checks: {violations:?}");
    Ok(())
}

#[tokio::test]
async fn in_flight_transcription_deduplicates_and_cannot_publish_after_archive_or_shutdown(
) -> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let arrived = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (counter, entered, gate) = (calls.clone(), arrived.clone(), release.clone());
    let provider = RawProvider::start_async(Arc::new(move |_request| {
        let (counter, entered, gate) = (counter.clone(), entered.clone(), gate.clone());
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            let released = gate.notified();
            entered.notify_one();
            released.await;
            RawAction::Reply(json_reply(json!({"text": "Held result"})))
        })
    }))
    .await?;
    let options = ServerConfig::new().option(
        "transcriptionProvider",
        json!({"baseURL": provider.base_url(), "model": "fixture"}),
    );
    let root = TestRoot::new()?;
    let mut app = root.start(options).await?;
    let agent = id(&api_agent(&app, "Held").await?)?;
    for shutdown in [false, true] {
        let thread = id(&crate::sa_support::side_thread(&app, &agent, "Side").await?)?;
        let files = format!("/api/threads/{thread}/files");
        let path = format!("{files}/{}", new_id());
        ensure!(
            upload(&app, &path, "audio.wav", b"RIFF0000WAVEfixture".to_vec()).await? == 200,
            "upload"
        );
        let endpoint = format!("{path}?transcription=1");
        let info = app.get(&endpoint).await?;
        let request = json!({"id": new_id(), "provider": info["provider"]["id"], "consent": true});
        let waiting = arrived.notified();
        let pending = {
            let call = app
                .request(Method::POST, &endpoint)?
                .header("content-type", "application/json")
                .body(request.to_string());
            tokio::spawn(async move { call.send().await.ok() })
        };
        tokio::time::timeout(Duration::from_secs(30), waiting).await?;
        let count = calls.load(Ordering::SeqCst);
        ensure!(
            app.post(&endpoint, request.clone()).await?["status"] == "pending",
            "duplicate status"
        );
        let mut other = request.clone();
        other["id"] = json!(new_id());
        ensure!(
            status(&app, &endpoint, other).await? == 409,
            "parallel request accepted"
        );
        ensure!(
            calls.load(Ordering::SeqCst) == count,
            "duplicate reached the provider"
        );
        if shutdown {
            app.shutdown().await?;
            let _ = pending.await;
            release.notify_one();
            let config = app.config().clone();
            app.start_stopped(config).await?;
        } else {
            app.post(
                &format!("/api/threads/{thread}/edit"),
                json!({"id": new_id(), "revision": 1, "name": "Side", "archived": true}),
            )
            .await?;
            release.notify_one();
            let response = pending.await?.ok_or("held request failed")?;
            let body: Value = response.json().await?;
            ensure!(body["status"] == "uncertain", "archived result: {body}");
        }
        ensure!(
            app.get(&endpoint).await?["operation"]["status"] == "uncertain",
            "operation after {}",
            if shutdown { "shutdown" } else { "archive" }
        );
        ensure!(
            app.get(&files).await?.as_array().map(Vec::len) == Some(1),
            "result published"
        );
        ensure!(calls.load(Ordering::SeqCst) == count, "request replayed");
    }
    release.notify_one();
    Ok(())
}

#[tokio::test]
async fn transcription_is_opt_in_and_validates_request_bodies() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let agent = id(&api_agent(&app, "Off").await?)?;
    let path = format!("/api/agents/{agent}/files/{}", new_id());
    upload(&app, &path, "audio.wav", b"RIFF0000WAVEfixture".to_vec()).await?;
    let endpoint = format!("{path}?transcription=1");
    ensure!(
        app.get(&endpoint).await?["provider"].is_null(),
        "provider without configuration"
    );
    ensure!(
        status(
            &app,
            &endpoint,
            json!({"id": new_id(), "provider": "none", "consent": true})
        )
        .await?
            == 503,
        "transcription without provider"
    );
    ensure!(
        status(
            &app,
            &endpoint,
            json!({"id": new_id(), "provider": {}, "consent": true})
        )
        .await?
            == 400,
        "object provider"
    );
    ensure!(
        status(&app, &endpoint, json!([])).await? == 400,
        "array body"
    );
    ensure!(
        app.call(Method::DELETE, &endpoint, None).await?.status == 405,
        "DELETE allowed"
    );
    Ok(())
}

#[derive(Default)]
struct Bounds {
    mode: &'static str,
    calls: usize,
    redirected: usize,
    violations: Vec<String>,
}

#[tokio::test]
async fn transcription_rejects_redirects_and_oversized_output_and_script_grants_cannot_replace_consent(
) -> Result<()> {
    let state: Shared<Bounds> = Shared::default();
    let script = state.clone();
    let provider = RawProvider::start(move |request| {
        let Ok(mut state) = script.lock() else {
            return (500, vec![], Vec::new());
        };
        if !contains(&request.body, b"filename=\"audio.webm\"") {
            state.violations.push("body lacks filename=\"audio.webm\"".into());
        }
        if request.path == "/unexpected" {
            state.redirected += 1;
            return json_reply(json!({}));
        }
        state.calls += 1;
        match state.mode {
            "redirect" => (307, vec![("location".into(), "/unexpected".into())], Vec::new()),
            "body" => json_reply(json!({"text": "Valid text", "padding": "x".repeat(512 * 1024)})),
            "text" => json_reply(json!({"text": "x".repeat(256 * 1024 + 1)})),
            "empty" => json_reply(json!({"text": " \n ", "usage": {"type": "tokens", "input_tokens": "2",
                "output_tokens": -1, "total_tokens": 1.1}})),
            _ => json_reply(json!({"text": "Accepted final text", "usage": {"type": "duration", "seconds": 1.25}})),
        }
    })
    .await?;
    let token = "synthetic-token-for-transcription-tests-123456789";
    let root = TestRoot::new()?;
    let app = root
        .start(ServerConfig::new().option("scriptToken", token).option(
            "transcriptionProvider",
            json!({"baseURL": provider.base_url(), "model": "fixture"}),
        ))
        .await?;
    let agent = id(&api_agent(&app, "Bounds").await?)?;
    app.post_expect(
        &format!("/api/agents/{agent}/scripts"),
        json!({"id": new_id(), "revision": 0, "enabled": true}),
        200,
    )
    .await?;
    let thread = id(&crate::sa_support::side_thread(&app, &agent, "Bounds").await?)?;
    let file = new_id();
    let files = format!("/api/threads/{thread}/files");
    let path = format!("{files}/{file}");
    let endpoint = format!("{path}?transcription=1");
    let webm = vec![
        0x1a, 0x45, 0xdf, 0xa3, 0x87, 0x42, 0x82, 0x84, 0x77, 0x65, 0x62, 0x6d,
    ];
    ensure!(
        upload(&app, &path, "audio.wav", webm).await? == 200,
        "upload"
    );
    let info = app.get(&endpoint).await?;
    let script = app.url(&format!(
        "/api/scripts/agents/{agent}/threads/{thread}/files/{file}?transcription=1"
    ));
    for method in ["GET", "POST"] {
        let mut request = app
            .client()?
            .request(Method::from_bytes(method.as_bytes())?, &script)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json");
        if method == "POST" {
            request = request.body(
                json!({"id": new_id(), "provider": info["provider"]["id"], "consent": true})
                    .to_string(),
            );
        }
        let response = request.send().await?;
        ensure!(
            response.status() == 403,
            "script {method}: {}",
            response.status()
        );
        let body: Value = response.json().await?;
        ensure!(
            body["error"]
                .as_str()
                .is_some_and(|e| e.contains("Transcription requires user approval in Minarbot.")),
            "script {method} error: {body}"
        );
    }
    ensure!(
        lock(&state)?.calls == 0,
        "script grant reached the provider"
    );
    let mut retry_of: Option<String> = None;
    for mode in ["redirect", "body", "text", "empty", "valid"] {
        lock(&state)?.mode = mode;
        let mut request =
            json!({"id": new_id(), "provider": info["provider"]["id"], "consent": true});
        if let Some(previous) = &retry_of {
            request["retryOf"] = json!(previous);
        }
        let reply = app
            .call(Method::POST, &endpoint, Some(request.clone()))
            .await?;
        ensure!(
            reply.status == 200,
            "{mode}: {} {}",
            reply.status,
            reply.body
        );
        let result = reply.body;
        ensure!(
            result["status"] == if mode == "valid" { "done" } else { "uncertain" },
            "{mode} status: {result}"
        );
        ensure!(lock(&state)?.redirected == 0, "redirect followed");
        let before = lock(&state)?.calls;
        ensure!(
            app.post(&endpoint, request.clone()).await? == result,
            "{mode} replay"
        );
        ensure!(
            lock(&state)?.calls == before,
            "{mode} replay called the provider"
        );
        let listed = app.get(&files).await?;
        ensure!(
            listed.as_array().map(Vec::len) == Some(if mode == "valid" { 2 } else { 1 }),
            "{mode} files: {listed}"
        );
        retry_of = request["id"].as_str().map(str::to_owned);
    }
    ensure!(
        lock(&state)?.calls == 5,
        "provider calls: {}",
        lock(&state)?.calls
    );
    let summary = usage(&app).await?;
    ensure!(summary["seconds"] == 1.25, "seconds: {summary}");
    ensure!(summary["total_tokens"].is_null(), "total tokens: {summary}");
    ensure!(summary["unknown"] == 4, "unknown: {summary}");
    ensure!(
        app.get(&endpoint).await?["operation"]["usage"]
            == json!({"type": "duration", "seconds": 1.25}),
        "operation usage"
    );
    let violations = lock(&state)?.violations.clone();
    ensure!(violations.is_empty(), "provider checks: {violations:?}");
    Ok(())
}

#[tokio::test]
async fn rust_upgrades_legacy_transcription_operations_and_reports_unknown_and_duration_usage(
) -> Result<()> {
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new()).await?;
    let agent = id(&api_agent(&app, "Legacy").await?)?;
    let (file, operation) = (new_id(), new_id());
    let path = format!("/api/agents/{agent}/files/{file}");
    ensure!(
        upload(&app, &path, "old.wav", b"RIFF0000WAVEfixture".to_vec()).await? == 200,
        "upload"
    );
    app.edit_database(|db| {
        db.execute_batch("ALTER TABLE transcriptions DROP COLUMN usage")?;
        db.execute(
            "INSERT INTO transcriptions VALUES (?,?,'{}','fixture','http://localhost','pending',NULL,'2026-01-01')",
            [&operation, &file],
        )?;
        Ok(())
    })
    .await?;
    let info = app.get(&format!("{path}?transcription=1")).await?;
    ensure!(
        info["operation"]["status"] == "uncertain",
        "legacy pending: {info}"
    );
    ensure!(info["operation"]["usage"].is_null(), "legacy usage: {info}");
    ensure!(usage(&app).await?["unknown"] == 1, "unknown usage");
    Fixture::open(app.database())?.execute(
        "UPDATE transcriptions SET usage=? WHERE id=?",
        [
            json!({"type": "duration", "seconds": 1.25}).to_string(),
            operation,
        ],
    )?;
    ensure!(usage(&app).await?["seconds"] == 1.25, "duration usage");
    ensure!(usage(&app).await?["total_tokens"].is_null(), "token usage");
    Ok(())
}

#[tokio::test]
async fn cancelling_streamed_transcription_persists_without_publication_or_replay() -> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let holding = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let received = Arc::new(Notify::new());
    let disconnected = Arc::new(Notify::new());
    let (counter, hold, arrived, closed) = (
        calls.clone(),
        holding.clone(),
        received.clone(),
        disconnected.clone(),
    );
    let provider = RawProvider::start_async(Arc::new(move |_request| {
        let (counter, hold, arrived, closed) = (
            counter.clone(),
            hold.clone(),
            arrived.clone(),
            closed.clone(),
        );
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            if !hold.load(Ordering::SeqCst) {
                return RawAction::Reply(json_reply(json!({"text": "New result"})));
            }
            arrived.notify_one();
            RawAction::Partial(
                (
                    200,
                    vec![("content-type".into(), "application/json".into())],
                    b"{\"text\":\"unfinished".to_vec(),
                ),
                closed,
            )
        })
    }))
    .await?;
    let options = ServerConfig::new().option(
        "transcriptionProvider",
        json!({"baseURL": provider.base_url(), "model": "fixture"}),
    );
    let root = TestRoot::new()?;
    let mut app = root.start(options).await?;
    let agent = id(&api_agent(&app, "Cancel").await?)?;
    let files = format!("/api/agents/{agent}/files");
    let file = new_id();
    let endpoint = format!("{files}/{file}?transcription=1");
    ensure!(
        upload(
            &app,
            &format!("{files}/{file}"),
            "audio.wav",
            b"RIFF0000WAVEfixture".to_vec()
        )
        .await?
            == 200,
        "upload"
    );
    let info = app.get(&endpoint).await?;
    let request = json!({"id": new_id(), "provider": info["provider"]["id"], "consent": true});
    let waiting = received.notified();
    let closed = disconnected.notified();
    let pending = {
        let call = app
            .request(Method::POST, &endpoint)?
            .header("content-type", "application/json")
            .body(request.to_string());
        tokio::spawn(async move { call.send().await })
    };
    tokio::time::timeout(Duration::from_secs(30), waiting).await?;
    let other = new_id();
    upload(
        &app,
        &format!("{files}/{other}"),
        "audio.wav",
        b"RIFF0000WAVEfixture".to_vec(),
    )
    .await?;
    ensure!(
        status(
            &app,
            &format!("{files}/{other}?transcription=1"),
            json!({"action": "cancel", "id": request["id"]})
        )
        .await?
            == 404,
        "cancel through another file"
    );
    ensure!(
        status(
            &app,
            &endpoint,
            json!({"action": "cancel", "id": request["id"], "consent": true})
        )
        .await?
            == 400,
        "cancel with extra field"
    );
    let cancellation = json!({"action": "cancel", "id": request["id"]});
    let result = app.post(&endpoint, cancellation.clone()).await?;
    ensure!(result["status"] == "cancelled", "cancel: {result}");
    ensure!(
        app.post(&endpoint, cancellation).await? == result,
        "cancel replay"
    );
    let response = pending.await??;
    let body: Value = response.json().await?;
    ensure!(body["status"] == "cancelled", "pending result: {body}");
    tokio::time::timeout(Duration::from_secs(30), closed).await?;
    ensure!(
        app.get(&files).await?.as_array().map(Vec::len) == Some(2),
        "cancelled result published"
    );
    app.restart().await?;
    ensure!(
        app.get(&endpoint).await?["operation"]["status"] == "cancelled",
        "cancel after restart"
    );
    ensure!(
        app.post(&endpoint, request.clone()).await?["status"] == "cancelled",
        "cancelled replay"
    );
    ensure!(
        calls.load(Ordering::SeqCst) == 1,
        "cancelled request replayed"
    );
    ensure!(usage(&app).await?["cancelled"] == 1, "cancelled usage");
    let mut fresh = request.clone();
    fresh["id"] = json!(new_id());
    ensure!(
        status(&app, &endpoint, fresh).await? == 409,
        "new request without retryOf"
    );
    holding.store(false, Ordering::SeqCst);
    let mut retry = request.clone();
    retry["id"] = json!(new_id());
    retry["retryOf"] = request["id"].clone();
    let done = app.post(&endpoint, retry.clone()).await?;
    ensure!(done["status"] == "done", "retry: {done}");
    ensure!(calls.load(Ordering::SeqCst) == 2, "retry calls");
    ensure!(
        status(
            &app,
            &endpoint,
            json!({"action": "cancel", "id": retry["id"]})
        )
        .await?
            == 409,
        "cancel after done"
    );
    let text = app
        .request(
            Method::GET,
            &format!(
                "{files}/{}",
                done["result"]["id"].as_str().unwrap_or_default()
            ),
        )?
        .send()
        .await?
        .text()
        .await?;
    ensure!(text == "New result", "retry output: {text}");
    Ok(())
}
