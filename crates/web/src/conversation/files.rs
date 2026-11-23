use super::*;
use results::{Kind, ResultTarget};
use serde_json::json;

#[derive(Clone, PartialEq, Deserialize)]
pub(super) struct Context {
    pub computer: Option<String>,
    pub workspace: Option<Value>,
    #[serde(default)]
    sites: Vec<Site>,
    #[serde(default)]
    previews: Vec<PanelFile>,
}

#[derive(Clone, PartialEq, Deserialize)]
struct Site {
    id: String,
    name: String,
    revision: u64,
}

#[derive(Clone, PartialEq, Deserialize)]
pub(super) struct PanelFile {
    id: String,
    name: String,
    #[serde(default)]
    seq: u64,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    image: bool,
}

#[derive(Clone, PartialEq)]
struct Preview {
    path: String,
    name: String,
    transcribe: bool,
}

pub(super) fn url(path: &str, pairs: &[(&str, &str)]) -> String {
    let Ok(params) = web_sys::UrlSearchParams::new() else {
        return path.into();
    };
    for (key, value) in pairs {
        params.set(key, value);
    }
    let sep = if path.contains('?') { '&' } else { '?' };
    format!(
        "{path}{sep}{}",
        params.to_string().as_string().unwrap_or_default()
    )
}

fn is_html(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.ends_with(".html") || name.ends_with(".htm")
}

pub(super) fn empty(title: &'static str) -> impl IntoView {
    view! { <div class="k-empty"><div class="k-empty__title">{title}</div></div> }
}

pub(super) fn loading(label: &'static str) -> impl IntoView {
    view! {
        <span class="sr-only" role="status">{label}</span>
        <div class="k-card" aria-hidden="true">
            {(0..3).map(|_| view! { <div class="k-row"><div class="k-row__main"><span class="k-row__text"><span class="k-skeleton panel-skeleton"></span><span class="k-skeleton is-small panel-skeleton"></span></span></div></div> }).collect_view()}
        </div>
    }
}

pub(super) fn retry(message: String, refresh: RwSignal<u32>) -> impl IntoView {
    let icon = use_context::<Icons>().unwrap_or_default().alert;
    view! { <div class="k-alert is-error" role="alert"><span class="k-alert__icon" inner_html=icon></span><div class="k-alert__body">{message}</div>
        <div class="k-alert__actions"><button type="button" class="k-btn k-btn--sm k-btn--secondary"
            on:click=move |_| refresh.update(|n| *n = n.wrapping_add(1))>"Retry"</button></div>
    </div> }
}

// ponytail: each panel has one request at a time per endpoint key. Share cached reads if multiple consumers need the same endpoint.
pub(super) fn watch<T>(
    endpoint: impl Fn() -> String + 'static,
    refresh: RwSignal<u32>,
    data: RwSignal<Option<Result<T, String>>>,
) where
    T: DeserializeOwned + PartialEq + Send + Sync + 'static,
{
    let tick = RwSignal::new(0_u32);
    let resume = move || {
        if !document().hidden() {
            tick.update(|n| *n = n.wrapping_add(1));
        }
    };
    let timer =
        leptos::leptos_dom::helpers::set_interval_with_handle(resume, Duration::from_secs(5)).ok();
    let focus = window_event_listener(leptos::ev::focus, move |_| resume());
    on_cleanup(move || {
        if let Some(timer) = timer {
            timer.clear();
        }
        focus.remove();
    });
    let generation = StoredValue::new(0_u64);
    Effect::new(move |previous: Option<String>| {
        tick.track();
        refresh.track();
        let endpoint = endpoint();
        if previous.as_ref() != Some(&endpoint) {
            data.set(None);
        }
        let id = generation.get_value().wrapping_add(1);
        generation.set_value(id);
        if !endpoint.is_empty() {
            let endpoint = endpoint.clone();
            leptos::task::spawn_local(async move {
                let next = get(&endpoint).await.map_err(|error| error.message);
                if generation.try_get_value() == Some(id)
                    && data.with_untracked(|old| old.as_ref() != Some(&next))
                {
                    data.set(Some(next));
                }
            });
        }
        endpoint
    });
}

fn size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let (scale, unit) = if bytes < 1_048_576 {
        (1024.0, "KiB")
    } else {
        (1_048_576.0, "MiB")
    };
    format!("{} {unit}", (bytes as f64 / scale * 10.0).round() / 10.0)
}

