use crate::{
    connection_ui::*,
    settings_recovery::{Command, Journal},
};
use leptos::prelude::*;
use serde::Deserialize;
use serde_json::json;

#[derive(Clone, Deserialize, PartialEq)]
struct Variable {
    name: String,
    updated: u64,
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Environment {
    items: Vec<Variable>,
    expose_computer: bool,
    revision: u64,
}

#[derive(Clone, Copy)]
struct Editor {
    journal: Journal,
    data: Query<Environment>,
    cache: StoredValue<Option<Environment>>,
    base: StoredValue<String>,
    confirmation: Confirm,
    open: RwSignal<bool>,
    editing: RwSignal<Option<Variable>>,
    name: RwSignal<String>,
    value: RwSignal<String>,
    notice: RwSignal<&'static str>,
}

impl Editor {
    fn saving(self) -> bool {
        self.journal.pending.with(|pending| {
            pending
                .as_ref()
                .is_some_and(|command| self.base.with_value(|base| &command.path == base))
        })
    }

    fn show(self, editing: Option<Variable>) {
        self.name.set(
            editing
                .as_ref()
                .map_or_else(String::new, |row| row.name.clone()),
        );
        self.value.set(String::new());
        self.editing.set(editing);
        self.open.set(true);
    }

    fn execute(self, command: Command) {
        let saving = self.base.with_value(|base| command.path == *base);
        let secret = saving.then(|| self.value.get_untracked());
        if secret.as_ref().is_some_and(String::is_empty) {
            self.journal
                .error
                .set("Paste the secret value again to retry this save.".into());
            return;
        }
        let notice = if saving {
            "Environment value saved."
        } else if command.path.ends_with("/remove") {
            "Environment value deleted."
        } else {
            "Computer environment setting saved."
        };
        self.notice.set("");
        self.cache.set_value(None);
        self.journal.run_secret(
            command,
            secret.map(|value| ("value", value)),
            move |result| {
                if result.is_some() {
                    self.value.set(String::new());
                    self.notice.set(notice);
                    self.open.set(false);
                    self.data.reload.run(());
                }
            },
        );
    }

    fn recovery(self) -> impl IntoView {
        let journal = self.journal;
        let discard = UiAction::new(move |()| {
            confirm(self.confirmation, "Discard saved request?",
                "Check the latest values first. The change may already have succeeded. Clearing this request does not undo saved changes.",
                "Discard request", true, UiAction::new(move |()| {
                    journal.discard(self.data.reload);
                    self.value.set(String::new());
                    self.open.set(false);
                }));
        });
        view! {
            <Show when=move || journal.invalid.get() || journal.pending.with(Option::is_some)>
                <Show when=move || journal.busy.get() fallback=move || view! {
                    <Alert tone="warn" actions=Box::new(move || view! {
                        <Show when=move || self.saving() && !self.open.get()>
                            <Button label="Enter value" size="sm" click=UiAction::new(move |()| {
                                if let Some(command) = journal.pending.get_untracked() {
                                    self.name.set(command.body["name"].as_str().unwrap_or_default().to_owned());
                                    self.editing.set(None);
                                    self.open.set(true);
                                }
                            }) />
                        </Show>
                        <Show when=move || journal.pending.with(Option::is_some)>
                            <Button label="Try again" size="sm" disabled=move || self.saving() && self.value.with(String::is_empty)
                                click=UiAction::new(move |()| {
                                    if let Some(command) = journal.pending.get_untracked() { self.execute(command); }
                                }) />
                        </Show>
                        <Button label="Discard request" size="sm" click=discard />
                    }.into_any())>
                        <div class="k-alert__title">{move || if journal.invalid.get() { "Recovery unavailable" } else { "Not confirmed" }}</div>
                        <Show when=move || self.saving()>"Enter the same value to retry. The value was not saved in browser storage."</Show>
                    </Alert>
                }><span class="k-progress" role="status"><span class="k-spinner" aria-hidden="true"></span>"Applying change..."</span></Show>
            </Show>
        }
    }

