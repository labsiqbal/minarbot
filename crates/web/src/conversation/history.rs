use super::*;

pub(super) fn focus_element(selector: &str) {
    if let Some(node) = document()
        .query_selector(selector)
        .ok()
        .flatten()
        .and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
    {
        let _ = node.focus();
    }
}

pub(super) fn destination(agent: &str, thread: Option<&str>, room: Option<&str>) -> Option<String> {
    if let Some(room) = room {
        return protocol::id(room).then(|| format!("/rooms/{room}"));
    }
    if !protocol::id(agent) {
        return None;
    }
    match thread {
        Some(thread) if protocol::id(thread) => {
            Some(format!("/agents/{agent}/conversations/{thread}"))
        }
        None => Some(format!("/agents/{agent}")),
        _ => None,
    }
}

pub(super) fn date(value: f64) -> String {
    js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(value))
        .to_locale_string("en-US", &wasm_bindgen::JsValue::UNDEFINED)
        .into()
}

#[derive(Clone, Deserialize)]
struct ForkContext {
    summary: Option<String>,
    inherited: Option<String>,
    turns: Vec<InheritedTurn>,
}

#[derive(Clone, Deserialize)]
struct InheritedTurn {
    text: String,
    reply: Option<String>,
}

fn inherited(context: ForkContext, base: &str, depth: u8) -> AnyView {
    let earlier = context
        .inherited
        .filter(|_| depth < 8)
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .map(|value| inherited(value, base, depth + 1));
    view! {
        <section class="fork-history" aria-label="Inherited messages">
            {earlier}
            {context.summary.map(|text| view! { <div class="message-body legacy">{body(text, base.into())}</div> })}
            {context.turns.into_iter().map(|turn| view! {
                <div class="turn-block">
                    <div class="human-message" tabindex="-1" aria-label="Your message">
                        {(!turn.text.trim().is_empty()).then(|| view! { <div class="human-bubble message-plain">{turn.text}</div> })}
                        <div class="turn-meta human-meta"></div>
                    </div>
                    {turn.reply.filter(|text| !text.is_empty()).map(|text| view! { <div class="agent-message message-body legacy">{body(text, base.into())}</div> })}
                </div>
            }).collect_view()}
        </section>
    }.into_any()
}

pub(super) fn fork(chat: Chat) -> impl IntoView {
    let Some(thread) = chat.config.with_value(|config| config.thread.clone()) else {
        return ().into_any();
    };
    let Some(origin) = thread.fork else {
        return ().into_any();
    };
    let endpoint = StoredValue::new(format!("/api/threads/{}/fork-context", thread.id));
    let context = RwSignal::new(None::<ForkContext>);
    let error = RwSignal::new(String::new());
    let load = move || {
        error.set(String::new());
        let path = endpoint.get_value();
        leptos::task::spawn_local(async move {
            match get::<Option<ForkContext>>(&path).await {
                Ok(value) => {
                    context.try_set(value);
                }
                Err(reason) => {
                    error.try_set(reason.message);
                }
            }
        });
    };
    load();
    let href = destination(
        &origin.source_agent,
        (origin.source_kind == "thread").then_some(origin.source_id.as_str()),
        (origin.source_kind == "channel").then_some(origin.source_id.as_str()),
    )
    .filter(|_| protocol::id(&origin.cutoff_turn))
    .map(|path| format!("{path}?focus={}", origin.cutoff_turn));
    view! {
        {move || context.get().map(|value| inherited(value, &chat.base(), 0))}
        <div class="fork-origin" aria-label="Fork origin" title=date(js_sys::Date::parse(&origin.created))>
            <span>"Forked from " {if let Some(href) = href {
                view! { <a class="link-btn" href=href>{origin.source_name}</a> }.into_any()
            } else { origin.source_name.into_any() }}</span>
        </div>
        <p role="status" hidden=move || context.with(Option::is_some) || !error.with(String::is_empty)>"Loading inherited messages…"</p>
        <div hidden=move || error.with(String::is_empty)>
            <span class="message-error" role="alert">{error}</span>
            <button type="button" class="link-btn" on:click=move |_| load()>"Retry inherited messages"</button>
        </div>
    }.into_any()
}

