//! Side threads, concurrency limits and conversation commands
//! (port of `app/threads.test.ts`).

use minarbot_testkit::{
    model::{Reply, ScriptedModel},
    prelude::*,
};

/// Model that answers `reply:<text>` and holds requests whose text starts with `hold`.
async fn echo_model() -> Result<ScriptedModel> {
    let model = ScriptedModel::start().await?;
    model.respond(|request| {
        let text = request.last_text();
        if text.starts_with("hold") {
            Reply::hold()
        } else {
            Reply::text(format!("reply:{text}"))
        }
    });
    Ok(model)
}

fn texts_of(model: &ScriptedModel, text: &str) -> Vec<Vec<String>> {
    model
        .requests()
        .iter()
        .filter(|request| request.last_text() == text)
        .map(|request| request.texts())
        .collect()
}

/// Wait until the model has received a step whose last text is `text`
/// (the turn is in flight at the provider, not only marked running).
async fn reached(model: &ScriptedModel, text: &str) -> Result<()> {
    until(format!("model receives {text}"), || async {
        Ok(called(model, |seen| seen == text).then_some(()))
    })
    .await
}

fn called(model: &ScriptedModel, predicate: impl Fn(&str) -> bool) -> bool {
    model
        .requests()
        .iter()
        .any(|request| predicate(&request.last_text()))
}

