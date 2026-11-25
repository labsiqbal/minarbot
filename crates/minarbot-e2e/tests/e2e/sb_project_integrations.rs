//! Project environment, project apps and project MCP (port of
//! `app/project-integrations.test.ts`).

use crate::sb_support::{after_tool, id, reminder, settle};
use minarbot_testkit::{
    control::{Answer, ControlModel},
    db::Inspector,
    fakes::{self, Fake},
    http::Method,
    prelude::*,
    protocol::sha256_hex,
    stub::{HttpStub, StubRequest, StubResponse},
    Server,
};
use std::{
    collections::HashMap,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
};

const SECRET: &str = "PROJECT_CANARY_j4W8sK3p";

struct Fixture {
    root: TestRoot,
    app: Server,
    responses: Arc<Mutex<Vec<String>>>,
}

impl Fixture {
    async fn start(root: TestRoot, config: ServerConfig) -> Result<Self> {
        // Project folders must stay inside the owner's home (the kit's HOME).
        let projects = root.dir("home/projects")?;
        let app = root
            .start(config.option("projectRoot", projects.display().to_string()))
            .await?;
        Ok(Self {
            root,
            app,
            responses: Arc::new(Mutex::new(Vec::new())),
        })
    }

    async fn request(&self, path: &str, body: Option<Value>, status: u16) -> Result<Value> {
        let method = if body.is_some() {
            Method::POST
        } else {
            Method::GET
        };
        let reply = self.app.call(method, path, body).await?;
        self.responses
            .lock()
            .map_err(|_| "responses")?
            .push(reply.body.to_string());
        reply.expect(status)
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.request(path, None, 200).await
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.request(path, Some(body), 200).await
    }

    async fn agent(&self, name: &str, id: &str) -> Result<Value> {
        self.request(
            "/api/agents",
            Some(json!({"runtime": "api", "id": id, "name": name, "model": "fake", "instructions": "Help."})),
            201,
        )
        .await
    }

    async fn project(&self, agent: &str, name: &str, folder: bool) -> Result<Value> {
        let mut body =
            json!({"id": new_id(), "name": name, "members": [{"agent": agent, "write": false}]});
        if folder {
            body["folders"] = json!([{"mode": "new"}]);
        }
        self.post("/api/projects", body).await
    }

    async fn run(&self, path: &str, text: &str, recipient: Option<&str>) -> Result<Value> {
        let id = new_id();
        let mut body = json!({"id": id, "text": text});
        if let Some(recipient) = recipient {
            body["recipient"] = json!(recipient);
        }
        self.request(path, Some(body), 202).await?;
        let row = settle(&self.app, path, &id).await?;
        ensure!(row["status"] == "done", "{text}: {}", row["error"]);
        Ok(row)
    }

    fn leaked(&self) -> Result<bool> {
        Ok(self
            .responses
            .lock()
            .map_err(|_| "responses")?
            .join("")
            .contains(SECRET))
    }
}

#[tokio::test]
async fn rust_project_environment_validates_names_and_utf_8_byte_bounds() -> Result<()> {
    let f = Fixture::start(TestRoot::new()?, ServerConfig::new()).await?;
    let agent = id(&f.agent("Member", &new_id()).await?)?;
    let project = id(&f.project(&agent, "Environment validation", false).await?)?;
    let path = format!("/api/projects/{project}/environment");
    let long = "A".repeat(65);
    for name in [
        "PATH",
        "HOME",
        "LD_PRELOAD",
        "NODE_OPTIONS",
        "MINARBOT_TOKEN",
        "BASH_ENV",
        "bad",
        "A-B",
        long.as_str(),
    ] {
        f.request(
            &path,
            Some(json!({"id": new_id(), "name": name, "value": "x"})),
            400,
        )
        .await?;
    }
    for value in ["é".repeat(8193), "a\u{0}b".to_owned()] {
        f.request(
            &path,
            Some(json!({"id": new_id(), "name": "TOKEN", "value": value})),
            400,
        )
        .await?;
    }
    let result = f
        .post(
            &path,
            json!({"id": new_id(), "name": "_TOKEN_1", "value": ""}),
        )
        .await?;
    ensure!(result["name"] == "_TOKEN_1", "saved: {result}");
    Ok(())
}

