use super::{id, json, until};
use crate::{
    loopback::Loopback,
    server::{Result, Server, TestRoot},
};
use axum::{
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use reqwest::{header, Method, RequestBuilder, StatusCode};
use rusqlite::{Connection, OpenFlags};
use rustix::process::Signal;
use serde_json::{json, Value};
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tokio::net::TcpListener;

const ISSUER: &str = "https://fixture.clerk.accounts.dev";
const ORIGIN: &str = "https://minarbot.example";
const CURRENT_SCHEMA_VERSION: i64 = 7;
pub(super) const CLOCK: i64 = 1_790_000_000_000;

fn openssl(args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("openssl")
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "openssl failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(output.stdout)
}

struct Identity {
    private: EncodingKey,
    public: String,
    jwk: Value,
}

impl Identity {
    fn new(root: &TestRoot) -> Result<Self> {
        let pem = root.path().join(format!("{}.pem", id()));
        let pem_path = pem.to_str().ok_or("invalid fixture key path")?;
        openssl(&[
            "genpkey",
            "-algorithm",
            "RSA",
            "-pkeyopt",
            "rsa_keygen_bits:2048",
            "-out",
            pem_path,
        ])?;
        let public = String::from_utf8(openssl(&["pkey", "-in", pem_path, "-pubout"])?)?;
        let private = EncodingKey::from_rsa_pem(&fs::read(&pem)?)?;
        let modulus = String::from_utf8(openssl(&["rsa", "-in", pem_path, "-noout", "-modulus"])?)?;
        fs::remove_file(pem)?;
        let modulus = modulus
            .trim()
            .strip_prefix("Modulus=")
            .ok_or("missing RSA modulus")?;
        let bytes: Vec<_> = (0..modulus.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&modulus[index..index + 2], 16))
            .collect::<std::result::Result<_, _>>()?;
        Ok(Self {
            private,
            public,
            jwk: json!({"kty":"RSA","n":URL_SAFE_NO_PAD.encode(bytes),"e":"AQAB","use":"sig","alg":"RS256"}),
        })
    }

    fn token(&self, kid: &str, overrides: Value) -> Result<String> {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.into());
        let seconds = CLOCK / 1000;
        let mut claims = json!({"iss":ISSUER,"sub":"user_fixture","sid":"sess_fixture","azp":ORIGIN,"nbf":seconds-1,"iat":seconds-1,"exp":seconds+60});
        for (key, value) in overrides.as_object().ok_or("expected claims object")? {
            claims[key] = value.clone();
        }
        Ok(encode(&header, &claims, &self.private)?)
    }
}

fn config(identity: Option<&Identity>) -> Value {
    let mut auth = json!({"secretKey":"sk_test_fixture","publishableKey":"pk_test_fixture","issuer":ISSUER,"origin":ORIGIN,"development":true});
    if let Some(identity) = identity {
        auth["jwtKey"] = json!(identity.public);
    }
    json!({"auth":auth,"fixedClock":CLOCK})
}

pub(super) fn canonical_config() -> Value {
    config(None)
}

fn request(
    app: &Server<'_>,
    method: Method,
    path: &str,
    token: Option<&str>,
    request_id: &str,
) -> RequestBuilder {
    let request = app
        .client
        .request(method, format!("{}{path}", app.origin))
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .header("X-Minarbot-Protocol", "1")
        .header("X-Minarbot-Request-Id", request_id);
    match token {
        Some(token) => request.bearer_auth(token),
        None => request,
    }
}

fn session(app: &Server<'_>, token: Option<&str>) -> RequestBuilder {
    request(app, Method::GET, "/api/session", token, &id())
}

#[tokio::test]
async fn unconfigured_browser_origin_rejected() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(json!({})).await?;
    assert_eq!(
        app.get("/api/agents")
            .header(header::HOST, "minarbot.example.test:4317")
            .send()
            .await?
            .status(),
        403
    );
    let agent = json!({"id":id(),"runtime":"api","name":"Origin check","model":"test-model","instructions":"Check origins.","threadLimit":2});
    assert_eq!(
        app.client
            .post(format!("{}/api/agents", app.origin))
            .header(header::ORIGIN, "https://minarbot.example.test:4317")
            .json(&agent)
            .send()
            .await?
            .status(),
        403
    );
    assert_eq!(
        app.post("/api/agents").json(&agent).send().await?.status(),
        201
    );
    Ok(())
}

