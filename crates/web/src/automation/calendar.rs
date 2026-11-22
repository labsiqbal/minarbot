use super::*;
use schedule::{date_label, device_zone, formatter, merge, resolve, routine_timing, wall_time};
use std::collections::BTreeMap;
use wasm_bindgen::{closure::Closure, JsCast};

const DAY: i64 = 86_400_000;
const STATUSES: &[(&str, &str)] = &[
    ("scheduled", "Scheduled"),
    ("running", "Running"),
    ("done", "Done"),
    ("failed", "Failed"),
    ("paused", "Paused"),
    ("no_update", "No update"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Period {
    Month,
    Week,
    Day,
}

impl Period {
    fn name(self) -> &'static str {
        match self {
            Self::Month => "Month",
            Self::Week => "Week",
            Self::Day => "Day",
        }
    }
    fn count(self) -> i64 {
        match self {
            Self::Month => 42,
            Self::Week => 7,
            Self::Day => 1,
        }
    }
}

fn day_key(at: i64) -> String {
    iso(at).chars().take(10).collect()
}
fn parse_day(day: &str) -> i64 {
    js_sys::Date::parse(day) as i64
}

fn first_day(at: i64, period: Period) -> i64 {
    if period == Period::Day {
        return at;
    }
    let start = if period == Period::Month {
        at - (i64::from(date_at(at).get_utc_date()) - 1) * DAY
    } else {
        at
    };
    start - i64::from((date_at(start).get_utc_day() + 6) % 7) * DAY
}

fn step(at: i64, period: Period, direction: i64) -> i64 {
    if period != Period::Month {
        return at + direction * period.count() * DAY;
    }
    let date = date_at(at);
    let _ = date.set_utc_date(1);
    let month = i64::from(date.get_utc_month()) + direction;
    if month == -1 {
        let _ = date.set_utc_full_year(date.get_utc_full_year().saturating_sub(1));
        let _ = date.set_utc_month(11);
    } else {
        let _ = date.set_utc_month(month as u32);
    }
    date.get_time() as i64
}

#[derive(Clone)]
struct Selection {
    item: Option<Value>,
    slot: Option<(String, String)>,
}

pub fn calendar_page(active: Signal<bool>) -> impl IntoView {
    let page = NodeRef::<leptos::html::Main>::new();
    Effect::watch(
        move || active.get(),
        move |is_active, _, _| {
            if *is_active {
                request_animation_frame(move || {
                    if !active.get_untracked() {
                        return;
                    }
                    if let Some(heading) = page
                        .get_untracked()
                        .and_then(|page| page.query_selector("h1").ok().flatten())
                        .and_then(|heading| heading.dyn_into::<web_sys::HtmlElement>().ok())
                    {
                        let _ = heading.focus();
                    }
                });
            }
        },
        false,
    );
    view! {
        <main node_ref=page class="chat page-view bs-root automation-calendar-route"
            hidden=move || !active.get() inert=move || !active.get()>
            <div class="page-scroll"><div class="page legacy is-wide">
                <div class="k-page bs-root calendar-page"><Calendar project=None active /></div>
            </div></div>
        </main>
    }
}

#[component]
pub(crate) fn Calendar(
    project: Option<Value>,
    #[prop(default = Signal::stored(true))] active: Signal<bool>,
) -> impl IntoView {
    let fixed = StoredValue::new(project);
    let projects = ui::query::<Value>(|| "/api/projects".to_owned());
    let zone = RwSignal::new(device_zone());
    let today = move || parse_day(&wall_time(js_sys::Date::now() as i64, &zone.get()).0);
    let date = RwSignal::new(None::<i64>);
    let period = RwSignal::new(Period::Month);
    let focused = move || date.get().unwrap_or_else(today);
    let project = RwSignal::new(fixed.with_value(|project| {
        project
            .as_ref()
            .map(|project| text(&project["id"]).to_owned())
            .unwrap_or_default()
    }));
    let agent = RwSignal::new(String::new());
    let kind = RwSignal::new(String::new());
    let state = RwSignal::new(String::new());
    let selection = RwSignal::new(None::<Selection>);
    let dragging = RwSignal::new(None::<Value>);
    let drop_day = RwSignal::new(String::new());
    let notice = RwSignal::new("");
    let path = Memo::new(move |_| {
        let start = first_day(focused(), period.get());
        let start_key = day_key(start);
        let end = day_key(start + period.get().count() * DAY);
        let values = [
            ("project", project.get()),
            ("agent", agent.get()),
            ("kind", kind.get()),
            ("status", state.get()),
        ];
        let mut pairs = vec![("start", start_key.as_str()), ("end", end.as_str())];
        pairs.extend(
            values
                .iter()
                .filter(|(_, value)| !value.is_empty())
                .map(|(key, value)| (*key, value.as_str())),
        );
        url("/api/automation-calendar", &pairs)
    });
    let query = ui::query::<Value>(move || path.get_untracked());
    Effect::watch(
        move || active.get(),
        move |is_active, _, _| {
            if *is_active {
                query.reload.run(());
            } else {
                selection.set(None);
                dragging.set(None);
                drop_day.set(String::new());
            }
        },
        false,
    );
    let events = Memo::new(move |_| {
        query.data.with(|value| {
            value
                .as_ref()
                .map(|value| (value["items"].clone(), enabled(&value["truncated"])))
        })
    });
    Effect::watch(
        move || path.get(),
        move |_, _, _| {
            query.data.set(None);
            query.reload.run(());
        },
        false,
    );
    Effect::new(move |_| {
        if let Some(new_zone) = query.data.with(|data| {
            data.as_ref()
                .and_then(|value| value["zone"].as_str().map(str::to_owned))
        }) {
            if zone.get_untracked() != new_zone {
                zone.set(new_zone);
            }
        }
    });
    poll(
        Callback::new(move |_| {
            if active.get_untracked() {
                query.reload.run(());
            }
        }),
        5000,
    );
    Effect::new(move |_| {
        if !active.get() {
            return;
        }
        if let Ok(source) = web_sys::EventSource::new("/api/automation-calendar/stream") {
            let changed = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| query.reload.run(()));
            source.set_onmessage(Some(changed.as_ref().unchecked_ref()));
            let source = StoredValue::new_local(source);
            let changed = StoredValue::new_local(changed);
            on_cleanup(move || {
                source.with_value(|source| {
                    source.set_onmessage(None);
                    source.close();
                });
                changed.dispose();
            });
        }
    });
    let archived = fixed.with_value(|project| {
        project
            .as_ref()
            .is_some_and(|project| enabled(&project["archived"]))
    });
    let agents = Signal::derive(move || {
        let agents = directory();
        fixed.with_value(|project| match project {
            Some(project) => agents
                .into_iter()
                .filter(|(id, _)| {
                    rows(&project["members"])
                        .iter()
                        .any(|member| member["agent"] == *id)
                })
                .collect(),
            None => agents,
        })
    });
    let ready = move || !archived && query.data.with(Option::is_some);
    let add = Callback::new(move |(date, time)| {
        selection.set(Some(Selection {
            item: None,
            slot: Some((date, time)),
        }))
    });
    let filters = [
        ("calendar-project", "Filter by project", project, Signal::derive(move || {
            let mut options = options(&[("","All projects"),("none","No project")]);
            if let Some(value) = projects.data.get() {
                options.extend(rows(&value["projects"]).iter().map(|row| (text(&row["id"]).to_owned(), text(&row["name"]).to_owned())));
            }
            options
        })),
        ("calendar-agent", "Filter by agent", agent, Signal::derive(move || {
            let mut options = options(&[("","All agents")]); options.extend(agents.get()); options
        })),
        ("calendar-kind", "Filter by kind", kind, Signal::stored(options(&[("","All kinds"),("routine","Routines"),("workflow","Workflows")]))),
        ("calendar-status", "Filter by status", state, Signal::derive(|| {
            let mut options = options(&[("","All statuses")]); options.extend(super::options(STATUSES)); options
        })),
    ].into_iter().filter(|(id,_,_,_)| *id != "calendar-project" || fixed.with_value(Option::is_none))
        .map(|(id,label,value,options)| view! {
            <Select name=id label=label value=value.into() options small=true change=Callback::new(move |next| value.set(next)) />
        }).collect_view();
    let add_slot = move |date: String, time: String, day_label: &str, hourly: bool| {
        let label = if hourly {
            format!("Add automation on {day_label} at {time}")
        } else {
            format!("Add automation on {day_label}")
        };
        view! { <button type="button" class="k-icon-btn calendar-add" aria-label=label disabled=move || !ready()
        on:click=move |_| add.run((date.clone(),time.clone()))>{kit::icon(kit::icons().plus)}</button> }
    };
    let event = move |item: Value, compact: bool, time: String| {
        let kind = if item["kind"] == "routine" {
            "Routine"
        } else {
            "Workflow"
        };
        let name = text(&item["title"]).to_owned();
        let status_value = text(&item["status"]).to_owned();
        let label = status(&status_value).to_owned();
        let count = number(&item["count"]);
        let spoken = format!(
            "{name}, {kind}, {label}, {time}{}",
            if count > 1 {
                format!(", {count} occurrences")
            } else {
                String::new()
            }
        );
        let mut context = label;
        if !compact {
            if let Some((_, name)) = agents
                .get()
                .into_iter()
                .find(|(id, _)| item["agent"] == *id)
            {
                context.push_str(&format!(" · {name}"));
            }
            if fixed.with_value(Option::is_none) {
                projects.data.with(|projects| {
                    let project = projects.as_ref().and_then(|data| {
                        rows(&data["projects"])
                            .iter()
                            .find(|row| row["id"] == item["project"])
                    });
                    if let Some(project) = project {
                        context.push_str(&format!(" · {}", text(&project["name"])));
                    }
                });
            }
        }
        let can_drag = item["status"] == "scheduled" && enabled(&item["editable"]) && !archived;
        let dragged = item.clone();
        let drag_name = name.clone();
        let tooltip = format!("{name} · {kind} · {context}");
        view! {
            <button type="button" class="calendar-event" aria-label=spoken title=tooltip data-status=status_value
                draggable=can_drag.to_string()
                on:dragstart=move |event: web_sys::DragEvent| {
                    if !can_drag { return; }
                    if let Some(data) = event.data_transfer() {
                        let _ = data.set_data("text/plain", &drag_name);
                        data.set_effect_allowed("move");
                    }
                    dragging.set(Some(dragged.clone()));
                }
                on:dragend=move |_| { dragging.set(None); drop_day.set(String::new()); }
                on:click=move |_| selection.set(Some(Selection {item:Some(item.clone()),slot:None}))>
                <span class="calendar-event__main"><ui::Icon name=if kind=="Routine" {"calendar"} else {"workflow"} />
                    <span class="calendar-event__name">{name}</span></span>
                <span class="calendar-event__meta"><span>{if count > 1 { format!("{count} runs") } else { time }}</span>
                    <span class="calendar-event__status">{context}</span></span>
            </button>
        }
    };
    let drop_at = Callback::new(move |(day, time): (String, Option<String>)| {
        drop_day.set(String::new());
        if let Some(item) = dragging.get_untracked() {
            let (old_day, old_time) = wall_time(number(&item["at"]), &zone.get_untracked());
            let time = time.unwrap_or_else(|| old_time.clone());
            if day != old_day || time != old_time {
                selection.set(Some(Selection {
                    item: Some(item),
                    slot: Some((day, time)),
                }));
            }
        }
        dragging.set(None);
    });
    let period_signal = period;
    let contents = move || {
        let Some((items, truncated)) = events.get() else {
            return if query.error.with(String::is_empty) {
                kit::skeleton(3, "Loading automation calendar...").into_any()
            } else {
                ().into_any()
            };
        };
        let focus = focused();
        let period = period.get();
        let start = first_day(focus, period);
        let zone = zone.get();
        let today = today();
        let full_date = formatter("en-US", &[("timeZone", "UTC"), ("dateStyle", "full")]);
        let local_day = formatter(
            "sv-SE",
            &[
                ("timeZone", &zone),
                ("year", "numeric"),
                ("month", "2-digit"),
                ("day", "2-digit"),
            ],
        );
        let clock = formatter(
            "en-GB",
            &[
                ("timeZone", &zone),
                ("hour", "2-digit"),
                ("minute", "2-digit"),
                ("hourCycle", "h23"),
            ],
        );
        let event = |item: Value, compact| {
            let time = clock(number(&item["at"]));
            event(item, compact, time)
        };
        let mut days: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for item in rows(&items) {
            days.entry(local_day(number(&item["at"])))
                .or_default()
                .push(item.clone());
        }
        let focus_key = day_key(focus);
        let grid = if period == Period::Day {
            let day = focus_key.clone();
            let label = full_date(focus);
            let mut by_hour: BTreeMap<String, Vec<Value>> = BTreeMap::new();
            for item in days.get(&day).into_iter().flatten() {
                let time = clock(number(&item["at"]));
                by_hour
                    .entry(time.chars().take(2).collect())
                    .or_default()
                    .push(item.clone());
            }
            view! { <ol class="calendar-day calendar-wide" aria-label="Hourly calendar">
                {(0..24).map(|hour| {
                    let key = format!("{hour:02}");
                    let time = format!("{key}:00");
                    let date = day.clone();
                    let target = time.clone();
                    let click_date = day.clone();
                    let click_time = time.clone();
                    let drop_key = format!("{date}{time}");
                    view! { <li data-drop={let key=drop_key.clone(); move || (drop_day.get() == key).then_some("")}
                        on:dragover=move |event| {
                            if dragging.with_untracked(Option::is_some) { event.prevent_default(); drop_day.set(drop_key.clone()); }
                        }
                        on:dragleave=move |_| drop_day.set(String::new())
                        on:drop=move |event| { event.prevent_default(); drop_at.run((date.clone(),Some(target.clone()))); }
                        on:click=move |event| {
                            let on_button=event.target().and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                                .is_some_and(|element| element.closest("button").is_ok_and(|button| button.is_some()));
                            if !on_button && ready() { add.run((click_date.clone(),click_time.clone())); }
                        }>
                        <span class="calendar-day__hour">{time.clone()}</span>
                        <div class="calendar-day__events">{by_hour.remove(&key).unwrap_or_default().into_iter().map(|item| event(item,false)).collect_view()}</div>
                        {add_slot(day.clone(), time, &label, true)}
                    </li> }
                }).collect_view()}
            </ol> }.into_any()
        } else {
            let label = format!("{} calendar", period.name());
            view! { <div class=format!("calendar-grid calendar-wide calendar-grid--{}",period.name().to_lowercase()) aria-label=label>
                {["Mon","Tue","Wed","Thu","Fri","Sat","Sun"].into_iter().map(|day| view! { <div class="calendar-grid__weekday">{day}</div> }).collect_view()}
                {(0..period.count()).map(|index| {
                    let at = start + index * DAY;
                    let day = day_key(at);
                    let label = full_date(at);
                    let outside = period == Period::Month && date_at(at).get_utc_month() != date_at(focus).get_utc_month();
                    let day_items = days.get(&day).map(Vec::as_slice).unwrap_or_default();
                    let limit = if period == Period::Month { 3 } else { day_items.len() };
                    let more = day_items.len().saturating_sub(limit);
                    let hover = day.clone();
                    let drop = day.clone();
                    let target = day.clone();
                    let click_day = day.clone();
                    let view_day = move || { date.set(Some(at)); period_signal.set(Period::Day); };
                    view! { <section class="calendar-cell" aria-label=label.clone() data-outside=outside.then_some("")
                        data-drop=move || (drop_day.get() == hover).then_some("")
                        on:dragover=move |event: web_sys::DragEvent| {
                            if dragging.with_untracked(Option::is_some) { event.prevent_default(); drop_day.set(target.clone()); }
                        }
                        on:dragleave=move |_| drop_day.set(String::new())
                        on:click=move |event| {
                            if event.target() == event.current_target() && ready() {add.run((click_day.clone(),"09:00".into()));}
                        }
                        on:drop=move |event| { event.prevent_default(); drop_at.run((drop.clone(),None)); }>
                        <div class="calendar-cell__head"><button type="button" class="k-btn k-btn--sm k-btn--ghost"
                            aria-label=format!("View {label}") aria-current=(at == today).then_some("date")
                            on:click=move |_| view_day()>{date_at(at).get_utc_date()}</button>{add_slot(day,"09:00".into(),&label,false)}</div>
                        {day_items.iter().take(limit).cloned().map(|item| event(item,true)).collect_view()}
                        {(more > 0).then(|| view! { <button type="button" class="k-btn k-btn--sm k-btn--ghost calendar-more" on:click=move |_| view_day()>{format!("+{more} more")}</button> })}
                    </section> }
                }).collect_view()}
            </div> }.into_any()
        };
        if days.is_empty() {
            days.insert(focus_key, Vec::new());
        }
        view! {
            {truncated.then(|| view! { <kit::Alert tone="warn">"Showing up to 500 items. Use filters or a shorter view to see more."</kit::Alert> })}
            {grid}
            <div class="calendar-agenda" aria-label="Agenda">{days.into_iter().map(|(day,items)| {
                let label = full_date(parse_day(&day));
                view! { <section class="calendar-agenda__date" aria-label=label.clone()>
                    <div class="calendar-agenda__head"><h3>{date_label(&day,&[("weekday","long"),("month","short"),("day","numeric")])}</h3>{add_slot(day.clone(),"09:00".into(),&label,false)}</div>
                    {if items.is_empty() { kit::empty("No automations in this range",String::new()).into_any() }
                      else { items.into_iter().map(|item| event(item,false)).collect_view().into_any() }}
                </section> }
            }).collect_view()}</div>
        }.into_any()
    };
    let download = fixed.with_value(|project| {
        project.as_ref().map(|project| {
            url(
                "/api/automation-calendar/routines.ics",
                &[("project", text(&project["id"]))],
            )
        })
    });
    let actions = kit::content(move || {
        view! {
            <button type="button" class="k-btn k-btn--sm k-btn--primary" disabled=move || !ready()
                on:click=move |_| add.run((day_key(focused()),"09:00".into()))><ui::Icon name="plus" />"Add automation"</button>
        }
    });
    view! {
        {match download {
            Some(download) => view! {
                <div class="calendar-actions">{actions()}
                    <a class="k-btn k-btn--secondary k-btn--sm" href=download download="minarbot-routines.ics">{kit::icon(kit::icons().download)}"Download routines"</a>
                </div>
            }.into_any(),
            None => view! { <PageHead title="Automation calendar" actions /> }.into_any(),
        }}
        <section class="automation-calendar" aria-labelledby="calendar-period-title">
            <div class="calendar-toolbar"><div class="calendar-period"><div class="calendar-period__nav">
                <button type="button" class="k-icon-btn" aria-label=move || format!("Previous {}",period.get().name().to_lowercase())
                    on:click=move |_| date.set(Some(step(focused(),period.get_untracked(),-1)))>{kit::icon(kit::icons().left)}</button>
                <button type="button" class="k-icon-btn" aria-label=move || format!("Next {}",period.get().name().to_lowercase())
                    on:click=move |_| date.set(Some(step(focused(),period.get_untracked(),1)))>{kit::icon(kit::icons().right)}</button>
                <kit::Button label=kit::text("Today") size="sm" variant="ghost" on_click=move || date.set(None) />
            </div><h2 id="calendar-period-title" aria-live="polite">{move || {
                let key = day_key(focused());
                match period.get() {
                    Period::Month => date_label(&key,&[("month","long"),("year","numeric")]),
                    Period::Day => date_label(&key,&[("weekday","long"),("month","long"),("day","numeric")]),
                    Period::Week => {
                        let start = first_day(focused(),Period::Week);
                        format!("{} – {}",date_label(&day_key(start),&[("month","short"),("day","numeric")]),
                            date_label(&day_key(start+6*DAY),&[("month","short"),("day","numeric"),("year","numeric")]))
                    }
                }
            }}</h2></div>
            <div class="calendar-view" role="group" aria-label="Calendar view">
                {[Period::Month,Period::Week,Period::Day].into_iter().map(|choice| view! {
                    <button type="button" class="k-btn k-btn--ghost k-btn--sm" aria-pressed=move || (period.get()==choice).to_string()
                        on:click=move |_| period.set(choice)>{choice.name()}</button>
                }).collect_view()}
            </div></div>
            <div class="calendar-filters">{filters}<span class="calendar-zone" title="Home device time zone">{zone}</span></div>
            <ReadError query />
            <div class="sr-only" role="status">{notice}</div>
            {contents}
            {move || selection.get().map(|selected| {
                let initial = StoredValue::new(selected.item.clone().unwrap_or_default());
                let follow = selected.item.is_some() && selected.slot.is_none();
                let live = Memo::new(move |_| {
                    if follow {
                        if let Some(current) = events.with(|data| data.as_ref().and_then(|(items, _)| {
                            initial.with_value(|initial| rows(items).iter().find(|item| item["id"] == initial["id"]).cloned())
                        })) { return current; }
                    }
                    initial.get_value()
                });
                let projects = projects.data.with_untracked(|data| data.as_ref().map_or_else(Vec::new, |data| rows(&data["projects"]).to_vec()));
                view! {
                <CalendarDialog selected live=live.into() zone=zone.get_untracked() default_project=project.get_untracked()
                    default_agent=agent.get_untracked() agents=agents.get_untracked() projects archived
                    close=Callback::new(move |_| selection.set(None))
                    saved=Callback::new(move |ran| {
                        notice.set(if ran { "Run requested." } else { "Schedule saved." });
                        selection.set(None);
                        if ran { date.set(None); state.set(String::new()); }
                        query.reload.run(());
                    }) /> }
            })}
        </section>
    }
}

