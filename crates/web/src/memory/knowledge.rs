use super::{pager, params, retry, visible_poll, Page};
use crate::{api, connection_ui as ui, system::kit, workspace_route::valid_id};
use leptos::{html, prelude::*, task::spawn_local};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Clone, Deserialize, PartialEq)]
#[serde(untagged)]
enum Record {
    Learned {
        id: i64,
        text: String,
        kind: String,
        sources: Vec<Source>,
        essential: bool,
        revision: i64,
    },
    Question {
        id: String,
        question: String,
        revision: i64,
        pending: bool,
        reflection: Option<Reflection>,
    },
}

#[derive(Clone, Deserialize, PartialEq)]
struct Reflection {
    answer: String,
    evidence: Vec<Evidence>,
}

#[derive(Clone, Deserialize, PartialEq)]
struct Evidence {
    source: Vec<Source>,
}

#[derive(Clone, Deserialize, PartialEq)]
struct Source {
    id: Option<String>,
    role: Option<String>,
    kind: Option<String>,
    agent: Option<String>,
    channel: Option<String>,
    thread: Option<String>,
    created: Option<String>,
}

impl Source {
    fn href(&self) -> Option<String> {
        let valid = |id: &String| valid_id(id);
        if let Some(id) = self.channel.as_ref().filter(|id| valid(id)) {
            return Some(format!("/rooms/{id}"));
        }
        let agent = self.agent.as_ref().filter(|id| valid(id))?;
        Some(match self.thread.as_ref().filter(|id| valid(id)) {
            Some(thread) => format!("/agents/{agent}/conversations/{thread}"),
            None => format!("/agents/{agent}"),
        })
    }

    fn label(&self) -> &'static str {
        match self.role.as_deref() {
            Some("prompt") => "User message",
            Some("reply") => "Agent reply",
            _ if self.kind.as_deref() == Some("note") => "Saved note",
            _ => "Retained memory",
        }
    }
}

fn sources(items: Vec<Source>) -> impl IntoView {
    let mut seen = std::collections::HashSet::new();
    let unique = items
        .into_iter()
        .filter(|source| seen.insert((source.id.clone(), source.role.clone())))
        .collect::<Vec<_>>();
    (!unique.is_empty()).then(|| view! {
        <details class="memory-sources">
            <summary>{format!("Sources ({})", unique.len())}</summary>
            <ul>{unique.into_iter().map(|source| {
                let label = source.label();
                let href = source.href();
                view! { <li>
                    {href.map_or_else(|| label.into_any(), |href| view! { <a class="k-link" href=href>{label}</a> }.into_any())}
                    {source.created.map(|created| {
                        let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_str(&created));
                        let text = date.to_locale_string("default", &wasm_bindgen::JsValue::UNDEFINED).as_string().unwrap_or_default();
                        view! { " · "<time datetime=created>{text}</time> }
                    })}
                </li> }
            }).collect_view()}</ul>
        </details>
    })
}

#[derive(Clone)]
struct Editor {
    item: Option<Record>,
    model: String,
    text: String,
}

#[derive(Clone)]
struct Notice {
    text: &'static str,
    undo: Option<Value>,
}

#[derive(Clone, Copy)]
struct Changes {
    endpoint: StoredValue<String>,
    read: ui::Query<Page<Record, i64>>,
    edit: RwSignal<Option<Editor>>,
    pending: RwSignal<Option<Value>>,
    notice: RwSignal<Option<Notice>>,
    error: RwSignal<String>,
    busy: RwSignal<bool>,
    search: NodeRef<html::Input>,
    add: NodeRef<html::Button>,
    archived: bool,
}

