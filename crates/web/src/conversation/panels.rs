use super::*;
use serde_json::json;

#[derive(Clone, Deserialize)]
struct ComponentReply {
    tree: Tree,
    data: std::collections::HashMap<String, Value>,
    frame: Option<Reply>,
}
#[derive(Clone, Deserialize)]
struct Tree {
    children: Vec<ComponentNode>,
}
#[derive(Clone, Deserialize)]
struct ComponentNode {
    #[serde(rename = "type")]
    kind: String,
    title: String,
    data: String,
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = MinarbotConversation, js_name = mountFrame)]
    fn mount_frame(
        wrapper: &web_sys::HtmlElement,
        scope: &str,
        reference: &str,
        mcp: bool,
        action: &js_sys::Function,
    ) -> js_sys::Function;

    #[wasm_bindgen(js_namespace = MinarbotConversation, js_name = mountVnc)]
    fn mount_vnc(
        target: &web_sys::HtmlElement,
        agent: &str,
        revision: u64,
        control: bool,
        status: &js_sys::Function,
    ) -> js_sys::Function;

    #[wasm_bindgen(js_namespace = MinarbotConversation, catch, js_name = readClipboard)]
    async fn read_clipboard() -> Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>;

    #[wasm_bindgen(js_namespace = MinarbotConversation, catch, js_name = writeClipboard)]
    async fn write_clipboard(text: &str) -> Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>;
}

type Mount = (js_sys::Function, Closure<dyn FnMut(String)>);

fn dom_mount(
    node: NodeRef<leptos::html::Div>,
    mount: impl Fn(web_sys::HtmlElement) -> Option<Mount> + 'static,
) {
    let handle = StoredValue::new_local(None::<Mount>);
    Effect::new(move |_| {
        let next = node.get().and_then(|node| {
            handle.update_value(|slot| {
                if let Some((dispose, _)) = slot.take() {
                    let _ = dispose.call0(&wasm_bindgen::JsValue::NULL);
                }
            });
            mount(node.unchecked_into())
        });
        handle.set_value(next);
    });
    on_cleanup(move || {
        handle.update_value(|slot| {
            if let Some((dispose, _)) = slot.take() {
                let _ = dispose.call0(&wasm_bindgen::JsValue::NULL);
            }
        })
    });
}

fn rich_frame(chat: Chat, reply: Reply, mcp: bool) -> impl IntoView {
    let reference =
        json!({"id": reply.id, "title": reply.title, "height": reply.height.unwrap_or(240)})
            .to_string();
    let scope = chat.base();
    let node = NodeRef::<leptos::html::Div>::new();
    dom_mount(node, move |node| {
        let callback = Closure::<dyn FnMut(String)>::new(move |raw: String| {
            if chat.locked() {
                return;
            }
            let Ok(value) = serde_json::from_str::<Value>(&raw) else {
                return;
            };
            match value["kind"].as_str() {
                Some("message") => {
                    if let Some(text) = value["text"].as_str() {
                        chat.send_from_app(text);
                    }
                }
                Some("draft") => {
                    if let Some(text) = value["text"]
                        .as_str()
                        .filter(|text| !text.trim().is_empty())
                    {
                        let old = chat.draft.get_untracked();
                        let text = if old.trim().is_empty() {
                            text.to_owned()
                        } else {
                            format!("{old}\n\n{text}")
                        };
                        if text.encode_utf16().count() <= MAX_MESSAGE {
                            chat.draft(text);
                        } else {
                            chat.error.set(
                                "Free up space in your draft before adding this reply.".into(),
                            );
                        }
                    }
                }
                Some("context") => {
                    let context = if value["context"].is_object() {
                        value["context"].clone()
                    } else {
                        json!({"renderId": value["id"], "data": value["data"]})
                    };
                    if context["data"] == "" {
                        chat.context.set(None);
                    } else if let Ok(context) = serde_json::from_value::<ReplyContext>(context) {
                        if context.valid() {
                            chat.context.set(Some(context));
                        }
                    }
                }
                _ => {}
            }
        });
        let dispose = mount_frame(
            &node,
            &scope,
            &reference,
            mcp,
            callback.as_ref().unchecked_ref(),
        );
        Some((dispose, callback))
    });
    view! { <div node_ref=node></div> }
}

fn component(chat: Chat, value: Value) -> impl IntoView {
    let Ok(reply) = serde_json::from_value::<ComponentReply>(value) else {
        return view! { <span /> }.into_any();
    };
    view! {
        <div class="k-card component-reply">
            {reply.tree.children.into_iter().map(|node| {
                let title = node.title.clone();
                let data = reply.data.get(&node.data).cloned().unwrap_or_default();
                view! {
                    <section class="component-node">
                        <h3 class="k-section__title">{title}</h3>
                        {if node.kind == "mermaid" {
                            reply.frame.clone().map(|frame| rich_frame(chat, frame, false)).into_any()
                        } else { component_data(node.kind, node.title, data) }}
                    </section>
                }
            }).collect_view()}
        </div>
    }
    .into_any()
}

