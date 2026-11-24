mod dispatch;
mod export;
mod fixture_control;
mod fixture_keys;
mod journal;
mod keys;
mod lock;
mod protected_wait;
mod provider;
mod schema;
mod stream;
mod task_loop;
#[cfg(test)]
mod tests;

use super::*;
use crate::config::now;
use reqwest::{Client, RequestBuilder};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::{
    net::TcpListener,
    sync::{oneshot, Mutex as AsyncMutex},
};
use uuid::Uuid;

const LIMIT: usize = 65_536;
const RESULT_LIMIT: usize = 8 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(15);

fn request_id() -> String {
    Uuid::new_v4().to_string()
}
fn invalid(message: &str) -> Failure {
    fail(409, "device_identity_invalid", message)
}
fn transport() -> Failure {
    let mut error = fail(503, "device_offline", "Device connection unavailable.");
    error.retryable = true;
    error
}
fn zone_now() -> String {
    std::env::var("TZ")
        .ok()
        .or_else(|| {
            fs::canonicalize("/etc/localtime")
                .ok()?
                .strip_prefix("/usr/share/zoneinfo")
                .ok()?
                .to_str()
                .map(str::to_owned)
        })
        .and_then(|candidate| zone(&json!(candidate)).ok())
        .unwrap_or_else(|| "UTC".into())
}
fn valid_origin(value: &str, development: bool) -> bool {
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    url.origin().ascii_serialization() == value
        && url.username().is_empty()
        && url.password().is_none()
        && (url.scheme() == "https"
            || development
                && url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "[::1]")))
}

fn private_metadata(path: &Path, directory: bool) -> db::Result<fs::Metadata> {
    let info = fs::symlink_metadata(path)?;
    if !(if directory {
        info.is_dir()
    } else {
        info.is_file() && info.nlink() == 1
    }) || info.mode() & 0o077 != 0
        || info.uid() != rustix::process::geteuid().as_raw()
    {
        return Err("Device metadata must be private and owned by the current user".into());
    }
    Ok(info)
}

