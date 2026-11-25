use super::{error, Result};
use crate::{
    data::{agents as local_agents, providers},
    files::{one, rows, text},
    updates::PeerError,
    web::Failure,
    AppState,
};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use futures_util::{stream, StreamExt, TryStreamExt};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    time::{Duration, Instant},
};

const SCOPES: [&str; 3] = ["agents:read", "agents:message", "history:read"];
fn hash(v: &Value) -> String {
    digest(v.to_string().as_bytes())
}
fn digest(v: &[u8]) -> String {
    format!("{:x}", Sha256::digest(v))
}
fn random<const N: usize>() -> Result<[u8; N]> {
    let mut v = [0; N];
    getrandom::getrandom(&mut v).map_err(|_| Failure::storage())?;
    Ok(v)
}
fn fields(v: &Value, keys: &[&str]) -> Result<()> {
    if !crate::files::keys(v, keys) {
        return Err(error(400, "Invalid machine request."));
    }
    Ok(())
}
fn field(v: &Value, max: usize) -> Result<String> {
    let s = v
        .as_str()
        .filter(|s| {
            !s.trim().is_empty()
                && s.encode_utf16().count() <= max
                && !s.chars().any(|c| c < ' ' || c == '\u{7f}')
        })
        .ok_or_else(|| error(400, "Invalid machine field."))?;
    Ok(s.trim().into())
}
fn request_id(v: &Value) -> Result<String> {
    v.as_str()
        .filter(|v| crate::web::id(v).is_some())
        .map(str::to_owned)
        .ok_or_else(|| error(400, "Invalid request ID."))
}
fn credential(v: &Value) -> Result<String> {
    v.as_str()
        .filter(|s| (16..=4096).contains(&s.len()) && token_syntax(s))
        .map(str::to_owned)
        .ok_or_else(|| error(400, "Invalid peer credential."))
}
fn token_syntax(s: &str) -> bool {
    !s.is_empty()
        && s.trim_end_matches('=')
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._~+/-".contains(&b))
}
fn invalid() -> Failure {
    error(400, "Pairing request is invalid or expired.")
}
fn code(v: &Value) -> Result<String> {
    let s = v.as_str().unwrap_or("").to_ascii_uppercase();
    if s.len() != 12
        || !s
            .bytes()
            .all(|b| b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ".contains(&b))
    {
        return Err(invalid());
    }
    Ok(s)
}
// Peers know this machine by its first allowed origin, like Node's getOrigin.
pub(crate) fn public_origin(s: &AppState) -> &str {
    s.config
        .allowed_origins
        .first()
        .map_or(s.origin.as_str(), String::as_str)
}
pub fn machine_origin(v: &str) -> Result<String> {
    if v.len() > 2048 || v.trim() != v {
        return Err(error(400, "Invalid machine origin."));
    }
    let u = url::Url::parse(v).map_err(|_| error(400, "Invalid machine origin."))?;
    if !(u.scheme() == "https"
        || u.scheme() == "http"
            && matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
        || !u.username().is_empty()
        || u.password().is_some()
        || u.path() != "/"
        || u.query().is_some()
        || u.fragment().is_some()
        || v.contains(['?', '#', '\\'])
        || u.origin().ascii_serialization() != v.strip_suffix('/').unwrap_or(v)
    {
        return Err(error(
            400,
            "Use an HTTPS origin without credentials, a path, query, or fragment.",
        ));
    }
    Ok(u.origin().ascii_serialization())
}
fn descriptor(v: &Value) -> Result<Value> {
    fields(v, &["id", "name", "version", "origin"])?;
    Ok(
        json!({"id":request_id(&v["id"])?,"name":field(&v["name"],120)?,"version":field(&v["version"],64)?,"origin":machine_origin(text(v,"origin"))?}),
    )
}
pub fn initialize(db: &Connection) -> crate::db::Result<()> {
    db.execute_batch(include_str!("machines.sql"))?;
    let host = rustix::system::uname();
    let name = host.nodename().to_string_lossy();
    let name = name.trim().chars().take(120).collect::<String>();
    db.execute(
        "INSERT OR IGNORE INTO environment_identity VALUES(1,?,?)",
        params![uuid::Uuid::new_v4().to_string(), name],
    )?;
    Ok(())
}
fn with_db<T>(s: &AppState, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let g = s.database.lock().map_err(|_| Failure::storage())?;
    f(&g.connection)
}
fn tx<T>(s: &AppState, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    with_db(s, |db| super::transaction(db, f))
}
fn row(s: &AppState, id: &str) -> Result<Value> {
    with_db(s, |db| {
        one(
            db,
            "SELECT * FROM paired_machines WHERE environment_id=?",
            [id],
        )
    })?
    .ok_or_else(|| error(404, "Paired machine not found."))
}
pub fn version() -> String {
    crate::updates::version()
}
pub fn environment(s: &AppState) -> Result<Value> {
    let mut v = with_db(s, |db| {
        one(
            db,
            "SELECT environment_id AS id,name FROM environment_identity WHERE singleton=1",
            [],
        )
    })?
    .ok_or_else(Failure::storage)?;
    v["version"] = version().into();
    v["origin"] = machine_origin(public_origin(s))?.into();
    Ok(v)
}
fn visible(s: &AppState, r: &Value) -> Result<Value> {
    let cached: Value = serde_json::from_str(text(r, "agents")).map_err(|_| Failure::storage())?;
    let allowed = with_db(s, |db| {
        one(
            db,
            "SELECT allowed FROM machine_update_policy WHERE environment_id=?",
            [text(r, "environment_id")],
        )
    })?
    .is_some_and(|v| v["allowed"] == 1);
    let mut v = json!({"id":r["environment_id"],"environmentId":r["environment_id"],"name":r["name"],"origin":r["origin"],"status":r["status"],"version":r["version"],"lastSeen":r["last_seen"],"agentCount":cached.as_array().map_or(0,|a|a.len()),"agents":cached,"pairingState":r["state"],"allowAgentMessaging":r["allow_agent_messaging"]==1,"allowUpdates":allowed});
    if let Some(policy) = with_db(s, |db| {
        one(
            db,
            "SELECT allowed FROM computer_peer_messaging WHERE token_id=?",
            [text(r, "local_token_id")],
        )
    })? {
        v["remoteAgentMessaging"] = json!(policy["allowed"] == 1);
    }
    if let Some(report) = with_db(s, |db| {
        one(
            db,
            "SELECT report FROM machine_provider_reports WHERE environment_id=?",
            [text(r, "environment_id")],
        )
    })? {
        let report = crate::web::parse_json(text(&report, "report").as_bytes())?;
        v["providerInstances"] = report["providerInstances"].clone();
        if let Some(bindings) = report["agentProviderInstances"].as_object() {
            v["agentProviders"] = json!(bindings
                .iter()
                .map(|(id, account)| (
                    id.clone(),
                    local_agents::device_provider_status(text(r, "name"), account)
                ))
                .collect::<serde_json::Map<String, Value>>());
        }
    }
    Ok(v)
}
pub fn list(s: &AppState) -> Result<Vec<Value>> {
    with_db(s, |db| {
        rows(
            db,
            "SELECT * FROM paired_machines ORDER BY name,environment_id",
            [],
        )
    })?
    .iter()
    .map(|r| visible(s, r))
    .collect()
}
fn key(s: &AppState, create: bool) -> Result<Vec<u8>> {
    let path = minarbot_launcher::files::sidecar(&s.config.database, ".peer-key");
    if create {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut f) => f
                .write_all(&random::<32>()?)
                .map_err(|_| Failure::storage())?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(Failure::storage()),
        }
    }
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
        .map_err(|_| {
            error(
                409,
                "Machine key is unavailable. Unpair and pair this machine again.",
            )
        })?;
    let m = f.metadata().map_err(|_| Failure::storage())?;
    if !m.is_file() || m.permissions().mode() & 0o77 != 0 || m.len() != 32 {
        return Err(error(
            409,
            "Machine key is unavailable. Unpair and pair this machine again.",
        ));
    }
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes).map_err(|_| Failure::storage())?;
    Ok(bytes)
}
fn crypt(s: &AppState, value: &str, aad: &str, encrypt: bool) -> Result<String> {
    let bytes = key(s, encrypt)?;
    let key =
        LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &bytes).map_err(|_| Failure::storage())?);
    if encrypt {
        let iv = random::<12>()?;
        let mut data = value.as_bytes().to_vec();
        key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(iv),
            Aad::from(aad.as_bytes()),
            &mut data,
        )
        .map_err(|_| Failure::storage())?;
        Ok(STANDARD.encode([iv.as_slice(), &data].concat()))
    } else {
        let mut data = STANDARD.decode(value).map_err(|_| {
            error(
                409,
                "Machine key is unavailable. Unpair and pair this machine again.",
            )
        })?;
        if data.len() < 28 {
            return Err(error(
                409,
                "Machine key is unavailable. Unpair and pair this machine again.",
            ));
        }
        let iv: [u8; 12] = data[..12].try_into().map_err(|_| Failure::storage())?;
        let plain = key
            .open_in_place(
                Nonce::assume_unique_for_key(iv),
                Aad::from(aad.as_bytes()),
                &mut data[12..],
            )
            .map_err(|_| {
                error(
                    409,
                    "Machine key is unavailable. Unpair and pair this machine again.",
                )
            })?;
        String::from_utf8(plain.to_vec()).map_err(|_| Failure::storage())
    }
}
pub async fn bridge(s: &AppState, operation: &str, args: Value) -> Result<Value> {
    let b = s
        .config
        .computer
        .test_computer_bridge
        .as_ref()
        .ok_or_else(Failure::storage)?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(
            if operation == "peer-http" && text(&args, "url").ends_with("/api/peer/updates/apply") {
                UPDATE_TIMEOUT + Duration::from_secs(5)
            } else {
                Duration::from_secs(15)
            },
        )
        .build()
        .map_err(|_| Failure::storage())?;
    let value: Value = client
        .post(&b.origin)
        .bearer_auth(&b.token)
        .json(&json!({"operation":operation,"args":args}))
        .send()
        .await
        .map_err(|_| Failure::storage())?
        .json()
        .await
        .map_err(|_| Failure::storage())?;
    if let Some(e) = value.get("error") {
        return Err(error(
            e["status"].as_u64().unwrap_or(503) as u16,
            text(e, "message"),
        ));
    }
    Ok(value["value"].clone())
}
pub(crate) async fn now(s: &AppState) -> Result<i64> {
    if s.config.computer.test_computer_bridge.is_some() {
        let v = bridge(s, "clock", json!({})).await?;
        Ok(v.as_i64().unwrap_or_else(|| s.config.now()))
    } else {
        Ok(s.config.now())
    }
}
async fn issue(s: &AppState, name: &str) -> Result<Value> {
    if s.config.computer.test_computer_bridge.is_some() {
        let v = bridge(s, "issue", json!({"name":name,"scopes":SCOPES})).await?;
        if !v.is_null() {
            return Ok(v);
        }
    }
    let token = URL_SAFE_NO_PAD.encode(random::<32>()?);
    let id = uuid::Uuid::new_v4().to_string();
    let scopes = json!(["agents:message", "agents:read", "history:read"]).to_string();
    with_db(s, |db| {
        db.execute("INSERT INTO mcp_server_tokens(id,token_hash,kind,name,scopes,created,request_digest) VALUES(?,?,'peer',?,?,?,?)",params![id,digest(token.as_bytes()),name,scopes,s.config.now(),digest(json!({"name":name,"scopes":SCOPES}).to_string().as_bytes())])?;
        Ok(())
    })?;
    Ok(json!({"id":id,"token":token}))
}
async fn revoke(s: &AppState, id: &str) -> Result<()> {
    with_db(s, |db| {
        db.execute("INSERT OR IGNORE INTO peer_revocations VALUES(?)", [id])?;
        Ok(())
    })?;
    if s.config.computer.test_computer_bridge.is_some() {
        bridge(s, "revoke", json!({"id":id})).await?;
    }
    with_db(s, |db| {
        db.execute(
            "UPDATE mcp_server_tokens SET revoked=coalesce(revoked,?) WHERE id=?",
            params![s.config.now(), id],
        )?;
        db.execute("DELETE FROM peer_revocations WHERE token_id=?", [id])?;
        Ok(())
    })
}
async fn authorize(s: &AppState, header: Option<&str>) -> Result<Value> {
    let token = header
        .and_then(|h| h.strip_prefix("Bearer "))
        .filter(|s| s.len() <= 4096 && token_syntax(s))
        .ok_or_else(|| error(401, "Unauthorized peer request."))?;
    let mut auth = if s.config.computer.test_computer_bridge.is_some() {
        bridge(s, "verify", json!({"token":token}))
            .await
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    if auth.is_null() {
        auth=with_db(s,|db|one(db,"SELECT id,scopes FROM mcp_server_tokens WHERE token_hash=? AND revoked IS NULL AND (expires IS NULL OR expires>?)",params![digest(token.as_bytes()),s.config.now()]))?.unwrap_or(Value::Null);
        if let Some(raw) = auth["scopes"].as_str() {
            auth["scopes"] = serde_json::from_str(raw).unwrap_or(Value::Null);
        }
    }
    if !auth["scopes"]
        .as_array()
        .is_some_and(|v| v.contains(&json!("agents:read")))
        || with_db(s, |db| {
            one(
                db,
                "SELECT 1 FROM peer_revocations WHERE token_id=?",
                [text(&auth, "id")],
            )
        })?
        .is_some()
    {
        return Err(error(401, "Unauthorized peer request."));
    }
    auth["token"] = token.into();
    Ok(auth)
}
fn no_secrets(value: &Value, tokens: &[&str]) -> Result<()> {
    let raw = value.to_string();
    if tokens.iter().any(|t| !t.is_empty() && raw.contains(t)) {
        return Err(error(
            502,
            "Remote response conflicts with peer credentials.",
        ));
    }
    Ok(())
}
const UPDATE_STATUS: &str = "/api/peer/updates";
const UPDATE_APPLY: &str = "/api/peer/updates/apply";
const PEER_AGENTS: &str = "/api/peer/agents";
const PEER_PROBE_HEADER: &str = "x-minarbot-peer-probe";
const MAX_STALE_AGE_MS: i64 = 90_000;
const UPDATE_TIMEOUT: Duration = Duration::from_secs(300);
const RELEASE_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
fn update_deadline(bytes: u64) -> Duration {
    // Allow for packaging/verification, reconnects and a shared link below 30 KB/s.
    UPDATE_TIMEOUT + Duration::from_secs(bytes.div_ceil(8 * 1024))
}
fn rejection(status: u16, update: bool, value: Option<&Value>) -> Failure {
    if update {
        if let Some(failure) = value.and_then(|v| {
            PeerError::from_code(text(v, "code"))
                .or_else(|| crate::updates::github_remote_error(text(v, "code"), text(v, "error")))
        }) {
            return failure;
        }
        if [401, 403].contains(&status) {
            return PeerError::PermissionRevoked.failure();
        }
        if status >= 500 && value.is_none_or(|v| v["code"] != "storage_failed") {
            return PeerError::Unconfirmed.failure();
        }
    }
    let message = if update {
        let reason =
            value.and_then(|v| v["error"].as_str()).filter(|v| {
                matches!(*v,
            "Another update is already running."
            | "Release version is not an update."
            | "Insufficient disk space for update snapshot and trial."
            | "Request ID was already used for another release."
            | "Receiving server restarted during the update."
            | "Receiving machine is not running under the Minarbot launcher."
            | "Peer release download failed integrity, permission, time or size checks."
            | "Machine updates require permission from both owners in Settings > Machines.")
            });
        format!(
            "Remote machine rejected the update (HTTP {status}).{}",
            reason.map(|r| format!(" {r}")).unwrap_or_default()
        )
    } else {
        "Remote machine rejected the request.".to_owned()
    };
    error(
        if [401, 403].contains(&status) {
            401
        } else {
            502
        },
        message,
    )
}
async fn remote(
    s: &AppState,
    origin: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&Value>,
) -> Result<Value> {
    remote_request(s, origin, path, token, body, None).await
}

async fn remote_request(
    s: &AppState,
    origin: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&Value>,
    peer_update_timeout: Option<Duration>,
) -> Result<Value> {
    let client = peer_client()?;
    let mode = peer_update_timeout.map_or(RemoteRequest::Standard, RemoteRequest::Update);
    remote_with_client(s, origin, path, token, body, mode, &client).await
}

fn peer_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| Failure::storage())
}

