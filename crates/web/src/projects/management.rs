use super::{folder_browser, Project, Section as ProjectSection};
use crate::{
    connection_ui::*,
    settings_recovery::{Command, Journal, UUID},
};
use leptos::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Clone, Copy)]
pub struct Commands {
    pub journal: Journal,
    pub confirmation: Confirm,
    pub version: RwSignal<u64>,
    pub create: RwSignal<bool>,
    name: RwSignal<Option<NameDialog>>,
    refresh: UiAction,
}

#[derive(Clone)]
struct NameDialog {
    path: String,
    revision: u64,
    title: &'static str,
    name: String,
}

impl Commands {
    pub fn new(refresh: UiAction) -> Self {
        Self {
            journal: Journal::new("minarbot:project-command", 160_000, |command| {
                command.valid(
                    &format!("^/api/projects(/{UUID}/(edit|archive|restore|delete|thread|sections(/{UUID}/edit)?))?$"),
                    &["revision", "name", "description", "folders", "members", "confirmWrite",
                      "agent", "folder", "codegraphEnabled", "allowInstallationApps", "confirmed", "archived", "position"],
                )
            }),
            confirmation: Confirm::new(None),
            version: RwSignal::new(0),
            create: RwSignal::new(false),
            name: RwSignal::new(None),
            refresh,
        }
    }

    pub fn execute(self, command: Command, done: Option<Callback<Value>>) {
        let create = command.path == "/api/projects";
        let archived = command
            .path
            .strip_suffix("/archive")
            .and_then(|path| path.strip_prefix("/api/projects/"))
            .map(str::to_owned);
        self.journal.run(command, None, move |result| {
            let Some(result) = result else {
                return;
            };
            self.name.set(None);
            self.version.update(|version| *version += 1);
            self.refresh.run(());
            if create {
                self.create.set(false);
                if let Some(channel) = result["channel"]
                    .as_str()
                    .filter(|id| crate::workspace_route::valid_id(id))
                {
                    let _ = window().location().assign(&format!("/rooms/{channel}"));
                }
            }
            if archived.is_some_and(|id| {
                let path = window().location().pathname().unwrap_or_default();
                path == format!("/projects/{id}")
                    || result["sections"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|row| {
                            row["id"]
                                .as_str()
                                .is_some_and(|section| path == format!("/rooms/{section}"))
                        })
                    || result["threads"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|row| {
                            let (Some(agent), Some(thread)) =
                                (row["agent"].as_str(), row["id"].as_str())
                            else {
                                return false;
                            };
                            path == format!("/agents/{agent}/conversations/{thread}")
                        })
            }) {
                let _ = window().location().assign("/");
            }
            if let Some(done) = done {
                done.run(result);
            }
        });
    }

    pub fn recovery(self) -> impl IntoView {
        let journal = self.journal;
        view! {
            <ErrorNotice error=journal.error.into() retry=self.refresh label="Reload projects" />
            <Show when=move || journal.invalid.get() || journal.pending.with(Option::is_some)>
                <Show when=move || journal.busy.get() fallback=move || view! {
                    <Alert tone="warn" actions=Box::new(move || view! {
                        <Show when=move || journal.pending.with(Option::is_some)>
                            <Button size="sm" label="Retry project change" click=UiAction::new(move |()| {
                                if let Some(command) = journal.pending.get_untracked() { self.execute(command, None); }
                            }) />
                        </Show>
                        <Button size="sm" label="Clear request" click=UiAction::new(move |()| {
                            confirm(self.confirmation, "Clear project request?",
                                "Check project state first. Changes already saved and folders already created remain. Clearing this request does not undo them.",
                                "Clear request", false, UiAction::new(move |()| {
                                    journal.discard(self.refresh);
                                    self.version.update(|version| *version += 1);
                                }));
                        }) />
                    }.into_any())>
                        {move || if journal.invalid.get() { "Saved project request could not be read" } else { "Project change is not confirmed" }}
                    </Alert>
                }><span class="k-progress" role="status"><span class="k-spinner" aria-hidden="true"></span>"Applying change..."</span></Show>
            </Show>
        }
    }

