use super::*;
use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveDateTime, Offset, TimeZone, Utc};
use chrono_tz::Tz;

pub const MAX_TIME: i64 = 253370764799999;
pub const CRON_HORIZON: i64 = 1825 * 86400000;
fn invalid() -> Failure {
    error(400, "Invalid schedule.")
}
/// The time zone of the machine Minarbot runs on.
pub fn machine_zone() -> String {
    std::env::var("TZ")
        .ok()
        .filter(|zone| zone.parse::<Tz>().is_ok())
        .or_else(|| {
            std::fs::read_link("/etc/localtime").ok().and_then(|p| {
                p.to_str()?
                    .split_once("zoneinfo/")
                    .map(|(_, z)| z.to_owned())
            })
        })
        .unwrap_or_else(|| "UTC".to_owned())
}
pub fn zone(value: Option<&str>) -> Result<String> {
    let value = value.map_or_else(machine_zone, str::to_owned);
    if value.len() > 100 || value.parse::<Tz>().is_err() {
        return Err(error(400, "Enter an IANA time zone, such as Asia/Jakarta."));
    }
    Ok(value)
}
/// The zone chosen in Settings > General, or the machine's zone.
pub fn home(db: &Connection) -> Result<String> {
    Ok(chosen(db)?.unwrap_or_else(machine_zone))
}
fn chosen(db: &Connection) -> Result<Option<String>> {
    Ok(
        one(db, "SELECT zone FROM time_zone_settings WHERE id=1", [])?
            .and_then(|row| row["zone"].as_str().map(str::to_owned)),
    )
}
/// Settings > General time zone. Routines that follow it move with it.
pub fn settings(db: &Connection, v: &Value, get: bool, now: i64) -> Result<Value> {
    if !get {
        let chosen = match &v["zone"] {
            Value::Null => None,
            Value::String(value) => Some(zone(Some(value))?),
            _ => return Err(error(400, "Invalid time zone settings.")),
        };
        if !data::closed(v, &["id", "zone"]) {
            return Err(error(400, "Invalid time zone settings."));
        }
        let before = home(db)?;
        db.execute(
            "INSERT INTO time_zone_settings(id,zone) VALUES(1,?) ON CONFLICT(id) DO UPDATE SET zone=excluded.zone",
            [&chosen],
        )?;
        let after = home(db)?;
        if after != before {
            follow(db, &after, now)?;
        }
    }
    let zones = chrono_tz::TZ_VARIANTS
        .iter()
        .map(|zone| zone.name())
        .filter(|name| {
            *name == "UTC"
                || name.split_once('/').is_some_and(|(region, _)| {
                    matches!(
                        region,
                        "Africa"
                            | "America"
                            | "Antarctica"
                            | "Asia"
                            | "Atlantic"
                            | "Australia"
                            | "Europe"
                            | "Indian"
                            | "Pacific"
                    )
                })
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"zone": chosen(db)?, "machine": machine_zone(), "effective": home(db)?, "zones": zones}),
    )
}
/// Moves the routines that follow the General zone to `zone`, with their
/// next one-time or calendar run on its clock.
fn follow(db: &Connection, zone: &str, now: i64) -> Result<()> {
    for row in all(
        db,
        "SELECT id,schedule_json FROM routines WHERE zone_follows=1",
        [],
    )? {
        let spec = row["schedule_json"]
            .as_str()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
        let next = spec.and_then(|spec| match string(&spec, "kind") {
            "once" => once(&spec, zone).ok(),
            "cron" => next_cron(string(&spec, "expression"), zone, now, true).ok(),
            _ => None,
        });
        db.execute(
            "UPDATE routines SET schedule_zone=?,next_at=coalesce(?,next_at),revision=revision+1 WHERE id=?",
            params![zone, next, string(&row, "id")],
        )?;
    }
    Ok(())
}
fn timezone(zone: &str) -> Result<Tz> {
    zone.parse()
        .map_err(|_| error(422, "Invalid home device time zone."))
}
fn date(at: i64) -> Result<DateTime<Utc>> {
    DateTime::from_timestamp_millis(at).ok_or_else(invalid)
}
fn wall(value: &Value) -> Result<NaiveDateTime> {
    let day = string(value, "date");
    let time = string(value, "time");
    if day.len() != 10 || time.len() != 5 || day < "1970-01-01" {
        return Err(invalid());
    }
    let wall = NaiveDateTime::parse_from_str(&format!("{day}T{time}:00"), "%Y-%m-%dT%H:%M:%S")
        .map_err(|_| invalid())?;
    if wall.format("%Y-%m-%dT%H:%M").to_string() != format!("{day}T{time}") {
        return Err(invalid());
    }
    Ok(wall)
}
pub fn once(value: &Value, zone: &str) -> Result<i64> {
    let zone = timezone(zone)?;
    let mut wall = wall(value)?;
    for _ in 0..=2880 {
        match zone.from_local_datetime(&wall) {
            LocalResult::Single(at) => return Ok(at.timestamp_millis()),
            LocalResult::Ambiguous(a, b) => return Ok(a.min(b).timestamp_millis()),
            LocalResult::None => {
                wall = wall
                    .checked_add_signed(Duration::minutes(1))
                    .ok_or_else(invalid)?
            }
        }
    }
    Err(error(422, "Scheduled time cannot be mapped."))
}
pub fn valid_epoch(at: i64) -> Result<i64> {
    if (0..=MAX_TIME).contains(&at) {
        Ok(at)
    } else {
        Err(invalid())
    }
}
pub fn interval(value: &Value) -> i64 {
    match string(value, "kind") {
        "legacy_interval" => value["intervalMs"].as_i64().unwrap_or(0),
        "interval" => {
            // Legacy routine intervals store exact seconds as fractional minutes.
            (value["amount"].as_f64().unwrap_or(0.)
                * match string(value, "unit") {
                    "minute" => 60000.,
                    "hour" => 3600000.,
                    "day" => 86400000.,
                    _ => 0.,
                })
            .round() as i64
        }
        _ => 0,
    }
}
pub fn parse(value: &Value) -> Result<Value> {
    let valid = match string(value, "kind") {
        "instant" => data::closed(value, &["kind", "at"]) && integer(&value["at"], 0, MAX_TIME),
        "once" => data::closed(value, &["kind", "date", "time"]) && wall(value).is_ok(),
        "cron" => {
            data::closed(value, &["kind", "expression"])
                && cron(string(value, "expression")).is_ok()
        }
        "legacy_interval" => {
            data::closed(value, &["kind", "intervalMs", "anchorAt"])
                && integer(&value["intervalMs"], 60000, 31536000000)
                && integer(&value["anchorAt"], 0, MAX_TIME)
        }
        "interval" => {
            data::closed(value, &["kind", "amount", "unit", "anchorAt"])
                && integer(&value["amount"], 1, 9007199254740991)
                && integer(&value["anchorAt"], 0, MAX_TIME)
                && (300000..=MAX_TIME).contains(&interval(value))
        }
        _ => false,
    };
    if !valid {
        return Err(invalid());
    }
    Ok(value.clone())
}
struct Cron {
    fields: Vec<Vec<u32>>,
    day: bool,
    weekday: bool,
}
fn cron(expression: &str) -> Result<Cron> {
    let bad = || error(400, "Invalid cron expression.");
    let parts = expression.split_whitespace().collect::<Vec<_>>();
    if expression.encode_utf16().count() > 256 || parts.len() != 5 {
        return Err(bad());
    }
    let mut fields = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        let (min, max) = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 6)][index];
        let mut values = std::collections::BTreeSet::new();
        let number = |s: &str| {
            s.parse::<u32>()
                .ok()
                .filter(|_| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .ok_or_else(bad)
        };
        for term in part.split(',') {
            let pieces = term.split('/').collect::<Vec<_>>();
            if pieces.len() > 2 {
                return Err(bad());
            }
            let step = if pieces.len() == 2 {
                number(pieces[1])?
            } else {
                1
            };
            if step == 0 || step > max {
                return Err(bad());
            }
            let (start, end) = if pieces[0] == "*" {
                (min, max)
            } else {
                let range = pieces[0].split('-').collect::<Vec<_>>();
                if range.len() > 2 {
                    return Err(bad());
                }
                let start = number(range[0])?;
                (
                    start,
                    if range.len() == 2 {
                        number(range[1])?
                    } else if pieces.len() == 2 {
                        max
                    } else {
                        start
                    },
                )
            };
            if start < min || end > max || start > end {
                return Err(bad());
            }
            values.extend((start..=end).step_by(step as usize));
        }
        fields.push(values.into_iter().collect());
    }
    Ok(Cron {
        fields,
        day: parts[2] != "*",
        weekday: parts[4] != "*",
    })
}
pub fn scan(
    expression: &str,
    zone: &str,
    after: i64,
    through: i64,
    mut visit: impl FnMut(i64, u8) -> Result<bool>,
) -> Result<()> {
    let spec = cron(expression)?;
    let zone = timezone(zone)?;
    let limit = through.min(MAX_TIME);
    let offset = |at| -> Result<i64> {
        Ok(i64::from(
            date(at)?
                .with_timezone(&zone)
                .offset()
                .fix()
                .local_minus_utc(),
        ) * 1000)
    };
    let mut segment = |from: i64, to: i64, offset: i64, fold_end: i64| -> Result<bool> {
        if to <= after {
            return Ok(true);
        }
        let mut day = date(from + offset)?.date_naive();
        let last = date(to - 1 + offset)?.date_naive();
        while day <= last {
            let d = spec.fields[2].contains(&day.day());
            let w = spec.fields[4].contains(&day.weekday().num_days_from_sunday());
            let matches = if spec.day && spec.weekday {
                d || w
            } else if spec.day {
                d
            } else {
                w
            };
            if spec.fields[3].contains(&day.month()) && matches {
                for hour in &spec.fields[1] {
                    for minute in &spec.fields[0] {
                        let wall = day.and_hms_opt(*hour, *minute, 0).ok_or_else(invalid)?;
                        let at = wall.and_utc().timestamp_millis() - offset;
                        if at > after
                            && at >= from
                            && at < to
                            && at <= limit
                            && !visit(at, u8::from(at < fold_end))?
                        {
                            return Ok(false);
                        }
                    }
                }
            }
            day = day.succ_opt().ok_or_else(invalid)?;
        }
        Ok(true)
    };
    let mut start = after.div_euclid(3600000) * 3600000 - 48 * 3600000;
    let mut previous = offset(start)?;
    let mut fold_end = 0;
    while start <= limit {
        let end = (start + 6 * 3600000).min(limit + 1);
        let current = offset(end)?;
        if current != previous {
            let mut low = start.div_euclid(1000);
            let mut high = (end + 999).div_euclid(1000);
            while high - low > 1 {
                let middle = (low + high).div_euclid(2);
                if offset(middle * 1000)? == previous {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            let transition = high * 1000;
            if !segment(start, transition, previous, fold_end)? {
                break;
            }
            if current < previous {
                fold_end = transition + previous - current;
            }
            if !segment(transition, end, current, fold_end)? {
                break;
            }
        } else if !segment(start, end, previous, fold_end)? {
            break;
        }
        start = end;
        previous = current;
    }
    Ok(())
}

pub fn cron_occurrences(
    expression: &str,
    zone: &str,
    after: i64,
    through: i64,
    first_fold_only: bool,
    mut visit: impl FnMut(i64) -> Result<bool>,
) -> Result<()> {
    scan(expression, zone, after, through, |at, fold| {
        if first_fold_only && fold != 0 {
            Ok(true)
        } else {
            visit(at)
        }
    })
}

// Number of elapsed intervals after the due run, and the next instant after `now`.
pub fn elapsed(due: i64, duration: Option<i64>, now: i64) -> (i64, Option<i64>) {
    let Some(duration) = duration.filter(|n| *n > 0) else {
        return (0, None);
    };
    let missed = (now - due).max(0) / duration;
    let next = due.saturating_add((missed + 1).saturating_mul(duration));
    (missed, (next <= MAX_TIME).then_some(next))
}

pub fn advance_cron(
    expression: &str,
    zone: &str,
    after: i64,
    now: i64,
    first_fold_only: bool,
) -> Result<(i64, Option<i64>)> {
    let mut missed = 0;
    let mut next = None;
    cron_occurrences(
        expression,
        zone,
        after,
        now.saturating_add(CRON_HORIZON),
        first_fold_only,
        |at| {
            if at > now {
                next = Some(at);
                Ok(false)
            } else {
                missed += 1;
                Ok(true)
            }
        },
    )?;
    Ok((missed, next))
}
#[cfg(test)]
fn candidates(expression: &str, zone: &str, after: i64, through: i64) -> Result<Vec<(i64, u8)>> {
    let mut result = Vec::new();
    scan(expression, zone, after, through, |at, index| {
        result.push((at, index));
        Ok(true)
    })?;
    Ok(result)
}
pub fn times(value: &Value, zone: &str, after: i64, count: usize) -> Result<Vec<i64>> {
    if !(1..=10).contains(&count) {
        return Err(invalid());
    }
    match string(value, "kind") {
        "cron" => {
            let mut times = Vec::new();
            scan(
                string(value, "expression"),
                zone,
                after,
                after + 1825 * 86400000,
                |at, index| {
                    if index == 0 {
                        times.push(at);
                    }
                    Ok(times.len() < count)
                },
            )?;
            Ok(times)
        }
        "once" => Ok(vec![once(value, zone)?]
            .into_iter()
            .filter(|at| *at > after)
            .collect()),
        "instant" => Ok(value["at"]
            .as_i64()
            .filter(|at| *at > after)
            .into_iter()
            .collect()),
        "interval" | "legacy_interval" => {
            let duration = interval(value);
            let anchor = value["anchorAt"].as_i64().ok_or_else(invalid)?;
            let first = if anchor > after {
                anchor
            } else {
                anchor + (after - anchor + duration) / duration * duration
            };
            Ok((0..count).map(|i| first + i as i64 * duration).collect())
        }
        _ => Err(invalid()),
    }
}
pub fn next_cron(expression: &str, zone: &str, now: i64, first_fold_only: bool) -> Result<i64> {
    let mut previous = None;
    let mut first = None;
    scan(
        expression,
        zone,
        now - 300000,
        now + 1825 * 86400000,
        |at, index| {
            if previous.is_some_and(|p| at - p < 300000) {
                return Err(error(400, "Cron interval must be at least five minutes."));
            }
            if at > now && (!first_fold_only || index == 0) {
                first.get_or_insert(at);
            }
            previous = Some(at);
            Ok(true)
        },
    )?;
    first.ok_or_else(|| {
        Failure::new(
            422,
            "schedule_unsatisfiable",
            "Cron has no occurrence within 1,825 days.",
        )
    })
}
pub fn local(value: &Value, zone: &str, now: i64) -> Result<(Value, i64, Option<i64>)> {
    let spec = parse(value)?;
    let duration = interval(&spec);
    let next = match string(&spec, "kind") {
        "legacy_interval" => return Err(error(400, "Use an interval schedule for new schedules.")),
        "once" => once(&spec, zone)?,
        "instant" => spec["at"].as_i64().ok_or_else(invalid)?,
        "interval" => spec["anchorAt"].as_i64().ok_or_else(invalid)?,
        "cron" => next_cron(string(&spec, "expression"), zone, now, true).map_err(|failure| {
            if failure.code == "schedule_unsatisfiable" {
                error(failure.status, failure.message)
            } else {
                failure
            }
        })?,
        _ => return Err(invalid()),
    };
    if !(1..=MAX_TIME).contains(&next) || duration > 31536000000 {
        return Err(error(400, "Schedule is outside the supported range."));
    }
    Ok((spec, next, (duration != 0).then_some(duration)))
}
pub fn preview(db: &Connection, q: &[(String, String)], now: i64) -> Result<Value> {
    if q.iter().any(|(k, _)| {
        !["zone", "schedule"].contains(&k.as_str()) || q.iter().filter(|(n, _)| n == k).count() != 1
    }) {
        return Err(error(400, "Invalid schedule preview."));
    }
    let zone = match data::param(q, "zone").filter(|zone| !zone.is_empty()) {
        Some(value) => zone(Some(value))?,
        None => home(db)?,
    };
    let mut resolved = None;
    let times = if let Some(raw) = data::param(q, "schedule") {
        let value: Value =
            serde_json::from_str(raw).map_err(|_| error(400, "Invalid schedule preview."))?;
        let spec = if value["kind"] == "legacy_interval" {
            parse(&value)?
        } else {
            local(&value, &zone, now)?.0
        };
        if spec["kind"] == "once" {
            resolved = Some(once(&spec, &zone)?);
        }
        times(&spec, &zone, now, 3)?
    } else {
        vec![]
    };
    let mut response = json!({"zone":zone,"times":times});
    if let Some(at) = resolved {
        response["at"] = json!(at);
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn epoch(s: &str) -> i64 {
        DateTime::parse_from_rfc3339(s)
            .map(|t| t.timestamp_millis())
            .unwrap_or(-1)
    }
    #[test]
    fn clocks_preserve_gaps_folds_and_skipped_dates() -> Result<()> {
        assert_eq!(
            once(
                &json!({"date":"2026-03-08","time":"02:30"}),
                "America/New_York"
            )?,
            epoch("2026-03-08T07:00:00Z")
        );
        assert_eq!(
            once(
                &json!({"date":"2026-11-01","time":"01:30"}),
                "America/New_York"
            )?,
            epoch("2026-11-01T05:30:00Z")
        );
        assert_eq!(
            once(&json!({"date":"2011-12-30","time":"12:00"}), "Pacific/Apia")?,
            epoch("2011-12-30T10:00:00Z")
        );
        let fold = candidates(
            "30 1 * * *",
            "America/New_York",
            epoch("2026-11-01T00:00:00Z"),
            epoch("2026-11-02T00:00:00Z"),
        )?;
        assert_eq!(
            fold,
            vec![
                (epoch("2026-11-01T05:30:00Z"), 0),
                (epoch("2026-11-01T06:30:00Z"), 1)
            ]
        );
        assert!(candidates(
            "30 2 * * *",
            "America/New_York",
            epoch("2026-03-08T00:00:00Z"),
            epoch("2026-03-09T00:00:00Z")
        )?
        .is_empty());
        for expr in ["* * * * *", "0 9 30 2 *", "0 9 * * 7", "61 9 * * *"] {
            assert!(local(
                &json!({"kind":"cron","expression":expr}),
                "UTC",
                epoch("2026-01-01T00:00:00Z")
            )
            .is_err());
        }
        Ok(())
    }
    #[test]
    fn local_schedule_preview_keeps_weekdays_wall_clocks_and_elapsed_intervals() -> Result<()> {
        for (expression, zone, after, expected) in [
            (
                "0 9 * * 1-5",
                "Asia/Jakarta",
                "2026-11-13T09:00:00Z",
                [
                    "2026-11-16T02:00:00Z",
                    "2026-11-17T02:00:00Z",
                    "2026-11-18T02:00:00Z",
                ],
            ),
            (
                "30 9 * * 1,3,5",
                "UTC",
                "2026-11-13T09:00:00Z",
                [
                    "2026-11-13T09:30:00Z",
                    "2026-11-16T09:30:00Z",
                    "2026-11-18T09:30:00Z",
                ],
            ),
            (
                "0 9 * * *",
                "America/New_York",
                "2026-03-07T00:00:00Z",
                [
                    "2026-03-07T14:00:00Z",
                    "2026-03-08T13:00:00Z",
                    "2026-03-09T13:00:00Z",
                ],
            ),
            (
                "30 1 * * *",
                "America/New_York",
                "2026-11-01T00:00:00Z",
                [
                    "2026-11-01T05:30:00Z",
                    "2026-11-02T06:30:00Z",
                    "2026-11-03T06:30:00Z",
                ],
            ),
            (
                "30 2 * * *",
                "America/New_York",
                "2026-03-07T08:00:00Z",
                [
                    "2026-03-09T06:30:00Z",
                    "2026-03-10T06:30:00Z",
                    "2026-03-11T06:30:00Z",
                ],
            ),
        ] {
            let after = epoch(after);
            let (spec, next, _) =
                local(&json!({"kind":"cron","expression":expression}), zone, after)?;
            assert_eq!(next, epoch(expected[0]));
            assert_eq!(times(&spec, zone, after, 3)?, expected.map(epoch));
        }
        let anchor = epoch("2026-03-08T06:00:00Z");
        let zone = "America/New_York";
        let (elapsed, _, _) = local(
            &json!({"kind":"interval","amount":2,"unit":"hour","anchorAt":anchor}),
            zone,
            anchor - 1,
        )?;
        assert_eq!(
            times(&elapsed, zone, anchor - 1, 3)?,
            [anchor, anchor + 7200000, anchor + 14400000]
        );
        assert!(local(&json!({"kind":"cron","expression":"0 9 * *"}), zone, anchor).is_err());
        Ok(())
    }
}
