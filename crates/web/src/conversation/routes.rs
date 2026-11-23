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

impl Navigation {
    pub(crate) fn page_path(self) -> String {
        self.0.with(|route| {
            crate::lazy_route::workflow_scope(route).map_or_else(
                || route.split('?').next().unwrap_or_default().to_owned(),
                |(_, back)| back,
            )
        })
    }
}

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
    if crate::lazy_route::workflow_scope(path).is_some() {
        return true;
    }
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let Some(route) = route(path) else {
        return false;
    };
    !matches!(route, Route::Default)
        && web_sys::UrlSearchParams::new_with_str(query).is_ok_and(|params| !params.has("computer"))
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
            let agent = agent.to_ascii_lowercase();
            Some(crate::lazy_view::load("inbox", move || inbox_view(agent.clone())).into_any())
        }
        ["machines", machine, "agents", agent] if protocol::id(machine) && protocol::id(agent) => {
            let machine = machine.to_ascii_lowercase();
            let agent = agent.to_ascii_lowercase();
            Some(
                crate::lazy_view::load("remote agent", move || {
                    remote_view(machine.clone(), agent.clone())
                })
                .into_any(),
            )
        }
        _ => None,
    }
}

#[leptos::wasm_split::wasm_split(inbox, wasm_split_path = leptos::wasm_split, fallible)]
fn inbox_view(agent: String) -> crate::lazy_view::ChunkView {
    Ok(inbox::page(agent).into_any())
}

#[leptos::wasm_split::wasm_split(remote, wasm_split_path = leptos::wasm_split, fallible)]
fn remote_view(machine: String, agent: String) -> crate::lazy_view::ChunkView {
    Ok(remote::page(machine, agent).into_any())
}

fn chat_view(config: ConversationConfig, restore_focus: bool) -> impl IntoView {
    crate::lazy_view::load("conversation", move || {
        render_conversation(config.clone(), restore_focus)
    })
}

