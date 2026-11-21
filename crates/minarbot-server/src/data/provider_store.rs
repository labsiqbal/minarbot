use super::{all, closed, error, one, providers, string, transaction, Result};
use crate::{json_response, operator_file, web::Failure, AppState};
use axum::{http::Method, response::Response};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use providers::Account;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};
use std::{ffi::OsStr, io::ErrorKind, path::Path};

const SELECT: &str =
    "SELECT id,name,family,base_url,key_ciphertext,default_model,revision FROM provider_accounts";

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    getrandom::getrandom(&mut bytes).map_err(|_| Failure::storage())?;
    Ok(bytes)
}

fn wrapping_key(database: &Path, create: bool) -> Result<LessSafeKey> {
    let parent = database
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .map_err(|_| Failure::storage())?;
    let directory = operator_file::open_directory(&parent).map_err(|_| Failure::storage())?;
    let key_path = minarbot_launcher::files::sidecar(database, ".provider-key");
    let name = key_path.file_name().ok_or_else(Failure::storage)?;
    let open = |name: &OsStr| operator_file::file_at(&directory, name);
    let file = match open(name) {
        Err(error) if create && error.kind() == ErrorKind::NotFound => {
            operator_file::publish(&directory, name, &random::<32>()?)
                .map_err(|_| Failure::storage())?;
            open(name)
        }
        result => result,
    }
    .map_err(|_| Failure::storage())?;
    if !operator_file::private(&file) {
        return Err(Failure::storage());
    }
    let bytes: [u8; 32] = operator_file::bounded_read(file, 32)
        .map_err(|_| Failure::storage())?
        .try_into()
        .map_err(|_| Failure::storage())?;
    let key = UnboundKey::new(&AES_256_GCM, &bytes).map_err(|_| Failure::storage())?;
    Ok(LessSafeKey::new(key))
}

fn encrypt(state: &AppState, account: &Account) -> Result<String> {
    let key = wrapping_key(&state.config.database, true)?;
    let nonce = random::<12>()?;
    let mut bytes = account
        .key
        .as_ref()
        .ok_or_else(Failure::storage)?
        .as_bytes()
        .to_vec();
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::from(account.id.as_bytes()),
        &mut bytes,
    )
    .map_err(|_| Failure::storage())?;
    Ok(B64.encode([nonce.as_slice(), &bytes].concat()))
}

fn decrypt(key: &LessSafeKey, row: &Value) -> Option<String> {
    let mut bytes = B64.decode(string(row, "key_ciphertext")).ok()?;
    let nonce = bytes.get(..12)?.try_into().ok()?;
    let plain = key
        .open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(string(row, "id").as_bytes()),
            &mut bytes[12..],
        )
        .ok()?;
    String::from_utf8(plain.to_vec()).ok()
}

fn read_connection(database: &Path) -> Result<Connection> {
    // Callers include active transactions on the main connection.
    Ok(Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?)
}

fn account(options: &providers::Options, row: &Value, key: Option<String>) -> Account {
    let family = string(row, "family").to_owned();
    Account {
        id: string(row, "id").into(),
        name: string(row, "name").into(),
        base: string(row, "base_url").into(),
        model: string(row, "default_model").into(),
        key,
        termination: if family == "openai-compatible" {
            options
                .stream_termination
                .as_deref()
                .unwrap_or("marker")
                .into()
        } else {
            "marker".into()
        },
        stream: options.stream.unwrap_or(false),
        max_tokens: options
            .max_tokens
            .as_ref()
            .and_then(Value::as_u64)
            .unwrap_or(4096),
        family,
        source: "settings",
    }
}

fn rows(db: &Connection) -> Result<Vec<Value>> {
    all(db, &format!("{SELECT} ORDER BY rowid"), [])
}

pub(super) fn stored(database: &Path, options: &providers::Options) -> Result<Vec<Account>> {
    let connection = read_connection(database)?;
    if !super::table(&connection, "provider_accounts")? {
        return Ok(Vec::new());
    }
    let rows = rows(&connection)?;
    let wrapping_key = (!rows.is_empty())
        .then(|| wrapping_key(database, false).ok())
        .flatten();
    Ok(rows
        .iter()
        .filter_map(|row| {
            let key = decrypt(wrapping_key.as_ref()?, row)?;
            Some(account(options, row, Some(key)))
        })
        .collect())
}

