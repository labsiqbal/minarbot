use crate::{
    api,
    connection_ui::{self as ui, Menu, MenuItem, Select, UiAction},
    general::ErrorAlert,
    shell, workspace,
};
use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub(crate) mod management;
pub(crate) use management::Commands;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Project {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) archived: bool,
    #[serde(default)]
    pub(crate) revision: u64,
    #[serde(default)]
    folders: Vec<String>,
    #[serde(default)]
    members: Vec<Member>,
    pub(crate) sections: Vec<Section>,
    #[serde(default)]
    pub(crate) threads: Vec<Thread>,
    #[serde(default)]
    codegraph_enabled: bool,
    #[cfg(not(feature = "workflows"))]
    #[serde(default)]
    allow_installation_apps: bool,
    codegraph_index: Option<Index>,
}

#[derive(Clone, Deserialize, Serialize)]
struct Member {
    agent: String,
    write: bool,
}

#[derive(Clone, Deserialize)]
pub(crate) struct Section {
    pub id: String,
    pub name: String,
    pub archived: bool,
    #[serde(default)]
    pub position: u64,
    #[serde(default)]
    pub activity: u64,
    #[serde(default)]
    pub read_position: u64,
}

#[derive(Clone, Deserialize)]
pub(crate) struct Thread {
    pub(crate) id: String,
    pub(crate) agent: String,
    pub(crate) name: String,
}

#[derive(Clone, Deserialize)]
struct Index {
    state: String,
    files: Option<u64>,
    errors: Option<u64>,
}

fn index_label(index: Option<&Index>) -> String {
    let Some(index) = index else {
        return "Off".into();
    };
    match index.state.as_str() {
        "noFolders" => "Add a folder to index code".into(),
        "unavailable" => "Unavailable on this host".into(),
        "queued" => "Indexing project folders".into(),
        "error" => "Indexing failed".into(),
        "ready" => {
            let count = index.files.unwrap_or(0);
            if count == 0 {
                return "No supported source files".into();
            }
            let files = format!(
                "{count} {} indexed",
                if count == 1 { "file" } else { "files" }
            );
            index
                .errors
                .filter(|errors| *errors > 0)
                .map_or(files.clone(), |errors| {
                    format!("{files} · {errors} with errors")
                })
        }
        _ => "Off".into(),
    }
}

fn directory() -> impl Fn() -> Vec<shell::Agent> + Copy {
    let signal = use_context::<ReadSignal<Option<Result<shell::Directory, String>>>>();
    move || {
        signal
            .and_then(|signal| signal.get())
            .and_then(Result::ok)
            .map(|directory| directory.agents)
            .unwrap_or_default()
    }
}

#[derive(Deserialize)]
struct FolderListing {
    path: String,
    parent: Option<String>,
    folders: Vec<FolderEntry>,
}

#[derive(Deserialize)]
struct FolderEntry {
    name: String,
    path: String,
}

