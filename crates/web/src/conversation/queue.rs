use super::*;
use serde_json::json;

pub(super) fn editor(chat: Chat, row: RwSignal<Turn>) -> impl IntoView {
    let endpoint = StoredValue::new(format!(
        "{}/turns/{}/edit",
        chat.base(),
        row.with_untracked(|row| row.id.clone())
    ));
    let key = StoredValue::new(format!("minarbot:queue-edit:{}", endpoint.get_value()));
    let raw_edit = saved(&key.get_value());
    let saved_edit = raw_edit
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .filter(|value| {
            ["id", "editor"]
                .iter()
                .all(|key| value[key].as_str().is_some_and(protocol::id))
                && value["action"] == "save"
                && value["revision"].as_u64().is_some_and(|r| r > 0)
                && value["text"]
                    .as_str()
                    .is_some_and(|text| text.encode_utf16().count() <= MAX_MESSAGE)
                && value["files"].as_array().is_some_and(|files| {
                    files.len() <= MAX_FILES
                        && files
                            .iter()
                            .all(|file| file.as_str().is_some_and(protocol::id))
                })
        });
    let lease = RwSignal::new(saved_edit.as_ref().map(|value| {
        (
            value["editor"].as_str().unwrap_or_default().to_owned(),
            value["revision"].as_u64().unwrap_or_default(),
        )
    }));
    let text = RwSignal::new(
        saved_edit
            .as_ref()
            .and_then(|value| value["text"].as_str())
            .unwrap_or_default()
            .to_owned(),
    );
    let files = RwSignal::new(
        saved_edit
            .as_ref()
            .and_then(|value| serde_json::from_value::<Vec<String>>(value["files"].clone()).ok())
            .unwrap_or_default(),
    );
    let pending = RwSignal::new(saved_edit.and(raw_edit));
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let input = NodeRef::<leptos::html::Textarea>::new();
    let archived = chat.config.with_value(|config| config.archived);
    let pencil = use_context::<Icons>().unwrap_or_default().edit;
    let change = move |value: String, selected: Vec<String>| {
        let Some((editor, revision)) = lease.get_untracked() else {
            return;
        };
        let Some(id) = uuid() else { return };
        if !save(&key.get_value(), &json!({"id": id, "editor": editor, "revision": revision, "action": "save", "text": value, "files": selected}).to_string()) {
            error.set("Edit draft could not be saved.".into());
        }
        text.set(value);
        files.set(selected);
    };
    let act = move |action: &'static str| {
        if busy.get_untracked() || archived {
            return;
        }
        if action == "save"
            && pending.with_untracked(Option::is_none)
            && text.with_untracked(|value| value.trim().is_empty())
            && files.with_untracked(Vec::is_empty)
        {
            return;
        }
        let new = if action == "hold" {
            uuid().map(|editor| (editor, row.with_untracked(|row| row.revision)))
        } else {
            lease.get_untracked()
        };
        let Some((editor, revision)) = new else {
            return;
        };
        let Some(id) = uuid() else { return };
        let storage_key = key.get_value();
        let raw = pending
            .get_untracked()
            .or_else(|| (action == "save").then(|| saved(&storage_key)).flatten())
            .unwrap_or_else(|| {
                let mut command =
                    json!({"id": id, "editor": editor, "revision": revision, "action": action});
                if action == "save" {
                    command["text"] = json!(text.get_untracked());
                    command["files"] = json!(files.get_untracked());
                }
                command.to_string()
            });
        let Ok(command) = serde_json::from_str::<Value>(&raw) else {
            error.set("Saved edit could not be read.".into());
            return;
        };
        let held = command["action"] == "hold";
        if !held && command["action"] == "save" && !save(&storage_key, &raw) {
            error.set("Edit could not be saved for retry.".into());
            return;
        }
        let owner = command["editor"].as_str().unwrap_or_default().to_owned();
        let path = endpoint.get_value();
        pending.set(Some(raw.clone()));
        busy.set(true);
        error.set(String::new());
        leptos::task::spawn_local(async move {
            let result = post::<Value>(&path, &raw).await;
            if busy.is_disposed() {
                return;
            }
            match result {
                Ok(receipt) if held => {
                    if let Some(revision) = receipt["revision"].as_u64() {
                        lease.set(Some((owner, revision)));
                        text.set(receipt["text"].as_str().unwrap_or_default().into());
                        files.set(
                            serde_json::from_value(receipt["files"].clone()).unwrap_or_default(),
                        );
                        request_animation_frame(move || {
                            if let Some(input) = input.get_untracked() {
                                let _ = input.focus();
                            }
                        });
                    }
                    pending.set(None);
                }
                Ok(_) => {
                    let _ = remove(&storage_key);
                    lease.set(None);
                    pending.set(None);
                    chat.refresh();
                }
                Err(reason) => {
                    if reason.definitive {
                        pending.set(None);
                    }
                    error.set(reason.message);
                }
            }
            busy.set(false);
        });
    };
    let renew = move || {
        if archived || busy.get_untracked() || pending.with_untracked(Option::is_some) {
            return;
        }
        let Some((editor, revision)) = lease.get_untracked() else {
            return;
        };
        let Some(id) = uuid() else { return };
        let path = endpoint.get_value();
        let raw = json!({"id": id, "editor": editor, "revision": revision, "action": "renew"})
            .to_string();
        leptos::task::spawn_local(async move {
            if let Err(reason) = post::<Value>(&path, &raw).await {
                error.try_set(reason.message);
            }
        });
    };
    let release = move || {
        let Some((editor, revision)) = lease.try_get_untracked().flatten() else {
            return;
        };
        let Some(id) = uuid() else { return };
        let raw = saved(&key.get_value()).unwrap_or_else(|| {
            json!({"id": id, "editor": editor, "revision": revision, "action": "cancel"})
                .to_string()
        });
        post_keepalive(&endpoint.get_value(), &raw);
        lease.try_set(None);
    };
    Effect::new(move |_| {
        if !lease.with(Option::is_some) {
            return;
        }
        let timer = set_interval_with_handle(renew, Duration::from_secs(10)).ok();
        let hide = window_event_listener(leptos::ev::pagehide, move |_| release());
        let reconnect = window_event_listener(leptos::ev::online, move |_| release());
        on_cleanup(move || {
            if let Some(timer) = timer {
                timer.clear();
            }
            hide.remove();
            reconnect.remove();
            release();
        });
    });
    if let Some(saved) = pending.get_untracked() {
        let retry = StoredValue::new(saved);
        if let Ok(timer) = set_interval_with_handle(
            move || {
                if pending.with_untracked(|pending| pending.as_ref() == Some(&retry.get_value())) {
                    act("save");
                }
            },
            Duration::from_secs(2),
        ) {
            on_cleanup(move || timer.clear());
        }
    }
    view! {
        <div class="queue-editor" hidden=move || archived || row.with(|turn| turn.status != "queued") && lease.with(Option::is_none) && pending.with(Option::is_none)>
            <button type="button" class="k-icon-btn" aria-label="Edit" title="Edit" disabled=busy
                hidden=move || row.with(|turn| turn.needs_you() || turn.paused != 0 || turn.editing || turn.revision == 0) || lease.with(Option::is_some) || pending.with(Option::is_some)
                on:click=move |_| act("hold")><span class="appearance-icon" inner_html=pencil></span></button>
            <form class="queue-editor__form k-stack" aria-label="Edit queued message" hidden=move || lease.with(Option::is_none)
                on:submit=move |event| { event.prevent_default(); act("save"); }
                on:keydown=move |event| { if event.key() == "Escape" && !busy.get_untracked() { event.prevent_default(); event.stop_propagation(); act("cancel"); } }>
                <textarea node_ref=input class="k-input" aria-label="Replacement message" maxlength=MAX_MESSAGE prop:value=text
                    readonly=move || busy.get() || pending.with(Option::is_some)
                    on:input=move |event| change(event_target_value(&event), files.get_untracked())
                    on:blur=move |event| {
                        let within = input.get_untracked().and_then(|input| input.form())
                            .zip(event.related_target().and_then(|target| target.dyn_into::<web_sys::Node>().ok()))
                            .is_some_and(|(form, target)| form.contains(Some(&target)));
                        if !within { act("save"); }
                    }
                    on:keydown=move |event| { if event.key() == "Enter" && !event.shift_key() && !event.is_composing() && event.key_code() != 229 { event.prevent_default(); act("save"); } } />
                {move || files.get().into_iter().map(|id| {
                    let name = row.with(|turn| turn.files.iter().find(|file| file.id == id).map(|file| file.name.clone())).unwrap_or_else(|| "Attachment".into());
                    view! { <button type="button" class="k-btn k-btn--sm k-btn--ghost" disabled=busy on:click=move |_| {
                        let mut selected = files.get_untracked(); selected.retain(|file| file != &id); change(text.get_untracked(), selected);
                    }>{format!("Remove {name}")}</button> }
                }).collect_view()}
                <div class="k-row-actions">
                    <button class="k-btn k-btn--sm k-btn--primary" type="submit" disabled=move || busy.get() || pending.with(Option::is_some)
                        || text.with(|text| text.trim().is_empty()) && files.with(Vec::is_empty)>"Save message"</button>
                    <button class="k-btn k-btn--sm k-btn--secondary" type="button" disabled=move || busy.get() || pending.with(Option::is_some)
                        on:click=move |_| act("cancel")>"Cancel edit"</button>
                </div>
            </form>
            <button class="k-btn k-btn--sm k-btn--secondary" type="button" hidden=move || pending.with(Option::is_none) || busy.get()
                on:click=move |_| act(if lease.with_untracked(Option::is_some) { "save" } else { "hold" })>"Retry change"</button>
            {move || (!error.with(String::is_empty)).then(|| view! { <p class="message-error" role="alert">{error}</p> })}
        </div>
    }
}

