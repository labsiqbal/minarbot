//! Paired machines, ported from `app/machines.test.ts` (#89).
//!
//! Every Node server had a computer bridge; tests that need
//! `refreshMachines`, a fake peer token issuer or the manual machine clock
//! start one with [`MachineBridge`]. Peer HTTP then flows through the
//! bridge's `peer-http` operation exactly as before. Tests without those
//! needs talk peer to peer directly, which exercises the product's own
//! HTTP client. Servers use the real clock, as the Node suite did.

use crate::pr_support::{
    call, command, first_machine, leaks, machines, pair, pause, peer_call, port, restart_same_port,
};
use minarbot_testkit::{
    db::Inspector,
    machine_bridge::{Clock, IssuedToken, Issuer, MachineBridge},
    prelude::*,
    relay::{self, Answer, Bytes, Relay},
    Servers,
};
use std::{
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn agent(name: &str) -> Value {
    json!({"id": new_id(), "runtime": "api", "name": name, "role": "Personal agent",
        "model": "fixture", "instructions": "Help."})
}

async fn bridged(issuer: Option<&Issuer>) -> Result<ServerConfig> {
    let mut bridge = MachineBridge::new();
    if let Some(issuer) = issuer {
        bridge = bridge.issuer(issuer);
    }
    Ok(ServerConfig::new()
        .real_clock()
        .computer_bridge(&bridge.bridge().await?))
}

fn peer_key(app: &Server) -> std::path::PathBuf {
    let mut path = app.database().into_os_string();
    path.push(".peer-key");
    path.into()
}

struct Link {
    target: String,
    route: String,
    slow: bool,
    probe_timed_out: bool,
    roster_bytes: usize,
}

fn locked<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[tokio::test]
async fn paired_machine_recovers_after_restart_while_roster_sync_times_out_on_a_shared_link(
) -> Result<()> {
    let root_a = TestRoot::new()?;
    let root_b = TestRoot::new()?;
    let servers = Servers::reserve(2).await?;
    let a = root_a
        .start_in(
            &servers,
            bridged(None)
                .await?
                .option("machines", json!({"healthIntervalMs": 1000})),
        )
        .await?;
    let mut b = root_b.start_in(&servers, bridged(None).await?).await?;
    let link = Arc::new(Mutex::new(Link {
        target: b.origin().to_owned(),
        route: String::new(),
        slow: false,
        probe_timed_out: false,
        roster_bytes: 0,
    }));
    let shared = link.clone();
    let proxy = Relay::start(move |exchange| {
        let shared = shared.clone();
        async move {
            let (target, route, slow, timed_out) = {
                let link = locked(&shared);
                (
                    link.target.clone(),
                    link.route.clone(),
                    link.slow,
                    link.probe_timed_out,
                )
            };
            if timed_out && exchange.path == "/api/environment" {
                return Ok(Answer::empty(504, Vec::new()));
            }
            let upstream = exchange.forward(&target).await?;
            let status = upstream.status;
            let mut body = upstream.bytes().await?;
            if exchange.path == "/api/environment" || exchange.path == "/api/pairing/redeem" {
                let mut value: Value = serde_json::from_slice(&body)?;
                if value.get("environment").is_some() {
                    value["environment"]["origin"] = json!(route);
                } else {
                    value["origin"] = json!(route);
                }
                body = value.to_string().into_bytes();
            }
            if slow {
                tokio::time::sleep(Duration::from_millis(120)).await;
            }
            if slow && exchange.path == "/api/peer/agents" && body.len() > 100 && status == 200 {
                body.extend(std::iter::repeat_n(b' ', 120 * 1024));
                locked(&shared).roster_bytes = body.len();
                let length = body.len();
                // Desktop traffic uses 15 KiB/s of the 25 KiB/s uplink.
                let chunks: Vec<Bytes> = body
                    .chunks(1024)
                    .map(|chunk| Bytes::copy_from_slice(chunk))
                    .collect();
                let stream = relay::paced(chunks, Duration::from_millis(100));
                return Ok(Answer::stream(
                    status,
                    vec![
                        ("content-type".into(), "application/json".into()),
                        ("content-length".into(), length.to_string()),
                    ],
                    stream,
                ));
            }
            Ok(Answer::bytes(
                status,
                vec![("content-type".into(), "application/json".into())],
                body,
            ))
        }
    })
    .await?;
    let route = proxy.origin().to_owned();
    locked(&link).route = route.clone();

    call(&b, "/api/agents", Some(agent("Remote agent")), 201).await?;
    let code = call(&b, "/api/pairing-codes", Some(command()), 200).await?;
    let paired = call(
        &a,
        "/api/machines",
        Some(json!({"id": new_id(), "origin": route, "code": code["code"]})),
        200,
    )
    .await?;
    ensure!(paired["agentCount"] == 1, "agent count: {paired}");
    ensure!(paired["status"] == "online", "status: {paired}");
    let config_b = b.config().clone();
    b.shutdown().await?;
    a.refresh_machines().await?;
    ensure!(
        first_machine(&a).await?["status"] == "offline",
        "B stopped but A lists it online"
    );

    locked(&link).slow = true;
    b.start_stopped(config_b).await?;
    locked(&link).target = b.origin().to_owned();
    let restarted = Instant::now();
    let deadline = restarted + Duration::from_secs(8);
    let mut peer = first_machine(&a).await?;
    while (peer["status"] != "online" || locked(&link).roster_bytes == 0)
        && Instant::now() < deadline
    {
        pause(50).await;
        peer = first_machine(&a).await?;
    }
    let roster_bytes = locked(&link).roster_bytes;
    ensure!(
        roster_bytes > 100 * 1024,
        "Roster takes over 10 seconds on the shared link ({roster_bytes} bytes)"
    );
    ensure!(peer["status"] == "online", "peer after restart: {peer}");
    ensure!(
        peer["lastSeen"].as_i64() > paired["lastSeen"].as_i64(),
        "lastSeen did not advance: {peer} vs {paired}"
    );
    ensure!(
        peer["agentCount"] == paired["agentCount"],
        "agent count: {peer}"
    );

    a.refresh_machines().await?;
    peer = first_machine(&a).await?;
    ensure!(
        peer["status"] == "online",
        "Roster deadline leaves authenticated liveness intact: {peer}"
    );
    ensure!(peer["agents"] == paired["agents"], "agents: {peer}");

    locked(&link).probe_timed_out = true;
    a.refresh_machines().await?;
    let stale = first_machine(&a).await?;
    ensure!(stale["status"] == "stale", "probe timeout: {stale}");
    ensure!(
        stale["lastSeen"].as_i64() >= peer["lastSeen"].as_i64(),
        "lastSeen went back: {stale}"
    );
    ensure!(stale["agents"] == paired["agents"], "agents: {stale}");
    Ok(())
}

/// Send a raw HTTP/1.1 request with an explicit `Host` header; returns the status.
async fn with_host(origin: &str, method: &str, path: &str, body: Option<&str>) -> Result<u16> {
    let address = origin.trim_start_matches("http://");
    let mut stream = tokio::net::TcpStream::connect(address).await?;
    let body = body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: workstation.example.test:4317\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await?;
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).await?;
    let text = String::from_utf8_lossy(&reply);
    Ok(text.split(' ').nth(1).ok_or("no status line")?.parse()?)
}

