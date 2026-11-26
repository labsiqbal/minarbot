//! Minarbot's MCP server for peers and agent sessions: tools, scopes,
//! credentials, browser boundaries, receipts and revocation (port of
//! `app/mcp-server.test.ts`).

use crate::{
    fx_support::gated,
    rg_support::{
        mint_session, peer, revoke_session, rpc, rpc_call, token, token_row, verified, McpClient,
    },
};
use minarbot_testkit::{
    gated::{Answer, Gate, GatedModel},
    http::Method,
    prelude::*,
    wait::until_within,
    Server,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

const ALL: [&str; 3] = ["agents:read", "agents:message", "history:read"];
const ALLOWED_ORIGIN: &str = "https://minarbot.example:4317";
const REPLY: &str = "Reply from fixture.";

/// The Node `fixture()`: a model that answers `Reply from fixture.` unless
/// held, a server that also allows `https://minarbot.example:4317`, and one
/// local agent.
struct Fixture {
    _root: TestRoot,
    app: Server,
    model: GatedModel,
    hold: Arc<Mutex<Option<Gate>>>,
    agent: String,
}

impl Fixture {
    async fn start() -> Result<Self> {
        let model = GatedModel::start().await?;
        let hold: Arc<Mutex<Option<Gate>>> = Arc::default();
        let held = hold.clone();
        model.respond(
            move |_| match held.lock().ok().and_then(|gate| gate.clone()) {
                Some(gate) => Answer::after(&gate, Answer::text(REPLY)),
                None => Answer::text(REPLY),
            },
        );
        let root = TestRoot::new()?;
        let app = root
            .start(gated(&model).option("allowedOrigins", json!([ALLOWED_ORIGIN])))
            .await?;
        let agent = app
            .post_expect(
                "/api/agents",
                json!({"runtime": "api", "id": new_id(), "name": "Local agent",
                    "role": "Personal agent", "model": "fixture", "instructions": "Help."}),
                201,
            )
            .await?;
        Ok(Self {
            _root: root,
            app,
            model,
            hold,
            agent: agent["id"].as_str().ok_or("agent id")?.to_owned(),
        })
    }

    fn hold(&self) {
        if let Ok(mut gate) = self.hold.lock() {
            *gate = Some(Gate::new());
        }
    }

    fn release(&self) {
        if let Some(gate) = self.hold.lock().ok().and_then(|mut gate| gate.take()) {
            gate.open();
        }
    }

    fn turns(&self) -> String {
        format!("/api/agents/{}/turns", self.agent)
    }

    async fn owner_turn(&self, text: &str) -> Result<String> {
        let id = new_id();
        self.app
            .post_expect(&self.turns(), json!({"id": id, "text": text}), 202)
            .await?;
        Ok(id)
    }

    async fn peer_client(&self, name: &str, scopes: &[&str]) -> Result<(Value, McpClient)> {
        let created = peer(&self.app, name, scopes).await?;
        let client = McpClient::connect(&self.app, &token(&created)?).await?;
        Ok((created, client))
    }
}

/// `waitFor` (400 x 10 ms in Node; the kit budget here).
async fn settle_status(client: &McpClient, request: &str, status: &str) -> Result<()> {
    until("MCP fixture did not settle", || async {
        let value = client
            .call("get_message_status", json!({"requestId": request}))
            .await?;
        Ok((value["status"] == status).then_some(()))
    })
    .await
}

#[tokio::test]
async fn real_mcp_client_calls_all_tools_isolated_peer_history_stable_send_receipts_and_payload_conflict(
) -> Result<()> {
    let f = Fixture::start().await?;
    f.owner_turn("OWNER_PRIVATE_CANARY").await?;
    let (a, ca) = f.peer_client("Laptop A", &ALL).await?;
    let (_b, cb) = f.peer_client("Laptop B", &ALL).await?;
    let mut names = ca.list_tools().await?;
    names.sort();
    ensure!(
        names
            == [
                "cancel_message",
                "get_message_status",
                "list_agents",
                "read_history",
                "send_message"
            ],
        "peer tools: {names:?}"
    );
    let list = ca.call("list_agents", json!({})).await?;
    let first = &list["agents"][0];
    ensure!(first["id"] == f.agent.as_str(), "list_agents: {list}");
    ensure!(first["model"] == "fixture", "model: {first}");
    ensure!(
        first["description"].as_str().is_some_and(|d| !d.is_empty()),
        "description: {first}"
    );
    ensure!(first["role"] == "Personal agent", "role: {first}");
    ensure!(
        first["status"] == "idle" || first["status"] == "working",
        "status: {first}"
    );

    let request = new_id();
    let args = json!({"requestId": request, "recipient": f.agent, "text": "PEER_A_CANARY"});
    let delivery = ca.call("send_message", args.clone()).await?;
    ensure!(delivery["status"] == "queued", "delivery: {delivery}");
    settle_status(&ca, &request, "done").await?;
    let status = ca
        .call("get_message_status", json!({"requestId": request}))
        .await?;
    ensure!(status["reply"] == REPLY, "reply: {status}");
    ensure!(
        ca.call("send_message", args.clone()).await? == delivery,
        "send receipt replay"
    );
    let mut changed = args.clone();
    changed["text"] = json!("Changed");
    let drift = rpc_call(&f.app, &token(&a)?, "send_message", changed).await?;
    ensure!(drift.status == 409, "payload conflict: {drift:?}");
    let before = f.model.calls();
    ensure!(
        ca.call("send_message", args.clone()).await? == delivery,
        "replay after conflict"
    );
    ensure!(f.model.calls() == before, "replay called the model");
    ca.call(
        "send_message",
        json!({"requestId": new_id(), "recipient": f.agent, "text": "PEER_A_SECOND"}),
    )
    .await?;
    cb.call(
        "send_message",
        json!({"requestId": new_id(), "recipient": f.agent, "text": "PEER_B_CANARY"}),
    )
    .await?;

    let page = ca.call("read_history", json!({"limit": 1})).await?;
    ensure!(
        page["messages"].as_array().map(Vec::len) == Some(1),
        "page: {page}"
    );
    ensure!(!page["next_before"].is_null(), "next_before: {page}");
    let older = ca
        .call(
            "read_history",
            json!({"limit": 1, "before": page["next_before"]}),
        )
        .await?;
    ensure!(
        older["messages"][0]["id"] == delivery["turn"],
        "older page: {older}"
    );
    ensure!(older["next_before"].is_null(), "older next_before: {older}");
    let history_b = cb.call("read_history", json!({})).await?;
    ensure!(
        history_b["messages"].as_array().map(Vec::len) == Some(1),
        "peer B history: {history_b}"
    );
    ensure!(
        history_b["messages"][0]["text"] == "PEER_B_CANARY",
        "peer B text: {history_b}"
    );
    let all = json!([page, older, history_b]).to_string();
    ensure!(!all.contains("OWNER_PRIVATE_CANARY"), "owner text leaked");
    ensure!(
        !history_b.to_string().contains("PEER_A_CANARY"),
        "peer A text reached peer B"
    );
    let denied = cb
        .call_tool("get_message_status", json!({"requestId": request}))
        .await?;
    ensure!(denied["isError"] == true, "foreign status: {denied}");
    ensure!(
        !denied
            .to_string()
            .contains(delivery["turn"].as_str().unwrap_or("-")),
        "foreign status leaked the turn"
    );
    let transcript = f
        .app
        .get(&format!(
            "/api/threads/{}/turns",
            delivery["thread"].as_str().ok_or("thread")?
        ))
        .await?;
    ensure!(transcript[0]["origin"] == "peer:Laptop A", "{transcript}");
    ensure!(
        transcript[0]["messageOrigin"]["name"] == "Laptop A",
        "{transcript}"
    );
    ensure!(
        transcript[0]["messageOrigin"]["kind"] == "peer",
        "{transcript}"
    );

    let mut f = f;
    f.app.restart().await?;
    let restarted = McpClient::connect(&f.app, &token(&a)?).await?;
    ensure!(
        restarted.call("send_message", args).await? == delivery,
        "receipt after restart"
    );
    let history = restarted.call("read_history", json!({})).await?;
    ensure!(
        history["messages"].as_array().map(Vec::len) == Some(2),
        "history after restart: {history}"
    );
    Ok(())
}

#[tokio::test]
async fn mcp_scope_credential_failures_schema_checks_and_browser_boundaries() -> Result<()> {
    let f = Fixture::start().await?;
    let (read, client) = f.peer_client("Read only", &["agents:read"]).await?;
    let read = token(&read)?;
    ensure!(
        client.list_tools().await? == ["list_agents"],
        "read-only tools"
    );
    let mut forbidden = Vec::new();
    for name in [
        "send_message",
        "get_message_status",
        "read_history",
        "unknown_tool",
    ] {
        let response = rpc_call(&f.app, &read, name, json!({})).await?;
        ensure!(response.status == 403, "{name}: {response:?}");
        forbidden.push(response.json()?);
    }
    ensure!(
        forbidden.iter().all(|body| *body == forbidden[0]),
        "forbidden bodies differ: {forbidden:?}"
    );
    let schema = rpc_call(
        &f.app,
        &read,
        "list_agents",
        json!({"extra": "not allowed"}),
    )
    .await?;
    ensure!(schema.status == 400, "schema: {schema:?}");
    let denied = rpc_call(&f.app, &read, "read_history", json!({"limit": -1})).await?;
    ensure!(
        denied.status == 403,
        "out-of-scope schema error: {denied:?}"
    );
    let full = token(&peer(&f.app, "Laptop", &ALL).await?)?;
    for args in [
        json!({"limit": 51}),
        json!({"before": 0}),
        json!({"agent": f.agent}),
        json!({"before": 1.5}),
    ] {
        let response = rpc_call(&f.app, &full, "read_history", args.clone()).await?;
        ensure!(response.status == 400, "{args}: {response:?}");
    }
    // The fixed clock stands still: a session minted with no lifetime left
    // is the Node session that expired after 1 ms.
    let expired = mint_session(&f.app, &f.agent, None, &ALL, 0)?;
    let revoked = peer(&f.app, "Revoked", &ALL).await?;
    f.app
        .post(
            &format!(
                "/api/mcp-server/peer-tokens/{}/revoke",
                revoked["id"].as_str().ok_or("peer id")?
            ),
            json!({"id": new_id()}),
        )
        .await?;
    let revoked = token(&revoked)?;
    let long = "A".repeat(43);
    let mut bodies = Vec::new();
    for credential in [
        None,
        Some("wrong-token"),
        Some(long.as_str()),
        Some(revoked.as_str()),
        Some(expired.token.as_str()),
    ] {
        let response = rpc(&f.app, credential, "tools/list", json!({}), &[]).await?;
        ensure!(response.status == 401, "{credential:?}: {response:?}");
        bodies.push(response.text);
    }
    ensure!(
        bodies.iter().all(|body| *body == bodies[0]),
        "credential bodies differ: {bodies:?}"
    );
    let forbidden_text = forbidden[0].to_string();
    ensure!(
        bodies[0] == forbidden_text,
        "401 and 403 bodies differ: {} / {}",
        bodies[0],
        forbidden[0]
    );
    let origin = |value: &'static str| [("Origin", value)];
    let evil = rpc(
        &f.app,
        Some(&full),
        "tools/list",
        json!({}),
        &origin("https://evil.example"),
    )
    .await?;
    ensure!(evil.status == 403, "foreign origin: {evil:?}");
    let allowed = rpc(
        &f.app,
        Some(&full),
        "tools/list",
        json!({}),
        &[("Origin", ALLOWED_ORIGIN)],
    )
    .await?;
    ensure!(allowed.status == 200, "allowed origin: {allowed:?}");
    let own = rpc(
        &f.app,
        Some(&full),
        "tools/list",
        json!({}),
        &[("Origin", f.app.origin())],
    )
    .await?;
    ensure!(own.status == 200, "own origin: {own:?}");
    let cookie = format!("token={full}");
    let cookie = rpc(
        &f.app,
        None,
        "tools/list",
        json!({}),
        &[("Cookie", cookie.as_str()), ("Origin", f.app.origin())],
    )
    .await?;
    ensure!(cookie.status == 401, "cookie credential: {cookie:?}");
    let host = f
        .app
        .client()?
        .request(Method::POST, f.app.url("/mcp"))
        .header("Host", "evil.example")
        .header("Authorization", format!("Bearer {full}"))
        .send()
        .await?
        .status();
    ensure!(host == 403, "foreign host: {host}");
    let allowed_host = rpc(
        &f.app,
        Some(&full),
        "tools/list",
        json!({}),
        &[("Host", "minarbot.example:4317")],
    )
    .await?;
    ensure!(allowed_host.status == 200, "allowed host: {allowed_host:?}");
    let get = f
        .app
        .client()?
        .get(f.app.url("/mcp"))
        .header("Authorization", format!("Bearer {full}"))
        .send()
        .await?
        .status();
    ensure!(get == 405, "GET /mcp: {get}");
    let plain = f
        .app
        .client()?
        .post(f.app.url("/mcp"))
        .header("Authorization", format!("Bearer {full}"))
        .header("Content-Type", "text/plain")
        .body(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}).to_string())
        .send()
        .await?
        .status();
    ensure!(plain == 415, "text/plain: {plain}");
    Ok(())
}

