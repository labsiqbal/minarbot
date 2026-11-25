//! Agent creation: prompt setup turns, local role receipts and canonical
//! (hosted) creation authority (port of `app/agent-create.test.ts`).

use crate::support::{after_question, after_question_reply};
use minarbot_testkit::{
    model::{ModelRequest, Reply, ScriptedModel},
    prelude::*,
    Server,
};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Setup {
    mode: &'static str,
    calls: usize,
    edits: usize,
    continued: bool,
    violations: Vec<String>,
}

fn setup_reply(script: &Mutex<Setup>, request: &ModelRequest) -> Reply {
    let Ok(mut script) = script.lock() else {
        return Reply::status(500, json!({"error": "poisoned"}));
    };
    if after_question(request) {
        return after_question_reply();
    }
    script.calls += 1;
    let mut tools = request.tool_names();
    tools.sort();
    // After the tool budget is spent the gateway offers no tools at all.
    if !tools.is_empty() && tools != ["ask_user", "edit_profile"] {
        script
            .violations
            .push(format!("setup offered tools {tools:?}"));
    }
    // A saved profile is a successful `edit_profile` result. Match by tool
    // name: resumed transcripts carry server-assigned call ids.
    let edits: Vec<Value> = request
        .tool_calls()
        .into_iter()
        .filter(|call| call["function"]["name"] == "edit_profile")
        .map(|call| call["id"].clone())
        .collect();
    let saved = request.tool_results().iter().any(|message| {
        edits.contains(&message["tool_call_id"])
            && message["content"]
                .as_str()
                .and_then(|content| serde_json::from_str::<Value>(content).ok())
                .is_some_and(|content| content["revision"].is_number())
    });
    let system = request.system();
    let revision = system
        .split("Current profile revision: ")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|digits| digits.parse::<i64>().ok());
    let Some(revision) = revision else {
        script
            .violations
            .push("setup prompt has no profile revision".into());
        return Reply::status(500, json!({"error": "no revision"}));
    };
    if saved && script.mode == "hold" {
        script.continued = true;
        return Reply::hold();
    }
    if saved || script.mode == "no-save" || script.mode == "name-only" && !edits.is_empty() {
        return Reply::text("Profile setup checked.");
    }
    match script.mode {
        "escape" => Reply::tool_call(
            "escape",
            "create_text_file",
            json!({"name": "no.txt", "text": "Forbidden"}),
        ),
        "ask" if !request.mentions("Research") => Reply::tool_call(
            "question",
            "ask_user",
            json!({"question": "Which role?", "choices": ["Research", "Writing"]}),
        ),
        mode => {
            script.edits += 1;
            let mut profile =
                json!({"revision": revision, "name": "Research partner", "role": "Research agent"});
            if mode != "name-only" {
                profile["instructions"] = json!("Compare sources and cite evidence.");
            }
            Reply::tool_call("profile", "edit_profile", profile)
        }
    }
}

fn command() -> Value {
    json!({"runtime": "api", "id": new_id(), "name": "Agent baru", "model": "fixture",
        "instructions": "I need an evidence-focused assistant.", "setup": true})
}

async fn first_turn(app: &Server, agent: &Value, done: impl Fn(&Value) -> bool) -> Result<Value> {
    let path = format!(
        "/api/agents/{}/turns",
        agent["id"].as_str().unwrap_or_default()
    );
    Ok(app.wait_get(&path, |rows| done(&rows[0])).await?[0].clone())
}

fn listed(agents: &Value, id: &Value) -> bool {
    agents["agents"]
        .as_array()
        .is_some_and(|rows| rows.iter().any(|row| row["id"] == *id))
}

