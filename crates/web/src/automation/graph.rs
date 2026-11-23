use super::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use wasm_bindgen::JsCast;

pub(super) const KINDS: &[(&str, &str)] = &[
    ("agent", "Agent"),
    ("parallel", "Parallel"),
    ("model", "Model"),
    ("condition", "Condition"),
    ("approval", "Approval"),
    ("wait", "Wait for event"),
    ("output", "Output"),
];

pub(super) fn kind_label(kind: &str) -> String {
    KINDS
        .iter()
        .find(|(value, _)| *value == kind)
        .map(|(_, label)| label.to_string())
        .unwrap_or_else(|| format!("Unsupported ({kind})"))
}

pub(super) fn id(node: &Value) -> &str {
    text(&node["id"])
}

pub(super) fn edges(node: &Value) -> &'static [(&'static str, &'static str, i64)] {
    match text(&node["kind"]) {
        "condition" => &[("yes", "Matched", 20), ("no", "Not matched", 60)],
        "approval" => &[("yes", "Approved", 40)],
        "agent" | "model" | "parallel" | "wait" => &[("next", "Continue", 40)],
        _ => &[],
    }
}

type Points = BTreeMap<String, (i64, i64)>;

fn layout(graph: &Value) -> Points {
    let nodes: BTreeMap<_, _> = rows(&graph["nodes"])
        .iter()
        .map(|node| (id(node), node))
        .collect();
    let mut levels = BTreeMap::new();
    let mut queue = VecDeque::new();
    let entry = text(&graph["entry"]);
    if nodes.contains_key(entry) {
        levels.insert(entry, 0);
        queue.push_back(entry);
    }
    while let Some(node) = queue.pop_front() {
        let level = levels.get(node).copied().unwrap_or(0);
        if let Some(value) = nodes.get(node) {
            for (edge, _, _) in edges(value) {
                let target = text(&value[*edge]);
                if nodes.contains_key(target) && !levels.contains_key(target) {
                    levels.insert(target, level + 1);
                    queue.push_back(target);
                }
            }
        }
    }
    let orphan = levels.values().copied().max().unwrap_or(0) + 1;
    let mut offsets = BTreeMap::new();
    rows(&graph["nodes"])
        .iter()
        .map(|node| {
            let id = id(node);
            let level = levels.get(id).copied().unwrap_or(orphan);
            let row = offsets.entry(level).or_insert(0);
            let point = &graph["positions"][id];
            let x = point["x"]
                .as_f64()
                .map(|x| x.round() as i64)
                .unwrap_or(64 + level * 280)
                .clamp(64, 20000);
            let y = point["y"]
                .as_f64()
                .map(|y| y.round() as i64)
                .unwrap_or(64 + *row * 160)
                .clamp(64, 20000);
            *row += 1;
            (id.to_owned(), (x, y))
        })
        .collect()
}

pub(super) fn cycles(graph: &Value) -> BTreeSet<String> {
    let nodes: BTreeMap<_, _> = rows(&graph["nodes"])
        .iter()
        .map(|node| (id(node), node))
        .collect();
    let mut seen = BTreeSet::new();
    let mut result = BTreeSet::new();
    for start in nodes.keys() {
        let mut path = Vec::<&str>::new();
        let mut next = *start;
        while let Some(node) = nodes.get(next).filter(|_| !seen.contains(next)) {
            if let Some(index) = path.iter().position(|value| *value == next) {
                result.extend(path[index..].iter().map(|value| value.to_string()));
                break;
            }
            path.push(next);
            if node["kind"] == "condition" {
                break;
            }
            let Some((edge, _, _)) = edges(node).first() else {
                break;
            };
            next = text(&node[*edge]);
        }
        seen.extend(path);
    }
    result
}

pub(super) fn step_label(step: &Value) -> String {
    if step["detail"] == "rejected" {
        return "Rejected".into();
    }
    let state = if step["status"] == "waiting" {
        if enabled(&step["paused"]) {
            return "Paused".into();
        }
        step["task_status"].as_str().unwrap_or("waiting")
    } else {
        text(&step["status"])
    };
    match state {
        "done" => "Completed",
        "waiting" => "Waiting for step",
        "approval" => "Waiting for approval",
        "ready" => "Ready",
        state => status(state),
    }
    .to_owned()
}

struct Drag {
    node: String,
    pointer: i32,
    x: i32,
    y: i32,
    initial: (i64, i64),
    scroll: (i32, i32),
}

