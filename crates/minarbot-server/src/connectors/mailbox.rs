use super::*;
use axum::{body::to_bytes, http::header, response::IntoResponse};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use mail_parser::MessageParser;
use ring::hmac;
use std::time::Duration;

mod tools;
pub use tools::{available, decide_draft, draft_preview, is_tool, journal, replay, run, specs};

pub const INBOUND: &str = "/api/email/inbound";
const KEY_SUFFIX: &str = "agent-mail-key";
const KEY_ID: &str = "00000000-0000-4000-8000-000000000001";
const MAX_RAW: usize = 2 * 1024 * 1024;
const PAGE: usize = 25;

pub fn initialize(db: &Connection) -> Result<()> {
    transaction(db, || {
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS agent_email_settings(
                id INTEGER PRIMARY KEY CHECK(id=1), domain TEXT, secret TEXT, revision INTEGER NOT NULL
             ) STRICT;
             INSERT OR IGNORE INTO agent_email_settings VALUES(1,NULL,NULL,0);
             CREATE TABLE IF NOT EXISTS agent_email_addresses(
                agent TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE, slug TEXT NOT NULL UNIQUE
             ) STRICT;
             CREATE TABLE IF NOT EXISTS agent_email_messages(
                seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE,
                agent TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
                received INTEGER NOT NULL, digest TEXT NOT NULL, ciphertext TEXT NOT NULL,
                UNIQUE(agent,digest)
             ) STRICT;
             CREATE INDEX IF NOT EXISTS agent_email_inbox ON agent_email_messages(agent,seq);
             CREATE TABLE IF NOT EXISTS agent_email_drafts(
                id TEXT PRIMARY KEY, agent TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
                turn TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE, call_id TEXT NOT NULL,
                ciphertext TEXT NOT NULL, created INTEGER NOT NULL, UNIQUE(turn,call_id)
             ) STRICT;",
        )?;
        for row in all(
            db,
            "SELECT id,name FROM agents WHERE id NOT IN(SELECT agent FROM agent_email_addresses)",
            [],
        )? {
            register(db, string(&row, "id"), string(&row, "name"))?;
        }
        Ok(())
    })
}

pub fn register(db: &Connection, agent: &str, name: &str) -> Result<()> {
    let slug = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .to_ascii_lowercase();
    let prefix = slug[..slug.len().min(31)].trim_end_matches('-');
    let suffix = agent.replace('-', "").to_ascii_lowercase();
    let slug = if prefix.is_empty() {
        suffix
    } else {
        format!("{prefix}-{suffix}")
    };
    db.execute(
        "INSERT INTO agent_email_addresses VALUES(?,?)",
        [agent, &slug],
    )?;
    Ok(())
}

fn key(state: &AppState, create: bool) -> Result<[u8; 32]> {
    crate::mcp::client::key(state, create, KEY_SUFFIX).map_err(|_| {
        error(
            503,
            "Agent email encryption key is unavailable on this install.",
        )
    })
}

fn seal(key: &[u8], context: &Value, value: &Value) -> Result<String> {
    String::from_utf8(mail::seal_envelope(
        KEY_ID,
        key,
        &context.to_string(),
        value.to_string().as_bytes(),
    )?)
    .map_err(|_| Failure::storage())
}

fn open(key: &[u8], context: &Value, ciphertext: &str) -> Result<Value> {
    let plain = mail::open_envelope(ciphertext.as_bytes(), key, &context.to_string())
        .map_err(|_| Failure::storage())?;
    serde_json::from_slice(&plain).map_err(|_| Failure::storage())
}

fn settings(db: &Connection) -> Result<Value> {
    one(
        db,
        "SELECT domain,secret,revision FROM agent_email_settings WHERE id=1",
        [],
    )?
    .ok_or_else(Failure::storage)
}

fn public_settings(row: &Value) -> Value {
    json!({"domain":row["domain"],"hasSecret":row["secret"].is_string(),"revision":row["revision"],"endpoint":INBOUND})
}

