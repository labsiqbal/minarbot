use crate::{api, connection_ui::*};
use crate::provider_icons::ProviderMark;
use leptos::{prelude::*, task::spawn_local};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const PATH: &str = "/api/provider-instances";

#[derive(Clone, Deserialize, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
struct ModelOption {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    values: Vec<String>,
}

#[derive(Clone, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
struct Model {
    id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    options: Vec<ModelOption>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
struct Environment {
    name: String,
    #[serde(default)]
    value: String,
    #[serde(default)]
    secret: bool,
    #[serde(default)]
    value_redacted: bool,
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Auth {
    status: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

impl Default for Auth {
    fn default() -> Self {
        Self { status: "unknown".into(), method: None, reason: None }
    }
}

#[derive(Clone, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
struct Instance {
    id: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    installed: bool,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    status: String,
    #[serde(default)]
    auth: Auth,
    #[serde(default)]
    models: Vec<Model>,
    #[serde(default)]
    binary_path: String,
    #[serde(default)]
    home_path: String,
    #[serde(default)]
    shadow_home_path: String,
    #[serde(default)]
    launch_args: Vec<String>,
    #[serde(default)]
    environment: Vec<Environment>,
    #[serde(default)]
    revision: u64,
}

#[derive(Clone, Deserialize, PartialEq, Default)]
struct Envelope {
    providers: Vec<Instance>,
}

#[derive(Clone, Deserialize, Default, PartialEq)]
struct MachineEnvironment {
    #[serde(default)]
    name: String,
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "codex" => "Codex",
        "claude" => "Claude",
        "hermes" => "Hermes",
        "antigravity" => "Antigravity",
        "grok" => "Grok",
        "pi" => "Pi",
        "opencode" => "OpenCode",
        "gemini" => "Gemini",
        "kiro" => "Kiro",
        _ => "Provider",
    }
}

fn status_line(provider: &Instance) -> String {
    if !provider.enabled {
        return "Disabled".into();
    }
    if provider.auth.status == "authenticated" {
        return format!("Authenticated · {}", provider.auth.method.clone().unwrap_or_else(|| kind_label(&provider.kind).into()));
    }
    if provider.installed {
        return format!("Not authenticated · {}", provider.auth.reason.clone().unwrap_or_else(|| "Sign in required".into()));
    }
    "Not installed".into()
}

fn slug(value: &str) -> String {
    let mut result = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            result.push(ch.to_ascii_lowercase());
        } else if !result.ends_with('-') {
            result.push('-');
        }
    }
    let result = result.trim_matches('-');
    if result.is_empty() { "provider-instance".into() } else { result.chars().take(64).collect() }
}

