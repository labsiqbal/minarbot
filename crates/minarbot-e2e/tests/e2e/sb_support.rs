//! Helpers shared by the `sb_*` modules (stale-b ports of #89).

use minarbot_testkit::{prelude::*, Server};

/// Milliseconds since the epoch of an RFC 3339 UTC timestamp such as
/// `2026-11-25T01:29:53.154Z`.
pub fn iso_millis(value: &Value) -> Result<i64> {
    let text = value
        .as_str()
        .ok_or_else(|| format!("not a time: {value}"))?;
    let bytes = text.as_bytes();
    let number = |range: std::ops::Range<usize>| -> Result<i64> {
        Ok(text
            .get(range)
            .ok_or_else(|| format!("bad time {text}"))?
            .parse::<i64>()?)
    };
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[10] != b'T' || !text.ends_with('Z') {
        return Err(format!("bad time {text}").into());
    }
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    let millis = if bytes.get(19) == Some(&b'.') {
        number(20..23)?
    } else {
        0
    };
    // Days from civil (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Ok(((days * 24 + hour) * 60 + minute) * 60_000 + second * 1000 + millis)
}

/// Whether `value` is an RFC 3339 timestamp.
pub fn is_time(value: &Value) -> bool {
    iso_millis(value).is_ok()
}

pub fn id(value: &Value) -> Result<String> {
    Ok(value["id"]
        .as_str()
        .ok_or_else(|| format!("no id: {value}"))?
        .to_owned())
}

/// Wait until turn `id` in `path` is done, failed or cancelled.
pub async fn settle(app: &Server, path: &str, id: &str) -> Result<Value> {
    app.wait_row(path, id, |row| {
        ["done", "failed", "cancelled"].contains(&row["status"].as_str().unwrap_or_default())
    })
    .await
}

/// Whether the last message of a model request is a tool result.
pub fn after_tool(request: &minarbot_testkit::model::ModelRequest) -> bool {
    request
        .messages()
        .last()
        .is_some_and(|message| message["role"] == "tool")
}

/// Parsed JSON content of the last tool result in a model request.
pub fn last_result(request: &minarbot_testkit::model::ModelRequest) -> Value {
    request
        .tool_results()
        .last()
        .and_then(|message| message["content"].as_str())
        .and_then(|content| serde_json::from_str(content).ok())
        .unwrap_or(Value::Null)
}

#[test]
fn iso_millis_matches_fixed_clock() -> Result<()> {
    ensure!(
        iso_millis(&json!("2026-11-18T11:06:40.000Z"))? == minarbot_testkit::FIXED_CLOCK,
        "fixed clock"
    );
    Ok(())
}

/// Whether a model request is the harness verification reminder that follows
/// an effectful turn (`turns::harness::reminder`), not the user's request.
pub fn reminder(request: &minarbot_testkit::model::ModelRequest) -> bool {
    request
        .last_user_text()
        .starts_with("One verification reminder")
}
