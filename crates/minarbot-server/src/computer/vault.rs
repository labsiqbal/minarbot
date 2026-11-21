use super::{error, Result};
use crate::{
    data::{closed, exists, one, string},
    web::Failure,
    AppState,
};
use axum::{extract::Request, http::Method, response::Response};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::{
    aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM},
    hmac,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub(super) fn initialize(db: &Connection) -> crate::db::Result<()> {
    db.execute_batch(include_str!("vault.sql"))?;
    Ok(())
}

pub(super) fn recover_requests(db: &Connection) -> crate::db::Result<()> {
    db.execute_batch(
        "UPDATE turns SET status='interrupted',paused=0,revision=revision+1,
            error='Value saved. Secure fill outcome is uncertain. Check Computer before starting a new task.'
         WHERE status='queued' AND paused=1 AND id IN (
            SELECT v.turn FROM vault_requests v
            JOIN computer_waits w ON w.turn=v.turn AND w.call_id=v.call_id AND w.state='waiting'
            JOIN computer_actions a ON a.turn=v.turn AND a.call_id=v.call_id AND a.state='uncertain'
         );
         UPDATE computer_waits SET state='returned' WHERE state='waiting' AND EXISTS(
            SELECT 1 FROM vault_requests v
            JOIN computer_actions a ON a.turn=v.turn AND a.call_id=v.call_id AND a.state='uncertain'
            WHERE v.turn=computer_waits.turn AND v.call_id=computer_waits.call_id
         );",
    )?;
    Ok(())
}

fn unavailable() -> Failure {
    error(503, "Unlock the system keyring to use Vault.")
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    getrandom::getrandom(&mut bytes).map_err(|_| Failure::storage())?;
    Ok(bytes)
}

pub(super) fn key(state: &AppState, db: &Connection, create: bool) -> Result<LessSafeKey> {
    let name: Option<String> = db
        .query_row("SELECT name FROM vault_key WHERE id=1", [], |r| r.get(0))
        .optional()?;
    let store = &state.config.remaining_data;
    let name = match name {
        Some(name) => name,
        None if create && !exists(db, "SELECT 1 FROM vault_entries", [])? => {
            let name = format!("vault-aead/{}", uuid::Uuid::new_v4());
            let encoded = URL_SAFE_NO_PAD.encode(random::<32>()?);
            store
                .write(&name, &encoded, false)
                .map_err(|_| unavailable())?;
            if store
                .read(&name, false)
                .map_err(|_| unavailable())?
                .as_deref()
                != Some(&encoded)
            {
                return Err(unavailable());
            }
            db.execute("INSERT INTO vault_key VALUES(1,?)", [&name])?;
            name
        }
        None => return Err(unavailable()),
    };
    if !name
        .strip_prefix("vault-aead/")
        .is_some_and(|id| crate::web::id(id).is_some())
    {
        return Err(Failure::storage());
    }
    let bytes = store.key(&name, false).map_err(|_| unavailable())?;
    Ok(LessSafeKey::new(
        UnboundKey::new(&AES_256_GCM, &bytes).map_err(|_| unavailable())?,
    ))
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Payload {
    url: String,
    username: String,
    password: String,
    totp: String,
    notes: String,
    value: String,
}

fn seal(key: &LessSafeKey, name: &str, payload: &Payload) -> Result<Vec<u8>> {
    let nonce = random::<12>()?;
    let mut bytes = serde_json::to_vec(payload).map_err(|_| Failure::storage())?;
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::from(name),
        &mut bytes,
    )
    .map_err(|_| Failure::storage())?;
    Ok([nonce.as_slice(), &bytes].concat())
}

fn open(key: &LessSafeKey, name: &str, mut bytes: Vec<u8>) -> Result<Payload> {
    let nonce = bytes
        .get(..12)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(Failure::storage)?;
    let plain = key
        .open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(name),
            &mut bytes[12..],
        )
        .map_err(|_| Failure::storage())?;
    serde_json::from_slice(plain).map_err(|_| Failure::storage())
}

pub(super) fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

