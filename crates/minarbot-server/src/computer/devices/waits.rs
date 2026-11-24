use super::*;
use tokio::sync::oneshot;

struct Job {
    event: bool,
    request: Value,
    digest: String,
    result: oneshot::Sender<Result<Value>>,
}
static JOBS: OnceLock<Mutex<HashMap<String, Job>>> = OnceLock::new();
static PROVISIONS: OnceLock<Mutex<HashMap<String, (String, i64)>>> = OnceLock::new();
fn jobs() -> Result<std::sync::MutexGuard<'static, HashMap<String, Job>>> {
    JOBS.get_or_init(Default::default)
        .lock()
        .map_err(|_| Failure::storage())
}
fn invalid(message: &str) -> Failure {
    fail(409, "wait_invalid", message)
}
fn offline() -> Failure {
    let mut e = fail(503, "device_offline", "Home device is not connected yet.");
    e.retryable = true;
    e
}
fn wait_key(value: &Value) -> Result<()> {
    if value
        .as_str()
        .is_none_or(|s| s.is_empty() || s.encode_utf16().count() > 1024)
    {
        return Err(fail(400, "wait_invalid", "Invalid wait key."));
    }
    Ok(())
}
fn hash(value: &Value) -> Result<()> {
    if value.as_str().is_none_or(|s| {
        s.len() != 64
            || !s
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }) {
        return Err(fail(
            400,
            "invalid_digest",
            "Digest must be lowercase SHA-256.",
        ));
    }
    Ok(())
}
fn event_id(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        !s.is_empty()
            && s.len() <= 200
            && s.as_bytes()[0].is_ascii_alphanumeric()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
    })
}
fn parse_request(v: &Value, event: bool) -> Result<()> {
    fields(
        v,
        if event {
            &[
                "waitKey",
                "sourceDeviceId",
                "deviceGeneration",
                "authorityGeneration",
                "secretGeneration",
                "connectionGeneration",
                "expiresAt",
                "byteLength",
                "bodyDigest",
                "headers",
                "accepted",
            ]
        } else {
            &[
                "requestId",
                "waitKey",
                "sourceDeviceId",
                "deviceGeneration",
                "authorityGeneration",
                "secretGeneration",
                "ownerAccountId",
                "expiresAt",
                "ephemeralPublicKey",
                "connectionGeneration",
            ]
        },
    )?;
    wait_key(&v["waitKey"])?;
    for k in ["sourceDeviceId", "authorityGeneration"] {
        id(&v[k])?;
    }
    for k in [
        "deviceGeneration",
        "secretGeneration",
        "connectionGeneration",
    ] {
        revision(&v[k], 1)?;
    }
    revision(&v["expiresAt"], 0)?;
    if !event {
        id(&v["requestId"])?;
        id(&v["ownerAccountId"])?;
        public_key(&v["ephemeralPublicKey"], "x25519")?;
        return Ok(());
    }
    if revision(&v["byteLength"], 1)? > 65536 {
        return Err(fail(413, "payload_too_large", "Event exceeds 64 KiB."));
    }
    hash(&v["bodyDigest"])?;
    let h = &v["headers"];
    fields(
        h,
        &["x-minarbot-time", "x-minarbot-id", "x-minarbot-signature"],
    )?;
    if h["x-minarbot-time"]
        .as_str()
        .is_none_or(|s| s.is_empty() || s.len() > 12 || !s.bytes().all(|b| b.is_ascii_digit()))
        || !event_id(&h["x-minarbot-id"])
        || hash(&h["x-minarbot-signature"]).is_err()
    {
        return Err(fail(
            401,
            "event_signature_invalid",
            "Invalid signed event header.",
        ));
    }
    if let Some(a) = v.get("accepted") {
        fields(a, &["id", "digest"])?;
        if !event_id(&a["id"]) {
            return Err(invalid("Wait event does not match."));
        }
        hash(&a["digest"])?;
    }
    Ok(())
}
pub(super) fn validate_host_request(value: &Value, event: bool) -> Result<()> {
    parse_request(value, event)
}
fn connected<'a>(
    db: &Connection,
    rt: &'a Runtime,
    device: &str,
    generation: i64,
    now: i64,
) -> Result<&'a Stream> {
    let stream = rt
        .streams
        .get(device)
        .filter(|s| s.principal.generation == generation)
        .ok_or_else(offline)?;
    let row = current(db, &stream.principal, now)?;
    if row["status"] != "ready"
        || row["connection_generation"] != stream.generation
        || row["last_seen"].as_i64().unwrap_or(0) < now - 30000
    {
        return Err(offline());
    }
    Ok(stream)
}
pub(super) fn disconnect(device: &str) {
    if let Ok(mut jobs) = jobs() {
        jobs.retain(|_, j| j.request["sourceDeviceId"] != device);
    }
    if let Ok(mut sent) = PROVISIONS.get_or_init(Default::default).lock() {
        sent.remove(device);
    }
}
#[allow(dead_code)]
pub fn connection(db: &Connection, device: &str, generation: i64, now: i64) -> bool {
    runtime().is_ok_and(|rt| connected(db, &rt, device, generation, now).is_ok())
}
#[allow(dead_code)]
pub fn provision(db: &Connection, value: &Value, now: i64) -> Result<bool> {
    let rt = runtime()?;
    let Ok(stream) = connected(
        db,
        &rt,
        text(value, "sourceDeviceId"),
        revision(&value["sourceDeviceGeneration"], 1)?,
        now,
    ) else {
        return Ok(false);
    };
    if value["authorityGeneration"] != stream.principal.authority {
        return Ok(false);
    }
    let mut sent = PROVISIONS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| Failure::storage())?;
    let device = &stream.principal.device;
    let provision_id = text(value, "provisionId");
    if sent
        .get(device)
        .is_some_and(|(id, at)| id == provision_id && now.saturating_sub(*at) < 5000)
    {
        return Ok(false);
    }
    let frame = db::canonical(
        &json!({"v":1,"connectionGeneration":stream.generation,"kind":"wait.provision.v1","body":value}),
    ) + "\n";
    if frame.len() > 65536 {
        return Err(fail(
            413,
            "payload_too_large",
            "Wait provision exceeds the frame limit.",
        ));
    }
    if stream.sender.try_send(frame).is_err() {
        return Ok(false);
    }
    sent.insert(device.clone(), (provision_id.to_owned(), now));
    Ok(true)
}
fn enqueue(
    db: &Connection,
    value: &Value,
    body: Option<&[u8]>,
    now: i64,
) -> Result<(String, oneshot::Receiver<Result<Value>>)> {
    let event = body.is_some();
    parse_request(value, event)?;
    let job_id = uuid::Uuid::new_v4().to_string();
    let hash = digest(&json!([
        if event {
            "minarbot-wait-event-v1"
        } else {
            "minarbot-wait-reveal-v1"
        },
        value
    ]));
    let (tx, rx) = oneshot::channel();
    {
        let rt = runtime()?;
        let stream = connected(
            db,
            &rt,
            text(value, "sourceDeviceId"),
            revision(&value["deviceGeneration"], 1)?,
            now,
        )?;
        if value["authorityGeneration"] != stream.principal.authority
            || value["connectionGeneration"] != stream.generation
        {
            return Err(offline());
        }
        let mut jobs = jobs()?;
        if jobs.values().filter(|j| j.event == event).count() >= 4
            || jobs
                .values()
                .any(|j| j.event == event && j.request["sourceDeviceId"] == value["sourceDeviceId"])
        {
            let mut e = fail(
                429,
                "server_capacity",
                if event {
                    "Device event is being processed."
                } else {
                    "Device reveal is being processed."
                },
            );
            e.retryable = true;
            return Err(e);
        }
        let mut frames = String::new();
        if let Some(bytes) = body {
            if value["byteLength"] != bytes.len() || value["bodyDigest"] != byte_digest(bytes) {
                return Err(invalid("Wait event does not match."));
            }
            for (index, chunk) in bytes.chunks(24576).enumerate() {
                frames.push_str(&(db::canonical(&json!({"v":1,"connectionGeneration":stream.generation,"kind":"wait.event.v1","body":{"jobId":job_id,"request":value,"payloadDigest":hash,"offset":index*24576,"chunk":B64.encode(chunk)}}))+"\n"));
            }
        } else {
            frames = db::canonical(
                &json!({"v":1,"connectionGeneration":stream.generation,"kind":"wait.reveal.v1","body":{"jobId":job_id,"request":value,"payloadDigest":hash}}),
            ) + "\n";
            if frames.len() > 65536 {
                return Err(fail(
                    413,
                    "payload_too_large",
                    "Reveal request exceeds the frame limit.",
                ));
            }
        }
        stream.sender.try_send(frames).map_err(|_| offline())?;
        jobs.insert(
            job_id.clone(),
            Job {
                event,
                request: value.clone(),
                digest: hash,
                result: tx,
            },
        );
    }
    Ok((job_id, rx))
}
#[allow(dead_code)]
pub async fn request(state: &AppState, value: &Value, body: Option<&[u8]>) -> Result<Value> {
    let (job_id, rx) = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        enqueue(&guard.connection, value, body, state.config.now())?
    };
    struct Remove(String);
    impl Drop for Remove {
        fn drop(&mut self) {
            if let Ok(mut jobs) = jobs() {
                jobs.remove(&self.0);
            }
        }
    }
    let _remove = Remove(job_id);
    tokio::time::timeout(std::time::Duration::from_secs(5), rx)
        .await
        .map_err(|_| offline())?
        .map_err(|_| offline())?
}
pub(super) fn resolve(
    db: &Connection,
    p: &Principal,
    v: &Value,
    event: bool,
    now: i64,
) -> Result<Value> {
    current(db, p, now)?;
    fields(v, &["requestId", "payloadDigest", "result", "errorCode"])?;
    let job_id = id(&v["requestId"])?;
    let rt = runtime()?;
    let mut jobs = jobs()?;
    let message = if event {
        "Event result does not match."
    } else {
        "Reveal result does not match."
    };
    let job = jobs.get(&job_id).ok_or_else(|| invalid(message))?;
    let request = &job.request;
    if job.event != event
        || request["sourceDeviceId"] != p.device
        || request["deviceGeneration"] != p.generation
        || request["authorityGeneration"] != p.authority
        || v["payloadDigest"] != job.digest
        || rt
            .streams
            .get(&p.device)
            .is_none_or(|s| request["connectionGeneration"] != s.generation)
        || event && request["expiresAt"].as_i64().unwrap_or(0) <= now
    {
        return Err(invalid(message));
    }
    let result = if let Some(code) = v.get("errorCode") {
        let (code, status) = match code.as_str() {
            Some("keystore_unavailable") => ("keystore_unavailable", 503),
            Some("wait_invalid") => ("wait_invalid", 409),
            Some("event_signature_invalid") if event => ("event_signature_invalid", 401),
            Some("event_invalid") if event => ("event_invalid", 400),
            Some("event_conflict") if event => ("event_conflict", 409),
            _ => {
                return Err(fail(
                    400,
                    "wait_invalid",
                    if event {
                        "Invalid event result."
                    } else {
                        "Invalid reveal result."
                    },
                ))
            }
        };
        if v.get("result").is_some() {
            return Err(fail(
                400,
                "wait_invalid",
                if event {
                    "Invalid event result."
                } else {
                    "Invalid reveal result."
                },
            ));
        }
        let mut e = fail(
            status,
            code,
            if event {
                "Event verification denied."
            } else {
                "Wait connection is unavailable."
            },
        );
        e.retryable = status == 503;
        Err(e)
    } else {
        let r = &v["result"];
        if event {
            fields(r, &["id", "digest", "eventTime"])?;
            if !event_id(&r["id"]) {
                return Err(invalid("Wait event does not match."));
            }
            hash(&r["digest"])?;
            revision(&r["eventTime"], 0)?;
            if r["id"] != request["headers"]["x-minarbot-id"]
                || r["digest"] != request["bodyDigest"]
            {
                return Err(invalid("Event result body does not match."));
            }
        } else {
            fields(
                r,
                &[
                    "requestId",
                    "waitKey",
                    "sourceDeviceId",
                    "deviceGeneration",
                    "secretGeneration",
                    "expiresAt",
                    "iv",
                    "ciphertext",
                    "tag",
                ],
            )?;
            for key in ["requestId", "sourceDeviceId"] {
                id(&r[key])?;
            }
            wait_key(&r["waitKey"])?;
            for key in ["deviceGeneration", "secretGeneration"] {
                revision(&r[key], 1)?;
            }
            revision(&r["expiresAt"], 0)?;
            for (key, size) in [("iv", 12), ("ciphertext", 64), ("tag", 16)] {
                decode(&r[key], size)?;
            }
            if [
                "requestId",
                "waitKey",
                "sourceDeviceId",
                "deviceGeneration",
                "secretGeneration",
                "expiresAt",
            ]
            .iter()
            .any(|k| r[k] != request[k])
                || r["expiresAt"].as_i64().unwrap_or(0) <= now
            {
                return Err(invalid(message));
            }
        }
        Ok(r.clone())
    };
    let job = jobs.remove(&job_id).ok_or_else(|| invalid(message))?;
    let _ = job.result.send(result);
    Ok(json!({"requestId":job_id,"accepted":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn bounded_transport_fences_results_and_disconnects() -> Result<()> {
        let db = Connection::open_in_memory()?;
        db.execute_batch("CREATE TABLE authority_state(id INTEGER,generation TEXT,serving_state TEXT);CREATE TABLE accounts(id TEXT,blocked INTEGER);CREATE TABLE installations(id TEXT,account TEXT,revoked INTEGER);CREATE TABLE devices(id TEXT,account TEXT,installation TEXT,generation INTEGER,descriptor_json TEXT,status TEXT,connection_generation INTEGER,last_seen INTEGER);")?;
        let p = Principal {
            device: uuid::Uuid::new_v4().to_string(),
            account: uuid::Uuid::new_v4().to_string(),
            generation: 1,
            authority: uuid::Uuid::new_v4().to_string(),
            session: None,
        };
        db.execute(
            "INSERT INTO authority_state VALUES(1,?,'active')",
            [&p.authority],
        )?;
        db.execute("INSERT INTO accounts VALUES(?,0)", [&p.account])?;
        db.execute(
            "INSERT INTO installations VALUES('fixture',?,0)",
            [&p.account],
        )?;
        db.execute(
            "INSERT INTO devices VALUES(?,?,'fixture',1,?,'ready',7,1000)",
            params![
                p.device,
                p.account,
                json!({"authorityGeneration":p.authority}).to_string()
            ],
        )?;
        let (tx, mut frames) = mpsc::channel(4);
        runtime()?.streams.insert(
            p.device.clone(),
            Stream {
                principal: p.clone(),
                generation: 7,
                sender: tx,
                provider_instances: None,
            },
        );
        let check=async {
            let mut key=vec![0x30,0x2a,0x30,0x05,0x06,0x03,0x2b,0x65,0x6e,0x03,0x21,0x00];key.extend([1;32]);
            let request=json!({"requestId":uuid::Uuid::new_v4().to_string(),"waitKey":"wait-fixture","sourceDeviceId":p.device,"deviceGeneration":1,"authorityGeneration":p.authority,"secretGeneration":1,"ownerAccountId":p.account,"expiresAt":10000,"ephemeralPublicKey":B64.encode(key),"connectionGeneration":7});
            let (id, mut result)=enqueue(&db,&request,None,1000)?;
            assert_eq!(enqueue(&db,&request,None,1000).err().map(|e|e.code),Some("server_capacity"));
            let frame:Value=serde_json::from_str(&frames.recv().await.ok_or_else(Failure::storage)?).map_err(|_|Failure::storage())?;
            assert_eq!(frame["kind"],"wait.reveal.v1");
            let mut reply=json!({"requestId":id,"payloadDigest":frame["body"]["payloadDigest"],"result":{"requestId":request["requestId"],"waitKey":request["waitKey"],"sourceDeviceId":p.device,"deviceGeneration":1,"secretGeneration":1,"expiresAt":10000,"iv":B64.encode([0;12]),"ciphertext":B64.encode([0;64]),"tag":B64.encode([0;16])}});
            reply["payloadDigest"]=json!("0".repeat(64));
            assert_eq!(resolve(&db,&p,&reply,false,1000).err().map(|e|e.code),Some("wait_invalid"));
            reply["payloadDigest"]=frame["body"]["payloadDigest"].clone();
            reply["result"]["requestId"]=json!(uuid::Uuid::new_v4().to_string());
            assert_eq!(resolve(&db,&p,&reply,false,1000).err().map(|e|e.code),Some("wait_invalid"));
            reply["result"]["requestId"]=request["requestId"].clone();
            assert_eq!(resolve(&db,&p,&reply,false,1000)?["accepted"],true);
            assert_eq!(result.try_recv().map_err(|_|Failure::storage())??,reply["result"]);
            assert_eq!(resolve(&db,&p,&reply,false,1000).err().map(|e|e.code),Some("wait_invalid"));
            let bytes=vec![42;65536];
            let event=json!({"waitKey":"wait-fixture","sourceDeviceId":p.device,"deviceGeneration":1,"authorityGeneration":p.authority,"secretGeneration":1,"expiresAt":10000,"connectionGeneration":7,"byteLength":65536,"bodyDigest":byte_digest(&bytes),"headers":{"x-minarbot-time":"1","x-minarbot-id":"event:fixture","x-minarbot-signature":"0".repeat(64)}});
            let (id,mut result)=enqueue(&db,&event,Some(&bytes),1000)?;
            let event_frames=frames.recv().await.ok_or_else(Failure::storage)?;
            assert_eq!(event_frames.lines().count(),3);
            let mut restored=Vec::new();
            for (index,frame) in event_frames.lines().enumerate() {
                assert!(frame.len()<65536);
                let frame:Value=serde_json::from_str(frame).map_err(|_|Failure::storage())?;
                assert_eq!(frame["body"]["offset"],index*24576);
                restored.extend(B64.decode(text(&frame["body"],"chunk")).map_err(|_|Failure::storage())?);
            }
            assert_eq!(restored,bytes);
            let reply=json!({"requestId":id,"payloadDigest":digest(&json!(["minarbot-wait-event-v1",event])),"result":{"id":"event:fixture","digest":event["bodyDigest"],"eventTime":1}});
            assert_eq!(resolve(&db,&p,&reply,true,1000)?["accepted"],true);
            assert_eq!(result.try_recv().map_err(|_|Failure::storage())??,reply["result"]);
            assert!(!connection(&db,&p.device,2,1000));
            assert!(!connection(&db,&p.device,1,31001));
            for _ in 0..4 {
                assert!(send(&p.device,7,"occupied\n".into())?);
            }
            assert_eq!(enqueue(&db,&request,None,1000).err().map(|e|e.code),Some("device_offline"));
            for _ in 0..4 {
                assert_eq!(frames.try_recv().map_err(|_|Failure::storage())?,"occupied\n");
            }
            let (id,mut result)=enqueue(&db,&request,None,1000)?;
            let payload_digest=digest(&json!(["minarbot-wait-reveal-v1",request]));
            let failed=json!({"requestId":id,"payloadDigest":payload_digest,"errorCode":"keystore_unavailable"});
            assert_eq!(resolve(&db,&p,&failed,false,1000)?["accepted"],true);
            assert_eq!(result.try_recv().map_err(|_|Failure::storage())?.err().map(|e|e.code),Some("keystore_unavailable"));
            let (id,mut result)=enqueue(&db,&request,None,1000)?;
            close_stream(&p.device)?;
            assert!(matches!(result.try_recv(),Err(oneshot::error::TryRecvError::Closed)));
            let delayed=json!({"requestId":id,"payloadDigest":payload_digest,"errorCode":"keystore_unavailable"});
            assert_eq!(resolve(&db,&p,&delayed,false,1000).err().map(|e|e.code),Some("wait_invalid"));
            assert_eq!(enqueue(&db,&request,None,1000).err().map(|e|e.code),Some("device_offline"));
            Ok(())
        }.await;
        close_stream(&p.device)?;
        check
    }
}
