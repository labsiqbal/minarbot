use super::*;
use controls::dialog_open;
use routes::Channel;
use serde_json::json;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = MinarbotConversation, js_name = workLog)]
    fn work_log(turn: &str, name: &str) -> String;
}

#[derive(Deserialize)]
struct WorkEntry {
    text: String,
    state: Option<String>,
    #[serde(default)]
    prose: bool,
}

#[derive(Clone, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Change {
    id: String,
    name: String,
    instructions: String,
    members: Vec<String>,
    lead: Option<String>,
    revision: u64,
    archived: bool,
}

impl Change {
    fn valid(&self) -> bool {
        protocol::id(&self.id)
            && (1..=80).contains(&self.name.trim().encode_utf16().count())
            && (1..=8000).contains(&self.instructions.trim().encode_utf16().count())
            && self.revision > 0
            && self.revision <= 9_007_199_254_740_991
            && !self.members.is_empty()
            && self.members.iter().all(|id| protocol::id(id))
            && self
                .lead
                .as_ref()
                .is_none_or(|id| self.members.contains(id))
    }
}

pub(super) fn settings(room: Channel, dialog: NodeRef<leptos::html::Dialog>) -> impl IntoView {
    let directory = RwSignal::new(Vec::<Agent>::new());
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let key = StoredValue::new(format!("minarbot:channel-command:{}", room.id));
    let endpoint = StoredValue::new(format!("/api/channels/{}/edit", room.id));
    let raw = saved(&key.get_value());
    let command = raw
        .as_ref()
        .and_then(|raw| serde_json::from_str::<Change>(raw).ok())
        .filter(Change::valid);
    let blocked = RwSignal::new(raw.is_some() && command.is_none());
    let name = RwSignal::new(
        command
            .as_ref()
            .map_or(room.name.clone(), |value| value.name.clone()),
    );
    let instructions = RwSignal::new(
        command
            .as_ref()
            .map_or(room.instructions, |value| value.instructions.clone()),
    );
    let members = RwSignal::new(
        command
            .as_ref()
            .map_or(room.members, |value| value.members.clone()),
    );
    let lead = RwSignal::new(
        command
            .as_ref()
            .map_or(room.lead, |value| value.lead.clone())
            .unwrap_or_default(),
    );
    let archived = RwSignal::new(
        command
            .as_ref()
            .map_or(room.archived != 0, |value| value.archived),
    );
    let pending = RwSignal::new(command);
    let locked = move || busy.get() || pending.with(Option::is_some) || blocked.get();
    let archive = NodeRef::<leptos::html::Dialog>::new();
    let discard = NodeRef::<leptos::html::Dialog>::new();
    let loaded = RwSignal::new(false);
    let load = move || {
        if loaded.get_untracked() {
            return;
        }
        leptos::task::spawn_local(async move {
            let result = get::<routes::Directory>("/api/agents").await;
            match result {
                Ok(value) => {
                    directory.try_set(value.agents);
                    loaded.try_set(true);
                }
                Err(reason) => {
                    error.try_set(reason.message);
                }
            }
        });
    };
    let launch = window_event_listener(
        leptos::ev::Custom::<web_sys::Event>::new("minarbot:section-settings"),
        move |_| {
            load();
            dialog_open(dialog);
        },
    );
    on_cleanup(move || launch.remove());
    let original = room.archived != 0;
    let submit = move || {
        if busy.get_untracked() || blocked.get_untracked() {
            return;
        }
        let value = pending.get_untracked().or_else(|| {
            Some(Change {
                id: uuid()?,
                name: name.get_untracked().trim().into(),
                instructions: instructions.get_untracked().trim().into(),
                members: members.get_untracked(),
                lead: Some(lead.get_untracked()).filter(|value| !value.is_empty()),
                revision: room.revision,
                archived: archived.get_untracked(),
            })
        });
        let Some(value) = value.filter(Change::valid) else {
            error.set("Enter a section name and purpose, and choose at least one member.".into());
            return;
        };
        let Ok(raw) = serde_json::to_string(&value) else {
            return;
        };
        let key = key.get_value();
        if !save(&key, &raw) {
            error.set("Section request was not saved in the browser. Check site storage before trying again.".into());
            return;
        }
        error.set(String::new());
        busy.set(true);
        pending.set(Some(value));
        let path = endpoint.get_value();
        leptos::task::spawn_local(async move {
            let result = post::<Channel>(&path, &raw).await;
            if busy.is_disposed() {
                return;
            }
            busy.set(false);
            match result {
                Ok(_) if remove(&key) => {
                    let _ = window().location().reload();
                }
                Ok(_) => error.set("Section saved. Recovery data could not be cleared.".into()),
                Err(reason) => {
                    if reason.definitive && remove(&key) {
                        pending.set(None);
                    }
                    error.set(reason.message);
                }
            }
        });
    };
    view! {
        <dialog class="k-dialog k-dialog--lg workspace-dialog" node_ref=dialog aria-label="Section settings"
            on:cancel=move |event: web_sys::Event| { if busy.get_untracked() { event.prevent_default(); } }>
            <div class="k-dialog__head"><h2 class="k-dialog__title">"Section settings"</h2></div>
            <div class="k-dialog__body k-stack">
                <p role="alert" class="k-field__error" hidden=move || !blocked.get()>"Section recovery data could not be read. Check section settings before discarding it."</p>
                <p role="status" class="k-field__hint" hidden=move || pending.with(Option::is_none)>"Request result is not confirmed yet. Retry the same request to check the result."</p>
                <form id="channel-form" class="k-fieldset" on:submit=move |event| { event.prevent_default(); submit(); }>
                    <label class="k-field"><span class="k-field__label">"Section name"</span>
                        <input class="k-input" autofocus maxlength="80" required readonly=locked prop:value=name on:input=move |event| name.set(event_target_value(&event)) />
                    </label>
                    <label class="k-field"><span class="k-field__label">"Purpose"</span>
                        <textarea class="k-input" maxlength="8000" rows="3" required readonly=locked prop:value=instructions on:input=move |event| instructions.set(event_target_value(&event))></textarea>
                    </label>
                    <fieldset class="k-fieldset" disabled=locked>
                        <legend class="k-field__label">"Members"</legend>
                        <p role="status" hidden=move || loaded.get() || !error.with(String::is_empty)>"Loading members…"</p>
                        {move || directory.get().into_iter().map(|agent| {
                            let id = agent.id.clone();
                            view! { <label class="k-check"><input type="checkbox" prop:checked=move || members.with(|members| members.contains(&id))
                                on:change=move |event| {
                                    if event_target_checked(&event) { members.update(|members| members.push(agent.id.clone())); }
                                    else {
                                        members.update(|members| members.retain(|id| id != &agent.id));
                                        if lead.get_untracked() == agent.id { lead.set(String::new()); }
                                    }
                                } />{agent.name}</label> }
                        }).collect_view()}
                    </fieldset>
                    <div class="k-field"><label class="k-field__label" for="section-lead">"Lead"</label>
                        <crate::select::Select name="section-lead" label="Lead" described_by=Some("section-lead-hint")
                            value=Signal::derive(move || lead.get()) change=Callback::new(move |value| lead.set(value)) disabled=Signal::derive(locked)
                            options=Signal::derive(move || std::iter::once((String::new(), "No lead".into())).chain(
                                directory.get().into_iter().filter(|agent| members.with(|members| members.contains(&agent.id)))
                                    .map(|agent| (agent.id, agent.name))
                            ).collect()) />
                        <span id="section-lead-hint" class="k-field__hint">"Optional. Gives a short reply when everyone stays quiet."</span>
                    </div>
                    <p role="status" hidden=move || archived.get() == original>
                        {move || if archived.get() { "Section will be archived and unfinished work cancelled when you save changes." } else { "Section will be restored when you save changes." }}
                    </p>
                    <p role="alert" class="k-field__error" hidden=move || error.with(String::is_empty)>{error}</p>
                </form>
            </div>
            <div class="k-dialog__foot k-row-actions">
                <button type="button" class="k-btn k-btn--ghost k-dialog__foot-start" disabled=locked on:click=move |_| dialog_open(archive)>
                    {move || if archived.get() { "Restore section" } else { "Archive section" }}
                </button>
                <button type="button" class="k-btn k-btn--ghost" hidden=move || !blocked.get() && pending.with(Option::is_none) disabled=busy on:click=move |_| dialog_open(discard)>"Discard section recovery"</button>
                <button type="button" class="k-btn k-btn--ghost" hidden=move || error.with(String::is_empty) || locked() on:click=move |_| { let _ = window().location().reload(); }>"Discard changes and reload"</button>
                <form method="dialog"><button type="submit" class="k-btn k-btn--secondary" disabled=busy>"Cancel"</button></form>
                <button type="submit" form="channel-form" class="k-btn k-btn--primary" disabled=move || busy.get() || blocked.get() || members.with(Vec::is_empty)>
                    {move || if busy.get() { "Saving..." } else if pending.with(Option::is_some) { "Retry saving section" } else { "Save section" }}
                </button>
            </div>
        </dialog>
        <dialog class="conversation-dialog" node_ref=archive aria-label=move || if archived.get() { "Restore section?" } else { "Archive section?" }>
            <form method="dialog" class="k-stack">
                <p>{move || if archived.get() { "Save changes to restore this section." } else { "Archive section and cancel unfinished work when you save changes." }}</p>
                <div class="k-row-actions">
                    <button type="submit" class="k-btn k-btn--secondary">"Cancel"</button>
                    <button type="submit" class="k-btn k-btn--primary" on:click=move |_| archived.update(|value| *value = !*value)>
                        {move || if archived.get() { "Restore" } else { "Archive" }}
                    </button>
                </div>
            </form>
        </dialog>
        <dialog class="conversation-dialog" node_ref=discard aria-label="Discard section recovery data?">
            <form method="dialog" class="k-stack">
                <p>"Changes may already be saved. Check the list and section settings before creating a new request."</p>
                <div class="k-row-actions">
                    <button type="submit" class="k-btn k-btn--secondary">"Cancel"</button>
                    <button type="submit" class="k-btn k-btn--danger" on:click=move |_| {
                        if remove(&key.get_value()) { pending.set(None); blocked.set(false); error.set(String::new()); }
                        else { error.set("Recovery data could not be deleted. Check site storage.".into()); }
                    }>"Discard"</button>
                </div>
            </form>
        </dialog>
    }
}

