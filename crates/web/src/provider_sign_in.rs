//! Sign-in dialog for Codex and Claude provider instances. The CLI runs on
//! the Minarbot host; the dialog shows its sign-in link (and Codex's
//! one-time code), so the browser can be on any machine.

use crate::{api, connection_ui::*};
use leptos::{prelude::*, task::spawn_local};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

const PATH: &str = "/api/provider-instances";

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Started {
    url: String,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    needs_code: bool,
}

#[derive(Deserialize)]
struct Listed {
    providers: Vec<Row>,
}

#[derive(Deserialize)]
struct Row {
    id: String,
    #[serde(default)]
    auth: Auth,
}

#[derive(Deserialize, Default)]
struct Auth {
    #[serde(default)]
    status: String,
}

/// What a sign-in dialog signs in: instance id, kind and display name.
#[derive(Clone, PartialEq)]
pub(crate) struct Target {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) name: String,
}

#[component]
pub(crate) fn SignIn(target: Target, close: UiAction, signed_in: UiAction) -> impl IntoView {
    let started = RwSignal::new(None::<Started>);
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let code = RwSignal::new(String::new());
    let copied = RwSignal::new(false);
    let attempt = RwSignal::new(0u32);
    let id = StoredValue::new(target.id.clone());
    let start = move || {
        started.set(None);
        error.set(String::new());
        code.set(String::new());
        attempt.update(|value| *value += 1);
        let request = attempt.get_untracked();
        spawn_local(async move {
            let path = format!("{PATH}/{}/sign-in", id.get_value());
            let result =
                api::command::<Started>(&path, json!({"instanceId": id.get_value()})).await;
            if attempt.try_get_untracked() != Some(request) {
                return;
            }
            match result {
                Ok(value) => started.set(Some(value)),
                Err(message) => error.set(message),
            }
        });
    };
    start();
    // Codex finishes on the sign-in page: watch the instance until it is in.
    let checking = StoredValue::new(false);
    if let Ok(timer) = set_interval_with_handle(
        move || {
            let waiting = started
                .with_untracked(|value| value.as_ref().is_some_and(|value| !value.needs_code));
            if !waiting || checking.get_value() {
                return;
            }
            checking.set_value(true);
            spawn_local(async move {
                if let Ok(list) = api::read::<Listed>(PATH).await {
                    let done = list
                        .providers
                        .iter()
                        .any(|row| row.id == id.get_value() && row.auth.status == "authenticated");
                    if done && started.try_get_untracked().flatten().is_some() {
                        started.set(None);
                        signed_in.run(());
                    }
                }
                let _ = checking.try_set_value(false);
            });
        },
        Duration::from_secs(2),
    ) {
        on_cleanup(move || timer.clear());
    }
    let submit = move |event: web_sys::SubmitEvent| {
        event.prevent_default();
        let value = code.get_untracked().trim().to_owned();
        if busy.get_untracked() || value.is_empty() {
            return;
        }
        busy.set(true);
        error.set(String::new());
        spawn_local(async move {
            let path = format!("{PATH}/{}/sign-in-code", id.get_value());
            let result = api::command::<serde_json::Value>(
                &path,
                json!({"instanceId": id.get_value(), "code": value}),
            )
            .await;
            let _ = busy.try_set(false);
            match result {
                Ok(_) => signed_in.run(()),
                Err(message) => {
                    // The CLI exits on a rejected code; the next try needs a new link.
                    started.set(None);
                    error.set(message);
                }
            }
        });
    };
    let copy = move |_| {
        if let Some(value) =
            started.with_untracked(|value| value.as_ref().and_then(|value| value.code.clone()))
        {
            let _ = window().navigator().clipboard().write_text(&value);
            copied.set(true);
            set_timeout(
                move || {
                    let _ = copied.try_set(false);
                },
                Duration::from_secs(2),
            );
        }
    };
    let url = move || {
        started.with(|value| {
            value
                .as_ref()
                .map(|value| value.url.clone())
                .unwrap_or_default()
        })
    };
    let device_code =
        move || started.with(|value| value.as_ref().and_then(|value| value.code.clone()));
    let needs_code =
        move || started.with(|value| value.as_ref().is_some_and(|value| value.needs_code));
    view! {
        <Modal title=format!("Sign in to {}", target.name) busy=busy close=close
            footer=Box::new(move || view! {
                <Button label="Cancel" disabled=busy click=close />
                <Show when=needs_code>
                    <Button label="Sign in" variant="primary" submit=true form="provider-sign-in-form" busy=busy disabled=move || code.with(|code| code.trim().is_empty()) />
                </Show>
            }.into_any())>
            <div class="k-stack provider-sign-in">
                <Show when=move || started.with(Option::is_none) && error.with(String::is_empty)>
                    <p class="provider-sign-in__waiting" role="status"><span class="k-spinner" aria-hidden="true"></span>"Starting sign-in…"</p>
                </Show>
                <Show when=move || started.with(Option::is_some)>
                    <ol class="provider-sign-in__steps">
                        <li>
                            <span>"Open the sign-in page"</span>
                            <a class="k-btn k-btn--md k-btn--secondary" href=url target="_blank" rel="noopener noreferrer">"Open"</a>
                        </li>
                        <Show when=move || device_code().is_some()>
                            <li>
                                <span>"Enter this code"</span>
                                <span class="provider-sign-in__code-row">
                                    <code class="provider-sign-in__code">{device_code}</code>
                                    <button type="button" class="k-btn k-btn--md k-btn--secondary" on:click=copy>{move || if copied.get() { "Copied" } else { "Copy" }}</button>
                                </span>
                            </li>
                        </Show>
                        <Show when=needs_code>
                            <li>
                                <label for="provider-sign-in-code">"Paste the code it shows"</label>
                                <form id="provider-sign-in-form" on:submit=submit>
                                    <input id="provider-sign-in-code" class="k-input" autocomplete="off" spellcheck="false" readonly=busy
                                        prop:value=code on:input=move |event| code.set(event_target_value(&event)) />
                                </form>
                            </li>
                        </Show>
                    </ol>
                    <Show when=move || !needs_code()>
                        <p class="provider-sign-in__waiting" role="status"><span class="k-spinner" aria-hidden="true"></span>"Waiting for sign-in…"</p>
                    </Show>
                </Show>
                <ErrorNotice error=error.into() retry=UiAction::new(move |()| start()) label="Start again" />
            </div>
        </Modal>
    }
}
