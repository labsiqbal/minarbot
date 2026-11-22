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
                        {(!matches!(node.kind.as_str(), "form" | "table")).then(|| view! { <h3 class="k-section__title">{title.clone()}</h3> })}
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

fn run_panel(chat: Chat) -> impl IntoView {
    let data = RwSignal::new(None::<Result<Value, String>>);
    let refresh = RwSignal::new(0_u32);
    files::watch(
        move || {
            let id = chat.run.get();
            if protocol::id(&id) {
                format!("/api/turns/{id}/inspect")
            } else {
                String::new()
            }
        },
        refresh,
        data,
    );
    view! { <section class="run-inspector" aria-label="Run inspector">
        {move || match data.get() {
            None => files::loading("Loading run...").into_any(),
            Some(Err(error)) => files::retry(error, refresh).into_any(),
            Some(Ok(run)) => inspection(run).into_any(),
        }}
    </section> }
}

fn inspect_value(label: &'static str, text: String) -> impl IntoView {
    let snippet = text
        .chars()
        .take(640)
        .collect::<String>()
        .lines()
        .take(8)
        .collect::<Vec<_>>()
        .join("\n");
    let short = snippet.len() < text.len();
    let preview = if short {
        format!("{snippet}…")
    } else {
        text.clone()
    };
    let expanded = RwSignal::new(false);
    let id = uuid().unwrap_or_default();
    let controls = id.clone();
    view! { <div class="run-inspector__value">
        <div class="run-inspector__value-head"><strong>{label}</strong>
            {short.then(|| view! { <button type="button" class="link-btn link-btn--small" aria-expanded=move || expanded.get().to_string()
                aria-controls=controls on:click=move |_| expanded.update(|value| *value = !*value)>
                {move || if expanded.get() { "Collapse" } else { "Expand" }}<span class="sr-only">{format!(" {}", label.to_ascii_lowercase())}</span>
            </button> })}
        </div>
        <pre id=id><code>{move || if expanded.get() { text.clone() } else { preview.clone() }}</code></pre>
    </div> }
}

fn step_state(state: Option<&str>, run: &str) -> &'static str {
    match state.unwrap_or_default() {
        "completed" | "done" | "received" | "succeeded" => "Completed",
        "failed" | "error" | "uncertain" => "Failed",
        "declined" | "denied" | "rejected" => "Declined",
        "waiting" | "waiting_for_user" | "approval" | "paused" => "Waiting",
        "interrupted" | "aborted" => "Interrupted",
        "cancelled" | "canceled" | "closed" => "Stopped",
        _ => match run {
            "failed" => "Failed",
            "interrupted" => "Interrupted",
            "cancelled" => "Stopped",
            _ if matches!(state, Some("running" | "in_progress" | "inProgress")) => "Running",
            _ => "Pending",
        },
    }
}

fn duration(ms: f64) -> String {
    if ms < 1_000.0 {
        format!("{ms:.0} ms")
    } else {
        format!("{:.2} s", ms / 1_000.0)
    }
}

fn inspection(run: Value) -> impl IntoView {
    let status = run["status"].as_str().unwrap_or_default();
    let chevron = use_context::<Icons>().unwrap_or_default().next;
    let rows = |key: &str| run[key].as_array().into_iter().flatten();
    let judgments = rows("judgments").map(|judge| {
        let title = format!("Turn {}", judge["number"]);
        let met = judge["met"] == true || judge["met"] == 1;
        let reason = judge["reason"].as_str().unwrap_or_default().to_owned();
        view! { <li><details><summary><span class="appearance-icon" inner_html=chevron.clone()></span>
            <span>{title}</span><span class="run-inspector__state" data-state=met.then_some("Done")>{if met { "Met" } else { "More work" }}</span>
        </summary><div class="run-inspector__details"><p>{reason}</p></div></details></li> }
    }).collect::<Vec<_>>();
    let providers = rows("providers").map(|call| {
        let model = call["model"].as_str().unwrap_or_default().to_owned();
        let state = call["status"].as_str();
        let state = step_state(if status == "cancelled" && state == Some("interrupted") { Some("cancelled") } else { state }, status);
        let label = call["duration_ms"].as_f64().map_or_else(|| state.to_owned(), |ms| format!("{state} · {}", duration(ms)));
        let times = inspect_times(call["created"].as_str(), call["finished"].as_str());
        let tokens = [("input_tokens", "Input tokens"), ("output_tokens", "Output tokens"), ("total_tokens", "Total tokens")]
            .into_iter().filter_map(|(field, label)| call[field].as_u64().map(|value| view! { <dt>{label}</dt><dd>{value.to_string()}</dd> })).collect_view();
        view! { <li><details><summary><span class="appearance-icon" inner_html=chevron.clone()></span>
            <span>{model}</span><span class="run-inspector__state">{label}</span>
        </summary><div class="run-inspector__details"><dl>{times}{tokens}</dl></div></details></li> }
    }).collect::<Vec<_>>();
    let show_tool = |step: &&Value| {
        let name = step["name"].as_str().unwrap_or_default();
        let name = name.to_ascii_lowercase().replace(['_', '-', ' '], "");
        !matches!(
            name.as_str(),
            "" | "usermessage"
                | "agentmessage"
                | "assistantmessage"
                | "reasoning"
                | "plan"
                | "contextcompaction"
                | "verificationreminder"
        ) && step["id"] != "verification-reminder"
    };
    let mut tools = rows("tools").filter(show_tool).collect::<Vec<_>>();
    let mut matched = std::collections::HashSet::new();
    let mut steps = Vec::new();
    for step in rows("harness").filter(show_tool) {
        if !matched.insert(step["id"].as_str().unwrap_or_default()) {
            continue;
        }
        let index = tools
            .iter()
            .position(|tool| tool["id"] == step["id"])
            .or_else(|| {
                tools.iter().position(|tool| {
                    tool["name"]
                        .as_str()
                        .map(|name| name.trim_start_matches("mcp__minarbot__"))
                        == step["name"]
                            .as_str()
                            .map(|name| name.trim_start_matches("mcp__minarbot__"))
                })
            });
        steps.push((Some(step), index.map(|index| tools.remove(index))));
    }
    steps.extend(tools.into_iter().map(|tool| (None, Some(tool))));
    let last_failed = steps
        .iter()
        .rposition(|(native, tool)| inspection_step_state(*native, *tool, status) == "Failed");
    let steps = steps.into_iter().enumerate().map(|(index, (native, tool))| {
        let data = native.or(tool).unwrap_or(&Value::Null);
        let state = inspection_step_state(native, tool, status);
        let name = data["name"].as_str().unwrap_or_default();
        let title = data["title"].as_str().map(str::to_owned).unwrap_or_else(|| match name {
            "commandExecution" | "exec_command" | "Bash" | "bash" => "Run shell".into(),
            "fileChange" | "apply_patch" | "Edit" | "MultiEdit" => "Edit files".into(),
            _ => {
                let title = name.rsplit("__").next().unwrap_or(name).replace(['_', '-'], " ");
                let mut letters = title.chars();
                letters.next().map_or_else(String::new, |first| format!("{}{}", first.to_uppercase(), letters.as_str()))
            }
        });
        let field = |key: &str| native.and_then(|value| value[key].as_str()).or_else(|| tool.and_then(|value| value[key].as_str()));
        let started = field("started");
        let ended = field("finished");
        let elapsed = started.zip(ended).map(|(start, end)| js_sys::Date::parse(end) - js_sys::Date::parse(start))
            .filter(|ms| ms.is_finite()).map_or_else(|| "Unavailable".into(), |ms| duration(ms.max(0.0)));
        let times = inspect_times(started, ended);
        let native = native.and_then(|value| value["state"].as_str()).map(str::to_owned);
        let name = name.to_owned();
        let arguments = field("arguments").map(|value| inspect_value("Arguments", value.to_owned()));
        let result = field("result").map(|value| inspect_value(if state == "Failed" { "Error" } else { "Result" }, value.to_owned()));
        view! { <li><details open=last_failed == Some(index)><summary><span class="appearance-icon" inner_html=chevron.clone()></span><span>{title}</span>
            <span class="run-inspector__state" data-state=state>{state}</span></summary><div class="run-inspector__details">
                <dl><dt>"Tool"</dt><dd><code>{name}</code></dd>
                {native.map(|state| view! { <dt>"Native state"</dt><dd><code>{state}</code></dd> })}
                {times}<dt>"Duration"</dt><dd>{elapsed}</dd></dl>
                {arguments}{result}
            </div></details></li> }
    }).collect::<Vec<_>>();
    let error = run["error"]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(|value| {
            inspect_value(
                if status == "cancelled" {
                    "Run stopped"
                } else {
                    "Run error"
                },
                value.to_owned(),
            )
        });
    let stderr = run["engineStderr"]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(|value| inspect_value("Engine stderr", value.to_owned()));
    let empty = error.is_none()
        && stderr.is_none()
        && steps.is_empty()
        && providers.is_empty()
        && judgments.is_empty();
    view! {
        {error}
        {(!judgments.is_empty()).then(|| view! { <section class="run-inspector__providers" aria-label="Goal judge">
            <h3>"Goal judge"</h3><ul class="run-inspector__list">{judgments}</ul></section> })}
        {(!steps.is_empty()).then(|| view! { <ul class="run-inspector__list" aria-label="Steps">{steps}</ul> })}
        {(!providers.is_empty()).then(|| view! { <section class="run-inspector__providers" aria-label="Provider calls">
            <h3>"Provider calls"</h3><ul class="run-inspector__list">{providers}</ul></section> })}
        {stderr}
        {empty.then(|| view! { <p role="status">"No run details"</p> })}
    }
}