pub(super) fn read(chat: Chat, follow: RwSignal<bool>) {
    let Some(room) = chat.config.with_value(|config| config.room.clone()) else {
        return;
    };
    let position = RwSignal::new(room.read_position);
    let id = StoredValue::new(room.id);
    let busy = RwSignal::new(false);
    let read = move || {
        if busy.try_get_untracked() != Some(false)
            || !follow.get_untracked()
            || !chat.focus.with_untracked(String::is_empty)
            || !chat.history.with_untracked(Vec::is_empty)
            || chat.finding.get_untracked()
            || chat.turns.with_untracked(Option::is_none)
            || document()
                .query_selector("dialog:modal")
                .ok()
                .flatten()
                .is_some()
        {
            return;
        }
        let id = id.get_value();
        busy.set(true);
        leptos::task::spawn_local(async move {
            let result = get::<Vec<Channel>>("/api/channels").await;
            if busy.is_disposed() {
                return;
            }
            if let Ok(rows) = result {
                if let Some(room) = rows
                    .into_iter()
                    .find(|room| room.id == id && room.activity > position.get_untracked())
                {
                    if let Some(request) = uuid() {
                        match post::<Channel>(
                            &format!("/api/channels/{id}/read"),
                            &json!({"id":request,"position":room.activity}).to_string(),
                        )
                        .await
                        {
                            Ok(updated) => {
                                position.try_set(updated.read_position);
                            }
                            Err(reason) => {
                                chat.error.try_set(reason.message);
                            }
                        }
                    }
                }
            }
            busy.try_set(false);
        });
    };
    let interval = set_interval_with_handle(read, Duration::from_millis(1200)).ok();
    on_cleanup(move || {
        if let Some(interval) = interval {
            interval.clear();
        }
    });
}

