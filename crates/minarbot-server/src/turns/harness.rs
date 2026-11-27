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
        let web_access = super::web::access(db, string(turn, "agent"))?;
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
    if crate::automation::model(db, string(turn, "id"))?.is_some() {
        return Ok(Vec::new());
    }
    let mut specs: Vec<Value> =
        serde_json::from_str(include_str!("tool-specs.json")).map_err(|_| Failure::storage())?;
    specs.retain(|t| {
        [
            "verify_result",
            "recall",
            "reflect",
            "remember",
            "search_history",
            "search_memory",
            "remember_memory",
            "select_memory",
            "create_text_file",
            "read_attachment",
            "read_workspace_file",
            "inspect_workspace_file",
            "open_file_in_panel",
            "ask_user",
            "run_shell",
            "project_stack",
            "library_docs",
        ]
        .contains(&string(&t["function"], "name"))
            || (turn["channel"].is_null()
                && [
                    "edit_profile",
                    "list_skills",
                    "read_skill",
                    "read_skill_file",
                ]
                .contains(&string(&t["function"], "name")))
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
    for offered in crate::turns::tools::offered(state, db, turn)? {
        let name = string(&offered["function"], "name");
        if !specs
            .iter()
            .any(|spec| string(&spec["function"], "name") == name)
        {
            specs.push(offered);
        }
    }
    specs.extend(crate::mcp::execution::specs()?);
    if crate::files::rich_html::available(state, turn) {
        specs.push(crate::files::components::spec());
    }
    if super::goals::offer_tools(state, db, turn)? {
        specs.extend(super::goals::specs());
    }
    specs.extend(crate::computer::craft::specs(state));
    if crate::connectors::mailbox::available(state, db, turn)? {
        specs.extend(crate::connectors::mailbox::specs()?);
    }
    specs.extend(crate::automation::tools(db, turn)?);
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
    if name == "list_skills" && !data::closed(args, &["after"]) {
        return Err(error(400, "Invalid skill request."));
    }
    if name == "read_skill" && !data::closed(args, &["id"]) {
        return Err(error(400, "Invalid skill request."));
    }
    let row = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        one(
            &guard.connection,
            // A blocking native tool pauses its turn while the MCP request is
            // outstanding. Keep that same session authorized until the tool
            // returns, while cancelled or ordinary queued turns stay fenced.
            "SELECT * FROM turns WHERE id=? AND (status='running' OR (status='queued' AND (paused=1 OR waiting_for IS NOT NULL)))",
            [turn],
        )?
        .ok_or_else(|| error(403, "MCP authorization failed."))?
    };
    if name == "wait_task" {
        let running = state.turns.running(turn)?;
        let (result, parked) = {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            let db = &guard.connection;
            let delivery = db
                .query_row(
                    "SELECT delivery FROM delegation_tasks WHERE id=?",
                    [string(args, "taskId")],
                    |r| r.get::<_, String>(0),
                )
                .map_err(|_| error(404, "Task was not found."))?;
            let status = super::messaging::status(db, &row, string(args, "taskId"))?;
            if !["queued", "running", "waiting"].contains(&string(&status, "status")) {
                db.execute(
                    "UPDATE delegation_tasks SET wake_delivered=1 WHERE id=?",
                    [string(args, "taskId")],
                )?;
                (status, false)
            } else {
                // Register the wait so `messaging::settle` stores its result and
                // resumes this turn; a bare pause is never woken.
                let timeout = args["timeoutSeconds"]
                    .as_i64()
                    .filter(|seconds| (1..=600).contains(seconds))
                    .unwrap_or(30);
                let call = json!({"id":call_id,"type":"function","function":{"name":name,"arguments":args.to_string()}});
                db.execute(
            "INSERT INTO delegation_waits(id,turn,call_id,call,task,deadline,status,after_output,after_action) VALUES(?,?,?,?,?,?,'waiting',(SELECT count(*) FROM generated_files WHERE turn=?),(SELECT count(*) FROM coordination WHERE turn=?))",
            rusqlite::params![
                uuid::Uuid::new_v4().to_string(),
                turn,
                call_id,
                call.to_string(),
                string(args, "taskId"),
                state.config.now() + timeout * 1000,
                turn,
                turn
            ],
        )?;
                let parked = db.execute(
            "UPDATE turns SET status='queued',paused=1,waiting_for=?,revision=revision+1 WHERE id=? AND status='running'",
            rusqlite::params![delivery, turn],
        )? > 0;
                (json!({"taskId":args["taskId"],"status":"waiting"}), parked)
            }
        };
        if parked {
            // Like the parking tools below: stop the run before replying.
            state.turns.cancel(turn);
            if let Some(mut running) = running {
                while running.changed().await.is_ok() {}
            }
        }
        return Ok(result);
    }
    let call = json!({"id":call_id,"type":"function","function":{"name":name,"arguments":args.to_string()}});
    // A tool that parks its turn (question, event wait, coordination,
    // reply-expecting message, Computer hand-off) stops the harness run. Its
    // reply is held until the worker has stopped, so the harness never takes
    // another model step; the scheduler resumes the turn.
    let parking = matches!(
        name,
        "ask_user" | "wait_for_event" | "transfer_task" | "delegate_task" | "send_message"
    ) || crate::computer::actions::is_computer_tool(name);
    let running = if parking {
        state.turns.running(turn)?
    } else {
        None
    };
    let clock = super::diagnostics::Clock::start(state, turn, call_id);
    let result = tools::run(state, &row, &call).await;
    drop(clock);
    if let Err(refusal) = &result {
        journal_refusal(state, &row, &call, refusal)?;
    }
    let mut result = result?;
    let parked = match name {
        "ask_user" => true,
        "wait_for_event" | "transfer_task" => result.is_null(),
        // A channel delegation returns null; `wait: true` returns a waiting
        // receipt. Both queue the turn until the child finishes.
        "delegate_task" => result.is_null() || result["status"] == "waiting",
        "send_message" => result["status"] == "waiting",
        _ => parking && result["status"] == "waiting_for_user",
    } && {
        // Only a turn the tool actually queued is parked; a running turn
        // keeps its run.
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        exists(
            &guard.connection,
            "SELECT 1 FROM turns WHERE id=? AND status='queued'",
            [turn],
        )?
    };
    if parked && crate::computer::actions::is_computer_tool(name) {
        // The user takes over; the run after hand-back starts a new session.
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        guard.connection.execute(
            "DELETE FROM harness_sessions WHERE agent=?",
            [string(&row, "agent")],
        )?;
    }
    if parked && name != "transfer_task" {
        state.turns.cancel(turn);
    }
    if name == "transfer_task" && result.is_null() {
        // The task now belongs to the recipient; count the call against the
        // shared attempt budget (the run stops before reporting it) and stop
        // this agent's harness run so it cannot spend more on failing calls.
        {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            guard.connection.execute(
                "UPDATE turns SET tool_budget_used=tool_budget_used+1,tool_budget_stopped=CASE WHEN tool_budget_used+1 >= (SELECT tool_budget FROM agents WHERE agents.id=turns.agent) THEN tool_budget_used+1 ELSE tool_budget_stopped END WHERE id=?",
                [turn],
            )?;
        }
        state.turns.cancel(turn);
    }
    if let Some(mut running) = running.filter(|_| parked) {
        // The pause is stored. Hold the reply until the worker has stopped its harness.
        while running.changed().await.is_ok() {}
    }
    if name == "read_attachment" {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        tools::journal(&guard.connection, &row, &call, &result)?;
    }
    if name == "read_attachment" && result["media_type"].is_string() {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        // The turn's own attachments already reach the harness as images.
        let attached: Vec<String> =
            serde_json::from_str(row["attachments"].as_str().unwrap_or("[]")).unwrap_or_default();
        if attached.iter().any(|id| id == string(&result, "id")) {
            return Ok(
                json!({"id":result["id"],"name":result["name"],"note":"Image already included in this execution."}),
            );
        }
        if let Some(file) =
            super::history::readable_file(&guard.connection, &row, string(&result, "id"))?
        {
            if file.bytes.len() * 4 / 3 > 2 * 1024 * 1024 {
                return Ok(
                    json!({"error":"Image context exceeds 4 MiB. Continue without reading another image."}),
                );
            }
            return Ok(json!({"content":[
                {"type":"text","text":result.to_string()},
                {"type":"image","mimeType":result["media_type"],"data":STANDARD.encode(file.bytes)}
            ]}));
        }
    }
    if name == "list_skills" {
        if let Some(items) = result["items"].as_array_mut() {
            if items.len() > 10 {
                let next = items
                    .get(9)
                    .map(|item| item["id"].clone())
                    .unwrap_or(Value::Null);
                items.truncate(10);
                result["next_after"] = next;
            }
        }
    }
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
    let recipients = args["recipients"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| vec![args["recipient"].clone()]);
    let result = messaging::run(
        state,
        db,
        &turn,
        &call,
        &json!({"recipients":recipients,"text":args["text"],"expects_reply":false}),
    )?;
    if args["expects_reply"] == true {
        db.execute(
            "UPDATE agent_requests SET expects_reply=1 WHERE id=?",
            [string(&result, "request")],
        )?;
    }
    one(db,"SELECT t.id,t.thread FROM agent_deliveries d JOIN turns t ON t.id=d.turn WHERE d.request=?",[string(&result,"request")])?.ok_or_else(||error(409,"Recipient is unavailable or message is invalid."))
}