pub(super) fn list(state: &AppState) -> Result<Vec<Value>> {
    let rows = rows(&read_connection(&state.config.database)?)?;
    let wrapping_key = (!rows.is_empty())
        .then(|| wrapping_key(&state.config.database, false).ok())
        .flatten();
    Ok(rows
        .iter()
        .map(|row| {
            let key = wrapping_key.as_ref().and_then(|key| decrypt(key, row));
            let configured = key.is_some();
            let mut result = providers::public(&account(&state.config.data, row, key));
            result["configured"] = json!(configured);
            result["hasKey"] = json!(true);
            result["revision"] = row["revision"].clone();
            result
        })
        .collect())
}

fn read(state: &AppState, id: &str) -> Result<Option<Value>> {
    one(
        &read_connection(&state.config.database)?,
        &format!("{SELECT} WHERE id=?"),
        [id],
    )
}

fn field(body: &Value, name: &str, max: usize) -> Result<Option<String>> {
    body.get(name)
        .map(|value| {
            value
                .as_str()
                .filter(|value| {
                    value.encode_utf16().count() <= max && !value.chars().any(char::is_control)
                })
                .map(|value| value.trim().to_owned())
                .ok_or_else(|| error(400, format!("Field {name} is invalid.")))
        })
        .transpose()
}

fn candidate(
    state: &AppState,
    body: &Value,
    id: &str,
    previous: Option<&Value>,
) -> Result<Account> {
    if !closed(
        body,
        &[
            "id",
            "provider",
            "name",
            "baseURL",
            "apiKey",
            "defaultModel",
            "revision",
        ],
    ) {
        return Err(error(400, "Unknown provider account field."));
    }
    let value = |name, column, max| -> Result<String> {
        Ok(field(body, name, max)?
            .unwrap_or_else(|| previous.map_or("", |row| string(row, column)).to_owned()))
    };
    let family = value("provider", "family", 32)?;
    if !providers::supported_family(&family) {
        return Err(error(
            400,
            "Field provider must be openai-compatible, anthropic or gemini.",
        ));
    }
    let name = value("name", "name", 80)?;
    if name.is_empty() {
        return Err(error(400, "Field name is required."));
    }
    let base = value("baseURL", "base_url", 2048)?
        .trim_end_matches('/')
        .to_owned();
    let address = providers::address(&base).map_err(|_| {
        error(
            400,
            "Field baseURL must use HTTPS or loopback HTTP without credentials, query or fragment.",
        )
    })?;
    let model = value("defaultModel", "default_model", 160)?;
    let key = match field(body, "apiKey", 8192)?.filter(|key| !key.is_empty()) {
        Some(key) => key,
        None => {
            let previous = previous.ok_or_else(|| error(400, "Field apiKey is required."))?;
            let same_origin = providers::address(string(previous, "base_url"))
                .is_ok_and(|old| old.origin() == address.origin());
            if !same_origin || string(previous, "family") != family {
                return Err(error(
                    400,
                    "Field apiKey is required when provider type or origin changes.",
                ));
            }
            wrapping_key(&state.config.database, false)
                .ok()
                .and_then(|key| decrypt(&key, previous))
                .ok_or_else(|| error(409, "Stored API key is unavailable. Enter an API key."))?
        }
    };
    if axum::http::HeaderValue::from_str(&key).is_err() {
        return Err(error(400, "Field apiKey is invalid."));
    }
    Ok(account(
        &state.config.data,
        &json!({
            "id":id,"name":name,"family":family,"base_url":base,"default_model":model
        }),
        Some(key),
    ))
}

