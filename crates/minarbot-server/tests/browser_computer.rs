use axum::{extract::State, response::Html, routing::any, Json, Router};
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    net::TcpListener,
    time::{sleep, timeout},
};
use uuid::Uuid;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

const PAGE: &str = r#"<!doctype html><title>Browser fixture</title>
<form onsubmit="event.preventDefault(); statusText.textContent = 'Form submitted'">
<label for="entry">Fixture entry</label><input id="entry" oninput="output.textContent = this.value">
<label for="choice">Fixture choice</label>
<select id="choice" onchange="statusText.textContent = this.value">
<option value="first">First</option><option value="second">Second</option></select>
<button type="button" onclick="statusText.textContent = 'Button clicked'">Fixture button</button>
<style>#hover-copy {display:none} #hover-button:hover + #hover-copy {display:inline}</style>
<button id="hover-button" type="button" onmouseenter="statusText.textContent = 'Button hovered'">Hover fixture</button><span id="hover-copy">Hover CSS active</span>
<button type="button" onclick="history.pushState({}, '', location.href)">Update history</button>
<button type="button" onclick="this.remove()">Remove fixture</button>
<a href="/next">Next page</a>
</form>
<p id="statusText">Fixture ready</p><p id="output"></p><p id="isolation"></p>
<script>
  if (document.cookie.includes("fixtureSession=present")) document.body.append("Cookie restored");
  document.cookie = "fixtureSession=present; Max-Age=3600; Path=/";
  Promise.all([9222, 3128].map(async port => {
    try {
      const reply = await fetch("http://127.0.0.1:" + port + "/json/version");
      return (await reply.text()).includes("webSocketDebuggerUrl");
    } catch { return false; }
  })).then(reachable => isolation.textContent = reachable.some(Boolean) ? "Debug endpoint exposed" : "Debug endpoint blocked");
</script>"#;

struct Fixture {
    server: Child,
    dir: PathBuf,
    origin: String,
    client: reqwest::Client,
    agent: Uuid,
    provider: Arc<Mutex<Provider>>,
}
#[derive(Default)]
struct Provider {
    pending: Option<(&'static str, Value)>,
    result: Option<Value>,
    tools: Vec<String>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(pid) = rustix::process::Pid::from_raw(self.server.id() as i32) {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
        }
        for _ in 0..100 {
            if self.server.try_wait().is_ok_and(|status| status.is_some()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        if self.server.try_wait().is_ok_and(|status| status.is_none()) {
            let _ = self.server.kill();
            let _ = self.server.wait();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Fixture {
    fn path(&self, suffix: &str) -> String {
        format!("/api/agents/{}{suffix}", self.agent)
    }

    async fn request(&self, path: &str, args: Option<Value>) -> Result<Value> {
        let url = format!("{}{path}", self.origin);
        let req = if let Some(args) = args {
            self.client
                .post(url)
                .header("origin", &self.origin)
                .json(&args)
        } else {
            self.client.get(url)
        };
        let res = req.send().await?;
        let status = res.status();
        let body = res.json::<Value>().await?;
        if !status.is_success() {
            return Err(format!("{path} returned {status}: {body}").into());
        }
        Ok(body)
    }

    async fn start_turn(&self, tool: &'static str, args: Value) -> Result<Uuid> {
        {
            let mut provider = self.provider.lock().map_err(|_| "fixture provider lock")?;
            provider.pending = Some((tool, args));
            provider.result = None;
        }
        let turn = Uuid::new_v4();
        self.request(
            &self.path("/turns"),
            Some(json!({"id":turn.to_string(),"text":"Use fixture tool."})),
        )
        .await?;
        Ok(turn)
    }

    async fn await_turn(&self, id: Uuid, expected: &str) -> Result<Value> {
        let id = id.to_string();
        timeout(Duration::from_secs(75), async {
            loop {
                let turns = self.request(&self.path("/turns"), None).await?;
                let turns = turns.as_array().ok_or("expected turns")?;
                if let Some(turn) = turns.iter().find(|turn| turn["id"] == id) {
                    if turn["status"] == expected && (expected != "queued" || turn["paused"] == 1) {
                        return Ok(turn.clone());
                    }
                    if turn["status"] == "failed" || turn["status"] == "done" {
                        return Err(format!(
                            "Expected {expected}, got {}: {}",
                            turn["status"], turn["error"]
                        )
                        .into());
                    }
                }
                sleep(Duration::from_millis(40)).await;
            }
        })
        .await?
    }

    async fn tool(&self, name: &'static str, args: Value) -> Result<Value> {
        let id = self.start_turn(name, args).await?;
        self.await_turn(id, "done")
            .await
            .map_err(|error| format!("{name}: {error}"))?;
        self.provider
            .lock()
            .map_err(|_| "fixture provider lock")?
            .result
            .take()
            .ok_or_else(|| format!("provider saw no result for {name}").into())
    }
}

async fn provider(
    State(state): State<Arc<Mutex<Provider>>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let mut state = match state.lock() {
        Ok(state) => state,
        Err(_) => return Json(json!({})),
    };
    state.tools = body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_owned))
        .collect();
    if let Some((name, args)) = state.pending.take() {
        return Json(json!({"choices":[{"message":{"tool_calls":[{
            "id":"fixture-call","type":"function","function":{"name":name,"arguments":args.to_string()}
        }]},"finish_reason":"tool_calls"}]}));
    }
    if let Some(message) = body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .find(|m| m["role"] == "tool")
    {
        let content = &message["content"];
        state.result = content
            .as_str()
            .and_then(|text| serde_json::from_str(text).ok())
            .or_else(|| Some(content.clone()));
    }
    Json(json!({"choices":[{"message":{"content":"Checked."},"finish_reason":"stop"}]}))
}

fn reference<'a>(snapshot: &'a Value, label: &str) -> Result<&'a str> {
    let tree = snapshot["tree"]
        .as_str()
        .ok_or("expected accessibility tree")?;
    tree.lines()
        .filter(|line| line.contains(label))
        .find_map(|line| {
            line.split_once("[ref=")
                .and_then(|(_, tail)| tail.strip_suffix(']'))
        })
        .ok_or_else(|| format!("Missing ref for {label} in {tree}").into())
}

fn done(result: &Value) {
    assert_eq!(result["status"], "done", "{result}");
}

#[tokio::test]
#[ignore = "requires Chrome, XFCE, Xvnc and unprivileged bwrap"]
async fn agent_tools_drive_visible_chrome_and_keep_refs_and_profiles_fenced() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let state = Arc::new(Mutex::new(Provider::default()));
    let routes = Router::new()
        .route("/", any(|| async { Html(PAGE) }))
        .route(
            "/next",
            any(|| async { Html("<title>Another fixture</title><p>Another fixture page</p>") }),
        )
        .route("/v1/chat/completions", any(provider))
        .with_state(state.clone());
    let _network = tokio::spawn(async move { axum::serve(listener, routes).await });