pub(super) fn stop(chat: Chat, rows: Memo<Vec<Turn>>, icon: String) -> impl IntoView {
    let active = Memo::new(move |_| {
        rows.with(|rows| {
            rows.iter()
                .filter_map(|turn| turn.room.as_ref())
                .find_map(|room| {
                    let root = room["root"].as_str().filter(|value| protocol::id(value))?;
                    let generation = room["generation"]
                        .as_str()
                        .filter(|value| protocol::id(value))?;
                    let key = format!("minarbot:room-stop:{}/turns:{root}", chat.base());
                    (room["active"] == true || saved(&key).is_some())
                        .then(|| (root.to_owned(), generation.to_owned()))
                })
        })
    });
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    view! {
        <span class="composer-stop" hidden=move || active.with(Option::is_none) || chat.config.with_value(|config| config.archived)>
            <button type="button" class="composer__cta composer__cta--stop"
                aria-label=move || if busy.get() { "Stopping…" } else if error.with(String::is_empty) { "Stop" } else { "Retry Stop" }
                title="Stop" disabled=busy on:click=move |_| {
                    if busy.get_untracked() { return; }
                    let Some((root, generation)) = active.get_untracked() else { return; };
                    let key = format!("minarbot:room-stop:{}/turns:{root}", chat.base());
                    let raw = if let Some(raw) = saved(&key) {
                        let valid = serde_json::from_str::<Value>(&raw).ok().is_some_and(|value| value["generation"] == generation
                            && value["id"].as_str().is_some_and(protocol::id));
                        if !valid { error.set("Stop recovery could not be read. Check conversation history.".into()); return; }
                        raw
                    } else {
                        let Some(id) = uuid() else { return; };
                        json!({"id": id, "generation": generation}).to_string()
                    };
                    if !save(&key, &raw) { error.set("Stop request could not be saved.".into()); return; }
                    let path = format!("{}/discussions/{root}/stop", chat.base());
                    error.set(String::new()); busy.set(true);
                    leptos::task::spawn_local(async move {
                        let result = post::<Value>(&path, &raw).await;
                        if busy.is_disposed() { return; }
                        busy.set(false);
                        match result {
                            Ok(_) if remove(&key) => { history::focus_element("#message"); chat.refresh(); }
                            Ok(_) => error.set("Stop confirmed. Recovery data could not be cleared.".into()),
                            Err(reason) => error.set(reason.message),
                        }
                    });
                }><span class="appearance-icon" inner_html=icon></span></button>
            <span class="message-error" role="alert" hidden=move || error.with(String::is_empty)>{error}</span>
        </span>
    }
}

