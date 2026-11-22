use super::{archive, error, release, Result};
use crate::{computer::machines, files, web::Failure, AppState};
use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::{header, HeaderValue, Method},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::AsyncReadExt;

static DOWNLOADS: AtomicUsize = AtomicUsize::new(0);
struct Download;
impl Drop for Download {
    fn drop(&mut self) {
        DOWNLOADS.fetch_sub(1, Ordering::AcqRel);
    }
}

pub fn initialize(db: &Connection) -> crate::db::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS install_tokens(digest BLOB PRIMARY KEY,expires INTEGER NOT NULL,used INTEGER NOT NULL DEFAULT 0,created INTEGER NOT NULL,request_id TEXT UNIQUE NOT NULL,release TEXT NOT NULL) STRICT; CREATE TABLE IF NOT EXISTS install_rate_limit(bucket INTEGER PRIMARY KEY,attempts INTEGER NOT NULL) STRICT;")?;
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS install_token_targets(
        token BLOB PRIMARY KEY REFERENCES install_tokens(digest) ON DELETE CASCADE,
        origin TEXT NOT NULL
    ) STRICT;",
    )?;
    Ok(())
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/install/tokens", any(entry))
        .route("/api/install/script", any(entry))
        .route("/api/install/release", any(entry))
        .route("/api/install/pairing-code", any(entry))
}
fn unauthorized() -> Failure {
    error(401, "Unauthorized install request.")
}
fn admit(db: &Connection, now: i64) -> Result<()> {
    let bucket = now.div_euclid(60000);
    db.execute(
        "DELETE FROM install_rate_limit WHERE bucket<?",
        [bucket - 1],
    )?;
    let count: i64 = db.query_row(
        "SELECT coalesce(sum(attempts),0) FROM install_rate_limit",
        [],
        |r| r.get(0),
    )?;
    if count >= 60 {
        return Err(error(429, "Too many install requests. Try again shortly."));
    }
    db.execute("INSERT INTO install_rate_limit VALUES(?,1) ON CONFLICT(bucket) DO UPDATE SET attempts=attempts+1", [bucket])?;
    Ok(())
}
fn lookup(db: &Connection, header: Option<&str>, now: i64, count: bool) -> Result<Vec<u8>> {
    if count {
        admit(db, now)?;
    }
    let token = header
        .and_then(|h| h.strip_prefix("Bearer "))
        .filter(|t| {
            t.len() == 47
                && t.starts_with("bsi_")
                && t[4..]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        })
        .ok_or_else(unauthorized)?;
    let digest = Sha256::digest(token.as_bytes()).to_vec();
    let active: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM install_tokens WHERE digest=? AND used=0 AND expires>?)",
        params![digest, now],
        |r| r.get(0),
    )?;
    if !active {
        return Err(unauthorized());
    }
    Ok(digest)
}
pub(crate) async fn issue(state: &AppState, id: &str, target: Option<&str>) -> Result<Value> {
    let mut random = [0; 32];
    getrandom::getrandom(&mut random).map_err(|_| Failure::storage())?;
    let token = format!("bsi_{}", URL_SAFE_NO_PAD.encode(random));
    let digest = Sha256::digest(token.as_bytes());
    let mut issued = issue_digest(state, id, &digest, target, false).await?;
    issued["token"] = json!(token);
    Ok(issued)
}