fn inspection_step_state(
    native: Option<&Value>,
    tool: Option<&Value>,
    status: &str,
) -> &'static str {
    let failed = native.is_some_and(|step| step["failed"] == 1 || step["failed"] == true)
        || tool.is_some_and(|step| step["state"] == "failed");
    step_state(
        if failed {
            Some("failed")
        } else {
            native.or(tool).and_then(|step| step["state"].as_str())
        },
        status,
    )
}

fn inspect_times(started: Option<&str>, ended: Option<&str>) -> impl IntoView {
    let options = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&options, &"dateStyle".into(), &"short".into());
    let _ = js_sys::Reflect::set(&options, &"timeStyle".into(), &"medium".into());
    let time = |value: Option<&str>| {
        let Some(value) = value.filter(|value| js_sys::Date::parse(value).is_finite()) else {
            return "Unavailable".into_any();
        };
        let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_str(value));
        let text = String::from(date.to_locale_string("default", options.as_ref()));
        view! { <time datetime=value.to_owned()>{text}</time> }.into_any()
    };
    view! { <dt>"Started"</dt><dd>{time(started)}</dd><dt>"Ended"</dt><dd>{time(ended)}</dd> }
}

#[derive(Clone, PartialEq, Deserialize)]
struct ComputerStatus {
    available: bool,
    enabled: bool,
    state: String,
    #[serde(default)]
    control: Control,
    #[serde(default)]
    revision: u64,
    #[serde(rename = "stopsInMs")]
    stops_in_ms: Option<u64>,
    #[serde(default)]
    requests: Vec<ComputerWait>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub(super) struct ComputerWait {
    turn: String,
    call_id: String,
    reason: String,
    secret_entry: Option<String>,
    secret_origin: Option<String>,
}

impl ComputerWait {
    fn label(&self, name: &str, viewer: bool) -> String {
        if self.secret_entry.is_some() {
            format!(
                "{name} needs a secure value for {}",
                self.secret_origin.as_deref().unwrap_or_default()
            )
        } else {
            let reason = if !self.reason.is_empty() {
                &self.reason
            } else if viewer {
                "Computer access is waiting for you."
            } else {
                "The user is controlling this computer."
            };
            format!("{name} needs you to take over: {reason}")
        }
    }
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

#[derive(Clone, Copy)]
struct ComputerControl {
    path: StoredValue<String>,
    key: StoredValue<String>,
    pending: RwSignal<Option<String>>,
    status: RwSignal<Option<ComputerStatus>>,
    error: RwSignal<String>,
    busy: RwSignal<bool>,
}

impl ComputerControl {
    fn new(agent: &str) -> Self {
        let key = format!("minarbot-computer-control:{agent}");
        let control = Self {
            path: StoredValue::new(format!("/api/agents/{agent}/computer")),
            pending: RwSignal::new(saved(&key)),
            key: StoredValue::new(key),
            status: RwSignal::new(None),
            error: RwSignal::new(String::new()),
            busy: RwSignal::new(false),
        };
        control.refresh();
        control
    }

    fn locked(self) -> bool {
        self.busy.get() || self.pending.with(Option::is_some) || self.status.with(Option::is_none)
    }

    fn user(self) -> bool {
        self.status.with(|status| {
            status
                .as_ref()
                .is_some_and(|value| value.control.mode == "user")
        })
    }

    fn refresh(self) {
        if self.status.is_disposed() {
            return;
        }
        let path = self.path.get_value();
        let previous = self.status.get_untracked();
        leptos::task::spawn_local(async move {
            if !document().hidden() {
                let result = get::<ComputerStatus>(&path).await;
                if self.status.is_disposed() {
                    return;
                }
                if !self.busy.get_untracked() && self.status.get_untracked() == previous {
                    match result {
                        Ok(next) => {
                            if previous.as_ref().is_none_or(|previous| {
                                next.control.revision >= previous.control.revision
                                    && previous != &next
                            }) {
                                self.status.set(Some(next));
                            }
                        }
                        Err(reason) => self.error.set(reason.message),
                    }
                }
            }
            set_timeout(move || self.refresh(), Duration::from_secs(1));
        });
    }

