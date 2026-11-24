pub mod host;
pub mod waits;
use crate::{
    auth::Actor,
    data, db,
    files::{one, rows, text},
    web::Failure,
    AppState,
};
use axum::{
    extract::Request,
    http::{header, Method},
    response::Response,
    routing::any,
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use ring::signature;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};
use tokio::sync::mpsc;
type Result<T> = std::result::Result<T, Failure>;
#[derive(Clone)]
pub struct Principal {
    pub device: String,
    pub account: String,
    pub generation: i64,
    pub authority: String,
    pub(crate) session: Option<String>,
}
struct Stream {
    principal: Principal,
    generation: i64,
    sender: mpsc::Sender<String>,
    provider_instances: Option<(Value, bool)>,
}
#[derive(Default)]
struct Runtime {
    streams: HashMap<String, Stream>,
    buckets: HashMap<String, (f64, i64)>,
}
static RUNTIME: OnceLock<Mutex<Runtime>> = OnceLock::new();
fn runtime() -> Result<std::sync::MutexGuard<'static, Runtime>> {
    RUNTIME
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| Failure::storage())
}
fn fail(status: u16, code: &'static str, message: &str) -> Failure {
    Failure::new(status, code, message)
}
fn denied() -> Failure {
    fail(
        401,
        "device_session_invalid",
        "Invalid session or device proof.",
    )
}
fn missing() -> Failure {
    fail(404, "resource_not_found", "Device not found.")
}
fn conflict() -> Failure {
    fail(
        409,
        "device_revision_conflict",
        "Device changed. Load the latest revision.",
    )
}
fn limited_error(ms: i64) -> Failure {
    let mut e = fail(429, "rate_limited", "Work limit reached. Try again later.");
    e.retryable = true;
    e.details = Some(json!({"retryAfterMs":ms}));
    e
}
fn limited(name: String, capacity: i64, interval: i64, now: i64) -> Result<()> {
    let mut rt = runtime()?;
    rt.buckets.retain(|_, (_, at)| *at >= now - 600000);
    if !rt.buckets.contains_key(&name) && rt.buckets.len() >= 8192 {
        return Err(limited_error(60000));
    }
    let (old, at) = rt
        .buckets
        .get(&name)
        .copied()
        .unwrap_or((capacity as f64, now));
    let tokens = (old + (now - at) as f64 * capacity as f64 / interval as f64).min(capacity as f64);
    rt.buckets.insert(name, ((tokens - 1.0).max(0.0), now));
    if tokens < 1.0 {
        return Err(limited_error(
            ((1.0 - tokens) * interval as f64 / capacity as f64).ceil() as i64,
        ));
    }
    Ok(())
}
pub fn fields(v: &Value, allowed: &[&str]) -> Result<()> {
    let obj = v
        .as_object()
        .ok_or_else(|| fail(400, "invalid_object", "Request must be a JSON object."))?;
    for key in obj.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(Failure::new(
                400,
                "unknown_field",
                format!("Field {key} is unknown."),
            ));
        }
    }
    Ok(())
}
pub fn id(v: &Value) -> Result<String> {
    v.as_str()
        .and_then(crate::web::id)
        .ok_or_else(|| fail(400, "invalid_id", "ID must be a UUID v4."))
}
pub fn revision(v: &Value, min: i64) -> Result<i64> {
    v.as_i64()
        .filter(|n| (min..=9_007_199_254_740_991).contains(n))
        .ok_or_else(|| {
            fail(
                400,
                "invalid_revision",
                if min == 0 {
                    "Revision must be a nonnegative safe integer."
                } else {
                    "Revision must be greater than zero."
                },
            )
        })
}
pub fn decode(v: &Value, size: usize) -> Result<Vec<u8>> {
    let raw = v.as_str().unwrap_or("");
    B64.decode(raw)
        .ok()
        .filter(|b| b.len() == size && B64.encode(b) == raw)
        .ok_or_else(|| {
            fail(
                400,
                "invalid_encoding",
                "Invalid base64url size or encoding.",
            )
        })
}
pub fn public_key(v: &Value, kind: &str) -> Result<Vec<u8>> {
    let bytes = decode(v, 44)?;
    let prefix = if kind == "ed25519" {
        [
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
        ]
    } else {
        [
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x6e, 0x03, 0x21, 0x00,
        ]
    };
    if !bytes.starts_with(&prefix) {
        return Err(Failure::new(
            400,
            "invalid_key",
            format!("Public key {kind} is invalid."),
        ));
    }
    Ok(bytes)
}
pub fn verify(key: &Value, body: &Value, sig: &Value) -> Result<()> {
    let key = public_key(key, "ed25519")?;
    let sig = decode(sig, 64)?;
    signature::UnparsedPublicKey::new(&signature::ED25519, &key[12..])
        .verify(body.to_string().as_bytes(), &sig)
        .map_err(|_| denied())
}
fn random() -> Result<String> {
    let mut bytes = [0; 32];
    getrandom::getrandom(&mut bytes).map_err(|_| Failure::storage())?;
    Ok(B64.encode(bytes))
}
pub fn zone(v: &Value) -> Result<String> {
    let raw = v
        .as_str()
        .filter(|s| {
            let mut parts = s.split('/');
            s.len() <= 120
                && parts.next().is_some_and(|s| {
                    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_')
                })
                && parts.all(|s| {
                    !s.is_empty()
                        && s.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_+.-".contains(&b))
                })
        })
        .ok_or_else(|| fail(400, "invalid_zone", "Use an IANA time zone."))?;
    // ponytail: system IANA zones require Linux tzdata; embed a tzdb when shipping without it.
    static ZONES: OnceLock<Vec<String>> = OnceLock::new();
    let zones = ZONES.get_or_init(|| {
        use std::io::Read;
        let root = std::path::Path::new("/usr/share/zoneinfo");
        let mut pending = vec![root.to_path_buf()];
        let mut names = Vec::new();
        while let Some(directory) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(relative) = path.strip_prefix(root) else {
                    continue;
                };
                if relative.components().count() > 4
                    || ["posix", "right", "localtime", "posixrules", "Factory"]
                        .contains(&relative.to_string_lossy().as_ref())
                {
                    continue;
                }
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    pending.push(path);
                    continue;
                }
                let mut header = [0; 4];
                if std::fs::File::open(&path)
                    .and_then(|mut f| f.read_exact(&mut header))
                    .is_ok()
                    && &header == b"TZif"
                {
                    names.push(relative.to_string_lossy().into_owned());
                }
            }
        }
        names
    });
    if !zones.iter().any(|s| s.eq_ignore_ascii_case(raw)) {
        return Err(fail(400, "invalid_zone", "Invalid IANA time zone."));
    }
    Ok(raw.into())
}

