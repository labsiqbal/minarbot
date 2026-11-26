use crate::{
    computer::hub::{self, Effect},
    operator_file,
    web::Failure,
    AppState,
};
use axum::{
    body::{Body, HttpBody},
    extract::{ConnectInfo, Request, State},
    http::{header, HeaderMap, HeaderName, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    collections::{hash_map::Entry, HashMap},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::sync::watch;
use url::Url;

type Result<T> = std::result::Result<T, Failure>;
const PREFIX: &str = "/api/hub-meter";
const SPEND_LIMIT: &str = "Local provider budget reached.";

#[derive(Default)]
pub(crate) struct Registry(Mutex<HashMap<String, Arc<Run>>>);

struct Run {
    token: String,
    upstream: Url,
    client: reqwest::Client,
    payload: Value,
    claude: bool,
    exhausted: AtomicBool,
    stop: watch::Receiver<()>,
}

pub(crate) struct Handle<'a> {
    registry: &'a Registry,
    turn: &'a str,
    run: Arc<Run>,
    _stop: watch::Sender<()>,
    pub url: String,
    pub codex_provider: Option<String>,
}

impl Drop for Handle<'_> {
    fn drop(&mut self) {
        if let Ok(mut runs) = self.registry.0.lock() {
            runs.remove(self.turn);
        }
    }
}

impl Handle<'_> {
    pub fn secret(&self) -> &str {
        &self.run.token
    }

    pub fn failure(&self) -> Option<Failure> {
        self.run
            .exhausted
            .load(Ordering::Acquire)
            .then(|| Failure::new(403, "spend_limit", SPEND_LIMIT))
    }
}

impl Registry {
    pub fn secret(&self, turn: &str) -> Option<String> {
        Some(self.0.lock().ok()?.get(turn)?.token.clone())
    }
}

fn fail_closed() -> Failure {
    Failure::new(
        403,
        "spend_limit",
        "This provider cannot enforce the hub spend limit.",
    )
}

fn denied() -> Failure {
    Failure::new(403, "hub_meter_denied", "Hub meter request denied.")
}

fn upstream_failed() -> Failure {
    Failure::new(502, "hub_meter_upstream", "Provider request failed.")
}

fn upstream(value: &str) -> Result<Url> {
    Url::parse(value)
        .ok()
        .filter(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
        })
        .ok_or_else(fail_closed)
}

fn codex(environment: &HashMap<String, String>) -> Result<(Url, String)> {
    let home = environment.get("CODEX_HOME").ok_or_else(fail_closed)?;
    let folder = operator_file::open_directory(Path::new(home)).map_err(|_| fail_closed())?;
    let file =
        operator_file::file_at(&folder, "config.toml".as_ref()).map_err(|_| fail_closed())?;
    let bytes = operator_file::bounded_read(file, 1024 * 1024).map_err(|_| fail_closed())?;
    let config =
        toml_edit::Document::parse(std::str::from_utf8(&bytes).map_err(|_| fail_closed())?)
            .map_err(|_| fail_closed())?;
    let provider = config
        .get("model_provider")
        .and_then(|value| value.as_str())
        .filter(|provider| {
            !provider.is_empty()
                && provider
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                && !matches!(*provider, "openai" | "ollama" | "lmstudio")
        })
        .ok_or_else(fail_closed)?;
    let table = config
        .get("model_providers")
        .and_then(|value| value.get(provider))
        .ok_or_else(fail_closed)?;
    if table
        .get("requires_openai_auth")
        .and_then(|value| value.as_bool())
        == Some(true)
    {
        return Err(fail_closed());
    }
    let base = table
        .get("base_url")
        .and_then(|value| value.as_str())
        .ok_or_else(fail_closed)?;
    Ok((upstream(base)?, provider.to_owned()))
}

