use super::{pager, params, retry, visible_poll, Page};
use crate::{api, connection_ui as ui, system::kit};
use leptos::{html, prelude::*, task::spawn_local};
use serde::{Deserialize, Deserializer, Serialize};

const MAX_REVISION: u64 = 9_007_199_254_740_991;

fn optional<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Entry {
    id: String,
    text: String,
    essential: u8,
    revision: u64,
    #[serde(
        default,
        deserialize_with = "optional",
        skip_serializing_if = "Option::is_none"
    )]
    selection_locked: Option<u8>,
}

impl Entry {
    fn valid(&self) -> bool {
        api::command_id(&self.id)
            && !self.text.trim().is_empty()
            && self.text.encode_utf16().count() <= 2000
            && self.essential <= 1
            && self.selection_locked.is_none_or(|value| value <= 1)
            && (1..=MAX_REVISION).contains(&self.revision)
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Draft {
    #[serde(deserialize_with = "Option::deserialize")]
    entry: Option<Entry>,
    text: String,
    essential: bool,
    #[serde(
        default,
        deserialize_with = "optional",
        skip_serializing_if = "Option::is_none"
    )]
    automatic: Option<bool>,
}

impl Draft {
    fn from_entry(entry: Option<Entry>) -> Self {
        let (text, essential, automatic) = entry.as_ref().map_or_else(
            || (String::new(), false, false),
            |entry| {
                (
                    entry.text.clone(),
                    entry.essential != 0,
                    entry.selection_locked == Some(0),
                )
            },
        );
        Self {
            entry,
            text,
            essential,
            automatic: Some(automatic),
        }
    }

    fn dirty(&self) -> bool {
        let original = Self::from_entry(self.entry.clone());
        self.text != original.text
            || self.essential != original.essential
            || self.automatic.unwrap_or_default() != original.automatic.unwrap_or_default()
    }

    fn valid(&self) -> bool {
        self.text.encode_utf16().count() <= 2000 && self.entry.as_ref().is_none_or(Entry::valid)
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Command {
    id: String,
    entry: String,
    revision: u64,
    text: String,
    essential: bool,
    remove: bool,
    #[serde(
        default,
        deserialize_with = "optional",
        skip_serializing_if = "Option::is_none"
    )]
    automatic: Option<bool>,
    #[serde(
        default,
        deserialize_with = "optional",
        skip_serializing_if = "Option::is_none"
    )]
    restore: Option<bool>,
}

impl Command {
    fn valid(&self) -> bool {
        let restore = self.restore == Some(true);
        api::command_id(&self.id)
            && api::command_id(&self.entry)
            && self.revision <= MAX_REVISION
            && self.text.encode_utf16().count() <= 2000
            && !(self.remove && restore)
            && if self.remove || restore {
                self.revision > 0 && self.text.is_empty() && !self.essential
            } else {
                !self.text.trim().is_empty()
            }
    }
}

#[derive(Deserialize)]
struct Receipt {
    id: String,
    revision: u64,
}

fn storage() -> Result<web_sys::Storage, ()> {
    window().session_storage().map_err(|_| ())?.ok_or(())
}

fn recover<T: serde::de::DeserializeOwned>(
    key: &str,
    valid: impl Fn(&T) -> bool,
) -> Result<Option<T>, ()> {
    storage()?
        .get_item(key)
        .map_err(|_| ())?
        .map(|raw| {
            let value = serde_json::from_str(&raw).map_err(|_| ())?;
            valid(&value).then_some(value).ok_or(())
        })
        .transpose()
}

#[derive(Clone, Copy)]
struct Journal {
    draft: RwSignal<Draft>,
    key: StoredValue<String>,
    draft_key: StoredValue<String>,
    pending: RwSignal<Option<Command>>,
    blocked: RwSignal<bool>,
    stored: RwSignal<bool>,
    error: RwSignal<String>,
    notice: RwSignal<String>,
    undo: RwSignal<Option<Command>>,
    busy: RwSignal<bool>,
}

impl Journal {
    fn new(path: &str, stored: RwSignal<bool>) -> Self {
        let key = format!("minarbot:memory:{path}");
        let draft_key = format!("{key}:draft");
        let recovery = recover(&key, Command::valid).and_then(|command| {
            Ok((
                command,
                recover(&draft_key, Draft::valid)?.unwrap_or_default(),
            ))
        });
        let blocked = recovery.is_err();
        let (pending, draft) = recovery.unwrap_or_default();
        Self {
            draft: RwSignal::new(draft),
            key: StoredValue::new(key),
            draft_key: StoredValue::new(draft_key),
            pending: RwSignal::new(pending),
            blocked: RwSignal::new(blocked),
            stored,
            error: RwSignal::new(String::new()),
            notice: RwSignal::new(String::new()),
            undo: RwSignal::new(None),
            busy: RwSignal::new(false),
        }
    }