#[derive(Clone, Deserialize)]
struct SearchResults {
    items: Vec<SearchHit>,
}

#[derive(Clone, Deserialize)]
struct SearchHit {
    unit: SearchUnit,
}

#[derive(Clone, Deserialize)]
struct SearchUnit {
    text: String,
    source: Value,
}

pub(super) fn search_all(chat: Chat, dialog: NodeRef<leptos::html::Dialog>) -> impl IntoView {
    let query = RwSignal::new(String::new());
    let results = RwSignal::new(None::<Vec<SearchHit>>);
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let agent = StoredValue::new(
        chat.config
            .with_value(|config| config.agent.as_ref().map(|agent| agent.id.clone())),
    );
    let icons = use_context::<Icons>().unwrap_or_default();
    view! {
        <dialog class="k-dialog k-dialog--lg workspace-dialog" node_ref=dialog aria-labelledby="history-search-title">
            <div class="k-dialog__head">
                <h2 class="k-dialog__title" id="history-search-title">"Search chat history"</h2>
                <form method="dialog"><button type="submit" class="k-icon-btn" aria-label="Close"><span class="appearance-icon" inner_html=icons.close></span></button></form>
            </div>
            <div class="k-dialog__body k-stack">
                <form class="k-inline" role="search" on:submit=move |event| {
                    event.prevent_default();
                    let text = query.get_untracked();
                    if text.trim().is_empty() || busy.get_untracked() { return; }
                    let Ok(params) = web_sys::UrlSearchParams::new() else { return; };
                    params.set("q", text.trim());
                    let base = agent.with_value(|agent| agent.as_ref().map_or_else(|| chat.base(), |id| format!("/api/agents/{id}")));
                    let path = format!("{base}/history/search?{}", params.to_string().as_string().unwrap_or_default());
                    results.set(None);
                    error.set(String::new());
                    busy.set(true);
                    leptos::task::spawn_local(async move {
                        match get::<SearchResults>(&path).await {
                            Ok(value) => { results.try_set(Some(value.items)); }
                            Err(reason) => { error.try_set(reason.message); }
                        }
                        busy.try_set(false);
                    });
                }>
                    <input type="search" class="k-input" aria-label="Search chat history" maxlength="200" required autofocus
                        prop:value=query on:input=move |event| query.set(event_target_value(&event)) />
                    <button type="submit" class="k-btn k-btn--primary" disabled=busy>"Search"</button>
                </form>
                <p role="status" hidden=move || !busy.get()>"Searching…"</p>
                <p role="alert" class="message-error" hidden=move || error.with(String::is_empty)>{error}</p>
                {move || results.get().map(|hits| if hits.is_empty() {
                    view! { <p role="status" class="k-field__hint">"No matching messages"</p> }.into_any()
                } else {
                    view! { <ul class="k-stack chat-search-results">{hits.into_iter().map(|hit| {
                        let source = hit.unit.source;
                        let href = agent.with_value(|agent| {
                            let turn = source["id"].as_str().filter(|id| protocol::id(id))?;
                            let path = destination(
                                source["agent"].as_str().or(agent.as_deref()).unwrap_or_default(),
                                source["thread"].as_str(),
                                source["channel"].as_str(),
                            ).or_else(|| chat.config.with_value(|config| config.channel).then(|| chat.base().replace("/api/channels/", "/rooms/")))?;
                            Some(format!("{path}?focus={turn}"))
                        });
                        view! { <li>{if let Some(href) = href {
                            view! { <a class="k-link" href=href>{hit.unit.text}</a> }.into_any()
                        } else { hit.unit.text.into_any() }}</li> }
                    }).collect_view()}</ul> }.into_any()
                })}
            </div>
        </dialog>
    }
}