#[tokio::test]
async fn project_environment_settings_and_removal_preserve_consent_replay_and_stale_write_fences(
) -> Result<()> {
    let f = Fixture::start(TestRoot::new()?, ServerConfig::new()).await?;
    let agent = id(&f.agent("Member", &new_id()).await?)?;
    let project = id(&f.project(&agent, "Environment", false).await?)?;
    let base = format!("/api/projects/{project}/environment");
    let setting = json!({"id": new_id(), "revision": 1, "exposeComputer": true, "confirmed": true});
    let mut unconfirmed = setting.clone();
    unconfirmed["confirmed"] = json!(false);
    f.request(&format!("{base}/settings"), Some(unconfirmed), 400)
        .await?;
    let expected = json!({"exposeComputer": true, "revision": 2});
    ensure!(
        f.post(&format!("{base}/settings"), setting.clone()).await? == expected,
        "settings"
    );
    ensure!(
        f.post(&format!("{base}/settings"), setting.clone()).await? == expected,
        "settings replay"
    );
    let mut other = setting.clone();
    other["id"] = json!(new_id());
    f.request(&format!("{base}/settings"), Some(other), 409)
        .await?;
    let mut changed = setting.clone();
    changed["exposeComputer"] = json!(false);
    f.request(&format!("{base}/settings"), Some(changed), 409)
        .await?;
    let saved = f
        .post(
            &base,
            json!({"id": new_id(), "name": "TOKEN", "value": "fixture-value"}),
        )
        .await?;
    let remove = json!({"id": new_id(), "name": saved["name"], "updated": saved["updated"], "confirmed": true});
    let mut unconfirmed = remove.clone();
    unconfirmed["confirmed"] = json!(false);
    f.request(&format!("{base}/remove"), Some(unconfirmed), 400)
        .await?;
    let mut stale = remove.clone();
    stale["updated"] = json!(saved["updated"].as_i64().unwrap_or(0) + 1);
    f.request(&format!("{base}/remove"), Some(stale), 409)
        .await?;
    ensure!(
        f.post(&format!("{base}/remove"), remove.clone()).await? == json!({"deleted": true}),
        "remove"
    );
    ensure!(
        f.post(&format!("{base}/remove"), remove).await? == json!({"deleted": true}),
        "remove replay"
    );
    ensure!(
        f.get(&base).await?["items"] == json!([]),
        "items after remove"
    );
    for operation in ["settings", "remove"] {
        f.request(
            &format!("/api/environment/{operation}"),
            Some(json!({"id": new_id()})),
            404,
        )
        .await?;
    }
    Ok(())
}

