use crate::{api, connection_ui::*};
use leptos::{prelude::*, task::spawn_local};
use serde::Deserialize;
use serde_json::{json, Value};

const PATH: &str = "/api/engine-logins";

#[derive(Clone, Deserialize, PartialEq)]
struct Runtimes {
    choices: Vec<Choice>,
}
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Choice {
    runtime: String,
    label: String,
    available: bool,
    version: Option<String>,
    signed_in: Option<bool>,
    reason: Option<String>,
}
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Login {
    id: String,
    runtime: String,
    name: String,
    checked_at: Option<Value>,
    signed_in: bool,
    command: String,
}

fn save(logins: Query<Vec<Login>>, login: Login) {
    logins.data.try_update(|data| {
        let rows = data.get_or_insert_with(Vec::new);
        if let Some(existing) = rows.iter_mut().find(|row| row.id == login.id) {
            *existing = login;
        } else {
            rows.push(login);
        }
    });
}

pub fn panel() -> impl IntoView {
    let engine = query::<Runtimes>(|| "/api/runtimes".into());
    let logins = query::<Vec<Login>>(|| PATH.into());
    let adding = RwSignal::new(None::<String>);
    let command = RwSignal::new(None::<Login>);
    let checking = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let confirmation = Confirm::new(None);
    let refresh = UiAction::new(move |()| {
        engine.reload.run(());
        logins.reload.run(());
    });
    let in_progress = move || !checking.with(String::is_empty);
    view! {
        <div class="k-page bs-root">
            <Header title="Engines".to_owned() actions=Box::new(move || view! {
                <Button label="Re-check" disabled=in_progress busy=move || engine.busy.get() || logins.busy.get() click=refresh />
            }.into_any()) />
            <Show when=move || !engine.error.with(String::is_empty)><Alert>"Engine status could not be loaded."</Alert></Show>
            <Show when=move || !logins.error.with(String::is_empty)><Alert>"Engine logins could not be loaded."</Alert></Show>
            <ErrorNotice error=error.into() />
            <Show when=move || engine.data.with(Option::is_none) && engine.error.with(String::is_empty)>
                <Loading label="Loading engines…" />
            </Show>
            <Show when=move || engine.data.with(Option::is_some)>
                <div class="k-card">
                    <For each=move || engine.data.get().map_or_else(Vec::new, |data| data.choices)
                        key=|choice| (choice.runtime.clone(), choice.available, choice.version.clone(), choice.signed_in, choice.reason.clone())
                        children=move |choice| {
                            let runtime = StoredValue::new(choice.runtime);
                            let can_add = runtime.with_value(|value| matches!(value.as_str(), "codex" | "claude"));
                            let aria = format!("Add login for {}", choice.label);
                            let available = choice.available;
                            view! {
                                <Row title=choice.label meta=Box::new(move || view! {
                                    {choice.version.map(|text| view! { <span>{text}</span> })}
                                    {choice.signed_in.map(|signed| view! { <span>{if signed { "Signed in" } else { "Signed out" }}</span> })}
                                    {choice.reason.map(|text| view! { <span>{text}</span> })}
                                }.into_any()) trailing=Box::new(move || view! {
                                    <Badge success=available>{if available { "Available" } else { "Unavailable" }}</Badge>
                                    {can_add.then(|| view! {
                                        <Button label="Add login" aria=aria icon="plus" size="sm"
                                            disabled=move || logins.busy.get() || !logins.error.with(String::is_empty) || in_progress()
                                            click=UiAction::new(move |()| { error.set(String::new()); adding.set(Some(runtime.get_value())); }) />
                                    })}
                                }.into_any()) />
                                <For each={move || logins.data.get().unwrap_or_default().into_iter()
                                        .filter(|login| runtime.with_value(|runtime| runtime == &login.runtime)).collect::<Vec<_>>()}
                                    key=|login| (login.id.clone(), login.signed_in, login.checked_at.as_ref().map(Value::to_string))
                                    children=move |login| {
                                        let status = if login.signed_in { "Signed in" } else if login.checked_at.is_some() { "Signed out" } else { "Not checked" };
                                        let login = StoredValue::new(login);
                                        let name = login.with_value(|row| row.name.clone());
                                        let id = login.with_value(|row| row.id.clone());
                                        let disabled = move || logins.busy.get() || in_progress();
                                        let check = UiAction::new(move |()| {
                                            let id = login.with_value(|login| login.id.clone());
                                            checking.set(id.clone());
                                            error.set(String::new());
                                            spawn_local(async move {
                                                match api::command::<Login>(&format!("{PATH}/{}/check", encode(&id)), json!({})).await {
                                                    Ok(row) => save(logins, row),
                                                    Err(message) => { error.try_set(message); }
                                                }
                                                checking.try_set(String::new());
                                            });
                                        });
                                        let remove = UiAction::new(move |()| {
                                            confirm(confirmation, "Remove login?", login.with_value(|row| row.name.clone()), "Remove", true, UiAction::new(move |()| {
                                                if in_progress() { return; }
                                                let id = login.with_value(|row| row.id.clone());
                                                checking.set(id.clone());
                                                error.set(String::new());
                                                spawn_local(async move {
                                                    match api::command::<Value>(&format!("{PATH}/{}/remove", encode(&id)), json!({})).await {
                                                        Ok(_) => { logins.data.try_update(|data| { if let Some(rows) = data { rows.retain(|row| row.id != id); } }); }
                                                        Err(message) => { error.try_set(message); }
                                                    }
                                                    checking.try_set(String::new());
                                                });
                                            }));
                                        });
                                        view! {
                                            <Row title=name.clone() icon="key"
                                                meta=Box::new(move || view! { <span role="status">{status}</span> }.into_any())
                                                trailing=Box::new(move || view! {
                                                    <Button label="Check sign-in" size="sm" disabled=disabled busy=move || checking.with(|checking| checking == &id) click=check />
                                                    <Menu label=format!("Actions for {name}") disabled=disabled
                                                        items=vec![
                                                            MenuItem::new("Sign-in command", UiAction::new(move |()| command.set(Some(login.get_value())))),
                                                            MenuItem::new("Remove", remove).danger(),
                                                        ] />
                                                }.into_any()) />
                                        }
                                    } />
                            }
                        } />
                </div>
            </Show>
            {move || adding.get().map(|runtime| editor(runtime, adding, command, logins))}
            {move || command.get().map(|login| command_dialog(login, command))}
            <ConfirmDialog state=confirmation />
        </div>
    }
}

