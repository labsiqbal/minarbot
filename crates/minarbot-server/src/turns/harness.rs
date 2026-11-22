pub use super::verification::command;
use super::*;
use rusqlite::OptionalExtension;
pub async fn drain(state: &AppState) -> Result<()> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if state
            .turns
            .active
            .lock()
            .map_err(|_| Failure::storage())?
            .is_empty()
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(error(503, "Turn shutdown did not settle."));
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}
pub fn workspace(
    state: &AppState,
    db: &Connection,
    turn: &Value,
) -> Result<Option<crate::config::WorkspaceConfig>> {
    tools::workspace(state, db, turn)
}
pub fn restricted(db: &Connection, turn: &Value) -> Result<bool> {
    Ok(tools::project_context(db, turn)?.is_some())
}
pub fn reminder(db: &Connection, turn: &Value, profile: &Value) -> Result<Option<String>> {
    if turn["agent"] == crate::mcp::guide::ID || super::profile::setup(db, turn)? {
        return Ok(None);
    }
    Ok(verification::nudge(db,string(turn,"id"),turn,profile)?.then(||"One verification reminder: no current check is recorded. Run one relevant check, compare it with the request, then finish. Reuse existing output; verify_result is optional. Keep tool mechanics out of the reply.".into()))
}
pub fn peer(db: &Connection, turn: &str) -> Result<bool> {
    exists(db,"WITH RECURSIVE a(id,parent,origin) AS(SELECT id,parent,origin FROM turns WHERE id=? UNION SELECT t.id,t.parent,t.origin FROM turns t JOIN a ON a.parent=t.id) SELECT 1 FROM a WHERE origin LIKE 'peer:%'",[turn])
}
pub fn instructions(
    state: &AppState,
    db: &Connection,
    turn: &Value,
    runtime: &str,
    model: &str,
) -> Result<String> {
    if turn["agent"] == crate::mcp::guide::ID {
        return Ok(crate::mcp::guide::instructions());
    }
    let mut specs = specs(state, db, turn)?;
    if !state.computer.enabled()
        || !exists(
            db,
            "SELECT 1 FROM agent_computers WHERE agent=? AND enabled=1",
            [string(turn, "agent")],
        )?
    {
        specs.retain(|t| {
            !crate::computer::actions::is_computer_tool(string(&t["function"], "name"))
        });
    }
    if !super::profile::setup(db, turn)? {
        let features: Vec<Value> = serde_json::from_str(include_str!("../mcp/feature-tools.json"))
            .map_err(|_| Failure::storage())?;
        let web_access = super::web::access(state, db, string(turn, "agent"))?;
        specs.extend(
            features
                .iter()
                .filter(|f| web_access.offered(string(f, "name")))
                .map(|f| json!({"function":{"name":f["name"]}})),
        );
    }
    let shell = match runtime {
        "codex" => "exec_command",
        "claude" if !crate::platform::LINUX_SANDBOX => "Bash",
        "pi" => "bash",
        _ => "",
    };
    specs.push(json!({"function":{"name":shell}}));
    execution::base_instructions(runtime, model, string(turn, "agent"), &specs)
}
pub fn specs(state: &AppState, db: &Connection, turn: &Value) -> Result<Vec<Value>> {
    let mut specs: Vec<Value> =
        serde_json::from_str(include_str!("tool-specs.json")).map_err(|_| Failure::storage())?;
    specs.retain(|t| {
        [
            "verify_result",
            "create_text_file",
            "read_attachment",
            "read_workspace_file",
            "open_file_in_panel",
            "ask_user",
            "edit_profile",
            "run_shell",
            "project_stack",
            "library_docs",
        ]
        .contains(&string(&t["function"], "name"))
            || (crate::files::rich_html::available(state, turn)
                && matches!(
                    string(&t["function"], "name"),
                    "html_render" | "html_preview"
                ))
    });
    for spec in &mut specs {
        if spec["function"]["name"] == "run_shell" {
            spec["function"]["description"] = json!(string(&spec["function"], "description")
                .replace(
                    "after command approval",
                    "with approval for sensitive commands"
                ));
        }
    }
    if !super::library_docs::enabled(db, string(turn, "agent"))? {
        specs.retain(|t| {
            !matches!(
                string(&t["function"], "name"),
                "project_stack" | "library_docs"
            )
        });
    }
    specs.extend(crate::mcp::execution::specs()?);
    if crate::files::rich_html::available(state, turn) {
        specs.push(crate::files::components::spec());
    }
    if super::goals::offer_tools(state, db, turn)? {
        specs.extend(super::goals::specs());
    }
    specs.extend(crate::computer::craft::specs(state, db, turn)?);
    if crate::connectors::mailbox::available(state, db, turn)? {
        specs.extend(crate::connectors::mailbox::specs()?);
    }
    specs.extend(
        crate::computer::actions::tools(state.computer.width, state.computer.height)
            .as_array()
            .ok_or_else(Failure::storage)?
            .iter()
            .cloned(),
    );
    if super::profile::setup(db, turn)? {
        specs.retain(|t| ["edit_profile", "ask_user"].contains(&string(&t["function"], "name")));
        for spec in &mut specs {
            if spec["function"]["name"] == "edit_profile" {
                spec["function"]["parameters"]["required"] =
                    json!(["revision", "name", "instructions"]);
            }
        }
    }
    Ok(specs)
}
pub async fn tool(
    state: &AppState,
    turn: &str,
    name: &str,
    args: &Value,
    call_id: &str,
) -> Result<Value> {
    let row = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        one(
            &guard.connection,
            "SELECT * FROM turns WHERE id=? AND status='running'",
            [turn],
        )?
        .ok_or_else(|| error(403, "MCP authorization failed."))?
    };
    let call = json!({"id":call_id,"type":"function","function":{"name":name,"arguments":args.to_string()}});
    let clock = super::diagnostics::Clock::start(state, turn, call_id);
    let result = tools::run(state, &row, &call).await;
    drop(clock);
    let mut result = result?;
    if name == "html_preview" && result["screenshot"].is_object() {
        let image = result["screenshot"]
            .as_object_mut()
            .and_then(|v| v.remove("data"))
            .ok_or_else(Failure::storage)?;
        return Ok(json!({"content":[
            {"type":"text","text":result.to_string()},
            {"type":"image","mimeType":"image/png","data":image}
        ]}));
    }
    let vault_secrets = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        crate::computer::vault::redactions(state, &guard.connection)?
    };
    crate::mcp::secrets::redact_content(&mut result, &vault_secrets);
    if crate::computer::actions::is_computer_tool(name) {
        use base64::Engine;
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        let image = db
            .query_row(
                "SELECT image FROM computer_actions WHERE turn=? AND call_id=?",
                [turn, call_id],
                |r| r.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()?
            .flatten();
        if result["status"] == "waiting_for_user" {
            db.execute(
                "DELETE FROM harness_sessions WHERE agent=?",
                [string(&row, "agent")],
            )?;
            state.turns.cancel(turn);
        }
        let mut content = vec![json!({"type":"text","text":result.to_string()})];
        if let Some(image) = image {
            content.push(json!({"type":"image","mimeType":"image/png","data":base64::engine::general_purpose::STANDARD.encode(image)}));
        }
        return Ok(json!({"content":content}));
    }
    Ok(result)
}
pub fn history(db: &Connection, turn: &Value, args: &Value) -> Result<Value> {
    super::history::read(db, turn, "read_history", args)
}
pub fn message(state: &AppState, db: &Connection, caller: &Value, args: &Value) -> Result<Value> {
    let turn = one(
        db,
        "SELECT * FROM turns WHERE id=?",
        [string(caller, "turn")],
    )?
    .ok_or_else(|| error(403, "MCP authorization failed."))?;
    let call = json!({"id":format!("mcp:{}:{}",string(caller,"id"),string(args,"requestId")),"function":{"name":"send_message"}});
    let result = messaging::run(
        state,
        db,
        &turn,
        &call,
        &json!({"recipients":[args["recipient"]],"text":args["text"],"expects_reply":false}),
    )?;
    one(db,"SELECT t.id,t.thread FROM agent_deliveries d JOIN turns t ON t.id=d.turn WHERE d.request=?",[string(&result,"request")])?.ok_or_else(||error(409,"Recipient is unavailable or message is invalid."))
}

pub fn journal(db: &Connection, turn: &Value, call: &Value, result: &Value) -> Result<()> {
    tools::journal(db, turn, call, result)
}

pub fn setup(db: &Connection, turn: &Value) -> Result<bool> {
    super::profile::setup(db, turn)
}