#[derive(Clone, Copy)]
enum RemoteRequest {
    Standard,
    Probe,
    Update(Duration),
}

fn probe_timeout() -> Failure {
    Failure::new(504, "peer_check_timeout", "Peer status check timed out.")
}

async fn remote_with_client(
    s: &AppState,
    origin: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&Value>,
    mode: RemoteRequest,
    client: &reqwest::Client,
) -> Result<Value> {
    let track_transport = matches!(mode, RemoteRequest::Update(_));
    let probe = matches!(mode, RemoteRequest::Probe);
    let timeout = match mode {
        RemoteRequest::Update(timeout) => timeout,
        _ => Duration::from_secs(10),
    };
    let update = path == UPDATE_APPLY;
    let unavailable = || {
        if update {
            return PeerError::Unconfirmed.failure();
        }
        error(
            502,
            "Remote machine is unavailable or exceeded its time, size, or JSON limits.",
        )
    };
    let started = Instant::now();
    let interrupted = |phase: &str, failure: Failure| {
        if track_transport && matches!(path, "/api/environment" | UPDATE_APPLY) {
            eprintln!(
                "peer update request path={path} phase={phase} elapsed_ms={} code={}",
                started.elapsed().as_millis(),
                failure.code
            );
        }
        if update {
            PeerError::Unconfirmed.failure()
        } else if path == "/api/environment" && failure.code == PeerError::Timeout.code() {
            Failure::new(
                504,
                "peer_precheck_timeout",
                "Remote machine identity check timed out.",
            )
        } else {
            failure
        }
    };
    let operation = async {
        let mut req = if let Some(body) = body {
            client.post(format!("{origin}{path}")).json(body)
        } else {
            client.get(format!("{origin}{path}"))
        }
        .timeout(timeout);
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }
        if path == PEER_AGENTS {
            req = req
                .header("X-Minarbot-Messaging-Policy", "1")
                .header("X-Minarbot-Provider-Accounts", "1");
            if probe {
                req = req.header(PEER_PROBE_HEADER, "1");
            }
        }
        let mut response = peer_http(s, req).await.map_err(|failure| {
            if track_transport {
                interrupted("headers", failure)
            } else if probe && failure.status == 504 {
                probe_timeout()
            } else {
                unavailable()
            }
        })?;
        let status = response.status();
        if probe && status == reqwest::StatusCode::GATEWAY_TIMEOUT {
            return Err(probe_timeout());
        }
        if !status.is_success() && !update {
            return Err(rejection(status.as_u16(), false, None));
        }
        if response.url().origin().ascii_serialization() != origin
            || response.content_length().is_some_and(|n| n > 128 * 1024)
            || !response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("application/json"))
        {
            return Err(if !status.is_success() {
                rejection(status.as_u16(), update, None)
            } else {
                unavailable()
            });
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|err| {
            if track_transport {
                interrupted("body", PeerError::network(err))
            } else if probe && err.is_timeout() {
                probe_timeout()
            } else {
                Failure::storage()
            }
        })? {
            if bytes.len() + chunk.len() > 128 * 1024 {
                return Err(unavailable());
            }
            bytes.extend_from_slice(&chunk);
        }
        let value = serde_json::from_slice(&bytes);
        if !status.is_success() {
            return Err(rejection(status.as_u16(), update, value.as_ref().ok()));
        }
        value.map_err(|_| unavailable())
    };
    tokio::time::timeout(timeout, operation)
        .await
        .map_err(|_| {
            if track_transport {
                interrupted("deadline", PeerError::Timeout.failure())
            } else if probe {
                probe_timeout()
            } else {
                unavailable()
            }
        })?
}
fn agents(value: &Value) -> Result<Value> {
    fields(
        value,
        &[
            "agents",
            "messagingAllowed",
            "providerInstances",
            "agentProviderInstances",
        ],
    )?;
    if value
        .get("messagingAllowed")
        .is_some_and(|v| !v.is_boolean())
    {
        return Err(error(502, "Invalid remote messaging policy."));
    }
    let list = value["agents"]
        .as_array()
        .filter(|v| v.len() <= 500)
        .ok_or_else(|| error(502, "Invalid remote agent list."))?;
    let mut result = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for a in list {
        fields(a, &["id", "name", "role", "model", "status"])?;
        let id = request_id(&a["id"])?;
        if !ids.insert(id.clone()) {
            return Err(error(502, "Invalid remote agent list."));
        }
        result.push(json!({"id":id,"name":field(&a["name"],200)?,"role":text(a,"role").chars().filter(|c|!c.is_control() && *c!='\u{2028}'&&*c!='\u{2029}').collect::<String>().trim().chars().take(60).collect::<String>(),"model":a["model"].as_str().filter(|s|s.len()<=200).unwrap_or(""),"status":field(&a["status"],32)?}));
    }
    Ok(json!(result))
}
fn provider_report(roster: &Value, agents: &Value) -> Result<Option<Value>> {
    let invalid = || error(502, "Invalid remote provider report.");
    let (accounts, bindings) = match (
        roster.get("providerInstances"),
        roster.get("agentProviderInstances"),
    ) {
        (None, None) => return Ok(None),
        (Some(accounts), Some(Value::Object(bindings))) => (accounts, bindings),
        _ => return Err(invalid()),
    };
    providers::validate_metadata(accounts).map_err(|_| invalid())?;
    let account_ids = accounts
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["id"].as_str())
        .collect::<std::collections::HashSet<_>>();
    let agent_ids = agents
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["id"].as_str())
        .collect::<std::collections::HashSet<_>>();
    if bindings.len() > agent_ids.len()
        || bindings.iter().any(|(agent, account)| {
            !agent_ids.contains(agent.as_str())
                || !account.is_null() && account.as_str().is_none_or(|id| !account_ids.contains(id))
        })
    {
        return Err(invalid());
    }
    Ok(Some(
        json!({"providerInstances":accounts,"agentProviderInstances":bindings}),
    ))
}

