use super::*;
use axum::{
    body::{to_bytes, Body},
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Request, State,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
};
use tokio::{
    net::TcpListener,
    sync::broadcast,
    task::JoinHandle,
    time::{sleep, timeout, Instant},
};

const TOKEN: &str = "fixture.discord.synthetic-bot-token";
const BOT: &str = "9001";
const REQUESTER: &str = "9002";
const OTHER: &str = "9003";
const AGENT: &str = "b1715307-0000-4000-8000-000000000014";
const PROJECT: &str = "b1715307-0000-4000-8000-000000000015";
const SECTION: &str = "b1715307-0000-4000-8000-000000000016";
const FLOW: &str = "b1715307-0000-4000-8000-000000000017";

type TestResult<T> = crate::db::Result<T>;
fn check<T>(result: Result<T>) -> TestResult<T> {
    result.map_err(|failure| failure.message.into())
}

#[derive(Clone)]
struct Sent {
    channel: String,
    id: String,
    body: Value,
    multipart: String,
    at: Instant,
}

struct Remote {
    origin: String,
    commands: broadcast::Sender<Value>,
    sessions: Mutex<Vec<Value>>,
    sent: Mutex<Vec<Sent>>,
    callbacks: Mutex<Vec<Value>>,
    prompts: Mutex<Vec<Value>>,
    sequence: AtomicU64,
    message: AtomicU64,
    heartbeats: AtomicU64,
    partial: AtomicU64,
    limited: AtomicBool,
}

async fn gateway_peer(mut socket: WebSocket, remote: Arc<Remote>) {
    async fn send(socket: &mut WebSocket, value: Value) -> bool {
        socket
            .send(Message::Text(value.to_string().into()))
            .await
            .is_ok()
    }
    let mut events = remote.commands.subscribe();
    if !send(
        &mut socket,
        json!({"op":10,"d":{"heartbeat_interval":2000}}),
    )
    .await
    {
        return;
    }
    let Some(Ok(Message::Text(first))) = socket.recv().await else {
        return;
    };
    let Ok(mut first) = serde_json::from_str::<Value>(&first) else {
        return;
    };
    assert!(first["d"]["token"] == TOKEN);
    if let Some(data) = first["d"].as_object_mut() {
        data.remove("token");
    }
    let op = first["op"].clone();
    if let Ok(mut sessions) = remote.sessions.lock() {
        sessions.push(first);
    }
    if op == 2 {
        let uri = remote.origin.replace("http://", "ws://") + "/gateway";
        if !send(&mut socket,json!({"op":0,"s":1,"t":"READY","d":{"session_id":"fixture-session","resume_gateway_url":uri,"user":{"id":BOT}}})).await { return; }
    } else if !send(
        &mut socket,
        json!({"op":0,"s":remote.sequence.fetch_add(1,Ordering::SeqCst),"t":"RESUMED","d":{}}),
    )
    .await
    {
        return;
    }
    loop {
        tokio::select! {
            request = socket.recv() => match request {
                Some(Ok(Message::Text(text))) if serde_json::from_str::<Value>(&text).is_ok_and(|v| v["op"]==1) => {
                    remote.heartbeats.fetch_add(1,Ordering::SeqCst);
                    if !send(&mut socket,json!({"op":11,"d":null})).await { break; }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => (),
            },
            event = events.recv() => {
                let Ok(event) = event else {break};
                let reconnect = event["op"]==7;
                if !send(&mut socket,event).await || reconnect {break;}
            }
        }
    }
}

async fn messages(
    State(remote): State<Arc<Remote>>,
    Path(channel): Path<String>,
    request: Request,
) -> impl IntoResponse {
    assert!(request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.strip_prefix("Bot ") == Some(TOKEN)));
    let bytes = to_bytes(request.into_body(), MAX_FILE + 8192).await;
    let body = bytes
        .as_ref()
        .ok()
        .and_then(|bytes| serde_json::from_slice(bytes).ok())
        .unwrap_or(Value::Null);
    let multipart = if body.is_null() {
        bytes
            .map(|v| String::from_utf8_lossy(&v).into_owned())
            .unwrap_or_default()
    } else {
        String::new()
    };
    let content = string(&body, "content");
    let id = remote.message.fetch_add(1, Ordering::SeqCst).to_string();
    if let Ok(mut sent) = remote.sent.lock() {
        sent.push(Sent {
            channel,
            id: id.clone(),
            body: body.clone(),
            multipart,
            at: Instant::now(),
        });
    }
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "x-ratelimit-bucket",
        axum::http::HeaderValue::from_static("messages-fixture"),
    );
    let (status, value) =
        if content.starts_with("Rate check") && !remote.limited.swap(true, Ordering::SeqCst) {
            headers.insert(
                "x-ratelimit-global",
                axum::http::HeaderValue::from_static("true"),
            );
            headers.insert("retry-after", axum::http::HeaderValue::from_static("0.55"));
            (
                StatusCode::TOO_MANY_REQUESTS,
                json!({"retry_after":0.55,"global":true}),
            )
        } else if content == "TAIL" {
            remote.partial.fetch_add(1, Ordering::SeqCst);
            (StatusCode::BAD_GATEWAY, json!({}))
        } else {
            if content == "Bucket first" {
                headers.insert(
                    "x-ratelimit-remaining",
                    axum::http::HeaderValue::from_static("0"),
                );
                headers.insert(
                    "x-ratelimit-reset-after",
                    axum::http::HeaderValue::from_static("0.55"),
                );
            }
            (StatusCode::OK, json!({"id":id}))
        };
    (status, headers, axum::Json(value))
}

