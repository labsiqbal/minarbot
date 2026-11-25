//! Agent deletion and visibility (ports of `app/agent-delete.test.ts` and
//! `app/agent-visibility.test.ts`).

use crate::fx_support::{api_agent, gated, id, row, settle};
use minarbot_testkit::{
    db::{Fixture, Inspector},
    gated::{Answer, GatedModel},
    http::Method,
    peer::PeerBridge,
    prelude::*,
    Server, FIXED_CLOCK,
};
use std::{
    fs,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
};

/// RFC 8032 test vector 1 public key as Ed25519 SPKI DER, base64url.
fn hub_key() -> String {
    use minarbot_testkit::base64::Engine;
    let mut der = vec![
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0,
    ];
    let key = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    der.extend((0..32).filter_map(|i| u8::from_str_radix(&key[i * 2..i * 2 + 2], 16).ok()));
    minarbot_testkit::base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(der)
}

/// POST with explicit headers (Origin defaults to the server's own origin).
async fn post_with(app: &Server, path: &str, body: Value, headers: &[(&str, &str)]) -> Result<u16> {
    let mut request = app
        .client()?
        .request(Method::POST, app.url(path))
        .header("Content-Type", "application/json")
        .body(body.to_string());
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("origin"))
    {
        request = request.header("Origin", app.origin());
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    Ok(request.send().await?.status().as_u16())
}

async fn create(app: &Server, name: &str) -> Result<String> {
    id(&api_agent(app, name, "fixture").await?)
}

/// Stop, seed with foreign keys on, start again (the Node fixture's `seed`).
async fn seed(app: &mut Server, change: impl FnOnce(&Fixture) -> Result<()>) -> Result<()> {
    app.edit_database(|db| {
        db.execute_batch("PRAGMA foreign_keys=ON")?;
        change(db)
    })
    .await
}

