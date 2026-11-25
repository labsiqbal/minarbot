//! Canonical workflow configuration, pages and the result inbox.

use super::{
    fixture::{account, agent_body, encode},
    routines::{raw_as, text},
    workflow_waits::WorkflowHome,
    workflows::{create, wait_run},
};
use minarbot_testkit::{http::Method, prelude::*};

fn with(command: &Value, fields: Value) -> Value {
    let mut command = command.clone();
    for (key, value) in fields.as_object().into_iter().flatten() {
        command[key] = value.clone();
    }
    command
}

fn ids(items: &Value, field: &str) -> Vec<Value> {
    items
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| row[field].clone())
        .collect()
}

/// Add the collaborator as a member assigned to the workflow agent.
async fn assigned_collaborator(w: &WorkflowHome, response_off: bool) -> Result<String> {
    let collaborator = account(&w.tp, "user_collaborator").await?;
    let db = w.tp.write()?;
    db.execute("INSERT INTO project_members VALUES (?,?,'member',1,0)", [&w.project, &collaborator])?;
    db.execute("INSERT INTO collaborator_agents VALUES (?,?,?,1,1)", [&w.project, &collaborator, &w.agent])?;
    if response_off {
        db.execute("INSERT INTO response_policies VALUES (?,?,?,0,1)", [&w.project, &collaborator, &w.agent])?;
    }
    Ok(collaborator)
}

