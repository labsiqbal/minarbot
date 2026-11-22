use super::*;
use js_sys::{Array, Intl, Object, Reflect};
use wasm_bindgen::JsValue;

pub(super) fn format(at: i64, locale: &str, fields: &[(&str, &str)]) -> String {
    date_at(at)
        .to_locale_string(locale, format_options(fields).as_ref())
        .as_string()
        .unwrap_or_default()
}

fn format_options(fields: &[(&str, &str)]) -> Object {
    let options = Object::new();
    for (key, value) in fields {
        let _ = Reflect::set(&options, &JsValue::from_str(key), &JsValue::from_str(value));
    }
    options
}

pub(super) fn formatter(locale: &str, fields: &[(&str, &str)]) -> impl Fn(i64) -> String {
    let format =
        Intl::DateTimeFormat::new(&Array::of1(&locale.into()), &format_options(fields)).format();
    move |at| {
        format
            .call1(&JsValue::UNDEFINED, &JsValue::from_f64(at as f64))
            .ok()
            .and_then(|value| value.as_string())
            .unwrap_or_default()
    }
}

pub(super) fn device_zone() -> String {
    let resolved = Intl::DateTimeFormat::new(&Array::new(), &Object::new()).resolved_options();
    Reflect::get(resolved.as_ref(), &"timeZone".into())
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| "UTC".into())
}

pub(super) fn wall_time(at: i64, zone: &str) -> (String, String) {
    (
        format(
            at,
            "sv-SE",
            &[
                ("timeZone", zone),
                ("year", "numeric"),
                ("month", "2-digit"),
                ("day", "2-digit"),
            ],
        ),
        format(
            at,
            "en-GB",
            &[
                ("timeZone", zone),
                ("hour", "2-digit"),
                ("minute", "2-digit"),
                ("hourCycle", "h23"),
            ],
        ),
    )
}

pub(super) fn date_label(day: &str, fields: &[(&str, &str)]) -> String {
    let mut utc = fields.to_vec();
    utc.push(("timeZone", "UTC"));
    format(js_sys::Date::parse(day) as i64, "en-US", &utc)
}

pub(super) async fn resolve(date: &str, time: &str, zone: &str) -> Result<i64, String> {
    let schedule = json!({"kind":"once","date":date,"time":time}).to_string();
    let path = url(
        "/api/routine-schedule",
        &[("zone", zone), ("schedule", &schedule)],
    );
    let value: Value = api::read(&path).await?;
    value["at"]
        .as_i64()
        .ok_or_else(|| "Schedule time could not be resolved.".into())
}

pub(super) fn routine_timing(routine: &Value, fallback: &str) -> Value {
    let zone = routine["schedule_zone"].as_str().unwrap_or(fallback);
    let schedule = &routine["schedule"];
    if schedule.is_null() || schedule["kind"] == "legacy_interval" {
        json!({"nextAt":routine["next_at"],"intervalSeconds":routine["interval_ms"].as_i64().map(|value| value / 1000),"zone":zone})
    } else {
        json!({"schedule":schedule,"zone":zone})
    }
}

pub(super) fn merge(target: &mut Value, fields: Value) {
    if let (Some(target), Value::Object(fields)) = (target.as_object_mut(), fields) {
        target.extend(fields);
    }
}

