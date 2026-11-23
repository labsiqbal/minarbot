use super::*;
use authority::{Context, ADMIN};
use axum::response::sse::{Event, Sse};
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest, Sha256};
use std::{convert::Infallible, fs, path::Path};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct WebConfig {
    dist_path: PathBuf,
    #[serde(default)]
    development_identity: bool,
}
pub(super) struct Assets {
    react: String,
    leptos: Option<String>,
    files: HashMap<String, Asset>,
    development_identity: bool,
}
struct Asset {
    bytes: Vec<u8>,
    gzip: Option<Vec<u8>>,
    leptos: bool,
}

impl Assets {
    pub(super) fn load(config: &WebConfig) -> crate::db::Result<Self> {
        let dist = &config.dist_path;
        let manifest: Value = serde_json::from_slice(&fs::read(dist.join(".vite/manifest.json"))?)?;
        let mut files = HashMap::new();
        let mut queue = vec!["hub.html"];
        let mut visited = std::collections::HashSet::new();
        while let Some(key) = queue.pop() {
            if !visited.insert(key) {
                continue;
            }
            let entry = manifest
                .get(key)
                .ok_or("Hub entry missing from build manifest.")?;
            let mut paths = vec![text(entry, "file")];
            for field in ["css", "assets"] {
                if let Some(items) = entry[field].as_array() {
                    for item in items {
                        paths.push(item.as_str().ok_or("Invalid hub asset.")?);
                    }
                }
            }
            for path in paths {
                let Some(name) = path.strip_prefix("assets/") else {
                    return Err("Invalid hub asset.".into());
                };
                if !filename(name) {
                    return Err("Invalid hub asset.".into());
                }
                files.insert(
                    format!("/{path}"),
                    Asset {
                        bytes: fs::read(dist.join(path))?,
                        gzip: None,
                        leptos: false,
                    },
                );
            }
            for field in ["imports", "dynamicImports"] {
                if let Some(items) = entry[field].as_array() {
                    for item in items {
                        queue.push(item.as_str().ok_or("Invalid hub asset import.")?);
                    }
                }
            }
        }
        let react = fs::read_to_string(dist.join("hub.html"))?;
        let leptos_root = dist.join("../_leptos");
        let selected = matches!(
            std::env::var("MINARBOT_APPEARANCE_CLIENT").as_deref(),
            Ok("leptos") | Err(std::env::VarError::NotPresent)
        );
        let load_leptos = || -> crate::db::Result<_> {
            let mut assets = HashMap::new();
            let manifest: Vec<String> =
                serde_json::from_slice(&fs::read(leptos_root.join("hub-assets.json"))?)?;
            for path in manifest {
                let name = path
                    .strip_prefix("/_leptos/assets/")
                    .ok_or("Invalid Leptos hub asset.")?;
                if !filename(name)
                    || !name
                        .rsplit_once('.')
                        .and_then(|(v, ext)| {
                            v.rsplit_once('.').map(|(_, hash)| {
                                hex(hash, 16)
                                    && matches!(ext, "js" | "css" | "svg" | "woff2" | "wasm")
                            })
                        })
                        .unwrap_or(false)
                {
                    return Err("Invalid Leptos hub asset.".into());
                }
                let file = leptos_root.join("assets").join(name);
                let gzip_file = leptos_root.join("assets").join(format!("{name}.gz"));
                let gzip = if gzip_file.try_exists()? {
                    Some(fs::read(gzip_file)?)
                } else {
                    None
                };
                assets.insert(
                    path,
                    Asset {
                        bytes: fs::read(file)?,
                        gzip,
                        leptos: true,
                    },
                );
            }
            let html = fs::read_to_string(leptos_root.join("hub.html"))?;
            if !crate::appearance::startup_asset(&html).is_some_and(|src| assets.contains_key(src))
            {
                return Err("Leptos hub startup module is missing from the asset manifest.".into());
            }
            Ok((html, assets))
        };
        let leptos = if selected && leptos_root.join("hub-assets.json").is_file() {
            match load_leptos() {
                Ok((html, assets)) => {
                    files.extend(assets);
                    Some(html)
                }
                Err(error) => {
                    eprintln!("Leptos hub build unavailable, serving React: {error}");
                    None
                }
            }
        } else {
            None
        };
        Ok(Self {
            react,
            leptos,
            files,
            development_identity: config.development_identity,
        })
    }
    fn response(
        &self,
        hub: &Hub,
        path: &str,
        query: &str,
        headers: &HeaderMap,
        central: bool,
    ) -> Result<Option<Response>> {
        if let Some(asset) = self.files.get(path) {
            let mime = match Path::new(path).extension().and_then(|x| x.to_str()) {
                Some("js") => "text/javascript",
                Some("css") => "text/css",
                Some("wasm") => "application/wasm",
                Some("svg") => "image/svg+xml",
                Some("woff2") => "font/woff2",
                _ => "application/octet-stream",
            };
            let gzip = asset
                .gzip
                .as_ref()
                .filter(|_| crate::accepts_gzip(crate::text_header(headers, "accept-encoding")));
            let mut response = (
                [(header::CONTENT_TYPE, mime)],
                gzip.unwrap_or(&asset.bytes).clone(),
            )
                .into_response();
            let out = response.headers_mut();
            if asset.leptos {
                out.insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=31536000, immutable"),
                );
            }
            if asset.gzip.is_some() {
                out.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
            }
            if gzip.is_some() {
                out.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
            }
            return Ok(Some(response));
        }
        if !matches!(path, "/" | "/login" | "/invite" | "/join" | "/nodes/link") {
            return Ok(None);
        }
        if !central && matches!(path, "/login" | "/join" | "/nodes/link")
            || central && path == "/invite"
        {
            return Err(missing());
        }
        let react_override = query_value(query, "ui").as_deref() == Some("react");
        let leptos = self.leptos.as_ref().filter(|_| !react_override);
        let html = leptos.unwrap_or(&self.react);
        let nonce = opaque()?;
        let safe = json!({"accountsHost":hub.config.accounts_host,"domain":hub.config.domain,"developmentIdentity":self.development_identity})
            .to_string().replace('<', "\\u003c").replace('>', "\\u003e").replace('&', "\\u0026");
        let mut fallback = url::form_urlencoded::Serializer::new(String::new());
        fallback
            .extend_pairs(url::form_urlencoded::parse(query.as_bytes()).filter(|(k, _)| k != "ui"));
        fallback.append_pair("ui", "react");
        let fallback = format!("{path}?{}", fallback.finish().replace('&', "&amp;"));
        let page = html.replace("__REACT_HUB__", &fallback).replacen("<head>",
            &format!("<head><meta name=\"csp-nonce\" content=\"{nonce}\"><script nonce=\"{nonce}\" type=\"application/json\" id=\"hub-config\">{safe}</script>"), 1);
        let wasm = if leptos.is_some() {
            " 'wasm-unsafe-eval'"
        } else {
            ""
        };
        html_response(page, &format!("default-src 'none'; script-src 'self' 'nonce-{nonce}'{wasm}; style-src 'self' 'nonce-{nonce}'; font-src 'self'; img-src 'self' data:; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'")).map(Some)
    }
}
fn filename(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}
fn html_response(html: String, csp: &str) -> Result<Response> {
    let mut response = ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response();
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(csp).map_err(|_| Failure::storage())?,
    );
    Ok(response)
}
fn callback() -> Result<Response> {
    // Browser fragment and sessionStorage glue. The server authorizes redemption and consumes the code.
    let script = include_str!("callback.js");
    let hash = STANDARD.encode(Sha256::digest(script));
    html_response(format!("<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><title>Minarbot sign-in</title><body>Completing sign-in...<script>{script}</script></body></html>"),
        &format!("default-src 'none'; script-src 'sha256-{hash}'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"))
}
fn query_value(query: &str, key: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}
fn api_allowed(path: &str, central: bool, post: bool, nodes: bool) -> bool {
    match (central, post) {
        (true, false) => {
            matches!(
                path,
                "/api/session" | "/api/projects" | "/api/nodes" | "/login" | "/auth/bootstrap"
            ) || nodes && path == "/api/nodes/enrollment"
        }
        (true, true) => {
            matches!(
                path,
                "/auth/session"
                    | "/auth/handoff"
                    | "/auth/logout"
                    | "/api/projects"
                    | "/api/agents"
                    | "/api/participations"
                    | "/api/project-options"
            ) || nodes && path == "/api/nodes/enrollment/approve"
        }
        (false, false) => matches!(
            path,
            "/auth/start"
                | "/auth/callback"
                | "/api/session"
                | "/api/invitation-session"
                | "/api/project"
                | "/api/sections"
                | "/api/history"
                | "/api/turn"
                | "/api/events"
                | "/api/members"
                | "/api/invitations"
                | "/api/participants"
                | "/api/turns"
        ),
        (false, true) => matches!(
            path,
            "/auth/redeem"
                | "/auth/logout"
                | "/api/sections"
                | "/api/messages"
                | "/api/members"
                | "/api/invitations"
                | "/api/invitations/revoke"
                | "/api/invitations/accept"
                | "/api/participants/consent"
                | "/api/participants/withdraw"
        ),
    }
}
pub(super) async fn route(
    hub: Arc<Hub>,
    peer: IpAddr,
    request: Request,
    host: String,
    central: bool,
) -> Result<Response> {
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or_default().to_owned();
    let headers = request.headers().clone();
    let post = request.method() == Method::POST;
    if !post && request.method() != Method::GET {
        return Err(missing());
    }
    if !post {
        if let Some(assets) = &hub.assets {
            if let Some(response) = assets.response(&hub, &path, &query, &headers, central)? {
                session::origin(&headers, &host, false)?;
                return Ok(response);
            }
        }
    }
    if !api_allowed(&path, central, post, hub.signer.is_some()) {
        return Err(missing());
    }
    session::origin(&headers, &host, post)?;
    match path.as_str() {
        "/login" | "/auth/bootstrap" => {
            hub.limit(format!("bootstrap:{peer}"), 60, 60_000)?;
            return session::bootstrap(query_value(&query, "state").as_deref());
        }
        "/auth/callback" => return callback(),
        "/api/events" => return events(hub, host, headers, query),
        _ => {}
    }
    let input = if post {
        body(request, true).await?
    } else {
        Value::Null
    };
    let mut db = hub.lock()?;
    transaction(&mut db, |db| {
        let project = hub.registered(db, &host)?;
        match path.as_str() {
            "/auth/start" => {
                hub.limit(format!("start:{peer}"), 60, 60_000)?;
                return session::start(&hub, db, &host);
            }
            "/auth/session" => {
                hub.limit(format!("login:{peer}"), 60, 60_000)?;
                return session::login(&hub, db, &host, &headers, &input);
            }
            "/auth/handoff" => return session::handoff(&hub, db, &host, &headers, &input),
            "/auth/redeem" => {
                hub.limit(format!("redeem:{peer}"), 60, 60_000)?;
                return session::redeem(&hub, db, &host, &headers, &input);
            }
            _ => {}
        }
        let actor = session::get(&hub, db, &host, &headers, post)?;
        if path == "/auth/logout" {
            return session::logout(db, &actor, &input);
        }
        let command = Command {
            path: &path,
            post,
            query: &query,
            input: &input,
        };
        let result = if let Some(project) = &project {
            project_api(
                &hub,
                db,
                &Context {
                    project,
                    account: &actor.account,
                    host: &host,
                },
                &actor,
                &command,
            )?
        } else {
            central_api(&hub, db, &actor, &command)?
        };
        Ok(json_ok(result))
    })
}

