mod controls;
mod panels;
mod protocol;
mod queue;
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
    stream: Vec<Segment>,
    tools: Vec<Value>,
    harness: Vec<Value>,
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

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct Turn {
    id: String,
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
            && (self.questions.iter().any(|q| q["answer"].is_null())
                || self
                    .approvals
                    .iter()
                    .chain(&self.harness_approvals)
                    .any(|a| a["state"] == "approval"))
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
    query: RwSignal<String>,
    before: RwSignal<Option<u64>>,
    tab: RwSignal<&'static str>,
    details: RwSignal<bool>,
    run: RwSignal<String>,
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
            active: RwSignal::new(Vec::new()),
            addressed: RwSignal::new(String::new()),
            focus: RwSignal::new(focus),
            query: RwSignal::new(String::new()),
            before: RwSignal::new(None),
            tab: RwSignal::new("chats"),
            details: RwSignal::new(false),
            run: RwSignal::new(String::new()),
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

    fn inspect(self, turn: String) {
        self.run.set(turn);
        self.tab.set("run");
        self.details.set(true);
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
    fn markdown(text: &str, scope: &str) -> String;

    #[wasm_bindgen(js_namespace = MinarbotConversation, js_name = markdownBlocks)]
    fn markdown_blocks(text: &str) -> js_sys::Array;
}

fn body(text: String, base: String) -> impl IntoView {
    view! { <div class="markdown" inner_html=markdown(&text, &base)></div> }
}

fn attachment(file: FileRef, base: String) -> impl IntoView {
    let path = format!("{base}/files/{}", file.id);
    let name = file.name;
    let download = name.clone();
    view! { <a class="k-link" href=path download=download>{name}</a> }
}

fn turn_view(chat: Chat, row: RwSignal<Turn>, id: String) -> impl IntoView {
    let inspect_id = id.clone();
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
        row.with(|turn| {
            turn.activity
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
                .cloned()
                .collect::<Vec<_>>()
        })
    });
    let prompt =
        Memo::new(move |_| row.with(|turn| turn.parent.is_none().then(|| turn.text.clone())));
    let outputs = Memo::new(move |_| row.with(|turn| turn.generated.clone()));
    view! {
        <article class="turn-block" id=format!("turn-{id}") tabindex="-1">
            <div class="human-message" hidden=move || prompt.get().is_none()>
                <div class="human-bubble message-plain">{prompt}</div>
                {move || row.with(|turn| turn.files.iter().filter(|file| !file.deleted).cloned().map(|file| attachment(file, base.clone())).collect_view())}
            </div>
            <div class="agent-message">
                <div class="message-column">
                    <div class="agent-identity">
                        <strong class="message-name">{move || row.with(|turn| turn.agent_name.clone()).unwrap_or_else(|| chat.config.with_value(|c| c.name.clone()))}</strong>
                        <span class="message-role activity-label" role="status">{move || row.with(|turn| if turn.harness_approvals.iter().any(|approval| approval["kind"] == "form" && approval["state"] == "approval") { "Waiting for your input" } else if turn.needs_you() { "Needs you" } else if turn.paused != 0 && turn.active() { "Paused" } else {
                            match turn.status.as_str() { "queued" => "Queued", "running" => "Working…", "done" => "Done", "cancelled" => "Stopped", "failed" => "Failed", "interrupted" => "Interrupted", _ => "Waiting" }
                        })}</span>
                        <button type="button" class="k-btn k-btn--sm k-btn--ghost" aria-label="Inspect run" on:click=move |_| chat.inspect(inspect_id.clone())>"Inspect"</button>
                    </div>
                    <Show when=move || row.with(|turn| turn.goal_continuation)>
                        <p class="message-muted">"Goal continued"</p>
                    </Show>
                    <details class="steps" hidden=move || activity.with(Vec::is_empty)>
                        <summary class="steps__toggle">"Work log"</summary>
                        {move || activity.get().into_iter().map(|step| view! { <p>{step.text}</p> }).collect_view()}
                    </details>
                    <div class="message-body" aria-label=label hidden=move || answer.with(String::is_empty)>
                        <div class="legacy stream-markdown">
                            <For each=move || blocks.get() key=|(index, block): &(usize, String)| (*index, block.clone())
                                children=move |(_, block)| body(block, text_base.clone()) />
                        </div>
                    </div>
                    <p class="message-muted">{move || row.with(|turn| turn.dispatch_label.clone())}</p>
                    <p class="message-error" hidden=move || row.with(|turn| turn.error.is_none())>{move || row.with(|turn| turn.error.clone())}</p>
                    {move || outputs.get().into_iter().filter(|file| !file.deleted).map(|file| attachment(file, rich_base.clone())).collect_view()}
                    {panels::rich_replies(chat, row)}
                    {panels::exchanges(chat, row)}
                    {controls::turn_controls(chat, row)}
                    {queue::editor(chat, row)}
                </div>
            </div>
        </article>
    }
}