#[component]
pub(super) fn ScheduleFields(
    initial: Value,
    next_at: i64,
    interval_ms: Option<i64>,
    #[prop(optional)] zone: Option<String>,
    #[prop(optional)] interval_anchor: Signal<Option<i64>>,
    active: Signal<bool>,
    locked: Signal<bool>,
    output: RwSignal<Option<Value>>,
) -> impl IntoView {
    let now = js_sys::Date::now() as i64;
    let existing_zone = zone
        .as_deref()
        .filter(|zone| !zone.is_empty())
        .unwrap_or("UTC");
    let (fallback_day, fallback_time) = wall_time(next_at, existing_zone);
    let date = RwSignal::new(initial["date"].as_str().unwrap_or(&fallback_day).to_owned());
    let parts = text(&initial["expression"])
        .split_whitespace()
        .collect::<Vec<_>>();
    let simple = parts.len() == 5
        && parts[2] == "*"
        && parts[3] == "*"
        && parts[0].parse::<u8>().is_ok()
        && parts[1].parse::<u8>().is_ok();
    let days = parts.get(4).copied().unwrap_or("1");
    let weekly = days
        .split(',')
        .all(|day| matches!(day, "0" | "1" | "2" | "3" | "4" | "5" | "6"));
    let mode = RwSignal::new(
        match text(&initial["kind"]) {
            "cron" if simple && days == "*" => "daily",
            "cron" if simple && days == "1-5" => "weekdays",
            "cron" if simple && weekly => "weekly",
            "cron" => "cron",
            "interval" => "interval",
            "legacy_interval" => "legacy",
            "once" | "instant" => "once",
            _ => "daily",
        }
        .to_owned(),
    );
    let time = RwSignal::new(if simple {
        format!("{:0>2}:{:0>2}", parts[1], parts[0])
    } else {
        initial["time"]
            .as_str()
            .unwrap_or(if initial["kind"] == "instant" {
                &fallback_time
            } else {
                "09:00"
            })
            .to_owned()
    });
    let week = RwSignal::new(if weekly {
        days.split(',').map(str::to_owned).collect::<Vec<_>>()
    } else {
        vec!["1".into()]
    });
    let expression = RwSignal::new(
        initial["expression"]
            .as_str()
            .unwrap_or("0 9 * * 1-5")
            .to_owned(),
    );
    let amount = RwSignal::new(initial["amount"].as_i64().map_or_else(
        || {
            interval_ms.map_or_else(
                || "1".to_owned(),
                |value| (value as f64 / 60_000.0).to_string(),
            )
        },
        |amount| amount.to_string(),
    ));
    let unit = RwSignal::new(initial["unit"].as_str().unwrap_or("hour").to_owned());
    let old = StoredValue::new(initial);
    let has_zone = zone.as_ref().is_some_and(|zone| !zone.is_empty());
    let zone = RwSignal::new(zone.unwrap_or_default());
    let zone_error = RwSignal::new(String::new());
    let preview = RwSignal::new(None::<Result<Value, String>>);
    let version = RwSignal::new(0_u64);
    if !has_zone {
        spawn_local(async move {
            match api::read::<Value>("/api/routine-schedule").await {
                Ok(value) => {
                    zone.try_set(text(&value["zone"]).into());
                }
                Err(error) => {
                    zone_error.try_set(error);
                }
            }
        });
    }
    Effect::new(move |previous: Option<Option<TimeoutHandle>>| {
        if let Some(Some(previous)) = previous {
            previous.clear();
        }
        version.update(|value| *value += 1);
        let current = version.get_untracked();
        let zone = zone.get();
        if !active.get() {
            output.set(Some(json!({"nextAt":next_at,"intervalSeconds":interval_ms.map(|ms| ms/1000),"zone":zone})));
            return None;
        }
        let mode = mode.get();
        let time = time.get();
        let raw_amount = amount.get();
        let amount = raw_amount.parse::<i64>().unwrap_or(0);
        let legacy_ms = (raw_amount.parse::<f64>().unwrap_or(0.0) * 60_000.0).round() as i64;
        let spec = match mode.as_str() {
            "once" => json!({"kind":"once","date":date.get(),"time":time}),
            "legacy" => {
                json!({"kind":"legacy_interval","intervalMs":legacy_ms,"anchorAt":next_at})
            }
            "interval" => {
                let unit = unit.get();
                let elapsed = match unit.as_str() {
                    "minute" => 60_000,
                    "day" => 86_400_000,
                    _ => 3_600_000,
                };
                let anchor = old
                    .with_value(|old| {
                        if old["unit"] == unit && old["amount"] == amount {
                            old["anchorAt"].as_i64()
                        } else {
                            None
                        }
                    })
                    .or_else(|| interval_anchor.get())
                    .unwrap_or_else(|| now.saturating_add(amount.saturating_mul(elapsed)));
                json!({"kind":"interval","amount":amount,"unit":unit,"anchorAt":anchor})
            }
            "cron" => json!({"kind":"cron","expression":expression.get()}),
            _ => {
                let (hour, minute) = time.split_once(':').unwrap_or(("00", "00"));
                let hour = hour.parse::<u8>().unwrap_or(0);
                let minute = minute.parse::<u8>().unwrap_or(0);
                let days = match mode.as_str() {
                    "daily" => "*".into(),
                    "weekdays" => "1-5".into(),
                    _ => week.with(|days| days.join(",")),
                };
                json!({"kind":"cron","expression":format!("{minute} {hour} * * {days}")})
            }
        };
        output.set(None);
        preview.set(None);
        if zone.is_empty() {
            return None;
        }
        let path = url(
            "/api/routine-schedule",
            &[("zone", &zone), ("schedule", &spec.to_string())],
        );
        set_timeout_with_handle(move || spawn_local(async move {
            let result = api::read::<Value>(&path).await;
            if version.try_get_untracked() != Some(current) { return; }
            if let Ok(value) = &result {
                output.set(Some(if mode == "legacy" {
                    json!({"nextAt":next_at,"intervalSeconds":legacy_ms / 1000,"zone":value["zone"]})
                } else { json!({"schedule":spec,"zone":value["zone"]}) }));
            }
            preview.set(Some(result));
        }), Duration::from_millis(300)).ok()
    });
    let mut modes = options(&[
        ("once", "Once"),
        ("daily", "Every day"),
        ("weekdays", "Weekdays"),
        ("weekly", "Weekly"),
        ("interval", "Every N hours / minutes (advanced)"),
        ("cron", "Custom cron (advanced)"),
    ]);
    if old.with_value(|old| old["kind"] == "legacy_interval") {
        modes.push(("legacy".into(), "Existing interval (minutes)".into()));
    }
    view! {
        <div class="routine-schedule k-stack" hidden=move || !active.get()>
            <kit::Field id="routine-mode" label="Schedule"><Select name="routine-mode" value=mode.into()
                options=Signal::stored(modes) disabled=locked change=Callback::new(move |value| mode.set(value)) /></kit::Field>
            <div class="k-grid-2">
                <Show when=move || mode.get() == "once">
                    <kit::Field id="routine-date" label="Date"><input id="routine-date" class="k-input" type="date" required=active disabled=locked
                        prop:value=date on:input=move |event| date.set(event_target_value(&event)) /></kit::Field>
                </Show>
                <Show when=move || mode.with(|mode| matches!(mode.as_str(),"once"|"daily"|"weekdays"|"weekly"))>
                    <kit::Field id="routine-time" label="Time"><input id="routine-time" class="k-input" type="time" required=active disabled=locked
                        prop:value=time on:input=move |event| time.set(event_target_value(&event)) /></kit::Field>
                </Show>
                <kit::Field id="routine-zone" label="Time zone"><input id="routine-zone" class="k-input" required=active disabled=locked maxlength="100"
                    aria-describedby="routine-zone-hint" placeholder="Asia/Jakarta" prop:value=zone on:input=move |event| zone.set(event_target_value(&event)) />
                    <span class="k-field__hint" id="routine-zone-hint">"IANA zone. Daily and weekly times follow its local clock."</span>
                </kit::Field>
            </div>
            <Show when=move || mode.get() == "weekly">
                <fieldset class="routine-days"><legend>"Days"</legend>
                  {["Sun","Mon","Tue","Wed","Thu","Fri","Sat"].into_iter().enumerate().map(|(index, day)| {
                    let key = index.to_string();
                    let checked = key.clone();
                    view! { <label><input type="checkbox" disabled=locked prop:checked=move || week.with(|days| days.contains(&checked))
                        on:change=move |event| week.update(|days| {
                            if event_target_checked(&event) { days.push(key.clone()); days.sort(); days.dedup(); }
                            else { days.retain(|day| day != &key); }
                        }) />{day}</label> }
                  }).collect_view()}
                </fieldset>
            </Show>
            <Show when=move || mode.with(|mode| matches!(mode.as_str(),"legacy"|"interval"))>
                <div class="k-grid-2">
                    <kit::Field id="routine-amount" label="Every"><input id="routine-amount" class="k-input" type="number" min="1" max="525600"
                        step=move || if mode.get()=="legacy" {"any"} else {"1"}
                        required=active disabled=locked prop:value=amount on:input=move |event| amount.set(event_target_value(&event)) /></kit::Field>
                    <Show when=move || mode.get() == "interval">
                        <kit::Field id="routine-unit" label="Unit"><Select name="routine-unit" value=unit.into()
                            options=Signal::stored(options(&[("hour","Hours"),("minute","Minutes"),("day","Days")]))
                            change=Callback::new(move |value| unit.set(value)) disabled=locked /></kit::Field>
                    </Show>
                </div>
            </Show>
            <Show when=move || mode.get() == "cron">
                <kit::Field id="routine-cron" label="Cron expression"><input id="routine-cron" class="k-input" required=active maxlength="256" disabled=locked
                    prop:value=expression on:input=move |event| expression.set(event_target_value(&event)) /></kit::Field>
            </Show>
            <section class="routine-preview" aria-label="Next runs" aria-live="polite"><strong>"Next runs"</strong>
                {move || match preview.get() {
                    Some(Ok(value)) if rows(&value["times"]).is_empty() => view! { <p>"No future runs. A past one-time schedule runs once when enabled."</p> }.into_any(),
                    Some(Ok(value)) => view! { <ol>{rows(&value["times"]).iter().map(|at| {
                        let at = number(at);
                        let zone = text(&value["zone"]);
                        let label = format(at, "en-GB", &[("timeZone",zone),("weekday","short"),("day","numeric"),("month","short"),("year","numeric"),("hour","2-digit"),("minute","2-digit")]);
                        view! { <li><time datetime=iso(at)>{label}</time></li> }
                    }).collect_view()}</ol> }.into_any(),
                    Some(Err(error)) => view! { <kit::Alert tone="error">{error}</kit::Alert> }.into_any(),
                    None if !zone_error.with(String::is_empty) => view! { <kit::Alert tone="error">{zone_error}</kit::Alert> }.into_any(),
                    None => view! { <p role="status">"Calculating schedule..."</p> }.into_any(),
                }}
            </section>
        </div>
    }
}