fn component_data(kind: String, title: String, data: Value) -> AnyView {
    match kind.as_str() {
        "metric" => view! { <strong class="component-metric">{data["value"].as_str().map(str::to_owned).unwrap_or_else(|| data["value"].to_string())}</strong><p class="component-detail">{data["detail"].as_str().map(str::to_owned)}</p> }.into_any(),
        "table" => view! {
            <div class="component-table" role="region" aria-label=title.clone() tabindex="0"><table><caption>{title.clone()}</caption>
                <thead><tr>{data["columns"].as_array().into_iter().flatten().filter_map(|value| value.as_str()).map(|value| view! { <th scope="col">{value.to_owned()}</th> }).collect_view()}</tr></thead>
                <tbody>{data["rows"].as_array().into_iter().flatten().map(|row| view! { <tr>{row.as_array().into_iter().flatten().map(|value| {
                    let text = value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string());
                    view! { <td>{text}</td> }
                }).collect_view()}</tr> }).collect_view()}</tbody>
            </table></div>
        }.into_any(),
        "timeline" => view! { <ol class="component-timeline" role="list">{data.as_array().into_iter().flatten().map(|value| view! {
            <li><strong>{value["label"].as_str().map(str::to_owned)}</strong><span>{value["when"].as_str().map(str::to_owned)}</span><p class="component-detail">{value["detail"].as_str().map(str::to_owned)}</p></li>
        }).collect_view()}</ol> }.into_any(),
        "bar" | "line" | "pie" => serde_json::from_value::<Vec<Point>>(data).ok().map(|points| chart(&kind, title, points)).into_any(),
        _ => ().into_any(),
    }
}

fn rich_reply(chat: Chat, row: RwSignal<Turn>) -> impl IntoView {
    let html = Memo::new(move |_| row.with(|turn| turn.html.clone()));
    let apps = Memo::new(move |_| row.with(|turn| turn.apps.clone()));
    let components = Memo::new(move |_| row.with(|turn| turn.components.clone()));
    view! {
        <For each=move || html.get().into_iter().take(4) key=|reply| reply.id.clone()
            children=move |reply| rich_frame(chat, reply, false) />
        <For each=move || apps.get().into_iter().take(4) key=|reply| reply.id.clone()
            children=move |reply| rich_frame(chat, reply, true) />
        {move || components.get().into_iter().map(|value| component(chat, value)).collect_view()}
    }
}

pub fn exchanges(chat: Chat, row: RwSignal<Turn>) -> impl IntoView {
    let exchanges = Memo::new(move |_| {
        row.with(|turn| {
            let mut values = turn.exchanges.clone();
            for reply in &turn.received {
                if !values.iter().any(|value| value["id"] == reply["id"]) {
                    values.push(reply.clone());
                }
            }
            values
        })
    });
    let discussion = Memo::new(move |_| {
        row.with(|turn| {
            turn.room
                .clone()
                .filter(|room| room["root"].as_str() == Some(turn.id.as_str()))
        })
    });
    view! {
        <section class="inline-delegation" aria-label="Agent conversation" hidden=move || exchanges.with(Vec::is_empty)>
            {move || exchanges.get().into_iter().map(|value| {
                let name = value["name"].as_str().unwrap_or("Agent").to_owned();
                let text = value["reply"].as_str().or_else(|| value["error"].as_str()).unwrap_or_default().to_owned();
                let href = value["agent"].as_str().zip(value["thread"].as_str()).filter(|(agent, thread)| protocol::id(agent) && protocol::id(thread)).map(|(agent, thread)| routes::link(&format!("/agents/{agent}/conversations/{thread}")));
                view! { <details class="inline-delegation__exchange"><summary>{name}</summary>{body(text, chat.base())}{href.map(|href| view! { <a class="k-link" href=href>"Open conversation"</a> })}</details> }
            }).collect_view()}
        </section>
        {move || discussion.with(|room| room.as_ref().map(|room| view! {
            <section class="room-conversation" aria-label="Section conversation">
                {room["messages"].as_array().into_iter().flatten().filter(|entry| entry["kind"] == "request").filter_map(|entry| {
                    let text = entry["text"].as_str()?.to_owned();
                    let name = entry["name"].as_str().unwrap_or("Agent").to_owned();
                    let target = entry["target_name"].as_str().map(|name| format!("To {name}"));
                    Some(view! { <div class="room-message-request"><strong>{name}</strong><span>{target}</span>{body(text, chat.base())}</div> })
                }).collect_view()}
                {room["note"].as_str().map(|note| view! { <p class="room-system-note" role="status">{note.to_owned()}</p> })}
            </section>
        }))}
    }
}

#[derive(Clone, Deserialize)]
struct Context {
    computer: Option<String>,
    workspace: Option<Workspace>,
}
#[derive(Clone, Deserialize)]
struct Workspace {
    name: String,
}