#[tokio::test]
async fn canonical_workflow_configuration_stays_project_scoped_with_immutable_versions_and_retry_receipts(
) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        let graph = w.graph.clone();
        let command = json!({"requestId": w.workflow, "id": w.workflow, "revision": 0,
            "name": "Original", "graph": graph});
        let (status, created) = w.tp.send(&w.list, Some(command.clone())).await?;
        ensure!(status == 201, "create: {status} {created}");
        ensure!(created["data"]["projectId"] == w.project.as_str(), "project");
        ensure!(created["data"]["homeDeviceId"] == w.home["id"], "home");
        let (status, replay) = w.tp.send(&w.list, Some(command.clone())).await?;
        ensure!(status == 200, "replay: {status}");
        ensure!(replay["receipt"] == created["receipt"], "replay receipt");
        ensure!(w.tp.data(&w.detail).await?["graph"] == graph, "graph");
        let other_list = format!("/api/projects/{}/workflows", w.other);
        ensure!(w.tp.data(&other_list).await?["items"] == json!([]), "other list");
        let other_detail = format!("/api/projects/{}/workflows/{}", w.other, w.workflow);
        ensure!(w.tp.send(&other_detail, None).await?.0 == 404, "other detail");
        let (status, _) = w
            .tp
            .send_as(
                &format!("/api/projects/{}/agents/{}", w.other, w.agent),
                Some(json!({"requestId": new_id(), "expectedProjectRevision": 1,
                    "expectedAgentRevision": 1, "expectedAuthorityRevision": 1,
                    "expectedParticipationRevision": 0, "rules": "Separate",
                    "capabilities": {"v": 1, "tools": [], "resources": []}})),
                "user_owner",
                Some(Method::PUT),
            )
            .await?;
        ensure!(status == 201, "participation: {status}");
        let other_workflow = new_id();
        ensure!(
            w.tp.send(
                &format!("/api/projects/{}/agents/{}/workflows", w.other, w.agent),
                Some(with(&command, json!({"requestId": other_workflow, "id": other_workflow,
                    "name": "Other project"})))
            )
            .await?
            .0 == 201,
            "other workflow"
        );
        ensure!(
            ids(&w.tp.data(&other_list).await?["items"], "id") == vec![json!(other_workflow)],
            "other list ids"
        );
        ensure!(
            w.tp.send(&other_detail, None).await?.0 == 404,
            "Same agent participation cannot transfer workflows"
        );
        ensure!(w.tp.send(&format!("/api/workflows/{}", w.workflow), None).await?.0 == 404, "unscoped");
        for path in [
            "/api/workflows?deleted=1".to_owned(),
            format!("/api/agents/{}/workflows?after={}", w.agent, w.workflow),
            format!("/api/workflows/{}/versions?before=2", w.workflow),
            format!("{}/run?before=2", w.detail),
        ] {
            ensure!(w.tp.send(&path, None).await?.0 == 404, "{path}");
        }
        let edit_id = new_id();
        let mut changed = graph.clone();
        changed["maxSteps"] = json!(2);
        let edited = json!({"requestId": edit_id, "id": edit_id, "revision": 1, "name": "Edited",
            "graph": changed});
        let (status, saved) = w.tp.send(&format!("{}/edit", w.detail), Some(edited.clone())).await?;
        ensure!(status == 200, "edit: {status} {saved}");
        w.tp.restart().await?;
        ensure!(
            w.tp.send(&format!("{}/edit", w.detail), Some(edited)).await?.1["receipt"] == saved["receipt"],
            "edit replay"
        );
        let versions = w.tp.data(&format!("{}/versions", w.detail)).await?["items"].clone();
        ensure!(ids(&versions, "revision") == vec![json!(2), json!(1)], "versions: {versions}");
        ensure!(versions[1]["graph"] == graph, "old graph");
        let restore = new_id();
        ensure!(
            w.tp.send(
                &format!("{}/restore", w.detail),
                Some(json!({"requestId": restore, "id": restore, "revision": 2, "version": 1}))
            )
            .await?
            .1["data"]["revision"]
                == 3,
            "restore"
        );
        let deletion = new_id();
        let delete = json!({"requestId": deletion, "id": deletion, "revision": 3});
        let (status, deleted) = w.tp.send(&format!("{}/delete", w.detail), Some(delete.clone())).await?;
        ensure!(status == 200, "delete: {status}");
        ensure!(
            w.tp.send(&format!("{}/delete", w.detail), Some(delete)).await?.1["receipt"] == deleted["receipt"],
            "delete replay"
        );
        let list = format!("/api/projects/{}/workflows", w.project);
        ensure!(w.tp.data(&list).await?["items"] == json!([]), "deleted listed");
        ensure!(
            w.tp.data(&format!("{list}?deleted=1")).await?["items"][0]["id"] == w.workflow.as_str(),
            "deleted list"
        );
        let run = new_id();
        ensure!(
            w.tp.send(
                &format!("{}/run", w.detail),
                Some(json!({"requestId": run, "id": run, "revision": 3, "input": "Must not execute"}))
            )
            .await?
            .0 == 404,
            "run deleted"
        );
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_workflows_enforce_current_assignments_private_mappings_selectors_and_atomic_receipt_storage(
) -> Result<()> {
    let w = WorkflowHome::start().await?;
    let result = async {
        let graph = w.graph.clone();
        let command = json!({"requestId": w.workflow, "id": w.workflow, "revision": 0,
            "name": "Shared", "graph": graph});
        let db = w.tp.write()?;
        ensure!(w.tp.send(&w.list, Some(command.clone())).await?.0 == 201, "create");
        let collaborator = account(&w.tp, "user_collaborator").await?;
        let as_member = |path: String, body: Option<Value>| {
            let tp = &w.tp;
            async move { tp.send_as(&path, body, "user_collaborator", None).await }
        };
        let list = format!("/api/projects/{}/workflows", w.project);
        db.execute("INSERT INTO project_members VALUES (?,?,'member',1,0)", [&w.project, &collaborator])?;
        ensure!(as_member(w.detail.clone(), None).await?.0 == 404, "unassigned detail");
        ensure!(as_member(list.clone(), None).await?.1["data"]["items"] == json!([]), "unassigned list");
        db.execute("INSERT INTO collaborator_agents VALUES (?,?,?,1,1)", [&w.project, &collaborator, &w.agent])?;
        db.execute("INSERT INTO response_policies VALUES (?,?,?,0,1)", [&w.project, &collaborator, &w.agent])?;
        ensure!(
            as_member(w.detail.clone(), None).await?.0 == 200,
            "Response OFF does not remove workflow assignment"
        );
        let edit_id = new_id();
        let edit = json!({"requestId": edit_id, "id": edit_id, "revision": 1,
            "name": "Collaborator edit", "graph": graph});
        let edit_path = format!("{}/edit", w.detail);
        ensure!(as_member(edit_path.clone(), Some(edit.clone())).await?.0 == 200, "member edit");
        db.execute(
            "UPDATE collaborator_agents SET enabled=0,revision=revision+1 WHERE project=? AND account=? AND agent=?",
            [&w.project, &collaborator, &w.agent],
        )?;
        ensure!(
            as_member(edit_path.clone(), Some(edit.clone())).await?.0 == 404,
            "Revoked assignment cannot replay a receipt"
        );
        db.execute(
            "UPDATE collaborator_agents SET enabled=1,revision=revision+1 WHERE project=? AND account=? AND agent=?",
            [&w.project, &collaborator, &w.agent],
        )?;
        let owner = account(&w.tp, "user_owner").await?;
        let key = json!([w.workflow]).to_string();
        db.execute("UPDATE record_scopes SET visibility='owner' WHERE table_name='workflows' AND key_json=?", [&key])?;
        ensure!(as_member(w.detail.clone(), None).await?.0 == 404, "private workflow");
        db.execute("UPDATE record_scopes SET visibility='project' WHERE table_name='workflows' AND key_json=?", [&key])?;
        let version = |revision: i64| json!([w.workflow, revision]).to_string();
        let move_version = |project: &str, revision: i64| {
            db.execute(
                "UPDATE record_scopes SET project=? WHERE table_name='workflow_versions' AND key_json=?",
                [project, &version(revision)],
            )
        };
        move_version(&w.other, 2)?;
        ensure!(w.tp.send(&w.detail, None).await?.0 == 404, "Unmapped version does not return graph");
        move_version(&w.project, 2)?;
        move_version(&w.other, 1)?;
        ensure!(
            w.tp.send(&w.list, Some(command.clone())).await?.0 == 404,
            "Receipt cannot return a graph whose version scope was removed"
        );
        move_version(&w.project, 1)?;
        for query in ["projectId=".to_owned() + &w.project, "after=x".into(), "after=".into(), "deleted=x".into(), "deleted=0&deleted=0".into()] {
            let path = if query == "after=" { w.detail.clone() } else { list.clone() };
            let (status, denied) = w.tp.send(&format!("{path}?{query}"), None).await?;
            ensure!(status == 404, "{query}: {status} {denied}");
        }
        let (status, _, _) = raw_as(
            &w.tp,
            "user_owner",
            Method::GET,
            &w.detail,
            &[("X-Minarbot-Request-Id", &new_id()), ("X-Minarbot-Project-Id", &w.project)],
            None,
        )
        .await?;
        ensure!(status == 404, "project selector header: {status}");
        ensure!(
            w.tp.send(&edit_path, Some(with(&edit, json!({"projectId": w.project})))).await?.0 == 404,
            "projectId body"
        );
        ensure!(
            w.tp.send(&edit_path, Some(with(&edit, json!({"requestId": new_id(), "id": new_id()})))).await?.0 == 400,
            "mismatched ids"
        );
        ensure!(w.tp.send(&w.list, Some(with(&command, json!({"name": "Different"})))).await?.0 == 409, "changed name");
        ensure!(
            w.tp.send(&w.list, Some(with(&command, json!({"name": " Shared "})))).await?.0 == 200,
            "Digest uses normalized feature name"
        );
        let bad = new_id();
        let mut nodes = vec![json!({"id": "work", "kind": "agent", "agent": bad, "prompt": "foreign", "next": "out"})];
        nodes.extend(graph["nodes"].as_array().cloned().unwrap_or_default());
        let foreign = json!({"entry": "work", "maxSteps": 2, "nodes": nodes});
        ensure!(
            w.tp.send(&w.list, Some(with(&command, json!({"id": bad, "requestId": bad, "graph": foreign})))).await?.0 == 400,
            "unknown graph agent"
        );
        let teammate = new_id();
        ensure!(
            w.tp.send("/api/agents", Some(agent_body(&teammate, "Other agent", "Help", &w.project, &w.home))).await?.0 == 201,
            "teammate"
        );
        let mut teammate_graph = foreign.clone();
        teammate_graph["nodes"][0]["agent"] = json!(teammate);
        ensure!(
            as_member(w.list.clone(), Some(with(&command, json!({"id": bad, "requestId": bad, "graph": teammate_graph})))).await?.0 == 404,
            "Every graph agent needs current assignment"
        );
        let channel = new_id();
        db.execute(
            "INSERT INTO channels(id,name,instructions,lead,revision,archived) VALUES (?,?,'',?,1,0)",
            [&channel, "Other project", &w.agent],
        )?;
        db.execute("INSERT INTO channel_members VALUES (?,?)", [&channel, &w.agent])?;
        db.execute(
            "INSERT INTO record_scopes VALUES ('channels',?,?,?,'project','channel',?)",
            [&json!([channel]).to_string(), &owner, &w.other, &channel],
        )?;
        let mut channel_graph = graph.clone();
        channel_graph["nodes"] = json!([{"id": "out", "kind": "output", "channel": channel}]);
        ensure!(
            w.tp.send(&w.list, Some(with(&command, json!({"id": bad, "requestId": bad, "graph": channel_graph})))).await?.0 == 404,
            "foreign channel output"
        );
        let change_id = new_id();
        let change = json!({"requestId": change_id, "id": change_id, "revision": 2, "name": "Atomic", "graph": graph});
        db.execute_batch("CREATE TRIGGER fail_workflow_receipt BEFORE INSERT ON receipts WHEN NEW.scope LIKE 'retained:%/workflows/%/edit' BEGIN SELECT RAISE(ABORT,'workflow receipt failure'); END;")?;
        ensure!(w.tp.send(&edit_path, Some(change.clone())).await?.0 == 500, "receipt failure");
        ensure!(w.tp.data(&w.detail).await?["name"] == "Collaborator edit", "edit rolled back");
        ensure!(
            ids(&w.tp.data(&format!("{}/versions", w.detail)).await?["items"], "revision") == vec![json!(2), json!(1)],
            "versions rolled back"
        );
        db.execute_batch("DROP TRIGGER fail_workflow_receipt")?;
        ensure!(w.tp.send(&edit_path, Some(change)).await?.0 == 200, "edit");
        ensure!(db.scalar::<i64>("SELECT count(*) FROM receipts WHERE request_id=?", [&change_id])? == 1, "receipt");
        db.execute_batch("CREATE TRIGGER fail_workflow_mapping BEFORE INSERT ON record_scopes WHEN NEW.table_name='workflow_versions' BEGIN SELECT RAISE(ABORT,'workflow mapping failure'); END;")?;
        ensure!(
            w.tp.send(&w.list, Some(with(&command, json!({"requestId": bad, "id": bad})))).await?.0 == 500,
            "mapping failure"
        );
        ensure!(
            w.tp.send(&format!("/api/projects/{}/workflows/{bad}", w.project), None).await?.0 == 404,
            "partial workflow readable"
        );
        ensure!(
            w.tp.data(&list).await?["items"].as_array().map(Vec::len) == Some(1),
            "Failed mapping leaves no partial workflow"
        );
        db.execute_batch("DROP TRIGGER fail_workflow_mapping")?;
        ensure!(
            w.tp.send(&w.list, Some(with(&command, json!({"requestId": bad, "id": bad})))).await?.0 == 201,
            "create after failure"
        );
        ensure!(
            db.scalar::<i64>("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN('workflow_webhooks','workflow_schedules')", [])? == 0,
            "Canonical startup never installs legacy schedule tables"
        );
        ensure!(
            !db.rows("PRAGMA table_info(workflow_waits)", [])?.iter().any(|row| row["name"] == "secret"),
            "Canonical waits contain protected references only"
        );
        db.execute(
            "UPDATE project_members SET deleted=1,grant_revision=grant_revision+1 WHERE project=? AND account=?",
            [&w.project, &collaborator],
        )?;
        ensure!(as_member(w.detail.clone(), None).await?.0 == 404, "removed member");
        ensure!(w.tp.send_as(&w.detail, None, "user_stranger", None).await?.0 == 404, "stranger");
        ensure!(
            db.scalar::<String>("SELECT account FROM projects WHERE id=?", [&w.project])? == owner,
            "project owner"
        );
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_workflow_pages_bound_graph_bytes_and_preserve_list_and_version_cursors(
) -> Result<()> {
    let w = WorkflowHome::start().await?;
    let result = async {
        let graph = w.graph.clone();
        let mut all = vec![w.workflow.clone()];
        all.extend((0..22).map(|_| new_id()));
        for id in &all {
            let (status, _) = w
                .tp
                .send(&w.list, Some(json!({"requestId": id, "id": id, "revision": 0, "name": "Paged", "graph": graph})))
                .await?;
            ensure!(status == 201, "workflow: {status}");
        }
        let list = format!("/api/projects/{}/workflows", w.project);
        let first = w.tp.data(&list).await?;
        ensure!(first["items"].as_array().map(Vec::len) == Some(20), "first page");
        ensure!(!first["nextCursor"].is_null(), "first cursor");
        let second = w.tp.data(&format!("{list}?after={}", text(&first["nextCursor"]))).await?;
        ensure!(second["items"].as_array().map(Vec::len) == Some(3), "second page");
        ensure!(second["nextCursor"].is_null(), "second cursor");
        let mut seen: Vec<String> = ids(&first["items"], "id")
            .into_iter()
            .chain(ids(&second["items"], "id"))
            .map(|id| text(&id))
            .collect();
        seen.sort();
        seen.dedup();
        ensure!(seen.len() == 23, "unique workflows: {}", seen.len());
        let mut nodes = vec![json!({"id": "work", "kind": "agent", "agent": w.agent,
            "prompt": "🦉".repeat(7000), "next": "out"})];
        nodes.extend(graph["nodes"].as_array().cloned().unwrap_or_default());
        let large = json!({"entry": "work", "maxSteps": 2, "nodes": nodes});
        for revision in 1..=10 {
            let id = new_id();
            let (status, _) = w
                .tp
                .send(
                    &format!("{}/edit", w.detail),
                    Some(json!({"requestId": id, "id": id, "revision": revision,
                        "name": "Large versions", "graph": large})),
                )
                .await?;
            ensure!(status == 200, "edit {revision}: {status}");
        }
        let mut versions = Vec::new();
        let mut before = String::new();
        loop {
            let suffix = if before.is_empty() { String::new() } else { format!("?before={before}") };
            let (status, body) = w.tp.send(&format!("{}/versions{suffix}", w.detail), None).await?;
            ensure!(status == 200, "versions: {status}");
            ensure!(body.to_string().len() <= 256 * 1024, "page bytes: {}", body.to_string().len());
            versions.extend(ids(&body["data"]["items"], "revision"));
            before = text(&body["data"]["nextCursor"]);
            if before.is_empty() {
                break;
            }
        }
        ensure!(versions == (1..=11).rev().map(|n| json!(n)).collect::<Vec<_>>(), "versions: {versions:?}");
        for query in ["before=0".to_owned(), "before=-1".into(), "before=abc".into(), "before=1&before=2".into(), format!("projectId={}", w.project)] {
            ensure!(w.tp.send(&format!("{}/versions?{query}", w.detail), None).await?.0 == 404, "{query}");
        }
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

fn inbox(w: &WorkflowHome) -> String {
    format!("/api/projects/{}/workflow-notifications", w.project)
}

async fn sync_from(w: &WorkflowHome, subject: &str, cursor: &Value) -> Result<(u16, Value)> {
    w.tp.send_as(
        &format!("/api/sync?projectId={}&cursor={}", w.project, encode(&text(cursor))),
        None,
        subject,
        None,
    )
    .await
}

async fn sync_cursor(w: &WorkflowHome, subject: &str) -> Result<Value> {
    Ok(w.tp
        .send_as(&format!("/api/sync?projectId={}", w.project), None, subject, None)
        .await?
        .1["data"]["nextCursor"]
        .clone())
}

/// Create the inbox workflow and run it to `done`.
async fn done_run(w: &WorkflowHome, name: &str, input: &str) -> Result<(String, String)> {
    create(w, name, &w.graph).await?;
    let run = new_id();
    let path = format!("/api/projects/{}/workflow-runs/{run}", w.project);
    let (status, _) = w
        .tp
        .send(&format!("{}/run", w.detail), Some(json!({"requestId": run, "id": run, "revision": 1, "input": input})))
        .await?;
    ensure!(status == 201, "run: {status}");
    wait_run(w, &path, |run| run["status"] == "done").await?;
    Ok((run, path))
}

#[tokio::test]
async fn canonical_workflow_inbox_keeps_read_state_per_authorized_human_across_retry_restart_and_deletion(
) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        let inbox = inbox(&w);
        assigned_collaborator(&w, true).await?;
        let (status, empty) = w.tp.send(&inbox, None).await?;
        ensure!(status == 200, "empty inbox: {status}");
        ensure!(empty["data"]["items"] == json!([]), "empty items");
        let (run, run_path) = done_run(&w, "Reader scoped", "Private result canary").await?;
        for subject in ["user_owner", "user_collaborator"] {
            let (status, page) = w.tp.send_as(&inbox, None, subject, None).await?;
            ensure!(status == 200, "{subject} inbox: {status}");
            ensure!(page["data"]["unread"] == 1, "{subject} unread");
            ensure!(page["data"]["items"][0]["run"] == run.as_str(), "{subject} run");
            ensure!(page["data"]["items"][0]["read"] == 0, "{subject} read");
            ensure!(!page.to_string().contains("Private result canary"), "{subject} result text");
        }
        let owner_cursor = sync_cursor(&w, "user_owner").await?;
        let member_cursor = sync_cursor(&w, "user_collaborator").await?;
        let id = new_id();
        let command = json!({"requestId": id, "id": id});
        let read_path = format!("{inbox}/{run}/read");
        let db = w.tp.write()?;
        db.execute_batch(&format!("CREATE TRIGGER fail_notice_receipt BEFORE INSERT ON receipts WHEN NEW.request_id='{id}' BEGIN SELECT RAISE(ABORT,'synthetic read receipt failure'); END"))?;
        ensure!(w.tp.send(&read_path, Some(command.clone())).await?.0 == 500, "receipt failure");
        ensure!(w.tp.data(&inbox).await?["unread"] == 1, "unread after failure");
        ensure!(
            sync_from(&w, "user_owner", &owner_cursor).await?.1["data"]["page"]["events"] == json!([]),
            "events after failure"
        );
        db.execute_batch("DROP TRIGGER fail_notice_receipt")?;
        let (status, accepted) = w.tp.send(&read_path, Some(command.clone())).await?;
        ensure!(status == 200, "read: {status}");
        ensure!(
            w.tp.send(&read_path, Some(command.clone())).await?.1["receipt"] == accepted["receipt"],
            "read replay"
        );
        ensure!(w.tp.data(&inbox).await?["unread"] == 0, "owner unread");
        ensure!(
            w.tp.send_as(&inbox, None, "user_collaborator", None).await?.1["data"]["unread"] == 1,
            "member unread"
        );
        let owner_delta = sync_from(&w, "user_owner", &owner_cursor).await?.1["data"].clone();
        let delta: Vec<Value> = owner_delta["page"]["events"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|event| json!([event["recordKind"], event["recordId"], event["revision"], event["body"]["read"]]))
            .collect();
        ensure!(delta == vec![json!(["workflow_notification", run, 2, true])], "owner delta: {delta:?}");
        ensure!(
            sync_from(&w, "user_collaborator", &member_cursor).await?.1["data"]["page"]["events"] == json!([]),
            "member delta"
        );
        let second = new_id();
        ensure!(
            w.tp.send(&read_path, Some(json!({"requestId": second, "id": second}))).await?.0 == 200,
            "second read"
        );
        ensure!(
            sync_from(&w, "user_owner", &owner_delta["nextCursor"]).await?.1["data"]["page"]["events"] == json!([]),
            "second read is quiet"
        );
        ensure!(
            w.tp.send_as(&read_path, Some(command.clone()), "user_collaborator", None).await?.0 == 404,
            "member replays owner receipt"
        );
        drop(db);
        w.tp.restart().await?;
        ensure!(
            w.tp.send(&read_path, Some(command)).await?.1["receipt"] == accepted["receipt"],
            "replay after restart"
        );
        let delete = new_id();
        ensure!(
            w.tp.send(&format!("{}/delete", w.detail), Some(json!({"requestId": delete, "id": delete, "revision": 1})))
                .await?
                .0
                == 200,
            "delete"
        );
        let retained = w.tp.data(&inbox).await?;
        ensure!(retained["items"].as_array().map(Vec::len) == Some(1), "retained notice");
        ensure!(retained["items"][0]["read"] == 1, "retained read state");
        ensure!(w.tp.data(&run_path).await?["last"] == "Private result canary", "run result");
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn canonical_workflow_inbox_hides_foreign_selectors_and_rechecks_current_reader_scopes_before_receipt_replay(
) -> Result<()> {
    let w = WorkflowHome::start().await?;
    let result = async {
        let inbox = inbox(&w);
        let collaborator = assigned_collaborator(&w, false).await?;
        let (run, _) = done_run(&w, "Scoped inbox", "Scoped result").await?;
        let read_path = format!("{inbox}/{run}/read");
        let id = new_id();
        let command = json!({"requestId": id, "id": id});
        let as_member = |path: String, body: Option<Value>| {
            let tp = &w.tp;
            async move { tp.send_as(&path, body, "user_collaborator", None).await }
        };
        ensure!(as_member(read_path.clone(), Some(command.clone())).await?.0 == 200, "member read");
        for path in [
            "/api/workflow-notifications".to_owned(),
            "/api/workflow-notifications?before=2".into(),
            format!("{inbox}?projectId={}", w.project),
            format!("{inbox}?before=0"),
            format!("{inbox}?before=-1"),
            format!("{inbox}?before=1.5"),
            format!("{inbox}?before=9007199254740992"),
            format!("{inbox}?before=2&before=3"),
            format!("{inbox}/bad/read"),
            format!("{read_path}?before=2"),
        ] {
            let body = (path == format!("{read_path}?before=2")).then(|| command.clone());
            let (status, response) = w.tp.send(&path, body).await?;
            ensure!(status == 404, "{path}: {status}");
            ensure!(response["error"]["code"] == "resource_not_found", "{path}: {response}");
        }
        for path in [read_path.replace(&w.project, &w.other), read_path.replace(&run, &new_id())] {
            ensure!(w.tp.send(&path, Some(command.clone())).await?.0 == 404, "{path}");
        }
        ensure!(w.tp.send_as(&inbox, None, "user_stranger", None).await?.0 == 404, "stranger");
        ensure!(w.tp.data(&inbox.replace(&w.project, &w.other)).await?["items"] == json!([]), "other inbox");
        ensure!(w.tp.send(&read_path, Some(with(&command, json!({"projectId": w.project})))).await?.0 == 404, "projectId");
        ensure!(w.tp.send(&read_path, Some(with(&command, json!({"read": true})))).await?.0 == 400, "extra field");
        ensure!(w.tp.send(&read_path, Some(with(&command, json!({"id": new_id()})))).await?.0 == 400, "mismatched id");
        for header in ["X-Minarbot-Project", "X-Minarbot-Project-Id"] {
            let (status, _, _) = raw_as(
                &w.tp,
                "user_owner",
                Method::GET,
                &inbox,
                &[("X-Minarbot-Request-Id", &new_id()), (header, &w.project)],
                None,
            )
            .await?;
            ensure!(status == 404, "{header}: {status}");
        }
        let db = w.tp.write()?;
        for (table, key) in [
            ("workflows", json!([w.workflow])),
            ("workflow_versions", json!([w.workflow, 1])),
            ("workflow_runs", json!([run])),
        ] {
            let before = sync_cursor(&w, "user_collaborator").await?;
            db.execute(
                "UPDATE record_scopes SET visibility='owner' WHERE table_name=? AND key_json=?",
                [table, &key.to_string()],
            )?;
            let hidden = as_member(inbox.clone(), None).await?.1["data"].clone();
            ensure!(hidden["items"] == json!([]), "{table} hidden items");
            ensure!(hidden["unread"] == 0, "{table} hidden unread");
            let (status, denied) = as_member(read_path.clone(), Some(command.clone())).await?;
            ensure!(status == 404, "{table} read: {status}");
            ensure!(denied.get("receipt").is_none(), "{table} receipt");
            ensure!(sync_from(&w, "user_collaborator", &before).await?.0 == 410, "{table} cursor");
            ensure!(
                !as_member(format!("/api/sync?projectId={}", w.project), None).await?.1["data"]["page"]["events"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|event| event["recordKind"] == "workflow_notification"),
                "{table} notification synced"
            );
            db.execute(
                "UPDATE record_scopes SET visibility='project' WHERE table_name=? AND key_json=?",
                [table, &key.to_string()],
            )?;
            ensure!(as_member(read_path.clone(), Some(command.clone())).await?.0 == 200, "{table} restored");
        }
        let run_key = json!([run]).to_string();
        db.execute(
            "UPDATE record_scopes SET visibility='owner',account=? WHERE table_name='workflow_runs' AND key_json=?",
            [&collaborator, &run_key],
        )?;
        ensure!(
            w.tp.data(&inbox).await?["items"] == json!([]),
            "Owner cannot see another human private result notice"
        );
        ensure!(
            as_member(inbox.clone(), None).await?.1["data"]["items"].as_array().map(Vec::len) == Some(1),
            "private notice for member"
        );
        db.execute("UPDATE record_scopes SET visibility='project' WHERE table_name='workflow_runs' AND key_json=?", [&run_key])?;
        db.execute("UPDATE agent_projects SET deleted=1 WHERE project=? AND agent=?", [&w.project, &w.agent])?;
        ensure!(w.tp.data(&inbox).await?["items"] == json!([]), "removed agent inbox");
        ensure!(as_member(read_path.clone(), Some(command.clone())).await?.0 == 404, "removed agent read");
        db.execute("UPDATE agent_projects SET deleted=0 WHERE project=? AND agent=?", [&w.project, &w.agent])?;
        db.execute(
            "UPDATE collaborator_agents SET enabled=0,revision=revision+1 WHERE project=? AND account=? AND agent=?",
            [&w.project, &collaborator, &w.agent],
        )?;
        ensure!(as_member(inbox.clone(), None).await?.1["data"]["items"] == json!([]), "revoked inbox");
        ensure!(as_member(read_path.clone(), Some(command)).await?.0 == 404, "revoked read");
        db.execute("UPDATE project_members SET deleted=1 WHERE project=? AND account=?", [&w.project, &collaborator])?;
        ensure!(as_member(inbox.clone(), None).await?.0 == 404, "removed member inbox");
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}