fn editor(
    runtime: String,
    adding: RwSignal<Option<String>>,
    command: RwSignal<Option<Login>>,
    logins: Query<Vec<Login>>,
) -> impl IntoView {
    let title = if runtime == "codex" {
        "Add Codex login"
    } else {
        "Add Claude login"
    };
    let close = UiAction::new(move |()| adding.set(None));
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let id = uuid();
    view! {
        <Modal title=title.to_owned() busy=busy close=close footer=Box::new(move || view! {
            <Button label="Cancel" disabled=busy click=close />
            <Button label="Add login" variant="primary" busy=busy submit=true form="engine-login" />
        }.into_any())>
            <form id="engine-login" class="k-stack" on:submit=move |event: web_sys::SubmitEvent| {
                let Some(fields) = form(&event) else { return };
                if busy.get_untracked() { return; }
                busy.set(true); error.set(String::new());
                let body = json!({ "id": id, "runtime": runtime, "name": field(&fields, "name").trim() });
                spawn_local(async move {
                    let result = api::post::<Login>(PATH, &body).await;
                    if busy.try_get_untracked().is_none() { return; }
                    busy.set(false);
                    match result {
                        Ok(login) => { save(logins, login.clone()); close.run(()); command.set(Some(login)); }
                        Err(message) => error.set(message),
                    }
                });
            }>
                <Field name="engine-login-name" label="Name">
                    <input class="k-input" id="engine-login-name" name="name" maxlength="80" autocomplete="off" required readonly=busy />
                </Field>
                <ErrorNotice error=error.into() />
            </form>
        </Modal>
    }
}

fn command_dialog(login: Login, state: RwSignal<Option<Login>>) -> impl IntoView {
    let feedback = RwSignal::new("");
    let text = StoredValue::new(login.command);
    let close = UiAction::new(move |()| state.set(None));
    view! {
        <Modal title="Sign in on this host".to_owned() description=login.name close=close
            footer=Box::new(move || view! { <Button label="Done" variant="primary" click=close /> }.into_any())>
            <div class="k-stack">
                <Code label="Terminal command" text=text.get_value() />
                <div class="k-inline">
                    <Button label="Copy command" click=UiAction::new(move |()| spawn_local(async move {
                        let result = copy(&text.get_value()).await;
                        feedback.try_set(if result.is_ok() { "Copied" } else { "Copy failed. Select the command to copy it." });
                    })) />
                    <span class="k-field__hint" role="status">{feedback}</span>
                </div>
            </div>
        </Modal>
    }
}
