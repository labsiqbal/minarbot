use super::{api, date, https_url, kit::*, poll, Query};
use crate::general::Switch;
use leptos::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};
use wasm_bindgen::JsValue;

const PATH: &str = "/api/device-pairing";
const SIGN_IN: &str = "Remote sign-in required";

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Device {
    id: String,
    name: String,
    last_seen: f64,
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Settings {
    required: bool,
    can_require: bool,
    current: Option<String>,
    pairing_url: Option<String>,
    devices: Vec<Device>,
}

#[derive(Clone, Deserialize)]
struct Pairing {
    code: String,
    url: String,
    qr: String,
}

pub fn panel() -> impl IntoView {
    let confirm = use_context::<Confirm>();
    let busy = RwSignal::new(false);
    let query = Query::<Settings>::new(|| PATH.into(), 5000, busy);
    let settings = query.value;
    let error_message = RwSignal::new(String::new());
    let code = RwSignal::new(None::<Pairing>);
    let deadline = RwSignal::new(0.0);
    let expired = RwSignal::new(false);
    poll(1000, move || {
        if code.with_untracked(Option::is_some) {
            expired.set(js_sys::Date::now() >= deadline.get_untracked());
        }
    });
    let close = move || {
        code.set(None);
        error_message.set(String::new());
    };
    let toggle = move |required| {
        if busy.get_untracked() || !query.fresh.get_untracked() {
            return;
        }
        busy.set(true);
        error_message.set(String::new());
        leptos::task::spawn_local(async move {
            if let Err(message) = api::post::<Value>(PATH, &json!({"required": required})).await {
                error_message.try_set(message);
            }
            busy.try_set(false);
        });
    };
    let issue = move || {
        if busy.get_untracked() || !query.fresh.get_untracked() {
            return;
        }
        busy.set(true);
        error_message.set(String::new());
        let started = js_sys::Date::now();
        let origin = window()
            .location()
            .search()
            .ok()
            .and_then(|search| web_sys::UrlSearchParams::new_with_str(&search).ok())
            .and_then(|params| params.get("pairingOrigin"))
            .filter(|value| !value.is_empty());
        leptos::task::spawn_local(async move {
            let body = origin.map_or_else(|| json!({}), |origin| json!({"origin": origin}));
            match api::post::<Pairing>("/api/device-pairing/code", &body).await {
                Ok(next) => {
                    deadline.try_set(started + 120_000.0);
                    expired.try_set(false);
                    code.try_set(Some(next));
                }
                Err(message) => {
                    error_message.try_set(message);
                }
            }
            busy.try_set(false);
        });
    };
    let revoke = move |device: Device, current: bool| {
        ask(
            confirm,
            "Revoke device?",
            &device.name,
            "Revoke",
            true,
            move || {
                if busy.get_untracked() {
                    return;
                }
                let id = device.id.clone();
                busy.set(true);
                error_message.set(String::new());
                leptos::task::spawn_local(async move {
                    match api::post::<Value>("/api/device-pairing/revoke", &json!({"id": id})).await
                    {
                        Ok(_) if current => {
                            let _ = window().location().reload();
                        }
                        Ok(_) => {}
                        Err(message) => {
                            error_message.try_set(message);
                        }
                    }
                    busy.try_set(false);
                });
            },
        );
    };
    let page_error = move || {
        let loading = query.error.get();
        if loading.is_empty() && code.with(Option::is_none) {
            error_message.get()
        } else {
            loading
        }
    };
    view! {
      <div class="k-page bs-root devices-page">
        <Header title="Devices" actions=content(move || view! {
          <Button label=text("Pair a device") variant="primary" on_click=issue
            disabled=Signal::derive(move || busy.get() || !query.fresh.get() || settings.with(|value| value.as_ref().is_none_or(|value| value.pairing_url.is_none()))) />
        }) />
        <Show when=move || !page_error().is_empty()><Alert tone="error">{page_error}</Alert></Show>
        <Section title="Remote access">
          <Show when=move || settings.with(Option::is_some) fallback=move || skeleton(1, "Loading device settings...")>
            <Card><Row title=text(SIGN_IN)
              meta=content(move || view! { <span hidden=move || settings.with(|value| value.as_ref().is_some_and(|value| value.can_require))>"Pair this browser first."</span> })
              trailing=content(move || view! {
                <Switch label=SIGN_IN checked=Signal::derive(move || settings.with(|value| value.as_ref().is_some_and(|value| value.required)))
                  disabled=Signal::derive(move || busy.get() || !query.fresh.get() || settings.with(|value| value.as_ref().is_none_or(|value| !value.can_require))) on_change=toggle />
              }) /></Card>
            <Show when=move || settings.with(|value| value.as_ref().is_some_and(|value| value.pairing_url.is_none()))>
              <Alert>"Set up an HTTPS address to pair a device."</Alert>
            </Show>
          </Show>
        </Section>
        <Section title="Paired devices">
          {move || match settings.get() {
            None => skeleton(3, "Loading paired devices...").into_any(),
            Some(value) if value.devices.is_empty() => empty("No paired devices", String::new()).into_any(),
            Some(value) => view! { <Card>{value.devices.into_iter().map(|device| {
                let current = value.current.as_deref() == Some(device.id.as_str());
                let seen = date(device.last_seen);
                let iso = String::from(seen.to_iso_string());
                let options = js_sys::Object::new();
                let _ = js_sys::Reflect::set(&options, &"dateStyle".into(), &"medium".into());
                let _ = js_sys::Reflect::set(&options, &"timeStyle".into(), &"short".into());
                let formatted = String::from(seen.to_locale_string("default", &JsValue::from(options)));
                let label = format!("Revoke {}", device.name);
                view! { <Row title=text(device.name.clone()) leading=icons().smartphone
                  meta=content(move || view! { {if current { "This browser · Last seen" } else { "Last seen" }}<time datetime=iso>{formatted}</time> })
                  trailing=content(move || view! { <Button label=text("Revoke") name=label size="sm" disabled=busy on_click=move || revoke(device.clone(), current) /> }) /> }
            }).collect_view()}</Card> }.into_any(),
          }}
        </Section>
        <Dialog id="device-pairing" title=text("Pair a device") open=Signal::derive(move || code.with(Option::is_some)) on_close=close locked=busy
          footer=content(move || view! {
            <Show when=move || expired.get() fallback=move || view! { <Button label=text("Done") on_click=close /> }>
              <Button label=text("Create new code") variant="primary" busy=busy on_click=issue />
            </Show>
          })>
          {error(error_message)}
          <Show when=move || expired.get()><Alert>"Pairing code expired."</Alert></Show>
          {move || (!expired.get()).then(|| code.get()).flatten().map(|value| {
            let url = https_url(&value.url);
            let qr = value.qr.starts_with("data:image/svg+xml;base64,").then_some(value.qr);
            let label = format!("Pairing code {}", value.code.chars().map(|digit| digit.to_string()).collect::<Vec<_>>().join(" "));
            view! { <div class="devices-pairing">
              {qr.map(|src| view! { <img src=src alt="QR code for the Minarbot pairing page" width="160" height="160" /> })}
              <strong aria-label=label>{value.code}</strong>
              {url.map(|url| view! { <a class="k-link" href=url.clone() target="_blank" rel="noopener noreferrer">{url.clone()}</a> })}
              <span class="k-field__hint">"Valid for 2 minutes"</span>
            </div> }
          })}
        </Dialog>
      </div>
    }
}
