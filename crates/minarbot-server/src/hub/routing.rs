use super::*;
use authority::{Context, Grant, WRITE};

fn message(value: &Value, limit: usize) -> Result<&str> {
    value
        .as_str()
        .filter(|v| !v.trim().is_empty() && !v.contains('\0') && v.encode_utf16().count() <= limit)
        .ok_or_else(|| Failure::new(400, "invalid_text", "Message is empty or too large."))
}
pub(super) fn enqueue(db: &Connection, frame: &Value, expires: i64) -> Result<()> {
    let encoded = canonical(frame);
    if encoded.len() > LIMIT {
        return Err(Failure::new(
            413,
            "payload_too_large",
            "Frame exceeds the limit.",
        ));
    }
    let fence = frame
        .get("fence")
        .map(canonical)
        .unwrap_or_else(|| "{}".to_owned());
    db.execute("INSERT INTO outbox(event_id,project_id,section_id,participation_id,node_id,turn_id,kind,fence_json,frame_json,expires_at) VALUES(?,?,?,?,?,?,?,?,?,?)",
        params![text(frame,"eventId"),text(frame,"projectId"),text(frame,"sectionId"),text(frame,"participationId"),text(frame,"nodeId"),frame["turnId"].as_str(),text(frame,"kind"),fence,encoded,expires])?;
    Ok(())
}
fn frame(
    db: &Connection,
    ctx: &Context<'_>,
    section: &str,
    participation: &str,
    payload: Value,
    turn: Option<&str>,
    grant: &Grant,
) -> Result<Value> {
    let mut frame = json!({"v":1,"kind":if turn.is_some() {"turn.request"} else {"section.event"},"eventId":new_id(),
        "projectId":ctx.project,"sectionId":section,"participationId":participation,"agentId":grant.entry["agent_id"],"nodeId":grant.node["id"],
        "requesterAccountId":ctx.account,"payerAccountId":grant.owner["id"],"payerBindingRef":grant.policy["payerBindingRef"],
        "fence":grant.fence(db,ctx,section,turn.is_some())?,"sequence":1,"expiresAt":now()+600_000,"payloadDigest":digest(&canonical(&payload)),"payload":payload});
    if let Some(turn) = turn {
        frame["turnId"] = json!(turn);
    }
    Ok(frame)
}
fn current(db: &Connection, frame: &Value) -> Result<()> {
    let project = text(frame, "projectId");
    let host = authority::context_host(db, project)?;
    let ctx = Context {
        project,
        account: text(frame, "requesterAccountId"),
        host: &host,
    };
    let section = text(frame, "sectionId");
    let grant = authority::active(db, &ctx, section, text(frame, "participationId"))?;
    if grant.fence(db, &ctx, section, frame["kind"] != "section.event")? != frame["fence"]
        || grant.entry["agent_id"] != frame["agentId"]
        || grant.node["id"] != frame["nodeId"]
        || grant.owner["id"] != frame["payerAccountId"]
        || grant.policy["payerBindingRef"] != frame["payerBindingRef"]
    {
        return Err(stale());
    }
    if grant.node["status"] != "ready" {
        return Err(authority::offline());
    }
    Ok(())
}
pub(super) fn post(db: &Connection, ctx: &Context<'_>, input: &Value) -> Result<Value> {
    closed(input, &["requestId", "sectionId", "text", "mentions"])?;
    let request = id(&input["requestId"])?;
    let section = id(&input["sectionId"])?;
    let text = message(&input["text"], 16_000)?;
    let mut mentions = input["mentions"]
        .as_array()
        .filter(|v| v.len() <= 16)
        .ok_or_else(denied)?
        .iter()
        .map(id)
        .collect::<Result<Vec<_>>>()?;
    mentions.sort_unstable();
    if mentions.windows(2).any(|v| v[0] == v[1]) {
        return Err(denied());
    }
    authority::section(db, ctx, &section, WRITE)?;
    let mut grants = Vec::with_capacity(mentions.len());
    for p in &mentions {
        let grant = authority::active(db, ctx, &section, p)?;
        grant.fence(db, ctx, &section, true)?;
        grants.push(grant);
    }
    let command = digest(&canonical(
        &json!({"sectionId":section,"text":text,"mentions":mentions}),
    ));
    if let Some(receipt) = one(db, "SELECT digest,result_json FROM command_receipts WHERE project_id=? AND actor_account_id=? AND request_id=?", params![ctx.project,ctx.account,request])? {
        if receipt["digest"] != command { return Err(conflict()); }
        return stored(&receipt, "result_json");
    }
    if grants.iter().any(|g| g.node["status"] != "ready") {
        return Err(authority::offline());
    }
    let queued: usize = db.query_row("SELECT count(*) FROM turns WHERE project_id=? AND state IN('pending','admitted') AND json_extract(request_json,'$.expiresAt')>?", params![ctx.project, now()], |r| r.get(0))?;
    if !mentions.is_empty() && queued + mentions.len() > 100 {
        return Err(Failure::new(
            429,
            "project_queue_full",
            "Project queue is full. Retry later.",
        ));
    }
    let message_id = new_id();
    db.execute(
        "INSERT INTO messages VALUES(?,?,?,?,?,?)",
        params![message_id, ctx.project, section, ctx.account, text, now()],
    )?;
    let payload = json!({"messageId":message_id,"text":text});
    for entry in rows(db, "SELECT participation_id AS id FROM participation_sections WHERE project_id=? AND section_id=?", params![ctx.project, section])? {
        let id = entry["id"].as_str().ok_or_else(Failure::storage)?;
        let event = (|| {
            let grant = authority::active(db, ctx, &section, id)?;
            if grant.node["status"] != "ready" { return Err(authority::offline()); }
            frame(db, ctx, &section, id, payload.clone(), None, &grant)
        })();
        match event {
            Ok(event) => enqueue(db, &event, now() + 600_000)?,
            Err(error) if error.status < 500 || error.code == "node_offline" => {},
            Err(error) => return Err(error),
        }
    }
    let mut turns = Vec::with_capacity(mentions.len());
    for (id, grant) in mentions.iter().zip(grants.iter()) {
        let turn = new_id();
        let mut payload = payload.clone();
        payload["maxCostMicros"] = grant.policy["maxCostMicros"].clone();
        let frame = frame(db, ctx, &section, id, payload, Some(&turn), grant)?;
        db.execute("INSERT INTO turns(id,project_id,section_id,participation_id,message_id,requester_account_id,payer_account_id,payer_binding_ref,node_id,command_digest,fence_json,request_json,state) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,'pending')",
            params![turn,ctx.project,section,id,message_id,ctx.account,grant.owner["id"].as_str(),grant.policy["payerBindingRef"].as_str(),grant.node["id"].as_str(),command,canonical(&frame["fence"]),canonical(&frame)])?;
        enqueue(
            db,
            &frame,
            frame["expiresAt"].as_i64().ok_or_else(Failure::storage)?,
        )?;
        turns.push(turn);
    }
    let result = json!({"messageId":message_id,"turnIds":turns});
    db.execute(
        "INSERT INTO command_receipts VALUES(?,?,?,?,?)",
        params![
            ctx.project,
            ctx.account,
            request,
            command,
            canonical(&result)
        ],
    )?;
    Ok(result)
}