#[tokio::test]
async fn cancel_message_cannot_cancel_another_peer_or_owner_work() -> Result<()> {
    let f = Fixture::start().await?;
    f.hold();
    let (_a, ca) = f.peer_client("A", &ALL).await?;
    let (_b, cb) = f.peer_client("B", &ALL).await?;
    let request = new_id();
    let delivery = ca
        .call(
            "send_message",
            json!({"requestId": request, "recipient": f.agent, "text": "Peer work"}),
        )
        .await?;
    f.model.wait_calls(1).await?;
    let foreign = cb
        .call_tool("cancel_message", json!({"requestId": request}))
        .await?;
    ensure!(foreign["isError"] == true, "foreign cancel: {foreign}");
    let status = ca
        .call("get_message_status", json!({"requestId": request}))
        .await?;
    ensure!(status["status"] == "running", "status: {status}");
    let owner = f.owner_turn("Owner work").await?;
    let owner_cancel = ca
        .call_tool("cancel_message", json!({"requestId": owner}))
        .await?;
    ensure!(
        owner_cancel["isError"] == true,
        "owner work cancel: {owner_cancel}"
    );
    for attempt in 0..2 {
        let cancelled = ca
            .call("cancel_message", json!({"requestId": request}))
            .await?;
        ensure!(
            cancelled["status"] == "cancelled",
            "cancel {attempt}: {cancelled}"
        );
    }
    let turns = f
        .app
        .get(&format!(
            "/api/threads/{}/turns",
            delivery["thread"].as_str().ok_or("thread")?
        ))
        .await?;
    ensure!(turns[0]["status"] == "cancelled", "peer turn: {turns}");
    let row = f.app.row(&f.turns(), &owner).await?.ok_or("owner turn")?;
    ensure!(row["status"] != "cancelled", "owner turn: {row}");
    f.release();
    Ok(())
}