#[tokio::test]
async fn configured_https_host_and_origin_are_exact() -> Result<()> {
    const HOST: &str = "minarbot.example.test:4317";
    const HTTPS: &str = "https://minarbot.example.test:4317";
    let root = TestRoot::new()?;
    let app = root.start(json!({"allowedOrigins":[HTTPS]})).await?;
    assert_eq!(
        app.get("/api/agents")
            .header(header::HOST, HOST)
            .send()
            .await?
            .status(),
        200
    );
    for (origin, status) in [
        (HTTPS, 201),
        ("https://evil.example.test:4317", 403),
        ("http://minarbot.example.test:4317", 403),
    ] {
        let agent = json!({"id":id(),"runtime":"api","name":"Origin check","model":"test-model","instructions":"Check origins.","threadLimit":2});
        let created = app
            .client
            .post(format!("{}/api/agents", app.origin))
            .header(header::HOST, HOST)
            .header(header::ORIGIN, origin)
            .json(&agent)
            .send()
            .await?;
        assert_eq!(created.status(), status);
    }
    assert_eq!(
        app.get("/api/agents")
            .header(header::HOST, "other.example.test:4317")
            .send()
            .await?
            .status(),
        403
    );
    assert_eq!(
        app.get("/api/agents")
            .header(header::HOST, HOST)
            .header("Sec-Fetch-Site", "cross-site")
            .send()
            .await?
            .status(),
        403
    );
    let agent = json!({"id":id(),"runtime":"api","name":"Origin check","model":"test-model","instructions":"Check origins.","threadLimit":2});
    assert_eq!(
        app.post("/api/agents").json(&agent).send().await?.status(),
        201
    );
    Ok(())
}

#[tokio::test]
async fn first_allowed_origin_identifies_machine() -> Result<()> {
    let root = TestRoot::new()?;
    let origin = "https://linc.example.test:4317";
    let app = root
        .start(json!({"allowedOrigins":[origin,"https://other.example.test"]}))
        .await?;
    assert_eq!(
        json(app.get("/api/environment"), 200).await?["origin"],
        origin
    );
    Ok(())
}

#[tokio::test]
async fn signed_sessions_validate_before_account_mapping() -> Result<()> {
    let root = TestRoot::new()?;
    let identity = Identity::new(&root)?;
    let app = root.start(config(Some(&identity))).await?;
    let token = identity.token("fixture", json!({}))?;
    let first = json(session(&app, Some(&token)), 200).await?;
    let account = first["data"]["accountId"]
        .as_str()
        .ok_or("missing mapped account")?;
    let db = Connection::open(root.database())?;
    assert_eq!(
        db.query_row(
            "SELECT clerk_sub FROM accounts WHERE id=?",
            [account],
            |row| row.get::<_, String>(0)
        )?,
        "user_fixture"
    );
    assert_eq!(
        json(session(&app, Some(&token)), 200).await?["data"]["accountId"],
        account
    );
    for table in ["projects", "record_scopes"] {
        assert_eq!(
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                .get::<_, i64>(0))?,
            0
        );
    }
    let other = identity.token("fixture", json!({"sub":"user_other"}))?;
    assert_ne!(
        json(session(&app, Some(&other)), 200).await?["data"]["accountId"],
        account
    );
    json(session(&app, None), 401).await?;
    json(
        session(&app, None).header(header::COOKIE, format!("__session={token}")),
        401,
    )
    .await?;
    let request_id = id();
    let foreign = app
        .client
        .post(format!("{}/api/session/logout", app.origin))
        .bearer_auth(&token)
        .header(header::ORIGIN, "https://evil.example")
        .header("X-Minarbot-Protocol", "1")
        .header("X-Minarbot-Request-Id", &request_id)
        .json(&json!({"requestId":request_id}));
    json(foreign, 403).await?;

    let now = CLOCK / 1000;
    for claims in [
        json!({"iss":"https://wrong.example"}),
        json!({"exp":now}),
        json!({"nbf":now+30}),
        json!({"azp":"https://wrong.example"}),
        json!({"sid":""}),
        json!({"sub":""}),
        json!({"sts":"pending"}),
    ] {
        let invalid = identity.token("fixture", claims)?;
        json(session(&app, Some(&invalid)), 401).await?;
    }
    let invalid = Identity::new(&root)?.token("fixture", json!({}))?;
    json(session(&app, Some(&invalid)), 401).await?;
    db.execute("UPDATE accounts SET blocked=1 WHERE id=?", [account])?;
    json(session(&app, Some(&token)), 401).await?;
    Ok(())
}

