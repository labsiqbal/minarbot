use crate::{
    agent_profile::{self, DialogTarget, Lifecycle},
    api, appearance,
    projects::{management, Commands, Project},
    settings,
};
use leptos::prelude::*;
use serde::Deserialize;
use wasm_bindgen::JsCast;

mod attention;
mod switcher;

fn start_conversation() {
    if let Ok(event) = web_sys::Event::new("minarbot:new-conversation") {
        let _ = window().dispatch_event(&event);
    }
}

#[derive(Clone, Deserialize)]
pub(crate) struct Directory {
    pub agents: Vec<Agent>,
    pub guide: Option<Agent>,
    #[serde(default)]
    pub configured: bool,
    #[serde(default, rename = "hubAvailable")]
    pub hub_available: bool,
    #[serde(default, rename = "defaultModel")]
    pub default_model: String,
}

#[derive(Clone, Deserialize)]
pub(crate) struct Agent {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub runnable: bool,
    #[serde(default)]
    pub provider_available: Option<bool>,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub avatar_revision: u64,
    #[serde(default)]
    pub visibility_revision: u64,
    #[serde(default)]
    pub builtin: bool,
    #[serde(flatten)]
    pub profile: agent_profile::Profile,
}

#[derive(Clone, Deserialize)]
struct Projects {
    projects: Vec<Project>,
    #[serde(default)]
    root: String,
}

#[derive(Clone, Deserialize)]
struct Summary {
    agent: String,
    #[serde(default)]
    needs_you: u32,
    #[serde(default)]
    preview: String,
    #[serde(default)]
    time: Option<String>,
    #[serde(default)]
    status: String,
}

#[derive(Deserialize)]
struct Summaries {
    summaries: Vec<Summary>,
}

#[derive(Clone, Deserialize, PartialEq)]
struct Machines {
    machines: Vec<Machine>,
}

#[derive(Clone, Deserialize, PartialEq)]
struct Machine {
    id: String,
    name: String,
    status: String,
    #[serde(default, rename = "agentCount")]
    agent_count: Option<usize>,
    agents: Vec<MachineAgent>,
}

#[derive(Clone, Deserialize, PartialEq)]
struct MachineAgent {
    id: String,
    name: String,
    #[serde(default)]
    role: String,
}

pub(crate) fn machine_status(status: &str) -> &'static str {
    match status {
        "online" => "Online",
        "stale" => "Stale",
        "offline" => "Offline",
        "unauthorized" => "Unauthorized",
        "version-mismatch" => "Version mismatch",
        _ => "Unavailable",
    }
}

#[derive(Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Icons {
    panel: String,
    filter: String,
    settings: String,
    routines: String,
    workflows: String,
    calendar: String,
    search: String,
    book: String,
    quick_search: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    close: String,
    #[serde(default)]
    settings_choices: Vec<switcher::Setting>,
    pub info: String,
    pub voice: String,
    pub monitor: String,
    pub plug: String,
    pub plus: String,
    pub send: String,
    pub check: String,
    pub folder: String,
    pub folder_open: String,
    pub hash: String,
    pub chevron: String,
}

#[cfg(feature = "conversation")]
#[derive(Clone, Copy)]
pub(crate) struct DirectoryRefresh(pub Callback<()>);

const COLLAPSED: &str = "minarbot:sidebar-collapsed";
const TAB: &str = "minarbot:sidebar-tab";

#[derive(Clone, Copy)]
pub(crate) struct AutomationNavigation(pub RwSignal<String>);

impl AutomationNavigation {
    fn open(self, path: &str) {
        let location = window().location();
        if self.0.with_untracked(|current| current == path)
            && location.search().is_ok_and(|query| query.is_empty())
        {
            return;
        }
        let pushed = window().history().and_then(|history| {
            history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(path))
        });
        if pushed.is_ok() {
            self.0.set(path.to_owned());
        } else {
            let _ = location.assign(path);
        }
    }
}

fn preference(key: &str) -> Option<String> {
    window()
        .local_storage()
        .ok()
        .flatten()?
        .get_item(key)
        .ok()?
}

fn save_preference(key: &str, value: &str) {
    if let Ok(Some(storage)) = window().local_storage() {
        let _ = storage.set_item(key, value);
    }
}

