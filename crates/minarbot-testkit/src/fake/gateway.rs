//! Codex `app-server` gateway peer (port of `app/fixtures/fake-gateway-harness.mjs`).
//!
//! The server seeds the `codex-test-gateway` provider instance with this
//! binary when a test configures a gateway ([`crate::ServerConfig::gateway`]).
//! For each turn the peer:
//!
//! 1. lists Minarbot's MCP tools (`tools/list`) and offers them as OpenAI
//!    functions (none for goal judges, which run without an MCP session;
//!    only `ask_user` and `edit_profile` for profile setup turns
//!    whose prompt contains `Initial name:`), plus the tools of every extra
//!    stdio server in `mcp_servers` (such as CodeGraph), as Codex mounts them;
//! 2. rebuilds the conversation from `<minarbot-message>` tags or
//!    `role: ` markers in the prompt, else from the saved session file
//!    `.fake-gateway-<thread>.json` (resume), else from additional context;
//! 3. posts `chat/completions` to `MINARBOT_GATEWAY_BASE_URL` (streaming
//!    when `MINARBOT_GATEWAY_STREAM=true`), with `MINARBOT_PROVIDER_KEY` as
//!    bearer token;
//! 4. executes tool calls through MCP `tools/call`, filling a stable
//!    `requestId` when the schema requires one, clamping `wait_task`
//!    timeouts, waiting for legacy `delegate_task {wait:true}`, fanning out
//!    `send_message` recipients and forwarding returned images;
//! 5. stops offering tools after `MINARBOT_TOOL_BUDGET` calls (default 200);
//! 6. streams the answer as `item/agentMessage/delta`, then
//!    `item/completed`, `thread/tokenUsage/updated` and `turn/completed`.
//!
//! `model/list` forwards the provider's `GET /models` as one catalog page.
//! Provider or MCP failures become an `error` notification; `turn/interrupt`
//! aborts the running turn without further output. Each input message's
//! method, id and thread are appended to `fake-gateway-log.jsonl`.

use super::{stdio_mcp::StdioServer, Args, Out, Result};
use serde_json::{json, Map, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    process::ExitCode,
    sync::{Arc, Mutex},
};
use tokio::task::AbortHandle;
use uuid::Uuid;

/// Protocol log (method, id, thread) in the peer's working directory.
pub const LOG: &str = "fake-gateway-log.jsonl";

struct State {
    session: String,
    mcp_url: String,
    mcp_token: String,
    model: String,
    workspace: PathBuf,
    saved: Option<Vec<Value>>,
    /// Active run per thread, tagged with its run id so a finished run never
    /// removes the run that replaced it.
    runs: HashMap<String, (String, AbortHandle)>,
    next_id: u64,
    request_ids: HashMap<String, String>,
    /// Extra stdio MCP servers from `thread/start` and which serves each tool.
    stdio: Vec<StdioServer>,
    stdio_tools: HashMap<String, usize>,
}

#[derive(Clone)]
struct Peer {
    out: Out,
    client: reqwest::Client,
    provider_url: String,
    provider_key: String,
    stream: bool,
    budget: usize,
    state: Arc<Mutex<State>>,
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

impl Peer {
    fn with<T>(&self, change: impl FnOnce(&mut State) -> T) -> T {
        match self.state.lock() {
            Ok(mut state) => change(&mut state),
            Err(poisoned) => change(&mut poisoned.into_inner()),
        }
    }

    fn notify(&self, method: &str, params: Value) {
        let mut ordered = json!({"threadId": self.with(|state| state.session.clone())});
        if let (Value::Object(target), Value::Object(source)) = (&mut ordered, params) {
            target.extend(source);
        }
        self.out.send(&json!({"method": method, "params": ordered}));
    }

    fn next_id(&self) -> u64 {
        self.with(|state| {
            let id = state.next_id;
            state.next_id += 1;
            id
        })
    }

