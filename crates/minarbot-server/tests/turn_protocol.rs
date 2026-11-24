#![allow(dead_code, unused_imports)]

#[path = "support/loopback.rs"]
mod loopback;
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

fn file_call(id: &str, name: &str, text: &str) -> Value {
    json!({
        "id":id,"type":"function",
        "function":{"name":"create_text_file","arguments":json!({"name":name,"text":text}).to_string()}
    })
}

#[tokio::test]
async fn seeded_sse_frames_wake_after_visibility_and_file_lane_commits() -> Result<()> {
    use rusqlite::{params, Connection};

    let root = TestRoot::new()?;
    let app = root.start(json!({})).await?;
    assert!(app.terminate(Signal::TERM).await?.success());
    let agent = id();
    let other = id();
    let turn = id();
    let call = {
        let db = Connection::open(root.database())?;
        for (agent, name) in [(&agent, "Fixture"), (&other, "Other")] {
            db.execute(
                "INSERT INTO agents(id,name,model,instructions) VALUES(?,?,'fixture','Help')",
                params![agent, name],
            )?;
        }
        let created = "2026-01-02T03:04:05.000Z";
        db.execute(
            "INSERT INTO turns(id,agent,text,status,created) VALUES(?,?,'Fixture','interrupted',?)",
            params![turn, agent, created],
        )?;
        db.execute("INSERT INTO provider_calls(turn,agent,model,origin,purpose,status,created) VALUES(?,?,'fixture','test','completion','received',?)", params![turn, agent, created])?;
        let call = db.last_insert_rowid();
        db.execute(
            "INSERT INTO provider_streams VALUES(?,'Saved answer')",
            [call],
        )?;
        db.execute("INSERT INTO provider_stream_segments VALUES(?,0,'text-0','text','Saved answer','',1,3)", [call])?;
        call
    };
    let app = root.start(json!({})).await?;
    let db = Connection::open(root.database())?;
    db.execute("UPDATE turns SET status='running' WHERE id=?", [&turn])?;
    let path = format!("/api/agents/{agent}/turns");
    let stream_path = format!("{path}/stream");
    for (request, status) in [
        (app.get(&format!("{stream_path}?q=x")), 400),
        (
            app.get(&stream_path)
                .header(header::ORIGIN, "https://foreign.example"),
            403,
        ),
        (app.get(&stream_path).bearer_auth("fixture"), 403),
        (app.get(&format!("/api/agents/{}/turns/stream", id())), 404),
    ] {
        assert_eq!(request.send().await?.status(), status);
    }
    let mut stream = TurnStream::connect(&app, &path).await?;
    let expected = json!({"id":turn,"partial":{"seq":call,"text":"Saved answer","model":"fixture","status":"received","segment":"text-0","revision":3,"final":true}});
    let expected = format!("data: {expected}\n\n");
    let mut frame = Vec::new();
    while frame.len() < expected.len() {
        frame.extend_from_slice(
            &stream
                .response
                .chunk()
                .await?
                .ok_or("missing initial SSE frame")?,
        );
    }
    assert_eq!(frame, expected.as_bytes());
    let mut isolated = TurnStream::connect(&app, &format!("/api/agents/{other}/turns")).await?;
    let quiet = Duration::from_millis(80);
    assert!(timeout(quiet, isolated.next()).await.is_err());
    let update = |text: &str, revision| -> Result<()> {
        db.execute(
            "UPDATE provider_streams SET text=? WHERE call=?",
            params![text, call],
        )?;
        db.execute(
            "UPDATE provider_stream_segments SET text=?,revision=? WHERE call=?",
            params![text, revision, call],
        )?;
        Ok(())
    };
    update("Latest answer", 4)?;
    post_json(
        &app,
        &format!("/api/agents/{agent}/visibility"),
        &json!({"id":id(),"revision":0,"hidden":false}),
        200,
    )
    .await?;
    assert_eq!(stream.snapshot().await?["partial"]["text"], "Latest answer");
    let writes = [
        app.post(&format!("/api/agents/{agent}/files/{}", id())).header(header::CONTENT_TYPE, "application/octet-stream").header("X-File-Name", "fixture.txt").body("file change"),
        app.post(&format!("/api/agents/{agent}/sites")).json(&json!({"id":id(),"name":"Fixture","source":"<p>Fixture</p>"})),
        app.post("/api/skills").json(&json!({"id":id(),"revision":0,"name":"Fixture","description":"Fixture","content":"Fixture"})),
    ];
    for (index, request) in writes.into_iter().enumerate() {
        let revision = index + 5;
        let text = format!("File lane commit {revision}");
        update(&text, revision)?;
        assert!(timeout(quiet, stream.next()).await.is_err());
        assert_eq!(request.send().await?.status(), 200);
        assert_eq!(stream.snapshot().await?["partial"]["text"], text);
    }
    let mut repaired = TurnStream::connect(&app, &path).await?;
    assert_eq!(repaired.snapshot().await?["partial"]["revision"], 7);
    assert!(app.terminate(Signal::TERM).await?.success());
    assert!(stream.next().await?.is_none());
    assert!(isolated.next().await?.is_none());
    assert!(repaired.next().await?.is_none());
    Ok(())
}
