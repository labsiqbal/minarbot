//! Helpers shared by the `pr_*` peer and multi-server modules (#89 peers
//! slice). They only send requests and read files; every assertion stays
//! in the test that ports the Node case.

use minarbot_testkit::{http::Method, prelude::*, relay::HttpClient};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// Send `path` like the Node peer suites' `call`/`api` helpers: POST JSON
/// when `body` is given, otherwise GET; `browser` adds the same-origin
/// `Origin` header; `bearer` adds `Authorization`. Returns status and JSON.
pub async fn send(
    app: &Server,
    path: &str,
    body: Option<&Value>,
    bearer: Option<&str>,
    browser: bool,
) -> Result<(u16, Value)> {
    send_to(&app.client()?, app.origin(), path, body, bearer, browser).await
}

/// [`send`] against a bare origin (a relay or a stopped-and-moved server).
pub async fn send_to(
    client: &HttpClient,
    origin: &str,
    path: &str,
    body: Option<&Value>,
    bearer: Option<&str>,
    browser: bool,
) -> Result<(u16, Value)> {
    let method = if body.is_some() {
        Method::POST
    } else {
        Method::GET
    };
    let mut request = client
        .request(method, format!("{origin}{path}"))
        .header("Content-Type", "application/json")
        .header("Connection", "close");
    if browser {
        request = request.header("Origin", origin);
    }
    if let Some(token) = bearer {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    if let Some(body) = body {
        request = request.body(body.to_string());
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    let text = response.text().await?;
    let value = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    };
    Ok((status, value))
}

/// Browser call that must answer `status`; returns the JSON body.
pub async fn call(app: &Server, path: &str, body: Option<Value>, status: u16) -> Result<Value> {
    let (got, value) = send(app, path, body.as_ref(), None, true).await?;
    ensure!(
        got == status,
        "{path}: expected {status}, got {got}: {value}"
    );
    Ok(value)
}

/// Non-browser (peer) call that must answer `status`.
pub async fn peer_call(
    app: &Server,
    path: &str,
    body: Option<Value>,
    bearer: Option<&str>,
    status: u16,
) -> Result<Value> {
    let (got, value) = send(app, path, body.as_ref(), bearer, false).await?;
    ensure!(
        got == status,
        "{path}: expected {status}, got {got}: {value}"
    );
    Ok(value)
}

/// `{id: <uuid>}` command body.
pub fn command() -> Value {
    json!({"id": new_id()})
}

/// `machines` array of `GET /api/machines`.
pub async fn machines(app: &Server) -> Result<Vec<Value>> {
    Ok(call(app, "/api/machines", None, 200).await?["machines"]
        .as_array()
        .cloned()
        .unwrap_or_default())
}

/// First paired machine.
pub async fn first_machine(app: &Server) -> Result<Value> {
    machines(app)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| "no paired machine".into())
}

/// Pair `a` with `b` through `origin` (B's origin unless a relay sits in
/// between): B mints a code, A redeems it. Returns (result, code, request).
pub async fn pair_via(a: &Server, b: &Server, origin: &str) -> Result<(Value, Value, Value)> {
    let code = call(b, "/api/pairing-codes", Some(command()), 200).await?;
    let request = json!({"id": new_id(), "origin": origin, "code": code["code"]});
    let result = call(a, "/api/machines", Some(request.clone()), 200).await?;
    Ok((result, code, request))
}

/// [`pair_via`] with B's own origin.
pub async fn pair(a: &Server, b: &Server) -> Result<(Value, Value, Value)> {
    let origin = b.origin().to_owned();
    pair_via(a, b, &origin).await
}

/// Port of a running server.
pub fn port(app: &Server) -> Result<u16> {
    minarbot_testkit::relay::port(app.origin())
}

/// Restart `app` on its current port with the same config.
pub async fn restart_same_port(app: &mut Server) -> Result<()> {
    let port = port(app)?;
    let config = app.config().clone().option("port", port);
    app.restart_with(config).await
}

/// Every regular file below `directory`.
pub fn files(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut pending = vec![directory.to_owned()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                out.push(entry.path());
            }
        }
    }
    Ok(out)
}

/// Files below `directory` (except `*-key` wrapping keys) that contain
/// any of `secrets`, as `path: secret` lines.
pub fn leaks(directory: &Path, secrets: &[String]) -> Result<Vec<String>> {
    let mut found = Vec::new();
    for path in files(directory)? {
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("-key"))
        {
            continue;
        }
        let bytes = std::fs::read(&path)?;
        for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
            if bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes())
            {
                found.push(format!("{}: {secret}", path.display()));
            }
        }
    }
    Ok(found)
}

/// Sleep (only for fixed observation windows the Node case also waited).
pub async fn pause(millis: u64) {
    tokio::time::sleep(Duration::from_millis(millis)).await;
}