fn loopback_origin(state: &AppState) -> Result<String> {
    let mut origin = Url::parse(&state.origin).map_err(|_| fail_closed())?;
    let ip = match origin.host() {
        Some(url::Host::Ipv4(ip)) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        Some(url::Host::Ipv6(ip)) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        Some(url::Host::Ipv4(ip)) if ip.is_loopback() => IpAddr::V4(ip),
        Some(url::Host::Ipv6(ip)) if ip.is_loopback() => IpAddr::V6(ip),
        _ => return Err(fail_closed()),
    };
    origin.set_ip_host(ip).map_err(|_| fail_closed())?;
    Ok(origin.origin().ascii_serialization())
}

pub(crate) fn start<'a>(
    state: &'a AppState,
    turn: &'a str,
    runtime: &str,
    environment: &HashMap<String, String>,
    launch_args: &[String],
    test_gateway: bool,
    payload: Value,
) -> Result<Option<Handle<'a>>> {
    if !hub::provider_ceiling(state, turn)? {
        return Ok(None);
    }
    // ponytail: Custom arguments can select another backend. Support them once
    // the resulting provider configuration can be resolved before launch.
    if !launch_args.is_empty() {
        return Err(fail_closed());
    }
    let (upstream, codex_provider) = match runtime {
        "codex" if test_gateway => (
            upstream(
                environment
                    .get("MINARBOT_GATEWAY_BASE_URL")
                    .ok_or_else(fail_closed)?,
            )?,
            None,
        ),
        "codex" => {
            let (upstream, provider) = codex(environment)?;
            (upstream, Some(provider))
        }
        "claude" => {
            if [
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_USE_VERTEX",
                "CLAUDE_CODE_USE_FOUNDRY",
            ]
            .iter()
            .any(|key| {
                environment
                    .get(*key)
                    .is_some_and(|value| !matches!(value.as_str(), "" | "0"))
            }) {
                return Err(fail_closed());
            }
            let base = environment
                .get("ANTHROPIC_BASE_URL")
                .filter(|value| !value.is_empty())
                .map_or("https://api.anthropic.com", String::as_str);
            (upstream(base)?, None)
        }
        _ => return Err(fail_closed()),
    };
    let origin = loopback_origin(state)?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| upstream_failed())?;
    let mut bytes = [0_u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|_| Failure::storage())?;
    let token = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let url = format!("{origin}{PREFIX}/{turn}/{token}");
    let (stop, receiver) = watch::channel(());
    let run = Arc::new(Run {
        token,
        upstream,
        client,
        payload,
        claude: runtime == "claude",
        exhausted: AtomicBool::new(false),
        stop: receiver,
    });
    let registry = &state.computer.hub.meter;
    match registry
        .0
        .lock()
        .map_err(|_| Failure::storage())?
        .entry(turn.to_owned())
    {
        Entry::Occupied(_) => return Err(fail_closed()),
        Entry::Vacant(entry) => entry.insert(run.clone()),
    };
    Ok(Some(Handle {
        registry,
        turn,
        run,
        _stop: stop,
        url,
        codex_provider,
    }))
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(PREFIX, any(proxy))
        .route("/api/hub-meter/", any(proxy))
        .route("/api/hub-meter/{*path}", any(proxy))
}

async fn proxy(State(state): State<AppState>, request: Request) -> Response {
    forward(&state, request)
        .await
        .unwrap_or_else(|failure| crate::local_error(failure.status, &failure.message))
}

fn forwarded(mut headers: HeaderMap) -> HeaderMap {
    let connection = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect::<Vec<_>>();
    for name in connection {
        headers.remove(name);
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-connection",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "host",
    ] {
        headers.remove(name);
    }
    headers
}

fn active(state: &AppState, turn: &str, run: &Run) -> Result<()> {
    if run.stop.has_changed().is_err() {
        return Err(denied());
    }
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    if !crate::data::exists(
        &guard.connection,
        "SELECT 1 FROM turns WHERE id=? AND status='running' AND paused=0",
        [turn],
    )? {
        return Err(denied());
    }
    Ok(())
}

