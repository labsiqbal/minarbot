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
            if step(&state).await.is_err() {
                eprintln!("Memory worker storage error");
            }
        }
    })
}

async fn step(state: &AppState) -> Result<()> {
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
            return Ok(());
        };
        let turn = string(work.source(), "id");
        if !exists(
            db,
            "SELECT 1 FROM memory_learning_ready WHERE id=? AND agent=?",
            params![turn, string(work.source(), "agent")],
        )? {
            return store
                .retry_work(&work, crate::config::now())
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
        return Ok(());
    }
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let store = Store::new(&guard.connection);
    if let Ok(provider::Answer::Text(answer)) = result {
        let answer = crate::mcp::secrets::redact(&answer, &secrets, false);
        if store.apply_work(&work, &answer).is_ok() {
            return Ok(());
        }
    }
    if exists(
        &guard.connection,
        "SELECT 1 FROM agents WHERE id=? AND learn_from_conversations=0",
        [string(work.source(), "agent")],
    )? {
        return Ok(());
    }
    store
        .retry_work(&work, crate::config::now())
        .map_err(|_| Failure::storage())
}
