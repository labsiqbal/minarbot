//! Automatic memory selection through `select_memory` (port of
//! `app/memory-selection.test.ts`).

use crate::sa_support::{api_agent, channel, id, lock, settle, tool_loop};
use minarbot_testkit::{model::ScriptedModel, prelude::*, Server};

fn note(text: &str, automatic: bool) -> Value {
    json!({"id": new_id(), "entry": new_id(), "revision": 0, "text": text,
        "essential": false, "remove": false, "automatic": automatic})
}

fn choose(entry: &Value, revision: u64, essential: bool) -> Value {
    json!({"id": entry["entry"], "revision": revision, "essential": essential})
}

fn with(base: &Value, fields: Value) -> Value {
    let mut value = base.clone();
    if let (Some(value), Some(fields)) = (value.as_object_mut(), fields.as_object()) {
        value.extend(fields.clone());
    }
    value
}

fn item(page: &Value, entry: &Value) -> Result<Value> {
    page["items"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["id"] == entry["entry"]))
        .cloned()
        .ok_or_else(|| format!("entry {} missing from {page}", entry["entry"]).into())
}

async fn run(app: &Server, path: &str) -> Result<()> {
    let turn = new_id();
    app.post(path, json!({"id": turn, "text": "Select saved context"}))
        .await?;
    let row = settle(app, path, &turn).await?;
    ensure!(row["status"] == "done", "selection turn: {row}");
    Ok(())
}

