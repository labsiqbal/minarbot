use crate::{api, connection_ui::Switch, general::ErrorAlert, shell, workspace};
use leptos::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};

const RECOVERY: &str = "minarbot-agent-creation";
const COMPUTER: &str = "/api/settings/computer";

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
    #[serde(default)]
    models: Vec<Model>,
}

#[derive(Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Auth {
    status: String,
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

#[derive(Deserialize)]
struct ProviderInstances {
    providers: Vec<ProviderInstance>,
}

#[derive(Clone, PartialEq)]
pub(crate) struct Connection {
    pub(crate) value: String,
    pub(crate) label: String,
    pub(crate) kind: String,
    pub(crate) model: String,
    pub(crate) ready: bool,
    pub(crate) reason: String,
}

fn ready(instance: &ProviderInstance) -> bool {
    instance.enabled
        && instance.installed
        && (instance.kind != "codex" && instance.kind != "claude"
            || instance.auth.status == "authenticated")
}

pub(crate) async fn connections() -> Result<(Vec<Connection>, Option<String>), String> {
    let response = api::read::<ProviderInstances>("/api/provider-instances").await?;
    let entries = response.providers.into_iter().map(|instance| {
        let is_ready = ready(&instance);
        let reason = if !instance.enabled {
            "Disabled".into()
        } else if !instance.installed {
            "Provider is not installed".into()
        } else if instance.auth.status != "authenticated" && (instance.kind == "codex" || instance.kind == "claude") {
            instance.auth.reason.unwrap_or_else(|| "Sign in required".into())
        } else {
            String::new()
        };
        Connection {
            value: format!("instance:{}", instance.id),
            label: instance.display_name,
            kind: instance.kind,
            model: instance.models.first().map(|model| model.id.clone()).unwrap_or_else(|| "default".into()),
            ready: is_ready,
            reason,
        }
    }).collect::<Vec<_>>();
    let automatic = entries.iter().find(|entry| entry.ready).map(|entry| entry.value.clone());
    Ok((entries, automatic))
}

fn request_id() -> Result<String, String> {
    window().crypto().map(|crypto| crypto.random_uuid()).map_err(|_| "Could not create a request. Reload and try again.".into())
}

#[derive(Deserialize)]
struct Models {
    models: Vec<String>,
    #[serde(default)]
    catalog: Vec<Model>,
}

fn recovery() -> Result<Option<Value>, String> {
    let storage = window().session_storage().ok().flatten().ok_or("Recovery storage is unavailable.")?;
    let Some(raw) = storage.get_item(RECOVERY).map_err(|_| "Recovery data could not be read.")? else { return Ok(None); };
    let value = (raw.len() <= 64_000).then(|| serde_json::from_str::<Value>(&raw).ok()).flatten();
    let value = value.filter(|value| value.is_object() && value["id"].as_str().is_some_and(crate::workspace_route::valid_id) && value["name"].is_string() && value["model"].is_string()).ok_or("Recovery data could not be read.")?;
    Ok(Some(value))
}

