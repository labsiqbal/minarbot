use super::*;
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use rusqlite::OptionalExtension;
use serde::Serialize;

#[derive(Deserialize)]
#[serde(
    tag = "mode",
    rename_all = "lowercase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(super) enum IdentityConfig {
    Stub {
        tokens_path: PathBuf,
    },
    Clerk {
        issuer: String,
        jwks_path: PathBuf,
        audience: Option<String>,
    },
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Identity {
    pub issuer: String,
    pub subject: String,
    session_id: String,
    expires_at: i64,
    pub verified_mailboxes: Vec<String>,
}

pub(super) enum Verifier {
    Stub(HashMap<String, Identity>),
    Clerk {
        issuer: String,
        audience: Option<String>,
        origin: String,
        keys: HashMap<String, DecodingKey>,
    },
}

impl Verifier {
    pub(super) fn load(config: &Config) -> crate::db::Result<Self> {
        match &config.identity {
            IdentityConfig::Stub { tokens_path } => Ok(Self::Stub(serde_json::from_slice(
                &std::fs::read(tokens_path)?,
            )?)),
            IdentityConfig::Clerk {
                issuer,
                jwks_path,
                audience,
            } => {
                let url = url::Url::parse(issuer)?;
                if url.scheme() != "https" || url.origin().ascii_serialization() != *issuer {
                    return Err("Exact HTTPS identity origin required.".into());
                }
                let raw = std::fs::read(jwks_path)?;
                let jwks = parse_json(&raw).map_err(|_| "Invalid pinned JWKS.")?;
                let mut keys = HashMap::new();
                for jwk in jwks["keys"]
                    .as_array()
                    .ok_or("Pinned JWKS keys are required.")?
                {
                    let kid = text(jwk, "kid");
                    let n = text(jwk, "n");
                    let modulus = URL_SAFE_NO_PAD.decode(n)?;
                    if kid.is_empty()
                        || jwk["kty"] != "RSA"
                        || jwk.get("d").is_some()
                        || jwk.get("alg").is_some_and(|v| v != "RS256")
                        || jwk.get("use").is_some_and(|v| v != "sig")
                        || modulus.len() < 256
                        || modulus
                            .first()
                            .is_none_or(|b| *b == 0 || (modulus.len() == 256 && *b < 128))
                        || keys.contains_key(kid)
                    {
                        return Err(
                            "Unique pinned RSA signing keys of at least 2048 bits are required."
                                .into(),
                        );
                    }
                    keys.insert(
                        kid.to_owned(),
                        DecodingKey::from_rsa_components(n, text(jwk, "e"))?,
                    );
                }
                if keys.is_empty() {
                    return Err("At least one pinned signing key is required.".into());
                }
                Ok(Self::Clerk {
                    issuer: issuer.clone(),
                    audience: audience.clone(),
                    origin: format!("https://{}", config.accounts_host),
                    keys,
                })
            }
        }
    }
    fn verify(&self, token: &str, clock: i64) -> Option<Identity> {
        if token.is_empty()
            || token.len() > 16_384
            || token.chars().any(|c| c == ',' || c.is_whitespace())
        {
            return None;
        }
        match self {
            Self::Stub(tokens) => tokens
                .get(token)
                .cloned()
                .filter(|v| v.expires_at > clock && !v.session_id.is_empty()),
            Self::Clerk {
                issuer,
                audience,
                origin,
                keys,
            } => {
                let [header, claims, signature]: [&str; 3] =
                    token.split('.').collect::<Vec<_>>().try_into().ok()?;
                let part = |v: &str, max| {
                    let decoded = URL_SAFE_NO_PAD.decode(v).ok()?;
                    (!decoded.is_empty()
                        && decoded.len() <= max
                        && URL_SAFE_NO_PAD.encode(&decoded) == v)
                        .then_some(decoded)
                };
                let header = parse_json(&part(header, 2048)?).ok()?;
                let claims = parse_json(&part(claims, 12288)?).ok()?;
                part(signature, 4096)?;
                if header["alg"] != "RS256"
                    || header.get("crit").is_some()
                    || header.get("b64").is_some()
                {
                    return None;
                }
                let key = keys.get(text(&header, "kid"))?;
                let mut checks = Validation::new(Algorithm::RS256);
                checks.validate_exp = false;
                checks.validate_aud = false;
                decode::<Value>(token, key, &checks).ok()?;
                let time = |key| integer(&claims[key], 0)?.checked_mul(1000);
                let bound = |key| {
                    claims[key]
                        .as_str()
                        .filter(|v| !v.is_empty() && v.encode_utf16().count() <= 256)
                        .map(str::to_owned)
                };
                let expires_at = time("exp")?;
                if expires_at <= clock
                    || time("nbf")? > clock
                    || time("iat")? > clock
                    || time("iat")? > expires_at
                    || claims["iss"] != *issuer
                    || claims["azp"] != *origin
                    || claims["sts"] == "pending"
                    || audience.as_ref().is_some_and(|aud| {
                        claims["aud"] != *aud
                            && !claims["aud"]
                                .as_array()
                                .is_some_and(|v| v.iter().any(|v| v == aud))
                    })
                {
                    return None;
                }
                let verified_mailboxes =
                    if claims["email_verified"] == true && claims["email"].is_string() {
                        vec![authority::mailbox(&claims["email"]).ok()?]
                    } else {
                        Vec::new()
                    };
                Some(Identity {
                    issuer: issuer.clone(),
                    subject: bound("sub")?,
                    session_id: bound("sid")?,
                    expires_at,
                    verified_mailboxes,
                })
            }
        }
    }
}

