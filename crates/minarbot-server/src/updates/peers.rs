use super::*;
use crate::computer::machines;
use axum::{
    body::{Body, Bytes},
    http::{header, HeaderMap, StatusCode},
};
use sha2::{Digest, Sha256};
use std::{io::SeekFrom, time::Instant};
use tokio::io::AsyncSeekExt;

pub async fn peer_statuses(state: &AppState) -> Result<Value> {
    let mut result = Vec::new();
    for machine in machines::list(state)? {
        let id = string(&machine, "id");
        let mut item = if machine["allowUpdates"] != true {
            json!({"machine":id,"available":false,"reason":"Enable updates on both machines."})
        } else if !["online", "version-mismatch"].contains(&string(&machine, "status")) {
            json!({"machine":id,"available":false,"reason":"Machine is offline."})
        } else {
            match machines::update_remote(state, id, None, 0, |_| Ok(())).await {
                Ok(value) => {
                    let mut item = json!({"machine":id,"available":true});
                    if let Some(map) = value.as_object() {
                        for (key, value) in map {
                            if key != "fanOutAttempt" {
                                item[key] = value.clone();
                            }
                        }
                    }
                    item
                }
                Err(_) => {
                    json!({"machine":id,"available":false,"reason":"Receiving owner must allow updates and start the launcher."})
                }
            }
        };
        if let Some(attempt) = attempts::latest(state, id)? {
            item["fanOutAttempt"] = attempt;
        }
        result.push(item);
    }
    Ok(json!({"machines":result}))
}

pub async fn fan_out(state: &AppState, value: &Value) -> Result<Value> {
    command(value, &["id", "machines", "version", "source"])?;
    let source = source(value)?;
    let invalid = || error(400, "Select 1 to 16 paired machines.");
    let mut ids = value["machines"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 16)
        .ok_or_else(invalid)?
        .iter()
        .map(|id| {
            id.as_str()
                .filter(|id| crate::web::id(id).is_some())
                .ok_or_else(invalid)
        })
        .collect::<Result<Vec<_>>>()?;
    ids.sort_unstable();
    if ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(invalid());
    }
    let mut digest = json!({"version":value["version"],"machines":ids});
    if let Some(source) = source {
        digest["source"] = json!(source);
    }
    let digest = digest.to_string();
    let id = string(value, "id");
    if let Some(saved) = prior(state, id, &digest)? {
        return Ok(saved);
    }
    let release = selected_target(state, string(value, "version"), source).await?;
    let uncertain = json!({"accepted":true,"machines":ids.iter().map(|machine|json!({"machine":machine,"status":"uncertain","reason":"Submission interrupted. Check the receiving machine before retrying."})).collect::<Vec<_>>()});
    if !attempts::record(state, id, &digest, &uncertain, &release.public(), &ids)? {
        return prior(state, id, &digest)?.ok_or_else(Failure::storage);
    }
    // Keep the attempt and archive alive if the owner closes the HTTP request.
    tokio::spawn(run_fan_out(
        state.clone(),
        id.to_owned(),
        release,
        ids.into_iter().map(str::to_owned).collect(),
    ))
    .await
    .map_err(|_| Failure::storage())?
}

async fn run_fan_out(
    state: AppState,
    id: String,
    release: Target,
    ids: Vec<String>,
) -> Result<Value> {
    let release = Arc::new(release);
    let fallback = Arc::new(tokio::sync::OnceCell::new());
    let mut tasks = tokio::task::JoinSet::new();
    let mut ids = ids.into_iter();
    let mut result = Vec::new();
    loop {
        while tasks.len() < 4 {
            let Some(machine) = ids.next() else { break };
            let state = state.clone();
            let (release, fallback, id) = (release.clone(), fallback.clone(), id.clone());
            tasks.spawn(async move {
                let outcome = async {
                    attempts::start(&state, &id, &machine)?;
                    update_peer_release(&state, &id, &machine, &release, &fallback).await
                }.await;
                attempts::finish(&state, &id, &machine, outcome.as_ref().err())?;
                Ok::<_, Failure>(match outcome {
                    Ok(value) => json!({"machine":machine,"status":"accepted","id":value["id"].as_str().unwrap_or("undefined")}),
                    Err(failure) if failure.code == PeerError::Unconfirmed.code() =>
                        json!({"machine":machine,"status":"uncertain","reason":failure.message}),
                    Err(failure) => json!({"machine":machine,"status":"failed","reason":failure.message}),
                })
            });
        }
        match tasks.join_next().await {
            Some(item) => result.push(item.map_err(|_| Failure::storage())??),
            None => break,
        }
    }
    let result = json!({"accepted":true,"machines":result});
    state
        .database
        .lock()
        .map_err(|_| Failure::storage())?
        .connection
        .execute(
            "UPDATE update_commands SET result=? WHERE id=?",
            rusqlite::params![result.to_string(), id],
        )?;
    Ok(result)
}