#[derive(Clone, PartialEq)]
struct DiscussionRow {
    id: String,
    agent: String,
    name: String,
    text: Option<String>,
    request: bool,
    target: Option<String>,
}

pub(super) fn discussion(chat: Chat, root: RwSignal<Turn>) -> impl IntoView {
    let messages = Memo::new(move |_| root.with(|turn| turn.room.clone()));
    let rows =
        Memo::new(move |_| {
            let Some(room) = messages.get() else {
                return Vec::new();
            };
            let mut rows = room["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|message| {
                    let id = message["turn"]
                        .as_str()
                        .filter(|id| protocol::id(id))?
                        .to_owned();
                    Some(DiscussionRow {
                        id,
                        agent: message["agent"].as_str().unwrap_or_default().into(),
                        name: message["name"].as_str().unwrap_or("Agent").into(),
                        text: message["text"].as_str().map(str::to_owned),
                        request: message["kind"] == "request",
                        target: message["target_name"].as_str().map(str::to_owned),
                    })
                })
                .collect::<Vec<_>>();
            chat.turns.with(|turns| {
                for turn in turns.iter().flatten() {
                    if !turn.room.as_ref().is_some_and(|value| {
                        value["root"] == room["root"] && value["silent"].is_null()
                    }) || rows.iter().any(|row| !row.request && row.id == turn.id)
                        || turn.status == "cancelled"
                            && turn.activity.calls.is_empty()
                            && turn.partial.is_none()
                            && turn.activity.stream.is_empty()
                            && turn.activity.tools.is_empty()
                            && turn.activity.harness.is_empty()
                            && turn.harness_approvals.is_empty()
                    {
                        continue;
                    }
                    rows.push(DiscussionRow {
                        id: turn.id.clone(),
                        agent: turn.agent.clone(),
                        name: turn
                            .agent_name
                            .clone()
                            .unwrap_or_else(|| turn.agent.clone()),
                        text: None,
                        request: false,
                        target: None,
                    });
                }
            });
            rows
        });
    view! {
        <div class="room-conversation" aria-label="Section conversation">
            <For each=move || rows.get() key=|row| (row.request, row.id.clone())
                children=move |item| {
                    if item.request {
                        let member = chat.config.with_value(|config| config.members.iter().find(|member| member.id == item.agent).cloned());
                        view! { <div class="agent-message">
                            <div class="agent-identity">{avatar(member, &item.name, false, 24)}<strong class="message-name">{item.name}</strong>
                                {item.target.map(|target| view! { <span class="message-role">{format!("to {target}")}</span> })}
                            </div>
                            <div class="message-column"><div class="room-message-request legacy">{body(item.text.unwrap_or_default(), chat.base())}</div></div>
                        </div> }.into_any()
                    } else {
                        member_reply(chat, item, rows).into_any()
                    }
                } />
            {move || messages.with(|value| value.as_ref().and_then(|room| room["note"].as_str()).map(str::to_owned)).map(|note| view! {
                <p class="room-system-note" role="status">{note}</p>
            })}
        </div>
    }
}

