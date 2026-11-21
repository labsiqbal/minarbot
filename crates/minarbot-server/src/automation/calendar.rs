use super::*;
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use rusqlite::named_params;

const DAY: i64 = 86_400_000;
const MAX_DAYS: i64 = 62;
const LIMIT: usize = 500;
const STATUSES: &[&str] = &[
    "scheduled",
    "running",
    "done",
    "failed",
    "paused",
    "no_update",
];

fn invalid() -> Failure {
    error(400, "Choose a valid calendar range of up to 62 days.")
}

fn project_exists(db: &Connection, project: &str) -> Result<()> {
    if !uuid(project) {
        return Err(invalid());
    }
    if !data::exists(
        db,
        "SELECT 1 FROM local_projects WHERE id=? AND deleted=0",
        [project],
    )? {
        return Err(error(404, "Project not found."));
    }
    Ok(())
}

fn day(at: i64, zone: Tz) -> Result<NaiveDate> {
    Ok(DateTime::<Utc>::from_timestamp_millis(at)
        .ok_or_else(invalid)?
        .with_timezone(&zone)
        .date_naive())
}

fn midnight(date: &str, zone: &str) -> Result<i64> {
    schedule::once(&json!({"date":date,"time":"00:00"}), zone)
}

fn expand(
    row: &Value,
    zone: Tz,
    from: i64,
    to: i64,
    mut visit: impl FnMut(i64, i64) -> Result<bool>,
) -> Result<()> {
    let due = row["next_at"].as_i64().ok_or_else(Failure::storage)?;
    if row["status"] == "paused" {
        if (from..to).contains(&due) {
            visit(due, 1)?;
        }
        return Ok(());
    }
    let spec = row["spec_json"]
        .as_str()
        .map(serde_json::from_str::<Value>)
        .transpose()
        .map_err(|_| Failure::storage())?;
    if let Some(spec) = spec.filter(|spec| spec["kind"] == "cron") {
        let mut daily = Vec::new();
        let mut current = None;
        let mut flush = |points: &mut Vec<i64>| -> Result<bool> {
            // Keep ordinary cron slots individually addressable.
            if points.len() > 8 {
                let keep = visit(points[0], points.len() as i64)?;
                points.clear();
                return Ok(keep);
            }
            for point in points.drain(..) {
                if !visit(point, 1)? {
                    return Ok(false);
                }
            }
            Ok(true)
        };
        let mut collect = |at| {
            let date = day(at, zone)?;
            if current != Some(date) && !flush(&mut daily)? {
                return Ok(false);
            }
            current = Some(date);
            daily.push(at);
            Ok(true)
        };
        if (from..to).contains(&due) {
            collect(due)?;
        }
        schedule::cron_occurrences(
            string(&spec, "expression"),
            row["schedule_zone"].as_str().unwrap_or(zone.name()),
            (from - 1).max(due),
            to - 1,
            true,
            collect,
        )?;
        flush(&mut daily)?;
        return Ok(());
    }
    let Some(interval) = row["interval_ms"].as_i64().filter(|n| *n > 0) else {
        if (from..to).contains(&due) {
            visit(due, 1)?;
        }
        return Ok(());
    };
    // Include the due time in the first day's count for elapsed schedules.
    let mut next = if due >= from {
        Some(due)
    } else {
        schedule::elapsed(due, Some(interval), from - 1).1
    };
    while let Some(at) = next.filter(|at| *at < to) {
        if interval < DAY {
            let next_day = day(at, zone)?.succ_opt().ok_or_else(invalid)?;
            let last = midnight(&next_day.to_string(), zone.name())?.min(to) - 1;
            let (missed, following) = schedule::elapsed(at, Some(interval), last);
            if !visit(at, missed + 1)? {
                break;
            }
            next = following;
        } else {
            if !visit(at, 1)? {
                break;
            }
            next = schedule::elapsed(at, Some(interval), at).1;
        }
    }
    Ok(())
}

fn item(row: &Value, at: i64, count: i64) -> Value {
    let id = string(row, "id");
    let kind = string(row, "kind");
    let key = row["run"].as_str().map_or_else(
        || format!("{kind}:{id}:{at}"),
        |run| format!("{kind}:{run}"),
    );
    json!({
        "id":key,"sourceId":id,"kind":kind,"title":row["name"],"agent":row["agent"],
        "project":row["project"],"at":at,"status":row["status"],"count":count,
        "run":row["run"],"thread":row["thread"],"reportTo":row["report_to"],"report":row["report"],
        "revision":row["revision"],"recurring":row["interval_ms"].is_number() || row["cron"] == 1,
        "editable":row["editable"] != 0
    })
}

