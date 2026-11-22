#![forbid(unsafe_code)]
mod appearance;
mod auth;
mod automation;
mod computer;
mod config;
mod connectors;
mod data;
mod db;
mod events;
mod files;
mod harness;
mod mcp;
mod media;
mod operator_file;
mod platform;
mod restore;
mod restore_cli;
mod restore_fence;
mod script_cli;
mod skill_directory;
mod system_keyring;
mod tasks;
mod turns;
mod updates;
mod web;

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use config::Config;
use serde_json::{json, Value};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::net::TcpListener;
use web::Failure;

const POLICY: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-src 'self' https: http:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

#[derive(Clone)]
pub(crate) struct AppState {
    database: Arc<Mutex<db::Database>>,
    config: Arc<Config>,
    origin: String,
    auth: Option<auth::Auth>,
    events: events::Hub,
    turns: turns::Runtime,
    whatsapp: Arc<connectors::whatsapp::Runtime>,
    computer: Arc<computer::Manager>,
    pub(crate) trial_pending: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

fn json_response(status: u16, value: Value) -> Response {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        value.to_string(),
    )
        .into_response()
}
fn local_error(status: u16, message: &str) -> Response {
    json_response(status, json!({"error":message}))
}
fn security_headers(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    if !response
        .headers()
        .contains_key(header::CONTENT_SECURITY_POLICY)
    {
        response.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(POLICY),
        );
    }
}
fn text_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}
pub(crate) fn allowed(state: &AppState, value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        value == state.origin
            || state
                .config
                .allowed_origins
                .iter()
                .any(|item| item == value)
    })
}