    fn request_id(&self, generation: u64, call: &str, suffix: &str) -> String {
        let key = format!("{generation}:{call}:{suffix}");
        self.with(|state| {
            state
                .request_ids
                .entry(key)
                .or_insert_with(|| Uuid::new_v4().to_string())
                .clone()
        })
    }

    fn save(&self, messages: &[Value]) {
        let path = self.with(|state| {
            state.saved = Some(messages.to_vec());
            state
                .workspace
                .join(format!(".fake-gateway-{}.json", state.session))
        });
        let _ = std::fs::write(path, Value::Array(messages.to_vec()).to_string());
    }

    /// MCP JSON-RPC call. HTTP 5xx and "uncertain" errors fail the turn;
    /// other failures become an `isError` tool result.
    async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        let (url, token) = self.with(|state| (state.mcp_url.clone(), state.mcp_token.clone()));
        let id = self.next_id();
        let response = self
            .client
            .post(&url)
            .bearer_auth(token)
            .header("Accept", "application/json, text/event-stream")
            .json(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .send()
            .await?;
        let status = response.status().as_u16();
        let body: Value = response.json().await?;
        if !(200..300).contains(&status) {
            if status >= 500 {
                return Err(format!("MCP request failed ({status}): {body}").into());
            }
            let message = match &body["error"] {
                Value::String(message) => message.clone(),
                error => error["message"]
                    .as_str()
                    .unwrap_or("MCP request failed.")
                    .to_owned(),
            };
            return Ok(tool_error(&message, json!(status)));
        }
        if let Some(error) = body.get("error").filter(|error| !error.is_null()) {
            let message = error["message"].as_str().unwrap_or("MCP request failed.");
            if message.to_lowercase().contains("uncertain") {
                return Err(message.to_owned().into());
            }
            return Ok(tool_error(message, error["code"].clone()));
        }
        Ok(match body.get("result") {
            Some(result) if !result.is_null() => result.clone(),
            _ => json!({}),
        })
    }

    async fn tools(&self) -> Result<Vec<Value>> {
        // Goal judges run without an MCP session: offer no tools. (The Node
        // fixture called `tools/list` on an empty URL and failed the judge.)
        if self.with(|state| state.mcp_url.is_empty()) {
            return Ok(Vec::new());
        }
        let result = self.rpc("tools/list", json!({})).await?;
        let mut tools: Vec<Value> = result["tools"]
            .as_array()
            .map(|tools| tools.iter().map(function).collect())
            .unwrap_or_default();
        let (servers, cwd) = self.with(|state| (state.stdio.clone(), state.workspace.clone()));
        for (index, server) in servers.iter().enumerate() {
            for tool in server.tools(&cwd).await? {
                if let Some(name) = tool["name"].as_str() {
                    self.with(|state| state.stdio_tools.insert(name.to_owned(), index));
                }
                tools.push(function(&tool));
            }
        }
        Ok(tools)
    }

    /// `tools/call` on the stdio server that offered `name`, if any.
    async fn stdio_call(&self, name: &str, arguments: &Value) -> Result<Option<Value>> {
        let (server, cwd) = self.with(|state| {
            let server = state
                .stdio_tools
                .get(name)
                .and_then(|index| state.stdio.get(*index))
                .cloned();
            (server, state.workspace.clone())
        });
        match server {
            Some(server) => Ok(Some(server.call(&cwd, name, arguments.clone()).await?)),
            None => Ok(None),
        }
    }

