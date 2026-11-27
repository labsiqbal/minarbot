use crate::api;
use leptos::prelude::*;
use serde::Deserialize;
use serde_json::json;
use web_sys::{Notification, NotificationPermission};

const CHATS: &str = "/api/conversations/settings";
const WEB_SEARCH: &str = "/api/settings/web-search";
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

#[derive(Clone, Deserialize)]
struct SearchSettings {
    url: Option<String>,
    managed: bool,
}

#[derive(Deserialize)]
struct SearchTest {
    count: u64,
}

fn web_search(error_icon: String) -> impl IntoView {
    let settings = RwSignal::new(None::<SearchSettings>);
    let url = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let load_error = RwSignal::new(String::new());
    let saved = RwSignal::new(false);
    let count = RwSignal::new(None::<u64>);
    let input = NodeRef::<leptos::html::Input>::new();
    leptos::task::spawn_local(async move {
        match api::read::<SearchSettings>(WEB_SEARCH).await {
            Ok(value) => {
                url.try_set(value.url.clone().unwrap_or_default());
                settings.try_set(Some(value));
            }
            Err(_) => {
                load_error
                    .try_set("Web search settings could not load. Reopen General to retry.".into());
            }
        }
    });
    let apply = move |test| {
        if busy.get_untracked()
            || !input
                .get_untracked()
                .is_some_and(|field| field.report_validity())
        {
            return;
        }
        busy.set(true);
        saved.set(false);
        error.set(String::new());
        let body = json!({ "url": url.get_untracked() });
        leptos::task::spawn_local(async move {
            let result = if test {
                count.try_set(None);
                api::command::<SearchTest>("/api/settings/web-search/test", body)
                    .await
                    .map(|result| {
                        count.try_set(Some(result.count));
                    })
            } else {
                api::command::<SearchSettings>(WEB_SEARCH, body)
                    .await
                    .map(|result| {
                        url.try_set(result.url.clone().unwrap_or_default());
                        settings.try_set(Some(result));
                        saved.try_set(true);
                    })
            };
            if let Err(message) = result {
                error.try_set(message);
            }
            busy.try_set(false);
        });
    };
    let managed = move || settings.with(|value| value.as_ref().is_some_and(|value| value.managed));
    let managed_without_url = move || {
        settings.with(|value| {
            value
                .as_ref()
                .is_some_and(|value| value.managed && value.url.is_none())
        })
    };
    view! {
        <Section title="Web search">
            <ErrorAlert message=load_error icon=error_icon />
            <div class="k-card" aria-hidden="true" hidden=move || settings.with(Option::is_some) || !load_error.with(String::is_empty)>
                <div class="k-row"><div class="k-row__main"><span class="k-row__text"><span class="k-skeleton"></span><span class="k-skeleton is-small"></span></span></div></div>
            </div>
            <div class="k-card" hidden=move || !managed_without_url()>
                <Row title="Backend"><span class="settings-fact">"Set by server"</span></Row>
            </div>
            <div class="k-card" hidden=move || settings.with(Option::is_none) || managed_without_url()>
                <form class="web-search-settings" aria-label="Web search" on:submit=move |event| {
                    event.prevent_default();
                    if !managed() { apply(false); }
                }>
                    <div class="k-row">
                        <div class="k-row__main"><span class="k-row__text">
                            <span class="k-row__title"><label for="web-search-url">"SearXNG URL"</label></span>
                            <span class="k-row__meta" hidden=managed>
                                <span role="status">{move || count.get().map(|count| format!("{count} result{}", if count == 1 { "" } else { "s" }))}</span>
                                <span class="sr-only" role="status">{move || saved.get().then_some("Web search settings saved.")}</span>
                                <span id="web-search-error" class="k-field__error" role="alert" hidden=move || error.with(String::is_empty)>{error}</span>
                            </span>
                        </span></div>
                        <span class="k-row__trailing">
                            <input id="web-search-url" name="url" class="k-input" node_ref=input type="url" maxlength="4096"
                                autocomplete="off" autocapitalize="none" spellcheck="false" placeholder="http://127.0.0.1:8890"
                                aria-describedby=move || (!error.with(String::is_empty)).then_some("web-search-error")
                                prop:value=url readonly=move || managed() || busy.get()
                                on:input=move |event| {
                                    url.set(event_target_value(&event));
                                    error.set(String::new());
                                    count.set(None);
                                    saved.set(false);
                                } />
                            <span class="settings-fact" hidden=move || !managed()>"Set by server"</span>
                            <button type="button" class="k-btn k-btn--sm k-btn--secondary" hidden=managed
                                disabled=move || busy.get() || url.with(|url| url.trim().is_empty()) on:click=move |_| apply(true)>"Test"</button>
                            <button type="submit" class="k-btn k-btn--sm k-btn--primary" hidden=managed
                                disabled=move || busy.get() || settings.with(|value| value.as_ref().and_then(|value| value.url.as_deref()).unwrap_or_default() == url.get().trim())>"Save"</button>
                        </span>
                    </div>
                </form>
            </div>
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
                <div class="k-row">
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
            {conversations(error_icon.clone())}
            {web_search(error_icon)}
        </div>
    }
}