fn files_panel(chat: Chat, context: RwSignal<Option<Context>>) -> impl IntoView {
    let files = RwSignal::new(None::<Vec<FileRef>>);
    let selected = RwSignal::new(None::<FileRef>);
    let error = RwSignal::new(String::new());
    let path = format!("{}/panel/files", chat.base());
    leptos::task::spawn_local(async move {
        match get::<Vec<FileRef>>(&path).await {
            Ok(value) => {
                files.try_set(Some(value));
            }
            Err(reason) => {
                error.try_set(reason.message);
            }
        }
    });
    view! {
        <section aria-label="Conversation files" class="k-stack">
            <h2 class="k-section__title">"Conversation files"</h2>
            <p role="status" hidden=move || files.with(Option::is_some) || !error.with(String::is_empty)>"Loading files…"</p>
            <p role="status" hidden=move || files.with(|files| files.as_ref().is_none_or(|files| !files.is_empty()))>"No files yet"</p>
            <p role="alert" class="message-error" hidden=move || error.with(String::is_empty)>{error}</p>
            <div class="attachments__list" hidden=move || selected.with(Option::is_some)>
                {move || files.with(|files| files.iter().flatten().filter(|file| protocol::id(&file.id) && !file.deleted).cloned().map(|file| {
                    let id = file.id.clone();
                    let label_id = id.clone();
                    let label = file.name.clone();
                    view! {
                        <div class="right-panel__file-row">
                            <button type="button" class="right-panel__file" aria-label=format!("Preview {label}") on:click=move |_| selected.set(Some(file.clone()))>{label.clone()}</button>
                            <button type="button" class="k-btn k-btn--sm k-btn--ghost" disabled=move || chat.locked()
                                on:click=move |_| {
                                    let mut attached = chat.files.get_untracked();
                                    if attached.contains(&id) { attached.retain(|value| value != &id); } else if attached.len() < MAX_FILES { attached.push(id.clone()); }
                                    chat.attach(attached);
                                }>{move || if chat.files.with(|files| files.contains(&label_id)) { "Remove from message" } else { "Attach to message" }}</button>
                        </div>
                    }
                }).collect_view())}
            </div>
            {move || selected.get().map(|file| {
                let path = format!("{}/panel/files/{}", chat.base(), file.id);
                view! {
                    <div class="k-row-actions"><button type="button" class="k-btn k-btn--sm k-btn--secondary" on:click=move |_| selected.set(None)>"Close file"</button>
                        <a class="k-link" href=path.clone() download=file.name.clone()>"Download original"</a>
                    </div>
                    {if file.image { view! { <img class="right-panel__image" src=format!("{path}?preview=image") alt=file.name.clone() /> }.into_any() } else if file.name.ends_with(".html") {
                        view! { <iframe class="html-reply" sandbox="" referrerpolicy="no-referrer" title=format!("Visual preview {}", file.name) src=format!("{path}?preview=render")></iframe> }.into_any()
                    } else { text_preview(path, file.name).into_any() }}
                }
            })}
            {move || context.with(|value| value.as_ref().and_then(|value| value.workspace.as_ref()).map(|workspace| view! { <p class="k-field__hint">{workspace.name.clone()}</p> }))}
        </section>
    }
}

fn text_preview(path: String, name: String) -> impl IntoView {
    let content = RwSignal::new(None::<String>);
    let error = RwSignal::new(String::new());
    leptos::task::spawn_local(async move {
        match get::<Value>(&format!("{path}?preview=text")).await {
            Ok(value) => {
                content.try_set(value["text"].as_str().map(str::to_owned));
            }
            Err(reason) => {
                error.try_set(reason.message);
            }
        }
    });
    view! {
        <p role="status" hidden=move || content.with(Option::is_some) || !error.with(String::is_empty)>"Loading preview…"</p>
        <p role="alert" class="message-error" hidden=move || error.with(String::is_empty)>{error}</p>
        <pre tabindex="0" hidden=move || content.with(Option::is_none) aria-label=format!("Contents of {name}")>{content}</pre>
    }
}

