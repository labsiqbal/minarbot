use super::*;
use std::collections::HashSet;

pub fn migrate(db: &Connection, now: i64) -> Result<()> {
    db.execute_batch("UPDATE channels SET project=(SELECT id FROM local_projects WHERE channel=channels.id AND deleted=0) WHERE project IS NULL AND id IN(SELECT channel FROM local_projects WHERE deleted=0); INSERT OR IGNORE INTO local_project_sections(project,channel,name,position,archived,revision) SELECT p.id,c.id,'general',0,c.archived,c.revision FROM local_projects p JOIN channels c ON c.id=p.channel WHERE p.deleted=0;")?;
    for row in all(db, "SELECT * FROM channels WHERE project IS NULL", [])? {
        let cid = string(&row, "id");
        let pid = if exists(db, "SELECT 1 FROM local_projects WHERE id=?", [cid])? {
            uuid::Uuid::new_v4().to_string()
        } else {
            cid.to_owned()
        };
        db.execute(
            "INSERT OR IGNORE INTO local_project_legacy_channels VALUES(?)",
            [cid],
        )?;
        db.execute("INSERT INTO local_projects(id,name,description,created,updated,channel,codegraph_enabled) VALUES(?,?,?,?,?,?,1)",rusqlite::params![pid,row["name"].as_str(),row["instructions"].as_str(),now,now,cid])?;
        db.execute(
            "UPDATE channels SET project=? WHERE id=?",
            [pid.as_str(), cid],
        )?;
        db.execute("INSERT INTO local_project_sections(project,channel,name,position,archived,revision) VALUES(?,?,'general',0,?,?)",rusqlite::params![pid,cid,row["archived"].as_i64(),row["revision"].as_i64()])?;
        db.execute("INSERT OR IGNORE INTO local_project_members SELECT ?,agent,0 FROM channel_members WHERE channel=?",[pid.as_str(),cid])?;
    }
    Ok(())
}
pub fn read(db: &Connection, id: &str) -> Result<Value> {
    let p = one(
        db,
        "SELECT * FROM local_projects WHERE id=? AND deleted=0",
        [id],
    )?
    .ok_or_else(|| error(404, "Project not found."))?;
    let mut sections=all(db,"SELECT s.channel AS id,s.name,s.position,s.archived,s.revision,c.activity,c.read_position,EXISTS(SELECT 1 FROM conversation_attention a WHERE a.channel=s.channel) AS needs_you FROM local_project_sections s JOIN channels c ON c.id=s.channel WHERE s.project=? ORDER BY s.position,s.channel",[id])?;
    for section in &mut sections {
        section["archived"] = json!(section["archived"] == 1);
    }
    let mut members = all(
        db,
        "SELECT agent,write FROM local_project_members WHERE project=? ORDER BY rowid",
        [id],
    )?;
    for member in &mut members {
        member["write"] = json!(member["write"] == 1);
    }
    let latest=one(db,"SELECT CAST((julianday(max(created))-2440587.5)*86400000 AS INTEGER) AS created FROM turns WHERE channel IN(SELECT channel FROM local_project_sections WHERE project=?) OR thread IN(SELECT thread FROM local_project_threads WHERE project=?)",[id,id])?.and_then(|v|v["created"].as_i64()).unwrap_or(0);
    Ok(
        json!({"id":id,"name":p["name"],"description":p["description"],"archived":p["archived"]==1,"allowInstallationApps":p["allow_installation_apps"]==1,"codegraphEnabled":true,"revision":p["revision"],"updated":p["updated"].as_i64().unwrap_or(0).max(latest),"folders":all(db,"SELECT directory FROM local_project_folders WHERE project=? ORDER BY rowid",[id])?.into_iter().map(|v|v["directory"].clone()).collect::<Vec<_>>(),"members":members,"channel":p["channel"],"sections":sections,"threads":all(db,"SELECT t.id,t.agent,t.name,p.folder,EXISTS(SELECT 1 FROM conversation_attention a WHERE a.thread=t.id) AS needs_you FROM local_project_threads p JOIN threads t ON t.id=p.thread WHERE p.project=? AND t.deleted=0 ORDER BY t.rowid DESC LIMIT 100",[id])?}),
    )
}
fn fields(v: &Value, keys: &[&str]) -> Result<()> {
    if closed(v, keys) {
        Ok(())
    } else {
        Err(error(400, "Invalid project fields."))
    }
}
fn text(v: &Value, max: usize, required: bool) -> Result<String> {
    v.as_str()
        .filter(|s| {
            s.encode_utf16().count() <= max
                && !s.chars().any(|c| c < ' ' || c == '\u{7f}')
                && (!required || !s.trim().is_empty())
        })
        .map(|s| s.trim().to_owned())
        .ok_or_else(|| error(400, "Invalid project text."))
}
fn revision(v: &Value) -> Result<()> {
    if integer(v, 1, 9_007_199_254_740_991) {
        Ok(())
    } else {
        Err(error(400, "Invalid project revision."))
    }
}
fn members(db: &Connection, v: &Value, verify: bool) -> Result<Vec<Value>> {
    let values = v
        .as_array()
        .filter(|v| v.len() <= 256)
        .ok_or_else(|| error(400, "Invalid project members."))?;
    let mut seen = HashSet::new();
    for v in values {
        fields(v, &["agent", "write"])?;
        let id = string(v, "agent");
        if !uuid(id) {
            return Err(error(400, "Invalid project or command ID."));
        }
        if !v["write"].is_boolean()
            || !seen.insert(id)
            || verify && !exists(db, "SELECT 1 FROM agents WHERE id=?", [id])?
        {
            return Err(error(
                400,
                "Choose unique local agents and an access level.",
            ));
        }
    }
    Ok(values.clone())
}
fn save(db: &Connection, id: &str, request: &Value, result: Value) -> Result<Value> {
    save_receipt(db, "local_project_commands", id, request, &result)?;
    Ok(result)
}
fn prior(db: &Connection, id: &str, request: &Value) -> Result<Option<Value>> {
    receipt(
        db,
        "local_project_commands",
        id,
        request,
        "Request ID was already used for a different project command.",
    )
}
fn set_members(db: &Connection, id: &str, members: &[Value]) -> Result<()> {
    db.execute("DELETE FROM local_project_members WHERE project=?", [id])?;
    db.execute("DELETE FROM channel_members WHERE channel IN(SELECT channel FROM local_project_sections WHERE project=?)",[id])?;
    for m in members {
        db.execute(
            "INSERT INTO local_project_members VALUES(?,?,?)",
            rusqlite::params![id, string(m, "agent"), i64::from(m["write"] == true)],
        )?;
        db.execute("INSERT INTO channel_members SELECT channel,? FROM local_project_sections WHERE project=?",[string(m,"agent"),id])?;
    }
    db.execute("UPDATE channels SET lead=?,revision=revision+1 WHERE id IN(SELECT channel FROM local_project_sections WHERE project=?)",rusqlite::params![members.first().and_then(|v|v["agent"].as_str()),id])?;
    Ok(())
}
fn create(state: &AppState, db: &Connection, v: &Value) -> Result<Value> {
    fields(
        v,
        &[
            "id",
            "name",
            "description",
            "folders",
            "members",
            "confirmWrite",
            "codegraphEnabled",
        ],
    )?;
    let id = string(v, "id");
    let name = text(&v["name"], 80, true)?;
    let description = text(v.get("description").unwrap_or(&json!("")), 2000, false)?;
    let members = members(db, v.get("members").unwrap_or(&json!([])), false)?;
    let folders = v
        .get("folders")
        .unwrap_or(&Value::Null)
        .as_array()
        .cloned()
        .unwrap_or_default();
    if v.get("folders").is_some_and(|v| !v.is_array()) || folders.len() > 32 {
        return Err(error(400, "Use up to 32 project folders."));
    }
    let mut parsed = Vec::new();
    let mut seen = HashSet::new();
    for folder in folders {
        fields(&folder, &["mode", "path"])?;
        let mode = string(&folder, "mode");
        let path = match mode {
            "new" => {
                let mut slug = String::new();
                for c in name.chars() {
                    if c.is_ascii_alphanumeric() {
                        slug.push(c.to_ascii_lowercase());
                    } else if !slug.ends_with('-') {
                        slug.push('-');
                    }
                }
                let slug = slug.trim_matches('-');
                let slug = if slug.is_empty() {
                    "project"
                } else {
                    &slug[..slug.len().min(64)]
                };
                folders::root(state)?.join(slug)
            }
            "existing" => folders::validate(state, &folder["path"], false, false)?,
            _ => return Err(error(400, "Choose a new or existing folder.")),
        };
        folders::validate(state, &json!(path), false, false)?;
        if !seen.insert(path.clone()) {
            return Err(error(400, "Project folders must be unique."));
        }
        parsed.push(json!({"mode":mode,"path":path}));
    }
    if members.iter().any(|v| v["write"] == true) && v["confirmWrite"] != true {
        return Err(error(400, "Confirm before granting write access."));
    }
    let request = json!({"action":"create","name":name,"description":description,"folders":parsed,"members":members,"confirmWrite":v["confirmWrite"]==true});
    if let Some(old) = prior(db, id, &request)? {
        return Ok(old);
    }
    self::members(db, &json!(members), true)?;
    for folder in &parsed {
        if folder["mode"] == "existing" {
            folders::validate(state, &folder["path"], false, true)?;
        }
    }
    let new = parsed
        .iter()
        .filter(|v| v["mode"] == "new")
        .collect::<Vec<_>>();
    if !new.is_empty() {
        transaction(db, || {
            let old = one(
                db,
                "SELECT request FROM local_project_folder_intents WHERE command=?",
                [id],
            )?;
            if let Some(old) = old {
                if json(&old["request"])? != request {
                    return Err(error(
                        409,
                        "Request ID was already used for different folder creation.",
                    ));
                }
            } else {
                if new
                    .iter()
                    .any(|v| std::fs::symlink_metadata(string(v, "path")).is_ok())
                {
                    return Err(error(409,"New folder already exists. Choose Use an existing folder or a different project name."));
                }
                db.execute(
                    "INSERT INTO local_project_folder_intents VALUES(?,?,?)",
                    rusqlite::params![
                        id,
                        request.to_string(),
                        json!(new.iter().map(|v| v["path"].clone()).collect::<Vec<_>>())
                            .to_string()
                    ],
                )?;
            }
            Ok(())
        })?;
    }
    transaction(db, || {
        for folder in new {
            folders::create(state, std::path::Path::new(string(folder, "path")))?;
        }
        let channel = uuid::Uuid::new_v4().to_string();
        let now = state.config.now();
        db.execute("INSERT INTO local_projects(id,name,description,created,updated,channel,codegraph_enabled) VALUES(?,?,?,?,?,?,1)",rusqlite::params![id,name,description,now,now,channel])?;
        db.execute(
            "INSERT INTO channels(id,name,instructions,lead,project) VALUES(?,'general',?,NULL,?)",
            [channel.as_str(), description.as_str(), id],
        )?;
        db.execute(
            "INSERT INTO local_project_sections VALUES(?,?,'general',0,0,1)",
            [id, channel.as_str()],
        )?;
        for folder in &parsed {
            db.execute(
                "INSERT INTO local_project_folders VALUES(?,?)",
                [id, string(folder, "path")],
            )?;
        }
        set_members(db, id, &members)?;
        save(db, id, &request, read(db, id)?)
    })
}
fn edit(state: &AppState, db: &Connection, pid: &str, op: &str, v: &Value) -> Result<Value> {
    fields(
        v,
        match op {
            "archive" | "restore" | "delete" => &["id", "revision", "confirmed"],
            "thread" => &["id", "revision", "name", "agent", "folder"],
            _ => &[
                "id",
                "revision",
                "name",
                "description",
                "members",
                "confirmWrite",
                "folders",
                "allowInstallationApps",
                "codegraphEnabled",
                "confirmed",
            ],
        },
    )?;
    if v.get("codegraphEnabled")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(error(400, "Invalid CodeGraph setting."));
    }
    revision(&v["revision"])?;
    let id = string(v, "id");
    let mut request = v.clone();
    if let Some(fields) = request.as_object_mut() {
        fields.shift_remove("codegraphEnabled");
    }
    request["projectId"] = json!(pid);
    request["action"] = json!(op);
    if let Some(old) = prior(db, id, &request)? {
        return Ok(old);
    }
    transaction(db, || {
        let current = read(db, pid)?;
        if current["archived"] == true && !matches!(op, "delete" | "restore") {
            return Err(error(409, "Project is archived."));
        }
        if current["revision"] != v["revision"] {
            return Err(error(409, "Project changed. Reload before saving."));
        }
        if active(db, pid)? {
            return Err(error(
                409,
                "Stop or finish project work before changing access.",
            ));
        }
        let mut archive_state = None;
        match op {
            "delete" | "archive" => {
                if v["confirmed"] != true {
                    return Err(error(
                        400,
                        if op == "archive" {
                            "Confirm project archive."
                        } else {
                            "Confirm project deletion."
                        },
                    ));
                }
                if op == "delete" {
                    db.execute("DELETE FROM project_environment WHERE project=?", [pid])?;
                    db.execute(
                        "DELETE FROM project_environment_settings WHERE project=?",
                        [pid],
                    )?;
                    db.execute("DELETE FROM mcp_tests WHERE id IN(SELECT test FROM mcp_test_projects WHERE project=?)", [pid])?;
                    db.execute("DELETE FROM mcp_test_projects WHERE project=?", [pid])?;
                    db.execute("DELETE FROM mcp_grants WHERE connection IN(SELECT connection FROM mcp_projects WHERE project=?)", [pid])?;
                    let connections = all(
                        db,
                        "SELECT connection FROM mcp_projects WHERE project=?",
                        [pid],
                    )?;
                    db.execute("DELETE FROM mcp_projects WHERE project=?", [pid])?;
                    for row in connections {
                        db.execute(
                            "DELETE FROM mcp_connections WHERE id=?",
                            [string(&row, "connection")],
                        )?;
                    }
                    db.execute("UPDATE local_projects SET deleted=1 WHERE id=?", [pid])?;
                    db.execute("DELETE FROM local_project_members WHERE project=?", [pid])?;
                    db.execute("DELETE FROM channel_members WHERE channel IN(SELECT channel FROM local_project_sections WHERE project=?)",[pid])?;
                    db.execute("UPDATE channels SET lead=NULL WHERE project=?", [pid])?;
                    db.execute("DELETE FROM local_project_folders WHERE project=?", [pid])?;
                } else {
                    archive_state = Some(json!({
                        "sections": all(db, "SELECT channel AS id FROM local_project_sections WHERE project=? AND archived=0", [pid])?,
                        "threads": all(db, "SELECT t.id FROM local_project_threads p JOIN threads t ON t.id=p.thread WHERE p.project=? AND t.archived=0 AND t.deleted=0", [pid])?,
                    }));
                    db.execute("UPDATE local_projects SET archived=1 WHERE id=?", [pid])?;
                }
                db.execute(
                    "UPDATE channels SET archived=1,revision=revision+1 WHERE project=?",
                    [pid],
                )?;
                db.execute("UPDATE local_project_sections SET archived=1,revision=revision+1 WHERE project=?",[pid])?;
                db.execute("UPDATE threads SET archived=1 WHERE id IN(SELECT thread FROM local_project_threads WHERE project=?)",[pid])?;
            }
            "restore" => {
                if v["confirmed"] != true || current["archived"] != true {
                    return Err(error(409, "Confirm restoration of an archived project."));
                }
                // Archives made before the snapshot leave their sections and threads
                // archived because their original status is unknown.
                let snapshot = one(db, "SELECT result FROM local_project_commands
                    WHERE json_extract(request,'$.projectId')=? AND json_extract(request,'$.action')='archive'
                    ORDER BY rowid DESC LIMIT 1", [pid])?
                    .map(|row| json(&row["result"])).transpose()?.unwrap_or(Value::Null);
                let archived = &snapshot["archiveState"];
                for section in archived["sections"].as_array().into_iter().flatten() {
                    let id = string(section, "id");
                    db.execute("UPDATE local_project_sections SET archived=0,revision=revision+1 WHERE project=? AND channel=?", [pid,id])?;
                    db.execute("UPDATE channels SET archived=0,revision=revision+1 WHERE project=? AND id=?", [pid,id])?;
                }
                for thread in archived["threads"].as_array().into_iter().flatten() {
                    db.execute("UPDATE threads SET archived=0,revision=revision+1 WHERE id IN
                        (SELECT thread FROM local_project_threads WHERE project=? AND thread=?) AND deleted=0", [pid,string(thread, "id")])?;
                }
                db.execute("UPDATE local_projects SET archived=0 WHERE id=?", [pid])?;
            }
            "thread" => {
                let agent = string(v, "agent");
                if !uuid(agent) {
                    return Err(error(400, "Invalid project or command ID."));
                }
                let fs = current["folders"].as_array().ok_or_else(Failure::storage)?;
                let folder = if let Some(first) = fs.first() {
                    folders::validate(state, v.get("folder").unwrap_or(first), false, true)?
                        .to_string_lossy()
                        .into_owned()
                } else {
                    String::new()
                };
                if !current["members"]
                    .as_array()
                    .is_some_and(|m| m.iter().any(|m| m["agent"] == agent))
                    || (!folder.is_empty() && !fs.contains(&json!(folder)))
                    || (folder.is_empty() && v.get("folder").is_some_and(|f| f != ""))
                {
                    return Err(error(
                        400,
                        "Choose a project member and valid project folder.",
                    ));
                }
                let thread = uuid::Uuid::new_v4().to_string();
                let name = text(v.get("name").unwrap_or(&json!("Project thread")), 80, true)?;
                db.execute(
                    "INSERT INTO threads(id,agent,name) VALUES(?,?,?)",
                    [thread.as_str(), agent, name.as_str()],
                )?;
                db.execute(
                    "INSERT INTO local_project_threads VALUES(?,?,?)",
                    [thread.as_str(), pid, folder.as_str()],
                )?;
            }
            "edit" => {
                if let Some(allow) = v.get("allowInstallationApps") {
                    if !allow.is_boolean() || allow == true && v["confirmed"] != true {
                        return Err(error(400, "Confirm allowing installation apps."));
                    }
                    db.execute(
                        "UPDATE local_projects SET allow_installation_apps=? WHERE id=?",
                        rusqlite::params![i64::from(allow == true), pid],
                    )?;
                }
                let members = members(db, v.get("members").unwrap_or(&current["members"]), true)?;
                let previous = current["members"].as_array().ok_or_else(Failure::storage)?;
                if members.iter().any(|m| {
                    m["write"] == true
                        && !previous
                            .iter()
                            .any(|old| old["agent"] == m["agent"] && old["write"] == true)
                }) && v["confirmWrite"] != true
                {
                    return Err(error(400, "Confirm before granting write access."));
                }
                let name = text(v.get("name").unwrap_or(&current["name"]), 80, true)?;
                let description = text(
                    v.get("description").unwrap_or(&current["description"]),
                    2000,
                    false,
                )?;
                db.execute(
                    "UPDATE local_projects SET name=?,description=? WHERE id=?",
                    [name.as_str(), description.as_str(), pid],
                )?;
                if let Some(folders) = v.get("folders") {
                    let folders = folders
                        .as_array()
                        .filter(|f| f.len() <= 32)
                        .ok_or_else(|| error(400, "Use up to 32 project folders."))?;
                    let mut seen = HashSet::new();
                    for folder in folders {
                        let path = folders::validate(state, folder, false, true)?;
                        if !seen.insert(path) {
                            return Err(error(400, "Project folders must be unique."));
                        }
                    }
                    if current["threads"].as_array().is_some_and(|threads| {
                        threads
                            .iter()
                            .any(|th| th["folder"] != "" && !folders.contains(&th["folder"]))
                    }) {
                        return Err(error(409, "A side thread still uses this folder."));
                    }
                    if folders.iter().any(|f| {
                        !current["folders"]
                            .as_array()
                            .is_some_and(|fs| fs.contains(f))
                    }) && members.iter().any(|m| m["write"] == true)
                        && v["confirmWrite"] != true
                    {
                        return Err(error(
                            400,
                            "Confirm before granting write access to added folders.",
                        ));
                    }
                    db.execute("DELETE FROM local_project_folders WHERE project=?", [pid])?;
                    for f in folders {
                        db.execute(
                            "INSERT INTO local_project_folders VALUES(?,?)",
                            [pid, f.as_str().unwrap_or("")],
                        )?;
                    }
                }
                if v.get("description").is_some() {
                    db.execute(
                        "UPDATE channels SET instructions=? WHERE project=?",
                        [description.as_str(), pid],
                    )?;
                }
                set_members(db, pid, &members)?;
            }
            _ => return Err(error(400, "Unknown project command.")),
        }
        db.execute(
            "UPDATE local_projects SET revision=revision+1,updated=? WHERE id=?",
            rusqlite::params![state.config.now(), pid],
        )?;
        let mut result = if op == "delete" {
            json!({"id":pid,"deleted":true})
        } else {
            read(db, pid)?
        };
        if let Some(snapshot) = archive_state {
            result["archiveState"] = snapshot;
        }
        save(db, id, &request, result)
    })
}
fn active(db: &Connection, pid: &str) -> Result<bool> {
    exists(db,"SELECT 1 FROM turns WHERE(status IN('running','queued') OR paused=1) AND(channel IN(SELECT channel FROM local_project_sections WHERE project=?) OR thread IN(SELECT thread FROM local_project_threads WHERE project=?)) LIMIT 1",[pid,pid])
}
fn section(
    state: &AppState,
    db: &Connection,
    pid: &str,
    target: Option<&str>,
    v: &Value,
) -> Result<Value> {
    fields(
        v,
        if target.is_some() {
            &["id", "revision", "name", "archived", "position"]
        } else {
            &["id", "revision", "name"]
        },
    )?;
    let id = string(v, "id");
    let mut request = v.clone();
    request["projectId"] = json!(pid);
    request["action"] = json!(if target.is_some() { "edit" } else { "create" });
    if let Some(t) = target {
        request["sectionId"] = json!(t);
    }
    if let Some(old) = prior(db, id, &request)? {
        return Ok(old);
    }
    transaction(db, || {
        let current = read(db, pid)?;
        if current["archived"] == true {
            return Err(error(409, "Project is archived."));
        }
        revision(&v["revision"])?;
        if current["revision"] != v["revision"] {
            return Err(error(409, "Project changed. Reload before saving."));
        }
        let sections = current["sections"]
            .as_array()
            .ok_or_else(Failure::storage)?;
        if let Some(target) = target {
            if !uuid(target) {
                return Err(error(400, "Invalid section ID."));
            }
            let old = sections
                .iter()
                .find(|s| s["id"] == target)
                .ok_or_else(|| error(404, "Section not found."))?;
            if v.get("archived").is_some_and(|v| !v.is_boolean()) {
                return Err(error(400, "Invalid section archive status."));
            }
            if exists(db,"SELECT 1 FROM turns WHERE channel=? AND(status IN('running','queued') OR paused=1) LIMIT 1",[target])?{return Err(error(409,"Stop or finish section work before changing it."));}
            let name = text(v.get("name").unwrap_or(&old["name"]), 80, true)?;
            if sections.iter().any(|s| {
                s["id"] != target && string(s, "name").to_lowercase() == name.to_lowercase()
            }) {
                return Err(error(409, "Section name already exists."));
            }
            let position = v.get("position").unwrap_or(&old["position"]);
            if !integer(position, 0, sections.len() as i64 - 1) {
                return Err(error(400, "Invalid section position."));
            }
            let pos = position.as_u64().ok_or_else(Failure::storage)? as usize;
            let archived = v.get("archived").unwrap_or(&old["archived"]) == true;
            let mut ordered = sections
                .iter()
                .filter(|s| s["id"] != target)
                .map(|s| string(s, "id"))
                .collect::<Vec<_>>();
            ordered.insert(pos, target);
            db.execute(
                "UPDATE channels SET name=?,archived=?,revision=revision+1 WHERE id=?",
                rusqlite::params![name, i64::from(archived), target],
            )?;
            db.execute("UPDATE local_project_sections SET name=?,archived=?,position=?,revision=revision+1 WHERE channel=?",rusqlite::params![name,i64::from(archived),pos as i64,target])?;
            for (i, id) in ordered.iter().enumerate() {
                if *id != target {
                    db.execute("UPDATE local_project_sections SET position=?,revision=revision+1 WHERE channel=? AND position<>?",rusqlite::params![i as i64,id,i as i64])?;
                }
            }
        } else {
            if sections.len() >= 100 {
                return Err(error(400, "Project limit is 100 sections."));
            }
            let name = text(&v["name"], 80, true)?;
            if sections
                .iter()
                .any(|s| string(s, "name").to_lowercase() == name.to_lowercase())
            {
                return Err(error(409, "Section name already exists."));
            }
            let channel = uuid::Uuid::new_v4().to_string();
            db.execute(
                "INSERT INTO channels(id,name,instructions,lead,project) VALUES(?,?,?,NULL,?)",
                [
                    channel.as_str(),
                    name.as_str(),
                    string(&current, "description"),
                    pid,
                ],
            )?;
            db.execute(
                "INSERT INTO local_project_sections VALUES(?,?,?,?,0,1)",
                rusqlite::params![pid, channel, name, sections.len() as i64],
            )?;
            db.execute("INSERT INTO channel_members SELECT ?,agent FROM local_project_members WHERE project=?",[channel.as_str(),pid])?;
            db.execute("UPDATE channels SET lead=(SELECT agent FROM local_project_members WHERE project=? ORDER BY rowid LIMIT 1) WHERE id=?",[pid,channel.as_str()])?;
        }
        db.execute(
            "UPDATE local_projects SET revision=revision+1,updated=? WHERE id=?",
            rusqlite::params![state.config.now(), pid],
        )?;
        save(db, id, &request, read(db, pid)?)
    })
}
pub fn sync_channel(db: &Connection, cid: &str, now: i64) -> Result<()> {
    let row=one(db,"SELECT c.* FROM channels c JOIN local_project_legacy_channels l ON l.channel=c.id WHERE c.id=?",[cid])?.ok_or_else(||error(409,"Manage project members and sections from the project home."))?;
    let pid = string(&row, "project");
    let current = read(db, pid)?;
    if current["folders"].as_array().is_none_or(|f| !f.is_empty())
        || current["sections"].as_array().is_none_or(|s| s.len() != 1)
    {
        return Err(error(
            409,
            "Manage project members and sections from the project home.",
        ));
    }
    db.execute(
        "UPDATE local_projects SET name=?,description=?,revision=revision+1,updated=? WHERE id=?",
        rusqlite::params![row["name"].as_str(), row["instructions"].as_str(), now, pid],
    )?;
    db.execute(
        "UPDATE local_project_sections SET archived=?,revision=revision+1 WHERE channel=?",
        rusqlite::params![row["archived"].as_i64(), cid],
    )?;
    db.execute("DELETE FROM local_project_members WHERE project=?", [pid])?;
    db.execute(
        "INSERT INTO local_project_members SELECT ?,agent,0 FROM channel_members WHERE channel=?",
        [pid, cid],
    )?;
    Ok(())
}
fn read_with_index(state: &AppState, db: &Connection, id: &str) -> Result<Value> {
    let mut project = read(db, id)?;
    project["codegraphIndex"] = crate::mcp::codegraph::project_status(state, &project);
    Ok(project)
}

pub fn route(
    state: &AppState,
    db: &Connection,
    p: &[&str],
    v: &Value,
    q: &[(String, String)],
    h: &HeaderMap,
    get: bool,
) -> Result<Value> {
    if h.contains_key("authorization") || h.contains_key("origin") && !origin(state, h) {
        return Err(error(403, "Projects require the local Owner in Minarbot."));
    }
    if p == ["api", "project-folders"] {
        if !get || q.iter().any(|(k, _)| k != "path") {
            return Err(error(400, "Invalid folder browser parameters."));
        }
        return folders::browse(state, param(q, "path"));
    }
    let archived = get
        && p == ["api", "projects"]
        && matches!(q, [(key, value)] if key == "archived" && value == "1");
    if !q.is_empty() && !archived {
        return Err(error(
            if get { 400 } else { 403 },
            if get {
                "Invalid project parameters."
            } else {
                "Projects require the local Owner in Minarbot."
            },
        ));
    }
    match p {
        ["api", "projects"] if get => {
            let rows=all(db,"SELECT id FROM local_projects WHERE deleted=0 AND archived=? ORDER BY updated DESC,rowid DESC",[i64::from(archived)])?;
            let projects = rows
                .iter()
                .map(|v| read_with_index(state, db, string(v, "id")))
                .collect::<Result<Vec<_>>>()?;
            Ok(
                json!({"projects":projects,"root":folders::root(state)?,"codegraphAvailable":crate::mcp::codegraph::available()}),
            )
        }
        ["api", "projects"] => create(state, db, v),
        ["api", "projects", id] if get => read_with_index(state, db, id),
        ["api", "projects", id, op]
            if ["edit", "archive", "restore", "delete", "thread"].contains(op) && !get =>
        {
            let mut result = edit(state, db, id, op, v)?;
            if let Some(result) = result.as_object_mut() {
                result.remove("archiveState");
            }
            Ok(result)
        }
        ["api", "projects", id, "sections"] if get => Ok(read(db, id)?["sections"].clone()),
        ["api", "projects", id, "sections"] => section(state, db, id, None, v),
        ["api", "projects", id, "sections", sid, "edit"] if get => read(db, id)?["sections"]
            .as_array()
            .and_then(|s| s.iter().find(|s| s["id"] == *sid))
            .cloned()
            .ok_or_else(|| error(404, "Section not found.")),
        ["api", "projects", id, "sections", sid, "edit"] => section(state, db, id, Some(sid), v),
        _ => Err(error(400, "Invalid project parameters.")),
    }
}