#[tokio::test]
async fn prompt_creation_atomically_queues_one_restricted_setup_and_recovers_without_duplicate_work(
) -> Result<()> {
    let script = Arc::new(Mutex::new(Setup {
        mode: "normal",
        ..Setup::default()
    }));
    let model = ScriptedModel::start().await?;
    let responder = script.clone();
    model.respond(move |request| setup_reply(&responder, request));
    let set_mode = |mode: &'static str| -> Result<()> {
        script.lock().map_err(|_| "poisoned")?.mode = mode;
        Ok(())
    };
    let read = |field: fn(&Setup) -> usize| -> Result<usize> {
        Ok(field(&*script.lock().map_err(|_| "poisoned")?))
    };
    let root = TestRoot::new()?;
    let config = ServerConfig::new().model(&model);
    let mut app = root.start(config.clone()).await?;
    let input = command();
    let turns = |agent: &Value| {
        format!(
            "/api/agents/{}/turns",
            agent["id"].as_str().unwrap_or_default()
        )
    };
    app.post_expect("/api/agents", input.clone(), 201).await?;
    first_turn(&app, &input, |row| row["status"] == "done").await?;
    let before = read(|s| s.calls)?;
    let replay = app.post_expect("/api/agents", input.clone(), 200).await?;
    ensure!(
        replay["name"] == "Research partner" && replay["role"] == "Research agent",
        "setup result: {replay}"
    );
    ensure!(
        read(|s| s.calls)? == before,
        "creation replay ran setup again"
    );
    let rows = app.get(&turns(&input)).await?;
    ensure!(
        rows.as_array().map(Vec::len) == Some(1),
        "setup turns: {rows}"
    );
    ensure!(
        rows[0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("I need an evidence-focused assistant.")),
        "setup prompt lost the request: {}",
        rows[0]
    );
    let mut changed = input.clone();
    changed["setup"] = json!(false);
    app.post_expect("/api/agents", changed, 409).await?;
    let mut changed = input.clone();
    changed["instructions"] = json!("Different");
    app.post_expect("/api/agents", changed, 409).await?;
    let mut manual = command();
    manual["setup"] = json!(false);
    app.post_expect("/api/agents", manual.clone(), 201).await?;
    ensure!(
        app.get(&turns(&manual)).await? == json!([]),
        "manual creation queued setup"
    );
    let mut malformed = command();
    malformed["setup"] = json!("yes");
    app.post_expect("/api/agents", malformed, 400).await?;

    set_mode("escape")?;
    let escaped = command();
    app.post_expect("/api/agents", escaped.clone(), 201).await?;
    let failed = first_turn(&app, &escaped, |row| row["status"] == "failed").await?;
    ensure!(
        failed["generatedFiles"] == json!([]),
        "restricted setup wrote a file: {failed}"
    );
    set_mode("name-only")?;
    let partial = command();
    app.post_expect("/api/agents", partial.clone(), 201).await?;
    let result = first_turn(&app, &partial, |row| {
        matches!(row["status"].as_str(), Some("done" | "failed"))
    })
    .await?;
    ensure!(result["status"] == "failed", "name-only setup finished");
    ensure!(
        app.post_expect("/api/agents", partial.clone(), 200).await?["revision"] == 1,
        "name-only setup changed profile"
    );
    set_mode("no-save")?;
    let empty = command();
    app.post_expect("/api/agents", empty.clone(), 201).await?;
    first_turn(&app, &empty, |row| row["status"] == "failed").await?;
    set_mode("ask")?;
    let question = command();
    app.post_expect("/api/agents", question.clone(), 201)
        .await?;
    let waiting = first_turn(&app, &question, |row| {
        row["paused"] == 1
            && row["questions"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty())
    })
    .await?;
    let qid = question["id"].as_str().ok_or("id")?;
    app.post(
        &format!(
            "/api/agents/{qid}/turns/{qid}/questions/{}/answer",
            waiting["questions"][0]["id"].as_str().ok_or("question")?
        ),
        json!({"id": new_id(), "text": "Research"}),
    )
    .await?;
    first_turn(&app, &question, |row| row["status"] == "done").await?;

    set_mode("hold")?;
    let held = command();
    app.post_expect("/api/agents", held.clone(), 201).await?;
    until("setup continues after saving", || async {
        Ok(script
            .lock()
            .map_err(|_| "poisoned")?
            .continued
            .then_some(()))
    })
    .await?;
    app.restart().await?;
    let count = read(|s| s.edits)?;
    set_mode("normal")?;
    let stopped = first_turn(&app, &held, |row| row["status"] == "interrupted").await?;
    app.post_expect("/api/agents", held.clone(), 200).await?;
    ensure!(
        read(|s| s.edits)? == count,
        "creation replay repeated the profile edit"
    );
    let hid = held["id"].as_str().ok_or("id")?;
    app.post(
        &format!("/api/agents/{hid}/turns/{hid}/resume"),
        json!({"id": new_id(), "revision": stopped["revision"], "acknowledgeUncertain": true}),
    )
    .await?;
    first_turn(&app, &held, |row| row["status"] == "done").await?;
    ensure!(
        read(|s| s.edits)? == count,
        "resume repeated the saved profile edit"
    );

    app.shutdown().await?;
    minarbot_testkit::db::Fixture::open(app.database())?
        .execute_batch("CREATE TRIGGER reject_setup BEFORE INSERT ON turns BEGIN SELECT RAISE(ABORT, 'synthetic setup fault'); END")?;
    app.start_stopped(config).await?;
    let rollback = command();
    app.post_expect("/api/agents", rollback.clone(), 500)
        .await?;
    ensure!(
        !listed(&app.get("/api/agents").await?, &rollback["id"]),
        "failed setup kept the agent"
    );
    let violations = script.lock().map_err(|_| "poisoned")?.violations.clone();
    ensure!(
        violations.is_empty(),
        "setup model contract: {violations:?}"
    );
    Ok(())
}