#[tokio::test]
async fn peer_transport_accepts_proxy_host_headers_while_owner_machine_routes_retain_the_local_host_boundary(
) -> Result<()> {
    let root = TestRoot::new()?;
    let servers = Servers::reserve(1).await?;
    let issuer = Issuer::new();
    let app = root
        .start_in(&servers, bridged(Some(&issuer)).await?)
        .await?;
    let origin = app.origin().to_owned();
    ensure!(
        with_host(&origin, "GET", "/api/environment", None).await? == 200,
        "environment through proxy host"
    );
    ensure!(
        with_host(&origin, "GET", "/api/peer/agents", None).await? == 401,
        "peer agents without bearer"
    );
    let probe = app
        .client()?
        .get(app.url("/api/peer/agents"))
        .header("X-Minarbot-Peer-Probe", "1")
        .send()
        .await?
        .status()
        .as_u16();
    ensure!(probe == 401, "probe header: {probe}");
    ensure!(
        with_host(&origin, "POST", "/api/pairing/redeem", Some("{}")).await? == 400,
        "redeem through proxy host"
    );
    ensure!(
        with_host(&origin, "POST", "/api/machines/reciprocal", Some("{}")).await? == 401,
        "reciprocal through proxy host"
    );
    ensure!(
        with_host(&origin, "POST", "/api/peer/unpair", Some("{}")).await? == 401,
        "unpair through proxy host"
    );
    ensure!(
        with_host(&origin, "GET", "/api/machines", None).await? == 403,
        "owner machine list through foreign host"
    );
    ensure!(
        with_host(
            &origin,
            "POST",
            "/api/pairing-codes",
            Some(&command().to_string())
        )
        .await?
            == 403,
        "owner pairing code through foreign host"
    );
    Ok(())
}

