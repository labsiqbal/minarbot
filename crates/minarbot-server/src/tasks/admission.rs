use super::*;
pub fn validate(v: &Value) -> Result<()> {
    fields(
        v,
        &[
            "requestId",
            "projectId",
            "agentId",
            "homeDeviceId",
            "payload",
            "payloadDigest",
            "expectedRevisions",
        ],
    )?;
    for key in ["requestId", "projectId", "agentId", "homeDeviceId"] {
        id(&v[key])?;
    }
    let p = &v["payload"];
    fields(
        p,
        &["v", "kind", "threadId", "channelId", "routing", "message"],
    )?;
    if p["v"] != 1 || !matches!(string(p, "kind"), "direct" | "side" | "channel") {
        return Err(fail(400, "invalid_task_payload", "Invalid task payload."));
    }
    let invalid = match string(p, "kind") {
        "direct" => ["threadId", "channelId", "routing"]
            .iter()
            .any(|k| p.get(k).is_some()),
        "side" => {
            p.get("channelId").is_some()
                || p.get("routing").is_some()
                || p.get("threadId").is_none()
        }
        _ => {
            p.get("threadId").is_some()
                || p.get("channelId").is_none()
                || !matches!(string(p, "routing"), "assigned" | "automatic")
        }
    };
    if invalid {
        return Err(fail(
            400,
            "invalid_task_payload",
            match string(p, "kind") {
                "direct" => "Invalid direct conversation payload.",
                "side" => "Invalid side thread payload.",
                _ => "Invalid channel payload.",
            },
        ));
    }
    for key in ["threadId", "channelId"] {
        if let Some(v) = p.get(key) {
            id(v)?;
        }
    }
    let m = &p["message"];
    fields(m, &["text", "files"])?;
    text(&m["text"], 16000, "Message")?;
    let files = m["files"]
        .as_array()
        .ok_or_else(|| fail(400, "invalid_attachments", "files must be an array."))?;
    let parsed = files.iter().map(id).collect::<Result<Vec<_>>>()?;
    if parsed.len() > 4
        || parsed
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != parsed.len()
    {
        return Err(fail(
            400,
            "invalid_attachments",
            "Maximum of four different files.",
        ));
    }
    let r = &v["expectedRevisions"];
    fields(
        r,
        &[
            "project",
            "agent",
            "homeBinding",
            "membership",
            "assignment",
            "capability",
            "thread",
            "channel",
        ],
    )?;
    for key in [
        "project",
        "agent",
        "homeBinding",
        "membership",
        "assignment",
        "capability",
    ] {
        revision(&r[key], 1)?;
    }
    for key in ["thread", "channel"] {
        if let Some(v) = r.get(key) {
            revision(v, 1)?;
        }
    }
    if (p["kind"] == "side") != r.get("thread").is_some() {
        return Err(fail(
            400,
            "invalid_revision_set",
            "Side thread revision does not match the payload.",
        ));
    }
    if (p["kind"] == "channel") != r.get("channel").is_some() {
        return Err(fail(
            400,
            "invalid_revision_set",
            "Channel revision does not match the payload.",
        ));
    }
    if hex(&v["payloadDigest"], "payloadDigest")?
        != digest(&json!([
            "minarbot-task-payload-v1",
            v["requestId"],
            v["projectId"],
            v["agentId"],
            v["homeDeviceId"],
            p,
            r
        ]))
    {
        return Err(fail(
            409,
            "payload_digest_mismatch",
            "Payload digest does not match.",
        ));
    }
    Ok(())
}
pub fn admit(db: &Connection, account: &str, v: &Value, now: i64) -> Result<Value> {
    validate(v)?;
    let request = id(&v["requestId"])?;
    let project = id(&v["projectId"])?;
    let agent = id(&v["agentId"])?;
    let device = id(&v["homeDeviceId"])?;
    let payload = &v["payload"];
    let pd = hex(&v["payloadDigest"], "payloadDigest")?;
    let m = authority::member(db, account, &project, false)?;
    let assignment = authority::assignment(db, account, &project, &agent, &m)?;
    let (table, root_kind, root_id) = match string(payload, "kind") {
        "side" => ("threads", "thread", string(payload, "threadId")),
        "channel" => ("channels", "channel", string(payload, "channelId")),
        _ => ("agents", "agent", agent.as_str()),
    };
    if table!="agents"&&!exists(db,"SELECT 1 FROM record_scopes WHERE table_name=? AND key_json=? AND project=? AND root_kind=? AND root_id=? AND (visibility='project' OR account=?)",params![table,json!([root_id]).to_string(),project,root_kind,root_id,account])?{return Err(fail(404,"resource_not_found","Conversation not found."))}
    let scope = format!("tasks:admit:{project}");
    if let Some(old) = replay(db, account, &project, &request, &scope, &pd, true)? {
        return Ok(old);
    }
    let part=one(db,"SELECT p.*,a.revision agent_revision FROM agent_projects p JOIN agents a ON a.id=p.agent WHERE p.project=? AND p.agent=? AND p.deleted=0",[&project,&agent])?.ok_or_else(missing)?;
    let owner = one(
        db,
        "SELECT * FROM agent_authority WHERE agent=? AND owner_account=? AND home_device=?",
        [&agent, string(&m, "account"), &device],
    )?
    .ok_or_else(missing)?;
    let d=one(db,"SELECT d.* FROM devices d JOIN installations i ON i.id=d.installation JOIN accounts a ON a.id=d.account WHERE d.id=? AND d.account=? AND i.account=d.account AND i.revoked=0 AND a.blocked=0 AND d.status NOT IN('revoked','blocked')",[&device,string(&m,"account")])?.ok_or_else(missing)?;
    if !data::json(&d["capabilities_json"])?
        .as_array()
        .is_some_and(|c| c.contains(&json!("execution")))
    {
        return Err(fail(
            422,
            "capability_missing",
            "Device does not support execution.",
        ));
    }
    let expected = &v["expectedRevisions"];
    if expected["project"] != m["revision"]
        || expected["agent"] != part["agent_revision"]
        || expected["homeBinding"] != owner["revision"]
        || expected["membership"] != m["grant_revision"]
        || expected["assignment"] != assignment
        || expected["capability"] != part["revision"]
    {
        return Err(fail(409, "stale_revision", "Authority changed."));
    }
    if table != "agents" {
        let root = one(db, &format!("SELECT * FROM {table} WHERE id=?"), [root_id])?
            .ok_or_else(missing)?;
        if root["deleted"] == 1 || table == "threads" && root["agent"] != agent {
            return Err(fail(404, "resource_not_found", "Side thread not found."));
        }
        if root["archived"] == 1 {
            return Err(fail(
                409,
                "conversation_archived",
                if table == "threads" {
                    "Side thread is archived."
                } else {
                    "Channel is archived."
                },
            ));
        }
        if root["revision"] != expected[root_kind] {
            return Err(fail(
                409,
                "stale_revision",
                if table == "threads" {
                    "Side thread changed."
                } else {
                    "Channel changed."
                },
            ));
        }
        if payload["routing"] == "automatic" && root["lead"] != agent {
            return Err(fail(
                409,
                "routing_unavailable",
                "Agent is not the channel lead.",
            ));
        }
        if payload["routing"] == "assigned"
            && !exists(
                db,
                "SELECT 1 FROM channel_members WHERE channel=? AND agent=?",
                [root_id, &agent],
            )?
        {
            return Err(fail(
                404,
                "resource_not_found",
                "Agent is not a channel member.",
            ));
        }
    }
    for file in payload["message"]["files"]
        .as_array()
        .ok_or_else(Failure::storage)?
    {
        if !exists(db,"SELECT 1 FROM record_scopes WHERE table_name='files' AND key_json=? AND project=? AND (visibility='project' OR account=?)",params![json!([file]).to_string(),project,account])?{return Err(fail(404,"resource_not_found","File not found."))}
    }
    let binding = if owner["provider_binding"].is_null() {
        None
    } else {
        Some(
            one(
                db,
                "SELECT * FROM provider_bindings WHERE id=? AND device=? AND owner_account=?",
                [
                    string(&owner, "provider_binding"),
                    &device,
                    string(&m, "account"),
                ],
            )?
            .ok_or_else(|| fail(404, "resource_not_found", "Provider binding not found."))?,
        )
    };
    if let Some((accounts, true)) =
        crate::computer::devices::provider_instances(&d, string(&m, "account"))?
    {
        if binding.as_ref().is_some_and(|binding| {
            data::agents::device_binding_account(&accounts, binding).is_none()
        }) {
            let device = crate::computer::devices::device_label(db, &d, string(&m, "account"))?;
            return Err(fail(
                409,
                "provider_unavailable",
                &format!("No provider on {device}"),
            ));
        }
    }
    data::limits::task_capacity(db, &project, &agent)?;
    let state = if d["status"] == "ready" {
        "queued"
    } else {
        "waiting_device"
    };
    db.execute("INSERT INTO turns(id,agent,thread,channel,recipient,automatic,text,attachments,status,created,profile_setup) VALUES(?,?,?,?,?,?,?,?,'queued',?,0)",params![request,agent,payload["threadId"].as_str(),payload["channelId"].as_str(),if payload["routing"]=="assigned"{Some(agent.as_str())}else{None},i64::from(payload["routing"]=="automatic"),string(&payload["message"],"text"),payload["message"]["files"].to_string(),data::iso(db,now)?])?;
    db.execute(
        "INSERT INTO record_scopes VALUES('turns',?,?,?,?,?,?)",
        params![
            json!([request]).to_string(),
            account,
            project,
            if root_kind == "channel" {
                "project"
            } else {
                "owner"
            },
            root_kind,
            root_id
        ],
    )?;
    let cap = data::json(&part["capabilities_json"])?;
    db.execute("INSERT INTO tasks(id,logical_turn,project,agent,initiator_account,home_device,assignment_revision,capability_revision,capability_digest,capability_snapshot_json,provider_binding_id,provider_binding_revision,provider_binding_owner,state,payload_digest,payload_json,revision,created) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,1,?)",params![request,request,project,agent,account,device,assignment,n(&part,"revision"),digest(&json!(["minarbot-capabilities-v1",cap])),canonical(&cap),binding.as_ref().and_then(|b|b["id"].as_str()),binding.as_ref().map(|b|n(b,"revision")),binding.as_ref().and_then(|b|b["owner_account"].as_str()),state,pd,canonical(payload),now])?;
    let task = task(db, &request)?;
    observe(db, &task, now)?;
    Save{account,task:&task,request:&request,scope:&scope,payload:&pd,now}.save(db,&request,1,202,json!({"taskId":request,"state":state,"revision":1,"homeDeviceId":device,"payloadDigest":pd}),None)
}
