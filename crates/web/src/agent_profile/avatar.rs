use super::*;
use crate::connection_ui::{self as ui, query, ErrorNotice};
use base64::{engine::general_purpose::STANDARD, Engine};
use js_sys::Uint8Array;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{CanvasRenderingContext2d, File, HtmlCanvasElement, HtmlImageElement};

const MAX_IMAGE: usize = 512 * 1024;
const IMAGE_ERROR: &str = "Choose a PNG, JPEG, or WebP image up to 512 KiB.";
const EYES: &str =
    "M43 49a3.5 3.5 0 0 0-7 0v5a3.5 3.5 0 0 0 7 0ZM64 49a3.5 3.5 0 0 0-7 0v5a3.5 3.5 0 0 0 7 0Z";
const CHECK: &str = "M20 6 9 17l-5-5";

#[derive(Deserialize, Default)]
struct Looks {
    shapes: Vec<Shape>,
    colors: Vec<Color>,
}
#[derive(Deserialize)]
struct Shape {
    name: String,
    path: String,
}
#[derive(Deserialize)]
struct Color {
    name: String,
    fill: String,
}

fn looks() -> &'static Looks {
    static LOOKS: std::sync::OnceLock<Looks> = std::sync::OnceLock::new();
    LOOKS.get_or_init(|| {
        serde_json::from_str(include_str!("../../assets/ui/avatar-looks.json"))
            .unwrap_or_default()
    })
}

fn check_icon(size: u8, stroke: &'static str) -> impl IntoView {
    view! {
        <svg xmlns="http://www.w3.org/2000/svg" width=size height=size viewBox="0 0 24 24"
            fill="none" stroke=stroke stroke-width="2" stroke-linecap="round" stroke-linejoin="round"
            class="lucide lucide-check" aria-hidden="true">
            <path d=CHECK></path>
        </svg>
    }
}

#[derive(Clone, Copy)]
pub(super) struct Look {
    shape: usize,
    color: usize,
}

pub(super) fn initial(name: &str) -> String {
    name.trim()
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string()
}

pub(super) fn picture(agent: &Agent, size: u32) -> impl IntoView {
    let content = if agent.avatar_revision > 0 {
        let src = format!(
            "/api/agents/{}/avatar/image?v={}",
            agent.id, agent.avatar_revision
        );
        view! { <img src=src alt="" /> }.into_any()
    } else {
        initial(&agent.name).into_any()
    };
    view! { <span class="avatar bs-avatar" style:width=format!("{size}px") style:height=format!("{size}px")
    style:font-size=format!("{}px", (size as f64 * 0.42).round()) aria-hidden="true">{content}</span> }
}

pub(super) fn mascot(look: Look, size: u32) -> impl IntoView {
    let shape = looks()
        .shapes
        .get(look.shape)
        .map_or("", |shape| shape.path.as_str());
    let color = looks()
        .colors
        .get(look.color)
        .map_or("", |color| color.fill.as_str());
    view! { <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width=size height=size aria-hidden="true">
        <path d=shape fill=color></path><path d=EYES fill="#1C1C1C"></path>
    </svg> }
}

pub(super) async fn png(look: Look) -> Result<String, String> {
    async fn render(look: Look) -> Option<String> {
        let shape = &looks().shapes.get(look.shape)?.path;
        let color = &looks().colors.get(look.color)?.fill;
        let svg = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="256" height="256"><path d="{shape}" fill="{color}"/><path d="{EYES}" fill="#1C1C1C"/></svg>"##
        );
        let image = HtmlImageElement::new().ok()?;
        image.set_src(&format!(
            "data:image/svg+xml;base64,{}",
            STANDARD.encode(svg)
        ));
        JsFuture::from(image.decode()).await.ok()?;
        let canvas = document()
            .create_element("canvas")
            .ok()?
            .dyn_into::<HtmlCanvasElement>()
            .ok()?;
        canvas.set_width(256);
        canvas.set_height(256);
        let context = canvas
            .get_context("2d")
            .ok()??
            .dyn_into::<CanvasRenderingContext2d>()
            .ok()?;
        context
            .draw_image_with_html_image_element(&image, 0.0, 0.0)
            .ok()?;
        let uri = canvas.to_data_url_with_type("image/png").ok()?;
        Some(uri.strip_prefix("data:image/png;base64,")?.to_owned())
    }
    render(look)
        .await
        .ok_or_else(|| "Avatar could not render. Try uploading an image instead.".into())
}

