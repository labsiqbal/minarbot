use super::*;
use serde_json::json;
use web_sys::HtmlFormElement;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = MinarbotConversation, catch)]
    async fn upload(
        path: &str,
        file: web_sys::File,
    ) -> Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>;

    #[wasm_bindgen(js_namespace = MinarbotConversation)]
    fn record(
        state: &js_sys::Function,
        file: &js_sys::Function,
        error: &js_sys::Function,
    ) -> js_sys::Array;
}

fn files(chat: Chat, selected: web_sys::FileList, uploading: RwSignal<bool>) {
    upload_files(
        chat,
        (0..selected.length())
            .filter_map(|index| selected.get(index))
            .collect(),
        uploading,
    );
}

fn upload_files(chat: Chat, selected: Vec<web_sys::File>, uploading: RwSignal<bool>) {
    if chat.locked() || uploading.get_untracked() {
        return;
    }
    let length = selected.len();
    if length + chat.files.with_untracked(Vec::len) > MAX_FILES {
        chat.error.set("Attach up to four files.".into());
        return;
    }
    if length == 0 {
        return;
    }
    uploading.set(true);
    let path = format!("{}/files", chat.base());
    leptos::task::spawn_local(async move {
        for file in selected {
            let result = upload(&path, file).await;
            if chat.files.is_disposed() {
                return;
            }
            match result {
                Ok(value) => {
                    if let Some(id) = value.as_string().filter(|id| protocol::id(id)) {
                        let mut files = chat.files.get_untracked();
                        if !files.contains(&id) {
                            files.push(id);
                            chat.attach(files);
                        }
                    }
                }
                Err(reason) => {
                    chat.error.set(
                        reason
                            .as_string()
                            .unwrap_or_else(|| "Upload failed.".into()),
                    );
                    break;
                }
            }
        }
        uploading.set(false);
    });
}

pub(super) fn dialog_open(dialog: NodeRef<leptos::html::Dialog>) {
    if let Some(dialog) = dialog.get_untracked() {
        let _ = dialog.show_modal();
    }
}

type Recording = (
    js_sys::Array,
    Closure<dyn FnMut(String)>,
    Closure<dyn FnMut(web_sys::File)>,
    Closure<dyn FnMut(String)>,
);

fn recording_control(
    chat: Chat,
    recording: RwSignal<&'static str>,
    uploading: RwSignal<bool>,
) -> impl IntoView {
    let icons = use_context::<Icons>().unwrap_or_default();
    let session = StoredValue::new_local(None::<Recording>);
    let started = RwSignal::new(0.0f64);
    let elapsed = RwSignal::new(0u64);
    let invoke = move |index| {
        session.with_value(|session| {
            if let Some((actions, _, _, _)) = session {
                if let Ok(action) = actions.get(index).dyn_into::<js_sys::Function>() {
                    let _ = action.call0(&wasm_bindgen::JsValue::NULL);
                }
            }
        });
    };
    let unload = window_event_listener(leptos::ev::pagehide, move |_| invoke(1));
    on_cleanup(move || {
        unload.remove();
        invoke(1);
    });
    Effect::new(move |_| {
        if recording.get() != "recording" {
            return;
        }
        let timer = set_interval_with_handle(
            move || {
                elapsed.set(
                    ((js_sys::Date::now() - started.get_untracked()) / 1000.0).max(0.0) as u64,
                );
            },
            Duration::from_millis(250),
        )
        .ok();
        on_cleanup(move || {
            if let Some(timer) = timer {
                timer.clear();
            }
        });
    });
    view! {
        <span class="voice-input">
            <button type="button" class="k-icon-btn" aria-label="Record audio" title="Record audio"
                hidden=move || recording.get() != "idle" || chat.locked() || uploading.get() || chat.files.with(|files| files.len() >= MAX_FILES)
                on:click=move |_| {
                    if recording.get_untracked() != "idle" || chat.locked() || uploading.get_untracked() { return }
                    elapsed.set(0);
                    let state = Closure::<dyn FnMut(String)>::new(move |state: String| {
                        let state = match state.as_str() {
                            "requesting" => "requesting",
                            "recording" => { started.try_set(js_sys::Date::now()); "recording" }
                            "stopping" => "stopping",
                            _ => "idle",
                        };
                        recording.try_set(state);
                    });
                    let file = Closure::<dyn FnMut(web_sys::File)>::new(move |file| {
                        if !chat.files.is_disposed() { upload_files(chat, vec![file], uploading); }
                    });
                    let error = Closure::<dyn FnMut(String)>::new(move |error| { chat.error.try_set(error); });
                    let actions = record(state.as_ref().unchecked_ref(), file.as_ref().unchecked_ref(), error.as_ref().unchecked_ref());
                    session.set_value(Some((actions, state, file, error)));
                }><span class="appearance-icon" inner_html=icons.mic></span></button>
            <span class="voice-input__pill" role="group" aria-label="Audio recording" hidden=move || recording.get() == "idle">
                <span class="voice-input__dot" aria-hidden="true" hidden=move || recording.get() != "recording"></span>
                <span class="voice-input__timer" role="timer">
                    {move || match recording.get() {
                        "recording" => { let seconds = elapsed.get(); format!("{}:{:02}", seconds / 60, seconds % 60) }
                        "requesting" => "Connecting…".into(),
                        _ => "Finishing…".into(),
                    }}
                </span>
                <button type="button" class="k-icon-btn" aria-label="Stop recording" hidden=move || recording.get() != "recording"
                    on:click=move |_| invoke(0)><span class="appearance-icon" inner_html=icons.stop></span></button>
                <button type="button" class="k-icon-btn" aria-label="Cancel recording" on:click=move |_| invoke(1)>
                    <span class="appearance-icon" inner_html=icons.close></span>
                </button>
            </span>
        </span>
    }
}

