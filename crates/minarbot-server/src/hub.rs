mod authority;
mod browser;
mod fixture;
mod node;
mod routing;
#[cfg(test)]
mod schema_tests;
mod session;
mod storage;
#[cfg(test)]
mod tests;

use crate::{
    db::{canonical, digest},
    files::{keys, one, rows, text, transaction},
    web::{parse_json, Failure},
};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{header, HeaderMap, HeaderValue, Method},
    response::{IntoResponse, Response},
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rusqlite::{params, Connection};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

type Result<T> = std::result::Result<T, Failure>;
const LIMIT: usize = 65_536;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    domain: String,
    accounts_host: String,
    data_path: PathBuf,
    mailbox_key_path: PathBuf,
    #[serde(default = "default_port")]
    port: u16,
    #[serde(default)]
    trusted_gateway_addresses: Vec<IpAddr>,
    identity: session::IdentityConfig,
    web: Option<browser::WebConfig>,
    node_link: Option<node::Config>,
}

fn default_port() -> u16 {
    4318
}

struct Hub {
    db: Mutex<Connection>,
    config: Config,
    mailbox_key: ring::hmac::Key,
    identity: session::Verifier,
    assets: Option<browser::Assets>,
    signer: Option<ring::signature::Ed25519KeyPair>,
    limits: Mutex<HashMap<String, (i64, u32)>>,
    streams: Arc<tokio::sync::Semaphore>,
    sockets: Arc<tokio::sync::Semaphore>,
    _writer: storage::Writer,
}

pub async fn run() -> crate::db::Result<()> {
    let path = std::env::var_os("MINARBOT_HUB_CONFIG")
        .ok_or("MINARBOT_HUB_CONFIG must name an isolated hub configuration.")?;
    let config: Config = serde_json::from_slice(&std::fs::read(path)?)?;
    if !exact_host(&config.domain)
        || config.accounts_host != format!("accounts.{}", config.domain)
        || !exact_host(&config.accounts_host)
    {
        return Err("Central accounts host must be accounts.<domain>.".into());
    }
    let commands = std::env::args_os().skip(2).collect::<Vec<_>>();
    let test_fixture = commands == ["test-fixture"];
    if test_fixture {
        fixture::validate(&config)?;
    } else if !commands.is_empty() {
        return storage::command(&config.data_path, &commands);
    }
    let key = std::fs::read(&config.mailbox_key_path)?;
    if key.len() < 32 {
        return Err("Private mailbox lookup key must contain at least 32 bytes.".into());
    }
    let identity = session::Verifier::load(&config)?;
    let assets = config.web.as_ref().map(browser::Assets::load);
    let signer = config.node_link.as_ref().map(node::signer).transpose()?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", config.port)).await?;
    let (mut db, writer) = storage::open(&config.data_path)?;
    storage::fence_restart(&mut db)?;
    let state = Arc::new(Hub {
        db: Mutex::new(db),
        mailbox_key: ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &key),
        config,
        identity,
        assets,
        signer,
        limits: Default::default(),
        streams: Arc::new(tokio::sync::Semaphore::new(100)),
        sockets: Arc::new(tokio::sync::Semaphore::new(100)),
        _writer: writer,
    });
    println!("Minarbot hub: http://{}", listener.local_addr()?);
    let fixture_stop = test_fixture.then(|| fixture::start(&state));
    let router = Router::new().fallback(dispatch).with_state(state.clone());
    let shutdown = async {
        #[cfg(unix)]
        {
            if let Ok(mut term) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                tokio::select! { _ = term.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
                return;
            }
        }
        let _ = tokio::signal::ctrl_c().await;
    };
    // Drop live streams/sockets together with the isolated listener on shutdown.
    tokio::select! {
        result = axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>()) => result?,
        _ = shutdown => {},
        result = async {
            match fixture_stop {
                Some(stop) => stop.await?,
                None => std::future::pending().await,
            }
        } => result?,
    }
    Ok(())
}

