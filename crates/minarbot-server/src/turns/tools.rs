use super::*;
use sha2::{Digest, Sha256};

pub(crate) fn project_context(db: &Connection, turn: &Value) -> Result<Option<Value>> {
    if turn["thread"].is_string() {
        one(
            db,
            "SELECT project,folder FROM local_project_threads WHERE thread=?",
            [string(turn, "thread")],
        )
    } else if turn["channel"].is_string() {
        one(
            db,
            "SELECT project FROM channels WHERE id=? AND project IS NOT NULL",
            [string(turn, "channel")],
        )
    } else {
        Ok(None)
    }
}
pub fn workspace(
    state: &AppState,
    db: &Connection,
    turn: &Value,
) -> Result<Option<crate::config::WorkspaceConfig>> {
    if turn["agent"] == crate::mcp::guide::ID {
        return Ok(Some(crate::config::WorkspaceConfig {
            agent: crate::mcp::guide::ID.into(),
            directory: data::agent_folders::ensure_managed(state, crate::mcp::guide::ID)?,
            write: false,
            managed: true,
        }));
    }
    if let Some(context) = project_context(db, turn)? {
        let project = string(&context, "project");
        let current = one(
            db,
            "SELECT archived FROM local_projects WHERE id=? AND deleted=0",
            [project],
        )?
        .ok_or_else(|| error(404, "Project not found."))?;
        if current["archived"] == 1 {
            return Err(error(409, "Project is archived."));
        }
        let member = one(
            db,
            "SELECT write FROM local_project_members WHERE project=? AND agent=?",
            [project, string(turn, "agent")],
        )?
        .ok_or_else(|| error(403, "Agent is not a project member."))?;
        let folders = all(
            db,
            "SELECT directory FROM local_project_folders WHERE project=? ORDER BY rowid",
            [project],
        )?;
        let Some(first) = folders.first() else {
            return Ok(None);
        };
        let directory = context["folder"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| string(first, "directory"));
        if !folders.iter().any(|r| r["directory"] == directory) {
            return Err(error(403, "Folder is not in this project."));
        }
        let binding = crate::config::WorkspaceConfig {
            agent: string(turn, "agent").to_owned(),
            directory: std::path::PathBuf::from(directory),
            write: member["write"] == 1,
            managed: false,
        };
        crate::files::workspace::validate_bindings(std::slice::from_ref(&binding)).map_err(
            |_| {
                error(
                    409,
                    "Project folder is unavailable or contains a symbolic link.",
                )
            },
        )?;
        return Ok(Some(binding));
    }
    if super::messaging::channel(db, string(turn, "id"))?.is_some() {
        return Ok(None);
    }
    data::agent_folders::effective(state, db, string(turn, "agent")).map(Some)
}
pub fn offered(state: &AppState, db: &Connection, turn: &Value) -> Result<Vec<Value>> {
    if crate::automation::model(db, string(turn, "id"))?.is_some() {
        return Ok(Vec::new());
    }
    if turn["agent"] == crate::mcp::guide::ID {
        return if crate::mcp::guide::eligible(turn) {
            Ok(crate::mcp::guide::declarations()?.into_iter().map(|spec| {
                json!({"type":"function","function":{"name":spec["name"],"description":spec["description"],"parameters":spec["inputSchema"]}})
            }).collect())
        } else {
            Ok(Vec::new())
        };
    }
    let mut specs: Vec<Value> =
        serde_json::from_str(include_str!("tool-specs.json")).map_err(|_| Failure::storage())?;
    let private = super::messaging::channel(db, string(turn, "id"))?.is_none();
    let binding = workspace(state, db, turn)?;
    let bound = binding.is_some();
    let writable = bound && binding.as_ref().is_some_and(|b| b.write);
    let web_access = super::web::access(state, db, string(turn, "agent"))?;
    let docs_enabled = super::library_docs::enabled(db, string(turn, "agent"))?;
    specs.retain(|t| match string(&t["function"], "name") {
        "read_history" | "read_message" | "read_attachment" | "create_text_file"
        | "open_file_in_panel" | "ask_user" | "select_memory" | "remember_memory"
        | "search_memory" | "verify_result" | "wait_for_event" | "list_data_tables"
        | "query_data" => true,
        "project_stack" | "library_docs" => docs_enabled,
        "web_fetch" | "web_search" => web_access.offered(string(&t["function"], "name")),
        "html_render" | "html_preview" => crate::files::rich_html::available(state, turn),
        "read_provider_settings" => private,
        "generate_image" => state.config.media.image_provider.is_some(),
        "list_agents" | "send_message" | "delegate_task" | "task_status" | "wait_task"
        | "cancel_task" => super::messaging::allowed(db, string(turn, "id")).unwrap_or(false),
        "interrupt_agent" => {
            private && super::messaging::allowed(db, string(turn, "id")).unwrap_or(false)
        }
        "edit_profile" | "list_skills" | "read_skill" | "read_skill_file" | "list_sites"
        | "publish_site" | "delete_site" | "set_avatar" | "list_routines" | "read_routine"
        | "set_routine_flow" => private,
        "run_shell" => bound && crate::platform::LINUX_SANDBOX,
        "git_status"
        | "git_diff"
        | "git_log"
        | "list_workspace"
        | "inspect_workspace_file"
        | "read_workspace_file" => bound,
        "create_workspace_file"
        | "create_workspace_directory"
        | "copy_skill_file"
        | "delete_workspace_file"
        | "replace_workspace_file"
        | "restore_workspace_file"
        | "git_commit"
        | "git_create_branch"
        | "git_create_worktree" => writable,
        _ => false,
    });
    specs.extend(crate::automation::tools(db, turn)?);
    if super::goals::offer_tools(state, db, turn)? {
        specs.extend(super::goals::specs());
    }
    if crate::connectors::mailbox::available(state, db, turn)? {
        specs.extend(crate::connectors::mailbox::specs()?);
    }
    if !turn["channel"].is_null() {
        specs.retain(|t| t["function"]["name"] != "delegate_task");
        specs.splice(0..0, super::coordination::tools(db, turn)?);
    }
    if private {
        specs.push(json!({"type":"function","function":{"name":"set_provider_account","description":"Request a user-confirmed switch to an already configured provider account and explicit model. Pauses the task; no binding changes until the user approves through Minarbot. This may disclose conversation history to the selected provider. Backup model is cleared. Cannot create accounts or change credentials.","parameters":{"type":"object","properties":{"revision":{"type":"integer","minimum":1},"providerAccount":{"type":"string"},"model":{"type":"string","maxLength":160}},"required":["revision","providerAccount","model"],"additionalProperties":false}}}));
        specs.push(json!({"type":"function","function":{"name":"list_provider_models","description":"Read model suggestions for a configured account, defaulting to your current account. Returns up to 100 sorted IDs and next_after; supply it as after for another page. Catalog may change between pages and does not guarantee model access. Does not change settings.","parameters":{"type":"object","properties":{"account":{"type":"string","maxLength":64},"after":{"type":"string","maxLength":160}},"additionalProperties":false}}}));
        specs.push(json!({"type":"function","function":{"name":"list_provider_accounts","description":"List your configured provider account choices without credentials or endpoint paths. Returns account ID, display name, provider family, origin and default model. Use set_provider_account only after the user requests a switch.","parameters":{"type":"object","properties":{},"additionalProperties":false}}}));
        specs.push(json!({"type":"function","function":{"name":"set_provider_model","description":"Change your own primary or backup model only when the user requests it. Supply current revision from read_provider_settings. Current execution keeps its model.","parameters":{"type":"object","properties":{"revision":{"type":"integer","minimum":1},"model":{"type":"string","maxLength":160},"backupModel":{"type":"string","maxLength":160}},"required":["revision"],"additionalProperties":false}}}));
    }
    if state.computer.enabled()
        && exists(
            db,
            "SELECT 1 FROM agent_computers WHERE agent=? AND enabled=1",
            [string(turn, "agent")],
        )?
    {
        specs.extend(
            crate::computer::actions::tools(state.computer.width, state.computer.height)
                .as_array()
                .ok_or_else(Failure::storage)?
                .iter()
                .cloned(),
        );
    }
    specs.extend(crate::mcp::execution::specs()?);
    if crate::files::rich_html::available(state, turn) {
        specs.push(crate::files::components::spec());
    }
    specs.extend(crate::computer::craft::specs(state, db, turn)?);
    if turn["profile_setup"] == 1 {
        specs.retain(|t| ["edit_profile", "ask_user"].contains(&string(&t["function"], "name")));
        for tool in &mut specs {
            if tool["function"]["name"] == "edit_profile" {
                tool["function"]["parameters"]["required"] =
                    json!(["revision", "name", "instructions"]);
            }
        }
    }
    Ok(specs)
}
pub async fn run(state: &AppState, turn: &Value, call: &Value) -> Result<Value> {
    if pending(state) {
        return Err(error(503, "Update trial is pending."));
    }
    let name = string(&call["function"], "name");
    if crate::connectors::mailbox::is_tool(name) {
        return crate::connectors::mailbox::run(state, turn, call).await;
    }
    let args: Value =
        serde_json::from_str(string(&call["function"], "arguments")).map_err(|_| {
            error(
                if name.ends_with("workspace_file") {
                    502
                } else {
                    400
                },
                "Tool arguments must be valid JSON.",
            )
        })?;
    if !args.is_object() {
        return Err(error(400, "Tool arguments must be a JSON object."));
    }
    if turn["agent"] == crate::mcp::guide::ID {
        return crate::mcp::guide::call(state, turn, name, &args).await;
    }
    if name == "render_component" {
        return crate::files::components::run(state, turn, call, &args).await;
    }
    if ["project_stack", "library_docs"].contains(&name) {
        let result = match super::library_docs::tool(state, turn, name, &args).await {
            Err(e) if e.status != 403 => json!({"error":e.message,"status":e.status}),
            other => other?,
        };
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        journal(&guard.connection, turn, call, &result)?;
        return Ok(result);
    }
    if name == "html_preview" {
        let result = crate::files::rich_html::preview(state, turn, &args)
            .await
            .unwrap_or_else(|failure| json!({"error":failure.message,"status":failure.status}));
        let mut summary = result.clone();
        if let Some(screenshot) = summary["screenshot"].as_object_mut() {
            screenshot.remove("data");
        }
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        journal(&guard.connection, turn, call, &summary)?;
        return Ok(result);
    }
    if name == "read_attachment"
        && closed(&args, &["id", "offset"])
        && uuid(string(&args, "id"))
        && args
            .get("offset")
            .is_none_or(|v| integer(v, 0, 9_007_199_254_740_991))
    {
        let file = {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            super::history::readable_file(&guard.connection, turn, string(&args, "id"))?
                .filter(|file| file.bytes.starts_with(b"%PDF-"))
        };
        if let Some(file) = file {
            let response = crate::files::preview::pdf(state, &file.meta, &file.bytes).await?;
            let bytes = axum::body::to_bytes(response.into_body(), 512 * 1024)
                .await
                .map_err(|_| Failure::storage())?;
            let content: Value = serde_json::from_slice(&bytes).map_err(|_| Failure::storage())?;
            let text = string(&content, "text");
            let offset = args["offset"].as_u64().unwrap_or(0) as usize;
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            if super::history::readable_file(&guard.connection, turn, string(&args, "id"))?
                .is_none()
            {
                return Ok(
                    json!({"error":"Attachment not found in sent messages in this conversation."}),
                );
            }
            return Ok(
                json!({"id":args["id"],"name":file.meta["name"],"source":"pdf-text","note":"Extracted text only. Images, scanned pages and visual layout are not included.","text":super::history::slice(text,offset,8000),"offset":offset,"next_offset":if text.encode_utf16().count()>offset+8000 {Some(offset+8000)}else{None}}),
            );
        }
    }
    if name == "list_provider_models" {
        if !closed(&args, &["account", "after"])
            || args.get("account").is_some_and(|v| {
                v.as_str().is_none_or(|id| {
                    id.len() > 64
                        || !id.is_empty()
                            && (!id.as_bytes()[0].is_ascii_lowercase()
                                && !id.as_bytes()[0].is_ascii_digit()
                                || !id.bytes().all(|b| {
                                    b.is_ascii_lowercase()
                                        || b.is_ascii_digit()
                                        || b"_-".contains(&b)
                                }))
                })
            })
            || args.get("after").is_some_and(|v| {
                v.as_str().is_none_or(|s| {
                    s.encode_utf16().count() > 160
                        || s.trim() != s
                        || s.chars().any(|c| c < ' ' || c == '\u{7f}')
                })
            })
        {
            return Err(error(400, "Invalid model catalog parameters."));
        }
        let account = match args["account"].as_str() {
            Some(id) => id.to_owned(),
            None => {
                let guard = state.database.lock().map_err(|_| Failure::storage())?;
                string(
                    &data::agents::read(&guard.connection, string(turn, "agent"))?,
                    "provider_account",
                )
                .to_owned()
            }
        };
        let result =
            data::providers::models(state, &[("account".to_owned(), account.clone())]).await;
        return Ok(match result {
            Ok(value) => {
                let after = args["after"].as_str().unwrap_or("");
                let models = value["models"]
                    .as_array()
                    .ok_or_else(Failure::storage)?
                    .iter()
                    .filter(|id| id.as_str().is_some_and(|id| id > after))
                    .collect::<Vec<_>>();
                json!({"account":account,"models":models.iter().take(100).collect::<Vec<_>>(),"next_after":if models.len()>100 {models[99].clone()} else {Value::Null}})
            }
            Err(e) if e.status == 503 => {
                json!({"account":account,"error":"Provider account is unavailable.","status":503})
            }
            Err(_) => {
                json!({"account":account,"error":"Model listing is unavailable.","status":502})
            }
        });
    }
    if name == "generate_image" {
        return super::media::generate(state, turn, call, &args).await;
    }
    if ["web_fetch", "web_search"].contains(&name) {
        let result = super::web::run(state, turn, name, &args).await?;
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        journal(&guard.connection, turn, call, &result)?;
        return Ok(result);
    }
    if ["list_data_tables", "query_data"].contains(&name) {
        return crate::files::shared::tool(state, turn, call, &args).await;
    }
    if name == "copy_skill_file" {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        if !exists(
            &guard.connection,
            "SELECT 1 FROM agent_skills WHERE agent=? AND skill=? AND enabled=1 AND removed=0",
            [string(turn, "agent"), string(&args, "id")],
        )? {
            return Err(error(502, "Enabled installed skill not found."));
        }
    }
    if ["list_mcp_tools", "call_mcp_tool"].contains(&name) {
        return crate::mcp::execution::run(state, turn, call, &args).await;
    }
    if crate::computer::craft::is_tool(name) {
        return crate::computer::craft::run(state, turn, call, &args).await;
    }
    if name == "run_shell" {
        return super::shell::run(state, turn, call, args).await;
    }
    if name.starts_with("git_") {
        let binding = {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            workspace(state, &guard.connection, turn)?.ok_or_else(|| {
                error(
                    403,
                    "Workspace access is not granted for this conversation.",
                )
            })?
        };
        let result = match name {
            "git_status" if closed(&args, &[]) => crate::files::git::status(Some(&binding)).await?,
            "git_diff"
                if closed(&args, &["path"]) && args.get("path").is_none_or(Value::is_string) =>
            {
                crate::files::git::diff(&binding, args["path"].as_str()).await?
            }
            "git_log"
                if closed(&args, &["limit"])
                    && args.get("limit").is_none_or(|v| integer(v, 1, 50)) =>
            {
                crate::files::git::log(&binding, args["limit"].as_i64().unwrap_or(10)).await?
            }
            "git_commit" | "git_create_branch" | "git_create_worktree" => {
                let actions = {
                    let guard = state.database.lock().map_err(|_| Failure::storage())?;
                    guard.connection.query_row(
                        "SELECT count(*) FROM coordination WHERE turn=?",
                        [string(turn, "id")],
                        |r| r.get(0),
                    )?
                };
                return crate::files::git::writes::execute(
                    state,
                    &binding,
                    string(turn, "id"),
                    call,
                    &args,
                    actions,
                )
                .await;
            }
            _ => return Err(error(400, "Invalid Git arguments.")),
        };
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        journal(&guard.connection, turn, call, &result)?;
        return Ok(result);
    }
    if [
        "create_workspace_file",
        "create_workspace_directory",
        "copy_skill_file",
        "delete_workspace_file",
        "replace_workspace_file",
        "restore_workspace_file",
    ]
    .contains(&name)
    {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        let binding = workspace(state, db, turn)?
            .ok_or_else(|| error(403, "Workspace does not have write permission."))?;
        let actions = db.query_row(
            "SELECT count(*) FROM coordination WHERE turn=?",
            [string(turn, "id")],
            |r| r.get(0),
        )?;
        return crate::files::writes::execute(db, &binding, string(turn, "id"), call, actions);
    }
    if crate::computer::actions::is_computer_tool(name) {
        let (outputs, actions) = {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            let db = &guard.connection;
            (
                db.query_row(
                    "SELECT count(*) FROM generated_files WHERE turn=?",
                    [string(turn, "id")],
                    |r| r.get::<_, i64>(0),
                )?,
                db.query_row(
                    "SELECT count(*) FROM coordination WHERE turn=?",
                    [string(turn, "id")],
                    |r| r.get::<_, i64>(0),
                )?,
            )
        };
        let outcome = crate::computer::control::action(
            state,
            string(turn, "agent"),
            string(turn, "id"),
            call,
            &args,
            outputs,
            actions,
        )
        .await?;
        return Ok(outcome.result);
    }
    data::avatar::run(state, |db, avatar| {
    let result = transaction(db, || {
        if !exists(
            db,
            "SELECT 1 FROM turns WHERE id=? AND status='running'",
            [string(turn, "id")],
        )? {
            return Err(error(409, "Task has stopped."));
        }
        let result = dispatch(state, db, turn, call, &args, avatar)?;
        if ![
            "delegate_task",
            "transfer_task",
            "send_message",
            "wait_task",
            "read_history",
            "read_message",
            "search_memory",
            "list_provider_accounts",
            "read_provider_settings",
            "list_skills",
            "read_skill",
            "read_skill_file",
            "ask_user",
            "wait_for_event",
            "set_provider_account",
        ]
        .contains(&name)
        {
            journal(db, turn, call, &result)?;
        }
        let wait = if name == "ask_user" && result.is_null() {
            crate::harness::config::CardWait::checkpoint(state, db, string(turn, "id"))?
        } else {
            None
        };
        Ok((result, wait))
    }).map(|(result, wait)| {
        if let Some(wait) = wait {
            wait.keep_paused();
            state.turns.cancel(string(turn, "id"));
        }
        result
    });
    if result
        .as_ref()
        .is_err_and(|e| e.status == 429 && e.message == "Agent message rate limit reached.")
    {
        db.execute("INSERT INTO agent_message_cooldowns(agent,until) VALUES(?,?) ON CONFLICT(agent) DO UPDATE SET until=excluded.until",params![string(turn,"agent"),state.config.now()+state.config.turns.limit("cooldownMs",600000)])?;
    }
    result
    }).await
}

