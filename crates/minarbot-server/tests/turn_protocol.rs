#[path = "support/loopback.rs"]
mod loopback;
#[path = "turn_protocol/provider_stream.rs"]
mod provider_stream;
#[path = "support/server.rs"]
mod server;

use axum::{
    body::{Body, Bytes},
    extract::Json,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use futures_util::{Stream, StreamExt};
use loopback::Loopback;
use rustix::process::Signal;
use serde_json::{json, Value};
use server::{Result, Server, TestRoot, WAIT};
use std::{
    convert::Infallible,
    fmt::Display,
    io::ErrorKind,
    net::SocketAddr,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    net::TcpStream,
    sync::Notify,
    time::{sleep, timeout},
};
use uuid::Uuid;

#[derive(Clone, Default)]
struct Calls(Arc<Mutex<Vec<Value>>>);

impl Calls {
    fn record(&self, request: &Value) {
        if let Ok(mut calls) = self.0.lock() {
            calls.push(request.clone());
        }
    }

    fn read(&self) -> Result<Vec<Value>> {
        Ok(self
            .0
            .lock()
            .map_err(|_| "provider request log poisoned")?
            .clone())
    }
}

fn id() -> String {
    Uuid::new_v4().to_string()
}

fn messages(body: &Value) -> &[Value] {
    body["messages"].as_array().map_or(&[], Vec::as_slice)
}

fn event(data: impl Display) -> Bytes {
    Bytes::from(format!("data: {data}\n\n"))
}

fn delta(delta: Value, finish: Value) -> Bytes {
    event(json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}]}))
}

fn text_delta(text: &str) -> Bytes {
    delta(json!({"content":text}), Value::Null)
}

fn upstream_sse(events: impl Stream<Item = Bytes> + Send + 'static) -> Response {
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        Body::from_stream(events.map(Ok::<_, Infallible>)),
    )
        .into_response()
}

fn completion(message: Value, reason: &str) -> Json<Value> {
    Json(json!({"choices":[{"message":message,"finish_reason":reason}]}))
}

async fn json_response(response: reqwest::Response, status: u16) -> Result<Value> {
    let actual = response.status().as_u16();
    let url = response.url().clone();
    let body = response.json::<Value>().await?;
    assert_eq!(actual, status, "{url}: {body}");
    Ok(body)
}

async fn get_json(app: &Server<'_>, path: &str) -> Result<Value> {
    json_response(app.get(path).send().await?, 200).await
}

async fn post_json(app: &Server<'_>, path: &str, body: &Value, status: u16) -> Result<Value> {
    json_response(app.post(path).json(body).send().await?, status).await
}

async fn agent(app: &Server<'_>, name: &str, model: &str) -> Result<String> {
    let agent = id();
    let body = json!({"id":agent,"name":name,"model":model,"runtime":"api","instructions":"Help"});
    let profile = post_json(app, "/api/agents", &body, 201).await?;
    assert_eq!(profile["id"], agent);
    Ok(agent)
}

async fn channel(app: &Server<'_>, members: &[&str], lead: Option<&str>) -> Result<String> {
    let room = id();
    let body = json!({"id":room,"name":"Fixture room","instructions":"Help","members":members,"lead":lead});
    let profile = post_json(app, "/api/channels", &body, 201).await?;
    assert_eq!(profile["id"], room);
    Ok(room)
}

async fn submit(app: &Server<'_>, path: &str, text: &str) -> Result<String> {
    let turn = id();
    let accepted = post_json(app, path, &json!({"id":turn,"text":text}), 202).await?;
    assert_eq!(accepted["id"], turn);
    Ok(turn)
}

fn rows(body: &Value) -> Result<&[Value]> {
    body.as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| "expected JSON array".into())
}

