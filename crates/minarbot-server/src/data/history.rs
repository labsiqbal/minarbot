use super::*;
use conversations::{clause, owner};

pub async fn search_sessions(
    state: &AppState,
    kind: &str,
    id: &str,
    params: &[(String, String)],
) -> Result<Response> {
    use minarbot_memory::{LocalModels, Query, Store};
    use std::sync::{Mutex, TryLockError};

    let text = param(params, "q").unwrap_or("").trim().to_owned();
    let mode = param(params, "mode").unwrap_or("keyword");
    if text.is_empty()
        || text.encode_utf16().count() > 200
        || !matches!(mode, "keyword" | "hybrid")
        || params
            .iter()
            .any(|(k, _)| !matches!(k.as_str(), "q" | "mode"))
        || params.len() > 2
        || params.len() == 2 && params[0].0 == params[1].0
    {
        return Err(error(400, "Invalid history search."));
    }
    let hybrid = mode == "hybrid";
    let path = state.config.database.clone();
    let kind = kind.to_owned();
    let id = id.to_owned();
    tokio::task::spawn_blocking(move || {
        let flags = if hybrid {
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
        } else {
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
        };
        let db = Connection::open_with_flags(&path, flags)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        owner(&db, &kind, &id)?;
        let bank = format!("{kind}:{id}");
        let query = Query {
            bank: &bank,
            text: &text,
            budget: 10,
            history_only: true,
            ..Query::default()
        };
        let hits = if hybrid {
            static MODELS: Mutex<Option<LocalModels>> = Mutex::new(None);
            let mut cached = MODELS.try_lock().map_err(|e| match e {
                TryLockError::WouldBlock => error(503, "Hybrid history search is busy."),
                TryLockError::Poisoned(_) => Failure::storage(),
            })?;
            if cached.is_none() {
                let directory = minarbot_launcher::files::sidecar(&path, ".memory-models");
                *cached = Some(LocalModels::load(&directory).map_err(|_| {
                    error(
                        503,
                        "Could not load local memory models. Keyword search is available.",
                    )
                })?);
            }
            let models = cached.as_ref().ok_or_else(Failure::storage)?;
            while Store::new(&db)
                .embed_pending(&bank, models)
                .map_err(|_| Failure::storage())?
                > 0
            {}
            let snapshot = db.unchecked_transaction()?;
            owner(&snapshot, &kind, &id)?;
            Store::new(&snapshot)
                .recall(&query, models)
                .map_err(|_| Failure::storage())?
        } else {
            let snapshot = db.unchecked_transaction()?;
            owner(&snapshot, &kind, &id)?;
            Store::new(&snapshot)
                .keyword(&query)
                .map_err(|_| Failure::storage())?
        };
        Ok(json_response(200, json!({"items":hits})))
    })
    .await
    .map_err(|_| Failure::storage())?
}

fn silence(text: &str) -> Result<bool> {
    let value =
        text.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c));
    if value.chars().count() > 64 || value.is_empty() {
        return Ok(false);
    }
    static PUNCTUATION: std::sync::OnceLock<std::result::Result<regex::Regex, regex::Error>> =
        std::sync::OnceLock::new();
    let punctuation = PUNCTUATION
        .get_or_init(|| regex::Regex::new(r"^\p{P}$"))
        .as_ref()
        .map_err(|_| Failure::storage())?;
    let trimmed =
        value.trim_matches(|c: char| c != '[' && c != ']' && punctuation.is_match(&c.to_string()));
    let markers = [
        "[SILENT]", "SILENT", "NO_REPLY", "NO REPLY", "[静默]", "静默", "[沉默]", "沉默",
    ];
    Ok([value, trimmed].iter().any(|value| {
        let normalized = value
            .split(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
            .to_uppercase();
        !normalized.is_empty() && markers.iter().any(|m| m.starts_with(&normalized))
    }))
}

