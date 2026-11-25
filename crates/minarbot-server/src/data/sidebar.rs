use super::*;
use regex::Regex;

const SUMMARY_SQL: &str = r#"WITH attention_counts AS (
        SELECT agent,count(*) AS needs_you FROM (
            SELECT DISTINCT agent,channel,CASE WHEN channel IS NULL THEN thread END FROM conversation_attention
        ) GROUP BY agent
    )
    SELECT a.id AS agent,substr(t.reply,1,8192) AS reply,substr(t.text,1,8192) AS text,t.created,
      CASE WHEN EXISTS(SELECT 1 FROM turns WHERE agent=a.id AND channel IS NULL AND status='running') THEN 'running' ELSE 'idle' END AS status,
      coalesce(n.needs_you,0) AS needs_you
    FROM agents a LEFT JOIN attention_counts n ON n.agent=a.id
    LEFT JOIN turns t ON t.seq=(SELECT seq FROM turns WHERE agent=a.id AND channel IS NULL ORDER BY seq DESC LIMIT 1)
    WHERE a.id=?1 OR a.rowid IN (SELECT rowid FROM agents WHERE id<>?1 ORDER BY rowid LIMIT 500)
    ORDER BY a.id=?1,a.rowid"#;
pub fn read(db: &Connection, q: &[(String, String)]) -> Result<Value> {
    if !q.is_empty() {
        return Err(error(400, "Invalid sidebar parameters."));
    }
    let row = one(db, "SELECT revision,sections FROM sidebar WHERE id=1", [])?
        .ok_or_else(Failure::storage)?;
    Ok(json!({"revision":row["revision"],"sections":json(&row["sections"])?}))
}
pub fn save(db: &Connection, v: &Value, q: &[(String, String)]) -> Result<Value> {
    if !q.is_empty()
        || !closed(v, &["id", "revision", "sections"])
        || !integer(&v["revision"], 0, 9_007_199_254_740_991)
    {
        return Err(error(400, "Invalid sidebar request."));
    }
    let sections = v["sections"]
        .as_array()
        .filter(|v| v.len() <= 100)
        .ok_or_else(|| error(400, "Sidebar limit is 100 sections."))?;
    let mut ids = std::collections::HashSet::new();
    let mut agents = std::collections::HashSet::new();
    let mut normalized = Vec::new();
    for section in sections {
        let name = section["name"].as_str().unwrap_or("");
        let id = string(section, "id");
        let valid_id = id.len() == 36
            && id.bytes().enumerate().all(|(i, b)| {
                if [8, 13, 18, 23].contains(&i) {
                    b == b'-'
                } else {
                    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
                }
            });
        if !closed(section, &["id", "name", "agents"])
            || !valid_id
            || !ids.insert(id)
            || name.trim().is_empty()
            || name.encode_utf16().count() > 80
            || name.chars().any(|c| c < ' ' || c == '\u{7f}')
            || section["agents"].as_array().is_none_or(|a| a.len() > 1000)
        {
            return Err(error(400, "Invalid sidebar section."));
        }
        for a in section["agents"].as_array().ok_or_else(Failure::storage)? {
            let id = a.as_str().unwrap_or("");
            if !uuid(id) || id != id.to_ascii_lowercase() || !agents.insert(id) {
                return Err(error(400, "Agent can only belong to one section."));
            }
        }
        normalized.push(json!({"id":id,"name":name.trim(),"agents":section["agents"]}));
    }
    let request = json!({"revision":v["revision"],"sections":normalized});
    if let Some(old) = receipt(
        db,
        "sidebar_commands",
        string(v, "id"),
        &request,
        "ID was already used for a different change.",
    )? {
        return Ok(old);
    }
    if read(db, &[])?["revision"] != v["revision"] {
        return Err(error(409, "Sidebar changed. Reload before saving."));
    }
    for id in agents {
        if !exists(db, "SELECT 1 FROM agents WHERE id=?", [id])? {
            return Err(error(404, "Sidebar agent not found."));
        }
    }
    let revision = v["revision"].as_i64().ok_or_else(Failure::storage)? + 1;
    let result = json!({"revision":revision,"sections":normalized});
    db.execute(
        "UPDATE sidebar SET revision=?,sections=? WHERE id=1",
        rusqlite::params![revision, json!(normalized).to_string()],
    )?;
    save_receipt(db, "sidebar_commands", string(v, "id"), &request, &result)?;
    Ok(result)
}
pub fn summary(
    state: &AppState,
    db: &Connection,
    q: &[(String, String)],
    h: &HeaderMap,
) -> Result<Value> {
    if !q.is_empty() {
        return Err(error(400, "Invalid sidebar summary parameters."));
    }
    if h.contains_key("origin") && !origin(state, h) {
        return Err(error(403, "Origin is not allowed."));
    }
    let mut secrets = providers::accounts_at(db)?
        .into_iter()
        .filter_map(|a| a.key)
        .collect::<Vec<_>>();
    for row in all(db, "SELECT id FROM provider_instances WHERE enabled=1", [])? {
        if let Some((_, _, _, environment)) =
            crate::data::provider_instances::launch_config_from_db(state, db, string(&row, "id"))?
        {
            secrets.extend(environment.into_values());
        }
    }
    if let Some(token) = &state.config.remaining_data.script_token {
        secrets.push(token.clone());
    }
    let pattern=Regex::new(r#"(?i)\b(?:sk-[A-Za-z0-9_-]{16,}|Bearer\s+[A-Za-z0-9._~+/-]+=*|(?:api[_-]?key|token|secret|password)["']?\s*[:=]\s*["']?[^\s,"'}]+)"#).map_err(|_|Failure::storage())?;
    let redact = |v: &str| {
        let mut text = v.to_owned();
        for key in &secrets {
            if !key.is_empty() {
                text = text.replace(key, "[redacted]");
            }
        }
        let text = pattern.replace_all(&text, "[redacted]");
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    let local = all(db, SUMMARY_SQL, [crate::mcp::guide::ID])?;
    let mut summaries=local.into_iter().map(|r|json!({"agent":r["agent"],"preview":redact(if string(&r,"reply").is_empty(){string(&r,"text")}else{string(&r,"reply")}),"time":r["created"],"status":r["status"],"needs_you":r["needs_you"]})).collect::<Vec<_>>();
    if table(db, "paired_machines")? {
        for r in all(db,"SELECT machine,agent,substr(reply,1,8192) AS reply,substr(text,1,8192) AS text,created,status FROM remote_agent_calls AS calls WHERE delivery IS NULL AND EXISTS(SELECT 1 FROM paired_machines WHERE environment_id=calls.machine AND state='paired') AND id=(SELECT id FROM remote_agent_calls AS latest WHERE latest.delivery IS NULL AND latest.machine=calls.machine AND latest.agent=calls.agent ORDER BY created DESC,id DESC LIMIT 1) ORDER BY machine,agent LIMIT 500",[])?{summaries.push(json!({"agent":format!("{}@{}",string(&r,"agent"),string(&r,"machine")),"preview":redact(if string(&r,"reply").is_empty(){string(&r,"text")}else{string(&r,"reply")}),"time":iso(db,r["created"].as_i64().unwrap_or(0))?,"status":r["status"],"needs_you":0}));}
    }
    Ok(json!({"summaries":summaries}))
}

pub(crate) fn attention(db: &Connection) -> Result<Value> {
    let pending = all(db,
        "SELECT agent,thread,channel,turn,focus,kind,request FROM conversation_attention ORDER BY seq,kind,request", [])?;
    // ponytail: short reconnect window. Use a durable cursor if offline delivery is needed.
    let finished = all(db, "SELECT coalesce(th.agent,t.agent) AS agent,t.thread,t.channel,t.id AS turn,coalesce(r.root,t.id) AS focus,t.status
        FROM turns t LEFT JOIN threads th ON th.id=t.thread LEFT JOIN channels c ON c.id=t.channel
        LEFT JOIN room_turns r ON r.turn=t.id
        WHERE t.attention_finished_at>=unixepoch()-120 AND t.status IN('done','failed')
          AND coalesce(th.deleted,0)=0 AND coalesce(th.archived,0)=0 AND coalesce(c.archived,0)=0
        ORDER BY t.attention_finished_at DESC,t.seq DESC LIMIT 100", [])?;
    Ok(json!({"pending":pending,"finished":finished}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_you_counts_conversations_and_clears_resolved_requests() -> crate::db::Result<()> {
        let directory =
            std::env::temp_dir().join(format!("minarbot-sidebar-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory)?;
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let database = crate::db::open(&directory.join("app.sqlite"), false, 0)?;
        let db = &database.connection;
        crate::files::init(db)?;
        crate::data::initialize(&database)?;
        crate::turns::initialize(db).map_err(|e| e.message)?;
        crate::connectors::initialize(db).map_err(|e| e.message)?;
        crate::computer::initialize(db)?;
        db.execute_batch(include_str!("attention.sql"))?;
        db.execute_batch(
            "INSERT INTO agents(id,name,model,instructions) VALUES ('a','Agent A','model',''),('b','Agent B','model','');
             INSERT INTO threads(id,agent,name) VALUES ('thread','a','Side chat');
             INSERT INTO turns(id,agent,thread,text,status,created,paused) VALUES
                  ('approve','a',NULL,'Approve','running','2026-01-01',0),
                  ('ask','a','thread','Ask','queued','2026-01-01',1),
                  ('other','b',NULL,'Other','queued','2026-01-01',0);
             INSERT INTO harness_approvals(id,turn,agent,runtime,workspace,kind,description,input,state)
                  VALUES ('ap','approve','a','api','/tmp','command','Approve','{}','approval');
             INSERT INTO questions(id,turn,call_id,call,prompt,choices,after_output,after_action)
                  VALUES ('q1','ask','call-1','{}','Question','[]',0,0),
                         ('q2','ask','call-2','{}','Question','[]',0,0);
             INSERT INTO computer_waits VALUES('ask','a','desktop','Take over','waiting',1);",
        )?;
        let counts = || {
            all(db, SUMMARY_SQL, [crate::mcp::guide::ID])
                .map(|rows| {
                    rows.into_iter()
                        .map(|row| row["needs_you"].clone())
                        .collect::<Vec<_>>()
                })
                .map_err(|e| e.message)
        };
        let threads = || conversations::threads(db, "a", &json!({}), true).map_err(|e| e.message);
        let snapshot = || attention(db).map_err(|e| e.message);
        assert_eq!(counts()?, [json!(2), json!(0)]);
        assert_eq!(threads()?[0]["needs_you"], 1);
        assert_eq!(snapshot()?["pending"].as_array().map(Vec::len), Some(4));
        db.execute_batch(
            "UPDATE questions SET answer='Blue'; UPDATE computer_waits SET state='returned';",
        )?;
        assert_eq!(counts()?, [json!(1), json!(0)]);
        assert_eq!(threads()?[0]["needs_you"], 0);
        db.execute_batch("UPDATE harness_approvals SET state='decided'; UPDATE turns SET status='done' WHERE id='approve';")?;
        assert_eq!(counts()?, [json!(0), json!(0)]);
        assert_eq!(snapshot()?["finished"][0]["turn"], "approve");
        Ok(())
    }
}
