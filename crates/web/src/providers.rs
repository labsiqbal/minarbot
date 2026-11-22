use crate::{api, connection_ui::*};
use leptos::{prelude::*, task::spawn_local};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

const PATH: &str = "/api/provider-accounts";
const TYPES: [(&str, &str, &str); 3] = [
    (
        "openai-compatible",
        "OpenAI-compatible",
        "https://api.openai.com/v1",
    ),
    ("anthropic", "Anthropic", "https://api.anthropic.com/v1"),
    (
        "gemini",
        "Gemini",
        "https://generativelanguage.googleapis.com/v1beta",
    ),
];

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Account {
    id: String,
    name: String,
    host: String,
    configured: bool,
    default_model: String,
    provider: String,
    source: String,
    #[serde(default, rename = "baseURL")]
    base_url: String,
    #[serde(default)]
    revision: u64,
}

#[derive(Deserialize, PartialEq)]
struct Models {
    models: Vec<String>,
}

pub fn panel() -> impl IntoView {
    let accounts = query::<Vec<Account>>(|| PATH.into());
    let editing = RwSignal::new(None::<Option<Account>>);
    let confirmation = Confirm::new(None);
    let removing = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let add = UiAction::new(move |()| {
        error.set(String::new());
        editing.set(Some(None));
    });
    let close = UiAction::new(move |()| {
        editing.set(None);
        focus_add_if_needed();
    });
    let visible = move || {
        accounts.data.with(|rows| {
            rows.as_ref().map(|rows| {
                rows.iter()
                    .filter(|row| row.configured || row.source == "settings")
                    .cloned()
                    .collect::<Vec<_>>()
            })
        })
    };
    view! {
        <div class="k-page bs-root">
            <Header title="Providers".to_owned()
                actions=Box::new(move || view! {
                    <Button label="Add provider" variant="primary" icon="plus" click=add
                        disabled=move || accounts.data.with(Option::is_none) />
                }.into_any()) />
            <ErrorNotice error=error.into() />
            <ErrorNotice error=accounts.error.into() retry=accounts.reload />
            <Show when=move || accounts.data.with(Option::is_none) && accounts.error.with(String::is_empty)>
                <Loading label="Loading providers…" />
            </Show>
            <Show when=move || visible().is_some_and(|rows| rows.is_empty())><Empty title="No providers" icon="server" /></Show>
            <For each=move || visible().unwrap_or_default() key=|row| (row.id.clone(), row.revision)
                children=move |row| {
                    let title = row.name.clone();
                    let host = if row.host.is_empty() { "Not configured".into() } else { row.host.clone() };
                    let configured = row.configured;
                    let account = row.clone();
                    let actions = if row.source == "settings" {
                        let edit = row.clone();
                        let label = format!("Actions for {}", row.name);
                        let remove = UiAction::new(move |()| {
                            let account = row.id.clone();
                            confirm(confirmation, "Remove provider?", row.name.clone(), "Remove", true, UiAction::new(move |()| {
                                if removing.get_untracked() { return; }
                                let path = format!("{PATH}/{}/delete", encode(&account));
                                let account = account.clone();
                                removing.set(true);
                                error.set(String::new());
                                spawn_local(async move {
                                    match api::command::<Value>(&path, json!({})).await {
                                        Ok(_) => {
                                            accounts.data.try_update(|rows| {
                                                if let Some(rows) = rows { rows.retain(|row| row.id != account); }
                                            });
                                            request_animation_frame(focus_add_if_needed);
                                        }
                                        Err(message) => { error.try_set(message); }
                                    }
                                    removing.try_set(false);
                                });
                            }));
                        });
                        let edit = UiAction::new(move |()| { error.set(String::new()); editing.set(Some(Some(edit.clone()))); });
                        view! { <Menu label=label disabled=move || removing.get() || accounts.busy.get()
                            items=vec![MenuItem::new("Edit", edit), MenuItem::new("Remove", remove).danger()] /> }.into_any()
                    } else {
                        view! { <Badge>"From server environment"</Badge> }.into_any()
                    };
                    view! {
                        <Section title=title actions=Box::new(move || actions)>
                            <div class="k-card">
                                <Row title="Host" trailing=Box::new(move || view! {
                                    <span class="provider-fact">{host}</span>
                                    <Badge success=configured>{if configured { "Configured" } else { "Not configured" }}</Badge>
                                }.into_any()) />
                                {models(account)}
                            </div>
                        </Section>
                    }
                } />
            {move || editing.get().map(|account| editor(account, accounts, close))}
            <ConfirmDialog state=confirmation />
        </div>
    }
}

