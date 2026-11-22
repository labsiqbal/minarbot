use super::*;
use crate::{files::workspace::open_directory, harness::process::Process, turns};
use std::{
    collections::HashMap,
    fs::{self, DirBuilder, File},
    io::{BufRead, Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt},
    },
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime},
};
use tokio::{process::Command, sync::Mutex};

const SHELL: &str = "/bin/sh";
const BWRAP: &str = "/usr/bin/bwrap";
const CACHE: &str = "codegraph-v1";
const DATABASE: &str = "/index/codegraph.db";
const STATUS: &str = "status.json";
const PROJECT: &str = "/workspace";
// Bound concurrent indexing memory and avoid racing init/sync for a shared project.
static INDEXING: Mutex<()> = Mutex::const_new(());

fn unavailable() -> Failure {
    error(502, "CodeGraph is unavailable for this turn.")
}

pub fn available() -> bool {
    crate::platform::LINUX_SANDBOX && Path::new(BWRAP).is_file()
}

fn executable() -> Option<PathBuf> {
    let path = std::env::current_exe().ok()?.canonicalize().ok()?;
    path.metadata()
        .ok()
        .filter(|m| m.is_file() && m.mode() & 0o111 != 0)?;
    Some(path)
}

#[derive(Debug, PartialEq, Eq)]
pub struct Project {
    directory: PathBuf,
    cache: PathBuf,
}

pub fn project(state: &AppState, db: &Connection, turn: &Value) -> Result<Option<Project>> {
    let Some(context) = turns::tools::project_context(db, turn)? else {
        return Ok(None);
    };
    if !data::exists(
        db,
        "SELECT 1 FROM local_projects WHERE id=? AND deleted=0 AND archived=0 AND codegraph_enabled=1",
        [string(&context, "project")],
    )? {
        return Ok(None);
    }
    if !available() {
        return Ok(None);
    }
    Ok(turns::tools::workspace(state, db, turn)?
        .map(|binding| Project::new(state, binding.directory)))
}

pub struct Server {
    args: Vec<String>,
    // Keep the mount sources pinned until the harness or bridge exits.
    _mounts: [File; 2],
}

impl Project {
    fn new(state: &AppState, directory: PathBuf) -> Self {
        let cache = state
            .config
            .database
            .with_extension(CACHE)
            .join(crate::db::digest(&directory.to_string_lossy()));
        Self { directory, cache }
    }

    pub async fn prepare(&self) -> Result<Server> {
        self.sync(false).await?;
        self.launch("stdio")
    }

    async fn sync(&self, first_index: bool) -> Result<()> {
        let _guard = INDEXING.lock().await;
        if first_index && !self.needs_first_index() {
            return Ok(());
        }
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.cache)
            .map_err(|_| unavailable())?;
        let result = self.run_sync().await;
        let status = match &result {
            Ok(stats) => json!({"state":"ready","files":stats["files"],"errors":stats["errors"]}),
            Err(_) => json!({"state":"error"}),
        };
        let temporary = self.cache.join("status.tmp");
        let saved = fs::write(&temporary, status.to_string())
            .and_then(|()| fs::rename(temporary, self.cache.join(STATUS)));
        saved.map_err(|_| unavailable())?;
        result.map(|_| ())
    }

    async fn run_sync(&self) -> Result<Value> {
        let server = self.launch("sync")?;
        let mut command = Command::new(SHELL);
        command
            .args(&server.args)
            .current_dir("/")
            .env_clear()
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let result = tokio::time::timeout(Duration::from_secs(90), command.output())
            .await
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())?;
        if !result.status.success() || result.stdout.len() > 2048 {
            return Err(unavailable());
        }
        serde_json::from_slice(&result.stdout).map_err(|_| unavailable())
    }

    fn launch(&self, mode: &str) -> Result<Server> {
        let root = open_directory(&self.directory).map_err(|_| unavailable())?;
        let index = open_directory(&self.cache).map_err(|_| unavailable())?;
        let binary = executable().ok_or_else(unavailable)?;
        // Pass only pinned directories and fixed argv through the shell.
        let mut args = vec![
            "-c".into(),
            r#"exec 3<"$1" 4<"$2" || exit; shift 2; exec "$@""#.into(),
            "minarbot-codegraph".into(),
            fd_path(&root),
            fd_path(&index),
            BWRAP.into(),
        ];
        args.extend(sandbox(&binary, mode == "sync")?);
        args.extend(["codegraph", mode, PROJECT, DATABASE].map(str::to_owned));
        Ok(Server {
            args,
            _mounts: [root, index],
        })
    }

    fn status(&self) -> Option<Value> {
        let cache = open_directory(&self.cache).ok()?;
        let file = crate::operator_file::file_at(&cache, STATUS.as_ref()).ok()?;
        let bytes = crate::operator_file::bounded_read(file, 1024).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn needs_first_index(&self) -> bool {
        match self
            .status()
            .as_ref()
            .and_then(|status| status["state"].as_str())
        {
            Some("ready") => !self.cache.join("codegraph.db").is_file(),
            Some("error") => self
                .cache
                .join(STATUS)
                .metadata()
                .and_then(|meta| meta.modified())
                .is_ok_and(|time| {
                    SystemTime::now()
                        .duration_since(time)
                        .is_ok_and(|elapsed| elapsed >= Duration::from_secs(120))
                }),
            _ => true,
        }
    }
}