    fn reload(self) {
        if self.busy.get_untracked() {
            return;
        }
        self.busy.set(true);
        let endpoint = self.path.get_value();
        let key = self.key.get_value();
        leptos::task::spawn_local(async move {
            match get::<ComputerStatus>(&endpoint).await {
                Ok(value) if remove(&key) => {
                    self.status.try_set(Some(value));
                    self.pending.try_set(None);
                    self.error.try_set(String::new());
                }
                Ok(_) => {
                    self.error
                        .try_set("Computer recovery data could not be cleared.".into());
                }
                Err(reason) => {
                    self.error.try_set(reason.message);
                }
            }
            self.busy.try_set(false);
        });
    }

    fn submit(self, operation: Option<(&str, Option<&str>)>) {
        if self.busy.get_untracked() {
            return;
        }
        let key = self.key.get_value();
        let raw = match (operation, self.pending.get_untracked()) {
            (None, Some(raw)) if valid_computer_command(&raw) => raw,
            (Some((operation, turn)), None) => {
                let Some(revision) = self
                    .status
                    .with_untracked(|s| s.as_ref().map(|s| s.control.revision))
                else {
                    return;
                };
                let Some(id) = uuid() else {
                    return;
                };
                let mut value = json!({"id":id, "revision":revision, "operation":operation});
                if let Some(turn) = turn.filter(|turn| protocol::id(turn)) {
                    value["turn"] = json!(turn);
                }
                let raw = value.to_string();
                if !valid_computer_command(&raw) {
                    return;
                }
                raw
            }
            _ => return,
        };
        if !save(&key, &raw) {
            self.error
                .set("Computer request could not be saved.".into());
            return;
        }
        self.pending.set(Some(raw.clone()));
        self.busy.set(true);
        self.error.set(String::new());
        let endpoint = format!("{}/control", self.path.get_value());
        leptos::task::spawn_local(async move {
            match post::<ComputerStatus>(&endpoint, &raw).await {
                Ok(value) => {
                    self.status.try_set(Some(value));
                    if remove(&key) {
                        self.pending.try_set(None);
                    } else {
                        self.error.try_set(
                            "Computer control changed. Recovery data could not be cleared.".into(),
                        );
                    }
                }
                Err(reason) => {
                    self.error.try_set(reason.message);
                }
            }
            self.busy.try_set(false);
        });
    }
}

fn computer_panel(agent: String, name: String, popout: bool, url: Option<String>) -> impl IntoView {
    let controls = ComputerControl::new(&agent);
    let ComputerControl {
        path,
        pending,
        status,
        error,
        busy,
        ..
    } = controls;
    let settings_key = StoredValue::new(format!("minarbot-computer:{agent}"));
    let settings_pending = RwSignal::new(saved(&settings_key.get_value()));
    let clipboard = RwSignal::new(String::new());
    let connection = RwSignal::new(String::new());
    let screen = NodeRef::<leptos::html::Div>::new();
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let stop_dialog = NodeRef::<leptos::html::Dialog>::new();
    let recover_dialog = NodeRef::<leptos::html::Dialog>::new();
    let hand_back = NodeRef::<leptos::html::Button>::new();
    let settings_dialog = NodeRef::<leptos::html::Dialog>::new();
    let reconnect = RwSignal::new(0_u32);
    let visible = RwSignal::new(!document().hidden());
    let locked = move || controls.locked() || settings_pending.with(Option::is_some);
    let listener = window_event_listener(leptos::ev::visibilitychange, move |_| {
        visible.set(!document().hidden())
    });
    on_cleanup(move || listener.remove());

    let user = move || controls.user();
    let running = move || {
        status.with(|status| {
            status
                .as_ref()
                .is_some_and(|s| matches!(s.state.as_str(), "running" | "idle"))
        })
    };
    let waits = Memo::new(move |_| {
        if running() {
            status.with(|value| {
                value
                    .as_ref()
                    .map_or_else(Vec::new, |value| value.requests.clone())
            })
        } else {
            Vec::new()
        }
    });
    let popup_url = format!("/?computer={agent}&ui=leptos");
    let request_agent = StoredValue::new(agent.clone());
    let visible_mount = Memo::new(move |_| {
        (
            running(),
            visible.get(),
            locked(),
            user(),
            status.with(|s| s.as_ref().map_or(0, |s| s.control.revision)),
        )
    });
    dom_mount(screen, move |node| {
        reconnect.track();
        clipboard.set(String::new());
        connection.set(String::new());
        let (running, visible, locked, user, revision) = visible_mount.get();
        if !running || !visible || locked {
            return None;
        }
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
            user,
            callback.as_ref().unchecked_ref(),
        );
        Some((dispose, callback))
    });
    let control = move |operation: Option<&str>| {
        controls.submit(operation.map(|operation| (operation, None)))
    };
    let command = move |operation: &str, retry: bool| {
        if controls.locked() || !retry && settings_pending.with_untracked(Option::is_some) {
            return;
        }
        let path = path.get_value();
        let endpoint = format!("{path}/{operation}");
        let settings_key = settings_key.get_value();
        let mut enable = None;
        if operation == "start" {
            if let Some(raw) = settings_pending.get_untracked() {
                let valid = serde_json::from_str::<Value>(&raw).ok().filter(|value| {
                    value.as_object().is_some_and(|object| {
                        object.len() == 3
                            && value["enabled"] == true
                            && value["id"].as_str().is_some_and(protocol::id)
                            && value["revision"]
                                .as_u64()
                                .is_some_and(|revision| revision <= 9_007_199_254_740_991)
                    })
                });
                if valid.is_none() {
                    error.set(
                        "Saved computer request is invalid. Load latest status before continuing."
                            .into(),
                    );
                    return;
                }
                enable = Some(raw);
            } else if let Some(revision) = status.with_untracked(|status| {
                status
                    .as_ref()
                    .filter(|status| !status.enabled)
                    .map(|status| status.revision)
            }) {
                let Some(id) = uuid() else {
                    return;
                };
                let raw = json!({"id":id,"revision":revision,"enabled":true}).to_string();
                if !save(&settings_key, &raw) {
                    error.set("Browser could not save recovery data. Check site storage.".into());
                    return;
                }
                settings_pending.set(Some(raw.clone()));
                enable = Some(raw);
            }
        }
        error.set(String::new());
        busy.set(true);
        leptos::task::spawn_local(async move {
            if let Some(raw) = enable {
                match post::<ComputerStatus>(&path, &raw).await {
                    Ok(value) if remove(&settings_key) => {
                        let enabled = value.enabled;
                        status.try_set(Some(value));
                        settings_pending.try_set(None);
                        if !enabled || busy.is_disposed() {
                            busy.try_set(false);
                            return;
                        }
                    }
                    Ok(_) => {
                        error.try_set(
                            "Computer change confirmed. Recovery data could not be cleared.".into(),
                        );
                        busy.try_set(false);
                        return;
                    }
                    Err(reason) => {
                        error.try_set(reason.message);
                        busy.try_set(false);
                        return;
                    }
                }
            }
            let result = post::<ComputerStatus>(&endpoint, "{}").await;
            match result {
                Ok(value) => {
                    status.try_set(Some(value));
                }
                Err(reason) => {
                    error.try_set(reason.message);
                }
            }
            busy.try_set(false);
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
    let icons = use_context::<Icons>().unwrap_or_default();
    let title = format!("{name}'s computer");
    let name = StoredValue::new(name);
    let title_attr = title.clone();
    let menu = NodeRef::<leptos::html::Details>::new();
    let close_menu = move || {
        if let Some(menu) = menu.get_untracked() {
            menu.set_open(false);
        }
    };
    view! {
        <section class="computer-viewer bs-root" aria-label="Computer">
            {url.map(|url| {
                let target = url.clone();
                view! { <div class="k-row-actions"><span class="right-panel__path">{url}</span>
                    <button class="k-btn k-btn--sm k-btn--secondary" type="button" disabled=move || !running() || !user() || locked() on:click=move |_| {
                        let revision = status.with_untracked(|status| status.as_ref().map(|status| status.control.revision));
                        let endpoint = format!("{}/input", path.get_value());
                        let raw = json!({"revision":revision,"name":"computer_open_url","args":{"url":target}}).to_string();
                        leptos::task::spawn_local(async move { if let Err(reason) = post::<Value>(&endpoint, &raw).await { error.try_set(reason.message); } });
                    }>"Open URL"</button>
                </div> }
            })}
            <header class="computer-viewer__toolbar" hidden=move || !running()>
                <div class="computer-viewer__identity">
                    <div class="computer-viewer__heading">
                        <h2 class="computer-viewer__title" title=title_attr>{title}</h2>
                        <span class="k-badge is-success"><span class="computer-viewer__status-dot is-live" aria-hidden="true"></span>"Live"</span>
                    </div>
                    <div class="computer-viewer__meta"><span class="computer-viewer__owner">
                        {move || if user() { "You're in control" } else { "Agent in control" }}
                    </span>{move || status.with(|status| status.as_ref().and_then(|status| status.stops_in_ms).map(|ms| view! {
                        <span class="computer-viewer__since">{format!("Stops in {} min", ms.div_ceil(60000).max(1))}</span>
                    }))}</div>
                </div>
                <div class="computer-viewer__actions">
                    <button type="button" class="k-icon-btn" aria-label="Pop out" title="Pop out" hidden=popout on:click=move |_| {
                        let _ = window().open_with_url_and_target_and_features(&popup_url, "_blank", "popup,width=1280,height=960,noopener");
                    }><span class="appearance-icon" inner_html=icons.external></span></button>
                    <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=locked hidden=move || !user() on:click=move |_| paste()>"Paste clipboard"</button>
                    <button type="button" class="k-btn k-btn--sm k-btn--secondary" hidden=move || !user() || clipboard.with(String::is_empty)
                        on:click=move |_| {
                            let text = clipboard.get_untracked();
                            leptos::task::spawn_local(async move { if write_clipboard(&text).await.is_err() { error.try_set("Clipboard could not be written.".into()); } });
                        }>"Copy clipboard"</button>
                    <button type="button" class="k-btn k-btn--sm k-btn--primary" disabled=locked hidden=user
                        on:click=move |_| { if let Some(dialog) = dialog.get_untracked() { let _ = dialog.show_modal(); } }>"Take control"</button>
                    <button type="button" class="k-btn k-btn--sm k-btn--primary" node_ref=hand_back disabled=locked hidden=move || !user()
                        aria-label="Hand back to agent" on:click=move |_| control(Some("hand-back"))>"Hand back"</button>
                    <details node_ref=menu class="chat-pilot-menu computer-actions" on:keydown=move |event| { if event.key() == "Escape" { close_menu(); event.stop_propagation(); } }>
                        <summary class="k-icon-btn" aria-label="More computer actions" title="More computer actions"><span class="appearance-icon" inner_html=icons.more></span></summary>
                        <div class="k-pop k-menu task-actions">
                            <button type="button" class="k-menu__item" disabled=locked on:click=move |_| { close_menu(); reconnect.update(|count| *count = count.wrapping_add(1)); }>"Reconnect"</button>
                            <button type="button" class="k-menu__item is-danger" disabled=locked
                                on:click=move |_| { close_menu(); if let Some(dialog) = stop_dialog.get_untracked() { let _ = dialog.show_modal(); } }>"Stop computer"</button>
                        </div>
                    </details>
                </div>
            </header>
            <Show when=move || !waits.with(Vec::is_empty)>
                <div class="computer-viewer__requests">
                    <For each=move || waits.get() key=|wait| (wait.turn.clone(), wait.call_id.clone()) children=move |wait| {
                        let turn = wait.turn.clone();
                        let label = name.with_value(|name| wait.label(name, true));
                        view! { <div class="computer-viewer__request"><span>{label}</span>
                            {wait.secret_entry.is_some().then(|| secure_request(request_agent.get_value(), wait, controls))}
                            <button type="button" class="k-btn k-btn--sm k-btn--ghost" hidden=user disabled=locked
                                on:click=move |_| controls.submit(Some(("dismiss", Some(&turn))))>"Dismiss"</button>
                        </div> }
                    }/>
                </div>
            </Show>
            <p class="computer-viewer__idle-control" hidden=move || running() || !user()>"You still hold control. "
                <button class="k-btn k-btn--sm k-btn--ghost" type="button" disabled=locked on:click=move |_| control(Some("hand-back"))>"Hand back"</button>
            </p>
            <div class="computer-viewer__screen" role=move || running().then_some("application") aria-label=move || running().then_some("Agent computer. Shift+Escape returns to controls.")
                class:is-controlled=user on:keydown:capture=move |event| {
                if user() && event.shift_key() && event.key() == "Escape" {
                    event.prevent_default(); event.stop_propagation();
                    if let Some(button) = hand_back.get_untracked() { let _ = button.focus(); }
                }
                if user() && (event.ctrl_key() || event.meta_key()) && event.key().eq_ignore_ascii_case("v") {
                    event.prevent_default(); event.stop_propagation(); paste();
                }
            }><div class="computer-viewer__canvas" node_ref=screen aria-label="Agent computer screen" hidden=move || !running()></div>
                <div class="computer-viewer__empty" role="status" aria-live="polite"
                    hidden=move || running() && visible.get() && connection.with(|state| state == "Live")>
                    {move || {
                        let starting = status.with(|value| value.as_ref().is_some_and(|value| value.state == "starting"));
                        let screen_error = connection.with(|state| !state.is_empty() && !matches!(state.as_str(), "Live" | "Connecting to computer…"));
                        let connecting = visible.get() && running() && !screen_error;
                        let message = if !visible.get() { Some("Viewer paused while window is hidden.".to_owned()) }
                            else if screen_error && running() { Some(connection.get()) }
                            else if starting { Some("Starting computer...".into()) }
                            else if connecting { Some("Connecting to computer...".into()) }
                            else if status.with(Option::is_none) { Some("Loading computer status...".into()) }
                            else { None };
                        view! {
                            {if starting || connecting { view! { <span class="k-spinner computer-viewer__spinner" aria-hidden="true"></span> }.into_any() }
                            else { view! { <span class="appearance-icon computer-empty-icon" inner_html=icons.computer.clone()></span> }.into_any() }}
                            {message.map(|message| view! { <p>{message}</p> })}
                            {if screen_error && running() && visible.get() {
                                view! { <button type="button" class="k-btn k-btn--sm k-btn--secondary"
                                    on:click=move |_| reconnect.update(|count| *count = count.wrapping_add(1))>"Reconnect"</button> }.into_any()
                            } else if !running() && !starting && status.with(|value| value.as_ref().is_some_and(|value| value.available)) {
                                view! { <button type="button" class="k-btn k-btn--sm k-btn--primary" disabled=locked on:click=move |_| command("start", false)>"Turn on computer"</button> }.into_any()
                            } else { ().into_any() }}
                        }
                    }}
                </div>
            </div>
            <p class="sr-only" role="status" hidden=move || !running()>{connection}</p>

            <div class="k-row-actions" hidden=move || settings_pending.with(Option::is_none)>
                <p role="status">"Couldn't confirm the change"</p>
                <button type="button" class="k-btn k-btn--sm k-btn--primary" disabled=busy on:click=move |_| command("start", true)>"Try again"</button>
                <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=busy on:click=move |_| controls::dialog_open(settings_dialog)>"Reload"</button>
            </div>
            <dialog class="conversation-dialog" node_ref=settings_dialog aria-label="Discard computer request?">
                <p>"Changes already accepted by the server still apply."</p>
                <form method="dialog" class="k-row-actions">
                    <button class="k-btn k-btn--sm k-btn--secondary">"Cancel"</button>
                    <button class="k-btn k-btn--sm k-btn--danger" on:click=move |_| {
                        if busy.get_untracked() { return; }
                        busy.set(true);
                        let path = path.get_value();
                        let key = settings_key.get_value();
                        leptos::task::spawn_local(async move {
                            match get::<ComputerStatus>(&path).await {
                                Ok(value) if remove(&key) => {
                                    status.try_set(Some(value));
                                    settings_pending.try_set(None);
                                    error.try_set(String::new());
                                }
                                Ok(_) => { error.try_set("Computer recovery data could not be cleared.".into()); }
                                Err(reason) => { error.try_set(reason.message); }
                            }
                            busy.try_set(false);
                        });
                    }>"Discard and reload"</button>
                </form>
            </dialog>
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
                    <button class="k-btn k-btn--sm k-btn--primary" on:click=move |_| controls.reload()>"Load current control"</button>
                </form>
            </dialog>
            <dialog class="conversation-dialog" node_ref=stop_dialog aria-label="Stop computer?">
                <p>"Unsaved work in the desktop may be lost."</p>
                <form method="dialog" class="k-row-actions">
                    <button class="k-btn k-btn--sm k-btn--secondary">"Cancel"</button>
                    <button class="k-btn k-btn--sm k-btn--primary" on:click=move |_| command("stop", false)>"Stop computer"</button>
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
    let context = RwSignal::new(None::<Result<files::Context, String>>);
    let files_page = RwSignal::new(None::<Result<Vec<files::PanelFile>, String>>);
    let git = RwSignal::new(None::<Result<changes::Status, String>>);
    let refresh = RwSignal::new(0_u32);
    let computer = Memo::new(move |_| {
        chat.result
            .with(|result| result.as_ref().and_then(|target| target.agent.clone()))
            .or_else(|| {
                context.with(|value| {
                    value
                        .as_ref()
                        .and_then(|value| value.as_ref().ok())
                        .and_then(|value| value.computer.clone())
                })
            })
            .filter(|id| protocol::id(id))
    });
    let status = RwSignal::new(None::<Result<ComputerStatus, String>>);
    files::watch(
        move || {
            if chat.details.get() {
                format!("{}/panel", chat.base())
            } else {
                String::new()
            }
        },
        refresh,
        context,
    );
    files::watch(
        move || {
            if chat.details.get() {
                format!("{}/panel/files", chat.base())
            } else {
                String::new()
            }
        },
        refresh,
        files_page,
    );
    files::watch(
        move || {
            if chat.details.get()
                && context.with(|value| {
                    value
                        .as_ref()
                        .and_then(|value| value.as_ref().ok())
                        .is_some_and(|context| context.workspace.is_some())
                })
            {
                format!("{}/panel/git", chat.base())
            } else {
                String::new()
            }
        },
        refresh,
        git,
    );
    files::watch(
        move || match computer.get().filter(|_| chat.details.get()) {
            Some(agent) => format!("/api/agents/{agent}/computer"),
            None => String::new(),
        },
        refresh,
        status,
    );
    let available = move || {
        status.with(|value| {
            value
                .as_ref()
                .and_then(|value| value.as_ref().ok())
                .is_some_and(|status| status.available)
        })
    };
    let current_tab = Memo::new(move |_| {
        chat.result.with(|target| {
            target
                .as_ref()
                .map_or_else(|| chat.tab.get(), results::ResultTarget::tab)
        })
    });
    let selected_tab = move || match current_tab.get() {
        "preview" | "diff" => "files",
        tab => tab,
    };
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
    let profile = NodeRef::<leptos::html::Dialog>::new();
    let thread_settings = NodeRef::<leptos::html::Dialog>::new();
    let (agent, thread) = chat
        .config
        .with_value(|config| (config.agent.clone(), config.thread.clone()));
    let settings_label = if thread.is_some() {
        "Conversation settings"
    } else if agent.as_ref().is_some_and(|agent| agent.builtin) {
        "AI connection"
    } else {
        "Edit profile"
    };
    let settings = if thread.is_some() {
        thread_settings
    } else {
        profile
    };
    let settings_icon = icons.settings;
    let drawer = NodeRef::<leptos::html::Dialog>::new();
    let narrow = RwSignal::new(details_overlay());
    let resize = window_event_listener(leptos::ev::resize, move |_| narrow.set(details_overlay()));
    on_cleanup(move || resize.remove());
    Effect::new(move |previous: Option<bool>| {
        let overlay = narrow.get();
        let open = chat.details.get();
        if let Some(drawer) = drawer.get() {
            if drawer.open()
                && (!open || previous.is_some_and(|was_overlay| was_overlay != overlay))
            {
                drawer.close();
            }
            if open && !drawer.open() {
                if overlay {
                    let _ = drawer.show_modal();
                    if let Ok(Some(tab)) = drawer.query_selector("[role=tab][aria-selected=true]") {
                        if let Ok(tab) = tab.dyn_into::<web_sys::HtmlElement>() {
                            let _ = tab.focus();
                        }
                    }
                } else {
                    let _ = drawer.show();
                }
            }
        }
        overlay
    });
    view! {
        <dialog class="conversation-panel-shell" node_ref=drawer aria-label="Conversation panel"
            on:cancel=move |event: web_sys::Event| { event.prevent_default(); chat.set_details(false); }>
        <aside class="details right-panel conversation-panel bs-root" aria-label="Conversation details">
            <div class="right-panel__header"><div>
                {agent.as_ref().map(|agent| if agent.builtin {
                    let from = routes::link(&format!("/agents/{}", agent.id));
                    view! { <a class="k-icon-btn" aria-label=settings_label title=settings_label href=crate::settings::section_url("/settings/providers", &from)>
                        <span class="appearance-icon" inner_html=settings_icon.clone()></span>
                    </a> }.into_any()
                } else {
                    view! { <button type="button" class="k-icon-btn" aria-label=settings_label title=settings_label on:click=move |_| controls::dialog_open(settings)>
                        <span class="appearance-icon" inner_html=settings_icon.clone()></span>
                    </button> }.into_any()
                })}
                <button type="button" class="k-icon-btn" aria-label="Close details" title="Close details" on:click=move |_| chat.set_details(false)>
                    <span class="appearance-icon" inner_html=icons.close></span>
                </button>
            </div></div>
            <div class="k-tabs">
                <div class="k-tabs__list" role="tablist" aria-label="Conversation panel" aria-orientation="horizontal"
                    on:keydown=move |event| crate::settings::move_focus(&event, "[role=tablist]", true)>
                    {tabs.into_iter().map(|(tab, label, icon)| view! {
                        <button type="button" role="tab" class="k-tabs__tab" title=label
                            data-state=move || if selected_tab() == tab { "active" } else { "inactive" }
                            aria-selected=move || (selected_tab() == tab).to_string() tabindex=move || if selected_tab() == tab { 0 } else { -1 }
                            hidden=move || tab == "computer" && !available() && current_tab.get() != "computer" || tab == "run" && chat.run.with(String::is_empty)
                            on:click=move |_| chat.select_tab(tab)>
                            <span class="appearance-icon" inner_html=icon></span><span class="right-panel__tab-label">{label}</span>
                        </button>
                    }).collect_view()}
                </div>
                {move || {
                    let error = [context.with(|value| value.as_ref().and_then(|result| result.as_ref().err()).cloned()),
                        files_page.with(|value| value.as_ref().and_then(|result| result.as_ref().err()).cloned()),
                        git.with(|value| value.as_ref().and_then(|result| result.as_ref().err()).cloned())].into_iter().flatten().next();
                    error.map(|error| view! { <div class="right-panel__error">{files::retry(error, refresh)}</div> })
                }}
                <div class="k-tabs__panel" role="tabpanel" aria-label=selected_tab>
                    {move || {
                        if !chat.details.get() { return ().into_any() }
                        match current_tab.get() {
                            "files" => files::panel(chat, context, git, files_page, refresh).into_any(),
                            "diff" => changes::panel(chat, git).into_any(),
                            "preview" => chat.result.get().filter(|target| target.view == results::Kind::Url)
                                .map(|target| result_preview(chat, target, computer, status).into_any())
                                .unwrap_or_else(|| context.with(|value| value.as_ref().and_then(|result| result.as_ref().ok()).cloned())
                                    .map_or_else(|| files::loading("Loading preview...").into_any(), |context| files::artifacts(chat, context).into_any())),
                            "computer" => {
                                if let Some(target) = chat.result.get().filter(|target| target.view == results::Kind::Computer) {
                                    result_preview(chat, target, computer, status).into_any()
                                } else {
                                    computer.get().map(|agent| view! { <div class="right-panel__computer">{computer_panel(agent, chat.config.with_value(|config| config.agent.as_ref().map_or_else(|| config.name.clone(), |agent| agent.name.clone())), false, None)}</div> }).into_any()
                                }
                            }
                            "run" => view! { <div class="right-panel__body">{run_panel(chat)}</div> }.into_any(),
                            _ => routes::chats(chat, profile).into_any(),
                        }
                    }}
                </div>
            </div>
        </aside>
        {agent.filter(|agent| !agent.builtin).map(|agent| view! {
            {controls::edit_dialog(agent.clone(), None, profile)}
            {thread.map(|thread| controls::edit_dialog(agent, Some(thread), thread_settings))}
        })}
        </dialog>
    }
}

fn details_overlay() -> bool {
    window()
        .match_media("(max-width: 767px)")
        .ok()
        .flatten()
        .is_some_and(|query| query.matches())
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
        <h3 class="k-section__title">{node.title.clone()}</h3>
        <p class="component-detail" role="status" hidden=open>{state}</p>
        <form class="component-form k-stack" node_ref=form aria-label=node.title.clone() autocomplete="off" hidden=!open
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
                <legend class="component-sr-only">{node.title.clone()}</legend>
                {fields.into_iter().map(|field| match field.kind {
                    FieldKind::Select => view! { <label class="k-field"><span class="k-field__label">{field.label}{(!field.required).then(|| view! { <span class="k-field__optional">"Optional"</span> })}</span>
                        <select class="k-input" name=field.name required=field.required>
                            <option value="">"Select…"</option>
                            {field.options.into_iter().map(|option| {
                                let value = option.clone();
                                view! { <option value=value>{option}</option> }
                            }).collect_view()}
                        </select></label> }.into_any(),
                    FieldKind::Boolean => view! { <label class="k-check component-checkbox"><input type="checkbox" name=field.name required=field.required />{field.label}{field.required.then(|| view! { <span class="component-detail" aria-hidden="true">"Required"</span> })}</label> }.into_any(),
                    kind => view! { <label class="k-field"><span class="k-field__label">{field.label}{(!field.required).then(|| view! { <span class="k-field__optional">"Optional"</span> })}</span>
                        <input type=if matches!(kind, FieldKind::Number) { "number" } else { "text" } class="k-input" name=field.name
                            required=field.required maxlength="1000" step="any" min="-1000000000000000" max="1000000000000000" />
                    </label> }.into_any(),
                }).collect_view()}
            </fieldset>
            <p class="message-error" role="alert" hidden=move || error.with(String::is_empty)>{error}</p>
            <div class="k-row-actions">
                <button type="button" class="k-btn k-btn--sm k-btn--secondary" disabled=move || busy.get() || chat.config.with_value(|c| c.archived)
                    on:click=move |_| controls::mutate(skip.clone(), json!({"decision": "deny", "revision": revision}), busy, error)>"Skip form"</button>
                <button type="submit" class="k-btn k-btn--sm k-btn--primary" disabled=move || busy.get() || chat.config.with_value(|c| c.archived)>{label}</button>
            </div>
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
    let high = if low == high { 1. } else { high };
    let range = high - low;
    let mut ticks = [low, (low + high) / 2., high, 0.].to_vec();
    ticks.sort_by(f64::total_cmp);
    ticks.dedup();
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
            {ticks.iter().map(|tick| view! {
                <line class=if *tick == 0. { "component-baseline" } else { "component-gridline" } x1="54" x2="418" y1=y(*tick).to_string() y2=y(*tick).to_string()></line>
            }).collect_view()}
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
            {ticks.into_iter().filter(|tick| *tick == 0. || (y(*tick) - y(0.)).abs() > 13.).map(|value| view! {
                <span class="component-axis-value" style:top=format!("{}%", y(value) / 152. * 100.)>{value.to_string()}</span>
            }).collect_view()}
            {(0..points.len().min(5)).map(|tick| {
                let index = (tick as f64 * (points.len() - 1) as f64 / (points.len().min(5) - 1).max(1) as f64).round() as usize;
                let point = &points[index];
                view! {
                <span class="component-axis-label" style:left=format!("{}%", x(index) / 426. * 100.)>{point.label.clone()}</span>
                }
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

fn result_preview(
    chat: Chat,
    target: results::ResultTarget,
    computer: Memo<Option<String>>,
    status: RwSignal<Option<Result<ComputerStatus, String>>>,
) -> impl IntoView {
    let target = StoredValue::new(target);
    let path = target.with_value(|target| target.path.clone());
    let browser = target.with_value(|target| target.view == results::Kind::Computer);
    let agent = Memo::new(move |_| {
        computer.get().filter(|_| {
            status.with(|status| {
                status
                    .as_ref()
                    .and_then(|value| value.as_ref().ok())
                    .is_some_and(|value| value.available)
            })
        })
    });
    let opened = RwSignal::new(None::<Result<(), String>>);
    let frame = RwSignal::new(None::<bool>);
    let url = StoredValue::new(path);
    if browser {
        Effect::new(move |_| {
            let Some(agent) = agent.get() else {
                return;
            };
            let path = url.get_value();
            let endpoint = format!("/api/agents/{agent}/computer/open-url");
            opened.set(None);
            leptos::task::spawn_local(async move {
                let result = post::<Value>(&endpoint, &json!({"url":path}).to_string()).await;
                opened.try_set(Some(result.map(|_| ()).map_err(|error| error.message)));
            });
        });
    } else {
        let path = url.get_value();
        let local = web_sys::Url::new(&path)
            .ok()
            .is_some_and(|url| window().location().origin().ok().as_deref() == Some(&url.origin()));
        if local {
            leptos::task::spawn_local(async move {
                frame.try_set(Some(
                    Request::get(&path)
                        .send()
                        .await
                        .is_ok_and(|response| response.ok()),
                ));
            });
        } else {
            frame.set(Some(true));
        }
    }
    let title = target.with_value(|target| {
        if target.name.is_empty() {
            target.path.clone()
        } else {
            target.name.clone()
        }
    });
    view! { <div class="right-panel__body k-stack">
        <div class="right-panel__preview-head">{files::back(chat, "Back to files")}<span class="right-panel__path">{title}</span></div>
        {move || if browser {
            match (agent.get(), opened.get()) {
                (None, _) if computer.get().is_some() && status.with(Option::is_none) => view! { <p role="status">"Opening page in Computer..."</p> }.into_any(),
                (None, _) => files::empty("Computer unavailable").into_any(),
                (Some(_), None) => view! { <p role="status">"Opening page in Computer..."</p> }.into_any(),
                (Some(_), Some(Err(message))) => view! { <p role="status">{message}</p> }.into_any(),
                (Some(agent), Some(Ok(()))) => computer_panel(agent, chat.config.with_value(|config| config.agent.as_ref().map_or_else(|| config.name.clone(), |agent| agent.name.clone())), false, Some(url.get_value())).into_any(),
            }
        } else {
            match frame.get() {
                None => view! { <p role="status">"Loading preview..."</p> }.into_any(),
                Some(false) => files::empty("Page not found").into_any(),
                Some(true) => view! { <iframe class="right-panel__preview" title="URL preview" sandbox="" referrerpolicy="no-referrer"
                    src=url.get_value() on:error=move |_| frame.set(Some(false))></iframe> }.into_any(),
            }
        }}
        {move || (!browser && agent.get().is_some()).then(|| view! {
            <button type="button" class="k-btn k-btn--md k-btn--secondary" on:click=move |_| {
                let mut target = target.get_value();
                target.view = results::Kind::Computer;
                target.agent = agent.get_untracked();
                results::open(chat, target);
            }>"Open in Computer"</button>
        })}
    </div> }
}

fn secure_request(agent: String, wait: ComputerWait, controls: ComputerControl) -> impl IntoView {
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let input = NodeRef::<leptos::html::Input>::new();
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let Some(entry) = wait.secret_entry else {
        return ().into_any();
    };
    let data = StoredValue::new((agent, wait.turn, wait.call_id));
    let icon = use_context::<Icons>().unwrap_or_default().close;
    let origin = wait.secret_origin.unwrap_or_default();
    let clear = move || {
        if let Some(input) = input.get_untracked() {
            input.set_value("");
        }
        error.set(String::new());
    };
    let cancel = move || {
        if !busy.get_untracked() {
            clear();
            if let Some(dialog) = dialog.get_untracked() {
                dialog.close();
            }
        }
    };
    let send = move |event: web_sys::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() || controls.locked() || controls.user() {
            return;
        }
        let Some(input) = input.get_untracked() else {
            return;
        };
        let value = input.value();
        if value.is_empty() || value.encode_utf16().count() > 4000 {
            return;
        }
        let (agent, turn, call) = data.get_value();
        let raw = json!({"agent":agent, "turn":turn, "call":call, "value":value}).to_string();
        input.set_value("");
        busy.set(true);
        error.set(String::new());
        leptos::task::spawn_local(async move {
            let response = post::<Value>("/api/vault/respond", &raw).await;
            if busy.is_disposed() {
                return;
            }
            busy.set(false);
            if response.is_ok() {
                cancel();
            } else {
                clear();
                error.set(
                    "Secure fill could not be confirmed. Check the request before trying again."
                        .into(),
                );
            }
        });
    };
    view! {
        <button type="button" class="k-btn k-btn--sm k-btn--primary" hidden=move || controls.user()
            disabled=move || controls.locked() || busy.get() on:click=move |_| { clear(); controls::dialog_open(dialog); }>"Enter value"</button>
        <dialog class="k-dialog k-dialog--md conversation-kit-dialog" aria-label="Enter a secure value" node_ref=dialog
            on:cancel=move |event: web_sys::Event| { event.prevent_default(); cancel(); }
            on:close=move |_| clear()>
            <div class="k-dialog__head"><div><h2 class="k-dialog__title">"Enter a secure value"</h2>
                <p class="k-dialog__desc">{origin}</p></div>
                <button type="button" class="k-icon-btn" aria-label="Close" title="Close" disabled=busy on:click=move |_| cancel()><span class="appearance-icon" inner_html=icon></span></button>
            </div>
            <form autocomplete="off" on:submit=send>
                <div class="k-dialog__body k-stack">
                    <p class="message-error" role="alert" hidden=move || error.with(String::is_empty)>{error}</p>
                    <label class="k-field"><span class="k-field__label">{entry}</span>
                        <input class="k-input" type="password" name="value" node_ref=input autocomplete="new-password" required maxlength="4000" autofocus disabled=busy />
                    </label>
                </div>
                <div class="k-dialog__foot">
                    <button type="button" class="k-btn k-btn--md k-btn--secondary" disabled=busy on:click=move |_| cancel()>"Cancel"</button>
                    <button type="submit" class="k-btn k-btn--md k-btn--primary" disabled=busy aria-busy=move || busy.get().to_string()>"Save and fill"</button>
                </div>
            </form>
        </dialog>
    }.into_any()
}

pub(super) fn computer_request(agent: String, name: String, wait: ComputerWait) -> impl IntoView {
    if !protocol::id(&agent) || !protocol::id(&wait.turn) {
        return ().into_any();
    }
    let controls = ComputerControl::new(&agent);
    let take = NodeRef::<leptos::html::Dialog>::new();
    let reset = NodeRef::<leptos::html::Dialog>::new();
    let turn = wait.turn.clone();
    let label = wait.label(&name, false);
    let secret = wait.secret_entry.is_some();
    view! {
        <div class="bs-root computer-request"><div class="k-card">
            <p><strong>{label}</strong></p>
            <div class="k-inline">
                <button type="button" class="k-btn k-btn--sm k-btn--secondary" hidden=move || !controls.user()
                    disabled=move || controls.locked() on:click=move |_| controls.submit(Some(("hand-back", None)))>"Hand back to agent"</button>
                {if secret { secure_request(agent, wait, controls).into_any() } else { view! {
                    <button type="button" class="k-btn k-btn--sm k-btn--primary" hidden=move || controls.user()
                        disabled=move || controls.locked() on:click=move |_| controls::dialog_open(take)>"Take control"</button>
                }.into_any() }}
                <button type="button" class="k-btn k-btn--sm k-btn--secondary" hidden=move || controls.user()
                    disabled=move || controls.locked() on:click=move |_| controls.submit(Some(("dismiss", Some(&turn))))>"Dismiss"</button>
            </div>
        </div>
        <dialog class="k-dialog k-dialog--sm conversation-kit-dialog" node_ref=take aria-label="Take control?">
            <div class="k-dialog__head"><div><h2 class="k-dialog__title">"Take control?"</h2>
                <p class="k-dialog__desc">"Computer tools will pause until you hand control back. Closing this viewer keeps you in control."</p></div></div>
            <form method="dialog" class="k-dialog__foot">
                <button class="k-btn k-btn--md k-btn--secondary">"Cancel"</button>
                <button class="k-btn k-btn--md k-btn--primary" on:click=move |_| controls.submit(Some(("take", None)))>"Take control"</button>
            </form>
        </dialog>
        <div class="k-row-actions" hidden=move || controls.pending.with(Option::is_none)>
            <p role="status">"Control change not confirmed"</p>
            <button class="k-btn k-btn--sm k-btn--primary" type="button" disabled=controls.busy
                hidden=move || !controls.pending.with(|raw| raw.as_deref().is_some_and(valid_computer_command))
                on:click=move |_| controls.submit(None)>"Retry saved request"</button>
            <button class="k-btn k-btn--sm k-btn--secondary" type="button" disabled=controls.busy on:click=move |_| controls::dialog_open(reset)>"Load current control"</button>
        </div>
        <dialog class="conversation-dialog" aria-label="Load current control?" node_ref=reset>
            <p>"Accepted changes still apply. Check current control before taking another action."</p>
            <form method="dialog" class="k-row-actions">
                <button class="k-btn k-btn--sm k-btn--secondary">"Cancel"</button>
                <button class="k-btn k-btn--sm k-btn--primary" on:click=move |_| controls.reload()>"Load current control"</button>
            </form>
        </dialog>
        <p class="message-error" role="alert" hidden=move || controls.error.with(String::is_empty)>{controls.error}</p>
        </div>
    }.into_any()
}

pub(crate) fn computer_window() -> impl IntoView {
    let agent = window()
        .location()
        .search()
        .ok()
        .and_then(|query| web_sys::UrlSearchParams::new_with_str(&query).ok())
        .and_then(|query| query.get("computer"))
        .filter(|id| protocol::id(id))
        .map(|id| id.to_ascii_lowercase());
    let result = RwSignal::new(None::<Result<(String, String), String>>);
    leptos::task::spawn_local(async move {
        let value = match agent {
            Some(id) => match get::<Value>("/api/agents").await {
                Ok(directory) => directory["agents"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|agent| agent["id"] == id)
                    .and_then(|agent| agent["name"].as_str())
                    .map(|name| (id, name.to_owned()))
                    .ok_or_else(|| "Agent not found.".into()),
                Err(reason) => Err(reason.message),
            },
            None => Err("Agent not found.".into()),
        };
        if let Ok((_, name)) = &value {
            document().set_title(&format!("Computer · {name} · Minarbot"));
        } else {
            document().set_title("Computer unavailable · Minarbot");
        }
        result.try_set(Some(value));
    });
    view! { <main class="computer-window bs-root">
        {move || match result.get() {
            Some(Ok((id, name))) => computer_panel(id, name, true, None).into_any(),
            Some(Err(reason)) => view! { <p class="message-error" role="alert">{reason}</p> }.into_any(),
            None => view! { <p role="status">"Loading computer status..."</p> }.into_any(),
        }}
    </main> }
}