async fn wait_turn(
    app: &Server<'_>,
    path: &str,
    id: &str,
    ready: impl Fn(&Value) -> bool,
) -> Result<Value> {
    let mut last = Value::Null;
    timeout(WAIT, async {
        loop {
            let history = get_json(app, path).await?;
            if let Some(turn) = rows(&history)?.iter().find(|turn| turn["id"] == id) {
                if ready(turn) {
                    return Ok(turn.clone());
                }
                last = turn.clone();
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| format!("turn {id} did not settle at {path}: {last}"))?
}

async fn wait_status(app: &Server<'_>, path: &str, id: &str, status: &str) -> Result<Value> {
    wait_turn(app, path, id, |row| row["status"] == status).await
}

struct TurnStream {
    response: reqwest::Response,
    pending: Vec<u8>,
}

impl TurnStream {
    async fn connect(app: &Server<'_>, path: &str) -> Result<Self> {
        let response = app
            .get(&format!("{path}/stream"))
            .header(header::ORIGIN, &app.origin)
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "no-cache, no-transform"
        );
        assert_eq!(response.headers()["x-accel-buffering"], "no");
        Ok(Self {
            response,
            pending: Vec::new(),
        })
    }

    async fn next(&mut self) -> Result<Option<Value>> {
        loop {
            if let Some(end) = self.pending.windows(2).position(|line| line == b"\n\n") {
                let frame = &self.pending[..end];
                let data = frame
                    .strip_prefix(b"data: ")
                    .map(serde_json::from_slice)
                    .transpose()?;
                self.pending.drain(..end + 2);
                if data.is_some() {
                    return Ok(data);
                }
            } else if let Some(chunk) = self.response.chunk().await? {
                self.pending.extend_from_slice(&chunk);
            } else if self.pending.is_empty() {
                return Ok(None);
            } else {
                return Err("SSE closed with an incomplete frame".into());
            }
        }
    }

    async fn snapshot(&mut self) -> Result<Value> {
        self.next()
            .await?
            .ok_or_else(|| "SSE closed before snapshot".into())
    }

    async fn wait_text(&mut self, turn: &str, text: &str) -> Result<Value> {
        loop {
            let frame = self.snapshot().await?;
            if frame["id"] == turn && frame["partial"]["text"] == text {
                return Ok(frame);
            }
        }
    }
}

#[tokio::test]
async fn fixture_reaps_server_and_releases_private_root_and_ports() -> Result<()> {
    let root = TestRoot::new()?;
    let directory = root.path().to_owned();
    assert_eq!(directory.metadata()?.permissions().mode() & 0o777, 0o700);
    let failed = root
        .start(json!({"streamTermination":"invalid"}))
        .await
        .err()
        .ok_or("invalid config started")?;
    assert!(failed.to_string().contains("MINARBOT_STREAM_TERMINATION"));
    assert!(!root.database().exists());

    let provider = Loopback::start(Router::new()).await?;
    let address = provider.address;
    let app = root.start(json!({})).await?;
    assert!(root.database().is_file());
    let server_address = app
        .origin
        .strip_prefix("http://")
        .ok_or("invalid fixture origin")?;
    let server_address: SocketAddr = server_address.parse()?;
    drop(app);
    drop(root);
    drop(provider);
    assert!(!directory.exists());
    for address in [server_address, address] {
        timeout(WAIT, async {
            loop {
                match TcpStream::connect(address).await {
                    Err(error) if error.kind() == ErrorKind::ConnectionRefused => return Ok(()),
                    Err(error) => return Err(error),
                    Ok(_) => sleep(Duration::from_millis(20)).await,
                }
            }
        })
        .await??;
    }
    Ok(())
}

#[tokio::test]
async fn admission_and_concurrent_exact_retries_keep_fifo_and_scope() -> Result<()> {
    let calls = Calls::default();
    let release = Arc::new(Notify::new());
    let provider = Loopback::start(Router::new().route(
        "/v1/chat/completions",
        post({
            let calls = calls.clone();
            let release = release.clone();
            move |Json(body): Json<Value>| {
                let calls = calls.clone();
                let release = release.clone();
                async move {
                    calls.record(&body);
                    let messages = messages(&body);
                    let text = messages
                        .last()
                        .and_then(|m| m["content"].as_str())
                        .unwrap_or_default();
                    if text == "first" {
                        release.notified().await;
                    }
                    if text == "fail" {
                        return (StatusCode::TOO_MANY_REQUESTS, "private upstream detail")
                            .into_response();
                    }
                    completion(
                        json!({"content":format!("reply:{text};history:{}", messages.len())}),
                        "stop",
                    )
                    .into_response()
                }
            }
        }),
    ))
    .await?;
    let root = TestRoot::new()?;
    let config = json!({"baseURL":provider.base_url()});
    let app = root.start(config.clone()).await?;
    let owner = agent(&app, "Owner", "fixture").await?;
    let other = agent(&app, "Other", "fixture").await?;
    let path = format!("/api/agents/{owner}/turns");
    let foreign = format!("/api/agents/{other}/turns");
    let first = id();
    let command = json!({"id":first,"text":"first"});

    post_json(&app, &path, &json!({"id":"bad","text":"bad"}), 400).await?;
    let denied = app
        .client
        .post(format!("{}{path}", app.origin))
        .header(header::ORIGIN, "https://foreign.example")
        .json(&command)
        .send()
        .await?;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert_eq!(get_json(&app, &path).await?, json!([]));
    assert!(calls.read()?.is_empty());

    let (one, retry) = tokio::try_join!(
        post_json(&app, &path, &command, 202),
        post_json(&app, &path, &command, 202),
    )?;
    for receipt in [one, retry] {
        assert_eq!(receipt["id"], first);
        assert_eq!(receipt["text"], "first");
        assert_eq!(receipt["agent"], owner);
    }
    post_json(&app, &path, &json!({"id":first,"text":"changed"}), 409).await?;
    post_json(&app, &foreign, &command, 409).await?;
    let second = submit(&app, &path, "second").await?;
    assert_eq!(
        wait_status(&app, &path, &second, "queued").await?["reply"],
        Value::Null
    );
    release.notify_one();
    let done = wait_status(&app, &path, &second, "done").await?;
    assert_eq!(done["reply"], "reply:second;history:4");
    let completed_history = get_json(&app, &path).await?;
    let turns = rows(&completed_history)?;
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0]["id"], first);
    assert_eq!(turns[0]["reply"], "reply:first;history:2");
    assert_eq!(turns[1]["id"], second);
    assert_eq!(get_json(&app, &foreign).await?, json!([]));
    let observed = calls.read()?;
    let prompts: Vec<_> = observed
        .iter()
        .filter_map(|body| messages(body).last()?.get("content")?.as_str())
        .collect();
    assert_eq!(prompts, ["first", "second"]);

    let failed = submit(&app, &path, "fail").await?;
    let failure = wait_status(&app, &path, &failed, "failed").await?;
    let error = failure["error"]
        .as_str()
        .ok_or("missing provider failure")?;
    assert!(error.contains("HTTP 429"));
    assert!(!error.contains("private"));
    let history = get_json(&app, &path).await?;

    assert!(app.terminate(Signal::TERM).await?.success());
    let app = root.start(json!({})).await?;
    let replay = post_json(&app, &path, &command, 202).await?;
    assert_eq!(replay["status"], "done");
    assert_eq!(replay["reply"], "reply:first;history:2");
    post_json(&app, &path, &json!({"id":id(),"text":"no provider"}), 503).await?;
    assert_eq!(get_json(&app, &path).await?, history);
    assert_eq!(calls.read()?.len(), 3);
    Ok(())
}

