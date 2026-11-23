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
    #[serde(rename = "providerAccount", alias = "provider_account")]
    pub provider_account: Option<String>,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub builtin: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ForkOrigin {
    pub source_name: String,
    pub source_id: String,
    pub source_kind: String,
    pub source_agent: String,
    pub cutoff_turn: String,
    pub created: String,
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
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub fork: Option<ForkOrigin>,
}

#[derive(Deserialize)]
pub(super) struct Directory {
    pub(super) agents: Vec<Agent>,
    guide: Option<Agent>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub section_name: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    pub members: Vec<String>,
    #[serde(default)]
    pub lead: Option<String>,
    #[serde(default)]
    pub instructions: String,
    pub archived: u8,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub activity: u64,
    #[serde(default)]
    pub read_position: u64,
}

enum Route {
    Default,
    Agent(String, Option<String>),
    Channel(String),
}

fn route(path: &str) -> Option<Route> {
    let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
    match parts.as_slice() {
        [""] => Some(Route::Default),
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
    path.to_owned()
}

fn load_error(message: &str) -> ApiError {
    ApiError {
        message: message.into(),
        definitive: true,
    }
}

type ThreadCache = RwSignal<Option<(f64, Thread)>>;

async fn load(
    route: Route,
    directory_cache: RwSignal<Option<(f64, Vec<Agent>)>>,
    thread_cache: ThreadCache,
) -> Result<ConversationConfig, ApiError> {
    let fresh = directory_cache.get_untracked().filter(|(loaded, agents)| {
        js_sys::Date::now() - loaded <= 10_000.0
            && match &route {
                Route::Agent(id, _) => agents.iter().any(|agent| agent.id == *id),
                Route::Default | Route::Channel(_) => true,
            }
    });
    let agents = if let Some((_, agents)) = fresh {
        agents
    } else {
        let directory: Directory = get("/api/agents").await?;
        let agents = directory
            .agents
            .into_iter()
            .chain(directory.guide)
            .collect::<Vec<_>>();
        directory_cache.try_set(Some((js_sys::Date::now(), agents.clone())));
        agents
    };
    Ok(match route {
        Route::Default => {
            let agent = agents
                .iter()
                .find(|agent| !agent.hidden && !agent.builtin && agent.runnable)
                .cloned()
                .ok_or_else(|| load_error("Set up an agent before opening chat."))?;
            let id = agent.id.clone();
            ConversationConfig {
                base: format!("/api/agents/{id}"),
                scope: id,
                name: agent.name.clone(),
                archived: false,
                can_send: agent.runnable,
                agent: Some(agent),
                thread: None,
                members: Vec::new(),
                channel: false,
                room: None,
            }
        }
        Route::Agent(id, thread_id) => {
            let agent = agents
                .iter()
                .find(|agent| agent.id == id)
                .cloned()
                .ok_or_else(|| load_error("Agent not found."))?;
            let thread = if let Some(id) = thread_id {
                let cached = thread_cache.get_untracked().filter(|(loaded, thread)| {
                    thread.id == id && js_sys::Date::now() - loaded <= 10_000.0
                });
                let thread = if let Some((_, thread)) = cached {
                    thread
                } else {
                    let thread: Thread = get(&format!("/api/threads/{id}")).await?;
                    thread_cache.try_set(Some((js_sys::Date::now(), thread.clone())));
                    thread
                };
                if thread.id != id || thread.agent != agent.id || thread.deleted != 0 {
                    return Err(load_error("Conversation not found."));
                }
                Some(thread)
            } else {
                None
            };
            ConversationConfig {
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
                room: None,
            }
        }
        Route::Channel(id) => {
            let channels: Vec<Channel> = get("/api/channels").await?;
            let channel = channels
                .into_iter()
                .find(|row| row.id == id)
                .ok_or_else(|| load_error("Section not found."))?;
            let project: Option<Value> =
                if let Some(project) = channel.project.as_deref().filter(|id| protocol::id(id)) {
                    get(&format!("/api/projects/{project}")).await.ok()
                } else {
                    None
                };
            let section_name = project
                .as_ref()
                .and_then(|project| project["sections"].as_array())
                .and_then(|sections| sections.iter().find(|section| section["id"] == channel.id))
                .and_then(|section| section["name"].as_str());
            let name = project
                .as_ref()
                .and_then(|project| project["name"].as_str())
                .zip(section_name)
                .map_or_else(
                    || channel.name.clone(),
                    |(project, section)| format!("{project} / {section}"),
                );
            let channel = Channel {
                section_name: section_name.map(str::to_owned).or(channel.section_name),
                ..channel
            };
            let members: Vec<_> = agents
                .iter()
                .filter(|agent| !agent.hidden && channel.members.contains(&agent.id))
                .cloned()
                .collect();
            ConversationConfig {
                base: format!("/api/channels/{id}"),
                scope: format!("channel:{id}"),
                channel: true,
                name,
                archived: channel.archived != 0,
                can_send: !members.is_empty() && members.iter().all(|agent| agent.runnable),
                agent: None,
                thread: None,
                members,
                room: Some(channel),
            }
        }
    })
}

#[derive(Clone, Copy)]
pub(crate) struct Navigation(pub RwSignal<String>);

#[derive(Clone, Copy)]
pub(crate) struct ProjectConversation(pub RwSignal<bool>);

fn location() -> String {
    let url = window().location();
    format!(
        "{}{}",
        url.pathname().unwrap_or_default(),
        url.search().unwrap_or_default()
    )
}

fn conversation_scope(url: &str) -> (&str, Option<String>) {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let focus = web_sys::UrlSearchParams::new_with_str(query)
        .ok()
        .and_then(|params| params.get("focus"));
    (path, focus)
}

fn internal(path: &str) -> bool {
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let Some(route) = route(path) else {
        return false;
    };
    !matches!(route, Route::Default)
        && web_sys::UrlSearchParams::new_with_str(query).is_ok_and(|params| {
            !params.has("computer")
                && !params
                    .get_all("ui")
                    .iter()
                    .any(|value| value.as_string().as_deref() == Some("react"))
        })
}

pub(super) fn open(nav: Option<Navigation>, path: &str) {
    if let Some(nav) = nav.filter(|_| internal(path)) {
        if window()
            .history()
            .and_then(|history| {
                history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(path))
            })
            .is_ok()
        {
            nav.0.set(location());
            return;
        }
    }
    let _ = window().location().assign(path);
}