    let dir = std::env::temp_dir().join(format!("minarbot-browser-{}", Uuid::new_v4().simple()));
    fs::create_dir(&dir)?;
    let server = Command::new(env!("CARGO_BIN_EXE_minarbot-server"))
        .arg("--test-config")
        .env("MINARBOT_TEST_SERVER", "rust")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut f = Fixture {
        server,
        dir,
        origin: String::new(),
        agent: Uuid::new_v4(),
        provider: state,
        client: reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(70))
            .build()?,
    };
    let hostname = "fixture.example.test";
    let url = format!("http://{hostname}:{port}");
    let config = json!({
        "database":f.dir.join("state.sqlite"),"port":0,
        "baseURL":format!("http://127.0.0.1:{port}/v1"),"apiKey":"browser-test-provider-key",
        "desktop":{"enabled":true,"leaseDir":f.dir.join("lease"),"testOnlyProxyTarget":{"hostname":hostname,"port":port}}
    });
    write!(
        f.server.stdin.take().ok_or("server stdin unavailable")?,
        "{config}"
    )?;
    let mut output = BufReader::new(f.server.stdout.take().ok_or("server stdout unavailable")?);
    let mut line = String::new();
    output.read_line(&mut line)?;
    f.origin = line
        .trim()
        .strip_prefix("Minarbot: ")
        .ok_or("server origin unavailable")?
        .to_owned();
    f.request("/api/agents", Some(json!({"id":f.agent.to_string(),"name":"Browser test","model":"fixture","runtime":"api","instructions":"Test only."}))).await?;
    let computer = f.path("/computer");
    f.request(
        &computer,
        Some(json!({"id":Uuid::new_v4().to_string(),"revision":0,"enabled":true})),
    )
    .await?;
    done(&f.tool("browser_navigate", json!({"url":url})).await?);
    {
        let provider = f.provider.lock().map_err(|_| "fixture provider lock")?;
        for name in [
            "browser_snapshot",
            "browser_screenshot",
            "computer_screenshot",
        ] {
            assert!(
                provider.tools.iter().any(|tool| tool == name),
                "tool {name} missing"
            );
        }
    }
    done(
        &f.tool("browser_wait", json!({"text":"Debug endpoint blocked"}))
            .await?,
    );
    done(&f.tool("browser_wait", json!({"network_idle":true})).await?);
    let snapshot = f.tool("browser_snapshot", json!({})).await?;
    let button = reference(&snapshot, "Fixture button")?;
    let entry = reference(&snapshot, "Fixture entry")?;
    let select = reference(&snapshot, "Fixture choice")?;
    let hover = reference(&snapshot, "Hover fixture")?;
    let main_tab = snapshot["tab"].clone();
    assert!(button.len() < 24 && entry != button);
    done(&f.tool("browser_click", json!({"ref":button})).await?);
    done(
        &f.tool("browser_wait", json!({"text":"Button clicked"}))
            .await?,
    );
    done(
        &f.tool("browser_type", json!({"ref":entry,"text":"Fixture café"}))
            .await?,
    );
    done(
        &f.tool("browser_wait", json!({"text":"Fixture café"}))
            .await?,
    );
    done(&f.tool("browser_press", json!({"key":"Enter"})).await?);
    done(
        &f.tool("browser_wait", json!({"text":"Form submitted"}))
            .await?,
    );
    done(
        &f.tool("browser_select", json!({"ref":select,"value":"second"}))
            .await?,
    );
    done(&f.tool("browser_wait", json!({"text":"second"})).await?);
    done(&f.tool("browser_hover", json!({"ref":hover})).await?);
    done(&f.tool("browser_wait", json!({"ref":entry})).await?);
    done(
        &f.tool("browser_wait", json!({"text":"Hover CSS active"}))
            .await?,
    );
    let page = f.tool("browser_extract", json!({})).await?;
    assert!(page["text"]
        .as_str()
        .is_some_and(|text| text.contains("Button hovered")));
    let timeout_result = f
        .tool(
            "browser_wait",
            json!({"text":"Absent text","timeout_ms":40}),
        )
        .await?;
    assert_eq!(timeout_result["error"], "Browser wait timed out.");

    let replacement = f.tool("browser_snapshot", json!({})).await?;
    let fresh = reference(&replacement, "Fixture button")?;
    assert_ne!(fresh, button);
    let stale = f.tool("browser_click", json!({"ref":button})).await?;
    assert!(stale["error"]
        .as_str()
        .is_some_and(|error| error.contains("Stale")));
    done(
        &f.tool(
            "browser_click",
            json!({"ref":reference(&replacement, "Update history")?}),
        )
        .await?,
    );
    let stale = f.tool("browser_click", json!({"ref":fresh})).await?;
    assert!(stale["error"]
        .as_str()
        .is_some_and(|error| error.contains("Stale")));

    let current = f.tool("browser_snapshot", json!({})).await?;
    let removed = reference(&current, "Remove fixture")?;
    done(&f.tool("browser_click", json!({"ref":removed})).await?);
    let stale = f.tool("browser_click", json!({"ref":removed})).await?;
    assert!(stale["error"]
        .as_str()
        .is_some_and(|error| error.contains("Stale")));
    done(
        &f.tool(
            "browser_click",
            json!({"ref":reference(&current, "Next page")?}),
        )
        .await?,
    );
    done(
        &f.tool("browser_wait", json!({"text":"Another fixture page"}))
            .await?,
    );
    let stale = f
        .tool(
            "browser_click",
            json!({"ref":reference(&current, "Fixture button")?}),
        )
        .await?;
    assert!(stale["error"]
        .as_str()
        .is_some_and(|error| error.contains("Stale")));
    done(&f.tool("browser_back", json!({})).await?);
    done(
        &f.tool("browser_wait", json!({"text":"Fixture entry"}))
            .await?,
    );

    let snap = f.tool("browser_snapshot", json!({})).await?;
    let button = reference(&snap, "Fixture button")?;
    let screenshot = f
        .start_turn("browser_screenshot", json!({"ref":button}))
        .await?;
    let checked = f.await_turn(screenshot, "done").await?;
    assert_eq!(
        checked["activity"]["artifacts"][0]["kind"], "screenshot",
        "{checked}"
    );
    let other = f
        .tool(
            "browser_tabs",
            json!({"action":"open","url":format!("{url}/next")}),
        )
        .await?["tab"]
        .clone();
    assert!(other.is_string());
    let stale = f.tool("browser_click", json!({"ref":button})).await?;
    assert!(stale["error"]
        .as_str()
        .is_some_and(|error| error.contains("Stale")));
    done(
        &f.tool("browser_tabs", json!({"action":"switch","tab":main_tab}))
            .await?,
    );

    assert_transport_isolated(f.server.id())?;
    f.request(&format!("{computer}/stop"), Some(json!({})))
        .await?;
    let tabs = f
        .tool("browser_tabs", json!({"action":"list","profile":"Default"}))
        .await?;
    let tabs = tabs["tabs"].as_array().ok_or("restored tabs missing")?;
    assert!(tabs
        .iter()
        .any(|tab| tab["id"] == main_tab && tab["active"] == true));
    assert!(tabs.iter().any(|tab| tab["id"] == other));
    done(
        &f.tool("browser_wait", json!({"text":"Cookie restored"}))
            .await?,
    );
    let invalid = f
        .tool(
            "browser_tabs",
            json!({"action":"switch","tab":other,"profile":"Other"}),
        )
        .await?;
    assert!(invalid["error"]
        .as_str()
        .is_some_and(|error| error.contains("Unknown browser profile")));
    done(
        &f.tool("browser_tabs", json!({"action":"close","tab":other}))
            .await?,
    );

    let before_takeover = f.tool("browser_snapshot", json!({})).await?;
    let before_takeover = reference(&before_takeover, "Fixture button")?;
    f.request(
        &format!("{computer}/control"),
        Some(json!({"id":Uuid::new_v4().to_string(),"revision":0,"operation":"take"})),
    )
    .await?;
    let paused = f
        .start_turn("browser_click", json!({"ref":before_takeover}))
        .await?;
    let turn = f.await_turn(paused, "queued").await?;
    assert_eq!(turn["paused"], 1);
    assert!(turn["computerWait"].is_object());
    f.request(
        &format!("{computer}/control"),
        Some(json!({"id":Uuid::new_v4().to_string(),"revision":1,"operation":"hand-back"})),
    )
    .await?;
    f.await_turn(paused, "done").await?;
    let stale = f
        .tool("browser_click", json!({"ref":before_takeover}))
        .await?;
    assert!(stale["error"]
        .as_str()
        .is_some_and(|error| error.contains("Stale")));
    let page = f.tool("browser_extract", json!({})).await?;
    assert!(page["text"]
        .as_str()
        .is_some_and(|text| !text.contains("Button clicked")));
    Ok(())
}

