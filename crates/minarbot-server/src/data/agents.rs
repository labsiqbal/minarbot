use super::*;
use axum::response::IntoResponse;
use base64::{engine::general_purpose::STANDARD, Engine};
use rusqlite::OptionalExtension;
use std::path::PathBuf;

pub fn read(db: &Connection, id: &str) -> Result<Value> {
    one(db, "SELECT * FROM agents WHERE id=?", [id])?.ok_or_else(|| error(404, "Agent not found."))
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
    json!({"queue":true,"restart":true,"steer":runtime=="api"})
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

pub fn provider_blocked_reason(
    runtime: &str,
    account: Option<&providers::Account>,
) -> Option<&'static str> {
    let family = account
        .filter(|account| !account.base.is_empty())
        .map(|account| account.family.as_str());
    match runtime {
        "api" if family.is_none() => Some("This agent needs a provider account."),
        "codex" if family != Some("openai-compatible") => {
            Some("Codex needs an OpenAI-compatible provider.")
        }
        // Claude's isolated home passes any configured base to its Anthropic client.
        "claude" if family.is_none() => Some("Claude Code needs an Anthropic-compatible endpoint."),
        "api" | "codex" | "claude" => None,
        _ if account.is_none() => Some("This agent's provider account is unavailable."),
        _ => None,
    }
}

pub fn binding_blocked_reason(
    db: &Connection,
    runtime: &str,
    binding: &str,
    accounts: &[providers::Account],
) -> Result<Option<&'static str>> {
    if binding == super::logins::DEVICE {
        return Ok(
            (!super::logins::device_runtime(runtime)).then_some(super::logins::DEVICE_RUNTIME_ONLY)
        );
    }
    if binding.starts_with(super::logins::PREFIX) {
        let profile = super::logins::bound(db, binding)?;
        return Ok(super::logins::blocked_reason(runtime, profile.as_ref()));
    }
    Ok(provider_blocked_reason(
        runtime,
        accounts.iter().find(|account| account.id == binding),
    ))
}