#[leptos::wasm_split::wasm_split(conversation, wasm_split_path = leptos::wasm_split, fallible)]
fn render_conversation(
    config: ConversationConfig,
    restore_focus: bool,
) -> crate::lazy_view::ChunkView {
    if restore_focus {
        request_animation_frame(move || history::focus_element(".chat__pill"));
    }
    Ok(shell(config).into_any())
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
    let workflow = Memo::new(move |_| {
        (!deleting())
            .then(|| nav.0.with(|path| crate::lazy_route::workflow_scope(path)))
            .flatten()
    });
    let was_workflow = RwSignal::new(false);
    let restore_conversation_focus = RwSignal::new(false);
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
            if anchor.has_attribute("data-scoped-workflow-back")
                && workflow.with_untracked(Option::is_some)
            {
                if window()
                    .history()
                    .and_then(|history| history.back())
                    .is_ok()
                {
                    return;
                }
            }
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
        error.set(String::new());
        if crate::lazy_route::workflow_scope(&path).is_some() {
            was_workflow.set(true);
            document().set_title("Workflows · Minarbot");
            // Release the hidden chat after the route switch gets a frame.
            request_animation_frame(move || {
                request_animation_frame(move || {
                    if generation.try_get_untracked() == Some(version) {
                        config.try_set(None);
                    }
                });
            });
            return;
        }
        config.set(None);
        leptos::task::spawn_local(async {
            let _ = __wasm_split_preload_render_conversation().await;
        });
        let restore_focus = was_workflow.get_untracked();
        was_workflow.set(false);
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
                    restore_conversation_focus.set(restore_focus);
                    config.set(Some(value));
                }
                Err(reason) => error.set(reason.message),
            }
        });
    });
    view! {
        <div style:display=move || if workflow.with(Option::is_some) {"none"} else {"contents"}
            inert=move || workflow.with(Option::is_some)>
            {move || if deleting() {
                view! { <main class="chat chat--empty bs-root"><p class="empty-chat__sub" role="status">"Deleting agent..."</p></main> }.into_any()
            } else {
                config.get().map(|config| chat_view(config, restore_conversation_focus.get_untracked())).into_any()
            }}
        </div>
        {move || workflow.get().map(|(owner, back)| {
            crate::lazy_route::outlet("workflows", "workflows", nav.0.get_untracked(), owner, Some(back))
        })}
        <p class="conversation-startup" role="status" hidden=move || workflow.with(Option::is_some) || config.with(Option::is_some) || !error.with(String::is_empty)>"Loading conversation…"</p>
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
        let lead = config.room.as_ref().and_then(|room| room.lead.as_deref());
        return view! {
            <div class="right-panel__body">
                <ul class="right-panel__members" aria-label="Section members">
                    {config.members.into_iter().map(|member| {
                        let role = if lead == Some(member.id.as_str()) { "Lead" } else { member.role.as_deref().unwrap_or_default() };
                        let picture = avatar(Some(member.clone()), &member.name, false, 32);
                        view! { <li>{picture}<div>
                            <strong><a class="section-member-link" href=link(&format!("/agents/{}", member.id))>{member.name.clone()}</a></strong>
                            <span>{role.to_owned()}</span>
                        </div></li> }
                    }).collect_view()}
                </ul>
            </div>
            {config.room.filter(|room| room.project.is_some()).map(|room| view! { <div class="details__sections details-activity">{routines::section(room.id)}</div> })}
        }.into_any();
    }
    let Some(agent) = config.agent else {
        return ().into_any();
    };
    if use_context::<EmbeddedStep>().is_some() {
        let name = agent.name.clone();
        let role = agent
            .role
            .clone()
            .filter(|role| !role.is_empty())
            .unwrap_or_else(|| "Agent".into());
        let model = agent.model.clone();
        return view! {
            <div class="details__scroll">
                <div class="details__identity">
                    {avatar(Some(agent), &name, false, 36)}
                    <div class="details__identity-text">
                        <div class="details__name">{name}</div>
                        <div class="details__who-sub">{role}</div>
                        <span class="details__model">{model}</span>
                    </div>
                </div>
            </div>
        }
        .into_any();
    }
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
    let main_menu = NodeRef::<leptos::html::Details>::new();
    let main_menu_trigger = NodeRef::<leptos::html::Summary>::new();
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
                <details class="chat-pilot-menu chat-list-menu" node_ref=main_menu><summary class="k-icon-btn" aria-label="Menu conversation Main chat" title="More actions" node_ref=main_menu_trigger>
                    <span class="appearance-icon" inner_html=more.get_value()></span></summary>
                    <div class="k-pop k-menu">
                        {if agent.builtin {
                            let from = link(&base);
                            view! { <a class="k-menu__item" href=crate::settings::section_url("/settings/providers", &from)>"AI connection"</a> }.into_any()
                        } else {
                            view! { <button class="k-menu__item" type="button" on:click=move |_| {
                                if let Some(menu) = main_menu.get_untracked() { menu.set_open(false); }
                                if let Some(trigger) = main_menu_trigger.get_untracked() { let _ = trigger.focus(); }
                                edit_profile.run(());
                            }>"Edit profile"</button> }.into_any()
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

pub(crate) fn workflow_step(agent: String, thread: String) -> impl IntoView {
    provide_context(EmbeddedStep);
    let config = RwSignal::new(None);
    let error = RwSignal::new(String::new());
    let directory_cache = RwSignal::new(None);
    let thread_cache = RwSignal::new(None);
    let load_step = Callback::new(move |()| {
        let route = Route::Agent(agent.clone(), Some(thread.clone()));
        error.set(String::new());
        leptos::task::spawn_local(async move {
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
        <>
            {move || config.get().map(|config| chat_view(config, false))}
            <Show when=move || config.with(Option::is_none)>
                <p class="empty" role="status" hidden=move || !error.with(String::is_empty)>"Loading step conversation..."</p>
                <p class="error" role="alert" hidden=move || error.with(String::is_empty)>{error} " "
                    <button type="button" class="secondary" on:click=move |_| load_step.run(())>"Reload step conversation"</button>
                </p>
            </Show>
        </>
    }
}
