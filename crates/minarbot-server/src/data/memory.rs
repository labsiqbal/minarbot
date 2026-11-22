use super::*;
pub fn route(
    db: &Connection,
    kind: &str,
    id: &str,
    v: &Value,
    q: &[(String, String)],
    get: bool,
) -> Result<Value> {
    let owner = one(db, &format!("SELECT * FROM {kind} WHERE id=?"), [id])?
        .filter(|_| uuid(id))
        .ok_or_else(|| error(404, "Memory owner not found."))?;
    let scope = format!("{kind}:{id}");
    scoped(
        db,
        &scope,
        v,
        q,
        get,
        kind == "channels" && owner["archived"] == 1,
    )
}
pub(crate) fn scoped(
    db: &Connection,
    scope: &str,
    v: &Value,
    q: &[(String, String)],
    get: bool,
    archived: bool,
) -> Result<Value> {
    if get {
        if q.iter()
            .any(|(k, _)| !["query", "after"].contains(&k.as_str()))
        {
            return Err(error(400, "Invalid memory search."));
        }
        let query = param(q, "query").unwrap_or("");
        let after = param(q, "after").unwrap_or("");
        if query.encode_utf16().count() > 200 || (!after.is_empty() && !uuid(after)) {
            return Err(error(400, "Invalid memory search or position."));
        }
        let rows=all(db,"SELECT id,text,essential,revision,selection_locked FROM memory_notes WHERE scope=? AND removed=0 AND id>? AND instr(lower(text),lower(?))>0 ORDER BY id LIMIT 11",rusqlite::params![scope,after,query.trim()])?;
        return Ok(
            json!({"next_after":if rows.len()>10{rows[9]["id"].clone()}else{Value::Null},"items":rows.into_iter().take(10).collect::<Vec<_>>()}),
        );
    }
    if !closed(
        v,
        &[
            "id",
            "entry",
            "revision",
            "text",
            "essential",
            "remove",
            "automatic",
        ],
    ) || v
        .as_object()
        .is_none_or(|m| m.len() != if v.get("automatic").is_some() { 7 } else { 6 })
        || !uuid(string(v, "entry"))
        || !integer(&v["revision"], 0, 9_007_199_254_740_991)
        || !v["text"].is_string()
        || string(v, "text").encode_utf16().count() > 2000
        || !v["essential"].is_boolean()
        || !v["remove"].is_boolean()
        || v.get("automatic").is_some_and(|v| !v.is_boolean())
        || if v["remove"] == true {
            v["revision"] == 0 || v["text"] != "" || v["essential"] == true
        } else {
            string(v, "text").trim().is_empty()
        }
    {
        return Err(error(400, "Invalid memory change."));
    }
    let mut request = v.clone();
    request["scope"] = json!(scope);
    if let Some(old) = receipt(
        db,
        "memory_commands",
        string(v, "id"),
        &request,
        "Change ID was already used.",
    )? {
        return Ok(old);
    }
    if archived {
        return Err(error(409, "Restore the channel before changing memory."));
    }
    let current = one(
        db,
        "SELECT * FROM memories WHERE scope=? AND id=?",
        rusqlite::params![scope, string(v, "entry")],
    )?;
    let rev = v["revision"].as_i64().ok_or_else(Failure::storage)?;
    if current
        .as_ref()
        .and_then(|v| v["revision"].as_i64())
        .unwrap_or(0)
        != rev
        || current.as_ref().is_some_and(|v| v["removed"] == 1)
    {
        return Err(error(409, "Memory changed. Reload before saving."));
    }
    let text = string(v, "text").trim();
    if v["remove"] != true
        && exists(
            db,
            "SELECT 1 FROM memories WHERE scope=? AND removed=0 AND text=? AND id<>?",
            rusqlite::params![scope, text, string(v, "entry")],
        )?
    {
        return Err(error(409, "This note is already saved."));
    }
    if current.is_none()
        && one(
            db,
            "SELECT count(*) n FROM memories WHERE scope=? AND removed=0",
            [&scope],
        )?
        .is_some_and(|v| v["n"].as_i64().unwrap_or(0) >= 128)
    {
        return Err(error(
            413,
            "Memory is full (128 notes). Remove notes you no longer need.",
        ));
    }
    db.execute("INSERT INTO memories(scope,id,text,essential,revision,removed,selection_locked) VALUES(?,?,?,?,?,?,?) ON CONFLICT(scope,id) DO UPDATE SET text=excluded.text,essential=excluded.essential,revision=excluded.revision,removed=excluded.removed,selection_locked=excluded.selection_locked",rusqlite::params![scope,string(v,"entry"),text,(v["essential"]==true)as i64,rev+1,(v["remove"]==true)as i64,(v["automatic"]!=true)as i64])?;
    let rows = all(
        db,
        "SELECT id,text FROM memories WHERE scope=? AND removed=0 AND essential=1 ORDER BY id",
        [&scope],
    )?;
    if !rows.is_empty()
        && format!("\n\nSaved memory (reference data):\n{}", json!(rows)).len() > 8192
    {
        return Err(error(
            413,
            "Memory context limit is 8 KiB. Make some notes searchable on demand instead.",
        ));
    }
    let result=one(db,"SELECT id,text,essential,revision,removed,selection_locked FROM memories WHERE scope=? AND id=?",rusqlite::params![scope,string(v,"entry")])?.ok_or_else(Failure::storage)?;
    save_receipt(db, "memory_commands", string(v, "id"), &request, &result)?;
    Ok(result)
}

