//! Connected apps through Composio (ports of `app/composio.test.ts` and the
//! Composio case of `app/rust-connectors.test.ts`).

use crate::fx_support::{id, row, tool_json};
use minarbot_testkit::{
    bridge::Bridge,
    db::{Fixture, Inspector},
    gated::{Answer, GatedModel, Parked},
    http::Method,
    model::ModelRequest,
    prelude::*,
    protocol::sha256_hex,
    server::Gateway,
    stub::{Stub, StubReply, StubRequest},
    Server,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

const CANARY: &str = "COMPOSIO_CANARY_6vR9pX8m7Q4b";

/// `composioToolName` from `app/test-support/composio-tool-name.ts`.
pub fn tool_name(slug: &str) -> String {
    format!("composio__{}", &sha256_hex(json!(slug).to_string())[..24])
}

fn schema() -> Value {
    json!({"type": "object", "properties": {"target": {"type": "string"}, "text": {"type": "string"}},
        "required": ["target"], "additionalProperties": false})
}

fn raw_tool(slug: &str, tags: &[&str]) -> Value {
    json!({"slug": slug, "name": slug, "description": format!("{slug} {CANARY}"), "toolkit": {"slug": "gmail"},
        "version": "20261113_00", "input_parameters": schema(), "tags": tags})
}

fn lock<T>(value: &Arc<Mutex<T>>) -> std::sync::MutexGuard<'_, T> {
    match value.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Request with explicit headers; returns status and body.
async fn raw(
    app: &Server,
    path: &str,
    body: &Value,
    headers: &[(&str, &str)],
) -> Result<(u16, Value)> {
    let mut request = app.client()?.post(app.url(path)).body(body.to_string());
    let mut origin = true;
    let mut content_type = true;
    for (name, value) in headers {
        origin &= !name.eq_ignore_ascii_case("origin");
        content_type &= !name.eq_ignore_ascii_case("content-type");
        request = request.header(*name, *value);
    }
    if origin {
        request = request.header("Origin", app.origin());
    }
    if content_type {
        request = request.header("Content-Type", "application/json");
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    let text = response.text().await?;
    Ok((
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    ))
}

#[derive(Default)]
struct Remote {
    connected: bool,
    connection_page: bool,
    fail: u8,
    tags: Vec<&'static str>,
    effects: Vec<String>,
    problems: Vec<String>,
}

fn composio_remote(state: &Arc<Mutex<Remote>>, request: &StubRequest) -> StubReply {
    let mut remote = lock(state);
    if request.header("x-api-key") != Some(CANARY) {
        remote
            .problems
            .push(format!("wrong key on {}", request.target));
    }
    match remote.fail {
        1 => return StubReply::Json(401, json!({"error": {"message": CANARY}})),
        2 => return StubReply::Raw(200, vec![], vec![b'x'; 512 * 1024 + 1]),
        3 => {
            return StubReply::Raw(
                302,
                vec![("Location".into(), "https://example.com/".into())],
                vec![],
            )
        }
        4 => return StubReply::Raw(200, vec![], format!("broken {CANARY}").into_bytes()),
        _ => {}
    }
    let path = request.path();
    let data = request.json();
    let cursor = request.query("cursor").is_some();
    match path {
        "/api/v3.1/toolkits/categories" => {
            StubReply::ok(json!({"items": [{"id": "communication", "name": "Communication"}]}))
        }
        "/api/v3.1/toolkits" => StubReply::ok(
            json!({"items": [{"slug": if cursor { "slack" } else { "gmail" },
            "name": if cursor { "Slack" } else { "Gmail" }, "meta": {"description": CANARY},
            "composio_managed_auth_schemes": ["OAUTH2"]}], "next_cursor": if cursor { Value::Null } else { json!("catalog-page-2") }}),
        ),
        "/api/v3.1/toolkits/gmail" => {
            StubReply::ok(json!({"slug": "gmail", "composio_managed_auth_schemes": ["OAUTH2"]}))
        }
        "/api/v3.1/auth_configs" => StubReply::Json(
            201,
            json!({"auth_config": {"id": "ac_gmail"}, "credentials": {"api_key": CANARY}}),
        ),
        "/api/v3.1/connected_accounts/link" => {
            if data["auth_config_id"] != "ac_gmail" {
                remote.problems.push(format!("link auth config {data}"));
            }
            StubReply::Json(
                201,
                json!({"redirect_url": "https://connect.composio.dev/link/test", "api_key": CANARY}),
            )
        }
        "/api/v3.1/connected_accounts" => {
            if !request
                .query("user_ids")
                .is_some_and(|ids| ids.starts_with("minarbot-"))
            {
                remote
                    .problems
                    .push(format!("account user ids {}", request.target));
            }
            if remote.connection_page && !cursor {
                return StubReply::ok(json!({"items": [], "next_cursor": "accounts-2"}));
            }
            let items = if remote.connected {
                json!([{"id": "ca_1", "toolkit": {"slug": "gmail"}, "status": "ACTIVE", "state": {"token": CANARY},
                    "connection_data": {"api_key": CANARY}}])
            } else {
                json!([])
            };
            StubReply::ok(json!({"items": items}))
        }
        "/api/v3.1/connected_accounts/ca_1" if request.method == "DELETE" => {
            remote.connected = false;
            StubReply::ok(json!({"deleted": true, "token": CANARY}))
        }
        "/api/v3.1/tools" => {
            let tags = remote.tags.clone();
            StubReply::ok(json!({"items": [raw_tool("GMAIL_FETCH_EMAILS", &tags),
                raw_tool("GMAIL_SEND_EMAIL", &["createHint"]), raw_tool("GMAIL_GET_AND_DELETE", &[])]}))
        }
        _ if path.starts_with("/api/v3.1/tools/execute/") => {
            if data["version"] != "20261113_00"
                || data["connected_account_id"] != "ca_1"
                || data.get("api_key").is_some()
            {
                remote.problems.push(format!("execute body {data}"));
            }
            remote
                .effects
                .push(path.rsplit('/').next().unwrap_or_default().to_owned());
            StubReply::ok(
                json!({"successful": true, "data": {"target": data["arguments"]["target"], "result": CANARY, CANARY: CANARY}, "error": null}),
            )
        }
        _ => StubReply::Json(404, json!({})),
    }
}

/// Start a write turn and wait for its approval card.
async fn send(app: &Server, turns: &str, label: &str) -> Result<(String, Value)> {
    let turn = new_id();
    app.post_expect(turns, json!({"id": turn, "text": label}), 202)
        .await?;
    let waiting = app
        .wait_row(turns, &turn, |row| {
            row["approvals"]
                .as_array()
                .is_some_and(|cards| cards.iter().any(|card| card["state"] == "approval"))
        })
        .await?;
    Ok((turn, waiting))
}

#[tokio::test]
async fn connected_apps_end_to_end_catalog_accounts_grants_tools_approvals_receipts_and_no_secrets(
) -> Result<()> {
    let state = Arc::new(Mutex::new(Remote {
        connected: true,
        tags: vec!["readOnlyHint"],
        ..Remote::default()
    }));
    let shared = state.clone();
    let remote = Stub::start(move |request| composio_remote(&shared, request)).await?;
    let mode: Arc<Mutex<&'static str>> = Arc::new(Mutex::new("read"));
    let model = GatedModel::start().await?;
    let current = mode.clone();
    model.respond(move |request| {
        let results = request.tool_results();
        if let Some(last) = results.last() {
            return Answer::text(json!(last["content"]).to_string());
        }
        let tool = match *lock(&current) {
            "read" => "GMAIL_FETCH_EMAILS",
            "unknown" => "GMAIL_GET_AND_DELETE",
            _ => "GMAIL_SEND_EMAIL",
        };
        let user = request.last_user_text();
        Answer::tool_id(
            &format!("call-{}", user.chars().take(40).collect::<String>()),
            &tool_name(tool),
            json!({"target": "recipient@example.test", "text": "Synthetic message"}),
        )
    });
    let key = Arc::new(Mutex::new(String::new()));
    let bridge = Bridge::composio_key_cell(key.clone()).await?;
    let root = TestRoot::new()?;
    let config = ServerConfig::new()
        .gateway(Gateway {
            api_key: Some("FAKE_PROVIDER_KEY".into()),
            default_model: Some("fake".into()),
            ..Gateway::new(model.base_url())
        })
        .composio(&format!("{}/api/v3.1", remote.origin()), &bridge);
    let mut app = root.start(config).await?;
    let mut responses: Vec<String> = Vec::new();
    let no_config = app.get("/api/connected-apps").await?;
    ensure!(
        no_config["configured"] == false && no_config["items"] == json!([]),
        "unconfigured {no_config}"
    );
    ensure!(remote.requests().is_empty(), "remote called without a key");
    app.post_expect(
        "/api/connected-apps/connect/gmail",
        json!({"id": new_id()}),
        503,
    )
    .await?;
    let agent = id(&app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Connected Apps Tester", "model": "fake",
                "instructions": "Use the offered app tool."}),
            201,
        )
        .await?)?;
    let other = id(&app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "No Apps", "model": "fake", "instructions": "No apps."}),
            201,
        )
        .await?)?;
    let turns = format!("/api/agents/{agent}/turns");
    let access = format!("/api/agents/{agent}/connected-apps/gmail");
    let offered_composio = |request: Option<&ModelRequest>| {
        request.is_some_and(|request| {
            request
                .tool_names()
                .iter()
                .any(|name| name.starts_with("composio__"))
        })
    };
    let unoffered = |request: Option<&ModelRequest>| {
        request
            .and_then(|request| request.tool_results().last().map(tool_json))
            .is_some_and(|result| result["status"] == 403)
    };
    let no_key = new_id();
    app.post_expect(&turns, json!({"id": no_key, "text": "unconfigured"}), 202)
        .await?;
    // #87: an unoffered tool call no longer fails the turn; MCP refuses it.
    app.wait_status(&turns, &no_key, "done").await?;
    ensure!(
        !offered_composio(model.requests().first()),
        "composio tools offered without a key"
    );
    ensure!(
        unoffered(model.requests().last()),
        "unoffered call not refused"
    );
    ensure!(remote.requests().is_empty(), "remote called without a key");
    *lock(&key) = CANARY.into();
    app.restart().await?;
    app.post(
        "/api/connected-apps/key",
        json!({"id": new_id(), "revision": 0, "key": CANARY}),
    )
    .await?;
    let catalog = app
        .get("/api/connected-apps?search=mail&category=communication")
        .await?;
    responses.push(catalog.to_string());
    ensure!(catalog["items"][0]["name"] == "Gmail", "catalog {catalog}");
    ensure!(
        catalog["items"][0]["description"] == "[REDACTED]",
        "description not redacted"
    );
    ensure!(
        remote
            .requests()
            .iter()
            .any(|request| request.target.contains("search=mail")
                && request.target.contains("category=communication")),
        "search not forwarded"
    );
    let second = app.get("/api/connected-apps?after=catalog-page-2").await?;
    responses.push(second.to_string());
    ensure!(second["items"][0]["name"] == "Slack", "page two {second}");
    let connect = json!({"id": new_id()});
    let link = app
        .post("/api/connected-apps/connect/gmail", connect.clone())
        .await?;
    responses.push(link.to_string());
    ensure!(
        link["redirect_url"] == "https://connect.composio.dev/link/test",
        "link {link}"
    );
    ensure!(
        app.post("/api/connected-apps/connect/gmail", connect.clone())
            .await?
            == link,
        "link replay"
    );
    ensure!(
        remote.count("/connected_accounts/link") == 1,
        "link created twice"
    );
    app.post_expect("/api/connected-apps/connect/slack", connect, 409)
        .await?;
    lock(&state).connection_page = true;
    let accounts = app.get("/api/connected-apps/accounts").await?;
    responses.push(accounts.to_string());
    ensure!(
        accounts["items"] == json!([{"id": "ca_1", "toolkit": "gmail", "status": "ACTIVE"}]),
        "accounts {accounts}"
    );
    lock(&state).connection_page = false;
    ensure!(
        app.get(&format!("/api/agents/{agent}/connected-apps"))
            .await?["items"]
            == json!([]),
        "grants before"
    );
    let grant = json!({"id": new_id(), "revision": 0, "enabled": true});
    ensure!(
        app.post(&access, grant.clone()).await?["revision"] == 1,
        "grant"
    );
    ensure!(
        app.post(&access, grant.clone()).await?["revision"] == 1,
        "grant replay"
    );
    let mut changed = grant.clone();
    changed["enabled"] = json!(false);
    app.post_expect(&access, changed, 409).await?;
    app.post_expect(
        &access,
        json!({"id": new_id(), "revision": 0, "enabled": false}),
        409,
    )
    .await?;
    let read = new_id();
    app.post_expect(&turns, json!({"id": read, "text": "read"}), 202)
        .await?;
    app.wait_status(&turns, &read, "done").await?;
    ensure!(
        lock(&state).effects == ["GMAIL_FETCH_EMAILS"],
        "effects {:?}",
        lock(&state).effects
    );
    ensure!(
        model.requests().last().is_some_and(|request| request
            .tool_names()
            .contains(&tool_name("GMAIL_FETCH_EMAILS"))),
        "read tool not offered"
    );
    ensure!(
        !model
            .requests()
            .iter()
            .any(|request| request.body.to_string().contains(CANARY)),
        "canary reached the model"
    );
    let other_turns = format!("/api/agents/{other}/turns");
    let other_turn = new_id();
    app.post_expect(
        &other_turns,
        json!({"id": other_turn, "text": "other"}),
        202,
    )
    .await?;
    app.wait_status(&other_turns, &other_turn, "done").await?;
    ensure!(
        !offered_composio(
            model
                .requests()
                .iter()
                .rev()
                .find(|request| request.last_user_text() == "other")
        ),
        "ungranted agent offered composio tools"
    );
    ensure!(
        unoffered(model.requests().last()),
        "ungranted call not refused"
    );
    *lock(&mode) = "write";
    let sends = |state: &Arc<Mutex<Remote>>| {
        lock(state)
            .effects
            .iter()
            .filter(|effect| *effect == "GMAIL_SEND_EMAIL")
            .count()
    };
    let (first, waiting) = send(&app, &turns, "allow once").await?;
    ensure!(lock(&state).effects.len() == 1, "write ran before approval");
    app.restart().await?;
    ensure!(
        row(&app, &turns, &first).await?["approvals"][0]["state"] == "approval",
        "approval lost"
    );
    ensure!(lock(&state).effects.len() == 1, "restart ran the write");
    let approval = waiting["approvals"][0].clone();
    let approval_id = id(&approval)?;
    let approve_path = format!("{turns}/{first}/app-approvals/{approval_id}");
    let allow = json!({"id": new_id(), "revision": approval["revision"], "decision": "allow"});
    app.post_expect(
        &format!("/api/agents/{other}/turns/{first}/app-approvals/{approval_id}"),
        allow.clone(),
        404,
    )
    .await?;
    let mut stale = allow.clone();
    stale["id"] = json!(new_id());
    stale["revision"] = json!(999);
    app.post_expect(&approve_path, stale, 409).await?;
    app.post(&approve_path, allow.clone()).await?;
    ensure!(
        app.post(&approve_path, allow.clone()).await?
            == json!({"id": approval_id, "decision": "allow"}),
        "approval replay"
    );
    let mut deny = allow.clone();
    deny["decision"] = json!("deny");
    app.post_expect(&approve_path, deny, 409).await?;
    app.wait_status(&turns, &first, "done").await?;
    ensure!(sends(&state) == 1, "sends {}", sends(&state));
    let (second_turn, waiting) = send(&app, &turns, "always").await?;
    let always = &waiting["approvals"][0];
    app.post(
        &format!("{turns}/{second_turn}/app-approvals/{}", id(always)?),
        json!({"id": new_id(), "revision": always["revision"], "decision": "always"}),
    )
    .await?;
    app.wait_status(&turns, &second_turn, "done").await?;
    app.restart().await?;
    let auto = new_id();
    app.post_expect(&turns, json!({"id": auto, "text": "always persisted"}), 202)
        .await?;
    let automated = app.wait_status(&turns, &auto, "done").await?;
    ensure!(
        automated["approvals"] == json!([]),
        "saved policy still asked"
    );
    ensure!(sends(&state) == 3, "sends {}", sends(&state));
    let peer = app
        .post(
            "/api/mcp-server/peer-tokens",
            json!({"id": new_id(), "name": "Peer", "scopes": ["agents:message"]}),
        )
        .await?;
    let receipt: Value = app
        .client()?
        .post(app.url("/mcp"))
        .header("Authorization", format!("Bearer {}", peer["token"].as_str().unwrap_or_default()))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .body(
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "send_message",
                "arguments": {"requestId": new_id(), "recipient": agent, "text": "Peer write with saved policy"}}})
            .to_string(),
        )
        .send()
        .await?
        .json()
        .await?;
    let delivery = &receipt["result"]["structuredContent"];
    let peer_turns = format!(
        "/api/threads/{}/turns",
        delivery["thread"].as_str().ok_or("thread")?
    );
    let peer_turn = delivery["turn"].as_str().ok_or("turn")?.to_owned();
    let peer_row = app
        .wait_row(&peer_turns, &peer_turn, |row| {
            row["approvals"]
                .as_array()
                .is_some_and(|cards| cards.iter().any(|card| card["state"] == "approval"))
        })
        .await?;
    ensure!(sends(&state) == 3, "peer write skipped approval");
    let peer_approval = &peer_row["approvals"][0];
    app.post(
        &format!(
            "{peer_turns}/{peer_turn}/app-approvals/{}",
            id(peer_approval)?
        ),
        json!({"id": new_id(), "revision": peer_approval["revision"], "decision": "deny"}),
    )
    .await?;
    app.wait_status(&peer_turns, &peer_turn, "done").await?;
    let policy_path = format!("/api/agents/{agent}/connected-app-policies");
    let policies = app.get(&policy_path).await?;
    ensure!(
        policies["items"].as_array().map(Vec::len) == Some(1),
        "policies {policies}"
    );
    app.post(
        &policy_path,
        json!({"id": new_id(), "fingerprint": policies["items"][0]["fingerprint"]}),
    )
    .await?;
    let (preflight, waiting) = send(&app, &turns, "account lookup temporarily fails").await?;
    let preflight_approval = waiting["approvals"][0].clone();
    lock(&state).fail = 1;
    app.post(
        &format!(
            "{turns}/{preflight}/app-approvals/{}",
            id(&preflight_approval)?
        ),
        json!({"id": new_id(), "revision": preflight_approval["revision"], "decision": "allow"}),
    )
    .await?;
    let failed_ready = app.wait_status(&turns, &preflight, "failed").await?;
    ensure!(
        failed_ready["approvals"][0]["state"] == "ready",
        "approval state {}",
        failed_ready["approvals"][0]
    );
    ensure!(sends(&state) == 3, "failed preflight sent");
    lock(&state).fail = 0;
    app.post(
        &format!("{turns}/{preflight}/resume"),
        json!({"id": new_id(), "revision": failed_ready["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    app.wait_status(&turns, &preflight, "done").await?;
    let (denied, waiting) = send(&app, &turns, "deny").await?;
    let denial = &waiting["approvals"][0];
    app.post(
        &format!("{turns}/{denied}/app-approvals/{}", id(denial)?),
        json!({"id": new_id(), "revision": denial["revision"], "decision": "deny"}),
    )
    .await?;
    let declined = app.wait_status(&turns, &denied, "done").await?;
    ensure!(
        declined["reply"]
            .as_str()
            .is_some_and(|reply| reply.contains("did not allow")),
        "reply {}",
        declined["reply"]
    );
    ensure!(sends(&state) == 4, "sends {}", sends(&state));
    for (label, slug, tags, next_mode) in [
        (
            "unknown metadata",
            "GMAIL_GET_AND_DELETE",
            vec!["readOnlyHint"],
            "unknown",
        ),
        (
            "conflicting metadata",
            "GMAIL_FETCH_EMAILS",
            vec!["readOnlyHint", "destructiveHint"],
            "read",
        ),
    ] {
        *lock(&mode) = next_mode;
        lock(&state).tags = tags;
        let (turn, waiting) = send(&app, &turns, label).await?;
        ensure!(
            waiting["approvals"][0]["slug"] == slug,
            "{label} slug {}",
            waiting["approvals"][0]
        );
        app.post(
            &format!("{turns}/{turn}/app-approvals/{}", id(&waiting["approvals"][0])?),
            json!({"id": new_id(), "revision": waiting["approvals"][0]["revision"], "decision": "deny"}),
        )
        .await?;
        app.wait_status(&turns, &turn, "done").await?;
    }
    lock(&state).tags = vec!["readOnlyHint"];
    *lock(&mode) = "write";
    let (revoked, waiting) = send(&app, &turns, "revoke before approval").await?;
    let revoked_approval = &waiting["approvals"][0];
    let revoked_path = format!("{turns}/{revoked}/app-approvals/{}", id(revoked_approval)?);
    app.post(
        &access,
        json!({"id": new_id(), "revision": 1, "enabled": false}),
    )
    .await?;
    app.post_expect(
        &revoked_path,
        json!({"id": new_id(), "revision": revoked_approval["revision"], "decision": "allow"}),
        409,
    )
    .await?;
    app.post(
        &revoked_path,
        json!({"id": new_id(), "revision": revoked_approval["revision"], "decision": "deny"}),
    )
    .await?;
    app.wait_status(&turns, &revoked, "done").await?;
    app.post(&access, grant).await?;
    ensure!(
        app.get(&format!("/api/agents/{agent}/connected-apps"))
            .await?["items"][0]["enabled"]
            == false,
        "old grant receipt re-enabled access"
    );
    app.post(
        &access,
        json!({"id": new_id(), "revision": 2, "enabled": true}),
    )
    .await?;
    let disconnect = json!({"id": new_id()});
    app.post_expect(
        "/api/connected-apps/disconnect/foreign",
        json!({"id": new_id()}),
        404,
    )
    .await?;
    app.post("/api/connected-apps/disconnect/ca_1", disconnect.clone())
        .await?;
    app.post("/api/connected-apps/disconnect/ca_1", disconnect)
        .await?;
    ensure!(
        remote
            .requests()
            .iter()
            .filter(|request| request.method == "DELETE")
            .count()
            == 1,
        "account deleted twice"
    );
    ensure!(
        app.get(&format!("/api/agents/{agent}/connected-apps"))
            .await?["items"][0]["enabled"]
            == false,
        "grant survived the disconnect"
    );
    ensure!(
        app.get("/api/connected-apps/accounts").await?["items"] == json!([]),
        "accounts after disconnect"
    );
    for fail in 1..=4 {
        lock(&state).fail = fail;
        let error = app.get_expect("/api/connected-apps", 502).await?;
        ensure!(
            !error.to_string().contains(CANARY),
            "failure {fail} leaked the key"
        );
    }
    lock(&state).fail = 0;
    ensure!(
        !responses.join("").contains(CANARY),
        "responses leaked the key"
    );
    let (stdout, stderr) = app.output()?;
    ensure!(
        !stdout.contains(CANARY) && !stderr.contains(CANARY),
        "server output leaked the key"
    );
    ensure!(
        lock(&state).problems.is_empty(),
        "remote checks {:?}",
        lock(&state).problems
    );
    app.shutdown().await?;
    let bytes = std::fs::read(root.database())?;
    ensure!(
        !bytes
            .windows(CANARY.len())
            .any(|window| window == CANARY.as_bytes()),
        "database holds the key"
    );
    let installation: String = Inspector::open(root.database())?
        .scalar("SELECT user_id FROM composio_installation", [])?;
    ensure!(
        installation.starts_with("minarbot-") && !installation.contains('@'),
        "installation id {installation}"
    );
    Ok(())
}

#[tokio::test]
async fn installation_api_key_validation_encrypted_persistence_receipts_runtime_precedence_and_restart(
) -> Result<()> {
    const STORED: &str = "INSTALLATION_CANARY_KEY_K9m2s8v4";
    const REPLACEMENT: &str = "REPLACEMENT_CANARY_KEY_V7r8b9n2";
    const ENVIRONMENT: &str = "ENVIRONMENT_CANARY_KEY_T8n5z2p4";
    let remote = Stub::start(|request| {
        let key = request.header("x-api-key").unwrap_or_default().to_owned();
        if ![STORED, REPLACEMENT, ENVIRONMENT].contains(&key.as_str()) {
            return StubReply::Json(401, json!({"error": key}));
        }
        StubReply::ok(if request.path().ends_with("/toolkits/categories") {
            json!({"items": []})
        } else {
            json!({"items": [{"slug": "gmail", "name": "Gmail", "meta": {"description": key},
                "composio_managed_auth_schemes": ["OAUTH2"]}]})
        })
    })
    .await?;
    let key = Arc::new(Mutex::new(String::new()));
    let bridge = Bridge::composio_key_cell(key.clone()).await?;
    let root = TestRoot::new()?;
    let mut app = root
        .start(ServerConfig::new().composio(&format!("{}/api/v3.1", remote.origin()), &bridge))
        .await?;
    let mut responses = Vec::new();
    let keys = |remote: &Stub| -> Vec<String> {
        remote
            .requests()
            .iter()
            .map(|request| request.header("x-api-key").unwrap_or_default().to_owned())
            .collect()
    };
    let key_path = "/api/connected-apps/key";
    ensure!(
        app.get(key_path).await? == json!({"source": "none", "revision": 0}),
        "initial key"
    );
    app.post_expect(
        key_path,
        json!({"id": new_id(), "revision": 0, "key": "REJECTED_CANARY"}),
        400,
    )
    .await?;
    ensure!(
        app.get(key_path).await? == json!({"source": "none", "revision": 0}),
        "rejected key saved"
    );
    ensure!(
        remote.requests().len() == 1,
        "validation calls {}",
        remote.requests().len()
    );
    for bad in [
        String::new(),
        "x\ny".into(),
        "x\u{85}y".into(),
        "x".repeat(4097),
    ] {
        app.post_expect(
            key_path,
            json!({"id": new_id(), "revision": 0, "key": bad}),
            400,
        )
        .await?;
    }
    app.post_expect(
        key_path,
        json!({"id": new_id(), "revision": -1, "key": STORED}),
        400,
    )
    .await?;
    let body = json!({"id": new_id(), "revision": 0, "key": STORED});
    ensure!(
        raw(
            &app,
            key_path,
            &body,
            &[("Origin", "https://attacker.example")]
        )
        .await?
        .0 == 403,
        "cross origin"
    );
    ensure!(
        raw(&app, key_path, &body, &[("Content-Type", "text/plain")])
            .await?
            .0
            == 403,
        "text/plain"
    );
    ensure!(
        remote.requests().len() == 1,
        "rejected requests reached Composio"
    );
    *lock(&key) = ENVIRONMENT.into();
    ensure!(
        app.get(key_path).await? == json!({"source": "environment", "revision": 0}),
        "environment key"
    );
    responses.push(app.get("/api/connected-apps").await?.to_string());
    ensure!(
        keys(&remote).last().map(String::as_str) == Some(ENVIRONMENT),
        "environment key not used"
    );
    let save = json!({"id": new_id(), "revision": 0, "key": format!("  {STORED}  ")});
    let receipt = app.post(key_path, save.clone()).await?;
    ensure!(
        receipt == json!({"source": "settings", "revision": 1}),
        "receipt {receipt}"
    );
    let count = remote.requests().len();
    ensure!(
        app.post(key_path, save.clone()).await? == receipt,
        "save replay"
    );
    ensure!(
        remote.requests().len() == count,
        "Exact retry must not verify or save twice"
    );
    let mut conflicting = save.clone();
    conflicting["key"] = json!(REPLACEMENT);
    app.post_expect(key_path, conflicting, 409).await?;
    ensure!(remote.requests().len() == count, "conflict verified");
    app.post_expect(
        key_path,
        json!({"id": new_id(), "revision": 0, "key": REPLACEMENT}),
        409,
    )
    .await?;
    ensure!(remote.requests().len() == count, "stale save verified");
    ensure!(app.get(key_path).await? == receipt, "key state");
    let catalog = app.get("/api/connected-apps").await?;
    responses.push(catalog.to_string());
    ensure!(
        catalog["items"][0]["description"] == "[REDACTED]",
        "description {catalog}"
    );
    ensure!(
        keys(&remote)[count..].iter().all(|used| used == STORED),
        "stored key not preferred"
    );
    app.restart().await?;
    ensure!(app.get(key_path).await? == receipt, "key after restart");
    responses.push(app.get("/api/connected-apps").await?.to_string());
    ensure!(
        keys(&remote).last().map(String::as_str) == Some(STORED),
        "stored key after restart"
    );
    let replaced = app
        .post(
            key_path,
            json!({"id": new_id(), "revision": 1, "key": REPLACEMENT}),
        )
        .await?;
    ensure!(replaced["revision"] == 2, "replace {replaced}");
    responses.push(app.get("/api/connected-apps").await?.to_string());
    ensure!(
        keys(&remote).last().map(String::as_str) == Some(REPLACEMENT),
        "replacement not used"
    );
    let remove = json!({"id": new_id(), "revision": 2});
    let removed = app
        .post(&format!("{key_path}/remove"), remove.clone())
        .await?;
    ensure!(
        removed == json!({"source": "environment", "revision": 3}),
        "remove {removed}"
    );
    ensure!(
        app.post(&format!("{key_path}/remove"), remove.clone())
            .await?
            == removed,
        "remove replay"
    );
    let mut later = remove.clone();
    later["revision"] = json!(3);
    app.post_expect(&format!("{key_path}/remove"), later, 409)
        .await?;
    let mut reuse = remove.clone();
    reuse["key"] = json!(STORED);
    app.post_expect(key_path, reuse, 409).await?;
    responses.push(app.get("/api/connected-apps").await?.to_string());
    ensure!(
        keys(&remote).last().map(String::as_str) == Some(ENVIRONMENT),
        "environment after removal"
    );
    app.restart().await?;
    ensure!(app.get(key_path).await? == removed, "removal after restart");
    lock(&key).clear();
    ensure!(
        app.get(key_path).await? == json!({"source": "none", "revision": 3}),
        "unset environment"
    );
    app.post_expect(
        key_path,
        json!({"id": new_id(), "revision": 0, "key": STORED}),
        409,
    )
    .await?;
    app.post(
        key_path,
        json!({"id": new_id(), "revision": 3, "key": STORED}),
    )
    .await?;
    app.shutdown().await?;
    let bytes = std::fs::read(root.database())?;
    for secret in [STORED, REPLACEMENT, ENVIRONMENT, "REJECTED_CANARY"] {
        ensure!(
            !responses.join("").contains(secret),
            "responses leaked {secret}"
        );
        ensure!(
            !bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "database holds {secret}"
        );
    }
    let db = Inspector::open(root.database())?;
    ensure!(
        !json!(db.rows("SELECT * FROM composio_commands", [])?)
            .to_string()
            .contains(STORED),
        "command journal holds the key"
    );
    ensure!(
        !db.row("SELECT ciphertext FROM composio_settings", [])?
            .to_string()
            .contains(STORED),
        "ciphertext holds the key"
    );
    let key_file = root.database().with_extension("sqlite.composio-key");
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key_file)?.permissions().mode() & 0o777;
        ensure!(mode == 0o600, "key file mode {mode:o}");
    }
    Ok(())
}

#[tokio::test]
async fn rust_key_replacement_and_removal_reset_identity_grants_policies_and_pending_calls_atomically(
) -> Result<()> {
    let dispatches = Arc::new(AtomicUsize::new(0));
    let validation = Arc::new(AtomicUsize::new(0));
    let (sent, checked) = (dispatches.clone(), validation.clone());
    let remote = Stub::start(move |request| {
        let path = request.path();
        if path.ends_with("/tools/execute/GMAIL_SEND_EMAIL") {
            sent.fetch_add(1, Ordering::SeqCst);
        }
        if path.ends_with("/toolkits/categories") {
            checked.fetch_add(1, Ordering::SeqCst);
        }
        StubReply::ok(if path.ends_with("/connected_accounts") {
            json!({"items": [{"id": "ca_1", "toolkit": {"slug": "gmail"}, "status": "ACTIVE"}]})
        } else if path.ends_with("/tools") {
            json!({"items": [raw_tool("GMAIL_SEND_EMAIL", &["createHint"])]})
        } else {
            json!({"items": []})
        })
    })
    .await?;
    let result_text: Arc<Mutex<Vec<String>>> = Arc::default();
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let model = GatedModel::start().await?;
    let (seen, released) = (result_text.clone(), release.clone());
    model.respond(move |request| {
        let last = request.messages().last().cloned().unwrap_or(Value::Null);
        if last["role"] == "tool" {
            lock(&seen).push(last["content"].as_str().unwrap_or_default().to_owned());
            return Answer::text("Checked result");
        }
        // The held step answers with the offered call; a harness turn that
        // the reset restarted asks again and gets the same call.
        if released.load(Ordering::SeqCst) {
            return Answer::Tools(vec![minarbot_testkit::model::ToolCall {
                id: "call".into(),
                name: tool_name("GMAIL_SEND_EMAIL"),
                arguments: "{\"target\":\"fixture\"}".into(),
            }]);
        }
        Answer::Park
    });
    let key = Arc::new(Mutex::new(CANARY.to_owned()));
    let bridge = Bridge::composio_key_cell(key).await?;
    let root = TestRoot::new()?;
    let app = root
        .start(
            ServerConfig::new()
                .gateway(Gateway::new(model.base_url()))
                .composio(&format!("{}/api/v3.1", remote.origin()), &bridge),
        )
        .await?;
    let agent = new_id();
    let grant = format!("/api/agents/{agent}/connected-apps/gmail");
    let turns = format!("/api/agents/{agent}/turns");
    let call = json!({"id": "call", "type": "function", "function": {"name": tool_name("GMAIL_SEND_EMAIL"), "arguments": "{\"target\":\"fixture\"}"}});
    app.post_expect(
        "/api/agents",
        json!({"id": agent, "runtime": "api", "name": "Key reset", "model": "fixture", "instructions": "Help"}),
        201,
    )
    .await?;
    let sql = Fixture::open(root.database())?;
    let done = |turn: String| {
        let app = &app;
        let turns = turns.clone();
        async move {
            app.wait_row(&turns, &turn, |row| {
                row["status"] == "done" || row["status"] == "failed"
            })
            .await
        }
    };
    for remove in [false, true] {
        let previous = app.get("/api/connected-apps/key").await?["revision"]
            .as_i64()
            .unwrap_or(0);
        let grant_revision: i64 = sql
            .rows(
                "SELECT revision FROM composio_grants WHERE agent=? AND toolkit='gmail'",
                [&agent],
            )?
            .first()
            .and_then(|row| row["revision"].as_i64())
            .unwrap_or(0);
        app.post(
            &grant,
            json!({"id": new_id(), "revision": grant_revision, "enabled": true}),
        )
        .await?;
        release.store(false, Ordering::SeqCst);
        let stale = new_id();
        app.post_expect(
            &turns,
            json!({"id": stale, "text": "Hold an offered action through rotation"}),
            202,
        )
        .await?;
        let held: Parked = model.parked().await?;
        let identity =
            || -> Result<String> { sql.scalar("SELECT user_id FROM composio_installation", []) };
        let old_identity = identity()?;
        sql.execute(
            "INSERT INTO composio_auth_configs VALUES ('gmail','ac_old')",
            [],
        )?;
        sql.execute(
            "INSERT INTO composio_policies VALUES (?,'old-policy','old-tool','ca_1','gmail')",
            [&agent],
        )?;
        let mut pending = Vec::new();
        for state in ["approval", "ready", "running", "uncertain"] {
            let turn = new_id();
            pending.push((state, turn.clone()));
            sql.execute(
                "INSERT INTO turns(id,agent,text,status,created) VALUES (?,?,'Seed pending write','interrupted','2026-01-01')",
                [&turn, &agent],
            )?;
            sql.connection().execute(
                "INSERT INTO composio_calls VALUES (?,?,?,?,?,?,?,NULL,0,0,1)",
                (
                    &turn,
                    "call",
                    &agent,
                    call.to_string(),
                    "{}",
                    state,
                    if state == "approval" {
                        None
                    } else {
                        Some("allow")
                    },
                ),
            )?;
        }
        let path = if remove {
            "/api/connected-apps/key/remove"
        } else {
            "/api/connected-apps/key"
        };
        let mut command = json!({"id": new_id(), "revision": previous});
        if !remove {
            command["key"] = json!("installation-key-one");
        }
        sql.execute_batch("CREATE TRIGGER refuse_reset BEFORE DELETE ON composio_policies BEGIN SELECT RAISE(ABORT,'refuse reset fixture'); END;")?;
        app.post_expect(path, command.clone(), 500).await?;
        ensure!(
            app.get("/api/connected-apps/key").await?["revision"] == previous,
            "failed reset changed revision"
        );
        ensure!(identity()? == old_identity, "failed reset changed identity");
        ensure!(
            sql.scalar::<i64>(
                "SELECT enabled FROM composio_grants WHERE agent=? AND toolkit='gmail'",
                [&agent]
            )? == 1,
            "failed reset disabled the grant"
        );
        sql.execute_batch("DROP TRIGGER refuse_reset")?;
        let changed = app.post(path, command).await?;
        ensure!(
            changed["revision"] == previous + 1,
            "reset revision {changed}"
        );
        ensure!(identity()? != old_identity, "identity kept");
        ensure!(
            sql.scalar::<i64>("SELECT count(*) FROM composio_auth_configs", [])? == 0,
            "auth configs kept"
        );
        ensure!(
            sql.scalar::<i64>("SELECT count(*) FROM composio_policies", [])? == 0,
            "policies kept"
        );
        let disabled = sql.row(
            "SELECT enabled,revision FROM composio_grants WHERE agent=? AND toolkit='gmail'",
            [&agent],
        )?;
        ensure!(
            disabled == json!({"enabled": 0, "revision": grant_revision + 2}),
            "grant {disabled}"
        );
        app.post(
            &grant,
            json!({"id": new_id(), "revision": disabled["revision"], "enabled": true}),
        )
        .await?;
        release.store(true, Ordering::SeqCst);
        held.answer(Answer::Tools(vec![minarbot_testkit::model::ToolCall {
            id: "call".into(),
            name: tool_name("GMAIL_SEND_EMAIL"),
            arguments: "{\"target\":\"fixture\"}".into(),
        }]));
        // #87: the Node API runtime kept the tool list it offered when the turn
        // began, so the held call was refused after the reset. A harness call
        // resolves the tools again over MCP; after the reset and the new grant
        // the write is offered for the new installation and waits for
        // approval. Deny it and prove nothing was sent.
        let card = app
            .wait_row(&turns, &stale, |row| {
                row["approvals"]
                    .as_array()
                    .is_some_and(|cards| cards.iter().any(|card| card["state"] == "approval"))
            })
            .await?["approvals"][0]
            .clone();
        app.post(
            &format!("{turns}/{stale}/app-approvals/{}", id(&card)?),
            json!({"id": new_id(), "revision": card["revision"], "decision": "deny"}),
        )
        .await?;
        let denied = done(stale).await?;
        ensure!(
            denied["status"] == "done",
            "stale turn {}",
            denied["status"]
        );
        ensure!(
            dispatches.load(Ordering::SeqCst) == 0,
            "held call dispatched after the reset"
        );
        for (prior, turn) in pending {
            let stored = sql.row(
                "SELECT state,decision FROM composio_calls WHERE turn=?",
                [&turn],
            )?;
            let expected = if ["approval", "ready"].contains(&prior) {
                "ready"
            } else {
                "uncertain"
            };
            ensure!(stored["state"] == expected, "{prior}: {stored}");
            if stored["state"] == "ready" {
                ensure!(stored["decision"] == "revoked", "{prior} decision {stored}");
            }
            let resume = json!({"id": new_id(), "revision": 1, "acknowledgeUncertain": true});
            app.post(&format!("{turns}/{turn}/resume"), resume.clone())
                .await?;
            let row = done(turn.clone()).await?;
            if stored["state"] == "uncertain" {
                ensure!(
                    row["error"]
                        .as_str()
                        .is_some_and(|error| error.contains("uncertain")),
                    "{prior}: {}",
                    row["error"]
                );
            } else {
                ensure!(row["status"] == "done", "{prior}: {}", row["error"]);
                ensure!(
                    lock(&result_text)
                        .last()
                        .is_some_and(|text| text.contains("access was removed")),
                    "{prior} result {:?}",
                    lock(&result_text).last()
                );
                app.post(&format!("{turns}/{turn}/resume"), resume).await?;
                ensure!(
                    sql.scalar::<i64>(
                        "SELECT count(*) FROM flow_tool_calls WHERE turn=?",
                        [&turn]
                    )? == 1,
                    "{prior} journal rows"
                );
            }
        }
        ensure!(
            dispatches.load(Ordering::SeqCst) == 0,
            "revoked write dispatched"
        );
    }
    ensure!(validation.load(Ordering::SeqCst) >= 2, "keys not validated");
    Ok(())
}

#[derive(Default)]
struct Fenced {
    mode: &'static str,
    disconnected: bool,
    effects: usize,
    problems: Vec<String>,
}

const FIXTURE_KEY: &str = "FIXTURE_COMPOSIO_KEY_93";

#[tokio::test]
async fn rust_composio_approval_recovery_revocation_schema_checks_and_uncertain_writes_remain_fenced(
) -> Result<()> {
    let state = Arc::new(Mutex::new(Fenced {
        mode: "read",
        ..Fenced::default()
    }));
    let shared = state.clone();
    let remote = Stub::start(move |request| {
        let mut remote = lock(&shared);
        if request.header("x-api-key") != Some(FIXTURE_KEY) {
            remote.problems.push(format!("key on {}", request.target));
        }
        let path = request.path();
        if path.ends_with("/toolkits") || path.ends_with("/toolkits/categories") {
            return StubReply::ok(json!({"items": []}));
        }
        if path.ends_with("/connected_accounts") {
            return StubReply::ok(json!({"items": [{"id": "ca_fixture", "toolkit": {"slug": "gmail"}, "status": "ACTIVE"}]}));
        }
        if path.ends_with("/tools") {
            let items: Vec<Value> = ["READ", "WRITE"]
                .iter()
                .map(|kind| json!({"slug": format!("GMAIL_{kind}"), "name": kind, "description": FIXTURE_KEY,
                    "toolkit": {"slug": "gmail"}, "version": "fixture",
                    "tags": [if *kind == "READ" { "readOnlyHint" } else { "createHint" }],
                    "input_parameters": {"type": "object", "properties": {"target": {"type": "string"}},
                        "required": ["target"], "additionalProperties": false}}))
                .collect();
            return StubReply::ok(json!({"items": items}));
        }
        remote.effects += 1;
        match remote.mode {
            "crash" => StubReply::Hang,
            "uncertain" => {
                remote.disconnected = true;
                StubReply::Drop
            }
            _ => StubReply::ok(json!({"successful": true, "data": {"result": FIXTURE_KEY}})),
        }
    })
    .await?;
    let mode = state.clone();
    let model = GatedModel::start().await?;
    let leaks = Arc::new(AtomicUsize::new(0));
    let leaked = leaks.clone();
    model.respond(move |request| {
        if request.body.to_string().contains(FIXTURE_KEY) {
            leaked.fetch_add(1, Ordering::SeqCst);
        }
        if let Some(result) = request.tool_results().last() {
            return Answer::text(result["content"].as_str().unwrap_or_default());
        }
        let current = lock(&mode).mode;
        let args = if current == "invalid" {
            json!({"unexpected": true})
        } else {
            json!({"target": "fixture@example.test"})
        };
        Answer::tool_id(
            "fixture-call",
            &tool_name(if current == "read" {
                "GMAIL_READ"
            } else {
                "GMAIL_WRITE"
            }),
            args,
        )
    });
    let bridge = Bridge::composio_key_cell(Arc::new(Mutex::new(String::new()))).await?;
    let root = TestRoot::new()?;
    let config = ServerConfig::new()
        .gateway(Gateway::new(model.base_url()))
        .composio(&format!("{}/api/v3.1", remote.origin()), &bridge);
    let mut app = root.start(config.clone()).await?;
    let mut responses = Vec::new();
    let agent = new_id();
    let turns = format!("/api/agents/{agent}/turns");
    let grant = format!("/api/agents/{agent}/connected-apps/gmail");
    app.post_expect(
        "/api/agents",
        json!({"id": agent, "runtime": "api", "name": "Fixture", "model": "fixture", "instructions": "Help"}),
        201,
    )
    .await?;
    app.post(
        "/api/connected-apps/key",
        json!({"id": new_id(), "revision": 0, "key": FIXTURE_KEY}),
    )
    .await?;
    app.post(
        &grant,
        json!({"id": new_id(), "revision": 0, "enabled": true}),
    )
    .await?;
    async fn run(app: &Server, turns: &str, text: &str) -> Result<String> {
        let turn = new_id();
        app.post_expect(turns, json!({"id": turn, "text": text}), 202)
            .await?;
        Ok(turn)
    }
    async fn approval(app: &Server, turns: &str, turn: &str, state: &str) -> Result<Value> {
        let row = app
            .wait_row(turns, turn, |row| {
                row["approvals"]
                    .as_array()
                    .is_some_and(|cards| cards.iter().any(|card| card["state"] == state))
            })
            .await?;
        Ok(row["approvals"][0].clone())
    }
    let effects = |state: &Arc<Mutex<Fenced>>| lock(state).effects;
    let read = run(&app, &turns, "read").await?;
    app.wait_status(&turns, &read, "done").await?;
    ensure!(effects(&state) == 1, "read effects {}", effects(&state));
    lock(&state).mode = "write";
    let write = run(&app, &turns, "write").await?;
    let card = approval(&app, &turns, &write, "approval").await?;
    app.restart().await?;
    let path = format!("{turns}/{write}/app-approvals/{}", id(&card)?);
    let decision = json!({"id": new_id(), "revision": card["revision"], "decision": "allow"});
    let mut stale = decision.clone();
    stale["revision"] = json!(999);
    app.post_expect(&path, stale, 409).await?;
    responses.push(app.post(&path, decision.clone()).await?.to_string());
    responses.push(app.post(&path, decision).await?.to_string());
    app.wait_status(&turns, &write, "done").await?;
    ensure!(effects(&state) == 2, "write effects {}", effects(&state));
    let revoke = run(&app, &turns, "write").await?;
    let pending = approval(&app, &turns, &revoke, "approval").await?;
    app.post(
        &grant,
        json!({"id": new_id(), "revision": 1, "enabled": false}),
    )
    .await?;
    let pending_path = format!("{turns}/{revoke}/app-approvals/{}", id(&pending)?);
    app.post_expect(
        &pending_path,
        json!({"id": new_id(), "revision": pending["revision"], "decision": "allow"}),
        409,
    )
    .await?;
    app.post(
        &pending_path,
        json!({"id": new_id(), "revision": pending["revision"], "decision": "deny"}),
    )
    .await?;
    app.wait_status(&turns, &revoke, "done").await?;
    ensure!(effects(&state) == 2, "revoked write ran");
    app.post(
        &grant,
        json!({"id": new_id(), "revision": 2, "enabled": true}),
    )
    .await?;
    lock(&state).mode = "invalid";
    let invalid = run(&app, &turns, "invalid").await?;
    app.wait_row(&turns, &invalid, |row| {
        row["status"] == "done" || row["status"] == "failed"
    })
    .await?;
    ensure!(effects(&state) == 2, "invalid arguments reached Composio");
    lock(&state).mode = "uncertain";
    let uncertain = run(&app, &turns, "uncertain").await?;
    let last = approval(&app, &turns, &uncertain, "approval").await?;
    app.post(
        &format!("{turns}/{uncertain}/app-approvals/{}", id(&last)?),
        json!({"id": new_id(), "revision": last["revision"], "decision": "allow"}),
    )
    .await?;
    approval(&app, &turns, &uncertain, "uncertain").await?;
    ensure!(lock(&state).disconnected, "write never reached Composio");
    ensure!(
        effects(&state) == 3,
        "uncertain effects {}",
        effects(&state)
    );
    app.restart().await?;
    ensure!(effects(&state) == 3, "restart replayed the uncertain write");
    lock(&state).mode = "crash";
    let killed = run(&app, &turns, "crash").await?;
    let before = approval(&app, &turns, &killed, "approval").await?;
    app.post(
        &format!("{turns}/{killed}/app-approvals/{}", id(&before)?),
        json!({"id": new_id(), "revision": before["revision"], "decision": "allow"}),
    )
    .await?;
    until("Composio accepts the pending write", || async {
        Ok((effects(&state) == 4).then_some(()))
    })
    .await?;
    ensure!(
        Inspector::open(root.database())?
            .scalar::<String>("SELECT state FROM composio_calls WHERE turn=?", [&killed])?
            == "running",
        "dispatched call not running"
    );
    app.crash().await?;
    app.start_stopped(config).await?;
    let crash = app.wait_status(&turns, &killed, "interrupted").await?;
    ensure!(
        crash["approvals"][0]["state"] == "uncertain",
        "crash approval {}",
        crash["approvals"][0]
    );
    app.post(
        &format!("{turns}/{killed}/resume"),
        json!({"id": new_id(), "revision": crash["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    let failed = app.wait_status(&turns, &killed, "failed").await?;
    ensure!(
        failed["error"]
            .as_str()
            .is_some_and(|error| error.contains("uncertain")),
        "error {}",
        failed["error"]
    );
    ensure!(
        effects(&state) == 4,
        "Acknowledging uncertainty must never send the write again."
    );
    ensure!(
        leaks.load(Ordering::SeqCst) == 0,
        "the key reached the model"
    );
    ensure!(
        !responses.join("").contains(FIXTURE_KEY),
        "responses leaked the key"
    );
    ensure!(
        lock(&state).problems.is_empty(),
        "remote checks {:?}",
        lock(&state).problems
    );
    app.shutdown().await?;
    let bytes = std::fs::read(root.database())?;
    ensure!(
        !bytes
            .windows(FIXTURE_KEY.len())
            .any(|window| window == FIXTURE_KEY.as_bytes()),
        "database holds the key"
    );
    let _ = Method::GET;
    Ok(())
}