/// Journal a refused native call (4xx) the way the agent loop does in
/// `execution.rs`, so a resumed transcript shows the refusal instead of a
/// gap the model would fill by retrying the mutation.
fn journal_refusal(state: &AppState, turn: &Value, call: &Value, refusal: &Failure) -> Result<()> {
    let name = string(&call["function"], "name");
    if refusal.status >= 500
        || [
            "interrupt_agent",
            "ask_user",
            "wait_for_event",
            "call_mcp_tool",
            "list_mcp_tools",
        ]
        .contains(&name)
        || (name == "send_message" && refusal.status != 429)
        || turn["agent"] == crate::mcp::guide::ID
    {
        return Ok(());
    }
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    if !exists(
        &guard.connection,
        "SELECT 1 FROM turns WHERE id=? AND status='running'",
        [string(turn, "id")],
    )? {
        return Ok(());
    }
    let value = json!({"error":refusal.message,"status":refusal.status});
    if crate::connectors::mailbox::is_tool(name) {
        crate::connectors::mailbox::journal(state, &guard.connection, turn, call, &value)
    } else {
        tools::journal(&guard.connection, turn, call, &value)
    }
}
pub fn journal(db: &Connection, turn: &Value, call: &Value, result: &Value) -> Result<()> {
    tools::journal(db, turn, call, result)
}

pub fn setup(db: &Connection, turn: &Value) -> Result<bool> {
    super::profile::setup(db, turn)
}