pub fn panel() -> impl IntoView {
    let providers = query::<Envelope>(|| PATH.into());
    let selected = RwSignal::new(None::<String>);
    let adding = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let checked = RwSignal::new("Checked just now".to_owned());
    // Lives with the panel: a reload after Save rebuilds the detail view.
    let saved = RwSignal::new(String::new());

    Effect::new(move |_| {
        if selected.get_untracked().is_none() {
            if let Some(id) = providers.data.with_untracked(|data| data.as_ref().and_then(|data| data.providers.first().map(|provider| provider.id.clone()))) {
                selected.set(Some(id));
            }
        }
    });
    let refresh = UiAction::new(move |()| {
        checked.set("Checking now…".into());
        providers.reload.run(());
        checked.set("Checked just now".into());
    });
    let current = move || providers.data.with(|data| data.as_ref().and_then(|data| {
        selected.get().as_deref().and_then(|id| data.providers.iter().find(|provider| provider.id == id)).cloned()
            .or_else(|| data.providers.first().cloned())
    }));
    let close_add = UiAction::new(move |()| adding.set(false));
    view! {
        <div class="k-page bs-root provider-instances-page">
            <Header title="Providers" actions=Box::new(move || view! {
                <span class="provider-check-status" role="status"><Icon name="refresh" />{checked}</span>
                <Button label="Add provider" variant="primary" icon="plus" click=UiAction::new(move |()| adding.set(true)) />
                <Button label="Refresh providers" aria="Refresh providers" icon="refresh" click=refresh.clone() busy=providers.busy />
            }.into_any()) />
            <ErrorNotice error=error.into() />
            <ErrorNotice error=providers.error.into() retry=providers.reload />
            <Show when=move || providers.data.with(Option::is_none) && providers.error.with(String::is_empty)>
                <Loading label="Loading providers…" />
            </Show>
            <Show when=move || providers.data.with(Option::is_some)>
                <div class="provider-instances-layout">
                    <nav class="k-card provider-instances-list" aria-label="Providers">
                        <For each=move || providers.data.with(|data| data.clone().unwrap_or_default().providers)
                            key=|provider| (provider.id.clone(), provider.revision, status_line(provider))
                            children=move |provider| {
                                let id = provider.id.clone();
                                let active_id = id.clone();
                                let active = move || selected.with(|value| value.as_deref() == Some(active_id.as_str()));
                                let active_for_button = active.clone();
                                let toggle_id = id.clone();
                                let toggle_provider = provider.clone();
                                view! {
                                    <div class="provider-instance-row" class:is-active=active>
                                        <button type="button" class="provider-instance-row__select" aria-current=move || active_for_button().then_some("true")
                                            on:click=move |_| { saved.set(String::new()); selected.set(Some(id.clone())); }>
                                            <span class="provider-instance-row__icon"><ProviderMark kind=provider.kind.clone() /></span>
                                            <span class="provider-instance-row__main">
                                                <strong>{provider.display_name.clone()}</strong>
                                                <small>{provider.version.clone().unwrap_or_else(|| kind_label(&provider.kind).into())}</small>
                                                <small>{status_line(&provider)}</small>
                                            </span>
                                        </button>
                                        <Switch label=format!("Enable {}", provider.display_name) checked=Signal::stored(provider.enabled)
                                            disabled=providers.busy change=Callback::new(move |enabled| {
                                                let body = json!({"instanceId":toggle_id,"revision":toggle_provider.revision,"enabled":enabled});
                                                let reload = providers.reload;
                                                let error = error;
                                                let id = toggle_provider.id.clone();
                                                spawn_local(async move { if let Err(message) = api::command::<Value>(&format!("{PATH}/{id}/update"), body).await { error.set(message); } reload.run(()); });
                                            }) />
                                    </div>
                                }
                            } />
                    </nav>
                    <Show when=move || current().is_some() fallback=|| view! { <Empty title="Choose a provider" icon="server" /> }>
                        {move || current().map(|provider| detail(provider, providers.reload, error, saved))}
                    </Show>
                </div>
            </Show>
            <Show when=move || adding.get()>
                <AddProvider close=close_add.clone() reload=providers.reload saved=move || { providers.reload.run(()); adding.set(false); } />
            </Show>
        </div>
    }
}