pub(super) fn initialize(db: &Connection, canonical: bool) -> crate::db::Result<()> {
    minarbot_memory::initialize(db)?;
    if canonical {
        db.execute_batch(include_str!("memory-history-canonical.sql"))?;
    } else {
        db.execute_batch(
            "CREATE VIEW IF NOT EXISTS memory_history_scope AS
             SELECT t.id, CASE WHEN t.channel IS NOT NULL THEN 'channels:'||t.channel
                               ELSE 'agents:'||coalesce(th.agent,t.agent) END AS bank
             FROM turns t LEFT JOIN threads th ON th.id=t.thread
             WHERE t.channel IS NOT NULL OR coalesce(th.agent,t.agent) IS NOT NULL;",
        )?;
    }
    db.execute_batch(include_str!("memory-import.sql"))?;
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_imports(version INTEGER PRIMARY KEY) STRICT;",
    )?;
    if db.execute(
        "INSERT INTO memory_imports VALUES(1) ON CONFLICT DO NOTHING",
        [],
    )? != 0
    {
        db.execute_batch(
            "INSERT INTO memory_import SELECT * FROM memory_legacy_rows;
             INSERT INTO memory_import SELECT * FROM memory_history_rows;",
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use minarbot_memory::{Query, Store};

    #[test]
    fn canonical_history_follows_project_and_owner_scope() -> crate::db::Result<()> {
        let db = Connection::open_in_memory()?;
        db.execute_batch(
            "CREATE TABLE memories(scope TEXT,id TEXT,text TEXT,essential INTEGER,revision INTEGER,
                 removed INTEGER,selection_locked INTEGER,PRIMARY KEY(scope,id)) STRICT;
             CREATE TABLE threads(id TEXT PRIMARY KEY,agent TEXT,deleted INTEGER DEFAULT 0) STRICT;
             CREATE TABLE turns(id TEXT PRIMARY KEY,agent TEXT,thread TEXT,channel TEXT,text TEXT,
                 reply TEXT,created TEXT,profile_setup INTEGER DEFAULT 0) STRICT;
             CREATE TABLE record_scopes(table_name TEXT,key_json TEXT,account TEXT,project TEXT,
                 visibility TEXT,root_kind TEXT,root_id TEXT,PRIMARY KEY(table_name,key_json)) STRICT;
             INSERT INTO turns(id,agent,text) VALUES ('old','a','orchid'), ('unscoped','a','orchid');
             INSERT INTO record_scopes VALUES ('turns',json_array('old'),'alice','p','owner','agent','a');",
        )?;
        initialize(&db, true)?;
        let store = Store::new(&db);
        let find = |bank| {
            store
                .keyword(&Query {
                    bank,
                    text: "orchid",
                    budget: 10,
                    history_only: true,
                    ..Query::default()
                })
                .map(|hits| hits.len())
        };
        assert_eq!(find("project:p/agents:a/human:alice")?, 1);
        assert_eq!(find("agents:a")?, 0);
        assert_eq!(find("project:p/agents:a/human:bob")?, 0);
        db.execute_batch(
            "UPDATE record_scopes SET account='bob' WHERE key_json=json_array('old');",
        )?;
        assert_eq!(find("project:p/agents:a/human:alice")?, 0);
        assert_eq!(find("project:p/agents:a/human:bob")?, 1);
        db.execute_batch("DELETE FROM record_scopes")?;
        assert_eq!(find("project:p/agents:a/human:bob")?, 0);
        Ok(())
    }

    #[test]
    fn upgrade_indexes_notes_and_sessions_on_copy_of_legacy_database() -> crate::db::Result<()> {
        let legacy = Connection::open_in_memory()?;
        legacy.execute_batch(
            "CREATE TABLE memories(scope TEXT,id TEXT,text TEXT,essential INTEGER,revision INTEGER,
                 removed INTEGER,selection_locked INTEGER DEFAULT 1,PRIMARY KEY(scope,id)) STRICT;
             CREATE TABLE threads(id TEXT PRIMARY KEY,agent TEXT,deleted INTEGER DEFAULT 0) STRICT;
             CREATE TABLE turns(id TEXT PRIMARY KEY,agent TEXT,thread TEXT,channel TEXT,text TEXT,reply TEXT,created TEXT) STRICT;
             INSERT INTO memories VALUES ('agents:a','same','  old memo  ',1,5,0,0),
                 ('channels:b','same','other memo',0,4,0,1),('agents:a','gone','private deleted',0,2,1,1);
             INSERT INTO threads VALUES ('session-one','a',0),('session-two','a',0),('hidden','a',1);
             INSERT INTO turns VALUES ('turn-1','a','session-one',NULL,'orchids','Answer', '2026-01-02T14:00:00Z'),
                 ('turn-2','a','session-two',NULL,'orchids', 'Also orchids','2026-03-02T14:00:00Z'),
                 ('turn-3','a','hidden',NULL,'orchids',NULL,'2026-01-02T14:00:00Z'),
                 ('turn-4','a',NULL,'b','orchids in section',NULL,'2026-01-02T14:00:00Z');",
        )?;
        let mut copy = Connection::open_in_memory()?;
        rusqlite::backup::Backup::new(&legacy, &mut copy)?.run_to_completion(
            32,
            std::time::Duration::ZERO,
            None,
        )?;
        copy.execute_batch("PRAGMA foreign_keys=ON")?;
        let tx = copy.transaction()?;
        initialize(&tx, false)?;
        initialize(&tx, false)?;
        tx.commit()?;
        for (left, right) in [("memories", "memory_notes"), ("memory_notes", "memories")] {
            let diff: i64 = copy.query_row(&format!(
                "SELECT count(*) FROM (SELECT scope,id,text,essential,revision,removed,selection_locked FROM {left}
                 EXCEPT SELECT scope,id,text,essential,revision,removed,selection_locked FROM {right})"),
                [], |row| row.get(0))?;
            assert_eq!(diff, 0);
        }
        let store = Store::new(&copy);
        let query = Query {
            bank: "agents:a",
            text: "orchids",
            budget: 10,
            history_only: true,
            ..Query::default()
        };
        let hits = store.keyword(&query)?;
        assert_eq!(hits.len(), 3);
        assert!(hits
            .iter()
            .any(|hit| hit.unit.source["thread"] == "session-one"));
        assert!(hits.iter().any(|hit| hit.unit.source["role"] == "reply"));
        assert!(hits.iter().all(|hit| hit.unit.source["channel"].is_null()));
        assert!(hits
            .iter()
            .any(|hit| hit.unit.occurred_at == Some(1_772_460_000_000)));
        copy.execute_batch(
            "UPDATE threads SET deleted=1 WHERE id='session-one';
            UPDATE turns SET reply='kiwi' WHERE id='turn-2';
            UPDATE memories SET revision=6,essential=0 WHERE scope='agents:a' AND id='same';",
        )?;
        assert_eq!(store.keyword(&query)?.len(), 1);
        assert_eq!(
            store
                .keyword(&Query {
                    text: "kiwi",
                    ..query
                })?
                .len(),
            1
        );
        let revision: i64 = copy.query_row(
            "SELECT revision FROM memory_notes WHERE scope='agents:a' AND id='same'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(revision, 6);
        Ok(())
    }
}