pub(super) fn panel(
    chat: Chat,
    context: RwSignal<Option<Result<Context, String>>>,
    git: RwSignal<Option<Result<super::changes::Status, String>>>,
    page: RwSignal<Option<Result<Vec<PanelFile>, String>>>,
    refresh: RwSignal<u32>,
) -> impl IntoView {
    let earlier = RwSignal::new(Vec::<PanelFile>::new());
    let removed = RwSignal::new(Vec::<String>::new());
    let more = RwSignal::new(true);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let selected = RwSignal::new(None::<Preview>);
    let opener = StoredValue::new_local(None::<web_sys::HtmlElement>);
    let copies = RwSignal::new(HashMap::<String, String>::new());
    let icons = use_context::<Icons>().unwrap_or_default();
    let has_workspace = Memo::new(move |_| {
        context.with(|value| {
            value
                .as_ref()
                .and_then(|value| value.as_ref().ok())
                .is_some_and(|value| value.workspace.is_some())
        })
    });
    let has_preview = move || {
        context.with(|value| {
            value
                .as_ref()
                .and_then(|value| value.as_ref().ok())
                .is_some_and(|value| !value.sites.is_empty() || !value.previews.is_empty())
        })
    };
    let rows = Memo::new(move |_| {
        let mut rows = page
            .with(|value| {
                value
                    .as_ref()
                    .and_then(|value| value.as_ref().ok())
                    .cloned()
            })
            .unwrap_or_default();
        rows.extend(earlier.get());
        let mut seen = std::collections::HashSet::new();
        rows.retain(|file| {
            protocol::id(&file.id)
                && seen.insert(file.id.clone())
                && !removed.with(|ids| ids.contains(&file.id))
        });
        rows
    });
    let load_more = move |_| {
        if busy.get_untracked() {
            return;
        }
        let Some(seq) = rows.with_untracked(|rows| rows.last().map(|file| file.seq)) else {
            return;
        };
        let url = format!("{}/panel/files?before={seq}", chat.base());
        busy.set(true);
        error.set(String::new());
        leptos::task::spawn_local(async move {
            match get::<Vec<PanelFile>>(&url).await {
                Ok(page) => {
                    more.try_set(page.len() == 100);
                    earlier.try_update(|rows| rows.extend(page));
                }
                Err(reason) => {
                    error.try_set(reason.message);
                }
            }
            busy.try_set(false);
        });
    };
    let open = Callback::new(move |file: Preview| {
        opener.set_value(
            document()
                .active_element()
                .and_then(|node| node.dyn_into().ok()),
        );
        if file.transcribe || file.path.contains('?') {
            results::clear(chat);
            selected.set(Some(file));
        } else {
            selected.set(None);
            results::open(chat, ResultTarget::file(file.path, file.name));
        }
    });
    let active = Memo::new(move |_| {
        chat.result
            .with(|target| {
                target
                    .as_ref()
                    .filter(|target| target.view == Kind::File)
                    .map(|target| Preview {
                        path: target.path.clone(),
                        name: target.name.clone(),
                        transcribe: false,
                    })
            })
            .or_else(|| selected.get())
    });
    let close = Callback::new(move |()| {
        selected.set(None);
        chat.select_tab("files");
        request_animation_frame(move || {
            if let Some(node) = opener
                .try_get_value()
                .flatten()
                .filter(|node| node.is_connected())
            {
                let _ = node.focus();
            }
        });
    });
    view! {
        <div class="right-panel__body k-stack">
            {move || active.get().map(|file| inline(file, close))}
            <div class="k-stack" hidden=move || active.with(Option::is_some)>
                <div class="k-stack" hidden=move || !has_preview() && !git.with(|value| {
                    value.as_ref().and_then(|value| value.as_ref().ok()).is_some_and(|value| value.is_repo)
                })>
                    {move || git.with(|git| git.as_ref().and_then(|value| value.as_ref().ok()).filter(|status| status.is_repo)
                        .map(|status| view! { <button type="button" class="right-panel__nav-row k-btn k-btn--ghost k-btn--md" on:click=move |_| chat.select_tab("diff")>
                            <span class="appearance-icon" inner_html=icons.diff.clone()></span>"Source control" <span class="k-badge is-neutral">{format!("{} changed", status.changed_files.len())}</span>
                        </button> }))}
                    <button type="button" class="right-panel__nav-row k-btn k-btn--ghost k-btn--md" hidden=move || !has_preview()
                        on:click=move |_| chat.select_tab("preview")><span class="appearance-icon" inner_html=icons.globe></span>"Preview artifacts"</button>
                </div>
                <section class="k-section" hidden=move || has_workspace.get() && rows.with(Vec::is_empty) && page.with(Option::is_some)>
                    <div class="k-section__head"><h2 class="k-section__title">"Conversation files"</h2></div>
                    {move || if page.with(Option::is_none) {
                        loading("Loading files...").into_any()
                    } else if rows.with(Vec::is_empty) && !has_workspace.get() {
                        empty("No files yet").into_any()
                    } else { ().into_any() }}
                    <div class="attachments__list right-panel__files" role="region" aria-label="Conversation files list" tabindex="0" hidden=move || rows.with(Vec::is_empty)>
                        <For each=move || rows.get() key=|file| (file.id.clone(), file.name.clone(), file.size, file.image)
                            children=move |file| row(chat, file, open, removed, copies, error, refresh) />
                    </div>
                    {move || (!error.with(String::is_empty)).then(|| retry(error.get(), refresh))}
                    <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=busy hidden=move || !more.get()
                        || page.with(|value| value.as_ref().and_then(|value| value.as_ref().ok()).is_none_or(|rows| rows.len() < 100))
                        on:click=load_more>"Earlier files"</button>
                </section>
                <Show when=move || has_workspace.get()>
                    {workspace(format!("{}/panel/workspace", chat.base()), None, Some(open))}
                </Show>
            </div>
        </div>
    }
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = MinarbotConversation, catch, js_name = downloadWorkspace)]
    async fn download_workspace(
        path: &str,
        name: &str,
    ) -> Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>;

    #[wasm_bindgen(js_namespace = MinarbotConversation, catch, js_name = copyPanelFile)]
    async fn copy_panel_file(
        base: &str,
        source: &str,
        name: &str,
    ) -> Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>;
}