#[tokio::test]
async fn sse_delivers_ordered_persisted_snapshots_and_reconnects_without_replay() -> Result<()> {
    let calls = Calls::default();
    let second_packet = Arc::new(Notify::new());
    let missed_packet = Arc::new(Notify::new());
    let finish = Arc::new(Notify::new());
    let provider = Loopback::start(Router::new().route(
        "/v1/chat/completions",
        post({
            let calls = calls.clone();
            let second_packet = second_packet.clone();
            let missed_packet = missed_packet.clone();
            let finish = finish.clone();
            move |Json(body): Json<Value>| {
                calls.record(&body);
                let second_packet = second_packet.clone();
                let missed_packet = missed_packet.clone();
                let finish = finish.clone();
                async move {
                    upstream_sse(async_stream::stream! {
                        yield text_delta("First café.\n\n");
                        second_packet.notified().await;
                        yield text_delta("Second");
                        missed_packet.notified().await;
                        yield text_delta(" block");
                        finish.notified().await;
                        yield text_delta(" complete.");
                        yield delta(json!({}), json!("stop"));
                        yield event("[DONE]");
                    })
                }
            }
        }),
    ))
    .await?;
    let root = TestRoot::new()?;
    let app = root
        .start(json!({"baseURL":provider.base_url(),"stream":true}))
        .await?;
    let owner = agent(&app, "Stream", "fixture").await?;
    let other = agent(&app, "Other", "fixture").await?;
    let thread = id();
    post_json(
        &app,
        &format!("/api/agents/{owner}/threads"),
        &json!({"id":thread,"name":"Side thread"}),
        200,
    )
    .await?;
    let room = channel(&app, &[&owner, &other], None).await?;
    let path = format!("/api/agents/{owner}/turns");
    let mut stream = TurnStream::connect(&app, &path).await?;

    let turn = submit(&app, &path, "Stream").await?;
    let admitted = stream.snapshot().await?;
    assert_eq!(admitted["id"], turn);
    assert_eq!(admitted["partial"]["text"], "");
    assert_eq!(admitted["partial"]["revision"], 0);
    let first = stream.snapshot().await?;
    assert_eq!(first["id"], turn);
    assert_eq!(first["partial"]["text"], "First café.\n\n");
    second_packet.notify_one();
    let second = stream.snapshot().await?;
    assert_eq!(second["id"], turn);
    assert_eq!(second["partial"]["text"], "First café.\n\nSecond");
    assert_eq!(second["partial"]["seq"], first["partial"]["seq"]);
    drop(stream);

    missed_packet.notify_one();
    let latest = wait_turn(&app, &path, &turn, |row| {
        row["partial"]["text"] == "First café.\n\nSecond block"
    })
    .await?;
    let mut reconnected = TurnStream::connect(&app, &path).await?;
    let snapshot = reconnected.snapshot().await?;
    assert_eq!(snapshot, json!({"id":turn,"partial":latest["partial"]}));
    assert_eq!(snapshot["partial"]["seq"], first["partial"]["seq"]);
    let before = second["partial"]["revision"]
        .as_u64()
        .ok_or("missing second revision")?;
    assert_eq!(snapshot["partial"]["revision"], before + 1);

    for scope in [
        format!("/api/agents/{other}/turns"),
        format!("/api/threads/{thread}/turns"),
        format!("/api/channels/{room}/turns"),
    ] {
        let mut isolated = TurnStream::connect(&app, &scope).await?;
        assert!(
            timeout(Duration::from_millis(80), isolated.next())
                .await
                .is_err(),
            "unexpected snapshot in {scope}"
        );
    }
    let stream_path = format!("{path}/stream");
    for (response, status) in [
        (app.get(&format!("{stream_path}?q=x")).send().await?, 400),
        (
            app.get(&stream_path)
                .header(header::ORIGIN, "https://foreign.example")
                .send()
                .await?,
            403,
        ),
        (
            app.get(&stream_path).bearer_auth("fixture").send().await?,
            403,
        ),
        (app.post(&stream_path).send().await?, 405),
    ] {
        assert_eq!(response.status().as_u16(), status);
    }

    finish.notify_one();
    let final_frame = loop {
        let frame = reconnected.snapshot().await?;
        assert_eq!(frame["id"], turn);
        if frame["partial"]["final"] == true {
            break frame;
        }
    };
    assert_eq!(
        final_frame["partial"]["text"],
        "First café.\n\nSecond block complete."
    );
    let done = wait_status(&app, &path, &turn, "done").await?;
    assert_eq!(done["reply"], final_frame["partial"]["text"]);
    assert_eq!(calls.read()?.len(), 1);
    assert!(app.terminate(Signal::TERM).await?.success());
    timeout(WAIT, async {
        while let Some(frame) = reconnected.next().await? {
            assert_eq!(frame["id"], turn);
        }
        Result::Ok(())
    })
    .await??;
    Ok(())
}

