use super::*;

pub(super) fn events(chat: Chat, row: RwSignal<Turn>) -> impl IntoView {
    let events = Memo::new(move |_| row.with(|turn| turn.events.clone()));
    let archived = chat.config.with_value(|config| config.archived);
    view! {
        <div class="turn-extras k-stack" hidden=move || events.with(Vec::is_empty)>
            {move || events.get().into_iter().filter_map(|event| {
                let id = event["id"].as_str().filter(|id| protocol::id(id))?.to_owned();
                let name = event["event"].as_str()?.to_owned();
                let status = match event["status"].as_str() {
                    Some("waiting") => "Waiting",
                    Some("received") => "Received",
                    Some("expired") => "Expired",
                    _ => "Closed",
                };
                let deadline = history::date(event["deadline"].as_f64().unwrap_or_default());
                Some(view! { <section class="k-stack">
                    <div class="k-row"><div class="k-row__main"><span class="k-row__text"><span class="k-row__title">{format!("Event {name}: {status}")}</span><span class="k-row__meta">{deadline}</span></span></div></div>
                    {(status == "Waiting" && !archived).then(|| event_connection(id))}
                </section> })
            }).collect_view()}
        </div>
    }
}

#[derive(Clone, Deserialize)]
struct EventConnection {
    event: String,
    deadline: f64,
    url: String,
    secret: String,
}

fn event_connection(id: String) -> impl IntoView {
    let connection = RwSignal::new(None::<EventConnection>);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let path = format!("/api/turn-waits/{id}");
    view! {
        <section class="k-stack bs-root event-connection">
            <h3 class="k-section__title">"Waiting for event"</h3>
            <p class="k-field__hint">"The event applies only to this request. Local URLs require Minarbot to keep running."</p>
            <p class="k-field__error" role="alert" hidden=move || error.with(String::is_empty)>{error}</p>
            <button type="button" class="k-btn k-btn--sm k-btn--secondary event-reveal" disabled=busy aria-expanded=move || connection.with(Option::is_some)
                on:click=move |_| {
                    if connection.with_untracked(Option::is_some) {
                        connection.set(None);
                        return;
                    }
                    if busy.get_untracked() { return; }
                    error.set(String::new());
                    busy.set(true);
                    let path = path.clone();
                    leptos::task::spawn_local(async move {
                        match get::<EventConnection>(&path).await {
                            Ok(value) => { connection.try_set(Some(value)); }
                            Err(reason) => { error.try_set(reason.message); }
                        }
                        busy.try_set(false);
                    });
                }>{move || if busy.get() { "Loading connection..." } else if connection.with(Option::is_some) { "Hide connection" } else { "Show event connection" }}</button>
            {move || connection.get().map(|value| view! {
                <p class="k-field__hint">{format!("Event name: {}. Deadline: {}.", value.event, history::date(value.deadline))}</p>
                <label class="k-field"><span class="k-field__label">"Event URL"</span><input class="k-input" readonly prop:value=value.url /></label>
                <label class="k-field"><span class="k-field__label">"Signing secret"</span><input class="k-input" readonly autocomplete="off" spellcheck="false" prop:value=value.secret /></label>
                <crate::system::kit::Alert tone="warn">"The secret grants permission to resume this step. Store it only in trusted integrations; do not enter it in conversations."</crate::system::kit::Alert>
                <p class="k-field__hint">"Send JSON with event, optional eventTime, and payload. The X-Minarbot-Time header contains Unix time in seconds, X-Minarbot-Id contains a unique delivery ID, and X-Minarbot-Signature contains the hexadecimal HMAC-SHA256 of the time, newline, ID, newline, then the exact JSON bytes. Use the secret as UTF-8 text."</p>
            })}
        </section>
    }
}

#[derive(Clone, Deserialize)]
struct Preview {
    turn: String,
    digest: String,
    files: Vec<CheckpointFile>,
}

#[derive(Clone, Deserialize)]
struct CheckpointFile {
    path: String,
    action: String,
}

#[derive(Clone, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Restore {
    id: String,
    checkpoint_id: String,
    digest: String,
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'a'..=b'f' | b'0'..=b'9'))
}

