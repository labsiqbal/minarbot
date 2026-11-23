mod backup;
mod computer;
mod data;
mod devices;
pub(crate) mod external;
pub(crate) mod kit;
mod machines;
pub(crate) mod updates;
mod usage;

use crate::{api, connection_ui::poll_with_backoff, routes::SettingsRoute};
use leptos::prelude::*;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use wasm_bindgen::{JsCast, JsValue};

pub fn panel(route: SettingsRoute) -> AnyView {
    match route {
        SettingsRoute::Machines => machines::panel().into_any(),
        SettingsRoute::Devices => devices::panel().into_any(),
        SettingsRoute::Computer => computer::panel().into_any(),
        SettingsRoute::Updates => updates::panel(),
        SettingsRoute::Usage => usage::panel().into_any(),
        SettingsRoute::Data => data::panel().into_any(),
        SettingsRoute::Backup => backup::panel().into_any(),
        SettingsRoute::External => external::panel().into_any(),
        _ => ().into_any(),
    }
}

fn poll(interval: u64, refresh: impl Fn() + Copy + Send + Sync + 'static) {
    poll_with_backoff(interval, || Some(false), refresh);
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
        Self::paced(path, interval, busy, || 0)
    }

    fn paced(
        path: impl Fn() -> String + Copy + Send + Sync + 'static,
        tick: u64,
        busy: RwSignal<bool>,
        interval: impl Fn() -> u64 + Copy + Send + Sync + 'static,
    ) -> Self {
        let query = Self {
            value: RwSignal::new(None),
            error: RwSignal::new(String::new()),
            fresh: RwSignal::new(false),
            reload: RwSignal::new(0),
        };
        let generation = RwSignal::new(0_u32);
        let running = RwSignal::new(false);
        let started = StoredValue::new(0.0);
        let value = query.value;
        let error = query.error;
        let fresh = query.fresh;
        let reload = query.reload;
        let refresh = move |invalidate: bool| {
            if invalidate {
                generation.update(|current| *current = current.wrapping_add(1));
                running.set(false);
            }
            let now = js_sys::Date::now();
            if busy.get_untracked()
                || running.get_untracked()
                || (!invalidate && now - started.get_value() < untrack(interval) as f64)
            {
                return;
            }
            started.set_value(now);
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
        poll_with_backoff(tick, move || (!busy.get_untracked() && !running.get_untracked()).then(|| !error.with_untracked(String::is_empty)), move || refresh(false));
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

// Same native fallback as app/src/ui/clipboard.ts (T3 MIT provenance in app/t3/provenance.json).
async fn copy(text: &str) -> Result<(), String> {
    let window = window();
    if window.top().ok().flatten().is_none_or(|top| top != window) {
        return Err("Copy is unavailable in embedded views. Open Minarbot directly.".into());
    }
    if let Ok(clipboard) = js_sys::Reflect::get(&window.navigator(), &"clipboard".into()) {
        if let Ok(clipboard) = clipboard.dyn_into::<web_sys::Clipboard>() {
            if clipboard.write_text(text).await.is_ok() {
                return Ok(());
            }
        }
    }
    copy_from_field(text)
        .ok_or_else(|| "Could not copy. Select the text and copy it manually.".into())
}

fn copy_from_field(text: &str) -> Option<()> {
    let document = document().dyn_into::<web_sys::HtmlDocument>().ok()?;
    let active = document
        .active_element()?
        .dyn_into::<web_sys::HtmlElement>()
        .ok()?;
    let parent = active
        .closest("dialog")
        .ok()
        .flatten()
        .or_else(|| document.body().map(Into::into))?;
    let area = document
        .create_element("textarea")
        .ok()?
        .dyn_into::<web_sys::HtmlTextAreaElement>()
        .ok()?;
    area.set_class_name("sr-only");
    area.set_tab_index(-1);
    area.set_read_only(true);
    area.set_value(text);
    parent.append_child(&area).ok()?;
    let _ = area.focus();
    area.select();
    let copied = document.exec_command("copy").unwrap_or(false);
    area.remove();
    let _ = active.focus();
    copied.then_some(())
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
