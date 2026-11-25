//! Queue edits and queue order (ports of `app/queue-edits.test.ts` and
//! `app/queue-order.test.ts`).

use crate::fx_support::{api_agent, fetch, gated, id, row, rows, settle, upload};
use minarbot_testkit::{
    db::{Fixture, Inspector},
    gated::{Answer, GatedModel, Parked},
    prelude::*,
    Server,
};

/// `BLOCK` parks the request for the test; everything else answers `Done`.
async fn blocking_model() -> Result<GatedModel> {
    let model = GatedModel::start().await?;
    model.respond(|request| {
        if request.last_text() == "BLOCK" {
            Answer::Park
        } else {
            Answer::text("Done")
        }
    });
    Ok(model)
}

fn calls(model: &GatedModel) -> Vec<String> {
    model
        .requests()
        .iter()
        .map(|request| request.last_text())
        .collect()
}

fn done(parked: Parked) {
    parked.answer(Answer::text("Done"));
}

async fn post_id(app: &Server, path: &str, body: Value) -> Result<Value> {
    app.post(path, body).await
}

#[tokio::test]
async fn queue_edit_holds_survive_restart_preserve_order_and_original_receipt_reject_competing_editors_and_completed_cancellations(
) -> Result<()> {
    let model = blocking_model().await?;
    let root = TestRoot::new()?;
    let mut app = root.start(gated(&model).concurrency(1)).await?;
    let a = id(&api_agent(&app, "A", "fixture").await?)?;
    let b = id(&api_agent(&app, "B", "fixture").await?)?;
    let side = id(&app
        .post(
            &format!("/api/agents/{a}/threads"),
            json!({"id": new_id(), "name": "Side"}),
        )
        .await?)?;
    let channel = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Channel", "lead": a, "members": [a], "instructions": "Help"}),
        )
        .await?)?;
    let mut hidden_revision = 0;
    for path in [
        format!("/api/agents/{a}/turns"),
        format!("/api/threads/{side}/turns"),
        format!("/api/channels/{channel}/turns"),
    ] {
        let blocker = new_id();
        post_id(&app, &path, json!({"id": blocker, "text": "BLOCK"})).await?;
        let held = model.parked().await?;
        let files = path.trim_end_matches("/turns").to_owned() + "/files";
        let file_id = new_id();
        let file_path = format!("{files}/{file_id}");
        ensure!(
            upload(&app, &file_path, "note.txt", "Saved attachment").await? == 200,
            "upload"
        );
        let paginated = path.starts_with("/api/agents/");
        if paginated {
            for n in 0..100 {
                let status = upload(
                    &app,
                    &format!("{files}/{}", new_id()),
                    &format!("newer-{n}.txt"),
                    "newer",
                )
                .await?;
                ensure!(status == 200, "newer upload {n}: {status}");
            }
            let first_page = rows(&app, &files).await?;
            ensure!(first_page.len() == 100, "first page {}", first_page.len());
            ensure!(
                !first_page.iter().any(|file| file["id"] == file_id.as_str()),
                "oldest file on first page"
            );
            let before = first_page.last().ok_or("empty page")?["seq"].clone();
            let second_page = rows(&app, &format!("{files}?before={before}")).await?;
            let ids: Vec<&Value> = second_page.iter().map(|file| &file["id"]).collect();
            ensure!(ids == [&json!(file_id)], "second page {ids:?}");
        }
        let (original_files, edited_files) = if paginated {
            (json!([]), json!([file_id]))
        } else {
            (json!([file_id]), json!([]))
        };
        let command = json!({"id": new_id(), "text": "ORIGINAL", "files": original_files});
        let tail = new_id();
        let turn = app.post(&path, command.clone()).await?;
        let turn_id = id(&turn)?;
        app.post(&path, json!({"id": tail, "text": "TAIL"})).await?;
        let edit_path = format!("{path}/{turn_id}/edit");
        let editor = new_id();
        let hold = json!({"id": new_id(), "editor": editor, "revision": turn["revision"], "action": "hold"});
        let accepted = app.post(&edit_path, hold.clone()).await?;
        ensure!(accepted["files"] == original_files, "hold files {accepted}");
        ensure!(
            app.post(&edit_path, hold.clone()).await? == accepted,
            "hold replay"
        );
        let mut renew = hold.clone();
        renew["id"] = json!(new_id());
        renew["revision"] = accepted["revision"].clone();
        renew["action"] = json!("renew");
        ensure!(
            app.post(&edit_path, renew.clone()).await?["revision"] == accepted["revision"],
            "renew revision"
        );
        ensure!(
            app.post(&edit_path, renew.clone()).await?
                == app.post(&edit_path, renew.clone()).await?,
            "renew replay"
        );
        let mut competing = renew.clone();
        competing["id"] = json!(new_id());
        competing["editor"] = json!(new_id());
        app.post_expect(&edit_path, competing, 409).await?;
        let mut stale = renew.clone();
        stale["id"] = json!(new_id());
        stale["revision"] = turn["revision"].clone();
        app.post_expect(&edit_path, stale, 409).await?;
        app.post_expect(
            &format!("/api/agents/{b}/turns/{turn_id}/edit"),
            hold.clone(),
            404,
        )
        .await?;
        let mut second = hold.clone();
        second["id"] = json!(new_id());
        second["editor"] = json!(new_id());
        second["revision"] = accepted["revision"].clone();
        app.post_expect(&edit_path, second, 409).await?;
        app.post_expect(
            &format!("{path}/{turn_id}/pause"),
            json!({"id": new_id(), "revision": accepted["revision"]}),
            409,
        )
        .await?;
        let public_rows = app.get(&path).await?;
        ensure!(
            row(&app, &path, &turn_id).await?["editing"] == true,
            "editing flag"
        );
        ensure!(
            !public_rows.to_string().contains(&editor),
            "editor id leaked: {public_rows}"
        );
        if path.contains("/channels/") {
            app.post(
                &format!("/api/agents/{a}/visibility"),
                json!({"id": new_id(), "revision": hidden_revision, "hidden": true}),
            )
            .await?;
            hidden_revision += 1;
            ensure!(
                row(&app, &path, &turn_id).await?["revision"] == accepted["revision"],
                "hiding the lead changed the held turn"
            );
            app.post(
                &format!("/api/agents/{a}/visibility"),
                json!({"id": new_id(), "revision": hidden_revision, "hidden": false}),
            )
            .await?;
            hidden_revision += 1;
        }
        done(held);
        settle(&app, &path, &blocker, "done").await?;
        app.restart().await?;
        ensure!(
            row(&app, &path, &tail).await?["status"] == "queued",
            "tail ran while the edit held the queue"
        );
        app.post_expect(
            &edit_path,
            json!({"id": new_id(), "editor": editor, "revision": accepted["revision"], "action": "save",
                "text": "WRONG", "files": [new_id()]}),
            404,
        )
        .await?;
        let save = json!({"id": new_id(), "editor": editor, "revision": accepted["revision"], "action": "save",
            "text": "CHANGED", "files": edited_files});
        let result = app.post(&edit_path, save.clone()).await?;
        ensure!(
            app.post(&edit_path, save.clone()).await? == result,
            "save replay"
        );
        settle(&app, &path, &tail, "done").await?;
        let seen = calls(&model);
        let n = seen.len();
        ensure!(n >= 3, "calls {seen:?}");
        ensure!(
            seen[n - 3] == "BLOCK" && seen[n - 1] == "TAIL",
            "order {seen:?}"
        );
        if paginated {
            ensure!(
                seen[n - 2].contains("CHANGED") && seen[n - 2].contains("Saved attachment"),
                "edited prompt {:?}",
                seen[n - 2]
            );
        } else {
            ensure!(seen[n - 2] == "CHANGED", "edited prompt {:?}", seen[n - 2]);
        }
        let edited = app.post(&path, command.clone()).await?;
        ensure!(edited["text"] == "CHANGED", "receipt text {edited}");
        let attachments: Value =
            serde_json::from_str(edited["attachments"].as_str().ok_or("attachments")?)?;
        ensure!(
            attachments == edited_files,
            "receipt attachments {attachments}"
        );
        let (status, bytes) = fetch(&app, &file_path).await?;
        ensure!(
            status == 200 && bytes == b"Saved attachment",
            "file {status}"
        );
        let mut changed = command.clone();
        changed["text"] = json!("CHANGED");
        app.post_expect(&path, changed, 409).await?;
        let mut cancel = save.clone();
        cancel["id"] = json!(new_id());
        cancel["action"] = json!("cancel");
        if let Some(fields) = cancel.as_object_mut() {
            fields.remove("text");
            fields.remove("files");
        }
        app.post_expect(&edit_path, cancel, 409).await?;
    }
    let path = format!("/api/agents/{a}/turns");
    app.post(&path, json!({"id": new_id(), "text": "BLOCK"}))
        .await?;
    let held = model.parked().await?;
    let turn = app
        .post(&path, json!({"id": new_id(), "text": "UNCHANGED"}))
        .await?;
    let turn_id = id(&turn)?;
    let edit_path = format!("{path}/{turn_id}/edit");
    let editor = new_id();
    let hold = app
        .post(
            &edit_path,
            json!({"id": new_id(), "editor": editor, "revision": turn["revision"], "action": "hold"}),
        )
        .await?;
    let cancel =
        json!({"id": new_id(), "editor": editor, "revision": hold["revision"], "action": "cancel"});
    let result = app.post(&edit_path, cancel.clone()).await?;
    ensure!(
        app.post(&edit_path, cancel).await? == result,
        "cancel replay"
    );
    app.post_expect(
        &edit_path,
        json!({"id": new_id(), "editor": editor, "revision": result["revision"], "action": "save",
            "text": "NEVER", "files": []}),
        409,
    )
    .await?;
    done(held);
    settle(&app, &path, &turn_id, "done").await?;
    ensure!(
        calls(&model).last().map(String::as_str) == Some("UNCHANGED"),
        "last call {:?}",
        calls(&model)
    );
    Ok(())
}

