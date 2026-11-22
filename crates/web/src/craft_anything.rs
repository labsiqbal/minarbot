use crate::{api, connection_ui::*};
use leptos::{prelude::*, task::spawn_local};
use serde::Deserialize;
use serde_json::{json, Value};

const PATH: &str = "/api/settings/craft-anything";

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Status {
    version: String,
    installed: bool,
    available: bool,
    computer_ready: bool,
    agents: Vec<String>,
    deep_projects: Vec<String>,
}

#[derive(Clone, Deserialize, PartialEq)]
struct Projects {
    projects: Vec<Project>,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Project {
    id: String,
    name: String,
    archived: bool,
}

pub fn panel() -> impl IntoView {
    let status = query::<Status>(|| PATH.into());
    let projects = query::<Projects>(|| "/api/projects".into());
    let pending = RwSignal::new("");
    let failure = RwSignal::new(String::new());
    let confirmation = Confirm::new(None);
    let locked = move || !pending.get().is_empty() || status.busy.get();
    let save = move |action: &'static str, body: Value| {
        if locked() {
            return;
        }
        pending.set(action);
        failure.set(String::new());
        spawn_local(async move {
            match api::post::<Status>(&format!("{PATH}/{action}"), &body).await {
                Ok(result) => {
                    status.data.try_set(Some(result));
                }
                Err(message) => {
                    failure.try_set(message);
                }
            }
            pending.try_set("");
        });
    };
    view! {
        <div class="k-page bs-root">
            <Header title="Craft Anything".to_owned() />
            <Show when=move || !status.error.with(String::is_empty)><Alert actions=Box::new(move || view! {
                <Button label="Reload" size="sm" click=status.reload />
            }.into_any())>"Craft Anything status could not be loaded."</Alert></Show>
            <ErrorNotice error=failure.into() />
            <Show when=move || status.data.with(Option::is_none) && status.error.with(String::is_empty)><Loading count=2 /></Show>
            {move || status.data.get().map(|data| {
                let installed = data.installed;
                let available = data.available;
                let action = if installed { "remove" } else { "install" };
                let computer_url = crate::settings::linked_section_url("/settings/computer");
                let vault_url = crate::settings::linked_section_url("/settings/vault");
                view! {
                    <div class="k-card">
                        <Row title=format!("Version {}", data.version) meta=Box::new(|| view! {
                            <a href="https://github.com/storytold/craft-anything" target="_blank" rel="noreferrer">"Third-party source · MIT or Apache-2.0"</a>
                        }.into_any()) trailing=Box::new(move || view! {
                            <span role="status"><Badge success=installed>{if installed { "Installed" } else { "Not installed" }}</Badge></span>
                            <Button label=if installed { "Remove" } else { "Install" } size="sm"
                                variant=if installed { "secondary" } else { "primary" }
                                disabled=move || locked() || (!installed && !available) busy=move || pending.get() == action
                                click=UiAction::new(move |()| save(action, json!({}))) />
                        }.into_any()) />
                    </div>
                    {(!available).then(|| view! { <Alert tone="info">"Install requires Linux with Bubblewrap, GPG and tar."</Alert> })}
                    {(available && !data.computer_ready).then(|| view! { <Alert tone="info"><a href=computer_url>"Set up Computer"</a>" to use Craft Anything."</Alert> })}
                    {installed.then(|| {
                        let agents = StoredValue::new(data.agents);
                        let deep = StoredValue::new(data.deep_projects);
                        view! {
                            <div class="k-card"><Row title="fal.ai key" meta=Box::new(|| view! { <code>"craft-anything-fal"</code> }.into_any())
                                trailing=Box::new(move || view! { <a href=vault_url>"Manage in Vault"</a> }.into_any()) /></div>
                            <Section title="Agent access">
                                {move || {
                                    let rows = local_agents().into_iter().filter(|agent| !agent.hidden).map(|agent| {
                                        let id = StoredValue::new(agent.id);
                                        view! { <Row title=agent.name.clone() trailing=Box::new(move || view! {
                                            <Switch label=format!("Craft Anything access for {}", agent.name) disabled=locked
                                                checked=agents.with_value(|agents| id.with_value(|id| agents.contains(id)))
                                                change=Callback::new(move |enabled| save("grant", json!({ "agent": id.get_value(), "enabled": enabled }))) />
                                        }.into_any()) /> }
                                    }).collect_view();
                                    if rows.is_empty() { view! { <p class="empty">"Create an agent to grant access."</p> }.into_any() }
                                    else { view! { <div class="k-card">{rows}</div> }.into_any() }
                                }}
                            </Section>
                            <Section title="Deep mode">
                                <ErrorNotice error=projects.error.into() retry=projects.reload />
                                {move || projects.data.get().map(|data| {
                                    let rows = data.projects.into_iter().filter(|project| !project.archived).map(|project| {
                                        let enabled = deep.with_value(|deep| deep.contains(&project.id));
                                        let title = project.name.clone();
                                        let project = StoredValue::new(project);
                                        view! { <Row title=title.clone() meta=Box::new(move || if enabled { "Deep mode allowed" } else { "Clean room" }.into_any())
                                            trailing=Box::new(move || view! {
                                                <Switch label=format!("Deep mode for {title}") checked=enabled disabled=locked
                                                    change=Callback::new(move |enabled| {
                                                        let apply = UiAction::new(move |()| save("deep", json!({ "project": project.with_value(|row| row.id.clone()), "confirmedOwnership": enabled })));
                                                        if enabled {
                                                            confirm(confirmation, project.with_value(|row| format!("Allow Deep mode for {}?", row.name)),
                                                                "Deep mode can inspect source and decompile files. Continue only if you own or have permission to inspect the targets in this project.",
                                                                "Confirm permission", false, apply);
                                                        } else { apply.run(()); }
                                                    }) />
                                            }.into_any()) /> }
                                    }).collect_view();
                                    if rows.is_empty() { view! { <p class="empty">"Create a project to confirm Deep mode access."</p> }.into_any() }
                                    else { view! { <div class="k-card">{rows}</div> }.into_any() }
                                })}
                            </Section>
                        }
                    })}
                }
            })}
            <ConfirmDialog state=confirmation />
        </div>
    }
}
