mod avatar;
#[cfg(not(feature = "hub"))]
mod form;
mod journal;
#[cfg(not(feature = "hub"))]
mod resources;
#[cfg(not(feature = "hub"))]
mod templates;

use crate::{
    api,
    connection_ui::{Button, UiAction},
    shell::{Agent, Directory},
    system::kit::*,
};
use journal::{fields, revision, Journal, Recovery};
use leptos::{prelude::*, task::spawn_local};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Clone, Deserialize, Default)]
#[serde(default)]
pub(crate) struct Profile {
    pub model: String,
    pub instructions: String,
    pub runtime: Option<String>,
    pub provider_account: String,
    pub provider_instance: String,
    pub model_options: Value,
    pub backup_model: String,
    pub goal_judge_model: Option<String>,
    pub memory_model: String,
    pub thread_limit: Option<u64>,
    pub tool_budget: Option<u64>,
    pub web_access: Option<u8>,
    pub learn_from_conversations: Option<u8>,
}

#[derive(Clone)]
pub(crate) enum DialogTarget {
    Create,
    Import,
    Edit(String),
    #[cfg(feature = "conversation")]
    Export(String),
}

#[derive(Clone, Copy)]
pub(crate) struct Lifecycle {
    target: RwSignal<Option<DialogTarget>>,
    directory: RwSignal<Option<Result<Directory, String>>>,
    pub error: RwSignal<String>,
    pub deleting: RwSignal<String>,
}

impl Lifecycle {
    pub fn new(directory: RwSignal<Option<Result<Directory, String>>>) -> Self {
        Self {
            target: RwSignal::new(None),
            directory,
            error: RwSignal::new(String::new()),
            deleting: RwSignal::new(String::new()),
        }
    }

    pub fn open(self, target: DialogTarget) {
        self.target.set(Some(target));
    }

    pub fn close(self) {
        self.target.set(None);
    }

    pub fn link(self, agent: &str) -> String {
        format!("/agents/{agent}")
    }

    fn start_chat(self, agent: &Value) {
        if let Some(id) = agent["id"]
            .as_str()
            .filter(|id| crate::workspace_route::valid_id(id))
        {
            let _ = window().location().assign(&self.link(id));
        }
    }

    fn current(self, id: &str) -> bool {
        let path = window().location().pathname().unwrap_or_default();
        let base = format!("/agents/{id}");
        path == base
            || path
                .strip_prefix(&base)
                .is_some_and(|suffix| suffix.starts_with('/'))
    }

    pub fn get(self, id: &str) -> Option<Agent> {
        self.directory
            .with_untracked(|data| find_agent(data.as_ref(), id))
    }

    fn live(self, id: &str) -> Option<Agent> {
        self.directory.with(|data| find_agent(data.as_ref(), id))
    }

    #[cfg(feature = "conversation")]
    pub fn deleting_current(self) -> bool {
        self.deleting.with(|id| !id.is_empty() && self.current(id))
    }

    fn refresh(self, id: &str, reload_conversation: bool) {
        if reload_conversation && self.current(id) {
            let _ = window().location().reload();
            return;
        }
        spawn_local(async move {
            match api::read("/api/agents").await {
                Ok(data) => {
                    self.directory.try_set(Some(Ok(data)));
                }
                Err(message) => {
                    self.error.try_set(message);
                }
            }
        });
    }
}

fn find_agent(data: Option<&Result<Directory, String>>, id: &str) -> Option<Agent> {
    let directory = data?.as_ref().ok()?;
    directory
        .agents
        .iter()
        .chain(&directory.guide)
        .find(|agent| agent.id == id)
        .cloned()
}