#[tokio::test]
async fn logout_denial_survives_restart_expiry_and_upstream_recovery() -> Result<()> {
    let root = TestRoot::new()?;
    let identity = Identity::new(&root)?;
    let restarted = Arc::new(AtomicBool::new(false));
    let observed = Arc::new(Mutex::new(None::<String>));
    let route = Router::new().route(
        "/v1/sessions/sess_fixture/revoke",
        post({
            let file = root.database();
            let restarted = restarted.clone();
            let observed = observed.clone();
            move || {
                let file = file.clone();
                let restarted = restarted.clone();
                let observed = observed.clone();
                async move {
                    let reason = denial_reason(&file).ok();
                    if let Ok(mut receipt) = observed.lock() {
                        *receipt = reason;
                    }
                    let status = if restarted.load(Ordering::SeqCst) {
                        StatusCode::OK
                    } else {
                        StatusCode::SERVICE_UNAVAILABLE
                    };
                    (status, Json(json!({})))
                }
            }
        }),
    );
    let upstream = Loopback::start(route).await?;
    let mut options = config(Some(&identity));
    options["auth"]["apiUrl"] = json!(format!("http://{}", upstream.address));
    let token = identity.token("fixture", json!({}))?;
    let app = root.start(options.clone()).await?;
    json(session(&app, Some(&token)), 200).await?;
    let request_id = id();
    let logout = request(
        &app,
        Method::POST,
        "/api/session/logout",
        Some(&token),
        &request_id,
    )
    .json(&json!({"requestId":request_id}));
    assert_eq!(
        json(logout, 200).await?["data"],
        json!({"state":"upstream_pending"})
    );
    assert_eq!(
        observed
            .lock()
            .map_err(|_| "denial receipt poisoned")?
            .as_deref(),
        Some("logout_pending")
    );
    assert!(app.terminate(Signal::TERM).await?.success());
    Connection::open(root.database())?
        .execute("UPDATE session_denials SET expires_at=?", [CLOCK - 60000])?;
    let app = root.start(options.clone()).await?;
    json(session(&app, Some(&token)), 401).await?;
    assert!(app.terminate(Signal::TERM).await?.success());
    restarted.store(true, Ordering::SeqCst);
    let app = root.start(options).await?;
    let file = root.database();
    until(async || Ok((denial_reason(&file)? == "logout_complete").then_some(()))).await?;
    json(session(&app, Some(&token)), 401).await?;
    Ok(())
}

fn denial_reason(database: &Path) -> Result<String> {
    Ok(
        Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?.query_row(
            "SELECT reason FROM session_denials",
            [],
            |row| row.get(0),
        )?,
    )
}

#[tokio::test]
async fn occupied_canonical_port_releases_database_for_restart() -> Result<()> {
    let root = TestRoot::new()?;
    let occupied = TcpListener::bind(("127.0.0.1", 0)).await?;
    let mut options = config(None);
    options["port"] = json!(occupied.local_addr()?.port());
    let error = root
        .start(options)
        .await
        .err()
        .ok_or("server bound occupied port")?;
    assert!(error.to_string().contains("Address already in use"));
    let app = root.start(config(None)).await?;
    assert!(app.terminate(Signal::TERM).await?.success());
    Ok(())
}

