#![allow(dead_code)]

use super::*;
use axum::response::IntoResponse;
use base64::{engine::general_purpose::STANDARD, Engine};
use rusqlite::{params, OptionalExtension};
use std::path::PathBuf;

pub fn read(db: &Connection, id: &str) -> Result<Value> {
    let mut agent = one(db, "SELECT * FROM agents WHERE id=?", [id])?
        .ok_or_else(|| error(404, "Agent not found."))?;
    agent["providerInstance"] = agent["provider_instance"].clone();
    agent["modelOptions"] =
        serde_json::from_str(string(&agent, "model_options")).unwrap_or_else(|_| json!({}));
    agent["runtime"] = one(
        db,
        "SELECT kind FROM provider_instances WHERE id=?",
        [string(&agent, "provider_instance")],
    )?
    .map(|row| row["kind"].clone())
    .unwrap_or(Value::Null);
    agent["provider_account"] = json!("");
    agent["backup_model"] = json!("");
    Ok(agent)
}

pub(crate) fn public_profile(mut value: Value) -> Value {
    if let Some(map) = value.as_object_mut() {
        for key in [
            "runtime",
            "provider_account",
            "backup_model",
            "provider_instance",
            "model_options",
        ] {
            map.remove(key);
        }
    }
    value
}
pub fn hidden(db: &Connection, id: &str) -> Result<bool> {
    Ok(one(
        db,
        "SELECT hidden FROM agent_visibility WHERE agent=?",
        [id],
    )?
    .is_some_and(|v| v["hidden"] == 1))
}
pub fn capabilities(runtime: &str) -> Value {
    let _ = runtime;
    json!({"queue":true,"restart":true,"steer":false})
}
pub fn executable(state: &AppState, runtime: &str) -> Option<PathBuf> {
    crate::harness::executable(state, runtime)
}
pub(super) fn reset_harness(db: &Connection, id: &str) -> Result<()> {
    for table in ["harness_sessions", "harness_policies"] {
        if super::table(db, table)? {
            db.execute(&format!("DELETE FROM {table} WHERE agent=?"), [id])?;
        }
    }
    Ok(())
}

pub(crate) fn device_provider_status(device: &str, account: &Value) -> Value {
    let runnable = account.is_string();
    json!({
        "provider_instance": account,
        "runnable": runnable,
        "blocked_reason": (!runnable).then(|| format!("No provider on {device}"))
    })
}

pub(crate) fn device_binding_account<'a>(
    accounts: &'a Value,
    binding: &Value,
) -> Option<&'a Value> {
    let account = binding["provider_account_id"].as_str()?;
    let id = binding["id"].as_str()?;
    let family = binding["provider_id"].as_str()?;
    accounts
        .as_array()?
        .iter()
        .filter(|a| a["provider"] == family && (a["id"] == account || a["id"] == id))
        .min_by_key(|a| a["id"] != account)
}

pub(crate) fn device_bindings(
    db: &Connection,
    device: &str,
    owner: &str,
    name: &str,
    accounts: &Value,
    complete: bool,
) -> Result<Value> {
    let agents = all(db,"SELECT a.id AS agent,a.model,p.id,p.provider_account_id,p.provider_id FROM agents a JOIN agent_authority h ON h.agent=a.id LEFT JOIN provider_bindings p ON p.id=h.provider_binding AND p.device=h.home_device AND p.owner_account=h.owner_account AND p.readiness='ready' WHERE h.home_device=? AND h.owner_account=? ORDER BY a.id",[device,owner])?;
    let bindings = agents
        .iter()
        .filter_map(|agent| {
            let account = device_binding_account(accounts, agent);
            if !complete && account.is_none() && !agent["id"].is_null() {
                return None;
            }
            let id = account.map_or(&Value::Null, |account| &account["id"]);
            let mut status = device_provider_status(name, id);
            status["binding"] = agent["id"].clone();
            status["model"] = if string(agent, "model").is_empty() {
                account.map_or(Value::Null, |account| account["defaultModel"].clone())
            } else {
                agent["model"].clone()
            };
            Some((string(agent, "agent").to_owned(), status))
        })
        .collect::<serde_json::Map<_, _>>();
    Ok(json!(bindings))
}