#[tokio::test]
async fn abandoned_edit_lease_expires_and_drains_its_message_and_following_queue_in_order(
) -> Result<()> {
    let model = blocking_model().await?;
    let root = TestRoot::new()?;
    let mut app = root.start(gated(&model)).await?;
    let agent = id(&api_agent(&app, "Abandoned", "fixture").await?)?;
    let path = format!("/api/agents/{agent}/turns");
    let blocker = new_id();
    app.post(&path, json!({"id": blocker, "text": "BLOCK"}))
        .await?;
    let turn = app
        .post(&path, json!({"id": new_id(), "text": "ABANDONED"}))
        .await?;
    let turn_id = id(&turn)?;
    let tail = id(&app
        .post(&path, json!({"id": new_id(), "text": "TAIL"}))
        .await?)?;
    let editor = new_id();
    let hold =
        json!({"id": new_id(), "editor": editor, "revision": turn["revision"], "action": "hold"});
    app.post(&format!("{path}/{turn_id}/edit"), hold.clone())
        .await?;
    done(model.parked().await?);
    settle(&app, &path, &blocker, "done").await?;
    app.edit_database(|db: &Fixture| {
        db.execute(
            "UPDATE queue_edits SET expires_at=0 WHERE editor=?",
            [&editor],
        )?;
        Ok(())
    })
    .await?;
    let tail_row = settle(&app, &path, &tail, "done").await?;
    ensure!(
        row(&app, &path, &turn_id).await?["editing"] == false,
        "Abandoned editor must release its queue hold"
    );
    ensure!(
        tail_row["status"] == "done",
        "Queue must drain after abandoned edit"
    );
    ensure!(
        calls(&model) == ["BLOCK", "ABANDONED", "TAIL"],
        "calls {:?}",
        calls(&model)
    );
    let mut stale = hold;
    stale["id"] = json!(new_id());
    stale["revision"] = json!(2);
    stale["action"] = json!("save");
    stale["text"] = json!("STALE");
    stale["files"] = json!([]);
    app.post_expect(&format!("{path}/{turn_id}/edit"), stale, 409)
        .await?;
    Ok(())
}

