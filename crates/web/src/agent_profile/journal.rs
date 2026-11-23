use crate::{
    api,
    connection_ui::{uuid, Button},
    settings_recovery::UUID,
    system::kit::*,
};
use leptos::{prelude::*, task::spawn_local};
use serde_json::Value;

pub fn command_id(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|id| js_sys::RegExp::new(&format!("^{UUID}$"), "i").test(id))
}

pub fn fields(value: &Value, names: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.keys().all(|name| names.contains(&name.as_str())))
}

pub fn revision(value: &Value, min: u64) -> bool {
    value
        .as_u64()
        .is_some_and(|value| (min..=9_007_199_254_740_991).contains(&value))
}

pub fn bounded(value: &Value, limit: usize, required: bool) -> bool {
    value.as_str().is_some_and(|text| {
        text.encode_utf16().count() <= limit && (!required || !text.trim().is_empty())
    })
}

#[derive(Clone, Copy)]
pub struct Journal {
    key: StoredValue<String>,
    validate: fn(&Value) -> bool,
    limit: usize,
    pub pending: RwSignal<Option<Value>>,
    pub invalid: RwSignal<bool>,
    pub busy: RwSignal<bool>,
    pub error: RwSignal<String>,
}

impl Journal {
    pub fn new(key: String, limit: usize, validate: fn(&Value) -> bool) -> Self {
        let restored: Result<Option<Value>, &str> = (|| {
            let Some(raw) = storage()?
                .get_item(&key)
                .map_err(|_| "Recovery data could not be read.")?
            else {
                return Ok(None);
            };
            if raw.len() > limit {
                return Err("Recovery data is invalid.");
            }
            let value =
                serde_json::from_str::<Value>(&raw).map_err(|_| "Recovery data is invalid.")?;
            if !command_id(&value["id"]) || !validate(&value) {
                return Err("Recovery data is invalid.");
            }
            Ok(Some(value))
        })();
        Self {
            key: StoredValue::new(key),
            validate,
            limit,
            invalid: RwSignal::new(restored.is_err()),
            pending: RwSignal::new(restored.ok().flatten()),
            busy: RwSignal::new(false),
            error: RwSignal::new(String::new()),
        }
    }

    pub fn locked(self) -> bool {
        self.busy.get() || self.invalid.get() || self.pending.with(Option::is_some)
    }

    pub fn discard(self, refresh: Callback<()>) {
        match storage().and_then(|storage| {
            storage
                .remove_item(&self.key.get_value())
                .map_err(|_| "Recovery data could not be removed.")
        }) {
            Ok(()) => {
                self.pending.set(None);
                self.invalid.set(false);
                self.error.set(String::new());
                refresh.run(());
            }
            Err(message) => self.error.set(message.into()),
        }
    }

    // Store the React command shape verbatim. Transform only on the wire (template import).
    pub fn run(
        self,
        mut fresh: Value,
        request: impl FnOnce(&Value) -> (String, Value),
        clear_on_rejection: bool,
        finish: impl FnOnce(Value) + 'static,
    ) {
        if self.busy.get_untracked() || self.invalid.get_untracked() {
            return;
        }
        let command = self.pending.get_untracked().unwrap_or_else(|| {
            fresh["id"] = Value::String(uuid());
            fresh
        });
        let raw = command.to_string();
        if raw.len() > self.limit || !command_id(&command["id"]) || !(self.validate)(&command) {
            self.error.set("Invalid request.".into());
            return;
        }
        let key = self.key.get_value();
        let store = storage().and_then(|storage| {
            storage
                .set_item(&key, &raw)
                .map_err(|_| "Browser could not save recovery data.")?;
            Ok(storage)
        });
        let storage = match store {
            Ok(storage) => storage,
            Err(message) => {
                self.error.set(message.into());
                return;
            }
        };
        let (path, body) = request(&command);
        self.pending.set(Some(command));
        self.busy.set(true);
        self.error.set(String::new());
        spawn_local(async move {
            let result = api::post_status::<Value>(&path, &body).await;
            let settled =
                result.is_ok() || clear_on_rejection && matches!(&result, Err((400..=499, _)));
            let cleared = settled
                && (storage.get_item(&key).ok().flatten().as_deref() != Some(raw.as_str())
                    || storage.remove_item(&key).is_ok());
            if self.busy.try_set(false).is_some() {
                return;
            }
            if cleared {
                self.pending.set(None);
            }
            match result {
                Ok(value) if cleared => finish(value),
                Ok(_) => self
                    .error
                    .set("Recovery data could not be removed. Retry the saved request.".into()),
                Err((_, message)) => self.error.set(message),
            }
        });
    }
}

fn storage() -> Result<web_sys::Storage, &'static str> {
    window()
        .session_storage()
        .ok()
        .flatten()
        .ok_or("Browser recovery storage is unavailable.")
}

#[component]
pub fn Recovery(
    journal: Journal,
    confirm: Confirm,
    title: &'static str,
    retry: Callback<()>,
    refresh: Callback<()>,
) -> impl IntoView {
    let discard = Callback::new(move |()| {
        ask(
            Some(confirm),
            "Discard saved request?",
            "Changes already accepted by the server still apply.",
            "Discard and reload",
            true,
            move || journal.discard(refresh),
        )
    });
    view! {
        <Show when=move || journal.pending.with(Option::is_some) || journal.invalid.get()>
            <Alert tone="warn" actions=content(move || view! {
                <Show when=move || !journal.invalid.get()><Button label="Try again" size="sm" disabled=journal.busy click=retry /></Show>
                <Button label="Discard and reload" size="sm" disabled=journal.busy click=discard />
            })>
                {move || if journal.invalid.get() { "Recovery data could not be read." } else { title }}
            </Alert>
        </Show>
        {error(journal.error)}
    }
}