fn runtime_blocked_reason(
    state: &AppState,
    db: &Connection,
    id: &str,
    runtime: &str,
    workspace_connected: bool,
) -> Result<Option<&'static str>> {
    if executable(state, runtime).is_none() {
        return Ok(Some("This engine is unavailable on this host."));
    }
    // ponytail: use the existing probe result when present, without probing on directory polls.
    if state
        .config
        .harness
        .cache
        .try_lock()
        .ok()
        .is_some_and(|cache| {
            cache.as_ref().is_some_and(|(at, catalog)| {
                at.elapsed() < std::time::Duration::from_secs(30)
                    && catalog["choices"].as_array().is_some_and(|choices| {
                        choices.iter().any(|choice| {
                            choice["runtime"] == runtime
                                && (choice["available"] == false || choice["signedIn"] == false)
                        })
                    })
            })
        })
    {
        return Ok(Some("Check this engine in Settings > Engines."));
    }
    if !workspace_connected
        && !exists(db,
            "SELECT 1 FROM local_project_members m JOIN local_projects p ON p.id=m.project WHERE m.agent=? AND p.deleted=0 AND p.archived=0",
            [id],
        )?
    {
        return Ok(Some("This agent needs a workspace folder."));
    }
    Ok(None)
}
pub fn list(state: &AppState, db: &Connection) -> Result<Value> {
    let mut rows = all(
        db,
        "SELECT a.*,j.model AS goal_judge_model FROM agents a
        LEFT JOIN provider_instances pi ON pi.id=a.provider_instance
        LEFT JOIN goal_judge_models j ON j.agent=a.id AND j.runtime=pi.kind ORDER BY a.rowid",
        [],
    )?;
    for a in &mut rows {
        let id = string(a, "id").to_owned();
        let runtime = one(
            db,
            "SELECT kind FROM provider_instances WHERE id=?",
            [string(a, "provider_instance")],
        )?
        .map(|row| string(&row, "kind").to_owned())
        .unwrap_or_default();
        let avatar = avatar_read(db, &id)?;
        let workspace_connected = super::agent_folders::effective(state, db, &id).is_ok();
        a["workspace_connected"] = json!(workspace_connected);
        a["dispatchCapabilities"] = capabilities(&runtime);
        a["computer_waiting"] = json!(if table(db, "computer_waits")? {
            exists(
                db,
                "SELECT 1 FROM computer_waits w JOIN turns t ON t.id=w.turn WHERE w.agent=? AND w.state='waiting' AND t.status NOT IN('done','cancelled')",
                [&id],
            )
            .unwrap_or(false)
        } else {
            false
        });
        a["avatar_revision"] = if avatar["present"] == true {
            avatar["revision"].clone()
        } else {
            json!(0)
        };
        let provider_reason = match provider_instance_kind(db, string(a, "provider_instance"))? {
            None => Some("Choose a provider instance."),
            Some((_, false)) => Some("This provider instance is disabled."),
            Some((_, true)) => None,
        };
        let blocked_reason = provider_reason.or(runtime_blocked_reason(
            state,
            db,
            &id,
            &runtime,
            workspace_connected,
        )?);
        a["provider_available"] = json!(provider_reason.is_none());
        a["runnable"] = json!(blocked_reason.is_none());
        a["blocked_reason"] = json!(blocked_reason);
        a["hidden"] = json!(hidden(db, &id)?);
        a["providerInstance"] = a["provider_instance"].clone();
        a["modelOptions"] =
            serde_json::from_str(string(a, "model_options")).unwrap_or_else(|_| json!({}));
        if let Some(map) = a.as_object_mut() {
            map.remove("provider_instance");
            map.remove("model_options");
        }
        a["visibility_revision"] = one(
            db,
            "SELECT revision FROM agent_visibility WHERE agent=?",
            [&id],
        )?
        .map(|v| v["revision"].clone())
        .unwrap_or(json!(0));
    }
    let guide = rows
        .iter()
        .position(|agent| agent["id"] == crate::mcp::guide::ID)
        .map(|index| {
            let mut guide = rows.remove(index);
            guide["builtin"] = json!(true);
            if string(&guide, "model").is_empty() {
                guide["runnable"] = json!(false);
                guide["blocked_reason"] = json!("Choose an AI connection.");
            }
            guide
        });
    let configured = rows.iter().any(|agent| agent["runnable"] == true);
    Ok(json!({
        "agents": rows,
        "guide": guide,
        "hubAvailable": false,
        "configured": configured,
        "defaultModel": state.config.data.default_model.as_deref().unwrap_or("")
    }))
}
pub(super) fn role(v: Option<&Value>) -> Result<String> {
    let s = match v {
        None => "",
        Some(Value::String(s)) => s,
        _ => {
            return Err(error(
                400,
                "Role must be a single line of up to 60 characters without control characters.",
            ))
        }
    };
    if s.trim().encode_utf16().count() > 60
        || s.chars()
            .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
    {
        return Err(error(
            400,
            "Role must be a single line of up to 60 characters without control characters.",
        ));
    }
    Ok(s.trim().into())
}
fn provider_instance(v: &Value) -> Result<Option<&str>> {
    v.get("providerInstance")
        .map(|value| {
            value
                .as_str()
                .filter(|value| {
                    (1..=64).contains(&value.len())
                        && value
                            .bytes()
                            .next()
                            .is_some_and(|byte| byte.is_ascii_alphabetic())
                        && value
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
                })
                .ok_or_else(|| error(400, "Invalid provider instance."))
        })
        .transpose()
}
fn model_options(v: &Value) -> Result<Option<String>> {
    v.get("modelOptions")
        .map(|value| {
            if !value.is_object() || value.to_string().len() > 64 * 1024 {
                return Err(error(400, "Model options must be a JSON object."));
            }
            Ok(value.to_string())
        })
        .transpose()
}
fn provider_instance_kind(db: &Connection, id: &str) -> Result<Option<(String, bool)>> {
    Ok(one(
        db,
        "SELECT kind,enabled FROM provider_instances WHERE id=?",
        [id],
    )?
    .map(|row| (string(&row, "kind").to_owned(), row["enabled"] == 1)))
}
fn test_runtime_instance(state: &AppState, db: &Connection, v: &Value) -> Result<Option<String>> {
    if !state.config.harness.test_mode {
        return Ok(None);
    }
    let runtime = v.get("runtime").and_then(Value::as_str);
    let Some(runtime) = runtime.filter(|runtime| crate::harness::RUNTIMES.contains(runtime)) else {
        return Ok(None);
    };
    db.execute(
        "INSERT INTO provider_instances(id,kind,display_name) VALUES(?,?,?) ON CONFLICT(id) DO NOTHING",
        params![runtime, runtime, crate::harness::label(runtime)],
    )?;
    Ok(Some(runtime.to_owned()))
}
fn default_provider_instance(state: &AppState, db: &Connection) -> Result<Option<String>> {
    let rows = all(
        db,
        "SELECT id,kind FROM provider_instances WHERE enabled=1 ORDER BY CASE kind WHEN 'codex' THEN 0 WHEN 'claude' THEN 1 ELSE 2 END,rowid",
        [],
    )?;
    Ok(rows
        .iter()
        .find(|row| executable(state, string(row, "kind")).is_some())
        .or_else(|| rows.first())
        .map(|row| string(row, "id").to_owned()))
}
fn bounds(v: &Value) -> Result<()> {
    if v.get("threadLimit").is_some_and(|v| !integer(v, 1, 32)) {
        return Err(error(400, "Concurrent conversation count must be 1 to 32."));
    }
    if v.get("toolBudget").is_some_and(|v| !integer(v, 10, 200)) {
        return Err(error(400, "Tool steps per task must be 10 to 200."));
    }
    Ok(())
}
pub fn create(
    state: &AppState,
    db: &Connection,
    v: &Value,
    avatar: &mut super::avatar::Prepared,
) -> Result<(u16, Value)> {
    let id = string(v, "id");
    if id == crate::mcp::guide::ID {
        return Err(error(409, "This agent ID is reserved."));
    }
    if exists(db, "SELECT 1 FROM deleted_agents WHERE id=?", [id])? {
        return Err(error(409, "Agent was deleted. Use a new ID."));
    }
    if v.get("setup").is_some_and(|v| !v.is_boolean()) {
        return Err(error(400, "Invalid profile setup choice."));
    }
    let setup = v["setup"] == true;
    let input = if let Some(a) = v.get("avatar") {
        if a.is_null() {
            return Err(error(400, "Invalid avatar."));
        }
        avatar_input(a)?
    } else {
        None
    };
    let name = field(v, "name", 80)?;
    let model = field(v, "model", 160)?;
    let instructions = field(v, "instructions", 8000)?;
    let role = role(v.get("role"))?;
    let previous = one(db, "SELECT request FROM agent_creations WHERE id=?", [id])?;
    let provider_instance = provider_instance(v)?
        .map(str::to_owned)
        .or(default_provider_instance(state, db)?);
    let model_options = model_options(v)?;
    let old_runtime = previous
        .as_ref()
        .map(|r| json(&r["request"]))
        .transpose()?
        .and_then(|v| v["providerInstance"].as_str().map(str::to_owned));
    let instance_kind = provider_instance
        .as_deref()
        .map(|id| provider_instance_kind(db, id))
        .transpose()?
        .flatten();
    if provider_instance.is_some() && instance_kind.is_none() {
        return Err(error(400, "Provider instance not found."));
    }
    if provider_instance.is_none() && old_runtime.is_some() {
        return Err(error(400, "Choose a provider instance."));
    }
    bounds(v)?;
    let mut request = json!({"name":name,"model":model,"instructions":instructions});
    if setup {
        request["setup"] = json!(true);
    }
    if let Some(bytes) = &input {
        request["avatarDigest"] = json!(super::digest_bytes(bytes));
    }
    if !role.is_empty() {
        request["role"] = json!(role);
    }
    for key in ["threadLimit", "toolBudget"] {
        if let Some(value) = v.get(key) {
            request[key] = value.clone();
        }
    }
    if let Some(instance) = &provider_instance {
        request["providerInstance"] = json!(instance);
    }
    if let Some(options) = &model_options {
        request["modelOptions"] = serde_json::from_str(options).map_err(|_| Failure::storage())?;
    }
    if let Some(old) = previous {
        if json(&old["request"])? != request {
            return Err(error(409, "ID was already used for a different agent."));
        }
        let mut a = read(db, id)?;
        if v.get("avatar").is_some() {
            let avatar = avatar_read(db, id)?;
            a["avatar_revision"] = if avatar["present"] == true {
                avatar["revision"].clone()
            } else {
                json!(0)
            };
        }
        return Ok((200, a));
    }
    let image = avatar.get(input)?;
    super::agent_folders::ensure_managed(state, id)?;
    db.execute("INSERT INTO agents(id,name,role,model,instructions,thread_limit,tool_budget,provider_instance,model_options) VALUES(?,?,?,?,?,?,?,?,?)",rusqlite::params![id,name,role,model,instructions,v["threadLimit"].as_i64().unwrap_or(2),v["toolBudget"].as_i64().unwrap_or(90),provider_instance.as_deref().unwrap_or(""),model_options.as_deref().unwrap_or("{}")])?;
    crate::connectors::mailbox::register(db, id, &name)?;
    if setup {
        let prompt=format!("Initial name: {}. Draft this agent's profile from the needs in the initial instructions. Use edit_profile to save a fitting name, a short role and clear working instructions. Do not start the actual work or change the account, permissions or model. If the needs are unclear, ask with ask_user. After saving the profile, briefly explain what the agent can and cannot do.\n\nInitial needs:\n{instructions}",json!(name));
        db.execute(
            "INSERT INTO turns(id,agent,text,status,created) VALUES(?,?,?,'queued',?)",
            rusqlite::params![id, id, prompt, iso(db, state.config.now())?],
        )?;
    }
    if let Some(image) = image {
        db.execute(
            "INSERT INTO agent_avatars VALUES(?,1,?)",
            rusqlite::params![id, image],
        )?;
    }
    db.execute(
        "INSERT INTO agent_creations VALUES(?,?)",
        rusqlite::params![id, request.to_string()],
    )?;
    crate::mcp::guide::inherit_connection(state, db)?;
    let mut result = read(db, id)?;
    if v.get("avatar").is_some() {
        result["avatar_revision"] = json!(1);
    }
    Ok((201, result))
}
pub fn edit(state: &AppState, db: &Connection, id: &str, v: &Value) -> Result<Value> {
    let mut guide_edit;
    let v = if id == crate::mcp::guide::ID {
        if !closed(
            v,
            &[
                "id",
                "revision",
                "model",
                "providerInstance",
                "modelOptions",
                "confirmRuntime",
            ],
        ) {
            return Err(error(
                400,
                "Change the guide's AI connection from its conversation.",
            ));
        }
        guide_edit = v.clone();
        guide_edit["name"] = json!(crate::mcp::guide::NAME);
        guide_edit["instructions"] = json!(crate::mcp::guide::PROFILE);
        &guide_edit
    } else {
        v
    };
    let name = field(v, "name", 80)?;
    let model = field(v, "model", 160)?;
    let instructions = field(v, "instructions", 8000)?;
    let role = v.get("role").map(|v| role(Some(v))).transpose()?;
    let provider_instance = provider_instance(v)?
        .map(str::to_owned)
        .or(test_runtime_instance(state, db, v)?);
    let model_options = model_options(v)?;
    let instance_kind = provider_instance
        .as_deref()
        .map(|id| provider_instance_kind(db, id))
        .transpose()?
        .flatten();
    if provider_instance.is_some() && instance_kind.is_none() {
        return Err(error(400, "Provider instance not found."));
    }
    bounds(v)?;
    if v.get("webAccess").is_some_and(|value| !value.is_boolean()) {
        return Err(error(400, "Web access must be a boolean."));
    }
    if v.get("libraryDocsAccess")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(error(400, "Library docs access must be a boolean."));
    }
    let learn = v
        .get("learnFromConversations")
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| error(400, "Learn from conversations must be a boolean."))
        })
        .transpose()?;
    let memory_model = v
        .get("memoryModel")
        .map(|value| {
            value
                .as_str()
                .filter(|s| {
                    s.encode_utf16().count() <= 160
                        && !s
                            .chars()
                            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
                })
                .map(str::trim)
                .ok_or_else(|| error(400, "Invalid memory model."))
        })
        .transpose()?;
    if !integer(&v["revision"], 1, 9_007_199_254_740_991) {
        return Err(error(400, "Invalid agent revision."));
    }
    let script_provider = v["scriptProvider"] == true;
    let mut request = if script_provider {
        json!({"agent":id,"revision":v["revision"],"scriptProvider":true,"model":model})
    } else {
        json!({"agent":id,"revision":v["revision"],"name":name,"model":model,"instructions":instructions})
    };
    for key in [
        "role",
        "harnessModel",
        "threadLimit",
        "toolBudget",
        "providerInstance",
        "modelOptions",
        "webAccess",
        "goalJudgeModel",
        "learnFromConversations",
        "memoryModel",
    ] {
        if let Some(value) = v.get(key) {
            request[key] = value.clone();
        }
    }
    if let Some(old) = receipt(
        db,
        "agent_edits",
        string(v, "id"),
        &request,
        "ID was already used for a different change.",
    )? {
        return Ok(old);
    }
    let current = read(db, id)?;
    if current["revision"] != v["revision"] {
        return Err(error(
            409,
            "Profile changed. Reload the profile before saving.",
        ));
    }
    if exists(
        db,
        "SELECT 1 FROM turns WHERE agent=? AND status IN('queued','running') LIMIT 1",
        [id],
    )? {
        return Err(error(
            409,
            "Wait for or cancel agent work before changing the profile.",
        ));
    }
    let selected_runtime = instance_kind
        .as_ref()
        .map(|(kind, _)| kind.clone())
        .or_else(|| {
            provider_instance_kind(db, string(&current, "providerInstance"))
                .ok()
                .flatten()
                .map(|(kind, _)| kind)
        })
        .unwrap_or_default();
    let instance_changed = provider_instance
        .as_deref()
        .is_some_and(|instance| instance != string(&current, "providerInstance"));
    if instance_changed {
        reset_harness(db, id)?;
    }
    if let Some(raw) = v.get("harnessModel") {
        let harness_model = raw
            .as_str()
            .filter(|v| {
                v.encode_utf16().count() <= 160 && !v.chars().any(|c| c < ' ' || c == '\u{7f}')
            })
            .ok_or_else(|| error(400, "Invalid harness model."))?
            .trim();
        let selected = selected_runtime.as_str();
        if one(
            db,
            "SELECT model FROM harness_models WHERE agent=? AND runtime=?",
            [id, selected],
        )?
        .is_none_or(|r| r["model"] != harness_model)
        {
            db.execute(
                "DELETE FROM harness_sessions WHERE agent=? AND runtime=?",
                [id, selected],
            )?;
        }
        db.execute("INSERT INTO harness_models(agent,runtime,model) VALUES(?,?,?) ON CONFLICT(agent,runtime) DO UPDATE SET model=excluded.model",[id,selected,harness_model])?;
    }
    let binding_changed = instance_changed;
    let memory_model = memory_model.or(binding_changed.then_some(""));
    db.execute("UPDATE agents SET name=?,role=coalesce(?,role),model=?,instructions=?,thread_limit=coalesce(?,thread_limit),tool_budget=coalesce(?,tool_budget),provider_instance=coalesce(?,provider_instance),model_options=coalesce(?,model_options),web_access=coalesce(?,web_access),learn_from_conversations=coalesce(?,learn_from_conversations),memory_model=coalesce(?,memory_model),revision=revision+1 WHERE id=?",rusqlite::params![name,role,model,instructions,v["threadLimit"].as_i64(),v["toolBudget"].as_i64(),provider_instance,model_options,v["webAccess"].as_bool(),learn,memory_model,id])?;
    if let Some(model) = v.get("goalJudgeModel") {
        if !selected_runtime.is_empty() {
            crate::turns::goals::save_model(db, id, &selected_runtime, model)?;
        }
    }
    let mut updated = read(db, id)?;
    if state.auth.is_none() && v.get("goalJudgeModel").is_some() {
        updated["goal_judge_model"] = crate::turns::goals::model(db, id)?["model"].clone();
    }
    db.execute(
        "INSERT INTO agent_edits VALUES(?,?,?,?)",
        rusqlite::params![
            string(v, "id"),
            id,
            request.to_string(),
            updated.to_string()
        ],
    )?;
    Ok(updated)
}
pub fn visibility(state: &AppState, db: &Connection, id: &str, v: &Value) -> Result<Value> {
    if !v["hidden"].is_boolean() || !integer(&v["revision"], 0, 9_007_199_254_740_991) {
        return Err(error(400, "Invalid agent status."));
    }
    let request = json!({"agent":id,"hidden":v["hidden"],"revision":v["revision"]});
    if let Some(old) = receipt(
        db,
        "agent_visibility_commands",
        string(v, "id"),
        &request,
        "ID was already used for a different change.",
    )? {
        return Ok(old);
    }
    read(db, id)?;
    let current = one(
        db,
        "SELECT revision,hidden FROM agent_visibility WHERE agent=?",
        [id],
    )?;
    let revision = v["revision"].as_i64().ok_or_else(Failure::storage)?;
    if current
        .as_ref()
        .and_then(|v| v["revision"].as_i64())
        .unwrap_or(0)
        != revision
    {
        return Err(error(
            409,
            "Agent status changed. Reload before trying again.",
        ));
    }
    if v["hidden"] == false && current.as_ref().is_some_and(|r| r["hidden"] == 1) {
        let now = state.config.now();
        if table(db, "routines")? {
            db.execute("INSERT OR IGNORE INTO agent_visibility_skips SELECT agent,id,next_at,CASE WHEN interval_ms IS NULL THEN 0 ELSE CAST((?-next_at)/interval_ms AS INTEGER) END FROM routines WHERE agent=? AND enabled=1 AND trigger='schedule' AND next_at<=?", rusqlite::params![now,id,now])?;
            db.execute("UPDATE routines SET enabled=CASE WHEN interval_ms IS NULL THEN 0 ELSE enabled END,next_at=CASE WHEN interval_ms IS NULL THEN next_at ELSE next_at+(CAST((?-next_at)/interval_ms AS INTEGER)+1)*interval_ms END,revision=revision+1 WHERE agent=? AND enabled=1 AND trigger='schedule' AND next_at<=?",rusqlite::params![now,id,now])?;
        }
        if table(db, "workflow_schedules")? && table(db, "workflows")? {
            for row in all(db,"SELECT workflow_schedules.* FROM workflow_schedules JOIN workflows ON workflows.id=workflow_schedules.workflow WHERE workflows.agent=? AND enabled=1 AND next_at<=?",rusqlite::params![id,now])? {
                let at = row["next_at"].as_i64().ok_or_else(Failure::storage)?;
                let interval = row["interval_ms"].as_i64();
                let missed = interval.filter(|i| *i > 0).map_or(0, |i| (now-at)/i);
                db.execute("INSERT OR IGNORE INTO workflow_occurrences VALUES (?,?,NULL,?,'hidden')",rusqlite::params![row["workflow"].as_str(),at,missed])?;
                db.execute("UPDATE workflow_schedules SET enabled=?,next_at=?,revision=revision+1 WHERE workflow=?",rusqlite::params![interval.is_some() as i64,interval.map_or(at, |i|at+(missed+1)*i),row["workflow"].as_str()])?;
            }
        }
    }
    db.execute("INSERT INTO agent_visibility VALUES(?,?,?) ON CONFLICT(agent) DO UPDATE SET hidden=excluded.hidden,revision=excluded.revision",rusqlite::params![id,(v["hidden"]==true)as i64,revision+1])?;
    let result = json!({"hidden":v["hidden"],"revision":revision+1});
    save_receipt(
        db,
        "agent_visibility_commands",
        string(v, "id"),
        &request,
        &result,
    )?;
    Ok(result)
}
pub fn template(db: &Connection, id: &str) -> Result<Value> {
    let a = read(db, id)?;
    let mut out = json!({"format":"minarbot-agent","version":1,"name":a["name"],"role":a["role"],"model":a["model"],"instructions":a["instructions"],"threadLimit":a["thread_limit"]});
    if a["tool_budget"] != 90 {
        out["toolBudget"] = a["tool_budget"].clone();
    }
    if let Some(image) = image(db, id)? {
        out["avatar"] = json!(STANDARD.encode(image));
    }
    Ok(out)
}
fn image(db: &Connection, id: &str) -> Result<Option<Vec<u8>>> {
    Ok(db
        .query_row("SELECT image FROM agent_avatars WHERE agent=?", [id], |r| {
            r.get::<_, Option<Vec<u8>>>(0)
        })
        .optional()?
        .flatten())
}
pub fn avatar_read(db: &Connection, id: &str) -> Result<Value> {
    read(db, id)?;
    Ok(one(
        db,
        "SELECT revision,image IS NOT NULL AS present FROM agent_avatars WHERE agent=?",
        [id],
    )?
    .map(|v| json!({"revision":v["revision"],"present":v["present"]==1}))
    .unwrap_or(json!({"revision":0,"present":false})))
}
pub(super) fn avatar_input(v: &Value) -> Result<Option<Vec<u8>>> {
    if v.is_null() {
        return Ok(None);
    }
    let s = v
        .as_str()
        .ok_or_else(|| error(400, "Invalid avatar image."))?;
    if s.is_empty() || s.len() > 699052 {
        return Err(error(413, "Avatar limit: 512 KiB."));
    }
    let bytes = STANDARD
        .decode(s)
        .ok()
        .filter(|b| b.len() <= 512 * 1024 && STANDARD.encode(b) == s)
        .ok_or_else(|| {
            error(
                400,
                "Choose a valid PNG, JPEG, or WebP image, up to 512 KiB.",
            )
        })?;
    let format = image::guess_format(&bytes)
        .ok()
        .filter(|f| {
            [
                image::ImageFormat::Png,
                image::ImageFormat::Jpeg,
                image::ImageFormat::WebP,
            ]
            .contains(f)
        })
        .ok_or_else(|| {
            error(
                400,
                "Choose a valid PNG, JPEG, or WebP image, up to 512 KiB.",
            )
        })?;
    if format == image::ImageFormat::Png {
        let mut offset = 8;
        while offset + 12 <= bytes.len() {
            let len = u32::from_be_bytes(
                bytes[offset..offset + 4]
                    .try_into()
                    .map_err(|_| Failure::storage())?,
            ) as usize;
            let kind = &bytes[offset + 4..offset + 8];
            if offset + 12 + len > bytes.len() {
                return Err(error(400, "Incomplete PNG image."));
            }
            if kind == b"acTL" || kind == b"fcTL" || kind == b"fdAT" {
                return Err(error(400, "Use a static image, not an animation."));
            }
            offset += 12 + len;
        }
    }
    Ok(Some(bytes))
}

