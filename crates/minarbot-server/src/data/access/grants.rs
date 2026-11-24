use super::*;

const TOOLS: &[&str] = &[
    "ask_user",
    "call_mcp_tool",
    "cancel_task",
    "copy_skill_file",
    "create_text_file",
    "create_workspace_directory",
    "create_workspace_file",
    "delegate_task",
    "delete_site",
    "edit_profile",
    "generate_image",
    "inspect_workspace_file",
    "interrupt_agent",
    "list_agents",
    "list_data_tables",
    "list_mcp_tools",
    "list_routines",
    "list_sites",
    "list_skills",
    "list_workspace",
    "no_update",
    "open_file_in_panel",
    "publish_site",
    "query_data",
    "read_attachment",
    "read_history",
    "read_message",
    "read_provider_settings",
    "read_routine",
    "read_skill",
    "read_skill_file",
    "read_workspace_file",
    "remember_memory",
    "report_failure",
    "search_memory",
    "select_memory",
    "send_message",
    "set_avatar",
    "set_routine_flow",
    "task_status",
    "transfer_task",
    "wait_for_event",
    "wait_task",
];
const OPERATIONS: &[&str] = &[
    "list_workspace",
    "read_workspace_file",
    "download_workspace_file",
    "create_workspace_file",
    "create_workspace_directory",
    "copy_skill_file",
    "download",
    "upload",
    "text_preview",
    "html_preview",
];

fn capabilities(v: &Value) -> Result<Value> {
    let invalid = || Failure::new(400, "invalid_capabilities", "Invalid agent capabilities.");
    if !closed(v, &["v", "tools", "resources"]) || v["v"] != 1 {
        return Err(invalid());
    }
    let mut tools = Vec::new();
    for tool in v["tools"].as_array().ok_or_else(invalid)? {
        let name = tool
            .as_str()
            .filter(|s| TOOLS.contains(s))
            .ok_or_else(|| Failure::new(422, "capability_missing", "Agent tool is unavailable."))?;
        if tools.contains(&name) {
            return Err(Failure::new(
                400,
                "duplicate_capability",
                "Duplicate agent tool.",
            ));
        }
        tools.push(name);
    }
    tools.sort();
    let mut resources = Vec::new();
    for item in v["resources"].as_array().ok_or_else(invalid)? {
        fields(item, &["resourceId", "operations"])?;
        let resource = id(&item["resourceId"])?;
        if resources
            .iter()
            .any(|v: &Value| v["resourceId"] == resource)
        {
            return Err(Failure::new(
                400,
                "duplicate_resource",
                "Duplicate agent resource.",
            ));
        }
        let operations = item["operations"]
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or_else(|| {
                Failure::new(
                    400,
                    "invalid_resource_operations",
                    "Resource operations are required.",
                )
            })?;
        let mut sorted = Vec::new();
        for operation in operations {
            let name = operation
                .as_str()
                .filter(|s| OPERATIONS.contains(s))
                .ok_or_else(|| {
                    Failure::new(422, "capability_missing", "Unsupported resource operation.")
                })?;
            if sorted.contains(&name) {
                return Err(Failure::new(
                    400,
                    "duplicate_resource_operation",
                    "Duplicate resource operation.",
                ));
            }
            sorted.push(name);
        }
        sorted.sort();
        resources.push(json!({"resourceId":resource,"operations":sorted}));
    }
    resources.sort_by(|a, b| string(a, "resourceId").cmp(string(b, "resourceId")));
    Ok(json!({"v":1,"tools":tools,"resources":resources}))
}

fn resources(db: &Connection, project: &str, caps: &Value) -> Result<()> {
    for resource in caps["resources"].as_array().ok_or_else(Failure::storage)? {
        if !table(db, "resources")? {
            return Err(missing());
        }
        let row = one(db,"SELECT r.*,d.capabilities_json AS host_capabilities FROM resources r JOIN projects p ON p.id=r.project JOIN devices d ON d.id=r.device WHERE r.id=? AND r.project=? AND d.account=p.account AND r.availability<>'deleted'",[string(resource,"resourceId"),project])?.ok_or_else(missing)?;
        let operations = json(&row["operations_json"])?;
        let host = json(&row["host_capabilities"])?;
        let workspace = ["repository", "workspace"].contains(&string(&row, "kind"));
        for op in resource["operations"]
            .as_array()
            .ok_or_else(Failure::storage)?
        {
            let op = op.as_str().ok_or_else(Failure::storage)?;
            let file = op.contains("workspace") || op == "copy_skill_file";
            let write = [
                "create_workspace_file",
                "create_workspace_directory",
                "copy_skill_file",
                "upload",
            ]
            .contains(&op);
            if !operations
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v == op))
                || file != workspace
                || !host.as_array().is_some_and(|a| {
                    a.iter().any(|v| {
                        v == if write {
                            "workspace-write"
                        } else {
                            "workspace-read"
                        }
                    })
                })
            {
                return Err(Failure::new(
                    422,
                    "capability_missing",
                    "Resource operation is not authorized by the host yet.",
                ));
            }
        }
    }
    Ok(())
}