#[tokio::test]
async fn delete_cascades_every_agent_fk_scoped_data_workflows_desktop_and_journals_preserving_room_history_and_shared_resources(
) -> Result<()> {
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new()).await?;
    let a = create(&app, "Delete me").await?;
    let b = create(&app, "Keep me").await?;
    let thread = id(&app
        .post(
            &format!("/api/agents/{a}/threads"),
            json!({"id": new_id(), "name": "Private"}),
        )
        .await?)?;
    let channel = id(&app
        .post_expect(
            "/api/channels",
            json!({"id": new_id(), "name": "Room", "instructions": "Help", "lead": a, "members": [a, b]}),
            201,
        )
        .await?)?;
    let (direct, room, file, routine, workflow, run, connection) = (
        new_id(),
        new_id(),
        new_id(),
        new_id(),
        new_id(),
        new_id(),
        new_id(),
    );
    let desktop = root.database().with_file_name("agent-desktops").join(&a);
    fs::create_dir_all(&desktop)?;
    fs::write(desktop.join("profile"), "synthetic")?;
    let (a2, b2, thread2, channel2) = (a.clone(), b.clone(), thread.clone(), channel.clone());
    seed(&mut app, move |db| {
        let (a, b, thread, channel) = (&a2, &b2, &thread2, &channel2);
        db.execute("INSERT INTO turns(id,agent,thread,text,reply,status,created) VALUES (?,?,?,'private','private reply','done','fixture')", [&direct, a, thread])?;
        db.execute("INSERT INTO turns(id,agent,channel,text,reply,status,created) VALUES (?,?,?,'shared','past answer','done','fixture')", [&room, a, channel])?;
        db.execute("INSERT INTO room_discussions(root,members,lead,mentioned,fallback) VALUES (?,?,?,0,1)", [&room, &json!([a, b]).to_string(), a])?;
        db.execute("INSERT INTO room_turns(turn,root,round,processed) VALUES (?,?,0,1)", [&room, &room])?;
        db.execute("INSERT INTO room_messages(root,turn,kind,agent,text) VALUES (?,?,'reply',?,'past answer')", [&room, &room, a])?;
        db.connection().execute("INSERT INTO files(id,scope,name,digest,bytes,created) VALUES (?,?,'private.txt','digest',?,'fixture')", (&file, format!("threads:{thread}"), b"secret".to_vec()))?;
        db.execute("INSERT INTO generated_files(turn,file,call_id,call,after_action) VALUES (?,?,'file','{}',0)", [&direct, &file])?;
        db.execute("INSERT INTO memories(scope,id,text,essential,revision,removed) VALUES (?,?,'private memory',1,1,0)", [&format!("agents:{a}"), &new_id()])?;
        db.execute("INSERT INTO routines(id,agent,name,prompt,next_at,interval_ms,enabled,revision,trigger) VALUES (?,?,'Timer','Help',0,NULL,0,1,'schedule')", [&routine, a])?;
        db.execute("INSERT INTO routine_runs(routine,turn,missed,source) VALUES (?,?,0,'test')", [&routine, &direct])?;
        db.execute("INSERT INTO workflows(id,agent,name,revision,deleted) VALUES (?,?,'Flow',1,0)", [&workflow, a])?;
        let graph = json!({"entry": "out", "maxSteps": 1, "nodes": [{"id": "out", "kind": "output"}]}).to_string();
        db.execute("INSERT INTO workflow_versions VALUES (?,1,?)", [&workflow, &graph])?;
        db.execute("INSERT INTO workflow_runs(id,workflow,version,agent,graph,input,last,cursor,steps,revision,status,created) VALUES (?,?,1,?,?,'input','result','out',1,1,'done','fixture')", [&run, &workflow, a, &graph])?;
        db.execute("INSERT INTO workflow_steps(run,seq,node,turn,status) VALUES (?,1,'out',?,'done')", [&run, &direct])?;
        db.execute("INSERT INTO workflow_schedules VALUES (?,1,0,NULL,'input',0)", [&workflow])?;
        db.connection().execute("INSERT INTO agent_avatars VALUES (?,1,?)", (a, b"image".to_vec()))?;
        db.execute("INSERT INTO agent_visibility VALUES (?,1,1)", [a])?;
        db.execute("UPDATE sidebar SET sections=? WHERE id=1", [&json!([{"id": new_id(), "name": "Team", "agents": [a, b]}]).to_string()])?;
        db.execute("INSERT INTO agent_computers VALUES (?,1,1)", [a])?;
        db.execute("INSERT INTO computer_control VALUES (?,'agent',?,'fixture',1)", [a, a])?;
        db.execute("INSERT INTO mcp_connections VALUES (?,'Shared','https://example.test','encrypted','[]',0,1)", [&connection])?;
        db.execute("INSERT INTO mcp_grants VALUES (?,?,1,'[]')", [a, &connection])?;
        db.execute("INSERT INTO composio_grants VALUES (?,'github',1,1)", [a])?;
        db.execute("INSERT INTO composio_policies VALUES (?,'fingerprint','tool','account','github')", [a])?;
        db.execute("INSERT INTO composio_policy_projects VALUES ('fingerprint','project')", [])?;
        for agent in [a, b] {
            db.execute("INSERT INTO composio_policies VALUES (?,'shared','tool','account','github')", [agent])?;
        }
        db.execute("INSERT INTO composio_policy_projects VALUES ('shared','shared-project')", [])?;
        db.execute("INSERT INTO queue_positions VALUES (?,1)", [&direct])?;
        db.execute("INSERT INTO workspace_checkpoints VALUES (?,?,'/synthetic','refs/minarbot/checkpoints/synthetic','hash','pruned',0)", [&direct, a])?;
        db.execute("INSERT INTO memory_commands VALUES (?,?,?)", [&new_id(), &json!({"scope": format!("agents:{a}"), "agent": a}).to_string(), &json!({"id": file}).to_string()])?;
        Ok(())
    })
    .await?;
    let command = json!({"id": new_id()});
    let path = format!("/api/agents/{a}/delete");
    ensure!(
        app.post(&path, command.clone()).await? == json!({"deleted": true}),
        "delete"
    );
    ensure!(
        app.post(&path, command.clone()).await? == json!({"deleted": true}),
        "replay"
    );
    ensure!(
        app.post(&path, json!({"id": new_id()})).await? == json!({"deleted": true}),
        "fresh command after delete"
    );
    ensure!(!desktop.exists(), "desktop survived");
    let agents = app.get("/api/agents").await?;
    let ids: Vec<&Value> = agents["agents"]
        .as_array()
        .ok_or("agents")?
        .iter()
        .map(|agent| &agent["id"])
        .collect();
    ensure!(ids == [&json!(b)], "agents {ids:?}");
    app.get_expect(&format!("/api/agents/{a}/turns"), 404)
        .await?;
    app.get_expect(&format!("/api/threads/{thread}"), 404)
        .await?;
    let turns = app.get(&format!("/api/channels/{channel}/turns")).await?;
    let turns = turns.as_array().ok_or("channel turns")?;
    ensure!(turns.len() == 1, "channel turns {turns:?}");
    ensure!(turns[0]["reply"] == "past answer", "reply {}", turns[0]);
    ensure!(
        turns[0]["agent_name"] == "Deleted agent",
        "agent_name {}",
        turns[0]
    );
    ensure!(
        turns[0]["room"]["messages"][0]["name"] == "Deleted agent",
        "room message {}",
        turns[0]["room"]
    );
    app.post_expect(
        "/api/agents",
        json!({"runtime": "api", "id": a, "name": "Revive", "model": "fixture", "instructions": "Help"}),
        409,
    )
    .await?;
    let db = Inspector::open(root.database())?;
    ensure!(
        db.rows("PRAGMA foreign_key_check", [])?.is_empty(),
        "foreign key violations"
    );
    for table in db.rows("SELECT name FROM sqlite_schema WHERE type='table'", [])? {
        let name = table["name"].as_str().unwrap_or_default().to_owned();
        for key in db.rows(&format!("PRAGMA foreign_key_list(\"{name}\")"), [])? {
            if key["table"] == "agents" {
                let from = key["from"].as_str().unwrap_or_default();
                let count: i64 = db.scalar(
                    &format!("SELECT count(*) FROM \"{name}\" WHERE \"{from}\"=?"),
                    [&a],
                )?;
                ensure!(count == 0, "{name}.{from} still references the agent");
            }
        }
    }
    for table in [
        "threads",
        "routines",
        "workflow_runs",
        "workflow_steps",
        "workflow_schedules",
        "workflows",
        "files",
        "memories",
        "memory_commands",
        "workspace_checkpoints",
    ] {
        ensure!(db.count(table)? == 0, "{table} not empty");
    }
    ensure!(
        db.rows(
            "SELECT fingerprint,project FROM composio_policy_projects",
            []
        )? == vec![json!({"fingerprint": "shared", "project": "shared-project"})],
        "policy projects"
    );
    ensure!(
        db.row("SELECT agent FROM composio_policies", [])?["agent"] == b.as_str(),
        "shared policy owner"
    );
    let sections: Value =
        serde_json::from_str(&db.scalar::<String>("SELECT sections FROM sidebar", [])?)?;
    ensure!(sections[0]["agents"] == json!([b]), "sidebar {sections}");
    ensure!(
        db.count("mcp_connections")? == 1,
        "shared MCP connection removed"
    );
    ensure!(db.count("channels")? == 1, "channel removed");
    Ok(())
}