pub fn composer(chat: Chat, rows: Memo<Vec<Turn>>) -> impl IntoView {
    let input = NodeRef::<leptos::html::Textarea>::new();
    let file_input = NodeRef::<leptos::html::Input>::new();
    let discard = NodeRef::<leptos::html::Dialog>::new();
    let mode_dialog = NodeRef::<leptos::html::Dialog>::new();
    let mode = RwSignal::new("");
    let prompt = RwSignal::new(String::new());
    let interval = RwSignal::new(5);
    let uploading = RwSignal::new(false);
    let recording = RwSignal::new("idle");
    let icons = use_context::<Icons>().unwrap_or_default();
    let label = chat.config.with_value(|c| {
        if c.channel {
            format!(
                "Message #{}",
                c.room
                    .as_ref()
                    .and_then(|room| room.section_name.as_deref())
                    .unwrap_or(&c.name)
            )
        } else {
            format!(
                "Message {}",
                c.agent
                    .as_ref()
                    .map_or(c.name.as_str(), |agent| agent.name.as_str())
            )
        }
    });
    let submit = move |steer| {
        if uploading.get_untracked() || recording.get_untracked() != "idle" {
            return;
        }
        if chat.pending.with_untracked(Option::is_none) && !chat.locked() {
            let draft = chat.draft.get_untracked();
            let mut parts = draft.trim().splitn(2, char::is_whitespace);
            if let Some(command @ ("/goal" | "/loop")) = parts.next() {
                let rest = parts.next().unwrap_or_default().trim_start();
                if command == "/goal" {
                    mode.set("goal");
                    prompt.set(rest.into());
                } else {
                    if !chat.files.with_untracked(Vec::is_empty) {
                        chat.error
                            .set("Remove attachments before scheduling a loop.".into());
                        return;
                    }
                    let mut parts = rest.splitn(2, char::is_whitespace);
                    let minutes = parts.next().and_then(|value| {
                        let multiplier = match value.as_bytes().last() {
                            Some(b'h') => 60,
                            Some(b'd') => 1440,
                            _ => 1,
                        };
                        value
                            .trim_end_matches(['m', 'h', 'd'])
                            .parse::<u32>()
                            .ok()?
                            .checked_mul(multiplier)
                    });
                    let Some(minutes) = minutes.filter(|value| (1..=525_600).contains(value))
                    else {
                        chat.error
                            .set("Enter a loop interval from 1m to 365d.".into());
                        return;
                    };
                    interval.set(minutes);
                    mode.set("loop");
                    prompt.set(parts.next().unwrap_or_default().trim_start().into());
                }
                dialog_open(mode_dialog);
                return;
            }
        }
        chat.send(steer);
    };
    let pending_mode = saved(&chat.key("goal-loop"));
    let mode_pending = RwSignal::new(pending_mode);
    if mode_pending.with_untracked(Option::is_some) {
        if let Some(raw) = mode_pending.get_untracked() {
            if let Ok(value) = serde_json::from_str::<Value>(&raw) {
                if let Some(kind @ ("goal" | "loop")) = value["mode"].as_str() {
                    mode.set(if kind == "goal" { "goal" } else { "loop" });
                }
                prompt.set(
                    value["body"]["target"]
                        .as_str()
                        .or_else(|| value["body"]["prompt"].as_str())
                        .unwrap_or_default()
                        .into(),
                );
            }
        }
        request_animation_frame(move || dialog_open(mode_dialog));
    }
    let mentions = Memo::new(move |_| {
        if !chat.config.with_value(|c| c.channel) || chat.locked() {
            return Vec::new();
        }
        let draft = chat.draft.get();
        let Some(query) = draft
            .split_whitespace()
            .last()
            .and_then(|word| word.strip_prefix('@'))
        else {
            return Vec::new();
        };
        if draft.ends_with(char::is_whitespace) {
            return Vec::new();
        }
        let query = query.to_lowercase();
        chat.config.with_value(|c| {
            c.members
                .iter()
                .filter(|member| member.name.to_lowercase().starts_with(&query))
                .map(|m| m.name.clone())
                .collect()
        })
    });
    let attachments = Memo::new(move |_| {
        chat.pending
            .with(|pending| {
                pending
                    .as_ref()
                    .and_then(|pending| pending.command.files.clone())
            })
            .unwrap_or_else(|| chat.files.get())
    });
    let file_names = RwSignal::new(Vec::<FileRef>::new());
    Effect::new(move |_| {
        if attachments.with(Vec::is_empty) {
            return;
        }
        let path = format!("{}/files", chat.base());
        leptos::task::spawn_local(async move {
            if let Ok(list) = get::<Vec<FileRef>>(&path).await {
                file_names.try_set(list);
            }
        });
    });
    let channel = chat.config.with_value(|config| config.channel);
    let active = Memo::new(move |_| chat.target().map(|run| run.id));
    let value = move || {
        chat.pending
            .with(|pending| pending.as_ref().map(|pending| pending.command.text.clone()))
            .unwrap_or_else(|| chat.draft.get())
    };
    Effect::new(move |_| {
        let _ = value();
        if input.get().is_some() {
            request_animation_frame(move || {
                if let Some(element) = input.get_untracked() {
                    let style = element.unchecked_ref::<web_sys::HtmlElement>().style();
                    let _ = style.set_property("height", "0px");
                    let height = element.scroll_height();
                    let _ = style.set_property("height", &format!("{}px", height.min(200)));
                    let _ = style
                        .set_property("overflow-y", if height > 200 { "auto" } else { "hidden" });
                }
            });
        }
    });
    let send_label = move || {
        if chat.busy.get() {
            "Sending..."
        } else if chat.pending.with(Option::is_some) {
            "Retry send"
        } else if chat.target().is_some() {
            "Queue message"
        } else {
            "Send message"
        }
    };
    view! {
        <footer class="composer-area">
            <div class="composer__inner">
                <p role="alert" class="composer__error" hidden=move || chat.error.with(String::is_empty)>{chat.error}</p>
                <p role="status" class="composer__note" hidden=move || chat.pending.with(Option::is_none) || chat.busy.get()>
                    "Send outcome is uncertain. Retry uses the same request."
                </p>
                <div class="composer__error" hidden=move || !chat.blocked.get()>
                    <p role="alert">"Message recovery data could not be read. Check history before sending."</p>
                    <button type="button" class="k-btn k-btn--sm k-btn--secondary" on:click=move |_| dialog_open(discard)>"Discard message recovery"</button>
                </div>
                <p class="composer__note" hidden=move || !chat.config.with_value(|c| c.archived)>"Archived"</p>
                <p class="composer__note" hidden=move || chat.config.with_value(|c| c.archived) || chat.can_send()><a class="k-link" href="/settings/providers">"Connect a provider"</a></p>
                {goal_chip(chat)}
                {queue::queued_runs(chat, rows)}
                <Show when=move || chat.context.with(Option::is_some)>
                <div class="k-inline">
                    <span role="status">"Reply context attached"</span>
                    <button type="button" class="k-btn k-btn--sm k-btn--ghost" disabled=move || chat.locked() on:click=move |_| chat.context.set(None)>"Remove context"</button>
                </div>
                </Show>
                <div class="k-inline">
                    {move || attachments.get().into_iter().map(|id| {
                        let title = file_names.with(|rows| rows.iter().find(|file| file.id == id).map(|file| file.name.clone())).unwrap_or_else(|| "Attachment".into());
                        view! {
                            <span class="k-badge">{title.clone()}</span>
                            <button type="button" class="k-btn k-btn--sm k-btn--ghost" disabled=move || chat.locked()
                                aria-label=format!("Remove {title}") on:click=move |_| {
                                    let mut files = chat.files.get_untracked();
                                    files.retain(|file| *file != id); chat.attach(files);
                                }>"Remove"</button>
                        }
                    }).collect_view()}
                </div>
                <form class="composer" class:is-multiline=move || value().contains('\n') aria-label="Composer" on:submit=move |event| { event.prevent_default(); submit(false); }
                    on:dragover=move |event: web_sys::DragEvent| {
                        if event.data_transfer().is_some_and(|data| data.types().iter().any(|kind| kind.as_string().as_deref() == Some("Files"))) {
                            event.prevent_default();
                        }
                    }
                    on:drop=move |event: web_sys::DragEvent| {
                        if let Some(list) = event.data_transfer().and_then(|data| data.files()).filter(|list| list.length() != 0) {
                            event.prevent_default(); files(chat, list, uploading);
                        }
                    }>
                    <div class="composer__left">
                    <label class="sr-only" for="message">{label.clone()}</label>
                    <textarea node_ref=input class="composer__input" id="message" rows="1" maxlength=MAX_MESSAGE
                        placeholder=label readonly=move || chat.locked()
                        prop:value=value
                        on:input=move |event| chat.draft(event_target_value(&event))
                        on:paste=move |event: web_sys::ClipboardEvent| {
                            if let Some(list) = event.clipboard_data().and_then(|data| data.files()).filter(|list| list.length() != 0) {
                                event.prevent_default(); files(chat, list, uploading);
                            }
                        }
                        on:keydown=move |event| {
                            if event.key() == "Enter" && !event.shift_key() && !event.is_composing() && event.key_code() != 229 {
                                event.prevent_default(); submit(event.ctrl_key() || event.meta_key());
                            }
                        } />
                    </div>
                    {move || mentions.get().into_iter().map(|name| {
                        let label = name.clone();
                        view! { <button type="button" class="k-btn k-btn--sm k-btn--secondary" on:click=move |_| {
                            let mut draft = chat.draft.get_untracked();
                            if let Some(at) = draft.rfind('@') { draft.truncate(at + 1); draft.push_str(&name); draft.push(' '); chat.draft(draft); }
                            if let Some(input) = input.get_untracked() { let _ = input.focus(); }
                        }>{format!("@{label}")}</button> }
                    }).collect_view()}
                    <div class="composer__toolbar">
                        <input node_ref=file_input type="file" multiple hidden on:change=move |_| {
                            if let Some(element) = file_input.get_untracked() {
                                if let Some(list) = element.files() { files(chat, list, uploading); }
                                element.set_value("");
                            }
                        } />
                        <span class="composer__plus-anchor">
                            <button type="button" class="composer__btn" aria-label="Attach conversation file" title="Attach file"
                                disabled=move || chat.locked() || uploading.get() || recording.get() != "idle" || attachments.with(|files| files.len() >= MAX_FILES)
                                on:click=move |_| { if let Some(element) = file_input.get_untracked() { element.click(); } }>
                                <span class="appearance-icon" inner_html=icons.attach></span>
                            </button>
                        </span>
                        {channel.then(|| rooms::addressed_member(chat, icons.chevron.clone()))}
                        <div class="composer__right">
                            <span class="composer__retry" hidden=move || chat.busy.get() || chat.pending.with(Option::is_none)>"Retry send"</span>
                            {recording_control(chat, recording, uploading)}
                            <button class="composer__cta" type="submit" aria-label=send_label title=send_label
                                hidden=move || active.with(Option::is_some) && value().trim().is_empty() && attachments.with(Vec::is_empty) && chat.pending.with(Option::is_none)
                                disabled=move || chat.busy.get() || chat.blocked.get() || uploading.get() || recording.get() != "idle" || chat.config.with_value(|c| c.archived)
                                    || chat.pending.with(Option::is_none) && (!chat.can_send() || chat.draft.with(|draft| draft.trim().is_empty()) && attachments.with(Vec::is_empty))>
                                <span class="step__spin" aria-hidden="true" hidden=move || !chat.busy.get()></span>
                                <span class="appearance-icon" inner_html=icons.send hidden=chat.busy></span>
                            </button>
                            {if channel { rooms::stop(chat, rows, icons.stop.clone()).into_any() } else {
                                view! { <For each=move || active.get() key=String::clone children=move |id| queue::cancel_button(chat, id, true) /> }.into_any()
                            }}
                        </div>
                    </div>
                </form>
            </div>
            <dialog node_ref=discard class="conversation-dialog k-stack" aria-label="Discard recovery data?">
                <p>"Check history before discarding recovery data. The message may already have been received."</p>
                <form method="dialog" class="k-row-actions">
                    <button class="k-btn k-btn--sm k-btn--secondary">"Cancel"</button>
                    <button class="k-btn k-btn--sm k-btn--primary" on:click=move |_| {
                        if remove(&chat.key("pending")) { chat.blocked.set(false); }
                        else { chat.error.set("Recovery data could not be cleared.".into()); }
                    }>"Discard message recovery"</button>
                </form>
            </dialog>
            {mode_form(chat, mode_dialog, mode, prompt, interval, mode_pending)}
            <p class="composer__foot" hidden=move || chat.focus.with(String::is_empty) && chat.before().is_none()>
                {move || if chat.focus.with(String::is_empty) { "Older messages · 100 per page" } else if channel { "Task family shown" } else { "Selected message shown" }}
            </p>
        </footer>
    }
}

