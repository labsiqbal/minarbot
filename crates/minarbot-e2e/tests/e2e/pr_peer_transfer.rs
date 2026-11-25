//! Peer release transfer, ported from `app/peer-release-transfer.test.ts`
//! (#89).
//!
//! As in Node, the servers run without a computer bridge, so the receiver
//! downloads straight from the sender. A [`Relay`] takes over the sender's
//! paired port and shapes each release download (throttle, cut, corrupt,
//! oversize, revoke mid-stream); another relay in front of the receiver can
//! lose the apply reply.

use crate::pr_support::{call, command, first_machine, pause, send_to};
use minarbot_testkit::{
    db::Fixture,
    prelude::*,
    relay::{self, Answer, Bytes, HttpClient, Relay, Upstream},
    release_fixture::{self, MockLauncher},
    Servers,
};
use std::{
    fs,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

type Shaped = Pin<Box<dyn Future<Output = Result<Answer>> + Send>>;
type Transfer = Arc<dyn Fn(Upstream) -> Shaped + Send + Sync>;

fn locked<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Clone, Debug)]
struct Range {
    range: Option<String>,
    status: u16,
}

struct Shared {
    transfer: Mutex<Transfer>,
    ranges: Mutex<Vec<Range>>,
    applied: Arc<AtomicUsize>,
    launcher_available: Arc<AtomicBool>,
    apply_started: Mutex<Option<Instant>>,
    apply_response: Mutex<Option<Instant>>,
    targets: Arc<Mutex<Vec<String>>>,
    receiver: Mutex<String>,
    backend: Mutex<String>,
    back: String,
}

fn transfer<F, Fut>(shape: F) -> Transfer
where
    F: Fn(Upstream) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Answer>> + Send + 'static,
{
    Arc::new(move |upstream| Box::pin(shape(upstream)))
}

fn forward() -> Transfer {
    transfer(|upstream: Upstream| async move { Ok(upstream.relay()) })
}

/// Pace the reply to `rate` bytes per second in 8 KiB parts.
fn throttle(rate: u64) -> Transfer {
    transfer(move |upstream: Upstream| async move {
        let (status, headers) = (upstream.status, upstream.headers.clone());
        let started = Instant::now();
        let stream = relay::rate_limited(upstream.stream(), rate, started);
        Ok(Answer::stream(status, headers, stream))
    })
}

/// First 16 KiB of the first chunk, a 30 ms pause, then a cut connection.
fn drop_after_first_chunk() -> Transfer {
    transfer(|upstream: Upstream| async move {
        let (status, headers) = (upstream.status, upstream.headers.clone());
        let mut stream = upstream.stream();
        let first = relay::next(&mut stream).await.transpose()?;
        let part = first.map(|chunk| chunk.slice(..chunk.len().min(16384)));
        Ok(Answer::stream(
            status,
            headers,
            relay::then_cut(part, Duration::from_millis(30)),
        ))
    })
}

struct Peers {
    shared: Arc<Shared>,
    sender: Server,
    receiver: Server,
    releases: PathBuf,
    received: PathBuf,
    machine: String,
    _relays: Vec<Relay>,
    _launcher: MockLauncher,
    _roots: Vec<TestRoot>,
    _servers: Servers,
}

fn client(budget: Duration) -> Result<HttpClient> {
    Ok(HttpClient::builder()
        .no_proxy()
        .redirect(relay::redirect_none())
        .timeout(budget)
        .build()?)
}

async fn attempt_at(backend: &str) -> Result<Value> {
    let (status, body) = send_to(
        &client(Duration::from_secs(30))?,
        backend,
        "/api/updates/machines",
        None,
        None,
        false,
    )
    .await?;
    ensure!(status == 200, "update machines: {status} {body}");
    Ok(body["machines"][0]["fanOutAttempt"].clone())
}