fn save_settings(state: &AppState, db: &Connection, input: &Value) -> Result<Value> {
    let row = settings(db)?;
    if row["revision"] != input["revision"] {
        return Err(error(409, "Email settings changed. Reload and try again."));
    }
    if closed(input, &["id", "revision", "domain"]) {
        let raw = input["domain"]
            .as_str()
            .ok_or_else(|| error(400, "Enter a receiving domain."))?
            .trim();
        let domain = if raw.is_empty() {
            None
        } else {
            let fail = || error(400, "Enter a valid receiving domain.");
            let email =
                crate::data::normalize_email(&json!(format!("x@{raw}"))).map_err(|_| fail())?;
            let domain = email
                .strip_prefix("x@")
                .filter(|s| s.contains('.') && s.len() <= 189)
                .ok_or_else(fail)?;
            Some(domain.to_owned())
        };
        db.execute(
            "UPDATE agent_email_settings SET domain=?, revision=revision+1 WHERE id=1",
            [domain],
        )?;
        return Ok(public_settings(&settings(db)?));
    }
    if !closed(input, &["id", "revision", "rotateSecret"])
        || input["rotateSecret"] != true
        || row["domain"].is_null()
    {
        return Err(error(
            400,
            "Save a receiving domain before generating a secret.",
        ));
    }
    let mut secret = [0; 32];
    getrandom::getrandom(&mut secret).map_err(|_| Failure::storage())?;
    let secret = B64.encode(secret);
    let encrypted = seal(
        &key(state, row["secret"].is_null())?,
        &json!(["agent-mail-relay-v1"]),
        &json!(secret),
    )?;
    db.execute(
        "UPDATE agent_email_settings SET secret=?, revision=revision+1 WHERE id=1",
        [encrypted],
    )?;
    let mut result = public_settings(&settings(db)?);
    result["secret"] = json!(secret);
    Ok(result)
}

fn address(db: &Connection, agent: &str) -> Result<Option<String>> {
    let row = one(db, "SELECT a.slug,s.domain FROM agent_email_addresses a CROSS JOIN agent_email_settings s WHERE a.agent=?", [agent])?
        .ok_or_else(|| error(404, "Agent not found."))?;
    Ok(row["domain"]
        .as_str()
        .map(|domain| format!("{}@{domain}", string(&row, "slug"))))
}

fn configured_address(db: &Connection, agent: &str) -> Result<String> {
    address(db, agent)?.ok_or_else(|| error(409, "Configure a receiving domain in Settings."))
}

fn cursor(value: &Value) -> Result<Option<i64>> {
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_i64()
        .filter(|n| *n > 0)
        .map(Some)
        .ok_or_else(|| error(400, "Invalid inbox cursor."))
}

fn message(key: &[u8], agent: &str, row: &Value) -> Result<Value> {
    let mut message = open(
        key,
        &json!(["agent-mail-message-v1", agent, row["id"]]),
        string(row, "ciphertext"),
    )?;
    message["id"] = row["id"].clone();
    message["received"] = row["received"].clone();
    Ok(message)
}

fn list(state: &AppState, db: &Connection, agent: &str, before: Option<i64>) -> Result<Value> {
    let address = address(db, agent)?;
    let rows = all(
        db,
        "SELECT * FROM agent_email_messages WHERE agent=? AND seq<? ORDER BY seq DESC LIMIT ?",
        params![agent, before.unwrap_or(i64::MAX), PAGE + 1],
    )?;
    let mut items = Vec::new();
    if !rows.is_empty() {
        let key = key(state, false)?;
        for row in rows.iter().take(PAGE) {
            let mail = message(&key, agent, row)?;
            items.push(json!({"id":mail["id"],"from":mail["from"],"subject":mail["subject"],"received":mail["received"]}));
        }
    }
    Ok(
        json!({"address":address,"items":items,"nextBefore":if rows.len()>PAGE {rows[PAGE-1]["seq"].as_i64()} else {None}}),
    )
}

