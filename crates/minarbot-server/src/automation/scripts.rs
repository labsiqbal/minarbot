use super::*;
use axum::{
    body::Body,
    http::{header, HeaderValue},
    response::IntoResponse,
};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, DirBuilder, Metadata, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use subtle::ConstantTimeEq;

#[derive(Clone)]
pub(crate) struct UploadAgent(pub String);

pub struct Discovery {
    directory: PathBuf,
    lock: Metadata,
    connection: Option<Metadata>,
}
fn remove_owned(path: &Path, metadata: &Metadata) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(current) if current.dev() == metadata.dev() && current.ino() == metadata.ino() => {
            fs::remove_file(path)
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
impl Drop for Discovery {
    fn drop(&mut self) {
        if let Some(owned) = &self.connection {
            let _ = remove_owned(&self.directory.join("connection.json"), owned);
        }
        let _ = remove_owned(&self.directory.join("owner.lock"), &self.lock);
    }
}
pub struct DiscoveryGuard(Arc<Mutex<Option<Discovery>>>);
impl Drop for DiscoveryGuard {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.0.lock() {
            slot.take();
        }
    }
}
pub fn open(config: &crate::config::Config) -> crate::db::Result<DiscoveryGuard> {
    let slot = config.automation.discovery.clone();
    if let Some(path) = &config.automation.script_directory {
        match DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.into()),
        }
        let meta = fs::symlink_metadata(path)?;
        if !meta.is_dir()
            || meta.uid() != rustix::process::geteuid().as_raw()
            || meta.mode() & 0o077 != 0
        {
            return Err("Script discovery directory must be private and owned by this user".into());
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(path.join("owner.lock"))?;
        let discovery = Discovery {
            directory: path.clone(),
            lock: file.metadata()?,
            connection: None,
        };
        writeln!(file, "{}", std::process::id())?;
        *slot.lock().map_err(|_| "Discovery lock poisoned")? = Some(discovery);
    }
    Ok(DiscoveryGuard(slot))
}
pub fn sync(state: &AppState) -> Result<()> {
    if state.auth.is_some() || state.config.automation.script_directory.is_none() {
        return Ok(());
    }
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let enabled = data::exists(
        &guard.connection,
        "SELECT 1 FROM script_permissions WHERE enabled=1 LIMIT 1",
        [],
    )?;
    let mut slot = state
        .config
        .automation
        .discovery
        .lock()
        .map_err(|_| Failure::storage())?;
    let Some(discovery) = slot.as_mut() else {
        return Ok(());
    };
    discovery
        .sync(
            enabled,
            &state.origin,
            state
                .config
                .remaining_data
                .script_token
                .as_deref()
                .unwrap_or(""),
        )
        .map_err(|_| Failure::storage())
}
impl Discovery {
    fn sync(&mut self, enabled: bool, origin: &str, token: &str) -> std::io::Result<()> {
        let path = self.directory.join("connection.json");
        if !enabled {
            if let Some(owned) = &self.connection {
                remove_owned(&path, owned)?;
            }
            self.connection = None;
        } else if self.connection.is_none() {
            let staged = self
                .directory
                .join(format!(".{}.json", uuid::Uuid::new_v4()));
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&staged)?;
            let owned = output.metadata()?;
            let result: std::io::Result<()> = (|| {
                writeln!(output, "{}", json!({"url":origin,"token":token}))?;
                fs::hard_link(&staged, &path)?;
                self.connection = Some(owned.clone());
                Ok(())
            })();
            remove_owned(&staged, &owned)?;
            result?;
        }
        Ok(())
    }
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/scripts/agents", any(entry))
        .route("/api/scripts/agents/{agent}/routines", any(entry))
        .route("/api/scripts/routines/{id}/run", any(entry))
        .route("/api/scripts/routines/{id}/runs", any(entry))
        .route("/api/scripts/agents/{agent}/workflows", any(entry))
        .route("/api/scripts/agents/{agent}/workflows/{id}/run", any(entry))
        .route(
            "/api/scripts/agents/{agent}/workflows/{id}/schedule",
            any(entry),
        )
        .route(
            "/api/scripts/agents/{agent}/workflows/{id}/webhook",
            any(entry),
        )
        .route("/api/scripts/agents/{agent}/workflow-runs/{id}", any(entry))
        .route(
            "/api/scripts/agents/{agent}/workflow-runs/{id}/approve",
            any(entry),
        )
        .route(
            "/api/scripts/agents/{agent}/workflow-runs/{id}/cancel",
            any(entry),
        )
        .route(
            "/api/scripts/agents/{agent}/workflow-runs/{id}/retry",
            any(entry),
        )
        .route(
            "/api/scripts/agents/{agent}/workflow-runs/{id}/files/{file}",
            any(entry),
        )
        .route("/api/scripts/agents/{agent}/approvals", any(entry))
        .route("/api/scripts/agents/{agent}/provider", any(entry))
        .route("/api/scripts/agents/{agent}/models", any(entry))
        .route("/api/scripts/agents/{agent}/turns", any(entry))
        .route(
            "/api/scripts/agents/{agent}/turns/{turn}/cancel",
            any(entry),
        )
        .route(
            "/api/scripts/agents/{agent}/turns/{turn}/questions/{question}/answer",
            any(entry),
        )
        .route("/api/scripts/agents/{agent}/threads", any(entry))
        .route(
            "/api/scripts/agents/{agent}/threads/{thread}/turns",
            any(entry),
        )
        .route(
            "/api/scripts/agents/{agent}/threads/{thread}/turns/{turn}/cancel",
            any(entry),
        )
        .route(
            "/api/scripts/agents/{agent}/threads/{thread}/turns/{turn}/questions/{question}/answer",
            any(entry),
        )
        .route(
            "/api/scripts/agents/{agent}/threads/{thread}/files",
            any(entry),
        )
        .route(
            "/api/scripts/agents/{agent}/threads/{thread}/files/{file}",
            any(entry),
        )
        .route(
            "/api/scripts/agents/{agent}/threads/{thread}/turns/{turn}/files",
            any(entry),
        )
        .route(
            "/api/scripts/agents/{agent}/threads/{thread}/turns/{turn}/files/{file}",
            any(entry),
        )
}
async fn entry(State(state): State<AppState>, request: Request) -> Response {
    let result = local(&state, request).await;
    let mut response = result.unwrap_or_else(|e| crate::local_error(e.status, &e.message));
    if let Some(token) = &state.config.remaining_data.script_token {
        if crate::text_header(response.headers(), "content-type")
            .is_some_and(|v| v.starts_with("application/json"))
        {
            let (parts, body) = response.into_parts();
            response = match axum::body::to_bytes(body, 16 * 1024 * 1024).await {
                Ok(bytes) => Response::from_parts(
                    parts,
                    Body::from(String::from_utf8_lossy(&bytes).replace(token, "[redacted]")),
                ),
                Err(_) => crate::local_error(500, "Internal storage error."),
            };
            response.headers_mut().remove(header::CONTENT_LENGTH);
        }
    }
    crate::security_headers(&mut response);
    response
}
pub(crate) fn allowed(db: &Connection, agent: &str) -> Result<()> {
    if !data::exists(
        db,
        "SELECT 1 FROM script_permissions WHERE agent=? AND enabled=1",
        [agent],
    )? {
        return Err(error(403, "Agent script permission is inactive."));
    }
    Ok(())
}
fn admit(db: &Connection, agent: &str, id: &str, now: i64) -> Result<()> {
    db.execute(
        "DELETE FROM script_admissions WHERE created<?",
        [now - 60000],
    )?;
    if db.query_row(
        "SELECT count(*) FROM script_admissions WHERE agent=?",
        [agent],
        |r| r.get::<_, i64>(0),
    )? >= 60
    {
        return Err(error(
            429,
            "Too many script requests. Try again in one minute.",
        ));
    }
    if data::exists(db, "SELECT 1 FROM script_admissions WHERE id=?", [id])? {
        return Err(error(
            409,
            "Script ID was already used for a different operation.",
        ));
    }
    db.execute(
        "INSERT INTO script_admissions VALUES(?,?,?)",
        params![id, agent, now],
    )?;
    Ok(())
}
pub(crate) async fn local(state: &AppState, request: Request) -> Result<Response> {
    if request.method() == Method::DELETE {
        return Err(error(
            403,
            "File deletion requires the local owner in Minarbot.",
        ));
    }
    let host = crate::text_header(request.headers(), "host").unwrap_or("");
    if state.auth.is_some()
        || format!("http://{host}") != state.origin
            && !state
                .config
                .allowed_origins
                .contains(&format!("https://{host}"))
        || crate::text_header(request.headers(), "sec-fetch-site") == Some("cross-site")
    {
        return Err(error(
            403,
            "Access is only allowed through the local Minarbot address.",
        ));
    }
    if crate::updates::pending(state) {
        return Err(error(503, "Update is being verified. Retry after restart."));
    }
    let token = state
        .config
        .remaining_data
        .script_token
        .as_deref()
        .ok_or_else(|| error(403, "Script access denied."))?;
    let provided = Sha256::digest(
        crate::text_header(request.headers(), "authorization")
            .unwrap_or("")
            .as_bytes(),
    );
    let expected = Sha256::digest(format!("Bearer {token}").as_bytes());
    if request.headers().contains_key("origin")
        || request.headers().get_all("authorization").iter().count() != 1
        || !bool::from(provided.ct_eq(&expected))
    {
        return Err(error(403, "Script access denied."));
    }
    let path = request.uri().path().to_owned();
    let p = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    if request.method() == Method::POST {
        if let ["api", "scripts", "agents", agent, "threads", thread, "files", file] = p.as_slice()
        {
            {
                let guard = state.database.lock().map_err(|_| Failure::storage())?;
                allowed(&guard.connection, agent)?;
                if !data::exists(
                    &guard.connection,
                    "SELECT 1 FROM threads WHERE id=? AND agent=? AND deleted=0",
                    params![thread, agent],
                )? {
                    return Err(error(404, "Side thread not found for this agent."));
                }
            }
            if data::param(&data::query(&request), "transcription") == Some("1") {
                return Err(error(
                    403,
                    "Transcription requires user approval in Minarbot.",
                ));
            }
            let (mut parts, body) = request.into_parts();
            parts.extensions.insert(UploadAgent((*agent).to_owned()));
            parts.headers.remove(header::AUTHORIZATION);
            parts.headers.insert(
                header::ORIGIN,
                HeaderValue::from_str(&state.origin).map_err(|_| Failure::storage())?,
            );
            parts.uri = format!("/api/threads/{thread}/files/{file}")
                .parse()
                .map_err(|_| Failure::storage())?;
            let response =
                crate::files::entry(State(state.clone()), Request::from_parts(parts, body)).await;
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            allowed(&guard.connection, agent)?;
            crate::files::context(&guard.connection, "threads", thread)?;
            return Ok(response);
        }
    }
    let q = data::query(&request);
    let (mut parts, raw) = request.into_parts();
    let post = parts.method == Method::POST;
    let body = if post {
        if crate::text_header(&parts.headers, "content-type")
            .unwrap_or("")
            .split(';')
            .next()
            != Some("application/json")
        {
            return Err(error(403, "Request must originate from Minarbot."));
        }
        let bytes = axum::body::to_bytes(raw, 64000)
            .await
            .map_err(|_| error(413, "Request is too large."))?;
        let v: Value = serde_json::from_slice(&bytes).map_err(|_| error(400, "Invalid JSON."))?;
        if !v.is_object() {
            return Err(error(400, "JSON object is required."));
        }
        if !v["id"].as_str().is_some_and(uuid) {
            return Err(error(400, "Invalid request ID."));
        }
        v
    } else {
        json!({})
    };
    let (mapped, body) = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        if p == ["api", "scripts", "agents"] && !post {
            let after = data::param(&q, "after").unwrap_or("");
            if q.iter().any(|(k, _)| k != "after") || !after.is_empty() && !uuid(after) {
                return Err(error(400, "Invalid agent cursor."));
            }
            let mut rows = all(db,"SELECT a.id,a.name,a.model FROM agents a JOIN script_permissions p ON p.agent=a.id WHERE p.enabled=1 AND a.id>? ORDER BY a.id LIMIT 101",[after])?;
            let next = if rows.len() > 100 {
                rows[99]["id"].clone()
            } else {
                Value::Null
            };
            rows.truncate(100);
            for r in &mut rows {
                r["hidden"] = json!(data::agents::hidden(db, string(r, "id"))?);
            }
            return Ok(crate::json_response(
                200,
                json!({"agents":rows,"next_after":next}),
            ));
        }
        if let ["api", "scripts", "routines", id, action] = p.as_slice() {
            let routine = one(db, "SELECT agent FROM routines WHERE id=?", [id])?
                .ok_or_else(|| error(404, "Routine not found."))?;
            let agent = string(&routine, "agent");
            allowed(db, agent)?;
            if *action == "runs" && !post {
                return Ok(crate::json_response(200,json!(all(db,"SELECT routine_runs.*,turns.status,turns.paused,turns.no_update,turns.reply,turns.error,turns.created FROM routine_runs JOIN turns ON turns.id=routine_runs.turn JOIN threads ON threads.id=turns.thread WHERE routine=? AND threads.deleted=0 ORDER BY turns.seq DESC LIMIT 100",[id])?)));
            }
            if *action == "run" && post {
                if !integer(&body["revision"], 1, 9007199254740991)
                    || body["payload"]
                        .as_str()
                        .is_none_or(|s| s.encode_utf16().count() > 16000)
                {
                    return Err(error(400, "Invalid script request."));
                }
                let value = transaction(db, || {
                    command(db, "routine_commands", &path, &body, || {
                        let r = routines::read(db, id)?;
                        if r["revision"] != body["revision"] {
                            return Err(error(409, "Routine changed. Reload before testing."));
                        }
                        admit(db, agent, string(&body, "id"), state.config.now())?;
                        routines::enqueue_routine(state, db, &r, "script", string(&body, "payload"))
                    })
                })?;
                return Ok(crate::json_response(200, value));
            }
            return Err(error(404, "Route not found."));
        }
        let ["api", "scripts", "agents", agent, tail @ ..] = p.as_slice() else {
            return Err(error(404, "Route not found."));
        };
        allowed(db, agent)?;
        if data::param(&q, "transcription") == Some("1") && tail.contains(&"files") {
            return Err(error(
                403,
                "Transcription requires user approval in Minarbot.",
            ));
        }
        let provider_question = match tail {
            ["turns", turn, "questions", question, "answer"] => data::exists(
                db,
                "SELECT 1 FROM provider_account_changes p JOIN turns t ON t.id=(SELECT turn FROM questions WHERE id=p.question) WHERE p.question=? AND p.agent=? AND t.id=? AND t.channel IS NULL AND t.thread IS NULL",
                params![question, agent, turn],
            )?,
            ["threads", thread, "turns", turn, "questions", question, "answer"] => data::exists(
                db,
                "SELECT 1 FROM provider_account_changes p JOIN turns t ON t.id=(SELECT turn FROM questions WHERE id=p.question) JOIN threads s ON s.id=t.thread WHERE p.question=? AND p.agent=? AND t.id=? AND t.thread=? AND s.agent=? AND s.deleted=0",
                params![question, agent, turn, thread, agent],
            )?,
            _ => false,
        };
        if provider_question {
            return Err(error(
                403,
                "Account changes can only be confirmed in Minarbot.",
            ));
        }
        let get = parts.method == Method::GET;
        let message = match tail {
            ["turns", _, "cancel"] | ["threads", _, "turns", _, "cancel"] if !post => {
                Some("Cancellation requires POST.")
            }
            ["turns", _, "questions", _, "answer"]
            | ["threads", _, "turns", _, "questions", _, "answer"]
                if !post =>
            {
                Some("Question answer requires POST.")
            }
            ["models"] if !get => Some("Model catalog requires GET."),
            ["approvals"] if !get => Some("Approval list requires GET."),
            ["workflows"] | ["workflow-runs", _] | ["workflow-runs", _, "files", _] if !get => {
                Some("Reading a workflow requires GET.")
            }
            _ => None,
        };
        if let Some(message) = message {
            return Err(error(405, message));
        }
        let method = &parts.method;
        match tail {
            ["routines"] if !post => {
                return Ok(crate::json_response(
                    200,
                    routines::route(
                        state,
                        db,
                        &format!("/api/agents/{agent}/routines"),
                        &["api", "agents", agent, "routines"],
                        method,
                        &q,
                        &parts.headers,
                        &body,
                    )?
                    .1,
                ))
            }
            ["workflows"] if !post => {
                let after = data::param(&q, "after").unwrap_or("");
                if !after.is_empty() && !uuid(after) {
                    return Err(error(400, "Invalid workflow cursor."));
                }
                let mut rows = all(db,"SELECT id,name,revision FROM workflows WHERE agent=? AND deleted=0 AND id>? ORDER BY id LIMIT 21",params![agent,after])?;
                let next = if rows.len() > 20 {
                    rows[19]["id"].clone()
                } else {
                    Value::Null
                };
                rows.truncate(20);
                return Ok(crate::json_response(
                    200,
                    json!({"workflows":rows,"next_after":next}),
                ));
            }
            ["approvals"] if !post => {
                let after = data::param(&q, "after").unwrap_or("");
                if !after.is_empty() && !uuid(after) {
                    return Err(error(400, "Invalid approval cursor."));
                }
                let rows=all(db,"SELECT r.* FROM workflow_runs r JOIN workflows w ON w.id=r.workflow WHERE r.agent=? AND r.status='approval' AND w.deleted=0 AND r.id>? ORDER BY r.id LIMIT 51",params![agent,after])?;
                let next = if rows.len() > 50 {
                    rows[49]["id"].clone()
                } else {
                    Value::Null
                };
                let mut values = Vec::new();
                for r in rows.into_iter().take(50) {
                    let g = data::json(&r["graph"])?;
                    let prompt = g["nodes"]
                        .as_array()
                        .and_then(|ns| ns.iter().find(|n| n["id"] == r["cursor"]))
                        .map(|n| n["prompt"].clone())
                        .unwrap_or(Value::Null);
                    values.push(json!({"run":r["id"],"revision":r["revision"],"step":r["cursor"],"prompt":prompt,"input":r["input"],"result":r["last"]}));
                }
                return Ok(crate::json_response(
                    200,
                    json!({"approvals":values,"next_after":next}),
                ));
            }
            ["provider"] if !post => {
                let a = data::agents::read(db, agent)?;
                let accounts=data::providers::accounts(state)?.into_iter().map(|a|json!({"id":a.id,"name":a.name,"provider":a.family,"configured":!a.base.is_empty(),"defaultModel":a.model})).collect::<Vec<_>>();
                return Ok(crate::json_response(
                    200,
                    json!({"revision":a["revision"],"model":a["model"],"backupModel":a["backup_model"],"providerAccount":a["provider_account"],"accounts":accounts}),
                ));
            }
            ["workflow-runs", id, rest @ ..] => {
                let r=one(db,"SELECT r.*,w.deleted FROM workflow_runs r JOIN workflows w ON w.id=r.workflow WHERE r.id=? AND r.agent=?",params![id,agent])?.filter(|r|r["deleted"]==0).ok_or_else(||error(404,"Approval not found for this agent."))?;
                if (rest.is_empty() || matches!(rest, ["files", _])) && !post {
                    return workflow_result(db, &r, rest, &q);
                }
                let [action @ ("approve" | "cancel" | "retry")] = rest else {
                    return Err(error(405, "Approval decision requires POST."));
                };
                if !post {
                    return Err(error(405, "Approval decision requires POST."));
                }
                let fields = if *action == "approve" {
                    &["id", "revision", "accepted"][..]
                } else {
                    &["id", "revision"][..]
                };
                if !data::closed(&body, fields) {
                    return Err(error(400, "Invalid approval decision."));
                }
                (format!("/api/workflow-runs/{id}/{action}"), body)
            }
            ["workflows", id, action @ ("run" | "schedule" | "webhook")] => {
                if !data::exists(
                    db,
                    "SELECT 1 FROM workflows WHERE id=? AND agent=? AND deleted=0",
                    params![id, agent],
                )? {
                    return Err(error(404, "Workflow not found for this agent."));
                }
                if *action == "run" && !post {
                    return Err(error(405, "Running a workflow requires POST."));
                }
                let mapped = format!("/api/workflows/{id}/{action}");
                let mapped_parts = mapped
                    .trim_start_matches('/')
                    .split('/')
                    .collect::<Vec<_>>();
                if *action == "webhook" || *action == "schedule" {
                    let (status, value) = transaction(db, || {
                        if *action == "webhook" {
                            webhooks::settings(state, db, &mapped, &mapped_parts, method, &body)
                        } else {
                            workflows::route(
                                state,
                                db,
                                &mapped,
                                &mapped_parts,
                                method,
                                &q,
                                &parts.headers,
                                &body,
                            )
                        }
                    })?;
                    let mut value = value;
                    if *action == "webhook" && !post {
                        if let Some(map) = value.as_object_mut() {
                            map.remove("url");
                        }
                    }
                    return Ok(crate::json_response(status, value));
                }
                if !data::closed(&body, &["id", "revision", "input"]) {
                    return Err(error(400, "Invalid workflow request."));
                }
                let (status, value) = transaction(db, || {
                    if !data::exists(
                        db,
                        "SELECT 1 FROM workflow_commands WHERE id=?",
                        [string(&body, "id")],
                    )? {
                        admit(db, agent, string(&body, "id"), state.config.now())?;
                    }
                    workflows::route(
                        state,
                        db,
                        &mapped,
                        &mapped_parts,
                        method,
                        &q,
                        &parts.headers,
                        &body,
                    )
                })?;
                if let Err(e) = transaction(db, || workflows::settle(state, db)) {
                    eprintln!("Workflow scheduling failed: {}", e.message);
                }
                return Ok(crate::json_response(status, value));
            }
            ["threads", thread, rest @ ..] => {
                if !data::exists(
                    db,
                    "SELECT 1 FROM threads WHERE id=? AND agent=? AND deleted=0",
                    params![thread, agent],
                )? {
                    return Err(error(404, "Side thread not found for this agent."));
                }
                if let ["turns", turn, "files", file @ ..] = rest {
                    if post {
                        return Err(error(405, "Reading a workflow requires GET."));
                    }
                    if !data::exists(db,"SELECT 1 FROM turns WHERE id=? AND agent=? AND thread=? AND channel IS NULL",params![turn,agent,thread])? {return Err(error(404,"Task not found in this conversation."));}
                    return result_files(
                        db,
                        crate::turns::requested_files(db, turn)?,
                        file.first().copied(),
                        &q,
                    );
                }
                if !matches!(
                    rest,
                    ["turns"]
                        | ["turns", _, "cancel"]
                        | ["turns", _, "questions", _, "answer"]
                        | ["files"]
                        | ["files", _]
                ) {
                    return Err(error(404, "Route not found."));
                }
                (format!("/api/threads/{thread}/{}", rest.join("/")), body)
            }
            ["turns"]
            | ["turns", _, "cancel"]
            | ["turns", _, "questions", _, "answer"]
            | ["threads"] => (path.replacen("/api/scripts/", "/api/", 1), body),
            ["models"] => {
                if q.iter().any(|(k, _)| k != "account") {
                    return Err(error(400, "Invalid catalog parameters."));
                }
                let a = data::agents::read(db, agent)?;
                let account = data::param(&q, "account").unwrap_or(string(&a, "provider_account"));
                (
                    format!(
                        "/api/models?account={}",
                        url::form_urlencoded::byte_serialize(account.as_bytes())
                            .collect::<String>()
                    ),
                    body,
                )
            }
            ["provider"] if post => {
                if !data::closed(
                    &body,
                    &["id", "revision", "model", "backupModel", "providerAccount"],
                ) {
                    return Err(error(400, "Invalid provider settings."));
                }
                let a = data::agents::read(db, agent)?;
                let mut v = body;
                v["scriptProvider"] = json!(true);
                for key in ["name", "instructions"] {
                    v[key] = a[key].clone();
                }
                (format!("/api/agents/{agent}/edit"), v)
            }
            _ => return Err(error(404, "Route not found.")),
        }
    };
    if mapped.starts_with("/api/models?") {
        let account = url::form_urlencoded::parse(
            mapped
                .split_once('?')
                .map(|(_, q)| q)
                .unwrap_or("")
                .as_bytes(),
        )
        .find(|(k, _)| k == "account")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default();
        let result = data::providers::models(state, &[("account".into(), account.clone())]).await;
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        allowed(&guard.connection, p[3])?;
        let mut value = result?;
        value["account"] = json!(account);
        return Ok(crate::json_response(200, value));
    }
    if post && mapped.ends_with("/turns") {
        let mapped_parts = mapped
            .trim_start_matches('/')
            .split('/')
            .collect::<Vec<_>>();
        crate::turns::prepare_script(state, &mapped_parts, &Method::POST, &body).await?;
        let (status, value) = {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            let db = &guard.connection;
            transaction(db, || {
                allowed(db, p[3])?;
                if !data::exists(db, "SELECT 1 FROM turns WHERE id=?", [string(&body, "id")])? {
                    admit(db, p[3], string(&body, "id"), state.config.now())?;
                }
                crate::turns::script_route(
                    state,
                    db,
                    &mapped,
                    &mapped_parts,
                    &Method::POST,
                    &q,
                    &body,
                )
            })?
        };
        crate::turns::start_queued(state, string(&body, "id")).await?;
        return Ok(crate::json_response(status, value));
    }
    parts.headers.remove(header::AUTHORIZATION);
    parts.headers.insert(
        header::ORIGIN,
        HeaderValue::from_str(&state.origin).map_err(|_| Failure::storage())?,
    );
    let suffix = parts
        .uri
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let target = if mapped.contains('?') {
        mapped.clone()
    } else {
        format!("{mapped}{suffix}")
    };
    parts.uri = target.parse().map_err(|_| Failure::storage())?;
    let req = Request::from_parts(
        parts,
        if post {
            Body::from(body.to_string())
        } else {
            Body::empty()
        },
    );
    if mapped.starts_with("/api/workflow-runs/") {
        return super::local(state, req).await;
    }
    if mapped.contains("/files") {
        let response = crate::files::entry(State(state.clone()), req).await;
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        allowed(&guard.connection, p[3])?;
        let context = mapped.split('/').collect::<Vec<_>>();
        crate::files::context(&guard.connection, context[2], context[3])?;
        return Ok(response);
    }
    let response = crate::local(state, req).await?;
    if !post && mapped.starts_with("/api/threads/") && mapped.ends_with("/turns") {
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
            .await
            .map_err(|_| Failure::storage())?;
        let mut rows: Value = serde_json::from_slice(&bytes).map_err(|_| Failure::storage())?;
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        allowed(&guard.connection, p[3])?;
        for row in rows.as_array_mut().into_iter().flatten() {
            row["generatedFiles"] = json!(crate::turns::requested_files(
                &guard.connection,
                string(row, "id"),
            )?);
        }
        return Ok(crate::json_response(status.as_u16(), rows));
    }
    Ok(response)
}
fn workflow_result(
    db: &Connection,
    run: &Value,
    rest: &[&str],
    q: &[(String, String)],
) -> Result<Response> {
    let mut files = Vec::new();
    for step in all(
        db,
        "SELECT turn FROM workflow_steps WHERE run=? AND turn IS NOT NULL ORDER BY seq",
        [string(run, "id")],
    )? {
        for file in crate::turns::requested_files(db, string(&step, "turn"))? {
            if !files.iter().any(|f: &Value| f["id"] == file["id"]) {
                files.push(file);
            }
        }
    }
    if !rest.is_empty() {
        return result_files(db, files, rest.get(1).copied(), q);
    }
    let mut value = json!({"id":run["id"],"version":run["version"],"status":run["status"],"revision":run["revision"],"error":run["error"],"result":run["last"],"generatedFiles":files});
    if run["status"] == "approval" {
        let graph = data::json(&run["graph"])?;
        value["prompt"] = graph["nodes"]
            .as_array()
            .and_then(|ns| ns.iter().find(|n| n["id"] == run["cursor"]))
            .map(|n| n["prompt"].clone())
            .unwrap_or(Value::Null);
    }
    Ok(crate::json_response(200, value))
}
fn result_files(
    db: &Connection,
    files: Vec<Value>,
    id: Option<&str>,
    q: &[(String, String)],
) -> Result<Response> {
    let Some(id) = id else {
        return Ok(crate::json_response(200, json!(files)));
    };
    if !files.iter().any(|f| f["id"] == id && f["deleted"] != true) {
        return Err(error(404, "Task result file not found."));
    }
    let (name, bytes): (String, Vec<u8>) =
        db.query_row("SELECT name,bytes FROM files WHERE id=?", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    if data::param(q, "metadata") == Some("1") {
        return Ok(crate::json_response(
            200,
            json!({"id":id,"name":name,"size":bytes.len()}),
        ));
    }
    let name = url::form_urlencoded::byte_serialize(name.as_bytes())
        .collect::<String>()
        .replace('+', "%20");
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"download\"; filename*=UTF-8''{name}"),
            ),
            (header::CONTENT_LENGTH, bytes.len().to_string()),
        ],
        bytes,
    )
        .into_response())
}