async fn issue_digest(
    state: &AppState,
    id: &str,
    digest: &[u8],
    target: Option<&str>,
    guide: bool,
) -> Result<Value> {
    let source = machines::machine_origin(machines::public_origin(state))?;
    let now = machines::now(state).await?;
    let db = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &db.connection;
    let approved_target: Option<String> = if guide {
        Some(db.query_row(
            "SELECT json_extract(plan,'$.target.origin') FROM guide_changes WHERE id=? AND status='applied' AND expires>?
             AND json_extract(plan,'$.tool')='add_guide_machine' AND json_extract(plan,'$.settings.source')=?",
            params![id, now, source],
            |row| row.get(0),
        ).optional()?.ok_or_else(|| error(409, "Apply an unexpired Add machine proposal first."))?)
    } else {
        None
    };
    admit(db, now)?;
    if db
        .query_row(
            "SELECT 1 FROM install_tokens WHERE request_id=?",
            [id],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some()
    {
        return Err(error(
            409,
            "This command was already shown. Create a new install command.",
        ));
    }
    db.execute("DELETE FROM install_tokens WHERE expires<?", [now])?;
    let count: i64 = db.query_row(
        "SELECT count(*) FROM install_tokens WHERE used=0",
        [],
        |r| r.get(0),
    )?;
    if count >= 5 {
        return Err(error(
            429,
            "Five install tokens are active. Wait for one to expire.",
        ));
    }
    let releases = state
        .config
        .update
        .options()
        .releases_dir
        .as_ref()
        .ok_or_else(|| error(409, "Publish a release before adding a machine."))?;
    let release_id = match release::latest(releases)? {
        Some(info) => files::text(&info, "id").to_owned(),
        None => minarbot_launcher::files::selection(releases, "current")
            .map_err(|_| error(409, "Current release selection is invalid."))?
            .ok_or_else(|| error(409, "Publish a release before adding a machine."))?,
    };
    let tx = db.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO install_tokens VALUES(?,?,0,?,?,?)",
        params![digest, now + 1800000, now, id, release_id],
    )?;
    if let Some(origin) = approved_target.as_deref().or(target) {
        tx.execute(
            "INSERT INTO install_token_targets VALUES(?,?)",
            params![digest, origin],
        )?;
    }
    tx.commit()?;
    Ok(json!({"source":source,"expiresAt":now+1800000}))
}

pub(crate) fn retire(state: &AppState, id: &str) -> Result<()> {
    let db = state.database.lock().map_err(|_| Failure::storage())?;
    let tx = db.connection.unchecked_transaction()?;
    tx.execute("UPDATE install_tokens SET used=1 WHERE request_id=?", [id])?;
    tx.execute("UPDATE pairing_codes SET used=1 WHERE id=? AND EXISTS(
        SELECT 1 FROM install_token_targets target JOIN install_tokens token ON token.digest=target.token WHERE token.request_id=?
    )", params![id,id])?;
    tx.commit()?;
    Ok(())
}