fn row(
    chat: Chat,
    file: PanelFile,
    open: Callback<Preview>,
    removed: RwSignal<Vec<String>>,
    copies: RwSignal<HashMap<String, String>>,
    error: RwSignal<String>,
    refresh: RwSignal<u32>,
) -> impl IntoView {
    let icons = use_context::<Icons>().unwrap_or_default();
    let path = format!("{}/panel/files/{}", chat.base(), file.id);
    let direct = StoredValue::new(format!("{}/files/{}", chat.base(), file.id));
    let preview = StoredValue::new(Preview {
        path: path.clone(),
        name: file.name.clone(),
        transcribe: false,
    });
    let id = StoredValue::new(file.id);
    let name = file.name;
    let length = file.size;
    let audio = name.rsplit_once('.').is_some_and(|(_, ext)| {
        matches!(
            ext.to_ascii_lowercase().as_str(),
            "mp3" | "wav" | "ogg" | "m4a" | "webm" | "mp4" | "mov"
        )
    });
    let menu = NodeRef::<leptos::html::Div>::new();
    let trigger = NodeRef::<leptos::html::Button>::new();
    let confirm = NodeRef::<leptos::html::Dialog>::new();
    let expanded = RwSignal::new(false);
    let close_menu = move || {
        if let Some(menu) = menu.get_untracked() {
            let _ = menu.hide_popover();
        }
        if let Some(trigger) = trigger.get_untracked() {
            let _ = trigger.focus();
        }
    };
    let show_menu = move |last: bool| {
        let (Some(menu), Some(trigger)) = (menu.get_untracked(), trigger.get_untracked()) else {
            return;
        };
        if menu.matches(":popover-open").unwrap_or(false) {
            close_menu();
            return;
        }
        if menu.show_popover().is_err() {
            return;
        }
        let rect = trigger.get_bounding_client_rect();
        let height = f64::from(menu.offset_height());
        let viewport = window()
            .inner_height()
            .ok()
            .and_then(|height| height.as_f64())
            .unwrap_or_default();
        let top = if rect.bottom() + height + 6.0 <= viewport {
            rect.bottom() + 6.0
        } else {
            (rect.top() - height - 6.0).max(8.0)
        };
        let left = (rect.right() - f64::from(menu.offset_width())).max(8.0);
        let style = web_sys::HtmlElement::style(&menu);
        let _ = style.set_property("left", &format!("{left}px"));
        let _ = style.set_property("top", &format!("{top}px"));
        let selector = if last {
            "[role=menuitem]:last-child:not(:disabled)"
        } else {
            "[role=menuitem]:not(:disabled)"
        };
        if let Some(item) = menu
            .query_selector(selector)
            .ok()
            .flatten()
            .and_then(|item| item.dyn_into::<web_sys::HtmlElement>().ok())
        {
            let _ = item.focus();
        }
    };
    let attached_id = move || {
        copies
            .with(|copies| id.with_value(|id| copies.get(id).cloned()))
            .unwrap_or_else(|| id.get_value())
    };
    let attached = move || chat.files.with(|ids| ids.contains(&attached_id()));
    let attach = move || {
        if chat.locked() {
            return;
        }
        let current = attached_id();
        let mut files = chat.files.get_untracked();
        if files.contains(&current) {
            files.retain(|id| id != &current);
            chat.attach(files);
            return;
        }
        if files.len() >= MAX_FILES {
            return;
        }
        if length > 10 * 1024 * 1024 {
            error.set("Maximum file size is 10 MiB.".into());
            return;
        }
        let target = direct.get_value();
        let endpoint = format!("{}/files", chat.base());
        let file = preview.get_value();
        let original = id.get_value();
        chat.panel_upload.set(true);
        leptos::task::spawn_local(async move {
            let result =
                match crate::api::read_status::<Value>(&format!("{target}?metadata=1")).await {
                    Ok(_) => Ok(original.clone()),
                    Err((404, _)) => copy_panel_file(&endpoint, &file.path, &file.name)
                        .await
                        .map_err(|error| {
                            error
                                .as_string()
                                .unwrap_or_else(|| "File could not load. Try again.".into())
                        })
                        .and_then(|value| {
                            value
                                .as_string()
                                .filter(|id| protocol::id(id))
                                .ok_or_else(|| "File receipt is invalid.".into())
                        }),
                    Err((_, error)) => Err(error),
                };
            if chat.files.is_disposed() {
                return;
            }
            chat.panel_upload.set(false);
            match result {
                Ok(copy) => {
                    copies.try_update(|copies| {
                        copies.insert(original, copy.clone());
                    });
                    if !chat.locked() {
                        let mut files = chat.files.get_untracked();
                        if files.len() < MAX_FILES && !files.contains(&copy) {
                            files.push(copy);
                            chat.attach(files);
                        }
                    }
                    refresh.try_update(|n| *n = n.wrapping_add(1));
                }
                Err(message) => {
                    error.try_set(message.clone());
                    chat.error.set(message);
                }
            }
        });
    };
    let delete = move |_| {
        if chat.locked() {
            return;
        }
        let endpoint = direct.get_value();
        let file = id.get_value();
        chat.panel_upload.set(true);
        leptos::task::spawn_local(async move {
            let result = Request::delete(&endpoint).send().await;
            if chat.files.is_disposed() {
                return;
            }
            chat.panel_upload.set(false);
            let message = match result {
                Ok(response) if response.ok() => {
                    removed.try_update(|removed| removed.push(file.clone()));
                    let mut ids = chat.files.get_untracked();
                    if ids.contains(&file) {
                        ids.retain(|id| id != &file);
                        chat.attach(ids);
                    }
                    refresh.try_update(|n| *n = n.wrapping_add(1));
                    if let Ok(event) = web_sys::Event::new("minarbot:message-changed") {
                        let _ = window().dispatch_event(&event);
                    }
                    return;
                }
                Ok(response) => {
                    crate::api::response_error(response, "File could not be deleted. Try again.")
                        .await
                }
                Err(_) => "File could not be deleted. Try again.".into(),
            };
            error.try_set(message);
        });
    };
    view! {
        <div class="right-panel__file-row">
            <button type="button" class="right-panel__file" aria-label=format!("Preview {name}") on:click=move |_| open.run(preview.get_value())>
                <span class="right-panel__thumbnail">
                    {if file.image { view! { <img src=url(&path, &[("preview", "image")]) alt="" loading="lazy" /> }.into_any() }
                    else { view! { <span class="appearance-icon" inner_html=icons.file></span> }.into_any() }}
                </span><span class="right-panel__file-name">{name.clone()}</span><span class="right-panel__file-size">{size(length)}</span>
            </button>
            <div class="file-actions">
                <button type="button" class="k-icon-btn" node_ref=trigger aria-label=format!("File actions: {name}") title="More actions"
                    aria-haspopup="menu" aria-expanded=move || expanded.get().to_string() on:click=move |_| show_menu(false)
                    on:keydown=move |event: web_sys::KeyboardEvent| {
                        if matches!(event.key().as_str(), "ArrowDown" | "ArrowUp") { event.prevent_default(); show_menu(event.key() == "ArrowUp"); }
                    }><span class="appearance-icon" inner_html=icons.more></span></button>
                <div class="k-pop k-menu bs-root system-menu" role="menu" popover="auto" node_ref=menu
                    on:toggle=move |_| expanded.set(menu.get_untracked().is_some_and(|menu| menu.matches(":popover-open").unwrap_or(false)))
                    on:keydown=move |event: web_sys::KeyboardEvent| {
                        let key = event.key();
                        if key == "Escape" { event.prevent_default(); event.stop_propagation(); close_menu(); return; }
                        if !matches!(key.as_str(), "ArrowDown" | "ArrowUp" | "Home" | "End") { return; }
                        let Some(nodes) = menu.get_untracked().and_then(|menu| menu.query_selector_all("[role=menuitem]:not(:disabled)").ok()) else { return; };
                        let items = (0..nodes.length()).filter_map(|i| nodes.item(i)?.dyn_into::<web_sys::HtmlElement>().ok()).collect::<Vec<_>>();
                        let Some(index) = document().active_element().and_then(|active| items.iter().position(|item| item.is_same_node(Some(&active)))) else { return; };
                        event.prevent_default();
                        let next = match key.as_str() {
                            "Home" => 0, "End" => items.len() - 1,
                            "ArrowUp" => (index + items.len() - 1) % items.len(), _ => (index + 1) % items.len(),
                        };
                        let _ = items[next].focus();
                    }>
                    <button type="button" class="k-menu__item" role="menuitem" tabindex="-1" on:click=move |_| { close_menu(); open.run(preview.get_value()); }>"Open"</button>
                    <a class="k-menu__item" role="menuitem" tabindex="-1" href=path download=name.clone() on:click=move |_| close_menu()>"Download"</a>
                    <button type="button" class="k-menu__item is-danger" role="menuitem" tabindex="-1" disabled=move || chat.locked() on:click=move |_| { close_menu(); controls::dialog_open(confirm); }>"Delete"</button>
                    {audio.then(|| view! { <button type="button" class="k-menu__item" role="menuitem" tabindex="-1" on:click=move |_| { close_menu(); open.run(Preview { transcribe: true, ..preview.get_value() }); }>"Transcribe"</button> })}
                    <button type="button" class="k-menu__item" role="menuitem" tabindex="-1" disabled=move || chat.locked() || !attached() && chat.files.with(|ids| ids.len() >= MAX_FILES)
                        on:click=move |_| { close_menu(); attach(); }>{move || if attached() { "Remove from message" } else { "Attach to message" }}</button>
                </div>
            </div>
            <dialog class="k-dialog k-dialog--sm conversation-kit-dialog" node_ref=confirm role="alertdialog" aria-label=format!("Delete {name}?")>
                <div class="k-dialog__head"><div><h2 class="k-dialog__title">{format!("Delete {name}?")}</h2><p class="k-dialog__desc">"This removes the file from this conversation and storage."</p></div></div>
                <form method="dialog" class="k-dialog__foot"><button class="k-btn k-btn--md k-btn--secondary" autofocus>"Cancel"</button>
                    <button class="k-btn k-btn--md k-btn--danger" on:click=delete>"Delete file"</button></form>
            </dialog>
        </div>
    }
}

