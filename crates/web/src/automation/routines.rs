use super::*;
use schedule::{format, merge, routine_timing, ScheduleFields};

const HUB_KEY: &str = "minarbot:routine-hub-command";
const TEMPLATES: &[(&str, &str, &str)] = &[
    (
        "slack",
        "Slack message",
        "Summarize the Slack message and flag any follow-up.",
    ),
    (
        "linear",
        "Linear issue",
        "Summarize the Linear issue and identify its next step.",
    ),
    (
        "sentry",
        "Sentry alert",
        "Summarize the Sentry alert and suggest a first diagnostic step.",
    ),
    (
        "github",
        "GitHub issue",
        "Summarize the GitHub issue and identify its next step.",
    ),
    ("schedule", "Schedule", ""),
];

fn hub_request(_: &str, value: Value) -> Result<Request, String> {
    let id = text(&value["routine"]);
    let action = text(&value["action"]);
    if !valid_id(id)
        || !matches!(action, "test" | "edit" | "delete")
        || value["data"]["revision"].as_i64().is_none()
    {
        return Err("Saved routine request could not be read.".into());
    }
    Request::restore(
        format!("/api/routines/{id}/{action}"),
        value["data"].clone(),
        value,
    )
}

fn hub_command(id: &str, action: &str, body: Value) -> Request {
    Request::new(
        format!("/api/routines/{id}/{action}"),
        body,
        |body| json!({"routine":id,"action":action,"data":body}),
    )
}

fn editor_request(key: &str, value: Value) -> Result<Request, String> {
    let owner = key
        .strip_prefix("minarbot:routine-command:")
        .filter(|owner| valid_id(owner))
        .ok_or("Saved routine owner could not be read.")?;
    let body = &value["data"];
    if !body["name"].is_string() || !body["prompt"].is_string() || !body["enabled"].is_boolean() {
        return Err("Saved routine request could not be read.".into());
    }
    let path = if value["routineId"].is_null() {
        format!("/api/agents/{owner}/routines")
    } else {
        let id = text(&value["routineId"]);
        if !valid_id(id) || body["revision"].as_i64().is_none() {
            return Err("Saved routine request could not be read.".into());
        }
        format!("/api/routines/{id}/edit")
    };
    Request::restore(path, body.clone(), value)
}

fn edit_payload(routine: &Value, active: bool) -> Value {
    let mut body = json!({"revision":routine["revision"],"name":routine["name"],"prompt":routine["prompt"],
        "project":routine["project"],"reportTo":routine["report_to"],"enabled":active,"trigger":routine["trigger"]});
    merge(&mut body, routine_timing(routine, "UTC"));
    body
}

fn routine_status(row: &Value, list: bool) -> &str {
    let (state, paused, quiet) = if list {
        ("last_status", "last_paused", "last_no_update")
    } else {
        ("status", "paused", "no_update")
    };
    if enabled(&row[paused]) {
        "Paused"
    } else if enabled(&row[quiet]) {
        "No updates"
    } else {
        row[state].as_str().map(status).unwrap_or("Never run")
    }
}

fn report(routine: &Value) -> String {
    match text(&routine["report_to"]) {
        "direct" => "Owner agent's direct chat".into(),
        "run" | "" => "Run conversation".into(),
        _ => routine["report_section"]
            .as_str()
            .unwrap_or("Section")
            .to_owned(),
    }
}

fn date_text(at: i64) -> String {
    format(at, "en-US", &[])
}

fn relative(at: i64) -> String {
    use js_sys::{Array, Intl, Object, Reflect};
    let delta = (at as f64 - js_sys::Date::now()) / 1000.0;
    let absolute = delta.abs();
    let (amount, unit) = if absolute < 60.0 {
        (delta, "second")
    } else if absolute < 3600.0 {
        (delta / 60.0, "minute")
    } else if absolute < 86400.0 {
        (delta / 3600.0, "hour")
    } else {
        (delta / 86400.0, "day")
    };
    let opts = Object::new();
    let _ = Reflect::set(&opts, &"numeric".into(), &"auto".into());
    Intl::RelativeTimeFormat::new(&Array::of1(&"en".into()), &opts)
        .format(amount.round(), unit)
        .as_string()
        .unwrap_or_default()
}

pub(super) fn trigger_label(routine: &Value) -> String {
    match text(&routine["trigger"]) {
        "manual" => "Manual".into(),
        "script" => "Local script".into(),
        "webhook" => TEMPLATES
            .iter()
            .find(|(id, _, _)| routine["webhook_source"] == *id)
            .map_or("Webhook", |(_, name, _)| *name)
            .into(),
        _ => {
            let spec = &routine["schedule"];
            let zone = routine["schedule_zone"].as_str().unwrap_or("UTC");
            match text(&spec["kind"]) {
                "cron" => {
                    let parts = text(&spec["expression"])
                        .split_whitespace()
                        .collect::<Vec<_>>();
                    match parts.as_slice() {
                        [minute, hour, "*", "*", "*"]
                            if minute.parse::<u8>().is_ok() && hour.parse::<u8>().is_ok() =>
                        {
                            format!("Every day at {hour:0>2}:{minute:0>2} ({zone})")
                        }
                        [minute, hour, "*", "*", "1-5"]
                            if minute.parse::<u8>().is_ok() && hour.parse::<u8>().is_ok() =>
                        {
                            format!("Weekdays at {hour:0>2}:{minute:0>2} ({zone})")
                        }
                        _ => format!("Cron {} ({zone})", text(&spec["expression"])),
                    }
                }
                "interval" => {
                    let amount = number(&spec["amount"]);
                    format!(
                        "Every {amount} {}{}",
                        text(&spec["unit"]),
                        if amount == 1 { "" } else { "s" }
                    )
                }
                _ => routine["interval_ms"].as_i64().map_or_else(
                    || {
                        format!(
                            "Once on {} ({zone})",
                            format(number(&routine["next_at"]), "en-GB", &[("timeZone", zone)])
                        )
                    },
                    |ms| format!("Every {} minutes", ms as f64 / 60_000.0),
                ),
            }
        }
    }
}