fn save(path: &Path, value: &[u8], replace: bool) -> db::Result<()> {
    let temp = path.with_extension(format!("{}.tmp", request_id()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temp)?;
    let installed = (|| -> db::Result<()> {
        file.write_all(value)?;
        file.sync_all()?;
        if replace {
            fs::rename(&temp, path)?;
        } else {
            fs::hard_link(&temp, path)?;
            fs::remove_file(&temp)?;
        }
        File::open(path.parent().ok_or("Missing metadata directory")?)?.sync_all()?;
        Ok(())
    })();
    if installed.is_err() {
        let _ = fs::remove_file(temp);
    }
    installed
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Identity {
    v: u8,
    server: String,
    installation_id: String,
    device_id: String,
    signing_ref: String,
    encryption_ref: String,
    zone: String,
    zone_revision: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    descriptor: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending: Option<Pending>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Pending {
    candidate: Value,
    challenge: Value,
    request: Value,
    signing_ref: String,
    encryption_ref: String,
}

struct Host {
    db: Connection,
    directory: PathBuf,
    identity: Identity,
    keys: Arc<dyn keys::Store>,
    client: Client,
    executor: Option<Arc<lock::Lock>>,
    tasks: Arc<task_loop::TaskSlots>,
    recovery: Arc<AsyncMutex<()>>,
    fixture_checkpoint: Option<String>,
    provider: Option<Arc<provider::Provider>>,
}

impl Host {
    fn open(
        database: &Path,
        server: &str,
        development: bool,
        keys: Box<dyn keys::Store>,
    ) -> db::Result<Self> {
        if !valid_origin(server, development) {
            return Err("Device server must be a configured HTTPS origin".into());
        }
        let database = std::path::absolute(database)?;
        let directory = minarbot_launcher::files::sidecar(&database, ".host");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)?;
        private_metadata(&directory, true)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&database)?;
        if !file.metadata()?.is_file() || file.metadata()?.nlink() != 1 {
            return Err("Device database must be a regular file".into());
        }
        let mut db = Connection::open(&database)?;
        schema::initialize(&mut db)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        let path = directory.join("identity.json");
        let new = match fs::symlink_metadata(&path) {
            Ok(_) => false,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
            Err(err) => return Err(err.into()),
        };
        let identity = if new {
            if db
                .query_row("SELECT 1 FROM device_state", [], |_| Ok(()))
                .optional()?
                .is_some()
            {
                return Err("Device identity metadata is missing; recovery required".into());
            }
            Identity {
                v: 1,
                server: server.into(),
                installation_id: request_id(),
                device_id: request_id(),
                signing_ref: format!("signing/{}", request_id()),
                encryption_ref: format!("encryption/{}", request_id()),
                zone: zone_now(),
                zone_revision: 1,
                descriptor: None,
                pending: None,
            }
        } else {
            if private_metadata(&path, false)?.len() > 32768 {
                return Err("Device identity file is too large".into());
            }
            let value = crate::web::parse_json(&fs::read(&path)?).map_err(|err| err.message)?;
            serde_json::from_value(value)?
        };
        if identity.v != 1
            || identity.server != server
            || id(&json!(identity.installation_id)).is_err()
            || id(&json!(identity.device_id)).is_err()
            || zone(&json!(identity.zone)).is_err()
            || identity.zone_revision < 1
        {
            return Err("Device identity does not match configured server".into());
        }
        let host = Self {
            db,
            directory,
            identity,
            keys: Arc::from(keys),
            executor: None,
            tasks: Arc::default(),
            recovery: Arc::default(),
            fixture_checkpoint: None,
            provider: None,
            client: Client::builder()
                .connect_timeout(TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        };
        host.key("installation", false, new)
            .map_err(|err| err.message)?;
        let signing = host
            .key(&host.identity.signing_ref, false, new)
            .map_err(|err| err.message)?;
        let encryption = host
            .key(&host.identity.encryption_ref, true, new)
            .map_err(|err| err.message)?;
        if let Some(d) = &host.identity.descriptor {
            descriptor(d, true).map_err(|err| err.message)?;
            if d["deviceId"] != host.identity.device_id
                || d["installationId"] != host.identity.installation_id
                || d["signingPublicKey"] != B64.encode(signing.public())
                || d["encryptionPublicKey"] != B64.encode(encryption.public())
            {
                return Err("Device identity key mismatch".into());
            }
            host.remember_descriptor(d).map_err(|err| err.message)?;
        }
        if new {
            host.persist(&host.identity, false)
                .map_err(|err| err.message)?;
        }
        Ok(host)
    }

    fn acquire_executor(&mut self) -> Result<()> {
        if self.executor.is_none() {
            self.executor = Some(Arc::new(lock::Lock::acquire(
                &self.db,
                &self.directory.join("executor"),
            )?));
        }
        Ok(())
    }

    fn stream_peer(&self, database: &Path) -> Result<Self> {
        let db = Connection::open_with_flags(
            database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.execute_batch(
            "PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL",
        )?;
        Ok(Self {
            db,
            directory: self.directory.clone(),
            identity: self.identity.clone(),
            keys: self.keys.clone(),
            client: self.client.clone(),
            executor: self.executor.clone(),
            tasks: self.tasks.clone(),
            recovery: self.recovery.clone(),
            fixture_checkpoint: None,
            provider: self.provider.clone(),
        })
    }

    async fn checkpoint(&self, name: &str) {
        if self.fixture_checkpoint.as_deref() == Some(name) {
            println!("{}", json!({"checkpoint":name}));
            std::future::pending::<()>().await;
        }
    }

    fn secret_service(&self) -> String {
        format!(
            "minarbot/{}/{}",
            self.identity.installation_id, self.identity.device_id
        )
    }

    fn key(&self, name: &str, encryption: bool, create: bool) -> Result<keys::Private> {
        keys::Private::open(
            &*self.keys,
            &format!("minarbot/installation/{}", self.identity.installation_id),
            name,
            encryption,
            create,
        )
    }

    fn persist(&self, identity: &Identity, replace: bool) -> Result<()> {
        let data = serde_json::to_vec(identity).map_err(|_| Failure::storage())?;
        save(&self.directory.join("identity.json"), &data, replace).map_err(|_| Failure::storage())
    }

    fn remember_descriptor(&self, d: &Value) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        let current: Option<String> = tx
            .query_row("SELECT device_id FROM device_state WHERE id=1", [], |r| {
                r.get(0)
            })
            .optional()?;
        if current
            .as_ref()
            .is_some_and(|id| id != &self.identity.device_id)
        {
            return Err(invalid("Device store belongs to another identity."));
        }
        let generation = revision(&d["deviceGeneration"], 1)?;
        let authority = text(d, "authorityGeneration");
        tx.execute("INSERT INTO device_state VALUES (1,?,?,?,0,0,'[]') ON CONFLICT(id) DO UPDATE SET authority_generation=excluded.authority_generation,device_generation=excluded.device_generation,executor_refs_json=CASE WHEN device_state.executor_refs_json='{}' THEN '[]' ELSE device_state.executor_refs_json END", params![self.identity.device_id, authority, generation])?;
        tx.execute(
            "INSERT OR IGNORE INTO device_enrollment_keys VALUES (?,?,?,?)",
            params![
                authority,
                generation,
                self.identity.signing_ref,
                text(d, "signingPublicKey")
            ],
        )?;
        let saved: (String, String) = tx.query_row("SELECT signing_ref,signing_public_key FROM device_enrollment_keys WHERE authority_generation=? AND device_generation=?", params![authority, generation], |r| Ok((r.get(0)?, r.get(1)?)))?;
        if saved.0 != self.identity.signing_ref || saved.1 != text(d, "signingPublicKey") {
            return Err(keys::unavailable());
        }
        tx.commit()?;
        Ok(())
    }

    async fn human(&mut self, path: &str, token: &str, value: Option<&Value>) -> Result<Value> {
        let request = value.map_or_else(request_id, |v| text(v, "requestId").to_owned());
        let mut req = self
            .client
            .request(
                if value.is_some() {
                    Method::POST
                } else {
                    Method::GET
                },
                format!("{}{path}", self.identity.server),
            )
            .bearer_auth(token)
            .header("origin", &self.identity.server)
            .header("content-type", "application/json")
            .header("x-minarbot-protocol", "1")
            .header("x-minarbot-request-id", request);
        if let Some(value) = value {
            req = req.body(value.to_string());
        }
        submit(req).await
    }

    async fn enroll(&mut self, token: &str, mode: &str) -> Result<Value> {
        let actor = self.human("/api/session", token, None).await?;
        let account = id(&actor["data"]["accountId"])?;
        if self.identity.pending.is_none() {
            let prior = self.identity.descriptor.clone();
            if (mode == "new") != prior.is_none()
                || prior.as_ref().is_some_and(|d| d["accountId"] != account)
            {
                return Err(invalid(
                    "Enrollment mode or account does not match local identity.",
                ));
            }
            let mut generation = 0;
            let mut key_revision = 1;
            let mut current_zone = self.identity.zone.clone();
            let mut zone_revision = self.identity.zone_revision;
            if let Some(prior) = prior {
                let list = self.human("/api/devices", token, None).await?;
                let current = list["data"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|v| {
                        v["id"] == self.identity.device_id
                            && v["installation"] == self.identity.installation_id
                    })
                    .ok_or_else(|| invalid("Enrolled device not found."))?;
                let saved = crate::web::parse_json(text(current, "descriptor_json").as_bytes())?;
                if saved["signingPublicKey"] != prior["signingPublicKey"]
                    || saved["encryptionPublicKey"] != prior["encryptionPublicKey"]
                {
                    return Err(invalid("Enrolled keys changed; recovery required."));
                }
                generation = revision(&current["generation"], 1)?;
                key_revision = revision(&saved["keyRevision"], 1)?;
                current_zone = zone(&current["zone"])?;
                zone_revision = revision(&current["zone_revision"], 1)?;
            }
            let rotate = mode == "rotate";
            let signing_ref = if rotate {
                format!("signing/{}", request_id())
            } else {
                self.identity.signing_ref.clone()
            };
            let encryption_ref = if rotate {
                format!("encryption/{}", request_id())
            } else {
                self.identity.encryption_ref.clone()
            };
            let installation = self.key("installation", false, false)?;
            let signing = self.key(&signing_ref, false, rotate)?;
            let encryption = self.key(&encryption_ref, true, rotate)?;
            let mut candidate = json!({
                "requestId":request_id(),"installationId":self.identity.installation_id,"deviceId":self.identity.device_id,
                "mode":mode,"installationPublicKey":B64.encode(installation.public()),"deviceSigningKey":B64.encode(signing.public()),
                "deviceEncryptionKey":B64.encode(encryption.public()),"deviceLabel":"Minarbot device","requestedCapabilities":["execution"],
                "zone":current_zone,"zoneRevision":zone_revision
            });
            if mode != "new" {
                candidate["expectedGeneration"] = generation.into();
                candidate["expectedKeyRevision"] = key_revision.into();
            }
            let candidate = super::candidate(&candidate)?;
            let challenge = self
                .human("/api/devices/link-challenges", token, Some(&candidate))
                .await?["data"]
                .clone();
            let expires_at = revision(&challenge["expiresAt"], 1)?;
            let current = now();
            id(&challenge["challengeId"])?;
            id(&challenge["authorityGeneration"])?;
            decode(&challenge["nonce"], 32)?;
            if challenge["accountId"] != account
                || challenge["installationThumbprint"] != byte_digest(&installation.public())
                || challenge["signingThumbprint"] != byte_digest(&signing.public())
                || challenge["encryptionThumbprint"] != byte_digest(&encryption.public())
                || challenge["deviceGeneration"] != generation + 1
                || challenge["keyRevision"] != key_revision + i64::from(rotate)
                || expires_at <= current
                || expires_at > current + 121_000
            {
                return Err(invalid("Invalid server link challenge."));
            }
            let d = &challenge;
            let c = &candidate;
            let body = json!([
                "minarbot-link-v1",
                d["challengeId"],
                account,
                d["clerkSub"],
                c["installationId"],
                c["deviceId"],
                d["installationThumbprint"],
                d["signingThumbprint"],
                d["encryptionThumbprint"],
                c["deviceLabel"],
                digest(&json!([
                    "minarbot-device-capabilities-v1",
                    c["requestedCapabilities"]
                ])),
                d["authorityGeneration"],
                d["deviceGeneration"],
                d["keyRevision"],
                c["zone"],
                c["zoneRevision"],
                c["mode"],
                c["expectedGeneration"],
                c["expectedKeyRevision"],
                d["nonce"],
                expires_at
            ]);
            let signature = signing.sign(&json!([
                "minarbot-device-descriptor-v1",
                account,
                c["installationId"],
                c["deviceId"],
                d["authorityGeneration"],
                d["deviceGeneration"],
                d["keyRevision"],
                c["zone"],
                c["zoneRevision"],
                c["deviceSigningKey"],
                c["deviceEncryptionKey"]
            ]))?;
            let descriptor = json!({"v":1,"accountId":account,"installationId":c["installationId"],"deviceId":c["deviceId"],
                "authorityGeneration":d["authorityGeneration"],"deviceGeneration":d["deviceGeneration"],"keyRevision":d["keyRevision"],
                "zone":c["zone"],"zoneRevision":c["zoneRevision"],"signingPublicKey":c["deviceSigningKey"],"encryptionPublicKey":c["deviceEncryptionKey"],"signature":signature});
            let mut request = json!({"requestId":request_id(),"challengeId":d["challengeId"],"nonce":d["nonce"],
                "installationSignature":installation.sign(&body)?,"deviceSignature":signing.sign(&body)?,"descriptor":descriptor});
            if rotate {
                let old = self.key(&self.identity.signing_ref, false, false)?;
                request["rotationSignature"] = old
                    .sign(&json!([
                        "minarbot-device-key-transition-v1",
                        byte_digest(&old.public()),
                        body
                    ]))?
                    .into();
            }
            let mut identity = self.identity.clone();
            identity.pending = Some(Pending {
                candidate,
                challenge,
                request,
                signing_ref,
                encryption_ref,
            });
            self.persist(&identity, true)?;
            self.identity = identity;
        }
        let pending = self.identity.pending.clone().ok_or_else(Failure::storage)?;
        if pending.challenge["accountId"] != account {
            return Err(invalid("Pending link belongs to another account."));
        }
        let reply = self
            .human("/api/devices/link", token, Some(&pending.request))
            .await?;
        let d = descriptor(&reply["data"]["descriptor"], true)?;
        if d != pending.request["descriptor"]
            || reply["receipt"]["requestId"] != pending.request["requestId"]
            || reply["receipt"]["actorId"] != account
        {
            return Err(invalid("Enrollment receipt mismatch."));
        }
        let mut identity = self.identity.clone();
        identity.signing_ref = pending.signing_ref;
        identity.encryption_ref = pending.encryption_ref;
        identity.zone = zone(&d["zone"])?;
        identity.zone_revision = revision(&d["zoneRevision"], 1)?;
        identity.descriptor = Some(d.clone());
        identity.pending = None;
        self.persist(&identity, true)?;
        self.identity = identity;
        self.remember_descriptor(&d)?;
        Ok(json!({"deviceId":self.identity.device_id,"generation":d["deviceGeneration"]}))
    }

    fn signed(
        &self,
        path: &str,
        method: Method,
        body: String,
        request: &str,
        token: Option<&str>,
    ) -> Result<RequestBuilder> {
        let d = self
            .identity
            .descriptor
            .as_ref()
            .ok_or_else(|| invalid("Device not enrolled."))?;
        let at = now() / 1000;
        let nonce = B64.encode(keys::entropy::<32>()?);
        let body_digest = byte_digest(body.as_bytes());
        let name = method.as_str();
        let sign = json!([
            "minarbot-device-request-v1",
            self.identity.device_id,
            d["deviceGeneration"],
            d["authorityGeneration"],
            request,
            nonce,
            at,
            at + 60,
            name,
            path,
            body_digest
        ]);
        let proof = json!({"deviceId":self.identity.device_id,"deviceGeneration":d["deviceGeneration"],
            "authorityGeneration":d["authorityGeneration"],"requestId":request,"nonce":nonce,
            "iat":at,"exp":at+60,"method":name,"path":path,"bodyDigest":body_digest,
            "signature":self.key(&self.identity.signing_ref,false,false)?.sign(&sign)?});
        let mut req = self
            .client
            .request(method, format!("{}{path}", self.identity.server))
            .header("content-type", "application/json")
            .header("x-minarbot-protocol", "1")
            .header("x-minarbot-request-id", request)
            .header("x-minarbot-proof", proof.to_string())
            .body(body);
        if let Some(token) = token {
            req = req.header("authorization", format!("Device {token}"));
        }
        Ok(req)
    }
}

async fn submit(request: RequestBuilder) -> Result<Value> {
    submit_bounded(request, LIMIT, TIMEOUT).await
}

async fn submit_bounded(request: RequestBuilder, limit: usize, timeout: Duration) -> Result<Value> {
    response_value(
        request
            .timeout(timeout)
            .send()
            .await
            .map_err(|_| transport())?,
        limit,
    )
    .await
}

async fn response_value(mut response: reqwest::Response, limit: usize) -> Result<Value> {
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| transport())? {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(transport());
        }
        bytes.extend(chunk);
    }
    let value = crate::web::parse_json(&bytes).map_err(|_| transport())?;
    if !(200..300).contains(&status) || value["ok"] != true {
        let code = match text(&value["error"], "code") {
            "rate_limited" => "rate_limited",
            "device_session_invalid" => "device_session_invalid",
            "device_offline" => "device_offline",
            "device_revoked" => "device_revoked",
            "stale_revision" => "stale_revision",
            "context_stale" => "context_stale",
            "action_conflict" => "action_conflict",
            "outcome_unresolved" => "outcome_unresolved",
            "result_invalid" => "result_invalid",
            "setup_incomplete" => "setup_incomplete",
            "task_terminal" => "task_terminal",
            "storage_failed" => "storage_failed",
            "challenge_expired" => "challenge_expired",
            "challenge_used" => "challenge_used",
            "session_invalid" => "session_invalid",
            "invalid_token" => "invalid_token",
            "permission_denied" => "permission_denied",
            "ownership_conflict" => "ownership_conflict",
            "result_missing" => "result_missing",
            "task_not_runnable" => "task_not_runnable",
            "execution_denied" => "execution_denied",
            "event_pending" => "event_pending",
            "question_pending" => "question_pending",
            "waiting_agents" => "waiting_agents",
            "connection_stale" => "connection_stale",
            "record_scope_missing" => "record_scope_missing",
            "resource_not_found" => "resource_not_found",
            _ => "request_denied",
        };
        let mut error = fail(status, code, "Device request denied.");
        error.details = value["error"].get("details").cloned();
        error.retryable = status == 429 || status >= 500;
        return Err(error);
    }
    Ok(value)
}