struct Command<'a> {
    path: &'a str,
    post: bool,
    query: &'a str,
    input: &'a Value,
}
fn public_session(actor: &session::Actor) -> Value {
    json!({"accountId":actor.account,"name":actor.identity.subject,"csrf":actor.csrf,"expiresAt":actor.expires_at})
}
fn members(db: &Connection, ctx: &Context<'_>) -> Result<Value> {
    Ok(json!(rows(db, "SELECT m.account_id AS accountId,a.subject AS name,m.role,m.status,m.revision FROM memberships m JOIN accounts a ON a.id=m.account_id
        WHERE m.project_id=? AND m.status='active' LIMIT 100", [ctx.project])?))
}
fn sections(db: &Connection, project: &str) -> Result<Value> {
    Ok(json!(rows(
        db,
        "SELECT id,name,revision FROM sections WHERE project_id=? AND archived=0 LIMIT 100",
        [project]
    )?))
}
fn central_api(
    hub: &Hub,
    db: &Connection,
    actor: &session::Actor,
    command: &Command<'_>,
) -> Result<Value> {
    let Command {
        path,
        post,
        query,
        input,
    } = *command;
    match (path, post) {
        ("/api/session", false) => Ok(public_session(actor)),
        ("/api/projects", false) => Ok(json!(rows(db,"SELECT p.id,h.host,p.name,m.role FROM projects p JOIN host_bindings h ON h.project_id=p.id
            JOIN memberships m ON m.project_id=p.id WHERE p.status='active' AND h.active=1 AND m.account_id=? AND m.status='active' LIMIT 100",
            [&actor.account])?)),
        ("/api/projects", true) => {
            hub.limit(format!("create:{}",actor.account),10,60_000)?;
            authority::create_project(hub, db, &actor.account, input)
        }
        ("/api/nodes", false) => Ok(json!(rows(db, "SELECT id,status,installation_id AS installationId FROM nodes WHERE owner_account_id=? LIMIT 100", [&actor.account])?)),
        ("/api/nodes/enrollment", false) => {
            let mut values = url::form_urlencoded::parse(query.as_bytes());
            let (_, code) = values.next().filter(|(key,_)| key == "humanCode").ok_or_else(missing)?;
            if values.next().is_some() { return Err(missing()); }
            node::inspect(hub, db, &actor.account, &code)
        }
        ("/api/nodes/enrollment/approve", true) => {
            closed(input, &["humanCode","fingerprint"])?;
            let code = text(input,"humanCode");
            let preview = node::inspect(hub, db, &actor.account, code)?;
            if preview["fingerprint"] != input["fingerprint"] { return Err(missing()); }
            node::approve(db, actor, code)?;
            Ok(json!({"approved":true}))
        }
        ("/api/agents", true) => {
            hub.limit(format!("manage:{}",actor.account),60,60_000)?;
            closed(input, &["agentId","nodeId","name"])?;
            let agent = id(&input["agentId"])?;
            let node = id(&input["nodeId"])?;
            let name = label(&input["name"],160)?;
            if let Some(prior) = one(db, "SELECT owner_account_id,home_node_id,name FROM agents WHERE id=?", [&agent])? {
                if prior["owner_account_id"] != actor.account || prior["home_node_id"] != node || prior["name"] != name { return Err(denied()); }
            } else {
                if !db.prepare("SELECT 1 FROM nodes WHERE id=? AND owner_account_id=? AND status!='revoked'")?.exists([&node,&actor.account])? { return Err(missing()); }
                db.execute("INSERT INTO agents(id,owner_account_id,home_node_id,name) VALUES(?,?,?,?)",params![agent,actor.account,node,name])?;
            }
            Ok(json!({"agentId":agent}))
        }
        ("/api/participations" | "/api/project-options", true) => {
            hub.limit(format!("manage:{}",actor.account),60,60_000)?;
            let options = path == "/api/project-options";
            closed(input, if options { &["projectHost"][..] } else { &["projectHost","agentId","policy","providerMaxCostMicros"] })?;
            let host = text(input,"projectHost");
            let project = hub.registered(db, host)?.ok_or_else(missing)?;
            let ctx = Context { project: &project, account: &actor.account, host };
            authority::member(db, &ctx, &[])?;
            if options {
                let mut sections = sections(db, &project)?;
                if let Some(items) = sections.as_array_mut() {
                    for item in items {
                        if let Some(item) = item.as_object_mut() { item.remove("revision"); }
                    }
                }
                return Ok(json!({"projectId":project,"members":members(db,&ctx)?,"sections":sections}));
            }
            authority::propose(db,&ctx,input)
        }
        _ => Err(missing()),
    }
}
fn project_api(
    hub: &Hub,
    db: &Connection,
    ctx: &Context<'_>,
    actor: &session::Actor,
    command: &Command<'_>,
) -> Result<Value> {
    let Command {
        path,
        post,
        query,
        input,
    } = *command;
    if path == "/api/invitation-session" {
        return Ok(public_session(actor));
    }
    if path == "/api/invitations/accept" {
        return authority::accept(hub, db, ctx, input, &actor.identity);
    }
    let member = authority::member(db, ctx, &[])?;
    match (path, post) {
        ("/api/session", false) => {
            let mut result = public_session(actor);
            result["projectId"] = json!(ctx.project);
            result["role"] = member["role"].clone();
            Ok(result)
        }
        ("/api/project", false) => one(
            db,
            "SELECT id,slug,name,revision FROM projects WHERE id=?",
            [ctx.project],
        )?
        .ok_or_else(missing),
        ("/api/sections", false) => sections(db, ctx.project),
        ("/api/sections", true) => {
            closed(input, &["name"])?;
            if !ADMIN.contains(&text(&member, "role")) {
                return Err(denied());
            }
            let section = new_id();
            db.execute(
                "INSERT INTO sections(id,project_id,name) VALUES(?,?,?)",
                params![section, ctx.project, label(&input["name"], 160)?],
            )?;
            Ok(json!({"sectionId":section}))
        }
        ("/api/members", false) => members(db, ctx),
        ("/api/members", true) => authority::edit_member(db, ctx, input),
        ("/api/invitations", false) => {
            if !ADMIN.contains(&text(&member, "role")) {
                return Err(denied());
            }
            Ok(json!(rows(db, "SELECT id,role,CASE WHEN state='pending' AND expires_at<=? THEN 'expired' ELSE state END AS state,
                expires_at AS expiresAt,accepted_by AS acceptedBy FROM invitations WHERE project_id=? ORDER BY rowid DESC LIMIT 100",params![now(),ctx.project])?))
        }
        ("/api/invitations", true) => authority::invite(hub, db, ctx, input),
        ("/api/invitations/revoke", true) => authority::revoke_invite(db, ctx, input),
        ("/api/participants", false) => {
            let mut items = rows(db,"SELECT p.id,p.agent_id AS agentId,a.name,a.owner_account_id AS ownerAccountId,ac.subject AS ownerName,n.id AS nodeId,n.status AS nodeStatus,
                p.state,p.revision,p.owner_consent_revision AS ownerConsentRevision,p.project_consent_revision AS projectConsentRevision,p.policy_json AS policy
                FROM participations p JOIN agents a ON a.id=p.agent_id JOIN accounts ac ON ac.id=a.owner_account_id JOIN nodes n ON n.id=a.home_node_id WHERE p.project_id=? LIMIT 100", [ctx.project])?;
            for item in &mut items {
                item["policyDigest"] = json!(digest(text(item, "policy")));
                item["policy"] = stored(item, "policy")?;
            }
            Ok(json!(items))
        }
        ("/api/participants/consent", true) => {
            closed(input, &["participationId", "revision", "policyDigest"])?;
            authority::consent(db, ctx, input, None)?;
            Ok(json!({"approved":true}))
        }
        ("/api/participants/withdraw", true) => authority::withdraw(db, ctx, input),
        ("/api/history", false) => {
            routing::history(db, ctx, &id(&json!(query_value(query, "sectionId")))?)
        }
        ("/api/turns", false) => Ok(json!(routing::turns(
            db,
            ctx,
            &id(&json!(query_value(query, "sectionId")))?
        )?)),
        ("/api/turn", false) => routing::turn(db, ctx, &id(&json!(query_value(query, "turnId")))?),
        ("/api/messages", true) => {
            hub.limit(format!("post:{}", ctx.account), 60, 60_000)?;
            routing::post(db, ctx, input)
        }
        _ => Err(missing()),
    }
}
type Snapshots = Option<(String, String)>;

fn events(hub: Arc<Hub>, host: String, headers: HeaderMap, query: String) -> Result<Response> {
    let section = query_value(&query, "sectionId")
        .map(|v| id(&json!(v)))
        .transpose()?;
    let actor = session::get(&hub, &*hub.lock()?, &host, &headers, false)?;
    let project = actor.project.ok_or_else(missing)?;
    let account = actor.account;
    let expires = actor.expires_at;
    {
        let db = hub.lock()?;
        let ctx = Context {
            project: &project,
            account: &account,
            host: &host,
        };
        authority::member(&db, &ctx, &[])?;
        if let Some(section) = &section {
            authority::section(&db, &ctx, section, &[])?;
        }
    }
    let permit = hub
        .streams
        .clone()
        .try_acquire_owned()
        .map_err(|_| Failure::new(429, "stream_limit", "Retry later."))?;
    let stream = async_stream::stream! {
        let _permit = permit;
        let mut cursor = 0;
        let mut last_history = String::new();
        let mut last_turns = String::new();
        let authorized = |db: &Connection| -> Result<()> {
            let actor = session::get(&hub,db,&host,&headers,false)?;
            if now() >= expires || actor.account != account || actor.project.as_deref() != Some(&project) { return Err(denied()); }
            let ctx = Context { project: &project, account: &account, host: &host };
            if let Some(section) = &section { authority::section(db,&ctx,section,&[])?; }
            else { authority::member(db,&ctx,&[])?; }
            Ok(())
        };
        let still_authorized = || hub.lock().is_ok_and(|db| authorized(&db).is_ok());
        loop {
            let read = || -> Result<(Vec<Value>, Snapshots)> {
                let db = hub.lock()?;
                authorized(&db)?;
                let ctx = Context { project: &project, account: &account, host: &host };
                let events = rows(&db,"SELECT * FROM authority_events WHERE project_id=? AND cursor>? ORDER BY cursor LIMIT 100",params![project,cursor])?;
                let snapshots = if let Some(section) = &section {
                    Some((routing::history(&db,&ctx,section)?.to_string(),json!(routing::turns(&db,&ctx,section)?).to_string()))
                } else { None };
                Ok((events,snapshots))
            };
            let Ok((events,snapshots)) = read() else { break; };
            for value in events {
                if !still_authorized() { return; }
                cursor = value["cursor"].as_i64().unwrap_or(cursor);
                yield Ok::<_, Infallible>(Event::default().id(cursor.to_string()).event("authority").data(value.to_string()));
            }
            if let Some((history,turns)) = snapshots {
                if history != last_history {
                    if !still_authorized() { return; }
                    last_history = history;
                    yield Ok(Event::default().event("section.snapshot").data(last_history.clone()));
                }
                if turns != last_turns {
                    if !still_authorized() { return; }
                    last_turns = turns;
                    yield Ok(Event::default().event("turn.snapshot").data(last_turns.clone()));
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    };
    let mut response = Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok(response)
}