fn file_call(id: &str, name: &str, text: &str) -> Value {
    json!({
        "id":id,"type":"function",
        "function":{"name":"create_text_file","arguments":json!({"name":name,"text":text}).to_string()}
    })
}

#[tokio::test]
async fn room_stop_seals_gated_partial_and_effects_across_lost_response_and_restart() -> Result<()>
{
    let calls = Calls::default();
    let run_tool = Arc::new(Notify::new());
    let late_final = Arc::new(Notify::new());
    let provider = Loopback::start(Router::new().route(
        "/v1/chat/completions",
        post({
            let calls = calls.clone();
            let run_tool = run_tool.clone();
            let late_final = late_final.clone();
            move |Json(body): Json<Value>| {
                calls.record(&body);
                let run_tool = run_tool.clone();
                let late_final = late_final.clone();
                async move {
                    upstream_sse(async_stream::stream! {
                        let last = messages(&body).last().cloned().unwrap_or_default();
                        if last["content"] == "New generation" {
                            yield text_delta("Restart finished");
                        } else if last["role"] == "tool" {
                            yield text_delta("Partial answer before Stop");
                            late_final.notified().await;
                            yield text_delta(" late result");
                        } else {
                            yield text_delta("Before tool.");
                            run_tool.notified().await;
                            let mut call = file_call("effect", "effect.txt", "Do not repeat");
                            call["index"] = json!(0);
                            yield delta(json!({"tool_calls":[call]}), Value::Null);
                            yield delta(json!({}), json!("tool_calls"));
                            yield event("[DONE]");
                            return;
                        }
                        yield delta(json!({}), json!("stop"));
                        yield event("[DONE]");
                    })
                }
            }
        }),
    ))
    .await?;
    let root = TestRoot::new()?;
    let config = json!({"baseURL":provider.base_url(),"stream":true});
    let app = root.start(config.clone()).await?;
    let owner = agent(&app, "Room owner", "fixture").await?;
    let room = channel(&app, &[&owner], Some(&owner)).await?;
    let path = format!("/api/channels/{room}/turns");
    let mut stream = TurnStream::connect(&app, &path).await?;
    let turn = submit(&app, &path, "Stop after tool").await?;
    let before = stream.wait_text(&turn, "Before tool.").await?;
    run_tool.notify_one();
    let draft = stream
        .wait_text(&turn, "Partial answer before Stop")
        .await?;
    let partial = wait_turn(&app, &path, &turn, |row| {
        row["partial"]["text"] == "Partial answer before Stop"
    })
    .await?;
    assert_eq!(partial["generatedFiles"].as_array().map(Vec::len), Some(1));
    let segments: Vec<_> = rows(&partial["activity"]["stream"])?
        .iter()
        .map(|item| {
            json!([
                item["seq"],
                item["ordinal"],
                item["kind"],
                item["text"],
                item["final"]
            ])
        })
        .collect();
    assert_eq!(
        segments,
        [
            json!([before["partial"]["seq"], 0, "text", "Before tool.", 0]),
            json!([before["partial"]["seq"], 1, "tool", "create_text_file", 0]),
            json!([
                draft["partial"]["seq"],
                0,
                "text",
                "Partial answer before Stop",
                0
            ]),
        ]
    );

    let stop_path = format!("/api/channels/{room}/discussions/{turn}/stop");
    let stop = json!({"id":id(),"generation":turn});
    // Leave the response unread until a separate connection observes the commit.
    let address = app
        .origin
        .strip_prefix("http://")
        .ok_or("invalid fixture origin")?;
    let body = stop.to_string();
    let mut lost = timeout(WAIT, TcpStream::connect(address)).await??;
    timeout(WAIT, lost.write_all(format!(
        "POST {stop_path} HTTP/1.1\r\nHost: {address}\r\nOrigin: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        app.origin, body.len()
    ).as_bytes())).await??;
    wait_status(&app, &path, &turn, "cancelled").await?;
    drop(lost);
    let receipt = post_json(&app, &stop_path, &stop, 200).await?;
    assert_eq!(
        receipt,
        json!({"root":turn,"generation":turn,"stopped":true,"cancelled":[turn]})
    );
    assert_eq!(post_json(&app, &stop_path, &stop, 200).await?, receipt);
    let mut conflict = stop.clone();
    conflict["generation"] = json!(id());
    post_json(&app, &stop_path, &conflict, 409).await?;

    late_final.notify_one();
    let next = submit(&app, &path, "New generation").await?;
    let next = wait_status(&app, &path, &next, "done").await?;
    assert_eq!(next["reply"], "Restart finished");
    let stopped = wait_status(&app, &path, &turn, "cancelled").await?;
    assert_eq!(stopped["partial"]["text"], partial["partial"]["text"]);
    assert_eq!(stopped["reply"], Value::Null);
    assert_eq!(stopped["generatedFiles"], partial["generatedFiles"]);
    assert_eq!(stopped["room"]["messages"], json!([]));
    let count = calls.read()?.len();
    assert!(app.terminate(Signal::TERM).await?.success());

    let app = root.start(config).await?;
    assert_eq!(post_json(&app, &stop_path, &stop, 200).await?, receipt);
    let recovered = wait_status(&app, &path, &turn, "cancelled").await?;
    assert_eq!(recovered["partial"], stopped["partial"]);
    assert_eq!(recovered["generatedFiles"], stopped["generatedFiles"]);
    assert_eq!(calls.read()?.len(), count);
    Ok(())
}