#[tokio::test]
async fn two_real_instances_pair_mutually_expose_scoped_cached_agents_survive_restarts_recover_offline_and_revoke_on_unpair_without_leaking_credentials(
) -> Result<()> {
    let root_a = TestRoot::new()?;
    let root_b = TestRoot::new()?;
    let servers = Servers::reserve(2).await?;
    let issuer_a = Issuer::new();
    let issuer_b = Issuer::new();
    let mut a = root_a
        .start_in(&servers, bridged(Some(&issuer_a)).await?)
        .await?;
    let mut b = root_b
        .start_in(&servers, bridged(Some(&issuer_b)).await?)
        .await?;
    let initial_a = call(&a, "/api/environment", None, 200).await?;
    let initial_b = call(&b, "/api/environment", None, 200).await?;
    ensure!(
        initial_a["id"] != initial_b["id"],
        "environment ids collide"
    );
    let mut rename = command();
    rename["name"] = json!("Machine B");
    call(&b, "/api/environment", Some(rename.clone()), 200).await?;
    call(&b, "/api/environment", Some(rename.clone()), 200).await?;
    let mut other = rename.clone();
    other["name"] = json!("Other");
    call(&b, "/api/environment", Some(other), 409).await?;
    call(&a, "/api/agents", Some(agent("Alpha")), 201).await?;
    call(&b, "/api/agents", Some(agent("Beta")), 201).await?;
    let (result, code, request) = pair(&a, &b).await?;
    ensure!(result["id"] == initial_b["id"], "paired id: {result}");
    ensure!(result["pairingState"] == "paired", "state: {result}");
    ensure!(result["status"] == "online", "status: {result}");
    ensure!(result["agentCount"] == 1, "agent count: {result}");
    ensure!(result["agents"][0]["name"] == "Beta", "agent: {result}");
    ensure!(
        result["agents"][0]["role"] == "Personal agent",
        "role: {result}"
    );
    call(&a, "/api/machines", Some(request), 200).await?;
    ensure!(
        issuer_a.issued().len() == 1,
        "A issued {:?}",
        issuer_a.issued()
    );
    ensure!(
        issuer_b.issued().len() == 1,
        "B issued {:?}",
        issuer_b.issued()
    );
    let back = first_machine(&b).await?;
    ensure!(back["id"] == initial_a["id"], "reciprocal id: {back}");
    ensure!(
        back["agents"][0]["name"] == "Alpha",
        "reciprocal agent: {back}"
    );
    let token_b = issuer_b.issued()[0].clone();
    let roster = peer_call(&b, "/api/peer/agents", None, Some(&token_b), 200).await?;
    ensure!(roster["agents"] == result["agents"], "roster: {roster}");
    peer_call(&b, "/api/peer/agents", None, None, 401).await?;
    peer_call(&b, "/api/peer/agents", None, Some("wrong-peer-token"), 401).await?;
    let auth = issuer_b.get(&token_b).ok_or("B token missing")?;
    issuer_b.set(
        &token_b,
        IssuedToken {
            id: auth.id.clone(),
            scopes: vec!["history:read".into()],
        },
    );
    peer_call(&b, "/api/peer/agents", None, Some(&token_b), 401).await?;
    a.refresh_machines().await?;
    ensure!(
        first_machine(&a).await?["status"] == "unauthorized",
        "scope downgrade not detected"
    );
    issuer_b.set(&token_b, auth);
    peer_call(
        &b,
        "/api/pairing/redeem",
        Some(json!({"code": code["code"], "returnPairing": initial_a})),
        None,
        400,
    )
    .await?;
    let b_port = port(&b)?;
    let a_port = port(&a)?;
    let config_b = b.config().clone().option("port", b_port);
    b.shutdown().await?;
    a.refresh_machines().await?;
    let offline = first_machine(&a).await?;
    ensure!(offline["status"] == "offline", "B offline: {offline}");
    ensure!(
        offline["agents"][0]["name"] == "Beta",
        "cached agents: {offline}"
    );
    b.start_stopped(config_b).await?;
    let env_b = call(&b, "/api/environment", None, 200).await?;
    ensure!(
        env_b["id"] == initial_b["id"],
        "B id after restart: {env_b}"
    );
    ensure!(
        env_b["name"] == "Machine B",
        "B name after restart: {env_b}"
    );
    a.refresh_machines().await?;
    ensure!(
        first_machine(&a).await?["status"] == "online",
        "B back online"
    );
    let config_a = a.config().clone().option("port", a_port);
    a.restart_with(config_a).await?;
    ensure!(
        call(&a, "/api/environment", None, 200).await?["id"] == initial_a["id"],
        "A id after restart"
    );
    b.refresh_machines().await?;
    ensure!(
        first_machine(&b).await?["status"] == "online",
        "A back online on B"
    );
    a.refresh_machines().await?;
    let code_request = command();
    let once = call(&a, "/api/pairing-codes", Some(code_request.clone()), 200).await?;
    call(&a, "/api/pairing-codes", Some(code_request), 409).await?;
    let tokens: Vec<String> = issuer_a
        .issued()
        .into_iter()
        .chain(issuer_b.issued())
        .collect();
    for app in [&a, &b] {
        for path in ["/api/environment", "/api/machines", "/api/agents"] {
            let response = call(app, path, None, 200).await?.to_string();
            for token in &tokens {
                ensure!(!response.contains(token.as_str()), "{path} leaked a token");
            }
        }
    }
    a.shutdown().await?;
    b.shutdown().await?;
    let mut secrets = tokens.clone();
    secrets.push(code["code"].as_str().unwrap_or_default().to_owned());
    secrets.push(once["code"].as_str().unwrap_or_default().to_owned());
    for root in [&root_a, &root_b] {
        let found = leaks(root.path(), &secrets)?;
        ensure!(found.is_empty(), "Secret in {found:?}");
    }
    for app in [&a, &b] {
        let mode = std::fs::metadata(peer_key(app))?.permissions().mode() & 0o777;
        ensure!(mode == 0o600, "peer key mode {mode:o}");
    }
    let canary = vec!["peer-canary-".to_owned()];
    for root in [&root_a, &root_b] {
        let found = leaks(&root.path().join("logs"), &canary)?;
        ensure!(found.is_empty(), "token canary in server logs: {found:?}");
    }
    let config_a = a.config().clone();
    let config_b = b.config().clone();
    a.start_stopped(config_a).await?;
    b.start_stopped(config_b).await?;
    let mut removal = command();
    removal["environmentId"] = initial_b["id"].clone();
    call(&a, "/api/machines/remove", Some(removal.clone()), 200).await?;
    call(&a, "/api/machines/remove", Some(removal), 200).await?;
    ensure!(machines(&a).await?.is_empty(), "A still lists B");
    ensure!(machines(&b).await?.is_empty(), "B still lists A");
    ensure!(issuer_a.live() == 0, "A tokens left: {}", issuer_a.live());
    ensure!(issuer_b.live() == 0, "B tokens left: {}", issuer_b.live());
    peer_call(&b, "/api/peer/agents", None, Some(&token_b), 401).await?;
    Ok(())
}