fn calendar_request(_: &str, value: Value) -> Result<Request, String> {
    let path = text(&value["path"]);
    let parts = path.split('/').collect::<Vec<_>>();
    match parts.as_slice() {
        ["", "api", "agents", id, "routines"]
        | ["", "api", "routines", id, "edit" | "test"]
        | ["", "api", "workflows", id, "schedule" | "run"]
            if valid_id(id) =>
        {
            Request::restore(path.into(), value["body"].clone(), value)
        }
        _ => Err("Saved calendar request could not be read.".into()),
    }
}

fn workflow_schedule(previous: &Value) -> Value {
    json!({
        "revision": number(&previous["revision"]),
        "nextAt": previous["next_at"],
        "intervalSeconds": previous["interval_ms"].as_i64().map(|value| value / 1000),
        "input":previous["input"].as_str().unwrap_or_default(),
        "enabled": previous.is_null() || enabled(&previous["enabled"]),
    })
}

fn routine_schedule(previous: &Value, zone: &str, fields: Value) -> Value {
    let mut body = json!({
        "revision":previous["revision"],"name":previous["name"],"prompt":previous["prompt"],
        "enabled": enabled(&previous["enabled"]),
    });
    if fields.get("schedule").is_none() && fields.get("nextAt").is_none() {
        merge(&mut body, routine_timing(previous, zone));
    }
    merge(&mut body, fields);
    body
}