#[tokio::test]
async fn queued_dispatch_replays_its_receipt_and_steers_once_at_the_safe_point() -> Result<()> {
    let calls = Calls::default();
    let safe_point = Arc::new(Notify::new());
    let provider = Loopback::start(Router::new().route("/v1/chat/completions", post({
        let calls = calls.clone();
        let safe_point = safe_point.clone();
        move |Json(body): Json<Value>| {
            calls.record(&body);
            let safe_point = safe_point.clone();
            async move {
                let results = messages(&body).iter().filter(|message| message["role"] == "tool").count();
                match results {
                    0 => {
                        safe_point.notified().await;
                        completion(json!({"tool_calls":[file_call("save","saved.txt","Saved once")]}), "tool_calls")
                    }
                    1 => completion(json!({"tool_calls":[{"id":"inspect","type":"function","function":{"name":"read_provider_settings","arguments":"{}"}}]}), "tool_calls"),
                    _ => completion(json!({"content":"Steered loop finished"}), "stop"),
                }
            }
        }
    }))).await?;

    let root = TestRoot::new()?;
    let config = json!({"baseURL":provider.base_url()});
    let app = root.start(config.clone()).await?;
    let owner = agent(&app, "Steer", "fixture").await?;
    let path = format!("/api/agents/{owner}/turns");
    let active = submit(&app, &path, "Work with tools").await?;
    wait_status(&app, &path, &active, "running").await?;
    let queued = submit(&app, &path, "Promoted guidance").await?;
    let endpoint = format!("{path}/{queued}/dispatch");
    let command = json!({"id":id(),"revision":1,"mode":"steer","targetTurn":active});

    let promoted = post_json(&app, &endpoint, &command, 200).await?;
    assert_eq!(promoted["id"], queued);
    assert_eq!(promoted["dispatch_mode"], "steer");
    assert_eq!(post_json(&app, &endpoint, &command, 200).await?, promoted);
    let mut changed = command.clone();
    changed["mode"] = json!("restart");
    post_json(&app, &endpoint, &changed, 409).await?;

    safe_point.notify_one();
    let done = wait_status(&app, &path, &active, "done").await?;
    assert_eq!(done["reply"], "Steered loop finished");
    assert_eq!(done["generatedFiles"].as_array().map(Vec::len), Some(1));
    wait_status(&app, &path, &queued, "done").await?;
    assert_eq!(rows(&get_json(&app, &path).await?)?.len(), 2);
    let observed = calls.read()?;
    assert_eq!(observed.len(), 4, "two tools, response, and verification");
    for body in observed.iter().skip(1) {
        assert_eq!(
            messages(body)
                .iter()
                .filter(|message| message["role"] == "user"
                    && message["content"] == "Promoted guidance")
                .count(),
            1
        );
    }

    assert!(app.terminate(Signal::TERM).await?.success());
    let app = root.start(config).await?;
    assert_eq!(post_json(&app, &endpoint, &command, 200).await?, promoted);
    assert_eq!(rows(&get_json(&app, &path).await?)?.len(), 2);
    assert_eq!(calls.read()?.len(), observed.len());
    Ok(())
}