fn rows(db: &Connection, table_name: &str, sql: &str, p: impl Params) -> Result<Vec<Value>> {
    if table(db, table_name)? {
        all(db, sql, p)
    } else {
        Ok(Vec::new())
    }
}
fn row(db: &Connection, table_name: &str, sql: &str, p: impl Params) -> Result<Value> {
    Ok(rows(db, table_name, sql, p)?
        .into_iter()
        .next()
        .unwrap_or(Value::Null))
}
pub fn read(
    state: &AppState,
    db: &Connection,
    kind: &str,
    id: &str,
    q: &[(String, String)],
) -> Result<Value> {
    owner(db, kind, id)?;
    let before = match param(q, "before") {
        None => 9_007_199_254_740_991,
        Some(s) => {
            let n = s
                .trim()
                .parse::<f64>()
                .map_err(|_| error(400, "Invalid history position."))?;
            if !n.is_finite() || n.fract() != 0.0 || !(1.0..=9_007_199_254_740_991.0).contains(&n) {
                return Err(error(400, "Invalid history position."));
            }
            n as i64
        }
    };
    let focus = param(q, "focus");
    let query = param(q, "q").unwrap_or("");
    if param(q, "before").is_some() && focus.is_some() {
        return Err(error(400, "Invalid history position."));
    }
    if param(q, "q").is_some()
        && (query.trim().is_empty() || query.encode_utf16().count() > 200 || focus.is_some())
    {
        return Err(error(
            400,
            "Search must contain 1 to 200 characters and cannot be combined with task focus.",
        ));
    }
    if let Some(focus) = focus {
        if !uuid(focus)
            || !exists(
                db,
                &format!("SELECT 1 FROM turns WHERE {} AND id=?", clause(kind)),
                [id, focus],
            )?
        {
            return Err(error(404, "Task not found in this conversation."));
        }
    }
    let mut turns = if kind == "channels" {
        let mut turns = if let Some(focus) = focus {
            all(db,"WITH RECURSIVE ancestors(id,parent) AS(SELECT id,parent FROM turns WHERE id=? UNION ALL SELECT t.id,t.parent FROM turns t JOIN ancestors a ON t.id=a.parent),family(id) AS(SELECT id FROM ancestors WHERE parent IS NULL UNION ALL SELECT t.id FROM turns t JOIN family f ON t.parent=f.id) SELECT turns.*,coalesce(agents.name,'Deleted agent') AS agent_name FROM turns LEFT JOIN agents ON agents.id=turns.agent WHERE turns.id IN(SELECT id FROM family) AND channel=? ORDER BY seq",[focus,id])?
        } else {
            all(db,"SELECT * FROM(SELECT turns.*,coalesce(agents.name,'Deleted agent') AS agent_name FROM turns LEFT JOIN agents ON agents.id=turns.agent WHERE channel=? AND seq<? AND(?='' OR instr(lower(text),lower(?))>0 OR instr(lower(coalesce(reply,'')),lower(?))>0) ORDER BY seq DESC LIMIT 100) ORDER BY seq",rusqlite::params![id,before,query,query,query])?
        };
        for turn in &mut turns {
            turn["history_page"] = json!(1);
        }
        if focus.is_none() && query.is_empty() && param(q, "before").is_none() {
            let active=all(db,"SELECT turns.*,coalesce(agents.name,'Deleted agent') AS agent_name FROM turns LEFT JOIN agents ON agents.id=turns.agent WHERE turns.channel=? AND(turns.status IN('queued','running') OR turns.id IN(SELECT root FROM room_turns JOIN turns t ON t.id=room_turns.turn WHERE t.channel=? AND t.status IN('queued','running'))) ORDER BY turns.seq",[id,id])?;
            for mut turn in active {
                if !turns.iter().any(|t| t["id"] == turn["id"]) {
                    turn["history_page"] = json!(0);
                    turns.push(turn);
                }
            }
            turns.sort_by_key(|t| t["seq"].as_i64().unwrap_or(0));
        }
        turns
    } else {
        all(db,&format!("SELECT * FROM(SELECT * FROM turns WHERE {} AND seq<? AND(? IS NULL OR id=?) AND(?='' OR instr(lower(text),lower(?))>0 OR instr(lower(coalesce(reply,'')),lower(?))>0) ORDER BY seq DESC LIMIT 100) ORDER BY seq",clause(kind)),rusqlite::params![id,before,focus,focus,query,query,query])?
    };
    for turn in &mut turns {
        if let Some(object) = turn.as_object_mut() {
            object.remove("profile_setup");
        }
        decorate(state, db, turn, kind == "channels")?;
    }
    Ok(json!(turns))
}
fn tool_string(v: &Value, key: &str) -> Option<Value> {
    serde_json::from_str(v[key].as_str().unwrap_or("{}")).ok()
}
fn decorate(state: &AppState, db: &Connection, turn: &mut Value, channel: bool) -> Result<()> {
    let id = string(turn, "id").to_owned();
    turn["editing"] = json!(!row(
        db,
        "queue_edits",
        "SELECT 1 FROM queue_edits WHERE turn=? AND state='held' AND expires_at>?",
        rusqlite::params![id, crate::config::now()]
    )?
    .is_null());
    turn["panelFile"]=row(db,"flow_tool_calls","SELECT f.seq,files.id,files.name,files.deleted,json_extract(f.result,'$.openedAt') AS openedAt FROM flow_tool_calls f JOIN files ON files.id=json_extract(f.result,'$.id') WHERE f.turn=? AND json_extract(f.call,'$.function.name')='open_file_in_panel' ORDER BY f.seq DESC LIMIT 1",[&id])?;
    let attachments = json(&turn["attachments"])?;
    let mut files = Vec::new();
    for file in attachments.as_array().ok_or_else(Failure::storage)? {
        if let Some(file)=one(db,"SELECT id,name,length(bytes) AS size,deleted,hex(substr(bytes,1,24)) AS header FROM files WHERE id=?",[file.as_str().unwrap_or("")])? {
            let deleted=file["deleted"]==1;
            let header=string(&file,"header");
            let mut meta=json!({"id":file["id"],"name":file["name"],"size":file["size"]});
            if deleted{meta["deleted"]=json!(true);}
            meta["image"] = json!(!deleted && super::image_header(header)); files.push(meta);
        }
    }
    turn["files"] = json!(files);
    turn["generatedFiles"] = generated(db, &id)?;
    turn["events"]=json!(rows(db,"turn_waits","SELECT w.id,w.event,w.deadline,CASE WHEN w.status='waiting' AND t.status NOT IN('queued','running') THEN 'closed' ELSE w.status END AS status FROM turn_waits w JOIN turns t ON t.id=w.turn WHERE w.turn=? ORDER BY w.rowid",[&id])?);
    let mut approvals = rows(db,"composio_calls","SELECT call_id,tool,state,decision,revision FROM composio_calls WHERE turn=? AND decision IS NOT NULL ORDER BY rowid",[&id])?;
    for approval in &mut approvals {
        let tool: Value = json(&approval["tool"])?;
        let call: Value = one(
            db,
            "SELECT call FROM composio_calls WHERE turn=? AND call_id=?",
            rusqlite::params![&id, string(approval, "call_id")],
        )?
        .ok_or_else(Failure::storage)?;
        let raw = tool_string(&call, "call")
            .and_then(|v| v["function"]["arguments"].as_str().map(str::to_owned))
            .unwrap_or_else(|| "{}".to_owned());
        approval["id"] = approval["call_id"].clone();
        approval["toolkit"] = tool["toolkit"].clone();
        approval["tool"] = tool["name"].clone();
        approval["slug"] = tool["slug"].clone();
        approval["account"] = tool["account"].clone();
        approval["arguments"] = serde_json::from_str(&raw).map_err(|_| Failure::storage())?;
        if let Some(m) = approval.as_object_mut() {
            m.remove("call_id");
        }
    }
    turn["approvals"] = json!(approvals);
    let mut harness = rows(db,"harness_approvals","SELECT id,kind,description,command,prefix,input,state,decision,revision FROM harness_approvals WHERE turn=? ORDER BY rowid",[&id])?;
    for approval in &mut harness {
        approval["input"] = if approval["kind"] == "email" {
            crate::connectors::mailbox::draft_preview(state, string(turn, "agent"), approval)?
        } else {
            json(&approval["input"])?
        };
    }
    turn["harnessApprovals"] = json!(harness);
    let mut questions = rows(
        db,
        "questions",
        "SELECT id,prompt,choices,answer FROM questions WHERE turn=? ORDER BY rowid",
        [&id],
    )?;
    for question in &mut questions {
        question["choices"] = json(&question["choices"])?;
    }
    turn["questions"] = json!(questions);
    if crate::mcp::guide::eligible(turn) {
        turn["guide"] = crate::mcp::guide::actions(db, &id, state.config.now())?;
    }
    turn["agentMessages"] = json!(message_rows(db, &id)?);
    let mut replies = Vec::new();
    for row in rows(
        db,
        "agent_reply_claims",
        "SELECT snapshot FROM agent_reply_claims WHERE consumer=? ORDER BY rowid",
        [&id],
    )? {
        let mut reply = json(&row["snapshot"])?;
        reply["truncated"] = json!(
            reply["truncated"] == true || string(&reply, "reply").encode_utf16().count() > 1000
        );
        let available = links(db, string(&reply, "id"))?;
        reply["generatedFiles"] = json!(reply["generatedFiles"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|file| available
                .as_array()
                .is_some_and(|v| v.iter().any(|a| a["id"] == file["id"])))
            .collect::<Vec<_>>());
        replies.push(reply);
    }
    turn["receivedAgentReplies"] = json!(replies);
    if !channel {
        turn["messageOrigin"] = if let Some(peer) = string(turn, "origin").strip_prefix("peer:") {
            json!({"kind":"peer","name":peer,"agent":null,"channel":null,"thread":null})
        } else {
            row(db,"agent_deliveries","SELECT a.name,t.agent,t.channel,t.thread FROM agent_deliveries d JOIN agent_requests r ON r.id=d.request JOIN turns t ON t.id=r.turn JOIN agents a ON a.id=t.agent WHERE d.turn=?",[&id])?
        };
    }
    turn["partial"] = if turn["status"] == "done" {
        Value::Null
    } else {
        partial(db, &id)?
    };
    turn["answer_model"]=row(db,"provider_calls","SELECT model FROM provider_calls WHERE turn=? AND purpose='completion' AND phase='answer' AND status='received' ORDER BY seq DESC LIMIT 1",[&id])?["model"].clone();
    turn["activity"] = activity(db, &id)?;
    turn["routineReport"] = row(
        db,
        "local_routine_reports",
        "SELECT run,name,agent,thread,status FROM local_routine_reports WHERE message=?",
        [&id],
    )?;
    let checkpoint = row(
        db,
        "workspace_checkpoints",
        "SELECT * FROM workspace_checkpoints WHERE agent=? AND turn=?",
        [string(turn, "agent"), &id],
    )?;
    turn["checkpoint"] = if checkpoint["state"] == "done" {
        let (kind, conversation) = if let Some(channel) = turn["channel"].as_str() {
            ("channels", channel)
        } else if let Some(thread) = turn["thread"].as_str() {
            ("threads", thread)
        } else {
            ("agents", string(turn, "agent"))
        };
        match panel::context(state, db, kind, conversation)
            .ok()
            .and_then(|v| v.workspace)
        {
            Some((directory, write)) if checkpoint["directory"] == directory => {
                json!({"turn":id,"restorable":write})
            }
            _ => Value::Null,
        }
    } else {
        Value::Null
    };
    turn["computerWait"] = row(db,"computer_waits","SELECT w.* FROM computer_waits w JOIN turns t ON t.id=w.turn WHERE w.turn=? AND w.state='waiting' AND t.status NOT IN('done','cancelled')",[&id])?;
    if turn["computerWait"].is_null() {
        turn.as_object_mut()
            .ok_or_else(Failure::storage)?
            .remove("computerWait");
    }
    turn["coordination"]=json!(rows(db,"coordination","SELECT coordination.kind,coordination.child,coordination.prompt,origin.name AS owner_name,target.name AS target_name FROM coordination JOIN agents origin ON origin.id=coordination.owner JOIN agents target ON target.id=coordination.target WHERE turn=? ORDER BY coordination.seq",[&id])?);
    turn["room"] = room(db, &id)?;
    Ok(())
}
pub fn generated(db: &Connection, id: &str) -> Result<Value> {
    if !table(db, "files")? {
        return Ok(json!([]));
    }
    let mut selects = Vec::new();
    for name in ["generated_files", "mcp_files", "image_generations"] {
        if table(db, name)? {
            selects.push(format!("SELECT turn,file FROM {name}"));
        }
    }
    if selects.is_empty() {
        return Ok(json!([]));
    }
    let mut rows=all(db,&format!("SELECT DISTINCT files.id,files.name,length(files.bytes) AS size,files.deleted FROM({}) outputs JOIN files ON files.id=outputs.file WHERE turn=? ORDER BY files.seq",selects.join(" UNION ALL ")),[id])?;
    for row in &mut rows {
        let deleted = row["deleted"] == 1;
        row.as_object_mut()
            .ok_or_else(Failure::storage)?
            .remove("deleted");
        if deleted {
            row["deleted"] = json!(true);
        }
    }
    Ok(json!(rows))
}

