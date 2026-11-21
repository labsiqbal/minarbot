use super::*;
use futures_util::FutureExt;
use std::{
    io::{self, Cursor, Seek, SeekFrom, Write},
    sync::Arc,
};
use whatsapp_rust::{
    async_trait,
    buffa::Message as _,
    media,
    prelude::*,
    wacore::download::{DownloadWriter, Downloadable, MediaType},
    wacore_binary::JidExt,
    InboundDurabilityHook, UploadOptions,
};

#[derive(Clone, Copy, PartialEq)]
pub(super) enum MediaKind {
    Document,
    Image,
    Audio,
}

pub(super) struct Attachment {
    pub name: String,
    pub kind: MediaKind,
    pub mime: String,
    pub bytes: Vec<u8>,
}

#[async_trait]
pub(super) trait Session: Send + Sync {
    fn connected(&self) -> bool;
    fn finished(&mut self) -> bool;
    async fn download(&self, descriptor: &[u8]) -> Result<Attachment>;
    async fn send(&self, chat: &str, id: &str, text: &str, file: Option<Attachment>) -> Result<()>;
    async fn close(self: Box<Self>, unlink: bool);
}

#[async_trait]
pub(super) trait Endpoint: Send + Sync {
    async fn open(&self, state: AppState, generation: &str) -> Result<Box<dyn Session>>;
}

pub(super) struct Native;

struct NativeSession {
    bot: whatsapp_rust::bot::BotHandle,
    ack: Ack,
    events: Events,
}

type Ack = Arc<Mutex<Option<(String, tokio::sync::oneshot::Sender<bool>)>>>;

#[derive(Clone)]
pub(super) struct Events {
    pub state: AppState,
    pub generation: String,
}

impl Events {
    pub fn qr(&self, code: &str, timeout: Duration) -> Result<()> {
        let image = render_qr(code)?;
        set_view(&self.state, &self.generation, |view| {
            view.qr = Some((image, Instant::now() + timeout.min(PAIR_TIMEOUT)));
        })
    }

    pub fn connected(&self, connected: bool) -> Result<()> {
        set_view(&self.state, &self.generation, |view| {
            view.connected = connected;
            if connected {
                view.qr = None;
            }
        })
    }

    pub fn expired(&self) -> Result<()> {
        invalidate(&self.state, &self.generation)
    }

    pub fn ingest(&self, batch: &[InboundMessage]) -> Result<()> {
        let db = lock(&self.state)?;
        if !exists(
            &db.connection,
            "SELECT 1 FROM whatsapp_link WHERE singleton=1 AND state='linked' AND generation=?",
            [&self.generation],
        )? {
            return Ok(());
        }
        transaction(&db.connection, || {
            for incoming in batch {
                if let Some(input) = parse(incoming) {
                    jobs::record(&self.state, &db.connection, input)?;
                }
            }
            Ok(())
        })?;
        self.state.whatsapp.wake.notify_one();
        Ok(())
    }
}

#[async_trait]
impl InboundDurabilityHook for Events {
    async fn on_messages(
        &self,
        _client: Arc<Client>,
        batch: &[InboundMessage],
    ) -> whatsapp_rust::anyhow::Result<()> {
        self.ingest(batch)
            .map_err(|_| whatsapp_rust::anyhow::anyhow!("WhatsApp inbox commit failed"))
    }
}

#[async_trait]
impl Endpoint for Native {
    async fn open(&self, state: AppState, generation: &str) -> Result<Box<dyn Session>> {
        let events = Events {
            state: state.clone(),
            generation: generation.to_owned(),
        };
        let ack: Ack = Default::default();
        let ack_events = ack.clone();
        let callback_events = events.clone();
        let bot = Bot::builder()
            .with_backend(store::Store {
                state,
                generation: generation.into(),
            })
            .skip_history_sync()
            .with_inbound_durability_hook(events.clone())
            .with_event_delivery(EventDelivery::Ordered { capacity: 32 })
            .on_event_for(
                &[
                    EventKind::PairingQrCode,
                    EventKind::PairingQrCodesExhausted,
                    EventKind::PairError,
                    EventKind::Connected,
                    EventKind::Disconnected,
                    EventKind::LoggedOut,
                    EventKind::ClientOutdated,
                    EventKind::ServerAck,
                ],
                move |event, _| {
                    let events = callback_events.clone();
                    let ack_events = ack_events.clone();
                    async move {
                        let _ = match event.as_ref() {
                            Event::PairingQrCode(qr) => events.qr(&qr.code, qr.timeout),
                            Event::PairingQrCodesExhausted(_)
                            | Event::PairError(_)
                            | Event::LoggedOut(_)
                            | Event::ClientOutdated(_) => events.expired(),
                            Event::Connected(_) => events.connected(true),
                            Event::Disconnected(_) => events.connected(false),
                            Event::ServerAck(ack) if ack.class.as_deref() == Some("message") => {
                                if let Ok(mut pending) = ack_events.lock() {
                                    if pending.as_ref().is_some_and(|(id, _)| *id == ack.id) {
                                        if let Some((_, tx)) = pending.take() {
                                            let _ = tx.send(ack.error.is_none());
                                        }
                                    }
                                }
                                Ok(())
                            }
                            _ => Ok(()),
                        };
                    }
                },
            )
            .build()
            .await
            .map_err(|_| error(409, "WhatsApp session could not be opened."))?;
        Ok(Box::new(NativeSession {
            bot: bot.spawn(),
            ack,
            events,
        }))
    }
}

