use super::{
    protocol::{Attachment, Endpoint, Events, MediaKind, Session},
    *,
};
use crate::connectors::test_support::{response, Fixture};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use whatsapp_rust::{
    async_trait,
    prelude::{wa, InboundMessage, MessageBuilderExt, MessageInfo},
    wacore::store::{
        traits::{DeviceStore, SignalStore},
        Device,
    },
};

const AGENT: &str = "bfb10560-2a18-4350-8dd0-729acf350bc9";
const SECTION: &str = "9bfa5715-d06f-4351-8d9a-a816c9490f1d";
const WORKFLOW: &str = "e575a3d1-f2be-4791-b36d-1dfa0ce7fc99";
const CHAT: &str = "12345@g.us";
const SENDER: &str = "12345678901@s.whatsapp.net";
const OTHER: &str = "12345678902@s.whatsapp.net";

fn checked<T>(value: Result<T>) -> crate::db::Result<T> {
    value.map_err(|error| error.message.into())
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> crate::db::Result<Self> {
        let path = std::env::temp_dir().join(format!("minarbot-whatsapp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn state(directory: &Scratch, provider: &str) -> crate::db::Result<AppState> {
    let mut config: crate::Config = serde_json::from_value(json!({
        "database":directory.0.join("minarbot.sqlite"),
        "baseURL":format!("{provider}/v1"),
        "apiKey":"fake-provider",
        "defaultModel":"fixture",
        "transcriptionProvider":{"baseURL":format!("{provider}/v1"),"model":"voice-fixture","apiKey":"fake-voice"}
    }))?;
    config.data.validate()?;
    let database = crate::db::open(&config.database, false, crate::config::now())?;
    let db = &database.connection;
    crate::files::init(db)?;
    crate::data::initialize(&database)?;
    checked(crate::turns::initialize(db))?;
    checked(crate::mcp::initialize(db))?;
    checked(crate::automation::initialize(db))?;
    checked(initialize(db))?;
    let computer = Arc::new(crate::computer::Manager::new(&config)?);
    Ok(AppState {
        database: Arc::new(Mutex::new(database)),
        config: Arc::new(config),
        origin: "http://127.0.0.1:12345".into(),
        auth: None,
        events: crate::events::Hub::new(),
        turns: Default::default(),
        computer,
        trial_pending: Default::default(),
        whatsapp: Default::default(),
    })
}

fn database<T>(
    state: &AppState,
    run: impl FnOnce(&Connection) -> Result<T>,
) -> crate::db::Result<T> {
    let guard = checked(lock(state))?;
    checked(run(&guard.connection))
}

fn change(state: &AppState, action: &str, mut value: Value) -> crate::db::Result<Value> {
    value["id"] = json!(uuid::Uuid::new_v4().to_string());
    checked(route(
        state,
        &format!("/api/whatsapp/{action}"),
        &Method::POST,
        &value,
    ))
}

fn view(state: &AppState) -> crate::db::Result<Value> {
    checked(route(state, "/api/whatsapp", &Method::GET, &Value::Null))
}

#[derive(Clone)]
struct FakeEndpoint {
    origin: String,
    open: tokio::sync::mpsc::UnboundedSender<Events>,
    dropped: Arc<AtomicBool>,
}
struct FakeSession {
    origin: String,
    dropped: Arc<AtomicBool>,
}
#[async_trait]
impl Endpoint for FakeEndpoint {
    async fn open(&self, state: AppState, generation: &str) -> Result<Box<dyn Session>> {
        let events = Events {
            state,
            generation: generation.into(),
        };
        self.dropped.store(false, Ordering::SeqCst);
        if lock(&events.state).and_then(|db| link_row(&db.connection))?["state"] == "pairing" {
            events.qr("fixture-only-whatsapp-qr", Duration::from_secs(30))?;
        } else {
            events.connected(true)?;
        }
        self.open.send(events).map_err(|_| Failure::storage())?;
        Ok(Box::new(FakeSession {
            origin: self.origin.clone(),
            dropped: self.dropped.clone(),
        }))
    }
}
#[async_trait]
impl Session for FakeSession {
    fn connected(&self) -> bool {
        !self.dropped.load(Ordering::SeqCst)
    }
    fn finished(&mut self) -> bool {
        !self.connected()
    }
    async fn download(&self, mut descriptor: &[u8]) -> Result<Attachment> {
        use whatsapp_rust::buffa::Message;
        let parsed = wa::Message::decode(&mut descriptor).map_err(|_| Failure::storage())?;
        let (path, name, kind) = if parsed.audio_message.as_option().is_some() {
            ("voice", "voice.ogg", MediaKind::Audio)
        } else if parsed.image_message.as_option().is_some() {
            ("image", "image.png", MediaKind::Image)
        } else {
            ("document", "document.txt", MediaKind::Document)
        };
        let bytes = reqwest::get(format!("{}/fake/{path}", self.origin))
            .await
            .map_err(|_| Failure::storage())?
            .bytes()
            .await
            .map_err(|_| Failure::storage())?;
        Ok(Attachment {
            name: name.into(),
            mime: String::new(),
            kind,
            bytes: bytes.to_vec(),
        })
    }
    async fn send(&self, chat: &str, id: &str, text: &str, file: Option<Attachment>) -> Result<()> {
        let body = json!({"chat":chat,"id":id,"text":text,"file":file.map(|file| file.name)});
        let res = reqwest::Client::new()
            .post(format!("{}/fake/send", self.origin))
            .json(&body)
            .send()
            .await
            .map_err(|_| Failure::storage())?;
        if res.status().is_success() {
            Ok(())
        } else {
            Err(error(502, "Unconfirmed test send"))
        }
    }
    async fn close(self: Box<Self>, _unlink: bool) {}
}

fn message(
    chat: &str,
    sender: &str,
    id: &str,
    body: wa::Message,
) -> crate::db::Result<InboundMessage> {
    let mut info = MessageInfo {
        id: id.into(),
        push_name: "Fixture chat".into(),
        ..Default::default()
    };
    info.source.chat = chat.parse()?;
    info.source.sender = sender.parse()?;
    Ok(InboundMessage::builder()
        .info(Arc::new(info))
        .message(Arc::new(body))
        .build())
}

async fn wait_for(state: &AppState, query: &str) -> crate::db::Result<()> {
    tokio::time::timeout(Duration::from_secs(35), async {
        loop {
            if database(state, |db| exists(db, query, []))? {
                return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await?
}

#[tokio::test]
async fn whatsapp_fake_endpoint_links_routes_transcribes_and_recovers() -> crate::db::Result<()> {
    use axum::response::IntoResponse;
    let sends = Arc::new(Mutex::new(Vec::<Value>::new()));
    let fixture = checked(
        Fixture::serve({
            let sends = sends.clone();
            move |req| {
                let sends = sends.clone();
                async move {
                    match req.uri().path() {
                        "/fake/voice" => b"OggS fixture OpusHead".as_slice().into_response(),
                        "/fake/image" => {
                            let mut bytes = std::io::Cursor::new(Vec::new());
                            assert!(image::DynamicImage::new_rgb8(1, 1)
                                .write_to(&mut bytes, image::ImageFormat::Png)
                                .is_ok());
                            bytes.into_inner().into_response()
                        }
                        "/fake/document" => "hello document".into_response(),
                        "/fake/send" => {
                            let body = super::super::test_support::body(req).await;
                            let fail = body["text"] == "Uncertain fixture";
                            if let Ok(mut outgoing) = sends.lock() {
                                outgoing.push(body);
                            }
                            if fail {
                                axum::http::StatusCode::BAD_GATEWAY.into_response()
                            } else {
                                response(json!({}))
                            }
                        }
                        "/v1/audio/transcriptions" => {
                            response(json!({"text":"Fixture voice transcript"}))
                        }
                        "/v1/chat/completions" => response(
                            json!({"choices":[{"message":{"content":"Fixture agent reply"}}]}),
                        ),
                        _ => axum::http::StatusCode::NOT_FOUND.into_response(),
                    }
                }
            }
        })
        .await,
    )?;
    let directory = Scratch::new()?;
    let state1 = state(&directory, &fixture.origin)?;
    database(&state1, |db| {
        db.execute("INSERT INTO agents(id,name,model,instructions,runtime) VALUES(?,'Fixture','fixture','Fixture agent','api')",[AGENT])?;
        db.execute("INSERT INTO channels(id,name,instructions,lead) VALUES(?,'Project section','Fixture section',?)",[SECTION,AGENT])?;
        db.execute(
            "INSERT INTO channel_members(channel,agent) VALUES(?,?)",
            [SECTION, AGENT],
        )?;
        db.execute(
            "INSERT INTO workflows(id,agent,name,revision) VALUES(?,?,'Fixture workflow',1)",
            [WORKFLOW, AGENT],
        )?;
        let graph = json!({"entry":"review","maxSteps":3,"nodes":[{"id":"review","kind":"approval","prompt":"Confirm fixture?","yes":"result"},{"id":"result","kind":"output"}]});
        db.execute(
            "INSERT INTO workflow_versions VALUES(?,1,?)",
            params![WORKFLOW, graph.to_string()],
        )?;
        Ok(())
    })?;
    let (opened, mut connections) = tokio::sync::mpsc::unbounded_channel();
    let endpoint = FakeEndpoint {
        origin: fixture.origin.clone(),
        open: opened,
        dropped: Default::default(),
    };
    assert_eq!(view(&state1)?["state"], "disconnected");
    let browser = axum::extract::Request::builder()
        .uri("/api/whatsapp")
        .header("host", "127.0.0.1:12345")
        .header("sec-fetch-site", "cross-site")
        .body(axum::body::Body::empty())?;
    let rejected = super::super::entry(axum::extract::State(state1.clone()), browser).await;
    assert_eq!(rejected.status(), axum::http::StatusCode::FORBIDDEN);

    let scheduler = crate::automation::start(state1.clone());
    let task = tokio::spawn({
        let state = state1.clone();
        let endpoint = endpoint.clone();
        async move { supervise(state, &endpoint).await }
    });
    assert_eq!(change(&state1, "link", json!({}))?["state"], "pairing");
    let events = tokio::time::timeout(Duration::from_secs(10), connections.recv())
        .await?
        .ok_or("Endpoint did not open")?;
    let start = view(&state1)?;
    assert!(string(&start, "qr").starts_with("data:image/svg+xml;base64,"));
    assert!(!start.to_string().contains("fixture-only-whatsapp-qr"));
    let store = store::Store {
        state: state1.clone(),
        generation: events.generation.clone(),
    };
    let mut device = Device::new();
    device.pn = Some(SENDER.parse()?);
    store.save(&device).await?;
    let fixture_secret = b"fixture-private-session-material";
    store
        .put_session("fixture-secret-index", fixture_secret)
        .await?;
    checked(events.connected(true))?;
    assert_eq!(view(&state1)?["state"], "linked");
    assert!(view(&state1)?["qr"].is_null());
    let persisted = database(&state1, |db| all(db, "SELECT * FROM whatsapp_session", []))?
        .into_iter()
        .map(|row| row.to_string())
        .collect::<String>();
    assert!(!persisted.contains("fixture-private-session-material"));
    assert!(!persisted.contains("fixture-secret-index"));

    let unmapped = message(CHAT, SENDER, "m0", wa::Message::text("Ignore me"))?;
    checked(events.ingest(&[unmapped]))?;
    assert!(!database(&state1, |db| exists(
        db,
        "SELECT 1 FROM whatsapp_inbox",
        []
    ))?);
    assert_eq!(view(&state1)?["chats"][0]["chat"], CHAT);
    change(
        &state1,
        "map",
        json!({"chat":CHAT,"kind":"channels","target":SECTION,"transcribe":true}),
    )?;
    let incoming = message(CHAT, SENDER, "m1", wa::Message::text("Hello fixture"))?;
    checked(events.ingest(&[incoming.clone(), incoming]))?;
    wait_for(
        &state1,
        "SELECT 1 FROM turns WHERE text='Hello fixture' AND channel IS NOT NULL",
    )
    .await?;
    assert_eq!(
        database(&state1, |db| Ok(db.query_row(
            "SELECT count(*) FROM whatsapp_inbox WHERE message='m1'",
            [],
            |r| r.get::<_, i64>(0)
        )?))?,
        1
    );

    let direct = message(
        OTHER,
        OTHER,
        "new-direct",
        wa::Message::text("Discover direct chat"),
    )?;
    checked(events.ingest(&[direct]))?;
    change(
        &state1,
        "map",
        json!({"chat":OTHER,"kind":"agents","target":AGENT,"transcribe":false}),
    )?;
    let direct = message(
        OTHER,
        OTHER,
        "direct",
        wa::Message::text("Direct routing fixture"),
    )?;
    checked(events.ingest(&[direct]))?;
    wait_for(&state1, "SELECT 1 FROM turns WHERE text='Direct routing fixture' AND thread IS NOT NULL AND channel IS NULL").await?;
    database(&state1, |db| {
        db.execute("UPDATE turns SET status='done',reply='Direct reply fixture' WHERE text='Direct routing fixture'", [])?;
        Ok(())
    })?;
    wait_for(
        &state1,
        "SELECT 1 FROM whatsapp_outbox WHERE status='sent' AND text='Direct reply fixture'",
    )
    .await?;

    let voice = wa::Message {
        audio_message: whatsapp_rust::prelude::MessageField::some(wa::message::AudioMessage {
            ptt: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    };
    let document = wa::Message {
        document_message: whatsapp_rust::prelude::MessageField::some(
            wa::message::DocumentMessage {
                file_name: Some("document.txt".into()),
                ..Default::default()
            },
        ),
        ..Default::default()
    };
    let image = wa::Message {
        image_message: whatsapp_rust::prelude::MessageField::some(wa::message::ImageMessage {
            caption: Some("Read the image".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    checked(events.ingest(&[
        message(CHAT, SENDER, "voice", voice)?,
        message(CHAT, SENDER, "doc", document)?,
        message(CHAT, SENDER, "image", image)?,
    ]))?;
    wait_for(
        &state1,
        "SELECT 1 FROM turns WHERE text='Fixture voice transcript'",
    )
    .await?;
    wait_for(&state1, "SELECT 1 FROM files WHERE name='document.txt'").await?;
    wait_for(&state1, "SELECT 1 FROM turns WHERE text='Read the image'").await?;

    // A partial send must preserve acknowledged files and avoid replaying uncertain text.
    database(&state1, |db| {
        let turn = one(db, "SELECT id FROM turns WHERE text='Hello fixture'", [])?
            .ok_or_else(Failure::storage)?;
        let turn = string(&turn, "id");
        for name in ["image.png", "document.txt", "voice.ogg"] {
            let file = one(db, "SELECT id FROM files WHERE name=?", [name])?
                .ok_or_else(Failure::storage)?;
            db.execute(
                "INSERT INTO generated_files(turn,file,call_id,call,after_action) VALUES(?,?,?,'{}',0)",
                [turn, string(&file, "id"), name],
            )?;
        }
        db.execute(
            "UPDATE turns SET status='done',reply='Uncertain fixture' WHERE id=?",
            [turn],
        )?;
        Ok(())
    })?;
    wait_for(
        &state1,
        "SELECT 1 FROM whatsapp_outbox WHERE status='uncertain'",
    )
    .await?;
    assert_eq!(view(&state1)?["deliveryProblems"][0]["status"], "uncertain");
    wait_for(&state1, "SELECT 1 WHERE (SELECT count(*) FROM whatsapp_outbox WHERE file IS NOT NULL AND status='sent')=3").await?;

    let workflow = message(
        CHAT,
        SENDER,
        "run",
        wa::Message::text(format!("/workflow {WORKFLOW} 1 Workflow round trip")),
    )?;
    checked(events.ingest(&[workflow]))?;
    wait_for(&state1,"SELECT 1 FROM whatsapp_prompts p JOIN whatsapp_outbox o ON o.id=p.notice WHERE p.prompt_type='workflow' AND o.status='sent'").await?;
    let notice = database(&state1, |db| {
        one(
            db,
            "SELECT notice FROM whatsapp_prompts WHERE prompt_type='workflow'",
            [],
        )
    })?
    .ok_or("No workflow prompt")?;
    let number = || {
        wa::Message::text_with_context(
            "1",
            wa::ContextInfo {
                stanza_id: Some(string(&notice, "notice").into()),
                ..Default::default()
            },
        )
    };
    checked(events.ingest(&[message(CHAT, OTHER, "not-requester", number())?]))?;
    wait_for(
        &state1,
        "SELECT 1 FROM whatsapp_inbox WHERE message='not-requester' AND status='done'",
    )
    .await?;
    assert!(database(&state1, |db| exists(
        db,
        "SELECT 1 FROM workflow_runs WHERE status='approval'",
        []
    ))?);
    checked(events.ingest(&[message(CHAT, SENDER, "requester", number())?]))?;
    wait_for(&state1, "SELECT 1 FROM whatsapp_prompts WHERE answered=1").await?;
    wait_for(
        &state1,
        "SELECT 1 FROM whatsapp_outbox WHERE text='Workflow round trip' AND status='sent'",
    )
    .await?;

    endpoint.dropped.store(true, Ordering::SeqCst);
    state1.whatsapp.wake.notify_one();
    let reconnect = tokio::time::timeout(Duration::from_secs(12), connections.recv())
        .await?
        .ok_or("Endpoint did not reconnect")?;
    assert_eq!(reconnect.generation, events.generation);
    task.abort();
    let _ = task.await;
    scheduler.abort();
    let _ = scheduler.await;
    drop(reconnect);
    drop(events);
    drop(store);
    drop(state1);
    let state2 = state(&directory, &endpoint.origin)?;
    let generation = database(&state2, link_row)?;
    let store = store::Store {
        state: state2.clone(),
        generation: string(&generation, "generation").into(),
    };
    assert_eq!(
        store.get_session("fixture-secret-index").await?.as_deref(),
        Some(fixture_secret.as_slice())
    );
    let task = tokio::spawn({
        let state = state2.clone();
        let endpoint = endpoint.clone();
        async move { supervise(state, &endpoint).await }
    });
    let restart = tokio::time::timeout(Duration::from_secs(10), connections.recv())
        .await?
        .ok_or("Endpoint did not reopen")?;
    assert!(view(&state2)?["connected"] == true);
    {
        let sent = sends.lock().map_err(|_| "Send fixture lock")?;
        assert_eq!(
            sent.iter()
                .filter(|row| row["text"] == "Uncertain fixture")
                .count(),
            1
        );
        assert!(sent.iter().all(|row| {
            row["chat"]
                == if row["text"] == "Direct reply fixture" {
                    OTHER
                } else {
                    CHAT
                }
        }));
    }
    checked(restart.expired())?;
    assert_eq!(view(&state2)?["state"], "needs_relink");
    assert_eq!(
        change(&state2, "unlink", json!({}))?["state"],
        "disconnected"
    );
    assert!(!database(&state2, |db| exists(
        db,
        "SELECT 1 FROM whatsapp_session",
        []
    ))?);
    assert!(!minarbot_launcher::files::sidecar(&state2.config.database, ".whatsapp-key").exists());
    task.abort();
    let _ = task.await;
    Ok(())
}
