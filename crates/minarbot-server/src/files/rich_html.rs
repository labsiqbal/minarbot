// Adapted from T3 Code (MIT, app/t3/NOTICE). See app/t3/provenance.json.
mod headless;

use super::*;
use axum::{http::HeaderValue, response::IntoResponse};
use rusqlite::{params, OptionalExtension};

const MAX_HTML: usize = 512_000;
pub const MCP_INPUT_LIMIT: usize = 6 * MAX_HTML + 4096;
pub const MCP_PREVIEW_LIMIT: usize = 2 * 1024 * 1024;
const MAX_CONTEXT: usize = 4_096;
const LIBRARIES: [&str; 3] = [
    "https://cdn.jsdelivr.net/npm/chart.js@4.4.9/dist/chart.umd.js",
    "https://cdn.jsdelivr.net/npm/d3@7.9.0/dist/d3.min.js",
    "https://cdn.jsdelivr.net/npm/mermaid@10.9.5/dist/mermaid.min.js",
];
const LIBRARY_DIGESTS: [&str; 3] = [
    "de315773454b6076b63990bfa05ce5155a37f71992c87d7bb5f9a2ea698697ef",
    "f2094bbf6141b359722c4fe454eb6c4b0f0e42cc10cc7af921fc158fceb86539",
    "616a109f19cd186842e11d45b35ac07456b3a75513310f6ea075351aa430b1e2",
];
const RESTRICTIONS: &str = "default-src 'none'; style-src 'unsafe-inline'; img-src data: blob:; connect-src 'none'; frame-src 'none'; worker-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'";
const BOOTSTRAP: &str = include_str!("rich_html/bootstrap.html");

pub fn available(state: &AppState, turn: &Value) -> bool {
    state.auth.is_none() && !text(turn, "origin").starts_with("peer:") && turn["profile_setup"] != 1
}

fn source(state: &AppState, db: &Connection, turn: &Value, args: &Value) -> Result<String> {
    let html = match (args.get("html"), args.get("file"), args.get("path")) {
        (Some(Value::String(html)), None, None) => html.clone(),
        (None, Some(Value::String(id)), None) if valid_id(id) => {
            let file = crate::turns::readable_file(db, turn, id)?
                .filter(|file| html_file(std::path::Path::new(text(&file.meta, "name"))))
                .ok_or_else(|| error(404, "HTML file not found in this conversation."))?;
            String::from_utf8(file.bytes).map_err(|_| error(415, "HTML must use UTF-8."))?
        }
        (None, None, Some(Value::String(path))) => {
            let binding = crate::turns::tools::workspace(state, db, turn)?
                .ok_or_else(|| error(403, "Workspace read access is unavailable."))?;
            let path = std::path::Path::new(path);
            let relative = if path.is_absolute() {
                path.strip_prefix(&binding.directory)
                    .map_err(|_| error(403, "HTML file must be inside the active workspace."))?
            } else {
                path
            };
            let relative = relative
                .to_str()
                .filter(|_| html_file(relative))
                .ok_or_else(|| error(400, "Choose a workspace .html or .htm file."))?;
            let (_, bytes) = workspace::download_bounded(&binding.directory, relative, MAX_HTML)?;
            String::from_utf8(bytes).map_err(|_| error(415, "HTML must use UTF-8."))?
        }
        _ => return Err(error(400, "Pass exactly one of html, file, or path.")),
    };
    if html.trim().is_empty() {
        return Err(error(400, "HTML is empty."));
    }
    if html.len() > MAX_HTML {
        return Err(error(413, "HTML exceeds 512,000 UTF-8 bytes."));
    }
    Ok(html)
}

fn html_file(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("html") || extension.eq_ignore_ascii_case("htm")
        })
}