async fn permission_at(receiver: &str, back: &str, allowed: bool) -> Result<()> {
    let (status, body) = send_to(
        &client(Duration::from_secs(30))?,
        receiver,
        "/api/machines/updates",
        Some(&json!({"id": new_id(), "environmentId": back, "allowed": allowed})),
        None,
        true,
    )
    .await?;
    ensure!(status == 200, "update permission: {status} {body}");
    Ok(())
}

async fn fan_out_at(backend: &str, machine: &str, version: &str) -> Result<Value> {
    let (status, body) = send_to(
        &client(Duration::from_secs(300))?,
        backend,
        "/api/updates/fan-out",
        Some(&json!({"id": new_id(), "machines": [machine], "version": version})),
        None,
        true,
    )
    .await?;
    ensure!(
        [200, 201, 202].contains(&status),
        "fan-out: {status} {body}"
    );
    Ok(body["machines"][0].clone())
}

/// Replace the environment origin, as `forwardPeer` did.
async fn forward_peer(exchange: &relay::Exchange, target: &str, origin: &str) -> Result<Answer> {
    let upstream = exchange.forward(target).await?;
    if exchange.path != "/api/environment" {
        return Ok(upstream.relay());
    }
    let status = upstream.status;
    let mut value: Value = serde_json::from_slice(&upstream.bytes().await?)?;
    value["origin"] = json!(origin);
    Ok(Answer::json(status, &value))
}

