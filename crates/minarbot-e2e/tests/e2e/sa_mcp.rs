//! External MCP connections: test before save, credential and grant
//! protection, journaled success or uncertainty without replay (port of
//! `app/mcp.test.ts`).

use crate::sa_support::{
    api_agent, channel, id, lock, offers, turn_tool_results, RawAction, RawProvider, Shared,
};
use minarbot_testkit::{
    model::{Reply, ScriptedModel},
    prelude::*,
    restore, Server,
};
use std::{sync::Arc, time::Duration};
use tokio::sync::Notify;

const TOKEN: &str = "test-secret-credential";

#[derive(Default)]
struct External {
    calls: usize,
    drift: bool,
    lost: bool,
    sse: bool,
    hold: bool,
    paged: bool,
    leak_key: bool,
    bearer: String,
    initialized: usize,
    deleted: usize,
    violations: Vec<String>,
}

#[derive(Default)]
struct Model {
    connection: String,
    fail_provider: bool,
    repeat: bool,
    raw: String,
    missing_tool: usize,
}

fn json_reply(
    body: &Value,
    sse: bool,
    extra: Vec<(String, String)>,
) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let mut headers = vec![(
        "content-type".to_owned(),
        if sse {
            "text/event-stream"
        } else {
            "application/json"
        }
        .to_owned(),
    )];
    headers.extend(extra);
    let message = body.to_string();
    let bytes = if sse {
        format!("event: message\ndata: {message}\n\n")
    } else {
        message
    };
    (200, headers, bytes.into_bytes())
}

fn external_server(
    state: Shared<External>,
    held: Arc<Notify>,
    release: Arc<Notify>,
) -> crate::sa_support::RawResponder {
    Arc::new(move |request| {
        let (state, held, release) = (state.clone(), held.clone(), release.clone());
        Box::pin(async move {
            if request.path == "/redirect" {
                return RawAction::Reply((
                    307,
                    vec![("location".into(), "/mcp".into())],
                    Vec::new(),
                ));
            }
            if request.method == "GET" {
                return RawAction::Reply((405, vec![], Vec::new()));
            }
            let body = request.json();
            let (result, sse, session, hold) = {
                let Ok(mut state) = state.lock() else {
                    return RawAction::Reply((500, vec![], Vec::new()));
                };
                let bearer = format!("Bearer {}", state.bearer);
                if request.authorization.as_deref() != Some(bearer.as_str()) {
                    state
                        .violations
                        .push(format!("authorization {:?}", request.authorization));
                }
                if request.method == "DELETE" {
                    state.deleted += 1;
                    return RawAction::Reply((200, vec![], Vec::new()));
                }
                if body["id"].is_null() {
                    return RawAction::Reply((202, vec![], Vec::new()));
                }
                let mut session = None;
                let mut hold = false;
                let mut result = match body["method"].as_str() {
                    Some("initialize") => {
                        state.initialized += 1;
                        session = Some(new_id());
                        json!({"protocolVersion": "2025-03-26", "capabilities": {"tools": {}},
                            "serverInfo": {"name": "fixture", "version": "1"}})
                    }
                    Some("tools/list") => {
                        let mut properties = json!({"value": {"type": "string"}, "count": {"type": "number", "default": 1}});
                        if state.leak_key {
                            properties[state.bearer.clone()] = json!({"type": "string"});
                        }
                        let description = if state.drift {
                            "Changed tool".to_owned()
                        } else {
                            format!("A tool {}", state.bearer)
                        };
                        json!({"tools": [{"name": "change", "description": description, "inputSchema": {"type": "object",
                            "properties": properties, "required": ["value"], "additionalProperties": false}}]})
                    }
                    Some("tools/call") => {
                        state.calls += 1;
                        if body["params"]["name"] != "change" && !state.paged {
                            state
                                .violations
                                .push(format!("tool {}", body["params"]["name"]));
                        }
                        if state.lost {
                            state.lost = false;
                            return RawAction::Lose;
                        }
                        hold = state.hold;
                        json!({"content": [{"type": "text", "text": format!("Saved {} {TOKEN}",
                            body["params"]["arguments"]["value"].as_str().unwrap_or_default())}],
                            "structuredContent": {TOKEN: "masked key"}})
                    }
                    other => {
                        state.violations.push(format!("method {other:?}"));
                        Value::Null
                    }
                };
                if state.paged && body["method"] == "tools/list" {
                    result = if body["params"]["cursor"].is_string() {
                        json!({"tools": [{"name": "second", "inputSchema": {"type": "object"}}]})
                    } else {
                        json!({"tools": [{"name": "change", "inputSchema": {"type": "object"},
                            "outputSchema": {"type": "object", "properties": {"count": {"type": "number"}}, "required": ["count"]}}],
                            "nextCursor": "next"})
                    };
                }
                if state.paged && body["method"] == "tools/call" {
                    result = json!({"content": [{"type": "text", "text": "Invalid structured output"}],
                        "structuredContent": {"count": "invalid"}});
                }
                (result, state.sse, session, hold)
            };
            if hold {
                let released = release.notified();
                held.notify_one();
                released.await;
            }
            let extra = session
                .map(|id| vec![("mcp-session-id".to_owned(), id)])
                .unwrap_or_default();
            RawAction::Reply(json_reply(
                &json!({"jsonrpc": "2.0", "id": body["id"], "result": result}),
                sse,
                extra,
            ))
        })
    })
}

