use super::{allowed_tool, error, release, Access, Result};
use crate::{web::Failure, AppState};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    os::unix::fs::PermissionsExt,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{watch, Mutex as AsyncMutex},
};

const SERVER: &str = "/minarbot-craft-supervisor";
const ENGINE: &str = "/craft-anything-cli";
const PROXY: &str = "/run/minarbot/proxy.sock";
const PROXY_URL: &str = "http://127.0.0.1:3128";
const RESPONSE_LIMIT: u64 = 512 * 1024;

pub struct Slot {
    pub mcp: AsyncMutex<Option<Mcp>>,
    pub stop: watch::Sender<bool>,
    agent: String,
    project: String,
}

#[derive(Default)]
pub struct Sessions {
    pub changes: AsyncMutex<()>,
    sessions: Mutex<HashMap<(String, bool), Arc<Slot>>>,
}

pub struct Turn<'a>(&'a Sessions, &'a str);

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        let _ = self.0.revoke(|id, _, _| id == self.1);
    }
}

impl Sessions {
    pub(super) fn session(&self, turn: &str, access: &Access) -> Result<Arc<Slot>> {
        let mut sessions = self.sessions.lock().map_err(|_| Failure::storage())?;
        Ok(sessions
            .entry((turn.to_owned(), access.deep))
            .or_insert_with(|| {
                Arc::new(Slot {
                    mcp: AsyncMutex::new(None),
                    stop: watch::channel(false).0,
                    agent: access.agent.clone(),
                    project: access.project.clone(),
                })
            })
            .clone())
    }

    fn revoke(&self, matches: impl Fn(&str, bool, &Slot) -> bool) -> Result<()> {
        let mut sessions = self.sessions.lock().map_err(|_| Failure::storage())?;
        sessions.retain(|(turn, deep), slot| {
            if matches(turn, *deep, slot) {
                slot.stop.send_replace(true);
                return false;
            }
            true
        });
        Ok(())
    }

    pub fn clear(&self) -> Result<()> {
        self.revoke(|_, _, _| true)
    }
    pub fn revoke_agent(&self, agent: &str) -> Result<()> {
        self.revoke(|_, _, slot| slot.agent == agent)
    }
    pub fn revoke_deep(&self, project: &str) -> Result<()> {
        self.revoke(|_, deep, slot| deep && slot.project == project)
    }
    pub fn for_turn<'a>(&'a self, turn: &'a str) -> Turn<'a> {
        Turn(self, turn)
    }
}

// This process only runs at the fixed bind-mounted path in the private network namespace.
// Killing bwrap's PID namespace also stops any Chromium descendants.
pub async fn supervise() -> crate::db::Result<()> {
    if std::env::current_exe()?.as_os_str() != SERVER {
        return Err("Craft Anything requires the Computer sandbox.".into());
    }
    let mode = match std::env::args().nth(2).as_deref() {
        Some("--clean-room") => "--clean-room",
        Some("--deep") => "--deep",
        Some("--assets") => "--clean-room",
        _ => return Err("Invalid Craft Anything mode.".into()),
    };
    fs::create_dir("/tmp/craft-bin")?;
    fs::write("/tmp/craft-bin/chromium",
        "#!/bin/sh\nexec \"$MINARBOT_CRAFT_CHROMIUM\" --no-sandbox --disable-dev-shm-usage --proxy-server=http://127.0.0.1:3128 --proxy-bypass-list='<-loopback>' \"$@\"\n")?;
    fs::set_permissions("/tmp/craft-bin/chromium", fs::Permissions::from_mode(0o500))?;
    let listener = TcpListener::bind(("127.0.0.1", 3128)).await?;
    let proxy = tokio::spawn(super::super::desktop_supervisor::forward(listener, PROXY));
    let mut command = Command::new(ENGINE);
    command.args([
        mode,
        "--project",
        "/work/investigation.cany",
        "mcp",
        "--all-tools",
    ]);
    if std::env::args().nth(2).as_deref() == Some("--assets") {
        fs::write("/work/craft.json", json!({"version":5,"name":"Minarbot","request":"Generate an asset",
            "target":{"kind":"none"},"created_ms":0,"updated_ms":0,"generate_assets":true,"asset_budget_usd":0.5}).to_string())?;
        command.args(["--inbox", "/work/inbox"]);
    } else {
        command.args([
            "--allow-run",
            "--restricted",
            "--allow-web",
            "--allow-capture",
        ]);
    }
    let status = command.kill_on_drop(true).status().await?;
    proxy.abort();
    if !status.success() {
        return Err("Craft Anything MCP server stopped.".into());
    }
    Ok(())
}