pub(super) fn record_failure(id: &str) {
    if let Ok(Some(storage)) = window().session_storage() {
        let _ = storage.set_item("minarbot:avatar-save-failed", id);
    }
}

pub(super) async fn save_created(id: &str, image: String) -> Result<(), String> {
    let command = json!({"id":ui::uuid(),"revision":0,"image":image});
    let key = format!("minarbot-avatar:{id}");
    let raw = command.to_string();
    let storage = window()
        .session_storage()
        .ok()
        .flatten()
        .ok_or("Browser recovery storage is unavailable.")?;
    storage
        .set_item(&key, &raw)
        .map_err(|_| "Browser could not save recovery data.")?;
    api::post::<Value>(&format!("/api/agents/{id}/avatar"), &command).await?;
    if storage.get_item(&key).ok().flatten().as_deref() == Some(&raw) {
        storage
            .remove_item(&key)
            .map_err(|_| "Recovery data could not be removed.")?;
    }
    Ok(())
}

fn picker(
    name: Signal<String>,
    look: RwSignal<Option<Look>>,
    locked: Signal<bool>,
    changed: Callback<()>,
) -> impl IntoView {
    let color = move || look.with(|look| look.map_or(0, |look| look.color));
    let choose = move |next| {
        look.set(next);
        changed.run(());
    };
    let preview_label = move || {
        look.with(|look| {
            look.and_then(|look| {
                Some(format!(
                    "{} {} preview",
                    looks().colors.get(look.color)?.name,
                    looks().shapes.get(look.shape)?.name
                ))
            })
            .unwrap_or_else(|| "Initials avatar preview".into())
        })
    };
    view! {
        <div class="k-avatar-picker bs-root">
            <div class="k-avatar-picker__head">
                <div class="k-avatar-picker__preview" role="img" aria-label=preview_label>
                    {move || match look.get() {
                        Some(look) => mascot(look, 76).into_any(),
                        None => view! { <span class="bs-avatar" aria-hidden="true">{initial(&name.get())}</span> }.into_any(),
                    }}
                </div>
                <div class="k-avatar-picker__intro">
                    <span class="k-field__label">"Choose a look "<span class="k-field__optional">"Optional"</span></span>
                    <span class="k-field__hint">"Pick a shape and color, or keep initials."</span>
                    <Show when=move || look.with(Option::is_some)>
                        <Button label="Clear choice" size="sm" variant="ghost" disabled=locked click=Callback::new(move |()| choose(None)) />
                    </Show>
                </div>
            </div>
            <div class="k-avatar-picker__shapes" role="radiogroup" aria-label="Avatar shape" on:keydown=move |event| crate::settings::move_focus(&event, "[role=radiogroup]", true)>
                {looks().shapes.iter().enumerate().map(|(index, shape)| {
                    let selected = move || look.with(|look| look.is_some_and(|look| look.shape == index));
                    view! {
                        <button type="button" class="k-avatar-picker__shape" role="radio" aria-label=shape.name.clone() title=shape.name.clone()
                          disabled=locked aria-checked=move || selected().to_string() data-state=move || if selected() {"checked"} else {"unchecked"}
                          tabindex=move || if selected() || index == 0 && look.with(Option::is_none) { 0 } else { -1 }
                          on:click=move |_| choose(Some(Look { shape:index, color:color() }))>
                            {move || mascot(Look { shape:index, color:color() }, 42)}
                            <Show when=selected><span class="k-avatar-picker__check">{check_icon(10, "currentColor")}</span></Show>
                        </button>
                    }
                }).collect_view()}
            </div>
            <div class="k-avatar-picker__colors" role="radiogroup" aria-label="Avatar color" on:keydown=move |event| crate::settings::move_focus(&event, "[role=radiogroup]", true)>
                {looks().colors.iter().enumerate().map(|(index, swatch)| {
                    let selected = move || color() == index;
                    view! { <button type="button" class="k-avatar-picker__swatch" role="radio" aria-label=swatch.name.clone() title=swatch.name.clone()
                      disabled=locked aria-checked=move || selected().to_string() data-state=move || if selected() {"checked"} else {"unchecked"}
                      tabindex=move || if selected() { 0 } else { -1 }
                      on:click=move |_| choose(Some(Look { shape:look.with_untracked(|look| look.map_or(0, |look| look.shape)), color:index }))>
                        <span style:background=swatch.fill.clone()><Show when=selected><span>{check_icon(14, "#1C1C1C")}</span></Show></span>
                    </button> }
                }).collect_view()}
            </div>
        </div>
    }
}