fn rules(v: &Value) -> Result<&str> {
    v.as_str()
        .filter(|s| s.encode_utf16().count() <= 8000 && !s.contains('\0'))
        .ok_or_else(|| Failure::new(400, "invalid_rules", "Invalid project rules."))
}

pub(super) fn edit(
    state: &AppState,
    db: &Connection,
    account: &str,
    project: &str,
    aid: &str,
    kind: &str,
    v: &Value,
) -> Result<Value> {
    let keys: &[&str] = match kind {
        "rules" => &["requestId", "expectedParticipationRevision", "rules"],
        "capabilities" => &["requestId", "expectedRevision", "capabilities"],
        _ => &[
            "requestId",
            "expectedProjectRevision",
            "expectedAgentRevision",
            "expectedAuthorityRevision",
            "expectedParticipationRevision",
            "rules",
            "capabilities",
        ],
    };
    fields(v, keys)?;
    let request = id(&v["requestId"])?;
    let mut command = json!({"requestId":request});
    if kind != "capabilities" {
        command["rules"] = json!(rules(&v["rules"])?);
    }
    if kind != "rules" {
        command["capabilities"] = capabilities(&v["capabilities"])?;
    }
    for key in keys.iter().filter(|k| k.starts_with("expected")) {
        let expected = if kind.is_empty() && *key == "expectedParticipationRevision" {
            if !integer(&v[*key], 0, 9_007_199_254_740_991) {
                return Err(Failure::new(
                    400,
                    "invalid_revision",
                    "Revision must be a nonnegative safe integer.",
                ));
            }
            v[*key].as_i64().ok_or_else(Failure::storage)?
        } else {
            revision(&v[*key])?
        };
        command[*key] = json!(expected);
    }
    let operation = if kind.is_empty() {
        "participation"
    } else {
        kind
    };
    let scope = format!("agents:{operation}:{project}:{aid}");
    let hash = digest(json!([
        format!("minarbot-agent-{operation}-v1"),
        project,
        aid,
        command
    ]));
    transaction(db, || {
        let owner = member(db, account, project, true)?;
        let row = if kind.is_empty() {
            None
        } else {
            Some(agent(db, project, aid)?)
        };
        if let Some(prior) = replay(db, account, &request, project, &scope, &hash)? {
            return Ok(prior);
        }
        let (next, status, data) = if kind.is_empty() {
            let authority = one(
                db,
                "SELECT * FROM agent_authority WHERE agent=? AND owner_account=?",
                [aid, account],
            )?
            .ok_or_else(missing)?;
            let profile =
                one(db, "SELECT revision FROM agents WHERE id=?", [aid])?.ok_or_else(missing)?;
            let current = one(
                db,
                "SELECT * FROM agent_projects WHERE project=? AND agent=?",
                [project, aid],
            )?;
            if owner["revision"] != command["expectedProjectRevision"]
                || profile["revision"] != command["expectedAgentRevision"]
                || authority["revision"] != command["expectedAuthorityRevision"]
            {
                return Err(stale());
            }
            resources(db, project, &command["capabilities"])?;
            if current.as_ref().is_some_and(|r| r["deleted"] == 0) {
                return Err(Failure::new(
                    409,
                    "participation_exists",
                    "Agent is already in the project.",
                ));
            }
            let prior = current
                .as_ref()
                .and_then(|r| r["revision"].as_i64())
                .unwrap_or(0);
            if command["expectedParticipationRevision"] != prior {
                return Err(stale());
            }
            let next = prior + 1;
            if current.is_some() {
                db.execute("UPDATE agent_projects SET rules=?,capabilities_json=?,revision=?,deleted=0 WHERE project=? AND agent=?",rusqlite::params![command["rules"].as_str(),command["capabilities"].to_string(),next,project,aid])?;
                db.execute("UPDATE collaborator_agents SET enabled=0,revision=revision+1 WHERE project=? AND agent=?",[project,aid])?;
                db.execute("UPDATE response_policies SET respond=0,revision=revision+1 WHERE project=? AND agent=?",[project,aid])?;
            } else {
                db.execute(
                    "INSERT INTO agent_projects VALUES(?,?,?, ?,1,0)",
                    rusqlite::params![
                        project,
                        aid,
                        command["rules"].as_str(),
                        command["capabilities"].to_string()
                    ],
                )?;
            }
            (
                next,
                if current.is_some() { 200 } else { 201 },
                json!({"projectId":project,"agentId":aid,"participationRevision":next,"homeDeviceId":authority["home_device"]}),
            )
        } else {
            let row = row.ok_or_else(Failure::storage)?;
            let key = if kind == "rules" {
                "expectedParticipationRevision"
            } else {
                "expectedRevision"
            };
            if row["participation_revision"] != command[key] {
                return Err(stale());
            }
            let next = command[key].as_i64().ok_or_else(Failure::storage)? + 1;
            if kind == "rules" {
                db.execute("UPDATE agent_projects SET rules=?,revision=revision+1 WHERE project=? AND agent=?",rusqlite::params![command["rules"].as_str(),project,aid])?;
                (
                    next,
                    200,
                    json!({"projectId":project,"agentId":aid,"rules":command["rules"],"revision":next}),
                )
            } else {
                resources(db, project, &command["capabilities"])?;
                db.execute("UPDATE agent_projects SET capabilities_json=?,revision=revision+1 WHERE project=? AND agent=?",rusqlite::params![command["capabilities"].to_string(),project,aid])?;
                (
                    next,
                    200,
                    json!({"projectId":project,"agentId":aid,"capabilities":command["capabilities"],"revision":next}),
                )
            }
        };
        Commit {
            account,
            project,
            scope: &scope,
            command: &command,
            digest: &hash,
        }
        .save(db, state.config.now(), status, aid, next, data)
    })
}

