// Hindsight v1.2.3, bd230e8bb40e768bf69f266f1dd3c9ae03e22fe3.
// Adapted from hindsight-api-slim/hindsight_api/engine/search/{retrieval,graph_retrieval,fusion,reranking}.py.

use crate::{embeddings, Models, Result, Store, Unit, DIMENSIONS};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Default)]
pub struct Query<'a> {
    pub bank: &'a str,
    pub text: &'a str,
    pub budget: usize,
    /// Inclusive UTC Unix milliseconds. Unknown dates are excluded if bounded.
    pub after: Option<i64>,
    pub before: Option<i64>,
    pub history_only: bool,
}

#[derive(Debug, Serialize)]
pub struct Hit {
    pub unit: Unit,
    pub rrf: f64,
    pub rerank: Option<f32>,
}

const CANDIDATES: usize = 64;
const RRF_K: usize = 60;
const FILTERS: &str = "
    u.bank=?1 AND u.removed=0 AND (?2 IS NULL OR u.occurred_at>=?2)
    AND (?3 IS NULL OR u.occurred_at<=?3) AND (NOT ?4 OR u.origin IN ('prompt','reply'))";

impl Store<'_> {
    /// BM25-only search, available offline while model-backed indexing catches up.
    pub fn keyword(&self, query: &Query<'_>) -> Result<Vec<Hit>> {
        query.validate()?;
        let _snapshot = self.snapshot()?;
        let ids = lexical(self.db, query)?;
        ids.into_iter()
            .take(query.budget)
            .map(|id| self.hit(id, 0.0))
            .collect()
    }

    pub fn recall(&self, query: &Query<'_>, models: &impl Models) -> Result<Vec<Hit>> {
        query.validate()?;
        if query.budget == 0
            || (query.text.trim().is_empty() && query.after.is_none() && query.before.is_none())
        {
            return Ok(Vec::new());
        }
        let _snapshot = self.snapshot()?;
        let keyword = lexical(self.db, query)?;
        let semantic = if query.text.trim().is_empty() {
            Vec::new()
        } else {
            let vector = embeddings(models, &[query.text], true)?.remove(0);
            vectors(self.db, query, &vector)?
        };
        let seeds: Vec<_> = keyword
            .iter()
            .take(6)
            .chain(semantic.iter().take(2))
            .copied()
            .collect();
        let graph = graph(self.db, query, &seeds)?;
        let temporal = temporal(self.db, query)?;
        let mut fused = HashMap::new();
        for arm in [&semantic, &keyword, &graph, &temporal] {
            for (rank, id) in arm.iter().enumerate() {
                *fused.entry(*id).or_insert(0.0_f64) += 1.0 / (RRF_K + rank + 1) as f64;
            }
        }
        let mut ranks: Vec<_> = fused.into_iter().collect();
        ranks.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut hits = ranks
            .into_iter()
            .take(query.budget.max(16))
            .map(|(id, score)| self.hit(id, score))
            .collect::<Result<Vec<_>>>()?;
        if !hits.is_empty() && !query.text.trim().is_empty() {
            for chunk in hits.chunks_mut(8) {
                let passages: Vec<_> = chunk.iter().map(|hit| hit.unit.text.as_str()).collect();
                let scores = models.rerank(query.text, &passages)?;
                if scores.len() != chunk.len() || scores.iter().any(|score| !score.is_finite()) {
                    return Err("Invalid cross-encoder scores".into());
                }
                for (hit, score) in chunk.iter_mut().zip(scores) {
                    hit.rerank = Some(score);
                }
            }
            hits.sort_by(|a, b| {
                b.rerank
                    .unwrap_or_default()
                    .total_cmp(&a.rerank.unwrap_or_default())
            });
        }
        hits.truncate(query.budget);
        Ok(hits)
    }

    fn snapshot(&self) -> rusqlite::Result<Option<rusqlite::Transaction<'_>>> {
        self.db
            .is_autocommit()
            .then(|| self.db.unchecked_transaction())
            .transpose()
    }

    fn hit(&self, id: i64, rrf: f64) -> Result<Hit> {
        let (bank, kind, text, occurred_at, mentioned_at, source, metadata) = self.db.query_row(
            "SELECT bank,kind,text,occurred_at,mentioned_at,source,metadata FROM memory_units WHERE id=?",
            [id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,
                row.get::<_, String>(5)?,row.get::<_, String>(6)?)),
        )?;
        let entities = self
            .db
            .prepare(
                "SELECT entity FROM memory_unit_entities WHERE bank=? AND unit=? ORDER BY entity",
            )?
            .query_map(params![bank, id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let unit = Unit {
            id,
            bank,
            kind,
            text,
            occurred_at,
            mentioned_at,
            source: serde_json::from_str(&source)?,
            metadata: serde_json::from_str(&metadata)?,
            entities,
        };
        Ok(Hit {
            unit,
            rrf,
            rerank: None,
        })
    }
}

