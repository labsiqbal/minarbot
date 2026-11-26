//! Shared helpers for the #89 regressed-file ports (`rg_*`): an MCP HTTP
//! client with the request shape of the `@modelcontextprotocol/sdk` client
//! the Node tests used, raw JSON-RPC calls, and the `mcpServer` facade
//! (`mintAgentSession`, `verifyBearer`, `revokeAgentSession`) as direct
//! token rows.

use minarbot_testkit::{
    db::{Fixture, Inspector},
    http::{Client, Method},
    prelude::*,
    protocol::sha256_hex,
    Server, FIXED_CLOCK,
};

/// Every scope of a peer token (`mcpScopes`).
pub const ALL_SCOPES: [&str; 21] = [
    "agents:read",
    "agents:profile",
    "history:read",
    "conversations:read",
    "projects:read",
    "routines:read",
    "workflows:read",
    "machines:read",
    "settings:read",
    "computer:read",
    "usage:read",
    "skills:read",
    "agents:write",
    "conversations:write",
    "projects:write",
    "routines:write",
    "agents:message",
    "workflows:run",
    "computer:run",
    "orchestration",
    "machine:update",
];

/// The scopes a harness session gets (`defaultSessionScopes`).
pub const SESSION_SCOPES: [&str; 19] = [
    "agents:read",
    "agents:profile",
    "agents:message",
    "history:read",
    "conversations:read",
    "conversations:write",
    "projects:read",
    "projects:write",
    "routines:read",
    "routines:write",
    "workflows:read",
    "workflows:run",
    "machines:read",
    "settings:read",
    "usage:read",
    "skills:read",
    "computer:read",
    "computer:run",
    "orchestration",
];

/// A raw `/mcp` reply.
#[derive(Debug)]
pub struct Rpc {
    pub status: u16,
    pub text: String,
    pub retry_after: Option<String>,
}

impl Rpc {
    pub fn json(&self) -> Result<Value> {
        Ok(serde_json::from_str(&self.text)?)
    }
}

