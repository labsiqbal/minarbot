//! Fan-out to a peer without apply polling, ported from
//! `app/fanout-older-peer.test.ts` (#89).
//!
//! The peer runs the current server with its update capabilities hidden by
//! a relay on its paired origin, so the sender takes the synchronous apply
//! path that releases before `peerApplyPolling` still use.

use crate::pr_support::{call, call_within, command, first_machine};
use minarbot_testkit::{
    machine_bridge::MachineBridge,
    prelude::*,
    relay::{self, Answer, Relay},
    release_fixture::{self, MockLauncher},
    Servers,
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime},
};

fn locked<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Default)]
struct Switches {
    hold: AtomicBool,
    mutate: AtomicBool,
    untrusted: AtomicBool,
    busy: AtomicBool,
    applied: AtomicUsize,
}

#[tokio::test]
async fn fan_out_serves_one_verified_executable_archive_to_a_peer_without_apply_polling_and_waits_beyond_its_former_apply_deadline(
) -> Result<()> {
    let servers = Servers::reserve(2).await?;
    let dir = TestRoot::new()?;
    let releases = dir.dir("releases")?;
    let received = dir.path().join("received");
    let id = "0.11.0-7d5216a6";
    let release = releases.join(id);
    fs::create_dir_all(release.join("bin"))?;
    fs::write(release.join("package.json"), r#"{"version":"0.11.0"}"#)?;
    for name in ["minarbot-server", "minarbot-launcher"] {
        let path = release.join("bin").join(name);
        fs::write(&path, "#!/bin/sh\nexit 0\n")?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    }
    let info = json!({"id": id, "version": "0.11.0",
        "commit": format!("7d5216a6{}", "0".repeat(32)), "created": "2026-11-18T00:00:00Z",
        "node": "rust", "notes": "Fixture", "files": release_fixture::manifest(&release)?});
    release_fixture::write_metadata(&releases, &info, true)?;

    let switches = Arc::new(Switches {
        mutate: AtomicBool::new(true),
        ..Switches::default()
    });
    let control = switches.clone();
    let launcher = MockLauncher::start(&dir.path().join("control.sock"), move |request| {
        if request["type"] == "status" {
            json!({"protocol": 1, "current": "development"})
        } else if control.busy.load(Ordering::SeqCst) {
            json!({"error": "Another update is already running."})
        } else {
            control.applied.fetch_add(1, Ordering::SeqCst);
            json!({"accepted": true, "id": request["id"]})
        }
    })
    .await?;

    // Pair with the peer's own origin, then move the peer and put the relay on that origin.
    let peer_root = TestRoot::new()?;
    let sender_root = TestRoot::new()?;
    let peer_config = ServerConfig::new()
        .real_clock()
        .computer_bridge(&MachineBridge::new().bridge().await?)
        .option(
            "updates",
            json!({"releasesDir": received, "launcherSocket": launcher.path()}),
        );
    let mut peer = peer_root.start_in(&servers, peer_config.clone()).await?;
    let origin = peer.origin().to_owned();
    let sender = sender_root
        .start_in(
            &servers,
            ServerConfig::new()
                .real_clock()
                .computer_bridge(&MachineBridge::new().bridge().await?)
                .option("updates", json!({"releasesDir": releases})),
        )
        .await?;
    let code = call(&peer, "/api/pairing-codes", Some(command()), 200).await?;
    let machine = call(
        &sender,
        "/api/machines",
        Some(json!({"id": new_id(), "origin": origin, "code": code["code"]})),
        200,
    )
    .await?;
    let back = first_machine(&peer).await?;
    call(
        &sender,
        "/api/machines/updates",
        Some(json!({"id": new_id(), "environmentId": machine["id"], "allowed": true})),
        200,
    )
    .await?;
    call(
        &peer,
        "/api/machines/updates",
        Some(json!({"id": new_id(), "environmentId": back["id"], "allowed": true})),
        200,
    )
    .await?;
    peer.shutdown().await?;
    peer.start_stopped(peer_config).await?;
    let peer_origin = peer.origin().to_owned();
    let responses = Arc::new(Mutex::new(Vec::<(u16, String)>::new()));
    let (state, seen, binary) = (
        switches.clone(),
        responses.clone(),
        release.join("bin/minarbot-server"),
    );
    let public_origin = origin.clone();
    let _proxy = Relay::bind(relay::port(&origin)?, move |exchange| {
        let (state, seen, binary) = (state.clone(), seen.clone(), binary.clone());
        let (peer_origin, public_origin) = (peer_origin.clone(), public_origin.clone());
        async move {
            let apply = exchange.path == "/api/peer/updates/apply";
            if apply && state.mutate.swap(false, Ordering::SeqCst) {
                fs::File::options()
                    .write(true)
                    .open(&binary)?
                    .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1))?;
            }
            let upstream = exchange.forward(&peer_origin).await?;
            let status = upstream.status;
            if exchange.path == "/api/environment" || exchange.path == "/api/peer/updates" {
                let mut value: Value =
                    serde_json::from_slice(&upstream.bytes().await?).unwrap_or_else(|_| json!({}));
                if exchange.path == "/api/environment" {
                    value["origin"] = json!(public_origin);
                } else if let Some(fields) = value.as_object_mut() {
                    fields.remove("peerApplyPolling");
                    fields.remove("peerGitHubUpdates");
                }
                return Ok(Answer::json(status, &value));
            }
            if !apply {
                return Ok(upstream.relay());
            }
            let body = String::from_utf8_lossy(&upstream.bytes().await?).into_owned();
            locked(&seen).push((status, body.clone()));
            if state.hold.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_secs(125)).await;
            }
            let body = if state.untrusted.load(Ordering::SeqCst) {
                r#"{"error":"fixture-private-value"}"#.to_owned()
            } else {
                body
            };
            Ok(Answer::bytes(
                status,
                vec![("content-type".into(), "application/json".into())],
                body,
            ))
        }
    })
    .await?;
    let fan_out = || json!({"id": new_id(), "machines": [machine["id"]], "version": "0.11.0"});
    let first = fan_out();
    let result = call(&sender, "/api/updates/fan-out", Some(first.clone()), 202).await?;
    ensure!(
        result["machines"][0]["status"] == "accepted",
        "fan-out: {result} {:?}",
        locked(&responses)
    );
    ensure!(locked(&responses)[0].0 == 202, "apply status");
    ensure!(
        release_fixture::verify(&received, id)?["version"] == "0.11.0",
        "received release does not verify"
    );
    let installed = received.join(id).join("bin/minarbot-server");
    ensure!(
        fs::metadata(&installed)?.permissions().mode() & 0o111 != 0,
        "received server is not executable"
    );
    ensure!(
        fs::read(&installed)? == fs::read(release.join("bin/minarbot-server"))?,
        "received server bytes differ"
    );
    let replay = call(&sender, "/api/updates/fan-out", Some(first), 202).await?;
    ensure!(replay == result, "replay differs: {replay} vs {result}");
    ensure!(switches.applied.load(Ordering::SeqCst) == 1, "applied once");

    switches.busy.store(true, Ordering::SeqCst);
    let rejected = call(&sender, "/api/updates/fan-out", Some(fan_out()), 202).await?;
    let (status, body) = locked(&responses)
        .last()
        .cloned()
        .ok_or("no apply response")?;
    ensure!(status == 409, "busy apply status {status}");
    let body: Value = serde_json::from_str(&body)?;
    ensure!(
        body["error"] == "Another update is already running.",
        "busy body: {body}"
    );
    ensure!(
        rejected["machines"][0]["reason"]
            == "Remote machine rejected the update (HTTP 409). Another update is already running.",
        "busy reason: {rejected}"
    );
    switches.untrusted.store(true, Ordering::SeqCst);
    let private = call(&sender, "/api/updates/fan-out", Some(fan_out()), 202).await?;
    ensure!(
        private["machines"][0]["reason"] == "Remote machine rejected the update (HTTP 409).",
        "untrusted reason: {private}"
    );
    switches.untrusted.store(false, Ordering::SeqCst);
    switches.busy.store(false, Ordering::SeqCst);
    switches.hold.store(true, Ordering::SeqCst);
    let started = Instant::now();
    let slow = call_within(
        &sender,
        "/api/updates/fan-out",
        Some(fan_out()),
        202,
        Duration::from_secs(170),
    )
    .await?;
    ensure!(
        started.elapsed() >= Duration::from_secs(120),
        "slow apply answered after {:?}",
        started.elapsed()
    );
    ensure!(
        slow["machines"][0]["status"] == "accepted",
        "slow apply: {slow}"
    );
    ensure!(
        locked(&responses).last().map(|entry| entry.0) == Some(202),
        "slow apply status"
    );
    ensure!(
        switches.applied.load(Ordering::SeqCst) == 2,
        "applied twice"
    );
    for entry in fs::read_dir(&releases)? {
        ensure!(
            !entry?
                .file_name()
                .to_string_lossy()
                .starts_with(".archive-"),
            "archive staging left behind"
        );
    }
    Ok(())
}
