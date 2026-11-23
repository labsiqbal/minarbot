use super::*;
use serde::Serialize;

#[derive(Clone, Default, Deserialize, Serialize)]
struct Draft {
    text: String,
    mentions: Vec<String>,
    pending: Option<Command>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Command {
    request_id: String,
    section_id: String,
    text: String,
    mentions: Vec<String>,
}

impl Draft {
    fn load(key: &str, section: &str) -> Result<Self, Failure> {
        let invalid =
            || Failure::message("Saved draft could not be read. Discard recovery before sending.");
        let value = storage()?.get_item(key).map_err(|_| invalid())?;
        let Some(value) = value else {
            return Ok(Self::default());
        };
        let draft: Self = serde_json::from_str(&value).map_err(|_| invalid())?;
        let valid_id = crate::workspace_route::valid_id;
        let valid = |text: &str, mentions: &[String]| {
            text.encode_utf16().count() <= 16_000
                && mentions.len() <= 100
                && mentions.iter().all(|id| valid_id(id))
                && mentions
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    == mentions.len()
        };
        if !valid(&draft.text, &draft.mentions)
            || draft.pending.as_ref().is_some_and(|c| {
                !valid_id(&c.request_id)
                    || c.section_id != section
                    || !valid(&c.text, &c.mentions)
                    || c.text != draft.text
                    || c.mentions != draft.mentions
            })
        {
            return Err(invalid());
        }
        Ok(draft)
    }
}

#[derive(Clone, Deserialize)]
struct Snapshot {
    messages: Vec<Message>,
    results: Vec<Frame>,
}

#[derive(Clone, Deserialize)]
struct Message {
    id: String,
    requester_account_id: String,
    text: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Frame {
    turn_id: String,
    kind: String,
    payload: Value,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Turn {
    id: String,
    message_id: String,
    agent_id: String,
    requester_account_id: String,
    payer_account_id: String,
    state: String,
}

#[derive(Clone, Copy)]
struct Composer {
    page: ProjectPage,
    draft: RwSignal<Draft>,
    key: StoredValue<String>,
    section: StoredValue<NamedSection>,
    busy: RwSignal<bool>,
    error: RwSignal<String>,
    blocked: RwSignal<bool>,
    viewer: bool,
}

impl Composer {
    fn persist(self, draft: Draft) -> bool {
        let value = serde_json::to_string(&draft).ok();
        if value.is_some_and(|value| self.key.with_value(|key| remember(key, &value).is_ok())) {
            self.draft.set(draft);
            true
        } else {
            self.blocked.set(true);
            self.error
                .set("Draft could not be saved. Check browser storage before sending.".into());
            false
        }
    }

    fn send(self) {
        if self.blocked.get_untracked()
            || self.busy.get_untracked()
            || self.viewer
            || !self.page.access.get_untracked()
        {
            return;
        }
        let draft = self.draft.get_untracked();
        if draft.text.trim().is_empty() {
            return;
        }
        let command = match draft.pending.clone() {
            Some(command) => command,
            None => {
                let request_id = crate::connection_ui::uuid();
                if request_id.is_empty() {
                    self.error
                        .set("Could not prepare a message. Reload this page.".into());
                    return;
                }
                Command {
                    request_id,
                    section_id: self.section.with_value(|section| section.id.clone()),
                    text: draft.text.clone(),
                    mentions: draft.mentions.clone(),
                }
            }
        };
        let body = match serde_json::to_value(&command) {
            Ok(body) => body,
            Err(_) => return,
        };
        if !self.persist(Draft {
            pending: Some(command),
            ..draft.clone()
        }) {
            return;
        }
        let key = self.key.get_value();
        let actor = self.page.actor.get_value();
        self.busy.set(true);
        self.error.set(String::new());
        spawn_local(async move {
            let result = actor.post::<Value>("/api/messages", body).await;
            if self.busy.try_get_untracked().is_none() {
                return;
            }
            match result {
                Ok(_) => match forget(&key) {
                    Ok(()) => self.draft.set(Draft::default()),
                    Err(failure) => {
                        self.error.set(failure.message);
                        self.blocked.set(true);
                    }
                },
                Err(failure) => {
                    if failure.status != 0
                        && failure.status < 500
                        && !matches!(failure.status, 401 | 429)
                        || failure.code == "node_offline"
                    {
                        self.persist(Draft {
                            pending: None,
                            ..draft
                        });
                    }
                    if failure.status == 401 {
                        self.page.failed(failure.clone());
                    }
                    self.error.try_set(if failure.code == "node_offline" {
                        "Waiting for member node. Retry when online.".into()
                    } else {
                        failure.message
                    });
                }
            }
            self.busy.try_set(false);
        });
    }

    fn locked(self) -> bool {
        self.viewer
            || self.busy.get()
            || self.blocked.get()
            || self.draft.with(|d| d.pending.is_some())
    }

    fn discard(self) {
        crate::system::kit::ask(
            Some(self.page.confirm),
            "Discard recovery?",
            "Check history first. The message may already have been accepted.",
            "Discard recovery",
            false,
            move || match self.key.with_value(|key| forget(key)) {
                Ok(()) => {
                    self.draft.set(Draft::default());
                    self.error.set(String::new());
                    self.blocked.set(false);
                }
                Err(failure) => self.error.set(failure.message),
            },
        );
    }

    fn agents(self) -> Vec<Participant> {
        self.section.with_value(|section| {
            self.page.participants.with(|agents| {
                agents
                    .iter()
                    .filter(|agent| agent.policy.section_ids.contains(&section.id))
                    .cloned()
                    .collect()
            })
        })
    }
}

#[component]
pub(super) fn Room(page: ProjectPage, section: NamedSection) -> impl IntoView {
    let actor = page.actor.get_value();
    let key = format!(
        "minarbot:hub-draft:{}:{}:{}",
        actor.account_id,
        actor.project_id.as_deref().unwrap_or_default(),
        section.id
    );
    let (draft, error) = match Draft::load(&key, &section.id) {
        Ok(draft) => (draft, String::new()),
        Err(failure) => (Draft::default(), failure.message),
    };
    let room = Composer {
        page,
        draft: RwSignal::new(draft),
        key: StoredValue::new(key),
        section: StoredValue::new(section.clone()),
        busy: RwSignal::new(false),
        error: RwSignal::new(error.clone()),
        blocked: RwSignal::new(!error.is_empty()),
        viewer: actor.role.as_deref() == Some("viewer"),
    };
    let snapshot = RwSignal::new(None::<Snapshot>);
    let turns = RwSignal::new(Vec::<Turn>::new());
    let connection = RwSignal::new("Connecting");
    let checking = RwSignal::new(false);
    let section_id = section.id.clone();
    let transcript = NodeRef::<leptos::html::Div>::new();
    let following = RwSignal::new(true);
    let alive = RwSignal::new(true);
    on_cleanup(move || {
        alive.try_set(false);
    });
    listen(
        &format!("/api/events?sectionId={}", encode(&section.id)),
        |_| {
            vec![
                (
                    "section.snapshot",
                    Closure::new(move |event: MessageEvent| {
                        match event
                            .data()
                            .as_string()
                            .and_then(|raw| serde_json::from_str(&raw).ok())
                        {
                            Some(value) => snapshot.set(Some(value)),
                            None => room
                                .error
                                .set("Stream data could not be read. Reload this section.".into()),
                        }
                    }),
                ),
                (
                    "turn.snapshot",
                    Closure::new(move |event: MessageEvent| {
                        match event
                            .data()
                            .as_string()
                            .and_then(|raw| serde_json::from_str(&raw).ok())
                        {
                            Some(value) => turns.set(value),
                            None => room
                                .error
                                .set("Turn status could not be read. Reload this section.".into()),
                        }
                    }),
                ),
            ]
        },
        move || {
            connection.set("Reconnecting");
            if checking.get_untracked() {
                return;
            }
            checking.set(true);
            let section_id = section_id.clone();
            spawn_local(async move {
                let result =
                    read::<Snapshot>(&format!("/api/history?sectionId={}", encode(&section_id)))
                        .await;
                if alive.try_get_untracked() != Some(true) {
                    return;
                }
                match result {
                    Ok(value) => snapshot.set(Some(value)),
                    Err(failure) if failure.access_ended() => page.failed(failure),
                    Err(failure) => {
                        room.error.try_set(failure.message);
                    }
                }
                checking.set(false);
            });
        },
        move || connection.set("Live"),
    );
    Effect::new(move |_| {
        snapshot.track();
        turns.track();
        if following.get_untracked() {
            request_animation_frame(move || {
                if let Some(element) = transcript.try_get_untracked().flatten() {
                    element.set_scroll_top(element.scroll_height());
                }
            });
        }
    });
    let section_name = section.name;
    let label = format!("Message {section_name}");
    let placeholder = if room.viewer {
        "Viewers can read this section".into()
    } else {
        label.clone()
    };
    let greeting = format!("Start in {section_name}");
    view! {
        <div class="hub-room">
            <header class="hub-header"><h1><Icon name="hash" />{section_name}</h1><span class="k-badge" role="status">{connection}</span></header>
            <div class="hub-agent-strip" aria-label="Section agents">
                {move || {
                    let agents = room.agents();
                    if agents.is_empty() { return view! { <span class="hub-address">"No participating agents"</span> }.into_any(); }
                    agents.into_iter().map(|agent| {
                        let id = agent.id;
                        let selected = id.clone();
                        let online = agent.node_status == "ready";
                        view! { <button class="k-btn k-btn--sm k-btn--ghost" type="button"
                            disabled=move || room.locked()
                            aria-pressed=move || room.draft.with(|d| d.mentions.contains(&selected)).to_string()
                            on:click=move |_| {
                                let mut draft = room.draft.get_untracked();
                                if draft.mentions.contains(&id) {
                                    draft.mentions.retain(|mention| mention != &id);
                                } else if draft.mentions.len() < 16 {
                                    draft.mentions.push(id.clone());
                                }
                                room.persist(draft);
                            }>
                            <Icon name="bot" />{format!("@{}",agent.name)}<span class=if online { "k-badge k-badge--success" } else { "k-badge k-badge--warn" }>{if online { "Online" } else { "Offline" }}</span>
                        </button> }
                    }).collect_view().into_any()
                }}
            </div>
            {alert(room.error)}
            <Show when=move || room.blocked.get() || room.draft.with(|d| d.pending.is_some())>
                <div class="hub-notice"><div class="k-alert k-alert--warn" role="status">
                    <span>{move || if room.blocked.get() { "Draft recovery unavailable" } else if room.busy.get() { "Sending message…" } else { "Message not confirmed" }}</span>
                    <div class="hub-actions">
                        <Show when=move || !room.blocked.get() && !room.busy.get()>
                            <button class=BUTTON type="button" on:click=move |_| room.send()>"Retry message"</button>
                        </Show>
                        <button class=BUTTON type="button" disabled=room.busy on:click=move |_| room.discard()>"Discard recovery"</button>
                    </div>
                </div></div>
            </Show>
            <div class="transcript-wrap">
            <div class="transcript" role="log" aria-label="Conversation history" node_ref=transcript on:scroll=move |_| {
                if let Some(element) = transcript.get_untracked() {
                    following.set(element.scroll_height() - element.scroll_top() - element.client_height() < 80);
                }
            }>
                <div class="transcript__col">
                    {move || match snapshot.get() {
                        None => view! { <p role="status">"Loading messages…"</p> }.into_any(),
                        Some(snapshot) if snapshot.messages.is_empty() => view! { <div class="k-empty"><h2>{greeting.clone()}</h2><p>"Send a message or select an agent to mention."</p></div> }.into_any(),
                        Some(snapshot) => {
                            let current_turns = turns.get();
                            let members = page.members.get();
                            let agents = room.agents();
                            snapshot.messages.into_iter().map(|message| exchange(message, &snapshot.results, &current_turns, &members, &agents)).collect_view().into_any()
                        }
                    }}
                </div>
            </div>
            </div>
            <div class="composer-wrap">
                <div class="hub-mentions" hidden=move || room.draft.with(|d| d.mentions.is_empty())>{move || {
                    let agents = room.agents();
                    room.draft.with(|draft| draft.mentions.iter().map(|id| badge(format!("@{}",agents.iter().find(|agent| &agent.id == id).map_or("Unavailable agent",|agent| &agent.name)))).collect_view())
                }}</div>
                <form class="hub-composer" method="post" action="/api/messages" on:submit=move |event| { event.prevent_default(); room.send(); }>
                    <label class="k-field"><span class="k-field__label">{label}</span>
                        <textarea class="k-input k-textarea" name="text" rows="2" maxlength="16000" placeholder=placeholder
                            prop:value=move || room.draft.with(|d| d.text.clone())
                            readonly=move || room.locked()
                            on:input=move |event| { let mut draft = room.draft.get_untracked(); draft.text = event_target_value(&event); room.persist(draft); }
                            on:keydown=move |event| {
                                if event.key() == "Enter" && !event.shift_key() && !event.is_composing() {
                                    event.prevent_default();
                                    room.send();
                                }
                            }></textarea>
                    </label>
                    <button class="k-icon-btn" type="submit" aria-label=move || if room.draft.with(|d| d.pending.is_some()) { "Retry message" } else { "Send message" }
                        disabled=move || room.viewer || room.busy.get() || room.blocked.get() || room.draft.with(|d| d.text.trim().is_empty())><Icon name="send" /></button>
                </form>
            </div>
        </div>
    }
}

fn exchange(
    message: Message,
    results: &[Frame],
    turns: &[Turn],
    members: &[Member],
    agents: &[Participant],
) -> impl IntoView {
    let name = |id: &str| {
        members
            .iter()
            .find(|member| member.account_id == id)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| id.into())
    };
    let requester = name(&message.requester_account_id);
    let replies = turns.iter().filter(|turn| turn.message_id == message.id).map(|turn| {
        let agent = agents.iter().find(|agent| agent.agent_id == turn.agent_id);
        let agent_name = agent.map_or_else(|| turn.agent_id.clone(), |agent| agent.name.clone());
        let payer = agent.map_or_else(|| name(&turn.payer_account_id), |agent| agent.owner_name.clone());
        let requester = name(&turn.requester_account_id);
        let result = results.iter().rev().find(|frame| frame.turn_id == turn.id && matches!(frame.kind.as_str(), "turn.partial" | "turn.final" | "turn.failed"));
        let text = match turn.state.as_str() {
            "unknown" => "Outcome uncertain. Check local history before retrying.",
            "revoked" => "Authorization revoked.",
            _ => result.and_then(|result| result.payload["text"].as_str().or_else(|| result.payload["code"].as_str())).unwrap_or("Waiting for member node…"),
        }.to_owned();
        let status = match turn.state.as_str() {
            "admitted" => "Streaming",
            "done" => "Completed",
            "pending" => "Waiting for member node",
            "unknown" => "Uncertain",
            other => other,
        }.to_owned();
        let tone = match turn.state.as_str() {
            "done" => "is-done",
            "failed" | "unknown" => "is-failed",
            _ => "is-running",
        };
        view! { <article class="agent-message">
            <div class="hub-author"><Icon name="bot" /><strong>{agent_name}</strong><span>{format!("Owned by {payer}")}</span><span>{format!("Requested by {requester}")}</span></div>
            <div class="message-column">
                <div class="hub-reply">{text}</div>
                <div class="turn-meta agent-meta"><span class=format!("turn-status {tone}")><i aria-hidden="true"></i>{status}</span></div>
            </div>
        </article> }
    }).collect_view();
    view! { <div class="hub-exchange">
        <article class="human-message" tabindex="-1" aria-label=format!("Message from {requester}")>
            <div class="human-bubble message-plain">{message.text}</div><div class="turn-meta human-meta">{requester.clone()}</div>
        </article>
        {replies}
    </div> }
}