pub fn avatar_save(
    db: &Connection,
    id: &str,
    v: &Value,
    avatar: &mut super::avatar::Prepared,
) -> Result<Value> {
    read(db, id)?;
    if !closed(v, &["id", "revision", "image"])
        || !integer(&v["revision"], 0, 9_007_199_254_740_991)
        || (!v["image"].is_null() && !v["image"].is_string())
        || v.get("image").is_none()
    {
        return Err(error(400, "Invalid avatar request."));
    }
    let input = avatar_input(&v["image"])?;
    let request = json!({"agent":id,"revision":v["revision"],"digest":input.as_ref().map(|b|super::digest_bytes(b))});
    if let Some(old) = receipt(
        db,
        "avatar_commands",
        string(v, "id"),
        &request,
        "Avatar change ID was already used for a different request.",
    )? {
        return Ok(old);
    }
    if avatar_read(db, id)?["revision"] != v["revision"] {
        return Err(error(409, "Avatar changed. Reload before saving."));
    }
    let image = avatar.get(input)?;
    let revision = v["revision"].as_i64().ok_or_else(Failure::storage)? + 1;
    db.execute("INSERT INTO agent_avatars VALUES(?,?,?) ON CONFLICT(agent) DO UPDATE SET revision=excluded.revision,image=excluded.image",rusqlite::params![id,revision,image])?;
    let result = json!({"revision":revision,"present":image.is_some()});
    save_receipt(db, "avatar_commands", string(v, "id"), &request, &result)?;
    Ok(result)
}
pub fn avatar_image(
    state: &AppState,
    db: &Connection,
    id: &str,
    h: &HeaderMap,
) -> Result<Response> {
    read(db, id)?;
    if h.contains_key("origin") && !origin(state, h) {
        return Err(error(403, "Origin is not allowed."));
    }
    let image = image(db, id)?.ok_or_else(|| error(404, "Avatar is not available yet."))?;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "image/png"),
            (
                axum::http::HeaderName::from_static("cross-origin-resource-policy"),
                "same-origin",
            ),
        ],
        image,
    )
        .into_response())
}
pub fn delete(
    _state: &AppState,
    db: &Connection,
    id: &str,
    v: &Value,
    q: &[(String, String)],
    h: &HeaderMap,
) -> Result<Value> {
    if !q.is_empty() || h.contains_key("authorization") {
        return Err(error(403, "Agent deletion requires the local owner."));
    }
    if !closed(v, &["id"]) || !uuid(id) {
        return Err(error(400, "Invalid agent deletion request."));
    }
    if id == crate::mcp::guide::ID {
        return Err(error(409, "The built-in guide cannot be deleted."));
    }
    let present = exists(db, "SELECT 1 FROM agents WHERE id=?", [id])?;
    if !present && !exists(db, "SELECT 1 FROM deleted_agents WHERE id=?", [id])? {
        return Err(error(404, "Agent not found."));
    }
    if present {
        if one(
            db,
            "SELECT count(*) n FROM agents WHERE id<>?",
            [crate::mcp::guide::ID],
        )?
        .is_some_and(|v| v["n"].as_i64().unwrap_or(0) <= 1)
        {
            return Err(error(409, "The last agent cannot be deleted."));
        }
        if exists(db,"SELECT 1 FROM turns WHERE (agent=? OR recipient=?) AND(status IN('queued','running') OR paused=1)",rusqlite::params![id,id])?{return Err(error(409,"Stop or finish agent work before deleting."));}
        if table(db,"workflow_runs")? && exists(db,"SELECT 1 FROM workflow_runs WHERE agent=? AND status NOT IN('done','failed','cancelled')",[id])? || exists(db,"SELECT 1 FROM room_discussions r JOIN turns t ON t.channel IS NOT NULL AND(t.id=r.root OR t.id IN(SELECT turn FROM room_turns WHERE root=r.root)) WHERE(r.lead=? OR EXISTS(SELECT 1 FROM json_each(r.members) WHERE value=?)) AND(t.status IN('queued','running') OR t.paused=1) LIMIT 1",[id,id])? {
            return Err(error(409,"Stop or finish agent work before deleting."));
        }
        for grant in all(
            db,
            "SELECT project_id,participation_id,value FROM hub_link_grants WHERE json_extract(value,'$.localAgentId')=? UNION ALL SELECT project_id,participation_id,json_extract(value,'$.grant') AS value FROM hub_link_requests WHERE json_extract(value,'$.grant.localAgentId')=?",
            [id, id],
        )? {
            let value: Value = serde_json::from_str(string(&grant, "value")).map_err(|_| Failure::storage())?;
            let revision = value["revision"].as_i64().filter(|revision| *revision > 0).ok_or_else(Failure::storage)?;
            db.execute(
                    "INSERT INTO hub_link_withdrawals(project_id,participation_id,revision) VALUES(?,?,?) ON CONFLICT(project_id,participation_id) DO UPDATE SET revision=excluded.revision",
                    rusqlite::params![string(&grant, "project_id"), string(&grant, "participation_id"), revision],
            )?;
        }
        super::deletion::remove(db, id)?;
    }
    Ok(json!({"deleted":true}))
}

