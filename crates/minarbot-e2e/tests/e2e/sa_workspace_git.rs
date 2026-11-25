//! Git workspace HTTP reads, tools and checkpoints (port of
//! `app/rust-workspace-git-http.test.ts`).

use crate::sa_support::{id, lock, offers, turn_tool_results, Shared};
use minarbot_testkit::{
    model::{Reply, ScriptedModel},
    prelude::*,
    Server,
};
use std::{path::Path, process::Command};

/// Run git in `root` with a hermetic configuration; returns trimmed stdout.
fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .current_dir(root)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()?;
    ensure!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim_end().to_owned())
}

/// Repository with one commit of `note.txt` ("before\n").
fn repository(root: &Path) -> Result<()> {
    git(root, &["init", "-q", "--initial-branch=main"])?;
    git(root, &["config", "user.name", "Workspace Tester"])?;
    git(root, &["config", "user.email", "workspace@example.test"])?;
    std::fs::write(root.join("note.txt"), "before\n")?;
    git(root, &["add", "--", "note.txt"])?;
    git(root, &["commit", "-q", "-m", "Initial"])?;
    Ok(())
}

#[tokio::test]
async fn git_log_returns_last_50_commits_and_no_more() -> Result<()> {
    let root = TestRoot::new()?;
    let repo = root.dir("repo")?;
    repository(&repo)?;
    let agent = new_id();
    let app = root
        .start(ServerConfig::new().workspace(&agent, &repo, false))
        .await?;
    app.post_expect(
        "/api/agents",
        json!({"runtime": "api", "id": agent, "name": "Reader", "model": "fixture",
            "instructions": "Help"}),
        201,
    )
    .await?;
    let tree = git(&repo, &["rev-parse", "HEAD^{tree}"])?;
    let mut head = git(&repo, &["rev-parse", "HEAD"])?;
    for n in 0..55 {
        head = git(
            &repo,
            &[
                "commit-tree",
                &tree,
                "-p",
                &head,
                "-m",
                &format!("History {n}"),
            ],
        )?;
    }
    git(&repo, &["update-ref", "HEAD", &head])?;
    let log = app
        .get_expect(&format!("/api/agents/{agent}/git/log"), 200)
        .await?;
    ensure!(
        log["commits"].as_array().map(Vec::len) == Some(50),
        "commits: {}",
        log["commits"].as_array().map_or(0, Vec::len)
    );
    ensure!(
        log["commits"][0]["subject"] == "History 54",
        "first: {}",
        log["commits"][0]
    );
    Ok(())
}

async fn send(app: &Server, path: &str) -> Result<Value> {
    let turn = new_id();
    app.post_expect(path, json!({"id": turn, "text": "Git task"}), 202)
        .await?;
    app.wait_row(path, &turn, |row| {
        row["status"] == "done" || row["status"] == "failed"
    })
    .await
}

/// The turn finished and its only tool call was refused by MCP authorization.
fn refused(row: &Value) -> bool {
    row["status"] == "done"
        && serde_json::from_str::<Value>(row["reply"].as_str().unwrap_or("null")).ok()
            == Some(json!({"error": "MCP authorization failed.", "status": 403}))
}

fn used(row: &Value, tool: &str) -> bool {
    row["activity"]["tools"]
        .as_array()
        .is_some_and(|tools| tools.iter().any(|item| item["name"] == tool))
}

