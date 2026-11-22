mod changes;
mod controls;
mod files;
mod guide;
mod input;
mod panels;
mod protocol;
mod queue;
mod results;
mod routes;

use std::{collections::HashMap, time::Duration};

use gloo_net::http::{Request, Response};
use leptos::{leptos_dom::helpers::set_timeout, prelude::*};
use protocol::{Command, Partial, ReplyContext, MAX_FILES, MAX_MESSAGE};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use wasm_bindgen::{closure::Closure, prelude::wasm_bindgen, JsCast};
use web_sys::{EventSource, MessageEvent, Storage};

pub(crate) use panels::computer_window;
pub use routes::{standalone, Agent, Thread};

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct Icons {
    pub attach: String,
    pub send: String,
    pub mic: String,
    pub stop: String,
    pub chevron: String,
    pub panel: String,
    pub search: String,
    pub copy: String,
    pub fork: String,
    pub inspect: String,
    pub chats: String,
    pub files: String,
    pub computer: String,
    pub members: String,
    pub filter: String,
    pub close: String,
    pub settings: String,
    pub file: String,
    pub shield: String,
    pub more: String,
    pub external: String,
    pub folder: String,
    pub next: String,
    pub alert: String,
    pub back: String,
    pub download: String,
    pub globe: String,
    pub diff: String,
    pub branch: String,
    pub wrap: String,
}

