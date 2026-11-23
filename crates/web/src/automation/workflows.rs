use super::*;
use graph::{edges, id, step_label, Graph, KINDS};
use schedule::{format, merge};
use std::cell::RefCell;
use wasm_bindgen::{prelude::wasm_bindgen, JsCast, JsValue};

thread_local! {
    // ponytail: keep one agent's first page for the lifetime of the document.
    static LIBRARY_PAGE: RefCell<Option<(String, Value)>> = const { RefCell::new(None) };
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_name = minarbotWorkflowMarkdown)]
    fn render_markdown(text: &str, scope: &str) -> Result<js_sys::Promise, JsValue>;
}

fn result_markdown(value: String, run: &str) -> impl IntoView {
    let html = RwSignal::new(None::<String>);
    let rendered = if value.is_empty() {
        None
    } else {
        render_markdown(&value, &format!("/api/workflow-runs/{run}")).ok()
    };
    if let Some(rendered) = rendered {
        spawn_local(async move {
            if let Ok(result) = wasm_bindgen_futures::JsFuture::from(rendered).await {
                html.try_set(result.as_string());
            }
        });
    }
    view! {
        <div class="legacy automation-markdown">
            {move || html.get().map_or_else(
                || view! { <p class="automation-result">{value.clone()}</p> }.into_any(),
                |html| view! { <div class="markdown" inner_html=html></div> }.into_any(),
            )}
        </div>
    }
}

fn badge_tone(value: &str) -> &'static str {
    match value {
        "done" => "success",
        "failed" | "approval" => "warn",
        _ => "neutral",
    }
}

fn workflow_request(key: &str, value: Value) -> Result<Request, String> {
    let owner = key
        .strip_prefix("minarbot:workflow-command:")
        .unwrap_or_default();
    let path = text(&value["path"]);
    let body = &value["body"];
    let kind = text(&value["kind"]);
    let workflow = &value["workflow"];
    let workflow_id = text(&workflow["id"]);
    let workflow_ok =
        workflow["agent"] == owner && valid_id(workflow_id) && number(&workflow["revision"]) > 0;
    let parts = path.split('/').collect::<Vec<_>>();
    let valid = match parts.as_slice() {
        ["", "api", "agents", id, "workflows"] => {
            kind == "save" && *id == owner && workflow.is_null() && body["revision"] == 0
        }
        ["", "api", "workflows", id, "edit"] => {
            kind == "save"
                && workflow_ok
                && *id == workflow_id
                && body["revision"] == workflow["revision"]
        }
        ["", "api", "workflows", id, action] => {
            workflow_ok
                && *id == workflow_id
                && *action == kind
                && matches!(kind, "delete" | "run" | "schedule" | "webhook" | "restore")
        }
        ["", "api", "workflow-runs", id, action] => {
            workflow_ok
                && valid_id(id)
                && *action == kind
                && matches!(kind, "approve" | "cancel" | "retry")
        }
        _ => false,
    };
    if !valid
        || !valid_id(owner)
        || body["revision"]
            .as_i64()
            .is_none_or(|revision| revision < 0)
    {
        return Err("Saved workflow request could not be read.".into());
    }
    Request::restore(path.to_owned(), body.clone(), value)
}

fn journal(owner: &str) -> Journal {
    Journal::new(
        format!("minarbot:workflow-command:{owner}"),
        workflow_request,
    )
}

fn command(path: String, body: Value, kind: &str, workflow: &Value) -> Request {
    Request::new(path.clone(), body, |body| {
        let mut record = json!({"path":path,"body":body,"kind":kind});
        if !workflow.is_null() {
            record["workflow"] = workflow.clone();
        }
        record
    })
}

fn action(workflow: &Value, kind: &str, body: Value) -> Request {
    command(
        format!("/api/workflows/{}/{kind}", text(&workflow["id"])),
        body,
        kind,
        workflow,
    )
}

#[derive(Clone)]
enum Screen {
    List,
    Owner,
    Editor { owner: String, workflow: Value },
    Detail(Value),
    Inbox,
}

#[leptos::wasm_split::wasm_split(workflow_editor, wasm_split_path = leptos::wasm_split, fallible)]
fn workflow_editor(
    owner: String,
    workflow: Value,
    back: Callback<()>,
    select: Callback<Value>,
    confirm: kit::Confirm,
) -> crate::lazy_view::ChunkView {
    Ok(view! { <Editor owner workflow back select confirm /> }.into_any())
}

#[leptos::wasm_split::wasm_split(workflow_detail, wasm_split_path = leptos::wasm_split, fallible)]
fn workflow_detail(
    workflow: Value,
    back: Callback<()>,
    editor: Callback<Value>,
    confirm: kit::Confirm,
    inline: bool,
) -> crate::lazy_view::ChunkView {
    Ok(view! { <Detail initial=workflow back editor confirm inline /> }.into_any())
}

pub(crate) fn workflows_page(active: Signal<bool>) -> impl IntoView {
    let search = window().location().search().unwrap_or_default();
    if let Some((owner, back)) = crate::lazy_route::workflow_scope(&format!("/workflows{search}")) {
        return agent_library(owner, format!("{back}?ui=leptos"), None).into_any();
    }
    let screen = RwSignal::new(Screen::List);
    let restore_error = RwSignal::new(String::new());
    let deleted = RwSignal::new(false);
    let cursors = RwSignal::new(Vec::<String>::new());
    let query = ui::query::<Value>(move || {
        let deleted = if deleted.get_untracked() { "1" } else { "0" };
        let cursor = cursors.with_untracked(|cursors| cursors.last().cloned());
        match cursor {
            Some(cursor) => url(
                "/api/workflows",
                &[("deleted", deleted), ("after", &cursor)],
            ),
            None => format!("/api/workflows?deleted={deleted}"),
        }
    });
    poll(
        Callback::new(move |_| {
            if active.get_untracked() && matches!(screen.get_untracked(), Screen::List) {
                query.reload.run(());
            }
        }),
        5000,
    );
    let refresh = Callback::new(move |_| query.reload.run(()));
    let choose = RwSignal::new(String::new());
    let (confirm, confirmation) = kit::confirmation();
    let activation = RwSignal::new(0_u64);
    Effect::watch(
        move || active.get(),
        move |is_active, previous, _| {
            activation.update(|version| *version += 1);
            let version = activation.get_untracked();
            if *is_active {
                let returning = return_state();
                let id = text(&returning["workflow"]);
                if valid_id(id) && valid_id(text(&returning["run"])) {
                    let path = format!("/api/workflows/{id}");
                    spawn_local(async move {
                        let result = api::read::<Value>(&path).await;
                        if activation.try_get_untracked() != Some(version) {
                            return;
                        }
                        match result {
                            Ok(workflow) => screen.set(Screen::Detail(workflow)),
                            Err(error) => restore_error.set(error),
                        }
                    });
                }
                if previous.is_some() {
                    request_animation_frame(move || {
                        if activation.try_get_untracked() != Some(version) {
                            return;
                        }
                        refresh.run(());
                        if let Ok(Some(heading)) = document()
                            .query_selector(".automation-workflows-route:not([hidden]) h1")
                        {
                            if let Ok(heading) = heading.dyn_into::<web_sys::HtmlElement>() {
                                let _ = heading.focus();
                            }
                        }
                    });
                }
            } else {
                query.invalidate();
                confirm.set(None);
                if deleted.get_untracked() || !cursors.with_untracked(Vec::is_empty) {
                    deleted.set(false);
                    cursors.set(Vec::new());
                    query.data.set(None);
                }
                choose.set(String::new());
                restore_error.set(String::new());
                if !matches!(screen.get_untracked(), Screen::List) {
                    screen.set(Screen::List);
                }
            }
        },
        true,
    );
    let available = Signal::derive(directory);
    let owner = Signal::derive(move || {
        let id = choose.get();
        available.with(|agents| {
            if agents.iter().any(|(agent, _)| *agent == id) {
                id
            } else {
                agents.first().map(|(id, _)| id.clone()).unwrap_or_default()
            }
        })
    });
    let back = Callback::new(move |_| {
        if return_state()["workflow"].is_string() {
            if let Ok(history) = window().history() {
                let _ = history.replace_state_with_url(&JsValue::NULL, "", None);
            }
        }
        screen.set(Screen::List);
        refresh.run(());
    });
    let select = Callback::new(move |workflow: Value| screen.set(Screen::Detail(workflow)));
    let editor = Callback::new(move |workflow: Value| {
        let owner = workflow["agent"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| owner.get_untracked());
        screen.set(Screen::Editor { owner, workflow });
    });
    let navigation = move |older| {
        if older {
            if let Some(next) = query.data.with_untracked(|data| {
                data.as_ref()
                    .and_then(|value| value["next_after"].as_str())
                    .map(str::to_owned)
            }) {
                cursors.update(|cursors| cursors.push(next));
            }
        } else {
            cursors.update(|cursors| {
                cursors.pop();
            });
        }
        query.data.set(None);
        refresh.run(());
    };
    route_page(view! {
        {move || match screen.get() {
            Screen::List | Screen::Owner => {
                let menu = vec![
                    kit::MenuItem { label:"Results inbox".into(), danger:false, action:Callback::new(move |_| screen.set(Screen::Inbox)) },
                    kit::MenuItem { label:if deleted.get() {"Hide deleted".into()} else {"Show deleted".into()}, danger:false,
                        action:Callback::new(move |_| {
                            deleted.update(|value| *value = !*value);
                            cursors.set(Vec::new()); query.data.set(None); refresh.run(());
                        }) },
                ];
                view! { <div class="k-page bs-root workflow-index">
                    <PageHead title="Workflows" actions=kit::content(move || view! {
                        <kit::Menu name="Workflow options".into() items=menu>{kit::icon(kit::icons().more)}</kit::Menu>
                        <Show when=move || !available.with(Vec::is_empty)>
                            <button type="button" class="k-btn k-btn--md k-btn--primary" on:click=move |_| screen.set(Screen::Owner)>
                                {kit::icon(kit::icons().plus)}"New workflow"</button>
                        </Show>
                    }) />
                    <ReadError query />
                    {kit::error(restore_error)}
                    <div class="k-stack">
                        {move || match query.data.get() {
                            Some(value) if rows(&value["workflows"]).is_empty() => kit::empty("No workflows yet",String::new()).into_any(),
                            Some(value) => view! { <kit::Card>{rows(&value["workflows"]).iter().map(|workflow| {
                                let action = workflow.clone();
                                let name = text(&workflow["name"]).to_owned();
                                let owner_name = text(&workflow["owner_name"]).to_owned();
                                let revision = number(&workflow["revision"]);
                                let count = rows(&workflow["graph"]["nodes"]).len();
                                let owner = text(&workflow["agent"]).to_owned();
                                view! {
                                    <ui::Row title=name icon="workflow" click=Callback::new(move |_| select.run(action.clone()))
                                        disabled=Signal::derive(move || !available.with(|agents| agents.iter().any(|(id,_)| *id==owner)))
                                        meta=kit::content(move || view! { <span>{owner_name}</span>{kit::badge("neutral",format!("Version {revision}"))}<span>{format!("{count} steps")}</span> }) />
                                }
                            }).collect_view()}</kit::Card> }.into_any(),
                            None if !query.error.with(String::is_empty) => ().into_any(),
                            None => kit::skeleton(4,"Loading workflow list...").into_any(),
                        }}
                        <div class="k-pager" hidden=move || cursors.with(Vec::is_empty) && query.data.with(|value| value.as_ref().is_none_or(|value| value["next_after"].is_null()))>
                            <button class="k-icon-btn" type="button" aria-label="Previous workflows" disabled=move || cursors.with(Vec::is_empty)
                                on:click=move |_| navigation(false)>{kit::icon(kit::icons().left)}</button>
                            <button class="k-icon-btn" type="button" aria-label="Next workflows" disabled=move || query.data.with(|value| value.as_ref().is_none_or(|value| value["next_after"].is_null()))
                                on:click=move |_| navigation(true)>{kit::icon(kit::icons().right)}</button>
                        </div>
                    </div>
                    <kit::Dialog id="workflow-owner" title=kit::text("New workflow")
                        open=Signal::derive(move || matches!(screen.get(),Screen::Owner)) on_close=move || screen.set(Screen::List)
                        footer=kit::content(move || view! {
                            <kit::Button label=kit::text("Cancel") on_click=move || screen.set(Screen::List) />
                            <kit::Button label=kit::text("Continue") variant="primary" disabled=Signal::derive(move || owner.with(String::is_empty))
                                on_click=move || editor.run(Value::Null) />
                        })>
                        <kit::Field id="workflow-starting-agent" label="Starting agent"><Select name="workflow-starting-agent" value=owner
                            options=available change=Callback::new(move |id| choose.set(id)) /></kit::Field>
                    </kit::Dialog>
                    {move || available.get().into_iter().filter_map(|(owner,_)| {
                        let key = format!("minarbot:workflow-command:{owner}");
                        storage().ok().and_then(|store| store.get_item(&key).ok().flatten()).map(|_| {
                            view! { <Recovery journal=journal(&owner) confirm done=Callback::new(move |_| refresh.run(())) /> }
                        })
                    }).collect_view()}
                </div> }.into_any()
            }
            Screen::Editor { owner,workflow } => crate::lazy_view::load("workflow editor", move || {
                workflow_editor(owner.clone(), workflow.clone(), back, select, confirm)
            }).into_any(),
            Screen::Detail(workflow) => crate::lazy_view::load("workflow details", move || {
                workflow_detail(workflow.clone(), back, editor, confirm, false)
            }).into_any(),
            Screen::Inbox => inbox(back).into_any(),
        }}
        {confirmation}
    }).into_any()
}

