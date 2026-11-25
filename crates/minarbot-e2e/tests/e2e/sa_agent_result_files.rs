//! A requester reads files a teammate returned, but not private inputs or
//! no-reply outputs (port of `app/agent-result-files.test.ts`).

use crate::sa_support::{lock, pdf_fixture, tool_json, turn_tool_results, Shared};
use minarbot_testkit::{
    bridge::Bridge,
    gate::ModelGate,
    http::Method,
    model::{Reply, ScriptedModel},
    prelude::*,
    Server,
};
use std::{
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

#[derive(Default)]
struct Script {
    worker: String,
    created: String,
    target: String,
    expects_reply: bool,
    restart: bool,
    open_returned: bool,
    creates: usize,
    results: Vec<Value>,
    returned: Vec<Value>,
}

async fn post_turn(app: &Server, path: &str, text: &str) -> Result<String> {
    let id = new_id();
    app.post(path, json!({"id": id, "text": text})).await?;
    Ok(id)
}

async fn wait(app: &Server, path: &str, id: &str, status: &str) -> Result<Value> {
    app.wait_status(path, id, status).await
}

/// PDF extraction hold: `true` while the next extraction must wait.
#[derive(Default)]
struct PdfHold {
    armed: bool,
    started: bool,
    released: bool,
}

async fn requester_reads_returned_teammate_artifacts(pdf: bool) -> Result<()> {
    let model = ScriptedModel::start().await?;
    let gate = ModelGate::start(&model).await?;
    let state: Shared<Script> = Arc::new(Mutex::new(Script {
        expects_reply: true,
        ..Script::default()
    }));
    let script = state.clone();
    let output = if pdf {
        String::from_utf8_lossy(&pdf_fixture("REQUESTED_OUTPUT", 1)).into_owned()
    } else {
        "REQUESTED_OUTPUT".to_owned()
    };
    model.respond(move |request| {
        let Ok(mut state) = script.lock() else {
            return Reply::status(500, json!({}));
        };
        if request.last_user_text().starts_with("One verification reminder") {
            return Reply::text("Done");
        }
        let last = turn_tool_results(request).last().map(tool_json);
        if request.model() == "worker" {
            return match last {
                None => {
                    state.creates += 1;
                    Reply::tool_call(
                        "create_text_file",
                        "create_text_file",
                        json!({"name": if pdf { "result.pdf" } else { "result.txt" }, "text": output}),
                    )
                }
                Some(result) => {
                    state.created = result["id"].as_str().unwrap_or_default().to_owned();
                    Reply::text("Done")
                }
            };
        }
        match last {
            None => Reply::tool_call(
                "send_message",
                "send_message",
                json!({"recipients": [state.worker], "text": "Make file", "expects_reply": state.expects_reply}),
            ),
            Some(result) if !result["request"].is_null() => {
                state.returned.push(result);
                if std::mem::take(&mut state.restart) {
                    return Reply::status(503, json!({}));
                }
                let id = if state.target.is_empty() { state.created.clone() } else { state.target.clone() };
                let name = if state.open_returned { "open_file_in_panel" } else { "read_attachment" };
                Reply::tool_call(name, name, json!({"id": id}))
            }
            Some(result) => {
                state.results.push(result.clone());
                Reply::text(result.to_string())
            }
        }
    });
    let hold: Arc<(Mutex<PdfHold>, Condvar)> = Arc::default();
    let pdf_hold = hold.clone();
    let bridge = Bridge::pdf(move |_| {
        let (lock, signal) = &*pdf_hold;
        if let Ok(mut hold) = lock.lock() {
            if std::mem::take(&mut hold.armed) {
                hold.started = true;
                signal.notify_all();
                // Bounded, so a failing test cannot leave the server hanging.
                let deadline = std::time::Instant::now() + Duration::from_secs(20);
                while !hold.released && std::time::Instant::now() < deadline {
                    let Ok((next, _)) = signal.wait_timeout(hold, Duration::from_millis(200))
                    else {
                        return None;
                    };
                    hold = next;
                }
            }
        }
        None
    })
    .await?;
    let root = TestRoot::new()?;
    let mut config = ServerConfig::new().gateway(gate.gateway());
    if pdf {
        config = config.pdf_bridge(&bridge);
    }
    let mut app = root.start(config).await?;
    let (owner, worker) = (new_id(), new_id());
    lock(&state)?.worker = worker.clone();
    for (id, model_name) in [(&owner, "owner"), (&worker, "worker")] {
        app.post(
            "/api/agents",
            json!({"runtime": "api", "id": id, "name": id, "model": model_name, "instructions": "Help"}),
        )
        .await?;
    }
    let private_thread = app
        .post(
            &format!("/api/agents/{worker}/threads"),
            json!({"id": new_id(), "name": "Private input"}),
        )
        .await?;
    let private_file = new_id();
    let status = app
        .request(
            Method::POST,
            &format!(
                "/api/threads/{}/files/{private_file}",
                private_thread["id"].as_str().unwrap_or_default()
            ),
        )?
        .header("content-type", "application/octet-stream")
        .header("x-file-name", "private.txt")
        .body("PRIVATE_INPUT")
        .send()
        .await?
        .status();
    ensure!(status.is_success(), "private upload: {status}");
    let side = app
        .post(
            &format!("/api/agents/{owner}/threads"),
            json!({"id": new_id(), "name": "Side"}),
        )
        .await?;
    let room = app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Room", "instructions": "Help", "members": [owner, worker], "lead": owner}),
        )
        .await?;
    let mut other_output = String::new();
    for path in [
        format!("/api/agents/{owner}/turns"),
        format!(
            "/api/threads/{}/turns",
            side["id"].as_str().unwrap_or_default()
        ),
        format!(
            "/api/channels/{}/turns",
            room["id"].as_str().unwrap_or_default()
        ),
    ] {
        lock(&state)?.created.clear();
        let id = new_id();
        let mut body = json!({"id": id, "text": "Return file"});
        if path.contains("/channels/") {
            body["recipient"] = json!(owner);
        }
        app.post(&path, body).await?;
        wait(&app, &path, &id, "done").await?;
        let (created, last_text, generated) = {
            let state = lock(&state)?;
            (
                state.created.clone(),
                state
                    .results
                    .last()
                    .map(|r| r["text"].as_str().unwrap_or_default().trim().to_owned()),
                state
                    .returned
                    .last()
                    .map(|r| r["replies"][0]["generatedFiles"][0]["id"].clone()),
            )
        };
        ensure!(
            last_text.as_deref() == Some("REQUESTED_OUTPUT"),
            "{path} read: {last_text:?}"
        );
        ensure!(
            generated == Some(json!(created)),
            "{path} returned file: {generated:?} vs {created}"
        );
        other_output = created.clone();
        let panel = path.replace("/turns", "/panel");
        let files = app.get(&format!("{panel}/files")).await?;
        ensure!(
            files
                .as_array()
                .is_some_and(|files| files.iter().any(|f| f["id"] == created.as_str())),
            "{path} panel files: {files}"
        );
        ensure!(
            app.get(&format!("{panel}/files/{created}?preview=text"))
                .await?["text"]
                .as_str()
                .map(str::trim)
                == Some("REQUESTED_OUTPUT"),
            "{path} preview"
        );
        ensure!(
            app.call(
                Method::GET,
                &format!("{panel}/files/{private_file}?preview=text"),
                None
            )
            .await?
            .status
                == 404,
            "{path} private input previewed"
        );
    }
    let path = format!("/api/agents/{owner}/turns");
    {
        let mut state = lock(&state)?;
        state.target = other_output.clone();
        state.created.clear();
    }
    let id = post_turn(&app, &path, "Read unrelated output").await?;
    wait(&app, &path, &id, "done").await?;
    ensure!(
        !lock(&state)?
            .results
            .last()
            .map_or(Value::Null, |r| r["error"].clone())
            .is_null(),
        "unrelated output read"
    );
    {
        let mut state = lock(&state)?;
        state.target = private_file.clone();
        state.created.clear();
    }
    let id = post_turn(&app, &path, "Read private input").await?;
    wait(&app, &path, &id, "done").await?;
    {
        let state = lock(&state)?;
        let last = state.results.last().cloned().unwrap_or(Value::Null);
        ensure!(!last["error"].is_null(), "private input read: {last}");
        ensure!(
            !last.to_string().contains("PRIVATE_INPUT"),
            "private input leaked"
        );
    }
    {
        let mut state = lock(&state)?;
        state.target.clear();
        state.expects_reply = false;
        state.created.clear();
    }
    let id = post_turn(&app, &path, "No reply").await?;
    wait(&app, &path, &id, "done").await?;
    ensure!(
        !lock(&state)?
            .results
            .last()
            .map_or(Value::Null, |r| r["error"].clone())
            .is_null(),
        "no-reply output read"
    );
    {
        let mut state = lock(&state)?;
        state.expects_reply = true;
        state.restart = true;
        state.created.clear();
    }
    let id = post_turn(&app, &path, "Restart").await?;
    let failed = wait(&app, &path, &id, "failed").await?;
    let (count, snapshot) = {
        let state = lock(&state)?;
        (state.creates, state.returned.last().cloned())
    };
    app.restart().await?;
    app.post(
        &format!("{path}/{id}/resume"),
        json!({"id": new_id(), "revision": failed["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    wait(&app, &path, &id, "done").await?;
    {
        let state = lock(&state)?;
        ensure!(state.creates == count, "resume recreated the file");
        ensure!(
            state.returned.last().cloned() == snapshot,
            "resume changed the returned result"
        );
        ensure!(
            state
                .results
                .last()
                .map(|r| r["text"].as_str().unwrap_or_default().trim().to_owned())
                .as_deref()
                == Some("REQUESTED_OUTPUT"),
            "resume read"
        );
    }
    {
        let mut state = lock(&state)?;
        state.open_returned = true;
        state.created.clear();
    }
    let id = post_turn(&app, &path, "Open returned file").await?;
    let opened = wait(&app, &path, &id, "done").await?;
    let created = lock(&state)?.created.clone();
    ensure!(
        opened["panelFile"]["id"] == created.as_str(),
        "opened: {}",
        opened["panelFile"]
    );
    lock(&state)?.open_returned = false;
    let source = lock(&state)?
        .returned
        .last()
        .map(|r| r["replies"][0]["generatedFiles"][0].clone())
        .unwrap_or(Value::Null);
    for _ in 0..2 {
        ensure!(
            app.call(
                Method::DELETE,
                &format!("/api/agents/{owner}/files/{created}"),
                None
            )
            .await?
            .status
                == 200,
            "delete returned file"
        );
    }
    ensure!(
        app.call(
            Method::GET,
            &format!(
                "/api/threads/{}/files/{created}",
                source["thread"].as_str().unwrap_or_default()
            ),
            None
        )
        .await?
        .status
            == 404,
        "deleted source still served"
    );
    ensure!(
        !app.get(&format!("/api/agents/{owner}/panel/files"))
            .await?
            .as_array()
            .is_some_and(|files| files.iter().any(|f| f["id"] == created.as_str())),
        "deleted file in panel"
    );
    lock(&state)?.created.clear();
    // Hold the requester's step after the teammate replied.
    gate.hold_when(|body| {
        body["model"] == "owner"
            && body["messages"]
                .as_array()
                .and_then(|m| m.last())
                .is_some_and(|m| {
                    m["role"] == "tool"
                        && m["content"]
                            .as_str()
                            .is_some_and(|c| c.contains("\"request\""))
                })
    });
    let id = post_turn(&app, &path, "Delete source").await?;
    until("held requester step", || async {
        Ok((gate.held() > 0).then_some(()))
    })
    .await?;
    gate.clear_hold();
    let parent = app.row(&path, &id).await?.ok_or("parent missing")?;
    let child = parent["agentMessages"][0].clone();
    let threads = app.get(&format!("/api/agents/{worker}/threads")).await?;
    let thread = threads
        .as_array()
        .and_then(|threads| threads.iter().find(|t| t["id"] == child["thread"]).cloned())
        .ok_or("child thread missing")?;
    if pdf {
        hold.0.lock().map_err(|_| "pdf hold poisoned")?.armed = true;
        gate.release();
        let started = minarbot_testkit::wait::until_within(
            "PDF extraction to start",
            Duration::from_secs(5),
            || async {
                Ok(hold
                    .0
                    .lock()
                    .map_err(|_| "pdf hold poisoned")?
                    .started
                    .then_some(true))
            },
        )
        .await
        .unwrap_or(false);
        ensure!(started, "PDF extraction did not start");
    }
    let thread_id = thread["id"].as_str().unwrap_or_default();
    let archived = app
        .post(
            &format!("/api/threads/{thread_id}/edit"),
            json!({"id": new_id(), "revision": thread["revision"], "name": thread["name"], "archived": true}),
        )
        .await?;
    app.post(
        &format!("/api/threads/{thread_id}/delete"),
        json!({"id": new_id(), "revision": archived["revision"]}),
    )
    .await?;
    gate.release();
    {
        let (guard, signal) = &*hold;
        if let Ok(mut state) = guard.lock() {
            state.released = true;
        }
        signal.notify_all();
    }
    wait(&app, &path, &id, "done").await?;
    let last = lock(&state)?.results.last().cloned().unwrap_or(Value::Null);
    ensure!(!last["error"].is_null(), "deleted source read: {last}");
    ensure!(
        !last.to_string().contains("REQUESTED_OUTPUT"),
        "deleted source leaked"
    );
    Ok(())
}

#[tokio::test]
async fn requester_reads_returned_teammate_text_artifacts_but_not_private_inputs_or_no_reply_outputs(
) -> Result<()> {
    requester_reads_returned_teammate_artifacts(false).await
}

// The PDF bridge holds extraction synchronously, so the test needs more
// than one runtime thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn requester_reads_returned_teammate_pdf_artifacts_but_not_private_inputs_or_no_reply_outputs(
) -> Result<()> {
    requester_reads_returned_teammate_artifacts(true).await
}
