use super::{all, closed, error, one, string, Result};
use crate::{data::provider_store, harness, json_response, web::Failure, AppState};
use axum::{http::Method, response::Response};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{collections::BTreeSet, collections::HashMap, path::PathBuf};

const KINDS: &[&str] = &[
    "codex",
    "claude",
    "hermes",
    "antigravity",
    "grok",
    "pi",
    "opencode",
    "gemini",
    "kiro",
];
const SELECT: &str = "SELECT id,kind,display_name,enabled,binary_path,home_path,shadow_home_path,launch_args,environment,accent_color,revision,created,updated FROM provider_instances";
pub(crate) type LaunchConfig = (
    String,
    Option<PathBuf>,
    Vec<String>,
    HashMap<String, String>,
);

fn valid_slug(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn parse_text(body: &Value, key: &str, max: usize, fallback: &str) -> Result<String> {
    match body.get(key) {
        None => Ok(fallback.to_owned()),
        Some(value) => value
            .as_str()
            .filter(|value| {
                value.encode_utf16().count() <= max && !value.chars().any(char::is_control)
            })
            .map(|value| value.trim().to_owned())
            .ok_or_else(|| error(400, format!("Field {key} is invalid."))),
    }
}

fn parse_args(value: Option<&Value>, fallback: &str) -> Result<Value> {
    let value = value.unwrap_or(&Value::Null);
    let value = if value.is_null() {
        serde_json::from_str(fallback).map_err(|_| Failure::storage())?
    } else {
        value.clone()
    };
    let args = value
        .as_array()
        .filter(|args| args.len() <= 64)
        .ok_or_else(|| error(400, "Field launchArgs must be an array."))?;
    if args.iter().any(|arg| {
        arg.as_str().is_none_or(|arg| {
            arg.encode_utf16().count() > 2048 || arg.chars().any(char::is_control)
        })
    }) {
        return Err(error(400, "Field launchArgs contains an invalid argument."));
    }
    Ok(Value::Array(args.to_vec()))
}

fn parse_environment(value: Option<&Value>, fallback: &str) -> Result<Vec<Value>> {
    let value = value.unwrap_or(&Value::Null);
    let value = if value.is_null() {
        serde_json::from_str(fallback).map_err(|_| Failure::storage())?
    } else {
        value.clone()
    };
    let entries = match value {
        Value::Object(map) => map
            .into_iter()
            .map(|(name, value)| {
                if value.is_object() {
                    let mut value = value;
                    value["name"] = json!(name);
                    value
                } else {
                    json!({"name":name,"value":value,"secret":false})
                }
            })
            .collect::<Vec<_>>(),
        Value::Array(entries) => entries,
        _ => return Err(error(400, "Field environment must be an object or array.")),
    };
    if entries.len() > 128 {
        return Err(error(400, "Field environment has too many variables."));
    }
    let mut names = BTreeSet::new();
    let mut result = Vec::with_capacity(entries.len());
    for entry in entries {
        let name = entry["name"]
            .as_str()
            .filter(|name| {
                (1..=128).contains(&name.len())
                    && name
                        .bytes()
                        .next()
                        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
                    && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            })
            .ok_or_else(|| error(400, "Environment variable name is invalid."))?
            .to_owned();
        if !names.insert(name.clone()) {
            return Err(error(400, "Environment variable names must be unique."));
        }
        let secret = entry["secret"] == true || entry["sensitive"] == true;
        let raw = entry["value"].as_str().unwrap_or("");
        if raw.len() > 65536 || raw.chars().any(|c| c == '\0') {
            return Err(error(400, "Environment variable value is invalid."));
        }
        result.push(json!({"name":name,"value":raw,"secret":secret,"valueRedacted":entry["valueRedacted"] == true}));
    }
    Ok(result)
}

fn default_name(kind: &str) -> String {
    harness::label(kind).to_owned()
}

fn ensure_defaults(db: &Connection, now: &str) -> Result<()> {
    for kind in KINDS {
        db.execute(
            "INSERT INTO provider_instances(id,kind,display_name,created,updated) VALUES(?,?,?,?,?) ON CONFLICT(id) DO NOTHING",
            params![kind, kind, default_name(kind), now, now],
        )?;
    }
    Ok(())
}

fn secret_environment(
    state: &AppState,
    id: &str,
    values: &[Value],
    previous: Option<&str>,
) -> Result<String> {
    let previous = previous
        .and_then(|raw| serde_json::from_str::<Vec<Value>>(raw).ok())
        .unwrap_or_default();
    let mut stored = Vec::with_capacity(values.len());
    for value in values {
        let secret = value["secret"] == true;
        let raw = value["value"].as_str().unwrap_or("");
        let stored_value = if secret {
            if value["valueRedacted"] == true {
                previous
                    .iter()
                    .find(|old| old["name"] == value["name"] && old["secret"] == true)
                    .and_then(|old| old["value"].as_str())
                    .map(str::to_owned)
                    .ok_or_else(Failure::storage)?
            } else {
                provider_store::encrypt_secret(state, id, raw)?
            }
        } else {
            raw.to_owned()
        };
        stored.push(json!({"name":value["name"],"value":stored_value,"secret":secret}));
    }
    Ok(Value::Array(stored).to_string())
}

fn public_environment(raw: &str) -> Value {
    let Ok(Value::Array(values)) = serde_json::from_str(raw) else {
        return json!([]);
    };
    Value::Array(values.into_iter().map(|value| {
        let secret = value["secret"] == true;
        json!({
            "name":value["name"],
            "value":if secret { Value::String(String::new()) } else { value["value"].clone() },
            "secret":secret,
            "valueRedacted":secret
        })
    }).collect())
}

fn public(state: &AppState, row: &Value) -> Value {
    let mut value = json!({
        "id":row["id"], "instanceId":row["id"], "kind":row["kind"],
        "displayName":row["display_name"], "enabled":row["enabled"] == 1,
        "binaryPath":row["binary_path"], "homePath":row["home_path"],
        "shadowHomePath":row["shadow_home_path"],
        "launchArgs":serde_json::from_str::<Value>(string(row,"launch_args")).unwrap_or_else(|_| json!([])),
        "environment":public_environment(string(row,"environment")),
        "accentColor":row["accent_color"], "revision":row["revision"],
    });
    let executable = if !string(row, "binary_path").is_empty() {
        Some(string(row, "binary_path").to_owned())
    } else {
        harness::executable(state, string(row, "kind"))
            .map(|path| path.to_string_lossy().into_owned())
    };
    value["installed"] = json!(executable.is_some());
    value["version"] = Value::Null;
    value["status"] = json!(if row["enabled"] == 1 {
        "unknown"
    } else {
        "disabled"
    });
    value["auth"] = json!({"status":"unknown"});
    value["models"] = json!([]);
    value
}

fn row(db: &Connection, id: &str) -> Result<Option<Value>> {
    one(db, &format!("{SELECT} WHERE id=?"), [id])
}

pub(crate) fn instance(state: &AppState, id: &str) -> Result<Option<Value>> {
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    row(&guard.connection, id)
}

pub(crate) fn launch_config(state: &AppState, id: &str) -> Result<Option<LaunchConfig>> {
    let Some(row) = instance(state, id)? else {
        return Ok(None);
    };
    let kind = string(&row, "kind").to_owned();
    let binary = (!string(&row, "binary_path").is_empty())
        .then(|| PathBuf::from(string(&row, "binary_path")));
    let args = serde_json::from_str::<Vec<String>>(string(&row, "launch_args"))
        .map_err(|_| Failure::storage())?;
    let mut environment = HashMap::new();
    let values: Vec<Value> =
        serde_json::from_str(string(&row, "environment")).map_err(|_| Failure::storage())?;
    for value in values {
        let name = string(&value, "name").to_owned();
        let raw = string(&value, "value");
        let resolved = if value["secret"] == true {
            provider_store::decrypt_secret(state, id, raw).ok_or_else(Failure::storage)?
        } else {
            raw.to_owned()
        };
        environment.insert(name, resolved);
    }
    if !string(&row, "home_path").is_empty() {
        let home = string(&row, "home_path").to_owned();
        if kind == "codex" {
            environment.insert("CODEX_HOME".into(), home.clone());
        } else if kind == "claude" {
            environment.insert("CLAUDE_CONFIG_DIR".into(), home.clone());
        }
    }
    if kind == "codex" && !string(&row, "shadow_home_path").is_empty() {
        environment.insert(
            "CODEX_HOME".into(),
            string(&row, "shadow_home_path").to_owned(),
        );
    }
    Ok(Some((kind, binary, args, environment)))
}

pub(crate) fn migrate_legacy(db: &Connection) -> crate::db::Result<()> {
    let table = |name: &str| -> rusqlite::Result<bool> {
        Ok(db
            .query_row(
                "SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?",
                [name],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    };
    if table("engine_logins")? {
        db.execute(
            "INSERT OR IGNORE INTO provider_instances(id,kind,display_name,created,updated) SELECT runtime||'-'||id,runtime,name,created,created FROM engine_logins",
            [],
        )?;
        db.execute(
            "UPDATE agents SET provider_instance=CASE WHEN provider_account='device' AND runtime IN('codex','claude') THEN runtime WHEN provider_account LIKE 'login:%' THEN runtime||'-'||substr(provider_account,7) WHEN provider_account<>'' AND runtime='codex' THEN 'codex-api-'||provider_account WHEN provider_account<>'' AND runtime='claude' THEN 'claude-api-'||provider_account ELSE provider_instance END WHERE provider_instance=''",
            [],
        )?;
    }
    if table("provider_accounts")? {
        db.execute(
            "INSERT OR IGNORE INTO provider_instances(id,kind,display_name,created,updated,environment) SELECT CASE WHEN family='anthropic' THEN 'claude-api-' ELSE 'codex-api-' END||id,CASE WHEN family='anthropic' THEN 'claude' ELSE 'codex' END,name,'','',json_array(json_object('name',CASE WHEN family='anthropic' THEN 'ANTHROPIC_API_KEY' ELSE 'MINARBOT_PROVIDER_KEY' END,'value',key_ciphertext,'secret',1,'legacyAccountId',id)) FROM provider_accounts",
            [],
        )?;
    }
    Ok(())
}

fn body_instance_id(body: &Value) -> Result<String> {
    let id = body
        .get("instanceId")
        .or_else(|| body.get("providerInstance"))
        .and_then(Value::as_str)
        .ok_or_else(|| error(400, "Field instanceId is required."))?;
    if !valid_slug(id) {
        return Err(error(400, "Field instanceId is invalid."));
    }
    Ok(id.to_owned())
}

fn save(state: &AppState, body: &Value, id: &str, previous: Option<Value>) -> Result<Response> {
    let kind = parse_text(
        body,
        "kind",
        64,
        previous.as_ref().map_or("", |v| string(v, "kind")),
    )?;
    if !KINDS.contains(&kind.as_str()) && kind != "acp" {
        return Err(error(400, "Field kind is not supported."));
    }
    let default_display_name = previous.as_ref().map_or_else(
        || default_name(&kind),
        |v| string(v, "display_name").to_owned(),
    );
    let name = parse_text(body, "displayName", 80, &default_display_name)?;
    if name.is_empty() {
        return Err(error(400, "Field displayName is required."));
    }
    let enabled = body
        .get("enabled")
        .map(|v| {
            v.as_bool()
                .ok_or_else(|| error(400, "Field enabled is invalid."))
        })
        .transpose()?
        .unwrap_or_else(|| previous.as_ref().is_none_or(|v| v["enabled"] == 1));
    let binary = parse_text(
        body,
        "binaryPath",
        4096,
        previous.as_ref().map_or("", |v| string(v, "binary_path")),
    )?;
    let home = parse_text(
        body,
        "homePath",
        4096,
        previous.as_ref().map_or("", |v| string(v, "home_path")),
    )?;
    let shadow = parse_text(
        body,
        "shadowHomePath",
        4096,
        previous
            .as_ref()
            .map_or("", |v| string(v, "shadow_home_path")),
    )?;
    let accent = parse_text(
        body,
        "accentColor",
        32,
        previous.as_ref().map_or("", |v| string(v, "accent_color")),
    )?;
    let args = parse_args(
        body.get("launchArgs"),
        previous.as_ref().map_or("[]", |v| string(v, "launch_args")),
    )?;
    let env = parse_environment(
        body.get("environment"),
        previous.as_ref().map_or("[]", |v| string(v, "environment")),
    )?;
    let env = secret_environment(
        state,
        id,
        &env,
        previous.as_ref().map(|row| string(row, "environment")),
    )?;
    let now = state.config.now().to_string();
    let revision = previous.as_ref().and_then(|v| v["revision"].as_i64());
    if revision.is_some() && body["revision"].as_i64() != revision {
        return Err(error(
            409,
            "Provider instance changed. Reopen it before saving.",
        ));
    }
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    let changed = if let Some(revision) = revision {
        db.execute(
            "UPDATE provider_instances SET kind=?,display_name=?,enabled=?,binary_path=?,home_path=?,shadow_home_path=?,launch_args=?,environment=?,accent_color=?,revision=revision+1,updated=? WHERE id=? AND revision=?",
            params![kind,name,enabled as i64,binary,home,shadow,args.to_string(),env,accent,now,id,revision],
        )?
    } else {
        db.execute(
            "INSERT INTO provider_instances(id,kind,display_name,enabled,binary_path,home_path,shadow_home_path,launch_args,environment,accent_color,created,updated) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
            params![id,kind,name,enabled as i64,binary,home,shadow,args.to_string(),env,accent,now,now],
        )?
    };
    if changed == 0 {
        return Err(error(
            409,
            "Provider instance changed. Reopen it before saving.",
        ));
    }
    let saved = row(db, id)?.ok_or_else(Failure::storage)?;
    Ok(json_response(
        if revision.is_some() { 200 } else { 201 },
        public(state, &saved),
    ))
}

fn remove(state: &AppState, id: &str) -> Result<Response> {
    if KINDS.contains(&id) {
        return Err(error(409, "Default provider instances cannot be deleted."));
    }
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    if db.execute("DELETE FROM provider_instances WHERE id=?", [id])? == 0 {
        return Err(error(404, "Provider instance not found."));
    }
    Ok(json_response(200, json!({"deleted":true})))
}

pub fn list(state: &AppState) -> Result<Value> {
    let now = state.config.now().to_string();
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    ensure_defaults(&guard.connection, &now)?;
    Ok(
        json!({"providers":all(&guard.connection, &format!("{SELECT} ORDER BY rowid"), [])?.iter().map(|row| public(state, row)).collect::<Vec<_>>() }),
    )
}

pub async fn route(
    state: &AppState,
    parts: &[&str],
    method: &Method,
    query: &[(String, String)],
    body: &Value,
) -> Result<Response> {
    if !query.is_empty() {
        return Err(error(400, "Invalid provider instance parameters."));
    }
    if *method == Method::GET && parts.len() == 2 {
        return Ok(json_response(200, list(state)?));
    }
    if *method != Method::POST {
        return Err(error(405, "Use GET or POST for provider instances."));
    }
    let id = body_instance_id(body)?;
    let previous = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        row(&guard.connection, &id)?
    };
    match parts {
        ["api", "provider-instances"] if previous.is_none() => save(state, body, &id, None),
        ["api", "provider-instances", instance, action] if *instance == id => match *action {
            "edit" | "update" => save(
                state,
                body,
                &id,
                Some(previous.ok_or_else(|| error(404, "Provider instance not found."))?),
            ),
            "delete" if closed(body, &["id", "instanceId", "revision"]) => remove(state, &id),
            "refresh" | "sign-in" | "signin" | "models" => {
                if *action == "models" {
                    return crate::harness::model_catalog::models_for_instance(state, &id, true)
                        .await
                        .map(|catalog| json_response(200, catalog));
                }
                let mut result = list(state)?;
                if *action == "refresh" {
                    let catalog =
                        crate::harness::model_catalog::models_for_instance(state, &id, true)
                            .await
                            .unwrap_or_else(|_| json!({"models":[],"catalog":[]}));
                    if let Some(provider) =
                        result["providers"].as_array_mut().and_then(|providers| {
                            providers.iter_mut().find(|provider| provider["id"] == id)
                        })
                    {
                        provider["models"] = catalog["catalog"].clone();
                        provider["modelIds"] = catalog["models"].clone();
                    }
                }
                Ok(json_response(200, result))
            }
            _ => Err(error(404, "Route not found.")),
        },
        ["api", "provider-instances"] => save(
            state,
            body,
            &id,
            Some(previous.ok_or_else(|| error(404, "Provider instance not found."))?),
        ),
        _ => Err(error(404, "Route not found.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_environment_is_redacted() -> Result<()> {
        let values = parse_environment(
            Some(&json!({"TOKEN":{"value":"secret","secret":true},"MODE":"fast"})),
            "[]",
        )?;
        assert_eq!(values.len(), 2);
        assert!(values.iter().any(|value| value["secret"] == true));
        Ok(())
    }

    #[test]
    fn slugs_reject_paths() {
        assert!(valid_slug("codex"));
        assert!(!valid_slug("../codex"));
        assert!(!valid_slug(""));
    }
}