#[derive(Clone, PartialEq, Deserialize)]
struct Metadata {
    name: String,
    image: bool,
}

#[derive(Clone, PartialEq, Deserialize)]
struct TextPage {
    text: String,
    digest: Option<String>,
    next_offset: Option<u64>,
}

fn inline(file: Preview, close: Callback<()>) -> impl IntoView {
    let path = StoredValue::new(file.path.clone());
    let title = NodeRef::<leptos::html::Span>::new();
    request_animation_frame(move || {
        if let Some(title) = title.get_untracked() {
            let _ = title.focus();
        }
    });
    let metadata = RwSignal::new(None::<Result<Metadata, String>>);
    let refresh = RwSignal::new(0_u32);
    let workspace = file.path.contains("/workspace/file?");
    let extension = file.name.to_ascii_lowercase();
    let hint = Metadata {
        name: file.name.clone(),
        image: [".png", ".jpg", ".jpeg", ".webp"]
            .iter()
            .any(|suffix| extension.ends_with(suffix)),
    };
    if !workspace && !file.transcribe {
        watch(
            move || url(&path.get_value(), &[("metadata", "1")]),
            refresh,
            metadata,
        );
    }
    let preview = Memo::new(move |_| metadata.get().unwrap_or_else(|| Ok(hint.clone())));
    let original = file
        .path
        .replace("/workspace/file?", "/workspace/download?");
    let name = StoredValue::new(file.name);
    let label = move || {
        metadata.with(|value| {
            value
                .as_ref()
                .and_then(|value| value.as_ref().ok())
                .map_or_else(|| name.get_value(), |value| value.name.clone())
        })
    };
    let icons = use_context::<Icons>().unwrap_or_default();
    view! {
        <div><section class="k-section">
            <div class="k-section__head">
                <h2 class="k-section__title"><span class="right-panel__preview-head">
                    <button type="button" class="k-icon-btn" aria-label="Close file" title="Back to files" on:click=move |_| close.run(())>
                        <span class="appearance-icon" inner_html=icons.back></span></button>
                    <span node_ref=title tabindex="-1" class="right-panel__path">{label}</span>
                </span></h2>
                <div class="k-section__actions"><a href=original download=label class="k-icon-btn" aria-label="Download original"
                    title="Download original. Preview removes scripts, forms, links and external resources."><span class="appearance-icon" inner_html=icons.download></span></a></div>
            </div>
            {if file.transcribe {
                transcribe(path.get_value().replace("/panel/files/", "/files/")).into_any()
            } else {
                (move || match preview.get() {
                    Err(error) => retry(error, refresh).into_any(),
                    Ok(meta) if meta.image => image_preview(path.get_value(), meta.name).into_any(),
                    Ok(meta) if is_html(&meta.name) => html_preview(path.get_value(), meta.name).into_any(),
                    Ok(meta) => text_preview(path.get_value(), meta.name).into_any(),
                }).into_any()
            }}
        </section></div>
    }
}

