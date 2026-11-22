// Adapted from T3 Code's MCP App contracts, snapshot and requests (MIT, app/t3/NOTICE).
mod document;

use super::*;
use axum::http::header;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::Map;
use std::time::Duration;

const READ_LIMIT: usize = 2 * 1024 * 1024;
const MAX_HTML: usize = 1024 * 1024;
const MAX_APPS: usize = 4;

#[derive(Serialize)]
struct Reference {
    id: String,
    title: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Bootstrap {
    csp: document::Csp,
    server: String,
    tool: Value,
    arguments: Map<String, Value>,
    result: ToolResult,
}

pub(super) struct Snapshot {
    html: String,
    title: String,
    bootstrap: Bootstrap,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolCall {
    name: String,
    #[serde(default)]
    arguments: Map<String, Value>,
    approval: Option<String>,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
enum ToolReply {
    Approval { id: String },
    Complete { result: ToolResult },
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolResult {
    content: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    structured_content: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_error: Option<bool>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    meta: Option<Map<String, Value>>,
}

fn missing() -> Failure {
    error(404, "MCP app not found.")
}

fn upstream() -> Failure {
    error(502, "MCP app server request failed.")
}

fn ui_uri(uri: &str) -> bool {
    uri.strip_prefix("ui://").is_some_and(|path| {
        !path.is_empty() && uri.len() <= 4096 && !uri.chars().any(char::is_whitespace)
    })
}

pub(super) fn is_ui_resource(item: &Value) -> bool {
    (item["type"] == "resource" && ui_uri(string(&item["resource"], "uri")))
        || (item["type"] == "resource_link" && ui_uri(string(item, "uri")))
}

pub(super) fn visible(tool: &Value, audience: &str) -> bool {
    tool["_meta"]["ui"]
        .get("visibility")
        .is_none_or(|visibility| {
            visibility
                .as_array()
                .is_some_and(|values| values.iter().any(|v| v == audience))
        })
}

pub(super) async fn discover(
    state: &AppState,
    turn: &Value,
    session: &mut client::Session,
    server: &str,
    tool: &Value,
    args: &Value,
    response: &Value,
) -> Option<Snapshot> {
    if response["isError"] == true || !crate::files::rich_html::available(state, turn) {
        return None;
    }
    let uri = tool["_meta"]["ui"]["resourceUri"]
        .as_str()
        .or_else(|| tool["_meta"]["ui/resourceUri"].as_str())
        .or_else(|| {
            (tool["_meta"]["minarbot.kind"] == "resource")
                .then(|| string(&tool["_meta"], "minarbot.target"))
        })
        .filter(|uri| ui_uri(uri))?;
    let matching = |value: &Value| {
        value["uri"] == uri
            && string(value, "mimeType")
                .replace(char::is_whitespace, "")
                .eq_ignore_ascii_case("text/html;profile=mcp-app")
    };
    let items = response["content"].as_array()?;
    let resource = if let Some(item) = items
        .iter()
        .find(|v| v["type"] == "resource" && matching(&v["resource"]))
    {
        item["resource"].clone()
    } else {
        let read = tokio::time::timeout(Duration::from_secs(6), session.read_app(uri, READ_LIMIT))
            .await
            .ok()?
            .ok()?;
        read["contents"]
            .as_array()?
            .iter()
            .find(|v| matching(v))?
            .clone()
    };
    let html = if let Some(text) = resource["text"].as_str() {
        text.to_owned()
    } else {
        let blob = resource["blob"].as_str()?;
        String::from_utf8(STANDARD.decode(blob).ok()?).ok()?
    };
    if html.trim().is_empty() || html.len() > MAX_HTML {
        return None;
    }
    let title = tool["title"]
        .as_str()
        .or_else(|| tool["annotations"]["title"].as_str())
        .filter(|title| {
            !title.trim().is_empty()
                && title.chars().count() <= 200
                && !title.chars().any(char::is_control)
        })
        .unwrap_or_else(|| string(tool, "name"))
        .chars()
        .take(200)
        .collect();
    let bootstrap = Bootstrap {
        csp: document::Csp::read(state, &resource["_meta"]["ui"]["csp"]),
        server: server.to_owned(),
        tool: tool.clone(),
        arguments: args.as_object()?.clone(),
        result: tool_result(response),
    };
    Some(Snapshot {
        html,
        title,
        bootstrap,
    })
}

fn tool_result(response: &Value) -> ToolResult {
    ToolResult {
        content: response["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| !is_ui_resource(item))
            .cloned()
            .collect(),
        structured_content: response["structuredContent"].as_object().cloned(),
        is_error: response["isError"].as_bool(),
        meta: response["_meta"].as_object().cloned(),
    }
}

fn reflects(value: &Value, secret: &str) -> bool {
    match value {
        Value::String(text) => secrets::reflected(text.as_bytes(), secret),
        Value::Array(items) => items.iter().any(|item| reflects(item, secret)),
        Value::Object(fields) => fields.iter().any(|(key, value)| {
            secrets::reflected(key.as_bytes(), secret)
                || reflects(value, secret)
                || matches!(key.as_str(), "data" | "blob")
                    && value
                        .as_str()
                        .and_then(|blob| STANDARD.decode(blob).ok())
                        .is_some_and(|bytes| secrets::reflected(&bytes, secret))
        }),
        _ => false,
    }
}

fn protected(
    state: &AppState,
    db: &Connection,
    token: &str,
    check: impl Fn(&str) -> bool,
) -> Result<bool> {
    let mut values = secrets::project(state, db, None)?;
    values.extend(crate::computer::vault::redactions(state, db)?);
    Ok((!token.is_empty() && check(token)) || values.iter().any(|secret| check(secret)))
}

pub(super) fn save(
    state: &AppState,
    db: &Connection,
    turn: &str,
    connection: &Value,
    token: &str,
    snapshot: Snapshot,
) -> Result<bool> {
    let bootstrap = json!(snapshot.bootstrap);
    if protected(state, db, token, |secret| {
        secrets::reflected(snapshot.html.as_bytes(), secret)
            || secrets::reflected(snapshot.title.as_bytes(), secret)
            || reflects(&bootstrap, secret)
    })? {
        return Ok(false);
    }
    let id = uuid::Uuid::new_v4().to_string();
    let count = db.execute(
        "INSERT INTO mcp_apps(id,turn,connection,revision,title,html,bootstrap)
         SELECT ?,?,?,?,?,?,? WHERE (SELECT count(*) FROM mcp_apps WHERE turn=?)<?",
        params![
            id,
            turn,
            connection["id"].as_str(),
            connection["revision"].as_i64(),
            snapshot.title,
            snapshot.html,
            bootstrap.to_string(),
            turn,
            MAX_APPS
        ],
    )?;
    Ok(count != 0)
}

pub fn references(db: &Connection, turn: &str) -> Result<Value> {
    if !data::table(db, "mcp_apps")? {
        return Ok(Value::Null);
    }
    let mut statement =
        db.prepare_cached("SELECT id,title FROM mcp_apps WHERE turn=? ORDER BY rowid")?;
    let refs = statement
        .query_map([turn], |row| {
            Ok(Reference {
                id: row.get(0)?,
                title: row.get(1)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(if refs.is_empty() {
        Value::Null
    } else {
        json!(refs)
    })
}

fn scoped_turn(db: &Connection, kind: &str, owner: &str, id: &str) -> Result<Value> {
    data::conversations::owner(db, kind, owner)?;
    let scope = data::conversations::clause(kind);
    one(
        db,
        &format!(
            "SELECT turns.* FROM turns WHERE {scope} AND id=(SELECT turn FROM mcp_apps WHERE id=?)"
        ),
        [owner, id],
    )?
    .ok_or_else(missing)
}

pub fn context_allowed(
    state: &AppState,
    db: &Connection,
    kind: &str,
    owner: &str,
    id: &str,
    context: &str,
) -> Result<bool> {
    if state.auth.is_some() || !data::table(db, "mcp_apps")? {
        return Ok(false);
    }
    if let Err(failure) = scoped_turn(db, kind, owner, id) {
        return if failure.status == 404 {
            Ok(false)
        } else {
            Err(failure)
        };
    }
    let connection = one(db, "SELECT c.url,c.token FROM mcp_apps a JOIN mcp_connections c ON c.id=a.connection WHERE a.id=?", [id])?.ok_or_else(missing)?;
    let token = client::decrypt(
        state,
        string(&connection, "token"),
        string(&connection, "url"),
    )?;
    if protected(state, db, &token, |secret| {
        secrets::reflected(context.as_bytes(), secret)
    })? {
        return Err(error(403, "MCP app context contains protected data."));
    }
    Ok(true)
}

fn granted_tool(
    db: &Connection,
    turn: &Value,
    app: &Value,
    target: &ToolCall,
) -> Result<(Value, Value)> {
    let id = string(app, "connection");
    let row = execution::granted(db, turn, id, &target.name)?;
    if row["revision"] != app["revision"] {
        return Err(error(409, "MCP connection changed. Open a new app reply."));
    }
    if !data::json(&row["permitted"])?
        .as_array()
        .is_some_and(|names| names.contains(&app["source_tool"]))
    {
        return Err(error(403, "The producing MCP tool is no longer granted."));
    }
    let tool = data::json(&row["tools"])?
        .as_array()
        .and_then(|tools| tools.iter().find(|tool| tool["name"] == target.name))
        .filter(|tool| tool["_meta"].get("minarbot.kind").is_none() && visible(tool, "app"))
        .cloned()
        .ok_or_else(|| error(403, "MCP app cannot call this tool."))?;
    if !super::validate::valid(&tool["inputSchema"], &json!(target.arguments))
        || tool["execution"]["taskSupport"] == "required"
    {
        return Err(error(400, "Arguments do not match the MCP tool schema."));
    }
    Ok((row, tool))
}

async fn call_tool(
    state: &AppState,
    kind: &str,
    owner: &str,
    id: &str,
    target: ToolCall,
) -> Result<ToolReply> {
    if target.name.trim().is_empty()
        || target.name.len() > 256
        || json!(target.arguments).to_string().len() > 32_000
        || target
            .approval
            .as_deref()
            .is_some_and(|approval| !uuid(approval))
    {
        return Err(error(400, "Invalid MCP app tool request."));
    }
    let args = json!(target.arguments);
    let (app, row, tool, digest, token) = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        let conversation = data::conversations::owner(db, kind, owner)?;
        if conversation["archived"] == 1 {
            return Err(error(403, "Conversation is archived."));
        }
        let turn = scoped_turn(db, kind, owner, id)?;
        let app = one(
            db,
            "SELECT connection,revision,json_extract(bootstrap,'$.tool.name') AS source_tool FROM mcp_apps WHERE id=?",
            [id],
        )?
        .ok_or_else(missing)?;
        let (row, tool) = granted_tool(db, &turn, &app, &target)?;
        let token = client::decrypt(state, string(&row, "token"), string(&row, "url"))?;
        if protected(state, db, &token, |secret| reflects(&args, secret))? {
            return Err(error(403, "MCP app arguments contain protected data."));
        }
        let read_only = tool["annotations"]["readOnlyHint"] == true;
        let digest = registry::hash(&crate::db::canonical(&json!([
            target.name,
            args,
            row["revision"],
            row["grant_revision"]
        ])));
        if !read_only && target.approval.is_none() {
            let approval = uuid::Uuid::new_v4().to_string();
            db.execute(
                "INSERT INTO mcp_app_approvals(app,id,digest,expires) VALUES(?,?,?,?)
                 ON CONFLICT(app) DO UPDATE SET id=excluded.id,digest=excluded.digest,expires=excluded.expires",
                params![id, approval, digest, state.config.now() + 120_000],
            )?;
            return Ok(ToolReply::Approval { id: approval });
        }
        (app, row, tool, digest, token)
    };
    let mut session = client::Session::open(string(&row, "url"), &token)
        .await
        .map_err(|_| upstream())?;
    let live =
        connections::clean_tools(&session.catalogue().await.map_err(|_| upstream())?, &token)
            .map_err(|_| upstream())?;
    if !live.as_array().is_some_and(|tools| tools.contains(&tool)) {
        return Err(error(
            409,
            "MCP tool definition changed. Test the connection again.",
        ));
    }
    {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        let turn = scoped_turn(db, kind, owner, id)?;
        if data::conversations::owner(db, kind, owner)?["archived"] == 1
            || granted_tool(db, &turn, &app, &target)?.0["grant_revision"] != row["grant_revision"]
        {
            return Err(error(409, "MCP app permissions changed."));
        }
        if let Some(approval) = &target.approval {
            if db.execute(
                "DELETE FROM mcp_app_approvals WHERE app=? AND id=? AND digest=? AND expires>?",
                params![id, approval, digest, state.config.now()],
            )? != 1
            {
                return Err(error(403, "MCP app approval expired or does not match."));
            }
        }
    }
    let response = session.call(&tool, &args).await.map_err(|_| {
        error(
            502,
            "MCP tool outcome is uncertain. Check the server before trying again.",
        )
    })?;
    let result = tool_result(&response);
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    let turn = scoped_turn(db, kind, owner, id)?;
    if granted_tool(db, &turn, &app, &target)?.0["grant_revision"] != row["grant_revision"] {
        return Err(error(409, "MCP app permissions changed after the call."));
    }
    let serialized = json!(&result);
    if protected(state, db, &token, |secret| reflects(&serialized, secret))? {
        return Err(error(502, "MCP tool result contains protected data."));
    }
    Ok(ToolReply::Complete { result })
}

pub async fn route(
    state: &AppState,
    request: Request,
    kind: &str,
    owner: &str,
    id: &str,
    action: Option<&str>,
) -> Result<Response> {
    crate::files::origin(state, &request)?;
    if request.headers().contains_key(header::AUTHORIZATION)
        || request.uri().query().is_some()
        || crate::text_header(request.headers(), "sec-fetch-site")
            .is_some_and(|site| !matches!(site, "same-origin" | "none"))
        || !matches!(kind, "agents" | "threads" | "channels")
        || !uuid(owner)
        || !uuid(id)
    {
        return Err(missing());
    }
    match (request.method().clone(), action) {
        (Method::GET, None | Some("data")) => {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            let db = &guard.connection;
            scoped_turn(db, kind, owner, id)?;
            let fields = if action.is_none() {
                "html,bootstrap"
            } else {
                "bootstrap"
            };
            let app = one(
                db,
                &format!("SELECT {fields} FROM mcp_apps WHERE id=?"),
                [id],
            )?
            .ok_or_else(missing)?;
            let mut data = data::json(&app["bootstrap"])?;
            if action.is_some() {
                data["hostVersion"] = json!(crate::updates::version());
                return Ok(crate::json_response(200, data));
            }
            let bootstrap: Bootstrap =
                serde_json::from_value(data).map_err(|_| Failure::storage())?;
            document::serve(string(&app, "html"), &bootstrap.csp)
        }
        (Method::POST, Some("call")) => {
            crate::files::mutation(state, &request)?;
            let bytes = axum::body::to_bytes(request.into_body(), 64_000)
                .await
                .map_err(|_| error(413, "MCP app request is too large."))?;
            let target = serde_json::from_slice(&bytes)
                .map_err(|_| error(400, "Invalid MCP app tool request."))?;
            Ok(crate::json_response(
                200,
                json!(call_tool(state, kind, owner, id, target).await?),
            ))
        }
        _ => Err(missing()),
    }
}
