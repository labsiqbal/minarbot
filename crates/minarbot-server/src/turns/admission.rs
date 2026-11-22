use super::*;

pub async fn prepare(
    state: &AppState,
    parts: &[&str],
    method: &Method,
    value: &Value,
) -> Result<()> {
    if method != Method::POST
        || !matches!(
            parts,
            ["api", "agents" | "threads" | "channels", _, "turns"]
                | [
                    "api",
                    "agents" | "threads" | "channels",
                    _,
                    "turns",
                    _,
                    "edit"
                ]
        )
    {
        return Ok(());
    }
    let files = files(value)?;
    let documents = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        if exists(db, "SELECT 1 FROM turns WHERE id=?", [string(value, "id")])? {
            return Ok(());
        }
        conversation(db, parts[1], parts[2], value)?;
        validate_files(db, &files, &format!("{}:{}", parts[1], parts[2]))?;
        files
            .as_array()
            .ok_or_else(Failure::storage)?
            .iter()
            .map(|id| {
                crate::files::attachments::file(
                    db,
                    id.as_str().unwrap_or(""),
                    &format!("{}:{}", parts[1], parts[2]),
                )
            })
            .collect::<Result<Vec<_>>>()?
    };
    let mut size = 0;
    for file in documents {
        if file.bytes.starts_with(b"%PDF-") {
            let response = crate::files::preview::pdf(state, &file.meta, &file.bytes).await?;
            let bytes = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
                .await
                .map_err(|_| Failure::storage())?;
            let value: Value = serde_json::from_slice(&bytes).map_err(|_| Failure::storage())?;
            size += string(&value, "text").len();
        } else if crate::files::attachments::image_type(&file.bytes).is_none()
            && super::transcription::audio(&file.bytes).is_none()
        {
            size += file.bytes.len();
        }
    }
    if size > 32 * 1024 {
        return Err(error(
            413,
            "Total text attachment limit is 32 KiB per message.",
        ));
    }
    Ok(())
}

