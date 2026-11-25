//! Self-tests for the fake peers: each drives a built `fake-*` binary over
//! stdio, without a Minarbot server, and proves one group of behaviours from
//! the checklist in `docs/testing.md` (ids N*, A*, G* below).

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::post, Json, Router};
use minarbot_testkit::{
    ensure,
    fake::native::{RELEASE, VERSION, WORKSPACE_SHELL},
    fakes::{self, Fake},
    model::{Reply, ScriptedModel},
    workspace::TempWorkspace,
    Result,
};
use serde_json::{json, Value};
use std::{
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines},
    net::TcpListener,
    process::{Child, ChildStdin, ChildStdout, Command},
};

const LIMIT: Duration = Duration::from_secs(10);

/// A fake peer process with line-based JSON I/O.
struct Peer {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Lines<BufReader<ChildStdout>>,
    seen: Vec<Value>,
}

fn command(
    fake: Fake,
    args: &[&str],
    cwd: &std::path::Path,
    env: &[(&str, &str)],
) -> Result<Command> {
    let mut command = Command::new(fakes::path(fake)?);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", cwd)
        .current_dir(cwd)
        .kill_on_drop(true);
    for (name, value) in env {
        command.env(name, value);
    }
    Ok(command)
}

impl Peer {
    async fn spawn(
        fake: Fake,
        args: &[&str],
        cwd: &std::path::Path,
        env: &[(&str, &str)],
    ) -> Result<Self> {
        let mut child = command(fake, args, cwd, env)?
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or("no stdout")?;
        Ok(Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            seen: Vec::new(),
        })
    }

    async fn send(&mut self, value: Value) -> Result<()> {
        let stdin = self.stdin.as_mut().ok_or("stdin closed")?;
        stdin.write_all(format!("{value}\n").as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    /// Read output until `done` matches a message; returns it.
    async fn expect(&mut self, label: &str, done: impl Fn(&Value) -> bool) -> Result<Value> {
        let deadline = tokio::time::Instant::now() + LIMIT;
        loop {
            let line = tokio::time::timeout_at(deadline, self.lines.next_line())
                .await
                .map_err(|_| format!("no {label} within {LIMIT:?}; saw {:?}", self.seen))??
                .ok_or_else(|| format!("output ended before {label}; saw {:?}", self.seen))?;
            let value: Value = serde_json::from_str(&line).unwrap_or(Value::String(line));
            self.seen.push(value.clone());
            if done(&value) {
                return Ok(value);
            }
        }
    }

    /// Wait for exit after closing stdin; returns code and stderr.
    async fn exit(mut self) -> Result<(Option<i32>, String)> {
        drop(self.stdin.take());
        let status = tokio::time::timeout(LIMIT, self.child.wait())
            .await
            .map_err(|_| "peer did not exit")??;
        let mut stderr = String::new();
        if let Some(mut pipe) = self.child.stderr.take() {
            pipe.read_to_string(&mut stderr).await?;
        }
        Ok((status.code(), stderr))
    }
}

async fn run(
    fake: Fake,
    args: &[&str],
    cwd: &std::path::Path,
    env: &[(&str, &str)],
) -> Result<(Option<i32>, String)> {
    let output = command(fake, args, cwd, env)?
        .stdin(Stdio::null())
        .output()
        .await?;
    Ok((output.status.code(), String::from_utf8(output.stdout)?))
}

fn method(name: &'static str) -> impl Fn(&Value) -> bool {
    move |value| value["method"] == name
}

const TOKEN: &str = "fixture-mcp-token";

/// Codex `thread/start` params that satisfy the fake's assertions.
fn thread_params(workspace: &TempWorkspace, mcp: &str) -> Value {
    json!({"threadId": "thread-1", "cwd": workspace.path(), "approvalPolicy": "on-request",
        "config": {"mcp_servers": {"minarbot": {"url": mcp, "bearer_token_env_var": "MINARBOT_MCP_TOKEN"}},
            "approvals_reviewer": "user", "shell_environment_policy.inherit": "none",
            "shell_environment_policy.set": {"PATH": "/usr/bin"}}})
}

async fn codex(workspace: &TempWorkspace, env: &[(&str, &str)]) -> Result<Peer> {
    let mut all = vec![("MINARBOT_MCP_TOKEN", TOKEN)];
    all.extend_from_slice(env);
    let mut peer = Peer::spawn(Fake::Codex, &["app-server"], workspace.path(), &all).await?;
    peer.send(json!({"id": 1, "method": "initialize", "params": {}}))
        .await?;
    let init = peer.expect("initialize result", |v| v["id"] == 1).await?;
    ensure!(
        init["result"]["userAgent"] == "fake",
        "N4 initialize: {init}"
    );
    peer.send(json!({"id": 2, "method": "thread/start", "params": thread_params(workspace, "http://127.0.0.1:9/mcp")}))
        .await?;
    let thread = peer.expect("thread result", |v| v["id"] == 2).await?;
    ensure!(
        thread["result"]["thread"]["id"] == "thread-1",
        "N5 thread: {thread}"
    );
    Ok(peer)
}

async fn codex_turn(peer: &mut Peer, id: u64, text: &str) -> Result<()> {
    peer.send(
        json!({"id": id, "method": "turn/start", "params": {"threadId": "thread-1",
        "input": [{"type": "text", "text": text}], "sandboxPolicy": {"type": "readOnly"},
        "additionalContext": {"runtime": {"value": "context"}}}}),
    )
    .await?;
    let started = peer.expect("turn result", |v| v["id"] == id).await?;
    ensure!(
        started["result"]["turn"]["status"] == "inProgress",
        "N6 turn/start: {started}"
    );
    Ok(())
}

fn deltas(seen: &[Value]) -> String {
    seen.iter()
        .filter(|v| v["method"] == "item/agentMessage/delta")
        .filter_map(|v| v["params"]["delta"].as_str())
        .collect()
}

#[tokio::test]
async fn n1_n2_n3_native_version_sign_in_status_and_scrubbed_environment() -> Result<()> {
    let workspace = TempWorkspace::new()?;
    for fake in [Fake::Codex, Fake::Claude] {
        let (code, out) = run(fake, &["--version"], workspace.path(), &[]).await?;
        ensure!(
            code == Some(0) && out.trim() == VERSION,
            "N1 {fake:?}: {code:?} {out}"
        );
    }
    let home = workspace.path().to_string_lossy().into_owned();
    let codex = [("CODEX_HOME", home.as_str())];
    ensure!(
        run(Fake::Codex, &["login", "status"], workspace.path(), &codex)
            .await?
            .0
            == Some(1),
        "N2 signed out"
    );
    workspace.write("auth.json", "{}")?;
    ensure!(
        run(Fake::Codex, &["login", "status"], workspace.path(), &codex)
            .await?
            .0
            == Some(0),
        "N2 signed in"
    );
    let claude = [("CLAUDE_CONFIG_DIR", home.as_str())];
    ensure!(
        run(Fake::Claude, &["auth", "status"], workspace.path(), &claude)
            .await?
            .0
            == Some(1),
        "N2 claude signed out"
    );
    workspace.write(".credentials.json", "{}")?;
    ensure!(
        run(Fake::Claude, &["auth", "status"], workspace.path(), &claude)
            .await?
            .0
            == Some(0),
        "N2 claude signed in"
    );
    for key in minarbot_testkit::fake::native::SCRUBBED {
        let peer = Peer::spawn(
            Fake::Codex,
            &["app-server"],
            workspace.path(),
            &[(key, "leak")],
        )
        .await?;
        let (code, stderr) = peer.exit().await?;
        ensure!(
            code == Some(1) && stderr.contains(&format!("{key} leaked")),
            "N3 {key}: {code:?} {stderr}"
        );
    }
    let peer = Peer::spawn(
        Fake::Claude,
        &[],
        workspace.path(),
        &[("ANTHROPIC_API_KEY", "minarbot-no-key")],
    )
    .await?;
    ensure!(
        peer.exit().await?.0 == Some(0),
        "N3 placeholder Anthropic key must be accepted"
    );
    Ok(())
}

#[tokio::test]
async fn n5_n6_codex_thread_and_turn_assertions_fail_the_peer() -> Result<()> {
    let workspace = TempWorkspace::new()?;
    let mut bad = thread_params(&workspace, "http://example.com/mcp");
    for (field, value) in [
        ("approvalPolicy", json!("never")),
        ("cwd", json!("/elsewhere")),
    ] {
        let mut params = thread_params(&workspace, "http://127.0.0.1:9/mcp");
        params[field] = value;
        let mut peer = Peer::spawn(
            Fake::Codex,
            &["app-server"],
            workspace.path(),
            &[("MINARBOT_MCP_TOKEN", TOKEN)],
        )
        .await?;
        peer.send(json!({"id": 2, "method": "thread/start", "params": params}))
            .await?;
        ensure!(peer.exit().await?.0 == Some(1), "N5 {field} accepted");
    }
    let mut peer = Peer::spawn(
        Fake::Codex,
        &["app-server"],
        workspace.path(),
        &[("MINARBOT_MCP_TOKEN", TOKEN)],
    )
    .await?;
    peer.send(json!({"id": 2, "method": "thread/start", "params": bad.take()}))
        .await?;
    ensure!(
        peer.exit().await?.0 == Some(1),
        "N5 non-loopback MCP accepted"
    );
    let mut leak = thread_params(&workspace, "http://127.0.0.1:9/mcp");
    leak["config"]["shell_environment_policy.set"] = json!({"TOKEN": TOKEN});
    let mut peer = Peer::spawn(
        Fake::Codex,
        &["app-server"],
        workspace.path(),
        &[("MINARBOT_MCP_TOKEN", TOKEN)],
    )
    .await?;
    peer.send(json!({"id": 2, "method": "thread/start", "params": leak}))
        .await?;
    ensure!(
        peer.exit().await?.0 == Some(1),
        "N5 token in shell environment accepted"
    );
    let mut peer = codex(&workspace, &[]).await?;
    peer.send(json!({"id": 3, "method": "turn/start", "params": {"threadId": "thread-1",
        "input": [{"text": "x"}], "sandboxPolicy": {"type": "workspaceWrite", "networkAccess": true}}}))
        .await?;
    ensure!(
        peer.exit().await?.0 == Some(1),
        "N6 network sandbox accepted"
    );
    Ok(())
}

#[tokio::test]
async fn n7_n8_n9_n17_n18_codex_scripted_turns_usage_secrets_and_log() -> Result<()> {
    let workspace = TempWorkspace::new()?;
    let mut peer = codex(
        &workspace,
        &[("MINARBOT_PROVIDER_KEY", "provider-secret-key")],
    )
    .await?;
    codex_turn(&mut peer, 3, "hello").await?;
    let usage = peer
        .expect("usage", method("thread/tokenUsage/updated"))
        .await?;
    ensure!(
        usage["params"]["tokenUsage"]["last"] == json!({"inputTokens": 11, "outputTokens": 7}),
        "N7 usage: {usage}"
    );
    peer.expect("completed", method("turn/completed")).await?;
    ensure!(
        deltas(&peer.seen) == "Harness finished",
        "N7 text: {}",
        deltas(&peer.seen)
    );
    ensure!(
        peer.seen
            .iter()
            .any(|v| v["method"] == "item/started"
                && v["params"]["item"]["type"] == "commandExecution"),
        "N7 shell item"
    );
    peer.seen.clear();
    codex_turn(
        &mut peer,
        4,
        "NATIVE_CHECK FINAL_SECRET FINAL_PROVIDER_SECRET",
    )
    .await?;
    peer.expect("completed", method("turn/completed")).await?;
    ensure!(
        peer.seen
            .iter()
            .any(|v| v["params"]["item"]["type"] == "fileChange"),
        "N8 file change"
    );
    let check = peer
        .seen
        .iter()
        .find(|v| v["params"]["item"]["aggregatedOutput"] == "Native check passed.")
        .ok_or("N8 check receipt")?;
    ensure!(
        check["params"]["item"]["command"] == "node --test fixture.test.mjs",
        "N8 check: {check}"
    );
    let pieces: Vec<&str> = peer
        .seen
        .iter()
        .filter(|v| v["method"] == "item/agentMessage/delta")
        .filter_map(|v| v["params"]["delta"].as_str())
        .collect();
    ensure!(
        pieces.contains(&" test-mcp-session-") && pieces.contains(&"token"),
        "N9 split token: {pieces:?}"
    );
    ensure!(
        pieces.contains(&"provider") && pieces.contains(&"-secret-key"),
        "N9 split provider key: {pieces:?}"
    );
    peer.send(json!({"id": 9, "method": "turn/interrupt", "params": {"threadId": "thread-1"}}))
        .await?;
    peer.expect("interrupt result", |v| v["id"] == 9).await?;
    peer.exit().await?;
    let log = fakes::native_log(workspace.path())?;
    ensure!(
        log.iter().any(|row| row["interrupted"] == true),
        "N17 interrupt log"
    );
    ensure!(
        log.iter()
            .any(|row| row["method"] == "initialize" && row["home"].is_string()),
        "N18 message log: {log:?}"
    );
    ensure!(
        log.iter()
            .any(|row| row["context"] == json!({"runtime": {"value": "context"}})),
        "N6 context log"
    );
    ensure!(fakes::turns(&log).len() == 2, "N18 turn log");
    Ok(())
}

#[tokio::test]
async fn n10_n11_codex_approvals_resume_the_turn_with_the_decision() -> Result<()> {
    let workspace = TempWorkspace::new()?;
    let mut peer = codex(&workspace, &[]).await?;
    for (text, method_name, decision, reply) in [
        (
            "APPROVE",
            "item/commandExecution/requestApproval",
            json!({"decision": "accept"}),
            "Harness finished",
        ),
        (
            "APPROVE_SECRET",
            "item/commandExecution/requestApproval",
            json!({"decision": "decline"}),
            "Harness denied",
        ),
        (
            "NETWORK",
            "item/permissions/requestApproval",
            json!({"permissions": {"network": {"enabled": true}}}),
            "Harness finished",
        ),
        (
            "OUTSIDE_FILE",
            "item/fileChange/requestApproval",
            json!({"decision": "decline"}),
            "Harness denied",
        ),
        (
            "READ_ONLY_WRITE",
            "item/fileChange/requestApproval",
            json!({"decision": "accept"}),
            "Harness finished",
        ),
    ] {
        peer.seen.clear();
        codex_turn(&mut peer, 10, text).await?;
        let request = peer
            .expect("approval", |v| v["id"] == "approval-native")
            .await?;
        ensure!(
            request["method"] == method_name,
            "N10/N11 {text}: {request}"
        );
        if text == "APPROVE_SECRET" {
            ensure!(
                request["params"]["command"] == "echo test-mcp-session-token",
                "N10 secret command"
            );
        }
        if text == "OUTSIDE_FILE" {
            ensure!(
                request["params"]["grantRoot"] == "/outside",
                "N11 grant root"
            );
        }
        peer.send(json!({"id": "approval-native", "result": decision}))
            .await?;
        peer.expect("completed", method("turn/completed")).await?;
        ensure!(
            deltas(&peer.seen) == reply,
            "N10/N11 {text}: {}",
            deltas(&peer.seen)
        );
    }
    Ok(())
}

#[tokio::test]
async fn n12_n13_n14_n15_n16_gates_crashes_oversize_and_descendants() -> Result<()> {
    let workspace = TempWorkspace::new()?;
    let mut peer = codex(&workspace, &[]).await?;
    codex_turn(&mut peer, 3, "FIRST_PARTIAL_GATE").await?;
    peer.expect("first delta", method("item/agentMessage/delta"))
        .await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    ensure!(
        !peer.seen.iter().any(|v| v["method"] == "turn/completed"),
        "N13 gate did not hold"
    );
    workspace.write(RELEASE, "")?;
    peer.expect("completed", method("turn/completed")).await?;
    std::fs::remove_file(workspace.join(RELEASE))?;
    peer.seen.clear();
    codex_turn(&mut peer, 4, "STREAM_GATE").await?;
    peer.expect("gate draft", |v| v["params"]["itemId"] == "answer")
        .await?;
    ensure!(
        peer.seen
            .iter()
            .any(|v| v["method"] == "item/started" && v["params"]["item"]["id"] == "gate-tool"),
        "N12 tool segment"
    );
    workspace.write(RELEASE, "")?;
    let final_item = peer
        .expect("final answer", |v| {
            v["params"]["item"]["type"] == "agentMessage"
        })
        .await?;
    ensure!(
        final_item["params"]["item"]["text"] == "Final answer.",
        "N12 final: {final_item}"
    );
    ensure!(
        fakes::native_log(workspace.path())?
            .iter()
            .any(|row| row["type"] == "stream-gate"),
        "N12 gate log"
    );
    codex_turn(&mut peer, 5, "STREAM_CRASH").await?;
    peer.expect("buffered tail", |v| {
        v["params"]["delta"] == " buffered tail"
    })
    .await?;
    ensure!(peer.exit().await?.0 == Some(19), "N12 crash exit");
    let mut peer = codex(&workspace, &[]).await?;
    codex_turn(&mut peer, 3, "CRASH").await?;
    ensure!(peer.exit().await?.0 == Some(19), "N14 crash exit");
    let mut peer = codex(&workspace, &[]).await?;
    codex_turn(&mut peer, 3, "OVERSIZE").await?;
    // The oversized output has no newline: it ends at EOF once stdin closes.
    drop(peer.stdin.take());
    let mut longest = 0;
    while let Some(line) = tokio::time::timeout(LIMIT, peer.lines.next_line()).await?? {
        longest = longest.max(line.len());
    }
    ensure!(longest == 1024 * 1024 + 1, "N15 oversize line {longest}");
    let mut peer = codex(&workspace, &[]).await?;
    codex_turn(&mut peer, 3, "HOLD").await?;
    let pid = minarbot_testkit::wait::until("descendant log", || async {
        Ok(fakes::native_log(workspace.path())?
            .iter()
            .find(|row| row["type"] == "descendant")
            .and_then(|row| row["pid"].as_u64()))
    })
    .await?;
    ensure!(
        std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "N16 descendant not running"
    );
    peer.exit().await?;
    let _ = Command::new("kill").arg(pid.to_string()).status().await;
    Ok(())
}

async fn claude(workspace: &TempWorkspace, args: &[&str], mcp_config: &Value) -> Result<Peer> {
    let config = mcp_config.to_string();
    let mut all: Vec<&str> = vec![
        "-p",
        "--mcp-config",
        &config,
        "--append-system-prompt",
        "fixture context",
    ];
    all.extend_from_slice(args);
    let mut peer = Peer::spawn(
        Fake::Claude,
        &all,
        workspace.path(),
        &[("MINARBOT_MCP_TOKEN", TOKEN)],
    )
    .await?;
    peer.send(json!({"type": "control_request", "request_id": "init", "request": {"subtype": "initialize",
        "hooks": {"PreToolUse": [{"hookCallbackIds": ["pre"]}], "Stop": [{"hookCallbackIds": ["stop"]}]}}}))
        .await?;
    let init = peer
        .expect("initialize response", |v| v["type"] == "control_response")
        .await?;
    ensure!(
        init["response"]["response"]["hooks_applied"] == true,
        "N19 initialize: {init}"
    );
    Ok(peer)
}

fn stdio_config() -> Value {
    json!({"mcpServers": {"minarbot": {"type": "stdio", "command": "/bin/true", "args": []}}})
}

async fn user(peer: &mut Peer, text: &str) -> Result<()> {
    peer.send(json!({"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": text}]}}))
        .await
}

