use super::{error, machines, Result};
use crate::{
    files::{keys, one, rows, text},
    web::Failure,
    AppState,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{collections::HashSet, time::Duration};

fn db<T>(state: &AppState, work: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    work(&guard.connection)
}
fn transaction<T>(state: &AppState, work: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    db(state, |db| super::transaction(db, work))
}
pub fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch("UPDATE turns SET status='interrupted',paused=0,error='Remote send is uncertain after restart. It will not be replayed.' WHERE id IN(SELECT delivery FROM remote_agent_calls WHERE state='sending'); UPDATE remote_agent_calls SET state='uncertain',status='interrupted',error='Remote send is uncertain after restart. It will not be replayed.' WHERE state='sending'; UPDATE turns SET status='queued',paused=0,error=NULL WHERE id IN(SELECT delivery FROM remote_agent_calls WHERE state='sent') AND status='interrupted'; UPDATE turns SET status='queued',paused=0,error=NULL WHERE status='interrupted' AND (EXISTS(SELECT 1 FROM agent_requests WHERE turn=turns.id AND expects_reply=1 AND settled=1) OR EXISTS(SELECT 1 FROM delegation_tasks d JOIN turns child ON child.id=d.delivery WHERE d.creator_turn=turns.id AND child.status='done' AND d.wake_delivered=0));")?;
    Ok(())
}
fn role(value: &Value) -> String {
    let cleaned: String = value
        .as_str()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '\u{2028}' | '\u{2029}'))
        .collect();
    let mut units = 0;
    cleaned
        .trim()
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= 60
        })
        .collect()
}
pub fn roster(db: &Connection, online_only: bool) -> Result<Vec<Value>> {
    let machines = rows(db,"SELECT environment_id,name,status,agents FROM paired_machines WHERE state='paired' AND (?=0 OR status='online') ORDER BY environment_id LIMIT 100",[online_only as i64])?;
    let mut result = Vec::new();
    for machine in machines {
        let agents: Value =
            serde_json::from_str(text(&machine, "agents")).map_err(|_| Failure::storage())?;
        for agent in agents.as_array().into_iter().flatten().take(500) {
            result.push(json!({"id":format!("{}@{}",text(agent,"id"),text(&machine,"environment_id")),"name":format!("{} on {}",text(agent,"name"),text(&machine,"name")),"role":role(&agent["role"]),"description":"Paired machine agent. Only your explicit message is shared.","status":agent["status"],"machine":machine["name"],"environmentId":machine["environment_id"],"online":machine["status"]=="online","agent":agent["id"],"agentName":agent["name"],"model":agent["model"]}));
        }
    }
    result.sort_by(|a, b| text(a, "id").cmp(text(b, "id")));
    Ok(result)
}
pub fn resolve(db: &Connection, address: &str) -> Result<Option<Value>> {
    let Some((agent, host)) = address.rsplit_once('@').filter(|(a, _)| !a.is_empty()) else {
        return Ok(None);
    };
    let host = host.to_lowercase();
    let exact=one(db,"SELECT environment_id,name,agents FROM paired_machines WHERE state='paired' AND environment_id=?",[&host])?;
    let machines = if let Some(exact) = exact {
        vec![exact]
    } else {
        rows(db,"SELECT environment_id,name,agents FROM paired_machines WHERE state='paired' AND lower(name)=? LIMIT 2",[&host])?
    };
    if machines.len() != 1 {
        return Ok(None);
    }
    let machine = &machines[0];
    let agents: Vec<Value> =
        serde_json::from_str(text(machine, "agents")).map_err(|_| Failure::storage())?;
    let candidates = if let Some(exact) = agents.iter().find(|v| v["id"] == agent) {
        vec![exact]
    } else {
        agents
            .iter()
            .filter(|v| text(v, "name").to_lowercase() == agent.to_lowercase())
            .collect()
    };
    Ok((candidates.len() == 1).then(
        || json!({"kind":"remote","machine":machine["environment_id"],"agent":candidates[0]["id"]}),
    ))
}
pub fn messaging_allowed(db: &Connection, peer: &str) -> Result<bool> {
    Ok(one(
        db,
        "SELECT allow_agent_messaging FROM paired_machines WHERE local_token_id=?",
        [peer],
    )?
    .is_none_or(|v| v["allow_agent_messaging"] != 0)
        && one(
            db,
            "SELECT 1 FROM peer_revocations WHERE token_id=?",
            [peer],
        )?
        .is_none())
}
pub fn enforce_peer_rate(db: &Connection, peer: &str, now: i64, limits: &Value) -> Result<()> {
    let rate = limits["rate"].as_i64().unwrap_or(20);
    let window = limits["rateWindowMs"].as_i64().unwrap_or(300000);
    let cooldown = limits["cooldownMs"].as_i64().unwrap_or(600000);
    if one(
        db,
        "SELECT until FROM peer_message_cooldowns WHERE peer=?",
        [peer],
    )?
    .and_then(|v| v["until"].as_i64())
    .is_some_and(|until| until > now)
    {
        return Err(error(429, "Peer message rate limit is cooling down."));
    }
    db.execute(
        "DELETE FROM peer_message_events WHERE created<=?",
        [now - window],
    )?;
    let count: i64 = db.query_row(
        "SELECT count(*) FROM peer_message_events WHERE peer=?",
        [peer],
        |r| r.get(0),
    )?;
    if count >= rate {
        db.execute("INSERT INTO peer_message_cooldowns VALUES(?,?) ON CONFLICT(peer) DO UPDATE SET until=excluded.until",params![peer,now+cooldown])?;
        return Err(error(
            429,
            "Peer message rate limit reached. Wait for cooldown.",
        ));
    }
    db.execute(
        "INSERT INTO peer_message_events VALUES(?,?,?)",
        params![uuid::Uuid::new_v4().to_string(), peer, now],
    )?;
    Ok(())
}
pub fn journal(db: &Connection, input: &Value, now: i64) -> Result<String> {
    let id = input["id"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    db.execute("INSERT INTO remote_agent_calls(id,machine,agent,name,text,depth,delivery,state,created) VALUES(?,?,?,?,?,?,?,'intent',?)",params![id,text(input,"machine"),text(input,"agent"),text(input,"name"),text(input,"text"),input["depth"].as_i64().unwrap_or(1),input["delivery"].as_str(),now])?;
    if let Some(delivery) = input["delivery"].as_str() {
        if let Some(row) = one(db, "SELECT thread FROM turns WHERE id=?", [delivery])?
            .filter(|v| v["thread"].is_string())
        {
            let machine = one(
                db,
                "SELECT name FROM paired_machines WHERE environment_id=?",
                [text(input, "machine")],
            )?
            .map(|v| text(&v, "name").to_owned())
            .unwrap_or_else(|| text(input, "machine").to_owned());
            let sender = text(input, "name")
                .strip_suffix(&format!(" on {machine}"))
                .unwrap_or(text(input, "name"));
            db.execute("UPDATE delivery_conversations SET sender=?,machine=?,activity=?,settled_at=NULL WHERE thread=?",params![sender,machine,now,text(&row,"thread")])?;
            let title = format!(
                "{} on {}: {}",
                sender.chars().take(28).collect::<String>(),
                machine.chars().take(24).collect::<String>(),
                text(input, "text")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .chars()
            .take(80)
            .collect::<String>();
            db.execute(
                "UPDATE threads SET name=? WHERE id=? AND revision=1",
                params![title, text(&row, "thread")],
            )?;
        }
    }
    Ok(id)
}
async fn follow(state: &AppState, id: &str) -> Result<()> {
    let mut row=db(state,|db|one(db,"SELECT r.*,t.status AS local_status FROM remote_agent_calls r LEFT JOIN turns t ON t.id=r.delivery WHERE r.id=?",[id]))?.ok_or_else(Failure::storage)?;
    let operation=async {
        if row["local_status"]=="cancelled" || row["cancel_requested"]==1 {
            db(state,|db|{db.execute("UPDATE remote_agent_calls SET cancel_requested=1 WHERE id=?",[id])?;Ok(())})?;
            if row["state"]=="intent" {
                db(state,|db|{db.execute("UPDATE remote_agent_calls SET state='result',cancel_sent=1 WHERE id=?",[id])?;Ok(())})?;
                return Ok(());
            }
            if row["cancel_sent"]!=1 {
                machines::call_tool(state,text(&row,"machine"),"cancel_message",&json!({"requestId":id})).await?;
                db(state,|db|{db.execute("UPDATE remote_agent_calls SET cancel_sent=1,state='result',status='cancelled' WHERE id=?",[id])?;Ok(())})?;
            }
            return Ok(());
        }
        if row["state"]=="intent" {
            db(state,|db|{db.execute("UPDATE remote_agent_calls SET state='sending' WHERE id=? AND state='intent'",[id])?;Ok(())})?;
            let sent=machines::call_tool(state,text(&row,"machine"),"send_message",&json!({"requestId":id,"recipient":row["agent"],"text":row["text"],"metadata":{"depth":row["depth"]}})).await?;
            if !sent["turn"].is_string() || sent["requestId"]!=id{return Err(error(502,"Invalid remote receipt"))}
            db(state,|db|{db.execute("UPDATE remote_agent_calls SET state='sent',remote_turn=?,attempts=0,error=NULL WHERE id=?",params![text(&sent,"turn"),id])?;Ok(())})?;
        }
        let result=machines::call_tool(state,text(&row,"machine"),"get_message_status",&json!({"requestId":id})).await?;
        if result["requestId"]!=id || !["queued","running","waiting","done","failed","cancelled","interrupted"].contains(&text(&result,"status")) || !result["reply"].is_null()&&!result["reply"].is_string(){return Err(error(502,"Invalid remote status"))}
        let terminal=["done","failed","cancelled","interrupted"].contains(&text(&result,"status"));
        let reply=text(&result,"reply").chars().take(8000).collect::<String>();
        let message=result["error"].as_str().map(|v|v.chars().take(1000).collect::<String>());
        transaction(state,|db| {
            db.execute("UPDATE turns SET status=?,paused=?,reply=?,error=? WHERE id=? AND status<>'cancelled'",params![if terminal{text(&result,"status")}else{"queued"},!terminal as i64,if reply.is_empty(){None}else{Some(&reply)},message,row["delivery"].as_str()])?;
            db.execute("UPDATE remote_agent_calls SET state=?,status=?,reply=?,attempts=attempts+1,next_poll=?,error=? WHERE id=?",params![if terminal{"result"}else{"sent"},text(&result,"status"),if reply.is_empty(){None}else{Some(&reply)},state.config.now()+5000.min(250*(1_i64<<row["attempts"].as_u64().unwrap_or(0).min(5))),message,id])?;
            Ok(())
        })
    }.await;
    if let Err(error) = operation {
        row = db(state, |db| {
            one(db, "SELECT * FROM remote_agent_calls WHERE id=?", [id])
        })?
        .ok_or_else(Failure::storage)?;
        let attempts = row["attempts"].as_i64().unwrap_or(0);
        let next = state.config.now() + 30000.min(500 * (1_i64 << (attempts as u32).min(6)));
        if row["cancel_requested"] == 1 {
            db(state, |db| {
                db.execute("UPDATE remote_agent_calls SET state='result',status='cancelled',cancel_sent=?,attempts=attempts+1,next_poll=?,error='Remote cancellation is not confirmed. Cancellation cannot undo accepted effects.' WHERE id=?",params![(attempts>=7)as i64,next,id])?;
                Ok(())
            })?;
            return Ok(());
        }
        let rejected = [400, 401, 403, 404, 409, 429].contains(&error.status);
        let uncertain = row["state"] == "sending" && !rejected;
        let message = if rejected {
            match error.status {
                429 => "Remote agent or peer message rate limit reached. Wait for cooldown.",
                409 => "Remote delegation depth limit reached or request conflicts.",
                _ => "Remote machine is unauthorized, unpaired, or agent messaging is disabled.",
            }
        } else if uncertain {
            "Remote send is uncertain. It will not be replayed. Check the receiving machine."
        } else {
            "Remote machine is offline or unavailable. Status polling will retry."
        };
        transaction(state, |db| {
            db.execute("UPDATE remote_agent_calls SET state=?,status=?,error=?,attempts=attempts+1,next_poll=? WHERE id=?",params![if uncertain{"uncertain"}else if rejected{"result"}else{text(&row,"state")},if uncertain{"interrupted"}else if rejected{"failed"}else{text(&row,"status")},message,next,id])?;
            db.execute(
                "UPDATE turns SET status=?,paused=?,error=? WHERE id=? AND status<>'cancelled'",
                params![
                    if uncertain {
                        "interrupted"
                    } else if rejected {
                        "failed"
                    } else {
                        "queued"
                    },
                    (!uncertain && !rejected) as i64,
                    message,
                    row["delivery"].as_str()
                ],
            )?;
            Ok(())
        })?;
    }
    Ok(())
}
pub async fn worker(state: AppState) {
    if state.auth.is_some() {
        return;
    }
    let mut active = HashSet::new();
    let mut jobs = tokio::task::JoinSet::new();
    let mut timer = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            result=jobs.join_next(),if !jobs.is_empty()=>{if let Some(Ok(id))=result {active.remove(&id);}},
            _=timer.tick()=>{
                if crate::updates::pending(&state) { continue; }
                let _ = transaction(&state, |db| {
                    db.execute("UPDATE turns SET status='queued',paused=0,error=NULL WHERE status='interrupted' AND (EXISTS(SELECT 1 FROM agent_requests WHERE turn=turns.id AND expects_reply=1 AND settled=1) OR EXISTS(SELECT 1 FROM delegation_tasks d JOIN turns child ON child.id=d.delivery WHERE d.creator_turn=turns.id AND child.status='done' AND d.wake_delivered=0))", [])?;
                    Ok(())
                });
                let pending=db(&state,|db|Ok(one(db,"SELECT 1 FROM remote_agent_calls WHERE state IN('intent','sent') OR cancel_sent=0 AND delivery IN(SELECT id FROM turns WHERE status='cancelled') LIMIT 1",[])?.is_some())).unwrap_or(false);
                if !pending { continue; }
                let ready=transaction(&state,|db|{
                    db.execute("UPDATE turns SET status='interrupted',paused=0,error='Remote task exceeded its 24-hour tracking limit. Check the receiving machine.' WHERE id IN(SELECT delivery FROM remote_agent_calls WHERE state IN('intent','sent') AND created<?) AND status<>'cancelled'",[state.config.now()-86400000])?;
                    db.execute("UPDATE remote_agent_calls SET state='uncertain',status='interrupted',error='Remote task exceeded its 24-hour tracking limit. Check the receiving machine.' WHERE state IN('intent','sent') AND created<?",[state.config.now()-86400000])?;
                    rows(db,"SELECT r.id FROM remote_agent_calls r LEFT JOIN turns t ON t.id=r.delivery WHERE r.next_poll<=? AND(r.state IN('intent','sent') OR(t.status='cancelled' AND r.cancel_sent=0)) ORDER BY r.created LIMIT 32",[state.config.now()])
                }).unwrap_or_default();
                for row in ready {
                    if active.len()>=4 {break}
                    let id=text(&row,"id").to_owned();
                    if !active.insert(id.clone()){continue}
                    let state=state.clone();
                    jobs.spawn(async move{let _=follow(&state,&id).await;id});
                }
            }
        }
    }
}
pub async fn handle(
    state: &AppState,
    request: axum::extract::Request,
) -> Result<axum::response::Response> {
    use axum::http::Method;
    if request.headers().contains_key("authorization") {
        return Err(error(
            403,
            "Remote messages require the local owner in Minarbot.",
        ));
    }
    if request.method() == Method::GET {
        let query = crate::files::query(&request);
        let machine = crate::files::parameter(&query, "machine").unwrap_or("");
        let agent = crate::files::parameter(&query, "agent").unwrap_or("");
        if crate::web::id(machine).is_none() || crate::web::id(agent).is_none() {
            return Err(error(400, "Choose a paired remote agent."));
        }
        return Ok(crate::json_response(
            200,
            json!({"messages":db(state,|db|rows(db,"SELECT id,name,text,status,state,reply,error,created FROM(SELECT * FROM remote_agent_calls WHERE delivery IS NULL AND machine=? AND agent=? ORDER BY created DESC LIMIT 100) ORDER BY created",params![machine,agent]))?}),
        ));
    }
    if request.method() != Method::POST {
        return Err(error(405, "Method not allowed."));
    }
    crate::files::mutation(state, &request)?;
    let value = super::body(request, 64000).await?;
    if !keys(&value, &["id", "machine", "agent", "text"])
        || ["id", "machine", "agent"]
            .iter()
            .any(|k| crate::web::id(text(&value, k)).is_none())
        || !value["text"].is_string()
        || text(&value, "text").trim().is_empty()
        || text(&value, "text").encode_utf16().count() > 16000
    {
        return Err(error(
            400,
            "Message requires a paired agent and 1 to 16,000 characters.",
        ));
    }
    let result = transaction(state, |db| {
        if let Some(prior) = one(
            db,
            "SELECT * FROM remote_agent_calls WHERE id=?",
            [text(&value, "id")],
        )? {
            if !prior["delivery"].is_null()
                || prior["machine"] != value["machine"]
                || prior["agent"] != value["agent"]
                || prior["text"] != text(&value, "text").trim()
            {
                return Err(error(409, "Request ID already used for another message."));
            }
            return Ok(json!({"id":value["id"],"status":prior["status"]}));
        }
        let target=roster(db,true)?.into_iter().find(|v|v["id"]==format!("{}@{}",text(&value,"agent"),text(&value,"machine"))).ok_or_else(||error(409,"Remote agent is offline, unauthorized, or no longer paired. Refresh Machines before sending."))?;
        if let Some(reason) =
            machines::agent_provider_block(db, text(&value, "machine"), text(&value, "agent"))?
        {
            return Err(error(409, reason));
        }
        let count:i64=db.query_row("SELECT count(*) FROM remote_agent_calls WHERE machine=? AND state IN('intent','sending','sent')",[text(&value,"machine")],|r|r.get(0))?;
        if count >= 100 {
            return Err(error(
                429,
                "Remote message queue is full. Wait for current work.",
            ));
        }
        let mut input = value.clone();
        input["name"] = target["name"].clone();
        input["text"] = text(&value, "text").trim().into();
        input["depth"] = 1.into();
        journal(db, &input, state.config.now())?;
        Ok(json!({"id":value["id"],"status":"queued"}))
    })?;
    Ok(crate::json_response(200, result))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roster_rates_and_restart_uncertainty() -> Result<()> {
        let db = Connection::open_in_memory()?;
        db.execute_batch("CREATE TABLE paired_machines(environment_id TEXT,name TEXT,state TEXT,status TEXT,agents TEXT,local_token_id TEXT,allow_agent_messaging INTEGER); CREATE TABLE turns(id TEXT,status TEXT,paused INTEGER,error TEXT); CREATE TABLE remote_agent_calls(id TEXT,machine TEXT,agent TEXT,name TEXT,text TEXT,depth INTEGER,delivery TEXT,state TEXT,created INTEGER,status TEXT,error TEXT); CREATE TABLE peer_revocations(token_id TEXT); CREATE TABLE peer_message_events(id TEXT,peer TEXT,created INTEGER); CREATE TABLE peer_message_cooldowns(peer TEXT PRIMARY KEY,until INTEGER);")?;
        db.execute("INSERT INTO paired_machines VALUES('host','Peer','paired','online',?,'token',1)",[json!([{"id":"agent","name":"Agent","role":"Personal agent","model":"fixture","status":"idle"}]).to_string()])?;
        assert_eq!(
            resolve(&db, "Agent@Peer")?,
            Some(json!({"kind":"remote","machine":"host","agent":"agent"}))
        );
        assert_eq!(roster(&db, true)?.len(), 1);
        db.execute("UPDATE paired_machines SET status='offline'", [])?;
        assert!(roster(&db, true)?.is_empty());
        assert!(resolve(&db, "agent@host")?.is_some());
        enforce_peer_rate(&db, "token", 0, &json!({"rate":1}))?;
        assert_eq!(
            enforce_peer_rate(&db, "token", 1, &json!({"rate":1}))
                .err()
                .map(|e| e.status),
            Some(429)
        );
        assert_eq!(
            enforce_peer_rate(&db, "token", 2, &json!({"rate":1}))
                .err()
                .map(|e| e.message),
            Some("Peer message rate limit is cooling down.".into())
        );
        db.execute("INSERT INTO turns VALUES('delivery','running',0,NULL)", [])?;
        db.execute("INSERT INTO remote_agent_calls VALUES('call','host','agent','','',1,'delivery','sending',0,'queued',NULL)",[])?;
        initialize(&db)?;
        initialize(&db)?;
        assert_eq!(
            one(&db, "SELECT state,status FROM remote_agent_calls", [])?,
            Some(json!({"state":"uncertain","status":"interrupted"}))
        );
        assert_eq!(
            one(&db, "SELECT status,paused FROM turns", [])?,
            Some(json!({"status":"interrupted","paused":0}))
        );
        db.execute("INSERT INTO peer_revocations VALUES('token')", [])?;
        assert!(!messaging_allowed(&db, "token")?);
        Ok(())
    }
}

