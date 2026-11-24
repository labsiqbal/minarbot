use super::{all, closed, error, one, string, Result};
use crate::{data::provider_store, harness, json_response, web::Failure, AppState};
use axum::{http::Method, response::Response};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::process::Stdio;
use std::{
    collections::BTreeSet,
    collections::HashMap,
    fs::{self, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
};
use tokio::process::Command;

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

fn auth_status(_state: &AppState, row: &Value) -> Value {
    let kind = string(row, "kind");
    if !matches!(kind, "codex" | "claude") {
        return json!({"status":"unknown"});
    }
    let home = if !string(row, "home_path").is_empty() {
        Some(PathBuf::from(string(row, "home_path")))
    } else {
        crate::data::logins::device_folder(kind)
    };
    let Some(home) = home else {
        return json!({"status":"unauthenticated","reason":"No managed home is configured."});
    };
    let credential = home.join(if kind == "codex" {
        "auth.json"
    } else {
        ".credentials.json"
    });
    match fs::metadata(&credential) {
        Ok(metadata) if metadata.is_file() && metadata.len() > 0 => json!({
            "status":"authenticated",
            "method":if kind == "codex" { "ChatGPT" } else { "Claude" }
        }),
        Ok(_) => json!({"status":"unauthenticated","reason":"Credential file is empty."}),
        Err(_) => {
            json!({"status":"unauthenticated","reason":"No sign-in found in this instance home."})
        }
    }
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
    value["status"] = json!(if row["enabled"] != 1 {
        "disabled"
    } else if executable.is_some() {
        "installed"
    } else {
        "not_installed"
    });
    value["auth"] = auth_status(state, row);
    value["models"] = json!([]);
    value
}

fn row(db: &Connection, id: &str) -> Result<Option<Value>> {
    one(db, &format!("{SELECT} WHERE id=?"), [id])
}

pub(crate) fn instance(state: &AppState, id: &str) -> Result<Option<Value>> {
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    instance_from_db(&guard.connection, id)
}

pub(crate) fn instance_from_db(db: &Connection, id: &str) -> Result<Option<Value>> {
    row(db, id)
}

pub(crate) fn launch_config(state: &AppState, id: &str) -> Result<Option<LaunchConfig>> {
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    launch_config_from_db(state, &guard.connection, id)
}

pub(crate) fn launch_config_from_db(
    state: &AppState,
    db: &Connection,
    id: &str,
) -> Result<Option<LaunchConfig>> {
    let Some(row) = row(db, id)? else {
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
    let database = db
        .query_row("PRAGMA database_list", [], |row| row.get::<_, String>(2))
        .map(PathBuf::from)
        .map_err(|_| "Cannot read database path")?;
    if database.as_os_str().is_empty() {
        return Ok(());
    }
    let data_root = database
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or("Cannot resolve database directory")?;
    let has_runtime = all(db, "PRAGMA table_info(agents)", [])
        .map_err(|_| "Cannot read agent columns")?
        .iter()
        .any(|column| column["name"] == "runtime");
    if table("engine_logins")? {
        let logins = all(
            db,
            "SELECT id,runtime,name,created FROM engine_logins WHERE runtime IN('codex','claude')",
            [],
        )
        .map_err(|_| "Cannot read legacy engine logins")?;
        for login in logins {
            let id = string(&login, "id");
            let runtime = string(&login, "runtime");
            let instance_id = format!("{runtime}-{id}");
            let home = data_root.join("engine-logins").join(runtime).join(id);
            db.execute(
                "INSERT OR IGNORE INTO provider_instances(id,kind,display_name,home_path,created,updated) VALUES(?,?,?,?,?,?)",
                params![instance_id, runtime, string(&login, "name"), home.to_string_lossy(), string(&login, "created"), string(&login, "created")],
            )?;
            // Older partial migrations created this row without its managed
            // home. Repair it in place so the login account is never lost.
            db.execute(
                "UPDATE provider_instances SET home_path=? WHERE id=? AND home_path=''",
                params![home.to_string_lossy(), instance_id],
            )?;
        }
    }
    if table("provider_accounts")? {
        let accounts = all(
            db,
            "SELECT id,name,family,base_url,key_ciphertext,default_model FROM provider_accounts WHERE family IN('openai-compatible','anthropic')",
            [],
        )
        .map_err(|_| "Cannot read legacy provider accounts")?;
        let managed_root = data_root.join("provider-instances");
        fs::create_dir_all(&managed_root)?;
        fs::set_permissions(&managed_root, fs::Permissions::from_mode(0o700))?;
        for account in accounts {
            let account_id = string(&account, "id");
            let family = string(&account, "family");
            let runtime = if family == "anthropic" {
                "claude"
            } else {
                "codex"
            };
            let instance_id = format!("{runtime}-api-{account_id}");
            if !valid_slug(&instance_id) {
                continue;
            }
            let home = managed_root.join(&instance_id);
            fs::create_dir_all(&home)?;
            fs::set_permissions(&home, fs::Permissions::from_mode(0o700))?;
            let (environment, config) = if let Some(secret) =
                provider_store::reencrypt_legacy_secret(
                    &database,
                    account_id,
                    &instance_id,
                    string(&account, "key_ciphertext"),
                ) {
                if runtime == "codex" {
                    let key_name = "MINARBOT_CODEX_PROVIDER_KEY";
                    let config = format!(
                        "model = {}\nmodel_provider = \"minarbot\"\n[model_providers.minarbot]\nname = \"Minarbot provider\"\nbase_url = {}\nwire_api = \"responses\"\nenv_key = \"{key_name}\"\nrequires_openai_auth = false\nsupports_websockets = false\n",
                        json!(string(&account, "default_model")),
                        json!(string(&account, "base_url").trim_end_matches('/')),
                    );
                    (
                        json!([{"name":key_name,"value":secret,"secret":true}]),
                        Some(config),
                    )
                } else {
                    let base = string(&account, "base_url")
                        .trim_end_matches('/')
                        .trim_end_matches("/v1");
                    (
                        json!([
                            {"name":"ANTHROPIC_BASE_URL","value":base,"secret":false},
                            {"name":"ANTHROPIC_API_KEY","value":secret,"secret":true},
                        ]),
                        None,
                    )
                }
            } else {
                (json!([]), None)
            };
            db.execute(
                "INSERT OR IGNORE INTO provider_instances(id,kind,display_name,home_path,environment,created,updated) VALUES(?,?,?,?,?,?,?)",
                params![instance_id, runtime, string(&account, "name"), home.to_string_lossy(), environment.to_string(), "", ""],
            )?;
            if environment != json!([]) {
                db.execute(
                    "UPDATE provider_instances SET home_path=?,environment=? WHERE id=? AND (home_path='' OR environment='[]' OR environment LIKE '%legacyAccountId%')",
                    params![home.to_string_lossy(), environment.to_string(), instance_id],
                )?;
            } else {
                db.execute(
                    "UPDATE provider_instances SET home_path=? WHERE id=? AND home_path=''",
                    params![home.to_string_lossy(), instance_id],
                )?;
            }
            if let Some(config) = config {
                let path = home.join("config.toml");
                if !path.exists() {
                    let mut file = OpenOptions::new()
                        .create(true)
                        .truncate(true)
                        .write(true)
                        .mode(0o600)
                        .open(path)?;
                    std::io::Write::write_all(&mut file, config.as_bytes())?;
                }
            }
        }
        if has_runtime {
            db.execute(
                "UPDATE agents SET provider_instance=CASE WHEN provider_account<>'' AND EXISTS(SELECT 1 FROM provider_accounts p WHERE p.id=agents.provider_account AND p.family IN('openai-compatible','anthropic')) THEN CASE WHEN (SELECT family FROM provider_accounts p WHERE p.id=agents.provider_account)='anthropic' THEN 'claude-api-' ELSE 'codex-api-' END||provider_account ELSE provider_instance END, runtime=CASE WHEN provider_account<>'' AND EXISTS(SELECT 1 FROM provider_accounts p WHERE p.id=agents.provider_account AND p.family IN('openai-compatible','anthropic')) THEN CASE WHEN (SELECT family FROM provider_accounts p WHERE p.id=agents.provider_account)='anthropic' THEN 'claude' ELSE 'codex' END ELSE runtime END, model=CASE WHEN model='' AND provider_account<>'' THEN coalesce((SELECT default_model FROM provider_accounts p WHERE p.id=agents.provider_account),'') ELSE model END WHERE provider_instance=''",
                [],
            )?;
            // Gemini API accounts have no CLI instance in this wave. Keep the
            // agent profile and model, but make it visibly unconfigured.
            db.execute(
                "UPDATE agents SET runtime='codex',provider_instance='' WHERE runtime='api' AND provider_account IN(SELECT id FROM provider_accounts WHERE family='gemini')",
                [],
            )?;
        } else {
            db.execute(
                "UPDATE agents SET provider_instance=CASE WHEN provider_account<>'' AND EXISTS(SELECT 1 FROM provider_accounts p WHERE p.id=agents.provider_account AND p.family IN('openai-compatible','anthropic')) THEN CASE WHEN (SELECT family FROM provider_accounts p WHERE p.id=agents.provider_account)='anthropic' THEN 'claude-api-' ELSE 'codex-api-' END||provider_account ELSE provider_instance END, model=CASE WHEN model='' AND provider_account<>'' THEN coalesce((SELECT default_model FROM provider_accounts p WHERE p.id=agents.provider_account),'') ELSE model END WHERE provider_instance=''",
                [],
            )?;
        }
    }
    if table("engine_logins")? && has_runtime {
        db.execute(
            "UPDATE agents SET provider_instance=CASE WHEN provider_account='device' AND runtime IN('codex','claude') THEN runtime WHEN provider_account LIKE 'login:%' AND runtime IN('codex','claude') THEN runtime||'-'||substr(provider_account,7) ELSE provider_instance END WHERE provider_instance=''",
            [],
        )?;
    }
    // Legacy bindings have been copied into provider instances. Remove the
    // old storage so new callers cannot accidentally depend on it. SQLite
    // preserves all remaining agent columns and values while dropping each
    // obsolete column, and rerunning this block is harmless.
    for column in ["runtime", "provider_account", "backup_model"] {
        let present = all(db, "PRAGMA table_info(agents)", [])
            .map_err(|error| error.message)?
            .iter()
            .any(|row| row["name"] == column);
        if present {
            db.execute(&format!("ALTER TABLE agents DROP COLUMN {column}"), [])?;
        }
    }
    if table("provider_accounts")? {
        db.execute_batch("DROP TABLE provider_accounts")?;
    }
    if table("engine_logins")? {
        db.execute_batch("DROP TABLE engine_logins")?;
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

async fn sign_in(state: &AppState, id: &str) -> Result<Response> {
    let row = instance(state, id)?.ok_or_else(|| error(404, "Provider instance not found."))?;
    let kind = string(&row, "kind");
    if !matches!(kind, "codex" | "claude") {
        return Err(error(400, "This provider does not support sign-in."));
    }
    let configured = launch_config(state, id)?;
    let executable = configured
        .as_ref()
        .and_then(|(_, binary, _, _)| binary.clone())
        .or_else(|| harness::executable(state, kind))
        .ok_or_else(|| error(503, "This provider is not installed on this host."))?;
    let mut environment = state.config.harness.environment.clone();
    let current_home = std::env::var("HOME").unwrap_or_default();
    if !current_home.is_empty() {
        environment.insert("HOME".into(), current_home);
    }
    if let Some((_, _, _, configured)) = configured {
        environment.extend(configured);
    }
    if string(&row, "home_path").is_empty() {
        if let Some((_, device_environment)) =
            crate::data::logins::device_process_environment(kind, harness::path(state))
        {
            environment.extend(device_environment);
        }
    }
    let working_directory = if !string(&row, "home_path").is_empty() {
        PathBuf::from(string(&row, "home_path"))
    } else {
        crate::data::logins::device_process_environment(kind, harness::path(state))
            .map(|(directory, _)| directory)
            .unwrap_or_else(|| PathBuf::from("."))
    };
    environment.insert("PATH".into(), harness::path(state));
    let args: &[&str] = if kind == "codex" {
        &["login"]
    } else {
        &["auth", "login"]
    };
    let child = Command::new(&executable)
        .args(args)
        .env_clear()
        .envs(&environment)
        .current_dir(working_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false)
        .spawn()
        .map_err(|_| error(502, "Provider sign-in could not start."))?;
    let pid = child.id();
    drop(child);
    Ok(json_response(
        202,
        json!({"started":true,"pid":pid,"provider":id}),
    ))
}

pub fn list(state: &AppState) -> Result<Value> {
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
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
                if *action == "sign-in" || *action == "signin" {
                    return sign_in(state, &id).await;
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

    #[test]
    fn legacy_login_migration_keeps_managed_home() -> crate::db::Result<()> {
        let directory = std::env::temp_dir().join(format!(
            "minarbot-provider-migration-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(directory.join("engine-logins/codex/login"))?;
        let database = directory.join("state.sqlite");
        let db = Connection::open(&database)?;
        db.execute_batch(
            "CREATE TABLE provider_instances(id TEXT PRIMARY KEY,kind TEXT NOT NULL,display_name TEXT NOT NULL,enabled INTEGER NOT NULL DEFAULT 1,binary_path TEXT NOT NULL DEFAULT '',home_path TEXT NOT NULL DEFAULT '',shadow_home_path TEXT NOT NULL DEFAULT '',launch_args TEXT NOT NULL DEFAULT '[]',environment TEXT NOT NULL DEFAULT '[]',accent_color TEXT NOT NULL DEFAULT '',revision INTEGER NOT NULL DEFAULT 1,created TEXT NOT NULL DEFAULT '',updated TEXT NOT NULL DEFAULT '');
             CREATE TABLE agents(id TEXT PRIMARY KEY,provider_instance TEXT NOT NULL DEFAULT '',provider_account TEXT NOT NULL DEFAULT '',runtime TEXT NOT NULL DEFAULT 'api');
             CREATE TABLE engine_logins(id TEXT PRIMARY KEY,runtime TEXT NOT NULL,name TEXT NOT NULL,created TEXT NOT NULL);",
        )?;
        db.execute(
            "INSERT INTO engine_logins VALUES('login','codex','Personal','2026-01-01')",
            [],
        )?;
        migrate_legacy(&db)?;
        let home: String = db.query_row(
            "SELECT home_path FROM provider_instances WHERE id='codex-login'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            home,
            directory
                .join("engine-logins/codex/login")
                .to_string_lossy()
        );
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn migrates_018_legacy_bindings_and_drops_legacy_storage() -> crate::db::Result<()> {
        let directory = std::env::temp_dir().join(format!(
            "minarbot-provider-migration-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&directory)?;
        let database = directory.join("state.sqlite");
        let db = Connection::open(&database)?;
        db.execute_batch(
            "CREATE TABLE provider_instances(id TEXT PRIMARY KEY,kind TEXT NOT NULL,display_name TEXT NOT NULL,enabled INTEGER NOT NULL DEFAULT 1,binary_path TEXT NOT NULL DEFAULT '',home_path TEXT NOT NULL DEFAULT '',shadow_home_path TEXT NOT NULL DEFAULT '',launch_args TEXT NOT NULL DEFAULT '[]',environment TEXT NOT NULL DEFAULT '[]',accent_color TEXT NOT NULL DEFAULT '',revision INTEGER NOT NULL DEFAULT 1,created TEXT NOT NULL DEFAULT '',updated TEXT NOT NULL DEFAULT '');
             CREATE TABLE agents(id TEXT PRIMARY KEY,name TEXT NOT NULL,model TEXT NOT NULL,instructions TEXT NOT NULL,provider_instance TEXT NOT NULL DEFAULT '',model_options TEXT NOT NULL DEFAULT '{}',provider_account TEXT NOT NULL DEFAULT '',runtime TEXT NOT NULL DEFAULT 'api',backup_model TEXT NOT NULL DEFAULT '');
             CREATE TABLE provider_accounts(id TEXT PRIMARY KEY,name TEXT NOT NULL,family TEXT NOT NULL,base_url TEXT NOT NULL,key_ciphertext TEXT NOT NULL,default_model TEXT NOT NULL,revision INTEGER NOT NULL DEFAULT 1);
             CREATE TABLE engine_logins(id TEXT PRIMARY KEY,runtime TEXT NOT NULL,name TEXT NOT NULL,created TEXT NOT NULL);
             CREATE TABLE turns(id TEXT PRIMARY KEY,agent TEXT NOT NULL,text TEXT NOT NULL);
             CREATE TABLE threads(id TEXT PRIMARY KEY,agent TEXT NOT NULL,name TEXT NOT NULL);",
        )?;
        let codex_secret =
            provider_store::encrypt_legacy_secret(&database, "openai", "secret-codex")?;
        let claude_secret =
            provider_store::encrypt_legacy_secret(&database, "anthropic", "secret-claude")?;
        db.execute(
            "INSERT INTO provider_accounts VALUES('openai','OpenAI','openai-compatible','https://provider.example',?,'gpt-5',1),('anthropic','Claude','anthropic','https://anthropic.example',?,'claude-sonnet',1),('gemini','Gemini','gemini','https://gemini.example','ciphertext','gemini-2',1)",
            params![codex_secret, claude_secret],
        )?;
        db.execute(
            "INSERT INTO engine_logins VALUES('login','codex','Personal','2026-01-01')",
            [],
        )?;
        db.execute(
            "INSERT INTO agents VALUES
                ('api-codex','API Codex','','','', '{}','openai','api',''),
                ('device-codex','Device Codex','','','', '{}','login:login','codex',''),
                ('api-gemini','API Gemini','','','', '{}','gemini','api','')",
            [],
        )?;
        db.execute(
            "INSERT INTO threads VALUES('thread','api-codex','History')",
            [],
        )?;
        db.execute(
            "INSERT INTO turns VALUES('turn','api-codex','Conversation')",
            [],
        )?;

        migrate_legacy(&db)?;

        let instance: String = db.query_row(
            "SELECT provider_instance FROM agents WHERE id='api-codex'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(instance, "codex-api-openai");
        let login_instance: String = db.query_row(
            "SELECT provider_instance FROM agents WHERE id='device-codex'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(login_instance, "codex-login");
        let gemini_instance: String = db.query_row(
            "SELECT provider_instance FROM agents WHERE id='api-gemini'",
            [],
            |row| row.get(0),
        )?;
        assert!(gemini_instance.is_empty());
        let environment: String = db.query_row(
            "SELECT environment FROM provider_instances WHERE id='codex-api-openai'",
            [],
            |row| row.get(0),
        )?;
        assert!(!environment.contains("secret-codex"));
        assert!(!environment.contains("legacyAccountId"));
        assert!(db
            .query_row(
                "SELECT 1 FROM sqlite_schema WHERE type='table' AND name IN('provider_accounts','engine_logins')",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_none());
        let columns = all(&db, "PRAGMA table_info(agents)", []).map_err(|e| e.message)?;
        for name in ["runtime", "provider_account", "backup_model"] {
            assert!(!columns.iter().any(|column| column["name"] == name));
        }
        assert_eq!(
            db.query_row("SELECT text FROM turns WHERE id='turn'", [], |row| row
                .get::<_, String>(
                0
            ))?,
            "Conversation"
        );
        fs::remove_dir_all(directory)?;
        Ok(())
    }
}