pub fn page(error_icon: String) -> impl IntoView {
    let icons = StoredValue::new(use_context::<shell::Icons>());
    let icon = move |pick: fn(&shell::Icons) -> &String| icons.with_value(|value| value.as_ref().map_or_else(String::new, |icons| pick(icons).clone()));
    let available = RwSignal::new(Vec::<Connection>::new());
    let choice = RwSignal::new(String::new());
    let loaded = RwSignal::new(false);
    let loading = RwSignal::new(false);
    let load_error = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let pending = RwSignal::new(recovery());
    let name = RwSignal::new(pending.with_untracked(|value| value.as_ref().ok().and_then(Option::as_ref).and_then(|value| value["name"].as_str()).unwrap_or_default().to_owned()));
    let model = RwSignal::new(String::new());
    let models = RwSignal::new(Vec::<String>::new());
    let model_catalog = RwSignal::new(Vec::<Model>::new());
    let model_options = RwSignal::new(json!({}));
    let selected = Memo::new(move |_| available.with(|entries| entries.iter().find(|entry| entry.value == choice.get()).cloned()));
    let directory = use_context::<ReadSignal<Option<Result<shell::Directory, String>>>>();
    let guide = move || directory.and_then(|signal| signal.get()).and_then(Result::ok).and_then(|value| value.guide).filter(|agent| !agent.hidden);
    let retry = move || {
        if loading.get_untracked() { return; }
        loading.set(true); load_error.set(String::new());
        leptos::task::spawn_local(async move {
            match connections().await {
                Ok((items, _automatic)) => {
                    if !items.iter().any(|item| item.value == choice.get_untracked()) { choice.set(_automatic.unwrap_or_default()); }
                    available.set(items); loaded.set(true);
                }
                Err(message) => load_error.set(message),
            }
            loading.set(false);
        });
    };
    retry();
    let generation = RwSignal::new(0u64);
    Effect::new(move |_| {
        generation.update(|value| *value += 1);
        let request = generation.get_untracked();
        if let Some(connection) = selected.get() {
            model.set(connection.model.clone()); models.set(Vec::new()); model_catalog.set(Vec::new()); model_options.set(json!({}));
            let Some(instance) = connection.value.strip_prefix("instance:") else { return; };
            let url = format!("/api/provider-instances/{instance}/models");
            leptos::task::spawn_local(async move {
                if let Ok(value) = api::read::<Models>(&url).await {
                    if generation.try_get_untracked() == Some(request) {
                        if model.get_untracked() == connection.model && !value.models.contains(&connection.model) {
                            if let Some(default) = value.models.first() { model.set(default.clone()); }
                        }
                        models.set(value.models);
                        model_catalog.set(value.catalog);
                    }
                }
            });
        } else { model.set(String::new()); models.set(Vec::new()); model_catalog.set(Vec::new()); model_options.set(json!({})); }
    });
    let model_options_for_selection = Signal::derive(move || model_catalog.with(|catalog| catalog.iter().find(|entry| entry.id == model.get()).map(|entry| entry.options.clone()).unwrap_or_default()));
    let retry_creation = move || pending.with(|value| matches!(value, Ok(Some(_))));
    let create = move || !retry_creation() && selected.with(|entry| entry.as_ref().is_some_and(|entry| entry.ready));
    let locked = move || busy.get() || pending.with(|value| !matches!(value, Ok(None)));
    let submit = move |event: web_sys::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() { return; }
        let command = pending.get_untracked().and_then(|value| {
            if let Some(value) = value { return Ok(value); }
            let connection = selected.get_untracked().filter(|connection| connection.ready).ok_or("Choose a ready provider.")?;
            let name = name.get_untracked().trim().to_owned();
            let model = model.get_untracked().trim().to_owned();
            if name.is_empty() || model.is_empty() { return Err("Enter a name and model.".into()); }
            Ok(json!({"id": request_id()?, "name": name, "role": "", "model": model, "instructions": "Help the user complete their tasks.", "threadLimit": 2, "toolBudget": 90, "providerInstance": connection.value.strip_prefix("instance:").unwrap_or(""), "modelOptions": model_options.get_untracked()}))
        });
        let save = command.and_then(|command| {
            let storage = window().session_storage().ok().flatten().ok_or("Recovery storage is unavailable.")?;
            storage.set_item(RECOVERY, &command.to_string()).map_err(|_| "Recovery data could not be saved.")?;
            Ok(command)
        });
        let command = match save { Ok(command) => command, Err(message) => { error.set(message); return; } };
        pending.set(Ok(Some(command.clone()))); busy.set(true); error.set(String::new());
        leptos::task::spawn_local(async move {
            match api::post::<shell::Agent>("/api/agents", &command).await {
                Ok(agent) => { if let Ok(Some(storage)) = window().session_storage() { let _ = storage.remove_item(RECOVERY); } let _ = window().location().assign(&format!("/agents/{}", agent.id)); }
                Err(message) => { error.set(message); busy.set(false); }
            }
        });
    };
    let guide_click = move |_| {
        if busy.get_untracked() { return; }
        let (Some(agent), Some(connection)) = (guide(), selected.get_untracked().filter(|connection| connection.ready)) else { return; };
        busy.set(true); error.set(String::new());
        leptos::task::spawn_local(async move {
            if !agent.runnable {
                let result = api::command::<Value>(&format!("/api/agents/{}/edit", agent.id), json!({"revision":agent.revision,"providerInstance":connection.value.strip_prefix("instance:").unwrap_or(""),"model":connection.model})).await;
                if let Err(message) = result { error.set(message); busy.set(false); return; }
            }
            let _ = window().location().assign(&format!("/agents/{}", agent.id));
        });
    };
    view! {
        <main class="chat page-view bs-root"><div class="page-scroll"><div class="first-run">
            {workspace::heading("Set up Minarbot".into(), String::new())}
            <ErrorAlert message=load_error icon=error_icon.clone() />
            <button type="button" class="k-btn k-btn--sm k-btn--secondary" hidden=move || load_error.with(String::is_empty) on:click=move |_| retry()>"Retry"</button>
            {computer(error_icon.clone(), icon(|icons| &icons.monitor))}
            <ol class="first-run__steps" aria-label="Setup progress">
                <li class:is-active=move || !create() class:is-done=create aria-current=move || (!create()).then_some("step")>
                    <div class="first-run__step"><span class="first-run__icon" aria-hidden="true" inner_html=move || if create() { icon(|icons| &icons.check) } else { icon(|icons| &icons.plug) }></span><strong>"Connect a provider"</strong><span class="first-run__status" hidden=move || !create()>{move || selected.get().map(|entry| entry.label)}</span></div>
                    <div class="first-run__form" hidden=move || create() || !loaded.get() && load_error.with(String::is_empty)><div class="k-stack">
                        <p class="k-field__hint" hidden=move || available.with(|entries| entries.iter().any(|entry| entry.ready))>"Add or enable a provider in Settings > Providers, then check again."</p>
                        <button type="button" class="k-btn k-btn--md k-btn--secondary" disabled=loading on:click=move |_| retry()>"Check again"</button>
                        <select class="k-input" aria-label="Provider" prop:value=choice on:change=move |event| choice.set(event_target_value(&event))>
                            <option value="">"Choose a provider"</option>
                            {move || available.get().into_iter().map(|entry| view! { <option value=entry.value.clone() disabled=!entry.ready>{if entry.ready { entry.label } else { format!("{} · {}", entry.label, entry.reason) }}</option> }).collect_view()}
                        </select>
                    </div></div>
                </li>
                <li class:is-active=create aria-current=move || create().then_some("step")>
                    <div class="first-run__step"><span class="first-run__icon" aria-hidden="true" inner_html=icon(|icons| &icons.plus)></span><strong>"Create agent"</strong></div>
                    <div class="first-run__form" hidden=move || !create() && pending.with(Result::is_ok)><div class="k-stack">
                        <button type="button" class="k-btn k-btn--md k-btn--secondary" hidden=move || guide().is_none() || locked() on:click=guide_click>{move || guide().map(|agent| format!("Set up with {}", agent.name))}</button>
                        <form class="k-stack" on:submit=submit>
                            <label class="k-field"><span class="k-field__label">"Provider"</span><select class="k-input k-select" aria-label="Create agent provider" prop:value=choice disabled=locked on:change=move |event| choice.set(event_target_value(&event))>
                                <option value="">"Choose a provider"</option>
                                {move || available.get().into_iter().map(|entry| view! { <option value=entry.value.clone() disabled=!entry.ready>{if entry.ready { entry.label } else { format!("{} · {}", entry.label, entry.reason) }}</option> }).collect_view()}
                            </select></label>
                            <label class="k-field"><span class="k-field__label">"Name"</span><input class="k-input" required maxlength="80" placeholder="Iris" prop:value=name readonly=locked on:input=move |event| name.set(event_target_value(&event)) /></label>
                            <label class="k-field"><span class="k-field__label">"Model"</span><input class="k-input" required maxlength="160" placeholder="Choose a provider model" list="onboarding-models" readonly=locked prop:value=move || pending.with(|value| value.as_ref().ok().and_then(Option::as_ref).and_then(|value| value["model"].as_str()).map(str::to_owned)).unwrap_or_else(|| model.get()) on:input=move |event| model.set(event_target_value(&event)) /></label>
                            <datalist id="onboarding-models">{move || models.get().into_iter().map(|entry| view! { <option value=entry /> }).collect_view()}</datalist>
                            <Show when=move || !model_options_for_selection.with(Vec::is_empty)>
                                <div class="agent-model-options"><h3>Model options</h3>
                                    {move || model_options_for_selection.get().into_iter().map(|option| {
                                        if option.kind == "boolean" {
                                            let key = option.id.clone();
                                            let checked = Signal::derive({ let key = key.clone(); move || model_options.with(|value| value.get(&key).and_then(Value::as_bool).unwrap_or(false)) });
                                            let change_key = option.id.clone();
                                            view! { <div class="k-row"><div class="k-row__main"><span class="k-row__text"><span class="k-row__title">{option.id.clone()}</span></span></div><Switch label=option.id.clone() checked change=Callback::new(move |value| model_options.update(|options| options[&change_key] = json!(value))) /></div> }.into_any()
                                        } else {
                                            let values = option.values.clone();
                                            let change_key = option.id.clone();
                                            let value_key = change_key.clone();
                                            view! { <label class="k-field"><span class="k-field__label">{option.id.clone()}</span><select class="k-input k-select" aria-label=option.id.clone() prop:value=move || model_options.with(|options| options.get(&value_key).and_then(Value::as_str).unwrap_or_default().to_owned()) on:change=move |event| model_options.update(|options| options[&change_key] = json!(event_target_value(&event)))>{values.into_iter().map(|value| view! { <option value=value.clone()>{value.clone()}</option> }).collect_view()}</select></label> }.into_any()
                                        }
                                    }).collect_view()}
                                </div>
                            </Show>
                            <ErrorAlert message=error icon=error_icon.clone() />
                            <p role="alert" hidden=move || pending.with(Result::is_ok)>"Recovery data could not be read."</p>
                            <div class="k-row-actions"><button type="submit" class="k-btn k-btn--md k-btn--primary" disabled=move || busy.get() || pending.with(Result::is_err)>{move || if retry_creation() { "Retry creation" } else { "Create agent" }}</button></div>
                        </form>
                    </div></div>
                </li>
                <li><div class="first-run__step"><span class="first-run__icon" aria-hidden="true" inner_html=icon(|icons| &icons.send)></span><strong>"Send first message"</strong></div></li>
            </ol>
        </div></div></main>
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Computer {
    state: String,
    runtime: Option<String>,
    missing_programs: Vec<String>,
    can_setup: bool,
    message: Option<String>,
}

fn computer(error_icon: String, monitor: String) -> impl IntoView {
    let status = RwSignal::new(None::<Computer>);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let refresh = move |setup| {
        if busy.get_untracked() || document().hidden() { return; }
        busy.set(true);
        leptos::task::spawn_local(async move {
            let result = if setup { api::post(COMPUTER, &json!({})).await } else { api::read(COMPUTER).await };
            match result { Ok(value) => { status.set(Some(value)); error.set(String::new()); }, Err(message) => error.set(message) }
            busy.set(false);
        });
    };
    refresh(false);
    crate::connection_ui::poll_with_backoff(5000, move || (!busy.get_untracked()).then(|| !error.with_untracked(String::is_empty)), move || refresh(false));
    view! {
        <div class="first-run__computer k-stack"><div class="k-card" hidden=move || status.with(Option::is_none)><div class="k-row"><div class="k-row__main"><span class="k-row__leading" aria-hidden="true" inner_html=monitor></span><span class="k-row__text"><span class="k-row__title">"Computer (optional)"</span><span class="k-row__meta" role="status">{move || status.get().map(|value| match value.state.as_str() { "no_runtime" => "No container runtime".into(), "image_missing" => "Image missing".into(), "downloading" => if value.runtime.is_none() { "Setting up Computer".into() } else { "Downloading image".into() }, "ready" => value.runtime.map_or_else(|| "Ready".into(), |runtime| format!("Ready · {runtime}")), _ => "Setup failed".into() })}</span><span class="k-row__meta" hidden=move || status.with(|value| value.as_ref().is_none_or(|value| value.missing_programs.is_empty()))>{move || status.get().map(|value| format!("Missing programs - {}", value.missing_programs.join(", ")))}</span></span></div><span class="k-row__trailing"><button type="button" class="k-btn k-btn--md k-btn--secondary" hidden=move || !status.with(|value| value.as_ref().is_some_and(|value| value.can_setup)) disabled=move || busy.get() on:click=move |_| refresh(true)>"Set up Computer"</button></span></div></div><ErrorAlert message=error icon=error_icon /></div>
    }
}
