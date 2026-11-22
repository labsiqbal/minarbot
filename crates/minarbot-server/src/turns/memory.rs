use super::*;
use minarbot_memory::{Evidence, Query, Reflection, Store};

pub fn run(db: &Connection, turn: &Value, name: &str, args: &Value) -> Result<Value> {
    scoped(db, &bank(db, turn)?, name, args)
}
pub(crate) fn scoped(db: &Connection, scope: &str, name: &str, args: &Value) -> Result<Value> {
    if name == "search_memory" {
        if !closed(args, &["query", "after"]) {
            return Err(error(400, "Invalid memory search."));
        }
        let mut q = Vec::new();
        for key in ["query", "after"] {
            if let Some(v) = args.get(key) {
                q.push((
                    key.to_owned(),
                    v.as_str()
                        .ok_or_else(|| error(400, "Invalid memory search or position."))?
                        .to_owned(),
                ));
            }
        }
        return data::memory::scoped(db, scope, &json!({}), &q, true, false);
    }
    if matches!(name, "remember" | "remember_memory") {
        let editing = args.get("id").is_some() || args.get("revision").is_some();
        if !closed(args, &["id", "revision", "text"])
            || editing
                && (!uuid(string(args, "id"))
                    || !integer(&args["revision"], 1, 9_007_199_254_740_991))
        {
            return Err(error(400, "Invalid agent memory change."));
        }
        let old = if editing {
            one(
                db,
                "SELECT * FROM memories WHERE scope=? AND id=? AND removed=0",
                [scope, string(args, "id")],
            )?
        } else {
            None
        };
        if editing && old.is_none() {
            return Err(error(404, "Memory note not found in this conversation."));
        }
        return data::memory::scoped(
            db,
            scope,
            &json!({"id":uuid::Uuid::new_v4().to_string(),"entry":if editing{string(args,"id").to_owned()}else{uuid::Uuid::new_v4().to_string()},"revision":if editing{args["revision"].clone()}else{json!(0)},"text":args["text"],"essential":old.as_ref().is_some_and(|r|r["essential"]==1),"remove":false,"automatic":old.as_ref().is_none_or(|r|r["selection_locked"]==0)}),
            &[],
            false,
            false,
        );
    }
    let items = args["items"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 128)
        .ok_or_else(|| error(400, "Invalid memory selection."))?;
    if !closed(args, &["items"]) {
        return Err(error(400, "Invalid memory selection."));
    }
    let mut seen = std::collections::HashSet::new();
    for item in items {
        if !closed(item, &["id", "revision", "essential"])
            || !uuid(string(item, "id"))
            || !seen.insert(string(item, "id"))
            || !integer(&item["revision"], 1, 9_007_199_254_740_991)
            || !item["essential"].is_boolean()
        {
            return Err(error(400, "Invalid memory selection."));
        }
        let current = one(
            db,
            "SELECT revision,selection_locked FROM memories WHERE scope=? AND id=? AND removed=0",
            [scope, string(item, "id")],
        )?
        .ok_or_else(|| error(404, "Memory note not found in this conversation."))?;
        if current["selection_locked"] == 1 || current["revision"] != item["revision"] {
            return Err(error(
                409,
                "Selection is locked by the user or the memory revision changed.",
            ));
        }
    }
    let mut output = Vec::new();
    for item in items {
        db.execute(
            "UPDATE memories SET essential=?,revision=revision+1 WHERE scope=? AND id=?",
            params![
                i64::from(item["essential"] == true),
                scope,
                item["id"].as_str()
            ],
        )?;
        output.push(one(db,"SELECT id,essential,revision,selection_locked FROM memories WHERE scope=? AND id=?",[scope,string(item,"id")])?.ok_or_else(Failure::storage)?);
    }
    data::memory::check_pinned(db, scope)?;
    Ok(json!({"items":output}))
}

