mod controls;
mod guide;
mod history;
mod input;
mod message_actions;
mod minimap;
mod panels;
mod protocol;
mod queue;
mod rooms;
mod routes;

use std::{collections::HashMap, time::Duration};

use gloo_net::http::{Request, Response};
use leptos::{leptos_dom::helpers::set_timeout, prelude::*};
use protocol::{Command, Partial, ReplyContext, MAX_FILES, MAX_MESSAGE};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::Value;
use wasm_bindgen::{closure::Closure, prelude::wasm_bindgen, JsCast};
use web_sys::{EventSource, MessageEvent, Storage};

pub use routes::{standalone, Agent, Thread};
pub(crate) use routes::{Navigation, ProjectConversation};

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct Icons {
    pub attach: String,
    pub send: String,
    pub mic: String,
    pub stop: String,
    pub chevron: String,
    pub panel: String,
    pub search: String,
    pub up: String,
    pub listen: String,
    pub restore: String,
    pub edit: String,
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
    pub room: Option<routes::Channel>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, serde::Serialize)]
struct Segment {
    seq: u64,
    id: String,
    kind: String,
    text: String,
    state: Option<String>,
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
    dispatch: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct ProviderCall {
    created: String,
    finished: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct FileRef {
    id: String,
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    image: bool,
    #[serde(default)]
    deleted: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct Reply {
    id: String,
    title: String,
    height: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
struct Turn {
    id: String,
    #[serde(default)]
    agent: String,
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
    parent: Option<String>,
    reply: Option<String>,
    error: Option<String>,
    partial: Option<Partial>,
    dispatch_label: Option<String>,
    dispatch_mode: Option<String>,
    dispatch_target: Option<String>,
    recipient: Option<String>,
    #[serde(default)]
    editing: bool,
    #[serde(default)]
    files: Vec<FileRef>,
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
    history_page: Option<u8>,
    checkpoint: Option<Value>,
    #[serde(default)]
    events: Vec<Value>,
    #[serde(rename = "computerWait")]
    computer_wait: Option<Value>,
    #[serde(rename = "messageOrigin")]
    origin: Option<Value>,
    #[serde(rename = "routineReport")]
    routine_report: Option<Value>,
    waiting_for: Option<String>,
    workflow_run: Option<String>,
    routing: Option<String>,
    #[serde(default)]
    coordination: Vec<Value>,
}

impl Turn {
    fn active(&self) -> bool {
        matches!(self.status.as_str(), "running" | "queued")
    }

    fn needs_you(&self) -> bool {
        self.active()
            && (self.questions.iter().any(|q| q["answer"].is_null())
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
    #[serde(default)]
    restart: bool,
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
    get_with_signal(path, None).await
}

async fn get_with_signal<T: DeserializeOwned>(
    path: &str,
    signal: Option<&web_sys::AbortSignal>,
) -> Result<T, ApiError> {
    read(Request::get(path).abort_signal(signal).send().await).await
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
    latest: RwSignal<Vec<Turn>>,
    optimistic: RwSignal<Vec<Turn>>,
    active: RwSignal<Vec<Dispatch>>,
    addressed: RwSignal<String>,
    focus: RwSignal<String>,
    refresh: RwSignal<u64>,
    focus_card: StoredValue<String>,
    query: RwSignal<String>,
    finding: RwSignal<bool>,
    history: RwSignal<Vec<u64>>,
    searched: RwSignal<bool>,
    move_focus: RwSignal<bool>,
    tab: RwSignal<&'static str>,
    details: RwSignal<bool>,
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
        let focus = web_sys::UrlSearchParams::new_with_str(
            &window().location().search().unwrap_or_default(),
        )
        .ok()
        .and_then(|params| params.get("focus"))
        .filter(|id| protocol::id(id))
        .or_else(|| saved(&focus_key))
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
            latest: RwSignal::new(Vec::new()),
            optimistic: RwSignal::new(Vec::new()),
            active: RwSignal::new(Vec::new()),
            addressed: RwSignal::new(String::new()),
            focus: RwSignal::new(focus.clone()),
            focus_card: StoredValue::new(focus_card),
            refresh: RwSignal::new(0),
            query: RwSignal::new(String::new()),
            finding: RwSignal::new(false),
            history: RwSignal::new(Vec::new()),
            searched: RwSignal::new(false),
            move_focus: RwSignal::new(!focus.is_empty()),
            tab: RwSignal::new("chats"),
            details: RwSignal::new(restore_details()),
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

    fn queued(self, turn: &Turn) -> bool {
        !self.config.with_value(|config| config.channel)
            && self
                .active
                .with(|active| active.iter().any(|active| active.id != turn.id))
            && turn.status == "queued"
            && turn.paused == 0
            && !turn.needs_you()
            && turn.parent.is_none()
            && turn.waiting_for.is_none()
            && turn.routine_report.is_none()
            && turn.workflow_run.is_none()
            && turn.activity.calls.is_empty()
            && turn.exchanges.is_empty()
            && !turn.events.iter().any(|event| event["status"] == "waiting")
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
        let queued = self.target().is_some();
        let local = Turn {
            id: pending.command.id.clone(),
            text: pending.command.text.clone(),
            status: if queued { "queued" } else { "running" }.into(),
            created: js_sys::Date::new_0()
                .to_iso_string()
                .as_string()
                .unwrap_or_default(),
            agent: config
                .agent
                .as_ref()
                .map_or_else(String::new, |agent| agent.id.clone()),
            dispatch_mode: pending.command.dispatch_mode.clone(),
            ..Turn::default()
        };
        self.optimistic.update(|rows| {
            if !rows.iter().any(|row| row.id == local.id) {
                rows.push(local);
            }
        });
        self.query.set(String::new());
        self.focus.set(String::new());
        self.history.set(Vec::new());
        self.finding.set(false);
        let path = format!("{}/turns", config.base);
        leptos::task::spawn_local(async move {
            let result = post::<Value>(&path, &pending.raw).await;
            if self.busy.is_disposed() {
                return;
            }
            match result {
                Ok(_) => {
                    self.finish();
                    self.refresh();
                }
                Err(error) => {
                    if error.definitive && remove(&key) {
                        self.pending.set(None);
                        self.optimistic
                            .update(|rows| rows.retain(|row| row.id != pending.command.id));
                    }
                    self.error.set(error.message);
                }
            }
            self.busy.set(false);
        });
    }

    fn set_details(self, open: bool) {
        if let Ok(Some(storage)) = window().local_storage() {
            let _ = storage.set_item("minarbot:details-open", if open { "1" } else { "0" });
        }
        self.details.set(open);
    }

    fn inspect(self, turn: String) {
        self.run.set(turn);
        self.tab.set("run");
        self.set_details(true);
    }

    fn focus_turn(self, id: &str) {
        if protocol::id(id) {
            self.finding.set(false);
            self.searched.set(false);
            self.query.set(String::new());
            self.history.set(Vec::new());
            self.move_focus.set(true);
            if self.focus.get_untracked() != id {
                self.turns.set(None);
                self.focus.set(id.into());
            }
        }
    }

    fn refresh(self) {
        self.refresh.update(|value| *value += 1);
    }

    fn before(self) -> Option<u64> {
        self.history.with(|pages| pages.last().copied())
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

fn polling(
    chat: Chat,
    snapshots: StoredValue<HashMap<String, Partial>>,
    generation: RwSignal<u64>,
    epoch: u64,
    abort: StoredValue<Option<web_sys::AbortController>, LocalStorage>,
    dispatch_busy: RwSignal<bool>,
) {
    let current = move || generation.try_get_untracked() == Some(epoch);
    if !current() {
        return;
    }
    let base = chat.base();
    let focus = chat.focus.get_untracked();
    let before = chat.history.with_untracked(|pages| pages.last().copied());
    let finding = chat.finding.get_untracked();
    let path = if protocol::id(&focus) {
        format!("{base}/turns?focus={focus}")
    } else if let Some(before) = before {
        format!("{base}/turns?before={before}")
    } else {
        format!("{base}/turns")
    };
    let signal =
        abort.with_value(|controller| controller.as_ref().map(web_sys::AbortController::signal));
    if !dispatch_busy.get_untracked() {
        dispatch_busy.set(true);
        let dispatch_signal = signal.clone();
        let dispatch_path = format!("{base}/dispatch");
        leptos::task::spawn_local(async move {
            let result =
                get_with_signal::<Vec<Dispatch>>(&dispatch_path, dispatch_signal.as_ref()).await;
            if current() {
                dispatch_busy.set(false);
                if let Ok(active) = result {
                    if chat.active.with_untracked(|old| *old != active) {
                        chat.active.set(active);
                    }
                }
            }
        });
    }
    leptos::task::spawn_local(async move {
        let result = get_with_signal::<Vec<Turn>>(&path, signal.as_ref()).await;
        if !current() {
            return;
        }
        let mut delay = Duration::from_millis(1200);
        let result = if let Ok(page) = &result {
            let history_mode = !focus.is_empty() || before.is_some();
            if history_mode {
                if let Ok(latest) =
                    get_with_signal::<Vec<Turn>>(&format!("{base}/turns"), signal.as_ref()).await
                {
                    if !current() {
                        return;
                    }
                    if chat.latest.with_untracked(|old| *old != latest) {
                        chat.latest.set(latest);
                    }
                }
                result
            } else {
                if chat.latest.with_untracked(|old| old != page) {
                    chat.latest.set(page.clone());
                }
                if chat.optimistic.with_untracked(|pending| {
                    pending
                        .iter()
                        .any(|turn| page.iter().any(|row| row.id == turn.id))
                }) {
                    chat.optimistic.update(|pending| {
                        pending.retain(|turn| !page.iter().any(|row| row.id == turn.id))
                    });
                }
                if chat.searched.get_untracked() {
                    let previous = chat
                        .turns
                        .with_untracked(|rows| rows.clone().unwrap_or_default());
                    Ok(history::merge(previous, page.clone()))
                } else if finding {
                    history::all_turns(&base, page.clone(), current, signal.as_ref()).await
                } else {
                    result
                }
            }
        } else {
            result
        };
        if !current() {
            return;
        }
        if finding && result.is_ok() {
            chat.searched.set(true);
        }
        {
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
        }
        set_timeout(
            move || polling(chat, snapshots, generation, epoch, abort, dispatch_busy),
            delay,
        );
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
            || chat.finding.get_untracked()
            || !chat.history.with_untracked(Vec::is_empty)
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
    fn markdown(text: &str, scope: &str) -> String;

    #[wasm_bindgen(js_namespace = MinarbotConversation, js_name = messageTime)]
    fn message_time(value: &str) -> String;

    #[wasm_bindgen(js_namespace = MinarbotConversation, catch, js_name = writeClipboard)]
    async fn write_clipboard(text: &str) -> Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>;

    #[wasm_bindgen(js_namespace = MinarbotConversation, js_name = markdownBlocks)]
    fn markdown_blocks(text: &str) -> js_sys::Array;

    #[wasm_bindgen(js_namespace = MinarbotConversation, js_name = bindTurnTabStops)]
    fn bind_turn_tab_stops(transcript: &web_sys::HtmlElement) -> js_sys::Function;

    #[wasm_bindgen(js_namespace = MinarbotConversation, js_name = postKeepalive)]
    fn post_keepalive(path: &str, body: &str);
}

fn body(text: String, base: String) -> impl IntoView {
    view! { <div class="markdown" inner_html=markdown(&text, &base)></div> }
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
        <span class="chat-action-tooltip">
        <button type="button" class="k-icon-btn" aria-label=label on:click=move |_| {
            let text = text();
            leptos::task::spawn_local(async move {
                match write_clipboard(&text).await {
                    Ok(_) => { feedback.try_set("Copied"); }
                    Err(_) => { feedback.try_set("Text could not be copied."); }
                }
                set_timeout(move || { feedback.try_set(""); }, Duration::from_secs(2));
            });
        }><span class="appearance-icon" inner_html=icon></span></button>
        <span class="k-tooltip" role="tooltip">{label}</span>
        </span>
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
    let nav = use_context::<Navigation>();
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
            <span class="chat-action-tooltip">
            <button type="button" class="k-icon-btn" aria-label="Fork from here" disabled=busy on:click=move |_| {
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
                            routes::open(nav, &format!("/agents/{}/conversations/{}", thread.agent, thread.id));
                        }
                        Ok(_) => { error.try_set("Fork response was invalid.".into()); }
                        Err(reason) => { error.try_set(reason.message); }
                    }
                    busy.try_set(false);
                });
            }><span class="appearance-icon" inner_html=icons.fork.clone()></span></button>
            <span class="k-tooltip" role="tooltip">"Fork from here"</span>
            </span>
            <time class="message-time" datetime=finished>{move || message_time(&finished())}</time>
            {move || (!error.with(String::is_empty)).then(|| view! { <span class="message-error" role="alert">{error}</span> })}
        </div>
    }
}

fn turn_view(chat: Chat, row: RwSignal<Turn>, id: String, latest: Memo<bool>) -> impl IntoView {
    let inspect_id = id.clone();
    let room_root = move || {
        row.with(|turn| {
            turn.room
                .as_ref()
                .is_some_and(|room| room["root"] == turn.id)
        })
    };
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
    let rich_base = base.clone();
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
    let answer = Memo::new(move |_| {
        if room_root() {
            return String::new();
        }
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
    let blocks = Memo::new(move |_| {
        answer.with(|text| {
            markdown_blocks(text)
                .iter()
                .enumerate()
                .filter_map(|(index, block)| Some((index, block.as_string()?)))
                .collect::<Vec<_>>()
        })
    });
    let activity = Memo::new(move |_| {
        if room_root() {
            return Vec::new();
        }
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
            if turn.room.is_none() {
                if let Some(routing) = turn.routing.as_deref() {
                    let label = match routing {
                        "pending" => "Selecting recipient".into(),
                        "clarify" => "Requested routing clarification".into(),
                        "none" => "No task needed".into(),
                        "released" => "Waiting for an available recipient".into(),
                        _ => format!(
                            "Recipient selected: {}",
                            turn.agent_name.as_deref().unwrap_or("Agent")
                        ),
                    };
                    steps.push((false, label));
                } else if turn.recipient.is_some() {
                    steps.push((
                        false,
                        format!(
                            "Recipient selected: {}",
                            turn.agent_name.as_deref().unwrap_or("Agent")
                        ),
                    ));
                }
            }
            if let Some(model) = &turn.activity.fallback_model {
                steps.push((false, format!("Fallback model used: {model}")));
            }
            steps.extend(turn.coordination.iter().map(|action| {
                let owner = action["owner_name"].as_str().unwrap_or("Agent");
                let target = action["target_name"].as_str().unwrap_or("Agent");
                let verb = if action["kind"] == "delegate_task" {
                    "delegated to"
                } else {
                    "transferred to"
                };
                (false, format!("{owner} {verb} {target}"))
            }));
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
    let prompt = Memo::new(move |_| row.with(|turn| turn.text.clone()));
    let outputs = Memo::new(move |_| row.with(|turn| turn.generated.clone()));
    view! {
        <article class="turn-block" class:is-latest=move || latest.get() id=format!("turn-{id}") tabindex="0"
            on:keydown=move |event| {
                if !matches!(event.key().as_str(), "ArrowLeft" | "ArrowRight" | "Home" | "End")
                    || event.target().and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                        .is_some_and(|target| target.closest("input,textarea,select,[contenteditable=true],[role=menu],[role=dialog],[role=listbox],dialog").ok().flatten().is_some())
                { return; }
                let Some(article) = event.current_target().and_then(|target| target.dyn_into::<web_sys::HtmlElement>().ok()) else { return; };
                let Ok(actions) = article.query_selector_all("button:not(:disabled),a[href],input:not(:disabled),textarea:not(:disabled),select:not(:disabled),[contenteditable=true],summary") else { return; };
                let actions = (0..actions.length()).filter_map(|index| actions.item(index))
                    .filter_map(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
                    .filter(|node| node.get_bounding_client_rect().width() > 0.0 && window().get_computed_style(node).ok().flatten().is_none_or(|style| style.get_property_value("visibility").ok().as_deref() != Some("hidden")))
                    .collect::<Vec<_>>();
                if actions.is_empty() { return; }
                event.prevent_default();
                let current = document().active_element().and_then(|node| actions.iter().position(|action| action.is_same_node(Some(&node))));
                let next = match event.key().as_str() {
                    "Home" => 0,
                    "End" => actions.len() - 1,
                    "ArrowRight" => current.map_or(0, |index| (index + 1) % actions.len()),
                    _ => current.map_or(actions.len() - 1, |index| (index + actions.len() - 1) % actions.len()),
                };
                let _ = actions[next].focus();
            }>
            <div class="human-message" tabindex="-1" aria-label="Your message"
                hidden=move || row.with(|turn| turn.goal_continuation)>
                <div class="human-bubble message-plain" hidden=move || prompt.with(|text| text.trim().is_empty())>{prompt}</div>
                {move || row.with(|turn| turn.files.iter().filter(|file| !file.deleted).cloned().map(|file| attachment(file, base.clone())).collect_view())}
                <div class="turn-meta human-meta">
                    <time class="message-time" datetime=move || row.with(|turn| turn.created.clone())>{move || row.with(|turn| message_time(&turn.created))}</time>
                    {move || row.with(|turn| {
                        if let Some(name) = turn.origin.as_ref().filter(|origin| origin["kind"] == "peer").and_then(|origin| origin["name"].as_str()) {
                            Some(format!("From {name}"))
                        } else if turn.workflow_run.is_some() { Some("Workflow".to_owned()) }
                        else if turn.parent.is_some() { Some("Subtask".to_owned()) }
                        else { None }
                    }).map(|label| view! { <span class="message-origin">{label}</span> })}
                    {move || row.with(|turn| turn.parent.clone().filter(|id| protocol::id(id)).filter(|_| turn.origin.is_none())).map(|parent| {
                        let href = format!("#turn-{parent}");
                        view! { <a class="link-btn link-btn--small" href=href on:click=move |event| { event.prevent_default(); chat.focus_turn(&parent); }>"View parent task"</a> }
                    })}
                    {move || row.with(|turn| turn.origin.as_ref().and_then(|origin| history::destination(origin["agent"].as_str().unwrap_or_default(), origin["thread"].as_str(), origin["channel"].as_str()))).map(|href| {
                        view! { <a class="link-btn link-btn--small" href=href>"Open requester conversation"</a> }
                    })}
                    {copy_action(move || row.with_untracked(|turn| turn.text.clone()), "Copy message")}
                </div>
            </div>
            <Show when=move || row.with(|turn| turn.goal_continuation)>
                <p class="message-muted goal-continuation">"Goal continued"</p>
            </Show>
            <Show when=room_root>{rooms::discussion(chat, row)}</Show>
            <div class="agent-message" hidden=room_root>
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
                                        <summary class="turn-activity__header" role="button">
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
                    <div class="message-body" hidden=move || answer.with(String::is_empty) && row.with(|turn| turn.html.is_empty() && turn.apps.is_empty() && turn.components.is_empty() && turn.error.is_none() && turn.dispatch_label.is_none() && turn.approval_subject().is_none() && turn.events.is_empty() && turn.waiting_for.is_none())>
                        {panels::rich_replies(chat, row)}
                        {move || row.with(|turn| (turn.dispatch_mode.as_deref() == Some("steer") && turn.active())
                            .then_some(turn.dispatch_label.clone()).flatten().map(|text| (text, turn.dispatch_target.clone()))).map(|(text, target)| view! {
                            <p class="dispatch-receipt">{text}{target.filter(|id| protocol::id(id)).map(|id| view! {
                                " · "<button type="button" class="link-btn link-btn--small" on:click=move |_| chat.focus_turn(&id)>"View target turn"</button>
                            })}</p>
                        })}
                        <div aria-label=label hidden=move || answer.with(String::is_empty)>
                            <div class="legacy stream-markdown">
                                <For each=move || blocks.get() key=|(index, block): &(usize, String)| (*index, block.clone())
                                    children=move |(_, block)| body(block, text_base.clone()) />
                            </div>
                        </div>
                        {move || row.with(|turn| {
                            if let Some(error) = turn.error.clone().filter(|_| turn.status != "cancelled") {
                                view! { <p class="message-error"><span class="appearance-icon" inner_html=icons.alert.clone()></span>{error}</p> }.into_any()
                            } else {
                                let text = turn.approval_subject().filter(|subject| *subject != "form").map(|subject| format!("Review the {subject} to continue."))
                                    .or_else(|| turn.events.iter().any(|event| event["status"] == "waiting").then(|| "Waiting for a signed event.".into()))
                                    .or_else(|| turn.waiting_for.is_some().then(|| "Waiting for child task results.".into()));
                                text.map(|text| view! { <p class="message-muted">{text}</p> }).into_any()
                            }
                        })}
                    </div>
                    {move || row.with(|turn| {
                        let subject = turn.approval_subject();
                        let label = if turn.active() && subject == Some("form") { "Waiting for your input" }
                            else if turn.active() && subject.is_some() { "Waiting for approval" }
                            else if turn.active() && turn.waiting_for_answer() { "Waiting for your answer" }
                            else if turn.active() && turn.paused != 0 { "Paused" }
                            else if turn.routing.as_deref() == Some("pending") && turn.status == "running" { "Selecting recipient" }
                            else if turn.routing.as_deref() == Some("clarify") { "Needs clarification" }
                            else if turn.routing.as_deref() == Some("none") { "No task needed" }
                            else if turn.waiting_for.is_some() && turn.status == "queued" { "Waiting for delegation" }
                            else { "" };
                        let text = match (label, turn.activity.fallback_model.is_some()) {
                            ("", true) if turn.status == "done" => "Done · Fallback model used".into(),
                            ("", true) => "Fallback model used".into(),
                            (_, true) => format!("{label} · Fallback model used"),
                            (_, false) => label.into(),
                        };
                        (!label.is_empty() || turn.activity.fallback_model.is_some()).then(|| view! {
                            <div class="turn-meta agent-meta"><span class="turn-status" class:is-attention=subject == Some("form") || turn.waiting_for_answer() || turn.paused != 0
                                class:is-working=subject.is_some_and(|value| value != "form")><i aria-hidden="true"></i>{text}</span></div>
                        })
                    })}
                    {move || outputs.get().into_iter().filter(|file| !file.deleted).map(|file| attachment(file, rich_base.clone())).collect_view()}
                    {panels::exchanges(chat, row)}
                    {message_actions::events(chat, row)}
                    {history::coordination(chat, row)}
                    {history::guidance(chat, row)}
                    {controls::turn_controls(chat, row, false)}
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

fn header(
    chat: Chat,
    config: ConversationConfig,
    search_dialog: NodeRef<leptos::html::Dialog>,
) -> impl IntoView {
    let icons = use_context::<Icons>().unwrap_or_default();
    let menu = NodeRef::<leptos::html::Details>::new();
    let trigger = NodeRef::<leptos::html::Summary>::new();
    let title = config.name.clone();
    let project = config
        .thread
        .as_ref()
        .and_then(|thread| thread.project.clone())
        .or_else(|| config.room.as_ref().and_then(|room| room.project.clone()))
        .filter(|id| protocol::id(id));
    let section_settings = config.channel && project.is_none();
    let builtin = config.agent.as_ref().is_some_and(|agent| agent.builtin);
    let toggle_details = move || chat.set_details(!chat.details.get_untracked());
    let keys = window_event_listener(leptos::ev::keydown, move |event| {
        if document()
            .query_selector("dialog:modal")
            .ok()
            .flatten()
            .is_some()
        {
            return;
        }
        if !(event.ctrl_key() || event.meta_key()) {
            return;
        }
        if event.shift_key() && event.key().eq_ignore_ascii_case("b") {
            event.prevent_default();
            toggle_details();
        } else if event.key().eq_ignore_ascii_case("f") {
            event.prevent_default();
            history::begin_find(chat);
        }
    });
    on_cleanup(move || keys.remove());
    let lifecycle = use_context::<crate::agent_profile::Lifecycle>();
    let profile_agent = config
        .agent
        .as_ref()
        .filter(|agent| !agent.builtin)
        .map(|agent| agent.id.clone());
    let main_chat = config.thread.is_none();
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
                    {project.map(|id| view! { <a class="k-menu__item" href=format!("/projects/{id}")>"Back to project"</a> })}
                    {section_settings.then(|| view! {
                        <button type="button" class="k-menu__item" on:click=move |_| {
                            if let Some(menu) = menu.get_untracked() { menu.set_open(false); }
                            let _ = web_sys::Event::new("minarbot:section-settings").and_then(|event| window().dispatch_event(&event));
                        }><span class="appearance-icon" inner_html=icons.settings></span>"Section settings"</button>
                    })}
                    <button type="button" class="k-menu__item" on:click=move |_| {
                        if let Some(menu) = menu.get_untracked() { menu.set_open(false); }
                        history::begin_find(chat);
                    }><span class="appearance-icon" inner_html=icons.search.clone()></span>"Find messages"</button>
                    {builtin.then(|| view! {
                        <button type="button" class="k-menu__item" on:click=move |_| {
                            if let Some(menu) = menu.get_untracked() { menu.set_open(false); }
                            chat.guide_connection.set(true);
                        }>"AI connection"</button>
                    })}
                    <button type="button" class="k-menu__item" on:click=move |_| {
                        if let Some(menu) = menu.get_untracked() { menu.set_open(false); }
                        controls::dialog_open(search_dialog);
                    }><span class="appearance-icon" inner_html=icons.search></span>"Search chat history"</button>
                    <button type="button" class="k-menu__item" on:click=move |_| {
                        if let Some(menu) = menu.get_untracked() { menu.set_open(false); }
                        toggle_details();
                        if let Some(trigger) = trigger.get_untracked() { let _ = trigger.focus(); }
                    }><span class="appearance-icon" inner_html=icons.panel.clone()></span>
                      {move || if chat.details.get() { "Hide panel" } else { "Show panel" }}</button>
                    {profile_agent.map(|id| {
                        let target = StoredValue::new(id);
                        view! {
                            {main_chat.then(|| view! { <button type="button" class="k-menu__item" on:click=move |_| {
                                if let Some(menu) = menu.get_untracked() { menu.set_open(false); }
                                if let Some(trigger) = trigger.get_untracked() { let _ = trigger.focus(); }
                                if let Some(lifecycle) = lifecycle { lifecycle.open(crate::agent_profile::DialogTarget::Edit(target.get_value())); }
                            }>"Edit profile"</button> })}
                            <button type="button" class="k-menu__item" on:click=move |_| {
                                if let Some(menu) = menu.get_untracked() { menu.set_open(false); }
                                if let Some(trigger) = trigger.get_untracked() { let _ = trigger.focus(); }
                                if let Some(lifecycle) = lifecycle { lifecycle.open(crate::agent_profile::DialogTarget::Export(target.get_value())); }
                            }>"Export template"</button>
                        }
                    })}
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
    let guide = config.agent.clone().filter(|agent| agent.builtin);
    let snapshots = StoredValue::new(HashMap::new());
    subscribe(chat, snapshots);
    let generation = RwSignal::new(0_u64);
    Effect::new(move |_| {
        chat.focus.track();
        chat.history.track();
        chat.finding.track();
        chat.refresh.track();
        generation.update(|value| *value += 1);
        let abort = StoredValue::new_local(web_sys::AbortController::new().ok());
        on_cleanup(move || {
            abort.try_with_value(|controller| {
                if let Some(controller) = controller {
                    controller.abort();
                }
            });
        });
        polling(
            chat,
            snapshots,
            generation,
            generation.get_untracked(),
            abort,
            RwSignal::new(false),
        );
    });
    message_actions::playback();
    if config
        .agent
        .as_ref()
        .is_some_and(|agent| !agent.builtin && !agent.hidden)
    {
        let create = move || {
            chat.new_thread.set(true);
            chat.tab.set("chats");
            chat.set_details(true);
        };
        let request = window_event_listener(
            leptos::ev::Custom::<web_sys::Event>::new("minarbot:new-conversation"),
            move |_| create(),
        );
        let shortcut = window_event_listener(leptos::ev::keydown, move |event| {
            if (event.ctrl_key() || event.meta_key())
                && event.shift_key()
                && event.key().eq_ignore_ascii_case("o")
                && document()
                    .query_selector("dialog:modal")
                    .ok()
                    .flatten()
                    .is_none()
            {
                event.prevent_default();
                create();
            }
        });
        on_cleanup(move || {
            request.remove();
            shortcut.remove();
        });
    }
    let search_dialog = NodeRef::<leptos::html::Dialog>::new();
    let room_settings = config
        .room
        .clone()
        .filter(|room| room.project.is_none())
        .map(|room| rooms::settings(room, NodeRef::new()));
    let rows = Memo::new(move |_| {
        let mut rows = chat.turns.with(|rows| rows.clone().unwrap_or_default());
        if chat.focus.with(String::is_empty) && chat.before().is_none() {
            chat.optimistic
                .with(|pending| rows.extend(pending.iter().cloned()));
        }
        rows
    });
    let visible = Memo::new(move |_| {
        rows.with(|rows| {
            let focus = chat.focus.get();
            rows.iter()
                .filter(|turn| {
                    turn.id == focus
                        || (!chat.queued(turn)
                            && !(turn.status == "cancelled"
                                && !turn
                                    .error
                                    .as_deref()
                                    .is_some_and(|error| error.starts_with("Stopped"))
                                && turn.activity.calls.is_empty()
                                && turn.room.is_none())
                            && turn.room.as_ref().is_none_or(|room| {
                                room["root"] == turn.id
                                    || !rows.iter().any(|root| room["root"] == root.id)
                            }))
                })
                .map(|turn| turn.id.clone())
                .collect::<Vec<_>>()
        })
    });
    let timeline = NodeRef::<leptos::html::Section>::new();
    let content = NodeRef::<leptos::html::Div>::new();
    let follow = RwSignal::new(true);
    Effect::new(move |_| {
        let Some(timeline) = timeline.get() else {
            return;
        };
        let stop = StoredValue::new_local(bind_turn_tab_stops(&timeline));
        on_cleanup(move || {
            stop.with_value(|close| {
                let _ = close.call0(&wasm_bindgen::JsValue::NULL);
            });
        });
    });
    rooms::read(chat, follow);
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
                && chat.history.with_untracked(Vec::is_empty)
                && !chat.finding.get_untracked()
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
    let navigation = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| {
        let loaded = chat.turns.with(Option::is_some);
        let move_focus = chat.move_focus.get();
        if !loaded {
            return;
        }
        request_animation_frame(move || {
            if chat.turns.is_disposed() {
                return;
            }
            let focus = chat.focus.get_untracked();
            let before = chat.history.with_untracked(|pages| pages.last().copied());
            if move_focus && chat.move_focus.get_untracked() {
                let target = if !focus.is_empty() {
                    let card = chat.focus_card.get_value();
                    let target = document()
                        .get_element_by_id(&format!("room-reply-{focus}"))
                        .or_else(|| document().get_element_by_id(&format!("turn-{focus}")));
                    target
                        .and_then(|target| {
                            document()
                                .get_element_by_id(&card)
                                .filter(|element| target.contains(Some(element)))
                                .or(Some(target))
                        })
                        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
                } else if before.is_some() {
                    navigation
                        .get_untracked()
                        .map(|node| node.unchecked_into::<web_sys::HtmlElement>())
                } else {
                    document()
                        .query_selector(".transcript article.is-latest")
                        .ok()
                        .flatten()
                        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
                };
                if let Some(target) = target {
                    target.scroll_into_view();
                    let options = web_sys::FocusOptions::new();
                    options.set_prevent_scroll(true);
                    let _ = target.focus_with_options(&options);
                }
                chat.move_focus.set(false);
            }
            if focus.is_empty()
                && before.is_none()
                && !chat.finding.get_untracked()
                && follow.get_untracked()
            {
                if let Some(node) = timeline.get_untracked() {
                    scroll_to_bottom(&node);
                }
            }
        });
    });
    let label = format!("Conversation {}", config.name);
    let subtitle = if guide.is_some() {
        Some("What work do you want to set up?".into())
    } else {
        config
            .agent
            .as_ref()
            .and_then(|agent| agent.role.clone())
            .filter(|role| !role.is_empty())
    };
    let empty_name = config
        .agent
        .as_ref()
        .map_or_else(|| config.name.clone(), |agent| agent.name.clone());
    let to_latest = move || {
        if !chat.focus.with_untracked(String::is_empty)
            || !chat.history.with_untracked(Vec::is_empty)
        {
            chat.turns.set(None);
            chat.focus.set(String::new());
            chat.history.set(Vec::new());
            chat.move_focus.set(true);
        }
        follow.set(true);
        if let Some(node) = timeline.get_untracked() {
            scroll_to_bottom(&node);
        }
    };
    let icons = use_context::<Icons>().unwrap_or_default();
    view! {
        <div class="chat-row conversation-layout bs-root">
            <main class="chat conversation-chat" class:is-finding=chat.finding aria-label=label>
                {history::find(chat, timeline, follow)}
                {history::search_all(chat, search_dialog)}
                {room_settings}
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
                        <div class="history-banner" role="status" hidden=move || chat.focus.with(String::is_empty) && chat.before().is_none()>
                            <span>{move || if chat.focus.with(String::is_empty) { "Older history" } else if chat.config.with_value(|config| config.channel) { "Task family" } else { "Selected message" }}</span>
                            <button type="button" class="link-btn" on:click=move |_| to_latest()>"Back to latest messages"</button>
                        </div>
                        <div class="history-navigation" aria-label="History navigation" node_ref=navigation tabindex="-1"
                            hidden=move || !chat.focus.with(String::is_empty) || chat.finding.get() || chat.searched.get() || chat.before().is_none() && rows.with(|rows| rows.iter().filter(|turn| turn.history_page != Some(0)).count() < 100)>
                            <button type="button" class="link-btn" hidden=move || rows.with(|rows| rows.iter().filter(|turn| turn.history_page != Some(0)).count() < 100)
                                on:click=move |_| {
                                    if let Some(seq) = rows.with_untracked(|rows| rows.iter().find(|turn| turn.history_page != Some(0)).map(|turn| turn.seq)) {
                                        follow.set(false); chat.move_focus.set(true); chat.turns.set(None); chat.history.update(|pages| pages.push(seq));
                                    }
                                }>"Older messages"</button>
                            <button type="button" class="link-btn" hidden=move || chat.before().is_none() on:click=move |_| {
                                follow.set(true); chat.move_focus.set(true); chat.turns.set(None); chat.history.update(|pages| { pages.pop(); });
                            }>"Newer messages"</button>
                        </div>
                        {history::fork(chat)}
                        <p class="empty-chat__sub" role="status" hidden=move || chat.turns.with(Option::is_some)>"Loading conversation..."</p>
                        <p class="chat-notice is-error" role="alert" hidden=move || chat.read_error.with(String::is_empty)>{chat.read_error}" History will be retried."</p>
                        <p class="empty-chat__sub" role="status" hidden=move || chat.before().is_none() || chat.turns.with(|rows| rows.as_ref().is_none_or(|rows| !rows.is_empty()))>
                            "Start of history reached. Select Newer messages to return."
                        </p>
                        <div class="empty-chat" hidden=move || chat.before().is_some() || chat.turns.with(|rows| rows.as_ref().is_none_or(|rows| !rows.is_empty())) || chat.config.with_value(|config| config.thread.as_ref().is_some_and(|thread| thread.fork.is_some()))>
                            {avatar(config.agent.clone(), &config.name, config.channel, 64)}
                            <div class="empty-chat__title">{empty_name}</div>
                            {subtitle.map(|role| view! { <div class="empty-chat__sub">{role}</div> })}
                        </div>
                        <For each=move || visible.get() key=|id| id.clone()
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
                {minimap::minimap(chat, rows, timeline, content, follow, last_scroll)}
                <div class="transcript-jump" hidden=move || follow.get() || chat.before().is_some() || !chat.focus.with(String::is_empty) || chat.finding.get()>
                    <button type="button" class="k-icon-btn jump-btn" aria-label="Jump to latest" on:click=move |_| to_latest()><span class="appearance-icon" inner_html=icons.chevron></span></button>
                </div>
                </div>
                {controls::composer(chat, Memo::new(move |_| {
                    let mut latest = chat.latest.get();
                    chat.optimistic.with(|pending| latest.extend(pending.iter().cloned()));
                    latest
                }))}
                {guide.map(|agent| guide::connection(agent, chat.guide_connection, move |agent| {
                    chat.guide_ready.try_set(Some(agent.runnable));
                    request_animation_frame(move || history::focus_element(".composer__input"));
                }))}
                {header(chat, config, search_dialog)}
            </main>
            {panels::right_panel(chat)}
        </div>
    }
}