pub(super) fn agent_provider_block(
    db: &Connection,
    machine: &str,
    agent: &str,
) -> Result<Option<String>> {
    let row = one(db, "SELECT m.name FROM machine_provider_reports r JOIN paired_machines m USING(environment_id),json_each(r.report,'$.agentProviderInstances') b WHERE m.environment_id=? AND b.key=? AND b.type='null'",[machine,agent])?;
    Ok(row.map(|row| format!("No provider on {}", text(&row, "name"))))
}

fn cache_provider_report(db: &Connection, id: &str, report: Option<&Value>) -> Result<()> {
    if let Some(report) = report {
        db.execute("INSERT INTO machine_provider_reports VALUES(?,?) ON CONFLICT(environment_id) DO UPDATE SET report=excluded.report",params![id,report.to_string()])?;
    } else {
        db.execute(
            "DELETE FROM machine_provider_reports WHERE environment_id=?",
            [id],
        )?;
    }
    Ok(())
}

fn prior(s: &AppState, id: &str, fingerprint: &str) -> Result<Option<Value>> {
    let v = with_db(s, |db| {
        one(db, "SELECT * FROM machine_commands WHERE id=?", [id])
    })?;
    if let Some(v) = &v {
        if v["request"] != fingerprint {
            return Err(error(409, "Request ID was already used."));
        }
    }
    Ok(v)
}
fn receipt(
    s: &AppState,
    id: &str,
    digest: &str,
    machine: Option<&str>,
    result: &Value,
) -> Result<()> {
    with_db(s, |db| {
        db.execute(
            "INSERT INTO machine_commands VALUES(?,?,?,?)",
            params![id, digest, machine, result.to_string()],
        )?;
        Ok(())
    })
}
async fn confirm(s: &AppState, r: &Value) -> Result<Value> {
    if r["state"] == "paired" {
        return visible(s, r);
    }
    let origin = text(r, "origin");
    let token = crypt(s, text(r, "token"), origin, false)?;
    let back = crypt(
        s,
        text(r, "return_token"),
        &format!("{origin}:return"),
        false,
    )?;
    remote(
        s,
        origin,
        "/api/machines/reciprocal",
        Some(&token),
        Some(&json!({"environment":environment(s)?,"token":back})),
    )
    .await?;
    with_db(s, |db| {
        db.execute(
            "UPDATE paired_machines SET state='paired' WHERE environment_id=? AND local_token_id=?",
            params![r["environment_id"].as_str(), r["local_token_id"].as_str()],
        )?;
        Ok(())
    })?;
    visible(s, &row(s, text(r, "environment_id"))?)
}
async fn add(s: &AppState, v: &Value) -> Result<Value> {
    fields(v, &["id", "origin", "code"])?;
    let id = request_id(&v["id"])?;
    let origin = machine_origin(text(v, "origin"))?;
    let code = code(&v["code"]).map_err(|_| error(400, "Enter the 12-character pairing code."))?;
    let fingerprint = hash(&json!({"origin":origin,"code":code}));
    if let Some(prior) = prior(s, &id, &fingerprint)? {
        let r = row(s, text(&prior, "environment_id")).map_err(|_| {
            error(
                409,
                "Pairing result is uncertain. Check both machines, then create a new code.",
            )
        })?;
        return confirm(s, &r).await;
    }
    if environment(s)?["origin"] == origin
        || with_db(s, |db| {
            one(
                db,
                "SELECT 1 FROM paired_machines WHERE origin=?",
                [&origin],
            )
        })?
        .is_some()
    {
        return Err(error(409, "Machine is already paired or is this machine."));
    }
    receipt(s, &id, &fingerprint, None, &Value::Null)?;
    let response = remote(
        s,
        &origin,
        "/api/pairing/redeem",
        None,
        Some(&json!({"code":code,"returnPairing":environment(s)?})),
    )
    .await?;
    fields(&response, &["environment", "token"])?;
    let peer = descriptor(&response["environment"])?;
    let token = credential(&response["token"])?;
    if peer["id"] == environment(s)?["id"] || peer["origin"] != origin {
        return Err(error(
            502,
            "Remote machine identity does not match its origin.",
        ));
    }
    no_secrets(&peer, &[&token])?;
    if row(s, text(&peer, "id")).is_ok() {
        return Err(error(409, "This environment is already paired."));
    }
    let issued = issue(s, &format!("Minarbot: {}", text(&peer, "name"))).await?;
    let result = (|| {
        let back = credential(&issued["token"])?;
        no_secrets(&peer, &[&back])?;
        let enc = crypt(s, &token, &origin, true)?;
        let enc_back = crypt(s, &back, &format!("{origin}:return"), true)?;
        tx(s, |db| {
            db.execute(
                "INSERT INTO paired_machines VALUES(?,?,?,?,?,?,'pending','offline',?,NULL,'[]',1)",
                params![
                    text(&peer, "id"),
                    text(&peer, "name"),
                    origin,
                    enc,
                    text(&issued, "id"),
                    enc_back,
                    text(&peer, "version")
                ],
            )?;
            db.execute(
                "UPDATE machine_commands SET environment_id=? WHERE id=?",
                params![text(&peer, "id"), id],
            )?;
            Ok(())
        })
    })();
    if result.is_err() {
        let _ = revoke(s, text(&issued, "id")).await;
        let _ = remote(
            s,
            &origin,
            "/api/peer/unpair",
            Some(&token),
            Some(&json!({})),
        )
        .await;
        return Err(error(
            503,
            "Pairing could not be saved. Create a new code and try again.",
        ));
    }
    confirm(s, &row(s, text(&peer, "id"))?).await.map_err(|_| {
        error(
            409,
            "Return pairing is not confirmed. Retry the same request or unpair this machine.",
        )
    })?;
    check_machine(
        s,
        &row(s, text(&peer, "id"))?,
        &peer_client()?,
        RemoteRequest::Standard,
    )
    .await?;
    visible(s, &row(s, text(&peer, "id"))?)
}
async fn redeem(s: &AppState, v: &Value) -> Result<Value> {
    let (peer, code) = (|| {
        fields(v, &["code", "returnPairing"])?;
        let c = code(&v["code"])?;
        let p = descriptor(&v["returnPairing"])?;
        if p["id"] == environment(s)?["id"] || p["origin"] == public_origin(s) {
            return Err(invalid());
        }
        Ok((p, c))
    })()
    .map_err(|_| invalid())?;
    let now = now(s).await?;
    tx(s, |db| {
        let changed = db.execute(
            "UPDATE pairing_codes SET used=1 WHERE digest=? AND expires>? AND used=0
             AND NOT EXISTS(SELECT 1 FROM pairing_code_targets WHERE id=pairing_codes.id AND origin<>?)",
            params![hash(&json!(code)), now, text(&peer, "origin")],
        )?;
        if changed!=1 || one(db,"SELECT 1 FROM paired_machines WHERE environment_id=? UNION ALL SELECT 1 FROM peer_pairing_pending WHERE environment_id=?",params![text(&peer,"id"),text(&peer,"id")])?.is_some(){return Err(invalid());}
        Ok(())
    })?;
    let issued = issue(s, &format!("Minarbot: {}", text(&peer, "name")))
        .await
        .map_err(|_| {
            error(
                503,
                "Peer token service is unavailable. Create a new code and try again.",
            )
        })?;
    with_db(s, |db| {
        db.execute(
            "INSERT INTO peer_pairing_pending VALUES(?,?,?,?,?,?)",
            params![
                text(&issued, "id"),
                text(&peer, "id"),
                text(&peer, "name"),
                text(&peer, "origin"),
                text(&peer, "version"),
                now + 600000
            ],
        )?;
        Ok(())
    })?;
    Ok(json!({"environment":environment(s)?,"token":issued["token"]}))
}
async fn reciprocal(s: &AppState, v: &Value, header: Option<&str>) -> Result<Value> {
    let auth = authorize(s, header).await?;
    fields(v, &["environment", "token"])?;
    let peer = descriptor(&v["environment"])?;
    let token = credential(&v["token"])?;
    let origin = text(&peer, "origin");
    let pending = with_db(s, |db| {
        one(
            db,
            "SELECT * FROM peer_pairing_pending WHERE token_id=? AND expires>?",
            params![text(&auth, "id"), s.config.now()],
        )
    })?;
    let Some(pending) = pending else {
        if let Ok(r) = row(s, text(&peer, "id")) {
            if r["local_token_id"] == auth["id"]
                && r["origin"] == peer["origin"]
                && crypt(s, text(&r, "token"), origin, false)? == token
            {
                return Ok(json!({"ok":true}));
            }
        }
        return Err(error(401, "Unauthorized peer request."));
    };
    if pending["environment_id"] != peer["id"] || pending["origin"] != peer["origin"] {
        return Err(error(401, "Unauthorized peer request."));
    }
    let check = descriptor(&remote(s, origin, "/api/environment", None, None).await?)?;
    if check["id"] != peer["id"] || check["origin"] != peer["origin"] {
        return Err(error(401, "Remote machine identity changed."));
    }
    let roster = remote(s, origin, PEER_AGENTS, Some(&token), None).await?;
    let cached = agents(&roster)?;
    let providers = provider_report(&roster, &cached)?;
    no_secrets(
        &json!({"peer":peer,"cached":cached,"providers":providers}),
        &[&token, text(&auth, "token")],
    )?;
    let enc = crypt(s, &token, origin, true)?;
    let back = crypt(s, text(&auth, "token"), &format!("{origin}:return"), true)?;
    tx(s, |db| {
        if one(
            db,
            "SELECT 1 FROM peer_pairing_pending WHERE token_id=? AND expires>?",
            params![text(&auth, "id"), s.config.now()],
        )?
        .is_none()
        {
            return Err(error(401, "Unauthorized peer request."));
        }
        db.execute(
            "INSERT INTO paired_machines VALUES(?,?,?,?,?,?,'paired','online',?,?,?,1)",
            params![
                text(&peer, "id"),
                text(&peer, "name"),
                origin,
                enc,
                text(&auth, "id"),
                back,
                text(&peer, "version"),
                s.config.now(),
                cached.to_string()
            ],
        )?;
        cache_provider_report(db, text(&peer, "id"), providers.as_ref())?;
        db.execute(
            "DELETE FROM peer_pairing_pending WHERE token_id=?",
            [text(&auth, "id")],
        )?;
        Ok(json!({"ok":true}))
    })
}
async fn peer_agents(
    s: &AppState,
    header: Option<&str>,
    policy: bool,
    report_providers: bool,
    probe: bool,
) -> Result<Value> {
    let auth = authorize(s, header).await?;
    if with_db(s,|db|one(db,"SELECT 1 FROM paired_machines WHERE local_token_id=? UNION ALL SELECT 1 FROM peer_pairing_pending WHERE token_id=? AND expires>?",params![text(&auth,"id"),text(&auth,"id"),s.config.now()]))?.is_none(){return Err(error(401,"Unauthorized peer request."));}
    if probe {
        return Ok(json!({"reachable":true}));
    }
    // Read provider metadata before opening the roster connection. Calling the
    // provider-instance list helper inside this database closure re-enters the
    // non-reentrant database mutex and stalls every request on this machine.
    let provider_metadata = if report_providers {
        Some(crate::data::provider_instances::list(s)?["providers"].clone())
    } else {
        None
    };
    let mut result = with_db(s, |db| {
        let mut roster = rows(db,"SELECT id,name,role,model,provider_instance,CASE WHEN EXISTS(SELECT 1 FROM turns t WHERE t.agent=agents.id AND t.status IN('queued','running') AND t.paused=0) THEN 'working' ELSE 'idle' END AS status FROM agents WHERE id<>? ORDER BY rowid",[crate::mcp::guide::ID])?;
        let mut result = json!({});
        if let Some(metadata) = &provider_metadata {
            let mut bindings = serde_json::Map::new();
            for agent in &roster {
                let binding = text(agent, "provider_instance");
                if binding.is_empty() {
                    bindings.insert(text(agent, "id").to_owned(), Value::Null);
                } else if metadata
                    .as_array()
                    .is_some_and(|instances| instances.iter().any(|a| a["id"] == binding))
                {
                    bindings.insert(text(agent, "id").to_owned(), json!(binding));
                }
            }
            result["providerInstances"] = metadata.clone();
            result["agentProviderInstances"] = json!(bindings);
        }
        for agent in &mut roster {
            if let Some(fields) = agent.as_object_mut() {
                fields.remove("provider_instance");
            }
        }
        result["agents"] = json!(roster);
        Ok(result)
    })?;
    if policy {
        let allowed = with_db(s, |db| {
            one(
                db,
                "SELECT allow_agent_messaging FROM paired_machines WHERE local_token_id=?",
                [text(&auth, "id")],
            )
        })?
        .is_none_or(|r| r["allow_agent_messaging"] != 0);
        result["messagingAllowed"] = json!(
            allowed
                && auth["scopes"]
                    .as_array()
                    .is_some_and(|a| a.contains(&json!("agents:message")))
        );
    }
    no_secrets(&result, &[text(&auth, "token")])?;
    Ok(result)
}
pub fn set_updates(s: &AppState, v: &Value) -> Result<Value> {
    fields(v, &["id", "environmentId", "allowed"])?;
    let id = request_id(&v["id"])?;
    let machine = request_id(&v["environmentId"])?;
    let allowed = v["allowed"]
        .as_bool()
        .ok_or_else(|| error(400, "Update setting is invalid."))?;
    let fingerprint = hash(&json!({"updatePolicy":machine,"allowed":allowed}));
    if let Some(old) = prior(s, &id, &fingerprint)? {
        return serde_json::from_str(text(&old, "result")).map_err(|_| Failure::storage());
    }
    let r = row(s, &machine)?;
    if r["state"] != "paired" {
        return Err(error(404, "Paired machine not found."));
    }
    tx(s, |db| {
        let token = one(
            db,
            "SELECT scopes FROM mcp_server_tokens WHERE id=? AND kind='peer' AND revoked IS NULL",
            [text(&r, "local_token_id")],
        )?
        .ok_or_else(|| error(409, "Pair this machine again before enabling updates."))?;
        let mut scopes: Vec<String> =
            serde_json::from_str(text(&token, "scopes")).map_err(|_| Failure::storage())?;
        scopes.retain(|v| v != "machine:update");
        if allowed {
            scopes.push("machine:update".into());
        }
        scopes.sort();
        db.execute(
            "UPDATE mcp_server_tokens SET scopes=? WHERE id=?",
            params![json!(scopes).to_string(), text(&r, "local_token_id")],
        )?;
        db.execute("INSERT INTO machine_update_policy VALUES(?,?) ON CONFLICT(environment_id) DO UPDATE SET allowed=excluded.allowed",params![machine,allowed as i64])?;
        Ok(())
    })?;
    let value = visible(s, &r)?;
    receipt(s, &id, &fingerprint, Some(&machine), &value)?;
    Ok(value)
}
pub async fn authorize_update(s: &AppState, header: Option<&str>) -> Result<String> {
    let auth = authorize(s, header).await?;
    let r = with_db(s, |db| {
        one(
            db,
            "SELECT * FROM paired_machines WHERE local_token_id=? AND state='paired'",
            [text(&auth, "id")],
        )
    })?;
    if let Some(r) = r {
        if auth["scopes"]
            .as_array()
            .is_some_and(|a| a.contains(&json!("machine:update")))
            && visible(s, &r)?["allowUpdates"] == true
        {
            return Ok(text(&r, "environment_id").into());
        }
    }
    Err(error(
        403,
        "Machine updates require permission from both owners in Settings > Machines.",
    ))
}
fn permitted_peer(s: &AppState, machine: &str) -> Result<Value> {
    let r = row(s, machine).map_err(|failure| {
        if failure.status == 404 {
            PeerError::PermissionRevoked.failure()
        } else {
            failure
        }
    })?;
    if r["state"] != "paired" || visible(s, &r)?["allowUpdates"] != true {
        return Err(PeerError::PermissionRevoked.failure());
    }
    Ok(r)
}
fn reachable(r: &Value) -> bool {
    matches!(text(r, "status"), "online" | "version-mismatch")
}
async fn update_peer(s: &AppState, machine: &str) -> Result<Value> {
    let r = permitted_peer(s, machine)?;
    if reachable(&r) {
        return Ok(r);
    }
    // The cached status lags a peer restart; check live before refusing an update.
    check_machine(s, &r, &peer_client()?, RemoteRequest::Standard).await?;
    let r = permitted_peer(s, machine)?;
    if !reachable(&r) {
        return Err(PeerError::ConnectionLost.detail("Paired machine is offline."));
    }
    Ok(r)
}
pub async fn update_remote(
    s: &AppState,
    machine: &str,
    body: Option<&Value>,
    bytes: u64,
    progress: impl Fn(u64) -> Result<()>,
) -> Result<Value> {
    let r = update_peer(s, machine).await?;
    let origin = text(&r, "origin");
    let peer = descriptor(
        &remote_request(
            s,
            origin,
            "/api/environment",
            None,
            None,
            Some(Duration::from_secs(10)),
        )
        .await?,
    )?;
    if peer["id"] != r["environment_id"] || peer["origin"] != r["origin"] {
        return Err(error(401, "Remote machine identity changed."));
    }
    let token = crypt(s, text(&r, "token"), origin, false)?;
    let status = remote(s, origin, UPDATE_STATUS, Some(&token), None).await?;
    let value = match body {
        Some(body)
            if body["source"] == "GitHub"
                && !(status["peerGitHubUpdates"] == true && status["peerApplyPolling"] == true) =>
        {
            return Err(PeerError::GitHubUnsupported.failure());
        }
        Some(body) if status["peerApplyPolling"] == true => {
            let mut body = body.clone();
            body["poll"] = json!(true);
            poll_update(s, origin, &token, &body, bytes, progress).await?
        }
        Some(body) => {
            let deadline = s
                .config
                .update
                .options()
                .test_peer_update_timeout_ms
                .map_or_else(|| update_deadline(bytes), Duration::from_millis);
            remote_request(
                s,
                origin,
                UPDATE_APPLY,
                Some(&token),
                Some(body),
                Some(deadline),
            )
            .await?
        }
        None => status,
    };
    if body.is_some() && crate::web::id(text(&value, "id")).is_none() {
        return Err(PeerError::Unconfirmed.failure());
    }
    no_secrets(
        &value,
        &[
            &token,
            &crypt(
                s,
                text(&r, "return_token"),
                &format!("{origin}:return"),
                false,
            )?,
        ],
    )?;
    Ok(value)
}
async fn poll_update(
    s: &AppState,
    origin: &str,
    token: &str,
    body: &Value,
    total: u64,
    progress: impl Fn(u64) -> Result<()>,
) -> Result<Value> {
    let client = peer_client()?;
    let mut last_reply = Instant::now();
    loop {
        let reply = remote_with_client(
            s,
            origin,
            UPDATE_APPLY,
            Some(token),
            Some(body),
            RemoteRequest::Update(Duration::from_secs(10)),
            &client,
        )
        .await;
        match reply {
            Ok(value) if value["pending"] == true => {
                progress(value["bytesTransferred"].as_u64().unwrap_or(0))?;
                last_reply = Instant::now();
            }
            Ok(value)
                if value["accepted"] == true && crate::web::id(text(&value, "id")).is_some() =>
            {
                // A receiver that restarted into the new release confirms the attempt
                // without a byte count. Its launcher only applies a complete, verified archive.
                progress(value["bytesTransferred"].as_u64().unwrap_or(total))?;
                return Ok(value);
            }
            Err(failure) if failure.code != PeerError::Unconfirmed.code() => return Err(failure),
            _ if last_reply.elapsed() >= Duration::from_secs(90) => {
                return Err(PeerError::Unconfirmed.failure())
            }
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn peer_http(s: &AppState, request: reqwest::RequestBuilder) -> Result<reqwest::Response> {
    if s.config.computer.test_computer_bridge.is_none() {
        return request.send().await.map_err(PeerError::network);
    }
    use reqwest::ResponseBuilderExt;
    let request = request.build().map_err(|_| Failure::storage())?;
    let headers = request
        .headers()
        .iter()
        .map(|(k, v)| {
            Ok((
                k.as_str().to_owned(),
                json!(v.to_str().map_err(|_| Failure::storage())?),
            ))
        })
        .collect::<Result<serde_json::Map<String, Value>>>()?;
    let body = request
        .body()
        .and_then(|b| b.as_bytes())
        .map(|b| String::from_utf8(b.to_vec()))
        .transpose()
        .map_err(|_| Failure::storage())?;
    let result = bridge(s, "peer-http", json!({"url":request.url().as_str(),"method":request.method().as_str(),"headers":headers,"body":body})).await?;
    let mut response = axum::http::Response::builder()
        .status(result["status"].as_u64().ok_or_else(Failure::storage)? as u16)
        .url(request.url().clone());
    for (k, v) in result["headers"].as_object().into_iter().flatten() {
        response = response.header(k, v.as_str().ok_or_else(Failure::storage)?);
    }
    Ok(response
        .body(
            STANDARD
                .decode(text(&result, "body"))
                .map_err(|_| Failure::storage())?,
        )
        .map_err(|_| Failure::storage())?
        .into())
}
pub async fn call_tool(s: &AppState, machine: &str, tool: &str, args: &Value) -> Result<Value> {
    let operation = async {
        let r = row(s, machine).map_err(|_| error(404, "Paired machine is unavailable."))?;
        if r["state"] != "paired" {
            return Err(error(404, "Paired machine is unavailable."));
        }
        let origin = text(&r, "origin");
        let peer = descriptor(&remote(s, origin, "/api/environment", None, None).await?)?;
        if peer["id"] != machine || peer["origin"] != origin {
            return Err(error(401, "Remote machine identity changed."));
        }
        let token = crypt(s, text(&r, "token"), origin, false)?;
        let return_token = crypt(
            s,
            text(&r, "return_token"),
            &format!("{origin}:return"),
            false,
        )?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| Failure::storage())?;
        let mut session = None;
        let mut protocol = "2025-03-26".to_owned();
        let mut result = Value::Null;
        for message in [
            json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"minarbot-peer","version":version()}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}}),
        ] {
            if !row(s, machine)
                .is_ok_and(|current| current["local_token_id"] == r["local_token_id"])
            {
                return Err(error(401, "Pairing was revoked."));
            }
            let mut request = client
                .post(format!("{origin}/mcp"))
                .bearer_auth(&token)
                .header("Accept", "application/json, text/event-stream")
                .header("MCP-Protocol-Version", &protocol)
                .json(&message);
            if let Some(session) = &session {
                request = request.header("Mcp-Session-Id", session);
            }
            let mut response = peer_http(s, request).await.map_err(|_| {
                error(
                    502,
                    "Remote machine is offline or MCP request exceeded its bounds.",
                )
            })?;
            if !response.status().is_success() {
                return Err(error(
                    if [401, 403].contains(&response.status().as_u16()) {
                        401
                    } else {
                        502
                    },
                    "Remote MCP request was rejected.",
                ));
            }
            if let Some(id) = response
                .headers()
                .get("mcp-session-id")
                .and_then(|v| v.to_str().ok())
            {
                session = Some(id.to_owned());
            }
            if response.url().origin().ascii_serialization() != origin
                || response.content_length().is_some_and(|n| n > 128 * 1024)
            {
                return Err(error(
                    502,
                    "Remote machine is offline or MCP request exceeded its bounds.",
                ));
            }
            if matches!(response.status().as_u16(), 202 | 204) {
                continue;
            }
            if !response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("application/json"))
            {
                return Err(error(
                    502,
                    "Remote machine is offline or MCP request exceeded its bounds.",
                ));
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| Failure::storage())? {
                if bytes.len() + chunk.len() > 128 * 1024 {
                    return Err(error(
                        502,
                        "Remote machine is offline or MCP request exceeded its bounds.",
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            let value: Value = serde_json::from_slice(&bytes)
                .map_err(|_| error(502, "Remote MCP returned an invalid result."))?;
            no_secrets(&value, &[&token, &return_token])?;
            if message["id"] == 0 {
                protocol = value["result"]["protocolVersion"]
                    .as_str()
                    .unwrap_or("2025-03-26")
                    .to_owned();
            } else if message["id"] == 1 {
                if value["result"]["isError"] == true {
                    let content = value["result"]["content"]
                        .as_array()
                        .and_then(|a| a.iter().find(|v| v["type"] == "text"));
                    let status = content
                        .and_then(|v| serde_json::from_str::<Value>(text(v, "text")).ok())
                        .and_then(|v| v["status"].as_u64())
                        .filter(|s| [400, 401, 403, 404, 409, 429].contains(s))
                        .unwrap_or(502);
                    return Err(error(status as u16, "Remote agent request was rejected."));
                }
                result = value["result"]["structuredContent"].clone();
                if !result.is_object() {
                    return Err(error(502, "Remote MCP returned an invalid result."));
                }
            }
        }
        if let Some(session) = session {
            let _ = client
                .delete(format!("{origin}/mcp"))
                .bearer_auth(&token)
                .header("Mcp-Session-Id", session)
                .send()
                .await;
        }
        Ok(result)
    };
    tokio::time::timeout(Duration::from_secs(10), operation)
        .await
        .map_err(|_| {
            error(
                502,
                "Remote machine is offline or MCP request exceeded its bounds.",
            )
        })?
}
pub async fn download_release(
    s: &AppState,
    machine: &str,
    release: &str,
    offset: u64,
    polling: bool,
) -> Result<reqwest::Response> {
    let r = update_peer(s, machine).await?;
    if !minarbot_launcher::files::release_id(release) {
        return Err(error(400, "Invalid release path."));
    }
    let origin = text(&r, "origin");
    let token = crypt(s, text(&r, "token"), origin, false)?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(RELEASE_IDLE_TIMEOUT)
        .build()
        .map_err(|_| Failure::storage())?;
    let mut request = client
        .get(format!("{origin}/api/updates/releases/{release}"))
        .bearer_auth(token);
    if polling {
        request = request.header("X-Minarbot-Update-Poll", "1");
    }
    if offset > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
    }
    request.send().await.map_err(PeerError::network)
}
fn cache_roster(s: &AppState, r: &Value, roster: &Value, secrets: &[&str]) -> Result<()> {
    let cached = agents(roster)?;
    let providers = provider_report(roster, &cached)?;
    no_secrets(&json!({"cached":cached,"providers":providers}), secrets)?;
    tx(s, |db| {
        let id = text(r, "environment_id");
        let token_id = text(r, "local_token_id");
        let changed = db.execute(
            "UPDATE paired_machines SET agents=? WHERE environment_id=? AND local_token_id=?",
            params![cached.to_string(), id, token_id],
        )?;
        if changed == 0 {
            return Ok(());
        }
        cache_provider_report(db, id, providers.as_ref())?;
        if let Some(allowed) = roster["messagingAllowed"].as_bool() {
            db.execute("INSERT INTO computer_peer_messaging VALUES(?,?) ON CONFLICT(token_id) DO UPDATE SET allowed=excluded.allowed",params![token_id,allowed as i64])?;
        } else {
            db.execute(
                "DELETE FROM computer_peer_messaging WHERE token_id=?",
                [token_id],
            )?;
        }
        Ok(())
    })
}

