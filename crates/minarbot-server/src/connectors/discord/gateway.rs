use super::*;
use futures_util::{SinkExt, StreamExt};
use std::{sync::Arc, time::Duration};
use tokio::{
    net::TcpStream,
    task::JoinSet,
    time::{sleep, timeout, Instant},
};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{protocol::WebSocketConfig, Message},
    MaybeTlsStream, WebSocketStream,
};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Default)]
struct Session {
    id: String,
    sequence: Option<u64>,
    url: Option<url::Url>,
    identified: Option<Instant>,
}

enum Disconnect {
    Resume,
    Identify,
    Fatal(&'static str),
}

async fn send(socket: &mut Socket, value: Value) -> std::result::Result<(), Disconnect> {
    timeout(
        Duration::from_secs(5),
        socket.send(Message::Text(value.to_string().into())),
    )
    .await
    .map_err(|_| Disconnect::Resume)?
    .map_err(|_| Disconnect::Resume)
}

fn gateway_url(raw: &str, test: Option<&str>) -> Option<url::Url> {
    let mut url = url::Url::parse(raw).ok()?;
    let fixture = test
        .and_then(|s| s.strip_prefix("http://"))
        .is_some_and(|authority| url.scheme() == "ws" && url.authority() == authority);
    let discord = url.scheme() == "wss"
        && url.port().is_none()
        && url.host_str().is_some_and(|h| {
            h == "gateway.discord.gg" || h.starts_with("gateway-") && h.ends_with(".discord.gg")
        });
    if !(if test.is_some() { fixture } else { discord })
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    url.set_query(Some("v=10&encoding=json"));
    url.set_fragment(None);
    Some(url)
}

pub(super) async fn listen(state: &AppState, bot: &Bot, api: &Arc<Api>) {
    let test = state.config.connectors.discord_test_origin.as_deref();
    let raw = test.and_then(|s| s.strip_prefix("http://")).map_or_else(
        || "wss://gateway.discord.gg/".into(),
        |s| format!("ws://{s}/gateway"),
    );
    let Some(base) = gateway_url(&raw, test) else {
        return;
    };
    let mut session = Session::default();
    let mut backoff = 1;
    let mut callbacks = JoinSet::new();
    loop {
        if let Some(identified) = session.identified.filter(|_| session.id.is_empty()) {
            tokio::time::sleep_until(identified + Duration::from_secs(5)).await;
        }
        let start = Instant::now();
        let url = session.url.clone().unwrap_or_else(|| base.clone());
        let result = connection(state, bot, &mut session, &url, api, &mut callbacks).await;
        match result {
            Disconnect::Fatal(status) => {
                let _ = connection_status(state, &bot.generation, status);
                return;
            }
            Disconnect::Identify => {
                session = Session {
                    identified: session.identified,
                    ..Session::default()
                }
            }
            Disconnect::Resume => (),
        }
        let _ = connection_status(state, &bot.generation, "reconnecting");
        if start.elapsed() > Duration::from_secs(60) {
            backoff = 1;
        }
        sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(30);
    }
}

fn dispatch(
    state: &AppState,
    bot: &Bot,
    event: &Value,
    session: &mut Session,
) -> Result<Option<Callback>> {
    let data = &event["d"];
    let callback = match string(event, "t") {
        "READY" => {
            if data["user"]["id"] != bot.id || string(data, "session_id").is_empty() {
                return Err(error(400, "Discord bot identity changed."));
            }
            session.id = string(data, "session_id").to_owned();
            session.url = gateway_url(
                string(data, "resume_gateway_url"),
                state.config.connectors.discord_test_origin.as_deref(),
            );
            connection_status(state, &bot.generation, "connected")?;
            None
        }
        "RESUMED" => {
            connection_status(state, &bot.generation, "connected")?;
            None
        }
        "MESSAGE_CREATE" => {
            jobs::record(state, bot, data)?;
            None
        }
        "INTERACTION_CREATE" => jobs::interaction(state, bot, data)?,
        _ => None,
    };
    if let Some(sequence) = event["s"].as_u64() {
        session.sequence = Some(sequence);
    }
    Ok(callback)
}

async fn connection(
    state: &AppState,
    bot: &Bot,
    session: &mut Session,
    url: &url::Url,
    api: &Arc<Api>,
    callbacks: &mut JoinSet<()>,
) -> Disconnect {
    let config = WebSocketConfig::default()
        .max_message_size(Some(2 * 1024 * 1024))
        .max_frame_size(Some(2 * 1024 * 1024));
    let Ok(Ok((mut socket, _))) = timeout(
        Duration::from_secs(15),
        connect_async_with_config(url.as_str(), Some(config), false),
    )
    .await
    else {
        return Disconnect::Resume;
    };
    let mut heartbeat = None;
    let mut due = Instant::now() + Duration::from_secs(15);
    let mut awaiting_ack = false;
    loop {
        let write = tokio::select! {
            _ = tokio::time::sleep_until(due) => {
                let Some(interval) = heartbeat else { return Disconnect::Resume; };
                if awaiting_ack { return Disconnect::Resume; }
                due = Instant::now() + interval;
                awaiting_ack = true;
                json!({"op":1,"d":session.sequence})
            }
            _ = callbacks.join_next(), if !callbacks.is_empty() => continue,
            message = socket.next() => {
                let value: Value = match message {
                    Some(Ok(Message::Text(text))) => match serde_json::from_str(&text) {
                        Ok(value) => value, Err(_) => return Disconnect::Resume,
                    },
                    Some(Ok(Message::Close(frame))) => return match frame.map(|f| u16::from(f.code)) {
                        Some(4004) => Disconnect::Fatal("invalid_token"),
                        Some(4013 | 4014) => Disconnect::Fatal("intents_required"),
                        Some(4010..=4012) => Disconnect::Fatal("gateway_rejected"),
                        Some(4003 | 4007 | 4009) => Disconnect::Identify,
                        _ => Disconnect::Resume,
                    },
                    Some(Ok(_)) => continue,
                    _ => return Disconnect::Resume,
                };
                match value["op"].as_u64() {
                    Some(10) if heartbeat.is_none() => {
                        let Some(ms) = value["d"]["heartbeat_interval"].as_u64().filter(|ms| (10..=120_000).contains(ms)) else {return Disconnect::Resume;};
                        heartbeat = Some(Duration::from_millis(ms));
                        let mut jitter = [0; 2];
                        let _ = getrandom::getrandom(&mut jitter);
                        due = Instant::now() + Duration::from_millis(ms * u64::from(u16::from_ne_bytes(jitter)) / 65_536);
                        if let Some(sequence) = session.sequence.filter(|_| !session.id.is_empty()) {
                            json!({"op":6,"d":{"token":bot.token,"session_id":session.id,"seq":sequence}})
                        } else {
                            session.identified = Some(Instant::now());
                            json!({"op":2,"d":{"token":bot.token,"intents":1|512|4096|32768,"properties":{"os":std::env::consts::OS,"browser":"Minarbot","device":"Minarbot"}}})
                        }
                    }
                    Some(0) if heartbeat.is_some() => {
                        match dispatch(state, bot, &value, session) {
                            Ok(Some(callback)) => {
                                let api = api.clone();
                                callbacks.spawn(async move { callback.respond(&api).await; });
                            }
                            Ok(None) => (),
                            Err(e) if e.status == 400 => return Disconnect::Fatal("gateway_rejected"),
                            Err(_) => return Disconnect::Resume,
                        }
                        continue;
                    }
                    Some(1) => { awaiting_ack = true; json!({"op":1,"d":session.sequence}) }
                    Some(7) => return Disconnect::Resume,
                    Some(9) if value["d"] != true => return Disconnect::Identify,
                    Some(9) => return Disconnect::Resume,
                    Some(11) => { awaiting_ack = false; continue; }
                    _ => continue,
                }
            }
        };
        if let Err(reason) = send(&mut socket, write).await {
            return reason;
        }
    }
}