#[derive(Clone)]
struct Link {
    origin: String,
    host: Arc<AsyncMutex<Host>>,
    token_hash: String,
    expires_at: i64,
    done: Arc<AsyncMutex<Option<oneshot::Sender<()>>>>,
    mode: &'static str,
}

async fn link_request(
    axum::extract::State(link): axum::extract::State<Link>,
    req: Request,
) -> Response {
    let result = async {
        let headers = req.headers();
        let presented = crate::text_header(headers, "authorization").unwrap_or("");
        let denied = || fail(403, "local_proof_denied", "Local proof denied.");
        if req.method() != Method::POST
            || req.uri().path_and_query().map(|p| p.as_str()) != Some("/local/link-proof")
            || crate::text_header(headers, "origin") != Some(&link.origin)
            || crate::text_header(headers, "host") != link.origin.strip_prefix("http://")
            || headers.keys().any(|name| {
                name.as_str().starts_with("forwarded") || name.as_str().starts_with("x-forwarded-")
            })
            || crate::text_header(headers, "content-type").and_then(|v| v.split(';').next())
                != Some("application/json")
            || now() >= link.expires_at
            || !bool::from(
                db::digest(presented)
                    .as_bytes()
                    .ct_eq(link.token_hash.as_bytes()),
            )
        {
            return Err(denied());
        }
        let mut host = link
            .host
            .try_lock()
            .map_err(|_| fail(409, "link_busy", "Enrollment is in progress."))?;
        let bytes = tokio::time::timeout(TIMEOUT, axum::body::to_bytes(req.into_body(), LIMIT))
            .await
            .map_err(|_| fail(408, "link_timeout", "Request timed out."))?
            .map_err(|_| fail(413, "payload_too_large", "Payload is too large."))?;
        let value = crate::web::parse_json(&bytes)?;
        fields(&value, &["requestId", "clerkToken"])?;
        id(&value["requestId"])?;
        let token = value["clerkToken"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 16000 && !s.chars().any(char::is_whitespace))
            .ok_or_else(|| fail(400, "session_invalid", "Invalid Clerk token."))?;
        let mut done = link.done.lock().await;
        if done.as_ref().is_none_or(oneshot::Sender::is_closed) || now() >= link.expires_at {
            return Err(denied());
        }
        let enrolled = host.enroll(token, link.mode).await?;
        if let Some(done) = done.take() {
            let _ = done.send(());
        }
        Ok(enrolled)
    }
    .await;
    let mut response = match result {
        Ok(value) => crate::json_response(200, json!({"ok":true,"data":value})),
        Err(error) => crate::json_response(
            error.status,
            json!({"ok":false,"error":{"code":error.code}}),
        ),
    };
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

async fn bootstrap(host: Host, mode: &'static str) -> db::Result<()> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let token = B64.encode(keys::entropy::<32>().map_err(|err| err.message)?);
    let file = host.directory.join(format!("link-token-{}", request_id()));
    save(&file, token.as_bytes(), false)?;
    let expires_at = now() + 300_000;
    let (done, ended) = oneshot::channel();
    let routes = Router::new().fallback(any(link_request)).with_state(Link {
        origin: origin.clone(),
        host: Arc::new(AsyncMutex::new(host)),
        token_hash: db::digest(&format!("Bearer {token}")),
        expires_at,
        done: Arc::new(AsyncMutex::new(Some(done))),
        mode,
    });
    println!(
        "{}",
        json!({"origin":origin,"tokenFile":file,"expiresAt":expires_at})
    );
    let served = axum::serve(listener, routes).with_graceful_shutdown(async {
        tokio::select! { _ = ended => {}, _ = crate::shutdown() => {}, _ = tokio::time::sleep(Duration::from_secs(300)) => {} }
        let _ = fs::remove_file(file);
    });
    served.await?;
    Ok(())
}

pub async fn run() -> db::Result<()> {
    let mut args = std::env::args().skip(2).peekable();
    let control = args.next_if(|value| value == "--test-control").is_some();
    let enroll = args.next_if(|value| value == "enroll").is_some();
    let export = !enroll && args.next_if(|value| value == "export-recovery").is_some();
    let task = if !enroll && !export && args.next_if(|value| value == "run-task").is_some() {
        Some(args.next().ok_or("Missing task ID")?)
    } else {
        None
    };
    let mut test_provider = None;
    let mut checkpoint = None;
    let mut server = None;
    let mut mode = "new";
    let mut fixture_keys = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--server" if server.is_none() => server = args.next(),
            "--test-keyring" if fixture_keys.is_none() => {
                fixture_keys = Some(PathBuf::from(
                    args.next().ok_or("Missing fixture keyring path")?,
                ));
            }
            "--test-checkpoint" if task.is_some() && checkpoint.is_none() => {
                checkpoint = Some(args.next().ok_or("Missing fixture checkpoint")?);
            }
            "--test-provider" if task.is_some() && test_provider.is_none() => {
                test_provider = Some(args.next().ok_or("Missing fixture provider URL")?);
            }
            "--rotate" if enroll && mode == "new" => mode = "rotate",
            "--reenroll" if enroll && mode == "new" => mode = "reenroll",
            _ => {
                return Err(
                    "Use --device-host [enroll | export-recovery | run-task <id>] --server <origin> [--rotate | --reenroll]".into(),
                )
            }
        }
    }
    let server = server.ok_or("Missing --server <origin>")?;
    let database = std::env::var_os("MINARBOT_DEVICE_DB").ok_or("Missing MINARBOT_DEVICE_DB")?;
    let development = std::env::var("MINARBOT_DEVELOPMENT").as_deref() == Ok("1");
    let testing = fixture_keys.is_some();
    if control && (!testing || !development || enroll || export || task.is_some()) {
        return Err(
            "Fixture control requires a loopback Rust test server and fixture keyring".into(),
        );
    }
    let keys: Box<dyn keys::Store> = if let Some(path) = fixture_keys {
        Box::new(fixture_keys::FixtureKeys::open(path, &server)?)
    } else {
        Box::new(keys::System)
    };
    let mut host = Host::open(Path::new(&database), &server, development, keys)?;
    if control {
        fixture_control::run(host, Path::new(&database)).await
    } else if enroll {
        bootstrap(host, mode).await
    } else if export {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take((LIMIT + 1) as u64)
            .read_to_end(&mut bytes)?;
        let result = if bytes.len() > LIMIT {
            Err(fail(413, "payload_too_large", "Payload is too large."))
        } else {
            crate::web::parse_json(&bytes).and_then(|value| host.export_recovery(&value))
        };
        command_output(result)
    } else if let Some(task) = task {
        if checkpoint.is_some() && (!testing || test_provider.is_none()) {
            return Err("Fixture checkpoint requires a loopback fixture provider".into());
        }
        host.fixture_checkpoint = checkpoint;
        let provider = Arc::new(provider::Provider::new(test_provider, testing)?);
        host.provider = Some(provider.clone());
        let result = async {
            id(&json!(task))?;
            host.acquire_executor()?;
            let (session, frames) = tokio::time::timeout(TIMEOUT, host.open_stream())
                .await
                .map_err(|_| transport())??;
            let mut observer = host.stream_peer(Path::new(&database))?;
            tokio::select! {
                biased;
                _ = crate::shutdown() => Err(transport()),
                error = stream::observe_task(&mut observer, &session, frames) => Err(error),
                result = async {
                    host.recover_journal(&session.token).await?;
                    host.run_task(&session.token, &task, &provider).await
                } => result,
            }
        }
        .await;
        command_output(result)
    } else {
        if !testing || std::env::var_os("MINARBOT_DATA_DIR").is_some() {
            host.provider = Some(Arc::new(provider::Provider::new(None, testing)?));
        }
        stream::run(host).await
    }
}

fn command_output(result: Result<Value>) -> db::Result<()> {
    match result {
        Ok(value) => {
            println!("{}", value);
            Ok(())
        }
        Err(error) => {
            eprintln!(
                "{}",
                json!({"status":error.status,"code":error.code,"message":error.message})
            );
            Err("Device command failed".into())
        }
    }
}
