use super::*;
use crate::mcp::codegraph;
use crate::{
    data::{agents, exists},
    harness::{
        process::{self, Process},
        snapshot::Snapshot,
    },
    mcp::secrets::redact,
    turns,
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{DirBuilder, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

// Codex caps each additionalContext value at 4,000 UTF-8 bytes.
fn additional_context(text: &str) -> Value {
    const MAX_BYTES: usize = 4000;
    let mut context = serde_json::Map::new();
    let mut chunk = String::new();
    let mut save = |chunk: &mut String| {
        let key = format!("minarbot_{:03}", context.len() + 1);
        context.insert(
            key,
            json!({"kind":"application","value":std::mem::take(chunk)}),
        );
    };
    for block in text.split_inclusive("\n\n") {
        if !chunk.is_empty() && chunk.len() + block.len() > MAX_BYTES {
            save(&mut chunk);
        }
        let mut rest = block;
        while rest.len() > MAX_BYTES {
            let (head, tail) = rest.split_at(rest.floor_char_boundary(MAX_BYTES));
            chunk.push_str(head);
            save(&mut chunk);
            rest = tail;
        }
        chunk.push_str(rest);
    }
    if !chunk.is_empty() {
        save(&mut chunk);
    }
    Value::Object(context)
}

pub struct Home {
    pub path: PathBuf,
    pub environment: HashMap<String, String>,
    pub fingerprint: String,
    pub login_id: Option<String>,
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
pub(super) fn private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| Failure::storage())?;
    file.write_all(bytes).map_err(|_| Failure::storage())
}
pub(super) fn home(
    root: &Path,
    runtime: &str,
    account: Option<&data::providers::Account>,
    login: Option<(&str, &[u8])>,
    device: Option<&Path>,
    model: &str,
    fingerprint: String,
) -> Result<Home> {
    let path = root.join(format!(
        "{}{}",
        config::SESSION_PREFIX,
        uuid::Uuid::new_v4()
    ));
    DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .map_err(|_| Failure::storage())?;
    let mut h = Home {
        path: path.clone(),
        environment: HashMap::from([
            ("HOME".into(), path.to_string_lossy().into_owned()),
            (
                "XDG_CONFIG_HOME".into(),
                path.join(".config").to_string_lossy().into_owned(),
            ),
            (
                "XDG_CACHE_HOME".into(),
                path.join(".cache").to_string_lossy().into_owned(),
            ),
            (
                "XDG_DATA_HOME".into(),
                path.join(".local/share").to_string_lossy().into_owned(),
            ),
        ]),
        fingerprint,
        login_id: login.map(|(id, _)| id.to_owned()),
    };
    if let (Some(folder), "codex") = (device, runtime) {
        // The device sign-in lives in Codex's own folder. Point Codex at it and
        // write nothing there; the CLI's own config and rules apply.
        h.environment
            .insert("CODEX_HOME".into(), folder.to_string_lossy().into_owned());
    } else if runtime == "codex" {
        let codex = path.join(".codex");
        DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(codex.join("rules"))
            .map_err(|_| Failure::storage())?;
        h.environment
            .insert("CODEX_HOME".into(), codex.to_string_lossy().into_owned());
        if let Some((_, bytes)) = login {
            private(&codex.join(data::logins::credential_name(runtime)), bytes)?;
            private(&codex.join("config.toml"), data::logins::CODEX_CONFIG)?;
        } else {
            let account = account
                .filter(|account| account.family == "openai-compatible" && !account.base.is_empty())
                .ok_or_else(|| {
                    error(
                        502,
                        "Codex requires an OpenAI-compatible provider in Settings.",
                    )
                })?;
            data::providers::address(&account.base)
                .map_err(|_| error(502, "Invalid Codex provider URL."))?;
            h.environment.insert(
                "MINARBOT_PROVIDER_KEY".into(),
                account
                    .key
                    .clone()
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| "minarbot-no-key".into()),
            );
            private(&codex.join("config.toml"),format!("model = {}\nmodel_provider = \"minarbot\"\n[model_providers.minarbot]\nname = \"Minarbot provider\"\nbase_url = {}\nwire_api = \"responses\"\nenv_key = \"MINARBOT_PROVIDER_KEY\"\nrequires_openai_auth = false\nsupports_websockets = false\n",json!(model),json!(account.base.trim_end_matches('/'))).as_bytes())?;
        }
        let mut rules = [
            "rm", "rmdir", "shred", "dd", "mkfs", "sudo", "su", "mount", "umount", "chmod",
            "chown", "kill", "pkill", "killall",
        ]
        .iter()
        .map(|s| vec![*s])
        .collect::<Vec<_>>();
        rules.extend([
            vec!["git", "push"],
            vec!["git", "reset"],
            vec!["git", "clean"],
            vec!["npm", "publish"],
            vec!["npm", "unpublish"],
            vec!["npm", "login"],
        ]);
        private(
            &codex.join("rules/default.rules"),
            rules
                .iter()
                .map(|v| format!("prefix_rule(pattern={}, decision=\"prompt\")", json!(v)))
                .collect::<Vec<_>>()
                .join("\n")
                .as_bytes(),
        )?;
    } else if runtime == "claude" {
        if let Some((_, bytes)) = login {
            let claude = path.join(".claude");
            DirBuilder::new()
                .mode(0o700)
                .create(&claude)
                .map_err(|_| Failure::storage())?;
            private(&claude.join(data::logins::credential_name(runtime)), bytes)?;
            h.environment.insert(
                "CLAUDE_CONFIG_DIR".into(),
                claude.to_string_lossy().into_owned(),
            );
        } else if let Some(account) = account.filter(|account| !account.base.is_empty()) {
            h.environment.insert(
                "ANTHROPIC_BASE_URL".into(),
                account
                    .base
                    .trim_end_matches('/')
                    .trim_end_matches("/v1")
                    .into(),
            );
            h.environment.insert(
                "ANTHROPIC_API_KEY".into(),
                account
                    .key
                    .clone()
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| "minarbot-no-key".into()),
            );
            h.environment.insert(
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
                "1".into(),
            );
        }
    }
    Ok(h)
}
fn command_prefix(command: &str) -> Option<&str> {
    (command.len() <= 512
        && command.split(' ').all(|s| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_./-".contains(&b))
        }))
    .then_some(command)
}

const CLAUDE_WORKSPACE_SHELL: &str = "Use mcp__minarbot__run_shell for workspace commands. If needed, load it first with ToolSearch (select:mcp__minarbot__run_shell). Use workspace-relative paths or a relative cwd. The sandbox has no host credentials or network.";

#[derive(Debug, PartialEq, Eq)]
enum ClaudeTool {
    Allow,
    UseWorkspaceShell,
    Approve(&'static str),
}

fn claude_tool(
    name: &str,
    args: &Value,
    workspace: &Path,
    writable: bool,
    workspace_shell: bool,
) -> ClaudeTool {
    match name {
        "Bash" if workspace_shell && args["dangerouslyDisableSandbox"] != true => {
            ClaudeTool::UseWorkspaceShell
        }
        "Bash" => ClaudeTool::Approve("command"),
        "WebFetch" | "WebSearch" => ClaudeTool::Approve("network"),
        "ToolSearch" | "TodoWrite" => ClaudeTool::Allow,
        // Minarbot tools enforce workspace grants and approvals on the server.
        name if name.starts_with("mcp__minarbot__") => ClaudeTool::Allow,
        "Read" | "Glob" | "Grep" | "LS" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            let read = matches!(name, "Read" | "Glob" | "Grep" | "LS");
            let path = args["file_path"].as_str().or(args["path"].as_str());
            let glob_outside = name == "Glob"
                && args["pattern"].as_str().is_some_and(|p| {
                    Path::new(p).is_absolute() || p.split('/').any(|part| part == "..")
                });
            if (read || writable)
                && !glob_outside
                && path
                    .or(read.then_some("."))
                    .is_some_and(|p| inside(workspace, p))
            {
                ClaudeTool::Allow
            } else {
                ClaudeTool::Approve("file")
            }
        }
        _ => ClaudeTool::Approve("tool"),
    }
}