#[cfg(test)]
mod roster_tests {
    use super::*;

    fn fixture(agent: Value) -> Result<Connection> {
        let db = Connection::open_in_memory()?;
        db.execute_batch("CREATE TABLE paired_machines(environment_id TEXT,name TEXT,state TEXT,status TEXT,agents TEXT)")?;
        db.execute(
            "INSERT INTO paired_machines VALUES ('host','B','paired','online',?)",
            [json!([agent]).to_string()],
        )?;
        Ok(db)
    }

    #[test]
    fn address_resolution_keeps_exact_ids_and_excludes_offline_rosters() -> Result<()> {
        let db = fixture(
            json!({"id":"agent","name":"Beta","role":"Personal agent","model":"fixture","status":"idle"}),
        )?;
        let expected = Some(json!({"kind":"remote","machine":"host","agent":"agent"}));
        assert_eq!(resolve(&db, "Beta@B")?, expected);
        assert_eq!(resolve(&db, "agent@host")?, expected);
        assert_eq!(resolve(&db, "Beta@Unpaired")?, None);
        let online = roster(&db, true)?;
        assert_eq!(online[0]["online"], true);
        assert_eq!(online[0]["machine"], "B");
        db.execute("UPDATE paired_machines SET status='offline'", [])?;
        assert!(roster(&db, true)?.is_empty());
        assert_eq!(resolve(&db, "Beta@B")?, expected);
        db.execute(
            "INSERT INTO paired_machines VALUES ('other','B','paired','online','[]')",
            [],
        )?;
        assert_eq!(resolve(&db, "Beta@B")?, None);
        assert_eq!(resolve(&db, "agent@host")?, expected);
        Ok(())
    }

    #[test]
    fn peer_roles_sanitize_bounded_text_and_default_for_old_summaries() -> Result<()> {
        assert_eq!(role(&Value::Null), "");
        assert_eq!(
            role(&json!("  Personal\n agent\u{0085}\u{2028}  ")),
            "Personal agent"
        );
        assert_eq!(role(&json!("x".repeat(100))), "x".repeat(60));
        assert_eq!(role(&json!("  Keep  spacing ")), "Keep  spacing");
        let db = fixture(json!({"id":"agent","name":"Odd","model":"fixture","status":"idle"}))?;
        assert_eq!(roster(&db, true)?[0]["role"], "");
        Ok(())
    }
}
