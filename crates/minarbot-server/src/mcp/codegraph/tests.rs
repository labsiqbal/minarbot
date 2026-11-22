use super::*;
use crate::{db, harness::process::ProbeHome};
use std::sync::Arc;

const AGENT: &str = "00112233-4455-4677-8899-aabbccddeeff";

fn state(scratch: &Path) -> db::Result<AppState> {
    let config: crate::Config = serde_json::from_value(json!({
        "database":scratch.join("state.sqlite")
    }))?;
    let database = db::open(&config.database, false, 0)?;
    crate::files::init(&database.connection)?;
    data::initialize(&database)?;
    turns::initialize(&database.connection).map_err(|e| e.message)?;
    database.connection.execute_batch(&format!(
        "INSERT INTO agents(id,name,model,instructions) VALUES('{AGENT}','Test','','');
         INSERT INTO threads(id,agent,name) VALUES('thread','{AGENT}','Test');
         INSERT INTO local_projects(id,name,description,created,updated,channel,codegraph_enabled) VALUES('project','Test','',0,0,'section',1);
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

#[test]
fn configuration_follows_project_access() -> db::Result<()> {
    let scratch = ProbeHome::new()?;
    let root = scratch.0.join("project");
    fs::create_dir(&root)?;
    let state = state(&scratch.0)?;
    let turn = json!({"id":"turn","agent":AGENT,"thread":"thread"});
    let db = state
        .database
        .lock()
        .map_err(|_| "Database lock poisoned")?;
    let db = &db.connection;
    let selected = || project(&state, db, &turn).map_err(|e| e.message);
    assert!(project(&state, db, &json!({"agent":AGENT}))
        .map_err(|e| e.message)?
        .is_none());
    assert!(configs(None).iter().all(Value::is_null));
    let project = selected()?;
    db.execute("UPDATE local_projects SET codegraph_enabled=0", [])?;
    assert!(selected()?.is_none());
    let Some(project) = project else {
        return Ok(());
    };
    assert!(!root.join(".codegraph").exists());
    fs::create_dir_all(&project.cache)?;
    let server = project.launch("stdio").map_err(|e| e.message)?;
    let [claude, codex, acp] = configs(Some(&server));
    assert_eq!(claude["type"], "stdio");
    assert_eq!(acp["name"], "codegraph");
    for config in [claude, codex, acp] {
        assert_eq!(config["command"], SHELL);
        let args = config["args"]
            .as_array()
            .ok_or("Missing CodeGraph arguments")?;
        assert!(args.ends_with(&[
            json!("codegraph"),
            json!("stdio"),
            json!(PROJECT),
            json!(DATABASE)
        ]));
        assert!(args
            .windows(3)
            .any(|part| part == [json!("--ro-bind-fd"), json!("4"), json!("/index")]));
    }
    db.execute("UPDATE local_projects SET codegraph_enabled=1", [])?;
    db.execute("DELETE FROM local_project_members", [])?;
    assert!(selected().is_err_and(|e| e.contains("not a project member")));
    Ok(())
}

#[test]
fn native_stdio_indexes_syncs_and_deduplicates_by_connection() -> db::Result<()> {
    let scratch = ProbeHome::new()?;
    let root = scratch.0.join("project");
    fs::create_dir(&root)?;
    let file = root.join("fixture.ts");
    fs::write(&file, "export function fixtureSymbol() { return 1; }\n")?;
    fs::write(
        root.join("view.tsx"),
        "export function FixtureView() { return <span>Members & folders</span>; }\n",
    )?;
    fs::write(
        scratch.0.join("outside.ts"),
        "export function outsideSymbol() {}\n",
    )?;
    std::os::unix::fs::symlink(scratch.0.join("outside.ts"), root.join("outside.ts"))?;
    let path = scratch.0.join("graph.db");
    let mut index = minarbot_codegraph::Index::open(&root, &path)?;
    let sync = index.sync()?;
    assert!(sync
        .issues
        .iter()
        .any(|issue| issue.code == "parse_recovered"));
    let request =
        |method, params: Value| json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
    let call = |name, args| request("tools/call", json!({"name":name,"arguments":args}));
    let run = |messages: &[Value]| -> db::Result<Vec<Value>> {
        let input = messages
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let reader = minarbot_codegraph::Index::open_read_only(&root, &path)?;
        let mut output = Vec::new();
        exchange(input.as_bytes(), &mut output, &reader)?;
        std::io::BufReader::new(output.as_slice())
            .lines()
            .map(|line| Ok(serde_json::from_str(&line?)?))
            .collect::<db::Result<_>>()
    };
    let explore = |symbol| call("codegraph_explore", json!({"query":symbol}));
    let responses = run(&[
        request("initialize", json!({"protocolVersion":"2025-03-26"})),
        request("tools/list", json!({})),
        call(
            "codegraph_explore",
            json!({"query":"fixtureSymbol","projectPath":scratch.0}),
        ),
        explore("fixtureSymbol"),
        explore("fixtureSymbol"),
        call("codegraph_node", json!({"file":"outside.ts"})),
    ])?;
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(
        responses[1]["result"]["tools"].as_array().map(Vec::len),
        Some(8)
    );
    assert_eq!(responses[2]["result"]["isError"], true);
    let text = |response: &Value| {
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };
    assert!(text(&responses[3]).contains("1\texport function fixtureSymbol() { return 1; }"));
    assert!(text(&responses[4]).contains("Previously shown source"));
    assert!(!text(&responses[4]).contains("export function"));
    assert!(text(&responses[5]).contains("No indexed file"));
    assert!(text(&run(&[explore("FixtureView")])?[0]).contains("function FixtureView"));
    assert!(text(&run(&[explore("fixtureSymbol")])?[0]).contains("return 1"));
    fs::write(&file, "export function updatedFixture() { return 2; }\n")?;
    index.sync()?;
    assert!(text(&run(&[explore("updatedFixture")])?[0]).contains("return 2"));
    Ok(())
}