fn image_preview(path: String, name: String) -> impl IntoView {
    let refresh = RwSignal::new(0_u32);
    let failed = RwSignal::new(false);
    let path = url(&path, &[("preview", "image")]);
    view! {
        {move || {
            refresh.track();
            failed.set(false);
            view! { <img class="right-panel__image" src=path.clone() alt=name.clone() on:error=move |_| failed.set(true) /> }
        }}
        {move || failed.get().then(|| retry("Image preview could not load. Download the original file.".into(), refresh))}
    }
}

fn html_preview(path: String, name: String) -> impl IntoView {
    let data = RwSignal::new(None::<Result<Value, String>>);
    let refresh = RwSignal::new(0_u32);
    let request = url(&path, &[("preview", "html")]);
    let render = url(&path, &[("preview", "render")]);
    watch(move || request.clone(), refresh, data);
    let label = format!("Preview {name}");
    view! { <div class="k-stack bs-root"><section class="k-stack" aria-label=label>
        {move || match data.get() {
            Some(Ok(_)) => view! { <iframe title=format!("Visual preview {name}") sandbox="" referrerpolicy="no-referrer"
                class="right-panel__preview conversation-html-preview" src=render.clone()></iframe> }.into_any(),
            Some(Err(message)) => retry(message, refresh).into_any(),
            None => view! { <p role="status">"Loading preview..."</p> }.into_any(),
        }}
    </section></div> }
}

fn text_preview(path: String, name: String) -> impl IntoView {
    let first = RwSignal::new(None::<Result<TextPage, String>>);
    let continuation = RwSignal::new(None::<TextPage>);
    let refresh = RwSignal::new(0_u32);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let endpoint = StoredValue::new(url(&path, &[("preview", "text")]));
    watch(move || endpoint.get_value(), refresh, first);
    Effect::new(move |_| {
        first.track();
        continuation.set(None);
        error.set(String::new());
    });
    let more = move |_| {
        if busy.get_untracked() {
            return;
        }
        let previous = continuation
            .get_untracked()
            .or_else(|| first.get_untracked().and_then(Result::ok));
        let Some(previous) = previous else {
            return;
        };
        let Some((offset, digest)) = previous.next_offset.zip(previous.digest.as_deref()) else {
            return;
        };
        let url = url(
            &endpoint.get_value(),
            &[("offset", &offset.to_string()), ("digest", digest)],
        );
        let current = first.get_untracked();
        error.set(String::new());
        busy.set(true);
        leptos::task::spawn_local(async move {
            let response = get::<TextPage>(&url).await;
            if busy.is_disposed() {
                return;
            }
            if first.get_untracked() == current {
                match response {
                    Ok(mut page) => {
                        page.text.insert_str(0, &previous.text);
                        continuation.set(Some(page));
                    }
                    Err(reason) => error.set(reason.message),
                }
            }
            busy.set(false);
        });
    };
    let markdown = [".md", ".mdown"]
        .iter()
        .any(|ext| name.to_ascii_lowercase().ends_with(ext));
    let pdf = name.to_ascii_lowercase().ends_with(".pdf");
    let scope = path.split("/files/").next().unwrap_or_default().to_owned();
    view! {
        {move || match first.get() {
            Some(Ok(page)) => {
                let text = continuation.get().unwrap_or(page).text;
                if markdown { super::body(text, scope.clone()).into_any() }
                else { view! {
                    {pdf.then(|| view! { <p class="k-field__hint">"Extracted PDF text. Layout, images and scanned pages are not included."</p> })}
                    <pre class="k-code conversation-preview-code" role="region" tabindex="0" aria-label=format!("Contents of {name}")>{if text.is_empty() { "(Empty file)".into() } else { text }}</pre>
                }.into_any() }
            }
            Some(Err(message)) => retry(message, refresh).into_any(),
            None => loading("Loading preview...").into_any(),
        }}
        <p class="message-error" role="alert" hidden=move || error.with(String::is_empty)>{error}</p>
        <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=busy on:click=more
            hidden=move || continuation.get().or_else(|| first.get().and_then(Result::ok)).is_none_or(|page| page.next_offset.is_none())>"Load more text"</button>
    }
}

fn transcribe(path: String) -> impl IntoView {
    let open = RwSignal::new(false);
    view! {
        <details class="k-collapsible conversation-disclosure" on:toggle=move |event| {
            if let Some(details) = event.target().and_then(|target| target.dyn_into::<web_sys::HtmlDetailsElement>().ok()) { open.set(details.open()); }
        }>
            <summary class="k-collapsible__trigger">"Transcribe"</summary>
            {move || open.get().then(|| transcription_controls(path.clone()))}
        </details>
    }
}

