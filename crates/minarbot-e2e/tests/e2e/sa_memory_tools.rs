//! Agent memory tools: scope, user selection, atomic errors, resume receipts
//! and ordering across delegation (port of `app/memory-tools.test.ts`).

use crate::sa_support::{api_agent, channel, count, id, lock, settle, side_thread, tool_loop};
use minarbot_testkit::{
    gate::ModelGate,
    model::{Reply, ScriptedModel},
    prelude::*,
    Server,
};

fn user_edit(entry: &str, revision: u64, text: &str, essential: bool) -> Value {
    json!({"id": new_id(), "entry": entry, "revision": revision, "text": text,
        "essential": essential, "remove": false})
}

async fn run(app: &Server, path: &str, extra: Value) -> Result<String> {
    let turn = new_id();
    let mut body = json!({"id": turn, "text": "Remember this"});
    if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        body.extend(extra.clone());
    }
    app.post(path, body).await?;
    let row = settle(app, path, &turn).await?;
    ensure!(row["status"] == "done", "memory turn in {path}: {row}");
    Ok(turn)
}

#[tokio::test]
async fn agent_memory_writes_preserve_scope_user_selection_atomic_errors_and_resume_receipts(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    let gate = ModelGate::start(&model).await?;
    let state = tool_loop(
        &model,
        "memory-call",
        "remember_memory",
        json!({"text": "First memory"}),
        "Saved.",
    );
    let set = |args: Value| -> Result<()> {
        lock(&state)?.args = args;
        Ok(())
    };
    let result = || -> Result<Value> { Ok(lock(&state)?.result.clone()) };
    let root = TestRoot::new()?;
    let mut app = root
        .start(ServerConfig::new().gateway(gate.gateway()))
        .await?;
    let a = id(&api_agent(&app, "A").await?)?;
    let b = id(&api_agent(&app, "B").await?)?;
    let room = id(&channel(&app, "C", &a, &[&a, &b]).await?)?;
    let side = id(&side_thread(&app, &a, "Side").await?)?;
    let path = format!("/api/agents/{a}/turns");
    let memory = format!("/api/agents/{a}/memory");
    let foreign = format!("/api/agents/{b}/memory");
    let shared = format!("/api/channels/{room}/memory");
    let room_turns = format!("/api/channels/{room}/turns");
    let to_a = json!({"recipient": a});

    lock(&state)?.hold = true;
    let task = new_id();
    app.post(&path, json!({"id": task, "text": "Remember"}))
        .await?;
    until("held memory result", || async {
        Ok(lock(&state)?.held.then_some(()))
    })
    .await?;
    let first = app.get(&memory).await?["items"][0].clone();
    ensure!(first["text"] == "First memory", "first: {first}");
    ensure!(first["essential"] == 0, "agent selected essential: {first}");
    let first_id = id(&first)?;
    ensure!(
        count(&app.get(&foreign).await?, "items") == 0,
        "foreign scope"
    );
    ensure!(
        count(&app.get(&shared).await?, "items") == 0,
        "channel scope"
    );
    app.edit_database(|db| {
        db.execute_batch("ALTER TABLE flow_tool_calls DROP COLUMN after_action")
    })
    .await?;
    lock(&state)?.hold = false;
    app.post(&memory, user_edit(&first_id, 1, "User corrected", true))
        .await?;
    let interrupted = app.row(&path, &task).await?.ok_or("task missing")?;
    ensure!(
        interrupted["status"] == "interrupted",
        "after restart: {interrupted}"
    );
    app.post(
        &format!("{path}/{task}/resume"),
        json!({"id": new_id(), "revision": interrupted["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    app.wait_status(&path, &task, "done").await?;
    ensure!(
        result()?["text"] == "First memory",
        "resume result: {}",
        result()?
    );
    ensure!(
        result()?["revision"] == 1,
        "resume replayed the write: {}",
        result()?
    );
    let current = app.get(&memory).await?["items"][0].clone();
    ensure!(
        current["text"] == "User corrected",
        "user edit lost: {current}"
    );
    ensure!(current["revision"] == 2, "revision: {current}");

    set(json!({"id": first_id, "revision": 2, "text": "Agent updated"}))?;
    run(&app, &path, json!({})).await?;
    // 89b7f4e (#87): a note the user selected is read-only to agents.
    ensure!(
        result()?["status"] == 409,
        "agent edited a user-selected note: {}",
        result()?
    );

    // The rejected agent edit left the note at revision 2.
    set(json!({"id": first_id, "revision": 2, "text": "Stale after user edit"}))?;
    gate.arm();
    let stale = new_id();
    app.post(&path, json!({"id": stale, "text": "Remember this"}))
        .await?;
    gate.wait_held().await?;
    app.post(&memory, user_edit(&first_id, 2, "New user choice", false))
        .await?;
    gate.release();
    let row = settle(&app, &path, &stale).await?;
    ensure!(row["status"] == "done", "stale turn: {row}");
    ensure!(result()?["status"] == 409, "stale write: {}", result()?);
    ensure!(
        app.get(&memory).await?["items"][0]["text"] == "New user choice",
        "stale write overwrote the user"
    );

    set(json!({"id": first_id, "revision": 3, "text": "Side update"}))?;
    run(&app, &format!("/api/threads/{side}/turns"), json!({})).await?;
    ensure!(
        result()?["status"] == 409,
        "side update of a user-selected note: {}",
        result()?
    );

    for invalid in [
        json!({"id": first_id, "revision": 5, "text": "Wrong scope"}),
        json!({"text": "Forbidden selection", "essential": true}),
        json!({"text": "Forbidden owner", "owner": a}),
        json!({"id": first_id, "text": "Missing revision"}),
        json!({"text": "x".repeat(2001)}),
    ] {
        set(invalid.clone())?;
        run(&app, &room_turns, to_a.clone()).await?;
        let status = result()?["status"].clone();
        ensure!(status == 400 || status == 404, "{invalid}: {}", result()?);
        ensure!(
            count(&app.get(&shared).await?, "items") == 0,
            "invalid write stored"
        );
    }
    set(json!({"text": "Channel note"}))?;
    run(&app, &room_turns, to_a.clone()).await?;
    ensure!(result()?["essential"] == 0, "channel note: {}", result()?);
    ensure!(
        app.get(&shared).await?["items"][0]["text"] == "Channel note",
        "channel note missing"
    );
    run(&app, &room_turns, to_a.clone()).await?;
    ensure!(result()?["status"] == 409, "duplicate note: {}", result()?);
    ensure!(
        count(&app.get(&shared).await?, "items") == 1,
        "duplicate stored"
    );

    app.post(&memory, user_edit(&first_id, 3, &"界".repeat(2000), true))
        .await?;
    let second = new_id();
    // `automatic: true` keeps the essential note unlocked, so the agent edit
    // reaches the context budget check instead of the user-selection lock.
    let mut short = user_edit(&second, 0, "short", true);
    short["automatic"] = json!(true);
    app.post(&memory, short).await?;
    set(json!({"id": second, "revision": 1, "text": "文".repeat(2000)}))?;
    run(&app, &path, json!({})).await?;
    ensure!(result()?["status"] == 413, "budget: {}", result()?);
    let unchanged = app.get(&memory).await?["items"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|row| row["id"] == second.as_str())
                .cloned()
        })
        .ok_or("second entry missing")?;
    ensure!(unchanged["text"] == "short", "budget wrote: {unchanged}");
    ensure!(unchanged["revision"] == 1, "budget revision: {unchanged}");

    set(json!({"text": "Must not exist"}))?;
    let cancelled = new_id();
    gate.arm();
    app.post(
        &path,
        json!({"id": cancelled, "text": "Cancel before write"}),
    )
    .await?;
    gate.wait_held().await?;
    app.post(
        &format!("{path}/{cancelled}/cancel"),
        json!({"id": new_id()}),
    )
    .await?;
    gate.release();
    app.wait_status(&path, &cancelled, "cancelled").await?;
    ensure!(
        count(&app.get(&format!("{memory}?query=Must")).await?, "items") == 0,
        "cancelled turn wrote memory"
    );

    let mut removal = user_edit(&second, 1, "", false);
    removal["remove"] = json!(true);
    app.post(&memory, removal).await?;
    set(json!({"id": second, "revision": 2, "text": "Deleted note must stay deleted"}))?;
    run(&app, &path, json!({})).await?;
    ensure!(result()?["status"] == 404, "deleted entry: {}", result()?);
    ensure!(
        count(&app.get(&format!("{memory}?query=Deleted")).await?, "items") == 0,
        "deleted entry came back"
    );

    app.edit_database(|db| {
        db.execute_batch(
            "CREATE TRIGGER reject_tool BEFORE INSERT ON flow_tool_calls \
             BEGIN SELECT RAISE(ABORT, 'synthetic journal fault'); END",
        )
    })
    .await?;
    set(json!({"text": "Atomic failure must roll back"}))?;
    let failed = new_id();
    app.post(
        &path,
        json!({"id": failed, "text": "Inject journal failure"}),
    )
    .await?;
    // #87: the storage failure reaches the harness as a tool error (the API
    // runtime failed the turn); the write must still roll back.
    let row = settle(&app, &path, &failed).await?;
    ensure!(row["status"] == "done", "journal failure turn: {row}");
    ensure!(
        result()? == json!({"error": "Save failed. Repeat the same request.", "status": 500}),
        "journal failure result: {}",
        result()?
    );
    ensure!(
        count(&app.get(&format!("{memory}?query=Atomic")).await?, "items") == 0,
        "journal failure kept the write"
    );
    ensure!(
        lock(&state)?.missing == 0,
        "remember_memory was not offered"
    );
    Ok(())
}

#[tokio::test]
async fn memory_and_artifact_order_survives_repeated_delegation_when_parent_continues() -> Result<()>
{
    let model = ScriptedModel::start().await?;
    let peer = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let (peer_id, seen) = (peer.clone(), observed.clone());
    model.respond(move |request| {
        if request.last_user_text() != "root" {
            return Reply::text("Done");
        }
        let names: Vec<String> = request
            .tool_calls()
            .iter()
            .filter_map(|call| call["function"]["name"].as_str().map(str::to_owned))
            .collect();
        let peer = peer_id.lock().map(|peer| peer.clone()).unwrap_or_default();
        let steps = [
            ("remember_memory", json!({"text": "Before delegation"})),
            (
                "create_text_file",
                json!({"name": "first.txt", "text": "first"}),
            ),
            ("delegate_task", json!({"recipient": peer, "task": "child"})),
            ("remember_memory", json!({"text": "Between delegations"})),
            (
                "create_text_file",
                json!({"name": "second.txt", "text": "second"}),
            ),
            (
                "delegate_task",
                json!({"recipient": peer, "task": "child again"}),
            ),
            ("remember_memory", json!({"text": "After delegations"})),
        ];
        match steps.get(names.len()) {
            Some((name, args)) => {
                Reply::tool_call(&format!("step-{}", names.len()), name, args.clone())
            }
            None => {
                if let Ok(mut seen) = seen.lock() {
                    *seen = names;
                }
                Reply::text("Done")
            }
        }
    });
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new().model(&model)).await?;
    let a = id(&api_agent(&app, "A").await?)?;
    let b = id(&api_agent(&app, "B").await?)?;
    *lock(&peer)? = b.clone();
    let room = id(&channel(&app, "Room", &a, &[&a, &b]).await?)?;
    let path = format!("/api/channels/{room}/turns");
    let turn = new_id();
    app.post(&path, json!({"id": turn, "text": "root", "recipient": a}))
        .await?;
    app.wait_status(&path, &turn, "done").await?;
    let names = lock(&observed)?.clone();
    ensure!(
        names
            == [
                "remember_memory",
                "create_text_file",
                "delegate_task",
                "remember_memory",
                "create_text_file",
                "delegate_task",
                "remember_memory"
            ],
        "tool order: {names:?}"
    );
    ensure!(
        count(
            &app.get(&format!("/api/channels/{room}/memory")).await?,
            "items"
        ) == 3,
        "channel memory"
    );
    Ok(())
}
