use crate::{
    api,
    connection_ui::{Switch, UiAction},
    general::ErrorAlert,
    provider_icons::ProviderMark,
    provider_instances::AddProvider,
    provider_sign_in::{SignIn, Target},
    shell, workspace,
};
use leptos::{prelude::*, task::spawn_local};
use serde::Deserialize;
use serde_json::{json, Value};

const RECOVERY: &str = "minarbot-agent-creation";
const INSTANCES: &str = "/api/provider-instances";

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderInstance {
    id: String,
    display_name: String,
    kind: String,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    installed: bool,
    #[serde(default)]
    auth: Auth,
    #[cfg_attr(not(feature = "workspace"), allow(dead_code))]
    #[serde(default)]
    models: Vec<Model>,
    #[serde(default)]
    revision: u64,
}

#[derive(Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Auth {
    status: String,
    #[serde(default)]
    method: Option<String>,
    #[cfg_attr(not(feature = "workspace"), allow(dead_code))]
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Model {
    id: String,
    #[serde(default)]
    options: Vec<ModelOption>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelOption {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    values: Vec<String>,
}

fn option_label(id: &str) -> String {
    match id {
        "reasoningEffort" => "Reasoning effort".into(),
        "serviceTier" => "Service tier".into(),
        "fastMode" => "Fast mode".into(),
        _ => id
            .chars()
            .enumerate()
            .fold(String::new(), |mut label, (index, ch)| {
                if index > 0 && ch.is_uppercase() {
                    label.push(' ');
                }
                if index == 0 {
                    label.extend(ch.to_uppercase());
                } else {
                    label.extend(ch.to_lowercase());
                }
                label
            }),
    }
}

fn value_label(value: &str) -> String {
    option_label(value)
}

#[derive(Deserialize)]
struct ProviderInstances {
    providers: Vec<ProviderInstance>,
}

#[cfg_attr(not(feature = "workspace"), allow(dead_code))]
#[derive(Clone, PartialEq)]
pub(crate) struct Connection {
    pub(crate) value: String,
    pub(crate) label: String,
    pub(crate) kind: String,
    pub(crate) model: String,
    pub(crate) ready: bool,
    pub(crate) reason: String,
}

#[cfg_attr(not(feature = "workspace"), allow(dead_code))]
fn ready(instance: &ProviderInstance) -> bool {
    instance.enabled
        && instance.installed
        && (instance.kind != "codex" && instance.kind != "claude"
            || instance.auth.status == "authenticated")
}

#[cfg_attr(not(feature = "workspace"), allow(dead_code))]
pub(crate) async fn connections() -> Result<(Vec<Connection>, Option<String>), String> {
    let response = api::read::<ProviderInstances>("/api/provider-instances").await?;
    let entries = response
        .providers
        .into_iter()
        .map(|instance| {
            let is_ready = ready(&instance);
            let reason = if !instance.enabled {
                "Disabled".into()
            } else if !instance.installed {
                "Provider is not installed".into()
            } else if instance.auth.status != "authenticated"
                && (instance.kind == "codex" || instance.kind == "claude")
            {
                instance
                    .auth
                    .reason
                    .unwrap_or_else(|| "Sign in required".into())
            } else {
                String::new()
            };
            Connection {
                value: format!("instance:{}", instance.id),
                label: instance.display_name,
                kind: instance.kind,
                model: instance
                    .models
                    .first()
                    .map(|model| model.id.clone())
                    .unwrap_or_else(|| "default".into()),
                ready: is_ready,
                reason,
            }
        })
        .collect::<Vec<_>>();
    let automatic = entries
        .iter()
        .find(|entry| entry.ready)
        .map(|entry| entry.value.clone());
    Ok((entries, automatic))
}

fn request_id() -> Result<String, String> {
    window()
        .crypto()
        .map(|crypto| crypto.random_uuid())
        .map_err(|_| "Could not create a request. Reload and try again.".into())
}

#[derive(Deserialize)]
struct Models {
    models: Vec<String>,
    #[serde(default)]
    catalog: Vec<Model>,
}

fn recovery() -> Result<Option<Value>, String> {
    let storage = window()
        .session_storage()
        .ok()
        .flatten()
        .ok_or("Recovery storage is unavailable.")?;
    let Some(raw) = storage
        .get_item(RECOVERY)
        .map_err(|_| "Recovery data could not be read.")?
    else {
        return Ok(None);
    };
    let value = (raw.len() <= 64_000)
        .then(|| serde_json::from_str::<Value>(&raw).ok())
        .flatten();
    let value = value
        .filter(|value| {
            value.is_object()
                && value["id"]
                    .as_str()
                    .is_some_and(crate::workspace_route::valid_id)
                && value["name"].is_string()
                && value["model"].is_string()
        })
        .ok_or("Recovery data could not be read.")?;
    Ok(Some(value))
}

/// What setup can do with a provider.
#[derive(Clone, Copy, PartialEq)]
enum State {
    Ready,
    SignIn,
    Disabled,
    Missing,
}

/// A provider row in setup: an instance, or Codex or Claude found on this
/// machine (`detected`) that gets its instance when the user picks it.
#[derive(Clone, PartialEq)]
struct Choice {
    id: String,
    kind: String,
    label: String,
    revision: u64,
    state: State,
    meta: String,
    detected: bool,
}

fn choice(instance: ProviderInstance, detected: bool) -> Choice {
    let signs_in = matches!(instance.kind.as_str(), "codex" | "claude");
    let (state, meta) = if !instance.installed {
        (State::Missing, "Not installed".to_owned())
    } else if !instance.enabled {
        (State::Disabled, "Off".to_owned())
    } else if signs_in && instance.auth.status != "authenticated" {
        (State::SignIn, "Not signed in".to_owned())
    } else if signs_in {
        // Claude has one sign-in method; Codex shows ChatGPT or its API provider.
        let method = instance.auth.method.filter(|method| method != "Claude");
        (
            State::Ready,
            method.map_or_else(
                || "Signed in".to_owned(),
                |method| format!("Signed in · {method}"),
            ),
        )
    } else {
        (State::Ready, "Ready".to_owned())
    };
    Choice {
        id: instance.id,
        kind: instance.kind,
        label: instance.display_name,
        revision: instance.revision,
        state,
        meta,
        detected,
    }
}

async fn choices() -> Result<Vec<Choice>, String> {
    let instances = api::read::<ProviderInstances>(INSTANCES).await?;
    let detected = api::read::<ProviderInstances>(&format!("{INSTANCES}/detected"))
        .await
        .map(|list| list.providers)
        .unwrap_or_default();
    let mut rows = instances
        .providers
        .into_iter()
        .map(|instance| choice(instance, false))
        .chain(detected.into_iter().map(|instance| choice(instance, true)))
        .collect::<Vec<_>>();
    // Usable providers first; the sort is stable.
    rows.sort_by_key(|row| row.state != State::Ready);
    Ok(rows)
}

/// Creates the instance of a detected provider.
async fn create(row: &Choice) -> Result<(), String> {
    api::command::<Value>(
        INSTANCES,
        json!({"instanceId": row.id, "kind": row.kind, "displayName": row.label, "enabled": true}),
    )
    .await
    .map(|_| ())
}

pub fn page(error_icon: String) -> impl IntoView {
    let refresh_directory = use_context::<shell::DirectoryRefresh>();
    let icons = StoredValue::new(use_context::<shell::Icons>());
    let icon = move |pick: fn(&shell::Icons) -> &String| {
        icons.with_value(|value| {
            value
                .as_ref()
                .map_or_else(String::new, |icons| pick(icons).clone())
        })
    };
    let rows = RwSignal::new(Vec::<Choice>::new());
    let choice_id = RwSignal::new(String::new());
    let loaded = RwSignal::new(false);
    let loading = RwSignal::new(false);
    let load_error = RwSignal::new(String::new());
    let row_error = RwSignal::new(String::new());
    let preparing = RwSignal::new(String::new());
    let adding = RwSignal::new(false);
    let signing = RwSignal::new(None::<Target>);
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let pending = RwSignal::new(recovery());
    let name = RwSignal::new(pending.with_untracked(|value| {
        value
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .and_then(|value| value["name"].as_str())
            .unwrap_or_default()
            .to_owned()
    }));
    let model = RwSignal::new(String::new());
    let models = RwSignal::new(Vec::<String>::new());
    let models_loading = RwSignal::new(false);
    let model_catalog = RwSignal::new(Vec::<Model>::new());
    let model_options = RwSignal::new(json!({}));
    let selected = Memo::new(move |_| {
        rows.with(|rows| {
            rows.iter()
                .find(|row| row.id == choice_id.get() && row.state == State::Ready && !row.detected)
                .cloned()
        })
    });
    let has_ready =
        move || rows.with_untracked(|rows| rows.iter().any(|row| row.state == State::Ready));

    // Runs `then` once the row has an instance, creating it for a detected provider.
    let prepare = move |row: Choice, then: Box<dyn FnOnce() + 'static>| {
        row_error.set(String::new());
        if !row.detected {
            then();
            return;
        }
        if !preparing.get_untracked().is_empty() {
            return;
        }
        preparing.set(row.id.clone());
        spawn_local(async move {
            match create(&row).await {
                Ok(()) => {
                    rows.update(|rows| {
                        if let Some(entry) = rows.iter_mut().find(|entry| entry.id == row.id) {
                            entry.detected = false;
                        }
                    });
                    then();
                }
                Err(message) => row_error.set(message),
            }
            preparing.set(String::new());
        });
    };
    let pick = move |row: Choice| {
        let id = row.id.clone();
        prepare(row, Box::new(move || choice_id.set(id)));
    };
    let refresh = move || {
        if loading.get_untracked() {
            return;
        }
        loading.set(true);
        spawn_local(async move {
            match choices().await {
                Ok(list) => {
                    load_error.set(String::new());
                    let keep = list.iter().any(|row| {
                        row.id == choice_id.get_untracked() && row.state == State::Ready
                    });
                    let first = list.iter().find(|row| row.state == State::Ready).cloned();
                    rows.set(list);
                    loaded.set(true);
                    // Keep a usable choice, else take the first usable provider.
                    if !keep {
                        match first {
                            Some(row) => pick(row),
                            None => choice_id.set(String::new()),
                        }
                    }
                }
                Err(message) => load_error.set(message),
            }
            loading.set(false);
        });
    };
    refresh();
    // A provider can become usable outside setup (a terminal sign-in, Settings).
    crate::connection_ui::poll_with_backoff(
        5000,
        move || {
            (!loading.get_untracked()
                && signing.with_untracked(Option::is_none)
                && !adding.get_untracked()
                && !has_ready())
            .then(|| !load_error.with_untracked(String::is_empty))
        },
        refresh,
    );
    let sign_in = move |row: Choice| {
        let target = Target {
            id: row.id.clone(),
            kind: row.kind.clone(),
            name: row.label.clone(),
        };
        prepare(row, Box::new(move || signing.set(Some(target))));
    };
    let enable = move |row: Choice| {
        if !preparing.get_untracked().is_empty() {
            return;
        }
        preparing.set(row.id.clone());
        row_error.set(String::new());
        spawn_local(async move {
            let path = format!("{INSTANCES}/{}/update", row.id);
            if let Err(message) = api::command::<Value>(
                &path,
                json!({"instanceId": row.id, "revision": row.revision, "enabled": true}),
            )
            .await
            {
                row_error.set(message);
            } else {
                choice_id.set(row.id.clone());
            }
            preparing.set(String::new());
            refresh();
        });
    };
    let close_add = UiAction::new(move |()| {
        adding.set(false);
        refresh();
    });
    let reload = UiAction::new(move |()| refresh());
    let open_sign_in = Callback::new(move |target: Target| {
        adding.set(false);
        signing.set(Some(target));
    });

    let generation = RwSignal::new(0u64);
    Effect::new(move |_| {
        generation.update(|value| *value += 1);
        let request = generation.get_untracked();
        model.set(String::new());
        models.set(Vec::new());
        model_catalog.set(Vec::new());
        model_options.set(json!({}));
        let Some(row) = selected.get() else {
            models_loading.set(false);
            return;
        };
        models_loading.set(true);
        let url = format!("{INSTANCES}/{}/models", row.id);
        spawn_local(async move {
            let result = api::read::<Models>(&url).await;
            if generation.try_get_untracked() != Some(request) {
                return;
            }
            if let Ok(value) = result {
                if let Some(default) = value.models.first() {
                    model.set(default.clone());
                }
                models.set(value.models);
                model_catalog.set(value.catalog);
            }
            models_loading.set(false);
        });
    });
    let model_options_for_selection = Signal::derive(move || {
        model_catalog.with(|catalog| {
            catalog
                .iter()
                .find(|entry| entry.id == model.get())
                .map(|entry| entry.options.clone())
                .unwrap_or_default()
        })
    });
    let retry_creation = move || pending.with(|value| matches!(value, Ok(Some(_))));
    // A started creation keeps its form open, so a failure and its retry show.
    let create_step =
        move || retry_creation() || pending.with(Result::is_err) || selected.with(Option::is_some);
    let locked = move || busy.get() || pending.with(|value| !matches!(value, Ok(None)));
    let submit = move |event: web_sys::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() {
            return;
        }
        let command = pending.get_untracked().and_then(|value| {
            if let Some(value) = value { return Ok(value); }
            let row = selected.get_untracked().ok_or("Choose a provider.")?;
            let name = name.get_untracked().trim().to_owned();
            let model = model.get_untracked().trim().to_owned();
            if name.is_empty() || model.is_empty() { return Err("Enter a name and model.".into()); }
            Ok(json!({"id": request_id()?, "name": name, "role": "", "model": model, "instructions": "Help the user complete their tasks.", "threadLimit": 2, "toolBudget": 90, "providerInstance": row.id, "modelOptions": model_options.get_untracked()}))
        });
        let save = command.and_then(|command| {
            let storage = window()
                .session_storage()
                .ok()
                .flatten()
                .ok_or("Recovery storage is unavailable.")?;
            storage
                .set_item(RECOVERY, &command.to_string())
                .map_err(|_| "Recovery data could not be saved.")?;
            Ok(command)
        });
        let command = match save {
            Ok(command) => command,
            Err(message) => {
                error.set(message);
                return;
            }
        };
        pending.set(Ok(Some(command.clone())));
        busy.set(true);
        error.set(String::new());
        spawn_local(async move {
            match api::post::<shell::Agent>("/api/agents", &command).await {
                Ok(agent) => {
                    if let Ok(Some(storage)) = window().session_storage() {
                        let _ = storage.remove_item(RECOVERY);
                    }
                    if let Some(refresh) = refresh_directory {
                        refresh.0.run(());
                    }
                    crate::router::open(&format!("/agents/{}", agent.id));
                }
                Err(message) => {
                    error.set(message);
                    busy.set(false);
                }
            }
        });
    };
    let check = icon(|icons| &icons.check);
    view! {
        <main class="chat page-view bs-root"><div class="page-scroll"><div class="first-run">
            {workspace::heading("Set up Minarbot".into(), String::new())}
            <ErrorAlert message=load_error icon=error_icon.clone() />
            <button type="button" class="k-btn k-btn--sm k-btn--secondary" hidden=move || load_error.with(String::is_empty) on:click=move |_| refresh()>"Retry"</button>
            <ol class="first-run__steps" aria-label="Setup progress">
                <li class:is-active=move || selected.with(Option::is_none) class:is-done=move || selected.with(Option::is_some) aria-current=move || selected.with(Option::is_none).then_some("step")>
                    <div class="first-run__step"><span class="first-run__icon" aria-hidden="true" inner_html=move || if selected.with(Option::is_some) { icon(|icons| &icons.check) } else { icon(|icons| &icons.plug) }></span><strong>"Connect a provider"</strong></div>
                    <div class="first-run__form"><div class="k-stack">
                        <div class="k-card setup-providers" aria-hidden="true" hidden=move || loaded.get() || !load_error.with(String::is_empty)>
                            <div class="setup-provider setup-provider--loading"><span class="setup-provider__mark k-skeleton"></span><span class="setup-provider__text"><span class="k-skeleton" style:width="30%"></span><span class="k-skeleton is-small" style:width="45%"></span></span></div>
                        </div>
                        <div class="k-card setup-providers" role="radiogroup" aria-label="Provider" hidden=move || rows.with(Vec::is_empty)>
                            <For each=move || rows.get() key=|row| (row.id.clone(), row.revision, row.meta.clone(), row.detected) children=move |row| {
                                let id = row.id.clone();
                                let checked = { let id = id.clone(); move || choice_id.with(|choice| choice == &id) && row_ready(rows, &id) };
                                let is_checked = checked.clone();
                                let shown = checked.clone();
                                let working = { let id = id.clone(); move || preparing.with(|value| value == &id) };
                                let pick_row = row.clone();
                                let action_row = row.clone();
                                let check = check.clone();
                                view! {
                                    <div class="setup-provider" class:is-selected=checked.clone() class:is-unavailable=row.state != State::Ready>
                                        <button type="button" role="radio" class="setup-provider__pick" aria-label=row.label.clone() aria-checked=move || is_checked().to_string()
                                            disabled=row.state != State::Ready on:click=move |_| pick(pick_row.clone())>
                                            <span class="setup-provider__mark"><ProviderMark kind=row.kind.clone() /></span>
                                            <span class="setup-provider__text"><span class="setup-provider__name">{row.label.clone()}</span><span class="setup-provider__meta">{row.meta.clone()}</span></span>
                                            <span class="setup-provider__check" aria-hidden="true" hidden=move || !shown() inner_html=check></span>
                                        </button>
                                        {match row.state {
                                            State::SignIn => Some(view! { <button type="button" class="k-btn k-btn--sm k-btn--primary" disabled=move || working() || signing.with(Option::is_some) on:click=move |_| sign_in(action_row.clone())>"Sign in"</button> }.into_any()),
                                            State::Disabled => Some(view! { <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=working on:click=move |_| enable(action_row.clone())>"Turn on"</button> }.into_any()),
                                            _ => None,
                                        }}
                                    </div>
                                }
                            } />
                        </div>
                        <ErrorAlert message=row_error icon=error_icon.clone() />
                        <div class="k-row-actions">
                            <button type="button" class=move || if rows.with(Vec::is_empty) { "k-btn k-btn--md k-btn--primary" } else { "k-btn k-btn--md k-btn--secondary" } on:click=move |_| adding.set(true)>"Add provider"</button>
                        </div>
                    </div></div>
                </li>
                <li class:is-active=create_step aria-current=move || create_step().then_some("step")>
                    <div class="first-run__step"><span class="first-run__icon" aria-hidden="true" inner_html=icon(|icons| &icons.plus)></span><strong>"Create agent"</strong></div>
                    <div class="first-run__form" hidden=move || !create_step()><div class="k-stack">
                        <form class="k-stack" on:submit=submit>
                            <label class="k-field"><span class="k-field__label">"Name"</span><input class="k-input" required maxlength="80" placeholder="Iris" prop:value=name readonly=locked on:input=move |event| name.set(event_target_value(&event)) /></label>
                            <label class="k-field"><span class="k-field__label">"Model"</span>
                                <Show when=move || models.with(Vec::is_empty) && !models_loading.get() && !retry_creation()
                                    fallback=move || view! {
                                        <select class="k-input" aria-label="Model" disabled=move || locked() || models_loading.get() prop:value=move || pending.with(|value| value.as_ref().ok().and_then(Option::as_ref).and_then(|value| value["model"].as_str()).map(str::to_owned)).unwrap_or_else(|| model.get()) on:change=move |event| { model.set(event_target_value(&event)); model_options.set(json!({})); }>
                                            {move || if models_loading.get() { view! { <option value="">"Loading models…"</option> }.into_any() } else { ().into_any() }}
                                            {move || pending.with(|value| value.as_ref().ok().and_then(Option::as_ref).and_then(|value| value["model"].as_str()).map(str::to_owned)).filter(|saved| !models.with(|models| models.contains(saved))).map(|saved| view! { <option value=saved.clone()>{saved.clone()}</option> })}
                                            {move || models.get().into_iter().map(|entry| { let text = entry.clone(); view! { <option value=entry>{text}</option> } }).collect_view()}
                                        </select>
                                    }>
                                    <input class="k-input" aria-label="Model" required maxlength="160" placeholder="Model ID" readonly=locked prop:value=model on:input=move |event| model.set(event_target_value(&event)) />
                                </Show>
                            </label>
                            <Show when=move || !model_options_for_selection.with(Vec::is_empty)>
                                <div class="agent-model-options"><h3>Model options</h3>
                                    {move || model_options_for_selection.get().into_iter().map(|option| {
                                        if option.kind == "boolean" {
                                            let key = option.id.clone();
                                            let checked = Signal::derive({ let key = key.clone(); move || model_options.with(|value| value.get(&key).and_then(Value::as_bool).unwrap_or(false)) });
                                            let change_key = option.id.clone();
                                            let label = option_label(&option.id);
                                            view! { <div class="k-row"><div class="k-row__main"><span class="k-row__text"><span class="k-row__title">{label.clone()}</span></span></div><Switch label=label.clone() checked change=Callback::new(move |value| model_options.update(|options| options[&change_key] = json!(value))) /></div> }.into_any()
                                        } else {
                                            let values = option.values.clone();
                                            let change_key = option.id.clone();
                                            let value_key = change_key.clone();
                                            let label = option_label(&option.id);
                                            view! { <label class="k-field"><span class="k-field__label">{label.clone()}</span><select class="k-input" aria-label=label.clone() prop:value=move || model_options.with(|options| options.get(&value_key).and_then(Value::as_str).unwrap_or_default().to_owned()) on:change=move |event| model_options.update(|options| options[&change_key] = json!(event_target_value(&event)))>{values.into_iter().map(|value| { let text = value_label(&value); view! { <option value=value.clone()>{text}</option> } }).collect_view()}</select></label> }.into_any()
                                        }
                                    }).collect_view()}
                                </div>
                            </Show>
                            <ErrorAlert message=error icon=error_icon.clone() />
                            <p role="alert" hidden=move || pending.with(Result::is_ok)>"Recovery data could not be read."</p>
                            <div class="k-row-actions"><button type="submit" class="k-btn k-btn--md k-btn--primary" disabled=move || busy.get() || models_loading.get() || pending.with(Result::is_err)>{move || if retry_creation() { "Retry creation" } else { "Create agent" }}</button></div>
                        </form>
                    </div></div>
                </li>
                <li><div class="first-run__step"><span class="first-run__icon" aria-hidden="true" inner_html=icon(|icons| &icons.send)></span><strong>"Send first message"</strong></div></li>
            </ol>
            <Show when=move || adding.get()>
                <AddProvider close=close_add reload=reload saved=move || close_add.run(()) sign_in=open_sign_in />
            </Show>
            {move || signing.get().map(|target| {
                let id = target.id.clone();
                view! {
                    <SignIn target close=UiAction::new(move |()| { signing.set(None); refresh(); })
                        signed_in=UiAction::new(move |()| { signing.set(None); choice_id.set(id.clone()); refresh(); }) />
                }
            })}
        </div></div></main>
    }
}

fn row_ready(rows: RwSignal<Vec<Choice>>, id: &str) -> bool {
    rows.with(|rows| {
        rows.iter()
            .any(|row| row.id == id && row.state == State::Ready)
    })
}