/// The server's database files, for "never persisted" checks.
fn database_bytes(app: &Server) -> Result<Vec<u8>> {
    let mut bytes = std::fs::read(app.database())?;
    for suffix in ["-wal", "-journal"] {
        let path = app.database().with_extension(format!("sqlite{suffix}"));
        if path.exists() {
            bytes.extend(std::fs::read(path)?);
        }
    }
    Ok(bytes)
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

#[tokio::test]
async fn owner_peer_token_routes_are_idempotent_reveal_once_validate_boundaries_and_never_persist_bearer_canaries(
) -> Result<()> {
    let mut f = Fixture::start().await?;
    let id = new_id();
    let input = json!({"id": id, "name": "Work laptop", "scopes": ALL});
    let result = f
        .app
        .post("/api/mcp-server/peer-tokens", input.clone())
        .await?;
    let canary = token(&result)?;
    ensure!(
        canary.len() == 43
            && canary
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
        "token shape: {canary}"
    );
    let repeat = f
        .app
        .post("/api/mcp-server/peer-tokens", input.clone())
        .await?;
    ensure!(repeat["token"].is_null(), "token revealed twice: {repeat}");
    ensure!(repeat["alreadyCreated"] == true, "repeat: {repeat}");
    let listing = f
        .app
        .request(Method::GET, "/api/mcp-server/peer-tokens")?
        .send()
        .await?
        .text()
        .await?;
    ensure!(!listing.contains(&canary), "listing reveals the token");
    let mut drift = input.clone();
    drift["name"] = json!("drift");
    f.app
        .post_expect("/api/mcp-server/peer-tokens", drift, 409)
        .await?;
    for patch in [
        json!({"scopes": []}),
        json!({"scopes": ["history:read", "history:read"]}),
        json!({"scopes": ["root:all"]}),
        json!({"name": ""}),
        json!({"name": "bad\nname"}),
        json!({"id": "bad"}),
        json!({"extra": true}),
    ] {
        let mut body = input.clone();
        body["id"] = json!(new_id());
        for (key, value) in patch.as_object().ok_or("patch")? {
            body[key] = value.clone();
        }
        f.app
            .post_expect("/api/mcp-server/peer-tokens", body, 400)
            .await?;
    }
    let untrusted = json!({"id": new_id(), "name": "Untrusted", "scopes": ALL}).to_string();
    let origin = f.app.origin().to_owned();
    let bearer = format!("Bearer {canary}");
    let bad_headers: [Vec<(&str, &str)>; 4] = [
        vec![("Content-Type", "application/json")],
        vec![
            ("Origin", "https://evil.example"),
            ("Content-Type", "application/json"),
        ],
        vec![("Origin", origin.as_str()), ("Content-Type", "text/plain")],
        vec![
            ("Origin", origin.as_str()),
            ("Content-Type", "application/json"),
            ("Authorization", bearer.as_str()),
        ],
    ];
    for headers in bad_headers {
        let mut request = f
            .app
            .client()?
            .post(f.app.url("/api/mcp-server/peer-tokens"));
        for (name, value) in &headers {
            request = request.header(*name, *value);
        }
        let status = request.body(untrusted.clone()).send().await?.status();
        ensure!(status == 403, "{headers:?}: {status}");
    }
    let session = mint_session(&f.app, &f.agent, None, &ALL, 60_000)?;
    ensure!(verified(&f.app, &canary).await?, "peer token works");
    f.app.restart().await?;
    ensure!(
        !verified(&f.app, &session.token).await?,
        "session survived restart"
    );
    ensure!(
        verified(&f.app, &canary).await?,
        "peer token lost on restart"
    );
    let revoke = json!({"id": new_id()});
    let path = format!("/api/mcp-server/peer-tokens/{id}/revoke");
    let revoked = f.app.post(&path, revoke.clone()).await?;
    ensure!(
        f.app.post(&path, revoke).await? == revoked,
        "revocation replay"
    );
    ensure!(
        !verified(&f.app, &canary).await?,
        "revoked token still works"
    );
    let listed = f.app.get("/api/mcp-server/peer-tokens").await?;
    ensure!(!listed["items"][0]["revoked"].is_null(), "{listed}");
    ensure!(!listed["items"][0]["lastUsed"].is_null(), "{listed}");
    ensure!(!listed.to_string().contains(&canary), "listing leak");
    let stored = database_bytes(&f.app)?;
    ensure!(!contains(&stored, &canary), "peer token stored in clear");
    ensure!(
        !contains(&stored, &session.token),
        "session token stored in clear"
    );
    let (stdout, stderr) = f.app.output()?;
    let log = format!("{stdout}{stderr}");
    ensure!(!log.contains(&canary), "peer token logged");
    ensure!(!log.contains(&session.token), "session token logged");
    Ok(())
}

#[tokio::test]
async fn agent_session_binds_agent_turn_reuses_conversation_history_rules_and_revokes_at_run_end(
) -> Result<()> {
    let f = Fixture::start().await?;
    let owner = f.owner_turn("Earlier same-agent message").await?;
    f.app.wait_status(&f.turns(), &owner, "done").await?;
    f.hold();
    let active = f.owner_turn("Current session").await?;
    f.app.wait_status(&f.turns(), &active, "running").await?;
    // Since #87 (aa2feb07) a session's `list_agents` is the native
    // coordination tool under `orchestration`, which every harness session
    // carries (`registry::mint`).
    let session = mint_session(
        &f.app,
        &f.agent,
        Some(&active),
        &[
            "agents:read",
            "agents:message",
            "history:read",
            "orchestration",
        ],
        60_000,
    )?;
    ensure!(verified(&f.app, &session.token).await?, "session verifies");
    let identity = token_row(&f.app, &session.token)?.ok_or("session row")?;
    ensure!(identity["kind"] == "session", "kind: {identity}");
    ensure!(identity["agent"] == f.agent.as_str(), "agent: {identity}");
    ensure!(identity["turn"] == active.as_str(), "turn: {identity}");
    let client = McpClient::connect(&f.app, &session.token).await?;
    let history = client.call("read_history", json!({})).await?;
    let ids: Vec<&Value> = history["messages"]
        .as_array()
        .ok_or("messages")?
        .iter()
        .map(|row| &row["id"])
        .collect();
    ensure!(ids == [&json!(owner)], "session history: {history}");
    let agents = client.call("list_agents", json!({})).await?;
    ensure!(
        agents["agents"].as_array().is_some_and(Vec::is_empty),
        "list_agents: {agents}"
    );
    f.release();
    until_within(
        "session revoked at run end",
        Duration::from_secs(20),
        || async { Ok((!verified(&f.app, &session.token).await?).then_some(())) },
    )
    .await?;
    let ended = rpc(&f.app, Some(&session.token), "tools/list", json!({}), &[]).await?;
    ensure!(ended.status == 401, "ended session: {ended:?}");
    let unbound = mint_session(&f.app, &f.agent, None, &["agents:read"], 60_000)?;
    revoke_session(&f.app, &unbound.id)?;
    ensure!(
        !verified(&f.app, &unbound.token).await?,
        "revoked unbound session"
    );
    // The Node facade refused unknown agents and turns that are not running.
    ensure!(
        mint_session(&f.app, &new_id(), None, &ALL, 60_000).is_err(),
        "unknown agent minted"
    );
    ensure!(
        mint_session(&f.app, &f.agent, Some(&owner), &ALL, 60_000).is_err(),
        "finished turn minted"
    );
    Ok(())
}

#[tokio::test]
async fn failed_bearer_authentication_is_rate_limited_with_uniform_responses() -> Result<()> {
    let f = Fixture::start().await?;
    let long = "A".repeat(43);
    for attempt in 0..12 {
        let response = rpc(&f.app, Some(&long), "tools/list", json!({}), &[]).await?;
        ensure!(response.status == 401, "{attempt}: {response:?}");
        ensure!(
            response.json()? == json!({"error": "MCP authorization failed."}),
            "{attempt}: {}",
            response.text
        );
        if attempt >= 10 {
            ensure!(
                response.retry_after.as_deref() == Some("60"),
                "{attempt}: retry-after {:?}",
                response.retry_after
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn interrupted_peer_work_keeps_its_original_receipt_across_restart_and_never_replays(
) -> Result<()> {
    let mut f = Fixture::start().await?;
    let (created, client) = f.peer_client("Laptop", &ALL).await?;
    f.hold();
    let request = new_id();
    let args =
        json!({"requestId": request, "recipient": f.agent, "text": "Interrupt this delivery"});
    let receipt = client.call("send_message", args.clone()).await?;
    f.model.wait_calls(1).await?;
    f.app.restart().await?;
    f.release();
    let restarted = McpClient::connect(&f.app, &token(&created)?).await?;
    let status = restarted
        .call("get_message_status", json!({"requestId": request}))
        .await?;
    ensure!(
        status["status"] != "queued" && status["status"] != "running",
        "interrupted status: {status}"
    );
    ensure!(
        restarted.call("send_message", args).await? == receipt,
        "receipt after restart"
    );
    ensure!(
        restarted
            .call("get_message_status", json!({"requestId": request}))
            .await?
            == status,
        "status changed"
    );
    ensure!(f.model.calls() == 1, "replayed: {} calls", f.model.calls());
    let history = restarted.call("read_history", json!({})).await?;
    ensure!(
        history["messages"]
            .as_array()
            .map(|rows| rows.iter().map(|row| row["id"].clone()).collect::<Vec<_>>())
            == Some(vec![receipt["turn"].clone()]),
        "history: {history}"
    );
    Ok(())
}
