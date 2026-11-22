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
}

fn files(chat: Chat, selected: web_sys::FileList, uploading: RwSignal<bool>) {
    if chat.locked() || uploading.get_untracked() {
        return;
    }
    let length = selected.length();
    if length as usize + chat.files.with_untracked(Vec::len) > MAX_FILES {
        chat.error.set("Attach up to four files.".into());
        return;
    }
    if length == 0 {
        return;
    }
    uploading.set(true);
    let path = format!("{}/files", chat.base());
    leptos::task::spawn_local(async move {
        for index in 0..length {
            let Some(file) = selected.get(index) else {
                continue;
            };
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

fn dialog_open(dialog: NodeRef<leptos::html::Dialog>) {
    if let Some(dialog) = dialog.get_untracked() {
        let _ = dialog.show_modal();
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
    let label = chat.config.with_value(|c| {
        if c.channel {
            format!("Message #{}", c.name)
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
        if uploading.get_untracked() {
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
    view! {
        <footer class="conversation-composer">
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
                <details class="conversation-queue" hidden=move || rows.with(|rows| !rows.iter().any(|turn| turn.status == "queued"))>
                    <summary>"Queued messages"</summary>
                    {move || rows.with(|rows| rows.iter().filter(|turn| turn.status == "queued").map(|turn| {
                        let id = turn.id.clone();
                        view! { <button type="button" class="panel-row" on:click=move |_| chat.inspect(id.clone())>{turn.text.clone()}</button> }
                    }).collect_view())}
                </details>
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
                <form class="composer" aria-label="Composer" on:submit=move |event| { event.prevent_default(); submit(false); }
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
                    <textarea node_ref=input class="composer__input" id="message" rows="2" maxlength=MAX_MESSAGE
                        aria-label=label placeholder="Write a message" readonly=move || chat.locked()
                        prop:value=move || chat.pending.with(|pending| pending.as_ref().map(|pending| pending.command.text.clone())).unwrap_or_else(|| chat.draft.get())
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
                    {move || mentions.get().into_iter().map(|name| {
                        let label = name.clone();
                        view! { <button type="button" class="k-btn k-btn--sm k-btn--secondary" on:click=move |_| {
                            let mut draft = chat.draft.get_untracked();
                            if let Some(at) = draft.rfind('@') { draft.truncate(at + 1); draft.push_str(&name); draft.push(' '); chat.draft(draft); }
                            if let Some(input) = input.get_untracked() { let _ = input.focus(); }
                        }>{format!("@{label}")}</button> }
                    }).collect_view()}
                    <div class="composer__bar k-row-actions">
                        <input node_ref=file_input type="file" multiple hidden on:change=move |_| {
                            if let Some(element) = file_input.get_untracked() {
                                if let Some(list) = element.files() { files(chat, list, uploading); }
                                element.set_value("");
                            }
                        } />
                        <button type="button" class="k-btn k-btn--sm k-btn--ghost" aria-label="Attach conversation file"
                            disabled=move || chat.locked() || uploading.get() || attachments.with(|files| files.len() >= MAX_FILES)
                            on:click=move |_| { if let Some(element) = file_input.get_untracked() { element.click(); } }>"Attach"</button>
                        <select class="k-input" aria-label="Addressed member" hidden=move || !chat.config.with_value(|c| c.channel) || chat.active.with(Vec::is_empty)
                            disabled=move || chat.locked() on:change=move |event| chat.addressed.set(event_target_value(&event))>
                            <option value="">"Everyone"</option>
                            {move || chat.active.get().into_iter().map(|run| {
                                let name = chat.config.with_value(|c| c.members.iter().find(|m| m.id == run.agent).map(|m| m.name.clone())).unwrap_or(run.agent);
                                view! { <option value=run.id>{name}</option> }
                            }).collect_view()}
                        </select>
                        <button type="button" class="k-btn k-btn--sm k-btn--ghost"
                            hidden=move || !chat.target().is_some_and(|run| run.capabilities.steer) || chat.context.with(Option::is_some)
                            disabled=move || chat.locked() || uploading.get() on:click=move |_| submit(true)>"Steer"</button>
                        <button class="k-btn k-btn--sm k-btn--primary" type="submit"
                            disabled=move || chat.busy.get() || chat.blocked.get() || uploading.get() || chat.config.with_value(|c| c.archived)
                                || chat.pending.with(Option::is_none) && (!chat.can_send() || chat.draft.with(|draft| draft.trim().is_empty()) && attachments.with(Vec::is_empty))>
                            {move || if chat.busy.get() { "Sending…" } else if chat.pending.with(Option::is_some) { "Retry send" } else if chat.target().is_some() { "Queue message" } else { "Send message" }}
                        </button>
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

pub fn mutate(path: String, mut body: Value, busy: RwSignal<bool>, error: RwSignal<String>) {
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
                let _ = remove(&key);
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

pub fn turn_controls(chat: Chat, row: RwSignal<Turn>) -> impl IntoView {
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let confirm = NodeRef::<leptos::html::Dialog>::new();
    let action = RwSignal::new("");
    let scope = StoredValue::new(format!(
        "{}/turns/{}",
        chat.base(),
        row.with_untracked(|turn| turn.id.clone())
    ));
    let questions = Memo::new(move |_| row.with(|turn| turn.questions.clone()));
    let approvals = Memo::new(move |_| row.with(|turn| turn.approvals.clone()));
    let harness = Memo::new(move |_| row.with(|turn| turn.harness_approvals.clone()));
    view! {
        <div class="turn-controls" role="group" aria-label="Message actions">
            <button type="button" class="k-btn k-btn--sm k-btn--ghost" disabled=busy hidden=move || chat.config.with_value(|c| c.archived)
                || row.with(|t| !t.active() || t.needs_you() || t.paused != 0)
                on:click=move |_| { action.set("cancel"); dialog_open(confirm); }>"Stop"</button>
            <button type="button" class="k-btn k-btn--sm k-btn--ghost" disabled=busy hidden=move || chat.config.with_value(|c| c.archived)
                || row.with(|t| !t.active() || t.needs_you() || t.paused != 0)
                on:click=move |_| mutate(format!("{}/pause", scope.get_value()), json!({"revision": row.with_untracked(|t| t.revision)}), busy, error)>"Pause"</button>
            <button type="button" class="k-btn k-btn--sm k-btn--ghost" disabled=busy hidden=move || chat.config.with_value(|c| c.archived)
                || row.with(|t| t.needs_you() || t.paused == 0 && !matches!(t.status.as_str(), "failed" | "interrupted"))
                on:click=move |_| { action.set("resume"); dialog_open(confirm); }>"Resume task"</button>
            <p role="alert" class="message-error" hidden=move || error.with(String::is_empty)>{error}</p>
            <dialog node_ref=confirm class="conversation-dialog" aria-label=move || if action.get() == "resume" { "Resume task?" } else { "Stop this task?" }>
                <form method="dialog" class="k-stack" on:submit=move |event: leptos::ev::SubmitEvent| {
                    let value = event.submitter().and_then(|element| element.get_attribute("value"));
                    if value.as_deref() != Some("confirm") { return }
                    let action = action.get_untracked();
                    let body = if action == "resume" { json!({"revision": row.with_untracked(|t| t.revision), "acknowledgeUncertain": true}) } else { json!({}) };
                    mutate(format!("{}/{action}", scope.get_value()), body, busy, error);
                }>
                    <p>{move || if action.get() == "resume" { "Check previous results. Resuming may resend unconfirmed steps." } else { "Steps already sent may have taken effect." }}</p>
                    <div class="k-row-actions"><button class="k-btn k-btn--sm k-btn--secondary" formnovalidate>"Cancel"</button>
                        <button class="k-btn k-btn--sm k-btn--primary" value="confirm">{move || if action.get() == "resume" { "Resume task" } else { "Stop task" }}</button></div>
                </form>
            </dialog>
        </div>
        {move || questions.get().into_iter().map(|q| question(chat, row, scope.get_value(), q)).collect_view()}
        {move || approvals.get().into_iter().map(|a| approval(chat, scope.get_value(), a, false)).collect_view()}
        {move || harness.get().into_iter().map(|a| approval(chat, scope.get_value(), a, true)).collect_view()}
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
    let allow_always = !form && (!harness || a["prefix"].is_string());
    view! {
        <section class="k-card conversation-card" aria-label="Approval">
            <h3>{title}</h3>
            <p role="status">{if open { "Approval needed" } else { a["state"].as_str().unwrap_or_default() }.to_owned()}</p>
            {form.then(|| panels::form_reply(chat, a["input"].clone(), path.clone(), revision, open, form_result, busy, error))}
            {(!form).then(|| view! {
                <details><summary>"Request details"</summary><pre>{details}</pre></details>
                <div class="k-row-actions" hidden=!open>
                    {[("deny", "Don't allow"), ("allow", "Allow once"), ("always", "Always allow")].into_iter()
                        .filter(|(decision, _)| *decision != "always" || allow_always).map(|(decision, title)| {
                            let path = path.clone();
                            view! { <button type="button" class="k-btn k-btn--sm k-btn--secondary"
                                disabled=move || busy.get() || chat.config.with_value(|c| c.archived)
                                on:click=move |_| mutate(path.clone(), json!({"revision": revision, "decision": decision}), busy, error)>{title}</button> }
                        }).collect_view()}
                </div>
            })}
            <p class="message-error" role="alert" hidden=move || error.with(String::is_empty)>{error}</p>
        </section>
    }
}