fn detail(provider: Instance, reload: UiAction, error: RwSignal<String>, saved: RwSignal<String>) -> AnyView {
    let name = RwSignal::new(provider.display_name.clone());
    let binary = RwSignal::new(provider.binary_path.clone());
    let home = RwSignal::new(provider.home_path.clone());
    let shadow = RwSignal::new(provider.shadow_home_path.clone());
    let args = RwSignal::new(provider.launch_args.join("\n"));
    let environment = RwSignal::new(provider.environment.clone());
    let busy = RwSignal::new(false);
    let confirm_open = RwSignal::new(false);
    let save_id = provider.id.clone();
    let provider_id = provider.id.clone();
    let provider_kind = provider.kind.clone();
    let provider_kind_view = provider.kind.clone();
    let provider_revision = provider.revision;
    let provider_enabled = provider.enabled;
    let display_name = provider.display_name.clone();
    let version = provider.version.clone();
    let shadow_path = provider.shadow_home_path.clone();
    let auth_status = provider.auth.status.clone();
    let save = UiAction::new(move |()| {
        if busy.get_untracked() { return; }
        busy.set(true);
        let body = json!({
            "instanceId": save_id.clone(),
            "revision": provider_revision,
            "kind": provider_kind.clone(),
            "displayName": name.get_untracked().trim(),
            "enabled": provider_enabled,
            "binaryPath": binary.get_untracked().trim(),
            "homePath": home.get_untracked().trim(),
            "shadowHomePath": shadow.get_untracked().trim(),
            "launchArgs": args.get_untracked().lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>(),
            "environment": environment.get_untracked(),
        });
        let reload = reload;
        let error = error;
        let request_id = save_id.clone();
        spawn_local(async move {
            match api::command::<Instance>(&format!("{PATH}/{request_id}/update"), body).await {
                Ok(_) => { saved.set("Saved".into()); reload.run(()); }
                Err(message) => error.set(message),
            }
            busy.set(false);
        });
    });
    let sign_in_id = provider_id.clone();
    let sign_in = UiAction::new(move |()| {
        busy.set(true);
        let id = sign_in_id.clone();
        let reload = reload;
        let error = error;
        spawn_local(async move {
            match api::command::<Value>(&format!("{PATH}/{id}/sign-in"), json!({"instanceId":id})).await {
                Ok(_) => { saved.set("Sign-in started".into()); reload.run(()); }
                Err(message) => error.set(message),
            }
            busy.set(false);
        });
    });
    let refresh_id = provider_id.clone();
    let refresh = UiAction::new(move |()| {
        busy.set(true);
        let id = refresh_id.clone();
        let reload = reload;
        let error = error;
        spawn_local(async move {
            let result = api::command::<Value>(&format!("{PATH}/{id}/refresh"), json!({"instanceId":id})).await;
            if let Err(message) = result { error.set(message); } else { saved.set("Checked just now".into()); reload.run(()); }
            busy.set(false);
        });
    });
    let delete_id = provider_id.clone();
    let delete_request = UiAction::new(move |()| {
        let id = delete_id.clone();
        let reload = reload;
        let error = error;
        confirm_open.set(false);
        spawn_local(async move {
            let request_id = id;
            match api::command::<Value>(&format!("{PATH}/{request_id}/delete"), json!({"instanceId":request_id,"revision":provider_revision})).await {
                Ok(_) => reload.run(()),
                Err(message) => error.set(message),
            }
        });
    });
    let delete = UiAction::new(move |()| confirm_open.set(true));
    let status = status_line(&provider);
    let auth_reason = provider.auth.reason.clone().unwrap_or_default();
    let kind_for_button = provider_kind_view.clone();
    view! {
        <main class="k-card provider-instance-detail">
            <div class="provider-instance-detail__head">
                <div class="provider-instance-title"><span class="provider-instance-detail__icon"><ProviderMark kind=provider_kind_view.clone() /></span><div><h2>{display_name.clone()}</h2><p>{kind_label(&provider_kind_view)}{version.clone().map(|version| format!(" · v{version}"))}</p></div></div>
                <Badge success=provider_enabled>{if provider_enabled { "Enabled" } else { "Disabled" }}</Badge>
            </div>
            <Section title="Status" actions=Box::new(move || view! { <div class="provider-detail-actions"><Button label="Re-check" icon="refresh" busy=busy click=refresh.clone() /><Button label="Sign in" icon="key" busy=busy disabled=move || kind_for_button != "codex" && kind_for_button != "claude" click=sign_in.clone() /></div> }.into_any())>
                <div class="provider-status-card"><span class="provider-status-dot" class:is-ready=auth_status == "authenticated" && provider_enabled></span><div><strong>{status}</strong>{(!auth_reason.is_empty()).then(|| view! { <p>{auth_reason.clone()}</p> })}</div></div>
            </Section>
            <form class="provider-detail-form" on:submit=move |event| { event.prevent_default(); save.run(()); }>
                <Section title="Configuration">
                    <label class="provider-field"><span>Display name</span><input class="k-input" name="displayName" prop:value=name on:input=move |event| name.set(event_target_value(&event)) /></label>
                </Section>
                <Section title="Runtime">
                    <label class="provider-field"><span>Binary path</span><input class="k-input" name="binaryPath" prop:value=binary on:input=move |event| binary.set(event_target_value(&event)) /></label>
                    <label class="provider-field"><span>{if provider_kind_view == "codex" { "CODEX_HOME path" } else { "Config directory" }}</span><input class="k-input" name="homePath" prop:value=home on:input=move |event| home.set(event_target_value(&event)) /></label>
                    {(shadow_path != "" || provider_kind_view == "codex").then(|| view! { <label class="provider-field"><span>Shadow home path</span><input class="k-input" name="shadowHomePath" prop:value=shadow on:input=move |event| shadow.set(event_target_value(&event)) /></label> })}
                    <label class="provider-field"><span>Launch arguments</span><textarea class="k-input k-textarea provider-args" name="launchArgs" prop:value=args on:input=move |event| args.set(event_target_value(&event)) placeholder="One argument per line"></textarea></label>
                </Section>
                <Section title="Environment" actions=Box::new(move || view! { <Button label="Add variable" icon="plus" click=UiAction::new(move |()| environment.update(|rows| rows.push(Environment { name: format!("VARIABLE_{}", rows.len() + 1), ..Environment::default() }))) /> }.into_any())>
                    <div class="provider-environment-list">
                        <For each=move || { environment.get().into_iter().enumerate().collect::<Vec<_>>() } key=|(index, row)| format!("{index}-{}", row.name) children=move |(index, row)| {
                            let secret = row.secret || row.value_redacted;
                            view! { <div class="provider-env-row">
                                <input class="k-input provider-env-name" aria-label="Variable name" prop:value=row.name.clone() on:input=move |event| environment.update(|rows| if let Some(item) = rows.get_mut(index) { item.name = event_target_value(&event); }) />
                                <input class="k-input provider-env-value" aria-label=format!("Value for {}", row.name) type=if secret { "password" } else { "text" } prop:value=if secret { String::new() } else { row.value.clone() } placeholder=secret.then_some("Secret value saved") on:input=move |event| environment.update(|rows| if let Some(item) = rows.get_mut(index) { item.value = event_target_value(&event); item.value_redacted = false; }) />
                                <label class="provider-env-secret"><input type="checkbox" prop:checked=secret on:change=move |event| environment.update(|rows| if let Some(item) = rows.get_mut(index) { item.secret = event_target_checked(&event); item.value_redacted = item.secret; }) />"Secret"</label>
                                <Button label="Remove" aria="Remove variable" icon="trash" click=UiAction::new(move |()| environment.update(|rows| { if index < rows.len() { rows.remove(index); } })) />
                            </div> }
                        } />
                    </div>
                    <Show when=move || environment.with(|rows| rows.is_empty())><p class="k-field__hint">No environment variables configured.</p></Show>
                </Section>
                <div class="provider-detail-footer"><span role="status">{saved}</span><Button label="Save changes" variant="primary" submit=true busy=busy /><Button label="Delete instance" variant="danger" click=delete disabled=busy /></div>
            </form>
            <Show when=move || confirm_open.get()>
                <Modal title="Delete provider instance?".to_owned() description="Agents using this instance will need another provider before they can run.".to_owned() confirm=true close=UiAction::new(move |()| confirm_open.set(false))
                    footer=Box::new(move || view! { <Button label="Cancel" click=UiAction::new(move |()| confirm_open.set(false)) /><Button label="Delete instance" variant="danger" click=delete_request.clone() /> }.into_any()) />
            </Show>
        </main>
    }.into_any()
}