#[tokio::test]
async fn setup_creation_requires_a_ready_provider() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let offline = command();
    app.post_expect("/api/agents", offline.clone(), 503).await?;
    ensure!(
        !listed(&app.get("/api/agents").await?, &offline["id"]),
        "offline setup kept the agent"
    );
    Ok(())
}

#[tokio::test]
async fn creation_replay_survives_provider_removal() -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|request| {
        if request.called("edit_profile") {
            Reply::text("Profile setup checked.")
        } else {
            Reply::tool("edit_profile", json!({"revision": 1, "name": "Research partner", "instructions": "Cite evidence."}))
        }
    });
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new().model(&model)).await?;
    let implicit = command();
    let mut explicit = command();
    explicit["providerInstance"] = json!("codex-test-gateway");
    for input in [&implicit, &explicit] {
        app.post_expect("/api/agents", input.clone(), 201).await?;
        first_turn(&app, input, |row| row["status"] == "done").await?;
    }
    app.restart_with(ServerConfig::new()).await?;
    app.post(
        "/api/provider-instances/codex-test-gateway/delete",
        json!({"id": new_id(), "instanceId": "codex-test-gateway"}),
    )
    .await?;
    let calls = model.calls();
    for input in [implicit, explicit] {
        let replay = app.post_expect("/api/agents", input.clone(), 200).await?;
        ensure!(
            replay["id"] == input["id"] && replay["name"] == "Research partner",
            "wrong creation replay: {replay}"
        );
        let mut changed = input.clone();
        changed["instructions"] = json!("Different");
        app.post_expect("/api/agents", changed, 409).await?;
    }
    ensure!(model.calls() == calls, "creation replay ran setup");
    Ok(())
}

#[tokio::test]
async fn local_role_create_edit_receipts_preserve_normalized_profile_omissions_and_revision_conflicts(
) -> Result<()> {
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new()).await?;
    let input = json!({"id": new_id(), "name": "Odd name", "role": "  Orchestration agent  ", "model": "fixture", "instructions": "Coordinate."});
    let mut create = input.clone();
    create["runtime"] = json!("api");
    let created = app.post_expect("/api/agents", create.clone(), 201).await?;
    ensure!(
        created["role"] == "Orchestration agent",
        "normalized role: {created}"
    );
    ensure!(
        app.post_expect("/api/agents", create.clone(), 200).await? == created,
        "create replay differs"
    );
    let mut changed = input.clone();
    changed["role"] = json!("Personal agent");
    app.post_expect("/api/agents", changed, 409).await?;
    for role in [
        json!(null),
        json!(1),
        json!("x".repeat(61)),
        json!("bad\nrole"),
        json!("bad\u{0085}role"),
        json!("bad\u{2028}role"),
    ] {
        let mut invalid = input.clone();
        invalid["id"] = json!(new_id());
        invalid["role"] = role.clone();
        app.post_expect("/api/agents", invalid, 400)
            .await
            .map_err(|error| format!("role {role}: {error}"))?;
    }
    let path = format!("/api/agents/{}/edit", created["id"].as_str().ok_or("id")?);
    let edit = json!({"id": new_id(), "revision": 1, "name": created["name"], "role": "x".repeat(60), "model": created["model"], "instructions": created["instructions"]});
    let updated = app.post_expect(&path, edit.clone(), 200).await?;
    ensure!(
        updated["role"] == edit["role"] && updated["revision"] == 2,
        "edit: {updated}"
    );
    ensure!(
        app.post_expect(&path, edit.clone(), 200).await? == updated,
        "edit replay differs"
    );
    let mut changed = edit.clone();
    changed["role"] = json!("Different");
    app.post_expect(&path, changed, 409).await?;
    let mut changed = edit.clone();
    changed["id"] = json!(new_id());
    app.post_expect(&path, changed, 409).await?;
    for role in [
        json!(null),
        json!(1),
        json!("x".repeat(61)),
        json!("bad\rrole"),
    ] {
        let mut invalid = edit.clone();
        invalid["id"] = json!(new_id());
        invalid["revision"] = json!(2);
        invalid["role"] = role.clone();
        app.post_expect(&path, invalid, 400)
            .await
            .map_err(|error| format!("role {role}: {error}"))?;
    }
    let mut without_role = edit.clone();
    if let Some(fields) = without_role.as_object_mut() {
        fields.remove("role");
    }
    without_role["id"] = json!(new_id());
    without_role["revision"] = json!(2);
    ensure!(
        app.post_expect(&path, without_role, 200).await?["role"] == edit["role"],
        "omitted role was not preserved"
    );
    let mut cleared = edit.clone();
    cleared["id"] = json!(new_id());
    cleared["revision"] = json!(3);
    cleared["role"] = json!("   ");
    ensure!(
        app.post_expect(&path, cleared, 200).await?["role"] == "",
        "blank role was not cleared"
    );
    app.restart().await?;
    ensure!(
        app.post_expect(&path, edit, 200).await? == updated,
        "edit receipt lost on restart"
    );
    ensure!(
        app.get("/api/agents").await?["agents"][0]["role"] == "",
        "stored role after restart"
    );
    ensure!(
        app.post_expect("/api/agents", create, 200).await?["role"] == "",
        "create replay shows current role"
    );
    Ok(())
}