fn field(form: &HtmlFormElement, name: &str) -> String {
    web_sys::FormData::new_with_form(form)
        .ok()
        .and_then(|data| data.get(name).as_string())
        .unwrap_or_default()
}

fn mode_form(
    chat: Chat,
    dialog: NodeRef<leptos::html::Dialog>,
    mode: RwSignal<&'static str>,
    prompt: RwSignal<String>,
    interval: RwSignal<u32>,
    pending: RwSignal<Option<String>>,
) -> impl IntoView {
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let scheduled = RwSignal::new(false);
    let form = NodeRef::<leptos::html::Form>::new();
    let title = move || {
        if mode.get() == "goal" {
            "Start goal"
        } else {
            "Schedule loop"
        }
    };
    view! {
        <div class="composer__note" role="status" hidden=move || !scheduled.get()>"Loop scheduled. " <a class="k-link" href="/routines">"View routines"</a></div>
        <dialog node_ref=dialog class="conversation-dialog" aria-label=title on:cancel=move |event: web_sys::Event| { if busy.get_untracked() { event.prevent_default(); } }>
            <h2>{title}</h2>
            <form node_ref=form class="k-stack" on:submit=move |event| {
                event.prevent_default();
                if busy.get_untracked() { return }
                let key = chat.key("goal-loop");
                let raw = if let Some(raw) = pending.get_untracked() { raw } else {
                    let Some(form) = form.get_untracked() else { return };
                    let Some(id) = uuid() else { return };
                    let text = field(&form, "prompt");
                    let text = text.trim();
                    if text.is_empty() { error.set("Enter a prompt.".into()); return }
                    let number = |name| field(&form, name).parse::<u32>().unwrap_or(0);
                    let kind = mode.get_untracked();
                    let mut body = if kind == "goal" {
                        json!({"id": id, "target": text, "maxTurns": number("turns"), "maxMinutes": number("minutes"), "maxTokens": number("tokens")})
                    } else {
                        json!({"id": id, "prompt": text, "intervalSeconds": number("interval") * 60, "maxRuns": number("runs")})
                    };
                    if kind == "goal" && !chat.files.with_untracked(Vec::is_empty) { body["files"] = json!(chat.files.get_untracked()); }
                    let until = field(&form, "until");
                    if kind == "loop" && !until.trim().is_empty() { body["until"] = json!(until.trim()); }
                    let recipient = field(&form, "agent");
                    if protocol::id(&recipient) { body["recipient"] = json!(recipient); }
                    json!({"mode": kind, "body": body}).to_string()
                };
                let Ok(command) = serde_json::from_str::<Value>(&raw) else { error.set("Saved request could not be read.".into()); return };
                let Some(kind @ ("goal" | "loop")) = command["mode"].as_str() else { error.set("Saved request could not be read.".into()); return };
                if !command["body"]["id"].as_str().is_some_and(protocol::id) || !save(&key, &raw) {
                    error.set("Request recovery could not be saved.".into()); return
                }
                let loop_mode = kind == "loop";
                let path = format!("{}/{kind}", chat.base());
                let body = command["body"].to_string();
                pending.set(Some(raw)); busy.set(true); error.set(String::new());
                leptos::task::spawn_local(async move {
                    let result = post::<Value>(&path, &body).await;
                    if busy.is_disposed() { return }
                    match result {
                        Ok(_) => {
                            let _ = remove(&key); pending.set(None); chat.draft(String::new()); chat.attach(Vec::new());
                            if loop_mode { scheduled.set(true); }
                            if let Some(dialog) = dialog.get_untracked() { dialog.close(); }
                        }
                        Err(reason) => { if reason.definitive && remove(&key) { pending.set(None); } error.set(reason.message); }
                    }
                    busy.set(false);
                });
            }>
                <label class="k-field"><span>{move || if mode.get() == "goal" { "Target" } else { "Prompt" }}</span>
                    <textarea class="k-input" name="prompt" prop:value=prompt required maxlength="12000" readonly=move || pending.with(Option::is_some) />
                </label>
                {move || if mode.get() == "goal" {
                    view! { <div class="goal-mode__limits">
                        {number_field("Max turns", "turns", 100, 20)}
                        {number_field("Max minutes", "minutes", 1440, 120)}
                        {number_field("Token budget", "tokens", 10_000_000, 250_000)}
                    </div> }.into_any()
                } else {
                    view! { <div class="goal-mode__limits">
                        {number_field("Interval (minutes)", "interval", 525_600, interval.get())}
                        {number_field("Max runs", "runs", 1000, 20)}
                        <label class="k-field"><span>"Until"</span><textarea class="k-input" name="until" maxlength="4000"></textarea></label>
                    </div> }.into_any()
                }}
                {chat.config.with_value(|c| c.channel.then(|| view! {
                    <label class="k-field"><span>"Agent"</span><select class="k-input" name="agent">
                        <option value="">"Default agent"</option>
                        {c.members.iter().map(|agent| view! { <option value=agent.id.clone()>{agent.name.clone()}</option> }).collect_view()}
                    </select></label>
                }))}
                <p role="alert" class="message-error" hidden=move || error.with(String::is_empty)>{error}</p>
                <div class="k-row-actions">
                    <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=busy
                        on:click=move |_| { if let Some(dialog) = dialog.get_untracked() { dialog.close(); } }>"Cancel"</button>
                    <button type="submit" class="k-btn k-btn--sm k-btn--primary" disabled=busy>{move || if pending.with(Option::is_some) { "Retry request" } else { title() }}</button>
                </div>
            </form>
        </dialog>
    }
}

