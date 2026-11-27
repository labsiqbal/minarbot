use crate::api;
use leptos::prelude::*;
use serde::Deserialize;
use serde_json::json;
use web_sys::{Notification, NotificationPermission};

const CHATS: &str = "/api/conversations/settings";
const NOTIFICATIONS: &str = "minarbot:workflow-notifications-muted";

#[component]
pub(crate) fn Section(title: &'static str, children: Children) -> impl IntoView {
    view! {
        <section class="k-section">
            <div class="k-section__head"><h2 class="k-section__title">{title}</h2></div>
            {children()}
        </section>
    }
}

#[component]
pub(crate) fn Row(title: &'static str, children: Children) -> impl IntoView {
    view! {
        <div class="k-row">
            <div class="k-row__main"><span class="k-row__text"><span class="k-row__title">{title}</span></span></div>
            <span class="k-row__trailing">{children()}</span>
        </div>
    }
}

#[component]
pub(crate) fn Switch(
    label: &'static str,
    #[prop(into)] checked: Signal<bool>,
    #[prop(into)] disabled: Signal<bool>,
    on_change: impl Fn(bool) + 'static,
) -> impl IntoView {
    view! {
        <button type="button" role="switch" class="k-switch" aria-label=label
            aria-checked=move || if checked.get() { "true" } else { "false" }
            data-state=move || if checked.get() { "checked" } else { "unchecked" }
            disabled=disabled on:click=move |_| on_change(!checked.get_untracked())>
            <span class="k-switch__thumb" data-state=move || if checked.get() { "checked" } else { "unchecked" }></span>
        </button>
    }
}

#[component]
pub(crate) fn ErrorAlert(message: RwSignal<String>, icon: String) -> impl IntoView {
    view! {
        <div class="k-alert is-error" role="alert" hidden=move || message.with(String::is_empty)>
            <span class="appearance-icon k-alert__icon" aria-hidden="true" inner_html=icon></span>
            <div class="k-alert__body"><div class="k-alert__text">{message}</div></div>
        </div>
    }
}