pub(crate) fn agent_library(
    owner: String,
    back: String,
    library: Option<js_sys::Promise>,
) -> impl IntoView {
    let recovery = journal(&owner);
    let initial = recovery.pending.with_untracked(|pending| {
        pending
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .map(|pending| {
                let workflow = pending.record["workflow"].clone();
                if pending.record["kind"] == "save" {
                    Screen::Editor {
                        owner: owner.clone(),
                        workflow,
                    }
                } else {
                    Screen::Detail(workflow)
                }
            })
            .unwrap_or(Screen::List)
    });
    let screen = RwSignal::new(initial);
    let owner = StoredValue::new(owner);
    let cursors = RwSignal::new(Vec::<String>::new());
    let query = ui::query_with_prefetch::<Value>(
        move || {
            let path = owner.with_value(|owner| format!("/api/agents/{owner}/workflows"));
            cursors.with_untracked(|cursors| {
                cursors
                    .last()
                    .map_or_else(|| path.clone(), |after| url(&path, &[("after", after)]))
            })
        },
        library,
    );
    let cached = LIBRARY_PAGE.with_borrow_mut(|slot| match slot.as_ref() {
        Some((agent, page)) if owner.with_value(|owner| owner == agent) => Some(page.clone()),
        _ => {
            *slot = None;
            None
        }
    });
    query.data.set(cached);
    Effect::new(move |_| {
        if query.busy.get() || !cursors.with(Vec::is_empty) {
            return;
        }
        if !query.error.with(String::is_empty) {
            LIBRARY_PAGE.with_borrow_mut(|slot| *slot = None);
            query.data.set(None);
            return;
        }
        let page = query.data.get_untracked();
        LIBRARY_PAGE.with_borrow_mut(|slot| *slot = page.map(|page| (owner.get_value(), page)));
    });
    let refresh = Callback::new(move |()| query.reload.run(()));
    let library = Callback::new(move |()| {
        screen.set(Screen::List);
        refresh.run(());
    });
    let open = Callback::new(move |workflow| screen.set(Screen::Detail(workflow)));
    let edit = Callback::new(move |workflow| {
        screen.set(Screen::Editor {
            owner: owner.get_value(),
            workflow,
        })
    });
    let next = move || {
        query.data.with(|data| {
            data.as_ref()
                .and_then(|data| data["next_after"].as_str().map(str::to_owned))
        })
    };
    let page = move |older| {
        if older {
            if let Some(after) = next() {
                cursors.update(|cursors| cursors.push(after));
            }
        } else {
            cursors.update(|cursors| {
                cursors.pop();
            });
        }
        query.data.set(None);
        refresh.run(());
    };
    let (confirm, confirmation) = kit::confirmation();
    route_page(view! {
        {move || match screen.get() {
            Screen::Editor { owner, workflow } => {
                let select = Callback::new(move |_| library.run(()));
                crate::lazy_view::load("workflow editor", move || {
                    workflow_editor(owner.clone(), workflow.clone(), library, select, confirm)
                }).into_any()
            },
            Screen::Detail(workflow) => crate::lazy_view::load("workflow details", move || {
                workflow_detail(workflow.clone(), library, edit, confirm, true)
            }).into_any(),
            _ => {
                let href = back.clone();
                view! { <div class="k-page bs-root workflow-page">
                    <PageHead title="Workflows" actions=kit::content(move || view! {
                        <a class="k-btn k-btn--md k-btn--ghost" data-scoped-workflow-back="" href=href>"Back"</a>
                        <button type="button" class="k-icon-btn" aria-label="Reload workflow" on:click=move |_| refresh.run(())>{kit::icon(kit::icons().refresh)}</button>
                        <kit::Button label=kit::text("Create workflow") variant="primary" disabled=Signal::derive(move || recovery.locked())
                            on_click=move || edit.run(Value::Null) />
                    }) />
                    <Recovery journal=recovery confirm done=Callback::new(move |_| refresh.run(())) />
                    <ReadError query />
                    <kit::Section title="Workflow library">
                        {move || match query.data.get() {
                            Some(data) if rows(&data["workflows"]).is_empty() => kit::empty("No workflows on this page yet", String::new()).into_any(),
                            Some(data) => view! { <kit::Card>{rows(&data["workflows"]).iter()
                                .filter(|row| owner.with_value(|owner| row["agent"] == *owner))
                                .map(|row| {
                                    let selected = row.clone();
                                    let editable = row.clone();
                                    let name = text(&row["name"]).to_owned();
                                    let version = format!("Version {}", number(&row["revision"]));
                                    let steps = format!("{} steps", rows(&row["graph"]["nodes"]).len());
                                    view! { <ui::Row title=name.clone() click=Callback::new(move |()| open.run(selected.clone()))
                                        meta=kit::content(move || view! { {kit::badge("neutral",version)}<span>{steps}</span> })
                                        trailing=kit::content(move || view! {
                                            <kit::Menu name=format!("{name} actions") disabled=Signal::derive(move || recovery.locked() || query.busy.get())
                                                items=vec![kit::MenuItem { label:format!("Edit {name}"), danger:false, action:Callback::new(move |_| edit.run(editable.clone())) }]>
                                                {kit::icon(kit::icons().more)}
                                            </kit::Menu>
                                        }) />
                                    }
                                }).collect_view()}</kit::Card> }.into_any(),
                            None if query.error.with(String::is_empty) => kit::skeleton(4, "Loading workflow...").into_any(),
                            None => ().into_any(),
                        }}
                        <div class="k-pager" hidden=move || cursors.with(Vec::is_empty) && next().is_none()>
                            <button class="k-icon-btn" type="button" aria-label="Previous workflows" disabled=move || cursors.with(Vec::is_empty)
                                on:click=move |_| page(false)>{kit::icon(kit::icons().left)}</button>
                            <button class="k-icon-btn" type="button" aria-label="Next workflows" disabled=move || next().is_none()
                                on:click=move |_| page(true)>{kit::icon(kit::icons().right)}</button>
                        </div>
                    </kit::Section>
                </div> }.into_any()
            }
        }}
        {confirmation}
    })
}

fn return_state() -> Value {
    window()
        .history()
        .and_then(|history| history.state())
        .ok()
        .and_then(|state| state.as_string())
        .and_then(|state| serde_json::from_str(&state).ok())
        .unwrap_or_default()
}

#[derive(Clone)]
struct History {
    graph: Value,
    past: Vec<Value>,
    future: Vec<Value>,
}

fn edit_graph(history: RwSignal<History>, change: impl FnOnce(&mut Value)) {
    history.update(|history| {
        let previous = history.graph.clone();
        change(&mut history.graph);
        if history.graph != previous {
            if history.past.len() == 100 {
                history.past.remove(0);
            }
            history.past.push(previous);
            history.future.clear();
        }
    });
}

fn set_node(history: RwSignal<History>, node: &str, change: impl FnOnce(&mut Value)) {
    edit_graph(history, |graph| {
        if let Some(value) = graph["nodes"]
            .as_array_mut()
            .and_then(|nodes| nodes.iter_mut().find(|value| id(value) == node))
        {
            change(value);
        }
    });
}

fn default_graph(owner: &str) -> Value {
    json!({"entry":"work","maxSteps":20,"nodes":[
        {"id":"work","kind":"agent","agent":owner,"prompt":"","next":"review"},
        {"id":"review","kind":"approval","prompt":"Approve the result to complete the workflow?","yes":"output"},
        {"id":"output","kind":"output"},
    ]})
}

fn valid_step_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn graph_shape(graph: &Value) -> bool {
    let nodes = rows(&graph["nodes"]);
    if !graph.is_object() || nodes.is_empty() || nodes.len() > 64 {
        return false;
    }
    let ids = nodes
        .iter()
        .map(id)
        .collect::<std::collections::BTreeSet<_>>();
    ids.len() == nodes.len()
        && ids.iter().all(|id| valid_step_id(id))
        && graph.get("positions").is_none_or(|value| {
            value.as_object().is_some_and(|positions| {
                positions.iter().all(|(id, point)| {
                    ids.contains(id.as_str())
                        && ["x", "y"].into_iter().all(|axis| {
                            point[axis]
                                .as_f64()
                                .is_some_and(|at| (64.0..=20000.0).contains(&at))
                        })
                })
            })
        })
}

fn complete_graph(graph: &Value) -> bool {
    let nodes = rows(&graph["nodes"]);
    let known = |id| nodes.iter().any(|node| graph::id(node) == id);
    graph_shape(graph)
        && known(text(&graph["entry"]))
        && (1..=1000).contains(&number(&graph["maxSteps"]))
        && nodes.iter().all(|node| {
            let kind = text(&node["kind"]);
            KINDS.iter().any(|(value, _)| *value == kind)
                && edges(node)
                    .iter()
                    .all(|(key, _, _)| known(text(&node[*key])))
                && match kind {
                    "agent" => {
                        valid_id(text(&node["agent"])) && !text(&node["prompt"]).trim().is_empty()
                    }
                    "model" => {
                        !text(&node["model"]).trim().is_empty()
                            && !text(&node["prompt"]).trim().is_empty()
                    }
                    "approval" => !text(&node["prompt"]).trim().is_empty(),
                    "wait" => {
                        !text(&node["event"]).trim().is_empty()
                            && (1..=604800).contains(&number(&node["timeoutSeconds"]))
                    }
                    "parallel" => {
                        (2..=8).contains(&rows(&node["branches"]).len())
                            && rows(&node["branches"]).iter().all(|branch| {
                                !text(&branch["prompt"]).trim().is_empty()
                                    && match text(&branch["kind"]) {
                                        "agent" => valid_id(text(&branch["agent"])),
                                        "model" => !text(&branch["model"]).trim().is_empty(),
                                        _ => false,
                                    }
                            })
                    }
                    "condition" => match text(&node["operator"]) {
                        "model" => {
                            !text(&node["model"]).trim().is_empty()
                                && !text(&node["value"]).trim().is_empty()
                        }
                        "equals" | "contains" => true,
                        _ => false,
                    },
                    "output" => {
                        node.get("channel")
                            .is_none_or(|value| valid_id(text(value)))
                            && node.get("discordChannel").is_none_or(|value| {
                                text(value).parse::<u64>().is_ok_and(|id| id > 0)
                            })
                    }
                    _ => true,
                }
        })
        && graph.get("trigger").is_none_or(|trigger| {
            text(&trigger["channel"])
                .parse::<u64>()
                .is_ok_and(|id| id > 0)
        })
}

fn focus_node(attribute: &str, id: &str) {
    if !valid_step_id(id) {
        return;
    }
    if let Ok(Some(node)) = document().query_selector(&format!("[{attribute}='{id}']")) {
        if let Ok(node) = node.dyn_into::<web_sys::HtmlElement>() {
            let _ = node.focus();
            let options = web_sys::ScrollIntoViewOptions::new();
            options.set_block(web_sys::ScrollLogicalPosition::Nearest);
            options.set_inline(web_sys::ScrollLogicalPosition::Nearest);
            node.scroll_into_view_with_scroll_into_view_options(&options);
        }
    }
}