#[tokio::test]
async fn pairing_uses_uniform_invalid_code_errors_strict_bodies_one_time_expiry_persisted_rate_limits_and_browser_only_owner_writes(
) -> Result<()> {
    let root = TestRoot::new()?;
    let servers = Servers::reserve(1).await?;
    let issuer = Issuer::new();
    let clock = Clock::now();
    let bridge = MachineBridge::new()
        .issuer(&issuer)
        .clock(&clock)
        .bridge()
        .await?;
    let mut app = root
        .start_in(
            &servers,
            ServerConfig::new().real_clock().computer_bridge(&bridge),
        )
        .await?;
    let version = call(&app, "/api/environment", None, 200).await?["version"].clone();
    let remote = json!({"id": new_id(), "name": "Remote", "version": version,
        "origin": "http://127.0.0.1:12345"});
    let made = call(&app, "/api/pairing-codes", Some(command()), 200).await?;
    clock.advance(10 * 60_000 + 1);
    let wrong = peer_call(
        &app,
        "/api/pairing/redeem",
        Some(json!({"code": "23456789ABCD", "returnPairing": remote})),
        None,
        400,
    )
    .await?;
    let expired = peer_call(
        &app,
        "/api/pairing/redeem",
        Some(json!({"code": made["code"], "returnPairing": remote})),
        None,
        400,
    )
    .await?;
    ensure!(
        expired == wrong,
        "expired code error differs: {expired} vs {wrong}"
    );
    let mut extra_remote = remote.clone();
    extra_remote["extra"] = json!(true);
    for body in [
        Value::Null,
        json!([]),
        json!({}),
        json!({"code": 42}),
        json!({"code": "23456789ABCD", "returnPairing": extra_remote}),
        json!({"code": "23456789ABCD", "returnPairing": remote, "extra": true}),
    ] {
        let answer = peer_call(&app, "/api/pairing/redeem", Some(body.clone()), None, 400).await?;
        ensure!(answer == wrong, "body {body} answered {answer}");
    }
    for _ in 8..12 {
        peer_call(
            &app,
            "/api/pairing/redeem",
            Some(json!({"code": "23456789ABCD", "returnPairing": remote})),
            None,
            400,
        )
        .await?;
    }
    peer_call(&app, "/api/pairing/redeem", Some(json!({})), None, 429).await?;
    restart_same_port(&mut app).await?;
    peer_call(&app, "/api/pairing/redeem", Some(json!({})), None, 429).await?;
    clock.advance(120_000);
    let valid = call(&app, "/api/pairing-codes", Some(command()), 200).await?;
    let code = valid["code"].as_str().unwrap_or_default().to_owned();
    let response = peer_call(
        &app,
        "/api/pairing/redeem",
        Some(json!({"code": code.to_lowercase(), "returnPairing": remote})),
        None,
        200,
    )
    .await?;
    let token = response["token"].as_str().unwrap_or_default().to_owned();
    ensure!(!token.is_empty(), "redeem gave no token: {response}");
    peer_call(
        &app,
        "/api/pairing/redeem",
        Some(json!({"code": code, "returnPairing": remote})),
        None,
        400,
    )
    .await?;
    let mut other = remote.clone();
    other["id"] = json!(new_id());
    peer_call(
        &app,
        "/api/machines/reciprocal",
        Some(json!({"environment": other, "token": "return-canary-token-123456"})),
        Some(&token),
        401,
    )
    .await?;
    peer_call(&app, "/api/pairing-codes", Some(command()), None, 403).await?;
    call(
        &app,
        "/api/pairing/redeem",
        Some(json!({"code": code, "returnPairing": remote})),
        400,
    )
    .await?;
    peer_call(
        &app,
        "/api/peer/unpair",
        Some(json!({})),
        Some("invalid-token-123456"),
        401,
    )
    .await?;
    let mut bad = command();
    bad["name"] = json!("");
    bad["extra"] = json!("x");
    call(&app, "/api/environment", Some(bad), 400).await?;
    call(&app, "/api/environment?x=1", None, 400).await?;
    clock.advance(10 * 60_000 + 1);
    app.refresh_machines().await?;
    ensure!(
        issuer.live() == 0,
        "unredeemed token survives expiry: {}",
        issuer.live()
    );
    Ok(())
}