fn mark_online(s: &AppState, r: &Value, peer: &Value) -> Result<i64> {
    let seen = s.config.now();
    with_db(s, |db| {
        db.execute(
            "UPDATE paired_machines SET status='online',name=?,version=?,last_seen=? WHERE environment_id=? AND local_token_id=?",
            params![text(peer, "name"), text(peer, "version"), seen, text(r, "environment_id"), text(r, "local_token_id")],
        )?;
        Ok(seen)
    })
}

fn failed_status(failure: &Failure, prior: &str, seen: Option<i64>, now: i64) -> &'static str {
    let timed_out = failure.code == "peer_check_timeout";
    if [401, 409].contains(&failure.status) || (timed_out && prior == "unauthorized") {
        return "unauthorized";
    }
    if timed_out && seen.is_some_and(|last_seen| now.saturating_sub(last_seen) <= MAX_STALE_AGE_MS)
    {
        return "stale";
    }
    "offline"
}

async fn check_machine(
    s: &AppState,
    r: &Value,
    client: &reqwest::Client,
    mode: RemoteRequest,
) -> Result<()> {
    let origin = text(r, "origin");
    let mut seen = r["last_seen"].as_i64();
    let result = async {
        let peer = descriptor(
            &remote_with_client(s, origin, "/api/environment", None, None, mode, client).await?,
        )?;
        if peer["id"] != r["environment_id"] || peer["origin"] != r["origin"] {
            return Err(error(401, "Remote machine identity changed."));
        }
        let token = crypt(s, text(r, "token"), origin, false)?;
        let back = crypt(
            s,
            text(r, "return_token"),
            &format!("{origin}:return"),
            false,
        )?;
        no_secrets(&peer, &[&token, &back])?;
        let response =
            remote_with_client(s, origin, PEER_AGENTS, Some(&token), None, mode, client).await?;

        if matches!(mode, RemoteRequest::Probe) && response == json!({"reachable":true}) {
            seen = Some(mark_online(s, r, &peer)?);
            let roster = remote_with_client(
                s,
                origin,
                PEER_AGENTS,
                Some(&token),
                None,
                RemoteRequest::Standard,
                client,
            )
            .await;
            return match roster {
                Ok(roster) => {
                    let _ = cache_roster(s, r, &roster, &[&token, &back]);
                    Ok(())
                }
                Err(failure) if failure.status == 401 => Err(failure),
                Err(_) => Ok(()),
            };
        }

        // Older peers ignore the probe header and send the roster as usual.
        cache_roster(s, r, &response, &[&token, &back])?;
        mark_online(s, r, &peer)?;
        Ok(())
    }
    .await;
    if let Err(failure) = result {
        let status = failed_status(&failure, text(r, "status"), seen, s.config.now());
        with_db(s, |db| {
            // A slow check must not overwrite a newer successful check.
            db.execute(
                "UPDATE paired_machines SET status=? WHERE environment_id=? AND local_token_id=? AND last_seen IS ?",
                params![status, text(r, "environment_id"), text(r, "local_token_id"), seen],
            )?;
            Ok(())
        })?;
    }
    Ok(())
}
pub async fn health(s: &AppState) -> Result<()> {
    let now = now(s).await?;
    let expired = with_db(s, |db| {
        rows(
            db,
            "SELECT token_id FROM peer_pairing_pending WHERE expires<=?",
            [now],
        )
    })?;
    for r in expired {
        with_db(s, |db| {
            db.execute(
                "DELETE FROM peer_pairing_pending WHERE token_id=?",
                [text(&r, "token_id")],
            )?;
            Ok(())
        })?;
        let _ = revoke(s, text(&r, "token_id")).await;
    }
    let revoked = with_db(s, |db| {
        rows(db, "SELECT token_id FROM peer_revocations", [])
    })?;
    for r in revoked {
        let _ = revoke(s, text(&r, "token_id")).await;
    }
    let peers = with_db(s, |db| {
        rows(db, "SELECT * FROM paired_machines WHERE state='paired'", [])
    })?;
    let client = peer_client()?;
    stream::iter(peers)
        .map(|peer| {
            let client = &client;
            async move { check_machine(s, &peer, client, RemoteRequest::Probe).await }
        })
        .buffer_unordered(8)
        .try_collect::<Vec<()>>()
        .await?;
    Ok(())
}
async fn unpair(s: &AppState, v: &Value) -> Result<Value> {
    fields(v, &["id", "environmentId"])?;
    let id = request_id(&v["id"])?;
    let machine = request_id(&v["environmentId"])?;
    let fp = hash(&json!({"unpair":machine}));
    if prior(s, &id, &fp)?.is_some() {
        return Ok(json!({"ok":true}));
    }
    let r = row(s, &machine).ok();
    tx(s, |db| {
        db.execute(
            "DELETE FROM paired_machines WHERE environment_id=?",
            [&machine],
        )?;
        db.execute(
            "DELETE FROM machine_update_policy WHERE environment_id=?",
            [&machine],
        )?;
        if let Some(r) = &r {
            db.execute(
                "INSERT OR IGNORE INTO peer_revocations VALUES(?)",
                [text(r, "local_token_id")],
            )?;
        }
        db.execute(
            "INSERT INTO machine_commands VALUES(?,?,NULL,'{\"ok\":true}')",
            params![id, fp],
        )?;
        Ok(())
    })?;
    if let Some(r) = r {
        let _ = revoke(s, text(&r, "local_token_id")).await;
        if let Ok(token) = crypt(s, text(&r, "token"), text(&r, "origin"), false) {
            let _ = remote(
                s,
                text(&r, "origin"),
                "/api/peer/unpair",
                Some(&token),
                Some(&json!({})),
            )
            .await;
        }
    }
    Ok(json!({"ok":true}))
}
async fn unpair_remote(s: &AppState, v: &Value, header: Option<&str>) -> Result<Value> {
    fields(v, &[])?;
    let auth = authorize(s, header).await?;
    let id = text(&auth, "id");
    let r=with_db(s,|db|one(db,"SELECT environment_id FROM paired_machines WHERE local_token_id=? UNION ALL SELECT environment_id FROM peer_pairing_pending WHERE token_id=?",params![id,id]))?.ok_or_else(||error(401,"Unauthorized peer request."))?;
    tx(s, |db| {
        db.execute(
            "DELETE FROM paired_machines WHERE environment_id=?",
            [text(&r, "environment_id")],
        )?;
        db.execute("DELETE FROM peer_pairing_pending WHERE token_id=?", [id])?;
        db.execute(
            "DELETE FROM machine_update_policy WHERE environment_id=?",
            [text(&r, "environment_id")],
        )?;
        db.execute("INSERT OR IGNORE INTO peer_revocations VALUES(?)", [id])?;
        Ok(())
    })?;
    let _ = revoke(s, id).await;
    Ok(json!({"ok":true}))
}