fn save_file(name: &str, body: &Value) -> Result<(), String> {
    let bytes = serde_json::to_string_pretty(body).map_err(|error| error.to_string())?;
    let parts = js_sys::Array::of1(&JsValue::from_str(&bytes));
    let blob =
        web_sys::Blob::new_with_str_sequence(&parts).map_err(|_| "Export could not be created.")?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)
        .map_err(|_| "Export could not be created.")?;
    let anchor = document()
        .create_element("a")
        .ok()
        .and_then(|node| node.dyn_into::<web_sys::HtmlAnchorElement>().ok());
    if let Some(anchor) = anchor {
        anchor.set_href(&url);
        anchor.set_download(name);
        anchor.click();
    }
    let _ = web_sys::Url::revoke_object_url(&url);
    Ok(())
}

#[component]
fn Editor(
    owner: String,
    workflow: Value,
    back: Callback<()>,
    select: Callback<Value>,
    confirm: kit::Confirm,
) -> impl IntoView {
    if use_context::<ReadSignal<Option<Result<crate::shell::Directory, String>>>>().is_none() {
        let directory = RwSignal::new(None::<Result<crate::shell::Directory, String>>);
        provide_context(directory.read_only());
        spawn_local(async move {
            directory.try_set(Some(api::read("/api/agents").await));
        });
    }
    let new = workflow.is_null();
    let current = RwSignal::new(workflow.clone());
    let owner = StoredValue::new(owner);
    let journal = owner.with_value(|owner| journal(owner));
    let key = owner.with_value(|owner| {
        format!(
            "minarbot:workflow-draft:{owner}:{}",
            if new { "new" } else { text(&workflow["id"]) }
        )
    });
    let key = StoredValue::new(key);
    let draft = storage().and_then(|storage| {
        let Some(raw) = storage
            .get_item(&key.get_value())
            .map_err(|_| "Browser draft could not be read.")?
        else {
            return Ok(None);
        };
        if raw.len() > 2 * 1024 * 1024 {
            return Err("Browser draft could not be read.".into());
        }
        let value = serde_json::from_str::<Value>(&raw).ok().filter(|value| {
            value["name"].is_string()
                && value["revision"].as_i64().is_some()
                && graph_shape(&value["graph"])
        });
        value
            .map(Some)
            .ok_or_else(|| "Browser draft could not be read.".into())
    });
    let bad_draft = RwSignal::new(draft.is_err());
    let revision = number(&workflow["revision"]);
    let conflict = RwSignal::new(
        draft
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .is_some_and(|draft| number(&draft["revision"]) != revision),
    );
    let draft = draft.ok().flatten().unwrap_or_else(|| {
        if new {
            json!({"name":"","graph":owner.with_value(|owner| default_graph(owner)),"revision":0})
        } else {
            workflow
        }
    });
    let name = RwSignal::new(text(&draft["name"]).to_owned());
    let history = RwSignal::new(History {
        graph: draft["graph"].clone(),
        past: Vec::new(),
        future: Vec::new(),
    });
    let graph = Signal::derive(move || history.with(|history| history.graph.clone()));
    let nodes = Memo::new(move |_| {
        graph.with(|graph| {
            rows(&graph["nodes"])
                .iter()
                .map(|node| id(node).to_owned())
                .collect::<Vec<_>>()
        })
    });
    let selected = RwSignal::new(text(&draft["graph"]["entry"]).to_owned());
    let import = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let action_state =
        Signal::derive(move || journal.locked() || bad_draft.get() || conflict.get());
    let dirty = move || {
        current.with(|current| {
            current["name"].as_str().unwrap_or_default() != name.get()
                || if current.is_null() {
                    graph.get() != owner.with_value(|owner| default_graph(owner))
                } else {
                    current["graph"] != graph.get()
                }
        })
    };
    Effect::new(move |_| {
        if bad_draft.get()
            || conflict.get()
            || journal.pending.with(|value| !matches!(value, Ok(None)))
        {
            return;
        }
        let dirty = dirty();
        let snapshot = json!({"name":name.get(),"graph":graph.get(),"revision":current.with(|workflow| number(&workflow["revision"]))});
        let result = storage().and_then(|storage| {
            let key = key.get_value();
            if dirty {
                storage.set_item(&key, &snapshot.to_string())
            } else {
                storage.remove_item(&key)
            }
            .map_err(|_| "Draft has not been saved in the browser.".to_owned())
        });
        if let Err(message) = result {
            error.try_set(message);
        }
    });
    let saved = move |value: Value| {
        if let Ok(storage) = storage() {
            let _ = storage.remove_item(&key.get_value());
        }
        value
    };
    let done = Callback::new(move |value| select.run(saved(value)));
    let autosaved = Callback::new(move |value| current.set(saved(value)));
    let request = move |name: String, graph: Value, automatic: bool| {
        let workflow = current.get_untracked();
        let path = if new {
            owner.with_value(|owner| format!("/api/agents/{owner}/workflows"))
        } else {
            format!("/api/workflows/{}/edit", text(&workflow["id"]))
        };
        let body = json!({"name":name.trim(),"graph":graph,"revision":workflow["revision"].as_i64().unwrap_or(0)});
        let mut request = command(path, body, "save", &workflow);
        if automatic {
            request.record["autosave"] = json!(true);
        }
        request
    };
    let last_attempt = RwSignal::new(Value::Null);
    Effect::new(move |timer: Option<Option<TimeoutHandle>>| {
        if let Some(Some(timer)) = timer {
            timer.clear();
        }
        if new
            || action_state.get()
            || !dirty()
            || journal.error.with(|error| error.contains("changed"))
        {
            return None;
        }
        let graph = graph.get();
        let name = name.get();
        if name.trim().is_empty() || !complete_graph(&graph) {
            return None;
        }
        let command = request(name, graph, true);
        let mut stamp = command.body.clone();
        if let Some(stamp) = stamp.as_object_mut() {
            stamp.remove("id");
        }
        if last_attempt.with_untracked(|last| *last == stamp) {
            return None;
        }
        set_timeout_with_handle(
            move || {
                if action_state.try_get_untracked() != Some(false) {
                    return;
                }
                last_attempt.set(stamp);
                journal.submit(command, autosaved);
            },
            Duration::from_millis(400),
        )
        .ok()
    });
    let submit = move |event: web_sys::SubmitEvent| {
        event.prevent_default();
        if action_state.get_untracked() {
            return;
        }
        let name = name.get_untracked();
        let graph = graph.get_untracked();
        if !complete_graph(&graph) || name.trim().is_empty() {
            error.set("Complete the workflow name, steps and connections before saving.".into());
            return;
        }
        error.set(String::new());
        journal.submit(request(name, graph, false), done);
    };
    let leave = move || {
        if dirty() {
            kit::ask(
                Some(confirm),
                "Leave editor?",
                "Unsaved changes are stored in this browser.",
                "Leave",
                true,
                move || back.run(()),
            );
        } else {
            back.run(());
        }
    };
    let rearrange = move |_| {
        edit_graph(history, |graph| {
            if let Some(graph) = graph.as_object_mut() {
                graph.remove("positions");
            }
        })
    };
    let undo = move |redo| {
        history.update(|history| {
            let (from, to) = if redo {
                (&mut history.future, &mut history.past)
            } else {
                (&mut history.past, &mut history.future)
            };
            if let Some(graph) = from.pop() {
                to.push(std::mem::replace(&mut history.graph, graph));
            }
        })
    };
    let load = move || {
        let id = current.with_untracked(|workflow| text(&workflow["id"]).to_owned());
        if id.is_empty() {
            return;
        }
        spawn_local(async move {
            match api::read::<Value>(&format!("/api/workflows/{id}")).await {
                Ok(workflow) if current.try_get_untracked().is_some() => {
                    let next_name = text(&workflow["name"]).to_owned();
                    edit_graph(history, |graph| *graph = workflow["graph"].clone());
                    current.set(workflow);
                    name.set(next_name);
                    conflict.set(false);
                    journal.error.set(String::new());
                }
                Err(message) => {
                    error.try_set(message);
                }
                _ => {}
            }
        });
    };
    let targets = Signal::derive(move || {
        let mut targets = options(&[("", "Choose step")]);
        targets.extend(nodes.get().into_iter().map(|id| (id.clone(), id)));
        targets
    });
    let trigger = Signal::derive(move || {
        if graph.with(|graph| graph["trigger"]["kind"] == "discord") {
            "discord".into()
        } else {
            "manual".into()
        }
    });
    let loop_nodes = Memo::new(move |_| graph.with(graph::cycles));
    let channels = ui::query::<Value>(|| "/api/channels".into());
    view! {
        <div class="k-page bs-root workflow-page">
            <PageHead title=if new {"New workflow"} else {"Edit workflow"} description="Arrange steps and their next destinations. Approvals wait for your decision. Saved changes apply only to new runs."
                actions=kit::content(move || view! {
                    <kit::Button label=kit::text("Back") variant="ghost" on_click=leave />
                    <button type="submit" class="k-btn k-btn--md k-btn--primary" form="workflow-editor" disabled=action_state>"Save workflow"</button>
                }) />
            <Recovery journal done confirm />
            {kit::error(error)}
            {(!new).then(|| view! {
                <p class="k-field__hint" role="status">{move || {
                    if action_state.get() { "Saving awaits confirmation or recovery.".into() }
                    else if !dirty() { format!("Saved, version {}.", current.with(|value|number(&value["revision"]))) }
                    else { "Changes save automatically after you stop typing.".into() }
                }}</p>
            })}
            <Show when=move || bad_draft.get()>
                <kit::Alert tone="error" actions=kit::content(move || view! {
                    <kit::Button label=kit::text("Discard browser draft") on_click=move || kit::ask(Some(confirm),"Discard the unreadable browser draft?","","Discard draft",true,move || {
                        match storage().and_then(|storage| storage.remove_item(&key.get_value()).map_err(|_| "Browser draft could not be cleared.".to_owned())) {
                            Ok(()) => bad_draft.set(false), Err(message) => error.set(message),
                        }
                    }) />
                })>"Browser draft could not be read."</kit::Alert>
            </Show>
            <Show when=move || conflict.get() || journal.error.with(|error| error.contains("changed"))>
                <kit::Alert tone="warn" actions=kit::content(move || view! {
                    <kit::Button label=kit::text("Load latest version") on_click=load />
                })>"Draft is from an older revision. Load the latest version before saving."</kit::Alert>
            </Show>
            <form id="workflow-editor" class="k-fieldset" on:submit=submit>
                <fieldset class="k-fieldset" disabled=action_state>
                    <kit::Field id="workflow-name" label="Workflow name"><input id="workflow-name" class="k-input" maxlength="80" required
                        prop:value=name on:input=move |event| name.set(event_target_value(&event)) /></kit::Field>
                    <kit::Field id="workflow-trigger" label="Trigger"><Select name="workflow-trigger" value=trigger
                        options=Signal::stored(options(&[("manual","Manual"),("discord","Discord message")]))
                        change=Callback::new(move |value: String| edit_graph(history,|graph| {
                            if value=="discord" { graph["trigger"]=json!({"kind":"discord","channel":""}); }
                            else if let Some(graph)=graph.as_object_mut() { graph.remove("trigger"); }
                        })) /></kit::Field>
                    <Show when=move || trigger.get()=="discord">
                        <kit::Field id="workflow-discord" label="Discord trigger channel ID"><input id="workflow-discord" class="k-input" required maxlength="20" pattern="[1-9][0-9]{0,19}"
                            prop:value=move || graph.with(|graph| text(&graph["trigger"]["channel"]).to_owned())
                            on:input=move |event| edit_graph(history,|graph| graph["trigger"]["channel"]=json!(event_target_value(&event))) /></kit::Field>
                    </Show>
                    <kit::Field id="workflow-entry" label="Starting step">
                        <select id="workflow-entry" class="k-input" required
                            on:change=move |event| edit_graph(history,|graph| graph["entry"]=json!(event_target_value(&event)))>
                            {move || targets.get().into_iter().map(|(id, name)| {
                                let entry = id.clone();
                                view! { <option value=id prop:selected=move || graph.with(|graph| graph["entry"]==entry)>{name}</option> }
                            }).collect_view()}
                        </select>
                    </kit::Field>
                    <kit::Field id="workflow-limit" label="Step limit"><input id="workflow-limit" class="k-input" type="number" min="1" max="1000" required
                        aria-describedby="workflow-limit-hint"
                        prop:value=move || graph.with(|graph| number(&graph["maxSteps"]).to_string())
                        on:input=move |event| edit_graph(history,|graph| graph["maxSteps"]=json!(event_target_value(&event).parse::<i64>().unwrap_or_default())) />
                        <div id="workflow-limit-hint" class="k-field__hint">"Loops stop with a failed status when this limit is reached."</div>
                    </kit::Field>
                    <div class="k-inline">
                        <button type="button" class="k-btn k-btn--md k-btn--secondary" disabled=move || history.with(|history| history.past.is_empty())
                            on:click=move |_| undo(false)><ui::Icon name="undo" />"Undo graph"</button>
                        <button type="button" class="k-btn k-btn--md k-btn--secondary" disabled=move || history.with(|history| history.future.is_empty())
                            on:click=move |_| undo(true)><ui::Icon name="redo" />"Redo graph"</button>
                        <button type="button" class="k-btn k-btn--md k-btn--secondary" disabled=move || graph.with(|graph| graph.get("positions").is_none()) on:click=rearrange>"Auto-arrange"</button>
                    </div>
                    <p class="k-field__hint">"Choose a step in the graph to open its settings. Undo and redo apply to graph changes, not the workflow name."</p>
                    <Show when=move || !loop_nodes.with(|nodes| nodes.is_empty())>
                        <kit::Alert tone="warn">{move || format!("Unconditional loop: {}. The loop stops at the step limit.",loop_nodes.with(|nodes| nodes.iter().cloned().collect::<Vec<_>>().join(", ")))}</kit::Alert>
                    </Show>
                    <Graph graph selected locked=action_state
                        clicked=Callback::new(move |node: String| { selected.set(node.clone()); focus_node("data-workflow-settings",&node); })
                        moved=Callback::new(move |(node,x,y): (String,i64,i64)| {
                            selected.set(node.clone());
                            edit_graph(history,|graph| graph["positions"][node]=json!({"x":x,"y":y}));
                        }) />
                    <For each=move || nodes.get() key=String::clone children=move |node| {
                        view! { <NodeSettings node history graph targets selected owner channels locked=action_state /> }
                    } />
                    <div class="k-inline">
                        <kit::Button label=kit::text("Add step") disabled=Signal::derive(move || nodes.with(|nodes| nodes.len()>=64))
                            on_click=move || edit_graph(history,|graph| {
                                if let Some(nodes)=graph["nodes"].as_array_mut() {
                                    let mut number=nodes.len()+1;
                                    while nodes.iter().any(|node| id(node)==format!("step{number}")) { number+=1; }
                                    nodes.push(json!({"id":format!("step{number}"),"kind":"output"}));
                                }
                            }) />
                        <kit::Button label=kit::text("Import graph") on_click=move || import.set(true) />
                        <kit::Button label=kit::text("Export graph") on_click=move || {
                            if let Err(message)=save_file("minarbot-workflow.json",&graph.get_untracked()) { error.set(message); }
                        } />
                    </div>
                </fieldset>
            </form>
            <Show when=move || import.get()><ImportGraph history error close=Callback::new(move |_| import.set(false)) /></Show>
        </div>
    }
}