#[tokio::test]
async fn origins_reject_unsafe_forms_remote_pairing_rejects_redirects_oversized_bodies_and_credential_reflection(
) -> Result<()> {
    let root = TestRoot::new()?;
    let servers = Servers::reserve(1).await?;
    let app = root
        .start_in(&servers, ServerConfig::new().real_clock())
        .await?;
    let target_calls = Arc::new(Mutex::new(0_usize));
    let counter = target_calls.clone();
    let destination = Relay::start(move |_| {
        let counter = counter.clone();
        async move {
            *locked(&counter) += 1;
            Ok(Answer::bytes(200, Vec::new(), "{}"))
        }
    })
    .await?;
    let steal = format!("{}/steal", destination.origin());
    let mode = Arc::new(Mutex::new(String::new()));
    let current = mode.clone();
    let fake = Relay::start(move |_| {
        let mode = locked(&current).clone();
        let steal = steal.clone();
        async move {
            let json = ("content-type".to_owned(), "application/json".to_owned());
            Ok(match mode.as_str() {
                "redirect" => Answer::empty(307, vec![json, ("location".into(), steal)]),
                "large" => Answer::json(200, &json!({"value": "x".repeat(129 * 1024)})),
                "declared" => Answer::stream(
                    200,
                    vec![json, ("content-length".into(), "9999999".into())],
                    relay::cut_after("{}"),
                ),
                _ => Answer::bytes(200, vec![json], "{}"),
            })
        }
    })
    .await?;
    for next in ["redirect", "large", "declared", "invalid"] {
        *locked(&mode) = next.to_owned();
        let status = if next == "invalid" { 400 } else { 502 };
        call(
            &app,
            "/api/machines",
            Some(json!({"id": new_id(), "origin": fake.origin(), "code": "23456789ABCD"})),
            status,
        )
        .await?;
    }
    ensure!(*locked(&target_calls) == 0, "redirect was followed");
    ensure!(
        machines(&app).await?.is_empty(),
        "failed pairing stored a machine"
    );
    let tokens = call(&app, "/api/mcp-server/peer-tokens", None, 200).await?;
    ensure!(
        tokens["items"].as_array().is_some_and(Vec::is_empty),
        "failed pairing issued a token: {tokens}"
    );
    Ok(())
}