fn move_routine(routine: &Value, from: i64, at: i64, zone: &str) -> Result<Value, String> {
    let zone = routine["schedule_zone"].as_str().unwrap_or(zone);
    let (day, time) = wall_time(at, zone);
    let schedule = &routine["schedule"];
    let next = (number(&routine["next_at"]) + at - from).max(1);
    let spec = match text(&schedule["kind"]) {
        "once" => json!({"kind":"once","date":day,"time":time}),
        "instant" => json!({"kind":"instant","at":at}),
        "interval" => {
            let mut schedule = schedule.clone();
            schedule["anchorAt"] = json!(next);
            schedule
        }
        "cron" => {
            let fields = text(&schedule["expression"])
                .split_whitespace()
                .collect::<Vec<_>>();
            let offset =
                ((parse_day(&day) - parse_day(&wall_time(from, zone).0)) / DAY).rem_euclid(7);
            let simple = |value: &str| value == "*" || value.parse::<u8>().is_ok();
            let invalid = || "Edit the cron expression to move this series.".to_owned();
            let [minute, hour, day, month, days] = fields.as_slice() else {
                return Err(invalid());
            };
            if minute.parse::<u8>().is_err()
                || hour.parse::<u8>().is_err()
                || !simple(day)
                || !simple(month)
                || offset != 0 && (*day != "*" || *month != "*")
            {
                return Err(invalid());
            }
            let shifted = if *days == "*" {
                "*".to_owned()
            } else {
                let mut weekdays = if *days == "1-5" {
                    (1..=5).collect()
                } else {
                    days.split(',')
                        .map(str::parse::<i64>)
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|_| invalid())?
                };
                if weekdays.iter().any(|value| !(0..=6).contains(value)) {
                    return Err(invalid());
                }
                if offset == 0 {
                    days.to_string()
                } else {
                    for day in &mut weekdays {
                        *day = (*day + offset) % 7;
                    }
                    weekdays.sort_unstable();
                    weekdays
                        .iter()
                        .map(i64::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                }
            };
            let (hour, minute) = time.split_once(':').ok_or_else(invalid)?;
            let hour = hour.parse::<u8>().map_err(|_| invalid())?;
            let minute = minute.parse::<u8>().map_err(|_| invalid())?;
            json!({"kind":"cron","expression":format!("{minute} {hour} {day} {month} {shifted}")})
        }
        _ => {
            return Ok(
                json!({"zone":zone,"nextAt":if routine["interval_ms"].is_null() {at} else {next},
            "intervalSeconds":routine["interval_ms"].as_i64().map(|value|value/1000)}),
            )
        }
    };
    Ok(json!({"zone":zone,"schedule":spec}))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DialogMode {
    Details,
    Edit,
    Move,
    Run,
}

#[component]
fn CalendarDialog(
    selected: Selection,
    live: Signal<Value>,
    zone: String,
    default_project: String,
    default_agent: String,
    agents: Vec<(String, String)>,
    projects: Vec<Value>,
    archived: bool,
    close: Callback<()>,
    saved: Callback<bool>,
) -> impl IntoView {
    let item = StoredValue::new(selected.item.unwrap_or_default());
    let existing = item.with_value(|item| !item.is_null());
    let is_workflow = item.with_value(|item| item["kind"] == "workflow");
    let editable = !archived
        && item.with_value(|item| {
            !existing
                || enabled(&item["editable"])
                    && !projects.iter().any(|project| {
                        project["id"] == item["project"] && enabled(&project["archived"])
                    })
        });
    let from = item.with_value(|item| number(&item["at"]));
    let slot = StoredValue::new(
        selected
            .slot
            .clone()
            .unwrap_or_else(|| wall_time(from, &zone)),
    );
    let mode = RwSignal::new(match (existing, selected.slot.is_some()) {
        (true, true) => DialogMode::Move,
        (true, false) => DialogMode::Details,
        _ => DialogMode::Edit,
    });
    let kind = RwSignal::new(if is_workflow { "workflow" } else { "routine" }.to_owned());
    let agents = StoredValue::new(agents);
    let projects = StoredValue::new(projects);
    let choose_project = !existing
        && default_project.is_empty()
        && projects.with_value(|projects| !projects.is_empty());
    let project = RwSignal::new(default_project);
    let available_agents = Signal::derive(move || {
        project.with(|project| {
            projects.with_value(|projects| {
                agents.with_value(|agents| {
                    let members = projects
                        .iter()
                        .find(|row| row["id"] == *project)
                        .map(|row| rows(&row["members"]));
                    agents
                        .iter()
                        .filter(|(id, _)| {
                            members
                                .is_none_or(|members| members.iter().any(|row| row["agent"] == *id))
                        })
                        .cloned()
                        .collect::<Vec<_>>()
                })
            })
        })
    });
    let selected_agent = RwSignal::new(default_agent);
    let owner = Signal::derive(move || {
        available_agents.with(|agents| {
            selected_agent.with(|selected| {
                agents
                    .iter()
                    .find(|(id, _)| id == selected)
                    .or_else(|| agents.first())
                    .map(|(id, _)| id.clone())
                    .unwrap_or_default()
            })
        })
    });
    let workflow = RwSignal::new(item.with_value(|item| {
        if is_workflow {
            text(&item["sourceId"]).to_owned()
        } else {
            String::new()
        }
    }));
    let routine_id = StoredValue::new(item.with_value(|item| {
        if is_workflow {
            String::new()
        } else {
            text(&item["sourceId"]).to_owned()
        }
    }));
    let zone = StoredValue::new(zone);
    let slot_anchor = RwSignal::new(None::<i64>);
    let slot_error = RwSignal::new(String::new());
    if !existing {
        let (day, time) = slot.get_value();
        let zone = zone.get_value();
        spawn_local(async move {
            match resolve(&day, &time, &zone).await {
                Ok(at) => {
                    slot_anchor.try_set(Some(at));
                }
                Err(error) => {
                    slot_error.try_set(error);
                }
            }
        });
    }
    let routine = ui::query::<Value>(move || {
        routine_id.with_value(|id| {
            if id.is_empty() || !editable {
                String::new()
            } else {
                format!("/api/routines/{id}")
            }
        })
    });
    let selected_workflow = ui::query::<Value>(move || {
        let id = workflow.get_untracked();
        if id.is_empty() || !editable {
            String::new()
        } else {
            format!("/api/workflows/{id}")
        }
    });
    let schedule = ui::query::<Value>(move || {
        let id = workflow.get_untracked();
        if id.is_empty() || !editable {
            String::new()
        } else {
            format!("/api/workflows/{id}/schedule")
        }
    });
    Effect::watch(
        move || workflow.get(),
        move |_, _, _| {
            selected_workflow.data.set(None);
            schedule.data.set(None);
            selected_workflow.reload.run(());
            schedule.reload.run(());
        },
        false,
    );
    let choices = RwSignal::new(None::<Vec<(String, String)>>);
    let choices_error = RwSignal::new(String::new());
    let choice_attempt = RwSignal::new(0_u64);
    let retry_choices = RwSignal::new(0_u64);
    if !existing {
        Effect::new(move |_| {
            let requested = kind.get() == "workflow";
            let project = project.get();
            retry_choices.track();
            choice_attempt.update(|attempt| *attempt += 1);
            let attempt = choice_attempt.get_untracked();
            choices.set(None);
            choices_error.set(String::new());
            if !requested {
                return;
            }
            let agents = agents.get_value();
            spawn_local(async move {
                let mut cursor = String::new();
                let mut values = Vec::new();
                loop {
                    let mut filters = Vec::new();
                    if !project.is_empty() {
                        filters.push(("project", project.as_str()));
                    }
                    if !cursor.is_empty() {
                        filters.push(("after", cursor.as_str()));
                    }
                    let result = api::read::<Value>(&url("/api/workflows", &filters)).await;
                    if choice_attempt.try_get_untracked() != Some(attempt) {
                        return;
                    }
                    match result {
                        Ok(page) => {
                            values.extend(
                                rows(&page["workflows"])
                                    .iter()
                                    .filter(|row| agents.iter().any(|(id, _)| row["agent"] == *id))
                                    .map(|row| {
                                        (text(&row["id"]).to_owned(), text(&row["name"]).to_owned())
                                    }),
                            );
                            cursor = text(&page["next_after"]).to_owned();
                            if cursor.is_empty() {
                                choices.try_set(Some(values));
                                break;
                            }
                        }
                        Err(error) => {
                            choices_error.try_set(error);
                            break;
                        }
                    }
                }
            });
        });
    }
    let timing = RwSignal::new(None::<Value>);
    let custom_move = RwSignal::new(false);
    let journal = Journal::new("minarbot:calendar-command".into(), calendar_request);
    let (confirm, confirmation) = kit::confirmation();
    let resolving = RwSignal::new(false);
    let reloaded = RwSignal::new(false);
    let conflict = RwSignal::new(false);
    let replay = Callback::new(move |request: Request| {
        let ran = enabled(&request.record["ran"]);
        journal.submit(request, Callback::new(move |_| saved.run(ran)));
    });
    let send = move |path: String, body: Value, ran: bool| {
        let request = Request::new(
            path.clone(),
            body,
            |body| json!({"path":path,"body":body,"ran":ran}),
        );
        journal.submit(request, Callback::new(move |_| saved.run(ran)));
    };
    let locked = Signal::derive(move || {
        resolving.get()
            || journal.locked()
            || conflict.get()
            || journal.conflict.with(Option::is_some)
    });
    let source_ready = move || {
        if kind.get() == "routine" {
            !existing || routine.data.with(Option::is_some)
        } else {
            selected_workflow.data.with(Option::is_some) && schedule.data.with(Option::is_some)
        }
    };
    let reload = move || {
        conflict.set(false);
        journal.conflict.set(None);
        journal.error.set(String::new());
        reloaded.set(true);
        routine.data.set(None);
        schedule.data.set(None);
        selected_workflow.data.set(None);
        routine.reload.run(());
        schedule.reload.run(());
        selected_workflow.reload.run(());
    };
    let submit = move |event| {
        let Some(data) = ui::form(&event) else {
            return;
        };
        if locked.get_untracked() {
            return;
        }
        let field = |name| ui::field(&data, name);
        let routine_row = routine.data.get_untracked().unwrap_or_default();
        let schedule_row = schedule.data.with_untracked(|value| {
            value
                .as_ref()
                .map_or(Value::Null, |value| value["schedule"].clone())
        });
        let workflow_id = workflow.get_untracked();
        let current_mode = mode.get_untracked();
        if current_mode == DialogMode::Edit && kind.get_untracked() == "routine" {
            let Some(timing) = timing.get_untracked() else {
                return;
            };
            if custom_move.get_untracked() && timing["schedule"] == routine_row["schedule"] {
                journal
                    .error
                    .set("Change the cron expression before saving.".into());
                return;
            }
            let mut fields = json!({"name":field("name").trim(),"prompt":field("prompt").trim()});
            let same_instant = existing
                && routine_row["schedule"].is_null()
                && routine_row["interval_ms"].is_null()
                && timing["schedule"]["kind"] == "once"
                && zone.with_value(|zone| {
                    timing["zone"] == routine_row["schedule_zone"].as_str().unwrap_or(zone)
                })
                && wall_time(number(&routine_row["next_at"]), text(&timing["zone"]))
                    == (
                        text(&timing["schedule"]["date"]).to_owned(),
                        text(&timing["schedule"]["time"]).to_owned(),
                    );
            if !same_instant {
                merge(&mut fields, timing);
            }
            if existing {
                let path = routine_id.with_value(|id| format!("/api/routines/{id}/edit"));
                let body = zone.with_value(|zone| routine_schedule(&routine_row, zone, fields));
                send(path, body, false);
            } else {
                fields["project"] =
                    project.with_untracked(|id| if valid_id(id) { json!(id) } else { Value::Null });
                fields["trigger"] = json!("schedule");
                fields["enabled"] = json!(true);
                send(
                    format!("/api/agents/{}/routines", owner.get_untracked()),
                    fields,
                    false,
                );
            }
            return;
        }

        let date = field("date");
        let time = field("time");
        let input = field("input");
        let minutes = field("minutes")
            .parse::<f64>()
            .ok()
            .map(|value| (value * 60.0).round() as i64);
        let home_zone = zone.get_value();
        let unchanged = (current_mode == DialogMode::Edit)
            .then(|| schedule_row["next_at"].as_i64())
            .flatten()
            .filter(|at| wall_time(*at, &home_zone) == (date.clone(), time.clone()));
        resolving.set(true);
        spawn_local(async move {
            let result = match unchanged {
                Some(at) => Ok(at),
                None => resolve(&date, &time, &home_zone).await,
            };
            if resolving.try_get_untracked().is_none() {
                return;
            }
            resolving.set(false);
            let at = match result {
                Ok(at) => at,
                Err(error) => {
                    journal.error.set(error);
                    return;
                }
            };
            if current_mode == DialogMode::Move {
                let changed = !reloaded.get_untracked()
                    && item.with_value(|item| {
                        !item["revision"].is_null()
                            && item["revision"]
                                != *if kind.get_untracked() == "routine" {
                                    &routine_row["revision"]
                                } else {
                                    &schedule_row["revision"]
                                }
                    });
                if changed {
                    journal
                        .error
                        .set("Schedule changed. Reload before moving it.".into());
                    conflict.set(true);
                    return;
                }
                if kind.get_untracked() == "routine" {
                    let fields = match move_routine(&routine_row, from, at, &home_zone) {
                        Ok(fields) => fields,
                        Err(error) => {
                            custom_move.set(true);
                            mode.set(DialogMode::Edit);
                            journal.error.set(error);
                            return;
                        }
                    };
                    send(
                        routine_id.with_value(|id| format!("/api/routines/{id}/edit")),
                        routine_schedule(&routine_row, &home_zone, fields),
                        false,
                    );
                } else {
                    let mut body = workflow_schedule(&schedule_row);
                    body["nextAt"] = json!(if schedule_row["interval_ms"].is_null() {
                        at
                    } else {
                        (number(&schedule_row["next_at"]) + at - from).max(1)
                    });
                    send(
                        format!("/api/workflows/{workflow_id}/schedule"),
                        body,
                        false,
                    );
                }
            } else {
                let mut body = workflow_schedule(&schedule_row);
                body["nextAt"] = json!(at);
                body["intervalSeconds"] = json!(minutes);
                body["input"] = json!(input);
                if !existing && !project.with_untracked(String::is_empty) {
                    body["project"] =
                        project.with_untracked(
                            |id| if valid_id(id) { json!(id) } else { Value::Null },
                        );
                }
                send(
                    format!("/api/workflows/{workflow_id}/schedule"),
                    body,
                    false,
                );
            }
        });
    };
    let toggle = move || {
        if let Some(row) = routine.data.get_untracked() {
            send(
                routine_id.with_value(|id| format!("/api/routines/{id}/edit")),
                zone.with_value(|zone| {
                    routine_schedule(&row, zone, json!({"enabled":!enabled(&row["enabled"])}))
                }),
                false,
            );
        } else if let Some(row) = schedule.data.get_untracked() {
            let mut body = workflow_schedule(&row["schedule"]);
            body["enabled"] = json!(!enabled(&row["schedule"]["enabled"]));
            send(
                format!("/api/workflows/{}/schedule", workflow.get_untracked()),
                body,
                false,
            );
        }
    };
    let run = move || {
        if let Some(row) = routine.data.get_untracked() {
            send(
                routine_id.with_value(|id| format!("/api/routines/{id}/test")),
                json!({"revision":row["revision"]}),
                true,
            );
        } else if let Some(row) = selected_workflow.data.get_untracked() {
            let input = schedule.data.with_untracked(|data| {
                data.as_ref()
                    .and_then(|data| data["schedule"]["input"].as_str().map(str::to_owned))
                    .unwrap_or_default()
            });
            send(
                format!("/api/workflows/{}/run", workflow.get_untracked()),
                json!({"revision":row["revision"],"input":input}),
                true,
            );
        }
    };
    let series = item.with_value(|item| enabled(&item["recurring"]));
    let move_label = if series {
        "Move series"
    } else {
        "Move occurrence"
    };
    let title = Signal::derive(move || match mode.get() {
        DialogMode::Details => item.with_value(|item| text(&item["title"]).to_owned()),
        DialogMode::Run => "Run inspector".into(),
        DialogMode::Move => move_label.into(),
        DialogMode::Edit => {
            if existing {
                "Edit schedule".into()
            } else {
                "Add to calendar".into()
            }
        }
    });
    let enabled_now = move || {
        routine
            .data
            .with(|value| value.as_ref().map(|value| enabled(&value["enabled"])))
            .or_else(|| {
                schedule.data.with(|value| {
                    value
                        .as_ref()
                        .map(|value| enabled(&value["schedule"]["enabled"]))
                })
            })
    };
    let ready = move || {
        source_ready()
            && (mode.get() == DialogMode::Move
                || kind.get() == "workflow"
                || timing.with(Option::is_some)
                    && (existing
                        || (slot_anchor.with(Option::is_some) && !owner.with(String::is_empty))))
    };
    view! {
        <kit::Dialog id="calendar-action" title open=Signal::stored(true) on_close=move || close.run(())
            locked=Signal::derive(move || resolving.get() || journal.busy.get())
            description=Signal::derive(move || if mode.get()==DialogMode::Move && series { "All future occurrences in this series will move.".into() } else { String::new() })
            footer=kit::content(move || view! {
                <Show when=move || mode.get()==DialogMode::Details && editable && source_ready()>
                    <kit::Button label=Signal::derive(move || if enabled_now()==Some(true) { "Pause schedule" } else { "Enable schedule" })
                        disabled=locked on_click=toggle />
                    <kit::Button label=kit::text("Run now") disabled=locked on_click=run />
                    <kit::Button label=kit::text("Edit schedule") variant="primary" disabled=locked on_click=move || mode.set(DialogMode::Edit) />
                </Show>
                <Show when=move || mode.get() == DialogMode::Run>
                    <kit::Button label=kit::text("Back to details") on_click=move || mode.set(DialogMode::Details) />
                </Show>
                <Show when=move || matches!(mode.get(),DialogMode::Edit|DialogMode::Move)>
                    <kit::Button label=kit::text(if existing {"Back"} else {"Cancel"}) disabled=journal.busy on_click=move || {
                        if existing { mode.set(DialogMode::Details); } else { close.run(()); }
                    } />
                    <button type="submit" form="calendar-action-form" class="k-btn k-btn--md k-btn--primary"
                        disabled=move || locked.get() || !ready()>{move || if mode.get()==DialogMode::Move { move_label } else { "Save schedule" }}</button>
                </Show>
            })>
            <Recovery journal done=Callback::new(|_| ()) replay confirm />
            <ReadError query=routine label="Reload saved schedule" /><ReadError query=selected_workflow label="Reload saved schedule" />
            <ReadError query=schedule label="Reload saved schedule" />
            <Show when=move || kind.get()=="routine">{kit::error(slot_error)}</Show>
            <Show when=move || !choices_error.with(String::is_empty)>
                <kit::Alert tone="error" actions=kit::content(move || view! {
                    <kit::Button label=kit::text("Retry") size="sm" on_click=move || retry_choices.update(|attempt| *attempt += 1) />
                })>{choices_error}</kit::Alert>
            </Show>
            <Show when=move || conflict.get() || journal.conflict.with(Option::is_some)>
                <kit::Button label=kit::text("Reload saved schedule") on_click=reload />
            </Show>
            {move || match mode.get() {
                DialogMode::Details => {
                    let item = live.get();
                    let mut context = agents.with_value(|agents| agents.iter().find(|(id, _)| item["agent"] == *id)
                        .map_or_else(String::new, |(_, name)| name.clone()));
                    if item["project"].is_string() {
                        projects.with_value(|projects| {
                            let project = projects.iter().find(|row| row["id"] == item["project"]);
                            if !context.is_empty() { context.push_str(" · "); }
                            context.push_str(project.map_or("Project", |row| text(&row["name"])));
                        });
                    }
                    let timing = routine.data.with(|row| row.as_ref().map(routines::trigger_label)).or_else(|| schedule.data.with(|row| {
                        row.as_ref().and_then(|row| row["schedule"]["interval_ms"].as_i64()).map(|ms| format!("Every {} minutes", ms as f64 / 60_000.0))
                    }));
                    calendar_details(item,zone,editable,mode,locked,context,timing).into_any()
                },
                DialogMode::Run => {
                    let run = live.with(|item| text(&item["run"]).to_owned());
                    view! { <div class="calendar-run">
                        {if is_workflow { workflows::run_details(run, None).into_any() }
                        else { routine_run_details(run).into_any() }}
                    </div> }.into_any()
                },
                DialogMode::Edit | DialogMode::Move => {
                    let moving = mode.get()==DialogMode::Move;
                    let routine_row = routine.data.get();
                    let flow_schedule = schedule.data.with(|data| data.as_ref().map_or(Value::Null, |data| data["schedule"].clone()));
                    let flow_schedule_input = flow_schedule.clone();
                    let is_routine = kind.get()=="routine";
                    let (day,time) = if moving || flow_schedule.is_null() {
                        slot.get_value()
                    } else { zone.with_value(|zone| wall_time(number(&flow_schedule["next_at"]),zone)) };
                    let routine_row = routine_row.unwrap_or_default();
                    let routine_prompt = routine_row.clone();
                    let routine_interval = routine_row["interval_ms"].as_i64();
                    let mut initial = routine_row["schedule"].clone();
                    if initial.is_null() {
                        initial = if let Some(ms) = routine_interval { json!({"kind":"legacy_interval","intervalMs":ms,"anchorAt":routine_row["next_at"]}) }
                            else if existing { json!({"kind":"instant","at":routine_row["next_at"]}) }
                            else { slot.with_value(|(date,time)| json!({"kind":"once","date":date,"time":time})) };
                    }
                    let at = routine_row["next_at"].as_i64().unwrap_or_else(|| slot.with_value(|(day,_)| parse_day(day)));
                    let routine_zone = routine_row["schedule_zone"].as_str().map(str::to_owned).unwrap_or_else(|| zone.get_value());
                    view! {
                        <form id="calendar-action-form" class="k-stack" on:submit=submit>
                            {(!existing).then(|| view! {
                                <kit::Field id="calendar-automation" label="Automation"><Select name="calendar-automation" value=kind.into()
                                    options=Signal::stored(options(&[("routine","New routine"),("workflow","Schedule workflow")]))
                                    disabled=locked change=Callback::new(move |value| { kind.set(value); workflow.set(String::new()); timing.set(None); }) /></kit::Field>
                                {choose_project.then(|| view! {
                                    <kit::Field id="calendar-dialog-project" label="Project"><Select name="calendar-dialog-project" value=project.into()
                                        options=Signal::derive(move || projects.with_value(|projects| {
                                            let mut options=if kind.get()=="routine" {options(&[("","No project")])}
                                                else {options(&[("","Keep workflow project"),("none","No project")])};
                                            options.extend(projects.iter().filter(|row| !enabled(&row["archived"]))
                                                .map(|row| (text(&row["id"]).to_owned(),text(&row["name"]).to_owned())));
                                            options
                                        })) disabled=locked change=Callback::new(move |value| { project.set(value); workflow.set(String::new()); }) />
                                    </kit::Field>
                                })}
                                {if is_routine {
                                    view! { <kit::Field id="calendar-owner" label="Agent"><Select name="calendar-owner" value=owner
                                        options=available_agents disabled=Signal::derive(move || locked.get() || available_agents.with(Vec::is_empty))
                                        change=Callback::new(move |value| selected_agent.set(value)) /></kit::Field> }.into_any()
                                } else {
                                    view! { <kit::Field id="calendar-workflow" label="Workflow"><Select name="calendar-workflow" value=workflow.into()
                                        options=Signal::derive(move || {
                                            let mut options = options(&[("","Choose workflow")]);
                                            options.extend(choices.get().unwrap_or_default()); options
                                        }) disabled=locked change=Callback::new(move |value| workflow.set(value)) /></kit::Field> }.into_any()
                                }}
                            })}
                            {if moving || !is_routine {
                                view! {
                                    <div class="k-grid-2"><kit::Field id="calendar-date" label="Date"><input id="calendar-date" name="date" type="date" class="k-input" value=day disabled=locked required /></kit::Field>
                                    <kit::Field id="calendar-time" label="Time"><input id="calendar-time" name="time" type="time" class="k-input" value=time disabled=locked required /></kit::Field></div>
                                    {(!moving).then(|| view! {
                                        <kit::Field id="calendar-repeat" label="Repeat every (minutes)"><input id="calendar-repeat" name="minutes" class="k-input" type="number" min="1" max="525600" step="any"
                                            value=flow_schedule["interval_ms"].as_i64().map(|value| (value as f64 / 60_000.0).to_string()).unwrap_or_default() disabled=locked /></kit::Field>
                                        <kit::Field id="calendar-input" label="Scheduled input"><textarea id="calendar-input" class="k-input k-textarea" name="input" maxlength="16000" disabled=locked>{text(&flow_schedule_input["input"]).to_owned()}</textarea></kit::Field>
                                    })}
                                }.into_any()
                            } else if !existing || !routine_row.is_null() {
                                view! {
                                    <kit::Field id="calendar-name" label="Name"><input id="calendar-name" name="name" class="k-input" maxlength="80" required disabled=locked value=text(&routine_row["name"]).to_owned() /></kit::Field>
                                    <kit::Field id="calendar-prompt" label="Prompt"><textarea id="calendar-prompt" name="prompt" class="k-input k-textarea" maxlength="16000" required disabled=locked>{text(&routine_prompt["prompt"]).to_owned()}</textarea></kit::Field>
                                    <schedule::ScheduleFields initial next_at=at interval_ms=routine_interval zone=routine_zone
                                        interval_anchor=slot_anchor.into() active=Signal::stored(true) locked output=timing />
                                }.into_any()
                            } else { kit::skeleton(2,"Loading schedule...").into_any() }}
                        </form>
                    }.into_any()
                }
            }}
        </kit::Dialog>
        {confirmation}
    }
}

fn calendar_details(
    item: Value,
    zone: StoredValue<String>,
    editable: bool,
    mode: RwSignal<DialogMode>,
    locked: Signal<bool>,
    context: String,
    timing: Option<String>,
) -> impl IntoView {
    let zone = zone.get_value();
    let agent = text(&item["agent"]);
    let run = text(&item["run"]);
    let is_routine = item["kind"] == "routine";
    let at = number(&item["at"]);
    let time = schedule::format(
        at,
        "en-US",
        &[
            ("timeZone", &zone),
            ("dateStyle", "full"),
            ("timeStyle", "short"),
        ],
    );
    view! {
        <div class="k-stack">
            <div class="k-inline">{kit::badge("neutral",if is_routine { "Routine" } else { "Workflow" })}
                {kit::badge(match text(&item["status"]) { "done" => "success", "failed" => "warn", _ => "neutral" },status(text(&item["status"])).to_owned())}</div>
            <p class="calendar-detail-time"><time datetime=iso(at)>{time}</time>{format!(" · {zone}")}</p>
            {(!context.is_empty()).then(|| view! { <p class="k-field__hint">{context}</p> })}
            {timing.map(|timing| view! { <p>{timing}</p> })}
            {(number(&item["count"])>1).then(|| view! { <p>{format!("{} occurrences on this date.",number(&item["count"]))}</p> })}
            <div class="k-row-actions">
                {valid_id(run).then(|| view! {
                    <kit::Button label=kit::text(if is_routine {"Open Run inspector"} else {"Open workflow run"}) on_click=move || mode.set(DialogMode::Run) />
                })}
                {item["thread"].as_str().filter(|id| valid_id(id)).map(|thread| view! {
                    <a class="k-btn k-btn--md k-btn--secondary" href=conversation(agent,thread)>"Open conversation"</a>
                })}
                {item["reportTo"].as_str().zip(item["report"].as_str()).filter(|(id,turn)| {
                    (*id=="direct" || valid_id(id)) && valid_id(turn)
                }).map(|(id,turn)| {
                    let (href,scope)=if id=="direct" {(format!("/agents/{agent}"),agent.to_owned())}
                        else {(format!("/rooms/{id}"),format!("channel:{id}"))};
                    let turn=turn.to_owned();
                    view! { <a class="k-btn k-btn--md k-btn--secondary" href=href on:click=move |_| {
                        if let Ok(storage)=storage() {let _=storage.set_item(&format!("minarbot:focus:{scope}"),&turn);}
                    }>"Open report"</a> }
                })}
                {(run.is_empty() && editable).then(|| view! {
                    <kit::Button label=kit::text("Move schedule") disabled=locked on_click=move || mode.set(DialogMode::Move) />
                })}
            </div>
        </div>
    }
}

fn inspect_state(state: &str, run: &str) -> &'static str {
    match (state, run) {
        ("completed" | "done" | "received" | "succeeded", _) => "Completed",
        ("failed" | "error" | "uncertain", _) => "Failed",
        ("declined" | "denied" | "rejected", _) => "Declined",
        ("waiting" | "waiting_for_user" | "approval" | "paused", _) => "Waiting",
        ("cancelled" | "canceled" | "closed", _) | ("interrupted", "cancelled") => "Stopped",
        ("interrupted" | "aborted", _) => "Interrupted",
        (_, "failed") => "Failed",
        (_, "interrupted") => "Interrupted",
        (_, "cancelled") => "Stopped",
        ("running" | "in_progress" | "inProgress", _) => "Running",
        _ => "Pending",
    }
}