#[component]
fn ImportGraph(
    history: RwSignal<History>,
    error: RwSignal<String>,
    close: Callback<()>,
) -> impl IntoView {
    let input = RwSignal::new(String::new());
    let parse_error = RwSignal::new(String::new());
    view! {
        <kit::Dialog id="workflow-import" title=kit::text("Import workflow graph") open=Signal::stored(true) on_close=move || close.run(())
            footer=kit::content(move || view! {
                <kit::Button label=kit::text("Cancel") on_click=move || close.run(()) />
                <kit::Button label=kit::text("Import graph") variant="primary" on_click=move || {
                    let raw=input.get_untracked();
                    let graph=(raw.len() <= 2 * 1024 * 1024).then(|| serde_json::from_str::<Value>(&raw).ok())
                        .flatten().filter(graph_shape);
                    if let Some(graph)=graph { edit_graph(history,|value| *value=graph); error.set(String::new()); close.run(()); }
                    else { parse_error.set("Choose a workflow JSON graph containing 1 to 64 uniquely named steps.".into()); }
                } />
            })>
            <kit::Field id="workflow-import-input" label="Graph JSON"><textarea id="workflow-import-input" class="k-input k-textarea" rows="8"
                prop:value=input on:input=move |event| input.set(event_target_value(&event))></textarea></kit::Field>
            {kit::error(parse_error)}
        </kit::Dialog>
    }
}

fn node_value(kind: &str, name: &str, owner: &str, target: &str) -> Value {
    let mut value = json!({"id":name,"kind":kind});
    let extra = match kind {
        "agent" => json!({"agent":owner,"prompt":"","next":target}),
        "model" => json!({"model":"","prompt":"","next":target}),
        "parallel" => {
            json!({"branches":[{"kind":"agent","agent":owner,"prompt":""},{"kind":"agent","agent":owner,"prompt":""}],"next":target})
        }
        "condition" => json!({"operator":"equals","value":"","yes":target,"no":target}),
        "approval" => json!({"prompt":"","yes":target}),
        "wait" => json!({"event":"","timeoutSeconds":3600,"next":target}),
        _ => json!({}),
    };
    merge(&mut value, extra);
    value
}

fn field(label: &'static str, tag: impl IntoView) -> impl IntoView {
    view! { <label class="k-field"><span class="k-field__label">{label}</span>{tag}</label> }
}

fn select_field(
    label: &'static str,
    value: Signal<String>,
    options: Signal<Vec<(String, String)>>,
    change: Callback<String>,
    required: bool,
) -> impl IntoView {
    field(label, select_input(label, value, options, change, required))
}

fn select_input(
    label: &'static str,
    value: Signal<String>,
    options: Signal<Vec<(String, String)>>,
    change: Callback<String>,
    required: bool,
) -> impl IntoView {
    view! {
        <span class="connection-select"><select class="k-input k-select" aria-label=label required=required on:change=move |event| change.run(event_target_value(&event))>
            {move || options.get().into_iter().map(|(id,label)| {
                let selected = id.clone();
                view! { <option value=id prop:selected=move || value.get()==selected>{label}</option> }
            }).collect_view()}
        </select><span class="k-select__chev"><ui::Icon name="chevron-down" /></span></span>
    }
}

#[component]
fn NodeSettings(
    node: String,
    history: RwSignal<History>,
    graph: Signal<Value>,
    targets: Signal<Vec<(String, String)>>,
    selected: RwSignal<String>,
    owner: StoredValue<String>,
    channels: ui::Query<Value>,
    locked: Signal<bool>,
) -> impl IntoView {
    let node = StoredValue::new(node);
    let value = Memo::new(move |_| {
        graph.with(|graph| {
            rows(&graph["nodes"])
                .iter()
                .find(|value| id(value) == node.get_value())
                .cloned()
                .unwrap_or_default()
        })
    });
    let kind = Memo::new(move |_| value.with(|value| text(&value["kind"]).to_owned()));
    let owner_select = Signal::derive(directory);
    let agent_hint = move || {
        value.with(|value| {
            directory()
                .into_iter()
                .find(|(id, _)| value["agent"] == *id)
                .map(|(_, name)| name)
        })
    };
    let field_value = move |field: &'static str| {
        Signal::derive(move || value.with(|value| text(&value[field]).to_owned()))
    };
    let update = move |field: &'static str| {
        Callback::new(move |value: String| {
            node.with_value(|node| set_node(history, node, |entry| entry[field] = json!(value)))
        })
    };
    let input = move |field_name: &'static str, label: &'static str, max: u32, required: bool| {
        field(
            label,
            view! {
                <input class="k-input" maxlength=max required=required prop:value=field_value(field_name)
                    on:input=move |event| update(field_name).run(event_target_value(&event)) />
            },
        )
    };
    let large = move |field_name: &'static str, label: &'static str, max: u32| {
        field(
            label,
            view! {
                <textarea class="k-input k-textarea" rows="3" maxlength=max required prop:value=field_value(field_name)
                    on:input=move |event| update(field_name).run(event_target_value(&event))></textarea>
            },
        )
    };
    let change_kind = Callback::new(move |kind: String| {
        let next = graph.with_untracked(|graph| {
            rows(&graph["nodes"])
                .iter()
                .find(|entry| id(entry) != node.get_value())
                .map(|entry| id(entry).to_owned())
                .unwrap_or_else(|| node.get_value())
        });
        set_node(history, &node.get_value(), |value| {
            *value = node_value(&kind, &node.get_value(), &owner.get_value(), &next)
        });
    });
    let remove = move || {
        let deleted = node.get_value();
        let next = graph.with_untracked(|graph| {
            rows(&graph["nodes"])
                .iter()
                .find(|entry| id(entry) != deleted)
                .map(|entry| id(entry).to_owned())
        });
        let Some(next) = next else {
            return;
        };
        edit_graph(history, |graph| {
            if graph["entry"] == deleted {
                graph["entry"] = json!("");
            }
            if let Some(positions) = graph["positions"].as_object_mut() {
                positions.remove(&deleted);
            }
            if let Some(nodes) = graph["nodes"].as_array_mut() {
                nodes.retain(|node| id(node) != deleted);
                for node in nodes {
                    for (key, _, _) in edges(node) {
                        if node[*key] == deleted {
                            node[*key] = json!("");
                        }
                    }
                }
            }
        });
        selected.set(next.clone());
        request_animation_frame(move || focus_node("data-workflow-node", &next));
    };
    let name = node.get_value();
    let unsupported = move || !kind.with(|kind| KINDS.iter().any(|(key, _)| *key == kind));
    let condition_model = Memo::new(move |_| value.with(|value| value["operator"] == "model"));
    let output_kind = Memo::new(move |_| {
        value.with(|value| {
            if value.get("discordChannel").is_some() {
                "discord"
            } else if value.get("channel").is_some() {
                "channel"
            } else {
                "inbox"
            }
        })
    });
    let options_channels = Signal::derive(move || {
        let mut choices = options(&[("", "Choose section")]);
        if let Some(value) = channels.data.get() {
            choices.extend(
                rows(&value)
                    .iter()
                    .map(|row| (text(&row["id"]).to_owned(), text(&row["name"]).to_owned())),
            );
        }
        choices
    });
    view! {
        <fieldset class="k-fieldset workflow-node" tabindex="-1" data-workflow-settings=name.clone()>
            <legend>{format!("Step {name}")}</legend>
            <div class="k-inline"><kit::Button label=kit::text(format!("Back to graph {name}")) on_click=move || {
                let name=node.get_value(); selected.set(name.clone()); focus_node("data-workflow-node",&name);
            } /></div>
            {select_field("Step type",kind.into(),Signal::derive(move || {
                let mut options=super::options(KINDS);
                if unsupported() { options.insert(0,(kind.get(), graph::kind_label(&kind.get()))); }
                options
            }),change_kind,true)}
            <Show when=unsupported><kit::Alert tone="warn">"Choose a supported type or delete this step."</kit::Alert></Show>
            <Show when=move || kind.get()=="agent">
                {field("Executing agent",view! {
                    {select_input("Executing agent",field_value("agent"),owner_select,update("agent"),true)}
                    {move || agent_hint().map(|name| view! { <span class="k-field__hint">{name}</span> })}
                })}
            </Show>
            <Show when=move || kind.get()=="model">{input("model","Model",200,true)}</Show>
            <Show when=move || kind.get()=="parallel"><Branches value history node owner /></Show>
            <Show when=move || kind.get()=="wait">
                {input("event","Event name",256,true)}
                {field("Wait timeout (seconds)",view! {
                    <input type="number" class="k-input" required min="1" max="604800"
                        prop:value=move || value.with(|value| number(&value["timeoutSeconds"]).to_string())
                        on:input=move |event| node.with_value(|id| set_node(history,id,|value| {
                            value["timeoutSeconds"]=json!(event_target_value(&event).parse::<i64>().unwrap_or_default());
                        })) />
                })}
            </Show>
            <Show when=move || matches!(kind.get().as_str(),"agent"|"model")>{large("prompt","Step instructions",16000)}</Show>
            <Show when=move || kind.get()=="approval">{large("prompt","Approval question",4000)}</Show>
            <Show when=move || kind.get()=="condition">
                {select_field("Condition evaluation",field_value("operator"),Signal::stored(options(&[("equals","Exact match"),("contains","Contains text"),("model","Model judgment")])),
                    Callback::new(move |operator: String| node.with_value(|node| set_node(history,node,|value| {
                        value["operator"]=json!(operator);
                        if operator=="model" { value["model"]=json!(""); }
                        else if let Some(value)=value.as_object_mut() { value.remove("model"); }
                    }))),true)}
                <Show when=move || condition_model.get()>{input("model","Condition model",200,true)}{input("value","Condition question",16000,true)}</Show>
                <Show when=move || !condition_model.get()>{input("value","Comparison text",16000,false)}</Show>
            </Show>
            <Show when=move || value.with(|value| edges(value).iter().any(|(key,_,_)| *key=="next"))>
                {select_field("Continue to",field_value("next"),targets,update("next"),true)}
            </Show>
            <Show when=move || kind.get()=="condition">
                {select_field("If matched",field_value("yes"),targets,update("yes"),true)}
                {select_field("If not matched",field_value("no"),targets,update("no"),true)}
            </Show>
            <Show when=move || kind.get()=="approval">{select_field("If approved",field_value("yes"),targets,update("yes"),true)}</Show>
            <Show when=move || kind.get()=="output">
                {select_field("Result destination",Signal::derive(move || output_kind.get().into()),
                    Signal::stored(options(&[("inbox","Workflow inbox"),("channel","Section and inbox"),("discord","Discord and inbox")])),
                    Callback::new(move |destination: String| set_node(history,&node.get_value(),|value| {
                        *value=json!({"id":node.get_value(),"kind":"output"});
                        match destination.as_str() { "channel" => value["channel"]=json!(""),"discord"=>value["discordChannel"]=json!(""),_=>{} }
                    })),true)}
                <Show when=move || output_kind.get()=="channel">
                    {select_field("Destination section",field_value("channel"),options_channels,update("channel"),true)}
                    <ReadError query=channels label="Reload destination sections" />
                </Show>
                <Show when=move || output_kind.get()=="discord">{input("discordChannel","Discord output channel ID",20,true)}</Show>
                <p class="k-field__hint">"Complete the workflow with the last step result."</p>
            </Show>
            <kit::Menu name=format!("Step {name} actions") disabled=Signal::derive(move || locked.get() || graph.with(|graph| rows(&graph["nodes"]).len()<=1))
                items=vec![kit::MenuItem{label:format!("Delete step {name}"),danger:true,action:Callback::new(move |_| remove())}]>{kit::icon(kit::icons().more)}</kit::Menu>
        </fieldset>
    }
}