impl Peers {
    async fn start(update_timeout: Option<u64>, relay_apply: bool) -> Result<Self> {
        let servers = Servers::reserve(2).await?;
        let dir = TestRoot::new()?;
        let releases = dir.dir("releases")?;
        let received = dir.path().join("received");
        let control = dir.path().join("control.sock");
        let launcher_available = Arc::new(AtomicBool::new(true));
        let applied = Arc::new(AtomicUsize::new(0));
        let (available, count) = (launcher_available.clone(), applied.clone());
        let targets: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen_targets = targets.clone();
        let launcher = MockLauncher::start(&control, move |request| {
            if request["type"] == "apply" {
                locked(&seen_targets).push(request["target"].to_string());
                count.fetch_add(1, Ordering::SeqCst);
                json!({"accepted": true, "id": request["id"]})
            } else {
                json!({"protocol": if available.load(Ordering::SeqCst) { 1 } else { 0 },
                    "current": "development"})
            }
        })
        .await?;
        let mut from_updates = json!({"releasesDir": releases});
        if let Some(timeout) = update_timeout {
            from_updates["testPeerUpdateTimeoutMs"] = json!(timeout);
        }
        let from = ServerConfig::new()
            .real_clock()
            .option("updates", from_updates);
        let to = ServerConfig::new().real_clock().option(
            "updates",
            json!({"releasesDir": received, "launcherSocket": control}),
        );
        let (root_from, root_to) = (TestRoot::new()?, TestRoot::new()?);
        let mut sender = root_from.start_in(&servers, from.clone()).await?;
        let mut receiver = root_to.start_in(&servers, to.clone()).await?;
        let code = call(&receiver, "/api/pairing-codes", Some(command()), 200).await?;
        let paired = call(
            &sender,
            "/api/machines",
            Some(json!({"id": new_id(), "origin": receiver.origin(), "code": code["code"]})),
            200,
        )
        .await?;
        let machine = paired["id"].as_str().unwrap_or_default().to_owned();
        let back = first_machine(&receiver).await?["id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        permission_at(sender.origin(), &machine, true).await?;
        permission_at(receiver.origin(), &back, true).await?;
        let shared = Arc::new(Shared {
            transfer: Mutex::new(forward()),
            ranges: Mutex::new(Vec::new()),
            applied,
            launcher_available,
            apply_started: Mutex::new(None),
            apply_response: Mutex::new(None),
            targets,
            receiver: Mutex::new(receiver.origin().to_owned()),
            backend: Mutex::new(String::new()),
            back,
        });
        let mut relays = Vec::new();
        if relay_apply {
            let origin = receiver.origin().to_owned();
            receiver.shutdown().await?;
            let state = shared.clone();
            let public = origin.clone();
            relays.push(
                Relay::bind(relay::port(&origin)?, move |exchange| {
                    let (state, public) = (state.clone(), public.clone());
                    async move {
                        let apply = exchange.path == "/api/peer/updates/apply";
                        if apply {
                            locked(&state.apply_started).get_or_insert_with(Instant::now);
                        }
                        let target = locked(&state.receiver).clone();
                        if apply && locked(&state.apply_response).is_none() {
                            let upstream = exchange.forward(&target).await?;
                            *locked(&state.apply_response) = Some(Instant::now());
                            drop(upstream);
                            return Ok(Answer::Drop);
                        }
                        forward_peer(&exchange, &target, &public).await
                    }
                })
                .await?,
            );
            receiver.start_stopped(to).await?;
            *locked(&shared.receiver) = receiver.origin().to_owned();
        }
        // Keep the paired public origin stable while inserting the shaping relay.
        let initial = sender.origin().to_owned();
        sender.shutdown().await?;
        let state = shared.clone();
        let public = initial.clone();
        relays.push(
            Relay::bind(relay::port(&initial)?, move |exchange| {
                let (state, public) = (state.clone(), public.clone());
                async move {
                    let backend = locked(&state.backend).clone();
                    if !exchange.path.starts_with("/api/updates/releases/") {
                        return forward_peer(&exchange, &backend, &public).await;
                    }
                    let upstream = exchange.forward(&backend).await?;
                    locked(&state.ranges).push(Range {
                        range: exchange.header("range").map(str::to_owned),
                        status: upstream.status,
                    });
                    let shape = locked(&state.transfer).clone();
                    shape(upstream).await
                }
            })
            .await?,
        );
        sender.start_stopped(from).await?;
        *locked(&shared.backend) = sender.origin().to_owned();
        Ok(Self {
            shared,
            sender,
            receiver,
            releases,
            received,
            machine,
            _relays: relays,
            _launcher: launcher,
            _roots: vec![dir, root_from, root_to],
            _servers: servers,
        })
    }

    /// Publish release `1.2.<patch>` with `payload` as its only extra file.
    fn release(&self, patch: u32, payload: &[u8]) -> Result<Value> {
        let version = format!("1.2.{patch}");
        let id = format!("{version}-abcdef12");
        let path = self.releases.join(&id);
        fs::create_dir_all(&path)?;
        fs::write(
            path.join("package.json"),
            json!({"version": version}).to_string(),
        )?;
        fs::write(path.join("payload"), payload)?;
        let info = release_fixture::info(
            &self.releases,
            &id,
            &version,
            &format!("abcdef12{}", "0".repeat(32)),
            "",
        )?;
        release_fixture::write_metadata(&self.releases, &info, true)?;
        Ok(info)
    }

    fn applied(&self) -> usize {
        self.shared.applied.load(Ordering::SeqCst)
    }

    fn set_transfer(&self, shape: Transfer) {
        *locked(&self.shared.transfer) = shape;
    }

    fn ranges(&self) -> Vec<Range> {
        locked(&self.shared.ranges).clone()
    }

    fn clear_ranges(&self) {
        locked(&self.shared.ranges).clear();
    }

    fn stderr(&self) -> Result<String> {
        Ok(self.receiver.output()?.1)
    }

    async fn attempt(&self) -> Result<Value> {
        attempt_at(self.sender.origin()).await
    }

    async fn fan_out(&self, version: &str) -> Result<Value> {
        fan_out_at(self.sender.origin(), &self.machine, version).await
    }

    /// Fan-out running in the background, for checks while it transfers.
    fn fan_out_later(&self, version: &str) -> tokio::task::JoinHandle<Result<Value>> {
        let (backend, machine, version) = (
            self.sender.origin().to_owned(),
            self.machine.clone(),
            version.to_owned(),
        );
        tokio::spawn(async move { fan_out_at(&backend, &machine, &version).await })
    }

    async fn set_receiver_permission(&self, allowed: bool) -> Result<()> {
        permission_at(self.receiver.origin(), &self.shared.back, allowed).await
    }

    fn mark_sender_offline_on_receiver(&self) -> Result<()> {
        let db = Fixture::open(self.receiver.database())?;
        db.execute("UPDATE paired_machines SET status='offline'", [])?;
        Ok(())
    }

    async fn restart_receiver(&mut self) -> Result<()> {
        let port = relay::port(self.receiver.origin())?;
        let config = self.receiver.config().clone().option("port", port);
        self.receiver.restart_with(config).await?;
        *locked(&self.shared.receiver) = self.receiver.origin().to_owned();
        Ok(())
    }

    fn partials(&self) -> Result<Vec<PathBuf>> {
        partials(&self.received)
    }
}

fn partials(received: &Path) -> Result<Vec<PathBuf>> {
    if !received.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(received)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(".peer-release-partial-")
        {
            out.push(entry.path());
        }
    }
    Ok(out)
}

