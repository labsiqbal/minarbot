//! MCP feature tools for owner peers and agent sessions: families, scopes,
//! isolation, receipts, secret-free listings, delegation and cursors (port
//! of `app/mcp-feature-tools.test.ts`).

use crate::{
    fx_support::gated,
    rg_support::{mint_session, peer, rpc_call, token, McpClient, ALL_SCOPES, SESSION_SCOPES},
};
use minarbot_testkit::{
    gated::{Answer, Gate, GatedModel},
    prelude::*,
    stub::{Stub, StubReply},
    Server, FIXED_CLOCK,
};

const GRAPH: &str = r#"{"entry":"out","maxSteps":1,"nodes":[{"id":"out","kind":"output"}]}"#;

fn graph() -> Result<Value> {
    Ok(serde_json::from_str(GRAPH)?)
}

/// An instant schedule an hour after the fixed clock (`spec()`).
fn schedule() -> Value {
    json!({"kind": "instant", "at": FIXED_CLOCK + 3_600_000})
}

fn id_of(value: &Value) -> Result<String> {
    Ok(value["id"].as_str().ok_or("id")?.to_owned())
}

/// Composio accounts (with secret canaries) and a tool-less external MCP
/// server, on one loopback stub (the Node provider served both).
async fn remote() -> Result<Stub> {
    Stub::start(|request| {
        if request.path().contains("connected_accounts") {
            return StubReply::ok(json!({"items": [{"id": "account_fixture", "status": "ACTIVE",
                "toolkit": {"slug": "gmail"}, "connection_data": {"token": "COMPOSIO_KEY_CANARY"},
                "secret": "COMPOSIO_KEY_CANARY"}]}));
        }
        if request.path().ends_with("/toolkits/categories") {
            // Composio's key check (`/api/connected-apps/key`).
            return StubReply::ok(json!({"items": []}));
        }
        if request.path() == "/mcp" {
            let data = request.json();
            if data["id"].is_null() {
                return StubReply::Raw(202, Vec::new(), Vec::new());
            }
            let result = match data["method"].as_str() {
                Some("initialize") => json!({"protocolVersion": "2025-03-26",
                    "capabilities": {"tools": {}}, "serverInfo": {"name": "fixture", "version": "1"}}),
                Some("tools/list") => json!({"tools": []}),
                _ => json!({}),
            };
            return StubReply::ok(json!({"jsonrpc": "2.0", "id": data["id"], "result": result}));
        }
        StubReply::Json(404, json!({}))
    })
    .await
}

/// The Node `fixture()`: agents A and B in their own projects, a held turn
/// of A in its side conversation, owner peer tokens and A's session.
struct Fixture {
    _root: TestRoot,
    app: Server,
    _model: GatedModel,
    gate: Gate,
    remote: Stub,
    a: String,
    b: String,
    p: String,
    q: String,
    thread: String,
    private_thread: String,
    turn: String,
}

impl Fixture {
    async fn start() -> Result<Self> {
        let model = GatedModel::start().await?;
        let gate = Gate::new();
        let held = gate.clone();
        model.respond(move |request| {
            if request.mentions("ACTIVE_M4_TURN") {
                Answer::after(&held, Answer::text("Fixture reply"))
            } else {
                Answer::text("Fixture reply")
            }
        });
        let remote = remote().await?;
        let root = TestRoot::new()?;
        let app = root
            .start(gated(&model).option("composio", json!({"baseURL": remote.origin()})))
            .await?;
        let a = id_of(
            &app.post(
                "/api/agents",
                json!({"runtime": "api", "id": new_id(), "name": "A", "role": "Agent A",
                    "model": "fixture", "instructions": "Help A."}),
            )
            .await?,
        )?;
        let b = id_of(
            &app.post(
                "/api/agents",
                json!({"runtime": "api", "id": new_id(), "name": "B", "role": "Agent B",
                    "model": "fixture", "instructions": "PRIVATE_B_INSTRUCTIONS"}),
            )
            .await?,
        )?;
        let p = id_of(
            &app.post(
                "/api/projects",
                json!({"id": new_id(), "name": "Project A", "members": [{"agent": a, "write": false}]}),
            )
            .await?,
        )?;
        let q = id_of(
            &app.post(
                "/api/projects",
                json!({"id": new_id(), "name": "PRIVATE_PROJECT_B",
                    "members": [{"agent": b, "write": false}]}),
            )
            .await?,
        )?;
        let thread = id_of(
            &app.post(
                &format!("/api/agents/{a}/threads"),
                json!({"id": new_id(), "name": "Current A"}),
            )
            .await?,
        )?;
        let private_thread = id_of(
            &app.post(
                &format!("/api/agents/{b}/threads"),
                json!({"id": new_id(), "name": "PRIVATE_B_CONVERSATION"}),
            )
            .await?,
        )?;
        let turn = new_id();
        let turns = format!("/api/threads/{thread}/turns");
        app.post(&turns, json!({"id": turn, "text": "ACTIVE_M4_TURN"}))
            .await?;
        app.wait_status(&turns, &turn, "running").await?;
        Ok(Self {
            _root: root,
            app,
            _model: model,
            gate,
            remote,
            a,
            b,
            p,
            q,
            thread,
            private_thread,
            turn,
        })
    }