#[tokio::test]
async fn canonical_project_receipts_are_durable_and_tenant_scoped() -> Result<()> {
    let root = TestRoot::new()?;
    let identity = Identity::new(&root)?;
    let app = root.start(config(Some(&identity))).await?;
    let owner = identity.token("fixture", json!({}))?;
    let other = identity.token("fixture", json!({"sub":"user_other"}))?;
    let before = json(session(&app, Some(&owner)), 200).await?;
    assert_eq!(before["data"]["projects"], json!([]));
    let command = id();
    let project = id();
    let create = json!({"requestId":command,"id":project,"name":"Collaboration"});
    let post = |path, command: &str| request(&app, Method::POST, path, Some(&owner), command);
    let made = json(post("/api/projects", &command).json(&create), 201).await?;
    assert_eq!(made["receipt"]["actorId"], before["data"]["accountId"]);
    let replay = json(post("/api/projects", &command).json(&create), 200).await?;
    assert_eq!(replay["receipt"], made["receipt"]);
    let mut changed = create.clone();
    changed["name"] = json!("Changed");
    json(post("/api/projects", &command).json(&changed), 409).await?;
    let invalid = id();
    json(
        post("/api/projects", &invalid)
            .json(&json!({"requestId":invalid,"id":id(),"name":"Collaboration","private":true})),
        400,
    )
    .await?;

    let get = |path, token| request(&app, Method::GET, path, Some(token), &id());
    assert_eq!(
        json(get("/api/projects", &other), 200).await?["data"],
        json!([])
    );
    let hidden_path = format!("/api/projects/{project}");
    let hidden = json(get(&hidden_path, &other), 404).await?;
    assert_eq!(hidden["error"]["code"], "resource_not_found");
    let legacy = id();
    let legacy_error = json(
        post("/api/agents", &legacy)
            .json(&json!({"requestId":legacy,"id":id(),"text":"Never execute"})),
        400,
    )
    .await?;
    assert_eq!(legacy_error["error"]["code"], "invalid_agent");
    let missing_turns = format!("/api/agents/{}/turns", id());
    json(get(&missing_turns, &owner), 404).await?;
    assert_eq!(
        json(get("/api/projects", "invalid"), 401).await?["ok"],
        false
    );
    let duplicate = id();
    let raw = format!(
        r#"{{"requestId":"{duplicate}","id":"{}","name":"First","name":"Second"}}"#,
        id()
    );
    let denied = json(post("/api/projects", &duplicate).body(raw), 400).await?;
    assert_eq!(denied["error"]["code"], "duplicate_key");
    Ok(())
}

#[tokio::test]
async fn jwks_rotates_and_enforces_audience() -> Result<()> {
    let root = TestRoot::new()?;
    let first = Identity::new(&root)?;
    let second = Identity::new(&root)?;
    let mut keys = [first.jwk.clone(), second.jwk.clone()];
    keys[0]["kid"] = json!("0");
    keys[1]["kid"] = json!("1");
    let rotated = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let keys = Arc::new(keys);
    let upstream = Loopback::start(Router::new().route(
        "/v1/jwks",
        get({
            let rotated = rotated.clone();
            let calls = calls.clone();
            move || {
                let keys = keys.clone();
                let rotated = rotated.clone();
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let count = if rotated.load(Ordering::SeqCst) { 2 } else { 1 };
                    Json(json!({"keys":&keys[..count]}))
                }
            }
        }),
    ))
    .await?;
    let mut options = config(None);
    options["auth"]["audience"] = json!("minarbot");
    options["auth"]["apiUrl"] = json!(format!("http://{}", upstream.address));
    let app = root.start(options).await?;
    let token = first.token("0", json!({"aud":"minarbot"}))?;
    let account = json(session(&app, Some(&token)), 200).await?["data"]["accountId"].clone();
    let before = calls.load(Ordering::SeqCst);
    assert!(before > 0);
    rotated.store(true, Ordering::SeqCst);
    let token = second.token("1", json!({"aud":"minarbot"}))?;
    assert_eq!(
        json(session(&app, Some(&token)), 200).await?["data"]["accountId"],
        account
    );
    assert!(calls.load(Ordering::SeqCst) > before);
    for (kid, audience, status) in [
        ("2", json!("minarbot"), 401),
        ("0", json!("wrong"), 401),
        ("0", json!(["other", "minarbot"]), 200),
    ] {
        let token = first.token(kid, json!({"aud":audience}))?;
        json(session(&app, Some(&token)), status).await?;
    }
    Ok(())
}

