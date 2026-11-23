use crate::{
    api,
    connection_ui::*,
    settings_recovery::{Command, Journal, Recovery, UUID},
    skill_import::{self, Importer},
};
use leptos::{prelude::*, task::spawn_local};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;

const SKILLS: &str = "/api/skills";
const PACKS: &str = "/api/skill-packs";

#[derive(Clone, Default, Deserialize, PartialEq)]
struct Asset {
    id: String,
    name: String,
    revision: u64,
    #[serde(default)]
    latest: u64,
    #[serde(default)]
    description: String,
    #[serde(default)]
    content: String,
    files: Option<Value>,
    #[serde(default)]
    members: Vec<Member>,
}
#[derive(Clone, Deserialize, PartialEq, Serialize)]
struct Member {
    skill: String,
    version: u64,
    #[serde(skip_serializing)]
    name: String,
    #[serde(skip_serializing)]
    description: String,
    #[serde(skip_serializing)]
    content: String,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Page {
    #[serde(alias = "skills", alias = "packs")]
    rows: Vec<Asset>,
    next_after: Option<String>,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Installed {
    skill: String,
    name: String,
    version: u64,
    enabled: u8,
    revision: u64,
    latest: u64,
    pack: Option<String>,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Installs {
    items: Vec<Installed>,
    next_after: Option<String>,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Binding {
    revision: u64,
    version: u64,
    enabled: u8,
    removed: u8,
    pack: Option<String>,
}

fn u53(value: &Value, min: u64) -> bool {
    value
        .as_u64()
        .is_some_and(|value| (min..(1 << 53)).contains(&value))
}
fn text_field(value: &Value, max: usize) -> bool {
    value
        .as_str()
        .is_some_and(|value| !value.trim().is_empty() && value.encode_utf16().count() <= max)
}

fn valid(command: &Command, pack: bool) -> bool {
    let kind = if pack { "skill-packs" } else { "skills" };
    let fields = if command.path.starts_with("/api/agents/") {
        if !u53(&command.body["version"], 1)
            || !command.body["enabled"].is_boolean()
            || command
                .body
                .get("remove")
                .is_some_and(|remove| remove != true || command.body["enabled"] != false)
        {
            return false;
        }
        &["revision", "version", "enabled", "remove"][..]
    } else if pack {
        let mut ids = BTreeSet::new();
        if !text_field(&command.body["name"], 80)
            || !command.body["members"].as_array().is_some_and(|members| {
                (1..=64).contains(&members.len())
                    && members.iter().all(|member| {
                        let Some(id) = member["skill"].as_str() else {
                            return false;
                        };
                        ids.insert(id.to_lowercase())
                            && js_sys::RegExp::new(&format!("^{UUID}$"), "i").test(id)
                            && u53(&member["version"], 1)
                            && member.as_object().is_some_and(|member| member.len() == 2)
                    })
            })
        {
            return false;
        }
        &["revision", "name", "members"][..]
    } else if let Some(files) = command.body.get("files") {
        if crate::skill_files::parse_files(files).is_err() {
            return false;
        }
        &["revision", "files"][..]
    } else {
        if !text_field(&command.body["name"], 80)
            || !text_field(&command.body["description"], 1024)
            || !text_field(&command.body["content"], 16000)
        {
            return false;
        }
        &["revision", "name", "description", "content"][..]
    };
    u53(&command.body["revision"], 0)
        && command.valid(
            &format!("^/api/({kind}(/{UUID})?|agents/{UUID}/{kind}/{UUID})$"),
            fields,
        )
}

#[derive(Clone, Copy)]
struct Library {
    pack: bool,
    path: &'static str,
    page: Query<Page>,
    cursor: Cursor,
    agent: RwSignal<String>,
    installed: Query<Installs>,
    install_cursor: Cursor,
    editor: RwSignal<Option<Asset>>,
    baseline: RwSignal<Option<Asset>>,
    version: RwSignal<String>,
    binding: Query<Option<Binding>>,
    working: RwSignal<bool>,
    import_busy: RwSignal<bool>,
    error: RwSignal<String>,
    confirm: Confirm,
    journal: Journal,
}

impl Library {
    fn new(pack: bool, agent: RwSignal<String>) -> Self {
        let cursor = Cursor::new(Vec::new());
        let install_cursor = Cursor::new(Vec::new());
        let editor = RwSignal::new(None::<Asset>);
        let path = if pack { PACKS } else { SKILLS };
        let state = Self {
            pack,
            path,
            editor,
            cursor,
            install_cursor,
            agent,
            page: query(move || format!("{path}{}", cursor_query(cursor))),
            installed: query(move || {
                let id = agent.get_untracked();
                if pack || id.is_empty() {
                    String::new()
                } else {
                    format!(
                        "/api/agents/{}/skills{}",
                        encode(&id),
                        cursor_query(install_cursor)
                    )
                }
            }),
            binding: query(move || {
                let id = agent.get_untracked();
                let skill = editor.with_untracked(|row| {
                    row.as_ref().map(|row| row.id.clone()).unwrap_or_default()
                });
                if id.is_empty() || skill.is_empty() {
                    String::new()
                } else {
                    format!(
                        "/api/agents/{}/{}/{}",
                        encode(&id),
                        if pack { "skill-packs" } else { "skills" },
                        encode(&skill)
                    )
                }
            }),
            baseline: RwSignal::new(None),
            version: RwSignal::new("1".into()),
            working: RwSignal::new(false),
            import_busy: RwSignal::new(false),
            error: RwSignal::new(String::new()),
            confirm: Confirm::new(None),
            journal: if pack {
                Journal::new("minarbot:pack-command", 4 * 1024 * 1024, |command| {
                    valid(command, true)
                })
            } else {
                Journal::new("minarbot:skill-command", 4 * 1024 * 1024, |command| {
                    valid(command, false)
                })
            },
        };
        let editor_id =
            Memo::new(move |_| editor.with(|row| row.as_ref().map(|row| row.id.clone())));
        Effect::new(move |_| {
            let _ = agent.get();
            state.install_cursor.set(Vec::new());
            state.installed.data.set(None);
            state.installed.reload.run(());
        });
        Effect::new(move |_| {
            let _ = agent.get();
            let _ = editor_id.get();
            state.binding.data.set(None);
            state.binding.reload.run(());
        });
        state.page.poll(15000, move || !state.locked());
        state.installed.poll(15000, move || !state.locked());
        state
    }
    fn locked(self) -> bool {
        self.working.get() || self.import_busy.get() || self.journal.locked()
    }
    fn refresh(self) {
        self.page.reload.run(());
        self.installed.reload.run(());
        self.binding.reload.run(());
    }
    fn allow_replace(self, next: UiAction) {
        if self.editor.with_untracked(Option::is_some)
            && self.editor.get_untracked() != self.baseline.get_untracked()
        {
            confirm(
                self.confirm,
                if self.pack {
                    "Discard unsaved pack changes?"
                } else {
                    "Discard skill changes?"
                },
                "Unsaved changes will be lost.",
                "Discard changes",
                true,
                next,
            );
        } else {
            next.run(());
        }
    }
    fn open(self, id: String, version: Option<u64>) {
        if self.locked() {
            return;
        }
        self.allow_replace(UiAction::new(move |()| {
            let path = format!(
                "{}/{}{}",
                self.path,
                encode(&id),
                version.map_or_else(String::new, |version| format!("?version={version}"))
            );
            self.working.set(true);
            self.error.set(String::new());
            spawn_local(async move {
                let result = api::read::<Asset>(&path).await;
                if self.working.try_set(false).is_some() {
                    return;
                }
                match result {
                    Ok(row) => {
                        self.version.set(row.revision.to_string());
                        self.baseline.set(Some(row.clone()));
                        self.editor.set(Some(row));
                    }
                    Err(message) => self.error.set(message),
                }
            });
        }));
    }
    fn create(self) {
        self.allow_replace(UiAction::new(move |()| {
            let row = Asset::default();
            self.editor.set(Some(row.clone()));
            self.baseline.set(self.pack.then_some(row));
            self.version.set("1".into());
            self.error.set(String::new());
        }));
    }
    fn import(self, parsed: Value) {
        self.allow_replace(UiAction::new(move |()| {
            let mut row = self.editor.get_untracked().unwrap_or_default();
            row.name = parsed["name"].as_str().unwrap_or_default().into();
            row.description = parsed["description"].as_str().unwrap_or_default().into();
            row.content = parsed["content"].as_str().unwrap_or_default().into();
            row.files = parsed.get("files").cloned();
            self.version.set(row.revision.max(1).to_string());
            self.editor.set(Some(row));
        }));
    }
    fn execute(self, command: Command) {
        let installing = command.path.starts_with("/api/agents/");
        self.error.set(String::new());
        self.journal.run(command, None, move |result| {
            if result.is_none() {
                return;
            }
            if !installing {
                self.editor.set(None);
                request_animation_frame(crate::settings::focus_heading);
            }
            self.refresh();
        });
    }
    fn save(self) {
        let Some(row) = self.editor.get_untracked() else {
            return;
        };
        if self.locked() {
            return;
        }
        let path = if row.id.is_empty() {
            self.path.into()
        } else {
            format!("{}/{}", self.path, encode(&row.id))
        };
        let body = if self.pack {
            json!({ "revision": row.latest, "name": row.name, "members": row.members })
        } else if let Some(files) = row.files {
            json!({ "revision": row.latest, "files": files })
        } else {
            json!({ "revision": row.latest, "name": row.name, "description": row.description, "content": row.content })
        };
        self.execute(Command::new(path, body));
    }
    fn bind(self, enabled: bool, remove: bool) {
        let Some(row) = self.editor.get_untracked().filter(|row| !row.id.is_empty()) else {
            return;
        };
        let Some(binding) = self.binding.data.get_untracked() else {
            return;
        };
        if self.locked()
            || self.binding.busy.get_untracked()
            || !self.pack
                && binding
                    .as_ref()
                    .is_some_and(|binding| binding.pack.is_some() && binding.removed == 0)
        {
            return;
        }
        let agent = self.agent.get_untracked();
        let name = agent_options(false)
            .into_iter()
            .find(|(id, _)| id == &agent)
            .map_or_else(String::new, |(_, name)| name);
        let version = if enabled {
            row.revision
        } else {
            binding.as_ref().map_or(0, |row| row.version)
        };
        if version == 0 || agent.is_empty() {
            return;
        }
        let kind = if self.pack { "skill-packs" } else { "skills" };
        let path = format!("/api/agents/{}/{kind}/{}", encode(&agent), encode(&row.id));
        let revision = binding.map_or(0, |binding| binding.revision);
        let apply = UiAction::new(move |()| {
            let mut body = json!({ "revision": revision, "version": version, "enabled": enabled });
            if remove {
                body["remove"] = json!(true);
            }
            self.execute(Command::new(path.clone(), body));
        });
        if self.pack && remove {
            confirm(
                self.confirm,
                "Remove the pack and all skills it manages?",
                "The library remains available.",
                "Remove pack",
                true,
                apply,
            );
        } else if self.pack {
            confirm(
                self.confirm,
                format!("Apply pack version {version} to this agent?"),
                "All members change together; unsaved drafts are excluded.",
                "Apply pack",
                false,
                apply,
            );
        } else {
            confirm(self.confirm, format!("Install version {version} on {name}?"),
                "Unsaved text changes are excluded from installation. Running executions stay unchanged.", "Install", false, apply);
        }
    }
    fn error(self) -> Signal<String> {
        Signal::derive(move || {
            [
                self.error.get(),
                self.journal.error.get(),
                self.page.error.get(),
                self.installed.error.get(),
                self.binding.error.get(),
            ]
            .into_iter()
            .find(|message| !message.is_empty())
            .unwrap_or_default()
        })
    }
    fn retry(self) -> UiAction {
        UiAction::new(move |()| {
            if let Some(command) = self.journal.pending.get_untracked() {
                self.execute(command);
            }
        })
    }
    fn refresh_action(self) -> UiAction {
        UiAction::new(move |()| self.refresh())
    }
}

pub fn panel() -> impl IntoView {
    let agent = agent_selection(false);
    let state = Library::new(false, agent);
    let tab = RwSignal::new("library");
    let error = state.error();
    let start = UiAction::new(move |()| state.create());
    view! {
        <div class="k-page bs-root bs-set3-skills">
            <Header title="Skills".to_owned() actions=Box::new(move || view! {
                <Show when=move || tab.get() != "packs">
                    <Importer busy=state.import_busy error=state.error disabled=move || state.locked()
                        result=Callback::new(move |value| state.import(value)) />
                    <Button label="Create skill" variant="primary" icon="plus" click=start disabled=move || state.locked() />
                </Show>
            }.into_any()) />
            <ErrorNotice error=error />
            <Recovery journal=state.journal retry=state.retry() confirmation=state.confirm refresh=state.refresh_action() />
            <div class="k-tabs">
                <div class="k-tabs__list" role="tablist" aria-label="Skills" on:keydown=move |event| crate::settings::move_focus(&event, "[role=tablist]", true)>
                    {[( "library", "Library"), ("installed", "Installed"), ("packs", "Packs")].into_iter().map(|(id, label)| {
                        let active = move || tab.get() == id;
                        view! { <button type="button" role="tab" class="k-tabs__tab" id=format!("skill-tab-{id}") aria-controls=format!("skill-panel-{id}")
                            aria-selected=move || active().to_string() tabindex=move || if active() { 0 } else { -1 }
                            data-state=move || if active() { "active" } else { "inactive" }
                            disabled=move || state.working.get() || state.import_busy.get() || id == "packs" && state.locked()
                            on:click=move |_| tab.set(id)>{label}</button> }
                    }).collect_view()}
                </div>
                <Show when=move || tab.get() == "library">
                    <div class="k-tabs__panel k-stack" id="skill-panel-library" role="tabpanel" tabindex="-1" aria-labelledby="skill-tab-library">{asset_list(state)}</div>
                </Show>
                <Show when=move || tab.get() == "installed">
                    <div class="k-tabs__panel" id="skill-panel-installed" role="tabpanel" tabindex="-1" aria-labelledby="skill-tab-installed">{installed(state)}</div>
                </Show>
                <Show when=move || tab.get() == "packs">
                    <div class="k-tabs__panel" id="skill-panel-packs" role="tabpanel" tabindex="-1" aria-labelledby="skill-tab-packs">{move || packs(agent)}</div>
                </Show>
            </div>
            {dialogs(state)}
        </div>
    }
}

fn asset_list(state: Library) -> impl IntoView {
    let (empty, icon, label, back, next) = if state.pack {
        (
            "No packs",
            "package",
            "Loading packs...",
            "Previous packs",
            "Next packs",
        )
    } else {
        (
            "No skills",
            "file",
            "Loading skills...",
            "Skill previous",
            "Skill next",
        )
    };
    let reload = UiAction::new(move |()| {
        state.page.reload.run(());
        request_animation_frame(crate::settings::focus_heading);
    });
    view! {
        <ErrorNotice error=state.page.error.into() retry=reload label="Try again" />
        <Show when=move || state.page.data.with(Option::is_none) && state.page.error.with(String::is_empty)><Loading label=label /></Show>
        <Show when=move || state.page.data.with(|page| page.as_ref().is_some_and(|page| page.rows.is_empty()))><Empty title=empty icon=icon /></Show>
        <Show when=move || state.page.data.with(|page| page.as_ref().is_some_and(|page| !page.rows.is_empty()))>
            <div class="k-card"><For each=move || state.page.data.get().map_or_else(Vec::new, |page| page.rows)
                key=|row| (row.id.clone(),row.revision)
                children=move |row| {
                    let id = StoredValue::new(row.id);
                    let open = UiAction::new(move |()| state.open(id.get_value(), None));
                    let meta = format!("v{}", row.revision);
                    let title = row.name;
                    if state.pack { view! { <Row title=title icon=icon click=open disabled=move || state.locked()
                        meta=Box::new(move || view! { <Badge>{meta}</Badge> }.into_any()) /> }.into_any() }
                    else { view! { <Row title=title.clone() icon=icon click=open disabled=move || state.locked()
                        meta=Box::new(move || meta.into_any()) trailing=Box::new(move || view! {
                            <Menu label=format!("Menu {title}") disabled=move || state.locked() items=vec![MenuItem::new("Edit skill", open)] />
                        }.into_any()) /> }.into_any() }
                } />
            </div>
        </Show>
        <Pager cursor=state.cursor next=Signal::derive(move || state.page.data.get().and_then(|page| page.next_after))
            previous_label=back next_label=next refresh=reload disabled=move || state.locked() />
    }
}

fn agent_select(state: Library, id: &'static str, label: &'static str) -> impl IntoView {
    view! { <Select name=id small=true label=label value=state.agent.into() options=Signal::derive(|| agent_options(false))
    disabled=move || state.locked() change=Callback::new(move |value| state.agent.set(value)) /> }
}

fn installed(state: Library) -> impl IntoView {
    let reload = UiAction::new(move |()| state.installed.reload.run(()));
    view! {
        <Section title="Agent" actions=Box::new(move || view! {
            <Show when=move || !agent_options(false).is_empty()>{agent_select(state, "installed-agent", "Agent")}</Show>
        }.into_any())>
            <Show when=move || state.agent.with(String::is_empty) fallback=move || view! {
                <ErrorNotice error=state.installed.error.into() retry=reload label="Try again" />
                {move || match state.installed.data.get() {
                    None if state.installed.error.with(String::is_empty) => view! { <Loading count=2 label="Loading installed skills..." /> }.into_any(),
                    None => ().into_any(),
                    Some(data) if data.items.is_empty() => view! { <Empty title="No installed skills" icon="book" /> }.into_any(),
                    Some(data) => view! { <div class="k-card">{data.items.into_iter().map(|row| installation_row(state, row)).collect_view()}</div> }.into_any(),
                }}
                <Pager cursor=state.install_cursor next=Signal::derive(move || state.installed.data.get().and_then(|data| data.next_after))
                    previous_label="Installation previous" next_label="Installation next" refresh=reload disabled=move || state.locked() />
            }><Empty title="No agents yet" icon="book" /></Show>
        </Section>
    }
}

fn installation_row(state: Library, row: Installed) -> impl IntoView {
    let id = StoredValue::new(row.skill);
    let open = UiAction::new(move |()| state.open(id.get_value(), Some(row.version)));
    let enabled = row.enabled != 0;
    let packed = row.pack.is_some();
    let mutate = move |remove: bool| {
        let mut body = json!({ "revision": row.revision, "enabled": !remove && !enabled, "version": row.version });
        if remove {
            body["remove"] = json!(true);
        }
        state.execute(Command::new(
            format!(
                "/api/agents/{}/skills/{}",
                encode(&state.agent.get_untracked()),
                encode(&id.get_value())
            ),
            body,
        ));
    };
    let name = StoredValue::new(row.name);
    let remove = UiAction::new(move |()| {
        let owner = state.agent.get_untracked();
        let agent = agent_options(false)
            .into_iter()
            .find(|(id, _)| id == &owner)
            .map_or_else(String::new, |(_, name)| name);
        confirm(
            state.confirm,
            format!("Remove {} from {agent}?", name.get_value()),
            "Library revisions remain available. Executions already running stay unchanged.",
            "Remove",
            true,
            UiAction::new(move |()| mutate(true)),
        );
    });
    view! {
        <Row title=name.get_value() icon="book" click=open disabled=move || state.locked()
            meta=Box::new(move || view! {
                <span>{format!("Version {}", row.version)}</span>
                {(row.latest > row.version).then(|| view! { <span class="k-badge is-warn">{format!("Version {} available", row.latest)}</span> })}
                {packed.then(|| view! { <Badge>"Pack"</Badge> })}
                {(!enabled).then(|| view! { <Badge>"Inactive"</Badge> })}
            }.into_any()) trailing=Box::new(move || view! {
                <Switch label=format!("{} {}", if enabled { "Disable" } else { "Enable" }, name.get_value())
                    checked=enabled disabled=move || state.locked() || packed change=Callback::new(move |_| mutate(false)) />
                <Menu label=format!("Menu {}", name.get_value()) disabled=move || state.locked() items=vec![
                    MenuItem::new(format!("Review version {}", row.version), open),
                    MenuItem { label: if packed { "Managed by Skill packs" } else { "Remove from agent" }.into(),
                        action: remove, danger: !packed, disabled: packed.into() },
                ] />
            }.into_any()) />
    }
}

fn packs(agent: RwSignal<String>) -> impl IntoView {
    let state = Library::new(true, agent);
    view! {
        <div class="k-page bs-root">
            <div class="k-row-actions pack-create"><Button label="Create pack" variant="primary" icon="plus"
                disabled=move || state.locked() click=UiAction::new(move |()| state.create()) /></div>
            <ErrorNotice error=state.error() />
            <Recovery journal=state.journal retry=state.retry() confirmation=state.confirm refresh=state.refresh_action() />
            <div class="k-stack">{asset_list(state)}</div>
            {dialogs(state)}
        </div>
    }
}

fn dialogs(state: Library) -> impl IntoView {
    let key = Memo::new(move |_| {
        state.editor.with(|row| {
            row.as_ref()
                .map(|row| (row.id.clone(), row.revision, row.files.is_some()))
        })
    });
    view! {
        {move || key.get().and_then(|_| state.editor.get_untracked()).map(|asset| editor(state, asset))}
        <ConfirmDialog state=state.confirm />
    }
}

fn editor(state: Library, asset: Asset) -> impl IntoView {
    let title = if !asset.id.is_empty() {
        asset.name.clone()
    } else if state.pack {
        "New pack".into()
    } else if asset.files.is_some() {
        format!("Import {}", asset.name)
    } else {
        "New skill".into()
    };
    let existing = !asset.id.is_empty();
    let files = asset.files.is_some();
    let revision = asset.revision;
    let latest = asset.latest;
    let id = StoredValue::new(asset.id);
    let close = UiAction::new(move |()| {
        state.allow_replace(UiAction::new(move |()| state.editor.set(None)))
    });
    let error = state.error();
    let description = existing.then(|| format!("v{revision} of {latest}"));
    let text = Signal::derive(move || {
        state
            .editor
            .with(|row| row.as_ref().map(|row| row.name.clone()).unwrap_or_default())
    });
    let form = if state.pack {
        "pack-editor"
    } else {
        "skill-editor"
    };
    view! {
        <Modal title=title description=description.unwrap_or_default() size="lg" close=close
            busy=move || state.working.get() || state.journal.busy.get() footer=Box::new(move || view! {
                {(existing && !state.pack).then(|| view! { <span class="k-dialog__foot-note">
                    <Importer compact=true busy=state.import_busy error=state.error disabled=move || state.locked()
                        result=Callback::new(move |value| state.import(value)) />
                </span> })}
                <Button label="Close" disabled=move || state.working.get() || state.journal.busy.get() click=close />
                <Button label="Save version" variant="primary" submit=true form=form busy=state.journal.busy
                    disabled=move || state.locked() || state.pack && state.editor.with(|row| row.as_ref().is_none_or(|row| row.members.is_empty())) />
            }.into_any())>
            <form id=form class="k-stack" on:submit=move |event: web_sys::SubmitEvent| { event.prevent_default(); state.save(); }>
                <ErrorNotice error=error />
                <Recovery journal=state.journal retry=state.retry() confirmation=state.confirm refresh=state.refresh_action() />
                <fieldset class="k-fieldset" disabled=move || state.locked()>
                    <Field label="Name" name="skill-name">
                        <input class="k-input" id="skill-name" name="name" required maxlength="80" readonly=files prop:value=text
                            placeholder=(!state.pack).then_some("For example: meeting-summary")
                            on:input=move |event| state.editor.update(|row| if let Some(row) = row { row.name = event_target_value(&event); }) />
                    </Field>
                    {if state.pack { pack_members(state).into_any() } else { skill_text(state, files).into_any() }}
                </fieldset>
                {files.then_some(move || state.editor.with(|row| row.as_ref().and_then(|row| row.files.clone())).map(|files| file_list(state, files)))}
                {existing.then(|| view! {
                    <Section title="Version">
                        <div class="k-version">
                            {version_picker(state, latest, id)}
                            {(!state.pack).then(|| view! {
                                <div class="k-version__install">
                                    {agent_select(state, "skill-install-agent", "Install on agent")}
                                    {move || state.binding.data.with(|row| row.as_ref().and_then(Option::as_ref).filter(|row| row.removed != 0).map(|_| view! { <Badge>"Removed"</Badge> }))}
                                    <Button label=format!("Install v{revision}") variant="brand" click=UiAction::new(move |()| state.bind(true,false))
                                        disabled=move || state.locked() || state.agent.with(String::is_empty) || state.binding.data.with(|row|
                                            row.as_ref().is_none_or(|row| row.as_ref().is_some_and(|binding| binding.pack.is_some() && binding.removed == 0))) />
                                </div>
                            })}
                        </div>
                    </Section>
                })}
            </form>
            {state.pack.then(|| pack_binding(state, existing, asset.name, revision))}
        </Modal>
    }
}

fn version_picker(state: Library, latest: u64, id: StoredValue<String>) -> impl IntoView {
    let version = move || {
        state.version.with(|version| {
            version
                .parse::<u64>()
                .ok()
                .filter(|version| (1..=latest).contains(version))
        })
    };
    view! { <Field label="Version" name="skill-version"><div class="k-inline">
        <input class="k-input skill-version" id="skill-version" name="version" type="number" min="1" max=latest step="1"
            prop:value=state.version on:input=move |event| state.version.set(event_target_value(&event)) />
        <Button label="Load" disabled=move || state.locked() || version().is_none()
            click=UiAction::new(move |()| { if let Some(version) = version() { state.open(id.get_value(), Some(version)); } }) />
    </div></Field> }
}

fn skill_text(state: Library, files: bool) -> impl IntoView {
    let description = Signal::derive(move || {
        state.editor.with(|row| {
            row.as_ref()
                .map(|row| row.description.clone())
                .unwrap_or_default()
        })
    });
    let content = Signal::derive(move || {
        state.editor.with(|row| {
            row.as_ref()
                .map(|row| row.content.clone())
                .unwrap_or_default()
        })
    });
    view! {
        <Field label="When to use" name="skill-description">
            <textarea class="k-input k-textarea skill-description" id="skill-description" name="description" required maxlength="1024" rows="2"
                readonly=files prop:value=description on:input=move |event| state.editor.update(|row| if let Some(row) = row { row.description = event_target_value(&event); })></textarea>
        </Field>
        <Field label="Instructions" name="skill-content">
            <textarea class="k-input k-textarea skill-instructions" id="skill-content" name="content" required maxlength="16000" rows="10"
                readonly=files prop:value=content on:input=move |event| state.editor.update(|row| if let Some(row) = row { row.content = event_target_value(&event); })></textarea>
        </Field>
    }
}

fn file_list(state: Library, files: Value) -> impl IntoView {
    let files = StoredValue::new(files);
    let download = move |zip| {
        if let Err(message) = files.with_value(|files| skill_import::download(files, zip)) {
            state.error.set(message);
        }
    };
    view! {
        <Section title=files.with_value(|files| format!("{} files from folder", files.as_array().map_or(0, Vec::len))) actions=Box::new(move || view! {
            <Button label="Export JSON" size="sm" variant="ghost" click=UiAction::new(move |()| download(false)) />
            <Button label="Export ZIP" size="sm" variant="ghost" click=UiAction::new(move |()| download(true)) />
        }.into_any())>
            <div class="k-card k-file-list">{files.with_value(|files| files.as_array().into_iter().flatten().map(|file| {
                let path = file["path"].as_str().unwrap_or_default().to_owned();
                let content = if file["encoding"] == "base64" { view! { <p class="k-file__note">"Binary asset. Export ZIP to get the original file."</p> }.into_any() }
                    else { view! { <pre class="k-file__content" tabindex="0" role="region" aria-label=format!("Contents of {path}")>{file["content"].as_str().unwrap_or_default().to_owned()}</pre> }.into_any() };
                view! { <div class="k-file"><Disclosure title=path>{content}</Disclosure></div> }
            }).collect_view())}</div>
        </Section>
    }
}

fn pack_members(state: Library) -> impl IntoView {
    let cursor = Cursor::new(Vec::new());
    let library = query::<Page>(move || format!("{SKILLS}{}", cursor_query(cursor)));
    let chosen = RwSignal::new(String::new());
    let version = RwSignal::new("1".to_owned());
    let members = Signal::derive(move || {
        state.editor.with(|row| {
            row.as_ref()
                .map_or_else(Vec::new, |row| row.members.clone())
        })
    });
    let refresh = UiAction::new(move |()| {
        chosen.set(String::new());
        library.reload.run(());
    });
    let add = UiAction::new(move |()| {
        let id = chosen.get_untracked();
        let Some(version) = version.with_untracked(|version| {
            version
                .parse::<u64>()
                .ok()
                .filter(|version| (1..(1 << 53)).contains(version))
        }) else {
            return;
        };
        if id.is_empty() || members.with_untracked(|members| members.len() >= 64) || state.locked()
        {
            return;
        }
        let path = format!("{SKILLS}/{}?version={version}", encode(&id));
        state.working.set(true);
        state.error.set(String::new());
        spawn_local(async move {
            let result = api::read::<Asset>(&path).await;
            if state.working.try_set(false).is_some() {
                return;
            }
            match result {
                Ok(skill) => state.editor.update(|row| {
                    if let Some(row) = row {
                        row.members.retain(|row| row.skill != id);
                        row.members.push(Member {
                            skill: skill.id,
                            version: skill.revision,
                            name: skill.name,
                            description: skill.description,
                            content: skill.content,
                        });
                    }
                }),
                Err(message) => state.error.set(message),
            }
        });
    });
    view! {
        <Section title="Skills">
            <Show when=move || members.with(Vec::is_empty) fallback=move || view! {
                <div class="k-card">
                    <For each=move || members.get() key=|row| (row.skill.clone(), row.version) children=move |row| {
                        let id = StoredValue::new(row.skill);
                        let name = row.name.clone();
                        let remove = UiAction::new(move |()| state.editor.update(|row| {
                            if let Some(row) = row { row.members.retain(|row| id.with_value(|id| id != &row.skill)); }
                        }));
                        view! { <Row title=row.name.clone()
                            meta=Box::new(move || view! { <Badge>{format!("Version {}", row.version)}</Badge> }.into_any())
                            trailing=Box::new(move || view! { <Menu label=format!("Menu {name}") disabled=move || state.locked()
                                items=vec![MenuItem::new("Remove from draft", remove).danger()] /> }.into_any())>
                            <Disclosure title=format!("Review instructions for {}", row.name)>
                                <p class="k-field__hint">{row.description}</p><Code label=format!("Instructions for {}", row.name) text=row.content />
                            </Disclosure>
                        </Row> }
                    } />
                </div>
            }><Empty title="No skills" icon="file" /></Show>
        </Section>
        <ErrorNotice error=library.error.into() retry=library.reload />
        <div class="k-grid-2">
            <Field label="Skill from library" name="pack-skill">
                <Select name="pack-skill" value=chosen.into() disabled=move || state.locked()
                    options=Signal::derive(move || {
                        let rows = library.data.get().map_or_else(Vec::new, |data| data.rows);
                        std::iter::once((String::new(), "Choose skill".into())).chain(rows.into_iter().map(|row| (row.id, row.name))).collect()
                    }) change=Callback::new(move |value: String| {
                        let current = library.data.with_untracked(|data| data.as_ref().and_then(|data| data.rows.iter().find(|row| row.id == value)).map_or(1, |row| row.revision));
                        chosen.set(value);
                        version.set(current.to_string());
                    }) />
            </Field>
            <Field label="Skill version" name="pack-skill-version"><input class="k-input" id="pack-skill-version" name="version" type="number"
                min="1" step="1" prop:value=version on:input=move |event| version.set(event_target_value(&event)) /></Field>
        </div>
        <div class="k-inline">
            <Button label="Add skill" size="sm" click=add disabled=move || state.locked() || chosen.with(String::is_empty) || members.with(|rows| rows.len() >= 64) />
            <Pager cursor=cursor next=Signal::derive(move || library.data.get().and_then(|data| data.next_after))
                previous_label="Previous library page" next_label="Next library page" refresh=refresh disabled=move || state.locked() />
        </div>
    }
}

fn pack_binding(state: Library, existing: bool, name: String, version: u64) -> impl IntoView {
    let name = StoredValue::new(name);
    view! {
        <Section title="Agent" actions=Box::new(move || agent_select(state, "pack-agent", "Agent").into_any())>
            {if existing { view! {
                <div class="k-card">
                    {move || {
                        let binding = state.binding.data.get();
                        let active = binding.as_ref().and_then(|row| row.as_ref()).filter(|row| row.removed == 0)
                            .map(|row| (row.version, row.enabled != 0));
                        let disabled = move || state.locked() || state.agent.with(String::is_empty) || state.binding.busy.get() || state.binding.data.with(Option::is_none);
                        let details = if let Some((version, enabled)) = active {
                            view! { <Badge>{format!("Installed version {version}")}</Badge>
                                <Badge>{if enabled { "Active" } else { "Inactive" }}</Badge> }.into_any()
                        } else if binding.as_ref().is_none_or(Option::is_none) {
                            view! { <span role="status">"Loading installation..."</span> }.into_any()
                        } else { "Not installed".into_any() };
                        let enabled = active.is_some_and(|(_, enabled)| enabled);
                        let show = active.is_some();
                        let mut items = vec![MenuItem { label: format!("Install v{version}"), action: UiAction::new(move |()| state.bind(true, false)), disabled: Signal::derive(disabled), danger: false }];
                        if show { items.push(MenuItem::new("Remove entire pack", UiAction::new(move |()| state.bind(false, true))).danger()); }
                        view! {
                            <Row title=name.get_value() meta=Box::new(move || details) trailing=Box::new(move || view! {
                                {show.then(|| view! { <Switch label="Disable entire pack" checked=enabled disabled=move || disabled() || !enabled
                                    change=Callback::new(move |_| state.bind(false, false)) /> })}
                                <Menu label="Pack installation actions" disabled=move || state.locked() items=items />
                            }.into_any()) />
                        }
                    }}
                </div>
            }.into_any() } else { view! { <div><Badge>"Save before installing"</Badge></div> }.into_any() }}
        </Section>
    }
}