fn number_field(label: &'static str, name: &'static str, max: u32, value: u32) -> impl IntoView {
    view! { <label class="k-field"><span>{label}</span><input class="k-input" type="number" name=name required min=1 max=max value=value /></label> }
}

fn goal_chip(chat: Chat) -> impl IntoView {
    let result = RwSignal::new(Value::Null);
    fn refresh(chat: Chat, result: RwSignal<Value>) {
        if result.is_disposed() {
            return;
        }
        let path = format!("{}/goal", chat.base());
        leptos::task::spawn_local(async move {
            if let Ok(value) = get(&path).await {
                result.try_set(value);
            }
            if !result.is_disposed() {
                set_timeout(move || refresh(chat, result), Duration::from_millis(1800));
            }
        });
    }
    refresh(chat, result);
    view! {
        {move || result.with(|goal| {
            let target = goal["target"].as_str()?.to_owned();
            let turn = goal["current_turn"].as_str().unwrap_or_default().to_owned();
            let status = goal["status"].as_str().unwrap_or_default();
            let busy = RwSignal::new(false);
            let endpoint = format!("{}/goal/stop", chat.base());
            let goal_id = goal["id"].clone();
            let active = matches!(status, "active" | "judging" | "needs_you" | "paused");
            let status = match status {
                "active" => "Working", "judging" => "Judging", "stopped" => "Stopped", "needs_you" => "Needs you",
                "met" => "Met", "paused" => "Paused", "failed" => "Failed", "interrupted" => "Interrupted",
                "token_limit" => "Token limit", "turn_limit" => "Turn limit", "wall_limit" => "Time limit",
                "no_progress" => "No progress", _ => "Goal",
            };
            Some(view! { <div class="composer__note goal-chip" aria-label="Goal progress">
                <button class="k-link" type="button" on:click=move |_| chat.inspect(turn.clone())>{target}</button>
                <div class="goal-chip__limits" role="status"><span class="k-badge">{status}</span>
                    <span>{format!("{}/{} turns", goal["turn_count"], goal["max_turns"])}</span>
                    <span>{format!("{}/{} tokens", goal["tokens"], goal["max_tokens"])}</span>
                </div>
                {active.then(|| view! { <button type="button" class="k-btn k-btn--sm k-btn--ghost" aria-label="Stop goal" disabled=busy
                    on:click=move |_| mutate(endpoint.clone(), json!({"goalId": goal_id}), busy, chat.error)>"Stop"</button> })}
            </div> })
        })}
    }
}