async fn entry(State(state): State<AppState>, request: Request) -> Response {
    let mut response = handle(&state, request)
        .await
        .unwrap_or_else(|e| crate::local_error(e.status, &e.message));
    crate::security_headers(&mut response);
    response
}
async fn handle(state: &AppState, request: Request) -> Result<Response> {
    let path = request.uri().path().to_owned();
    if state.auth.is_some() {
        return Err(error(404, "Route is not available yet."));
    }
    if super::pending(state) {
        return Err(error(503, "Update is being verified. Retry after restart."));
    }
    if path == "/api/install/tokens" {
        files::origin(state, &request)?;
        files::mutation(state, &request)?;
        if crate::text_header(request.headers(), "authorization")
            .is_some_and(|v| v.starts_with("Bearer bsi_"))
        {
            return Err(error(401, "Unauthorized request."));
        }
        let remote = request
            .extensions()
            .get::<crate::auth::pairing::BrowserAccess>()
            .cloned();
        let query = request.uri().query().is_some();
        let bytes = axum::body::to_bytes(request.into_body(), 64000)
            .await
            .map_err(|_| error(413, "Data is too large."))?;
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| error(400, "Invalid JSON."))?;
        let id = files::text(&value, "id").trim();
        if crate::web::id(id).is_none() {
            return Err(error(400, "Invalid request ID."));
        }
        if query || !files::keys(&value, &["id", "digest"]) {
            return Err(error(400, "Invalid install request."));
        }
        if let Some(digest) = value.get("digest") {
            let digest = digest
                .as_str()
                .and_then(|digest| URL_SAFE_NO_PAD.decode(digest).ok())
                .filter(|digest| digest.len() == 32)
                .ok_or_else(|| error(400, "Invalid install token digest."))?;
            if let Some(access) = remote {
                let db = state.database.lock().map_err(|_| Failure::storage())?;
                if !access.paired(&db.connection, state.config.now())? {
                    return Err(error(
                        403,
                        "Pair this browser before creating a Guide install command.",
                    ));
                }
            }
            return Ok(crate::json_response(
                201,
                issue_digest(state, id, &digest, None, true).await?,
            ));
        }
        return Ok(crate::json_response(201, issue(state, id, None).await?));
    }
    if path == "/api/install/script"
        && request.method() == Method::GET
        && request.uri().query().is_none()
    {
        return Ok((
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            include_str!("../../../../scripts/install.sh"),
        )
            .into_response());
    }
    if request.uri().query().is_some()
        || request.headers().contains_key("origin")
        || request.headers().contains_key("sec-fetch-site")
    {
        return Err(unauthorized());
    }
    let auth = crate::text_header(request.headers(), "authorization");
    if path == "/api/install/pairing-code" && request.method() == Method::POST {
        let now = machines::now(state).await?;
        let db = state.database.lock().map_err(|_| Failure::storage())?;
        let digest = lookup(&db.connection, auth, now, true)?;
        let target: Option<(String, String)> = db.connection.query_row(
            "SELECT token.request_id,target.origin FROM install_token_targets target JOIN install_tokens token ON token.digest=target.token WHERE token.digest=?",
            [&digest], |row| Ok((row.get(0)?,row.get(1)?)),
        ).optional()?;
        let id = target
            .as_ref()
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let value = machines::pairing_code(&db.connection, &id, now, |db| {
            if db.execute(
                "UPDATE install_tokens SET used=1 WHERE digest=? AND used=0 AND expires>?",
                params![digest, now],
            )? != 1
            {
                return Err(unauthorized());
            }
            if let Some((_, origin)) = &target {
                db.execute(
                    "INSERT INTO pairing_code_targets VALUES(?,?)",
                    params![id, origin],
                )?;
            }
            Ok(())
        })?;
        return Ok(crate::json_response(200, value));
    }
    if path != "/api/install/release" || request.method() != Method::GET {
        return Err(unauthorized());
    }
    let now = machines::now(state).await?;
    let id: String = {
        let db = state.database.lock().map_err(|_| Failure::storage())?;
        let digest = lookup(&db.connection, auth, now, true)?;
        db.connection.query_row(
            "SELECT release FROM install_tokens WHERE digest=?",
            [digest],
            |r| r.get(0),
        )?
    };
    let root = state
        .config
        .update
        .options()
        .releases_dir
        .as_ref()
        .ok_or_else(|| error(404, "Release feed is unavailable."))?;
    DOWNLOADS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < 2).then_some(n + 1)
        })
        .map_err(|_| error(429, "Two release downloads are active. Try again shortly."))?;
    let download = Download;
    let archive = archive::create(root, &id).await.map_err(|e| {
        if e.message.contains("exceeds") {
            error(413, "Release exceeds download bounds.")
        } else {
            e
        }
    })?;
    let now = machines::now(state).await?;
    {
        let db = state.database.lock().map_err(|_| Failure::storage())?;
        lookup(&db.connection, auth, now, false)?;
    }
    let size = std::fs::metadata(&archive.file)
        .map_err(|_| Failure::storage())?
        .len();
    let mut file = tokio::fs::File::open(&archive.file)
        .await
        .map_err(|_| Failure::storage())?;
    let hash = archive.sha256.clone();
    let stream = async_stream::stream! {
        let _download=download;
        let _archive=archive;
        loop {
            let mut bytes=vec![0;65536];
            match tokio::time::timeout(std::time::Duration::from_secs(120),file.read(&mut bytes)).await {
                Ok(Ok(0))=>break,
                Ok(Ok(n))=>{bytes.truncate(n);yield Ok::<Bytes,std::io::Error>(Bytes::from(bytes));},
                Ok(Err(e))=>{yield Err(e);break;},
                Err(_)=>{yield Err(std::io::Error::new(std::io::ErrorKind::TimedOut,"Release download timed out."));break;}
            }
        }
    };
    let mut response = Body::from_stream(stream).into_response();
    for (key, value) in [
        ("content-type", "application/gzip".to_owned()),
        ("content-length", size.to_string()),
        ("x-minarbot-size", size.to_string()),
        ("x-minarbot-sha256", hash),
        ("x-minarbot-release", id),
    ] {
        response.headers_mut().insert(
            axum::http::HeaderName::from_static(key),
            HeaderValue::from_str(&value).map_err(|_| Failure::storage())?,
        );
    }
    Ok(response)
}