#[tokio::test]
async fn http_git_reads_tools_checkpoints_use_workspace_gating_and_reject_foreign_requests_restart_and_restore_through_api(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    let chosen: Shared<String> = std::sync::Arc::new(std::sync::Mutex::new("git_status".into()));
    let tool = chosen.clone();
    model.respond(move |request| {
        if let Some(result) = turn_tool_results(request).last() {
            return Reply::text(result["content"].as_str().unwrap_or_default());
        }
        let name = tool.lock().map(|name| name.clone()).unwrap_or_default();
        let arguments = if name == "git_commit" {
            json!({"message": "Tool commit"})
        } else {
            json!({})
        };
        Reply::tool_call("git-call", &name, arguments)
    });
    let root = TestRoot::new()?;
    let repo = root.dir("repo")?;
    repository(&repo)?;
    let (agent, readonly) = (new_id(), new_id());
    let config = ServerConfig::new()
        .model(&model)
        .workspace(&agent, &repo, true)
        .workspace(&readonly, &repo, false);
    let mut app = root.start(config).await?;
    let managed = new_id();
    for id in [&agent, &readonly, &managed] {
        app.post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": id, "name": "Git fixture", "model": "fixture",
                "instructions": "Help"}),
            201,
        )
        .await?;
    }
    let path = format!("/api/agents/{agent}/git");
    let turns = format!("/api/agents/{agent}/turns");
    ensure!(
        app.get(&path).await?["isRepo"] == true,
        "workspace is not a repo"
    );
    ensure!(
        app.get(&format!("/api/agents/{managed}/git")).await? == json!({"isRepo": false}),
        "managed workspace"
    );
    let foreign = app
        .client()?
        .get(app.url(&path))
        .header("origin", "https://foreign.invalid")
        .send()
        .await?;
    ensure!(
        foreign.status() == 403,
        "foreign origin: {}",
        foreign.status()
    );
    for query in [
        "/diff?path=../private",
        "/diff?path=note.txt&path=note.txt",
        "/log?limit=51",
        "?fetch=1",
    ] {
        app.get_expect(&format!("{path}{query}"), 400).await?;
    }
    let row = send(&app, &turns).await?;
    ensure!(row["status"] == "done", "git turn: {row}");
    ensure!(
        used(&row, "git_status"),
        "git_status not used: {}",
        row["activity"]
    );
    ensure!(
        row["checkpoint"]["restorable"] == true,
        "checkpoint: {}",
        row["checkpoint"]
    );
    let first = model
        .requests()
        .first()
        .cloned()
        .ok_or("no model request")?;
    for name in [
        "git_status",
        "git_diff",
        "git_log",
        "git_commit",
        "git_create_branch",
        "git_create_worktree",
    ] {
        ensure!(
            offers(&first, name),
            "{name} not offered: {:?}",
            first.tool_names()
        );
    }
    std::fs::write(repo.join("note.txt"), "restore this")?;
    let turn = id(&row)?;
    let preview = app.get(&format!("{path}/checkpoints/{turn}")).await?;
    let command = json!({"id": new_id(), "checkpointId": turn, "digest": preview["digest"]});
    let restore = format!("{path}/checkpoints/{turn}/restore");
    let restored = app.post(&restore, command.clone()).await?;
    ensure!(restored["restored"] == true, "restore: {restored}");
    ensure!(
        std::fs::read_to_string(repo.join("note.txt"))? == "before\n",
        "checkpoint not restored"
    );
    app.restart().await?;
    std::fs::write(repo.join("note.txt"), "later")?;
    ensure!(
        app.post(&restore, command).await? == restored,
        "restore receipt"
    );
    ensure!(
        std::fs::read_to_string(repo.join("note.txt"))? == "later",
        "restore replayed after restart"
    );
    let side = app
        .post(
            &format!("/api/agents/{agent}/threads"),
            json!({"id": new_id(), "name": "Side"}),
        )
        .await?;
    ensure!(
        send(&app, &format!("/api/threads/{}/turns", id(&side)?)).await?["status"] == "done",
        "side thread turn"
    );
    *lock(&chosen)? = "git_commit".into();
    let denied = send(&app, &format!("/api/agents/{readonly}/turns")).await?;
    // #87: a harness gets an unoffered tool call back as an MCP error instead
    // of the server failing the turn; the call must not run.
    ensure!(refused(&denied), "read-only commit: {denied}");
    ensure!(
        !git(&repo, &["log", "--format=%s"])?.contains("Tool commit"),
        "read-only agent committed"
    );
    let last = model.requests().last().cloned().ok_or("no request")?;
    ensure!(!offers(&last, "git_commit"), "git_commit offered read-only");
    *lock(&chosen)? = "git_status".into();
    let managed_turn = send(&app, &format!("/api/agents/{managed}/turns")).await?;
    ensure!(
        managed_turn["status"] == "done",
        "managed turn: {managed_turn}"
    );
    ensure!(used(&managed_turn, "git_status"), "managed git_status");
    let reply: Value = serde_json::from_str(managed_turn["reply"].as_str().unwrap_or("null"))?;
    ensure!(reply == json!({"isRepo": false}), "managed reply: {reply}");
    // The gateway peer keeps its own session and log files in its working
    // directory; the turn itself must not create anything.
    let entries = app
        .get(&format!("/api/agents/{managed}/workspace/entries"))
        .await?["entries"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    ensure!(
        entries
            .iter()
            .all(|entry| entry["name"].as_str().is_some_and(|name| name
                == "fake-gateway-log.jsonl"
                || name.starts_with(".fake-gateway-"))),
        "managed entries: {}",
        app.get(&format!("/api/agents/{managed}/workspace/entries"))
            .await?
    );
    let room = app
        .post_expect(
            "/api/channels",
            json!({"id": new_id(), "name": "Team", "instructions": "Help", "members": [agent],
                "lead": agent}),
            201,
        )
        .await?;
    let room_turn = send(&app, &format!("/api/channels/{}/turns", id(&room)?)).await?;
    // 7098b86 (#87): a channel without a project uses the lead's workspace,
    // so the channel turn may read Git status (it was refused before).
    ensure!(
        room_turn["status"] == "done",
        "channel git turn: {room_turn}"
    );
    ensure!(
        used(&room_turn, "git_status"),
        "channel git_status: {room_turn}"
    );
    let last = model.requests().last().cloned().ok_or("no request")?;
    ensure!(
        offers(&last, "git_status"),
        "git_status not offered in a channel"
    );
    Ok(())
}
