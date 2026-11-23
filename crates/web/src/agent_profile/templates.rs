use super::*;
use crate::connection_ui as ui;
use base64::{engine::general_purpose::STANDARD, Engine};
use journal::bounded;
use leptos::leptos_dom::helpers::set_timeout;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, BlobPropertyBag, HtmlAnchorElement, Url};

const IMPORT_STORAGE: &str = "minarbot:agent-template-import";
const LIMIT: usize = 750_000;

fn valid_avatar(data: &str) -> bool {
    if data.len() > 699_052 {
        return false;
    }
    let Ok(bytes) = STANDARD.decode(data) else {
        return false;
    };
    if bytes.len() > 512 * 1024
        || !bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.get(12..24) != Some(b"IHDR\0\0\x01\0\0\0\x01\0")
    {
        return false;
    }
    let mut chunks = &bytes[8..];
    while let Some(length) = chunks
        .get(..4)
        .and_then(|length| length.try_into().ok())
        .map(u32::from_be_bytes)
    {
        let length = length as usize;
        let Some(rest) = chunks.get(length.saturating_add(12)..) else {
            return false;
        };
        match &chunks[4..8] {
            b"IEND" => return length == 0 && rest.is_empty(),
            b"acTL" | b"fcTL" | b"fdAT" => return false,
            _ => chunks = rest,
        }
    }
    false
}

fn normalize(mut value: Value) -> Result<Value, &'static str> {
    if value["format"] != "minarbot-agent"
        || value["version"] != 1
        || !fields(
            &value,
            &[
                "format",
                "version",
                "name",
                "role",
                "model",
                "backupModel",
                "instructions",
                "threadLimit",
                "toolBudget",
                "avatar",
            ],
        )
    {
        return Err("Unsupported template format or version.");
    }
    for (key, limit, message) in [
        ("name", 80, "Field name is invalid."),
        ("model", 160, "Field model is invalid."),
        ("instructions", 8000, "Field instructions is invalid."),
    ] {
        if !bounded(&value[key], limit, true) {
            return Err(message);
        }
        value[key] = json!(value[key].as_str().unwrap_or_default().trim());
    }
    if value.get("role").is_some_and(|role| {
        !role.as_str().is_some_and(|role| {
            role.trim().encode_utf16().count() <= 60
                && !role
                    .chars()
                    .any(|ch| ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}'))
        })
    }) {
        return Err(
            "Role must be a single line of up to 60 characters without control characters.",
        );
    }
    value["role"] = json!(value["role"].as_str().unwrap_or_default().trim());
    if !value["threadLimit"]
        .as_u64()
        .is_some_and(|value| (1..=32).contains(&value))
    {
        return Err("Side thread count must be 1 to 32.");
    }
    if value.get("toolBudget").is_some_and(|value| {
        !value
            .as_u64()
            .is_some_and(|value| (10..=200).contains(&value))
    }) {
        return Err("Tool steps per task must be 10 to 200.");
    }
    if let Some(backup) = value.get("backupModel") {
        let Some(backup) = backup.as_str().filter(|backup| {
            backup.encode_utf16().count() <= 160
                && !backup.chars().any(char::is_control)
                && Some(backup.trim()) != value["model"].as_str()
        }) else {
            return Err("Fallback model is invalid or matches the primary model.");
        };
        value["backupModel"] = json!(backup.trim());
    }
    if value
        .get("avatar")
        .is_some_and(|value| !value.as_str().is_some_and(valid_avatar))
    {
        return Err("Template avatar must be a static 256x256 PNG, up to 512 KiB.");
    }
    Ok(value)
}

fn valid_import(value: &Value) -> bool {
    fields(value, &["id", "template"]) && normalize(value["template"].clone()).is_ok()
}

fn template_field(draft: RwSignal<Option<Value>>, field: &'static str) -> Signal<String> {
    Signal::derive(move || {
        draft.with(|draft| {
            draft
                .as_ref()
                .map(|value| value[field].as_str().unwrap_or_default().to_owned())
                .unwrap_or_default()
        })
    })
}

fn edit_field(draft: RwSignal<Option<Value>>, field: &'static str, value: Value) {
    draft.update(|draft| {
        if let Some(draft) = draft {
            draft[field] = value;
        }
    });
}

fn preview_field(
    draft: RwSignal<Option<Value>>,
    field: &'static str,
    label: &'static str,
    limit: u32,
    required: bool,
) -> impl IntoView {
    let id = format!("agent-template-{field}");
    let role_hint = (field == "role").then_some("agent-template-role-hint");
    view! {
        <label class="k-field" for=id.clone()><span class="k-field__label">{label}
            {(!required).then(|| view! { <span class="k-field__optional">"Optional"</span> })}</span>
            <input class="k-input" id=id.clone() required=required maxlength=limit aria-describedby=role_hint prop:value=template_field(draft, field)
              on:input=move |event| edit_field(draft, field, json!(event_target_value(&event))) />
            {role_hint.map(|id| view! { <span id=id class="k-field__hint">"Shown under the agent's name, e.g. Orchestration agent"</span> })}
        </label>
    }
}

