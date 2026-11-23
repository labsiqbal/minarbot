use crate::{config::Config, data::device_pairing as store, db, web::Failure, AppState};
use axum::{
    body::{to_bytes, Body},
    extract::{ConnectInfo, Request, State},
    http::{header, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Redirect, Response},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::StreamExt;
use qrcode::{render::svg, QrCode};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::json;
use std::{net::SocketAddr, time::Duration};

type Result<T> = std::result::Result<T, Failure>;

const COOKIE: &str = "__Host-minarbot-device";
const LEGACY_COOKIE: &str = "__Host-minarbot-device";
const MANAGE: &str = "/api/device-pairing";
const ISSUE: &str = "/api/device-pairing/code";
const REVOKE: &str = "/api/device-pairing/revoke";

fn denied() -> Failure {
    Failure::new(
        401,
        "device_pairing_required",
        "Pair this browser to open Minarbot.",
    )
}

fn forbidden() -> Failure {
    Failure::new(
        403,
        "pairing_origin_denied",
        "Open pairing from your Minarbot address.",
    )
}

fn failure(error: Failure) -> Response {
    crate::json_response(
        error.status,
        json!({"error":error.message,"code":error.code}),
    )
}

fn serving_origin<'a>(local: &'a str, config: &'a Config, host: &str) -> Option<&'a str> {
    [local]
        .into_iter()
        .chain(config.allowed_origins.iter().map(String::as_str))
        .find(|origin| {
            origin
                .split_once("://")
                .is_some_and(|(_, authority)| authority == host)
        })
}

fn trusted_loopback(
    local: &str,
    origin: Option<&str>,
    peer: Option<SocketAddr>,
    forwarded: bool,
    test_remote: bool,
) -> bool {
    !test_remote
        && !forwarded
        && origin == Some(local)
        && peer.is_some_and(|peer| peer.ip().is_loopback())
        && url::Url::parse(local)
            .ok()
            .is_some_and(|url| match url.host() {
                Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                _ => false,
            })
}

#[derive(Clone)]
pub(crate) struct BrowserAccess {
    local: bool,
    hash: Option<String>,
}

impl BrowserAccess {
    fn from_request(local: bool, request: &Request) -> Self {
        let cookies = |key| {
            request
                .headers()
                .get_all(header::COOKIE)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .flat_map(|value| value.split(';'))
                .filter_map(|pair| pair.trim().split_once('='))
                .filter_map(move |(name, value)| (name == key).then_some(value))
        };
        let mut modern = cookies(COOKIE).peekable();
        let mut legacy;
        let cookies: &mut dyn Iterator<Item = &str> = if modern.peek().is_some() {
            &mut modern
        } else {
            legacy = cookies(LEGACY_COOKIE);
            &mut legacy
        };
        let hash = cookies
            .next()
            .filter(|value| {
                cookies.next().is_none()
                    && value.len() == 43
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            })
            .map(db::digest);
        Self { local, hash }
    }

    fn device(&self, db: &Connection, now: i64, seen: bool) -> Result<Option<String>> {
        self.hash
            .as_ref()
            .map(|hash| store::device(db, hash, now, seen))
            .transpose()
            .map(Option::flatten)
    }

    pub(crate) fn paired(&self, db: &Connection, now: i64) -> Result<bool> {
        Ok(self.device(db, now, false)?.is_some())
    }

    fn admitted(&self, db: &Connection, now: i64, seen: bool) -> Result<bool> {
        Ok(self.local || !store::required(db)? || self.device(db, now, seen)?.is_some())
    }

    pub(crate) async fn wait_until_denied(&self, state: &AppState) {
        let mut changes = state.events.subscribe();
        let mut clock = tokio::time::interval(Duration::from_secs(15));
        loop {
            let admitted = state.database.lock().ok().is_some_and(|db| {
                self.admitted(&db.connection, state.config.now(), false)
                    .unwrap_or(false)
            });
            if !admitted {
                return;
            }
            tokio::select! {
                _ = async {
                    while let Ok(event) = changes.recv().await {
                        if event == "browser-pairing" { break; }
                    }
                } => {},
                _ = clock.tick() => {},
            }
        }
    }
}

async fn page(state: &AppState, status: StatusCode, error: &str, secure: bool) -> Response {
    let mut script = String::new();
    if secure {
        if let Some(document) =
            crate::appearance::load_document(&state.config.dist, crate::appearance::DOCUMENT).await
        {
            if let Some(src) = crate::appearance::startup_asset(&document) {
                script = format!(r#"<script type="module" src="{src}"></script>"#);
            }
        }
    }
    let pilot = !script.is_empty();
    let root = if pilot {
        r#"<div id="leptos-root" data-pairing></div>"#
    } else {
        ""
    };
    let form = r#"<form method="post" action="/pair">
<label for="code">Pairing code</label>
<input id="code" name="code" inputmode="numeric" autocomplete="one-time-code" pattern="[0-9]{6}" maxlength="6" required>
<label for="name">Device name</label>
<input id="name" name="name" maxlength="64" required>
<button type="submit">Pair device</button>
</form>"#;
    let error = html_escape(error);
    let content = if secure {
        form
    } else {
        "<p>Open Minarbot over HTTPS to pair this device.</p>"
    };
    let mut response = (
        status,
        Html(format!(
            r#"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<meta name="color-scheme" content="light dark">
<link rel="stylesheet" href="/pair.css">
<title>Pair device · Minarbot</title>
{script}</head><body><main id="appearance-fallback">
<h1>Pair this device</h1>
<p>Get a code from Settings &gt; Devices on a browser that can open Minarbot.</p>
{error}{content}
</main>{root}</body></html>"#
        )),
    )
        .into_response();
    if pilot {
        response.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(crate::appearance::POLICY),
        );
    }
    crate::security_headers(&mut response);
    // Native form POSTs need their same-origin Origin header for CSRF validation.
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    response
}

fn html_escape(message: &str) -> String {
    if message.is_empty() {
        return String::new();
    }
    format!(
        "<p role=\"alert\">{}</p>",
        message
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    )
}

fn secure(config: &Config, origin: &str) -> bool {
    origin.starts_with("https://")
        || config.test_remote_pairing && crate::config::trusted_origin(origin, true).is_ok()
}

fn pairing_origin<'a>(state: &'a AppState, origin: &'a str, local: bool) -> Option<&'a str> {
    if local {
        state
            .config
            .allowed_origins
            .first()
            .map(String::as_str)
            .or_else(|| secure(&state.config, origin).then_some(origin))
    } else {
        secure(&state.config, origin).then_some(origin)
    }
}

fn exact_origin(request: &Request, origin: &str) -> bool {
    crate::text_header(request.headers(), "origin") == Some(origin)
        && crate::text_header(request.headers(), "sec-fetch-site") != Some("cross-site")
}

fn form_field<'a>(fields: &'a [(String, String)], name: &str) -> &'a str {
    fields
        .iter()
        .find_map(|(key, value)| (key == name).then_some(value.as_str()))
        .unwrap_or("")
}