pub async fn handle(
    s: &AppState,
    request: axum::extract::Request,
) -> Result<axum::response::Response> {
    use axum::http::Method;
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    let auth = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if request.uri().query().is_some() {
        return Err(error(400, "Machine routes do not accept query parameters."));
    }
    if method == Method::GET {
        let value = match path.as_str() {
            "/api/environment" => environment(s)?,
            "/api/machines" => {
                if s.config
                    .computer
                    .test_computer_bridge
                    .as_ref()
                    .is_some_and(|b| {
                        request
                            .headers()
                            .get("x-minarbot-test-refresh")
                            .and_then(|v| v.to_str().ok())
                            == Some(b.token.as_str())
                    })
                {
                    health(s).await?;
                }
                json!({"machines":list(s)?})
            }
            PEER_AGENTS => {
                peer_agents(
                    s,
                    auth.as_deref(),
                    request
                        .headers()
                        .get("x-minarbot-messaging-policy")
                        .and_then(|v| v.to_str().ok())
                        == Some("1"),
                    crate::text_header(request.headers(), "x-minarbot-provider-accounts")
                        == Some("1"),
                    crate::text_header(request.headers(), PEER_PROBE_HEADER) == Some("1"),
                )
                .await?
            }
            "/api/mcp-server/peer-tokens" => with_db(s, |db| {
                Ok(
                    json!({"items":rows(db,"SELECT id,name,scopes,created,last_used AS lastUsed,revoked FROM mcp_server_tokens WHERE kind='peer' ORDER BY created DESC,id",[])?.into_iter().map(|mut r|{r["scopes"]=serde_json::from_str(text(&r,"scopes")).unwrap_or(Value::Null);r}).collect::<Vec<_>>()}),
                )
            })?,
            _ => return Err(error(405, "Method not allowed.")),
        };
        return Ok(crate::json_response(200, value));
    }
    if method != Method::POST {
        return Err(error(405, "Method not allowed."));
    }
    let transport = [
        "/api/pairing/redeem",
        "/api/machines/reciprocal",
        "/api/peer/unpair",
    ]
    .contains(&path.as_str());
    if path == "/api/pairing/redeem" {
        let now = now(s).await?;
        tx(s, |db| {
            let bucket = now / 60000;
            db.execute(
                "DELETE FROM pairing_rate_limit WHERE bucket<?",
                [bucket - 1],
            )?;
            let count: i64 = db.query_row(
                "SELECT coalesce(sum(attempts),0) FROM pairing_rate_limit",
                [],
                |r| r.get(0),
            )?;
            if count >= 12 {
                return Err(error(
                    429,
                    "Too many pairing attempts. Try again in two minutes.",
                ));
            }
            db.execute("INSERT INTO pairing_rate_limit VALUES(?,1) ON CONFLICT(bucket) DO UPDATE SET attempts=attempts+1",[bucket])?;
            Ok(())
        })?;
    }
    if transport {
        if request.headers().contains_key("origin")
            || request.headers().contains_key("sec-fetch-site")
            || request
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .split(';')
                .next()
                != Some("application/json")
        {
            return Err(invalid());
        }
    } else {
        crate::files::mutation(s, &request)?;
    }
    let value = super::body(request, 8192)
        .await
        .map_err(|e| if transport { invalid() } else { e })?;
    let result = match path.as_str() {
        "/api/environment" => {
            fields(&value, &["id", "name"])?;
            let id = request_id(&value["id"])?;
            let name = field(&value["name"], 120)?;
            let fp = hash(&json!({"name":name}));
            if let Some(old) = prior(s, &id, &fp)? {
                serde_json::from_str(text(&old, "result")).map_err(|_| Failure::storage())?
            } else {
                with_db(s, |db| {
                    db.execute(
                        "UPDATE environment_identity SET name=? WHERE singleton=1",
                        [name],
                    )?;
                    Ok(())
                })?;
                let v = environment(s)?;
                receipt(s, &id, &fp, None, &v)?;
                v
            }
        }
        "/api/pairing-codes" => {
            fields(&value, &["id"])?;
            let id = request_id(&value["id"])?;
            let now = now(s).await?;
            with_db(s, |db| pairing_code(db, &id, now, |_| Ok(())))?
        }
        "/api/machines" => add(s, &value).await?,
        "/api/pairing/redeem" => redeem(s, &value).await?,
        "/api/machines/reciprocal" => reciprocal(s, &value, auth.as_deref()).await?,
        "/api/machines/remove" => unpair(s, &value).await?,
        "/api/peer/unpair" => unpair_remote(s, &value, auth.as_deref()).await?,
        "/api/machines/updates" => set_updates(s, &value)?,
        "/api/machines/agent-messaging" => {
            fields(&value, &["id", "environmentId", "allowed"])?;
            let id = request_id(&value["id"])?;
            let machine = request_id(&value["environmentId"])?;
            let allowed = value["allowed"]
                .as_bool()
                .ok_or_else(|| error(400, "Agent messaging setting is invalid."))?;
            let fp = hash(&json!({"machine":machine,"allowed":allowed}));
            if let Some(old) = prior(s, &id, &fp)? {
                serde_json::from_str(text(&old, "result")).map_err(|_| Failure::storage())?
            } else {
                let r = row(s, &machine)?;
                with_db(s, |db| {
                    db.execute(
                        "UPDATE paired_machines SET allow_agent_messaging=? WHERE environment_id=?",
                        params![allowed as i64, machine],
                    )?;
                    if !allowed {
                        db.execute("UPDATE turns SET status='cancelled',paused=0,revision=revision+1,error='Peer agent messaging was disabled by the host owner.' WHERE id IN(SELECT turn FROM mcp_server_messages WHERE caller=?) AND status NOT IN('done','cancelled')",[text(&r,"local_token_id")])?;
                    }
                    Ok(())
                })?;
                let v = visible(s, &row(s, &machine)?)?;
                receipt(s, &id, &fp, Some(&machine), &v)?;
                v
            }
        }
        _ => return Err(error(404, "Route not found.")),
    };
    Ok(crate::json_response(200, result))
}

