//! Turn input and workspace tools: attachment-only messages in direct chats,
//! side threads and rooms, and workspace file tools bound to the agent's
//! folder (ports of `app/attachment-only.test.ts` and the tool cases of
//! `app/workspace-files.test.ts`, #89 triage).

use minarbot_testkit::{
    http::Method,
    model::{Reply, ScriptedModel},
    prelude::*,
    protocol::sha256_hex,
    Server,
};
use std::{
    fs,
    sync::{Arc, Mutex},
};

fn id(value: &Value) -> Result<String> {
    Ok(value["id"].as_str().ok_or("missing id")?.to_owned())
}

async fn upload(app: &Server, scope: &str, name: &str, bytes: Vec<u8>) -> Result<String> {
    let file = new_id();
    let status = app
        .request(Method::POST, &format!("{scope}/files/{file}"))?
        .header("content-type", "application/octet-stream")
        .header("x-file-name", name)
        .body(bytes)
        .send()
        .await?
        .status();
    ensure!(status == 200, "upload {name} to {scope}: {status}");
    Ok(file)
}

#[tokio::test]
async fn attachment_only_messages_validate_recover_and_retry_in_direct_chats_threads_and_rooms(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|_| Reply::text("Attachment received"));
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new().model(&model)).await?;
    let agent = id(&app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Files", "model": "fixture",
                "instructions": "Synthetic"}),
        )
        .await?)?;
    let thread = id(&app
        .post(
            &format!("/api/agents/{agent}/threads"),
            json!({"id": new_id(), "name": "Side"}),
        )
        .await?)?;
    let room = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Room", "members": [agent], "lead": agent,
                "instructions": "Synthetic"}),
        )
        .await?)?;
    for scope in [
        format!("/api/agents/{agent}"),
        format!("/api/threads/{thread}"),
        format!("/api/channels/{room}"),
    ] {
        let turns = format!("{scope}/turns");
        let file = upload(&app, &scope, "notes.txt", b"Synthetic attachment".to_vec()).await?;
        let command = json!({"id": new_id(), "text": "", "files": [file]});
        let accepted = app.post(&turns, command.clone()).await?;
        ensure!(accepted["text"] == "", "{scope} accepted: {accepted}");
        let retried = app.post(&turns, command.clone()).await?;
        ensure!(
            retried["id"] == command["id"],
            "{scope} retry: {retried} for {command}"
        );
        let mut changed = command.clone();
        changed["files"] = json!([]);
        app.post_expect(&turns, changed, 400).await?;
        app.post_expect(
            &turns,
            json!({"id": new_id(), "text": "", "files": []}),
            400,
        )
        .await?;
        app.post_expect(
            &turns,
            json!({"id": new_id(), "text": "", "files": [new_id()]}),
            404,
        )
        .await?;
        app.post_expect(
            &turns,
            json!({"id": new_id(), "text": " ".repeat(16001), "files": [file]}),
            400,
        )
        .await?;
        let done = app.wait_status(&turns, &id(&command)?, "done").await?;
        ensure!(
            done["files"][0]["id"] == file.as_str(),
            "{scope} files: {done}"
        );
    }
    let scope = format!("/api/agents/{agent}");
    let turns = format!("{scope}/turns");
    let audio = [b"RIFF\0\0\0\0WAVE".as_slice(), b"PRIVATE_AUDIO_BYTES"].concat();
    let file = upload(&app, &scope, "Voice note 0:04.wav", audio.clone()).await?;
    let command = json!({"id": new_id(), "text": "", "files": [file]});
    app.post(&turns, command.clone()).await?;
    ensure!(
        app.post(&turns, command.clone()).await?["id"] == command["id"],
        "audio retry identity"
    );
    let done = app.wait_status(&turns, &id(&command)?, "done").await?;
    ensure!(
        done["files"][0]["name"] == "Voice note 0:04.wav",
        "audio files: {done}"
    );
    let mut large = Vec::new();
    for _ in 0..2 {
        large.push(
            upload(
                &app,
                &scope,
                "large.wav",
                [audio.clone(), vec![0; 6 * 1024 * 1024]].concat(),
            )
            .await?,
        );
    }
    app.post_expect(
        &turns,
        json!({"id": new_id(), "text": "", "files": large}),
        413,
    )
    .await?;
    let requests = model.requests();
    ensure!(!requests.is_empty(), "no provider call");
    for request in requests {
        let raw = request.body.to_string();
        ensure!(
            raw.contains("Synthetic attachment")
                || raw.contains("Its contents have not been transcribed"),
            "request {} lacks the attachment: {raw}",
            request.index
        );
        ensure!(
            !raw.contains("PRIVATE_AUDIO_BYTES"),
            "request {} carries raw audio bytes",
            request.index
        );
    }
    Ok(())
}

#[derive(Default)]
struct Advertised(bool);

