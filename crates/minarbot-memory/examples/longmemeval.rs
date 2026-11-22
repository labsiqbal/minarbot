use minarbot_memory::{initialize, LocalModels, Query, Result, Retain, Store};
use rusqlite::{params, Connection};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{io, path::PathBuf, time::Instant};

#[derive(Deserialize)]
struct Case {
    id: String,
    question: String,
    documents: Vec<Document>,
}

#[derive(Deserialize)]
struct Document {
    session: String,
    text: String,
}

fn main() -> Result<()> {
    let cache = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("Model cache is required")?;
    let cases: Vec<Case> = serde_json::from_reader(io::stdin().lock())?;
    let models = LocalModels::load(&cache)?;
    let mut results = Vec::new();
    for case in cases {
        let db = Connection::open_in_memory()?;
        db.execute_batch(
            "PRAGMA foreign_keys=ON; CREATE TABLE old_memory(session TEXT,text TEXT)",
        )?;
        initialize(&db)?;
        let store = Store::new(&db);
        for document in case.documents {
            db.execute(
                "INSERT INTO old_memory VALUES(?,?)",
                params![document.session, document.text],
            )?;
            store.retain(
                "fixture",
                &document.text,
                Retain {
                    source: json!({"session":document.session}),
                    ..Retain::default()
                },
                &models,
            )?;
        }
        let old = db
            .prepare("SELECT session FROM old_memory WHERE instr(lower(text),lower(?))>0 LIMIT 5")?
            .query_map([&case.question], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let query = Query {
            bank: "fixture",
            text: &case.question,
            budget: 5,
            ..Query::default()
        };
        let ids = |hits: Vec<minarbot_memory::Hit>| -> Vec<Value> {
            hits.into_iter()
                .map(|hit| hit.unit.source["session"].clone())
                .collect()
        };
        let start = Instant::now();
        let keyword = ids(store.keyword(&query)?);
        let keyword_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        let hybrid = ids(store.recall(&query, &models)?);
        let hybrid_ms = start.elapsed().as_secs_f64() * 1000.0;
        results.push(
            json!({"id":case.id,"legacy":old,"keyword":keyword,"hybrid":hybrid,
            "keyword_ms":keyword_ms,"hybrid_ms":hybrid_ms}),
        );
    }
    println!("{}", serde_json::to_string_pretty(&results)?);
    Ok(())
}