// Canonical (hosted) creation authority.

use minarbot_testkit::{
    canonical::{Account, Canonical, CanonicalReply},
    protocol::{digest_command, sha256_hex},
};

/// Tools every new canonical participation receives (`retainedToolNames`).
const RETAINED_TOOLS: [&str; 43] = [
    "ask_user",
    "call_mcp_tool",
    "cancel_task",
    "copy_skill_file",
    "create_text_file",
    "create_workspace_directory",
    "create_workspace_file",
    "delegate_task",
    "delete_site",
    "edit_profile",
    "generate_image",
    "inspect_workspace_file",
    "interrupt_agent",
    "list_agents",
    "list_data_tables",
    "list_mcp_tools",
    "list_routines",
    "list_sites",
    "list_skills",
    "list_workspace",
    "no_update",
    "open_file_in_panel",
    "publish_site",
    "query_data",
    "read_attachment",
    "read_history",
    "read_message",
    "read_provider_settings",
    "read_routine",
    "read_skill",
    "read_skill_file",
    "read_workspace_file",
    "remember_memory",
    "report_failure",
    "search_memory",
    "select_memory",
    "send_message",
    "set_avatar",
    "set_routine_flow",
    "task_status",
    "transfer_task",
    "wait_for_event",
    "wait_task",
];

struct Hosted {
    c: Canonical,
    member: Account,
    foreign: Account,
    home: String,
    project: String,
}

impl Hosted {
    async fn start() -> Result<Self> {
        let mut c = Canonical::new()?;
        let member = c.add_account("user_member")?;
        let foreign = c.add_account("user_foreign")?;
        let owner = c.owner.clone();
        let home = enroll(&c, &owner, &["execution"])?;
        let project = new_id();
        c.db.execute(
            "INSERT INTO projects(id,account,name,private) VALUES (?,?,'A',0)",
            [&project, &owner.account_id],
        )?;
        c.db.execute(
            "INSERT INTO project_members VALUES (?,?,'owner',1,0),(?,?,'member',1,0)",
            [&project, &owner.account_id, &project, &member.account_id],
        )?;
        c.start(ServerConfig::new()).await?;
        Ok(Self {
            c,
            member,
            foreign,
            home,
            project,
        })
    }

    fn command(&self, change: Value) -> Value {
        let id = new_id();
        let mut command = json!({"requestId": id, "id": id, "name": "Agent", "model": "fixture", "backupModel": "",
            "instructions": "Help with research", "threadLimit": 2, "projectId": self.project, "homeDeviceId": self.home,
            "expectedProjectRevision": 1, "expectedHomeDeviceRevision": 1, "providerBinding": null, "setup": false});
        if let (Some(command), Value::Object(change)) = (command.as_object_mut(), change) {
            command.extend(change);
        }
        command
    }

    async fn create(&self, actor: &Account, body: &Value) -> Result<CanonicalReply> {
        self.c.post("/api/agents", body.clone(), actor).await
    }

    fn agent_path(&self, project: &str, agent: &str) -> String {
        format!("/api/projects/{project}/agents/{agent}")
    }

    async fn require_execution(
        &self,
        actor: &Account,
        project: &str,
        agent: &str,
    ) -> Result<CanonicalReply> {
        let request_id = new_id();
        let payload = json!({"v": 1, "kind": "direct", "message": {"text": "Check", "files": []}});
        let expected = json!({"project": 1, "agent": 1, "homeBinding": 1, "membership": 1, "assignment": 1, "capability": 4});
        let digest = digest_command(&json!([
            "minarbot-task-payload-v1",
            request_id,
            project,
            agent,
            self.home,
            payload,
            expected
        ]));
        self.c
            .post(
                "/api/tasks",
                json!({"requestId": request_id, "projectId": project, "agentId": agent, "homeDeviceId": self.home,
                    "payload": payload, "expectedRevisions": expected, "payloadDigest": digest}),
                actor,
            )
            .await
    }