fn unknown() -> Failure {
    error(502, "Send outcome could not be confirmed. Check WhatsApp.")
}

#[async_trait]
impl Session for NativeSession {
    fn connected(&self) -> bool {
        let client = self.bot.client();
        client.is_connected() && client.is_logged_in()
    }

    fn finished(&mut self) -> bool {
        (&mut self.bot).now_or_never().is_some()
    }

    async fn download(&self, mut descriptor: &[u8]) -> Result<Attachment> {
        let invalid = || {
            error(
                415,
                "WhatsApp attachment is invalid or exceeds its size limit.",
            )
        };
        let message = wa::Message::decode(&mut descriptor).map_err(|_| invalid())?;
        let (part, name, kind) = attachment(&message).ok_or_else(invalid)?;
        let limit = media_limit(kind);
        if part
            .file_length()
            .is_some_and(|length| length > limit as u64)
            || part.direct_path().is_none_or(|path| {
                !path.starts_with('/') || path.chars().any(|c| c.is_control() || c == '\\')
            })
            || part.media_key().is_none_or(|key| key.len() != 32)
        {
            return Err(invalid());
        }
        let bytes = tokio::time::timeout(
            Duration::from_secs(45),
            self.bot
                .client()
                .download_to_writer(part, BoundedWriter(Cursor::new(Vec::new()), limit)),
        )
        .await
        .map_err(|_| invalid())?
        .map_err(|_| invalid())?
        .0
        .into_inner();
        if bytes.is_empty() {
            return Err(invalid());
        }
        Ok(Attachment {
            name,
            kind,
            mime: String::new(),
            bytes,
        })
    }

    async fn send(&self, chat: &str, id: &str, text: &str, file: Option<Attachment>) -> Result<()> {
        let jid = chat
            .parse::<Jid>()
            .map_err(|_| error(400, "WhatsApp chat is invalid."))?;
        let client = self.bot.client();
        let send = async {
            let message = if let Some(file) = file {
                let kind = match file.kind {
                    MediaKind::Document => MediaType::Document,
                    MediaKind::Image => MediaType::Image,
                    MediaKind::Audio => MediaType::Audio,
                };
                let upload = client
                    .upload(file.bytes, kind, UploadOptions::new())
                    .await
                    .map_err(|_| error(422, "WhatsApp attachment could not be uploaded."))?;
                match file.kind {
                    MediaKind::Image => media::image_message(
                        upload,
                        media::ImageOptions {
                            mimetype: Some(file.mime),
                            caption: Some(text.into()),
                            ..Default::default()
                        },
                    ),
                    MediaKind::Audio => media::audio_message(
                        upload,
                        media::AudioOptions {
                            ptt: Some(file.mime == "audio/ogg; codecs=opus"),
                            mimetype: Some(file.mime),
                            ..Default::default()
                        },
                    ),
                    MediaKind::Document => media::document_message(
                        upload,
                        media::DocumentOptions {
                            mimetype: Some(file.mime),
                            file_name: Some(file.name),
                            caption: Some(text.into()),
                            ..Default::default()
                        },
                    ),
                }
            } else {
                wa::Message::text(text)
            };
            {
                let db = lock(&self.events.state)?;
                if !epoch_active(&db.connection, &self.events.generation)?
                    || !exists(&db.connection, "SELECT 1 FROM whatsapp_outbox o JOIN whatsapp_inbox i ON i.id=o.input JOIN whatsapp_chats c ON c.chat=i.chat AND c.revision=i.route WHERE o.id=? AND o.status='sending'", [id])?
                {
                    return Err(error(422, "WhatsApp mapping changed before send."));
                }
            }
            let (tx, rx) = tokio::sync::oneshot::channel();
            *self.ack.lock().map_err(|_| unknown())? = Some((id.into(), tx));
            client
                .send_message_with_options(jid, message, SendOptions::default().with_message_id(id))
                .await
                .map_err(|_| unknown())?;
            if rx.await.map_err(|_| unknown())? {
                Ok(())
            } else {
                Err(error(422, "WhatsApp rejected the send."))
            }
        };
        let result = tokio::time::timeout(Duration::from_secs(35), send)
            .await
            .map_err(|_| unknown())
            .and_then(|result| result);
        if let Ok(mut state) = self.ack.lock() {
            state.take();
        }
        result
    }

    async fn close(self: Box<Self>, unlink: bool) {
        let _ = tokio::time::timeout(Duration::from_secs(3), async {
            if unlink {
                self.bot.client().logout().await;
            }
            self.bot.shutdown().await;
        })
        .await;
    }
}