impl Query<'_> {
    fn validate(&self) -> Result<()> {
        if self.bank.trim().is_empty()
            || self.text.chars().count() > 2000
            || self.budget > CANDIDATES
            || self
                .after
                .zip(self.before)
                .is_some_and(|(after, before)| after > before)
        {
            return Err("Invalid recall scope, query, budget or date range".into());
        }
        Ok(())
    }
}

fn lexical(db: &Connection, q: &Query<'_>) -> Result<Vec<i64>> {
    let query = q
        .text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .take(64)
        .map(|token| format!("\"{token}\""))
        .collect::<Vec<_>>()
        .join(" OR ");
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT u.id FROM memory_fts JOIN memory_units u ON u.id=memory_fts.rowid
        WHERE {FILTERS} AND memory_fts MATCH ?5 ORDER BY bm25(memory_fts),u.id LIMIT {CANDIDATES}"
    );
    ids(
        db,
        &sql,
        params![q.bank, q.after, q.before, q.history_only, query],
    )
}

fn vectors(db: &Connection, q: &Query<'_>, vector: &[f32; DIMENSIONS]) -> Result<Vec<i64>> {
    let mut stmt = db.prepare(&format!(
        "SELECT u.id,u.embedding FROM memory_units u
        WHERE {FILTERS} AND u.embedding IS NOT NULL"
    ))?;
    let mut rows = stmt.query(params![q.bank, q.after, q.before, q.history_only])?;
    let mut scored = Vec::new();
    while let Some(row) = rows.next()? {
        // Exact cosine on unit vectors, scoped before scoring. Avoid a platform SQLite extension.
        let dot = row
            .get_ref(1)?
            .as_blob()?
            .chunks_exact(4)
            .zip(vector)
            .map(|(bytes, value)| {
                f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) * value
            })
            .sum::<f32>();
        scored.push((row.get(0)?, dot));
    }
    scored.sort_unstable_by(|a: &(i64, f32), b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    Ok(scored
        .into_iter()
        .take(CANDIDATES)
        .map(|(id, _)| id)
        .collect())
}

fn graph(db: &Connection, q: &Query<'_>, seeds: &[i64]) -> Result<Vec<i64>> {
    if seeds.is_empty() {
        return Ok(Vec::new());
    }
    // UNION deduplicates at each depth. Cap expansion and never traverse a removed or foreign unit.
    let sql = format!("WITH RECURSIVE walk(id,depth) AS (
        SELECT value,0 FROM json_each(?5)
        UNION
        SELECT l.target,walk.depth+1 FROM walk JOIN memory_links l ON l.bank=?1 AND l.source=walk.id
          JOIN memory_units u ON u.id=l.target WHERE walk.depth<2 AND {FILTERS}
        UNION
        SELECT l.source,walk.depth+1 FROM walk JOIN memory_links l ON l.bank=?1 AND l.target=walk.id
          JOIN memory_units u ON u.id=l.source WHERE walk.depth<2 AND {FILTERS}
        UNION
        SELECT b.unit,walk.depth+1 FROM walk JOIN memory_unit_entities a ON a.bank=?1 AND a.unit=walk.id
          JOIN memory_unit_entities b ON b.bank=a.bank AND b.entity=a.entity AND b.unit<>a.unit
          JOIN memory_units u ON u.id=b.unit WHERE walk.depth<2 AND {FILTERS}
        LIMIT 512
    ) SELECT id FROM walk WHERE depth>0 AND id NOT IN (SELECT value FROM json_each(?5))
      GROUP BY id ORDER BY min(depth),id LIMIT {CANDIDATES}");
    ids(
        db,
        &sql,
        params![
            q.bank,
            q.after,
            q.before,
            q.history_only,
            serde_json::to_string(seeds)?
        ],
    )
}

fn temporal(db: &Connection, q: &Query<'_>) -> Result<Vec<i64>> {
    if q.after.is_none() && q.before.is_none() {
        return Ok(Vec::new());
    }
    ids(
        db,
        &format!(
            "SELECT u.id FROM memory_units u WHERE {FILTERS}
        ORDER BY u.occurred_at DESC,u.id LIMIT {CANDIDATES}"
        ),
        params![q.bank, q.after, q.before, q.history_only],
    )
}

fn ids(db: &Connection, sql: &str, args: impl rusqlite::Params) -> Result<Vec<i64>> {
    Ok(db
        .prepare(sql)?
        .query_map(args, |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}