    fn dialog(self) -> impl IntoView {
        let journal = self.journal;
        let row = self.editing.get_untracked();
        let title = row.as_ref().map_or_else(
            || "Add variable".into(),
            |row| format!("Replace {}", row.name),
        );
        let updated = row.as_ref().map(|row| row.updated);
        let editing = row.is_some();
        let lock_value = move || {
            journal.busy.get()
                || journal.invalid.get()
                || journal.pending.with(Option::is_some) && !self.saving()
        };
        view! {
            <Modal title=title busy=journal.busy close=UiAction::new(move |()| {
                self.value.set(String::new());
                self.open.set(false);
            }) footer=Box::new(move || view! {
                {move || view! {
                    <Button label=if self.saving() { "Try again" } else if editing { "Replace value" } else { "Add variable" }
                        variant="primary" submit=true form="project-environment-form" busy=journal.busy
                        disabled=move || lock_value() || self.name.with(String::is_empty) || self.value.with(String::is_empty) />
                }}
            }.into_any())>
                <Show when=move || !journal.error.with(String::is_empty)><Alert>{journal.error}</Alert></Show>
                {self.recovery()}
                <form class="k-stack" id="project-environment-form" on:submit=move |event: web_sys::SubmitEvent| {
                    event.prevent_default();
                    if lock_value() { return; }
                    if let Some(command) = journal.pending.get_untracked() {
                        self.execute(command);
                        return;
                    }
                    let name = self.name.get_untracked();
                    let command = Command::new(self.base.get_value(), json!({"name":name, "updated":updated}));
                    if editing {
                        confirm(self.confirmation, format!("Replace {name}?"),
                            "Future project runs will use the new value. The old value cannot be shown again.",
                            "Replace value", true, UiAction::new(move |()| self.execute(command.clone())));
                    } else { self.execute(command); }
                }>
                    <Field label="Variable name" name="environment-name">
                        <input class="k-input" id="environment-name" required pattern="[A-Z_][A-Z0-9_]{0,63}"
                            maxlength="64" placeholder="SERVICE_API_TOKEN" autocomplete="off" spellcheck="false"
                            disabled=move || editing || journal.locked() prop:value=self.name
                            on:input=move |event| self.name.set(event_target_value(&event)) />
                    </Field>
                    <Field label="Secret value" name="environment-value">
                        <input class="k-input" id="environment-value" required type="password" autocomplete="new-password"
                            spellcheck="false" disabled=lock_value prop:value=self.value on:input=move |event| self.value.set(event_target_value(&event)) />
                    </Field>
                </form>
            </Modal>
        }
    }
}

pub(crate) fn panel(
    project: String,
    archived: bool,
    cache: StoredValue<Option<Environment>>,
) -> impl IntoView {
    let base = format!("/api/projects/{project}/environment");
    let path = base.clone();
    let journal = Journal::new(
        format!("minarbot:project-environment:{project}"),
        4000,
        move |command| {
            if !command.valid(
                &format!("^{path}(/remove|/settings)?$"),
                &["name", "updated", "confirmed", "exposeComputer", "revision"],
            ) {
                return false;
            }
            if command.path.ends_with("/settings") {
                return command.body["revision"].is_u64()
                    && command.body["exposeComputer"].is_boolean()
                    && command.body["confirmed"].is_boolean();
            }
            let name = command.body["name"].as_str().unwrap_or_default();
            js_sys::RegExp::new("^[A-Z_][A-Z0-9_]{0,63}$", "").test(name)
                && (command.body["updated"].is_null() || command.body["updated"].is_u64())
                && (!command.path.ends_with("/remove")
                    || command.body["updated"].is_u64() && command.body["confirmed"] == true)
        },
    );
    let base = StoredValue::new(base);
    let editor = Editor {
        journal,
        base,
        data: cached_query(move || base.get_value(), Some(cache)),
        cache,
        confirmation: Confirm::new(None),
        open: RwSignal::new(false),
        editing: RwSignal::new(None),
        name: RwSignal::new(String::new()),
        value: RwSignal::new(String::new()),
        notice: RwSignal::new(""),
    };
    let locked = move || archived || journal.locked() || editor.data.busy.get();
    let error = Signal::derive(move || {
        let error = journal.error.get();
        if error.is_empty() {
            editor.data.error.get()
        } else {
            error
        }
    });
    view! {
        <div class="k-page bs-root project-environment bs-set3-environment">
            <Header title="Environment".to_owned() actions=Box::new(move || view! {
                <button class="k-icon-btn" type="button" aria-label="Reload environment" title="Reload environment"
                    disabled=journal.busy on:click=move |_| editor.data.reload.run(())><Icon name="refresh" /></button>
                <Button label="Add variable" variant="primary" icon="plus" disabled=locked click=UiAction::new(move |()| editor.show(None)) />
            }.into_any()) />
            <Show when=move || !error.with(String::is_empty)><Alert>{error}</Alert></Show>
            <p role="status" class="sr-only">{editor.notice}</p>
            {editor.recovery()}
            <div class="k-stack">{move || match editor.data.data.get() {
                None if error.with(String::is_empty) => view! { <Loading label="Loading environment..." /> }.into_any(),
                None => ().into_any(),
                Some(data) if data.items.is_empty() => view! { <Empty icon="key" title="No environment variables" /> }.into_any(),
                Some(data) => view! { <div class="k-card">{data.items.into_iter().map(|row| {
                    let name = row.name.clone();
                    let updated = row.updated;
                    let row = StoredValue::new(row);
                    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(updated as f64));
                    let date = date.to_locale_string("default", &js_sys::Object::new()).as_string().unwrap_or_default();
                    view! { <Row title=name.clone() icon="key" meta=Box::new(move || format!("Updated {date}").into_any())
                        trailing=Box::new(move || view! {
                            <Badge success=true>"Set"</Badge>
                            <Menu label=format!("Variable actions for {name}") disabled=locked items=vec![
                                MenuItem::new("Replace", UiAction::new(move |()| editor.show(Some(row.get_value())))),
                                MenuItem::new("Delete", UiAction::new(move |()| {
                                    let name = row.with_value(|row| row.name.clone());
                                    confirm(editor.confirmation, format!("Delete {name}?"),
                                        "Future project runs will no longer receive this value. Saved files and completed actions remain.",
                                        "Delete variable", true, UiAction::new(move |()| editor.execute(Command::new(
                                            format!("{}/remove", base.get_value()), json!({"name":name, "updated":updated, "confirmed":true})
                                        ))));
                                })).danger(),
                            ] />
                        }.into_any()) /> }
                }).collect_view()}</div> }.into_any(),
            }}</div>
            <Section title="Agent computer">
                {move || match editor.data.data.get() {
                    Some(data) => view! { <div class="k-card"><Row title="Expose environment to the agent computer" trailing=Box::new(move || view! {
                        <Switch label="Expose environment to the agent computer" checked=data.expose_computer disabled=locked
                            change=Callback::new(move |enabled| {
                                let command = Command::new(format!("{}/settings", base.get_value()),
                                    json!({"revision":data.revision, "exposeComputer":enabled, "confirmed":enabled}));
                                if enabled {
                                    confirm(editor.confirmation, "Expose project secrets to the agent computer?",
                                        "Desktop applications and websites may access or display these values. Screenshots can reveal them, and text redaction cannot remove secrets from images. The desktop restarts before and after each exposed action.",
                                        "Expose environment", true, UiAction::new(move |()| editor.execute(command.clone())));
                                } else { editor.execute(command); }
                            }) />
                    }.into_any()) /></div> }.into_any(),
                    None if error.with(String::is_empty) => view! { <Loading count=1 /> }.into_any(),
                    None => ().into_any(),
                }}
            </Section>
            <Show when=move || editor.open.get()>{move || editor.dialog()}</Show>
            <ConfirmDialog state=editor.confirmation />
        </div>
    }
}
