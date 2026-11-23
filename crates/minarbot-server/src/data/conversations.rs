use super::*;

pub fn settings(db: &Connection, v: &Value, get: bool) -> Result<Value> {
    if !get {
        if !closed(v, &["id", "autoArchive"]) || !v["autoArchive"].is_boolean() {
            return Err(error(400, "Invalid conversation settings."));
        }
        db.execute(
            "UPDATE conversation_settings SET auto_archive=? WHERE id=1",
            [i64::from(v["autoArchive"] == true)],
        )?;
    }
    Ok(
        json!({"autoArchive":one(db,"SELECT auto_archive FROM conversation_settings WHERE id=1",[])?.is_some_and(|v|v["auto_archive"]==1)}),
    )
}
pub fn unfinished(db: &Connection, id: &str) -> Result<bool> {
    exists(db,"WITH RECURSIVE tree(id) AS(SELECT id FROM turns WHERE thread=? UNION SELECT t.id FROM turns t JOIN tree ON t.parent=tree.id) SELECT 1 FROM turns t JOIN tree ON tree.id=t.id WHERE t.status IN('queued','running') OR t.paused=1 LIMIT 1",[id])
}
pub fn threads(db: &Connection, agent: &str, v: &Value, get: bool) -> Result<Value> {
    agents::read(db, agent)?;
    if get {
        let mut rows = all(
            db,
            r#"SELECT th.*,dc.sender AS delivery_sender,dc.machine AS delivery_machine,
          coalesce(dc.activity,(SELECT max(seq) FROM turns WHERE thread=th.id),0) AS activity,
          EXISTS(SELECT 1 FROM conversation_attention p WHERE p.thread=th.id) AS needs_you
        FROM threads th LEFT JOIN delivery_conversations dc ON dc.thread=th.id
        WHERE th.agent=? AND th.deleted=0 AND NOT EXISTS(SELECT 1 FROM local_project_threads WHERE thread=th.id)
        ORDER BY activity DESC,th.rowid DESC"#,
            [agent],
        )?;
        for row in &mut rows {
            let id = string(row, "id").to_owned();
            row["delivery"] = json!(!row["delivery_sender"].is_null());
            row["active"] = json!(unfinished(db, &id)?);
            row["fork"] = fork_header(db, &id)?;
        }
        return Ok(json!(rows));
    }
    command(db, Some(agent), None, "create", v)
}
pub fn thread(db: &Connection, id: &str) -> Result<Value> {
    one(db, "SELECT * FROM threads WHERE id=? AND deleted=0", [id])?
        .ok_or_else(|| error(404, "Side thread not found."))
}
pub fn thread_info(db: &Connection, id: &str) -> Result<Value> {
    let mut row = thread(db, id)?;
    row["fork"] = fork_header(db, id)?;
    if let Some(context) = one(
        db,
        "SELECT project,folder FROM local_project_threads WHERE thread=?",
        [id],
    )? {
        for (k, v) in context.as_object().ok_or_else(Failure::storage)? {
            row[k] = v.clone();
        }
    }
    Ok(row)
}
pub fn edit_thread(db: &Connection, id: &str, op: &str, v: &Value) -> Result<Value> {
    command(db, None, Some(id), op, v)
}
fn command(
    db: &Connection,
    agent: Option<&str>,
    target: Option<&str>,
    op: &str,
    v: &Value,
) -> Result<Value> {
    let name = if op == "delete" {
        None
    } else {
        Some(field(v, "name", 80)?)
    };
    if target.is_some() && !integer(&v["revision"], 1, 9_007_199_254_740_991) {
        return Err(error(400, "Invalid conversation revision."));
    }
    if op == "edit" && !v["archived"].is_boolean() {
        return Err(error(400, "Invalid archive status."));
    }
    let path = if let Some(agent) = agent {
        format!("/api/agents/{agent}/threads")
    } else {
        format!("/api/threads/{}/{op}", target.unwrap_or(""))
    };
    let request =
        json!({"path":path,"name":name,"revision":v["revision"],"archived":v["archived"]});
    let id = string(v, "id");
    if let Some(old) = receipt(
        db,
        "thread_commands",
        id,
        &request,
        "ID was already used for a different change.",
    )? {
        return Ok(old);
    }
    let tid = target.unwrap_or(id);
    if let Some(agent) = agent {
        if exists(db, "SELECT 1 FROM threads WHERE id=?", [id])? {
            return Err(error(409, "Conversation ID was already used."));
        }
        db.execute(
            "INSERT INTO threads(id,agent,name) VALUES(?,?,?)",
            rusqlite::params![id, agent, name],
        )?;
    } else {
        let current = thread(db, tid)?;
        if current["revision"] != v["revision"] {
            return Err(error(409, "Conversation changed. Reload before saving."));
        }
        if op == "delete" && unfinished(db, tid)? {
            return Err(error(
                409,
                "Stop running or queued work before deleting this conversation.",
            ));
        }
        if op != "delete" && v["archived"] == true {
            db.execute("WITH RECURSIVE tree(id) AS(SELECT id FROM turns WHERE thread=? UNION SELECT t.id FROM turns t JOIN tree ON t.parent=tree.id) UPDATE turns SET status='cancelled',paused=0,revision=revision+1,error='Conversation archived. Earlier requests may still be billed.' WHERE id IN(SELECT id FROM tree) AND status NOT IN('done','cancelled')",[tid])?;
        }
        db.execute(
            "UPDATE threads SET name=?,archived=?,deleted=?,revision=revision+1 WHERE id=?",
            rusqlite::params![
                name.as_deref().unwrap_or(string(&current, "name")),
                i64::from(op == "delete" || v["archived"] == true),
                i64::from(op == "delete"),
                tid
            ],
        )?;
        db.execute(
            "UPDATE delivery_conversations SET settled_at=NULL,activity=? WHERE thread=?",
            rusqlite::params![crate::config::now(), tid],
        )?;
    }
    let updated =
        one(db, "SELECT * FROM threads WHERE id=?", [tid])?.ok_or_else(Failure::storage)?;
    save_receipt(db, "thread_commands", id, &request, &updated)?;
    Ok(updated)
}
pub fn channel(db: &Connection, id: &str) -> Result<Value> {
    let mut row = one(db, "SELECT * FROM channels WHERE id=?", [id])?
        .ok_or_else(|| error(404, "Channel not found."))?;
    row["members"] = json!(all(
        db,
        "SELECT agent FROM channel_members WHERE channel=? ORDER BY agent",
        [id]
    )?
    .into_iter()
    .map(|v| v["agent"].clone())
    .collect::<Vec<_>>());
    Ok(row)
}
pub fn channels(db: &Connection) -> Result<Value> {
    let rows = all(db, "SELECT c.id,EXISTS(SELECT 1 FROM conversation_attention a WHERE a.channel=c.id) AS needs_you FROM channels c ORDER BY c.rowid", [])?;
    Ok(json!(rows
        .iter()
        .map(|row| {
            let mut channel = channel(db, string(row, "id"))?;
            channel["needs_you"] = row["needs_you"].clone();
            Ok(channel)
        })
        .collect::<Result<Vec<_>>>()?))
}
pub fn save_channel(
    state: &AppState,
    db: &Connection,
    target: Option<&str>,
    v: &Value,
) -> Result<(u16, Value)> {
    let id = string(v, "id");
    let name = field(v, "name", 80)?;
    let instructions = field(v, "instructions", 8000)?;
    let mut members = v["members"]
        .as_array()
        .filter(|m| !m.is_empty())
        .ok_or_else(|| error(400, "Choose members from available agents."))?
        .clone();
    for member in &members {
        if !member.as_str().is_some_and(uuid)
            || !exists(
                db,
                "SELECT 1 FROM agents WHERE id=?",
                [member.as_str().unwrap_or("")],
            )?
        {
            return Err(error(400, "Choose members from available agents."));
        }
    }
    members.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    members.dedup();
    let lead = if v["lead"].is_null() || v["lead"] == "" {
        None
    } else {
        Some(field(v, "lead", 36)?)
    };
    if lead.as_ref().is_some_and(|s| !members.contains(&json!(s))) {
        return Err(error(400, "Lead must be a channel member."));
    }
    let cid = target.unwrap_or(id);
    let mut request =
        json!({"name":name,"instructions":instructions,"members":members,"lead":lead});
    if target.is_some() {
        if !integer(&v["revision"], 1, 9_007_199_254_740_991) || !v["archived"].is_boolean() {
            return Err(error(400, "Invalid revision or archive status."));
        }
        request["channel"] = json!(cid);
        request["revision"] = v["revision"].clone();
        request["archived"] = v["archived"].clone();
        if let Some(old) = receipt(
            db,
            "channel_edits",
            id,
            &request,
            "ID was already used for a different change.",
        )? {
            return Ok((200, old));
        }
        if exists(db,"SELECT 1 FROM channels WHERE id=? AND project IS NOT NULL AND id NOT IN(SELECT channel FROM local_project_legacy_channels)",[cid])?{return Err(error(409,"Manage project members from the project home."));}
        let current = channel(db, cid)?;
        if current["revision"] != v["revision"] {
            return Err(error(409, "Channel changed. Reload before saving."));
        }
        let active=exists(db,"WITH RECURSIVE tree(id) AS(SELECT id FROM turns WHERE channel=? UNION SELECT t.id FROM turns t JOIN tree ON t.parent=tree.id) SELECT 1 FROM turns WHERE id IN(SELECT id FROM tree) AND(status IN('queued','running') OR paused=1) LIMIT 1",[cid])?;
        if active && v["archived"] != true {
            return Err(error(
                409,
                "Wait for or cancel channel work before changing settings.",
            ));
        }
        if v["archived"] == true {
            db.execute("WITH RECURSIVE tree(id) AS(SELECT id FROM turns WHERE channel=? UNION SELECT t.id FROM turns t JOIN tree ON t.parent=tree.id) UPDATE turns SET status='cancelled',paused=0,revision=revision+1,error='Channel archived. Steps already sent to the provider may still be billed.' WHERE id IN(SELECT id FROM tree) AND(status IN('queued','running') OR paused=1)",[cid])?;
        }
        db.execute("UPDATE channels SET name=?,instructions=?,lead=?,archived=?,revision=revision+1 WHERE id=?",rusqlite::params![name,instructions,lead,i64::from(v["archived"]==true),cid])?;
        db.execute("DELETE FROM channel_members WHERE channel=?", [cid])?;
    } else {
        if exists(db, "SELECT 1 FROM channels WHERE id=?", [id])? {
            let old = one(db, "SELECT request FROM channel_creations WHERE id=?", [id])?;
            if old
                .as_ref()
                .map(|r| json(&r["request"]))
                .transpose()?
                .as_ref()
                != Some(&request)
            {
                return Err(error(409, "ID was already used for a different channel."));
            }
            return Ok((200, channel(db, id)?));
        }
        db.execute(
            "INSERT INTO channels(id,name,instructions,lead) VALUES(?,?,?,?)",
            rusqlite::params![id, name, instructions, lead],
        )?;
    }
    for member in &members {
        db.execute(
            "INSERT INTO channel_members VALUES(?,?)",
            [cid, member.as_str().unwrap_or("")],
        )?;
    }
    if target.is_some() {
        projects::sync_channel(db, cid, state.config.now())?;
    } else {
        db.execute(
            "INSERT INTO channel_creations VALUES(?,?)",
            rusqlite::params![id, request.to_string()],
        )?;
        projects::migrate(db, state.config.now())?;
    }
    let result = channel(db, cid)?;
    if target.is_some() {
        save_receipt(db, "channel_edits", id, &request, &result)?;
    }
    Ok((if target.is_some() { 200 } else { 201 }, result))
}
pub fn read_channel(db: &Connection, id: &str, v: &Value) -> Result<Value> {
    let current = channel(db, id)?;
    if !integer(&v["position"], 0, current["activity"].as_i64().unwrap_or(0)) {
        return Err(error(400, "Invalid read position."));
    }
    db.execute(
        "UPDATE channels SET read_position=max(read_position,?) WHERE id=?",
        rusqlite::params![v["position"].as_i64(), id],
    )?;
    channel(db, id)
}
pub fn dispatch(db: &Connection, kind: &str, id: &str) -> Result<Value> {
    owner(db, kind, id)?;
    let mut rows = all(
        db,
        &format!(
            "SELECT id,agent FROM turns WHERE {} AND status='running' ORDER BY seq LIMIT 100",
            clause(kind)
        ),
        [id],
    )?;
    for row in &mut rows {
        row["capabilities"] =
            agents::capabilities(string(&agents::read(db, string(row, "agent"))?, "runtime"));
    }
    Ok(json!(rows))
}