pub(crate) fn device_provider_status(device: &str, account: &Value) -> Value {
    let runnable = account.is_string();
    json!({
        "provider_account": account,
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
    if runtime == "api" {
        return Ok(None);
    }
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
    let accounts = providers::accounts(state)?;
    let mut rows = all(db, "SELECT * FROM agents ORDER BY rowid", [])?;
    for a in &mut rows {
        let id = string(a, "id").to_owned();
        let runtime = string(a, "runtime").to_owned();
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
        let provider_reason =
            binding_blocked_reason(db, &runtime, string(a, "provider_account"), &accounts)?;
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
        a["visibility_revision"] = one(
            db,
            "SELECT revision FROM agent_visibility WHERE agent=?",
            [&id],
        )?
        .map(|v| v["revision"].clone())
        .unwrap_or(json!(0));
    }
    let configured = rows.iter().any(|agent| agent["runnable"] == true);
    Ok(json!({
        "agents": rows,
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
fn backup(v: &Value, model: &str) -> Result<Option<String>> {
    let Some(value) = v.get("backupModel") else {
        return Ok(None);
    };
    let s=value.as_str().filter(|s|s.encode_utf16().count()<=160&&!s.chars().any(|c|c<' '||c=='\u{7f}')&&s.trim()!=model).ok_or_else(||error(400,"Fallback model must differ from the primary model, up to 160 characters. Leave empty to disable."))?;
    Ok(Some(s.trim().into()))
}
fn account(v: &Value) -> Result<Option<&str>> {
    v.get("providerAccount")
        .map(|v| {
            v.as_str()
                .filter(|s| s.encode_utf16().count() <= 64)
                .ok_or_else(|| error(400, "Invalid provider account."))
        })
        .transpose()
}
fn validate_account(state: &AppState, db: &Connection, runtime: &str, binding: &str) -> Result<()> {
    if binding == super::logins::DEVICE {
        if !super::logins::device_runtime(runtime) {
            return Err(error(400, super::logins::DEVICE_RUNTIME_ONLY));
        }
    } else if binding.starts_with(super::logins::PREFIX) {
        let profile = super::logins::bound(db, binding)?
            .ok_or_else(|| error(400, "Engine login profile not found."))?;
        if profile["runtime"] != runtime {
            return Err(error(400, "Choose an engine login for this runtime."));
        }
    } else if !providers::accounts(state)
        .is_ok_and(|accounts| accounts.iter().any(|account| account.id == binding))
    {
        return Err(error(400, "Provider account not found."));
    }
    Ok(())
}
fn runtime(v: &Value) -> Result<Option<&str>> {
    v.get("runtime")
        .map(|v| {
            v.as_str()
                .filter(|s| {
                    [
                        "api",
                        "codex",
                        "claude",
                        "hermes",
                        "antigravity",
                        "grok",
                        "pi",
                        "opencode",
                        "gemini",
                        "kiro",
                    ]
                    .contains(s)
                })
                .ok_or_else(|| error(400, "Invalid agent runtime."))
        })
        .transpose()
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
    let backup = backup(v, &model)?;
    let account = account(v)?;
    let old_runtime = previous
        .as_ref()
        .map(|r| json(&r["request"]))
        .transpose()?
        .and_then(|v| v["runtime"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "api".into());
    let runtime = runtime(v)?.map(str::to_owned).unwrap_or_else(|| {
        if previous.is_some() {
            old_runtime
        } else if executable(state, "codex").is_some() {
            "codex".into()
        } else {
            "api".into()
        }
    });
    if previous.is_none() && runtime != "api" && executable(state, &runtime).is_none() {
        return Err(error(
            400,
            "Coding runtime requires an installed executable on PATH.",
        ));
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
    if let Some(b) = &backup {
        request["backupModel"] = json!(b);
    }
    if let Some(a) = account {
        request["providerAccount"] = json!(a);
    }
    if runtime != "api" {
        request["runtime"] = json!(runtime);
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
    if let Some(account) = account {
        validate_account(state, db, &runtime, account)?;
    }
    let image = avatar.get(input)?;
    super::agent_folders::ensure_managed(state, id)?;
    db.execute("INSERT INTO agents(id,name,role,model,instructions,thread_limit,tool_budget,backup_model,provider_account,runtime) VALUES(?,?,?,?,?,?,?,?,?,?)",rusqlite::params![id,name,role,model,instructions,v["threadLimit"].as_i64().unwrap_or(2),v["toolBudget"].as_i64().unwrap_or(90),backup.as_deref().unwrap_or(""),account.unwrap_or(""),runtime])?;
    crate::connectors::mailbox::register(db, id, &name)?;
    if setup {
        let a = providers::accounts(state)?
            .into_iter()
            .find(|a| a.id == account.unwrap_or(""));
        if runtime == "api" && a.as_ref().is_none_or(|a| a.base.is_empty()) {
            return Err(error(503,"The default provider account is not configured. Check server configuration before sending again."));
        }
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
    let mut result = read(db, id)?;
    if v.get("avatar").is_some() {
        result["avatar_revision"] = json!(1);
    }
    Ok((201, result))
}
pub fn edit(state: &AppState, db: &Connection, id: &str, v: &Value) -> Result<Value> {
    let name = field(v, "name", 80)?;
    let model = field(v, "model", 160)?;
    let instructions = field(v, "instructions", 8000)?;
    let role = v.get("role").map(|v| role(Some(v))).transpose()?;
    let backup = backup(v, &model)?;
    let account = account(v)?;
    let runtime = runtime(v)?;
    bounds(v)?;
    if v.get("webAccess").is_some_and(|value| !value.is_boolean()) {
        return Err(error(400, "Web access must be a boolean."));
    }
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
        "runtime",
        "harnessModel",
        "threadLimit",
        "toolBudget",
        "backupModel",
        "providerAccount",
        "webAccess",
    ] {
        if let Some(value) = v.get(key) {
            request[key] = value.clone();
        }
    }
    if runtime.is_some() {
        request["confirmRuntime"] = v["confirmRuntime"].clone();
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
    if backup.is_none() && current["backup_model"] == model {
        return Err(error(400,"Primary and fallback models must differ. Clear the fallback model or choose another model."));
    }
    let saved_binding = string(&current, "provider_account");
    let selected_runtime = runtime.unwrap_or_else(|| string(&current, "runtime"));
    if let Some(binding) = account.or_else(|| {
        (runtime.is_some()
            && (saved_binding.starts_with(super::logins::PREFIX)
                || saved_binding == super::logins::DEVICE))
            .then_some(saved_binding)
    }) {
        validate_account(state, db, selected_runtime, binding)?;
    }
    if let Some(runtime) = runtime {
        if runtime != string(&current, "runtime") {
            if v["confirmRuntime"] != true {
                return Err(error(
                    409,
                    "Confirm the runtime change in Minarbot before saving.",
                ));
            }
            if runtime != "api" && executable(state, runtime).is_none() {
                return Err(error(
                    400,
                    "Coding runtime requires an installed executable on PATH.",
                ));
            }
            db.execute("UPDATE agents SET runtime=? WHERE id=?", [runtime, id])?;
            reset_harness(db, id)?;
        }
    }
    if let Some(raw) = v.get("harnessModel") {
        let harness_model = raw
            .as_str()
            .filter(|v| {
                v.encode_utf16().count() <= 160 && !v.chars().any(|c| c < ' ' || c == '\u{7f}')
            })
            .ok_or_else(|| error(400, "Invalid harness model."))?
            .trim();
        let selected = runtime.unwrap_or_else(|| string(&current, "runtime"));
        if matches!(selected, "api" | "codex" | "claude") {
            return Err(error(400, "Harness model is unavailable for this runtime."));
        }
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
    db.execute("UPDATE agents SET name=?,role=coalesce(?,role),model=?,instructions=?,thread_limit=coalesce(?,thread_limit),tool_budget=coalesce(?,tool_budget),backup_model=coalesce(?,backup_model),provider_account=coalesce(?,provider_account),web_access=coalesce(?,web_access),revision=revision+1 WHERE id=?",rusqlite::params![name,role,model,instructions,v["threadLimit"].as_i64(),v["toolBudget"].as_i64(),backup,account,v["webAccess"].as_bool(),id])?;
    if account.is_some_and(|a| a != string(&current, "provider_account"))
        && table(db, "model_fallbacks")?
    {
        db.execute("DELETE FROM model_fallbacks WHERE agent=?", [id])?;
    }
    let updated = read(db, id)?;
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
    if !string(&a, "backup_model").is_empty() {
        out["backupModel"] = a["backup_model"].clone();
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
    let present = exists(db, "SELECT 1 FROM agents WHERE id=?", [id])?;
    if !present && !exists(db, "SELECT 1 FROM deleted_agents WHERE id=?", [id])? {
        return Err(error(404, "Agent not found."));
    }
    if present {
        if one(db, "SELECT count(*) n FROM agents", [])?
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
    fn device_sign_in_binds_only_codex_and_claude() -> Result<()> {
        let db = Connection::open_in_memory()?;
        for (runtime, runnable) in [
            ("codex", true),
            ("claude", true),
            ("api", false),
            ("pi", false),
        ] {
            assert_eq!(
                binding_blocked_reason(&db, runtime, super::super::logins::DEVICE, &[])?.is_none(),
                runnable,
                "{runtime}"
            );
        }
        Ok(())
    }

    #[test]
    fn device_binding_uses_only_the_reported_account() {
        let report = json!([{"id":"device-account","provider":"openai-compatible","defaultModel":"fixture"}]);
        let binding =
            json!({"id":"grant","provider_account_id":"missing","provider_id":"openai-compatible"});
        let account = device_binding_account(&report, &binding);
        let status = device_provider_status(
            "Laptop",
            account.map_or(&Value::Null, |account| &account["id"]),
        );
        assert_eq!(status["runnable"], false);
        assert_eq!(status["blocked_reason"], "No provider on Laptop");
        let mut binding = binding;
        binding["provider_account_id"] = json!("device-account");
        assert_eq!(
            device_binding_account(&report, &binding).map(|account| &account["id"]),
            Some(&json!("device-account"))
        );
    }

    #[test]
    fn runtime_provider_requirements_match_execution() {
        let account = |family: &str, base: &str| providers::Account {
            id: String::new(),
            name: "Fixture".into(),
            family: family.into(),
            base: base.into(),
            key: None,
            model: String::new(),
            stream: false,
            termination: "marker".into(),
            max_tokens: 4096,
            source: "environment",
        };
        let base = "http://127.0.0.1:1/v1";
        let unconfigured = account("openai-compatible", "");
        let openai = account("openai-compatible", base);
        let anthropic = account("anthropic", base);
        let gemini = account("gemini", base);
        for (runtime, account, runnable) in [
            ("api", None, false),
            ("api", Some(&unconfigured), false),
            ("api", Some(&openai), true),
            ("api", Some(&anthropic), true),
            ("api", Some(&gemini), true),
            ("codex", None, false),
            ("codex", Some(&unconfigured), false),
            ("codex", Some(&openai), true),
            ("codex", Some(&anthropic), false),
            ("claude", None, false),
            ("claude", Some(&unconfigured), false),
            ("claude", Some(&anthropic), true),
            ("claude", Some(&openai), true),
            ("pi", Some(&unconfigured), true),
            ("pi", None, false),
        ] {
            assert_eq!(
                provider_blocked_reason(runtime, account).is_none(),
                runnable,
                "{runtime}"
            );
        }
    }

    #[test]
    fn dispatch_capabilities_only_advertise_steering_for_api() {
        assert_eq!(
            capabilities("api"),
            json!({"queue": true, "restart": true, "steer": true})
        );
        for runtime in [
            "codex",
            "claude",
            "hermes",
            "antigravity",
            "grok",
            "pi",
            "opencode",
            "gemini",
            "kiro",
        ] {
            assert_eq!(
                capabilities(runtime),
                json!({"queue": true, "restart": true, "steer": false})
            );
        }
    }
}
