use super::{api, Directory, Icons, Machines, Projects};
use crate::{settings, workspace_route::valid_id};
use leptos::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Clone, Deserialize)]
struct Thread {
    id: String,
    agent: String,
    name: String,
    #[serde(default)]
    archived: u8,
    #[serde(default)]
    deleted: u8,
    #[serde(default)]
    delivery: bool,
}

#[derive(Clone, Deserialize)]
pub(super) struct Setting {
    id: String,
    label: String,
    svg: String,
}

#[derive(Clone, PartialEq)]
enum Destination {
    Link(String),
    Restore(String, u64),
    NewConversation,
    NewAgent,
    NewProject,
}

#[derive(Clone, PartialEq)]
struct Item {
    title: String,
    group: &'static str,
    detail: String,
    icon: String,
    avatar: Option<Option<String>>,
    destination: Destination,
}

impl Item {
    fn link(
        title: impl Into<String>,
        group: &'static str,
        detail: impl Into<String>,
        icon: String,
        path: String,
    ) -> Self {
        Self {
            title: title.into(),
            group,
            detail: detail.into(),
            icon,
            avatar: None,
            destination: Destination::Link(path),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Data {
    pub directory: RwSignal<Option<Result<Directory, String>>>,
    pub machines: RwSignal<Option<Result<Machines, String>>>,
    pub projects: RwSignal<Option<Result<Projects, String>>>,
    pub base_path: Signal<String>,
    pub new_conversation: Signal<bool>,
    pub new_project: RwSignal<bool>,
    pub online: RwSignal<bool>,
    pub attention: RwSignal<bool>,
}

#[cfg(not(feature = "hub"))]
pub(super) fn mount(
    requested: RwSignal<bool>,
    opened: RwSignal<bool>,
    data: Data,
    icons: Icons,
) -> impl IntoView {
    view! {
        <Show when=move || requested.get()>
            {crate::lazy_view::load("Quick switcher", {
                let icons = icons.clone();
                move || switcher_dialog(opened, data, icons.clone())
            })}
        </Show>
    }
}

#[cfg(not(feature = "hub"))]
#[leptos::wasm_split::wasm_split(switcher, wasm_split_path = leptos::wasm_split, fallible)]
fn switcher_dialog(
    opened: RwSignal<bool>,
    data: Data,
    icons: Icons,
) -> crate::lazy_view::ChunkView {
    Ok(render(opened, data, icons).into_any())
}

fn items(data: &Data, icons: &Icons, threads: Vec<Thread>) -> Vec<Item> {
    let mut items = Vec::new();
    let directory = data.directory.get().and_then(Result::ok);
    let agents = || {
        directory
            .iter()
            .flat_map(|data| data.agents.iter().chain(data.guide.iter()))
    };
    if data.new_conversation.get() {
        let mut item = Item::link(
            "New conversation",
            "Create",
            "Ctrl/Cmd+Shift+O",
            icons.plus.clone(),
            String::new(),
        );
        item.destination = Destination::NewConversation;
        items.push(item);
    }
    for agent in agents().filter(|agent| valid_id(&agent.id)) {
        let mut item = Item::link(
            &agent.name,
            "Agents",
            "",
            String::new(),
            format!("/agents/{}", agent.id),
        );
        item.avatar = Some((agent.avatar_revision > 0).then(|| {
            format!(
                "/api/agents/{}/avatar/image?v={}",
                agent.id, agent.avatar_revision
            )
        }));
        if agent.hidden {
            item.title = format!("Restore {}", agent.name);
            item.group = "Hidden agents";
            item.destination = Destination::Restore(agent.id.clone(), agent.visibility_revision);
        }
        items.push(item);
    }
    if !data.attention.get() {
        let machines = data.machines.get().and_then(Result::ok);
        for machine in machines
            .iter()
            .flat_map(|machines| machines.machines.iter())
            .filter(|machine| {
                valid_id(&machine.id) && (!data.online.get() || machine.status == "online")
            })
        {
            for agent in machine.agents.iter().filter(|agent| valid_id(&agent.id)) {
                let mut item = Item::link(
                    &agent.name,
                    "Agents",
                    &machine.name,
                    String::new(),
                    format!("/machines/{}/agents/{}", machine.id, agent.id),
                );
                item.avatar = Some(None);
                items.push(item);
            }
        }
    }
    let thread_link = |agent: &str, id: &str, name: String, detail: String| {
        let path = format!("/agents/{agent}/conversations/{id}");
        Item::link(name, "Conversations", detail, icons.message.clone(), path)
    };
    if let Some(Ok(projects)) = data.projects.get() {
        for project in projects
            .projects
            .into_iter()
            .filter(|project| !project.archived && valid_id(&project.id))
        {
            items.push(Item::link(
                &project.name,
                "Projects",
                "",
                icons.folder.clone(),
                format!("/projects/{}", project.id),
            ));
            for section in project
                .sections
                .into_iter()
                .filter(|section| !section.archived && valid_id(&section.id))
            {
                items.push(Item::link(
                    format!("# {}", section.name),
                    "Sections",
                    &project.name,
                    icons.hash.clone(),
                    format!("/rooms/{}", section.id),
                ));
            }
            for thread in project
                .threads
                .into_iter()
                .filter(|thread| valid_id(&thread.id) && valid_id(&thread.agent))
            {
                items.push(thread_link(
                    &thread.agent,
                    &thread.id,
                    thread.name,
                    project.name.clone(),
                ));
            }
        }
    }
    for thread in threads {
        let detail = agents()
            .find(|agent| agent.id == thread.agent)
            .map_or_else(String::new, |agent| agent.name.clone());
        items.push(thread_link(&thread.agent, &thread.id, thread.name, detail));
    }
    let base = data.base_path.get();
    for setting in &icons.settings_choices {
        if setting.id != "hub"
            || directory
                .as_ref()
                .is_some_and(|directory| directory.hub_available)
        {
            let path = settings::section_url(&format!("/settings/{}", setting.id), &base);
            items.push(Item::link(
                &setting.label,
                "Settings",
                "",
                setting.svg.clone(),
                path,
            ));
        }
    }
    if directory.is_some() {
        let mut item = Item::link("New agent", "Create", "", icons.plus.clone(), String::new());
        item.destination = Destination::NewAgent;
        items.push(item);
    }
    let mut item = Item::link(
        "New project",
        "Create",
        "",
        icons.plus.clone(),
        String::new(),
    );
    item.destination = Destination::NewProject;
    items.push(item);
    items
}

pub(super) fn render(opened: RwSignal<bool>, data: Data, icons: Icons) -> impl IntoView {
    let lifecycle = use_context::<crate::agent_profile::Lifecycle>();
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let query = RwSignal::new(String::new());
    let active = RwSignal::new(0_usize);
    let threads = RwSignal::new(Vec::<Thread>::new());
    let thread_error = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let restoring = RwSignal::new(false);
    let generation = RwSignal::new(0_u32);
    let directory = data.directory;
    let pending = RwSignal::new(0);
    let fetch_threads = move || {
        let revision = generation.get_untracked().wrapping_add(1);
        generation.set(revision);
        threads.set(Vec::new());
        thread_error.set(false);
        let agents = directory.with(|result| match result {
            Some(Ok(data)) => data
                .agents
                .iter()
                .chain(data.guide.iter())
                .filter(|agent| !agent.hidden && valid_id(&agent.id))
                .map(|agent| agent.id.clone())
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        });
        pending.set(agents.len());
        for agent in agents {
            leptos::task::spawn_local(async move {
                let result =
                    api::read::<Vec<Thread>>(&format!("/api/agents/{agent}/threads")).await;
                if generation.try_get_untracked() != Some(revision)
                    || opened.try_get_untracked() != Some(true)
                {
                    return;
                }
                pending.update(|count| *count -= 1);
                match result {
                    Ok(rows) => threads.update(|threads| {
                        threads.extend(rows.into_iter().filter(|row| {
                            row.agent == agent
                                && valid_id(&row.id)
                                && row.archived == 0
                                && row.deleted == 0
                                && !row.delivery
                        }))
                    }),
                    Err(_) => thread_error.set(true),
                }
            });
        }
    };
    Effect::new(move |_| {
        if opened.get() {
            fetch_threads();
            crate::connection_ui::poll_with_backoff(
                30000,
                move || (pending.get_untracked() == 0).then(|| thread_error.get_untracked()),
                fetch_threads,
            );
        }
    });
    Effect::new(move |_| {
        if let Some(dialog) = dialog.get() {
            if opened.get() && !dialog.open() {
                query.set(String::new());
                active.set(0);
                let connected = dialog.is_connected();
                let show = move || {
                    if opened.try_get_untracked() == Some(true) && dialog.is_connected() {
                        let _ = dialog.show_modal();
                    }
                };
                if connected {
                    show();
                } else {
                    request_animation_frame(show);
                }
            } else if !opened.get() && dialog.open() {
                dialog.close();
            }
        }
    });
    let search_icon = icons.quick_search.clone();
    let matches = Memo::new(move |_| {
        if !opened.get() {
            return Vec::new();
        }
        let mut items = items(&data, &icons, threads.get());
        let query = query.get().to_lowercase();
        items.retain(|item| {
            let haystack = format!("{} {} {}", item.title, item.detail, item.group).to_lowercase();
            query.split_whitespace().all(|term| haystack.contains(term))
        });
        items.sort_by_key(|item| item.group);
        items
    });
    let current = move || {
        active
            .get()
            .min(matches.with(|items| items.len().saturating_sub(1)))
    };
    let choose = move |item: Item| {
        opened.set(false);
        if let Some(dialog) = dialog.get_untracked() {
            dialog.close();
        }
        match item.destination {
            Destination::Link(path) => {
                if item.group == "Agents" {
                    super::save_preference(super::TAB, "agents");
                }
                if matches!(item.group, "Projects" | "Sections") {
                    super::save_preference(super::TAB, "projects");
                }
                let _ = window().location().assign(&path);
            }
            Destination::NewConversation => super::start_conversation(),
            Destination::NewProject => data.new_project.set(true),
            Destination::NewAgent => {
                if let Some(lifecycle) = lifecycle {
                    lifecycle.open(crate::agent_profile::DialogTarget::Create);
                }
            }
            Destination::Restore(id, revision) => {
                super::save_preference(super::TAB, "agents");
                if restoring.get_untracked() {
                    return;
                }
                restoring.set(true);
                error.set(String::new());
                leptos::task::spawn_local(async move {
                    match api::command::<Value>(
                        &format!("/api/agents/{id}/visibility"),
                        json!({"revision": revision, "hidden": false}),
                    )
                    .await
                    {
                        Ok(_) => {
                            directory.try_set(Some(api::read("/api/agents").await));
                        }
                        Err(reason) => {
                            error.try_set(reason);
                        }
                    }
                    restoring.try_set(false);
                });
            }
        }
    };
    view! {
        <p class="message-error" role="alert" hidden=move || error.with(String::is_empty)>{error}</p>
        <dialog node_ref=dialog class="k-dialog k-dialog--md workspace-dialog" aria-label="Quick switcher" closedby="any"
          on:cancel=move |event: web_sys::Event| {
            event.prevent_default();
            opened.set(false);
        } on:close=move |_| {
            if dialog.get_untracked().is_some_and(|dialog| dialog.open()) { return; }
            opened.set(false);
            if document().active_element().is_none_or(|element| element.tag_name() == "BODY") {
                use wasm_bindgen::JsCast;
                if let Ok(Some(input)) = document().query_selector(".composer__input") {
                    if let Ok(input) = input.dyn_into::<web_sys::HtmlElement>() { let _ = input.focus(); }
                }
            }
        }>
            <div class="quick-switcher">
                <div class="quick-switcher__search">
                    <span class="appearance-icon" inner_html=search_icon></span>
                    <input class="k-input" role="combobox" aria-label="Jump to" placeholder="Jump to…" autofocus
                        aria-autocomplete="list" aria-expanded="true" aria-controls="workspace-switch-list"
                        aria-activedescendant=move || (!matches.with(Vec::is_empty)).then(|| format!("workspace-switch-{}", current()))
                        prop:value=query on:input=move |event| { query.set(event_target_value(&event)); active.set(0); }
                        on:keydown=move |event| {
                            if event.is_composing() || event.key_code() == 229 { return; }
                            let items = matches.get_untracked();
                            let length = items.len();
                            if length == 0 { return; }
                            let index = untrack(current);
                            let next = match event.key().as_str() {
                                "ArrowDown" => (index + 1) % length,
                                "ArrowUp" => (index + length - 1) % length,
                                "Home" => 0,
                                "End" => length - 1,
                                "Enter" => { event.prevent_default(); choose(items[index].clone()); return; }
                                _ => return,
                            };
                            event.prevent_default();
                            active.set(next);
                            if let Some(row) = document().get_element_by_id(&format!("workspace-switch-{next}")) {
                                let options = web_sys::ScrollIntoViewOptions::new();
                                options.set_block(web_sys::ScrollLogicalPosition::Nearest);
                                row.scroll_into_view_with_scroll_into_view_options(&options);
                            }
                        } />
                    <kbd>"Esc"</kbd>
                </div>
                <div id="workspace-switch-list" class="quick-switcher__list" role="listbox" aria-label="Destinations">
                    {move || {
                        let mut group = "";
                        matches.get().into_iter().enumerate().map(|(index, item)| {
                            let heading = (group != item.group).then_some(item.group);
                            group = item.group;
                            let icon = if let Some(src) = item.avatar.clone() {
                                let initial = item.title.strip_prefix("Restore ").unwrap_or(&item.title).chars().next().map(|letter| letter.to_uppercase().to_string());
                                view! { <span class="avatar bs-avatar quick-switcher__avatar" aria-hidden="true">{src.map(|src| view! { <img src=src alt="" /> }.into_any()).unwrap_or_else(|| initial.into_any())}</span> }.into_any()
                            } else {
                                view! { <span class="appearance-icon" inner_html=item.icon.clone()></span> }.into_any()
                            };
                            let title = item.title.clone();
                            let detail = (!item.detail.is_empty()).then(|| item.detail.clone());
                            view! {
                                <div>
                                    {heading.map(|group| view! { <div class="quick-switcher__group">{group}</div> })}
                                    <div id=format!("workspace-switch-{index}") role="option" aria-selected=move || (current() == index).to_string()
                                        class="quick-switcher__item" on:pointermove=move |_| active.set(index)
                                        on:mousedown=move |event| event.prevent_default() on:click=move |_| choose(item.clone())>
                                        {icon}<span>{title}{detail.map(|detail| view! { <small>{detail}</small> })}</span>
                                    </div>
                                </div>
                            }
                        }).collect_view()
                    }}
                </div>
                <p class="quick-switcher__empty" role="status" hidden=move || !matches.with(Vec::is_empty)>"No results"</p>
                <p class="message-error" role="alert" hidden=move || !thread_error.get()>"Some conversations could not load. Reopen to retry."</p>
            </div>
        </dialog>
    }
}