#[derive(Clone, Debug)]
pub struct ConversationConfig {
    pub base: String,
    pub scope: String,
    pub name: String,
    pub agent: Option<Agent>,
    pub thread: Option<Thread>,
    pub members: Vec<Agent>,
    pub channel: bool,
    pub archived: bool,
    pub can_send: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct Segment {
    seq: u64,
    id: String,
    kind: String,
    text: String,
    #[serde(rename = "final")]
    finished: u8,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Activity {
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    stopped_at: Option<String>,
    #[serde(default)]
    calls: Vec<ProviderCall>,
    #[serde(default)]
    fallback_model: Option<String>,
    #[serde(default)]
    stream: Vec<Segment>,
    tools: Vec<Value>,
    harness: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct ProviderCall {
    created: String,
    finished: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
struct FileRef {
    id: String,
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    image: bool,
    #[serde(default)]
    deleted: bool,
    thread: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct PanelFileRequest {
    seq: u64,
    id: String,
    name: String,
    #[serde(rename = "openedAt")]
    opened_at: String,
    #[serde(default)]
    deleted: u8,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct Reply {
    id: String,
    title: String,
    height: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct Turn {
    id: String,
    #[serde(default)]
    created: String,
    seq: u64,
    text: String,
    status: String,
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    paused: u8,
    agent_name: Option<String>,
    agent: Option<String>,
    #[serde(rename = "computerWait")]
    computer_wait: Option<panels::ComputerWait>,
    parent: Option<String>,
    reply: Option<String>,
    error: Option<String>,
    partial: Option<Partial>,
    dispatch_label: Option<String>,
    #[serde(default)]
    editing: bool,
    #[serde(default)]
    files: Vec<FileRef>,
    #[serde(rename = "panelFile")]
    panel_file: Option<PanelFileRequest>,
    #[serde(rename = "generatedFiles", default)]
    generated: Vec<FileRef>,
    #[serde(default)]
    activity: Activity,
    #[serde(default)]
    approvals: Vec<Value>,
    #[serde(default, rename = "harnessApprovals")]
    harness_approvals: Vec<Value>,
    #[serde(default)]
    questions: Vec<Value>,
    #[serde(rename = "htmlReplies", default)]
    html: Vec<Reply>,
    #[serde(rename = "mcpApps", default)]
    apps: Vec<Reply>,
    #[serde(rename = "componentReplies", default)]
    components: Vec<Value>,
    #[serde(rename = "agentMessages", default)]
    exchanges: Vec<Value>,
    #[serde(default)]
    guide: Option<guide::Actions>,
    #[serde(rename = "receivedAgentReplies", default)]
    received: Vec<Value>,
    room: Option<Value>,
    #[serde(default, rename = "goalContinuation")]
    goal_continuation: bool,
}

impl Turn {
    fn active(&self) -> bool {
        matches!(self.status.as_str(), "running" | "queued")
    }

    fn needs_you(&self) -> bool {
        self.active()
            && (self.computer_wait.is_some()
                || self.questions.iter().any(|q| q["answer"].is_null())
                || self
                    .approvals
                    .iter()
                    .chain(&self.harness_approvals)
                    .any(|a| a["state"] == "approval"))
    }

    fn waiting_for_answer(&self) -> bool {
        self.questions
            .iter()
            .any(|question| question["answer"].is_null())
    }

    fn live_activity(&self) -> bool {
        self.active() && self.paused == 0 && !self.waiting_for_answer()
    }

    fn approval_subject(&self) -> Option<&'static str> {
        if self
            .approvals
            .iter()
            .any(|approval| approval["state"] == "approval")
        {
            return Some("connected app action");
        }
        self.harness_approvals
            .iter()
            .find(|approval| approval["state"] == "approval")
            .map(|approval| match approval["kind"].as_str() {
                Some("command") => "shell command",
                Some("file") => "file change",
                Some("network") => "network access request",
                Some("email") => "email draft",
                Some("form") => "form",
                _ => "tool request",
            })
    }

    fn streaming(&self) -> bool {
        self.status == "running"
            && self.paused == 0
            && self.partial.as_ref().is_some_and(|p| {
                !p.text.is_empty() && matches!(p.status.as_str(), "pending" | "received")
            })
    }
}

#[derive(Clone, Deserialize, PartialEq)]
struct Dispatch {
    id: String,
    agent: String,
    capabilities: Capabilities,
}

#[derive(Clone, Deserialize, PartialEq)]
struct Capabilities {
    steer: bool,
}

struct ApiError {
    message: String,
    definitive: bool,
}

async fn read<T: DeserializeOwned>(
    response: Result<Response, gloo_net::Error>,
) -> Result<T, ApiError> {
    let unavailable = || ApiError {
        message: "Minarbot connection lost. Try again.".into(),
        definitive: false,
    };
    let response = response.map_err(|_| unavailable())?;
    let status = response.status();
    let bytes = response.text().await.map_err(|_| unavailable())?;
    if response.ok() {
        return serde_json::from_str(&bytes).map_err(|_| ApiError {
            message: "Minarbot returned an invalid response.".into(),
            definitive: false,
        });
    }
    let body = serde_json::from_str::<Value>(&bytes).ok();
    let message = body
        .as_ref()
        .and_then(|value| {
            value["error"]
                .as_str()
                .or_else(|| value["error"]["message"].as_str())
        })
        .map(str::to_owned)
        .unwrap_or_else(|| format!("Request failed ({status})."));
    Err(ApiError {
        message,
        definitive: status < 500 || status == 503 && body.is_some(),
    })
}

async fn get<T: DeserializeOwned>(path: &str) -> Result<T, ApiError> {
    read(Request::get(path).send().await).await
}

async fn post<T: DeserializeOwned>(path: &str, raw: &str) -> Result<T, ApiError> {
    let body = Request::post(path)
        .header("Content-Type", "application/json")
        .body(raw)
        .map_err(|_| ApiError {
            message: "Request could not be created.".into(),
            definitive: false,
        })?;
    read(body.send().await).await
}

fn session() -> Option<Storage> {
    window().session_storage().ok().flatten()
}

fn uuid() -> Option<String> {
    Some(window().crypto().ok()?.random_uuid())
}

fn save(key: &str, value: &str) -> bool {
    session().is_some_and(|store| store.set_item(key, value).is_ok())
}

fn saved(key: &str) -> Option<String> {
    session()?.get_item(key).ok().flatten()
}

fn remove(key: &str) -> bool {
    session().is_some_and(|store| store.remove_item(key).is_ok())
}

#[derive(Clone)]
struct Pending {
    command: Command,
    raw: String,
}

#[derive(Clone, Copy)]
struct Chat {
    config: StoredValue<ConversationConfig>,
    draft: RwSignal<String>,
    files: RwSignal<Vec<String>>,
    context: RwSignal<Option<ReplyContext>>,
    pending: RwSignal<Option<Pending>>,
    blocked: RwSignal<bool>,
    busy: RwSignal<bool>,
    error: RwSignal<String>,
    read_error: RwSignal<String>,
    turns: RwSignal<Option<Vec<Turn>>>,
    active: RwSignal<Vec<Dispatch>>,
    addressed: RwSignal<String>,
    focus: RwSignal<String>,
    focus_card: StoredValue<String>,
    query: RwSignal<String>,
    before: RwSignal<Option<u64>>,
    tab: RwSignal<&'static str>,
    details: RwSignal<bool>,
    result: RwSignal<Option<results::ResultTarget>>,
    panel_upload: RwSignal<bool>,
    workspace: RwSignal<bool>,
    run: RwSignal<String>,
    new_thread: RwSignal<bool>,
    guide_connection: RwSignal<bool>,
    guide_ready: RwSignal<Option<bool>>,
}

impl Chat {
    fn new(config: ConversationConfig) -> Self {
        let key = |name| format!("minarbot:{name}:{}", config.scope);
        let raw = saved(&key("pending"));
        let pending = raw.and_then(|raw| {
            Some(Pending {
                command: protocol::restore(&raw, config.channel)?,
                raw,
            })
        });
        let blocked = pending.is_none() && saved(&key("pending")).is_some();
        let focus_key = key("focus");
        let focus = saved(&focus_key)
            .filter(|id| protocol::id(id))
            .unwrap_or_default();
        let _ = remove(&focus_key);
        let card_key = key("focus-card");
        let focus_card = saved(&card_key).unwrap_or_default();
        let _ = remove(&card_key);
        let files = saved(&key("attachments"))
            .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
            .filter(|files| files.len() <= MAX_FILES && files.iter().all(|id| protocol::id(id)))
            .unwrap_or_default();
        let result = results::current().ok().flatten();
        let stored_tab = window()
            .local_storage()
            .ok()
            .flatten()
            .and_then(|storage| storage.get_item(&key("right-panel")).ok().flatten())
            .and_then(|raw| serde_json::from_str::<String>(&raw).ok());
        let tab = match stored_tab.as_deref() {
            Some("files") => "files",
            Some("diff") => "diff",
            Some("preview") => "preview",
            Some("computer" | "browser") => "computer",
            _ => "chats",
        };
        Self {
            draft: RwSignal::new(saved(&key("draft")).unwrap_or_default()),
            files: RwSignal::new(files),
            context: RwSignal::new(
                pending
                    .as_ref()
                    .and_then(|pending| pending.command.html_context.clone()),
            ),
            pending: RwSignal::new(pending),
            blocked: RwSignal::new(blocked),
            busy: RwSignal::new(false),
            error: RwSignal::new(String::new()),
            read_error: RwSignal::new(String::new()),
            turns: RwSignal::new(None),
            active: RwSignal::new(Vec::new()),
            addressed: RwSignal::new(String::new()),
            focus: RwSignal::new(focus),
            focus_card: StoredValue::new(focus_card),
            query: RwSignal::new(String::new()),
            before: RwSignal::new(None),
            tab: RwSignal::new(tab),
            details: RwSignal::new(result.is_some() || restore_details()),
            result: RwSignal::new(result),
            panel_upload: RwSignal::new(false),
            workspace: RwSignal::new(false),
            run: RwSignal::new(String::new()),
            new_thread: RwSignal::new(false),
            guide_connection: RwSignal::new(false),
            guide_ready: RwSignal::new(None),
            config: StoredValue::new(config),
        }
    }

    fn key(self, name: &str) -> String {
        self.config
            .with_value(|config| format!("minarbot:{name}:{}", config.scope))
    }

    fn base(self) -> String {
        self.config.with_value(|config| config.base.clone())
    }

    fn locked(self) -> bool {
        self.blocked.get()
            || self.panel_upload.get()
            || self.busy.get()
            || self.pending.get().is_some()
            || self.config.with_value(|config| config.archived)
    }

    fn target(self) -> Option<Dispatch> {
        let channel = self.config.with_value(|config| config.channel);
        self.active.with(|active| {
            active
                .iter()
                .find(|run| !channel || self.addressed.with(|id| *id == run.id))
                .cloned()
        })
    }

    fn can_send(self) -> bool {
        if let Some(ready) = self.guide_ready.get() {
            return ready;
        }
        self.config.with_value(|config| {
            if config.channel {
                if let Some(target) = self.target() {
                    return config
                        .members
                        .iter()
                        .any(|agent| agent.id == target.agent && agent.runnable);
                }
            }
            config.can_send
        })
    }

    fn draft(self, text: String) {
        if !save(&self.key("draft"), &text) {
            self.error
                .set("Draft was not saved. Copy your message before leaving.".into());
        }
        self.draft.set(text);
    }

    fn attach(self, files: Vec<String>) {
        if files.len() > MAX_FILES {
            self.error.set("Attach up to four files.".into());
            return;
        }
        if serde_json::to_string(&files).is_ok_and(|raw| save(&self.key("attachments"), &raw)) {
            self.files.set(files);
        } else {
            self.error
                .set("Attachment selection could not be saved.".into());
        }
    }

    fn finish(self) {
        // Clear the draft first. If removal fails, the same pending ID stays safe to retry.
        for key in ["draft", "attachments", "pending"] {
            if !remove(&self.key(key)) {
                self.error
                    .set("Message received. Browser recovery data could not be cleared.".into());
                return;
            }
        }
        self.pending.set(None);
        self.draft.set(String::new());
        self.files.set(Vec::new());
        self.context.set(None);
        self.addressed.set(String::new());
        self.error.set(String::new());
    }

    fn send(self, steer: bool) {
        if self.busy.get_untracked() || self.blocked.get_untracked() {
            return;
        }
        let config = self.config.get_value();
        if config.archived {
            return;
        }
        let pending = if let Some(pending) = self.pending.get_untracked() {
            pending
        } else {
            if !self.can_send() {
                return;
            }
            let Some(id) = uuid() else {
                self.error.set("Could not create a message ID.".into());
                return;
            };
            let files = self.files.get_untracked();
            let target = self.target();
            let context = self.context.get_untracked();
            let steer = steer
                && context.is_none()
                && target.as_ref().is_some_and(|run| run.capabilities.steer);
            let command = Command {
                id,
                text: self.draft.get_untracked().trim().into(),
                files: (!files.is_empty()).then_some(files),
                recipient: if config.channel {
                    target.as_ref().map(|run| run.agent.clone())
                } else {
                    None
                },
                automatic: None,
                dispatch_mode: steer.then(|| "steer".into()),
                target_turn: if steer {
                    target.map(|run| run.id)
                } else {
                    None
                },
                html_context: context,
            };
            if !command.valid(config.channel) {
                self.error
                    .set("Enter a valid message or attach a file.".into());
                return;
            }
            let Ok(raw) = serde_json::to_string(&command) else {
                return;
            };
            Pending { raw, command }
        };
        let key = self.key("pending");
        if !save(&key, &pending.raw) {
            self.error.set(
                "Message status could not be saved. Enable browser storage before sending.".into(),
            );
            return;
        }
        self.busy.set(true);
        self.error.set(String::new());
        self.pending.set(Some(pending.clone()));
        self.query.set(String::new());
        self.focus.set(String::new());
        self.before.set(None);
        let path = format!("{}/turns", config.base);
        leptos::task::spawn_local(async move {
            let result = post::<Value>(&path, &pending.raw).await;
            if self.busy.is_disposed() {
                return;
            }
            match result {
                Ok(_) => self.finish(),
                Err(error) => {
                    if error.definitive && remove(&key) {
                        self.pending.set(None);
                    }
                    self.error.set(error.message);
                }
            }
            self.busy.set(false);
        });
    }

    fn set_details(self, open: bool) {
        if !open {
            results::clear(self);
        }
        if let Ok(Some(storage)) = window().local_storage() {
            let _ = storage.set_item("minarbot:details-open", if open { "1" } else { "0" });
        }
        self.details.set(open);
    }

    fn inspect(self, turn: String) {
        self.run.set(turn);
        self.select_tab("run");
        self.set_details(true);
    }

    fn select_tab(self, tab: &'static str) {
        self.tab.set(tab);
        results::clear(self);
        if tab != "run" {
            if let Ok(Some(storage)) = window().local_storage() {
                let _ = storage.set_item(&self.key("right-panel"), &format!("\"{tab}\""));
            }
        }
    }

    fn send_from_app(self, text: &str) {
        if self.locked() || !self.can_send() {
            return;
        }
        let Some(id) = uuid() else { return };
        let key = self.key("mcp-app-send");
        let mut command = Command {
            id,
            text: text.trim().into(),
            files: None,
            recipient: None,
            automatic: None,
            dispatch_mode: None,
            target_turn: None,
            html_context: self.context.get_untracked(),
        };
        let channel = self.config.with_value(|config| config.channel);
        if !command.valid(channel) {
            self.error.set("App message could not be sent.".into());
            return;
        }
        let raw = if let Some(raw) = saved(&key) {
            let Some(previous) = protocol::restore(&raw, channel) else {
                self.error
                    .set("App message recovery could not be read.".into());
                return;
            };
            command.id.clone_from(&previous.id);
            if previous != command {
                self.error
                    .set("Retry the previous app message to resolve its outcome.".into());
                return;
            }
            raw
        } else {
            let Ok(raw) = serde_json::to_string(&command) else {
                return;
            };
            raw
        };
        if !save(&key, &raw) {
            self.error
                .set("App message recovery could not be saved.".into());
            return;
        }
        let path = format!("{}/turns", self.base());
        self.busy.set(true);
        leptos::task::spawn_local(async move {
            let result = post::<Value>(&path, &raw).await;
            if self.busy.is_disposed() {
                return;
            }
            match result {
                Ok(_) => {
                    let _ = remove(&key);
                    if self
                        .context
                        .with_untracked(|context| *context == command.html_context)
                    {
                        self.context.set(None);
                    }
                }
                Err(reason) => {
                    if reason.definitive {
                        let _ = remove(&key);
                    }
                    self.error.set(reason.message);
                }
            }
            self.busy.set(false);
        });
    }
}

fn restore_details() -> bool {
    let viewport = window()
        .inner_width()
        .ok()
        .and_then(|width| width.as_f64())
        .unwrap_or_default();
    let storage = window().local_storage().ok().flatten();
    let preference = |name| {
        storage
            .as_ref()
            .and_then(|store| store.get_item(name).ok().flatten())
    };
    let sidebar = if preference("minarbot:sidebar-collapsed").as_deref() == Some("1") {
        0.0
    } else {
        300.0
    };
    viewport - sidebar >= 920.0
        && preference("minarbot:details-open").map_or(viewport >= 1280.0, |open| open == "1")
}

fn turn_query(chat: Chat) -> String {
    let base = chat.base();
    let focus = chat.focus.get_untracked();
    let query = chat.query.get_untracked();
    let before = chat.before.get_untracked();
    if protocol::id(&focus) {
        return format!("{base}/turns?focus={focus}");
    }
    let Ok(params) = web_sys::UrlSearchParams::new() else {
        return format!("{base}/turns");
    };
    if let Some(before) = before {
        params.set("before", &before.to_string());
    }
    if !query.is_empty() {
        params.set("q", &query);
    }
    let query = params.to_string().as_string().unwrap_or_default();
    format!("{base}/turns?{query}")
}

fn polling(chat: Chat, snapshots: StoredValue<HashMap<String, Partial>>) {
    if chat.turns.is_disposed() {
        return;
    }
    let path = turn_query(chat);
    leptos::task::spawn_local(async move {
        let result = get::<Vec<Turn>>(&path).await;
        if chat.turns.is_disposed() {
            return;
        }
        let mut delay = Duration::from_millis(1200);
        if path == turn_query(chat) {
            match result {
                Ok(mut rows) => {
                    snapshots.update_value(|snapshots| {
                        for turn in &mut rows {
                            if turn.status == "running" {
                                if let Some(incoming) = snapshots.get(&turn.id) {
                                    turn.partial = Some(protocol::newest(
                                        turn.partial.take(),
                                        incoming.clone(),
                                    ));
                                }
                            } else if turn.status != "queued" {
                                snapshots.remove(&turn.id);
                            }
                        }
                    });
                    if !chat.busy.get_untracked()
                        && chat.pending.with_untracked(|pending| {
                            pending.as_ref().is_some_and(|pending| {
                                rows.iter().any(|turn| turn.id == pending.command.id)
                            })
                        })
                    {
                        chat.finish();
                    }
                    if !chat.busy.get_untracked()
                        && saved(&chat.key("mcp-app-send"))
                            .and_then(|raw| {
                                protocol::restore(
                                    &raw,
                                    chat.config.with_value(|config| config.channel),
                                )
                            })
                            .is_some_and(|command| rows.iter().any(|turn| turn.id == command.id))
                    {
                        let _ = remove(&chat.key("mcp-app-send"));
                    }
                    if rows.iter().any(Turn::active) || chat.pending.with_untracked(Option::is_some)
                    {
                        delay = Duration::from_millis(400);
                    }
                    if chat.turns.with_untracked(|old| old.as_ref() != Some(&rows)) {
                        chat.turns.set(Some(rows));
                    }
                    chat.read_error.set(String::new());
                }
                Err(error) => chat.read_error.set(error.message),
            }
        } else {
            delay = Duration::ZERO;
        }
        let result = get::<Vec<Dispatch>>(&format!("{}/dispatch", chat.base())).await;
        if chat.turns.is_disposed() {
            return;
        }
        if let Ok(active) = result {
            if chat.active.with_untracked(|old| *old != active) {
                chat.active.set(active);
            }
        }
        set_timeout(move || polling(chat, snapshots), delay);
    });
}

fn subscribe(chat: Chat, snapshots: StoredValue<HashMap<String, Partial>>) {
    let Ok(source) = EventSource::new(&format!("{}/turns/stream", chat.base())) else {
        return;
    };
    let callback = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        let Some(snapshot) = event
            .data()
            .as_string()
            .and_then(|raw| protocol::parse_snapshot(&raw))
        else {
            return;
        };
        let id = snapshot.id;
        let Some(partial) = snapshots.try_update_value(|entries| {
            let partial = protocol::newest(entries.remove(&id), snapshot.partial);
            entries.insert(id.clone(), partial.clone());
            partial
        }) else {
            return;
        };
        if !chat.focus.get_untracked().is_empty()
            || !chat.query.get_untracked().is_empty()
            || chat.before.get_untracked().is_some()
        {
            return;
        }
        if chat.turns.with_untracked(|rows| {
            rows.as_ref().is_some_and(|rows| {
                rows.iter().any(|turn| {
                    turn.id == id
                        && turn.status == "running"
                        && turn.partial.as_ref() != Some(&partial)
                })
            })
        }) {
            chat.turns.update(|rows| {
                if let Some(turn) = rows
                    .as_mut()
                    .and_then(|rows| rows.iter_mut().find(|turn| turn.id == id))
                {
                    turn.partial = Some(protocol::newest(turn.partial.take(), partial));
                }
            });
        }
    });
    source.set_onmessage(Some(callback.as_ref().unchecked_ref()));
    let handle = StoredValue::new_local((source, callback));
    on_cleanup(move || {
        handle.with_value(|(source, _)| {
            source.set_onmessage(None);
            source.close();
        })
    });
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = MinarbotConversation)]
    fn markdown(text: &str, scope: &str, files: Option<String>) -> String;

    #[wasm_bindgen(js_namespace = MinarbotConversation, js_name = messageTime)]
    fn message_time(value: &str) -> String;

    #[wasm_bindgen(js_namespace = MinarbotConversation, catch, js_name = writeClipboard)]
    async fn write_clipboard(text: &str) -> Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>;

    #[wasm_bindgen(js_namespace = MinarbotConversation, js_name = markdownBlocks)]
    fn markdown_blocks(text: &str) -> js_sys::Array;
}

fn body(text: String, base: String) -> impl IntoView {
    view! { <div class="markdown" inner_html=markdown(&text, &base, None)></div> }
}

fn activity_label(turn: &Turn, now: f64) -> String {
    if turn.active() && turn.waiting_for_answer() {
        return "Waiting for your answer".into();
    }
    if turn.active() && turn.paused != 0 {
        return "Paused".into();
    }
    let calls = &turn.activity.calls;
    let start = turn
        .activity
        .started_at
        .as_deref()
        .or_else(|| calls.first().map(|call| call.created.as_str()))
        .or((turn.status != "cancelled").then_some(turn.created.as_str()));
    let end = if turn.status == "cancelled" {
        turn.activity.stopped_at.as_deref().map(js_sys::Date::parse)
    } else if turn.live_activity() {
        Some(now)
    } else {
        calls
            .last()
            .and_then(|call| call.finished.as_deref())
            .map(js_sys::Date::parse)
    };
    let duration = end.zip(start).and_then(|(end, start)| {
        let elapsed = (end - js_sys::Date::parse(start)) / 1000.0;
        elapsed
            .is_finite()
            .then(|| match elapsed.max(0.0).floor() as u64 {
                0 => "<1s".to_owned(),
                seconds @ 1..=59 => format!("{seconds}s"),
                seconds => format!("{}m {}s", seconds / 60, seconds % 60),
            })
    });
    let (short, long) = match turn.status.as_str() {
        "done" => ("Worked", "Worked for"),
        "cancelled" => ("Stopped", "Stopped after"),
        "failed" => ("Failed", "Failed after"),
        "interrupted" => ("Interrupted", "Interrupted after"),
        "queued" => return "Queued".into(),
        _ => ("Working…", "Working…"),
    };
    duration.map_or_else(|| short.into(), |time| format!("{long} {time}"))
}

fn attachment(file: FileRef, base: String) -> impl IntoView {
    let path = format!("{base}/files/{}", file.id);
    let name = file.name;
    let download = name.clone();
    view! { <a class="k-link" href=path download=download>{name}</a> }
}

fn copy_action(text: impl Fn() -> String + 'static, label: &'static str) -> impl IntoView {
    let icon = use_context::<Icons>().unwrap_or_default().copy;
    let feedback = RwSignal::new("");
    view! {
        <button type="button" class="k-icon-btn" aria-label=label title=label on:click=move |_| {
            let text = text();
            leptos::task::spawn_local(async move {
                match write_clipboard(&text).await {
                    Ok(_) => { feedback.try_set("Copied"); }
                    Err(_) => { feedback.try_set("Text could not be copied."); }
                }
                set_timeout(move || { feedback.try_set(""); }, Duration::from_secs(2));
            });
        }><span class="appearance-icon" inner_html=icon></span></button>
        {move || match feedback.get() {
            "" => None,
            "Copied" => Some(view! { <span class="sr-only" role="status">"Copied"</span> }.into_any()),
            reason => Some(view! { <span class="message-error" role="alert">{reason}</span> }.into_any()),
        }}
    }
}

pub(super) fn answer_actions(chat: Chat, row: RwSignal<Turn>) -> impl IntoView {
    let icons = use_context::<Icons>().unwrap_or_default();
    let turn_id = row.with_untracked(|turn| turn.id.clone());
    let endpoint = format!("{}/turns/{turn_id}/fork", chat.base());
    let key = format!("minarbot:fork:{turn_id}");
    let finished = move || {
        row.with(|turn| {
            turn.activity
                .calls
                .last()
                .and_then(|call| call.finished.clone())
                .unwrap_or_else(|| turn.created.clone())
        })
    };
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    view! {
        <div class="answer-actions" role="group" aria-label="Reply actions"
            hidden=move || !row.with(|turn| matches!(turn.status.as_str(), "done" | "cancelled" | "failed" | "interrupted"))>
            {copy_action(move || row.with(|turn| turn.reply.clone().or_else(|| turn.partial.as_ref().map(|partial| partial.text.clone())).or_else(|| turn.error.clone()).unwrap_or_default()), "Copy reply")}
            <button type="button" class="k-icon-btn" aria-label="Fork from here" title="Fork from here" disabled=busy on:click=move |_| {
                if busy.get_untracked() { return }
                let Some(id) = saved(&key).or_else(uuid).filter(|id| protocol::id(id)) else { error.set("Fork recovery could not be read.".into()); return };
                if !save(&key, &id) { error.set("Fork request could not be saved.".into()); return }
                let endpoint = endpoint.clone();
                let key = key.clone();
                busy.set(true);
                leptos::task::spawn_local(async move {
                    match post::<Thread>(&endpoint, &serde_json::json!({"id": id}).to_string()).await {
                        Ok(thread) if protocol::id(&thread.agent) && protocol::id(&thread.id) => {
                            let _ = remove(&key);
                            let _ = window().location().set_href(&routes::link(&format!("/agents/{}/conversations/{}", thread.agent, thread.id)));
                        }
                        Ok(_) => { error.try_set("Fork response was invalid.".into()); }
                        Err(reason) => { error.try_set(reason.message); }
                    }
                    busy.try_set(false);
                });
            }><span class="appearance-icon" inner_html=icons.fork.clone()></span></button>
            <time class="message-time" datetime=finished>{move || message_time(&finished())}</time>
            {move || (!error.with(String::is_empty)).then(|| view! { <span class="message-error" role="alert">{error}</span> })}
        </div>
    }
}

fn turn_view(chat: Chat, row: RwSignal<Turn>, id: String, latest: Memo<bool>) -> impl IntoView {
    let inspect_id = id.clone();
    let icons = use_context::<Icons>().unwrap_or_default();
    let now = RwSignal::new(js_sys::Date::now());
    Effect::new(move |_| {
        if row.with(Turn::live_activity) {
            let interval = set_interval_with_handle(
                move || now.set(js_sys::Date::now()),
                Duration::from_secs(1),
            )
            .ok();
            on_cleanup(move || {
                if let Some(interval) = interval {
                    interval.clear();
                }
            });
        }
    });
    let base = chat.base();
    let text_base = base.clone();
    let label = move || {
        if row.with(Turn::streaming) {
            "Streaming answer"
        } else {
            "Agent reply"
        }
    };
    let working = move || row.with(|turn| turn.status == "running" && turn.live_activity());
    let failed = move || row.with(|turn| matches!(turn.status.as_str(), "failed" | "interrupted"));
    let inspect = move |_| chat.inspect(inspect_id.clone());
    let inspect_icon = StoredValue::new(icons.inspect.clone());
    let inspect = Callback::new(inspect);
    let computer_wait = Memo::new(move |_| {
        row.with(|turn| {
            let wait = turn.computer_wait.clone()?;
            let agent = turn.agent.clone()?;
            let name = turn
                .agent_name
                .clone()
                .unwrap_or_else(|| chat.config.with_value(|config| config.name.clone()));
            Some((wait, agent, name))
        })
    });
    let answer = Memo::new(move |_| {
        row.with(|turn| {
            turn.reply
                .clone()
                .or_else(|| {
                    (turn.streaming() || turn.status == "cancelled")
                        .then_some(turn.partial.as_ref())
                        .flatten()
                        .map(|partial| partial.text.clone())
                })
                .unwrap_or_default()
        })
    });
    let outputs = Memo::new(move |_| row.with(|turn| turn.generated.clone()));
    let blocks = Memo::new(move |_| {
        answer.with(|text| {
            if !outputs.with(Vec::is_empty) {
                return vec![(0, text.clone())];
            }
            markdown_blocks(text)
                .iter()
                .enumerate()
                .filter_map(|(index, block)| Some((index, block.as_string()?)))
                .collect::<Vec<_>>()
        })
    });
    let activity = Memo::new(move |_| {
        row.with(|turn| {
            let mut steps = turn
                .activity
                .stream
                .iter()
                .filter(|step| {
                    step.finished == 0
                        && !step.text.is_empty()
                        && (step.kind == "tool"
                            || !turn.partial.as_ref().is_some_and(|p| {
                                p.seq == step.seq && p.segment.as_deref() == Some(step.id.as_str())
                            }))
                })
                .map(|step| (step.kind == "text", step.text.clone()))
                .collect::<Vec<_>>();
            for tool in turn.activity.tools.iter().chain(&turn.activity.harness) {
                let name = tool["name"].as_str().unwrap_or_default();
                if name.is_empty()
                    || matches!(
                        name,
                        "userMessage"
                            | "agentMessage"
                            | "assistantMessage"
                            | "reasoning"
                            | "plan"
                            | "contextCompaction"
                            | "verificationReminder"
                    )
                {
                    continue;
                }
                let title = tool["title"]
                    .as_str()
                    .filter(|title| !title.is_empty())
                    .unwrap_or(name);
                if !steps
                    .iter()
                    .any(|(prose, text)| !prose && (text == name || text == title))
                {
                    steps.push((false, title.to_owned()));
                }
            }
            steps
        })
    });
    let prompt =
        Memo::new(move |_| row.with(|turn| turn.parent.is_none().then(|| turn.text.clone())));
    view! {
        <article class="turn-block" class:is-latest=move || latest.get() id=format!("turn-{id}") tabindex="0">
            <div class="human-message" tabindex="-1" aria-label="Your message"
                hidden=move || row.with(|turn| turn.goal_continuation || turn.parent.is_some())>
                <div class="human-bubble message-plain" hidden=move || prompt.with(|text| text.as_ref().is_none_or(|text| text.trim().is_empty()))>{prompt}</div>
                {move || row.with(|turn| turn.files.iter().filter(|file| !file.deleted).cloned().map(|file| attachment(file, base.clone())).collect_view())}
                <div class="turn-meta human-meta">
                    <time class="message-time" datetime=move || row.with(|turn| turn.created.clone())>{move || row.with(|turn| message_time(&turn.created))}</time>
                    {copy_action(move || row.with_untracked(|turn| turn.text.clone()), "Copy message")}
                </div>
            </div>
            <Show when=move || row.with(|turn| turn.goal_continuation)>
                <p class="message-muted goal-continuation">"Goal continued"</p>
            </Show>
            <div class="agent-message">
                <div class="message-column">
                    <div class="agent-identity" hidden=move || chat.config.with_value(|config| !config.channel || config.members.len() < 2)>
                        <strong class="message-name">{move || row.with(|turn| turn.agent_name.clone()).unwrap_or_else(|| chat.config.with_value(|c| c.name.clone()))}</strong>
                    </div>
                    <div class="turn-activity">
                        {move || {
                            if activity.with(Vec::is_empty) {
                                view! {
                                    <div class="turn-activity__header">
                                        <span class="activity-label">
                                            <span class="activity-working" aria-hidden="true" hidden=move || !working()></span>
                                            {move || row.with(|turn| activity_label(turn, now.get()))}
                                        </span>
                                        <button type="button" class="link-btn link-btn--small turn-activity__inspect" aria-label="Inspect run" hidden=move || !failed() on:click=move |event| inspect.run(event)>"Inspect"</button>
                                    </div>
                                }.into_any()
                            } else {
                                view! {
                                    <details class="turn-activity__details">
                                        <summary class="turn-activity__header">
                                            <span class="k-collapsible__trigger">
                                            <span class="activity-label">
                                                <span class="activity-working" aria-hidden="true" hidden=move || !working()></span>
                                                {move || row.with(|turn| activity_label(turn, now.get()))}
                                            </span>
                                            <span class="appearance-icon k-collapsible__chev" inner_html=icons.chevron.clone()></span>
                                            </span>
                                        </summary>
                                        <ol class="activity-list" aria-label="Turn activity">
                                            {move || activity.get().into_iter().map(|(prose, text)| {
                                                if prose {
                                                    view! { <li><div class="legacy">{body(text, chat.base())}</div></li> }.into_any()
                                                } else {
                                                    view! { <li><span>{text}</span></li> }.into_any()
                                                }
                                            }).collect_view()}
                                        </ol>
                                    </details>
                                }.into_any()
                            }
                        }}
                        <button type="button" class="link-btn link-btn--small conversation-inspect" aria-label="Inspect run" title="Inspect run"
                            hidden=move || failed() && activity.with(Vec::is_empty) on:click=move |event| inspect.run(event)>
                            <span class="appearance-icon" inner_html=inspect_icon.get_value()></span>
                        </button>
                    </div>
                    <div class="message-body" hidden=move || answer.with(String::is_empty) && outputs.with(Vec::is_empty) && row.with(|turn| turn.html.is_empty() && turn.apps.is_empty() && turn.components.is_empty() && turn.error.is_none() && turn.dispatch_label.is_none() && turn.approval_subject().is_none())>
                        {panels::rich_replies(chat, row)}
                        {move || row.with(|turn| turn.dispatch_label.clone()).map(|text| view! { <p class="dispatch-receipt">{text}</p> })}
                        <div aria-label=label hidden=move || answer.with(String::is_empty) && outputs.with(Vec::is_empty)>
                            <div class="legacy stream-markdown">
                                <For each=move || blocks.get() key=|(index, block): &(usize, String)| (*index, block.clone())
                                    children=move |(_, block)| {
                                        let scope = text_base.clone();
                                        view! { <div class="markdown" inner_html=move || markdown(&block, &scope, serde_json::to_string(&outputs.get()).ok())></div> }
                                    } />
                            </div>
                        </div>
                        {move || row.with(|turn| {
                            if let Some(error) = turn.error.clone() {
                                view! { <p class="message-error"><span class="appearance-icon" inner_html=icons.alert.clone()></span>{error}</p> }.into_any()
                            } else {
                                turn.approval_subject().filter(|subject| *subject != "form").map(|subject| view! {
                                    <p class="message-muted">{format!("Review the {subject} to continue.")}</p>
                                }).into_any()
                            }
                        })}
                    </div>
                    {move || row.with(|turn| {
                        let subject = turn.approval_subject();
                        let label = if turn.active() && subject == Some("form") { "Waiting for your input" }
                            else if turn.active() && subject.is_some() { "Waiting for approval" }
                            else if turn.active() && turn.waiting_for_answer() { "Waiting for your answer" }
                            else if turn.active() && turn.paused != 0 { "Paused" } else { "" };
                        let text = match (label, turn.activity.fallback_model.is_some()) {
                            ("", true) => "Fallback model used".into(),
                            (_, true) => format!("{label} · Fallback model used"),
                            (_, false) => label.into(),
                        };
                        (!label.is_empty() || turn.activity.fallback_model.is_some()).then(|| view! {
                            <div class="turn-meta agent-meta"><span class="turn-status" class:is-attention=subject == Some("form") || turn.waiting_for_answer() || turn.paused != 0
                                class:is-working=subject.is_some_and(|value| value != "form")><i aria-hidden="true"></i>{text}</span></div>
                        })
                    })}
                    {move || computer_wait.get().map(|(wait, agent, name)| panels::computer_request(agent, name, wait))}
                    {panels::exchanges(chat, row)}
                    {controls::turn_controls(chat, row)}
                    {queue::editor(chat, row)}
                </div>
            </div>
        </article>
    }
}

fn avatar(agent: Option<Agent>, name: &str, channel: bool, size: u8) -> impl IntoView {
    let image = agent
        .as_ref()
        .filter(|agent| agent.avatar_revision > 0 && protocol::id(&agent.id))
        .map(|agent| {
            format!(
                "/api/agents/{}/avatar/image?v={}",
                agent.id, agent.avatar_revision
            )
        });
    let initial = if channel {
        "#".into()
    } else {
        agent
            .as_ref()
            .map_or(name, |agent| agent.name.as_str())
            .chars()
            .next()
            .map_or_else(String::new, |letter| letter.to_uppercase().collect())
    };
    view! {
        <span class="avatar bs-avatar" class:is-channel=channel style:width=format!("{size}px") style:height=format!("{size}px") style:font-size=format!("{}px", (size as f32 * 0.42).round()) aria-hidden="true">
            {image.map(|src| view! { <img src=src alt="" /> }.into_any()).unwrap_or_else(|| initial.into_any())}
        </span>
    }
}

fn header(chat: Chat, config: ConversationConfig, searching: RwSignal<bool>) -> impl IntoView {
    let icons = use_context::<Icons>().unwrap_or_default();
    let menu = NodeRef::<leptos::html::Details>::new();
    let trigger = NodeRef::<leptos::html::Summary>::new();
    let title = config.name.clone();
    let workspace = config.agent.as_ref().is_some_and(|agent| !agent.builtin);
    let builtin = config.agent.as_ref().is_some_and(|agent| agent.builtin);
    let toggle_details = move || chat.set_details(!chat.details.get_untracked());
    let keys = window_event_listener(leptos::ev::keydown, move |event| {
        if !(event.ctrl_key() || event.meta_key())
            || document()
                .query_selector("dialog:modal")
                .ok()
                .flatten()
                .is_some()
        {
            return;
        }
        if event.shift_key() && event.key().eq_ignore_ascii_case("b") {
            event.prevent_default();
            toggle_details();
        } else if event.key().eq_ignore_ascii_case("f") {
            event.prevent_default();
            searching.set(true);
        }
    });
    on_cleanup(move || keys.remove());
    view! {
        <header class="chat__header">
            <details class="chat__pill-anchor chat-pilot-menu" node_ref=menu on:keydown=move |event| {
                if event.key() == "Escape" && menu.get_untracked().is_some_and(|menu| menu.open()) {
                    event.prevent_default();
                    if let Some(menu) = menu.get_untracked() { menu.set_open(false); }
                    if let Some(trigger) = trigger.get_untracked() { let _ = trigger.focus(); }
                }
            }>
                <summary class="chat__pill" node_ref=trigger>
                    {avatar(config.agent, &config.name, config.channel, 24)}
                    <span class="chat__pill-name">{title.clone()}</span>
                    <span class="appearance-icon chat__pill-chev" inner_html=icons.chevron></span>
                </summary>
                <nav class="k-pop chat-pilot-menu__items" aria-label=format!("Menu {title}")>
                    {builtin.then(|| view! {
                        <button type="button" class="k-menu__item" on:click=move |_| {
                            if let Some(menu) = menu.get_untracked() { menu.set_open(false); }
                            chat.guide_connection.set(true);
                        }>"AI connection"</button>
                    })}
                    <button type="button" class="k-menu__item" on:click=move |_| {
                        if let Some(menu) = menu.get_untracked() { menu.set_open(false); }
                        searching.set(true);
                    }><span class="appearance-icon" inner_html=icons.search></span>"Find messages"</button>
                    {workspace.then(|| view! {
                        <button type="button" class="k-menu__item" on:click=move |_| {
                            if let Some(menu) = menu.get_untracked() { menu.set_open(false); }
                            chat.workspace.set(true);
                        }><span class="appearance-icon" inner_html=icons.folder.clone()></span>"Workspace files"</button>
                    })}
                    <button type="button" class="k-menu__item" on:click=move |_| {
                        if let Some(menu) = menu.get_untracked() { menu.set_open(false); }
                        toggle_details();
                        if let Some(trigger) = trigger.get_untracked() { let _ = trigger.focus(); }
                    }><span class="appearance-icon" inner_html=icons.panel.clone()></span>
                      {move || if chat.details.get() { "Hide panel" } else { "Show panel" }}</button>
                </nav>
            </details>
            <div class="chat__header-right">
                <button type="button" class="icon-btn icon-btn--bordered" aria-label="Show details"
                    title="Show details (Ctrl/Cmd+Shift+B)" aria-keyshortcuts="Control+Shift+B Meta+Shift+B"
                    hidden=move || chat.details.get() on:click=move |_| chat.set_details(true)>
                    <span class="appearance-icon" inner_html=icons.panel></span>
                </button>
            </div>
        </header>
    }
}

/// Mount inside the conversation region of the Leptos shell.
/// Key this component by conversation scope when changing routes.
pub fn shell(config: ConversationConfig) -> impl IntoView {
    let chat = Chat::new(config.clone());
    let workspace = StoredValue::new(
        config
            .agent
            .as_ref()
            .map(|agent| (agent.id.clone(), agent.name.clone())),
    );
    results::connect(chat);
    let guide = config.agent.clone().filter(|agent| agent.builtin);
    let snapshots = StoredValue::new(HashMap::new());
    subscribe(chat, snapshots);
    polling(chat, snapshots);
    let create = window_event_listener(
        leptos::ev::Custom::<web_sys::Event>::new("minarbot:new-conversation"),
        move |_| {
            if chat.config.with_value(|config| {
                config
                    .agent
                    .as_ref()
                    .is_some_and(|agent| !agent.builtin && !agent.hidden)
            }) {
                chat.new_thread.set(true);
                chat.tab.set("chats");
                chat.set_details(true);
            }
        },
    );
    on_cleanup(move || create.remove());
    let new_chat = window_event_listener(leptos::ev::keydown, move |event| {
        if (event.ctrl_key() || event.meta_key())
            && event.shift_key()
            && event.key().eq_ignore_ascii_case("o")
            && document()
                .query_selector("dialog:modal")
                .ok()
                .flatten()
                .is_none()
            && chat.config.with_value(|config| {
                config
                    .agent
                    .as_ref()
                    .is_some_and(|agent| !agent.builtin && !agent.hidden)
            })
        {
            event.prevent_default();
            chat.new_thread.set(true);
        }
    });
    on_cleanup(move || new_chat.remove());
    let filter = RwSignal::new(String::new());
    let searching = RwSignal::new(false);
    let search_input = NodeRef::<leptos::html::Input>::new();
    Effect::new(move |_| {
        if searching.get() {
            request_animation_frame(move || {
                if let Some(input) = search_input.get_untracked() {
                    let _ = input.focus();
                }
            });
        }
    });
    let rows = Memo::new(move |_| chat.turns.with(|rows| rows.clone().unwrap_or_default()));
    let timeline = NodeRef::<leptos::html::Section>::new();
    let content = NodeRef::<leptos::html::Div>::new();
    let follow = RwSignal::new(true);
    let last_scroll = RwSignal::new(0);
    let scroll_to_bottom = move |node: &web_sys::HtmlElement| {
        node.set_scroll_top(node.scroll_height());
        last_scroll.set(node.scroll_top());
    };
    Effect::new(move |_| {
        let (Some(timeline), Some(content)) = (timeline.get(), content.get()) else {
            return;
        };
        let target = timeline.clone();
        let resize = Closure::<dyn FnMut()>::new(move || {
            if follow.get_untracked()
                && chat.focus.with_untracked(String::is_empty)
                && chat.before.get_untracked().is_none()
                && !searching.get_untracked()
            {
                scroll_to_bottom(&target);
            }
        });
        if let Ok(observer) = web_sys::ResizeObserver::new(resize.as_ref().unchecked_ref()) {
            observer.observe(&timeline);
            observer.observe(&content);
            let handle = StoredValue::new_local((observer, resize));
            on_cleanup(move || handle.with_value(|(observer, _)| observer.disconnect()));
        }
    });
    Effect::new(move |_| {
        rows.track();
        if follow.get_untracked() {
            let focus = chat.focus.get_untracked();
            request_animation_frame(move || {
                if !focus.is_empty() {
                    if let Some(target) = document()
                        .get_element_by_id(&chat.focus_card.get_value())
                        .or_else(|| document().get_element_by_id(&format!("turn-{focus}")))
                        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
                    {
                        target.scroll_into_view();
                        let options = web_sys::FocusOptions::new();
                        options.set_prevent_scroll(true);
                        let _ = target.focus_with_options(&options);
                        follow.set(false);
                    }
                } else if let Some(node) = timeline.get_untracked() {
                    scroll_to_bottom(&node);
                }
            });
        }
    });
    let label = format!("Conversation with {}", config.name);
    let close_search = move || {
        chat.query.set(String::new());
        filter.set(String::new());
        searching.set(false);
        if let Some(composer) = document()
            .get_element_by_id("message")
            .and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
        {
            let _ = composer.focus();
        }
    };
    view! {
        {move || chat.workspace.get().then(|| workspace.get_value())
            .flatten().map(|(agent, name)| files::workspace_page(chat, agent, name))}
        <div class="chat-row conversation-layout bs-root" hidden=move || chat.workspace.get() on:click=move |event| results::follow(chat, event)>
            <main class="chat conversation-chat" class:is-finding=searching aria-label=label>
                <form class="conversation-find k-inline" role="search" hidden=move || !searching.get()
                    on:keydown=move |event| { if event.key() == "Escape" { event.prevent_default(); close_search(); } }
                    on:submit=move |event| {
                    event.prevent_default();
                    chat.before.set(None);
                    chat.focus.set(String::new());
                    chat.query.set(filter.get_untracked().trim().into());
                }>
                    <input node_ref=search_input type="search" class="k-input" aria-label="Find messages" maxlength="200" prop:value=filter
                        on:input=move |event| filter.set(event_target_value(&event)) />
                    <button type="submit" class="k-btn k-btn--sm k-btn--primary">"Find"</button>
                    <button type="button" class="k-btn k-btn--sm k-btn--ghost" on:click=move |_| close_search()>"Close search"</button>
                </form>
                <div class="transcript-wrap">
                <section class="transcript" aria-label="Conversation history" node_ref=timeline on:scroll=move |_| {
                    if let Some(node) = timeline.get_untracked() {
                        let top = node.scroll_top();
                        if top == last_scroll.get_untracked() && follow.get_untracked() { return; }
                        last_scroll.set(top);
                        follow.set(node.scroll_height() - node.client_height() - top < 32);
                    }
                }>
                    <div class="transcript__col" node_ref=content>
                        <p role="status" hidden=move || chat.turns.with(Option::is_some)>"Loading conversation…"</p>
                        <p class="message-error" role="alert" hidden=move || chat.read_error.with(String::is_empty)>{chat.read_error}</p>
                        {guide.as_ref().map(|agent| {
                            let name = agent.name.clone();
                            view! { <div class="empty-chat" hidden=move || !chat.query.with(String::is_empty) || chat.turns.with(|rows| rows.as_ref().is_none_or(|rows| !rows.is_empty()))>
                                {avatar(Some(agent.clone()), &name, false, 64)}
                                <div class="empty-chat__title">{name}</div>
                                <div class="empty-chat__sub">"What work do you want to set up?"</div>
                            </div> }
                        })}
                        <p class="message-muted" hidden=move || chat.turns.with(|rows| rows.as_ref().is_none_or(|rows| !rows.is_empty()))
                            || (chat.query.with(String::is_empty) && chat.config.with_value(|config| config.agent.as_ref().is_some_and(|agent| agent.builtin)))>
                            {move || if chat.query.with(String::is_empty) { "No messages yet" } else { "No matching messages" }}
                        </p>
                        <button type="button" class="k-btn k-btn--sm k-btn--ghost" hidden=move || rows.with(|rows| rows.len() < 100)
                            on:click=move |_| chat.before.set(rows.with_untracked(|rows| rows.first().map(|turn| turn.seq)))>"Older messages"</button>
                        <button type="button" class="k-btn k-btn--sm k-btn--ghost" hidden=move || chat.before.get().is_none() && chat.focus.with(String::is_empty)
                            on:click=move |_| { chat.before.set(None); chat.focus.set(String::new()); follow.set(true); }>"Latest messages"</button>
                        <For each=move || rows.with(|rows| rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>()) key=|id| id.clone()
                            children=move |id| {
                                let row = RwSignal::new(rows.with_untracked(|rows| rows.iter().find(|row| row.id == id).cloned()).unwrap_or_else(|| unreachable!()));
                                let key = id.clone();
                                Effect::new(move |_| {
                                    if let Some(next) = rows.with(|rows| rows.iter().find(|row| row.id == key).cloned()) {
                                        if row.with_untracked(|previous| *previous != next) { row.set(next); }
                                    }
                                });
                                let latest_id = id.clone();
                                let latest = Memo::new(move |_| rows.with(|turns| turns.last().is_some_and(|turn| turn.id == latest_id)));
                                turn_view(chat, row, id, latest)
                            } />
                        <div></div>
                    </div>
                </section>
                </div>
                {controls::composer(chat, rows)}
                {guide.map(|agent| guide::connection(agent, chat.guide_connection, move |agent| {
                    chat.guide_ready.try_set(Some(agent.runnable));
                    request_animation_frame(move || {
                        if let Ok(Some(input)) = document().query_selector(".composer__input") {
                            if let Ok(input) = input.dyn_into::<web_sys::HtmlElement>() { let _ = input.focus(); }
                        }
                    });
                }))}
                {header(chat, config, searching)}
            </main>
            {panels::right_panel(chat)}
        </div>
    }
}