#[tokio::test]
async fn rust_expires_migrated_permanent_queue_holds_and_fences_the_prior_revision_once(
) -> Result<()> {
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new()).await?;
    let legacy = new_id();
    let current = new_id();
    app.edit_database(|db| {
        db.execute_batch(
            "INSERT INTO agents(id,name,model,instructions) VALUES ('fixture','Fixture','fixture','Help');
             ALTER TABLE queue_edits DROP COLUMN expires_at;",
        )?;
        db.execute(
            "INSERT INTO turns(id,agent,text,status,created,paused) VALUES (?,'fixture','Legacy','queued','2026-01-01',1)",
            [&legacy],
        )?;
        db.execute(
            "INSERT INTO queue_edits(editor,turn,state,original) VALUES ('legacy-editor',?,'held','{}')",
            [&legacy],
        )?;
        Ok(())
    })
    .await?;
    let edit = |turn: &str| -> Result<Value> {
        Inspector::open(root.database())?.row(
            "SELECT state,expires_at FROM queue_edits WHERE turn=?",
            [turn],
        )
    };
    let revision = |turn: &str| -> Result<i64> {
        Inspector::open(root.database())?.scalar("SELECT revision FROM turns WHERE id=?", [turn])
    };
    ensure!(
        edit(&legacy)? == json!({"state": "cancelled", "expires_at": 0}),
        "legacy edit {}",
        edit(&legacy)?
    );
    ensure!(
        revision(&legacy)? == 2,
        "legacy revision {}",
        revision(&legacy)?
    );
    // Queue edit leases use SQLite's wall clock, not the server test clock.
    let deadline = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis(),
    )? + 30_000;
    {
        let db = Fixture::open(root.database())?;
        db.execute(
            "INSERT INTO turns(id,agent,text,status,created,paused) VALUES (?,'fixture','Current','queued','2026-01-01',1)",
            [&current],
        )?;
        db.execute(
            "INSERT INTO queue_edits(editor,turn,state,original,expires_at) VALUES ('current-editor',?,'held','{}',?)",
            (&current, deadline),
        )?;
    }
    app.restart().await?;
    ensure!(revision(&legacy)? == 2, "legacy fenced twice");
    ensure!(
        edit(&current)? == json!({"state": "held", "expires_at": deadline}),
        "current edit {}",
        edit(&current)?
    );
    ensure!(
        revision(&current)? == 1,
        "current revision {}",
        revision(&current)?
    );
    app.edit_database(|db| {
        db.execute(
            "UPDATE queue_edits SET expires_at=0 WHERE turn=?",
            [&current],
        )?;
        Ok(())
    })
    .await?;
    ensure!(
        edit(&current)?["state"] == "cancelled",
        "current not expired"
    );
    ensure!(
        revision(&current)? == 2,
        "current revision {}",
        revision(&current)?
    );
    Ok(())
}