fn message_rows(db: &Connection, id: &str) -> Result<Vec<Value>> {
    let rows=rows(db,"agent_requests","SELECT r.id AS request,r.expects_reply,r.settled,t.id,t.agent,t.thread,coalesce(remote.name,a.name) AS name,remote.machine AS remote_machine,remote.agent AS remote_agent,t.status,CASE WHEN remote.id IS NOT NULL THEN 0 ELSE t.paused END AS paused,substr(t.reply,1,1000) AS reply,length(t.reply) AS reply_length,t.error,dg.id AS task_id,dg.wait AS task_wait,substr(t.text,1,1000) AS task,t.created,substr(t.reply,1,16000) AS detailReply,a.role,t.waiting_for FROM agent_requests r JOIN agent_deliveries d ON d.request=r.id JOIN turns t ON t.id=d.turn JOIN agents a ON a.id=t.agent LEFT JOIN delegation_tasks dg ON dg.delivery=t.id LEFT JOIN remote_agent_calls remote ON remote.delivery=t.id WHERE r.turn=? ORDER BY r.rowid,t.seq",[id])?;
    let mut out = Vec::new();
    for row in rows {
        let turn = string(&row, "id");
        let remote = row["remote_machine"].is_string();
        let mut result = json!({"request":row["request"],"expects_reply":row["expects_reply"],"settled":row["settled"],"id":row["id"],"agent":if remote{json!(format!("{}@{}",string(&row,"remote_agent"),string(&row,"remote_machine")))}else{row["agent"].clone()},"thread":row["thread"].as_str().unwrap_or(""),"role":row["role"],"task":row["task"],"name":row["name"],"status":row["status"],"paused":row["paused"],"error":row["error"],"reply":row["reply"],"truncated":row["reply_length"].as_i64().unwrap_or(0)>1000||string(&row,"reply").encode_utf16().count()>1000,"created":row["created"],"detailReply":row["detailReply"],"activity":activity(db,turn)?,"generatedFiles":if row["expects_reply"]==1 {links(db,turn)?}else{json!([])}});
        if remote {
            result["machine"] = row["remote_machine"].clone();
        }
        if !row["task_id"].is_null() {
            let effective = crate::turns::messaging::effective(db, turn)?;
            result["taskId"] = row["task_id"].clone();
            result["wait"] = json!(row["task_wait"] == 1);
            result["status"] = if effective["status"] == "waiting" {
                json!("waiting")
            } else if row["status"] == "interrupted" {
                json!("uncertain")
            } else if row["status"] == "queued"
                && (row["paused"] == 1
                    || !row["waiting_for"].is_null()
                    || !row_waiting(db, turn)?.is_null())
            {
                json!("waiting")
            } else {
                row["status"].clone()
            };
        }
        out.push(result);
    }
    Ok(out)
}
fn row_waiting(db: &Connection, turn: &str) -> Result<Value> {
    if !row(
        db,
        "turn_waits",
        "SELECT 1 FROM turn_waits WHERE turn=? AND status='waiting'",
        [turn],
    )?
    .is_null()
    {
        return Ok(json!(true));
    }
    row(db,"agent_requests","SELECT 1 FROM agent_requests WHERE turn=? AND expects_reply=1 AND settled=0 UNION ALL SELECT 1 FROM delegation_waits WHERE turn=? AND status='waiting' UNION ALL SELECT 1 FROM delegation_tasks WHERE creator_turn=? AND wait=1 AND wake_delivered=0",[turn,turn,turn])
}
fn links(db: &Connection, turn: &str) -> Result<Value> {
    let mut out = Vec::new();
    for mut file in requested(db, turn)?
        .as_array()
        .ok_or_else(Failure::storage)?
        .clone()
    {
        let scope = row(
            db,
            "files",
            "SELECT scope FROM files WHERE id=?",
            [string(&file, "id")],
        )?;
        if let Some(thread) = string(&scope, "scope").strip_prefix("threads:") {
            file["thread"] = json!(thread);
            out.push(file);
        }
    }
    Ok(json!(out))
}
pub fn partial(db: &Connection, id: &str) -> Result<Value> {
    let Some(call)=one(db,"SELECT c.seq,coalesce(s.text,'') AS text,c.model,c.status FROM provider_calls c LEFT JOIN provider_streams s ON s.call=c.seq WHERE c.turn=? AND c.purpose='completion' AND c.phase IN('answer','budget_summary') ORDER BY c.seq DESC LIMIT 1",[id])?else{return Ok(Value::Null);};
    let seq = call["seq"].as_i64().ok_or_else(Failure::storage)?;
    let last=one(db,"SELECT id,kind,ordinal,final FROM provider_stream_segments WHERE call=? ORDER BY ordinal DESC LIMIT 1",[seq])?.unwrap_or(Value::Null);
    let revision = one(
        db,
        "SELECT coalesce(max(revision),0) AS revision FROM provider_stream_segments WHERE call=?",
        [seq],
    )?
    .unwrap_or(Value::Null);
    let mut text = call["text"].clone();
    if exists(db, "SELECT 1 FROM room_turns WHERE turn=?", [id])? {
        if let Some(value) = text.as_str() {
            if silence(value)? {
                text = json!("");
            }
        }
    }
    Ok(
        json!({"seq":seq,"text":text,"model":call["model"],"status":call["status"],"segment":if last["kind"]=="text"{last["id"].clone()}else{json!(format!("text-{}",last["ordinal"].as_i64().map(|v|v+1).unwrap_or(0)))},"revision":revision["revision"],"final":last["final"]==1}),
    )
}
fn activity(db: &Connection, id: &str) -> Result<Value> {
    let mut v = one(
        db,
        "SELECT started_at,stopped_at,tool_budget_stopped AS budget_stopped FROM turns WHERE id=?",
        [id],
    )?
    .ok_or_else(Failure::storage)?;
    v["dispatch"]=json!(all(db,"SELECT source_turn,target_turn,mode FROM dispatch_links WHERE source_turn=? OR target_turn=? ORDER BY created",[id,id])?);
    v["calls"]=json!(all(db,"SELECT seq,model,purpose,phase,status,created,finished FROM provider_calls WHERE turn=? ORDER BY seq",[id])?);
    let mut tools=rows(db,"flow_tool_calls","SELECT call_id AS id,json_extract(call,'$.function.name') AS name,coalesce(after_action,0) AS after_action,CASE WHEN json_valid(result) THEN (coalesce(json_type(result,'$.error'),'null') <> 'null' OR coalesce(json_extract(result,'$.exitCode'),0) <> 0 OR coalesce(json_extract(result,'$.isError'),0)=1 OR coalesce(json_extract(result,'$.status'),'') IN('error','failed')) ELSE 0 END AS failed FROM flow_tool_calls WHERE turn=? ORDER BY after_action,after_output,seq",[id])?;
    if table(db, "files")? {
        tools.extend(rows(db,"generated_files","SELECT g.call_id AS id,json_extract(g.call,'$.function.name') AS name,'Created '||f.name AS title,coalesce(g.after_action,0) AS after_action,0 AS failed FROM generated_files g JOIN files f ON f.id=g.file WHERE g.turn=? ORDER BY g.seq",[id])?);
    }
    v["tools"] = json!(tools);
    v["artifacts"] = json!(rows(
        db,
        "verification_artifacts",
        "SELECT coalesce(f.id,v.source) AS id,coalesce(f.name,'Verification result') AS name,v.kind,CASE WHEN f.id IS NULL THEN NULL ELSE '/api/' || replace(f.scope,':','/') || '/files/' || f.id END AS url FROM verification_artifacts v LEFT JOIN files f ON f.id=v.file AND v.kind='screenshot' WHERE v.turn=? AND (f.id IS NULL OR f.deleted=0) ORDER BY v.rowid",
        [id]
    )?);
    v["harness"] = json!(rows(
        db,
        "harness_steps",
        "SELECT h.id,h.name,h.state,coalesce(s.failed,0) AS failed FROM harness_steps h LEFT JOIN turn_step_diagnostics s USING(turn,id) WHERE h.turn=? ORDER BY h.rowid",
        [id]
    )?);
    let mut stream=all(db,"SELECT c.seq,s.ordinal,s.id,s.kind,s.text,s.state,s.final,s.revision FROM provider_stream_segments s JOIN provider_calls c ON c.seq=s.call WHERE c.turn=? AND c.purpose='completion' AND c.phase IN('answer','budget_summary') ORDER BY c.seq,s.ordinal",[id])?;
    if exists(db, "SELECT 1 FROM room_turns WHERE turn=?", [id])? {
        let mut visible = Vec::new();
        for segment in stream {
            if segment["kind"] != "text" || !silence(string(&segment, "text"))? {
                visible.push(segment);
            }
        }
        stream = visible;
    }
    v["stream"] = json!(stream);
    v["fallback_model"] = row(
        db,
        "model_fallbacks",
        "SELECT model FROM model_fallbacks WHERE turn=?",
        [id],
    )?["model"]
        .clone();
    v["checkpoints"] = json!(rows(
        db,
        "workspace_checkpoints",
        "SELECT turn,state,created FROM workspace_checkpoints WHERE turn=?",
        [id]
    )?);
    Ok(v)
}
fn room(db: &Connection, id: &str) -> Result<Value> {
    let Some(mut r) = one(db, "SELECT rt.* FROM room_turns rt WHERE rt.turn=?", [id])? else {
        return Ok(Value::Null);
    };
    let root = string(&r, "root").to_owned();
    r["generation"] = json!(root);
    let discussion = one(
        db,
        "SELECT stopped,note FROM room_discussions WHERE root=?",
        [&root],
    )?
    .unwrap_or(Value::Null);
    r["stopped"] = json!(discussion["stopped"] == 1);
    r["note"] = discussion["note"].clone();
    r["active"]=json!(exists(db,"WITH RECURSIVE tree(id) AS(SELECT turn FROM room_turns WHERE root=? UNION SELECT turn FROM room_dispatches WHERE root=? UNION SELECT t.id FROM turns t JOIN tree ON t.parent=tree.id) SELECT 1 FROM turns t JOIN tree ON t.id=tree.id WHERE t.status IN('queued','running') OR t.paused=1",[&root,&root])?);
    r["messages"]=json!(all(db,"SELECT m.*,coalesce(a.name,'Deleted agent') AS name,a.role,target.name AS target_name FROM room_messages m LEFT JOIN agents a ON a.id=m.agent LEFT JOIN agents target ON target.id=m.target WHERE root=? ORDER BY m.seq",[&root])?);
    r["quiet"]=json!(all(db,"SELECT t.agent,coalesce(a.name,'Deleted agent') AS name,rt.silent FROM room_turns rt JOIN turns t ON t.id=rt.turn LEFT JOIN agents a ON a.id=t.agent WHERE root=? AND silent IS NOT NULL",[&root])?);
    Ok(r)
}
fn bounded(v: &str, max: usize, truncated: &mut bool) -> usize {
    let mut length = v.encode_utf16().count();
    while super::wire::slice(v, 0..length).len() > max {
        length /= 2;
        *truncated = true;
    }
    length
}
pub fn transcript(db: &Connection, kind: &str, id: &str, cutoff: i64) -> Result<String> {
    let rows=all(db,&format!("SELECT id,agent,text,coalesce(reply,(SELECT s.text FROM provider_streams s JOIN provider_calls c ON c.seq=s.call WHERE c.turn=turns.id AND s.text<>'' ORDER BY c.seq DESC LIMIT 1)) AS reply,status,created FROM turns WHERE {} AND seq<=? AND status IN('done','failed','cancelled','interrupted') ORDER BY seq DESC LIMIT 41",clause(kind)),rusqlite::params![id,cutoff])?;
    let mut truncated = rows.len() > 40;
    let mut turns = rows.into_iter().take(40).collect::<Vec<_>>();
    turns.reverse();
    for t in &mut turns {
        let guidance=all(db,"SELECT id,text,attachments FROM turns WHERE dispatch_target=? AND dispatch_mode='steer' AND status='done' ORDER BY seq",[string(t,"id")])?;
        t["guidance"] = json!(guidance).to_string().into();
    }
    let scope = format!("{kind}:{id}");
    let summary = if table(db, "compaction_sources")?
        && exists(
            db,
            "SELECT 1 FROM compaction_sources s LEFT JOIN turns t ON t.seq=s.seq WHERE s.scope=? AND(s.seq>? OR t.seq IS NULL OR t.revision<>s.revision OR t.status<>s.status) LIMIT 1",
            rusqlite::params![scope, cutoff],
        )? {
        Value::Null
    } else {
        row(
            db,
            "compactions",
            "SELECT summary FROM compactions WHERE scope=? AND through<?",
            rusqlite::params![scope, cutoff + 1],
        )?["summary"]
            .clone()
    };
    let inherited = if kind == "threads" {
        one(
            db,
            "SELECT transcript FROM context_transfers WHERE target_thread=?",
            [id],
        )?
    } else {
        None
    };
    let inherited = inherited
        .map(|v| v["transcript"].clone())
        .unwrap_or(Value::Null);
    loop {
        let mut snapshot =
            json!({"summary":summary,"inherited":inherited,"turns":turns,"truncated":truncated});
        let mut ranges = Vec::new();
        for turn in snapshot["turns"].as_array().into_iter().flatten() {
            for (key, max) in [("text", 16000), ("reply", 24000), ("guidance", 16000)] {
                if let Some(text) = turn[key].as_str() {
                    ranges.push(bounded(text, max, &mut truncated));
                }
            }
        }
        if let Some(text) = snapshot["inherited"].as_str() {
            ranges.push(bounded(text, 24000, &mut truncated));
        }
        snapshot["truncated"] = json!(truncated);
        let mut ranges = ranges.into_iter();
        let mut rules = Vec::new();
        for turn in snapshot["turns"].as_array().into_iter().flatten() {
            for key in ["text", "reply", "guidance"] {
                if turn[key].is_string() {
                    rules.push((&turn[key], 0..ranges.next().ok_or_else(Failure::storage)?));
                }
            }
        }
        if snapshot["inherited"].is_string() {
            rules.push((
                &snapshot["inherited"],
                0..ranges.next().ok_or_else(Failure::storage)?,
            ));
        }
        let encoded = super::wire::json(&snapshot, &rules);
        if encoded.len() <= 128 * 1024 || turns.len() <= 1 {
            return Ok(encoded);
        }
        turns.remove(0);
        truncated = true;
    }
}
pub fn requested(db: &Connection, turn: &str) -> Result<Value> {
    let turns=all(db,"WITH RECURSIVE results(id) AS(SELECT turns.id FROM turns JOIN threads ON threads.id=turns.thread AND threads.deleted=0 WHERE turns.id=? UNION SELECT child.id FROM results JOIN agent_requests request ON request.turn=results.id AND request.expects_reply=1 JOIN agent_deliveries delivery ON delivery.request=request.id JOIN turns child ON child.id=delivery.turn JOIN threads ON threads.id=child.thread AND threads.deleted=0) SELECT id FROM results",[turn])?;
    let mut files = Vec::new();
    for row in turns {
        files.extend(
            generated(db, string(&row, "id"))?
                .as_array()
                .ok_or_else(Failure::storage)?
                .clone(),
        );
    }
    Ok(json!(files))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_stream_hides_partial_silence_markers_only() -> Result<()> {
        for text in ["NO", "[SIL", "*NO_REP*"] {
            assert!(silence(text)?, "{text}");
        }
        assert!(!silence("No useful answer yet")?);
        Ok(())
    }
}