fn run_panel(chat: Chat) -> impl IntoView {
    let data = RwSignal::new(None::<Value>);
    let error = RwSignal::new(String::new());
    fn refresh(chat: Chat, data: RwSignal<Option<Value>>, error: RwSignal<String>, id: String) {
        if data.is_disposed() || chat.run.get_untracked() != id {
            return;
        }
        leptos::task::spawn_local(async move {
            let result = get(&format!("/api/turns/{id}/inspect")).await;
            if data.is_disposed() || chat.run.get_untracked() != id {
                return;
            }
            match result {
                Ok(value) => {
                    if data.with_untracked(|previous| previous.as_ref() != Some(&value)) {
                        data.set(Some(value));
                    }
                    error.set(String::new());
                }
                Err(reason) => error.set(reason.message),
            }
            set_timeout(
                move || refresh(chat, data, error, id),
                Duration::from_millis(1200),
            );
        });
    }
    Effect::new(move |_| {
        let id = chat.run.get();
        if protocol::id(&id) {
            data.set(None);
            refresh(chat, data, error, id);
        }
    });
    view! {
        <section class="run-inspector" aria-label="Run inspector">
            <h2 class="k-section__title">"Run inspector"</h2>
            <p role="status" hidden=move || data.with(Option::is_some) || !error.with(String::is_empty)>"Loading run…"</p>
            <p role="alert" class="message-error" hidden=move || error.with(String::is_empty)>{error}</p>
            {move || data.with(|data| data.as_ref().map(|run| {
                let stopped = run["status"] == "cancelled";
                let error = run["error"].as_str().map(str::to_owned);
                view! {
                    {error.map(|error| view! {
                        <div class="run-inspector__value"><strong>{if stopped { "Run stopped" } else { "Run error" }}</strong><pre>{error}</pre></div>
                    })}
                    {["judgments", "tools", "harness", "providers"].into_iter().filter_map(|key| {
                        let list = run[key].as_array()?;
                        if list.is_empty() { return None }
                        let title = match key { "judgments" => "Goal judge", "providers" => "Provider calls", "tools" => "Tool steps", _ => "Harness steps" };
                        let rows = list.iter().map(|step| {
                            let header = if key == "judgments" { format!("Turn {}", step["number"]) } else {
                                step["title"].as_str().or_else(|| step["name"].as_str()).or_else(|| step["model"].as_str()).unwrap_or(title).to_owned()
                            };
                            let status = if key == "judgments" {
                                if step["met"] == 1 || step["met"] == true { "Met" } else { "More work" }
                            } else {
                                match step["state"].as_str().or_else(|| step["status"].as_str()).unwrap_or_default() {
                                    "pending" => "Pending", "running" => "Running", "received" | "done" | "completed" => "Completed",
                                    "failed" => "Failed", "interrupted" if stopped => "Stopped", "interrupted" => "Interrupted", _ => "Waiting",
                                }
                            };
                            view! { <li><details><summary><span>{header}</span><span class="run-inspector__state">{status}</span></summary>
                                <div class="run-inspector__details">
                                    {["arguments", "result", "reason", "created", "finished"].into_iter().filter_map(|key| step[key].as_str().map(|text|
                                        view! { <pre>{text.to_owned()}</pre> })).collect_view()}
                                </div></details></li> }
                        }).collect_view();
                        Some(view! { <section class="run-inspector__providers" aria-label=title><h3>{title}</h3><ul class="run-inspector__list">{rows}</ul></section> })
                    }).collect_view()}
                    {run["engineStderr"].as_str().map(|text| view! { <details><summary>"Engine stderr"</summary><pre>{text.to_owned()}</pre></details> })}
                }
            }))}
        </section>
    }
}

