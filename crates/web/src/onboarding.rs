use crate::{api, general::ErrorAlert, shell, workspace};
use leptos::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};

const PROVIDERS: &str = "/api/provider-accounts";
const RECOVERY: &str = "minarbot-agent-creation";
const COMPUTER: &str = "/api/settings/computer";

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Provider {
    id: String,
    name: String,
    provider: String,
    default_model: String,
    configured: bool,
}

#[derive(Deserialize)]
struct Runtimes {
    choices: Vec<Runtime>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Runtime {
    runtime: String,
    label: String,
    available: bool,
    enabled: bool,
    signed_in: Option<bool>,
    models: Vec<Model>,
}

#[derive(Deserialize)]
struct Model {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Login {
    id: String,
    name: String,
    runtime: String,
    signed_in: bool,
}

#[derive(Clone, PartialEq)]
pub(crate) struct Connection {
    pub(crate) value: String,
    pub(crate) label: String,
    pub(crate) runtime: String,
    pub(crate) account: String,
    pub(crate) model: String,
}

fn provider_connection(provider: Provider, codex: bool) -> Connection {
    Connection {
        value: format!("account:{}", provider.id),
        label: provider.name,
        runtime: if provider.provider == "openai-compatible" && codex {
            "codex"
        } else {
            "api"
        }
        .into(),
        account: provider.id,
        model: provider.default_model,
    }
}

pub(crate) async fn connections() -> Result<(Vec<Connection>, Option<String>, bool), String> {
    let providers: Vec<Provider> = api::read(PROVIDERS).await?;
    let logins: Vec<Login> = api::read("/api/engine-logins").await?;
    let runtimes = api::read::<Runtimes>("/api/runtimes").await?.choices;
    let ready: Vec<_> = runtimes
        .into_iter()
        .filter(|engine| engine.available && engine.enabled)
        .collect();
    let codex = ready.iter().any(|engine| engine.runtime == "codex");
    let devices: Vec<_> = ["codex", "claude"]
        .into_iter()
        .filter_map(|runtime| {
            let engine = ready
                .iter()
                .find(|engine| engine.runtime == runtime && engine.signed_in == Some(true))?;
            Some(Connection {
                value: format!("device:{runtime}"),
                label: engine.label.clone(),
                runtime: runtime.into(),
                account: "device".into(),
                model: "default".into(),
            })
        })
        .collect();
    let providers: Vec<_> = providers
        .into_iter()
        .filter(|provider| provider.configured)
        .map(|provider| provider_connection(provider, codex))
        .collect();
    let logins: Vec<_> = logins
        .into_iter()
        .filter(|login| {
            login.signed_in && ready.iter().any(|engine| engine.runtime == login.runtime)
        })
        .map(|login| {
            let id = format!("login:{}", login.id);
            Connection {
                value: id.clone(),
                label: login.name,
                runtime: login.runtime,
                account: id,
                model: "default".into(),
            }
        })
        .collect();
    let hosts: Vec<_> = ready
        .into_iter()
        .filter(|engine| {
            engine.signed_in == Some(true) && !matches!(engine.runtime.as_str(), "codex" | "claude")
        })
        .map(|engine| Connection {
            value: format!("host:{}", engine.runtime),
            label: engine.label,
            runtime: engine.runtime,
            account: String::new(),
            model: engine
                .models
                .into_iter()
                .next()
                .map_or_else(|| "default".into(), |model| model.id),
        })
        .collect();
    let automatic = devices
        .first()
        .or_else(|| {
            providers
                .iter()
                .find(|provider| provider.runtime == "codex")
        })
        .or_else(|| providers.first())
        .or_else(|| logins.first())
        .or_else(|| hosts.first())
        .map(|entry| entry.value.clone());
    Ok((
        devices
            .into_iter()
            .chain(logins)
            .chain(providers)
            .chain(hosts)
            .collect(),
        automatic,
        codex,
    ))
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
}

#[component]
fn ProviderForm(
    saved: impl Fn(Provider) + Copy + Send + 'static,
    error_icon: String,
) -> impl IntoView {
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
    let kind = RwSignal::new(0usize);
    let url = RwSignal::new(TYPES[0].2.to_owned());
    let name = RwSignal::new(String::new());
    let key = RwSignal::new_local(String::new());
    let models = RwSignal::new(None::<Vec<String>>);
    let model = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let id = StoredValue::new(request_id());
    let apply = move |event: web_sys::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() {
            return;
        }
        let Ok(id) = id.get_value() else {
            error.set("Could not create a request. Reload and try again.".into());
            return;
        };
        let save = event
            .submitter()
            .and_then(|button| button.get_attribute("value"))
            .as_deref()
            == Some("save");
        let selected = model.get_untracked();
        if save
            && !models.with_untracked(|models| {
                models
                    .as_ref()
                    .is_some_and(|models| models.contains(&selected))
            })
        {
            return;
        }
        let mut body = json!({ "id": id, "provider": TYPES[kind.get_untracked()].0, "name": name.get_untracked(), "baseURL": url.get_untracked(), "apiKey": key.get_untracked() });
        if !save {
            models.set(None);
        }
        error.set(String::new());
        busy.set(true);
        leptos::task::spawn_local(async move {
            let result = if save {
                body["defaultModel"] = json!(selected);
                api::post(PROVIDERS, &body).await.map(|result| {
                    key.try_set(String::new());
                    saved(result);
                })
            } else {
                api::post::<Models>("/api/provider-accounts/test", &body)
                    .await
                    .map(|result| {
                        model.try_set(result.models.first().cloned().unwrap_or_default());
                        models.try_set(Some(result.models));
                    })
            };
            if let Err(message) = result {
                error.try_set(message);
            }
            busy.try_set(false);
        });
    };
    view! {
        <form class="k-stack" on:submit=apply>
            <label class="k-field"><span class="k-field__label">"Type"</span><span class="connection-select"><select class="k-input" disabled=busy on:change=move |event| {
                if let Some(index) = TYPES.iter().position(|entry| entry.0 == event_target_value(&event)) {
                    kind.set(index); url.set(TYPES[index].2.into()); models.set(None); error.set(String::new());
                }
            }>{TYPES.iter().map(|(id, label, _)| view! { <option value=*id>{*label}</option> }).collect_view()}</select><span class="k-select__chev"><crate::connection_ui::Icon name="chevron-down" /></span></span></label>
            <label class="k-field"><span class="k-field__label">"Name"</span><input class="k-input" required maxlength="80" readonly=busy on:input=move |event| name.set(event_target_value(&event)) /></label>
            <label class="k-field"><span class="k-field__label">"Base URL"</span><input class="k-input" type="url" required maxlength="2048" prop:value=url readonly=busy
                on:input=move |event| { url.set(event_target_value(&event)); models.set(None); error.set(String::new()); } /></label>
            <label class="k-field"><span class="k-field__label">"API key"</span><input class="k-input" type="password" required maxlength="8192" autocomplete="new-password" readonly=busy
                prop:value=move || key.get() on:input=move |event| { key.set(event_target_value(&event)); models.set(None); error.set(String::new()); } /></label>
            <label class="k-field"><span class="k-field__label">"Default model"</span><span class="connection-select"><select class="k-input" prop:value=model disabled=move || busy.get() || models.with(|models| models.as_ref().is_none_or(Vec::is_empty))
                on:change=move |event| model.set(event_target_value(&event))>
                <option value="" disabled>"Test to load models"</option>
                {move || models.get().unwrap_or_default().into_iter().map(|value| view! { <option value=value.clone()>{value.clone()}</option> }).collect_view()}
            </select><span class="k-select__chev"><crate::connection_ui::Icon name="chevron-down" /></span></span></label>
            <span role="status" class="k-field__hint" hidden=move || models.with(Option::is_none)>{move || models.with(|value| value.as_ref().map(|models| format!("{} models found", models.len())))}</span>
            <ErrorAlert message=error icon=error_icon />
            <div class="k-row-actions">
                <button type="submit" name="action" value="test" class="k-btn k-btn--md k-btn--secondary" disabled=busy>"Test"</button>
                <button type="submit" name="action" value="save" class="k-btn k-btn--md k-btn--primary" disabled=move || busy.get() || models.with(|value| value.as_ref().is_none_or(Vec::is_empty))>"Save"</button>
            </div>
        </form>
    }
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

pub fn page(error_icon: String) -> impl IntoView {
    let icons = StoredValue::new(use_context::<shell::Icons>());
    let icon = move |pick: fn(&shell::Icons) -> &String| {
        icons.with_value(|value| {
            value
                .as_ref()
                .map(|icons| pick(icons).clone())
                .unwrap_or_default()
        })
    };
    let available = RwSignal::new(Vec::<Connection>::new());
    let choice = RwSignal::new(String::new());
    let codex = RwSignal::new(false);
    let loaded = RwSignal::new(false);
    let loading = RwSignal::new(false);
    let adding = RwSignal::new(false);
    let load_error = RwSignal::new(String::new());
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
    let selected = Memo::new(move |_| {
        available.with(|entries| {
            entries
                .iter()
                .find(|entry| entry.value == choice.get())
                .cloned()
        })
    });
    let directory = use_context::<ReadSignal<Option<Result<shell::Directory, String>>>>();
    let guide = move || {
        directory
            .and_then(|signal| signal.get())
            .and_then(Result::ok)
            .and_then(|value| value.guide)
            .filter(|agent| !agent.hidden)
    };
    let retry = move || {
        if loading.get_untracked() {
            return;
        }
        loading.set(true);
        load_error.set(String::new());
        leptos::task::spawn_local(async move {
            match connections().await {
                Ok((items, automatic, ready)) => {
                    if !items
                        .iter()
                        .any(|item| item.value == choice.get_untracked())
                    {
                        choice.set(automatic.unwrap_or_default());
                    }
                    available.try_set(items);
                    codex.try_set(ready);
                    loaded.try_set(true);
                }
                Err(_) => {
                    load_error.try_set("Setup status could not be loaded.".into());
                }
            }
            loading.try_set(false);
        });
    };
    retry();
    let generation = RwSignal::new(0u64);
    Effect::new(move |_| {
        generation.update(|value| *value += 1);
        let request = generation.get_untracked();
        if let Some(connection) = selected.get() {
            model.set(connection.model.clone());
            models.set(Vec::new());
            if let Ok(query) = web_sys::UrlSearchParams::new() {
                query.set("runtime", &connection.runtime);
                query.set("account", &connection.account);
                let url = format!(
                    "/api/models?{}",
                    query.to_string().as_string().unwrap_or_default()
                );
                leptos::task::spawn_local(async move {
                    if let Ok(value) = api::read::<Models>(&url).await {
                        if generation.try_get_untracked() == Some(request) {
                            if connection.account != "device"
                                && !connection.account.starts_with("login:")
                                && model.get_untracked() == connection.model
                                && !value.models.contains(&connection.model)
                            {
                                if let Some(default) = value.models.first() {
                                    model.set(default.clone());
                                }
                            }
                            models.set(value.models);
                        }
                    }
                });
            }
        }
    });
    let save_provider = move |provider| {
        let entry = provider_connection(provider, codex.get_untracked());
        choice.set(entry.value.clone());
        available.update(|available| available.push(entry));
        adding.set(false);
    };
    let retry_creation = move || pending.with(|value| matches!(value, Ok(Some(_))));
    let create = move || !adding.get() && (selected.with(Option::is_some) || retry_creation());
    let locked = move || busy.get() || pending.with(|value| !matches!(value, Ok(None)));
    let submit = move |event: web_sys::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() {
            return;
        }
        let command = pending.get_untracked().and_then(|value| {
            if let Some(value) = value { return Ok(value); }
            let connection = selected.get_untracked().ok_or("Choose an AI connection.")?;
            let name = name.get_untracked().trim().to_owned();
            let model = model.get_untracked().trim().to_owned();
            if name.is_empty() || model.is_empty() { return Err("Enter a name and model.".into()); }
            Ok(json!({
                "id": request_id()?, "name": name, "role": "", "model": model, "backupModel": "",
                "instructions": "Help the user complete their tasks.", "threadLimit": 2, "toolBudget": 90,
                "providerAccount": connection.account, "runtime": connection.runtime,
            }))
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
        leptos::task::spawn_local(async move {
            match api::post::<shell::Agent>("/api/agents", &command).await {
                Ok(agent) => {
                    if let Ok(Some(storage)) = window().session_storage() {
                        let _ = storage.remove_item(RECOVERY);
                    }
                    let _ = window().location().assign(&format!("/agents/{}", agent.id));
                }
                Err(message) => {
                    error.try_set(message);
                    busy.try_set(false);
                }
            }
        });
    };
    let guide_click = move |_| {
        if busy.get_untracked() {
            return;
        }
        let (Some(agent), Some(connection)) = (guide(), selected.get_untracked()) else {
            return;
        };
        busy.set(true);
        error.set(String::new());
        leptos::task::spawn_local(async move {
            if !agent.runnable {
                let result = api::command::<Value>(&format!("/api/agents/{}/edit", agent.id), json!({
                    "revision": agent.revision, "runtime": connection.runtime, "providerAccount": connection.account,
                    "model": connection.model, "confirmRuntime": true,
                })).await;
                if let Err(message) = result {
                    error.try_set(message);
                    busy.try_set(false);
                    return;
                }
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
                    <div class="first-run__step"><span class="first-run__icon" aria-hidden="true"
                        inner_html=move || if create() { icon(|icons| &icons.check) } else { icon(|icons| &icons.plug) }></span>
                        <strong>"Connect an AI"</strong><span class="first-run__status" hidden=move || !create()>{move || selected.get().map(|entry| entry.label)}</span>
                        <button type="button" class="k-btn k-btn--md k-btn--secondary" hidden=move || !create() || locked() || selected.with(|entry| entry.as_ref().is_some_and(|entry| entry.value.starts_with("account:")))
                            on:click=move |_| adding.set(true)>"Use an API key"</button>
                    </div>
                    <div class="first-run__form" hidden=move || create() || !loaded.get() && load_error.with(String::is_empty)><div class="k-stack">
                        <p class="k-field__hint" hidden=move || selected.with(Option::is_some)>"Sign in to Codex or Claude Code on this machine, then check again. No API key needed."</p>
                        <button type="button" class="k-btn k-btn--md k-btn--secondary" disabled=loading hidden=move || selected.with(Option::is_some) on:click=move |_| retry()>"Check again"</button>
                        <a class="k-btn k-btn--md k-btn--secondary" href="/settings/engines">"Set up a separate engine login"</a>
                        <p class="k-field__hint">"Or use an API key from an AI provider."</p>
                        <ProviderForm saved=save_provider error_icon=error_icon.clone() />
                        <button type="button" class="k-btn k-btn--md k-btn--ghost" hidden=move || selected.with(Option::is_none) on:click=move |_| adding.set(false)>
                            {move || selected.get().map(|entry| format!("Use {}", entry.label))}
                        </button>
                    </div></div>
                </li>
                <li class:is-active=create aria-current=move || create().then_some("step")>
                    <div class="first-run__step"><span class="first-run__icon" aria-hidden="true" inner_html=icon(|icons| &icons.plus)></span><strong>"Create agent"</strong></div>
                    <div class="first-run__form" hidden=move || !create() && pending.with(Result::is_ok)><div class="k-stack">
                        <button type="button" class="k-btn k-btn--md k-btn--secondary" hidden=move || guide().is_none() || locked() on:click=guide_click>
                            {move || guide().map(|agent| format!("Set up with {}", agent.name))}
                        </button>
                        <form class="k-stack" on:submit=submit>
                            <label class="k-field"><span class="k-field__label">"Name"</span><input class="k-input" required maxlength="80" placeholder="Iris" prop:value=name readonly=locked on:input=move |event| name.set(event_target_value(&event)) /></label>
                            <label class="k-field" hidden=move || available.with(|entries| entries.len() < 2)><span class="k-field__label">"Run with"</span>
                                <span class="connection-select"><select class="k-input" prop:value=choice disabled=locked on:change=move |event| choice.set(event_target_value(&event))>
                                    {move || available.get().into_iter().map(|entry| view! { <option value=entry.value>{entry.label}</option> }).collect_view()}
                                </select><span class="k-select__chev"><crate::connection_ui::Icon name="chevron-down" /></span></span>
                            </label>
                            <label class="k-field"><span class="k-field__label">"Model"</span><input class="k-input" required maxlength="160" placeholder="Enter model ID" list="onboarding-models" readonly=locked
                                prop:value=move || pending.with(|value| value.as_ref().ok().and_then(Option::as_ref).and_then(|value| value["model"].as_str()).map(str::to_owned)).unwrap_or_else(|| model.get())
                                on:input=move |event| model.set(event_target_value(&event)) /></label>
                            <datalist id="onboarding-models">{move || models.get().into_iter().map(|entry| view! { <option value=entry /> }).collect_view()}</datalist>
                            <ErrorAlert message=error icon=error_icon />
                            <p role="alert" hidden=move || pending.with(Result::is_ok)>"Recovery data could not be read."</p>
                            <div class="k-row-actions">
                                <button type="submit" class="k-btn k-btn--md k-btn--primary" disabled=move || busy.get() || pending.with(Result::is_err)>
                                    {move || if retry_creation() { "Retry creation" } else { "Create agent" }}
                                </button>
                                <button type="button" class="k-btn k-btn--md k-btn--ghost" disabled=busy hidden=move || pending.with(|value| matches!(value, Ok(None)))
                                    on:click=move |_| {
                                        if window().confirm_with_message("Discard recovery data? Check the agent list first.").unwrap_or(false) {
                                            if let Ok(Some(storage)) = window().session_storage() {
                                                if storage.remove_item(RECOVERY).is_ok() { pending.set(Ok(None)); }
                                            }
                                        }
                                    }>{move || if pending.with(Result::is_err) { "Discard recovery data" } else { "Discard recovery" }}</button>
                            </div>
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
        if busy.get_untracked() || document().hidden() {
            return;
        }
        busy.set(true);
        leptos::task::spawn_local(async move {
            let result = if setup {
                api::post(COMPUTER, &json!({})).await
            } else {
                api::read(COMPUTER).await
            };
            match result {
                Ok(value) => {
                    status.try_set(Some(value));
                    error.try_set(String::new());
                }
                Err(message) => {
                    error.try_set(message);
                }
            }
            busy.try_set(false);
        });
    };
    refresh(false);
    if let Ok(timer) =
        set_interval_with_handle(move || {
            if !document().hidden() {
                refresh(false);
            }
        }, std::time::Duration::from_secs(5))
    {
        on_cleanup(move || timer.clear());
    }
    view! {
        <div class="first-run__computer k-stack">
            <div class="k-card" hidden=move || status.with(Option::is_none)>
                <div class="k-row"><div class="k-row__main"><span class="k-row__leading" aria-hidden="true" inner_html=monitor></span><span class="k-row__text">
                    <span class="k-row__title">"Computer (optional)"</span>
                    <span class="k-row__meta" role="status">{move || status.get().map(|value| match value.state.as_str() {
                        "no_runtime" => "No container runtime".into(), "image_missing" => "Image missing".into(),
                        "downloading" => if value.runtime.is_none() { "Setting up Computer".into() } else { "Downloading image".into() },
                        "ready" => value.runtime.map_or_else(|| "Ready".into(), |runtime| format!("Ready · {runtime}")),
                        _ => "Setup failed".into(),
                    })}</span>
                    <span class="k-row__meta" hidden=move || status.with(|value| value.as_ref().is_none_or(|value| value.missing_programs.is_empty()))>
                        {move || status.get().map(|value| format!("Missing programs - {}", value.missing_programs.join(", ")))}
                    </span>
                </span></div><span class="k-row__trailing">
                    <button type="button" class="k-btn k-btn--md k-btn--secondary" hidden=move || !status.with(|value| value.as_ref().is_some_and(|value| value.can_setup))
                        disabled=move || busy.get() || status.with(|value| value.as_ref().is_some_and(|value| value.state == "downloading"))
                        on:click=move |_| refresh(true)>"Set up Computer"</button>
                </span></div>
            </div>
            <ErrorAlert message=error icon=error_icon />
            <p role="alert" hidden=move || status.with(|value| value.as_ref().is_none_or(|value| value.message.is_none()))>{move || status.get().and_then(|value| value.message)}</p>
        </div>
    }
}
