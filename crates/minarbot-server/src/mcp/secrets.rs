use super::*;
pub fn project(state: &AppState, db: &Connection, project: Option<&str>) -> Result<Vec<String>> {
    let mut values = Vec::new();
    if data::exists(
        db,
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='project_environment_redactions'",
        [],
    )? {
        for row in all(db,"SELECT project,name,ciphertext FROM project_environment_redactions WHERE (? IS NULL OR project=?)",[project,project])?{
            values.push(data::environment::decrypt(state,&row,string(&row,"project"))?);
        }
    }
    values.retain(|v| !v.is_empty());
    values.sort_by_key(|s| std::cmp::Reverse(s.len()));
    Ok(values)
}
pub fn reflected(bytes: &[u8], secret: &str) -> bool {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let encodings = [
        secret.as_bytes().to_vec(),
        secret.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        secret.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        STANDARD.encode(secret).into_bytes(),
    ];
    encodings
        .iter()
        .any(|s| !s.is_empty() && bytes.windows(s.len()).any(|w| w == s))
}

pub(crate) fn redact(text: &str, secrets: &[String], streaming: bool) -> String {
    let mut value = text.to_owned();
    let mut sorted = secrets.iter().filter(|s| !s.is_empty()).collect::<Vec<_>>();
    sorted.sort_by_key(|v| std::cmp::Reverse(v.len()));
    for secret in &sorted {
        value = value.replace(secret.as_str(), "[REDACTED]");
        let encoded = json!(secret).to_string();
        if let Some(escaped) = encoded.get(1..encoded.len().saturating_sub(1)) {
            value = value.replace(escaped, "[REDACTED]");
        }
    }
    if streaming {
        for secret in sorted {
            for size in (1..secret.len().min(value.len() + 1)).rev() {
                if secret.is_char_boundary(size) && value.ends_with(&secret[..size]) {
                    value.truncate(value.len() - size);
                    break;
                }
            }
        }
    }
    value
}

pub(crate) fn redact_content(value: &mut Value, secrets: &[String]) {
    if secrets.is_empty() {
        return;
    }
    match value {
        Value::String(value) => *value = redact(value, secrets, false),
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| redact_content(value, secrets)),
        Value::Object(fields) => fields.iter_mut().for_each(|(name, value)| {
            // Preserve message routing identifiers and binary image payloads.
            if !["id", "type", "role", "name", "tool_call_id", "image_url"].contains(&name.as_str())
            {
                redact_content(value, secrets);
            }
        }),
        _ => {}
    }
}