#[component]
fn Branches(
    value: Memo<Value>,
    history: RwSignal<History>,
    node: StoredValue<String>,
    owner: StoredValue<String>,
) -> impl IntoView {
    let count = Memo::new(move |_| value.with(|value| rows(&value["branches"]).len()));
    let change = move |index: usize, field: &'static str| {
        Callback::new(move |value: String| {
            set_node(history, &node.get_value(), |entry| {
                if let Some(branch) = entry["branches"]
                    .as_array_mut()
                    .and_then(|branches| branches.get_mut(index))
                {
                    if field == "kind" {
                        let prompt = branch["prompt"].clone();
                        *branch = if value == "agent" {
                            json!({"kind":"agent","agent":owner.get_value(),"prompt":prompt})
                        } else {
                            json!({"kind":"model","model":"","prompt":prompt})
                        };
                    } else {
                        branch[field] = json!(value);
                    }
                }
            });
        })
    };
    view! {
        <p class="k-field__hint">"Branches receive the same input. Results merge in branch order."</p>
        <For each=move || 0..count.get() key=|index| *index children=move |index| {
            let branch = move |field: &'static str| Signal::derive(move || value.with(|value| text(&value["branches"][index][field]).to_owned()));
            let kind = branch("kind");
            view! {
                <fieldset class="k-fieldset"><legend>{format!("Branch {}",index+1)}</legend>
                    {select_field("Executor",kind,Signal::stored(options(&[("agent","Agent"),("model","Model")])),change(index,"kind"),true)}
                    <Show when=move || kind.get()=="agent">
                        {select_field("Branch agent",branch("agent"),Signal::derive(directory),change(index,"agent"),true)}
                    </Show>
                    <Show when=move || kind.get()=="model">
                        {field("Branch model",view! { <input class="k-input" required maxlength="200" prop:value=branch("model")
                            on:input=move |event| change(index,"model").run(event_target_value(&event)) /> })}
                    </Show>
                    {field("Branch instructions",view! { <textarea class="k-input k-textarea" required maxlength="16000" prop:value=branch("prompt")
                        on:input=move |event| change(index,"prompt").run(event_target_value(&event))></textarea> })}
                    <kit::Button label=kit::text(format!("Remove branch {}",index+1)) disabled=Signal::derive(move || count.get()<=2)
                        on_click=move || {
                            focus_node("data-workflow-settings",&node.get_value());
                            set_node(history,&node.get_value(),|value| {
                                if let Some(branches)=value["branches"].as_array_mut().filter(|branches| index<branches.len()&&branches.len()>2) { branches.remove(index); }
                            });
                        } />
                </fieldset>
            }
        } />
        <kit::Button label=kit::text("Add branch") disabled=Signal::derive(move || count.get()>=8)
            on_click=move || set_node(history,&node.get_value(),|value| {
                if let Some(branches)=value["branches"].as_array_mut().filter(|branches| branches.len()<8) {
                    branches.push(json!({"kind":"agent","agent":owner.get_value(),"prompt":""}));
                }
            }) />
    }
}

#[derive(Clone)]
struct StepConversation {
    agent: String,
    thread: String,
    title: String,
    description: String,
    sequence: i64,
}

#[derive(Clone, Copy)]
struct InlineStep(RwSignal<Option<StepConversation>>);

#[component]
fn Detail(
    initial: Value,
    back: Callback<()>,
    editor: Callback<Value>,
    confirm: kit::Confirm,
    #[prop(optional)] inline: bool,
) -> impl IntoView {
    let id = StoredValue::new(text(&initial["id"]).to_owned());
    let workflow = RwSignal::new(initial);
    let query = ui::query::<Value>(move || id.with_value(|id| format!("/api/workflows/{id}")));
    let heading = NodeRef::<leptos::html::H1>::new();
    heading.on_load(|heading| {
        let _ = heading.focus();
    });
    Effect::new(move |_| {
        if let Some(value) = query.data.get() {
            workflow.set(value);
        }
    });
    let owner = workflow.with_untracked(|workflow| text(&workflow["agent"]).to_owned());
    let journal = journal(&owner);
    let locked = Signal::derive(move || journal.locked());
    let input = RwSignal::new(String::new());
    let returning = return_state();
    let restore = text(&returning["run"]);
    let run = RwSignal::new(
        if id.with_value(|id| returning["workflow"] == *id) && valid_id(restore) {
            restore.to_owned()
        } else {
            journal.pending.with_untracked(|pending| {
                pending
                    .as_ref()
                    .ok()
                    .and_then(Option::as_ref)
                    .and_then(|request| {
                        match text(&request.record["kind"]) {
                            "run" => request.body["id"].as_str(),
                            "cancel" | "approve" | "retry" => request.path.split('/').nth(3),
                            _ => None,
                        }
                        .filter(|id| valid_id(id))
                        .map(str::to_owned)
                    })
                    .unwrap_or_default()
            })
        },
    );
    let history_cursor = RwSignal::new(Vec::<i64>::new());
    let runs = ui::query::<Value>(move || {
        id.with_value(|id| {
            history_cursor.with_untracked(|cursors| match cursors.last() {
                Some(before) => format!("/api/workflows/{id}/runs?before={before}"),
                None => format!("/api/workflows/{id}/runs"),
            })
        })
    });
    poll(runs.reload, 1200);
    let deleted = move || workflow.with(|workflow| enabled(&workflow["deleted"]));
    let revision = move || workflow.with(|workflow| number(&workflow["revision"]));
    let version = RwSignal::new(0_u64);
    let launch = Callback::new(move |value: Value| {
        if let Some(id) = value["id"].as_str().filter(|id| valid_id(id)) {
            run.set(id.to_owned());
        }
        runs.reload.run(());
    });
    let select_run = Callback::new(move |id: String| run.set(id));
    let changed = Callback::new(move |value: Value| {
        if enabled(&value["deleted"]) {
            back.run(());
        } else {
            if value["graph"].is_object() {
                workflow.set(value);
            }
            query.reload.run(());
            runs.reload.run(());
            version.update(|value| *value += 1);
        }
    });
    let replay = Callback::new(move |request: Request| {
        let done = if request.record["kind"] == "run" {
            launch
        } else {
            changed
        };
        journal.submit(request, done);
    });
    let run_action = move |event: web_sys::SubmitEvent| {
        event.prevent_default();
        if locked.get_untracked() {
            return;
        }
        workflow.with_untracked(|workflow| {
            journal.submit(
                action(
                    workflow,
                    "run",
                    json!({"revision":workflow["revision"],"input":input.get_untracked()}),
                ),
                launch,
            )
        });
    };
    let delete = Callback::new(move |_| {
        workflow.with_untracked(|workflow| {
            let title = format!("Delete workflow {}?", text(&workflow["name"]));
            let request = action(workflow, "delete", json!({"revision":workflow["revision"]}));
            kit::ask(
                Some(confirm),
                title,
                "Schedules stop and the webhook URL is revoked. Saved results remain available.",
                "Delete workflow",
                true,
                move || journal.submit(request.clone(), changed),
            );
        });
    });
    let page_runs = move |older| {
        if older {
            if let Some(before) = runs.data.with_untracked(|value| {
                value
                    .as_ref()
                    .and_then(|value| rows(value).last())
                    .and_then(|row| row["seq"].as_i64())
            }) {
                history_cursor.update(|cursors| cursors.push(before));
            }
        } else {
            history_cursor.update(|cursors| {
                cursors.pop();
            });
        }
        runs.data.set(None);
        runs.reload.run(());
    };
    let step = RwSignal::new(None::<StepConversation>);
    if inline {
        provide_context(InlineStep(step));
    }
    view! {
        <div class="k-page bs-root workflow-library workflow-page" hidden=move || step.with(Option::is_some)>
            <header class="k-page-head"><div class="k-page-head__text"><h1 node_ref=heading tabindex="-1">{move || workflow.with(|workflow| text(&workflow["name"]).to_owned())}</h1></div>
                <div class="k-page-head__actions">
                    <kit::Button label=kit::text("Back") variant="ghost" on_click=move || back.run(()) />
                    <button type="button" class="k-icon-btn" aria-label="Reload workflow" on:click=move |_| changed.run(Value::Null)>{kit::icon(kit::icons().refresh)}</button>
                    <Show when=move || !inline && !deleted()>
                        <kit::Button label=kit::text("Edit workflow") disabled=locked on_click=move || editor.run(workflow.get_untracked()) />
                    </Show>
                </div>
            </header>
            <Recovery journal done=changed replay confirm />
            <ReadError query label="Reload workflow" />
            <Show when=deleted><kit::Alert tone="warn">"Workflow has been deleted. Saved results can still be read."</kit::Alert></Show>
            <Show when=move || !deleted()>
                <kit::Section title="Start a run" actions=kit::content(move || view! {
                    <kit::Menu name="Workflow actions".into() disabled=locked
                        items=vec![kit::MenuItem{label:"Delete workflow".into(),danger:true,action:delete}]>{kit::icon(kit::icons().more)}</kit::Menu>
                })>
                    <form class="k-fieldset" on:submit=run_action>
                        {field("Workflow input",view! { <textarea class="k-input k-textarea" maxlength="16000" readonly=locked
                            prop:value=input on:input=move |event| input.set(event_target_value(&event))></textarea> })}
                        <div class="k-inline"><button class="k-btn k-btn--md k-btn--primary" type="submit" disabled=locked>{move || format!("Run version {}",revision())}</button></div>
                    </form>
                </kit::Section>
                <WorkflowSchedule id workflow journal confirm select_run version />
                <WorkflowWebhook id workflow journal confirm select_run version />
                <Versions id workflow journal confirm changed version />
            </Show>
            <kit::Section title="Run history">
                <ReadError query=runs />
                {move || match runs.data.get() {
                    Some(value) if rows(&value).is_empty() => kit::empty("No runs on this page yet",String::new()).into_any(),
                    Some(value) => view! { <kit::Card>{rows(&value).iter().map(|row| {
                        let id=text(&row["id"]).to_owned();
                        let tone = badge_tone(text(&row["status"]));
                        let label = if row["status"] == "done" { "Done".to_owned() } else { step_label(row) };
                        let when=date_text(&row["created"]);
                        view! { <ui::Row title=when meta=kit::content(move || kit::badge(tone,label))
                            trailing=kit::content(move || view! {
                                <button type="button" class="k-btn k-btn--sm k-btn--secondary"
                                    aria-pressed={ let selected=id.clone(); move || (run.get()==selected).to_string() }
                                    on:click={ let id=id.clone(); move |_| select_run.run(id.clone()) }>"Open run"</button>
                            }) /> }
                    }).collect_view()}</kit::Card> }.into_any(),
                    None => kit::skeleton(3,"Loading run history...").into_any(),
                }}
                <div class="k-pager" hidden=move || history_cursor.with(Vec::is_empty) && runs.data.with(|data| data.as_ref().is_none_or(|data| rows(data).len()<20))>
                    <button class="k-icon-btn" type="button" aria-label="Newer runs" disabled=move || history_cursor.with(Vec::is_empty)
                        on:click=move |_| page_runs(false)>{kit::icon(kit::icons().left)}</button>
                    <button class="k-icon-btn" type="button" aria-label="Older runs" disabled=move || runs.data.with(|data| data.as_ref().is_none_or(|data| rows(data).len()<20))
                        on:click=move |_| page_runs(true)>{kit::icon(kit::icons().right)}</button>
                </div>
            </kit::Section>
            {move || { let id=run.get(); (!id.is_empty()).then(|| run_details(id,Some((workflow,journal,confirm)))) }}
        </div>
        {move || step.get().map(|current| {
            let back_label = format!("Back to run {}", current.title);
            let back = NodeRef::<leptos::html::Button>::new();
            back.on_load(|back| request_animation_frame(move || { let _ = back.focus(); }));
            view! {
                <div class="workflow-step-conversation">
                    <div class="k-page bs-root workflow-page"><PageHead title=current.title description=current.description actions=kit::content(move || view! {
                        <button type="button" node_ref=back class="k-btn k-btn--md k-btn--ghost" on:click=move |_| {
                            step.set(None);
                            request_animation_frame(move || focus_node("data-workflow-step", &current.sequence.to_string()));
                        }>{back_label}</button>
                    }) /></div>
                    <div class="workflow-step-chat">
                        {crate::lazy_route::outlet("step", "step conversation", current.agent, current.thread, None)}
                    </div>
                </div>
            }
        })}
    }
}