fn stream_text(seen: &[Value]) -> String {
    seen.iter()
        .filter(|v| v["type"] == "stream_event")
        .filter_map(|v| v["event"]["delta"]["text"].as_str())
        .collect()
}

#[tokio::test]
async fn n19_n20_n24_n25_claude_stream_json_turns_usage_interrupt_and_resume() -> Result<()> {
    let workspace = TempWorkspace::new()?;
    let mut peer = claude(&workspace, &["--resume", "session-7"], &stdio_config()).await?;
    user(&mut peer, "hello").await?;
    let system = peer
        .expect("system init", |v| v["type"] == "system")
        .await?;
    ensure!(
        system["session_id"] == "session-7",
        "N25 resumed session: {system}"
    );
    let result = peer.expect("result", |v| v["type"] == "result").await?;
    ensure!(
        result["usage"]["input_tokens"] == 11 && result["usage"]["output_tokens"] == 7,
        "N20 usage: {result}"
    );
    ensure!(
        stream_text(&peer.seen) == "Harness finished",
        "N20 text: {}",
        stream_text(&peer.seen)
    );
    ensure!(
        peer.seen
            .iter()
            .any(|v| v["type"] == "assistant" && v["message"]["content"][0]["name"] == "Bash"),
        "N20 tool use"
    );
    peer.send(json!({"type": "control_request", "request_id": "stop-now", "request": {"subtype": "interrupt"}}))
        .await?;
    peer.expect("interrupt response", |v| {
        v["response"]["request_id"] == "stop-now"
    })
    .await?;
    peer.exit().await?;
    let log = fakes::native_log(workspace.path())?;
    let init = log
        .iter()
        .find(|row| row["type"] == "init")
        .ok_or("N19 init log")?;
    ensure!(
        init["resume"] == true && init["callback"] == true && init["context"] == "fixture context",
        "N19 init log: {init}"
    );
    ensure!(
        log.iter().any(|row| row["interrupted"] == true),
        "N24 interrupt log"
    );
    let leaky = json!({"mcpServers": {"minarbot": {"type": "stdio", "command": "/bin/true", "args": [TOKEN]}}});
    let config = leaky.to_string();
    let mut peer = Peer::spawn(
        Fake::Claude,
        &["--mcp-config", &config],
        workspace.path(),
        &[("MINARBOT_MCP_TOKEN", TOKEN)],
    )
    .await?;
    peer.send(json!({"type": "control_request", "request_id": "init", "request": {"subtype": "initialize"}})).await?;
    ensure!(
        peer.exit().await?.0 == Some(1),
        "N19 token in --mcp-config accepted"
    );
    Ok(())
}