    pub fn dialogs(self, root: Signal<String>) -> impl IntoView {
        #[cfg(not(feature = "hub"))]
        let create = move || {
            self.create.get().then(|| {
                crate::lazy_view::load("new project dialog", move || create_dialog(self, root))
            })
        };
        #[cfg(feature = "hub")]
        let create = ();
        view! {
            {create}
            {move || self.name.get().map(|dialog| name_dialog(self, dialog))}
            <ConfirmDialog state=self.confirmation />
        }
    }
}

#[cfg(not(feature = "hub"))]
#[leptos::wasm_split::wasm_split(project_dialog, wasm_split_path = leptos::wasm_split, fallible)]
fn create_dialog(commands: Commands, root: Signal<String>) -> crate::lazy_view::ChunkView {
    Ok(create_project(commands, root).into_any())
}

pub fn change(project: &str, revision: u64, operation: &str, mut body: Value) -> Command {
    body["revision"] = json!(revision);
    Command::new(format!("/api/projects/{project}/{operation}"), body)
}

fn name_dialog(commands: Commands, dialog: NameDialog) -> impl IntoView {
    let value = RwSignal::new(dialog.name);
    let title = dialog.title;
    let close = UiAction::new(move |()| commands.name.set(None));
    let locked = move || commands.journal.locked();
    view! {
        <Modal title=title.into() busy=commands.journal.busy close=close footer=Box::new(move || view! {
            <Button label="Cancel" disabled=commands.journal.busy click=close />
            <Button label=if title == "New section" { "Create section" } else { "Save name" }
                busy=commands.journal.busy variant="primary" submit=true form="project-name-form"
                disabled=move || locked() || value.with(|name| name.trim().is_empty()) />
        }.into_any())>
            <form id="project-name-form" class="k-stack" on:submit=move |event: web_sys::SubmitEvent| {
                event.prevent_default();
                if locked() { return; }
                commands.execute(Command::new(dialog.path.clone(), json!({"revision":dialog.revision, "name":value.get_untracked()})), None);
            }>
                <Field label=if title == "Rename project" { "Project name" } else { "Section name" } name="project-name">
                    <input class="k-input" id="project-name" required maxlength="80" prop:value=value disabled=locked
                        on:input=move |event| value.set(event_target_value(&event)) />
                </Field>
                {commands.recovery()}
            </form>
        </Modal>
    }
}

pub fn archive_item(commands: Commands, project: Project, label: &'static str) -> MenuItem {
    MenuItem::new(label, UiAction::new(move |()| {
        let command = change(&project.id, project.revision, "archive", json!({"confirmed":true}));
        confirm(commands.confirmation, format!("Archive project {}?", project.name),
            "Sections and conversations become read-only. History, members, folder settings, and files stay saved.",
            "Archive project", true, UiAction::new(move |()| commands.execute(command.clone(), None)));
    })).danger()
}

pub fn restore_item(commands: Commands, project: Project) -> MenuItem {
    MenuItem::new(
        "Restore project",
        UiAction::new(move |()| {
            let command = change(
                &project.id,
                project.revision,
                "restore",
                json!({"confirmed":true}),
            );
            confirm(
                commands.confirmation,
                format!("Restore project {}?", project.name),
                "The project returns to the sidebar.",
                "Restore project",
                false,
                UiAction::new(move |()| commands.execute(command.clone(), None)),
            );
        }),
    )
}

pub fn project_menu(commands: Commands, project: Project) -> impl IntoView {
    let id = project.id.clone();
    let revision = project.revision;
    let new_section = NameDialog {
        path: format!("/api/projects/{id}/sections"),
        revision,
        title: "New section",
        name: String::new(),
    };
    let rename = NameDialog {
        path: format!("/api/projects/{id}/edit"),
        revision,
        title: "Rename project",
        name: project.name.clone(),
    };
    let label = format!("Menu project {}", project.name);
    let items = if project.archived {
        vec![restore_item(commands, project)]
    } else {
        vec![
            MenuItem::new(
                "New section",
                UiAction::new(move |()| commands.name.set(Some(new_section.clone()))),
            ),
            MenuItem::new(
                "Rename",
                UiAction::new(move |()| commands.name.set(Some(rename.clone()))),
            ),
            MenuItem::new(
                "Project settings",
                UiAction::new(move |()| {
                    let _ = window().location().assign(&format!("/projects/{id}"));
                }),
            ),
            archive_item(commands, project, "Archive"),
        ]
    };
    view! { <Menu label=label items=items disabled=move || commands.journal.locked() /> }
}