    fn count(&self, sql: &str, params: impl rusqlite_params::Params) -> Result<i64> {
        self.c.db.scalar(sql, params)
    }
}

/// Re-export so helper signatures can name SQLite parameters.
mod rusqlite_params {
    pub use minarbot_testkit::db::Params;
}

fn enroll(c: &Canonical, actor: &Account, capabilities: &[&str]) -> Result<String> {
    let installation = new_id();
    let device = new_id();
    c.db.execute(
        "INSERT INTO installations VALUES (?,?,'fixture',?,1,0,1,0)",
        [&installation, &actor.account_id, &installation],
    )?;
    c.db.execute(
        "INSERT INTO devices VALUES (?,?,?,'fixture','fixture',?,?,'{}',1,0,'offline','Asia/Jakarta',1,NULL,?,1)",
        [&device, &installation, &actor.account_id, &device, &device, &json!(capabilities).to_string()],
    )?;
    Ok(device)
}

#[tokio::test]
async fn canonical_creation_is_owner_home_scoped_offline_safe_exact_and_fully_atomic() -> Result<()>
{
    let f = Hosted::start().await?;
    let owner = f.c.owner.clone();
    f.create(&f.member, &f.command(json!({})))
        .await?
        .error("owner_required")?;
    f.create(&f.foreign, &f.command(json!({})))
        .await?
        .error("resource_not_found")?;
    f.create(&owner, &f.command(json!({"expectedProjectRevision": 2})))
        .await?
        .error("stale_revision")?;
    f.create(&owner, &f.command(json!({"expectedHomeDeviceRevision": 2})))
        .await?
        .error("stale_revision")?;
    let foreign_home = enroll(&f.c, &f.foreign, &["execution"])?;
    f.create(&owner, &f.command(json!({"homeDeviceId": foreign_home})))
        .await?
        .error("resource_not_found")?;
    let bare_home = enroll(&f.c, &owner, &[])?;
    f.create(&owner, &f.command(json!({"homeDeviceId": bare_home})))
        .await?
        .error("capability_missing")?;
    f.create(&owner, &f.command(json!({"setup": true})))
        .await?
        .error("setup_unavailable")?;
    f.c.db
        .execute("UPDATE devices SET status='blocked' WHERE id=?", [&f.home])?;
    f.create(&owner, &f.command(json!({})))
        .await?
        .error("resource_not_found")?;
    f.c.db
        .execute("UPDATE devices SET status='offline' WHERE id=?", [&f.home])?;
    let command = f.command(json!({"role": " Orchestration agent ", "toolBudget": 30}));
    let agent = command["id"].as_str().ok_or("id")?.to_owned();
    let result = f.create(&owner, &command).await?.ok()?;
    ensure!(
        f.c.db
            .scalar::<String>("SELECT role FROM agents WHERE id=?", [&agent])?
            == "Orchestration agent",
        "stored role"
    );
    ensure!(
        f.count("SELECT tool_budget FROM agents WHERE id=?", [&agent])? == 30,
        "stored tool budget"
    );
    let mut conflict = command.clone();
    conflict["role"] = json!("Personal agent");
    f.create(&owner, &conflict)
        .await?
        .error("payload_conflict")?;
    f.create(&owner, &f.command(json!({"role": "x".repeat(61)})))
        .await?
        .error("invalid_agent")?;
    ensure!(
        result.status == 201 && result.data()["homeDeviceId"] == json!(f.home),
        "creation: {:?}",
        result.body
    );
    ensure!(
        f.count("SELECT count(*) FROM turns", [])? == 0,
        "creation queued a turn"
    );
    let capabilities: Value = serde_json::from_str(&f.c.db.scalar::<String>(
        "SELECT capabilities_json FROM agent_projects WHERE agent=?",
        [&agent],
    )?)?;
    ensure!(
        capabilities == json!({"v": 1, "tools": RETAINED_TOOLS.to_vec(), "resources": []}),
        "capabilities: {capabilities}"
    );
    ensure!(
        f.c.db
            .scalar::<String>("SELECT rules FROM agent_projects WHERE agent=?", [&agent])?
            .is_empty(),
        "default rules"
    );
    ensure!(
        f.count("SELECT count(*) FROM collaborator_agents", [])? == 0,
        "collaborators created"
    );
    f.c.db.execute(
        "UPDATE devices SET status='revoked',revision=revision+1 WHERE id=?",
        [&f.home],
    )?;
    ensure!(
        f.create(&owner, &command).await?.ok()?.data() == result.data(),
        "replay after home revocation"
    );
    let mut changed = command.clone();
    changed["instructions"] = json!("Changed");
    f.create(&owner, &changed)
        .await?
        .error("payload_conflict")?;
    let other_home = enroll(&f.c, &owner, &["execution"])?;
    let moved = f.c.db.execute(
        "UPDATE agent_authority SET home_device=? WHERE agent=?",
        [&other_home, &agent],
    );
    ensure!(
        moved
            .as_ref()
            .is_err_and(|error| error.to_string().contains("agent_home_immutable")),
        "home device changed: {moved:?}"
    );
    f.c.db.execute(
        "UPDATE devices SET status='offline',revision=1 WHERE id=?",
        [&f.home],
    )?;
    for table in [
        "agents",
        "agent_authority",
        "agent_projects",
        "record_scopes",
        "agent_creations",
        "receipts",
    ] {
        let failed = f.command(json!({}));
        let failed_id = failed["id"].as_str().ok_or("id")?.to_owned();
        f.c.db.execute_batch(&format!(
            "CREATE TRIGGER reject_insert BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT,'synthetic insertion fault'); END"
        ))?;
        f.create(&owner, &failed).await?.error("storage_failed")?;
        f.c.db.execute_batch("DROP TRIGGER reject_insert")?;
        for (table, key) in [
            ("agents", "id"),
            ("agent_authority", "agent"),
            ("agent_projects", "agent"),
            ("agent_creations", "id"),
            ("receipts", "request_id"),
        ] {
            ensure!(
                f.count(
                    &format!("SELECT count(*) FROM {table} WHERE {key}=?"),
                    [&failed_id]
                )? == 0,
                "partial {table} row after fault"
            );
        }
    }
    ensure!(
        f.count("SELECT count(*) FROM agents", [])? == 1,
        "agent count"
    );
    ensure!(
        f.count("SELECT revision FROM projects WHERE id=?", [&f.project])? == 1,
        "project revision changed"
    );
    Ok(())
}

