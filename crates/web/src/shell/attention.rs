use leptos::prelude::*;
use serde::Deserialize;
use std::collections::HashSet;
use wasm_bindgen::{closure::Closure, JsCast};

use crate::workspace_route::valid_id;

#[derive(Clone, PartialEq, Deserialize)]
pub(super) struct Notice {
    pub agent: String,
    thread: Option<String>,
    channel: Option<String>,
    turn: String,
    focus: String,
    kind: Option<String>,
    request: Option<String>,
    status: Option<String>,
}

impl Notice {
    fn valid(&self) -> bool {
        [&self.agent, &self.turn, &self.focus]
            .into_iter()
            .all(|id| valid_id(id))
            && self.thread.as_deref().is_none_or(valid_id)
            && self.channel.as_deref().is_none_or(valid_id)
    }

    pub fn conversation(&self) -> &str {
        self.channel
            .as_deref()
            .or(self.thread.as_deref())
            .unwrap_or("")
    }

    fn card(&self) -> Option<String> {
        let kind = self.kind.as_deref()?;
        let request = self.request.as_deref()?;
        (matches!(kind, "question" | "app" | "harness" | "computer")
            && !request.is_empty()
            && request.len() <= 500
            && !request.chars().any(char::is_control))
        .then(|| format!("{kind}:{}:{request}", self.turn))
    }

    fn key(&self) -> String {
        self.card()
            .unwrap_or_else(|| format!("finished:{}", self.turn))
    }

    pub fn href(&self) -> String {
        if let Some(channel) = &self.channel {
            format!("/rooms/{channel}")
        } else if let Some(thread) = &self.thread {
            format!("/agents/{}/conversations/{thread}", self.agent)
        } else {
            format!("/agents/{}", self.agent)
        }
    }

    pub fn open(&self) {
        let scope = if let Some(channel) = &self.channel {
            format!("channel:{channel}")
        } else if let Some(thread) = &self.thread {
            format!("thread:{thread}")
        } else {
            self.agent.clone()
        };
        if let Ok(Some(storage)) = window().session_storage() {
            let _ = storage.set_item(&format!("minarbot:focus:{scope}"), &self.focus);
            let card = format!("minarbot:focus-card:{scope}");
            if let Some(id) = self.card() {
                let _ = storage.set_item(&card, &id);
            } else {
                let _ = storage.remove_item(&card);
            }
        }
        let _ = window().location().assign(&self.href());
    }
}

#[derive(Deserialize)]
struct Snapshot {
    pending: Vec<Notice>,
    finished: Vec<Notice>,
}

pub(super) fn subscribe() -> ReadSignal<Option<Vec<Notice>>> {
    let pending = RwSignal::new(None);
    let Ok(source) = web_sys::EventSource::new("/api/sidebar/stream") else {
        return pending.read_only();
    };
    let previous = StoredValue::new(None::<HashSet<String>>);
    let message =
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            let Some(snapshot) = event
                .data()
                .as_string()
                .and_then(|raw| serde_json::from_str::<Snapshot>(&raw).ok())
            else {
                return;
            };
            let current = snapshot
                .pending
                .into_iter()
                .filter(Notice::valid)
                .collect::<Vec<_>>();
            if pending.with_untracked(|old| old.as_ref() != Some(&current)) {
                pending.set(Some(current.clone()));
            }
            let notices = current
                .into_iter()
                .chain(snapshot.finished.into_iter().filter(Notice::valid))
                .collect::<Vec<_>>();
            let before = previous.get_value();
            previous.set_value(Some(notices.iter().map(Notice::key).collect()));
            if let Some(before) = before.filter(|_| {
                document().hidden()
                    && js_sys::Reflect::has(&window(), &"Notification".into()).unwrap_or(false)
                    && web_sys::Notification::permission()
                        == web_sys::NotificationPermission::Granted
                    && super::preference("minarbot:workflow-notifications-muted").as_deref()
                        != Some("1")
            }) {
                for notice in notices
                    .into_iter()
                    .filter(|notice| !before.contains(&notice.key()))
                {
                    notify(notice);
                }
            }
        });
    let error = Closure::<dyn FnMut()>::new(move || {
        pending.try_set(None);
    });
    source.set_onmessage(Some(message.as_ref().unchecked_ref()));
    source.set_onerror(Some(error.as_ref().unchecked_ref()));
    let handle = StoredValue::new_local((source, message, error));
    on_cleanup(move || {
        handle.with_value(|(source, _, _)| {
            source.set_onmessage(None);
            source.set_onerror(None);
            source.close();
        })
    });
    pending.read_only()
}

fn notify(notice: Notice) {
    const KEY: &str = "minarbot:needs-you-delivered";
    let id = notice.key();
    let Ok(Some(storage)) = window().local_storage() else {
        return;
    };
    let Ok(raw) = storage.get_item(KEY) else {
        return;
    };
    let mut ids = raw
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .unwrap_or_default();
    if ids.contains(&id) {
        return;
    }
    // ponytail: match React's bounded local history. Use server receipts for cross-device delivery.
    ids.drain(..ids.len().saturating_sub(999));
    ids.push(id.clone());
    if storage
        .set_item(KEY, &serde_json::json!(ids).to_string())
        .is_err()
    {
        return;
    }
    let title = match notice.status.as_deref() {
        Some("failed") => "An agent turn failed",
        Some("done") => "An agent finished",
        _ => "An agent needs you",
    };
    let options = web_sys::NotificationOptions::new();
    options.set_body("Open Minarbot to view the conversation.");
    options.set_tag(&format!("minarbot:{id}"));
    if let Ok(notification) = web_sys::Notification::new_with_options(title, &options) {
        let clicked = notification.clone();
        let handler = Closure::once(move || {
            let _ = window().focus();
            notice.open();
            clicked.close();
        });
        notification.set_onclick(Some(handler.into_js_value().unchecked_ref()));
    }
}
