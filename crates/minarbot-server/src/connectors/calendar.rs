use super::*;
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace("\r\n", "\\n")
        .replace(['\r', '\n'], "\\n")
        .replace(',', "\\,")
        .replace(';', "\\;")
}
pub fn render(db: &Connection, entries: &[Value], now: i64) -> Result<String> {
    let stamp = |at| {
        db.query_row(
            "SELECT strftime('%Y%m%dT%H%M%SZ',?/1000.0,'unixepoch')",
            [at],
            |r| r.get::<_, String>(0),
        )
        .map_err(Failure::from)
    };
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_owned(),
        "VERSION:2.0".to_owned(),
        "PRODID:-//Minarbot//Routines//EN".to_owned(),
        "CALSCALE:GREGORIAN".to_owned(),
    ];
    let mut notes = vec![];
    for row in entries {
        let spec = if row["schedule_json"].is_string() {
            crate::data::json(&row["schedule_json"])?
        } else {
            row["schedule"].clone()
        };
        if spec["kind"] == "cron" {
            notes.push(format!(
                "Omitted cron routine \"{}\": cron recurrence cannot be represented safely.",
                string(row, "name")
            ));
            continue;
        }
        let interval = row["interval_ms"].as_i64().unwrap_or(0);
        if interval > 2147483647_i64 * 1000 {
            notes.push(format!(
                "Omitted routine \"{}\": interval exceeds the calendar limit.",
                string(row, "name")
            ));
            continue;
        }
        let at = if spec["kind"] == "once" {
            match row["pendingDueAt"].as_i64().or_else(|| {
                crate::automation::schedule::once(
                    &spec,
                    row["zone"]
                        .as_str()
                        .unwrap_or_else(|| string(row, "schedule_zone")),
                )
                .ok()
            }) {
                Some(at) => at,
                None => {
                    notes.push(format!("Omitted routine \"{}\": home device time zone cannot resolve its schedule.",string(row,"name")));
                    continue;
                }
            }
        } else {
            row["next_at"].as_i64().ok_or_else(Failure::storage)?
        };
        lines.extend([
            "BEGIN:VEVENT".to_owned(),
            // An existing calendar import identifies this event by its full UID.
            format!("UID:{}@minarbot.local", string(row, "id")),
            format!("DTSTAMP:{}", stamp(now)?),
            format!("SEQUENCE:{}", row["revision"]),
            format!("DTSTART:{}", stamp(at)?),
            format!("SUMMARY:{}", escape(string(row, "name"))),
        ]);
        if interval != 0 {
            lines.push(format!(
                "RRULE:FREQ=SECONDLY;INTERVAL={}",
                interval as f64 / 1000.0
            ));
        }
        lines.push("END:VEVENT".to_owned());
    }
    if !notes.is_empty() {
        lines.push(format!("X-WR-CALDESC:{}", escape(&notes.join(" "))));
    }
    lines.push("END:VCALENDAR".to_owned());
    let mut body = String::new();
    for line in lines {
        let mut width = 0;
        for c in line.chars() {
            if width + c.len_utf8() > 75 {
                body.push_str("\r\n ");
                width = 1;
            }
            body.push(c);
            width += c.len_utf8();
        }
        body.push_str("\r\n");
    }
    Ok(body)
}
pub fn export(db: &Connection, owner: &str, now: i64) -> Result<Response> {
    crate::data::agents::read(db, owner)?;
    let entries=all(db,"SELECT * FROM routines WHERE agent=? AND enabled=1 AND trigger='schedule' ORDER BY next_at",[owner])?;
    download(db, &entries, now)
}
pub fn download(db: &Connection, entries: &[Value], now: i64) -> Result<Response> {
    Response::builder()
        .header("Content-Type", "text/calendar; charset=utf-8")
        .header(
            "Content-Disposition",
            "attachment; filename=\"minarbot-routines.ics\"",
        )
        .body(axum::body::Body::from(render(db, entries, now)?))
        .map_err(|_| Failure::storage())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn once_exports_current_home_clock_or_retained_pending_time() -> Result<()> {
        let db = Connection::open_in_memory()?;
        let mut entry = json!({
            "id": "schedule",
            "name": "First check",
            "revision": 2,
            "next_at": 1_710_037_800_000_i64,
            "schedule": {"kind": "once", "date": "2024-03-10", "time": "02:30"},
            "zone": "America/New_York",
        });
        let render = |entry: &Value| super::render(&db, std::slice::from_ref(entry), 0);
        assert!(render(&entry)?.contains("DTSTART:20240310T070000Z"));
        entry["zone"] = json!("UTC");
        assert!(render(&entry)?.contains("DTSTART:20240310T023000Z"));
        entry["pendingDueAt"] = json!(1_710_037_800_000_i64);
        entry["zone"] = json!("America/New_York");
        assert!(render(&entry)?.contains("DTSTART:20240310T023000Z"));
        Ok(())
    }
}
