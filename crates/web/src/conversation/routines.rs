use super::*;
use crate::{api, connection_ui as ui, routine_label, system::kit};

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(super) struct Report {
    agent: String,
    thread: String,
    name: String,
}

pub(super) fn report(chat: Chat, row: RwSignal<Turn>, report: Report) -> impl IntoView {
    let href = (protocol::id(&report.agent) && protocol::id(&report.thread)).then(|| {
        routes::link(&format!(
            "/agents/{}/conversations/{}",
            report.agent, report.thread
        ))
    });
    let replies = Memo::new(move |_| {
        row.with(|turn| {
            (
                turn.reply.clone().unwrap_or_default(),
                turn.generated.clone(),
            )
        })
    });
    view! {
        <section class="routine-report" aria-label=format!("Routine report: {}", report.name)>
            <div class="routine-report__meta"><ui::Icon name="schedule" /><strong>"Routine report"</strong>
                <time datetime=move || row.with(|turn| turn.created.clone())>{move || row.with(|turn| message_time(&turn.created))}</time>
            </div>
            <div class="legacy routine-report__body">{move || replies.with(|(reply, files)| {
                let html = markdown(reply, &chat.base(), serde_json::to_string(files).ok());
                view! { <div class="markdown" inner_html=html></div> }
            })}</div>
            {href.map(|href| view! { <a class="k-link" href=href>"Open full run"</a> })}
        </section>
    }
}

fn when(at: i64, compact: bool) -> impl IntoView {
    let date = date_at(at);
    let fields = [
        ("month", "short"),
        ("day", "numeric"),
        ("hour", "2-digit"),
        ("minute", "2-digit"),
    ];
    let text = if compact {
        date_format(&date, &js_sys::Array::of1(&"en-US".into()), &fields)
    } else {
        date.to_locale_string("en-US", &wasm_bindgen::JsValue::UNDEFINED)
            .as_string()
            .unwrap_or_default()
    };
    view! { <time class="panel-row__meta" datetime=date.to_iso_string().as_string()>{text}</time> }
}

fn disclose(count: usize, next: Option<i64>, children: impl IntoView) -> impl IntoView {
    let open = RwSignal::new(false);
    view! {
        <details class="k-collapsible system-disclosure" data-state=move || if open.get() { "open" } else { "closed" }
            on:toggle=move |event| open.set(event.target().and_then(|target| target.dyn_into::<web_sys::HtmlDetailsElement>().ok()).is_some_and(|details| details.open()))>
            <summary class="k-collapsible__trigger k-collapsible__trigger--section" aria-expanded=move || open.get().to_string()>
                <span class="k-collapsible__title">"Upcoming"{kit::badge("neutral", count)}{next.map(|at| when(at, true))}</span>
                <span class="k-collapsible__chev"><ui::Icon name="chevron-down" /></span>
            </summary>
            <div class="k-collapsible__content" data-state=move || if open.get() { "open" } else { "closed" }>{children}</div>
        </details>
    }
}

pub(super) fn manage(agent: &str) -> String {
    format!("/routines?agent={agent}")
}

pub(super) fn workflow_url(agent: &str) -> String {
    let from = window().location().pathname().unwrap_or_default();
    format!(
        "/workflows?agent={agent}&from={}&ui=leptos",
        ui::encode(&from)
    )
}