pub(super) fn merge(previous: Vec<Turn>, latest: Vec<Turn>) -> Vec<Turn> {
    let combined = previous
        .into_iter()
        .chain(latest)
        .map(|turn| (turn.id.clone(), turn))
        .collect::<HashMap<_, _>>();
    let mut turns = combined.into_values().collect::<Vec<_>>();
    turns.sort_unstable_by(|left, right| {
        left.seq
            .cmp(&right.seq)
            .then_with(|| left.id.cmp(&right.id))
    });
    turns
}

pub(super) async fn all_turns(
    base: &str,
    mut turns: Vec<Turn>,
    current: impl Fn() -> bool,
    signal: Option<&web_sys::AbortSignal>,
) -> Result<Vec<Turn>, ApiError> {
    let mut page = turns
        .iter()
        .filter(|turn| turn.history_page != Some(0))
        .cloned()
        .collect::<Vec<_>>();
    while current() && page.len() >= 100 {
        let Some(before) = page.first().map(|turn| turn.seq) else {
            break;
        };
        page =
            get_with_signal::<Vec<Turn>>(&format!("{base}/turns?before={before}"), signal).await?;
        if page.first().is_none_or(|turn| turn.seq >= before) {
            break;
        }
        turns = merge(page.clone(), turns);
    }
    Ok(turns)
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = MinarbotConversation, js_name = findMatches)]
    fn find_matches(viewport: &web_sys::HtmlElement, query: &str) -> js_sys::Array;

    #[wasm_bindgen(js_namespace = MinarbotConversation, js_name = selectMatch)]
    fn select_match(viewport: &web_sys::HtmlElement, range: &wasm_bindgen::JsValue, scroll: bool);

    #[wasm_bindgen(js_namespace = MinarbotConversation, js_name = clearMatches)]
    fn clear_matches();
}

pub(super) fn find(
    chat: Chat,
    viewport: NodeRef<leptos::html::Section>,
    follow: RwSignal<bool>,
) -> impl IntoView {
    let icons = use_context::<Icons>().unwrap_or_default();
    let input = NodeRef::<leptos::html::Input>::new();
    let ranges = StoredValue::new_local(js_sys::Array::new());
    let position = RwSignal::new((0_u32, 0_u32));
    let select = move |offset: i32, scroll: bool| {
        let Some(view) = viewport.get_untracked() else {
            return;
        };
        let index = position.get_untracked().0;
        ranges.with_value(|ranges| {
            let count = ranges.length();
            let index = if count == 0 {
                0
            } else {
                (i64::from(index) + i64::from(offset)).rem_euclid(i64::from(count)) as u32
            };
            select_match(&view, &ranges.get(index), scroll);
            position.set((index, count));
        });
    };
    Effect::new(move |_| {
        if chat.finding.get() {
            request_animation_frame(move || {
                if let Some(input) = input.try_get_untracked().flatten() {
                    let _ = input.focus();
                }
            });
        } else {
            clear_matches();
        }
    });
    let previous = StoredValue::new(String::new());
    Effect::new(move |_| {
        if !chat.finding.get() {
            return;
        }
        let query = chat.query.get();
        chat.turns.track();
        request_animation_frame(move || {
            if chat.finding.try_get_untracked() != Some(true) || chat.query.get_untracked() != query
            {
                return;
            }
            let Some(view) = viewport.get_untracked() else {
                return;
            };
            let matches = find_matches(&view, &query);
            let (index, count) = position.get_untracked();
            let changed = previous.with_value(|last| *last != query);
            let reset = changed || count == 0;
            position.set((
                if reset {
                    0
                } else {
                    index.min(matches.length().saturating_sub(1))
                },
                matches.length(),
            ));
            previous.set_value(query);
            ranges.set_value(matches);
            select(0, reset);
        });
    });
    on_cleanup(clear_matches);
    let close = move || {
        chat.query.set(String::new());
        chat.finding.set(false);
        focus_element("#message");
    };
    view! {
        <form class="message-find bs-root" role="search" aria-label="Find in conversation"
            hidden=move || !chat.finding.get()
            on:submit=move |event| { event.prevent_default(); select(1, true); }
            on:keydown=move |event| {
                if event.key() == "Escape" { event.prevent_default(); event.stop_propagation(); close(); }
                if event.key() == "Enter" && event.shift_key() && !event.is_composing() && event.key_code() != 229 { event.prevent_default(); select(-1, true); }
            }>
            <span class="appearance-icon" inner_html=icons.search></span>
            <input node_ref=input aria-label="Find in conversation" type="text" role="searchbox" maxlength="200"
                prop:value=chat.query on:input=move |event| { follow.set(false); chat.query.set(event_target_value(&event)); } />
            <span class="message-find__count" role="status">
                {move || { let (index, count) = position.get(); if count == 0 { "0 of 0".into() } else { format!("{} of {count}", index + 1) } }}
            </span>
            <button type="button" class="k-icon-btn" aria-label="Previous match" disabled=move || position.with(|value| value.1 == 0)
                on:click=move |_| select(-1, true)><span class="appearance-icon" inner_html=icons.up></span></button>
            <button type="submit" class="k-icon-btn" aria-label="Next match" disabled=move || position.with(|value| value.1 == 0)>
                <span class="appearance-icon" inner_html=icons.chevron></span></button>
            <button type="button" class="k-icon-btn" aria-label="Close search" on:click=move |_| close()><span class="appearance-icon" inner_html=icons.close></span></button>
        </form>
    }
}