fn transcription_controls(path: String) -> impl IntoView {
    let data = RwSignal::new(None::<Result<Value, String>>);
    let refresh = RwSignal::new(0_u32);
    let busy = RwSignal::new(false);
    let consent = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let base = StoredValue::new(path);
    let endpoint = StoredValue::new(format!("{}?transcription=1", base.get_value()));
    watch(move || endpoint.get_value(), refresh, data);
    let command = move |cancel: bool| {
        if busy.get_untracked() {
            return;
        }
        let Some(Ok(info)) = data.get_untracked() else {
            return;
        };
        let operation = &info["operation"];
        let state = operation["status"].as_str();
        let payload = if cancel {
            let Some(id) = operation["id"]
                .as_str()
                .filter(|_| state == Some("pending"))
            else {
                return;
            };
            json!({"action":"cancel","id":id})
        } else {
            if !consent.get_untracked()
                || !info["format"].is_string()
                || !operation.is_null() && !matches!(state, Some("uncertain" | "cancelled"))
            {
                return;
            }
            let Some(provider) = info["provider"]["id"].as_str() else {
                return;
            };
            let Some(id) = uuid() else {
                return;
            };
            let mut payload = json!({"id":id,"provider":provider,"consent":true});
            if let Some(prior) = operation["id"].as_str() {
                payload["retryOf"] = json!(prior);
            }
            payload
        };
        let endpoint = endpoint.get_value();
        consent.set(false);
        error.set(String::new());
        busy.set(true);
        leptos::task::spawn_local(async move {
            match post::<Value>(&endpoint, &payload.to_string()).await {
                Ok(operation) => {
                    let mut info = info;
                    info["operation"] = operation;
                    data.try_set(Some(Ok(info)));
                }
                Err(_) => {
                    data.try_set(None);
                    error.try_set(if cancel { "Cancellation is unconfirmed. Reload status before trying again." }
                        else { "Delivery is unconfirmed. Reload status; do not resend until status is known." }.into());
                }
            }
            busy.try_set(false);
        });
    };
    let reload = move |_| {
        consent.set(false);
        error.set(String::new());
        refresh.update(|n| *n = n.wrapping_add(1));
    };
    view! { <section class="k-stack bs-root set4-transcription">
        <div class="set4-transcription__head"><span class="k-field__hint">"File transcription"</span>
            <button type="button" class="k-btn k-btn--sm k-btn--ghost" disabled=busy on:click=reload>"Reload transcription status"</button></div>
        <p class="message-error" role="alert" hidden=move || error.with(String::is_empty)>{error}</p>
        {move || match data.get() {
            None if error.with(String::is_empty) => loading("Loading transcription...").into_any(),
            None => ().into_any(),
            Some(Err(error)) => retry(error, refresh).into_any(),
            Some(Ok(value)) => {
                let operation = &value["operation"];
                let status = operation["status"].as_str();
                let waiting = status == Some("pending");
                let ready = value["provider"]["id"].is_string() && value["format"].is_string() && (operation.is_null() || matches!(status, Some("uncertain" | "cancelled")));
                let provider = value["provider"]["origin"].as_str().unwrap_or_default();
                let model = value["provider"]["model"].as_str().unwrap_or_default();
                let video = matches!(value["format"].as_str(), Some("mp4" | "webm"));
                let download = operation["result"]["id"].as_str().zip(operation["result"]["name"].as_str()).filter(|(id, _)| protocol::id(id)).and_then(|(id, name)| {
                    let path = base.get_value();
                    Some((format!("{}/{id}", path.rsplit_once('/')?.0), name.to_owned()))
                });
                let notice = match status {
                    Some("pending") => Some("Processing. Closing keeps transcription running."),
                    Some("cancelled") => Some("Cancelled in Minarbot. Provider may still process or bill this request. New attempts may incur another charge."),
                    Some("uncertain") => Some("The previous result is uncertain. Check the provider before retrying. A new request may incur another charge."),
                    _ => None,
                };
                view! {
                    {value["provider"].is_null().then(|| view! { <p class="k-field__hint">"Transcription unavailable. No provider configured."</p> })}
                    {value["format"].is_null().then(|| view! { <p class="k-field__hint">"Unsupported file. WAV, MP3, FLAC, OGG, WebM or M4A/MP4, up to 10 MiB."</p> })}
                    {video.then(|| view! { <p class="k-field__hint">"Whole file is sent, including any video. Provider must support its format and codec."</p> })}
                    {notice.map(|notice| view! { <p role="status">{notice}</p> })}
                    {waiting.then(|| view! { <button class="k-btn k-btn--sm k-btn--secondary" type="button" disabled=busy on:click=move |_| command(true)>"Cancel transcription"</button> })}
                    {download.map(|(href, name)| view! { <a class="k-link" href=href download=name>"Download transcript"</a> })}
                    {ready.then(|| view! {
                        <p class="k-field__hint">{format!("{provider} · {model} · Cost unknown")}</p>
                        <label class="k-inline"><input type="checkbox" disabled=busy prop:checked=consent on:change=move |event| consent.set(event_target_checked(&event)) />
                            {format!("Send this file to {provider}{}{}.", if video { " (including any video)" } else { "" },
                                if operation.is_null() { "" } else { " and accept a possible additional charge" })}
                        </label>
                        <button type="button" class="k-btn k-btn--sm k-btn--primary" disabled=move || busy.get() || !consent.get()
                            on:click=move |_| command(false)>"Transcribe"</button>
                    })}
                }.into_any()
            }
        }}
    </section> }
}

#[derive(Clone, Default, PartialEq)]
struct Location {
    path: String,
    file: bool,
    after: Option<String>,
    offset: Option<u64>,
    digest: Option<String>,
}

#[derive(Clone, PartialEq, Deserialize)]
struct WorkspacePage {
    #[serde(default)]
    entries: Vec<Entry>,
    next_after: Option<String>,
    text: Option<String>,
    digest: Option<String>,
    next_offset: Option<u64>,
}

#[derive(Clone, PartialEq, Deserialize)]
struct Entry {
    name: String,
    kind: String,
}