    fn change(self, update: impl FnOnce(&mut Draft)) {
        self.draft.update(update);
        let saved = self.draft.with_untracked(|draft| {
            let Ok(raw) = serde_json::to_string(draft) else {
                return false;
            };
            storage().is_ok_and(|storage| {
                self.draft_key
                    .with_value(|key| storage.set_item(key, &raw).is_ok())
            })
        });
        self.stored.set(saved);
        if !saved {
            self.error.set(
                "Draft has not been saved in the browser. Copy the text before leaving this page."
                    .into(),
            );
        }
    }

    fn discard(self, reload: Callback<()>) {
        let removed = storage().and_then(|storage| {
            if self.blocked.get_untracked() {
                self.draft_key
                    .with_value(|key| storage.remove_item(key))
                    .map_err(|_| ())?;
                self.draft.set(Draft::default());
            }
            self.key
                .with_value(|key| storage.remove_item(key))
                .map_err(|_| ())
        });
        if removed.is_ok() {
            self.pending.set(None);
            self.blocked.set(false);
            self.error.set(String::new());
            self.notice.set(
                "Draft kept. For revision conflicts, choose Edit note from the latest list.".into(),
            );
            reload.run(());
        } else {
            self.error
                .set("Recovery data could not be deleted. Check browser storage.".into());
        }
    }

    fn send(
        self,
        path: StoredValue<String>,
        archived: bool,
        remove: bool,
        restore: Option<Command>,
        reload: Callback<()>,
        heading: NodeRef<html::H1>,
    ) {
        let pending = self.pending.get_untracked();
        if self.busy.get_untracked()
            || self.blocked.get_untracked()
            || archived && pending.is_none()
        {
            return;
        }
        let command = pending.or(restore).unwrap_or_else(|| {
            self.draft.with_untracked(|draft| Command {
                id: ui::uuid(),
                entry: draft
                    .entry
                    .as_ref()
                    .map_or_else(ui::uuid, |entry| entry.id.clone()),
                revision: draft.entry.as_ref().map_or(0, |entry| entry.revision),
                text: if remove {
                    String::new()
                } else {
                    draft.text.clone()
                },
                essential: !remove && draft.essential,
                remove,
                automatic: Some(draft.automatic.unwrap_or_default()),
                restore: None,
            })
        });
        if !command.valid() {
            self.error.set("Invalid memory change.".into());
            return;
        }
        let key = self.key.get_value();
        let draft_key = self.draft_key.get_value();
        let saved = storage().and_then(|storage| {
            let raw = serde_json::to_string(&command).map_err(|_| ())?;
            storage.set_item(&key, &raw).map_err(|_| ())?;
            Ok((storage, raw))
        });
        let Ok((storage, raw)) = saved else {
            self.error
                .set("Changes could not be saved in the browser.".into());
            return;
        };
        self.busy.set(true);
        self.error.set(String::new());
        self.notice.set(String::new());
        self.pending.set(Some(command.clone()));
        let path = path.get_value();
        spawn_local(async move {
            let outcome = api::post_status::<Receipt>(&path, &command).await;
            if self.busy.is_disposed() {
                return;
            }
            match outcome {
                Ok(receipt) => {
                    let cleared = storage.get_item(&key).ok().flatten().as_deref() == Some(&raw)
                        && storage.remove_item(&draft_key).is_ok()
                        && storage.remove_item(&key).is_ok();
                    if cleared {
                        self.pending.set(None);
                        self.draft.set(Draft::default());
                        self.stored.set(true);
                        self.undo.set(command.remove.then(|| Command {
                            id: ui::uuid(),
                            entry: receipt.id,
                            revision: receipt.revision,
                            text: String::new(),
                            essential: false,
                            remove: false,
                            automatic: None,
                            restore: Some(true),
                        }));
                        self.notice.set(
                            if command.remove {
                                "Note forgotten."
                            } else if command.restore == Some(true) {
                                "Note restored."
                            } else {
                                "Note saved."
                            }
                            .into(),
                        );
                        if let Some(heading) = heading.get_untracked() {
                            let _ = heading.focus();
                        }
                    } else {
                        self.error.set(
                            "Recovery data could not be deleted. Check browser storage.".into(),
                        );
                    }
                    reload.run(());
                }
                Err((status, error)) => {
                    if matches!(status, 400 | 413) && storage.remove_item(&key).is_ok() {
                        self.pending.set(None);
                    }
                    self.error.set(if status == 0 {
                        "Save result is unknown. Retry with the same changes.".into()
                    } else {
                        error
                    });
                }
            }
            self.busy.set(false);
        });
    }
}