fn routine_run_details(id: String) -> impl IntoView {
    let query = ui::query::<Value>(move || format!("/api/turns/{id}/inspect"));
    poll(query.reload, 1200);
    view! {
        <div class="right-panel__body run-inspector">
            <ReadError query />
            {move || match query.data.get() {
                Some(run) => {
                    let native=rows(&run["harness"]);
                    let tools=rows(&run["tools"]);
                    let judgments=rows(&run["judgments"]);
                    let providers=rows(&run["providers"]);
                    let summary=judgments.is_empty() && providers.is_empty() && native.is_empty() && tools.is_empty()
                        && run["engineStderr"].is_null() && run["error"].is_null();
                    view! {
                        {run["error"].as_str().map(str::to_owned).map(|error| view! { <kit::Alert tone="error">{error}</kit::Alert> })}
                        {(!judgments.is_empty()).then(|| view! {
                            <section class="run-inspector__providers" aria-label="Goal judge"><h3>"Goal judge"</h3><ul class="run-inspector__list">
                                {judgments.iter().map(|judge| view! {
                                    <li><details><summary><ui::Icon name="chevron-right" /><span>{format!("Turn {}",number(&judge["number"]))}</span>
                                        <span class="run-inspector__state">{if enabled(&judge["met"]) {"Met"} else {"More work"}}</span>
                                    </summary><div class="run-inspector__details"><p>{text(&judge["reason"]).to_owned()}</p></div></details></li>
                                }).collect_view()}
                            </ul></section>
                        })}
                        {(!(native.is_empty() && tools.is_empty())).then(|| view! {
                            <ul class="run-inspector__list" aria-label="Steps">
                                {tools.iter().filter(|tool| !native.iter().any(|step| step["id"]==tool["id"])).chain(native).map(|step| {
                                    let label=step["title"].as_str().unwrap_or_else(||text(&step["name"])).to_owned();
                                    let state=inspect_state(if enabled(&step["failed"]) {"failed"} else {text(&step["state"])}, text(&run["status"]));
                                    let failed = state == "Failed";
                                    view! {
                                        <li><details><summary><ui::Icon name="chevron-right" /><span>{label}</span><span class="run-inspector__state" data-state=failed.then_some("Failed")>{state}</span></summary>
                                            <div class="run-inspector__details">
                                                <dl><dt>"Tool"</dt><dd>{text(&step["name"]).to_owned()}</dd>
                                                  <dt>"Started"</dt><dd>{inspect_time(&step["started"])}</dd>
                                                  <dt>"Ended"</dt><dd>{inspect_time(&step["finished"])}</dd></dl>
                                                {inspect_value("Arguments",&step["arguments"])}{inspect_value("Result",&step["result"])}
                                            </div>
                                        </details></li>
                                    }
                                }).collect_view()}
                            </ul>
                        })}
                        {(!providers.is_empty()).then(|| view! {
                            <section class="run-inspector__providers" aria-label="Provider calls"><h3>"Provider calls"</h3><ul class="run-inspector__list">
                                {providers.iter().map(|call| {
                                    let duration=call["duration_ms"].as_i64().map(|duration| if duration<1000 {format!(" · {duration} ms")}
                                        else {format!(" · {:.2} s",duration as f64 / 1000.0)}).unwrap_or_default();
                                    view! { <li><details><summary><ui::Icon name="chevron-right" /><span>{text(&call["model"]).to_owned()}</span>
                                        <span class="run-inspector__state">{format!("{}{duration}",inspect_state(text(&call["status"]), text(&run["status"])))}</span>
                                    </summary><div class="run-inspector__details"><dl>
                                        <dt>"Started"</dt><dd>{inspect_time(&call["created"])}</dd>
                                        <dt>"Ended"</dt><dd>{inspect_time(&call["finished"])}</dd>
                                        {[( "input_tokens","Input tokens"),("output_tokens","Output tokens"),("total_tokens","Total tokens")].into_iter()
                                            .filter_map(|(key,label)| call[key].as_i64().map(|tokens| view! { <dt>{label}</dt><dd>{tokens}</dd> })).collect_view()}
                                    </dl></div></details></li> }
                                }).collect_view()}
                            </ul></section>
                        })}
                        {inspect_value("Engine stderr",&run["engineStderr"])}
                        {summary.then(|| view! { <p role="status">"No run details"</p> })}
                    }.into_any()
                }
                None if !query.error.with(String::is_empty) => ().into_any(),
                None=>kit::skeleton(3,"Loading run inspector...").into_any(),
            }}
        </div>
    }
}