pub(super) fn workspace_page(chat: Chat, agent: String, name: String) -> impl IntoView {
    let close = Callback::new(move |()| {
        chat.workspace.set(false);
        request_animation_frame(|| {
            if let Some(node) = document()
                .query_selector(".chat__pill")
                .ok()
                .flatten()
                .and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
            {
                let _ = node.focus();
            }
        });
    });
    view! { <main class="chat page-view bs-root"><div class="page-scroll"><div class="page legacy">
        {workspace(format!("/api/agents/{agent}/workspace"), Some((name, close)), None)}
    </div></div></main> }
}

fn workspace(
    base: String,
    full: Option<(String, Callback<()>)>,
    open: Option<Callback<Preview>>,
) -> impl IntoView {
    let base = StoredValue::new(base);
    let history = RwSignal::new(vec![Location::default()]);
    let location =
        Memo::new(move |_| history.with(|items| items.last().cloned().unwrap_or_default()));
    let full_view = full.is_some();
    let result = RwSignal::new(None::<Result<WorkspacePage, String>>);
    let refresh = RwSignal::new(0_u32);
    let heading = NodeRef::<leptos::html::H1>::new();
    Effect::new(move |_| {
        location.track();
        if let Some(heading) = heading.get() {
            let _ = heading.focus();
        }
    });
    watch(
        move || {
            let Location {
                path,
                file,
                after,
                offset,
                digest,
            } = location.get();
            let mut values = vec![("path", path)];
            if let Some(after) = after {
                values.push(("after", after));
            }
            if let Some(offset) = offset {
                values.push(("offset", offset.to_string()));
            }
            if let Some(digest) = digest {
                values.push(("digest", digest));
            }
            let values: Vec<_> = values
                .iter()
                .map(|(key, value)| (*key, value.as_str()))
                .collect();
            url(
                &format!(
                    "{}/{}",
                    base.get_value(),
                    if file { "file" } else { "entries" }
                ),
                &values,
            )
        },
        refresh,
        result,
    );
    let icons = use_context::<Icons>().unwrap_or_default();
    let folder = icons.folder.clone();
    let entry_icons = StoredValue::new(icons);
    view! {
        <div class=if full_view { "k-page bs-root conversation-workspace-page" } else { "k-stack bs-root conversation-workspace" }>
            {full.map(|(name, close)| view! { <header class="k-page-head"><div class="k-page-head__text">
                <h1 node_ref=heading tabindex="-1">{format!("Workspace files for {name}")}</h1>
                <p>"Local files linked by the host owner. Previews are read-only; opening files here does not send their contents to the model."</p>
                </div><div class="k-page-head__actions"><button type="button" class="k-btn k-btn--md k-btn--ghost" on:click=move |_| close.run(())>"Back"</button></div></header> })}
            <nav class="k-row-actions" aria-label="Workspace path">
                {full_view.then(|| view! { <button type="button" class="k-btn k-btn--md k-btn--secondary" disabled=move || history.with(|items| items.len() < 2)
                    on:click=move |_| history.update(|items| { if items.len() > 1 { items.pop(); } })>
                    <span class="workspace-previous-icon" inner_html=entry_icons.with_value(|icons| icons.next.clone())></span>"Previous"</button> })}
                <button type="button" class="k-btn k-btn--md k-btn--ghost" on:click=move |_| history.set(vec![Location::default()])>
                    <span class="appearance-icon" inner_html=folder></span>{if full_view { "Root directory" } else { "Workspace" }}
                </button>
                {move || if full_view { ().into_any() } else {
                    let path = location.with(|item| item.path.clone());
                    let mut prefix = String::new();
                    path.split('/').filter(|part| !part.is_empty()).map(|part| {
                        if !prefix.is_empty() { prefix.push('/'); }
                        prefix.push_str(part);
                        let path = prefix.clone();
                        view! { <button type="button" class="k-btn k-btn--ghost k-btn--md"
                            on:click=move |_| history.set(vec![Location { path: path.clone(), ..Location::default() }])>{part.to_owned()}</button> }
                    }).collect_view().into_any()
                }}
            </nav>
            <section class="k-section">
                {full_view.then(|| view! { <div class="k-section__head"><h2 class="k-section__title">{move || location.with(|item| if item.path.is_empty() { "Root directory".into() } else { item.path.clone() })}</h2></div> })}
                {move || {
                    let current = location.get();
                    let download = current.file.then(|| {
                        let download = url(&format!("{}/download", base.get_value()), &[("path", &current.path)]);
                        let name = current.path.rsplit('/').next().unwrap_or_default().to_owned();
                        workspace_download(download, name, entry_icons)
                    });
                    let content = match result.get() {
                        None => loading("Loading workspace...").into_any(),
                        Some(Err(message)) => retry(message, refresh).into_any(),
                        Some(Ok(page)) if current.file => {
                            let next = page.next_offset.zip(page.digest).map(|(offset, digest)| Location { offset: Some(offset), digest: Some(digest), ..current.clone() });
                            let text = page.text.filter(|text| !text.is_empty()).unwrap_or_else(|| "(Empty file)".into());
                            view! {
                                <p class="k-field__hint">{format!("Starting at character {}. Maximum 8000 characters per page.", current.offset.unwrap_or(0) + 1)}</p>
                                <pre class="k-code conversation-workspace-code" role="region" tabindex="0" aria-label=format!("Contents of {}", current.path)>{text}</pre>
                                {next.map(|next| pager("Next text page", entry_icons, move || history.update(|items| items.push(next.clone()))))}
                            }.into_any()
                        }
                        Some(Ok(page)) => {
                            let icons = entry_icons.get_value();
                            let rows = page.entries.into_iter().filter(|entry| entry.name != ".git").map(|entry| {
                                let file = entry.kind == "file";
                                let directory = entry.kind == "directory";
                                let valid = (file || directory) && !entry.name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\'));
                                let path = if current.path.is_empty() { entry.name.clone() } else { format!("{}/{}", current.path, entry.name) };
                                let next = Location { path, file, ..Location::default() };
                                let name = entry.name;
                                let name_open = name.clone();
                                view! { <div class="k-row is-clickable"><button type="button" class="k-row__main" disabled=!valid on:click=move |_| {
                                    if let Some(open) = open.filter(|_| file) {
                                        open.run(Preview { path: url(&format!("{}/file", base.get_value()), &[("path", &next.path)]), name: name_open.clone(), transcribe: false });
                                    } else { history.update(|items| items.push(next.clone())); }
                                }><span class="k-row__leading" inner_html=if directory { icons.folder.clone() } else { icons.file.clone() }></span>
                                    <span class="k-row__text"><span class="k-row__title">{name}</span>
                                        {full_view.then(|| view! { <span class="k-row__meta">{if directory { "Directory" } else if file { "File" } else { "Unsupported" }}</span> })}
                                    </span>{full_view.then(|| view! { <span class="k-row__chev" aria-hidden="true" inner_html=icons.next.clone()></span> })}</button></div> }
                            }).collect::<Vec<_>>();
                            let next = page.next_after.map(|after| Location { after: Some(after), ..current.clone() });
                            view! {
                                {if rows.is_empty() { view! { <div class="k-empty"><span class="k-empty__icon" inner_html=icons.folder></span><div class="k-empty__title">"Directory is empty on this page."</div></div> }.into_any() }
                                else { view! { <div class="k-card">{rows}</div> }.into_any() }}
                                {next.map(|next| pager("Next entries", entry_icons, move || history.update(|items| items.push(next.clone()))))}
                            }.into_any()
                        }
                    };
                    (download, content)
                }}
            </section>
        </div>
    }
}