fn date_text(value: &Value) -> String {
    let timestamp = value.as_i64().or_else(|| {
        let date = js_sys::Date::parse(text(value));
        date.is_finite().then_some(date as i64)
    });
    timestamp.map_or_else(|| text(value).to_owned(), |at| format(at, "en-US", &[]))
}

#[component]
fn WorkflowSchedule(
    id: StoredValue<String>,
    workflow: RwSignal<Value>,
    journal: Journal,
    confirm: kit::Confirm,
    select_run: Callback<String>,
    version: RwSignal<u64>,
) -> impl IntoView {
    let query =
        ui::query::<Value>(move || id.with_value(|id| format!("/api/workflows/{id}/schedule")));
    let conflict = Signal::derive(move || {
        journal
            .conflict
            .with(|url| url.as_ref().is_some_and(|url| url.ends_with("/schedule")))
    });
    let locked = Signal::derive(move || journal.locked() || conflict.get());
    let pending = StoredValue::new(journal.pending.with_untracked(|pending| {
        pending
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .filter(|request| request.path.ends_with("/schedule"))
            .map(|request| request.body.clone())
    }));
    let done = Callback::new(move |_| query.reload.run(()));
    Effect::watch(
        move || version.get(),
        move |_, _, _| {
            pending.set_value(None);
            query.reload.run(());
        },
        false,
    );
    let reload = move || {
        kit::ask(
            Some(confirm),
            "Reload schedule?",
            "Unsaved changes will be discarded.",
            "Reload",
            true,
            move || {
                query.data.set(None);
                query.reload.run(());
                pending.set_value(None);
                journal.conflict.set(None);
                journal.error.set(String::new());
            },
        )
    };
    view! {
        <kit::Section title="Workflow schedule" actions=kit::content(move || view! {
            <button type="button" class="k-icon-btn" aria-label="Reload schedule" disabled=move || journal.locked() on:click=move |_| reload()>{kit::icon(kit::icons().refresh)}</button>
        })>
            <kit::Alert>"Schedules use this device time. Missed occurrences are combined into one run; if the workflow is still active, the occurrence is skipped."</kit::Alert>
            <ReadError query label="Reload schedule" />
            {move || match query.data.get() {
                Some(state) => {
                    let schedule = &state["schedule"];
                    let revision=number(&schedule["revision"]);
                    let pending = pending.get_value();
                    let at = pending.as_ref().and_then(|row| row["nextAt"].as_i64()).or(schedule["next_at"].as_i64())
                        .unwrap_or_else(|| js_sys::Date::now() as i64+60_000);
                    let initial = local_date(at);
                    let interval=pending.as_ref().map_or_else(|| schedule["interval_ms"].as_i64().map(|ms| ms as f64/60_000.0),
                        |row| row["intervalSeconds"].as_i64().map(|s| s as f64/60.0)).map(|minutes| minutes.to_string()).unwrap_or_default();
                    let input=pending.as_ref().and_then(|row| row["input"].as_str()).unwrap_or_else(|| text(&schedule["input"])).to_owned();
                    let enabled=pending.as_ref().map_or_else(|| enabled(&schedule["enabled"]), |row| enabled(&row["enabled"]));
                    let save=move |event| {
                        let Some(form)=ui::form(&event) else {return;};
                        if locked.get_untracked() {return;}
                        let at=js_sys::Date::parse(&ui::field(&form,"nextAt"));
                        if !at.is_finite() {journal.error.set("Choose a workflow start time.".into());return;}
                        let minutes=ui::field(&form,"interval").parse::<i64>().ok();
                        let body=json!({"revision":revision,"nextAt":at as i64,"intervalSeconds":minutes.map(|value|value*60),
                            "input":ui::field(&form,"input"),"enabled":form.has("enabled")});
                        workflow.with_untracked(|workflow| journal.submit(action(workflow,"schedule",body),done));
                    };
                    let occurrences=rows(&state["occurrences"]).to_vec();
                    view! {
                        <form class="k-fieldset" on:submit=save>
                            {field("Workflow start time",view! { <input class="k-input" name="nextAt" type="datetime-local" step="1" required readonly=locked value=initial /> })}
                            {field("Repeat workflow every (minutes)",view! {
                                <input class="k-input" name="interval" type="number" min="1" max="525600" readonly=locked value=interval aria-describedby="workflow-interval-hint" />
                                <div id="workflow-interval-hint" class="k-field__hint">"Leave empty to run once."</div>
                            })}
                            {field("Scheduled input",view! { <textarea class="k-input k-textarea" name="input" maxlength="16000" readonly=locked>{input}</textarea> })}
                            <label class="k-check" class:is-disabled=locked>
                                <input class="k-check__box" name="enabled" type="checkbox" checked=enabled disabled=locked />
                                <span class="k-check__label">"Workflow schedule active"</span>
                            </label>
                            <Show when=move || conflict.get()><kit::Alert tone="warn">"Schedule has changed. Reload before saving again."</kit::Alert></Show>
                            <div class="k-inline"><button type="submit" class="k-btn k-btn--md k-btn--secondary" disabled=locked>"Save workflow schedule"</button></div>
                        </form>
                        <kit::Section title="Recent schedule occurrences">
                            {if occurrences.is_empty() {kit::empty("No schedule occurrences yet",String::new()).into_any()} else {
                                view! { <kit::Card>{occurrences.into_iter().map(|item| {
                                    let id=text(&item["run"]).to_owned();
                                    let reason=match text(&item["reason"]) {
                                        "hidden"=>"Skipped: agent hidden","busy"=>"Skipped: workflow still active",_=>"Run created",
                                    };
                                    let missed=number(&item["missed"]);
                                    view! { <ui::Row title=date_text(&item["scheduled_at"])
                                        meta=kit::content(move || view! { {kit::badge("neutral",reason)}{(missed>0).then(||format!("{missed} missed intervals"))} })
                                        trailing=kit::content(move || result_button(id,select_run)) /> }
                                }).collect_view()}</kit::Card> }.into_any()
                            }}
                        </kit::Section>
                    }.into_any()
                }
                None if !query.error.with(String::is_empty) => ().into_any(),
                None => kit::skeleton(3,"Loading schedule...").into_any(),
            }}
        </kit::Section>
    }
}

fn result_button(id: String, open: Callback<String>) -> impl IntoView {
    valid_id(&id).then(|| view! {
        <kit::Button label=kit::text("Open result") size="sm" on_click=move || open.run(id.clone()) />
    })
}

#[component]
fn WorkflowWebhook(
    id: StoredValue<String>,
    workflow: RwSignal<Value>,
    journal: Journal,
    confirm: kit::Confirm,
    select_run: Callback<String>,
    version: RwSignal<u64>,
) -> impl IntoView {
    let hook =
        ui::query::<Value>(move || id.with_value(|id| format!("/api/workflows/{id}/webhook")));
    let events = ui::query::<Value>(move || {
        id.with_value(|id| format!("/api/workflows/{id}/webhook-events"))
    });
    let secret = RwSignal::new(String::new());
    Effect::watch(
        move || version.get(),
        move |_, _, _| {
            hook.reload.run(());
            events.reload.run(());
        },
        false,
    );
    let conflict = Signal::derive(move || {
        journal
            .conflict
            .with(|url| url.as_ref().is_some_and(|url| url.ends_with("/webhook")))
    });
    let locked = Signal::derive(move || journal.locked() || conflict.get());
    let done = Callback::new(move |value: Value| {
        secret.set(text(&value["secret"]).to_owned());
        hook.reload.run(());
        events.reload.run(());
    });
    let reload = move || {
        kit::ask(
            Some(confirm),
            "Reload webhook?",
            "Unsaved changes will be discarded.",
            "Reload",
            true,
            move || {
                secret.set(String::new());
                hook.data.set(None);
                hook.reload.run(());
                events.reload.run(());
                journal.conflict.set(None);
                journal.error.set(String::new());
            },
        )
    };
    view! {
        <kit::Section title="Workflow webhook" actions=kit::content(move || view! {
            <button type="button" class="k-icon-btn" aria-label="Reload webhook" disabled=move || journal.locked() on:click=move |_| reload()>{kit::icon(kit::icons().refresh)}</button>
        })>
            <kit::Alert>"Send signed events to run the workflow. Events received while the workflow is still active are skipped and will not run again."</kit::Alert>
            <Show when=move || conflict.get()><kit::Alert tone="warn">"Webhook has changed. Reload settings before saving again."</kit::Alert></Show>
            <ReadError query=hook label="Reload webhook" />
            {move || match hook.data.get() {
                Some(hook) => {
                    let revision=number(&hook["revision"]);
                    let saved=hook["route"].as_str().is_some_and(valid_id);
                    let url=text(&hook["url"]).to_owned();
                    let enabled=enabled(&hook["enabled"]);
                    let route=hook["route"].clone();
                    let save=move |event| {
                        let Some(form)=ui::form(&event) else {return;};
                        if locked.get_untracked() {return;}
                        secret.set(String::new());
                        workflow.with_untracked(|workflow| journal.submit(action(workflow,"webhook",
                            json!({"revision":revision,"enabled":form.has("enabled"),"rotate":!saved || form.has("rotate")})),done));
                    };
                    let remove=Callback::new(move |_| workflow.with_untracked(|workflow| {
                        let request=action(workflow,"webhook",json!({"revision":revision,"remove":true,"route":route}));
                        kit::ask(Some(confirm),"Delete webhook trigger?","The old URL and secret will no longer work.",
                            "Delete trigger",true,move || journal.submit(request.clone(),done));
                    }));
                    view! {
                        <form class="k-fieldset" on:submit=save>
                            {saved.then(|| field("Workflow webhook URL",view! { <input class="k-input" readonly value=url /> }))}
                            <Show when=move || !secret.with(String::is_empty)>
                                {field("Workflow webhook secret",view! { <input class="k-input" readonly type="password" autocomplete="off" prop:value=secret /> })}
                                <p class="k-field__hint">"Save this secret before leaving."</p>
                            </Show>
                            <label class="k-check" class:is-disabled=locked>
                                <input class="k-check__box" name="enabled" type="checkbox" checked=enabled disabled=locked />
                                <span class="k-check__label">"Workflow webhook active"</span>
                            </label>
                            {saved.then(|| view! {
                                <label class="k-check"><input name="rotate" type="checkbox" disabled=locked />"Replace secret. Senders must use the new secret."</label>
                            })}
                            <div class="k-inline">
                                <button type="submit" class="k-btn k-btn--md k-btn--secondary" disabled=locked>"Save workflow webhook"</button>
                                {saved.then(|| view! {
                                    <kit::Menu name="Webhook actions".into() disabled=locked items=vec![kit::MenuItem{label:"Delete webhook trigger".into(),danger:true,action:remove}]>
                                        {kit::icon(kit::icons().more)}
                                    </kit::Menu>
                                })}
                            </div>
                        </form>
                    }.into_any()
                }
                None if !hook.error.with(String::is_empty) => ().into_any(),
                None => kit::skeleton(3,"Loading webhook...").into_any(),
            }}
            <kit::Section title="Recent webhook events">
                <ReadError query=events />
                {move || events.data.get().map(|events| {
                    if rows(&events).is_empty() { return kit::empty("No webhook events yet",String::new()).into_any(); }
                    view! { <kit::Card>{rows(&events).iter().map(|event| {
                        let id=text(&event["result"]["id"]).to_owned();
                        let state=if event["result"]["outcome"]=="queued" { "Run created" } else if event["result"]["reason"]=="busy" { "Skipped: workflow still active" } else { "Skipped: webhook inactive" };
                        let time=date_text(&event["received"]);
                        view! { <ui::Row title=text(&event["delivery"]).to_owned()
                            meta=kit::content(move || view! { <span>{time}</span>{kit::badge("neutral",state)} })
                            trailing=kit::content(move || result_button(id,select_run)) /> }
                    }).collect_view()}</kit::Card> }.into_any()
                })}
            </kit::Section>
        </kit::Section>
    }
}