pub(super) fn history(db: &Connection, ctx: &Context<'_>, section: &str) -> Result<Value> {
    authority::section(db, ctx, section, &[])?;
    let messages = rows(db, "SELECT * FROM messages WHERE project_id=? AND section_id=? ORDER BY created_at,id LIMIT 100", [ctx.project,section])?;
    let results = rows(db, "SELECT r.frame_json FROM node_results r JOIN turns t ON t.project_id=r.project_id AND t.id=r.turn_id WHERE t.project_id=? AND t.section_id=? ORDER BY t.rowid,r.sequence LIMIT 100", [ctx.project,section])?
        .into_iter().map(|row| stored(&row,"frame_json")).collect::<Result<Vec<_>>>()?;
    Ok(json!({"messages":messages,"results":results}))
}
pub(super) fn turns(db: &Connection, ctx: &Context<'_>, section: &str) -> Result<Vec<Value>> {
    authority::section(db, ctx, section, &[])?;
    rows(db, "SELECT t.id,t.message_id AS messageId,p.agent_id AS agentId,t.requester_account_id AS requesterAccountId,t.payer_account_id AS payerAccountId,t.state
        FROM turns t JOIN participations p ON p.project_id=t.project_id AND p.id=t.participation_id WHERE t.project_id=? AND t.section_id=? ORDER BY t.rowid DESC LIMIT 100", [ctx.project,section])
}
pub(super) fn turn(db: &Connection, ctx: &Context<'_>, id: &str) -> Result<Value> {
    authority::member(db, ctx, &[])?;
    let turn = one(
        db,
        "SELECT * FROM turns WHERE project_id=? AND id=?",
        [ctx.project, id],
    )?
    .ok_or_else(missing)?;
    authority::section(db, ctx, text(&turn, "section_id"), &[])?;
    Ok(turn)
}

fn valid_delivery(db: &Connection, row: &Value) -> Result<Value> {
    let f = stored(row, "frame_json")?;
    if f.get("fence").is_some() {
        if row["expires_at"].as_i64().is_none_or(|v| v <= now()) {
            return Err(stale());
        }
        current(db, &f)?;
        if let Some(turn) = f["turnId"].as_str() {
            if !db.prepare("SELECT 1 FROM turns WHERE project_id=? AND id=? AND state IN('pending','admitted')")?.exists([text(&f,"projectId"),turn])? { return Err(stale()); }
        }
    } else if f["authorityGeneration"] != generation(db)? {
        return Err(stale());
    }
    Ok(f)
}
pub(super) fn pending(db: &Connection, principal: &Value) -> Result<Vec<Value>> {
    authority::current_node(db, principal)?;
    let mut frames = Vec::new();
    for row in rows(db, "SELECT * FROM outbox WHERE node_id=? AND acked=0 ORDER BY CASE WHEN kind IN('grant.revoked','turn.cancel') THEN 0 ELSE 1 END,rowid LIMIT 100", [text(principal,"nodeId")])? {
        match valid_delivery(db, &row) {
            Ok(frame) => frames.push(frame),
            Err(error) if error.status < 500 || error.code == "node_offline" => { db.execute("DELETE FROM outbox WHERE event_id=?", [text(&row,"event_id")])?; },
            Err(error) => return Err(error),
        }
    }
    Ok(frames)
}
pub(super) fn ack(db: &Connection, principal: &Value, input: &Value) -> Result<()> {
    authority::current_node(db, principal)?;
    let event = id(&input["eventId"])?;
    let row = one(
        db,
        "SELECT * FROM outbox WHERE node_id=? AND event_id=?",
        [text(principal, "nodeId"), &event],
    )?
    .ok_or_else(missing)?;
    valid_delivery(db, &row)?;
    if input["frameDigest"] != digest(text(&row, "frame_json")) {
        return Err(conflict());
    }
    db.execute("UPDATE outbox SET acked=1 WHERE event_id=?", [event])?;
    Ok(())
}
pub(super) fn grants(db: &Connection, principal: &Value) -> Result<Vec<Value>> {
    let node = authority::current_node(db, principal)?;
    let generation = generation(db)?;
    let mut grants = Vec::new();
    for row in rows(db, "SELECT p.project_id,p.id,s.section_id,h.host FROM participations p JOIN agents a ON a.id=p.agent_id
        JOIN participation_sections s ON s.project_id=p.project_id AND s.participation_id=p.id JOIN host_bindings h ON h.project_id=p.project_id AND h.active=1
        WHERE a.home_node_id=?", [text(&node,"id")])? {
        let ctx = Context { project: text(&row,"project_id"), account: text(principal,"accountId"), host: text(&row,"host") };
        let section = text(&row,"section_id");
        let id = text(&row,"id");
        match authority::active(db, &ctx, section, id) {
            Ok(g) => grants.push(json!({"v":1,"kind":"grant.snapshot","projectId":ctx.project,"sectionId":section,"participationId":id,
                "agentId":g.entry["agent_id"],"nodeId":node["id"],"agentOwnerAccountId":ctx.account,"authorityGeneration":generation,
                "nodeGeneration":node["generation"],"connectionGeneration":node["connection_generation"],"revision":g.entry["revision"],
                "ownerConsentRevision":g.entry["owner_consent_revision"],"projectConsentRevision":g.entry["project_consent_revision"],
                "ownerMembershipRevision":g.membership["revision"],"policy":g.policy})),
            Err(error) if error.status < 500 => {},
            Err(error) => return Err(error),
        }
    }
    Ok(grants)
}

pub(super) fn lease(db: &Connection, principal: &Value, input: &Value) -> Result<Value> {
    authority::current_node(db, principal)?;
    closed(
        input,
        &["projectId", "turnId", "actionId", "actionDigest", "fence"],
    )?;
    let project = id(&input["projectId"])?;
    let turn = id(&input["turnId"])?;
    let action = id(&input["actionId"])?;
    let action_digest = text(input, "actionDigest");
    if !hex(action_digest, 64) {
        return Err(denied());
    }
    let row = one(db, "SELECT request_json FROM turns WHERE project_id=? AND id=? AND node_id=? AND state='admitted'", [project.as_str(),turn.as_str(),text(principal,"nodeId")])?.ok_or_else(denied)?;
    let request = stored(&row, "request_json")?;
    current(db, &request)?;
    let clock = now();
    let deadline = request["expiresAt"].as_i64().ok_or_else(Failure::storage)?;
    if deadline <= clock || input["fence"] != request["fence"] {
        return Err(stale());
    }
    let prior = one(
        db,
        "SELECT * FROM action_leases WHERE project_id=? AND turn_id=? AND action_id=?",
        [&project, &turn, &action],
    )?;
    let (lease, expires) = if let Some(prior) = prior {
        let expires = prior["expires_at"].as_i64().ok_or_else(Failure::storage)?;
        if prior["action_digest"] != action_digest {
            return Err(conflict());
        }
        if expires <= clock {
            return Err(stale());
        }
        (text(&prior, "id").to_owned(), expires)
    } else {
        let lease = new_id();
        let expires = deadline.min(clock + 5_000);
        db.execute(
            "INSERT INTO action_leases VALUES(?,?,?,?,?,?,?)",
            params![
                lease,
                project,
                turn,
                action,
                action_digest,
                canonical(&request["fence"]),
                expires
            ],
        )?;
        (lease, expires)
    };
    Ok(
        json!({"leaseId":lease,"actionId":action,"actionDigest":action_digest,"fence":request["fence"],"expiresAt":expires}),
    )
}

pub(super) fn accept(db: &Connection, principal: &Value, f: &Value) -> Result<Value> {
    authority::current_node(db, principal)?;
    closed(
        f,
        &[
            "v",
            "kind",
            "eventId",
            "projectId",
            "sectionId",
            "participationId",
            "agentId",
            "nodeId",
            "turnId",
            "requesterAccountId",
            "payerAccountId",
            "payerBindingRef",
            "fence",
            "sequence",
            "expiresAt",
            "payloadDigest",
            "payload",
        ],
    )?;
    let kind = text(f, "kind");
    if f["v"] != 1
        || f["nodeId"] != principal["nodeId"]
        || !matches!(
            kind,
            "turn.admitted"
                | "turn.partial"
                | "turn.final"
                | "turn.failed"
                | "turn.approval.request"
                | "turn.effect.receipt"
        )
    {
        return Err(denied());
    }
    let project = id(&f["projectId"])?;
    let turn = id(&f["turnId"])?;
    let event = id(&f["eventId"])?;
    let sequence = revision(&f["sequence"])?;
    let row = one(
        db,
        "SELECT * FROM turns WHERE project_id=? AND id=? AND node_id=?",
        [project.as_str(), turn.as_str(), text(principal, "nodeId")],
    )?
    .ok_or_else(missing)?;
    let request = stored(&row, "request_json")?;
    for key in [
        "projectId",
        "sectionId",
        "participationId",
        "agentId",
        "nodeId",
        "turnId",
        "requesterAccountId",
        "payerAccountId",
        "payerBindingRef",
        "fence",
        "expiresAt",
    ] {
        if f[key] != request[key] {
            return Err(denied());
        }
    }
    current(db, &request)?;
    let encoded = canonical(f);
    let digest_value = digest(&encoded);
    if let Some(prior) = one(
        db,
        "SELECT digest,event_id FROM node_results WHERE project_id=? AND turn_id=? AND sequence=?",
        params![project, turn, sequence],
    )? {
        if prior["digest"] != digest_value || prior["event_id"] != event {
            return Err(conflict());
        }
        return Ok(json!({"accepted":true,"duplicate":true,"sequence":sequence}));
    }
    let payload = &f["payload"];
    match kind {
        "turn.partial" | "turn.final" => {
            closed(payload, &["text"])?;
            message(
                &payload["text"],
                if kind == "turn.partial" {
                    8_000
                } else {
                    16_000
                },
            )?;
        }
        "turn.failed" => {
            closed(payload, &["code"])?;
            let code = text(payload, "code");
            if code.is_empty()
                || code.len() > 80
                || !code
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            {
                return Err(denied());
            }
        }
        "turn.approval.request" => {
            closed(
                payload,
                &[
                    "actionId",
                    "actionDigest",
                    "requiredApproverAccountId",
                    "expiresAt",
                ],
            )?;
            id(&payload["actionId"])?;
            if !hex(text(payload, "actionDigest"), 64)
                || payload["requiredApproverAccountId"] != request["payerAccountId"]
                || integer(&payload["expiresAt"], now())
                    .is_none_or(|v| Some(v) > request["expiresAt"].as_i64())
            {
                return Err(denied());
            }
        }
        "turn.effect.receipt" => {
            closed(payload, &["actionId", "actionDigest", "outcome", "leaseId"])?;
            if !matches!(text(payload,"outcome"),"succeeded"|"failed"|"unknown") || !hex(text(payload,"actionDigest"),64) ||
                !db.prepare("SELECT 1 FROM action_leases WHERE id=? AND project_id=? AND turn_id=? AND action_id=? AND action_digest=? AND fence_json=?")?
                .exists(params![id(&payload["leaseId"])?,project,turn,id(&payload["actionId"])?,text(payload,"actionDigest"),canonical(&request["fence"])])? { return Err(denied()); }
        }
        _ => closed(payload, &[])?,
    }
    if f["payloadDigest"] != digest(&canonical(payload)) {
        return Err(Failure::new(
            409,
            "digest_mismatch",
            "Payload digest differs.",
        ));
    }
    if db
        .prepare("SELECT 1 FROM node_results WHERE project_id=? AND event_id=?")?
        .exists([&project, &event])?
    {
        return Err(conflict());
    }
    if request["expiresAt"].as_i64().is_none_or(|v| v <= now())
        || Some(sequence - 1) != row["result_sequence"].as_i64()
        || !matches!(text(&row, "state"), "pending" | "admitted")
    {
        return Err(stale());
    }
    if (row["state"] == "pending") != (kind == "turn.admitted") {
        return Err(denied());
    }
    if !db
        .prepare("SELECT 1 FROM participations WHERE project_id=? AND id=? AND state='active'")?
        .exists(params![project, text(&request, "participationId")])?
    {
        return Err(stale());
    }
    if kind == "turn.admitted" {
        let (active, owned): (i64, i64) = db.query_row(
            "SELECT count(*),coalesce(sum(payer_account_id=?),0) FROM turns WHERE state='admitted'",
            [text(principal, "accountId")],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if active >= 16 || owned >= 4 {
            return Err(rate_limited());
        }
    }
    let published: usize = db.query_row("SELECT coalesce(sum(length(CAST(json_extract(frame_json,'$.payload.text') AS BLOB))),0) FROM node_results WHERE project_id=? AND turn_id=?", [&project,&turn], |r| r.get(0))?;
    if published + text(payload, "text").len() > 262_144 {
        return Err(Failure::new(
            413,
            "result_limit",
            "Turn publication limit reached.",
        ));
    }
    db.execute(
        "INSERT INTO node_results VALUES(?,?,?,?,?,?)",
        params![project, turn, sequence, event, digest_value, encoded],
    )?;
    let state = match kind {
        "turn.final" => "done",
        "turn.failed" => "failed",
        _ => "admitted",
    };
    db.execute(
        "UPDATE turns SET state=?,result_sequence=? WHERE project_id=? AND id=?",
        params![state, sequence, project, turn],
    )?;
    Ok(json!({"accepted":true,"duplicate":false,"sequence":sequence}))
}
