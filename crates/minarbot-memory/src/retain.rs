// Hindsight v1.2.3, bd230e8bb40e768bf69f266f1dd3c9ae03e22fe3.
// Adapted from hindsight-api-slim/hindsight_api/engine/retain/{fact_extraction,fact_storage,link_creation}.py.

use crate::{recall::fts_expression, Result, Store};
use chrono::{DateTime, NaiveDate, NaiveDateTime, SecondsFormat};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;

mod consolidate;

const CHUNK_BYTES: i64 = 4096;
const MAX_FACTS: usize = 12;

#[derive(Clone)]
struct Source {
    raw: i64,
    generation: i64,
    offset: i64,
    bank: String,
    info: Value,
    mentioned: Option<i64>,
}

pub struct Work {
    source: Source,
    attempts: i64,
    input: Input,
}

enum Input {
    Extract { text: String, end: i64, done: bool },
    Consolidate(consolidate::Batch),
}

impl Work {
    pub fn source(&self) -> &Value {
        &self.source.info
    }

    pub fn extracting(&self) -> bool {
        matches!(self.input, Input::Extract { .. })
    }

    pub fn prompt(&self) -> (&'static str, Value) {
        match &self.input {
            Input::Extract { text, .. } => (
                include_str!("prompts/extraction.txt"),
                json!({
                    "Event Date": timestamp(self.source.mentioned),
                    "role": if self.source.info["role"] == "reply" { "assistant" } else { "user" },
                    "content": text
                }),
            ),
            Input::Consolidate(batch) => {
                (include_str!("prompts/consolidation.txt"), batch.prompt())
            }
        }
    }
}

impl Store<'_> {
    /// Schedule an existing raw unit. Retaining text and scheduling can share a transaction.
    pub fn queue_extraction(&self, raw: i64) -> Result<()> {
        self.db.execute(
            "INSERT INTO memory_extraction(raw) SELECT id FROM memory_units
             WHERE id=? AND removed=0 AND kind<>'observation'
               AND NOT EXISTS (SELECT 1 FROM memory_facts WHERE unit=?1) ON CONFLICT DO NOTHING",
            [raw],
        )?;
        Ok(())
    }

    /// Compute one bounded request. Its result can be committed after releasing the connection.
    pub fn pending_work(&self, now: i64) -> Result<Option<Work>> {
        let fact = self.db.query_row(
            "SELECT f.raw,f.offset,f.attempts FROM memory_facts f JOIN memory_units u ON u.id=f.raw
             WHERE f.consolidated=0 AND f.attempts<3 AND f.ready_at<=? AND u.removed=0
             ORDER BY f.ready_at,f.unit LIMIT 1",
            [now],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).optional()?;
        if let Some((raw, offset, attempts)) = fact {
            let source = read_source(self.db, raw, offset)?;
            let batch = consolidate::Batch::read(self.db, &source, now, attempts)?;
            return Ok(Some(Work {
                source,
                attempts,
                input: Input::Consolidate(batch),
            }));
        }
        let pending = self
            .db
            .query_row(
                "SELECT q.raw,q.offset,q.attempts,substr(CAST(u.text AS BLOB),q.offset+1,?2),
                    length(CAST(u.text AS BLOB))
             FROM memory_extraction q JOIN memory_units u ON u.id=q.raw
             WHERE q.done=0 AND q.attempts<3 AND q.ready_at<=?1 AND u.removed=0
             ORDER BY q.ready_at,q.raw LIMIT 1",
                params![now, CHUNK_BYTES],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get(2)?,
                        r.get::<_, Vec<u8>>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((raw, offset, attempts, bytes, len)) = pending else {
            return Ok(None);
        };
        let source = read_source(self.db, raw, offset)?;
        let text = chunk(&bytes, offset + bytes.len() as i64 == len)?;
        let end = offset + text.len() as i64;
        Ok(Some(Work {
            source,
            attempts,
            input: Input::Extract {
                text: text.to_owned(),
                end,
                done: end == len,
            },
        }))
    }

    /// Commit all facts or observation changes for one model response. An edit
    /// or removal during inference invalidates the request.
    pub fn apply_work(&self, work: &Work, answer: &str) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        if current(&tx, work)? {
            match &work.input {
                Input::Extract { end, done, .. } => {
                    store_facts(&tx, &work.source, answer)?;
                    tx.execute(
                        "UPDATE memory_extraction SET offset=?,done=?,attempts=0,ready_at=0 WHERE raw=?",
                        params![end, done, work.source.raw],
                    )?;
                }
                Input::Consolidate(batch) => batch.apply(&tx, &work.source, answer)?,
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Retry a bounded number of times. A failed stage leaves the raw message
    /// and any facts already committed available for recall.
    pub fn retry_work(&self, work: &Work, now: i64) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        if current(&tx, work)? {
            let ready_at = now.saturating_add(if work.attempts == 0 { 5_000 } else { 30_000 });
            match &work.input {
                Input::Extract { .. } => {
                    tx.execute(
                        "UPDATE memory_extraction SET attempts=attempts+1,ready_at=? WHERE raw=?",
                        params![ready_at, work.source.raw],
                    )?;
                }
                Input::Consolidate(batch) => batch.retry(&tx, ready_at)?,
            }
        }
        tx.commit()?;
        Ok(())
    }
}

fn read_source(db: &Connection, raw: i64, offset: i64) -> Result<Source> {
    let (bank, source, mentioned, generation) = db.query_row(
        "SELECT u.bank,u.source,u.mentioned_at,q.generation
         FROM memory_units u JOIN memory_extraction q ON q.raw=u.id WHERE u.id=?",
        [raw],
        |r| Ok((r.get(0)?, r.get::<_, String>(1)?, r.get(2)?, r.get(3)?)),
    )?;
    Ok(Source {
        raw,
        generation,
        offset,
        bank,
        info: serde_json::from_str(&source)?,
        mentioned,
    })
}

fn current(db: &Connection, work: &Work) -> Result<bool> {
    let source = &work.source;
    let status = db
        .query_row(
            "SELECT q.offset,q.done,q.attempts FROM memory_extraction q
         JOIN memory_units u ON u.id=q.raw
         WHERE q.raw=? AND q.generation=? AND u.removed=0",
            params![source.raw, source.generation],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, bool>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((offset, done, attempts)) = status else {
        return Ok(false);
    };
    match &work.input {
        Input::Extract { .. } => Ok(!done && offset == source.offset && attempts == work.attempts),
        Input::Consolidate(batch) => batch.current(db, work.attempts),
    }
}

fn chunk(bytes: &[u8], last: bool) -> Result<&str> {
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) if !last && err.error_len().is_none() => {
            std::str::from_utf8(&bytes[..err.valid_up_to()])?
        }
        Err(err) => return Err(err.into()),
    };
    if last {
        return Ok(text);
    }
    let cut = text
        .char_indices()
        .rev()
        .find(|(i, c)| *i >= CHUNK_BYTES as usize * 3 / 4 && c.is_whitespace())
        .map_or(text.len(), |(i, c)| i + c.len_utf8());
    Ok(&text[..cut])
}