async fn pair(state: &AppState, request: Request, origin: &str) -> Result<Response> {
    let secure = secure(&state.config, origin);
    match *request.method() {
        Method::GET | Method::HEAD => Ok(page(state, StatusCode::OK, "", secure).await),
        Method::POST
            if secure
                && exact_origin(&request, origin)
                && crate::text_header(request.headers(), "content-type")
                    == Some("application/x-www-form-urlencoded") =>
        {
            let bytes = to_bytes(request.into_body(), 2048).await.map_err(|_| {
                Failure::new(413, "pairing_form_too_large", "Pairing form is too large.")
            })?;
            let fields: Vec<_> = url::form_urlencoded::parse(&bytes).into_owned().collect();
            let token = {
                let db = state.database.lock().map_err(|_| Failure::storage())?;
                store::redeem(
                    &db.connection,
                    origin,
                    form_field(&fields, "code"),
                    form_field(&fields, "name"),
                    state.config.now(),
                )
            };
            match token {
                Ok(token) => {
                    let mut response = Redirect::to("/").into_response();
                    set_cookies(&mut response, &token, store::COOKIE_AGE / 1000)?;
                    Ok(response)
                }
                Err(error) => Ok(page(
                    state,
                    StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                    &error.message,
                    true,
                )
                .await),
            }
        }
        _ => Err(forbidden()),
    }
}

