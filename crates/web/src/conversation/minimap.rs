// Minimap geometry adapted from T3 (MIT). See app/t3/provenance.json.
use super::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = MinarbotConversation, js_name = minimapPreview)]
    fn preview_text(text: &str) -> String;

    #[wasm_bindgen(js_namespace = MinarbotConversation, js_name = bindTimelineWheel)]
    fn bind_wheel(
        rail: &web_sys::HtmlElement,
        viewport: &web_sys::HtmlElement,
        navigate: &js_sys::Function,
    ) -> js_sys::Function;
}

#[derive(Clone, Default, PartialEq)]
struct Metrics {
    width: i32,
    current: usize,
    at_end: bool,
    visible: Vec<bool>,
}

fn top(index: usize, count: usize) -> String {
    format!(
        "{}%",
        index as f64 * 100.0 / count.saturating_sub(1).max(1) as f64
    )
}

fn row_element(id: &str) -> Option<web_sys::Element> {
    document().get_element_by_id(&format!("turn-{id}"))
}

pub(super) fn minimap(
    chat: Chat,
    rows: Memo<Vec<Turn>>,
    viewport: NodeRef<leptos::html::Section>,
    content: NodeRef<leptos::html::Div>,
    follow: RwSignal<bool>,
    scroll: RwSignal<i32>,
) -> impl IntoView {
    let ids = Memo::new(move |_| {
        rows.with(|rows| {
            rows.iter()
                .filter(|turn| {
                    turn.parent.is_none()
                        && turn.origin.is_none()
                        && turn.routine_report.is_none()
                        && turn.room.as_ref().is_none_or(|room| {
                            room["root"] == turn.id
                                || !rows.iter().any(|row| room["root"] == row.id)
                        })
                })
                .map(|turn| turn.id.clone())
                .collect::<Vec<_>>()
        })
    });
    let metrics = RwSignal::new(Metrics::default());
    let active = RwSignal::new(None::<usize>);
    let rail = NodeRef::<leptos::html::Div>::new();
    let hit = NodeRef::<leptos::html::Button>::new();
    let held = RwSignal::new(false);
    let pass = RwSignal::new(false);
    let scheduled = RwSignal::new(false);
    let schedule = move || {
        if scheduled.try_get_untracked() != Some(false) {
            return;
        }
        scheduled.set(true);
        request_animation_frame(move || {
            if scheduled.is_disposed() {
                return;
            }
            scheduled.set(false);
            let (Some(viewport), Some(content)) =
                (viewport.get_untracked(), content.get_untracked())
            else {
                return;
            };
            let bounds = viewport.get_bounding_client_rect();
            let gutter = (f64::from(viewport.client_width())
                - content.get_bounding_client_rect().width())
                / 2.0;
            let range = viewport.scroll_height() - viewport.client_height();
            let mut next = Metrics {
                width: if gutter >= 64.0 && range > 1 {
                    (gutter as i32 - 12).clamp(8, 40)
                } else {
                    0
                },
                at_end: range - viewport.scroll_top() <= 1,
                ..Metrics::default()
            };
            let mut found = false;
            ids.with_untracked(|ids| {
                for (index, id) in ids.iter().enumerate() {
                    let bounds_row = row_element(id)
                        .and_then(|element| element.query_selector(".human-message").ok().flatten())
                        .map(|node| node.get_bounding_client_rect());
                    let visible = bounds_row.as_ref().is_some_and(|row| {
                        row.top() < bounds.bottom() && row.bottom() > bounds.top()
                    });
                    if !found && (visible || bounds_row.is_some_and(|row| row.top() < bounds.top()))
                    {
                        next.current = index;
                        found = visible;
                    }
                    next.visible.push(visible);
                }
            });
            if metrics.with_untracked(|old| *old != next) {
                metrics.set(next);
            }
        });
    };
    Effect::new(move |_| {
        let (Some(viewport), Some(content)) = (viewport.get(), content.get()) else {
            return;
        };
        let resize = Closure::<dyn FnMut()>::new(schedule);
        if let Ok(observer) = web_sys::ResizeObserver::new(resize.as_ref().unchecked_ref()) {
            observer.observe(&viewport);
            observer.observe(&content);
            let observer = StoredValue::new_local((observer, resize));
            on_cleanup(move || observer.with_value(|(observer, _)| observer.disconnect()));
        }
    });
    Effect::new(move |_| {
        ids.track();
        scroll.track();
        schedule();
    });
    Effect::new(move |_| {
        let (Some(rail), Some(viewport)) = (rail.get(), viewport.get()) else {
            return;
        };
        let navigate = Closure::<dyn FnMut()>::new(move || {
            follow.set(false);
        });
        let stop = bind_wheel(&rail, &viewport, navigate.as_ref().unchecked_ref());
        let handle = StoredValue::new_local((stop, navigate));
        on_cleanup(move || {
            handle.with_value(|(stop, _)| {
                let _ = stop.call0(&wasm_bindgen::JsValue::NULL);
            })
        });
    });
    let release = window_event_listener(leptos::ev::mouseup, move |_| {
        held.set(false);
        pass.set(false);
    });
    let reenter = window_event_listener(leptos::ev::mousemove, move |event| {
        if event.buttons() == 0 && (held.get_untracked() || pass.get_untracked()) {
            held.set(false);
            pass.set(false);
        }
    });
    on_cleanup(move || {
        release.remove();
        reenter.remove();
    });
    let select = move |index: usize| {
        let item = ids.with_untracked(|ids| ids.get(index).cloned());
        if let (Some(view), Some(row)) = (
            viewport.get_untracked(),
            item.as_deref().and_then(row_element),
        ) {
            follow.set(false);
            let top = window()
                .get_computed_style(&view)
                .ok()
                .flatten()
                .and_then(|style| style.get_property_value("scroll-padding-top").ok())
                .and_then(|value| value.trim_end_matches("px").parse::<f64>().ok())
                .unwrap_or_default();
            let offset =
                row.get_bounding_client_rect().top() - view.get_bounding_client_rect().top() - top;
            view.set_scroll_top(view.scroll_top() + offset.round() as i32);
        }
    };
    let index_at = move |event: &web_sys::MouseEvent| {
        let rect = hit.get_untracked()?.get_bounding_client_rect();
        let count = ids.with_untracked(Vec::len);
        (rect.height() > 0.0 && count > 0).then(|| {
            (((f64::from(event.client_y()) - rect.top()) / rect.height()).clamp(0.0, 1.0)
                * count.saturating_sub(1) as f64)
                .round() as usize
        })
    };
    let visible = move || ids.with(Vec::len) >= 2 && metrics.with(|metrics| metrics.width > 0);
    let preview = Memo::new(move |_| {
        let index = active.get()?;
        let id = ids.with(|ids| ids.get(index).cloned())?;
        rows.with(|rows| {
            let turn = rows.iter().find(|turn| turn.id == id)?;
            let room_reply = turn.room.as_ref().and_then(|room| {
                let partial = rows.iter().rev().find(|turn| {
                    turn.reply.is_none()
                        && turn
                            .partial
                            .as_ref()
                            .is_some_and(|partial| !partial.text.is_empty())
                        && turn
                            .room
                            .as_ref()
                            .is_some_and(|other| other["root"] == room["root"])
                });
                partial
                    .and_then(|turn| turn.partial.as_ref())
                    .map(|value| value.text.as_str())
                    .or_else(|| {
                        room["messages"]
                            .as_array()?
                            .iter()
                            .rev()
                            .find(|item| item["kind"] == "reply")?["text"]
                            .as_str()
                    })
            });
            let reply = room_reply
                .or(turn.reply.as_deref())
                .or_else(|| turn.partial.as_ref().map(|value| value.text.as_str()))
                .unwrap_or_default();
            let text = turn.text.split_whitespace().collect::<Vec<_>>().join(" ");
            Some((
                index,
                if text.is_empty() {
                    "User message".into()
                } else {
                    text
                },
                preview_text(reply),
            ))
        })
    });
    let icons = use_context::<Icons>().unwrap_or_default();
    view! {
        <Show when=visible>
            <div class="timeline-minimap" data-persistent-gutter="true" data-testid="timeline-minimap" node_ref=rail>
                <div class="timeline-minimap__rail"
                    style:height=move || format!("min({}px, max(4rem, calc(100% - 14.75rem)))", ids.with(Vec::len).saturating_sub(1).saturating_mul(8).max(1))
                    style:width=move || format!("{}px", metrics.with(|value| value.width))
                    style:pointer-events=move || if pass.get() { "none" } else { "" }
                    on:mousedown=move |_| held.set(true)
                    on:mousemove=move |event| {
                        if event.buttons() != 0 && !held.get_untracked() { active.set(None); pass.set(true); }
                    }>
                    <button type="button" class="k-icon-btn timeline-minimap__previous" aria-label="Previous turn"
                        disabled=move || metrics.with(|value| value.current == 0)
                        on:click=move |_| select(metrics.with_untracked(|value| value.current.saturating_sub(1)))><span class="appearance-icon" inner_html=icons.up.clone()></span></button>
                    <button type="button" class="timeline-minimap__hit" node_ref=hit
                        aria-label=move || format!("Jump to message: {}", preview.with(|value| value.as_ref().map_or_else(|| "User message".into(), |value| value.1.clone())))
                        on:focus=move |_| active.set(Some(metrics.with_untracked(|value| value.current)))
                        on:blur=move |_| active.set(None)
                        on:mouseleave=move |_| active.set(None)
                        on:mousemove=move |event| { if event.buttons() == 0 || held.get_untracked() { active.set(index_at(&event)); } }
                        on:mousedown=move |event| event.prevent_default()
                        on:click=move |event| {
                            let index = if event.detail() == 0 { active.get_untracked() } else { index_at(&event) };
                            if let Some(index) = index { select(index); }
                            if event.detail() != 0 { if let Some(button) = hit.get_untracked() { let _ = button.blur(); } }
                        }
                        on:keydown=move |event| {
                            let count = ids.with_untracked(Vec::len);
                            let here = active.get_untracked().unwrap_or_else(|| metrics.with_untracked(|value| value.current));
                            let index = match event.key().as_str() {
                                "ArrowDown" => here.saturating_add(1).min(count.saturating_sub(1)),
                                "ArrowUp" => here.saturating_sub(1),
                                "Home" => 0,
                                "End" => count.saturating_sub(1),
                                "Escape" => { if let Some(button) = hit.get_untracked() { let _ = button.blur(); } return; },
                                _ => return,
                            };
                            event.prevent_default();
                            active.set(Some(index));
                        }>
                        {move || {
                            let values = ids.get();
                            let count = values.len();
                            values.into_iter().enumerate().map(|(index, id)| view! {
                                <span aria-hidden="true" data-minimap-strip="" data-turn=id class="timeline-minimap__tick"
                                    data-in-view=move || if metrics.with(|value| value.visible.get(index).copied().unwrap_or(false)) { "true" } else { "false" }
                                    style:top=top(index, count)
                                    style:transform=move || match active.get().map(|active| active.abs_diff(index)) {
                                        Some(0) => "translateY(-50%) scaleX(1)",
                                        Some(1) => "translateY(-50%) scaleX(.667)",
                                        Some(2) => "translateY(-50%) scaleX(.417)",
                                        _ => "translateY(-50%) scaleX(.333)",
                                    }></span>
                            }).collect_view()
                        }}
                        {move || preview.get().map(|(index, text, reply)| view! {
                            <span class="timeline-minimap__preview" style:top=top(index, ids.with(Vec::len))>
                                <strong>{text}</strong>{(!reply.is_empty()).then(|| view! { <span>{reply}</span> })}
                            </span>
                        })}
                    </button>
                    <button type="button" class="k-icon-btn timeline-minimap__next" aria-label="Next turn"
                        disabled=move || metrics.with(|value| value.at_end || value.current + 1 >= ids.with(Vec::len))
                        on:click=move |_| select(metrics.with_untracked(|value| value.current + 1))><span class="appearance-icon" inner_html=icons.chevron.clone()></span></button>
                </div>
            </div>
        </Show>
    }
}
