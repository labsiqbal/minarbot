use crate::{api, connection_ui::*};
use leptos::{prelude::*, task::spawn_local};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const UUID: &str = "[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}";

#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub path: String,
    pub body: Value,
}

impl Command {
    pub fn new(path: impl Into<String>, mut body: Value) -> Self {
        body["id"] = Value::String(uuid());
        Self {
            path: path.into(),
            body,
        }
    }

    pub fn valid(&self, path: &str, fields: &[&str]) -> bool {
        matches(&self.path, path)
            && self.body.as_object().is_some_and(|body| {
                body.keys()
                    .all(|key| key == "id" || fields.contains(&key.as_str()))
            })
            && self.body["id"]
                .as_str()
                .is_some_and(|id| matches(id, &format!("^{UUID}$")))
    }
}

fn matches(value: &str, expression: &str) -> bool {
    js_sys::RegExp::new(expression, "i").test(value)
}

#[derive(Clone, Copy)]
pub struct Journal {
    key: &'static str,
    limit: usize,
    validate: fn(&Command) -> bool,
    pub pending: RwSignal<Option<Command>>,
    pub invalid: RwSignal<bool>,
    pub busy: RwSignal<bool>,
    pub error: RwSignal<String>,
}
fn storage() -> Result<web_sys::Storage, String> {
    window()
        .session_storage()
        .ok()
        .flatten()
        .ok_or_else(|| "Browser recovery storage is unavailable.".into())
}
impl Journal {
    pub fn new(key: &'static str, limit: usize, validate: fn(&Command) -> bool) -> Self {
        let restored = storage().and_then(|storage| {
            let Some(raw) = storage
                .get_item(key)
                .map_err(|_| "Browser recovery data could not be read.")?
            else {
                return Ok(None);
            };
            if raw.len() > limit {
                return Err("Recovery data is too large.".into());
            }
            serde_json::from_str::<Command>(&raw)
                .ok()
                .filter(validate)
                .map(Some)
                .ok_or_else(|| "Recovery data is invalid.".into())
        });
        let invalid = restored.is_err();
        Self {
            key,
            limit,
            validate,
            pending: RwSignal::new(restored.ok().flatten()),
            invalid: RwSignal::new(invalid),
            busy: RwSignal::new(false),
            error: RwSignal::new(String::new()),
        }
    }
    pub fn locked(self) -> bool {
        self.busy.get() || self.pending.with(Option::is_some) || self.invalid.get()
    }
    pub fn discard(self, refresh: UiAction) {
        if self.busy.get_untracked() {
            return;
        }
        match storage().and_then(|storage| {
            storage
                .remove_item(self.key)
                .map_err(|_| "Browser recovery data could not be removed.".into())
        }) {
            Ok(()) => {
                self.pending.set(None);
                self.invalid.set(false);
                self.error.set(String::new());
                refresh.run(());
            }
            Err(message) => self.error.set(message),
        }
    }
    pub fn run(
        self,
        command: Command,
        key: Option<String>,
        finish: impl FnOnce(Option<Value>) + 'static,
    ) {
        if self.busy.get_untracked()
            || self.invalid.get_untracked()
            || self.pending.with_untracked(|pending| {
                pending.as_ref().is_some_and(|pending| pending != &command)
            })
        {
            finish(None);
            return;
        }
        let save = (|| {
            let raw = serde_json::to_string(&command).map_err(|_| "Command could not be saved.")?;
            if raw.len() > self.limit || !(self.validate)(&command) {
                return Err("Request is invalid.".into());
            }
            let storage = storage()?;
            storage
                .set_item(self.key, &raw)
                .map_err(|_| "Browser recovery data could not be saved.")?;
            Ok((storage, raw))
        })();
        let (storage, raw) = match save {
            Ok(result) => result,
            Err(message) => {
                self.error.set(message);
                finish(None);
                return;
            }
        };
        self.pending.set(Some(command.clone()));
        self.busy.set(true);
        self.error.set(String::new());
        spawn_local(async move {
            let mut body = command.body;
            if let Some(key) = &key {
                body["key"] = Value::String(key.clone());
            }
            let result = api::post::<Value>(&command.path, &body).await;
            let settled = result.and_then(|value| {
                if storage.get_item(self.key).ok().flatten().as_deref() == Some(&raw) {
                    storage
                        .remove_item(self.key)
                        .map_err(|_| "Browser recovery data could not be removed.")?;
                }
                Ok(value)
            });
            if self.busy.try_set(false).is_some() {
                finish(None);
                return;
            }
            match settled {
                Ok(result) => {
                    self.pending.set(None);
                    finish(Some(result));
                }
                Err(message) => {
                    self.error
                        .set(redact(message, key.as_deref().unwrap_or("")));
                    finish(None);
                }
            }
        });
    }
}

#[component]
pub fn Recovery(
    journal: Journal,
    retry: UiAction,
    confirmation: Confirm,
    refresh: UiAction,
    #[prop(optional)] enter_key: Option<UiAction>,
    #[prop(optional, into)] needs_key: Signal<bool>,
) -> impl IntoView {
    let discard = UiAction::new(move |()| {
        confirm(
            confirmation,
            "Discard saved request?",
            "The change may already have succeeded. Check the list after it reloads.",
            "Discard request",
            true,
            UiAction::new(move |()| journal.discard(refresh)),
        )
    });
    view! {
        <Show when=move || journal.invalid.get()>
            <Alert actions=Box::new(move || view! { <Button label="Discard request" size="sm" click=discard /> }.into_any())>"Recovery unavailable"</Alert>
        </Show>
        <Show when=move || journal.pending.with(Option::is_some)>
            <Show when=move || journal.busy.get() fallback=move || view! {
                <Alert tone="warn" actions=Box::new(move || view! {
                    {enter_key.map(|enter| view! {
                        <Show when=move || needs_key.get()><Button label="Enter key" size="sm" click=enter /></Show>
                    })}
                    <Button label="Try again" size="sm" disabled=needs_key click=retry />
                    <Button label="Discard request" size="sm" click=discard />
                }.into_any())>
                    "Not confirmed"
                    <Show when=move || needs_key.get()><p>"Enter the same API key to retry. The key was not saved in browser storage."</p></Show>
                </Alert>
            }><span class="k-progress" role="status"><span class="k-spinner" aria-hidden="true"></span>"Applying change..."</span></Show>
        </Show>
    }
}