fn focus_add_if_needed() {
    if document()
        .active_element()
        .is_none_or(|element| element.tag_name() == "BODY")
    {
        if let Some(button) = document()
            .query_selector(".app-settings__page .k-page-head__actions button")
            .ok()
            .flatten()
        {
            use wasm_bindgen::JsCast;
            if let Ok(button) = button.dyn_into::<web_sys::HtmlElement>() {
                let _ = button.focus();
            }
        }
    }
}

fn models(account: Account) -> AnyView {
    let model = account.default_model;
    let default_model = if model.is_empty() {
        "None".into()
    } else {
        model.clone()
    };
    let section = account.configured.then(|| {
        let id = encode(&account.id);
        let models = query::<Models>(move || format!("/api/models?account={id}"));
        let search = RwSignal::new(String::new());
        let account_name = StoredValue::new(account.name);
        let model = StoredValue::new(model);
        view! {
            <div class="provider-models">
                <div class="provider-models__head"><span>"Models"</span>
                    {move || models.data.with(|data| data.as_ref().map(|data| {
                        let count = data.models.len();
                        view! { <Badge>{count}</Badge> }
                    }))}
                </div>
                <input class="k-input" type="search" aria-label=account_name.with_value(|name| format!("Search models for {name}"))
                    placeholder="Search models" on:input=move |event| search.set(event_target_value(&event)) />
                <ErrorNotice error=models.error.into() retry=models.reload />
                <Show when=move || models.data.with(Option::is_none) && models.error.with(String::is_empty)>
                    <p role="status">"Loading models..."</p>
                </Show>
                <Show when=move || models.data.with(Option::is_some)>
                    <div class="provider-models__scroll" role="region" aria-label=account_name.with_value(|name| format!("Models for {name}")) tabindex="0">
                        {move || {
                            let query = search.with(|query| query.trim().to_lowercase());
                            let mut groups = BTreeMap::<String, Vec<String>>::new();
                            models.data.with(|data| {
                                if let Some(data) = data {
                                    for model in data.models.iter().collect::<BTreeSet<_>>() {
                                        if model.to_lowercase().contains(&query) {
                                            let prefix = model.split_once('/').map_or_else(|| "Other".into(), |(prefix, _)| format!("{prefix}/"));
                                            groups.entry(prefix).or_default().push(model.clone());
                                        }
                                    }
                                }
                            });
                            if groups.is_empty() {
                                return view! { <p role="status">{if query.is_empty() { "No models" } else { "No matching models" }}</p> }.into_any();
                            }
                            groups.into_iter().map(|(prefix, names)| {
                                let count = names.len();
                                view! {
                                    <Disclosure title=prefix count=count open=true>
                                        <ul class="provider-models__list">
                                            {names.into_iter().map(|name| {
                                                let default = model.with_value(|model| model == &name);
                                                view! { <li><span class="provider-model-id">{name}</span>
                                                    {default.then(|| view! { <Badge>"Default"</Badge> })}
                                                </li> }
                                            }).collect_view()}
                                        </ul>
                                    </Disclosure>
                                }
                            }).collect_view().into_any()
                        }}
                    </div>
                </Show>
            </div>
        }
    });
    view! {
        <Row title="Default model" trailing=Box::new(move || view! {
            <span class="provider-fact provider-model-id">{default_model}</span>
        }.into_any()) />
        {section}
    }
    .into_any()
}

