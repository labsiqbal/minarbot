use crate::{api, general::ErrorAlert, shell, workspace};
use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const RECOVERY: &str = "minarbot:project-command";

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    id: String,
    name: String,
    description: String,
    archived: bool,
    revision: u64,
    folders: Vec<String>,
    members: Vec<Member>,
    sections: Vec<Section>,
    threads: Vec<Thread>,
    codegraph_enabled: bool,
    codegraph_index: Option<Index>,
}

#[derive(Clone, Deserialize, Serialize)]
struct Member {
    agent: String,
    write: bool,
}

#[derive(Clone, Deserialize)]
struct Section {
    id: String,
    name: String,
    archived: bool,
}

#[derive(Clone, Deserialize)]
struct Thread {
    id: String,
    agent: String,
    name: String,
}

#[derive(Clone, Deserialize)]
struct Index {
    state: String,
    files: Option<u64>,
    errors: Option<u64>,
}

#[derive(Clone, Deserialize, Serialize)]
struct Pending {
    path: String,
    body: Value,
}

fn read_pending() -> Result<Option<Pending>, String> {
    let storage = window()
        .session_storage()
        .ok()
        .flatten()
        .ok_or("Project recovery storage is unavailable.")?;
    let Some(raw) = storage
        .get_item(RECOVERY)
        .map_err(|_| "Saved project request could not be read.")?
    else {
        return Ok(None);
    };
    serde_json::from_str::<Pending>(&raw)
        .ok()
        .filter(|value| {
            value.body["id"]
                .as_str()
                .is_some_and(crate::workspace_route::valid_id)
                && recovery_path(&value.path)
        })
        .map(Some)
        .ok_or_else(|| "Saved project request could not be read.".into())
}

fn recovery_path(path: &str) -> bool {
    if path == "/api/projects" {
        return true;
    }
    let Some((id, operation)) = path
        .strip_prefix("/api/projects/")
        .and_then(|rest| rest.split_once('/'))
    else {
        return false;
    };
    crate::workspace_route::valid_id(id)
        && (matches!(
            operation,
            "edit" | "archive" | "delete" | "thread" | "sections"
        ) || operation
            .strip_prefix("sections/")
            .and_then(|rest| rest.strip_suffix("/edit"))
            .is_some_and(crate::workspace_route::valid_id))
}

async fn submit(command: Pending) -> Result<Value, String> {
    let storage = window()
        .session_storage()
        .ok()
        .flatten()
        .ok_or("Project recovery storage is unavailable.")?;
    let raw = serde_json::to_string(&command).map_err(|_| "Project request could not be saved.")?;
    storage
        .set_item(RECOVERY, &raw)
        .map_err(|_| "Project request could not be saved.")?;
    let result = api::post(&command.path, &command.body).await?;
    if storage.get_item(RECOVERY).ok().flatten().as_ref() == Some(&raw) {
        storage
            .remove_item(RECOVERY)
            .map_err(|_| "The project changed but recovery storage could not be cleared.")?;
    }
    Ok(result)
}

fn new_command(path: String, mut body: Value) -> Result<Pending, String> {
    let id = window()
        .crypto()
        .map_err(|_| "Could not create a project request.")?
        .random_uuid();
    body["id"] = json!(id);
    Ok(Pending { path, body })
}

fn confirmation(text: &str) -> bool {
    window().confirm_with_message(text).unwrap_or(false)
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
    reload();
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
                Some(Ok(project)) => overview(project, error_icon.get_value()).into_any(),
            }}
        </div></main>
    }
}