pub fn section_menu(
    commands: Commands,
    project: Project,
    section: ProjectSection,
) -> impl IntoView {
    let op = format!("sections/{}/edit", section.id);
    let label = format!("Menu section {} in {}", section.name, project.name);
    let active: Vec<_> = project
        .sections
        .iter()
        .filter(|row| !row.archived)
        .collect();
    let index = active.iter().position(|row| row.id == section.id);
    let archived = section.archived;
    let status = if archived {
        "Restore section"
    } else {
        "Archive section"
    };
    let command = change(
        &project.id,
        project.revision,
        &op,
        json!({"archived": !archived}),
    );
    let toggle = MenuItem::new(
        status,
        UiAction::new(move |()| {
            confirm(
                commands.confirmation,
                format!("{status} {}?", section.name),
                if archived {
                    "The section returns to the sidebar."
                } else {
                    "History, memory, and files stay saved. View archived sections from project home."
                },
                status,
                !archived,
                UiAction::new({
                    let command = command.clone();
                    move |()| commands.execute(command.clone(), None)
                }),
            );
        }),
    );
    let mut items = Vec::new();
    if let Some(index) = index {
        let rename = NameDialog {
            path: format!("/api/projects/{}/{op}", project.id),
            revision: project.revision,
            title: "Rename section",
            name: active[index].name.clone(),
        };
        items.push(MenuItem::new(
            "Rename section",
            UiAction::new(move |()| commands.name.set(Some(rename.clone()))),
        ));
        for (label, neighbor) in [
            (
                "Move section up",
                index.checked_sub(1).and_then(|i| active.get(i)),
            ),
            ("Move section down", active.get(index + 1)),
        ] {
            let cmd = neighbor.map(|row| {
                change(
                    &project.id,
                    project.revision,
                    &op,
                    json!({"position":row.position}),
                )
            });
            let disabled = cmd.is_none();
            items.push(MenuItem::new(
                label,
                UiAction::new(move |()| {
                    if let Some(command) = cmd.clone() {
                        commands.execute(command, None);
                    }
                }),
            ));
            if let Some(item) = items.last_mut() {
                item.disabled = disabled.into();
            }
        }
    }
    items.push(if archived { toggle } else { toggle.danger() });
    view! { <Menu label=label items=items disabled=move || project.archived || commands.journal.locked() /> }
}

#[derive(Clone)]
struct Folder {
    id: u32,
    existing: RwSignal<String>,
    mode: RwSignal<String>,
    browse: RwSignal<bool>,
}