fn folder_browser(path: RwSignal<String>, open: RwSignal<bool>) -> impl IntoView {
    let listing = RwSignal::new(None::<Result<FolderListing, String>>);
    let read = move |folder: Option<String>| {
        listing.set(None);
        leptos::task::spawn_local(async move {
            let mut url = "/api/project-folders".to_owned();
            if let Some(folder) = folder {
                if let Ok(query) = web_sys::UrlSearchParams::new() {
                    query.set("path", &folder);
                    url.push('?');
                    url.push_str(&query.to_string().as_string().unwrap_or_default());
                }
            }
            listing.try_set(Some(api::read(&url).await));
        });
    };
    read(None);
    view! {
        <section class="project-browser" aria-label="Choose existing folder">
            <div class="k-row-actions">
                <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=move || listing.with(|value| !matches!(value, Some(Ok(data)) if data.parent.is_some()))
                    on:click=move |_| read(listing.with_untracked(|value| value.as_ref().and_then(|value| value.as_ref().ok()).and_then(|data| data.parent.clone())))>"Parent folder"</button>
                <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=move || listing.with(|value| !matches!(value, Some(Ok(data)) if data.parent.is_some()))
                    on:click=move |_| {
                        if let Some(folder) = listing.with_untracked(|value| value.as_ref().and_then(|value| value.as_ref().ok()).map(|data| data.path.clone())) {
                            path.set(folder);
                            open.set(false);
                        }
                    }>"Use this folder"</button>
                <button type="button" class="k-btn k-btn--sm k-btn--secondary" on:click=move |_| open.set(false)>"Close browser"</button>
            </div>
            {move || listing.with(|listing| match listing {
                None => view! { <p role="status">"Loading folders..."</p> }.into_any(),
                Some(Err(error)) => view! { <p role="alert">{error.clone()}</p> }.into_any(),
                Some(Ok(data)) => view! {
                    <p class="project-path">{data.path.clone()}</p>
                    <div class="k-card">{data.folders.iter().map(|entry| {
                        let folder = entry.path.clone();
                        view! { <button type="button" class="k-row k-row--button" on:click=move |_| read(Some(folder.clone()))>{entry.name.clone()}</button> }
                    }).collect_view()}</div>
                    {data.folders.is_empty().then(|| view! { <p class="k-field__hint">"No available subfolders."</p> })}
                }.into_any(),
            })}
        </section>
    }
}

fn avatar(agent: shell::Agent) -> impl IntoView {
    view! {
        <span class="avatar bs-avatar project-member-avatar" aria-hidden="true">
            {if agent.avatar_revision > 0 {
                view! { <img src=format!("/api/agents/{}/avatar/image?v={}", agent.id, agent.avatar_revision) alt="" /> }.into_any()
            } else {
                agent.name.chars().next().map(|initial| initial.to_uppercase().to_string()).into_any()
            }}
        </span>
    }
}

pub fn page(id: String, error_icon: String) -> impl IntoView {
    let project = RwSignal::new(None::<Result<Project, String>>);
    let path = StoredValue::new(format!("/api/projects/{id}"));
    let error_icon = StoredValue::new(error_icon);
    let reload = move || {
        leptos::task::spawn_local(async move {
            let result = api::read(&path.get_value()).await;
            project.try_set(Some(result));
        });
    };
    let commands = expect_context::<Commands>();
    Effect::new(move |_| {
        commands.version.get();
        reload();
    });
    view! {
        <main class="chat page-view bs-root"><div class="page-scroll">
            {move || match project.get() {
                None => view! { <p class="k-empty" role="status">"Loading project…"</p> }.into_any(),
                Some(Err(error)) => view! {
                    <div class="k-page bs-root project-home">
                        {workspace::heading("Project unavailable".into(), String::new())}
                        <ErrorAlert message=RwSignal::new(error) icon=error_icon.get_value() />
                        <button type="button" class="k-btn k-btn--sm k-btn--secondary" on:click=move |_| reload()>"Reload projects"</button>
                    </div>
                }.into_any(),
                Some(Ok(project)) => overview(project).into_any(),
            }}
        </div></main>
    }
}

const TABS: &[(&str, &str)] = &[
    ("overview", "Overview"),
    ("calendar", "Calendar"),
    ("apps", "Connected apps"),
    ("environment", "Environment"),
    ("mcp", "MCP"),
];