pub(super) fn checkpoint(chat: Chat, row: RwSignal<Turn>) -> impl IntoView {
    let Some((agent, turn)) = row
        .with_untracked(|row| {
            (protocol::id(&row.agent) && protocol::id(&row.id))
                .then(|| (row.agent.clone(), row.id.clone()))
        })
        .filter(|_| chat.config.with_value(|config| !config.channel))
    else {
        return ().into_any();
    };
    let key = StoredValue::new(format!("minarbot:git-restore:{agent}:{turn}"));
    let endpoint = StoredValue::new(format!("/api/agents/{agent}/git/checkpoints/{turn}"));
    let raw = saved(&key.get_value());
    let command = raw
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Restore>(raw).ok())
        .filter(|command| {
            protocol::id(&command.id) && command.checkpoint_id == turn && digest(&command.digest)
        });
    let invalid = RwSignal::new(raw.is_some() && command.is_none());
    let pending = RwSignal::new(command);
    let turn = StoredValue::new(turn);
    let preview = RwSignal::new(None::<Preview>);
    let error = RwSignal::new(String::new());
    let notice = RwSignal::new("");
    let busy = RwSignal::new(false);
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let clear = NodeRef::<leptos::html::Dialog>::new();
    let icons = use_context::<Icons>().unwrap_or_default();
    let show = move || {
        controls::dialog_open(dialog);
        if busy.get_untracked()
            || invalid.get_untracked()
            || pending.with_untracked(Option::is_some)
        {
            return;
        }
        preview.set(None);
        error.set(String::new());
        busy.set(true);
        let path = endpoint.get_value();
        let id = turn.get_value();
        leptos::task::spawn_local(async move {
            match get::<Preview>(&path).await {
                Ok(value) if value.turn == id && digest(&value.digest) => {
                    preview.try_set(Some(value));
                }
                Ok(_) => {
                    error.try_set("Checkpoint preview was invalid.".into());
                }
                Err(reason) => {
                    error.try_set(reason.message);
                }
            }
            busy.try_set(false);
        });
    };
    let restore = move || {
        if busy.get_untracked() || invalid.get_untracked() {
            return;
        }
        let command = pending.get_untracked().or_else(|| {
            let preview = preview
                .get_untracked()
                .filter(|item| !item.files.is_empty())?;
            Some(Restore {
                id: uuid()?,
                checkpoint_id: turn.get_value(),
                digest: preview.digest,
            })
        });
        let Some(command) = command else {
            return;
        };
        let Ok(raw) = serde_json::to_string(&command) else {
            return;
        };
        let key = key.get_value();
        if !save(&key, &raw) {
            error.set("Restoration request could not be saved.".into());
            return;
        }
        pending.set(Some(command));
        error.set(String::new());
        busy.set(true);
        let path = format!("{}/restore", endpoint.get_value());
        leptos::task::spawn_local(async move {
            let result = post::<Value>(&path, &raw).await;
            if busy.is_disposed() {
                return;
            }
            busy.set(false);
            match result {
                Ok(value) if value["restored"] == true && remove(&key) => {
                    pending.set(None);
                    if let Some(dialog) = dialog.get_untracked() { dialog.close(); }
                    notice.set("Workspace files restored. A safety checkpoint was saved.");
                }
                Ok(value) => error.set(value["error"].as_str().unwrap_or("Restoration is uncertain. Inspect source control before another write.").into()),
                Err(reason) => error.set(reason.message),
            }
        });
    };
    view! {
        <div class="bs-root checkpoint-action">
            <button type="button" class="k-icon-btn" aria-label="Restore to before this turn" title="Restore to before this turn"
                hidden=move || !pending.with(Option::is_some) && !invalid.get() && !row.with(|row| !row.active() && row.checkpoint.as_ref().is_some_and(|point| point["restorable"] == true))
                on:click=move |_| show()><span class="appearance-icon" inner_html=icons.restore></span></button>
            <p role="status" class="source-control__hint" hidden=move || notice.get().is_empty()>{notice}</p>
            <dialog class="k-dialog k-dialog--md workspace-dialog bs-root chat-control-dialog" node_ref=dialog aria-label="Restore to before this turn?"
                on:cancel=move |event: web_sys::Event| { if busy.get_untracked() { event.prevent_default(); } }>
                <div class="k-dialog__head"><div><h2 class="k-dialog__title">"Restore to before this turn?"</h2>
                    <p class="k-dialog__desc">"Restore the listed workspace files. Current files are checkpointed first. HEAD and the Git index stay unchanged."</p></div>
                    <form method="dialog"><button type="submit" class="k-icon-btn" aria-label="Close" disabled=busy><span class="appearance-icon" inner_html=icons.close></span></button></form>
                </div>
                <div class="k-dialog__body">
                    <p role="alert" class="k-field__error" hidden=move || error.with(String::is_empty)>{error}</p>
                    <p role="alert" class="k-field__error" hidden=move || !invalid.get()>"Saved restoration request could not be read. Inspect workspace files before clearing it."</p>
                    <p role="status" class="k-alert k-alert--warn" hidden=move || pending.with(Option::is_none)>"Restoration was requested. Check its saved result before another write."</p>
                    <p role="status" hidden=move || !busy.get() || preview.with(Option::is_some)>"Loading checkpoint..."</p>
                    {move || preview.get().map(|value| if value.files.is_empty() {
                        view! { <div class="k-empty"><strong>"No files to restore"</strong><span>"Workspace already matches this checkpoint."</span></div> }.into_any()
                    } else {
                        view! { <ul class="checkpoint-files" aria-label="Files to restore">{value.files.into_iter().map(|file| view! {
                            <li><span>{file.path}</span><span class="k-badge">{file.action}</span></li>
                        }).collect_view()}</ul> }.into_any()
                    })}
                </div>
                <div class="k-dialog__foot">
                    <button type="button" class="k-btn k-btn--md k-btn--secondary" hidden=move || !invalid.get() && pending.with(Option::is_none) disabled=busy on:click=move |_| controls::dialog_open(clear)>"Clear request"</button>
                    <form method="dialog"><button type="submit" class="k-btn k-btn--md k-btn--secondary" disabled=busy autofocus>"Cancel"</button></form>
                    <button type="button" class="k-btn k-btn--md k-btn--danger"
                        disabled=move || busy.get() || invalid.get() || pending.with(Option::is_none) && preview.with(|value| value.as_ref().is_none_or(|preview| preview.files.is_empty()))
                        on:click=move |_| restore()>{move || if pending.with(Option::is_some) { "Check restoration result" } else { "Restore files" }}</button>
                </div>
            </dialog>
            <dialog class="conversation-dialog" node_ref=clear aria-label="Clear restoration request?">
                <form method="dialog" class="k-stack">
                    <p>"Inspect the workspace first. Files already restored stay changed; clearing this request does not undo them."</p>
                    <div class="k-row-actions">
                        <button type="submit" class="k-btn k-btn--secondary">"Cancel"</button>
                        <button type="submit" class="k-btn k-btn--danger" on:click=move |_| {
                            if remove(&key.get_value()) {
                                pending.set(None); invalid.set(false); error.set(String::new());
                                if let Some(dialog) = dialog.get_untracked() { dialog.close(); }
                            } else { error.set("Restoration request could not be cleared.".into()); }
                        }>"Clear request"</button>
                    </div>
                </form>
            </dialog>
        </div>
    }.into_any()
}

