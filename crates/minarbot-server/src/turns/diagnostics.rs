use super::*;

pub fn secrets(state: &AppState, db: &Connection) -> Result<Vec<String>> {
    let mut values = data::providers::accounts_at(db)?
        .into_iter()
        .filter_map(|account| account.key)
        .collect::<Vec<_>>();
    values.extend(crate::mcp::secrets::project(state, db, None)?);
    values.extend(crate::computer::vault::redactions(state, db)?);
    if let Some(token) = state.config.remaining_data.script_token.clone() {
        values.push(token);
    }
    Ok(values)
}

pub fn text(raw: &str, secrets: &[String]) -> String {
    static PATTERN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?i)\b(?:sk-[A-Za-z0-9_-]{16,}|bsi_[A-Za-z0-9_-]+|Bearer\s+[A-Za-z0-9._~+/-]+=*|([\w-]*(?:api[_-]?key|token|secret|password)["']?\s*[:=]\s*)(?:"(?:\\.|[^"\\])*"|'[^']*'|[^\s,;"'}]+))"#)
            .unwrap_or_else(|_| unreachable!("invalid diagnostic redaction pattern"))
    });
    let redacted = crate::mcp::secrets::redact(raw, secrets, false);
    PATTERN
        .replace_all(&redacted, |captures: &regex::Captures<'_>| {
            if let Some(key) = captures.get(1) {
                format!("{}\"[REDACTED]\"", key.as_str())
            } else {
                "[REDACTED]".to_owned()
            }
        })
        .into_owned()
}

pub fn bounded(raw: &str) -> String {
    const MAX_BYTES: usize = 16 * 1024;
    if raw.len() <= MAX_BYTES {
        return raw.to_owned();
    }
    format!("{}\n…", &raw[..raw.floor_char_boundary(MAX_BYTES)])
}

fn mask_fields(value: &mut Value, secrets: &[String]) {
    match value {
        Value::Object(fields) => {
            for (key, item) in fields {
                let key = key.to_ascii_lowercase().replace(['-', '_'], "");
                if [
                    "authorization",
                    "cookie",
                    "setcookie",
                    "credentials",
                    "apikey",
                    "token",
                    "secret",
                    "password",
                    "privatekey",
                ]
                .iter()
                .any(|name| key.ends_with(name))
                {
                    *item = json!("[REDACTED]");
                } else {
                    mask_fields(item, secrets);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| mask_fields(item, secrets)),
        Value::String(raw) => *raw = text(raw, secrets),
        _ => {}
    }
}

pub fn display(value: &Value, secrets: &[String]) -> Option<String> {
    if value.is_null() {
        return None;
    }
    if let Some(raw) = value.as_str() {
        return Some(bounded(&text(raw, secrets)));
    }
    let mut value = value.clone();
    mask_fields(&mut value, secrets);
    let raw = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    Some(bounded(&text(&raw, secrets)))
}

pub fn tool_failed(result: &Value) -> bool {
    result.get("error").is_some_and(|value| !value.is_null())
        || result["isError"] == true
        || result["exitCode"].as_i64().is_some_and(|code| code != 0)
        || matches!(result["status"].as_str(), Some("error" | "failed"))
}

pub struct Clock<'a> {
    state: &'a AppState,
    turn: &'a str,
    id: &'a str,
}

impl<'a> Clock<'a> {
    pub fn start(state: &'a AppState, turn: &'a str, id: &'a str) -> Self {
        let clock = Self { state, turn, id };
        if let Ok(guard) = state.database.lock() {
            let now = iso(&guard.connection, state.config.now()).ok();
            let _ = guard.connection.execute(
                "INSERT OR IGNORE INTO turn_step_diagnostics(turn,id,started) VALUES(?,?,?)",
                params![turn, id, now],
            );
        }
        clock
    }
}

impl Drop for Clock<'_> {
    fn drop(&mut self) {
        if let Ok(guard) = self.state.database.lock() {
            let _ = guard.connection.execute(
                "UPDATE turn_step_diagnostics SET finished=? WHERE turn=? AND id=?",
                params![
                    iso(&guard.connection, self.state.config.now()).ok(),
                    self.turn,
                    self.id
                ],
            );
        }
    }
}