pub(super) fn picker_dialog(
    name: RwSignal<String>,
    look: RwSignal<Option<Look>>,
    close: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    view! {
        <Dialog id="agent-look" title=text("Avatar") open=Signal::stored(true) on_close=close
          footer=content(move || view! { <Button label="Done" click=Callback::new(move |()| close()) /> })>
            {picker(name.into(), look, Signal::stored(false), Callback::new(|()| {}))}
        </Dialog>
    }
}

fn valid_request(value: &Value) -> bool {
    fields(value, &["id", "revision", "image"])
        && revision(&value["revision"], 0)
        && value.get("image").is_some_and(|image| {
            image.is_null()
                || image.as_str().is_some_and(|data| {
                    data.len() <= 699_052
                        && STANDARD
                            .decode(data)
                            .is_ok_and(|bytes| !bytes.is_empty() && bytes.len() <= MAX_IMAGE)
                })
        })
}

fn valid_file(file: &File) -> bool {
    file.size() > 0.0
        && file.size() <= MAX_IMAGE as f64
        && matches!(
            file.type_().as_str(),
            "image/png" | "image/jpeg" | "image/webp"
        )
}

pub(super) fn editor(
    lifecycle: Lifecycle,
    agent: Agent,
    close: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let confirm = use_context::<Confirm>().unwrap_or_else(|| RwSignal::new(None));
    let agent_id = StoredValue::new(agent.id);
    let endpoint = StoredValue::new(format!("/api/agents/{}/avatar", agent_id.get_value()));
    let status = query::<Value>(move || endpoint.get_value());
    let journal = Journal::new(
        format!("minarbot-avatar:{}", agent_id.get_value()),
        701_000,
        valid_request,
    );
    let look = RwSignal::new(None::<Look>);
    let read_file = RwSignal::new(false);
    let filename = RwSignal::new(String::new());
    let valid_selection = RwSignal::new(false);
    let notice = RwSignal::new("");
    let file = NodeRef::<leptos::html::Input>::new();
    let locked = Signal::derive(move || {
        status.data.with(Option::is_none) || read_file.get() || journal.locked()
    });
    let file_changed = Callback::new(move |()| {
        if let Some(file) = file.get_untracked() {
            file.set_value("");
        }
        filename.set(String::new());
        valid_selection.set(false);
        journal.error.set(String::new());
    });
    let submit = move |image: Value| {
        let version = status.data.with_untracked(|data| {
            data.as_ref()
                .map_or(Value::Null, |value| value["revision"].clone())
        });
        journal.run(
            json!({"revision":version,"image":image}),
            |command| (endpoint.get_value(), command.clone()),
            false,
            move |result| {
                status.invalidate();
                status.data.set(Some(result));
                file_changed.run(());
                look.set(None);
                notice.set("Avatar updated.");
                status.reload.run(());
                lifecycle.refresh(&agent_id.get_value(), false);
            },
        );
    };
    let save = move || {
        if journal.pending.with_untracked(Option::is_some) {
            submit(Value::Null);
            return;
        }
        let selected = file
            .get_untracked()
            .and_then(|input| input.files())
            .and_then(|files| files.get(0))
            .filter(valid_file);
        let selected_look = look.get_untracked();
        read_file.set(true);
        spawn_local(async move {
            let image = if let Some(look) = selected_look {
                png(look).await
            } else if let Some(file) = selected {
                JsFuture::from(file.array_buffer())
                    .await
                    .map(|buffer| STANDARD.encode(Uint8Array::new(&buffer).to_vec()))
                    .map_err(|_| IMAGE_ERROR.into())
            } else {
                Err(IMAGE_ERROR.into())
            };
            if read_file.try_set(false).is_some() {
                return;
            }
            match image {
                Ok(image) => submit(json!(image)),
                Err(message) => journal.error.set(message),
            }
        });
    };
    let image = Signal::derive(move || {
        status.data.with(|data| {
            data.as_ref()
                .filter(|value| value["present"] == true)
                .map(|value| format!("{}/image?v={}", endpoint.get_value(), value["revision"]))
        })
    });
    view! {
        <Dialog id="agent-avatar" title=text("Avatar") open=Signal::stored(true) on_close=close locked=move || journal.busy.get() || read_file.get()
            footer=content(move || view! {
                <button type="button" class="k-btn k-btn--md k-btn--ghost k-dialog__foot-start" disabled=move || locked.get() || image.with(Option::is_none)
                  on:click=move |_| ask(Some(confirm), "Remove avatar?", "", "Remove avatar", true, move || submit(Value::Null))>"Remove avatar"</button>
                <Button label="Done" disabled=journal.busy click=Callback::new(move |()| close()) />
                <button type="button" class="k-btn k-btn--md k-btn--primary" disabled=move || journal.busy.get() || journal.invalid.get() || read_file.get() || status.data.with(Option::is_none)
                  || journal.pending.with(Option::is_none) && !valid_selection.get() && look.with(Option::is_none)
                  on:click=move |_| save()>{move || if journal.busy.get() { "Saving..." } else if journal.pending.with(Option::is_some) { "Try again" } else { "Save" }}</button>
            })>
            <Section title="Image" actions=content(move || view! {
                <button type="button" class="k-icon-btn" aria-label="Reload avatar" disabled=journal.busy on:click=move |_| {
                    if journal.locked() {
                        ask(Some(confirm), "Discard the saved avatar request and load the latest version?", "Changes already accepted by the server still apply.",
                            "Discard and reload", true, move || journal.discard(status.reload));
                    } else { status.reload.run(()); }
                }>{icon(icons().refresh)}</button>
            })>
                <Card><Row title=text("Current avatar") trailing=content(move || view! {
                    {move || image.get().map(|src| view! { <img class="agent-avatar-current" src=src alt="Current agent avatar" width="64" height="64" /> })}
                }) /></Card>
                {picker(Signal::stored(String::new()), look, locked, file_changed)}
                <div class="k-field"><span class="k-field__label">"Upload image"</span>
                  <input type="file" accept="image/png,image/jpeg,image/webp" node_ref=file hidden disabled=locked
                    on:change=move |_| {
                        let selected = file.get_untracked().and_then(|input| input.files()).and_then(|files| files.get(0));
                        let valid = selected.as_ref().is_some_and(valid_file);
                        if selected.is_some() { look.set(None); }
                        filename.set(selected.as_ref().map_or_else(String::new, File::name));
                        valid_selection.set(valid);
                        journal.error.set(if selected.is_some() && !valid { IMAGE_ERROR.into() } else { String::new() });
                    } />
                  <div class="k-row-actions">
                    <Button label="Choose image" aria="Choose avatar image" size="sm" disabled=locked click=Callback::new(move |()| {
                        if let Some(file) = file.get_untracked() { file.click(); }
                    }) />
                    <span class="k-field__hint agent-avatar-filename" role="status">{move || filename.with(|name| if name.is_empty() { "No image chosen".into() } else { name.clone() })}</span>
                  </div>
                </div>
                <ErrorNotice error=status.error.into() retry=status.reload />
                <Recovery journal confirm title="Not confirmed" retry=Callback::new(move |()| save()) refresh=status.reload />
                <span class="sr-only" role="status">{notice}</span>
            </Section>
        </Dialog>
    }
}