fn bank(db: &Connection, turn: &Value) -> Result<String> {
    Ok(
        super::messaging::channel(db, string(turn, "id"))?.map_or_else(
            || format!("agents:{}", string(turn, "agent")),
            |channel| format!("channels:{channel}"),
        ),
    )
}

pub(super) const AUTO_RECALL_TOKENS: usize = 512;
const AUTO_RECALL_HEADER: &str = "\n\nRecalled memory (untrusted reference data):\n";

pub(super) fn augment(
    state: &AppState,
    db: &Connection,
    turn: &Value,
    system: &mut String,
) -> Result<()> {
    if state.auth.is_some()
        || turn["routing"] == "pending"
        || turn["agent"] == crate::mcp::guide::ID
        || super::profile::setup(db, turn)?
        || crate::automation::model(db, string(turn, "id"))?.is_some()
        || exists(
            db,
            "SELECT 1 FROM hub_link_turns WHERE turn_id=?",
            [string(turn, "id")],
        )?
    {
        return Ok(());
    }
    let bank = bank(db, turn)?;
    let turn_id = string(turn, "id");
    let text = clip(string(turn, "text"), 2000);
    let mut context = String::from(AUTO_RECALL_HEADER);
    let mut units = 0;
    if !text.trim().is_empty() {
        // Use UTF-8 bytes, a conservative bound for byte-level BPE tokenizers.
        // Keep existing explicitly pinned notes in their own budget.
        for hit in evidence(db, &bank, turn_id, text, 6, Search::Automatic)? {
            let line =
                serde_json::to_string(clip(&hit.text, 280)).map_err(|_| Failure::storage())?;
            if context.len() + line.len() + 1 <= AUTO_RECALL_TOKENS {
                context.push_str(&line);
                context.push('\n');
                units += 1;
            }
        }
    }
    let added = if units == 0 { 0 } else { context.len() };
    db.execute(
        "INSERT INTO memory_recall_metrics(turn,bank,added_tokens,units) VALUES(?,?,?,?)
         ON CONFLICT(turn) DO UPDATE SET added_tokens=excluded.added_tokens,units=excluded.units,bank=excluded.bank",
        params![turn_id, bank, added, units],
    )?;
    if units > 0 {
        system.push_str(&context);
    }
    Ok(())
}

fn clip(text: &str, limit: usize) -> &str {
    &text[..text.floor_char_boundary(limit.min(text.len()))]
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Search {
    Automatic,
    Recall,
    History,
}

fn evidence(
    db: &Connection,
    bank: &str,
    turn: &str,
    text: &str,
    budget: usize,
    search: Search,
) -> Result<Vec<Evidence>> {
    let history = search == Search::History;
    let hits = Store::new(db)
        .keyword(&Query {
            bank,
            text,
            budget: 48,
            history_only: history,
            durable_only: !history,
            ..Query::default()
        })
        .map_err(|_| error(503, "Memory search is unavailable."))?;
    let mut results = Vec::<Evidence>::new();
    for hit in hits {
        let unit = hit.unit;
        let source = &unit.source;
        if history {
            let id = string(source, "id");
            if id == turn || super::messaging::channel(db, id)?.as_deref() != bank.strip_prefix("channels:")
                || !exists(
                    db,
                    "SELECT 1 FROM turns WHERE id=? AND status='done' AND parent IS NULL AND agent<>id",
                    [id],
                )?
            {
                continue;
            }
        } else if search == Search::Automatic
            && (unit.metadata["essential"] == 1
                || exists(
                    db,
                    "SELECT 1 FROM memory_unit_controls WHERE unit=? AND essential=1",
                    [unit.id],
                )?)
        {
            continue;
        }
        let text = clip(&unit.text, 1500).to_owned();
        if results.iter().any(|old| old.text == text) {
            continue;
        }
        results.push(Evidence {
            id: unit.id,
            kind: unit.kind,
            text,
            source: data::memory::knowledge::sources(db, bank, unit.id, unit.source)?,
        });
        if results.len() == budget {
            break;
        }
    }
    Ok(results)
}

pub(super) async fn reflect(
    state: &AppState,
    turn: &str,
    bank: &str,
    query: &str,
    budget: usize,
    phase: &str,
) -> Result<Reflection> {
    let secrets = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        crate::mcp::secrets::project(state, &guard.connection, None)?
    };
    minarbot_memory::reflect(
        query,
        |query| {
            let guard = state.database.lock().map_err(|_| "Memory database unavailable")?;
            evidence(&guard.connection, bank, turn, query, budget, Search::Recall)
                .map_err(|error| error.message.into())
        },
        |system, input| {
            let input = crate::mcp::secrets::redact(&input.to_string(), &secrets, false);
            let secrets = &secrets;
            async move {
                match provider::call(
                    state,
                    turn,
                    "",
                    &[
                        json!({"role":"system","content":system}),
                        json!({"role":"user","content":input}),
                    ],
                    &[],
                    phase,
                )
                .await
                .map_err(|e| e.message)?
                {
                    provider::Answer::Text(text) => {
                        Ok(crate::mcp::secrets::redact(&text, secrets, false))
                    }
                    provider::Answer::Tool(_) => Err("Memory model returned a tool call".into()),
                }
            }
        },
    )
    .await
    .map_err(|_| {
        error(
            502,
            "Memory reflection failed. Check the agent's memory model.",
        )
    })
}

