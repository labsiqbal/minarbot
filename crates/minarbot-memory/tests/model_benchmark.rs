use minarbot_memory::{initialize, LocalModels, Query, Result, Retain, Store};
use rusqlite::{params, Connection};
use serde_json::json;
use std::time::Instant;

const DOCUMENTS: &[&str] = &[
    "Mira adopted a rescue cat named Wasabi and bought a litter tray.",
    "The rental contract for Mira's apartment renews in September.",
    "I keep the bicycle in a shed behind the kitchen.",
    "The espresso machine uses a 58 mm portafilter.",
    "In June, Ren switched from dairy milk to oat milk in coffee.",
    "The migration to PostgreSQL was postponed until after the audit.",
    "Kai repaired the broken derailleur on the red bicycle.",
    "The new office is a five-minute walk from the station.",
    "Client invoices go to accounts payable by the tenth of each month.",
    "The local time zone for the sales scheduler is Asia/Jakarta.",
    "Sensor calibration uses reference voltage and ambient temperature.",
    "The deployment log contains the CI build number and artifact checksum.",
];
const QUERIES: &[(&str, i64)] = &[
    ("What did Mira name her newly adopted pet?", 1),
    ("Where do I store my bike when I get home?", 3),
    (
        "What plant-based substitute did Ren start putting in the morning drink?",
        5,
    ),
    ("58 mm portafilter", 4),
];

#[test]
#[ignore = "downloads pinned models and measures release-build CPU inference"]
fn local_model_quality_and_10k_recall() -> Result<()> {
    let root = tempfile::tempdir()?;
    let started = Instant::now();
    let models = LocalModels::load(&root.path().join("models"))?;
    println!(
        "Cold download, verification and model load: {:.2} s",
        started.elapsed().as_secs_f64()
    );
    let path = root.path().join("benchmark.sqlite");
    let db = Connection::open(&path)?;
    initialize(&db)?;
    let store = Store::new(&db);
    for (index, doc) in DOCUMENTS.iter().enumerate() {
        store.retain(
            "fixture",
            doc,
            Retain {
                source: json!({"id":index+1}),
                ..Retain::default()
            },
            &models,
        )?;
    }
    let mut baseline = 0;
    let mut hybrid = 0;
    for (q, correct) in QUERIES {
        let exact: Vec<i64> = db.prepare("SELECT id FROM memory_units WHERE instr(lower(text),lower(?))>0 ORDER BY id LIMIT 3")?
            .query_map([q], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
        let hits = store.recall(
            &Query {
                bank: "fixture",
                text: q,
                budget: 3,
                ..Query::default()
            },
            &models,
        )?;
        hybrid += hits.iter().filter(|hit| hit.unit.id == *correct).count();
        baseline += exact.iter().filter(|id| **id == *correct).count();
        println!(
            "q={q:?} correct={correct} baseline={exact:?} hybrid={:?}",
            hits.iter().map(|hit| hit.unit.id).collect::<Vec<_>>()
        );
    }
    let returned_slots = (QUERIES.len() * 3) as f64;
    println!(
        "Macro precision@3: substring {:.3}, hybrid {:.3}",
        baseline as f64 / returned_slots,
        hybrid as f64 / returned_slots
    );
    assert!(
        hybrid > baseline,
        "Fixed fixture must improve over substring search"
    );

    // Repeat the irrelevant log passage's vector at unique rowids. Measures the
    // full 10k cosine scan and real reranking without timing bulk embedding.
    db.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<?)
        INSERT INTO memory_units(bank,origin,source_id,kind,text,source,embedding)
        SELECT bank,origin,'noise-'||i,kind,text,source,embedding FROM n,memory_units WHERE id=?",
        params![10_000 - DOCUMENTS.len(), DOCUMENTS.len()],
    )?;
    let count: i64 = db.query_row("SELECT count(*) FROM memory_units", [], |r| r.get(0))?;
    assert_eq!(count, 10_000);
    println!(
        "10k fixture database: {} bytes",
        std::fs::metadata(path)?.len()
    );
    let mut elapsed = Vec::new();
    for _ in 0..5 {
        for (q, _) in QUERIES {
            let started = Instant::now();
            let hits = store.recall(
                &Query {
                    bank: "fixture",
                    text: q,
                    budget: 3,
                    ..Query::default()
                },
                &models,
            )?;
            assert!(!hits.is_empty());
            elapsed.push(started.elapsed().as_secs_f64() * 1000.0);
        }
    }
    elapsed.sort_by(f64::total_cmp);
    println!(
        "10k warm recall ms (n={}): min={:.1} p50={:.1} p95={:.1} max={:.1}",
        elapsed.len(),
        elapsed[0],
        elapsed[9],
        elapsed[18],
        elapsed[19]
    );
    Ok(())
}