async fn save(
    state: &AppState,
    body: &Value,
    id: &str,
    previous: Option<Value>,
) -> Result<Response> {
    let revision = previous.as_ref().and_then(|row| row["revision"].as_i64());
    if previous.is_some() && !super::integer(&body["revision"], 1, 9_007_199_254_740_991) {
        return Err(error(400, "Field revision is invalid."));
    }
    let changed = || error(409, "Provider account changed. Reopen it before saving.");
    if revision.is_some() && body["revision"].as_i64() != revision {
        return Err(changed());
    }
    let account = candidate(state, body, id, previous.as_ref())?;
    if account.model.is_empty() {
        return Err(error(400, "Field defaultModel is required."));
    }
    // ponytail: verify on each save. Add signed test receipts if provider rate limits require it.
    let models = providers::test_models(state, &account).await?;
    if !models.contains(&account.model) {
        return Err(error(
            400,
            "Field defaultModel must be a model returned by Test.",
        ));
    }
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    let ciphertext = encrypt(state, &account)?;
    let saved = if let Some(revision) = revision {
        db.execute(
            "UPDATE provider_accounts SET name=?,family=?,base_url=?,key_ciphertext=?,default_model=?,revision=revision+1 WHERE id=? AND revision=?",
            params![account.name, account.family, account.base, ciphertext, account.model, id, revision],
        )?
    } else {
        db.execute(
            "INSERT INTO provider_accounts(id,name,family,base_url,key_ciphertext,default_model) VALUES(?,?,?,?,?,?) ON CONFLICT(id) DO NOTHING",
            params![id, account.name, account.family, account.base, ciphertext, account.model],
        )?
    };
    if saved == 0 {
        return Err(changed());
    }
    let mut result = providers::public(&account);
    result["revision"] = json!(revision.unwrap_or(0) + 1);
    Ok(json_response(
        if revision.is_some() { 200 } else { 201 },
        result,
    ))
}

fn remove(state: &AppState, id: &str) -> Result<Response> {
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    transaction(db, || {
        let agents = all(
            db,
            "SELECT name FROM agents WHERE provider_account=? AND (id<>? OR EXISTS(
                SELECT 1 FROM turns WHERE agent=agents.id AND status IN('queued','running'))) ORDER BY name",
            [id, crate::mcp::guide::ID],
        )?;
        if !agents.is_empty() {
            let names = agents
                .iter()
                .map(|agent| string(agent, "name"))
                .collect::<Vec<_>>();
            return Err(error(
                409,
                format!("Provider is used by {}.", names.join(", ")),
            ));
        }
        if db.execute("DELETE FROM provider_accounts WHERE id=?", [id])? == 0 {
            return Err(error(404, "Provider account not found."));
        }
        crate::mcp::guide::detach_connection(db, id)?;
        Ok(json_response(200, json!({"deleted":true})))
    })
}

pub(super) async fn route(
    state: &AppState,
    parts: &[&str],
    method: &Method,
    query: &[(String, String)],
    body: &Value,
) -> Result<Response> {
    if !query.is_empty() {
        return Err(error(400, "Invalid provider account parameters."));
    }
    if *method == Method::GET && parts.len() == 2 {
        return Ok(json_response(200, providers::list(state)?));
    }
    if *method != Method::POST {
        return Err(error(405, "Use GET or POST for provider accounts."));
    }
    let request_id = crate::web::id(string(body, "id"))
        .ok_or_else(|| error(400, "Field id must be a UUID v4."))?;
    let (id, action, previous) = match parts {
        ["api", "provider-accounts"] => (request_id.as_str(), "create", None),
        ["api", "provider-accounts", "test"] => (request_id.as_str(), "test", None),
        ["api", "provider-accounts", id, action] => {
            if state.config.data.account(id).is_some() {
                return Err(error(
                    409,
                    "Provider account is from the server environment.",
                ));
            }
            let previous =
                read(state, id)?.ok_or_else(|| error(404, "Provider account not found."))?;
            (*id, *action, Some(previous))
        }
        _ => return Err(error(404, "Route not found.")),
    };
    match action {
        "create" | "edit" => {
            if state.config.data.account(id).is_some() {
                return Err(error(
                    409,
                    "Provider account is from the server environment.",
                ));
            }
            save(state, body, id, previous).await
        }
        "test" => {
            let account = candidate(state, body, id, previous.as_ref())?;
            let models = providers::test_models(state, &account).await?;
            Ok(json_response(200, json!({"models":models})))
        }
        "delete" if closed(body, &["id"]) => remove(state, id),
        "delete" => Err(error(400, "Unknown provider account field.")),
        _ => Err(error(404, "Route not found.")),
    }
}

#[cfg(test)]
mod tests;