async fn environment_isolation(runtime: &str) -> Result<()> {
    let root = TestRoot::new()?;
    let bin = root.dir("bin")?;
    let binary = fakes::install(Fake::native(runtime)?, &bin, "fake-harness")?;
    let direct = root.dir("direct")?;
    let agent_id = new_id();
    let config = ServerConfig::new()
        .workspace(&agent_id, &direct, false)
        .executable(runtime, &binary);
    let mut f = Fixture::start(root, config).await?;
    let a = f.agent("Member", &agent_id).await?;
    let p = f.project(&agent_id, "Environment A", true).await?;
    let b = f.project(&agent_id, "Environment B", true).await?;
    let base = format!("/api/projects/{}/environment", id(&p)?);
    for name in [
        "PATH",
        "HOME",
        "LD_LIBRARY_PATH",
        "NODE_OPTIONS",
        "MINARBOT_TOKEN",
    ] {
        f.request(
            &base,
            Some(json!({"id": new_id(), "name": name, "value": SECRET})),
            400,
        )
        .await?;
    }
    f.request(
        &base,
        Some(json!({"id": new_id(), "name": "TOKEN", "value": "x".repeat(16385)})),
        400,
    )
    .await?;
    let command =
        json!({"id": new_id(), "name": "PROJECT_TEST_TOKEN", "value": SECRET, "updated": null});
    let saved = f.post(&base, command.clone()).await?;
    ensure!(
        f.post(&base, command).await? == saved,
        "environment receipt"
    );
    let key = std::path::PathBuf::from(format!("{}.project-env-key", f.root.database().display()));
    ensure!(
        std::fs::metadata(&key)?.permissions().mode() & 0o777 == 0o600,
        "key file mode"
    );
    ensure!(
        !f.get(&base).await?.to_string().contains(SECRET),
        "environment list leaked the value"
    );
    f.request(&base, Some(json!({"id": new_id(), "name": "PROJECT_TEST_TOKEN", "value": "other", "updated": null})), 409)
        .await?;
    f.post(
        &format!("/api/agents/{agent_id}/edit"),
        json!({"id": new_id(), "revision": a["revision"], "name": "Member", "model": "fake", "instructions": "Help.",
            "runtime": runtime, "confirmRuntime": true}),
    )
    .await?;
    let thread = |project: &Value, name: &str| json!({"id": new_id(), "revision": project["revision"], "agent": agent_id, "folder": project["folders"][0], "name": name});
    let side = f
        .post(
            &format!("/api/projects/{}/thread", id(&p)?),
            thread(&p, "Project shell"),
        )
        .await?;
    let path = format!(
        "/api/threads/{}/turns",
        side["threads"]
            .as_array()
            .and_then(|rows| rows.last())
            .ok_or("thread")?["id"]
            .as_str()
            .ok_or("thread id")?
    );
    let turn = f.run(&path, "ENV_PRESENT", None).await?;
    ensure!(
        turn["activity"]["stream"]
            .to_string()
            .contains("[REDACTED]"),
        "stream: {}",
        turn["activity"]["stream"]
    );
    // A harness turn's reply joins its agent messages; the shell output part
    // is redacted.
    let finished = |row: &Value| {
        row["reply"]
            .as_str()
            .is_some_and(|reply| reply.ends_with("finished") && !reply.contains(SECRET))
    };
    ensure!(finished(&turn), "reply: {}", turn["reply"]);
    ensure!(!turn.to_string().contains(SECRET), "turn leaked the value");
    let other = f
        .post(
            &format!("/api/projects/{}/thread", id(&b)?),
            thread(&b, "Other shell"),
        )
        .await?;
    let other_path = format!(
        "/api/threads/{}/turns",
        other["threads"]
            .as_array()
            .and_then(|rows| rows.last())
            .ok_or("thread")?["id"]
            .as_str()
            .ok_or("thread id")?
    );
    f.run(&other_path, "ENV_ABSENT", None).await?;
    f.run(&format!("/api/agents/{agent_id}/turns"), "ENV_ABSENT", None)
        .await?;
    let folder = p["folders"][0].as_str().ok_or("folder")?;
    let log = std::fs::read_to_string(std::path::Path::new(folder).join("fake-log.jsonl"))?;
    ensure!(!log.contains(SECRET), "harness log leaked the value");
    let routine = f
        .post(
            &format!("/api/agents/{agent_id}/routines"),
            json!({"id": new_id(), "name": "Project routine", "prompt": "ENV_PRESENT", "trigger": "manual",
                "nextAt": minarbot_testkit::FIXED_CLOCK + 60_000, "enabled": false, "project": id(&p)?}),
        )
        .await?;
    let run = f
        .post(
            &format!("/api/routines/{}/test", id(&routine)?),
            json!({"id": new_id(), "revision": routine["revision"]}),
        )
        .await?;
    let done = settle(
        &f.app,
        &format!(
            "/api/threads/{}/turns",
            run["thread"].as_str().ok_or("thread")?
        ),
        run["turn"].as_str().ok_or("turn")?,
    )
    .await?;
    ensure!(done["status"] == "done", "routine: {}", done["error"]);
    ensure!(
        done["activity"]["stream"]
            .to_string()
            .contains("[REDACTED]"),
        "routine stream"
    );
    ensure!(finished(&done), "routine reply: {}", done["reply"]);
    f.app.restart().await?;
    f.run(&path, "ENV_PRESENT", None).await?;
    let listed = f.get(&base).await?;
    ensure!(
        listed["items"].as_array().map(Vec::len) == Some(1),
        "items: {listed}"
    );
    ensure!(listed["exposeComputer"] == false, "expose: {listed}");
    f.request(
        &format!("{base}/settings"),
        Some(json!({"id": new_id(), "revision": 1, "exposeComputer": true})),
        400,
    )
    .await?;
    f.post(
        &format!("{base}/settings"),
        json!({"id": new_id(), "revision": 1, "exposeComputer": true, "confirmed": true}),
    )
    .await?;
    let replacement = f
        .post(&base, json!({"id": new_id(), "name": saved["name"], "value": "replacement-value", "updated": saved["updated"]}))
        .await?;
    f.post(
        &format!("{base}/remove"),
        json!({"id": new_id(), "name": saved["name"], "updated": replacement["updated"], "confirmed": true}),
    )
    .await?;
    ensure!(
        f.get(&base).await?["items"] == json!([]),
        "items after remove"
    );
    f.run(&path, "ENV_ABSENT", None).await?;
    ensure!(!f.leaked()?, "an API response leaked the value");
    f.app.shutdown().await?;
    let bytes = std::fs::read(f.root.database())?;
    ensure!(
        !bytes
            .windows(SECRET.len())
            .any(|window| window == SECRET.as_bytes()),
        "database stores the value"
    );
    let db = Inspector::open(f.root.database())?;
    ensure!(
        !db.rows("SELECT * FROM provider_streams", [])?
            .iter()
            .any(|row| row.to_string().contains(SECRET)),
        "streams"
    );
    ensure!(
        !db.rows("SELECT * FROM provider_stream_segments", [])?
            .iter()
            .any(|row| row.to_string().contains(SECRET)),
        "segments"
    );
    Ok(())
}