async fn update_peer_release(
    state: &AppState,
    id: &str,
    machine: &str,
    release: &Target,
    fallback: &tokio::sync::OnceCell<Arc<archive::Archive>>,
) -> Result<Value> {
    let progress = |bytes| attempts::progress(state, id, machine, bytes);
    let mut transfer_id = id.to_owned();
    if let Target::GitHub(manifest) = release {
        attempts::transfer(state, id, machine, manifest.size, github::SOURCE)?;
        let body = json!({"id":id,"version":manifest.version,"target":manifest.id,
            "sha256":manifest.sha256,"size":manifest.size,"source":github::SOURCE});
        match machines::update_remote(state, machine, Some(&body), manifest.size, progress).await {
            Err(failure)
                if failure.code == github::UNREACHABLE
                    || failure.code == PeerError::GitHubUnsupported.code() => {}
            result => return result,
        }
        // A receiver binds the request ID to one transport and archive.
        transfer_id = uuid::Uuid::new_v4().to_string();
    }
    let info = release.public();
    let target = string(&info, "id");
    let archive = fallback
        .get_or_try_init(|| async {
            let options = state.config.update.options();
            let root = options.releases_dir.as_ref().ok_or_else(Failure::storage)?;
            if let Target::GitHub(manifest) = release {
                if !release::verify(root, target).is_ok_and(|info| manifest.matches(&info)) {
                    let _receiving = receive_lock()?;
                    let feed = options.github.as_ref().ok_or_else(github::unreachable)?;
                    feed.stage(root, manifest, || async { Ok(()) }).await?;
                }
            }
            archive::create(root, target).await
        })
        .await?;
    attempts::transfer(state, id, machine, archive.size, LOCAL_SOURCE)?;
    let body =
        json!({"id":transfer_id,"version":info["version"],"target":target,"sha256":archive.sha256});
    machines::update_remote(state, machine, Some(&body), archive.size, progress).await
}

/// Update-permission revocations per machine since this server started. A
/// peer apply that saw one while it transferred never publishes or applies
/// its release, even when the owner allows updates again before it ends.
static REVOCATIONS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, u64>>> =
    std::sync::LazyLock::new(Default::default);

fn revocations(machine: &str) -> u64 {
    REVOCATIONS
        .lock()
        .map_or(u64::MAX, |map| map.get(machine).copied().unwrap_or(0))
}

/// Record that the owner turned off updates from `machine`.
pub(crate) fn updates_revoked(machine: &str) {
    if let Ok(mut map) = REVOCATIONS.lock() {
        *map.entry(machine.to_owned()).or_default() += 1;
    }
}

async fn check_unrevoked(
    state: &AppState,
    machine: &str,
    bearer: Option<&str>,
    epoch: u64,
) -> Result<()> {
    check_permission(state, machine, bearer).await?;
    if revocations(machine) != epoch {
        return Err(PeerError::PermissionRevoked.failure());
    }
    Ok(())
}

async fn check_permission(state: &AppState, machine: &str, bearer: Option<&str>) -> Result<()> {
    if authorize(state, bearer).await? != machine {
        return Err(PeerError::PermissionRevoked.failure());
    }
    machines::ensure_updates_allowed(state, machine).await
}

