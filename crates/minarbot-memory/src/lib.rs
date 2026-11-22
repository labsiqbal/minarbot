// Minarbot Memory, based on Hindsight v1.2.3, bd230e8bb40e768bf69f266f1dd3c9ae03e22fe3.
// Retain: hindsight-api-slim/hindsight_api/engine/retain/orchestrator.py.

#![forbid(unsafe_code)]

mod models;
mod recall;
mod retain;

pub use models::LocalModels;
pub use recall::{Hit, Query};
pub use retain::Work;

use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::{json, Value};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub const DIMENSIONS: usize = 384;

/// Embeddings are L2-normalized. Reranking returns one relevance logit per pair.
pub trait Models {
    fn embed(&self, text: &[&str], query: bool) -> Result<Vec<[f32; DIMENSIONS]>>;
    fn rerank(&self, query: &str, passages: &[&str]) -> Result<Vec<f32>>;
}

#[derive(Clone, Copy, Default)]
pub enum Kind {
    #[default]
    World,
    Experience,
    Observation,
}

#[derive(Clone, Copy)]
pub enum Link {
    Entity,
    Semantic,
    Temporal,
}

#[derive(Default)]
pub struct Retain<'a> {
    pub kind: Kind,
    pub occurred_at: Option<i64>,
    pub mentioned_at: Option<i64>,
    pub entities: &'a [&'a str],
    pub source: Value,
    pub metadata: Value,
}

#[derive(Debug, Serialize)]
pub struct Unit {
    pub id: i64,
    pub bank: String,
    pub kind: String,
    pub text: String,
    pub occurred_at: Option<i64>,
    pub mentioned_at: Option<i64>,
    pub source: Value,
    pub metadata: Value,
    pub entities: Vec<String>,
}

pub fn initialize(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(concat!(
        include_str!("schema.sql"),
        include_str!("retain.sql")
    ))
}

pub struct Store<'a> {
    db: &'a Connection,
}

impl<'a> Store<'a> {
    /// Use a connection initialized with `initialize` and foreign keys enabled.
    pub fn new(db: &'a Connection) -> Self {
        Self { db }
    }

    pub fn retain(
        &self,
        bank: &str,
        text: &str,
        metadata: Retain<'_>,
        models: &impl Models,
    ) -> Result<i64> {
        if bank.trim().is_empty() || text.trim().is_empty() {
            return Err("Memory requires a bank and non-empty text".into());
        }
        let embedding = embeddings(models, &[text], false)?.remove(0);
        let source_id = uuid::Uuid::new_v4().to_string();
        let source = if metadata.source.is_null() {
            json!({"kind":"retained","id":source_id})
        } else {
            metadata.source
        };
        let tx = self.db.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO memory_banks VALUES(?) ON CONFLICT DO NOTHING",
            [bank],
        )?;
        let kind = match metadata.kind {
            Kind::World => "world",
            Kind::Experience => "experience",
            Kind::Observation => "observation",
        };
        let unit = tx.query_row(
            "INSERT INTO memory_units(bank,origin,source_id,kind,text,occurred_at,mentioned_at,source,metadata,embedding)
             VALUES(?,'retained',?,?,?,?,coalesce(?,CAST(unixepoch('subsec')*1000 AS INTEGER)),?,?,?) RETURNING id",
            params![bank, source_id, kind, text, metadata.occurred_at,
                metadata.mentioned_at, source.to_string(), metadata.metadata.to_string(), blob(&embedding)],
            |row| row.get::<_, i64>(0),
        )?;
        let mut insert =
            tx.prepare("INSERT INTO memory_unit_entities VALUES(?,?,?) ON CONFLICT DO NOTHING")?;
        for entity in metadata.entities {
            let entity = entity
                .split_whitespace()
                .map(str::to_lowercase)
                .collect::<Vec<_>>()
                .join(" ");
            if !entity.is_empty() {
                insert.execute(params![bank, unit, entity])?;
            }
        }
        drop(insert);
        tx.commit()?;
        Ok(unit)
    }

    pub fn link(&self, bank: &str, source: i64, target: i64, kind: Link) -> Result<()> {
        let kind = match kind {
            Link::Entity => "entity",
            Link::Semantic => "semantic",
            Link::Temporal => "temporal",
        };
        self.db.execute(
            "INSERT INTO memory_links(bank,source,target,kind)
             SELECT ?1,?2,?3,?4 WHERE (SELECT count(*) FROM memory_units
                 WHERE bank=?1 AND id IN(?2,?3) AND removed=0)=2 ON CONFLICT DO NOTHING",
            params![bank, source, target, kind],
        )?;
        Ok(())
    }

    /// Embed one bounded batch of imported or edited records. Updates are fenced
    /// by row identity and original text so a concurrent edit can retry safely.
    pub fn embed_pending(&self, bank: &str, models: &impl Models) -> Result<usize> {
        let rows = self
            .db
            .prepare(
                "SELECT id,origin,source_id,text FROM memory_units
             WHERE bank=? AND embedding IS NULL AND removed=0 ORDER BY id LIMIT 16",
            )?
            .query_map([bank], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if rows.is_empty() {
            return Ok(0);
        }
        let text = rows.iter().map(|row| row.3.as_str()).collect::<Vec<_>>();
        let vectors = embeddings(models, &text, false)?;
        let tx = self.db.unchecked_transaction()?;
        for ((id, origin, source, text), vector) in rows.iter().zip(vectors) {
            tx.execute(
                "UPDATE memory_units SET embedding=? WHERE bank=? AND id=? AND origin=?
                    AND source_id=? AND text=? AND removed=0 AND embedding IS NULL",
                params![blob(&vector), bank, id, origin, source, text],
            )?;
        }
        tx.commit()?;
        Ok(rows.len())
    }
}

fn embeddings(models: &impl Models, texts: &[&str], query: bool) -> Result<Vec<[f32; DIMENSIONS]>> {
    let vectors = models.embed(texts, query)?;
    if vectors.len() != texts.len() || vectors.iter().flatten().any(|n| !n.is_finite()) {
        return Err("Invalid embedding batch".into());
    }
    Ok(vectors)
}

fn blob(vector: &[f32; DIMENSIONS]) -> Vec<u8> {
    vector
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}