fn safe_input(state: &AppState, db: &Connection, input: &str) -> Result<()> {
    let secrets = crate::computer::vault::redactions(state, db)?;
    if crate::mcp::secrets::redact(input, &secrets, false) != input
        || secrets
            .iter()
            .any(|secret| crate::mcp::secrets::reflected(input.as_bytes(), secret))
    {
        return Err(error(403, "HTML replies cannot include vault secrets."));
    }
    Ok(())
}

pub fn publish(
    state: &AppState,
    db: &Connection,
    turn: &Value,
    call: &Value,
    args: &Value,
) -> Result<Value> {
    if !available(state, turn) {
        return Err(error(
            403,
            "HTML reply is unavailable for this conversation.",
        ));
    }
    if !keys(args, &["html", "file", "path", "title", "height"]) {
        return Err(error(400, "Invalid HTML render arguments."));
    }
    let title = text(args, "title").trim();
    if title.is_empty() || title.chars().count() > 200 || title.chars().any(char::is_control) {
        return Err(error(400, "Title must contain 1 to 200 characters."));
    }
    let height = args["height"]
        .as_u64()
        .filter(|height| (80..=2000).contains(height))
        .ok_or_else(|| error(400, "Initial height must be 80 to 2000 CSS pixels."))?;
    let html = source(state, db, turn, args)?;
    safe_input(state, db, &html)?;
    safe_input(state, db, title)?;
    let id = text(turn, "id");
    let call_id = text(call, "id");
    if let Some(prior) = one(
        db,
        "SELECT id,title,height FROM html_replies WHERE turn=? AND call_id=?",
        [id, call_id],
    )? {
        return Ok(json!({"htmlRender":prior, "message":"Shown inline above your reply."}));
    }
    let count: u64 = db.query_row(
        "SELECT count(*) FROM html_replies WHERE turn=?",
        [id],
        |r| r.get(0),
    )?;
    if count >= 4 {
        return Err(error(429, "A turn can show up to four HTML replies."));
    }
    let attachment = uuid::Uuid::new_v4().to_string();
    db.execute(
        "INSERT INTO html_replies(id,turn,call_id,title,height,html) VALUES(?,?,?,?,?,?)",
        params![attachment, id, call_id, title, height, html],
    )?;
    Ok(
        json!({"htmlRender":{"id":attachment,"title":title,"height":height},"message":"Shown inline above your reply."}),
    )
}

pub fn references(db: &Connection, turn: &str) -> Result<Vec<Value>> {
    rows(
        db,
        "SELECT id,title,height FROM html_replies WHERE turn=? ORDER BY rowid",
        [turn],
    )
}

fn policy(offline: bool) -> String {
    let scripts = if offline {
        "data:".to_owned()
    } else {
        LIBRARIES.join(" ")
    };
    format!("{RESTRICTIONS}; script-src 'unsafe-inline' {scripts}")
}

fn document(source: &str, preview: Option<&str>) -> String {
    // An implicit HTML head lets the bootstrap run before any authored markup.
    let meta = preview.map_or_else(String::new, |mode| format!(
        "<meta http-equiv=\"Content-Security-Policy\" content=\"{}\"><meta name=\"minarbot-appearance\" content=\"{mode}\">",
        policy(true)
    ));
    format!("<!doctype html><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">{meta}{BOOTSTRAP}{source}")
}

pub fn serve(
    state: &AppState,
    request: &Request,
    kind: &str,
    owner: &str,
    id: &str,
) -> Result<Response> {
    origin(state, request)?;
    if request.method() != Method::GET
        || request.headers().contains_key(header::AUTHORIZATION)
        || request.uri().query().is_some()
        || crate::text_header(request.headers(), "sec-fetch-site")
            .is_some_and(|site| !matches!(site, "same-origin" | "none"))
        || !valid_id(owner)
        || !valid_id(id)
    {
        return Err(error(404, "HTML reply not found."));
    }
    let db = state.database.lock().map_err(|_| Failure::storage())?;
    let condition = match kind {
        "agents" => "t.agent=?2 AND t.thread IS NULL AND t.channel IS NULL",
        "threads" => "t.thread=?2",
        "channels" => "t.channel=?2",
        _ => return Err(error(404, "HTML reply not found.")),
    };
    context(&db.connection, kind, owner)?;
    let html: String = db.connection.query_row(
        &format!("SELECT h.html FROM html_replies h JOIN turns t ON t.id=h.turn WHERE h.id=?1 AND {condition}"),
        [id, owner],
        |r| r.get(0),
    ).optional()?.ok_or_else(|| error(404, "HTML reply not found."))?;
    if html.len() > MAX_HTML {
        return Err(Failure::storage());
    }
    let csp = format!(
        "{}; sandbox allow-scripts allow-forms; frame-ancestors 'self'",
        policy(false)
    );
    let mut response = (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CONTENT_DISPOSITION, "inline"),
            (header::X_FRAME_OPTIONS, "SAMEORIGIN"),
        ],
        document(&html, None),
    )
        .into_response();
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(&csp).map_err(|_| Failure::storage())?,
    );
    response.headers_mut().insert(
        "permissions-policy",
        HeaderValue::from_static(
            "camera=(), microphone=(), geolocation=(), payment=(), usb=(), display-capture=()",
        ),
    );
    Ok(response)
}