fn receipt_id(value: &Value, machine: &str) -> Result<String> {
    command(
        value,
        &[
            "id", "version", "target", "sha256", "poll", "source", "size",
        ],
    )?;
    let github = value["source"] == github::SOURCE;
    if (github
        && (value["poll"] != true
            || !value["size"]
                .as_u64()
                .is_some_and(|size| (1..=archive::LIMIT).contains(&size))))
        || (!github && (value.get("source").is_some() || value.get("size").is_some()))
    {
        return Err(error(400, "Invalid peer release source."));
    }
    let target = string(value, "target");
    if !archive::valid_hash(string(value, "sha256"))
        || !release::id(target)
        || !target.starts_with(&format!("{}-", string(value, "version")))
        || value.get("poll").is_some_and(|poll| poll != true)
    {
        return Err(error(400, "Invalid peer release."));
    }
    let hash = format!(
        "{:x}",
        Sha256::digest(format!("update:{machine}:{}", string(value, "id")).as_bytes())
    );
    Ok(format!(
        "{}-{}-4{}-8{}-{}",
        &hash[..8],
        &hash[8..12],
        &hash[13..16],
        &hash[17..20],
        &hash[20..32]
    ))
}

async fn apply_release(
    state: &AppState,
    value: &Value,
    machine: &str,
    bearer: Option<&str>,
    id: &str,
) -> Result<Value> {
    let epoch = revocations(machine);
    let target = string(value, "target");
    let expected = string(value, "sha256");
    let options = state.config.update.options();
    let releases = options
        .releases_dir
        .as_ref()
        .ok_or_else(|| PeerError::LauncherUnavailable.failure())?;
    let status = status(state).await?;
    if status["launcherAvailable"] != true {
        return Err(PeerError::LauncherUnavailable.failure());
    }
    let polling = value["poll"] == true;
    let bytes = if value["source"] == github::SOURCE {
        if !release::newer(
            string(value, "version"),
            string(&status["current"], "version"),
        ) {
            return Err(error(409, "Release version is not an update."));
        }
        let feed = options.github.as_ref().ok_or_else(github::unreachable)?;
        let manifest = feed.check().await?;
        if manifest.id != target
            || manifest.version != value["version"]
            || manifest.sha256 != expected
            || manifest.size != value["size"]
        {
            return Err(github::invalid(
                "Requested version or archive does not match the signed GitHub release.",
            ));
        }
        feed.stage(releases, &manifest, || {
            check_unrevoked(state, machine, bearer, epoch)
        })
        .await?
    } else if releases.join(target).exists() {
        0
    } else {
        archive::receive(
            releases,
            archive::Download::Peer {
                id: target,
                sha256: expected,
            },
            |offset| machines::download_release(state, machine, target, offset, polling),
            || check_unrevoked(state, machine, bearer, epoch),
        )
        .await?
    };
    let release = release::verify(releases, target)
        .map_err(|_| PeerError::IntegrityMismatch.detail("Peer release manifest mismatch."))?;
    if release["version"] != value["version"] {
        return Err(PeerError::IntegrityMismatch.detail("Peer release version mismatch."));
    }
    check_unrevoked(state, machine, bearer, epoch).await?;
    let mut result = launcher(
        options,
        json!({"type":"apply","id":id,"target":release["id"],"version":release["version"]}),
    )
    .await?;
    if polling {
        result["bytesTransferred"] = json!(bytes);
    }
    Ok(result)
}

fn apply_response(value: &Value, machine: &str, result: Result<Value>) -> (u16, Value) {
    match result {
        Ok(value) => (202, value),
        Err(failure) => {
            eprintln!(
                "peer release apply machine={machine} release={:?} code={} message={:?}",
                string(value, "target"),
                failure.code,
                failure.message
            );
            (
                failure.status,
                json!({"error":failure.message,"code":failure.code}),
            )
        }
    }
}

