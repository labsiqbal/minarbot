use minarbot_memory::{initialize, Query, Result, Store};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

fn raw(db: &Connection, text: &str, date: i64, role: &str) -> Result<i64> {
    db.execute("INSERT OR IGNORE INTO memory_banks VALUES('bank')", [])?;
    let id = db.query_row(
        "INSERT INTO memory_units(bank,origin,source_id,kind,text,mentioned_at,source)
         VALUES('bank',?,?,'experience',?,?,json_object('role',?,'agent','a')) RETURNING id",
        params![role, uuid::Uuid::new_v4().to_string(), text, date, role],
        |r| r.get(0),
    )?;
    Store::new(db).queue_extraction(id)?;
    Ok(id)
}

fn fixture() -> Result<Connection> {
    let db = Connection::open_in_memory()?;
    db.execute_batch("PRAGMA foreign_keys=ON;")?;
    initialize(&db)?;
    Ok(db)
}

fn respond(store: &Store<'_>, extraction: bool, answer: Value) -> Result<Value> {
    let work = store.pending_work(0)?.ok_or("Expected memory work")?;
    assert_eq!(work.extracting(), extraction);
    let (_, input) = work.prompt();
    store.apply_work(&work, &answer.to_string())?;
    Ok(input)
}

fn world(text: &str) -> Value {
    json!({"what":text,"fact_kind":"conversation","fact_type":"world","entities":["Atlas"]})
}

fn decision(existing: Value, new: &[&str]) -> Value {
    json!({"facts":[{"fact_id":"F1","kind":"lasting","existing":existing,"new":new}]})
}

#[test]
fn fake_model_extracts_and_reconciles_observations_with_evidence() -> Result<()> {
    let db = fixture()?;
    let store = Store::new(&db);
    let first = raw(&db, "Atlas is owned by Ada.", 3000, "prompt")?;
    assert!(store.pending_work_excluding(0, &["a"])?.is_none());
    respond(
        &store,
        true,
        json!({"facts":[world("Atlas is owned by Ada.")]}),
    )?;
    assert!(store.pending_work_excluding(0, &["a"])?.is_none());
    respond(
        &store,
        false,
        decision(json!([]), &["Atlas is owned by Ada."]),
    )?;

    raw(&db, "Ada owns Atlas.", 4000, "prompt")?;
    respond(&store, true, json!({"facts":[world("Ada owns Atlas.")]}))?;
    let matches = respond(
        &store,
        false,
        decision(json!([{"observation_id":"O1","effect":"repeats"}]), &[]),
    )?;
    assert_eq!(matches["observations"][0]["proof_count"], 1);

    let latest = raw(&db, "Atlas is now owned by Kim.", 5000, "prompt")?;
    respond(
        &store,
        true,
        json!({"facts":[world("Atlas is owned by Kim.")]}),
    )?;
    respond(
        &store,
        false,
        decision(
            json!([{"observation_id":"O1","effect":"changes","text":"Atlas is owned by Kim."}]),
            &[],
        ),
    )?;

    raw(&db, "Atlas was owned by Jo.", 1000, "prompt")?;
    respond(
        &store,
        true,
        json!({"facts":[world("Atlas is owned by Jo.")]}),
    )?;
    let matches = respond(
        &store,
        false,
        decision(
            json!([{"observation_id":"O1","effect":"changes","text":"Atlas is owned by Jo."}]),
            &[],
        ),
    )?;
    assert_eq!(
        matches["facts"][0]["closest_observations"][0]["stated_after_fact"],
        true
    );
    let rows = store.keyword(&Query {
        bank: "bank",
        text: "Atlas",
        budget: 32,
        ..Query::default()
    })?;
    let observation = rows
        .iter()
        .find(|hit| hit.unit.kind == "observation")
        .ok_or("Missing observation")?;
    assert!(observation.unit.text.starts_with("Atlas is owned by Kim."));
    assert!(observation.unit.text.contains("Atlas is owned by Jo."));
    assert_eq!(observation.unit.metadata["proof_count"], 4);
    assert_eq!(observation.unit.entities, ["atlas"]);
    assert_eq!(
        rows.iter()
            .filter(|hit| hit.unit.kind == "observation")
            .count(),
        1
    );

    db.execute("DELETE FROM memory_units WHERE id=?", [latest])?;
    assert_eq!(
        db.query_row("SELECT count(*) FROM memory_observations", [], |r| r
            .get::<_, i64>(0))?,
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM memory_facts WHERE consolidated=0",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        3
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM memory_units WHERE id=?",
            [first],
            |r| r.get::<_, i64>(0)
        )?,
        1
    );
    assert!(store.pending_work(0)?.is_some());
    Ok(())
}

#[test]
fn fake_model_work_resumes_and_keeps_raw_units() -> Result<()> {
    let db = fixture()?;
    let store = Store::new(&db);
    let text = format!("{}{}", "é".repeat(1600), "界".repeat(500));
    let id = raw(&db, &text, 3000, "reply")?;
    let stale = store.pending_work(0)?.ok_or("Missing work")?;
    assert!(store.apply_work(&stale, "broken JSON").is_err());
    store.retry_work(&stale, 0)?;
    assert!(store.pending_work(4_999)?.is_none());
    let store = Store::new(&db);
    let first = store.pending_work(5_000)?.ok_or("Missing retry")?;
    let (_, first_input) = first.prompt();
    store.apply_work(&first, r#"{"facts":[]}"#)?;
    let second = store.pending_work(0)?.ok_or("Missing second chunk")?;
    let (_, second_input) = second.prompt();
    let combined = [
        first_input["content"]
            .as_str()
            .ok_or("Missing first text")?,
        second_input["content"]
            .as_str()
            .ok_or("Missing second text")?,
    ]
    .concat();
    assert_eq!(combined, text);
    db.execute(
        "UPDATE memory_units SET text='Revised raw reply' WHERE id=?",
        [id],
    )?;
    store.apply_work(&second, &json!({"facts":[world("Stale fact")]}).to_string())?;
    let fresh = store.pending_work(0)?.ok_or("Missing revised work")?;
    assert_eq!(fresh.prompt().1["content"], "Revised raw reply");
    store.apply_work(&fresh,&json!({"facts":[
        {"what":"Atlas service failed.", "fact_type":"world","fact_kind":"event",
            "occurred_start":"2026-02-28","occurred_end":"2026-03-01","entities":["Atlas"]},
        {"what":"Assistant repaired Atlas.","fact_type":"assistant","fact_kind":"event",
            "occurred_start":"2026-02-28","entities":["Atlas"],"causal_relations":[
                {"target_index":0,"relation_type":"caused_by"},{"target_index":2,"relation_type":"caused_by"}]}
    ]}).to_string())?;
    let batch = store.pending_work(0)?.ok_or("Missing consolidation")?;
    assert!(store.apply_work(&batch, r#"{"facts":[]}"#).is_err());
    store.retry_work(&batch, 0)?;
    let experience = store.keyword(&Query {
        bank: "bank",
        text: "repaired",
        budget: 10,
        ..Query::default()
    })?;
    assert_eq!(experience[0].unit.kind, "experience");
    assert_eq!(experience[0].unit.occurred_at, Some(1_772_236_800_000));
    assert_eq!(
        experience[0].unit.metadata["caused_by"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM memory_links WHERE bank='bank' AND kind='temporal'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        1
    );
    assert_eq!(
        db.query_row("SELECT text FROM memory_units WHERE id=?", [id], |r| r
            .get::<_, String>(
            0
        ))?,
        "Revised raw reply"
    );
    Ok(())
}