#[tokio::test]
async fn n21_n22_claude_hooks_decide_approvals_and_stop_reminders() -> Result<()> {
    let workspace = TempWorkspace::new()?;
    let mut peer = claude(&workspace, &["--session-id", "session-1"], &stdio_config()).await?;
    for (text, tool, decision, reply) in [
        ("APPROVE", "Bash", "allow", "Harness finished"),
        ("NETWORK", "WebFetch", "deny", "Harness denied"),
        ("OUTSIDE_FILE", "Write", "deny", "Harness denied"),
        ("WORKSPACE_WRITE", "Write", "allow", "Harness finished"),
    ] {
        peer.seen.clear();
        user(&mut peer, text).await?;
        let hook = peer
            .expect("hook", |v| v["request_id"] == "approval-native")
            .await?;
        ensure!(
            hook["request"]["callback_id"] == "pre"
                && hook["request"]["input"]["tool_name"] == tool,
            "N21 {text}: {hook}"
        );
        peer.send(
            json!({"type": "control_response", "response": {"request_id": "approval-native",
            "response": {"hookSpecificOutput": {"permissionDecision": decision}}}}),
        )
        .await?;
        peer.expect("result", |v| v["type"] == "result").await?;
        ensure!(
            stream_text(&peer.seen) == reply,
            "N21 {text}: {}",
            stream_text(&peer.seen)
        );
    }
    peer.seen.clear();
    user(&mut peer, "Create a deliverable.").await?;
    let stop = peer
        .expect("stop hook", |v| v["request_id"] == "sv-stop")
        .await?;
    ensure!(
        stop["request"]["callback_id"] == "stop",
        "N22 stop hook: {stop}"
    );
    peer.send(
        json!({"type": "control_response", "response": {"request_id": "sv-stop",
        "response": {"decision": "block", "reason": "One verification reminder: check."}}}),
    )
    .await?;
    peer.expect("reminder result", |v| v["type"] == "result")
        .await?;
    peer.exit().await?;
    let log = fakes::native_log(workspace.path())?;
    ensure!(
        log.iter()
            .any(|row| row["type"] == "stop-hook" && row["decision"] == "block"),
        "N22 stop log"
    );
    let turns = fakes::turns(&log);
    ensure!(
        turns
            .last()
            .is_some_and(|turn| turn["prompt"] == "One verification reminder: check."
                && turn["session"] == "session-1"),
        "N22 reminder turn: {turns:?}"
    );
    Ok(())
}