async fn peer_apply(
    state: &AppState,
    value: Value,
    machine: String,
    bearer: Option<String>,
) -> Result<Response> {
    let id = receipt_id(&value, &machine)?;
    if value["poll"] != true {
        let _receiving = receive_lock()?;
        let result = apply_release(state, &value, &machine, bearer.as_deref(), &id).await;
        let (status, body) = apply_response(&value, &machine, result);
        return Ok(json_response(status, body));
    }

    let key = format!("peer-apply:{id}");
    let fingerprint = if value["source"] == github::SOURCE {
        json!([
            value["target"],
            value["version"],
            value["sha256"],
            value["source"],
            value["size"]
        ])
    } else {
        json!([value["target"], value["version"], value["sha256"]])
    }
    .to_string();
    let saved = {
        let db = state.database.lock().map_err(|_| Failure::storage())?;
        let saved = db
            .connection
            .query_row(
                "SELECT request,result FROM update_commands WHERE id=?",
                [&key],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;
        if let Some((request, result)) = saved {
            if request != fingerprint {
                return Err(error(
                    409,
                    "Request ID was already used for another release.",
                ));
            }
            let result: Value = serde_json::from_str(&result).map_err(|_| Failure::storage())?;
            if result["pending"] == true && RECEIVING.try_lock().is_ok() {
                json!({"unconfirmed":true})
            } else {
                result
            }
        } else {
            let receiving = receive_lock()?;
            let pending = json!({"pending":true});
            db.connection.execute(
                "INSERT INTO update_commands VALUES(?,?,?)",
                rusqlite::params![key, fingerprint, pending.to_string()],
            )?;
            let (state, value, id) = (state.clone(), value.clone(), id.clone());
            tokio::spawn(async move {
                let _receiving = receiving;
                let result = apply_release(&state, &value, &machine, bearer.as_deref(), &id).await;
                let (status, body) = apply_response(&value, &machine, result);
                if let Err(failure) =
                    save_update_result(&state, &key, &json!({"status":status,"body":body}))
                {
                    eprintln!("peer release apply receipt {id}: {}", failure.code);
                }
            });
            pending
        }
    };
    if saved["pending"] == true {
        let bytes = downloaded(state, string(&value, "target"), string(&value, "sha256"));
        return Ok(json_response(
            202,
            json!({"pending":true,"id":id,"bytesTransferred":bytes}),
        ));
    }
    if saved["interrupted"] == true || saved["unconfirmed"] == true {
        let control = launcher(state.config.update.options(), json!({"type":"status"})).await?;
        if control["lastAttempt"]["id"] == id {
            return Ok(json_response(202, json!({"accepted":true,"id":id})));
        }
        if saved["unconfirmed"] == true {
            return Err(PeerError::Unconfirmed.failure());
        }
        return Err(error(409, "Receiving server restarted during the update."));
    }
    Ok(json_response(
        saved["status"].as_u64().ok_or_else(Failure::storage)? as u16,
        saved["body"].clone(),
    ))
}

async fn authorize(state: &AppState, bearer: Option<&str>) -> Result<String> {
    machines::authorize_update(state, bearer)
        .await
        .map_err(|failure| {
            if [401, 403].contains(&failure.status) {
                PeerError::PermissionRevoked.failure()
            } else {
                failure
            }
        })
}

fn byte_range(value: &str, size: u64) -> Option<(u64, u64)> {
    let (start, end) = value.strip_prefix("bytes=")?.split_once('-')?;
    let number = |value: &str| {
        value
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| value.parse::<u64>().ok())
            .flatten()
    };
    let last = size.checked_sub(1)?;
    if start.is_empty() {
        let suffix = number(end)?;
        return (suffix > 0).then(|| (size.saturating_sub(suffix), last));
    }
    let start = number(start)?;
    let end = if end.is_empty() {
        last
    } else {
        number(end)?.min(last)
    };
    (start <= end).then_some((start, end))
}

