use super::*;
use serde_json::json;

#[derive(Clone, Debug, Deserialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub role: Option<String>,
    #[serde(default)]
    pub runnable: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub avatar_revision: u64,
    pub model: Option<String>,
    pub runtime: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Thread {
    pub id: String,
    pub agent: String,
    pub name: String,
    #[serde(default)]
    pub archived: u8,
    #[serde(default)]
    pub needs_you: u8,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub deleted: u8,
}

#[derive(Deserialize)]
struct Directory {
    agents: Vec<Agent>,
    guide: Option<Agent>,
}

#[derive(Deserialize)]
struct Channel {
    id: String,
    name: String,
    section_name: Option<String>,
    members: Vec<String>,
    archived: u8,
}

enum Route {
    Agent(String, Option<String>),
    Channel(String),
}

fn route(path: &str) -> Option<Route> {
    let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
    match parts.as_slice() {
        ["agents", agent] if protocol::id(agent) => {
            Some(Route::Agent(agent.to_ascii_lowercase(), None))
        }
        ["agents", agent, "conversations", thread]
            if protocol::id(agent) && protocol::id(thread) =>
        {
            Some(Route::Agent(
                agent.to_ascii_lowercase(),
                Some(thread.to_ascii_lowercase()),
            ))
        }
        ["rooms", channel] if protocol::id(channel) => {
            Some(Route::Channel(channel.to_ascii_lowercase()))
        }
        _ => None,
    }
}

pub fn link(path: &str) -> String {
    format!("{path}?ui=leptos")
}

fn load_error(message: &str) -> ApiError {
    ApiError {
        message: message.into(),
        definitive: true,
    }
}

async fn load(route: Route) -> Result<ConversationConfig, ApiError> {
    let directory: Directory = get("/api/agents").await?;
    let agents: Vec<_> = directory
        .agents
        .into_iter()
        .chain(directory.guide)
        .collect();
    match route {
        Route::Agent(id, thread_id) => {
            let agent = agents
                .into_iter()
                .find(|agent| agent.id == id)
                .ok_or_else(|| load_error("Agent not found."))?;
            let thread = if let Some(id) = thread_id {
                let thread: Thread = get(&format!("/api/threads/{id}")).await?;
                if thread.agent != agent.id {
                    return Err(load_error("Conversation not found."));
                }
                Some(thread)
            } else {
                None
            };
            Ok(ConversationConfig {
                scope: thread
                    .as_ref()
                    .map_or_else(|| id.clone(), |thread| format!("thread:{}", thread.id)),
                base: thread.as_ref().map_or_else(
                    || format!("/api/agents/{id}"),
                    |thread| format!("/api/threads/{}", thread.id),
                ),
                name: thread
                    .as_ref()
                    .map_or_else(|| agent.name.clone(), |thread| thread.name.clone()),
                archived: agent.hidden
                    || thread.as_ref().is_some_and(|thread| thread.archived != 0),
                can_send: agent.runnable,
                agent: Some(agent),
                thread,
                members: Vec::new(),
                channel: false,
            })
        }
        Route::Channel(id) => {
            let channels: Vec<Channel> = get("/api/channels").await?;
            let channel = channels
                .into_iter()
                .find(|row| row.id == id)
                .ok_or_else(|| load_error("Section not found."))?;
            let members: Vec<_> = agents
                .into_iter()
                .filter(|agent| channel.members.contains(&agent.id))
                .collect();
            Ok(ConversationConfig {
                base: format!("/api/channels/{id}"),
                scope: format!("channel:{id}"),
                channel: true,
                name: channel.section_name.unwrap_or(channel.name),
                archived: channel.archived != 0,
                can_send: !members.is_empty() && members.iter().all(|agent| agent.runnable),
                agent: None,
                thread: None,
                members,
            })
        }
    }
}

pub fn standalone() -> impl IntoView {
    let config = RwSignal::new(None);
    let error = RwSignal::new(String::new());
    leptos::task::spawn_local(async move {
        let result = match window()
            .location()
            .pathname()
            .ok()
            .and_then(|path| route(&path))
        {
            Some(route) => load(route).await,
            None => Err(load_error("Conversation not found.")),
        };
        match result {
            Ok(value) => {
                document().set_title(&format!("{} · Minarbot", value.name));
                config.set(Some(value));
            }
            Err(reason) => error.set(reason.message),
        }
    });
    view! {
        {move || config.get().map(shell)}
        <p class="conversation-startup" role="status" hidden=move || config.with(Option::is_some) || !error.with(String::is_empty)>"Loading conversation…"</p>
        <p class="conversation-startup message-error" role="alert" hidden=move || error.with(String::is_empty)>{error}</p>
    }
}