pub async fn preview(state: &AppState, turn: &Value, args: &Value) -> Result<Value> {
    if !available(state, turn) {
        return Err(error(
            403,
            "HTML preview is unavailable for this conversation.",
        ));
    }
    if !keys(args, &["html", "file", "path", "width", "appearance"]) {
        return Err(error(400, "Invalid HTML preview arguments."));
    }
    let width = match args.get("width") {
        None => 736,
        Some(v) => v
            .as_u64()
            .filter(|v| (240..=1600).contains(v))
            .ok_or_else(|| error(400, "Preview width must be 240 to 1600 CSS pixels."))?,
    };
    let appearance = match args.get("appearance") {
        None => "dark",
        Some(v) => v
            .as_str()
            .filter(|v| matches!(*v, "dark" | "light"))
            .ok_or_else(|| error(400, "Preview appearance must be dark or light."))?,
    };
    let html = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        if one(
            db,
            "SELECT 1 FROM turns WHERE id=? AND status='running'",
            [text(turn, "id")],
        )?
        .is_none()
        {
            return Err(error(409, "Task has stopped."));
        }
        let html = source(state, db, turn, args)?;
        safe_input(state, db, &html)?;
        html
    };
    let mut result = headless::capture(html, width, appearance).await?;
    let db = state.database.lock().map_err(|_| Failure::storage())?;
    let secrets = crate::computer::vault::redactions(state, &db.connection)?;
    crate::mcp::secrets::redact_content(&mut result["consoleMessages"], &secrets);
    Ok(result)
}

pub fn accept_context(
    state: &AppState,
    db: &Connection,
    kind: &str,
    owner: &str,
    input: &Value,
) -> Result<Value> {
    let id = text(input, "renderId");
    let data = text(input, "data");
    if !keys(input, &["renderId", "data"])
        || !valid_id(id)
        || data.len() > MAX_CONTEXT
        || serde_json::from_str::<Value>(data).is_err()
    {
        return Err(error(400, "Invalid HTML reply context (4 KiB maximum)."));
    }
    let turn = one(
        db,
        "SELECT t.* FROM html_replies h JOIN turns t ON t.id=h.turn WHERE h.id=?",
        [id],
    )?
    .ok_or_else(|| error(404, "HTML reply context is unavailable."))?;
    if crate::turns::execution::scope(&turn) != format!("{kind}:{owner}") {
        return Err(error(404, "HTML reply context is unavailable."));
    }
    safe_input(state, db, data)?;
    Ok(input.clone())
}

pub fn turn_context(db: &Connection, turn: &str) -> Result<String> {
    Ok(db
        .query_row(
            "SELECT context FROM html_reply_context WHERE turn=?",
            [turn],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_default())
}