#[tokio::test]
async fn delete_guards_last_agent_active_paused_queued_work_unknown_ids_cross_origin_bearer_peer_script_views_and_hub_grant_ownership(
) -> Result<()> {
    let root = TestRoot::new()?;
    let mut app = root
        .start(ServerConfig::new().option(
            "hubLink",
            json!({"origin": "https://hub.example.test", "hubSigningPublicKey": hub_key()}),
        ))
        .await?;
    let a = create(&app, "Owner agent").await?;
    let path = format!("/api/agents/{a}/delete");
    let command = json!({"id": new_id()});
    app.post_expect(&path, command.clone(), 409).await?;
    let b = create(&app, "Other").await?;
    ensure!(
        post_with(
            &app,
            &path,
            command.clone(),
            &[("Origin", "https://foreign.test")]
        )
        .await?
            == 403,
        "cross origin"
    );
    ensure!(
        post_with(
            &app,
            &path,
            command.clone(),
            &[("Authorization", "Bearer peer-token")]
        )
        .await?
            == 403,
        "bearer"
    );
    ensure!(
        post_with(
            &app,
            &format!("/api/peer/agents/{a}/delete"),
            command.clone(),
            &[("Authorization", "Bearer peer-token")]
        )
        .await?
            == 404,
        "peer view"
    );
    app.post_expect(
        &format!("/api/scripts/agents/{a}/delete"),
        command.clone(),
        403,
    )
    .await?;
    let mut forced = command.clone();
    forced["force"] = json!(true);
    app.post_expect(&path, forced, 400).await?;
    app.post_expect(
        &format!("/api/agents/{}/delete", new_id()),
        command.clone(),
        404,
    )
    .await?;
    let turn = new_id();
    let (turn2, a2) = (turn.clone(), a.clone());
    seed(&mut app, move |db| {
        db.execute("INSERT INTO turns(id,agent,text,status,created,paused) VALUES (?,?,'waiting','queued','fixture',1)", [&turn2, &a2])?;
        Ok(())
    })
    .await?;
    app.post_expect(&path, command.clone(), 409).await?;
    let turn2 = turn.clone();
    seed(&mut app, move |db| {
        db.execute(
            "UPDATE turns SET status='cancelled',paused=0 WHERE id=?",
            [&turn2],
        )?;
        Ok(())
    })
    .await?;
    let (project, participation, other_participation) = (new_id(), new_id(), new_id());
    let (a2, b2, project2, participation2, other2) = (
        a.clone(),
        b.clone(),
        project.clone(),
        participation.clone(),
        other_participation.clone(),
    );
    seed(&mut app, move |db| {
        db.execute("INSERT INTO hub_link_grants VALUES (?,?,?,1)", [&project2, &participation2, &json!({"projectId": project2, "participationId": participation2, "localAgentId": a2, "agentId": a2, "revision": 1}).to_string()])?;
        db.execute("INSERT INTO hub_link_grants VALUES (?,?,?,1)", [&project2, &other2, &json!({"projectId": project2, "participationId": other2, "localAgentId": b2, "agentId": b2, "revision": 1}).to_string()])?;
        Ok(())
    })
    .await?;
    app.post(&path, command).await?;
    let db = Inspector::open(root.database())?;
    ensure!(db.count("hub_link_grants")? == 1, "grants");
    ensure!(
        db.row("SELECT participation_id FROM hub_link_withdrawals", [])?["participation_id"]
            == participation.as_str(),
        "withdrawal"
    );
    Ok(())
}