#[derive(Clone)]
enum Editor {
    New(Option<&'static str>),
    Edit(Value),
}

pub(crate) fn routines_page() -> impl IntoView {
    let agents = Signal::derive(directory);
    let projects = ui::query::<Value>(|| "/api/projects".into());
    let editing = RwSignal::new(None::<Editor>);
    let template = RwSignal::new(false);
    let open = RwSignal::new(None::<Value>);
    let webhook = RwSignal::new(false);
    let search = RwSignal::new(String::new());
    let agent = RwSignal::new(
        window()
            .location()
            .search()
            .ok()
            .and_then(|query| {
                web_sys::UrlSearchParams::new_with_str(&query)
                    .ok()?
                    .get("agent")
            })
            .filter(|id| valid_id(id))
            .unwrap_or_default()
            .to_ascii_lowercase(),
    );
    let state = RwSignal::new(String::new());
    let trigger = RwSignal::new(String::new());
    let project = RwSignal::new(String::new());
    let cursors = RwSignal::new(Vec::<String>::new());
    let filter = Memo::new(move |_| {
        (
            search.get(),
            agent.get(),
            state.get(),
            trigger.get(),
            project.get(),
        )
    });
    let query = ui::query::<Value>(move || {
        filter.with_untracked(|(search, agent, state, trigger, project)| {
            let after =
                cursors.with_untracked(|cursors| cursors.last().cloned().unwrap_or_default());
            url(
                "/api/routines",
                &[
                    ("search", search),
                    ("agent", agent),
                    ("enabled", state),
                    ("trigger", trigger),
                    ("project", project),
                    ("after", &after),
                ],
            )
        })
    });
    Effect::watch(
        move || filter.get(),
        move |_, _, _| {
            cursors.set(Vec::new());
            query.data.set(None);
            query.reload.run(());
        },
        false,
    );
    poll(query.reload, 1200);
    let hub = Journal::new(HUB_KEY.to_owned(), hub_request);
    let (confirm, confirmation) = kit::confirmation();
    let notice = RwSignal::new(String::new());
    let done = Callback::new(move |value: Value| {
        if enabled(&value["deleted"]) {
            open.set(None);
            notice.set("Routine deleted. Existing run conversations are retained.".into());
        } else if value["thread"].as_str().is_some_and(valid_id) {
            notice.set("Run accepted. Open Overview to follow its progress.".into());
        } else {
            notice.set("Routine updated.".into());
            open.update(|open| {
                if let Some(row) = open.as_mut().filter(|row| row["id"] == value["id"]) {
                    merge(row, value.clone());
                }
            });
        }
        query.reload.run(());
    });
    Effect::new(move |_| {
        if let Some(data) = query.data.get() {
            if let Some(current) = open.get_untracked() {
                if let Some(row) = rows(&data["routines"]).iter().find(|row| {
                    row["id"] == current["id"]
                        && number(&row["revision"]) >= number(&current["revision"])
                }) {
                    if row != &current {
                        open.set(Some(row.clone()));
                    }
                }
            }
        }
    });
    let edit_saved = Callback::new(move |value: Value| {
        let native_webhook =
            open.with_untracked(Option::is_none) && value["webhook_source"].is_string();
        let updated =
            open.with_untracked(|open| open.as_ref().is_some_and(|row| row["id"] == value["id"]));
        if native_webhook {
            open.set(Some(value));
            webhook.set(true);
        } else if updated {
            open.update(|row| {
                if let Some(row) = row {
                    merge(row, value);
                }
            });
        }
        editing.set(None);
        query.reload.run(());
    });
    let locked = Signal::derive(move || hub.locked());
    let back = Callback::new(move |_| {
        if webhook.get_untracked() {
            webhook.set(false);
        } else {
            open.set(None);
            query.reload.run(());
        }
    });
    let active_filters = move || {
        filter.with(|parts| {
            !parts.0.is_empty()
                || !parts.1.is_empty()
                || !parts.2.is_empty()
                || !parts.3.is_empty()
                || !parts.4.is_empty()
        })
    };
    let show_filters = move || {
        active_filters()
            || query.data.with(|value| {
                value
                    .as_ref()
                    .is_some_and(|value| !rows(&value["routines"]).is_empty())
            })
    };
    let selected_owner = Signal::derive(move || {
        let agents = agents.get();
        if let Some((id, _)) = agents.iter().find(|(id, _)| {
            storage()
                .ok()
                .and_then(|storage| {
                    storage
                        .get_item(&format!("minarbot:routine-command:{id}"))
                        .ok()
                        .flatten()
                })
                .is_some()
        }) {
            return id.clone();
        }
        let selected = agent.get();
        if selected.is_empty() {
            agents.first().map(|(id, _)| id.clone()).unwrap_or_default()
        } else {
            selected
        }
    });
    let paging = move |next| {
        if next {
            if let Some(cursor) = query.data.with_untracked(|value| {
                value
                    .as_ref()
                    .and_then(|value| value["next_after"].as_str())
                    .map(str::to_owned)
            }) {
                cursors.update(|cursors| cursors.push(cursor));
            }
        } else {
            cursors.update(|cursors| {
                cursors.pop();
            });
        }
        query.data.set(None);
        query.reload.run(());
    };
    route_page(view! {
        <Show when=move || open.with(Option::is_none)>
            <div class="k-page bs-root routines-hub">
                <PageHead title="Routines" actions=kit::content(move || view! {
                    <Show when=move || !agents.with(Vec::is_empty)>
                        <kit::Button label=kit::text("New from template") disabled=locked on_click=move || template.set(true) />
                        <Show when=show_filters>
                            <button type="button" class="k-btn k-btn--md k-btn--primary" disabled=locked
                                on:click=move |_| editing.set(Some(Editor::New(None)))>{kit::icon(kit::icons().plus)}"New routine"</button>
                        </Show>
                    </Show>
                }) />
                <Show when=show_filters>
                    <div class="routine-filters">
                        <kit::Field id="routine-search" label="Search routines"><input id="routine-search" class="k-input" type="search" maxlength="80"
                            placeholder="Search by name" prop:value=search on:input=move |event| search.set(event_target_value(&event)) /></kit::Field>
                        <kit::Field id="routine-agent" label="Agent"><Select name="routine-agent" value=agent.into()
                            options=Signal::derive(move || {let mut list=options(&[("","All agents")]);list.extend(agents.get());list})
                            change=Callback::new(move |value| agent.set(value)) /></kit::Field>
                        <kit::Field id="routine-status" label="Status"><Select name="routine-status" value=state.into()
                            options=Signal::stored(options(&[("","All statuses"),("1","Enabled"),("0","Disabled")]))
                            change=Callback::new(move |value| state.set(value)) /></kit::Field>
                        <kit::Field id="routine-project" label="Project"><Select name="routine-project" value=project.into()
                            options=Signal::derive(move || {
                                let mut list=options(&[("","All projects"),("none","No project")]);
                                if let Some(value)=projects.data.get(){list.extend(project_options(&value));} list
                            }) change=Callback::new(move |value| project.set(value)) /></kit::Field>
                        <kit::Field id="routine-trigger" label="Trigger"><Select name="routine-trigger" value=trigger.into()
                            options=Signal::stored(options(&[("","All triggers"),("once","Once"),("recurring","Recurring schedule"),("webhook","Webhook"),("manual","Manual"),("script","Local script")]))
                            change=Callback::new(move |value| trigger.set(value)) /></kit::Field>
                    </div>
                </Show>
                <ReadError query label="Reload routines" />
                <p role="status" class="k-field__hint" hidden=move || notice.with(String::is_empty)>{notice}</p>
                {move || match query.data.get() {
                    Some(data) if rows(&data["routines"]).is_empty()=>view! {
                        <div class="k-empty">
                            <div class="k-empty__title">{if active_filters() {"No matching routines"} else {"No routines yet"}}</div>
                            {if active_filters() {
                                view! { <kit::Button label=kit::text("Clear filters") on_click=move || {
                                    search.set(String::new());agent.set(String::new());state.set(String::new());trigger.set(String::new());project.set(String::new());
                                } /> }.into_any()
                            } else {
                                view! { <Show when=move || !agents.with(Vec::is_empty)>
                                    <button type="button" class="k-btn k-btn--md k-btn--primary" disabled=locked
                                on:click=move |_| editing.set(Some(Editor::New(None)))>{kit::icon(kit::icons().plus)}"New routine"</button>
                                </Show> }.into_any()
                            }}
                        </div>
                    }.into_any(),
                    Some(data)=>view! {
                        <div class="k-table-wrap" role="region" aria-label="Routines table" tabindex="0"><table class="k-table routine-table"><thead><tr>
                            <th>"Routine"</th><th>"Owner"</th><th>"Project"</th><th>"Report to"</th><th>"Trigger"</th><th>"Last run"</th><th>"Next run"</th><th>"Enabled"</th>
                        </tr></thead><tbody>
                            {rows(&data["routines"]).iter().map(|row| {
                                let value=row.clone();
                                let switch_value=row.clone();
                                let name=text(&row["name"]).to_owned();
                                let id=text(&row["id"]).to_owned();
                                let owner=text(&row["owner_name"]).to_owned();
                                let active=enabled(&row["enabled"]);
                                let done_loop=row["loop"]["reason"].is_string();
                                let at=number(&row["next_at"]);
                                let avatar=ui::local_agents().into_iter().find(|agent| row["agent"]==agent.id);
                                let flow_agents=number(&row["flow_agents"]);
                                let last=last_run_link(row);
                                view! { <tr>
                                    <td><button type="button" class="routine-name k-link" on:click=move |_| {
                                        notice.set(String::new());open.set(Some(value.clone()));
                                    }>{name.clone()}</button>{(flow_agents>1).then(|| kit::badge("neutral",format!("{flow_agents} agents")))}</td>
                                    <td><span class="routine-owner">{avatar.map(|agent| {
                                        let first=agent.name.chars().next().map(|value|value.to_uppercase().to_string());
                                        let img=(agent.avatar_revision>0).then(||format!("/api/agents/{}/avatar/image?v={}",agent.id,agent.avatar_revision));
                                        view! { <span class="avatar bs-avatar routine-avatar" aria-hidden="true">
                                            {img.map_or_else(||first.into_any(),|src|view!{<img src=src alt="" />}.into_any())}
                                        </span> }
                                    })}<span>{owner}</span></span></td>
                                    <td>{row["project_name"].as_str().unwrap_or("No project").to_owned()}</td>
                                    <td>{report(row)}</td><td>{trigger_label(row)}</td><td>{last}</td>
                                    <td>{if active && row["trigger"]=="schedule" {view! { <time datetime=iso(at) title=date_text(at)>{relative(at)}</time> }.into_any()}
                                        else {view! { <span class="k-field__hint">{if active {"On demand"} else if done_loop {"Finished"} else {"Paused"}}</span> }.into_any()}}</td>
                                    <td><ui::Switch label=format!("Enable {name}") checked=Signal::stored(active)
                                        disabled=Signal::derive(move || done_loop || locked.get()) change=Callback::new(move |active| {
                                            hub.submit(hub_command(&id,"edit",edit_payload(&switch_value,active)),done);
                                        }) /></td>
                                </tr> }
                            }).collect_view()}
                        </tbody></table></div>
                    }.into_any(),
                    None if !query.error.with(String::is_empty)=>().into_any(),
                    None=>kit::skeleton(4,"Loading routines...").into_any(),
                }}
                <div class="k-pager" hidden=move || cursors.with(Vec::is_empty) && query.data.with(|data| data.as_ref().is_none_or(|data| data["next_after"].is_null()))>
                    <button type="button" class="k-icon-btn" aria-label="Previous routines" disabled=move || cursors.with(Vec::is_empty)
                        on:click=move |_| paging(false)>{kit::icon(kit::icons().left)}</button>
                    <button type="button" class="k-icon-btn" aria-label="Next routines" disabled=move || query.data.with(|data| data.as_ref().is_none_or(|data| data["next_after"].is_null()))
                        on:click=move |_| paging(true)>{kit::icon(kit::icons().right)}</button>
                </div>
            </div>
        </Show>
        <Recovery journal=hub done confirm />
        <Show when=move || open.with(Option::is_some) && !webhook.get()>
            <RoutineDetail open hub done notice back confirm edit=Callback::new(move |_| {
                if let Some(row)=open.get_untracked(){editing.set(Some(Editor::Edit(row)));}
            }) webhook=Callback::new(move |_| webhook.set(true)) />
        </Show>
        {move || webhook.get().then(|| open.get_untracked().map(|row| view! { <Webhook row back confirm /> })).flatten()}
        <kit::Dialog id="routine-template-picker" title=kit::text("New from template") open=template on_close=move || template.set(false)>
            <kit::Card>{TEMPLATES.iter().map(|(id,name,_)| {
                let id=*id;
                view! { <ui::Row title=*name icon=id click=Callback::new(move |_| {
                    template.set(false);editing.set(Some(Editor::New(Some(id))));
                }) /> }
            }).collect_view()}</kit::Card>
        </kit::Dialog>
        {move || editing.get().map(|editor| {
            let owner=match &editor { Editor::Edit(row)=>text(&row["agent"]).to_owned(), Editor::New(_)=>selected_owner.get_untracked() };
            view! { <RoutineEditor owner editor projects confirm saved=edit_saved close=Callback::new(move |_| editing.set(None)) /> }
        })}
        {confirmation}
    })
}

fn project_options(value: &Value) -> Vec<(String, String)> {
    rows(&value["projects"])
        .iter()
        .map(|value| {
            (
                text(&value["id"]).to_owned(),
                text(&value["name"]).to_owned(),
            )
        })
        .collect()
}

fn last_run_link(row: &Value) -> impl IntoView {
    let label = routine_status(row, true).to_owned();
    let created = text(&row["last_created"]).to_owned();
    let at = js_sys::Date::parse(&created);
    let agent = text(&row["agent"]);
    if let Some(thread) = row["last_thread"]
        .as_str()
        .filter(|thread| valid_id(thread))
    {
        view! { <a class="routine-run k-link" href=conversation(agent,thread)><span>{label}</span>
            <time datetime=created>{if at.is_finite(){relative(at as i64)}else{String::new()}}</time></a> }.into_any()
    } else {
        view! { <span class="k-field__hint">"Never run"</span> }.into_any()
    }
}

#[component]
fn RoutineEditor(
    owner: String,
    editor: Editor,
    projects: ui::Query<Value>,
    confirm: kit::Confirm,
    saved: Callback<Value>,
    close: Callback<()>,
) -> impl IntoView {
    let owner = RwSignal::new(owner);
    let journal = Journal::new(
        format!("minarbot:routine-command:{}", owner.get_untracked()),
        editor_request,
    );
    let (row, template) = match editor {
        Editor::New(template) => (Value::Null, template),
        Editor::Edit(row) => (row, None),
    };
    let pending = journal.pending.with_untracked(|pending| {
        pending
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .map(|request| request.record.clone())
            .unwrap_or_default()
    });
    let initial = &pending["data"];
    let routine_id = pending["routineId"]
        .as_str()
        .or(row["id"].as_str())
        .map(str::to_owned);
    let is_edit = routine_id.is_some();
    let routine_id = StoredValue::new(routine_id);
    let revision = initial["revision"]
        .as_i64()
        .or(row["revision"].as_i64())
        .unwrap_or_default();
    let preset = template.and_then(|id| TEMPLATES.iter().find(|(key, _, _)| *key == id));
    let source = initial["webhookSource"]
        .as_str()
        .or(row["webhook_source"].as_str())
        .or(template.filter(|id| *id != "schedule"))
        .map(str::to_owned);
    let webhook_label = TEMPLATES
        .iter()
        .find(|(id, _, _)| source.as_deref() == Some(*id))
        .map_or("Webhook", |(_, name, _)| *name);
    let trigger = RwSignal::new(
        initial["trigger"]
            .as_str()
            .or(row["trigger"].as_str())
            .unwrap_or(if source.is_some() {
                "webhook"
            } else {
                "schedule"
            })
            .to_owned(),
    );
    let name = initial["name"]
        .as_str()
        .or(row["name"].as_str())
        .or_else(|| preset.map(|(_, name, _)| *name))
        .unwrap_or_default()
        .to_owned();
    let prompt = initial["prompt"]
        .as_str()
        .or(row["prompt"].as_str())
        .or_else(|| preset.map(|(_, _, prompt)| *prompt))
        .unwrap_or_default()
        .to_owned();
    let project = RwSignal::new(
        initial["project"]
            .as_str()
            .or(row["project"].as_str())
            .unwrap_or_default()
            .to_owned(),
    );
    let report_to = RwSignal::new(
        initial["reportTo"]
            .as_str()
            .or(row["report_to"].as_str())
            .unwrap_or("direct")
            .to_owned(),
    );
    let next = initial["nextAt"]
        .as_i64()
        .or(row["next_at"].as_i64())
        .unwrap_or_else(|| js_sys::Date::now() as i64 + 3_600_000);
    let interval = initial["intervalSeconds"]
        .as_i64()
        .map(|s| s * 1000)
        .or(row["interval_ms"].as_i64());
    let spec = initial
        .get("schedule")
        .or(row.get("schedule"))
        .filter(|spec| spec.is_object())
        .cloned()
        .unwrap_or_else(|| {
            if !is_edit {
                Value::Null
            } else if let Some(ms) = interval {
                json!({"kind":"legacy_interval","intervalMs":ms,"anchorAt":next})
            } else {
                json!({"kind":"instant","at":next})
            }
        });
    let zone = initial["zone"]
        .as_str()
        .or(row["schedule_zone"].as_str())
        .map(str::to_owned);
    let enabled = RwSignal::new(
        initial["enabled"]
            .as_bool()
            .unwrap_or_else(|| row.is_null() || super::enabled(&row["enabled"])),
    );
    let timing = RwSignal::new(None::<Value>);
    let locked = Signal::derive(move || {
        journal.locked() || journal.error.with(|error| error.contains("changed"))
    });
    let active = Signal::derive(move || trigger.get() == "schedule");
    let source = StoredValue::new(source);
    let submit = move |event| {
        let Some(form) = ui::form(&event) else {
            return;
        };
        if locked.get_untracked() {
            return;
        }
        let Some(timing) = timing.get_untracked() else {
            return;
        };
        let id = routine_id.get_value();
        let trigger = trigger.get_untracked();
        let mut body = json!({"name":ui::field(&form,"name").trim(),"prompt":ui::field(&form,"prompt").trim(),
            "project":project.with_untracked(|project| valid_id(project).then(||json!(project))),"reportTo":report_to.get_untracked(),
            "enabled":enabled.get_untracked(),"trigger":trigger});
        if id.is_some() {
            body["revision"] = json!(revision);
        }
        if id.is_none() && trigger == "webhook" {
            if let Some(source) = source.get_value() {
                body["webhookSource"] = json!(source);
            }
        }
        merge(&mut body, timing);
        let path = id.as_ref().map_or_else(
            || format!("/api/agents/{}/routines", owner.get_untracked()),
            |id| format!("/api/routines/{id}/edit"),
        );
        let req = Request::new(path, body, |body| json!({"routineId":id,"data":body}));
        journal.submit(req, saved);
    };
    let available_projects = Signal::derive(move || {
        let mut list = options(&[("", "No project")]);
        if let Some(value) = projects.data.get() {
            let owner = owner.get();
            list.extend(
                rows(&value["projects"])
                    .iter()
                    .filter(|row| {
                        rows(&row["members"])
                            .iter()
                            .any(|member| member["agent"] == owner)
                    })
                    .map(|row| (text(&row["id"]).to_owned(), text(&row["name"]).to_owned())),
            );
        }
        list
    });
    let report_options = Signal::derive(move || {
        let mut list = options(&[
            ("direct", "Owner agent's direct chat"),
            ("run", "Only in the run's own conversation"),
        ]);
        if let Some(value) = projects.data.get() {
            let id = project.get();
            if let Some(project) = rows(&value["projects"]).iter().find(|row| row["id"] == id) {
                list.extend(
                    rows(&project["sections"])
                        .iter()
                        .filter(|row| !super::enabled(&row["archived"]))
                        .map(|row| {
                            (
                                text(&row["id"]).to_owned(),
                                format!("{} / # {}", text(&project["name"]), text(&row["name"])),
                            )
                        }),
                );
            }
        }
        list
    });
    view! {
        <kit::Dialog id="routine-editor" title=kit::text(if is_edit {"Edit routine"} else {"New routine"}) open=Signal::stored(true)
            size="lg" on_close=move || close.run(()) locked=journal.busy
            description=kit::text(if preset.is_some() { "" } else { "Each run opens a separate conversation and can share its result. Minarbot must be running for schedules." })
            footer=kit::content(move || view! {
                <span class="k-dialog__foot-start"><label class="k-check"><input type="checkbox" prop:checked=enabled disabled=locked
                    on:change=move |event| enabled.set(event_target_checked(&event)) />"Enabled"</label></span>
                <kit::Button label=Signal::derive(move || if journal.error.with(|error|error.contains("changed")) {"Discard changes and reload"} else {"Cancel"})
                    disabled=journal.busy on_click=move || close.run(()) />
                <button type="submit" form="routine-editor-form" class="k-btn k-btn--primary k-btn--md"
                    disabled=move || locked.get() || timing.with(Option::is_none)>"Save routine"</button>
            })>
            <form id="routine-editor-form" class="k-fieldset bs-root" on:submit=submit>
                <Recovery journal done=saved confirm />
                <div class="k-grid-2">
                    <kit::Field id="routine-owner" label="Owner agent"><Select name="routine-owner" value=owner.into() options=Signal::derive(directory)
                        disabled=Signal::derive(move || locked.get() || is_edit) change=Callback::new(move |value| {
                            if storage().ok().and_then(|storage| storage.get_item(&format!("minarbot:routine-command:{value}")).ok().flatten()).is_some() {
                                journal.error.set("This agent has a pending routine request. Resume it first.".into());return;
                            }
                            journal.key.set_value(format!("minarbot:routine-command:{value}"));
                            owner.set(value);
                            if !available_projects.with(|projects| projects.iter().any(|(id,_)| *id==project.get_untracked())) {
                                project.set(String::new());report_to.set("direct".into());
                            }
                        }) /></kit::Field>
                    <kit::Field id="routine-name" label="Name"><input id="routine-name" class="k-input" name="name" maxlength="80" required
                        readonly=locked value=name placeholder="Morning briefing" /></kit::Field>
                </div>
                <kit::Field id="routine-instructions" label="Instructions"><textarea id="routine-instructions" name="prompt" class="k-input k-textarea"
                    maxlength="16000" required readonly=locked rows="3" placeholder="What should this routine do?">{prompt}</textarea></kit::Field>
                <kit::Field id="routine-editor-trigger" label="Trigger"><Select name="routine-editor-trigger" value=trigger.into()
                    options=Signal::stored(options(&[("schedule","Schedule"),("manual","Manual"),("webhook",webhook_label),("script","Local script")]))
                    disabled=locked change=Callback::new(move |value| trigger.set(value)) /></kit::Field>
                <Show when=move || trigger.get()=="webhook" && source.with_value(Option::is_none)>
                    <kit::Alert>"After saving, open Webhook settings to create a URL and secret."</kit::Alert>
                </Show>
                <Show when=move || trigger.get()=="script"><kit::Alert>"After saving, allow Local script access in the routine overview."</kit::Alert></Show>
                <ScheduleFields initial=spec next_at=next interval_ms=interval zone={zone.unwrap_or_default()} active locked output=timing />
                <div class="k-grid-2">
                    <kit::Field id="routine-editor-project" label="Project"><Select name="routine-editor-project" value=project.into()
                        options=available_projects disabled=locked change=Callback::new(move |value: String| {
                            report_to.set(if value.is_empty() {"direct".into()} else {"run".into()});project.set(value);
                        }) /></kit::Field>
                    <kit::Field id="routine-report-to" label="Report to"><Select name="routine-report-to" value=report_to.into()
                        options=report_options disabled=locked change=Callback::new(move |value| report_to.set(value)) /></kit::Field>
                </div>
            </form>
        </kit::Dialog>
    }
}

#[component]
fn RoutineDetail(
    open: RwSignal<Option<Value>>,
    hub: Journal,
    done: Callback<Value>,
    notice: RwSignal<String>,
    back: Callback<()>,
    confirm: kit::Confirm,
    edit: Callback<()>,
    webhook: Callback<()>,
) -> impl IntoView {
    let routine = Signal::derive(move || open.get().unwrap_or_default());
    let id = StoredValue::new(text(&routine.get_untracked()["id"]).to_owned());
    let agent = StoredValue::new(text(&routine.get_untracked()["agent"]).to_owned());
    let runs = ui::query::<Value>(move || id.with_value(|id| format!("/api/routines/{id}/runs")));
    poll(runs.reload, 1200);
    let tab = RwSignal::new(false);
    let locked = Signal::derive(move || hub.locked());
    let loop_done = move || routine.with(|routine| routine["loop"]["reason"].is_string());
    let toggle = move || {
        routine.with_untracked(|routine| {
            hub.submit(
                hub_command(
                    text(&routine["id"]),
                    "edit",
                    edit_payload(routine, !enabled(&routine["enabled"])),
                ),
                done,
            );
        })
    };
    let run = move || {
        routine.with_untracked(|routine| {
            let id = text(&routine["id"]);
            let key = format!("minarbot:routine-test:{id}");
            let request = storage().and_then(|storage| {
                let pending = storage
                    .get_item(&key)
                    .map_err(|_| "Previous run request could not be read.")?;
                if let Some(raw) = pending {
                    let body = serde_json::from_str::<Value>(&raw)
                        .map_err(|_| "Previous run request could not be read.")?;
                    if body["revision"].as_i64().is_none() {
                        return Err("Previous run request could not be read.".into());
                    }
                    Request::restore(
                        format!("/api/routines/{id}/test"),
                        body.clone(),
                        json!({"routine":id,"action":"test","data":body}),
                    )
                } else {
                    Ok(hub_command(
                        id,
                        "test",
                        json!({"revision":routine["revision"]}),
                    ))
                }
            });
            match request {
                Ok(request) => hub.submit(
                    request,
                    Callback::new(move |value| {
                        if let Ok(storage) = storage() {
                            let _ = storage.remove_item(&key);
                        }
                        done.run(value);
                        runs.reload.run(());
                    }),
                ),
                Err(error) => hub.error.set(error),
            }
        })
    };
    let remove = Callback::new(move |_| {
        routine.with_untracked(|routine| {
            let req = hub_command(
                text(&routine["id"]),
                "delete",
                json!({"revision":routine["revision"]}),
            );
            kit::ask(
                Some(confirm),
                format!("Delete {}?", text(&routine["name"])),
                "Future triggers stop. Existing conversations and run records are retained.",
                "Delete routine",
                true,
                move || hub.submit(req.clone(), done),
            );
        })
    });
    let heading = NodeRef::<leptos::html::H1>::new();
    heading.on_load(|heading| {
        let _ = heading.focus();
    });
    let meta = move |key: &'static str, fallback: &'static str| {
        routine.with(|routine| routine[key].as_str().unwrap_or(fallback).to_owned())
    };
    view! {
        <div class="k-page bs-root routine-detail">
            <header class="k-page-head"><div class="k-page-head__text">
                <h1 tabindex="-1" node_ref=heading>{move || meta("name","")}</h1><p>{move || format!("Owned by {}",meta("owner_name","local agent"))}</p>
            </div><div class="k-page-head__actions">
                <kit::Button label=kit::text("All routines") variant="ghost" on_click=move || back.run(()) />
                <kit::Button label=kit::text("Edit routine") disabled=locked on_click=move || edit.run(()) />
                <kit::Button label=Signal::derive(move || if routine.with(|routine| enabled(&routine["enabled"])) {"Pause"} else {"Resume"})
                    disabled=Signal::derive(move || locked.get() || loop_done()) on_click=toggle />
                <kit::Button label=kit::text("Run now") variant="primary" disabled=Signal::derive(move || locked.get() || loop_done()) on_click=run />
                <kit::Menu name="Routine actions".into() disabled=locked
                    items=vec![kit::MenuItem{label:"Delete routine".into(),danger:true,action:remove}]>{kit::icon(kit::icons().more)}</kit::Menu>
            </div></header>
            <p role="status" class="k-field__hint" hidden=move || notice.with(String::is_empty)>{notice}</p>
            <div class="k-tabs">
                <div class="k-tabs__list" role="tablist" aria-label="Routine sections"
                    on:keydown=move |event| crate::settings::move_focus(&event,"[role=tablist]",true)>
                    {[(false,"Overview","routine-overview"),(true,"Flow","routine-flow")].into_iter().map(|(flow,label,id)| view! {
                        <button type="button" role="tab" class="k-tabs__tab" id=format!("{id}-tab") aria-controls=id
                            aria-selected=move || (tab.get()==flow).to_string() tabindex=move || if tab.get()==flow {0}else{-1}
                            data-state=move || if tab.get()==flow {"active"}else{"inactive"} on:click=move |_| tab.set(flow)>{label}</button>
                    }).collect_view()}
                </div>
                <div id="routine-overview" class="k-tabs__panel" role="tabpanel" aria-labelledby="routine-overview-tab" hidden=tab>
                    <div class="k-stack">
                        <kit::Card>
                            <kit::Row title=kit::text("Trigger") meta=kit::content(move || move || routine.with(trigger_label))
                                trailing=kit::content(move || kit::badge("neutral",move || if routine.with(|row| enabled(&row["enabled"])) {"Enabled"}else{"Paused"})) />
                            <kit::Row title=kit::text("Project") meta=kit::content(move || move || meta("project_name","No project")) />
                            <kit::Row title=kit::text("Report to") meta=kit::content(move || move || routine.with(report)) />
                            <kit::Row title=kit::text("Next run") meta=kit::content(move || move || routine.with(|row| {
                                if !enabled(&row["enabled"]) {"Paused".into()} else if row["trigger"]=="schedule" {date_text(number(&row["next_at"]))} else {"On demand".into()}
                            })) />
                            <kit::Row title=kit::text("Last run") meta=kit::content(move || move || routine.with(last_run_link)) />
                            <kit::Row title=kit::text("Flow") meta=kit::content(move || move || routine.with(|row| {
                                let count=number(&row["flow_agents"]);
                                if count>1 {format!("{count} agents")}else{"Single agent".into()}
                            })) />
                        </kit::Card>
                        <Show when=move || routine.with(|row| row["loop"].is_object())>
                            <kit::Section title="Loop"><kit::Card>
                                <kit::Row title=kit::text("Runs") meta=kit::content(move || move || routine.with(|row| format!("{}/{}",row["loop"]["runs"],row["loop"]["max_runs"]))) />
                                <Show when=move || routine.with(|row| row["loop"]["until_condition"].is_string())>
                                    <kit::Row title=kit::text("Until") meta=kit::content(move || move || routine.with(|row| text(&row["loop"]["until_condition"]).to_owned())) />
                                </Show>
                                <Show when=loop_done><kit::Row title=kit::text("Finished")
                                    meta=kit::content(move || move || routine.with(|row| text(&row["loop"]["reason"]).to_owned())) /></Show>
                            </kit::Card></kit::Section>
                        </Show>
                        <kit::Section title="Instructions"><p class="routine-instructions">{move || meta("prompt","")}</p></kit::Section>
                        <div class="k-row-actions">
                            <Show when=move || routine.with(|row| row["trigger"]=="webhook")>
                                <kit::Button label=kit::text("Webhook settings") on_click=move || webhook.run(()) />
                            </Show>
                            <a class="k-link" href=agent.with_value(|agent| format!("/api/agents/{agent}/routines/calendar.ics"))
                                download="minarbot-routines.ics">"Download agent calendar"</a>
                        </div>
                        <kit::Section title="Run history">
                            <ReadError query=runs label="Reload runs" />
                            {move || match runs.data.get() {
                                Some(data) if rows(&data).is_empty()=>kit::empty("No runs yet",String::new()).into_any(),
                                Some(data)=>view! { <kit::Card>{rows(&data).iter().map(|row| run_row(row,agent.get_value())).collect_view()}</kit::Card> }.into_any(),
                                None if !runs.error.with(String::is_empty)=>().into_any(),
                                None=>kit::skeleton(3,"Loading run history...").into_any(),
                            }}
                        </kit::Section>
                        <kit::Disclosure title="Local script access"><ScriptAccess agent confirm /></kit::Disclosure>
                    </div>
                </div>
                <Show when=move || tab.get()>
                    <div id="routine-flow" class="k-tabs__panel" role="tabpanel" aria-labelledby="routine-flow-tab">
                        <FlowEditor id agent confirm />
                    </div>
                </Show>
            </div>
        </div>
    }
}

fn run_row(row: &Value, agent: String) -> impl IntoView {
    let run_state = routine_status(row, false).to_owned();
    let source = match text(&row["source"]) {
        "webhook" => "Webhook",
        "script" => "Local script",
        _ if row["scheduled_at"].is_null() => "Manual",
        _ => "Schedule",
    };
    let at = row["scheduled_at"]
        .as_i64()
        .unwrap_or_else(|| js_sys::Date::parse(text(&row["created"])) as i64);
    let when = date_text(at);
    let relative = relative(js_sys::Date::parse(text(&row["created"])) as i64);
    let thread = row["thread"]
        .as_str()
        .filter(|id| valid_id(id))
        .map(|id| conversation(&agent, id));
    let missed = number(&row["missed"]);
    let error = row["error"].as_str().map(str::to_owned);
    view! {
        <ui::Row title=when meta=kit::content(move || view! { {kit::badge("neutral",run_state)}<span>{format!("{source} · {relative}")}</span> })
            trailing=kit::content(move || thread.map(|href| view! {
                <a class="k-btn k-btn--sm k-btn--secondary" href=href>"Open run"</a>
            }))>
            {(missed>0).then(|| view! { <p class="k-field__hint">{format!("{missed} missed schedules were combined into this run.")}</p> })}
            {error.map(|error| view! { <kit::Alert tone="error">{error}</kit::Alert> })}
        </ui::Row>
    }
}

fn script_request(key: &str, body: Value) -> Result<Request, String> {
    let agent = key
        .strip_prefix("minarbot:script-access:")
        .filter(|id| valid_id(id))
        .ok_or("Saved script permission is invalid.")?;
    if body["revision"].as_i64().is_none() || !body["enabled"].is_boolean() {
        return Err("Saved script permission is invalid.".into());
    }
    Request::restore(format!("/api/agents/{agent}/scripts"), body.clone(), body)
}

#[component]
fn ScriptAccess(agent: StoredValue<String>, confirm: kit::Confirm) -> impl IntoView {
    let path = StoredValue::new(agent.with_value(|agent| format!("/api/agents/{agent}/scripts")));
    let query = ui::query::<Value>(move || path.get_value());
    let journal = Journal::new(
        agent.with_value(|agent| format!("minarbot:script-access:{agent}")),
        script_request,
    );
    let done = Callback::new(move |_| query.reload.run(()));
    let state = move || {
        query
            .data
            .with(|row| row.as_ref().is_some_and(|row| enabled(&row["enabled"])))
    };
    let disabled = Signal::derive(move || {
        journal.locked()
            || query.data.with(|row| {
                row.as_ref()
                    .is_none_or(|row| !enabled(&row["configured"]) && !enabled(&row["enabled"]))
            })
    });
    view! {
        <kit::Section title="Local script">
            <p class="k-field__hint">"Scripts with a server token can run routines and workflows and access this agent's conversations."</p>
            {move || query.data.get().map(|row| view! {
                <kit::Alert>{if enabled(&row["enabled"]) {"Script access is active."}else{"Script access is inactive."}}
                    {(!enabled(&row["configured"])).then_some(" Script token is not configured on the server.")}</kit::Alert>
            })}
            <ReadError query label="Reload permission" />
            <Recovery journal done confirm />
            <div class="k-row-actions">
                <kit::Button label=Signal::derive(move || if state() {"Disable script access"}else{"Allow script access"})
                    disabled on_click=move || if let Some(row)=query.data.get_untracked() {
                        let command=Request::new(path.get_value(),json!({"revision":row["revision"],"enabled":!enabled(&row["enabled"])}),Value::clone);
                        journal.submit(command,done);
                    } />
                <button type="button" class="k-icon-btn" aria-label="Reload permission" on:click=move |_| query.reload.run(())>{kit::icon(kit::icons().refresh)}</button>
            </div>
        </kit::Section>
    }
}

fn flow_request(key: &str, body: Value) -> Result<Request, String> {
    let id = key
        .strip_prefix("minarbot:flow:")
        .filter(|id| valid_id(id))
        .ok_or("Saved flow request is invalid.")?;
    if body["revision"].as_i64().is_none() || !body["links"].is_array() {
        return Err("Saved flow request is invalid.".into());
    }
    Request::restore(format!("/api/routines/{id}/flow"), body.clone(), body)
}

fn reachable(root: &str, links: &[Value]) -> std::collections::BTreeSet<String> {
    let mut reached = std::collections::BTreeSet::from([root.to_owned()]);
    loop {
        let before = reached.len();
        for edge in links {
            if reached.contains(text(&edge["from"])) {
                reached.insert(text(&edge["to"]).to_owned());
            }
        }
        if before == reached.len() {
            return reached;
        }
    }
}

#[component]
fn FlowDiagram(root: StoredValue<String>, links: RwSignal<Vec<Value>>) -> impl IntoView {
    use std::collections::BTreeMap;

    let marker = format!("flow-arrow-{}", ui::uuid());
    view! {
        <div class="flow-canvas" role="region" aria-label="Flow diagram, scroll to view the full graph" tabindex="0">
            {move || {
                let links = links.get();
                let mut depth = BTreeMap::from([(root.get_value(), 0)]);
                for _ in 0..links.len() {
                    let mut changed = false;
                    for link in &links {
                        if let Some(level) = depth.get(text(&link["from"])).copied() {
                            let target = depth.entry(text(&link["to"]).to_owned()).or_insert(0);
                            if *target <= level && level < links.len() {
                                *target = level + 1;
                                changed = true;
                            }
                        }
                    }
                    if !changed { break; }
                }
                let mut counts = BTreeMap::<usize,usize>::new();
                let positions = depth.iter().map(|(id, level)| {
                    let row = counts.entry(*level).or_default();
                    let point = (12 + level * 200, 12 + *row * 84);
                    *row += 1;
                    (id.as_str(), point)
                }).collect::<BTreeMap<_,_>>();
                let names = directory().into_iter().collect::<BTreeMap<_,_>>();
                let width = ((counts.keys().last().copied().unwrap_or(0) + 1) * 200).max(360);
                let height = counts.values().max().copied().unwrap_or(1) * 84;
                view! { <svg width=width height=height role="img" aria-label="Agent relationships in flow">
                    <defs><marker id=marker.clone() viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6" markerHeight="6" orient="auto">
                        <path d="M 0 0 L 10 5 L 0 10 z" fill="var(--color-brand)" />
                    </marker></defs>
                    {links.iter().filter_map(|link| {
                        let (from_x, from_y) = positions.get(text(&link["from"]))?;
                        let (to_x, to_y) = positions.get(text(&link["to"]))?;
                        Some(view! { <line x1=from_x+160 y1=from_y+26 x2=to_x-4 y2=to_y+26 stroke="var(--color-brand)" stroke-width="2" marker-end=format!("url(#{marker})") /> })
                    }).collect_view()}
                    {positions.iter().map(|(id, (x,y))| {
                        let name = names.get(*id).cloned().unwrap_or_else(|| id.to_string());
                        let label = if name.chars().count()>20 { format!("{}…",name.chars().take(18).collect::<String>()) } else {name.clone()};
                        view! { <g transform=format!("translate({x},{y})")><title>{name}</title><rect width="160" height="52" rx="7" fill="var(--color-bg-pane)" stroke="var(--color-border-primary)" /><text x="12" y="31" fill="var(--color-text-primary)">{label}</text></g> }
                    }).collect_view()}
                </svg> }
            }}
        </div>
    }
}

#[component]
fn FlowEditor(
    id: StoredValue<String>,
    agent: StoredValue<String>,
    confirm: kit::Confirm,
) -> impl IntoView {
    let path = StoredValue::new(id.with_value(|id| format!("/api/routines/{id}/flow")));
    let query = ui::query::<Value>(move || path.get_value());
    let runs =
        ui::query::<Value>(move || id.with_value(|id| format!("/api/routines/{id}/flow-runs")));
    poll(runs.reload, 1200);
    let journal = Journal::new(
        id.with_value(|id| format!("minarbot:flow:{id}")),
        flow_request,
    );
    let links = RwSignal::new(Vec::<Value>::new());
    let from = RwSignal::new(agent.get_value());
    let to = RwSignal::new(String::new());
    let loaded = Signal::derive(move || query.data.with(Option::is_some));
    let error = RwSignal::new(String::new());
    let locked = Signal::derive(move || {
        !loaded.get() || journal.locked() || journal.error.with(|error| error.contains("changed"))
    });
    Effect::new(move |_| {
        if let Some(value) = query.data.get() {
            let pending = journal.pending.with_untracked(|value| {
                value
                    .as_ref()
                    .ok()
                    .and_then(Option::as_ref)
                    .map(|request| rows(&request.body["links"]).to_vec())
            });
            links.set(pending.unwrap_or_else(|| rows(&value["graph"]["links"]).to_vec()));
        }
    });
    let done = Callback::new(move |_| {
        query.reload.run(());
        runs.reload.run(());
    });
    let from_options = Signal::derive(move || {
        let graph = links.with(|links| agent.with_value(|agent| reachable(agent, links)));
        directory()
            .into_iter()
            .filter(|(id, _)| graph.contains(id))
            .collect()
    });
    let to_options = Signal::derive(move || {
        let mut options = options(&[("", "Select agent")]);
        let from = from.get();
        options.extend(
            directory()
                .into_iter()
                .filter(|(id, _)| *id != from && *id != agent.get_value()),
        );
        options
    });
    let connect = move || {
        let source = from.get_untracked();
        let target = to.get_untracked();
        let edge = json!({"from":source,"to":target});
        if links.with_untracked(|links| {
            links.contains(&edge)
                || links.len() >= 64
                || reachable(&target, links).contains(&source)
        }) {
            error.set("Flow links must be unique and cannot form a cycle.".into());
            return;
        }
        error.set(String::new());
        links.update(|links| links.push(edge));
    };
    let name = move |id: &str| {
        directory()
            .into_iter()
            .find(|(agent, _)| agent == id)
            .map_or_else(|| id.to_owned(), |(_, name)| name)
    };
    view! {
        <div class="k-stack bs-root routine-flow">
            <kit::Section title="Agent flow" actions=kit::content(move || view! {
                <button type="button" class="k-icon-btn" aria-label="Reload flow" disabled=journal.busy on:click=move |_| {
                    journal.error.set(String::new());error.set(String::new());query.data.set(None);query.reload.run(());
                }>{kit::icon(kit::icons().refresh)}</button>
                <kit::Button label=kit::text("Save flow") variant="primary" disabled=locked on_click=move || {
                    let revision=query.data.with_untracked(|value|value.as_ref().map_or(0,|value|number(&value["revision"])));
                    journal.submit(Request::new(path.get_value(),json!({"revision":revision,"links":links.get_untracked()}),Value::clone),done);
                } />
            })><p class="k-field__hint">"Connect routine results to the next agent. Changes apply to new runs."</p></kit::Section>
            <Recovery journal done confirm />
            <ReadError query label="Reload flow" />
            {kit::error(error)}
            <Show when=move || !loaded.get() && query.error.with(String::is_empty)><p role="status">"Loading flow..."</p></Show>
            <Show when=move || loaded.get()>
                <FlowDiagram root=agent links=links />
                <div class="k-grid-2">
                    <kit::Field id="flow-from" label="From agent"><Select name="flow-from" value=from.into() options=from_options
                        disabled=locked change=Callback::new(move |next: String| {if to.get_untracked()==next{to.set(String::new());}from.set(next);}) /></kit::Field>
                    <kit::Field id="flow-to" label="To agent"><Select name="flow-to" value=to.into() options=to_options
                        disabled=locked change=Callback::new(move |next|to.set(next)) /></kit::Field>
                </div>
                <div><kit::Button label=kit::text("Connect") disabled=Signal::derive(move || locked.get() || to.with(String::is_empty)) on_click=connect /></div>
                <kit::Card>{move || links.get().into_iter().map(|link| {
                    let label=format!("{} to {}",name(text(&link["from"])),name(text(&link["to"])));
                    let remove_name=format!("Remove relationship {label}");
                    view! { <ui::Row title=label trailing=kit::content(move || view! {
                        <button type="button" class="k-icon-btn" aria-label=remove_name disabled=locked on:click=move |_| {
                            links.update(|links| {
                                links.retain(|item| item!=&link);
                                let graph=agent.with_value(|id|reachable(id,links));
                                links.retain(|edge|graph.contains(text(&edge["from"])));
                            });
                            from.set(agent.get_value());
                        }>{kit::icon(kit::icons().close)}</button>
                    }) /> }
                }).collect_view()}</kit::Card>
            </Show>
            <kit::Section title="Execution steps">
                <ReadError query=runs />
                {move || runs.data.get().map(|data| {
                    if rows(&data).is_empty(){return kit::empty("No runs with saved flows",String::new()).into_any();}
                    view! { <div class="k-stack">{rows(&data).to_vec().into_iter().map(|run| {
                        let created=date_text(js_sys::Date::parse(text(&run["created"])) as i64);
                        view! { <section class="k-section"><h3 class="k-section__title">{created}</h3>
                            <kit::Card>{rows(&run["steps"]).iter().map(|step| {
                                let name=text(&step["name"]).to_owned();
                                let link=step["thread"].as_str().filter(|id|valid_id(id)).map(|id|conversation(text(&step["agent"]),id));
                                let button=format!("Open step {name}");
                                let state=if enabled(&step["paused"]) {"Paused"}else{step["task_status"].as_str().or(step["status"].as_str()).map(status).unwrap_or("Waiting")}.to_owned();
                                let result=text(&step["result"]).to_owned();
                                view! { <ui::Row title=name meta=kit::content(move || kit::badge("neutral",state))
                                    trailing=kit::content(move || link.map(|href| view! { <a class="k-btn k-btn--sm k-btn--secondary" href=href>{button}</a> }))>
                                    <span class="automation-result k-field__hint">{result}</span>
                                </ui::Row> }
                            }).collect_view()}</kit::Card>
                        </section> }
                    }).collect_view()}</div> }.into_any()
                })}
            </kit::Section>
        </div>
    }
}

fn webhook_request(key: &str, body: Value) -> Result<Request, String> {
    let id = key
        .strip_prefix("minarbot:webhook:")
        .filter(|id| valid_id(id))
        .ok_or("Saved webhook request is invalid.")?;
    if body["revision"].as_i64().is_none()
        || !body["rotate"].is_boolean()
        || !body["eventName"].is_string()
        || !body["filters"].is_array()
        || body.get("signingSecret").is_some()
    {
        return Err("Saved webhook request is invalid.".into());
    }
    Request::restore(format!("/api/routines/{id}/webhook"), body.clone(), body)
}

#[component]
fn Webhook(row: Value, back: Callback<()>, confirm: kit::Confirm) -> impl IntoView {
    let id = StoredValue::new(text(&row["id"]).to_owned());
    let path = StoredValue::new(id.with_value(|id| format!("/api/routines/{id}/webhook")));
    let hook = ui::query::<Value>(move || path.get_value());
    let events = ui::query::<Value>(move || {
        id.with_value(|id| format!("/api/routines/{id}/webhook-events"))
    });
    let source_name = TEMPLATES
        .iter()
        .find(|(key, _, _)| *key == text(&row["webhook_source"]))
        .map(|(_, name, _)| *name);
    let native = source_name.is_some();
    let title = source_name.map_or_else(
        || format!("Webhook {}", text(&row["name"])),
        |name| format!("{name} webhook"),
    );
    let secret = RwSignal::new(String::new());
    let signing_secret = NodeRef::<leptos::html::Input>::new();
    let native_busy = RwSignal::new(false);
    let journal = Journal::new(
        id.with_value(|id| format!("minarbot:webhook:{id}")),
        webhook_request,
    );
    let error = RwSignal::new(String::new());
    let locked = Signal::derive(move || {
        native_busy.get()
            || journal.locked()
            || journal.error.with(|error| error.contains("changed"))
    });
    let reload = move || {
        secret.set(String::new());
        journal.error.set(String::new());
        error.set(String::new());
        hook.data.set(None);
        hook.reload.run(());
        events.reload.run(());
    };
    let done = Callback::new(move |result: Value| {
        spawn_local(async move {
            let latest = api::read::<Value>(&path.get_value()).await;
            if secret.try_get_untracked().is_none() {
                return;
            }
            match latest {
                Ok(latest)
                    if result["revision"] == latest["revision"]
                        && result["route"] == latest["route"] =>
                {
                    secret.set(if native {
                        String::new()
                    } else {
                        text(&result["secret"]).to_owned()
                    });
                    if native {
                        if let Some(input) = signing_secret.get_untracked() {
                            input.set_value("");
                        }
                    }
                    hook.data.set(Some(latest));
                    events.reload.run(());
                }
                Ok(_) => error.set("Webhook changed. Reload before using the secret.".into()),
                Err(message) => error.set(message),
            }
        });
    });
    view! {
        <div class="k-page bs-root">
            <PageHead title actions=kit::content(move || view! {
                <kit::Button label=kit::text("Back") variant="ghost" on_click=move || back.run(()) />
                <button type="button" class="k-icon-btn" aria-label="Reload webhook" disabled=journal.busy
                    on:click=move |_| reload()>{kit::icon(kit::icons().refresh)}</button>
            }) />
            <ReadError query=hook label="Reload webhook" /><ReadError query=events label="Reload events" />
            <Recovery journal done confirm />{kit::error(error)}
            {move || match hook.data.get() {
                Some(config)=>{
                    let revision=number(&config["revision"]);
                    let configured=revision>0;
                    let hook_url=text(&config["url"]).to_owned();
                    let has_hook=!config.is_null();
                    let event_name=text(&config["event_name"]).to_owned();
                    let filters=serde_json::to_string_pretty(rows(&config["filters"])).unwrap_or_else(|_| "[]".into());
                    let save=move |event| {
                        let Some(form)=ui::form(&event) else {return;};
                        if locked.get_untracked(){return;}
                        let filters=serde_json::from_str::<Value>(&ui::field(&form,"filters")).ok().filter(Value::is_array);
                        let Some(filters)=filters else {error.set("Filter must be a valid JSON list.".into());return;};
                        secret.set(String::new());error.set(String::new());
                        if native {
                            // Signing secrets stay in this request and never enter the recovery journal.
                            let signing_secret=ui::field(&form,"signingSecret");
                            let mut body=json!({"revision":revision,"filters":filters});
                            if !signing_secret.is_empty(){body["signingSecret"]=json!(signing_secret);}
                            native_busy.set(true);
                            spawn_local(async move {
                                let result=api::command::<Value>(&path.get_value(),body).await;
                                if native_busy.try_get_untracked().is_none(){return;}
                                native_busy.set(false);
                                match result {Ok(value)=>done.run(value),Err(message)=>error.set(message)}
                            });
                        } else {
                            let body=json!({"revision":revision,"filters":filters,"eventName":ui::field(&form,"eventName"),
                                "rotate":!has_hook || form.has("rotate")});
                            journal.submit(Request::new(path.get_value(),body,Value::clone),done);
                        }
                    };
                    view! {
                        {has_hook.then(|| view! { <kit::Field id="routine-hook-url" label="Webhook URL">
                            <input id="routine-hook-url" class="k-input" readonly value=hook_url />
                        </kit::Field> })}
                        <Show when=move || !secret.with(String::is_empty)>
                            <kit::Field id="routine-hook-secret" label="New secret"><input id="routine-hook-secret" class="k-input" type="password"
                                readonly autocomplete="off" prop:value=secret /></kit::Field>
                        </Show>
                        <form class="k-fieldset" on:submit=save>
                            {if native {
                                view! { <kit::Field id="routine-signed-secret" label="Signing secret"><input id="routine-signed-secret" node_ref=signing_secret name="signingSecret"
                                    class="k-input" type="password" autocomplete="new-password" minlength="16" maxlength="512" required=!configured disabled=locked /></kit::Field> }.into_any()
                            } else {
                                view! { <kit::Field id="routine-event-name" label="Event name"><input id="routine-event-name" name="eventName"
                                    class="k-input" maxlength="256" value=event_name readonly=locked /></kit::Field> }.into_any()
                            }}
                            <kit::Field id="routine-hook-filters" label="Payload filters"><textarea id="routine-hook-filters" name="filters"
                                class="k-input k-textarea" readonly=locked rows="4">{filters}</textarea></kit::Field>
                            {(has_hook&&!native).then(|| view! { <label class="k-check"><input type="checkbox" name="rotate" disabled=locked />
                                "Rotate secret. The old secret becomes invalid immediately."</label> })}
                            <div><button type="submit" class="k-btn k-btn--md k-btn--primary" disabled=locked>
                                {if configured {"Save webhook"}else if native {"Enable webhook"}else{"Create URL and secret"}}
                            </button></div>
                        </form>
                    }.into_any()
                },
                None if !hook.error.with(String::is_empty)=>().into_any(),
                None=>kit::skeleton(3,"Loading webhook...").into_any(),
            }}
            <kit::Section title="Received events">
                {move || events.data.get().map(|events| {
                    if rows(&events).is_empty(){return kit::empty("No saved events",String::new()).into_any();}
                    view! { <kit::Card>{rows(&events).iter().map(|event| {
                        let received=date_text(number(&event["received"]));
                        let state=if event["result"]["outcome"]=="queued" {"Accepted for execution"}else{"Ignored"};
                        let reason=text(&event["result"]["reason"]).to_owned();
                        view! { <ui::Row title=text(&event["delivery"]).to_owned()
                            meta=kit::content(move || view! { <span>{received}</span>{kit::badge("neutral",state)} })>
                            <span class="k-field__hint">{reason}</span>
                        </ui::Row> }
                    }).collect_view()}</kit::Card> }.into_any()
                })}
            </kit::Section>
        </div>
    }
}
