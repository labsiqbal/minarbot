use super::*;
use minarbot_memory::Store;
use tokio::time::{interval, Duration, MissedTickBehavior};

pub fn initialize(db: &Connection) -> crate::db::Result<()> {
    db.execute_batch(include_str!("../data/memory-worker.sql"))?;
    Ok(())
}

pub fn start(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut timer = interval(Duration::from_secs(2));
        timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            timer.tick().await;
            if pending(&state) {
                continue;
            }
            let result = match step(&state).await {
                Ok(false) => refresh_question(&state).await,
                Ok(true) => Ok(()),
                Err(error) => Err(error),
            };
            if result.is_err() {
                eprintln!("Memory worker storage error");
            }
        }
    })
}

async fn step(state: &AppState) -> Result<bool> {
    let (work, secrets) = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        let store = Store::new(db);
        let disabled = all(
            db,
            "SELECT id FROM agents WHERE learn_from_conversations=0",
            [],
        )?;
        let disabled = disabled.iter().map(|a| string(a, "id")).collect::<Vec<_>>();
        let Some(work) = store
            .pending_work_excluding(crate::config::now(), &disabled)
            .map_err(|_| Failure::storage())?
        else {
            return Ok(false);
        };
        let turn = string(work.source(), "id");
        if !exists(
            db,
            "SELECT 1 FROM memory_learning_ready WHERE id=? AND agent=?",
            params![turn, string(work.source(), "agent")],
        )? {
            return store
                .retry_work(&work, crate::config::now())
                .map(|()| true)
                .map_err(|_| Failure::storage());
        };
        let secrets = crate::mcp::secrets::project(state, db, None)?;
        (work, secrets)
    };
    let (system, input) = work.prompt();
    let messages = [
        json!({"role":"system","content":system}),
        json!({"role":"user","content":crate::mcp::secrets::redact(&input.to_string(),&secrets,false)}),
    ];
    let phase = match (work.extracting(), string(work.source(), "role")) {
        (true, "prompt") => "memory_extract_prompt",
        (true, _) => "memory_extract_reply",
        (false, "prompt") => "memory_consolidate_prompt",
        (false, _) => "memory_consolidate_reply",
    };
    let result = provider::call(
        state,
        string(work.source(), "id"),
        "",
        &messages,
        &[],
        phase,
    )
    .await;
    if pending(state) {
        return Ok(true);
    }
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let store = Store::new(&guard.connection);
    if let Ok(provider::Answer::Text(answer)) = result {
        let answer = crate::mcp::secrets::redact(&answer, &secrets, false);
        if store.apply_work(&work, &answer).is_ok() {
            return Ok(true);
        }
    }
    if exists(
        &guard.connection,
        "SELECT 1 FROM agents WHERE id=? AND learn_from_conversations=0",
        [string(work.source(), "agent")],
    )? {
        return Ok(true);
    }
    store
        .retry_work(&work, crate::config::now())
        .map(|()| true)
        .map_err(|_| Failure::storage())
}

async fn refresh_question(state: &AppState) -> Result<()> {
    let now = state.config.now();
    let row = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        one(
            &guard.connection,
            "SELECT m.bank,m.id,m.revision,m.question,coalesce(g.generation,0) AS generation,
                    (SELECT t.id FROM memory_learning_candidates r JOIN turns t ON t.id=r.id
                     JOIN memory_history_scope scope ON scope.id=t.id
                     WHERE scope.bank=m.bank ORDER BY t.seq DESC LIMIT 1) AS turn
             FROM memory_mental_models m LEFT JOIN memory_bank_generations g ON g.bank=m.bank
             WHERE m.removed=0 AND m.generation<>coalesce(g.generation,0) AND m.retry_at<=?
               AND turn IS NOT NULL ORDER BY m.retry_at,m.id LIMIT 1",
            [now],
        )?
    };
    let Some(row) = row else { return Ok(()) };
    let bank = string(&row, "bank");
    let turn = string(&row, "turn");
    let id = string(&row, "id");
    let revision = row["revision"].as_i64().ok_or_else(Failure::storage)?;
    let generation = row["generation"].as_i64().ok_or_else(Failure::storage)?;
    let answer = super::memory::reflect(
        state,
        turn,
        bank,
        string(&row, "question"),
        6,
        "memory_refresh",
    )
    .await;
    if pending(state) {
        return Ok(());
    }
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    let retry_at = now.saturating_add(60_000);
    if let Ok(answer) = answer {
        db.execute(
            "UPDATE memory_mental_models SET answer=?,evidence=?,generation=?,retry_at=?
             WHERE bank=? AND id=? AND revision=? AND removed=0 AND ?=coalesce(
                 (SELECT generation FROM memory_bank_generations WHERE bank=?),0)
             AND EXISTS(SELECT 1 FROM memory_learning_candidates WHERE id=?)",
            params![
                answer.answer,
                json!(answer.evidence).to_string(),
                generation,
                retry_at,
                bank,
                id,
                revision,
                generation,
                bank,
                turn
            ],
        )?;
    } else {
        db.execute(
            "UPDATE memory_mental_models SET retry_at=? WHERE bank=? AND id=? AND revision=?",
            params![retry_at, bank, id, revision],
        )?;
    }
    Ok(())
}