fn parse_creation(v: &Value) -> Result<Value> {
    let invalid = |s: &str| Failure::new(400, "invalid_agent", s);
    if !v.is_object() {
        return Err(invalid("Invalid agent creation data."));
    }
    if !closed(
        v,
        &[
            "requestId",
            "id",
            "name",
            "role",
            "model",
            "backupModel",
            "instructions",
            "threadLimit",
            "toolBudget",
            "projectId",
            "homeDeviceId",
            "expectedProjectRevision",
            "expectedHomeDeviceRevision",
            "providerBinding",
            "setup",
            "avatar",
        ],
    ) {
        return Err(invalid("Unknown agent creation field."));
    }
    let mut command = json!({});
    for key in ["requestId", "id", "projectId", "homeDeviceId"] {
        command[key] = json!(id(&v[key]).map_err(|_| invalid("Invalid agent creation ID."))?);
    }
    for key in ["expectedProjectRevision", "expectedHomeDeviceRevision"] {
        command[key] =
            json!(revision(&v[key]).map_err(|_| invalid("Invalid agent creation revision."))?);
    }
    if command["requestId"] != command["id"]
        || !v["backupModel"].is_string()
        || v.get("setup").is_some_and(|v| !v.is_boolean())
        || v.get("avatar")
            .is_some_and(|v| v.as_str().is_none_or(|s| s.is_empty() || s.len() > 699052))
    {
        return Err(invalid("Invalid agent creation data."));
    }
    for (key, max) in [("name", 80), ("model", 160), ("instructions", 8000)] {
        command[key] =
            json!(field(v, key, max).map_err(|_| invalid(&format!("Field {key} is invalid.")))?);
    }
    let role = agents::role(v.get("role")).map_err(|e| invalid(&e.message))?;
    if v.get("role").is_some() {
        command["role"] = json!(role);
    }
    if v.get("toolBudget").is_some() {
        if !integer(&v["toolBudget"], 10, 200) {
            return Err(invalid("Tool steps per task must be 10 to 200."));
        }
        command["toolBudget"] = v["toolBudget"].clone();
    }
    if !integer(&v["threadLimit"], 1, 32) {
        return Err(invalid("Side thread count must be 1 to 32."));
    }
    command["threadLimit"] = v["threadLimit"].clone();
    let backup = string(v, "backupModel");
    if backup.encode_utf16().count() > 160
        || backup.chars().any(|c| c < ' ' || c == '\u{7f}')
        || backup.trim() == string(&command, "model")
    {
        return Err(invalid(
            "Fallback model is invalid or matches the primary model.",
        ));
    }
    command["backupModel"] = json!(backup.trim());
    command["providerBinding"] = if v.get("providerBinding") == Some(&Value::Null) {
        Value::Null
    } else {
        let b = &v["providerBinding"];
        if !b.is_object() {
            return Err(invalid("Provider binding must be selected or null."));
        }
        if !closed(b, &["bindingId", "expectedRevision"]) {
            return Err(invalid("Invalid provider binding."));
        }
        json!({"bindingId":id(&b["bindingId"]).map_err(|_|invalid("Invalid agent creation ID."))?,"expectedRevision":revision(&b["expectedRevision"]).map_err(|_|invalid("Invalid agent creation revision."))?})
    };
    command["setup"] = json!(v["setup"] == true);
    if let Some(avatar) = v.get("avatar") {
        command["avatar"] = avatar.clone();
    }
    Ok(command)
}