fn authority(db: &Connection) -> Result<String> {
    one(
        db,
        "SELECT generation FROM authority_state WHERE id=1 AND serving_state='active'",
        [],
    )?
    .map(|r| text(&r, "generation").to_owned())
    .ok_or_else(|| {
        let mut e = fail(503, "authority_unavailable", "Authority is not active yet.");
        e.retryable = true;
        e
    })
}
fn cleanup(db: &Connection, now: i64) -> Result<()> {
    for table in ["link_challenges", "proof_replays", "device_sessions"] {
        db.execute(&format!("DELETE FROM {table} WHERE rowid IN(SELECT rowid FROM {table} WHERE expires_at<? ORDER BY expires_at LIMIT 1000)"),[now])?;
    }
    Ok(())
}
fn candidate(v: &Value) -> Result<Value> {
    fields(
        v,
        &[
            "requestId",
            "installationId",
            "deviceId",
            "mode",
            "expectedGeneration",
            "expectedKeyRevision",
            "installationPublicKey",
            "deviceSigningKey",
            "deviceEncryptionKey",
            "deviceLabel",
            "requestedCapabilities",
            "zone",
            "zoneRevision",
        ],
    )?;
    let mode = text(v, "mode");
    if !["new", "reenroll", "rotate"].contains(&mode) {
        return Err(fail(400, "invalid_mode", "Invalid enrollment mode."));
    }
    if mode == "new"
        && (v.get("expectedGeneration").is_some() || v.get("expectedKeyRevision").is_some())
    {
        return Err(fail(
            400,
            "invalid_revision",
            "New enrollment does not accept an old revision.",
        ));
    }
    let installation = public_key(&v["installationPublicKey"], "ed25519")?;
    let signing = public_key(&v["deviceSigningKey"], "ed25519")?;
    public_key(&v["deviceEncryptionKey"], "x25519")?;
    if installation == signing {
        return Err(fail(
            400,
            "invalid_key",
            "Installation and device must use different keys.",
        ));
    }
    let label = v["deviceLabel"]
        .as_str()
        .filter(|s| {
            !s.trim().is_empty()
                && s.encode_utf16().count() <= 120
                && !s.chars().any(|c| c < ' ' || c == '\x7f')
        })
        .ok_or_else(|| fail(400, "invalid_label", "deviceLabel is invalid."))?
        .trim();
    let caps = v["requestedCapabilities"]
        .as_array()
        .filter(|a| {
            a.iter().all(|c| {
                [
                    "execution",
                    "workspace-read",
                    "workspace-write",
                    "browser",
                    "provider",
                    "mcp",
                ]
                .contains(&c.as_str().unwrap_or(""))
            })
        })
        .ok_or_else(|| fail(422, "capability_missing", "Unsupported device capability."))?;
    let mut caps = caps.iter().filter_map(Value::as_str).collect::<Vec<_>>();
    caps.sort_unstable();
    caps.dedup();
    let mut c = json!({"requestId":id(&v["requestId"])?,"installationId":id(&v["installationId"])?,"deviceId":id(&v["deviceId"])?,"mode":mode,"installationPublicKey":v["installationPublicKey"],"deviceSigningKey":v["deviceSigningKey"],"deviceEncryptionKey":v["deviceEncryptionKey"],"deviceLabel":label,"requestedCapabilities":caps,"zone":zone(&v["zone"])?,"zoneRevision":revision(&v["zoneRevision"],1)?});
    if mode != "new" {
        c["expectedGeneration"] = revision(&v["expectedGeneration"], 1)?.into();
        c["expectedKeyRevision"] = revision(&v["expectedKeyRevision"], 1)?.into();
    }
    Ok(c)
}
fn candidate_state(db: &Connection, actor: &Actor, c: &Value) -> Result<Value> {
    let install = one(
        db,
        "SELECT * FROM installations WHERE id=?",
        [text(c, "installationId")],
    )?;
    if install.as_ref().is_some_and(|r| {
        r["account"] != actor.account
            || r["signing_public_key"] != c["installationPublicKey"]
            || r["revoked"] != 0
    }) {
        return Err(missing());
    }
    let device = one(
        db,
        "SELECT * FROM devices WHERE id=?",
        [text(c, "deviceId")],
    )?;
    if c["mode"] == "new" {
        if device.is_some() {
            return Err(conflict());
        }
        return Ok(json!({"deviceGeneration":1,"keyRevision":1}));
    }
    let row = device
        .filter(|r| {
            install.is_some()
                && r["account"] == actor.account
                && r["installation"] == c["installationId"]
        })
        .ok_or_else(missing)?;
    let desc: Value =
        serde_json::from_str(text(&row, "descriptor_json")).map_err(|_| Failure::storage())?;
    if row["generation"] != c["expectedGeneration"]
        || desc["keyRevision"] != c["expectedKeyRevision"]
        || row["zone"] != c["zone"]
        || row["zone_revision"] != c["zoneRevision"]
    {
        return Err(conflict());
    }
    let changed = row["signing_public_key"] != c["deviceSigningKey"]
        || row["encryption_public_key"] != c["deviceEncryptionKey"];
    if changed != (c["mode"] == "rotate") {
        return Err(fail(
            409,
            "device_key_conflict",
            "Mode does not match the key change.",
        ));
    }
    Ok(
        json!({"deviceGeneration":revision(&row["generation"],1)?+1,"keyRevision":revision(&desc["keyRevision"],1)?+i64::from(c["mode"]=="rotate")}),
    )
}
fn descriptor(v: &Value, check: bool) -> Result<Value> {
    fields(
        v,
        &[
            "v",
            "accountId",
            "installationId",
            "deviceId",
            "authorityGeneration",
            "deviceGeneration",
            "keyRevision",
            "zone",
            "zoneRevision",
            "signingPublicKey",
            "encryptionPublicKey",
            "signature",
        ],
    )?;
    if v["v"] != 1 {
        return Err(fail(
            400,
            "invalid_descriptor",
            "Unsupported descriptor version.",
        ));
    }
    public_key(&v["signingPublicKey"], "ed25519")?;
    public_key(&v["encryptionPublicKey"], "x25519")?;
    decode(&v["signature"], 64)?;
    let d = json!({"v":1,"accountId":id(&v["accountId"])?,"installationId":id(&v["installationId"])?,"deviceId":id(&v["deviceId"])?,"authorityGeneration":id(&v["authorityGeneration"])?,"deviceGeneration":revision(&v["deviceGeneration"],1)?,"keyRevision":revision(&v["keyRevision"],1)?,"zone":zone(&v["zone"])?,"zoneRevision":revision(&v["zoneRevision"],1)?,"signingPublicKey":v["signingPublicKey"],"encryptionPublicKey":v["encryptionPublicKey"],"signature":v["signature"]});
    if check {
        verify(
            &d["signingPublicKey"],
            &json!([
                "minarbot-device-descriptor-v1",
                d["accountId"],
                d["installationId"],
                d["deviceId"],
                d["authorityGeneration"],
                d["deviceGeneration"],
                d["keyRevision"],
                d["zone"],
                d["zoneRevision"],
                d["signingPublicKey"],
                d["encryptionPublicKey"]
            ]),
            &d["signature"],
        )?;
    }
    Ok(d)
}
fn digest(v: &Value) -> String {
    db::digest(&db::canonical(v))
}
fn byte_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
fn challenge(db: &Connection, actor: &Actor, v: &Value, now: i64) -> Result<Value> {
    limited(format!("link-account:{}", actor.account), 5, 60000, now)?;
    limited("link-peer:loopback".into(), 10, 60000, now)?;
    limited("verify".into(), 400, 2000, now)?;
    let c = candidate(v)?;
    super::transaction(db, |db| {
        let generation = authority(db)?;
        let state = candidate_state(db, actor, &c)?;
        cleanup(db, now)?;
        for (column, key, max) in [
            ("1", "", 4096),
            ("account", "accountId", 32),
            ("installation", "installationId", 4),
            ("device", "deviceId", 8),
        ] {
            let sql = if column == "1" {
                "SELECT count(*) FROM link_challenges WHERE consumed=0 AND expires_at>=?".into()
            } else {
                format!("SELECT count(*) FROM link_challenges WHERE consumed=0 AND expires_at>=? AND {column}=?2")
            };
            let val = if key == "accountId" {
                actor.account.as_str()
            } else {
                text(&c, key)
            };
            let count: i64 = if column == "1" {
                db.query_row(&sql, [now], |r| r.get(0))?
            } else {
                db.query_row(&sql, params![now, val], |r| r.get(0))?
            };
            if count >= max {
                return Err(limited_error(120000));
            }
        }
        let nonce = random()?;
        let cid = uuid::Uuid::new_v4().to_string();
        let mut r = json!({"challengeId":cid,"accountId":actor.account,"clerkSub":actor.subject,"authorityGeneration":generation,"deviceGeneration":state["deviceGeneration"],"keyRevision":state["keyRevision"],"expiresAt":now+120000,"installationThumbprint":byte_digest(&public_key(&c["installationPublicKey"],"ed25519")?),"signingThumbprint":byte_digest(&public_key(&c["deviceSigningKey"],"ed25519")?),"encryptionThumbprint":byte_digest(&public_key(&c["deviceEncryptionKey"],"x25519")?)});
        db.execute(
            "INSERT INTO link_challenges VALUES(?,?,?,?,?,?,?,?,?,?,?,0)",
            params![
                cid,
                actor.account,
                actor.session,
                text(&c, "installationId"),
                text(&c, "deviceId"),
                text(&r, "installationThumbprint"),
                text(&r, "signingThumbprint"),
                text(&r, "encryptionThumbprint"),
                db::canonical(&json!({"request":c,"response":r})),
                byte_digest(&decode(&json!(nonce), 32)?),
                now + 120000
            ],
        )?;
        r["nonce"] = nonce.into();
        Ok(r)
    })
}
fn replay(
    db: &Connection,
    account: &str,
    request: &str,
    scope: &str,
    digest: &str,
) -> Result<Option<Value>> {
    let Some(row) = one(db, "SELECT * FROM receipts WHERE request_id=?", [request])? else {
        return Ok(None);
    };
    if row["actor"] != account || row["scope"] != scope {
        return Err(fail(
            403,
            "receipt_denied",
            "Receipt is outside the account's access.",
        ));
    }
    if row["payload_digest"] != digest {
        return Err(fail(
            409,
            "payload_conflict",
            "Request ID was already used for different content.",
        ));
    }
    let mut r: Value =
        serde_json::from_str(text(&row, "result_json")).map_err(|_| Failure::storage())?;
    r["status"] = 200.into();
    Ok(Some(r))
}
#[allow(clippy::too_many_arguments)]
fn save(
    db: &Connection,
    actor: &str,
    request: &str,
    scope: &str,
    digest: &str,
    record: &str,
    rev: i64,
    data: Value,
    status: u16,
    now: i64,
) -> Result<Value> {
    let receipt = json!({"requestId":request,"actorId":actor,"payloadDigest":digest,"status":status,"recordId":record,"recordRevision":rev,"committedAt":now});
    let r = json!({"status":status,"data":data,"receipt":receipt});
    db.execute(
        "INSERT INTO receipts VALUES(?,?,NULL,?,?,?,?,?)",
        params![request, actor, scope, digest, status, r.to_string(), now],
    )?;
    Ok(r)
}
fn session(
    db: &Connection,
    device: &str,
    generation: i64,
    authority: &str,
    now: i64,
) -> Result<Value> {
    let token = random()?;
    cleanup(db, now)?;
    db.execute(
        "UPDATE device_sessions SET revoked=1 WHERE device=?",
        [device],
    )?;
    db.execute(
        "INSERT INTO device_sessions VALUES(?,?,?,?,?,0)",
        params![
            db::digest(&token),
            device,
            generation,
            authority,
            now + 900000
        ],
    )?;
    Ok(json!({"deviceSession":token,"expiresAt":now+900000,"generation":generation}))
}
fn own(db: &Connection, actor: &Actor, device: &str) -> Result<Value> {
    authority(db)?;
    one(db,"SELECT d.* FROM devices d JOIN installations i ON i.id=d.installation JOIN accounts a ON a.id=d.account WHERE d.id=? AND d.account=? AND i.account=d.account AND i.revoked=0 AND a.blocked=0",[device,&actor.account])?.ok_or_else(missing)
}
fn link(db: &Connection, actor: &Actor, v: &Value, now: i64) -> Result<Value> {
    limited("link-peer:loopback".into(), 10, 60000, now)?;
    limited("verify".into(), 400, 2000, now)?;
    fields(
        v,
        &[
            "requestId",
            "challengeId",
            "nonce",
            "installationSignature",
            "deviceSignature",
            "descriptor",
            "rotationSignature",
        ],
    )?;
    let request = id(&v["requestId"])?;
    let cid = id(&v["challengeId"])?;
    let nonce = decode(&v["nonce"], 32)?;
    decode(&v["installationSignature"], 64)?;
    decode(&v["deviceSignature"], 64)?;
    let desc = descriptor(&v["descriptor"], false)?;
    let mut command = v.clone();
    command["requestId"] = request.clone().into();
    command["challengeId"] = cid.clone().into();
    command["descriptor"] = desc.clone();
    let hash = digest(&json!(["minarbot-device-link-v1", command]));
    let result = super::transaction(db, |db| {
        authority(db)?;
        if desc["accountId"] != actor.account {
            return Err(missing());
        }
        if let Some(row) = one(
            db,
            "SELECT account,status FROM devices WHERE id=?",
            [text(&desc, "deviceId")],
        )? {
            if row["account"] != actor.account
                || row["status"] == "revoked"
                    && one(db, "SELECT 1 FROM receipts WHERE request_id=?", [&request])?.is_some()
            {
                return Err(missing());
            }
        }
        if let Some(prior) = replay(db, &actor.account, &request, "device_enrollment", &hash)? {
            return Ok(prior);
        }
        let row = one(db, "SELECT * FROM link_challenges WHERE id=?", [&cid])?
            .filter(|r| r["account"] == actor.account && r["sid"] == actor.session)
            .ok_or_else(missing)?;
        if row["consumed"] != 0 {
            return Err(fail(409, "challenge_used", "Challenge was already used."));
        }
        if row["expires_at"].as_i64().unwrap_or(0) <= now {
            return Err(fail(410, "challenge_expired", "Challenge has expired."));
        }
        if text(&row, "nonce_digest") != byte_digest(&nonce) {
            return Err(denied());
        }
        let stored: Value =
            serde_json::from_str(text(&row, "candidate_json")).map_err(|_| Failure::storage())?;
        let c = candidate(&stored["request"])?;
        let mut r = stored["response"].clone();
        r["nonce"] = v["nonce"].clone();
        let state = candidate_state(db, actor, &c)?;
        if r["authorityGeneration"] != authority(db)?
            || desc["accountId"] != actor.account
            || desc["installationId"] != c["installationId"]
            || desc["deviceId"] != c["deviceId"]
            || desc["authorityGeneration"] != r["authorityGeneration"]
            || desc["deviceGeneration"] != state["deviceGeneration"]
            || desc["deviceGeneration"] != r["deviceGeneration"]
            || desc["keyRevision"] != state["keyRevision"]
            || desc["keyRevision"] != r["keyRevision"]
            || desc["zone"] != c["zone"]
            || desc["zoneRevision"] != c["zoneRevision"]
            || desc["signingPublicKey"] != c["deviceSigningKey"]
            || desc["encryptionPublicKey"] != c["deviceEncryptionKey"]
        {
            return Err(denied());
        }
        descriptor(&desc, true)?;
        let body = json!([
            "minarbot-link-v1",
            r["challengeId"],
            r["accountId"],
            r["clerkSub"],
            c["installationId"],
            c["deviceId"],
            r["installationThumbprint"],
            r["signingThumbprint"],
            r["encryptionThumbprint"],
            c["deviceLabel"],
            digest(&json!([
                "minarbot-device-capabilities-v1",
                c["requestedCapabilities"]
            ])),
            r["authorityGeneration"],
            r["deviceGeneration"],
            r["keyRevision"],
            c["zone"],
            c["zoneRevision"],
            c["mode"],
            c["expectedGeneration"],
            c["expectedKeyRevision"],
            r["nonce"],
            r["expiresAt"]
        ]);
        verify(
            &c["installationPublicKey"],
            &body,
            &v["installationSignature"],
        )?;
        verify(&c["deviceSigningKey"], &body, &v["deviceSignature"])?;
        if c["mode"] == "rotate" {
            let old = own(db, actor, text(&c, "deviceId"))?;
            if v.get("rotationSignature").is_none() {
                return Err(fail(
                    409,
                    "old_key_unavailable",
                    "Old key proof is required for rotation.",
                ));
            }
            verify(
                &old["signing_public_key"],
                &json!([
                    "minarbot-device-key-transition-v1",
                    old["signing_thumbprint"],
                    body
                ]),
                &v["rotationSignature"],
            )?;
        } else if v.get("rotationSignature").is_some() {
            return Err(fail(
                400,
                "invalid_proof",
                "This mode does not accept rotationSignature.",
            ));
        }
        let old = one(
            db,
            "SELECT * FROM devices WHERE id=?",
            [text(&c, "deviceId")],
        )?;
        if let Some(old) = &old {
            if one(db,"SELECT 1 FROM receipts WHERE actor=? AND scope IN('device_enrollment','device_enrollment_evidence') AND json_extract(result_json,'$.data.deviceId')=? AND json_extract(result_json,'$.data.generation')=?",params![actor.account,text(&c,"deviceId"),old["generation"].as_i64()])?.is_none(){let evidence=uuid::Uuid::new_v4().to_string();save(db,&actor.account,&evidence,"device_enrollment_evidence",&db::digest(text(old,"descriptor_json")),text(&c,"deviceId"),revision(&old["generation"],1)?,json!({"deviceId":c["deviceId"],"generation":old["generation"],"descriptor":serde_json::from_str::<Value>(text(old,"descriptor_json")).map_err(|_|Failure::storage())?}),200,now)?;}
        }
        if one(
            db,
            "SELECT 1 FROM installations WHERE id=?",
            [text(&c, "installationId")],
        )?
        .is_none()
        {
            db.execute(
                "INSERT INTO installations VALUES(?,?,?,?,1,0,1,?)",
                params![
                    text(&c, "installationId"),
                    actor.account,
                    text(&c, "installationPublicKey"),
                    text(&r, "installationThumbprint"),
                    now
                ],
            )?;
        }
        if old.is_none() {
            db.execute(
                "INSERT INTO devices VALUES(?,?,?,?,?,?,?,?,?,0,'offline',?,?,NULL,?,1)",
                params![
                    text(&c, "deviceId"),
                    text(&c, "installationId"),
                    actor.account,
                    text(&c, "deviceSigningKey"),
                    text(&c, "deviceEncryptionKey"),
                    text(&r, "signingThumbprint"),
                    text(&r, "encryptionThumbprint"),
                    desc.to_string(),
                    state["deviceGeneration"].as_i64(),
                    text(&c, "zone"),
                    c["zoneRevision"].as_i64(),
                    c["requestedCapabilities"].to_string()
                ],
            )?;
        } else {
            db.execute("UPDATE devices SET signing_public_key=?,encryption_public_key=?,signing_thumbprint=?,encryption_thumbprint=?,descriptor_json=?,generation=?,connection_generation=connection_generation+1,status='offline',zone=?,zone_revision=?,last_seen=NULL,capabilities_json=?,revision=revision+1 WHERE id=?",params![text(&c,"deviceSigningKey"),text(&c,"deviceEncryptionKey"),text(&r,"signingThumbprint"),text(&r,"encryptionThumbprint"),desc.to_string(),state["deviceGeneration"].as_i64(),text(&c,"zone"),c["zoneRevision"].as_i64(),c["requestedCapabilities"].to_string(),text(&c,"deviceId")])?;
        }
        data::append_device(db, text(&c, "deviceId"), now)?;
        db.execute("UPDATE link_challenges SET consumed=1 WHERE id=?", [&cid])?;
        let rev = old
            .as_ref()
            .and_then(|r| r["revision"].as_i64())
            .unwrap_or(0)
            + 1;
        let mut result = save(
            db,
            &actor.account,
            &request,
            "device_enrollment",
            &hash,
            text(&c, "deviceId"),
            rev,
            json!({"deviceId":c["deviceId"],"generation":state["deviceGeneration"],"keyRevision":state["keyRevision"],"descriptor":desc}),
            201,
            now,
        )?;
        db.execute(
            "UPDATE receipts SET result_json=json_set(result_json,'$.deviceLabel',?) WHERE request_id=?",
            params![text(&c, "deviceLabel"), request],
        )?;
        let secret = session(
            db,
            text(&c, "deviceId"),
            revision(&state["deviceGeneration"], 1)?,
            text(&r, "authorityGeneration"),
            now,
        )?;
        for (key, value) in secret.as_object().ok_or_else(Failure::storage)? {
            result["data"][key] = value.clone();
        }
        Ok(result)
    })?;
    if result["status"] == 201 {
        close_stream(text(&desc, "deviceId"))?;
    }
    Ok(result)
}
pub fn current(db: &Connection, p: &Principal, now: i64) -> Result<Value> {
    let authority = authority(db)?;
    let row=one(db,"SELECT d.* FROM devices d JOIN installations i ON i.id=d.installation JOIN accounts a ON a.id=d.account WHERE d.id=? AND d.account=? AND d.generation=? AND i.account=d.account AND i.revoked=0 AND a.blocked=0",params![p.device,p.account,p.generation])?.ok_or_else(denied)?;
    let desc: Value = serde_json::from_str(text(&row, "descriptor_json")).map_err(|_| denied())?;
    if ["blocked", "revoked"].contains(&text(&row, "status"))
        || p.authority != authority
        || desc["authorityGeneration"] != authority
    {
        return Err(denied());
    }
    if let Some(hash) = &p.session {
        if one(db,"SELECT 1 FROM device_sessions WHERE id_hash=? AND device=? AND generation=? AND authority_generation=? AND revoked=0 AND expires_at>?",params![hash,p.device,p.generation,p.authority,now])?.is_none(){return Err(denied());}
    }
    Ok(row)
}
#[allow(clippy::too_many_arguments)]
pub fn authenticate(
    db: &Connection,
    headers: &axum::http::HeaderMap,
    method: &str,
    path: &str,
    body: &[u8],
    request: &str,
    refresh: bool,
    now: i64,
) -> Result<Principal> {
    let raw = crate::text_header(headers, "x-minarbot-proof")
        .filter(|s| s.len() <= 16384)
        .ok_or_else(denied)?;
    let d = crate::web::parse_json(raw.as_bytes())?;
    fields(
        &d,
        &[
            "deviceId",
            "deviceGeneration",
            "authorityGeneration",
            "requestId",
            "nonce",
            "iat",
            "exp",
            "method",
            "path",
            "bodyDigest",
            "signature",
        ],
    )?;
    let device = id(&d["deviceId"])?;
    limited(format!("proof:{device}"), 40, 2000, now)?;
    limited("verify".into(), 400, 2000, now)?;
    let row = one(db, "SELECT account FROM devices WHERE id=?", [&device])?.ok_or_else(denied)?;
    if refresh {
        limited(format!("refresh:{}", text(&row, "account")), 5, 60000, now)?;
        limited("refresh-peer:loopback".into(), 10, 60000, now)?;
    }
    let iat = d["iat"]
        .as_i64()
        .filter(|n| n.unsigned_abs() <= 9_007_199_254_740_991)
        .ok_or_else(denied)?;
    let exp = d["exp"]
        .as_i64()
        .filter(|n| n.unsigned_abs() <= 9_007_199_254_740_991)
        .ok_or_else(denied)?;
    if exp <= iat
        || exp - iat > 120
        || iat as i128 * 1000 > now as i128 + 60000
        || exp as i128 * 1000 + 60000 <= now as i128
        || d["method"] != method
        || d["path"] != path
        || id(&d["requestId"])? != request
        || d["bodyDigest"] != byte_digest(body)
    {
        return Err(denied());
    }
    let nonce = decode(&d["nonce"], 32)?;
    let token = crate::text_header(headers, "authorization")
        .and_then(|s| s.strip_prefix("Device "))
        .filter(|s| {
            s.len() == 43
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        });
    if !refresh && token.is_none() {
        return Err(denied());
    }
    let p = Principal {
        device,
        account: text(&row, "account").into(),
        generation: revision(&d["deviceGeneration"], 1)?,
        authority: id(&d["authorityGeneration"])?,
        session: if refresh { None } else { token.map(db::digest) },
    };
    let row = current(db, &p, now)?;
    verify(
        &row["signing_public_key"],
        &json!([
            "minarbot-device-request-v1",
            p.device,
            p.generation,
            p.authority,
            request,
            d["nonce"],
            iat,
            exp,
            method,
            path,
            d["bodyDigest"]
        ]),
        &d["signature"],
    )?;
    super::transaction(db, |db| {
        current(db, &p, now)?;
        cleanup(db, now)?;
        let hash = byte_digest(&nonce);
        if one(
            db,
            "SELECT 1 FROM proof_replays WHERE device=? AND nonce_digest=?",
            [&p.device, &hash],
        )?
        .is_some()
        {
            return Err(fail(409, "proof_replayed", "Nonce was already used."));
        }
        let count: i64 = db.query_row("SELECT count(*) FROM proof_replays", [], |r| r.get(0))?;
        let own: i64 = db.query_row(
            "SELECT count(*) FROM proof_replays WHERE device=?",
            [&p.device],
            |r| r.get(0),
        )?;
        if count >= 262144 || own >= 4096 {
            return Err(limited_error(120000));
        }
        db.execute(
            "INSERT INTO proof_replays VALUES(?,?,?)",
            params![p.device, hash, (exp + 60) * 1000],
        )?;
        Ok(())
    })?;
    Ok(p)
}
fn envelope(result: Value) -> Response {
    crate::json_response(
        result["status"].as_u64().unwrap_or(200) as u16,
        json!({"ok":true,"data":result["data"],"revision":result["receipt"]["recordRevision"],"receipt":result["receipt"]}),
    )
}
pub(crate) fn provider_instances(device: &Value, account: &str) -> Result<Option<(Value, bool)>> {
    Ok(runtime()?
        .streams
        .get(text(device, "id"))
        .filter(|stream| {
            device["status"] == "ready"
                && account == stream.principal.account
                && device["generation"] == stream.principal.generation
                && device["connection_generation"] == stream.generation
        })
        .and_then(|stream| stream.provider_instances.clone()))
}