    /// One provider request; streamed responses are folded into a
    /// non-streamed completion plus the original content deltas. Each content
    /// delta is also forwarded as `item/agentMessage/delta` for `item` the
    /// moment it arrives, like Codex streams a reply.
    async fn provider(&self, body: &Value, item: &str) -> Result<(Value, Vec<String>)> {
        let mut request = self
            .client
            .post(format!(
                "{}/chat/completions",
                self.provider_url.trim_end_matches('/')
            ))
            .json(body);
        if !self.provider_key.is_empty() {
            request = request.bearer_auth(&self.provider_key);
        }
        let response = request.send().await;
        super::log(
            LOG,
            &json!({"stage": "provider-response", "pid": std::process::id(), "ok": response.is_ok()}),
        );
        let response = response?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("Gateway provider returned {}", status.as_u16()).into());
        }
        if body["stream"] != true {
            return Ok((response.json().await?, Vec::new()));
        }
        let mut response = response;
        let mut pending: Vec<u8> = Vec::new();
        let mut lines: Vec<String> = Vec::new();
        let mut content = String::new();
        let mut deltas = Vec::new();
        let mut calls: Vec<Value> = Vec::new();
        let mut finish = Value::Null;
        let mut usage = Value::Null;
        let mut ended = false;
        while !ended {
            match response.chunk().await? {
                Some(chunk) => pending.extend_from_slice(&chunk),
                None => {
                    pending.push(b'\n');
                    ended = true;
                }
            }
            while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = pending.drain(..=end).collect();
                lines.push(String::from_utf8_lossy(&line).trim_end().to_owned());
            }
            for line in lines.drain(..) {
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                let chunk: Value = serde_json::from_str(data)?;
                // A provider `error` event ends the stream as a failure.
                if chunk.get("error").is_some_and(|error| !error.is_null()) {
                    return Err("Gateway provider stream failed".into());
                }
                let choice = &chunk["choices"][0];
                let delta = &choice["delta"];
                if let Some(text) = delta["content"].as_str() {
                    if !text.is_empty() {
                        self.notify(
                            "item/agentMessage/delta",
                            json!({"itemId": item, "delta": text}),
                        );
                    }
                    deltas.push(text.to_owned());
                    content.push_str(text);
                }
                for call in delta["tool_calls"].as_array().into_iter().flatten() {
                    let index = call["index"]
                        .as_u64()
                        .and_then(|index| usize::try_from(index).ok())
                        .unwrap_or(calls.len());
                    while calls.len() <= index {
                        let position = calls.len();
                        calls.push(json!({"id": format!("call-{position}"), "type": "function",
                        "function": {"name": "", "arguments": ""}}));
                    }
                    let item = &mut calls[index];
                    if let Some(id) = call["id"].as_str().filter(|id| !id.is_empty()) {
                        item["id"] = json!(id);
                    }
                    for field in ["name", "arguments"] {
                        if let Some(part) = call["function"][field].as_str() {
                            let joined = format!(
                                "{}{part}",
                                item["function"][field].as_str().unwrap_or_default()
                            );
                            item["function"][field] = json!(joined);
                        }
                    }
                }
                if finish.is_null() {
                    finish = choice["finish_reason"].clone();
                }
                if usage.is_null() {
                    usage = chunk["usage"].clone();
                }
            }
        }
        if finish.is_null() {
            finish = json!(if calls.is_empty() {
                "stop"
            } else {
                "tool_calls"
            });
        }
        let mut completion = json!({"choices": [{"finish_reason": finish,
            "message": {"role": "assistant", "content": content, "tool_calls": calls}}]});
        if !usage.is_null() {
            completion["usage"] = usage;
        }
        Ok((completion, deltas))
    }
}

impl Peer {
    /// `model/list`: the provider's `GET /models` as a Codex catalog page.
    async fn models(&self) -> Result<Value> {
        let mut request = self.client.get(format!(
            "{}/models",
            self.provider_url.trim_end_matches('/')
        ));
        if !self.provider_key.is_empty() {
            request = request.bearer_auth(&self.provider_key);
        }
        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("Gateway provider returned {}", status.as_u16()).into());
        }
        let body: Value = response.json().await?;
        let data: Vec<Value> = body["data"]
            .as_array()
            .ok_or("Gateway provider returned a malformed model list")?
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let id = row["id"].as_str()?;
                Some(json!({"id": id, "displayName": id, "isDefault": index == 0, "hidden": false}))
            })
            .collect();
        Ok(json!({"data": data, "nextCursor": null}))
    }
}

