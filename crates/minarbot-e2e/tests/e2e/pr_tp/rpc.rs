//! Canonical RPCs through a home: memory, history, skills, directory,
//! messages, setup, interruption, profile, provider settings and routines.

use super::{
    fixture::{agent_body, digest, direct, fresh_revisions, locked, until, Home, Owned, Tp, SETTLE},
    routines::text,
    shared::run,
};
use minarbot_testkit::{device_host::is_code, http::Method, prelude::*};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

/// Lose the `/context-mutations` reply of `input`, apply `change`, restart
/// and wait for recovery to acknowledge the action; returns its content.
async fn lost_then(tp: &mut Tp, home: &Home, owned: &Owned, input: &Value, change: impl FnOnce(&Tp) -> Result<()>) -> Result<Value> {
    let action = text(&input["actionId"]);
    let seen = tp.lose_next("/context-mutations");
    let lost = home.execute(input).await;
    tp.fault(None);
    ensure!(lost.is_err(), "reply was not lost: {lost:?}");
    ensure!(*locked(&seen) == Some(200), "lost reply status {:?}", locked(&seen));
    change(tp)?;
    tp.restart_home(home).await?;
    home.acknowledged(&action).await?;
    home.content(tp, &owned.task, &action).await
}

async fn run_lost(tp: &mut Tp, home: &Home, owned: &mut Owned, name: &str, args: Value, change: impl FnOnce(&Tp) -> Result<()>) -> Result<Value> {
    let input = home.canonical_input(tp, owned, name, &args).await?;
    lost_then(tp, home, owned, &input, change).await
}

async fn admitted(tp: &Tp, home: &Home, project: &str, agent: &str, text_body: &str) -> Result<Owned> {
    let task = new_id();
    tp.task(&task, project, agent, home, text_body).await?;
    home.start(&task).await
}

fn ids(rows: &Value, field: &str) -> Vec<Value> {
    rows.as_array().into_iter().flatten().map(|row| row[field].clone()).collect()
}

async fn participate(tp: &Tp, project: &str, agent: &str, tools: Value) -> Result<()> {
    let (status, _) = tp
        .send_as(
            &format!("/api/projects/{project}/agents/{agent}"),
            Some(json!({"requestId": new_id(), "expectedProjectRevision": 1, "expectedAgentRevision": 1,
                "expectedAuthorityRevision": 1, "expectedParticipationRevision": 0, "rules": "",
                "capabilities": {"v": 1, "tools": tools, "resources": []}})),
            "user_owner",
            Some(Method::PUT),
        )
        .await?;
    ensure!(status == 201, "participation: {status}");
    Ok(())
}

fn account_of(tp: &Tp, subject: &str) -> Result<String> {
    tp.inspect()?.scalar("SELECT id FROM accounts WHERE clerk_sub=?", [subject])
}

#[tokio::test]
async fn canonical_memory_rpc_isolates_projects_and_retains_committed_memory_after_a_lost_reply(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let home = tp.home("home.sqlite").await?;
        let (project, other, agent) = (new_id(), new_id(), new_id());
        for id in [&project, &other] {
            tp.project(id, id).await?;
        }
        tp.agent(&agent, "Agent", "Remember", &project, &home).await?;
        participate(&tp, &other, &agent, json!(["remember_memory", "search_memory", "select_memory"])).await?;
        let mut first = admitted(&tp, &home, &project, &agent, "Remember here").await?;
        let saved = run_lost(&mut tp, &home, &mut first, "remember_memory", json!({"text": "Project A private memory"}), |_| Ok(())).await?;
        ensure!(saved["revision"] == 1, "saved: {saved}");
        let selected = run(&mut tp, &home, &mut first, "select_memory",
            json!({"items": [{"id": saved["id"], "revision": 1, "essential": true}]}), false).await?;
        ensure!(selected["items"][0]["revision"] == 2, "selected: {selected}");
        let found = run(&mut tp, &home, &mut first, "search_memory", json!({}), false).await?;
        ensure!(found["items"][0]["text"] == "Project A private memory", "search: {found}");
        let long = run(&mut tp, &home, &mut first, "remember_memory", json!({"text": "x".repeat(2001)}), false).await?;
        ensure!(long["status"] == 400, "long memory: {long}");
        ensure!(
            run(&mut tp, &home, &mut first, "search_memory", json!({}), false).await?["items"].as_array().map(Vec::len) == Some(1),
            "one memory"
        );
        let mut second = admitted(&tp, &home, &other, &agent, "Remember here").await?;
        ensure!(run(&mut tp, &home, &mut second, "search_memory", json!({}), false).await?["items"] == json!([]), "other project");
        let db = tp.write()?;
        ensure!(db.scalar::<i64>("SELECT count(*) FROM memories", [])? == 1, "memories");
        ensure!(db.scalar::<String>("SELECT project FROM record_scopes WHERE table_name='memories'", [])? == project, "memory scope");
        let scope: String = db.scalar("SELECT scope FROM memories", [])?;
        db.execute(
            "INSERT INTO memories(scope,id,text,essential,revision,removed,selection_locked) VALUES (?,?,?,0,1,0,1)",
            [&scope, &new_id(), "Unmapped legacy private content"],
        )?;
        let input = home.canonical_input(&tp, &mut first, "search_memory", &json!({})).await?;
        let denied = home.execute(&input).await;
        ensure!(denied.as_ref().is_err_and(|error| is_code(error, "record_scope_missing")), "unmapped memory: {denied:?}");
        Ok(())
    }
    .await;
    tp.fault(None);
    tp.close().await?;
    result
}