pub(crate) fn device_label(db: &Connection, device: &Value, owner: &str) -> Result<String> {
    let row = one(db,"SELECT json_extract(result_json,'$.deviceLabel') label FROM receipts WHERE scope='device_enrollment' AND actor=? AND json_extract(result_json,'$.data.deviceId')=? AND json_extract(result_json,'$.data.generation')=? LIMIT 1",params![owner,text(device,"id"),device["generation"].as_i64()])?;
    Ok(row
        .and_then(|row| row["label"].as_str().map(str::to_owned))
        .unwrap_or_else(|| text(device, "id").to_owned()))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/devices", any(crate::dispatch))
        .route("/api/devices/link", any(crate::dispatch))
        .route("/api/devices/link-challenges", any(crate::dispatch))
        .route("/api/devices/{id}/revoke", any(crate::dispatch))
        .route("/api/devices/{id}/session", any(crate::dispatch))
        .route("/api/devices/{id}/stream", any(crate::dispatch))
        .route("/api/devices/{id}/heartbeat", any(crate::dispatch))
        .route(
            "/api/devices/{id}/wait-reveal-results",
            any(crate::dispatch),
        )
        .route("/api/devices/{id}/wait-event-results", any(crate::dispatch))
}
pub fn host_path(path: &str) -> bool {
    let p = path.split('/').collect::<Vec<_>>();
    matches!(
        p.as_slice(),
        [
            "",
            "api",
            "devices",
            _,
            "session"
                | "stream"
                | "heartbeat"
                | "acquire"
                | "wait-secret-results"
                | "wait-reveal-results"
                | "wait-event-results"
        ]
    )
}
pub async fn human(
    state: &AppState,
    path: &str,
    method: &Method,
    v: &Value,
    actor: &Actor,
    headers: &axum::http::HeaderMap,
) -> Result<Option<Response>> {
    if path != "/api/devices" && !path.starts_with("/api/devices/") {
        return Ok(None);
    }
    state
        .auth
        .as_ref()
        .ok_or_else(Failure::storage)?
        .require_current(&state.database, actor, state.config.now())?;
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    let now = state.config.now();
    if path == "/api/devices" && *method == Method::GET {
        authority(db)?;
        let mut devices = rows(db,"SELECT id,installation,generation,connection_generation,status,zone,zone_revision,last_seen,capabilities_json,revision,descriptor_json FROM devices WHERE account=? ORDER BY id",[&actor.account])?;
        for device in &mut devices {
            let Some((accounts, complete)) = provider_instances(device, &actor.account)? else {
                continue;
            };
            let id = text(device, "id");
            let name = device_label(db, device, &actor.account)?;
            let bindings =
                data::agents::device_bindings(db, id, &actor.account, &name, &accounts, complete)?;
            device["providerInstances"] = accounts;
            device["providerInstancesComplete"] = json!(complete);
            device["agentProviders"] = bindings;
        }
        return Ok(Some(crate::json_response(
            200,
            json!({"ok":true,"data":devices}),
        )));
    }
    if *method == Method::POST
        && (path == "/api/devices/link" || path == "/api/devices/link-challenges")
    {
        if headers.contains_key("x-forwarded-host") || headers.contains_key("x-forwarded-proto") {
            return Err(fail(
                403,
                "origin_denied",
                "Forwarded authority is not allowed for enrollment.",
            ));
        }
        return Ok(Some(if path.ends_with("link-challenges") {
            crate::json_response(201, json!({"ok":true,"data":challenge(db,actor,v,now)?}))
        } else {
            envelope(link(db, actor, v, now)?)
        }));
    }
    let p = path.split('/').collect::<Vec<_>>();
    if let ["", "api", "devices", device, "revoke"] = p.as_slice() {
        if *method == Method::POST {
            let device = id(&json!(device))?;
            fields(v, &["requestId", "expectedRevision"])?;
            let request = id(&v["requestId"])?;
            let expected = revision(&v["expectedRevision"], 1)?;
            let hash = digest(&json!([
                "minarbot-device-revoke-v1",
                device,
                json!({"requestId":request,"expectedRevision":expected})
            ]));
            let scope = format!("device_revoke:{device}");
            let r = super::transaction(db, |db| {
                let row = own(db, actor, &device)?;
                if let Some(prior) = replay(db, &actor.account, &request, &scope, &hash)? {
                    return Ok(prior);
                }
                if row["revision"] != expected {
                    return Err(conflict());
                }
                db.execute("UPDATE devices SET generation=generation+1,connection_generation=connection_generation+1,status='revoked',revision=revision+1 WHERE id=?",[&device])?;
                db.execute(
                    "UPDATE device_sessions SET revoked=1 WHERE device=?",
                    [&device],
                )?;
                data::append_device(db, &device, now)?;
                save(
                    db,
                    &actor.account,
                    &request,
                    &scope,
                    &hash,
                    &device,
                    expected + 1,
                    json!({"deviceId":device,"generation":revision(&row["generation"],1)?+1,"status":"revoked"}),
                    200,
                    now,
                )
            })?;
            close_stream(&device)?;
            return Ok(Some(envelope(r)));
        }
    }
    Err(fail(
        404,
        "resource_not_found",
        "Route is not available yet.",
    ))
}
pub async fn host(state: &AppState, request: Request, request_id: &str) -> Result<Response> {
    let path = request.uri().path().to_owned();
    let operation = path.rsplit('/').next().unwrap_or("");
    let device = path.split('/').nth(3).ok_or_else(missing)?;
    let device = id(&json!(device))?;
    let method = request.method().clone();
    let headers = request.headers().clone();
    let raw = if method == Method::GET || method == Method::HEAD {
        Vec::new()
    } else {
        axum::body::to_bytes(request.into_body(), 65536)
            .await
            .map_err(|_| fail(413, "payload_too_large", "Payload exceeds the limit."))?
            .to_vec()
    };
    let v = if raw.is_empty() {
        json!({})
    } else {
        let v = crate::web::parse_json(&raw)?;
        if id(&v["requestId"])? != request_id {
            return Err(fail(
                400,
                "request_id_mismatch",
                "Header and body IDs must match.",
            ));
        }
        v
    };
    let principal = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        authenticate(
            &guard.connection,
            &headers,
            method.as_str(),
            &path,
            &raw,
            request_id,
            operation == "session",
            state.config.now(),
        )?
    };
    if principal.device != device {
        return Err(fail(
            403,
            "permission_denied",
            "Target device does not match.",
        ));
    }
    if method == Method::GET && operation == "stream" {
        return open_stream(state, principal).await;
    }
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    let now = state.config.now();
    if method == Method::POST && ["wait-reveal-results", "wait-event-results"].contains(&operation)
    {
        let result = waits::resolve(db, &principal, &v, operation == "wait-event-results", now)?;
        return Ok(crate::json_response(200, json!({"ok":true,"data":result})));
    }
    if method == Method::POST && operation == "wait-secret-results" {
        let result = data::transaction(db, || {
            crate::tasks::waits::secret_result(state, db, &principal, &v, now)
        })?;
        return crate::tasks::response(result);
    }
    if method == Method::POST && operation == "acquire" {
        let token = crate::text_header(&headers, "x-minarbot-executor").ok_or_else(|| {
            fail(
                401,
                "executor_invalid",
                "Executor token is not available yet.",
            )
        })?;
        let result = data::transaction(db, || {
            crate::tasks::execution::acquire(db, &principal, &v, token, now)
        })?;
        return crate::tasks::response(result);
    }
    if method == Method::POST && operation == "session" {
        fields(&v, &["requestId", "expectedGeneration"])?;
        let result = super::transaction(db, |db| {
            current(db, &principal, now)?;
            if revision(&v["expectedGeneration"], 1)? != principal.generation {
                return Err(conflict());
            }
            session(db, &device, principal.generation, &principal.authority, now)
        })?;
        return Ok(crate::json_response(200, json!({"ok":true,"data":result})));
    }
    if method == Method::POST && operation == "heartbeat" {
        fields(
            &v,
            &[
                "requestId",
                "connectionGeneration",
                "zone",
                "zoneRevision",
                "journalWatermark",
                "providerInstances",
                "providerInstancesComplete",
            ],
        )?;
        let mut normalized = v.clone();
        normalized["requestId"] = request_id.into();
        let connection = revision(&v["connectionGeneration"], 1)?;
        let zone = zone(&v["zone"])?;
        let zr = revision(&v["zoneRevision"], 1)?;
        revision(&v["journalWatermark"], 0)?;
        let accounts = match (
            v.get("providerInstances"),
            v.get("providerInstancesComplete"),
        ) {
            (None, None) => None,
            (Some(accounts), Some(Value::Bool(complete))) => {
                data::providers::validate_metadata(accounts)?;
                Some((accounts.clone(), *complete))
            }
            _ => {
                return Err(fail(
                    400,
                    "invalid_provider_instances",
                    "Invalid device provider accounts.",
                ))
            }
        };
        let hash = digest(&json!(["minarbot-device-heartbeat-v1", device, normalized]));
        let scope = format!("device_heartbeat:{device}");
        let mut fresh = false;
        let result = super::transaction(db, |db| {
            let row = current(db, &principal, now)?;
            if row["connection_generation"] != connection
                || runtime()?
                    .streams
                    .get(&device)
                    .is_none_or(|s| s.generation != connection)
            {
                return Err(fail(409, "connection_stale", "Connection was replaced."));
            }
            if let Some(prior) = replay(db, &principal.account, request_id, &scope, &hash)? {
                return Ok(prior);
            }
            if if row["zone"] == zone {
                row["zone_revision"] != zr
            } else {
                revision(&row["zone_revision"], 1)? + 1 != zr
            } {
                return Err(fail(
                    409,
                    "zone_conflict",
                    "Time zone revision does not match.",
                ));
            }
            let changed = row["status"] != "ready" || row["zone"] != zone;
            db.execute("UPDATE devices SET status='ready',last_seen=?,zone=?,zone_revision=?,revision=revision+? WHERE id=?",params![now,zone,zr,i64::from(changed),device])?;
            if changed {
                data::append_device(db, &device, now)?;
            }
            fresh = true;
            save(
                db,
                &principal.account,
                request_id,
                &scope,
                &hash,
                &device,
                revision(&row["revision"], 1)? + i64::from(changed),
                json!({"generation":principal.generation,"serverTime":now}),
                200,
                now,
            )
        })?;
        if fresh {
            if let Some(stream) = runtime()?
                .streams
                .get_mut(&device)
                .filter(|stream| stream.generation == connection)
            {
                stream.provider_instances = accounts;
            }
        }
        return Ok(envelope(result));
    }
    Err(fail(
        404,
        "resource_not_found",
        "Device route is unavailable.",
    ))
}
async fn open_stream(state: &AppState, p: Principal) -> Result<Response> {
    let (tx, mut rx) = mpsc::channel(4);
    let now = state.config.now();
    let generation = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        let mut rt = runtime()?;
        if rt.streams.len() >= 128 && !rt.streams.contains_key(&p.device) {
            return Err(limited_error(1000));
        }
        let next = super::transaction(db, |db| {
            let row = current(db, &p, now)?;
            let next = revision(&row["connection_generation"], 0)? + 1;
            db.execute("UPDATE devices SET connection_generation=?,status='connecting',revision=revision+1,last_seen=? WHERE id=?",params![next,now,p.device])?;
            data::append_device(db, &p.device, now)?;
            Ok(next)
        })?;
        waits::disconnect(&p.device);
        rt.streams.insert(
            p.device.clone(),
            Stream {
                principal: p.clone(),
                generation: next,
                sender: tx,
                provider_instances: None,
            },
        );
        next
    };
    let frame=json!({"v":1,"connectionGeneration":generation,"kind":"connected","body":{"deviceId":p.device,"generation":p.generation,"authorityGeneration":p.authority,"reportsProviders":true}}).to_string()+"\n";
    struct Disconnect {
        state: AppState,
        device: String,
        generation: i64,
    }
    impl Drop for Disconnect {
        fn drop(&mut self) {
            let Ok(guard) = self.state.database.lock() else {
                return;
            };
            let Ok(mut rt) = runtime() else { return };
            if rt
                .streams
                .get(&self.device)
                .is_some_and(|s| s.generation == self.generation)
            {
                rt.streams.remove(&self.device);
                waits::disconnect(&self.device);
                if mark_offline(
                    &guard.connection,
                    &self.state,
                    &self.device,
                    self.generation,
                )
                .is_err()
                {
                    retry_offline(self.state.clone(), self.device.clone(), self.generation);
                }
            }
        }
    }
    let close = Disconnect {
        state: state.clone(),
        device: p.device,
        generation,
    };
    let stream = async_stream::stream! {let _close=close;yield Ok::<_,std::convert::Infallible>(axum::body::Bytes::from(frame));while let Some(frame)=rx.recv().await{yield Ok(axum::body::Bytes::from(frame));}};
    use axum::response::IntoResponse;
    Ok((
        [(header::CONTENT_TYPE, "application/x-ndjson")],
        axum::body::Body::from_stream(stream),
    )
        .into_response())
}
#[allow(dead_code)]
pub fn send(device: &str, generation: i64, frame: String) -> Result<bool> {
    let rt = runtime()?;
    Ok(rt
        .streams
        .get(device)
        .filter(|s| s.generation == generation)
        .is_some_and(|s| s.sender.try_send(frame).is_ok()))
}
pub fn maintain(state: &AppState) -> Result<()> {
    if state.auth.is_none() {
        return Ok(());
    }
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    let now = state.config.now();
    cleanup(db, now)?;
    let mut rt = runtime()?;
    let ids = rt.streams.keys().cloned().collect::<Vec<_>>();
    for device in ids {
        let row = one(
            db,
            "SELECT status,last_seen,connection_generation FROM devices WHERE id=?",
            [&device],
        )?;
        if row.as_ref().is_none_or(|r| {
            r["last_seen"].as_i64().unwrap_or(0) < now - 30000
                || ["blocked", "revoked"].contains(&text(r, "status"))
                || rt.streams.get(&device).is_none_or(|s| {
                    r["connection_generation"] != s.generation
                        || current(db, &s.principal, now).is_err()
                })
        }) {
            rt.streams.remove(&device);
            waits::disconnect(&device);
        }
    }
    let active = json!(rt.streams.keys().collect::<Vec<_>>()).to_string();
    drop(rt);
    super::transaction(db, |db| {
        for row in rows(db,"SELECT id FROM devices WHERE status IN('ready','connecting') AND id NOT IN(SELECT value FROM json_each(?)) ORDER BY id LIMIT 128",[&active])?{db.execute("UPDATE devices SET status='offline',revision=revision+1 WHERE id=?",[text(&row,"id")])?;data::append_device(db,text(&row,"id"),now)?;}
        Ok(())
    })
}