pub(super) fn begin_find(chat: Chat) {
    if !chat.focus.with_untracked(String::is_empty) || !chat.history.with_untracked(Vec::is_empty) {
        chat.turns.set(None);
    }
    chat.move_focus.set(false);
    chat.history.set(Vec::new());
    chat.focus.set(String::new());
    chat.finding.set(true);
}

pub(super) fn coordination(chat: Chat, row: RwSignal<Turn>) -> impl IntoView {
    let actions = Memo::new(move |_| row.with(|turn| turn.coordination.clone()));
    let chevron = use_context::<Icons>().unwrap_or_default().chevron;
    view! {
        <details class="k-collapsible conversation-disclosure" hidden=move || actions.with(Vec::is_empty)>
            <summary class="k-collapsible__trigger" role="button">
                <span class="k-collapsible__title">{move || format!("Task coordination ({})", actions.with(Vec::len))}</span>
                <span class="appearance-icon k-collapsible__chev" inner_html=chevron></span>
            </summary>
            {move || actions.get().into_iter().map(|action| {
                let owner = action["owner_name"].as_str().unwrap_or("Agent");
                let target = action["target_name"].as_str().unwrap_or("Agent");
                let verb = if action["kind"] == "delegate_task" { "delegated to" } else { "reassigned to" };
                let title = format!("{owner} {verb} {target}.");
                let prompt = action["prompt"].as_str().unwrap_or_default().to_owned();
                let child = action["child"].as_str().filter(|id| protocol::id(id)).map(str::to_owned);
                view! { <div class="k-stack">
                    <div class="k-section__title">{title}</div><div class="message-plain">{prompt}</div>
                    {child.map(|id| view! { <button type="button" class="k-btn k-btn--sm k-btn--ghost" on:click=move |_| chat.focus_turn(&id)>"View subtask"</button> })}
                </div> }
            }).collect_view()}
        </details>
    }
}

pub(super) fn guidance(chat: Chat, row: RwSignal<Turn>) -> impl IntoView {
    let links = Memo::new(move |_| row.with(|turn| turn.activity.dispatch.clone()));
    move || {
        links.get().into_iter().filter_map(|link| {
        if link["mode"] != "steer" { return None; }
        let id = link["source_turn"].as_str().filter(|id| protocol::id(id))?.to_owned();
        Some(view! { <button type="button" class="link-btn link-btn--small" on:click=move |_| chat.focus_turn(&id)>"Guidance linked to running turn"</button> })
    }).collect_view()
    }
}
