//! Agent Client Protocol peer (port of `app/fixtures/fake-acp.mjs`, plus the
//! inline goal-judge fixture of `app/goals.test.ts`).
//!
//! JSON-RPC 2.0 over stdio: `initialize`, `authenticate`, `session/new`,
//! `session/load` (replays one historic chunk that must not reach the
//! transcript), `session/set_mode`, `session/set_model`, `session/prompt`,
//! `session/cancel`, and client requests `session/request_permission`,
//! `fs/write_text_file`, `fs/read_text_file`. Lines with a `type` field are
//! Pi RPC (`prompt`, `get_state`, `get_available_models`,
//! `extension_ui_response`). Every input line is appended to
//! `fake-acp-log.jsonl` together with the leaked canary, if any.
//!
//! Prompt keywords: `CRASH`, `INVALID`, `TREE`, `WAIT`, `FILES`, `READONLY`.
//! A client that initializes with `fs.readTextFile = false` is a goal judge:
//! its session is `judge`, it must not receive MCP servers, and it answers
//! with a `met` verdict; work prompts containing `ACP_READY` answer
//! `ACP_READY`.

use super::{log, spawn_idle_child, Args, Out, Result};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    process::ExitCode,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::oneshot;

/// Log file in the peer's working directory.
pub const LOG: &str = "fake-acp-log.jsonl";
pub const SESSION: &str = "fixture-session";

#[derive(Default)]
struct State {
    prompt: Option<Value>,
    request: i64,
    mode: Option<String>,
    judge: bool,
    waiting: HashMap<i64, oneshot::Sender<Value>>,
}

#[derive(Clone)]
struct Peer {
    out: Out,
    state: Arc<Mutex<State>>,
}

impl Peer {
    fn with<T>(&self, change: impl FnOnce(&mut State) -> T) -> T {
        match self.state.lock() {
            Ok(mut state) => change(&mut state),
            Err(poisoned) => change(&mut poisoned.into_inner()),
        }
    }

    fn send(&self, mut value: Value) {
        let mut ordered = json!({"jsonrpc": "2.0"});
        if let (Value::Object(target), Value::Object(source)) = (&mut ordered, value.take()) {
            target.extend(source);
        }
        self.out.send(&ordered);
    }

    fn reply(&self, id: &Value, result: Value) {
        self.send(json!({"id": id, "result": result}));
    }

    fn session(&self) -> &'static str {
        if self.with(|state| state.judge) {
            "judge"
        } else {
            SESSION
        }
    }

    fn update(&self, update: Value) {
        self.send(json!({"method": "session/update",
            "params": {"sessionId": self.session(), "update": update}}));
    }

    fn chunk(&self, text: &str) {
        self.update(json!({"sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": text}}));
    }

    async fn ask(&self, method: &str, mut params: Value) -> Value {
        let (sender, receiver) = oneshot::channel();
        let id = self.with(|state| {
            state.request += 1;
            state.waiting.insert(state.request, sender);
            state.request
        });
        params["sessionId"] = json!(self.session());
        self.send(json!({"id": id, "method": method, "params": params}));
        receiver.await.unwrap_or(Value::Null)
    }
}