#[tokio::test]
async fn unpair_removes_first_while_remote_is_offline_missing_wrapping_key_fails_closed_and_cached_agents_remain_readable(
) -> Result<()> {
    let root_a = TestRoot::new()?;
    let root_b = TestRoot::new()?;
    let servers = Servers::reserve(2).await?;
    let issuer_a = Issuer::new();
    let issuer_b = Issuer::new();
    let a = root_a
        .start_in(&servers, bridged(Some(&issuer_a)).await?)
        .await?;
    let mut b = root_b
        .start_in(&servers, bridged(Some(&issuer_b)).await?)
        .await?;
    pair(&a, &b).await?;
    let key = peer_key(&a);
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644))?;
    a.refresh_machines().await?;
    ensure!(
        first_machine(&a).await?["status"] == "unauthorized",
        "readable wrapping key still used"
    );
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600))?;
    b.shutdown().await?;
    let target = first_machine(&a).await?;
    let mut removal = command();
    removal["environmentId"] = target["id"].clone();
    call(&a, "/api/machines/remove", Some(removal), 200).await?;
    ensure!(
        machines(&a).await?.is_empty(),
        "offline unpair kept the machine"
    );
    ensure!(issuer_a.live() == 0, "A tokens left: {}", issuer_a.live());
    Ok(())
}

struct Health {
    mode: String,
    origin: String,
    authorized_calls: usize,
    issued_back: String,
}