/// Mount inside the conversation region of the Leptos shell.
/// Key this component by conversation scope when changing routes.
pub fn shell(config: ConversationConfig) -> impl IntoView {
    let chat = Chat::new(config.clone());
    let snapshots = StoredValue::new(HashMap::new());
    subscribe(chat, snapshots);
    polling(chat, snapshots);
    let filter = RwSignal::new(String::new());
    let searching = RwSignal::new(false);
    let rows = Memo::new(move |_| chat.turns.with(|rows| rows.clone().unwrap_or_default()));
    let timeline = NodeRef::<leptos::html::Div>::new();
    let follow = RwSignal::new(true);
    Effect::new(move |_| {
        rows.track();
        if follow.get_untracked() {
            let focus = chat.focus.get_untracked();
            request_animation_frame(move || {
                if !focus.is_empty() {
                    if let Some(target) = document()
                        .get_element_by_id(&format!("turn-{focus}"))
                        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
                    {
                        target.scroll_into_view();
                        let _ = target.focus();
                        follow.set(false);
                    }
                } else if let Some(node) = timeline.get_untracked() {
                    node.set_scroll_top(node.scroll_height());
                }
            });
        }
    });
    let label = format!("Conversation with {}", config.name);
    view! {
        <div class="conversation-layout bs-root">
            <main class="chat conversation-chat" aria-label=label>
                <header class="conversation-header">
                    <h1>{config.name}</h1>
                    <div class="k-row-actions">
                        <button type="button" class="k-btn k-btn--sm k-btn--ghost" aria-expanded=move || searching.get().to_string()
                            on:click=move |_| searching.update(|open| *open = !*open)>"Find messages"</button>
                        <button type="button" class="k-btn k-btn--sm k-btn--ghost" aria-expanded=move || chat.details.get().to_string()
                            on:click=move |_| chat.details.update(|open| *open = !*open)>
                            {move || if chat.details.get() { "Hide details" } else { "Show details" }}
                        </button>
                    </div>
                </header>
                <form class="conversation-find k-inline" role="search" hidden=move || !searching.get() on:submit=move |event| {
                    event.prevent_default();
                    chat.before.set(None);
                    chat.focus.set(String::new());
                    chat.query.set(filter.get_untracked().trim().into());
                }>
                    <input type="search" class="k-input" aria-label="Find messages" maxlength="200" prop:value=filter
                        on:input=move |event| filter.set(event_target_value(&event)) />
                    <button type="submit" class="k-btn k-btn--sm k-btn--primary">"Find"</button>
                    <button type="button" class="k-btn k-btn--sm k-btn--ghost" on:click=move |_| {
                        chat.query.set(String::new()); filter.set(String::new()); searching.set(false);
                    }>"Close search"</button>
                </form>
                <div class="conversation-scroll" node_ref=timeline on:scroll=move |_| {
                    if let Some(node) = timeline.get_untracked() { follow.set(node.scroll_height() - node.client_height() - node.scroll_top() <= 80); }
                }>
                    <div class="transcript__col">
                        <p role="status" hidden=move || chat.turns.with(Option::is_some)>"Loading conversation…"</p>
                        <p class="message-error" role="alert" hidden=move || chat.read_error.with(String::is_empty)>{chat.read_error}</p>
                        <p class="message-muted" hidden=move || chat.turns.with(|rows| rows.as_ref().is_none_or(|rows| !rows.is_empty()))>
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
                                turn_view(chat, row, id)
                            } />
                    </div>
                </div>
                {controls::composer(chat, rows)}
            </main>
            {panels::right_panel(chat)}
        </div>
    }
}