fn mark_offline(db: &Connection, state: &AppState, device: &str, generation: i64) -> Result<()> {
    super::transaction(db, |db| {
        let changed=db.execute("UPDATE devices SET status='offline',revision=revision+1 WHERE id=? AND connection_generation=? AND status IN('ready','connecting')",params![device,generation])?;
        if changed > 0 {
            data::append_device(db, device, state.config.now())?;
        }
        Ok(())
    })
}

/// Retries a failed disconnect write soon, so a device does not look ready until the next 30 s sweep.
fn retry_offline(state: AppState, device: String, generation: i64) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    runtime.spawn(async move {
        for seconds in [1, 2, 4, 8] {
            tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
            let Ok(guard) = state.database.lock() else {
                return;
            };
            if mark_offline(&guard.connection, &state, &device, generation).is_ok() {
                return;
            }
        }
    });
}

pub fn close(state: &AppState) -> Result<()> {
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let mut rt = runtime()?;
    let devices = rt.streams.keys().cloned().collect::<Vec<_>>();
    for device in &devices {
        waits::disconnect(device);
    }
    rt.streams.clear();
    rt.buckets.clear();
    super::transaction(&guard.connection, |db| {
        for device in &devices {
            if db.execute("UPDATE devices SET status='offline',revision=revision+1 WHERE id=? AND status IN('ready','connecting')", [device])? > 0 {
                data::append_device(db, device, state.config.now())?;
            }
        }
        Ok(())
    })
}

