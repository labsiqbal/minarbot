use js_sys::{Array, Date, Intl, Object, Reflect};
use serde_json::Value;

pub fn trigger(row: &Value) -> String {
    match row["trigger"].as_str() {
        Some("manual") => "Manual".into(),
        Some("script") => "Local script".into(),
        Some("webhook") => match row["webhook_source"].as_str() {
            Some("slack") => "Slack message",
            Some("linear") => "Linear issue",
            Some("sentry") => "Sentry alert",
            Some("github") => "GitHub issue",
            _ => "Webhook",
        }
        .into(),
        _ => schedule(row),
    }
}

fn schedule(row: &Value) -> String {
    let spec = &row["schedule"];
    let zone = row["schedule_zone"]
        .as_str()
        .filter(|zone| !zone.is_empty())
        .map_or_else(
            || {
                let options =
                    Intl::DateTimeFormat::new(&Array::new(), &Object::new()).resolved_options();
                Reflect::get(options.as_ref(), &"timeZone".into())
                    .ok()
                    .and_then(|zone| zone.as_string())
                    .unwrap_or_else(|| "UTC".into())
            },
            str::to_owned,
        );
    let text = |key| spec[key].as_str().unwrap_or_default();
    let once = |at: i64| {
        let options = Object::new();
        let _ = Reflect::set(&options, &"timeZone".into(), &zone.as_str().into());
        let date = Date::new(&(at as f64).into())
            .to_locale_string("en-GB", &options)
            .as_string()
            .unwrap_or_default();
        format!("Once on {date} ({zone})")
    };
    let minutes = |ms: i64| format!("Every {} minutes", ms as f64 / 60_000.0);
    match spec["kind"].as_str() {
        Some("once") => format!("Once on {} at {} ({zone})", text("date"), text("time")),
        Some("cron") => {
            let expression = text("expression");
            let fields = expression.split(' ').collect::<Vec<_>>();
            if let [minute, hour, "*", "*", days] = fields.as_slice() {
                let numeric = |field: &str| {
                    !field.is_empty() && field.bytes().all(|byte| byte.is_ascii_digit())
                };
                let label = match *days {
                    "*" => Some("Every day".into()),
                    "1-5" => Some("Weekdays".into()),
                    _ => days
                        .split(',')
                        .map(|day| {
                            let index = day.as_bytes().first()?.checked_sub(b'0')?;
                            (day.len() == 1).then_some(())?;
                            ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
                                .get(usize::from(index))
                                .copied()
                        })
                        .collect::<Option<Vec<_>>>()
                        .map(|days| format!("Weekly on {}", days.join(", "))),
                };
                if numeric(minute) && numeric(hour) {
                    if let Some(label) = label {
                        return format!("{label} at {hour:0>2}:{minute:0>2} ({zone})");
                    }
                }
            }
            format!("Cron: {expression} ({zone})")
        }
        Some("interval") => {
            let amount = spec["amount"].as_i64().unwrap_or_default();
            format!(
                "Every {amount} {}{}",
                text("unit"),
                if amount == 1 { "" } else { "s" }
            )
        }
        Some("legacy_interval") => minutes(spec["intervalMs"].as_i64().unwrap_or_default()),
        Some(_) => once(spec["at"].as_i64().unwrap_or_default()),
        None => row["interval_ms"]
            .as_i64()
            .filter(|ms| *ms != 0)
            .map_or_else(
                || once(row["next_at"].as_i64().unwrap_or_default()),
                minutes,
            ),
    }
}