fn overview(project: Project, error_icon: String) -> impl IntoView {
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
    let project = StoredValue::new(project);
    let project_url = project.with_value(|project| format!("/projects/{}", project.id));
    let react_url = format!("{project_url}?ui=react");
    let pending = RwSignal::new(read_pending());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let action = RwSignal::new("");
    let name = RwSignal::new(String::new());
    let member = RwSignal::new(String::new());
    let folder = RwSignal::new(
        project.with_value(|value| value.folders.first().cloned().unwrap_or_default()),
    );
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let archived = project.with_value(|project| project.archived);
    let locked =
        move || busy.get() || archived || pending.with(|pending| !matches!(pending, Ok(None)));
    let run = move |command: Result<Pending, String>| {
        if busy.get_untracked() {
            return;
        }
        let command = match command {
            Ok(command) => command,
            Err(message) => {
                error.set(message);
                return;
            }
        };
        let kind = command
            .path
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_owned();
        let agent = command.body["agent"].as_str().map(str::to_owned);
        let old_threads = project.with_value(|project| {
            project
                .threads
                .iter()
                .map(|thread| thread.id.clone())
                .collect::<Vec<_>>()
        });
        busy.set(true);
        error.set(String::new());
        let api_path = project.with_value(|project| format!("/api/projects/{}", project.id));
        leptos::task::spawn_local(async move {
            match submit(command).await {
                Ok(_) if kind == "archive" => {
                    let _ = window().location().assign("/");
                }
                Ok(_) if kind == "thread" => {
                    let result = api::read::<Project>(&api_path).await;
                    if let (Some(agent), Ok(saved)) = (agent, result) {
                        if let Some(thread) = saved.threads.iter().find(|thread| {
                            thread.agent == agent && !old_threads.contains(&thread.id)
                        }) {
                            let _ = window().location().assign(&format!(
                                "/agents/{}/conversations/{}",
                                thread.agent, thread.id
                            ));
                            return;
                        }
                    }
                    let _ = window().location().reload();
                }
                Ok(_) => {
                    let _ = window().location().reload();
                }
                Err(message) => {
                    pending.try_set(read_pending());
                    error.try_set(message);
                    busy.try_set(false);
                }
            }
        });
    };
    let build = move |operation: &str, body: Value| {
        project.with_value(|project| {
            let mut body = body;
            body["revision"] = json!(project.revision);
            new_command(format!("/api/projects/{}/{operation}", project.id), body)
        })
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
        if !grant_write || confirmation("Grant project write access? Selected agents can create, change, and delete files in project folders.") {
            run(build("edit", json!({
                "name": edit_name.get_untracked(), "description": edit_description.get_untracked(),
                "members": members, "folders": folders, "confirmWrite": grant_write,
            })));
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
            "sections" if !name.get_untracked().trim().is_empty() => run(build(
                "sections",
                json!({ "name": name.get_untracked().trim() }),
            )),
            "thread" if !member.get_untracked().is_empty() => run(build(
                "thread",
                json!({ "name": "Project conversation", "agent": member.get_untracked(), "folder": folder.get_untracked() }),
            )),
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
    view! {
        <div class="page legacy is-wide"><div class="k-page bs-root project-home">
            <header class="k-page-head">
                {workspace::heading_text(project.with_value(|project| project.name.clone()), project.with_value(|project| project.description.clone()))}
                <div class="k-page-head__actions">
                    <button class="k-icon-btn" type="button" popovertarget="project-actions" aria-label="Project menu" disabled=locked>"⋯"</button>
                    <div id="project-actions" class="k-pop k-menu" popover="auto">
                        <button type="button" class="k-menu__item" popovertarget="project-actions" popovertargetaction="hide" on:click=move |_| edit(false)>"Edit project"</button>
                        <button type="button" class="k-menu__item" on:click=move |_| {
                            if !locked() && confirmation("Archive project? Sections and conversations become read-only. History and files stay saved.") {
                                run(build("archive", json!({ "confirmed": true })));
                            }
                        }>"Archive project"</button>
                    </div>
                </div>
            </header>
            <ErrorAlert message=error icon=error_icon />
            <div class="k-alert is-warn" role="status" hidden=move || pending.with(|value| matches!(value, Ok(None)))>
                <div class="k-alert__body">
                    <div class="k-alert__title">{move || if pending.with(Result::is_err) { "Saved project request could not be read" } else { "Project change is not confirmed" }}</div>
                    <div class="k-alert__actions">
                        <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=busy hidden=move || pending.with(|value| !matches!(value, Ok(Some(_))))
                            on:click=move |_| { if let Ok(Some(request)) = pending.get_untracked() { run(Ok(request)); } }>"Retry project change"</button>
                        <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=busy on:click=move |_| {
                            if confirmation("Clear project request? Check the project for changes before clearing. This does not undo changes.") {
                                if let Ok(Some(storage)) = window().session_storage() {
                                    if storage.remove_item(RECOVERY).is_ok() { pending.set(Ok(None)); }
                                }
                            }
                        }>"Clear request"</button>
                    </div>
                </div>
            </div>

            <div class="k-tabs"><nav class="k-tabs__list" aria-label="Project sections">
                <a class="k-tabs__tab" href=project_url aria-current="page" data-state="active">"Overview"</a>
                {[( "calendar", "Calendar"), ("apps", "Connected apps"), ("environment", "Environment"), ("mcp", "MCP")].into_iter()
                    .map(|(tab, label)| view! { <a class="k-tabs__tab" href=format!("{react_url}&projectTab={tab}")>{label}</a> }).collect_view()}
            </nav>
            <div class="k-tabs__panel">
            <form class="k-stack" hidden=move || !editing.get() on:submit=save>
                <section class="k-section">
                    <div class="k-section__head"><h2 class="k-section__title">"Project details"</h2></div>
                    <label class="k-field"><span class="k-field__label">"Project name"</span><input class="k-input" required maxlength="80" prop:value=edit_name readonly=locked on:input=move |event| edit_name.set(event_target_value(&event)) /></label>
                    <label class="k-field"><span class="k-field__label">"Description"</span><textarea class="k-input" maxlength="2000" prop:value=edit_description readonly=locked on:input=move |event| edit_description.set(event_target_value(&event)) /></label>
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
                    project.members.iter().map(|member| view! {
                        <div class="k-row"><div class="k-row__main"><span class="k-row__title">{member_name(&member.agent)}</span></div>
                            <span class="k-row__trailing"><span class="k-badge is-neutral">{if member.write { "Can edit" } else { "Read only" }}</span></span>
                        </div>
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
                    on_change=move |enabled| run(build("edit", json!({ "codegraphEnabled": enabled }))) />
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
                    </span><span class="k-row__chev" inner_html=chevron.get_value()></span></a></div>
                }).collect_view())}</div>
            </section>
            <section class="k-section">
                <div class="k-section__head"><h2 class="k-section__title">"Conversations"</h2>
                    <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=locked on:click=move |_| {
                        if project.with_value(|project| project.members.is_empty()) {
                            edit(false);
                        } else { show("thread"); }
                    }><span class="appearance-icon" inner_html=plus.get_value()></span>"New conversation"</button></div>
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
            </div></div>
            <dialog class="k-dialog bs-root project-dialog" aria-labelledby="project-dialog-title" node_ref=dialog>
                <div class="k-dialog__head"><h2 id="project-dialog-title" class="k-dialog__title">{label}</h2></div>
                <form class="k-dialog__body k-stack" id="project-action" on:submit=submit_form>
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
