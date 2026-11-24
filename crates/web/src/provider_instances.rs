use crate::{api, connection_ui::*};
use leptos::{prelude::*, task::spawn_local};
use serde::Deserialize;
use serde_json::{json, Value};

const PATH: &str = "/api/provider-instances";

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Instance {
    id: String,
    kind: String,
    display_name: String,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    installed: bool,
    version: Option<String>,
    status: String,
    auth: Auth,
    #[serde(default)]
    models: Vec<Value>,
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
    revision: u64,
}

#[derive(Clone, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
struct Environment {
    name: String,
    #[serde(default)]
    secret: bool,
    #[serde(default)]
    value_redacted: bool,
}

#[derive(Clone, Deserialize, PartialEq, Default)]
struct Auth {
    status: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Clone, Deserialize, PartialEq, Default)]
struct Envelope {
    providers: Vec<Instance>,
}

pub fn panel() -> impl IntoView {
    let providers = query::<Envelope>(|| PATH.into());
    let selected = RwSignal::new(None::<String>);
    let adding = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let open_add = UiAction::new(move |()| {
        error.set(String::new());
        adding.set(true);
    });
    let close_add = UiAction::new(move |()| adding.set(false));
    let current = move || {
        let selected_id = selected.get();
        providers.data.with(|data| {
            data.as_ref().and_then(|data| {
                selected_id
                    .as_deref()
                    .and_then(|id| data.providers.iter().find(|provider| provider.id == id))
                    .cloned()
                    .or_else(|| data.providers.first().cloned())
            })
        })
    };
    view! {
        <div class="k-page bs-root provider-instances-page">
            <Header title="Providers".to_owned() actions=Box::new(move || view! {
                <Button label="Add provider" variant="primary" icon="plus" click=open_add.clone()
                    disabled=move || providers.busy.get() />
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
                            key=|provider| provider.id.clone()
                            children=move |provider| {
                                let id = provider.id.clone();
                                let active_id = id.clone();
                                let active = move || selected.with(|value| value.as_deref() == Some(active_id.as_str()));
                                view! {
                                    <button class="provider-instance-row" class:is-active=active
                                        on:click=move |_| selected.set(Some(id.clone()))>
                                        <span class="provider-instance-row__icon"><Icon name="server" /></span>
                                        <span class="provider-instance-row__main">
                                            <strong>{provider.display_name.clone()}</strong>
                                            <small>{provider.version.clone().unwrap_or_else(|| provider.kind.clone())}</small>
                                            <small>{if provider.auth.status == "authenticated" { format!("Authenticated · {}", provider.auth.method.clone().unwrap_or_default()) } else if provider.installed { provider.auth.reason.clone().unwrap_or_else(|| "Not authenticated".into()) } else { "Not installed".into() }}</small>
                                        </span>
                                        <span class="provider-instance-row__toggle" aria-hidden="true">{if provider.enabled { "On" } else { "Off" }}</span>
                                    </button>
                                }
                            } />
                    </nav>
                    <Show when=move || current().is_some() fallback=|| view! { <Empty title="Choose a provider" icon="server" /> }>
                        {move || current().map(detail)}
                    </Show>
                </div>
            </Show>
            <Show when=move || adding.get()>
                <AddProvider close=close_add.clone() saved=move || { providers.reload.run(()); adding.set(false); } error=error />
            </Show>
        </div>
    }
}

fn detail(provider: Instance) -> AnyView {
    let status = if provider.installed { provider.status.clone() } else { "Not installed".into() };
    view! {
        <main class="k-card provider-instance-detail">
            <div class="provider-instance-detail__head">
                <div><h2>{provider.display_name.clone()}</h2><p>{provider.kind.clone()}</p></div>
                <Badge success=provider.enabled>{if provider.enabled { "Enabled" } else { "Disabled" }}</Badge>
            </div>
            <Section title="Status" actions=Box::new(|| ().into_any())>
                <Row title="Status" trailing=Box::new(move || view! { <span>{status.clone()}</span> }.into_any()) />
                <Row title="Authentication" trailing=Box::new(move || view! { <span>{if provider.auth.status == "authenticated" { format!("Authenticated · {}", provider.auth.method.clone().unwrap_or_default()) } else { provider.auth.reason.clone().unwrap_or_else(|| "Not authenticated".into()) }}</span> }.into_any()) />
                <Row title="Models" trailing=Box::new(move || view! { <span>{provider.models.len()}</span> }.into_any()) />
            </Section>
            <Section title="Configuration" actions=Box::new(|| ().into_any())>
                <Row title="Display name" trailing=Box::new(move || view! { <span>{provider.display_name.clone()}</span> }.into_any()) />
            </Section>
            <Section title="Runtime" actions=Box::new(|| ().into_any())>
                <Row title="Binary path" trailing=Box::new(move || view! { <code>{provider.binary_path.clone()}</code> }.into_any()) />
                <Row title=if provider.kind == "codex" { "CODEX_HOME" } else { "Config directory" } trailing=Box::new(move || view! { <code>{provider.home_path.clone()}</code> }.into_any()) />
                {(!provider.shadow_home_path.is_empty()).then(|| view! { <Row title="Shadow home path" trailing=Box::new(move || view! { <code>{provider.shadow_home_path.clone()}</code> }.into_any()) /> })}
                <Row title="Launch arguments" trailing=Box::new(move || view! { <code>{provider.launch_args.join(" ")}</code> }.into_any()) />
            </Section>
            <Section title="Environment" actions=Box::new(|| view! { <Button label="Add variable" /> }.into_any())>
                {provider.environment.into_iter().map(|variable| view! {
                    <Row title=variable.name.clone() trailing=Box::new(move || view! { <code>{if variable.secret || variable.value_redacted { "••••••" } else { "" }}</code> }.into_any()) />
                }).collect_view()}
            </Section>
        </main>
    }.into_any()
}

#[component]
fn AddProvider(
    close: UiAction,
    saved: impl Fn() + Copy + Send + 'static,
    error: RwSignal<String>,
) -> impl IntoView {
    let step = RwSignal::new(0u8);
    let kind = RwSignal::new("codex".to_owned());
    let busy = RwSignal::new(false);
    let name = RwSignal::new(String::new());
    let submit = move |event: web_sys::SubmitEvent| {
        let Some(fields) = form(&event) else { return };
        if busy.get_untracked() { return; }
        name.set(field(&fields, "displayName"));
        let instance = field(&fields, "instanceId");
        let kind_value = kind.get_untracked();
        if instance.trim().is_empty() { error.set("Instance ID is required.".into()); return; }
        busy.set(true);
        spawn_local(async move {
            let result = api::post::<Value>(PATH, &json!({"id":uuid(),"instanceId":instance,"kind":kind_value,"displayName":name.get_untracked()})).await;
            busy.set(false);
            match result { Ok(_) => saved(), Err(message) => error.set(message) }
        });
    };
    view! {
        <Modal title="Add provider".to_owned() description="Add an account or configure a provider on this machine".to_owned() busy=busy close=close.clone()
            footer=Box::new(move || view! {
                <Button label="Cancel" disabled=move || busy.get() click=close.clone() />
                <Show when=move || step.get() < 2>
                    <Button label="Continue" variant="primary" click=UiAction::new(move |()| step.update(|step| *step += 1)) />
                </Show>
                <Show when=move || step.get() == 2>
                    <Button label="Configure manually" submit=true form="provider-instance-form" busy=busy />
                    <Button label="Sign in" variant="primary" submit=true form="provider-instance-form" busy=busy />
                </Show>
            }.into_any())>
            <form id="provider-instance-form" class="k-stack" on:submit=submit>
                <ol class="provider-wizard-stepper" aria-label="Provider setup steps">
                    <li class:active=move || step.get() == 0>"1 Provider"</li>
                    <li class:active=move || step.get() == 1>"2 Identity"</li>
                    <li class:active=move || step.get() == 2>"3 Config"</li>
                </ol>
                <Show when=move || step.get() == 0>
                    <Field label="Provider" name="kind"><Select name="kind" value=Signal::derive(move || kind.get())
                        options=Signal::derive(|| ["codex","claude","hermes","antigravity","grok","pi","opencode","gemini","kiro"].into_iter().map(|kind| (kind.into(), kind.into())).collect())
                        change=Callback::new(move |value| kind.set(value)) /></Field>
                </Show>
                <Show when=move || step.get() == 1>
                    <Field label="Display name" name="displayName"><input class="k-input" name="displayName" required placeholder="My Codex" /></Field>
                    <Field label="Instance ID" name="instanceId"><input class="k-input" name="instanceId" required placeholder="codex-personal" /></Field>
                    <p class="k-field__hint">"Sign-in runs inside this provider instance home."</p>
                </Show>
                <Show when=move || step.get() == 2>
                    <p>Configure manually, then refresh status from provider details.</p>
                </Show>
            </form>
        </Modal>
    }
}