#[component]
fn Versions(
    id: StoredValue<String>,
    workflow: RwSignal<Value>,
    journal: Journal,
    confirm: kit::Confirm,
    changed: Callback<Value>,
    version: RwSignal<u64>,
) -> impl IntoView {
    let cursors = RwSignal::new(Vec::<i64>::new());
    let query = ui::query::<Value>(move || {
        id.with_value(
            |id| match cursors.with_untracked(|cursors| cursors.last().copied()) {
                Some(before) => format!("/api/workflows/{id}/versions?before={before}"),
                None => format!("/api/workflows/{id}/versions"),
            },
        )
    });
    Effect::watch(
        move || version.get(),
        move |_, _, _| query.reload.run(()),
        false,
    );
    let turn = move |older| {
        if older {
            if let Some(before) = query
                .data
                .with_untracked(|data| data.as_ref().and_then(|data| data["next_before"].as_i64()))
            {
                cursors.update(|cursors| cursors.push(before));
            }
        } else {
            cursors.update(|cursors| {
                cursors.pop();
            });
        }
        query.data.set(None);
        query.reload.run(());
    };
    view! {
        <kit::Section title="Saved versions">
            <p class="k-field__hint">"Restore the graph as a new revision. The workflow name and runs already started stay unchanged."</p>
            <ReadError query label="Reload versions" />
            {move || match query.data.get() {
                Some(page) => view! { <kit::Card>{rows(&page["versions"]).iter().map(|row| {
                    let revision=number(&row["revision"]);
                    let graph=serde_json::to_string_pretty(&row["graph"]).unwrap_or_default();
                    let count=rows(&row["graph"]["nodes"]).len();
                    let entry=text(&row["graph"]["entry"]).to_owned();
                    let current=move || workflow.with(|workflow| workflow["revision"]==revision);
                    let restore=Callback::new(move |_| workflow.with_untracked(|workflow| {
                        let req=action(workflow,"restore",json!({"revision":workflow["revision"],"version":revision}));
                        kit::ask(Some(confirm),format!("Restore graph version {revision} as a new revision?"),
                            "Existing runs retain their saved graph.","Restore version",false,move || journal.submit(req.clone(),changed));
                    }));
                    view! {
                        <ui::Row title=format!("Version {revision}") meta=kit::content(move || view! {
                            <span>{format!("{count} steps")}</span><span>{format!("Start: {entry}")}</span>
                            <Show when=current>{kit::badge("neutral","Current")}</Show>
                        }) trailing=kit::content(move || view! {
                            <kit::Menu name=format!("Version {revision} actions") disabled=Signal::derive(move || current() || journal.locked())
                              items=vec![kit::MenuItem{label:format!("Restore version {revision}"),danger:false,action:restore}]>{kit::icon(kit::icons().more)}</kit::Menu>
                        })>
                            <kit::Disclosure title=format!("View graph version {revision}")>
                                {kit::code(format!("Graph version {revision}"),graph)}
                            </kit::Disclosure>
                        </ui::Row>
                    }
                }).collect_view()}</kit::Card> }.into_any(),
                None if !query.error.with(String::is_empty) => ().into_any(),
                None => kit::skeleton(3,"Loading versions...").into_any(),
            }}
            <div class="k-pager" hidden=move || cursors.with(Vec::is_empty) && query.data.with(|data| data.as_ref().is_none_or(|data| data["next_before"].is_null()))>
                <button type="button" class="k-icon-btn" aria-label="Newer versions" disabled=move || cursors.with(Vec::is_empty) on:click=move |_| turn(false)>{kit::icon(kit::icons().left)}</button>
                <button type="button" class="k-icon-btn" aria-label="Older versions" disabled=move || query.data.with(|data| data.as_ref().is_none_or(|data| data["next_before"].is_null()))
                    on:click=move |_| turn(true)>{kit::icon(kit::icons().right)}</button>
            </div>
        </kit::Section>
    }
}

pub(super) fn run_details(
    id: String,
    actions: Option<(RwSignal<Value>, Journal, kit::Confirm)>,
) -> impl IntoView {
    let id = StoredValue::new(id);
    let query = ui::query::<Value>(move || id.with_value(|id| format!("/api/workflow-runs/{id}")));
    poll(query.reload, 1200);
    let loaded = move || query.data.with(Option::is_some);
    let snapshot = Signal::derive(move || query.data.get().unwrap_or_default());
    let graph = Signal::derive(move || snapshot.with(|run| run["graph"].clone()));
    let steps = Signal::derive(move || snapshot.with(|run| run["steps"].clone()));
    let result = Memo::new(move |_| snapshot.with(|run| text(&run["last"]).to_owned()));
    let selection = RwSignal::new(String::new());
    let selected = Signal::derive(move || {
        selection.with(|id| {
            if id.is_empty() {
                snapshot.with(|run| text(&run["cursor"]).to_owned())
            } else {
                id.clone()
            }
        })
    });
    let details = NodeRef::<leptos::html::Div>::new();
    let return_to_step = actions.is_some();
    let inline = use_context::<InlineStep>();
    view! {
        <div class=if return_to_step { "k-section" } else { "k-stack" }>
            <ReadError query label="Reload run" />
            <Show when=move || !loaded() && query.error.with(String::is_empty)>{kit::skeleton(3,"Loading run inspector...")}</Show>
            <Show when=loaded>
                {move || snapshot.with(|run| {
                    let version = format!("Version {}", number(&run["version"]));
                    if return_to_step {
                        let label = if run["status"] == "done" { "Done".into() } else { step_label(run) };
                        view! { <div class="k-section__head"><h2 class="k-section__title">
                            {version} " " {kit::badge(badge_tone(text(&run["status"])), label)}
                        </h2></div> }.into_any()
                    } else {
                        view! { <div>{kit::badge("neutral",format!("{version} · {}",step_label(run)))}</div> }.into_any()
                    }
                })}
                <Show when=move || snapshot.with(|run| !text(&run["error"]).is_empty())>
                    <kit::Alert tone="error">{move || snapshot.with(|run| text(&run["error"]).to_owned())}</kit::Alert>
                </Show>
                <section>
                <Graph graph selected steps=steps clicked=Callback::new(move |id| {
                    selection.set(id);
                    request_animation_frame(move || {
                        if let Some(node)=details.get_untracked() {
                            let _=node.focus();
                            let options = web_sys::ScrollIntoViewOptions::new();
                            options.set_block(web_sys::ScrollLogicalPosition::Nearest);
                            options.set_inline(web_sys::ScrollLogicalPosition::Nearest);
                            node.scroll_into_view_with_scroll_into_view_options(&options);
                        }
                    });
                }) />
                <div class="workflow-step-detail" tabindex="-1" node_ref=details hidden=move || selection.with(String::is_empty)>
                    <h3>{move || format!("Step {}",selection.get())}</h3>
                    {move || {
                        let node=selection.get();
                        let matched=steps.with(|steps| rows(steps).iter().filter(|step| step["node"]==node).cloned().collect::<Vec<_>>());
                        if matched.is_empty() { return view! { <p>"Step has not run in this execution yet."</p> }.into_any(); }
                        view! { <ol>{matched.iter().enumerate().map(|(index,step)| view! {
                            <li>{format!("Attempt {}: {}",index+1,step_label(step))}
                                {step["detail"].as_str().filter(|text| !text.is_empty()).map(|text| id.with_value(|id| result_markdown(text.to_owned(),id)))}
                            </li>
                        }).collect_view()}</ol> }.into_any()
                    }}
                    <kit::Button label=kit::text("Back to run graph") on_click=move || focus_node("data-workflow-node",&selection.get_untracked()) />
                </div>
                </section>
                {actions.map(|(workflow,journal,confirm)| run_actions(snapshot,workflow,journal,confirm,query.reload))}
                {move || id.with_value(|id| result_markdown(result.get(), id))}
                <kit::Card>{move || steps.with(|steps| rows(steps).iter().map(|step| {
                    let node=text(&step["node"]).to_owned();
                    let label=step_label(step);
                    let target=step["thread"].as_str().zip(step["agent"].as_str()).filter(|(thread,agent)| valid_id(thread)&&valid_id(agent))
                        .map(|(thread,agent)| (thread.to_owned(), agent.to_owned()));
                    let detail=text(&step["detail"]).to_owned();
                    let name=format!("Open step {node}");
                    let step_node=node.clone();
                    let sequence=number(&step["seq"]);
                    view! { <ui::Row title=node meta=kit::content(move || kit::badge("neutral",label))
                        trailing=kit::content(move || target.map(|(thread, agent)| {
                            if let (Some(InlineStep(active)), Some((workflow, _, _))) = (inline, actions) {
                                return view! { <button type="button" class="k-btn k-btn--sm k-btn--secondary" data-workflow-step=sequence.to_string()
                                    on:click=move |_| {
                                        let title = workflow.with_untracked(|row| text(&row["name"]).to_owned());
                                        let version = snapshot.with_untracked(|run| number(&run["version"]));
                                        let description = format!("{title} · Version {version} · Step {step_node} · Run {}", id.get_value());
                                        active.set(Some(StepConversation { agent:agent.clone(), thread:thread.clone(), title, description, sequence }));
                                    }>{name}</button> }.into_any();
                            }
                            let url = conversation(&agent, &thread);
                            let link = NodeRef::<leptos::html::A>::new();
                            if return_to_step && return_state()["step"] == sequence {
                                link.on_load(|link| { let _=link.focus(); });
                            }
                            view! { <a node_ref=link class="k-btn k-btn--sm k-btn--secondary" href=url on:click=move |_| {
                                if let Some((workflow,_,_))=actions {
                                    let target=workflow.with_untracked(|row| json!({"workflow":row["id"],"run":id.get_value(),"step":sequence}));
                                    if let Ok(history)=window().history() {
                                        let _=history.replace_state_with_url(&JsValue::from_str(&target.to_string()),"",None);
                                    }
                                }
                            }>{name}</a> }.into_any()
                        }))>
                        {id.with_value(|id| result_markdown(detail,id))}
                    </ui::Row> }
                }).collect_view())}</kit::Card>
            </Show>
        </div>
    }
}