#[tokio::test]
async fn provider_binding_stays_explicit_and_avatar_receipt_hashes_original_bytes() -> Result<()> {
    let f = Hosted::start().await?;
    let owner = f.c.owner.clone();
    let binding = new_id();
    let other_home = enroll(&f.c, &owner, &["execution"])?;
    f.c.db.execute(
        "INSERT INTO provider_bindings VALUES (?, ?, ?, 'openai-compatible', 'owner-provider', 'Owner', 'missing', 1)",
        [&binding, &other_home, &owner.account_id],
    )?;
    let command =
        f.command(json!({"providerBinding": {"bindingId": binding, "expectedRevision": 1}}));
    f.create(&owner, &command)
        .await?
        .error("resource_not_found")?;
    f.c.db.execute(
        "UPDATE provider_bindings SET device=? WHERE id=?",
        [&f.home, &binding],
    )?;
    let mut stale = command.clone();
    stale["providerBinding"]["expectedRevision"] = json!(2);
    f.create(&owner, &stale).await?.error("stale_revision")?;
    let original = minarbot_testkit::image::png(12, 20, [0xc5, 0x3e, 0x20, 0xff])?;
    let encoded = {
        use base64_engine::Engine;
        base64_engine::STANDARD.encode(&original)
    };
    let mut with_avatar = command.clone();
    with_avatar["avatar"] = json!(encoded);
    let result = f.create(&owner, &with_avatar).await?.ok()?;
    // Since #87 the binding is recorded on the agent's authority row instead
    // of copying the binding's provider account onto the agent.
    ensure!(
        f.c.db.scalar::<String>(
            "SELECT provider_binding FROM agent_authority WHERE agent=?",
            [command["id"].as_str().ok_or("id")?]
        )? == binding,
        "explicit provider binding not recorded"
    );
    let image = f.c.db.blob("SELECT image FROM agent_avatars", [])?;
    ensure!(
        minarbot_testkit::image::dimensions(&image)? == (256, 256),
        "avatar size"
    );
    ensure!(
        f.count("SELECT revision FROM agent_avatars", [])? == 1,
        "avatar revision"
    );
    let mut digested = command.clone();
    digested["avatar"] = json!(sha256_hex(&original));
    let expected = digest_command(&json!(["minarbot-agent-create-v1", digested]));
    ensure!(
        result.body["receipt"]["payloadDigest"] == json!(expected),
        "receipt digest: {}",
        result.body
    );
    ensure!(
        !f.c.db
            .scalar::<String>("SELECT request FROM agent_creations", [])?
            .contains(&encoded),
        "creation receipt stored the avatar bytes"
    );
    f.c.db.execute(
        "UPDATE provider_bindings SET readiness='revoked',revision=2",
        [],
    )?;
    ensure!(
        f.create(&owner, &with_avatar).await?.ok()?.status == 200,
        "replay after binding revocation"
    );
    let mut invalid = f.command(json!({}));
    invalid["avatar"] = json!("iVBORw0KGgo=");
    f.create(&owner, &invalid).await?.error("invalid_avatar")?;
    Ok(())
}