#[derive(Deserialize)]
struct Extraction {
    facts: Vec<Fact>,
}

#[derive(Deserialize)]
struct Fact {
    what: String,
    when: Option<String>,
    #[serde(rename = "where")]
    where_: Option<String>,
    who: Option<String>,
    why: Option<String>,
    fact_type: FactType,
    fact_kind: FactKind,
    occurred_start: Option<String>,
    occurred_end: Option<String>,
    #[serde(default)]
    entities: Vec<String>,
    causal_relations: Option<Vec<Cause>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum FactType {
    World,
    Assistant,
    Experience,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum FactKind {
    Event,
    Conversation,
}

#[derive(Deserialize)]
struct Cause {
    target_index: usize,
    relation_type: String,
}

fn store_facts(db: &Connection, source: &Source, answer: &str) -> Result<()> {
    let parsed: Extraction = parse(answer)?;
    if parsed.facts.len() > MAX_FACTS {
        return Err("Too many extracted facts".into());
    }
    let mut ids = Vec::with_capacity(parsed.facts.len());
    for (index, fact) in parsed.facts.into_iter().enumerate() {
        let kind = match fact.fact_type {
            FactType::World => "world",
            FactType::Assistant | FactType::Experience => "experience",
        };
        let (start, end) = if matches!(fact.fact_kind, FactKind::Event) {
            let start = parse_date(fact.occurred_start.as_deref());
            let end = parse_date(fact.occurred_end.as_deref());
            (
                start,
                end.filter(|end| start.is_none_or(|start| start <= *end)),
            )
        } else {
            (None, None)
        };
        let mut text = checked_text(&fact.what, 1200)?.to_owned();
        for part in [fact.when, fact.where_, fact.who, fact.why]
            .into_iter()
            .flatten()
        {
            let part = part.trim();
            if !part.is_empty()
                && !["n/a", "null", "unknown", "none"].contains(&part.to_ascii_lowercase().as_str())
                && part.len() <= 200
                && !text.contains(part)
            {
                text.push_str(" | ");
                text.push_str(part);
            }
        }
        let causes = fact
            .causal_relations
            .unwrap_or_default()
            .into_iter()
            .filter(|c| c.relation_type == "caused_by")
            .filter_map(|c| ids.get(c.target_index).copied())
            .collect::<BTreeSet<_>>();
        let source_id = format!(
            "fact:{}:{}:{}:{index}",
            source.raw, source.generation, source.offset
        );
        let id = db.query_row(
            "INSERT INTO memory_units(bank,origin,source_id,kind,text,occurred_at,mentioned_at,source,metadata)
             VALUES(?,'retained',?,?,?,?,?,?,?) RETURNING id",
            params![source.bank,source_id,kind,text,start,source.mentioned,
                json!({"kind":"fact","raw":source.raw,"message":source.info,"offset":source.offset}).to_string(),
                json!({"occurred_end":end,"caused_by":causes}).to_string()],
            |r| r.get::<_, i64>(0),
        )?;
        ids.push(id);
        db.execute(
            "INSERT INTO memory_facts(unit,raw,offset) VALUES(?,?,?)",
            params![id, source.raw, source.offset],
        )?;
        for entity in fact.entities.iter().take(16).map(|e| normalize(e)) {
            if !entity.is_empty() && entity.len() <= 200 {
                db.execute(
                    "INSERT INTO memory_unit_entities VALUES(?,?,?) ON CONFLICT DO NOTHING",
                    params![source.bank, id, entity],
                )?;
            }
        }
        for target in causes.into_iter().chain([source.raw]) {
            link(db, &source.bank, id, target)?;
        }
        if let Some(start) = start {
            // ponytail: bound temporal neighbors to four per fact. Use an interval index if wide-range links are needed.
            db.execute(
                "INSERT INTO memory_links(bank,source,target,kind)
                 SELECT ?1,?2,u.id,'temporal' FROM memory_units u JOIN memory_facts f ON f.unit=u.id
                 WHERE u.bank=?1 AND u.id<>?2 AND u.removed=0 AND u.occurred_at BETWEEN ?3 AND ?4
                 ORDER BY u.occurred_at DESC,u.id LIMIT 4",
                params![
                    source.bank,
                    id,
                    start - 86_400_000,
                    end.unwrap_or(start).saturating_add(86_400_000)
                ],
            )?;
        }
    }
    Ok(())
}

fn link(db: &Connection, bank: &str, from: i64, to: i64) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO memory_links VALUES(?,?,?,'semantic') ON CONFLICT DO NOTHING",
        params![bank, from, to],
    )?;
    Ok(())
}