pub async fn approve(
    state: &AppState,
    turn_id: &str,
    kind: &str,
    command: Option<&str>,
    description: &str,
    input: &Value,
    token: &str,
) -> Result<bool> {
    let id = uuid::Uuid::new_v4().to_string();
    let mut changes = state.events.subscribe();
    let _wait = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        let turn = one(
            db,
            "SELECT * FROM turns WHERE id=? AND status='running'",
            [turn_id],
        )?
        .ok_or_else(|| error(409, "Task has stopped."))?;
        if turn["agent"] == crate::mcp::guide::ID {
            return Ok(false);
        }
        let profile = agents::read(db, string(&turn, "agent"))?;
        let binding = if kind == "craft" {
            None
        } else {
            let binding = turns::harness::workspace(state, db, &turn)?
                .ok_or_else(|| error(403, "Shell requires a workspace."))?;
            if !binding.write && turns::harness::restricted(db, &turn)? {
                return Ok(false);
            }
            Some(binding)
        };
        let workspace = match &binding {
            Some(binding) => binding.directory.to_str(),
            None => Some(""),
        };
        let secrets = data::providers::accounts(state)?
            .into_iter()
            .filter_map(|a| a.key)
            .chain(std::iter::once(token.into()))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        let prefix = command
            .filter(|_| kind == "command")
            .filter(|c| !secrets.iter().any(|s| c.contains(s)))
            .and_then(command_prefix);
        if !turns::harness::peer(db, turn_id)? {
            if let Some(prefix) = prefix {
                for row in all(db,"SELECT prefix FROM harness_policies WHERE agent=? AND runtime=? AND workspace=?",rusqlite::params![turn["agent"].as_str(),profile["runtime"].as_str(),workspace])?{let saved=string(&row,"prefix");if prefix==saved||prefix.starts_with(&format!("{saved} ")){return Ok(true);}}
            }
        }
        let clean = |v: &str| {
            redact(
                &if token.is_empty() {
                    v.to_owned()
                } else {
                    v.replace(token, "[redacted]")
                },
                &secrets,
                false,
            )
        };
        let serialized = clean(&input.to_string());
        if serialized.len() > 32000 {
            return Err(error(502, "Harness approval arguments exceed 32 KiB."));
        }
        let wait = config::CardWait::start(state, turn_id)?;
        db.execute("INSERT INTO harness_approvals(id,turn,agent,runtime,workspace,kind,description,command,prefix,input,state) VALUES(?,?,?,?,?,?,?,?,?,?,'approval')",rusqlite::params![id,turn_id,turn["agent"].as_str(),profile["runtime"].as_str(),workspace,kind,clean(description),command.map(clean),prefix,serialized])?;
        wait
    };
    loop {
        {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            let row = one(
                &guard.connection,
                "SELECT state,decision FROM harness_approvals WHERE id=?",
                [&id],
            )?
            .ok_or_else(Failure::storage)?;
            if row["state"] == "decided" {
                return Ok(row["decision"] != "deny");
            }
            if row["state"] != "approval"
                || !exists(
                    &guard.connection,
                    "SELECT 1 FROM turns WHERE id=? AND status='running'",
                    [turn_id],
                )?
            {
                return Err(error(
                    409,
                    "Harness approval expired because this turn stopped.",
                ));
            }
        }
        tokio::select! {_=changes.recv()=>(),_=tokio::time::sleep(Duration::from_millis(100))=>()}
    }
}
struct Lease {
    state: AppState,
    id: String,
    external: bool,
}
impl Drop for Lease {
    fn drop(&mut self) {
        crate::mcp::revoke_session(&self.state, &self.id);
        let state = self.state.clone();
        let id = self.id.clone();
        let external = self.external;
        if external {
            let tasks = self.state.config.harness.revocations.clone();
            let task = tokio::spawn(async move {
                let _ = bridge(&state, "revoke", json!({"id":id})).await;
            });
            if let Ok(mut tasks) = tasks.lock() {
                tasks.push(task);
            };
        }
    }
}
async fn bridge(state: &AppState, operation: &str, args: Value) -> Result<Value> {
    let bridge = state
        .config
        .harness
        .test_harness_bridge
        .as_ref()
        .ok_or_else(Failure::storage)?;
    let value: Value = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| Failure::storage())?
        .post(&bridge.origin)
        .bearer_auth(&bridge.token)
        .json(&json!({"operation":operation,"args":args}))
        .send()
        .await
        .map_err(|_| error(502, "Harness fixture bridge unavailable."))?
        .json()
        .await
        .map_err(|_| Failure::storage())?;
    if value.get("error").is_some() {
        return Err(error(502, "Harness fixture bridge failed."));
    }
    Ok(value["value"].clone())
}
fn save_session(
    state: &AppState,
    agent: &str,
    conversation: &str,
    runtime: &str,
    workspace: &Path,
    id: &str,
) -> Result<()> {
    if id.is_empty() || id.len() > 256 {
        return Err(error(502, "Harness session identifier is invalid."));
    }
    let db = state.database.lock().map_err(|_| Failure::storage())?;
    db.connection.execute("INSERT INTO harness_sessions VALUES(?,?,?,?,?) ON CONFLICT(agent,conversation,runtime,workspace) DO UPDATE SET session=excluded.session",rusqlite::params![agent,conversation,runtime,workspace.to_str(),id])?;
    Ok(())
}
fn reminder(
    state: &AppState,
    turn: &Value,
    profile: &Value,
    phase: &str,
) -> Result<Option<String>> {
    if phase != "answer" {
        return Ok(None);
    }
    let db = state.database.lock().map_err(|_| Failure::storage())?;
    turns::harness::reminder(&db.connection, turn, profile)
}
pub(super) async fn rpc(
    process: &mut Process,
    seq: &mut i64,
    method: &str,
    params: Value,
) -> Result<Value> {
    *seq += 1;
    let id = *seq;
    process
        .send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
        .await?;
    tokio::time::timeout(Duration::from_secs(15),async{loop{let value=process.next().await?;if value["id"]==id{if value.get("error").is_some(){return Err(error(502,"Harness rejected a protocol request. Check host login, version and session; no automatic replay."));}return Ok(value["result"].clone());}}}).await.map_err(|_|error(502,format!("Coding harness timed out during {method}. No automatic replay.")))?
}
pub(super) fn acp_args(runtime: &str, path: &Path) -> Vec<String> {
    match runtime {
        "hermes" if path.file_name().is_some_and(|v| v == "hermes-acp") => vec![],
        "grok" => vec![
            "--permission-mode".into(),
            "default".into(),
            "agent".into(),
            "stdio".into(),
        ],
        "gemini" => vec!["--experimental-acp".into()],
        "kiro" => vec![
            "acp".into(),
            "--agent-engine=v3".into(),
            "--auth-method=cli".into(),
        ],
        _ => vec!["acp".into()],
    }
}
struct TurnProcess<'a> {
    process: Option<Process>,
    notification: Option<Value>,
    config: &'a config::Config,
    turn: &'a str,
}
impl Drop for TurnProcess<'_> {
    fn drop(&mut self) {
        if let Some(notification) = self.notification.take() {
            if let Ok(mut processes) = self.config.stopping_processes.lock() {
                if let Some(process) = self.process.take() {
                    processes.insert(
                        self.turn.to_owned(),
                        tokio::spawn(process.cancel(notification)),
                    );
                }
            }
        }
    }
}

fn launch_turn(
    db: &rusqlite::Connection,
    turn: &str,
    launch: impl FnOnce() -> Result<Process>,
) -> Result<Process> {
    if !exists(
        db,
        "SELECT 1 FROM turns WHERE id=? AND status IN('queued','running') AND paused=0",
        [turn],
    )? {
        return Err(error(409, "Task has stopped."));
    }
    launch()
}

