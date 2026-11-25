use super::{hub_config::Options, machines};
use crate::{
    files::{self, one, rows, text},
    web::Failure,
    AppState,
};
use axum::{
    extract::{Request, State},
    http::Method,
    response::Response,
    routing::any,
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};

type Result<T> = std::result::Result<T, Failure>;
#[derive(Default)]
pub struct Hub {
    current: Mutex<Current>,
    stopped: AtomicBool,
    calls: Mutex<HashMap<String, oneshot::Sender<Result<Value>>>>,
    sender: Mutex<Option<mpsc::Sender<Outgoing>>>,
    keys: Mutex<HashMap<String, Vec<u8>>>,
    snapshots: Mutex<()>,
}
#[derive(Default)]
struct Current {
    principal: Option<Value>,
    reconciled: bool,
    session: Option<Value>,
    pending: Option<Value>,
    grants: Vec<Value>,
    now: i64,
}
enum Outgoing {
    Frame(String),
    Close,
}
fn denied() -> Failure {
    Failure::new(
        403,
        "local_policy_denied",
        "Local owner policy does not allow this work.",
    )
}
fn unavailable() -> Failure {
    Failure::new(409, "hub_link_unavailable", "Member node is not connected.")
}
fn hash(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn canonical(v: &Value) -> String {
    fn sort(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut keys = m.keys().collect::<Vec<_>>();
                keys.sort();
                Value::Object(keys.into_iter().map(|k| (k.clone(), sort(&m[k]))).collect())
            }
            Value::Array(a) => Value::Array(a.iter().map(sort).collect()),
            _ => v.clone(),
        }
    }
    sort(v).to_string()
}
fn db<T>(state: &AppState, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    f(&guard.connection)
}
fn identity(state: &AppState) -> Result<Option<Value>> {
    db(state, |db| {
        one(db, "SELECT * FROM hub_link_identity WHERE id=1", [])
    })
}
fn options(state: &AppState) -> Result<&Options> {
    state.config.hub.hub_link.as_ref().ok_or_else(denied)
}
fn uuid(value: &Value) -> Result<&str> {
    value
        .as_str()
        .filter(|s| crate::web::id(s).is_some())
        .ok_or_else(denied)
}
fn random() -> Result<String> {
    let mut b = [0; 32];
    getrandom::getrandom(&mut b).map_err(|_| Failure::storage())?;
    Ok(URL_SAFE_NO_PAD.encode(b))
}
impl Hub {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Current>> {
        self.current.lock().map_err(|_| Failure::storage())
    }
    pub fn initialize(db: &Connection) -> crate::db::Result<()> {
        db.execute_batch(include_str!("hub.sql"))?;
        db.execute_batch("UPDATE turns SET status='interrupted',paused=0,error='Hub authority ended. Check prior effects before retrying.' WHERE id IN(SELECT turn_id FROM hub_link_turns WHERE state='admitted') AND status IN('queued','running'); UPDATE hub_link_turns SET state='unknown' WHERE state='admitted'; UPDATE hub_link_actions SET state='unknown' WHERE state IN('executing','intent','approval');")?;
        Ok(())
    }
}
pub fn initialize(db: &Connection) -> crate::db::Result<()> {
    Hub::initialize(db)
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/hub-link", any(entry))
        .route("/api/hub-link/history", any(entry))
        .route("/api/hub-link/requests", any(entry))
        .route("/api/hub-link/grants", any(entry))
        .route("/api/hub-link/agents/{id}/policy", any(entry))
        .route("/api/hub-link/enrollment/start", any(entry))
        .route("/api/hub-link/enrollment/poll", any(entry))
        .route("/api/hub-link/grants/approve", any(entry))
        .route("/api/hub-link/grants/revoke", any(entry))
        .route("/api/hub-link/approvals", any(entry))
        .route("/api/hub-link/revoke", any(entry))
}
async fn entry(State(state): State<AppState>, request: Request) -> Response {
    let mut response = handle(&state, request)
        .await
        .unwrap_or_else(|e| crate::local_error(e.status, &e.message));
    crate::security_headers(&mut response);
    response
}
pub async fn handle(state: &AppState, request: Request) -> Result<Response> {
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    let host = crate::text_header(request.headers(), "host").unwrap_or("");
    if options(state).is_err()
        || state.auth.is_some()
        || format!("http://{host}") != state.origin
        || request.headers().contains_key("authorization")
        || request.uri().query().is_some()
        || crate::text_header(request.headers(), "sec-fetch-site") == Some("cross-site")
        || request.headers().contains_key("origin")
            && crate::text_header(request.headers(), "origin") != Some(state.origin.as_str())
        || method != Method::GET
            && crate::text_header(request.headers(), "origin") != Some(state.origin.as_str())
    {
        return Err(Failure::new(
            403,
            "invalid_request",
            "Hub enrollment requires the local owner in Minarbot.",
        ));
    }
    if crate::updates::pending(state) {
        return Err(Failure::new(
            503,
            "invalid_request",
            "Update is being verified. Retry after restart.",
        ));
    }
    let hub = &state.computer.hub;
    let result = if method == Method::GET {
        match path.as_str() {
            "/api/hub-link" => {
                let mut v = status(state)?;
                v["origin"] = json!(options(state)?.origin);
                v["approvals"] = approvals(state)?;
                v
            }
            "/api/hub-link/requests" => db(state, |db| {
                Ok(json!(rows(
                    db,
                    "SELECT value FROM hub_link_requests LIMIT 100",
                    []
                )?
                .iter()
                .map(|v| serde_json::from_str::<Value>(text(v, "value"))
                    .map_err(|_| Failure::storage()))
                .collect::<Result<Vec<_>>>()?))
            })?,
            "/api/hub-link/grants" => db(state, |db| {
                Ok(json!(rows(
                    db,
                    "SELECT value,enabled FROM hub_link_grants LIMIT 100",
                    []
                )?
                .into_iter()
                .map(|r| {
                    let mut v: Value =
                        serde_json::from_str(text(&r, "value")).map_err(|_| Failure::storage())?;
                    v["enabled"] = json!(r["enabled"] == 1);
                    Ok(v)
                })
                .collect::<Result<Vec<_>>>()?))
            })?,
            "/api/hub-link/history" => history(state)?,
            _ => {
                let agent = path
                    .strip_prefix("/api/hub-link/agents/")
                    .and_then(|p| p.strip_suffix("/policy"))
                    .ok_or_else(denied)?;
                json!({"localPolicyDigest":policy_digest(state,agent)?})
            }
        }
    } else if method == Method::POST {
        if crate::text_header(request.headers(), "content-type").and_then(|v| v.split(';').next())
            != Some("application/json")
        {
            return Err(Failure::new(
                415,
                "invalid_request",
                "Use Content-Type application/json.",
            ));
        }
        let test = request
            .headers()
            .get("x-minarbot-test-hub")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| {
                state
                    .config
                    .computer
                    .test_computer_bridge
                    .as_ref()
                    .is_some_and(|b| b.token == t)
            });
        let value = files::body(request, 65536).await?;
        if test && path == "/api/hub-link" {
            match text(&value, "operation") {
                "close" => {
                    stop(state).await?;
                    json!({})
                }
                "beforeEffect" => {
                    let v = &value["value"];
                    let effect = before_effect(
                        state,
                        text(v, "turn"),
                        text(v, "kind"),
                        v["payload"].clone(),
                    )
                    .await;
                    if let Err(e) = effect {
                        return Ok(crate::json_response(
                            e.status,
                            json!({"code":e.code,"error":e.message}),
                        ));
                    }
                    json!({})
                }
                _ => return Err(denied()),
            }
        } else {
            match path.as_str() {
                "/api/hub-link/enrollment/start" => start_enrollment(state).await?,
                "/api/hub-link/enrollment/poll" => poll_enrollment(state).await?,
                "/api/hub-link/grants/approve" => approve_grant(state, &value).await?,
                "/api/hub-link/grants/revoke" => {
                    if !files::keys(&value, &["projectId", "participationId"]) {
                        return Err(denied());
                    }
                    withdraw(
                        state,
                        uuid(&value["projectId"])?,
                        uuid(&value["participationId"])?,
                    )
                    .await?
                }
                "/api/hub-link/approvals" => approve_action(state, &value)?,
                "/api/hub-link/revoke" => {
                    db(state, |db| {
                        db.execute("UPDATE hub_link_identity SET disabled=1 WHERE id=1", [])?;
                        Ok(())
                    })?;
                    if hub.lock()?.principal.is_some() {
                        let _ = rpc(state, "node.revoke", json!({})).await;
                    }
                    disconnect(state)?;
                    json!({"revoked":true})
                }
                _ => {
                    return Err(Failure::new(
                        404,
                        "invalid_request",
                        "Hub link route not found.",
                    ))
                }
            }
        }
    } else {
        return Err(Failure::new(405, "invalid_request", "Method not allowed."));
    };
    snapshot(state)?;
    Ok(crate::json_response(200, result))
}
pub fn policy_digest(state: &AppState, agent: &str) -> Result<String> {
    db(state, |db| {
        let profile = one(db, "SELECT * FROM agents WHERE id=?", [agent])?
            .ok_or_else(|| Failure::new(404, "invalid_request", "Agent not found."))?;
        let account = crate::turns::hub_account(state, db, agent)?;
        Ok(hash(json!({"profile":profile,"provider":{"id":account.id,"provider":account.family,"baseURL":account.base,"maxTokens":account.max_tokens}}).to_string()))
    })
}
fn status(state: &AppState) -> Result<Value> {
    let i = identity(state)?.unwrap_or(Value::Null);
    let hub = state.computer.hub.lock()?;
    let mut v = json!({"enrolled":i["account_id"].is_string(),"connected":hub.principal.is_some()&&hub.reconciled,"supportedRuntimes":["api"],"unsupportedRuntimes":{"codex":"native_effect_fencing_unavailable","claude":"native_effect_fencing_unavailable","hermes":"native_effect_fencing_unavailable","antigravity":"native_effect_fencing_unavailable","grok":"native_effect_fencing_unavailable","pi":"native_effect_fencing_unavailable","opencode":"native_effect_fencing_unavailable","gemini":"native_effect_fencing_unavailable","kiro":"native_effect_fencing_unavailable"},"pendingWithdrawals":db(state,|db|Ok(db.query_row("SELECT count(*) FROM hub_link_withdrawals",[],|r|r.get::<_,i64>(0))?))?,"accountName":db(state,|db|Ok(one(db,"SELECT name FROM hub_link_profile WHERE id=1",[])?.map(|v|v["name"].clone())))?,"nodeId":i["node_id"],"accountId":i["account_id"],"disabled":i["disabled"]==1});
    if let Some(keys) = state
        .computer
        .hub
        .keys
        .lock()
        .map_err(|_| Failure::storage())?
        .get("node-signing")
    {
        v["fingerprint"] = json!(hash(public(keys)?));
    }
    if let Some(p) = &hub.pending {
        for key in ["humanCode", "expiresAt", "intervalMs", "verificationUri"] {
            v[key] = p[key].clone();
        }
    }
    Ok(v)
}
fn history(state: &AppState) -> Result<Value> {
    db(state, |db| {
        let rs=rows(db,"SELECT t.turn_id,t.frame,t.state,l.thread,a.name,turns.text,turns.reply FROM hub_link_turns t JOIN hub_link_threads l ON l.project_id=json_extract(t.frame,'$.projectId') AND l.section_id=json_extract(t.frame,'$.sectionId') JOIN agents a ON a.id=l.agent_id JOIN turns ON turns.id=t.turn_id AND turns.agent=a.id ORDER BY turns.rowid DESC LIMIT 100",[])?;
        Ok(json!(rs.into_iter().map(|r|{let f:Value=serde_json::from_str(text(&r,"frame")).map_err(|_|Failure::storage())?;Ok(json!({"turnId":r["turn_id"],"threadId":r["thread"],"agentName":r["name"],"projectId":f["projectId"],"sectionId":f["sectionId"],"requesterAccountId":f["requesterAccountId"],"payerAccountId":f["payerAccountId"],"state":r["state"],"text":r.get("reply").filter(|v|!v.is_null()).unwrap_or(&r["text"])}))}).collect::<Result<Vec<_>>>()?))
    })
}
fn approvals(state: &AppState) -> Result<Value> {
    db(state, |db| {
        Ok(json!(rows(db,"SELECT a.action_id,a.turn_id,a.digest,a.kind,a.payload,t.frame FROM hub_link_actions a JOIN hub_link_turns t ON t.turn_id=a.turn_id WHERE a.state='approval' AND t.state='admitted'",[])?.into_iter().map(|r|{let f:Value=serde_json::from_str(text(&r,"frame")).map_err(|_|Failure::storage())?;let payload:Value=serde_json::from_str(text(&r,"payload")).map_err(|_|Failure::storage())?;Ok(json!({"actionId":r["action_id"],"turnId":r["turn_id"],"actionDigest":r["digest"],"kind":r["kind"],"payload":payload,"projectId":f["projectId"],"sectionId":f["sectionId"],"participationId":f["participationId"],"agentId":f["agentId"],"requesterAccountId":f["requesterAccountId"],"requiredApproverAccountId":f["payerAccountId"],"expiresAt":f["expiresAt"]}))}).collect::<Result<Vec<_>>>()?))
    })
}
fn snapshot(state: &AppState) -> Result<()> {
    let _snapshot = state
        .computer
        .hub
        .snapshots
        .lock()
        .map_err(|_| Failure::storage())?;
    let status = status(state)?;
    let approvals = approvals(state)?;
    let agents = db(state, |db| rows(db, "SELECT id FROM agents", []))?;
    let mut policies = json!({});
    for a in agents {
        let agent = text(&a, "id");
        if let Ok(d) = policy_digest(state, agent) {
            policies[agent] = json!(d);
        }
    }
    db(state, |db| {
        db.execute(
            "INSERT OR REPLACE INTO hub_link_runtime VALUES(1,?)",
            [json!({"status":status,"approvals":approvals,"policies":policies}).to_string()],
        )?;
        Ok(())
    })
}
async fn clock(state: &AppState) -> Result<i64> {
    let now = if state.config.computer.test_computer_bridge.is_some() {
        machines::bridge(state, "hub-clock", json!({}))
            .await?
            .as_i64()
            .ok_or_else(Failure::storage)?
    } else {
        state.config.now()
    };
    state.computer.hub.lock()?.now = now;
    Ok(now)
}
fn public(key: &[u8]) -> Result<String> {
    let key = Ed25519KeyPair::from_pkcs8(key).map_err(|_| denied())?;
    let mut bytes = vec![
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0,
    ];
    bytes.extend_from_slice(key.public_key().as_ref());
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
async fn key(state: &AppState, name: &str, create: bool) -> Result<Vec<u8>> {
    if let Some(k) = state
        .computer
        .hub
        .keys
        .lock()
        .map_err(|_| Failure::storage())?
        .get(name)
        .cloned()
    {
        return Ok(k);
    }
    let identity = identity(state)?.ok_or_else(unavailable)?;
    let service = format!(
        "minarbot/installation/{}",
        text(&identity, "installation_id")
    );
    let read = || -> Result<Option<String>> {
        if state.config.harness.test_mode {
            return Err(Failure::new(
                503,
                "keystore_unavailable",
                "Node key is unavailable.",
            ));
        }
        crate::system_keyring::read(&service, name).map_err(|_| denied())
    };
    let mut saved = if state.config.computer.test_computer_bridge.is_some() {
        machines::bridge(state, "hub-key-get", json!({"service":service,"name":name}))
            .await?
            .as_str()
            .map(str::to_owned)
    } else {
        read()?
    };
    if saved.is_none() && create {
        let raw = if name == "node-encryption" {
            let output = tokio::process::Command::new("openssl")
                .args(["genpkey", "-algorithm", "X25519", "-outform", "DER"])
                .kill_on_drop(true)
                .output()
                .await
                .map_err(|_| denied())?;
            if !output.status.success() {
                return Err(denied());
            }
            output.stdout
        } else {
            Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
                .map_err(|_| denied())?
                .as_ref()
                .to_vec()
        };
        let encoded = URL_SAFE_NO_PAD.encode(&raw);
        if state.config.computer.test_computer_bridge.is_some() {
            machines::bridge(
                state,
                "hub-key-set",
                json!({"service":service,"name":name,"value":encoded}),
            )
            .await?;
            saved = machines::bridge(state, "hub-key-get", json!({"service":service,"name":name}))
                .await?
                .as_str()
                .map(str::to_owned);
        } else {
            crate::system_keyring::write(&service, name, &encoded).map_err(|_| denied())?;
            saved = read()?;
        }
        if saved.as_ref() != Some(&encoded) {
            return Err(denied());
        }
    }
    let raw = URL_SAFE_NO_PAD
        .decode(saved.ok_or_else(denied)?)
        .map_err(|_| denied())?;
    if name != "node-encryption" {
        public(&raw)?;
    }
    state
        .computer
        .hub
        .keys
        .lock()
        .map_err(|_| Failure::storage())?
        .insert(name.into(), raw.clone());
    Ok(raw)
}
fn sign(state: &AppState, body: Value, name: &str) -> Result<Value> {
    let keys = state
        .computer
        .hub
        .keys
        .lock()
        .map_err(|_| Failure::storage())?;
    let raw = keys.get(name).ok_or_else(denied)?;
    let pair = Ed25519KeyPair::from_pkcs8(raw).map_err(|_| denied())?;
    let encoded = URL_SAFE_NO_PAD.decode(public(raw)?).map_err(|_| denied())?;
    let signature = pair.sign(format!("minarbot.node.v1\n{}", canonical(&body)).as_bytes());
    Ok(
        json!({"v":1,"keyId":hash(encoded),"body":body,"signature":URL_SAFE_NO_PAD.encode(signature.as_ref())}),
    )
}
fn verify_hub_envelope(public_key: &str, bytes: &[u8]) -> Result<Value> {
    if bytes.len() > 65536 {
        return Err(denied());
    }
    let envelope = crate::web::parse_json(bytes).map_err(|_| denied())?;
    if !files::keys(&envelope, &["v", "keyId", "body", "signature"])
        || envelope["v"] != 1
        || !envelope["body"].is_object()
    {
        return Err(denied());
    }
    let raw = super::devices::public_key(&json!(public_key), "ed25519").map_err(|_| denied())?;
    if envelope["keyId"] != hash(&raw) {
        return Err(denied());
    }
    let signature = super::devices::decode(&envelope["signature"], 64).map_err(|_| denied())?;
    UnparsedPublicKey::new(&ED25519, &raw[12..])
        .verify(
            format!("minarbot.hub.v1\n{}", canonical(&envelope["body"])).as_bytes(),
            &signature,
        )
        .map_err(|_| denied())?;
    Ok(envelope["body"].clone())
}
async fn proof(state: &AppState, mut body: Value) -> Result<Value> {
    let now = clock(state).await?;
    body["origin"] = json!(options(state)?.origin);
    body["nonce"] = json!(random()?);
    body["issuedAt"] = json!(now);
    body["expiresAt"] = json!(now + 60000);
    sign(state, body, "node-signing")
}
async fn http(state: &AppState, path: &str, value: Value) -> Result<Value> {
    let (status, body) = if state.config.computer.test_computer_bridge.is_some() {
        let r = machines::bridge(
            state,
            "hub-http",
            json!({"path":path,"body":canonical(&value)}),
        )
        .await?;
        (
            r["status"].as_u64().unwrap_or(503) as u16,
            text(&r, "body").as_bytes().to_vec(),
        )
    } else {
        let mut response = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| unavailable())?
            .post(format!("{}{path}", options(state)?.origin))
            .header("content-type", "application/json")
            .body(canonical(&value))
            .send()
            .await
            .map_err(|_| unavailable())?;
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
            if bytes.len() + chunk.len() > 65536 {
                return Err(denied());
            }
            bytes.extend_from_slice(&chunk);
        }
        (status, bytes)
    };
    let body = verify_hub_envelope(&options(state)?.hub_signing_public_key, &body)?;
    if !(200..300).contains(&status) {
        return Err(Failure::new(
            status,
            "hub_request_failed",
            "Hub request refused.",
        ));
    }
    Ok(body)
}
async fn start_enrollment(state: &AppState) -> Result<Value> {
    let i = identity(state)?;
    if i.as_ref()
        .is_some_and(|i| i["account_id"].is_string() || i["disabled"] == 1)
    {
        return Err(denied());
    }
    if i.is_none() {
        db(state, |db| {
            if one(
                db,
                "SELECT name FROM sqlite_schema WHERE type='table' AND name='device_state'",
                [],
            )?
            .is_some()
                && one(db, "SELECT device_id FROM device_state WHERE id=1", [])?.is_some()
            {
                return Err(Failure::new(
                    409,
                    "canonical_identity_required",
                    "Supply the existing device runtime hubIdentity.",
                ));
            }
            let environment = one(
                db,
                "SELECT environment_id FROM environment_identity WHERE singleton=1",
                [],
            )?
            .ok_or_else(Failure::storage)?;
            db.execute("INSERT INTO hub_link_identity(id,installation_id,node_id,origin,hub_key) VALUES(1,?,?,?,?)",params![uuid::Uuid::new_v4().to_string(),text(&environment,"environment_id"),options(state)?.origin,options(state)?.hub_signing_public_key])?;
            Ok(())
        })?;
    }
    let installation = key(state, "installation", true).await?;
    let signing = key(state, "node-signing", true).await?;
    let encryption = key(state, "node-encryption", true).await?;
    use tokio::io::AsyncWriteExt;
    let mut child = tokio::process::Command::new("openssl")
        .args(["pkey", "-inform", "DER", "-pubout", "-outform", "DER"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| denied())?;
    child
        .stdin
        .take()
        .ok_or_else(denied)?
        .write_all(&encryption)
        .await
        .map_err(|_| denied())?;
    let out = child.wait_with_output().await.map_err(|_| denied())?;
    if !out.status.success() {
        return Err(denied());
    }
    let i = identity(state)?.ok_or_else(denied)?;
    let candidate = json!({"nodeId":i["node_id"],"installationId":i["installation_id"],"installationPublicKey":public(&installation)?,"signingPublicKey":public(&signing)?,"encryptionPublicKey":URL_SAFE_NO_PAD.encode(out.stdout),"label":"Minarbot member node","nonce":random()?,"capabilities":["chat"]});
    let body =
        json!({"kind":"enrollment.start","candidate":candidate,"origin":options(state)?.origin});
    let result=http(state,"/api/nodes/enrollment/start",json!({"candidate":candidate,"installationProof":sign(state,body.clone(),"installation")?,"nodeProof":sign(state,body,"node-signing")?})).await?;
    uuid(&result["authorityGeneration"])?;
    db(state, |db| {
        db.execute(
            "UPDATE hub_link_identity SET authority_generation=?,node_generation=1 WHERE id=1",
            [text(&result, "authorityGeneration")],
        )?;
        Ok(())
    })?;
    state.computer.hub.lock()?.pending = Some(result);
    status(state)
}
async fn remember(state: &AppState, value: Value) -> Result<()> {
    let now = clock(state).await?;
    let i = identity(state)?.ok_or_else(denied)?;
    uuid(&value["accountId"])?;
    uuid(&value["authorityGeneration"])?;
    if value["nodeId"] != i["node_id"]
        || i["account_id"].is_string() && i["account_id"] != value["accountId"]
        || value["nodeGeneration"].as_u64().is_none_or(|v| v < 1)
        || value["expiresAt"]
            .as_i64()
            .is_none_or(|t| t <= now || t > now + 900000)
        || URL_SAFE_NO_PAD
            .decode(text(&value, "token"))
            .map_or(true, |v| v.len() != 32)
    {
        return Err(denied());
    }
    db(state, |db| {
        db.execute("UPDATE hub_link_identity SET account_id=?,node_generation=?,authority_generation=? WHERE id=1",params![text(&value,"accountId"),value["nodeGeneration"].as_i64(),text(&value,"authorityGeneration")])?;
        Ok(())
    })?;
    state.computer.hub.lock()?.session = Some(value);
    Ok(())
}
async fn renew(state: &AppState) -> Result<()> {
    let i = identity(state)?.ok_or_else(denied)?;
    let p=proof(state,json!({"kind":"session.renew","nodeId":i["node_id"],"nodeGeneration":i["node_generation"],"authorityGeneration":i["authority_generation"]})).await?;
    remember(
        state,
        http(
            state,
            "/api/nodes/session",
            json!({"nodeId":i["node_id"],"proof":p}),
        )
        .await?,
    )
    .await?;
    state.computer.hub.lock()?.pending = None;
    Ok(())
}
async fn poll_enrollment(state: &AppState) -> Result<Value> {
    let pending = state.computer.hub.lock()?.pending.clone();
    let now = clock(state).await?;
    if let Some(p) = pending.filter(|p| p["expiresAt"].as_i64().is_some_and(|t| t > now)) {
        let proof=proof(state,json!({"kind":"enrollment.poll","deviceCodeDigest":hash(text(&p,"deviceCode")),"candidateDigest":p["candidateDigest"]})).await?;
        match http(
            state,
            "/api/nodes/enrollment/poll",
            json!({"deviceCode":p["deviceCode"],"proof":proof}),
        )
        .await
        {
            Ok(result) => {
                if result["kind"] == "enrollment.complete" {
                    remember(state, result).await?;
                    state.computer.hub.lock()?.pending = None;
                }
            }
            Err(e) => {
                if renew(state).await.is_err() {
                    return Err(e);
                }
            }
        }
    } else {
        renew(state).await?;
    }
    snapshot(state)?;
    status(state)
}
async fn send(state: &AppState, body: Value) -> Result<()> {
    let envelope = canonical(&sign(state, body, "node-signing")?);
    if envelope.len() > 65536 {
        return Err(denied());
    }
    let tx = state
        .computer
        .hub
        .sender
        .lock()
        .map_err(|_| Failure::storage())?
        .clone()
        .ok_or_else(unavailable)?;
    tx.try_send(Outgoing::Frame(envelope))
        .map_err(|_| unavailable())
}
async fn rpc(state: &AppState, kind: &str, body: Value) -> Result<Value> {
    let principal = state
        .computer
        .hub
        .lock()?
        .principal
        .clone()
        .ok_or_else(unavailable)?;
    let request = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = oneshot::channel();
    state
        .computer
        .hub
        .calls
        .lock()
        .map_err(|_| Failure::storage())?
        .insert(request.clone(), tx);
    let mut frame = json!({"kind":kind,"requestId":request,"principal":principal});
    if let Some(m) = body.as_object() {
        for (k, v) in m {
            frame[k] = v.clone();
        }
    }
    let result = async {
        send(state, frame).await?;
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())?
    }
    .await;
    state
        .computer
        .hub
        .calls
        .lock()
        .map_err(|_| Failure::storage())?
        .remove(&request);
    result
}
fn local_grant(state: &AppState, frame: &Value) -> Result<Value> {
    db(state, |db| {
        let r=one(db,"SELECT value FROM hub_link_grants WHERE project_id=? AND participation_id=? AND enabled=1",params![text(frame,"projectId"),text(frame,"participationId")])?.ok_or_else(denied)?;
        serde_json::from_str(text(&r, "value")).map_err(|_| Failure::storage())
    })
}
fn authorize(state: &AppState, frame: &Value, invoke: bool) -> Result<Value> {
    let i = identity(state)?.ok_or_else(denied)?;
    let local = local_grant(state, frame)?;
    let digest = policy_digest(state, text(&local, "localAgentId"))?;
    let h = state.computer.hub.lock()?;
    let principal = h.principal.as_ref().ok_or_else(denied)?;
    if !h.reconciled
        || frame["nodeId"] != i["node_id"]
        || frame["payerAccountId"] != i["account_id"]
        || frame["expiresAt"].as_i64().is_none_or(|t| t <= h.now)
        || frame["payloadDigest"] != hash(canonical(&frame["payload"]))
        || local["policy"]["localPolicyDigest"] != digest
    {
        return Err(denied());
    }
    let grant = h
        .grants
        .iter()
        .find(|g| {
            g["projectId"] == frame["projectId"]
                && g["sectionId"] == frame["sectionId"]
                && g["participationId"] == frame["participationId"]
        })
        .ok_or_else(denied)?;
    let policy = &local["policy"];
    let fence = &frame["fence"];
    if frame["agentId"] != local["agentId"]
        || grant["agentId"] != local["agentId"]
        || grant["revision"] != local["revision"]
        || canonical(&grant["policy"]) != canonical(policy)
        || !policy["sectionIds"]
            .as_array()
            .is_some_and(|v| v.contains(&frame["sectionId"]))
        || !policy["scopes"]
            .as_array()
            .is_some_and(|v| v.contains(&json!("read")))
        || frame["payerBindingRef"] != policy["payerBindingRef"]
        || fence["authorityGeneration"] != grant["authorityGeneration"]
        || fence["participationRevision"] != grant["revision"]
        || fence["ownerConsentRevision"] != grant["ownerConsentRevision"]
        || fence["projectConsentRevision"] != grant["projectConsentRevision"]
        || fence["ownerMembershipRevision"] != grant["ownerMembershipRevision"]
        || fence["nodeGeneration"] != principal["nodeGeneration"]
        || fence["connectionGeneration"] != principal["connectionGeneration"]
    {
        return Err(denied());
    }
    if invoke
        && (!policy["scopes"]
            .as_array()
            .is_some_and(|v| v.contains(&json!("invoke")))
            || frame["requesterAccountId"] != i["account_id"]
                && policy["invocation"]["mode"] != "all-members"
                && !(policy["invocation"]["mode"] == "members"
                    && policy["invocation"]["accountIds"]
                        .as_array()
                        .is_some_and(|v| v.contains(&frame["requesterAccountId"])))
            || frame["payload"]["maxCostMicros"] != policy["maxCostMicros"])
    {
        return Err(denied());
    }
    Ok(local)
}
pub fn check(state: &AppState, turn: &str) -> Result<Option<Value>> {
    let row = db(state, |db| {
        one(
            db,
            "SELECT frame,state FROM hub_link_turns WHERE turn_id=?",
            [turn],
        )
    })?;
    let Some(row) = row else { return Ok(None) };
    if row["state"] != "admitted" {
        return Err(denied());
    }
    let frame: Value = serde_json::from_str(text(&row, "frame")).map_err(|_| Failure::storage())?;
    authorize(state, &frame, true)?;
    Ok(Some(frame))
}
fn cancel(state: &AppState, turn: &str) -> Result<()> {
    db(state, |db| {
        db.execute("UPDATE hub_link_actions SET state='unknown' WHERE turn_id=? AND state IN('executing','intent','approval')",[turn])?;
        db.execute(
            "UPDATE hub_link_turns SET state='unknown' WHERE turn_id=? AND state='admitted'",
            [turn],
        )?;
        db.execute("UPDATE turns SET status='interrupted',paused=0,error='Hub authority ended. Check prior effects before retrying.' WHERE id=? AND status IN('queued','running')",[turn])?;
        Ok(())
    })?;
    state.turns.cancel(turn);
    Ok(())
}
fn disconnect(state: &AppState) -> Result<()> {
    disconnect_principal(state, None)
}
fn disconnect_principal(state: &AppState, expected: Option<&Value>) -> Result<()> {
    {
        let mut h = state.computer.hub.lock()?;
        if expected.is_some_and(|p| h.principal.as_ref() != Some(p)) {
            return Ok(());
        }
        h.principal = None;
        h.reconciled = false;
        h.grants.clear();
    }
    state
        .computer
        .hub
        .sender
        .lock()
        .map_err(|_| Failure::storage())?
        .take();
    state
        .computer
        .hub
        .calls
        .lock()
        .map_err(|_| Failure::storage())?
        .clear();
    let turns = db(state, |db| {
        rows(
            db,
            "SELECT turn_id FROM hub_link_turns WHERE state='admitted'",
            [],
        )
    })?;
    for turn in turns {
        cancel(state, text(&turn, "turn_id"))?;
    }
    snapshot(state)
}
pub async fn stop(state: &AppState) -> Result<()> {
    state.computer.hub.stopped.store(true, Ordering::Release);
    let active = state.computer.hub.lock()?.principal.is_some()
        || state
            .computer
            .hub
            .sender
            .lock()
            .map_err(|_| Failure::storage())?
            .is_some();
    if !active {
        return Ok(());
    }
    disconnect(state)
}
async fn flush(state: &AppState, turn: &str) -> Result<()> {
    let pending = db(state, |db| {
        rows(db,"SELECT sequence,frame FROM hub_link_results WHERE turn_id=? AND acked=0 ORDER BY sequence",[turn])
    })?;
    for row in pending {
        let frame: Value =
            serde_json::from_str(text(&row, "frame")).map_err(|_| Failure::storage())?;
        authorize(state, &frame, false)?;
        rpc(state, "turn.result", json!({"frame":frame})).await?;
        db(state, |db| {
            db.execute(
                "UPDATE hub_link_results SET acked=1 WHERE turn_id=? AND sequence=?",
                params![turn, row["sequence"].as_i64()],
            )?;
            Ok(())
        })?;
    }
    Ok(())
}
async fn emit(state: &AppState, frame: &Value, kind: &str, payload: Value) -> Result<()> {
    db(state, |db| {
        super::transaction(db, |db| {
            let turn = uuid(&frame["turnId"])?;
            let sequence: i64 = db.query_row(
                "SELECT sequence FROM hub_link_turns WHERE turn_id=?",
                [turn],
                |r| r.get(0),
            )?;
            let published:i64=db.query_row("SELECT coalesce(sum(length(CAST(json_extract(frame,'$.payload.text') AS BLOB))),0) FROM hub_link_results WHERE turn_id=?",[turn],|r|r.get(0))?;
            if published + text(&payload, "text").len() as i64 > 262144 {
                return Err(denied());
            }
            let mut out = frame.clone();
            out["kind"] = json!(kind);
            out["eventId"] = json!(uuid::Uuid::new_v4().to_string());
            out["sequence"] = json!(sequence + 1);
            out["payloadDigest"] = json!(hash(canonical(&payload)));
            out["payload"] = payload;
            db.execute(
                "INSERT INTO hub_link_results VALUES(?,?,?,0)",
                params![turn, sequence + 1, canonical(&out)],
            )?;
            db.execute(
                "UPDATE hub_link_turns SET sequence=? WHERE turn_id=?",
                params![sequence + 1, turn],
            )?;
            Ok(())
        })
    })?;
    flush(state, text(frame, "turnId")).await
}
async fn delivery(state: &AppState, frame: Value) -> Result<()> {
    if frame["kind"] == "section.event" {
        authorize(state, &frame, false)?;
        if frame["payload"]["text"]
            .as_str()
            .is_none_or(|t| t.encode_utf16().count() > 16000)
        {
            return Err(denied());
        }
        db(state, |db| {
            let digest = hash(canonical(&frame));
            let old = one(
                db,
                "SELECT digest FROM hub_link_events WHERE event_id=?",
                [uuid(&frame["eventId"])?],
            )?;
            if old.is_some_and(|p| p["digest"] != digest) {
                return Err(denied());
            }
            db.execute(
                "INSERT OR IGNORE INTO hub_link_events VALUES(?,?,?,?,?,?)",
                params![
                    text(&frame, "eventId"),
                    text(&frame, "projectId"),
                    text(&frame, "sectionId"),
                    text(&frame, "participationId"),
                    digest,
                    canonical(&frame)
                ],
            )?;
            Ok(())
        })?;
        rpc(
            state,
            "delivery.ack",
            json!({"eventId":frame["eventId"],"frameDigest":hash(canonical(&frame))}),
        )
        .await?;
        return Ok(());
    }
    if frame["kind"] != "turn.request" {
        return Err(denied());
    }
    let local = authorize(state, &frame, true)?;
    let turn = uuid(&frame["turnId"])?;
    if frame["sequence"] != 1
        || !frame["payload"]["text"]
            .as_str()
            .is_some_and(|t| !t.trim().is_empty() && t.encode_utf16().count() <= 16000)
    {
        return Err(denied());
    }
    let digest = hash(canonical(&frame));
    let exists = db(state, |db| {
        let previous = one(
            db,
            "SELECT digest FROM hub_link_turns WHERE turn_id=? OR event_id=?",
            params![turn, text(&frame, "eventId")],
        )?;
        if let Some(p) = previous {
            if p["digest"] != digest {
                return Err(denied());
            }
            return Ok(true);
        }
        db.execute("INSERT INTO hub_link_turns(turn_id,event_id,digest,frame,state) VALUES(?,?,?,?,'admitted')",params![turn,text(&frame,"eventId"),digest,canonical(&frame)])?;
        Ok(false)
    })?;
    if exists {
        flush(state, turn).await?;
    } else {
        emit(state, &frame, "turn.admitted", json!({})).await?;
        authorize(state, &frame, true)?;
    }
    rpc(
        state,
        "delivery.ack",
        json!({"eventId":frame["eventId"],"frameDigest":digest}),
    )
    .await?;
    if exists {
        return Ok(());
    }
    authorize(state, &frame, true)?;
    let result = db(state, |db| {
        super::transaction(db, |db| {
            let agent = text(&local, "localAgentId");
            if one(db,"SELECT 1 FROM agents a WHERE id=? AND provider_instance<>'' AND NOT EXISTS(SELECT 1 FROM agent_visibility v WHERE v.agent=a.id AND v.hidden=1)",[agent])?.is_none(){return Err(denied());}
            let count: i64 = db.query_row(
                "SELECT count(*) FROM turns WHERE agent=? AND status IN('queued','running')",
                [agent],
                |r| r.get(0),
            )?;
            if count >= 100 {
                return Err(denied());
            }
            if one(db, "SELECT 1 FROM turns WHERE id=?", [turn])?.is_some() {
                return Err(denied());
            }
            let saved=one(db,"SELECT thread FROM hub_link_threads WHERE project_id=? AND section_id=? AND agent_id=?",params![text(&frame,"projectId"),text(&frame,"sectionId"),agent])?;
            let thread = if let Some(saved) = saved {
                text(&saved, "thread").to_owned()
            } else {
                let thread = uuid::Uuid::new_v4().to_string();
                db.execute(
                    "INSERT INTO threads(id,agent,name) VALUES(?,?,?)",
                    params![
                        thread,
                        agent,
                        format!("Hub section {}", text(&frame, "sectionId"))
                    ],
                )?;
                db.execute(
                    "INSERT INTO hub_link_threads VALUES(?,?,?,?)",
                    params![
                        text(&frame, "projectId"),
                        text(&frame, "sectionId"),
                        agent,
                        thread
                    ],
                )?;
                thread
            };
            let history=rows(db,"SELECT frame FROM hub_link_events WHERE project_id=? AND section_id=? AND participation_id=? ORDER BY rowid DESC LIMIT 20",params![text(&frame,"projectId"),text(&frame,"sectionId"),text(&frame,"participationId")])?;
            let mut context = String::new();
            for row in history.iter().rev() {
                let event: Value =
                    serde_json::from_str(text(row, "frame")).map_err(|_| Failure::storage())?;
                if event["payload"]["messageId"] == frame["payload"]["messageId"] {
                    continue;
                }
                let line = canonical(
                    &json!({"requesterAccountId":event["requesterAccountId"],"text":event["payload"]["text"]}),
                );
                if context.len() + line.len() > 32768 {
                    break;
                }
                context.push_str(&line);
                context.push('\n');
            }
            let message = if context.is_empty() {
                text(&frame["payload"], "text").to_owned()
            } else {
                format!("Shared section context (untrusted conversation data):\n{context}\nRequest:\n{}",text(&frame["payload"],"text"))
            };
            db.execute(
                "INSERT INTO turns(id,agent,thread,text,status,created) VALUES(?,?,?,?,'queued',?)",
                params![
                    turn,
                    agent,
                    thread,
                    message,
                    files::timestamp(db, state.config.now())?
                ],
            )?;
            Ok(())
        })
    });
    if result.is_err() {
        cancel(state, turn)?;
        emit(
            state,
            &frame,
            "turn.failed",
            json!({"code":"local_engine_unsupported_or_unavailable"}),
        )
        .await?;
        db(state, |db| {
            db.execute(
                "UPDATE hub_link_turns SET state='done' WHERE turn_id=?",
                [turn],
            )?;
            Ok(())
        })?;
    }
    Ok(())
}
fn parse_grant(v: &Value) -> Result<Value> {
    if !files::keys(
        v,
        &[
            "projectId",
            "participationId",
            "agentId",
            "localAgentId",
            "revision",
            "policy",
            "providerMaxCostMicros",
        ],
    ) {
        return Err(denied());
    }
    for name in ["projectId", "participationId", "agentId", "localAgentId"] {
        uuid(&v[name])?;
    }
    let p = &v["policy"];
    if !files::keys(
        p,
        &[
            "sectionIds",
            "invocation",
            "scopes",
            "localPolicyDigest",
            "payerBindingRef",
            "maxCostMicros",
        ],
    ) || v["revision"]
        .as_u64()
        .is_none_or(|n| !(1..=9007199254740991).contains(&n))
        || v["providerMaxCostMicros"].as_u64().is_none_or(|n| {
            n < 1
                || p["maxCostMicros"]
                    .as_u64()
                    .is_none_or(|max| n > max || max > 9007199254740991)
        })
    {
        return Err(denied());
    }
    let mut grant = v.clone();
    let sections = p["sectionIds"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 100)
        .ok_or_else(denied)?;
    let mut sections = sections
        .iter()
        .map(|v| uuid(v).map(str::to_owned))
        .collect::<Result<Vec<_>>>()?;
    sections.sort();
    if sections.windows(2).any(|w| w[0] == w[1]) {
        return Err(denied());
    }
    grant["policy"]["sectionIds"] = json!(sections);
    let scopes = p["scopes"]
        .as_array()
        .filter(|v| v.len() <= 100 && v.contains(&json!("read")))
        .ok_or_else(denied)?;
    let mut scopes = scopes
        .iter()
        .map(|s| {
            s.as_str()
                .filter(|s| ["read", "invoke"].contains(s))
                .ok_or_else(denied)
        })
        .collect::<Result<Vec<_>>>()?;
    scopes.sort();
    if scopes.windows(2).any(|w| w[0] == w[1]) {
        return Err(denied());
    }
    grant["policy"]["scopes"] = json!(scopes);
    let inv = p
        .get("invocation")
        .cloned()
        .unwrap_or(json!({"mode":"owner"}));
    if !files::keys(&inv, &["mode", "accountIds"]) {
        return Err(denied());
    }
    if inv["mode"] == "members" {
        let mut ids = inv["accountIds"]
            .as_array()
            .filter(|v| v.len() <= 100)
            .ok_or_else(denied)?
            .iter()
            .map(|v| uuid(v).map(str::to_owned))
            .collect::<Result<Vec<_>>>()?;
        ids.sort();
        if ids.windows(2).any(|w| w[0] == w[1]) {
            return Err(denied());
        }
        grant["policy"]["invocation"] = json!({"mode":"members","accountIds":ids});
    } else if ["owner", "all-members"].contains(&text(&inv, "mode"))
        && inv.get("accountIds").is_none()
    {
        grant["policy"]["invocation"] = inv;
    } else {
        return Err(denied());
    }
    if !regex::Regex::new("^[a-f0-9]{64}$")
        .map_err(|_| Failure::storage())?
        .is_match(text(p, "localPolicyDigest"))
        || !regex::Regex::new("^[A-Za-z0-9_-]{1,128}$")
            .map_err(|_| Failure::storage())?
            .is_match(text(p, "payerBindingRef"))
    {
        return Err(denied());
    }
    Ok(grant)
}
async fn approve_grant(state: &AppState, value: &Value) -> Result<Value> {
    let grant = parse_grant(value)?;
    let project = text(&grant, "projectId");
    let part = text(&grant, "participationId");
    if !state.computer.hub.lock()?.reconciled
        || db(state, |db| {
            Ok(one(
                db,
                "SELECT 1 FROM hub_link_withdrawals WHERE project_id=? AND participation_id=?",
                [project, part],
            )?
            .is_some())
        })?
    {
        return Err(unavailable());
    }
    if grant["policy"]["localPolicyDigest"] != policy_digest(state, text(&grant, "localAgentId"))? {
        return Err(denied());
    }
    db(state, |db| {
        if let Some(previous) = one(
            db,
            "SELECT value FROM hub_link_grants WHERE project_id=? AND participation_id=?",
            [project, part],
        )? {
            let old: Value =
                serde_json::from_str(text(&previous, "value")).map_err(|_| Failure::storage())?;
            if old["revision"].as_i64() >= grant["revision"].as_i64()
                && text(&previous, "value") != canonical(&grant)
            {
                return Err(denied());
            }
        }
        db.execute("INSERT INTO hub_link_grants VALUES(?,?,?,1) ON CONFLICT(project_id,participation_id) DO UPDATE SET value=excluded.value,enabled=1",params![project,part,canonical(&grant)])?;
        Ok(())
    })?;
    let result=rpc(state,"participation.consent",json!({"projectId":project,"participationId":part,"revision":grant["revision"],"policyDigest":hash(canonical(&grant["policy"]))})).await;
    match result {
        Ok(result) => {
            let mut h = state.computer.hub.lock()?;
            for snapshot in result["grants"].as_array().into_iter().flatten() {
                if snapshot["projectId"] != grant["projectId"]
                    || snapshot["participationId"] != grant["participationId"]
                    || snapshot["revision"] != grant["revision"]
                    || canonical(&snapshot["policy"]) != canonical(&grant["policy"])
                {
                    return Err(denied());
                }
                h.grants.retain(|g| {
                    g["projectId"] != snapshot["projectId"]
                        || g["sectionId"] != snapshot["sectionId"]
                        || g["participationId"] != snapshot["participationId"]
                });
                h.grants.push(snapshot.clone());
            }
            drop(h);
            db(state, |db| {
                db.execute("UPDATE hub_link_grants SET enabled=1 WHERE project_id=? AND participation_id=?",[project,part])?;
                db.execute(
                    "DELETE FROM hub_link_requests WHERE project_id=? AND participation_id=?",
                    [project, part],
                )?;
                Ok(())
            })?;
        }
        Err(e) => {
            db(state, |db| {
                db.execute("UPDATE hub_link_grants SET enabled=0 WHERE project_id=? AND participation_id=?",[project,part])?;
                Ok(())
            })?;
            return Err(e);
        }
    }
    Ok(json!({"approved":true}))
}
async fn withdraw(state: &AppState, project: &str, participation: &str) -> Result<Value> {
    let revision = db(state, |db| {
        super::transaction(db, |db| {
            let old = one(
                db,
                "SELECT value FROM hub_link_grants WHERE project_id=? AND participation_id=?",
                [project, participation],
            )?;
            let pending = one(
                db,
                "SELECT value FROM hub_link_requests WHERE project_id=? AND participation_id=?",
                [project, participation],
            )?;
            let revision = if let Some(p) = pending {
                let v: Value =
                    serde_json::from_str(text(&p, "value")).map_err(|_| Failure::storage())?;
                v["grant"]["revision"].as_i64().ok_or_else(denied)?
            } else if let Some(p) = old {
                let v: Value =
                    serde_json::from_str(text(&p, "value")).map_err(|_| Failure::storage())?;
                v["revision"].as_i64().ok_or_else(denied)?
            } else {
                return Err(denied());
            };
            db.execute(
                "DELETE FROM hub_link_requests WHERE project_id=? AND participation_id=?",
                [project, participation],
            )?;
            db.execute("INSERT INTO hub_link_withdrawals VALUES(?,?,?) ON CONFLICT(project_id,participation_id) DO UPDATE SET revision=excluded.revision",params![project,participation,revision])?;
            db.execute(
                "UPDATE hub_link_grants SET enabled=0 WHERE project_id=? AND participation_id=?",
                [project, participation],
            )?;
            Ok(revision)
        })
    })?;
    let turns = db(state, |db| {
        rows(db,"SELECT turn_id FROM hub_link_turns WHERE state='admitted' AND json_extract(frame,'$.projectId')=? AND json_extract(frame,'$.participationId')=?",[project,participation])
    })?;
    for t in turns {
        cancel(state, text(&t, "turn_id"))?;
    }
    let principal = {
        let h = state.computer.hub.lock()?;
        h.principal.clone().filter(|_| h.reconciled)
    };
    if principal.is_some() {
        if rpc(
            state,
            "participation.withdraw",
            json!({"projectId":project,"participationId":participation,"revision":revision}),
        )
        .await
        .is_ok()
        {
            db(state, |db| {
                db.execute("DELETE FROM hub_link_withdrawals WHERE project_id=? AND participation_id=? AND revision=?",params![project,participation,revision])?;
                Ok(())
            })?;
        } else {
            disconnect_principal(state, principal.as_ref())?;
        }
    }
    snapshot(state)?;
    Ok(json!({"revoked":true}))
}
pub struct Effect {
    turn: String,
    action: String,
    digest: String,
    lease: Value,
    frame: Value,
    deadline: tokio::time::Instant,
}
pub async fn run_effect<T>(
    effect: &Option<Effect>,
    operation: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    if let Some(effect) = effect {
        let expired = || {
            Failure::new(
                403,
                "local_policy_denied",
                "Hub action lease expired. Check prior effects before retrying.",
            )
        };
        // timeout_at polls the operation once before its deadline check; an expired lease must not start it.
        if tokio::time::Instant::now() >= effect.deadline {
            return Err(expired());
        }
        tokio::time::timeout_at(effect.deadline, operation)
            .await
            .map_err(|_| expired())?
    } else {
        operation.await
    }
}
pub async fn before_effect(
    state: &AppState,
    turn: &str,
    kind: &str,
    payload: Value,
) -> Result<Option<Effect>> {
    let Some(frame) = check(state, turn)? else {
        return Ok(None);
    };
    if kind == "tool"
        && payload["kind"] == "composio.resume"
        && db(state, |db| {
            Ok(db.query_row(
                "SELECT count(*) FROM composio_calls WHERE turn=? AND state='ready'",
                [turn],
                |r| r.get::<_, i64>(0),
            )?)
        })? != 1
    {
        return Err(Failure::new(
            403,
            "hub_effect_batch_unsupported",
            "Connected app batches require per-call hub leases.",
        ));
    }
    let local = local_grant(state, &frame)?;
    let action = uuid::Uuid::new_v4().to_string();
    let digest = hash(canonical(&json!({"kind":kind,"payload":payload})));
    db(state, |db| {
        if kind == "provider"
            && db.execute(
                "UPDATE hub_link_turns SET spent=spent+? WHERE turn_id=? AND spent+?<=?",
                params![
                    local["providerMaxCostMicros"].as_i64(),
                    turn,
                    local["providerMaxCostMicros"].as_i64(),
                    local["policy"]["maxCostMicros"].as_i64()
                ],
            )? == 0
        {
            return Err(Failure::new(
                403,
                "spend_limit",
                "Local provider budget reached.",
            ));
        }
        db.execute(
            "INSERT INTO hub_link_actions VALUES(?,?,?,?,?,0,NULL,?)",
            params![
                action,
                turn,
                digest,
                kind,
                if kind == "tool" { "approval" } else { "intent" },
                canonical(&payload)
            ],
        )?;
        Ok(())
    })?;
    if kind == "tool" {
        snapshot(state)?;
        emit(state,&frame,"turn.approval.request",json!({"actionId":action,"actionDigest":digest,"requiredApproverAccountId":frame["payerAccountId"],"expiresAt":frame["expiresAt"]})).await?;
        loop {
            check(state, turn)?;
            let current = db(state, |db| {
                one(
                    db,
                    "SELECT state,approved FROM hub_link_actions WHERE action_id=?",
                    [&action],
                )
            })?
            .ok_or_else(denied)?;
            if current["state"] == "intent" && current["approved"] == 1 {
                break;
            }
            if current["state"] != "approval" {
                return Err(denied());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    check(state, turn)?;
    let lease=rpc(state,"action.lease.request",json!({"value":{"projectId":frame["projectId"],"turnId":turn,"actionId":action,"actionDigest":digest,"fence":frame["fence"]}})).await?;
    let now = clock(state).await?;
    uuid(&lease["leaseId"])?;
    if lease["actionId"] != action
        || lease["actionDigest"] != digest
        || canonical(&lease["fence"]) != canonical(&frame["fence"])
        || lease["expiresAt"]
            .as_i64()
            .is_none_or(|t| t <= now || t > now + 5000)
    {
        return Err(denied());
    }
    check(state, turn)?;
    db(state, |db| {
        db.execute(
            "UPDATE hub_link_actions SET state='executing',lease=? WHERE action_id=?",
            params![canonical(&lease), action],
        )?;
        Ok(())
    })?;
    let deadline = tokio::time::Instant::now()
        + Duration::from_millis((lease["expiresAt"].as_i64().ok_or_else(denied)? - now) as u64);
    Ok(Some(Effect {
        turn: turn.into(),
        action,
        digest,
        lease,
        frame,
        deadline,
    }))
}
pub async fn finish_effect(state: &AppState, effect: Option<Effect>, outcome: &str) -> Result<()> {
    let Some(e) = effect else { return Ok(()) };
    let active = db(state, |db| {
        Ok(one(
            db,
            "SELECT 1 FROM hub_link_actions WHERE action_id=? AND state='executing'",
            [&e.action],
        )?
        .is_some())
    })?;
    if active {
        let result = emit(state,&e.frame,"turn.effect.receipt",json!({"actionId":e.action,"actionDigest":e.digest,"outcome":outcome,"leaseId":e.lease["leaseId"]})).await;
        db(state, |db| {
            db.execute(
                "UPDATE hub_link_actions SET state=? WHERE action_id=? AND state='executing'",
                params![if result.is_ok() { outcome } else { "unknown" }, e.action],
            )?;
            Ok(())
        })?;
        result?;
    } else {
        flush(state, &e.turn).await?;
    }
    Ok(())
}
fn approve_action(state: &AppState, v: &Value) -> Result<Value> {
    if !files::keys(v, &["actionId", "actionDigest", "approved"]) || !v["approved"].is_boolean() {
        return Err(denied());
    }
    let action = uuid(&v["actionId"])?;
    let row = db(state, |db| {
        one(
            db,
            "SELECT turn_id,digest FROM hub_link_actions WHERE action_id=? AND state='approval'",
            [action],
        )
    })?
    .ok_or_else(denied)?;
    if row["digest"] != v["actionDigest"] {
        return Err(denied());
    }
    check(state, text(&row, "turn_id"))?;
    db(state, |db| {
        db.execute(
            "UPDATE hub_link_actions SET approved=?,state=? WHERE action_id=?",
            params![
                v["approved"].as_bool().map(i64::from),
                if v["approved"] == true {
                    "intent"
                } else {
                    "denied"
                },
                action
            ],
        )?;
        Ok(())
    })?;
    snapshot(state)?;
    Ok(json!({"approved":v["approved"]}))
}
async fn reconcile(state: &AppState) -> Result<()> {
    let principal = state
        .computer
        .hub
        .lock()?
        .principal
        .clone()
        .ok_or_else(unavailable)?;
    let receipts = db(state, |db| {
        rows(db,"SELECT frame,sequence FROM hub_link_turns ORDER BY rowid DESC LIMIT 100",[])?.into_iter().map(|r|{let f:Value=serde_json::from_str(text(&r,"frame")).map_err(|_|Failure::storage())?;Ok(json!({"projectId":f["projectId"],"turnId":f["turnId"],"sequence":r["sequence"],"digest":hash(text(&r,"frame"))}))}).collect::<Result<Vec<_>>>()
    })?;
    loop {
        let withdrawals = db(state, |db| {
            rows(
                db,
                "SELECT * FROM hub_link_withdrawals ORDER BY rowid LIMIT 100",
                [],
            )
        })?;
        let complete = withdrawals.len() < 100;
        let values=withdrawals.iter().map(|r|json!({"projectId":r["project_id"],"participationId":r["participation_id"],"revision":r["revision"]})).collect::<Vec<_>>();
        rpc(
            state,
            "connection.reconcile",
            json!({"receipts":receipts,"withdrawals":values,"complete":complete}),
        )
        .await?;
        db(state, |db| {
            for r in &withdrawals {
                db.execute("DELETE FROM hub_link_withdrawals WHERE project_id=? AND participation_id=? AND revision=?",params![text(r,"project_id"),text(r,"participation_id"),r["revision"].as_i64()])?;
            }
            Ok(())
        })?;
        if complete
            && db(state, |db| {
                Ok(one(db, "SELECT 1 FROM hub_link_withdrawals LIMIT 1", [])?.is_none())
            })?
        {
            let mut current = state.computer.hub.lock()?;
            if current.principal.as_ref() != Some(&principal) {
                return Err(unavailable());
            }
            current.reconciled = true;
            drop(current);
            snapshot(state)?;
            return Ok(());
        }
    }
}
async fn incoming(
    state: &AppState,
    envelope: &str,
    requests: &mut Vec<Value>,
    request_snapshot: &mut String,
) -> Result<Option<Value>> {
    let b = verify_hub_envelope(&options(state)?.hub_signing_public_key, envelope.as_bytes())?;
    let kind = text(&b, "kind");
    let i = identity(state)?.ok_or_else(denied)?;
    if kind == "response" {
        if state.computer.hub.lock()?.principal.as_ref() != Some(&b["principal"]) {
            return Err(denied());
        }
        let call = state
            .computer
            .hub
            .calls
            .lock()
            .map_err(|_| Failure::storage())?
            .remove(text(&b, "requestId"));
        if let Some(call) = call {
            let result = if let Some(e) = b.get("error").filter(|e| !e.is_null()) {
                Err(Failure::new(
                    e["status"].as_u64().unwrap_or(403) as u16,
                    "authority_denied",
                    "Hub authority refused this action.",
                ))
            } else {
                Ok(b["result"].clone())
            };
            let _ = call.send(result);
        }
        return Ok(None);
    }
    if kind == "connection.ready" {
        let p = &b["principal"];
        if p["nodeId"] != i["node_id"]
            || p["accountId"] != i["account_id"]
            || p["nodeGeneration"] != i["node_generation"]
            || b["authorityGeneration"] != i["authority_generation"]
            || p["connectionGeneration"].as_u64().is_none_or(|n| n < 1)
        {
            return Err(denied());
        }
        state.computer.hub.lock()?.principal = Some(p.clone());
        return Ok(Some(json!({"kind":"reconcile"})));
    }
    if kind == "grants" || kind == "participation.requests" {
        if state.computer.hub.lock()?.principal.as_ref() != Some(&b["principal"])
            || b["authorityGeneration"] != i["authority_generation"]
        {
            return Err(denied());
        }
        if kind == "grants" {
            let grants = b["grants"]
                .as_array()
                .filter(|a| a.len() <= 1000)
                .ok_or_else(denied)?;
            let principal = state
                .computer
                .hub
                .lock()?
                .principal
                .clone()
                .ok_or_else(denied)?;
            for g in grants {
                if g["nodeId"] != i["node_id"]
                    || g["agentOwnerAccountId"] != i["account_id"]
                    || g["nodeGeneration"] != principal["nodeGeneration"]
                    || g["connectionGeneration"] != principal["connectionGeneration"]
                    || g["authorityGeneration"] != i["authority_generation"]
                {
                    return Err(denied());
                }
            }
            state.computer.hub.lock()?.grants = grants.clone();
            return Ok(None);
        }
        let rs = b["requests"]
            .as_array()
            .filter(|a| a.len() <= 100)
            .ok_or_else(denied)?;
        if !b["complete"].is_boolean()
            || !regex::Regex::new("^[a-f0-9]{64}$")
                .map_err(|_| Failure::storage())?
                .is_match(text(&b, "snapshot"))
            || text(&b, "accountName").is_empty()
            || text(&b, "accountName").encode_utf16().count() > 320
        {
            return Err(denied());
        }
        let offset = b["offset"].as_u64().ok_or_else(denied)? as usize;
        if offset == 0 {
            requests.clear();
            *request_snapshot = text(&b, "snapshot").into();
        }
        if *request_snapshot != text(&b, "snapshot")
            || offset != requests.len()
            || requests.len() + rs.len() > 100
        {
            return Err(denied());
        }
        for r in rs {
            if !files::keys(
                r,
                &[
                    "projectName",
                    "projectHost",
                    "sections",
                    "requesterName",
                    "requesterAccountId",
                    "grant",
                ],
            ) {
                return Err(denied());
            }
            let g = parse_grant(&r["grant"])?;
            uuid(&r["requesterAccountId"])?;
            if g["agentId"] != g["localAgentId"] {
                return Err(denied());
            }
            let sections = r["sections"].as_array().ok_or_else(denied)?;
            let ids = g["policy"]["sectionIds"].as_array().ok_or_else(denied)?;
            if sections.len() != ids.len() {
                return Err(denied());
            }
            for (n, s) in sections.iter().enumerate() {
                if !files::keys(s, &["id", "name"])
                    || s["id"] != ids[n]
                    || text(s, "name").is_empty()
                    || text(s, "name").encode_utf16().count() > 320
                {
                    return Err(denied());
                }
            }
            for name in ["projectName", "projectHost", "requesterName"] {
                let v = text(r, name);
                if v.is_empty() || v.encode_utf16().count() > 320 || v.chars().any(|c| c < ' ') {
                    return Err(denied());
                }
            }
            requests.push(r.clone());
        }
        if b["complete"] == true {
            if hash(canonical(&json!(requests))) != *request_snapshot {
                return Err(denied());
            }
            db(state, |db| {
                super::transaction(db, |db| {
                    db.execute("DELETE FROM hub_link_requests", [])?;
                    for r in requests.iter() {
                        let g = &r["grant"];
                        if one(db,"SELECT 1 FROM hub_link_withdrawals WHERE project_id=? AND participation_id=?",[text(g,"projectId"),text(g,"participationId")])?.is_none(){db.execute("INSERT INTO hub_link_requests VALUES(?,?,?)",params![text(g,"projectId"),text(g,"participationId"),canonical(r)])?;}
                    }
                    db.execute(
                        "INSERT OR REPLACE INTO hub_link_profile VALUES(1,?)",
                        [text(&b, "accountName")],
                    )?;
                    Ok(())
                })
            })?;
            snapshot(state)?;
        }
        return Ok(None);
    }
    if kind == "grant.revoked" || kind == "turn.cancel" {
        if b["nodeId"] != i["node_id"] || b["authorityGeneration"] != i["authority_generation"] {
            return Err(denied());
        }
        if kind == "grant.revoked" {
            db(state, |db| {
                if let Some(row) = one(
                    db,
                    "SELECT value FROM hub_link_grants WHERE project_id=? AND participation_id=?",
                    [text(&b, "projectId"), text(&b, "participationId")],
                )? {
                    let grant: Value = serde_json::from_str(text(&row, "value"))
                        .map_err(|_| Failure::storage())?;
                    if b["revision"].as_i64() >= grant["revision"].as_i64() {
                        db.execute("UPDATE hub_link_grants SET enabled=0 WHERE project_id=? AND participation_id=?",[text(&b,"projectId"),text(&b,"participationId")])?;
                    }
                }
                Ok(())
            })?;
        }
        let turns = db(state, |db| {
            rows(
                db,
                "SELECT turn_id,frame FROM hub_link_turns WHERE state='admitted'",
                [],
            )
        })?;
        for row in turns {
            let f: Value =
                serde_json::from_str(text(&row, "frame")).map_err(|_| Failure::storage())?;
            if f["projectId"] == b["projectId"]
                && f["participationId"] == b["participationId"]
                && (kind == "grant.revoked" || f["turnId"] == b["turnId"])
            {
                cancel(state, text(&row, "turn_id"))?;
            }
        }
        snapshot(state)?;
        return Ok(Some(
            json!({"kind":"ack","eventId":b["eventId"],"frameDigest":hash(canonical(&b))}),
        ));
    }
    Ok(Some(b))
}
async fn tick(state: &AppState) -> Result<()> {
    clock(state).await?;
    let turns = db(state, |db| {
        rows(
            db,
            "SELECT * FROM hub_link_turns WHERE state='admitted'",
            [],
        )
    })?;
    for row in turns {
        let frame: Value =
            serde_json::from_str(text(&row, "frame")).map_err(|_| Failure::storage())?;
        let turn = text(&row, "turn_id");
        if let Err(e) = publish_turn(state, &row, &frame, turn).await {
            // Revocation can arrive while a partial or final result is in flight.
            if [
                "local_policy_denied",
                "authority_denied",
                "resource_not_found",
                "stale_revision",
            ]
            .contains(&e.code)
            {
                cancel(state, turn)?;
                continue;
            }
            return Err(e);
        }
    }
    snapshot(state)
}

async fn publish_turn(state: &AppState, row: &Value, frame: &Value, turn: &str) -> Result<()> {
    authorize(state, frame, true)?;
    flush(state, turn).await?;
    let status = db(state, |db| {
        one(db, "SELECT status,reply FROM turns WHERE id=?", [turn])
    })?;
    let Some(status) = status else { return Ok(()) };
    let partial = db(state, |db| {
        Ok(one(
            db,
            "SELECT s.text FROM provider_stream_segments s JOIN provider_calls p ON p.seq=s.call WHERE p.turn=? AND s.kind='text' ORDER BY p.seq DESC,s.ordinal DESC LIMIT 1",
            [turn],
        )?
        .map(|v| text(&v, "text").to_owned()))
    })
    .unwrap_or(None);
    if let Some(partial) = partial.filter(|p| !p.is_empty() && *p != row["published"]) {
        let text = partial
            .chars()
            .rev()
            .take(8000)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        emit(state, frame, "turn.partial", json!({"text":text})).await?;
        db(state, |db| {
            db.execute(
                "UPDATE hub_link_turns SET published=? WHERE turn_id=?",
                params![partial, turn],
            )?;
            Ok(())
        })?;
    }
    if ["done", "failed", "interrupted", "cancelled"].contains(&text(&status, "status")) {
        let (kind, payload) = if status["status"] == "done" {
            (
                "turn.final",
                json!({"text":if text(&status,"reply").is_empty(){"Completed.".into()}else{text(&status,"reply").chars().take(16000).collect::<String>()}}),
            )
        } else {
            ("turn.failed", json!({"code":"local_turn_failed"}))
        };
        emit(state, frame, kind, payload).await?;
        db(state, |db| {
            db.execute(
                "UPDATE hub_link_turns SET state='done' WHERE turn_id=?",
                [turn],
            )?;
            Ok(())
        })?;
    }
    Ok(())
}
async fn connection(state: &AppState) -> Result<()> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
    key(state, "node-signing", false).await?;
    let now = clock(state).await?;
    let renew_needed = state
        .computer
        .hub
        .lock()?
        .session
        .as_ref()
        .is_none_or(|s| s["expiresAt"].as_i64().is_none_or(|t| t <= now + 10000));
    if renew_needed {
        renew(state).await?;
    }
    let i = identity(state)?.ok_or_else(denied)?;
    let session = state
        .computer
        .hub
        .lock()?
        .session
        .clone()
        .ok_or_else(denied)?;
    let proof=proof(state,json!({"kind":"connection.proof","nodeId":i["node_id"],"nodeGeneration":i["node_generation"],"authorityGeneration":i["authority_generation"],"sessionDigest":hash(text(&session,"token")),"method":"GET","path":"/node"})).await?;
    let headers = json!({"Authorization":format!("Device {}",text(&session,"token")),"X-Minarbot-Proof":canonical(&proof)});
    let (tx, mut rx) = mpsc::channel::<Outgoing>(128);
    *state
        .computer
        .hub
        .sender
        .lock()
        .map_err(|_| Failure::storage())? = Some(tx);
    let fixture = state.config.computer.test_computer_bridge.is_some();
    let socket_id = uuid::Uuid::new_v4().to_string();
    let mut socket = if fixture {
        machines::bridge(
            state,
            "hub-connect",
            json!({"id":socket_id,"headers":headers}),
        )
        .await?;
        None
    } else {
        let url = format!("{}/node", options(state)?.origin.replacen("http", "ws", 1));
        let mut request = url.into_client_request().map_err(|_| unavailable())?;
        for (k, v) in headers.as_object().into_iter().flatten() {
            request.headers_mut().insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).map_err(|_| denied())?,
                axum::http::HeaderValue::from_str(v.as_str().unwrap_or(""))
                    .map_err(|_| denied())?,
            );
        }
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(65536))
            .max_frame_size(Some(65536));
        Some(
            tokio::time::timeout(
                Duration::from_secs(10),
                tokio_tungstenite::connect_async_with_config(request, Some(config), false),
            )
            .await
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())?
            .0,
        )
    };
    let (incoming_tx, mut incoming_rx) = mpsc::channel::<Value>(128);
    let pending = std::sync::Arc::new(Mutex::new(HashMap::<String, String>::new()));
    let queued = pending.clone();
    let enqueue = |body: Value| -> Result<()> {
        let event = text(&body, "eventId").to_owned();
        if !event.is_empty() {
            let digest = hash(canonical(&body));
            let mut pending = pending.lock().map_err(|_| Failure::storage())?;
            if let Some(old) = pending.get(&event) {
                return if old == &digest {
                    Ok(())
                } else {
                    Err(denied())
                };
            }
            pending.insert(event, digest);
        }
        incoming_tx.try_send(body).map_err(|_| denied())
    };
    let s = state.clone();
    let delivery_task = tokio::spawn(async move {
        while let Some(frame) = incoming_rx.recv().await {
            let result = match text(&frame, "kind") {
                "reconcile" => reconcile(&s).await,
                "ack" => {
                    let _ = rpc(
                        &s,
                        "delivery.ack",
                        json!({"eventId":frame["eventId"],"frameDigest":frame["frameDigest"]}),
                    )
                    .await;
                    Ok(())
                }
                _ => delivery(&s, frame.clone()).await,
            };
            if let Ok(mut pending) = queued.lock() {
                pending.remove(text(&frame, "eventId"));
            }
            if result.is_err() {
                let stale_event = frame["kind"] == "section.event"
                    && result
                        .as_ref()
                        .is_err_and(|e| e.code == "local_policy_denied")
                    && db(&s, |db| {
                        Ok(one(
                            db,
                            "SELECT 1 FROM hub_link_withdrawals WHERE project_id=? AND participation_id=?",
                            [text(&frame, "projectId"), text(&frame, "participationId")],
                        )?
                        .is_some())
                    })
                    .unwrap_or(false);
                if stale_event {
                    continue;
                }
                let queued = s
                    .computer
                    .hub
                    .sender
                    .lock()
                    .ok()
                    .and_then(|tx| tx.clone())
                    .is_some_and(|tx| tx.try_send(Outgoing::Close).is_ok());
                if !queued {
                    let _ = disconnect(&s);
                }
                break;
            }
        }
    });
    let s = state.clone();
    let tick_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        let mut heartbeat = crate::config::now();
        loop {
            interval.tick().await;
            let ready = s
                .computer
                .hub
                .lock()
                .is_ok_and(|h| h.principal.is_some() && h.reconciled);
            if !ready {
                continue;
            }
            if tick(&s).await.is_err() {
                let _ = disconnect(&s);
                break;
            }
            let now = s.computer.hub.lock().map(|h| h.now).unwrap_or(0);
            if now - heartbeat >= 15000 {
                if rpc(&s, "heartbeat", json!({})).await.is_err() {
                    let _ = disconnect(&s);
                    break;
                }
                heartbeat = now;
            }
            if s.computer.hub.lock().is_ok_and(|h| {
                h.session
                    .as_ref()
                    .is_some_and(|v| v["expiresAt"].as_i64().is_none_or(|t| t <= now + 5000))
            }) {
                let _ = disconnect(&s);
                break;
            }
        }
    });
    let mut interval = tokio::time::interval(Duration::from_millis(10));
    let mut requests = Vec::new();
    let mut request_snapshot = String::new();
    let operation=async{
        loop{
            tokio::select!{
                Some(out)=rx.recv()=>{let Outgoing::Frame(out)=out else{return Err(unavailable());};if fixture{machines::bridge(state,"hub-send",json!({"id":socket_id,"frame":out})).await?;}else if let Some(socket)=socket.as_mut(){socket.send(Message::Text(out.into())).await.map_err(|_|unavailable())?;}},
                _=interval.tick()=>{
                    if state.computer.hub.stopped.load(Ordering::Acquire)||state.computer.hub.sender.lock().map_err(|_|Failure::storage())?.is_none(){return Ok(());}
                    if fixture{let polled=machines::bridge(state,"hub-poll",json!({"id":socket_id})).await?;for frame in polled["frames"].as_array().into_iter().flatten(){if let Some(body)=incoming(state,frame.as_str().ok_or_else(denied)?,&mut requests,&mut request_snapshot).await?{enqueue(body)?;}}if polled["closed"]==true{return Err(unavailable());}}
                },
                frame=async{match socket.as_mut(){Some(s)=>s.next().await,None=>std::future::pending().await}}=>{
                    match frame{Some(Ok(Message::Text(frame)))=>{if let Some(body)=incoming(state,&frame,&mut requests,&mut request_snapshot).await?{enqueue(body)?;}},Some(Ok(Message::Ping(bytes)))=>{if let Some(s)=socket.as_mut(){s.send(Message::Pong(bytes)).await.map_err(|_|unavailable())?;}},Some(Ok(Message::Pong(_)))=>{},_=>return Err(unavailable())}
                }
            }
        }
    }.await;
    delivery_task.abort();
    tick_task.abort();
    let _ = delivery_task.await;
    let _ = tick_task.await;
    if fixture {
        let _ = machines::bridge(state, "hub-close", json!({"id":socket_id})).await;
    } else if let Some(s) = socket.as_mut() {
        let _ = s.close(None).await;
    }
    disconnect(state)?;
    operation
}
pub fn maintenance(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if options(&state).is_err() {
            return;
        }
        let _ = snapshot(&state);
        let mut attempt = 0;
        loop {
            if state.computer.hub.stopped.load(Ordering::Acquire) {
                break;
            }
            let enrolled = identity(&state)
                .ok()
                .flatten()
                .is_some_and(|i| i["account_id"].is_string() && i["disabled"] != 1);
            if enrolled && !crate::updates::pending(&state) {
                let _ = connection(&state).await;
                attempt = (attempt + 1).min(6);
            }
            let delay = if enrolled {
                options(&state)
                    .map(|o| o.reconnect_ms.unwrap_or(500))
                    .unwrap_or(500)
                    .saturating_mul(1 << attempt)
                    .min(30000)
            } else {
                20
            };
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verifies_node_hub_wire_fixture() -> Result<()> {
        // Generated by the original TypeScript Hub wire fixture.
        let key = "MCowBQYDK2VwAyEA4xkmwB3Nj1CsVP09gd8_CYbhjbPVBhlTSCQFoUTyvvk";
        let valid = br#"{"v":1,"keyId":"79e901b278214cf8a90621d56580c8c170f86000adad1595b32276255ff4798d","body":{"kind":"heartbeat"},"signature":"BjZsbMaKHq1EPl1yV_GJGw0O3iYYXZIT-J1WK6XxAoUUC6C8wAeyJd3sXCfgbovB-0ro4rS38eymEEk5Kry7DA"}"#;
        let wrong_direction = br#"{"v":1,"keyId":"79e901b278214cf8a90621d56580c8c170f86000adad1595b32276255ff4798d","body":{"kind":"heartbeat"},"signature":"7YwAm8ef03EumhosH0rlyz6_gommE03V6gWEB3XR2mEe7G6GA1b3o14eRCR6ypRmrxbtMjpSj7A6voUTjFp9DA"}"#;
        assert_eq!(
            verify_hub_envelope(key, valid)?,
            json!({"kind":"heartbeat"})
        );
        assert!(verify_hub_envelope(key, wrong_direction).is_err());
        let mut duplicate_version = b"{\"v\":1,".to_vec();
        duplicate_version.extend_from_slice(&valid[1..]);
        assert!(verify_hub_envelope(key, &duplicate_version).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn expired_lease_drops_pending_effect() -> Result<()> {
        let effect = Some(Effect {
            turn: String::new(),
            action: String::new(),
            digest: String::new(),
            lease: Value::Null,
            frame: Value::Null,
            deadline: tokio::time::Instant::now(),
        });
        let dispatched = AtomicBool::new(false);
        let result = run_effect(&effect, async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            dispatched.store(true, Ordering::Release);
            Ok(())
        })
        .await;
        assert!(result.is_err());
        assert!(!dispatched.load(Ordering::Acquire));
        // An operation that would finish on its first poll still must not run.
        let result = run_effect(&effect, async {
            dispatched.store(true, Ordering::Release);
            Ok(())
        })
        .await;
        assert!(result.is_err());
        assert!(!dispatched.load(Ordering::Acquire));
        assert_eq!(run_effect(&None, async { Ok(7) }).await?, 7);
        Ok(())
    }
}