pub(super) fn import(lifecycle: Lifecycle) -> impl IntoView {
    let file = NodeRef::<leptos::html::Input>::new();
    let choose = NodeRef::<leptos::html::Button>::new();
    let journal = Journal::new(IMPORT_STORAGE.into(), LIMIT, valid_import);
    let draft = RwSignal::new(journal.pending.with_untracked(|pending| {
        pending
            .as_ref()
            .and_then(|pending| normalize(pending["template"].clone()).ok())
    }));
    let confirm = use_context::<Confirm>().unwrap_or_else(|| RwSignal::new(None));
    let processing = RwSignal::new(false);
    let locked = Signal::derive(move || journal.locked() || processing.get());
    let read = move || {
        let Some(file) = file
            .get_untracked()
            .and_then(|input| input.files())
            .and_then(|files| files.get(0))
        else {
            return;
        };
        journal.error.set(String::new());
        draft.set(None);
        if file.size() > LIMIT as f64 {
            journal
                .error
                .set("Template must be no larger than 750 KB.".into());
            return;
        }
        processing.set(true);
        spawn_local(async move {
            let result = async {
                let text = JsFuture::from(file.text())
                    .await
                    .ok()
                    .and_then(|value| value.as_string())
                    .ok_or("Template could not be read.")?;
                normalize(serde_json::from_str(&text).map_err(|_| "File is not valid JSON.")?)
            }
            .await;
            if processing.try_set(false).is_some() {
                return;
            }
            match result {
                Ok(value) => {
                    draft.set(Some(value));
                    request_animation_frame(move || {
                        if let Some(button) = choose.get_untracked() {
                            let _ = button.focus();
                        }
                    });
                }
                Err(message) => journal.error.set(message.into()),
            }
        });
    };
    let apply = move || {
        if processing.get_untracked() || journal.invalid.get_untracked() {
            return;
        }
        let command = if journal.pending.with_untracked(Option::is_some) {
            Value::Null
        } else {
            let Some(value) = draft.get_untracked() else {
                return;
            };
            match normalize(value) {
                Ok(template) => json!({"template":template}),
                Err(message) => {
                    journal.error.set(message.into());
                    return;
                }
            }
        };
        journal.run(
            command,
            |command| {
                let mut body = normalize(command["template"].clone()).unwrap_or_default();
                if let Some(object) = body.as_object_mut() {
                    object.remove("format");
                    object.remove("version");
                }
                body["id"] = command["id"].clone();
                ("/api/agents".into(), body)
            },
            false,
            move |agent| lifecycle.start_chat(&agent),
        );
    };
    view! {
        <Dialog id="agent-template-import" title=text("Import template") open=Signal::stored(true) on_close=move || lifecycle.close() size="lg"
          locked=move || processing.get() || journal.busy.get()>
            <div class="k-page bs-root">
                <Section title="Template file">
                    <input node_ref=file hidden type="file" accept=".json,application/json" disabled=locked on:change=move |_| read() />
                    <div><button node_ref=choose type="button" class="k-btn k-btn--md k-btn--secondary" autofocus
                      disabled=locked on:click=move |_| {
                        if !locked.get_untracked() {
                            if let Some(file) = file.get_untracked() { file.click(); }
                        }
                    }>"Choose file"</button></div>
                    <p role="status" class="k-field__hint" hidden=move || !processing.get()>"Processing template..."</p>
                    <Recovery journal confirm title="Creation is not confirmed yet" retry=Callback::new(move |()| apply())
                        refresh=Callback::new(move |()| {
                            draft.set(None); if let Some(file) = file.get_untracked() { file.set_value(""); }
                        }) />
                </Section>
                <Show when=move || draft.with(Option::is_some)>
                    <form on:submit=move |event| { event.prevent_default(); apply(); }>
                        <Section title="New profile preview">
                            <Card><fieldset class="k-fieldset agent-template-fields" disabled=locked>
                                <div class="k-inline">
                                    {move || draft.with(|value| value.as_ref().and_then(|value| value["avatar"].as_str()).map(|data|
                                        view! { <img class="agent-template-avatar" src=format!("data:image/png;base64,{data}") alt="Avatar to import" width="64" height="64" /> }
                                    ))}
                                    <Button label="Omit avatar" variant="ghost" size="sm" disabled=move || draft.with(|draft| draft.as_ref().is_none_or(|value| value.get("avatar").is_none()))
                                      click=Callback::new(move |()| draft.update(|draft| { if let Some(object) = draft.as_mut().and_then(Value::as_object_mut) { object.remove("avatar"); } })) />
                                </div>
                                {preview_field(draft, "name", "Name", 80, true)}
                                {preview_field(draft, "role", "Role", 60, false)}
                                {preview_field(draft, "model", "Model", 160, true)}
                                {preview_field(draft, "backupModel", "Fallback model", 160, false)}
                                <p class="k-field__hint">"Models use the provider configured on this computer. Model IDs from another computer may be unavailable."</p>
                                <ui::Field label="Instructions" name="agent-template-instructions">
                                    <textarea id="agent-template-instructions" class="k-input k-textarea" required maxlength="8000" rows="8" prop:value=template_field(draft,"instructions")
                                      on:input=move |event| edit_field(draft, "instructions", json!(event_target_value(&event)))></textarea>
                                </ui::Field>
                                <ui::Field label="Parallel chats" name="agent-template-parallel">
                                    <input id="agent-template-parallel" class="k-input" type="number" min="1" max="32" required step="1"
                                      prop:value=move || draft.with(|draft| draft.as_ref().map(|value| value["threadLimit"].to_string()).unwrap_or_default())
                                      on:input=move |event| edit_field(draft, "threadLimit", json!(event_target_value(&event).parse::<u64>().ok())) />
                                </ui::Field>
                            </fieldset></Card>
                            <div class="k-row-actions">
                                <button type="submit" class="k-btn k-btn--md k-btn--primary" disabled=move || journal.busy.get() || processing.get()>
                                    {move || if journal.pending.with(Option::is_some) { "Retry creation" } else { "Create agent" }}
                                </button>
                            </div>
                        </Section>
                    </form>
                </Show>
            </div>
        </Dialog>
    }
}