#[allow(clippy::too_many_arguments)]
async fn execute(
    state: &AppState,
    turn: &Value,
    profile: &Value,
    runtime: &str,
    executable: Option<&Path>,
    model: &str,
    messages: &[Value],
    phase: &str,
    call: i64,
    token: &str,
    env: HashMap<String, String>,
    saved: Option<String>,
    binding: &crate::config::WorkspaceConfig,
    secrets: Vec<String>,
    clock: Option<config::TurnClock>,
) -> Result<String> {
    let executable = executable.ok_or_else(|| {
        error(
            503,
            "Coding runtime requires an installed executable on PATH.",
        )
    })?;
    let memory = phase.starts_with("memory_");
    let judging = phase == turns::goals::JUDGE || memory;
    let workspace = &binding.directory;
    let mcp_url = crate::mcp::url(state);
    let native_model = if ["codex", "claude"].contains(&runtime)
        || judging && !model.is_empty() && model != string(profile, "model")
    {
        model.to_owned()
    } else {
        let db = state.database.lock().map_err(|_| Failure::storage())?;
        one(
            &db.connection,
            "SELECT model FROM harness_models WHERE agent=? AND runtime=?",
            [string(turn, "agent"), runtime],
        )?
        .map(|v| string(&v, "model").to_owned())
        .unwrap_or_default()
    };
    let instructions = if memory {
        "Complete the memory request in JSON. Use no tools.".into()
    } else if judging {
        "Evaluate supplied evidence and return the requested JSON. Use no tools.".into()
    } else {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let instructions =
            turns::harness::instructions(state, &guard.connection, turn, runtime, &native_model)?;
        redact(&instructions, &secrets, false)
    };
    let codegraph = if runtime == "pi" || phase != "answer" {
        None
    } else {
        let project = {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            if turns::harness::setup(&guard.connection, turn)? {
                None
            } else {
                codegraph::project(state, &guard.connection, turn)?
            }
        };
        match project {
            Some(project) => match project.prepare().await {
                Ok(server) => Some(server),
                Err(e) => {
                    eprintln!("[server] {}", e.message);
                    None
                }
            },
            None => None,
        }
    };
    let mut prompt = messages
        .iter()
        .map(|v| {
            format!(
                "{}: {}",
                string(v, "role"),
                v["content"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v["content"].to_string())
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    prompt = redact(&prompt, &secrets, false);
    if !judging {
        prompt = format!(
            "{}\n\n{prompt}",
            if saved.is_some() {
                "Continuing the saved harness conversation. The Minarbot transcript below is reference context; follow its latest user request."
            } else {
                "Minarbot owns tasks and approvals. Use the minarbot MCP server for agent communication and delegation."
            }
        );
    }
    if messages
        .iter()
        .any(|v| v["role"] == "system" && string(v, "content").contains("Interrupted task context"))
    {
        prompt = format!("Harness restart context. Never replay uncertain operations.\n\n{prompt}");
    }
    if prompt.len() > 2 * 1024 * 1024 {
        return Err(error(
            502,
            "Coding harness context exceeds 2 MiB. Start a new conversation.",
        ));
    }
    let mut session = saved
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut args = if runtime == "codex" {
        vec!["app-server".into()]
    } else if runtime == "claude" {
        let shim = std::env::current_exe().map_err(|_| Failure::storage())?;
        let mut mcp = json!({"mcpServers":{"minarbot":{"type":"stdio","command":shim,"args":["--mcp-stdio"]}}});
        codegraph::Server::configure(codegraph.as_ref(), runtime, &mut mcp);
        let shell = if crate::platform::LINUX_SANDBOX {
            CLAUDE_WORKSPACE_SHELL
        } else {
            "Use Minarbot file tools for workspace tasks."
        };
        let mut args = [
            "--print",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
            "--permission-mode",
            "default",
            "--setting-sources",
            "",
            "--strict-mcp-config",
        ]
        .map(str::to_owned)
        .to_vec();
        if judging {
            mcp = json!({"mcpServers":{}});
            args.extend([
                "--tools".into(),
                String::new(),
                "--no-session-persistence".into(),
            ]);
        }
        args.extend([
            "--append-system-prompt".into(),
            if judging {instructions.clone()} else {format!("{instructions}\n{shell} Native host Bash requires explicit approval. Discover tools needed for the task together; reuse loaded tools and their receipts.")},
            "--mcp-config".into(),
            mcp.to_string(),
        ]);
        if codegraph.is_some() {
            args.extend(["--allowedTools".into(), "mcp__codegraph__*".into()]);
        }
        args.extend([
            if saved.is_some() {
                "--resume".into()
            } else {
                "--session-id".into()
            },
            session.clone(),
        ]);
        if !native_model.is_empty() {
            args.extend(["--model".into(), native_model.clone()]);
        }
        args
    } else {
        acp_args(runtime, executable)
    };
    let extension_home = if runtime == "pi" && !judging {
        Some(process::ProbeHome::new().map_err(|_| Failure::storage())?)
    } else {
        None
    };
    if runtime == "pi" {
        args = vec![
            "--mode".into(),
            "rpc".into(),
            "--offline".into(),
            "--no-extensions".into(),
            "--no-skills".into(),
            "--no-prompt-templates".into(),
            "--no-themes".into(),
            "--no-context-files".into(),
            "--no-mcp".into(),
            "--no-approve".into(),
            "--no-session".into(),
        ];
        if judging {
            args.push("--no-tools".into());
        }
        if let Some(extension_home) = extension_home.as_ref() {
            let extension = extension_home.0.join("minarbot-pi.mjs");
            std::fs::write(
                &extension,
                include_str!("../../../../app/t3/pi-harness-extension.mjs"),
            )
            .map_err(|_| Failure::storage())?;
            args.extend([
                "--extension".into(),
                extension.to_string_lossy().into_owned(),
                "--append-system-prompt".into(),
                instructions.clone(),
            ]);
        }
    }
    let turn_id = string(turn, "id");
    let mut child = TurnProcess {
        process: Some({
            // Serialize the final state check and spawn with Stop.
            let db = state.database.lock().map_err(|_| Failure::storage())?;
            if memory {
                turns::memory::completion_agent(&db.connection, turn_id, phase)?;
                Process::launch(executable, &args, workspace, &env)?
            } else {
                launch_turn(&db.connection, turn_id, || {
                    Process::launch(executable, &args, workspace, &env)
                })?
            }
        }),
        notification: None,
        config: &state.config.harness,
        turn: turn_id,
    };
    let process = child.process.as_mut().ok_or_else(Failure::storage)?;
    if !judging {
        process.record_stderr(state, call, &secrets);
    }
    process.clock = clock;
    let mut seq = 0;
    if runtime == "codex" {
        rpc(process,&mut seq,"initialize",json!({"clientInfo":{"name":"minarbot","title":"Minarbot","version":"0.1.0"},"capabilities":{"experimentalApi":true}})).await?;
        process.send(json!({"method":"initialized"})).await?;
        let environment = if judging {
            HashMap::new()
        } else {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            data::environment::values(
                state,
                &guard.connection,
                string(turn, "id"),
                string(turn, "agent"),
            )?
        };
        let mut params = json!({"cwd":workspace,"approvalPolicy":"on-request","sandbox":if binding.write{"workspace-write"}else{"read-only"},"config":{"mcp_servers":{"minarbot":{"url":mcp_url,"bearer_token_env_var":"MINARBOT_MCP_TOKEN","tool_timeout_sec":crate::files::components::FORM_WAIT_SECONDS+10}},"approvals_reviewer":"user","shell_environment_policy.inherit":"none","shell_environment_policy.set":{"PATH":super::path(state),"LANG":"C.UTF-8","HOME":workspace},"shell_environment_policy.exclude":["MINARBOT_PROVIDER_KEY","MINARBOT_MCP_TOKEN","ANTHROPIC_API_KEY"]}});
        codegraph::Server::configure(codegraph.as_ref(), runtime, &mut params);
        if judging {
            params["approvalPolicy"] = json!("never");
            params["config"]["mcp_servers"] = json!({});
            params["config"]["features.shell_tool"] = json!(false);
            params["config"]["web_search"] = json!("disabled");
            params["ephemeral"] = json!(true);
        }
        for (name, value) in environment {
            params["config"]["shell_environment_policy.set"][name] = json!(value);
        }
        if !native_model.is_empty() {
            params["model"] = json!(native_model);
        }
        if let Some(id) = &saved {
            params["threadId"] = json!(id);
        }
        let response = rpc(
            process,
            &mut seq,
            if saved.is_some() {
                "thread/resume"
            } else {
                "thread/start"
            },
            params,
        )
        .await?;
        session = string(&response["thread"], "id").into();
    } else if runtime == "claude" {
        process.send(json!({"type":"control_request","request_id":"init","request":{"subtype":"initialize","hooks":{"PreToolUse":[{"hookCallbackIds":["approval"],"onFailure":"block"}],"Stop":[{"hookCallbackIds":["stop"]}]}}})).await?;
        loop {
            let value = process.next().await?;
            if value["type"] == "control_response"
                && (value["response"]["subtype"] != "success"
                    || value["response"]["response"]["hook_on_failure_supported"] != true)
            {
                return Err(error(502,"Claude Code lacks fail-closed permission hooks. Update the host executable before using this runtime."));
            }
            if value["type"] == "control_response" && value["response"]["request_id"] == "init" {
                break;
            }
        }
    } else if runtime == "pi" {
        process.send(json!({"id":1,"type":"get_state"})).await?;
        if process.next().await?["success"] != true {
            return Err(process::stopped());
        }
        process.send(json!({"id":2,"type":"get_commands"})).await?;
        if process.next().await?["success"] != true {
            return Err(process::stopped());
        }
        if !native_model.is_empty() {
            process
                .send(json!({"id":3,"type":"get_available_models"}))
                .await?;
            let models = process.next().await?;
            let model = models["data"]["models"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|m| format!("{}/{}", string(m, "provider"), string(m, "id")) == native_model)
                .ok_or_else(|| error(502, "Selected Pi model is unavailable."))?;
            process.send(json!({"id":4,"type":"set_model","provider":model["provider"],"modelId":model["id"]})).await?;
            if process.next().await?["success"] != true {
                return Err(process::stopped());
            }
        }
    } else {
        let init=rpc(process,&mut seq,"initialize",json!({"protocolVersion":1,"clientInfo":{"name":"minarbot","version":"0.1.0"},"clientCapabilities":{"fs":{"readTextFile":!judging,"writeTextFile":!judging},"terminal":false}})).await?;
        if init["protocolVersion"] != 1 {
            return Err(error(
                502,
                "Harness does not support ACP protocol version 1.",
            ));
        }
        if runtime == "antigravity"
            && init["authMethods"]
                .as_array()
                .is_some_and(|v| v.iter().any(|v| v["id"] == "oauth-personal"))
        {
            rpc(
                process,
                &mut seq,
                "authenticate",
                json!({"methodId":"oauth-personal"}),
            )
            .await?;
        }
        let mut params = json!({"cwd":workspace,"mcpServers":[{"type":"http","name":"minarbot","url":mcp_url,"headers":[{"name":"Authorization","value":format!("Bearer {token}")}]}]});
        codegraph::Server::configure(codegraph.as_ref(), runtime, &mut params);
        if judging {
            params["mcpServers"] = json!([]);
        }
        if let Some(id) = &saved {
            params["sessionId"] = json!(id);
        }
        let setup = rpc(
            process,
            &mut seq,
            if saved.is_some() {
                "session/load"
            } else {
                "session/new"
            },
            params,
        )
        .await?;
        session = saved
            .clone()
            .unwrap_or_else(|| string(&setup, "sessionId").into());
        if !memory && !session.is_empty() && session.len() <= 256 {
            let mut params = json!({"sessionId":session});
            if runtime == "grok" {
                params["_meta"] = json!({"cancelTrigger":"ctrl_c"});
            }
            child.notification =
                Some(json!({"jsonrpc":"2.0","method":"session/cancel","params":params}));
        }
        let models = setup["models"]["availableModels"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|v| json!({"id":v["modelId"],"label":v["name"]}))
            .collect::<Vec<_>>();
        {
            let db = state.database.lock().map_err(|_| Failure::storage())?;
            db.connection.execute("INSERT INTO harness_catalog VALUES(?,?) ON CONFLICT(runtime) DO UPDATE SET models=excluded.models",[runtime,json!(models).to_string().as_str()])?;
        }
        if setup["modes"]["availableModes"]
            .as_array()
            .is_some_and(|v| v.iter().any(|v| v["id"] == "default"))
        {
            rpc(
                process,
                &mut seq,
                "session/set_mode",
                json!({"sessionId":session,"modeId":"default"}),
            )
            .await?;
        } else {
            return Err(error(502, "Harness has no supervised permission mode."));
        }
        if !native_model.is_empty() {
            rpc(
                process,
                &mut seq,
                "session/set_model",
                json!({"sessionId":session,"modelId":native_model}),
            )
            .await?;
        }
    }
    let conversation = turn["channel"]
        .as_str()
        .or(turn["thread"].as_str())
        .unwrap_or("direct");
    if runtime != "pi" && !judging {
        save_session(
            state,
            string(turn, "agent"),
            conversation,
            runtime,
            workspace,
            &session,
        )?;
    }
    let native_secrets = secrets.clone();
    let mut snapshot = Snapshot::new(state, call, string(turn, "id"), secrets).private(judging);
    let mut usage = [None, None];
    let estimated_input = (prompt.len() as i64 + 3) / 4;
    let mut acp_segment = 0;
    let mut claude_message = String::new();
    let mut claude_tools = HashMap::<String, (String, Option<String>)>::new();
    for pass in 0..2 {
        let prompt_id;
        if runtime == "codex" {
            let context = additional_context(&instructions);
            let result=rpc(process,&mut seq,"turn/start",json!({"threadId":session,"additionalContext":context,"input":[{"type":"text","text":prompt}],"cwd":workspace,"approvalPolicy":if judging{"never"}else{"on-request"},"sandboxPolicy":if binding.write{json!({"type":"workspaceWrite","writableRoots":[workspace],"networkAccess":false,"excludeTmpdirEnvVar":true,"excludeSlashTmp":true})}else{json!({"type":"readOnly"})}})).await?;
            if !result["turn"]["id"].is_string() {
                return Err(process::stopped());
            }
            prompt_id = Value::Null;
        } else if runtime == "claude" {
            process
                .send(json!({"type":"user","message":{"role":"user","content":prompt}}))
                .await?;
            prompt_id = Value::Null;
        } else if runtime == "pi" {
            process
                .send(json!({"id":2+pass,"type":"prompt","message":prompt}))
                .await?;
            prompt_id = json!(2 + pass);
        } else {
            seq += 1;
            prompt_id = json!(seq);
            acp_segment += 1;
            process.send(json!({"jsonrpc":"2.0","id":seq,"method":"session/prompt","params":{"sessionId":session,"prompt":[{"type":"text","text":format!("<minarbot_context>\n{instructions}\n</minarbot_context>")},{"type":"text","text":prompt}]}})).await?;
        }
        let mut pass_usage = [None, None];
        loop {
            let value = tokio::select! {value=process.next()=>value?,_=tokio::time::sleep(Duration::from_millis(40))=>{snapshot.flush()?;continue;}};
            if runtime == "codex" {
                let method = string(&value, "method");
                let p = &value["params"];
                if value.get("id").is_some() && value.get("method").is_some() {
                    if judging {
                        return Err(error(502, "Private completion attempted to use a tool."));
                    }
                    if ![
                        "mcpServer/elicitation/request",
                        "item/commandExecution/requestApproval",
                        "item/fileChange/requestApproval",
                        "item/permissions/requestApproval",
                    ]
                    .contains(&method)
                    {
                        process.send(json!({"id":value["id"],"error":{"code":-32601,"message":"Request unavailable in this Minarbot session."}})).await?;
                        continue;
                    }
                    let result = if method == "mcpServer/elicitation/request" {
                        if p["threadId"] == session {
                            super::protocol::elicitation(p)
                        } else {
                            json!({"action":"decline"})
                        }
                    } else {
                        if p["threadId"] != session {
                            process.send(json!({"id":value["id"],"error":{"code":-32601,"message":"Unsupported approval request."}})).await?;
                            continue;
                        }
                        let kind = if method.contains("permissions") {
                            if p["permissions"]["network"].is_null() {
                                "file"
                            } else {
                                "network"
                            }
                        } else if method.contains("fileChange") {
                            "file"
                        } else {
                            "command"
                        };
                        let allow = approve(
                            state,
                            string(turn, "id"),
                            kind,
                            p["command"].as_str(),
                            p["reason"].as_str().unwrap_or("Allow harness tool?"),
                            p,
                            token,
                        )
                        .await?;
                        if method.contains("permissions") {
                            json!({"permissions":if allow{p["permissions"].clone()}else{json!({})},"scope":"turn"})
                        } else {
                            json!({"decision":if allow{"accept"}else{"decline"}})
                        }
                    };
                    process
                        .send(json!({"id":value["id"],"result":result}))
                        .await?;
                    continue;
                }
                if p["threadId"].as_str().is_some_and(|id| id != session) {
                    continue;
                }
                if method == "item/agentMessage/delta" {
                    snapshot.text(string(p, "delta"), p["itemId"].as_str(), false)?;
                }
                if method == "item/completed" && p["item"]["type"] == "agentMessage" {
                    snapshot.text(string(&p["item"], "text"), p["item"]["id"].as_str(), true)?;
                }
                if ["item/started", "item/completed"].contains(&method)
                    && p["item"]["type"].as_str().is_some_and(|v| {
                        matches!(
                            v,
                            "commandExecution"
                                | "fileChange"
                                | "mcpToolCall"
                                | "dynamicToolCall"
                                | "webSearch"
                                | "imageView"
                                | "imageGeneration"
                                | "collabAgentToolCall"
                        )
                    })
                {
                    let item = &p["item"];
                    let kind = string(item, "type");
                    let completed = method == "item/completed";
                    let arguments = match kind {
                        "commandExecution" => {
                            json!({"command": item["command"], "cwd": item["cwd"]})
                        }
                        "fileChange" => item["changes"].clone(),
                        "webSearch" => json!({"query": item["query"]}),
                        _ => item["arguments"].clone(),
                    };
                    let result = match kind {
                        "commandExecution" if completed => {
                            json!({"output": item["aggregatedOutput"], "exitCode": item["exitCode"]})
                        }
                        "mcpToolCall" => item
                            .get("error")
                            .filter(|value| !value.is_null())
                            .cloned()
                            .map(|error| json!({"error": error}))
                            .unwrap_or_else(|| {
                                item["result"]["structuredContent"]
                                    .as_object()
                                    .map_or_else(|| item["result"].clone(), |value| json!(value))
                            }),
                        "dynamicToolCall" if completed => {
                            json!({"content": item["contentItems"], "isError": item["success"] == false})
                        }
                        _ => Value::Null,
                    };
                    let status = item["status"].as_str().unwrap_or(if completed {
                        "completed"
                    } else {
                        "running"
                    });
                    snapshot.tool(
                        string(item, "id"),
                        item["tool"].as_str().unwrap_or(kind),
                        status,
                        &arguments,
                        &result,
                    )?;
                }
                if method == "item/completed" && p["item"]["type"] == "commandExecution" {
                    let item = &p["item"];
                    if item.get("exitCode").is_some() {
                        let guard = state.database.lock().map_err(|_| Failure::storage())?;
                        turns::harness::command(
                            &guard.connection,
                            string(turn, "id"),
                            string(item, "id"),
                            &redact(string(item, "command"), &native_secrets, false),
                            item["exitCode"].as_i64(),
                            false,
                        )?;
                    }
                }
                if method == "thread/tokenUsage/updated" {
                    pass_usage = [
                        p["tokenUsage"]["last"]["inputTokens"].as_i64(),
                        p["tokenUsage"]["last"]["outputTokens"].as_i64(),
                    ];
                }
                if method == "turn/completed" {
                    if p["turn"]["status"] != "completed" {
                        return Err(process::stopped());
                    }
                    break;
                }
                if method == "error" && p["willRetry"] != true {
                    return Err(process::stopped());
                }
            } else if runtime == "claude" {
                match string(&value, "type") {
                    "control_request" => {
                        let req = &value["request"];
                        let input = &req["input"];
                        let result = if input["hook_event_name"] == "Stop" {
                            reminder(state, turn, profile, phase)?
                                .map(|reason| json!({"decision":"block","reason":reason}))
                                .unwrap_or(json!({}))
                        } else if req["subtype"] == "hook_callback"
                            || req["subtype"] == "can_use_tool"
                        {
                            let name = input["tool_name"]
                                .as_str()
                                .or(req["tool_name"].as_str())
                                .unwrap_or("");
                            let args = input
                                .get("tool_input")
                                .or(req.get("input"))
                                .cloned()
                                .unwrap_or(json!({}));
                            let policy = claude_tool(
                                name,
                                &args,
                                workspace,
                                binding.write,
                                crate::platform::LINUX_SANDBOX,
                            );
                            let allow = !judging
                                && match policy {
                                    ClaudeTool::Allow => true,
                                    ClaudeTool::UseWorkspaceShell => false,
                                    ClaudeTool::Approve(kind) => {
                                        approve(
                                            state,
                                            string(turn, "id"),
                                            kind,
                                            args["command"].as_str(),
                                            &format!("Claude Code: {name}"),
                                            &args,
                                            token,
                                        )
                                        .await?
                                    }
                                };
                            let reason = match policy {
                                ClaudeTool::UseWorkspaceShell => CLAUDE_WORKSPACE_SHELL,
                                _ if allow => "Allowed by Minarbot.",
                                _ => "Not allowed in Minarbot.",
                            };
                            if req["subtype"] == "can_use_tool" {
                                if allow {
                                    json!({"behavior":"allow","updatedInput":args})
                                } else {
                                    json!({"behavior":"deny","message":reason})
                                }
                            } else {
                                json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":if allow{"allow"}else{"deny"},"permissionDecisionReason":reason}})
                            }
                        } else {
                            json!({})
                        };
                        process.send(json!({"type":"control_response","response":{"subtype":"success","request_id":value["request_id"],"response":result}})).await?;
                    }
                    "stream_event" => {
                        let event = &value["event"];
                        if event["type"] == "message_start" {
                            claude_message = string(&event["message"], "id").into();
                        }
                        if event["type"] == "content_block_delta"
                            && event["delta"]["type"] == "text_delta"
                        {
                            let id = (!claude_message.is_empty())
                                .then(|| format!("{}:{}", claude_message, event["index"]));
                            snapshot.text(string(&event["delta"], "text"), id.as_deref(), false)?;
                        }
                    }
                    "assistant" => {
                        if let Some(content) = value["message"]["content"].as_array() {
                            for (index, block) in content.iter().enumerate() {
                                if block["type"] == "text" {
                                    let id = format!("{}:{index}", string(&value["message"], "id"));
                                    snapshot.text(string(block, "text"), Some(&id), true)?;
                                }
                                if block["type"] == "tool_use" {
                                    claude_tools.insert(
                                        string(block, "id").into(),
                                        (
                                            string(block, "name").into(),
                                            (block["name"] == "Bash").then(|| {
                                                string(&block["input"], "command").to_owned()
                                            }),
                                        ),
                                    );
                                    snapshot.tool(
                                        string(block, "id"),
                                        string(block, "name"),
                                        "running",
                                        &block["input"],
                                        &Value::Null,
                                    )?;
                                }
                            }
                        }
                        claude_message.clear();
                    }
                    "user" => {
                        if let Some(content) = value["message"]["content"].as_array() {
                            for block in content {
                                if block["type"] == "tool_result" {
                                    let Some((name, command)) =
                                        claude_tools.remove(string(block, "tool_use_id"))
                                    else {
                                        continue;
                                    };
                                    snapshot.tool(
                                        string(block, "tool_use_id"),
                                        &name,
                                        if block["is_error"] == true {
                                            "failed"
                                        } else {
                                            "completed"
                                        },
                                        &Value::Null,
                                        &block["content"],
                                    )?;
                                    if let Some(command) = command
                                        .or_else(|| (name == "Read").then(|| "cat file".into()))
                                    {
                                        let guard = state
                                            .database
                                            .lock()
                                            .map_err(|_| Failure::storage())?;
                                        turns::harness::command(
                                            &guard.connection,
                                            string(turn, "id"),
                                            string(block, "tool_use_id"),
                                            &redact(&command, &native_secrets, false),
                                            Some(if block["is_error"] == true { 1 } else { 0 }),
                                            false,
                                        )?;
                                    }
                                }
                            }
                        }
                    }
                    "result" => {
                        if value["subtype"] != "success" || value["is_error"] == true {
                            return Err(process::stopped());
                        }
                        pass_usage = [
                            value["usage"]["input_tokens"].as_i64(),
                            value["usage"]["output_tokens"].as_i64(),
                        ];
                        break;
                    }
                    _ => (),
                }
            } else if runtime == "pi" {
                if value["type"] == "extension_error" {
                    return Err(error(502, "Pi permission extension failed."));
                }
                if value["type"] == "message_update"
                    && value["assistantMessageEvent"]["type"] == "text_delta"
                {
                    snapshot.text(
                        string(&value["assistantMessageEvent"], "delta"),
                        None,
                        false,
                    )?;
                }
                if value["type"] == "extension_ui_request" {
                    let allow = if value["method"] == "confirm"
                        && value["title"] == "Minarbot tool approval"
                    {
                        let args: Value = serde_json::from_str(string(&value, "message"))
                            .map_err(|_| process::stopped())?;
                        approve(
                            state,
                            string(turn, "id"),
                            match string(&args, "toolName") {
                                "bash" | "powershell" => "command",
                                "read" | "write" | "edit" | "grep" | "find" | "ls" => "file",
                                _ => "tool",
                            },
                            args["input"]["command"].as_str(),
                            "Pi: tool approval",
                            &args,
                            token,
                        )
                        .await?
                    } else {
                        false
                    };
                    process.send(json!({"type":"extension_ui_response","id":value["id"],"confirmed":allow,"cancelled":!allow})).await?;
                }
                if value["type"] == "message_end" && value["message"]["role"] == "assistant" {
                    let message = &value["message"];
                    if matches!(message["stopReason"].as_str(), Some("error" | "aborted")) {
                        return Err(process::stopped());
                    }
                    if let Some(content) = message["content"].as_array() {
                        for block in content {
                            if block["type"] == "text" {
                                snapshot.text(string(block, "text"), None, true)?;
                            }
                        }
                    }
                    pass_usage = [
                        message["usage"]["input"].as_i64(),
                        message["usage"]["output"].as_i64(),
                    ];
                }
                if matches!(
                    value["type"].as_str(),
                    Some("tool_execution_start" | "tool_execution_update" | "tool_execution_end")
                ) {
                    snapshot.tool(
                        string(&value, "toolCallId"),
                        string(&value, "toolName"),
                        if value["type"] == "tool_execution_end" {
                            if value["isError"] == true {
                                "failed"
                            } else {
                                "completed"
                            }
                        } else {
                            "running"
                        },
                        &value["args"],
                        &value["result"],
                    )?;
                }
                if value["type"] == "agent_end" {
                    break;
                }
            } else {
                if value["id"] == prompt_id {
                    if value["result"]["stopReason"] != "end_turn" {
                        return Err(process::stopped());
                    }
                    break;
                }
                let p = &value["params"];
                if value.get("id").is_some() {
                    if judging {
                        return Err(error(502, "Private completion attempted to use a tool."));
                    }
                    if p["sessionId"] != session {
                        process.send(json!({"jsonrpc":"2.0","id":value["id"],"error":{"code":-32602,"message":"Request unavailable in this Minarbot session."}})).await?;
                        continue;
                    }
                    if value["method"] == "fs/read_text_file"
                        || value["method"] == "fs/write_text_file"
                    {
                        let result = super::fs::request(
                            state,
                            turn,
                            binding,
                            value["method"] == "fs/write_text_file",
                            p,
                            token,
                        )
                        .await;
                        let reply = match result {
                            Ok(v) => json!({"jsonrpc":"2.0","id":value["id"],"result":v}),
                            Err(_) => {
                                json!({"jsonrpc":"2.0","id":value["id"],"error":{"code":-32602,"message":"Request unavailable in this Minarbot session."}})
                            }
                        };
                        process.send(reply).await?;
                        continue;
                    }
                    if value["method"] != "session/request_permission" {
                        process.send(json!({"jsonrpc":"2.0","id":value["id"],"error":{"code":-32601,"message":"Request unavailable in this Minarbot session."}})).await?;
                        continue;
                    }
                    let result = if value["method"] == "session/request_permission" {
                        let input = &p["toolCall"];
                        let allow = approve(
                            state,
                            string(turn, "id"),
                            if input["kind"] == "execute" {
                                "command"
                            } else if input["kind"] == "fetch" {
                                "network"
                            } else {
                                "file"
                            },
                            input["rawInput"]["command"].as_str(),
                            input["title"].as_str().unwrap_or("Allow harness tool?"),
                            input,
                            token,
                        )
                        .await?;
                        let option = p["options"].as_array().and_then(|opts| {
                            opts.iter().find(|o| {
                                o["kind"] == if allow { "allow_once" } else { "reject_once" }
                            })
                        });
                        option.map(|o|json!({"outcome":{"outcome":"selected","optionId":o["optionId"]}})).unwrap_or(json!({"outcome":{"outcome":"cancelled"}}))
                    } else {
                        json!({})
                    };
                    process
                        .send(json!({"jsonrpc":"2.0","id":value["id"],"result":result}))
                        .await?;
                    continue;
                }
                if value["method"] != "session/update" || p["sessionId"] != session {
                    continue;
                }
                let update = &p["update"];
                if update["sessionUpdate"] == "agent_message_chunk"
                    && update["content"]["type"] == "text"
                {
                    snapshot.text(
                        string(&update["content"], "text"),
                        Some(&format!("acp-{acp_segment}")),
                        false,
                    )?;
                }
                if ["tool_call", "tool_call_update"].contains(&string(update, "sessionUpdate")) {
                    if update["sessionUpdate"] == "tool_call" {
                        acp_segment += 1;
                    }
                    snapshot.tool(
                        string(update, "toolCallId"),
                        string(update, "title"),
                        string(update, "status"),
                        &update["rawInput"],
                        &update["rawOutput"],
                    )?;
                }
            }
        }
        usage = if pass == 0 {
            pass_usage
        } else {
            [
                usage[0].zip(pass_usage[0]).map(|(a, b)| a + b),
                usage[1].zip(pass_usage[1]).map(|(a, b)| a + b),
            ]
        };
        if pass == 0 {
            if let Some(reason) = reminder(state, turn, profile, phase)? {
                prompt = reason;
                continue;
            }
        }
        break;
    }
    child.notification = None;
    let reply = snapshot.finish()?;
    let db = state.database.lock().map_err(|_| Failure::storage())?;
    db.connection.execute("UPDATE provider_calls SET status='received',finished=?,input_tokens=?,output_tokens=?,total_tokens=? WHERE seq=? AND status='pending'",rusqlite::params![data::iso(&db.connection,state.config.now())?,usage[0],usage[1],usage[0].zip(usage[1]).map(|(a,b)|a+b),call])?;
    if state.auth.is_none() && !memory {
        turns::goals::account_unmetered(
            &db.connection,
            string(turn, "id"),
            call,
            estimated_input + (reply.len() as i64 + 3) / 4,
        )?;
    }
    Ok(reply)
}
pub fn inside(workspace: &Path, path: &str) -> bool {
    if path.is_empty() || path.contains('\0') {
        return false;
    }
    let Ok(root) = workspace.canonicalize() else {
        return false;
    };
    let target = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        root.join(path)
    };
    if target
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return false;
    }
    let mut current = target.as_path();
    while !current.exists() {
        let Some(parent) = current.parent() else {
            return false;
        };
        current = parent;
    }
    target.starts_with(&root) && current.canonicalize().is_ok_and(|p| p.starts_with(&root))
}
pub async fn run(
    state: &AppState,
    turn_id: &str,
    model: &str,
    messages: &[Value],
    phase: &str,
) -> Result<String> {
    let memory = phase.starts_with("memory_");
    let judging = phase == turns::goals::JUDGE || memory;
    let judge_home = judging
        .then(process::ProbeHome::new)
        .transpose()
        .map_err(|_| Failure::storage())?;
    let (turn, profile, binding, account, login, device) = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        if memory {
            turns::memory::completion_agent(db, turn_id, phase)?;
        }
        let turn = one(
            db,
            "SELECT * FROM turns WHERE id=? AND (? OR status IN('queued','running') AND paused=0)",
            rusqlite::params![turn_id, memory],
        )?
        .ok_or_else(|| error(409, "Task has stopped."))?;
        let profile = agents::read(db, string(&turn, "agent"))?;
        let binding = if let Some(home) = &judge_home {
            crate::config::WorkspaceConfig {
                agent: string(&turn, "agent").into(),
                directory: home.0.clone(),
                write: false,
                managed: false,
            }
        } else {
            turns::harness::workspace(state, db, &turn)?
                .ok_or_else(|| error(403, "Coding harness requires a host workspace folder."))?
        };
        let provider = string(&profile, "provider_account");
        let (account, login, device) = if provider == data::logins::DEVICE {
            let runtime = string(&profile, "runtime");
            if !data::logins::device_runtime(runtime) {
                return Err(error(503, data::logins::DEVICE_RUNTIME_ONLY));
            }
            let folder = data::logins::device_folder(runtime).ok_or_else(|| {
                error(
                    503,
                    "This machine's engine folder could not be found. Sign in to the CLI on this machine.",
                )
            })?;
            (None, None, Some(folder))
        } else if provider.starts_with(data::logins::PREFIX) {
            let selected = data::logins::bound(db, provider)?;
            if let Some(reason) =
                data::logins::blocked_reason(string(&profile, "runtime"), selected.as_ref())
            {
                return Err(error(503, reason));
            }
            let selected = selected.ok_or_else(Failure::storage)?;
            let credentials = data::logins::credentials(state, &selected)?.ok_or_else(|| {
                error(
                    503,
                    "Engine login credentials are missing. Check sign-in in Settings > Engines.",
                )
            })?;
            (
                None,
                Some((string(&selected, "id").to_owned(), credentials)),
                None,
            )
        } else {
            let account = data::providers::accounts(state)?
                .into_iter()
                .find(|account| account.id == provider)
                .ok_or_else(|| {
                    error(
                        503,
                        "The selected provider account is not configured. Check Settings > Providers.",
                    )
                })?;
            (Some(account), None, None)
        };
        (turn, profile, binding, account, login, device)
    };
    let runtime = string(&profile, "runtime");
    let executable = super::executable(state, runtime);
    let mise = executable.as_deref().is_some_and(process::mise_shim);
    // Claude on the device sign-in runs with the real HOME; see logins::device_environment.
    let device_claude = device.is_some() && runtime == "claude";
    let isolated_home = (mise || ["codex", "claude"].contains(&runtime)) && !device_claude;
    let conversation = turn["channel"]
        .as_str()
        .or(turn["thread"].as_str())
        .unwrap_or("direct");
    let key = json!([
        turn["agent"],
        if memory {
            "memory"
        } else if judging {
            "goal-judge"
        } else {
            conversation
        },
        runtime,
        (!judging).then_some(&binding.directory)
    ])
    .to_string();
    let fingerprint = if let Some(account) = &account {
        json!([
            account.id,
            account.base,
            format!(
                "{:x}",
                Sha256::digest(account.key.as_deref().unwrap_or("").as_bytes())
            ),
            model
        ])
    } else if let Some((id, bytes)) = &login {
        json!([
            data::logins::PREFIX,
            id,
            format!("{:x}", Sha256::digest(bytes)),
            model
        ])
    } else if let Some(folder) = &device {
        json!([data::logins::DEVICE, folder, model])
    } else {
        return Err(Failure::storage());
    }
    .to_string();
    let mut env = state.config.harness.environment.clone();
    if !judging {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        env.extend(data::environment::values(
            state,
            &guard.connection,
            turn_id,
            string(&turn, "agent"),
        )?);
    }
    if device_claude {
        env.extend(
            data::logins::device_environment(runtime)
                .into_iter()
                .map(|(key, value)| (key.into(), value.to_string_lossy().into_owned())),
        );
    }
    if login.is_some() || device.is_some() {
        for variable in [
            "OPENAI_API_KEY",
            "CODEX_API_KEY",
            "CODEX_ACCESS_TOKEN",
            "OPENAI_BASE_URL",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
        ] {
            env.remove(variable);
        }
    }
    let same_home;
    {
        let mut homes = state.config.harness.homes.lock().await;
        same_home = homes
            .get(&key)
            .is_some_and(|h| h.fingerprint == fingerprint);
        if isolated_home {
            if !same_home {
                let root =
                    config::homes_root(&state.config.database).map_err(|_| Failure::storage())?;
                if std::iter::once(&binding)
                    .chain(&state.config.workspaces)
                    .any(|workspace| {
                        root.starts_with(&workspace.directory)
                            || workspace.directory.starts_with(&root)
                    })
                {
                    return Err(error(409, "Harness homes must be outside working folders."));
                }
                homes.remove(&key);
                homes.insert(
                    key.clone(),
                    home(
                        &root,
                        runtime,
                        account.as_ref(),
                        login
                            .as_ref()
                            .map(|(id, bytes)| (id.as_str(), bytes.as_slice())),
                        device.as_deref(),
                        model,
                        fingerprint,
                    )?,
                );
            }
            env.extend(
                homes
                    .get(&key)
                    .ok_or_else(Failure::storage)?
                    .environment
                    .clone(),
            );
        }
    }
    let saved = {
        let db = state.database.lock().map_err(|_| Failure::storage())?;
        if judging
            || exists(
                &db.connection,
                "SELECT 1 FROM dispatch_links WHERE target_turn=? AND mode='restart'",
                [turn_id],
            )?
        {
            None
        } else if !isolated_home || same_home {
            one(&db.connection,"SELECT session FROM harness_sessions WHERE agent=? AND conversation=? AND runtime=? AND workspace=?",rusqlite::params![turn["agent"].as_str(),conversation,runtime,binding.directory.to_str()])?.map(|v|string(&v,"session").to_owned())
        } else {
            None
        }
    };
    let call = {
        let db = state.database.lock().map_err(|_| Failure::storage())?;
        if memory {
            turns::memory::completion_agent(&db.connection, turn_id, phase)?;
        } else if db.connection.execute("UPDATE turns SET status='running',started_at=coalesce(started_at,?) WHERE id=? AND status IN('queued','running') AND paused=0",rusqlite::params![data::iso(&db.connection,state.config.now())?,turn_id])? == 0 {
            return Err(process::stopped());
        }
        db.connection.execute("INSERT INTO provider_calls(turn,agent,model,origin,purpose,phase,family,account,status,created) VALUES(?,?,?,'local-harness',?,? ,?,'','pending',?)",rusqlite::params![turn_id,turn["agent"].as_str(),if model.is_empty(){label(runtime)}else{model},if phase=="routing"{"routing"}else{"completion"},phase,runtime,data::iso(&db.connection,state.config.now())?])?;
        if !memory {
            state.events.turn(&db.connection, turn_id)?;
        }
        db.connection.last_insert_rowid()
    };
    let external = if !judging && state.config.harness.test_harness_bridge.is_some() {
        bridge(
            state,
            "mint",
            json!({"agent":turn["agent"],"turn":turn_id,"scopes":["orchestration"],"ttlMs":120000}),
        )
        .await?
    } else {
        Value::Null
    };
    let (id, token) = if judging {
        (String::new(), String::new())
    } else if external.is_null() {
        crate::mcp::mint_session(state, string(&turn, "agent"), turn_id)?
    } else {
        (
            string(&external, "id").to_owned(),
            string(&external, "token").to_owned(),
        )
    };
    if !judging && (id.is_empty() || token.is_empty() || token.len() > 4096) {
        return Err(error(
            502,
            "Minarbot MCP issuer returned an invalid session.",
        ));
    }
    let mut lease = (!judging).then(|| Lease {
        state: state.clone(),
        id,
        external: !external.is_null(),
    });
    if !judging {
        if runtime == "claude" {
            env.insert(
                "MCP_TOOL_TIMEOUT".into(),
                ((crate::files::components::FORM_WAIT_SECONDS + 10) * 1000).to_string(),
            );
        }
        env.insert("MINARBOT_MCP_TOKEN".into(), token.clone());
        env.insert("MINARBOT_MCP_URL".into(), crate::mcp::url(state));
    }
    if runtime == "hermes" {
        env.extend(process::HERMES_ENV.map(|(key, value)| (key.into(), value.into())));
    }
    if mise {
        process::configure_mise(&mut env);
    }
    let model = if (login.is_some() || device.is_some()) && model == "default" {
        ""
    } else {
        model
    };
    let mut secrets = vec![token.clone()];
    secrets.extend(account.and_then(|account| account.key));
    if let Some((_, bytes)) = login {
        secrets.extend(data::logins::secrets(runtime, &bytes));
    }
    if device.is_some() {
        if let Some(bytes) = data::logins::device_credentials(runtime) {
            secrets.extend(data::logins::secrets(runtime, &bytes));
        }
    }
    {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        secrets.extend(crate::mcp::secrets::project(
            state,
            &guard.connection,
            None,
        )?);
        secrets.extend(crate::computer::vault::redactions(
            state,
            &guard.connection,
        )?);
    }
    let operation = |clock| {
        execute(
            state,
            &turn,
            &profile,
            runtime,
            executable.as_deref(),
            model,
            messages,
            phase,
            call,
            &token,
            env,
            saved,
            &binding,
            secrets,
            clock,
        )
    };
    // Dropping execute flushes partial output and schedules the ACP cancel. The turn worker
    // awaits that cleanup before releasing its slot.
    let result = if memory {
        let ms = state.config.data.timeout.unwrap_or(90_000).min(90_000);
        tokio::time::timeout(Duration::from_millis(ms), operation(None))
            .await
            .unwrap_or_else(|_| Err(error(502, "Memory engine timed out.")))
    } else if let Some(ms) = state.config.data.timeout {
        tokio::time::timeout(Duration::from_millis(ms), operation(None)).await.map_err(|_|error(502,"Coding harness timed out. Check workspace changes before retrying; this turn was not replayed."))?
    } else {
        let active = config::ActiveClock::new(state, turn_id)?;
        tokio::select! {
            biased;
            failure = active.clock.expired() => return Err(failure),
            result = operation(Some(active.clock.clone())) => result,
        }
    };
    if memory && result.is_err() {
        let db = state.database.lock().map_err(|_| Failure::storage())?;
        db.connection.execute(
            "UPDATE provider_calls SET status='failed',finished=? WHERE seq=? AND status='pending'",
            rusqlite::params![data::iso(&db.connection, state.config.now())?, call],
        )?;
    }
    if let Some(lease) = lease.as_mut().filter(|lease| lease.external) {
        bridge(state, "revoke", json!({"id":lease.id})).await?;
        lease.external = false;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn idle_timeout_flushes_partial_reply_and_cancels_acp() -> crate::db::Result<()> {
        use std::sync::{Arc, Mutex};

        let scratch = process::ProbeHome::new()?;
        let executable = scratch.0.join("fake-acp");
        std::fs::write(
            &executable,
            r#"#!/bin/sh
read -r request
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}'
read -r request
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"fixture","modes":{"availableModes":[{"id":"default"}]}}}'
read -r request
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{}}'
read -r request
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fixture","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Still working."}}}}'
read -r cancel
printf '%s\n' "$cancel" > cancel.json
while read -r request; do :; done
"#,
        )?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
        let mut config: crate::config::Config = serde_json::from_value(json!({
            "database":scratch.0.join("state.sqlite"),
            "harness":{"executables":{"hermes":executable}},
            "workspaces":[{"agent":"agent","directory":scratch.0,"write":true}]
        }))?;
        config.data.turn_idle_timeout_ms = Some(1000);
        config.data.validate()?;
        let database = crate::db::open(&config.database, false, 0)?;
        crate::files::init(&database.connection)?;
        data::initialize(&database)?;
        turns::initialize(&database.connection).map_err(|failure| failure.message)?;
        crate::connectors::initialize(&database.connection).map_err(|failure| failure.message)?;
        crate::automation::initialize(&database.connection).map_err(|failure| failure.message)?;
        crate::harness::initialize(&database.connection).map_err(|failure| failure.message)?;
        crate::mcp::initialize(&database.connection).map_err(|failure| failure.message)?;
        crate::computer::initialize(&database.connection)?;
        database.connection.execute_batch(
            "INSERT INTO agents(id,name,model,instructions,runtime) VALUES('agent','Test','','','hermes');
             INSERT INTO turns(id,agent,text,status,created) VALUES('turn','agent','Wait','queued','');",
        )?;
        let state = AppState {
            computer: Arc::new(crate::computer::Manager::new(&config)?),
            config: Arc::new(config),
            database: Arc::new(Mutex::new(database)),
            origin: "http://127.0.0.1".into(),
            auth: None,
            events: crate::events::Hub::new(),
            turns: turns::Runtime::default(),
            whatsapp: Default::default(),
            trial_pending: Arc::default(),
        };
        let Err(failure) = run(&state, "turn", "", &[], "answer").await else {
            panic!("silent harness completed");
        };
        crate::harness::settle_process(&state, "turn").await;
        assert!(
            failure.message.starts_with("No activity for 1 s."),
            "{}",
            failure.message
        );
        let cancel: Value = serde_json::from_slice(&std::fs::read(scratch.0.join("cancel.json"))?)?;
        assert_eq!(
            cancel,
            json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"fixture"}})
        );
        let db = state
            .database
            .lock()
            .map_err(|_| "Database lock poisoned")?;
        let partial: String =
            db.connection
                .query_row("SELECT text FROM provider_streams", [], |row| row.get(0))?;
        assert_eq!(partial, "Still working.");
        Ok(())
    }

    #[tokio::test]
    async fn acp_abort_before_launch_spawns_nothing() -> crate::db::Result<()> {
        let db = rusqlite::Connection::open_in_memory()?;
        db.execute_batch(
            "CREATE TABLE turns(id TEXT PRIMARY KEY, status TEXT, paused INTEGER);
            INSERT INTO turns VALUES ('turn', 'running', 0);
            UPDATE turns SET status='cancelled' WHERE id='turn';",
        )?;
        let scratch = process::ProbeHome::new()?;
        let marker = scratch.0.join("launched");
        let result = launch_turn(&db, "turn", || {
            std::fs::File::create(&marker).map_err(|_| Failure::storage())?;
            Process::launch(Path::new("/bin/true"), &[], &scratch.0, &HashMap::new())
        });
        assert_eq!(result.err().map(|failure| failure.status), Some(409));
        assert!(!marker.exists());
        Ok(())
    }

    #[test]
    fn workspace_and_prefix_reject_escapes_and_shell_syntax() -> crate::db::Result<()> {
        let scratch = process::ProbeHome::new()?;
        let workspace = scratch.0.join("work");
        std::fs::create_dir(&workspace)?;
        std::os::unix::fs::symlink(&scratch.0, workspace.join("escape"))?;
        assert!(inside(&workspace, "new.txt"));
        assert!(!inside(&workspace, "escape/new.txt"));
        assert!(!inside(&workspace, "../outside"));
        assert_eq!(command_prefix("echo hello"), Some("echo hello"));
        assert_eq!(command_prefix("echo hello; curl attacker"), None);
        assert!(command_prefix(&"x".repeat(512)).is_some());
        assert_eq!(command_prefix(&"x".repeat(513)), None);
        Ok(())
    }

    #[test]
    fn claude_workspace_tools_and_host_approval_policy() -> crate::db::Result<()> {
        use ClaudeTool::{Allow, Approve, UseWorkspaceShell};
        let scratch = process::ProbeHome::new()?;
        let workspace = scratch.0.join("work");
        std::fs::create_dir(&workspace)?;
        std::os::unix::fs::symlink(&scratch.0, workspace.join("outside"))?;
        for (name, input, write, expected) in [
            ("Bash", json!({"command":"ls -al"}), true, UseWorkspaceShell),
            (
                "Bash",
                json!({"dangerouslyDisableSandbox":true}),
                true,
                Approve("command"),
            ),
            ("ToolSearch", json!({}), true, Allow),
            ("TodoWrite", json!({}), true, Allow),
            ("mcp__minarbot__run_shell", json!({}), true, Allow),
            (
                "mcp__minarbot__read_workspace_file",
                json!({}),
                false,
                Allow,
            ),
            ("Read", json!({"file_path":"index.js"}), false, Allow),
            ("Glob", json!({"pattern":"../*"}), true, Approve("file")),
            ("Grep", json!({"path":"outside"}), true, Approve("file")),
            ("Edit", json!({"file_path":"index.js"}), true, Allow),
            (
                "Write",
                json!({"file_path":"index.js"}),
                false,
                Approve("file"),
            ),
            (
                "Write",
                json!({"file_path":"../index.js"}),
                true,
                Approve("file"),
            ),
            ("WebFetch", json!({}), true, Approve("network")),
            ("WebSearch", json!({}), true, Approve("network")),
            ("mcp__mail__send", json!({}), true, Approve("tool")),
            ("mcp__store__purchase", json!({}), true, Approve("tool")),
            ("mcp__vault__get_secret", json!({}), true, Approve("tool")),
        ] {
            assert_eq!(
                claude_tool(name, &input, &workspace, write, true),
                expected,
                "{name}"
            );
        }
        assert_eq!(
            claude_tool("Bash", &json!({"command":"ls"}), &workspace, true, false),
            Approve("command")
        );
        Ok(())
    }

    #[test]
    fn codex_home_keeps_credentials_out_of_private_config() -> crate::db::Result<()> {
        let scratch = process::ProbeHome::new()?;
        let database = scratch.0.join("state.sqlite");
        let _lease = config::prepare_homes(&database)?;
        let root = config::homes_root(&database)?;
        let mut account = data::providers::Account {
            id: "router".into(),
            name: "Router".into(),
            family: "openai-compatible".into(),
            base: "http://127.0.0.1:20128/v1".into(),
            key: Some("synthetic-secret".into()),
            model: "cx/gpt-6.1-sol".into(),
            stream: false,
            termination: String::new(),
            max_tokens: 0,
            source: "environment",
        };
        let session = home(
            &root,
            "codex",
            Some(&account),
            None,
            None,
            &account.model,
            String::new(),
        )
        .map_err(|failure| failure.message)?;
        assert_eq!(session.path.parent(), Some(root.as_path()));
        let codex = Path::new(&session.environment["CODEX_HOME"]);
        let config = codex.join("config.toml");
        let text = std::fs::read_to_string(&config)?;
        assert!(text.contains("model = \"cx/gpt-6.1-sol\""));
        assert!(text.contains("base_url = \"http://127.0.0.1:20128/v1\""));
        assert!(text.contains("env_key = \"MINARBOT_PROVIDER_KEY\""));
        assert!(!text.contains("synthetic-secret"));
        assert_eq!(
            session.environment["MINARBOT_PROVIDER_KEY"],
            "synthetic-secret"
        );
        assert_eq!(
            std::fs::metadata(config)?.permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&session.path)?.permissions().mode() & 0o777,
            0o700
        );
        assert!(std::fs::read_to_string(codex.join("rules/default.rules"))?
            .contains("pattern=[\"rm\"], decision=\"prompt\""));
        let path = session.path.clone();
        drop(session);
        assert!(!path.exists());
        account.base = "https://user:password@example.test/v1".into();
        assert!(home(
            &root,
            "codex",
            Some(&account),
            None,
            None,
            "x",
            String::new()
        )
        .is_err());
        account.base = "https://example.test/v1".into();
        account.family = "anthropic".into();
        assert!(home(
            &root,
            "codex",
            Some(&account),
            None,
            None,
            "x",
            String::new()
        )
        .is_err());
        assert!(std::fs::read_dir(root)?.next().is_none());
        Ok(())
    }

    #[test]
    fn engine_login_home_has_only_profile_auth_and_generated_config() -> crate::db::Result<()> {
        let scratch = process::ProbeHome::new()?;
        let database = scratch.0.join("state.sqlite");
        let _lease = config::prepare_homes(&database)?;
        let root = config::homes_root(&database)?;
        for (runtime, variable, file, expected) in [
            (
                "codex",
                "CODEX_HOME",
                "auth.json",
                &["auth.json", "config.toml", "rules"][..],
            ),
            (
                "claude",
                "CLAUDE_CONFIG_DIR",
                ".credentials.json",
                &[".credentials.json"][..],
            ),
        ] {
            let bytes = b"synthetic-profile-credential";
            let session = home(
                &root,
                runtime,
                None,
                Some(("login", bytes)),
                None,
                "",
                String::new(),
            )
            .map_err(|failure| failure.message)?;
            let directory = Path::new(&session.environment[variable]);
            assert_eq!(directory.parent(), Some(session.path.as_path()));
            assert_eq!(session.environment["HOME"], session.path.to_string_lossy());
            let mut names = std::fs::read_dir(directory)?
                .map(|entry| entry.map(|value| value.file_name()))
                .collect::<std::io::Result<Vec<_>>>()?;
            names.sort_unstable();
            assert_eq!(names, expected);
            assert_eq!(std::fs::read(directory.join(file))?, bytes);
            assert_eq!(
                std::fs::metadata(directory.join(file))?
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert!(!session.environment.contains_key("MINARBOT_PROVIDER_KEY"));
            assert!(!session.environment.contains_key("ANTHROPIC_API_KEY"));
        }
        Ok(())
    }

    #[test]
    fn device_login_home_points_at_the_codex_folder_and_writes_nothing_there(
    ) -> crate::db::Result<()> {
        let scratch = process::ProbeHome::new()?;
        let database = scratch.0.join("state.sqlite");
        let _lease = config::prepare_homes(&database)?;
        let root = config::homes_root(&database)?;
        // Claude on the device sign-in skips the private home entirely.
        let folder = scratch.0.join("device-codex");
        std::fs::create_dir(&folder)?;
        let session = home(&root, "codex", None, None, Some(&folder), "", String::new())
            .map_err(|failure| failure.message)?;
        assert_eq!(session.environment["CODEX_HOME"], folder.to_string_lossy());
        assert_eq!(session.environment["HOME"], session.path.to_string_lossy());
        assert!(std::fs::read_dir(&folder)?.next().is_none());
        assert!(std::fs::read_dir(&session.path)?.next().is_none());
        assert!(!session.environment.contains_key("MINARBOT_PROVIDER_KEY"));
        assert!(!session.environment.contains_key("ANTHROPIC_API_KEY"));
        let path = session.path.clone();
        drop(session);
        assert!(!path.exists());
        assert!(folder.exists());
        Ok(())
    }

    #[test]
    fn codex_context_chunks_preserve_multibyte_text() -> Result<()> {
        let text = format!("Header.\n\n{}\n\nFinal paragraph.", "🧪".repeat(3000));
        let context = additional_context(&text);
        let entries = context.as_object().ok_or_else(Failure::storage)?;
        assert_eq!(entries["minarbot_001"]["value"], "Header.\n\n");
        let mut joined = String::new();
        for entry in entries.values() {
            assert_eq!(entry["kind"], "application");
            let chunk = entry["value"].as_str().ok_or_else(Failure::storage)?;
            assert!(chunk.len() <= 4000);
            joined.push_str(chunk);
        }
        assert_eq!(joined, text);
        assert_eq!(additional_context(""), json!({}));
        Ok(())
    }
}