#[tokio::test]
async fn codex_project_environment_encryption_harness_shell_routine_and_direct_chat_isolation(
) -> Result<()> {
    environment_isolation("codex").await
}

#[tokio::test]
async fn claude_project_environment_encryption_harness_shell_routine_and_direct_chat_isolation(
) -> Result<()> {
    environment_isolation("claude").await
}

fn composio_name(slug: &str) -> String {
    format!("composio__{}", &sha256_hex(json!(slug).to_string())[..24])
}

#[derive(Default)]
struct Remote {
    accounts: HashMap<String, String>,
    links: Vec<String>,
    executed: Vec<String>,
    write: bool,
    binary: bool,
    reflect: bool,
    violations: Vec<String>,
}

fn composio(state: &Arc<Mutex<Remote>>, request: &StubRequest) -> StubResponse {
    let Ok(mut state) = state.lock() else {
        return StubResponse::status(500);
    };
    let body = request.json();
    let path = request.path().to_owned();
    let result = if path == "/toolkits/categories" {
        json!({"items": []})
    } else if path == "/toolkits/gmail" {
        json!({"composio_managed_auth_schemes": ["OAUTH2"]})
    } else if path == "/auth_configs" {
        json!({"auth_config": {"id": "auth-gmail"}})
    } else if path == "/connected_accounts/link" {
        let user = body["user_id"].as_str().unwrap_or_default().to_owned();
        state.links.push(user.clone());
        let account = format!("account_{}", state.links.len());
        state.accounts.insert(user, account.clone());
        json!({"connected_account_id": account, "redirect_url": "https://connect.composio.dev/test"})
    } else if path == "/connected_accounts" && request.method == "GET" {
        let identity = request.query("user_ids").unwrap_or_default();
        match state.accounts.get(&identity) {
            Some(account) => {
                json!({"items": [{"id": account, "user_id": identity, "status": "ACTIVE", "toolkit": {"slug": "gmail"}}]})
            }
            None => json!({"items": []}),
        }
    } else if path.starts_with("/connected_accounts/") && request.method == "DELETE" {
        let id = path.rsplit('/').next().unwrap_or_default().to_owned();
        state.accounts.retain(|_, account| *account != id);
        json!({})
    } else if path == "/tools" {
        let hint = if state.write {
            "createHint"
        } else {
            "readOnlyHint"
        };
        json!({"items": [{"slug": "GMAIL_ACTION", "name": "Gmail action", "toolkit": {"slug": "gmail"}, "version": "20261115",
            "input_parameters": {"type": "object", "properties": {}, "additionalProperties": false}, "tags": [hint]}]})
    } else if path.starts_with("/tools/execute/") {
        let user = body["user_id"].as_str().unwrap_or_default().to_owned();
        if state.accounts.get(&user).map(String::as_str) != body["connected_account_id"].as_str() {
            state
                .violations
                .push(format!("execute with a foreign account: {body}"));
        }
        state.executed.push(user);
        let data = if state.reflect {
            json!(SECRET.repeat(300))
        } else {
            json!("App result")
        };
        json!({"successful": true, "data": data})
    } else {
        json!({})
    };
    let _ = state.binary;
    StubResponse::json(&result)
}