fn notifications(error_icon: String) -> impl IntoView {
    let info_icon = use_context::<crate::shell::Icons>()
        .map(|icons| icons.info)
        .unwrap_or_default();
    let available = window().get("Notification").is_some();
    let permission = RwSignal::new(available.then(Notification::permission));
    let muted = RwSignal::new(
        window()
            .local_storage()
            .ok()
            .flatten()
            .and_then(|storage| storage.get_item(NOTIFICATIONS).ok().flatten())
            .is_some_and(|value| value == "1"),
    );
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let change = move |enabled| {
        busy.set(true);
        error.set(String::new());
        leptos::task::spawn_local(async move {
            let next = async {
                if enabled && Notification::permission() != NotificationPermission::Granted {
                    let request = Notification::request_permission().map_err(|_| {
                        "Notification permission could not be requested. Check browser settings."
                    })?;
                    request.await.map_err(|_| {
                        "Notification permission could not be requested. Check browser settings."
                    })?;
                    let result = Notification::permission();
                    permission.try_set(Some(result));
                    if result != NotificationPermission::Granted {
                        return Ok(());
                    }
                }
                window()
                    .local_storage()
                    .ok()
                    .flatten()
                    .and_then(|storage| {
                        storage
                            .set_item(NOTIFICATIONS, if enabled { "0" } else { "1" })
                            .ok()
                    })
                    .ok_or("Notification preference could not be saved. Check browser storage.")?;
                muted.try_set(!enabled);
                Ok::<_, &str>(())
            }
            .await;
            if let Err(message) = next {
                error.try_set(message.into());
            }
            busy.try_set(false);
        });
    };
    view! {
        <Section title="Notifications">
            <div class="k-card"><Row title="Agent and workflow notifications">
                <Switch label="Agent and workflow notifications"
                    checked=Signal::derive(move || permission.get() == Some(NotificationPermission::Granted) && !muted.get())
                    disabled=Signal::derive(move || busy.get() || matches!(permission.get(), None | Some(NotificationPermission::Denied)))
                    on_change=change />
            </Row></div>
            <ErrorAlert message=error icon=error_icon />
            <div class="k-alert is-info" role="status" hidden=move || permission.get() != Some(NotificationPermission::Denied)>
                <span class="appearance-icon" aria-hidden="true" inner_html=info_icon></span>
                <div class="k-alert__body"><div class="k-alert__text">"Notifications are blocked. Change site permissions in browser settings."</div></div>
            </div>
        </Section>
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConversationSettings {
    auto_archive: bool,
}

fn conversations(error_icon: String) -> impl IntoView {
    let info_icon = use_context::<crate::shell::Icons>()
        .map(|icons| icons.info)
        .unwrap_or_default();
    let archive = RwSignal::new(None);
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    leptos::task::spawn_local(async move {
        match api::read::<ConversationSettings>(CHATS).await {
            Ok(settings) => {
                archive.try_set(Some(settings.auto_archive));
            }
            Err(_) => {
                error.try_set(
                    "Conversation settings could not be loaded. Reopen General to retry.".into(),
                );
            }
        }
    });
    let change = move |checked| {
        busy.set(true);
        error.set(String::new());
        leptos::task::spawn_local(async move {
            match api::command::<ConversationSettings>(CHATS, json!({ "autoArchive": checked }))
                .await
            {
                Ok(settings) => {
                    archive.try_set(Some(settings.auto_archive));
                }
                Err(_) => {
                    error.try_set("Conversation settings could not be saved. Reopen General to check the current value.".into());
                }
            }
            busy.try_set(false);
        });
    };
    view! {
        <Section title="Conversations">
            <div class="k-card"><Row title="Auto-archive agent deliveries">
                <span class="settings-fact">"7 days"</span>
                <button type="button" class="k-icon-btn" aria-label="About auto-archive" title="After work settles; history stays saved.">
                    <span class="appearance-icon" inner_html=info_icon.clone()></span>
                </button>
                <Switch label="Auto-archive agent deliveries"
                    checked=Signal::derive(move || archive.get().unwrap_or(true))
                    disabled=Signal::derive(move || archive.get().is_none() || busy.get()) on_change=change />
            </Row></div>
            <ErrorAlert message=error icon=error_icon />
        </Section>
    }
}

fn time_zone(error_icon: String) -> impl IntoView {
    use crate::time_zone::{Settings, ZoneSelect, PATH};
    let settings = RwSignal::new(None::<Settings>);
    let value = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let load = move |result: Result<Settings, String>, failure: &str| match result {
        Ok(next) => {
            value.try_set(next.zone.clone().unwrap_or_default());
            settings.try_set(Some(next));
        }
        Err(_) => {
            error.try_set(failure.into());
        }
    };
    leptos::task::spawn_local(async move {
        load(
            api::read::<Settings>(PATH).await,
            "Time zone could not load. Reopen General to retry.",
        );
    });
    let change = Callback::new(move |zone: String| {
        busy.set(true);
        error.set(String::new());
        leptos::task::spawn_local(async move {
            let zone = (!zone.is_empty()).then_some(zone);
            load(
                api::command::<Settings>(PATH, json!({ "zone": zone })).await,
                "Time zone could not be saved. Reopen General to check the current value.",
            );
            busy.try_set(false);
        });
    });
    let first = Signal::derive(move || {
        settings.with(|settings| {
            settings.as_ref().map_or_else(String::new, |settings| {
                format!("Automatic · {}", settings.machine.replace('_', " "))
            })
        })
    });
    let zones = Signal::derive(move || {
        settings.with(|settings| {
            settings
                .as_ref()
                .map(|settings| settings.zones.clone())
                .unwrap_or_default()
        })
    });
    view! {
        <Section title="Time">
            <div class="k-card">
                <div class="k-row general-time-zone-row">
                    <div class="k-row__main"><span class="k-row__text">
                        <span class="k-row__title"><label for="general-time-zone">"Time zone"</label></span>
                        <span class="k-row__meta">"Routines follow this zone unless they set their own."</span>
                    </span></div>
                    <span class="k-row__trailing general-time-zone">
                        <ZoneSelect id="general-time-zone" value first zones change
                            disabled=Signal::derive(move || settings.with(Option::is_none) || busy.get()) />
                    </span>
                </div>
            </div>
            <ErrorAlert message=error icon=error_icon />
        </Section>
    }
}

pub fn panel(error_icon: String) -> impl IntoView {
    view! {
        <div class="k-page bs-root">
            <header class="k-page-head"><div class="k-page-head__text"><h1 tabindex="-1">"General"</h1></div></header>
            {notifications(error_icon.clone())}
            {time_zone(error_icon.clone())}
            {crate::voice::settings()}
            {conversations(error_icon)}
        </div>
    }
}