#[cfg(not(feature = "hub"))]
#[leptos::wasm_split::wasm_split(agent_profile, wasm_split_path = leptos::wasm_split, fallible)]
fn profile_dialog(lifecycle: Lifecycle, target: DialogTarget) -> crate::lazy_view::ChunkView {
    Ok(match target {
        DialogTarget::Create => {
            let model = lifecycle
                .directory
                .with_untracked(|state| {
                    state
                        .as_ref()
                        .and_then(|state| state.as_ref().ok())
                        .map(|directory| directory.default_model.clone())
                })
                .unwrap_or_default();
            form::agent_form(lifecycle, None, model).into_any()
        }
        DialogTarget::Edit(id) => lifecycle
            .get(&id)
            .map(|agent| form::agent_form(lifecycle, Some(agent), String::new()))
            .into_any(),
        #[cfg(feature = "conversation")]
        DialogTarget::Export(id) => lifecycle
            .get(&id)
            .map(|agent| templates::export(agent, move || lifecycle.close(), true))
            .into_any(),
        DialogTarget::Import => templates::import(lifecycle).into_any(),
    })
}

#[cfg(not(feature = "hub"))]
pub(crate) fn mount(lifecycle: Lifecycle) -> impl IntoView {
    let target = move || {
        lifecycle.target.get().map(|target| {
            crate::lazy_view::load("agent dialog", move || {
                profile_dialog(lifecycle, target.clone())
            })
        })
    };
    let avatar_failure = RwSignal::new(window().session_storage().ok().flatten().is_some_and(
        |storage| {
            let key = "minarbot:avatar-save-failed";
            storage
                .get_item(key)
                .ok()
                .flatten()
                .is_some_and(|id| lifecycle.current(&id))
                && storage.remove_item(key).is_ok()
        },
    ));
    view! {
        {target}
        <Show when=move || avatar_failure.get()>
            <div class="k-avatar-toast-viewport"><div class="k-avatar-toast bs-root" role="status">
                <strong>"Agent created"</strong><span>"Avatar could not save. Open Edit profile to try again."</span>
                <Button label="Dismiss" size="sm" click=Callback::new(move |()| avatar_failure.set(false)) />
            </div></div>
        </Show>
    }
}

pub(crate) fn row_actions(lifecycle: Lifecycle, agent: Agent) -> impl IntoView {
    let builtin = agent.builtin;
    let id = StoredValue::new(agent.id);
    let name = StoredValue::new(agent.name);
    let confirm = use_context::<Confirm>();
    let visibility = if agent.hidden { "Restore" } else { "Hide" };
    let visibility_icon = if agent.hidden { "eye" } else { "eye-off" };
    let item_icons = if builtin {
        vec!["edit", visibility_icon]
    } else {
        vec!["edit", "inbox", visibility_icon, "trash"]
    };
    let busy = RwSignal::new(String::new());
    let version = agent.visibility_revision;
    let hidden = agent.hidden;
    let mut items = vec![MenuItem {
        label: if builtin { "AI connection" } else { "Edit" }.into(),
        danger: false,
        action: Callback::new(move |()| {
            if builtin {
                let _ = window().location().assign(&crate::settings::section_url(
                    "/settings/providers",
                    &lifecycle.link(&id.get_value()),
                ));
            } else {
                lifecycle.open(DialogTarget::Edit(id.get_value()));
            }
        }),
    }];
    if !builtin {
        items.push(MenuItem {
            label: "Inbox".into(),
            danger: false,
            action: Callback::new(move |()| {
                let _ = window()
                    .location()
                    .assign(&format!("/agents/{}/inbox", id.get_value()));
            }),
        });
    }
    items.push(MenuItem {
        label: visibility.into(),
        danger: false,
        action: Callback::new(move |()| change_visibility(lifecycle, id, version, !hidden, busy)),
    });
    if !builtin {
        items.push(MenuItem {
            label: "Delete".into(),
            danger: true,
            action: Callback::new(move |()| delete(lifecycle, confirm, id, name, lifecycle.error)),
        });
    }
    view! {
        <Menu name=format!("Actions for {}", name.get_value()) class="k-icon-btn agent-actions" separate_danger=true items item_icons>
            <Ellipsis />
        </Menu>
    }
}

#[component]
fn Ellipsis() -> impl IntoView {
    // Same Lucide ellipsis geometry as the existing workspace actions.
    view! { <svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
        <circle cx="12" cy="12" r="1"></circle><circle cx="19" cy="12" r="1"></circle><circle cx="5" cy="12" r="1"></circle>
    </svg> }
}