/// Settled turn row; the caller asserts the status.
async fn run(app: &Server, path: &str, text: &str) -> Result<Value> {
    let turn = new_id();
    app.post_expect(path, json!({"id": turn, "text": text}), 202)
        .await?;
    app.wait_row(path, &turn, |row| {
        row["status"] == "done" || row["status"] == "failed"
    })
    .await
}

/// #87 (H1): a refused MCP call reaches the harness as a tool error; the
/// scripted model answers with it and the turn finishes.
fn refused(row: &Value, needle: &str) -> bool {
    row["status"] == "done"
        && row["reply"]
            .as_str()
            .is_some_and(|reply| reply.contains(needle))
}

async fn post_secret_free(app: &Server, path: &str, body: Value, status: u16) -> Result<Value> {
    let value = app.post_expect(path, body, status).await?;
    ensure!(
        !value.to_string().contains(TOKEN),
        "{path} leaked the token: {value}"
    );
    Ok(value)
}

#[tokio::test]
async fn mcp_tests_before_save_protects_credentials_grants_and_journals_success_or_uncertainty_without_replay(
) -> Result<()> {
    let state: Shared<External> = Shared::default();
    lock(&state)?.bearer = TOKEN.into();
    let held = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let external = RawProvider::start_async(external_server(
        state.clone(),
        held.clone(),
        release.clone(),
    ))
    .await?;
    let url = format!("{}/mcp", external.origin());
    let model = ScriptedModel::start().await?;
    let script: Shared<Model> = Shared::default();
    let modeled = script.clone();
    model.respond(move |request| {
        let Ok(mut script) = modeled.lock() else {
            return Reply::status(500, json!({}));
        };
        script.raw.push_str(&request.body.to_string());
        if !offers(request, "call_mcp_tool") {
            script.missing_tool += 1;
        }
        let result = turn_tool_results(request).last().cloned();
        if result.is_some() && script.fail_provider {
            script.fail_provider = false;
            return Reply::status(503, json!({}));
        }
        let prompt = request.last_user_text();
        if result.is_none() || script.repeat {
            return if prompt == "list" {
                Reply::tool_call(&new_id(), "list_mcp_tools", json!({}))
            } else {
                Reply::tool_call(
                    &new_id(),
                    "call_mcp_tool",
                    json!({"connection": script.connection, "name": "change", "arguments": {"value": "one"}}),
                )
            };
        }
        Reply::text(result.and_then(|r| r["content"].as_str().map(str::to_owned)).unwrap_or_default())
    });
    let calls =
        |state: &Shared<External>| lock(state).map(|state| state.calls).unwrap_or(usize::MAX);
    let root = TestRoot::new()?;
    let options = ServerConfig::new().model(&model);
    let mut app = root.start(options.clone()).await?;
    let a = id(&api_agent(&app, "A").await?)?;
    let b = id(&api_agent(&app, "B").await?)?;
    let (ap, bp) = (
        format!("/api/agents/{a}/turns"),
        format!("/api/agents/{b}/turns"),
    );
    post_secret_free(
        &app,
        "/api/mcp",
        json!({"id": new_id(), "test": new_id()}),
        409,
    )
    .await?;
    for bad in [
        "http://example.com/mcp".to_owned(),
        format!("{url}?token=secret"),
        "file:///etc/passwd".to_owned(),
        url.replacen("127.0.0.1", "user:pass@127.0.0.1", 1),
    ] {
        post_secret_free(
            &app,
            "/api/mcp/test",
            json!({"id": new_id(), "name": "Bad", "url": bad, "token": TOKEN}),
            400,
        )
        .await?;
    }
    post_secret_free(
        &app,
        "/api/mcp/test",
        json!({"id": new_id(), "name": "Redirect", "url": url.replace("/mcp", "/redirect"), "token": TOKEN}),
        502,
    )
    .await?;
    for bearer in ["name", "1"] {
        lock(&state)?.bearer = bearer.into();
        let short = post_secret_free(
            &app,
            "/api/mcp/test",
            json!({"id": new_id(), "name": "Short token", "url": url, "token": bearer}),
            200,
        )
        .await?;
        ensure!(
            short["tools"][0]["name"] == "change",
            "short token tools: {short}"
        );
        ensure!(
            short["tools"][0]["inputSchema"]["properties"]["count"]["default"] == 1,
            "schema default: {short}"
        );
    }
    lock(&state)?.bearer = "object".into();
    post_secret_free(
        &app,
        "/api/mcp/test",
        json!({"id": new_id(), "name": "Collision", "url": url, "token": "object"}),
        400,
    )
    .await?;
    {
        let mut state = lock(&state)?;
        state.bearer = TOKEN.into();
        state.leak_key = true;
    }
    post_secret_free(
        &app,
        "/api/mcp/test",
        json!({"id": new_id(), "name": "Leaked key", "url": url, "token": TOKEN}),
        400,
    )
    .await?;
    lock(&state)?.leak_key = false;
    {
        let state = lock(&state)?;
        ensure!(
            state.initialized == state.deleted,
            "sessions {} opened, {} closed",
            state.initialized,
            state.deleted
        );
    }
    let test_command = json!({"id": new_id(), "name": "Fixture", "url": url, "token": TOKEN});
    let tested = post_secret_free(&app, "/api/mcp/test", test_command.clone(), 200).await?;
    ensure!(
        tested["tools"][0]["description"] == "A tool [REDACTED]",
        "redaction: {tested}"
    );
    ensure!(
        post_secret_free(&app, "/api/mcp/test", test_command, 200).await? == tested,
        "test replay"
    );
    let save = json!({"id": new_id(), "test": tested["id"]});
    let connection = save["id"].as_str().unwrap_or_default().to_owned();
    lock(&script)?.connection = connection.clone();
    let saved = post_secret_free(&app, "/api/mcp", save.clone(), 200).await?;
    ensure!(saved["hasToken"] == true, "saved: {saved}");
    ensure!(
        post_secret_free(&app, "/api/mcp", save, 200).await? == saved,
        "save replay"
    );
    post_secret_free(
        &app,
        &format!("/api/mcp/{connection}"),
        json!({"id": new_id(), "revision": 1, "enabled": true, "url": format!("{url}/other")}),
        400,
    )
    .await?;
    ensure!(calls(&state) == 0, "calls before grant");
    let ungranted = run(&app, &ap, "call").await?;
    ensure!(refused(&ungranted, "error"), "ungranted call: {ungranted}");
    ensure!(calls(&state) == 0, "ungranted call reached the server");
    let gp = format!("/api/agents/{a}/mcp/{connection}");
    let grant =
        json!({"id": new_id(), "revision": 0, "connectionRevision": 1, "tools": ["change"]});
    post_secret_free(&app, &gp, grant.clone(), 200).await?;
    ensure!(
        post_secret_free(&app, &gp, grant.clone(), 200).await?
            == json!({"revision": 1, "tools": ["change"]}),
        "grant replay"
    );
    let mut other = grant.clone();
    other["id"] = json!(new_id());
    post_secret_free(&app, &gp, other, 409).await?;
    let listed = run(&app, &ap, "list").await?;
    let reply: Value = serde_json::from_str(listed["reply"].as_str().unwrap_or("null"))?;
    ensure!(
        reply["connections"][0]["tools"][0]["name"] == "change",
        "list: {listed}"
    );
    let foreign = run(&app, &bp, "call").await?;
    ensure!(refused(&foreign, "error"), "other agent call: {foreign}");
    ensure!(calls(&state) == 0, "other agent reached the server");
    let room = id(&channel(&app, "Channel", &a, &[&a]).await?)?;
    let in_room = run(&app, &format!("/api/channels/{room}/turns"), "channel").await?;
    ensure!(refused(&in_room, "error"), "channel call: {in_room}");
    ensure!(calls(&state) == 0, "channel reached the server");
    lock(&state)?.sse = true;
    lock(&script)?.fail_provider = true;
    let failed = run(&app, &ap, "call").await?;
    ensure!(failed["status"] == "failed", "provider failure: {failed}");
    ensure!(calls(&state) == 1, "calls: {}", calls(&state));
    app.shutdown().await?;
    let database = std::fs::read(app.database())?;
    ensure!(
        !database
            .windows(TOKEN.len())
            .any(|window| window == TOKEN.as_bytes()),
        "token stored in plain text"
    );
    app.start_stopped(options.clone()).await?;
    app.post(
        &format!("{ap}/{}/resume", failed["id"].as_str().unwrap_or_default()),
        json!({"id": new_id(), "revision": failed["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    let done = app
        .wait_status(&ap, failed["id"].as_str().unwrap_or_default(), "done")
        .await?;
    ensure!(
        done["reply"]
            .as_str()
            .is_some_and(|reply| reply.contains("Saved one")),
        "resumed: {done}"
    );
    ensure!(calls(&state) == 1, "resume replayed the call");
    lock(&state)?.drift = true;
    let drifted = run(&app, &ap, "call").await?;
    ensure!(refused(&drifted, "error"), "drifted call: {drifted}");
    ensure!(calls(&state) == 1, "drifted tool called");
    lock(&state)?.drift = false;
    lock(&state)?.lost = true;
    let uncertain = run(&app, &ap, "call").await?;
    ensure!(uncertain["status"] == "failed", "lost call: {uncertain}");
    ensure!(calls(&state) == 2, "lost call count");
    ensure!(
        uncertain["error"]
            .as_str()
            .is_some_and(|error| error.contains("uncertain")),
        "lost call error: {uncertain}"
    );
    app.restart().await?;
    lock(&script)?.repeat = true;
    app.post(
        &format!(
            "{ap}/{}/resume",
            uncertain["id"].as_str().unwrap_or_default()
        ),
        json!({"id": new_id(), "revision": uncertain["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    app.wait_status(&ap, uncertain["id"].as_str().unwrap_or_default(), "failed")
        .await?;
    ensure!(calls(&state) == 2, "uncertain call replayed");
    lock(&script)?.repeat = false;
    lock(&state)?.hold = true;
    let waiting = held.notified();
    let active = new_id();
    app.post_expect(&ap, json!({"id": active, "text": "call"}), 202)
        .await?;
    tokio::time::timeout(Duration::from_secs(30), waiting).await?;
    post_secret_free(
        &app,
        &gp,
        json!({"id": new_id(), "revision": 1, "connectionRevision": 1, "tools": []}),
        200,
    )
    .await?;
    app.wait_status(&ap, &active, "failed").await?;
    release.notify_one();
    lock(&state)?.hold = false;
    let revoked = run(&app, &ap, "call").await?;
    ensure!(refused(&revoked, "error"), "revoked call: {revoked}");
    ensure!(calls(&state) == 3, "calls after revoke: {}", calls(&state));
    ensure!(
        !lock(&script)?.raw.contains(TOKEN),
        "token sent to the model"
    );
    ensure!(
        !app.get(&format!("/api/mcp/{connection}?agent={a}"))
            .await?
            .to_string()
            .contains(TOKEN),
        "token in connection view"
    );
    let backup = app
        .request(minarbot_testkit::http::Method::POST, "/api/backup")?
        .header("content-type", "application/json")
        .body(json!({"id": new_id(), "confirmed": true}).to_string())
        .send()
        .await?;
    ensure!(backup.status().is_success(), "backup: {}", backup.status());
    let bytes = backup.bytes().await?;
    let backup_path = root.path().join("backup.sqlite");
    std::fs::write(&backup_path, &bytes)?;
    app.shutdown().await?;
    let restored_root = TestRoot::new()?;
    std::fs::create_dir_all(restored_root.path().join("data"))?;
    let restored = restore::run(
        restored_root.path(),
        &[
            "workspace",
            &backup_path.to_string_lossy(),
            "--db",
            &restored_root.database().to_string_lossy(),
        ],
        &[],
        None,
    )?;
    ensure!(restored["exitCode"] == 0, "restore: {restored}");
    let app = restored_root.start(options).await?;
    ensure!(
        app.get("/api/mcp").await?["items"][0]["enabled"] == 0,
        "restored connection enabled"
    );
    post_secret_free(
        &app,
        &format!("/api/mcp/{connection}"),
        json!({"id": new_id(), "revision": 2, "enabled": true}),
        200,
    )
    .await?;
    post_secret_free(
        &app,
        &gp,
        json!({"id": new_id(), "revision": 2, "connectionRevision": 3, "tools": ["change"]}),
        200,
    )
    .await?;
    let missing = run(&app, &ap, "call").await?;
    ensure!(refused(&missing, "host key"), "missing host key: {missing}");
    ensure!(calls(&state) == 3, "restored call reached the server");
    lock(&state)?.paged = true;
    let pages = post_secret_free(
        &app,
        "/api/mcp/test",
        json!({"id": new_id(), "name": "Paged", "url": url, "token": TOKEN}),
        200,
    )
    .await?;
    ensure!(
        pages["tools"].as_array().map(Vec::len) == Some(2),
        "paged tools: {pages}"
    );
    let saved_pages = post_secret_free(
        &app,
        "/api/mcp",
        json!({"id": new_id(), "test": pages["id"]}),
        200,
    )
    .await?;
    let paged_connection = id(&saved_pages)?;
    lock(&script)?.connection = paged_connection.clone();
    post_secret_free(
        &app,
        &format!("/api/agents/{a}/mcp/{paged_connection}"),
        json!({"id": new_id(), "revision": 0, "connectionRevision": 1, "tools": ["change"]}),
        200,
    )
    .await?;
    let invalid = run(&app, &ap, "call").await?;
    ensure!(invalid["status"] == "failed", "invalid output: {invalid}");
    ensure!(
        invalid["error"]
            .as_str()
            .is_some_and(|error| error.contains("uncertain")),
        "invalid output error: {invalid}"
    );
    ensure!(calls(&state) == 4, "paged call count");
    ensure!(
        !lock(&script)?.raw.contains("Invalid structured output"),
        "invalid output reached the model"
    );
    let state = lock(&state)?;
    ensure!(
        state.violations.is_empty(),
        "MCP server checks: {:?}",
        state.violations
    );
    ensure!(
        lock(&script)?.missing_tool == 0,
        "call_mcp_tool not offered"
    );
    Ok(())
}
