use super::*;
use crate::{db, harness::process::ProbeHome};
use std::{os::unix::fs::PermissionsExt, sync::Arc};

const AGENT: &str = "00112233-4455-4677-8899-aabbccddeeff";
const CLI: &str = r#"#!/bin/sh
set -eu
test "$CODEGRAPH_TELEMETRY" = 0
test "$DO_NOT_TRACK" = 1
test ! -e outside.ts
test -r fixture.ts
test ! -w fixture.ts
case "$1" in
  init)
    test "$2" = --yes
    printf 'init\n' > .codegraph/codegraph.db
    ;;
  sync)
    printf 'sync\n' >> .codegraph/codegraph.db
    ;;
  serve)
    while read -r request; do
      case "$request" in
        *'"method":"initialize"'*)
          printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}}}}'
          ;;
        *'"method":"tools/list"'*)
          printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"codegraph_explore","description":"Explore code","inputSchema":{"type":"object","properties":{"query":{"type":"string"},"projectPath":{"type":"string"}},"required":["query"]}}]}}'
          ;;
        *'"method":"tools/call"'*)
          printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"fixtureSymbol"}]}}'
          ;;
      esac
    done
    ;;
esac
"#;

fn state(scratch: &Path, cli: &Path) -> db::Result<AppState> {
    let config: crate::Config = serde_json::from_value(json!({
        "database":scratch.join("state.sqlite"),
        "harness":{"executables":{"codegraph":cli}}
    }))?;
    let database = db::open(&config.database, false, 0)?;
    crate::files::init(&database.connection)?;
    data::initialize(&database)?;
    turns::initialize(&database.connection).map_err(|e| e.message)?;
    database.connection.execute_batch(&format!(
        "INSERT INTO agents(id,name,model,instructions) VALUES('{AGENT}','Test','','');
         INSERT INTO threads(id,agent,name) VALUES('thread','{AGENT}','Test');
         INSERT INTO local_projects(id,name,description,created,updated,channel) VALUES('project','Test','',0,0,'section');
         INSERT INTO local_project_threads VALUES('thread','project','');
         INSERT INTO local_project_members VALUES('project','{AGENT}',0);
         INSERT INTO turns(id,agent,thread,text,status,created) VALUES('turn','{AGENT}','thread','Test','running','');",
    ))?;
    database.connection.execute(
        "INSERT INTO local_project_folders VALUES('project',?)",
        [scratch.join("project").to_string_lossy().as_ref()],
    )?;
    Ok(AppState {
        whatsapp: Default::default(),
        computer: Arc::new(crate::computer::Manager::new(&config)?),
        config: Arc::new(config),
        database: Arc::new(std::sync::Mutex::new(database)),
        origin: "http://127.0.0.1".into(),
        auth: None,
        events: crate::events::Hub::new(),
        turns: turns::Runtime::default(),
        trial_pending: Arc::default(),
    })
}

fn configs(server: Option<&Server>) -> [Value; 3] {
    let mut claude = json!({"mcpServers":{"minarbot":{"type":"stdio"}}});
    let mut codex = json!({"config":{"mcp_servers":{"minarbot":{}}}});
    let mut acp = json!({"mcpServers":[{"name":"minarbot","type":"http"}]});
    Server::configure(server, "claude", &mut claude);
    Server::configure(server, "codex", &mut codex);
    Server::configure(server, "gemini", &mut acp);
    [
        claude["mcpServers"]["codegraph"].clone(),
        codex["config"]["mcp_servers"]["codegraph"].clone(),
        acp["mcpServers"][1].clone(),
    ]
}