pub async fn ensure_updates_allowed(state: &AppState, machine: &str) -> Result<()> {
    update_peer(state, machine).await.map(|_| ())
}

pub(crate) fn pairing_code(
    db: &Connection,
    id: &str,
    now: i64,
    commit: impl FnOnce(&Connection) -> Result<()>,
) -> Result<Value> {
    let mut code = String::new();
    while code.len() < 12 {
        for b in random::<12>()? {
            if b < 248 {
                code.push(b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ"[(b as usize) % 31] as char);
                if code.len() == 12 {
                    break;
                }
            }
        }
    }
    let expires = now + 600000;
    super::transaction(db, |db| {
        if one(db, "SELECT 1 FROM pairing_codes WHERE id=?", [&id])?.is_some() {
            return Err(error(
                409,
                "This code was already shown. Create a new pairing code.",
            ));
        }
        let count: i64 = db.query_row(
            "SELECT count(*) FROM pairing_codes WHERE used=0 AND expires>?",
            [now],
            |r| r.get(0),
        )?;
        if count >= 5 {
            return Err(error(
                429,
                "Five pairing codes are active. Wait for one to expire.",
            ));
        }
        db.execute(
            "INSERT INTO pairing_codes VALUES(?,?,?,0)",
            params![id, hash(&json!(code)), expires],
        )?;
        commit(db)?;
        Ok(())
    })?;
    Ok(json!({"code":code,"expiresAt":expires}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_status_ages_out_timeout_and_keeps_authorization_failures_distinct() -> Result<()> {
        let seen = Some(1_000);
        let expires = 1_000 + MAX_STALE_AGE_MS;
        let timeout = probe_timeout();
        assert_eq!(failed_status(&timeout, "online", seen, 31_000), "stale");
        assert_eq!(failed_status(&timeout, "stale", seen, expires), "stale");
        assert_eq!(
            failed_status(&timeout, "stale", seen, expires + 1),
            "offline"
        );
        assert_eq!(failed_status(&timeout, "offline", None, expires), "offline");
        assert_eq!(
            failed_status(&timeout, "unauthorized", seen, expires),
            "unauthorized"
        );
        for status in [401, 409] {
            assert_eq!(
                failed_status(
                    &error(status, "Authorization failed."),
                    "stale",
                    seen,
                    expires
                ),
                "unauthorized"
            );
        }
        assert_eq!(
            failed_status(
                &PeerError::ConnectionLost.failure(),
                "online",
                seen,
                expires
            ),
            "offline"
        );
        Ok(())
    }

    #[test]
    fn peer_origins_require_https_or_loopback_without_url_extras() -> Result<()> {
        for origin in [
            "http://example.test",
            "https://user:pass@machine.example.test",
            "https://host/path",
            "https://host?x=1",
            "https://host#x",
            "https://host\\evil",
            " https://host",
            "https://host?",
            "https://host#",
        ] {
            assert!(machine_origin(origin).is_err(), "{origin}");
        }
        for origin in ["https://machine.example.test:4317", "http://[::1]:1234"] {
            assert_eq!(machine_origin(origin)?, origin);
        }
        Ok(())
    }
}