pub fn project_status(state: &AppState, project: &Value) -> Value {
    if project["codegraphEnabled"] != true {
        return json!({"state":"off"});
    }
    let folders: Vec<_> = project["folders"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if folders.is_empty() {
        return json!({"state":"noFolders"});
    }
    if !available() {
        return json!({"state":"unavailable"});
    }
    let mut statuses = Vec::new();
    for folder in folders {
        let project = Project::new(state, folder.into());
        statuses.push(
            project
                .status()
                .unwrap_or_else(|| json!({"state":"queued"})),
        );
    }
    let files: u64 = statuses
        .iter()
        .filter_map(|status| status["files"].as_u64())
        .sum();
    let errors: u64 = statuses
        .iter()
        .filter_map(|status| status["errors"].as_u64())
        .sum();
    let status = if statuses.iter().any(|status| status["state"] == "error") {
        "error"
    } else if statuses.iter().any(|status| status["state"] == "queued") {
        "queued"
    } else {
        "ready"
    };
    json!({"state":status,"files":files,"errors":errors})
}

pub fn start(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if !available() || state.auth.is_some() {
            return;
        }
        let mut timer = tokio::time::interval(Duration::from_secs(5));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            timer.tick().await;
            if turns::pending(&state) {
                continue;
            }
            let paths = {
                let Ok(db) = state.database.lock() else {
                    return;
                };
                data::all(&db.connection,
                    "SELECT DISTINCT f.directory FROM local_project_folders f JOIN local_projects p ON p.id=f.project
                     WHERE p.deleted=0 AND p.archived=0 AND p.codegraph_enabled=1", [])
                    .unwrap_or_default()
            };
            for path in paths {
                let folder = string(&path, "directory");
                let project = Project::new(&state, folder.into());
                if project.needs_first_index() && !turns::pending(&state) {
                    let _ = project.sync(true).await;
                }
            }
        }
    })
}

fn fd_path(file: &File) -> String {
    format!("/proc/{}/fd/{}", std::process::id(), file.as_raw_fd())
}

fn sandbox(binary: &Path, write: bool) -> Result<Vec<String>> {
    let mut args = [
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--ro-bind",
        "/usr/lib",
        "/usr/lib",
        "--ro-bind-try",
        "/usr/lib64",
        "/usr/lib64",
        "--symlink",
        "usr/bin",
        "/bin",
        "--symlink",
        "usr/lib",
        "/lib",
        "--symlink",
        "usr/lib64",
        "/lib64",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--ro-bind-fd",
        "3",
        PROJECT,
        if write { "--bind-fd" } else { "--ro-bind-fd" },
        "4",
        "/index",
        "--proc",
        "/proc",
        "--chdir",
        PROJECT,
        "--clearenv",
        "--setenv",
        "HOME",
        "/tmp",
        "--setenv",
        "PATH",
        "/usr/bin:/bin",
        "--setenv",
        "LANG",
        "C.UTF-8",
        "--setenv",
        "RAYON_NUM_THREADS",
        "2",
    ]
    .map(str::to_owned)
    .to_vec();
    args.extend(["--ro-bind-try", "/usr/bin/git", "/usr/bin/git"].map(str::to_owned));
    let source = binary.to_str().ok_or_else(unavailable)?;
    args.extend(
        [
            "--ro-bind",
            source,
            "/opt/minarbot-server",
            "--",
            "/opt/minarbot-server",
        ]
        .map(str::to_owned),
    );
    Ok(args)
}