#[tokio::test]
async fn delete_preserves_coordination_journals_until_another_agent_finishes_its_dependent_work(
) -> Result<()> {
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new()).await?;
    let a = create(&app, "Target").await?;
    let b = create(&app, "Working").await?;
    let turn = new_id();
    let (a2, b2, turn2) = (a.clone(), b.clone(), turn.clone());
    seed(&mut app, move |db| {
        db.execute("INSERT INTO turns(id,agent,text,status,created,paused) VALUES (?,?,'waiting','queued','fixture',1)", [&turn2, &b2])?;
        db.execute("INSERT INTO coordination(turn,owner,target,kind,prompt,call) VALUES (?,?,?,'message','help','{}')", [&turn2, &b2, &a2])?;
        Ok(())
    })
    .await?;
    let path = format!("/api/agents/{a}/delete");
    let command = json!({"id": new_id()});
    app.post_expect(&path, command.clone(), 409).await?;
    let (a2, turn2) = (a.clone(), turn.clone());
    seed(&mut app, move |db| {
        let coordination: i64 = db.scalar("SELECT count(*) FROM coordination", [])?;
        if coordination != 1 {
            return Err(format!("coordination rows {coordination}").into());
        }
        if db
            .rows("SELECT 1 FROM agents WHERE id=?", [&a2])?
            .is_empty()
        {
            return Err("agent deleted despite dependent work".into());
        }
        db.execute(
            "UPDATE turns SET status='cancelled',paused=0 WHERE id=?",
            [&turn2],
        )?;
        Ok(())
    })
    .await?;
    app.post(&path, command).await?;
    let db = Inspector::open(root.database())?;
    ensure!(
        !db.rows("SELECT 1 FROM turns WHERE id=?", [&turn])?
            .is_empty(),
        "dependent turn removed"
    );
    ensure!(db.count("coordination")? == 0, "coordination kept");
    Ok(())
}