fn agent_surface(path: &str) -> Option<AnyView> {
    let parts = path.strip_prefix('/')?.split('/').collect::<Vec<_>>();
    match parts.as_slice() {
        ["agents", agent, "inbox"] if protocol::id(agent) => {
            Some(inbox::page(agent.to_ascii_lowercase()).into_any())
        }
        ["machines", machine, "agents", agent] if protocol::id(machine) && protocol::id(agent) => {
            Some(remote::page(machine.to_ascii_lowercase(), agent.to_ascii_lowercase()).into_any())
        }
        _ => None,
    }
}

pub fn standalone() -> impl IntoView {
    if let Some(view) = agent_surface(&window().location().pathname().unwrap_or_default()) {
        return view;
    }
    let config = RwSignal::new(None);
    let directory_cache = RwSignal::new(None);
    let thread_cache = RwSignal::new(None::<(f64, Thread)>);
    provide_context(thread_cache);
    let generation = RwSignal::new(0_u64);
    let lifecycle = use_context::<crate::agent_profile::Lifecycle>();
    let deleting = move || lifecycle.is_some_and(|lifecycle| lifecycle.deleting_current());
    let error = RwSignal::new(String::new());
    let nav = use_context::<Navigation>().unwrap_or_else(|| Navigation(RwSignal::new(location())));
    let project_conversation = use_context::<ProjectConversation>();
    let pop = window_event_listener(leptos::ev::popstate, move |_| {
        let location = location();
        if results::current().is_err()
            || !error.with_untracked(String::is_empty)
            || nav.0.with_untracked(|current| {
                conversation_scope(current) != conversation_scope(&location)
            })
        {
            nav.0.set(location);
        }
    });
    let click = window_event_listener(leptos::ev::click, move |event| {
        if event.default_prevented()
            || event.button() != 0
            || event.ctrl_key()
            || event.meta_key()
            || event.shift_key()
            || event.alt_key()
        {
            return;
        }
        let Some(anchor) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .and_then(|element| element.closest("a[href]").ok().flatten())
            .and_then(|element| element.dyn_into::<web_sys::HtmlAnchorElement>().ok())
        else {
            return;
        };
        if anchor.has_attribute("download")
            || !anchor.target().is_empty()
            || !anchor.hash().is_empty()
            || window().location().origin().ok().as_deref() != Some(anchor.origin().as_str())
            || anchor.has_attribute("data-minarbot-result")
                && window().location().pathname().ok().as_deref()
                    == Some(anchor.pathname().as_str())
        {
            return;
        }
        let path = format!("{}{}", anchor.pathname(), anchor.search());
        if internal(&path) {
            event.prevent_default();
            if path != location() {
                open(Some(nav), &path);
            }
        }
    });
    on_cleanup(move || {
        click.remove();
        pop.remove();
    });
    let revision = RwSignal::new(0_u64);
    Effect::new(move |_| {
        let path = nav.0.get();
        revision.track();
        generation.update(|value| *value += 1);
        let version = generation.get_untracked();
        config.set(None);
        error.set(String::new());
        leptos::task::spawn_local(async move {
            let result = match (
                results::current(),
                route(path.split('?').next().unwrap_or_default()),
            ) {
                (Ok(_), Some(route)) => load(route, directory_cache, thread_cache).await,
                _ => Err(load_error("Conversation not found.")),
            };
            if generation.try_get_untracked() != Some(version) {
                return;
            }
            match result {
                Ok(value) => {
                    if path.split('?').next() == Some("/") {
                        if let Some(agent) = &value.agent {
                            let search = window().location().search().unwrap_or_default();
                            let url = format!("/agents/{}{search}", agent.id);
                            if window()
                                .history()
                                .and_then(|history| {
                                    history.replace_state_with_url(
                                        &wasm_bindgen::JsValue::NULL,
                                        "",
                                        Some(&url),
                                    )
                                })
                                .is_ok()
                            {
                                nav.0.set(url);
                                return;
                            }
                        }
                    }
                    document().set_title(&format!("{} · Minarbot", value.name));
                    if let Some(ProjectConversation(active)) = project_conversation {
                        active.set(
                            value
                                .thread
                                .as_ref()
                                .is_some_and(|thread| thread.project.is_some()),
                        );
                    }
                    config.set(Some(value));
                }
                Err(reason) => error.set(reason.message),
            }
        });
    });
    view! {
        {move || if deleting() { view! { <main class="chat chat--empty bs-root"><p class="empty-chat__sub" role="status">"Deleting agent..."</p></main> }.into_any() } else { config.get().map(shell).into_any() }}
        <p class="conversation-startup" role="status" hidden=move || config.with(Option::is_some) || !error.with(String::is_empty)>"Loading conversation…"</p>
        <div class="conversation-startup" hidden=move || error.with(String::is_empty)>
            <p class="message-error" role="alert">{error}</p>
            <button type="button" class="k-btn k-btn--sm" on:click=move |_| {
                directory_cache.set(None); thread_cache.set(None); revision.update(|n| *n += 1);
            }>"Retry"</button>
            <a class="k-link" href="/">"Back to chats"</a>
        </div>
    }.into_any()
}