pub fn scoped(db: &Connection, kind: &str, owner: &str, id: &str, message: &str) -> Result<Value> {
    let row =
        one(db, "SELECT * FROM turns WHERE id=?", [id])?.ok_or_else(|| error(404, message))?;
    let valid = match kind {
        "channels" => row["channel"] == owner,
        "threads" => row["thread"] == owner,
        _ => row["agent"] == owner && row["thread"].is_null() && row["channel"].is_null(),
    };
    if !valid {
        return Err(error(404, message));
    }
    Ok(row)
}
pub(super) fn files(v: &Value) -> Result<Value> {
    let files = v.get("files").cloned().unwrap_or_else(|| json!([]));
    let mut seen = std::collections::HashSet::new();
    if !files.as_array().is_some_and(|a| {
        a.len() <= 4
            && a.iter()
                .all(|v| v.as_str().is_some_and(|s| uuid(s) && seen.insert(s)))
    }) {
        return Err(error(400, "Choose up to four different files."));
    }
    Ok(files)
}
pub fn validate_files(db: &Connection, files: &Value, scope: &str) -> Result<()> {
    let (mut text_bytes, mut image_bytes, mut audio_bytes) = (0, 0, 0);
    for f in files.as_array().ok_or_else(Failure::storage)? {
        let id = f.as_str().ok_or_else(Failure::storage)?;
        let file = crate::files::attachments::file(db, id, scope)?;
        if crate::files::attachments::image_type(&file.bytes).is_some() {
            image_bytes += file.bytes.len();
        } else if super::transcription::audio(&file.bytes).is_some() {
            audio_bytes += file.bytes.len();
        } else if file.bytes.starts_with(b"%PDF-") {
            if file.bytes.len() > 10 * 1024 * 1024 {
                return Err(error(413, "PDF attachment limit is 10 MiB."));
            }
        } else {
            std::str::from_utf8(&file.bytes)
                .ok()
                .filter(|text| {
                    !text
                        .chars()
                        .any(|c| c < ' ' && !['\n', '\r', '\t'].contains(&c))
                })
                .ok_or_else(|| error(415, "Message attachment must be UTF-8 text."))?;
            text_bytes += file.bytes.len();
        }
    }
    if image_bytes > 4 * 1024 * 1024 {
        return Err(error(
            413,
            "Total image attachment limit is 4 MiB per message.",
        ));
    }
    if audio_bytes > 10 * 1024 * 1024 {
        return Err(error(
            413,
            "Total audio attachment limit is 10 MiB per message.",
        ));
    }
    if text_bytes > 32 * 1024 {
        return Err(error(
            413,
            "Total text attachment limit is 32 KiB per message.",
        ));
    }
    Ok(())
}
pub(crate) fn conversation(
    db: &Connection,
    kind: &str,
    owner: &str,
    v: &Value,
) -> Result<(String, Option<String>, Option<String>)> {
    match kind {
        "agents" => {
            data::agents::read(db, owner)?;
            Ok((owner.to_owned(), None, None))
        }
        "threads" => {
            let t = one(
                db,
                "SELECT * FROM threads WHERE id=? AND deleted=0",
                [owner],
            )?
            .ok_or_else(|| error(404, "Side thread not found."))?;
            if t["archived"] == 1 {
                return Err(error(409, "Restore the side thread before sending."));
            }
            Ok((string(&t, "agent").to_owned(), Some(owner.to_owned()), None))
        }
        _ => {
            let c = one(db, "SELECT * FROM channels WHERE id=?", [owner])?
                .ok_or_else(|| error(404, "Channel not found."))?;
            if c["archived"] == 1 {
                return Err(error(
                    409,
                    "Channel is archived. Restore the channel before sending new messages.",
                ));
            }
            let members = all(
                db,
                "SELECT m.agent,coalesce(v.hidden,0) AS hidden FROM channel_members m LEFT JOIN agent_visibility v ON v.agent=m.agent WHERE m.channel=? ORDER BY m.agent",
                [owner],
            )?;
            let recipient = v["recipient"].as_str().filter(|s| !s.is_empty());
            if v.get("recipient")
                .is_some_and(|s| !s.is_null() && !s.is_string())
            {
                return Err(error(400, "Invalid room recipient."));
            }
            if recipient.is_some_and(|r| !members.iter().any(|m| m["agent"] == r)) {
                return Err(error(400, "Recipient is not a channel member."));
            }
            let lead = c["lead"].as_str().filter(|s| {
                members
                    .iter()
                    .any(|m| m["agent"] == *s && (v["automatic"] == true || m["hidden"] != 1))
            });
            let agent = recipient
                .or(lead)
                .or_else(|| {
                    (v["automatic"] != true)
                        .then(|| {
                            members
                                .iter()
                                .find(|m| m["hidden"] != 1)
                                .and_then(|m| m["agent"].as_str())
                        })
                        .flatten()
                })
                .ok_or_else(|| error(409, "This channel has no available members."))?;
            Ok((agent.to_owned(), None, Some(owner.to_owned())))
        }
    }
}
pub fn ready(state: &AppState, db: &Connection, agent: &str) -> Result<()> {
    let profile = data::agents::read(db, agent)?;
    if agent == crate::mcp::guide::ID && string(&profile, "model").is_empty() {
        return Err(error(
            503,
            "Choose an AI connection for the built-in guide.",
        ));
    }
    let runtime = string(&profile, "runtime");
    if runtime == "api" {
        account(state, db, agent)?;
        return Ok(());
    }
    if crate::harness::executable(state, runtime).is_none() {
        return Err(error(
            503,
            "Coding runtime requires an installed executable on PATH.",
        ));
    }
    let accounts = data::providers::accounts(state)?;
    if let Some(reason) = data::agents::binding_blocked_reason(
        db,
        runtime,
        string(&profile, "provider_account"),
        &accounts,
    )? {
        return Err(error(503, reason));
    }
    Ok(())
}
pub fn account(state: &AppState, db: &Connection, agent: &str) -> Result<data::providers::Account> {
    let a = data::agents::read(db, agent)?;
    let id = string(&a, "provider_account");
    let a=data::providers::accounts(state)?.into_iter().find(|a|a.id==id).ok_or_else(||error(503,"The selected provider account is missing. Choose another account or update server configuration."))?;
    if a.base.is_empty() {
        return Err(error(
            503,
            if id.is_empty() {
                "The default provider account is not configured. Check server configuration or choose a configured account."
            } else {
                "The selected provider account is not configured. Check server configuration or choose another account."
            },
        ));
    }
    Ok(a)
}
pub fn route(
    state: &AppState,
    db: &Connection,
    path: &str,
    p: &[&str],
    method: &Method,
    q: &[(String, String)],
    v: &Value,
) -> Result<(u16, Value)> {
    match p {
        ["api", kind, owner, "compaction"] if *method == Method::GET => {
            let scope = if *kind == "channels" {
                format!("channels:{owner}")
            } else if *kind == "threads" {
                format!("threads:{owner}")
            } else {
                format!("agents:{owner}")
            };
            let pos = q
                .iter()
                .find(|(k, _)| k == "before")
                .and_then(|(_, v)| v.parse().ok())
                .unwrap_or(i64::MAX);
            let snap = super::compaction::read(db, &scope, pos)?;
            if let Some(snap) = snap {
                let after = q
                    .iter()
                    .find(|(k, _)| k == "after")
                    .and_then(|(_, v)| v.parse().ok())
                    .unwrap_or(0);
                let sources=all(db,"SELECT t.id,s.seq,s.status FROM compaction_sources s JOIN turns t ON t.seq=s.seq WHERE s.scope=? AND s.seq>? ORDER BY s.seq LIMIT 21",params![scope,after])?;
                let next = if sources.len() > 20 {
                    sources.get(19).map(|r| r["seq"].clone())
                } else {
                    None
                };
                return Ok((
                    200,
                    json!({"version":snap["version"],"through":snap["through"],"summary":snap["summary"],"sources":sources.into_iter().take(20).collect::<Vec<_>>(),"next_after":next,"source_count":one(db,"SELECT count(*) AS n FROM compaction_sources WHERE scope=?",[&scope])?.map(|r|r["n"].clone()).unwrap_or(json!(0))}),
                ));
            }
            Ok((200, Value::Null))
        }
        ["api", kind, owner, "turns", turn, "harness-approvals", approval]
            if *method == Method::POST =>
        {
            scoped(
                db,
                kind,
                owner,
                turn,
                "Approval not found in this conversation.",
            )?;
            Ok((200, shell::decide(db, turn, approval, v)?))
        }
        ["api", kind, owner, "turns"] if *method == Method::POST => {
            create(state, db, path, kind, owner, v)
        }
        ["api", kind, owner, "turns", turn, "edit"] if *method == Method::POST => {
            Ok((200, queue::edit(state, db, path, kind, owner, turn, v)?))
        }
        ["api", kind, owner, "turns", turn, "dispatch"] if *method == Method::POST => Ok((
            200,
            dispatch::promote(state, db, path, kind, owner, turn, v)?,
        )),
        ["api", kind, owner, "turns", turn, "questions", question, "answer"]
            if *method == Method::POST =>
        {
            Ok((
                200,
                questions::answer(state, db, path, (kind, owner), turn, question, v)?,
            ))
        }
        ["api", kind, owner, "queue"] if *method == Method::POST => {
            Ok((200, queue::reorder(db, path, kind, owner, v)?))
        }
        ["api", kind, owner, "turns", turn, action] if *method == Method::POST => {
            control(state, db, path, kind, owner, turn, action, v)
        }
        ["api", kind, owner, "queue"] if *method == Method::GET => {
            Ok((200, queue::read(db, path, kind, owner, q)?))
        }
        _ => Err(error(404, "Route not found.")),
    }
}
fn create(
    state: &AppState,
    db: &Connection,
    path: &str,
    kind: &str,
    owner: &str,
    v: &Value,
) -> Result<(u16, Value)> {
    let id = string(v, "id");
    let f = files(v)?;
    let text = if v["text"]
        .as_str()
        .is_some_and(|s| s.trim().is_empty() && s.encode_utf16().count() <= 16000)
        && f.as_array().is_some_and(|a| !a.is_empty())
    {
        String::new()
    } else {
        field(v, "text", 16000)?
    };
    let text = crate::mcp::secrets::redact(
        &text,
        &crate::computer::vault::redactions(state, db)?,
        false,
    );
    let mode = v
        .get("dispatchMode")
        .and_then(Value::as_str)
        .unwrap_or("queue");
    if !["queue", "steer", "restart"].contains(&mode) {
        return Err(error(400, "Invalid dispatch mode."));
    }
    if v.get("automatic").is_some_and(|v| !v.is_boolean()) {
        return Err(error(400, "Invalid routing choice."));
    }
    let automatic = i64::from(v["automatic"] == true);
    if automatic == 1 && kind != "channels" {
        return Err(error(
            400,
            "Automatic routing is only available in channels.",
        ));
    }
    let target = v.get("targetTurn").filter(|v| !v.is_null());
    if target.is_some_and(|v| !v.as_str().is_some_and(uuid)) {
        return Err(error(400, "Invalid active turn."));
    }
    if mode == "queue" && target.is_some() {
        return Err(error(400, "Queue does not target an active turn."));
    }
    if kind == "channels"
        && mode != "queue"
        && (target.is_none() || v["recipient"].as_str().is_none_or(str::is_empty))
    {
        return Err(error(
            400,
            "Select an active member turn before steering or restarting.",
        ));
    }
    let mut request = json!({"path":path,"text":text,"files":f,"recipient":v["recipient"].as_str().filter(|s|!s.is_empty()),"automatic":automatic,"mode":mode,"target":target});
    if let Some(context) = v.get("htmlContext") {
        if mode != "queue" {
            return Err(error(400, "Send HTML reply context with a new message."));
        }
        request["htmlContext"] =
            crate::files::rich_html::accept_context(state, db, kind, owner, context)?;
    }
    if let Some(old) = one(db, "SELECT * FROM turns WHERE id=?", [id])? {
        if let Some(receipt) = one(
            db,
            "SELECT request,result FROM dispatch_commands WHERE id=?",
            [id],
        )? {
            if data::json(&receipt["request"])? != request {
                return Err(error(
                    409,
                    "ID was already used for a different message or dispatch mode.",
                ));
            }
            if mode != "queue" {
                return Ok((202, data::json(&receipt["result"])?));
            }
        } else if old["text"] != text || data::json(&old["attachments"])? != f {
            return Err(error(409, "ID was already used for a different message."));
        }
        scoped(
            db,
            kind,
            owner,
            id,
            "ID was already used for a different message.",
        )
        .map_err(|_| error(409, "ID was already used for a different message."))?;
        return Ok((202, old));
    }
    let (agent, thread, channel) = conversation(db, kind, owner, v)?;
    if agent == crate::mcp::guide::ID && (thread.is_some() || channel.is_some()) {
        return Err(error(403, "Use the guide's direct conversation."));
    }
    if data::agents::hidden(db, &agent)? {
        return Err(error(
            409,
            "Restore the recipient agent before sending a message.",
        ));
    }
    ready(state, db, &agent)?;
    if data::agents::read(db, &agent)?["runtime"] != "api"
        && super::harness::workspace(
            state,
            db,
            &json!({"agent":agent,"channel":channel,"thread":thread}),
        )?
        .is_none()
    {
        return Err(error(
            503,
            "Coding runtime requires a workspace for this conversation.",
        ));
    }
    let count: i64 = db.query_row(
        "SELECT count(*) FROM turns WHERE agent=? AND status IN ('queued','running')",
        [&agent],
        |r| r.get(0),
    )?;
    if count >= 100 {
        return Err(error(
            429,
            "Queue is full. Wait for previous messages to finish.",
        ));
    }
    validate_files(db, &f, &format!("{kind}:{owner}"))?;
    if channel.is_some() && v["recipient"].as_str().is_none_or(str::is_empty) && automatic == 0 {
        let members=all(db,"SELECT agent FROM channel_members m WHERE channel=? AND NOT EXISTS(SELECT 1 FROM agent_visibility v WHERE v.agent=m.agent AND v.hidden=1)",[owner])?;
        if members.len() > 6 {
            return Err(error(400, "Chatrooms support at most 6 active members."));
        }
        for member in members {
            let member = string(&member, "agent");
            ready(state, db, member)?;
            if db.query_row(
                "SELECT count(*) FROM turns WHERE agent=? AND status IN('queued','running')",
                [member],
                |r| r.get::<_, i64>(0),
            )? >= 100
            {
                return Err(error(429, "A room member queue is full."));
            }
        }
    }
    let routing = if channel.is_some() && v["recipient"].as_str().is_none_or(str::is_empty) {
        Some(if automatic == 1 { "pending" } else { "room" })
    } else {
        None
    };
    db.execute("INSERT INTO turns(id,agent,thread,channel,recipient,automatic,routing,text,attachments,status,created) VALUES (?,?,?,?,?,?,?,?,?,'queued',?)",params![id,agent,thread,channel,v["recipient"].as_str().filter(|s|!s.is_empty()),automatic,routing,text,f.to_string(),iso(db,state.config.now())?])?;
    if let Some(context) = request.get("htmlContext") {
        db.execute(
            "INSERT INTO html_reply_context(turn,context) VALUES(?,?)",
            params![id, string(context, "data")],
        )?;
    }
    if channel.is_none() || v["recipient"].as_str().is_some_and(|s| !s.is_empty()) || automatic == 1
    {
        super::messaging::capture(db, id)?;
    }
    if channel.is_some() && v["recipient"].as_str().is_none_or(str::is_empty) && automatic == 0 {
        let lead = one(
            db,
            "SELECT lead FROM channels WHERE id=?",
            [channel.as_deref().unwrap_or("")],
        )?
        .and_then(|r| r["lead"].as_str().map(str::to_owned));
        room::start(db, id, lead.as_deref())?;
    }
    if let Some(thread) = &thread {
        db.execute("UPDATE threads SET name=?,revision=revision+1 WHERE id=? AND name='New conversation' AND NOT EXISTS(SELECT 1 FROM turns WHERE thread=? AND id<>?)",params![text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(80).collect::<String>(),thread,thread,id])?;
    }
    if mode != "queue" {
        let active = if let Some(t) = target.and_then(Value::as_str) {
            Some(scoped(
                db,
                kind,
                owner,
                t,
                "Active turn not found in this conversation.",
            )?)
        } else {
            one(db,"SELECT * FROM turns WHERE agent=? AND thread IS ? AND channel IS ? AND status='running' AND id<>? ORDER BY seq DESC LIMIT 1",params![agent,thread,channel,id])?
        };
        if let Some(a) = &active {
            if a["agent"] != agent {
                return Err(error(404, "Active turn not found in this conversation."));
            }
            if room::stopped(db, string(a, "id"))? {
                return Err(error(409, "Discussion is stopped. Send a new message."));
            }
            if let Some(root) = room::generation(db, string(a, "id"))? {
                db.execute("INSERT INTO room_dispatches VALUES(?,?)", params![id, root])?;
            }
        }
        let native = data::agents::read(db, &agent)?["runtime"] != "api";
        let mode = if native { "restart" } else { mode };
        if let Some(a) = active.filter(|a| a["status"] == "running") {
            let active = string(&a, "id");
            db.execute(
                "INSERT INTO dispatch_links VALUES (?,?,?,?,?,?)",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    active,
                    id,
                    mode,
                    "",
                    iso(db, state.config.now())?
                ],
            )?;
            if mode == "steer" {
                if f.as_array().is_some_and(|a| !a.is_empty()) {
                    return Err(error(
                        400,
                        "Queue or restart to send attachments while a turn runs.",
                    ));
                }
                db.execute("UPDATE turns SET dispatch_mode='steer',dispatch_target=?,dispatch_label='Waiting for steering safe point' WHERE id=?",[active,id])?;
            } else {
                dispatch::cancel(db, active, "Stopped for restart. Provider effects may already have happened; uncertain operations were not replayed.")?;
                db.execute("INSERT INTO queue_positions VALUES(?,?) ON CONFLICT(turn) DO UPDATE SET position=excluded.position",params![id,history::position(db,&a)?])?;
                state.turns.cancel(active);
                db.execute("UPDATE turns SET dispatch_mode='restart',dispatch_target=?,dispatch_label=? WHERE id=?",[active,if native {"Runtime cannot steer; restarted from interrupted turn"}else{"Restarted from interrupted turn"},id])?;
            }
        } else {
            db.execute(
                "UPDATE turns SET dispatch_label='Queued after active turn' WHERE id=?",
                [id],
            )?;
        }
    }
    let result = one(db, "SELECT * FROM turns WHERE id=?", [id])?.ok_or_else(Failure::storage)?;
    data::save_receipt(db, "dispatch_commands", id, &request, &result)?;
    Ok((202, result))
}
#[allow(clippy::too_many_arguments)]
fn control(
    state: &AppState,
    db: &Connection,
    path: &str,
    kind: &str,
    owner: &str,
    id: &str,
    action: &str,
    v: &Value,
) -> Result<(u16, Value)> {
    let t = scoped(
        db,
        kind,
        owner,
        id,
        if action == "cancel" {
            "Message not found in this conversation."
        } else {
            "Task not found in this conversation."
        },
    )?;
    if action == "cancel" {
        if t["status"] == "done" {
            return Err(error(409, "Message is complete and cannot be cancelled."));
        }
        let branch=all(db,"WITH RECURSIVE tree(id) AS(SELECT id FROM turns WHERE id=? UNION ALL SELECT t.id FROM turns t JOIN tree ON t.parent=tree.id) SELECT t.* FROM turns t JOIN tree ON tree.id=t.id",[id])?;
        for t in branch {
            let id = string(&t, "id");
            db.execute(
                "UPDATE queue_edits SET state='cancelled' WHERE turn=? AND state='held'",
                [id],
            )?;
            db.execute("UPDATE turns SET status='cancelled',paused=0,revision=revision+1,error=? WHERE id=? AND status IN ('queued','running','failed','interrupted')",[if t["status"]=="running"{"Stopped. Provider may have accepted the request; usage may still be counted."}else{"Cancelled. Steps already sent to the provider may still be counted."},id])?;
            state.turns.cancel(id);
        }
    } else if ["pause", "resume", "reassign"].contains(&action) {
        if action != "pause" && queue::editing(db, id)? {
            return Err(error(
                409,
                "Finish editing the queue before changing the task.",
            ));
        }
        if action == "pause" && queue::editing(db, id)? {
            return Err(error(
                409,
                "Finish editing the queue before changing the task.",
            ));
        }
        if !integer(&v["revision"], 1, i64::MAX) {
            return Err(error(400, "Invalid task revision."));
        }
        let request = json!({"path":path,"revision":v["revision"],"recipient":if action=="reassign"{Some(field(v,"recipient",36)?)}else{None},"acknowledgeUncertain":v["acknowledgeUncertain"]==true});
        if let Some(r) = data::receipt(
            db,
            "task_controls",
            string(v, "id"),
            &request,
            "ID was already used for a different control.",
        )? {
            return Ok((200, r));
        }
        if t["revision"] != v["revision"] {
            return Err(error(409, "Task changed. Reload before continuing."));
        }
        if ["done", "cancelled"].contains(&string(&t, "status")) {
            return Err(error(409, "Terminal tasks cannot be resumed or paused."));
        }
        if exists(
            db,
            "SELECT 1 FROM flow_steps WHERE turn=? AND status IN('done','failed','skipped')",
            [id],
        )? {
            return Err(error(
                409,
                "Flow step has already settled. Start a new routine run.",
            ));
        }
        let branch = all(db,"WITH RECURSIVE tree(id) AS(SELECT id FROM turns WHERE id=? UNION ALL SELECT t.id FROM turns t JOIN tree ON t.parent=tree.id) SELECT t.* FROM turns t JOIN tree ON tree.id=t.id",[id])?;
        let mut unfinished = Vec::new();
        for item in &branch {
            if ["done", "cancelled"].contains(&string(item, "status")) {
                continue;
            }
            if item["id"] == id || !exists(db,"SELECT 1 FROM agent_deliveries d JOIN agent_requests r ON r.id=d.request WHERE d.turn=? AND(r.settled=1 OR EXISTS(SELECT 1 FROM agent_reply_claims WHERE delivery=d.turn))",[string(item,"id")])? {
                unfinished.push(item);
            }
        }
        if crate::computer::control::turn_wait(db, id)?.is_some() {
            return Err(error(
                409,
                "Hand computer control back or dismiss its request before changing the task.",
            ));
        }
        if action == "pause" {
            for item in &unfinished {
                let child = string(item, "id");
                db.execute("UPDATE turns SET paused=1,revision=revision+1,status=CASE WHEN status='running' THEN 'interrupted' ELSE status END,error=CASE WHEN status='running' THEN 'Stop requested. Check the provider before resuming.' ELSE error END WHERE id=?",[child])?;
                state.turns.cancel(child);
            }
        } else {
            if t["paused"] != 1 && !["interrupted", "failed"].contains(&string(&t, "status")) {
                return Err(error(
                    409,
                    "Pause the task before resuming or changing its owner.",
                ));
            }
            if exists(db,"WITH RECURSIVE tree(id,parent,paused,status) AS(SELECT id,parent,paused,status FROM turns WHERE id=? UNION ALL SELECT t.id,t.parent,t.paused,t.status FROM turns t JOIN tree ON t.id=tree.parent) SELECT 1 FROM tree WHERE id<>? AND(paused=1 OR status='cancelled')",[id,id])? {
                return Err(error(409,"Resume the parent task first."));
            }
            if state
                .turns
                .active
                .lock()
                .map_err(|_| Failure::storage())?
                .keys()
                .any(|active| unfinished.iter().any(|item| item["id"] == *active))
            {
                return Err(error(
                    409,
                    "Previous request has not stopped yet. Try after the local stop completes.",
                ));
            }
            if unfinished
                .iter()
                .any(|item| ["failed", "interrupted"].contains(&string(item, "status")))
                && v["acknowledgeUncertain"] != true
            {
                return Err(error(
                    409,
                    "Check provider results and effects, then confirm before resuming.",
                ));
            }
            for item in &branch {
                if crate::computer::control::turn_wait(db, string(item, "id"))?.is_some() {
                    return Err(error(409,"Hand computer control back or dismiss its request before resuming or reassigning."));
                }
                if !["done", "cancelled"].contains(&string(item, "status"))
                    && exists(
                        db,
                        "SELECT 1 FROM questions WHERE turn=? AND answer IS NULL",
                        [string(item, "id")],
                    )?
                {
                    return Err(error(
                        409,
                        "Answer the task question before resuming or reassigning.",
                    ));
                }
            }
            if action == "reassign" {
                for item in &branch {
                    if exists(
                        db,
                        "SELECT 1 FROM turn_waits WHERE turn=? AND status='waiting'",
                        [string(item, "id")],
                    )? {
                        return Err(error(
                            409,
                            "Complete or cancel the event wait before reassigning the task.",
                        ));
                    }
                }
            }
            let recipient = if action == "reassign" {
                &v["recipient"]
            } else {
                &t["agent"]
            };
            let (agent, _, _) = conversation(db, kind, owner, &json!({"recipient":recipient}))?;
            for item in unfinished {
                let child = string(item, "id");
                if exists(db,"SELECT 1 FROM agent_deliveries d JOIN agent_requests r ON r.id=d.request WHERE d.turn=? AND(r.settled=1 OR EXISTS(SELECT 1 FROM agent_reply_claims WHERE delivery=d.turn))",[child])? {
                    return Err(error(409,"Message result was already sent to the requester. Start a new request."));
                }
                if exists(
                    db,
                    "SELECT 1 FROM threads WHERE id=? AND(archived=1 OR deleted=1)",
                    [item["thread"].as_str().unwrap_or("")],
                )? {
                    return Err(error(
                        409,
                        "Restore the subtask conversation before resuming.",
                    ));
                }
                let owner = if child == id {
                    agent.as_str()
                } else {
                    string(item, "agent")
                };
                if data::agents::hidden(db, owner)? {
                    return Err(error(409, "Restore the agent before resuming the task."));
                }
                if let Some(channel) = t["channel"].as_str() {
                    if !exists(
                        db,
                        "SELECT 1 FROM channel_members WHERE channel=? AND agent=?",
                        [channel, owner],
                    )? {
                        return Err(error(409, "Task owner is no longer a channel member."));
                    }
                }
                ready(state, db, owner)?;
                if db.query_row("SELECT count(*) FROM turns WHERE agent=? AND id<>? AND status IN('queued','running')",[owner,child],|r|r.get::<_,i64>(0))? >= 100 {
                    return Err(error(429,"Recipient queue is full."));
                }
                db.execute("UPDATE turns SET agent=?,routing=CASE WHEN ?='reassign' THEN 'assigned' ELSE routing END,paused=0,revision=revision+1,status='queued',error=NULL,tool_budget_used=0,waiting_for=CASE WHEN waiting_for IN(SELECT id FROM turns WHERE status='cancelled') THEN NULL ELSE waiting_for END WHERE id=?",[owner,action,child])?;
            }
        }
        let result =
            one(db, "SELECT * FROM turns WHERE id=?", [id])?.ok_or_else(Failure::storage)?;
        data::save_receipt(db, "task_controls", string(v, "id"), &request, &result)?;
        return Ok((200, result));
    } else {
        return Err(error(404, "Route not found."));
    }
    Ok((
        200,
        one(db, "SELECT * FROM turns WHERE id=?", [id])?.ok_or_else(Failure::storage)?,
    ))
}