/// Stdio MCP stand-in: answers one `tools/call` with exit code 0.
fn stdio_mcp(workspace: &TempWorkspace) -> Result<Value> {
    let script = workspace.write(
        "mcp.sh",
        "#!/bin/sh\nread line\necho '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"structuredContent\":{\"exitCode\":0}}}'\n",
    )?;
    Ok(
        json!({"mcpServers": {"minarbot": {"type": "stdio", "command": "/bin/sh", "args": [script]}}}),
    )
}

#[tokio::test]
async fn n23_claude_workspace_shell_goes_through_minarbot_run_shell() -> Result<()> {
    let workspace = TempWorkspace::new()?;
    let mut peer = claude(&workspace, &[], &stdio_mcp(&workspace)?).await?;
    user(&mut peer, "WORKSPACE_SHELL_FIXTURE").await?;
    let ls = peer
        .expect("native ls hook", |v| v["request_id"] == "workspace-ls")
        .await?;
    ensure!(
        ls["request"]["input"]["tool_input"]["command"] == "ls -al",
        "N23 ls: {ls}"
    );
    let deny = |id: &str| {
        json!({"type": "control_response", "response": {"request_id": id,
        "response": {"hookSpecificOutput": {"permissionDecision": "deny",
            "permissionDecisionReason": "Use mcp__minarbot__run_shell."}}}})
    };
    peer.send(deny("workspace-ls")).await?;
    peer.expect("loop permission", |v| v["request_id"] == "workspace-loop")
        .await?;
    peer.send(
        json!({"type": "control_response", "response": {"request_id": "workspace-loop",
        "response": {"behavior": "deny", "message": "Use mcp__minarbot__run_shell."}}}),
    )
    .await?;
    let sandbox = peer
        .expect("run_shell hook", |v| v["request_id"] == "workspace-sandbox")
        .await?;
    ensure!(
        sandbox["request"]["input"]["tool_input"]["command"] == WORKSPACE_SHELL,
        "N23 run_shell: {sandbox}"
    );
    peer.send(
        json!({"type": "control_response", "response": {"request_id": "workspace-sandbox",
        "response": {"hookSpecificOutput": {"permissionDecision": "allow"}}}}),
    )
    .await?;
    peer.expect("result", |v| v["type"] == "result").await?;
    ensure!(
        stream_text(&peer.seen) == "finished",
        "N23 reply: {}",
        stream_text(&peer.seen)
    );
    Ok(())
}