pub(super) fn origin(raw: &str) -> Result<String> {
    if raw.len() > 2000 || raw.chars().any(char::is_control) {
        return Err(error(400, "Enter a valid HTTPS URL."));
    }
    let url = url::Url::parse(raw).map_err(|_| error(400, "Enter a valid HTTPS URL."))?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(url.scheme() == "https" || url.scheme() == "http" && loopback)
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(error(400, "Use HTTPS or local HTTP without credentials."));
    }
    Ok(url.origin().ascii_serialization())
}

pub(super) fn value(raw: &str) -> bool {
    (1..=4000).contains(&raw.encode_utf16().count()) && !raw.chars().any(char::is_control)
}

fn decode_totp(text: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut bits = 0;
    let mut buf: u16 = 0;
    for c in text.bytes().filter(|&c| c != b' ') {
        let v = match c.to_ascii_uppercase() {
            b'A'..=b'Z' => c.to_ascii_uppercase() - b'A',
            b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        };
        buf = (buf << 5) | u16::from(v);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            bytes.push((buf >> bits) as u8);
        }
    }
    (bits < 5 && buf & ((1 << bits) - 1) == 0 && (10..=128).contains(&bytes.len())).then_some(bytes)
}

fn otp(seed: &str, now: i64) -> Option<String> {
    let counter = u64::try_from(now).ok()? / 30_000;
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, &decode_totp(seed)?);
    let tag = hmac::sign(&key, &counter.to_be_bytes());
    let bytes = tag.as_ref();
    let offset = usize::from(bytes[19] & 15);
    let code = u32::from_be_bytes(bytes[offset..offset + 4].try_into().ok()?);
    Some(format!("{:06}", (code & 0x7fff_ffff) % 1_000_000))
}

pub(super) fn choices(state: &AppState, db: &Connection, agent: &str) -> Result<Value> {
    let mut entries = crate::data::all(db, "SELECT e.name,e.kind FROM vault_entries e JOIN vault_grants g ON g.entry=e.name WHERE g.agent=? ORDER BY e.name", [agent])?;
    if !entries.is_empty() {
        let key = key(state, db, false)?;
        for entry in &mut entries {
            let name = string(entry, "name");
            let bytes = db.query_row(
                "SELECT payload FROM vault_entries WHERE name=?",
                [name],
                |r| r.get(0),
            )?;
            let payload = open(&key, name, bytes)?;
            let mut fields = if entry["kind"] == "login" {
                vec!["username", "password"]
            } else {
                vec!["value"]
            };
            if !payload.totp.is_empty() {
                fields.push("totp");
            }
            entry["origin"] = json!(origin(&payload.url)?);
            entry["fields"] = json!(fields);
        }
    }
    Ok(json!({"entries":entries}))
}

pub(super) fn placeholder(name: &str, field: &str) -> String {
    format!("{{{{vault:{name}.{field}}}}}")
}