fn partial_bytes(received: &Path) -> Result<u64> {
    Ok(match partials(received)?.first() {
        Some(path) => fs::metadata(path)?.len(),
        None => 0,
    })
}

fn random_bytes(length: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(length + 16);
    while bytes.len() < length {
        bytes.extend_from_slice(uuid_bytes().as_slice());
    }
    bytes.truncate(length);
    bytes
}

fn uuid_bytes() -> [u8; 16] {
    let id = new_id().replace('-', "");
    let mut out = [0_u8; 16];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&id[index * 2..index * 2 + 2], 16).unwrap_or(0);
    }
    out
}

fn number(value: &Value, field: &str) -> i64 {
    value[field].as_i64().unwrap_or(-1)
}

#[tokio::test]
async fn peer_fan_out_follows_receiver_progress_beyond_the_apply_request_deadline() -> Result<()> {
    let deadline = 1_000_u64;
    let peers = Peers::start(Some(deadline), true).await?;
    let release = peers.release(11, &random_bytes(90_000))?;
    peers.set_transfer(throttle(25_000));
    let started = Instant::now();
    let version = release["version"].as_str().unwrap_or_default().to_owned();
    let outcome = peers.fan_out_later(&version);
    tokio::time::sleep(Duration::from_millis(deadline + 350)).await;
    let before = partial_bytes(&peers.received)?;
    let attempt = peers.attempt().await?;
    pause(500).await;
    let after = partial_bytes(&peers.received)?;
    let apply_started = *locked(&peers.shared.apply_started);
    let apply_response = *locked(&peers.shared.apply_response);
    ensure!(
        before > 0 && after > before,
        "receiver partial {before} to {after} bytes ({:?} after start)",
        started.elapsed()
    );
    ensure!(attempt["state"] == "transferring", "attempt: {attempt}");
    let transferred = number(&attempt, "bytesTransferred");
    ensure!(
        transferred > 0 && transferred <= after as i64,
        "bytesTransferred {transferred} of {after}"
    );
    let latency = apply_started
        .zip(apply_response)
        .map(|(start, reply)| reply.saturating_duration_since(start));
    ensure!(
        latency.is_some_and(
            |latency| latency > Duration::ZERO && latency < Duration::from_millis(deadline)
        ),
        "apply reply latency {latency:?}"
    );
    ensure!(peers.applied() == 0, "applied before transfer finished");
    let outcome = outcome.await??;
    ensure!(outcome["status"] == "accepted", "outcome: {outcome}");
    let done = peers.attempt().await?;
    ensure!(
        number(&done, "bytesTransferred") == number(&done, "totalBytes"),
        "done: {done}"
    );
    ensure!(peers.applied() == 1, "applied {}", peers.applied());
    Ok(())
}

