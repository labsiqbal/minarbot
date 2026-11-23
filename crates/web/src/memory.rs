mod knowledge;
mod notes;

use crate::{connection_ui as ui, system::kit};
use leptos::{html, prelude::*};
use serde::Deserialize;
#[cfg(feature = "conversation")]
use wasm_bindgen::JsCast;

const TABS: [(&str, &str); 3] = [
    ("notes", "Notes"),
    ("learned", "Learned"),
    ("questions", "Standing questions"),
];

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Page<T, Cursor> {
    items: Vec<T>,
    next_after: Option<Cursor>,
}

fn params(query: &str, after: &str, kind: Option<&str>) -> String {
    let Ok(params) = web_sys::UrlSearchParams::new() else {
        return String::new();
    };
    params.append("query", query);
    if let Some(kind) = kind {
        params.append("kind", kind);
    }
    params.append("after", after);
    params.to_string().as_string().unwrap_or_default()
}

fn visible_poll(root: NodeRef<html::Div>, reload: impl Fn() + Copy + 'static) {
    let refresh = move || {
        if root.get_untracked().is_some_and(|root| {
            let rect = root.get_bounding_client_rect();
            let height = window()
                .inner_height()
                .ok()
                .and_then(|h| h.as_f64())
                .unwrap_or_default();
            rect.height() > 0.0 && rect.bottom() > 0.0 && rect.top() < height
        }) && !document().hidden()
        {
            reload();
        }
    };
    ui::poll(5, refresh);
    let focus = window_event_listener(leptos::ev::focus, move |_| refresh());
    let visibility = window_event_listener(leptos::ev::visibilitychange, move |_| refresh());
    on_cleanup(move || {
        focus.remove();
        visibility.remove();
    });
}

fn retry(error: RwSignal<String>, reload: Callback<()>) -> impl IntoView {
    view! {
        <Show when=move || !error.with(String::is_empty)>
            <kit::Alert tone="error" actions=kit::content(move || view! {
                <kit::Button label=kit::text("Retry") size="sm" on_click=move || reload.run(()) />
            })>{error}</kit::Alert>
        </Show>
    }
}

fn pager<Cursor: Clone + Send + Sync + 'static>(
    cursors: RwSignal<Vec<Cursor>>,
    next: Signal<Option<Cursor>>,
    locked: Signal<bool>,
    labels: (&'static str, &'static str),
    load: Callback<()>,
) -> impl IntoView {
    let icons = kit::icons();
    view! {
        <div class="k-pager" hidden=move || cursors.with(Vec::is_empty) && next.with(Option::is_none)>
            <button type="button" class="k-icon-btn" aria-label=labels.0 title=labels.0
                disabled=move || locked.get() || cursors.with(Vec::is_empty)
                on:click=move |_| { cursors.update(|items| { items.pop(); }); load.run(()); }>{kit::icon(icons.left.clone())}</button>
            <button type="button" class="k-icon-btn" aria-label=labels.1 title=labels.1
                disabled=move || locked.get() || next.with(Option::is_none)
                on:click=move |_| { if let Some(cursor) = next.get_untracked() { cursors.update(|items| items.push(cursor)); load.run(()); } }>{kit::icon(icons.right)}</button>
        </div>
    }
}

