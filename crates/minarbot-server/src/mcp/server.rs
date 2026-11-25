static FAILURES: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, u32)>>,
> = std::sync::OnceLock::new();
use super::*;
use crate::mcp::validate::valid;
use crate::turns::{harness, messaging};
use axum::{extract::Request, http::Method, response::Response};
use serde_json::{json, Value};
static REQUESTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(16);

fn tools() -> Value {
    json!([
    {"name":"list_agents","description":"List available local agents, ten per page. Continue with next_after.","inputSchema":{"type":"object","properties":{"after":{"type":"string","maxLength":400}},"additionalProperties":false}},
    {"name":"send_message","description":"Queue an isolated message. Retry only with the same requestId.","inputSchema":{"type":"object","properties":{"requestId":{"type":"string","pattern":"^[a-fA-F0-9]{8}-[a-fA-F0-9]{4}-4[a-fA-F0-9]{3}-[89abAB][a-fA-F0-9]{3}-[a-fA-F0-9]{12}$"},"recipient":{"type":"string","minLength":1,"maxLength":400},"recipients":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":400}},"text":{"type":"string","minLength":1,"maxLength":16000,"pattern":"\\S"},"expects_reply":{"type":"boolean"},"metadata":{"type":"object","properties":{"depth":{"type":"integer","minimum":0,"maximum":100}},"required":["depth"],"additionalProperties":false}},"required":["requestId","text"],"additionalProperties":false}},
    {"name":"get_message_status","description":"Read status and up to 8000 characters of reply from a delivery created by this caller. Use read_history for paginated message summaries.","inputSchema":{"type":"object","properties":{"requestId":{"type":"string","pattern":"^[a-fA-F0-9]{8}-[a-fA-F0-9]{4}-4[a-fA-F0-9]{3}-[89abAB][a-fA-F0-9]{3}-[a-fA-F0-9]{12}$"}},"required":["requestId"],"additionalProperties":false}},
    {"name":"cancel_message","description":"Best-effort cancellation of a delivery created by this peer.","inputSchema":{"type":"object","properties":{"requestId":{"type":"string","pattern":"^[a-fA-F0-9]{8}-[a-fA-F0-9]{4}-4[a-fA-F0-9]{3}-[89abAB][a-fA-F0-9]{3}-[a-fA-F0-9]{12}$"}},"required":["requestId"],"additionalProperties":false}},
    {"name":"read_history","description":"Read bounded history, newest first. Peers see only deliveries they created; sessions see only their current conversation before the bound turn.","inputSchema":{"type":"object","properties":{"before":{"type":"integer","minimum":1,"maximum":9007199254740991i64},"limit":{"type":"integer","minimum":1,"maximum":50}},"additionalProperties":false}}
    ])
}
fn allowed(row: &Value, scope: &str) -> bool {
    data::json(&row["scopes"])
        .ok()
        .and_then(|v| v.as_array().cloned())
        .is_some_and(|a| a.iter().any(|v| v == scope))
}
async fn declarations(state: &AppState, caller: &Value) -> Result<Vec<Value>> {
    let mut setup = false;
    let mut automation_model = false;
    let mut web_access = crate::turns::web::Access::default();
    let mut specs = tools().as_array().cloned().unwrap_or_default();
    for t in &mut specs {
        t["scope"] = json!(match string(t, "name") {
            "list_agents" => "agents:read",
            "read_history" => "history:read",
            _ => "agents:message",
        });
    }
    if caller["kind"] == "session" && caller["turn"].is_string() {
        let turn = {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            one(
                &guard.connection,
                "SELECT * FROM turns WHERE id=? AND (status='running' OR (status='queued' AND (paused=1 OR waiting_for IS NOT NULL OR EXISTS(SELECT 1 FROM provider_calls WHERE turn=turns.id AND status='pending'))))",
                [caller["turn"].as_str()],
            )?
            .ok_or_else(|| error(403, "MCP authorization failed."))?
        };
        if turn["agent"] == super::guide::ID {
            return if super::guide::eligible(&turn) {
                super::guide::declarations()
            } else {
                Ok(Vec::new())
            };
        }
        let native_specs = {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            let db = &guard.connection;
            setup = harness::setup(db, &turn)?;
            automation_model = crate::automation::model(db, string(&turn, "id"))?.is_some()
                || (turn["workflow_run"].is_string()
                    && one(
                        db,
                        "SELECT 1 FROM workflow_steps WHERE turn=? AND model IS NOT NULL",
                        [string(&turn, "id")],
                    )?
                    .is_some());
            web_access = crate::turns::web::access(state, db, string(caller, "agent"))?;
            harness::specs(state, db, &turn)?
        };
        for spec in native_specs {
            let f = &spec["function"];
            let name = string(f, "name");
            if ["list_agents", "send_message", "read_history"].contains(&name) {
                continue;
            }
            let scope = if crate::computer::actions::is_computer_tool(name) {
                if crate::computer::actions::is_read_tool(name) {
                    "computer:read"
                } else {
                    "computer:run"
                }
            } else {
                "orchestration"
            };
            specs.push(json!({"name":name,"description":f["description"],"inputSchema":f["parameters"],"scope":scope,"native":true}));
        }
        let composio_tools = crate::connectors::composio::tools(state, &turn).await?;
        for spec in crate::connectors::composio::specs(&composio_tools) {
            let function = &spec["function"];
            specs.push(json!({"name":function["name"],"description":function["description"],"inputSchema":function["parameters"],"scope":"orchestration","native":true}));
        }
    }
    let feature_names: Vec<String> =
        serde_json::from_str::<Vec<Value>>(include_str!("feature-tools.json"))
            .map_err(|_| Failure::storage())?
            .iter()
            .map(|t| string(t, "name").to_owned())
            .collect();
    if automation_model {
        specs.clear();
    }
    specs.retain(|t| {
        !feature_names
            .iter()
            .any(|n| t["name"] == *n && t["native"] != true)
    });
    if !automation_model {
        specs.extend(
            serde_json::from_str::<Vec<Value>>(include_str!("feature-tools.json"))
                .map_err(|_| Failure::storage())?,
        );
    }
    if caller["kind"] == "session" && caller["turn"].is_string() {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let turn = one(
            &guard.connection,
            "SELECT * FROM turns WHERE id=? AND (status='running' OR (status='queued' AND (paused=1 OR waiting_for IS NOT NULL OR EXISTS(SELECT 1 FROM provider_calls WHERE turn=turns.id AND status='pending'))))",
            [string(caller, "turn")],
        )?
        .ok_or_else(|| error(403, "MCP authorization failed."))?;
        if crate::turns::messaging::channel(&guard.connection, string(&turn, "id"))?.is_some() {
            specs.retain(|t| {
                t["native"] == true
                    || !crate::turns::tools::PRIVATE_TOOLS.contains(&string(t, "name"))
            });
        }
        for spec in crate::turns::tools::offered(state, &guard.connection, &turn)? {
            let name = string(&spec["function"], "name");
            if [
                "list_agents",
                "send_message",
                "delegate_task",
                "transfer_task",
                "task_status",
                "wait_task",
                "cancel_task",
                "interrupt_agent",
            ]
            .contains(&name)
            {
                specs.retain(|item| item["name"] != name);
                specs.push(json!({
                    "name": name,
                    "description": spec["function"]["description"],
                    "inputSchema": spec["function"]["parameters"],
                    "scope": "orchestration",
                    "native": true
                }));
            }
        }
    }
    specs.retain(|t| {
        allowed(caller, string(t, "scope"))
            && (!setup || ["edit_profile", "ask_user"].contains(&string(t, "name")))
            && web_access.offered(string(t, "name"))
    });
    Ok(specs)
}
fn public_tools(specs: &[Value]) -> Vec<Value> {
    specs.iter().map(|s|json!({"name":s["name"],"description":s["description"],"inputSchema":s["inputSchema"]})).collect()
}
pub async fn rpc(state: &AppState, request: Request) -> Result<Response> {
    let bearer = crate::text_header(request.headers(), "authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_owned();
    let failures = FAILURES.get_or_init(Default::default);
    let throttled = {
        let mut map = failures.lock().map_err(|_| Failure::storage())?;
        map.retain(|_, v| v.0.elapsed() < std::time::Duration::from_secs(60));
        map.get(&state.origin).is_some_and(|v| v.1 >= 10)
    };
    if throttled {
        let mut response = crate::local_error(401, "MCP authorization failed.");
        response
            .headers_mut()
            .insert("retry-after", axum::http::HeaderValue::from_static("60"));
        return Ok(response);
    }
    let caller = match registry::verify(state, &bearer) {
        Ok(v) => v,
        Err(e) => {
            let mut map = failures.lock().map_err(|_| Failure::storage())?;
            if map.len() >= 1024 {
                map.clear();
            }
            let v = map
                .entry(state.origin.clone())
                .or_insert((std::time::Instant::now(), 0));
            v.1 += 1;
            return Err(e);
        }
    };
    if request.method() != Method::POST {
        return Err(error(405, "Use POST for MCP requests."));
    }
    if crate::text_header(request.headers(), "content-type")
        .unwrap_or("")
        .split(';')
        .next()
        != Some("application/json")
    {
        return Err(error(415, "Use Content-Type application/json."));
    }
    let _permit = REQUESTS
        .try_acquire()
        .map_err(|_| error(429, "MCP request limit reached. Try again later."))?;
    let limit = if caller["kind"] == "session" {
        crate::files::rich_html::MCP_INPUT_LIMIT
    } else {
        64_000
    };
    let bytes = axum::body::to_bytes(request.into_body(), limit)
        .await
        .map_err(|_| error(413, "MCP request exceeds the size limit."))?;
    let caller = registry::verify(state, &bearer)?;
    let body: Value =
        serde_json::from_slice(&bytes).map_err(|_| error(400, "Invalid MCP request."))?;
    if bytes.len() > 64_000
        && (body["method"] != "tools/call"
            || !matches!(
                body["params"]["name"].as_str(),
                Some("html_preview" | "html_render")
            ))
    {
        return Err(error(413, "MCP request exceeds 64 KB."));
    }
    if body["jsonrpc"] != "2.0" || !body["method"].is_string() {
        return Err(error(400, "Invalid MCP request."));
    }
    let method = string(&body, "method");
    if method == "notifications/initialized" {
        return Ok(crate::json_response(202, Value::Null));
    }
    if method == "initialize" {
        return Ok(crate::json_response(
            200,
            json!({"jsonrpc":"2.0","id":body["id"],"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"minarbot","version":"0.1.0"}}}),
        ));
    }
    if method == "tools/list" {
        return Ok(crate::json_response(
            200,
            json!({"jsonrpc":"2.0","id":body["id"],"result":{"tools":public_tools(&declarations(state,&caller).await?)}}),
        ));
    }
    if method != "tools/call" {
        return Ok(crate::json_response(
            200,
            if method == "ping" {
                json!({"jsonrpc":"2.0","id":body["id"],"result":{}})
            } else {
                json!({"jsonrpc":"2.0","id":body["id"],"error":{"code":-32601,"message":"Method not found"}})
            },
        ));
    }
    let params = &body["params"];
    let name = string(params, "name");
    let mut args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let decl = declarations(state, &caller)
        .await?
        .into_iter()
        .find(|t| t["name"] == name)
        .ok_or_else(|| error(403, "MCP authorization failed."))?;
    if name == "read_skill" && !data::closed(&args, &["id"]) {
        return Ok(reply(
            &body["id"],
            Err(error(400, "Invalid skill request.")),
        ));
    }
    // The native tool takes only {agent, task, wait}; the feature tool needs
    // the request identity and owning turn.
    if caller["kind"] == "session"
        && name == "delegate_task"
        && decl["native"] != true
        && args["agent"].is_string()
    {
        args["requestId"] = json!(uuid::Uuid::new_v4().to_string());
        args["turn"] = caller["turn"].clone();
    }
    if caller["kind"] == "session" && caller["agent"] == super::guide::ID {
        let turn = {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            one(
                &guard.connection,
                "SELECT * FROM turns WHERE id=? AND (status='running' OR (status='queued' AND (paused=1 OR waiting_for IS NOT NULL OR EXISTS(SELECT 1 FROM provider_calls WHERE turn=turns.id AND status='pending'))))",
                [string(&caller, "turn")],
            )?
            .ok_or_else(|| error(403, "MCP authorization failed."))?
        };
        return Ok(reply(
            &body["id"],
            super::guide::call(state, &turn, name, &args).await,
        ));
    }
    if decl["scope"].is_string()
        && decl["native"] != true
        && ![
            "list_agents",
            "send_message",
            "read_history",
            "get_message_status",
            "cancel_message",
        ]
        .contains(&name)
    {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        features::authorize(state, &guard.connection, &caller, name, &args)?;
    }
    if decl["native"] != true
        && !(name == "read_history" && caller["kind"] == "session")
        && !valid(&decl["inputSchema"], &args)
    {
        return Err(error(400, "Invalid MCP tool call."));
    }
    if caller["kind"] == "session" && args["recipients"].is_array() {
        let mut native = args.clone();
        if let Some(map) = native.as_object_mut() {
            map.remove("requestId");
            map.remove("metadata");
            map.entry("expects_reply").or_insert(json!(false));
        }
        let call_id = uuid::Uuid::new_v4().to_string();
        let mut result =
            harness::tool(state, string(&caller, "turn"), name, &native, &call_id).await?;
        if native["expects_reply"] == true && result["status"] == "waiting" {
            for _ in 0..1500 {
                let completed = {
                    let guard = state.database.lock().map_err(|_| Failure::storage())?;
                    one(
                        &guard.connection,
                        "SELECT result FROM flow_tool_calls WHERE turn=? AND call_id=?",
                        params![string(&caller, "turn"), call_id],
                    )?
                    .and_then(|row| serde_json::from_str::<Value>(string(&row, "result")).ok())
                    .filter(|value| value["status"] == "completed")
                };
                if let Some(value) = completed {
                    result = value;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }
        return Ok(reply(&body["id"], Ok(result)));
    }
    if name == "render_component" && super::secrets::reflected(args.to_string().as_bytes(), &bearer)
    {
        return Err(error(400, "Component replies cannot include credentials."));
    }
    if decl["native"] == true {
        let turn = string(&caller, "turn");
        let call = json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "type": "function",
            "function": {"name": name, "arguments": args.to_string()},
        });
        let effect = crate::computer::hub::before_effect(state, turn, "tool", call).await?;
        let result = crate::computer::hub::run_effect(
            &effect,
            harness::tool(state, turn, name, &args, &uuid::Uuid::new_v4().to_string()),
        )
        .await;
        crate::computer::hub::finish_effect(
            state,
            effect,
            if result.is_ok() {
                "succeeded"
            } else {
                "unknown"
            },
        )
        .await?;
        let result = result.and_then(|v| {
            if name == "read_skill_file" {
                Ok(v)
            } else if v["error"].is_string() {
                Err(error(400, string(&v, "error")))
            } else {
                Ok(v)
            }
        });
        return Ok(reply(&body["id"], result));
    }
    if ![
        "list_agents",
        "send_message",
        "read_history",
        "get_message_status",
        "cancel_message",
    ]
    .contains(&name)
    {
        let result = features::run(state, &caller, name, &args).await;
        if result
            .as_ref()
            .is_err_and(|e| e.status == 403 && e.message == "MCP authorization failed.")
        {
            return Err(error(403, "MCP authorization failed."));
        }
        return Ok(reply(&body["id"], result));
    }
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    if name == "send_message" {
        if let Some(row) = one(
            db,
            "SELECT digest FROM mcp_server_messages WHERE caller=? AND request_id=?",
            [string(&caller, "id"), string(&args, "requestId")],
        )? {
            if row["digest"] != registry::hash(&send_digest(&args).to_string()) {
                return Err(error(409, "ID was already used for a different message."));
            }
        }
    }
    let result = (|| {
        let value = match name {
            "list_agents" => {
                let mut rows = messaging::roster(
                    db,
                    &json!({"id":caller["turn"],"agent":caller["agent"]}),
                    string(&args, "after"),
                )?;
                for row in &mut rows {
                    if !string(row, "id").contains('@') {
                        row["model"] = data::agents::read(db, string(row, "id"))?["model"].clone();
                    }
                }
                json!({"agents":rows.iter().take(10).collect::<Vec<_>>(),"next_after":if rows.len()>10 {rows[9]["id"].clone()}else{Value::Null}})
            }
            "read_history" => {
                if caller["kind"] == "peer" {
                    let limit = args["limit"].as_i64().unwrap_or(5);
                    let before = args["before"].as_i64().unwrap_or(i64::MAX);
                    let mut rows=all(db,"SELECT t.id,t.seq,t.agent,t.text,t.reply,t.status,t.created FROM mcp_server_messages m JOIN turns t ON t.id=m.turn WHERE m.caller=? AND t.seq<? ORDER BY t.seq DESC LIMIT ?",params![string(&caller,"id"),before,limit+1])?;
                    let next = if rows.len() > limit as usize {
                        rows.get(limit as usize - 1).map(|r| r["seq"].clone())
                    } else {
                        None
                    };
                    rows.truncate(limit as usize);
                    return Ok(json!({"messages":rows,"next_before":next}));
                }
                let turn = caller["turn"].as_str().unwrap_or("");
                let row = one(db, "SELECT * FROM turns WHERE id=?", [turn])?
                    .ok_or_else(|| error(403, "MCP authorization failed."))?;
                harness::history(db, &row, &args)?
            }
            "get_message_status" => {
                if caller["kind"] == "session" {
                    let request_id = string(&args, "requestId");
                    let deliveries = all(
                        db,
                        "SELECT t.status,t.paused,t.reply,t.error FROM agent_deliveries d JOIN turns t ON t.id=d.turn JOIN agent_requests r ON r.id=d.request WHERE r.id=? AND r.turn=?",
                        params![request_id, string(&caller, "turn")],
                    )?;
                    if deliveries.is_empty() {
                        return Err(error(404, "Message not found."));
                    }
                    let active = deliveries.iter().any(|row| {
                        row["paused"] == 1 || ["queued", "running"].contains(&string(row, "status"))
                    });
                    let reply = deliveries
                        .iter()
                        .find(|row| row["status"] == "done")
                        .map(|row| row["reply"].clone())
                        .unwrap_or(Value::Null);
                    return Ok(
                        json!({"requestId":request_id,"status":if active {"waiting"} else {"done"},"reply":reply,"error":Value::Null}),
                    );
                }
                let row=one(db,"SELECT t.* FROM mcp_server_messages m JOIN turns t ON t.id=m.turn WHERE m.caller=? AND m.request_id=?",[caller["id"].as_str().unwrap_or(""),string(&args,"requestId")])?.ok_or_else(||error(404,"Message not found."))?;
                json!({"requestId":args["requestId"],"status":row["status"],"reply":row["reply"],"error":row["error"]})
            }
            "cancel_message" => {
                let row = one(
                    db,
                    "SELECT turn FROM mcp_server_messages WHERE caller=? AND request_id=?",
                    [
                        caller["id"].as_str().unwrap_or(""),
                        string(&args, "requestId"),
                    ],
                )?
                .ok_or_else(|| error(404, "Message not found."))?;
                crate::turns::messaging::cancel(
                    db,
                    &[string(&row, "turn").to_owned()],
                    "Cancelled by peer.",
                )?;
                json!({"requestId":args["requestId"],"status":"cancelled"})
            }
            "send_message" => send(state, db, &caller, &args)?,
            _ => json!({}),
        };
        Ok(value)
    })();
    Ok(reply(&body["id"], result))
}
fn reply(id: &Value, result: Result<Value>) -> Response {
    let value = match result {
        Ok(value) if value["content"].is_array() => value,
        Ok(value) => {
            json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value})
        }

        Err(e) => {
            json!({"isError":true,"content":[{"type":"text","text":json!({"error":e.message,"status":e.status}).to_string()}]})
        }
    };
    crate::json_response(200, json!({"jsonrpc":"2.0","id":id,"result":value}))
}
fn send_digest(args: &Value) -> Value {
    let mut value =
        json!({"recipient":args["recipient"],"recipients":args["recipients"],"text":args["text"]});
    if let Some(expects_reply) = args.get("expects_reply") {
        value["expects_reply"] = expects_reply.clone();
    }
    if let Some(metadata) = args.get("metadata") {
        value["metadata"] = metadata.clone();
    }
    value
}
fn send(state: &AppState, db: &Connection, caller: &Value, args: &Value) -> Result<Value> {
    let request_id = string(args, "requestId");
    if !uuid(request_id) {
        return Err(error(400, "Invalid message request ID."));
    }
    let digest = registry::hash(&send_digest(args).to_string());
    if caller["kind"] == "peer" {
        if !crate::computer::remote::messaging_allowed(db, string(caller, "id"))? {
            return Err(error(403, "Agent messaging is disabled for this peer."));
        }
        let depth = args["metadata"]["depth"].as_i64().unwrap_or(1);
        if depth > state.config.turns.limit("depth", 3) {
            return Err(error(409, "Remote delegation depth limit reached."));
        }
    }
    if let Some(row) = one(
        db,
        "SELECT * FROM mcp_server_messages WHERE caller=? AND request_id=?",
        [string(caller, "id"), request_id],
    )? {
        if row["digest"] != digest {
            return Err(error(409, "ID was already used for a different message."));
        }
        return data::json(&row["result"]);
    }
    let text = field(args, "text", 16000)?;
    let recipients = args["recipients"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 8);
    let recipient = args["recipient"]
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= 400);
    if recipients.is_none() && recipient.is_none() {
        return Err(error(400, "Invalid message recipient."));
    }
    if caller["kind"] == "peer" {
        crate::computer::remote::enforce_peer_rate(
            db,
            string(caller, "id"),
            state.config.now(),
            &state.config.turns.agent_limits.clone().unwrap_or(json!({})),
        )?;
    }
    if caller["kind"] == "session" && caller["turn"].is_string() {
        return transaction(db, || {
            let delivery = harness::message(state, db, caller, args)?;
            let result = json!({"requestId":request_id,"turn":delivery["id"],"thread":delivery["thread"],"status":"queued"});
            db.execute(
                "INSERT INTO mcp_server_messages VALUES(?,?,?,?,?)",
                params![
                    string(caller, "id"),
                    request_id,
                    digest,
                    delivery["id"].as_str(),
                    result.to_string()
                ],
            )?;
            Ok(result)
        });
    }
    let recipient = recipient.ok_or_else(|| error(400, "Invalid message recipient."))?;
    data::agents::read(db, recipient)?;
    if data::agents::hidden(db, recipient)? {
        return Err(error(
            409,
            "Recipient is unavailable or message is invalid.",
        ));
    }
    let thread = uuid::Uuid::new_v4().to_string();
    let turn = uuid::Uuid::new_v4().to_string();
    let sender = if caller["kind"] == "peer" {
        string(caller, "name").to_owned()
    } else {
        string(&data::agents::read(db, string(caller, "agent"))?, "name").to_owned()
    };
    let machine = if caller["kind"] == "peer" {
        sender.strip_prefix("Minarbot: ")
    } else {
        None
    };
    let title = if let Some(machine) = machine {
        format!(
            "{} on {}: {}",
            "Minarbot",
            machine.chars().take(24).collect::<String>(),
            text.split_whitespace().collect::<Vec<_>>().join(" ")
        )
    } else {
        format!(
            "{} asked: {}",
            sender.chars().take(40).collect::<String>(),
            text.split_whitespace().collect::<Vec<_>>().join(" ")
        )
    }
    .chars()
    .take(80)
    .collect::<String>();
    let result = json!({"requestId":request_id,"thread":thread,"turn":turn,"status":"queued"});
    transaction(db, || {
        db.execute(
            "INSERT INTO threads(id,agent,name) VALUES(?,?,?)",
            params![thread, recipient, title],
        )?;
        db.execute(
            "INSERT INTO delivery_conversations(thread,sender,machine,activity) VALUES(?,?,?,?)",
            params![
                thread,
                if machine.is_some() {
                    "Minarbot"
                } else {
                    &sender
                },
                machine,
                state.config.now()
            ],
        )?;
        db.execute("INSERT INTO turns(id,agent,thread,text,status,created,origin,message_depth) VALUES(?,?,?,?,'queued',?,?,?)",params![turn,recipient,thread,text,data::iso(db,state.config.now())?,format!("{}:{}",string(caller,"kind"),sender),args["metadata"]["depth"].as_i64().unwrap_or(1)])?;
        db.execute(
            "INSERT INTO mcp_server_messages VALUES(?,?,?,?,?)",
            params![
                string(caller, "id"),
                request_id,
                digest,
                turn,
                result.to_string()
            ],
        )?;
        Ok(result)
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_schema_supports_batch_coordination() -> Result<()> {
        let definitions = tools();
        let send = definitions
            .as_array()
            .and_then(|items| items.iter().find(|item| item["name"] == "send_message"))
            .ok_or_else(Failure::storage)?;
        assert_eq!(
            send["inputSchema"]["properties"]["recipients"]["maxItems"],
            8
        );
        assert_eq!(
            send["inputSchema"]["properties"]["expects_reply"]["type"],
            "boolean"
        );
        assert!(send["inputSchema"]["required"]
            .as_array()
            .is_some_and(|required| required.iter().any(|value| value == "requestId")));
        Ok(())
    }

    #[tokio::test]
    async fn formatted_results_keep_their_original_envelope(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        for value in [
            json!({"content":[{"type":"image","data":"aW1hZ2U=","mimeType":"image/png"}]}),
            json!({"isError":true,"content":[{"type":"text","text":"Denied"}]}),
        ] {
            let response = reply(&json!(1), Ok(value.clone()));
            let body = axum::body::to_bytes(response.into_body(), 1024).await?;
            let actual: Value = serde_json::from_slice(&body)?;
            assert_eq!(actual["result"], value);
        }
        Ok(())
    }
}
