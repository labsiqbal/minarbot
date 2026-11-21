use super::*;
pub(super) mod runtime;
pub(super) mod validation;
pub use runtime::settle;
fn read(db: &Connection, id: &str) -> Result<Value> {
    one(db, "SELECT * FROM workflows WHERE id=?", [id])?
        .ok_or_else(|| error(404, "Workflow not found."))
}
fn active(db: &Connection, id: &str) -> Result<Value> {
    let w = read(db, id)?;
    if w["deleted"] == 1 {
        return Err(error(404, "Workflow was deleted."));
    }
    Ok(w)
}
fn graph(db: &Connection, id: &str, rev: i64) -> Result<Value> {
    let r = one(
        db,
        "SELECT graph FROM workflow_versions WHERE workflow=? AND revision=?",
        params![id, rev],
    )?
    .ok_or_else(|| error(404, "Workflow version not found."))?;
    data::json(&r["graph"])
}
fn present(db: &Connection, mut r: Value) -> Result<Value> {
    r["graph"] = graph(db, string(&r, "id"), r["revision"].as_i64().unwrap_or(1))?;
    Ok(r)
}
fn save(
    db: &Connection,
    id: &str,
    owner: &str,
    name: &str,
    revision: &Value,
    g: &Value,
) -> Result<Value> {
    if !integer(revision, 0, 9007199254740991) {
        return Err(error(400, "Invalid workflow revision."));
    }
    let old = one(db, "SELECT * FROM workflows WHERE id=?", [id])?;
    if old.as_ref().is_some_and(|r| r["deleted"] == 1) {
        return Err(error(404, "Workflow was deleted."));
    }
    if old.as_ref().is_some_and(|r| r["agent"] != owner) {
        return Err(error(404, "Workflow not found."));
    }
    if old
        .as_ref()
        .map(|r| r["revision"].clone())
        .unwrap_or(json!(0))
        != *revision
    {
        return Err(error(409, "Workflow changed. Reload before saving."));
    }
    let g = validation::validate(db, g)?;
    for n in g["nodes"].as_array().ok_or_else(Failure::storage)? {
        if n["kind"] == "output" {
            if let Some(c) = n["channel"].as_str() {
                if !runtime::channel(db, c, owner)? {
                    return Err(error(
                        409,
                        "Output channel is inactive or the owner agent is not a member.",
                    ));
                }
            }
        }
    }
    let rev = revision.as_i64().ok_or_else(Failure::storage)? + 1;
    db.execute("INSERT INTO workflows(id,agent,name,revision) VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,revision=excluded.revision",params![id,owner,name,rev])?;
    db.execute(
        "INSERT INTO workflow_versions VALUES(?,?,?)",
        params![id, rev, g.to_string()],
    )?;
    Ok(json!({"id":id,"agent":owner,"name":name,"revision":rev,"graph":g}))
}
pub(super) fn start(
    state: &AppState,
    db: &Connection,
    workflow: &str,
    id: &str,
    input: &str,
) -> Result<()> {
    let w = active(db, workflow)?;
    if data::agents::hidden(db, string(&w, "agent"))? {
        return Err(error(409, "Restore the agent before running the workflow."));
    }
    let g = graph(
        db,
        workflow,
        w["revision"].as_i64().ok_or_else(Failure::storage)?,
    )?;
    db.execute("INSERT INTO workflow_runs(id,workflow,version,agent,graph,input,last,cursor,status,created) VALUES(?,?,?,?,?,?,?,?,'ready',?)",params![id,workflow,w["revision"].as_i64(),string(&w,"agent"),g.to_string(),input,input,string(&g,"entry"),iso(db,state.config.now())?])?;
    db.execute(
        "INSERT INTO local_workflow_run_projects SELECT ?,project FROM local_workflow_schedule_projects WHERE workflow=?",
        [id, workflow],
    )?;
    Ok(())
}
#[allow(clippy::too_many_arguments)]
pub fn route(
    state: &AppState,
    db: &Connection,
    path: &str,
    p: &[&str],
    method: &Method,
    q: &[(String, String)],
    _h: &HeaderMap,
    v: &Value,
) -> Result<(u16, Value)> {
    let get = *method == Method::GET;
    let post = *method == Method::POST;
    let owner = if p.get(1) == Some(&"agents") {
        data::agents::read(db, p[2])?;
        Some(p[2])
    } else {
        None
    };
    if get && (path == "/api/workflows" || owner.is_some()) {
        let after = data::param(q, "after").unwrap_or("");
        let deleted = data::param(q, "deleted").unwrap_or("0");
        let project = data::param(q, "project");
        if q.iter()
            .any(|(k, _)| !["after", "deleted", "project"].contains(&k.as_str()))
            || project.is_some_and(|p| p != "none" && !uuid(p))
            || !["0", "1"].contains(&deleted)
            || !after.is_empty() && !uuid(after)
        {
            return Err(error(400, "Invalid workflow list position."));
        }
        let mut rows=all(db,"SELECT w.*,a.name AS owner_name,v.graph,b.project AS schedule_project FROM workflows w JOIN agents a ON a.id=w.agent JOIN workflow_versions v ON v.workflow=w.id AND v.revision=w.revision LEFT JOIN local_workflow_schedule_projects b ON b.workflow=w.id WHERE (?1 IS NULL OR w.agent=?1) AND w.deleted=?2 AND w.id>?3 AND (?4 IS NULL OR (?4='none' AND b.project IS NULL) OR ((b.project IS NULL OR b.project=?4) AND EXISTS(SELECT 1 FROM local_project_members m JOIN local_projects p ON p.id=m.project WHERE m.project=?4 AND m.agent=w.agent AND p.archived=0 AND p.deleted=0))) ORDER BY w.id LIMIT 21",params![owner,deleted,after,project])?;
        for row in &mut rows {
            parse_graph(row)?;
        }
        let next = if rows.len() > 20 {
            rows[19]["id"].clone()
        } else {
            Value::Null
        };
        rows.truncate(20);
        return Ok((200, json!({"workflows":rows,"next_after":next})));
    }
    if get && p.get(1) == Some(&"workflows") {
        let id = p[2];
        let w = read(db, id)?;
        return Ok((
            200,
            match p.get(3).copied() {
                Some("versions") => {
                    let before = cursor(q, "before", "Invalid workflow version position.")?;
                    let mut rows=all(db,"SELECT revision,graph FROM workflow_versions WHERE workflow=? AND revision<? ORDER BY revision DESC LIMIT 21",params![id,before])?;
                    for r in &mut rows {
                        parse_graph(r)?;
                    }
                    let next = if rows.len() > 20 {
                        rows[19]["revision"].clone()
                    } else {
                        Value::Null
                    };
                    rows.truncate(20);
                    json!({"versions":rows,"next_before":next})
                }
                Some("runs") => {
                    let before = cursor(q, "before", "Invalid run history position.")?;
                    json!(all(db,"SELECT rowid AS seq,id,version,status,cursor,steps,revision,error,created,substr(last,1,1000) AS result FROM workflow_runs WHERE workflow=? AND rowid<? ORDER BY rowid DESC LIMIT 20",params![id,before])?)
                }
                Some("schedule") => {
                    json!({"schedule":read_schedule(db,id)?,"occurrences":all(db,"SELECT * FROM workflow_occurrences WHERE workflow=? ORDER BY scheduled_at DESC LIMIT 20",[id])?})
                }
                _ => present(db, w)?,
            },
        ));
    }
    if get && p.get(1) == Some(&"workflow-runs") {
        let mut r = one(db, "SELECT * FROM workflow_runs WHERE id=?", [p[2]])?
            .ok_or_else(|| error(404, "Workflow run not found."))?;
        parse_graph(&mut r)?;
        r["steps"]=json!(all(db,"SELECT s.*,t.status AS task_status,t.paused,t.agent,CASE WHEN th.deleted=0 THEN t.thread END AS thread FROM workflow_steps s LEFT JOIN turns t ON t.id=s.turn LEFT JOIN threads th ON th.id=t.thread WHERE run=? ORDER BY s.seq",[p[2]])?);
        return Ok((200, r));
    }
    if get && path == "/api/workflow-notifications" {
        let before = cursor(q, "before", "Invalid notification position.")?;
        let mut rows = all(
            db,
            "SELECT * FROM workflow_notifications WHERE seq<? ORDER BY seq DESC LIMIT 21",
            [before],
        )?;
        let next = if rows.len() > 20 {
            rows[19]["seq"].clone()
        } else {
            Value::Null
        };
        rows.truncate(20);
        let unread: i64 = db.query_row(
            "SELECT count(*) FROM workflow_notifications WHERE read=0",
            [],
            |r| r.get(0),
        )?;
        return Ok((
            200,
            json!({"notifications":rows,"next_before":next,"unread":unread}),
        ));
    }
    if !post {
        return Err(error(404, "Route not found."));
    }
    if p.get(1) == Some(&"workflow-notifications") {
        if !data::closed(v, &["id"]) {
            return Err(error(400, "Invalid read request."));
        }
        if db.execute(
            "UPDATE workflow_notifications SET read=1 WHERE run=?",
            [p[2]],
        )? == 0
        {
            return Err(error(404, "Notification not found."));
        }
        return Ok((200, json!({"run":p[2],"read":1})));
    }
    let target = if owner.is_some() {
        Some(string(v, "id").to_owned())
    } else if p.get(1) == Some(&"workflow-runs") {
        one(db, "SELECT workflow FROM workflow_runs WHERE id=?", [p[2]])?
            .map(|r| string(&r, "workflow").to_owned())
    } else {
        Some(p[2].to_owned())
    };
    if p.last() != Some(&"delete") {
        if let Some(id) = target.as_deref() {
            if one(db, "SELECT deleted FROM workflows WHERE id=?", [id])?
                .is_some_and(|r| r["deleted"] == 1)
            {
                return Err(error(404, "Workflow was deleted."));
            }
        }
    }
    command(db,"workflow_commands",path,v,||{
        if let Some(owner)=owner{return save(db,string(v,"id"),owner,&field(v,"name",80)?,&v["revision"],&v["graph"]);}
        if p.get(1)==Some(&"workflows") {let id=p[2];let w=active(db,id)?;
            match p.get(3).copied(){
                Some("edit")=>return save(db,id,string(&w,"agent"),&field(v,"name",80)?,&v["revision"],&v["graph"]),
                Some("restore")=>{if !data::closed(v,&["id","revision","version"])||!integer(&v["version"],1,9007199254740991){return Err(error(400,"Invalid restore version."));}return save(db,id,string(&w,"agent"),string(&w,"name"),&v["revision"],&graph(db,id,v["version"].as_i64().ok_or_else(Failure::storage)?)?);},
                Some("run")=>{if w["revision"]!=v["revision"]{return Err(error(409,"Workflow changed. Reload before running."));}let input=v["input"].as_str().filter(|s|s.encode_utf16().count()<=16000).ok_or_else(||error(400,"Invalid workflow input."))?;if queue_count(db)? >=100{return Err(error(429,"Workflow queue is full."));}start(state,db,id,string(v,"id"),input)?;return Ok(json!({"id":v["id"]}));},
                Some("delete")=>{if !data::closed(v,&["id","revision"]){return Err(error(400,"Invalid workflow deletion request."));}
if w["revision"]!=v["revision"]{return Err(error(409,"Workflow changed. Reload before deleting."));}
if data::exists(db,"SELECT 1 FROM workflow_runs WHERE workflow=? AND status IN('ready','waiting','approval')",[id])?{return Err(error(409,"Stop active runs before deleting the workflow."));}
if let Some(h)=one(db,"SELECT route,revision FROM workflow_webhooks WHERE workflow=?",[id])?{db.execute("INSERT INTO workflow_webhook_revocations VALUES(?,?,?)",params![string(&h,"route"),id,h["revision"].as_i64().unwrap_or(0)+1])?;}db.execute("DELETE FROM workflow_webhooks WHERE workflow=?",[id])?;db.execute("UPDATE workflow_schedules SET enabled=0,revision=revision+1 WHERE workflow=?",[id])?;db.execute("UPDATE workflows SET deleted=1 WHERE id=?",[id])?;return Ok(json!({"id":id,"deleted":1}));},
                Some("schedule")=>return save_schedule(db,id,v),
                _=>()
            }
        }
        if p.get(1)==Some(&"workflow-runs") {let id=p[2];let run=one(db,"SELECT * FROM workflow_runs WHERE id=?",[id])?.ok_or_else(||error(404,"Workflow run not found."))?;
            match p.get(3).copied(){
                Some("approve")=>{if !integer(&v["revision"],0,9007199254740991)||!v["accepted"].is_boolean(){return Err(error(400,"Invalid workflow decision."));}
if run["status"]!="approval"||run["revision"]!=v["revision"]{return Err(error(409,"Workflow run changed."));}let n=runtime::node(&run)?;let yes=v["accepted"]==true;db.execute("UPDATE workflow_steps SET status=?,detail=? WHERE run=? AND seq=?",params![if yes{"done"}else{"failed"},if yes{"approved"}else{"rejected"},id,run["steps"].as_i64()])?;db.execute("UPDATE workflow_runs SET status=?,cursor=?,error=?,revision=revision+1 WHERE id=?",params![if yes{"ready"}else{"failed"},if yes{string(&n,"yes")}else{string(&run,"cursor")},if yes{None}else{Some("Workflow approval denied.")},id])?;},
                Some("cancel")=>{if run["revision"]!=v["revision"]||["done","failed","cancelled"].contains(&string(&run,"status")){return Err(error(409,"Workflow run changed."));}db.execute("WITH RECURSIVE tree(id) AS(SELECT turn FROM workflow_steps WHERE run=? AND turn IS NOT NULL UNION SELECT t.id FROM turns t JOIN tree ON t.parent=tree.id) UPDATE turns SET status='cancelled',paused=0,revision=revision+1,error='Workflow cancelled.' WHERE id IN(SELECT id FROM tree) AND status NOT IN('done','failed','cancelled')",[id])?;db.execute("UPDATE workflow_runs SET status='cancelled',revision=revision+1 WHERE id=?",[id])?;db.execute("UPDATE workflow_steps SET status='cancelled',detail='Workflow cancelled.' WHERE run=? AND status IN('waiting','approval')",[id])?;},
                Some("retry")=>runtime::retry(state,db,&run,v)?,
                _=>return Err(error(404,"Route not found."))
            }return Ok(json!({"id":id}));
        }
        Err(error(404,"Route not found."))
    }).map(|v|(200,v))
}
fn queue_count(db: &Connection) -> Result<i64> {
    Ok(db.query_row(
        "SELECT count(*) FROM workflow_runs WHERE status IN('ready','waiting','approval')",
        [],
        |r| r.get(0),
    )?)
}
fn read_schedule(db: &Connection, id: &str) -> Result<Option<Value>> {
    one(
        db,
        "SELECT s.*,b.project FROM workflow_schedules s LEFT JOIN local_workflow_schedule_projects b USING(workflow) WHERE s.workflow=?",
        [id],
    )
}
fn save_schedule(db: &Connection, id: &str, v: &Value) -> Result<Value> {
    let old = read_schedule(db, id)?;
    if !data::closed(
        v,
        &[
            "id",
            "revision",
            "nextAt",
            "intervalSeconds",
            "input",
            "enabled",
            "project",
        ],
    ) || !integer(&v["revision"], 0, 9007199254740991)
        || !integer(&v["nextAt"], 1, 253370764799999)
        || !v["enabled"].is_boolean()
        || !v["project"].is_null() && !v["project"].as_str().is_some_and(uuid)
        || v["input"]
            .as_str()
            .is_none_or(|s| s.encode_utf16().count() > 16000)
        || !v
            .get("intervalSeconds")
            .is_some_and(|n| n.is_null() || integer(n, 60, 31536000))
    {
        return Err(error(400, "Invalid workflow schedule."));
    }
    if let Some(project) = v["project"].as_str() {
        if !data::exists(db,"SELECT 1 FROM local_projects p JOIN local_project_members m ON m.project=p.id JOIN workflows w ON w.agent=m.agent WHERE p.id=? AND w.id=? AND p.deleted=0 AND p.archived=0",[project,id])? {
            return Err(error(403, "Workflow owner must be a member of an active project."));
        }
    }
    if old
        .as_ref()
        .map(|r| r["revision"].clone())
        .unwrap_or(json!(0))
        != v["revision"]
    {
        return Err(error(409, "Schedule changed. Reload before saving."));
    }
    if v["enabled"] == true
        && data::exists(
            db,
            "SELECT 1 FROM workflow_occurrences WHERE workflow=? AND scheduled_at=?",
            params![id, v["nextAt"].as_i64()],
        )?
    {
        return Err(error(
            409,
            "Scheduled occurrence was already processed. Choose a new time.",
        ));
    }
    db.execute("INSERT INTO workflow_schedules VALUES(?,?,?,?,?,?) ON CONFLICT(workflow) DO UPDATE SET revision=excluded.revision,next_at=excluded.next_at,interval_ms=excluded.interval_ms,input=excluded.input,enabled=excluded.enabled",params![id,v["revision"].as_i64().unwrap_or(0)+1,v["nextAt"].as_i64(),v["intervalSeconds"].as_i64().map(|n|n*1000),string(v,"input"),i64::from(v["enabled"]==true)])?;
    if let Some(project) = v["project"].as_str() {
        db.execute("INSERT INTO local_workflow_schedule_projects VALUES(?,?) ON CONFLICT(workflow) DO UPDATE SET project=excluded.project", [id, project])?;
    } else if v.get("project").is_some() {
        db.execute(
            "DELETE FROM local_workflow_schedule_projects WHERE workflow=?",
            [id],
        )?;
    }
    read_schedule(db, id)?.ok_or_else(Failure::storage)
}
