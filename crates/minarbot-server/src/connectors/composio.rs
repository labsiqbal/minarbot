use super::*;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    time::Duration,
};

const UNCERTAIN:&str="Connected app result is uncertain. Check the target app before starting another task. This action will not be repeated.";
fn num(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}
fn hash(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}
pub fn tool_name(slug: &str) -> String {
    format!("composio__{}", &hash(&json!(slug))[..24])
}
fn fields(v: &Value, names: &[&str]) -> Result<()> {
    if !closed(v, names) {
        return Err(error(400, "Unknown connected apps field."));
    }
    Ok(())
}
fn bounded(v: &Value, max: usize) -> Result<&str> {
    v.as_str()
        .filter(|s| s.encode_utf16().count() <= max && !s.chars().any(|c| c < ' ' || c == '\u{7f}'))
        .ok_or_else(|| error(400, "Invalid connected apps field."))
}
fn slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}
fn lock(state: &AppState) -> Result<std::sync::MutexGuard<'_, crate::db::Database>> {
    state.database.lock().map_err(|_| Failure::storage())
}
fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    getrandom::getrandom(&mut bytes).map_err(|_| Failure::storage())?;
    Ok(bytes)
}
pub fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(include_str!("composio.sql"))?;
    db.execute(
        "INSERT OR IGNORE INTO composio_installation VALUES(1,?)",
        [format!("minarbot-{}", uuid::Uuid::new_v4())],
    )?;
    Ok(())
}
fn file_key(state: &AppState, create: bool) -> Result<Vec<u8>> {
    let path = minarbot_launcher::files::sidecar(&state.config.database, ".composio-key");
    if create {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut f) => {
                f.write_all(&random::<32>()?)
                    .map_err(|_| Failure::storage())?;
                f.sync_all().map_err(|_| Failure::storage())?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(_) => return Err(Failure::storage()),
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)
        .map_err(|_| Failure::storage())?;
    let stat = file.metadata().map_err(|_| Failure::storage())?;
    if !stat.is_file()
        || stat.nlink() != 1
        || stat.uid() != rustix::process::geteuid().as_raw()
        || stat.mode() & 0o077 != 0
        || stat.len() != 32
    {
        return Err(Failure::storage());
    }
    let mut bytes = vec![];
    file.take(33)
        .read_to_end(&mut bytes)
        .map_err(|_| Failure::storage())?;
    if bytes.len() != 32 {
        return Err(Failure::storage());
    }
    Ok(bytes)
}
fn encrypt(state: &AppState, text: &str) -> Result<String> {
    let key = LessSafeKey::new(
        UnboundKey::new(&AES_256_GCM, &file_key(state, true)?).map_err(|_| Failure::storage())?,
    );
    let nonce = random::<12>()?;
    let mut bytes = text.as_bytes().to_vec();
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::from(b"composio-api-key"),
        &mut bytes,
    )
    .map_err(|_| Failure::storage())?;
    Ok(B64.encode([nonce.to_vec(), bytes].concat()))
}
pub(crate) fn decrypt(state: &AppState, text: &str) -> Result<String> {
    let run = || -> Result<String> {
        let mut bytes = B64.decode(text).map_err(|_| Failure::storage())?;
        if bytes.len() < 28 {
            return Err(Failure::storage());
        }
        let nonce: [u8; 12] = bytes[..12].try_into().map_err(|_| Failure::storage())?;
        let key = LessSafeKey::new(
            UnboundKey::new(&AES_256_GCM, &file_key(state, false)?)
                .map_err(|_| Failure::storage())?,
        );
        let plain = key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(b"composio-api-key"),
                &mut bytes[12..],
            )
            .map_err(|_| Failure::storage())?;
        String::from_utf8(plain.to_vec()).map_err(|_| Failure::storage())
    };
    run().map_err(|_| {
        error(
            409,
            "Stored Composio key is unavailable. Save a new key on this host.",
        )
    })
}
async fn environment_key(state: &AppState) -> Result<String> {
    let key = if let Some(bridge) = &state.config.connectors.test_connector_bridge {
        let response = client()?
            .get(format!("{}/composio-key", bridge.origin))
            .bearer_auth(&bridge.token)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map_err(|_| Failure::storage())?;
        let data = read(response, 8192).await?;
        string(&data, "key").to_owned()
    } else if std::env::var("MINARBOT_TEST_SERVER").as_deref() == Ok("rust") {
        String::new()
    } else {
        std::env::var("COMPOSIO_API_KEY").unwrap_or_default()
    };
    if key.len() > 4096 || key.chars().any(|c| c < ' ' || c == '\u{7f}') {
        return Err(error(
            400,
            "COMPOSIO_API_KEY must be a single header-safe value, up to 4096 characters.",
        ));
    }
    Ok(key)
}
async fn current(state: &AppState) -> Result<(String, Value)> {
    let row = one(
        &lock(state)?.connection,
        "SELECT revision,ciphertext FROM composio_settings WHERE id=1",
        [],
    )?;
    let revision = row.as_ref().map(|r| num(r, "revision")).unwrap_or(0);
    if let Some(row) = row {
        if !string(&row, "ciphertext").is_empty() {
            return Ok((
                decrypt(state, string(&row, "ciphertext"))?,
                json!({"source":"settings","revision":revision}),
            ));
        }
    }
    let key = environment_key(state).await?;
    let source = if key.is_empty() {
        "none"
    } else {
        "environment"
    };
    Ok((key, json!({"source":source,"revision":revision})))
}
pub fn redact(v: &mut Value, key: &str) {
    if key.is_empty() {
        return;
    }
    match v {
        Value::String(s) => *s = s.replace(key, "[REDACTED]"),
        Value::Array(a) => a.iter_mut().for_each(|v| redact(v, key)),
        Value::Object(o) => {
            let old = std::mem::take(o);
            for (k, mut v) in old {
                redact(&mut v, key);
                o.insert(k.replace(key, "[REDACTED]"), v);
            }
        }
        _ => (),
    }
}
fn identity(db: &Connection, project: &str) -> Result<String> {
    let root = one(
        db,
        "SELECT user_id FROM composio_installation WHERE id=1",
        [],
    )?
    .ok_or_else(Failure::storage)?;
    if project.is_empty() {
        return Ok(string(&root, "user_id").into());
    }
    if !uuid(project) {
        return Err(error(400, "Invalid project ID."));
    }
    let value = format!("{}-project-{project}", string(&root, "user_id"));
    db.execute(
        "INSERT OR IGNORE INTO composio_project_identities VALUES(?,?)",
        [project, &value],
    )?;
    Ok(string(
        &one(
            db,
            "SELECT user_id FROM composio_project_identities WHERE project=?",
            [project],
        )?
        .ok_or_else(Failure::storage)?,
        "user_id",
    )
    .into())
}
fn base(state: &AppState) -> &str {
    state
        .config
        .connectors
        .composio
        .as_ref()
        .and_then(|c| c.base_url.as_deref())
        .unwrap_or("https://backend.composio.dev/api/v3.1")
        .trim_end_matches('/')
}
fn timeout(state: &AppState) -> Duration {
    Duration::from_millis(
        state
            .config
            .connectors
            .composio
            .as_ref()
            .and_then(|c| c.timeout)
            .unwrap_or(15000)
            .max(1),
    )
}
fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| Failure::storage())
}
async fn read(mut response: reqwest::Response, limit: usize) -> Result<Value> {
    let mut bytes = vec![];
    while let Some(chunk) = response.chunk().await.map_err(|_| {
        error(
            502,
            "Composio request failed or exceeded its time, size, or JSON limits.",
        )
    })? {
        if bytes.len() + chunk.len() > limit {
            return Err(error(
                502,
                "Composio request failed or exceeded its time, size, or JSON limits.",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice::<Value>(&bytes)
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| {
            error(
                502,
                "Composio request failed or exceeded its time, size, or JSON limits.",
            )
        })
}
async fn request(
    state: &AppState,
    path: &str,
    method: reqwest::Method,
    body: Option<&Value>,
) -> Result<Value> {
    let (key, _) = current(state).await?;
    ready(&key)?;
    let before = identity(&lock(state)?.connection, "")?;
    let mut call = client()?
        .request(method, format!("{}{path}", base(state)))
        .header("x-api-key", &key)
        .header("Content-Type", "application/json")
        .timeout(timeout(state));
    if let Some(body) = body {
        call = call.json(body);
    }
    let response = call.send().await.map_err(|_| {
        error(
            502,
            "Composio request failed or exceeded its time, size, or JSON limits.",
        )
    })?;
    if !response.status().is_success() {
        return Err(error(
            502,
            format!(
                "Composio request failed (HTTP {}). Check server configuration and app access.",
                response.status().as_u16()
            ),
        ));
    }
    let mut value = read(response, 512 * 1024).await?;
    if before != identity(&lock(state)?.connection, "")? {
        return Err(error(
            409,
            "Composio API key changed. Reload connected apps.",
        ));
    }
    redact(&mut value, &key);
    Ok(value)
}
fn ready(key: &str) -> Result<()> {
    if key.is_empty() {
        return Err(error(
            503,
            "Connected apps are not configured. Save a Composio API key in Settings.",
        ));
    }
    Ok(())
}
fn page(v: &Value, limit: usize) -> Result<&Vec<Value>> {
    v["items"]
        .as_array()
        .filter(|rows| {
            rows.len() <= limit
                && rows.iter().all(Value::is_object)
                && (v["next_cursor"].is_null()
                    || v["next_cursor"].as_str().is_some_and(|s| s.len() <= 1024))
        })
        .ok_or_else(|| error(502, "Invalid Composio page."))
}
fn encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}
async fn pages(state: &AppState, path: &str) -> Result<Vec<Value>> {
    let (mut rows, mut cursor, mut seen) =
        (vec![], String::new(), std::collections::HashSet::new());
    for _ in 0..5 {
        let value = request(
            state,
            &format!(
                "{path}{}",
                if cursor.is_empty() {
                    String::new()
                } else {
                    format!("&cursor={}", encode(&cursor))
                }
            ),
            reqwest::Method::GET,
            None,
        )
        .await?;
        rows.extend(page(&value, 100)?.iter().cloned());
        let next = string(&value, "next_cursor");
        if next.is_empty() {
            return Ok(rows);
        }
        if !seen.insert(next.to_owned()) {
            break;
        }
        cursor = next.to_owned();
    }
    Err(error(
        502,
        "Composio pagination exceeds five pages or repeats.",
    ))
}
fn receipt(db: &Connection, id: &str, data: &Value) -> Result<Option<Value>> {
    if !uuid(id) {
        return Err(error(400, "Invalid connected apps request ID."));
    }
    if let Some(row) = one(
        db,
        "SELECT request,result FROM composio_commands WHERE id=?",
        [id],
    )? {
        if row["request"] != hash(data) {
            return Err(error(
                409,
                "Request ID was already used for a different change.",
            ));
        }
        if row["result"].is_null() {
            return Err(error(409,"Connection change is uncertain. Reload connected accounts before starting a new change."));
        }
        return Ok(Some(crate::data::json(&row["result"])?));
    }
    Ok(None)
}
fn command(
    db: &Connection,
    id: &str,
    data: &Value,
    run: impl FnOnce() -> Result<Value>,
) -> Result<Value> {
    transaction(db, || {
        if let Some(prior) = receipt(db, id, data)? {
            return Ok(prior);
        }
        let out = run()?;
        db.execute(
            "INSERT INTO composio_commands VALUES(?,?,?)",
            params![
                id,
                hash(data),
                if out.is_null() {
                    None
                } else {
                    Some(out.to_string())
                }
            ],
        )?;
        Ok(out)
    })
}
fn member(db: &Connection, project: &str, agent: &str) -> Result<()> {
    if !project.is_empty()&&!exists(db,"SELECT 1 FROM local_project_members m JOIN local_projects p ON p.id=m.project WHERE m.project=? AND m.agent=? AND p.deleted=0",[project,agent])?{return Err(error(403,"Agent is not an active project member."));}
    Ok(())
}
fn grants(db: &Connection, agent: &str, project: &str) -> Result<Value> {
    member(db, project, agent)?;
    if !uuid(agent) || !exists(db, "SELECT 1 FROM agents WHERE id=?", [agent])? {
        return Err(error(404, "Agent not found."));
    }
    let mut rows = if project.is_empty() {
        all(
            db,
            "SELECT toolkit,enabled,revision FROM composio_grants WHERE agent=? ORDER BY toolkit",
            [agent],
        )?
    } else {
        all(db,"SELECT toolkit,enabled,revision FROM composio_project_grants WHERE project=? AND agent=? ORDER BY toolkit",[project,agent])?
    };
    for row in &mut rows {
        row["enabled"] = json!(row["enabled"] == 1);
    }
    Ok(json!(rows))
}
fn allowed(db: &Connection, agent: &str, toolkit: &str, project: &str) -> Result<bool> {
    member(db, project, agent)?;
    if project.is_empty() {
        exists(
            db,
            "SELECT 1 FROM composio_grants WHERE agent=? AND toolkit=? AND enabled=1",
            [agent, toolkit],
        )
    } else {
        exists(db,"SELECT 1 FROM composio_project_grants WHERE project=? AND agent=? AND toolkit=? AND enabled=1",[project,agent,toolkit])
    }
}
pub async fn connections(state: &AppState, project: &str) -> Result<Vec<Value>> {
    if current(state).await?.0.is_empty() {
        return Ok(vec![]);
    }
    let user = identity(&lock(state)?.connection, project)?;
    let mut result = vec![];
    for row in pages(
        state,
        &format!("/connected_accounts?user_ids={}&limit=100", encode(&user)),
    )
    .await?
    {
        let id = string(&row, "id");
        let toolkit = string(&row["toolkit"], "slug");
        if row.get("user_id").is_some_and(|v| v != &user)
            || id.is_empty()
            || id.len() > 160
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || !slug(toolkit)
            || !row["status"].is_string()
        {
            return Err(error(502, "Invalid Composio connected account."));
        }
        if !project.is_empty() {
            let guard = lock(state)?;
            guard.connection.execute("INSERT OR IGNORE INTO composio_project_accounts(project,account,toolkit) VALUES(?,?,?)",[project,id,toolkit])?;
            if exists(&guard.connection,"SELECT 1 FROM composio_project_accounts WHERE project=? AND account=? AND removed=1",[project,id])?{continue;}
        }
        result.push(json!({"id":id,"toolkit":toolkit,"status":row["status"].as_str().unwrap_or("").chars().take(40).collect::<String>()}));
    }
    Ok(result)
}

pub async fn remove_project(state: &AppState, project: &str) -> Result<()> {
    if !current(state).await?.0.is_empty() {
        let _ = connections(state, project).await;
    }
    let accounts = {
        let guard = lock(state)?;
        let db = &guard.connection;
        transaction(db, || {
            let rows = all(
                db,
                "SELECT account FROM composio_project_accounts WHERE project=? AND removed=0",
                [project],
            )?;
            db.execute(
                "UPDATE composio_project_accounts SET removed=1 WHERE project=?",
                [project],
            )?;
            db.execute(
                "DELETE FROM composio_project_grants WHERE project=?",
                [project],
            )?;
            db.execute("DELETE FROM composio_policies WHERE fingerprint IN(SELECT fingerprint FROM composio_policy_projects WHERE project=?)", [project])?;
            db.execute(
                "DELETE FROM composio_policy_projects WHERE project=?",
                [project],
            )?;
            Ok(rows)
        })?
    };
    for account in accounts {
        let _ = request(
            state,
            &format!(
                "/connected_accounts/{}",
                encode(string(&account, "account"))
            ),
            reqwest::Method::DELETE,
            None,
        )
        .await;
    }
    Ok(())
}
fn reset(db: &Connection, revision: i64, ciphertext: &str) -> Result<()> {
    db.execute("INSERT INTO composio_settings VALUES(1,?,?) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,ciphertext=excluded.ciphertext",params![revision+1,ciphertext])?;
    db.execute(
        "UPDATE composio_installation SET user_id=? WHERE id=1",
        [format!("minarbot-{}", uuid::Uuid::new_v4())],
    )?;
    db.execute_batch("DELETE FROM composio_project_identities; DELETE FROM composio_project_accounts; UPDATE composio_project_grants SET enabled=0,revision=revision+1; DELETE FROM composio_auth_configs; UPDATE composio_grants SET enabled=0,revision=revision+1; DELETE FROM composio_policies; UPDATE composio_calls SET state='ready',decision='revoked',revision=revision+1 WHERE state IN('approval','ready'); UPDATE composio_calls SET state='uncertain',revision=revision+1 WHERE state='running';")?;
    Ok(())
}
fn revision(db: &Connection) -> Result<i64> {
    Ok(
        one(db, "SELECT revision FROM composio_settings WHERE id=1", [])?
            .map(|r| num(&r, "revision"))
            .unwrap_or(0),
    )
}
fn check_revision(db: &Connection, v: &Value) -> Result<()> {
    if v["revision"] != revision(db)? {
        return Err(error(
            409,
            "Composio API key changed. Reload Settings before saving.",
        ));
    }
    Ok(())
}
pub async fn route(
    state: &AppState,
    path: &str,
    method: &Method,
    q: &[(String, String)],
    v: &Value,
) -> Result<Value> {
    let original = path.split('/').skip(1).collect::<Vec<_>>();
    let mut scoped_query = q.to_vec();
    let scoped_path;
    let path = if let ["api", "projects", project, rest @ ..] = original.as_slice() {
        scoped_query.retain(|(name, _)| name != "project");
        scoped_query.push(("project".into(), project.to_string()));
        scoped_path = format!("/api/{}", rest.join("/"));
        scoped_path.as_str()
    } else {
        path
    };
    let q = scoped_query.as_slice();
    let parts: Vec<_> = path.split('/').skip(1).collect();
    let post = *method == Method::POST;
    let id = string(v, "id");
    let (key, status) = current(state).await?;
    let project = param(q, "project").unwrap_or("");
    if !project.is_empty() {
        let guard = lock(state)?;
        if !uuid(project)
            || !exists(
                &guard.connection,
                "SELECT 1 FROM local_projects WHERE id=? AND deleted=0",
                [project],
            )?
        {
            return Err(error(404, "Project not found."));
        }
    }
    if post
        && !path.starts_with("/api/connected-apps/key")
        && !key.is_empty()
        && v.to_string().contains(&key)
    {
        return Err(error(
            400,
            "Connected apps input conflicts with server credentials.",
        ));
    }
    match parts.as_slice() {
        ["api", "connected-apps", "key"] if !post => Ok(status),
        ["api", "connected-apps", "key"] if post => {
            fields(v, &["id", "revision", "key"])?;
            if v["revision"].as_i64().is_none_or(|n| n < 0) {
                return Err(error(400, "Invalid Composio key request ID or revision."));
            }
            let candidate = v["key"]
                .as_str()
                .filter(|s| {
                    !s.trim().is_empty()
                        && s.encode_utf16().count() <= 4096
                        && !s
                            .chars()
                            .any(|c| c < ' ' || ('\u{7f}'..='\u{9f}').contains(&c))
                })
                .ok_or_else(|| {
                    error(
                        400,
                        "Composio API key must be 1-4096 characters with no control characters.",
                    )
                })?
                .trim();
            let data = json!({"saveKey":hash(&json!(candidate)),"revision":v["revision"]});
            {
                let guard = lock(state)?;
                if let Some(prior) = receipt(&guard.connection, id, &data)? {
                    return Ok(prior);
                }
                check_revision(&guard.connection, v)?;
            }
            let response = client()?
                .get(format!("{}/toolkits/categories", base(state)))
                .header("x-api-key", candidate)
                .timeout(timeout(state))
                .send()
                .await
                .map_err(|_| {
                    error(
                        502,
                        "Composio key verification failed. Check your connection and try again.",
                    )
                })?;
            if !response.status().is_success() {
                return Err(error(
                    if matches!(response.status().as_u16(), 401 | 403) {
                        400
                    } else {
                        502
                    },
                    "Composio could not verify this API key. Check the key and try again.",
                ));
            }
            let guard = lock(state)?;
            command(&guard.connection, id, &data, || {
                check_revision(&guard.connection, v)?;
                let encrypted = encrypt(state, candidate).map_err(|_| {
                    error(
                        409,
                        "Composio key file is unavailable. Check its permissions on this host.",
                    )
                })?;
                reset(&guard.connection, num(v, "revision"), &encrypted)?;
                Ok(json!({"source":"settings","revision":num(v,"revision")+1}))
            })
        }
        ["api", "connected-apps", "key", "remove"] if post => {
            fields(v, &["id", "revision"])?;
            if v["revision"].as_i64().is_none_or(|n| n < 0) {
                return Err(error(400, "Invalid Composio key revision."));
            }
            let env = environment_key(state).await?;
            let guard = lock(state)?;
            let db = &guard.connection;
            command(
                db,
                id,
                &json!({"removeKey":true,"revision":v["revision"]}),
                || {
                    check_revision(db, v)?;
                    if one(
                        db,
                        "SELECT ciphertext FROM composio_settings WHERE id=1",
                        [],
                    )?
                    .is_none_or(|r| string(&r, "ciphertext").is_empty())
                    {
                        return Err(error(
                            409,
                            "Only a key saved in Settings can be removed here.",
                        ));
                    }
                    reset(db, num(v, "revision"), "")?;
                    Ok(
                        json!({"source":if env.is_empty(){"none"}else{"environment"},"revision":num(v,"revision")+1}),
                    )
                },
            )
        }
        ["api", "connected-apps"] if !post => {
            if q.iter().any(|(name, _)| {
                !["search", "category", "after", "project"].contains(&name.as_str())
            }) {
                return Err(error(400, "Invalid connected apps search."));
            }
            let query = {
                let mut query = url::form_urlencoded::Serializer::new(String::new());
                query
                    .append_pair("limit", "50")
                    .append_pair("sort_by", "usage");
                for (name, value) in q {
                    if name == "project" {
                        continue;
                    }
                    bounded(&json!(value), if name == "after" { 1024 } else { 100 })?;
                    query.append_pair(if name == "after" { "cursor" } else { name }, value);
                }
                query.finish()
            };
            if key.is_empty() {
                return Ok(
                    json!({"configured":false,"items":[],"categories":[],"next_after":null}),
                );
            }
            let rows = request(
                state,
                &format!("/toolkits?{}", query),
                reqwest::Method::GET,
                None,
            )
            .await?;
            let categories =
                request(state, "/toolkits/categories", reqwest::Method::GET, None).await?;
            let items=page(&rows,50)?.iter().map(|r|{if !slug(string(r,"slug"))||!r["name"].is_string(){return Err(error(502,"Invalid Composio toolkit."));}Ok(json!({"slug":r["slug"],"name":string(r,"name").chars().take(160).collect::<String>(),"description":string(&r["meta"],"description").chars().take(1000).collect::<String>(),"managedAuth":r["composio_managed_auth_schemes"].as_array().is_some_and(|a|a.iter().any(|s|s=="OAUTH2"))}))}).collect::<Result<Vec<_>>>()?;
            let categories = page(&categories, 200)?
                .iter()
                .map(|r| Ok(json!({"id":bounded(&r["id"],100)?,"name":bounded(&r["name"],100)?})))
                .collect::<Result<Vec<_>>>()?;
            Ok(
                json!({"configured":true,"items":items,"categories":categories,"next_after":rows.get("next_cursor").cloned().unwrap_or(Value::Null)}),
            )
        }
        ["api", "connected-apps", "grant-all", toolkit] if post && !project.is_empty() => {
            ready(&key)?;
            if !closed(v, &["id", "confirmed"]) || v["confirmed"] != true {
                return Err(error(400, "Confirm granting app access to all members."));
            }
            if !slug(toolkit) {
                return Err(error(400, "Invalid project or toolkit."));
            }
            let guard = lock(state)?;
            let db = &guard.connection;
            command(
                db,
                id,
                &json!({"project":project,"toolkit":toolkit,"grantAll":true}),
                || {
                    let members = all(
                        db,
                        "SELECT agent FROM local_project_members WHERE project=?",
                        [project],
                    )?;
                    for row in &members {
                        db.execute("INSERT INTO composio_project_grants VALUES(?,?,?,1,1) ON CONFLICT(project,agent,toolkit) DO UPDATE SET enabled=1,revision=revision+1",[project,string(row,"agent"),toolkit])?;
                    }
                    Ok(json!({"granted":true,"members":members.len()}))
                },
            )
        }
        ["api", "connected-apps", "accounts"] if !post => {
            Ok(json!({"configured":!key.is_empty(),"items":connections(state,project).await?}))
        }
        ["api", "connected-apps", action, target]
            if post && ["connect", "disconnect"].contains(action) =>
        {
            ready(&key)?;
            fields(v, &["id"])?;
            if *action == "connect" && !slug(target) {
                return Err(error(400, "Invalid toolkit."));
            }
            let data = if *action == "connect" {
                json!({"connect":target,"project":project})
            } else {
                json!({"disconnect":target,"project":project})
            };
            {
                let guard = lock(state)?;
                if let Some(prior) = receipt(&guard.connection, id, &data)? {
                    return Ok(prior);
                }
                command(&guard.connection, id, &data, || Ok(Value::Null))?;
            }
            let result = if *action == "connect" {
                let saved = one(
                    &lock(state)?.connection,
                    "SELECT config FROM composio_auth_configs WHERE toolkit=?",
                    [target],
                )?;
                let config = if let Some(saved) = saved {
                    string(&saved, "config").to_owned()
                } else {
                    let info = request(
                        state,
                        &format!("/toolkits/{}", encode(target)),
                        reqwest::Method::GET,
                        None,
                    )
                    .await?;
                    if !info["composio_managed_auth_schemes"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|s| s == "OAUTH2"))
                    {
                        return Err(error(409,"This app needs a custom authentication configuration. Managed OAuth is unavailable."));
                    }
                    let created=request(state,"/auth_configs",reqwest::Method::POST,Some(&json!({"toolkit":{"slug":target},"auth_config":{"type":"use_composio_managed_auth","credentials":{}}}))).await?;
                    let config = bounded(&created["auth_config"]["id"], 160)?.to_owned();
                    if config.is_empty() {
                        return Err(error(
                            502,
                            "Composio did not return an authentication configuration.",
                        ));
                    }
                    lock(state)?.connection.execute(
                        "INSERT OR REPLACE INTO composio_auth_configs VALUES(?,?)",
                        [target, &config.as_str()],
                    )?;
                    config
                };
                let user = identity(&lock(state)?.connection, project)?;
                let link = request(
                    state,
                    "/connected_accounts/link",
                    reqwest::Method::POST,
                    Some(&json!({"user_id":user,"auth_config_id":config})),
                )
                .await?;
                let redirect = url::Url::parse(bounded(&link["redirect_url"], 4000)?)
                    .map_err(|_| error(502, "Composio returned an invalid connect link."))?;
                if redirect.scheme() != "https"
                    || !redirect.username().is_empty()
                    || redirect.password().is_some()
                    || !redirect
                        .host_str()
                        .is_some_and(|h| h == "composio.dev" || h.ends_with(".composio.dev"))
                {
                    return Err(error(502, "Composio returned an invalid connect link."));
                }
                Ok(json!({"redirect_url":redirect.as_str()}))
            } else {
                let owned = connections(state, project)
                    .await?
                    .into_iter()
                    .find(|r| r["id"] == *target)
                    .ok_or_else(|| {
                        error(404, "Connected account not found for this installation.")
                    })?;
                {
                    let guard = lock(state)?;
                    let db = &guard.connection;
                    transaction(db, || {
                        if project.is_empty() {
                            db.execute("UPDATE composio_grants SET enabled=0,revision=revision+1 WHERE toolkit=?",[string(&owned,"toolkit")])?;
                        } else {
                            db.execute("UPDATE composio_project_grants SET enabled=0,revision=revision+1 WHERE project=? AND toolkit=?",[project,string(&owned,"toolkit")])?;
                            db.execute("UPDATE composio_project_accounts SET removed=1 WHERE project=? AND account=?",[project,target])?;
                        }
                        db.execute("DELETE FROM composio_policies WHERE account=?", [target])?;
                        Ok(())
                    })?;
                }
                match request(
                    state,
                    &format!("/connected_accounts/{}", encode(target)),
                    reqwest::Method::DELETE,
                    None,
                )
                .await
                {
                    Ok(_) => Ok(json!({"disconnected":true,"remoteDeleted":true})),
                    Err(e) if project.is_empty() => Err(e),
                    Err(_) => Ok(json!({"disconnected":true,"remoteDeleted":false})),
                }
            }?;
            lock(state)?.connection.execute(
                "UPDATE composio_commands SET result=? WHERE id=?",
                params![result.to_string(), id],
            )?;
            Ok(result)
        }
        ["api", "agents", agent, "connected-apps"] if !post => {
            Ok(json!({"items":grants(&lock(state)?.connection,agent,project)?}))
        }
        ["api", "agents", agent, "connected-apps", toolkit] if post => {
            ready(&key)?;
            fields(v, &["id", "revision", "enabled"])?;
            if !slug(toolkit)
                || !v["enabled"].is_boolean()
                || v["revision"].as_i64().is_none_or(|n| n < 0)
            {
                return Err(error(400, "Invalid connected app access change."));
            }
            let guard = lock(state)?;
            let db = &guard.connection;
            grants(db, agent, project)?;
            command(
                db,
                id,
                &json!({"agent":agent,"toolkit":toolkit,"project":project,"data":v}),
                || {
                    let enabled = grants(db, agent, project)?;
                    if v["enabled"] == true
                        && enabled.as_array().is_some_and(|a| {
                            a.iter()
                                .filter(|r| r["enabled"] == true && r["toolkit"] != *toolkit)
                                .count()
                                >= 16
                        })
                    {
                        return Err(error(
                            400,
                            "Connected apps limit is 16 granted toolkits per agent.",
                        ));
                    }
                    let row = if project.is_empty() {
                        one(
                            db,
                            "SELECT revision FROM composio_grants WHERE agent=? AND toolkit=?",
                            [agent, toolkit],
                        )?
                    } else {
                        one(db,"SELECT revision FROM composio_project_grants WHERE project=? AND agent=? AND toolkit=?",[project,agent,toolkit])?
                    };
                    if row.map(|r| num(&r, "revision")).unwrap_or(0) != num(v, "revision") {
                        return Err(error(
                            409,
                            "Connected app access changed. Reload before saving.",
                        ));
                    }
                    if project.is_empty() {
                        db.execute("INSERT INTO composio_grants VALUES(?,?,?,?) ON CONFLICT(agent,toolkit) DO UPDATE SET enabled=excluded.enabled,revision=excluded.revision",params![agent,toolkit,v["enabled"]==true,num(v,"revision")+1])?;
                    } else {
                        db.execute("INSERT INTO composio_project_grants VALUES(?,?,?,?,?) ON CONFLICT(project,agent,toolkit) DO UPDATE SET enabled=excluded.enabled,revision=excluded.revision",params![project,agent,toolkit,v["enabled"]==true,num(v,"revision")+1])?;
                    }
                    db.execute("DELETE FROM composio_policies WHERE agent=? AND toolkit=? AND coalesce((SELECT project FROM composio_policy_projects WHERE fingerprint=composio_policies.fingerprint),'')=?",[agent,toolkit,project])?;
                    Ok(
                        json!({"toolkit":toolkit,"enabled":v["enabled"],"revision":num(v,"revision")+1}),
                    )
                },
            )
        }
        ["api", "agents", agent, "connected-app-policies"] => {
            let guard = lock(state)?;
            let db = &guard.connection;
            grants(db, agent, project)?;
            if !post {
                return Ok(
                    json!({"items":all(db,"SELECT p.tool,p.toolkit,p.account,p.fingerprint FROM composio_policies p LEFT JOIN composio_policy_projects s ON s.fingerprint=p.fingerprint WHERE p.agent=? AND coalesce(s.project,'')=?",[agent,project])?}),
                );
            }
            fields(v, &["id", "fingerprint"])?;
            bounded(&v["fingerprint"], 64)?;
            command(
                db,
                id,
                &json!({"agent":agent,"project":project,"data":v}),
                || {
                    db.execute("DELETE FROM composio_policies WHERE agent=? AND fingerprint=? AND coalesce((SELECT project FROM composio_policy_projects WHERE fingerprint=composio_policies.fingerprint),'')=?",[agent,string(v,"fingerprint"),project])?;
                    Ok(json!({"revoked":true}))
                },
            )
        }
        ["api", kind, owner, "turns", turn, "app-approvals", call] if post => {
            let guard = lock(state)?;
            let db = &guard.connection;
            let task = one(db, "SELECT * FROM turns WHERE id=?", [turn])?
                .filter(|r| match *kind {
                    "agents" => {
                        r["agent"] == *owner && r["thread"].is_null() && r["channel"].is_null()
                    }
                    "threads" => r["thread"] == *owner,
                    "channels" => r["channel"] == *owner,
                    _ => false,
                })
                .ok_or_else(|| error(404, "Task not found."))?;
            decide(
                db,
                id,
                &task,
                &url::form_urlencoded::parse(format!("v={}", call.replace('+', "%2B")).as_bytes())
                    .next()
                    .map(|(_, v)| v.into_owned())
                    .ok_or_else(|| error(400, "Invalid approval ID."))?,
                v,
            )
        }
        _ => Err(error(404, "Route not found.")),
    }
}
fn turn_project(db: &Connection, turn: &str) -> Result<String> {
    Ok(one(db,"SELECT coalesce(p.project,c.project,'') AS project FROM turns t LEFT JOIN local_project_threads p ON p.thread=t.thread LEFT JOIN channels c ON c.id=t.channel WHERE t.id=?",[turn])?.map(|r|string(&r,"project").to_owned()).unwrap_or_default())
}
fn scope_allowed(db: &Connection, tool: &Value, agent: &str, turn: &str) -> Result<bool> {
    let project = turn_project(db, turn)?;
    if string(tool, "project") != project {
        return Ok(false);
    }
    if !project.is_empty() {
        member(db, &project, agent)?;
        if string(tool, "sourceProject").is_empty()
            && !exists(
                db,
                "SELECT 1 FROM local_projects WHERE id=? AND allow_installation_apps=1",
                [&project],
            )?
        {
            return Ok(false);
        }
    }
    Ok(true)
}
pub async fn tools(state: &AppState, turn: &Value) -> Result<Vec<Value>> {
    let agent = string(turn, "agent");
    let project = turn_project(&lock(state)?.connection, string(turn, "id"))?;
    let mut result = scoped_tools(state, agent, &project).await?;
    let include = !project.is_empty()
        && exists(
            &lock(state)?.connection,
            "SELECT 1 FROM local_projects WHERE id=? AND allow_installation_apps=1",
            [&project],
        )?;
    if include {
        for mut tool in scoped_tools(state, agent, "").await? {
            if result.len() < 32 && !result.iter().any(|t| t["slug"] == tool["slug"]) {
                tool["fingerprint"] = json!(hash(
                    &json!({"fingerprint":tool["fingerprint"],"project":project})
                ));
                tool["project"] = json!(project);
                tool["sourceProject"] = json!("");
                result.push(tool);
            }
        }
    }
    if json!(result).to_string().len() > 64000 {
        return Err(error(502, "Connected apps tool catalog exceeds 64 KB."));
    }
    Ok(result)
}
async fn scoped_tools(state: &AppState, agent: &str, project: &str) -> Result<Vec<Value>> {
    let (key, _) = current(state).await?;
    if key.is_empty() {
        return Ok(vec![]);
    }
    let enabled = grants(&lock(state)?.connection, agent, project)?;
    let enabled: Vec<Value> = enabled
        .as_array()
        .ok_or_else(Failure::storage)?
        .iter()
        .filter(|r| r["enabled"] == true)
        .cloned()
        .collect();
    if enabled.is_empty() {
        return Ok(vec![]);
    }
    let user = identity(&lock(state)?.connection, project)?;
    let accounts = connections(state, project).await?;
    let mut result = vec![];
    for grant in enabled {
        let toolkit = string(&grant, "toolkit");
        let mut active: Vec<_> = accounts
            .iter()
            .filter(|r| r["toolkit"] == toolkit && r["status"] == "ACTIVE")
            .collect();
        active.sort_by_key(|r| string(r, "id"));
        let Some(account) = active.first() else {
            continue;
        };
        let response = request(
            state,
            &format!(
                "/tools?toolkit_slug={}&limit=32&toolkit_versions=latest",
                encode(toolkit)
            ),
            reqwest::Method::GET,
            None,
        )
        .await?;
        for row in page(&response, 32)? {
            if row["toolkit"]["slug"] != toolkit || row["is_deprecated"] == true {
                continue;
            }
            let slug = string(row, "slug");
            let version = string(row, "version");
            if slug.is_empty()
                || slug.len() > 160
                || !slug.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || version.is_empty()
                || version.len() > 100
                || version == "latest"
                || !version
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
                || !row["input_parameters"].is_object()
                || row["input_parameters"]["type"] != "object"
                || row["input_parameters"].to_string().len() > 16000
            {
                return Err(error(502, "Invalid Composio tool metadata."));
            }
            let tags = row["tags"].as_array().cloned().unwrap_or_default();
            let read = tags.contains(&json!("readOnlyHint"))
                && !tags.iter().any(|t| {
                    ["createHint", "updateHint", "destructiveHint"]
                        .iter()
                        .any(|s| t == s)
                });
            let mut tool = json!({"slug":slug,"name":row.get("name").unwrap_or(&row["slug"]),"toolkit":toolkit,"version":version,"parameters":row["input_parameters"],"description":string(row,"description").chars().take(2000).collect::<String>(),"account":account["id"],"readOnly":read,"installation":user});
            if !project.is_empty() {
                tool["project"] = json!(project);
                tool["sourceProject"] = json!(project);
            }
            tool["fingerprint"] = json!(hash(&tool));
            if result.iter().any(|t: &Value| t["slug"] == slug) {
                return Err(error(502, "Duplicate Composio tool."));
            }
            result.push(tool);
            if json!(result).to_string().len() > 64000 {
                return Err(error(502, "Connected apps tool catalog exceeds 64 KB."));
            }
            if result.len() == 32 {
                return Ok(result);
            }
        }
    }
    Ok(result)
}
pub fn specs(tools: &[Value]) -> Vec<Value> {
    tools.iter().map(|t|json!({"type":"function","function":{"name":tool_name(string(t,"slug")),"description":format!("{}: {} Connected app action.",string(t,"toolkit"),if string(t,"description").is_empty(){string(t,"name")}else{string(t,"description")}),"parameters":t["parameters"]}})).collect()
}
fn validate(schema: &Value, value: &Value) -> bool {
    if let Some(options) = schema["enum"].as_array() {
        if !options.contains(value) {
            return false;
        }
    }
    if let Some(constant) = schema.get("const") {
        if constant != value {
            return false;
        }
    }
    if let Some(types) = schema.get("type") {
        let valid = |t: &str| match t {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "number" => value.is_number(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => false,
        };
        if !types.as_str().is_some_and(valid)
            && !types
                .as_array()
                .is_some_and(|a| a.iter().any(|t| t.as_str().is_some_and(valid)))
        {
            return false;
        }
    }
    if let Some(object) = value.as_object() {
        if schema["required"].as_array().is_some_and(|a| {
            a.iter()
                .any(|k| k.as_str().is_none_or(|k| !object.contains_key(k)))
        }) {
            return false;
        }
        for (k, v) in object {
            if let Some(s) = schema["properties"].get(k) {
                if !validate(s, v) {
                    return false;
                }
            } else if schema["additionalProperties"] == false {
                return false;
            }
        }
    }
    if let Some(array) = value.as_array() {
        if schema["maxItems"]
            .as_u64()
            .is_some_and(|n| array.len() as u64 > n)
            || schema["minItems"]
                .as_u64()
                .is_some_and(|n| (array.len() as u64) < n)
        {
            return false;
        }
        if let Some(items) = schema.get("items") {
            if !array.iter().all(|v| validate(items, v)) {
                return false;
            }
        }
    }
    if let Some(text) = value.as_str() {
        let n = text.chars().count() as u64;
        if schema["maxLength"].as_u64().is_some_and(|max| n > max)
            || schema["minLength"].as_u64().is_some_and(|min| n < min)
        {
            return false;
        }
        if let Some(pattern) = schema["pattern"].as_str() {
            if regex::Regex::new(pattern)
                .map(|r| !r.is_match(text))
                .unwrap_or(true)
            {
                return false;
            }
        }
    }
    if let Some(n) = value.as_f64() {
        if schema["minimum"].as_f64().is_some_and(|min| n < min)
            || schema["maximum"].as_f64().is_some_and(|max| n > max)
        {
            return false;
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(choices) = schema[keyword].as_array() {
            let n = choices.iter().filter(|s| validate(s, value)).count();
            if keyword == "allOf" && n != choices.len()
                || keyword == "anyOf" && n == 0
                || keyword == "oneOf" && n != 1
            {
                return false;
            }
        }
    }
    !schema.get("not").is_some_and(|s| validate(s, value))
}
fn journal(db: &Connection, row: &Value, result: &Value) -> Result<()> {
    db.execute("INSERT INTO flow_tool_calls(turn,call_id,call,result,after_output,after_action) VALUES(?,?,?,?,?,?)",params![row["turn"].as_str(),row["call_id"].as_str(),row["call"].as_str(),result.to_string(),row["after_output"].as_i64(),row["after_action"].as_i64()])?;
    db.execute("UPDATE composio_calls SET state='done',result=?,revision=revision+1 WHERE turn=? AND call_id=?",params![result.to_string(),row["turn"].as_str(),row["call_id"].as_str()])?;
    Ok(())
}
async fn run(state: &AppState, turn: &str, id: &str) -> Result<Value> {
    let row = one(
        &lock(state)?.connection,
        "SELECT * FROM composio_calls WHERE turn=? AND call_id=?",
        [turn, id],
    )?
    .ok_or_else(Failure::storage)?;
    if row["state"] == "done" {
        return crate::data::json(&row["result"]);
    }
    if row["state"] == "running"
        || row["state"] == "uncertain"
        || exists(
            &lock(state)?.connection,
            "SELECT 1 FROM composio_calls WHERE turn=? AND state='uncertain'",
            [turn],
        )?
    {
        return Err(error(409, UNCERTAIN));
    }
    let tool = crate::data::json(&row["tool"])?;
    let call = crate::data::json(&row["call"])?;
    let result = if row["decision"] == "revoked" {
        json!({"error":"Connected app access was removed or the account is inactive."})
    } else if row["decision"] == "deny" {
        json!({"error":"The user did not allow this action. Do not retry it unless the user asks.","denied":true})
    } else {
        let accounts = connections(state, string(&tool, "sourceProject")).await?;
        let allowed = {
            let guard = lock(state)?;
            let db = &guard.connection;
            scope_allowed(db, &tool, string(&row, "agent"), turn)?
                && allowed(
                    db,
                    string(&row, "agent"),
                    string(&tool, "toolkit"),
                    string(&tool, "sourceProject"),
                )?
                && tool["installation"] == identity(db, string(&tool, "sourceProject"))?
                && accounts.iter().any(|a| {
                    a["id"] == tool["account"]
                        && a["toolkit"] == tool["toolkit"]
                        && a["status"] == "ACTIVE"
                })
        };
        if !allowed {
            json!({"error":"Connected app access was removed or the account is inactive."})
        } else {
            let user = {
                let guard = lock(state)?;
                let db = &guard.connection;
                transaction(db, || {
                    if !exists(
                        db,
                        "SELECT 1 FROM turns WHERE id=? AND status='running'",
                        [turn],
                    )? || crate::turns::pending(state)
                    {
                        return Err(error(409, "Connected app task stopped."));
                    }
                    db.execute("UPDATE composio_calls SET state='running',revision=revision+1 WHERE turn=? AND call_id=? AND state='ready'",[turn,id])?;
                    identity(db, string(&tool, "sourceProject"))
                })?
            };
            let args: Value = serde_json::from_str(string(&call["function"], "arguments"))
                .map_err(|_| Failure::storage())?;
            let response=request(state,&format!("/tools/execute/{}",encode(string(&tool,"slug"))),reqwest::Method::POST,Some(&json!({"user_id":user,"connected_account_id":tool["account"],"version":tool["version"],"arguments":args,"allow_tracing":false}))).await;
            let row = one(
                &lock(state)?.connection,
                "SELECT state FROM composio_calls WHERE turn=? AND call_id=?",
                [turn, id],
            )?
            .ok_or_else(Failure::storage)?;
            if response.is_err() || row["state"] == "uncertain" {
                lock(state)?.connection.execute("UPDATE composio_calls SET state='uncertain',result=?,revision=revision+1 WHERE turn=? AND call_id=?",params![json!({"error":UNCERTAIN,"uncertain":true}).to_string(),turn,id])?;
                return Err(error(409, UNCERTAIN));
            }
            let mut response = response?;
            let secrets = {
                let guard = lock(state)?;
                let project = turn_project(&guard.connection, turn)?;
                crate::mcp::secrets::project(state, &guard.connection, Some(&project))?
            };
            for secret in secrets {
                redact(&mut response, &secret);
            }
            if response["successful"] == true {
                json!({"successful":true,"data":response.get("data").cloned().unwrap_or(Value::Null)})
            } else {
                json!({"error":"Connected app action failed. Check the target app before trying another action."})
            }
        }
    };
    let guard = lock(state)?;
    transaction(&guard.connection, || {
        journal(&guard.connection, &row, &result)
    })?;
    Ok(result)
}
pub async fn call(
    state: &AppState,
    turn: &Value,
    call: &Value,
    offered: &[Value],
) -> Result<Value> {
    let (key, _) = current(state).await?;
    ready(&key)?;
    if !key.is_empty() && call.to_string().contains(&key) {
        return Err(error(
            400,
            "Connected apps input conflicts with server credentials.",
        ));
    }
    let id = string(call, "id");
    let args = string(&call["function"], "arguments");
    let turn_id = string(turn, "id");
    let agent = string(turn, "agent");
    if id.is_empty() || id.len() > 160 || args.len() > 32000 {
        return Err(error(400, "Invalid connected app tool call."));
    }
    let prior = one(
        &lock(state)?.connection,
        "SELECT * FROM composio_calls WHERE turn=? AND call_id=?",
        [turn_id, id],
    )?;
    if let Some(prior) = prior {
        let encoded_call = call.to_string();
        if prior["call"].as_str() != Some(encoded_call.as_str()) || prior["agent"] != agent {
            return Err(error(409, "Tool call ID was already used."));
        }
        return if prior["state"] == "approval" {
            Ok(json!({"paused":true}))
        } else {
            run(state, turn_id, id).await
        };
    }
    let tool = offered
        .iter()
        .find(|t| tool_name(string(t, "slug")) == string(&call["function"], "name"))
        .ok_or_else(|| {
            error(
                403,
                "Connected app tool was not offered or access was revoked.",
            )
        })?;
    let args: Value = serde_json::from_str(args)
        .map_err(|_| error(400, "Connected app arguments must be JSON."))?;
    if !args.is_object() || !validate(&tool["parameters"], &args) {
        return Err(error(
            400,
            "Connected app arguments do not match the tool schema.",
        ));
    }
    let paused = {
        let guard = lock(state)?;
        let db = &guard.connection;
        if tool["installation"] != identity(db, string(tool, "sourceProject"))?
            || !scope_allowed(db, tool, agent, turn_id)?
            || !allowed(
                db,
                agent,
                string(tool, "toolkit"),
                string(tool, "sourceProject"),
            )?
        {
            return Err(error(
                403,
                "Connected app tool was not offered or access was revoked.",
            ));
        }
        let peer=exists(db,"WITH RECURSIVE tree(id,parent,origin) AS (SELECT id,parent,origin FROM turns WHERE id=? UNION ALL SELECT t.id,t.parent,t.origin FROM turns t JOIN tree ON t.id=tree.parent) SELECT 1 FROM tree WHERE origin LIKE 'peer:%' LIMIT 1",[turn_id])?;
        let paused = tool["readOnly"] != true
            && (!exists(
                db,
                "SELECT 1 FROM composio_policies WHERE agent=? AND fingerprint=?",
                [agent, string(tool, "fingerprint")],
            )? || peer);
        transaction(db, || {
            if !exists(
                db,
                "SELECT 1 FROM turns WHERE id=? AND status='running'",
                [turn_id],
            )? {
                return Err(error(409, "Connected app task stopped."));
            }
            let count: i64 = db.query_row(
                "SELECT count(*) FROM generated_files WHERE turn=?",
                [turn_id],
                |r| r.get(0),
            )?;
            db.execute(
                "INSERT INTO composio_calls VALUES(?,?,?,?,?,?,?,?,?,?,1)",
                params![
                    turn_id,
                    id,
                    agent,
                    call.to_string(),
                    tool.to_string(),
                    if paused { "approval" } else { "ready" },
                    if paused { Some("pending") } else { None },
                    None::<String>,
                    count,
                    0
                ],
            )?;
            if paused {
                db.execute(
                    "UPDATE turns SET status='queued',paused=1,revision=revision+1 WHERE id=?",
                    [turn_id],
                )?;
            }
            Ok(())
        })?;
        paused
    };
    if paused {
        Ok(json!({"paused":true}))
    } else {
        run(state, turn_id, id).await
    }
}
pub async fn resume(state: &AppState, id: &str) -> Result<()> {
    let rows = all(
        &lock(state)?.connection,
        "SELECT call_id FROM composio_calls WHERE turn=? AND state='ready' ORDER BY rowid",
        [id],
    )?;
    if !rows.is_empty() {
        lock(state)?.connection.execute(
            "UPDATE turns SET status='running' WHERE id=? AND status='queued' AND paused=0",
            [id],
        )?;
    }
    for row in rows {
        run(state, id, string(&row, "call_id")).await?;
    }
    if exists(
        &lock(state)?.connection,
        "SELECT 1 FROM composio_calls WHERE turn=? AND state IN('running','uncertain')",
        [id],
    )? {
        return Err(error(409, UNCERTAIN));
    }
    Ok(())
}
fn decide(db: &Connection, id: &str, task: &Value, call: &str, v: &Value) -> Result<Value> {
    fields(v, &["id", "revision", "decision"])?;
    if !v["revision"].is_i64() || !["allow", "always", "deny"].contains(&string(v, "decision")) {
        return Err(error(400, "Invalid connected app approval."));
    }
    let turn = string(task, "id");
    command(db, id, &json!({"turn":turn,"callId":call,"data":v}), || {
        let row = one(
            db,
            "SELECT * FROM composio_calls WHERE turn=? AND call_id=?",
            [turn, call],
        )?
        .filter(|r| {
            r["state"] == "approval"
                && r["revision"] == v["revision"]
                && task["status"] == "queued"
                && task["paused"] == 1
                && task["agent"] == r["agent"]
        })
        .ok_or_else(|| {
            error(
                409,
                "Approval changed or task is unavailable. Reload before deciding.",
            )
        })?;
        let tool = crate::data::json(&row["tool"])?;
        if (!scope_allowed(db, &tool, string(&row, "agent"), turn)?
            || !allowed(
                db,
                string(&row, "agent"),
                string(&tool, "toolkit"),
                string(&tool, "sourceProject"),
            )?)
            && v["decision"] != "deny"
        {
            return Err(error(409, "Connected app access was revoked."));
        }
        if v["decision"] == "always" {
            db.execute(
                "INSERT OR IGNORE INTO composio_policies VALUES(?,?,?,?,?)",
                [
                    string(&row, "agent"),
                    string(&tool, "fingerprint"),
                    string(&tool, "slug"),
                    string(&tool, "account"),
                    string(&tool, "toolkit"),
                ],
            )?;
            if !string(&tool, "project").is_empty() {
                db.execute(
                    "INSERT OR REPLACE INTO composio_policy_projects VALUES(?,?)",
                    [string(&tool, "fingerprint"), string(&tool, "project")],
                )?;
            }
        }
        db.execute("UPDATE composio_calls SET state='ready',decision=?,revision=revision+1 WHERE turn=? AND call_id=?",[string(v,"decision"),turn,call])?;
        db.execute(
            "UPDATE turns SET paused=0,revision=revision+1 WHERE id=?",
            [turn],
        )?;
        Ok(json!({"id":call,"decision":v["decision"]}))
    })
}