    /// `createPeerToken` with every scope (or `scopes`).
    async fn peer(&self, scopes: &[&str]) -> Result<String> {
        token(&peer(&self.app, "Owner", scopes).await?)
    }

    /// `mintAgentSession` for A's active turn.
    fn session(&self, scopes: &[&str]) -> Result<String> {
        Ok(mint_session(&self.app, &self.a, Some(&self.turn), scopes, 60_000)?.token)
    }

    async fn client(&self, token: &str) -> Result<McpClient> {
        McpClient::connect(&self.app, token).await
    }

    fn remote_mcp(&self) -> String {
        format!("{}/mcp", self.remote.origin())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.gate.open();
    }
}

fn denied_by_authorization(result: Result<Value>) -> bool {
    matches!(result, Err(error) if error.to_string().contains("MCP authorization failed"))
}

fn len(value: &Value) -> Option<usize> {
    value.as_array().map(Vec::len)
}

#[tokio::test]
async fn m4_real_owner_and_session_mcp_http_clients_read_and_write_feature_families() -> Result<()>
{
    let f = Fixture::start().await?;
    let owner = f.client(&f.peer(&ALL_SCOPES).await?).await?;
    let session = f.client(&f.session(&SESSION_SCOPES)?).await?;
    ensure!(
        owner
            .list_tools()
            .await?
            .iter()
            .any(|t| t == "delegate_task"),
        "owner lacks delegate_task"
    );
    ensure!(
        session
            .list_tools()
            .await?
            .iter()
            .any(|t| t == "create_routine"),
        "session lacks create_routine"
    );
    ensure!(
        owner.call("read_agent", json!({"agent": f.a})).await?["name"] == "A",
        "read_agent"
    );
    let agent = owner
        .call(
            "create_agent",
            json!({"requestId": new_id(), "name": "MCP agent", "model": "fixture",
                "instructions": "Help.", "toolBudget": 25}),
        )
        .await?;
    ensure!(agent["tool_budget"] == 25, "created: {agent}");
    let agent = id_of(&agent)?;
    owner
        .call(
            "update_agent",
            json!({"requestId": new_id(), "agent": agent, "revision": 1, "role": "New role",
                "toolBudget": 30}),
        )
        .await?;
    ensure!(
        owner.call("read_agent", json!({"agent": agent})).await?["tool_budget"] == 30,
        "updated tool budget"
    );
    let hidden = owner
        .call(
            "hide_agent",
            json!({"requestId": new_id(), "agent": f.b, "revision": 0, "confirmed": true}),
        )
        .await?;
    ensure!(hidden["hidden"] == true, "hidden: {hidden}");
    let open = owner
        .call_tool(
            "open_computer",
            json!({"requestId": new_id(), "agent": f.a}),
        )
        .await?;
    ensure!(open["isError"] == true, "open_computer: {open}");
    ensure!(open.to_string().contains("403"), "open_computer: {open}");
    let side = id_of(
        &owner
            .call(
                "create_conversation",
                json!({"requestId": new_id(), "agent": agent, "name": "MCP side"}),
            )
            .await?,
    )?;
    owner
        .call("list_conversations", json!({"agent": agent}))
        .await?;
    owner
        .call(
            "read_conversation",
            json!({"kind": "thread", "conversation": side}),
        )
        .await?;
    owner
        .call(
            "rename_conversation",
            json!({"requestId": new_id(), "thread": side, "revision": 1, "name": "Renamed"}),
        )
        .await?;
    owner
        .call(
            "archive_conversation",
            json!({"requestId": new_id(), "thread": side, "revision": 2}),
        )
        .await?;
    owner
        .call(
            "restore_conversation",
            json!({"requestId": new_id(), "thread": side, "revision": 3}),
        )
        .await?;
    let project = id_of(
        &owner
            .call(
                "create_project",
                json!({"requestId": new_id(), "name": "MCP project",
                    "members": [{"agent": agent, "write": false}]}),
            )
            .await?,
    )?;
    owner.call("list_projects", json!({})).await?;
    owner
        .call("read_project", json!({"project": project}))
        .await?;
    let section = owner
        .call(
            "add_project_section",
            json!({"requestId": new_id(), "project": project, "revision": 1, "name": "research"}),
        )
        .await?;
    ensure!(len(&section["sections"]) == Some(2), "sections: {section}");
    let routine = id_of(
        &owner
            .call(
                "create_routine",
                json!({"requestId": new_id(), "agent": agent, "name": "MCP routine",
                    "prompt": "Work", "schedule": schedule(), "enabled": false,
                    "trigger": "manual", "project": project, "reportTo": "direct"}),
            )
            .await?,
    )?;
    owner
        .call("list_routines", json!({"project": project}))
        .await?;
    owner
        .call("read_routine", json!({"routine": routine}))
        .await?;
    for (tool, revision, extra) in [
        ("resume_routine", 1, json!({})),
        ("pause_routine", 2, json!({})),
        ("update_routine", 3, json!({"name": "Changed"})),
        ("run_routine", 4, json!({})),
    ] {
        let mut args = json!({"requestId": new_id(), "routine": routine, "revision": revision});
        for (key, value) in extra.as_object().ok_or("extra")? {
            args[key] = value.clone();
        }
        owner.call(tool, args).await?;
    }
    let workflow = id_of(
        &f.app
            .post(
                &format!("/api/agents/{agent}/workflows"),
                json!({"id": new_id(), "revision": 0, "name": "MCP workflow", "graph": graph()?}),
            )
            .await?,
    )?;
    owner.call("list_workflows", json!({})).await?;
    owner
        .call("read_workflow", json!({"workflow": workflow}))
        .await?;
    let run = owner
        .call(
            "start_workflow",
            json!({"requestId": new_id(), "workflow": workflow, "revision": 1, "input": "Input"}),
        )
        .await?;
    owner
        .call("read_workflow_run", json!({"run": run["id"]}))
        .await?;
    for client in [&owner, &session] {
        client.call("list_machines", json!({})).await?;
        client.call("read_update_status", json!({})).await?;
        client
            .call("list_connected_apps", json!({"project": f.p}))
            .await?;
        client
            .call("list_mcp_connections", json!({"project": f.p}))
            .await?;
        client
            .call("list_project_environment", json!({"project": f.p}))
            .await?;
        client
            .call("read_computer_status", json!({"agent": f.a}))
            .await?;
        client.call("read_usage", json!({})).await?;
        client.call("list_skills", json!({})).await?;
    }
    session.call("read_agent", json!({"agent": f.a})).await?;
    let conversations = session
        .call("list_conversations", json!({"agent": f.a}))
        .await?;
    ensure!(
        len(&conversations["items"]) == Some(1),
        "session conversations: {conversations}"
    );
    session
        .call(
            "read_conversation",
            json!({"kind": "thread", "conversation": f.thread}),
        )
        .await?;
    session
        .call(
            "rename_conversation",
            json!({"requestId": new_id(), "thread": f.thread, "revision": 1,
                "name": "Session title"}),
        )
        .await?;
    session
        .call(
            "create_conversation",
            json!({"requestId": new_id(), "agent": f.a, "name": "Agent side"}),
        )
        .await?;
    session.call("list_projects", json!({})).await?;
    session
        .call("read_project", json!({"project": f.p}))
        .await?;
    session
        .call(
            "add_project_section",
            json!({"requestId": new_id(), "project": f.p, "revision": 1, "name": "planning"}),
        )
        .await?;
    let session_routine = id_of(
        &session
            .call(
                "create_routine",
                json!({"requestId": new_id(), "agent": f.a, "name": "Session routine",
                    "prompt": "Work", "schedule": schedule(), "enabled": false,
                    "trigger": "manual", "project": f.p}),
            )
            .await?,
    )?;
    session.call("list_routines", json!({})).await?;
    // Since #87 (5528da5e) sessions get the native routine readers, which
    // take the routine as `id`.
    session
        .call("read_routine", json!({"id": session_routine}))
        .await?;
    session
        .call(
            "pause_routine",
            json!({"requestId": new_id(), "routine": session_routine, "revision": 1}),
        )
        .await?;
    let session_workflow = id_of(
        &f.app
            .post(
                &format!("/api/agents/{}/workflows", f.a),
                json!({"id": new_id(), "revision": 0, "name": "Session workflow",
                    "graph": graph()?}),
            )
            .await?,
    )?;
    session.call("list_workflows", json!({})).await?;
    session
        .call("read_workflow", json!({"workflow": session_workflow}))
        .await?;
    let session_run = session
        .call(
            "start_workflow",
            json!({"requestId": new_id(), "workflow": session_workflow, "revision": 1,
                "input": "Session input"}),
        )
        .await?;
    session
        .call("read_workflow_run", json!({"run": session_run["id"]}))
        .await?;
    Ok(())
}