#[tokio::test]
async fn queue_order_survives_restart_preserves_transcript_order_blocks_edits_and_scopes_supplies_reordered_context(
) -> Result<()> {
    let model = GatedModel::start().await?;
    model.respond(|request| {
        let messages = request.messages();
        if messages
            .first()
            .and_then(|message| message["content"].as_str())
            .is_some_and(|content| content.starts_with("Summarize conversation history"))
        {
            return Answer::text("REORDERED SUMMARY");
        }
        let text = request.last_text();
        if text == "EARLY LAST"
            && !messages.iter().any(|message| {
                message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("REORDERED SUMMARY"))
            })
        {
            return Answer::Status(
                500,
                json!({"error": "EARLY LAST without reordered summary"}),
            );
        }
        if text == "FIRST"
            && !messages
                .iter()
                .any(|message| message["role"] == "user" && message["content"] == "SECOND")
        {
            return Answer::Status(500, json!({"error": "FIRST without SECOND before it"}));
        }
        if text == "BLOCK" {
            Answer::Park
        } else {
            Answer::text("Done")
        }
    });
    let root = TestRoot::new()?;
    let mut app = root.start(gated(&model).concurrency(1)).await?;
    let a = id(&api_agent(&app, "A", "fixture").await?)?;
    let side = id(&app
        .post(
            &format!("/api/agents/{a}/threads"),
            json!({"id": new_id(), "name": "Side"}),
        )
        .await?)?;
    let channel = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Channel", "lead": a, "members": [a], "instructions": "Help"}),
        )
        .await?)?;
    let work = |model: &GatedModel| -> Vec<String> {
        model
            .requests()
            .iter()
            .filter(|request| {
                !request
                    .system()
                    .starts_with("Summarize conversation history")
            })
            .map(|request| request.last_text())
            .collect()
    };
    for path in [
        format!("/api/agents/{a}/turns"),
        format!("/api/threads/{side}/turns"),
        format!("/api/channels/{channel}/turns"),
    ] {
        app.post(&path, json!({"id": new_id(), "text": "BLOCK"}))
            .await?;
        let held = model.parked().await?;
        let first = app
            .post(&path, json!({"id": new_id(), "text": "FIRST"}))
            .await?;
        let second = app
            .post(&path, json!({"id": new_id(), "text": "SECOND"}))
            .await?;
        let (first_id, second_id) = (id(&first)?, id(&second)?);
        let endpoint = path.trim_end_matches("/turns").to_owned() + "/queue";
        let editor = new_id();
        let hold = app
            .post(
                &format!("{path}/{first_id}/edit"),
                json!({"id": new_id(), "editor": editor, "revision": first["revision"], "action": "hold"}),
            )
            .await?;
        let mut swap = json!({"id": new_id(), "target": first_id, "other": second_id,
            "revision": hold["revision"], "otherRevision": second["revision"]});
        app.post_expect(&endpoint, swap.clone(), 409).await?;
        let cancelled = app
            .post(
                &format!("{path}/{first_id}/edit"),
                json!({"id": new_id(), "editor": editor, "revision": hold["revision"], "action": "cancel"}),
            )
            .await?;
        swap["revision"] = cancelled["revision"].clone();
        let result = app.post(&endpoint, swap.clone()).await?;
        ensure!(
            app.post(&endpoint, swap.clone()).await? == result,
            "swap replay"
        );
        let queue = app.get(&endpoint).await?;
        let order: Vec<&Value> = queue["turns"]
            .as_array()
            .ok_or("queue turns")?
            .iter()
            .map(|row| &row["id"])
            .collect();
        ensure!(
            order == [&json!(second_id), &json!(first_id)],
            "queue {order:?}"
        );
        let mut again = swap.clone();
        again["id"] = json!(new_id());
        app.post_expect(&endpoint, again.clone(), 409).await?;
        again["id"] = json!(new_id());
        app.post_expect(&format!("/api/agents/{}/queue", new_id()), again, 404)
            .await?;
        app.restart().await?;
        drop(held);
        settle(&app, &path, &first_id, "done").await?;
        let seen = work(&model);
        ensure!(
            seen.len() >= 3 && seen[seen.len() - 3..] == ["BLOCK", "SECOND", "FIRST"],
            "calls {seen:?}"
        );
        let history = rows(&app, &path).await?;
        let position = |id: &str| history.iter().position(|row| row["id"] == id);
        ensure!(
            position(&first_id) < position(&second_id),
            "history order {history:?}"
        );
        ensure!(
            app.get(&endpoint).await?["turns"] == json!([]),
            "queue not empty"
        );
        ensure!(
            app.post(&endpoint, swap).await? == result,
            "swap receipt after restart"
        );
    }
    let path = format!("/api/agents/{a}/turns");
    app.post(&path, json!({"id": new_id(), "text": "BLOCK"}))
        .await?;
    let held = model.parked().await?;
    let early = app
        .post(&path, json!({"id": new_id(), "text": "EARLY LAST"}))
        .await?;
    let mut last = Value::Null;
    for n in 0..45 {
        last = app
            .post(&path, json!({"id": new_id(), "text": format!("LATE {n}")}))
            .await?;
    }
    app.post(
        &format!("/api/agents/{a}/queue"),
        json!({"id": new_id(), "target": early["id"], "other": last["id"],
            "revision": early["revision"], "otherRevision": last["revision"]}),
    )
    .await?;
    let offset = work(&model).len();
    done(held);
    let mut order = vec!["LATE 44".to_owned()];
    order.extend((0..44).map(|n| format!("LATE {n}")));
    order.push("EARLY LAST".into());
    for (index, text) in order.iter().enumerate() {
        let seen = until(text.clone(), || async {
            let seen = work(&model);
            Ok((seen.len() > offset + index).then_some(seen))
        })
        .await?;
        ensure!(
            seen[offset + index] == *text,
            "call {} was {:?}, expected {text}",
            offset + index,
            seen[offset + index]
        );
    }
    settle(&app, &path, &id(&early)?, "done").await?;
    let last_id = id(&last)?;
    ensure!(
        model.requests().iter().any(|request| {
            request
                .system()
                .starts_with("Summarize conversation history")
                && request
                    .messages()
                    .get(1)
                    .and_then(|message| message["content"].as_str())
                    .and_then(|content| serde_json::from_str::<Value>(content).ok())
                    .is_some_and(|summary| {
                        summary["messages"].as_array().is_some_and(|messages| {
                            messages
                                .iter()
                                .any(|message| message["id"] == last_id.as_str())
                        })
                    })
        }),
        "no summary covered the reordered last message"
    );
    app.post(&path, json!({"id": new_id(), "text": "BLOCK"}))
        .await?;
    let _held = model.parked().await?;
    let b = id(&api_agent(&app, "B", "fixture").await?)?;
    let group = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Large queue", "lead": a, "members": [a, b], "instructions": "Help"}),
        )
        .await?)?;
    let group_path = format!("/api/channels/{group}/turns");
    let group_queue = format!("/api/channels/{group}/queue");
    let mut ids = Vec::new();
    for n in 0..101 {
        let turn = new_id();
        ids.push(json!(turn));
        app.post(
            &group_path,
            json!({"id": turn, "text": format!("QUEUED {n}"), "recipient": if n < 98 { &a } else { &b }}),
        )
        .await?;
    }
    let first_page = app.get(&group_queue).await?;
    let first_turns = first_page["turns"].as_array().ok_or("turns")?.clone();
    ensure!(first_turns.len() == 100, "first page {}", first_turns.len());
    ensure!(first_page["more"] == true, "more {first_page}");
    let cursor = format!(
        "?after={}&snapshot={}",
        first_page["next_after"],
        first_page["snapshot"].as_str().ok_or("snapshot")?
    );
    let second_page = app.get(&format!("{group_queue}{cursor}")).await?;
    let second_turns = second_page["turns"].as_array().ok_or("turns")?.clone();
    ensure!(second_turns.len() == 1, "second page {second_page}");
    ensure!(
        second_page["next_after"].is_null(),
        "next_after {second_page}"
    );
    let all: Vec<Value> = first_turns
        .iter()
        .chain(second_turns.iter())
        .map(|row| row["id"].clone())
        .collect();
    ensure!(all == ids, "paged ids differ");
    let tail = first_turns.last().ok_or("tail")?;
    ensure!(tail["next"]["id"] == second_turns[0]["id"], "next link");
    ensure!(
        second_turns[0]["previous"]["id"] == tail["id"],
        "previous link"
    );
    app.get_expect(
        &format!(
            "{group_queue}?after=NaN&snapshot={}",
            first_page["snapshot"].as_str().unwrap_or_default()
        ),
        400,
    )
    .await?;
    app.get_expect(&format!("{group_queue}?after=1"), 400)
        .await?;
    app.get_expect(&format!("/api/agents/{b}/queue{cursor}"), 409)
        .await?;
    let (left, right) = (&first_turns[0], &first_turns[1]);
    app.post(
        &group_queue,
        json!({"id": new_id(), "target": left["id"], "other": right["id"],
            "revision": left["revision"], "otherRevision": right["revision"]}),
    )
    .await?;
    app.get_expect(&format!("{group_queue}{cursor}"), 409)
        .await?;
    let boundary = &second_turns[0];
    let neighbor = &boundary["previous"];
    app.post(
        &group_queue,
        json!({"id": new_id(), "target": boundary["id"], "other": neighbor["id"],
            "revision": boundary["revision"], "otherRevision": neighbor["revision"]}),
    )
    .await?;
    let changed = app.get(&group_queue).await?;
    ensure!(
        changed["snapshot"] != first_page["snapshot"],
        "snapshot unchanged"
    );
    ensure!(
        changed["turns"][0]["id"] == ids[1],
        "first after swap {}",
        changed["turns"][0]
    );
    Ok(())
}