fn workspace_download(path: String, name: String, icons: StoredValue<Icons>) -> impl IntoView {
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    view! {
        <div class="k-row-actions">
            <button type="button" class="k-btn k-btn--secondary k-btn--md" disabled=busy aria-busy=move || busy.get().to_string() on:click=move |_| {
                if busy.get_untracked() { return; }
                let path = path.clone();
                let name = name.clone();
                busy.set(true);
                error.set(String::new());
                leptos::task::spawn_local(async move {
                    if let Err(reason) = download_workspace(&path, &name).await {
                        error.try_set(reason.as_string().unwrap_or_else(|| "Download failed. Try again.".into()));
                    }
                    busy.try_set(false);
                });
            }><span class="appearance-icon" inner_html=icons.with_value(|icons| icons.download.clone())></span>
                {move || if busy.get() { "Preparing download..." } else { "Download original file" }}
            </button>
        </div>
        <p class="k-field__hint">"Text or binary, up to 10 MiB. Downloads use the current file version."</p>
        <p class="message-error" role="alert" hidden=move || error.with(String::is_empty)>{error}</p>
    }
}

fn pager(
    label: &'static str,
    icons: StoredValue<Icons>,
    click: impl Fn() + 'static,
) -> impl IntoView {
    view! { <div class="k-pager"><button type="button" class="k-icon-btn" aria-label=label title=label on:click=move |_| click()>
    <span class="appearance-icon" inner_html=icons.with_value(|icons| icons.next.clone())></span></button></div> }
}

pub(super) fn back(chat: Chat, label: &'static str) -> impl IntoView {
    let icon = use_context::<Icons>().unwrap_or_default().back;
    view! { <button type="button" class="k-icon-btn" aria-label=label title=label on:click=move |_| chat.select_tab("files")>
    <span class="appearance-icon" inner_html=icon></span></button> }
}

pub(super) fn artifacts(chat: Chat, context: Context) -> impl IntoView {
    let base = format!("{}/panel", chat.base());
    let items = context
        .previews
        .into_iter()
        .filter(|file| protocol::id(&file.id) && is_html(&file.name))
        .map(|file| {
            (
                file.id.clone(),
                file.name,
                format!("{base}/files/{}", file.id),
                None,
            )
        })
        .chain(
            context
                .sites
                .into_iter()
                .filter(|site| protocol::id(&site.id))
                .map(|site| {
                    (
                        site.id.clone(),
                        site.name,
                        format!("{base}/sites/{}", site.id),
                        Some(site.revision),
                    )
                }),
        )
        .collect::<Vec<_>>();
    let items = StoredValue::new(items);
    let selected = RwSignal::new(String::new());
    let current = Memo::new(move |_| {
        items.with_value(|items| {
            items
                .iter()
                .find(|item| item.0 == selected.get())
                .or_else(|| items.first())
                .cloned()
        })
    });
    let download = use_context::<Icons>().unwrap_or_default().download;
    view! { <div class="right-panel__body k-stack">
        <div class="right-panel__preview-head">{back(chat, "Back to files")}
            <span class="right-panel__path">{move || current.with(|item| item.as_ref().map_or_else(|| "Preview".into(), |item| item.1.clone()))}</span>
            {move || current.get().map(|(_, name, path, _)| view! { <a class="k-icon-btn" aria-label="Download original" href=path download=name><span class="appearance-icon" inner_html=download.clone()></span></a> })}
        </div>
        {items.with_value(|items| (items.len() > 1).then(|| view! {
            <select class="k-input k-select conversation-select" aria-label="Preview artifact" on:change=move |event| selected.set(event_target_value(&event))>
                {items.iter().map(|(id, name, _, _)| view! { <option value=id.clone()>{name.clone()}</option> }).collect_view()}
            </select>
        }))}
        {move || match current.get() {
            Some((_, name, path, Some(_))) => view! { <iframe class="right-panel__preview" title=format!("Visual preview {name}") sandbox="" referrerpolicy="no-referrer" src=path></iframe> }.into_any(),
            Some((_, name, path, None)) => html_preview(path, name).into_any(),
            None => empty("No HTML artifacts yet").into_any(),
        }}
    </div> }
}