/// A direct, side-thread or channel task that reads history.
async fn history_task(tp: &Tp, home: &Home, kind: &str, project: &str, agent: &str, thread: &str, channel: &str) -> Result<Owned> {
    let task = new_id();
    let mut payload = json!({"v": 1, "kind": kind, "message": {"text": "Read history", "files": []}});
    let mut revisions = fresh_revisions();
    if kind == "side" {
        payload["threadId"] = json!(thread);
        revisions["thread"] = json!(1);
    } else if kind == "channel" {
        payload["channelId"] = json!(channel);
        payload["routing"] = json!("assigned");
        revisions["channel"] = json!(1);
    }
    admit_with(tp, home, &task, project, agent, &payload, &revisions).await?;
    home.start(&task).await
}

async fn admit_with(tp: &Tp, home: &Home, task: &str, project: &str, agent: &str, payload: &Value, revisions: &Value) -> Result<()> {
    let home_id = home.id();
    let (status, body) = tp
        .send(
            "/api/tasks",
            Some(json!({"requestId": task, "projectId": project, "agentId": agent, "homeDeviceId": home_id,
                "payload": payload, "expectedRevisions": revisions,
                "payloadDigest": digest(&json!(["minarbot-task-payload-v1", task, project, agent, home_id, payload, revisions]))})),
        )
        .await?;
    ensure!(status == 202, "task: {status} {body}");
    Ok(())
}