pub(super) fn resolve(
    state: &AppState,
    db: &Connection,
    agent: &str,
    name: &str,
    field: &str,
    now: i64,
) -> Result<(String, String)> {
    let stored: Option<(String, Vec<u8>)> = db.query_row(
        "SELECT e.kind,e.payload FROM vault_entries e JOIN vault_grants g ON g.entry=e.name WHERE e.name=? AND g.agent=?",
        [name, agent],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional()?;
    let (kind, bytes) =
        stored.ok_or_else(|| error(403, "Vault entry unavailable to this agent."))?;
    let payload = open(&key(state, db, false)?, name, bytes)?;
    let secret = match (kind.as_str(), field) {
        ("login", "username") => payload.username,
        ("login", "password") => payload.password,
        ("login", "totp") => otp(&payload.totp, now).unwrap_or_default(),
        ("secret", "value") => payload.value,
        _ => String::new(),
    };
    if secret.is_empty() {
        return Err(error(400, "Vault field is unavailable."));
    }
    Ok((origin(&payload.url)?, secret))
}

pub(crate) fn redactions(state: &AppState, db: &Connection) -> Result<Vec<String>> {
    if !exists(
        db,
        "SELECT 1 FROM sqlite_schema WHERE name='vault_entries'",
        [],
    )? || !exists(db, "SELECT 1 FROM vault_entries", [])?
    {
        return Ok(Vec::new());
    }
    let key = key(state, db, false)?;
    let now = state.config.now();
    let mut statement = db.prepare("SELECT name,payload FROM vault_entries")?;
    let mut rows = statement.query([])?;
    let mut values = Vec::new();
    while let Some(row) = rows.next()? {
        let name: String = row.get(0)?;
        let payload = open(&key, &name, row.get(1)?)?;
        for period in [-30_000, 0, 30_000] {
            values.extend(otp(&payload.totp, now + period));
        }
        values.extend([
            payload.username,
            payload.password,
            payload.totp,
            payload.value,
            payload.notes,
        ]);
    }
    values.retain(|s| !s.is_empty());
    Ok(values)
}

fn list(state: &AppState, db: &Connection) -> Result<Value> {
    let mut items = crate::data::all(
        db,
        "SELECT name,kind,revision FROM vault_entries ORDER BY name",
        [],
    )?;
    if items.is_empty() {
        return Ok(json!(items));
    }
    // Metadata and grants remain manageable if the operator's keyring is locked.
    let key = key(state, db, false).ok();
    for item in &mut items {
        let name = string(item, "name");
        let mut grants =
            db.prepare("SELECT agent FROM vault_grants WHERE entry=? ORDER BY agent")?;
        let agents = grants
            .query_map([name], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if let Some(key) = &key {
            let bytes = db.query_row(
                "SELECT payload FROM vault_entries WHERE name=?",
                [name],
                |r| r.get(0),
            )?;
            let payload = open(key, name, bytes)?;
            item["origin"] = json!(origin(&payload.url)?);
            item["hasTotp"] = json!(!payload.totp.is_empty());
            item["hasNotes"] = json!(!payload.notes.is_empty());
        }
        item["agents"] = json!(agents);
    }
    Ok(json!(items))
}

pub(super) fn store_requested(
    db: &Connection,
    key: &LessSafeKey,
    name: &str,
    agent: &str,
    origin: &str,
    value: &str,
) -> Result<()> {
    save(
        db,
        key,
        &json!({"name":name,"kind":"secret","revision":0,"agents":[agent],"url":origin,"value":value}),
    )
}

fn save(db: &Connection, key: &LessSafeKey, body: &Value) -> Result<()> {
    let name = string(body, "name");
    let kind = string(body, "kind");
    let revision = body["revision"]
        .as_i64()
        .filter(|n| *n >= 0)
        .ok_or_else(|| error(400, "Invalid Vault revision."))?;
    if !valid_name(name)
        || !matches!(kind, "login" | "secret")
        || !closed(
            body,
            &[
                "name", "kind", "revision", "url", "username", "password", "totp", "notes",
                "value", "agents",
            ],
        )
    {
        return Err(error(400, "Invalid Vault entry."));
    }
    let current = one(
        db,
        "SELECT kind,revision FROM vault_entries WHERE name=?",
        [name],
    )?;
    if current.as_ref().is_some_and(|v| v["kind"] != kind)
        || current
            .as_ref()
            .and_then(|v| v["revision"].as_i64())
            .unwrap_or(0)
            != revision
    {
        return Err(error(409, "Vault entry changed. Reload Vault."));
    }
    let agents = body["agents"]
        .as_array()
        .ok_or_else(|| error(400, "Choose valid agents."))?;
    let agents = agents
        .iter()
        .map(|a| {
            a.as_str()
                .filter(|s| crate::web::id(s).is_some())
                .ok_or_else(|| error(400, "Choose valid agents."))
        })
        .collect::<Result<std::collections::BTreeSet<_>>>()?;
    for agent in &agents {
        if !exists(db, "SELECT 1 FROM agents WHERE id=?", [agent])? {
            return Err(error(400, "Choose valid agents."));
        }
    }
    let mut payload = if revision == 0 {
        Payload::default()
    } else {
        let bytes = db.query_row(
            "SELECT payload FROM vault_entries WHERE name=?",
            [name],
            |r| r.get(0),
        )?;
        open(key, name, bytes)?
    };
    for (field, target) in [
        ("url", &mut payload.url),
        ("username", &mut payload.username),
        ("password", &mut payload.password),
        ("totp", &mut payload.totp),
        ("notes", &mut payload.notes),
        ("value", &mut payload.value),
    ] {
        if let Some(input) = body.get(field) {
            let input = input
                .as_str()
                .filter(|s| s.is_empty() || field == "notes" && s.len() <= 4000 || value(s))
                .ok_or_else(|| error(400, "Invalid Vault field."))?;
            *target = input.to_owned();
        }
    }
    origin(&payload.url)?;
    if kind == "login"
        && (!value(&payload.username)
            || !value(&payload.password)
            || !payload.value.is_empty()
            || !payload.totp.is_empty() && decode_totp(&payload.totp).is_none())
        || kind == "secret"
            && (!value(&payload.value)
                || ![&payload.username, &payload.password, &payload.totp]
                    .iter()
                    .all(|s| s.is_empty()))
    {
        return Err(error(
            400,
            "Complete the required fields and check the TOTP key.",
        ));
    }
    let encrypted = seal(key, name, &payload)?;
    db.execute("INSERT INTO vault_entries VALUES(?,?,1,?) ON CONFLICT(name) DO UPDATE SET payload=excluded.payload,revision=revision+1", params![name,kind,encrypted])?;
    db.execute("DELETE FROM vault_grants WHERE entry=?", [name])?;
    for agent in agents {
        db.execute("INSERT INTO vault_grants VALUES(?,?)", [name, agent])?;
    }
    Ok(())
}

pub(crate) async fn local(state: &AppState, request: Request) -> Result<Response> {
    let path = request.uri().path();
    let headers = request.headers();
    if headers.contains_key("authorization")
        || headers.contains_key("origin") && !crate::data::origin(state, headers)
    {
        return Err(error(403, "Vault requires the local owner in Minarbot."));
    }
    if request.uri().query().is_some() {
        return Err(error(400, "Vault does not accept query parameters."));
    }
    if request.method() == Method::GET && path == "/api/vault" {
        let db = state.database.lock().map_err(|_| Failure::storage())?;
        return Ok(crate::json_response(200, list(state, &db.connection)?));
    }
    if request.method() != Method::POST {
        return Err(error(405, "Method not allowed."));
    }
    crate::files::mutation(state, &request)?;
    let path = path.to_owned();
    let bytes = axum::body::to_bytes(request.into_body(), 64_000)
        .await
        .map_err(|_| error(413, "Vault request is too large."))?;
    let body = crate::web::parse_json(&bytes).map_err(|_| error(400, "Invalid Vault JSON."))?;
    if path == "/api/vault/respond" {
        super::control::respond_secret(state, &body).await?;
        return Ok(crate::json_response(200, json!({"ok":true})));
    }
    let db = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &db.connection;
    match path.as_str() {
        "/api/vault" => {
            let key = key(state, db, body["revision"] == 0)?;
            super::transaction(db, |db| save(db, &key, &body))?;
        }
        "/api/vault/delete" => {
            if !closed(&body, &["name", "revision"]) || !valid_name(string(&body, "name")) {
                return Err(error(400, "Invalid Vault entry."));
            }
            if db.execute(
                "DELETE FROM vault_entries WHERE name=? AND revision=?",
                params![string(&body, "name"), body["revision"].as_i64()],
            )? != 1
            {
                return Err(error(409, "Vault entry changed. Reload Vault."));
            }
        }
        _ => return Err(error(404, "Route not found.")),
    }
    Ok(crate::json_response(200, list(state, db)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_totp_uses_rfc_6238_counter_and_never_fills_the_seed() -> Result<()> {
        assert_eq!(
            otp("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ", 59_000).as_deref(),
            Some("287082")
        );
        assert!(otp("unusable", 59_000).is_none());
        Ok(())
    }

    #[test]
    fn vault_ciphertext_binds_entry_name_and_detects_tampering() -> Result<()> {
        let key = LessSafeKey::new(
            UnboundKey::new(&AES_256_GCM, &[39; 32]).map_err(|_| Failure::storage())?,
        );
        let value = Payload {
            value: "synthetic".into(),
            ..Default::default()
        };
        let mut ciphertext = seal(&key, "fixture", &value)?;
        assert!(ciphertext != seal(&key, "fixture", &value)?);
        assert!(open(&key, "other", ciphertext.clone()).is_err());
        assert!(open(&key, "fixture", ciphertext.clone())?.value == value.value);
        ciphertext[15] ^= 1;
        assert!(open(&key, "fixture", ciphertext).is_err());
        Ok(())
    }
}
