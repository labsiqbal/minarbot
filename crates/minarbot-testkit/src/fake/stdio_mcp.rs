//! Extra stdio MCP servers for the gateway peer.
//!
//! Codex mounts every `mcp_servers.<name>` entry of `thread/start` with a
//! `command`, such as the server's built-in CodeGraph. The gateway peer
//! offers their tools next to Minarbot's and runs each request in a fresh
//! process: `initialize`, `notifications/initialized`, the request, exit.

use super::Result;
use serde_json::{json, Value};
use std::{path::Path, process::Stdio};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

#[derive(Clone, Debug)]
pub struct StdioServer {
    pub name: String,
    command: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
}

impl StdioServer {
    /// Every `mcp_servers` entry with a `command` (Minarbot's own entry is HTTP).
    pub fn from_config(config: &Value) -> Vec<Self> {
        config["mcp_servers"]
            .as_object()
            .map(|servers| {
                servers
                    .iter()
                    .filter_map(|(name, server)| {
                        Some(Self {
                            name: name.clone(),
                            command: server["command"].as_str()?.to_owned(),
                            args: server["args"]
                                .as_array()
                                .map(|args| {
                                    args.iter()
                                        .filter_map(|arg| arg.as_str().map(str::to_owned))
                                        .collect()
                                })
                                .unwrap_or_default(),
                            env: Vec::new(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A stdio MCP server started as `command args` with extra environment
    /// (for example `minarbot-server --mcp-stdio` with its URL and token).
    pub fn new(name: &str, command: &str, args: &[&str], env: &[(&str, &str)]) -> Self {
        Self {
            name: name.to_owned(),
            command: command.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            env: env
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        }
    }

    /// One request in a fresh session; a JSON-RPC error becomes an
    /// `isError` tool result carrying its message.
    pub async fn request(&self, cwd: &Path, method: &str, params: Value) -> Result<Value> {
        let mut child = Command::new(&self.command)
            .args(&self.args)
            .envs(self.env.iter().map(|(name, value)| (name, value)))
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdin = child.stdin.take().ok_or("stdio MCP stdin")?;
        let stdout = child.stdout.take().ok_or("stdio MCP stdout")?;
        let messages = [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2025-03-26", "capabilities": {},
                "clientInfo": {"name": "fake-gateway", "version": "1"}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": method, "params": params}),
        ];
        for message in &messages {
            stdin.write_all(format!("{message}\n").as_bytes()).await?;
        }
        stdin.flush().await?;
        let mut lines = BufReader::new(stdout).lines();
        let result = loop {
            let line = lines
                .next_line()
                .await?
                .ok_or_else(|| format!("stdio MCP server {} exited", self.name))?;
            let Ok(reply) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if reply["id"] != 2 {
                continue;
            }
            if let Some(error) = reply.get("error").filter(|error| !error.is_null()) {
                break super::gateway::tool_error_value(
                    error["message"].as_str().unwrap_or("MCP request failed."),
                    error["code"].clone(),
                );
            }
            break reply["result"].clone();
        };
        drop(stdin);
        let _ = child.kill().await;
        Ok(result)
    }

    /// The server's tools as MCP tool declarations.
    pub async fn tools(&self, cwd: &Path) -> Result<Vec<Value>> {
        let result = self.request(cwd, "tools/list", json!({})).await?;
        Ok(result["tools"].as_array().cloned().unwrap_or_default())
    }

    pub async fn call(&self, cwd: &Path, name: &str, arguments: Value) -> Result<Value> {
        self.request(
            cwd,
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        )
        .await
    }
}