pub fn read(db: &Connection, q: &[(String, String)], now: i64) -> Result<Value> {
    let keys = ["start", "end", "project", "agent", "kind", "status"];
    if q.iter().any(|(key, _)| {
        !keys.contains(&key.as_str()) || q.iter().filter(|(k, _)| k == key).count() > 1
    }) {
        return Err(invalid());
    }
    let param = |key| data::param(q, key);
    let zone = schedule::zone(None)?;
    let tz: Tz = zone.parse().map_err(|_| Failure::storage())?;
    let start = param("start").ok_or_else(invalid)?;
    let end = param("end").ok_or_else(invalid)?;
    let range = NaiveDate::parse_from_str(end, "%Y-%m-%d")
        .ok()
        .zip(NaiveDate::parse_from_str(start, "%Y-%m-%d").ok());
    if !range.is_some_and(|(end, start)| (1..=MAX_DAYS).contains(&(end - start).num_days())) {
        return Err(invalid());
    }
    let from = midnight(start, &zone)?;
    let to = midnight(end, &zone)?;
    if from < 0 || to > schedule::MAX_TIME || from > to {
        return Err(invalid());
    }
    let project = param("project");
    if let Some(project) = project.filter(|p| *p != "none") {
        project_exists(db, project)?;
    }
    let agent = param("agent");
    let kind = param("kind");
    let status = param("status");
    if agent.is_some_and(|a| !uuid(a))
        || kind.is_some_and(|k| !["routine", "workflow"].contains(&k))
        || status.is_some_and(|s| !STATUSES.contains(&s))
    {
        return Err(invalid());
    }

    let mut items = Vec::new();
    let cap = (LIMIT + 1) as i64;
    let past = all(
        db,
        include_str!("calendar-runs.sql"),
        named_params! {
            ":project":project, ":agent":agent, ":kind":kind, ":status":status,
            ":from":from, ":to":to, ":limit":cap,
        },
    )?;
    for row in &past {
        items.push(item(
            row,
            row["at"].as_i64().ok_or_else(Failure::storage)?,
            1,
        ));
    }
    let mut truncated = items.len() > LIMIT;
    if items.len() <= LIMIT && status.is_none_or(|s| ["paused", "scheduled"].contains(&s)) {
        let schedules = all(
            db,
            include_str!("calendar-schedules.sql"),
            named_params! {
                ":project":project, ":agent":agent, ":kind":kind, ":status":status,
                ":from":from, ":to":to, ":limit":cap,
            },
        )?;
        truncated |= schedules.len() > LIMIT;
        for row in schedules.iter().take(LIMIT) {
            let start = if row["status"] == "paused" {
                from
            } else {
                from.max(now)
            };
            if start >= to {
                continue;
            }
            expand(row, tz, start, to, |at, count| {
                items.push(item(row, at, count));
                Ok(items.len() <= LIMIT)
            })?;
            if items.len() > LIMIT {
                truncated = true;
                break;
            }
        }
    }
    items.truncate(LIMIT);
    items.sort_by(|a, b| {
        a["at"]
            .as_i64()
            .cmp(&b["at"].as_i64())
            .then_with(|| string(a, "id").cmp(string(b, "id")))
    });
    Ok(json!({"zone":zone,"now":now,"items":items,"truncated":truncated}))
}