pub(crate) fn panel(
    path: String,
    channel: bool,
    archived: bool,
    in_dialog: bool,
    close: Callback<()>,
) -> impl IntoView {
    let path = StoredValue::new(path);
    let tab = RwSignal::new("notes");
    let heading = NodeRef::<html::H1>::new();
    let (confirm, confirmation) = kit::confirmation();
    provide_context(confirm);
    let stored = RwSignal::new(true);
    let close = move || {
        if stored.get_untracked() {
            close.run(());
        } else {
            kit::ask(
                Some(confirm),
                "Leave and discard changes?",
                "Draft has not been saved in the browser.",
                "Leave",
                true,
                move || close.run(()),
            );
        }
    };
    if !in_dialog {
        Effect::new(move |_| {
            if heading.get().is_some() {
                request_animation_frame(move || {
                    if let Some(heading) = heading.try_get().flatten() {
                        let _ = heading.focus();
                    }
                });
            }
        });
    }
    view! {
        <div class="k-page bs-root">
            <header class="k-page-head">
                <div class="k-page-head__text">
                    <h1 node_ref=heading tabindex="-1">"Minarbot Memory"</h1>
                    <p>{if channel { "Shared section memory" } else { "Private agent memory" }}</p>
                </div>
                <div class="k-page-head__actions"><kit::Button label=kit::text("Back") variant="ghost" on_click=close /></div>
            </header>
            {archived.then(|| view! { <kit::Alert>"Section archived. Restore the section to change memory."</kit::Alert> })}
            <div class="k-tabs">
                <div class="k-tabs__list" role="tablist" aria-label="Memory views" aria-orientation="horizontal"
                    on:keydown=move |event| {
                        if !matches!(event.key().as_str(), "ArrowUp" | "ArrowDown") {
                            crate::settings::move_focus(&event, "[role=tablist]", true);
                        }
                    }>
                    {TABS.map(|(id, label)| view! {
                        <button type="button" role="tab" id=format!("memory-tab-{id}") class="k-tabs__tab"
                            data-state=move || if tab.get() == id { "active" } else { "inactive" }
                            aria-selected=move || (tab.get() == id).to_string()
                            tabindex=move || if tab.get() == id { 0 } else { -1 }
                            on:click=move |_| tab.set(id)>{label}</button>
                    }).into_iter().collect_view()}
                </div>
                <div role="tabpanel" aria-labelledby="memory-tab-notes" tabindex="-1" class="k-tabs__panel"
                    hidden=move || tab.get() != "notes">
                    {notes::panel(path.get_value(), archived, stored, Signal::derive(move || tab.get() == "notes"), heading)}
                </div>
                {move || {
                    let active = tab.get();
                    (active != "notes").then(|| view! {
                        <div role="tabpanel" aria-labelledby=format!("memory-tab-{active}") tabindex="-1" class="k-tabs__panel">
                            {knowledge::panel(path.get_value(), active == "questions", archived)}
                        </div>
                    })
                }}
            </div>
            {confirmation}
        </div>
    }
}

pub(crate) fn dialog(path: String, open: RwSignal<bool>) -> impl IntoView {
    let path = StoredValue::new(path);
    view! {
        <kit::Dialog id="agent-memory" title=kit::text("Minarbot Memory") open=Signal::from(open)
            size="lg" hide_header=true on_close=move || open.set(false)>
            {move || open.get().then(|| panel(path.get_value(), false, false, true, Callback::new(move |()| open.set(false))))}
        </kit::Dialog>
    }
}

pub(crate) fn profile_resources(agent: String) -> impl IntoView {
    let open = RwSignal::new(false);
    let base = format!("/api/agents/{agent}");
    view! {
        <button type="button" class="panel-row panel-row--link" on:click=move |_| open.set(true)>
            <ui::Icon name="brain" /><span class="panel-row__title">"Agent memory"</span><ui::Icon name="chevron-right" />
        </button>
        {dialog(format!("{base}/memory"), open)}
        {summary(format!("{base}/compaction"))}
    }
}