fn close_stream(device: &str) -> Result<()> {
    runtime()?.streams.remove(device);
    waits::disconnect(device);
    Ok(())
}

#[allow(dead_code)]
pub fn enrollment_descriptor(
    state: &AppState,
    actor: &Actor,
    device: &str,
    generation: i64,
) -> Result<Value> {
    state
        .auth
        .as_ref()
        .ok_or_else(Failure::storage)?
        .require_current(&state.database, actor, state.config.now())?;
    id(&json!(device))?;
    revision(&json!(generation), 1)?;
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    if one(db, "SELECT 1 FROM authority_state WHERE id=1 AND serving_state IN('active','read_only_recovery')", [])?.is_none() {
        let mut e = fail(503, "authority_unavailable", "Authority is not available yet.");
        e.retryable = true;
        return Err(e);
    }
    if one(
        db,
        "SELECT 1 FROM devices WHERE id=? AND account=?",
        [device, &actor.account],
    )?
    .is_none()
    {
        return Err(missing());
    }
    let row = one(db, "SELECT result_json FROM receipts WHERE actor=? AND scope IN('device_enrollment','device_enrollment_evidence') AND json_extract(result_json,'$.data.deviceId')=? AND json_extract(result_json,'$.data.generation')=? ORDER BY created LIMIT 1", params![actor.account,device,generation])?.ok_or_else(missing)?;
    let result: Value =
        serde_json::from_str(text(&row, "result_json")).map_err(|_| Failure::storage())?;
    descriptor(&result["data"]["descriptor"], true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expired_enrollment_challenges_are_cleaned_in_batches() -> Result<()> {
        let db = Connection::open_in_memory()?;
        db::migrate(&db, 1, db::SCHEMA_VERSION).map_err(|_| Failure::storage())?;
        db.execute("INSERT INTO accounts(id,issuer,clerk_sub,created) VALUES('owner','issuer','subject',0)", [])?;
        db.execute_batch(
            "WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<1501)
             INSERT INTO link_challenges SELECT printf('%d',i),'owner','sid','installation','device',
             'install','sign','encrypt','{}',printf('%064d',i),1,0 FROM n;",
        )?;
        let count = || {
            db.query_row("SELECT count(*) FROM link_challenges", [], |r| {
                r.get::<_, i64>(0)
            })
        };
        cleanup(&db, 2)?;
        assert_eq!(count()?, 501);
        cleanup(&db, 2)?;
        assert_eq!(count()?, 0);
        let account = uuid::Uuid::new_v4();
        let now = crate::config::now();
        let consume = |now| limited(format!("link-account:{account}"), 5, 60000, now);
        for _ in 0..5 {
            consume(now)?;
        }
        assert_eq!(consume(now).err().map(|error| error.status), Some(429));
        consume(now + 60001)?;
        Ok(())
    }

    #[test]
    fn zones_preserve_case_and_reject_non_zone_files() -> Result<()> {
        for name in ["UTC", "Asia/Jakarta", "asia/jakarta", "Etc/GMT+7"] {
            assert_eq!(zone(&json!(name))?, name);
        }
        for name in [
            "",
            "../UTC",
            "GMT0",
            "+07:00",
            "Etc/Unknown",
            "localtime",
            "Factory",
            "zone.tab",
        ] {
            assert_eq!(
                zone(&json!(name)).err().map(|e| e.code),
                Some("invalid_zone")
            );
        }
        Ok(())
    }
}