#[tokio::test]
async fn canonical_history_rpc_pages_scoped_originals_and_retains_exact_reads_after_a_lost_reply(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let home = tp.home("home.sqlite").await?;
        let (project, other, agent, thread, channel) = (new_id(), new_id(), new_id(), new_id(), new_id());
        for id in [&project, &other] {
            tp.project(id, id).await?;
        }
        tp.agent(&agent, "Reader", "Read originals", &project, &home).await?;
        tp.send_as("/api/session", None, "user_foreign", None).await?;
        let (owner, foreign) = (account_of(&tp, "user_owner")?, account_of(&tp, "user_foreign")?);
        let db = tp.write()?;
        let seed = |text_body: &str, root_kind: &str, root: &str, visibility: &str, account: &str, project_id: &str, mapped: bool| -> Result<(String, i64)> {
            let id = new_id();
            db.execute(
                "INSERT INTO turns(id,agent,text,reply,status,created) VALUES (?,?,?,?,'done','fixture')",
                [&id, &agent, text_body, "Saved reply"],
            )?;
            let seq = db.connection().last_insert_rowid();
            if mapped {
                db.execute(
                    "INSERT INTO record_scopes VALUES ('turns',?,?,?,?,?,?)",
                    [&json!([id]).to_string(), account, project_id, visibility, root_kind, root],
                )?;
            }
            Ok((id, seq))
        };
        db.execute("INSERT INTO threads(id,agent,name) VALUES (?,?,?)", [&thread, &agent, "Side"])?;
        db.execute("INSERT INTO record_scopes VALUES ('threads',?,?,?,'owner','thread',?)", [&json!([thread]).to_string(), &owner, &project, &thread])?;
        db.execute("INSERT INTO channels(id,name,instructions,lead) VALUES (?,'Room','Read',?)", [&channel, &agent])?;
        db.execute("INSERT INTO channel_members VALUES (?,?)", [&channel, &agent])?;
        db.execute("INSERT INTO record_scopes VALUES ('channels',?,?,?,'project','channel',?)", [&json!([channel]).to_string(), &owner, &project, &channel])?;
        let original_text = format!("{}ORIGINAL_END", "x".repeat(9000));
        let oldest = seed(&original_text, "agent", &agent, "owner", &owner, &project, true)?;
        let direct_rows: Vec<(String, i64)> = (0..6)
            .map(|i| seed(&format!("Direct {i}"), "agent", &agent, "owner", &owner, &project, true))
            .collect::<Result<_>>()?;
        let side = seed("SIDE_ONLY", "thread", &thread, "owner", &owner, &project, true)?;
        let shared = seed("CHANNEL_SHARED", "channel", &channel, "project", &foreign, &project, true)?;
        let hidden = [
            seed("PRIVATE_HUMAN", "agent", &agent, "owner", &foreign, &project, true)?,
            seed("FOREIGN_PROJECT", "agent", &agent, "owner", &owner, &other, true)?,
            seed("UNMAPPED", "agent", &agent, "owner", &owner, &project, false)?,
        ];
        let mut first = history_task(&tp, &home, "direct", &project, &agent, &thread, &channel).await?;
        let future = new_id();
        db.execute("INSERT INTO turns(id,agent,text,reply,status,created) VALUES (?,?,'FUTURE','Future reply','done','fixture')", [&future, &agent])?;
        db.execute("INSERT INTO record_scopes VALUES ('turns',?,?,?,'owner','agent',?)", [&json!([future]).to_string(), &owner, &project, &agent])?;
        drop(db);
        let page = run(&mut tp, &home, &mut first, "read_history", json!({}), false).await?;
        let expected: Vec<Value> = direct_rows[1..].iter().rev().map(|row| json!(row.0)).collect();
        ensure!(ids(&page["messages"], "id") == expected, "first page: {page}");
        ensure!(page["next_before"] == direct_rows[1].1, "next_before");
        let next = run(&mut tp, &home, &mut first, "read_history", json!({"before": page["next_before"]}), false).await?;
        ensure!(ids(&next["messages"], "id") == vec![json!(direct_rows[0].0), json!(oldest.0)], "second page: {next}");
        ensure!(next["next_before"].is_null(), "second next_before");
        ensure!(next["messages"][1]["truncated"] == true, "truncated");
        let oldest_id = oldest.0.clone();
        let original = run_lost(&mut tp, &home, &mut first, "read_message", json!({"id": oldest.0}), |tp| {
            tp.write()?.execute("UPDATE turns SET text=?,revision=revision+1 WHERE id=?", ["Changed after read", &oldest_id])?;
            Ok(())
        })
        .await?;
        ensure!(original["text"] == "x".repeat(8000), "original text");
        ensure!(original["next_offset"] == 8000, "next offset");
        ensure!(original["attachments"] == json!([]), "attachments");
        ensure!(original["generatedFiles"] == json!([]), "generated files");
        ensure!(original["questions"] == json!([]), "questions");
        tp.write()?.execute("UPDATE turns SET text=?,revision=revision+1 WHERE id=?", [&original_text, &oldest.0])?;
        let tail = run(&mut tp, &home, &mut first, "read_message", json!({"id": oldest.0, "offset": 8000}), false).await?;
        ensure!(tail["text"] == format!("{}ORIGINAL_END", "x".repeat(1000)), "tail text");
        ensure!(tail["next_offset"].is_null(), "tail offset");
        let missing = run(&mut tp, &home, &mut first, "read_message", json!({"id": hidden[0].0}), false).await?;
        ensure!(text(&missing["error"]).contains("not found"), "hidden message: {missing}");
        ensure!(run(&mut tp, &home, &mut first, "read_history", json!({"before": 0}), false).await?["status"] == 400, "before 0");
        ensure!(run(&mut tp, &home, &mut first, "read_message", json!({"id": oldest.0, "offset": -1}), false).await?["status"] == 400, "offset -1");
        for (kind, id) in [("side", &side.0), ("channel", &shared.0)] {
            let mut task = history_task(&tp, &home, kind, &project, &agent, &thread, &channel).await?;
            let result = run(&mut tp, &home, &mut task, "read_history", json!({}), false).await?;
            ensure!(ids(&result["messages"], "id") == vec![json!(id)], "{kind} history: {result}");
        }
        Ok(())
    }
    .await;
    tp.fault(None);
    tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_skill_reads_recover_exact_pinned_text_after_lost_response_disable_and_server_restart(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let home = tp.home("home.sqlite").await?;
        let (project, agent, skill) = (new_id(), new_id(), new_id());
        tp.project(&project, "Skill reads").await?;
        tp.agent(&agent, "Reader", "Read installed skills", &project, &home).await?;
        let owner = account_of(&tp, "user_owner")?;
        let db = tp.write()?;
        let versions = [
            ("pinned-reader", "Read immutable support files", "PINNED_CONTENT"),
            ("latest-reader", "Uninstalled version", "LATEST_PRIVATE"),
        ];
        db.execute("INSERT INTO skills VALUES (?,?,2)", [&skill, versions[1].0])?;
        use minarbot_testkit::base64::Engine;
        for (index, (name, description, content)) in versions.iter().enumerate() {
            let version = (index + 1) as i64;
            db.execute(
                "INSERT INTO skill_versions VALUES (?,?,?,?,?)",
                minarbot_testkit::hosted::params![skill, version, name, description, content],
            )?;
            let mut files = vec![json!({"path": "SKILL.md",
                "content": format!("---\nname: {name}\ndescription: {description}\n---\n{content}")})];
            if index == 0 {
                files.push(json!({"path": "notes.txt", "content": format!("{}ORIGINAL_TAIL", "x".repeat(8000))}));
                files.push(json!({"path": "asset.bin",
                    "content": minarbot_testkit::base64::engine::general_purpose::STANDARD.encode("BINARY_PRIVATE"),
                    "encoding": "base64"}));
            }
            db.execute(
                "INSERT INTO skill_files VALUES (?,?,?)",
                minarbot_testkit::hosted::params![skill, version, json!(files).to_string()],
            )?;
        }
        db.execute("INSERT INTO agent_skills(agent,skill,version,enabled,revision,removed) VALUES (?,?,1,1,1,0)", [&agent, &skill])?;
        for (table, key) in [
            ("skills", json!([skill])),
            ("skill_versions", json!([skill, 1])),
            ("agent_skills", json!([agent, skill])),
            ("skill_files", json!([skill, 1])),
        ] {
            db.execute(
                "INSERT INTO record_scopes VALUES (?,?,?,?,'owner','agent',?)",
                [table, &key.to_string(), &owner, &project, &agent],
            )?;
        }
        drop(db);
        let mut owned = admitted(&tp, &home, &project, &agent, "Inspect pinned skill").await?;
        let listed = run(&mut tp, &home, &mut owned, "list_skills", json!({}), false).await?;
        ensure!(
            listed["items"] == json!([{"id": skill, "version": 1, "name": "pinned-reader", "description": "Read immutable support files"}]),
            "skills: {listed}"
        );
        let read = run(&mut tp, &home, &mut owned, "read_skill", json!({"id": skill}), false).await?;
        ensure!(read["content"] == "PINNED_CONTENT", "content");
        ensure!(read["version"] == 1, "version");
        ensure!(!read.to_string().contains("LATEST_PRIVATE"), "latest content");
        ensure!(!read.to_string().contains("BINARY_PRIVATE"), "binary content");
        let (agent_id, skill_id) = (agent.clone(), skill.clone());
        let file = run_lost(&mut tp, &home, &mut owned, "read_skill_file", json!({"id": skill, "path": "notes.txt", "offset": 8000}), |tp| {
            tp.write()?.execute(
                "UPDATE agent_skills SET enabled=0,revision=revision+1 WHERE agent=? AND skill=?",
                [&agent_id, &skill_id],
            )?;
            Ok(())
        })
        .await?;
        ensure!(file["content"] == "ORIGINAL_TAIL", "file content");
        ensure!(file["next_offset"].is_null(), "file offset");
        let disabled = run(&mut tp, &home, &mut owned, "read_skill", json!({"id": skill}), false).await?;
        ensure!(text(&disabled["error"]).contains("not found"), "disabled: {disabled}");
        ensure!(tp.task_detail(&owned.task).await?["results"].as_array().map(Vec::len) == Some(4), "results");
        Ok(())
    }
    .await;
    tp.fault(None);
    tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_agent_directory_isolates_projects_and_recovers_exact_roster_after_lost_reply(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let home = tp.home("home.sqlite").await?;
        let (project, other, agent, peer, foreign) = (new_id(), new_id(), new_id(), new_id(), new_id());
        for id in [&project, &other] {
            tp.project(id, "Directory").await?;
        }
        for (id, project_id) in [(&agent, &project), (&peer, &project), (&foreign, &other)] {
            let name = if id == &peer { "Original teammate" } else { "Agent" };
            tp.agent(id, name, "Project teammate", project_id, &home).await?;
        }
        let mut owned = admitted(&tp, &home, &project, &agent, "List teammates").await?;
        let context = home.host.context(&owned.task, &owned.ownership, &owned.revision).await?;
        ensure!(
            context["snapshot"]["tools"].as_array().is_some_and(|tools| tools.iter().any(|tool| tool["function"]["name"] == "list_agents")),
            "list_agents offered"
        );
        let cursor = run(&mut tp, &home, &mut owned, "list_agents", json!({"after": peer.to_uppercase()}), false).await?;
        ensure!(cursor == json!({"agents": [], "next_after": null}), "cursor page: {cursor}");
        let (peer_id, project_id) = (peer.clone(), project.clone());
        let input = home.canonical_input(&tp, &mut owned, "list_agents", &json!({})).await?;
        let action = text(&input["actionId"]);
        lost_then(&mut tp, &home, &owned, &input, |tp| {
            let db = tp.write()?;
            db.execute("UPDATE agents SET name='Later teammate',revision=revision+1 WHERE id=?", [&peer_id])?;
            db.execute("UPDATE agent_projects SET deleted=1,revision=revision+1 WHERE project=? AND agent=?", [&project_id, &peer_id])?;
            Ok(())
        })
        .await?;
        let detail = tp.task_detail(&owned.task).await?;
        ensure!(detail["results"].as_array().map(Vec::len) == Some(2), "results");
        ensure!(
            detail["results"][1]["content"]
                == json!({"agents": [{"id": peer, "name": "Original teammate", "role": "", "description": "Project teammate",
                    "status": "idle", "queuedMessages": 0}], "next_after": null}),
            "roster: {}",
            detail["results"][1]["content"]
        );
        let db = tp.inspect()?;
        ensure!(db.scalar::<i64>("SELECT count(*) FROM task_results WHERE action=?", [&action])? == 1, "task results");
        ensure!(db.scalar::<i64>("SELECT count(*) FROM task_reports WHERE action=?", [&action])? == 1, "task reports");
        Ok(())
    }
    .await;
    tp.fault(None);
    tp.close().await?;
    result
}