fn member_reply(chat: Chat, item: DiscussionRow, rows: Memo<Vec<DiscussionRow>>) -> impl IntoView {
    let member = chat.config.with_value(|config| {
        config
            .members
            .iter()
            .find(|member| member.id == item.agent)
            .cloned()
    });
    let name = member
        .as_ref()
        .map_or(item.name.clone(), |member| member.name.clone());
    let id = StoredValue::new(item.id.clone());
    let current = move || {
        chat.turns.with(|turns| {
            turns
                .iter()
                .flatten()
                .find(|turn| id.with_value(|id| turn.id == *id))
                .cloned()
        })
    };
    let row = RwSignal::new(current().unwrap_or_else(|| Turn {
        id: item.id,
        agent: item.agent,
        status: "done".into(),
        ..Turn::default()
    }));
    Effect::new(move |_| {
        if let Some(turn) = current() {
            if row.with_untracked(|old| *old != turn) {
                row.set(turn);
            }
        }
    });
    let answer = Memo::new(move |_| {
        rows.with(|values| {
            values
                .iter()
                .find(|value| !value.request && id.with_value(|id| value.id == *id))
                .and_then(|item| item.text.clone())
        })
    });
    let text = Memo::new(move |_| {
        answer
            .get()
            .or_else(|| row.with(|turn| turn.partial.as_ref().map(|partial| partial.text.clone())))
            .unwrap_or_default()
    });
    let blocks = Memo::new(move |_| {
        text.with(|text| {
            markdown_blocks(text)
                .iter()
                .filter_map(|value| value.as_string())
                .enumerate()
                .collect::<Vec<_>>()
        })
    });
    let base = StoredValue::new(chat.base());
    let now = RwSignal::new(js_sys::Date::now());
    Effect::new(move |_| {
        if row.with(Turn::live_activity) {
            let timer = set_interval_with_handle(
                move || now.set(js_sys::Date::now()),
                Duration::from_secs(1),
            )
            .ok();
            on_cleanup(move || {
                if let Some(timer) = timer {
                    timer.clear();
                }
            });
        }
    });
    let label = move || row.with(|turn| activity_label(turn, now.get()));
    let has_work = move || {
        row.with(|turn| {
            !turn.activity.stream.is_empty()
                || !turn.activity.calls.is_empty()
                || !turn.activity.tools.is_empty()
                || !turn.activity.harness.is_empty()
        })
    };
    let open = RwSignal::new(false);
    let icons = use_context::<Icons>().unwrap_or_default();
    let from = StoredValue::new(name.clone());
    let chevron = StoredValue::new(icons.chevron);
    let result_name = move || {
        format!(
            "{} from {}",
            if row.with(Turn::streaming) {
                "Streaming answer"
            } else {
                "Answer"
            },
            from.get_value()
        )
    };
    view! {
        <section class="room-agent" aria-label=format!("Reply from {name}") tabindex="-1" id=move || format!("room-reply-{}", id.get_value())>
            <div class=move || if answer.with(Option::is_some) { "room-agent__line" } else { "room-pending" } class:is-working=move || row.with(Turn::live_activity)>
                {avatar(member, &name, false, 20)}
                <strong class="room-agent__name message-name" title=name.clone()>{name.clone()}</strong>
                <Show when=has_work fallback=move || view! { <span class="room-agent__status">
                    <span class="activity-working" aria-hidden="true" hidden=move || !row.with(|turn| turn.status == "running" && turn.live_activity())></span>{label}
                </span> }>
                <button type="button" class="room-agent__status"
                    aria-expanded=move || if open.get() { "true" } else { "false" } aria-label=label
                    on:click=move |_| open.update(|open| *open = !*open)>
                    <span class="activity-working" aria-hidden="true" hidden=move || !row.with(|turn| turn.status == "running" && turn.live_activity())></span>
                    {label}<span class="appearance-icon k-collapsible__chev" inner_html=chevron.get_value()></span>
                </button>
                </Show>
            </div>
            <Show when=move || open.get() && has_work()>
                <div class="room-agent__activity">
                    <ol class="activity-list" aria-label="Turn activity">
                        {move || row.with(|turn| {
                            let data = json!({"status": turn.status, "room": turn.room,
                                "partial": turn.partial.as_ref().map(|part| json!({"seq": part.seq, "segment": part.segment})),
                                "activity": {"stream": turn.activity.stream, "tools": turn.activity.tools,
                                    "harness": turn.activity.harness, "fallback_model": turn.activity.fallback_model}});
                            serde_json::from_str::<Vec<WorkEntry>>(&work_log(&data.to_string(), &from.get_value()))
                                .unwrap_or_default().into_iter().map(|step| view! {
                                    <li>{if step.prose { view! { <div class="legacy">{body(step.text, base.get_value())}</div> }.into_any() }
                                    else { view! { <span>{step.text}{step.state.map(|state| view! {
                                        <span class="activity-step-state" data-state=state.clone()>{format!(" {state}")}</span>
                                    })}</span> }.into_any() }}</li>
                                }).collect_view()
                        })}
                    </ol>
                    <button type="button" class="link-btn link-btn--small" on:click=move |_| chat.inspect(id.get_value())>"Inspect run"</button>
                </div>
            </Show>
            {panels::rich_replies(chat, row)}
            <Show when=move || !text.with(String::is_empty)><div class="message-body" aria-label=result_name>
                <div class="legacy stream-markdown">
                    <For each=move || blocks.get() key=|(index, text)| (*index, text.clone())
                        children=move |(_, text)| body(text, base.get_value()) />
                </div>
            </div></Show>
            {move || row.with(|turn| turn.generated.clone()).into_iter().filter(|file| !file.deleted).map(|file| attachment(file, base.get_value())).collect_view()}
            <p class="message-error" hidden=move || row.with(|turn| turn.error.is_none() || turn.status == "cancelled")>
                {move || row.with(|turn| turn.error.clone())}
            </p>
            {message_actions::events(chat, row)}
            {controls::turn_controls(chat, row, true)}
        </section>
    }
}

pub(super) fn addressed_member(chat: Chat) -> impl IntoView {
    let options = Signal::derive(move || {
        let mut options = vec![(String::new(), "Everyone".to_owned())];
        chat.active.with(|active| {
            options.extend(active.iter().map(|run| {
                let name = chat.config.with_value(|config| {
                    config
                        .members
                        .iter()
                        .find(|member| member.id == run.agent)
                        .map_or_else(|| run.agent.clone(), |member| member.name.clone())
                });
                (run.id.clone(), name)
            }));
        });
        options
    });
    view! {
        <div hidden=move || chat.active.with(Vec::is_empty)>
            <crate::select::Select name="addressed-members" label="Addressed member" described_by=None small=true
                value=Signal::derive(move || chat.target().map(|run| run.id).unwrap_or_default())
                change=Callback::new(move |value| chat.addressed.set(value))
                disabled=Signal::derive(move || chat.locked()) options=options />
        </div>
    }
}