async fn dispatch(State(state): State<AppState>, request: Request) -> Response {
    if state.auth.is_some() {
        return canonical(&state, request).await;
    }
    let host = text_header(request.headers(), "host").unwrap_or("");
    if !updates::transport(request.uri().path())
        && format!("http://{host}") != state.origin
        && !state
            .config
            .allowed_origins
            .contains(&format!("https://{host}"))
        || text_header(request.headers(), "sec-fetch-site") == Some("cross-site")
    {
        let mut response = local_error(
            403,
            "Access is only allowed through the local Minarbot address.",
        );
        security_headers(&mut response);
        return response;
    }
    let mut response = local(&state, request)
        .await
        .unwrap_or_else(|error| local_error(error.status, &error.message));
    let cache = response.headers().get(header::CACHE_CONTROL).cloned();
    security_headers(&mut response);
    if let Some(cache) = cache {
        response.headers_mut().insert(header::CACHE_CONTROL, cache);
    }
    response
}
async fn local(state: &AppState, request: Request) -> Result<Response, Failure> {
    let path = request.uri().path();
    if updates::pending(state) && !["/api/environment", "/api/updates"].contains(&path) {
        return Ok(local_error(
            503,
            "Update is being verified. Retry after restart.",
        ));
    }
    if matches!(
        path,
        "/api/vault" | "/api/vault/delete" | "/api/vault/respond"
    ) {
        return computer::vault::local(state, request).await;
    }
    if path.starts_with("/api/scripts/") {
        return Box::pin(automation::scripts::local(state, request)).await;
    }
    if text_header(request.headers(), "authorization")
        .is_some_and(|value| value.starts_with("Bearer bsi_"))
    {
        return Ok(local_error(401, "Unauthorized request."));
    }
    let scope = events::scope(path);
    if scope.is_some()
        || matches!(
            path,
            "/api/sidebar/stream" | "/api/automation-calendar/stream"
        )
    {
        if request.headers().contains_key("authorization")
            || request.headers().contains_key("origin")
                && !allowed(state, text_header(request.headers(), "origin"))
        {
            return Ok(local_error(
                403,
                "Streaming requires the local owner in Minarbot.",
            ));
        }
        if request.method() != Method::GET {
            return Ok(local_error(405, "Streaming requires GET."));
        }
        if request.uri().query().is_some()
            && request.uri().path() != "/api/sync"
            && !tasks::query_allowed(
                request.uri().path(),
                request.method(),
                &data::query(&request),
            )
        {
            return Ok(local_error(
                400,
                "Streaming does not accept query parameters.",
            ));
        }
        return match scope {
            Some(scope) => state.events.response(scope, &state.database),
            None if path == "/api/automation-calendar/stream" => Ok(state.events.calendar()),
            None => Ok(state.events.sidebar(&state.database)),
        };
    }
    if updates::handles(path) {
        return updates::local(state, request).await;
    }
    if harness::handles(path, request.method()) {
        return harness::local(state, request).await;
    }
    if mcp::handles(path) {
        return mcp::local(state, request).await;
    }
    if turns::handles(path, request.method()) {
        return turns::local(state, request).await;
    }
    if data::handles(path) {
        return data::local(state, request).await;
    }
    if (request.method() == Method::GET
        || request.method() == Method::HEAD && path.starts_with(appearance::NAMESPACE))
        && path != "/api"
        && !path.starts_with("/api/")
    {
        return static_ui(
            state,
            path,
            request.uri().query().unwrap_or(""),
            accepts_gzip(text_header(request.headers(), "accept-encoding")),
        )
        .await;
    }
    Ok(local_error(404, "Route not found."))
}
fn accepts_gzip(header: Option<&str>) -> bool {
    header.is_some_and(|value| {
        value.split(',').any(|item| {
            let mut parts = item.split(';').map(str::trim);
            parts
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case("gzip"))
                && !parts.any(|param| {
                    param
                        .strip_prefix("q=")
                        .and_then(|q| q.parse::<f32>().ok())
                        .is_some_and(|q| q <= 0.0)
                })
        })
    })
}
fn gzip(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write;
    let mut encoder = GzEncoder::new(Vec::with_capacity(bytes.len() / 3), Compression::default());
    encoder.write_all(bytes)?;
    encoder.finish()
}
pub(crate) fn public_ui_asset(path: &str) -> bool {
    matches!(
        path,
        "/manifest.webmanifest"
            | "/icons/icon-192.png"
            | "/icons/icon-512.png"
            | "/icons/icon-maskable-512.png"
            | "/pair.css"
    )
}
async fn static_ui(
    state: &AppState,
    path: &str,
    query: &str,
    gzip_ok: bool,
) -> Result<Response, Failure> {
    let leptos_asset = path.starts_with(appearance::NAMESPACE);
    if leptos_asset && !appearance::asset(path) {
        return Ok(local_error(404, "Page not found."));
    }
    let hashed = path.starts_with("/assets/") || leptos_asset;
    let spa = !hashed && !public_ui_asset(path);
    let document = appearance::document_for_route(path, query);
    let leptos_document = state.config.dist.join(document);
    let pilot = spa
        && state.config.appearance_client == config::AppearanceClient::Leptos
        && !appearance::react_override(query)
        && tokio::fs::try_exists(&leptos_document)
            .await
            .unwrap_or(false);
    let leptos_page = pilot
        .then(|| appearance::route_title(state, path, query))
        .flatten();
    let conversation_page = spa
        && state.config.appearance_client == config::AppearanceClient::Leptos
        && !appearance::react_override(query)
        && appearance::conversation_route(path, query)
        && (path != "/" || appearance::route_title(state, path, query).is_none())
        && tokio::fs::try_exists(state.config.dist.join(appearance::CONVERSATION_DOCUMENT))
            .await
            .unwrap_or(false);
    let file = state.config.dist.join(if spa {
        if leptos_page.is_some() {
            document
        } else if conversation_page {
            appearance::CONVERSATION_DOCUMENT
        } else {
            "index.html"
        }
    } else {
        path.trim_start_matches('/')
    });
    if !spa && path.split('/').any(|part| part == "..") {
        return Ok(local_error(404, "Page not found."));
    }
    let precompressed = if leptos_asset && gzip_ok {
        let mut gzip_file = file.as_os_str().to_owned();
        gzip_file.push(".gz");
        tokio::fs::read(gzip_file).await.ok()
    } else {
        None
    };
    let encoded = precompressed.is_some();
    let bytes = match precompressed {
        Some(bytes) => Ok(bytes),
        None => tokio::fs::read(&file).await,
    };
    let Ok(mut bytes) = bytes else {
        return Ok(if leptos_asset {
            local_error(404, "Page not found.")
        } else {
            local_error(404, "App has not been built yet. Run npm run build.")
        });
    };
    let mut policy = None;
    if let Some(title) = leptos_page {
        bytes =
            appearance::document(&String::from_utf8_lossy(&bytes), path, title, query).into_bytes();
        policy = Some(appearance::POLICY.to_owned());
    } else if conversation_page {
        bytes = appearance::conversation_document(&String::from_utf8_lossy(&bytes), path, query)
            .into_bytes();
        policy = Some(appearance::POLICY.to_owned());
    } else if spa {
        let html = String::from_utf8_lossy(&bytes);
        let Some(end) = html.find("</head>") else {
            return Ok(local_error(500, "Built app is missing </head>."));
        };
        let mut random = [0; 16];
        getrandom::getrandom(&mut random).map_err(|_| Failure::storage())?;
        let nonce = STANDARD.encode(random);
        let selector = if pilot {
            "<meta name=\"minarbot-appearance-client\" content=\"leptos\">"
        } else {
            ""
        };
        bytes = format!(
            "{}<meta name=\"csp-nonce\" content=\"{nonce}\">{selector}{}",
            &html[..end],
            &html[end..]
        )
        .into_bytes();
        policy = Some(POLICY.replace(
            "style-src 'self'",
            &format!("style-src 'self' 'nonce-{nonce}'"),
        ));
    }
    let mime = match file.extension().and_then(|value| value.to_str()) {
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("webmanifest") => "application/manifest+json",
        Some("html") => "text/html",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("woff2") => "font/woff2",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    };
    let status = if spa && !navigation(path) {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::OK
    };
    let compressible = matches!(
        mime,
        "text/html" | "text/javascript" | "text/css" | "image/svg+xml" | "application/wasm"
    );
    let compress = !leptos_asset && compressible && gzip_ok && bytes.len() > 1024;
    if compress {
        bytes = tokio::task::spawn_blocking(move || gzip(&bytes))
            .await
            .map_err(|_| Failure::storage())?
            .map_err(|_| Failure::storage())?;
    }
    let mut response = (status, [(header::CONTENT_TYPE, mime)], bytes).into_response();
    if compressible {
        response
            .headers_mut()
            .insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    }
    if encoded || compress {
        response
            .headers_mut()
            .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    }
    if hashed {
        // Both client builds name immutable assets by content hash.
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
    }
    if let Some(policy) = policy {
        response.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_str(&policy).map_err(|_| Failure::storage())?,
        );
    }
    Ok(response)
}
fn navigation(path: &str) -> bool {
    fn id(value: &str) -> bool {
        value.len() == 36
            && value.bytes().enumerate().all(|(index, byte)| {
                if [8, 13, 18, 23].contains(&index) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
    }
    let parts: Vec<_> = path.split('/').skip(1).collect();
    match parts.as_slice() {
        [""] | ["routines"] | ["workflows"] | ["calendar"] => true,
        ["settings", section] => [
            "general",
            "appearance",
            "providers",
            "engines",
            "skills",
            "connected-apps",
            "hub",
            "mcp",
            "machines",
            "devices",
            "computer",
            "craft-anything",
            "updates",
            "usage",
            "data",
            "backup",
            "mcp-server",
            "email",
            "vault",
        ]
        .contains(section),
        ["agents" | "rooms" | "projects", value] => id(value),
        ["agents", agent, "inbox"] => id(agent),
        ["agents", agent, "conversations", thread] => id(agent) && id(thread),
        ["machines", machine, "agents", agent] => id(machine) && id(agent),
        _ => false,
    }
}
async fn canonical(state: &AppState, request: Request) -> Response {
    let mut request_id = String::new();
    let result = canonical_request(state, request, &mut request_id).await;
    let mut response = match result {
        Ok(value) => value,
        Err(error) => {
            let mut value = json!({"ok":false,"error":{"code":error.code,"message":error.message,"retryable":error.retryable,"requestId":request_id}});
            if let Some(details) = error.details {
                value["error"]["details"] = details;
            }
            let mut response = json_response(error.status, value);
            if error.status == 413 {
                response
                    .headers_mut()
                    .insert(header::CONNECTION, HeaderValue::from_static("close"));
            }
            response
        }
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert("x-minarbot-protocol", HeaderValue::from_static("1"));
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response
            .headers_mut()
            .insert("x-minarbot-request-id", value);
    }
    response
}
async fn canonical_request(
    state: &AppState,
    request: Request,
    request_id: &mut String,
) -> Result<Response, Failure> {
    if (request.uri().path().starts_with("/api/turn-waits/")
        || request.uri().path().starts_with("/api/workflow-waits/"))
        && request.uri().path().ends_with("/events")
    {
        return tasks::wait_routes::event(state, request, request_id).await;
    }
    if request.uri().path().starts_with("/api/projects/")
        && request.uri().path().ends_with("/webhook-events")
    {
        return automation::project::event(state, request, request_id)
            .await?
            .ok_or_else(|| Failure::new(404, "resource_not_found", "Resource not found."));
    }
    if text_header(request.headers(), "x-minarbot-protocol") != Some("1") {
        return Err(Failure::new(
            426,
            "unsupported_version",
            "Unsupported protocol version.",
        ));
    }
    let media = text_header(request.headers(), "content-type")
        .unwrap_or("")
        .to_ascii_lowercase();
    let valid_media = media == "application/json"
        || media.split_once(';').is_some_and(|(kind, parameter)| {
            kind.trim_end() == "application/json" && parameter.trim_start() == "charset=utf-8"
        });
    if !valid_media {
        return Err(Failure::new(
            415,
            "unsupported_media_type",
            "Use Content-Type application/json.",
        ));
    }
    let id = text_header(request.headers(), "x-minarbot-request-id")
        .and_then(web::id)
        .ok_or_else(|| Failure::new(400, "invalid_id", "requestId must be a UUID v4."))?;
    *request_id = id;
    let feature = automation::project::feature(request.uri().path());
    if feature && !automation::project::valid(request.uri().path()) {
        return Err(Failure::new(
            404,
            "resource_not_found",
            "Resource not found.",
        ));
    }
    if request.uri().query().is_some()
        && request.uri().path() != "/api/sync"
        && !feature
        && !tasks::query_allowed(
            request.uri().path(),
            request.method(),
            &data::query(&request),
        )
    {
        return Err(Failure::new(400, "invalid_route", "Invalid route."));
    }
    let _capacity =
        data::limits::reserve(request.uri().path(), request.method(), request.headers())?;
    if tasks::device_path(request.uri().path()) {
        return tasks::device(state, request, request_id).await;
    }
    if computer::devices::host_path(request.uri().path()) {
        return computer::devices::host(state, request, request_id).await;
    }
    let auth = state.auth.as_ref().ok_or_else(Failure::storage)?;
    let actor = auth
        .authenticate(
            &state.database,
            request.headers(),
            request.method(),
            state.config.now(),
        )
        .await?;
    let query = data::query(&request);
    let headers = request.headers().clone();
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    let body = if method != Method::GET && method != Method::HEAD {
        let limit = data::limits::body_limit(&path, &method, &headers);
        let bytes = axum::body::to_bytes(request.into_body(), limit)
            .await
            .map_err(|_| {
                let mut error =
                    Failure::new(413, "payload_too_large", "Payload exceeds the limit.");
                error.details = Some(json!({"maxBytes":limit}));
                error
            })?;
        let value = web::parse_json(&bytes)?;
        let body_id = value.get("requestId").and_then(Value::as_str);
        if let Some(body_id) = body_id {
            let body_id = web::id(body_id)
                .ok_or_else(|| Failure::new(400, "invalid_id", "ID must be a UUID v4."))?;
            if body_id != request_id.as_str() {
                return Err(Failure::new(
                    400,
                    "request_id_mismatch",
                    "Header and body IDs must match.",
                ));
            }
        } else {
            return Err(Failure::new(
                400,
                "request_id_mismatch",
                "Header and body IDs must match.",
            ));
        }
        value
    } else {
        json!({})
    };
    if path == "/api/restore/reconcile" && method == Method::POST {
        let value = restore::reconcile(state, &actor, &body)?;
        return Ok(json_response(200, json!({"ok":true,"data":value})));
    }
    if let Some(response) = automation::project::route(
        state,
        &path,
        &method,
        &body,
        &actor.account,
        &query,
        &headers,
    )? {
        return Ok(response);
    }
    if path == "/api/session" && method == Method::GET {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let active: i64 = guard.connection.query_row(
            "SELECT count(*) FROM authority_state WHERE id=1 AND serving_state='active'",
            [],
            |row| row.get(0),
        )?;
        if active == 0 {
            let mut error =
                Failure::new(503, "authority_unavailable", "Authority is not active yet.");
            error.retryable = true;
            return Err(error);
        }
        let mut query = guard.connection.prepare("SELECT p.id,p.name,p.private,p.revision,m.role,m.grant_revision FROM projects p JOIN project_members m ON m.project=p.id WHERE m.account=? AND m.deleted=0 AND p.deleted=0 ORDER BY p.id")?;
        let projects = query.query_map([&actor.account], |row| Ok(json!({"id":row.get::<_,String>(0)?,"name":row.get::<_,String>(1)?,"private":row.get::<_,i64>(2)?,"revision":row.get::<_,i64>(3)?,"role":row.get::<_,String>(4)?,"grant_revision":row.get::<_,i64>(5)?})))?.collect::<Result<Vec<_>,_>>()?;
        return Ok(json_response(
            200,
            json!({"ok":true,"data":{"accountId":actor.account,"projects":projects}}),
        ));
    }
    if let Some(response) =
        computer::devices::human(state, &path, &method, &body, &actor, &headers).await?
    {
        return Ok(response);
    }
    if let Some(response) =
        tasks::wait_routes::human_route(state, &actor, &path, &method, &body).await?
    {
        return Ok(response);
    }
    if let Some(response) =
        tasks::human(state, &path, &method, &body, &actor, &query, &headers).await?
    {
        return Ok(response);
    }
    if let Some(response) = data::canonical(state, &path, &method, &body, &actor, &query).await? {
        return Ok(response);
    }
    if path == "/api/session/logout" && method == Method::POST {
        if let Some(key) = body
            .as_object()
            .and_then(|map| map.keys().find(|key| *key != "requestId"))
        {
            return Err(Failure::new(
                400,
                "unknown_field",
                format!("Field {key} is unknown."),
            ));
        }
        let value = auth
            .logout(&state.database, &actor, state.config.now())
            .await?;
        return Ok(json_response(200, json!({"ok":true,"data":value})));
    }
    Err(Failure::new(
        404,
        "resource_not_found",
        "Route is not available yet.",
    ))
}

fn initialize_local(database: &db::Database) -> db::Result<()> {
    let db = &database.connection;
    files::init(db)?;
    data::initialize(database)?;
    turns::initialize(db).map_err(|e| e.message)?;
    connectors::initialize(db).map_err(|e| e.message)?;
    automation::initialize(db).map_err(|e| e.message)?;
    harness::initialize(db).map_err(|e| e.message)?;
    mcp::initialize(db).map_err(|e| e.message)?;
    mcp::guide::initialize(db).map_err(|e| e.message)?;
    computer::initialize(db)?;
    updates::initialize(db)?;
    db.execute_batch(include_str!("data/attention.sql"))?;
    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> db::Result<()> {
    if std::env::args().nth(1).as_deref() == Some(mcp::guide_infra::ASKPASS_COMMAND) {
        return mcp::guide_infra::askpass();
    }
    if std::env::args().nth(1).as_deref() == Some("codegraph") {
        return mcp::codegraph::codegraph_command(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if std::env::args().nth(1).as_deref() == Some("--desktop-supervisor") {
        if !platform::LINUX_SANDBOX {
            return Err(platform::COMPUTER_UNAVAILABLE.into());
        }
        return computer::desktop_supervisor::run().await;
    }
    if std::env::args().nth(1).as_deref() == Some("--craft-mcp") {
        return computer::craft::supervise().await;
    }
    if std::env::args().nth(1).as_deref() == Some("--device-host") {
        if !platform::LINUX_SANDBOX {
            return Err(platform::COMPUTER_UNAVAILABLE.into());
        }
        return computer::devices::host::run().await;
    }
    if std::env::args().nth(1).as_deref() == Some("--connector-journal-config") {
        return connectors::runner::journal();
    }
    if std::env::args().nth(1).as_deref() == Some("--connector-config") {
        return connectors::runner::run().await;
    }
    if std::env::args().nth(1).as_deref() == Some("mail-keyring") {
        return connectors::mail::keyring_command(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if std::env::args().nth(1).as_deref() == Some("--mcp-stdio") {
        return mcp::stdio().await;
    }
    if let Some(command @ ("script" | "restore")) = std::env::args().nth(1).as_deref() {
        let args = std::env::args().skip(2).collect::<Vec<_>>();
        let result = if command == "restore" {
            restore_cli::run(&args).await
        } else {
            script_cli::run(&args).await
        };
        println!("{result}");
        std::process::exit(result["exitCode"].as_i64().unwrap_or(1) as i32);
    }
    if std::env::args().nth(1).as_deref() == Some("skills") {
        match skill_directory::run(&std::env::args().skip(2).collect::<Vec<_>>()) {
            Ok(result) => println!("{}", serde_json::to_string_pretty(&result)?),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("--restore-read-only") {
        let [source, target]: [std::ffi::OsString; 2] = std::env::args_os()
            .skip(2)
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| "Use --restore-read-only BACKUP NEW_DATABASE.")?;
        println!("{}", restore::read_only(source.as_ref(), target.as_ref())?);
        return Ok(());
    }
    let mut config = Config::read()?;
    harness::hydrate(&mut config.harness).await;
    let _script_discovery = automation::scripts::open(&config)?;
    let database = Arc::new(Mutex::new(db::open(
        &config.database,
        config.auth.is_some(),
        config.now(),
    )?));
    let harness_homes = harness::config::prepare_homes(&config.database)?;
    let events = events::Hub::new();
    {
        let guard = database.lock().map_err(|_| "Database lock poisoned")?;
        if config.auth.is_none() {
            initialize_local(&guard)?;
        } else {
            db::provider_tables(&guard.connection)?;
            data::initialize_canonical(&guard)?;
            turns::initialize_canonical(&guard.connection).map_err(|e| e.message)?;
            computer::initialize_canonical(&guard.connection)?;
            connectors::initialize(&guard.connection).map_err(|e| e.message)?;
            harness::initialize(&guard.connection).map_err(|e| e.message)?;
            mcp::initialize(&guard.connection).map_err(|e| e.message)?;
            files::init_canonical(&guard.connection)?;
            automation::project::initialize(&guard.connection).map_err(|e| e.message)?;
        }
        data::agent_folders::warn_unknown_bindings(&config, &guard.connection)
            .map_err(|e| e.message)?;
        events.watch(&guard.connection);
    }
    let listener = TcpListener::bind((config.host.as_str(), config.port)).await?;
    let origin = format!(
        "{}://{}",
        if config.tls.is_some() {
            "https"
        } else {
            "http"
        },
        listener.local_addr()?
    );
    let tls = config.tls.clone();
    let auth = config
        .auth
        .clone()
        .map(|auth| auth::Auth::new(auth, config.remaining_data.test_canonical_bridge.clone()))
        .transpose()?;
    let trial_pending = updates::trial(&config);
    let computer = Arc::new(computer::Manager::new(&config)?);
    {
        let guard = database.lock().map_err(|_| "Database lock poisoned")?;
        computer.load_browser_profiles(&guard.connection)?;
    }
    let state = AppState {
        database,
        config: Arc::new(config),
        origin: origin.clone(),
        auth,
        events,
        trial_pending,
        turns: turns::Runtime::new()?,
        whatsapp: Default::default(),
        computer: computer.clone(),
    };
    if state.auth.is_none() {
        let db = state
            .database
            .lock()
            .map_err(|_| "Database lock poisoned")?;
        mcp::guide::inherit_connection(&state, &db.connection).map_err(|e| e.message)?;
    }
    automation::scripts::sync(&state).map_err(|e| e.message)?;
    let memory_task = if state.auth.is_none() {
        let db = state
            .database
            .lock()
            .map_err(|_| "Database lock poisoned")?;
        turns::memory_worker::initialize(&db.connection)?;
        if !state.config.harness.test_mode || state.config.test_memory_extraction {
            Some(turns::memory_worker::start(state.clone()))
        } else {
            None
        }
    } else {
        None
    };
    let turn_task = turns::start(state.clone()).map_err(|e| e.message)?;
    let automation_task = automation::start(state.clone());
    let computer_maintenance = computer::maintenance(state.clone());
    let conversation_maintenance = data::maintenance(state.clone());
    let codegraph_task = mcp::codegraph::start(state.clone());
    let trial_watch = updates::watch_trial(state.clone());
    let maintenance_state = state.clone();
    let maintenance = tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(30));
        loop {
            timer.tick().await;
            if let Some(auth) = &maintenance_state.auth {
                let _ = auth.retry_logouts(&maintenance_state.database).await;
            }
            let _ = computer::devices::maintain(&maintenance_state);
        }
    });
    let shutdown_state = state.clone();
    let discord_task = connectors::discord::start(state.clone());
    let connector_maintenance = connectors::mail::start(state.clone());
    let whatsapp_maintenance = connectors::whatsapp::start(state.clone());
    let router = Router::new()
        .merge(computer::routes())
        .merge(harness::routes())
        .merge(mcp::routes())
        .merge(mcp::guide::routes())
        .merge(files::routes())
        .merge(updates::routes())
        .merge(turns::routes())
        .merge(tasks::routes())
        .merge(connectors::routes())
        .merge(automation::routes())
        .route("/api/session", any(dispatch))
        .route("/api/session/logout", any(dispatch))
        .route("/api/restore/reconcile", any(dispatch))
        .route("/api/vault", any(dispatch))
        .route("/api/vault/delete", any(dispatch))
        .route("/api/vault/respond", any(dispatch))
        .route("/api/agents", any(dispatch))
        .route("/api/sidebar/stream", any(dispatch))
        .route("/api/automation-calendar/stream", any(dispatch))
        .route("/api/agents/{id}/turns/stream", any(dispatch))
        .route("/api/threads/{id}/turns/stream", any(dispatch))
        .route("/api/channels/{id}/turns/stream", any(dispatch))
        .merge(data::routes())
        .fallback(dispatch)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::pairing::gate,
        ))
        .with_state(state);
    println!("Minarbot: {origin}");
    let result = if let Some(tls) = tls {
        let configuration = axum_server::tls_rustls::RustlsConfig::from_pem(
            tls.cert.into_bytes(),
            tls.key.into_bytes(),
        )
        .await?;
        let handle = axum_server::Handle::new();
        let stop = handle.clone();
        let task = tokio::spawn(async move {
            shutdown().await;
            let _ = computer::hub::stop(&shutdown_state).await;
            if let Err(error) = turns::stop(&shutdown_state) {
                eprintln!("Turn shutdown failed: {}", error.message);
            }
            if let Err(error) = computer::devices::close(&shutdown_state) {
                eprintln!("Device shutdown failed: {}", error.message);
            }
            harness::stop(&shutdown_state).await;
            shutdown_state.events.close();
            stop.graceful_shutdown(Some(Duration::from_secs(5)));
        });
        let result = axum_server::from_tcp_rustls(listener.into_std()?, configuration)?
            .handle(handle)
            .serve(router.into_make_service_with_connect_info::<SocketAddr>())
            .await;
        task.abort();
        result
    } else {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            shutdown().await;
            let _ = computer::hub::stop(&shutdown_state).await;
            if let Err(error) = turns::stop(&shutdown_state) {
                eprintln!("Turn shutdown failed: {}", error.message);
            }
            if let Err(error) = computer::devices::close(&shutdown_state) {
                eprintln!("Device shutdown failed: {}", error.message);
            }
            harness::stop(&shutdown_state).await;
            shutdown_state.events.close();
        })
        .await
    };
    turn_task.abort();
    if let Some(task) = memory_task {
        task.abort();
    }
    automation_task.abort();
    maintenance.abort();
    conversation_maintenance.abort();
    codegraph_task.abort();
    connector_maintenance.abort();
    discord_task.abort();
    whatsapp_maintenance.abort();
    for task in computer_maintenance {
        task.abort();
    }
    computer.close().await;
    trial_watch.abort();
    result?;
    harness_homes.unlock()?;
    Ok(())
}

async fn shutdown() {
    let interrupt = tokio::signal::ctrl_c();
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    tokio::select! { _ = interrupt => (), _ = terminate => (), _ = platform::launcher_exit() => () }
}

#[cfg(test)]
mod tests {
    use super::navigation;
    #[test]
    fn navigation_matches_node_paths() {
        assert!(navigation("/settings/engines"));
        for path in ["/", "/routines", "/workflows", "/settings/appearance", "/settings/email", "/agents/00000000-0000-4000-8000-000000000000", "/agents/00000000-0000-4000-8000-000000000000/inbox", "/agents/00000000-0000-4000-8000-000000000000/conversations/00000000-0000-4000-8000-000000000001"] { assert!(navigation(path), "{path}"); }
        for path in [
            "/settings/nope",
            "/agents/malformed",
            "/rooms/deleted",
            "/projects/deleted",
            "/nonsense/path",
            "/missing.txt",
        ] {
            assert!(!navigation(path), "{path}");
        }
    }
}

#[cfg(test)]
mod static_tests {
    use super::*;

    #[test]
    fn gzip_acceptance_honours_q_values() {
        for (header, expected) in [
            (None, false),
            (Some("identity"), false),
            (Some("gzip"), true),
            (Some("GZIP"), true),
            (Some("br, gzip;q=0.5"), true),
            (Some("gzip;q=0"), false),
            (Some("gzip; q=0.0, deflate"), false),
            (Some("x-gzip"), false),
        ] {
            assert_eq!(accepts_gzip(header), expected, "{header:?}");
        }
    }

    #[test]
    fn gzip_round_trips() -> std::io::Result<()> {
        use std::io::Read;
        let text = "const x = 1;\n".repeat(400);
        let packed = gzip(text.as_bytes())?;
        assert!(packed.len() < text.len() / 10);
        let mut unpacked = String::new();
        flate2::read::GzDecoder::new(&packed[..]).read_to_string(&mut unpacked)?;
        assert_eq!(unpacked, text);
        Ok(())
    }
}