#[derive(Clone, PartialEq, Deserialize)]
struct ComputerStatus {
    available: bool,
    enabled: bool,
    state: String,
    #[serde(default)]
    control: Control,
}
#[derive(Clone, PartialEq, Deserialize, Default)]
struct Control {
    mode: String,
    revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComputerCommand {
    id: String,
    revision: u64,
    operation: String,
    turn: Option<String>,
}

fn valid_computer_command(raw: &str) -> bool {
    serde_json::from_str::<ComputerCommand>(raw).is_ok_and(|command| {
        protocol::id(&command.id)
            && command.revision <= 9_007_199_254_740_991
            && match command.operation.as_str() {
                "take" | "hand-back" => command.turn.is_none(),
                "dismiss" => command.turn.as_deref().is_some_and(protocol::id),
                _ => false,
            }
    })
}

fn computer_panel(chat: Chat, agent: String) -> impl IntoView {
    let path = StoredValue::new(format!("/api/agents/{agent}/computer"));
    let key = StoredValue::new(format!("minarbot-computer-control:{agent}"));
    let pending = RwSignal::new(saved(&key.get_value()));
    let status = RwSignal::new(None::<ComputerStatus>);
    let clipboard = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let connection = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let screen = NodeRef::<leptos::html::Div>::new();
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let stop_dialog = NodeRef::<leptos::html::Dialog>::new();
    let recover_dialog = NodeRef::<leptos::html::Dialog>::new();
    let hand_back = NodeRef::<leptos::html::Button>::new();
    let reconnect = RwSignal::new(0_u32);
    let visible = RwSignal::new(!document().hidden());
    let locked = move || busy.get() || pending.with(Option::is_some);
    let listener = window_event_listener(leptos::ev::visibilitychange, move |_| {
        visible.set(!document().hidden())
    });
    on_cleanup(move || listener.remove());

    fn refresh(
        path: StoredValue<String>,
        status: RwSignal<Option<ComputerStatus>>,
        busy: RwSignal<bool>,
        error: RwSignal<String>,
    ) {
        if status.is_disposed() {
            return;
        }
        let endpoint = path.get_value();
        leptos::task::spawn_local(async move {
            let result = get::<ComputerStatus>(&endpoint).await;
            if status.is_disposed() {
                return;
            }
            match result {
                Ok(next) => {
                    if !busy.get_untracked()
                        && status.with_untracked(|current| {
                            current.as_ref().is_none_or(|current| {
                                next.control.revision >= current.control.revision
                                    && *current != next
                            })
                        })
                    {
                        status.set(Some(next));
                    }
                }
                Err(reason) => error.set(reason.message),
            }
            set_timeout(
                move || refresh(path, status, busy, error),
                Duration::from_secs(1),
            );
        });
    }
    refresh(path, status, busy, error);
    let user =
        move || status.with(|status| status.as_ref().is_some_and(|s| s.control.mode == "user"));
    let running = move || {
        status.with(|status| {
            status
                .as_ref()
                .is_some_and(|s| matches!(s.state.as_str(), "running" | "idle"))
        })
    };
    dom_mount(screen, move |node| {
        reconnect.track();
        clipboard.set(String::new());
        if !running() || !visible.get() || locked() {
            return None;
        }
        let revision = status.with(|s| s.as_ref().map_or(0, |s| s.control.revision));
        let callback = Closure::<dyn FnMut(String)>::new(move |raw: String| {
            if let Ok(value) = serde_json::from_str::<Value>(&raw) {
                if let Some(text) = value["clipboard"].as_str() {
                    clipboard.set(text.into());
                }
                if let Some(text) = value["status"].as_str() {
                    connection.set(text.into());
                }
            }
        });
        let dispose = mount_vnc(
            &node,
            &agent,
            revision,
            user(),
            callback.as_ref().unchecked_ref(),
        );
        Some((dispose, callback))
    });
    let control = move |operation: Option<&str>| {
        if busy.get_untracked() {
            return;
        }
        let key = key.get_value();
        let raw = match (operation, pending.get_untracked()) {
            (None, Some(raw)) if valid_computer_command(&raw) => raw,
            (Some(operation), None) => {
                let Some(revision) =
                    status.with_untracked(|s| s.as_ref().map(|s| s.control.revision))
                else {
                    return;
                };
                let Some(id) = uuid() else { return };
                json!({"id": id, "revision": revision, "operation": operation}).to_string()
            }
            _ => return,
        };
        if !save(&key, &raw) {
            error.set("Computer request could not be saved.".into());
            return;
        }
        pending.set(Some(raw.clone()));
        busy.set(true);
        error.set(String::new());
        let endpoint = format!("{}/control", path.get_value());
        leptos::task::spawn_local(async move {
            match post::<ComputerStatus>(&endpoint, &raw).await {
                Ok(value) => {
                    status.try_set(Some(value));
                    if remove(&key) {
                        pending.try_set(None);
                    } else {
                        error.try_set(
                            "Computer control changed. Recovery data could not be cleared.".into(),
                        );
                    }
                }
                Err(reason) => {
                    error.try_set(reason.message);
                }
            }
            busy.try_set(false);
        });
    };
    let command = move |operation: &str| {
        if locked() {
            return;
        }
        let path = format!("{}/{operation}", path.get_value());
        busy.set(true);
        leptos::task::spawn_local(async move {
            let result = post::<ComputerStatus>(&path, "{}").await;
            if busy.is_disposed() {
                return;
            }
            match result {
                Ok(value) => status.set(Some(value)),
                Err(reason) => error.set(reason.message),
            }
            busy.set(false);
        });
    };
    let paste = move || {
        if !running() || !user() || locked() {
            return;
        }
        let revision = status.with_untracked(|s| s.as_ref().map(|s| s.control.revision));
        leptos::task::spawn_local(async move {
            let result = read_clipboard().await;
            if busy.is_disposed()
                || locked()
                || !running()
                || !user()
                || status.with_untracked(|s| s.as_ref().map(|s| s.control.revision)) != revision
            {
                return;
            }
            let Some(text) = result.ok().and_then(|value| value.as_string()) else {
                error.set("Clipboard could not be read.".into());
                return;
            };
            if text.encode_utf16().count() > 4000 {
                error.set("Clipboard text exceeds 4000 characters.".into());
                return;
            }
            if text.is_empty() {
                return;
            }
            let body =
                json!({"revision": revision, "name": "computer_type", "args": {"text": text}});
            if let Err(reason) =
                post::<Value>(&format!("{}/input", path.get_value()), &body.to_string()).await
            {
                error.try_set(reason.message);
            }
        });
    };
    view! {
        <section class="computer-viewer" aria-label="Computer">
            <h2 class="computer-viewer__title">{chat.config.with_value(|config| format!("{}’s computer", config.name))}</h2>
            <p role="status">{move || status.with(|status| status.as_ref().map(|s| {
                if !s.available { "Unavailable".into() } else if !s.enabled { "No computer assigned.".into() } else { s.state.clone() }
            }).unwrap_or_else(|| "Loading computer status…".into()))}</p>
            <div class="computer-viewer__screen" role="application" aria-label="Agent computer. Shift+Escape returns to controls."
                hidden=move || !running() on:keydown:capture=move |event| {
                if user() && event.shift_key() && event.key() == "Escape" {
                    event.prevent_default(); event.stop_propagation();
                    if let Some(button) = hand_back.get_untracked() { let _ = button.focus(); }
                }
                if user() && (event.ctrl_key() || event.meta_key()) && event.key().eq_ignore_ascii_case("v") {
                    event.prevent_default(); event.stop_propagation(); paste();
                }
            }><div class="computer-viewer__canvas" node_ref=screen aria-label="Agent computer screen"></div></div>
            <p role="status" hidden=move || !running()>{connection}</p>
            <div class="k-row-actions">
                <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=locked
                    hidden=move || running() || status.with(|status| status.as_ref().is_none_or(|s| !s.enabled || !s.available || s.state == "starting"))
                    on:click=move |_| command("start")>"Start computer"</button>
                <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=locked hidden=move || !running()
                    on:click=move |_| { if let Some(dialog) = stop_dialog.get_untracked() { let _ = dialog.show_modal(); } }>"Stop computer"</button>
                <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=locked hidden=move || !running()
                    on:click=move |_| reconnect.update(|count| *count = count.wrapping_add(1))>"Reconnect"</button>
                <button type="button" class="k-btn k-btn--sm k-btn--primary" disabled=locked hidden=move || !running() || user()
                    on:click=move |_| { if let Some(dialog) = dialog.get_untracked() { let _ = dialog.show_modal(); } }>"Take control"</button>
                <button type="button" class="k-btn k-btn--sm k-btn--primary" node_ref=hand_back disabled=locked hidden=move || !user()
                    on:click=move |_| control(Some("hand-back"))>"Hand back to agent"</button>
                <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=locked hidden=move || !user() on:click=move |_| paste()>"Paste clipboard"</button>
                <button type="button" class="k-btn k-btn--sm k-btn--secondary" hidden=move || !user() || clipboard.with(String::is_empty)
                    on:click=move |_| {
                        let text = clipboard.get_untracked();
                        leptos::task::spawn_local(async move { if write_clipboard(&text).await.is_err() { error.try_set("Clipboard could not be written.".into()); } });
                    }>"Copy clipboard"</button>
            </div>
            <div class="k-row-actions" hidden=move || pending.with(Option::is_none)>
                <p role="status">"Control change not confirmed"</p>
                <button type="button" class="k-btn k-btn--sm k-btn--primary" disabled=busy
                    hidden=move || !pending.with(|raw| raw.as_deref().is_some_and(valid_computer_command))
                    on:click=move |_| control(None)>"Retry saved request"</button>
                <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=busy
                    on:click=move |_| { if let Some(dialog) = recover_dialog.get_untracked() { let _ = dialog.show_modal(); } }>"Load current control"</button>
            </div>
            <p class="message-error" role="alert" hidden=move || error.with(String::is_empty)>{error}</p>
            <dialog class="conversation-dialog" node_ref=recover_dialog aria-label="Load current control?">
                <p>"Accepted changes still apply. Check current control before taking another action."</p>
                <form method="dialog" class="k-row-actions">
                    <button class="k-btn k-btn--sm k-btn--secondary">"Cancel"</button>
                    <button class="k-btn k-btn--sm k-btn--primary" on:click=move |_| {
                        if busy.get_untracked() { return }
                        busy.set(true);
                        let endpoint = path.get_value();
                        let key = key.get_value();
                        leptos::task::spawn_local(async move {
                            match get::<ComputerStatus>(&endpoint).await {
                                Ok(value) if remove(&key) => {
                                    status.try_set(Some(value)); pending.try_set(None); error.try_set(String::new());
                                }
                                Ok(_) => { error.try_set("Computer recovery data could not be cleared.".into()); }
                                Err(reason) => { error.try_set(reason.message); }
                            }
                            busy.try_set(false);
                        });
                    }>"Load current control"</button>
                </form>
            </dialog>
            <dialog class="conversation-dialog" node_ref=stop_dialog aria-label="Stop computer?">
                <p>"Unsaved work in the desktop may be lost."</p>
                <form method="dialog" class="k-row-actions">
                    <button class="k-btn k-btn--sm k-btn--secondary">"Cancel"</button>
                    <button class="k-btn k-btn--sm k-btn--primary" on:click=move |_| command("stop")>"Stop computer"</button>
                </form>
            </dialog>
            <dialog class="conversation-dialog" node_ref=dialog aria-label="Take control?">
                <p>"Computer tools pause until you hand control back."</p>
                <form method="dialog" class="k-row-actions">
                    <button class="k-btn k-btn--sm k-btn--secondary">"Cancel"</button>
                    <button class="k-btn k-btn--sm k-btn--primary" on:click=move |_| control(Some("take"))>"Take control"</button>
                </form>
            </dialog>
        </section>
    }
}

pub fn right_panel(chat: Chat) -> impl IntoView {
    let context = RwSignal::new(None::<Context>);
    let computer_available = RwSignal::new(false);
    Effect::new(move |_| {
        if !chat.details.get() {
            return;
        }
        let path = format!("{}/panel", chat.base());
        leptos::task::spawn_local(async move {
            match get::<Context>(&path).await {
                Ok(value) => {
                    let agent = value.computer.clone();
                    context.try_set(Some(value));
                    let available = if let Some(agent) = agent.filter(|agent| protocol::id(agent)) {
                        get::<ComputerStatus>(&format!("/api/agents/{agent}/computer"))
                            .await
                            .is_ok_and(|status| status.available)
                    } else {
                        false
                    };
                    computer_available.try_set(available);
                }
                Err(reason) => {
                    chat.error.try_set(reason.message);
                }
            }
        });
    });
    let icons = use_context::<Icons>().unwrap_or_default();
    let channel = chat.config.with_value(|c| c.channel);
    let tabs = [
        (
            "chats",
            if channel { "Members" } else { "Chats" },
            if channel { icons.members } else { icons.chats },
        ),
        ("files", "Files", icons.files),
        ("computer", "Computer", icons.computer),
        ("run", "Run", icons.inspect.clone()),
    ];
    view! {
        <aside class="details right-panel conversation-panel bs-root" aria-label="Conversation details" hidden=move || !chat.details.get()>
            <div class="right-panel__header"><div>
                <button type="button" class="k-icon-btn" aria-label="Close details" title="Close details" on:click=move |_| chat.set_details(false)>
                    <span class="appearance-icon" inner_html=icons.close></span>
                </button>
            </div></div>
            <div class="k-tabs">
                <div class="k-tabs__list" role="tablist" aria-label="Conversation panel" aria-orientation="horizontal"
                    on:keydown=move |event| crate::settings::move_focus(&event, "[role=tablist]", true)>
                    {tabs.into_iter().map(|(tab, label, icon)| view! {
                        <button type="button" role="tab" class="k-tabs__tab" title=label
                            data-state=move || if chat.tab.get() == tab { "active" } else { "inactive" }
                            aria-selected=move || (chat.tab.get() == tab).to_string() tabindex=move || if chat.tab.get() == tab { 0 } else { -1 }
                            hidden=move || tab == "computer" && !computer_available.get() || tab == "run" && chat.run.with(String::is_empty)
                            on:click=move |_| chat.tab.set(tab)>
                            <span class="appearance-icon" inner_html=icon></span><span class="right-panel__tab-label">{label}</span>
                        </button>
                    }).collect_view()}
                </div>
                <div class="k-tabs__panel" role="tabpanel" aria-label=move || chat.tab.get()>
                    {move || {
                        if !chat.details.get() { return ().into_any() }
                        match chat.tab.get() {
                            "files" => view! { <div class="right-panel__body">{files_panel(chat, context)}</div> }.into_any(),
                            "computer" => context.with(|context| context.as_ref().and_then(|value| value.computer.clone()))
                                .filter(|agent| protocol::id(agent)).map(|agent| view! { <div class="right-panel__computer">{computer_panel(chat, agent)}</div> }).into_any(),
                            "run" => view! { <div class="right-panel__body">{run_panel(chat)}</div> }.into_any(),
                            _ => routes::chats(chat).into_any(),
                        }
                    }}
                </div>
            </div>
        </aside>
    }
}

pub fn rich_replies(chat: Chat, row: RwSignal<Turn>) -> impl IntoView {
    rich_reply(chat, row)
}

#[derive(Clone, Deserialize)]
struct FormField {
    name: String,
    label: String,
    #[serde(rename = "type")]
    kind: FieldKind,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    options: Vec<String>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum FieldKind {
    Text,
    Number,
    Boolean,
    Select,
}

pub fn form_reply(
    chat: Chat,
    value: Value,
    path: String,
    revision: u64,
    open: bool,
    state: String,
    busy: RwSignal<bool>,
    error: RwSignal<String>,
) -> impl IntoView {
    let label = value["actions"][0]["label"]
        .as_str()
        .unwrap_or("Submit")
        .to_owned();
    let Some(reply) = serde_json::from_value::<ComponentReply>(value).ok() else {
        return ().into_any();
    };
    let Some(node) = reply.tree.children.iter().find(|node| node.kind == "form") else {
        return ().into_any();
    };
    let Ok(fields) = serde_json::from_value::<Vec<FormField>>(
        reply.data.get(&node.data).cloned().unwrap_or_default(),
    ) else {
        return ().into_any();
    };
    let skip = path.clone();
    let submit_fields = fields.clone();
    let form = NodeRef::<leptos::html::Form>::new();
    view! {
        <h3>{node.title.clone()}</h3>
        <p role="status" hidden=open>{state}</p>
        <form class="component-form k-stack" node_ref=form aria-label=node.title.clone() hidden=!open
            on:submit=move |event| {
                event.prevent_default();
                let Some(data) = form.get_untracked().and_then(|form| web_sys::FormData::new_with_form(&form).ok()) else { return };
                let values: serde_json::Map<String, Value> = submit_fields.iter().filter_map(|field| {
                    let raw = data.get(&field.name).as_string().unwrap_or_default();
                    let value = match field.kind {
                        FieldKind::Boolean => json!(data.has(&field.name)),
                        FieldKind::Number => json!(raw.parse::<f64>().ok().filter(|value| value.is_finite())?),
                        FieldKind::Select if raw.is_empty() => return None,
                        FieldKind::Select => json!(raw),
                        FieldKind::Text if raw.trim().is_empty() => return None,
                        FieldKind::Text => json!(raw.trim()),
                    };
                    Some((field.name.clone(), value))
                }).collect();
                controls::mutate(path.clone(), json!({"decision": "allow", "revision": revision, "values": values}), busy, error);
            }>
            <fieldset class="k-stack component-fields" disabled=move || busy.get() || chat.config.with_value(|c| c.archived)>
                <legend class="sr-only">"Form fields"</legend>
                {fields.into_iter().map(|field| match field.kind {
                    FieldKind::Select => view! { <label class="k-field"><span>{field.label}</span>
                        <select class="k-input" name=field.name required=field.required>
                            <option value="">"Select…"</option>
                            {field.options.into_iter().map(|option| {
                                let value = option.clone();
                                view! { <option value=value>{option}</option> }
                            }).collect_view()}
                        </select></label> }.into_any(),
                    FieldKind::Boolean => view! { <label class="k-check"><input type="checkbox" name=field.name required=field.required />{field.label}</label> }.into_any(),
                    kind => view! { <label class="k-field"><span>{field.label}</span>
                        <input type=if matches!(kind, FieldKind::Number) { "number" } else { "text" } class="k-input" name=field.name
                            required=field.required maxlength="1000" step="any" min="-1000000000000000" max="1000000000000000" />
                    </label> }.into_any(),
                }).collect_view()}
                <div class="k-row-actions">
                    <button type="button" class="k-btn k-btn--sm k-btn--secondary"
                        on:click=move |_| controls::mutate(skip.clone(), json!({"decision": "deny", "revision": revision}), busy, error)>"Skip form"</button>
                    <button type="submit" class="k-btn k-btn--sm k-btn--primary">{label}</button>
                </div>
            </fieldset>
        </form>
    }.into_any()
}

#[derive(Deserialize)]
struct Point {
    label: String,
    value: f64,
}

fn chart(kind: &str, title: String, points: Vec<Point>) -> impl IntoView {
    let low = points.iter().map(|p| p.value).fold(0., f64::min);
    let high = points.iter().map(|p| p.value).fold(0., f64::max);
    let range = if low < high { high - low } else { 1. };
    let step = 364. / points.len().max(1) as f64;
    let x = |index| 54. + (index as f64 + 0.5) * step;
    let y = |value| 126. - (value - low) / range * 116.;
    let plot = if kind == "pie" {
        let total: f64 = points.iter().map(|p| p.value).sum();
        let mut offset = 0.;
        view! { <div class="component-pie" aria-hidden="true">
            <svg viewBox="0 0 100 100">{points.iter().enumerate().filter(|(_, point)| total > 0. && point.value > 0.).map(|(i, point)| {
                let length = point.value / total * 100.;
                let start = offset;
                offset += length;
                view! { <circle class=format!("component-palette-{}", i % 6) cx="50" cy="50" r="34" pathLength="100"
                    stroke-dasharray=format!("{length} {}", 100. - length) stroke-dashoffset=(-start).to_string() transform="rotate(-90 50 50)"></circle> }
            }).collect_view()}</svg>
            <ul>{points.iter().enumerate().map(|(i, point)| view! { <li><span class=format!("component-palette-{}", i % 6)></span>{point.label.clone()}</li> }).collect_view()}</ul>
        </div> }.into_any()
    } else {
        let width = (step * 0.58).min(34.);
        view! { <div class="component-cartesian" aria-hidden="true"><svg viewBox="0 0 426 152">
            <line class="component-baseline" x1="54" x2="418" y1=y(0.).to_string() y2=y(0.).to_string()></line>
            {if kind == "bar" {
                points.iter().enumerate().map(|(i, point)| view! {
                    <rect class="component-plot-fill" x=(x(i) - width / 2.).to_string() y=y(point.value).min(y(0.)).to_string()
                        width=width.to_string() height=(y(point.value) - y(0.)).abs().to_string() rx="2"></rect>
                }).collect_view().into_any()
            } else {
                let line = points.iter().enumerate().map(|(i, point)| format!("{},{}", x(i), y(point.value))).collect::<Vec<_>>().join(" ");
                view! { <polyline class="component-plot-line" points=line></polyline>
                    {points.iter().enumerate().map(|(i, point)| view! {
                        <circle class="component-plot-fill" cx=x(i).to_string() cy=y(point.value).to_string() r="3"></circle>
                    }).collect_view()}
                }.into_any()
            }}
        </svg>
            {[low, high].into_iter().map(|value| view! {
                <span class="component-axis-value" style:top=format!("{}%", y(value) / 152. * 100.)>{value.to_string()}</span>
            }).collect_view()}
            {points.iter().enumerate().take(5).map(|(index, point)| view! {
                <span class="component-axis-label" style:left=format!("{}%", x(index) / 426. * 100.)>{point.label.clone()}</span>
            }).collect_view()}
        </div> }.into_any()
    };
    view! { {plot}
        <details class="component-chart-data">
            <summary>"View data"<span class="sr-only">{format!(" for {title}")}</span></summary>
            <div class="component-table" role="region" aria-label="Chart data" tabindex="0">
                <table><caption>{format!("{title} data")}</caption><thead><tr><th scope="col">"Label"</th><th scope="col">"Value"</th></tr></thead>
                    <tbody>{points.into_iter().map(|p| view! { <tr><th scope="row">{p.label}</th><td>{p.value.to_string()}</td></tr> }).collect_view()}</tbody>
                </table>
            </div>
        </details>
    }
}
