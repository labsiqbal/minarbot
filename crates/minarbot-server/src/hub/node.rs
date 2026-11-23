use super::*;
use axum::extract::{
    ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    FromRequestParts,
};
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use sha2::{Digest, Sha256};

const ED_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0,
];
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Config {
    signing_key_path: PathBuf,
}

pub(super) fn signer(config: &Config) -> crate::db::Result<Ed25519KeyPair> {
    use base64::engine::general_purpose::STANDARD;
    let file = std::fs::read(&config.signing_key_path)?;
    let invalid = "Hub signer must be an Ed25519 PKCS#8 key.";
    let bytes = if let Ok(value) = std::str::from_utf8(&file) {
        let value = value.trim();
        let encoded = value
            .strip_prefix("-----BEGIN PRIVATE KEY-----")
            .and_then(|v| v.strip_suffix("-----END PRIVATE KEY-----"))
            .ok_or(invalid)?;
        STANDARD.decode(encoded.split_ascii_whitespace().collect::<String>())?
    } else {
        file
    };
    Ed25519KeyPair::from_pkcs8_maybe_unchecked(&bytes).map_err(|_| invalid.into())
}
fn deny() -> Failure {
    Failure::new(403, "node_link_denied", "Node link is unavailable.")
}
fn hash(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}
fn key(encoded_key: &str, algorithm: u8) -> Result<Vec<u8>> {
    let raw = encoded(encoded_key, 44).ok_or_else(deny)?;
    let mut prefix = ED_PREFIX;
    prefix[8] = algorithm;
    if !raw.starts_with(&prefix) {
        return Err(deny());
    }
    Ok(raw)
}
fn verify(envelope: &Value, encoded_key: &str) -> Result<Value> {
    closed(envelope, &["v", "keyId", "body", "signature"])?;
    let public = key(encoded_key, 0x70)?;
    let invalid = || Failure::new(401, "invalid_signature", "Invalid node link proof.");
    let sig = encoded(text(envelope, "signature"), 64).ok_or_else(invalid)?;
    let body = &envelope["body"];
    if envelope["v"] != 1 || envelope["keyId"] != hash(&public) || !body.is_object() {
        return Err(invalid());
    }
    UnparsedPublicKey::new(&ED25519, &public[12..])
        .verify(
            format!("minarbot.node.v1\n{}", canonical(body)).as_bytes(),
            &sig,
        )
        .map_err(|_| invalid())?;
    Ok(body.clone())
}