#[derive(Default)]
struct Mcp {
    calls: Vec<Value>,
}

/// HTTP MCP stand-in for the gateway and the native setup fixture.
async fn mcp_server(state: Arc<Mutex<Mcp>>) -> Result<String> {
    async fn handle(
        State(state): State<Arc<Mutex<Mcp>>>,
        Json(body): Json<Value>,
    ) -> axum::response::Response {
        let id = body["id"].clone();
        if body["method"] == "tools/list" {
            return Json(json!({"jsonrpc": "2.0", "id": id, "result": {"tools": [
                {"name": "echo", "description": "Echo", "inputSchema": {"type": "object", "required": ["requestId"]}},
                {"name": "ask_user", "inputSchema": {"type": "object"}},
                {"name": "edit_profile", "inputSchema": {"type": "object"}},
                {"name": "forbidden", "inputSchema": {"type": "object"}},
                {"name": "image", "inputSchema": {"type": "object"}},
                {"name": "create_text_file", "inputSchema": {"type": "object"}},
                {"name": "wait_task", "inputSchema": {"type": "object"}},
                {"name": "delegate_task", "inputSchema": {"type": "object"}}]}}))
            .into_response();
        }
        let name = body["params"]["name"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if let Ok(mut state) = state.lock() {
            state.calls.push(body["params"].clone());
        }
        match name.as_str() {
            "forbidden" | "create_text_file" => (StatusCode::FORBIDDEN, Json(json!({"error": "Tool not allowed."}))).into_response(),
            "explode" => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "boom"}))).into_response(),
            "image" => Json(json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "image", "data": "aGVsbG8=", "mimeType": "image/png"}]}})).into_response(),
            "delegate_task" => Json(json!({"jsonrpc": "2.0", "id": id, "result": {"structuredContent": {"taskId": "task-1"}}})).into_response(),
            "edit_profile" => Json(json!({"jsonrpc": "2.0", "id": id, "result": {"structuredContent": {"revision": 2}}})).into_response(),
            _ => Json(json!({"jsonrpc": "2.0", "id": id, "result": {"structuredContent": body["params"]["arguments"]}})).into_response(),
        }
    }
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let url = format!("http://{}/mcp", listener.local_addr()?);
    let app = Router::new().route("/mcp", post(handle)).with_state(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(url)
}