pub(super) fn queued_runs(chat: Chat, rows: Memo<Vec<Turn>>) -> impl IntoView {
    let expanded = RwSignal::new(false);
    let order = NodeRef::<leptos::html::Dialog>::new();
    let queued = Memo::new(move |_| {
        rows.with(|rows| {
            rows.iter()
                .filter(|turn| chat.queued(turn))
                .map(|turn| turn.id.clone())
                .collect::<Vec<_>>()
        })
    });
    view! {
        <Show when=move || !queued.with(Vec::is_empty)>
        <section class="queued-runs" aria-label="Queued messages">
            <div class="queued-runs__heading">"Queued " <span>{move || queued.with(Vec::len)}</span>
                <button type="button" class="k-btn k-btn--sm k-btn--ghost" on:click=move |_| controls::dialog_open(order)>"Order"</button>
            </div>
            <ol><For each=move || queued.with(|queued| queued.iter().take(if expanded.get() { usize::MAX } else { 3 }).cloned().collect::<Vec<_>>())
                key=String::clone children=move |id| {
                    let key = id.clone();
                    let row = RwSignal::new(rows.with_untracked(|rows| rows.iter().find(|row| row.id == id).cloned()).unwrap_or_default());
                    Effect::new(move |_| {
                        if let Some(next) = rows.with(|rows| rows.iter().find(|row| row.id == key).cloned()) {
                            if row.with_untracked(|row| *row != next) { row.set(next); }
                        }
                    });
                    queued_run(chat, row, id)
                } /></ol>
            <button type="button" class="k-btn k-btn--sm k-btn--ghost" hidden=move || queued.with(Vec::len) <= 3 on:click=move |_| expanded.update(|value| *value = !*value)>
                {move || if expanded.get() { "Show less".into() } else { format!("Show {} more", queued.with(Vec::len).saturating_sub(3)) }}
            </button>
        </section>
        </Show>
        {order_dialog(chat, order, queued)}
    }
}