struct Fixture {
    state: AppState,
    remote: Arc<Remote>,
    root: PathBuf,
    tasks: Vec<JoinHandle<()>>,
}

impl Fixture {
    async fn new() -> TestResult<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let remote = Arc::new(Remote {
            origin: origin.clone(),
            commands: broadcast::channel(64).0,
            sessions: Mutex::default(),
            sent: Mutex::default(),
            callbacks: Mutex::default(),
            prompts: Mutex::default(),
            sequence: AtomicU64::new(2),
            message: AtomicU64::new(10000),
            heartbeats: AtomicU64::new(0),
            partial: AtomicU64::new(0),
            limited: AtomicBool::new(false),
        });
        let router = Router::new()
            .route("/gateway",get(|ws:WebSocketUpgrade,State(remote): State<Arc<Remote>>| async move {
                ws.on_upgrade(move |socket| gateway_peer(socket,remote))
            }))
            .route("/api/v10/users/@me",get(|request: Request| async move {
                if request.headers().get("authorization").and_then(|v|v.to_str().ok()).is_some_and(|v|v.strip_prefix("Bot ")==Some(TOKEN)) {
                    crate::json_response(200,json!({"id":BOT,"bot":true}))
                } else { crate::json_response(401,json!({"message":TOKEN})) }
            }))
            .route("/api/v10/channels/{channel}/messages",post(messages))
            .route("/api/v10/interactions/{id}/{token}/callback",post(|State(remote):State<Arc<Remote>>,axum::Json(body):axum::Json<Value>| async move {
                if let Ok(mut callbacks) = remote.callbacks.lock() {callbacks.push(body);}
                StatusCode::NO_CONTENT
            }))
            .route("/attachments/123/789/text.txt",get(|request:Request|async move {
                assert!(!request.headers().contains_key("authorization"));
                "Exact inbound attachment"
            }))
            .route("/v1/chat/completions",post(|State(remote):State<Arc<Remote>>, axum::Json(body):axum::Json<Value>| async move {
                let raw=body.to_string();
                assert!(!raw.contains(TOKEN));
                if let Ok(mut prompts) = remote.prompts.lock() { prompts.push(body); }
                if raw.contains("Exact inbound attachment") && !raw.contains("outbound.txt") {
                    return axum::Json(json!({"choices":[{"finish_reason":"tool_calls","message":{"tool_calls":[{"id":"make-file","type":"function","function":{"name":"create_text_file","arguments":json!({"name":"outbound.txt","text":"Exact outbound file"}).to_string()}}]}}]}));
                }
                axum::Json(json!({"choices":[{"finish_reason":"stop","message":{"content":"Fixture agent reply"}}]}))
            }))
            .with_state(remote.clone());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        let root = std::env::temp_dir().join(format!("minarbot-discord-{}", runner::new_id()));
        let mut config: crate::config::Config = serde_json::from_value(json!({
            "database":root.join("state.sqlite"),"discordTestOrigin":origin,
            "baseURL":format!("{origin}/v1"),"defaultModel":"fixture","apiKey":"fake-provider-key",
        }))?;
        config.data.validate()?;
        let database = crate::db::open(&config.database, false, config.now())?;
        crate::files::init(&database.connection)?;
        crate::data::initialize(&database)?;
        check(crate::turns::initialize(&database.connection))?;
        check(crate::connectors::initialize(&database.connection))?;
        check(crate::automation::initialize(&database.connection))?;
        check(crate::harness::initialize(&database.connection))?;
        check(crate::mcp::initialize(&database.connection))?;
        crate::computer::initialize(&database.connection)?;
        database.connection.execute("INSERT INTO agents(id,name,model,instructions,provider_instance,model_options) VALUES(?,'Fixture','fixture','Reply to messages','codex','{}')",[AGENT])?;
        database.connection.execute(
            "INSERT INTO channels(id,name,instructions,lead) VALUES(?,'General','',?)",
            [SECTION, AGENT],
        )?;
        database.connection.execute("INSERT INTO local_projects(id,name,description,created,updated,channel) VALUES(?,'Fixture project','',1,1,?)",[PROJECT,SECTION])?;
        database.connection.execute("INSERT INTO local_project_sections(project,channel,name,position) VALUES(?,?,'General',0)",[PROJECT,SECTION])?;
        database
            .connection
            .execute("INSERT INTO channel_members VALUES(?,?)", [SECTION, AGENT])?;
        let events = crate::events::Hub::new();
        events.watch(&database.connection);
        let state = AppState {
            whatsapp: Default::default(),
            database: Arc::new(Mutex::new(database)),
            computer: Arc::new(crate::computer::Manager::new(&config)?),
            config: Arc::new(config),
            origin,
            auth: None,
            events,
            turns: crate::turns::Runtime::default(),
            trial_pending: Arc::new(AtomicBool::new(false)),
        };
        let turns = check(crate::turns::start(state.clone()))?;
        let automation = crate::automation::start(state.clone());
        let discord = start(state.clone());
        Ok(Self {
            state,
            remote,
            root,
            tasks: vec![task, turns, automation, discord],
        })
    }

    async fn settings(
        &self,
        action: &str,
        body: Option<Value>,
        expected: u16,
    ) -> TestResult<Value> {
        let mut req = Request::builder()
            .uri(format!("{SETTINGS}{action}"))
            .header("host", self.remote.origin.trim_start_matches("http://"))
            .header("origin", &self.remote.origin);
        let body = if let Some(body) = body {
            req = req
                .method(Method::POST)
                .header("content-type", "application/json");
            Body::from(body.to_string())
        } else {
            Body::empty()
        };
        let response = crate::connectors::entry(State(self.state.clone()), req.body(body)?).await;
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), 64_000).await?;
        let value: Value = serde_json::from_slice(&bytes)?;
        assert!(!value.to_string().contains(TOKEN));
        assert_eq!(status, expected, "{action}: {value}");
        Ok(value)
    }

    async fn connect(&self) -> TestResult<String> {
        let generation = runner::new_id();
        self.settings(
            "/connect",
            Some(json!({"id":generation,"generation":"","token":"wrong.fixture.token-value"})),
            401,
        )
        .await?;
        self.settings(
            "/connect",
            Some(json!({"id":generation,"generation":"","token":TOKEN})),
            200,
        )
        .await?;
        let guard = check(lock(&self.state))?;
        let ciphertext: String =
            guard
                .connection
                .query_row("SELECT ciphertext FROM discord_bot", [], |r| r.get(0))?;
        assert!(!ciphertext.contains(TOKEN));
        Ok(generation)
    }

    async fn route(
        &self,
        generation: &str,
        channel: &str,
        kind: &str,
        target: &str,
    ) -> TestResult<()> {
        self.settings(
            "/add-route",
            Some(json!({"id":runner::new_id(),"generation":generation,
            "channel":channel,"kind":kind,"target":target,"users":[REQUESTER,OTHER]})),
            200,
        )
        .await?;
        Ok(())
    }

    fn event(&self, kind: &str, data: Value) -> TestResult<u64> {
        let s = self.remote.sequence.fetch_add(1, Ordering::SeqCst);
        self.remote
            .commands
            .send(json!({"op":0,"s":s,"t":kind,"d":data}))
            .map_err(|_| "Gateway is unavailable")?;
        Ok(s)
    }

    fn snapshot(&self) -> Vec<Sent> {
        self.remote
            .sent
            .lock()
            .map(|rows| rows.clone())
            .unwrap_or_default()
    }

    async fn ready(&self) -> TestResult<()> {
        until(|| {
            let status = lock(&self.state).and_then(|g| view(&g.connection)).ok()?;
            (status["status"] == "connected" && self.remote.heartbeats.load(Ordering::SeqCst) > 0)
                .then_some(())
        })
        .await
    }

    fn workflow(&self, trigger: &str, out: &str, approval: bool) -> TestResult<()> {
        let guard = check(lock(&self.state))?;
        let db = &guard.connection;
        let output = json!({"id":"out","kind":"output","discordChannel":out});
        let nodes = if approval {
            json!([{"id":"confirm","kind":"approval","prompt":"Send this result?","yes":"out"},output])
        } else {
            json!([output])
        };
        let path = format!("/api/agents/{AGENT}/workflows");
        check(transaction(db, || {
            crate::automation::workflows::route(
                &self.state,
                db,
                &path,
                &["api", "agents", AGENT, "workflows"],
                &Method::POST,
                &[],
                &Default::default(),
                &json!({"id":FLOW,"name":"Discord fixture","revision":0,
                "graph":{"entry":if approval{"confirm"}else{"out"},"maxSteps":3,"nodes":nodes,"trigger":{"kind":"discord","channel":trigger}}}),
            )
        }))?;
        Ok(())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

async fn until<T>(mut test: impl FnMut() -> Option<T>) -> TestResult<T> {
    timeout(Duration::from_secs(15), async {
        loop {
            if let Some(result) = test() {
                return result;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(|_| "Discord fixture timed out".into())
}
fn message(id: &str, channel: &str, user: &str, text: &str) -> Value {
    json!({"id":id,"channel_id":channel,"author":{"id":user},"type":0,"content":text,"attachments":[]})
}
fn click(id: &str, notice: &Sent, user: &str, index: usize) -> Value {
    let custom = string(
        &notice.body["components"][0]["components"][index],
        "custom_id",
    );
    json!({"id":id,"type":3,"token":"fixture-interaction","channel_id":notice.channel,
        "member":{"user":{"id":user}},"message":{"id":notice.id,"author":{"id":BOT},"channel_id":notice.channel},
        "data":{"component_type":2,"custom_id":custom}})
}

#[tokio::test]
#[ignore = "legacy direct provider HTTP path removed"]
async fn discord_routes_dm_and_thread_to_agent_and_section_and_moves_files() -> TestResult<()> {
    let f = Fixture::new().await?;
    let generation = f.connect().await?;
    f.route(&generation, "101", "agent", AGENT).await?;
    f.route(&generation, "102", "section", SECTION).await?;
    f.ready().await?;
    let mut media = message("301", "101", REQUESTER, "Process attachment");
    media["attachments"] = json!([{"id":"789","filename":"text.txt","size":24,"url":format!("{}/attachments/123/789/text.txt",f.remote.origin)}]);
    f.event("MESSAGE_CREATE", media.clone())?;
    f.event("MESSAGE_CREATE", media)?;
    f.event(
        "MESSAGE_CREATE",
        message("302", "102", REQUESTER, "Project section task"),
    )?;
    f.event(
        "MESSAGE_CREATE",
        message("303", "102", "9999", "Blocked user"),
    )?;
    f.event(
        "MESSAGE_CREATE",
        message("304", "103", REQUESTER, "Unmapped thread"),
    )?;
    let mut oversized = message("305", "101", REQUESTER, "Oversized file");
    oversized["attachments"] = json!([{"id":"790","filename":"big.txt","size":MAX_FILE+1,"url":format!("{}/attachments/123/789/text.txt",f.remote.origin)}]);
    f.event("MESSAGE_CREATE", oversized)?;
    until(|| {
        f.snapshot()
            .into_iter()
            .find(|r| r.multipart.contains("Exact outbound file"))
    })
    .await?;
    let sent = until(|| {
        let sent = f.snapshot();
        (sent.len() >= 4 && sent.iter().any(|r| r.channel == "102")).then_some(sent)
    })
    .await?;
    assert_eq!(sent.len(), 4);
    assert!(sent.iter().any(|r| r.channel == "101"
        && r.body["message_reference"]["message_id"] == "301"
        && r.body["content"] == "Fixture agent reply"));
    assert!(sent
        .iter()
        .any(|r| r.channel == "102" && r.body["message_reference"]["message_id"] == "302"));
    assert!(sent.iter().any(|r| r.channel == "101"
        && r.body["message_reference"]["message_id"] == "305"
        && string(&r.body, "content").contains("could not be used")));
    assert!(sent.iter().any(|r| r.channel == "101"
        && r.multipart.contains("outbound.txt")
        && r.multipart.contains("\"message_id\":\"301\"")));
    let guard = check(lock(&f.state))?;
    let turns = check(all(
        &guard.connection,
        "SELECT channel,thread,text FROM turns ORDER BY seq",
        [],
    ))?;
    assert_eq!(turns.len(), 2);
    assert!(turns
        .iter()
        .any(|r| r["channel"] == SECTION && string(r, "text").contains("Project section task")));
    assert!(turns
        .iter()
        .any(|r| r["thread"].is_string() && string(r, "text").contains("Process attachment")));
    Ok(())
}

#[tokio::test]
#[ignore = "legacy direct provider HTTP path removed"]
async fn discord_resumes_and_only_requester_can_approve_triggered_workflow() -> TestResult<()> {
    let f = Fixture::new().await?;
    let generation = f.connect().await?;
    f.route(&generation, "101", "agent", AGENT).await?;
    f.workflow("101", "101", true)?;
    f.ready().await?;
    let sequence = f.event(
        "MESSAGE_CREATE",
        message("401", "101", REQUESTER, "Approved result"),
    )?;
    let notice = until(|| {
        f.snapshot()
            .into_iter()
            .find(|r| r.body["components"].is_array())
    })
    .await?;
    until(|| (f.remote.commands.receiver_count() > 0).then_some(())).await?;
    f.remote
        .commands
        .send(json!({"op":7,"d":null}))
        .map_err(|_| "Gateway unavailable")?;
    let resume = until(|| {
        f.remote
            .sessions
            .lock()
            .ok()?
            .iter()
            .find(|s| s["op"] == 6)
            .cloned()
    })
    .await?;
    assert_eq!(resume["d"]["seq"], sequence);
    assert_eq!(resume["d"]["session_id"], "fixture-session");
    f.ready().await?;
    f.event("INTERACTION_CREATE", click("501", &notice, OTHER, 0))?;
    let denied = until(|| f.remote.callbacks.lock().ok()?.first().cloned()).await?;
    assert_eq!(denied["data"]["flags"], 64);
    assert_eq!(
        denied["data"]["content"],
        "Only the original requester can choose."
    );
    assert_eq!(f.snapshot().len(), 1);
    f.event("INTERACTION_CREATE", click("502", &notice, REQUESTER, 0))?;
    f.event("INTERACTION_CREATE", click("503", &notice, REQUESTER, 1))?;
    let output = until(|| {
        f.snapshot()
            .into_iter()
            .find(|r| r.body["content"] == "Approved result")
    })
    .await?;
    assert_eq!(output.body["message_reference"]["message_id"], "401");
    assert_eq!(output.channel, "101");
    until(|| {
        f.remote
            .callbacks
            .lock()
            .ok()?
            .iter()
            .any(|r| r["data"]["content"] == "A choice was already recorded.")
            .then_some(())
    })
    .await?;
    assert_eq!(f.snapshot().len(), 2);
    Ok(())
}

#[tokio::test]
#[ignore = "legacy direct provider HTTP path removed"]
async fn discord_observes_rate_limits_and_reports_unknown_partial_send_without_retry(
) -> TestResult<()> {
    let f = Fixture::new().await?;
    let generation = f.connect().await?;
    f.route(&generation, "101", "agent", AGENT).await?;
    f.workflow("101", "101", false)?;
    f.ready().await?;
    f.event(
        "MESSAGE_CREATE",
        message("601", "101", REQUESTER, "Rate check"),
    )?;
    until(|| (f.snapshot().len() >= 2).then_some(())).await?;
    let sent = f.snapshot();
    assert!(sent[1].at.duration_since(sent[0].at) >= Duration::from_millis(500));
    f.event(
        "MESSAGE_CREATE",
        message("602", "101", REQUESTER, "Bucket first"),
    )?;
    f.event(
        "MESSAGE_CREATE",
        message("603", "101", REQUESTER, "Bucket second"),
    )?;
    until(|| (f.snapshot().len() >= 4).then_some(())).await?;
    let sent = f.snapshot();
    assert!(sent[3].at.duration_since(sent[2].at) >= Duration::from_millis(500));
    let content = "X".repeat(2000) + "TAIL";
    f.event("MESSAGE_CREATE", message("604", "101", REQUESTER, &content))?;
    let failed = until(|| {
        let guard = lock(&f.state).ok()?;
        one(
            &guard.connection,
            "SELECT error FROM workflow_runs WHERE status='failed'",
            [],
        )
        .ok()
        .flatten()
    })
    .await?;
    assert!(string(&failed, "error").contains("1 of 2 parts confirmed"));
    let status = f.settings("", None, 200).await?;
    assert_eq!(status["deliveries"][0]["status"], "uncertain");
    assert_eq!(status["deliveries"][0]["part"], 1);
    sleep(Duration::from_millis(350)).await;
    assert_eq!(f.remote.partial.load(Ordering::SeqCst), 1);
    assert_eq!(f.snapshot().len(), 6);
    Ok(())
}
