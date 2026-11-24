use super::*;

#[allow(clippy::too_many_arguments)]
pub fn promote(
    state: &AppState,
    db: &Connection,
    path: &str,
    kind: &str,
    owner: &str,
    id: &str,
    v: &Value,
) -> Result<Value> {
    if !integer(&v["revision"], 1, 9_007_199_254_740_991)
        || !["steer", "restart"].contains(&string(v, "mode"))
        || !uuid(string(v, "targetTurn"))
    {
        return Err(error(400, "Invalid queued dispatch."));
    }
    let turn = admission::scoped(
        db,
        kind,
        owner,
        id,
        "Message not found in this conversation.",
    )?;
    let request =
        json!({"path":path,"revision":v["revision"],"mode":v["mode"],"targetTurn":v["targetTurn"]});
    if let Some(prior) = data::receipt(
        db,
        "task_controls",
        string(v, "id"),
        &request,
        "ID was already used for another control.",
    )? {
        return Ok(prior);
    }
    if turn["revision"] != v["revision"]
        || !queue::editable(db, &turn)?
        || queue::editing(db, id)?
        || turn["paused"] == 1
    {
        return Err(error(409, "Message changed or has already started."));
    }
    if exists(
        db,
        "SELECT 1 FROM remote_agent_calls WHERE delivery=?",
        [id],
    )? {
        return Err(error(
            409,
            "Remote receipts cannot execute locally. Open this agent from Machines.",
        ));
    }
    admission::ready(state, db, string(&turn, "agent"))?;
    let active = admission::scoped(
        db,
        kind,
        owner,
        string(v, "targetTurn"),
        "Active turn not found in this conversation.",
    )?;
    if active["id"] == id
        || active["agent"] != turn["agent"]
        || active["channel"] != turn["channel"]
        || active["thread"] != turn["thread"]
    {
        return Err(error(404, "Active turn not found in this conversation."));
    }
    if data::agents::hidden(db, string(&turn, "agent"))?
        || turn["thread"].as_str().is_some_and(|thread| {
            exists(
                db,
                "SELECT 1 FROM threads WHERE id=? AND(archived=1 OR deleted=1)",
                [thread],
            )
            .unwrap_or(true)
        })
    {
        return Err(error(409, "Restore the conversation first."));
    }
    let mode = string(v, "mode");
    if mode == "steer" && data::json(&turn["attachments"])? != json!([]) {
        return Err(error(409, "This message can be queued or restarted."));
    }
    if active["status"] != "running" {
        db.execute("UPDATE turns SET dispatch_label='Queued after active turn',revision=revision+1 WHERE id=?",[id])?;
    } else {
        let target = string(&active, "id");
        db.execute(
            "INSERT INTO dispatch_links VALUES(?,?,?,?,?,?)",
            params![
                uuid::Uuid::new_v4().to_string(),
                target,
                id,
                mode,
                "",
                iso(db, state.config.now())?
            ],
        )?;
        if mode == "restart" {
            cancel(db, target, "Stopped for restart.")?;
            db.execute("INSERT INTO queue_positions VALUES(?,?) ON CONFLICT(turn) DO UPDATE SET position=excluded.position",params![id,history::position(db,&active)?])?;
        }
        db.execute("UPDATE turns SET dispatch_mode=?,dispatch_target=?,dispatch_label=?,revision=revision+1 WHERE id=?",params![mode,target,if mode=="steer"{"Waiting for steering safe point"}else{"Restarted from interrupted turn"},id])?;
    }
    let result = one(db, "SELECT * FROM turns WHERE id=?", [id])?.ok_or_else(Failure::storage)?;
    data::save_receipt(db, "task_controls", string(v, "id"), &request, &result)?;
    Ok(result)
}
pub fn cancel(db: &Connection, id: &str, message: &str) -> Result<()> {
    for row in all(db,"WITH RECURSIVE tree(id) AS(SELECT id FROM turns WHERE id=? UNION SELECT t.id FROM turns t JOIN tree ON t.parent=tree.id WHERE NOT EXISTS(SELECT 1 FROM room_turns rt WHERE rt.turn=t.id AND rt.root=t.parent AND rt.round=1)) SELECT id FROM tree",[id])? {
        let id=string(&row,"id");
        db.execute("UPDATE queue_edits SET state='cancelled' WHERE turn=? AND state='held'",[id])?;
        db.execute("UPDATE turns SET status='cancelled',paused=0,revision=revision+1,error=? WHERE id=? AND status IN('queued','running','failed','interrupted')",[message,id])?;
    }
    Ok(())
}
pub fn guidance(db: &Connection, id: &str) -> Result<Vec<Value>> {
    all(db,"SELECT id,text,attachments FROM turns WHERE dispatch_target=? AND dispatch_mode='steer' AND status='done' ORDER BY seq",[id])
}
pub fn context(db: &Connection, id: &str) -> Result<Option<String>> {
    let Some(link) = one(
        db,
        "SELECT * FROM dispatch_links WHERE target_turn=? AND mode='restart'",
        [id],
    )?
    else {
        return Ok(None);
    };
    if !string(&link, "context").is_empty() {
        return Ok(Some(string(&link, "context").to_owned()));
    }
    let source = string(&link, "source_turn");
    let turn = one(
        db,
        "SELECT id,text,reply,error,status FROM turns WHERE id=?",
        [source],
    )?
    .ok_or_else(Failure::storage)?;
    let partial=one(db,"SELECT s.text FROM provider_streams s JOIN provider_calls c ON c.seq=s.call WHERE c.turn=? ORDER BY c.seq DESC LIMIT 1",[source])?.map(|r|history::slice(string(&r,"text"),0,24000));
    let mut tools = all(
        db,
        "SELECT call,result FROM flow_tool_calls WHERE turn=? ORDER BY seq LIMIT 32",
        [source],
    )?;
    for tool in &mut tools {
        for key in ["call", "result"] {
            tool[key] = json!(history::slice(string(tool, key), 0, 2000));
        }
    }
    let files = all(
        db,
        "SELECT call,file FROM generated_files WHERE turn=? ORDER BY seq LIMIT 8",
        [source],
    )?;
    let calls = all(
        db,
        "SELECT model,status,purpose FROM provider_calls WHERE turn=? ORDER BY seq DESC LIMIT 32",
        [source],
    )?;
    let context=json!({"source":turn,"partial":partial,"guidance":guidance(db,source)?,"tools":tools,"files":files,"calls":calls,"uncertain":[],"harness":all(db,"SELECT id,name,state FROM harness_steps WHERE turn=?",[source])?}).to_string();
    db.execute(
        "UPDATE dispatch_links SET context=? WHERE id=?",
        [&context, string(&link, "id")],
    )?;
    Ok(Some(context))
}