pub(super) struct Actor {
    pub account: String,
    pub project: Option<String>,
    pub csrf: String,
    pub expires_at: i64,
    pub identity: Identity,
    digest: String,
}

fn invalid() -> Failure {
    Failure::new(401, "session_invalid", "Session is invalid or expired.")
}
fn forbidden() -> Failure {
    Failure::new(
        403,
        "origin_or_csrf_denied",
        "Origin or CSRF proof is not allowed.",
    )
}
fn opaque_value(v: &Value) -> Result<&str> {
    v.as_str()
        .filter(|s| encoded(s, 32).is_some())
        .ok_or_else(invalid)
}
fn check_denial(db: &Connection, identity: &Identity) -> Result<()> {
    if db
        .prepare("SELECT 1 FROM hub_browser_denials WHERE issuer=? AND sid=?")?
        .exists(params![identity.issuer, identity.session_id])?
    {
        return Err(invalid());
    }
    Ok(())
}
pub(super) fn origin(headers: &HeaderMap, host: &str, required: bool) -> Result<()> {
    match single(headers, "origin")? {
        Some(v) if v == format!("https://{host}") => Ok(()),
        None if !required => Ok(()),
        _ => Err(forbidden()),
    }
}
fn cookie(headers: &HeaderMap, name: &str) -> Result<String> {
    let prefix = format!("{name}=");
    let mut values = single(headers, "cookie")?
        .unwrap_or_default()
        .split(';')
        .filter_map(|part| part.trim().strip_prefix(&prefix));
    let value = values
        .next()
        .filter(|s| encoded(s, 32).is_some())
        .ok_or_else(invalid)?;
    if values.next().is_some() {
        return Err(invalid());
    }
    Ok(digest(value))
}
fn set_cookie(response: &mut Response, name: &str, value: &str, seconds: i64) -> Result<()> {
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "{name}={value}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={}",
            seconds.max(0)
        ))
        .map_err(|_| Failure::storage())?,
    );
    Ok(())
}
fn csrf(headers: &HeaderMap, value: &str) -> Result<()> {
    use subtle::ConstantTimeEq;
    if !single(headers, "x-csrf-token")?
        .is_some_and(|v| bool::from(v.as_bytes().ct_eq(value.as_bytes())))
    {
        return Err(forbidden());
    }
    Ok(())
}
pub(super) fn read(
    hub: &Hub,
    db: &Connection,
    host: &str,
    digest: &str,
    clock: i64,
) -> Result<Actor> {
    fn record(db: &Connection, host: &str, digest: &str, clock: i64) -> Result<Value> {
        one(db, "SELECT * FROM hub_browser_sessions WHERE host=? AND digest=? AND expires_at>? AND generation=?",
            params![host, digest, clock, generation(db)?])?.ok_or_else(invalid)
    }
    let row = record(db, host, digest, clock)?;
    let identity: Identity =
        serde_json::from_str(text(&row, "identity_json")).map_err(|_| Failure::storage())?;
    let account = authority::account(db, text(&row, "account_id"))?;
    if identity.expires_at <= clock
        || account["issuer"] != identity.issuer
        || account["subject"] != identity.subject
    {
        return Err(invalid());
    }
    check_denial(db, &identity)?;
    let project = hub.registered(db, host)?;
    if row["project_id"].as_str() != project.as_deref() {
        return Err(invalid());
    }
    if project.is_some() {
        let parent = record(
            db,
            &hub.config.accounts_host,
            text(&row, "parent_digest"),
            clock,
        )?;
        if parent["account_id"] != row["account_id"]
            || parent["identity_json"] != row["identity_json"]
            || !parent["project_id"].is_null()
            || !parent["parent_digest"].is_null()
        {
            return Err(invalid());
        }
    }
    Ok(Actor {
        account: text(&row, "account_id").to_owned(),
        project,
        digest: digest.to_owned(),
        csrf: text(&row, "csrf").to_owned(),
        expires_at: row["expires_at"].as_i64().ok_or_else(Failure::storage)?,
        identity,
    })
}
pub(super) fn get(
    hub: &Hub,
    db: &Connection,
    host: &str,
    headers: &HeaderMap,
    mutation: bool,
) -> Result<Actor> {
    let actor = read(hub, db, host, &cookie(headers, "__Host-session")?, now())?;
    if mutation {
        csrf(headers, &actor.csrf)?;
    }
    Ok(actor)
}
fn remove(db: &Connection, digest: &str) -> Result<()> {
    db.execute(
        "DELETE FROM hub_browser_sessions WHERE parent_digest=? OR digest=?",
        [digest, digest],
    )?;
    Ok(())
}
fn prune(db: &Connection, clock: i64) -> Result<()> {
    db.execute("DELETE FROM hub_browser_codes WHERE expires_at<=?", [clock])?;
    db.execute(
        "DELETE FROM hub_browser_starts WHERE expires_at<=? OR generation!=?",
        params![clock, generation(db)?],
    )?;
    db.execute(
        "DELETE FROM hub_browser_sessions WHERE expires_at<=? OR generation!=?",
        params![clock, generation(db)?],
    )?;
    Ok(())
}
fn mint(
    hub: &Hub,
    db: &Connection,
    host: &str,
    headers: &HeaderMap,
    actor: &Actor,
    parent: bool,
) -> Result<Response> {
    if let Ok(old) = cookie(headers, "__Host-session") {
        if read(hub, db, host, &old, now()).is_ok() {
            remove(db, &old)?;
        }
    }
    let token = opaque()?;
    let csrf = opaque()?;
    let expires_at = actor.expires_at.min(now() + 900_000);
    if expires_at <= now() {
        return Err(invalid());
    }
    db.execute(
        "INSERT INTO hub_browser_sessions VALUES(?,?,?,?,?,?,?,?,?)",
        params![
            digest(&token),
            host,
            hub.registered(db, host)?,
            actor.account,
            serde_json::to_string(&actor.identity).map_err(|_| Failure::storage())?,
            csrf,
            expires_at,
            parent.then_some(&actor.digest),
            generation(db)?
        ],
    )?;
    let mut response = json_ok(json!({"csrf":csrf,"expiresAt":expires_at}));
    set_cookie(
        &mut response,
        "__Host-session",
        &token,
        (expires_at - now()) / 1000,
    )?;
    set_cookie(&mut response, "__Host-login", "", 0)?;
    Ok(response)
}