fn create_project(commands: Commands, root: Signal<String>) -> impl IntoView {
    let name = RwSignal::new(String::new());
    let description = RwSignal::new(String::new());
    let folders = RwSignal::new(Vec::<Folder>::new());
    let next_folder = RwSignal::new(0_u32);
    let members = RwSignal::new(Vec::<super::Member>::new());
    let agents = scoped_agents(false, None);
    let locked = move || commands.journal.locked();
    let close = UiAction::new(move |()| commands.create.set(false));
    let needed = Signal::derive(move || {
        let mut rows = agents.get();
        rows.retain(|(id, _)| {
            !members.with(|members| members.iter().any(|member| &member.agent == id))
        });
        rows.insert(0, (String::new(), "Add member...".into()));
        rows
    });
    view! {
        <Modal title="New project".into() size="lg" busy=commands.journal.busy close=close footer=Box::new(move || view! {
            <Button label="Cancel" disabled=commands.journal.busy click=close />
            <Button label="Create project" variant="primary" form="new-project" submit=true busy=commands.journal.busy
                disabled=move || locked() || name.with(|name| name.trim().is_empty()) />
        }.into_any())>
            <form id="new-project" class="k-stack" on:submit=move |event: web_sys::SubmitEvent| {
                event.prevent_default();
                if locked() { return; }
                let members = members.get_untracked();
                let write = members.iter().any(|member| member.write);
                let folders = folders.with_untracked(|folders| folders.iter().map(|folder| {
                    if folder.mode.with_untracked(|mode| mode == "new") { json!({"mode":"new"}) }
                    else { json!({"mode":"existing", "path":folder.existing.get_untracked()}) }
                }).collect::<Vec<_>>());
                let command = Command::new("/api/projects", json!({"name":name.get_untracked(), "description":description.get_untracked(),
                    "folders":folders, "members":members, "confirmWrite":write}));
                if write {
                    confirm(commands.confirmation, "Grant project write access?",
                        "Selected agents can create, change, and delete files in these folders while working in this project. Archiving the project keeps folder contents.",
                        "Grant write access", false, UiAction::new(move |()| commands.execute(command.clone(), None)));
                } else { commands.execute(command, None); }
            }>
                <Field label="Project name" name="new-project-name">
                    <input class="k-input" id="new-project-name" required maxlength="80" disabled=locked prop:value=name
                        on:input=move |event| name.set(event_target_value(&event)) />
                </Field>
                <Field label="Description" name="new-project-description" optional=true>
                    <textarea class="k-input k-textarea" id="new-project-description" maxlength="2000" disabled=locked prop:value=description
                        on:input=move |event| description.set(event_target_value(&event))></textarea>
                </Field>
                <Section title="Folders">
                    {move || folders.get().into_iter().enumerate().map(|(index, folder)| {
                        let number = index + 1;
                        let Folder { id, mode, existing, browse } = folder;
                        let label = format!("Folder {number}");
                        let input = format!("project-folder-{id}");
                        let mode_label = format!("{input}-mode");
                        view! { <div class="k-stack">
                            <label class="k-field"><span class="k-field__label">{label.clone()}</span>
                                <span class="connection-select">
                                    <select class="k-input k-select" id=mode_label disabled=locked prop:value=mode on:change=move |event| {
                                        mode.set(event_target_value(&event)); existing.set(String::new()); browse.set(false);
                                    }><option value="new">"Create a new folder"</option><option value="existing">"Use an existing folder"</option></select>
                                    <span class="k-select__chev"><Icon name="chevron-down" /></span>
                                </span>
                            </label>
                            <Show when=move || mode.with(|mode| mode == "existing") fallback=move || view! {
                                <p class="k-field__hint">"Creates a private folder under "<span class="project-path">{root}</span>"."</p>
                            }>
                                <label class="k-field"><span class="k-field__label">{format!("Existing folder path {number}")}</span>
                                    <input id=input.clone() class="k-input" required placeholder="/home/you/work/project" disabled=locked
                                        prop:value=existing on:input=move |event| existing.set(event_target_value(&event)) />
                                </label>
                                <Button label="Browse folders" size="sm" disabled=locked click=UiAction::new(move |()| browse.set(true)) />
                                <Show when=move || browse.get()>{move || folder_browser(existing, browse)}</Show>
                            </Show>
                            <Button label=format!("Remove folder {number}") size="sm" disabled=locked click=UiAction::new(move |()| {
                                folders.update(|rows| rows.retain(|row| row.id != id));
                            }) />
                        </div> }
                    }).collect_view()}
                    <div><Button label="Add folder" size="sm" icon="plus" disabled=move || locked() || folders.with(|folders| folders.len() >= 32)
                        click=UiAction::new(move |()| {
                            let id = next_folder.get_untracked();
                            next_folder.set(id + 1);
                            let mode = if folders.with_untracked(Vec::is_empty) { "new" } else { "existing" };
                            folders.update(|folders| folders.push(Folder {
                                id, mode: RwSignal::new(mode.into()), existing: RwSignal::new(String::new()), browse: RwSignal::new(false),
                            }));
                        }) /></div>
                </Section>
                <Section title="Members">
                    <div class="k-stack">
                        {move || {
                            let rows = members.get();
                            (!rows.is_empty()).then(|| view! { <div class="k-card">{rows.into_iter().map(|member| {
                                let id = StoredValue::new(member.agent);
                                let agent = super::directory()().into_iter().find(|agent| id.with_value(|id| id == &agent.id));
                                let name = agent.as_ref().map_or_else(|| "Agent".into(), |agent| agent.name.clone());
                                let initial = Signal::stored(if member.write { "write".to_owned() } else { "read".to_owned() });
                                view! { <div class="k-row">
                                    <div class="k-row__main">
                                        {agent.map(|agent| view! { <span class="k-row__leading">{super::avatar(agent)}</span> })}
                                        <span class="k-row__text"><span class="k-row__title">{name.clone()}</span></span></div>
                                    <div class="k-row__trailing">
                                        <span class="connection-select">
                                            <select class="k-input k-select" aria-label=format!("Access for {name}") prop:value=initial disabled=locked
                                                on:change=move |event| members.update(|members| {
                                                    if let Some(row) = members.iter_mut().find(|row| id.with_value(|id| id == &row.agent)) {
                                                        row.write = event_target_value(&event) == "write";
                                                    }
                                                })><option value="read">"Read only"</option><option value="write">"Read and write"</option></select>
                                            <span class="k-select__chev"><Icon name="chevron-down" /></span>
                                        </span>
                                        <button class="k-icon-btn" type="button" aria-label=format!("Remove {name} from project")
                                            disabled=locked on:click=move |_| members.update(|rows| rows.retain(|row| id.with_value(|id| id != &row.agent)))><Icon name="close" /></button>
                                    </div>
                                </div> }
                            }).collect_view()}</div> })
                        }}
                        <Show when=move || needed.with(|rows| rows.len() > 1)>
                            <Select name="add-project-member" label="Add project member" value=Signal::stored(String::new()) options=needed disabled=locked
                                change=Callback::new(move |agent: String| {
                                    if !agent.is_empty() { members.update(|rows| rows.push(super::Member {agent, write:false})); }
                                }) />
                        </Show>
                    </div>
                </Section>
                {commands.recovery()}
            </form>
        </Modal>
    }
}