/// An MCP tool declaration as an OpenAI function.
fn function(tool: &Value) -> Value {
    let parameters = match tool.get("inputSchema") {
        Some(schema) if !schema.is_null() => schema.clone(),
        _ => json!({"type": "object"}),
    };
    let mut function = json!({"name": tool["name"]});
    if let Some(description) = tool.get("description") {
        function["description"] = description.clone();
    }
    function["parameters"] = parameters;
    json!({"type": "function", "function": function})
}

/// `isError` tool result, also for stdio servers.
pub(super) fn tool_error_value(message: &str, status: Value) -> Value {
    tool_error(message, status)
}

fn tool_error(message: &str, status: Value) -> Value {
    json!({"isError": true, "content": [{"type": "text", "text": message}],
        "structuredContent": {"error": message, "status": status}})
}

/// Text the model sees for a tool result.
/// Tool result text for the model: structured content when present; a
/// result with fields beyond `content`/`isError` (Minarbot's `files`,
/// `omitted`, `uncertain`) as its full JSON, as the Node provider saw it;
/// otherwise the text content.
fn tool_text(result: &Value) -> String {
    if let Some(structured) = result
        .get("structuredContent")
        .filter(|value| !value.is_null())
    {
        return structured.to_string();
    }
    let extra = result.as_object().is_some_and(|fields| {
        fields.iter().any(|(key, value)| {
            !["content", "isError", "structuredContent"].contains(&key.as_str()) && !value.is_null()
        })
    });
    if extra {
        return result.to_string();
    }
    if let Some(content) = result["content"].as_array() {
        return content
            .iter()
            .map(|item| item["text"].as_str().unwrap_or_default())
            .collect();
    }
    result.to_string()
}

/// Messages embedded in a prompt: `<minarbot-message>{json}</minarbot-message>`
/// tags, else `role: ` blocks separated by blank lines.
fn transcript(prompt: &str) -> Vec<Value> {
    let mut tagged = Vec::new();
    let mut rest = prompt;
    while let Some(start) = rest.find("<minarbot-message>") {
        let after = &rest[start + "<minarbot-message>".len()..];
        let Some(end) = after.find("</minarbot-message>") else {
            break;
        };
        if let Ok(message) = serde_json::from_str(&after[..end]) {
            tagged.push(message);
        }
        rest = &after[end + "</minarbot-message>".len()..];
    }
    if !tagged.is_empty() {
        return tagged;
    }
    let mut markers = Vec::new();
    for role in ["system", "user", "assistant", "tool"] {
        let marker = format!("{role}: ");
        if prompt.starts_with(&marker) {
            markers.push((0, marker.len(), role));
        }
        let separated = format!("\n\n{marker}");
        let mut from = 0;
        while let Some(at) = prompt[from..].find(&separated) {
            let index = from + at;
            markers.push((index, separated.len(), role));
            from = index + separated.len();
        }
    }
    markers.sort_by_key(|marker| marker.0);
    markers
        .iter()
        .enumerate()
        .map(|(position, (index, length, role))| {
            let end = markers
                .get(position + 1)
                .map_or(prompt.len(), |next| next.0);
            json!({"role": role, "content": &prompt[index + length..end]})
        })
        .collect()
}

fn last_index(messages: &[Value], before: usize, role: &str) -> Option<usize> {
    messages[..before.min(messages.len())]
        .iter()
        .rposition(|message| message["role"] == role)
}