impl Changes {
    fn send(self, command: Value) {
        if self.archived
            || self.busy.get_untracked()
            || self.pending.with_untracked(|pending| {
                pending.as_ref().is_some_and(|pending| *pending != command)
            })
        {
            return;
        }
        self.busy.set(true);
        self.pending.set(Some(command.clone()));
        self.error.set(String::new());
        self.read.invalidate();
        let path = self.endpoint.get_value();
        spawn_local(async move {
            let result = api::post_status::<Value>(&path, &command).await;
            if self.busy.is_disposed() {
                return;
            }
            match result {
                Ok(receipt) => {
                    self.pending.set(None);
                    self.edit.set(None);
                    let revision = &receipt["revision"];
                    let action = command["action"].as_str().unwrap_or_default();
                    let notice = match action {
                        "forget" => Notice {
                            text: "Memory forgotten.",
                            undo: Some(json!({
                                "id": ui::uuid(), "action": "restore", "unit": command["unit"], "revision": revision,
                            })),
                        },
                        "forget_question" => Notice {
                            text: "Question forgotten.",
                            undo: Some(json!({
                                "id": ui::uuid(), "action": "restore_question", "model": command["model"], "revision": revision,
                            })),
                        },
                        "restore" | "restore_question" => Notice {
                            text: "Restored.",
                            undo: None,
                        },
                        _ => Notice {
                            text: "Saved.",
                            undo: None,
                        },
                    };
                    self.notice.set(Some(notice));
                    if action == "forget_question" {
                        if let Some(button) = self.add.get_untracked() {
                            let _ = button.focus();
                        }
                    } else if action == "forget" {
                        if let Some(input) = self.search.get_untracked() {
                            let _ = input.focus();
                        }
                    }
                    self.read.reload.run(());
                }
                Err((status, error)) => {
                    if (400..500).contains(&status) {
                        self.pending.set(None);
                        self.read.reload.run(());
                    }
                    self.error.set(error);
                }
            }
            self.busy.set(false);
        });
    }

    fn open(self, item: Option<Record>) {
        self.error.set(String::new());
        let (model, text) = match &item {
            Some(Record::Question { id, question, .. }) => (id.clone(), question.clone()),
            Some(Record::Learned { text, .. }) => (String::new(), text.clone()),
            None => (ui::uuid(), String::new()),
        };
        self.edit.set(Some(Editor { item, model, text }));
    }
}

fn save_error(changes: Changes) -> impl IntoView {
    view! {
        <Show when=move || !changes.error.with(String::is_empty)>
            <kit::Alert tone="error" actions=kit::content(move || view! {
                <Show when=move || changes.pending.with(Option::is_some)>
                    <kit::Button label=kit::text("Retry change") size="sm" disabled=Signal::from(changes.busy) on_click=move || {
                        if let Some(command) = changes.pending.get_untracked() { changes.send(command); }
                    } />
                </Show>
            })>{changes.error}</kit::Alert>
        </Show>
    }
}

fn row(record: Record, changes: Changes, locked: Signal<bool>) -> impl IntoView {
    let edit = record.clone();
    let mut items = vec![kit::MenuItem {
        label: "Edit".into(),
        danger: false,
        action: Callback::new(move |()| changes.open(Some(edit.clone()))),
    }];
    let (title, meta, answer, sources_data, name, forget) = match record {
        Record::Learned {
            id,
            text,
            kind,
            sources,
            essential,
            revision,
        } => {
            items.push(kit::MenuItem {
                label: if essential { "Unpin" } else { "Pin to context" }.into(),
                danger: false,
                action: Callback::new(move |()| changes.send(json!({
                    "id": ui::uuid(), "action": "pin", "unit": id, "revision": revision, "essential": !essential,
                }))),
            });
            let kind = match kind.as_str() {
                "observation" => "Observation",
                "experience" => "Experience",
                _ => "Fact",
            };
            (
                text,
                Some((kind, essential)),
                None,
                sources,
                "Memory actions",
                json!({"action":"forget","unit":id,"revision":revision}),
            )
        }
        Record::Question {
            id,
            question,
            pending,
            reflection,
            revision,
        } => {
            let (answer, sources) = reflection.map_or_else(
                || (None, Vec::new()),
                |r| {
                    (
                        Some(r.answer),
                        r.evidence.into_iter().flat_map(|e| e.source).collect(),
                    )
                },
            );
            (
                question,
                pending.then_some(("Awaiting refresh", false)),
                answer,
                sources,
                "Question actions",
                json!({"action":"forget_question","model":id,"revision":revision}),
            )
        }
    };
    items.push(kit::MenuItem {
        label: "Forget".into(),
        danger: true,
        action: Callback::new(move |()| {
            let mut command = forget.clone();
            command["id"] = json!(ui::uuid());
            changes.send(command);
        }),
    });
    view! {
        <li><div class="k-row">
            <div class="k-row__main"><span class="k-row__text">
                <span class="k-row__title"><span class="memory-text">{title}</span></span>
                {meta.map(|(kind, pin)| view! { <span class="k-row__meta">{kind}{pin.then(|| view! { " · "{kit::badge("neutral", "Pinned")} })}</span> })}
                {answer.map(|text| view! { <span class="memory-answer">{text}</span> })}
            </span></div>
            <span class="k-row__trailing"><kit::Menu name=name.to_owned() items disabled=locked>{kit::icon(kit::icons().more)}</kit::Menu></span>
        </div>
        {sources(sources_data)}</li>
    }
}