fn read(state: &AppState, db: &Connection, agent: &str, id: &str) -> Result<Value> {
    let missing = || error(404, "Email message not found.");
    if !uuid(id) {
        return Err(missing());
    }
    let row = one(
        db,
        "SELECT * FROM agent_email_messages WHERE agent=? AND id=?",
        [agent, id],
    )?
    .ok_or_else(missing)?;
    message(&key(state, false)?, agent, &row)
}

pub fn owner_route(
    state: &AppState,
    path: &str,
    method: &Method,
    query: &[(String, String)],
    input: &Value,
) -> Result<Response> {
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    let parts: Vec<_> = path.split('/').collect();
    let value = match (method, parts.as_slice()) {
        (&Method::GET, ["", "api", "email", "settings"]) if query.is_empty() => {
            public_settings(&settings(db)?)
        }
        (&Method::POST, ["", "api", "email", "settings"]) if query.is_empty() => {
            save_settings(state, db, input)?
        }
        (&Method::GET, ["", "api", "agents", agent, "email"]) if uuid(agent) => {
            let before = match query {
                [] => None,
                [(name, value)] if name == "before" => {
                    cursor(&json!(value.parse::<i64>().unwrap_or_default()))?
                }
                _ => return Err(error(400, "Invalid inbox parameters.")),
            };
            list(state, db, agent, before)?
        }
        (&Method::GET, ["", "api", "agents", agent, "email", "messages", id])
            if uuid(agent) && query.is_empty() =>
        {
            read(state, db, agent, id)?
        }
        _ => return Err(error(404, "Email route not found.")),
    };
    Ok(crate::json_response(200, value))
}

fn signature(key: &[u8], timestamp: &str, recipient: &str, raw: &[u8]) -> hmac::Tag {
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let mut message = hmac::Context::with_key(&key);
    message.update(b"minarbot-email-v1\n");
    message.update(timestamp.as_bytes());
    message.update(b"\n");
    message.update(recipient.as_bytes());
    message.update(b"\n");
    message.update(raw);
    message.sign()
}

fn unauthorized() -> Failure {
    error(401, "Invalid email relay signature.")
}

fn parse(raw: &[u8], recipient: &str) -> Result<Value> {
    let invalid = || error(400, "Invalid RFC 5322 message.");
    let mail = MessageParser::default().parse(raw).ok_or_else(invalid)?;
    let from = mail
        .from()
        .and_then(|from| from.first())
        .and_then(|addr| addr.address());
    let from = from
        .and_then(|address| crate::data::normalize_email(&json!(address)).ok())
        .ok_or_else(invalid)?;
    let subject: String = mail
        .subject()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(320)
        .collect();
    // ponytail: retain plain text only. Add attachment storage if the inbox needs downloads.
    let body = mail.body_text(0).unwrap_or_default();
    let mut text = body
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\r' | '\n' | '\t'));
    let excerpt: String = text.by_ref().take(32_000).collect();
    Ok(
        json!({"from":from,"to":recipient,"subject":subject,"text":excerpt,"textTruncated":text.next().is_some()}),
    )
}

struct Delivery<'a> {
    timestamp: &'a str,
    recipient: &'a str,
    signature: &'a [u8],
    raw: &'a [u8],
}