#[tokio::test]
async fn m4_scope_families_and_session_resource_isolation() -> Result<()> {
    let f = Fixture::start().await?;
    let limited = f.peer(&["agents:read"]).await?;
    let scopes: Vec<&str> = ALL_SCOPES
        .iter()
        .copied()
        .filter(|scope| *scope != "machine:update")
        .collect();
    let session = f.client(&f.session(&scopes)?).await?;
    for name in [
        "read_agent",
        "create_agent",
        "list_conversations",
        "create_conversation",
        "list_projects",
        "create_project",
        "list_routines",
        "create_routine",
        "list_workflows",
        "start_workflow",
        "list_machines",
        "list_project_environment",
        "read_usage",
        "list_skills",
        "read_computer_status",
        "open_computer",
        "delegate_task",
    ] {
        let response = rpc_call(&f.app, &limited, name, json!({})).await?;
        ensure!(response.status == 403, "{name}: {response:?}");
    }
    let denied = [
        ("read_agent", json!({"agent": f.b})),
        ("list_conversations", json!({"agent": f.b})),
        (
            "read_conversation",
            json!({"kind": "thread", "conversation": f.private_thread}),
        ),
        ("read_project", json!({"project": f.q})),
        (
            "add_project_section",
            json!({"requestId": new_id(), "project": f.q, "revision": 1, "name": "Bad"}),
        ),
        ("list_project_environment", json!({"project": f.q})),
        ("list_connected_apps", json!({"project": f.q})),
        ("list_mcp_connections", json!({"project": f.q})),
        (
            "create_routine",
            json!({"requestId": new_id(), "agent": f.a, "name": "Bad", "prompt": "Bad",
                "schedule": schedule(), "enabled": false, "project": f.q}),
        ),
        (
            "add_project_member",
            json!({"requestId": new_id(), "project": f.p, "agent": f.b, "revision": 1,
                "write": true, "confirmWrite": true}),
        ),
        (
            "create_agent",
            json!({"requestId": new_id(), "name": "Bad", "model": "x", "instructions": "x"}),
        ),
    ];
    for (name, args) in denied {
        ensure!(
            denied_by_authorization(session.call_tool(name, args).await),
            "{name} was not refused"
        );
    }
    let projects = session.call("list_projects", json!({})).await?;
    let ids: Vec<&Value> = projects["items"]
        .as_array()
        .ok_or("items")?
        .iter()
        .map(|project| &project["id"])
        .collect();
    ensure!(ids == [&json!(f.p)], "session projects: {projects}");
    ensure!(
        !projects.to_string().contains("PRIVATE_PROJECT_B"),
        "foreign project leaked"
    );
    let other = id_of(
        &f.app
            .post(
                &format!("/api/agents/{}/threads", f.a),
                json!({"id": new_id(), "name": "Other same agent"}),
            )
            .await?,
    )?;
    ensure!(
        denied_by_authorization(
            session
                .call_tool(
                    "read_conversation",
                    json!({"kind": "thread", "conversation": other}),
                )
                .await
        ),
        "another conversation of the same agent was readable"
    );
    Ok(())
}