/// Entry point of `fake-acp`.
pub async fn main(args: Args) -> Result<ExitCode> {
    if args.has("--help") {
        println!("Commands:\n  acp Start ACP");
        return Ok(ExitCode::SUCCESS);
    }
    if args.has("--version") {
        println!("fake-acp 1.0.0");
        return Ok(ExitCode::SUCCESS);
    }
    let peer = Peer {
        out: Out::default(),
        state: Arc::new(Mutex::new(State {
            request: 100,
            ..State::default()
        })),
    };
    let mut lines = super::stdin_lines();
    while let Some(line) = super::next_line(&mut lines).await? {
        let value: Value = serde_json::from_str(&line)?;
        let mut entry = value.clone();
        if let (Value::Object(fields), Some(canary)) =
            (&mut entry, std::env::var("MINARBOT_HARNESS_CANARY").ok())
        {
            fields.insert("canary".into(), json!(canary));
        }
        log(LOG, &entry);
        if value.get("type").is_some_and(|kind| !kind.is_null()) {
            pi(&peer, &args, &value)?;
            continue;
        }
        let id = &value["id"];
        let Some(method) = value["method"].as_str() else {
            if let Some(id) = id.as_i64() {
                if let Some(sender) = peer.with(|state| state.waiting.remove(&id)) {
                    let _ = sender.send(value.clone());
                }
            }
            continue;
        };
        match method {
            "initialize" => {
                let judge = value["params"]["clientCapabilities"]["fs"]["readTextFile"] == false;
                peer.with(|state| state.judge = judge);
                peer.reply(id, json!({"protocolVersion": 1,
                    "agentInfo": {"name": "fake", "version": "1"},
                    "authMethods": [{"id": "oauth-personal", "type": "agent"}],
                    "agentCapabilities": {"loadSession": true, "mcpCapabilities": {"http": true}},
                    "models": {"availableModels": [{"modelId": "fixture-model", "name": "Fixture model"}]}}));
            }
            "session/new" | "session/load" => {
                let judge = peer.with(|state| state.judge);
                let servers = value["params"]["mcpServers"].as_array().map_or(0, Vec::len);
                if judge && servers > 0 {
                    return Err("Judge exposed MCP".into());
                }
                if method == "session/load" {
                    peer.chunk("Historic text must not replay.");
                }
                peer.reply(id, json!({"sessionId": peer.session(),
                    "modes": {"availableModes": [{"id": "default", "name": "Supervised"}]},
                    "models": {"currentModelId": "fixture-model",
                        "availableModels": [{"modelId": "fixture-model", "name": "Fixture model"}]}}));
            }
            "session/set_mode" => {
                let mode = value["params"]["modeId"].as_str().map(str::to_owned);
                peer.with(|state| state.mode = mode);
                peer.reply(id, json!({}));
            }
            "authenticate" | "session/set_model" => peer.reply(id, json!({})),
            "session/cancel" => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                eprintln!(
                    "ACP cancelled {}",
                    std::env::var("MINARBOT_MCP_TOKEN").unwrap_or_else(|_| "undefined".into())
                );
                let _ = std::fs::write("cancel-received", "yes");
                if let Some(prompt) = peer.with(|state| state.prompt.clone()) {
                    peer.reply(&prompt, json!({"stopReason": "cancelled"}));
                }
            }
            "session/prompt" => prompt(&peer, &value)?,
            _ => {
                if !id.is_null() {
                    peer.send(
                        json!({"id": id, "error": {"code": -32601, "message": "unsupported"}}),
                    );
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn pi(peer: &Peer, args: &Args, value: &Value) -> Result<()> {
    let kind = value["type"].as_str().unwrap_or_default();
    let id = &value["id"];
    match kind {
        "extension_ui_response" => {
            let confirmed = value["confirmed"] == true;
            peer.send(
                json!({"type": "tool_execution_end", "toolCallId": "pi-tool",
                "toolName": "bash", "isError": !confirmed}),
            );
            peer.send(json!({"type": "message_update",
                "assistantMessageEvent": {"type": "text_delta", "delta": "Pi answer."}}));
            peer.send(json!({"type": "agent_end"}));
        }
        "prompt" => {
            peer.send(json!({"type": "response", "id": id, "success": true, "data": {}}));
            peer.send(json!({"type": "message_start", "message": {"role": "assistant"}}));
            let message = value["message"].as_str().unwrap_or_default();
            if message.contains("WAIT") {
                peer.send(json!({"type": "message_update",
                    "assistantMessageEvent": {"type": "text_delta", "delta": "Waiting."}}));
                return Ok(());
            }
            peer.send(json!({"type": "tool_execution_start", "toolCallId": "pi-tool", "toolName": "bash"}));
            peer.send(json!({"type": "extension_ui_request", "method": "confirm",
                "title": "Minarbot tool approval", "id": "pi-approval",
                "message": json!({"toolName": "bash", "input": {"command": "echo fixture"}}).to_string()}));
        }
        _ => {
            if kind == "get_state" && args.has("--append-system-prompt") && !args.has("--extension")
            {
                return Err("Pi context must retain permission extension".into());
            }
            let data = match kind {
                "get_state" => json!({"sessionId": "pi-fixture"}),
                "get_available_models" => {
                    json!({"models": [{"provider": "fixture", "id": "model", "name": "Fixture"}]})
                }
                _ => json!({}),
            };
            peer.send(json!({"type": "response", "id": id, "success": true, "data": data}));
        }
    }
    Ok(())
}

fn prompt(peer: &Peer, value: &Value) -> Result<()> {
    let id = value["id"].clone();
    peer.with(|state| state.prompt = Some(id.clone()));
    let content = value["params"]["prompt"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .map(|block| block["text"].as_str().unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let (judge, mode) = peer.with(|state| (state.judge, state.mode.clone()));
    if judge {
        if value["params"]["sessionId"] != "judge" {
            return Err("Judge reused work session".into());
        }
        let verdict = json!({"met": true, "reason": "Answer matches.", "evidence": ["answer"]});
        peer.chunk(&verdict.to_string());
        peer.reply(&id, json!({"stopReason": "end_turn"}));
        return Ok(());
    }
    if content.contains("ACP_READY") {
        if value["params"]["sessionId"] != SESSION {
            return Err("Work prompt used a foreign session".into());
        }
        peer.chunk("ACP_READY");
        peer.reply(&id, json!({"stopReason": "end_turn"}));
        return Ok(());
    }
    if mode.as_deref() != Some("default") {
        std::process::exit(2);
    }
    let peer = peer.clone();
    tokio::spawn(async move {
        if let Err(error) = scripted(&peer, &id, &content).await {
            eprintln!("fake-acp failed: {error}");
            std::process::exit(1);
        }
    });
    Ok(())
}

async fn scripted(peer: &Peer, id: &Value, content: &str) -> Result<()> {
    if content.contains("CRASH") {
        peer.chunk("Partial before crash.");
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::process::exit(3);
    }
    if content.contains("INVALID") {
        peer.out.raw(b"bad json\n");
        return Ok(());
    }
    if content.contains("TREE") {
        let pid = spawn_idle_child(true, true)?;
        std::fs::write("child-pid", pid.to_string())?;
        peer.update(json!({"sessionUpdate": "tool_call", "toolCallId": "worker",
            "title": "Start worker", "status": "in_progress", "rawInput": {"pid": pid}}));
        peer.update(
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "worker",
            "rawOutput": "Worker started"}),
        );
    }
    if content.contains("WAIT") || content.contains("TREE") {
        peer.chunk("Waiting.");
        return Ok(());
    }
    peer.chunk("Before tool. ");
    peer.update(json!({"sessionUpdate": "tool_call", "toolCallId": "tool-1",
        "title": "echo fixture", "kind": "execute", "status": "pending"}));
    let result = peer
        .ask(
            "session/request_permission",
            json!({"toolCall": {"toolCallId": "tool-1", "title": "Run echo fixture",
                "kind": "execute", "rawInput": {"command": "echo fixture"}},
                "options": [{"kind": "allow_once", "optionId": "once"},
                    {"kind": "allow_always", "optionId": "persistent"},
                    {"kind": "reject_once", "optionId": "deny"}]}),
        )
        .await;
    let allowed = result["result"]["outcome"]["optionId"] == "once";
    peer.update(
        json!({"sessionUpdate": "tool_call_update", "toolCallId": "tool-1",
        "status": if allowed { "completed" } else { "failed" }}),
    );
    if content.contains("FILES") {
        let outside = peer
            .ask(
                "fs/write_text_file",
                json!({"path": "../escape.txt", "content": "escape"}),
            )
            .await;
        let link = peer
            .ask("fs/read_text_file", json!({"path": "outside/escape.txt"}))
            .await;
        let inside = peer
            .ask(
                "fs/write_text_file",
                json!({"path": "hello.txt", "content": "hello\n"}),
            )
            .await;
        let read = peer
            .ask("fs/read_text_file", json!({"path": "hello.txt"}))
            .await;
        let error = |value: &Value| value.get("error").is_some_and(|error| !error.is_null());
        if !error(&outside)
            || !error(&link)
            || error(&inside)
            || read["result"]["content"] != "hello\n"
        {
            std::process::exit(4);
        }
    }
    if content.contains("READONLY") {
        let write = peer
            .ask(
                "fs/write_text_file",
                json!({"path": "readonly.txt", "content": "no"}),
            )
            .await;
        if write.get("error").is_none_or(Value::is_null) {
            std::process::exit(5);
        }
    }
    peer.chunk("Final ");
    peer.chunk("answer.");
    peer.reply(id, json!({"stopReason": "end_turn"}));
    Ok(())
}