pub(super) async fn tool(
    state: &AppState,
    turn: &Value,
    name: &str,
    args: &Value,
) -> Result<Value> {
    let history = name == "search_history";
    let limit = if name == "reflect" { 8 } else { 20 };
    if !closed(args, &["query", "budget"])
        || args
            .get("budget")
            .is_some_and(|value| !integer(value, 1, limit))
    {
        return Err(error(400, "Invalid memory search."));
    }
    let query = field(args, "query", if history { 200 } else { 2000 })?;
    let budget = args["budget"].as_u64().unwrap_or(6) as usize;
    let turn_id = string(turn, "id");
    let bank = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        if state.auth.is_some()
            || super::profile::setup(db, turn)?
            || crate::automation::model(db, turn_id)?.is_some()
            || !exists(
                db,
                "SELECT 1 FROM turns WHERE id=? AND status='running' AND paused=0",
                [turn_id],
            )?
            || exists(
                db,
                "SELECT 1 FROM hub_link_turns WHERE turn_id=?",
                [turn_id],
            )?
        {
            return Err(error(403, "Memory is unavailable in this turn."));
        }
        let bank = bank(db, turn)?;
        if name != "reflect" {
            return Ok(
                json!({"items":evidence(db, &bank, turn_id, &query, budget, if history {Search::History} else {Search::Recall})?}),
            );
        }
        if let Some(answer) = data::memory::knowledge::standing_answer(db, &bank, &query)? {
            return Ok(json!(answer));
        }
        bank
    };
    let answer = Box::pin(reflect(
        state,
        turn_id,
        &bank,
        &query,
        budget,
        "memory_reflect",
    ))
    .await?;
    Ok(json!(answer))
}

pub(crate) fn completion_agent(db: &Connection, turn: &str, phase: &str) -> Result<Value> {
    let sql = match phase {
        "memory_reflect" =>
            "SELECT a.*,coalesce(nullif(a.memory_model,''),a.model) AS learning_model
             FROM turns t JOIN agents a ON a.id=t.agent WHERE t.id=? AND t.status='running' AND t.paused=0",
        "memory_refresh" =>
            "SELECT a.*,r.model AS learning_model FROM agents a JOIN memory_learning_candidates r
             ON r.agent=a.id WHERE r.id=?",
        _ =>
            "SELECT a.*,r.model AS learning_model FROM agents a JOIN memory_learning_ready r
             ON r.agent=a.id WHERE r.id=?",
    };
    one(db, sql, [turn])?.ok_or_else(|| error(409, "Memory completion is unavailable."))
}