#[tokio::test]
async fn background_health_records_a_different_peer_version_as_online_and_never_sends_bearer_to_a_changed_identity_or_redirected_descriptor(
) -> Result<()> {
    let root = TestRoot::new()?;
    let servers = Servers::reserve(1).await?;
    let issuer = Issuer::new();
    let app = root
        .start_in(
            &servers,
            bridged(Some(&issuer))
                .await?
                .option("machines", json!({"healthIntervalMs": 50})),
        )
        .await?;
    let version = call(&app, "/api/environment", None, 200).await?["version"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let id = new_id();
    let remote_token = "remote-peer-canary-1234567890123456";
    let agent = json!({"id": new_id(), "name": "Remote agent", "role": "Personal agent",
        "model": "fixture", "status": "idle"});
    let state = Arc::new(Mutex::new(Health {
        mode: "normal".into(),
        origin: String::new(),
        authorized_calls: 0,
        issued_back: String::new(),
    }));
    let shared = state.clone();
    let app_environment = app.url("/api/environment");
    let (fixture_id, fixture_agent, fixture_version) = (id.clone(), agent.clone(), version.clone());
    let server = Relay::start(move |exchange| {
        let shared = shared.clone();
        let (id, agent, version) = (
            fixture_id.clone(),
            fixture_agent.clone(),
            fixture_version.clone(),
        );
        let app_environment = app_environment.clone();
        async move {
            let data = exchange.json();
            let mut state = locked(&shared);
            let descriptor = json!({
                "id": if state.mode == "identity" { new_id() } else { id },
                "name": "Synthetic workstation",
                "origin": state.origin,
                "version": if state.mode == "version" { "99.0.0".to_owned() } else { version },
            });
            if exchange.header("authorization").is_some() {
                state.authorized_calls += 1;
            }
            Ok(match exchange.path.as_str() {
                "/api/pairing/redeem" => Answer::json(
                    200,
                    &json!({"environment": descriptor, "token": remote_token}),
                ),
                "/api/machines/reciprocal" => {
                    state.issued_back = data["token"].as_str().unwrap_or_default().to_owned();
                    Answer::json(200, &json!({"ok": true}))
                }
                "/api/environment" if state.mode == "redirect" => {
                    Answer::empty(302, vec![("location".into(), app_environment)])
                }
                "/api/environment" => Answer::json(200, &descriptor),
                "/api/peer/agents" if state.mode == "unauthorized" => {
                    Answer::json(401, &json!({"error": "no"}))
                }
                "/api/peer/agents" => {
                    let mut agent = agent;
                    if state.mode == "reflection" {
                        agent["name"] = json!(state.issued_back);
                    }
                    let mut body = json!({"agents": [agent]});
                    if state.mode == "bad-policy" {
                        body["messagingAllowed"] = json!("false");
                    }
                    Answer::json(200, &body)
                }
                _ => Answer::json(200, &json!({"ok": true})),
            })
        }
    })
    .await?;
    locked(&state).origin = server.origin().to_owned();
    // Finish any background probe before changing the remote state.
    let set_mode = |next: &'static str| {
        let app = &app;
        let state = state.clone();
        async move {
            app.refresh_machines().await?;
            locked(&state).mode = next.to_owned();
            app.refresh_machines().await?;
            Ok::<(), minarbot_testkit::Error>(())
        }
    };
    let result = call(
        &app,
        "/api/machines",
        Some(json!({"id": new_id(), "origin": server.origin(), "code": "23456789ABCD"})),
        200,
    )
    .await?;
    ensure!(result["status"] == "online", "paired: {result}");
    locked(&state).mode = "version".into();
    let deadline = Instant::now() + Duration::from_secs(20);
    while first_machine(&app).await?["version"] != "99.0.0" && Instant::now() < deadline {
        pause(50).await;
    }
    // A different app version is recorded but stays online: rolling updates
    // must not cut paired machines off.
    let peer = first_machine(&app).await?;
    ensure!(peer["version"] == "99.0.0", "version not recorded: {peer}");
    ensure!(
        peer["status"] == "online",
        "version change went offline: {peer}"
    );
    set_mode("normal").await?;
    let peer = first_machine(&app).await?;
    ensure!(peer["status"] == "online", "normal: {peer}");
    ensure!(
        peer.get("remoteAgentMessaging").is_none(),
        "Older peers omit availability hints: {peer}"
    );
    set_mode("bad-policy").await?;
    ensure!(
        first_machine(&app).await?["status"] == "offline",
        "invalid messaging policy accepted"
    );
    set_mode("unauthorized").await?;
    ensure!(
        first_machine(&app).await?["status"] == "unauthorized",
        "401 roster not reported"
    );
    set_mode("identity").await?;
    let before = locked(&state).authorized_calls;
    app.refresh_machines().await?;
    ensure!(
        first_machine(&app).await?["status"] == "unauthorized",
        "identity change not reported"
    );
    ensure!(
        locked(&state).authorized_calls == before,
        "Identity mismatch received bearer"
    );
    set_mode("redirect").await?;
    ensure!(
        first_machine(&app).await?["status"] == "offline",
        "redirected descriptor accepted"
    );
    ensure!(
        locked(&state).authorized_calls == before,
        "redirected descriptor received bearer"
    );
    set_mode("reflection").await?;
    let listed = machines(&app).await?;
    ensure!(
        listed[0]["status"] == "offline",
        "reflection: {}",
        listed[0]
    );
    ensure!(
        listed[0]["agents"][0]["name"] == agent["name"],
        "reflected roster replaced cache: {}",
        listed[0]
    );
    let issued_back = locked(&state).issued_back.clone();
    ensure!(
        !json!(listed).to_string().contains(&issued_back),
        "machine list reflects the return token"
    );
    ensure!(issuer.issued().len() == 1, "issued: {:?}", issuer.issued());
    Ok(())
}

struct Retry {
    origin: String,
    attempts: usize,
    saved: String,
    mismatch: bool,
}

