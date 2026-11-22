#[path = "support/server.rs"]
mod server;

use reqwest::{header, StatusCode};
use rustix::process::Signal;
use serde_json::{json, Value};
use server::{Result, TestRoot};

#[tokio::test]
async fn pairing_gates_remote_routes_without_locking_out_loopback() -> Result<()> {
    const HOST: &str = "minarbot.example";
    const ORIGIN: &str = "https://minarbot.example";
    let root = TestRoot::new()?;
    let server = root.start(json!({"allowedOrigins": [ORIGIN]})).await?;
    let local = &server.origin;
    let client = &server.client;

    let manage = format!("{local}/api/device-pairing");
    let agents = format!("{local}/api/agents");
    let status = client
        .get(&manage)
        .header(header::HOST, HOST)
        .send()
        .await?;
    assert_eq!(status.json::<Value>().await?["required"], false);
    assert_eq!(
        client
            .get(&agents)
            .header(header::HOST, HOST)
            .send()
            .await?
            .status(),
        StatusCode::OK
    );

    let locked = client
        .post(&manage)
        .header(header::HOST, HOST)
        .header(header::ORIGIN, ORIGIN)
        .json(&json!({"required":true}))
        .send()
        .await?;
    assert_eq!(locked.status(), StatusCode::FORBIDDEN);
    assert_eq!(locked.json::<Value>().await?["code"], "pair_browser_first");

    let issue = server
        .post("/api/device-pairing/code")
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    assert_eq!(issue["url"], format!("{ORIGIN}/pair"));
    assert!(issue["qr"]
        .as_str()
        .ok_or("pairing response has no QR code")?
        .starts_with("data:image/svg+xml;base64,"));

    client
        .post(&manage)
        .header(header::ORIGIN, local)
        .json(&json!({"required":true}))
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(client.get(&agents).send().await?.status(), StatusCode::OK);
    assert_eq!(
        server
            .get("/api/agents")
            .header("x-forwarded-for", "100.64.0.1")
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );

    for path in ["/api/agents", "/api/environment", "/api/machines"] {
        let response = client
            .get(format!("{local}{path}"))
            .header(header::HOST, HOST)
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(
            response.json::<Value>().await?["code"],
            "device_pairing_required"
        );
    }
    let page = client.get(local).header(header::HOST, HOST).send().await?;
    assert_eq!(page.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(page.headers()[header::REFERRER_POLICY], "same-origin");
    assert!(page.text().await?.contains("Pair this device"));

    let paired = client
        .post(format!("{local}/pair"))
        .header(header::HOST, HOST)
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(format!(
            "code={}&name=Test+phone",
            issue["code"]
                .as_str()
                .ok_or("pairing response has no code")?
        ))
        .send()
        .await?;
    assert_eq!(paired.status(), StatusCode::SEE_OTHER);
    let cookie = paired.headers()[header::SET_COOKIE].to_str()?;
    assert!(cookie.ends_with("; HttpOnly; Secure; SameSite=Strict"));
    let cookie = cookie.split(';').next().ok_or("empty pairing cookie")?;
    assert_eq!(
        client
            .get(&agents)
            .header(header::HOST, HOST)
            .header(header::COOKIE, cookie)
            .send()
            .await?
            .status(),
        StatusCode::OK
    );
    let status = client
        .get(&manage)
        .header(header::HOST, HOST)
        .header(header::COOKIE, cookie)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert_eq!(status["devices"][0]["name"], "Test phone");
    assert_eq!(status["devices"][0]["id"], status["current"]);
    client
        .post(format!("{manage}/revoke"))
        .header(header::ORIGIN, local)
        .json(&json!({"id":status["current"]}))
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(
        client
            .get(&agents)
            .header(header::HOST, HOST)
            .header(header::COOKIE, cookie)
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert!(server.terminate(Signal::TERM).await?.success());
    Ok(())
}
