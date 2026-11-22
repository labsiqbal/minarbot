use crate::{api, appearance, settings};
use leptos::prelude::*;
use serde::Deserialize;

#[derive(Clone, Deserialize)]
pub(crate) struct Directory {
    pub agents: Vec<Agent>,
    pub guide: Option<Agent>,
    #[serde(default)]
    pub configured: bool,
    #[serde(default, rename = "hubAvailable")]
    pub hub_available: bool,
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
    pub revision: u64,
    #[serde(default)]
    pub avatar_revision: u64,
}

#[derive(Clone, Deserialize)]
struct Project {
    id: String,
    name: String,
    description: String,
    archived: bool,
    sections: Vec<ProjectSection>,
}

#[derive(Clone, Deserialize)]
struct ProjectSection {
    id: String,
    name: String,
    archived: bool,
}

#[derive(Deserialize)]
struct Projects {
    projects: Vec<Project>,
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

#[derive(Deserialize)]
struct Machines {
    machines: Vec<Machine>,
}

#[derive(Deserialize)]
struct Machine {
    id: String,
    name: String,
    status: String,
    agents: Vec<Agent>,
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
    let projects = RwSignal::new(None::<Result<Projects, String>>);
    let summaries = RwSignal::new(Vec::<Summary>::new());
    let machines = RwSignal::new(None::<Result<Machines, String>>);
    let projects_tab = RwSignal::new(
        (base_path.starts_with("/projects/") || base_path.starts_with("/rooms/"))
            || preference(TAB).as_deref() == Some("projects"),
    );
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
    let retry = move || {
        directory.set(None);
        projects.set(None);
        leptos::task::spawn_local(async move {
            directory.try_set(Some(api::read("/api/agents").await));
        });
        leptos::task::spawn_local(async move {
            projects.try_set(Some(api::read("/api/projects").await));
        });
        leptos::task::spawn_local(async move {
            if let Ok(data) = api::read::<Summaries>("/api/sidebar/summary").await {
                summaries.try_set(data.summaries);
            }
        });
        leptos::task::spawn_local(async move {
            machines.try_set(Some(api::read("/api/machines").await));
        });
    };
    retry();
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
    let switcher = NodeRef::<leptos::html::Dialog>::new();
    let help = NodeRef::<leptos::html::Dialog>::new();
    let query = RwSignal::new(String::new());
    let show_switcher = move || {
        query.set(String::new());
        if let Some(dialog) = switcher.get_untracked() {
            let _ = dialog.show_modal();
        }
    };
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
            "k" => show_switcher(),
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
                }
            }
            path
        });
    }
    let route_path = move || {
        #[cfg(feature = "conversation")]
        if let Some(navigation) = conversation {
            return navigation
                .0
                .with(|route| route.split('?').next().unwrap_or_default().to_owned());
        }
        automation
            .map(|nav| nav.0.get())
            .unwrap_or_else(|| base_path.get_value())
    };
    let settings_link = move |section| {
        let mut path = route_path();
        let location = window().location();
        if location.pathname().ok().as_deref() == Some(path.as_str()) {
            path.push_str(&location.search().unwrap_or_default());
        }
        settings::section_url(section, &path)
    };
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
    let switch_matches = Memo::new(move |_| {
        let query = query.get().to_lowercase();
        let mut rows = Vec::new();
        directory.with(|state| {
            if let Some(Ok(data)) = state {
                rows.extend(
                    data.agents
                        .iter()
                        .chain(&data.guide)
                        .filter(|agent| !agent.hidden)
                        .map(|agent| {
                            (
                                agent.name.clone(),
                                "Agents",
                                format!("/agents/{}", agent.id),
                            )
                        }),
                );
            }
        });
        projects.with(|state| {
            if let Some(Ok(data)) = state {
                for project in data.projects.iter().filter(|project| !project.archived) {
                    rows.push((
                        project.name.clone(),
                        "Projects",
                        format!("/projects/{}", project.id),
                    ));
                    rows.extend(
                        project
                            .sections
                            .iter()
                            .filter(|section| !section.archived)
                            .map(|section| {
                                (
                                    section.name.clone(),
                                    "Sections",
                                    format!("/rooms/{}", section.id),
                                )
                            }),
                    );
                }
            }
        });
        rows.retain(|(label, group, _)| {
            let haystack = format!("{label} {group}").to_lowercase();
            query.split_whitespace().all(|term| haystack.contains(term))
        });
        rows
    });
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
            let detail = summary.filter(|row| !row.preview.is_empty()).map(|row| plain_preview(&row.preview)).unwrap_or_else(|| agent.role.clone());
            if agent.hidden && !current || !format!("{} {}", agent.name, detail).to_lowercase().contains(&query) {
                return None;
            }
            if attention.get() && summary.is_none_or(|row| row.needs_you == 0) {
                return None;
            }
            let active = summary.map_or(0, |row| row.needs_you);
            let working = summary.is_some_and(|row| row.status == "running");
            let status = if active > 0 { "Needs you" } else if working { "Working" } else { "Idle" };
            let dot = if active > 0 { "is-attention" } else if working { "is-working" } else { "is-idle" };
            let time = summary.filter(|_| active == 0).and_then(|row| row.time.as_deref());
            let avatar = (agent.avatar_revision > 0).then(|| format!("/api/agents/{}/avatar/image?v={}", agent.id, agent.avatar_revision));
            let initial = agent.name.chars().next().map(|initial| initial.to_uppercase().to_string());
            Some(view! {
                <a class="bot-row" class:is-selected=current href=path aria-current=current.then_some("page")>
                    <span class="bot-row__avatar" aria-hidden="true"><span class="avatar bs-avatar">
                        {if let Some(src) = avatar { view! { <img src=src alt="" /> }.into_any() } else { initial.into_any() }}
                    </span><span class=format!("status-dot {dot}")></span></span>
                    <span class="sr-only">{status}</span>
                    <span class="bot-row__text"><span class="bot-row__name">{agent.name}</span><span class="bot-row__preview">{detail}</span></span>
                    {time.map(|time| view! { <time class="bot-row__time" datetime=time.to_owned()>{relative_time(time)}</time> })}
                    {(active > 0).then(|| view! { <span class="needs-you-badge"><span class="k-badge is-warn" aria-label=format!("{active} need you")>{active}</span></span> })}
                </a>
            })
        }).collect_view();
        if items.is_empty() {
            view! { <p class="machine-group__note" role="status">{if attention.get() && query.is_empty() { "No agents need you." } else if query.is_empty() && !online.get() { "No agents yet." } else { "No matching agents." }}</p> }.into_any()
        } else {
            items.into_any()
        }
    };
    let remote_rows = move || {
        let query = agent_search.get().trim().to_lowercase();
        let online = online.get();
        let hidden = attention.get();
        machines.with(|result| match result {
            Some(Ok(result)) if !hidden => result.machines.iter()
                .filter(|machine| !online || machine.status == "online")
                .filter_map(|machine| {
                    let links = machine.agents.iter().filter_map(|agent| {
                        if !format!("{} {}", agent.name, agent.role).to_lowercase().contains(&query) { return None; }
                        let path = format!("/machines/{}/agents/{}", machine.id, agent.id);
                        let current = route_path() == path;
                        Some(view! {
                            <a class="bot-row" class:is-selected=current href=path aria-current=current.then_some("page")
                                aria-label=format!("{} on {}", agent.name, machine.name)>
                                <span class="bot-row__text"><span class="bot-row__name">{agent.name.clone()}</span><span class="bot-row__preview">{agent.role.clone()}</span></span>
                            </a>
                        })
                    }).collect_view();
                    if links.is_empty() && !query.is_empty() { return None; }
                    Some(view! {
                        <details class="machine-group" open>
                            <summary class="machine-group__header"><span class="machine-group__name">{machine.name.clone()}</span></summary>
                            <nav class="bot-list" aria-label=format!("Agents on {}", machine.name)>
                                {if links.is_empty() { view! { <p class="machine-group__note">"No agents cached yet."</p> }.into_any() } else { links.into_any() }}
                            </nav>
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
    let chevron_icon = StoredValue::new(icons.chevron.clone());
    let project_rows = move || {
        let query = project_search.get().trim().to_lowercase();
        projects.with(|projects| match projects {
            None => view! { <p class="machine-group__note" role="status">"Loading projects…"</p> }.into_any(),
            Some(Err(_)) => view! { <p class="machine-group__note" role="alert">"Projects could not be loaded."</p><button class="k-btn k-btn--sm" on:click=move |_| retry()>"Retry"</button> }.into_any(),
            Some(Ok(projects)) => {
                let rows = projects.projects.iter().filter(|project| !project.archived).filter_map(|project| {
                    let name_matches = format!("{} {}", project.name, project.description).to_lowercase().contains(&query);
                    let sections = project.sections.iter().filter(|section| !section.archived && (name_matches || section.name.to_lowercase().contains(&query))).map(|section| {
                        let path = format!("/rooms/{}", section.id);
                        let current = route_path() == path;
                        let href = path;
                        view! { <div class="project-navigation__section" class:is-selected=current><a class="project-navigation__room" class:is-selected=current href=href aria-current=current.then_some("page")
                            aria-label=format!("# {} in {}", section.name, project.name)><span class="appearance-icon" inner_html=section_icon.get_value()></span><span>{section.name.clone()}</span></a></div> }
                    }).collect_view();
                    if !name_matches && sections.is_empty() { return None; }
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
                                    <span class:is-open=open inner_html=chevron_icon.get_value()></span>
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
                            </div>
                            <div class="project-navigation__sections" hidden=move || !open()>{sections}</div>
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
    let conversation_creation =
        move || conversation_navigation && route_path().starts_with("/agents/");
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
                            <summary class="k-icon-btn" aria-label="Create new" title="Create new"><span class="appearance-icon" inner_html=plus></span></summary>
                            <div class="k-pop k-menu">
                                <button type="button" class="k-menu__item" hidden=move || !conversation_creation() on:click=move |_| {
                                    if let Some(menu) = create_menu.get_untracked() { menu.set_open(false); }
                                    if let Ok(event) = web_sys::Event::new("minarbot:new-conversation") {
                                        let _ = window().dispatch_event(&event);
                                    }
                                }>"New conversation"</button>
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
                                    <div class="machine-group__content"><nav class="bot-list" aria-label="Choose agent">{agent_rows}</nav></div>
                                </details>
                                {remote_rows}
                                <a class="bot-list__empty" href=move || settings_link("/settings/machines") hidden=move || attention.get() || machines.with(|value|
                                    !matches!(value, Some(Ok(machines)) if machines.machines.is_empty()))>
                                    <span class="appearance-icon" inner_html=monitor.clone()></span><span><span class="bot-row__name">"Pair a machine"</span></span>
                                </a>
                            </nav>
                        </div>
                        <div id="workspace-panel-projects" role="tabpanel" aria-labelledby="workspace-tab-projects" hidden=move || !projects_tab.get()>
                            <nav class="project-navigation" aria-label="Projects">{project_rows}</nav>
                        </div>
                    </div>
                </div></div>
                <div class="sidebar__footer">
                    <div class="sidebar__fade" aria-hidden="true"></div>
                    <a class="provider-status" aria-label=provider_status title=provider_status href=move || settings_link("/settings/providers")><i class:is-on=provider_ready aria-hidden="true"></i></a>
                    <button type="button" class="k-icon-btn" aria-label="Quick switcher (Ctrl/Cmd+K)" title="Quick switcher (Ctrl/Cmd+K)"
                        aria-keyshortcuts="Control+k Meta+k" on:click=move |_| show_switcher()>
                        <span class="appearance-icon" inner_html=icons.quick_search.clone()></span>
                    </button>
                    <button type="button" class="k-icon-btn" aria-label="Help" title="Help" on:click=move |_| {
                        if let Some(dialog) = help.get_untracked() { let _ = dialog.show_modal(); }
                    }><span class="appearance-icon" inner_html=icons.book></span></button>
                    <a class="k-icon-btn sidebar-settings" node_ref=settings_focus aria-label="Settings" title="Settings" href=move || settings_link("/settings/general")><span class="appearance-icon" inner_html=icons.settings></span></a>
                </div>
            </aside>
            <dialog class="k-dialog k-dialog--md workspace-dialog" node_ref=switcher aria-label="Quick switcher">
                <div class="quick-switcher">
                    <form class="quick-switcher__search" on:submit=move |event| {
                        event.prevent_default();
                        if let Some(path) = switch_matches.with_untracked(|rows| rows.first().map(|row| row.2.clone())) {
                            let href = path;
                            let _ = window().location().assign(&href);
                        }
                    }>
                        <span class="appearance-icon" inner_html=icons.quick_search.clone()></span>
                        <input class="k-input" aria-label="Jump to" placeholder="Jump to…" autofocus
                            prop:value=query on:input=move |event| query.set(event_target_value(&event)) />
                        <button type="button" class="k-btn k-btn--sm k-btn--ghost" aria-label="Close quick switcher" on:click=move |_| {
                            if let Some(dialog) = switcher.get_untracked() { dialog.close(); }
                        }><kbd>"Esc"</kbd></button>
                    </form>
                    <nav class="quick-switcher__list" aria-label="Destinations">
                        {move || switch_matches.get().into_iter().map(|(name, group, path)| {
                            let href = path;
                            view! { <a class="quick-switcher__item" href=href><span>{name}<small>{group}</small></span></a> }
                        }).collect_view()}
                        <p class="quick-switcher__empty" role="status" hidden=move || !switch_matches.with(Vec::is_empty)>"No results"</p>
                    </nav>
                </div>
            </dialog>
            <dialog class="k-dialog k-dialog--lg workspace-dialog" node_ref=help aria-labelledby="workspace-help-title">
                <div class="k-dialog__head"><h2 class="k-dialog__title" id="workspace-help-title">"Keyboard shortcuts"</h2>
                    <form method="dialog"><button type="submit" class="k-btn k-btn--sm k-btn--ghost" autofocus>"Close"</button></form>
                </div>
                <div class="k-dialog__body"><div class="k-card">
                    {[("Ctrl/Cmd + K", "Quick switcher"), ("Ctrl/Cmd + B", "Toggle sidebar"),
                        ("Ctrl/Cmd + 1", "Agents"), ("Ctrl/Cmd + 2", "Projects"),
                        ("Ctrl/Cmd + F", "Find in conversation"), ("Ctrl/Cmd + Shift + B", "Toggle conversation panel"),
                        ("Enter", "Send or queue message"), ("Shift + Enter", "New line"), ("Esc", "Close dialog")]
                        .into_iter().map(|(key, action)| view! {
                            <div class="k-row"><div class="k-row__main"><span class="k-row__text"><span class="k-row__title">{action}</span></span></div>
                                <span class="k-row__trailing"><kbd>{key}</kbd></span></div>
                        }).collect_view()}
                </div></div>
            </dialog>
            <div class="leptos-workspace__outlet" inert=move || viewport.get() && narrow_open.get()>
                {header.map(|header| view! { <header class="leptos-workspace__header">{header()}</header> })}
                {children()}
            </div>
        </div>
    }
}