pub fn chats(chat: Chat, edit_profile: Callback<()>) -> impl IntoView {
    let config = chat.config.get_value();
    if config.channel {
        return view! {
            <div class="right-panel__body">
                <ul class="right-panel__members" aria-label="Section members">
                    {config.members.into_iter().map(|member| view! {
                        <li><a class="k-link" href=link(&format!("/agents/{}", member.id))>{member.name}</a><span>{member.role}</span></li>
                    }).collect_view()}
                </ul>
            </div>
            {config.room.filter(|room| room.project.is_some()).map(|room| view! { <div class="details__sections details-activity">{routines::section(room.id)}</div> })}
        }.into_any();
    }
    let Some(agent) = config.agent else {
        return ().into_any();
    };
    let upcoming = (!agent.builtin).then(|| agent.id.clone());
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
    let nav = use_context::<Navigation>();
    let thread_cache = use_context::<ThreadCache>();
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let editing = RwSignal::new(None::<(Thread, &'static str)>);
    let new_name = RwSignal::new(String::new());
    let open_action = move |thread: Thread, action| {
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
                    if let Some(cache) = thread_cache {
                        cache.set(None);
                    }
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
                        if create {
                            open(nav, &route);
                        } else {
                            let _ = window().location().assign(&route);
                        }
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
    let create = move || {
        let Some(id) = uuid() else { return };
        execute(Some(
            json!({"path": api.get_value(), "data": {"id": id, "name": "New conversation"}}),
        ));
    };
    Effect::new(move |_| {
        if chat.new_thread.get() {
            chat.new_thread.set(false);
            create();
        }
    });
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
    let more = StoredValue::new(icons.more.clone());
    let chat_icon = StoredValue::new(icons.chats.clone());
    view! {
        <div class="details__scroll">
        <div class="details__identity">
            {avatar(Some(agent.clone()), &name, false, 36)}
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
                        disabled=move || busy.get() || pending.with(Option::is_some) on:click=move |_| create()><span class="appearance-icon" inner_html=icons.attach></span></button>
                </div>
            </div>
            <p class="message-error" role="alert" hidden=move || error.with(String::is_empty)>{error}</p>
            <div class="k-row-actions" hidden=move || pending.with(Option::is_none)>
                <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=busy on:click=move |_| execute(None)>"Retry change"</button>
            </div>
            <div class="panel-card">
            <div class="conversation-row" data-conversation-id="main" class:is-current=main>
                <a class="panel-row panel-row--link" class:is-current=main aria-current=main.then_some("true") href=link(&base)>
                    <span class="appearance-icon" inner_html=icons.chats></span><span class="panel-row__title">"Main chat"</span>
                </a>
                <details class="chat-pilot-menu chat-list-menu"><summary class="k-icon-btn" aria-label="Menu conversation Main chat" title="More actions">
                    <span class="appearance-icon" inner_html=more.get_value()></span></summary>
                    <div class="k-pop k-menu">
                        {if agent.builtin {
                            let from = link(&base);
                            view! { <a class="k-menu__item" href=crate::settings::section_url("/settings/providers", &from)>"AI connection"</a> }.into_any()
                        } else {
                            view! { <button class="k-menu__item" type="button" on:click=move |_| edit_profile.run(())>"Edit profile"</button> }.into_any()
                        }}
                    </div>
                </details>
            </div>
            {move || threads.get().into_iter().filter(|thread| thread.archived == 0 || show_archived.get()).map(|thread| {
                let selected = current.as_deref() == Some(thread.id.as_str());
                let label = format!("Menu conversation {}", thread.name);
                let attention = thread.needs_you;
                let archived = thread.archived != 0;
                let name = thread.name.clone();
                let href = link(&format!("{base}/conversations/{}", thread.id));
                let rename = thread.clone();
                let archive = thread.clone();
                let delete = thread.clone();
                view! {
                    <div class="conversation-row" data-conversation-id=thread.id.clone()>
                        <a class="panel-row panel-row--link" class:is-current=selected aria-current=selected.then_some("true") href=href>
                            <span class="appearance-icon" inner_html=chat_icon.get_value()></span>
                            <span class="panel-row__text"><span class="panel-row__title">{name}</span>
                                {archived.then(|| view! { <span class="panel-row__meta">"Archived"</span> })}
                            </span>
                            {(attention != 0).then(|| view! { <span class="needs-you-badge"><span class="k-badge is-warn" aria-label="Needs you">{attention}</span></span> })}
                        </a>
                        <details class="chat-pilot-menu chat-list-menu"><summary class="k-icon-btn" aria-label=label title="More actions">
                            <span class="appearance-icon" inner_html=more.get_value()></span></summary>
                            <div class="k-pop k-menu">
                                <button class="k-btn k-btn--sm k-btn--ghost" type="button" disabled=move || busy.get() || pending.with(Option::is_some) on:click=move |_| open_action(rename.clone(), "rename")>"Rename"</button>
                                <button class="k-btn k-btn--sm k-btn--ghost" type="button" disabled=move || busy.get() || pending.with(Option::is_some)
                                    on:click=move |_| open_action(archive.clone(), if archived { "restore" } else { "archive" })>{if archived { "Restore" } else { "Archive" }}</button>
                                <button class="k-btn k-btn--sm k-btn--ghost" type="button" disabled=move || busy.get() || pending.with(Option::is_some)
                                    on:click=move |_| open_action(delete.clone(), "delete")>"Delete"</button>
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
        {upcoming.map(|agent| view! { <div class="details__sections details-activity">{routines::upcoming(agent)}</div> })}
    }.into_any()
}

#[cfg(feature = "workflows")]
pub(crate) fn workflow_step(agent: String, thread: String) -> impl IntoView {
    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(catch, js_name = minarbotLoadWorkflowConversation)]
        async fn load_adapter() -> Result<(), wasm_bindgen::JsValue>;
    }
    let config = RwSignal::new(None);
    let error = RwSignal::new(String::new());
    let directory_cache = RwSignal::new(None);
    let thread_cache = RwSignal::new(None);
    let load_step = Callback::new(move |()| {
        let route = Route::Agent(agent.clone(), Some(thread.clone()));
        error.set(String::new());
        leptos::task::spawn_local(async move {
            if load_adapter().await.is_err() {
                error.try_set("Step conversation could not load.".into());
                return;
            }
            match load(route, directory_cache, thread_cache).await {
                Ok(value) => {
                    config.try_set(Some(value));
                }
                Err(reason) => {
                    error.try_set(reason.message);
                }
            }
        });
    });
    load_step.run(());
    view! {
        <div class="workflow-step-chat">
            {move || config.get().map(shell)}
            <Show when=move || config.with(Option::is_none)>
                <p class="empty" role="status" hidden=move || !error.with(String::is_empty)>"Loading step conversation..."</p>
                <p class="error" role="alert" hidden=move || error.with(String::is_empty)>{error} " "
                    <button type="button" class="secondary" on:click=move |_| load_step.run(())>"Reload step conversation"</button>
                </p>
            </Show>
        </div>
    }
}
