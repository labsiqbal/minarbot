//! Update fan-out and trial admission, ported from `app/updates.test.ts`
//! (#89). The release builder cases live in `crates/xtask/src/release_port.rs`
//! and the peer SHA-256 case in
//! `crates/minarbot-server/src/updates/tests/peer_sha_mismatch.rs`.

use crate::pr_support::{call, command, first_machine, leaks, pair, send_to};
use minarbot_testkit::{
    bridge::BridgeError,
    db::Inspector,
    machine_bridge::{decrypt_peer_token, peer_http_json, peer_http_request, Extra, MachineBridge},
    prelude::*,
    relay::HttpClient,
    release_fixture::{self, MockLauncher},
    wait::until,
    Servers,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

const COMMIT: &str = "abcdef1234567890abcdef1234567890abcdef12";

fn locked<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The Node `mockLauncher`: status, and apply commands that keep one digest per request ID.
async fn mock_launcher(
    directory: &std::path::Path,
) -> Result<(MockLauncher, Arc<Mutex<HashMap<String, String>>>)> {
    let commands = Arc::new(Mutex::new(HashMap::<String, String>::new()));
    let seen = commands.clone();
    let launcher = MockLauncher::start(&directory.join("control.sock"), move |request| {
        if request["type"] == "status" {
            return json!({"protocol": 1, "current": "development"});
        }
        let digest = json!([request["target"], request["version"]]).to_string();
        let id = request["id"].as_str().unwrap_or_default().to_owned();
        let mut commands = locked(&seen);
        match commands.get(&id) {
            Some(old) if *old != digest => json!({"error": "Request ID was already used."}),
            _ => {
                commands.insert(id.clone(), digest);
                json!({"accepted": true, "id": id})
            }
        }
    })
    .await?;
    Ok((launcher, commands))
}

/// Bridge hook recording peer tokens from pairing traffic, as the Node test
/// recorded them from `fetch`.
fn token_capture(tokens: Arc<Mutex<Vec<String>>>) -> Extra {
    Arc::new(move |operation: &str, args: &Value| {
        if operation != "peer-http" {
            return None;
        }
        let url = args["url"].as_str().unwrap_or_default().to_owned();
        if url.ends_with("/api/machines/reciprocal") {
            let body: Value = args["body"]
                .as_str()
                .and_then(|body| serde_json::from_str(body).ok())
                .unwrap_or(Value::Null);
            if let Some(token) = body["token"].as_str() {
                locked(&tokens).push(token.to_owned());
            }
        }
        if !url.ends_with("/api/pairing/redeem") {
            return None;
        }
        let (args, tokens) = (args.clone(), tokens.clone());
        Some(Box::pin(async move {
            let reply = peer_http_request(&args)
                .await
                .map_err(|error| BridgeError::new(503, error.to_string()))?;
            if let Some(token) = peer_http_json(&reply)["token"].as_str() {
                locked(&tokens).push(token.to_owned());
            }
            Ok(reply)
        }))
    })
}

#[tokio::test]
async fn peer_fan_out_needs_both_owners_grants_no_default_update_scope_streams_verified_release_and_keeps_tokens_secret(
) -> Result<()> {
    let servers = Servers::reserve(2).await?;
    let dir = TestRoot::new()?;
    let (ctl_a, _commands_a) = mock_launcher(&dir.dir("ctl-a")?).await?;
    let (ctl_b, commands_b) = mock_launcher(&dir.dir("ctl-b")?).await?;
    let releases = dir.dir("releases")?;
    let b_releases = dir.path().join("b-releases");
    let release = release_fixture::publish(&releases, "1.0.1", COMMIT, "Fixture 1.0.1")?;
    let tokens = Arc::new(Mutex::new(Vec::<String>::new()));
    let bridge_a = MachineBridge::new()
        .extra(token_capture(tokens.clone()))
        .bridge()
        .await?;
    let bridge_b = MachineBridge::new()
        .extra(token_capture(tokens.clone()))
        .bridge()
        .await?;
    let root_a = TestRoot::new()?;
    let root_b = TestRoot::new()?;
    let mut a = root_a
        .start_in(
            &servers,
            ServerConfig::new()
                .real_clock()
                .computer_bridge(&bridge_a)
                .option(
                    "updates",
                    json!({"releasesDir": releases, "launcherSocket": ctl_a.path()}),
                ),
        )
        .await?;
    let mut b = root_b
        .start_in(
            &servers,
            ServerConfig::new()
                .real_clock()
                .computer_bridge(&bridge_b)
                .option(
                    "updates",
                    json!({"releasesDir": b_releases, "launcherSocket": ctl_b.path()}),
                ),
        )
        .await?;
    let (machine, ..) = pair(&a, &b).await?;
    let back = first_machine(&b).await?;
    ensure!(
        machine["allowUpdates"] == false,
        "updates on by default: {machine}"
    );
    let issued = call(&a, "/api/mcp-server/peer-tokens", None, 200).await?;
    ensure!(
        issued["items"]
            .as_array()
            .is_some_and(|items| items.iter().all(|token| {
                !token["scopes"]
                    .as_array()
                    .is_some_and(|scopes| scopes.contains(&json!("machine:update")))
            })),
        "default peer token grants machine:update: {issued}"
    );
    let peers = call(&a, "/api/updates/machines", None, 200).await?;
    ensure!(
        peers["machines"]
            == json!([{"machine": machine["id"], "available": false,
                "reason": "Enable updates on both machines."}]),
        "update machines: {peers}"
    );
    let fan_out = |id: String, version: &str| json!({"id": id, "machines": [machine["id"]], "version": version});
    let version = release["version"].as_str().unwrap_or_default().to_owned();
    let result = call(
        &a,
        "/api/updates/fan-out",
        Some(fan_out(new_id(), &version)),
        202,
    )
    .await?;
    ensure!(
        result["machines"][0]["status"] == "failed",
        "fan-out without consent: {result}"
    );
    ensure!(locked(&commands_b).is_empty(), "B launcher got a command");
    call(
        &a,
        "/api/machines/updates",
        Some(json!({"id": new_id(), "environmentId": machine["id"], "allowed": true})),
        200,
    )
    .await?;
    let result = call(
        &a,
        "/api/updates/fan-out",
        Some(fan_out(new_id(), &version)),
        202,
    )
    .await?;
    ensure!(
        result["machines"][0]["status"] == "failed",
        "fan-out with only sender consent: {result}"
    );
    ensure!(locked(&commands_b).is_empty(), "B launcher got a command");
    call(
        &b,
        "/api/machines/updates",
        Some(json!({"id": new_id(), "environmentId": back["id"], "allowed": true})),
        200,
    )
    .await?;
    let accepted_id = new_id();
    let accepted = fan_out(accepted_id.clone(), &version);
    let result = call(&a, "/api/updates/fan-out", Some(accepted.clone()), 202).await?;
    ensure!(
        result["machines"][0]["status"] == "accepted",
        "fan-out with both consents: {result}"
    );
    ensure!(locked(&commands_b).len() == 1, "B launcher commands");
    let id = release["id"].as_str().unwrap_or_default();
    ensure!(
        release_fixture::verify(&b_releases, id)?["version"] == release["version"],
        "received release does not verify"
    );
    let replay = call(&a, "/api/updates/fan-out", Some(accepted.clone()), 202).await?;
    ensure!(
        replay == result,
        "fan-out replay differs: {replay} vs {result}"
    );
    ensure!(
        locked(&commands_b).len() == 1,
        "replay reached the launcher"
    );
    call(
        &a,
        "/api/updates/fan-out",
        Some(fan_out(accepted_id, "1.0.2")),
        409,
    )
    .await?;
    a.refresh_machines().await?;
    let peers = call(&a, "/api/updates/machines", None, 200).await?;
    ensure!(
        peers["machines"][0]["machine"] == machine["id"],
        "update machines: {peers}"
    );
    ensure!(
        peers["machines"][0]["available"] == true,
        "update machines: {peers}"
    );
    call(
        &b,
        "/api/machines/updates",
        Some(json!({"id": new_id(), "environmentId": back["id"], "allowed": false})),
        200,
    )
    .await?;
    let revoked = call(
        &a,
        "/api/updates/fan-out",
        Some(fan_out(new_id(), &version)),
        202,
    )
    .await?;
    ensure!(
        revoked["machines"][0]["status"] == "failed",
        "fan-out after receiver revoked: {revoked}"
    );
    ensure!(
        locked(&commands_b).len() == 1,
        "revoked receiver got a command"
    );
    for app in [&a, &b] {
        let row =
            Inspector::open(app.database())?.row("SELECT token,origin FROM paired_machines", [])?;
        let token = decrypt_peer_token(
            &app.database(),
            row["token"].as_str().unwrap_or_default(),
            row["origin"].as_str().unwrap_or_default(),
        )?;
        let mut tokens = locked(&tokens);
        if !tokens.contains(&token) {
            tokens.push(token);
        }
    }
    let tokens = locked(&tokens).clone();
    ensure!(tokens.len() == 2, "peer tokens: {}", tokens.len());
    for token in &tokens {
        ensure!(
            !result.to_string().contains(token.as_str()),
            "fan-out result leaked a peer token"
        );
        for root in [&root_a, &root_b] {
            let found = leaks(&root.path().join("logs"), std::slice::from_ref(token))?;
            ensure!(found.is_empty(), "token in server logs: {found:?}");
        }
    }
    a.shutdown().await?;
    b.shutdown().await?;
    for root in [&root_a, &root_b] {
        let databases: Vec<_> = crate::pr_support::files(&root.path().join("data"))?
            .into_iter()
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains(".sqlite") && !name.ends_with("-key"))
            })
            .collect();
        for database in databases {
            let bytes = std::fs::read(&database)?;
            for token in &tokens {
                ensure!(
                    !bytes
                        .windows(token.len())
                        .any(|window| window == token.as_bytes()),
                    "token in {}",
                    database.display()
                );
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn rust_trial_gates_owner_work_until_matching_private_socket_commit() -> Result<()> {
    let dir = TestRoot::new()?;
    let id = new_id();
    let committed = Arc::new(Mutex::new(false));
    let state = committed.clone();
    let trial = id.clone();
    let control = MockLauncher::start(&dir.path().join("control.sock"), move |request| {
        if request["type"] == "trial" {
            json!({"committed": *locked(&state) && request["id"] == trial.as_str()})
        } else {
            json!({"protocol": 1, "current": "development"})
        }
    })
    .await?;
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.local_addr()?.port()
    };
    let origin = format!("http://127.0.0.1:{port}");
    let mut child = tokio::process::Command::new(minarbot_testkit::server::server_binary(None)?)
        .env_clear()
        .env("PATH", minarbot_testkit::server::HERMETIC_PATH)
        .env("HOME", dir.path())
        .env("PORT", port.to_string())
        .env("MINARBOT_DATA_DIR", dir.path().join("data"))
        .env("MINARBOT_UPDATE_TRIAL", &id)
        .env("MINARBOT_LAUNCHER_SOCKET", control.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let client = HttpClient::builder().no_proxy().build()?;
    let status = |path: &'static str| {
        let (client, origin) = (client.clone(), origin.clone());
        async move {
            Ok::<u16, minarbot_testkit::Error>(
                send_to(&client, &origin, path, None, None, false).await?.0,
            )
        }
    };
    until("trial server", || async {
        Ok(status("/api/environment")
            .await
            .ok()
            .filter(|code| *code == 200))
    })
    .await?;
    ensure!(status("/api/updates").await? == 200, "updates during trial");
    ensure!(status("/api/agents").await? == 503, "agents during trial");
    ensure!(
        status("/api/machines").await? == 503,
        "machines during trial"
    );
    let (code, _) = send_to(
        &client,
        &origin,
        "/api/pairing-codes",
        Some(&command()),
        None,
        true,
    )
    .await?;
    ensure!(code == 503, "pairing during trial: {code}");
    *locked(&committed) = true;
    until("committed trial", || async {
        Ok(status("/api/agents").await.ok().filter(|code| *code == 200))
    })
    .await?;
    ensure!(
        status("/api/machines").await? == 200,
        "machines after commit"
    );
    child.start_kill()?;
    let _ = child.wait().await;
    Ok(())
}