pub fn clause(kind: &str) -> &'static str {
    match kind {
        "agents" => "agent=? AND channel IS NULL AND thread IS NULL",
        "threads" => "thread=? AND channel IS NULL",
        _ => "channel=?",
    }
}
pub fn owner(db: &Connection, kind: &str, id: &str) -> Result<Value> {
    match kind {
        "agents" => agents::read(db, id),
        "threads" => thread(db, id),
        _ => channel(db, id),
    }
}
pub fn fork_header(db: &Connection, id: &str) -> Result<Value> {
    Ok(one(db,"SELECT t.source_kind,t.source_id,t.cutoff_turn,t.created,coalesce(th.name,c.name,'Main chat') AS source_name,coalesce(th.agent,a.id,t.agent) AS source_agent FROM context_transfers t LEFT JOIN threads th ON t.source_kind='thread' AND th.id=t.source_id LEFT JOIN channels c ON t.source_kind='channel' AND c.id=t.source_id LEFT JOIN agents a ON t.source_kind='agent' AND a.id=t.source_id WHERE t.target_thread=?",[id])?.unwrap_or(Value::Null))
}
pub fn fork_context(
    state: &AppState,
    db: &Connection,
    id: &str,
    h: &HeaderMap,
) -> Result<Response> {
    if h.contains_key("authorization") || h.contains_key("origin") && !origin(state, h) {
        return Err(error(
            403,
            "Fork context requires the local owner in Minarbot.",
        ));
    }
    thread(db, id)?;
    use axum::response::IntoResponse;
    let body = one(
        db,
        "SELECT transcript FROM context_transfers WHERE target_thread=?",
        [id],
    )?
    .map(|v| string(&v, "transcript").to_owned())
    .unwrap_or_else(|| "null".into());
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "application/json; charset=utf-8",
        )],
        body,
    )
        .into_response())
}
pub fn fork(
    state: &AppState,
    db: &Connection,
    kind: &str,
    id: &str,
    turn: &str,
    v: &Value,
) -> Result<Value> {
    let path = format!("/api/{kind}/{id}/turns/{turn}/fork");
    let request = json!(path);
    if let Some(old) = receipt(
        db,
        "fork_commands",
        string(v, "id"),
        &request,
        "ID was already used for a different fork.",
    )? {
        return Ok(old);
    }
    let cutoff = one(
        db,
        &format!("SELECT * FROM turns WHERE {} AND id=?", clause(kind)),
        [id, turn],
    )?
    .ok_or_else(|| error(404, "Answer not found in this conversation."))?;
    if !["done", "failed", "cancelled", "interrupted"].contains(&string(&cutoff, "status")) {
        return Err(error(409, "Fork requires a finished agent answer."));
    }
    owner(db, kind, id).map_err(|_| error(404, "Source conversation not found."))?;
    if agents::hidden(db, string(&cutoff, "agent"))? {
        return Err(error(409, "Restore the answering agent before forking."));
    }
    let transcript = super::history::transcript(
        db,
        kind,
        id,
        cutoff["seq"].as_i64().ok_or_else(Failure::storage)?,
    )?;
    let target = uuid::Uuid::new_v4().to_string();
    let created = iso(db, state.config.now())?;
    db.execute(
        "INSERT INTO threads(id,agent,name) VALUES(?,?,'New conversation')",
        [target.as_str(), string(&cutoff, "agent")],
    )?;
    db.execute(
        "INSERT INTO context_transfers VALUES(?,?,?,?,?,?,?,?,?)",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            match kind {
                "agents" => "agent",
                "threads" => "thread",
                _ => "channel",
            },
            id,
            turn,
            target,
            cutoff["agent"].as_str(),
            transcript,
            created,
            string(v, "id")
        ],
    )?;
    let mut result = thread(db, &target)?;
    result["fork"] = fork_header(db, &target)?;
    save_receipt(db, "fork_commands", string(v, "id"), &request, &result)?;
    Ok(result)
}