fn plain_preview(text: &str) -> String {
    // ponytail: strip inline markers; render the full Markdown only in the transcript.
    text.replace(['*', '_', '~', '`'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn relative_time(value: &str) -> String {
    let age = js_sys::Date::now() - js_sys::Date::parse(value);
    if !age.is_finite() {
        return String::new();
    }
    let minutes = (age.max(0.0) / 60_000.0).floor() as u64;
    if minutes < 60 {
        format!("{}m", minutes.max(1))
    } else if minutes < 1_440 {
        format!("{}h", minutes / 60)
    } else {
        format!("{}d", minutes / 1_440)
    }
}

#[component]
pub(crate) fn WorkspaceShell(
    base_path: String,
    colors: StoredValue<Vec<appearance::ColorTheme>>,
    icons: Icons,
    #[prop(optional)] conversation_navigation: bool,
    #[prop(optional)] header: Option<Children>,
    children: Children,
) -> impl IntoView {
    appearance::initialize(colors);
    let directory = RwSignal::new(None::<Result<Directory, String>>);
    provide_context(directory.read_only());
    let lifecycle = Lifecycle::new(directory);
    provide_context(lifecycle);
    let (confirm_agent, agent_confirmation) =
        crate::system::kit::named_confirmation("agent-lifecycle-confirm");
    provide_context(confirm_agent);
    let projects = RwSignal::new(None::<Result<Projects, String>>);
    let summaries = RwSignal::new(Vec::<Summary>::new());
    let machines = RwSignal::new(None::<Result<Machines, String>>);
    let route_tab = if base_path.starts_with("/projects/") || base_path.starts_with("/rooms/") {
        Some("projects")
    } else if base_path.starts_with("/agents/") || base_path.starts_with("/machines/") {
        Some("agents")
    } else {
        None
    };
    if let Some(tab) = route_tab {
        save_preference(TAB, tab);
    }
    let projects_tab = RwSignal::new(route_tab.map_or_else(
        || preference(TAB).as_deref() == Some("projects"),
        |tab| tab == "projects",
    ));
    let collapsed = RwSignal::new(preference(COLLAPSED).as_deref() == Some("1"));
    let narrow = || {
        window()
            .match_media("(max-width: 767px)")
            .ok()
            .flatten()
            .is_some_and(|query| query.matches())
    };
    let narrow_open = RwSignal::new(false);
    let sidebar = NodeRef::<leptos::html::Aside>::new();
    let reveal = NodeRef::<leptos::html::Button>::new();
    let close_sidebar = move || {
        narrow_open.set(false);
        request_animation_frame(move || {
            if let Some(button) = reveal.get_untracked() {
                let _ = button.focus();
            }
        });
    };
    let is_collapsed = move || {
        if narrow() {
            !narrow_open.get()
        } else {
            collapsed.get()
        }
    };
    let toggle_sidebar = move || {
        if narrow() {
            if narrow_open.get_untracked() {
                close_sidebar();
            } else {
                narrow_open.set(true);
                request_animation_frame(move || {
                    if let Some(sidebar) = sidebar.get_untracked() {
                        let _ = sidebar.focus();
                    }
                });
            }
        } else {
            collapsed.update(|value| *value = !*value);
            save_preference(COLLAPSED, if collapsed.get_untracked() { "1" } else { "0" });
        }
    };
    let choose_tab = move |projects| {
        projects_tab.set(projects);
        save_preference(TAB, if projects { "projects" } else { "agents" });
    };
    let refresh_directory = move || {
        leptos::task::spawn_local(async move {
            directory.try_set(Some(api::read("/api/agents").await));
        });
    };
    #[cfg(feature = "conversation")]
    provide_context(DirectoryRefresh(Callback::new(move |()| {
        refresh_directory()
    })));
    let refresh_projects = crate::connection_ui::UiAction::new(move |()| {
        leptos::task::spawn_local(async move {
            projects.try_set(Some(api::read("/api/projects").await));
        });
    });
    let project_commands = Commands::new(refresh_projects);
    provide_context(project_commands);
    let new_project = move || project_commands.create.set(true);
    let refresh_machines = move || {
        leptos::task::spawn_local(async move {
            let next = Some(api::read("/api/machines").await);
            machines.try_maybe_update(|current| {
                if *current == next {
                    return (false, ());
                }
                *current = next;
                (true, ())
            });
        });
    };
    let retry = move || {
        directory.set(None);
        projects.set(None);
        refresh_directory();
        refresh_projects.run(());
        leptos::task::spawn_local(async move {
            if let Ok(data) = api::read::<Summaries>("/api/sidebar/summary").await {
                summaries.try_set(data.summaries);
            }
        });
        refresh_machines();
    };
    retry();
    if let Ok(timer) = set_interval_with_handle(
        move || {
            if document().visibility_state() == web_sys::VisibilityState::Visible {
                refresh_machines();
            }
        },
        std::time::Duration::from_secs(5),
    ) {
        on_cleanup(move || timer.clear());
    }
    let agent_search = RwSignal::new(String::new());
    let project_search = RwSignal::new(String::new());
    let search = move || {
        if projects_tab.get() {
            project_search
        } else {
            agent_search
        }
    };
    let online = RwSignal::new(false);
    let attention = RwSignal::new(false);
    let viewport = RwSignal::new(narrow());
    let resize = window_event_listener(leptos::ev::resize, move |_| viewport.set(narrow()));
    on_cleanup(move || resize.remove());
    let switcher = RwSignal::new(false);
    let switcher_requested = RwSignal::new(false);
    let open_switcher = move || {
        switcher.set(true);
        switcher_requested.set(true);
    };
    let help = NodeRef::<leptos::html::Dialog>::new();
    let pending = attention::subscribe();
    let keys = window_event_listener(leptos::ev::keydown, move |event| {
        let key = event.key();
        if key == "Escape"
            && narrow_open.get_untracked()
            && document()
                .query_selector("dialog:modal")
                .ok()
                .flatten()
                .is_none()
        {
            close_sidebar();
            event.prevent_default();
            return;
        }
        if !(event.ctrl_key() || event.meta_key())
            || document()
                .query_selector("dialog:modal")
                .ok()
                .flatten()
                .is_some()
        {
            return;
        }
        match key.as_str() {
            "b" => toggle_sidebar(),
            "1" => choose_tab(false),
            "2" => choose_tab(true),
            "k" => open_switcher(),
            "," if !event.shift_key() => {
                if let Ok(Some(link)) = document().query_selector(".sidebar-settings") {
                    if let Ok(link) = link.dyn_into::<web_sys::HtmlElement>() {
                        link.click();
                    }
                }
            }
            _ => return,
        }
        event.prevent_default();
    });
    on_cleanup(move || keys.remove());
    let base_path = StoredValue::new(base_path);
    let automation = use_context::<AutomationNavigation>();
    #[cfg(feature = "conversation")]
    let conversation = use_context::<crate::conversation::Navigation>();
    #[cfg(feature = "conversation")]
    let project_conversation = use_context::<crate::conversation::ProjectConversation>();
    #[cfg(feature = "conversation")]
    if let Some(navigation) = conversation {
        Effect::new(move |previous: Option<String>| {
            let path = navigation.0.get();
            if previous.as_ref().is_some_and(|previous| previous != &path) {
                narrow_open.set(false);
                if path.starts_with("/rooms/") {
                    projects_tab.set(true);
                    save_preference(TAB, "projects");
                } else if path.starts_with("/agents/") || path.starts_with("/machines/") {
                    projects_tab.set(false);
                    save_preference(TAB, "agents");
                }
            }
            path
        });
    }
    let route_path = move || {
        #[cfg(feature = "conversation")]
        if let Some(navigation) = conversation {
            return navigation.page_path();
        }
        automation
            .map(|nav| nav.0.get())
            .unwrap_or_else(|| base_path.get_value())
    };
    let current_url = move || {
        let mut path = route_path();
        let location = window().location();
        if location.pathname().ok().as_deref() == Some(path.as_str()) {
            path.push_str(&location.search().unwrap_or_default());
        }
        path
    };
    let settings_link = move |section| settings::section_url(section, &current_url());
    let automation_link = move |path: &'static str, label: &'static str, icon: String| {
        view! {
            <button type="button" class="k-icon-btn" aria-label=label title=label aria-current=move || (route_path() == path).then_some("page")
                on:click=move |_| {
                    narrow_open.set(false);
                    if let Some(automation) = automation { automation.open(path); }
                    else { let _ = window().location().assign(path); }
                }><span class="appearance-icon" inner_html=icon></span></button>
        }
    };
    provide_context(icons.clone());
    let settings_focus = NodeRef::<leptos::html::A>::new();
    let location = window().location();
    let return_path = format!(
        "{}{}",
        location.pathname().unwrap_or_default(),
        location.search().unwrap_or_default()
    );
    if window()
        .session_storage()
        .ok()
        .flatten()
        .is_some_and(|storage| {
            let key = settings::RETURN_FOCUS_KEY;
            storage.get_item(key).ok().flatten().as_deref() == Some(&return_path)
                && storage.remove_item(key).is_ok()
        })
    {
        request_animation_frame(move || {
            if is_collapsed() {
                if let Some(button) = reveal.get_untracked() {
                    let _ = button.focus();
                }
            } else if let Some(link) = settings_focus.get_untracked() {
                let _ = link.focus();
            }
        });
    }
    let panel_icon = icons.panel.clone();
    let hidden = move || {
        let _ = viewport.get();
        is_collapsed()
    };
    let agent_rows = move || {
        let Some(result) = directory.get() else {
            return view! { <p class="machine-group__note" role="status">"Loading agents…"</p> }
                .into_any();
        };
        let Ok(directory) = result else {
            return view! { <p class="machine-group__note" role="alert">"Agents could not be loaded."</p><button class="k-btn k-btn--sm" on:click=move |_| retry()>"Retry"</button> }.into_any();
        };
        let query = agent_search.get().trim().to_lowercase();
        let summaries = summaries.get();
        let has_local_agents = !directory.agents.is_empty();
        let pending = pending.get();
        let items = directory.agents.into_iter().chain(directory.guide).filter_map(|agent| {
            let path = format!("/agents/{}", agent.id);
            let current = {
                let base = route_path();
                let local = base == path || base.strip_prefix(&path).is_some_and(|suffix| suffix.starts_with('/'));
                #[cfg(feature = "conversation")]
                let local = local && !project_conversation.is_some_and(|active| active.0.get());
                local
            };
            let summary = summaries.iter().find(|row| row.agent == agent.id);
            let mut detail = summary.filter(|row| !row.preview.is_empty()).map(|row| plain_preview(&row.preview)).unwrap_or_else(|| agent.role.clone());
            if agent.hidden && !current || !format!("{} {}", agent.name, detail).to_lowercase().contains(&query) {
                return None;
            }
            let mut destinations = std::collections::HashSet::new();
            let inputs = pending.iter().flatten().filter(|input| input.agent == agent.id).collect::<Vec<_>>();
            let first = inputs.first().map(|input| (*input).clone());
            let active = pending.as_ref().map_or_else(|| summary.map_or(0, |row| row.needs_you as usize),
                |_| inputs.iter().filter(|input| destinations.insert(input.conversation())).count());
            if attention.get() && active == 0 { return None; }
            if agent.hidden { detail = "Hidden".into(); }
            let working = summary.is_some_and(|row| row.status == "running");
            let status = if active > 0 { "Needs you" } else if working { "Working" } else { "Idle" };
            let dot = if active > 0 || agent.provider_available == Some(false) { "is-attention" } else if working { "is-working" } else { "is-idle" };
            let time = summary.filter(|_| active == 0).and_then(|row| row.time.as_deref());
            let avatar = (agent.avatar_revision > 0).then(|| format!("/api/agents/{}/avatar/image?v={}", agent.id, agent.avatar_revision));
            let initial = agent.name.chars().next().map(|initial| initial.to_uppercase().to_string());
            let actions = agent_profile::row_actions(lifecycle, agent.clone());
            let href = first.as_ref().map_or_else(|| path, attention::Notice::href);
            let badge = if active == 1 { "Needs you".into() } else { format!("Needs you in {active} conversations") };
            Some(view! {
                <div class="sidebar-agent" on:contextmenu=move |event| { event.prevent_default(); open_agent_actions(event.current_target()); }
                    on:keydown=move |event| {
                        if event.key() == "ContextMenu" || event.shift_key() && event.key() == "F10" {
                            event.prevent_default(); open_agent_actions(event.current_target());
                        }
                    }>
                <a class="bot-row" class:is-selected=current href=href aria-current=current.then_some("page") on:click=move |event| {
                    if let Some(input) = &first {
                        if event.button() == 0 && !event.ctrl_key() && !event.meta_key() && !event.shift_key() && !event.alt_key() {
                            event.prevent_default();
                            input.open();
                        }
                    }
                }>
                    <span class="bot-row__avatar" aria-hidden="true"><span class="avatar bs-avatar">
                        {if let Some(src) = avatar { view! { <img src=src alt="" /> }.into_any() } else { initial.into_any() }}
                    </span><span class=format!("status-dot {dot}")></span></span>
                    <span class="sr-only">{status}{(agent.provider_available == Some(false)).then_some(" - Provider account unavailable")}</span>
                    <span class="bot-row__text"><span class="bot-row__name">{agent.name}</span><span class="bot-row__preview">{detail}</span></span>
                    {time.map(|time| view! { <time class="bot-row__time" datetime=time.to_owned()>{relative_time(time)}</time> })}
                    {(active > 0).then(|| view! { <span class="needs-you-badge"><span class="k-badge is-warn" title=badge.clone()><span aria-hidden="true">{active}</span><span class="sr-only">{badge.clone()}</span></span></span> })}
                </a>{actions}
                </div>
            })
        }).collect_view();
        if items.is_empty() && has_local_agents {
            view! { <p class="machine-group__note" role="status">{if attention.get() && query.is_empty() { "No agents need you." } else if query.is_empty() && !online.get() { "No agents yet." } else { "No matching agents." }}</p> }.into_any()
        } else {
            items.into_any()
        }
    };
    let hidden_agents =
        agent_profile::hidden_agents(lifecycle, directory.read_only(), agent_search);
    let chevron_icon = StoredValue::new(icons.chevron.clone());
    let remote_rows = move || {
        let query = agent_search.get().trim().to_lowercase();
        let online = online.get();
        let hidden = attention.get();
        machines.with(|result| match result {
            Some(Ok(result)) if !hidden => result.machines.iter()
                .filter(|machine| !online || machine.status == "online")
                .filter_map(|machine| {
                    let status = machine_status(&machine.status);
                    let online = machine.status == "online";
                    let count = machine.agent_count.unwrap_or(machine.agents.len());
                    let links = machine.agents.iter().filter_map(|agent| {
                        if !format!("{} {}", agent.name, agent.role).to_lowercase().contains(&query) { return None; }
                        let path = format!("/machines/{}/agents/{}", machine.id, agent.id);
                        let current = route_path() == path;
                        let initial = agent.name.chars().next().map(|initial| initial.to_uppercase().to_string());
                        Some(view! {
                            <a class="bot-row" class:is-selected=current href=path aria-current=current.then_some("page")
                                aria-label=format!("{} on {}", agent.name, machine.name)>
                                <span class="bot-row__avatar">
                                    <span class="avatar bs-avatar" aria-hidden="true">{initial}</span>
                                    <span class=if online { "status-dot is-online" } else { "status-dot is-offline" } role="img" aria-label=status></span>
                                </span>
                                <span class="bot-row__text"><span class="bot-row__name">{agent.name.clone()}</span><span class="bot-row__preview">{agent.role.clone()}</span></span>
                            </a>
                        })
                    }).collect_view();
                    if links.is_empty() && !query.is_empty() { return None; }
                    Some(view! {
                        <details class="machine-group" open>
                            <summary class="machine-group__header" title=format!("{} · {status}", machine.name)>
                                <span class="machine-group__chevron" inner_html=chevron_icon.get_value()></span>
                                <span class="sidebar-machine-icon"><crate::connection_ui::Icon name="server" /></span>
                                <span class="machine-group__name">{machine.name.clone()}</span>
                                <span class="machine-group__count" aria-label=format!("{count} agents")>{count}</span>
                                <span class=format!("machine-dot is-{}", machine.status) role="img" aria-label=status title=status></span>
                            </summary>
                            <div class="machine-group__content" role="group" aria-label=format!("{} contents", machine.name)>
                                {(!online).then(|| view! { <p class="machine-group__note">{format!("{status} · Last known agents")}</p> })}
                                <nav class="bot-list" aria-label=format!("Agents on {}", machine.name)>
                                    {if links.is_empty() { view! { <p class="machine-group__note">"No agents cached yet."</p> }.into_any() } else { links.into_any() }}
                                </nav>
                            </div>
                        </details>
                    })
                }).collect_view().into_any(),
            Some(Err(_)) if !hidden => view! { <p class="machine-group__note" role="status">"Machines could not be refreshed."</p> }.into_any(),
            _ => ().into_any(),
        })
    };
    let folder_icon = StoredValue::new(icons.folder.clone());
    let open_icon = StoredValue::new(icons.folder_open.clone());
    let section_icon = StoredValue::new(icons.hash.clone());
    let project_rows = move || {
        let query = project_search.get().trim().to_lowercase();
        projects.with(|projects| match projects {
            None => view! { <p class="machine-group__note" role="status">"Loading projects…"</p> }.into_any(),
            Some(Err(_)) => view! { <p class="machine-group__note" role="alert">"Projects could not be loaded."</p><button class="k-btn k-btn--sm" on:click=move |_| retry()>"Retry"</button> }.into_any(),
            Some(Ok(projects)) => {
                let rows = projects.projects.iter().filter(|project| !project.archived).filter_map(|project| {
                    let name_matches = format!("{} {}", project.name, project.description).to_lowercase().contains(&query);
                    let matching = |section: &&crate::projects::Section| !section.archived && (name_matches || section.name.to_lowercase().contains(&query));
                    let has_sections = project.sections.iter().any(|section| matching(&section));
                    let sections = project.sections.iter().filter(matching).map(|section| {
                        let path = format!("/rooms/{}", section.id);
                        let current = route_path() == path;
                        let unread = section.activity > section.read_position;
                        view! { <div class="project-navigation__section" class:is-selected=current>
                            <a class="project-navigation__room" class:is-selected=current href=path aria-current=current.then_some("page")
                                aria-label=format!("# {} in {}{}", section.name, project.name, if unread { " - Unread" } else { "" })>
                                <span class="appearance-icon" inner_html=section_icon.get_value()></span><span>{section.name.clone()}</span>
                                {unread.then(|| view! { <span class="project-navigation__unread" role="img" aria-label="Unread"></span> })}
                            </a>
                            {management::section_menu(project_commands, project.clone(), section.clone())}
                        </div> }
                    }).collect_view();
                    if !name_matches && !has_sections { return None; }
                    let path = format!("/projects/{}", project.id);
                    let current = route_path() == path;
                    let key = StoredValue::new(format!("minarbot:sidebar-project:{}", project.id));
                    let expanded = RwSignal::new(key.with_value(|key| preference(key).as_deref() != Some("closed")));
                    let searching = !query.is_empty();
                    let open = move || searching || expanded.get();
                    let change = move |show| {
                        expanded.set(show);
                        key.with_value(|key| save_preference(key, if show { "open" } else { "closed" }));
                    };
                    let name = project.name.clone();
                    Some(view! {
                        <div class="project-navigation__project">
                            <div class="project-navigation__header" class:is-selected=current>
                                <button class="k-icon-btn" type="button" aria-expanded=move || if open() { "true" } else { "false" }
                                    aria-label=move || format!("{} project {name}", if open() { "Collapse" } else { "Expand" })
                                    on:click=move |_| change(!expanded.get_untracked())>
                                    <span class="project-navigation__chevron" class:is-open=open inner_html=chevron_icon.get_value()></span>
                                </button>
                                <a class="project-navigation__name" href=path aria-current=current.then_some("page")
                                    on:keydown=move |event: web_sys::KeyboardEvent| match event.key().as_str() {
                                        "ArrowRight" => { event.prevent_default(); change(true); }
                                        "ArrowLeft" => { event.prevent_default(); change(false); }
                                        _ => {}
                                    }>
                                    <span class="appearance-icon" inner_html=move || if open() { open_icon.get_value() } else { folder_icon.get_value() }></span>
                                    <span>{project.name.clone()}</span>
                                </a>
                                {management::project_menu(project_commands, project.clone())}
                            </div>
                            <div class="project-navigation__sections" hidden=move || !open()>
                                {(!has_sections).then(|| view! { <p class="machine-group__note">"No active sections."</p> })}{sections}
                            </div>
                        </div>
                    })
                }).collect_view();
                if rows.is_empty() {
                    view! { <p class="machine-group__note" role="status">{if query.is_empty() { "No projects yet" } else { "No matching projects or sections." }}</p> }.into_any()
                } else {
                    rows.into_any()
                }
            }
        })
    };
    let local_agent_count = move || {
        directory.with(|value| {
            value
                .as_ref()
                .and_then(|value| value.as_ref().ok())
                .map_or(0, |directory| {
                    directory
                        .agents
                        .iter()
                        .chain(directory.guide.iter())
                        .filter(|agent| !agent.hidden)
                        .count()
                })
        })
    };
    let agent_count = move || {
        local_agent_count()
            + machines.with(|value| {
                value
                    .as_ref()
                    .and_then(|value| value.as_ref().ok())
                    .map_or(0, |machines| {
                        machines
                            .machines
                            .iter()
                            .map(|machine| machine.agents.len())
                            .sum::<usize>()
                    })
            })
    };
    let project_count = move || {
        projects.with(|value| {
            value
                .as_ref()
                .and_then(|value| value.as_ref().ok())
                .map_or(0, |items| {
                    items.projects.iter().filter(|item| !item.archived).count()
                })
        })
    };
    let provider_ready = move || {
        directory.with(|value| {
            value
                .as_ref()
                .and_then(|value| value.as_ref().ok())
                .is_some_and(|directory| {
                    directory.configured
                        || directory.guide.as_ref().is_some_and(|agent| agent.runnable)
                })
        })
    };
    let provider_status = move || {
        if provider_ready() {
            "Provider configured"
        } else {
            "Provider not configured"
        }
    };
    let monitor = icons.monitor.clone();
    let create_menu = NodeRef::<leptos::html::Details>::new();
    let create_trigger = NodeRef::<leptos::html::Summary>::new();
    let open_lifecycle = move |target| {
        if let Some(menu) = create_menu.get_untracked() {
            menu.set_open(false);
        }
        if let Some(trigger) = create_trigger.get_untracked() {
            let _ = trigger.focus();
        }
        lifecycle.open(target);
    };
    let directory_ready = move || directory.with(|data| data.as_ref().is_some_and(Result::is_ok));
    let conversation_creation = Signal::derive(move || {
        conversation_navigation && route_path().strip_prefix("/agents/").and_then(|path| path.split('/').next())
            .is_some_and(|id| directory.with(|result| matches!(result, Some(Ok(data)) if data.agents.iter().any(|agent| agent.id == id && !agent.hidden))))
    });
    let plus = icons.plus.clone();
    let tabs = [("Agents", "agents", false), ("Projects", "projects", true)]
        .into_iter()
        .map(|(label, id, project)| {
            let selected = move || projects_tab.get() == project;
            view! {
                <button type="button" role="tab" class="k-tabs__tab"
                    id=format!("workspace-tab-{id}") aria-controls=format!("workspace-panel-{id}")
                    tabindex=move || if selected() { 0 } else { -1 }
                    aria-selected=move || if selected() { "true" } else { "false" }
                    data-state=move || if selected() { "active" } else { "inactive" }
                    on:click=move |_| choose_tab(project)>{label}
                    <span class="k-tabs__count">{move || if project { project_count() } else { agent_count() }}</span>
                </button>
            }
        })
        .collect_view();
    let switcher_icons = icons.clone();
    let switcher_data = switcher::Data {
        directory,
        machines,
        projects,
        base_path: Signal::derive(current_url),
        new_conversation: conversation_creation,
        new_project: project_commands.create,
        online,
        attention,
    };
    let (update_capsule, update_dialog) = crate::system::updates::workspace();
    view! {
        <div class="app bs-root leptos-workspace" class:is-sidebar-collapsed=hidden>
            <button type="button" class="sidebar-reveal k-icon-btn icon-btn--bordered" hidden=move || !hidden()
                node_ref=reveal aria-label="Expand sidebar" aria-keyshortcuts="Control+b Meta+b" on:click=move |_| toggle_sidebar()>
                <span class="appearance-icon" inner_html=panel_icon></span>
            </button>
            <button type="button" class="sidebar-backdrop" hidden=move || !viewport.get() || !narrow_open.get()
                tabindex="-1" aria-label="Close workspace navigation" on:click=move |_| close_sidebar()></button>
            <aside class="sidebar bs-root" node_ref=sidebar tabindex="-1" aria-label="Workspace navigation" hidden=hidden>
                <div class="sidebar__titlebar">
                    <span class="sidebar__bots-label">"Minarbot"</span>
                    <nav class="sidebar__controls" aria-label="Automation">
                        {automation_link("/routines", "Routines", icons.routines)}
                        {automation_link("/workflows", "Workflows", icons.workflows)}
                        {automation_link("/calendar", "Automation calendar", icons.calendar)}
                        <details class="sidebar-create" node_ref=create_menu>
                            <summary class="k-icon-btn" node_ref=create_trigger aria-label="Create new" title="Create new"><span class="appearance-icon" inner_html=plus></span></summary>
                            <div class="k-pop k-menu">
                                <button type="button" class="k-menu__item" hidden=move || !conversation_creation.get() on:click=move |_| {
                                    if let Some(menu) = create_menu.get_untracked() { menu.set_open(false); }
                                    start_conversation();
                                }>"New conversation"</button>
                                <button type="button" class="k-menu__item" disabled=move || !directory_ready()
                                    on:click=move |_| open_lifecycle(DialogTarget::Create)><crate::connection_ui::Icon name="plus" />"New agent"</button>
                                <button type="button" class="k-menu__item" disabled=move || !directory_ready()
                                    on:click=move |_| open_lifecycle(DialogTarget::Import)><crate::connection_ui::Icon name="download" />"Import template"</button>
                                <button type="button" class="k-menu__item" on:click=move |_| {
                                    if let Some(menu) = create_menu.get_untracked() { menu.set_open(false); }
                                    if let Some(trigger) = create_trigger.get_untracked() { let _ = trigger.focus(); }
                                    new_project();
                                }><crate::connection_ui::Icon name="folder" />"New project"</button>
                                <a class="k-menu__item" href=move || settings_link("/settings/machines")>"Add a machine"</a>
                            </div>
                        </details>
                        <button type="button" class="k-icon-btn" aria-label="Collapse sidebar" on:click=move |_| toggle_sidebar()><span class="appearance-icon" inner_html=icons.panel></span></button>
                    </nav>
                </div>
                <div class="sidebar__tabs"><div class="k-tabs">
                    <div class="k-tabs__list" role="tablist" aria-label="Sidebar"
                        on:keydown=move |event| settings::move_focus(&event, "[role=tablist]", true)>{tabs}</div>
                    <div class="sidebar__search">
                        <span class="appearance-icon" inner_html=icons.search></span>
                        <input class="k-input" type="search"
                            aria-label=move || if projects_tab.get() { "Search projects" } else { "Search agents" }
                            placeholder=move || if projects_tab.get() { "Search projects" } else { "Search agents" }
                            prop:value=move || search().get()
                            on:input=move |event| search().set(event_target_value(&event)) />
                        <button type="button" class="k-icon-btn" aria-label="Filter sidebar" popovertarget="workspace-filters"
                            aria-pressed=move || if online.get() || attention.get() { "true" } else { "false" }
                            hidden=move || projects_tab.get()><span class="appearance-icon" inner_html=icons.filter></span></button>
                        <div id="workspace-filters" popover="auto" class="k-pop k-menu">
                            <label class="k-menu__item"><input type="checkbox" prop:checked=online on:change=move |event| online.set(event_target_checked(&event)) />"Only online"</label>
                            <label class="k-menu__item"><input type="checkbox" prop:checked=attention on:change=move |event| attention.set(event_target_checked(&event)) />"Needs you"</label>
                        </div>
                    </div>
                    <div class="sidebar__scroll">
                        <div id="workspace-panel-agents" role="tabpanel" aria-labelledby="workspace-tab-agents" hidden=projects_tab>
                            <nav class="machine-groups" aria-label="Machines">
                                <details class="machine-group" open>
                                    <summary class="machine-group__header">
                                        <span class="machine-group__chevron" inner_html=chevron_icon.get_value()></span>
                                        <span class="appearance-icon" inner_html=monitor.clone()></span><span class="machine-group__name">"This device"</span>
                                        <span class="machine-group__count" aria-label=move || format!("{} agents", local_agent_count())>{local_agent_count}</span>
                                    </summary>
                                    <div class="machine-group__content"><nav class="bot-list" aria-label="Choose agent">{agent_rows}
                                        <Show when=move || agent_search.with(String::is_empty) && !attention.get() && directory.with(|data| matches!(data, Some(Ok(data)) if data.agents.is_empty()))>
                                            <button type="button" class="bot-list__empty" on:click=move |_| lifecycle.open(DialogTarget::Create)>
                                                <span class="bot-list__empty-plus"><crate::connection_ui::Icon name="plus" /></span>
                                                <span><span class="bot-row__name">"New agent"</span></span>
                                            </button>
                                        </Show>
                                    </nav></div>
                                </details>
                                {remote_rows}
                                {crate::system::kit::error(lifecycle.error)}
                                <a class="bot-list__empty" href=move || settings_link("/settings/machines") hidden=move || attention.get() || machines.with(|value|
                                    !matches!(value, Some(Ok(machines)) if machines.machines.is_empty()))>
                                    <span class="appearance-icon" inner_html=monitor.clone()></span><span><span class="bot-row__name">"Pair a machine"</span></span>
                                </a>
                                {hidden_agents}
                            </nav>
                        </div>
                        <div id="workspace-panel-projects" role="tabpanel" aria-labelledby="workspace-tab-projects" hidden=move || !projects_tab.get()>
                            <nav class="project-navigation" aria-label="Projects">
                                {project_rows}
                                <button type="button" class="project-navigation__new" on:click=move |_| new_project()>
                                    <crate::connection_ui::Icon name="plus" /><span>"New project"</span>
                                </button>
                                <Show when=move || projects_tab.get()>{move || management::archived_projects(project_commands)}</Show>
                                {project_commands.recovery()}
                            </nav>
                        </div>
                    </div>
                </div></div>
                {update_capsule}
                <div class="sidebar__footer">
                    <div class="sidebar__fade" aria-hidden="true"></div>
                    <a class="provider-status" aria-label=provider_status title=provider_status href=move || settings_link("/settings/providers")><i class:is-on=provider_ready aria-hidden="true"></i></a>
                    <button type="button" class="k-icon-btn" aria-label="Quick switcher (Ctrl/Cmd+K)" title="Quick switcher (Ctrl/Cmd+K)"
                        aria-keyshortcuts="Control+k Meta+k" on:click=move |_| open_switcher()>
                        <span class="appearance-icon" inner_html=icons.quick_search.clone()></span>
                    </button>
                    <button type="button" class="k-icon-btn" aria-label="Help" title="Help" on:click=move |_| {
                        if let Some(dialog) = help.get_untracked() { let _ = dialog.show_modal(); }
                    }><span class="appearance-icon" inner_html=icons.book></span></button>
                    <a class="k-icon-btn sidebar-settings" node_ref=settings_focus aria-label="Settings" title="Settings" href=move || settings_link("/settings/general")><span class="appearance-icon" inner_html=icons.settings></span></a>
                </div>
            </aside>
            {update_dialog}
            {
                #[cfg(not(feature = "hub"))]
                {switcher::mount(switcher_requested, switcher, switcher_data, switcher_icons)}
                #[cfg(feature = "hub")]
                {switcher::render(switcher, switcher_data, switcher_icons)}
            }
            <dialog class="k-dialog k-dialog--lg workspace-dialog" node_ref=help aria-labelledby="workspace-help-title" closedby="any">
                <div class="k-dialog__head"><h2 class="k-dialog__title" id="workspace-help-title">"Keyboard shortcuts"</h2>
                    <form method="dialog"><button type="submit" class="k-icon-btn k-dialog__close" aria-label="Close" autofocus><span class="appearance-icon" inner_html=icons.close></span></button></form>
                </div>
                <div class="k-dialog__body legacy"><div class="k-page bs-root help-page"><section class="k-section">
                    <div class="k-section__head"><h2 class="k-section__title">"Workspace"</h2></div>
                    <div class="k-card">
                    {[("Ctrl/Cmd + Shift + O", "New conversation"), ("Ctrl/Cmd + K", "Quick switcher"),
                        ("Ctrl/Cmd + F", "Find in conversation"), ("Ctrl/Cmd + ,", "Settings"),
                        ("Ctrl/Cmd + B", "Toggle sidebar"), ("Ctrl/Cmd + Shift + B", "Toggle conversation panel"),
                        ("Ctrl/Cmd + 1", "Agents"), ("Ctrl/Cmd + 2", "Projects"), ("F6", "Focus message composer"),
                        ("Enter", "Send or queue message"), ("Shift + Enter", "New line"), ("Esc", "Close search or dialog")]
                        .into_iter().map(|(key, action)| view! {
                            <div class="k-row"><div class="k-row__main"><span class="k-row__text"><span class="k-row__title">{action}</span></span></div>
                                <span class="k-row__trailing"><kbd>{key}</kbd></span></div>
                        }).collect_view()}
                </div></section></div></div>
            </dialog>
            {
                #[cfg(not(feature = "hub"))]
                {agent_profile::mount(lifecycle)}
            }
            {agent_confirmation}
            {project_commands.dialogs(Signal::derive(move || projects.with(|data| data.as_ref().and_then(|result| result.as_ref().ok()).map_or_else(String::new, |data| data.root.clone()))))}
            <div class="leptos-workspace__outlet" inert=move || viewport.get() && narrow_open.get()>
                {header.map(|header| view! { <header class="leptos-workspace__header">{header()}</header> })}
                {children()}
            </div>
        </div>
    }
}

fn open_agent_actions(target: Option<web_sys::EventTarget>) {
    if let Some(button) = target
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        .and_then(|row| row.query_selector(".agent-actions").ok().flatten())
        .and_then(|button| button.dyn_into::<web_sys::HtmlElement>().ok())
    {
        let _ = button.focus();
        button.click();
    }
}