fn editor(
    account: Option<Account>,
    accounts: Query<Vec<Account>>,
    close: UiAction,
) -> impl IntoView {
    let edit = account.is_some();
    let initial_type = account
        .as_ref()
        .map_or_else(|| TYPES[0].0.into(), |row| row.provider.clone());
    let base = RwSignal::new(
        account
            .as_ref()
            .map_or_else(|| TYPES[0].2.into(), |row| row.base_url.clone()),
    );
    let provider = RwSignal::new(initial_type);
    let name = account.as_ref().map(|row| row.name.clone());
    let model_label = account.as_ref().map_or_else(
        || "Test to load models".into(),
        |row| row.default_model.clone(),
    );
    let selected = RwSignal::new(
        account
            .as_ref()
            .map_or_else(String::new, |row| row.default_model.clone()),
    );
    let models = RwSignal::new(None::<Vec<String>>);
    let busy = RwSignal::new(None::<&'static str>);
    let error = RwSignal::new(String::new());
    let id = StoredValue::new(uuid());
    let revision = account.as_ref().map(|row| row.revision);
    let path = StoredValue::new(
        account.map_or_else(|| PATH.into(), |row| format!("{PATH}/{}", encode(&row.id))),
    );
    let changed = move || {
        models.set(None);
        error.set(String::new());
    };
    let can_save = move || {
        models.with(|models| {
            models
                .as_ref()
                .is_some_and(|models| models.contains(&selected.get()))
        })
    };
    let submit = move |event: web_sys::SubmitEvent| {
        let save = event
            .submitter()
            .and_then(|node| node.get_attribute("value"))
            .as_deref()
            == Some("save");
        let Some(fields) = form(&event) else { return };
        if busy.with_untracked(Option::is_some) || save && !can_save() {
            return;
        }
        let key = field(&fields, "apiKey");
        let mut data = json!({ "id": id.get_value(), "provider": provider.get_untracked(), "baseURL": base.get_untracked(), "name": field(&fields, "name"), "apiKey": key });
        let mut endpoint = path.get_value();
        if save {
            data["defaultModel"] = json!(selected.get_untracked());
            if let Some(revision) = revision {
                data["revision"] = json!(revision);
                endpoint.push_str("/edit");
            }
        } else {
            endpoint.push_str("/test");
            models.set(None);
        }
        busy.set(Some(if save { "save" } else { "test" }));
        error.set(String::new());
        spawn_local(async move {
            let result = if save {
                api::post::<Account>(&endpoint, &data).await.map(|row| {
                    accounts.data.try_update(|rows| {
                        let rows = rows.get_or_insert_with(Vec::new);
                        if let Some(old) = rows.iter_mut().find(|old| old.id == row.id) {
                            *old = row;
                        } else {
                            rows.push(row);
                        }
                    });
                })
            } else {
                api::post::<Models>(&endpoint, &data).await.map(|result| {
                    if let Some(current) = selected.try_get_untracked() {
                        if !result.models.contains(&current) {
                            selected.set(result.models.first().cloned().unwrap_or_default());
                        }
                        models.set(Some(result.models));
                    }
                })
            };
            if let Err(message) = result {
                error.try_set(redact(message, &key));
            }
            let alive = busy.try_get_untracked().is_some();
            busy.try_set(None);
            if save && alive && error.try_get_untracked().as_deref() == Some("") {
                close.run(());
            }
        });
    };
    view! {
        <Modal title=if edit { "Edit provider".to_owned() } else { "Add provider".to_owned() }
            busy=move || busy.with(Option::is_some) close=close footer=Box::new(move || view! {
                <Button label="Test" submit=true form="provider-form" value="test"
                    disabled=move || busy.with(Option::is_some) busy=move || busy.get() == Some("test") />
                <Button label="Save" submit=true form="provider-form" value="save" variant="primary"
                    disabled=move || busy.with(Option::is_some) || !can_save() busy=move || busy.get() == Some("save") />
            }.into_any())>
            <form id="provider-form" class="k-stack" on:submit=submit>
                <Field label="Type" name="provider-type">
                    <Select name="provider-type" value=provider.into() disabled=move || busy.with(Option::is_some)
                        options=Signal::derive(move || TYPES.into_iter().map(|(id,label,_)| (id.to_owned(), label.to_owned())).collect())
                        change=Callback::new(move |value: String| {
                            if let Some((_, _, url)) = TYPES.iter().find(|(key, _, _)| *key == value) {
                                provider.set(value); base.set((*url).into()); changed();
                            }
                        }) />
                </Field>
                <Field label="Name" name="provider-name">
                    <input class="k-input" id="provider-name" name="name" required maxlength="80" value=name readonly=move || busy.with(Option::is_some)
                        on:input=move |_| error.set(String::new()) />
                </Field>
                <Field label="Base URL" name="provider-url">
                    <input class="k-input" id="provider-url" name="baseURL" type="url" required maxlength="2048" spellcheck="false"
                        autocapitalize="none" prop:value=base readonly=move || busy.with(Option::is_some)
                        on:input=move |event| { base.set(event_target_value(&event)); changed(); } />
                </Field>
                <Field label="API key" name="provider-key" hint={if edit { "Blank keeps the saved key." } else { "" }}>
                    <input class="k-input" id="provider-key" name="apiKey" type="password" required=!edit maxlength="8192"
                        autocomplete="new-password" readonly=move || busy.with(Option::is_some) aria-describedby=edit.then_some("provider-key-hint")
                        on:input=move |_| changed() />
                </Field>
                <Field label="Default model" name="provider-model">
                    <Select name="provider-model" value=selected.into() disabled=move || busy.with(Option::is_some) || models.with(|models| models.as_ref().is_none_or(Vec::is_empty))
                        options=Signal::derive(move || models.with(|models| {
                            models.as_ref().filter(|models| !models.is_empty()).map_or_else(
                                || vec![(selected.get_untracked(), model_label.clone())],
                                |models| models.iter().map(|model| (model.clone(), model.clone())).collect()
                            )
                        })) change=Callback::new(move |value| selected.set(value)) />
                </Field>
                {move || models.with(|models| models.as_ref().map(|models| {
                    let status = if models.is_empty() { "No models found".into() } else { format!("{} models found", models.len()) };
                    view! { <span role="status" class="k-field__hint">{status}</span> }
                }))}
                <ErrorNotice error=error.into() />
            </form>
        </Modal>
    }
}
