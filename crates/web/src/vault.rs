use crate::{api, connection_ui::*};
use leptos::{prelude::*, task::spawn_local};
use serde::Deserialize;
use serde_json::{json, Value};

const PATH: &str = "/api/vault";

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Entry {
    name: String,
    kind: String,
    revision: u64,
    origin: Option<String>,
    agents: Vec<String>,
    #[serde(default)]
    has_totp: bool,
    #[serde(default)]
    has_notes: bool,
}

pub fn panel() -> impl IntoView {
    let entries = query::<Vec<Entry>>(|| PATH.into());
    let editing = RwSignal::new(None::<Option<Entry>>);
    let deleting = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let confirmation = Confirm::new(None);
    let disabled = move || !deleting.with(String::is_empty) || entries.busy.get();
    let reload = UiAction::new(move |()| {
        error.set(String::new());
        entries.reload.run(());
    });
    view! {
        <div class="k-page bs-root">
            <Header title="Vault".to_owned() actions=Box::new(move || view! {
                <Button label="Add entry" variant="primary" icon="plus-md" disabled=move || disabled() || entries.data.with(Option::is_none)
                    click=UiAction::new(move |()| editing.set(Some(None))) />
            }.into_any()) />
            <ErrorNotice error=Signal::derive(move || if error.with(String::is_empty) { entries.error.get() } else { error.get() }) retry=reload label="Reload" />
            <Show when=move || entries.data.with(Option::is_none) && entries.error.with(String::is_empty)>
                <Loading count=2 label="Loading Vault…" />
            </Show>
            <Show when=move || entries.data.with(|data| data.as_ref().is_some_and(Vec::is_empty))>
                <Empty title="No Vault entries" />
            </Show>
            <Show when=move || entries.data.with(|data| data.as_ref().is_some_and(|rows| !rows.is_empty()))>
                <div class="k-card">
                    <For each=move || entries.data.get().unwrap_or_default() key=|entry| (entry.name.clone(), entry.revision)
                        children=move |entry| {
                            let name = entry.name.clone();
                            let count = entry.agents.len();
                            let kind = if entry.kind == "login" { "Login" } else { "Secret" };
                            let click = UiAction::new({ let entry = entry.clone(); move |()| editing.set(Some(Some(entry.clone()))) });
                            let remove = UiAction::new(move |()| {
                                let name = entry.name.clone();
                                let revision = entry.revision;
                                confirm(confirmation, format!("Delete {name}?"), "Agents will lose access to this Vault entry.", "Delete entry", true, UiAction::new(move |()| {
                                    if disabled() { return; }
                                    let name = name.clone();
                                    deleting.set(name.clone());
                                    error.set(String::new());
                                    spawn_local(async move {
                                        match api::post::<Vec<Entry>>("/api/vault/delete", &json!({ "name": name, "revision": revision })).await {
                                            Ok(data) => { entries.data.try_set(Some(data)); }
                                            Err(message) => { error.try_set(message); }
                                        }
                                        deleting.try_set(String::new());
                                    });
                                }));
                            });
                            view! {
                                <Row title=name.clone() icon="key" click=click disabled=disabled
                                    meta=Box::new(move || view! {
                                        {kind}
                                        {entry.origin.map(|origin| view! { <span>{origin}</span> })}
                                        <span>{format!("{count} {}", if count == 1 { "agent" } else { "agents" })}</span>
                                    }.into_any())
                                    trailing=Box::new(move || view! {
                                        <Button label="Delete" aria=format!("Delete {name}") size="sm" variant="ghost"
                                            disabled=disabled busy=move || deleting.with(|value| value == &name) click=remove />
                                    }.into_any()) />
                            }
                        } />
                </div>
            </Show>
            {move || editing.get().map(|entry| editor(entry, editing, entries))}
            <ConfirmDialog state=confirmation />
        </div>
    }
}