#[cfg(feature = "conversation")]
pub(crate) fn context_dialog(base: String, open: RwSignal<bool>) -> impl IntoView {
    let path = StoredValue::new(format!("{base}/compaction"));
    view! {
        <kit::Dialog id="conversation-context" title=kit::text("Conversation context") open=Signal::from(open)
            on_close=move || { open.set(false); focus_chat_menu(); }>
            {move || open.get().then(|| summary(path.get_value()))}
        </kit::Dialog>
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Summary {
    version: String,
    through: i64,
    summary: String,
    source_count: usize,
    sources: Vec<SummarySource>,
    next_after: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct SummarySource {
    id: String,
    seq: i64,
    status: String,
}

fn summary(path: String) -> impl IntoView {
    let root = NodeRef::<html::Div>::new();
    let heading = NodeRef::<html::H2>::new();
    let cursors = RwSignal::new(Vec::<i64>::new());
    let open = RwSignal::new(false);
    let disclosure = RwSignal::new(true);
    let paging = RwSignal::new(false);
    let read = ui::query::<Option<Summary>>(move || {
        let after = cursors.with_untracked(|pages| pages.last().copied().unwrap_or(0));
        format!("{path}?after={after}")
    });
    visible_poll(root, move || {
        if !read.busy.get_untracked() {
            read.reload.run(());
        }
    });
    let refresh_page = Callback::new(move |()| {
        paging.set(true);
        read.reload.run(());
    });
    Effect::new(move |_| {
        if !read.busy.get() && paging.get_untracked() {
            paging.set(false);
            if let Some(heading) = heading.get_untracked() {
                let _ = heading.focus();
            }
        }
    });
    let available = move || read.data.with(|data| matches!(data, Some(Some(_))));
    view! {
        <div node_ref=root class="bs-root summary-row">
            <div class="details-list-row">
                <button type="button" class="panel-row panel-row--link" hidden=move || !available()
                    on:click=move |_| { disclosure.set(true); open.set(true); }>
                    <ui::Icon name="history" /><span class="panel-row__title">"History summary"</span>
                    {kit::badge("neutral", "Up to date")}<ui::Icon name="chevron-right" />
                </button>
                <div class="panel-row" hidden=available>
                    <ui::Icon name="history" /><span class="panel-row__title">"History summary"</span>
                    {kit::badge("neutral", move || if !read.error.with(String::is_empty) { "Unavailable" } else if read.data.with(Option::is_some) { "None yet" } else { "Loading" })}
                </div>
            </div>
            {retry(read.error, read.reload)}
            <kit::Dialog id="history-summary" title=kit::text("History summary") open=Signal::from(open)
                size="lg" on_close=move || open.set(false)>
                <Show when=move || !read.error.with(String::is_empty)><kit::Alert tone="error">{read.error}</kit::Alert></Show>
            {move || read.data.get().flatten().filter(|_| read.error.with(String::is_empty)).map(|data| view! {
                    {kit::badge("neutral", format!("{} source messages · through {}", data.source_count, data.through))}
                    <pre class="k-code memory-summary" tabindex="0" role="region" aria-label="History summary">{data.summary}</pre>
                })}
                <div class="k-collapsible" hidden=move || !available() || !read.error.with(String::is_empty)>
                    <button type="button" class="k-collapsible__trigger" aria-expanded=move || disclosure.get().to_string()
                        data-state=move || if disclosure.get() { "open" } else { "closed" } on:click=move |_| disclosure.update(|open| *open = !*open)>
                        <span class="k-collapsible__title">"Source messages"</span>
                        <span class="k-collapsible__chev">{kit::icon(kit::icons().down)}</span>
                    </button>
                    <div class="k-collapsible__content" data-state=move || if disclosure.get() { "open" } else { "closed" }>
                        <h2 node_ref=heading tabindex="-1" class="k-section__title">"Source list"</h2>
                        <kit::Card>
                            {move || read.data.get().flatten().map(|data| data.sources.into_iter().map(|source| view! {
                                <kit::Row title=kit::text(format!("Order {} · {}", source.seq, source.status))
                                    meta=kit::content(move || view! { <code class="memory-text">{source.id}</code> }) />
                            }).collect_view())}
                        </kit::Card>
                        {pager(cursors, Signal::derive(move || read.data.with(|data| data.as_ref().and_then(|data| data.as_ref()?.next_after))),
                            Signal::derive(move || paging.get()), ("Previous sources", "Next sources"), refresh_page)}
                    </div>
                </div>
            </kit::Dialog>
        </div>
    }
}

#[cfg(feature = "conversation")]
pub(crate) fn focus_chat_menu() {
    request_animation_frame(|| {
        if let Ok(Some(menu)) = document().query_selector(".chat__pill") {
            if let Ok(menu) = menu.dyn_into::<web_sys::HtmlElement>() {
                let _ = menu.focus();
            }
        }
    });
}