#[tokio::test]
async fn m4_idempotency_confirmed_deletes_active_work_guard_and_cursor_bounds() -> Result<()> {
    let f = Fixture::start().await?;
    let p = f.peer(&ALL_SCOPES).await?;
    let c = f.client(&p).await?;
    let second = f.client(&f.peer(&ALL_SCOPES).await?).await?;
    let input = json!({"requestId": new_id(), "agent": f.b, "name": "Idempotent"});
    let first = c.call("create_conversation", input.clone()).await?;
    ensure!(
        c.call("create_conversation", input.clone()).await? == first,
        "receipt replay"
    );
    ensure!(
        second.call("create_conversation", input.clone()).await?["id"] != first["id"],
        "another caller reused the receipt"
    );
    let mut drift = input.clone();
    drift["name"] = json!("Drift");
    let conflict = c.call_tool("create_conversation", drift).await?;
    ensure!(conflict["isError"] == true, "conflict: {conflict}");
    ensure!(conflict.to_string().contains("409"), "conflict: {conflict}");
    let unconfirmed = rpc_call(
        &f.app,
        &p,
        "delete_conversation",
        json!({"requestId": new_id(), "thread": first["id"], "revision": 1}),
    )
    .await?;
    ensure!(unconfirmed.status == 400, "unconfirmed: {unconfirmed:?}");
    let deletion =
        json!({"requestId": new_id(), "thread": first["id"], "revision": 1, "confirmed": true});
    let deleted = c.call("delete_conversation", deletion.clone()).await?;
    ensure!(
        c.call("delete_conversation", deletion).await? == deleted,
        "delete replay"
    );
    let busy = c
        .call_tool(
            "delete_conversation",
            json!({"requestId": new_id(), "thread": f.thread, "revision": 1, "confirmed": true}),
        )
        .await?;
    ensure!(busy["isError"] == true, "busy delete: {busy}");
    ensure!(busy.to_string().contains("409"), "busy delete: {busy}");
    for index in 0..24 {
        c.call(
            "create_conversation",
            json!({"requestId": new_id(), "agent": f.b, "name": format!("Page {index}")}),
        )
        .await?;
    }
    let page = c
        .call("list_conversations", json!({"agent": f.b, "limit": 10}))
        .await?;
    ensure!(len(&page["items"]) == Some(10), "page: {page}");
    ensure!(!page["next_after"].is_null(), "next_after: {page}");
    let next = c
        .call(
            "list_conversations",
            json!({"agent": f.b, "limit": 10, "after": page["next_after"]}),
        )
        .await?;
    let seen: Vec<&Value> = page["items"]
        .as_array()
        .ok_or("items")?
        .iter()
        .map(|row| &row["id"])
        .collect();
    ensure!(
        next["items"]
            .as_array()
            .ok_or("next items")?
            .iter()
            .all(|row| !seen.contains(&&row["id"])),
        "pages overlap: {next}"
    );
    for limit in [0, 1, 9, 51] {
        let response = rpc_call(&f.app, &p, "list_projects", json!({"limit": limit})).await?;
        ensure!(response.status == 400, "limit {limit}: {response:?}");
    }
    let nested = rpc_call(
        &f.app,
        &p,
        "create_project",
        json!({"requestId": new_id(), "name": "Nested bad",
            "members": [{"agent": f.a, "write": false, "token": "SECRET"}]}),
    )
    .await?;
    ensure!(nested.status == 400, "nested member field: {nested:?}");
    Ok(())
}

