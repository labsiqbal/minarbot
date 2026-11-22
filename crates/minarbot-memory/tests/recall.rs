use minarbot_memory::{initialize, Kind, Link, Models, Query, Result, Retain, Store, DIMENSIONS};
use rusqlite::Connection;
use serde_json::json;

struct FixtureModels;

impl Models for FixtureModels {
    fn embed(&self, texts: &[&str], _: bool) -> Result<Vec<[f32; DIMENSIONS]>> {
        Ok(texts
            .iter()
            .map(|text| {
                let mut vector = [0.; DIMENSIONS];
                let related =
                    text.contains("feline") || text.contains("cat") || text.contains("litter");
                vector[usize::from(!related)] = 1.0;
                vector
            })
            .collect())
    }

    fn rerank(&self, _: &str, texts: &[&str]) -> Result<Vec<f32>> {
        Ok(texts
            .iter()
            .map(|text| if text.contains("cat") { 2.0 } else { 0.0 })
            .collect())
    }
}

#[test]
fn recall_fuses_and_reranks_with_sources_and_date_fences() -> Result<()> {
    let db = Connection::open_in_memory()?;
    db.execute_batch("PRAGMA foreign_keys=ON")?;
    initialize(&db)?;
    let store = Store::new(&db);
    let date = 1_790_000_123_000;
    let add = |bank, text, entities| {
        store.retain(
            bank,
            text,
            Retain {
                kind: Kind::Experience,
                occurred_at: Some(date),
                entities,
                source: json!({"kind":"turn","id":text}),
                ..Retain::default()
            },
            &FixtureModels,
        )
    };
    let litter = add("agents:a", "feline litter measurements", &["sam"])?;
    let cat = add("agents:a", "Sam adopted a cat named Pepper", &["Sam"])?;
    let third = add("agents:a", "Pet carrier was returned", &["  SAM "])?;
    let aside = add("agents:a", "Transport details", &[])?;
    add("agents:b", "Another private cat", &[])?;
    store.link("agents:a", litter, aside, Link::Temporal)?;
    let query = Query {
        bank: "agents:a",
        text: "feline litter",
        budget: 4,
        after: Some(date),
        ..Query::default()
    };
    let hits = store.recall(&query, &FixtureModels)?;
    assert_eq!(hits.first().ok_or("No results")?.unit.id, cat);
    assert_eq!(hits[0].unit.source["id"], "Sam adopted a cat named Pepper");
    // Graph contributes additional rank to same-entity and explicit-link neighbors.
    for id in [aside, third] {
        assert!(hits
            .iter()
            .any(|hit| hit.unit.id == id && hit.rrf > 2.0 / 65.0));
    }
    assert!(store
        .recall(
            &Query {
                after: Some(date + 1),
                ..query
            },
            &FixtureModels
        )?
        .is_empty());
    assert!(store
        .keyword(&Query {
            text: "feline litter",
            budget: 1,
            ..query
        })?
        .iter()
        .all(|hit| hit.unit.id == litter));
    Ok(())
}