fn run_actions(
    run: Signal<Value>,
    workflow: RwSignal<Value>,
    journal: Journal,
    confirm: kit::Confirm,
    reload: Callback<()>,
) -> impl IntoView {
    let locked = Signal::derive(move || journal.locked());
    let execute = move |kind: &'static str, fields: Value| {
        run.with_untracked(|run| {
            workflow.with_untracked(|workflow| {
                let mut body = json!({"revision":run["revision"]});
                merge(&mut body, fields);
                command(
                    format!("/api/workflow-runs/{}/{kind}", text(&run["id"])),
                    body,
                    kind,
                    workflow,
                )
            })
        })
    };
    let done = Callback::new(move |_| reload.run(()));
    let current_node = move || {
        run.with(|run| {
            rows(&run["graph"]["nodes"])
                .iter()
                .find(|node| node["id"] == run["cursor"])
                .cloned()
                .unwrap_or_default()
        })
    };
    let cancel = Callback::new(move |_| {
        let request = execute("cancel", json!({}));
        kit::ask(
            Some(confirm),
            "Cancel the workflow and its active step?",
            "Saved results remain available.",
            "Cancel workflow",
            true,
            move || journal.submit(request.clone(), done),
        );
    });
    view! {
        <Show when=move || run.with(|run| run["status"]=="failed" && rows(&run["steps"]).len() < number(&run["graph"]["maxSteps"]) as usize)
            && matches!(text(&current_node()["kind"]),"agent"|"model"|"condition")>
            <kit::Button label=kit::text("Retry failed step") disabled=locked on_click=move || {
                let request=execute("retry",json!({}));
                kit::ask(Some(confirm),"Retry the failed step?","Previous effects may already have occurred.","Retry step",false,
                    move || journal.submit(request.clone(),done));
            } />
        </Show>
        <Show when=move || run.with(|run| run["status"]=="waiting") && current_node()["kind"]=="wait">
            {move || run.with(|run| rows(&run["steps"]).last().map(|step| {
                let path=format!("/api/workflow-waits/{}/{}",text(&run["id"]),number(&step["seq"]));
                view! { <WaitConnection path /> }
            }))}
        </Show>
        <Show when=move || run.with(|run| run["status"]=="approval")>
            {move || run.with(|run| result_markdown(text(&current_node()["prompt"]).to_owned(),text(&run["id"]))) }
            <div class="k-inline">{[(true,"Approve step"),(false,"Reject step")].into_iter().map(|(accepted,label)| view! {
                <kit::Button label=kit::text(label) disabled=locked on_click=move || journal.submit(execute("approve",json!({"accepted":accepted})),done) />
            }).collect_view()}</div>
        </Show>
        <Show when=move || run.with(|run| !matches!(text(&run["status"]),"done"|"failed"|"cancelled"))>
            <kit::Menu name="Run actions".into() disabled=locked items=vec![kit::MenuItem{label:"Cancel workflow".into(),danger:true,action:cancel}]>
                {kit::icon(kit::icons().more)}
            </kit::Menu>
        </Show>
    }
}

#[component]
fn WaitConnection(path: String) -> impl IntoView {
    let shown = RwSignal::new(false);
    let path = StoredValue::new(path);
    let query = ui::query::<Value>(move || {
        if shown.get_untracked() {
            path.get_value()
        } else {
            String::new()
        }
    });
    view! {
        <section class="k-stack bs-root">
            <h3 class="k-section__title">"Waiting for event"</h3>
            <button type="button" class="k-btn k-btn--sm k-btn--secondary" aria-expanded=move || shown.get().to_string()
                disabled=query.busy on:click=move |_| {shown.update(|shown| *shown = !*shown); query.reload.run(());}>
                {move || if shown.get() {"Hide connection"} else {"Show event connection"}}
            </button>
            <ReadError query />
            {move || query.data.get().map(|data| {
                let url=text(&data["url"]).to_owned();
                let secret=text(&data["secret"]).to_owned();
                view! {
                    <p class="k-field__hint">{format!("Event name: {}. Deadline: {}.",text(&data["event"]),date_text(&data["deadline"]))}</p>
                    {field("Event URL",view! { <input class="k-input" readonly value=url /> })}
                    {field("Signing secret",view! { <input class="k-input" readonly type="password" autocomplete="off" value=secret /> })}
                    <kit::Alert tone="warn">"The secret grants permission to resume this step. Store it only in trusted integrations."</kit::Alert>
                    <p class="k-field__hint">"Send JSON with event, optional eventTime, and payload. X-Minarbot-Time is Unix time in seconds. X-Minarbot-Id is a unique delivery ID. X-Minarbot-Signature is the hexadecimal HMAC-SHA256 of time, newline, ID, newline, then the exact JSON bytes, keyed with the UTF-8 secret."</p>
                }
            })}
        </section>
    }
}

fn inbox(back: Callback<()>) -> impl IntoView {
    use web_sys::{Notification, NotificationOptions, NotificationPermission};

    const PATH: &str = "/api/workflow-notifications";
    let cursors = RwSignal::new(Vec::<i64>::new());
    let first_page = move || cursors.with(Vec::is_empty);
    let query = ui::query::<Value>(move || {
        cursors.with_untracked(|cursors| match cursors.last() {
            Some(before) => format!("{PATH}?before={before}"),
            None => PATH.into(),
        })
    });
    let result = RwSignal::new(None::<(String, String, String)>);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let available =
        js_sys::Reflect::has(window().as_ref(), &"Notification".into()).unwrap_or(false);
    let permission = RwSignal::new(available.then(Notification::permission));
    let notifications = RwSignal::new(false);
    let newest = ui::query::<Value>(move || {
        if notifications.get_untracked() && !cursors.with_untracked(Vec::is_empty) {
            PATH.into()
        } else {
            String::new()
        }
    });
    let refresh = Callback::new(move |_| {
        query.reload.run(());
        newest.reload.run(());
    });
    poll(refresh, 5000);
    let seen = RwSignal::new(None::<i64>);
    Effect::new(move |_| {
        let data = if first_page() {
            query.data.get()
        } else {
            newest.data.get()
        };
        let Some(data) = data else {
            return;
        };
        let items = rows(&data["notifications"]);
        let sequence = items.first().map_or(0, |item| number(&item["seq"]));
        let last = seen.get_untracked();
        seen.set(Some(sequence));
        if !notifications.get_untracked()
            || Notification::permission() != NotificationPermission::Granted
            || last.is_none_or(|last| {
                !items
                    .iter()
                    .any(|item| number(&item["seq"]) > last && !enabled(&item["read"]))
            })
        {
            return;
        }
        let muted = window()
            .local_storage()
            .ok()
            .flatten()
            .and_then(|storage| {
                storage
                    .get_item("minarbot:workflow-notifications-muted")
                    .ok()
                    .flatten()
            })
            .as_deref()
            == Some("1");
        if muted {
            return;
        }
        let options = NotificationOptions::new();
        options.set_body("Open the Minarbot inbox to read results.");
        options.set_tag("minarbot-workflow-results");
        if Notification::new_with_options("Workflow results available", &options).is_err() {
            notifications.set(false);
            error.set(
                "Browser notifications are unavailable. Results remain saved in the inbox.".into(),
            );
        }
    });
    let toggle_notifications = move || {
        if notifications.get_untracked() {
            notifications.set(false);
            return;
        }
        let Ok(request) = Notification::request_permission() else {
            error.set(
                "Notification permission could not be requested. Check browser settings.".into(),
            );
            return;
        };
        spawn_local(async move {
            if wasm_bindgen_futures::JsFuture::from(request).await.is_err() {
                error.try_set(
                    "Notification permission could not be requested. Check browser settings."
                        .into(),
                );
                return;
            }
            let value = Notification::permission();
            permission.try_set(Some(value));
            notifications.try_set(value == NotificationPermission::Granted);
        });
    };
    let next = move || {
        query.data.with(|value| {
            value
                .as_ref()
                .and_then(|value| value["next_before"].as_i64())
        })
    };
    let page = move |older| {
        if older {
            if let Some(before) = next() {
                cursors.update(|cursors| cursors.push(before));
            }
        } else {
            cursors.update(|cursors| {
                cursors.pop();
            });
        }
        query.data.set(None);
        refresh.run(());
    };
    let open = move |run: String, name: String| {
        if busy.get_untracked() || !valid_id(&run) {
            return;
        }
        busy.set(true);
        spawn_local(async move {
            let response = async {
                let data = api::read::<Value>(&format!("/api/workflow-runs/{run}")).await?;
                api::command::<Value>(
                    &format!("/api/workflow-notifications/{run}/read"),
                    json!({}),
                )
                .await?;
                Ok::<_, String>(text(&data["last"]).to_owned())
            }
            .await;
            if busy.try_get_untracked().is_none() {
                return;
            }
            busy.set(false);
            match response {
                Ok(text) => {
                    result.set(Some((run, name, text)));
                    query.reload.run(());
                }
                Err(message) => error.set(message),
            }
        });
    };
    view! {
        <div class="k-page bs-root">
            {move || {
                let title = result.with(|result| result.as_ref().map_or_else(|| "Workflow inbox".to_owned(), |(_,name,_)| name.clone()));
                view! { <PageHead title actions=kit::content(move || view! {
                    <kit::Button label=kit::text("Back to workflows") variant="ghost" on_click=move || back.run(()) />
                    <Show when=move || result.with(Option::is_some) fallback=move || view! {
                        <button type="button" class="k-icon-btn" aria-label="Reload inbox" on:click=move |_| refresh.run(())>{kit::icon(kit::icons().refresh)}</button>
                    }><kit::Button label=kit::text("Back to inbox") on_click=move || result.set(None) /></Show>
                }) /> }
            }}
            <ReadError query />
            {kit::error(error)}
            {move || match result.get() {
                Some((run,_,text))=>result_markdown(if text.is_empty() {"Workflow completed without result text.".into()} else {text},&run).into_any(),
                None=>match query.data.get() {
                    Some(page)=>view! {
                        <section class="k-section"><h2 class="k-section__title">{format!("{} unread results",number(&page["unread"]))}</h2>
                            {if rows(&page["notifications"]).is_empty() {kit::empty("No results on this page yet",String::new()).into_any()} else {
                                view! { <kit::Card>{rows(&page["notifications"]).iter().map(|item| {
                                    let run=text(&item["run"]).to_owned();
                                    let name=text(&item["name"]).to_owned();
                                    let title=name.clone();
                                    let label=if enabled(&item["read"]) {"Read"} else {"Unread"};
                                    let time=date_text(&item["created"]);
                                    view! { <ui::Row title click=Callback::new(move |_| open(run.clone(),name.clone())) disabled=Signal::derive(move || busy.get())
                                        meta=kit::content(move || view! { <span>{time}</span>{kit::badge("neutral",label)} }) /> }
                                }).collect_view()}</kit::Card> }.into_any()
                            }}
                        </section>
                    }.into_any(),
                    None=>kit::skeleton(3,"Loading inbox...").into_any(),
                }
            }}
            <Show when=move || result.with(Option::is_none)>
                <kit::Alert actions=kit::content(move || view! {
                    <kit::Button label=Signal::derive(move || if notifications.get() { "Turn off browser notifications" } else { "Enable browser notifications" })
                        disabled=Signal::derive(move || matches!(permission.get(), None | Some(NotificationPermission::Denied))) on_click=toggle_notifications />
                })>"Browser notifications cover new results while this inbox is open."</kit::Alert>
                <Show when=move || permission.get().is_none()><kit::Alert>"This browser does not support notifications."</kit::Alert></Show>
                <Show when=move || permission.get()==Some(NotificationPermission::Denied)><kit::Alert>"Notifications are blocked. Change site permissions in browser settings."</kit::Alert></Show>
                <div class="k-pager" hidden=move || first_page() && next().is_none()>
                    <button type="button" class="k-icon-btn" aria-label="Newer results" disabled=move || first_page() || busy.get()
                        on:click=move |_| page(false)>{kit::icon(kit::icons().left)}</button>
                    <button type="button" class="k-icon-btn" aria-label="Older results" disabled=move || next().is_none() || busy.get()
                        on:click=move |_| page(true)>{kit::icon(kit::icons().right)}</button>
                </div>
            </Show>
        </div>
    }
}
