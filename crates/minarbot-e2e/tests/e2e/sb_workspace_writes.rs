//! Workspace write grants, recovery journal and malformed input (port of
//! `app/rust-workspace-writes-http.test.ts`).
//!
//! A refused write (revoked skill, read-only binding) reaches a
//! harness turn as an MCP tool error and the turn finishes; the Node direct
//! client failed it. Those steps assert that the tool was not offered or
//! returned an error, and that the workspace did not change.

use crate::sb_support::{after_tool, id, reminder, settle};
use minarbot_testkit::{
    control::{Answer, ControlModel},
    db::Fixture,
    model::ModelRequest,
    prelude::*,
    protocol::sha256_hex,
    server::server_binary,
    Server,
};
use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

/// Text of the last tool result in a request.
fn last_tool_text(request: &ModelRequest) -> String {
    request
        .tool_results()
        .last()
        .and_then(|message| message["content"].as_str())
        .unwrap_or_default()
        .to_owned()
}

/// `minarbot-server restore workspace <source> --db <target>`, as the Node
/// `restoreWorkspace` helper ran it; the JSON `exitCode` must be 0.
fn restore(source: &Path, target: &Path) -> Result<()> {
    let output = std::process::Command::new(server_binary(None)?)
        .arg("restore")
        .arg("workspace")
        .arg(source)
        .arg("--db")
        .arg(target)
        .output()?;
    let value: Value = serde_json::from_slice(&output.stdout)?;
    ensure!(
        value["exitCode"] == 0 && output.status.success(),
        "restore failed: {value} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// Workspace backup of `database` (the Node `workspaceSnapshot`).
fn snapshot(database: &Path, file: &Path) -> Result<()> {
    let db = Fixture::open(database)?;
    db.execute("VACUUM INTO ?", [file.to_string_lossy()])?;
    drop(db);
    let backup = Fixture::open(file)?;
    backup.execute_batch(
        "CREATE TABLE IF NOT EXISTS minarbot_backup (format TEXT NOT NULL,version INTEGER NOT NULL,created TEXT NOT NULL) STRICT;
         DELETE FROM minarbot_backup;
         INSERT INTO minarbot_backup VALUES ('minarbot-workspace',1,'2026-11-18T11:06:40.000Z');",
    )?;
    Ok(())
}

/// Workspace entries, without the fake gateway's own protocol log and
/// session files (written to its working directory).
fn entries(directory: &Path) -> Result<Vec<String>> {
    let mut names: Vec<String> = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|name| !name.starts_with("fake-gateway") && !name.starts_with(".fake-gateway"))
        .collect();
    names.sort();
    Ok(names)
}

#[tokio::test]
async fn restored_incomplete_workspace_intent_resumes_with_uncertainty_without_replaying_the_write(
) -> Result<()> {
    let model = ControlModel::start().await?;
    model.respond(|_| Answer::text("Inspect previous write before continuing."));
    let root = TestRoot::new()?;
    let old = root.dir("old")?;
    let fresh = root.dir("fresh")?;
    let agent = new_id();
    let turn = new_id();
    let config = ServerConfig::new()
        .gateway(model.gateway())
        .workspace(&agent, &old, true);
    let mut app = root.start(config.clone()).await?;
    app.post(
        "/api/agents",
        json!({"runtime": "api", "id": agent, "name": "Restore writer", "model": "fixture", "instructions": "Help"}),
    )
    .await?;
    let path = format!("/api/agents/{agent}/turns");
    app.post(&path, json!({"id": turn, "text": "Create file"}))
        .await?;
    app.wait_status(&path, &turn, "done").await?;
    app.shutdown().await?;
    let before = model.calls();
    let call = json!({"id": "unfinished-write", "type": "function", "function": {"name": "create_workspace_file",
        "arguments": json!({"path": "result.txt", "text": "Original bytes"}).to_string()}});
    {
        let db = Fixture::open(root.database())?;
        // The crash boundary: the intent is durable, its result never saved.
        db.execute(
            "UPDATE turns SET status='running',reply='',paused=0 WHERE id=?",
            [&turn],
        )?;
        db.execute(
            "INSERT INTO workspace_writes VALUES (?,?,?,?,?)",
            (&turn, "unfinished-write", call.to_string(), 0, 0),
        )?;
    }
    fs::write(old.join("result.txt"), "Original bytes")?;
    let backup = root.path().join("backup.sqlite");
    snapshot(&root.database(), &backup)?;
    let restored = root.path().join("restored.sqlite");
    restore(&backup, &restored)?;
    // The kit always starts on the root database, so the restored database
    // replaces it (the Node case started a second server on the copy).
    let database = root.database();
    for suffix in ["-wal", "-shm"] {
        let side = database.with_file_name(format!(
            "{}{suffix}",
            database
                .file_name()
                .ok_or("database name")?
                .to_string_lossy()
        ));
        if side.exists() {
            fs::remove_file(side)?;
        }
    }
    fs::copy(&restored, &database)?;
    let restored_config = ServerConfig::new()
        .gateway(model.gateway())
        .workspace(&agent, &fresh, true);
    for _ in 0..2 {
        app.start_stopped(restored_config.clone()).await?;
        let row = app.get(&path).await?[0].clone();
        ensure!(
            row["status"] == "interrupted" && row["paused"] == 1,
            "restored turn: {row}"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
        ensure!(model.calls() == before, "restore replayed the turn");
        ensure!(entries(&fresh)?.is_empty(), "restore replayed the write");
        app.shutdown().await?;
    }
    app.start_stopped(restored_config.clone()).await?;
    let row = app.get(&path).await?[0].clone();
    app.post(
        &format!("{path}/{turn}/resume"),
        json!({"id": new_id(), "revision": row["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    app.wait_status(&path, &turn, "done").await?;
    ensure!(
        model.calls() == before + 1,
        "resume calls: {}",
        model.calls() - before
    );
    let resumed = &model.requests()[before];
    let results: Vec<Value> = resumed
        .tool_results()
        .into_iter()
        .filter(|message| {
            message["content"]
                .as_str()
                .is_some_and(|content| content.contains("uncertain"))
        })
        .collect();
    ensure!(
        results.len() == 1,
        "uncertain results: {:?}",
        resumed.tool_results()
    );
    let result: Value = serde_json::from_str(results[0]["content"].as_str().unwrap_or_default())?;
    ensure!(result["uncertain"] == true, "result: {result}");
    ensure!(
        entries(&fresh)?.is_empty(),
        "resume wrote to the restored workspace"
    );
    ensure!(
        fs::read_to_string(old.join("result.txt"))? == "Original bytes",
        "original bytes changed"
    );
    app.shutdown().await?;
    let saved = Fixture::open(&database)?;
    let journaled: i64 = saved.scalar(
        "SELECT count(*) FROM flow_tool_calls WHERE turn=? AND call_id='unfinished-write'",
        [&turn],
    )?;
    ensure!(journaled == 1, "journaled results: {journaled}");
    Ok(())
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    File,
    Directory,
    Skill,
    Delete,
    Restore,
    Replace,
}

#[derive(Default)]
struct Script {
    tool: String,
    args: Value,
    advertised: bool,
    requests: usize,
}

fn recovery_dirs(root: &Path) -> Result<Vec<String>> {
    Ok(entries(root)?
        .into_iter()
        .filter(|name| name.starts_with(".minarbot-removed-"))
        .collect())
}

fn check_effect(mode: Mode, root: &Path, recovery: &Path) -> Result<()> {
    if mode == Mode::Replace {
        let dirs = recovery_dirs(root)?;
        ensure!(dirs.len() == 1, "replace recovery: {dirs:?}");
        ensure!(
            fs::read_to_string(root.join(&dirs[0]).join("file"))? == "OLD",
            "replaced bytes kept"
        );
    }
    if mode == Mode::Restore {
        ensure!(
            fs::read_to_string(root.join(recovery))? == "RESULT",
            "recovery copy"
        );
    }
    match mode {
        Mode::Delete => {
            ensure!(!root.join("made.txt").exists(), "file not removed");
            let dirs = recovery_dirs(root)?;
            ensure!(dirs.len() == 1, "delete recovery: {dirs:?}");
            ensure!(
                fs::read_to_string(root.join(&dirs[0]).join("file"))? == "RESULT",
                "removed bytes kept"
            );
        }
        Mode::Directory => ensure!(root.join("made").is_dir(), "directory"),
        Mode::Skill => ensure!(
            fs::read(root.join("made.txt"))? == [0u8, 255, 128, 1],
            "skill bytes"
        ),
        _ => ensure!(
            fs::read_to_string(root.join("made.txt"))? == "RESULT",
            "file bytes"
        ),
    }
    Ok(())
}

async fn settle_row(app: &Server, path: &str, command: Value) -> Result<Value> {
    let id = command["id"].as_str().ok_or("id")?.to_owned();
    app.post_expect(path, command, 202).await?;
    settle(app, path, &id).await
}

async fn write_grant(mode: Mode) -> Result<()> {
    let skill = new_id();
    let root_dir = TempWorkspace::new()?;
    let root = root_dir.path().to_owned();
    if mode == Mode::Replace {
        fs::write(root.join("made.txt"), "OLD")?;
    }
    if mode == Mode::Delete {
        fs::write(root.join("made.txt"), "RESULT")?;
    }
    let input = [vec!["d".repeat(250); 8], vec!["e".repeat(38), "f".into()]]
        .concat()
        .join("/");
    let parent = Path::new(&input).parent().ok_or("parent")?.to_owned();
    let recovery = parent
        .join(format!(".minarbot-removed-{}", new_id()))
        .join("file");
    if mode == Mode::Restore {
        ensure!(input.len() == 2048, "recovery input length {}", input.len());
        fs::create_dir_all(root.join(&parent))?;
        fs::write(root.join(&input), "RESULT")?;
        ensure!(
            recovery.to_string_lossy().len() > 2048,
            "recovery path length"
        );
        fs::create_dir_all(root.join(recovery.parent().ok_or("recovery parent")?))?;
        fs::rename(root.join(&input), root.join(&recovery))?;
    }
    let (tool, args) = match mode {
        Mode::Replace => (
            "replace_workspace_file",
            json!({"path": "made.txt", "digest": sha256_hex("OLD"), "text": "RESULT"}),
        ),
        Mode::Restore => (
            "restore_workspace_file",
            json!({"path": "made.txt", "recoveryPath": recovery, "digest": sha256_hex("RESULT")}),
        ),
        Mode::Delete => (
            "delete_workspace_file",
            json!({"path": "made.txt", "digest": sha256_hex("RESULT")}),
        ),
        Mode::Directory => ("create_workspace_directory", json!({"path": "made"})),
        Mode::Skill => (
            "copy_skill_file",
            json!({"id": skill, "source": "assets/raw.bin", "path": "made.txt"}),
        ),
        Mode::File => (
            "create_workspace_file",
            json!({"path": "made.txt", "text": "RESULT"}),
        ),
    };
    let script = Arc::new(Mutex::new(Script {
        tool: tool.into(),
        args,
        ..Script::default()
    }));
    let model = ControlModel::start().await?;
    let shared = script.clone();
    model.respond(move |request| {
        let Ok(mut script) = shared.lock() else {
            return Answer::Status(500, json!({"error": "script"}));
        };
        script.requests += 1;
        script.advertised = request.tool_names().iter().any(|name| *name == script.tool);
        if after_tool(request) || reminder(request) {
            return Answer::text(last_tool_text(request));
        }
        Answer::Tool {
            id: "create".into(),
            name: script.tool.clone(),
            arguments: script.args.clone(),
        }
    });
    let advertised = |script: &Arc<Mutex<Script>>| -> Result<bool> {
        Ok(script.lock().map_err(|_| "script")?.advertised)
    };
    let agent = new_id();
    let state = TestRoot::new()?;
    let config = ServerConfig::new()
        .gateway(model.gateway())
        .workspace(&agent, &root, true);
    let mut app = state.start(config.clone()).await?;
    app.post(
        "/api/agents",
        json!({"runtime": "api", "id": agent, "name": "Writer", "model": "test", "instructions": "Help"}),
    )
    .await?;
    if mode == Mode::Skill {
        app.post(
            "/api/skills",
            json!({"id": skill, "revision": 0, "files": [
                {"path": "SKILL.md", "content": "---\nname: copying\ndescription: Copy fixtures\n---\nCopy assets on request."},
                {"path": "assets/raw.bin", "content": "AP+AAQ==", "encoding": "base64"}]}),
        )
        .await?;
        app.post(
            &format!("/api/agents/{agent}/skills/{skill}"),
            json!({"id": new_id(), "revision": 0, "version": 1, "enabled": true}),
        )
        .await?;
        app.post(
            &format!("/api/skills/{skill}"),
            json!({"id": new_id(), "revision": 1, "files": [
                {"path": "SKILL.md", "content": "---\nname: copying\ndescription: New version\n---\nNew assets."},
                {"path": "assets/raw.bin", "content": "NEW"}]}),
        )
        .await?;
    }
    let path = format!("/api/agents/{agent}/turns");
    let command = json!({"id": new_id(), "text": "Create"});
    let row = settle_row(&app, &path, command.clone()).await?;
    ensure!(row["status"] == "done", "first write: {row}");
    ensure!(advertised(&script)?, "{tool} not offered");
    check_effect(mode, &root, &recovery)?;
    let count = model.calls();
    app.restart().await?;
    app.post_expect(&path, command, 202).await?;
    ensure!(model.calls() == count, "receipt replayed the write");
    let refused = if mode == Mode::Delete {
        "Removal did not confirm"
    } else {
        "never replaced"
    };
    let duplicate =
        settle_row(&app, &path, json!({"id": new_id(), "text": "Create again"})).await?;
    ensure!(
        duplicate["reply"]
            .as_str()
            .is_some_and(|reply| reply.contains(refused)),
        "duplicate write: {duplicate}"
    );
    check_effect(mode, &root, &recovery)?;
    let side = id(&app
        .post(
            &format!("/api/agents/{agent}/threads"),
            json!({"id": new_id(), "name": "Side"}),
        )
        .await?)?;
    let side_row = settle_row(
        &app,
        &format!("/api/threads/{side}/turns"),
        json!({"id": new_id(), "text": "Side"}),
    )
    .await?;
    ensure!(
        side_row["reply"]
            .as_str()
            .is_some_and(|reply| reply.contains(refused)),
        "side write: {side_row}"
    );
    ensure!(advertised(&script)?, "side thread lost the tool");
    if mode == Mode::Skill {
        app.post(
            &format!("/api/agents/{agent}/skills/{skill}"),
            json!({"id": new_id(), "revision": 1, "version": 1, "enabled": false}),
        )
        .await?;
        let revoked = settle_row(
            &app,
            &path,
            json!({"id": new_id(), "text": "Copy disabled skill"}),
        )
        .await?;
        ensure!(
            revoked["status"] == "failed"
                || revoked["reply"]
                    .as_str()
                    .is_some_and(|reply| reply.contains("error")),
            "disabled skill copied: {revoked}"
        );
        check_effect(mode, &root, &recovery)?;
    }
    let channel = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Team", "instructions": "Help", "members": [agent], "lead": agent}),
        )
        .await?)?;
    let channel_row = settle_row(
        &app,
        &format!("/api/channels/{channel}/turns"),
        json!({"id": new_id(), "text": "Channel"}),
    )
    .await?;
    // dcaab7f (#87) gives legacy channels the agent's managed workspace, so
    // the write is offered there (Node: not offered); the journal still
    // refuses the duplicate.
    ensure!(
        advertised(&script)?,
        "legacy channel lost the agent workspace"
    );
    let skill_revoked = mode == Mode::Skill;
    ensure!(
        channel_row["reply"].as_str().is_some_and(|reply| {
            reply.contains(refused) || skill_revoked && reply.contains("skill not found")
        }),
        "channel write: {channel_row}"
    );
    check_effect(mode, &root, &recovery)?;
    app.restart_with(
        ServerConfig::new()
            .gateway(model.gateway())
            .workspace(&agent, &root, false),
    )
    .await?;
    // 5528da5 (#87) offers the write tools on a read-only binding too (Node:
    // not offered); the call is refused and nothing changes.
    let denied = settle_row(&app, &path, json!({"id": new_id(), "text": "Denied"})).await?;
    ensure!(
        denied["reply"]
            .as_str()
            .is_some_and(|reply| reply.contains("error")),
        "read-only write: {denied}"
    );
    check_effect(mode, &root, &recovery)?;
    Ok(())
}

#[tokio::test]
async fn write_grant_creates_file_once_through_provider_and_result_journal_survives_restart(
) -> Result<()> {
    write_grant(Mode::File).await
}

#[tokio::test]
async fn write_grant_creates_directory_once_through_provider_and_result_journal_survives_restart(
) -> Result<()> {
    write_grant(Mode::Directory).await
}

#[tokio::test]
async fn write_grant_creates_skill_once_through_provider_and_result_journal_survives_restart(
) -> Result<()> {
    write_grant(Mode::Skill).await
}

#[tokio::test]
async fn write_grant_creates_delete_once_through_provider_and_result_journal_survives_restart(
) -> Result<()> {
    write_grant(Mode::Delete).await
}

#[tokio::test]
async fn write_grant_creates_restore_once_through_provider_and_result_journal_survives_restart(
) -> Result<()> {
    write_grant(Mode::Restore).await
}

#[tokio::test]
async fn write_grant_creates_replace_once_through_provider_and_result_journal_survives_restart(
) -> Result<()> {
    write_grant(Mode::Replace).await
}

#[tokio::test]
async fn provider_workspace_writes_reject_malformed_unicode_without_changing_bytes_or_path(
) -> Result<()> {
    let args = Arc::new(Mutex::new(String::new()));
    let model = ControlModel::start().await?;
    let shared = args.clone();
    model.respond(move |request| {
        if after_tool(request) || reminder(request) {
            return Answer::text("Done");
        }
        let arguments = shared.lock().map(|args| args.clone()).unwrap_or_default();
        Answer::RawTool {
            name: "create_workspace_file".into(),
            arguments,
        }
    });
    let workspace = TempWorkspace::new()?;
    let root = workspace.path().to_owned();
    let agent = new_id();
    let state = TestRoot::new()?;
    let app = state
        .start(
            ServerConfig::new()
                .gateway(model.gateway())
                .workspace(&agent, &root, true),
        )
        .await?;
    app.post(
        "/api/agents",
        json!({"runtime": "api", "id": agent, "name": "Unicode", "model": "fixture", "instructions": "Help."}),
    )
    .await?;
    let path = format!("/api/agents/{agent}/turns");
    for invalid in [
        r#"{"path":"bad.txt","text":"\ud800"}"#,
        r#"{"path":"bad\udc00.txt","text":"text"}"#,
    ] {
        *args.lock().map_err(|_| "args")? = invalid.into();
        let row = settle_row(
            &app,
            &path,
            json!({"id": new_id(), "text": "Create requested file."}),
        )
        .await?;
        ensure!(
            row["status"] == "failed"
                || row["reply"]
                    .as_str()
                    .is_some_and(|reply| !reply.contains("made")),
            "{invalid}: {row}"
        );
        ensure!(
            entries(&root)?.is_empty(),
            "{invalid} wrote: {:?}",
            entries(&root)?
        );
    }
    let name = "valid-\u{1f600}.txt";
    let text = "Exact \u{1f600} text";
    *args.lock().map_err(|_| "args")? = json!({"path": name, "text": text}).to_string();
    let row = settle_row(
        &app,
        &path,
        json!({"id": new_id(), "text": "Create valid file."}),
    )
    .await?;
    ensure!(row["status"] == "done", "valid file: {row}");
    ensure!(fs::read_to_string(root.join(name))? == text, "valid bytes");
    fs::write(root.join("replacement-\u{fffd}.txt"), "KEEP")?;
    let file = format!("/api/agents/{agent}/workspace/file?path=");
    let invalid = app
        .call(
            minarbot_testkit::http::Method::GET,
            &format!("{file}replacement-%ED%A0%80.txt"),
            None,
        )
        .await?;
    ensure!(
        !(200..300).contains(&invalid.status),
        "invalid UTF-8 path served: {}",
        invalid.body
    );
    ensure!(
        app.get(&format!("{file}replacement-%EF%BF%BD.txt")).await?["text"] == "KEEP",
        "replacement character path"
    );
    Ok(())
}