mod base64_engine {
    pub use minarbot_testkit::base64::{engine::general_purpose::STANDARD, Engine};
}

#[tokio::test]
async fn canonical_setup_admission_rolls_back_every_row_and_requires_a_ready_explicit_home_binding(
) -> Result<()> {
    let f = Hosted::start().await?;
    let owner = f.c.owner.clone();
    let binding = new_id();
    f.c.db.execute(
        "INSERT INTO provider_bindings VALUES (?,?,?,'openai-compatible','setup','Setup','missing',1)",
        [&binding, &f.home, &owner.account_id],
    )?;
    let command = || {
        f.command(json!({"setup": true, "providerBinding": {"bindingId": binding, "expectedRevision": 1}}))
    };
    for readiness in ["missing", "unreadable", "unavailable"] {
        f.c.db.execute(
            "UPDATE provider_bindings SET readiness=? WHERE id=?",
            [readiness, &binding],
        )?;
        f.create(&owner, &command())
            .await?
            .error("provider_unavailable")?;
        ensure!(
            f.count("SELECT count(*) FROM agents", [])? == 0,
            "{readiness} binding stored an agent"
        );
    }
    f.c.db.execute(
        "UPDATE provider_bindings SET readiness='ready' WHERE id=?",
        [&binding],
    )?;
    let tables = [
        "agents",
        "agent_authority",
        "agent_projects",
        "agent_creations",
        "turns",
        "tasks",
        "receipts",
        "record_scopes",
    ];
    let counts = || -> Result<Vec<i64>> {
        tables
            .iter()
            .map(|table| f.count(&format!("SELECT count(*) FROM {table}"), []))
            .collect()
    };
    let before = counts()?;
    for table in ["turns", "tasks", "receipts"] {
        f.c.db.execute_batch(&format!(
            "CREATE TRIGGER reject_setup BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT,'setup rollback'); END"
        ))?;
        f.create(&owner, &command())
            .await?
            .error("storage_failed")?;
        f.c.db.execute_batch("DROP TRIGGER reject_setup")?;
        ensure!(counts()? == before, "{table} fault left rows behind");
    }
    f.c.db.execute(
        "UPDATE project_members SET grant_revision=3 WHERE project=? AND account=?",
        [&f.project, &owner.account_id],
    )?;
    let input = command();
    let id = input["id"].as_str().ok_or("id")?.to_owned();
    let created = f.create(&owner, &input).await?.ok()?;
    let task = f.c.db.row("SELECT * FROM tasks WHERE id=?", [&id])?;
    ensure!(
        created.status == 201,
        "setup creation status {}",
        created.status
    );
    ensure!(
        task["state"] == "waiting_device" && task["assignment_revision"] == 3,
        "task: {task}"
    );
    ensure!(
        task["provider_binding_id"] == json!(binding),
        "task binding: {task}"
    );
    ensure!(
        f.count("SELECT profile_setup FROM turns WHERE id=?", [&id])? == 1,
        "setup turn flag"
    );
    f.c.db.execute(
        "UPDATE provider_bindings SET readiness='unavailable',revision=2 WHERE id=?",
        [&binding],
    )?;
    f.c.db.execute(
        "UPDATE devices SET status='revoked',revision=2 WHERE id=?",
        [&f.home],
    )?;
    ensure!(
        f.create(&owner, &input).await?.ok()?.data() == created.data(),
        "setup replay differs"
    );
    ensure!(
        f.count("SELECT count(*) FROM tasks", [])? == 1,
        "setup replay queued another task"
    );
    Ok(())
}