async fn device_row(tp: &Tp, id: &str) -> Result<Value> {
    tp.data("/api/devices")
        .await?
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == id).cloned())
        .ok_or_else(|| format!("device {id} missing").into())
}

async fn control(tp: &Tp, task: &str, revision: &Value, operation: &str) -> Result<(u16, Value)> {
    tp.send(
        &format!("/api/tasks/{task}/control"),
        Some(json!({"requestId": new_id(), "expectedRevision": revision, "operation": operation})),
    )
    .await
}

#[tokio::test]
async fn canonical_teammate_sends_retain_one_scoped_delivery_on_its_fixed_home_after_lost_reply_and_restart(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let home = tp.home("sender.sqlite").await?;
        let remote = tp.device("recipient.sqlite")?;
        remote.enroll(&tp.jwt("user_owner")?, "new").await?;
        let other_home = device_row(&tp, &remote.device_id()?).await?;
        let (project, agent, peer) = (new_id(), new_id(), new_id());
        tp.project(&project, "Messages").await?;
        tp.agent(&agent, "Sender", "Project teammate", &project, &home).await?;
        ensure!(tp.send("/api/agents", Some(agent_body(&peer, "Recipient", "Project teammate", &project, &other_home))).await?.0 == 201, "recipient");
        let mut owned = admitted(&tp, &home, &project, &agent, "PRIVATE_SENDER_HISTORY").await?;
        let input = home
            .canonical_input(&tp, &mut owned, "send_message", &json!({"recipients": [peer], "text": "Only this text", "expects_reply": false}))
            .await?;
        let action = text(&input["actionId"]);
        lost_then(&mut tp, &home, &owned, &input, |_| Ok(())).await?;
        let detail = tp.task_detail(&owned.task).await?;
        let sent = detail["results"][0]["content"].clone();
        ensure!(sent["status"] == "sent", "status: {sent}");
        ensure!(sent["expects_reply"] == false, "expects_reply");
        let db = tp.inspect()?;
        let children = db.rows("SELECT turn FROM agent_deliveries WHERE request=?", [&text(&sent["request"])])?;
        ensure!(children.len() == 1, "deliveries: {children:?}");
        let child = text(&children[0]["turn"]);
        ensure!(db.count("tasks")? == 2, "tasks");
        ensure!(db.scalar::<i64>("SELECT count(*) FROM task_results WHERE action=?", [&action])? == 1, "results");
        let child_detail = tp.task_detail(&child).await?;
        ensure!(child_detail["homeDeviceId"] == other_home["id"], "child home");
        ensure!(child_detail["state"] == "waiting_device", "child state");
        ensure!(tp.send_as(&format!("/api/tasks/{child}"), None, "user_foreign", None).await?.0 == 404, "foreign child");
        let remote_connection = remote.connect().await?;
        until("remote", SETTLE, || async { Ok(remote_connection.phase() == "connected") }).await?;
        let child_owned = remote.start_task(&child).await?;
        let child_context = remote.context(&child, &child_owned["ownership"], &child_owned["revision"]).await?;
        let conversation = &child_context["snapshot"]["conversation"];
        ensure!(conversation["message"] == json!({"text": "Only this text", "files": []}), "child message");
        ensure!(conversation["history"] == json!([]), "child history");
        ensure!(!child_context["snapshot"].to_string().contains("PRIVATE_SENDER_HISTORY"), "sender history leaked");
        ensure!(
            !child_context["snapshot"]["tools"].as_array().into_iter().flatten().any(|tool| {
                ["send_message", "list_agents", "interrupt_agent"].contains(&tool["function"]["name"].as_str().unwrap_or_default())
            }),
            "child coordination tools"
        );
        let (status, paused) = control(&tp, &owned.task, &detail["revision"], "pause").await?;
        ensure!(status == 200, "pause: {status}");
        let denied = remote.context(&child, &child_owned["ownership"], &child_owned["revision"]).await;
        ensure!(denied.as_ref().is_err_and(|error| is_code(error, "task_not_runnable")), "paused child: {denied:?}");
        ensure!(control(&tp, &child, &child_owned["revision"], "pause").await?.0 == 200, "child pause");
        let (status, resumed) = control(&tp, &owned.task, &paused["data"]["revision"], "resume").await?;
        ensure!(status == 200, "resume: {status}");
        ensure!(tp.task_detail(&child).await?["state"] == "paused", "child stays paused");
        let (status, child_resumed) = control(&tp, &child, &json!(child_owned["revision"].as_i64().unwrap_or(0) + 1), "resume").await?;
        ensure!(status == 200, "child resume: {status}");
        remote.context(&child, &child_owned["ownership"], &child_resumed["data"]["revision"]).await?;
        let continued = home.host.context(&owned.task, &owned.ownership, &resumed["data"]["revision"]).await?;
        ensure!(
            continued["snapshot"]["coordination"].as_array().is_some_and(|rows| rows.iter().any(|row| row["request"] == sent["request"])),
            "coordination: {}",
            continued["snapshot"]["coordination"]
        );
        Ok(())
    }
    .await;
    tp.fault(None);
    tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_setup_creation_retains_one_restricted_task_and_creation_receipt_after_lost_response_and_restart(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let runtime = tp.device("setup-home.sqlite")?;
        runtime.enroll(&tp.jwt("user_owner")?, "new").await?;
        let home = tp.data("/api/devices").await?[0].clone();
        let (project, agent, binding) = (new_id(), new_id(), new_id());
        tp.project(&project, "Setup").await?;
        let db = tp.write()?;
        let device = text(&home["id"]);
        let account: String = db.scalar("SELECT account FROM devices WHERE id=?", [&device])?;
        db.execute(
            "INSERT INTO provider_bindings VALUES (?,?,?,'openai-compatible','synthetic-setup','Synthetic','ready',1)",
            [&binding, &device, &account],
        )?;
        drop(db);
        let mut command = agent_body(&agent, "New agent", "Build a careful research profile.", &project, &home);
        command["providerBinding"] = json!({"bindingId": binding, "expectedRevision": 1});
        command["setup"] = json!(true);
        // The first reply is lost after the server created the agent.
        let (status, lost) = tp.send("/api/agents", Some(command.clone())).await?;
        ensure!(status == 201, "lost creation: {status} {lost}");
        tp.restart().await?;
        let (status, retry) = tp.send("/api/agents", Some(command.clone())).await?;
        ensure!(status == 200, "retry: {status} {retry}");
        let (_, lookup) = tp.send(&format!("/api/tasks/{agent}"), None).await?;
        ensure!(lookup["data"]["state"] == "waiting_device", "setup state");
        ensure!(lookup["receipt"]["requestId"] == agent.as_str(), "receipt request");
        ensure!(lookup["receipt"]["payloadDigest"] == retry["receipt"]["payloadDigest"], "receipt digest");
        let connection = runtime.connect().await?;
        until("home", SETTLE, || async { Ok(connection.phase() == "connected") }).await?;
        let owned = runtime.start_task(&agent).await?;
        let context = runtime.context(&agent, &owned["ownership"], &owned["revision"]).await?;
        let tools = context["snapshot"]["tools"].as_array().cloned().unwrap_or_default();
        let mut names: Vec<String> = tools.iter().map(|tool| text(&tool["function"]["name"])).collect();
        names.sort();
        ensure!(names == ["ask_user", "edit_profile"], "setup tools: {names:?}");
        ensure!(
            tools.iter().find(|tool| tool["function"]["name"] == "edit_profile").map(|tool| tool["function"]["parameters"]["required"].clone())
                == Some(json!(["revision", "name", "instructions"])),
            "edit_profile required"
        );
        let conversation = context["snapshot"]["conversation"].to_string();
        ensure!(conversation.contains("Do not start the actual work"), "setup guard");
        ensure!(conversation.contains("Build a careful research profile"), "setup brief");
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        runtime
            .action(
                &json!({"taskId": agent, "actionId": new_id(), "sequence": 0, "actionKind": "provider",
                    "ownership": owned["ownership"], "payload": {"prompt": "Synthetic setup request"}, "context": context,
                    "grantRevision": 1, "bindingId": binding, "bindingRevision": 1}),
                move |_| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    async { Ok(json!({"name": "Research partner", "instructions": "Compare sources and cite evidence."})) }
                },
            )
            .await?;
        let current = tp.task_detail(&agent).await?;
        let next = runtime.context(&agent, &owned["ownership"], &current["revision"]).await?;
        let edit = new_id();
        runtime
            .action(
                &json!({"taskId": agent, "actionId": edit, "sequence": 1, "actionKind": "canonical",
                    "ownership": owned["ownership"],
                    "payload": {"toolCall": {"id": edit, "function": {"name": "edit_profile",
                        "arguments": json!({"revision": 1, "name": "Research partner",
                            "instructions": "Compare sources and cite evidence."}).to_string()}},
                        "expectedRevision": next["snapshot"]["taskRevision"].as_i64().unwrap_or(0) + 1},
                    "context": next, "grantRevision": 1}),
                |_| async { Err("Host effect".into()) },
            )
            .await?;
        ensure!(tp.send("/api/agents", Some(command)).await?.0 == 200, "creation replay");
        ensure!(calls.load(Ordering::SeqCst) == 1, "provider calls");
        let saved = tp.inspect()?;
        ensure!(saved.scalar::<i64>("SELECT count(*) FROM tasks WHERE id=?", [&agent])? == 1, "tasks");
        ensure!(saved.scalar::<i64>("SELECT count(*) FROM turns WHERE id=?", [&agent])? == 1, "turns");
        ensure!(saved.scalar::<i64>("SELECT count(*) FROM receipts WHERE request_id=?", [&agent])? == 1, "receipts");
        ensure!(saved.scalar::<String>("SELECT name FROM agents WHERE id=?", [&agent])? == "Research partner", "name");
        connection.stop().await?;
        Ok(())
    }
    .await;
    tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_caller_interruption_retains_one_notice_and_fences_remote_deliveries_after_lost_result_and_restart(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let release = Arc::new(tokio::sync::Notify::new());
    let result = async {
        let home = tp.home("caller.sqlite").await?;
        let remote = tp.home("worker.sqlite").await?;
        let (project, agent, peer) = (new_id(), new_id(), new_id());
        tp.project(&project, "Interruption").await?;
        tp.agent(&agent, &agent, "Project teammate", &project, &home).await?;
        tp.agent(&peer, &peer, "Project teammate", &project, &remote).await?;
        let mut parent = admitted(&tp, &home, &project, &agent, "Caller command").await?;
        let send = home
            .canonical_input(&tp, &mut parent, "send_message", &json!({"recipients": [peer], "text": "Work that should stop", "expects_reply": true}))
            .await?;
        home.execute(&send).await?;
        let child: String = tp.inspect()?.scalar(
            "SELECT d.turn FROM agent_deliveries d JOIN agent_requests r ON r.id=d.request WHERE r.turn=?",
            [&parent.task],
        )?;
        let child_owned = remote.host.start_task(&child).await?;
        let child_context = remote.host.context(&child, &child_owned["ownership"], &child_owned["revision"]).await?;
        let started = Arc::new(AtomicBool::new(false));
        let (flag, hold) = (started.clone(), release.clone());
        let worker = remote.host.clone();
        let effect_input = json!({"taskId": child, "actionId": new_id(), "sequence": 0, "actionKind": "read",
            "ownership": child_owned["ownership"], "payload": {"operation": "synthetic read"},
            "context": child_context, "grantRevision": 1});
        let pending = tokio::spawn(async move {
            worker
                .action(&effect_input, move |_| {
                    let (flag, hold) = (flag.clone(), hold.clone());
                    async move {
                        flag.store(true, Ordering::SeqCst);
                        hold.notified().await;
                        Ok(json!({"text": "Accepted effect completed after interruption"}))
                    }
                })
                .await
        });
        until("effect started", SETTLE, || async { Ok(started.load(Ordering::SeqCst)) }).await?;
        let mut stopper = admitted(&tp, &home, &project, &agent, "Caller command").await?;
        let stop = home
            .canonical_input(&tp, &mut stopper, "interrupt_agent", &json!({"recipient": peer, "reason": "Changed direction"}))
            .await?;
        let seen = tp.lose_next("/context-mutations");
        ensure!(home.execute(&stop).await.is_err(), "interruption reply was not lost");
        tp.fault(None);
        ensure!(*locked(&seen) == Some(200), "interruption status");
        ensure!(tp.task_detail(&child).await?["state"] == "cancelled", "child cancelled");
        release.notify_one();
        pending.await??;
        let cancelled = tp.task_detail(&child).await?;
        ensure!(cancelled["state"] == "cancelled", "child still cancelled");
        ensure!(
            cancelled["results"][0]["content"] == json!({"text": "Accepted effect completed after interruption"}),
            "accepted effect: {cancelled}"
        );
        let (caller_generation, worker_generation) = (home.connection.generation(), remote.connection.generation());
        tp.restart().await?;
        until("homes reconnected", SETTLE, || async {
            Ok(home.connection.phase() == "connected"
                && remote.connection.phase() == "connected"
                && home.connection.generation() > caller_generation
                && remote.connection.generation() > worker_generation)
        })
        .await?;
        let stop_action = text(&stop["actionId"]);
        home.acknowledged(&stop_action).await?;
        let detail = tp.task_detail(&stopper.task).await?;
        let interrupted = &detail["results"][0]["content"];
        ensure!(interrupted["status"] == "interrupted", "status: {interrupted}");
        ensure!(interrupted["cancelled"] == json!([child]), "cancelled: {interrupted}");
        let fenced = remote.host.context(&child, &child_owned["ownership"], &child_owned["revision"]).await;
        ensure!(fenced.as_ref().is_err_and(|error| is_code(error, "ownership_conflict")), "fenced child: {fenced:?}");
        let saved = tp.inspect()?;
        let notices = saved.rows(
            "SELECT t.text,r.expects_reply FROM agent_requests r JOIN agent_deliveries d ON d.request=r.id JOIN turns t ON t.id=d.turn WHERE r.turn=?",
            [&stopper.task],
        )?;
        ensure!(notices.len() == 1, "notices: {notices:?}");
        ensure!(notices[0]["expects_reply"] == 0, "notice expects reply");
        ensure!(text(&notices[0]["text"]).contains("Changed direction"), "notice text");
        ensure!(saved.scalar::<i64>("SELECT count(*) FROM task_results WHERE action=?", [&stop_action])? == 1, "stop results");
        let parent_detail = tp.task_detail(&parent.task).await?;
        home.host.context(&parent.task, &parent.ownership, &parent_detail["revision"]).await?;
        Ok(())
    }
    .await;
    release.notify_one();
    tp.fault(None);
    tp.close().await?;
    result
}

