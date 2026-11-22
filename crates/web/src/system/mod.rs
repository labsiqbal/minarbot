mod backup;
mod computer;
mod data;
mod devices;
pub(crate) mod external;
pub(crate) mod kit;
mod machines;
mod updates;
mod usage;

use crate::{api, routes::SettingsRoute};
use leptos::prelude::*;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use std::time::Duration;
use wasm_bindgen::JsValue;

pub fn panel(route: SettingsRoute) -> AnyView {
    match route {
        SettingsRoute::Machines => machines::panel().into_any(),
        SettingsRoute::Devices => devices::panel().into_any(),
        SettingsRoute::Computer => computer::panel().into_any(),
        SettingsRoute::Updates => updates::panel().into_any(),
        SettingsRoute::Usage => usage::panel().into_any(),
        SettingsRoute::Data => data::panel().into_any(),
        SettingsRoute::Backup => backup::panel().into_any(),
        SettingsRoute::External => external::panel().into_any(),
        _ => ().into_any(),
    }
}

fn poll(interval: u64, refresh: impl Fn() + Copy + Send + Sync + 'static) {
    if let Ok(timer) = set_interval_with_handle(
        move || {
            if !document().hidden() {
                refresh();
            }
        },
        Duration::from_millis(interval),
    ) {
        on_cleanup(move || timer.clear());
    }
    let listener = window_event_listener(leptos::ev::focus, move |_| {
        if !document().hidden() {
            refresh();
        }
    });
    on_cleanup(move || listener.remove());
}

struct Query<T: Send + Sync + 'static> {
    value: RwSignal<Option<T>>,
    error: RwSignal<String>,
    fresh: RwSignal<bool>,
    reload: RwSignal<u32>,
}

impl<T: Send + Sync + 'static> Copy for Query<T> {}
impl<T: Send + Sync + 'static> Clone for Query<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Clone + PartialEq + DeserializeOwned + Send + Sync + 'static> Query<T> {
    fn new(
        path: impl Fn() -> String + Copy + Send + Sync + 'static,
        interval: u64,
        busy: RwSignal<bool>,
    ) -> Self {
        let query = Self {
            value: RwSignal::new(None),
            error: RwSignal::new(String::new()),
            fresh: RwSignal::new(false),
            reload: RwSignal::new(0),
        };
        let generation = RwSignal::new(0_u32);
        let running = RwSignal::new(false);
        let value = query.value;
        let error = query.error;
        let fresh = query.fresh;
        let reload = query.reload;
        let refresh = move |invalidate: bool| {
            if invalidate {
                generation.update(|current| *current = current.wrapping_add(1));
                running.set(false);
            }
            if busy.get_untracked() || running.get_untracked() {
                return;
            }
            let active = generation.get_untracked();
            let path = untrack(path);
            running.set(true);
            leptos::task::spawn_local(async move {
                let result = api::read::<T>(&path).await;
                if generation.try_get_untracked() != Some(active)
                    || busy.try_get_untracked() != Some(false)
                {
                    return;
                }
                running.set(false);
                fresh.set(result.is_ok());
                match result {
                    Ok(next) => {
                        if value.with_untracked(|current| current.as_ref() != Some(&next)) {
                            value.set(Some(next));
                        }
                        error.set(String::new());
                    }
                    Err(message) => error.set(message),
                }
            });
        };
        let last_path = StoredValue::new(String::new());
        Effect::new(move |_| {
            let current = path();
            let _ = reload.get();
            let _ = busy.get();
            if last_path.with_value(|previous| previous != &current) {
                value.set(None);
                error.set(String::new());
                fresh.set(false);
                last_path.set_value(current);
            }
            refresh(true);
        });
        poll(interval, move || refresh(false));
        query
    }

    fn retry(&self) {
        self.reload.update(|value| *value = value.wrapping_add(1));
    }
}

fn storage() -> Result<web_sys::Storage, String> {
    window()
        .session_storage()
        .ok()
        .flatten()
        .ok_or_else(|| "Browser storage is unavailable.".into())
}

fn restore<T: DeserializeOwned>(key: &str) -> Result<Option<T>, String> {
    storage()?
        .get_item(key)
        .map_err(|_| "Browser storage could not be read.".to_owned())?
        .map(|raw| {
            serde_json::from_str(&raw).map_err(|_| "Saved request could not be read.".to_owned())
        })
        .transpose()
}

fn save(key: &str, value: &impl Serialize) -> Result<(), String> {
    let value = serde_json::to_string(value).map_err(|_| "Request could not be saved.")?;
    storage()?
        .set_item(key, &value)
        .map_err(|_| "Request could not be saved. Check browser storage.".into())
}

fn discard(key: &str) -> Result<(), String> {
    storage()?
        .remove_item(key)
        .map_err(|_| "Browser storage could not be updated.".into())
}

fn uuid() -> Result<String, String> {
    window()
        .crypto()
        .map(|crypto| crypto.random_uuid())
        .map_err(|_| "Command could not be created. Reload Minarbot and try again.".into())
}

fn valid_uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            14 => byte == b'4',
            19 => matches!(byte.to_ascii_lowercase(), b'8' | b'9' | b'a' | b'b'),
            _ => byte.is_ascii_hexdigit(),
        })
}

async fn copy(text: &str) -> Result<(), String> {
    window()
        .navigator()
        .clipboard()
        .write_text(text)
        .await
        .map(|_| ())
        .map_err(|_| "Could not copy. Select the text and copy it manually.".into())
}

fn https_url(value: &str) -> Option<String> {
    let url = web_sys::Url::new(value).ok()?;
    (url.protocol() == "https:" && url.username().is_empty() && url.password().is_empty())
        .then(|| url.href())
}

fn date(value: impl Into<JsValue>) -> js_sys::Date {
    js_sys::Date::new(&value.into())
}

fn date_time(value: impl Into<JsValue>, locale: &str) -> String {
    date(value)
        .to_locale_string(locale, &JsValue::UNDEFINED)
        .into()
}

fn number(value: Value) -> String {
    if value.is_null() {
        return "-".into();
    }
    let raw = value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned);
    let (whole, decimal) = raw.split_once('.').unwrap_or((&raw, ""));
    let mut grouped = String::new();
    for (index, digit) in whole.chars().enumerate() {
        if index > 0 && (whole.len() - index).is_multiple_of(3) && grouped != "-" {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    if !decimal.is_empty() {
        grouped.push('.');
        grouped.push_str(decimal);
    }
    grouped
}