fn mcp(state: &Arc<Mutex<Remote>>, request: &StubRequest) -> StubResponse {
    if request.method == "GET" {
        return StubResponse::status(405);
    }
    if request.method == "DELETE" {
        return StubResponse::status(200);
    }
    let body = request.json();
    if body.get("id").is_none() {
        return StubResponse::status(202);
    }
    let binary = state.lock().map(|state| state.binary).unwrap_or(false);
    let result = match body["method"].as_str().unwrap_or_default() {
        "initialize" => json!({"protocolVersion": "2025-03-26", "capabilities": {"tools": {}},
            "serverInfo": {"name": "project-fixture", "version": "1"}}),
        "tools/list" => {
            json!({"tools": [{"name": "echo", "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}}]})
        }
        "tools/call" if binary => {
            use minarbot_testkit::base64::Engine;
            json!({"content": [{"type": "resource", "resource": {"uri": "file:///secret.txt", "mimeType": "text/plain",
                "blob": minarbot_testkit::base64::engine::general_purpose::STANDARD.encode(SECRET)}}]})
        }
        "tools/call" => json!({"content": [{"type": "text", "text": SECRET}]}),
        _ => json!({}),
    };
    StubResponse::json(&json!({"jsonrpc": "2.0", "id": body["id"], "result": result}))
}

#[derive(Default)]
struct Script {
    mode: String,
    connection: String,
}

#[tokio::test]
async fn project_apps_and_mcp_separate_oauth_identities_offered_tools_approvals_restart_and_cleanup(
) -> Result<()> {
    let state = Arc::new(Mutex::new(Remote::default()));
    let remote_state = state.clone();
    let remote = HttpStub::start(move |request| composio(&remote_state, request)).await?;
    let mcp_state = state.clone();
    let mcp_server = HttpStub::start(move |request| mcp(&mcp_state, request)).await?;
    let script = Arc::new(Mutex::new(Script {
        mode: "apps".into(),
        ..Script::default()
    }));
    let model = ControlModel::start().await?;
    model.catalog(["fake"]);
    let scripted = script.clone();
    let app_tool = composio_name("GMAIL_ACTION");
    let tool_name = app_tool.clone();
    model.respond(move |request| {
        if after_tool(request) {
            let content = request
                .tool_results()
                .last()
                .and_then(|message| message["content"].as_str().map(str::to_owned))
                .unwrap_or_default();
            return Answer::text(content);
        }
        if reminder(request) {
            return Answer::text("Checked.");
        }
        let Ok(script) = scripted.lock() else {
            return Answer::Status(500, json!({"error": "script"}));
        };
        match script.mode.as_str() {
            "apps" if request.tool_names().iter().any(|name| *name == tool_name) => {
                Answer::tool(&tool_name, json!({}))
            }
            "mcp" => Answer::tool(
                "call_mcp_tool",
                json!({"connection": script.connection, "name": "echo", "arguments": {}}),
            ),
            "list" => Answer::tool("list_mcp_tools", json!({})),
            _ => Answer::text("No tools"),
        }
    });
    let config = ServerConfig::new()
        .gateway(model.gateway())
        .option("composio", json!({"baseURL": remote.origin()}));
    let mut f = Fixture::start(TestRoot::new()?, config).await?;
    let catalog = f
        .post(
            "/api/provider-instances/codex-test-gateway/models",
            json!({"id": new_id(), "instanceId": "codex-test-gateway"}),
        )
        .await?;
    ensure!(catalog["models"] == json!(["fake"]), "catalog: {catalog}");
    f.post(
        "/api/connected-apps/key",
        json!({"id": new_id(), "revision": 0, "key": "FAKE_COMPOSIO_KEY"}),
    )
    .await?;
    let a = id(&f.agent("Member", &new_id()).await?)?;
    let foreign = id(&f.agent("Nonmember", &new_id()).await?)?;
    // Since #87 every turn runs in a harness, which needs a workspace, so
    // these projects get a folder (the Node API runtime did not need one).
    let p = f.project(&a, "Apps A", true).await?;
    let b = f.project(&a, "Apps B", true).await?;
    let (p_id, b_id) = (id(&p)?, id(&b)?);
    let direct = format!("/api/agents/{a}/turns");
    let side = |project: &str| {
        let f = &f;
        let a = a.clone();
        let project = project.to_owned();
        async move {
            let current = f.get(&format!("/api/projects/{project}")).await?;
            let created = f
                .post(
                    &format!("/api/projects/{project}/thread"),
                    json!({"id": new_id(), "revision": current["revision"], "agent": a, "name": "Apps thread"}),
                )
                .await?;
            let thread = created["threads"]
                .as_array()
                .and_then(|rows| rows.last())
                .ok_or("thread")?["id"]
                .clone();
            Ok::<_, minarbot_testkit::Error>(format!(
                "/api/threads/{}/turns",
                thread.as_str().unwrap_or_default()
            ))
        }
    };
    let xp = side(&p_id).await?;
    let yp = side(&b_id).await?;
    let scopes = [
        String::new(),
        format!("/projects/{p_id}"),
        format!("/projects/{b_id}"),
    ];
    for scope in &scopes {
        f.post(
            &format!("/api{scope}/connected-apps/connect/gmail"),
            json!({"id": new_id()}),
        )
        .await?;
    }
    let links = state.lock().map_err(|_| "remote")?.links.clone();
    ensure!(
        links.len() == 3 && links.iter().collect::<std::collections::HashSet<_>>().len() == 3,
        "links: {links:?}"
    );
    ensure!(
        links[1] == format!("{}-project-{p_id}", links[0]),
        "project identity: {links:?}"
    );
    ensure!(
        links[2] == format!("{}-project-{b_id}", links[0]),
        "project identity: {links:?}"
    );
    for (index, scope) in scopes.iter().enumerate() {
        let accounts = f
            .get(&format!("/api{scope}/connected-apps/accounts"))
            .await?;
        ensure!(
            accounts["items"][0]["id"] == format!("account_{}", index + 1),
            "{scope} accounts: {accounts}"
        );
    }
    let executed = |state: &Arc<Mutex<Remote>>| {
        state
            .lock()
            .map(|state| state.executed.clone())
            .unwrap_or_default()
    };
    f.post(
        &format!("/api/agents/{a}/connected-apps/gmail"),
        json!({"id": new_id(), "revision": 0, "enabled": true}),
    )
    .await?;
    f.run(&direct, "Direct app", None).await?;
    ensure!(
        executed(&state).last() == Some(&links[0]),
        "direct app identity: {:?}",
        executed(&state)
    );
    f.run(&xp, "Hidden installation apps", None).await?;
    ensure!(
        executed(&state).len() == 1,
        "installation app ran in a project"
    );
    f.post(
        &format!("/api/projects/{p_id}/connected-apps/grant-all/gmail"),
        json!({"id": new_id(), "confirmed": true}),
    )
    .await?;
    f.run(&xp, "Project A app", None).await?;
    ensure!(
        executed(&state).last() == Some(&links[1]),
        "project identity: {:?}",
        executed(&state)
    );
    f.run(&yp, "Project B no grant", None).await?;
    ensure!(executed(&state).len() == 2, "ungranted project app ran");
    let current = f.get(&format!("/api/projects/{b_id}")).await?;
    f.post(
        &format!("/api/projects/{b_id}/edit"),
        json!({"id": new_id(), "revision": current["revision"], "allowInstallationApps": true, "confirmed": true}),
    )
    .await?;
    f.run(&yp, "Opt-in installation", None).await?;
    ensure!(
        executed(&state).last() == Some(&links[0]),
        "opt-in identity: {:?}",
        executed(&state)
    );
    f.request(
        &format!("/api/projects/{p_id}/agents/{foreign}/connected-apps/gmail"),
        Some(json!({"id": new_id(), "revision": 0, "enabled": true})),
        403,
    )
    .await?;
    let section = format!(
        "/api/channels/{}/turns",
        p["channel"].as_str().ok_or("project channel")?
    );
    f.run(&section, "Project section app", Some(&a)).await?;
    ensure!(
        executed(&state).last() == Some(&links[1]),
        "section identity: {:?}",
        executed(&state)
    );
    state.lock().map_err(|_| "remote")?.write = true;
    let pending = |path: String| {
        let f = &f;
        async move {
            let id = new_id();
            f.request(
                &path,
                Some(json!({"id": id, "text": "Write approval"})),
                202,
            )
            .await?;
            f.app
                .wait_row(&path, &id, |row| {
                    row["approvals"]
                        .as_array()
                        .is_some_and(|cards| cards.iter().any(|card| card["state"] == "approval"))
                })
                .await
        }
    };
    let first = pending(xp.clone()).await?;
    let card = &first["approvals"][0];
    f.post(
        &format!(
            "{xp}/{}/app-approvals/{}",
            id(&first)?,
            card["id"].as_str().ok_or("card")?
        ),
        json!({"id": new_id(), "revision": card["revision"], "decision": "always"}),
    )
    .await?;
    settle(&f.app, &xp, &id(&first)?).await?;
    f.run(&xp, "Always in A", None).await?;
    let global = pending(direct.clone()).await?;
    let denial = &global["approvals"][0];
    f.post(
        &format!(
            "{direct}/{}/app-approvals/{}",
            id(&global)?,
            denial["id"].as_str().ok_or("card")?
        ),
        json!({"id": new_id(), "revision": denial["revision"], "decision": "deny"}),
    )
    .await?;
    settle(&f.app, &direct, &id(&global)?).await?;
    ensure!(
        f.get(&format!("/api/agents/{a}/connected-app-policies"))
            .await?["items"]
            == json!([]),
        "global policy saved"
    );
    ensure!(
        f.get(&format!(
            "/api/projects/{p_id}/agents/{a}/connected-app-policies"
        ))
        .await?["items"]
            .as_array()
            .map(Vec::len)
            == Some(1),
        "project policy"
    );
    f.post(
        &format!("/api/projects/{p_id}/environment"),
        json!({"id": new_id(), "name": "TOKEN", "value": SECRET}),
    )
    .await?;
    state.lock().map_err(|_| "remote")?.reflect = true;
    let redacted = f.run(&xp, "Redact large app output", None).await?;
    ensure!(
        redacted["reply"]
            .as_str()
            .is_some_and(|reply| reply.contains("REDACTED")),
        "app output: {}",
        redacted["reply"]
    );
    state.lock().map_err(|_| "remote")?.reflect = false;
    let tested = f
        .post(
            &format!("/api/projects/{p_id}/mcp/test"),
            json!({"id": new_id(), "name": "Project MCP", "url": mcp_server.origin(), "token": "FAKE_MCP_TOKEN"}),
        )
        .await?;
    let connection = new_id();
    f.post(
        &format!("/api/projects/{p_id}/mcp"),
        json!({"id": connection, "test": tested["id"]}),
    )
    .await?;
    f.request(
        &format!("/api/projects/{b_id}/mcp"),
        Some(json!({"id": new_id(), "test": tested["id"]})),
        409,
    )
    .await?;
    f.post(
        &format!("/api/projects/{p_id}/agents/{a}/mcp/{connection}"),
        json!({"id": new_id(), "revision": 0, "connectionRevision": 1, "tools": ["echo"]}),
    )
    .await?;
    ensure!(
        f.get("/api/mcp").await?["items"] == json!([]),
        "installation MCP list"
    );
    ensure!(
        f.get(&format!("/api/projects/{b_id}/mcp")).await?["items"] == json!([]),
        "project B MCP list"
    );
    {
        let mut script = script.lock().map_err(|_| "script")?;
        script.mode = "list".into();
        script.connection = connection.clone();
    }
    let before = model.calls();
    let listed = f.run(&section, "Project section MCP", Some(&a)).await?;
    ensure!(
        listed["reply"]
            .as_str()
            .is_some_and(|reply| reply.contains("Project MCP")),
        "section list: {}",
        listed["reply"]
    );
    ensure!(
        model.requests()[before..].iter().any(|request| request
            .tool_names()
            .iter()
            .any(|name| name == "list_mcp_tools")),
        "list_mcp_tools not offered"
    );
    ensure!(
        f.run(&xp, "List A MCP", None).await?["reply"]
            .as_str()
            .is_some_and(|reply| reply.contains("Project MCP")),
        "project A list"
    );
    ensure!(
        !f.run(&yp, "List B MCP", None).await?["reply"]
            .as_str()
            .unwrap_or_default()
            .contains(&connection),
        "project B saw project A's connection"
    );
    script.lock().map_err(|_| "script")?.mode = "mcp".into();
    ensure!(
        f.run(&xp, "Call A MCP", None).await?["reply"]
            .as_str()
            .is_some_and(|reply| reply.contains("REDACTED")),
        "MCP output redaction"
    );
    // A foreign project's connection is refused as an MCP error (Node: the
    // direct client failed the turn).
    let blocked = f.run(&yp, "Call foreign MCP", None).await?;
    ensure!(
        blocked["reply"]
            .as_str()
            .is_some_and(|reply| reply.contains("error")),
        "foreign MCP: {}",
        blocked["reply"]
    );
    ensure!(
        !model
            .requests()
            .iter()
            .any(|request| request.body.to_string().contains(SECRET)),
        "a model request carried the secret"
    );
    f.app.restart().await?;
    ensure!(
        f.get(&format!("/api/projects/{p_id}/mcp")).await?["items"][0]["id"] == connection.as_str(),
        "MCP after restart"
    );
    ensure!(
        f.get(&format!("/api/projects/{p_id}/connected-apps/accounts"))
            .await?["items"][0]["id"]
            == "account_2",
        "accounts after restart"
    );
    f.run(&xp, "MCP after restart", None).await?;
    state.lock().map_err(|_| "remote")?.binary = true;
    let binary_turn = new_id();
    f.request(
        &xp,
        Some(json!({"id": binary_turn, "text": "Refuse secret file"})),
        202,
    )
    .await?;
    let refused = settle(&f.app, &xp, &binary_turn).await?;
    ensure!(
        refused["status"] == "failed"
            || refused["reply"]
                .as_str()
                .is_some_and(|reply| !reply.contains(SECRET)),
        "secret file: {refused}"
    );
    state.lock().map_err(|_| "remote")?.binary = false;
    let current = f.get(&format!("/api/projects/{p_id}")).await?;
    f.post(
        &format!("/api/projects/{p_id}/delete"),
        json!({"id": new_id(), "revision": current["revision"], "confirmed": true}),
    )
    .await?;
    {
        let state = state.lock().map_err(|_| "remote")?;
        ensure!(
            !state
                .accounts
                .values()
                .any(|account| account == "account_2"),
            "project account kept"
        );
        ensure!(
            state.accounts.contains_key(&links[0]),
            "installation account removed"
        );
        ensure!(
            state.violations.is_empty(),
            "Composio contract: {:?}",
            state.violations
        );
    }
    f.app.shutdown().await?;
    let bytes = std::fs::read(f.root.database())?;
    ensure!(
        !bytes
            .windows(SECRET.len())
            .any(|window| window == SECRET.as_bytes()),
        "project secrets remain in SQLite pages"
    );
    let db = Inspector::open(f.root.database())?;
    ensure!(
        db.scalar::<i64>(
            "SELECT count(*) FROM mcp_files WHERE turn=?",
            [&binary_turn]
        )? == 0,
        "secret file stored"
    );
    ensure!(
        db.scalar::<i64>(
            "SELECT count(*) FROM project_environment WHERE project=?",
            [&p_id]
        )? == 0,
        "environment kept"
    );
    ensure!(
        db.scalar::<i64>("SELECT count(*) FROM mcp_projects WHERE project=?", [&p_id])? == 0,
        "project MCP kept"
    );
    ensure!(!f.leaked()?, "an API response leaked the secret");
    Ok(())
}