#[tokio::test]
async fn peer_fan_out_succeeds_while_the_receiver_still_lists_the_restarted_sender_as_offline(
) -> Result<()> {
    let peers = Peers::start(None, false).await?;
    let release = peers.release(1, &random_bytes(65_536))?;
    peers.mark_sender_offline_on_receiver()?;
    let result = peers
        .fan_out(release["version"].as_str().unwrap_or_default())
        .await?;
    ensure!(result["status"] == "accepted", "fan-out: {result}");
    ensure!(peers.applied() == 1, "applied {}", peers.applied());
    Ok(())
}

fn is_resume(range: &Option<String>) -> bool {
    range
        .as_deref()
        .and_then(|value| value.strip_prefix("bytes="))
        .and_then(|value| value.strip_suffix('-'))
        .is_some_and(|offset| {
            !offset.is_empty()
                && !offset.starts_with('0')
                && offset.chars().all(|c| c.is_ascii_digit())
        })
}

#[tokio::test]
async fn peer_fan_out_resumes_dropped_downloads_including_after_receiver_restart() -> Result<()> {
    let mut peers = Peers::start(None, false).await?;
    let bytes = random_bytes(524_288);
    let first = peers.release(1, &bytes)?;
    let progress = Arc::new(Mutex::new(Vec::<i64>::new()));
    let (seen, shared, backend) = (
        progress.clone(),
        peers.shared.clone(),
        peers.sender.origin().to_owned(),
    );
    peers.set_transfer(transfer(move |upstream| {
        let (seen, shared, backend) = (seen.clone(), shared.clone(), backend.clone());
        async move {
            let attempt = attempt_at(&backend).await?;
            locked(&seen).push(number(&attempt, "bytesTransferred"));
            let shape = if locked(&shared.ranges).len() == 1 {
                drop_after_first_chunk()
            } else {
                forward()
            };
            shape(upstream).await
        }
    }));
    let result = peers
        .fan_out(first["version"].as_str().unwrap_or_default())
        .await?;
    ensure!(result["status"] == "accepted", "fan-out: {result}");
    let ranges = peers.ranges();
    ensure!(ranges.len() == 2, "ranges: {ranges:?}");
    ensure!(ranges[1].status == 206, "resume status: {ranges:?}");
    ensure!(is_resume(&ranges[1].range), "resume range: {ranges:?}");
    let done = peers.attempt().await?;
    ensure!(done["releaseId"] == first["id"], "attempt release: {done}");
    ensure!(done["state"] == "accepted", "attempt state: {done}");
    ensure!(done["error"].is_null(), "attempt error: {done}");
    ensure!(
        number(&done, "bytesTransferred") == number(&done, "totalBytes"),
        "attempt bytes: {done}"
    );
    let progress = locked(&progress).clone();
    ensure!(
        progress.len() == 2
            && progress[0] <= progress[1]
            && progress[1] <= number(&done, "bytesTransferred"),
        "progress {progress:?} then {done}"
    );
    ensure!(
        done["finishedAt"].as_i64() >= done["startedAt"].as_i64() && !done["finishedAt"].is_null(),
        "attempt times: {done}"
    );

    let second = peers.release(2, &bytes)?;
    peers.clear_ranges();
    let shared = peers.shared.clone();
    peers.set_transfer(transfer(move |upstream| {
        let shared = shared.clone();
        async move {
            if locked(&shared.ranges).len() == 1 {
                return drop_after_first_chunk()(upstream).await;
            }
            Ok(Answer::empty(502, Vec::new()))
        }
    }));
    let version = second["version"].as_str().unwrap_or_default().to_owned();
    let failed = peers.fan_out(&version).await?;
    ensure!(failed["status"] == "failed", "fan-out: {failed}");
    ensure!(peers.ranges().len() == 4, "ranges: {:?}", peers.ranges());
    let attempt = peers.attempt().await?;
    ensure!(
        attempt["error"]["code"] == "peer_connection_lost",
        "attempt: {attempt}"
    );
    ensure!(
        peers.stderr()?.contains("code=peer_connection_lost"),
        "receiver log lacks the failure code"
    );
    ensure!(peers.applied() == 1, "applied {}", peers.applied());
    ensure!(!peers.partials()?.is_empty(), "partial was not retained");
    peers.restart_receiver().await?;
    peers.clear_ranges();
    peers.set_transfer(forward());
    let resumed = peers.fan_out(&version).await?;
    ensure!(resumed["status"] == "accepted", "fan-out: {resumed}");
    let ranges = peers.ranges();
    ensure!(
        ranges.first().is_some_and(|range| is_resume(&range.range)),
        "restart resume range: {ranges:?}"
    );
    ensure!(
        ranges.first().map(|range| range.status) == Some(206),
        "restart resume status: {ranges:?}"
    );
    ensure!(peers.applied() == 2, "applied {}", peers.applied());
    ensure!(peers.partials()?.is_empty(), "partial left behind");
    Ok(())
}

