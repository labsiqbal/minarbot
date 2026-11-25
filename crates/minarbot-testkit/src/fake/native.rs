//! Native Codex and Claude Code peers (port of `app/fixtures/fake-harness.mjs`).
//!
//! * Codex speaks `app-server` JSON-RPC: `initialize`, `thread/start`,
//!   `thread/resume`, `turn/start`, `turn/interrupt`, server-to-client
//!   approval requests and `item/*`, `thread/tokenUsage/updated` and
//!   `turn/completed` notifications.
//! * Claude speaks Claude Code `stream-json`: `control_request` /
//!   `control_response` (initialize, interrupt, `hook_callback`,
//!   `can_use_tool`), `user` input, and `system`, `stream_event`,
//!   `assistant`, `user` and `result` output.
//!
//! Turns are scripted by keywords in the prompt (see [`run`]). Every peer
//! appends to `fake-log.jsonl` in its working directory, which tests read
//! with [`crate::fakes::native_log`].

use super::{check, cwd, env, log, spawn_idle_child, Args, Out, Result};
use serde_json::{json, Value};
use std::{
    path::Path,
    process::ExitCode,
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

/// Log file in the peer's working directory.
pub const LOG: &str = "fake-log.jsonl";
/// Version line printed for `--version`.
pub const VERSION: &str = "fake-harness 1.0.0";
/// Environment variables that must never reach a harness process.
pub const SCRUBBED: [&str; 6] = [
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "COMPOSIO_API_KEY",
    "MINARBOT_SCRIPT_TOKEN",
    "NODE_OPTIONS",
    "MINARBOT_HARNESS_CANARY",
];
/// The sandboxed check the Claude workspace-shell turn runs through Minarbot's
/// `run_shell` MCP tool.
pub const WORKSPACE_SHELL: &str = "ls -al && for file in *.test.sh; do head -5 \"$file\"; done && sh fixture.test.sh && printf checked > result.txt";
/// The plain shell loop the Claude workspace-shell turn first tries natively.
pub const WORKSPACE_LOOP: &str = "for file in *.test.sh; do head -5 \"$file\"; done";
/// File whose creation releases `FIRST_PARTIAL_GATE` and `STREAM_GATE` turns.
pub const RELEASE: &str = "stream-release";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Codex,
    Claude,
}

struct State {
    session: String,
    prompt: String,
    callback: String,
    stop_callback: String,
    approval_id: String,
    mcp_url: String,
}

struct Peer {
    mode: Mode,
    args: Args,
    out: Out,
    has_resume: bool,
    state: Mutex<State>,
}

type Shared = Arc<Peer>;

fn uuid() -> String {
    Uuid::new_v4().to_string()
}

fn exists(name: &str) -> bool {
    Path::new(name).exists()
}