pub struct Mcp {
    _child: Child,
    pub work: release::Temp,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    tools: Vec<Value>,
    next: u64,
}

impl Mcp {
    pub(super) async fn open(
        state: &AppState,
        access: &Access,
        fal_key: Option<&str>,
    ) -> Result<Self> {
        let proxy = state.computer.craft_proxy()?;
        let browser =
            super::super::runtime::program(&["google-chrome", "google-chrome-stable", "chromium"])
                .ok_or_else(|| error(503, "Install Chromium to use Craft Anything."))?;
        let executable = std::env::current_exe().map_err(|_| Failure::storage())?;
        let root = super::root(state);
        let work = release::Temp::new(&root)?;
        let mut command = Command::new("/usr/bin/bwrap");
        command
            .env_clear()
            .kill_on_drop(true)
            .args([
                "--unshare-all",
                "--die-with-parent",
                "--new-session",
                "--cap-drop",
                "ALL",
                "--ro-bind",
                "/usr",
                "/usr",
                "--ro-bind-try",
                "/etc/alternatives",
                "/etc/alternatives",
                "--ro-bind-try",
                "/etc/fonts",
                "/etc/fonts",
                "--ro-bind-try",
                "/etc/ssl/certs",
                "/etc/ssl/certs",
                "--ro-bind-try",
                "/etc/pki",
                "/etc/pki",
                "--ro-bind-try",
                "/opt/google/chrome",
                "/opt/google/chrome",
                "--symlink",
                "usr/bin",
                "/bin",
                "--symlink",
                "usr/lib",
                "/lib",
                "--symlink",
                "usr/lib64",
                "/lib64",
                "--proc",
                "/proc",
                "--dev",
                "/dev",
                "--tmpfs",
                "/tmp",
                "--dir",
                "/home/craft",
                "--ro-bind",
            ])
            .arg(proxy)
            .arg(PROXY)
            .arg("--ro-bind")
            .arg(executable)
            .arg(SERVER)
            .arg("--ro-bind")
            .arg(release::binary(&root))
            .arg(ENGINE)
            .arg("--bind")
            .arg(&work.0)
            .arg("/work");
        let _target = if fal_key.is_none() {
            access
                .target
                .as_ref()
                .map(|path| {
                    use std::os::fd::AsRawFd;
                    let directory = crate::files::workspace::open_directory(path)
                        .map_err(|_| error(403, "Project folder is unavailable."))?;
                    command.current_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()));
                    command.arg("--ro-bind").arg(".").arg("/target");
                    Ok::<_, Failure>(directory)
                })
                .transpose()?
        } else {
            None
        };
        let mode = if let Some(secret) = fal_key {
            command.env("FAL_KEY", secret);
            "--assets"
        } else if access.deep {
            "--deep"
        } else {
            "--clean-room"
        };
        command
            .args(["--chdir", "/work", "--", SERVER, "--craft-mcp", mode])
            .env("PATH", "/tmp/craft-bin:/usr/bin:/bin")
            .env("HOME", "/home/craft")
            .env("LANG", "C.UTF-8")
            .env("MINARBOT_CRAFT_CHROMIUM", browser)
            .env("HTTP_PROXY", PROXY_URL)
            .env("HTTPS_PROXY", PROXY_URL)
            .env("NO_PROXY", "localhost,127.0.0.1")
            .env("CRAFT_ANYTHING_MCP_MAX_RESPONSE_BYTES", "196608")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|_| error(503, "Computer sandbox is unavailable."))?;
        let mut mcp = Self {
            input: child.stdin.take().ok_or_else(Failure::storage)?,
            output: BufReader::new(child.stdout.take().ok_or_else(Failure::storage)?),
            _child: child,
            work,
            tools: Vec::new(),
            next: 0,
        };
        mcp.rpc("initialize", json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"Minarbot","version":"1"}})).await?;
        mcp.write(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await?;
        let response = mcp.rpc("tools/list", json!({})).await?;
        mcp.tools = response["tools"]
            .as_array()
            .ok_or_else(connection_error)?
            .iter()
            .filter(|t| {
                t["name"].as_str().is_some_and(|name| {
                    if fal_key.is_some() {
                        super::asset_tool(name)
                    } else {
                        allowed_tool(name)
                    }
                })
            })
            .cloned()
            .collect();
        if fal_key.is_none() {
            mcp.tools.extend(super::artifacts::specs());
        }
        mcp.tools
            .sort_by(|a, b| crate::data::string(a, "name").cmp(crate::data::string(b, "name")));
        Ok(mcp)
    }

    pub fn tool(&self, name: &str) -> Option<&Value> {
        self.tools.iter().find(|t| t["name"] == name)
    }

    pub fn catalogue(&self, name: &str, after: &str) -> Value {
        if !name.is_empty() {
            return json!({"tool":self.tool(name)});
        }
        let mut page = self
            .tools
            .iter()
            .filter(|t| crate::data::string(t, "name") > after)
            .take(21)
            .collect::<Vec<_>>();
        let next = if page.len() > 20 {
            page.pop();
            page.last().map(|t| t["name"].clone())
        } else {
            None
        };
        json!({"tools":page.into_iter().map(|t| json!({"name":t["name"],"title":t["title"]})).collect::<Vec<_>>(),"next_after":next})
    }

    async fn write(&mut self, message: &Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(message).map_err(|_| Failure::storage())?;
        bytes.push(b'\n');
        self.input
            .write_all(&bytes)
            .await
            .map_err(|_| connection_error())
    }

    async fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next;
        self.next += 1;
        tokio::time::timeout(Duration::from_secs(110), async {
            self.write(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
                .await?;
            loop {
                let mut bytes = Vec::new();
                let read = (&mut self.output)
                    .take(RESPONSE_LIMIT + 1)
                    .read_until(b'\n', &mut bytes)
                    .await
                    .map_err(|_| connection_error())?;
                if read == 0 || read as u64 > RESPONSE_LIMIT {
                    return Err(connection_error());
                }
                let message: Value =
                    serde_json::from_slice(&bytes).map_err(|_| connection_error())?;
                if message["id"] == id {
                    return message.get("result").cloned().ok_or_else(connection_error);
                }
            }
        })
        .await
        .map_err(|_| {
            error(
                504,
                "Craft Anything timed out. Check effects before retrying.",
            )
        })?
    }

    pub async fn call(&mut self, name: &str, arguments: &Value) -> Result<Value> {
        self.rpc("tools/call", json!({"name":name,"arguments":arguments}))
            .await
    }
}

fn connection_error() -> Failure {
    error(502, "Craft Anything MCP connection failed.")
}

pub async fn until_stopped<T>(
    slot: &Slot,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    let mut stop = slot.stop.subscribe();
    tokio::select! {
        biased;
        _ = stop.wait_for(|stopped| *stopped) => Err(error(409, "Craft Anything access changed or the turn stopped.")),
        result = future => result,
    }
}