#[tokio::test]
async fn m4_secret_free_listings_and_revocation_before_receipt_replay() -> Result<()> {
    let f = Fixture::start().await?;
    let peer_token = f.peer(&ALL_SCOPES).await?;
    let owner = f.client(&peer_token).await?;
    let session = f.client(&f.session(&SESSION_SCOPES)?).await?;
    let environment = "ENVIRONMENT_CANARY_with_quote\"and_newline\n";
    f.app
        .post(
            &format!("/api/projects/{}/environment", f.p),
            json!({"id": new_id(), "name": "SERVICE_TOKEN", "value": environment}),
        )
        .await?;
    f.app
        .post(
            "/api/connected-apps/key",
            json!({"id": new_id(), "revision": 0, "key": "COMPOSIO_KEY_CANARY"}),
        )
        .await?;
    let tested = f
        .app
        .post(
            &format!("/api/projects/{}/mcp/test", f.p),
            json!({"id": new_id(), "name": "Fixture MCP", "url": f.remote_mcp(),
                "token": "MCP_TOKEN_CANARY"}),
        )
        .await?;
    f.app
        .post(
            &format!("/api/projects/{}/mcp", f.p),
            json!({"id": new_id(), "test": tested["id"]}),
        )
        .await?;
    f.app
        .post(
            &format!("/api/agents/{}/edit", f.b),
            json!({"id": new_id(), "revision": 1, "name": "B", "model": "fixture",
                "instructions": format!("COMPOSIO_KEY_CANARY MCP_TOKEN_CANARY {environment}")}),
        )
        .await?;
    let mut results = vec![
        owner.call("read_agent", json!({"agent": f.b})).await?,
        session.call("read_agent", json!({"agent": f.a})).await?,
    ];
    for client in [&owner, &session] {
        for name in [
            "list_project_environment",
            "list_connected_apps",
            "list_mcp_connections",
        ] {
            results.push(client.call(name, json!({"project": f.p})).await?);
        }
    }
    let encoded = json!(results).to_string();
    let escaped = serde_json::to_string(environment)?;
    for secret in [
        escaped.trim_matches('"'),
        "ENVIRONMENT_CANARY",
        "COMPOSIO_KEY_CANARY",
        "MCP_TOKEN_CANARY",
        peer_token.as_str(),
    ] {
        ensure!(!encoded.contains(secret), "{secret:?} leaked: {encoded}");
    }
    ensure!(encoded.contains("SERVICE_TOKEN"), "names listed: {encoded}");
    let command = json!({"requestId": new_id(), "project": f.p, "revision": 1,
        "name": "Scope receipt"});
    session.call("add_project_section", command.clone()).await?;
    let changed = owner
        .call(
            "remove_project_member",
            json!({"requestId": new_id(), "project": f.p, "agent": f.a, "revision": 2}),
        )
        .await?;
    ensure!(len(&changed["members"]) == Some(0), "members: {changed}");
    ensure!(
        denied_by_authorization(session.call_tool("add_project_section", command).await),
        "receipt replayed after membership removal"
    );
    ensure!(
        denied_by_authorization(
            session
                .call_tool("list_project_environment", json!({"project": f.p}))
                .await
        ),
        "environment readable after membership removal"
    );
    Ok(())
}

