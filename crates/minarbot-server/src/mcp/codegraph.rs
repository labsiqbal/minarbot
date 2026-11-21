use super::*;
use crate::{files::workspace::open_directory, harness::process::Process, turns};
use rustix::fs::{mkdirat, openat, Mode, OFlags};
use std::{
    collections::HashMap,
    fs::{self, File},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{process::Command, sync::Mutex};

const SHELL: &str = "/bin/sh";
const BWRAP: &str = "/usr/bin/bwrap";
const INDEX: &str = ".codegraph";
const PROJECT: &str = "/workspace";
// Bound concurrent indexing memory and avoid racing init/sync for a shared project.
static INDEXING: Mutex<()> = Mutex::const_new(());

fn unavailable() -> Failure {
    error(502, "CodeGraph is unavailable for this turn.")
}

pub fn available(state: &AppState) -> bool {
    executable(state).is_some()
}

fn executable(state: &AppState) -> Option<PathBuf> {
    let cli = crate::harness::executable(state, "codegraph")?
        .canonicalize()
        .ok()?;
    cli.metadata()
        .ok()
        .filter(|m| m.is_file() && m.mode() & 0o111 != 0)?;
    Some(cli)
}

#[derive(Debug, PartialEq, Eq)]
pub struct Project {
    cli: PathBuf,
    directory: PathBuf,
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
    let Some(cli) = executable(state) else {
        return Ok(None);
    };
    Ok(
        turns::tools::workspace(state, db, turn)?.map(|binding| Project {
            cli,
            directory: binding.directory,
        }),
    )
}

pub struct Server {
    args: Vec<String>,
    // Keep the mount sources pinned until the harness or bridge exits.
    _mounts: [File; 2],
}

impl Project {
    pub async fn prepare(&self) -> Result<Server> {
        let _guard = INDEXING.lock().await;
        let root = open_directory(&self.directory).map_err(|_| unavailable())?;
        match mkdirat(&root, INDEX, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => (),
            Err(_) => return Err(unavailable()),
        }
        let index = File::from(
            openat(
                &root,
                INDEX,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| unavailable())?,
        );
        let index_path = fd_path(&index);
        let initialized = match fs::symlink_metadata(Path::new(&index_path).join("codegraph.db")) {
            Ok(meta) if meta.is_file() && meta.nlink() == 1 => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            _ => return Err(unavailable()),
        };
        // Open the pinned directories before unsharing. The shell only passes FDs and argv.
        let mut args = vec![
            "-c".into(),
            r#"exec 3<"$1" 4<"$2" || exit; shift 2; exec "$@""#.into(),
            "minarbot-codegraph".into(),
            fd_path(&root),
            index_path,
            BWRAP.into(),
        ];
        args.extend(sandbox(&self.cli)?);
        let mut child = Command::new(SHELL);
        child
            .args(&args)
            .args(if initialized {
                &["sync", PROJECT][..]
            } else {
                &["init", "--yes", PROJECT]
            })
            .current_dir("/")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        // Incremental turn-start sync avoids a watcher for every idle project or session.
        if !tokio::time::timeout(Duration::from_secs(90), child.status())
            .await
            .is_ok_and(|result| result.is_ok_and(|status| status.success()))
        {
            return Err(unavailable());
        }
        args.extend(["serve", "--mcp", "--no-watch", "--path", PROJECT].map(str::to_owned));
        Ok(Server {
            args,
            _mounts: [root, index],
        })
    }
}

fn fd_path(file: &File) -> String {
    format!("/proc/{}/fd/{}", std::process::id(), file.as_raw_fd())
}

fn bundle(cli: &Path) -> Option<PathBuf> {
    if cli.file_name()? == "npm-shim.js" {
        let package = cli.parent()?;
        let arch = match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            arch => arch,
        };
        let platform = format!("@colbymchenry/codegraph-linux-{arch}");
        return [
            package.join("node_modules").join(&platform),
            package.parent()?.parent()?.join(platform),
        ]
        .into_iter()
        .find(|p| p.join("bin/codegraph").is_file());
    }
    let prefix = cli.parent()?.parent()?;
    (prefix.join("node").is_file() && prefix.join("lib/dist/bin/codegraph.js").is_file())
        .then(|| prefix.to_owned())
}

fn sandbox(cli: &Path) -> Result<Vec<String>> {
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
        "--bind-fd",
        "4",
        "/workspace/.codegraph",
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
        "DO_NOT_TRACK",
        "1",
        "--setenv",
        "CODEGRAPH_TELEMETRY",
        "0",
        "--setenv",
        "CODEGRAPH_NO_UPDATE_CHECK",
        "1",
        "--setenv",
        "CODEGRAPH_NO_DAEMON",
        "1",
        "--setenv",
        "CODEGRAPH_NO_DOWNLOAD",
        "1",
    ]
    .map(str::to_owned)
    .to_vec();
    for command in ["dash", "dirname", "readlink", "git", "env"] {
        let path = format!("/usr/bin/{command}");
        args.extend(["--ro-bind-try".into(), path.clone(), path]);
    }
    args.extend(["--symlink", "dash", "/usr/bin/sh"].map(str::to_owned));
    // ponytail: Mount installed CLI bundles and self-contained executables. Add a
    // packaging-specific mount when another supported install layout needs it.
    let (source, target, launch) = match bundle(cli) {
        Some(bundle) => (bundle, "/opt/codegraph", "/opt/codegraph/bin/codegraph"),
        None => (cli.to_owned(), "/opt/codegraph-cli", "/opt/codegraph-cli"),
    };
    args.extend(
        [
            "--ro-bind",
            source.to_str().ok_or_else(unavailable)?,
            target,
            "--",
            launch,
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
    tools: Vec<Value>,
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
            tools: Vec::new(),
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
        let catalogue = bridge.rpc("tools/list", json!({})).await?;
        // ponytail: CodeGraph lists all tools in one response. Handle cursors if it adds pagination.
        let tools = catalogue["tools"]
            .as_array()
            .filter(|items| items.len() <= 64 && catalogue["nextCursor"].is_null())
            .ok_or_else(unavailable)?;
        for tool in tools {
            let name = string(tool, "name");
            if !name.starts_with("codegraph_")
                || name.len() > 64
                || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || bridge.tools.iter().any(|t| t["name"] == name)
                || tool["inputSchema"]["type"] != "object"
                || tool["execution"]["taskSupport"] == "required"
            {
                return Err(unavailable());
            }
            let mut tool = tool.clone();
            if let Some(properties) = tool["inputSchema"]["properties"].as_object_mut() {
                properties.remove("projectPath");
            }
            tool["inputSchema"]["additionalProperties"] = json!(false);
            bridge.tools.push(tool);
        }
        Ok(bridge)
    }

    pub fn specs(&self) -> impl Iterator<Item = Value> + '_ {
        self.tools.iter().map(|tool| json!({
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
        let tool = self
            .tools
            .iter()
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