fn delete(
    lifecycle: Lifecycle,
    confirm: Option<Confirm>,
    id: StoredValue<String>,
    name: StoredValue<String>,
    error: RwSignal<String>,
) {
    ask(
        confirm,
        format!("Delete {}?", name.get_value()),
        "Permanently removes agent data and access.",
        "Delete agent",
        true,
        move || {
            if !lifecycle.deleting.with_untracked(String::is_empty) {
                return;
            }
            let id = id.get_value();
            lifecycle.deleting.set(id.clone());
            spawn_local(async move {
                match api::command::<Value>(&format!("/api/agents/{id}/delete"), json!({})).await {
                    Ok(_) => {
                        lifecycle.close();
                        if lifecycle.current(&id) {
                            let _ = window().location().replace("/");
                        } else {
                            lifecycle.deleting.set(String::new());
                            lifecycle.refresh(&id, false);
                        }
                    }
                    Err(message) => {
                        lifecycle.deleting.try_set(String::new());
                        error.try_set(message);
                    }
                }
            });
        },
    );
}

fn availability(
    lifecycle: Lifecycle,
    agent: Agent,
    open: Signal<bool>,
    close: impl Fn() + Copy + Send + Sync + 'static,
    reload_conversation: bool,
) -> impl IntoView {
    let id = StoredValue::new(agent.id);
    let name = StoredValue::new(agent.name);
    let hidden = RwSignal::new(agent.hidden);
    let version = RwSignal::new(agent.visibility_revision);
    let confirm = use_context::<Confirm>().unwrap_or_else(|| RwSignal::new(None));
    let journal = Journal::new(
        format!("minarbot-visibility:{}", id.get_value()),
        512,
        |value| {
            fields(value, &["id", "revision", "hidden"])
                && revision(&value["revision"], 0)
                && value["hidden"].is_boolean()
        },
    );
    let reload = UiAction::new(move |()| {
        spawn_local(async move {
            match api::read::<Directory>("/api/agents").await {
                Ok(data) => {
                    if let Some(agent) = data
                        .agents
                        .iter()
                        .chain(&data.guide)
                        .find(|agent| agent.id == id.get_value())
                    {
                        hidden.try_set(agent.hidden);
                        version.try_set(agent.visibility_revision);
                    }
                    lifecycle.directory.try_set(Some(Ok(data)));
                }
                Err(message) => {
                    journal.error.try_set(message);
                }
            }
        })
    });
    let save = move || {
        let next = !hidden.get_untracked();
        journal.run(
            json!({"revision": version.get_untracked(), "hidden": next}),
            |command| {
                (
                    format!("/api/agents/{}/visibility", id.get_value()),
                    command.clone(),
                )
            },
            false,
            move |value| {
                hidden.set(value["hidden"].as_bool().unwrap_or(next));
                version.set(
                    value["revision"]
                        .as_u64()
                        .unwrap_or_else(|| version.get_untracked() + 1),
                );
                lifecycle.refresh(&id.get_value(), reload_conversation);
            },
        );
    };
    let title = move || {
        if hidden.get() {
            "Restore agent"
        } else {
            "Hide agent"
        }
    };
    view! {
        <Dialog id="agent-availability" title=text("Availability") open on_close=close locked=journal.busy
            footer=content(move || view! { <Button label="Done" click=Callback::new(move |()| close()) /> })>
            <div class="k-stack">
                <Card><div class="k-row">
                    <div class="k-row__main"><span class="k-row__text"><span class="k-row__title">
                        {badge("neutral", move || if hidden.get() { "Hidden · queue paused" } else { "Available" })}
                    </span></span></div>
                    <span class="k-row__trailing">
                        <crate::system::kit::Button label=Signal::derive(move || title().to_owned()) size="sm" disabled=Signal::derive(move || journal.locked()) on_click=move || {
                            ask(Some(confirm), format!("{} {}?", if hidden.get_untracked() { "Restore" } else { "Hide" }, name.get_value()),
                                if hidden.get_untracked() { "The held queue will resume. Missed schedules will not run again." } else { "Running work can finish. The queue is held and new messages are rejected until the agent is restored." },
                                title(), !hidden.get_untracked(), save);
                        } />
                    </span>
                </div></Card>
                <Recovery journal confirm title="Not confirmed" retry=Callback::new(move |()| save()) refresh=reload />
            </div>
        </Dialog>
    }
}