fn inspect_time(value: &Value) -> String {
    let at = js_sys::Date::parse(text(value));
    if at.is_finite() {
        schedule::format(
            at as i64,
            "en-US",
            &[("dateStyle", "short"), ("timeStyle", "medium")],
        )
    } else {
        "Unavailable".into()
    }
}

fn inspect_value(label: &'static str, value: &Value) -> impl IntoView {
    value.as_str().filter(|value| !value.is_empty()).map(|value| {
        let full=value.to_owned();
        let preview=full.chars().take(640).collect::<String>().lines().take(8).collect::<Vec<_>>().join("\n");
        let expanded=RwSignal::new(false);
        let truncated=preview.len()<full.len();
        view! {
            <div class="run-inspector__value">
                <div class="run-inspector__value-head"><strong>{label}</strong>
                    {truncated.then(|| view! { <button type="button" class="link-btn link-btn--small" aria-expanded=move || expanded.get().to_string()
                        on:click=move |_| expanded.update(|value| *value = !*value)>{move || if expanded.get() {"Collapse"}else{"Expand"}}
                        <span class="sr-only">{format!(" {}",label.to_lowercase())}</span></button> })}
                </div>
                <pre><code>{move || if expanded.get() || !truncated {full.clone()}else{format!("{preview}…")}}</code></pre>
            </div>
        }
    })
}