pub fn mutate(path: String, body: Value, busy: RwSignal<bool>, error: RwSignal<String>) {
    mutate_then(path, body, busy, error, || {});
}

pub(super) fn mutate_then(
    path: String,
    mut body: Value,
    busy: RwSignal<bool>,
    error: RwSignal<String>,
    complete: impl FnOnce() + 'static,
) {
    if busy.get_untracked() {
        return;
    }
    let key = format!("minarbot:leptos-action:{path}");
    let raw = if let Some(raw) = saved(&key) {
        let Ok(previous) = serde_json::from_str::<Value>(&raw) else {
            error.set("Saved decision could not be read.".into());
            return;
        };
        let Some(id) = previous["id"].as_str().filter(|id| protocol::id(id)) else {
            return;
        };
        body["id"] = json!(id);
        if body != previous {
            error.set("Retry the original request to resolve its outcome.".into());
            return;
        }
        raw
    } else {
        let Some(id) = uuid() else { return };
        body["id"] = json!(id);
        body.to_string()
    };
    if !save(&key, &raw) {
        error.set("Request recovery could not be saved.".into());
        return;
    }
    busy.set(true);
    error.set(String::new());
    leptos::task::spawn_local(async move {
        let result = post::<Value>(&path, &raw).await;
        if busy.is_disposed() {
            return;
        }
        match result {
            Ok(_) => {
                if remove(&key) {
                    complete();
                } else {
                    error.set("Change saved. Recovery data could not be cleared.".into());
                }
            }
            Err(reason) => {
                if reason.definitive {
                    let _ = remove(&key);
                }
                error.set(reason.message);
            }
        }
        busy.set(false);
    });
}