#[tokio::test]
async fn second_project_participation_and_shared_cas_isolate_rules_capabilities_and_stale_grants(
) -> Result<()> {
    let f = Hosted::start().await?;
    let owner = f.c.owner.clone();
    let command = f.command(json!({}));
    let agent = command["id"].as_str().ok_or("id")?.to_owned();
    f.create(&owner, &command).await?.ok()?;
    let second = new_id();
    f.c.post(
        "/api/projects",
        json!({"requestId": new_id(), "id": second, "name": "B"}),
        &owner,
    )
    .await?
    .ok()?;
    let path = f.agent_path(&second, &agent);
    let participation = json!({"requestId": new_id(), "expectedProjectRevision": 1, "expectedAgentRevision": 1,
        "expectedAuthorityRevision": 1, "expectedParticipationRevision": 0, "rules": "Only B",
        "capabilities": {"v": 1, "tools": ["read_history"], "resources": []}});
    ensure!(
        f.c.put(&path, participation.clone(), &owner)
            .await?
            .ok()?
            .status
            == 201,
        "participation status"
    );
    ensure!(
        f.c.put(&path, participation.clone(), &owner)
            .await?
            .ok()?
            .status
            == 200,
        "participation replay status"
    );
    let mut duplicate = participation.clone();
    duplicate["requestId"] = json!(new_id());
    f.c.put(&path, duplicate, &owner)
        .await?
        .error("participation_exists")?;
    f.c.put(&path, participation.clone(), &f.foreign)
        .await?
        .error("resource_not_found")?;
    f.require_execution(&f.member, &second, &agent)
        .await?
        .error("resource_not_found")?;
    let rules = json!({"requestId": new_id(), "expectedParticipationRevision": 1, "rules": "B only, revised"});
    f.c.put(&format!("{path}/rules"), rules.clone(), &owner)
        .await?
        .ok()?;
    ensure!(
        f.c.put(&format!("{path}/rules"), rules.clone(), &owner)
            .await?
            .ok()?
            .data()["revision"]
            == 2,
        "rules replay"
    );
    let mut changed = rules.clone();
    changed["rules"] = json!("changed");
    f.c.put(&format!("{path}/rules"), changed, &owner)
        .await?
        .error("payload_conflict")?;
    let caps = json!({"requestId": new_id(), "expectedRevision": 1, "capabilities": {"v": 1, "tools": [], "resources": []}});
    f.c.put(&format!("{path}/capabilities"), caps.clone(), &owner)
        .await?
        .error("stale_revision")?;
    let mut update = caps.clone();
    update["expectedRevision"] = json!(2);
    f.c.put(&format!("{path}/capabilities"), update.clone(), &owner)
        .await?
        .ok()?;
    ensure!(
        f.c.put(&format!("{path}/capabilities"), update.clone(), &owner)
            .await?
            .ok()?
            .data()["revision"]
            == 3,
        "capabilities replay"
    );
    let mut unknown = update.clone();
    unknown["capabilities"]["tools"] = json!(["unknown"]);
    f.c.put(&format!("{path}/capabilities"), unknown, &owner)
        .await?
        .error("capability_missing")?;
    let mut escalated = update.clone();
    escalated["capabilities"]["role"] = json!("owner");
    f.c.put(&format!("{path}/capabilities"), escalated, &owner)
        .await?
        .error("invalid_capabilities")?;
    ensure!(
        f.c.db
            .scalar::<String>(
                "SELECT rules FROM agent_projects WHERE project=?",
                [&f.project]
            )?
            .is_empty(),
        "first project rules changed"
    );
    f.c.db.execute(
        "INSERT INTO project_members VALUES (?,?,'member',1,0)",
        [&second, &f.member.account_id],
    )?;
    f.c.db.execute(
        "INSERT INTO collaborator_agents VALUES (?,?,?,1,1)",
        [&second, &f.member.account_id, &agent],
    )?;
    f.c.db.execute(
        "INSERT INTO response_policies VALUES (?,?,?,1,1)",
        [&second, &f.member.account_id, &agent],
    )?;
    f.c.db.execute(
        "UPDATE agent_projects SET deleted=1 WHERE project=?",
        [&second],
    )?;
    f.c.put(&format!("{path}/rules"), rules, &owner)
        .await?
        .error("resource_not_found")?;
    let mut reactivate = participation.clone();
    reactivate["requestId"] = json!(new_id());
    reactivate["expectedParticipationRevision"] = json!(3);
    let mut stale = reactivate.clone();
    stale["expectedParticipationRevision"] = json!(0);
    f.c.put(&path, stale, &owner)
        .await?
        .error("stale_revision")?;
    ensure!(
        f.c.put(&path, reactivate, &owner).await?.ok()?.data()["participationRevision"] == 4,
        "reactivated participation revision"
    );
    ensure!(
        f.count("SELECT enabled FROM collaborator_agents", [])? == 0,
        "stale collaborator grant kept"
    );
    ensure!(
        f.count("SELECT respond FROM response_policies", [])? == 0,
        "stale response policy kept"
    );
    f.require_execution(&f.member, &second, &agent)
        .await?
        .error("execution_denied")?;
    ensure!(
        f.count("SELECT count(*) FROM agent_authority", [])? == 1,
        "authority rows"
    );
    Ok(())
}