/// The Add provider wizard, shared by Settings > Providers and setup.
/// `reload` runs once the instance exists, `saved` after a manual setup.
#[component]
pub(crate) fn AddProvider(close: UiAction, reload: UiAction, saved: impl Fn() + Copy + Send + 'static) -> impl IntoView {
    // Shown inside the modal: the page behind it sits under the backdrop.
    let error = RwSignal::new(String::new());
    let machine = query::<MachineEnvironment>(|| "/api/environment".into());
    let step = RwSignal::new(0u8);
    let kind = RwSignal::new("codex".to_owned());
    let name = RwSignal::new(String::new());
    let instance_id = RwSignal::new(String::new());
    let binary = RwSignal::new(String::new());
    let home = RwSignal::new(String::new());
    let shadow = RwSignal::new(String::new());
    let args = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let sign_in_status = RwSignal::new(String::new());
    let submit = move |event: web_sys::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() { return; }
        let display = name.get_untracked().trim().to_owned();
        let requested_id = instance_id.get_untracked();
        let id = slug(if requested_id.trim().is_empty() { &display } else { requested_id.trim() });
        if display.is_empty() { error.set("Display name is required.".into()); return; }
        let sign_in = event.submitter().and_then(|button| button.get_attribute("value")) == Some("signin".into());
        let kind_value = kind.get_untracked();
        let body = json!({"instanceId":id,"kind":kind_value,"displayName":display,"binaryPath":binary.get_untracked().trim(),"homePath":home.get_untracked().trim(),"shadowHomePath":shadow.get_untracked().trim(),"launchArgs":args.get_untracked().lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>(),"environment":[]});
        busy.set(true);
        error.set(String::new());
        sign_in_status.set(String::new());
        spawn_local(async move {
            let result = match api::command::<Instance>(PATH, body).await {
                Ok(_) if sign_in => {
                    // The provider exists now; show it even while sign-in runs.
                    reload.run(());
                    api::command::<Value>(&format!("{PATH}/{id}/sign-in"), json!({"instanceId":id})).await.map(|_| ())
                }
                Ok(_) => Ok(()),
                Err(message) => Err(message),
            };
            busy.set(false);
            match result {
                Ok(_) if sign_in => sign_in_status.set("Sign-in started. Finish in the provider home.".into()),
                Ok(_) => saved(),
                Err(message) => error.set(message),
            }
        });
    };
    let kinds = [("codex", "Codex"), ("claude", "Claude"), ("hermes", "Hermes"), ("antigravity", "Antigravity"), ("grok", "Grok"), ("pi", "Pi"), ("opencode", "OpenCode"), ("gemini", "Gemini"), ("kiro", "Kiro")];
    let sign_in_kind = move || matches!(kind.get().as_str(), "codex" | "claude");
    view! {
        <Modal title="Add provider".to_owned() size="lg" busy=busy close=close.clone()
            footer=Box::new(move || view! {
                <Button label="Cancel" disabled=busy click=close.clone() />
                <Show when=move || step.get() == 0><Button label="Continue" variant="primary" click=UiAction::new(move |()| step.update(|value| *value = (*value + 1).min(2))) /></Show>
                <Show when=move || step.get() == 1 && !sign_in_kind()><Button label="Continue" variant="primary" click=UiAction::new(move |()| step.update(|value| *value = (*value + 1).min(2))) /></Show>
                <Show when=move || step.get() == 1 && sign_in_kind()>
                    <Button label="Configure manually" submit=true form="provider-wizard-form" value="manual" busy=busy />
                    <button type="submit" class="k-btn k-btn--md k-btn--primary" form="provider-wizard-form" value="signin" disabled=move || busy.get()>{move || if kind.get() == "codex" { "Continue with ChatGPT" } else { "Sign in with Claude" }}</button>
                </Show>
                <Show when=move || step.get() == 2>
                    <Show when=sign_in_kind fallback=move || view! { <Button label="Configure manually" variant="primary" submit=true form="provider-wizard-form" value="manual" busy=busy /> }>
                        <Button label="Configure manually" submit=true form="provider-wizard-form" value="manual" busy=busy />
                        <button type="submit" class="k-btn k-btn--md k-btn--primary" form="provider-wizard-form" value="signin" disabled=move || busy.get()>{move || if kind.get() == "codex" { "Continue with ChatGPT" } else { "Sign in with Claude" }}</button>
                    </Show>
                </Show>
            }.into_any())>
            <form id="provider-wizard-form" class="k-stack provider-wizard" on:submit=submit>
                <p class="provider-wizard-subtitle">{move || format!("Add an account or configure a provider on {}", machine.data.with(|data| data.as_ref().map(|data| data.name.clone()).filter(|name| !name.is_empty()).unwrap_or_else(|| "this machine".into())))}</p>
                <ol class="provider-wizard-stepper" aria-label="Provider setup steps">
                    <li class:active=move || step.get() == 0><span>1</span>Provider</li><li class:active=move || step.get() == 1><span>2</span>Identity</li><li class:active=move || step.get() == 2><span>3</span>Config</li>
                </ol>
                <Show when=move || step.get() == 0>
                    <fieldset class="provider-kind-grid"><legend>Provider</legend>{kinds.into_iter().map(|(id, label)| view! {
                        <button type="button" class="provider-kind-tile" class:is-active=move || kind.get() == id on:click=move |_| kind.set(id.into())><ProviderMark kind=id.to_owned() /><span>{label}</span><Show when=move || kind.get() == id><Icon name="check" /></Show></button>
                    }).collect_view()}</fieldset>
                </Show>
                <Show when=move || step.get() == 1>
                    <label class="k-field"><span class="k-field__label">Display name</span><input class="k-input" name="displayName" required placeholder=move || format!("My {}", kind_label(&kind.get())) prop:value=name on:input=move |event| name.set(event_target_value(&event)) /></label>
                    <label class="k-field"><span class="k-field__label">Instance ID <span class="k-field__optional">Optional</span></span><input class="k-input" name="instanceId" placeholder="Generated from display name" prop:value=instance_id on:input=move |event| instance_id.set(event_target_value(&event)) /></label>
                </Show>
                <Show when=move || step.get() == 2>
                    <div class="k-grid-2"><label class="k-field"><span class="k-field__label">Binary path</span><input class="k-input" name="binaryPath" prop:value=binary on:input=move |event| binary.set(event_target_value(&event)) /></label><label class="k-field"><span class="k-field__label">{if kind.get() == "codex" { "CODEX_HOME path" } else { "Config directory" }}</span><input class="k-input" name="homePath" prop:value=home on:input=move |event| home.set(event_target_value(&event)) /></label></div>
                    <label class="k-field"><span class="k-field__label">Shadow home path <span class="k-field__optional">Optional</span></span><input class="k-input" name="shadowHomePath" prop:value=shadow on:input=move |event| shadow.set(event_target_value(&event)) /></label>
                    <label class="k-field"><span class="k-field__label">Launch arguments <span class="k-field__optional">Optional</span></span><textarea class="k-input k-textarea provider-args" name="launchArgs" prop:value=args on:input=move |event| args.set(event_target_value(&event)) placeholder="One argument per line"></textarea></label>
                </Show>
                <Show when=move || !sign_in_status.get().is_empty()><p class="provider-wizard-status" role="status">{sign_in_status}</p></Show>
                <ErrorNotice error=error.into() />
            </form>
        </Modal>
    }
}