pub fn turn_controls(chat: Chat, row: RwSignal<Turn>, skip: bool) -> impl IntoView {
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let confirm = NodeRef::<leptos::html::Dialog>::new();
    let cancel = NodeRef::<leptos::html::Dialog>::new();
    let scope = StoredValue::new(format!(
        "{}/turns/{}",
        chat.base(),
        row.with_untracked(|turn| turn.id.clone())
    ));
    let questions = Memo::new(move |_| row.with(|turn| turn.questions.clone()));
    let approvals = Memo::new(move |_| row.with(|turn| turn.approvals.clone()));
    let harness = Memo::new(move |_| row.with(|turn| turn.harness_approvals.clone()));
    let icons = use_context::<Icons>().unwrap_or_default();
    let menu = NodeRef::<leptos::html::Details>::new();
    let active = move || {
        row.with(|turn| {
            turn.active()
                && !turn.waiting_for_answer()
                && turn.computer_wait.is_none()
                && turn.dispatch_mode.as_deref() != Some("steer")
                && turn.paused == 0
                && !turn.approvals.iter().any(|approval| {
                    matches!(approval["state"].as_str(), Some("approval" | "ready"))
                })
        })
    };
    let resumable = move || {
        row.with(|turn| {
            !turn.needs_you()
                && turn.computer_wait.is_none()
                && !matches!(turn.status.as_str(), "done" | "cancelled")
                && turn.dispatch_mode.as_deref() != Some("steer")
                && (turn.paused != 0 || matches!(turn.status.as_str(), "failed" | "interrupted"))
        })
    };
    let available =
        move || !chat.config.with_value(|config| config.archived) && (active() || resumable());
    let finished = move || {
        row.with(|turn| {
            matches!(
                turn.status.as_str(),
                "done" | "cancelled" | "failed" | "interrupted"
            )
        })
    };
    let recover = move || {
        let base = scope.get_value();
        ["pause", "resume", "reassign", "cancel"]
            .into_iter()
            .find_map(|action| {
                let path = format!("{base}/{action}");
                let raw = saved(&format!("minarbot:leptos-action:{path}"))?;
                let body = serde_json::from_str::<Value>(&raw).ok()?;
                body["id"].as_str().filter(|id| protocol::id(id))?;
                Some((path, body))
            })
    };
    let retry = Memo::new(move |_| {
        busy.track();
        recover().is_some()
    });
    let close = move || {
        if let Some(menu) = menu.get_untracked() {
            menu.set_open(false);
        }
    };
    view! {
        <div class="turn-extras k-stack" hidden=move || questions.with(Vec::is_empty) && approvals.with(Vec::is_empty) && harness.with(Vec::is_empty)>
        {move || questions.get().into_iter().map(|q| question(chat, row, scope.get_value(), q)).collect_view()}
        {move || approvals.get().into_iter().map(|a| approval(chat, scope.get_value(), a, false)).collect_view()}
        {move || harness.get().into_iter().map(|a| approval(chat, scope.get_value(), a, true)).collect_view()}
        </div>
        <div class="turn-controls" role="group" aria-label="Message actions" hidden=move || !finished() && !available() && !retry.get() && error.with(String::is_empty)>
            {answer_actions(chat, row)}
            {message_actions::checkpoint(chat, row)}
            {message_actions::listen(row)}
            <button type="button" class="k-btn k-btn--sm k-btn--ghost" disabled=busy hidden=move || !retry.get()
                on:click=move |_| { if let Some((path, body)) = recover() { mutate_then(path, body, busy, error, move || chat.refresh()); } }>"Retry control"</button>
            <button type="button" class="k-btn k-btn--sm k-btn--ghost" hidden=move || !available() || !resumable() || retry.get() disabled=busy
                on:click=move |_| dialog_open(confirm)>
                {move || if row.with(|turn| turn.status == "failed") { "Retry" } else { "Resume" }}
            </button>
            <details class="chat-pilot-menu turn-task-controls" node_ref=menu hidden=move || !available() || retry.get()
                on:keydown=move |event| {
                    if event.key() == "Escape" {
                        event.prevent_default(); close();
                        if let Some(summary) = menu.get_untracked().and_then(|menu| menu.query_selector("summary").ok().flatten())
                            .and_then(|summary| summary.dyn_into::<web_sys::HtmlElement>().ok()) { let _ = summary.focus(); }
                    }
                }>
                <summary class=move || if resumable() || skip { "k-icon-btn" } else { "k-btn k-btn--sm k-btn--ghost" }
                    aria-label=move || if resumable() { "Recovery actions" } else if skip { "Work log actions" } else { "Task actions" }>
                    <span hidden=move || skip || resumable()>"Task actions"</span>
                    <span class="appearance-icon" hidden=move || skip || resumable() inner_html=icons.chevron></span>
                    <span class="appearance-icon" hidden=move || !skip && !resumable() inner_html=icons.more></span>
                </summary>
                <div class="k-pop task-actions" role="menu">
                    <button type="button" class="k-menu__item" role="menuitem" disabled=busy hidden=move || !available() || !skip && !resumable()
                        on:click=move |_| {
                            close();
                            mutate_then(format!("{}/cancel", scope.get_value()), json!({ "revision": row.with_untracked(|turn| turn.revision), "acknowledgeUncertain": false }), busy, error, move || chat.refresh());
                        }>
                        {move || if resumable() { "Dismiss" } else { "Skip" }}
                    </button>
                    <button type="button" class="k-menu__item" role="menuitem" disabled=busy hidden=move || !active()
                        on:click=move |_| {
                            close();
                            mutate_then(format!("{}/pause", scope.get_value()), json!({"revision": row.with_untracked(|turn| turn.revision)}), busy, error, move || chat.refresh());
                        }>"Pause"</button>
                    <button type="button" class="k-menu__item" disabled=busy hidden=move || !resumable()
                        on:click=move |_| { close(); dialog_open(confirm); }>"Resume task"</button>
                </div>
            </details>
            {move || (!error.with(String::is_empty)).then(|| view! { <p role="alert" class="message-error">{error}</p> })}
            {resume_dialog(chat, row, scope, confirm, busy, error)}
            <button type="button" class="k-btn k-btn--sm k-btn--ghost" disabled=busy
                hidden=move || skip || chat.config.with_value(|config| config.archived) || row.with(|turn| turn.status != "queued" || chat.queued(turn))
                on:click=move |_| dialog_open(cancel)>
                {move || row.with(|turn| if turn.waiting_for.is_some() { "Cancel task and delegation" } else if turn.paused != 0 || turn.needs_you() { "Cancel task" } else { "Cancel queued task" })}
            </button>
            <dialog class="conversation-dialog" node_ref=cancel aria-label="Stop this task?">
                <form method="dialog" class="k-stack">
                    <p>"Steps already sent may have taken effect."</p>
                    <div class="k-row-actions">
                        <button type="submit" class="k-btn k-btn--secondary">"Cancel"</button>
                        <button type="submit" class="k-btn k-btn--danger" on:click=move |_| {
                            mutate_then(format!("{}/cancel", scope.get_value()), json!({}), busy, error, move || chat.refresh());
                        }>"Stop task"</button>
                    </div>
                </form>
            </dialog>
        </div>
    }
}

fn question(chat: Chat, row: RwSignal<Turn>, path: String, q: Value) -> impl IntoView {
    let id = q["id"]
        .as_str()
        .filter(|id| protocol::id(id))
        .unwrap_or_default();
    let id = id.to_owned();
    let label = q["prompt"].as_str().unwrap_or("Question").to_owned();
    let choices = q["choices"].as_array().cloned().unwrap_or_default();
    let answer = q["answer"].as_str().map(str::to_owned);
    let busy = RwSignal::new(false);
    let message = RwSignal::new(String::new());
    let form = NodeRef::<leptos::html::Form>::new();
    let acknowledged = RwSignal::new(false);
    let interrupted = move || row.with(|turn| turn.status == "interrupted");
    let unavailable = move || {
        chat.config.with_value(|c| c.archived)
            || row.with(|turn| !matches!(turn.status.as_str(), "queued" | "interrupted"))
    };
    let answer_key = StoredValue::new(format!("minarbot:answer:{id}:draft"));
    let draft = RwSignal::new(saved(&answer_key.get_value()).unwrap_or_default());
    let endpoint = format!("{path}/questions/{id}/answer");
    if answer.is_some() {
        let _ = remove(&answer_key.get_value());
    }
    view! {
        <section class="k-card conversation-card" aria-label="Agent question">
            <h3>{label}</h3>
            {answer.map(|answer| view! { <p>{format!("Answer: {answer}")}</p> }).map(IntoAny::into_any).unwrap_or_else(move || view! {
                <form class="k-stack" node_ref=form on:submit=move |event| {
                    event.prevent_default();
                    if let Some(form) = form.get_untracked() {
                        if !unavailable() && (!interrupted() || acknowledged.get_untracked()) {
                            mutate(endpoint.clone(), json!({"text": field(&form, "answer").trim(), "acknowledgeUncertain": acknowledged.get_untracked()}), busy, message);
                        }
                    }
                }>
                    {if choices.is_empty() {
                        view! { <label class="k-field"><span>"Answer"</span><textarea name="answer" class="k-input" maxlength="4000" required
                            prop:value=draft on:input=move |event| {
                                let text = event_target_value(&event);
                                let _ = save(&answer_key.get_value(), &text);
                                draft.set(text);
                            }></textarea></label> }.into_any()
                    } else {
                        view! { <fieldset class="k-fieldset"><legend>"Answer"</legend>
                            {choices.into_iter().filter_map(|choice| choice.as_str().map(str::to_owned)).map(|choice| view! {
                                <label class="k-check"><input type="radio" name="answer" value=choice.clone() required />{choice}</label>
                            }).collect_view()}
                        </fieldset> }.into_any()
                    }}
                    <label class="k-check" hidden=move || !interrupted()><input type="checkbox" prop:checked=acknowledged
                        on:change=move |event| acknowledged.set(event_target_checked(&event)) />
                        "I have checked the effects of the task before continuing"
                    </label>
                    <button type="submit" class="k-btn k-btn--sm k-btn--primary" disabled=move || busy.get() || unavailable() || interrupted() && !acknowledged.get()>"Answer"</button>
                </form>
            }.into_any())}
            <p class="message-error" role="alert" hidden=move || message.with(String::is_empty)>{message}</p>
        </section>
    }
}