pub(super) fn panel(
    path: String,
    archived: bool,
    stored: RwSignal<bool>,
    active: Signal<bool>,
    heading: NodeRef<html::H1>,
    dialog_open: Option<RwSignal<bool>>,
) -> impl IntoView {
    let journal = Journal::new(&path, stored);
    let path = StoredValue::new(path);
    let cursors = RwSignal::new(Vec::<String>::new());
    let query = RwSignal::new(String::new());
    let search = RwSignal::new(String::new());
    let editor = NodeRef::<html::Textarea>::new();
    let root = NodeRef::<html::Div>::new();
    let read = ui::query::<Page<Entry, String>>(move || {
        let params = search.with_untracked(|query| {
            cursors.with_untracked(|cursors| {
                params(query, cursors.last().map_or("", String::as_str), None)
            })
        });
        format!("{}?{params}", path.get_value())
    });
    if let Some(open) = dialog_open {
        Effect::new(move |previous| {
            let open = open.get();
            if !open {
                query.set(String::new());
                let first_page = search.with_untracked(String::is_empty)
                    && cursors.with_untracked(Vec::is_empty);
                if !first_page {
                    read.invalidate();
                    search.set(String::new());
                    cursors.set(Vec::new());
                    read.data.set(None);
                }
                journal.notice.set(String::new());
                journal.undo.set(None);
            } else if previous == Some(false) {
                read.reload.run(());
            }
            open
        });
    }
    let change_page = Callback::new(move |()| {
        read.data.set(None);
        read.reload.run(());
    });
    visible_poll(root, move || {
        if active.get_untracked() && !read.busy.get_untracked() && !journal.busy.get_untracked() {
            read.reload.run(());
        }
    });
    let locked = Signal::derive(move || {
        archived
            || journal.busy.get()
            || journal.blocked.get()
            || journal.pending.with(Option::is_some)
    });
    let stale = Signal::derive(move || {
        locked.get()
            || read.busy.get()
            || !read.error.with(String::is_empty)
            || read.data.with(Option::is_none)
    });
    let confirm = use_context::<kit::Confirm>();
    let choose = Callback::new(move |entry: Option<Entry>| {
        if locked.get_untracked() || entry.is_some() && stale.get_untracked() {
            return;
        }
        let apply = move || {
            journal.change(|draft| *draft = Draft::from_entry(entry.clone()));
            if let Some(editor) = editor.get_untracked() {
                let _ = editor.focus();
            }
        };
        if journal.draft.with_untracked(Draft::dirty) {
            kit::ask(
                confirm,
                "Discard unsaved note changes?",
                "",
                "Discard changes",
                true,
                apply,
            );
        } else {
            apply();
        }
    });
    let send = move |remove, undo| {
        read.invalidate();
        journal.send(path, archived, remove, undo, read.reload, heading);
    };
    view! {
        <div node_ref=root class="k-stack memory-view">
            <Show when=move || journal.blocked.get() || !journal.error.with(String::is_empty)>
                <kit::Alert tone="error">{move || if journal.blocked.get() { "Saved changes could not be read. Discard recovery to continue.".into() } else { journal.error.get() }}</kit::Alert>
            </Show>
            {move || (!journal.notice.with(String::is_empty)).then(|| {
                if let Some(command) = journal.undo.get() {
                    view! { <kit::Alert actions=kit::content(move || view! {
                        <kit::Button label=kit::text("Undo") size="sm" disabled=locked on_click=move || send(false, Some(command.clone())) />
                    })>{journal.notice}</kit::Alert> }.into_any()
                } else {
                    view! { <kit::Alert>{journal.notice}</kit::Alert> }.into_any()
                }
            })}
            <Show when=move || journal.blocked.get() || journal.pending.with(Option::is_some)>
                <Show when=move || journal.busy.get() fallback=move || view! {
                    <kit::Alert tone="warn" title=kit::text("Changes await confirmation") actions=kit::content(move || view! {
                        <Show when=move || journal.pending.with(Option::is_some)>
                            <kit::Button label=kit::text("Retry save") size="sm" on_click=move || send(false, None) />
                        </Show>
                        <kit::Button label=kit::text("Discard recovery and reload") size="sm" on_click=move || {
                            kit::ask(confirm, "Delete recovery data?", "Changes may already be saved on the server. Check the list before making new changes.",
                                "Delete recovery", true, move || journal.discard(read.reload));
                        } />
                    })>
                        "Retrying uses the same data."
                        {move || journal.pending.get().map(|pending| view! {
                            <p class="memory-text">{if pending.remove { "Forget note".into() } else if pending.restore == Some(true) { "Restore note".into() } else { pending.text }}</p>
                        })}
                    </kit::Alert>
                }>
                    <span class="k-progress" role="status"><span class="k-spinner" aria-hidden="true"></span>"Applying change..."</span>
                </Show>
            </Show>
            <section class="k-section">
                <div class="k-section__head"><h2 class="k-section__title">{move || if journal.draft.with(|draft| draft.entry.is_some()) { "Edit note" } else { "New note" }}</h2></div>
                <form class="k-stack" on:submit=move |event| {
                    event.prevent_default();
                    event.stop_propagation();
                    if !locked.get_untracked() { send(false, None); }
                }>
                    <kit::Field id="note-text" label="Memory contents">
                        <textarea id="note-text" node_ref=editor class="k-input k-textarea" required maxlength="2000"
                            prop:value=move || journal.draft.with(|draft| draft.text.clone()) disabled=locked
                            on:input=move |event| journal.change(|draft| draft.text = event_target_value(&event))></textarea>
                    </kit::Field>
                    <ui::Check label="Pin to context" checked=Signal::derive(move || journal.draft.with(|draft| draft.essential)) disabled=locked
                        change=Callback::new(move |checked| journal.change(|draft| draft.essential = checked)) />
                    <ui::Check label="Allow agent to manage context selection" checked=Signal::derive(move || journal.draft.with(|draft| draft.automatic.unwrap_or_default())) disabled=locked
                        change=Callback::new(move |checked| journal.change(|draft| draft.automatic = Some(checked))) />
                    <div class="k-row-actions">
                        <button type="submit" class="k-btn k-btn--primary k-btn--md" disabled=move || locked.get() || journal.draft.with(|draft| draft.text.trim().is_empty())>"Save note"</button>
                        <kit::Button label=kit::text("New note") disabled=locked on_click=move || choose.run(None) />
                        <Show when=move || journal.draft.with(|draft| draft.entry.is_some())>
                            <kit::Menu name="Note actions".to_owned() disabled=locked items=vec![kit::MenuItem {
                                label: "Forget note".into(), danger: true, action: Callback::new(move |()| {
                                    kit::ask(confirm, "Forget this note?", "Earlier messages still appear in conversation history.", "Forget note", true, move || send(true, None));
                                }),
                            } ]>{kit::icon(kit::icons().more)}</kit::Menu>
                        </Show>
                    </div>
                </form>
            </section>
            {retry(read.error, read.reload)}
            <kit::Section title="Saved notes">
                <form class="k-stack" on:submit=move |event| {
                    event.prevent_default();
                    event.stop_propagation();
                    if !journal.busy.get_untracked() {
                        cursors.set(Vec::new()); search.set(query.get_untracked()); change_page.run(());
                    }
                }>
                    <kit::Field id="note-search" label="Search memory">
                        <input id="note-search" class="k-input" maxlength="200" prop:value=query disabled=journal.busy
                            on:input=move |event| query.set(event_target_value(&event)) />
                    </kit::Field>
                    <div class="k-row-actions"><button type="submit" class="k-btn k-btn--secondary k-btn--md" disabled=journal.busy>"Search"</button></div>
                </form>
                {move || match read.data.get() {
                    Some(page) if page.items.is_empty() => kit::empty("No matching notes yet.", String::new()).into_any(),
                    Some(page) => view! { <kit::Card>{page.items.into_iter().map(|entry| {
                        let pin = if entry.essential != 0 { "Pinned" } else { "Searchable" };
                        let selection = if entry.selection_locked == Some(0) { "Selection managed by agent" } else { "Selection locked by user" };
                        view! { <kit::Row title=kit::text(entry.text.clone()) meta=kit::content(move || format!("{pin} · {selection}"))
                            trailing=kit::content(move || view! {
                                <kit::Button label=kit::text("Edit note") size="sm" disabled=stale on_click=move || choose.run(Some(entry.clone())) />
                            }) />
                        }
                    }).collect_view()}</kit::Card> }.into_any(),
                    None if read.error.with(String::is_empty) => kit::skeleton(3, "Loading memory...").into_any(),
                    None => ().into_any(),
                }}
                {pager(cursors, Signal::derive(move || read.data.with(|page| page.as_ref().and_then(|page| page.next_after.clone()))),
                    Signal::from(journal.busy), ("Previous page", "Next page"), change_page)}
            </kit::Section>
        </div>
    }
}