#[tokio::test]
async fn running_provider_work_must_settle_before_delete_transactional_failure_preserves_agent_and_desktop_for_retry(
) -> Result<()> {
    let model = GatedModel::start().await?;
    model.respond(|_| Answer::Park);
    let root = TestRoot::new()?;
    let mut app = root.start(gated(&model)).await?;
    let agent = new_id();
    let other = new_id();
    for agent in [&agent, &other] {
        app.post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": agent, "name": "Agent", "model": "fixture", "instructions": "Help"}),
            201,
        )
        .await?;
    }
    let turn = new_id();
    let command = json!({"id": new_id()});
    let path = format!("/api/agents/{agent}/delete");
    let turns = format!("/api/agents/{agent}/turns");
    app.post_expect(&turns, json!({"id": turn, "text": "Hold"}), 202)
        .await?;
    let held = model.parked().await?;
    app.post_expect(&path, command.clone(), 409).await?;
    held.answer(Answer::text("Finished"));
    settle(&app, &turns, &turn, "done").await?;
    app.shutdown().await?;
    let desktop = root
        .database()
        .with_file_name("agent-desktops")
        .join(&agent);
    fs::create_dir_all(&desktop)?;
    fs::write(desktop.join("synthetic"), "saved")?;
    Fixture::open(root.database())?.execute_batch(
        "CREATE TRIGGER fail_delete BEFORE DELETE ON agents BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;",
    )?;
    app.start_stopped(ServerConfig::new()).await?;
    app.post_expect(&path, command.clone(), 500).await?;
    ensure!(desktop.exists(), "desktop removed by a failed delete");
    app.shutdown().await?;
    {
        let retry = Fixture::open(root.database())?;
        ensure!(
            !retry
                .rows("SELECT 1 FROM agents WHERE id=?", [&agent])?
                .is_empty(),
            "agent removed by a failed delete"
        );
        ensure!(
            !retry
                .rows("SELECT 1 FROM turns WHERE id=?", [&turn])?
                .is_empty(),
            "turn removed by a failed delete"
        );
        retry.execute_batch("DROP TRIGGER fail_delete")?;
    }
    app.start_stopped(ServerConfig::new()).await?;
    app.post(&path, command).await?;
    ensure!(!desktop.exists(), "desktop survived the retried delete");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paired_peer_cannot_delete_owner_agents_deletion_on_owner_refreshes_peer_roster(
) -> Result<()> {
    let issuer = PeerBridge::issuer().await?;
    let peer_bridge = PeerBridge::start().await?;
    let owner_root = TestRoot::new()?;
    let peer_root = TestRoot::new()?;
    let (owner, peer) = minarbot_testkit::server::start_pair(
        &owner_root,
        ServerConfig::new().computer_bridge(issuer.bridge()),
        &peer_root,
        ServerConfig::new().computer_bridge(peer_bridge.bridge()),
    )
    .await?;
    let agent = new_id();
    for id in [agent.clone(), new_id()] {
        owner
            .post_expect(
                "/api/agents",
                json!({"runtime": "api", "id": id, "name": "Owner", "model": "fixture", "instructions": "Help"}),
                201,
            )
            .await?;
    }
    let code = owner
        .post("/api/pairing-codes", json!({"id": new_id()}))
        .await?;
    peer.post(
        "/api/machines",
        json!({"id": new_id(), "origin": owner.origin(), "code": code["code"]}),
    )
    .await?;
    let token = issuer
        .issued()
        .first()
        .map(|issued| issued.token.clone())
        .ok_or("owner issued no peer token")?;
    let bearer = format!("Bearer {token}");
    ensure!(
        post_with(
            &owner,
            &format!("/api/agents/{agent}/delete"),
            json!({"id": new_id()}),
            &[("Authorization", &bearer)]
        )
        .await?
            == 403,
        "peer token deleted through the owner route"
    );
    ensure!(
        post_with(
            &owner,
            &format!("/api/peer/agents/{agent}/delete"),
            json!({"id": new_id()}),
            &[("Authorization", &bearer)]
        )
        .await?
            == 404,
        "peer delete route exists"
    );
    let roster = |machines: &Value| {
        machines["machines"][0]["agents"]
            .as_array()
            .map(|agents| agents.iter().any(|item| item["id"] == agent.as_str()))
    };
    owner
        .post(
            &format!("/api/agents/{agent}/delete"),
            json!({"id": new_id()}),
        )
        .await?;
    let machines = peer.refresh_machines().await?;
    ensure!(
        roster(&machines) == Some(false),
        "peer roster kept the deleted agent: {machines}"
    );
    Ok(())
}