#[tokio::test]
async fn peer_fan_out_reports_receiver_restart_and_can_resume_its_retained_partial() -> Result<()> {
    let mut peers = Peers::start(None, false).await?;
    let release = peers.release(9, &random_bytes(524_288))?;
    let retained = Arc::new(Mutex::new(0_usize));
    let marked = minarbot_testkit::async_model::Gate::new();
    let (kept, mark) = (retained.clone(), marked.clone());
    peers.set_transfer(transfer(move |upstream| {
        let (kept, mark) = (kept.clone(), mark.clone());
        async move {
            let (status, headers) = (upstream.status, upstream.headers.clone());
            let mut stream = upstream.stream();
            let first = relay::next(&mut stream).await.transpose()?;
            let part = first.map(|chunk| chunk.slice(..chunk.len().min(16384)));
            *locked(&kept) = part.as_ref().map_or(0, Bytes::len);
            mark.open();
            Ok(Answer::stream(status, headers, relay::then_hang(part)))
        }
    }));
    let version = release["version"].as_str().unwrap_or_default().to_owned();
    let outcome = peers.fan_out_later(&version);
    tokio::time::timeout(Duration::from_secs(30), marked.wait())
        .await
        .map_err(|_| "download never started")?;
    minarbot_testkit::wait::until("receiver progress", || async {
        Ok((number(&peers.attempt().await?, "bytesTransferred") > 0).then_some(()))
    })
    .await?;
    peers.restart_receiver().await?;
    let interrupted = outcome.await??;
    ensure!(
        interrupted["status"] == "failed",
        "interrupted: {interrupted}"
    );
    ensure!(
        interrupted["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("Receiving server restarted"),
        "interrupted reason: {interrupted}"
    );
    ensure!(peers.applied() == 0, "applied {}", peers.applied());
    let retained = *locked(&retained);
    ensure!(retained > 0, "nothing retained");
    peers.clear_ranges();
    peers.set_transfer(forward());
    let resumed = peers.fan_out(&version).await?;
    ensure!(resumed["status"] == "accepted", "fan-out: {resumed}");
    let ranges: Vec<_> = peers
        .ranges()
        .into_iter()
        .map(|range| (range.range, range.status))
        .collect();
    ensure!(
        ranges == [(Some(format!("bytes={retained}-")), 206)],
        "resume ranges: {ranges:?}"
    );
    ensure!(peers.applied() == 1, "applied {}", peers.applied());
    ensure!(peers.partials()?.is_empty(), "partial left behind");
    Ok(())
}

#[tokio::test]
async fn peer_fan_out_reports_timeout_integrity_size_permission_and_launcher_failures() -> Result<()>
{
    let peers = Peers::start(None, false).await?;
    let bytes = random_bytes(524_288);
    async fn fails(
        peers: &Peers,
        patch: u32,
        code: &str,
        bytes: &[u8],
        shape: Transfer,
    ) -> Result<()> {
        let release = peers.release(patch, bytes)?;
        peers.set_transfer(shape);
        let result = peers
            .fan_out(release["version"].as_str().unwrap_or_default())
            .await?;
        ensure!(result["status"] == "failed", "{code}: {result}");
        let attempt = peers.attempt().await?;
        ensure!(attempt["state"] == "failed", "{code} attempt: {attempt}");
        ensure!(
            attempt["error"]["code"] == code,
            "{code} attempt: {attempt}"
        );
        ensure!(
            attempt["error"]["message"] == result["reason"],
            "{code} message: {attempt} vs {result}"
        );
        ensure!(
            peers.stderr()?.contains(&format!("code={code}")),
            "receiver log lacks code={code}"
        );
        let id = release["id"].as_str().unwrap_or_default();
        ensure!(
            !peers.received.join(id).exists(),
            "{code}: release was published"
        );
        ensure!(
            peers.applied() == 0,
            "{code}: applied {} {:?} result {result} attempt {attempt}\nRECEIVER {}\nSENDER {}",
            peers.applied(),
            locked(&peers.shared.targets),
            peers.receiver.output()?.1,
            peers.sender.output()?.1
        );
        Ok(())
    }
    fails(
        &peers,
        3,
        "peer_transfer_timeout",
        &bytes,
        transfer(|_| async { Ok(Answer::empty(504, Vec::new())) }),
    )
    .await?;
    fails(
        &peers,
        4,
        "peer_integrity_mismatch",
        &bytes,
        transfer(|upstream: Upstream| async move {
            let (status, headers) = (upstream.status, upstream.headers.clone());
            Ok(Answer::stream(
                status,
                headers,
                relay::flip_first_byte(upstream.stream()),
            ))
        }),
    )
    .await?;
    fails(
        &peers,
        5,
        "peer_size_limit",
        &bytes,
        transfer(|upstream: Upstream| async move {
            let mut headers = relay::without_length(upstream.headers.clone());
            headers.push(("content-length".into(), "1073741825".into()));
            Ok(Answer::stream(200, headers, relay::hang()))
        }),
    )
    .await?;
    let (receiver, back) = (
        peers.receiver.origin().to_owned(),
        peers.shared.back.clone(),
    );
    fails(
        &peers,
        6,
        "peer_permission_revoked",
        &bytes,
        transfer(move |upstream: Upstream| {
            let (receiver, back) = (receiver.clone(), back.clone());
            async move {
                let (status, headers) = (upstream.status, upstream.headers.clone());
                let mut stream = upstream.stream();
                let first = relay::next(&mut stream).await.transpose()?;
                permission_at(&receiver, &back, false).await?;
                Ok(Answer::stream(
                    status,
                    headers,
                    relay::prepend(first, stream),
                ))
            }
        }),
    )
    .await?;
    peers.set_receiver_permission(true).await?;
    peers
        .shared
        .launcher_available
        .store(false, Ordering::SeqCst);
    fails(&peers, 7, "peer_launcher_unavailable", &bytes, forward()).await?;
    Ok(())
}

#[tokio::test]
async fn kb_per_second_25_control_keeps_fan_out_open_beyond_60_seconds() -> Result<()> {
    let peers = Peers::start(None, false).await?;
    let release = peers.release(10, &random_bytes(1_625_000))?;
    peers.set_transfer(throttle(25_000));
    let started = Instant::now();
    let result = peers
        .fan_out(release["version"].as_str().unwrap_or_default())
        .await?;
    ensure!(result["status"] == "accepted", "fan-out: {result}");
    let elapsed = started.elapsed();
    ensure!(
        elapsed > Duration::from_secs(60),
        "fan-out finished after {elapsed:?}"
    );
    ensure!(peers.applied() == 1, "applied {}", peers.applied());
    Ok(())
}