pub(super) fn upcoming(agent: String) -> impl IntoView {
    let href = StoredValue::new(manage(&agent));
    let list = ui::query::<Vec<Value>>(move || format!("/api/agents/{agent}/routines"));
    list.poll(1200, || true);
    let upcoming = Memo::new(move |_| {
        list.data.with(|rows| {
            let mut rows = rows
                .iter()
                .flatten()
                .filter_map(|row| {
                    (enabled(row) && row["trigger"] == "schedule")
                        .then(|| Some((row["next_at"].as_i64()?, row["name"].as_str()?.to_owned())))
                        .flatten()
                })
                .collect::<Vec<_>>();
            rows.sort_by_key(|(at, _)| *at);
            rows
        })
    });
    let manage_link = move |class| view! { <a class=class href=href.get_value()>"Manage"</a> };
    view! {
        <section class="activity-section" aria-label="Upcoming routines" hidden=move || list.error.with(String::is_empty) && upcoming.with(Vec::is_empty)>
            {move || if !list.error.with(String::is_empty) {
                view! { <kit::Alert tone="error" actions=kit::content(move || manage_link("k-btn k-btn--sm k-btn--ghost"))>{list.error}</kit::Alert> }.into_any()
            } else {
                let rows = upcoming.get();
                let Some((next, _)) = rows.first() else { return ().into_any() };
                let entries = rows.iter().take(3).map(|(at, name)| view! {
                    <a class="panel-row panel-row--routine panel-row--link" href=href.get_value()><ui::Icon name="clock" />
                        <span class="panel-row__text"><span class="panel-row__title">{name.clone()}</span>{when(*at, false)}</span>
                    </a>
                }).collect_view();
                view! { <div class="upcoming-row">{disclose(rows.len(), Some(*next), view! { <div class="panel-card">{entries}</div> })}
                    {manage_link("k-btn k-btn--sm k-btn--ghost upcoming-manage")}
                </div> }.into_any()
            }}
        </section>
    }
}

fn enabled(row: &Value) -> bool {
    row["enabled"] == true || row["enabled"] == 1
}

async fn section_rows(channel: &str) -> Result<Vec<Value>, String> {
    let mut items = Vec::new();
    let mut after = String::new();
    loop {
        let page: Value = api::read(&format!(
            "/api/routines?reportTo={channel}&after={}",
            ui::encode(&after)
        ))
        .await?;
        let next = page["next_after"].as_str().unwrap_or_default();
        items.extend(page["routines"].as_array().into_iter().flatten().cloned());
        if next.is_empty() {
            return Ok(items);
        }
        if next == after {
            return Err("Routine history did not advance.".into());
        }
        after = next.into();
    }
}

pub(super) fn section(channel: String) -> impl IntoView {
    let channel = StoredValue::new(channel);
    let rows = RwSignal::new(None::<Vec<Value>>);
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let reload = Callback::new(move |()| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        let channel = channel.get_value();
        leptos::task::spawn_local(async move {
            let result = section_rows(&channel).await;
            if busy.try_get_untracked().is_none() {
                return;
            }
            busy.set(false);
            match result {
                Ok(data) => {
                    rows.maybe_update(|rows| {
                        if rows.as_ref() == Some(&data) {
                            false
                        } else {
                            *rows = Some(data);
                            true
                        }
                    });
                    error.set(String::new());
                }
                Err(message) => error.set(message),
            }
        });
    });
    reload.run(());
    crate::connection_ui::poll_with_backoff(
        5000,
        move || Some(!error.with_untracked(String::is_empty)),
        move || reload.run(()),
    );
    view! {
        <section class="activity-section" aria-label="Routines reporting here" hidden=move || error.with(String::is_empty) && rows.with(|rows| rows.as_ref().is_none_or(Vec::is_empty))>
            {move || {
                let entries = rows.get();
                disclose(entries.as_ref().map_or(0, Vec::len), None, view! {
                    {kit::error(error)}
                    <div class="panel-card">{match entries {
                        None => view! { <p class="panel-empty" role="status">"Loading routines..."</p> }.into_any(),
                        Some(entries) if entries.is_empty() => view! { <p class="panel-empty">"No routines report to this section."</p> }.into_any(),
                        Some(entries) => entries.into_iter().map(|row| {
                            let name = row["name"].as_str().unwrap_or_default().to_owned();
                            let label = format!("{} · {}", routine_label::trigger(&row), if enabled(&row) { "Enabled" } else { "Paused" });
                            let href = row["last_thread"].as_str().zip(row["agent"].as_str())
                                .filter(|(thread, agent)| protocol::id(thread) && protocol::id(agent))
                                .map(|(thread, agent)| routes::link(&format!("/agents/{agent}/conversations/{thread}")));
                            view! { <div class="panel-row"><span class="panel-row__text"><span class="panel-row__title">{name}</span>
                                <span class="panel-row__meta">{label}</span>{href.map(|href| view! { <a class="link-btn link-btn--small" href=href>"Open last run"</a> })}
                            </span></div> }
                        }).collect_view().into_any(),
                    }}</div>
                })
            }}
        </section>
    }
}
