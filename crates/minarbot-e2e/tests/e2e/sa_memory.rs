//! Memory CRUD, retry receipts, scoped search, bounded context and capacity
//! (port of `app/memory.test.ts`).

use crate::sa_support::{
    after_tool, api_agent, channel, count, id, last_tool_result, offers, settle, side_thread,
};
use minarbot_testkit::{
    model::{ModelRequest, Reply, ScriptedModel},
    prelude::*,
    Server,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

fn command(text: &str, essential: bool) -> Value {
    json!({"id": new_id(), "entry": new_id(), "revision": 0, "text": text,
        "essential": essential, "remove": false})
}

fn with(base: &Value, fields: Value) -> Value {
    let mut value = base.clone();
    if let (Some(value), Some(fields)) = (value.as_object_mut(), fields.as_object()) {
        value.extend(fields.clone());
    }
    value
}

fn texts(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["text"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Run one turn and return the system contexts and tool results of the model
/// requests it made.
async fn recall(
    app: &Server,
    model: &ScriptedModel,
    path: &str,
    status: &str,
) -> Result<(Vec<String>, Vec<Value>)> {
    let before = model.calls();
    let turn = new_id();
    app.post(path, json!({"id": turn, "text": "Recall"}))
        .await?;
    let row = settle(app, path, &turn).await?;
    ensure!(row["status"] == status, "turn in {path}: {row}");
    let requests: Vec<ModelRequest> = model.requests().into_iter().skip(before).collect();
    Ok((
        requests.iter().map(ModelRequest::system).collect(),
        requests
            .iter()
            .filter(|request| after_tool(request))
            .filter_map(last_tool_result)
            .collect(),
    ))
}

#[tokio::test]
async fn memory_crud_exact_retry_isolated_provider_search_and_bounded_context_survive_restart(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    let malformed = Arc::new(AtomicBool::new(false));
    let flag = malformed.clone();
    model.respond(move |request| {
        if offers(request, "search_memory") && !after_tool(request) {
            let arguments = if flag.load(Ordering::SeqCst) {
                json!({"owner": "foreign"})
            } else {
                json!({})
            };
            return Reply::tool_call(&new_id(), "search_memory", arguments);
        }
        Reply::text("Done")
    });
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new().model(&model)).await?;
    let a = id(&api_agent(&app, "A").await?)?;
    let b = id(&api_agent(&app, "B").await?)?;
    let room = id(&channel(&app, "C", &a, &[&a, &b]).await?)?;
    let side = id(&side_thread(&app, &a, "Side").await?)?;
    let pa = format!("/api/agents/{a}/memory");
    let pb = format!("/api/agents/{b}/memory");
    let pc = format!("/api/channels/{room}/memory");

    let original = command("PRIVATE_A", true);
    let created = app.post(&pa, original.clone()).await?;
    ensure!(
        app.post(&pa, original.clone()).await? == created,
        "exact retry differs"
    );
    app.post_expect(&pb, original.clone(), 409).await?;
    app.post_expect(
        &pa,
        with(&original, json!({"id": new_id(), "entry": new_id()})),
        409,
    )
    .await?;
    app.post(&pa, command("SEARCHABLE_A", false)).await?;
    app.post(&pb, command("PRIVATE_B", true)).await?;
    let shared = command("SHARED_CHANNEL", true);
    let shared_result = app.post(&pc, shared.clone()).await?;

    for path in [
        format!("/api/agents/{a}/turns"),
        format!("/api/threads/{side}/turns"),
    ] {
        let (contexts, results) = recall(&app, &model, &path, "done").await?;
        let context = contexts.first().ok_or("no model request")?;
        ensure!(context.contains("PRIVATE_A"), "{path} context: {context}");
        for hidden in ["PRIVATE_B", "SHARED_CHANNEL", "SEARCHABLE_A"] {
            ensure!(
                !context.contains(hidden),
                "{path} context has {hidden}: {context}"
            );
        }
        let mut found = texts(results.first().ok_or("no search result")?);
        found.sort();
        ensure!(
            found == ["PRIVATE_A", "SEARCHABLE_A"],
            "{path} search: {found:?}"
        );
    }
    let (contexts, results) =
        recall(&app, &model, &format!("/api/channels/{room}/turns"), "done").await?;
    let context = contexts.first().ok_or("no channel request")?;
    ensure!(
        context.contains("SHARED_CHANNEL"),
        "channel context: {context}"
    );
    for hidden in ["PRIVATE_A", "PRIVATE_B", "SEARCHABLE_A"] {
        ensure!(!context.contains(hidden), "channel context has {hidden}");
    }
    ensure!(
        results.first().map(|page| page["items"][0]["text"].clone())
            == Some(json!("SHARED_CHANNEL")),
        "channel search: {results:?}"
    );
    let (contexts, _) = recall(&app, &model, &format!("/api/agents/{b}/turns"), "done").await?;
    let context = contexts.first().ok_or("no B request")?;
    ensure!(context.contains("PRIVATE_B"), "B context: {context}");
    ensure!(
        !context.contains("PRIVATE_A") && !context.contains("SHARED_CHANNEL"),
        "B context leaked: {context}"
    );

    let workflow = app
        .post(
            &format!("/api/agents/{a}/workflows"),
            json!({"id": new_id(), "revision": 0, "name": "Memory exclusion", "graph": {
                "entry": "model", "maxSteps": 2, "nodes": [
                    {"id": "model", "kind": "model", "model": "fixture", "prompt": "Model only", "next": "out"},
                    {"id": "out", "kind": "output"}]}}),
        )
        .await?;
    let before = model.calls();
    let model_run = app
        .post(
            &format!("/api/workflows/{}/run", id(&workflow)?),
            json!({"id": new_id(), "revision": 1, "input": "Test"}),
        )
        .await?;
    app.wait_get(&format!("/api/workflow-runs/{}", id(&model_run)?), |run| {
        run["status"] == "done"
    })
    .await?;
    let requests: Vec<ModelRequest> = model.requests().into_iter().skip(before).collect();
    let results: Vec<Value> = requests
        .iter()
        .filter(|request| after_tool(request))
        .filter_map(last_tool_result)
        .collect();
    ensure!(
        results.is_empty(),
        "workflow model step used tools: {results:?}"
    );
    let context = requests
        .first()
        .map(ModelRequest::system)
        .unwrap_or_default();
    for hidden in ["PRIVATE_A", "SEARCHABLE_A", "SHARED_CHANNEL"] {
        ensure!(
            !context.contains(hidden),
            "workflow context has {hidden}: {context}"
        );
    }

    malformed.store(true, Ordering::SeqCst);
    let (_, results) = recall(&app, &model, &format!("/api/agents/{a}/turns"), "done").await?;
    malformed.store(false, Ordering::SeqCst);
    ensure!(
        results == [json!({"error": "Invalid memory search.", "status": 400})],
        "malformed search: {results:?}"
    );

    let edit = with(
        &original,
        json!({"id": new_id(), "revision": 1, "text": "UPDATED_A"}),
    );
    app.post(&pa, edit.clone()).await?;
    app.post_expect(&pa, with(&edit, json!({"id": new_id()})), 409)
        .await?;
    ensure!(
        app.post(&pa, original.clone()).await? == created,
        "original receipt changed"
    );
    ensure!(
        count(&app.get(&format!("{pa}?query=UPDATED")).await?, "items") == 1,
        "updated entry not found"
    );
    let removal = with(
        &edit,
        json!({"id": new_id(), "revision": 2, "remove": true, "text": "", "essential": false}),
    );
    app.post(&pa, removal).await?;
    app.post_expect(
        &pa,
        with(&edit, json!({"id": new_id(), "revision": 3})),
        409,
    )
    .await?;
    app.post(&pa, edit.clone()).await?;
    ensure!(
        count(&app.get(&format!("{pa}?query=UPDATED")).await?, "items") == 0,
        "edit replay resurrected the entry"
    );
    app.post(&pa, command(&"界".repeat(2000), true)).await?;
    app.post_expect(&pa, command(&"文".repeat(2000), true), 413)
        .await?;
    ensure!(count(&app.get(&pa).await?, "items") == 2, "context budget");
    for n in 0..11 {
        app.post(&pa, command(&format!("page {n}"), false)).await?;
    }
    let first = app.get(&pa).await?;
    let after = first["next_after"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| first["next_after"].to_string());
    let second = app.get(&format!("{pa}?after={after}")).await?;
    ensure!(count(&first, "items") == 10, "first page: {first}");
    ensure!(count(&second, "items") == 3, "second page: {second}");
    let mut ids: Vec<Value> = first["items"]
        .as_array()
        .into_iter()
        .chain(second["items"].as_array())
        .flatten()
        .map(|item| item["id"].clone())
        .collect();
    ids.sort_by_key(Value::to_string);
    ids.dedup();
    ensure!(ids.len() == 13, "pages overlap: {ids:?}");
    ensure!(
        count(&app.get(&format!("{pa}?query=page")).await?, "items") == 10,
        "query page size"
    );
    app.get_expect(&format!("{pa}?after=bad"), 400).await?;
    app.get_expect(&format!("/api/agents/{}/memory", new_id()), 404)
        .await?;
    app.post_expect(
        &pa,
        with(&command("bad", false), json!({"extra": true})),
        400,
    )
    .await?;
    // Server-side form of the `memoryCommand` shape checks.
    for bad in [
        Value::Null,
        json!([]),
        json!({}),
        with(&original, json!({"text": ""})),
        with(&original, json!({"revision": -1})),
        with(&original, json!({"essential": 1})),
    ] {
        app.post_expect(&pa, bad, 400).await?;
    }

    app.restart().await?;
    ensure!(
        app.post(&pa, original).await? == created,
        "receipt lost on restart"
    );
    ensure!(
        count(&app.get(&format!("{pa}?query=UPDATED")).await?, "items") == 0,
        "removed entry back after restart"
    );
    app.post(
        &format!("/api/channels/{room}/edit"),
        json!({"id": new_id(), "revision": 1, "name": "C", "instructions": "Help", "lead": a,
            "members": [a, b], "archived": true}),
    )
    .await?;
    ensure!(
        app.post(&pc, shared).await? == shared_result,
        "archived replay"
    );
    app.post_expect(&pc, command("archived", false), 409)
        .await?;
    ensure!(
        count(&app.get(&pc).await?, "items") == 1,
        "archived channel memory"
    );
    Ok(())
}

#[tokio::test]
async fn rust_memory_permits_edits_at_capacity_and_rolls_back_edits_over_the_context_budget(
) -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let agent = api_agent(&app, "Memory capacity").await?;
    let path = format!("/api/agents/{}/memory", id(&agent)?);
    let original = command("first", false);
    let second = command("unique second note", false);
    app.post_expect(&path, original.clone(), 200).await?;
    app.post_expect(&path, second.clone(), 200).await?;
    for index in 2..128 {
        app.post_expect(&path, command(&format!("note {index}"), false), 200)
            .await?;
    }
    let overflow = app
        .post_expect(&path, command("overflow", false), 413)
        .await?;
    ensure!(
        overflow["error"]
            .as_str()
            .is_some_and(|error| error.contains("128")),
        "overflow: {overflow}"
    );
    let revised = app
        .post_expect(
            &path,
            with(
                &original,
                json!({"id": new_id(), "revision": 1, "text": "edited"}),
            ),
            200,
        )
        .await?;
    ensure!(revised["text"] == "edited", "revised: {revised}");
    app.post_expect(
        &path,
        with(
            &original,
            json!({"id": new_id(), "revision": 2, "text": "", "remove": true}),
        ),
        200,
    )
    .await?;
    app.post_expect(
        &path,
        with(
            &command(&"界".repeat(2000), false),
            json!({"essential": true}),
        ),
        200,
    )
    .await?;
    let denied = app
        .post_expect(
            &path,
            with(
                &second,
                json!({"id": new_id(), "revision": 1, "text": "文".repeat(2000), "essential": true}),
            ),
            413,
        )
        .await?;
    ensure!(
        denied["error"]
            .as_str()
            .is_some_and(|error| error.contains("8 KiB")),
        "denied: {denied}"
    );
    let page = app
        .get_expect(&format!("{path}?query=unique%20second%20note"), 200)
        .await?;
    let saved = page["items"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["id"] == second["entry"]))
        .ok_or_else(|| format!("second note missing: {page}"))?;
    ensure!(saved["essential"] == 0, "essential: {saved}");
    ensure!(saved["text"] == "unique second note", "text: {saved}");
    ensure!(saved["revision"] == 1, "revision: {saved}");
    Ok(())
}