impl Server {
    pub fn configure(server: Option<&Self>, runtime: &str, config: &mut Value) {
        let Some(server) = server else {
            return;
        };
        let stdio = json!({"command":SHELL,"args":server.args});
        match runtime {
            "claude" => {
                config["mcpServers"]["codegraph"] = stdio;
                config["mcpServers"]["codegraph"]["type"] = json!("stdio");
            }
            "codex" => config["config"]["mcp_servers"]["codegraph"] = stdio,
            _ => {
                if let Some(servers) = config["mcpServers"].as_array_mut() {
                    servers.push(
                        json!({"name":"codegraph","command":SHELL,"args":server.args,"env":[]}),
                    );
                }
            }
        }
    }
}

pub struct Bridge {
    project: Project,
    process: Process,
    _server: Server,
    sequence: u64,
}

/// One client connection per Minarbot turn, including a fresh dedup ledger.
pub fn codegraph_command(args: &[String]) -> crate::db::Result<()> {
    let [mode, root, database] = args else {
        return Err("Usage: minarbot-server codegraph <sync|stdio> <root> <database>".into());
    };
    if mode == "sync" {
        let mut index = minarbot_codegraph::Index::open(root, database)?;
        index.sync()?;
        println!("{}", index.query().stats()?);
        return Ok(());
    }
    if mode != "stdio" {
        return Err("Unknown CodeGraph command.".into());
    }
    let index = minarbot_codegraph::Index::open_read_only(root, database)?;
    exchange(std::io::stdin().lock(), std::io::stdout().lock(), &index)
}

fn exchange(
    mut input: impl BufRead,
    mut output: impl Write,
    index: &minarbot_codegraph::Index,
) -> crate::db::Result<()> {
    let mut session = minarbot_codegraph::ExploreSession::default();
    let mut line = Vec::new();
    loop {
        line.clear();
        const LIMIT: usize = 32 * 1024;
        if (&mut input)
            .take(LIMIT as u64 + 1)
            .read_until(b'\n', &mut line)?
            == 0
        {
            break;
        }
        if line.len() > LIMIT {
            return Err("CodeGraph MCP message exceeds 32 KiB.".into());
        }
        let message: Value = match serde_json::from_slice(&line) {
            Ok(message) => message,
            Err(_) => {
                reply(
                    &mut output,
                    &json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Invalid JSON."}}),
                )?;
                continue;
            }
        };
        let Some(id) = message.get("id") else {
            continue;
        };
        let result = match message["method"].as_str() {
            Some("initialize") => {
                let protocol = message["params"]["protocolVersion"]
                    .as_str()
                    .filter(|version| {
                        matches!(
                            *version,
                            "2024-11-05" | "2025-03-26" | "2025-06-18" | "2025-11-25"
                        )
                    })
                    .unwrap_or("2024-11-05");
                Ok(
                    json!({"protocolVersion":protocol,"capabilities":{"tools":{}},
                    "serverInfo":{"name":"Minarbot CodeGraph","version":"0.1.0"}}),
                )
            }
            Some("ping") => Ok(json!({})),
            Some("tools/list") => Ok(json!({"tools":minarbot_codegraph::mcp_tools()})),
            Some("tools/call") => {
                let name = string(&message["params"], "name");
                let args = message["params"]
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let spec = catalogue().find(|tool| tool["name"] == name);
                let result = if spec
                    .is_some_and(|spec| super::validate::valid(&spec["inputSchema"], &args))
                {
                    match index.query().call(&mut session, name, &args) {
                        Ok(result) => result,
                        Err(error) => {
                            let message = match error {
                                minarbot_codegraph::QueryError::Invalid(reason) => reason,
                                _ => "CodeGraph query failed.",
                            };
                            json!({"isError":true,"content":[{"type":"text","text":message}]})
                        }
                    }
                } else {
                    json!({"isError":true,"content":[{"type":"text","text":"Unknown tool or invalid arguments."}]})
                };
                Ok(result)
            }
            _ => Err(json!({"code":-32601,"message":"Method not found."})),
        };
        let response = match result {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(error) => json!({"jsonrpc":"2.0","id":id,"error":error}),
        };
        reply(&mut output, &response)?;
    }
    Ok(())
}