pub(crate) fn hidden_agents(
    lifecycle: Lifecycle,
    directory: ReadSignal<Option<Result<Directory, String>>>,
    search: RwSignal<String>,
) -> impl IntoView {
    let open = RwSignal::new(false);
    let busy = RwSignal::new(String::new());
    let rows = Memo::new(move |_| {
        let search = search.get().trim().to_lowercase();
        directory.with(|data| {
            data.as_ref()
                .and_then(|data| data.as_ref().ok())
                .map_or_else(Vec::new, |data| {
                    data.agents
                        .iter()
                        .chain(&data.guide)
                        .filter(|agent| {
                            agent.hidden
                                && format!("{} {}", agent.name, agent.role)
                                    .to_lowercase()
                                    .contains(&search)
                        })
                        .map(|agent| {
                            (
                                agent.id.clone(),
                                agent.name.clone(),
                                agent.avatar_revision,
                                agent.visibility_revision,
                            )
                        })
                        .collect()
                })
        })
    });
    view! {
        <details class="machine-group hidden-agents" hidden=move || rows.with(Vec::is_empty)
          open=move || open.get() || !search.with(String::is_empty)
          on:toggle=move |event| {
            if search.with_untracked(String::is_empty) {
                use wasm_bindgen::JsCast;
                if let Some(node) = event.target().and_then(|node| node.dyn_into::<web_sys::HtmlDetailsElement>().ok()) { open.set(node.open()); }
            }
          }>
            <summary class="machine-group__header">
                <span class="machine-group__chevron"><crate::connection_ui::Icon name="chevron-right" /></span>
                <span class="machine-group__name">"Hidden"</span>
                <span class="machine-group__count">{move || rows.with(Vec::len)}</span>
            </summary>
            <div class="machine-group__content"><nav aria-label="Hidden agents">
                {move || rows.get().into_iter().map(|(id, name, avatar_revision, version)| {
                    let src = (avatar_revision != 0).then(|| format!("/api/agents/{id}/avatar/image?v={avatar_revision}"));
                    let initial = avatar::initial(&name);
                    let agent = StoredValue::new(id.clone());
                    view! {
                        <div class="hidden-agent-row">
                            <a class="bot-row" aria-label=format!("Open hidden agent {name}") href=lifecycle.link(&id)>
                                <span class="avatar bs-avatar agent-hidden-avatar" aria-hidden="true">{src.map_or_else(|| initial.into_any(), |src| view! { <img src=src alt="" /> }.into_any())}</span>
                                <span class="bot-row__text"><span class="bot-row__name">{name.clone()}</span></span>
                            </a>
                            <Button label="Restore" aria=format!("Restore {name}") variant="ghost" size="sm"
                              busy=move || busy.with(|value| value == &id) disabled=move || !busy.with(String::is_empty)
                              click=Callback::new(move |()| restore(lifecycle, agent, version, busy)) />
                        </div>
                    }
                }).collect_view()}</nav>
            </div>
        </details>
    }
}

pub(crate) fn restore(
    lifecycle: Lifecycle,
    agent: StoredValue<String>,
    version: u64,
    busy: RwSignal<String>,
) {
    change_visibility(lifecycle, agent, version, false, busy);
}

fn change_visibility(
    lifecycle: Lifecycle,
    agent: StoredValue<String>,
    version: u64,
    hidden: bool,
    busy: RwSignal<String>,
) {
    if !busy.with_untracked(String::is_empty) {
        return;
    }
    let id = agent.get_value();
    busy.set(id.clone());
    lifecycle.error.set(String::new());
    spawn_local(async move {
        let result = api::command::<Value>(
            &format!("/api/agents/{id}/visibility"),
            json!({"revision":version,"hidden":hidden}),
        )
        .await;
        if busy.try_set(String::new()).is_some() {
            return;
        }
        match result {
            Ok(_) => lifecycle.refresh(&id, true),
            Err(message) => lifecycle.error.set(message),
        }
    });
}