fn queued_run(chat: Chat, row: RwSignal<Turn>, turn: String) -> impl IntoView {
    let archived = chat.config.with_value(|config| config.archived);
    let path = StoredValue::new(format!("{}/turns/{turn}", chat.base()));
    let key = StoredValue::new(format!(
        "minarbot:queue-dispatch:{}/turns:{turn}",
        chat.base()
    ));
    let pending = RwSignal::new(saved(&key.get_value()));
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let icons = use_context::<Icons>().unwrap_or_default();
    let dispatch = move || {
        if busy.get_untracked() || archived {
            return;
        }
        let raw = pending.get_untracked().or_else(|| {
            let target = chat.target().filter(|target| target.capabilities.restart)?;
            Some(json!({"id":uuid()?, "revision":row.with_untracked(|turn| turn.revision), "mode":"restart", "targetTurn":target.id}).to_string())
        });
        let Some(raw) = raw else {
            return;
        };
        if !serde_json::from_str::<Value>(&raw)
            .ok()
            .is_some_and(|body| {
                ["id", "targetTurn"]
                    .iter()
                    .all(|key| body[key].as_str().is_some_and(protocol::id))
                    && body["revision"]
                        .as_u64()
                        .is_some_and(|revision| (1..=9_007_199_254_740_991).contains(&revision))
                    && matches!(body["mode"].as_str(), Some("restart" | "steer"))
            })
        {
            error.set("Queue recovery could not be read. Check browser storage.".into());
            return;
        }
        let key = key.get_value();
        if !save(&key, &raw) {
            error.set("Queue change could not be saved.".into());
            return;
        }
        pending.set(Some(raw.clone()));
        busy.set(true);
        error.set(String::new());
        let path = format!("{}/dispatch", path.get_value());
        leptos::task::spawn_local(async move {
            let result = post::<Value>(&path, &raw).await;
            if busy.is_disposed() {
                return;
            }
            busy.set(false);
            match result {
                Ok(_) if remove(&key) => {
                    pending.set(None);
                    chat.refresh();
                }
                Ok(_) => error.set("Queue changed. Recovery data could not be cleared.".into()),
                Err(reason) => {
                    if reason.definitive && remove(&key) {
                        pending.set(None);
                        chat.refresh();
                    }
                    error.set(reason.message);
                }
            }
        });
    };
    let text = move || {
        row.with(|turn| {
            if turn.text.is_empty() {
                if turn.files.is_empty() {
                    "Attachment".into()
                } else {
                    turn.files
                        .iter()
                        .map(|file| file.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            } else {
                turn.text.clone()
            }
        })
    };
    let special =
        move || row.with(|turn| matches!(turn.dispatch_mode.as_deref(), Some("steer" | "restart")));
    view! {
        <li class="queued-run" aria-label=move || format!("Queued message: {}", text())>
            <div class="queued-run__line">
                <span class="queued-run__text" title=text>{text}</span>
                <div class="queued-run__actions">
                    <div hidden=move || special() || archived>{editor(chat, row)}</div>
                    <button type="button" class="k-btn k-btn--sm k-btn--ghost" disabled=busy hidden=move || pending.with(Option::is_none) on:click=move |_| dispatch()>"Retry change"</button>
                    <button type="button" class="k-btn k-btn--sm k-btn--ghost" title="Stop current reply and send this message"
                        disabled=busy hidden=move || archived || special() || pending.with(Option::is_some) || row.with(|row| row.editing || row.revision == 0) || chat.target().is_none_or(|run| !run.capabilities.restart)
                        on:click=move |_| dispatch()><span class="appearance-icon" inner_html=icons.send></span>"Send now"</button>
                    <span hidden=move || row.with(|turn| turn.revision == 0)>{cancel_button(chat, turn, false)}</span>
                </div>
            </div>
            {move || row.with(|row| match (row.dispatch_mode.as_deref(), row.dispatch_label.as_deref()) {
                (Some("steer"), _) => Some("Sends next"),
                (Some("restart"), _) => Some("Sending now…"),
                (_, Some("Queued after active turn")) => Some("Queued for next turn"),
                _ => None,
            }).map(|label| view! { <span class="queued-run__status">{label}</span> })}
            <p class="message-error" role="alert" hidden=move || error.with(String::is_empty)>{error}</p>
        </li>
    }
}

#[derive(Clone, Deserialize)]
struct QueuePage {
    turns: Vec<QueueEntry>,
    next_after: Option<u64>,
    snapshot: String,
}

#[derive(Clone, Deserialize)]
struct QueueEntry {
    #[serde(flatten)]
    neighbor: Neighbor,
    text: String,
    paused: u8,
    previous: Option<Neighbor>,
    next: Option<Neighbor>,
}

#[derive(Clone, Deserialize)]
struct Neighbor {
    id: String,
    agent: String,
    revision: u64,
    editing: bool,
}

fn order_dialog(
    chat: Chat,
    dialog: NodeRef<leptos::html::Dialog>,
    queued: Memo<Vec<String>>,
) -> impl IntoView {
    let endpoint = StoredValue::new(format!("{}/queue", chat.base()));
    let key = StoredValue::new(format!("minarbot-queue-order:{}", endpoint.get_value()));
    let pending = RwSignal::new(saved(&key.get_value()));
    let page = RwSignal::new(None::<QueuePage>);
    let cursors = RwSignal::new(Vec::<(u64, String)>::new());
    let busy = RwSignal::new(false);
    let loading = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let notice = RwSignal::new("");
    let open = RwSignal::new(false);
    let discard = NodeRef::<leptos::html::Dialog>::new();
    let load = move || {
        let recovery = pending.get_untracked();
        let empty = queued.with_untracked(Vec::is_empty);
        if (!open.get_untracked() && !(recovery.is_some() && empty))
            || busy.get_untracked()
            || loading.get_untracked()
        {
            return;
        }
        let mut path = endpoint.get_value();
        let cursor = open
            .get_untracked()
            .then(|| cursors.with_untracked(|cursors| cursors.last().cloned()))
            .flatten();
        if let Some((after, snapshot)) = &cursor {
            if !snapshot.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return;
            }
            path.push_str(&format!("?after={after}&snapshot={snapshot}"));
        }
        loading.set(true);
        leptos::task::spawn_local(async move {
            let result = get::<QueuePage>(&path).await;
            if loading.is_disposed() {
                return;
            }
            loading.set(false);
            match result {
                Ok(rows) => {
                    if empty
                        && cursor.is_none()
                        && rows.turns.is_empty()
                        && rows.next_after.is_none()
                        && recovery.is_some()
                        && saved(&key.get_value()) == recovery
                        && remove(&key.get_value())
                    {
                        pending.set(None);
                    }
                    page.set(Some(rows));
                }
                Err(reason) if cursor.is_some() && reason.message.contains("changed") => {
                    notice.set("Queue changed. The list returned to the first page.");
                    page.set(None);
                    cursors.set(Vec::new());
                }
                Err(reason) => error.set(reason.message),
            }
        });
    };
    let submit = move |pair: Option<(Neighbor, Neighbor)>| {
        if busy.get_untracked()
            || loading.get_untracked()
            || chat.config.with_value(|config| config.archived)
        {
            return;
        }
        let raw = pending.get_untracked().or_else(|| {
            let (row, other) = pair?;
            Some(json!({"id":uuid()?,"target":row.id,"other":other.id,"revision":row.revision,"otherRevision":other.revision}).to_string())
        });
        let Some(raw) = raw else {
            return;
        };
        let valid = serde_json::from_str::<Value>(&raw)
            .ok()
            .is_some_and(|body| {
                ["id", "target", "other"]
                    .iter()
                    .all(|key| body[key].as_str().is_some_and(protocol::id))
                    && ["revision", "otherRevision"].iter().all(|key| {
                        body[key]
                            .as_u64()
                            .is_some_and(|revision| (1..=9_007_199_254_740_991).contains(&revision))
                    })
            });
        if !valid {
            error.set("Saved order request is invalid.".into());
            return;
        }
        let key = key.get_value();
        if !save(&key, &raw) {
            error.set("Order has not been saved in the browser. Check site storage.".into());
            return;
        }
        pending.set(Some(raw.clone()));
        busy.set(true);
        error.set(String::new());
        let path = endpoint.get_value();
        leptos::task::spawn_local(async move {
            let result = post::<Value>(&path, &raw).await;
            if busy.is_disposed() {
                return;
            }
            busy.set(false);
            match result {
                Ok(_) if remove(&key) => {
                    pending.set(None);
                    cursors.set(Vec::new());
                    load();
                    chat.refresh();
                }
                Ok(_) => error.set("Queue changed. Recovery data could not be cleared.".into()),
                Err(reason) => error.set(reason.message),
            }
        });
    };
    Effect::new(move |_| {
        open.track();
        cursors.track();
        queued.track();
        load();
    });
    let interval = set_interval_with_handle(load, Duration::from_millis(1500)).ok();
    on_cleanup(move || {
        if let Some(interval) = interval {
            interval.clear();
        }
    });
    let icons = use_context::<Icons>().unwrap_or_default();
    let up = StoredValue::new(icons.up);
    let down = StoredValue::new(icons.next);
    view! {
        <dialog class="k-dialog k-dialog--md workspace-dialog bs-root chat-control-dialog" aria-label="Queue order" node_ref=dialog
            on:toggle=move |_| { open.set(dialog.get_untracked().is_some_and(|dialog| dialog.open())); }
            on:cancel=move |event: web_sys::Event| { if busy.get_untracked() { event.prevent_default(); } }>
            <div class="k-dialog__head"><h2 class="k-dialog__title">"Queue order"</h2>
                <form method="dialog"><button type="submit" class="k-icon-btn" aria-label="Close" disabled=busy><span class="appearance-icon" inner_html=icons.close></span></button></form>
            </div>
            <div class="k-dialog__body">
                <details class="activity-section bs-root k-collapsible conversation-disclosure">
                    <summary role="button" class="k-collapsible__trigger k-collapsible__trigger--section">
                        <span class="k-collapsible__title">"Queue "<span class="k-badge">{move || page.with(|page| page.as_ref().map_or(0, |page| page.turns.len()))}</span></span>
                        <span class="appearance-icon k-collapsible__chev" inner_html=icons.chevron></span>
                    </summary>
                    <div class="k-collapsible__content"><div class="k-stack">
                <div class="k-row-actions"><button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=busy
                    on:click=move |_| { error.set(String::new()); cursors.set(Vec::new()); load(); }>"Reload queue"</button></div>
                <p role="status" hidden=move || notice.get().is_empty()>{notice}</p>
                <p role="alert" class="k-field__error" hidden=move || error.with(String::is_empty)>{error}</p>
                <div class="k-stack" hidden=move || pending.with(Option::is_none)>
                    <p role="status">"Order awaits confirmation"</p>
                    <div class="k-row-actions">
                        <button type="button" class="k-btn k-btn--sm" disabled=busy on:click=move |_| submit(None)>"Retry order"</button>
                        <button type="button" class="k-btn k-btn--sm" disabled=busy on:click=move |_| controls::dialog_open(discard)>"Discard saved request"</button>
                    </div>
                </div>
                <p role="status" hidden=move || !loading.get()>"Loading queue..."</p>
                <p role="status" hidden=move || loading.get() || page.with(|page| page.as_ref().is_none_or(|page| !page.turns.is_empty()))>"No queued messages yet."</p>
                {move || page.get().map(|page| view! { <div class="k-card">{page.turns.into_iter().map(|entry| {
                    let id = entry.neighbor.id.clone();
                    let actions = [("Move up", entry.previous, up.get_value()), ("Move down", entry.next, down.get_value())];
                    view! { <div class="k-row"><div class="k-row__main"><div class="k-row__text">
                        <span class="k-row__title message-plain">{entry.text}</span>
                        <span class="k-row__meta">{if entry.neighbor.editing { "Being edited" } else if entry.paused != 0 { "Paused" } else { "Waiting in queue" }}</span>
                        <div class="k-row-actions queue-order-row-actions">
                            <button type="button" class="k-btn k-btn--sm k-btn--secondary" on:click=move |_| {
                                if let Some(dialog) = dialog.get_untracked() { dialog.close(); }
                                chat.focus_turn(&id);
                            }>"Open message"</button>
                            {actions.into_iter().filter_map(|(label, other, icon)| {
                                let row = entry.neighbor.clone();
                                let other = other.filter(|other| other.agent == row.agent && !row.editing && !other.editing)?;
                                Some(view! { <button type="button" class="k-icon-btn" aria-label=label title=label
                                    disabled=move || busy.get() || loading.get() || pending.with(Option::is_some) || chat.config.with_value(|config| config.archived)
                                    on:click=move |_| submit(Some((row.clone(), other.clone())))><span class="appearance-icon" inner_html=icon></span></button> })
                            }).collect_view()}
                        </div>
                    </div></div></div> }
                }).collect_view()}</div> })}
                <div class="k-pager" hidden=move || cursors.with(Vec::is_empty) && page.with(|page| page.as_ref().is_none_or(|page| page.next_after.is_none()))>
                    <button type="button" class="k-btn k-btn--sm" aria-label="Previous queue page" disabled=move || busy.get() || loading.get() || cursors.with(Vec::is_empty)
                        on:click=move |_| { page.set(None); cursors.update(|cursors| { cursors.pop(); }); }>"Previous"</button>
                    <button type="button" class="k-btn k-btn--sm" aria-label="Next queue page" disabled=move || busy.get() || loading.get() || page.with(|page| page.as_ref().is_none_or(|page| page.next_after.is_none()))
                        on:click=move |_| {
                            if let Some(cursor) = page.with_untracked(|page| page.as_ref().and_then(|page| page.next_after.map(|after| (after, page.snapshot.clone())))) {
                                page.set(None); cursors.update(|cursors| cursors.push(cursor));
                            }
                        }>"Next"</button>
                </div>
                    </div></div>
                </details>
            </div>
        </dialog>
        <dialog class="conversation-dialog" node_ref=discard aria-label="Discard the saved order request?">
            <form method="dialog" class="k-stack">
                <p>"Changes already accepted by the server still apply."</p>
                <div class="k-row-actions">
                    <button type="submit" class="k-btn k-btn--secondary">"Cancel"</button>
                    <button type="submit" class="k-btn k-btn--danger" on:click=move |_| {
                        if remove(&key.get_value()) { pending.set(None); error.set(String::new()); load(); }
                        else { error.set("Browser storage is unavailable.".into()); }
                    }>"Discard request"</button>
                </div>
            </form>
        </dialog>
    }
}

pub(super) fn cancel_button(chat: Chat, turn: String, primary: bool) -> impl IntoView {
    let scope = format!("{}/turns", chat.base());
    let key = StoredValue::new(format!("minarbot:cancel:{scope}:{turn}"));
    let path = StoredValue::new(format!("{scope}/{turn}/cancel"));
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let label = move || {
        if busy.get() {
            "Stopping…"
        } else if error.with(String::is_empty) {
            if primary {
                "Stop"
            } else {
                "Remove"
            }
        } else if primary {
            "Retry Stop"
        } else {
            "Retry Remove"
        }
    };
    let icon = use_context::<Icons>().map(|icons| if primary { icons.stop } else { icons.close });
    view! {
        <span class=if primary { "composer-stop" } else { "queue-remove" } data-turn=turn
            hidden=move || chat.config.with_value(|config| config.archived)>
            <button type="button" class=if primary { "composer__cta composer__cta--stop" } else { "k-icon-btn" }
                aria-label=label title=label disabled=busy on:click=move |_| {
                    if busy.get_untracked() { return; }
                    let key = key.get_value();
                    let Some(id) = saved(&key).or_else(uuid).filter(|id| protocol::id(id)) else {
                        error.set("Stop recovery could not be read. Check browser storage.".into()); return;
                    };
                    if !save(&key, &id) { error.set("Stop request could not be saved.".into()); return; }
                    busy.set(true); error.set(String::new());
                    let path = path.get_value();
                    leptos::task::spawn_local(async move {
                        let result = post::<Value>(&path, &json!({"id":id}).to_string()).await;
                        if busy.is_disposed() { return; }
                        busy.set(false);
                        match result {
                            Ok(_) if remove(&key) => {
                                if primary { history::focus_element("#message"); }
                                chat.refresh();
                            }
                            Ok(_) => error.set("Stop confirmed. Recovery data could not be cleared.".into()),
                            Err(reason) => error.set(reason.message),
                        }
                    });
                }><span class="appearance-icon" inner_html=icon.unwrap_or_default()></span></button>
            <span class="message-error" role="alert" hidden=move || error.with(String::is_empty)>{error}</span>
        </span>
    }
}