#[wasm_bindgen]
extern "C" {
    type Playback;

    #[wasm_bindgen(js_namespace = MinarbotConversation, js_name = speechPlayback)]
    fn speech_playback(listener: &js_sys::Function) -> wasm_bindgen::JsValue;

    #[wasm_bindgen(method)]
    fn play(this: &Playback, owner: &str, text: &str);

    #[wasm_bindgen(method)]
    fn stop(this: &Playback, owner: &str);

    #[wasm_bindgen(method)]
    fn dispose(this: &Playback);
}

#[derive(Clone, Copy)]
struct Voice {
    engine: StoredValue<Playback, LocalStorage>,
    state: RwSignal<(bool, String, bool, String)>,
}

pub(super) fn playback() {
    let state = RwSignal::new((false, String::new(), false, String::new()));
    let listener = Closure::<dyn FnMut(bool, String, bool, String)>::new(
        move |enabled, owner, playing, error| {
            state.try_set((enabled, owner, playing, error));
        },
    );
    let engine = speech_playback(listener.as_ref().unchecked_ref());
    if engine.is_null() || engine.is_undefined() {
        return;
    }
    let engine = StoredValue::new_local(engine.unchecked_into::<Playback>());
    let listener = StoredValue::new_local(listener);
    provide_context(Voice { engine, state });
    on_cleanup(move || {
        engine.with_value(Playback::dispose);
        listener.dispose();
    });
}

pub(super) fn listen(row: RwSignal<Turn>) -> impl IntoView {
    let Some(voice) = use_context::<Voice>() else {
        return ().into_any();
    };
    let id = StoredValue::new(row.with_untracked(|turn| turn.id.clone()));
    on_cleanup(move || {
        let _ = id.try_with_value(|id| voice.engine.try_with_value(|engine| engine.stop(id)));
    });
    let playing = move || {
        voice
            .state
            .with(|state| state.2 && id.with_value(|id| state.1 == *id))
    };
    let error = move || {
        voice.state.with(|state| {
            id.with_value(|id| {
                if state.1 == *id {
                    state.3.clone()
                } else {
                    String::new()
                }
            })
        })
    };
    let icons = StoredValue::new(use_context::<Icons>().unwrap_or_default());
    view! {
        <span class="speech-playback" hidden=move || !voice.state.with(|state| state.0) || row.with(|turn| turn.reply.as_ref().is_none_or(String::is_empty))>
            <button type="button" class="k-icon-btn" aria-label=move || if playing() { "Stop" } else { "Listen" }
                title=move || if playing() { "Stop reading" } else { "Listen to answer" }
                on:click=move |_| {
                    if let Some(reply) = row.with_untracked(|turn| turn.reply.clone()) {
                        voice.engine.with_value(|engine| id.with_value(|id| engine.play(id, &reply)));
                    }
                }><span class="appearance-icon" inner_html=move || icons.with_value(|icons| if playing() { icons.stop.clone() } else { icons.listen.clone() })></span></button>
            {move || { let message = error(); (!message.is_empty()).then(|| view! { <span class="k-field__error" role="status">{message}</span> }) }}
        </span>
    }.into_any()
}