#[component]
pub(super) fn Graph(
    graph: Signal<Value>,
    #[prop(into)] selected: Signal<String>,
    clicked: Callback<String>,
    #[prop(optional)] steps: Option<Signal<Value>>,
    #[prop(optional)] moved: Option<Callback<(String, i64, i64)>>,
    #[prop(optional)] locked: Signal<bool>,
) -> impl IntoView {
    let layout = Memo::new(move |_| graph.with(layout));
    let ids = Memo::new(move |_| {
        graph.with(|g| {
            rows(&g["nodes"])
                .iter()
                .map(|node| id(node).to_owned())
                .collect::<Vec<_>>()
        })
    });
    let viewport = NodeRef::<leptos::html::Div>::new();
    let drag = StoredValue::new(None::<Drag>);
    let preview = RwSignal::new(None::<(String, i64, i64)>);
    let suppress = RwSignal::new(false);
    let point = move |id: &str| {
        if let Some((_, x, y)) = preview.get().filter(|(node, _, _)| node == id) {
            (x, y)
        } else {
            layout.with(|layout| layout.get(id).copied().unwrap_or((64, 64)))
        }
    };
    let scroll = move || {
        viewport
            .get_untracked()
            .map(|view| (view.scroll_left(), view.scroll_top()))
            .unwrap_or_default()
    };
    let size = move || {
        layout.with(|layout| {
            layout
                .keys()
                .map(|id| point(id))
                .fold((360, 200), |(w, h), (x, y)| {
                    (w.max(x + 244), h.max(y + 144))
                })
        })
    };
    let loop_nodes = Memo::new(move |_| graph.with(cycles));
    let marker = format!("workflow-arrow-{}", ui::uuid());
    let marker_url = format!("url(#{marker})");
    let help = format!("{marker}-help");
    let show_steps = steps.is_some();
    let edges_view = move || {
        graph.with(|graph| {
        rows(&graph["nodes"]).iter().flat_map(|node| {
            edges(node).iter().filter_map(|(edge,label,port)| {
                let from = id(node);
                let to = text(&node[*edge]);
                if !layout.with(|value| value.contains_key(to)) { return None; }
                let (x,y) = point(from);
                let (target_x,target_y) = point(to);
                let start_x = x+180;
                let start_y = y+port;
                let end_y = target_y+port;
                let forward = target_x>start_x;
                let lane = y.min(target_y)-28;
                let edge = if forward {
                    format!("M{start_x},{start_y} C{},{} {},{} {target_x},{end_y}", start_x+50,start_y,target_x-50,end_y)
                } else {
                    format!("M{start_x},{start_y} C{},{} {},{lane} {start_x},{lane} L{target_x},{lane} C{},{lane} {},{end_y} {target_x},{end_y}",
                        start_x+40,start_y,start_x+40,target_x-40,target_x-40)
                };
                Some(view! { <g><path d=edge fill="none" stroke="var(--color-border-primary)" stroke-width="1.5" marker-end=marker_url.clone() />
                    <text x=(start_x+target_x)/2 y=if forward { (start_y+end_y)/2-8 } else { lane-5 } text-anchor="middle">{*label}</text></g> })
            })
        }).collect_view()
    })
    };
    view! {
        <p id=help.clone() class="k-field__hint">{if moved.is_some() {
            "Drag nodes to position them. With a keyboard, focus a node and press Alt and an arrow key. Enter opens settings."
        } else { "Graph version when the run started. Status shows the last attempt for each step. Choose a node to see its details." }}</p>
        <div class="workflow-graph" node_ref=viewport role="region" aria-label="Workflow graph, scroll to see all steps" tabindex="0">
            <div class="workflow-graph-content" style:width=move || format!("{}px",size().0) style:height=move || format!("{}px",size().1)>
                <svg width=move || size().0 height=move || size().1 aria-hidden="true">
                    <defs><marker id=marker markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
                        <path d="M0 0 L8 4 L0 8 Z" fill="var(--color-border-primary)" />
                    </marker></defs>
                    {edges_view}
                </svg>
                <For each=move || ids.get() key=String::clone children=move |node: String| {
                    let node = StoredValue::new(node);
                    let described = help.clone();
                    let label = move || graph.with(|graph| {
                        rows(&graph["nodes"]).iter().find(|entry| id(entry)==node.get_value()).map_or_else(String::new, |entry| {
                            let name = kind_label(text(&entry["kind"]));
                            if graph["entry"] == node.get_value() { format!("{name} · Start") } else { name }
                        })
                    });
                    let detail = move || {
                        if let Some(steps) = steps {
                            steps.with(|steps| rows(steps).iter().rev().find(|step| step["node"] == node.get_value()).map(step_label)
                                .or_else(|| Some("Not run yet".into())))
                        } else if loop_nodes.with(|nodes| nodes.contains(&node.get_value())) { Some("Unconditional loop".into()) }
                        else { None }
                    };
                    let at = move || node.with_value(|id| point(id));
                    let commit = move |x: i64,y: i64| {
                        if let Some(moved) = moved { moved.run((node.get_value(),x.clamp(64,20000),y.clamp(64,20000))); }
                    };
                    view! {
                        <button type="button" class="workflow-graph-node" class:workflow-run-node=show_steps data-workflow-node=node.get_value()
                            data-movable=moved.map(|_| "") aria-describedby=described disabled=locked
                            aria-current=move || (selected.get()==node.get_value()).then_some("step")
                            style:left=move || format!("{}px",at().0) style:top=move || format!("{}px",at().1)
                            on:keydown=move |event: web_sys::KeyboardEvent| {
                                suppress.set(false);
                                if event.key()=="Escape" { drag.set_value(None); preview.set(None); return; }
                                if !event.alt_key() || moved.is_none() { return; }
                                let (x,y) = at();
                                let target = match event.key().as_str() {
                                    "ArrowLeft" => (x-20,y), "ArrowRight" => (x+20,y),
                                    "ArrowUp" => (x,y-20), "ArrowDown" => (x,y+20), _ => return,
                                };
                                event.prevent_default(); commit(target.0,target.1);
                            }
                            on:pointerdown=move |event: web_sys::PointerEvent| {
                                if moved.is_none() || event.button()!=0 || !event.is_primary() { return; }
                                suppress.set(false);
                                if let Some(target) = event.current_target().and_then(|target| target.dyn_into::<web_sys::Element>().ok()) {
                                    let _ = target.set_pointer_capture(event.pointer_id());
                                }
                                drag.set_value(Some(Drag {node:node.get_value(),pointer:event.pointer_id(),x:event.client_x(),y:event.client_y(),initial:at(),scroll:scroll()}));
                            }
                            on:pointermove=move |event: web_sys::PointerEvent| {
                                drag.with_value(|drag| {
                                    let Some(drag) = drag.as_ref().filter(|drag| drag.pointer==event.pointer_id()) else { return; };
                                    let dx = event.client_x()-drag.x;
                                    let dy = event.client_y()-drag.y;
                                    if dx.abs()+dy.abs()<5 && preview.with_untracked(Option::is_none) { return; }
                                    let (sx,sy) = scroll();
                                    preview.set(Some((drag.node.clone(),(drag.initial.0+i64::from(dx+sx-drag.scroll.0)).clamp(64,20000),
                                        (drag.initial.1+i64::from(dy+sy-drag.scroll.1)).clamp(64,20000))));
                                });
                            }
                            on:pointerup=move |_| {
                                drag.set_value(None);
                                if let Some((_,x,y))=preview.get_untracked() { suppress.set(true); preview.set(None); commit(x,y); }
                            }
                            on:lostpointercapture=move |_| { drag.set_value(None); preview.set(None); }
                            on:click=move |_| {
                                if suppress.get_untracked() { suppress.set(false); } else { clicked.run(node.get_value()); }
                            }>
                            <strong>{node.get_value()}</strong><span>{label}</span>
                            {move || detail().map(|detail| view! { <small>{detail}</small> })}
                        </button>
                    }
                } />
            </div>
        </div>
        <div class="workflow-graph-links"><kit::Disclosure title="Step connections">
            <ul>{move || graph.with(|graph| rows(&graph["nodes"]).iter().flat_map(|node| {
                edges(node).iter().map(|(edge,label,_)| {
                    let target = node[*edge].as_str().filter(|value| !value.is_empty()).unwrap_or("not selected yet");
                    let missing = target != "not selected yet" && !rows(&graph["nodes"]).iter().any(|node| id(node) == target);
                    view! { <li>{format!("{}: {} to {target}{}",id(node),label.to_lowercase(),if missing {" (step not found)"} else {""})}</li> }
                })
            }).collect_view())}</ul>
        </kit::Disclosure></div>
    }
}