async fn run(peer: Peer, prompt: String, additional: Value, images: Vec<String>) -> Result<()> {
    let context = additional
        .as_object()
        .map(|entries| {
            entries
                .values()
                .filter_map(|entry| entry["value"].as_str())
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default();
    let parsed = transcript(&prompt);
    let saved = peer.with(|state| state.saved.clone());
    let mut messages: Vec<Value> = if !parsed.is_empty() {
        parsed.clone()
    } else if let Some(saved) = saved {
        let mut messages = saved;
        messages.push(json!({"role": "user", "content": prompt}));
        messages
    } else {
        let system = [context.as_str(), prompt.as_str()]
            .iter()
            .filter(|part| !part.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("\n\n");
        vec![json!({"role": "system", "content": system})]
    };
    let parsed_user = last_index(&parsed, parsed.len(), "user");
    let reminder = prompt.contains("One verification reminder:");
    let current_user = match parsed_user {
        Some(index) if reminder => last_index(&parsed, index, "user"),
        Some(index) => Some(index),
        None => last_index(&messages, messages.len(), "user"),
    };
    if !messages.iter().any(|message| message["role"] == "user") {
        let content = parsed
            .last()
            .map_or_else(|| json!(prompt), |message| message["content"].clone());
        messages.push(json!({"role": "user", "content": content}));
    }
    if !images.is_empty() {
        if let Some(index) = last_index(&messages, messages.len(), "user") {
            let mut parts = match &messages[index]["content"] {
                Value::Array(parts) => parts.clone(),
                Value::String(text) => vec![json!({"type": "text", "text": text})],
                other => vec![json!({"type": "text", "text": other.to_string()})],
            };
            for url in &images {
                if !parts
                    .iter()
                    .any(|part| part["image_url"]["url"] == url.as_str())
                {
                    parts.push(json!({"type": "image_url", "image_url": {"url": url}}));
                }
            }
            messages[index]["content"] = json!(parts);
        }
    }
    let has_mcp = peer.with(|state| !state.mcp_url.is_empty());
    super::log(LOG, &json!({"stage": "tools"}));
    let mut tools = peer.tools().await?;
    if prompt.contains("Initial name:") {
        tools.retain(|tool| {
            matches!(
                tool["function"]["name"].as_str(),
                Some("ask_user" | "edit_profile")
            )
        });
    }
    let mut calls = 0;
    let mut exhausted = peer.budget == 0;
    let generation = peer.next_id();
    loop {
        let disabled = messages
            .last()
            .and_then(|message| message["content"].as_str())
            .is_some_and(|content| content.contains("Tools are now disabled"));
        if exhausted && !disabled {
            messages.push(json!({"role": "user", "content": format!(
                "Stopped after {} tool steps. Tools are now disabled. Summarise what you did, what is left, and tell the user they can reply \"continue\". Do not claim unfinished work is complete.",
                peer.budget)}));
        }
        let current = current_user.map_or(-1, |index| index as i64);
        let mut body_messages: Vec<Value> = messages
            .iter()
            .enumerate()
            .map(|(index, message)| {
                if (index as i64) > current || message["role"] != "tool" {
                    message.clone()
                } else {
                    let mut message = message.clone();
                    message["role"] = json!("assistant");
                    message
                }
            })
            .collect();
        if reminder {
            if let Some(prior) =
                current_user.and_then(|current| last_index(&body_messages, current, "assistant"))
            {
                body_messages[prior]["role"] = json!("tool");
            }
        }
        let model = peer.with(|state| state.model.clone());
        let mut body = json!({"model": if model.is_empty() { "fixture".to_owned() } else { model },
            "messages": body_messages, "stream": peer.stream});
        if !exhausted && has_mcp {
            body["tools"] = json!(tools);
            body["parallel_tool_calls"] = json!(false);
        }
        super::log(
            LOG,
            &json!({"stage": "provider", "pid": std::process::id(), "messages": body["messages"].as_array().map_or(0, Vec::len),
                "hubMeter": peer.provider_url.contains("/api/hub-meter/")}),
        );
        let item = Uuid::new_v4().to_string();
        let (value, deltas) = peer.provider(&body, &item).await?;
        let choice = &value["choices"][0];
        let message = &choice["message"];
        if !choice.is_object() || !message.is_object() {
            return Err("Gateway provider returned malformed response".into());
        }
        let tool_calls = message["tool_calls"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if !tool_calls.is_empty() && choice["finish_reason"] == "tool_calls" && !exhausted {
            let streamed: String = deltas.concat();
            if !streamed.is_empty() {
                peer.notify(
                    "item/completed",
                    json!({"item": {"id": item, "type": "agentMessage", "text": streamed}}),
                );
            }
            let mut assistant = Map::new();
            assistant.insert("role".into(), json!("assistant"));
            if let Some(fields) = message.as_object() {
                assistant.extend(fields.clone());
            }
            messages.push(Value::Object(assistant));
            for call in &tool_calls {
                // Codex rejects provider tool calls without a function name.
                if call["function"]["name"].as_str().is_none_or(str::is_empty) {
                    return Err("Gateway provider returned a malformed tool call".into());
                }
                let raw = call["function"]["arguments"]
                    .as_str()
                    .filter(|raw| !raw.is_empty())
                    .unwrap_or("{}");
                let args: Value = serde_json::from_str(raw)
                    .map_err(|_| "Gateway provider returned malformed tool arguments")?;
                let id = call["id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .map_or_else(|| Uuid::new_v4().to_string(), str::to_owned);
                let name = call["function"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                peer.notify(
                    "item/started",
                    json!({"item": {"id": id, "type": "mcpToolCall",
                    "tool": name, "arguments": args}}),
                );
                let schema = tools
                    .iter()
                    .find(|tool| tool["function"]["name"] == name)
                    .map(|tool| tool["function"]["parameters"].clone())
                    .unwrap_or_else(|| json!({}));
                let needs_request = schema["required"]
                    .as_array()
                    .is_some_and(|required| required.iter().any(|field| field == "requestId"));
                let call_one = |input: Value, suffix: &str| {
                    let mut normalized = input;
                    if needs_request && !truthy(&normalized["requestId"]) {
                        normalized["requestId"] = json!(peer.request_id(generation, &id, suffix));
                    }
                    if name == "wait_task" {
                        if let Some(timeout) = normalized.get("timeoutSeconds").cloned() {
                            normalized["timeoutSeconds"] = json!(clamp_timeout(&timeout));
                        }
                    }
                    let peer = peer.clone();
                    let name = name.clone();
                    async move {
                        if let Some(result) = peer.stdio_call(&name, &normalized).await? {
                            return Ok(result);
                        }
                        peer.rpc("tools/call", json!({"name": name, "arguments": normalized}))
                            .await
                    }
                };
                let result = if name == "send_message" && args["recipients"].is_array() {
                    let mut input = json!({"requestId": peer.request_id(generation, &id, "recipients"),
                        "recipients": args["recipients"], "text": args["text"],
                        "expects_reply": args["expects_reply"]});
                    if !args["metadata"].is_null() {
                        input["metadata"] = args["metadata"].clone();
                    }
                    call_one(input, "recipients").await?
                } else {
                    let mut result = call_one(args.clone(), "").await?;
                    if result["isError"] == true
                        && tool_text(&result).to_lowercase().contains("uncertain")
                    {
                        return Err(tool_text(&result).into());
                    }
                    // When Minarbot already parked the turn for the result
                    // (`status: waiting`), it resumes the turn itself, like a
                    // real harness that moves on; only a queued task is
                    // waited for here.
                    let legacy_wait = name == "delegate_task"
                        && args["wait"] == true
                        && result["structuredContent"]["status"] != "waiting";
                    if legacy_wait {
                        if let Some(task) = result["structuredContent"]["taskId"].as_str() {
                            let task = task.to_owned();
                            result = peer
                                .rpc(
                                    "tools/call",
                                    json!({"name": "wait_task", "arguments": {
                                        "requestId": peer.request_id(generation, &id, "wait"),
                                        "taskId": task,
                                        "timeoutSeconds": clamp_timeout(&args["timeoutSeconds"]),
                                    }}),
                                )
                                .await?;
                        }
                    }
                    result
                };
                peer.notify(
                    "item/completed",
                    json!({"item": {"id": id, "type": "mcpToolCall",
                    "tool": name, "arguments": args, "status": "completed", "result": result}}),
                );
                messages.push(
                    json!({"role": "tool", "tool_call_id": id, "content": tool_text(&result)}),
                );
                let image = result["content"].as_array().and_then(|items| {
                    items.iter().find(|item| {
                        item["type"] == "image"
                            && item["data"].as_str().is_some_and(|data| !data.is_empty())
                            && item["mimeType"]
                                .as_str()
                                .is_some_and(|mime| !mime.is_empty())
                    })
                });
                if let Some(image) = image {
                    let url = format!(
                        "data:{};base64,{}",
                        image["mimeType"].as_str().unwrap_or_default(),
                        image["data"].as_str().unwrap_or_default()
                    );
                    messages.push(json!({"role": "user", "content": [
                        {"type": "text", "text": "Image attachment (reference data)."},
                        {"type": "image_url", "image_url": {"url": url}},
                    ]}));
                }
                calls += 1;
            }
            exhausted = calls >= peer.budget;
            peer.save(&messages);
            continue;
        }
        if !tool_calls.is_empty() {
            return Err("Gateway provider returned tools after budget exhaustion".into());
        }
        let id = item;
        let content = message["content"].as_str().unwrap_or_default().to_owned();
        // Streamed deltas were forwarded as they arrived.
        if deltas.is_empty() && !content.is_empty() {
            peer.notify(
                "item/agentMessage/delta",
                json!({"itemId": id, "delta": content}),
            );
        }
        peer.notify(
            "item/completed",
            json!({"item": {"id": id, "type": "agentMessage", "text": content}}),
        );
        // A provider answer without usage leaves the counts unknown, so the
        // peer reports no token usage for it.
        let usage = &value["usage"];
        if usage.is_object() {
            peer.notify(
                "thread/tokenUsage/updated",
                json!({"tokenUsage": {"last": {
                "inputTokens": usage["prompt_tokens"].as_u64().unwrap_or(0),
                "outputTokens": usage["completion_tokens"].as_u64().unwrap_or(0)}}}),
            );
        }
        peer.notify(
            "turn/completed",
            json!({"turn": {"id": "fake-turn", "status": "completed"}}),
        );
        peer.save(&messages);
        return Ok(());
    }
}

/// JavaScript truthiness of a JSON value.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `Math.min(30, Math.max(1, Number(value) || 30))`.
fn clamp_timeout(value: &Value) -> f64 {
    let number = match value {
        Value::Number(number) => number.as_f64().unwrap_or(0.0),
        Value::String(text) => text.trim().parse().unwrap_or(0.0),
        Value::Bool(true) => 1.0,
        _ => 0.0,
    };
    let number = if number == 0.0 || number.is_nan() {
        30.0
    } else {
        number
    };
    number.clamp(1.0, 30.0)
}

/// Entry point of `fake-gateway`.
pub async fn main(args: Args) -> Result<ExitCode> {
    if args.has("--version") {
        println!("fake-gateway-harness 1.0.0");
        return Ok(ExitCode::SUCCESS);
    }
    // Unset means the default; 0 is a continuation with no budget left.
    let budget = env("MINARBOT_TOOL_BUDGET").parse::<usize>().unwrap_or(200);
    let peer = Peer {
        out: Out::default(),
        client: reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        provider_url: env("MINARBOT_GATEWAY_BASE_URL"),
        provider_key: env("MINARBOT_PROVIDER_KEY"),
        stream: env("MINARBOT_GATEWAY_STREAM") == "true",
        budget,
        state: Arc::new(Mutex::new(State {
            session: Uuid::new_v4().to_string(),
            mcp_url: env("MINARBOT_MCP_URL"),
            mcp_token: env("MINARBOT_MCP_TOKEN"),
            model: env("MINARBOT_GATEWAY_MODEL"),
            workspace: std::env::current_dir().unwrap_or_default(),
            saved: None,
            runs: HashMap::new(),
            next_id: 1,
            request_ids: HashMap::new(),
            stdio: Vec::new(),
            stdio_tools: HashMap::new(),
        })),
    };
    let mut lines = super::stdin_lines();
    while let Some(line) = super::next_line(&mut lines).await? {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        super::log(
            LOG,
            &json!({"method": value["method"], "id": value["id"], "threadId": value["params"]["threadId"]}),
        );
        let id = &value["id"];
        let params = &value["params"];
        match value["method"].as_str() {
            Some("initialize") => peer
                .out
                .send(&json!({"id": id, "result": {"userAgent": "fake-gateway"}})),
            Some(method @ ("thread/start" | "thread/resume")) => {
                let config = &params["config"];
                let server = &config["mcp_servers"]["minarbot"];
                let token_name = server["bearer_token_env_var"]
                    .as_str()
                    .unwrap_or("MINARBOT_MCP_TOKEN")
                    .to_owned();
                let session = peer.with(|state| {
                    if let Some(url) = server["url"].as_str() {
                        state.mcp_url = url.to_owned();
                    }
                    state.mcp_token = env(&token_name);
                    if let Some(cwd) = params["cwd"].as_str() {
                        state.workspace = PathBuf::from(cwd);
                    }
                    if let Some(thread) = params["threadId"].as_str() {
                        state.session = thread.to_owned();
                    }
                    if let Some(model) = params["model"].as_str() {
                        state.model = model.to_owned();
                    }
                    state.stdio = StdioServer::from_config(config);
                    state.stdio_tools.clear();
                    if method == "thread/resume" {
                        let saved = state
                            .workspace
                            .join(format!(".fake-gateway-{}.json", state.session));
                        if saved.exists() {
                            state.saved = std::fs::read_to_string(saved)
                                .ok()
                                .and_then(|text| serde_json::from_str(&text).ok());
                        }
                    }
                    state.session.clone()
                });
                peer.out
                    .send(&json!({"id": id, "result": {"thread": {"id": session}}}));
            }
            Some("turn/start") => {
                if let Some(message) = super::codex_input_error(&params["input"]) {
                    peer.out
                        .send(&json!({"id": id, "error": {"code": -32600, "message": message}}));
                    continue;
                }
                peer.out.send(&json!({"id": id,
                    "result": {"turn": {"id": "fake-turn", "status": "inProgress"}}}));
                peer.notify("turn/started", json!({"turn": {"id": "fake-turn"}}));
                let key = params["threadId"]
                    .as_str()
                    .map_or_else(|| peer.with(|state| state.session.clone()), str::to_owned);
                let prompt = params["input"][0]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                let additional = params["additionalContext"].clone();
                // Attached images arrive as `image` items after the text.
                let images: Vec<String> = params["input"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|item| item["type"] == "image")
                    .filter_map(|item| item["url"].as_str().map(str::to_owned))
                    .collect();
                let worker = peer.clone();
                let finished = key.clone();
                let run_id = Uuid::new_v4().to_string();
                let own = run_id.clone();
                let task = super::spawn(async move {
                    if let Err(error) = run(worker.clone(), prompt, additional, images).await {
                        super::log(
                            LOG,
                            &json!({"stage": "error", "message": error.to_string()}),
                        );
                        worker.notify(
                            "error",
                            json!({"willRetry": false,
                            "error": {"message": error.to_string()}}),
                        );
                    }
                    worker.with(|state| {
                        if state.runs.get(&finished).is_some_and(|(id, _)| *id == own) {
                            state.runs.remove(&finished);
                        }
                    });
                });
                peer.with(|state| state.runs.insert(key, (run_id, task)));
            }
            Some("model/list") => {
                let peer = peer.clone();
                let id = id.clone();
                super::spawn(async move {
                    match peer.models().await {
                        Ok(result) => peer.out.send(&json!({"id": id, "result": result})),
                        Err(error) => peer.out.send(&json!({"id": id,
                            "error": {"code": -32000, "message": error.to_string()}})),
                    }
                });
            }
            Some("turn/interrupt") => {
                let key = params["threadId"]
                    .as_str()
                    .map_or_else(|| peer.with(|state| state.session.clone()), str::to_owned);
                if let Some((_, run)) = peer.with(|state| state.runs.remove(&key)) {
                    run.abort();
                }
                peer.out.send(&json!({"id": id, "result": {}}));
            }
            _ => {}
        }
    }
    Ok(ExitCode::SUCCESS)
}