async fn dispatch(
    State(hub): State<Arc<Hub>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response {
    let mut response = route(hub, peer.ip(), request)
        .await
        .unwrap_or_else(|error| {
            let mut response = crate::json_response(
                error.status,
                json!({"error":{"code":error.code,"message":error.message}}),
            );
            if error.status == 429 {
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
            }
            if error.status == 413 {
                response
                    .headers_mut()
                    .insert(header::CONNECTION, HeaderValue::from_static("close"));
            }
            response
        });
    response
        .headers_mut()
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .entry(header::CONTENT_SECURITY_POLICY)
        .or_insert(HeaderValue::from_static(
            "default-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'",
        ));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

async fn route(hub: Arc<Hub>, peer: IpAddr, request: Request) -> Result<Response> {
    let host = {
        let db = hub.lock()?;
        hub.host(&db, request.headers(), peer)?
    };
    let path = request.uri().path();
    if request.uri().scheme().is_some()
        || request.uri().authority().is_some()
        || !path.starts_with('/')
        || path.starts_with("//")
        || path.bytes().any(|c| c <= b' ' || c == b'\\' || c == b'#')
        || path.split('/').any(|s| {
            matches!(
                s.to_ascii_lowercase().as_str(),
                "." | ".." | "%2e" | "%2e%2e" | ".%2e" | "%2e."
            )
        })
    {
        return Err(missing());
    }
    let central = host == hub.config.accounts_host;
    if central
        && hub.signer.is_some()
        && matches!(
            path,
            "/node"
                | "/api/nodes/enrollment/start"
                | "/api/nodes/enrollment/poll"
                | "/api/nodes/session"
        )
    {
        return node::route(hub, peer, request).await;
    }
    browser::route(hub, peer, request, host, central).await
}

impl Hub {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.db.lock().map_err(|_| Failure::storage())
    }
    fn host(&self, db: &Connection, headers: &HeaderMap, peer: IpAddr) -> Result<String> {
        let raw = single(headers, "host")?
            .ok_or_else(|| Failure::new(400, "invalid_host", "Host is required."))?;
        if !exact_host(raw) {
            return Err(missing());
        }
        if single(headers, "forwarded")?.is_some() {
            return Err(missing());
        }
        let host = match single(headers, "x-forwarded-host")? {
            Some(v) if self.config.trusted_gateway_addresses.contains(&peer) && exact_host(v) => v,
            Some(_) => return Err(missing()),
            None => raw,
        };
        self.registered(db, host)?;
        Ok(host.to_owned())
    }
    fn registered(&self, db: &Connection, host: &str) -> Result<Option<String>> {
        if host == self.config.accounts_host {
            return Ok(None);
        }
        let valid = exact_host(host)
            && host
                .strip_suffix(&self.config.domain)
                .and_then(|v| v.strip_suffix('.'))
                .is_some_and(slug);
        if !valid {
            return Err(missing());
        }
        Ok(Some(db.query_row(
            "SELECT p.id FROM host_bindings h JOIN projects p ON p.id=h.project_id WHERE h.host=? AND h.active=1 AND p.status='active'",
            [host], |row| row.get(0)
        ).map_err(|error| if error == rusqlite::Error::QueryReturnedNoRows { missing() } else { error.into() })?))
    }
    fn limit(&self, key: String, ceiling: u32, window: i64) -> Result<()> {
        let now = now();
        let mut limits = self.limits.lock().map_err(|_| Failure::storage())?;
        limits.retain(|_, (until, _)| *until > now);
        if limits.len() >= 10_000 && !limits.contains_key(&key) {
            return Err(rate_limited());
        }
        let row = limits.entry(key).or_insert((now + window, 0));
        if row.1 >= ceiling {
            return Err(rate_limited());
        }
        row.1 += 1;
        Ok(())
    }
}
fn now() -> i64 {
    #[cfg(test)]
    if let Some(clock) = tests::clock() {
        return clock;
    }
    fixture::clock().unwrap_or_else(crate::config::now)
}
fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    getrandom::getrandom(&mut bytes).map_err(|_| Failure::storage())?;
    Ok(bytes)
}
fn opaque() -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(random::<32>()?))
}
fn encoded(value: &str, len: usize) -> Option<Vec<u8>> {
    if value.len() != (len * 4).div_ceil(3) {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(value).ok()?;
    (bytes.len() == len && URL_SAFE_NO_PAD.encode(&bytes) == value).then_some(bytes)
}
fn id(value: &Value) -> Result<String> {
    value
        .as_str()
        .and_then(crate::web::id)
        .ok_or_else(|| Failure::new(400, "invalid_id", "ID must be a UUID v4."))
}
fn integer(value: &Value, min: i64) -> Option<i64> {
    value
        .as_i64()
        .filter(|v| (min..=9_007_199_254_740_991).contains(v))
}
fn revision(value: &Value) -> Result<i64> {
    integer(value, 1).ok_or_else(|| {
        Failure::new(
            400,
            "invalid_revision",
            "Revision must be greater than zero.",
        )
    })
}
fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn label(value: &Value, limit: usize) -> Result<&str> {
    value
        .as_str()
        .filter(|s| {
            !s.trim().is_empty()
                && s.encode_utf16().count() <= limit
                && !s.chars().any(|c| c <= '\u{1f}' || c == '\u{7f}')
        })
        .ok_or_else(|| Failure::new(400, "invalid_label", "Label is invalid."))
}
fn exact_host(value: &str) -> bool {
    let Some((prefix, suffix)) = value.rsplit_once('.') else {
        return false;
    };
    value.len() <= 253
        && suffix.len() >= 2
        && suffix.len() <= 63
        && suffix.bytes().all(|v| v.is_ascii_lowercase())
        && prefix.split('.').all(dns_label)
}
fn dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|v| v.is_ascii_lowercase() || v.is_ascii_digit() || v == b'-')
}
fn slug(v: &str) -> bool {
    dns_label(v)
        && (3..=63).contains(&v.len())
        && v.as_bytes()[0].is_ascii_lowercase()
        && !matches!(
            v,
            "accounts"
                | "account"
                | "api"
                | "assets"
                | "www"
                | "admin"
                | "status"
                | "mail"
                | "support"
                | "auth"
                | "login"
                | "static"
                | "cdn"
                | "hub"
                | "nodes"
                | "localhost"
                | "hub-origin"
        )
        && !v.starts_with("xn--")
}
fn single<'a>(headers: &'a HeaderMap, key: &str) -> Result<Option<&'a str>> {
    let mut entries = headers.get_all(key).iter();
    let result = entries
        .next()
        .map(|v| v.to_str())
        .transpose()
        .map_err(|_| missing())?;
    if entries.next().is_some() {
        return Err(missing());
    }
    Ok(result)
}
fn closed(value: &Value, fields: &[&str]) -> Result<()> {
    if !keys(value, fields) {
        return Err(Failure::new(
            400,
            "unknown_field",
            "Unexpected object field.",
        ));
    }
    Ok(())
}
async fn body(request: Request, browser: bool) -> Result<Value> {
    if browser {
        let media = single(request.headers(), "content-type")?
            .unwrap_or_default()
            .to_ascii_lowercase();
        if media != "application/json"
            && !media.split_once(';').is_some_and(|(media, charset)| {
                media.trim_end() == "application/json" && charset.trim_start() == "charset=utf-8"
            })
        {
            return Err(Failure::new(
                415,
                "unsupported_media_type",
                "Use Content-Type application/json.",
            ));
        }
    }
    let bytes = tokio::time::timeout(
        Duration::from_secs(15),
        axum::body::to_bytes(request.into_body(), LIMIT),
    )
    .await
    .map_err(|_| Failure::new(408, "request_timeout", "Request timed out."))?
    .map_err(|_| Failure::new(413, "payload_too_large", "JSON payload exceeds the limit."))?;
    parse_json(&bytes)
}
fn stored(value: &Value, key: &str) -> Result<Value> {
    serde_json::from_str(text(value, key)).map_err(|_| Failure::storage())
}
fn json_ok(value: Value) -> Response {
    crate::json_response(200, value)
}
fn missing() -> Failure {
    Failure::new(404, "resource_not_found", "Resource not found.")
}
fn denied() -> Failure {
    Failure::new(403, "authority_denied", "Authority denied this request.")
}
fn stale() -> Failure {
    Failure::new(409, "stale_revision", "Authority revision changed.")
}
fn conflict() -> Failure {
    Failure::new(
        409,
        "idempotency_conflict",
        "Request ID already has a different payload.",
    )
}
fn rate_limited() -> Failure {
    Failure::new(429, "rate_limited", "Retry later.")
}
fn generation(db: &Connection) -> Result<String> {
    one(
        db,
        "SELECT authority_generation FROM hub_state WHERE id=1 AND state='active'",
        [],
    )?
    .map(|row| text(&row, "authority_generation").to_owned())
    .ok_or_else(|| {
        Failure::new(
            503,
            "authority_unavailable",
            "Hub is awaiting recovery reconciliation.",
        )
    })
}