#[tokio::test]
async fn lost_reciprocal_acknowledgement_retries_encrypted_pending_credentials_after_restart_without_minting_another_token(
) -> Result<()> {
    let root = TestRoot::new()?;
    let servers = Servers::reserve(1).await?;
    let issuer = Issuer::new();
    let mut app = root
        .start_in(&servers, bridged(Some(&issuer)).await?)
        .await?;
    let version = call(&app, "/api/environment", None, 200).await?["version"].clone();
    let peer_id = new_id();
    let token = "retry-peer-canary-12345678901234567890";
    let state = Arc::new(Mutex::new(Retry {
        origin: String::new(),
        attempts: 0,
        saved: String::new(),
        mismatch: false,
    }));
    let shared = state.clone();
    let fixture_id = peer_id.clone();
    let server = Relay::start(move |exchange| {
        let shared = shared.clone();
        let peer_id = fixture_id.clone();
        let version = version.clone();
        async move {
            let data = exchange.json();
            let mut state = locked(&shared);
            let environment = json!({"id": peer_id, "name": "Retry workstation",
                "version": version, "origin": state.origin});
            Ok(match exchange.path.as_str() {
                "/api/pairing/redeem" => {
                    Answer::json(200, &json!({"environment": environment, "token": token}))
                }
                "/api/machines/reciprocal" => {
                    state.attempts += 1;
                    let sent = data["token"].as_str().unwrap_or_default().to_owned();
                    if state.saved.is_empty() {
                        state.saved = sent;
                        Answer::Drop
                    } else {
                        if sent != state.saved {
                            state.mismatch = true;
                        }
                        Answer::json(200, &json!({"ok": true}))
                    }
                }
                "/api/environment" => Answer::json(200, &environment),
                "/api/peer/agents" => Answer::json(200, &json!({"agents": []})),
                _ => Answer::json(200, &json!({"ok": true})),
            })
        }
    })
    .await?;
    locked(&state).origin = server.origin().to_owned();
    let request = json!({"id": new_id(), "origin": server.origin(), "code": "23456789ABCD"});
    call(&app, "/api/machines", Some(request.clone()), 409).await?;
    ensure!(
        first_machine(&app).await?["pairingState"] == "pending",
        "lost acknowledgement not pending"
    );
    restart_same_port(&mut app).await?;
    let retried = call(&app, "/api/machines", Some(request.clone()), 200).await?;
    ensure!(retried["pairingState"] == "paired", "retry: {retried}");
    ensure!(
        locked(&state).attempts == 2,
        "attempts: {}",
        locked(&state).attempts
    );
    ensure!(!locked(&state).mismatch, "retry sent another return token");
    ensure!(
        issuer.issued().len() == 1,
        "minted again: {:?}",
        issuer.issued()
    );
    let mut mismatch = request.clone();
    mismatch["code"] = json!("23456789ABCE");
    call(&app, "/api/machines", Some(mismatch), 409).await?;
    app.shutdown().await?;
    let saved = locked(&state).saved.clone();
    let row = Inspector::open(app.database())?
        .row("SELECT token,return_token FROM paired_machines", [])?;
    ensure!(
        !row["token"].to_string().contains(token),
        "remote token stored in plain text"
    );
    ensure!(
        !row["return_token"].to_string().contains(&saved),
        "return token stored in plain text"
    );
    Ok(())
}

#[tokio::test]
async fn default_wiring_issues_pairing_tokens_from_the_minarbot_mcp_token_registry_and_revokes_them_on_unpair(
) -> Result<()> {
    let root_a = TestRoot::new()?;
    let root_b = TestRoot::new()?;
    let servers = Servers::reserve(2).await?;
    let a = root_a
        .start_in(&servers, ServerConfig::new().real_clock())
        .await?;
    let b = root_b
        .start_in(&servers, ServerConfig::new().real_clock())
        .await?;
    let mut name = command();
    name["name"] = json!("Machine B");
    call(&b, "/api/environment", Some(name), 200).await?;
    let mut name = command();
    name["name"] = json!("Machine A");
    call(&a, "/api/environment", Some(name), 200).await?;
    call(&b, "/api/agents", Some(agent("Beta")), 201).await?;
    let (result, ..) = pair(&a, &b).await?;
    ensure!(result["pairingState"] == "paired", "state: {result}");
    ensure!(result["status"] == "online", "status: {result}");
    ensure!(result["agents"][0]["name"] == "Beta", "agent: {result}");
    ensure!(
        result["agents"][0]["role"] == "Personal agent",
        "role: {result}"
    );
    let on_b = call(&b, "/api/mcp-server/peer-tokens", None, 200).await?["items"].clone();
    ensure!(on_b.as_array().map(Vec::len) == Some(1), "B tokens: {on_b}");
    ensure!(
        on_b[0]["name"] == "Minarbot: Machine A",
        "token name: {on_b}"
    );
    ensure!(on_b[0]["revoked"].is_null(), "fresh token revoked: {on_b}");
    let mut scopes: Vec<String> = on_b[0]["scopes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|scope| scope.as_str().map(str::to_owned))
        .collect();
    scopes.sort();
    ensure!(
        scopes == ["agents:message", "agents:read", "history:read"],
        "scopes: {scopes:?}"
    );
    let on_a = call(&a, "/api/mcp-server/peer-tokens", None, 200).await?;
    ensure!(
        on_a["items"][0]["name"] == "Minarbot: Machine B",
        "A token name: {on_a}"
    );
    let mut removal = command();
    removal["environmentId"] = result["id"].clone();
    call(&a, "/api/machines/remove", Some(removal), 200).await?;
    ensure!(machines(&a).await?.is_empty(), "A still lists B");
    ensure!(machines(&b).await?.is_empty(), "B still lists A");
    let after = call(&b, "/api/mcp-server/peer-tokens", None, 200).await?;
    ensure!(
        !after["items"][0]["revoked"].is_null(),
        "B revoked the token it issued to A: {after}"
    );
    Ok(())
}