/// Entry point of `fake-codex` and `fake-claude`.
pub async fn main(mode: Mode, args: Args) -> Result<ExitCode> {
    if args.has("--version") {
        println!("{VERSION}");
        return Ok(ExitCode::SUCCESS);
    }
    // Sign-in status: signed in when the instance home holds the CLI's
    // credential file.
    if args.nth(1) == Some("login") && args.nth(2) == Some("status") {
        let home = std::env::var("CODEX_HOME").unwrap_or_default();
        return Ok(status(Path::new(&home).join("auth.json").exists()));
    }
    if args.nth(1) == Some("auth") && args.nth(2) == Some("status") {
        let home = std::env::var("CLAUDE_CONFIG_DIR")
            .unwrap_or_else(|_| format!("{}/.claude", std::env::var("HOME").unwrap_or_default()));
        return Ok(status(Path::new(&home).join(".credentials.json").exists()));
    }
    for key in SCRUBBED {
        let allowed =
            key == "ANTHROPIC_API_KEY" && std::env::var(key).as_deref() == Ok("minarbot-no-key");
        check(
            std::env::var_os(key).is_none() || allowed,
            format!("{key} leaked"),
        )?;
    }
    let has_resume = args
        .0
        .iter()
        .any(|value| value == "--resume" || value.starts_with("--resume="));
    let session = has_resume
        .then(|| args.value("--resume"))
        .flatten()
        .or_else(|| args.value("--session-id"))
        .unwrap_or_else(uuid);
    let peer: Shared = Arc::new(Peer {
        mode,
        args,
        out: Out::default(),
        has_resume,
        state: Mutex::new(State {
            session,
            prompt: String::new(),
            callback: String::new(),
            stop_callback: String::new(),
            approval_id: String::new(),
            mcp_url: std::env::var("MINARBOT_MCP_URL").unwrap_or_default(),
        }),
    });
    let mut lines = super::stdin_lines();
    while let Some(line) = super::next_line(&mut lines).await? {
        let value: Value = serde_json::from_str(&line)?;
        match mode {
            Mode::Codex => codex_message(&peer, &value)?,
            Mode::Claude => claude_message(&peer, &value).await?,
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn status(ok: bool) -> ExitCode {
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

impl Peer {
    fn with<T>(&self, change: impl FnOnce(&mut State) -> T) -> T {
        match self.state.lock() {
            Ok(mut state) => change(&mut state),
            Err(poisoned) => change(&mut poisoned.into_inner()),
        }
    }

    fn session(&self) -> String {
        self.with(|state| state.session.clone())
    }

    fn prompt(&self) -> String {
        self.with(|state| state.prompt.clone())
    }

    fn send(&self, value: Value) {
        self.out.send(&value);
    }

    /// Codex notification with `threadId` merged into `params`.
    fn notify(&self, method: &str, mut params: Value) {
        params["threadId"] = json!(self.session());
        let mut ordered = json!({"threadId": self.session()});
        if let (Value::Object(target), Value::Object(source)) = (&mut ordered, params) {
            target.extend(source);
        }
        self.send(json!({"method": method, "params": ordered}));
    }

    fn delta(&self, text: &str) {
        match self.mode {
            Mode::Codex => self.notify("item/agentMessage/delta", json!({"delta": text})),
            Mode::Claude => self.send(json!({
                "type": "stream_event", "session_id": self.session(), "uuid": uuid(),
                "parent_tool_use_id": null,
                "event": {"type": "content_block_delta", "index": 0,
                    "delta": {"type": "text_delta", "text": text}},
            })),
        }
    }

    fn pre_tool_use(&self, id: &str, name: &str, input: Value) {
        let callback = self.with(|state| state.callback.clone());
        self.send(json!({
            "type": "control_request", "request_id": id,
            "request": {"subtype": "hook_callback", "callback_id": callback, "input": {
                "session_id": self.session(), "cwd": cwd(), "hook_event_name": "PreToolUse",
                "tool_name": name, "tool_input": input, "tool_use_id": id,
            }},
        }));
    }

    fn log_turn(&self, prompt: &str) {
        log(
            LOG,
            &json!({
                "type": "turn", "session": self.session(), "prompt": prompt,
                "home": std::env::var("HOME").ok(),
                "claudeConfigDir": std::env::var("CLAUDE_CONFIG_DIR").ok(),
            }),
        );
    }

    fn finish(&self, allowed: bool) {
        let prompt = self.prompt();
        let checked = prompt.contains("NATIVE_CHECK");
        self.project_environment(&prompt);
        match self.mode {
            Mode::Codex => {
                if checked {
                    self.notify(
                        "item/completed",
                        json!({"item": {"id": "edit", "type": "fileChange", "status": "completed"}}),
                    );
                }
                let mut item = json!({
                    "id": "shell", "type": "commandExecution",
                    "command": if checked { "node --test fixture.test.mjs" } else { "echo hello" },
                    "status": if allowed { "completed" } else { "declined" },
                });
                if checked {
                    item["aggregatedOutput"] = json!("Native check passed.");
                    item["exitCode"] = json!(0);
                }
                self.notify("item/completed", json!({"item": item}));
                self.delta(if allowed { "finished" } else { "denied" });
                if prompt.contains("FINAL_SECRET") {
                    self.delta(" test-mcp-session-");
                    self.delta("token");
                }
                if prompt.contains("FINAL_PROVIDER_SECRET") {
                    if let Some(key) = env("MINARBOT_PROVIDER_KEY") {
                        let split = key.char_indices().nth(8).map_or(key.len(), |(at, _)| at);
                        self.delta(&key[..split]);
                        self.delta(&key[split..]);
                    }
                }
                self.notify(
                    "thread/tokenUsage/updated",
                    json!({"tokenUsage": {"last": {"inputTokens": 11, "outputTokens": 7}}}),
                );
                self.notify(
                    "turn/completed",
                    json!({"turn": {"id": "fake-turn", "status": "completed"}}),
                );
            }
            Mode::Claude => {
                let session = self.session();
                if checked {
                    self.send(
                        json!({"type": "assistant", "message": {"id": "edit", "content": [
                        {"type": "tool_use", "id": "edit", "name": "Write",
                         "input": {"file_path": "result.txt", "content": "checked"}}]}}),
                    );
                    self.send(json!({"type": "user", "message": {"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": "edit", "content": "File saved."}]}}));
                }
                if !prompt.contains("WORKSPACE_SHELL_FIXTURE") {
                    let command = if checked {
                        "node --test fixture.test.mjs"
                    } else {
                        "echo hello"
                    };
                    self.send(json!({
                        "type": "assistant", "uuid": uuid(), "session_id": session,
                        "parent_tool_use_id": null,
                        "message": {"id": "m1", "role": "assistant", "type": "message",
                            "content": [{"type": "tool_use", "id": "shell", "name": "Bash",
                                "input": {"command": command}}],
                            "model": "fake", "stop_reason": "tool_use", "stop_sequence": null,
                            "usage": {"input_tokens": 11, "output_tokens": 7}},
                    }));
                    self.send(json!({
                        "type": "user", "uuid": uuid(), "session_id": session,
                        "parent_tool_use_id": null,
                        "message": {"role": "user", "content": [{"type": "tool_result",
                            "tool_use_id": "shell", "content": "fake result", "is_error": !allowed}]},
                    }));
                }
                self.delta(if allowed { "finished" } else { "denied" });
                if prompt.contains("FINAL_SECRET") {
                    self.delta(" test-mcp-session-");
                    self.delta("token");
                }
                self.send(result_message(&session, "fake result", 11, 7));
            }
        }
    }
}

impl Peer {
    /// `ENV_PRESENT` / `ENV_ABSENT` in the latest user message (port of the
    /// `project-integrations` fixture patch): `PROJECT_TEST_TOKEN` must be set
    /// (and visible to a shell command, whose output is streamed as two
    /// deltas) or must be absent; otherwise the peer exits 1.
    fn project_environment(&self, prompt: &str) {
        let latest = prompt
            .rfind("\"role\":\"user\"")
            .or_else(|| prompt.rfind("user:"))
            .map_or(prompt, |at| &prompt[at..]);
        let value = std::env::var("PROJECT_TEST_TOKEN").ok();
        if latest.contains("ENV_PRESENT") {
            let output = std::process::Command::new("/bin/sh")
                .args(["-c", "printf %s \"$PROJECT_TEST_TOKEN\""])
                .output()
                .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
                .unwrap_or_default();
            if value.as_deref().is_none_or(str::is_empty)
                || value.as_deref() != Some(output.as_str())
            {
                eprintln!("fake harness: PROJECT_TEST_TOKEN missing from the project shell");
                std::process::exit(1);
            }
            let split = output
                .char_indices()
                .nth(8)
                .map_or(output.len(), |(at, _)| at);
            self.delta(&output[..split]);
            self.delta(&output[split..]);
        } else if latest.contains("ENV_ABSENT") && value.is_some() {
            eprintln!("fake harness: PROJECT_TEST_TOKEN leaked outside its project");
            std::process::exit(1);
        }
    }
}

fn result_message(session: &str, text: &str, input: u64, output: u64) -> Value {
    json!({
        "type": "result", "subtype": "success", "is_error": false, "result": text,
        "session_id": session, "uuid": uuid(), "duration_ms": 5, "duration_api_ms": 5,
        "num_turns": 1, "total_cost_usd": 0,
        "usage": {"input_tokens": input, "output_tokens": output,
            "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0},
        "modelUsage": {}, "permission_denials": [],
    })
}

fn later(peer: &Shared, delay: Duration, action: impl FnOnce(&Shared) + Send + 'static) {
    let peer = peer.clone();
    super::spawn(async move {
        tokio::time::sleep(delay).await;
        action(&peer);
    });
}

async fn released() {
    while !exists(RELEASE) {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Script one turn from its prompt.
///
/// | Keyword | Codex | Claude |
/// |---|---|---|
/// | `STREAM_GATE` | text, tool, gated draft, final answer | same in stream-json |
/// | `STREAM_CRASH` | gated draft, buffered tail, exit 19 | same |
/// | `WORKSPACE_SHELL_FIXTURE` | - | native `Bash` denied, `run_shell` via MCP stdio |
/// | `FIRST_PARTIAL_GATE` | `Harness ` then wait for `stream-release` | same |
/// | `Create a deliverable.` | - | `Stop` hook; reminder runs a second turn |
/// | `NATIVE_SETUP_FIXTURE` + `Initial needs:` | MCP `create_text_file` (403) and `edit_profile` | same |
/// | `CRASH` | exit 19 | exit 19 |
/// | `OVERSIZE` | 1 MiB + 1 byte without newline | same |
/// | `HOLD` (without `RESTART_FINISH`) | spawn a descendant and wait | same |
/// | `NETWORK`, `OUTSIDE_FILE`, `READ_ONLY_WRITE` | permission / file approval | `PreToolUse` hook |
/// | `WORKSPACE_WRITE` | finishes | `PreToolUse` hook for `Write` |
/// | `APPROVE`, `APPROVE_SECRET` | command approval | `PreToolUse` hook for `Bash` |
/// | `NATIVE_CHECK` | file change plus check receipt | `Write` plus check `Bash` |
/// | `FINAL_SECRET`, `FINAL_PROVIDER_SECRET` | split secret in final text | `FINAL_SECRET` |
/// | anything else | shell item and `finished` after 100 ms | same |
fn run(peer: &Shared, text: String) {
    if text.contains("STREAM_GATE") || text.contains("STREAM_CRASH") {
        stream_gate(peer, text);
        return;
    }
    peer.with(|state| state.prompt = text.clone());
    peer.log_turn(&text);
    if peer.mode == Mode::Claude && text.contains("WORKSPACE_SHELL_FIXTURE") {
        peer.pre_tool_use(
            "workspace-ls",
            "Bash",
            json!({"command": "ls -al", "description": "Inspect workspace"}),
        );
        return;
    }
    peer.delta("Harness ");
    if text.contains("FIRST_PARTIAL_GATE") {
        let peer = peer.clone();
        super::spawn(async move {
            released().await;
            run_rest(&peer, &text);
        });
        return;
    }
    run_rest(peer, &text);
}

fn run_rest(peer: &Shared, text: &str) {
    let session = peer.session();
    if peer.mode == Mode::Claude && text.contains("Create a deliverable.") {
        let callback = peer.with(|state| state.stop_callback.clone());
        peer.send(json!({
            "type": "control_request", "request_id": "sv-stop",
            "request": {"subtype": "hook_callback", "callback_id": callback, "input": {
                "session_id": session, "transcript_path": "", "cwd": cwd(),
                "hook_event_name": "Stop", "stop_hook_active": false,
                "last_assistant_message": "Done.",
            }},
        }));
        return;
    }
    if text.contains("NATIVE_SETUP_FIXTURE") && text.contains("Initial needs:") {
        let peer = peer.clone();
        super::spawn(async move {
            if let Err(error) = native_setup(&peer).await {
                log(LOG, &json!({"setupError": error.to_string()}));
                std::process::exit(1);
            }
        });
        return;
    }
    if text.contains("CRASH") {
        later(peer, Duration::from_millis(20), |_| std::process::exit(19));
        return;
    }
    if text.contains("OVERSIZE") {
        peer.out.raw(&vec![b'x'; 1024 * 1024 + 1]);
        return;
    }
    if text.contains("HOLD") && !text.contains("RESTART_FINISH") {
        let pid = spawn_idle_child(false, false).unwrap_or(0);
        log(LOG, &json!({"type": "descendant", "pid": pid}));
        return;
    }
    match peer.mode {
        Mode::Codex => {
            peer.notify(
                "item/started",
                json!({"item": {"id": "shell", "type": "commandExecution", "command": "echo hello"}}),
            );
            let network = text.contains("NETWORK");
            if network || text.contains("OUTSIDE_FILE") || text.contains("READ_ONLY_WRITE") {
                peer.with(|state| state.approval_id = "approval-native".into());
                let mut params = json!({"threadId": session, "turnId": "fake-turn",
                    "itemId": "shell", "reason": "Allow requested access?"});
                if network {
                    params["permissions"] = json!({"network": {"enabled": true}});
                } else {
                    params["grantRoot"] = json!(if text.contains("OUTSIDE_FILE") {
                        "/outside".to_owned()
                    } else {
                        cwd()
                    });
                }
                let method = if network {
                    "item/permissions/requestApproval"
                } else {
                    "item/fileChange/requestApproval"
                };
                peer.send(json!({"id": "approval-native", "method": method, "params": params}));
                return;
            }
            if text.contains("APPROVE") {
                peer.with(|state| state.approval_id = "approval-native".into());
                let command = if text.contains("APPROVE_SECRET") {
                    "echo test-mcp-session-token"
                } else {
                    "echo hello"
                };
                peer.send(json!({"id": "approval-native",
                    "method": "item/commandExecution/requestApproval",
                    "params": {"threadId": session, "turnId": "fake-turn", "itemId": "shell",
                        "command": command, "reason": "Run command outside sandbox?", "cwd": cwd()}}));
                return;
            }
        }
        Mode::Claude => {
            let network = text.contains("NETWORK");
            let file = ["OUTSIDE_FILE", "WORKSPACE_WRITE", "READ_ONLY_WRITE"]
                .iter()
                .any(|keyword| text.contains(keyword));
            if network || file || text.contains("APPROVE") {
                peer.with(|state| state.approval_id = "approval-native".into());
                let (tool, input) = if network {
                    ("WebFetch", json!({"url": "https://example.invalid/"}))
                } else if file {
                    let path = if text.contains("OUTSIDE_FILE") {
                        "/outside/hello.txt".to_owned()
                    } else {
                        format!("{}/hello.txt", cwd())
                    };
                    ("Write", json!({"file_path": path, "content": "hello"}))
                } else {
                    let command = if text.contains("APPROVE_SECRET") {
                        "echo test-mcp-session-token"
                    } else {
                        "echo hello"
                    };
                    (
                        "Bash",
                        json!({"dangerouslyDisableSandbox": true, "command": command}),
                    )
                };
                let callback = peer.with(|state| state.callback.clone());
                peer.send(json!({
                    "type": "control_request", "request_id": "approval-native",
                    "request": {"subtype": "hook_callback", "callback_id": callback, "input": {
                        "session_id": session, "transcript_path": "", "cwd": cwd(),
                        "hook_event_name": "PreToolUse", "tool_name": tool,
                        "tool_input": input, "tool_use_id": "shell",
                    }},
                }));
                return;
            }
        }
    }
    later(peer, Duration::from_millis(100), |peer| peer.finish(true));
}

async fn native_setup(peer: &Shared) -> Result<()> {
    let url = peer.with(|state| state.mcp_url.clone());
    let token = std::env::var("MINARBOT_MCP_TOKEN").unwrap_or_default();
    let client = reqwest::Client::builder().no_proxy().build()?;
    let call = |name: &'static str, arguments: Value| {
        let request = client
            .post(&url)
            .bearer_auth(&token)
            .header("Accept", "application/json, text/event-stream")
            .json(
                &json!({"jsonrpc": "2.0", "id": uuid(), "method": "tools/call",
                "params": {"name": name, "arguments": arguments}}),
            );
        async move {
            let response = request.send().await?;
            let status = response.status().as_u16();
            let body: Value = response.json().await?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>((status, body))
        }
    };
    let (status, _) = call(
        "create_text_file",
        json!({"name": "forbidden", "text": "x"}),
    )
    .await?;
    check(
        status == 403,
        format!("create_text_file returned {status}, expected 403"),
    )?;
    let (_, body) = call(
        "edit_profile",
        json!({"revision": 1, "name": "Native assistant", "instructions": "Native setup saved."}),
    )
    .await?;
    check(!body["result"].is_null(), body.to_string())?;
    check(
        body["result"]["structuredContent"]["revision"] == 2,
        body.to_string(),
    )?;
    peer.finish(true);
    Ok(())
}

fn stream_gate(peer: &Shared, text: String) {
    log(
        LOG,
        &json!({"type": "turn", "session": peer.session(), "prompt": text}),
    );
    let start = |id: &str| {
        if peer.mode == Mode::Claude {
            peer.send(
                json!({"type": "stream_event", "session_id": peer.session(), "uuid": uuid(),
                "parent_tool_use_id": null, "event": {"type": "message_start", "message": {
                    "id": id, "type": "message", "role": "assistant", "content": [],
                    "model": "fake", "stop_reason": null, "stop_sequence": null,
                    "usage": {"input_tokens": 1, "output_tokens": 0}}}}),
            );
        }
    };
    let emit = |id: &str, value: &str| match peer.mode {
        Mode::Codex => peer.notify(
            "item/agentMessage/delta",
            json!({"itemId": id, "delta": value}),
        ),
        Mode::Claude => peer.delta(value),
    };
    start("before");
    emit("before", "Before tool. ");
    for _ in 0..100 {
        emit("before", "🧪");
    }
    emit("before", "test-mcp-session-");
    emit("before", "token");
    match peer.mode {
        Mode::Codex => peer.notify(
            "item/started",
            json!({"item": {"id": "gate-tool", "type": "commandExecution", "command": "echo gate"}}),
        ),
        Mode::Claude => peer.send(json!({"type": "assistant", "uuid": uuid(),
            "session_id": peer.session(), "parent_tool_use_id": null,
            "message": {"id": "before", "role": "assistant", "type": "message",
                "content": [{"type": "tool_use", "id": "gate-tool", "name": "Read",
                    "input": {"file_path": "fixture.txt"}}],
                "model": "fake", "stop_reason": "tool_use", "stop_sequence": null,
                "usage": {"input_tokens": 1, "output_tokens": 1}}})),
    }
    start("answer");
    emit("answer", "Draft ");
    for _ in 0..100 {
        emit("answer", "x");
    }
    log(LOG, &json!({"type": "stream-gate"}));
    let peer = peer.clone();
    super::spawn(async move {
        released().await;
        let session = peer.session();
        if text.contains("STREAM_CRASH") {
            match peer.mode {
                Mode::Codex => peer.notify(
                    "item/agentMessage/delta",
                    json!({"itemId": "answer", "delta": " buffered tail"}),
                ),
                Mode::Claude => peer.delta(" buffered tail"),
            }
            std::process::exit(19);
        }
        match peer.mode {
            Mode::Codex => {
                peer.notify(
                    "item/completed",
                    json!({"item": {"id": "gate-tool",
                    "type": "commandExecution", "command": "echo gate", "status": "completed"}}),
                );
                peer.notify(
                    "item/completed",
                    json!({"item": {"id": "answer",
                    "type": "agentMessage", "text": "Final answer."}}),
                );
                peer.notify(
                    "turn/completed",
                    json!({"turn": {"id": "fake-turn", "status": "completed"}}),
                );
            }
            Mode::Claude => {
                peer.send(
                    json!({"type": "user", "uuid": uuid(), "session_id": session,
                    "parent_tool_use_id": null, "message": {"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": "gate-tool", "content": "Done"}]}}),
                );
                peer.send(
                    json!({"type": "assistant", "uuid": uuid(), "session_id": session,
                    "parent_tool_use_id": null, "message": {"id": "answer", "role": "assistant",
                        "type": "message", "content": [{"type": "text", "text": "Final answer."}],
                        "model": "fake", "stop_reason": "end_turn", "stop_sequence": null,
                        "usage": {"input_tokens": 1, "output_tokens": 1}}}),
                );
                peer.send(result_message(&session, "Final answer.", 1, 1));
            }
        }
    });
}

fn codex_message(peer: &Shared, value: &Value) -> Result<()> {
    let method = value["method"].as_str();
    log(
        LOG,
        &json!({"method": method.unwrap_or("response"),
            "home": std::env::var("HOME").ok(), "codexHome": std::env::var("CODEX_HOME").ok()}),
    );
    let id = &value["id"];
    let params = &value["params"];
    match method {
        Some("initialize") => peer.send(json!({"id": id, "result": {"userAgent": "fake"}})),
        Some("thread/start" | "thread/resume") => {
            let config = &params["config"];
            let server = &config["mcp_servers"]["minarbot"];
            let url = server["url"].as_str().unwrap_or_default();
            check(
                params["cwd"] == cwd(),
                format!("thread cwd {} is not {}", params["cwd"], cwd()),
            )?;
            check(mcp_url(url), format!("MCP URL {url} is not loopback /mcp"))?;
            check(
                server["bearer_token_env_var"] == "MINARBOT_MCP_TOKEN",
                "MCP bearer token must come from MINARBOT_MCP_TOKEN",
            )?;
            let token = env("MINARBOT_MCP_TOKEN");
            check(token.is_some(), "MINARBOT_MCP_TOKEN missing")?;
            check(
                params["approvalPolicy"] == "on-request",
                "approvalPolicy must be on-request",
            )?;
            check(
                config["approvals_reviewer"] == "user",
                "approvals_reviewer must be user",
            )?;
            check(
                config["shell_environment_policy.inherit"] == "none",
                "shell environment must not inherit",
            )?;
            let set = config
                .get("shell_environment_policy.set")
                .ok_or("shell_environment_policy.set missing")?
                .to_string();
            check(
                !set.contains(token.as_deref().unwrap_or_default()),
                "MCP token leaked into the shell environment",
            )?;
            if let Some(key) = env("MINARBOT_PROVIDER_KEY") {
                check(
                    !set.contains(&key),
                    "provider key leaked into the shell environment",
                )?;
            }
            peer.with(|state| {
                state.mcp_url = url.to_owned();
                if let Some(thread) = params["threadId"].as_str() {
                    state.session = thread.to_owned();
                }
            });
            peer.send(json!({"id": id, "result": {"thread": {"id": peer.session()}}}));
        }
        Some("turn/start") => {
            match params.get("additionalContext") {
                Some(context) => log(LOG, &json!({"context": context})),
                None => log(LOG, &json!({})),
            }
            check(
                params["threadId"] == peer.session(),
                "turn/start threadId does not match the session",
            )?;
            let sandbox = params
                .get("sandboxPolicy")
                .ok_or("turn/start without sandboxPolicy")?;
            check(
                matches!(sandbox["networkAccess"], Value::Null | Value::Bool(false)),
                "sandbox must not allow network",
            )?;
            if sandbox["type"] == "workspaceWrite" {
                check(
                    sandbox["writableRoots"] == json!([cwd()]),
                    format!("writableRoots {} must be [cwd]", sandbox["writableRoots"]),
                )?;
                check(
                    sandbox["excludeTmpdirEnvVar"] == true,
                    "excludeTmpdirEnvVar must be true",
                )?;
                check(
                    sandbox["excludeSlashTmp"] == true,
                    "excludeSlashTmp must be true",
                )?;
            }
            peer.send(
                json!({"id": id, "result": {"turn": {"id": "fake-turn", "status": "inProgress"}}}),
            );
            peer.notify("turn/started", json!({"turn": {"id": "fake-turn"}}));
            let text = params["input"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            run(peer, text);
        }
        _ => {}
    }
    let approval = peer.with(|state| state.approval_id.clone());
    if !approval.is_empty() && *id == approval && value.get("result").is_some_and(|r| !r.is_null())
    {
        let result = &value["result"];
        log(LOG, &json!({"decision": result["decision"]}));
        let allowed = result["decision"] == "accept"
            || !result["permissions"]["network"].is_null()
                && result["permissions"]["network"] != false;
        peer.finish(allowed);
    }
    if method == Some("turn/interrupt") {
        log(LOG, &json!({"interrupted": true}));
        peer.send(json!({"id": id, "result": {}}));
    }
    Ok(())
}

fn mcp_url(url: &str) -> bool {
    url.strip_prefix("http://127.0.0.1:")
        .and_then(|rest| rest.strip_suffix("/mcp"))
        .is_some_and(|port| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()))
}

fn control_success(peer: &Shared, request_id: &Value, response: Value) {
    peer.send(json!({"type": "control_response", "response": {
        "subtype": "success", "request_id": request_id, "response": response}}));
}

fn mentions_run_shell(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|text| text.contains("mcp__minarbot__run_shell"))
}

async fn claude_message(peer: &Shared, value: &Value) -> Result<()> {
    if value["type"] == "control_request" {
        let request = &value["request"];
        let request_id = &value["request_id"];
        match request["subtype"].as_str() {
            Some("initialize") => {
                let first = |name: &str| {
                    request["hooks"][name][0]["hookCallbackIds"][0]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned()
                };
                let (callback, stop) = (first("PreToolUse"), first("Stop"));
                peer.with(|state| {
                    state.callback = callback.clone();
                    state.stop_callback = stop;
                });
                let context = peer
                    .args
                    .value("--append-system-prompt")
                    .map(Value::from)
                    .or_else(|| request.get("appendSystemPrompt").cloned());
                let mut entry = json!({"type": "init", "resume": peer.has_resume, "callback": !callback.is_empty()});
                if let Some(context) = context {
                    entry["context"] = context;
                }
                log(LOG, &entry);
                let raw = peer
                    .args
                    .value("--mcp-config")
                    .ok_or("--mcp-config missing")?;
                let config: Value = serde_json::from_str(&raw)?;
                check(
                    config["mcpServers"]["minarbot"]["type"] == "stdio",
                    "Minarbot MCP server must be stdio",
                )?;
                let token = env("MINARBOT_MCP_TOKEN").ok_or("MINARBOT_MCP_TOKEN missing")?;
                check(
                    !config.to_string().contains(&token),
                    "MCP token leaked into --mcp-config",
                )?;
                control_success(
                    peer,
                    request_id,
                    json!({"commands": [], "models": [], "account": {},
                        "hook_on_failure_supported": true, "hooks_applied": true}),
                );
            }
            Some("interrupt") => {
                log(LOG, &json!({"interrupted": true}));
                control_success(peer, request_id, json!({}));
            }
            _ => control_success(peer, request_id, json!({})),
        }
    }
    if value["type"] == "user" {
        peer.send(
            json!({"type": "system", "subtype": "init", "session_id": peer.session(),
            "uuid": uuid(), "cwd": cwd(), "tools": [], "mcp_servers": [], "model": "fake",
            "permissionMode": "default", "apiKeySource": "none", "slash_commands": []}),
        );
        let content = &value["message"]["content"];
        let text = match content {
            Value::String(text) => text.clone(),
            Value::Array(blocks) => blocks
                .iter()
                .map(|block| block["text"].as_str().unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        run(peer, text);
    }
    if value["type"] != "control_response" {
        return Ok(());
    }
    let response = &value["response"];
    let inner = &response["response"];
    match response["request_id"].as_str() {
        Some("sv-stop") => {
            log(
                LOG,
                &json!({"type": "stop-hook", "decision": inner["decision"]}),
            );
            run(
                peer,
                inner["reason"]
                    .as_str()
                    .unwrap_or("No reminder.")
                    .to_owned(),
            );
        }
        Some("workspace-ls") => {
            let decision = &inner["hookSpecificOutput"];
            check(
                decision["permissionDecision"] == "deny",
                "native ls must be denied",
            )?;
            check(
                mentions_run_shell(&decision["permissionDecisionReason"]),
                "deny reason must name mcp__minarbot__run_shell",
            )?;
            peer.send(
                json!({"type": "control_request", "request_id": "workspace-loop",
                "request": {"subtype": "can_use_tool", "tool_name": "Bash",
                    "input": {"command": WORKSPACE_LOOP}}}),
            );
        }
        Some("workspace-loop") => {
            check(inner["behavior"] == "deny", "native loop must be denied")?;
            check(
                mentions_run_shell(&inner["message"]),
                "deny message must name mcp__minarbot__run_shell",
            )?;
            peer.pre_tool_use(
                "workspace-sandbox",
                "mcp__minarbot__run_shell",
                json!({"command": WORKSPACE_SHELL, "check": true}),
            );
        }
        Some("workspace-sandbox") => {
            check(
                inner["hookSpecificOutput"]["permissionDecision"] == "allow",
                "run_shell must be allowed",
            )?;
            let raw = peer
                .args
                .value("--mcp-config")
                .ok_or("--mcp-config missing")?;
            tokio::task::spawn_blocking(move || run_workspace_shell(&raw)).await??;
            peer.finish(true);
        }
        Some(other) => {
            let approval = peer.with(|state| state.approval_id.clone());
            if !approval.is_empty() && other == approval {
                let decision = &inner["hookSpecificOutput"]["permissionDecision"];
                log(LOG, &json!({"decision": decision}));
                peer.finish(*decision == "allow");
            }
        }
        None => {}
    }
    Ok(())
}

/// Call `run_shell` through the stdio MCP server named in `--mcp-config`.
fn run_workspace_shell(raw_config: &str) -> Result<()> {
    use std::io::Write;
    let config: Value = serde_json::from_str(raw_config)?;
    let server = &config["mcpServers"]["minarbot"];
    let command = server["command"]
        .as_str()
        .ok_or("MCP stdio command missing")?;
    let args: Vec<String> = server["args"]
        .as_array()
        .map(|args| {
            args.iter()
                .filter_map(|arg| arg.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let mut child = std::process::Command::new(command)
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "run_shell", "arguments": {"command": WORKSPACE_SHELL, "check": true}}});
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(format!("{request}\n").as_bytes())?;
    }
    // Node allowed 5 s. A test-mode approval (B8 in docs/test-ledger.md)
    // can hold the call while the test answers it, so wait up to 30 s.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            return Err("run_shell MCP call timed out".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        use std::io::Read;
        stdout.read_to_string(&mut output)?;
    }
    check(
        status.success(),
        format!("MCP stdio exited with {status}: {output}"),
    )?;
    let receipt: Value = serde_json::from_str(output.trim())?;
    let receipt = &receipt["result"];
    check(
        receipt.get("isError").is_none(),
        format!("run_shell failed: {receipt}"),
    )?;
    check(
        receipt["structuredContent"]["exitCode"] == 0,
        format!("run_shell exit code: {receipt}"),
    )?;
    Ok(())
}