#[tokio::test]
async fn m4_delegation_mcp_tools_preserve_parent_session_and_caller_receipts() -> Result<()> {
    let f = Fixture::start().await?;
    let session = f.client(&f.session(&SESSION_SCOPES)?).await?;
    // Since #87 (aa2feb07) a session delegates through the native tool
    // (`{agent, task, wait}`), which takes its parent from the session.
    let native = session
        .call(
            "delegate_task",
            json!({"agent": f.b, "task": "Delegated fixture"}),
        )
        .await?;
    ensure!(native["taskId"].is_string(), "session delegation: {native}");
    // Caller receipts stay with the feature tool, which an owner peer calls
    // with the parent turn.
    let owner = f.client(&f.peer(&ALL_SCOPES).await?).await?;
    let args = json!({"requestId": new_id(), "agent": f.b, "task": "Delegated fixture",
        "turn": f.turn});
    let delegated = owner.call("delegate_task", args.clone()).await?;
    ensure!(delegated["taskId"].is_string(), "delegated: {delegated}");
    ensure!(
        owner.call("delegate_task", args.clone()).await? == delegated,
        "delegation receipt replay"
    );
    session
        .call("task_status", json!({"taskId": delegated["taskId"]}))
        .await?;
    let wait = json!({"requestId": new_id(), "taskId": delegated["taskId"], "timeoutSeconds": 2});
    let waited = session.call("wait_task", wait.clone()).await?;
    ensure!(
        session.call("wait_task", wait).await? == waited,
        "wait receipt replay"
    );
    // The blocking wait returns to the same live run: the turn and its
    // session stay usable.
    let parent = f
        .app
        .row(&format!("/api/threads/{}/turns", f.thread), &f.turn)
        .await?
        .ok_or("parent turn")?;
    ensure!(parent["status"] == "running", "parent after wait: {parent}");
    let cancel = json!({"requestId": new_id(), "taskId": delegated["taskId"]});
    let cancelled = session.call("cancel_task", cancel.clone()).await?;
    ensure!(
        session.call("cancel_task", cancel).await? == cancelled,
        "cancel receipt replay"
    );
    let mut changed = args;
    changed["task"] = json!("Changed");
    let drift = owner.call_tool("delegate_task", changed).await?;
    ensure!(drift["isError"] == true, "drift: {drift}");
    ensure!(drift.to_string().contains("409"), "drift: {drift}");
    Ok(())
}

