// Hindsight v1.2.3, bd230e8bb40e768bf69f266f1dd3c9ae03e22fe3.
// Adapted from hindsight-api-slim/hindsight_api/engine/consolidation/consolidator.py.

use super::*;
use std::collections::BTreeMap;

pub(super) struct Batch {
    facts: Vec<Memory>,
    candidates: Vec<Memory>,
    closest: Vec<Vec<usize>>,
}

struct Memory {
    id: i64,
    text: String,
    start: Option<i64>,
    end: Option<i64>,
    mentioned: Option<i64>,
    proofs: i64,
}

const COLUMNS: &str = "u.id,u.text,u.occurred_at,json_extract(u.metadata,'$.occurred_end'),u.mentioned_at,coalesce(json_extract(u.metadata,'$.proof_count'),0)";

impl Memory {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            text: row.get(1)?,
            start: row.get(2)?,
            end: row.get(3)?,
            mentioned: row.get(4)?,
            proofs: row.get(5)?,
        })
    }

    fn prompt(&self, id: String) -> Value {
        json!({
            "id":id,"text":self.text,"mentioned_at":timestamp(self.mentioned),
            "occurred_start":timestamp(self.start),"occurred_end":timestamp(self.end),
        })
    }
}

impl Batch {
    pub(super) fn read(db: &Connection, source: &Source, now: i64, attempts: i64) -> Result<Self> {
        let facts = db
            .prepare(&format!(
                "SELECT {COLUMNS} FROM memory_facts f JOIN memory_units u ON u.id=f.unit
             WHERE f.raw=? AND f.offset=? AND u.removed=0 AND f.consolidated=0 AND f.ready_at<=? AND f.attempts=?
             ORDER BY f.unit LIMIT {MAX_FACTS}"
            ))?
            .query_map(
                params![source.raw, source.offset, now, attempts],
                Memory::read,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut candidates: Vec<Memory> = Vec::new();
        let mut closest = Vec::new();
        let mut statement = db.prepare(&format!(
            "SELECT {COLUMNS} FROM memory_fts JOIN memory_observations o ON o.unit=memory_fts.rowid
             JOIN memory_units u ON u.id=o.unit
             WHERE o.bank=? AND o.agent=? AND memory_fts MATCH ?
             AND NOT EXISTS(SELECT 1 FROM memory_unit_controls c WHERE c.unit=o.unit AND (c.edited=1 OR c.essential=1))
             ORDER BY bm25(memory_fts),u.id DESC LIMIT 3"
        ))?;
        // ponytail: use FTS candidates offline. Add vector candidates if lexical recall misses paraphrases.
        for fact in &facts {
            let mut matches = Vec::new();
            let query = fts_expression(&fact.text);
            if !query.is_empty() {
                for observation in
                    statement.query_map(params![source.bank, agent(source), query], Memory::read)?
                {
                    let observation = observation?;
                    if let Some(i) = candidates.iter().position(|c| c.id == observation.id) {
                        matches.push(i);
                    } else if candidates.len() < 12 {
                        matches.push(candidates.len());
                        candidates.push(observation);
                    }
                }
            }
            closest.push(matches);
        }
        Ok(Self {
            facts,
            candidates,
            closest,
        })
    }

    pub(super) fn prompt(&self) -> Value {
        let facts = self.facts.iter().enumerate().map(|(i, fact)| {
            let mut value = fact.prompt(format!("F{}",i+1));
            value["closest_observations"] = json!(self.closest[i].iter().map(|&j| {
                json!({"id":format!("O{}",j+1),"stated_after_fact":self.candidates[j].mentioned > fact.mentioned})
            }).collect::<Vec<_>>());
            value
        }).collect::<Vec<_>>();
        let latest = self.facts.iter().filter_map(|f| f.mentioned).max();
        let observations = self
            .candidates
            .iter()
            .enumerate()
            .map(|(i, observation)| {
                let mut value = observation.prompt(format!("O{}", i + 1));
                value["proof_count"] = json!(observation.proofs);
                value["newer_than_input_facts"] = json!(observation.mentioned > latest);
                value
            })
            .collect::<Vec<_>>();
        json!({"facts":facts,"observations":observations})
    }

    pub(super) fn current(&self, db: &Connection, attempts: i64) -> Result<bool> {
        for fact in &self.facts {
            if !db.query_row(
                "SELECT EXISTS(SELECT 1 FROM memory_facts f JOIN memory_units u ON u.id=f.unit
                 WHERE f.unit=? AND f.consolidated=0 AND f.attempts=? AND u.removed=0 AND u.text=?)",
                params![fact.id,attempts,fact.text], |r| r.get::<_, bool>(0),
            )? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(super) fn retry(&self, db: &Connection, ready_at: i64) -> Result<()> {
        for fact in &self.facts {
            db.execute(
                "UPDATE memory_facts SET attempts=attempts+1,ready_at=? WHERE unit=?",
                params![ready_at, fact.id],
            )?;
        }
        Ok(())
    }

    pub(super) fn apply(&self, db: &Connection, source: &Source, answer: &str) -> Result<()> {
        for observation in &self.candidates {
            if !db.query_row(
                "SELECT EXISTS(SELECT 1 FROM memory_units WHERE id=? AND removed=0 AND text=? AND mentioned_at IS ?
                 AND NOT EXISTS(SELECT 1 FROM memory_unit_controls c WHERE c.unit=memory_units.id AND (c.edited=1 OR c.essential=1)))",
                params![observation.id,observation.text,observation.mentioned],
                |r| r.get::<_, bool>(0),
            )? {
                return Ok(());
            }
        }

        let response: Decisions = parse(answer)?;
        if response.facts.len() != self.facts.len()
            || response.deletes.len() > self.candidates.len()
        {
            return Err("Invalid consolidation decision count".into());
        }
        let mut affected: Vec<BTreeSet<i64>> = Vec::new();
        let mut merged = BTreeMap::new();
        for (i, (fact, result)) in self.facts.iter().zip(response.facts).enumerate() {
            if index(&result.fact_id, 'F', self.facts.len())? != i
                || result.new.len() > 4
                || result.existing.len() > self.candidates.len()
            {
                return Err("Invalid consolidation fact decision".into());
            }
            let mut observations = BTreeSet::new();
            if matches!(result.kind, Duration::Lasting) {
                for change in result.existing {
                    let candidate = &self.candidates
                        [index(&change.observation_id, 'O', self.candidates.len())?];
                    if !matches!(change.effect, Effect::Unrelated) {
                        let id = resolve(&merged, candidate.id);
                        let observation = update(db, source, fact, id, change)?;
                        if observation != id {
                            merged.insert(id, observation);
                        }
                        observations.insert(observation);
                    }
                }
                if let Some(previous) = result.same_as_fact {
                    observations.extend(&affected[index(&previous, 'F', i)?]);
                }
                for text in result.new {
                    observations.insert(create(db, source, checked_text(&text, 1200)?)?);
                }
                if observations.is_empty() {
                    // A lasting fact must end up somewhere.
                    observations.insert(create(db, source, &fact.text)?);
                }
                for &observation in &observations {
                    cite(db, source, resolve(&merged, observation), fact.id)?;
                }
            }
            affected.push(observations);
            db.execute(
                "UPDATE memory_facts SET consolidated=1 WHERE unit=?",
                [fact.id],
            )?;
        }

        let mut deletes = BTreeSet::new();
        let mut targets = BTreeSet::new();
        for deletion in response.deletes {
            let from =
                self.candidates[index(&deletion.observation_id, 'O', self.candidates.len())?].id;
            let to = self.candidates[index(&deletion.merged_into, 'O', self.candidates.len())?].id;
            let from = resolve(&merged, from);
            let to = resolve(&merged, to);
            if from == to {
                continue;
            }
            if !deletes.insert(from) || targets.contains(&from) || deletes.contains(&to) {
                return Err("Invalid observation merge".into());
            }
            targets.insert(to);
            merge(db, source, from, to)?;
        }
        Ok(())
    }
}

fn resolve(merged: &BTreeMap<i64, i64>, mut id: i64) -> i64 {
    while let Some(&target) = merged.get(&id) {
        id = target;
    }
    id
}

#[derive(Deserialize)]
struct Decisions {
    facts: Vec<Decision>,
    #[serde(default)]
    deletes: Vec<Merge>,
}

#[derive(Deserialize)]
struct Decision {
    fact_id: String,
    kind: Duration,
    #[serde(default)]
    existing: Vec<Change>,
    #[serde(default)]
    new: Vec<String>,
    same_as_fact: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Duration {
    Lasting,
    Momentary,
}

#[derive(Deserialize)]
struct Change {
    observation_id: String,
    effect: Effect,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Effect {
    Unrelated,
    Repeats,
    Changes,
    Ends,
}

#[derive(Deserialize)]
struct Merge {
    observation_id: String,
    merged_into: String,
}

fn index(id: &str, prefix: char, len: usize) -> Result<usize> {
    id.strip_prefix(prefix)
        .and_then(|v| v.parse::<usize>().ok())
        .and_then(|n| n.checked_sub(1))
        .filter(|n| *n < len)
        .ok_or_else(|| "Unknown memory model reference".into())
}

fn agent(source: &Source) -> &str {
    source.info["agent"].as_str().unwrap_or_default()
}

fn key(text: &str) -> String {
    normalize(text.trim().trim_end_matches('.'))
}

fn duplicate(db: &Connection, source: &Source, key: &str) -> rusqlite::Result<Option<i64>> {
    db.query_row(
        "SELECT unit FROM memory_observations WHERE bank=? AND agent=? AND normalized=?",
        params![source.bank, agent(source), key],
        |r| r.get(0),
    )
    .optional()
}

fn create(db: &Connection, source: &Source, text: &str) -> Result<i64> {
    let key = key(text);
    if let Some(id) = duplicate(db, source, &key)? {
        return Ok(id);
    }
    let id = db.query_row(
        "INSERT INTO memory_units(bank,origin,source_id,kind,text,source)
         VALUES(?,'retained',?,'observation',?,json_object('kind','observation')) RETURNING id",
        params![source.bank, uuid::Uuid::new_v4().to_string(), text],
        |r| r.get::<_, i64>(0),
    )?;
    db.execute(
        "INSERT INTO memory_observations VALUES(?,?,?,?)",
        params![id, source.bank, agent(source), key],
    )?;
    Ok(id)
}

fn update(db: &Connection, source: &Source, fact: &Memory, id: i64, change: Change) -> Result<i64> {
    if matches!(change.effect, Effect::Repeats) {
        return Ok(id);
    }
    let (current, mentioned) = db.query_row(
        "SELECT text,mentioned_at FROM memory_units WHERE id=?",
        [id],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?)),
    )?;
    let text = if mentioned > fact.mentioned {
        let before = timestamp(fact.mentioned).unwrap_or_else(|| "Earlier".to_owned());
        let note = format!("({before}: {})", fact.text);
        if current.contains(&note) || current.len() + note.len() >= 2000 {
            return Ok(id);
        }
        format!("{current} {note}")
    } else {
        if change.text.trim().is_empty() {
            return Err("Missing updated observation text".into());
        }
        checked_text(&change.text, 1200)?.to_owned()
    };
    let key = key(&text);
    if let Some(other) = duplicate(db, source, &key)?.filter(|other| *other != id) {
        merge(db, source, id, other)?;
        return Ok(other);
    }
    db.execute(
        "UPDATE memory_observations SET normalized=? WHERE unit=?",
        params![key, id],
    )?;
    db.execute(
        "UPDATE memory_units SET text=?,embedding=NULL WHERE id=? AND text<>?",
        params![text, id, text],
    )?;
    Ok(id)
}

fn cite(db: &Connection, source: &Source, observation: i64, fact: i64) -> Result<()> {
    db.execute(
        "INSERT INTO memory_observation_evidence VALUES(?,?) ON CONFLICT DO NOTHING",
        params![observation, fact],
    )?;
    link(db, &source.bank, observation, fact)?;
    db.execute(
        "INSERT INTO memory_unit_entities(bank,unit,entity)
         SELECT bank,?1,entity FROM memory_unit_entities WHERE bank=?2 AND unit=?3
         ON CONFLICT DO NOTHING",
        params![observation, source.bank, fact],
    )?;
    refresh(db, observation)?;
    Ok(())
}

fn refresh(db: &Connection, id: i64) -> rusqlite::Result<()> {
    db.execute(
        "WITH stats AS (
           SELECT count(*) proofs,min(u.occurred_at) start,max(coalesce(json_extract(u.metadata,'$.occurred_end'),u.occurred_at)) end,
                  max(u.mentioned_at) mentioned
           FROM memory_observation_evidence e JOIN memory_units u ON u.id=e.fact WHERE e.observation=?1
         ) UPDATE memory_units SET (occurred_at,mentioned_at,metadata)=(
           SELECT start,mentioned,json_object('proof_count',proofs,'occurred_end',end) FROM stats
         ) WHERE id=?1",
        [id],
    )?;
    Ok(())
}

fn merge(db: &Connection, source: &Source, from: i64, to: i64) -> Result<()> {
    db.execute(
        "INSERT INTO memory_observation_evidence SELECT ?1,fact FROM memory_observation_evidence
         WHERE observation=?2 ON CONFLICT DO NOTHING",
        params![to, from],
    )?;
    db.execute(
        "INSERT INTO memory_links SELECT ?1,?2,fact,'semantic' FROM memory_observation_evidence
         WHERE observation=?3 ON CONFLICT DO NOTHING",
        params![source.bank, to, from],
    )?;
    db.execute(
        "INSERT INTO memory_unit_entities SELECT bank,?1,entity FROM memory_unit_entities
         WHERE bank=?2 AND unit=?3 ON CONFLICT DO NOTHING",
        params![to, source.bank, from],
    )?;
    refresh(db, to)?;
    db.execute("DELETE FROM memory_units WHERE id=?", [from])?;
    Ok(())
}