pub fn journal(db: &Connection, turn: &Value, call: &Value, result: &Value) -> Result<()> {
    if exists(db,"SELECT 1 FROM flow_tool_calls WHERE turn=? AND call_id=? UNION ALL SELECT 1 FROM generated_files WHERE turn=? AND call_id=?",[string(turn,"id"),string(call,"id"),string(turn,"id"),string(call,"id")])? {return Ok(());}
    let recorded = if matches!(
        string(&call["function"], "name"),
        "html_render" | "html_preview" | "render_component" | "library_docs"
    ) {
        // Reply payloads have their own tables. Keep raw documents out of the tool journal.
        let mut recorded = call.clone();
        recorded["function"]["arguments"] = json!("{}");
        recorded.to_string()
    } else {
        call.to_string()
    };
    db.execute("INSERT INTO flow_tool_calls(turn,call_id,call,result,after_output,after_action) VALUES (?,?,?,?,(SELECT count(*) FROM generated_files WHERE turn=?),(SELECT count(*) FROM coordination WHERE turn=?))",params![turn["id"].as_str(),call["id"].as_str(),recorded,result.to_string(),turn["id"].as_str(),turn["id"].as_str()])?;
    Ok(())
}
pub(super) fn dispatch(
    state: &AppState,
    db: &Connection,
    turn: &Value,
    call: &Value,
    args: &Value,
    avatar: &mut data::avatar::Prepared,
) -> Result<Value> {
    let name = string(&call["function"], "name");
    let agent = string(turn, "agent");
    match name {
        "list_routines" | "read_routine" | "set_routine_flow" | "no_update" | "report_failure" => {
            crate::automation::tool(db, turn, name, args)
        }
        "set_provider_account" => super::accounts::request(state, db, turn, call, args),
        "read_history" | "read_message" | "read_attachment" => {
            super::history::read(db, turn, name, args)
        }
        "search_memory" | "remember_memory" | "select_memory" => {
            super::memory::run(db, turn, name, args)
        }
        "create_text_file" => file_tool(state, db, turn, call, args),
        "html_render" => crate::files::rich_html::publish(state, db, turn, call, args),
        "open_file_in_panel" => {
            if !closed(args, &["id"]) || !uuid(string(args, "id")) {
                return Err(error(400, "Invalid file ID."));
            }
            let Some(file) = super::history::readable_file(db, turn, string(args, "id"))? else {
                return Ok(json!({"error":"File not found in this conversation."}));
            };
            Ok(
                json!({"id":file.meta["id"],"name":file.meta["name"],"size":file.bytes.len(),"url":format!("/api/{}/files/{}",execution::scope(turn).replace(':',"/"),string(args,"id")),"openedAt":iso(db,state.config.now())?}),
            )
        }
        "read_provider_settings" => {
            if !turn["channel"].is_null() {
                return Err(error(
                    403,
                    "Provider settings are only available in the agent's private conversation.",
                ));
            }
            if !closed(args, &[]) {
                return Err(error(
                    400,
                    "Reading provider settings does not accept arguments.",
                ));
            }
            let a = data::agents::read(db, agent)?;
            let configured = data::providers::accounts(state)?
                .iter()
                .any(|x| x.id == a["provider_account"] && !x.base.is_empty());
            Ok(
                json!({"revision":a["revision"],"providerAccount":a["provider_account"],"model":a["model"],"backupModel":a["backup_model"],"configured":configured}),
            )
        }
        "list_provider_accounts" => {
            if !closed(args, &[]) {
                return Err(error(
                    400,
                    "Provider account listing does not accept arguments.",
                ));
            }
            Ok(json!(data::providers::accounts(state)?.into_iter().map(|a|json!({"id":a.id,"name":a.name,"provider":a.family,"origin":url::Url::parse(&a.base).map(|u|u.origin().ascii_serialization()).ok(),"defaultModel":a.model,"configured":!a.base.is_empty()})).collect::<Vec<_>>()))
        }
        "set_provider_model" => {
            if !closed(args, &["revision", "model", "backupModel"])
                || !integer(&args["revision"], 1, 9_007_199_254_740_991)
                || args.get("model").is_none() && args.get("backupModel").is_none()
            {
                return Err(error(400, "Invalid model change."));
            }
            let a = data::agents::read(db, agent)?;
            if a["revision"] != args["revision"] {
                return Err(error(
                    409,
                    "Profile changed. Ask the user for guidance before changing it again.",
                ));
            }
            let model = if args.get("model").is_some() {
                field(args, "model", 160)?
            } else {
                string(&a, "model").into()
            };
            if model.chars().any(char::is_control) {
                return Err(error(400, "Invalid model."));
            }
            let backup = if let Some(b) = args.get("backupModel") {
                b.as_str()
                    .filter(|b| b.encode_utf16().count() <= 160 && !b.chars().any(char::is_control))
                    .ok_or_else(|| {
                        error(
                            400,
                            "Fallback model is invalid or matches the primary model.",
                        )
                    })?
                    .trim()
            } else {
                string(&a, "backup_model")
            };
            if backup == model {
                return Err(error(
                    400,
                    "Fallback model is invalid or matches the primary model.",
                ));
            }
            db.execute(
                "UPDATE agents SET model=?,backup_model=?,revision=revision+1 WHERE id=?",
                params![model, backup, agent],
            )?;
            Ok(
                json!({"revision":a["revision"].as_i64().unwrap_or(0)+1,"model":model,"backupModel":backup,"providerAccount":a["provider_account"]}),
            )
        }
        "wait_for_event" => super::waits::request(state, db, turn, call, args),
        "ask_user" => {
            if !closed(args, &["question", "choices"]) {
                return Err(error(400, "Invalid question or choices."));
            }
            let prompt = field(args, "question", 2000)
                .map_err(|_| error(400, "Invalid question or choices."))?;
            let choices = args["choices"]
                .as_array()
                .filter(|a| a.len() <= 8)
                .ok_or_else(|| error(400, "Invalid question or choices."))?;
            let mut seen = std::collections::HashSet::new();
            let mut clean = Vec::new();
            for c in choices {
                let c = c
                    .as_str()
                    .filter(|s| {
                        !s.trim().is_empty()
                            && s.encode_utf16().count() <= 200
                            && seen.insert(s.trim())
                    })
                    .ok_or_else(|| error(400, "Invalid question or choices."))?;
                clean.push(c.trim());
            }
            if one(
                db,
                "SELECT count(*) AS n FROM questions WHERE turn=?",
                [string(turn, "id")],
            )?
            .is_some_and(|v| v["n"].as_i64().unwrap_or(0) >= 8)
                || exists(
                    db,
                    "SELECT 1 FROM questions WHERE turn=? AND answer IS NULL",
                    [string(turn, "id")],
                )?
            {
                return Err(error(
                    409,
                    "Task question limit reached or an answer is still pending.",
                ));
            }
            db.execute("INSERT INTO questions VALUES (?,?,?,?,?,?,NULL,(SELECT count(*) FROM generated_files WHERE turn=?),(SELECT count(*) FROM coordination WHERE turn=?))",params![uuid::Uuid::new_v4().to_string(),turn["id"].as_str(),call["id"].as_str(),call.to_string(),prompt,json!(clean).to_string(),turn["id"].as_str(),turn["id"].as_str()])?;
            db.execute("UPDATE turns SET status='queued',paused=1,revision=revision+1,error=NULL WHERE id=?",[string(turn,"id")])?;
            Ok(Value::Null)
        }
        "transfer_task" => super::coordination::execute(state, db, turn, call, args),
        "delegate_task" if args.get("agent").is_none() && !turn["channel"].is_null() => {
            super::coordination::execute(state, db, turn, call, args)
        }
        "list_agents" | "send_message" | "delegate_task" | "task_status" | "wait_task"
        | "cancel_task" | "interrupt_agent" => super::messaging::run(state, db, turn, call, args),
        "verify_result" => super::verification::review(db, turn, args),
        "set_goal" | "set_loop" => super::goals::tool(state, db, turn, name, args),
        "edit_profile" | "set_avatar" | "list_sites" | "publish_site" | "delete_site" => {
            super::profile::execute(state, db, turn, name, args, avatar)
        }
        "list_workspace" | "read_workspace_file" | "inspect_workspace_file" => {
            let binding = workspace(state, db, turn)?.ok_or_else(|| {
                error(
                    403,
                    "Workspace access is not granted for this conversation.",
                )
            })?;
            let params = args
                .as_object()
                .ok_or_else(Failure::storage)?
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| v.to_string()),
                    )
                })
                .collect::<Vec<_>>();
            if name == "inspect_workspace_file" {
                if !closed(args, &["path"]) {
                    return Err(error(400, "Invalid workspace arguments."));
                }
                let path = field(args, "path", 2048)?;
                let (_, bytes) = crate::files::workspace::download(&binding.directory, &path)?;
                Ok(
                    json!({"path":path,"size":bytes.len(),"digest":format!("{:x}",Sha256::digest(bytes))}),
                )
            } else {
                crate::files::workspace::read(&binding.directory, name == "list_workspace", &params)
            }
        }
        "list_skills" | "read_skill" | "read_skill_file" => skill(db, agent, name, args),
        _ => Err(error(502, "Invalid coordination request.")),
    }
}
fn skill(db: &Connection, agent: &str, name: &str, args: &Value) -> Result<Value> {
    let listing = name == "list_skills";
    let key = if listing { "after" } else { "id" };
    let file = name == "read_skill_file";
    if !closed(
        args,
        if file {
            &["id", "path", "offset"]
        } else {
            std::slice::from_ref(&key)
        },
    ) || (!listing || args.get(key).is_some()) && !uuid(string(args, key))
        || file
            && (args["path"].as_str().is_none_or(|s| s.len() > 240)
                || args
                    .get("offset")
                    .is_some_and(|v| !integer(v, 0, 9_007_199_254_740_991)))
    {
        return Err(error(400, "Invalid skill request."));
    }
    if listing {
        let mut rows=all(db,"SELECT a.skill AS id,a.version,v.name,v.description FROM agent_skills a JOIN skill_versions v ON v.skill=a.skill AND v.revision=a.version WHERE a.agent=? AND a.enabled=1 AND a.removed=0 AND a.skill>? ORDER BY a.skill LIMIT 11",[agent,string(args,key)])?;
        let next = if rows.len() > 10 {
            rows.get(9).map(|r| r["id"].clone())
        } else {
            None
        };
        rows.truncate(10);
        return Ok(json!({"items":rows,"next_after":next}));
    }
    let Some(mut row)=one(db,"SELECT a.skill AS id,a.version,v.name,v.description,v.content FROM agent_skills a JOIN skill_versions v ON v.skill=a.skill AND v.revision=a.version WHERE a.agent=? AND a.enabled=1 AND a.removed=0 AND a.skill=?",[agent,string(args,key)])? else{return Ok(json!({"error":"Enabled installed skill not found."}));};
    let files = one(
        db,
        "SELECT files FROM skill_files WHERE skill=? AND revision=?",
        params![string(args, key), row["version"].as_i64()],
    )?
    .map(|r| data::json(&r["files"]))
    .transpose()?
    .unwrap_or(json!([]));
    let files = files.as_array().ok_or_else(Failure::storage)?;
    if !file {
        row["files"] = json!(files
            .iter()
            .map(|f| if f["encoding"].is_null() {
                json!({"path":f["path"],"characters":string(f,"content").encode_utf16().count()})
            } else {
                json!({"path":f["path"],"binary":true})
            })
            .collect::<Vec<_>>());
        return Ok(row);
    }
    let Some(f) = files.iter().find(|f| f["path"] == args["path"]) else {
        return Ok(json!({"error":"File not found in installed skill."}));
    };
    if f.get("encoding").is_some() {
        return Ok(
            json!({"error":"Binary skill asset cannot be read as text. Export the skill package from the library to use its original bytes.","path":f["path"],"binary":true}),
        );
    }
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    Ok(
        json!({"id":args["id"],"version":row["version"],"path":f["path"],"content":super::history::slice(string(f,"content"),offset,8000),"next_offset":if string(f,"content").encode_utf16().count()>offset+8000 {Some(offset+8000)}else{None}}),
    )
}
fn file_tool(
    state: &AppState,
    db: &Connection,
    turn: &Value,
    call: &Value,
    args: &Value,
) -> Result<Value> {
    let name =
        field(args, "name", 200).map_err(|_| error(400, "Invalid text file name or content."))?;
    let text = args["text"]
        .as_str()
        .filter(|s| {
            s.encode_utf16().count() <= 16000
                && !s
                    .chars()
                    .any(|c| matches!(c,'\x00'..='\x08'|'\x0b'..='\x0c'|'\x0e'..='\x1f'))
        })
        .ok_or_else(|| error(400, "Invalid text file name or content."))?;
    if !closed(args, &["name", "text"])
        || name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
        || [".", ".."].contains(&name.as_str())
    {
        return Err(error(400, "Invalid text file name or content."));
    }
    if text.len() > 32768 {
        return Err(error(413, "Text file limit is 32 KiB."));
    }
    let binding = workspace(state, db, turn)?.filter(|w| w.write);
    let scope = execution::scope(turn);
    let existing = one(
        db,
        "SELECT f.id,f.digest,g.workspace FROM files f JOIN generated_files g ON g.file=f.id WHERE f.scope=? AND f.name=? AND f.deleted=0 ORDER BY g.seq DESC LIMIT 1",
        params![scope, name],
    )?;
    let id = existing
        .as_ref()
        .map(|r| string(r, "id").to_owned())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if one(
        db,
        "SELECT count(DISTINCT file) AS n FROM generated_files WHERE turn=?",
        [string(turn, "id")],
    )?
    .is_some_and(|r| r["n"].as_i64().unwrap_or(0) >= 8)
        && !exists(
            db,
            "SELECT 1 FROM generated_files WHERE turn=? AND file=?",
            [string(turn, "id"), &id],
        )?
    {
        return Err(error(429, "Task file limit reached."));
    }
    let digest = format!("{:x}", Sha256::digest(text.as_bytes()));
    if existing.is_some() {
        crate::files::preview::forget(db, &id)?;
        db.execute(
            "UPDATE files SET bytes=?,digest=? WHERE id=?",
            params![text.as_bytes(), digest, id],
        )?;
    } else {
        db.execute(
            "INSERT INTO files(id,scope,name,digest,bytes,created) VALUES(?,?,?,?,?,?)",
            params![
                id,
                scope,
                name,
                digest,
                text.as_bytes(),
                iso(db, state.config.now())?
            ],
        )?;
    }
    let directory = binding.as_ref().and_then(|w| w.directory.to_str());
    db.execute("INSERT INTO generated_files(turn,file,call_id,call,after_action,workspace) VALUES(?,?,?,?,(SELECT count(*) FROM coordination WHERE turn=?),?)",params![turn["id"].as_str(),id,call["id"].as_str(),call.to_string(),turn["id"].as_str(),directory])?;
    let mut result = json!({"id":id,"name":name,"size":text.len(),"url":format!("/api/{}/files/{id}",scope.replace(':',"/"))});
    if let Some(binding) = &binding {
        let previous = existing
            .as_ref()
            .filter(|v| v["workspace"].as_str() == directory)
            .map(|v| string(v, "digest"));
        crate::files::writes::publish_generated(binding, &name, text, previous)?;
        result["path"] = json!(name);
    }
    Ok(result)
}
