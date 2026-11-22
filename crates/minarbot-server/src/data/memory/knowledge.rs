use super::*;
use minarbot_memory::{Evidence, Reflection};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct KnowledgePage {
    items: Vec<Knowledge>,
    next_after: Option<i64>,
}

#[derive(Serialize)]
struct Knowledge {
    id: i64,
    text: String,
    kind: String,
    sources: Vec<Value>,
    essential: bool,
    revision: i64,
}

#[derive(Serialize)]
struct StandingAnswer {
    id: String,
    question: String,
    reflection: Option<Reflection>,
    revision: i64,
    pending: bool,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Change {
    Edit {
        id: String,
        unit: i64,
        revision: i64,
        text: String,
    },
    Pin {
        id: String,
        unit: i64,
        revision: i64,
        essential: bool,
    },
    Forget {
        id: String,
        unit: i64,
        revision: i64,
    },
    Restore {
        id: String,
        unit: i64,
        revision: i64,
    },
    SaveQuestion {
        id: String,
        model: String,
        revision: i64,
        question: String,
    },
    ForgetQuestion {
        id: String,
        model: String,
        revision: i64,
    },
    RestoreQuestion {
        id: String,
        model: String,
        revision: i64,
    },
}

pub(crate) fn route(
    db: &Connection,
    kind: &str,
    id: &str,
    models: bool,
    request: &Value,
    q: &[(String, String)],
    get: bool,
) -> Result<Value> {
    let owner = conversations::owner(db, kind, id)?;
    let bank = format!("{kind}:{id}");
    if get {
        return if models {
            if !q.is_empty() {
                return Err(error(400, "Invalid standing question parameters."));
            }
            list_models(db, &bank)
        } else {
            list_units(db, &bank, q)
        };
    }
    if !q.is_empty() {
        return Err(error(400, "Invalid memory parameters."));
    }
    if kind == "channels" && owner["archived"] == 1 {
        return Err(error(409, "Restore the section before changing memory."));
    }
    let command = serde_json::from_value::<Change>(request.clone())
        .map_err(|_| error(400, "Invalid memory change."))?;
    let receipt_request = json!({"bank":bank,"models":models,"command":request});
    let command_id = string(request, "id");
    if let Some(previous) = receipt(
        db,
        "memory_commands",
        command_id,
        &receipt_request,
        "Change ID was already used.",
    )? {
        return Ok(previous);
    }
    let result = match command {
        Change::Edit {
            id,
            unit,
            revision,
            text,
        } if !models && uuid(&id) => {
            let text = text.trim();
            if text.is_empty() || text.encode_utf16().count() > 2000 || text.contains('\0') {
                return Err(error(400, "Enter 1 to 2,000 characters."));
            }
            change_unit(db, &bank, unit, revision, UnitChange::Edit(text))?
        }
        Change::Pin {
            id,
            unit,
            revision,
            essential,
        } if !models && uuid(&id) => {
            change_unit(db, &bank, unit, revision, UnitChange::Pin(essential))?
        }
        Change::Forget { id, unit, revision } if !models && uuid(&id) => {
            change_unit(db, &bank, unit, revision, UnitChange::Remove(true))?
        }
        Change::Restore { id, unit, revision } if !models && uuid(&id) => {
            change_unit(db, &bank, unit, revision, UnitChange::Remove(false))?
        }
        Change::SaveQuestion {
            id,
            model,
            revision,
            question,
        } if models && uuid(&id) => {
            change_model(db, &bank, &model, revision, Some(question.trim()), false)?
        }
        Change::ForgetQuestion {
            id,
            model,
            revision,
        } if models && uuid(&id) => change_model(db, &bank, &model, revision, None, true)?,
        Change::RestoreQuestion {
            id,
            model,
            revision,
        } if models && uuid(&id) => change_model(db, &bank, &model, revision, None, false)?,
        _ => return Err(error(400, "Invalid memory change.")),
    };
    save_receipt(db, "memory_commands", command_id, &receipt_request, &result)?;
    Ok(result)
}

fn list_units(db: &Connection, bank: &str, q: &[(String, String)]) -> Result<Value> {
    let query = param(q, "query").unwrap_or("").trim();
    let kind = param(q, "kind").unwrap_or("all");
    let after = param(q, "after")
        .unwrap_or("0")
        .parse::<i64>()
        .map_err(|_| error(400, "Invalid memory position."))?;
    if query.encode_utf16().count() > 200
        || after < 0
        || !matches!(kind, "all" | "facts" | "observations")
        || q.iter()
            .any(|(key, _)| !matches!(key.as_str(), "query" | "kind" | "after"))
    {
        return Err(error(400, "Invalid memory search."));
    }
    let rows = all(db,
        "SELECT u.id,u.text,u.kind,u.source,coalesce(c.essential,0) AS essential,coalesce(c.revision,1) AS revision
         FROM memory_units u LEFT JOIN memory_unit_controls c ON c.unit=u.id
         WHERE u.bank=?1 AND u.origin='retained' AND u.removed=0 AND (?2=0 OR u.id<?2)
         AND (?3='all' OR (?3='observations')=(u.kind='observation'))
         AND instr(lower(u.text),lower(?4))>0 ORDER BY u.id DESC LIMIT 11",
        rusqlite::params![bank, after, kind, query],
    )?;
    let next_after = (rows.len() > 10).then(|| rows[9]["id"].as_i64()).flatten();
    let items = rows
        .into_iter()
        .take(10)
        .map(|row| {
            let id = row["id"].as_i64().ok_or_else(Failure::storage)?;
            Ok(Knowledge {
                id,
                text: string(&row, "text").to_owned(),
                kind: string(&row, "kind").to_owned(),
                sources: sources(db, bank, id, json(&row["source"])?)?,
                essential: row["essential"] == 1,
                revision: row["revision"].as_i64().ok_or_else(Failure::storage)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    serde_json::to_value(KnowledgePage { items, next_after }).map_err(|_| Failure::storage())
}

pub(crate) fn sources(db: &Connection, bank: &str, unit: i64, source: Value) -> Result<Vec<Value>> {
    if source["kind"] == "fact" {
        return Ok(vec![source["message"].clone()]);
    }
    if source["kind"] != "observation" {
        return Ok(vec![source]);
    }
    all(
        db,
        "SELECT DISTINCT json_extract(f.source,'$.message') AS source
         FROM memory_observation_evidence e JOIN memory_units f ON f.id=e.fact
         WHERE e.observation=? AND f.bank=? AND f.removed=0 AND json_type(f.source,'$.message')='object'
         ORDER BY 1 LIMIT 16",
        rusqlite::params![unit, bank],
    )?
    .into_iter()
    .map(|row| json(&row["source"]))
    .collect()
}

pub(crate) fn standing_answer(
    db: &Connection,
    bank: &str,
    query: &str,
) -> Result<Option<Reflection>> {
    let row = one(
        db,
        "SELECT m.answer,m.evidence FROM memory_mental_models m
         LEFT JOIN memory_bank_generations g ON g.bank=m.bank
         WHERE m.bank=? AND lower(m.question)=lower(?) AND m.removed=0
           AND m.generation=coalesce(g.generation,0) AND m.answer<>'' ORDER BY m.id LIMIT 1",
        [bank, query],
    )?;
    row.as_ref().map(reflection).transpose()
}

fn reflection(row: &Value) -> Result<Reflection> {
    Ok(Reflection {
        answer: string(row, "answer").to_owned(),
        evidence: serde_json::from_value::<Vec<Evidence>>(json(&row["evidence"])?)
            .map_err(|_| Failure::storage())?,
    })
}

fn list_models(db: &Connection, bank: &str) -> Result<Value> {
    let rows = all(
        db,
        "SELECT m.id,m.question,m.answer,m.evidence,m.revision,
                m.generation=coalesce(g.generation,0) AND m.answer<>'' AS fresh
         FROM memory_mental_models m LEFT JOIN memory_bank_generations g ON g.bank=m.bank
         WHERE m.bank=? AND m.removed=0 ORDER BY m.id",
        [bank],
    )?;
    let items = rows
        .iter()
        .map(|row| {
            Ok(StandingAnswer {
                id: string(row, "id").to_owned(),
                question: string(row, "question").to_owned(),
                revision: row["revision"].as_i64().ok_or_else(Failure::storage)?,
                pending: row["fresh"] != 1,
                reflection: (row["fresh"] == 1).then(|| reflection(row)).transpose()?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({"items":items}))
}

enum UnitChange<'a> {
    Edit(&'a str),
    Pin(bool),
    Remove(bool),
}

fn change_unit(
    db: &Connection,
    bank: &str,
    unit: i64,
    revision: i64,
    change: UnitChange<'_>,
) -> Result<Value> {
    let is_restore = matches!(change, UnitChange::Remove(false));
    let row = one(
        db,
        "SELECT u.removed,coalesce(c.revision,1) AS revision,coalesce(c.essential,0) AS essential
         FROM memory_units u LEFT JOIN memory_unit_controls c ON c.unit=u.id
         WHERE u.bank=? AND u.id=? AND u.origin='retained'",
        rusqlite::params![bank, unit],
    )?
    .ok_or_else(|| error(404, "Memory not found in this bank."))?;
    if row["revision"] != revision || (row["removed"] == 1) != is_restore {
        return Err(error(409, "Memory changed. Reload before saving."));
    }
    db.execute(
        "INSERT INTO memory_unit_controls(unit,revision,essential,edited) VALUES(?,?,?,?)
         ON CONFLICT(unit) DO UPDATE SET revision=excluded.revision,essential=excluded.essential,
             edited=max(memory_unit_controls.edited,excluded.edited)",
        rusqlite::params![
            unit,
            revision,
            match change {
                UnitChange::Pin(pinned) => i64::from(pinned),
                _ => row["essential"].as_i64().ok_or_else(Failure::storage)?,
            },
            matches!(change, UnitChange::Edit(_))
        ],
    )?;
    match change {
        UnitChange::Edit(text) => db.execute(
            "UPDATE memory_units SET text=?,embedding=NULL WHERE id=?",
            rusqlite::params![text, unit],
        )?,
        UnitChange::Remove(removed) => db.execute(
            "UPDATE memory_units SET removed=? WHERE id=?",
            rusqlite::params![removed, unit],
        )?,
        UnitChange::Pin(_) => 0,
    };
    db.execute(
        "UPDATE memory_unit_controls SET revision=? WHERE unit=?",
        rusqlite::params![revision + 1, unit],
    )?;
    check_pinned(db, bank)?;
    Ok(json!({"unit":unit,"revision":revision+1}))
}

fn change_model(
    db: &Connection,
    bank: &str,
    model: &str,
    revision: i64,
    question: Option<&str>,
    removed: bool,
) -> Result<Value> {
    if !uuid(model)
        || !(0..9_007_199_254_740_991).contains(&revision)
        || question.is_some_and(|text| {
            text.is_empty() || text.encode_utf16().count() > 2000 || text.contains('\0')
        })
    {
        return Err(error(
            400,
            "Enter a standing question of up to 2,000 characters.",
        ));
    }
    if (revision == 0 || question.is_none() && !removed)
        && exists(
            db,
            "SELECT 1 FROM memory_mental_models WHERE bank=? AND removed=0 LIMIT 1 OFFSET 15",
            [bank],
        )?
    {
        return Err(error(413, "This bank already has 16 standing questions."));
    }
    let changed = if revision == 0 {
        let question = question.ok_or_else(|| error(400, "Enter a standing question."))?;
        db.execute(
            "INSERT INTO memory_banks VALUES(?) ON CONFLICT DO NOTHING",
            [bank],
        )?;
        db.execute(
            "INSERT INTO memory_mental_models(bank,id,question) VALUES(?,?,?) ON CONFLICT DO NOTHING",
            [bank, model, question],
        )?
    } else if let Some(question) = question {
        db.execute(
            "UPDATE memory_mental_models SET question=?,revision=revision+1,answer='',evidence='[]',generation=-1,retry_at=0
             WHERE bank=? AND id=? AND revision=? AND removed=0",
            rusqlite::params![question, bank, model, revision],
        )?
    } else {
        db.execute(
            "UPDATE memory_mental_models SET removed=?1,revision=revision+1
             WHERE bank=?2 AND id=?3 AND revision=?4 AND removed<>?1",
            rusqlite::params![removed, bank, model, revision],
        )?
    };
    if changed == 0 {
        return Err(error(
            409,
            "Standing question changed. Reload before saving.",
        ));
    }
    Ok(json!({"model":model,"revision":revision+1}))
}