#[tokio::test]
async fn side_threads_isolate_queues_history_controls_and_archive_recovery() -> Result<()> {
    let model = echo_model().await?;
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new().model(&model)).await?;
    let agent = app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Same agent", "threadLimit": 4,
                "model": "fixture", "instructions": "Help."}),
            201,
        )
        .await?;
    let agent_id = agent["id"].as_str().ok_or("agent id")?;
    let list = format!("/api/agents/{agent_id}/threads");
    let main = format!("/api/agents/{agent_id}/turns");
    let create = json!({"id": new_id(), "name": "Side work"});
    let side = app.post_expect(&list, create.clone(), 200).await?;
    let side_id = side["id"].as_str().ok_or("side id")?.to_owned();
    let path = format!("/api/threads/{side_id}/turns");
    let edit = format!("/api/threads/{side_id}/edit");
    let remove = format!("/api/threads/{side_id}/delete");
    ensure!(
        app.post_expect(&list, create.clone(), 200).await? == side,
        "create replay differs"
    );
    app.post_expect(
        &list,
        json!({"id": create["id"], "name": "Other name"}),
        409,
    )
    .await?;

    let main_hold = new_id();
    app.post_expect(&main, json!({"id": main_hold, "text": "hold-main"}), 202)
        .await?;
    app.wait_status(&main, &main_hold, "running").await?;
    let first = new_id();
    app.post_expect(&path, json!({"id": first, "text": "side-one"}), 202)
        .await?;
    app.wait_status(&path, &first, "done").await?;
    let main_rows = app.get(&main).await?;
    ensure!(
        main_rows.as_array().map(Vec::len) == Some(1),
        "main turns: {main_rows}"
    );
    ensure!(
        main_rows[0]["status"] == "running",
        "main turn: {}",
        main_rows[0]
    );
    let side_rows = app.get(&path).await?;
    ensure!(
        side_rows.as_array().map(Vec::len) == Some(1),
        "side turns: {side_rows}"
    );
    app.post_expect(
        &format!("{main}/{first}/cancel"),
        json!({"id": new_id()}),
        404,
    )
    .await?;
    app.post_expect(&main, json!({"id": first, "text": "side-one"}), 409)
        .await?;
    let second = new_id();
    app.post_expect(&path, json!({"id": second, "text": "side-two"}), 202)
        .await?;
    app.wait_status(&path, &second, "done").await?;
    let history = texts_of(&model, "side-two");
    ensure!(history.len() == 1, "side-two calls: {history:?}");
    ensure!(
        history[0].iter().any(|text| text == "reply:side-one"),
        "side history: {history:?}"
    );
    ensure!(
        !history[0].iter().any(|text| text == "hold-main"),
        "main leaked into side: {history:?}"
    );

    app.post_expect(&remove, json!({"id": new_id(), "revision": 0}), 400)
        .await?;
    let held = new_id();
    app.post_expect(&path, json!({"id": held, "text": "hold-side"}), 202)
        .await?;
    app.wait_status(&path, &held, "running").await?;
    app.post_expect(&remove, json!({"id": new_id(), "revision": 1}), 409)
        .await?;
    let queued = new_id();
    app.post_expect(&path, json!({"id": queued, "text": "queued-side"}), 202)
        .await?;
    app.post_expect(
        &format!("{path}/{queued}/pause"),
        json!({"id": new_id(), "revision": 1}),
        200,
    )
    .await?;
    app.post_expect(
        &format!("{path}/{held}/cancel"),
        json!({"id": new_id()}),
        200,
    )
    .await?;
    app.wait_status(&path, &held, "cancelled").await?;
    ensure!(
        app.get(&main).await?[0]["status"] == "running",
        "main turn stopped with side cancel"
    );

    app.restart().await?;
    let paused = app.wait_status(&path, &queued, "queued").await?;
    ensure!(paused["paused"] == 1, "paused turn after restart: {paused}");
    ensure!(
        !called(&model, |text| text == "queued-side"),
        "paused turn ran"
    );
    app.post_expect(
        &format!("{path}/{queued}/resume"),
        json!({"id": new_id(), "revision": 2}),
        200,
    )
    .await?;
    app.wait_status(&path, &queued, "done").await?;
    ensure!(
        texts_of(&model, "queued-side").len() == 1,
        "resumed turn ran more than once"
    );

    let archived_work = new_id();
    app.post_expect(
        &path,
        json!({"id": archived_work, "text": "hold-archive"}),
        202,
    )
    .await?;
    app.wait_status(&path, &archived_work, "running").await?;
    reached(&model, "hold-archive").await?;
    let archive = json!({"id": new_id(), "revision": 1, "name": "Archived side", "archived": true});
    let archived = app.post_expect(&edit, archive.clone(), 200).await?;
    ensure!(archived["archived"] == 1, "archive: {archived}");
    app.wait_status(&path, &archived_work, "cancelled").await?;
    app.post_expect(&path, json!({"id": new_id(), "text": "blocked"}), 409)
        .await?;
    app.post_expect(
        &edit,
        json!({"id": new_id(), "revision": 2, "name": "Archived side", "archived": false}),
        200,
    )
    .await?;
    ensure!(
        app.post_expect(&edit, archive.clone(), 200).await? == archived,
        "archive replay differs"
    );
    ensure!(
        app.get(&list).await?[0]["archived"] == 0,
        "replayed archive changed state"
    );
    ensure!(
        texts_of(&model, "hold-archive").len() == 1,
        "archived work ran twice"
    );
    app.post_expect(
        &edit,
        json!({"id": new_id(), "revision": 3, "name": "Archived side", "archived": true}),
        200,
    )
    .await?;
    let deletion = json!({"id": new_id(), "revision": 4});
    let deleted = app.post_expect(&remove, deletion.clone(), 200).await?;
    ensure!(deleted["deleted"] == 1, "delete: {deleted}");
    ensure!(
        app.post_expect(&remove, deletion, 200).await? == deleted,
        "delete replay differs"
    );
    ensure!(
        app.get(&list).await? == json!([]),
        "deleted thread still listed"
    );
    app.get_expect(&path, 404).await?;
    app.post_expect(&list, create, 200).await?;
    ensure!(
        app.get(&list).await? == json!([]),
        "create replay resurrected a deleted thread"
    );

    let main_reply = new_id();
    app.post_expect(
        &main,
        json!({"id": main_reply, "text": "main-after-side"}),
        202,
    )
    .await?;
    app.wait_status(&main, &main_reply, "done").await?;
    let history = texts_of(&model, "main-after-side");
    ensure!(
        history.len() == 1 && !history[0].iter().any(|text| text.contains("side-one")),
        "side history leaked into main: {history:?}"
    );

    let mut workers = Vec::new();
    for index in 0..5 {
        let thread = app
            .post_expect(
                &list,
                json!({"id": new_id(), "name": format!("Worker {index}")}),
                200,
            )
            .await?;
        let worker_path = format!(
            "/api/threads/{}/turns",
            thread["id"].as_str().ok_or("thread id")?
        );
        let id = new_id();
        app.post_expect(
            &worker_path,
            json!({"id": id, "text": format!("hold-worker-{index}")}),
            202,
        )
        .await?;
        app.wait_status(
            &worker_path,
            &id,
            if index < 4 { "running" } else { "queued" },
        )
        .await?;
        if index < 4 {
            reached(&model, &format!("hold-worker-{index}")).await?;
        }
        workers.push((worker_path, id));
    }
    ensure!(
        !called(&model, |text| text == "hold-worker-4"),
        "fifth worker exceeded threadLimit 4"
    );
    let fifo = [new_id(), new_id()];
    for (index, id) in fifo.iter().enumerate() {
        app.post_expect(
            &workers[0].0,
            json!({"id": id, "text": format!("fifo-{index}")}),
            202,
        )
        .await?;
    }
    ensure!(
        !called(&model, |text| text.starts_with("fifo-")),
        "queued FIFO work ran early"
    );

    app.restart().await?;
    for (index, (worker_path, id)) in workers.iter().take(4).enumerate() {
        app.wait_status(worker_path, id, "interrupted").await?;
        let runs = texts_of(&model, &format!("hold-worker-{index}")).len();
        ensure!(
            runs == 1,
            "worker {index} replayed after restart ({runs} calls)"
        );
    }
    app.wait_status(&workers[4].0, &workers[4].1, "running")
        .await?;
    app.wait_status(&workers[0].0, &fifo[1], "done").await?;
    let order: Vec<String> = model
        .requests()
        .iter()
        .map(|request| request.last_text())
        .filter(|text| text.starts_with("fifo-"))
        .collect();
    ensure!(order == ["fifo-0", "fifo-1"], "FIFO order: {order:?}");
    let history = texts_of(&model, "fifo-1");
    ensure!(
        history[0].iter().any(|text| text == "reply:fifo-0"),
        "fifo history: {history:?}"
    );
    app.post_expect(
        &format!("{}/{}/cancel", workers[4].0, workers[4].1),
        json!({"id": new_id()}),
        200,
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn configured_concurrency_limits_side_threads_and_validates_before_opening_storage(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|request| {
        if request.last_text() == "hold" {
            Reply::hold()
        } else {
            Reply::text("done")
        }
    });
    let root = TestRoot::new()?;
    // NaN and Infinity have no JSON form; a string and an out-of-range
    // float are their closest serializable stand-ins.
    for concurrency in [
        json!(0),
        json!(-1),
        json!(1.5),
        json!(33),
        json!("NaN"),
        json!(1e300),
    ] {
        let error = root
            .start_error(ServerConfig::new().concurrency(concurrency.clone()))
            .await?;
        ensure!(
            error.contains("CONCURRENCY"),
            "concurrency {concurrency}: {error}"
        );
    }
    ensure!(
        !root.database().exists(),
        "invalid configuration opened storage"
    );
    let mut app = root
        .start(ServerConfig::new().concurrency(1).model(&model))
        .await?;
    let agent = app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "One worker", "model": "fixture",
                "instructions": "Help."}),
        )
        .await?;
    let agent_id = agent["id"].as_str().ok_or("agent id")?;
    let main = format!("/api/agents/{agent_id}/turns");
    let hold = new_id();
    app.post(&main, json!({"id": hold, "text": "hold"})).await?;
    model.wait_calls(1).await?;
    let texts = |model: &ScriptedModel| -> Vec<String> {
        model
            .requests()
            .iter()
            .map(|request| request.last_text())
            .collect()
    };
    ensure!(texts(&model) == ["hold"], "calls: {:?}", texts(&model));
    let thread = app
        .post(
            &format!("/api/agents/{agent_id}/threads"),
            json!({"id": new_id(), "name": "Waiting side"}),
        )
        .await?;
    let path = format!(
        "/api/threads/{}/turns",
        thread["id"].as_str().ok_or("thread id")?
    );
    app.post(&path, json!({"id": new_id(), "text": "side"}))
        .await?;
    ensure!(
        app.get(&path).await?[0]["status"] == "queued",
        "side turn did not queue"
    );
    ensure!(texts(&model) == ["hold"], "calls: {:?}", texts(&model));
    app.post(&format!("{main}/{hold}/cancel"), json!({"id": new_id()}))
        .await?;
    until_value(
        "side turn done",
        || async { Ok(app.get(&path).await?[0].clone()) },
        |row| row["status"] == "done",
    )
    .await?;
    ensure!(
        texts(&model) == ["hold", "side"],
        "calls: {:?}",
        texts(&model)
    );
    ensure!(agent["thread_limit"] == 2, "default thread limit: {agent}");
    let mut edit = agent.clone();
    edit["id"] = json!(new_id());
    edit["threadLimit"] = json!(1);
    let edit_path = format!("/api/agents/{agent_id}/edit");
    ensure!(
        app.post(&edit_path, edit.clone()).await?["thread_limit"] == 1,
        "thread limit edit"
    );
    app.restart_with(ServerConfig::new().model(&model)).await?;
    ensure!(
        app.post(&edit_path, edit).await?["thread_limit"] == 1,
        "edit replay after restart"
    );
    let second = app
        .post(
            &format!("/api/agents/{agent_id}/threads"),
            json!({"id": new_id(), "name": "Second side"}),
        )
        .await?;
    let other_path = format!(
        "/api/threads/{}/turns",
        second["id"].as_str().ok_or("thread id")?
    );
    let side_hold = new_id();
    app.post(&path, json!({"id": side_hold, "text": "hold"}))
        .await?;
    let queued = new_id();
    app.post(&other_path, json!({"id": queued, "text": "after-limit"}))
        .await?;
    ensure!(
        app.get(&other_path).await?[0]["status"] == "queued",
        "thread limit 1 not applied"
    );
    let separate = new_id();
    app.post(&main, json!({"id": separate, "text": "main-independent"}))
        .await?;
    app.wait_status(&main, &separate, "done").await?;
    ensure!(
        !texts(&model).iter().any(|text| text == "after-limit"),
        "queued side turn ran"
    );
    app.post(
        &format!("{path}/{side_hold}/cancel"),
        json!({"id": new_id()}),
    )
    .await?;
    app.wait_status(&other_path, &queued, "done").await?;
    for limit in [json!(0), json!(33), json!(1.5), json!(null), json!("2")] {
        app.post_expect(
            "/api/agents",
            json!({"id": new_id(), "name": "Invalid", "model": "fixture", "instructions": "Help.",
                "threadLimit": limit}),
            400,
        )
        .await?;
    }
    Ok(())
}