struct ReviewedTemplate<'a>(&'a Value);

impl serde::Serialize for ReviewedTemplate<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut fields = serializer.serialize_map(None)?;
        for name in [
            "format",
            "version",
            "name",
            "role",
            "model",
            "backupModel",
            "instructions",
            "threadLimit",
            "toolBudget",
            "avatar",
        ] {
            if let Some(value) = self.0.get(name) {
                fields.serialize_entry(name, value)?;
            }
        }
        fields.end()
    }
}

fn download(text: String) -> Result<(), JsValue> {
    let data = js_sys::Array::of1(&js_sys::Uint8Array::from(text.as_bytes()));
    let options = BlobPropertyBag::new();
    options.set_type("application/json");
    let blob = Blob::new_with_u8_array_sequence_and_options(&data, &options)?;
    let url = Url::create_object_url_with_blob(&blob)?;
    let anchor = document()
        .create_element("a")?
        .dyn_into::<HtmlAnchorElement>()?;
    anchor.set_href(&url);
    anchor.set_download("minarbot-agent-template.json");
    anchor.click();
    set_timeout(
        move || {
            let _ = Url::revoke_object_url(&url);
        },
        std::time::Duration::from_secs(1),
    );
    Ok(())
}

pub(super) fn export(
    agent: Agent,
    close: impl Fn() + Copy + Send + Sync + 'static,
    from_chat: bool,
) -> impl IntoView {
    let template = RwSignal::new(None::<Value>);
    let busy = RwSignal::new(false);
    let failure = RwSignal::new(String::new());
    let endpoint = StoredValue::new(format!("/api/agents/{}/template", agent.id));
    let preview = move || {
        failure.set(String::new());
        template.set(None);
        busy.set(true);
        spawn_local(async move {
            let result = api::read::<Value>(&endpoint.get_value())
                .await
                .and_then(|data| normalize(data).map_err(str::to_owned));
            if busy.try_set(false).is_some() {
                return;
            }
            match result {
                Ok(value) => template.set(Some(value)),
                Err(message) => failure.set(message),
            }
        });
    };
    let footer = content(move || {
        (!from_chat)
            .then(|| view! { <Button label="Done" click=Callback::new(move |()| close()) /> })
    });
    view! {
        <Dialog id="agent-template-export" title=text(if from_chat { format!("{} · Export template", agent.name) } else { "Export template".into() })
          description=text(if from_chat { String::new() } else { agent.name.clone() })
          open=Signal::stored(true) on_close=close size="lg" footer=footer>
            <div class="k-page bs-root">
                <Section title="Export template">
                    <p class="k-field__hint">"Export includes the saved profile and avatar, without history, conversation files, or provider credential settings. Instructions are included in the download: check for sensitive information before sharing the file."</p>
                    <div class="k-row-actions">
                        <crate::system::kit::Button label=Signal::derive(move || if busy.get() { "Loading saved profile...".to_owned() } else { "Preview export template".to_owned() })
                            size="sm" disabled=busy on_click=preview />
                    </div>
                    {error(failure)}
                    {move || template.get().map(|value| {
                        let avatar = value["avatar"].as_str().map(str::to_owned);
                        let formatted = serde_json::to_string_pretty(&ReviewedTemplate(&value)).unwrap_or_default();
                        let contents = formatted.clone();
                        view! {
                            {avatar.map(|image| view! { <img class="agent-template-avatar" src=format!("data:image/png;base64,{image}") alt="Template avatar" width="64" height="64" /> })}
                            {code("File contents to download".into(),formatted)}
                            <div class="k-row-actions"><Button label="Download reviewed template" size="sm" click=Callback::new(move |()| {
                                if download(format!("{contents}\n")).is_err() { failure.set("Template could not be downloaded.".into()); }
                            }) /></div>
                        }
                    })}
                </Section>
            </div>
        </Dialog>
    }
}