async fn serve_release(
    state: &AppState,
    headers: &HeaderMap,
    machine: String,
    id: &str,
) -> Result<Response> {
    let root = state
        .config
        .update
        .options()
        .releases_dir
        .as_ref()
        .ok_or_else(|| error(404, "Release feed is unavailable."))?;
    let archive = archive::create(root, id).await?;
    authorize(state, crate::text_header(headers, "authorization")).await?;
    let size = archive.size;
    let mut response = Response::builder()
        .header(header::CONTENT_TYPE, "application/gzip")
        .header(header::ACCEPT_RANGES, "bytes")
        .header("x-minarbot-sha256", &archive.sha256);
    let (start, end) = if let Some(value) = headers.get(header::RANGE) {
        let Some((start, end)) = value
            .to_str()
            .ok()
            .and_then(|value| byte_range(value, size))
        else {
            return response
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                .body(Body::empty())
                .map_err(|_| Failure::storage());
        };
        response = response
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"));
        (start, end)
    } else {
        (0, size - 1)
    };
    let length = end - start + 1;
    let mut file = tokio::fs::File::open(&archive.file)
        .await
        .map_err(|_| error(500, "Could not read release."))?;
    file.seek(SeekFrom::Start(start))
        .await
        .map_err(|_| error(500, "Could not seek release."))?;
    let mut file = file.take(length);
    let attempt = if headers.contains_key("x-minarbot-update-poll") {
        None
    } else {
        attempts::active(state, &machine, id)?
    };
    if let Some(attempt) = &attempt {
        attempts::progress(state, attempt, &machine, start)?;
    }
    let state = state.clone();
    let stream = async_stream::stream! {
        let _archive = archive;
        let mut offset = start;
        let mut next_progress = Instant::now();
        while offset <= end {
            let mut bytes = vec![0; 65536];
            let n = match file.read(&mut bytes).await {
                Ok(0) => {
                    yield Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
                    break;
                }
                Ok(n) => n,
                Err(failure) => {
                    yield Err(failure);
                    break;
                }
            };
            offset += n as u64;
            bytes.truncate(n);
            if let Some(attempt) = &attempt {
                if offset > end || Instant::now() >= next_progress {
                    if let Err(failure) = attempts::progress(&state, attempt, &machine, offset) {
                        eprintln!("peer release progress machine={machine} code={}", failure.code);
                    }
                    next_progress = Instant::now() + Duration::from_secs(1);
                }
            }
            yield Ok(Bytes::from(bytes));
        }
    };
    response
        .header(header::CONTENT_LENGTH, length)
        .body(Body::from_stream(stream))
        .map_err(|_| Failure::storage())
}

pub async fn peer(state: &AppState, request: Request) -> Result<Response> {
    if request.headers().contains_key("origin")
        || request.headers().contains_key("sec-fetch-site")
        || request.uri().query().is_some()
    {
        return Err(error(403, "Use the authenticated peer channel."));
    }
    let is_apply =
        request.uri().path() == "/api/peer/updates/apply" && request.method() == Method::POST;
    let result = peer_request(state, request).await;
    Ok(match result {
        Ok(response) => response,
        Err(failure) => {
            if is_apply {
                eprintln!(
                    "peer release apply code={} message={:?}",
                    failure.code, failure.message
                );
            }
            json_response(
                failure.status,
                json!({"error":failure.message,"code":failure.code}),
            )
        }
    })
}

async fn peer_request(state: &AppState, request: Request) -> Result<Response> {
    let bearer = crate::text_header(request.headers(), "authorization");
    let machine = authorize(state, bearer).await?;
    let path = request.uri().path();
    if path == "/api/peer/updates" && request.method() == Method::GET {
        let mut status = status(state).await?;
        status["peerApplyPolling"] = json!(true);
        status["peerGitHubUpdates"] = json!(true);
        return Ok(json_response(200, status));
    }
    if path == "/api/peer/updates/apply" && request.method() == Method::POST {
        if crate::text_header(request.headers(), "content-type").and_then(|v| v.split(';').next())
            != Some("application/json")
        {
            return Err(error(415, "Use Content-Type application/json."));
        }
        let bearer = bearer.map(str::to_owned);
        let bytes = axum::body::to_bytes(request.into_body(), 8192)
            .await
            .map_err(|_| error(400, "Invalid update request."))?;
        let value =
            serde_json::from_slice(&bytes).map_err(|_| error(400, "Invalid update request."))?;
        return peer_apply(state, value, machine, bearer).await;
    }
    if let Some(id) = path
        .strip_prefix("/api/updates/releases/")
        .filter(|_| request.method() == Method::GET)
    {
        return serve_release(state, request.headers(), machine, id).await;
    }
    Err(error(405, "Method not allowed."))
}