fn approval(chat: Chat, path: String, a: Value, harness: bool) -> impl IntoView {
    let id = a["id"]
        .as_str()
        .filter(|id| protocol::id(id))
        .unwrap_or_default();
    let resource = if harness {
        "harness-approvals"
    } else {
        "app-approvals"
    };
    let path = format!("{path}/{resource}/{id}");
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let open = a["state"] == "approval";
    let title = a["tool"]
        .as_str()
        .or_else(|| a["description"].as_str())
        .unwrap_or("Approval")
        .to_owned();
    let details = a
        .get(if harness { "input" } else { "arguments" })
        .and_then(|value| serde_json::to_string_pretty(value).ok());
    let revision = a["revision"].as_u64().unwrap_or_default();
    let form = a["kind"] == "form";
    let form_result = if a["state"] == "expired" {
        "Form expired"
    } else if a["decision"] == "deny" {
        "Form skipped"
    } else {
        "Form submitted"
    }
    .to_owned();
    if form {
        return view! {
            <div class="k-card component-reply"><section class="component-node">
                {panels::form_reply(chat, a["input"].clone(), path, revision, open, form_result, busy, error)}
            </section></div>
        }.into_any();
    }
    let allow_always = !harness || a["prefix"].is_string();
    let icons = use_context::<Icons>().unwrap_or_default();
    let command = a["command"].as_str().map(str::to_owned);
    let outcome = if open {
        "Approval needed"
    } else if a["state"] == "expired" {
        "Expired; ask again"
    } else if a["decision"] == "deny" {
        "Not allowed"
    } else if a["kind"] == "email" {
        "Draft saved"
    } else if a["decision"] == "always" {
        "Always allowed for this command prefix"
    } else {
        "Allowed once"
    };
    let label = if harness {
        match a["kind"].as_str() {
            Some("email") => "Email draft",
            Some("craft") => "Craft Anything permission",
            _ => "Coding harness permission",
        }
    } else {
        "Approval"
    };
    view! {
        <section class="bs-root k-stack" aria-label=label>
            <div class="k-card"><div class="approval-card-body k-stack">
                <div class="k-row">
                    <div class="k-row__main">
                        <span class="k-row__leading" inner_html=icons.shield></span>
                        <span class="k-row__text"><span class="k-row__title"><h3 class="k-section__title">{label}</h3></span></span>
                    </div>
                    <span class="k-row__trailing"><span class="k-badge" class:is-warn=open role="status">{outcome}</span></span>
                </div>
                <p class="message-plain">{title}</p>
                {command.map(|command| view! {
                    <pre class="k-code" tabindex="0" role="region" aria-label="Shell command">{command}</pre>
                })}
                <details class="k-collapsible conversation-disclosure">
                    <summary class="k-collapsible__trigger">"Request details"<span class="appearance-icon k-collapsible__chev" inner_html=icons.chevron></span></summary>
                    <div class="k-collapsible__content">
                        <pre class="k-code" tabindex="0" role="region" aria-label="Request arguments">{details}</pre>
                    </div>
                </details>
                <div class="k-row-actions" hidden=!open>
                    {[("deny", "Don't allow"), ("always", if harness { "Always allow for this agent + command prefix" } else { "Always allow" }),
                        ("allow", if a["kind"] == "email" { "Approve draft" } else { "Allow once" })].into_iter()
                        .filter(|(decision, _)| *decision != "always" || allow_always).map(|(decision, title)| {
                            let path = path.clone();
                            view! { <button type="button" class=if decision == "allow" { "k-btn k-btn--sm k-btn--primary" } else { "k-btn k-btn--sm k-btn--secondary" }
                                disabled=move || busy.get() || chat.config.with_value(|c| c.archived)
                                on:click=move |_| mutate(path.clone(), json!({"revision": revision, "decision": decision}), busy, error)>{title}</button> }
                        }).collect_view()}
                </div>
                <p class="message-error" role="alert" hidden=move || error.with(String::is_empty)>{error}</p>
            </div></div>
        </section>
    }.into_any()
}

