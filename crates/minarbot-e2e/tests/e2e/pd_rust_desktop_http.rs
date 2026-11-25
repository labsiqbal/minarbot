//! Agent Computer over MCP and HTTP (port of `app/rust-desktop-http.test.ts`).

use minarbot_testkit::{
    base64::{engine::general_purpose::STANDARD, Engine},
    db::{Fixture, Inspector},
    desktop::DesktopFixture,
    fake::stdio_mcp::StdioServer,
    fakes::{self, Fake},
    http::{Method, Reply as Exchange},
    image,
    model::{Reply, ScriptedModel},
    parked::{Answer, ParkedModel, Route},
    prelude::*,
    protocol::sha256_hex,
    server::{server_binary, Gateway},
    vnc::VncClient,
    wait::{never, until_within},
    FIXED_CLOCK,
};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

const COMPUTER_TOOLS: [&str; 10] = [
    "computer_screenshot",
    "computer_click",
    "computer_type",
    "computer_key",
    "computer_scroll",
    "computer_open_url",
    "computer_request_user",
    "computer_list_vault",
    "computer_fill_secret",
    "computer_request_secret",
];

async fn desktop(_root: &TestRoot) -> Result<DesktopFixture> {
    let png = image::png(1280, 800, [255, 255, 255, 255])?;
    DesktopFixture::start(png).await
}

/// `POST`/`GET` with an explicit Origin (or none) and content type.
async fn raw(
    app: &Server,
    method: Method,
    path: &str,
    body: Option<Value>,
    origin: Option<&str>,
    content_type: &str,
) -> Result<Exchange> {
    let mut request = app.client()?.request(method.clone(), app.url(path));
    if let Some(origin) = origin {
        request = request.header("Origin", origin);
    }
    if let Some(body) = body {
        request = request
            .header("Content-Type", content_type)
            .body(body.to_string());
    }
    Exchange::read(&method, path, request.send().await?).await
}

async fn status(
    app: &Server,
    method: Method,
    path: &str,
    body: Option<Value>,
    origin: Option<&str>,
) -> Result<u16> {
    Ok(raw(app, method, path, body, origin, "application/json")
        .await?
        .status)
}

fn commands_contain(desktop: &DesktopFixture, needle: &str) -> bool {
    desktop.commands().iter().any(|command| {
        command.args.iter().any(|arg| arg == needle) || command.text().contains(needle)
    })
}

/// A live MCP session for `turn` with `scopes`, stored as the server stores
/// minted sessions (the Node test called `mcpServer.mintAgentSession`).
fn mint_session(app: &Server, agent: &str, turn: &str, scopes: &[&str]) -> Result<String> {
    let token = format!("{}{}", new_id().replace('-', ""), new_id().replace('-', ""));
    Fixture::open(app.database())?.execute(
        "INSERT INTO mcp_server_tokens(id,token_hash,kind,name,agent,turn,scopes,created,request_digest) VALUES(?,?,'session','',?,?,?,?,'')",
        (new_id(), sha256_hex(&token), agent, turn, json!(scopes).to_string(), FIXED_CLOCK),
    )?;
    Ok(token)
}

/// `minarbot-server --mcp-stdio`, the harness stdio MCP bridge.
fn bridge(app: &Server, token: &str) -> Result<StdioServer> {
    let binary = server_binary(None)?;
    let url = app.url("/mcp");
    Ok(StdioServer::new(
        "minarbot",
        &binary.to_string_lossy(),
        &["--mcp-stdio"],
        &[
            ("MINARBOT_MCP_URL", url.as_str()),
            ("MINARBOT_MCP_TOKEN", token),
        ],
    ))
}