pub(super) struct Input {
    pub chat: String,
    pub sender: String,
    pub message: String,
    pub label: String,
    pub text: String,
    pub quoted: Option<String>,
    pub media: Option<Vec<u8>>,
}

fn contact(value: &Jid) -> bool {
    matches!(
        value.server,
        whatsapp_rust::Server::Pn | whatsapp_rust::Server::Lid
    )
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub(super) fn parse(incoming: &InboundMessage) -> Option<Input> {
    let source = &incoming.info.source;
    if source.is_from_me
        || (!contact(&source.chat) && !source.chat.is_group())
        || !contact(&source.sender)
        || !valid_id(&incoming.info.id)
    {
        return None;
    }
    let sender = if source.sender.server == whatsapp_rust::Server::Lid {
        &source.sender
    } else {
        source
            .sender_alt
            .as_ref()
            .filter(|jid| jid.server == whatsapp_rust::Server::Lid)
            .unwrap_or(&source.sender)
    }
    .to_non_ad_string();
    let chat = if source.chat.is_group() {
        source.chat.to_non_ad_string()
    } else {
        sender.clone()
    };
    let message = incoming.message.get_base_message();
    let text = message
        .conversation
        .as_deref()
        .or_else(|| {
            message
                .extended_text_message
                .as_option()
                .and_then(|m| m.text.as_deref())
        })
        .or_else(|| {
            message
                .image_message
                .as_option()
                .and_then(|m| m.caption.as_deref())
        })
        .or_else(|| {
            message
                .document_message
                .as_option()
                .and_then(|m| m.caption.as_deref())
        })
        .unwrap_or("")
        .trim();
    let quoted = message
        .extended_text_message
        .as_option()
        .and_then(|message| message.context_info.as_option())
        .and_then(|context| context.stanza_id.as_ref())
        .filter(|id| valid_id(id))
        .cloned();
    let descriptor = if attachment(message).is_some() {
        let mut media = wa::Message {
            image_message: message.image_message.clone(),
            document_message: message.document_message.clone(),
            audio_message: message.audio_message.clone(),
            ..Default::default()
        };
        // Only use service-verified CDN hosts. Never use a peer-provided URL.
        if let Some(image) = media.image_message.as_option_mut() {
            image.url = None;
            image.context_info = Default::default();
        }
        if let Some(document) = media.document_message.as_option_mut() {
            document.url = None;
            document.context_info = Default::default();
        }
        if let Some(audio) = media.audio_message.as_option_mut() {
            audio.url = None;
            audio.context_info = Default::default();
        }
        Some(media.encode_to_vec()).filter(|bytes| bytes.len() <= 256 * 1024)
    } else {
        None
    };
    if (text.is_empty() && descriptor.is_none()) || text.encode_utf16().count() > 16_000 {
        return None;
    }
    let label = if source.chat.is_group() {
        format!("Group {}", source.chat.user)
    } else {
        let label: String = incoming
            .info
            .push_name
            .chars()
            .filter(|c| !c.is_control())
            .take(80)
            .collect();
        if label.trim().is_empty() {
            format!("Chat {}", source.sender.user)
        } else {
            label
        }
    };
    Some(Input {
        chat,
        sender,
        message: incoming.info.id.clone(),
        label,
        text: text.to_owned(),
        quoted,
        media: descriptor,
    })
}

fn attachment(message: &wa::Message) -> Option<(&dyn Downloadable, String, MediaKind)> {
    if let Some(image) = message.image_message.as_option() {
        let extension = match image.mimetype.as_deref() {
            Some("image/png") => "png",
            Some("image/webp") => "webp",
            _ => "jpg",
        };
        Some((
            image,
            format!("whatsapp-image.{extension}"),
            MediaKind::Image,
        ))
    } else if let Some(document) = message.document_message.as_option() {
        let name = document
            .file_name
            .as_deref()
            .filter(|name| {
                !name.trim().is_empty()
                    && !matches!(*name, "." | "..")
                    && name.encode_utf16().count() <= 200
                    && !name
                        .chars()
                        .any(|c| c.is_control() || c == '/' || c == '\\')
            })
            .unwrap_or("whatsapp-document");
        Some((document, name.into(), MediaKind::Document))
    } else {
        message.audio_message.as_option().map(|audio| {
            (
                audio as &dyn Downloadable,
                "whatsapp-voice.ogg".into(),
                MediaKind::Audio,
            )
        })
    }
}

pub(super) fn media_limit(kind: MediaKind) -> usize {
    match kind {
        MediaKind::Image => 4 * 1024 * 1024,
        MediaKind::Document | MediaKind::Audio => MAX_MEDIA,
    }
}

struct BoundedWriter(Cursor<Vec<u8>>, usize);
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.position().saturating_add(bytes.len() as u64) > self.1 as u64 {
            return Err(io::Error::other("WhatsApp media exceeds limit"));
        }
        self.0.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}
impl Seek for BoundedWriter {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.0.seek(position)
    }
}
impl DownloadWriter for BoundedWriter {
    fn truncate(&mut self, length: u64) -> io::Result<()> {
        self.0.get_mut().truncate(length as usize);
        Ok(())
    }
}