pub(super) fn bootstrap(state: Option<&str>) -> Result<Response> {
    let nonce = opaque()?;
    let mut response = json_ok(json!({"csrf":nonce,"identity":"central","state":state}));
    set_cookie(&mut response, "__Host-login", &nonce, 300)?;
    Ok(response)
}
pub(super) fn start(hub: &Hub, db: &Connection, host: &str) -> Result<Response> {
    let clock = now();
    prune(db, clock)?;
    let project = hub.registered(db, host)?.ok_or_else(invalid)?;
    let state = opaque()?;
    let browser = opaque()?;
    db.execute(
        "INSERT INTO hub_browser_starts VALUES(?,?,?,?,?,?)",
        params![
            digest(&state),
            digest(&browser),
            host,
            project,
            generation(db)?,
            clock + 300_000
        ],
    )?;
    let mut response = json_ok(
        json!({"state":state,"login":format!("https://{}/login?state={state}", hub.config.accounts_host)}),
    );
    set_cookie(&mut response, "__Host-login", &browser, 300)?;
    Ok(response)
}
pub(super) fn login(
    hub: &Hub,
    db: &Connection,
    host: &str,
    headers: &HeaderMap,
    input: &Value,
) -> Result<Response> {
    closed(input, &[])?;
    let login = cookie(headers, "__Host-login")?;
    let proof = single(headers, "x-csrf-token")?.ok_or_else(forbidden)?;
    if digest(proof) != login {
        return Err(forbidden());
    }
    let token = single(headers, "authorization")?
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(invalid)?;
    let clock = now();
    let identity = hub.identity.verify(token, clock).ok_or_else(invalid)?;
    check_denial(db, &identity)?;
    let account = db
        .query_row(
            "SELECT id FROM accounts WHERE issuer=? AND subject=?",
            params![identity.issuer, identity.subject],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| Failure::new(401, "account_unavailable", "Pilot invitation required."))?;
    authority::account(db, &account)?;
    prune(db, clock)?;
    let actor = Actor {
        account,
        expires_at: identity.expires_at,
        identity,
        project: None,
        csrf: String::new(),
        digest: String::new(),
    };
    mint(hub, db, host, headers, &actor, false)
}
pub(super) fn handoff(
    hub: &Hub,
    db: &Connection,
    host: &str,
    headers: &HeaderMap,
    input: &Value,
) -> Result<Response> {
    closed(input, &["state"])?;
    let actor = get(hub, db, host, headers, true)?;
    let state = opaque_value(&input["state"])?;
    let state_digest = digest(state);
    let row = one(
        db,
        "SELECT * FROM hub_browser_starts WHERE state_digest=? AND expires_at>? AND generation=?",
        params![state_digest, now(), generation(db)?],
    )?
    .ok_or_else(invalid)?;
    let target = text(&row, "host");
    if hub.registered(db, target)?.as_deref() != row["project_id"].as_str() || target == host {
        return Err(invalid());
    }
    let code = opaque()?;
    let expires = (now() + 60_000).min(actor.expires_at);
    db.execute("INSERT INTO hub_browser_codes VALUES(?,?,?,?) ON CONFLICT(state_digest) DO UPDATE SET digest=excluded.digest,parent_digest=excluded.parent_digest,expires_at=excluded.expires_at",
        params![digest(&code), state_digest, actor.digest, expires])?;
    Ok(json_ok(
        json!({"callback":format!("https://{target}/auth/callback#code={code}&state={state}"),"expiresAt":expires}),
    ))
}
pub(super) fn redeem(
    hub: &Hub,
    db: &Connection,
    host: &str,
    headers: &HeaderMap,
    input: &Value,
) -> Result<Response> {
    closed(input, &["state", "code"])?;
    let state = opaque_value(&input["state"])?;
    csrf(headers, state)?;
    let state_digest = digest(state);
    let row = one(db, "SELECT s.project_id,c.parent_digest FROM hub_browser_starts s JOIN hub_browser_codes c USING(state_digest)
        WHERE c.digest=? AND s.state_digest=? AND s.host=? AND s.expires_at>? AND c.expires_at>? AND s.generation=? AND s.browser_digest=?",
        params![digest(opaque_value(&input["code"])?), state_digest, host, now(), now(), generation(db)?, cookie(headers, "__Host-login")?])?.ok_or_else(invalid)?;
    if hub.registered(db, host)?.as_deref() != row["project_id"].as_str() {
        return Err(invalid());
    }
    let parent = read(
        hub,
        db,
        &hub.config.accounts_host,
        text(&row, "parent_digest"),
        now(),
    )?;
    let response = mint(hub, db, host, headers, &parent, true)?;
    db.execute(
        "DELETE FROM hub_browser_starts WHERE state_digest=?",
        [state_digest],
    )?;
    Ok(response)
}
pub(super) fn logout(db: &Connection, actor: &Actor, input: &Value) -> Result<Response> {
    closed(input, &["global"])?;
    let global = input["global"]
        .as_bool()
        .ok_or_else(|| Failure::new(400, "invalid_logout", "Choose host or global logout."))?;
    if global {
        db.execute(
            "INSERT OR IGNORE INTO hub_browser_denials VALUES(?,?)",
            params![actor.identity.issuer, actor.identity.session_id],
        )?;
        db.execute("DELETE FROM hub_browser_sessions WHERE json_extract(identity_json,'$.issuer')=? AND json_extract(identity_json,'$.sessionId')=?",
            params![actor.identity.issuer, actor.identity.session_id])?;
    } else {
        remove(db, &actor.digest)?;
    }
    let mut response =
        json_ok(json!({"state":if global {"upstream_pending"} else {"host_revoked"}}));
    set_cookie(&mut response, "__Host-session", "", 0)?;
    set_cookie(&mut response, "__Host-login", "", 0)?;
    Ok(response)
}
