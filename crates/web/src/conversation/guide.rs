use super::*;
use crate::{api, onboarding, settings, system::kit};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde_json::{json, Map};
use wasm_bindgen_futures::JsFuture;

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(super) struct Actions {
    changes: Vec<Change>,
    screens: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Change {
    id: String,
    status: String,
    change: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    status: String,
    #[serde(rename = "redirect_url")]
    redirect_url: Option<String>,
    install_request: Option<String>,
    message: Option<String>,
}

fn from() -> String {
    let location = window().location();
    format!(
        "{}{}",
        location.pathname().unwrap_or_default(),
        location.search().unwrap_or_default()
    )
}

fn screen(id: &str, origin: Option<&str>) -> Option<AnyView> {
    let catalog: HashMap<String, Screen> =
        serde_json::from_str(include_str!("../../assets/guide-screens.json")).ok()?;
    let Screen { title, path } = catalog.get(id)?;
    let mut href = if path.starts_with("/settings/") {
        settings::section_url(path, &from())
    } else {
        path.clone()
    };
    if let Some(origin) = origin {
        let params = web_sys::UrlSearchParams::new().ok()?;
        params.set("pairingOrigin", origin);
        let query = params.to_string().as_string()?;
        let separator = if href.contains('?') { '&' } else { '?' };
        href.push(separator);
        href.push_str(&query);
    }
    Some(view! { <a class="k-link" href=href>"Open "{title.clone()}</a> }.into_any())
}

#[derive(Deserialize)]
struct Screen {
    title: String,
    path: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Install {
    source: String,
    expires_at: f64,
}

async fn prepare_install(id: &str) -> Result<(Install, String), String> {
    let error = "Install command could not be created.";
    let crypto = window().crypto().map_err(|_| error)?;
    let mut bytes = [0; 32];
    crypto
        .get_random_values_with_u8_array(&mut bytes)
        .map_err(|_| error)?;
    let token = format!("bsi_{}", URL_SAFE_NO_PAD.encode(bytes));
    let digest = crypto
        .subtle()
        .digest_with_str_and_u8_array("SHA-256", token.as_bytes())
        .map_err(|_| error)?;
    let digest = JsFuture::from(digest).await.map_err(|_| error)?;
    let digest = URL_SAFE_NO_PAD.encode(js_sys::Uint8Array::new(&digest).to_vec());
    let install = api::post("/api/install/tokens", &json!({"id": id, "digest": digest})).await?;
    Ok((install, token))
}

#[derive(Deserialize)]
struct Machines {
    machines: Vec<Machine>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Machine {
    origin: String,
    status: String,
    pairing_state: String,
}

fn install_command(install: Install, token: String, target: Value) -> impl IntoView {
    let origin = target["origin"].as_str().unwrap_or_default().to_owned();
    let name = target["name"].as_str().unwrap_or_default();
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    let command = format!(
        "curl -fsSL {} | sh -s -- --source {} --token {} --name {}{}",
        quote(&format!("{}/api/install/script", install.source)),
        quote(&install.source),
        quote(&token),
        quote(name),
        if origin.starts_with("https://") {
            format!(" --public-origin {}", quote(&origin))
        } else {
            String::new()
        },
    );
    let expired = RwSignal::new(js_sys::Date::now() >= install.expires_at);
    let online = RwSignal::new(false);
    let copied = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let origin = StoredValue::new(origin);
    let busy = RwSignal::new(false);
    let failed = RwSignal::new(false);
    let refresh = move || {
        expired.set(js_sys::Date::now() >= install.expires_at);
        if expired.get_untracked()
            || online.get_untracked()
            || busy.get_untracked()
            || document().hidden()
        {
            return;
        }
        busy.set(true);
        leptos::task::spawn_local(async move {
            let result = api::read::<Machines>("/api/machines").await;
            failed.try_set(result.is_err());
            if let Ok(result) = result {
                online.try_set(
                    origin
                        .try_with_value(|origin| {
                            result.machines.iter().any(|machine| {
                                &machine.origin == origin
                                    && machine.pairing_state == "paired"
                                    && machine.status == "online"
                            })
                        })
                        .unwrap_or(false),
                );
            }
            busy.try_set(false);
        });
    };
    refresh();
    crate::connection_ui::poll_with_backoff(
        5000,
        move || (!busy.get_untracked()).then(|| failed.get_untracked()),
        refresh,
    );
    let command = StoredValue::new(command);
    view! {
        <span role="status">{move || if online.get() { "Machine paired and online." } else if expired.get() { "Install command expired." } else { "Waiting for the machine to pair." }}</span>
        <Show when=move || !online.get() && !expired.get()>
            <label class="k-field"><span class="k-field__label">"Run on the new machine"</span>
                <textarea class="k-input k-textarea" rows="3" readonly spellcheck="false" autocomplete="off" prop:value=command.get_value()></textarea>
            </label>
            <div class="k-row-actions"><button class="k-btn k-btn--sm k-btn--secondary" type="button" on:click=move |_| {
                let text = command.get_value();
                leptos::task::spawn_local(async move {
                    if write_clipboard(&text).await.is_ok() {
                        copied.try_set(true); error.try_set(String::new());
                    } else {
                        error.try_set("Select the command to copy it.".into());
                    }
                });
            }>{move || if copied.get() { "Copied" } else { "Copy command" }}</button></div>
            {kit::error(error)}
        </Show>
        {screen("settings.machines", None)}
    }
}

fn field(key: &str, value: &Value) -> (String, String) {
    let title = match key {
        "agent" => "Agent ID".into(),
        "id" => "Routine ID".into(),
        "currentName" => "Current name".into(),
        "providerAccount" => "Provider".into(),
        "reportTo" => "Report to".into(),
        "toolBudget" => "Tool steps per task".into(),
        "backupModel" => "Fallback model".into(),
        "confirmRuntime" => "Switch runtime".into(),
        "hostKey" => "SSH host key".into(),
        "vaultEntry" => "Vault entry".into(),
        "port" => "SSH port".into(),
        "servicePort" => "Minarbot port".into(),
        "restartRequired" => "Restart required".into(),
        _ => key.chars().next().map_or_else(String::new, |first| {
            format!("{}{}", first.to_uppercase(), &key[first.len_utf8()..])
        }),
    };
    let value = match value {
        Value::Null => "Default".into(),
        Value::Bool(true) => "Yes".into(),
        Value::Bool(false) => "No".into(),
        Value::String(value) => match (key, value.as_str()) {
            ("runtime", "api") => "API".into(),
            ("backupModel", "") => "Clear fallback model".into(),
            (_, "") => "Default".into(),
            _ => value.clone(),
        },
        _ => serde_json::to_string_pretty(value).unwrap_or_default(),
    };
    (title, value)
}

fn fields(values: &Value) -> impl Iterator<Item = (&String, &Value)> {
    values.as_object().into_iter().flat_map(Map::iter)
}

fn proposal(chat: Chat, row: RwSignal<Turn>, item: Change) -> impl IntoView {
    let change = item.change;
    let key = StoredValue::new(item.id.clone());
    let path = StoredValue::new(format!("/api/guide/changes/{}", item.id));
    let current = Memo::new(move |_| {
        row.with(|turn| {
            turn.guide
                .as_ref()
                .and_then(|guide| {
                    guide
                        .changes
                        .iter()
                        .find(|change| &change.id == &key.get_value())
                })
                .map_or_else(|| item.status.clone(), |change| change.status.clone())
        })
    });
    let tool = change["tool"].as_str().unwrap_or_default();
    let title = match tool {
        "create_agent" => "Create agent",
        "update_agent" => "Update agent",
        "create_routine" => "Create routine",
        "update_routine" => "Update routine",
        "pause_routine" => "Pause routine",
        "resume_routine" => "Resume routine",
        "connect_app" => "Connect app",
        "set_connected_app_access" => "Change app access",
        "add_guide_machine" => "Add machine",
        "install_guide_vps" => "Install on VPS",
        "configure_guide_remote_access" => "Configure remote access",
        "setup_guide_computer" => "Set up Computer",
        _ => "Proposed change",
    };
    let fields = fields(&change["target"])
        .map(|(key, value)| {
            field(
                if key == "name" && tool != "add_guide_machine" {
                    "currentName"
                } else {
                    key
                },
                value,
            )
        })
        .chain(
            fields(&change["settings"])
                .filter(|(key, _)| *key != "vaultRevision")
                .map(|(key, value)| field(key, value)),
        )
        .collect::<Vec<_>>();
    let target = StoredValue::new(change["target"].clone());
    let tool = StoredValue::new(tool.to_owned());
    let change = StoredValue::new(change);
    let refresh = use_context::<crate::shell::DirectoryRefresh>();
    let heading = NodeRef::<leptos::html::H3>::new();
    let busy = RwSignal::new(false);
    let status = RwSignal::new(None::<String>);
    let shown = move || status.get().unwrap_or_else(|| current.get());
    let error = RwSignal::new(String::new());
    let notice = RwSignal::new(None::<String>);
    let link = RwSignal::new(None::<String>);
    let command = RwSignal::new(None::<(Install, String)>);
    let blocked = move || chat.config.with_value(|config| config.archived) || busy.get();
    let review = move |action| {
        if untrack(blocked) || untrack(shown) != "pending" {
            return;
        }
        busy.set(true);
        error.set(String::new());
        let endpoint = path.get_value();
        let body = json!({"action": action, "change": change.get_value()});
        leptos::task::spawn_local(async move {
            match api::post::<Receipt>(&endpoint, &body).await {
                Ok(receipt) => {
                    if receipt.status == "applied" {
                        if let Some(refresh) = refresh {
                            refresh.0.run(());
                        }
                    }
                    status.try_set(Some(receipt.status));
                    notice.try_set(receipt.message);
                    link.try_set(receipt.redirect_url.filter(|url| {
                        web_sys::Url::new(url).is_ok_and(|url| url.protocol() == "https:")
                    }));
                    if let Some(id) = receipt.install_request {
                        match prepare_install(&id).await {
                            Ok(install) => {
                                command.try_set(Some(install));
                            }
                            Err(message) => {
                                error.try_set(message);
                            }
                        }
                    }
                    if let Some(heading) = heading.try_get_untracked().flatten() {
                        let _ = heading.focus();
                    }
                }
                Err(message) => {
                    error.try_set(message);
                }
            }
            busy.try_set(false);
        });
    };
    let heading_id = format!("guide-change-{}", key.get_value());
    view! {
        <article class="k-card guide-change" aria-labelledby=heading_id.clone()>
            <header class="guide-change__header">
                <h3 id=heading_id.clone() tabindex="-1" node_ref=heading>{title}</h3>
                <span role="status"><span class="k-badge" class:is-warn=move || shown() == "pending"
                    class:is-neutral=move || shown() != "pending">
                    {move || if busy.get() && shown() == "pending" { "Applying…".to_owned() } else { match shown().as_str() {
                        "pending" => "Ready for review", "applied" => "Applied", "applying" => "Applying…",
                        "cancelled" => "Cancelled", "expired" => "Expired", "failed" => "Failed",
                        "unknown" => "Check settings to confirm", other => other,
                    }.to_owned() }}
                </span></span>
            </header>
            {(!fields.is_empty()).then(|| view! { <dl class="guide-change__fields">{fields.into_iter().map(|(key, value)| view! { <div><dt>{key}</dt><dd>{value}</dd></div> }).collect_view()}</dl> })}
            {move || (!error.with(String::is_empty)).then(|| view! { <kit::Alert tone="error">{error}</kit::Alert> })}
            {move || notice.get().map(|message| view! { <span role="status">{message}</span> })}
            <Show when=move || shown() == "applied">
                {move || if let Some((install, token)) = command.get() {
                    install_command(install, token, target.get_value()).into_any()
                } else { match tool.get_value().as_str() {
                    "add_guide_machine" | "install_guide_vps" => screen("settings.machines", None),
                    "configure_guide_remote_access" => target.with_value(|target| screen("settings.devices", target["origin"].as_str())),
                    "setup_guide_computer" => screen("settings.computer", None),
                    _ => None,
                }.into_any() }}
            </Show>
            {move || link.get().map(|href| view! { <a class="k-link" href=href target="_blank" rel="noopener noreferrer">"Finish app sign-in"</a> })}
            <Show when=move || shown() == "pending">
                <footer class="k-row-actions">
                    <kit::Button label=kit::text("Cancel") disabled=Signal::derive(blocked) on_click=move || review("cancel") />
                    <kit::Button label=kit::text("Apply") variant="primary" disabled=Signal::derive(blocked) on_click=move || review("apply") />
                </footer>
            </Show>
        </article>
    }
}

impl Actions {
    pub(super) fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.screens.is_empty()
    }
}

pub(super) fn actions(chat: Chat, row: RwSignal<Turn>) -> impl IntoView {
    let changes = Memo::new(move |_| {
        row.with(|turn| {
            turn.guide
                .as_ref()
                .map(|guide| guide.changes.clone())
                .unwrap_or_default()
        })
    });
    let screens = Memo::new(move |_| {
        row.with(|turn| {
            turn.guide
                .as_ref()
                .map(|guide| guide.screens.clone())
                .unwrap_or_default()
        })
    });
    view! {
        <For each=move || changes.get().into_iter().filter(|change| protocol::id(&change.id)) key=|change| change.id.clone()
            children=move |change| proposal(chat, row, change) />
        <div class="k-row-actions guide-screen-links" hidden=move || screens.with(Vec::is_empty)>
            {move || screens.get().into_iter().filter_map(|id| screen(&id, None)).collect_view()}
        </div>
    }
}

pub(super) fn connection(
    agent: Agent,
    opened: RwSignal<bool>,
    update: impl Fn(Agent) + Copy + Send + 'static,
) -> impl IntoView {
    let agent = RwSignal::new(agent);
    let refresh = use_context::<crate::shell::DirectoryRefresh>();
    let chevron = kit::icons().down;
    let options = RwSignal::new(Vec::<onboarding::Connection>::new());
    let pending = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let saving = RwSignal::new(String::new());
    let selected = RwSignal::new(String::new());
    let load = move || {
        if pending.get_untracked() {
            return;
        }
        pending.set(true);
        error.set(String::new());
        leptos::task::spawn_local(async move {
            match onboarding::connections().await {
                Ok((items, automatic)) => {
                    let current = agent.get_untracked();
                    let choice = items
                        .iter()
                        .find(|choice| choice.value.strip_prefix("instance:") == current.provider_instance.as_deref())
                        .map(|choice| choice.value.clone())
                        .or(automatic)
                        .unwrap_or_default();
                    selected.try_set(choice);
                    options.try_set(items);
                }
                Err(_) => {
                    error.try_set("Could not load all AI connections.".into());
                }
            }
            pending.try_set(false);
        });
    };
    Effect::new(move |_| {
        if opened.get() {
            untrack(load);
        }
    });
    let save = move |event: web_sys::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() {
            return;
        }
        let Some(choice) = options.with_untracked(|options| {
            options
                .iter()
                .find(|option| option.value == selected.get_untracked())
                .cloned()
        }) else {
            return;
        };
        busy.set(true);
        saving.set(String::new());
        let current = agent.get_untracked();
        leptos::task::spawn_local(async move {
            match api::command::<Agent>(&format!("/api/agents/{}/edit", current.id), json!({
                "revision": current.revision,
                "providerInstance": choice.value.strip_prefix("instance:").unwrap_or(""),
                "model": choice.model,
            })).await {
                Ok(next) => {
                    agent.try_set(next.clone());
                    opened.try_set(false);
                    update(next);
                    if let Some(refresh) = refresh { refresh.0.run(()); }
                }
                Err(message) => { saving.try_set(message); }
            }
            busy.try_set(false);
        });
    };
    view! {
        <kit::Dialog id="guide-connection" title=Signal::derive(move || format!("AI connection for {}", agent.with(|agent| agent.name.clone())))
            open=opened on_close=move || opened.set(false) locked=busy
            footer=kit::content(move || view! { <button type="submit" form="guide-connection-form" class="k-btn k-btn--md k-btn--primary"
                disabled=move || busy.get() || pending.get() || selected.with(String::is_empty)>"Use connection"</button> })>
            <form class="k-stack" id="guide-connection-form" on:submit=save>
                <Show when=move || !options.with(Vec::is_empty)>
                    <label class="k-field"><span class="k-field__label">"Run with"</span>
                        <span class="connection-select">
                            <select class="k-input k-select" disabled=busy on:change=move |event| selected.set(event_target_value(&event))>
                                {move || options.get().into_iter().map(|choice| {
                                    let value = choice.value.clone();
                                    view! { <option prop:selected=move || selected.with(|selected| selected == &value) value=choice.value>{choice.label}</option> }
                                }).collect_view()}
                            </select>
                            <span class="k-select__chev">{kit::icon(chevron.clone())}</span>
                        </span>
                    </label>
                </Show>
                <p role="status" hidden=move || !options.with(Vec::is_empty)>{move || if pending.get() { "Checking AI connections…" } else { "No AI connection available." }}</p>
                <Show when=move || !error.with(String::is_empty)><kit::Alert tone="error" actions=kit::content(move || view! {
                    <kit::Button label=kit::text("Retry") on_click=load />
                })>{error}</kit::Alert></Show>
                <Show when=move || !saving.with(String::is_empty)><kit::Alert tone="error">{saving}</kit::Alert></Show>
                <div class="k-row-actions">
                    <a class="k-btn k-btn--md k-btn--secondary" href=settings::section_url("/settings/providers", &from())>"Providers"</a>
                </div>
            </form>
        </kit::Dialog>
    }
}