#[derive(Clone, Deserialize, PartialEq)]
struct Archive {
    projects: Vec<ArchivedProject>,
}

#[derive(Clone, Deserialize, PartialEq)]
struct ArchivedProject {
    id: String,
    name: String,
    revision: u64,
}

pub fn archived_projects(commands: Commands) -> impl IntoView {
    let open = RwSignal::new(false);
    let archive = query::<Archive>(move || {
        if open.get_untracked() {
            "/api/projects?archived=1".into()
        } else {
            String::new()
        }
    });
    Effect::new(move |_| {
        commands.version.get();
        open.get();
        archive.reload.run(());
    });
    view! {
        <details class="project-navigation__archive" on:toggle=move |event| {
            if let Some(element) = event.target().and_then(|node| wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlDetailsElement>(node).ok()) {
                open.set(element.open());
            }
        }>
            <summary class="project-navigation__new">"Archived projects"</summary>
            <ErrorNotice error=archive.error.into() retry=archive.reload />
            <Show when=move || archive.busy.get() && archive.data.with(Option::is_none)>
                <p class="machine-group__note" role="status">"Loading archived projects…"</p>
            </Show>
            {move || archive.data.get().map(|data| {
                if data.projects.is_empty() {
                    return view! { <p class="machine-group__note">"No archived projects."</p> }.into_any();
                }
                data.projects.into_iter().map(|row| {
                    let href = format!("/projects/{}", row.id);
                    let command = change(&row.id, row.revision, "restore", json!({"confirmed":true}));
                    let label = format!("Menu project {}", row.name);
                    view! { <div class="project-navigation__header">
                        <a href=href class="project-navigation__name">{row.name.clone()}</a>
                        <Menu label=label disabled=move || commands.journal.locked() items=vec![
                            MenuItem::new("Restore project", UiAction::new(move |()| {
                                confirm(commands.confirmation, format!("Restore project {}?", row.name),
                                    "The project returns to the sidebar.", "Restore project", false, UiAction::new({
                                        let command = command.clone();
                                        move |()| commands.execute(command.clone(), None)
                                    }));
                            }))
                        ] />
                    </div> }
                }).collect_view().into_any()
            })}
        </details>
    }
}