#[tokio::test]
async fn canonical_tls_returns_protocol_errors_with_verification() -> Result<()> {
    let root = TestRoot::new()?;
    let cert = root.path().join("cert");
    let key = root.path().join("key");
    // Treat the synthetic self-signed certificate as a TLS end entity.
    let leaf_extensions = root.path().join("leaf.cnf");
    fs::write(
        &leaf_extensions,
        "[req]\ndistinguished_name=subject\nx509_extensions=leaf\n[subject]\n[leaf]\nsubjectAltName=IP:127.0.0.1\n",
    )?;
    openssl(&[
        "req",
        "-x509",
        "-config",
        leaf_extensions
            .to_str()
            .ok_or("invalid fixture config path")?,
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        key.to_str().ok_or("invalid key path")?,
        "-out",
        cert.to_str().ok_or("invalid certificate path")?,
        "-days",
        "1",
        "-subj",
        "/CN=localhost",
    ])?;
    let mut options = config(None);
    options["auth"]["development"] = json!(false);
    options["tls"] = json!({"cert":fs::read_to_string(cert)?,"key":fs::read_to_string(key)?});
    let app = root.start(options).await?;
    assert!(app.origin.starts_with("https://"));
    assert_eq!(
        json(app.get("/api/session"), 426).await?,
        json!({"ok":false,"error":{"code":"unsupported_version","message":"Unsupported protocol version.","retryable":false,"requestId":""}})
    );
    Ok(())
}

#[tokio::test]
async fn canonical_schema_reopens_and_rejects_future_versions_and_checksum_drift() -> Result<()> {
    let root = TestRoot::new()?;
    let options = config(None);
    let app = root.start(options.clone()).await?;
    let busy = root
        .start(options.clone())
        .await
        .err()
        .ok_or("second canonical server started")?;
    assert!(busy
        .to_string()
        .contains("Another canonical server is still running"));
    assert!(app.terminate(Signal::TERM).await?.success());
    let expected = schema(&root)?;

    let app = root.start(options.clone()).await?;
    assert!(app.terminate(Signal::TERM).await?.success());
    assert_eq!(schema(&root)?, expected);

    Connection::open(root.database())?.execute(
        &format!(
            "INSERT INTO schema_migrations VALUES({},'future','future',0)",
            CURRENT_SCHEMA_VERSION + 1
        ),
        [],
    )?;
    let future = root
        .start(options.clone())
        .await
        .err()
        .ok_or("future schema started")?;
    assert!(future
        .to_string()
        .contains("Database schema is newer than this server"));
    Connection::open(root.database())?.execute_batch(&format!(
        "DELETE FROM schema_migrations WHERE version={}; UPDATE schema_migrations SET checksum='bad' WHERE version=6;",
        CURRENT_SCHEMA_VERSION + 1
    ))?;
    let drift = root
        .start(options)
        .await
        .err()
        .ok_or("schema with drift started")?;
    assert!(drift
        .to_string()
        .contains("Schema migration checksum mismatch"));
    Ok(())
}