fn git(workspace: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()?;
    ensure!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

#[tokio::test]
async fn owned_git_checkpoint_refs_clean_up_durably_without_removing_project_files_failed_cleanup_retries(
) -> Result<()> {
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new()).await?;
    let a = create(&app, "Delete").await?;
    create(&app, "Keep").await?;
    let workspace = root.dir("project")?;
    fs::write(workspace.join("keep.txt"), "Shared project")?;
    git(&workspace, &["init", "--quiet"])?;
    git(&workspace, &["add", "."])?;
    git(
        &workspace,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "--quiet",
            "-m",
            "Synthetic",
        ],
    )?;
    let hash = git(&workspace, &["rev-parse", "HEAD"])?;
    let turn = new_id();
    let reference = format!("refs/minarbot/checkpoints/{turn}");
    git(&workspace, &["update-ref", &reference, &hash])?;
    let directory = workspace.to_string_lossy().into_owned();
    let (a2, turn2, reference2, directory2) = (
        a.clone(),
        turn.clone(),
        reference.clone(),
        directory.clone(),
    );
    seed(&mut app, move |db| {
        db.execute(
            "INSERT INTO turns(id,agent,text,status,created) VALUES (?,?,'done','done','fixture')",
            [&turn2, &a2],
        )?;
        db.execute(
            "INSERT INTO workspace_checkpoints VALUES (?,?,?,?,?,'done',0)",
            [&turn2, &a2, &directory2, &reference2, &hash],
        )?;
        Ok(())
    })
    .await?;
    // Change the recorded directory, so the rows go but the ref unlink fails.
    let missing = root.path().join("missing").to_string_lossy().into_owned();
    let a2 = a.clone();
    seed(&mut app, move |db| {
        db.execute(
            "UPDATE workspace_checkpoints SET directory=? WHERE agent=?",
            [&missing, &a2],
        )?;
        Ok(())
    })
    .await?;
    let path = format!("/api/agents/{a}/delete");
    let command = json!({"id": new_id()});
    app.post_expect(&path, command.clone(), 400).await?;
    let a2 = a.clone();
    seed(&mut app, move |db| {
        if !db
            .rows("SELECT 1 FROM agents WHERE id=?", [&a2])?
            .is_empty()
        {
            return Err("agent row survived".into());
        }
        if db
            .rows(
                "SELECT 1 FROM agent_delete_checkpoints WHERE agent=?",
                [&a2],
            )?
            .is_empty()
        {
            return Err("no pending checkpoint cleanup".into());
        }
        db.execute(
            "UPDATE agent_delete_checkpoints SET directory=? WHERE agent=?",
            [&directory, &a2],
        )?;
        Ok(())
    })
    .await?;
    app.post(&path, command).await?;
    ensure!(workspace.join("keep.txt").exists(), "project file removed");
    ensure!(
        git(
            &workspace,
            &["for-each-ref", "--format=%(refname)", &reference]
        )?
        .is_empty(),
        "checkpoint ref survived"
    );
    ensure!(
        Inspector::open(root.database())?.count("agent_delete_checkpoints")? == 0,
        "cleanup journal kept"
    );
    Ok(())
}