fn set_cookies(response: &mut Response, token: &str, age: i64) -> Result<()> {
    for name in [COOKIE, LEGACY_COOKIE] {
        let cookie = HeaderValue::from_str(&format!(
            "{name}={token}; Path=/; Max-Age={age}; HttpOnly; Secure; SameSite=Strict"
        ))
        .map_err(|_| Failure::storage())?;
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignIn {
    required: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokeDevice {
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IssueDevice {
    origin: Option<String>,
}

async fn manage(
    state: &AppState,
    request: Request,
    access: &BrowserAccess,
    origin: &str,
) -> Result<Response> {
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    let body = if method == Method::POST {
        if !exact_origin(&request, origin)
            || crate::text_header(request.headers(), "content-type") != Some("application/json")
        {
            return Err(forbidden());
        }
        Some(to_bytes(request.into_body(), 1024).await.map_err(|_| {
            Failure::new(
                413,
                "pairing_request_too_large",
                "Pairing request is too large.",
            )
        })?)
    } else {
        None
    };
    let db = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &db.connection;
    let now = state.config.now();
    let current = access.device(db, now, true)?;
    let required = store::required(db)?;
    let can_require = access.local || current.is_some();
    if required && !can_require {
        return Err(denied());
    }
    let target = pairing_origin(state, origin, access.local);
    let invalid = |_| Failure::new(400, "invalid_pairing_request", "Invalid pairing request.");
    match (path.as_str(), method, body) {
        (MANAGE, Method::GET, _) => Ok(crate::json_response(
            200,
            json!({
                "required":required, "canRequire":can_require, "current":current,
                "pairingUrl":target.map(|target| format!("{target}/pair")), "devices":store::devices(db, now)?
            }),
        )),
        (MANAGE, Method::POST, Some(body)) => {
            let input: SignIn = serde_json::from_slice(&body).map_err(invalid)?;
            if input.required && !can_require {
                return Err(Failure::new(
                    403,
                    "pair_browser_first",
                    "Pair this browser before requiring remote sign-in.",
                ));
            }
            store::set_required(db, input.required)?;
            state.events.publish("browser-pairing".to_owned());
            Ok(crate::json_response(
                200,
                json!({ "required": input.required }),
            ))
        }
        (ISSUE, Method::POST, Some(body)) => {
            let input: IssueDevice = if body.is_empty() {
                IssueDevice { origin: None }
            } else {
                serde_json::from_slice(&body).map_err(invalid)?
            };
            let target = match input.origin.as_deref() {
                Some(requested)
                    if can_require
                        && state
                            .config
                            .allowed_origins
                            .iter()
                            .any(|origin| origin == requested) =>
                {
                    Some(requested)
                }
                Some(_) => return Err(forbidden()),
                None => target,
            };
            let target = target.ok_or_else(|| {
                Failure::new(
                    400,
                    "pairing_https_required",
                    "Set up an HTTPS address to pair a device.",
                )
            })?;
            let url = format!("{target}/pair");
            let qr = QrCode::new(url.as_bytes())
                .map_err(|_| Failure::storage())?
                .render::<svg::Color>()
                .min_dimensions(160, 160)
                .build();
            let (code, expires) = store::issue(db, target, now)?;
            Ok(crate::json_response(
                200,
                json!({
                    "code":code, "expiresAt":expires, "url":url,
                    "qr":format!("data:image/svg+xml;base64,{}", STANDARD.encode(qr))
                }),
            ))
        }
        (REVOKE, Method::POST, Some(body)) => {
            let input: RevokeDevice = serde_json::from_slice(&body).map_err(invalid)?;
            let id = crate::web::id(&input.id)
                .ok_or_else(|| Failure::new(400, "invalid_device", "Invalid device ID."))?;
            store::revoke(db, &id)?;
            state.events.publish("browser-pairing".to_owned());
            let mut response = crate::json_response(200, json!({"ok":true}));
            if current.as_deref() == Some(&id) {
                set_cookies(&mut response, "", 0)?;
            }
            Ok(response)
        }
        _ => Err(Failure::new(
            405,
            "method_not_allowed",
            "Method is not allowed.",
        )),
    }
}

async fn check(state: &AppState, mut request: Request, next: Next) -> Result<Response> {
    let host = crate::text_header(request.headers(), "host").unwrap_or("");
    let origin = serving_origin(&state.origin, &state.config, host);
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0);
    let forwarded = request.headers().keys().any(|name| {
        name.as_str() == "forwarded"
            || name.as_str().starts_with("x-forwarded-")
            || name.as_str().starts_with("tailscale-user-")
    });
    let local = trusted_loopback(
        &state.origin,
        origin,
        peer,
        forwarded,
        state.config.test_remote_pairing,
    );
    let path = request.uri().path();
    let pairing_path = matches!(path, "/pair" | MANAGE | ISSUE | REVOKE);
    if pairing_path {
        if crate::updates::pending(state) {
            return Err(Failure::new(
                503,
                "update_pending",
                "Update is being verified. Retry after restart.",
            ));
        }
        let origin = origin.ok_or_else(forbidden)?;
        if request.headers().contains_key(header::AUTHORIZATION) {
            return Err(forbidden());
        }
        if path == "/pair" {
            return pair(state, request, origin).await;
        }
        if crate::text_header(request.headers(), "sec-fetch-site") == Some("cross-site") {
            return Err(forbidden());
        }
        let access = BrowserAccess::from_request(local, &request);
        return manage(state, request, &access, origin).await;
    }
    if local {
        return Ok(next.run(request).await);
    }

    let access = BrowserAccess::from_request(false, &request);
    let (required, admitted) = {
        let db = state.database.lock().map_err(|_| Failure::storage())?;
        let required = store::required(&db.connection)?;
        (
            required,
            access
                .device(&db.connection, state.config.now(), true)?
                .is_some()
                || !required,
        )
    };
    let asset = crate::public_ui_asset(path)
        || (origin.is_some()
            && matches!(*request.method(), Method::GET | Method::HEAD)
            && crate::appearance::asset(path));
    if !admitted && !asset {
        if origin.is_some()
            && matches!(*request.method(), Method::GET | Method::HEAD)
            && path != "/api"
            && !path.starts_with("/api/")
        {
            return Ok(page(
                state,
                StatusCode::UNAUTHORIZED,
                "",
                secure(&state.config, origin.unwrap_or_default()),
            )
            .await);
        }
        return Err(denied());
    }
    if required
        && !asset
        && !matches!(*request.method(), Method::GET | Method::HEAD)
        && !origin.is_some_and(|origin| exact_origin(&request, origin))
    {
        return Err(forbidden());
    }

    request.extensions_mut().insert(access.clone());
    let response = next.run(request).await;
    if crate::text_header(response.headers(), "content-type") != Some("text/event-stream") {
        return Ok(response);
    }
    let (parts, body) = response.into_parts();
    let state = state.clone();
    let stream = async_stream::stream! {
        let denied = access.wait_until_denied(&state);
        tokio::pin!(denied);
        let mut data = body.into_data_stream();
        loop {
            tokio::select! {
                biased;
                _ = &mut denied => break,
                chunk = data.next() => match chunk { Some(chunk) => yield chunk, None => break },
            }
        }
    };
    Ok(Response::from_parts(parts, Body::from_stream(stream)))
}

pub(crate) async fn gate(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if state.auth.is_some() || request.uri().path() == crate::connectors::mailbox::INBOUND {
        return next.run(request).await;
    }
    let path = request.uri().path().to_owned();
    match check(&state, request, next).await {
        Ok(mut response) => {
            if (matches!(path.as_str(), "/pair" | MANAGE | ISSUE | REVOKE)
                || response.status() == StatusCode::UNAUTHORIZED)
                && !response
                    .headers()
                    .contains_key(header::CONTENT_SECURITY_POLICY)
            {
                crate::security_headers(&mut response);
            }
            response
        }
        Err(error) => {
            let mut response = failure(error);
            crate::security_headers(&mut response);
            response
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_direct_loopback_bypasses_pairing() -> std::result::Result<(), std::net::AddrParseError>
    {
        let local = "http://127.0.0.1:4412";
        let peer = Some("127.0.0.1:51121".parse()?);
        let remote = Some("100.64.0.12:51121".parse()?);
        assert!(trusted_loopback(local, Some(local), peer, false, false));
        assert!(!trusted_loopback(local, Some(local), peer, true, false));
        assert!(!trusted_loopback(local, Some(local), remote, false, false));
        assert!(!trusted_loopback(
            local,
            Some("https://minarbot.example"),
            peer,
            false,
            false
        ));
        assert!(!trusted_loopback(local, Some(local), peer, false, true));
        assert!(!trusted_loopback(local, Some(local), None, false, false));
        Ok(())
    }

    #[test]
    fn device_cookie_is_host_only_and_secure() -> Result<()> {
        let mut response = ().into_response();
        set_cookies(&mut response, "token", 10)?;
        let cookies = response.headers().get_all(header::SET_COOKIE);
        let mut cookies = cookies.iter();
        for name in [COOKIE, LEGACY_COOKIE] {
            let cookie = cookies.next().ok_or_else(Failure::storage)?;
            assert_eq!(
                cookie,
                format!("{name}=token; Path=/; Max-Age=10; HttpOnly; Secure; SameSite=Strict")
                    .as_str()
            );
        }
        assert!(cookies.next().is_none());
        Ok(())
    }
}