fn editor(
    entry: Option<Entry>,
    editing: RwSignal<Option<Option<Entry>>>,
    entries: Query<Vec<Entry>>,
) -> impl IntoView {
    let title = entry.as_ref().map_or_else(
        || "Add Vault entry".into(),
        |entry| format!("Edit {}", entry.name),
    );
    let kind = RwSignal::new(
        entry
            .as_ref()
            .map_or_else(|| "login".into(), |entry| entry.kind.clone()),
    );
    let access = RwSignal::new(
        entry
            .as_ref()
            .map_or_else(Vec::new, |entry| entry.agents.clone()),
    );
    let clear_totp = RwSignal::new(false);
    let clear_notes = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let close = UiAction::new(move |()| editing.set(None));
    let new = entry.is_none();
    let totp = entry.as_ref().is_some_and(|entry| entry.has_totp);
    let notes = entry.as_ref().is_some_and(|entry| entry.has_notes);
    let origin = entry.as_ref().and_then(|entry| entry.origin.clone());
    let submit = move |event: web_sys::SubmitEvent| {
        let Some(fields) = form(&event) else { return };
        if busy.get_untracked() {
            return;
        }
        let mut body = json!({
            "name": entry.as_ref().map_or_else(|| field(&fields, "name"), |entry| entry.name.clone()),
            "kind": kind.get_untracked(),
            "agents": access.get_untracked(),
            "revision": entry.as_ref().map_or(0, |entry| entry.revision)
        });
        let mut private = Vec::new();
        for name in ["url", "username", "password", "totp", "notes", "value"] {
            let value = field(&fields, name);
            if !value.is_empty() {
                if name != "url" {
                    private.push(value.clone());
                }
                body[name] = Value::String(value);
            }
        }
        if clear_totp.get_untracked() {
            body["totp"] = json!("");
        }
        if clear_notes.get_untracked() {
            body["notes"] = json!("");
        }
        busy.set(true);
        error.set(String::new());
        spawn_local(async move {
            let result = api::post::<Vec<Entry>>(PATH, &body).await;
            if busy.try_get_untracked().is_none() {
                return;
            }
            busy.set(false);
            match result {
                Ok(data) => {
                    entries.data.set(Some(data));
                    close.run(());
                }
                Err(message) => error.set(
                    private
                        .iter()
                        .fold(message, |message, secret| redact(message, secret)),
                ),
            }
        });
    };
    let agents = use_context::<ReadSignal<Option<Result<crate::shell::Directory, String>>>>();
    view! {
        <Modal title=title busy=busy close=close footer=Box::new(move || view! {
            <Button label="Cancel" disabled=busy click=close />
            <Button label="Save entry" variant="primary" busy=busy submit=true form="vault-form" />
        }.into_any())>
            <form id="vault-form" class="k-stack" autocomplete="off" on:submit=submit>
                <ErrorNotice error=error.into() />
                {new.then(|| view! {
                    <Field label="Type" name="vault-kind">
                        <Select name="vault-kind" value=kind.into() disabled=busy
                            options=vec![("login".into(), "Login".into()), ("secret".into(), "Secret".into())].into()
                            change=Callback::new(move |value| kind.set(value)) />
                    </Field>
                    <Field label="Entry name" name="vault-name" hint="Lowercase letters, numbers, and hyphens.">
                        <input class="k-input" id="vault-name" name="name" required maxlength="64"
                            pattern="(?:[a-z0-9]|-){1,64}" spellcheck="false" aria-describedby="vault-name-hint" readonly=busy />
                    </Field>
                })}
                <Field label="Site URL" name="vault-url">
                    <input class="k-input" id="vault-url" name="url" type="url" required=new maxlength="2000" placeholder=origin spellcheck="false" readonly=busy />
                </Field>
                <Show when=move || kind.with(|kind| kind == "login") fallback=move || view! {
                    <Field label="Secret value" name="vault-value">
                        <input class="k-input" id="vault-value" name="value" type="password" maxlength="4000" required=new readonly=busy
                            autocomplete="new-password" placeholder=(!new).then_some("Leave blank to keep saved value") />
                    </Field>
                }>
                    <Field label="Username" name="vault-username">
                        <input class="k-input" id="vault-username" name="username" required=new maxlength="4000" spellcheck="false" readonly=busy
                            placeholder=(!new).then_some("Leave blank to keep saved username") />
                    </Field>
                    <Field label="Password" name="vault-password">
                        <input class="k-input" id="vault-password" name="password" type="password" required=new maxlength="4000" readonly=busy
                            autocomplete="new-password" placeholder=(!new).then_some("Leave blank to keep saved password") />
                    </Field>
                    <Field label="TOTP key" name="vault-totp" optional=true>
                        <input class="k-input" id="vault-totp" name="totp" type="password" maxlength="256" autocomplete="new-password"
                            placeholder=totp.then_some("Leave blank to keep saved key") disabled=clear_totp readonly=busy />
                    </Field>
                    {totp.then(|| view! { <Check label="Remove saved TOTP key" checked=clear_totp disabled=busy change=Callback::new(move |value| clear_totp.set(value)) /> })}
                </Show>
                <Field label="Notes" name="vault-notes" optional=true>
                    <textarea class="k-input k-textarea" id="vault-notes" name="notes" rows="2" maxlength="4000" disabled=clear_notes readonly=busy
                        placeholder=notes.then_some("Leave blank to keep saved notes")></textarea>
                </Field>
                {notes.then(|| view! { <Check label="Remove saved notes" checked=clear_notes disabled=busy change=Callback::new(move |value| clear_notes.set(value)) /> })}
                {move || {
                    let Some(Ok(data)) = agents.and_then(|data| data.get()) else { return None };
                    let agents = data.agents.into_iter().chain(data.guide.filter(|guide| !guide.hidden)).collect::<Vec<_>>();
                    (!agents.is_empty()).then(|| view! {
                        <fieldset class="k-fieldset">
                            <legend class="k-field__label">"Agent access"</legend>
                            {agents.into_iter().map(|agent| {
                                let id = StoredValue::new(agent.id);
                                view! { <Check label=agent.name checked=move || access.with(|rows| id.with_value(|id| rows.contains(id)))
                                    disabled=busy change=Callback::new(move |checked| access.update(|rows| {
                                        if checked { rows.push(id.get_value()); }
                                        else { rows.retain(|row| id.with_value(|id| row != id)); }
                                    })) /> }
                            }).collect_view()}
                        </fieldset>
                    })
                }}
            </form>
        </Modal>
    }
}