pub(super) fn panel(path: String, questions: bool, archived: bool) -> impl IntoView {
    let root = NodeRef::<html::Div>::new();
    let query = RwSignal::new(String::new());
    let search = RwSignal::new(String::new());
    let kind = RwSignal::new("all".to_owned());
    let cursors = RwSignal::new(Vec::<i64>::new());
    let endpoint = StoredValue::new(format!(
        "{path}/{}",
        if questions { "questions" } else { "knowledge" }
    ));
    let read = ui::query::<Page<Record, i64>>(move || {
        let endpoint = endpoint.get_value();
        if questions {
            return endpoint;
        }
        let after = cursors.with_untracked(|cursors| cursors.last().copied().unwrap_or_default());
        format!(
            "{endpoint}?{}",
            params(
                &search.get_untracked(),
                &after.to_string(),
                Some(&kind.get_untracked())
            )
        )
    });
    let changes = Changes {
        endpoint,
        read,
        edit: RwSignal::new(None),
        notice: RwSignal::new(None),
        pending: RwSignal::new(None),
        busy: RwSignal::new(false),
        error: RwSignal::new(String::new()),
        search: NodeRef::new(),
        add: NodeRef::new(),
        archived,
    };
    let pending =
        Signal::derive(move || changes.busy.get() || changes.pending.with(Option::is_some));
    let locked = Signal::derive(move || {
        archived
            || pending.get()
            || read.data.with(Option::is_none)
            || !read.error.with(String::is_empty)
    });
    let reload = Callback::new(move |()| {
        if !questions {
            read.data.set(None);
        }
        read.reload.run(());
    });
    visible_poll(root, move || {
        if !read.busy.get_untracked() && !changes.busy.get_untracked() {
            read.reload.run(());
        }
    });
    let icons = kit::icons();
    let editing = Signal::derive(move || changes.edit.with(Option::is_some));
    let title: Signal<String> = Signal::derive(move || {
        if !questions {
            "Edit memory"
        } else if changes
            .edit
            .with(|edit| edit.as_ref().is_some_and(|edit| edit.item.is_some()))
        {
            "Edit question"
        } else {
            "Add standing question"
        }
        .to_owned()
    });
    view! {
        <div node_ref=root class="k-stack memory-view">
            <Show when=move || changes.notice.with(Option::is_some) || !changes.error.with(String::is_empty)>
                <div class="k-stack">
                    <Show when=move || !editing.get()>{save_error(changes)}</Show>
                    {move || changes.notice.get().map(|notice| {
                        if let Some(undo) = notice.undo {
                            view! { <kit::Alert actions=kit::content(move || view! {
                                <kit::Button label=kit::text("Undo") size="sm" disabled=move || archived || pending.get()
                                    on_click=move || changes.send(undo.clone()) />
                            })>{notice.text}</kit::Alert> }.into_any()
                        } else {
                            view! { <kit::Alert>{notice.text}</kit::Alert> }.into_any()
                        }
                    })}
                </div>
            </Show>
            <form class="memory-toolbar" role="search" on:submit=move |event| {
                event.prevent_default(); search.set(query.get_untracked().trim().into());
                    event.stop_propagation();
                cursors.set(Vec::new()); reload.run(());
            }>
                <kit::Field id="memory-search" label=if questions { "Search questions" } else { "Search learned memory" }>
                    <input id="memory-search" node_ref=changes.search class="k-input" type="search" maxlength="200"
                        prop:value=query on:input=move |event| query.set(event_target_value(&event)) />
                </kit::Field>
                {(!questions).then(|| view! {
                    <kit::Field id="memory-kind" label="Type"><span class="connection-select">
                        <select id="memory-kind" class="k-input k-select" on:change=move |event| {
                            kind.set(event_target_value(&event)); cursors.set(Vec::new()); reload.run(());
                        }><option value="all">"All"</option><option value="facts">"Facts"</option><option value="observations">"Observations"</option></select>
                        <span class="k-select__chev">{kit::icon(icons.down)}</span>
                    </span></kit::Field>
                })}
                <button type="submit" class="k-btn k-btn--secondary k-btn--md">"Search"</button>
                {questions.then(|| view! { <button type="button" node_ref=changes.add class="k-btn k-btn--primary k-btn--md" disabled=locked
                    on:click=move |_| changes.open(None)>"Add question"</button> })}
            </form>
            {retry(read.error, read.reload)}
            {move || match read.data.get() {
                Some(page) => {
                    let needle = search.get().to_lowercase();
                    let items = page.items.into_iter().filter(|item| match item {
                        Record::Question { question, .. } => question.to_lowercase().contains(&needle),
                        _ => true,
                    }).collect::<Vec<_>>();
                    if items.is_empty() {
                        let title = if !needle.is_empty() { "No matches" } else if questions { "No standing questions yet" } else { "No learned memory yet" };
                        kit::empty(title, String::new()).into_any()
                    } else {
                        view! { <kit::Card><ul class="memory-list" role="list">
                            {items.into_iter().map(|item| row(item, changes, locked)).collect_view()}
                        </ul></kit::Card> }.into_any()
                    }
                }
                None if read.error.with(String::is_empty) => kit::skeleton(3, "Loading memory").into_any(),
                None => ().into_any(),
            }}
            {pager(cursors, Signal::derive(move || read.data.with(|page| page.as_ref()?.next_after)),
                Signal::derive(move || read.data.with(Option::is_none)), ("Previous page", "Next page"), reload)}
            <kit::Dialog id="memory-editor" title=title open=editing locked=pending on_close=move || changes.edit.set(None)>
                <form class="k-stack" on:submit=move |event| {
                    event.prevent_default();
                    event.stop_propagation();
                    if pending.get_untracked() { return; }
                    if let Some(edit) = changes.edit.get_untracked().filter(|edit| !edit.text.trim().is_empty()) {
                        let id = ui::uuid();
                        let command = match edit.item {
                            Some(Record::Learned { id: unit, revision, .. }) => json!({"id":id, "action":"edit", "unit":unit, "revision":revision, "text":edit.text}),
                            Some(Record::Question { revision, .. }) => json!({"id":id, "action":"save_question", "model":edit.model, "revision":revision, "question":edit.text}),
                            None => json!({"id":id, "action":"save_question", "model":edit.model, "revision":0, "question":edit.text}),
                        };
                        changes.send(command);
                    }
                }>
                    <kit::Field id="memory-edit-text" label=if questions { "Question" } else { "Memory contents" }>
                        <textarea id="memory-edit-text" class="k-input k-textarea" autofocus required maxlength="2000"
                            readonly=pending prop:value=move || changes.edit.with(|editor| editor.as_ref().map_or_else(String::new, |edit| edit.text.clone()))
                            on:input=move |event| changes.edit.update(|editor| {
                                if let Some(edit) = editor { edit.text = event_target_value(&event); }
                            })></textarea>
                    </kit::Field>
                    {save_error(changes)}
                    <div class="k-row-actions">
                        <button type="submit" class="k-btn k-btn--primary k-btn--md" class:is-busy=changes.busy
                            aria-busy=move || changes.busy.get().then_some("true")
                            disabled=move || archived || pending.get() || changes.edit.with(|edit| edit.as_ref().is_none_or(|edit| edit.text.trim().is_empty()))>
                            {move || changes.busy.get().then(|| view! { <span class="k-spinner" aria-hidden="true"></span> })}"Save"</button>
                        <kit::Button label=kit::text("Cancel") disabled=pending on_click=move || changes.edit.set(None) />
                    </div>
                </form>
            </kit::Dialog>
        </div>
    }
}