fn checked_text(text: &str, max: usize) -> Result<&str> {
    let text = text.trim();
    if text.is_empty() || text.len() > max || text.contains('\0') {
        return Err("Invalid memory model text".into());
    }
    Ok(text)
}

fn normalize(text: &str) -> String {
    text.split_whitespace()
        .flat_map(|part| [part, " "])
        .collect::<String>()
        .trim_end()
        .to_lowercase()
}

fn parse<T: DeserializeOwned>(text: &str) -> Result<T> {
    if text.len() > 128 * 1024 {
        return Err("Memory model response too large".into());
    }
    let text = text.trim();
    let text = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .and_then(|inner| inner.strip_suffix("```"))
        .unwrap_or(text);
    serde_json::from_str(text).map_err(|_| "Invalid memory model JSON".into())
}

fn parse_date(text: Option<&str>) -> Option<i64> {
    let text = text?.trim();
    DateTime::parse_from_rfc3339(text)
        .map(|t| t.timestamp_millis())
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|t| t.and_utc().timestamp_millis())
        })
        .or_else(|| {
            NaiveDate::parse_from_str(text, "%Y-%m-%d")
                .ok()?
                .and_hms_opt(0, 0, 0)
                .map(|t| t.and_utc().timestamp_millis())
        })
}

fn timestamp(ms: Option<i64>) -> Option<String> {
    DateTime::from_timestamp_millis(ms?).map(|d| d.to_rfc3339_opts(SecondsFormat::Millis, true))
}