#[tokio::test]
async fn n26_native_setup_fixture_saves_the_profile_through_mcp() -> Result<()> {
    let workspace = TempWorkspace::new()?;
    let state = Arc::new(Mutex::new(Mcp::default()));
    let url = mcp_server(state.clone()).await?;
    let mut peer = Peer::spawn(
        Fake::Codex,
        &["app-server"],
        workspace.path(),
        &[("MINARBOT_MCP_TOKEN", TOKEN)],
    )
    .await?;
    peer.send(
        json!({"id": 2, "method": "thread/start", "params": thread_params(&workspace, &url)}),
    )
    .await?;
    peer.expect("thread", |v| v["id"] == 2).await?;
    codex_turn(&mut peer, 3, "NATIVE_SETUP_FIXTURE Initial needs: research").await?;
    peer.expect("completed", method("turn/completed")).await?;
    let calls: Vec<Value> = state
        .lock()
        .map_err(|_| "poisoned")?
        .calls
        .iter()
        .map(|call| call["name"].clone())
        .collect();
    ensure!(
        calls == [json!("create_text_file"), json!("edit_profile")],
        "N26 MCP calls: {calls:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a1_to_a12_acp_session_permission_files_cancel_and_errors() -> Result<()> {
    let workspace = TempWorkspace::new()?;
    let (code, help) = run(Fake::Acp, &["--help"], workspace.path(), &[]).await?;
    ensure!(code == Some(0) && help.contains("acp"), "A1 help: {help}");
    ensure!(
        run(Fake::Acp, &["--version"], workspace.path(), &[])
            .await?
            .1
            .trim()
            == "fake-acp 1.0.0",
        "A1 version"
    );
    let env = [
        ("MINARBOT_MCP_TOKEN", TOKEN),
        ("MINARBOT_HARNESS_CANARY", "seen"),
    ];
    let mut peer = Peer::spawn(Fake::Acp, &["acp"], workspace.path(), &env).await?;
    peer.send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientCapabilities": {"fs": {"readTextFile": true}}}})).await?;
    let init = peer.expect("initialize", |v| v["id"] == 1).await?;
    ensure!(
        init["result"]["agentCapabilities"]["loadSession"] == true
            && init["result"]["authMethods"][0]["id"] == "oauth-personal",
        "A2: {init}"
    );
    peer.send(json!({"jsonrpc": "2.0", "id": 2, "method": "session/load", "params": {"sessionId": "fixture-session", "mcpServers": []}})).await?;
    let historic = peer
        .expect("historic chunk", method("session/update"))
        .await?;
    ensure!(
        historic["params"]["update"]["content"]["text"] == "Historic text must not replay.",
        "A3 load replay: {historic}"
    );
    let loaded = peer.expect("load result", |v| v["id"] == 2).await?;
    ensure!(
        loaded["result"]["sessionId"] == "fixture-session",
        "A3 load: {loaded}"
    );
    peer.send(json!({"jsonrpc": "2.0", "id": 3, "method": "session/set_mode", "params": {"modeId": "default"}})).await?;
    peer.expect("mode", |v| v["id"] == 3).await?;
    peer.send(json!({"jsonrpc": "2.0", "id": 4, "method": "session/prompt", "params": {"sessionId": "fixture-session", "prompt": [{"type": "text", "text": "FILES READONLY"}]}})).await?;
    let permission = peer
        .expect("permission request", method("session/request_permission"))
        .await?;
    ensure!(
        permission["params"]["options"].as_array().map(Vec::len) == Some(3),
        "A5 options: {permission}"
    );
    peer.send(json!({"jsonrpc": "2.0", "id": permission["id"], "result": {"outcome": {"optionId": "once"}}})).await?;
    for (expected, reply) in [
        (
            "../escape.txt",
            json!({"error": {"code": 1, "message": "outside"}}),
        ),
        (
            "outside/escape.txt",
            json!({"error": {"code": 1, "message": "link"}}),
        ),
        ("hello.txt", json!({"result": {}})),
        ("hello.txt", json!({"result": {"content": "hello\n"}})),
        (
            "readonly.txt",
            json!({"error": {"code": 1, "message": "read-only"}}),
        ),
    ] {
        let request = peer
            .expect("fs request", |v| {
                v["method"].as_str().is_some_and(|m| m.starts_with("fs/"))
            })
            .await?;
        ensure!(
            request["params"]["path"] == expected,
            "A6/A7 fs order: {request}"
        );
        let mut answer = reply.clone();
        answer["jsonrpc"] = json!("2.0");
        answer["id"] = request["id"].clone();
        peer.send(answer).await?;
    }
    let done = peer.expect("prompt result", |v| v["id"] == 4).await?;
    ensure!(done["result"]["stopReason"] == "end_turn", "A5 end: {done}");
    ensure!(
        peer.seen
            .iter()
            .any(|v| v["params"]["update"]["status"] == "completed"),
        "A5 allowed tool completed"
    );
    peer.send(json!({"jsonrpc": "2.0", "id": 5, "method": "session/prompt", "params": {"sessionId": "fixture-session", "prompt": [{"text": "WAIT"}]}})).await?;
    peer.expect("waiting", |v| {
        v["params"]["update"]["content"]["text"] == "Waiting."
    })
    .await?;
    peer.send(json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": "fixture-session"}})).await?;
    let cancelled = peer.expect("cancelled", |v| v["id"] == 5).await?;
    ensure!(
        cancelled["result"]["stopReason"] == "cancelled",
        "A10 cancel: {cancelled}"
    );
    peer.send(json!({"jsonrpc": "2.0", "id": 6, "method": "unknown/method"}))
        .await?;
    let unsupported = peer.expect("unsupported", |v| v["id"] == 6).await?;
    ensure!(unsupported["error"]["code"] == -32601, "A11: {unsupported}");
    let (_, stderr) = peer.exit().await?;
    ensure!(
        stderr.contains(&format!("ACP cancelled {TOKEN}")),
        "A10 stderr: {stderr}"
    );
    ensure!(workspace.read("cancel-received")? == "yes", "A10 marker");
    let log = fakes::acp_log(workspace.path())?;
    ensure!(
        log.iter().all(|row| row["canary"] == "seen"),
        "A12 canary log"
    );
    let mut peer = Peer::spawn(Fake::Acp, &["acp"], workspace.path(), &[]).await?;
    peer.send(json!({"jsonrpc": "2.0", "id": 1, "method": "session/prompt", "params": {"prompt": [{"text": "x"}]}})).await?;
    ensure!(
        peer.exit().await?.0 == Some(2),
        "A4 prompt without default mode"
    );
    for (text, code) in [("CRASH", Some(3)), ("TREE", None), ("INVALID", None)] {
        let mut peer = Peer::spawn(Fake::Acp, &["acp"], workspace.path(), &[]).await?;
        peer.send(json!({"jsonrpc": "2.0", "id": 3, "method": "session/set_mode", "params": {"modeId": "default"}})).await?;
        peer.expect("mode", |v| v["id"] == 3).await?;
        peer.send(json!({"jsonrpc": "2.0", "id": 4, "method": "session/prompt", "params": {"prompt": [{"text": text}]}})).await?;
        match text {
            "CRASH" => {
                peer.expect("partial", |v| {
                    v["params"]["update"]["content"]["text"] == "Partial before crash."
                })
                .await?;
                ensure!(peer.exit().await?.0 == code, "A8 crash exit");
            }
            "INVALID" => {
                peer.expect("bad json", |v| *v == json!("bad json")).await?;
            }
            _ => {
                peer.expect("worker", |v| {
                    v["params"]["update"]["toolCallId"] == "worker"
                })
                .await?;
                let pid = workspace.read("child-pid")?;
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
                ensure!(!stat.contains(") Z "), "A9 detached child not running");
                let _ = Command::new("kill").args(["-TERM", &pid]).status().await;
                tokio::time::sleep(Duration::from_millis(200)).await;
                ensure!(
                    std::path::Path::new(&format!("/proc/{pid}")).exists(),
                    "A9 child must survive TERM"
                );
                let _ = Command::new("kill").args(["-KILL", &pid]).status().await;
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn a13_a14_pi_rpc_and_goal_judge_mode() -> Result<()> {
    let workspace = TempWorkspace::new()?;
    let mut peer = Peer::spawn(Fake::Acp, &["--extension", "x"], workspace.path(), &[]).await?;
    peer.send(json!({"type": "get_state", "id": "s"})).await?;
    let state = peer.expect("state", |v| v["id"] == "s").await?;
    ensure!(
        state["data"]["sessionId"] == "pi-fixture",
        "A13 state: {state}"
    );
    peer.send(json!({"type": "prompt", "id": "p", "message": "run"}))
        .await?;
    peer.expect("ui request", |v| v["type"] == "extension_ui_request")
        .await?;
    peer.send(json!({"type": "extension_ui_response", "id": "pi-approval", "confirmed": false}))
        .await?;
    let end = peer
        .expect("tool end", |v| v["type"] == "tool_execution_end")
        .await?;
    ensure!(end["isError"] == true, "A13 rejected tool: {end}");
    peer.expect("agent end", |v| v["type"] == "agent_end")
        .await?;
    peer.exit().await?;
    let mut peer = Peer::spawn(
        Fake::Acp,
        &["--append-system-prompt", "x"],
        workspace.path(),
        &[],
    )
    .await?;
    peer.send(json!({"type": "get_state", "id": "s"})).await?;
    ensure!(
        peer.exit().await?.0 == Some(1),
        "A13 Pi context without extension accepted"
    );
    let mut judge = Peer::spawn(Fake::Acp, &["acp"], workspace.path(), &[]).await?;
    judge.send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientCapabilities": {"fs": {"readTextFile": false}}}})).await?;
    judge.expect("initialize", |v| v["id"] == 1).await?;
    judge.send(json!({"jsonrpc": "2.0", "id": 2, "method": "session/new", "params": {"mcpServers": []}})).await?;
    ensure!(
        judge.expect("session", |v| v["id"] == 2).await?["result"]["sessionId"] == "judge",
        "A14 judge session"
    );
    judge.send(json!({"jsonrpc": "2.0", "id": 3, "method": "session/prompt", "params": {"sessionId": "judge", "prompt": [{"text": "Judge"}]}})).await?;
    let verdict = judge.expect("verdict", method("session/update")).await?;
    let verdict: Value = serde_json::from_str(
        verdict["params"]["update"]["content"]["text"]
            .as_str()
            .unwrap_or_default(),
    )?;
    ensure!(verdict["met"] == true, "A14 verdict: {verdict}");
    judge.exit().await?;
    let mut leaky = Peer::spawn(Fake::Acp, &["acp"], workspace.path(), &[]).await?;
    leaky.send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientCapabilities": {"fs": {"readTextFile": false}}}})).await?;
    leaky.expect("initialize", |v| v["id"] == 1).await?;
    leaky.send(json!({"jsonrpc": "2.0", "id": 2, "method": "session/new", "params": {"mcpServers": [{"name": "minarbot"}]}})).await?;
    ensure!(
        leaky.exit().await?.0 == Some(1),
        "A14 judge accepted MCP servers"
    );
    Ok(())
}

async fn gateway(
    workspace: &TempWorkspace,
    model: &ScriptedModel,
    mcp: &str,
    extra: &[(&str, &str)],
) -> Result<Peer> {
    let base = model.base_url();
    let mut env = vec![
        ("MINARBOT_GATEWAY_BASE_URL", base.as_str()),
        ("MINARBOT_PROVIDER_KEY", "provider-key"),
        ("MINARBOT_MCP_TOKEN", TOKEN),
    ];
    env.extend_from_slice(extra);
    let mut peer = Peer::spawn(Fake::Gateway, &["app-server"], workspace.path(), &env).await?;
    peer.send(json!({"id": 1, "method": "initialize"})).await?;
    peer.expect("initialize", |v| v["id"] == 1).await?;
    let mut params = json!({"threadId": "thread-1", "cwd": workspace.path(), "model": "work"});
    if !mcp.is_empty() {
        params["config"] = json!({"mcp_servers": {"minarbot": {"url": mcp, "bearer_token_env_var": "MINARBOT_MCP_TOKEN"}}});
    }
    peer.send(json!({"id": 2, "method": "thread/start", "params": params}))
        .await?;
    peer.expect("thread", |v| v["id"] == 2).await?;
    Ok(peer)
}

async fn gateway_turn(peer: &mut Peer, id: u64, text: &str) -> Result<Value> {
    peer.seen.clear();
    peer.send(json!({"id": id, "method": "turn/start", "params": {"threadId": "thread-1", "input": [{"text": text}]}})).await?;
    peer.expect("turn end", |v| {
        v["method"] == "turn/completed" || v["method"] == "error"
    })
    .await
}

#[tokio::test]
async fn g1_to_g7_g12_g13_g14_g17_gateway_tools_streaming_errors_and_normalization() -> Result<()> {
    let workspace = TempWorkspace::new()?;
    ensure!(
        run(Fake::Gateway, &["--version"], workspace.path(), &[])
            .await?
            .1
            .trim()
            == "fake-gateway-harness 1.0.0",
        "G1"
    );
    let state = Arc::new(Mutex::new(Mcp::default()));
    let mcp = mcp_server(state.clone()).await?;
    let model = ScriptedModel::start().await?;
    let mut peer = gateway(
        &workspace,
        &model,
        &mcp,
        &[("MINARBOT_GATEWAY_STREAM", "true")],
    )
    .await?;
    model.push(Reply::tool_call("call-1", "echo", json!({"text": "hi"})));
    model.push(Reply::deltas(["Hel", "lo"]).usage(20, 10));
    let end = gateway_turn(&mut peer, 3, "first").await?;
    ensure!(end["method"] == "turn/completed", "G3 end: {end}");
    let requests = model.requests();
    ensure!(
        requests[0].authorization.as_deref() == Some("Bearer provider-key")
            && requests[0].model() == "work",
        "G17 provider request"
    );
    ensure!(
        requests[0].stream() && requests[0].tool_names().contains(&"echo".to_owned()),
        "G3/G4 tools offered"
    );
    let call = state.lock().map_err(|_| "poisoned")?.calls[0].clone();
    ensure!(
        call["arguments"]["requestId"]
            .as_str()
            .is_some_and(|id| id.len() == 36),
        "G5 requestId filled: {call}"
    );
    ensure!(
        requests[1].tool_results()[0]["content"]
            .as_str()
            .is_some_and(|c| c.contains("\"text\":\"hi\"")),
        "G5 tool result to model"
    );
    let pieces: Vec<&str> = peer
        .seen
        .iter()
        .filter_map(|v| v["params"]["delta"].as_str())
        .collect();
    ensure!(pieces == ["Hel", "lo"], "G4 streamed deltas: {pieces:?}");
    ensure!(
        peer.seen.iter().any(
            |v| v["method"] == "item/completed" && v["params"]["item"]["type"] == "mcpToolCall"
        ),
        "G5 tool item"
    );
    let usage = peer
        .seen
        .iter()
        .find(|v| v["method"] == "thread/tokenUsage/updated")
        .ok_or("G3 usage")?;
    ensure!(
        usage["params"]["tokenUsage"]["last"] == json!({"inputTokens": 20, "outputTokens": 10}),
        "G3 usage: {usage}"
    );
    model.push(Reply::tool("forbidden", json!({})));
    model.push(Reply::text("after error"));
    gateway_turn(&mut peer, 4, "second").await?;
    let last = model.requests().last().cloned().ok_or("request")?;
    ensure!(
        last.tool_results().last().is_some_and(|m| m["content"]
            .as_str()
            .is_some_and(|c| c.contains("Tool not allowed."))),
        "G6 4xx becomes a tool error"
    );
    model.push(Reply::tool("explode", json!({})));
    let error = gateway_turn(&mut peer, 5, "third").await?;
    ensure!(
        error["method"] == "error"
            && error["params"]["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("500")),
        "G6 5xx fails: {error}"
    );
    model.push(Reply::status(500, json!({"error": "down"})));
    let error = gateway_turn(&mut peer, 6, "fourth").await?;
    ensure!(
        error["params"]["error"]["message"] == "Gateway provider returned 500",
        "G10 provider error: {error}"
    );
    model.push(Reply::tool_calls(vec![
        minarbot_testkit::model::ToolCall::new("w", "wait_task", json!({"timeoutSeconds": 99})),
        minarbot_testkit::model::ToolCall::new("d", "delegate_task", json!({"wait": true})),
        minarbot_testkit::model::ToolCall::new("i", "image", json!({})),
    ]));
    model.push(Reply::text("done"));
    gateway_turn(&mut peer, 7, "fifth").await?;
    let calls = state.lock().map_err(|_| "poisoned")?.calls.clone();
    let named = |name: &str| {
        calls
            .iter()
            .filter(|c| c["name"] == name)
            .cloned()
            .collect::<Vec<_>>()
    };
    ensure!(
        named("wait_task")
            .iter()
            .any(|c| c["arguments"]["timeoutSeconds"] == 30.0),
        "G13 wait clamp: {calls:?}"
    );
    ensure!(
        named("wait_task")
            .iter()
            .any(|c| c["arguments"]["taskId"] == "task-1"),
        "G13 delegate wait: {calls:?}"
    );
    let last = model.requests().last().cloned().ok_or("request")?;
    ensure!(
        last.mentions("data:image/png;base64,aGVsbG8="),
        "G14 image forwarded"
    );
    model.push(Reply::tool("ask_user", json!({})));
    model.push(Reply::text("profile"));
    gateway_turn(&mut peer, 8, "Initial name: \"A\"").await?;
    let setup = &model.requests()[model.calls() - 2];
    let mut tools = setup.tool_names();
    tools.sort();
    ensure!(
        tools == ["ask_user", "edit_profile"],
        "G12 setup tools: {tools:?}"
    );
    peer.exit().await?;
    let log = fakes::gateway_log(workspace.path())?;
    ensure!(
        log.iter().any(|row| row["method"] == "turn/start"),
        "G18 log"
    );
    Ok(())
}

#[tokio::test]
async fn g8_g9_g11_g15_g16_gateway_budget_resume_interrupt_judge_and_catalog() -> Result<()> {
    let workspace = TempWorkspace::new()?;
    let state = Arc::new(Mutex::new(Mcp::default()));
    let mcp = mcp_server(state).await?;
    let model = ScriptedModel::start().await?;
    let mut peer = gateway(&workspace, &model, &mcp, &[("MINARBOT_TOOL_BUDGET", "1")]).await?;
    model.respond(|request| {
        if request.mentions("Tools are now disabled") {
            Reply::text("Stopped.")
        } else {
            Reply::tool("echo", json!({}))
        }
    });
    gateway_turn(&mut peer, 3, "loop").await?;
    let last = model.requests().last().cloned().ok_or("request")?;
    ensure!(
        last.tool_names().is_empty() && last.last_text().contains("Stopped after 1 tool steps"),
        "G8 budget: {:?}",
        last.body
    );
    model.respond(|request| Reply::text(format!("seen {}", request.messages().len())));
    gateway_turn(
        &mut peer,
        4,
        "user: earlier\n\nassistant: reply\n\nuser: latest",
    )
    .await?;
    let parsed = model.requests().last().cloned().ok_or("request")?;
    ensure!(
        parsed.texts() == ["earlier", "reply", "latest"],
        "G9 role markers: {:?}",
        parsed.texts()
    );
    peer.exit().await?;
    ensure!(
        workspace.exists(".fake-gateway-thread-1.json"),
        "G9 saved session"
    );
    let mut resumed = gateway(&workspace, &model, &mcp, &[]).await?;
    resumed.send(json!({"id": 5, "method": "thread/resume", "params": {"threadId": "thread-1", "cwd": workspace.path(),
        "config": {"mcp_servers": {"minarbot": {"url": mcp, "bearer_token_env_var": "MINARBOT_MCP_TOKEN"}}}}})).await?;
    resumed.expect("resume", |v| v["id"] == 5).await?;
    gateway_turn(&mut resumed, 6, "after resume").await?;
    let after = model.requests().last().cloned().ok_or("request")?;
    ensure!(
        after.texts().first().map(String::as_str) == Some("earlier")
            && after.last_text() == "after resume",
        "G9 resume history: {:?}",
        after.texts()
    );
    model.respond(|_| Reply::hold());
    resumed.seen.clear();
    resumed.send(json!({"id": 7, "method": "turn/start", "params": {"threadId": "thread-1", "input": [{"text": "hold"}]}})).await?;
    model.wait_calls(model.calls() + 1).await?;
    resumed
        .send(json!({"id": 8, "method": "turn/interrupt", "params": {"threadId": "thread-1"}}))
        .await?;
    resumed.expect("interrupt", |v| v["id"] == 8).await?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    ensure!(
        !resumed
            .seen
            .iter()
            .any(|v| v["method"] == "turn/completed" || v["method"] == "error"),
        "G11 output after interrupt"
    );
    model.catalog(["work", "judge"]);
    resumed
        .send(json!({"id": 9, "method": "model/list", "params": {}}))
        .await?;
    let catalog = resumed.expect("catalog", |v| v["id"] == 9).await?;
    ensure!(
        catalog["result"]["data"][1]["id"] == "judge" && catalog["result"]["nextCursor"].is_null(),
        "G16 catalog: {catalog}"
    );
    resumed.exit().await?;
    let mut judge = gateway(&workspace, &model, "", &[]).await?;
    model.respond(|_| Reply::text("{\"met\":true}"));
    let end = gateway_turn(&mut judge, 3, "Judge whether the target is met.").await?;
    ensure!(end["method"] == "turn/completed", "G15 judge: {end}");
    let request = model.requests().last().cloned().ok_or("request")?;
    ensure!(
        request.body.get("tools").is_none(),
        "G15 judge offered tools"
    );
    Ok(())
}