fn overview(project: Project) -> impl IntoView {
    let agents = directory();
    let icons = use_context::<shell::Icons>().unwrap_or_default();
    let plus = StoredValue::new(icons.plus);
    let hash = StoredValue::new(icons.hash);
    let chevron = StoredValue::new(icons.chevron);
    let folder_icon = StoredValue::new(icons.folder);
    document().set_title(&format!("{} · Minarbot", project.name));
    let edit_name = RwSignal::new(project.name.clone());
    let edit_description = RwSignal::new(project.description.clone());
    let edit_members = RwSignal::new(project.members.clone());
    let edit_folders = RwSignal::new(project.folders.clone());
    let folder_path = RwSignal::new(String::new());
    let editing = RwSignal::new(false);
    let browsing = RwSignal::new(false);
    let archived = project.archived;
    let project = StoredValue::new(project);
    let project_url = project.with_value(|project| format!("/projects/{}", project.id));
    let current_tab = move || {
        if archived {
            return "overview";
        }
        window()
            .location()
            .search()
            .ok()
            .and_then(|query| {
                web_sys::UrlSearchParams::new_with_str(&query)
                    .ok()?
                    .get("projectTab")
            })
            .and_then(|id| TABS.iter().find(|(tab, _)| *tab == id).map(|(tab, _)| *tab))
            .unwrap_or("overview")
    };
    let tab = RwSignal::new(current_tab());
    let same_document = move |next: &str| (next == "calendar") == cfg!(feature = "workflows");
    let project_url = StoredValue::new(project_url);
    let select_tab = move |next: &'static str| {
        if archived || next == tab.get_untracked() {
            return;
        }
        let mut url = project_url.get_value();
        if next != "overview" {
            url.push_str("?projectTab=");
            url.push_str(next);
        }
        if same_document(next)
            && window()
                .history()
                .and_then(|history| {
                    history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&url))
                })
                .is_ok()
        {
            tab.set(next);
        } else {
            let _ = window().location().assign(&url);
        }
    };
    let listener = window_event_listener(leptos::ev::popstate, move |_| tab.set(current_tab()));
    on_cleanup(move || listener.remove());
    let commands = expect_context::<Commands>();
    let busy = commands.journal.busy;
    let error = commands.journal.error;
    let action = RwSignal::new("");
    let name = RwSignal::new(String::new());
    let member = RwSignal::new(String::new());
    let folder = RwSignal::new(
        project.with_value(|value| value.folders.first().cloned().unwrap_or_default()),
    );
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let locked = move || archived || commands.journal.locked();
    let run = move |operation: &str, body: Value| {
        let project_id = project.with_value(|project| project.id.clone());
        let done = if operation == "thread" {
            let known = project.with_value(|project| {
                project
                    .threads
                    .iter()
                    .map(|thread| thread.id.clone())
                    .collect::<Vec<_>>()
            });
            Some(Callback::new(move |result: Value| {
                if let Some(thread) =
                    result["threads"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|thread| {
                            thread["id"]
                                .as_str()
                                .is_some_and(|id| !known.iter().any(|old| old == id))
                        })
                {
                    if let (Some(agent), Some(id)) =
                        (thread["agent"].as_str(), thread["id"].as_str())
                    {
                        let _ = window()
                            .location()
                            .assign(&format!("/agents/{agent}/conversations/{id}"));
                    }
                }
            }))
        } else {
            None
        };
        project.with_value(|project| {
            commands.execute(
                management::change(&project_id, project.revision, operation, body),
                done,
            )
        });
    };
    let edit = move |browse: bool| {
        project.with_value(|value| {
            edit_name.set(value.name.clone());
            edit_description.set(value.description.clone());
            edit_members.set(value.members.clone());
            edit_folders.set(value.folders.clone());
        });
        folder_path.set(String::new());
        browsing.set(browse);
        editing.set(true);
    };
    let save = move |event: web_sys::SubmitEvent| {
        event.prevent_default();
        if locked() {
            return;
        }
        let members = edit_members.get_untracked();
        let folders = edit_folders.get_untracked();
        let grant_write = project.with_value(|value| {
            members.iter().any(|member| {
                member.write
                    && (!value
                        .members
                        .iter()
                        .any(|old| old.agent == member.agent && old.write)
                        || folders.iter().any(|folder| !value.folders.contains(folder)))
            })
        });
        let body = json!({
                "name": edit_name.get_untracked(), "description": edit_description.get_untracked(),
                "members": members, "folders": folders, "confirmWrite": grant_write,
        });
        if grant_write {
            ui::confirm(commands.confirmation, "Grant project write access?",
                "Selected agents can create, change, and delete files in project folders. Access applies only in project sections and conversations.",
                "Grant write access", false, UiAction::new(move |()| run("edit", body.clone())));
        } else {
            run("edit", body);
        }
    };
    let show = move |kind| {
        if locked() {
            return;
        }
        action.set(kind);
        name.set(String::new());
        if let Some(dialog) = dialog.get_untracked() {
            let _ = dialog.show_modal();
        }
    };
    let submit_form = move |event: web_sys::SubmitEvent| {
        event.prevent_default();
        if locked() {
            return;
        }
        match action.get_untracked() {
            "sections" if !name.get_untracked().trim().is_empty() => {
                run("sections", json!({ "name": name.get_untracked() }))
            }
            "thread" if !member.get_untracked().is_empty() => run(
                "thread",
                json!({ "name": "Project conversation", "agent": member.get_untracked(), "folder": folder.get_untracked() }),
            ),
            _ => {}
        }
    };
    let member_name = move |id: &str| {
        agents()
            .into_iter()
            .find(|agent| agent.id == id)
            .map_or_else(|| "Agent".into(), |agent| agent.name)
    };
    let label = move || {
        if action.get() == "sections" {
            "New section"
        } else {
            "New conversation"
        }
    };
    #[cfg(feature = "workflows")]
    let calendar_panel = view! {
                <Show when=move || tab.get() == "calendar">
                    <div id="project-calendar-panel" class="k-tabs__panel calendar-in-project" role="tabpanel" aria-labelledby="project-calendar-tab">
                        <crate::automation::Calendar project=Some(project.with_value(|row| json!({"id":row.id,"archived":row.archived,"members":row.members}))) />
                    </div>
                </Show>
    };
    #[cfg(not(feature = "workflows"))]
    let calendar_panel = ();
    #[cfg(not(feature = "workflows"))]
    let mcp_first_page = StoredValue::new(None);
    #[cfg(not(feature = "workflows"))]
    let apps_catalog = StoredValue::new(None);
    #[cfg(not(feature = "workflows"))]
    let environment_data = StoredValue::new(None);
    view! {
        <div class="page legacy is-wide"><div class="k-page bs-root project-home">
            <header class="k-page-head">
                {workspace::heading_text(project.with_value(|project| project.name.clone()), project.with_value(|project| project.description.clone()))}
                <div class="k-page-head__actions">
                    <Menu label="Project menu" disabled=move || commands.journal.locked() items=if archived {
                        vec![management::restore_item(commands, project.get_value())]
                    } else {
                        vec![
                            MenuItem::new("Edit project", UiAction::new(move |()| { select_tab("overview"); edit(false); })),
                            management::archive_item(commands, project.get_value(), "Archive project"),
                        ]
                    } />
                </div>
            </header>
            {commands.recovery()}
            <div class="k-tabs">
                <div class="k-tabs__list" role="tablist" aria-label="Project sections"
                    on:keydown=move |event| crate::settings::move_focus(&event, "[role=tablist]", true)>
                    {TABS.iter().map(|&(id, label)| {
                        let selected = move || tab.get() == id;
                        view! {
                            <button type="button" class="k-tabs__tab" role="tab" id=format!("project-{id}-tab")
                                aria-controls=format!("project-{id}-panel") aria-selected=move || selected().to_string()
                                tabindex=move || if selected() { 0 } else { -1 }
                                disabled=archived && id != "overview"
                                data-state=move || if selected() { "active" } else { "inactive" }
                                on:click=move |_| select_tab(id)>{label}</button>
                        }
                    }).collect_view()}
                </div>
                {calendar_panel}
                {move || {
                    #[cfg(not(feature = "workflows"))]
                    {
                        let id = tab.get();
                        let members = || project.with_value(|row| row.members.iter().map(|member| member.agent.clone()).collect::<Vec<_>>());
                        let project_id = || project.with_value(|row| row.id.clone());
                        let panel = match id {
                            "apps" => crate::connected_apps::scoped_panel(Some(crate::connected_apps::ProjectApps {
                                id: StoredValue::new(project_id()), members: StoredValue::new(members()),
                                inherited: project.with_value(|row| row.allow_installation_apps),
                                change: Callback::new(move |enabled| run("edit", json!({"allowInstallationApps":enabled,"confirmed":enabled}))),
                                locked: Signal::derive(locked),
                                catalog: apps_catalog,
                            })).into_any(),
                            "environment" => crate::project_integrations::environment::panel(project_id(), archived, environment_data).into_any(),
                            "mcp" => crate::mcp_connections::scoped_panel(Some(project_id()), Some(members()), Some(mcp_first_page)).into_any(),
                            _ => return ().into_any(),
                        };
                        view! { <div id=format!("project-{id}-panel") class="k-tabs__panel" role="tabpanel" aria-labelledby=format!("project-{id}-tab")>{panel}</div> }.into_any()
                    }
                    #[cfg(feature = "workflows")]
                    ().into_any()
                }}
            <div id="project-overview-panel" class="k-tabs__panel" role="tabpanel" aria-labelledby="project-overview-tab" hidden=move || tab.get() != "overview">
            <form class="k-stack" hidden=move || !editing.get() on:submit=save>
                <section class="k-section">
                    <div class="k-section__head"><h2 class="k-section__title">"Project details"</h2></div>
                    <label class="k-field"><span class="k-field__label">"Project name"</span><input class="k-input" required maxlength="80" prop:value=edit_name readonly=locked on:input=move |event| edit_name.set(event_target_value(&event)) /></label>
                    <label class="k-field"><span class="k-field__label">"Description"</span><textarea class="k-input k-textarea" maxlength="2000" prop:value=edit_description readonly=locked on:input=move |event| edit_description.set(event_target_value(&event)) /></label>
                </section>
                <section class="k-section">
                    <div class="k-section__head"><h2 class="k-section__title">"Members"</h2></div>
                    {move || edit_members.get().into_iter().map(|member| {
                        let agent = StoredValue::new(member.agent);
                        let name = member_name(&agent.get_value());
                        view! {
                            <div class="k-row"><div class="k-row__main"><span class="k-row__title">{name.clone()}</span></div>
                                <div class="k-row__trailing"><select class="k-input" aria-label=format!("Access for {name}") disabled=locked
                                    on:change=move |event| edit_members.update(|members| {
                                        if let Some(member) = members.iter_mut().find(|entry| entry.agent == agent.get_value()) {
                                            member.write = event_target_value(&event) == "write";
                                        }
                                    })>
                                    <option selected=!member.write value="read">"Read only"</option>
                                    <option selected=member.write value="write">"Read and write"</option>
                                </select><button type="button" class="k-icon-btn" aria-label=format!("Remove {name} from project") disabled=locked
                                    on:click=move |_| edit_members.update(|members| members.retain(|entry| entry.agent != agent.get_value()))>"×"</button></div>
                            </div>
                        }
                    }).collect_view()}
                    <select class="k-input" aria-label="Add project member" disabled=locked on:change=move |event| {
                        let agent = event_target_value(&event);
                        if !agent.is_empty() { edit_members.update(|members| members.push(Member { agent, write: false })); }
                    }>
                        <option selected value="">"Add member..."</option>
                        {move || agents().into_iter().filter(|agent| edit_members.with(|members| !members.iter().any(|entry| entry.agent == agent.id)))
                            .map(|agent| view! { <option value=agent.id>{agent.name}</option> }).collect_view()}
                    </select>
                </section>
                <section class="k-section">
                    <div class="k-section__head"><h2 class="k-section__title">"Folders"</h2></div>
                    {move || edit_folders.get().into_iter().map(|folder| {
                        let remove = folder.clone();
                        view! { <div class="k-row"><span class="k-row__title project-path">{folder.clone()}</span>
                            <button type="button" class="k-icon-btn" disabled=locked aria-label=format!("Remove folder {folder}")
                                on:click=move |_| edit_folders.update(|folders| folders.retain(|path| path != &remove))>"×"</button></div> }
                    }).collect_view()}
                    <label class="k-field"><span class="k-field__label">"Add existing folder"</span>
                        <input class="k-input" prop:value=folder_path readonly=locked on:input=move |event| folder_path.set(event_target_value(&event)) /></label>
                    <div class="k-row-actions">
                        <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=move || locked() || folder_path.with(String::is_empty) || edit_folders.with(|folders| folders.len() >= 32)
                            on:click=move |_| { let path = folder_path.get_untracked(); edit_folders.update(|folders| { if !folders.contains(&path) { folders.push(path); } }); folder_path.set(String::new()); }>"Add folder"</button>
                        <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=locked on:click=move |_| browsing.set(true)>"Browse folders"</button>
                    </div>
                    <Show when=move || browsing.get()>
                        {folder_browser(folder_path, browsing)}
                    </Show>
                    <div class="k-row-actions">
                        <button type="submit" class="k-btn k-btn--md k-btn--primary" disabled=locked>"Save project"</button>
                        <button type="button" class="k-btn k-btn--md k-btn--secondary" disabled=busy on:click=move |_| { editing.set(false); browsing.set(false); }>"Cancel"</button>
                    </div>
                </section>
            </form>
            <section class="k-section" hidden=editing>
                <div class="k-section__head"><h2 class="k-section__title">"Members"</h2>
                    <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=locked on:click=move |_| edit(false)>"Manage members & folders"</button></div>
                <div class="k-card">{move || project.with_value(|project| {
                    if project.members.is_empty() { return view! { <div class="k-empty"><div class="k-empty__title">"No members yet"</div></div> }.into_any(); }
                    project.members.iter().map(|member| {
                        let agent = agents().into_iter().find(|agent| agent.id == member.agent);
                        let name = agent.as_ref().map_or_else(|| "Agent".into(), |agent| agent.name.clone());
                        view! { <div class="k-row"><div class="k-row__main">
                            {agent.map(|agent| view! { <span class="k-row__leading">{avatar(agent)}</span> })}
                            <span class="k-row__text"><span class="k-row__title">{name}</span></span>
                        </div><span class="k-row__trailing"><span class="k-badge is-neutral">{if member.write { "Can edit" } else { "Read only" }}</span></span></div> }
                    }).collect_view().into_any()
                })}</div>
            </section>
            <section class="k-section" hidden=editing>
                <div class="k-section__head"><h2 class="k-section__title">"Folders"</h2>
                    <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=locked on:click=move |_| edit(true)>
                        <span class="appearance-icon" inner_html=plus.get_value()></span>"Add folder"</button></div>
                {project.with_value(|project| if project.folders.is_empty() {
                    view! { <p class="k-field__hint">"No folders"</p> }.into_any()
                } else {
                    view! { <div class="k-card">{project.folders.iter().enumerate().map(|(index, path)| view! {
                        <div class="k-row"><div class="k-row__main"><span class="k-row__leading" inner_html=folder_icon.get_value()></span><span class="k-row__text">
                            <span class="k-row__title project-path">{path.clone()}</span>
                            <span class="k-row__meta">{if index == 0 { "Default folder for sections" } else { "Available to project sections and conversations" }}</span>
                        </span></div></div>
                    }).collect_view()}</div> }.into_any()
                })}
            </section>
            <div class="k-card"><div class="k-row"><div class="k-row__main"><span class="k-row__text">
                <span class="k-row__title">"Minarbot CodeGraph"</span>
                <span class="k-row__meta project-index-status" role="status">{project.with_value(|value| index_label(value.codegraph_index.as_ref()))}</span>
            </span></div><span class="k-row__trailing">
                <crate::general::Switch label="Minarbot CodeGraph"
                    checked=Signal::derive(move || project.with_value(|value| value.codegraph_enabled))
                    disabled=Signal::derive(move || locked() || editing.get() || project.with_value(|value| value.codegraph_index.as_ref().is_none_or(|index| index.state == "unavailable")))
                    on_change=move |enabled| run("edit", json!({ "codegraphEnabled": enabled })) />
            </span></div></div>
            <section class="k-section">
                <div class="k-section__head"><h2 class="k-section__title">"Sections"</h2>
                    <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=locked on:click=move |_| show("sections")>
                        <span class="appearance-icon" inner_html=plus.get_value()></span>"New section"</button></div>
                <div class="k-card">{project.with_value(|project| project.sections.iter().map(|section| view! {
                    <div class="k-row is-clickable"><a class="k-row__main" href=format!("/rooms/{}", section.id)>
                        <span class="k-row__leading" inner_html=hash.get_value()></span><span class="k-row__text">
                        <span class="k-row__title">{section.name.clone()}</span>
                        {section.archived.then(|| view! { <span class="k-row__meta">"Archived"</span> })}
                    </span><span class="k-row__chev" inner_html=chevron.get_value()></span></a>
                    {section.archived.then(|| management::section_menu(commands, project.clone(), section.clone()))}
                    </div>
                }).collect_view())}</div>
            </section>
            <section class="k-section">
                <div class="k-section__head"><h2 class="k-section__title">"Conversations"</h2>
                    <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=locked on:click=move |_| {
                        if project.with_value(|project| project.members.is_empty()) {
                            edit(false);
                        } else { show("thread"); }
                    }><span class="appearance-icon" inner_html=plus.get_value()></span>"New conversation"</button></div>
                {project.with_value(|row| (!row.folders.is_empty()).then(|| view! {
                    <Select name="project-conversation-folder" label="Conversation folder" value=folder.into()
                        options=Signal::stored(row.folders.iter().map(|path| (path.clone(), path.clone())).collect())
                        change=Callback::new(move |path| folder.set(path)) />
                }))}
                {move || project.with_value(|project| if project.threads.is_empty() {
                    view! { <p class="k-field__hint">"No conversations"</p> }.into_any()
                } else {
                    view! { <div class="k-card">{project.threads.iter().map(|thread| view! {
                        <div class="k-row"><a class="k-row__main" href=format!("/agents/{}/conversations/{}", thread.agent, thread.id)><span class="k-row__text">
                            <span class="k-row__title">{thread.name.clone()}</span><span class="k-row__meta">{member_name(&thread.agent)}</span>
                        </span></a></div>
                    }).collect_view()}</div> }.into_any()
                })}
            </section>
            <Show when=move || tab.get() == "overview" && !folder.with(String::is_empty)>
                {move || crate::project_integrations::git::panel(StoredValue::new(project.with_value(|row| row.id.clone())), folder.get())}
            </Show>
            </div></div>
            <dialog class="k-dialog bs-root project-dialog" aria-labelledby="project-dialog-title" node_ref=dialog>
                <div class="k-dialog__head"><h2 id="project-dialog-title" class="k-dialog__title">{label}</h2></div>
                <form class="k-dialog__body k-stack" id="project-action" on:submit=submit_form>
                    <Show when=move || !error.with(String::is_empty)><ui::Alert>{error}</ui::Alert></Show>
                    <label class="k-field" hidden=move || action.get() != "sections"><span class="k-field__label">"Section name"</span>
                        <input class="k-input" required=move || action.get() == "sections" maxlength="80" prop:value=name on:input=move |event| name.set(event_target_value(&event)) /></label>
                    <label class="k-field" hidden=move || action.get() != "thread"><span class="k-field__label">"Agent"</span>
                        <select class="k-input" required=move || action.get() == "thread" prop:value=member on:change=move |event| member.set(event_target_value(&event))>
                            <option value="">"Choose agent"</option>
                            {move || project.with_value(|project| project.members.iter().map(|agent| view! { <option value=agent.agent.clone()>{member_name(&agent.agent)}</option> }).collect_view())}
                        </select></label>
                    <label class="k-field" hidden=move || action.get() != "thread" || project.with_value(|project| project.folders.is_empty())>
                        <span class="k-field__label">"Conversation folder"</span><select class="k-input" prop:value=folder on:change=move |event| folder.set(event_target_value(&event))>
                            {project.with_value(|project| project.folders.iter().map(|path| view! { <option value=path.clone()>{path.clone()}</option> }).collect_view())}
                        </select>
                    </label>
                </form>
                <div class="k-dialog__foot"><button type="button" class="k-btn k-btn--md k-btn--secondary" on:click=move |_| {
                    if let Some(dialog) = dialog.get_untracked() { dialog.close(); }
                }>"Cancel"</button><button type="submit" form="project-action" class="k-btn k-btn--md k-btn--primary" disabled=locked>
                    {move || if action.get() == "sections" { "Create section" } else { "Create conversation" }}
                </button></div>
            </dialog>
        </div></div>
    }
}