#[cfg(test)]
#[test]
fn verifies_node_payload_key_and_signature_direction() -> crate::db::Result<()> {
    let signer = Ed25519KeyPair::from_seed_unchecked(&[17; 32])
        .map_err(|_| "invalid fixture signing key")?;
    let mut key = ED_PREFIX.to_vec();
    key.extend_from_slice(signer.public_key().as_ref());
    let public = URL_SAFE_NO_PAD.encode(&key);
    let body = json!({"kind":"heartbeat"});
    let envelope = |direction| {
        let signed =
            signer.sign(format!("minarbot.{direction}.v1\n{}", canonical(&body)).as_bytes());
        json!({"v":1,"keyId":hash(&key),"body":body,"signature":URL_SAFE_NO_PAD.encode(signed.as_ref())})
    };
    let valid = envelope("node");
    assert_eq!(verify(&valid, &public).ok(), Some(body.clone()));
    assert!(verify(&envelope("hub"), &public).is_err());
    for (key, value) in [
        ("signature", json!("x")),
        ("body", json!({"kind":"turn.request"})),
        ("keyId", json!("0".repeat(64))),
    ] {
        let mut forged = valid.clone();
        forged[key] = value;
        assert!(verify(&forged, &public).is_err());
    }
    let installation = Ed25519KeyPair::from_seed_unchecked(&[18; 32])
        .map_err(|_| "invalid fixture installation key")?;
    let mut installation_der = ED_PREFIX.to_vec();
    installation_der.extend_from_slice(installation.public_key().as_ref());
    let mut encryption_der = ED_PREFIX.to_vec();
    encryption_der[8] = 0x6e;
    encryption_der.extend_from_slice(&[19; 32]);
    let mut value = json!({"nodeId":new_id(),"installationId":new_id(),
        "installationPublicKey":URL_SAFE_NO_PAD.encode(installation_der),"signingPublicKey":public,
        "encryptionPublicKey":URL_SAFE_NO_PAD.encode(encryption_der),"label":"fixture",
        "nonce":URL_SAFE_NO_PAD.encode([0;32]),"capabilities":["chat"]});
    assert!(candidate(&value).is_ok());
    value["signingPublicKey"] = json!("bad");
    assert!(candidate(&value).is_err());
    Ok(())
}
fn signed(hub: &Hub, body: &Value) -> Result<Value> {
    let signer = hub.signer.as_ref().ok_or_else(deny)?;
    let mut public = ED_PREFIX.to_vec();
    public.extend_from_slice(signer.public_key().as_ref());
    let sig = signer.sign(format!("minarbot.hub.v1\n{}", canonical(body)).as_bytes());
    Ok(
        json!({"v":1,"keyId":hash(&public),"body":body,"signature":URL_SAFE_NO_PAD.encode(sig.as_ref())}),
    )
}
fn origin(hub: &Hub) -> String {
    format!("https://{}", hub.config.accounts_host)
}
fn exact(hub: &Hub, request: &Request) -> Result<()> {
    if single(request.headers(), "host")? != Some(&hub.config.accounts_host)
        || ["forwarded", "x-forwarded-host", "origin", "cookie"]
            .iter()
            .any(|name| request.headers().contains_key(*name))
        || request.uri().query().is_some()
    {
        return Err(deny());
    }
    Ok(())
}
fn fresh(db: &Connection, proof: &Value, scope: &str) -> Result<()> {
    let nonce = text(proof, "nonce");
    let clock = now();
    let issued = integer(&proof["issuedAt"], clock - 60_000)
        .filter(|v| *v <= clock + 5_000)
        .ok_or_else(deny)?;
    let expires = integer(&proof["expiresAt"], clock + 1)
        .filter(|v| *v <= issued + 60_000)
        .ok_or_else(deny)?;
    if encoded(nonce, 32).is_none() {
        return Err(deny());
    }
    db.execute("DELETE FROM node_link_replays WHERE expires_at<=?", [clock])?;
    let added = db.execute(
        "INSERT OR IGNORE INTO node_link_replays VALUES(?,?)",
        params![digest(&format!("{scope}:{nonce}")), expires],
    )?;
    if added != 1 {
        return Err(deny());
    }
    Ok(())
}
fn mint(
    db: &Connection,
    node: &str,
    account: &str,
    node_generation: i64,
    kind: &str,
) -> Result<Value> {
    let token = opaque()?;
    let clock = now();
    let expires = clock + 900_000;
    let generation = generation(db)?;
    db.execute(
        "DELETE FROM node_link_sessions WHERE node_id=? OR expires_at<=?",
        params![node, clock],
    )?;
    db.execute(
        "INSERT INTO node_link_sessions VALUES(?,?,?,?,?,?)",
        params![
            digest(&token),
            node,
            account,
            node_generation,
            generation,
            expires
        ],
    )?;
    Ok(
        json!({"kind":kind,"token":token,"expiresAt":expires,"nodeId":node,"accountId":account,"nodeGeneration":node_generation,"authorityGeneration":generation}),
    )
}
fn candidate(value: &Value) -> Result<Value> {
    closed(
        value,
        &[
            "nodeId",
            "installationId",
            "installationPublicKey",
            "signingPublicKey",
            "encryptionPublicKey",
            "label",
            "nonce",
            "capabilities",
        ],
    )?;
    let node = id(&value["nodeId"])?;
    let installation = id(&value["installationId"])?;
    let install = text(value, "installationPublicKey");
    let signing = text(value, "signingPublicKey");
    let encryption = text(value, "encryptionPublicKey");
    key(install, 0x70)?;
    key(signing, 0x70)?;
    key(encryption, 0x6e)?;
    let label = label(&value["label"], 120)?;
    let nonce = text(value, "nonce");
    if install == signing || encoded(nonce, 32).is_none() {
        return Err(deny());
    }
    let mut caps = Vec::new();
    for cap in value["capabilities"]
        .as_array()
        .filter(|a| a.len() <= 16)
        .ok_or_else(deny)?
    {
        caps.push(
            cap.as_str()
                .filter(|v| {
                    (1..=40).contains(&v.len())
                        && v.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
                })
                .ok_or_else(deny)?,
        );
    }
    caps.sort_unstable();
    caps.dedup();
    Ok(
        json!({"nodeId":node,"installationId":installation,"installationPublicKey":install,"signingPublicKey":signing,
        "encryptionPublicKey":encryption,"label":label,"nonce":nonce,"capabilities":caps}),
    )
}
fn start(hub: &Hub, db: &Connection, peer: IpAddr, input: &Value) -> Result<Value> {
    hub.limit(format!("enroll:{peer}"), 10, 60_000)?;
    hub.limit("enroll:global".to_owned(), 400, 60_000)?;
    closed(input, &["candidate", "installationProof", "nodeProof"])?;
    let c = candidate(&input["candidate"])?;
    let origin = origin(hub);
    let start = json!({"kind":"enrollment.start","candidate":c,"origin":origin});
    if verify(&input["nodeProof"], text(&c, "signingPublicKey"))? != start
        || verify(
            &input["installationProof"],
            text(&c, "installationPublicKey"),
        )? != start
    {
        return Err(deny());
    }
    let clock = now();
    let generation = generation(db)?;
    db.execute("DELETE FROM node_link_codes WHERE expires_at<=?", [clock])?;
    let (total, same): (i64,i64) = db.query_row("SELECT count(*),coalesce(sum(json_extract(candidate,'$.signingPublicKey')=?),0) FROM node_link_codes",
        [text(&c,"signingPublicKey")],|r| Ok((r.get(0)?,r.get(1)?)))?;
    if total >= 4096 || same >= 4 {
        return Err(deny());
    }
    let device = opaque()?;
    let human: String = random::<6>()?.iter().map(|b| format!("{b:02X}")).collect();
    let expires = clock + 300_000;
    let candidate = canonical(&c);
    db.execute(
        "INSERT INTO node_link_codes VALUES(?,?,?,NULL,?,?,0)",
        params![digest(&device), digest(&human), candidate, expires, clock],
    )?;
    Ok(
        json!({"kind":"enrollment.started","deviceCode":device,"humanCode":human,"expiresAt":expires,"intervalMs":5_000,
        "verificationUri":format!("{origin}/nodes/link"),"candidateDigest":digest(&candidate),"authorityGeneration":generation}),
    )
}
pub(super) fn inspect(hub: &Hub, db: &Connection, account: &str, code: &str) -> Result<Value> {
    authority::account(db, account)?;
    hub.limit(format!("claim:{account}"), 10, 60_000)?;
    if code.len() != 12
        || !code
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
    {
        return Err(deny());
    }
    let row = one(
        db,
        "SELECT * FROM node_link_codes WHERE human_digest=? AND expires_at>? AND consumed=0",
        params![digest(code), now()],
    )?
    .filter(|r| r["account_id"].as_str().is_none_or(|v| v == account))
    .ok_or_else(deny)?;
    let c = stored(&row, "candidate")?;
    Ok(
        json!({"nodeId":c["nodeId"],"installationId":c["installationId"],"label":c["label"],"capabilities":c["capabilities"],
        "fingerprint":digest(text(&c,"signingPublicKey")),"accountId":account,"expiresAt":row["expires_at"]}),
    )
}
pub(super) fn approve(db: &Connection, actor: &session::Actor, code: &str) -> Result<()> {
    db.execute(
        "UPDATE node_link_codes SET account_id=? WHERE human_digest=? AND consumed=0",
        params![actor.account, digest(code)],
    )?;
    let name = actor
        .identity
        .verified_mailboxes
        .first()
        .unwrap_or(&actor.identity.subject);
    db.execute(
        "INSERT OR REPLACE INTO node_link_profiles VALUES(?,?)",
        params![actor.account, name],
    )?;
    Ok(())
}
fn poll(hub: &Hub, db: &mut Connection, input: &Value) -> Result<Value> {
    closed(input, &["deviceCode", "proof"])?;
    let code = text(input, "deviceCode");
    if encoded(code, 32).is_none() {
        return Err(deny());
    }
    let code = digest(code);
    let clock = now();
    let row = one(
        db,
        "SELECT * FROM node_link_codes WHERE digest=? AND expires_at>? AND consumed=0",
        params![code, clock],
    )?
    .ok_or_else(deny)?;
    let c = stored(&row, "candidate")?;
    let node = text(&c, "nodeId");
    let proof = verify(&input["proof"], text(&c, "signingPublicKey"))?;
    closed(
        &proof,
        &[
            "kind",
            "deviceCodeDigest",
            "candidateDigest",
            "origin",
            "nonce",
            "issuedAt",
            "expiresAt",
        ],
    )?;
    if proof["kind"] != "enrollment.poll"
        || proof["deviceCodeDigest"] != code
        || proof["candidateDigest"] != digest(&canonical(&c))
        || proof["origin"] != origin(hub)
    {
        return Err(deny());
    }
    fresh(db, &proof, node)?;
    if row["next_poll"].as_i64().is_none_or(|v| v > clock) {
        return Err(Failure::new(429, "slow_down", "Wait before polling."));
    }
    db.execute(
        "UPDATE node_link_codes SET next_poll=? WHERE digest=?",
        params![clock + 5_000, code],
    )?;
    let Some(account) = row["account_id"].as_str() else {
        return Ok(json!({"kind":"enrollment.pending"}));
    };
    transaction(db, |db| {
        authority::account(db, account)?;
        let install = text(&c, "installationId");
        if db
            .prepare("SELECT 1 FROM nodes WHERE id=? OR installation_id=?")?
            .exists([node, install])?
        {
            return Err(Failure::new(
                409,
                "node_exists",
                "Installation is already enrolled.",
            ));
        }
        db.execute("INSERT INTO nodes(id,owner_account_id,installation_id,signing_public_key,encryption_public_key,capabilities_json) VALUES(?,?,?,?,?,?)",
            params![node,account,install,text(&c,"signingPublicKey"),text(&c,"encryptionPublicKey"),canonical(&c["capabilities"])])?;
        db.execute(
            "UPDATE node_link_codes SET consumed=1 WHERE digest=?",
            [code],
        )?;
        mint(db, node, account, 1, "enrollment.complete")
    })
}
fn renew(hub: &Hub, db: &mut Connection, input: &Value) -> Result<Value> {
    closed(input, &["nodeId", "proof"])?;
    let node = id(&input["nodeId"])?;
    let row = one(
        db,
        "SELECT * FROM nodes WHERE id=? AND status!='revoked'",
        [&node],
    )?
    .ok_or_else(deny)?;
    let account = text(&row, "owner_account_id");
    authority::account(db, account)?;
    let proof = verify(&input["proof"], text(&row, "signing_public_key"))?;
    closed(
        &proof,
        &[
            "kind",
            "nodeId",
            "nodeGeneration",
            "authorityGeneration",
            "origin",
            "nonce",
            "issuedAt",
            "expiresAt",
        ],
    )?;
    if proof["kind"] != "session.renew"
        || proof["nodeId"] != node
        || proof["nodeGeneration"] != row["generation"]
        || proof["authorityGeneration"] != generation(db)?
        || proof["origin"] != origin(hub)
    {
        return Err(deny());
    }
    fresh(db, &proof, &node)?;
    transaction(db, |db| {
        mint(
            db,
            &node,
            account,
            revision(&row["generation"])?,
            "session.renewed",
        )
    })
}
pub(super) async fn route(hub: Arc<Hub>, peer: IpAddr, request: Request) -> Result<Response> {
    let path = request.uri().path().to_owned();
    if path == "/node" {
        exact(&hub, &request).map_err(|_| missing())?;
        if request.method() != Method::GET {
            return Err(missing());
        }
        return upgrade(hub, peer, request).await;
    }
    let reply: Result<Value> = async {
        exact(&hub, &request)?;
        if request.method() != Method::POST {
            return Err(deny());
        }
        hub.limit(format!("node-http:{peer}"), 60, 60_000)?;
        let input = body(request, false).await?;
        let mut db = hub.lock()?;
        match path.as_str() {
            "/api/nodes/enrollment/start" => {
                transaction(&mut db, |db| start(&hub, db, peer, &input))
            }
            "/api/nodes/enrollment/poll" => poll(&hub, &mut db, &input),
            _ => renew(&hub, &mut db, &input),
        }
    }
    .await;
    let (status, response) = match reply {
        Ok(v) => (200, v),
        Err(e) => (e.status, json!({"kind":"error","code":e.code})),
    };
    Ok(crate::json_response(status, signed(&hub, &response)?))
}