#[tokio::test]
async fn harness_stdio_mcp_bridge_delivers_computer_images_input_scoped_access_and_human_control_pause(
) -> Result<()> {
    let root = TestRoot::new()?;
    let fixture = desktop(&root).await?;
    let bin = root.dir("bin")?;
    let codex = fakes::install(Fake::Codex, &bin, "codex")?;
    let config = ServerConfig::new()
        .computer_bridge(fixture.bridge())
        .option("desktop", fixture.options(json!({})))
        .executable("codex", &codex);
    let app = root.start(config).await?;
    let agent = new_id();
    app.post(
        "/api/agents",
        json!({"id": agent, "runtime": "api", "name": "Default coder", "model": "fixture",
            "instructions": "Help."}),
    )
    .await?;
    app.post(
        &format!("/api/agents/{agent}/edit"),
        json!({"id": new_id(), "revision": 1, "name": "Default coder", "model": "fixture",
            "instructions": "Help.", "runtime": "codex", "confirmRuntime": true}),
    )
    .await?;
    let path = format!("/api/agents/{agent}/computer");
    app.post(
        &path,
        json!({"id": new_id(), "revision": 0, "enabled": true}),
    )
    .await?;
    let turns = format!("/api/agents/{agent}/turns");
    let turn = new_id();
    app.post(&turns, json!({"id": turn, "text": "HOLD"}))
        .await?;
    app.wait_status(&turns, &turn, "running").await?;
    let cwd = root.path().to_owned();
    let client = bridge(
        &app,
        &mint_session(&app, &agent, &turn, &["computer:read", "computer:run"])?,
    )?;
    let listed: Vec<String> = client.request(&cwd, "tools/list", json!({})).await?["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool["name"].as_str())
                .filter(|name| name.starts_with("computer_"))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    ensure!(listed == COMPUTER_TOOLS, "computer tools: {listed:?}");
    let screenshot = client.call(&cwd, "computer_screenshot", json!({})).await?;
    ensure!(screenshot["isError"] != true, "screenshot: {screenshot}");
    let image = &screenshot["content"][1];
    ensure!(
        image["type"] == "image",
        "screenshot content: {}",
        screenshot["content"]
    );
    ensure!(image["mimeType"] == "image/png", "screenshot mime: {image}");
    let bytes = STANDARD.decode(image["data"].as_str().unwrap_or(""))?;
    ensure!(
        bytes.get(1..4) == Some(b"PNG".as_slice()),
        "screenshot is not a PNG"
    );
    for (name, args) in [
        ("computer_click", json!({"x": 32, "y": 64})),
        ("computer_type", json!({"text": "Hello from MCP"})),
        ("computer_key", json!({"keys": ["Control_L", "a"]})),
        ("computer_scroll", json!({"dx": 0, "dy": 3})),
        ("computer_open_url", json!({"url": "https://example.com"})),
    ] {
        let result = client.call(&cwd, name, args).await?;
        ensure!(result["isError"] != true, "{name}: {result}");
    }
    ensure!(
        commands_contain(&fixture, "Hello from MCP"),
        "typed text not sent: {:?}",
        fixture.commands()
    );

    // A read-only session cannot act.
    let limited = bridge(
        &app,
        &mint_session(&app, &agent, &turn, &["computer:read"])?,
    )?;
    let denied = limited
        .call(&cwd, "computer_click", json!({"x": 1, "y": 1}))
        .await?;
    // The Rust stdio bridge reports the refusal as JSON-RPC -32603
    // "MCP request failed." (the Node bridge said "Minarbot MCP request failed.").
    ensure!(
        denied["isError"] == true
            && denied["structuredContent"]["status"] == -32603
            && denied.to_string().contains("MCP request failed"),
        "read-only click: {denied}"
    );

    // Taking control pauses the turn and blocks agent input.
    let state = app.get(&path).await?;
    app.post(
        &format!("{path}/control"),
        json!({"operation": "take", "id": new_id(), "revision": state["control"]["revision"]}),
    )
    .await?;
    let count = fixture.commands().len();
    let _ = client
        .call(&cwd, "computer_click", json!({"x": 10, "y": 10}))
        .await;
    ensure!(
        fixture.commands().len() == count,
        "click ran under human control"
    );
    let paused = app.row(&turns, &turn).await?.ok_or("turn")?;
    ensure!(paused["paused"] == 1, "turn not paused: {paused}");
    ensure!(
        !paused["computerWait"].is_null() && paused["computerWait"] != false,
        "computerWait missing: {paused}"
    );
    let state = app.get(&path).await?;
    app.post(
        &format!("{path}/control"),
        json!({"operation": "hand-back", "id": new_id(), "revision": state["control"]["revision"]}),
    )
    .await?;
    app.wait_status(&turns, &turn, "running").await?;
    let resumed = bridge(&app, &mint_session(&app, &agent, &turn, &["computer:run"])?)?;
    let _ = resumed
        .call(
            &cwd,
            "computer_request_user",
            json!({"reason": "Sign in to fixture"}),
        )
        .await;
    let requested = app.row(&turns, &turn).await?.ok_or("turn")?;
    ensure!(
        requested["paused"] == 1,
        "request_user did not pause: {requested}"
    );
    ensure!(
        requested["computerWait"]["reason"] == "Sign in to fixture",
        "computerWait: {}",
        requested["computerWait"]
    );
    // Each pause stopped the harness run, descendants included.
    let workspace = app
        .database()
        .parent()
        .ok_or("data dir")?
        .join("agent-workspaces")
        .join(&agent);
    let descendants: Vec<u64> = fakes::native_log(&workspace)?
        .iter()
        .filter(|row| row["type"] == "descendant")
        .filter_map(|row| row["pid"].as_u64())
        .collect();
    ensure!(descendants.len() == 2, "descendants: {descendants:?}");
    for pid in descendants {
        until(
            format!("harness descendant {pid} exits after the pause"),
            || async {
                let state =
                    std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
                Ok((state.is_empty() || state.contains(") Z ")).then_some(()))
            },
        )
        .await?;
    }
    Ok(())
}

#[derive(Default)]
struct Advertised {
    mode: &'static str,
    tools: Vec<Value>,
    pixels: bool,
    during: bool,
}

fn computer_tools(request: &minarbot_testkit::model::ModelRequest) -> Vec<Value> {
    request.body["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .map(|tool| tool["function"].clone())
                .filter(|function| {
                    function["name"]
                        .as_str()
                        .is_some_and(|name| name.starts_with("computer_"))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn exposed(tools: &[Value]) -> Vec<String> {
    tools
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect()
}

fn advertise_answer(mode: &str, request: &minarbot_testkit::model::ModelRequest) -> Answer {
    let done = mode == "text" || !request.tool_results().is_empty();
    if done {
        Answer::text("Checked.")
    } else if mode == "screenshot" {
        Answer::tool("computer_screenshot", json!({}))
    } else {
        Answer::tool("computer_click", json!({"x": 12, "y": 24}))
    }
}

async fn send_and_wait(app: &Server, agent: &str, state: &str) -> Result<Value> {
    let turns = format!("/api/agents/{agent}/turns");
    let id = new_id();
    app.post(
        &turns,
        json!({"id": id, "text": "Use computer if permitted."}),
    )
    .await?;
    app.wait_status(&turns, &id, state).await
}

#[tokio::test]
async fn server_advertises_gated_tools_sends_model_image_pixels_protects_viewer_routes_and_refuses_uncertain_resume(
) -> Result<()> {
    let root = TestRoot::new()?;
    let fixture = desktop(&root).await?;
    let seen = Arc::new(Mutex::new(Advertised {
        mode: "text",
        ..Advertised::default()
    }));
    let routed = seen.clone();
    let model = ParkedModel::start(move |request| {
        let Ok(mut seen) = routed.lock() else {
            return Route::Now(Answer::fail("poisoned"));
        };
        seen.tools = computer_tools(request);
        if request.messages().iter().any(|message| {
            message["content"].as_array().is_some_and(|parts| {
                parts.iter().any(|part| {
                    part["type"] == "image_url"
                        && part["image_url"]["url"]
                            .as_str()
                            .is_some_and(|url| url.starts_with("data:image/png;base64,"))
                })
            })
        }) {
            seen.pixels = true;
        }
        if seen.during {
            seen.during = false;
            return Route::Park("during".into());
        }
        Route::Now(advertise_answer(seen.mode, request))
    })
    .await?;
    let config = ServerConfig::new()
        .gateway(Gateway::new(model.base_url()))
        .computer_bridge(fixture.bridge())
        .option(
            "desktop",
            fixture.options(json!({"geometry": "1920x1200", "idleMs": 30_000})),
        );
    let mut app = root.start(config.clone()).await?;
    let id = new_id();
    app.post(
        "/api/agents",
        json!({"runtime": "api", "id": id, "name": "Desktop", "model": "fixture",
            "instructions": "Help."}),
    )
    .await?;
    let path = format!("/api/agents/{id}/computer");
    let set_mode = |mode: &'static str| {
        if let Ok(mut seen) = seen.lock() {
            seen.mode = mode;
        }
    };
    let exposed_now = || {
        seen.lock()
            .map(|seen| exposed(&seen.tools))
            .unwrap_or_default()
    };
    ensure!(
        app.get(&path).await?["enabled"] == false,
        "computer enabled by default"
    );
    send_and_wait(&app, &id, "done").await?;
    ensure!(
        exposed_now().is_empty(),
        "tools offered while disabled: {:?}",
        exposed_now()
    );
    ensure!(
        fixture.launches().is_empty(),
        "desktop launched while disabled"
    );
    let enable = json!({"id": new_id(), "revision": 0, "enabled": true});
    ensure!(
        app.post(&path, enable.clone()).await? == app.post(&path, enable.clone()).await?,
        "enable replay differs"
    );
    ensure!(
        app.get(&path).await?["revision"] == 1,
        "revision after enable"
    );
    let mut stale = enable.clone();
    stale["id"] = json!(new_id());
    ensure!(
        status(&app, Method::POST, &path, Some(stale), Some(app.origin())).await? == 409,
        "stale enable accepted"
    );

    // While a turn owns the Computer, the viewer cannot open URLs.
    set_mode("screenshot");
    if let Ok(mut seen) = seen.lock() {
        seen.during = true;
    }
    let turns = format!("/api/agents/{id}/turns");
    let checked_id = new_id();
    app.post(
        &turns,
        json!({"id": checked_id, "text": "Use computer if permitted."}),
    )
    .await?;
    model.wait_parked(&["during"]).await?;
    let before = fixture.commands().len();
    let busy = raw(
        &app,
        Method::POST,
        &format!("{path}/open-url"),
        Some(json!({"url": "https://example.com/busy"})),
        Some(app.origin()),
        "application/json",
    )
    .await?;
    let native_calls = fixture.commands().len() - before;
    let mode = seen.lock().map(|seen| seen.mode).unwrap_or("text");
    let parked = model.parked_request("during").ok_or("parked request")?;
    model.finish("during", advertise_answer(mode, &parked))?;
    let checked = app.wait_status(&turns, &checked_id, "done").await?;
    ensure!(
        checked["activity"]["artifacts"].as_array().map(Vec::len) == Some(1),
        "artifacts: {}",
        checked["activity"]["artifacts"]
    );
    ensure!(
        checked["activity"]["artifacts"][0]["kind"] == "screenshot",
        "artifact: {}",
        checked["activity"]["artifacts"][0]
    );
    ensure!(
        checked["generatedFiles"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "screenshot stored as a file"
    );
    ensure!(
        exposed_now() == COMPUTER_TOOLS,
        "advertised: {:?}",
        exposed_now()
    );
    ensure!(
        seen.lock().is_ok_and(|seen| seen.pixels),
        "model received no pixels"
    );
    ensure!(
        fixture.launches().len() == 1,
        "launches: {}",
        fixture.launches().len()
    );
    ensure!(
        busy.status == 409,
        "busy open-url: {} {}",
        busy.status,
        busy.body
    );
    ensure!(
        busy.body.to_string().contains("Computer is busy"),
        "busy body: {}",
        busy.body
    );
    ensure!(
        native_calls == 0,
        "busy open-url ran {native_calls} commands"
    );
    let tools = seen
        .lock()
        .map(|seen| seen.tools.clone())
        .unwrap_or_default();
    let tool = |name: &str| tools.iter().find(|tool| tool["name"] == name).cloned();
    ensure!(
        tool("computer_screenshot")
            .and_then(|tool| tool["description"].as_str().map(str::to_owned))
            .is_some_and(|description| description.contains("1920x1200")),
        "screenshot description lacks the geometry"
    );
    ensure!(
        tool("computer_click").map(|tool| tool["parameters"]["properties"]["x"].clone())
            == Some(json!({"type": "integer", "minimum": 0, "maximum": 1919})),
        "click x schema"
    );

    // Viewer routes are owner-only and Origin-checked.
    let shot = app
        .client()?
        .get(app.url(&format!("{path}/screenshot")))
        .header("Origin", app.origin())
        .send()
        .await?;
    ensure!(
        shot.status().as_u16() == 200,
        "screenshot: {}",
        shot.status()
    );
    ensure!(
        shot.headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            == Some("image/jpeg"),
        "screenshot type: {:?}",
        shot.headers().get("content-type")
    );
    app.post(
        &format!("{path}/open-url"),
        json!({"url": "https://example.com/result"}),
    )
    .await?;
    ensure!(
        commands_contain(&fixture, "https://example.com/result"),
        "open-url not sent: {:?}",
        fixture.commands()
    );
    ensure!(
        app.get(&path).await?["control"]["mode"] == "agent",
        "control mode"
    );
    ensure!(
        status(
            &app,
            Method::POST,
            &format!("{path}/open-url"),
            Some(json!({"url": "javascript:alert(1)"})),
            Some(app.origin())
        )
        .await?
            == 400,
        "javascript URL accepted"
    );
    let ticket = app
        .post(
            &format!("{path}/vnc-ticket"),
            json!({"mode": "watch", "revision": 0}),
        )
        .await?;
    ensure!(ticket["ticket"].is_string(), "ticket: {ticket}");
    ensure!(
        status(&app, Method::GET, &format!("{path}/stream"), None, None).await? == 404,
        "legacy stream route"
    );
    for suffix in [
        "",
        "/status",
        "/screenshot",
        "/navigate",
        "/back",
        "/forward",
        "/reload",
        "/stop",
        "/switch",
        "/close",
        "/new",
        "/input",
    ] {
        let route = format!("/api/agents/{id}/browser{suffix}");
        ensure!(
            status(&app, Method::GET, &route, None, None).await? == 404,
            "GET {route}"
        );
        ensure!(
            status(
                &app,
                Method::POST,
                &route,
                Some(json!({"id": new_id()})),
                Some(app.origin())
            )
            .await?
                == 404,
            "POST {route}"
        );
    }
    for suffix in ["", "/screenshot"] {
        ensure!(
            status(
                &app,
                Method::GET,
                &format!("{path}{suffix}"),
                None,
                Some("http://evil.invalid")
            )
            .await?
                == 403,
            "foreign origin {suffix}"
        );
    }
    ensure!(
        status(
            &app,
            Method::POST,
            &format!("{path}/start"),
            Some(json!({})),
            None
        )
        .await?
            == 403,
        "start without Origin"
    );
    ensure!(
        status(&app, Method::GET, &format!("{path}?display=0"), None, None).await? == 400,
        "display parameter"
    );
    let other = new_id();
    app.post(
        "/api/agents",
        json!({"runtime": "api", "id": other, "name": "Other", "model": "fixture",
            "instructions": "Help."}),
    )
    .await?;
    ensure!(
        status(
            &app,
            Method::GET,
            &format!("/api/agents/{other}/computer/screenshot"),
            None,
            None
        )
        .await?
            == 403,
        "screenshot of a disabled computer"
    );

    // A failed click is uncertain and never replays on resume.
    set_mode("click");
    fixture.fail();
    let failed = send_and_wait(&app, &id, "failed").await?;
    let failed_id = failed["id"].as_str().ok_or("failed id")?.to_owned();
    let count = fixture.commands().len();
    app.shutdown().await?;
    {
        let db = Inspector::open(app.database())?;
        let saved = db.row(
            "SELECT state, length(image) AS size FROM computer_actions WHERE turn=?",
            [&checked_id],
        )?;
        ensure!(saved["state"] == "done", "screenshot action: {saved}");
        ensure!(
            saved["size"].as_i64().is_some_and(|size| size > 0),
            "screenshot image"
        );
        ensure!(
            db.scalar::<String>(
                "SELECT state FROM computer_actions WHERE turn=?",
                [&failed_id]
            )? == "uncertain",
            "failed action state"
        );
        ensure!(
            db.scalar::<String>(
                "SELECT result FROM flow_tool_calls WHERE turn=?",
                [&failed_id]
            )?
            .contains("uncertain"),
            "failed tool result"
        );
    }
    app.start_stopped(config.clone()).await?;
    let before_resume = model.calls();
    let current = app.wait_status(&turns, &failed_id, "failed").await?;
    app.post(
        &format!("{turns}/{failed_id}/resume"),
        json!({"id": new_id(), "revision": current["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    let resumed = app
        .wait_row(&turns, &failed_id, |row| {
            row["status"] == "failed" && row["revision"] != current["revision"]
        })
        .await?;
    ensure!(
        resumed["error"]
            .as_str()
            .is_some_and(|error| error.contains("Computer action outcome is uncertain")),
        "resume error: {}",
        resumed["error"]
    );
    ensure!(
        fixture.commands().len() == count,
        "resume replayed the click"
    );
    ensure!(model.calls() == before_resume, "resume called the model");

    // With the desktop runtime off, no Computer tools are offered.
    let off = config.clone().option(
        "desktop",
        fixture.options(json!({"geometry": "1920x1200", "idleMs": 30_000, "enabled": false})),
    );
    app.restart_with(off).await?;
    set_mode("text");
    send_and_wait(&app, &id, "done").await?;
    ensure!(
        exposed_now().is_empty(),
        "tools offered while off: {:?}",
        exposed_now()
    );
    app.post(
        &path,
        json!({"id": new_id(), "revision": 1, "enabled": false}),
    )
    .await?;
    Ok(())
}

async fn post_status(
    app: &Server,
    path: String,
    body: Value,
    origin: Option<String>,
    content_type: &'static str,
) -> Result<u16> {
    Ok(raw(
        app,
        Method::POST,
        &path,
        Some(body),
        Some(origin.as_deref().unwrap_or(app.origin())),
        content_type,
    )
    .await?
    .status)
}

/// A turn paused while it waits for the user.
async fn waiting(app: &Server, turns: &str, id: &str) -> Result<Value> {
    app.wait_row(turns, id, |row| {
        row["status"] == "queued" && row["paused"] == 1
    })
    .await
}

/// Product bug B1 (`docs/test-ledger.md`), here for `computer_request_user`
/// and clicks under human control: the tool pauses the turn and answers
/// `waiting_for_user`, and the harness takes one more model step. That step
/// fails at once and is left out of model-call counts; the strict counts
/// stay in the retained Node case.
fn after_pause(request: &minarbot_testkit::model::ModelRequest) -> bool {
    request.messages().last().is_some_and(|message| {
        message["role"] == "tool"
            && message["content"]
                .as_str()
                .is_some_and(|content| content.contains("waiting_for_user"))
    })
}

fn after_pause_reply() -> Reply {
    Reply::status(500, json!({"error": "no model step after a pause (B1)"}))
}

fn work_calls(model: &ScriptedModel) -> usize {
    model
        .requests()
        .iter()
        .filter(|request| !after_pause(request))
        .count()
}

#[derive(Default)]
struct Control {
    action: &'static str,
    last_tool: String,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_control_and_user_input_enforce_origin_revision_validation_and_rate_limits_paused_turn_resumes_once(
) -> Result<()> {
    let root = TestRoot::new()?;
    let fixture = desktop(&root).await?;
    let control = Arc::new(Mutex::new(Control {
        action: "computer_request_user",
        ..Control::default()
    }));
    let state = control.clone();
    let model = ScriptedModel::start().await?;
    model.respond(move |request| {
        let Ok(mut state) = state.lock() else {
            return Reply::status(500, json!({}));
        };
        if after_pause(request) {
            return after_pause_reply();
        }
        match request
            .messages()
            .iter()
            .rev()
            .find(|message| message["role"] == "tool")
        {
            Some(result) => {
                state.last_tool = result["content"].as_str().unwrap_or("").to_owned();
                Reply::text("Continued.")
            }
            None if state.action == "computer_click" => {
                Reply::tool_call("request-control", "computer_click", json!({"x": 1, "y": 1}))
            }
            None => Reply::tool_call(
                "request-control",
                state.action,
                json!({"reason": "Sign in to fixture"}),
            ),
        }
    });
    let config = ServerConfig::new()
        .model(&model)
        .computer_bridge(fixture.bridge())
        .option("desktop", fixture.options(json!({})));
    let mut app = root.start(config).await?;
    let agent = new_id();
    let path = format!("/api/agents/{agent}/computer");
    let turns = format!("/api/agents/{agent}/turns");
    let last_tool = || {
        control
            .lock()
            .map(|state| state.last_tool.clone())
            .unwrap_or_default()
    };
    app.post(
        "/api/agents",
        json!({"runtime": "api", "id": agent, "name": "Control HTTP", "model": "fixture",
            "instructions": "Help."}),
    )
    .await?;
    app.post(
        &path,
        json!({"id": new_id(), "revision": 0, "enabled": true}),
    )
    .await?;
    let input = json!({"revision": 0, "name": "computer_click", "args": {"x": 4, "y": 8}});
    let with = |base: &Value, changes: Value| {
        let mut out = base.clone();
        if let (Some(out), Some(changes)) = (out.as_object_mut(), changes.as_object()) {
            for (key, value) in changes {
                out.insert(key.clone(), value.clone());
            }
        }
        out
    };
    let json_type = "application/json";
    ensure!(
        post_status(
            &app,
            format!("{path}/input"),
            input.clone(),
            None,
            json_type
        )
        .await?
            == 409,
        "input without human control"
    );
    let turn = new_id();
    app.post(&turns, json!({"id": turn, "text": "Request human sign in"}))
        .await?;
    let paused = waiting(&app, &turns, &turn).await?;
    ensure!(
        paused["computerWait"]["reason"] == "Sign in to fixture",
        "computerWait: {}",
        paused["computerWait"]
    );
    ensure!(work_calls(&model) == 1, "calls: {}", work_calls(&model));
    let agents = app.get("/api/agents").await?;
    ensure!(
        agents["agents"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["id"] == agent.as_str()))
            .is_some_and(|row| row["computer_waiting"] == true || row["computer_waiting"] == 1),
        "computer_waiting not listed"
    );
    ensure!(
        post_status(
            &app,
            format!("{turns}/{turn}/resume"),
            json!({"id": new_id(), "revision": paused["revision"]}),
            None,
            json_type
        )
        .await?
            == 409,
        "resume while waiting for the user"
    );
    let take = json!({"id": new_id(), "revision": 0, "operation": "take"});
    ensure!(
        post_status(
            &app,
            format!("{path}/control"),
            take.clone(),
            Some("https://evil.invalid".into()),
            json_type
        )
        .await?
            == 403,
        "foreign take"
    );
    ensure!(
        post_status(
            &app,
            format!("{path}/control"),
            take.clone(),
            None,
            "text/plain"
        )
        .await?
            == 403,
        "text/plain take"
    );
    let taken = app.post(&format!("{path}/control"), take.clone()).await?;
    ensure!(taken["control"]["mode"] == "user", "take: {taken}");
    ensure!(taken["control"]["who"] == "owner", "take: {taken}");
    ensure!(
        taken["control"]["since"].is_string() || taken["control"]["since"].is_number(),
        "take: {taken}"
    );
    app.post(&format!("{path}/control"), take.clone()).await?;
    ensure!(
        app.get(&path).await?["control"]["revision"] == 1,
        "take replay revision"
    );
    ensure!(
        post_status(
            &app,
            format!("{path}/control"),
            json!({"id": new_id(), "revision": 0, "operation": "hand-back"}),
            None,
            json_type
        )
        .await?
            == 409,
        "stale hand-back"
    );
    for (body, origin, expected) in [
        (
            json!({"revision": 1, "name": "computer_open_url", "args": {"url": "ftp://example.test/"}}),
            None,
            400,
        ),
        (
            with(&input, json!({"revision": 1})),
            Some("https://evil.invalid".to_owned()),
            403,
        ),
        (
            with(&input, json!({"revision": 1, "args": {"x": 1280, "y": 0}})),
            None,
            400,
        ),
        (
            with(
                &input,
                json!({"revision": 1, "args": {"x": 1, "y": 1, "display": ":0"}}),
            ),
            None,
            400,
        ),
    ] {
        ensure!(
            post_status(
                &app,
                format!("{path}/input"),
                body.clone(),
                origin,
                json_type
            )
            .await?
                == expected,
            "input {body}"
        );
    }
    app.post(
        &format!("{path}/input"),
        with(&input, json!({"revision": 1})),
    )
    .await?;
    ensure!(
        fixture.commands().len() == 1,
        "input commands: {}",
        fixture.commands().len()
    );
    app.restart().await?;
    ensure!(
        app.get(&path).await?["control"] == taken["control"],
        "control after restart"
    );
    ensure!(work_calls(&model) == 1, "restart called the model");

    // Hand-back resumes the paused turn exactly once.
    let back = json!({"id": new_id(), "revision": 1, "operation": "hand-back"});
    app.post(&format!("{path}/control"), back.clone()).await?;
    app.wait_status(&turns, &turn, "done").await?;
    ensure!(
        last_tool().contains("screen may have changed; take a screenshot first"),
        "resume tool result: {}",
        last_tool()
    );
    ensure!(work_calls(&model) == 2, "calls: {}", work_calls(&model));
    app.post(&format!("{path}/control"), back.clone()).await?;
    ensure!(work_calls(&model) == 2, "hand-back replay resumed again");
    ensure!(
        fixture.commands().len() == 1,
        "commands: {}",
        fixture.commands().len()
    );
    ensure!(
        post_status(
            &app,
            format!("{path}/input"),
            with(&input, json!({"revision": 1})),
            None,
            json_type
        )
        .await?
            == 409,
        "input after hand-back"
    );
    app.restart().await?;
    app.post(&format!("{path}/control"), back.clone()).await?;
    ensure!(work_calls(&model) == 2, "hand-back replay after restart");

    // Dismissing a request resumes with user_declined, once.
    let next = new_id();
    app.post(&turns, json!({"id": next, "text": "Request and decline"}))
        .await?;
    waiting(&app, &turns, &next).await?;
    let dismiss = json!({"id": new_id(), "revision": 2, "operation": "dismiss", "turn": next});
    app.post(&format!("{path}/control"), dismiss.clone())
        .await?;
    app.wait_status(&turns, &next, "done").await?;
    ensure!(
        last_tool().contains("user_declined"),
        "dismiss result: {}",
        last_tool()
    );
    ensure!(work_calls(&model) == 4, "calls: {}", work_calls(&model));
    app.post(&format!("{path}/control"), dismiss.clone())
        .await?;
    ensure!(work_calls(&model) == 4, "dismiss replay resumed again");
    ensure!(
        post_status(
            &app,
            format!("{path}/control"),
            with(&dismiss, json!({"revision": 3, "id": new_id()})),
            None,
            json_type
        )
        .await?
            == 409,
        "second dismiss"
    );

    // A take waits for an admitted agent action to finish.
    if let Ok(mut state) = control.lock() {
        state.action = "computer_click";
    }
    fixture.hold();
    let admitted_turn = new_id();
    let before_action = fixture.commands().len();
    let admitted_before = fixture.admitted();
    app.post(
        &turns,
        json!({"id": admitted_turn, "text": "Admit model click"}),
    )
    .await?;
    until_within(
        "model click reaches the desktop",
        Duration::from_secs(10),
        || async { Ok((fixture.admitted() > admitted_before).then_some(())) },
    )
    .await?;
    let control_path = format!("{path}/control");
    let (taking, mode) = tokio::join!(
        app.post(
            &control_path,
            json!({"id": new_id(), "revision": 3, "operation": "take"})
        ),
        async {
            let mode = app
                .get(&path)
                .await
                .map(|state| state["control"]["mode"].clone());
            fixture.release();
            mode
        }
    );
    ensure!(
        mode? == "agent",
        "control changed during an admitted action"
    );
    ensure!(
        taking?["control"]["mode"] == "user",
        "take after the action"
    );
    app.wait_status(&turns, &admitted_turn, "done").await?;
    ensure!(
        fixture.commands().len() == before_action + 1,
        "commands: {}",
        fixture.commands().len()
    );

    // Under human control a model click waits without acting.
    let blocked = new_id();
    app.post(
        &turns,
        json!({"id": blocked, "text": "Click while owner controls"}),
    )
    .await?;
    waiting(&app, &turns, &blocked).await?;
    ensure!(
        fixture.commands().len() == before_action + 1,
        "blocked click ran"
    );
    {
        let db = Inspector::open(app.database())?;
        ensure!(
            db.scalar::<i64>(
                "SELECT count(*) FROM computer_actions WHERE turn=?",
                [&blocked]
            )? == 0,
            "blocked click recorded an action"
        );
        ensure!(
            db.scalar::<String>(
                "SELECT result FROM flow_tool_calls WHERE turn=?",
                [&blocked]
            )?
            .contains("waiting_for_user"),
            "blocked tool result"
        );
    }
    app.post(
        &format!("{path}/input"),
        json!({"revision": 4, "name": "computer_open_url",
            "args": {"url": "https://example.com/fixture"}}),
    )
    .await?;
    ensure!(
        commands_contain(&fixture, "https://example.com/fixture"),
        "owner input not sent"
    );
    let count = fixture.commands().len();
    app.post(
        &format!("{path}/control"),
        json!({"id": new_id(), "revision": 4, "operation": "hand-back"}),
    )
    .await?;
    ensure!(
        post_status(
            &app,
            format!("{path}/input"),
            with(&input, json!({"revision": 4})),
            None,
            json_type
        )
        .await?
            == 409,
        "input after hand-back"
    );
    app.wait_status(&turns, &blocked, "done").await?;
    ensure!(fixture.commands().len() == count, "blocked click replayed");

    // Held native work makes queue pressure observable: some inputs get 429.
    app.post(
        &format!("{path}/control"),
        json!({"id": new_id(), "revision": 5, "operation": "take"}),
    )
    .await?;
    fixture.hold();
    let (sender, mut statuses) = tokio::sync::mpsc::unbounded_channel();
    let body = with(&input, json!({"revision": 6})).to_string();
    for _ in 0..40 {
        let request = app
            .client()?
            .post(app.url(&format!("{path}/input")))
            .header("Origin", app.origin())
            .header("Content-Type", "application/json")
            .body(body.clone());
        let sender = sender.clone();
        tokio::spawn(async move {
            let status = request
                .send()
                .await
                .map_or(0, |response| response.status().as_u16());
            let _ = sender.send(status);
        });
    }
    drop(sender);
    let mut seen = Vec::new();
    let rejected = tokio::time::timeout(minarbot_testkit::WAIT, async {
        while let Some(status) = statuses.recv().await {
            seen.push(status);
            if status == 429 {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    ensure!(
        rejected,
        "no input was rejected under queue pressure: {seen:?}"
    );
    fixture.release();
    while let Some(status) = statuses.recv().await {
        seen.push(status);
    }
    ensure!(seen.len() == 40, "statuses: {seen:?}");
    ensure!(seen.contains(&429), "statuses: {seen:?}");
    ensure!(
        seen.iter().all(|status| *status == 200 || *status == 429),
        "statuses: {seen:?}"
    );
    Ok(())
}

/// The fake VNC server behind the relay: RFB 3.8 greeting, security, init
/// (1280x800), then every client byte is recorded.
async fn vnc_backend(
    socket: &Path,
) -> Result<(
    Arc<Mutex<Vec<u8>>>,
    Arc<tokio::sync::Notify>,
    tokio::task::JoinHandle<()>,
)> {
    let listener = tokio::net::UnixListener::bind(socket)?;
    let received: Arc<Mutex<Vec<u8>>> = Arc::default();
    let notify: Arc<tokio::sync::Notify> = Arc::default();
    let (record, signal) = (received.clone(), notify.clone());
    let task = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut stream, _)) = listener.accept().await {
            let (record, signal) = (record.clone(), signal.clone());
            tokio::spawn(async move {
                if stream.write_all(b"RFB 003.008\n").await.is_err() {
                    return;
                }
                let mut state = 0;
                let mut buffer = [0u8; 4096];
                while let Ok(read) = stream.read(&mut buffer).await {
                    if read == 0 {
                        break;
                    }
                    let reply: Option<Vec<u8>> = match state {
                        0 => Some(vec![1, 1]),
                        1 => Some(vec![0; 4]),
                        2 => {
                            let mut init = vec![0u8; 24];
                            init[0..2].copy_from_slice(&1280u16.to_be_bytes());
                            init[2..4].copy_from_slice(&800u16.to_be_bytes());
                            Some(init)
                        }
                        _ => {
                            if let Ok(mut record) = record.lock() {
                                record.extend_from_slice(&buffer[..read]);
                            }
                            signal.notify_waiters();
                            None
                        }
                    };
                    state += 1;
                    if let Some(reply) = reply {
                        if stream.write_all(&reply).await.is_err() {
                            break;
                        }
                    }
                }
            });
        }
    });
    Ok((received, notify, task))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[tokio::test]
async fn http_vnc_relay_enforces_owner_host_origin_tickets_viewer_cap_live_control_fencing_and_stop_cleanup(
) -> Result<()> {
    use std::os::fd::AsRawFd;
    let root = TestRoot::new()?;
    let fixture = desktop(&root).await?;
    let config = ServerConfig::new()
        .computer_bridge(fixture.bridge())
        .option("desktop", fixture.options(json!({})));
    let app = root.start(config).await?;
    let agent = new_id();
    let path = format!("/api/agents/{agent}/computer");
    app.post(
        "/api/agents",
        json!({"runtime": "api", "id": agent, "name": "VNC fixture", "model": "fixture",
            "instructions": "Help."}),
    )
    .await?;
    app.post(
        &path,
        json!({"id": new_id(), "revision": 0, "enabled": true}),
    )
    .await?;
    app.post(&format!("{path}/start"), json!({})).await?;
    let x11 = fixture
        .directory()
        .join("agent-desktops")
        .join(&agent)
        .join("x11");
    // Bind through the directory descriptor: the full path can exceed the
    // Unix socket length limit.
    let directory = std::fs::File::open(&x11)?;
    let (received, notify, backend) = vnc_backend(Path::new(&format!(
        "/proc/self/fd/{}/vnc.sock",
        directory.as_raw_fd()
    )))
    .await?;
    let ticket_path = format!("{path}/vnc-ticket");
    let watch = json!({"mode": "watch", "revision": 0});
    ensure!(
        status(
            &app,
            Method::POST,
            &ticket_path,
            Some(watch.clone()),
            Some("https://evil.invalid")
        )
        .await?
            == 403,
        "foreign ticket"
    );
    let peer = app
        .request(Method::POST, &ticket_path)?
        .header("Content-Type", "application/json")
        .bearer_auth("peer-token")
        .body(watch.to_string())
        .send()
        .await?;
    ensure!(
        peer.status().as_u16() == 403,
        "peer ticket: {}",
        peer.status()
    );
    ensure!(
        status(
            &app,
            Method::POST,
            &ticket_path,
            Some(json!({"mode": ["control"], "revision": 0})),
            Some(app.origin())
        )
        .await?
            == 400,
        "array mode"
    );
    ensure!(
        status(
            &app,
            Method::POST,
            &ticket_path,
            Some(json!({"mode": "control", "revision": 0})),
            Some(app.origin())
        )
        .await?
            == 409,
        "control ticket without human control"
    );
    let mint = |mode: &'static str, revision: i64| {
        let app = &app;
        let ticket_path = ticket_path.clone();
        async move {
            let ticket = app
                .post(&ticket_path, json!({"mode": mode, "revision": revision}))
                .await?;
            Ok::<String, minarbot_testkit::Error>(
                ticket["ticket"].as_str().ok_or("ticket")?.to_owned(),
            )
        }
    };
    let url = |ticket: &str| app.url(&format!("{path}/vnc?ticket={ticket}"));
    let refused = |result: Result<VncClient>, status: &str| match result {
        Ok(_) => Err(format!("VNC connection accepted; expected {status}")),
        Err(error) if error.to_string().contains(status) => Ok(()),
        Err(error) => Err(format!("expected {status}, got {error}")),
    };
    let ticket = mint("watch", 0).await?;
    refused(
        VncClient::connect(&url(&ticket), "https://evil.invalid", &[]).await,
        "403",
    )?;
    refused(
        VncClient::connect(&url(&ticket), app.origin(), &[("Host", "evil.invalid")]).await,
        "403",
    )?;
    refused(
        VncClient::connect(
            &url(&ticket),
            app.origin(),
            &[("Authorization", "Bearer script-token")],
        )
        .await,
        "403",
    )?;
    let socket = x11.join("vnc.sock");
    let real = x11.join("vnc.sock.real");
    std::fs::rename(&socket, &real)?;
    std::os::unix::fs::symlink(&real, &socket)?;
    refused(
        VncClient::connect(&url(&mint("watch", 0).await?), app.origin(), &[]).await,
        "503",
    )?;
    std::fs::remove_file(&socket)?;
    std::fs::rename(&real, &socket)?;
    let mut clients = Vec::new();
    let mut one = VncClient::connect(&url(&ticket), app.origin(), &[]).await?;
    one.handshake().await?;
    refused(
        VncClient::connect(&url(&ticket), app.origin(), &[]).await,
        "401",
    )?;
    for _ in 0..2 {
        let mut next =
            VncClient::connect(&url(&mint("watch", 0).await?), app.origin(), &[]).await?;
        next.handshake().await?;
        clients.push(next);
    }
    ensure!(
        status(
            &app,
            Method::POST,
            &ticket_path,
            Some(watch.clone()),
            Some(app.origin())
        )
        .await?
            == 429,
        "viewer cap"
    );
    let key_down = [4, 1, 0, 0, 0, 0, 0, 97];
    let key_up = [4, 0, 0, 0, 0, 0, 0, 97];
    one.key(97, true).await?;
    one.pointer(4, 8, 1).await?;
    never(
        "a watch viewer sends input",
        Duration::from_millis(30),
        || async {
            Ok(received
                .lock()
                .is_ok_and(|bytes| contains(&bytes, &key_down)))
        },
    )
    .await?;
    clients.insert(0, one);

    // Taking control disconnects watchers; a control viewer drives input.
    app.post(
        &format!("{path}/control"),
        json!({"id": new_id(), "revision": 0, "operation": "take"}),
    )
    .await?;
    for client in &clients {
        client.wait_closed().await?;
    }
    let wait_key = |key: [u8; 8]| {
        let received = received.clone();
        let notify = notify.clone();
        async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                let notified = notify.notified();
                if received.lock().is_ok_and(|bytes| contains(&bytes, &key)) {
                    return Ok::<(), minarbot_testkit::Error>(());
                }
                if tokio::time::timeout_at(deadline, notified).await.is_err() {
                    return Err("key event not relayed".into());
                }
            }
        }
    };
    let mut control =
        VncClient::connect(&url(&mint("control", 1).await?), app.origin(), &[]).await?;
    control.handshake().await?;
    control.key(97, true).await?;
    control.pointer(10, 20, 1).await?;
    wait_key(key_down).await?;
    app.post(
        &format!("{path}/control"),
        json!({"id": new_id(), "revision": 1, "operation": "hand-back"}),
    )
    .await?;
    control.wait_closed().await?;
    wait_key(key_up).await?;
    let mut last = VncClient::connect(&url(&mint("watch", 2).await?), app.origin(), &[]).await?;
    last.handshake().await?;
    app.post(&format!("{path}/stop"), json!({})).await?;
    last.wait_closed().await?;
    for client in clients.iter().chain([&control, &last]) {
        client.close().await;
    }
    backend.abort();
    Ok(())
}

#[tokio::test]
async fn always_on_computer_uses_a_desktop_slot_survives_idle_and_restores_on_server_restart(
) -> Result<()> {
    let root = TestRoot::new()?;
    let fixture = desktop(&root).await?;
    let config = ServerConfig::new()
        .computer_bridge(fixture.bridge())
        .option("desktop", fixture.options(json!({"max": 1})))
        // Idle stops follow server time, which must advance here.
        .real_clock();
    let mut app = root.start(config).await?;
    let computer = |name: &'static str| {
        let app = &app;
        async move {
            let id = new_id();
            let path = format!("/api/agents/{id}/computer");
            app.post_expect(
                "/api/agents",
                json!({"id": id, "name": name, "runtime": "api", "model": "fixture",
                    "instructions": "Help."}),
                201,
            )
            .await?;
            app.post(
                &path,
                json!({"id": new_id(), "revision": 0, "enabled": true}),
            )
            .await?;
            Ok::<String, minarbot_testkit::Error>(path)
        }
    };
    let first = computer("Keep on").await?;
    let second = computer("On demand").await?;
    app.post(&format!("{second}/start"), json!({})).await?;
    ensure!(
        app.get(&first).await?["alwaysOnLimitReached"] == true,
        "slot limit not reported"
    );
    let enable = json!({"id": new_id(), "revision": 1, "alwaysOn": true});
    app.post_expect(&first, enable.clone(), 429).await?;
    app.post(&format!("{second}/stop"), json!({})).await?;
    let running = app.post(&first, enable.clone()).await?;
    ensure!(running["alwaysOn"] == true, "always on: {running}");
    ensure!(running["stopsInMs"].is_null(), "stopsInMs: {running}");
    ensure!(
        app.post(&first, enable.clone()).await?["revision"] == running["revision"],
        "enable replay revision"
    );
    app.post_expect(&format!("{second}/start"), json!({}), 429)
        .await?;
    let live = |state: &Value| state["state"] == "running" || state["state"] == "idle";
    never(
        "the always-on desktop stops when idle",
        Duration::from_millis(2150),
        || async { Ok(!live(&app.get(&first).await?)) },
    )
    .await?;
    app.wait_get(&first, live).await?;
    ensure!(
        fixture.launches().len() == 2,
        "launches: {}",
        fixture.launches().len()
    );
    app.restart().await?;
    let restored = app.wait_get(&first, live).await?;
    ensure!(restored["alwaysOn"] == true, "restored: {restored}");
    let launches = fixture.launches();
    ensure!(launches.len() == 3, "launches: {}", launches.len());
    ensure!(
        launches[2].profile == launches[1].profile,
        "restored desktop used another profile"
    );
    ensure!(
        app.get(&second).await?["state"] == "off",
        "second desktop started"
    );
    app.post(
        &first,
        json!({"id": new_id(), "revision": restored["revision"], "alwaysOn": false}),
    )
    .await?;
    app.wait_get(&first, |state| state["state"] == "off")
        .await?;
    app.wait_get(&second, |state| {
        state["state"] == "off" && state["alwaysOnLimitReached"] != true
    })
    .await?;
    Ok(())
}
