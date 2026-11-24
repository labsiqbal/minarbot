use super::{
    protocol::{Attachment, Endpoint, Events, MediaKind, Session},
    *,
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use whatsapp_rust::{
    async_trait,
    prelude::{wa, InboundMessage, MessageInfo},
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
