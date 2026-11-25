//! Conversation output files, panel requests and owner deletion (port of
//! `app/rust-file-output-panel.test.ts`).

use crate::sa_support::{id, lock, offers, raw, tool_json, turn_tool_results, Shared};
use minarbot_testkit::{
    db::Inspector,
    http::Method,
    model::{Reply, ScriptedModel},
    prelude::*,
    Server,
};

#[derive(Default)]
struct Panel {
    target: String,
    invalid: bool,
    filename: String,
    /// Requests that did not offer `open_file_in_panel`.
    missing: usize,
    /// `open` results whose error presence differed from `invalid`.
    mismatched: Vec<String>,
}

async fn run(app: &Server, path: &str, text: &str, files: Option<Value>) -> Result<Value> {
    let turn = new_id();
    let mut body = json!({"id": turn, "text": text});
    if let Some(files) = files {
        body["files"] = files;
    }
    app.post(&format!("{path}/turns"), body).await?;
    let row = app
        .wait_row(&format!("{path}/turns"), &turn, |row| {
            row["status"] == "done" || row["status"] == "failed"
        })
        .await?;
    ensure!(row["status"] == "done", "{text} in {path}: {row}");
    Ok(row)
}

async fn body(app: &Server, path: &str) -> Result<(u16, String)> {
    let response = app.request(Method::GET, path)?.send().await?;
    Ok((response.status().as_u16(), response.text().await?))
}