/// `POST /mcp` with an optional bearer and extra headers; never fails on status.
pub async fn rpc(
    app: &Server,
    token: Option<&str>,
    method: &str,
    params: Value,
    headers: &[(&str, &str)],
) -> Result<Rpc> {
    let mut request = app
        .client()?
        .request(Method::POST, app.url("/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    if let Some(token) = token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let response = request.body(body.to_string()).send().await?;
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    Ok(Rpc {
        status,
        text: response.text().await?,
        retry_after,
    })
}

/// `tools/call` through [`rpc`].
pub async fn rpc_call(app: &Server, token: &str, name: &str, args: Value) -> Result<Rpc> {
    rpc(
        app,
        Some(token),
        "tools/call",
        json!({"name": name, "arguments": args}),
        &[],
    )
    .await
}

/// Minimal streamable-HTTP MCP client (JSON responses only), as the SDK
/// `Client` used it: `initialize`, `notifications/initialized`, then calls.
pub struct McpClient {
    client: Client,
    url: String,
    token: String,
    next: std::sync::atomic::AtomicU64,
}

impl McpClient {
    pub async fn connect(app: &Server, token: &str) -> Result<Self> {
        let client = Self {
            client: app.client()?,
            url: app.url("/mcp"),
            token: token.to_owned(),
            next: std::sync::atomic::AtomicU64::new(0),
        };
        let init = client
            .request(
                "initialize",
                json!({"protocolVersion": "2025-03-26", "capabilities": {},
                    "clientInfo": {"name": "minarbot-test", "version": "1"}}),
            )
            .await?;
        ensure!(
            init["protocolVersion"].is_string(),
            "initialize result: {init}"
        );
        let status = client
            .post(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await?
            .0;
        ensure!(status == 202, "notifications/initialized: {status}");
        Ok(client)
    }

    async fn post(&self, body: Value) -> Result<(u16, String)> {
        let response = self
            .client
            .request(Method::POST, &self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("Authorization", format!("Bearer {}", self.token))
            .body(body.to_string())
            .send()
            .await?;
        Ok((response.status().as_u16(), response.text().await?))
    }

    /// One JSON-RPC request; a non-2xx HTTP status fails the way the SDK
    /// transport rejects (`Error POSTing to endpoint: <body>`).
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let (status, text) = self
            .post(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await?;
        if !(200..300).contains(&status) {
            return Err(format!("HTTP {status}: Error POSTing to endpoint: {text}").into());
        }
        let body: Value = serde_json::from_str(&text)?;
        ensure!(body["id"] == id, "response id: {body}");
        if body["error"].is_object() {
            return Err(format!("JSON-RPC error: {}", body["error"]).into());
        }
        Ok(body["result"].clone())
    }

    pub async fn list_tools(&self) -> Result<Vec<String>> {
        let result = self.request("tools/list", json!({})).await?;
        Ok(result["tools"]
            .as_array()
            .ok_or("tools/list result")?
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .collect())
    }

    /// `callTool`: the raw tool result (`isError` included).
    pub async fn call_tool(&self, name: &str, args: Value) -> Result<Value> {
        self.request("tools/call", json!({"name": name, "arguments": args}))
            .await
    }

    /// `call` of the Node tests: the tool must succeed; returns
    /// `structuredContent`.
    pub async fn call(&self, name: &str, args: Value) -> Result<Value> {
        let result = self.call_tool(name, args).await?;
        ensure!(result["isError"] != true, "{name} failed: {result}");
        Ok(result["structuredContent"].clone())
    }
}

/// The text of a tool error result, parsed (`{error, status}`).
pub fn tool_error(result: &Value) -> Value {
    result["content"][0]["text"]
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or(Value::Null)
}

/// A session token row, as `mintAgentSession` stored it.
pub struct Session {
    pub id: String,
    pub token: String,
}

fn secret() -> String {
    format!("{}{}", new_id().replace('-', ""), new_id().replace('-', ""))
}

/// `mintAgentSession({agent, turn, scopes, ttlMs})`: the Node facade wrote
/// the row directly (`created` = now, `expires` = now + ttl) after checking
/// that the agent exists and the bound turn is running.
pub fn mint_session(
    app: &Server,
    agent: &str,
    turn: Option<&str>,
    scopes: &[&str],
    ttl_ms: i64,
) -> Result<Session> {
    let inspector = Inspector::open(app.database())?;
    if inspector
        .rows("SELECT 1 FROM agents WHERE id=?", [agent])?
        .is_empty()
    {
        return Err("Invalid agent session.".into());
    }
    if let Some(turn) = turn {
        if inspector
            .rows(
                "SELECT 1 FROM turns WHERE id=? AND agent=? AND status='running'",
                [turn, agent],
            )?
            .is_empty()
        {
            return Err("Agent session requires its running turn.".into());
        }
    }
    let session = Session {
        id: new_id(),
        token: secret(),
    };
    let mut sorted = scopes.to_vec();
    sorted.sort_unstable();
    Fixture::open(app.database())?.execute(
        "INSERT INTO mcp_server_tokens(id,token_hash,kind,name,agent,turn,scopes,created,expires,request_digest) VALUES (?,?,'session','',?,?,?,?,?,'')",
        (
            &session.id,
            sha256_hex(&session.token),
            agent,
            turn,
            json!(sorted).to_string(),
            FIXED_CLOCK,
            FIXED_CLOCK + ttl_ms,
        ),
    )?;
    Ok(session)
}

/// `revokeAgentSession(id)`.
pub fn revoke_session(app: &Server, id: &str) -> Result<()> {
    Fixture::open(app.database())?.execute(
        "UPDATE mcp_server_tokens SET revoked=coalesce(revoked,?) WHERE id=? AND kind='session'",
        (FIXED_CLOCK, id),
    )?;
    Ok(())
}

/// `verifyBearer`: the server accepts the token (`initialize` answers 200).
pub async fn verified(app: &Server, token: &str) -> Result<bool> {
    Ok(rpc(app, Some(token), "initialize", json!({}), &[])
        .await?
        .status
        == 200)
}

/// The stored token row of `token`, if any.
pub fn token_row(app: &Server, token: &str) -> Result<Option<Value>> {
    Ok(Inspector::open(app.database())?
        .rows(
            "SELECT * FROM mcp_server_tokens WHERE token_hash=?",
            [sha256_hex(token)],
        )?
        .into_iter()
        .next())
}

/// `POST /api/mcp-server/peer-tokens` with fresh ID.
pub async fn peer(app: &Server, name: &str, scopes: &[&str]) -> Result<Value> {
    app.post(
        "/api/mcp-server/peer-tokens",
        json!({"id": new_id(), "name": name, "scopes": scopes}),
    )
    .await
}

/// The `token` field of a peer token reply.
pub fn token(value: &Value) -> Result<String> {
    Ok(value["token"].as_str().ok_or("token")?.to_owned())
}