struct Link {
    principal: Value,
    public: String,
    expires: i64,
    seen: i64,
    reconciled: bool,
    requests: HashMap<String, String>,
    last_grants: Option<String>,
    last_requests: Option<String>,
    delivered: HashMap<String, i64>,
}
async fn upgrade(hub: Arc<Hub>, peer: IpAddr, request: Request) -> Result<Response> {
    hub.limit(format!("upgrade:{peer}"), 10, 60_000)?;
    let permit = hub
        .sockets
        .clone()
        .try_acquire_owned()
        .map_err(|_| missing())?;
    let (mut parts, _) = request.into_parts();
    let ws = WebSocketUpgrade::from_request_parts(&mut parts, &())
        .await
        .map_err(|_| missing())?;
    let link = connect(&hub, &parts.headers)?;
    let failed_hub = hub.clone();
    let failed_principal = link.principal.clone();
    Ok(ws
        .max_message_size(LIMIT)
        .max_frame_size(LIMIT)
        .max_write_buffer_size(1_048_576)
        .on_failed_upgrade(move |_| {
            let _ = gone(&failed_hub, &failed_principal);
        })
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            run_socket(hub, socket, link).await;
        }))
}
fn connect(hub: &Hub, headers: &HeaderMap) -> Result<Link> {
    let token = single(headers, "authorization")?
        .and_then(|v| v.strip_prefix("Device "))
        .filter(|v| encoded(v, 32).is_some())
        .ok_or_else(deny)?;
    let proof = single(headers, "x-minarbot-proof")?
        .filter(|v| v.len() <= 8192)
        .ok_or_else(deny)?;
    let clock = now();
    let mut db = hub.lock()?;
    let row = one(&db,"SELECT s.*,n.signing_public_key,n.generation,n.connection_generation FROM node_link_sessions s JOIN nodes n ON n.id=s.node_id AND n.owner_account_id=s.account_id
        WHERE s.digest=? AND s.expires_at>? AND n.status!='revoked'",params![digest(token),clock])?.ok_or_else(deny)?;
    let node = text(&row, "node_id");
    let account = text(&row, "account_id");
    let authority = generation(&db)?;
    authority::account(&db, account)?;
    if row["node_generation"] != row["generation"] || row["authority_generation"] != authority {
        return Err(deny());
    }
    let public = text(&row, "signing_public_key");
    let proof = verify(&parse_json(proof.as_bytes())?, public)?;
    closed(
        &proof,
        &[
            "kind",
            "nodeId",
            "nodeGeneration",
            "authorityGeneration",
            "nonce",
            "issuedAt",
            "expiresAt",
            "sessionDigest",
            "method",
            "path",
            "origin",
        ],
    )?;
    if proof["kind"] != "connection.proof"
        || proof["nodeId"] != node
        || proof["nodeGeneration"] != row["generation"]
        || proof["authorityGeneration"] != authority
        || proof["sessionDigest"] != digest(token)
        || proof["method"] != "GET"
        || proof["path"] != "/node"
        || proof["origin"] != origin(hub)
    {
        return Err(deny());
    }
    fresh(&db, &proof, node)?;
    let connection = integer(&row["connection_generation"], 0).ok_or_else(Failure::storage)? + 1;
    transaction(&mut db, |db| {
        db.execute(
            "UPDATE nodes SET status='ready',connection_generation=?,last_seen=? WHERE id=?",
            params![connection, clock, node],
        )?;
        fence_turns(db, node)
    })?;
    Ok(Link {
        principal: json!({"nodeId":node,"accountId":account,"nodeGeneration":row["generation"],"connectionGeneration":connection}),
        public: public.to_owned(),
        expires: row["expires_at"].as_i64().ok_or_else(Failure::storage)?,
        seen: clock,
        reconciled: false,
        requests: Default::default(),
        last_grants: None,
        last_requests: None,
        delivered: Default::default(),
    })
}
fn fence_turns(db: &Connection, node: &str) -> Result<()> {
    db.execute(
        "UPDATE turns SET state='unknown' WHERE node_id=? AND state IN('pending','admitted')",
        [node],
    )?;
    db.execute(
        "DELETE FROM outbox WHERE node_id=? AND kind NOT IN('grant.revoked','turn.cancel')",
        [node],
    )?;
    Ok(())
}
fn gone(hub: &Hub, principal: &Value) -> Result<()> {
    let mut db = hub.lock()?;
    transaction(&mut db, |db| {
        let node = text(principal, "nodeId");
        if db.execute("UPDATE nodes SET status='offline' WHERE id=? AND generation=? AND connection_generation=? AND status='ready'",
            params![node,principal["nodeGeneration"].as_i64(),principal["connectionGeneration"].as_i64()])? == 1 {
            fence_turns(db,node)?;
        }
        Ok(())
    })
}
fn ready(hub: &Hub, link: &Link) -> Result<Value> {
    Ok(
        json!({"kind":"connection.ready","principal":link.principal,"authorityGeneration":generation(&*hub.lock()?)?,"expiresAt":link.expires}),
    )
}
async fn send(hub: &Hub, socket: &mut WebSocket, body: Value) -> Result<()> {
    let frame = canonical(&signed(hub, &body)?);
    if frame.len() > LIMIT {
        return Err(rate_limited());
    }
    tokio::time::timeout(
        Duration::from_secs(3),
        socket.send(Message::Text(frame.into())),
    )
    .await
    .map_err(|_| rate_limited())?
    .map_err(|_| deny())
}
async fn run_socket(hub: Arc<Hub>, mut socket: WebSocket, mut link: Link) {
    let output = async {
        send(&hub, &mut socket, ready(&hub, &link)?).await?;
        let mut timer = tokio::time::interval(Duration::from_millis(250));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = timer.tick() => {},
                packet = socket.recv() => match packet {
                    Some(Ok(Message::Text(packet))) => {
                        let output = receive(&hub,&mut link,&packet)?;
                        send(&hub,&mut socket,output).await?;
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    Some(Ok(Message::Binary(_))) => return Err(deny()),
                    _ => return Ok(()),
                }
            }
            for frame in pump(&hub, &mut link)? {
                send(&hub, &mut socket, frame).await?;
            }
        }
    }
    .await;
    let _ = gone(&hub, &link.principal);
    if let Err(error) = output {
        let code = match error.status {
            401 => 4401,
            409 => 4409,
            429 => 4429,
            _ => 4403,
        };
        let _ = tokio::time::timeout(
            Duration::from_secs(1),
            socket.send(Message::Close(Some(CloseFrame {
                code,
                reason: error.code.into(),
            }))),
        )
        .await;
    }
}
fn check_link(db: &Connection, link: &Link) -> Result<()> {
    authority::current_node(db, &link.principal)?;
    if now() >= link.expires || now() - link.seen >= 45_000 {
        return Err(Failure::new(401, "node_expired", "Node session expired."));
    }
    Ok(())
}
fn receive(hub: &Hub, link: &mut Link, packet: &str) -> Result<Value> {
    if packet.len() > LIMIT {
        return Err(deny());
    }
    let node = text(&link.principal, "nodeId");
    hub.limit(format!("frame:{node}"), 1200, 60_000)?;
    let input = verify(&parse_json(packet.as_bytes())?, &link.public)?;
    if input["principal"] != link.principal {
        return Err(deny());
    }
    let request = id(&input["requestId"])?;
    let digest = digest(&canonical(&input));
    if link.requests.get(&request).is_some_and(|v| *v != digest) {
        return Err(deny());
    }
    if link.requests.len() >= 4096 && !link.requests.contains_key(&request) {
        return Err(rate_limited());
    }
    let mut db = hub.lock()?;
    check_link(&db, link)?;
    link.seen = now();
    if text(&input, "kind") == "turn.result" && input["frame"]["kind"] == "turn.partial" {
        hub.limit(
            format!("partials:{node}:{}", text(&input["frame"], "turnId")),
            10,
            1_000,
        )?;
    }
    let result = transaction(&mut db, |db| command(db, link, &input));
    match result {
        Ok(result) => {
            if input["kind"] == "connection.reconcile" {
                link.reconciled = input["complete"] == true;
            }
            link.requests.insert(request.clone(), digest);
            Ok(
                json!({"kind":"response","requestId":request,"result":result,"principal":link.principal}),
            )
        }
        Err(e)
            if matches!(
                e.code,
                "authority_denied" | "resource_not_found" | "stale_revision" | "node_offline"
            ) =>
        {
            Ok(
                json!({"kind":"response","requestId":request,"error":{"status":e.status,"code":e.code},"principal":link.principal}),
            )
        }
        Err(e) => Err(e),
    }
}
fn command(db: &Connection, link: &Link, input: &Value) -> Result<Value> {
    let p = &link.principal;
    let kind = text(input, "kind");
    if kind != "connection.reconcile" && !link.reconciled {
        return Err(deny());
    }
    let extra: &[&str] = match kind {
        "connection.reconcile" => &["receipts", "withdrawals", "complete"],
        "heartbeat" | "node.revoke" => &[],
        "delivery.ack" => &["eventId", "frameDigest"],
        "turn.result" => &["frame"],
        "action.lease.request" => &["value"],
        "participation.consent" => &["projectId", "participationId", "revision", "policyDigest"],
        "participation.withdraw" => &["projectId", "participationId", "revision"],
        _ => return Err(deny()),
    };
    let mut fields = vec!["kind", "requestId", "principal"];
    fields.extend_from_slice(extra);
    closed(input, &fields)?;
    match kind {
        "connection.reconcile" => return reconcile(db, p, input),
        "heartbeat" => return Ok(json!({"expiresAt":link.expires})),
        "delivery.ack" => routing::ack(db, p, input)?,
        "turn.result" => return routing::accept(db, p, &input["frame"]),
        "action.lease.request" => return routing::lease(db, p, &input["value"]),
        "participation.consent" => {
            let project = id(&input["projectId"])?;
            let participation = id(&input["participationId"])?;
            let host = authority::context_host(db, &project)?;
            let context = authority::Context {
                project: &project,
                account: text(p, "accountId"),
                host: &host,
            };
            authority::consent(db, &context, input, Some(p))?;
            let grants = routing::grants(db, p)?
                .into_iter()
                .filter(|v| v["projectId"] == project && v["participationId"] == participation)
                .collect::<Vec<_>>();
            return Ok(json!({"grants":grants}));
        }
        "participation.withdraw" => authority::withdraw_node(db, p, input)?,
        "node.revoke" => authority::revoke_node(db, p)?,
        _ => return Err(deny()),
    }
    Ok(json!({}))
}
fn reconcile(db: &Connection, principal: &Value, input: &Value) -> Result<Value> {
    if !input["complete"].is_boolean() {
        return Err(deny());
    }
    for item in input["withdrawals"]
        .as_array()
        .filter(|a| a.len() <= 100)
        .ok_or_else(deny)?
    {
        closed(item, &["projectId", "participationId", "revision"])?;
        authority::withdraw_node(db, principal, item)?;
    }
    let mut outcomes = Vec::new();
    for item in input["receipts"]
        .as_array()
        .filter(|a| a.len() <= 100)
        .ok_or_else(deny)?
    {
        closed(item, &["projectId", "turnId", "sequence", "digest"])?;
        let project = id(&item["projectId"])?;
        let turn = id(&item["turnId"])?;
        let digest_value = text(item, "digest");
        if !hex(digest_value, 64) || integer(&item["sequence"], 0).is_none() {
            return Err(deny());
        }
        let row = one(db,"SELECT state,result_sequence,request_json FROM turns WHERE project_id=? AND id=? AND node_id=?", [project.as_str(),turn.as_str(),text(principal,"nodeId")])?;
        if row
            .as_ref()
            .is_some_and(|v| digest(text(v, "request_json")) != digest_value)
        {
            return Err(deny());
        }
        let (state, sequence) = row
            .as_ref()
            .map(|v| (text(v, "state"), v["result_sequence"].as_i64().unwrap_or(0)))
            .unwrap_or(("unknown", 0));
        outcomes.push(json!({"projectId":project,"turnId":turn,"state":state,"sequence":sequence}));
    }
    Ok(json!({"outcomes":outcomes}))
}
fn proposals(db: &Connection, principal: &Value) -> Result<Vec<Value>> {
    let account = text(principal, "accountId");
    let mut requests = Vec::new();
    for r in rows(db,"SELECT p.id,p.project_id,p.agent_id,p.revision,p.policy_json,pr.name AS projectName,h.host,ac.subject AS requesterName,q.requester_account_id,q.provider_max_cost_micros
        FROM participations p JOIN agents ag ON ag.id=p.agent_id JOIN projects pr ON pr.id=p.project_id
        JOIN host_bindings h ON h.project_id=p.project_id AND h.active=1 JOIN hub_web_requests q ON q.participation_id=p.id AND q.revision=p.revision
        JOIN accounts ac ON ac.id=q.requester_account_id WHERE ag.home_node_id=? AND ag.owner_account_id=? AND p.state='proposed'
        AND p.owner_consent_revision IS NOT p.revision LIMIT 100", [text(principal,"nodeId"),account])? {
        let ctx = authority::Context { project: text(&r,"project_id"), account, host: text(&r,"host") };
        match authority::member(db,&ctx,authority::WRITE) {
            Ok(_) => {},
            Err(error) if error.status < 500 => continue,
            Err(error) => return Err(error),
        }
        let policy = stored(&r,"policy_json")?;
        let sections = rows(db,"SELECT s.id,s.name FROM participation_sections p JOIN sections s ON s.project_id=p.project_id AND s.id=p.section_id
            WHERE p.project_id=? AND p.participation_id=? AND s.archived=0 ORDER BY s.id",[ctx.project,text(&r,"id")])?;
        if policy["sectionIds"].as_array().is_none_or(|v| v.len() != sections.len()) { continue; }
        requests.push(json!({"projectName":r["projectName"],"projectHost":ctx.host,"sections":sections,"requesterName":r["requesterName"],"requesterAccountId":r["requester_account_id"],
            "grant":{"projectId":ctx.project,"participationId":r["id"],"agentId":r["agent_id"],"localAgentId":r["agent_id"],"revision":r["revision"],"policy":policy,"providerMaxCostMicros":r["provider_max_cost_micros"]}}));
    }
    Ok(requests)
}
fn pump(hub: &Hub, link: &mut Link) -> Result<Vec<Value>> {
    let db = hub.lock()?;
    check_link(&db, link)?;
    if !link.reconciled {
        return Ok(Vec::new());
    }
    let p = &link.principal;
    let current = now();
    link.delivered.retain(|_, seen| *seen + 60_000 >= current);
    let mut controls = Vec::new();
    let mut data = Vec::new();
    let mut waiting = false;
    for frame in routing::pending(&db, p)? {
        let control = matches!(text(&frame, "kind"), "grant.revoked" | "turn.cancel");
        if frame["kind"] == "grant.revoked" {
            let prior = one(
                &db,
                "SELECT state,revision FROM participations WHERE project_id=? AND id=?",
                [text(&frame, "projectId"), text(&frame, "participationId")],
            )?;
            if prior.is_some_and(|v| {
                v["state"] == "active" && v["revision"].as_i64() >= frame["revision"].as_i64()
            }) {
                routing::ack(
                    &db,
                    p,
                    &json!({"eventId":frame["eventId"],"frameDigest":digest(&canonical(&frame))}),
                )?;
                continue;
            }
        }
        waiting |= control;
        let event = text(&frame, "eventId");
        if link
            .delivered
            .get(event)
            .is_some_and(|v| *v + 1_000 > current)
        {
            continue;
        }
        if control {
            link.delivered.insert(event.to_owned(), current);
            controls.push(frame);
        } else {
            data.push(frame);
        }
    }
    if waiting {
        return Ok(controls);
    }
    let generation = generation(&db)?;
    let requests = proposals(&db, p)?;
    let snapshot = digest(&canonical(&json!(requests)));
    let mut output = Vec::new();
    if link.last_requests.as_ref() != Some(&snapshot) {
        let profile = one(
            &db,
            "SELECT name FROM node_link_profiles WHERE account_id=?",
            [text(p, "accountId")],
        )?;
        let account = authority::account(&db, text(p, "accountId"))?;
        let name = profile
            .as_ref()
            .map(|v| text(v, "name"))
            .unwrap_or_else(|| text(&account, "subject"));
        let mut offset = 0;
        let mut batch = Vec::new();
        let mut size = 2;
        for request in requests {
            let length = canonical(&request).len() + 1;
            if !batch.is_empty() && size + length > LIMIT - 4096 {
                output.push(json!({"kind":"participation.requests","requests":batch,"snapshot":snapshot,"offset":offset,"complete":false,"accountName":name,"principal":p,"authorityGeneration":generation}));
                offset += batch.len();
                batch.clear();
                size = 2;
            }
            size += length;
            batch.push(request);
        }
        output.push(json!({"kind":"participation.requests","requests":batch,"snapshot":snapshot,"offset":offset,"complete":true,"accountName":name,"principal":p,"authorityGeneration":generation}));
        link.last_requests = Some(snapshot);
    }
    let grants = routing::grants(&db, p)?;
    let current_grants = digest(&canonical(&json!(grants)));
    if link.last_grants.as_ref() != Some(&current_grants) {
        link.last_grants = Some(current_grants);
        output.push(
            json!({"kind":"grants","grants":grants,"principal":p,"authorityGeneration":generation}),
        );
    }
    for frame in data {
        link.delivered
            .insert(text(&frame, "eventId").to_owned(), current);
        output.push(frame);
    }
    Ok(output)
}