fn catalogue() -> impl Iterator<Item = &'static Value> {
    minarbot_codegraph::mcp_tools()
        .as_array()
        .into_iter()
        .flatten()
}

fn reply(output: &mut impl Write, value: &Value) -> crate::db::Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

impl Bridge {
    pub async fn open(project: Project) -> Result<Self> {
        let server = project.prepare().await?;
        let process = Process::launch(
            Path::new(SHELL),
            &server.args,
            Path::new("/"),
            &HashMap::new(),
        )
        .map_err(|_| unavailable())?;
        let mut bridge = Self {
            project,
            process,
            _server: server,
            sequence: 0,
        };
        let response = bridge.rpc("initialize", json!({"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"minarbot","version":"0.1.0"}})).await?;
        if !response["capabilities"]["tools"].is_object() {
            return Err(unavailable());
        }
        bridge
            .process
            .send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await
            .map_err(|_| unavailable())?;
        Ok(bridge)
    }

    pub fn specs(&self) -> impl Iterator<Item = Value> {
        catalogue().map(|tool| json!({
            "type":"function",
            "function":{"name":tool["name"],"description":tool["description"],"parameters":tool["inputSchema"]}
        }))
    }

    pub async fn call(&mut self, state: &AppState, turn: &Value, call: &Value) -> Result<Value> {
        {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            self.check_access(state, &guard.connection, turn)?;
        }
        let name = string(&call["function"], "name");
        let tool = catalogue()
            .find(|tool| tool["name"] == name)
            .ok_or_else(|| error(400, "Unknown CodeGraph tool."))?;
        let raw = string(&call["function"], "arguments");
        if raw.len() > 32_000 {
            return Err(error(400, "CodeGraph arguments exceed the size limit."));
        }
        let args: Value =
            serde_json::from_str(raw).map_err(|_| error(400, "Invalid CodeGraph arguments."))?;
        if args.get("projectPath").is_some() || !super::validate::valid(&tool["inputSchema"], &args)
        {
            return Err(error(
                400,
                "CodeGraph arguments must match the current project's tool schema.",
            ));
        }
        let result = match self
            .rpc("tools/call", json!({"name":name,"arguments":args}))
            .await
        {
            Ok(result) => result,
            Err(e) => json!({"error":e.message,"status":e.status}),
        };
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        self.check_access(state, db, turn)?;
        turns::tools::journal(db, turn, call, &result)?;
        Ok(result)
    }

    fn check_access(&self, state: &AppState, db: &Connection, turn: &Value) -> Result<()> {
        if turns::pending(state)
            || !data::exists(
                db,
                "SELECT 1 FROM turns WHERE id=? AND status='running' AND paused=0",
                [string(turn, "id")],
            )?
            || project(state, db, turn)?.as_ref() != Some(&self.project)
        {
            return Err(error(403, "CodeGraph access changed."));
        }
        Ok(())
    }

    async fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        self.sequence += 1;
        let id = self.sequence;
        let result = tokio::time::timeout(Duration::from_secs(20), async {
            self.process
                .send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
                .await?;
            loop {
                let response = self.process.next().await?;
                if response["id"] == id && response.get("method").is_none() {
                    let result = &response["result"];
                    if response["jsonrpc"] != "2.0"
                        || !result.is_object()
                        || result.to_string().len() > 256 * 1024
                    {
                        return Err(unavailable());
                    }
                    return Ok(result.clone());
                }
            }
        })
        .await;
        result
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())
    }
}

#[cfg(test)]
mod tests;