pub fn chats(chat: Chat) -> impl IntoView {
    let config = chat.config.get_value();
    if config.channel {
        return view! {
            <ul class="right-panel__members" aria-label="Section members">
                {config.members.into_iter().map(|member| view! {
                    <li><a class="k-link" href=link(&format!("/agents/{}", member.id))>{member.name}</a><span>{member.role}</span></li>
                }).collect_view()}
            </ul>
        }.into_any();
    }
    let Some(agent) = config.agent else {
        return ().into_any();
    };
    let base = format!("/agents/{}", agent.id);
    let api = StoredValue::new(format!("/api{base}/threads"));
    let threads = RwSignal::new(Vec::<Thread>::new());
    let error = RwSignal::new(String::new());
    let show_archived = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let current = config.thread.map(|thread| thread.id);
    let main = current.is_none();
    let load = move || {
        let path = api.get_value();
        leptos::task::spawn_local(async move {
            match get::<Vec<Thread>>(&path).await {
                Ok(rows) => {
                    threads.try_set(rows);
                }
                Err(reason) => {
                    error.try_set(reason.message);
                }
            }
        });
    };
    load();
    let owner = StoredValue::new(agent.id.clone());
    let key = StoredValue::new(format!("minarbot:thread-command:{}", owner.get_value()));
    let pending = RwSignal::new(saved(&key.get_value()));
    let selected = StoredValue::new(current.clone());
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let editing = RwSignal::new(None::<(Thread, &'static str)>);
    let new_name = RwSignal::new(String::new());
    let open = move |thread: Thread, action| {
        if thread.active && action == "delete" {
            error.set("Stop unfinished turns before deleting this conversation.".into());
            return;
        }
        new_name.set(thread.name.clone());
        editing.set(Some((thread, action)));
        if let Some(dialog) = dialog.get_untracked() {
            let _ = dialog.show_modal();
        }
    };
    let execute = move |new: Option<Value>| {
        if busy.get_untracked() {
            return;
        }
        let key = key.get_value();
        let raw = if let Some(raw) = pending.get_untracked() {
            if new.is_some() {
                return;
            }
            raw
        } else {
            let Some(new) = new else { return };
            new.to_string()
        };
        let Ok(command) = serde_json::from_str::<Value>(&raw) else {
            error.set("Saved conversation change could not be read.".into());
            return;
        };
        let Some(path) = command["path"].as_str().map(str::to_owned) else {
            return;
        };
        let body = &command["data"];
        let create = path == api.get_value();
        let known = threads.with_untracked(|rows| {
            rows.iter().any(|thread| {
                path == format!("/api/threads/{}/edit", thread.id)
                    || path == format!("/api/threads/{}/delete", thread.id)
            })
        });
        if (!create && !known)
            || !body["id"].as_str().is_some_and(protocol::id)
            || !save(&key, &raw)
        {
            error.set("Check saved conversation change before retrying.".into());
            return;
        }
        busy.set(true);
        pending.set(Some(raw));
        error.set(String::new());
        let body = body.to_string();
        let agent = owner.get_value();
        leptos::task::spawn_local(async move {
            let result = post::<Thread>(&path, &body).await;
            if busy.is_disposed() {
                return;
            }
            busy.set(false);
            match result {
                Ok(thread) => {
                    if remove(&key) {
                        pending.set(None);
                    }
                    if let Some(dialog) = dialog.get_untracked() {
                        dialog.close();
                    }
                    if create || selected.with_value(|id| id.as_ref() == Some(&thread.id)) {
                        let route = if thread.archived != 0 || thread.deleted != 0 {
                            format!("/agents/{agent}")
                        } else {
                            format!("/agents/{agent}/conversations/{}", thread.id)
                        };
                        let _ = window().location().assign(&link(&route));
                    } else {
                        load();
                    }
                }
                Err(reason) => {
                    if reason.definitive && remove(&key) {
                        pending.set(None);
                    }
                    error.set(reason.message);
                }
            }
        });
    };
    let create = move |_| {
        let Some(id) = uuid() else { return };
        execute(Some(
            json!({"path": api.get_value(), "data": {"id": id, "name": "New conversation"}}),
        ));
    };
    let icons = use_context::<Icons>().unwrap_or_default();
    let role = agent
        .role
        .as_deref()
        .filter(|role| !role.is_empty())
        .unwrap_or("Agent")
        .to_owned();
    let model = agent
        .runtime
        .as_deref()
        .filter(|runtime| *runtime != "api")
        .map(|runtime| format!("{runtime} runtime"))
        .or_else(|| agent.model.clone());
    let name = agent.name.clone();
    view! {
        <div class="details__scroll">
        <div class="details__identity">
            {avatar(Some(agent), &name, false, 36)}
            <div class="details__identity-text">
                <div class="details__name" title=name>{name.clone()}</div>
                <div class="details__who-sub" title=role>{role.clone()}</div>
                {model.map(|model| view! { <span class="details__model" title=model>{model.clone()}</span> })}
            </div>
        </div>
        <div class="details__sections">
        <section aria-label="Agent conversations">
            <div class="section-label section-label--row">
                <span>"Conversations"</span>
                <div class="details-list-actions">
                    <details class="chat-pilot-menu">
                        <summary class="k-icon-btn" aria-label="Conversation filters"><span class="appearance-icon" inner_html=icons.filter></span></summary>
                        <label class="k-pop k-check"><input type="checkbox" on:change=move |event| show_archived.set(event_target_checked(&event)) />"Show archived"</label>
                    </details>
                    <button type="button" class="k-icon-btn" aria-label="New conversation" title="New conversation"
                        disabled=move || busy.get() || pending.with(Option::is_some) on:click=create><span class="appearance-icon" inner_html=icons.attach></span></button>
                </div>
            </div>
            <p class="message-error" role="alert" hidden=move || error.with(String::is_empty)>{error}</p>
            <div class="k-row-actions" hidden=move || pending.with(Option::is_none)>
                <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=busy on:click=move |_| execute(None)>"Retry change"</button>
            </div>
            <div class="panel-card">
            <div class="conversation-row" class:is-current=main>
                <a class="panel-row panel-row--link" class:is-current=main aria-current=main.then_some("true") href=link(&base)>
                    <span class="appearance-icon" inner_html=icons.chats></span><span class="panel-row__title">"Main chat"</span>
                </a>
            </div>
            {move || threads.get().into_iter().filter(|thread| thread.archived == 0 || show_archived.get()).map(|thread| {
                let selected = current.as_deref() == Some(thread.id.as_str());
                let label = format!("Menu conversation {}", thread.name);
                let archived = thread.archived != 0;
                let name = thread.name.clone();
                let href = link(&format!("{base}/conversations/{}", thread.id));
                let rename = thread.clone();
                let archive = thread.clone();
                view! {
                    <div class="conversation-row">
                        <a class="panel-row panel-row--link" class:is-current=selected aria-current=selected.then_some("page") href=href>
                            <span class="panel-row__title">{name}</span>
                            {(thread.needs_you != 0).then(|| view! { <span class="k-badge is-warn">"Needs you"</span> })}
                        </a>
                        <details><summary aria-label=label>"Actions"</summary>
                            <div class="k-row-actions">
                                <button class="k-btn k-btn--sm k-btn--ghost" type="button" disabled=move || busy.get() || pending.with(Option::is_some) on:click=move |_| open(rename.clone(), "rename")>"Rename"</button>
                                <button class="k-btn k-btn--sm k-btn--ghost" type="button" disabled=move || busy.get() || pending.with(Option::is_some)
                                    on:click=move |_| open(archive.clone(), if archived { "restore" } else { "archive" })>{if archived { "Restore" } else { "Archive" }}</button>
                                <button class="k-btn k-btn--sm k-btn--ghost" type="button" disabled=move || busy.get() || pending.with(Option::is_some)
                                    on:click=move |_| open(thread.clone(), "delete")>"Delete"</button>
                            </div>
                        </details>
                    </div>
                }
            }).collect_view()}
            </div>
            <dialog node_ref=dialog class="conversation-dialog" aria-label="Conversation change"
                on:cancel=move |event: web_sys::Event| { if busy.get_untracked() { event.prevent_default(); } }>
                <form class="k-stack" on:submit=move |event| {
                    event.prevent_default();
                    let Some((thread, action)) = editing.get_untracked() else { return };
                    let Some(id) = uuid() else { return };
                    let name = new_name.get_untracked();
                    let operation = if action == "delete" { "delete" } else { "edit" };
                    let body = if action == "delete" { json!({"id": id, "revision": thread.revision}) } else {
                        json!({"id": id, "revision": thread.revision, "name": name.trim(), "archived": action == "archive" || action == "rename" && thread.archived != 0})
                    };
                    execute(Some(json!({"path": format!("/api/threads/{}/{operation}", thread.id), "data": body})));
                }>
                    <label class="k-field" hidden=move || editing.with(|entry| entry.as_ref().is_none_or(|(_, action)| *action != "rename"))>
                        <span>"Conversation name"</span><input class="k-input" maxlength="80" required prop:value=new_name
                            on:input=move |event| new_name.set(event_target_value(&event)) />
                    </label>
                    {move || editing.with(|entry| entry.as_ref().map(|(_, action)| match *action {
                        "delete" => "Delete this conversation and its history?",
                        "archive" => "Archive this conversation and stop unfinished work?",
                        "restore" => "Restore this conversation?",
                        _ => "Rename conversation",
                    }))}
                    <p class="message-error" role="alert" hidden=move || error.with(String::is_empty)>{error}</p>
                    <div class="k-row-actions">
                        <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=busy
                            on:click=move |_| { if let Some(dialog) = dialog.get_untracked() { dialog.close(); } }>"Cancel"</button>
                        <button type="submit" class="k-btn k-btn--sm k-btn--primary" disabled=busy>"Confirm"</button>
                    </div>
                </form>
            </dialog>
        </section>
        </div>
        </div>
    }.into_any()
}