pub fn scripts(state: &AppState, db: &Connection, id: &str, v: &Value, get: bool) -> Result<Value> {
    read(db, id)?;
    let current = one(
        db,
        "SELECT enabled,revision FROM script_permissions WHERE agent=?",
        [id],
    )?
    .unwrap_or(json!({"enabled":0,"revision":0}));
    if get {
        let mut result = current;
        result["configured"] = json!(state.config.remaining_data.script_token.is_some());
        return Ok(result);
    }
    if !integer(&v["revision"], 0, 9_007_199_254_740_991) || !v["enabled"].is_boolean() {
        return Err(error(400, "Invalid script permission."));
    }
    let request = json!({"path":format!("/api/agents/{id}/scripts"),"enabled":v["enabled"],"revision":v["revision"]});
    if let Some(old) = receipt(
        db,
        "script_commands",
        string(v, "id"),
        &request,
        "ID was already used for a different change.",
    )? {
        return Ok(old);
    }
    if current["revision"] != v["revision"] {
        return Err(error(
            409,
            "Script permission changed. Reload before saving.",
        ));
    }
    if v["enabled"] == true && state.config.remaining_data.script_token.is_none() {
        return Err(error(
            409,
            "Script token is not configured on the server yet.",
        ));
    }
    let revision = current["revision"].as_i64().ok_or_else(Failure::storage)? + 1;
    let enabled = i64::from(v["enabled"] == true);
    db.execute("INSERT INTO script_permissions VALUES (?,?,?) ON CONFLICT(agent) DO UPDATE SET enabled=excluded.enabled,revision=excluded.revision", rusqlite::params![id,enabled,revision])?;
    let result = json!({"enabled":enabled,"revision":revision});
    save_receipt(db, "script_commands", string(v, "id"), &request, &result)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_binding_uses_only_the_reported_instance() {
        let report = json!([{"id":"instance","provider":"codex","defaultModel":"fixture"}]);
        let binding = json!({"id":"grant","provider_account_id":"missing","provider_id":"codex"});
        assert!(device_binding_account(&report, &binding).is_none());
        let mut binding = binding;
        binding["provider_account_id"] = json!("instance");
        assert_eq!(
            device_binding_account(&report, &binding).map(|account| &account["id"]),
            Some(&json!("instance"))
        );
    }

    #[test]
    fn dispatch_capabilities_disable_steering() {
        for runtime in ["codex", "claude", "hermes", "gemini", "kiro"] {
            assert_eq!(
                capabilities(runtime),
                json!({"queue": true, "restart": true, "steer": false})
            );
        }
    }
}