#[tokio::test]
async fn automatic_memory_selection_is_atomic_user_protected_scoped_and_durable_across_resume(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    let state = tool_loop(&model, "selection", "select_memory", json!({}), "Done");
    let set = |args: Value| -> Result<()> {
        lock(&state)?.args = args;
        Ok(())
    };
    let result = || -> Result<Value> { Ok(lock(&state)?.result.clone()) };
    let initial = || -> Result<String> { Ok(lock(&state)?.initial.clone()) };
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new().model(&model)).await?;
    let a = id(&api_agent(&app, "A").await?)?;
    let room = id(&channel(&app, "C", &a, &[&a]).await?)?;
    let path = format!("/api/agents/{a}/turns");
    let memory = format!("/api/agents/{a}/memory");
    let first = note("AUTOMATIC_CONTEXT", true);
    let second = note("MANUAL_CONTEXT", false);
    let third = note("OTHER_AUTOMATIC", true);
    for entry in [&first, &second, &third] {
        app.post(&memory, entry.clone()).await?;
    }

    set(json!({"items": [choose(&first, 1, true), choose(&second, 1, true)]}))?;
    run(&app, &path).await?;
    ensure!(
        result()?["status"] == 409,
        "manual entry selected: {}",
        result()?
    );
    let page = app.get(&memory).await?;
    ensure!(
        page["items"].as_array().is_some_and(|items| items
            .iter()
            .all(|row| row["essential"] == 0 && row["revision"] == 1)),
        "selection was not atomic: {page}"
    );
    for (items, status) in [
        (
            json!([choose(&first, 1, true), choose(&first, 1, true)]),
            400,
        ),
        (json!([choose(&first, 2, true)]), 409),
        (
            json!([choose(&first, 1, true), {"id": new_id(), "revision": 1, "essential": true}]),
            404,
        ),
        (json!([]), 400),
    ] {
        set(json!({"items": items}))?;
        run(&app, &path).await?;
        ensure!(result()?["status"] == status, "{items}: {}", result()?);
    }
    set(json!({"items": [choose(&first, 1, true)]}))?;
    run(&app, &format!("/api/channels/{room}/turns")).await?;
    ensure!(result()?["status"] == 404, "channel scope: {}", result()?);

    set(json!({"items": [choose(&first, 1, true), choose(&third, 1, true)]}))?;
    lock(&state)?.hold = true;
    let task = new_id();
    app.post(&path, json!({"id": task, "text": "Select and interrupt"}))
        .await?;
    until("held selection result", || async {
        Ok(lock(&state)?.held.then_some(()))
    })
    .await?;
    ensure!(
        !initial()?.contains("AUTOMATIC_CONTEXT"),
        "context before selection"
    );
    ensure!(
        result()?["items"][0]["revision"] == 2,
        "selected: {}",
        result()?
    );
    app.restart().await?;
    lock(&state)?.hold = false;
    app.post(
        &memory,
        with(
            &first,
            json!({"id": new_id(), "revision": 2, "automatic": false, "essential": false}),
        ),
    )
    .await?;
    let stopped = app.row(&path, &task).await?.ok_or("task missing")?;
    app.post(
        &format!("{path}/{task}/resume"),
        json!({"id": new_id(), "revision": stopped["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    app.wait_status(&path, &task, "done").await?;
    ensure!(
        result()?["items"][0]["revision"] == 2,
        "resume replayed: {}",
        result()?
    );
    let current = item(&app.get(&memory).await?, &first)?;
    ensure!(current["essential"] == 0, "user choice lost: {current}");
    ensure!(current["selection_locked"] == 1, "lock: {current}");
    ensure!(current["revision"] == 3, "revision: {current}");

    set(json!({"items": [choose(&first, 3, true)]}))?;
    run(&app, &path).await?;
    ensure!(
        result()?["status"] == 409,
        "locked entry selected: {}",
        result()?
    );
    ensure!(
        initial()?.contains("OTHER_AUTOMATIC"),
        "selected context missing"
    );
    ensure!(
        !initial()?.contains("AUTOMATIC_CONTEXT"),
        "unselected context present"
    );
    app.post(
        &memory,
        with(
            &first,
            json!({"id": new_id(), "revision": 3, "automatic": true}),
        ),
    )
    .await?;
    set(json!({"items": [choose(&first, 4, true)]}))?;
    run(&app, &path).await?;
    ensure!(
        result()?["items"][0]["essential"] == 1,
        "unlock: {}",
        result()?
    );

    let large_a = note(&"界".repeat(2000), true);
    let large_b = note(&"文".repeat(2000), true);
    app.post(&memory, large_a.clone()).await?;
    app.post(&memory, large_b.clone()).await?;
    set(
        json!({"items": [choose(&first, 5, false), choose(&large_a, 1, true), choose(&large_b, 1, true)]}),
    )?;
    run(&app, &path).await?;
    ensure!(result()?["status"] == 413, "budget: {}", result()?);
    let page = app.get(&memory).await?;
    ensure!(
        item(&page, &first)?["essential"] == 1,
        "budget rolled forward"
    );
    ensure!(
        item(&page, &large_a)?["revision"] == 1,
        "budget changed large entry"
    );
    app.post(
        &memory,
        with(
            &third,
            json!({"id": new_id(), "revision": 2, "text": "", "essential": false, "remove": true}),
        ),
    )
    .await?;
    set(json!({"items": [choose(&third, 3, true)]}))?;
    run(&app, &path).await?;
    ensure!(result()?["status"] == 404, "deleted entry: {}", result()?);

    let mut old = note("LEGACY", true);
    if let Some(fields) = old.as_object_mut() {
        fields.remove("automatic");
    }
    let receipt = app.post(&memory, old.clone()).await?;
    app.edit_database(|db| {
        db.execute_batch(
            "DROP TRIGGER memory_note_insert; DROP TRIGGER memory_note_update; \
             DROP TRIGGER memory_note_delete; DROP VIEW memory_legacy_rows; \
             DELETE FROM memory_units WHERE origin='legacy'; \
             DELETE FROM memory_imports WHERE version=1; \
             ALTER TABLE memories DROP COLUMN selection_locked;",
        )
    })
    .await?;
    ensure!(
        app.post(&memory, old.clone()).await? == receipt,
        "legacy receipt"
    );
    let page = app.get(&memory).await?;
    ensure!(
        page["items"]
            .as_array()
            .is_some_and(|items| items.iter().all(|row| row["selection_locked"] == 1)),
        "legacy rows unlocked: {page}"
    );
    // Server-side form of `memoryCommand({...old, automatic: 'yes'})` throwing.
    app.post_expect(
        &memory,
        with(&old, json!({"id": new_id(), "automatic": "yes"})),
        400,
    )
    .await?;
    ensure!(lock(&state)?.missing == 0, "select_memory was not offered");
    Ok(())
}
