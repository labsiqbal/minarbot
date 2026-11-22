use crate::{api, connection_ui::*};
use leptos::{prelude::*, task::spawn_local};
use serde::Deserialize;
use serde_json::json;
use wasm_bindgen::JsCast;

const PATH: &str = "/api/email/settings";

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Settings {
    domain: Option<String>,
    has_secret: bool,
    revision: u64,
    endpoint: String,
}

#[derive(Deserialize)]
struct Saved {
    #[serde(flatten)]
    settings: Settings,
    secret: Option<String>,
}

pub fn panel() -> impl IntoView {
    let settings = query::<Settings>(|| PATH.into());
    let domain = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let notice = RwSignal::new("");
    let secret = RwSignal::new(None::<String>);
    let confirmation = Confirm::new(None);
    Effect::new(move |_| {
        domain.set(
            settings
                .data
                .get()
                .and_then(|settings| settings.domain)
                .unwrap_or_default(),
        )
    });
    let save = move |rotate: bool| {
        let Some(settings_now) = settings.data.get_untracked() else {
            return;
        };
        if busy.get_untracked() || settings.busy.get_untracked() {
            return;
        }
        let body = if rotate {
            json!({ "revision": settings_now.revision, "rotateSecret": true })
        } else {
            json!({ "revision": settings_now.revision, "domain": domain.get_untracked().trim() })
        };
        busy.set(true);
        error.set(String::new());
        notice.set("");
        spawn_local(async move {
            match api::command::<Saved>(PATH, body).await {
                Ok(saved) => {
                    settings.data.try_set(Some(saved.settings));
                    if let Some(value) = saved.secret {
                        secret.try_set(Some(value));
                    } else {
                        notice.try_set("Receiving domain saved.");
                    }
                }
                Err(message) => {
                    error.try_set(message);
                }
            }
            busy.try_set(false);
        });
    };
    let reload = UiAction::new(move |()| {
        error.set(String::new());
        settings.reload.run(());
    });
    let close = UiAction::new(move |()| {
        notice.set("");
        secret.set(None);
    });
    let unavailable = move || busy.get() || settings.busy.get();
    view! {
        <div class="k-page bs-root agent-email-settings">
            <Header title="Email".to_owned() />
            <ErrorNotice error=Signal::derive(move || {
                let error = error.get();
                if error.is_empty() { settings.error.get() } else { error }
            }) retry=reload label="Reload settings" />
            <Show when=move || settings.data.with(Option::is_none) && settings.error.with(String::is_empty)>
                <Loading label="Loading email settings…" />
            </Show>
            <Show when=move || settings.data.with(Option::is_some)>
                <form class="agent-email-settings__form" on:submit=move |event| { event.prevent_default(); save(false); }>
                    <Field label="Receiving domain" name="email-domain">
                        <input class="k-input" id="email-domain" name="domain" maxlength="253" placeholder="agents.example.com"
                            autocomplete="off" autocapitalize="none" spellcheck="false" disabled=unavailable
                            prop:value=domain on:input=move |event| domain.set(event_target_value(&event)) />
                    </Field>
                    <Button label="Save domain" variant="primary" submit=true disabled=move || unavailable() ||
                        settings.data.with(|data| data.as_ref().and_then(|data| data.domain.as_deref()).unwrap_or("") == domain.with(|value| value.trim().to_owned())) />
                </form>
                <span class="settings-fact" role="status" hidden=move || notice.get().is_empty() || secret.with(Option::is_some)>{notice}</span>
                {move || settings.data.get().map(|data| {
                    let rotate = UiAction::new(move |()| {
                        if data.has_secret {
                            confirm(confirmation, "Rotate endpoint secret?", "Update your relay with the new secret to resume email delivery.",
                                "Rotate secret", true, UiAction::new(move |()| save(true)));
                        } else { save(true); }
                    });
                    let ready = data.domain.is_some_and(|value| !value.is_empty());
                    view! {
                        <Section title="Inbound relay">
                            <div class="k-card">
                                <Row title="Endpoint path" meta=Box::new(move || view! { <code>{data.endpoint}</code> }.into_any()) />
                                <Row title="Endpoint secret" meta=Box::new(move || if data.has_secret { "Configured" } else { "Not generated" }.into_any())
                                    trailing=Box::new(move || view! {
                                        <Button label=if data.has_secret { "Rotate secret" } else { "Generate secret" }
                                            size="sm" disabled=move || unavailable() || !ready click=rotate />
                                    }.into_any()) />
                            </div>
                        </Section>
                    }
                })}
            </Show>
            {move || secret.with(|value| value.is_some()).then(|| view! {
                <Modal title="Save endpoint secret".to_owned()
                    description="Shown once. Copy it to your relay before closing." close=close footer=Box::new(move || view! {
                        <Button label="Done" variant="primary" click=close />
                    }.into_any())>
                    <Field label="Endpoint secret" name="email-secret">
                        <div class="k-inline">
                            <input class="k-input" id="email-secret" data-private-token="" readonly autocomplete="off" spellcheck="false"
                                prop:value=move || secret.get().unwrap_or_default()
                                on:focus=move |event| {
                                    if let Some(input) = event.target().and_then(|el| el.dyn_into::<web_sys::HtmlInputElement>().ok()) { input.select(); }
                                } />
                            <button type="button" class="k-icon-btn" aria-label="Copy endpoint secret" on:click=move |_| {
                                if let Some(value) = secret.get_untracked() {
                                    spawn_local(async move {
                                        let ok = copy(&value).await.is_ok();
                                        if secret.try_with(Option::is_some) == Some(true) {
                                            notice.set(if ok { "Copied." } else { "Select the secret to copy it." });
                                        }
                                    });
                                }
                            }><Icon name="copy" /></button>
                        </div>
                    </Field>
                    <span role="status">{notice}</span>
                </Modal>
            })}
            <ConfirmDialog state=confirmation />
        </div>
    }
}