fn exhausted(run: &Run) -> Response {
    run.exhausted.store(true, Ordering::Release);
    let body = if run.claude {
        json!({"type":"error","error":{"type":"rate_limit_error","message":SPEND_LIMIT}})
    } else {
        json!({"error":{"type":"rate_limit_error","code":"spend_limit","message":SPEND_LIMIT}})
    };
    (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response()
}

// The dispatch receipt must also settle if the client drops the response body.
struct Receipt {
    state: AppState,
    effect: Option<Effect>,
    outcome: &'static str,
}
impl Receipt {
    async fn finish(&mut self) {
        let _ = hub::finish_effect(&self.state, self.effect.take(), self.outcome).await;
    }
}
impl Drop for Receipt {
    fn drop(&mut self) {
        if let Some(effect) = self.effect.take() {
            let state = self.state.clone();
            let outcome = self.outcome;
            tokio::spawn(async move {
                let _ = hub::finish_effect(&state, Some(effect), outcome).await;
            });
        }
    }
}

async fn forward(state: &AppState, request: Request) -> Result<Response> {
    if !request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_some_and(|peer| peer.0.ip().is_loopback())
    {
        return Err(denied());
    }
    let path = request
        .uri()
        .path()
        .strip_prefix("/api/hub-meter/")
        .ok_or_else(denied)?;
    let mut path = path.splitn(3, '/');
    let turn = path.next().ok_or_else(denied)?;
    let token = path.next().ok_or_else(denied)?;
    let rest = path.next().unwrap_or_default();
    let run = state
        .computer
        .hub
        .meter
        .0
        .lock()
        .map_err(|_| Failure::storage())?
        .get(turn)
        .cloned()
        .ok_or_else(denied)?;
    if !bool::from(run.token.as_bytes().ct_eq(token.as_bytes())) {
        return Err(denied());
    }
    active(state, turn, &run)?;
    let effect = if request.method() == Method::POST {
        match hub::before_effect(state, turn, "provider", run.payload.clone()).await {
            Ok(Some(effect)) => Some(effect),
            Ok(None) => return Err(denied()),
            Err(failure) if failure.code == "spend_limit" => return Ok(exhausted(&run)),
            Err(failure) => return Err(failure),
        }
    } else {
        hub::check(state, turn)?.ok_or_else(denied)?;
        None
    };
    let mut receipt = Receipt {
        state: state.clone(),
        effect,
        outcome: "unknown",
    };
    active(state, turn, &run)?;
    let mut target = run.upstream.as_str().trim_end_matches('/').to_owned();
    if !rest.is_empty() {
        target.push('/');
        target.push_str(rest);
    }
    if let Some(query) = request.uri().query() {
        target.push('?');
        target.push_str(query);
    }
    let (parts, body) = request.into_parts();
    let mut outbound = run
        .client
        .request(parts.method, target)
        .headers(forwarded(parts.headers));
    if !body.is_end_stream() {
        outbound = outbound.body(reqwest::Body::wrap_stream(body.into_data_stream()));
    }
    let mut stop = run.stop.clone();
    let mut response = tokio::select! {
        biased;
        _ = stop.changed() => return Err(denied()),
        response = outbound.send() => response.map_err(|_| upstream_failed())?,
    };
    // A provider response confirms dispatch even if the CLI stops reading.
    receipt.outcome = "succeeded";
    let status = response.status();
    let headers = forwarded(response.headers().clone());
    // A CLI following an upstream redirect could bypass the meter.
    if status.is_redirection() && headers.contains_key(header::LOCATION) {
        return Err(upstream_failed());
    }
    let mut response = Response::new(Body::from_stream(async_stream::stream! {
        loop {
            match response.chunk().await {
                Ok(Some(bytes)) => yield Ok(bytes),
                Ok(None) => break,
                Err(_) => {
                    receipt.outcome = "unknown";
                    receipt.finish().await;
                    yield Err(std::io::Error::other("Provider response failed."));
                    return;
                }
            }
        }
        receipt.finish().await;
    }));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    Ok(response)
}