pub fn export(db: &Connection, q: &[(String, String)], now: i64) -> Result<Response> {
    let [(key, project)] = q else {
        return Err(invalid());
    };
    if key != "project" {
        return Err(invalid());
    }
    project_exists(db, project)?;
    let rows = all(db, "SELECT * FROM routines r WHERE project=? AND enabled=1 AND trigger='schedule' AND id NOT IN(SELECT routine FROM local_routine_deletions) AND NOT EXISTS(SELECT 1 FROM agent_visibility v WHERE v.agent=r.agent AND v.hidden=1) ORDER BY next_at,id", [project])?;
    crate::connectors::calendar::download(db, &rows, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verify(spec: Value, after: i64, end: i64, first: i64, expected_len: usize) -> Result<()> {
        let zone = chrono_tz::America::New_York;
        let (spec, due, duration) = schedule::local(&spec, zone.name(), after)?;
        assert_eq!(due, first);
        let row = json!({"spec_json":spec.to_string(),"schedule_zone":zone.name(),"next_at":due,"interval_ms":duration,"status":"scheduled"});
        let mut actual = Vec::new();
        expand(&row, zone, after, end, |at, count| {
            assert_eq!(count, 1);
            actual.push(at);
            Ok(true)
        })?;
        let mut expected = Vec::new();
        let mut next = Some(due);
        while let Some(at) = next.filter(|at| *at < end) {
            expected.push(at);
            next = if spec["kind"] == "cron" {
                schedule::advance_cron(string(&spec, "expression"), zone.name(), at, at, true)?.1
            } else {
                schedule::elapsed(at, duration, at).1
            };
        }
        assert_eq!(actual.len(), expected_len, "{spec}");
        assert_eq!(actual, expected, "{spec}");
        Ok(())
    }

    fn epoch(value: &str) -> Result<i64> {
        Ok(DateTime::parse_from_rfc3339(value)
            .map_err(|_| invalid())?
            .timestamp_millis())
    }

    #[test]
    fn automation_calendar_matches_scheduler_at_dst_and_cron_floor() -> Result<()> {
        let march = epoch("2026-03-07T00:00:00Z")?;
        let end = march + 3 * DAY;
        verify(
            json!({"kind":"instant","at":march + 60000}),
            march,
            end,
            march + 60000,
            1,
        )?;
        verify(
            json!({"kind":"once","date":"2026-03-08","time":"02:30"}),
            march,
            end,
            epoch("2026-03-08T07:00:00Z")?,
            1,
        )?;
        verify(
            json!({"kind":"interval","amount":36,"unit":"hour","anchorAt":march}),
            march,
            end,
            march,
            2,
        )?;
        for (expression, first, len) in [
            ("0 9 * * *", "2026-03-07T14:00:00Z", 3),
            ("0 9 * * 1-5", "2026-03-09T13:00:00Z", 1),
            ("0 9 * * 0", "2026-03-08T13:00:00Z", 1),
            ("30 2 * * *", "2026-03-07T07:30:00Z", 2),
            ("0,5 9 * * *", "2026-03-07T14:00:00Z", 6),
        ] {
            verify(
                json!({"kind":"cron","expression":expression}),
                march,
                end,
                epoch(first)?,
                len,
            )?;
        }
        let november = epoch("2026-11-01T00:00:00Z")?;
        let first_fold = epoch("2026-11-01T05:30:00Z")?;
        verify(
            json!({"kind":"once","date":"2026-11-01","time":"01:30"}),
            november,
            november + DAY,
            first_fold,
            1,
        )?;
        verify(
            json!({"kind":"cron","expression":"30 1 * * *"}),
            november,
            november + DAY,
            first_fold,
            1,
        )?;
        assert!(schedule::local(
            &json!({"kind":"cron","expression":"0,4 9 * * *"}),
            "America/New_York",
            march
        )
        .is_err());
        Ok(())
    }

    #[test]
    fn automation_calendar_bounds_range_and_collapses_dense_intervals() -> Result<()> {
        struct TestDir(std::path::PathBuf);
        impl Drop for TestDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = TestDir(
            std::env::temp_dir().join(format!("minarbot-calendar-{}", uuid::Uuid::new_v4())),
        );
        let home_zone = schedule::zone(None)?;
        let from = midnight("2026-03-08", &home_zone)?;
        let database = crate::db::open(&dir.0.join("state.sqlite"), false, from)
            .map_err(|_| Failure::storage())?;
        let db = &database.connection;
        crate::files::init(db).map_err(|_| Failure::storage())?;
        crate::data::initialize(&database).map_err(|_| Failure::storage())?;
        super::super::initialize(db)?;

        let q = |end: &str| {
            vec![
                ("start".into(), "2026-03-08".into()),
                ("end".into(), end.into()),
            ]
        };
        for end in ["2026-03-08", "2026-02-01", "2026-05-10", "2026-03-011"] {
            assert!(matches!(read(db, &q(end), from), Err(e) if e.status == 400));
        }
        db.execute(
            "INSERT INTO agents(id,name,model,instructions) VALUES('a','Owner','fixture','')",
            [],
        )?;
        db.execute("INSERT INTO routines(id,agent,name,prompt,next_at,interval_ms,enabled) VALUES('dense','a','Dense','test',?,60000,1)", [from])?;
        let end = midnight("2026-03-09", &home_zone)?;
        let dense = read(db, &q("2026-03-09"), from)?;
        assert_eq!(dense["items"][0]["count"], (end - from) / 60000);
        assert_eq!(dense["items"][0]["at"], from);
        let zone = chrono_tz::America::New_York;
        let spring = midnight("2026-03-08", zone.name())?;
        let end = midnight("2026-03-09", zone.name())?;
        let row = json!({"next_at":spring,"interval_ms":300000,"status":"scheduled"});
        let mut points = Vec::new();
        expand(&row, zone, spring, end, |at, count| {
            points.push((at, count));
            Ok(true)
        })?;
        assert_eq!(points, vec![(spring, 23 * 12)]);
        assert_eq!(
            schedule::elapsed(spring, Some(300000), end - 1),
            (23 * 12 - 1, Some(end))
        );

        db.execute("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<510)
          INSERT INTO routines(id,agent,name,prompt,next_at,enabled) SELECT 'once-'||i,'a','Once','test',?+i,1 FROM n", [from])?;
        let overflow = read(db, &q("2026-03-09"), from)?;
        assert_eq!(overflow["items"].as_array().map(Vec::len), Some(LIMIT));
        assert_eq!(overflow["truncated"], true);
        Ok(())
    }
}
