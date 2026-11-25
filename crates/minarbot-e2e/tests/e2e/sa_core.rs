//! Static serving, local boundary errors and test-server option checks (port
//! of `app/rust-core.test.ts`).

use crate::sa_support::{web_dist, DIST_SCRIPT, DIST_SCRIPT_BYTES};
use minarbot_testkit::{http::Method, prelude::*};

#[tokio::test]
async fn core_serves_exact_static_bytes_strict_asset_csp_html_404_and_local_boundary_errors(
) -> Result<()> {
    let root = TestRoot::new()?;
    let dist = web_dist(&root)?;
    let app = root
        .start(ServerConfig::new().option("dist", json!(dist)))
        .await?;
    let page = app.client()?.get(app.url("/")).send().await?;
    ensure!(page.status() == 200, "page status {}", page.status());
    ensure!(
        page.headers()
            .get("referrer-policy")
            .and_then(|v| v.to_str().ok())
            == Some("no-referrer"),
        "referrer policy: {:?}",
        page.headers()
    );
    let html = page.text().await?;
    let asset = html
        .split("src=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .filter(|src| src.starts_with("/_leptos/assets/"))
        .ok_or_else(|| format!("no script asset in {html}"))?
        .to_owned();
    ensure!(asset == DIST_SCRIPT, "asset {asset}");
    let response = app.client()?.get(app.url(&asset)).send().await?;
    let cache = response
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let policy = response
        .headers()
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .ok_or("asset has no CSP")?;
    ensure!(
        response.bytes().await?.as_ref() == DIST_SCRIPT_BYTES,
        "asset bytes differ"
    );
    ensure!(
        cache.as_deref() == Some("public, max-age=31536000, immutable"),
        "cache-control: {cache:?}"
    );
    ensure!(
        !policy.contains("nonce-"),
        "asset CSP has a nonce: {policy}"
    );
    for path in ["/missing/path", "/rooms/deleted", "/settings/nope"] {
        let missing = app.client()?.get(app.url(path)).send().await?;
        ensure!(missing.status() == 404, "{path}: {}", missing.status());
        let kind = missing
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        ensure!(kind.contains("text/html"), "{path} content type {kind}");
        missing.text().await?;
    }
    let denied = app
        .client()?
        .get(app.url("/api/agents"))
        .header("sec-fetch-site", "cross-site")
        .send()
        .await?;
    ensure!(denied.status() == 403, "cross-site: {}", denied.status());
    ensure!(
        denied.json::<Value>().await?
            == json!({"error": "Access is only allowed through the local Minarbot address."}),
        "cross-site body"
    );
    let cookies = app
        .client()?
        .request(Method::GET, app.url("/api/agents"))
        .header("cookie", "__session=untrusted")
        .send()
        .await?;
    ensure!(
        cookies.headers().get("set-cookie").is_none(),
        "set-cookie sent"
    );
    ensure!(
        cookies.status() == 200,
        "cookie request: {}",
        cookies.status()
    );
    cookies.text().await?;
    Ok(())
}

#[tokio::test]
async fn rust_test_bridge_rejects_unported_options_before_starting_a_process() -> Result<()> {
    let root = TestRoot::new()?;
    let error = root
        .start(ServerConfig::new().option("unported", true))
        .await
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    ensure!(
        error.contains("not ported: option unported"),
        "unported: {error}"
    );
    let error = root
        .start(ServerConfig::new().option(
            "hubLink",
            json!({"origin": "https://hub.example.test", "hubSigningPublicKey": "fixture", "unported": true}),
        ))
        .await
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    ensure!(
        error.contains("not ported: option hubLink.unported"),
        "nested unported: {error}"
    );
    // NaN and Infinity have no JSON form: "NaN" and 1e300 stand in for them.
    for concurrency in [json!("NaN"), json!(1e300), json!(0), json!(33)] {
        let error = root
            .start_error(ServerConfig::new().concurrency(concurrency.clone()))
            .await?;
        ensure!(
            error.contains("MINARBOT_CONCURRENCY must be 1..32"),
            "concurrency {concurrency}: {error}"
        );
    }
    ensure!(
        !root.database().exists(),
        "a rejected start created the database"
    );
    Ok(())
}