fn schema(root: &TestRoot) -> Result<Vec<Value>> {
    let db = Connection::open_with_flags(root.database(), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let version: i64 = db.query_row("SELECT max(version) FROM schema_migrations", [], |row| {
        row.get(0)
    })?;
    assert_eq!(version, CURRENT_SCHEMA_VERSION);
    let indexes = [
        "files_scope_seq",
        "occurrence_pending",
        "project_workflow_notifications_reader",
        "provider_calls_turn_seq",
        "queue_edit_active",
        "routine_occurrences_active",
        "routine_occurrences_schedule",
    ];
    let mut seen = Vec::new();
    let mut snapshot = Vec::new();
    let mut statement = db.prepare("SELECT type,name,tbl_name FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let kind: String = row.get(0)?;
        let name: String = row.get(1)?;
        let table: String = row.get(2)?;
        let mut item = json!({"type":kind,"name":name,"table":table});
        let columns = match kind.as_str() {
            "table" => Some(("columns", "SELECT json_group_array(json_array(cid,name,type,\"notnull\",dflt_value,pk)) FROM pragma_table_info(?)")),
            "index" => Some(("keys", "SELECT json_group_array(json_array(seqno,cid,name,\"desc\",coll,key)) FROM pragma_index_xinfo(?)")),
            _ => None,
        };
        if let Some((key, query)) = columns {
            let columns: String = db.query_row(query, [&name], |row| row.get(0))?;
            item[key] = serde_json::from_str(&columns)?;
        }
        if kind == "index" && indexes.contains(&name.as_str()) {
            seen.push(name);
        }
        snapshot.push(item);
    }
    assert_eq!(seen, indexes);
    Ok(snapshot)
}

#[tokio::test]
async fn canonical_invitation_delivers_in_flight_outbox_after_restart() -> Result<()> {
    let root = TestRoot::new()?;
    let identity = Identity::new(&root)?;
    let deliveries = Arc::new(Mutex::new(Vec::<String>::new()));
    let route = Router::new().route(
        "/emails",
        post({
            let deliveries = deliveries.clone();
            move |body: String| {
                let deliveries = deliveries.clone();
                async move {
                    if let Ok(mut calls) = deliveries.lock() {
                        calls.push(body);
                    }
                    Json(json!({"id":"remote-mail-1"}))
                }
            }
        }),
    );
    let upstream = Loopback::start(route).await?;
    let mut options = config(Some(&identity));
    let key = id();
    options["mailFrom"] = json!("noreply@example.com");
    options["mailOrigin"] = json!(ORIGIN);
    options["mailApiKey"] = json!("resend_fixture");
    options["installationId"] = json!(id());
    options["mailEndpoint"] = json!(format!("http://{}/emails", upstream.address));
    options["testKeyring"] = json!({
        "mail-aead/active":key,
        format!("mail-aead/{key}"):URL_SAFE_NO_PAD.encode([7;32]),
        "mail-lookup/v1":URL_SAFE_NO_PAD.encode([9;32]),
    });
    let app = root.start(options.clone()).await?;
    let actor = identity.token("fixture", json!({}))?;
    let project = id();
    let command = id();
    let create = request(&app, Method::POST, "/api/projects", Some(&actor), &command)
        .json(&json!({"requestId":command,"id":project,"name":"Mail fixture"}));
    json(create, 201).await?;
    let invitation = id();
    let create = request(&app, Method::POST, &format!("/api/projects/{project}/invitations"), Some(&actor), &invitation)
        .json(&json!({"requestId":invitation,"email":"member@example.com","agentAssignments":[],"expiresAt":CLOCK+60000,"expectedProjectRevision":1}));
    json(create, 201).await?;
    assert!(app.terminate(Signal::TERM).await?.success());
    let db = Connection::open(root.database())?;
    assert_eq!(
        db.execute(
            "UPDATE mail_outbox SET state='sending',next_attempt=? WHERE id=?",
            rusqlite::params![CLOCK - 1, invitation]
        )?,
        1
    );

    let active = id();
    options["testKeyring"]["mail-aead/active"] = json!(active);
    options["testKeyring"][format!("mail-aead/{active}")] = json!(URL_SAFE_NO_PAD.encode([11; 32]));
    let _app = root.start(options).await?;
    until(async || {
        let state: String = db.query_row(
            "SELECT state FROM mail_outbox WHERE id=?",
            [&invitation],
            |row| row.get(0),
        )?;
        Ok((state == "sent").then_some(()))
    })
    .await?;
    let deliveries = deliveries.lock().map_err(|_| "delivery log poisoned")?;
    assert_eq!(deliveries.len(), 1);
    assert!(deliveries[0].contains("member@example.com"));
    assert!(deliveries[0].contains("Minarbot"));
    Ok(())
}
