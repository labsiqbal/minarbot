use super::*;
use serde_json::json;

pub(super) fn editor(chat: Chat, row: RwSignal<Turn>) -> impl IntoView {
    let endpoint = StoredValue::new(format!(
        "{}/turns/{}/edit",
        chat.base(),
        row.with_untracked(|row| row.id.clone())
    ));
    let key = StoredValue::new(format!("minarbot:queue-edit:{}", endpoint.get_value()));
    let saved_edit = saved(&key.get_value())
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
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
    let pending = RwSignal::new(saved_edit.map(|value| value.to_string()));
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let input = NodeRef::<leptos::html::Textarea>::new();
    let archived = chat.config.with_value(|config| config.archived);
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
    if let Ok(timer) = set_interval_with_handle(renew, Duration::from_secs(10)) {
        on_cleanup(move || timer.clear());
    }
    view! {
        <div class="queue-editor" hidden=move || archived || row.with(|turn| turn.status != "queued") && lease.with(Option::is_none) && pending.with(Option::is_none)>
            <button type="button" class="k-btn k-btn--sm k-btn--ghost" disabled=busy
                hidden=move || row.with(|turn| turn.needs_you() || turn.paused != 0 || turn.editing) || lease.with(Option::is_some) || pending.with(Option::is_some)
                on:click=move |_| act("hold")>"Edit queued message"</button>
            <form class="queue-editor__form k-stack" aria-label="Edit queued message" hidden=move || lease.with(Option::is_none)
                on:submit=move |event| { event.prevent_default(); act("save"); }>
                <textarea node_ref=input class="k-input" aria-label="Replacement message" maxlength=MAX_MESSAGE prop:value=text
                    readonly=move || busy.get() || pending.with(Option::is_some)
                    on:input=move |event| change(event_target_value(&event), files.get_untracked()) />
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
            <p class="message-error" role="alert" hidden=move || error.with(String::is_empty)>{error}</p>
        </div>
    }
}