#[tokio::test]
async fn config_and_bridge_follow_project_access() -> db::Result<()> {
    let scratch = ProbeHome::new()?;
    let cli = scratch.0.join("codegraph");
    let root = scratch.0.join("project");
    fs::create_dir(&root)?;
    fs::write(
        root.join("fixture.ts"),
        "export function fixtureSymbol() {}\n",
    )?;
    fs::write(
        scratch.0.join("outside.ts"),
        "export function outsideSymbol() {}\n",
    )?;
    std::os::unix::fs::symlink(scratch.0.join("outside.ts"), root.join("outside.ts"))?;
    let state = state(&scratch.0, &cli)?;
    let turn = json!({"id":"turn","agent":AGENT,"thread":"thread"});
    let selected = || -> Result<Option<Project>> {
        let db = state.database.lock().map_err(|_| Failure::storage())?;
        project(&state, &db.connection, &turn)
    };
    let enable = || -> db::Result<()> {
        let db = state
            .database
            .lock()
            .map_err(|_| "Database lock poisoned")?;
        db.connection
            .execute("UPDATE local_projects SET codegraph_enabled=1", [])?;
        Ok(())
    };
    fs::write(&cli, CLI)?;
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o700))?;
    assert!(available(&state));
    assert!(selected().map_err(|e| e.message)?.is_none());
    enable()?;
    let hidden = scratch.0.join("missing-codegraph");
    fs::rename(&cli, &hidden)?;
    assert!(!available(&state));
    assert!(selected().map_err(|e| e.message)?.is_none());
    assert!(configs(None).iter().all(Value::is_null));
    fs::rename(hidden, &cli)?;
    {
        let db = state
            .database
            .lock()
            .map_err(|_| "Database lock poisoned")?;
        assert!(project(&state, &db.connection, &json!({"agent":AGENT}))
            .map_err(|e| e.message)?
            .is_none());
    }
    assert!(!root.join(INDEX).exists());
    let project = selected()
        .map_err(|e| e.message)?
        .ok_or("Project unavailable")?;
    // Configuration can be checked on hosts where user namespaces are unavailable.
    if !Command::new(BWRAP)
        .args([
            "--unshare-all",
            "--ro-bind",
            "/usr",
            "/usr",
            "--symlink",
            "usr/lib64",
            "/lib64",
            "/usr/bin/true",
        ])
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
    {
        return Ok(());
    }
    let server = project.prepare().await.map_err(|e| e.message)?;
    let [claude, codex, acp] = configs(Some(&server));
    assert_eq!(claude["type"], "stdio");
    assert_eq!(codex["command"], SHELL);
    assert_eq!(acp["name"], "codegraph");
    for config in [claude, codex, acp] {
        assert_eq!(config["command"], SHELL);
        assert!(config["args"]
            .as_array()
            .is_some_and(|args| args.ends_with(&[
                json!("serve"),
                json!("--mcp"),
                json!("--no-watch"),
                json!("--path"),
                json!(PROJECT),
            ])));
    }
    let index = root.join(INDEX).join("codegraph.db");
    assert_eq!(fs::read_to_string(&index)?, "init\n");
    assert!(!scratch.0.join(INDEX).exists());
    let mut bridge = Bridge::open(project).await.map_err(|e| e.message)?;
    assert_eq!(fs::read_to_string(&index)?, "init\nsync\n");
    let specs = bridge.specs().collect::<Vec<_>>();
    assert_eq!(specs[0]["function"]["name"], "codegraph_explore");
    assert!(specs[0]["function"]["parameters"]["properties"]
        .get("projectPath")
        .is_none());
    let call = |args: Value| json!({"id":"tool","function":{"name":"codegraph_explore","arguments":args.to_string()}});
    let denied = bridge
        .call(
            &state,
            &turn,
            &call(json!({"query":"outsideSymbol","projectPath":scratch.0})),
        )
        .await;
    assert!(denied.is_err_and(|e| e.status == 400));
    let result = bridge
        .call(&state, &turn, &call(json!({"query":"fixtureSymbol"})))
        .await
        .map_err(|e| e.message)?;
    assert_eq!(result["content"][0]["text"], "fixtureSymbol");
    {
        let db = state
            .database
            .lock()
            .map_err(|_| "Database lock poisoned")?;
        db.connection
            .execute("DELETE FROM local_project_members", [])?;
    }
    assert!(selected().is_err_and(|e| e.status == 403));
    let denied = bridge
        .call(&state, &turn, &call(json!({"query":"fixtureSymbol"})))
        .await;
    assert!(denied.is_err_and(|e| e.status == 403));
    Ok(())
}

#[tokio::test]
#[ignore = "requires installed CodeGraph and bubblewrap"]
async fn real_codegraph_indexes_and_syncs_a_tiny_project() -> db::Result<()> {
    let scratch = ProbeHome::new()?;
    let cli = std::env::split_paths(&std::env::var_os("PATH").ok_or("PATH is unset")?)
        .map(|dir| dir.join("codegraph"))
        .find(|cli| cli.is_file())
        .ok_or("Install CodeGraph on PATH")?;
    let directory = scratch.0.join("project");
    fs::create_dir(&directory)?;
    let source = directory.join("fixture.ts");
    fs::write(&source, "export function firstFixture() { return 1; }\n")?;
    for symbol in ["firstFixture", "updatedFixture"] {
        let mut bridge = Bridge::open(Project {
            cli: cli.canonicalize()?,
            directory: directory.clone(),
        })
        .await
        .map_err(|e| e.message)?;
        assert!(bridge
            .specs()
            .any(|spec| spec["function"]["name"] == "codegraph_explore"));
        let response = bridge
            .rpc(
                "tools/call",
                json!({"name":"codegraph_explore","arguments":{"query":symbol}}),
            )
            .await
            .map_err(|e| e.message)?;
        assert!(
            response.to_string().contains(&format!("function {symbol}")),
            "{response}"
        );
        fs::write(&source, "export function updatedFixture() { return 2; }\n")?;
    }
    Ok(())
}