/// Workspace file tools of a bound agent reach its folder from the direct
/// chat, side threads and rooms it leads; unbound agents and rooms get the
/// managed folder, and so does the agent once its binding is removed.
async fn workspace_tools(metadata: bool) -> Result<()> {
    let root = TestRoot::new()?;
    let folder = root.dir("root")?;
    let binary = vec![0u8, 255, 128, 42];
    let expected = if metadata {
        fs::write(folder.join("note.txt"), &binary)?;
        json!({"path": "note.txt", "size": binary.len(), "digest": sha256_hex(&binary)}).to_string()
    } else {
        fs::write(folder.join("note.txt"), "OWN_WORKSPACE")?;
        "OWN_WORKSPACE".to_owned()
    };
    let tool = if metadata {
        "inspect_workspace_file"
    } else {
        "read_workspace_file"
    };
    let advertised = Arc::new(Mutex::new(Advertised::default()));
    let model = ScriptedModel::start().await?;
    let seen = advertised.clone();
    model.respond(move |request| {
        if let Ok(mut seen) = seen.lock() {
            seen.0 = request.tool_names().iter().any(|name| name == tool);
        }
        let Some(result) = request.tool_results().last().cloned() else {
            return Reply::tool_call("read", tool, json!({"path": "note.txt"}));
        };
        let content = result["content"].as_str().unwrap_or_default().to_owned();
        let parsed: Value = serde_json::from_str(&content).unwrap_or(Value::Null);
        Reply::text(if !parsed["error"].is_null() {
            "Workspace file unavailable.".to_owned()
        } else if metadata {
            content
        } else {
            parsed["text"].as_str().unwrap_or_default().to_owned()
        })
    });
    let was_advertised = || -> Result<bool> { Ok(advertised.lock().map_err(|_| "poisoned")?.0) };
    let (agent, other) = (new_id(), new_id());
    let bound = ServerConfig::new()
        .model(&model)
        .workspace(&agent, &folder, false);
    let mut app = root.start(bound).await?;
    for (id, name) in [(&agent, "Bound"), (&other, "Unbound")] {
        app.post(
            "/api/agents",
            json!({"runtime": "api", "id": id, "name": name, "model": "test", "instructions": "Help"}),
        )
        .await?;
    }
    async fn send(app: &Server, turns: &str) -> Result<Value> {
        let turn = new_id();
        app.post(turns, json!({"id": turn, "text": "Read workspace"}))
            .await?;
        app.wait_row(turns, &turn, |row| {
            row["status"] == "done" || row["status"] == "failed"
        })
        .await
    }
    let direct = format!("/api/agents/{agent}/turns");
    let reply = send(&app, &direct).await?;
    ensure!(reply["reply"] == expected.as_str(), "direct: {reply}");
    ensure!(was_advertised()?, "{tool} not offered in the direct chat");
    let side = id(&app
        .post(
            &format!("/api/agents/{agent}/threads"),
            json!({"id": new_id(), "name": "Side"}),
        )
        .await?)?;
    let reply = send(&app, &format!("/api/threads/{side}/turns")).await?;
    ensure!(reply["reply"] == expected.as_str(), "side thread: {reply}");
    let reply = send(&app, &format!("/api/agents/{other}/turns")).await?;
    ensure!(
        reply["reply"] == "Workspace file unavailable.",
        "unbound agent: {reply}"
    );
    ensure!(was_advertised()?, "{tool} not offered to the unbound agent");
    let team = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Team", "instructions": "Help", "members": [agent],
                "lead": agent}),
        )
        .await?)?;
    let reply = send(&app, &format!("/api/channels/{team}/turns")).await?;
    ensure!(
        reply["status"] == "done" && reply["reply"] == expected.as_str(),
        "room led by the bound agent: {reply}"
    );
    ensure!(was_advertised()?, "{tool} not offered in the room");
    let isolated = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Isolated", "instructions": "Help", "members": [other],
                "lead": other}),
        )
        .await?)?;
    let reply = send(&app, &format!("/api/channels/{isolated}/turns")).await?;
    ensure!(
        reply["status"] == "done" && reply["reply"] == "Workspace file unavailable.",
        "room led by the unbound agent: {reply}"
    );
    app.restart().await?;
    let reply = send(&app, &direct).await?;
    ensure!(
        reply["reply"] == expected.as_str(),
        "direct after restart: {reply}"
    );
    app.restart_with(ServerConfig::new().model(&model)).await?;
    let reply = send(&app, &direct).await?;
    ensure!(
        reply["reply"] == "Workspace file unavailable.",
        "direct after the binding was removed: {reply}"
    );
    ensure!(
        was_advertised()?,
        "{tool} not offered after the binding was removed"
    );
    Ok(())
}

#[tokio::test]
async fn workspace_read_tool_shares_agent_direct_and_side_folder_and_isolates_managed_folders_and_rooms_across_restart(
) -> Result<()> {
    workspace_tools(false).await
}

#[tokio::test]
async fn workspace_inspect_tool_shares_agent_direct_and_side_folder_and_isolates_managed_folders_and_rooms_across_restart(
) -> Result<()> {
    workspace_tools(true).await
}