fn deliver(
    db: &Connection,
    key: &[u8],
    config: &Value,
    delivery: Delivery<'_>,
    now: i64,
) -> Result<()> {
    let Delivery {
        timestamp,
        recipient,
        signature: signed,
        raw,
    } = delivery;
    let secret = open(
        key,
        &json!(["agent-mail-relay-v1"]),
        string(config, "secret"),
    )?;
    let instant = timestamp.parse::<i64>().map_err(|_| unauthorized())?;
    let tag = signature(
        secret.as_str().ok_or_else(Failure::storage)?.as_bytes(),
        timestamp,
        recipient,
        raw,
    );
    use subtle::ConstantTimeEq;
    if instant.abs_diff(now.div_euclid(1000)) > 300 || !bool::from(tag.as_ref().ct_eq(signed)) {
        return Err(unauthorized());
    }
    let unknown = || error(404, "Unknown email recipient.");
    let normalized = crate::data::normalize_email(&json!(recipient)).map_err(|_| unknown())?;
    let (slug, domain) = normalized.split_once('@').ok_or_else(unknown)?;
    if config["domain"] != domain {
        return Err(unknown());
    }
    let row = one(
        db,
        "SELECT agent FROM agent_email_addresses WHERE slug=?",
        [slug],
    )?
    .ok_or_else(unknown)?;
    let agent = string(&row, "agent");
    let digest_key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let mut digest = hmac::Context::with_key(&digest_key);
    digest.update(b"minarbot-mail-dedupe-v1\n");
    digest.update(normalized.as_bytes());
    digest.update(b"\n");
    digest.update(raw);
    let digest = B64.encode(digest.sign());
    if exists(
        db,
        "SELECT 1 FROM agent_email_messages WHERE agent=? AND digest=?",
        [agent, &digest],
    )? {
        return Ok(());
    }
    let value = parse(raw, &normalized)?;
    let id = uuid::Uuid::new_v4().to_string();
    let ciphertext = seal(key, &json!(["agent-mail-message-v1", agent, id]), &value)?;
    db.execute(
        "INSERT INTO agent_email_messages(id,agent,received,digest,ciphertext) VALUES(?,?,?,?,?)",
        params![id, agent, now, digest, ciphertext],
    )?;
    Ok(())
}

pub async fn inbound(state: &AppState, request: Request) -> Result<Response> {
    static CAPACITY: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
    if request.method() != Method::POST {
        return Err(error(405, "Email delivery requires POST."));
    }
    if request.headers().contains_key(header::ORIGIN)
        || request.uri().query().is_some()
        || crate::text_header(request.headers(), "content-type") != Some("message/rfc822")
    {
        return Err(error(
            400,
            "Send raw email with Content-Type message/rfc822.",
        ));
    }
    let recipient = crate::text_header(request.headers(), "x-minarbot-email-to")
        .filter(|s| !s.is_empty() && s.len() <= 254)
        .map(str::to_owned)
        .ok_or_else(unauthorized)?;
    let timestamp = crate::text_header(request.headers(), "x-minarbot-email-timestamp")
        .filter(|s| !s.is_empty() && s.len() <= 12)
        .map(str::to_owned)
        .ok_or_else(unauthorized)?;
    let signed = B64
        .decode(
            crate::text_header(request.headers(), "x-minarbot-email-signature")
                .filter(|s| !s.is_empty() && s.len() <= 43)
                .ok_or_else(unauthorized)?,
        )
        .ok()
        .filter(|s| s.len() == 32)
        .ok_or_else(unauthorized)?;
    let _permit = CAPACITY
        .try_acquire()
        .map_err(|_| error(503, "Email relay is busy. Retry delivery."))?;
    let raw = tokio::time::timeout(
        Duration::from_secs(10),
        to_bytes(request.into_body(), MAX_RAW),
    )
    .await
    .map_err(|_| error(408, "Email upload timed out."))?
    .map_err(|_| error(413, "Email exceeds 2 MiB."))?;
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    let config = settings(db)?;
    if config["domain"].is_null() || config["secret"].is_null() {
        return Err(error(503, "Email receiving is disabled."));
    }
    deliver(
        db,
        &key(state, false)?,
        &config,
        Delivery {
            timestamp: &timestamp,
            recipient: &recipient,
            signature: &signed,
            raw: &raw,
        },
        state.config.now(),
    )?;
    Ok(axum::http::StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests;