#[tokio::test]
async fn m4_route_backed_workflow_and_mcp_cursors_do_not_skip_ten_item_page_tails() -> Result<()> {
    let f = Fixture::start().await?;
    let c = f.client(&f.peer(&ALL_SCOPES).await?).await?;
    let mut expected = Vec::new();
    for index in 0..25 {
        let workflow = f
            .app
            .post(
                &format!("/api/agents/{}/workflows", f.a),
                json!({"id": new_id(), "revision": 0, "name": format!("Page {index}"),
                    "graph": graph()?}),
            )
            .await?;
        expected.push(id_of(&workflow)?);
    }
    let mut found = Vec::new();
    let mut after = Value::Null;
    loop {
        let mut args = json!({"limit": 10});
        if !after.is_null() {
            args["after"] = after.clone();
        }
        let page = c.call("list_workflows", args).await?;
        let items = page["items"].as_array().ok_or("items")?;
        ensure!(items.len() <= 10, "page size: {page}");
        found.extend(
            items
                .iter()
                .filter_map(|w| w["id"].as_str().map(str::to_owned)),
        );
        after = page["next_after"].clone();
        if after.is_null() {
            break;
        }
    }
    expected.sort();
    ensure!(found == expected, "workflows: {found:?} != {expected:?}");
    let mut connections = Vec::new();
    for index in 0..15 {
        let tested = f
            .app
            .post(
                &format!("/api/projects/{}/mcp/test", f.p),
                json!({"id": new_id(), "name": format!("Page {index}"), "url": f.remote_mcp(),
                    "token": "fixture-token"}),
            )
            .await?;
        let saved = f
            .app
            .post(
                &format!("/api/projects/{}/mcp", f.p),
                json!({"id": new_id(), "test": tested["id"]}),
            )
            .await?;
        connections.push(id_of(&saved)?);
    }
    let first = c
        .call("list_mcp_connections", json!({"project": f.p, "limit": 10}))
        .await?;
    ensure!(len(&first["items"]) == Some(10), "first: {first}");
    ensure!(!first["next_after"].is_null(), "first next: {first}");
    let second = c
        .call(
            "list_mcp_connections",
            json!({"project": f.p, "limit": 10, "after": first["next_after"]}),
        )
        .await?;
    ensure!(len(&second["items"]) == Some(5), "second: {second}");
    ensure!(second["next_after"].is_null(), "second next: {second}");
    let listed: Vec<String> = first["items"]
        .as_array()
        .ok_or("first items")?
        .iter()
        .chain(second["items"].as_array().ok_or("second items")?)
        .filter_map(|item| item["id"].as_str().map(str::to_owned))
        .collect();
    connections.sort();
    ensure!(listed == connections, "connections: {listed:?}");
    Ok(())
}

#[tokio::test]
async fn manual_routine_creation_defaults_to_authenticated_agent_without_inventing_a_schedule(
) -> Result<()> {
    let f = Fixture::start().await?;
    let session = f.client(&f.session(&SESSION_SCOPES)?).await?;
    let input = json!({"requestId": new_id(), "name": "Daily sales",
        "prompt": "Read sales.csv and report paid revenue.", "enabled": false,
        "trigger": "manual", "reportTo": "direct"});
    let saved = session.call("create_routine", input.clone()).await?;
    ensure!(saved["agent"] == f.a.as_str(), "owner: {saved}");
    ensure!(saved["trigger"] == "manual", "trigger: {saved}");
    ensure!(saved["enabled"] == 0, "enabled: {saved}");
    ensure!(saved["report_to"] == "direct", "report_to: {saved}");
    ensure!(
        session.call("create_routine", input.clone()).await? == saved,
        "receipt replay"
    );
    // Since #87 (5528da5e) the session reads through the native tool
    // (`{id}`); the run history comes from the feature tool.
    let read = session
        .call("read_routine", json!({"id": saved["id"]}))
        .await?;
    ensure!(read["prompt"] == input["prompt"], "session read: {read}");
    let owner = f.client(&f.peer(&ALL_SCOPES).await?).await?;
    let history = owner
        .call("read_routine", json!({"routine": saved["id"]}))
        .await?;
    ensure!(
        history["routine"]["prompt"] == input["prompt"],
        "routine: {history}"
    );
    ensure!(len(&history["runs"]) == Some(0), "runs: {history}");
    Ok(())
}