#[tokio::test]
async fn conversation_commands_protect_main_chat_revisions_queued_work_and_deleted_history(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|_| Reply::hold());
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new().model(&model)).await?;
    let agent = app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Owner agent", "model": "fixture",
                "instructions": "Help."}),
            201,
        )
        .await?;
    let agent_id = agent["id"].as_str().ok_or("agent id")?;
    let list = format!("/api/agents/{agent_id}/threads");
    let side = app
        .post_expect(&list, json!({"id": new_id(), "name": "Side task"}), 200)
        .await?;
    let side_id = side["id"].as_str().ok_or("side id")?;
    let edit = format!("/api/threads/{side_id}/edit");
    let remove = format!("/api/threads/{side_id}/delete");
    let turns = format!("/api/threads/{side_id}/turns");
    let rename = json!({"id": new_id(), "revision": 1, "name": "Renamed task", "archived": false});
    let renamed = app.post_expect(&edit, rename.clone(), 200).await?;
    ensure!(
        renamed["revision"] == 2 && renamed["name"] == "Renamed task",
        "rename: {renamed}"
    );
    ensure!(
        app.post_expect(&edit, rename.clone(), 200).await? == renamed,
        "rename replay differs"
    );
    let mut changed_id = rename.clone();
    changed_id["id"] = json!(new_id());
    app.post_expect(&edit, changed_id, 409).await?;
    let mut changed_name = rename.clone();
    changed_name["name"] = json!("Conflict");
    app.post_expect(&edit, changed_name, 409).await?;
    app.post_expect(
        &format!("/api/threads/{agent_id}/delete"),
        json!({"id": new_id(), "revision": 1}),
        404,
    )
    .await?;
    app.post_expect(
        &format!("/api/agents/{agent_id}/delete"),
        json!({"id": new_id(), "revision": 1}),
        400,
    )
    .await?;
    let queued = new_id();
    app.post_expect(&turns, json!({"id": queued, "text": "Queued task"}), 202)
        .await?;
    app.post_expect(&remove, json!({"id": new_id(), "revision": 2}), 409)
        .await?;
    let second = new_id();
    app.post_expect(
        &turns,
        json!({"id": second, "text": "Second queued task"}),
        202,
    )
    .await?;
    let row = app
        .row(&turns, &second)
        .await?
        .ok_or("second turn missing")?;
    ensure!(row["status"] == "queued", "second turn: {row}");
    let archive = json!({"id": new_id(), "revision": 2, "name": "Renamed task", "archived": true});
    let archived = app.post_expect(&edit, archive.clone(), 200).await?;
    ensure!(archived["revision"] == 3, "archive: {archived}");
    ensure!(
        app.get(&turns).await?[0]["status"] == "cancelled",
        "archive left work running"
    );
    ensure!(
        app.post_expect(&edit, archive, 200).await? == archived,
        "archive replay differs"
    );
    let restore = json!({"id": new_id(), "revision": 3, "name": "Renamed task", "archived": false});
    let restored = app.post_expect(&edit, restore.clone(), 200).await?;
    ensure!(
        restored["revision"] == 4 && restored["archived"] == 0,
        "restore: {restored}"
    );
    ensure!(
        app.post_expect(&edit, restore, 200).await? == restored,
        "restore replay differs"
    );
    let deletion = json!({"id": new_id(), "revision": 4});
    let deleted = app.post_expect(&remove, deletion.clone(), 200).await?;
    ensure!(
        deleted["deleted"] == 1 && deleted["revision"] == 5,
        "delete: {deleted}"
    );
    ensure!(
        app.post_expect(&remove, deletion.clone(), 200).await? == deleted,
        "delete replay differs"
    );
    let mut stale = deletion.clone();
    stale["revision"] = json!(3);
    app.post_expect(&remove, stale, 409).await?;
    ensure!(app.get(&list).await? == json!([]), "deleted thread listed");
    app.get_expect(&turns, 404).await?;
    ensure!(
        app.get("/api/conversations/settings").await? == json!({"autoArchive": true}),
        "default conversation settings"
    );
    app.post_expect(
        "/api/conversations/settings",
        json!({"id": new_id(), "autoArchive": "yes"}),
        400,
    )
    .await?;
    app.post_expect(
        "/api/conversations/settings",
        json!({"id": new_id(), "autoArchive": false}),
        200,
    )
    .await?;
    app.restart().await?;
    ensure!(
        app.post_expect(&remove, deletion, 200).await? == deleted,
        "delete receipt lost on restart"
    );
    ensure!(
        app.get(&list).await? == json!([]),
        "deleted thread listed after restart"
    );
    ensure!(
        app.get("/api/conversations/settings").await? == json!({"autoArchive": false}),
        "conversation settings lost on restart"
    );
    Ok(())
}