#[tokio::test]
async fn conversation_output_updates_panel_requests_and_owner_deletion_preserve_reference_identity(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    let state: Shared<Panel> = Shared::default();
    let script = state.clone();
    model.respond(move |request| {
        let Ok(mut state) = script.lock() else {
            return Reply::status(500, json!({"error": "poisoned"}));
        };
        let text = request
            .last_user_text()
            .split('\n')
            .next()
            .unwrap_or_default()
            .to_owned();
        // The server's one-time done check after a deliverable (#87 harness turns).
        if request.last_user_text().starts_with("One verification reminder") {
            return Reply::text("Ready.");
        }
        if !offers(request, "open_file_in_panel") {
            state.missing += 1;
        }
        let results = turn_tool_results(request);
        let opening = text == "open";
        if results.len() >= if text == "twice" { 2 } else { 1 } {
            if opening {
                let failed = !tool_json(results.last().unwrap_or(&Value::Null))["error"].is_null();
                if failed != state.invalid {
                    let seen = results.last().map(Value::to_string).unwrap_or_default();
                    state.mismatched.push(seen);
                }
            }
            return Reply::text("Ready.");
        }
        if opening {
            Reply::tool_call(&new_id(), "open_file_in_panel", json!({"id": state.target}))
        } else {
            Reply::tool_call(
                &new_id(),
                "create_text_file",
                json!({"name": state.filename, "text": format!("<h1>{text}-{}</h1>", results.len())}),
            )
        }
    });
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new().model(&model)).await?;
    let agent = id(&app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Panel files", "model": "fixture",
                "instructions": "Help."}),
        )
        .await?)?;
    let room = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Panel room", "members": [agent], "lead": agent,
                "instructions": "Help."}),
        )
        .await?)?;
    let thread = id(&app
        .post(
            &format!("/api/agents/{agent}/threads"),
            json!({"id": new_id(), "name": "Panel side"}),
        )
        .await?)?;
    let scopes = [
        format!("/api/agents/{agent}"),
        format!("/api/channels/{room}"),
        format!("/api/threads/{thread}"),
    ];
    let mut removed = Vec::new();
    for path in &scopes {
        let kind = path.split('/').nth(2).unwrap_or_default().to_owned();
        lock(&state)?.filename = format!("{kind}.html");
        let first = run(&app, path, "first", None).await?;
        let target = first["generatedFiles"][0]["id"]
            .as_str()
            .ok_or_else(|| format!("no generated file: {first}"))?
            .to_owned();
        lock(&state)?.target = target.clone();
        let second = run(&app, path, "twice", None).await?;
        ensure!(
            second["generatedFiles"].as_array().map(Vec::len) == Some(1),
            "second: {second}"
        );
        ensure!(
            second["generatedFiles"][0]["id"] == target.as_str(),
            "identity changed"
        );
        ensure!(
            app.get(&format!("{path}/files"))
                .await?
                .as_array()
                .map(Vec::len)
                == Some(1),
            "files after update"
        );
        let (_, content) = body(&app, &format!("{path}/files/{target}")).await?;
        ensure!(content == "<h1>twice-1</h1>", "content: {content}");
        ensure!(
            app.get(&format!("{path}/panel/files"))
                .await?
                .as_array()
                .map(Vec::len)
                == Some(1),
            "panel files"
        );
        let opened = run(&app, path, "open", None).await?;
        ensure!(
            opened["panelFile"]["id"] == target.as_str(),
            "opened: {opened}"
        );
        ensure!(
            opened["panelFile"]["name"] == format!("{kind}.html"),
            "name: {opened}"
        );
        ensure!(
            opened["panelFile"]["seq"]
                .as_i64()
                .is_some_and(|seq| seq > 0),
            "seq: {opened}"
        );
        ensure!(
            app.get(&format!("{path}/files"))
                .await?
                .as_array()
                .map(Vec::len)
                == Some(1),
            "open created a file"
        );
        let foreign = scopes
            .iter()
            .find(|scope| *scope != path)
            .ok_or("no foreign scope")?;
        lock(&state)?.invalid = true;
        let denied = run(&app, foreign, "open", None).await?;
        ensure!(denied["panelFile"].is_null(), "foreign open: {denied}");
        lock(&state)?.invalid = false;
        run(&app, path, "reference", Some(json!([target]))).await?;
        let file = format!("{path}/files/{target}");
        for headers in [
            vec![],
            vec![("origin", "https://foreign.test")],
            vec![
                ("origin", app.origin()),
                ("authorization", "Bearer synthetic"),
            ],
        ] {
            ensure!(
                raw(&app, Method::DELETE, &file, &headers).await? == 403,
                "delete with {headers:?}"
            );
        }
        let origin = [("origin", app.origin())];
        ensure!(
            raw(
                &app,
                Method::DELETE,
                &format!("{file}?preview=text"),
                &origin
            )
            .await?
                == 403,
            "delete with query"
        );
        ensure!(
            raw(
                &app,
                Method::DELETE,
                &format!("{foreign}/files/{target}"),
                &origin
            )
            .await?
                == 404,
            "foreign delete"
        );
        ensure!(
            raw(
                &app,
                Method::DELETE,
                &file.replacen("/api/", "/api/scripts/", 1),
                &origin
            )
            .await?
                == 403,
            "script delete"
        );
        ensure!(
            raw(&app, Method::DELETE, &file, &origin).await? == 200,
            "delete"
        );
        ensure!(
            raw(&app, Method::DELETE, &file, &origin).await? == 200,
            "delete retry"
        );
        ensure!(body(&app, &file).await?.0 == 404, "deleted file served");
        ensure!(
            body(&app, &format!("{path}/panel/files/{target}?preview=html"))
                .await?
                .0
                == 404,
            "deleted panel file served"
        );
        ensure!(
            app.get(&format!("{path}/files")).await? == json!([]),
            "files listed"
        );
        ensure!(
            app.get(&format!("{path}/panel/files")).await? == json!([]),
            "panel listed"
        );
        let history = app.get(&format!("{path}/turns")).await?;
        let rows = history.as_array().cloned().unwrap_or_default();
        ensure!(
            rows.iter().any(|turn| turn["files"]
                .as_array()
                .is_some_and(|files| files.iter().any(|file| file["deleted"] == true))),
            "reference not marked deleted: {history}"
        );
        ensure!(
            rows.iter()
                .find(|turn| turn["id"] == first["id"])
                .is_some_and(|turn| turn["generatedFiles"][0]["deleted"] == true),
            "output not marked deleted"
        );
        lock(&state)?.invalid = true;
        ensure!(
            run(&app, path, "open", None).await?["panelFile"].is_null(),
            "deleted file opened"
        );
        lock(&state)?.invalid = false;
        lock(&state)?.filename = format!("{kind}-replacement.html");
        let replacement = run(&app, path, "replacement", None).await?;
        ensure!(
            replacement["generatedFiles"][0]["id"] != target.as_str(),
            "replacement reused the deleted id"
        );
        removed.push(target);
    }
    app.shutdown().await?;
    {
        let db = Inspector::open(app.database())?;
        for file in &removed {
            let tombstone = db.row(
                "SELECT deleted,length(bytes) AS size,digest FROM files WHERE id=?",
                [file],
            )?;
            ensure!(
                tombstone == json!({"deleted": 1, "size": 0, "digest": ""}),
                "tombstone {file}: {tombstone}"
            );
        }
        let opens: i64 = db.scalar(
            "SELECT count(*) FROM flow_tool_calls WHERE json_extract(call,'$.function.name')='open_file_in_panel'",
            [],
        )?;
        ensure!(opens > 0, "open_file_in_panel not journaled");
    }
    let config = app.config().clone();
    app.start_stopped(config).await?;
    ensure!(
        app.get(&format!("{}/files", scopes[0]))
            .await?
            .as_array()
            .map(Vec::len)
            == Some(1),
        "files after restart"
    );
    ensure!(
        app.get(&format!("{}/turns", scopes[0]))
            .await?
            .as_array()
            .is_some_and(|turns| turns.iter().any(|turn| turn["generatedFiles"]
                .as_array()
                .is_some_and(|files| files.iter().any(|file| file["deleted"] == true)))),
        "deleted marker lost on restart"
    );
    let state = lock(&state)?;
    ensure!(state.missing == 0, "open_file_in_panel not offered");
    ensure!(
        state.mismatched.is_empty(),
        "open results: {:?}",
        state.mismatched
    );
    Ok(())
}