pub(super) fn create(
    state: &AppState,
    db: &Connection,
    account: &str,
    v: &Value,
    avatar: &mut super::super::avatar::Prepared,
) -> Result<Value> {
    let command = parse_creation(v)?;
    let aid = string(&command, "id");
    let project = string(&command, "projectId");
    let home = string(&command, "homeDeviceId");
    member(db, account, project, true)?;
    let original = command
        .get("avatar")
        .map(agents::avatar_input)
        .transpose()
        .map_err(|e| Failure::new(e.status, "invalid_avatar", e.message))?
        .flatten();
    let mut evidence = command.clone();
    if let Some(bytes) = &original {
        evidence["avatar"] = json!(digest_bytes(bytes));
    }
    let scope = format!("agents:create:{project}");
    let hash = digest(json!(["minarbot-agent-create-v1", evidence]));
    let prior = || -> Result<Option<Value>> {
        member(db, account, project, true)?;
        if one(
            db,
            "SELECT actor,project FROM receipts WHERE request_id=?",
            [aid],
        )?
        .is_some_and(|r| r["actor"] == account && r["project"] != project)
        {
            return Err(conflict());
        }
        replay(db, account, aid, project, &scope, &hash)
    };
    if let Some(old) = prior()? {
        return Ok(old);
    }
    let avatar = avatar
        .get(original)
        .map_err(|e| Failure::new(e.status, "invalid_avatar", e.message))?;
    transaction(db, || {
        if let Some(old) = prior()? {
            return Ok(old);
        }
        let device=one(db,"SELECT d.* FROM devices d JOIN installations i ON i.id=d.installation WHERE d.id=? AND d.account=? AND i.account=d.account AND i.revoked=0 AND d.status NOT IN('blocked','revoked')",[home,account])?.ok_or_else(missing)?;
        if device["revision"] != command["expectedHomeDeviceRevision"]
            || member(db, account, project, true)?["revision"] != command["expectedProjectRevision"]
        {
            return Err(stale());
        }
        if !json(&device["capabilities_json"])?
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == "execution"))
        {
            return Err(Failure::new(
                422,
                "capability_missing",
                "Device has not advertised execution yet.",
            ));
        }
        if exists(db, "SELECT 1 FROM agents WHERE id=?", [aid])? {
            if one(
                db,
                "SELECT home_device FROM agent_authority WHERE agent=? AND owner_account=?",
                [aid, account],
            )?
            .is_some_and(|r| r["home_device"] != home)
            {
                return Err(Failure::new(
                    409,
                    "home_device_conflict",
                    "Agent is already bound to another home device.",
                ));
            }
            return Err(Failure::new(
                409,
                "agent_exists",
                "Agent ID was already used.",
            ));
        }
        let binding = if command["providerBinding"].is_null() {
            None
        } else {
            let row=one(db,"SELECT * FROM provider_bindings WHERE id=? AND device=? AND owner_account=? AND readiness<>'revoked'",[string(&command["providerBinding"],"bindingId"),home,account])?.ok_or_else(missing)?;
            if row["revision"] != command["providerBinding"]["expectedRevision"] {
                return Err(stale());
            }
            Some(row)
        };
        if command["setup"] == true {
            let mut failure = Failure::new(
                503,
                "setup_unavailable",
                "Setup requires an executor and provider binding.",
            );
            failure.retryable = true;
            let Some(binding) = &binding else {
                return Err(failure);
            };
            if binding["readiness"] != "ready" {
                failure.code = "provider_unavailable";
                failure.message = "Provider binding is not ready yet.".into();
                return Err(failure);
            }
        }
        let caps = json!({"v":1,"tools":TOOLS,"resources":[]});
        db.execute("INSERT INTO agents(id,name,role,model,instructions,thread_limit,tool_budget,provider_instance,model_options,revision) VALUES(?,?,?,?,?,?,?,?,?,1)",rusqlite::params![aid,command["name"].as_str(),command["role"].as_str().unwrap_or(""),command["model"].as_str(),command["instructions"].as_str(),command["threadLimit"].as_i64(),command["toolBudget"].as_i64().unwrap_or(90),string(&command,"providerInstance"),command.get("modelOptions").map(|value| value.to_string()).unwrap_or_else(|| "{}".into())])?;
        db.execute(
            "INSERT INTO agent_authority VALUES(?,?,?,?,1)",
            rusqlite::params![
                aid,
                account,
                home,
                binding.as_ref().map(|b| string(b, "id"))
            ],
        )?;
        db.execute(
            "INSERT INTO agent_projects VALUES(?,?,?, ?,1,0)",
            rusqlite::params![project, aid, "", caps.to_string()],
        )?;
        db.execute(
            "INSERT INTO record_scopes VALUES('agents',?,?,?,'owner','agent',?)",
            rusqlite::params![json!([aid]).to_string(), account, project, aid],
        )?;
        db.execute(
            "INSERT INTO agent_creations VALUES(?,?)",
            rusqlite::params![aid, evidence.to_string()],
        )?;
        if let Some(image) = &avatar {
            db.execute(
                "INSERT INTO agent_avatars VALUES(?,1,?)",
                rusqlite::params![aid, image],
            )?;
        }
        if command["setup"] == true {
            limits::task_capacity(db, project, aid)?;
            let prompt=format!("Initial name: {}. Draft this agent's profile from the needs in the initial instructions. Use edit_profile to save a fitting name, a short role and clear working instructions. Do not start the actual work or change the account, permissions or model. If the needs are unclear, ask with ask_user. After saving the profile, briefly explain what the agent can and cannot do.\n\nInitial needs:\n{}",command["name"],string(&command,"instructions"));
            let payload = json!({"v":1,"kind":"direct","message":{"text":prompt,"files":[]}});
            let membership = member(db, account, project, true)?["grant_revision"].clone();
            let revisions = json!({"project":command["expectedProjectRevision"],"agent":1,"homeBinding":1,"membership":membership,"assignment":membership,"capability":1});
            let payload_digest = digest(json!([
                "minarbot-task-payload-v1",
                aid,
                project,
                aid,
                home,
                payload,
                revisions
            ]));
            let binding = binding.as_ref().ok_or_else(Failure::storage)?;
            db.execute("INSERT INTO turns(id,agent,text,attachments,status,created,profile_setup) VALUES(?,?,?,?,'queued',?,1)",rusqlite::params![aid,aid,prompt,"[]",iso(db,state.config.now())?])?;
            db.execute(
                "INSERT INTO record_scopes VALUES('turns',?,?,?,'owner','agent',?)",
                rusqlite::params![json!([aid]).to_string(), account, project, aid],
            )?;
            db.execute("INSERT INTO tasks(id,logical_turn,project,agent,initiator_account,home_device,assignment_revision,capability_revision,capability_digest,capability_snapshot_json,provider_binding_id,provider_binding_revision,provider_binding_owner,state,payload_digest,payload_json,revision,created) VALUES(?,?,?,?,?,?,?,1,?,?,?,?,?,?,?,?,1,?)",rusqlite::params![aid,aid,project,aid,account,home,membership.as_i64(),digest(json!(["minarbot-capabilities-v1",caps])),crate::db::canonical(&caps),binding["id"].as_str(),binding["revision"].as_i64(),binding["owner_account"].as_str(),if device["status"]=="ready"{"queued"}else{"waiting_device"},payload_digest,crate::db::canonical(&payload),state.config.now()])?;
        }
        Commit{account,project,scope:&scope,command:&command,digest:&hash}.save(db,state.config.now(),201,aid,1,json!({"agentId":aid,"agentRevision":1,"authorityRevision":1,"projectId":project,"participationRevision":1,"homeDeviceId":home,"avatarRevision":if avatar.is_some(){1}else{0}}))
    })
}