pub(super) fn edit_dialog(
    agent: Agent,
    thread: Option<Thread>,
    dialog: NodeRef<leptos::html::Dialog>,
) -> impl IntoView {
    let form = NodeRef::<leptos::html::Form>::new();
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let thread_edit = thread.is_some();
    let title = if thread_edit {
        "Conversation settings"
    } else {
        "Edit profile"
    };
    let name = thread
        .as_ref()
        .map_or_else(|| agent.name.clone(), |value| value.name.clone());
    let endpoint = thread.as_ref().map_or_else(
        || format!("/api/agents/{}/edit", agent.id),
        |thread| format!("/api/threads/{}/edit", thread.id),
    );
    let recovery = format!("minarbot:leptos-action:{endpoint}");
    let pending = RwSignal::new(saved(&recovery));
    let recovery = StoredValue::new(recovery);
    Effect::new(move |_| {
        busy.track();
        recovery.with_value(|key| pending.set(saved(key)));
    });
    let model = agent.model.unwrap_or_default();
    let role = agent.role.unwrap_or_default();
    let instructions = agent.instructions;
    let payload = move |form: &HtmlFormElement| {
        let mut body = json!({"name": field(form, "name").trim()});
        if let Some(thread) = &thread {
            body["revision"] = json!(thread.revision);
            body["archived"] = json!(thread.archived != 0);
        } else {
            body["revision"] = json!(agent.revision);
            body["role"] = json!(field(form, "role").trim());
            body["model"] = json!(field(form, "model").trim());
            body["instructions"] = json!(field(form, "instructions").trim());
        }
        body
    };
    let submit = move |event: web_sys::SubmitEvent| {
        event.prevent_default();
        let body = if let Some(raw) = pending.get_untracked() {
            let Ok(body) = serde_json::from_str::<Value>(&raw) else {
                error.set("Saved profile change could not be read.".into());
                return;
            };
            body
        } else {
            let Some(form) = form.get_untracked() else {
                return;
            };
            payload(&form)
        };
        mutate_then(endpoint.clone(), body, busy, error, move || {
            let _ = window().location().reload();
        });
    };
    view! {
        <dialog class="conversation-dialog bs-root" node_ref=dialog aria-label=title
            on:cancel=move |event: web_sys::Event| { if busy.get_untracked() { event.prevent_default(); } }>
            <h2 class="k-dialog__title">{title}</h2>
            <form class="k-stack" node_ref=form on:submit=submit>
                <fieldset class="component-fields k-stack" disabled=move || busy.get() || pending.with(Option::is_some)>
                    <label class="k-field"><span class="k-field__label">"Name"</span><input class="k-input" name="name" value=name maxlength="80" required autofocus /></label>
                    {(!thread_edit).then(|| view! {
                        <label class="k-field"><span class="k-field__label">"Role"</span><input class="k-input" name="role" value=role maxlength="80" /></label>
                        <label class="k-field"><span class="k-field__label">"Model"</span><input class="k-input" name="model" value=model maxlength="160" required /></label>
                        <label class="k-field"><span class="k-field__label">"Instructions"</span><textarea class="k-input k-textarea" name="instructions" maxlength="8000" required>{instructions}</textarea></label>
                    })}
                </fieldset>
                <p class="message-error" role="alert" hidden=move || error.with(String::is_empty)>{error}</p>
                <div class="k-row-actions">
                    <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=busy
                        on:click=move |_| { if let Some(dialog) = dialog.get_untracked() { dialog.close(); } }>"Cancel"</button>
                    <button type="submit" class="k-btn k-btn--sm k-btn--primary" disabled=busy>
                        {move || if pending.with(Option::is_some) { "Retry saved change" } else { "Save" }}
                    </button>
                </div>
            </form>
        </dialog>
    }
}

fn resume_dialog(
    chat: Chat,
    row: RwSignal<Turn>,
    scope: StoredValue<String>,
    dialog: NodeRef<leptos::html::Dialog>,
    busy: RwSignal<bool>,
    error: RwSignal<String>,
) -> impl IntoView {
    let checked = RwSignal::new(false);
    let recipient = RwSignal::new(String::new());
    let members = chat.config.with_value(|config| config.members.clone());
    let has_members = !members.is_empty();
    let disabled =
        move || !checked.get() || busy.get() || chat.config.with_value(|config| config.archived);
    let submit = move |kind: &str| {
        if disabled() {
            return;
        }
        let mut command = json!({"revision": row.with_untracked(|turn| turn.revision), "acknowledgeUncertain": true});
        if kind == "reassign" {
            let id = recipient.get_untracked();
            if !protocol::id(&id) {
                return;
            }
            command["recipient"] = json!(id);
        }
        mutate_then(
            format!("{}/{kind}", scope.get_value()),
            command,
            busy,
            error,
            move || {
                if let Some(dialog) = dialog.get_untracked() {
                    dialog.close();
                }
                chat.refresh();
            },
        );
    };
    view! {
        <dialog node_ref=dialog aria-label="Resume or reassign" class="k-dialog k-dialog--md workspace-dialog"
            on:close=move |_| { checked.set(false); recipient.set(String::new()); }
            on:cancel=move |event: web_sys::Event| { if busy.get_untracked() { event.prevent_default(); } }>
            <div class="k-dialog__head"><h2 class="k-dialog__title">"Resume or reassign"</h2></div>
            <div class="k-dialog__body k-stack">
                <p class="k-field__hint">"Check previous results and effects in the provider. Resuming may resend steps that are not confirmed yet."</p>
                <label class="k-check"><input type="checkbox" prop:checked=checked disabled=busy on:change=move |event| checked.set(event_target_checked(&event)) />
                    "I have checked previous results and effects"</label>
                <div><button type="button" class="k-btn k-btn--sm k-btn--ghost" disabled=disabled on:click=move |_| submit("resume")>"Resume task"</button></div>
                <div class="k-stack" hidden=move || !has_members || row.with(|turn| turn.approvals.iter().any(|approval| approval["state"] == "ready"))>
                    <label class="k-field"><span class="k-field__label">"New owner"</span>
                        <select class="k-input" prop:value=recipient disabled=busy on:change=move |event| recipient.set(event_target_value(&event))>
                            <option value="">"Select agent"</option>
                            {members.into_iter().map(|member| view! { <option value=member.id>{member.name}</option> }).collect_view()}
                        </select>
                    </label>
                    <div><button type="button" class="k-btn k-btn--sm k-btn--ghost" disabled=move || disabled() || recipient.with(String::is_empty)
                        on:click=move |_| submit("reassign")>"Reassign task"</button></div>
                </div>
                <p class="k-field__error" role="alert" hidden=move || error.with(String::is_empty)>{error}</p>
            </div>
            <div class="k-dialog__foot"><form method="dialog"><button type="submit" class="k-btn k-btn--secondary" disabled=busy>"Cancel"</button></form></div>
        </dialog>
    }
}