fn assert_transport_isolated(server: u32) -> Result<()> {
    let processes: Vec<(u32, u32)> = fs::read_dir("/proc")?
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse().ok()?;
            let stat = fs::read_to_string(entry.path().join("stat")).ok()?;
            let ppid = stat
                .rsplit_once(") ")?
                .1
                .split_ascii_whitespace()
                .nth(1)?
                .parse()
                .ok()?;
            Some((pid, ppid))
        })
        .collect();
    let own = |mut pid| loop {
        if pid == server {
            break true;
        }
        let Some((_, parent)) = processes.iter().find(|(id, _)| *id == pid) else {
            break false;
        };
        if *parent <= 1 {
            break false;
        }
        pid = *parent;
    };
    let processes = processes
        .iter()
        .filter(|(pid, _)| own(*pid))
        .filter_map(|(pid, _)| {
            let command = String::from_utf8(fs::read(format!("/proc/{pid}/cmdline")).ok()?).ok()?;
            Some((pid, command))
        })
        .collect::<Vec<_>>();
    let (supervisor, _) = processes
        .iter()
        .find(|(_, args)| args.starts_with("/minarbot-server\0--desktop-supervisor\0"))
        .ok_or("fixture supervisor not found")?;
    let (chrome, command) = processes
        .iter()
        .find(|(_, args)| args.contains("--remote-debugging-pipe") && !args.contains("--type="))
        .ok_or("fixture Chrome not found")?;
    assert!(!command.contains("--remote-debugging-port"));
    for fd in [0, 1] {
        let path = format!("/proc/{supervisor}/fd/{fd}");
        assert!(fs::read_link(&path)?
            .to_string_lossy()
            .starts_with("socket:"));
        assert!(fs::File::open(path).is_err());
    }
    for fd in [3, 4] {
        assert!(fs::File::open(format!("/proc/{chrome}/fd/{fd}")).is_err());
    }
    let shell = Command::new("/usr/bin/bwrap")
        .args([
            "--unshare-all",
            "--ro-bind",
            "/usr",
            "/usr",
            "--symlink",
            "usr/lib",
            "/lib",
            "--symlink",
            "usr/lib64",
            "/lib64",
            "--proc",
            "/proc",
            "--",
            "/usr/bin/sh",
            "-c",
            "test ! -e /proc/$1/fd/0 && test ! -e /proc/$2/fd/3",
            "shell",
        ])
        .arg(supervisor.to_string())
        .arg(chrome.to_string())
        .output()?;
    assert!(
        shell.status.success(),
        "agent shell sandbox could see a private descriptor"
    );
    for family in ["tcp", "tcp6"] {
        let network = fs::read_to_string(format!("/proc/{supervisor}/net/{family}"))?;
        for row in network.lines().skip(1) {
            let fields = row.split_ascii_whitespace().collect::<Vec<_>>();
            if fields.get(3) == Some(&"0A") {
                assert_eq!(
                    fields.get(1),
                    Some(&"0100007F:0C38"),
                    "unexpected desktop listener"
                );
            }
        }
    }
    Ok(())
}