#[tokio::test]
async fn hidden_agents_finish_active_work_hold_queue_across_restart_reject_new_work_and_skip_missed_schedules(
) -> Result<()> {
    let model = GatedModel::start().await?;
    let roster: Arc<Mutex<Vec<String>>> = Arc::default();
    let calls: Arc<Mutex<Vec<String>>> = Arc::default();
    let (seen, recorded) = (roster.clone(), calls.clone());
    model.respond(move |request| {
        let last = request.messages().last().cloned().unwrap_or(Value::Null);
        let text = last["content"].as_str().unwrap_or_default().to_owned();
        if text == "CHECK ROSTER" {
            return Answer::tool_id("list", "list_agents", json!({}));
        }
        if last["role"] == "tool" {
            let result: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            if let Some(agents) = result["agents"].as_array() {
                if let Ok(mut seen) = seen.lock() {
                    seen.extend(
                        agents
                            .iter()
                            .filter_map(|agent| agent["id"].as_str().map(str::to_owned)),
                    );
                }
                return Answer::text("Finished");
            }
        }
        if let Ok(mut recorded) = recorded.lock() {
            recorded.push(text.clone());
        }
        match text.as_str() {
            "WAIT" => Answer::tool_id(
                "wait",
                "wait_for_event",
                json!({"event": "ready", "timeoutSeconds": 3600}),
            ),
            "HOLD" => Answer::Park,
            _ => Answer::text("Finished"),
        }
    });
    let root = TestRoot::new()?;
    let mut app = root.start(gated(&model).concurrency(1)).await?;
    let a = create(&app, "A").await?;
    let b = create(&app, "B").await?;
    let direct = format!("/api/agents/{a}/turns");
    let visibility = format!("/api/agents/{a}/visibility");
    let side = id(&app
        .post(
            &format!("/api/agents/{a}/threads"),
            json!({"id": new_id(), "name": "Side"}),
        )
        .await?)?;
    let channel = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Team", "instructions": "Help", "lead": a, "members": [a, b]}),
        )
        .await?)?;
    let routine = app
        .post(
            &format!("/api/agents/{a}/routines"),
            json!({"id": new_id(), "name": "Timer", "prompt": "Timer", "nextAt": FIXED_CLOCK + 3_600_000,
                "intervalSeconds": 60, "enabled": true}),
        )
        .await?;
    let routine_id = id(&routine)?;
    let waiting_id = new_id();
    let channel_path = format!("/api/channels/{channel}/turns");
    app.post(
        &channel_path,
        json!({"id": waiting_id, "text": "WAIT", "recipient": a}),
    )
    .await?;
    app.wait_row(&channel_path, &waiting_id, |turn| {
        turn["events"]
            .as_array()
            .is_some_and(|events| !events.is_empty())
    })
    .await?;
    let (active, queued) = (new_id(), new_id());
    app.post(&direct, json!({"id": active, "text": "HOLD"}))
        .await?;
    let held = model.parked().await?;
    app.post(&direct, json!({"id": queued, "text": "QUEUED"}))
        .await?;
    let channel_queued = new_id();
    app.post(
        &channel_path,
        json!({"id": channel_queued, "text": "CHANNEL QUEUED", "recipient": a}),
    )
    .await?;
    let c = create(&app, "C").await?;
    let group = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Group", "instructions": "Help", "lead": a, "members": [a, b, c]}),
        )
        .await?)?;
    let group_path = format!("/api/channels/{group}/turns");
    let released = new_id();
    app.post(
        &group_path,
        json!({"id": released, "text": "REASSIGNED", "recipient": a}),
    )
    .await?;
    let hide = json!({"id": new_id(), "revision": 0, "hidden": true});
    let result = app.post(&visibility, hide.clone()).await?;
    ensure!(
        row(&app, &channel_path, &channel_queued).await?["agent"] == b.as_str(),
        "queued channel turn not moved to another member"
    );
    ensure!(
        app.post(&visibility, hide.clone()).await? == result,
        "hide replay"
    );
    let suspended = row(&app, &channel_path, &waiting_id).await?;
    ensure!(
        suspended["agent"] == a.as_str(),
        "waiting turn moved: {suspended}"
    );
    ensure!(
        suspended["routing"] != "released",
        "waiting turn released: {suspended}"
    );
    let waiting = row(&app, &group_path, &released).await?;
    ensure!(
        waiting["routing"] == "released",
        "group turn routing {waiting}"
    );
    app.post(
        &format!("{group_path}/{released}/pause"),
        json!({"id": new_id(), "revision": waiting["revision"]}),
    )
    .await?;
    let paused = row(&app, &group_path, &released).await?;
    app.post(
        &format!("{group_path}/{released}/reassign"),
        json!({"id": new_id(), "revision": paused["revision"], "recipient": b}),
    )
    .await?;
    ensure!(
        row(&app, &group_path, &released).await?["routing"] == "assigned",
        "reassigned routing"
    );
    let mut changed = hide.clone();
    changed["hidden"] = json!(false);
    app.post_expect(&visibility, changed, 409).await?;
    let mut fresh = hide.clone();
    fresh["id"] = json!(new_id());
    app.post_expect(&visibility, fresh, 409).await?;
    for path in [
        direct.clone(),
        format!("/api/threads/{side}/turns"),
        channel_path.clone(),
    ] {
        let mut body = json!({"id": new_id(), "text": "Denied"});
        if path.contains("channels") {
            body["recipient"] = json!(a);
        }
        app.post_expect(&path, body, 409).await?;
    }
    app.post_expect(
        &channel_path,
        json!({"id": new_id(), "text": "Hidden lead"}),
        202,
    )
    .await?;
    app.post_expect(
        &format!("/api/routines/{routine_id}/test"),
        json!({"id": new_id(), "revision": routine["revision"]}),
        409,
    )
    .await?;
    let workflow = app
        .post(
            &format!("/api/agents/{a}/workflows"),
            json!({"id": new_id(), "revision": 0, "name": "Workflow",
                "graph": {"entry": "out", "maxSteps": 1, "nodes": [{"id": "out", "kind": "output"}]}}),
        )
        .await?;
    let workflow_id = id(&workflow)?;
    app.post_expect(
        &format!("/api/workflows/{workflow_id}/run"),
        json!({"id": new_id(), "revision": 1, "input": "Denied"}),
        409,
    )
    .await?;
    app.post(
        &format!("/api/workflows/{workflow_id}/schedule"),
        json!({"id": new_id(), "revision": 0, "nextAt": FIXED_CLOCK - 1000, "intervalSeconds": null,
            "input": "skip", "enabled": true}),
    )
    .await?;
    let once = id(&app
        .post(
            &format!("/api/agents/{a}/routines"),
            json!({"id": new_id(), "name": "Once", "prompt": "Never run", "nextAt": FIXED_CLOCK - 1000,
                "intervalSeconds": null, "enabled": true}),
        )
        .await?)?;
    held.answer(Answer::text("Finished"));
    settle(&app, &direct, &active, "done").await?;
    ensure!(
        row(&app, &direct, &queued).await?["status"] == "queued",
        "hidden queue ran"
    );
    settle(&app, &channel_path, &channel_queued, "done").await?;
    settle(&app, &group_path, &released, "done").await?;
    app.restart().await?;
    let agents = app.get("/api/agents").await?;
    ensure!(
        agents["agents"]
            .as_array()
            .and_then(|agents| agents.iter().find(|agent| agent["id"] == a.as_str()))
            .is_some_and(|agent| agent["hidden"] == true),
        "hidden flag lost: {agents}"
    );
    ensure!(
        row(&app, &direct, &queued).await?["status"] == "queued",
        "queue after restart"
    );
    let source = new_id();
    let roster_path = format!("/api/agents/{b}/turns");
    app.post_expect(
        &roster_path,
        json!({"id": source, "text": "CHECK ROSTER"}),
        202,
    )
    .await?;
    settle(&app, &roster_path, &source, "done").await?;
    {
        let roster = roster.lock().map_err(|_| "roster poisoned")?;
        ensure!(roster.contains(&c), "roster misses C: {roster:?}");
        ensure!(!roster.contains(&a), "roster lists hidden A: {roster:?}");
    }
    let routine2 = routine_id.clone();
    app.edit_database(move |db| {
        db.connection().execute(
            "UPDATE routines SET next_at=? WHERE id=?",
            (FIXED_CLOCK - 180_000, &routine2),
        )?;
        Ok(())
    })
    .await?;
    let restore = json!({"id": new_id(), "revision": 1, "hidden": false});
    app.post(&visibility, restore.clone()).await?;
    app.post(&visibility, restore).await?;
    settle(&app, &direct, &queued, "done").await?;
    settle(&app, &channel_path, &channel_queued, "done").await?;
    {
        let calls = calls.lock().map_err(|_| "calls poisoned")?;
        ensure!(
            calls
                .iter()
                .filter(|text| *text == "CHANNEL QUEUED")
                .count()
                == 1,
            "channel queued calls {calls:?}"
        );
        ensure!(
            calls.iter().filter(|text| *text == "QUEUED").count() == 1,
            "queued calls {calls:?}"
        );
        ensure!(calls.len() == 6, "calls {calls:?}");
    }
    app.shutdown().await?;
    let check = Inspector::open(root.database())?;
    ensure!(
        check.scalar::<i64>("SELECT next_at FROM routines WHERE id=?", [&routine_id])?
            > FIXED_CLOCK,
        "missed routine not moved forward"
    );
    ensure!(check.count("agent_visibility_skips")? == 2, "skips");
    ensure!(
        check.scalar::<i64>("SELECT enabled FROM routines WHERE id=?", [&once])? == 0,
        "one-off routine still enabled"
    );
    ensure!(
        check.scalar::<i64>(
            "SELECT enabled FROM workflow_schedules WHERE workflow=?",
            [&workflow_id]
        )? == 0,
        "workflow schedule still enabled"
    );
    ensure!(
        check.scalar::<String>(
            "SELECT reason FROM workflow_occurrences WHERE workflow=?",
            [&workflow_id]
        )? == "hidden",
        "workflow occurrence reason"
    );
    ensure!(
        check.scalar::<i64>(
            "SELECT count(*) FROM workflow_runs WHERE workflow=?",
            [&workflow_id]
        )? == 0,
        "hidden workflow ran"
    );
    Ok(())
}