async fn profile_task(tp: &Tp, home: &Home, project: &str, agent: &str, agent_revision: i64) -> Result<String> {
    let task = new_id();
    let mut revisions = fresh_revisions();
    revisions["agent"] = json!(agent_revision);
    admit_with(tp, home, &task, project, agent, &direct("Edit profile"), &revisions).await?;
    Ok(task)
}

#[tokio::test]
async fn canonical_profile_edits_recover_exact_result_without_replacing_later_edits_or_current_task_instructions(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let home = tp.home("home.sqlite").await?;
        let (project, agent) = (new_id(), new_id());
        tp.project(&project, "Profile edits").await?;
        tp.agent(&agent, "Reader", "Original instructions", &project, &home).await?;
        let task = profile_task(&tp, &home, &project, &agent, 1).await?;
        let mut owned = home.start(&task).await?;
        let context = home.host.context(&task, &owned.ownership, &owned.revision).await?;
        let input = home
            .canonical_input(&tp, &mut owned, "edit_profile", &json!({"revision": 1, "name": "Edited", "instructions": "Saved instructions"}))
            .await?;
        let agent_id = agent.clone();
        lost_then(&mut tp, &home, &owned, &input, |tp| {
            tp.write()?.execute(
                "UPDATE agents SET name='Later edit',instructions='Later instructions',revision=revision+1 WHERE id=?",
                [&agent_id],
            )?;
            Ok(())
        })
        .await?;
        let detail = tp.task_detail(&task).await?;
        ensure!(detail["results"].as_array().map(Vec::len) == Some(1), "results");
        ensure!(
            detail["results"][0]["content"] == json!({"name": "Edited", "role": "", "instructions": "Saved instructions", "revision": 2}),
            "edit result: {}",
            detail["results"][0]["content"]
        );
        let continued = home.host.context(&task, &owned.ownership, &detail["revision"]).await?;
        ensure!(continued["snapshot"]["profile"] == context["snapshot"]["profile"], "current task profile");
        ensure!(
            continued["snapshot"]["sourceRevisions"].as_array().and_then(|rows| rows.iter().find(|row| row["kind"] == "agent").map(|row| row["revision"].clone()))
                == Some(json!(3)),
            "agent source revision"
        );
        let next = profile_task(&tp, &home, &project, &agent, 3).await?;
        let next_owned = home.host.start_task(&next).await?;
        let fresh = home.host.context(&next, &next_owned["ownership"], &next_owned["revision"]).await?;
        let profile = &fresh["snapshot"]["profile"];
        ensure!(profile["instructions"] == "Later instructions", "fresh instructions");
        ensure!(profile["name"] == "Later edit", "fresh name");
        ensure!(profile["revision"] == 3, "fresh revision");
        Ok(())
    }
    .await;
    tp.fault(None);
    tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_provider_settings_use_home_binding_metadata_and_recover_saved_result_after_lost_reply(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let home = tp.home("home.sqlite").await?;
        let (project, agent, binding) = (new_id(), new_id(), new_id());
        tp.project(&project, "Provider reads").await?;
        let mut body = agent_body(&agent, "Reader", "Read settings", &project, &home.device);
        body["backupModel"] = json!("backup");
        ensure!(tp.send("/api/agents", Some(body)).await?.0 == 201, "agent");
        let owner = account_of(&tp, "user_owner")?;
        let db = tp.write()?;
        db.execute(
            "INSERT INTO provider_bindings VALUES (?,?,?,'openai-compatible','bound-account','Owner','ready',1)",
            [&binding, &home.id(), &owner],
        )?;
        db.execute("UPDATE agent_authority SET provider_binding=? WHERE agent=?", [&binding, &agent])?;
        // #87 dropped `agents.provider_account`; a database that still carries
        // the legacy column must not leak it (the Node case failed at base).
        db.execute_batch("ALTER TABLE agents ADD COLUMN provider_account TEXT")?;
        db.execute("UPDATE agents SET provider_account='LEGACY_MUST_NOT_LEAK' WHERE id=?", [&agent])?;
        drop(db);
        let mut owned = admitted(&tp, &home, &project, &agent, "Read provider settings").await?;
        let input = home.canonical_input(&tp, &mut owned, "read_provider_settings", &json!({})).await?;
        let binding_id = binding.clone();
        lost_then(&mut tp, &home, &owned, &input, |tp| {
            tp.write()?.execute(
                "UPDATE provider_bindings SET provider_account_id='Changed',readiness='revoked',revision=2 WHERE id=?",
                [&binding_id],
            )?;
            Ok(())
        })
        .await?;
        let results = tp.task_detail(&owned.task).await?["results"].clone();
        ensure!(results.as_array().map(Vec::len) == Some(1), "results");
        // #87 removed fallback models (`agents.backup_model`); the Node case
        // expected the creation's `backupModel: "backup"` here.
        ensure!(
            results[0]["content"]
                == json!({"revision": 1, "providerAccount": "bound-account", "model": "fixture", "backupModel": "", "configured": true}),
            "settings: {}",
            results[0]["content"]
        );
        Ok(())
    }
    .await;
    tp.fault(None);
    tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_routine_reads_and_edits_isolate_project_configuration_and_recover_original_flow_after_lost_reply(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let home = tp.home("home.sqlite").await?;
        let (project, other, agent, routine, hidden) = (new_id(), new_id(), new_id(), new_id(), new_id());
        for id in [&project, &other] {
            tp.project(id, id).await?;
        }
        tp.agent(&agent, "Routine reader", "Read project routines", &project, &home).await?;
        let graph = json!({"root": agent, "nodes": [agent], "links": []});
        let owner = account_of(&tp, "user_owner")?;
        let db = tp.write()?;
        db.execute_batch("ALTER TABLE routines ADD COLUMN secret TEXT")?;
        for (id, project_id) in [(&routine, &project), (&hidden, &other)] {
            db.execute(
                "INSERT INTO routines(id,agent,name,prompt,next_at,interval_ms,enabled,revision,trigger,secret) VALUES (?,?,'Routine','Original prompt',123456,NULL,1,1,'webhook','WEBHOOK_SECRET')",
                [id, &agent],
            )?;
            db.execute(
                "INSERT INTO record_scopes VALUES ('routines',?,?,?,'owner','agent',?)",
                [&json!([id]).to_string(), &owner, project_id, &agent],
            )?;
        }
        db.execute("INSERT INTO routine_flows VALUES (?,?,1)", [&routine, &graph.to_string()])?;
        db.execute(
            "INSERT INTO record_scopes VALUES ('routine_flows',?,?,?,'owner','agent',?)",
            [&json!([routine]).to_string(), &owner, &project, &agent],
        )?;
        drop(db);
        let mut owned = admitted(&tp, &home, &project, &agent, "Read routine").await?;
        let change = |routine: String| {
            move |tp: &Tp| -> Result<()> {
                let db = tp.write()?;
                db.execute("UPDATE routines SET prompt='Changed prompt',revision=revision+1 WHERE id=?", [&routine])?;
                db.execute("UPDATE routine_flows SET graph='{}',revision=revision+1 WHERE routine=?", [&routine])?;
                Ok(())
            }
        };
        let list = run(&mut tp, &home, &mut owned, "list_routines", json!({}), false).await?;
        ensure!(ids(&list["routines"], "id") == vec![json!(routine)], "routines: {list}");
        ensure!(list["next_after"].is_null(), "next_after");
        ensure!(!list.to_string().contains("WEBHOOK_SECRET"), "secret listed");
        let read = run_lost(&mut tp, &home, &mut owned, "read_routine", json!({"id": routine}), change(routine.clone())).await?;
        ensure!(read["prompt"] == "Original prompt", "prompt");
        ensure!(read["flow"] == json!({"graph": graph, "revision": 1}), "flow: {read}");
        ensure!(!read.to_string().contains("WEBHOOK_SECRET"), "secret read");
        let hidden_read = run(&mut tp, &home, &mut owned, "read_routine", json!({"id": hidden}), false).await?;
        ensure!(text(&hidden_read["error"]).contains("not found"), "hidden: {hidden_read}");
        let edit = run_lost(&mut tp, &home, &mut owned, "set_routine_flow", json!({"id": routine, "revision": 2, "links": []}), change(routine.clone())).await?;
        ensure!(edit == json!({"graph": graph, "revision": 3}), "edit: {edit}");
        let db = tp.inspect()?;
        ensure!(db.scalar::<i64>("SELECT revision FROM routine_flows WHERE routine=?", [&routine])? == 4, "flow revision");
        ensure!(db.scalar::<i64>("SELECT count(*) FROM task_results WHERE task=?", [&owned.task])? == 4, "results");
        Ok(())
    }
    .await;
    tp.fault(None);
    tp.close().await?;
    result
}
