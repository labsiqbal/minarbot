//! Signed human sync: workflow, task and device observations.

use super::{
    execution::{loop_task, Binding},
    fixture::{account, agent_body, digest, direct, encode, fresh_revisions, until, Tp, DEVICE_RECONNECT, SETTLE},
    routines::text,
    workflow_waits::{signed_event, workflow_wait, WorkflowHome},
    workflows::{create, wait_run},
};
use minarbot_testkit::{http::Method, prelude::*};

fn sync(project: &str) -> String {
    format!("/api/sync?projectId={project}")
}

fn delta(project: &str, cursor: &Value) -> String {
    format!("{}&cursor={}", sync(project), encode(&text(cursor)))
}

fn page(data: &Value) -> Vec<Value> {
    data["page"]["events"].as_array().cloned().unwrap_or_default()
}

fn approval_graph(prompt: &str) -> Value {
    json!({"entry": "approval", "maxSteps": 2, "nodes": [
        {"id": "approval", "kind": "approval", "prompt": prompt, "yes": "out"},
        {"id": "out", "kind": "output"}]})
}

#[tokio::test]
async fn canonical_workflow_inbox_pages_twenty_outputs_without_duplicate_completion_or_recipient_backfill(
) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        let inbox = format!("/api/projects/{}/workflow-notifications", w.project);
        let collaborator = account(&w.tp, "user_collaborator").await?;
        w.tp.write()?
            .execute("INSERT INTO project_members VALUES (?,?,'member',1,0)", [&w.project, &collaborator])?;
        create(&w, "Paged inbox", &w.graph).await?;
        let run_path = format!("{}/run", w.detail);
        let mut runs = Vec::new();
        for n in 0..21 {
            let run = new_id();
            let command = json!({"requestId": run, "id": run, "revision": 1, "input": n.to_string()});
            runs.push(run);
            let (status, accepted) = w.tp.send(&run_path, Some(command.clone())).await?;
            ensure!(status == 201, "run {n}: {status}");
            ensure!(w.tp.send(&run_path, Some(command)).await?.1["receipt"] == accepted["receipt"], "replay {n}");
        }
        until("21 unread", SETTLE, || async { Ok(w.tp.data(&inbox).await?["unread"] == 21) }).await?;
        let first = w.tp.data(&inbox).await?;
        ensure!(first["items"].as_array().map(Vec::len) == Some(20), "first page");
        ensure!(!first["nextCursor"].is_null(), "first cursor");
        let second_path = format!("{inbox}?before={}", text(&first["nextCursor"]));
        let second = w.tp.data(&second_path).await?;
        ensure!(second["items"].as_array().map(Vec::len) == Some(1), "second page");
        ensure!(second["nextCursor"].is_null(), "second cursor");
        ensure!(second["unread"] == 21, "second unread");
        let mut listed: Vec<String> = page_items(&first).chain(page_items(&second)).collect();
        listed.sort();
        let mut expected = runs.clone();
        expected.sort();
        ensure!(listed == expected, "inbox runs");
        let as_member = |path: String| {
            let tp = &w.tp;
            async move { tp.send_as(&path, None, "user_collaborator", None).await }
        };
        ensure!(as_member(inbox.clone()).await?.1["data"]["items"] == json!([]), "unassigned member inbox");
        w.tp.write()?.execute(
            "INSERT INTO collaborator_agents VALUES (?,?,?,1,1)",
            [&w.project, &collaborator, &w.agent],
        )?;
        ensure!(
            as_member(format!("/api/projects/{}/workflow-runs/{}", w.project, runs[0])).await?.0 == 200,
            "assigned member run"
        );
        ensure!(as_member(inbox.clone()).await?.1["data"]["items"] == json!([]), "no recipient backfill");
        w.tp.restart().await?;
        ensure!(w.tp.data(&inbox).await?["unread"] == 21, "unread after restart");
        ensure!(w.tp.data(&second_path).await? == second, "second page after restart");
        let last = new_id();
        ensure!(
            w.tp.send(&run_path, Some(json!({"requestId": last, "id": last, "revision": 1,
                "input": "Newly assigned reader"})))
                .await?
                .0
                == 201,
            "last run"
        );
        until("member unread", SETTLE, || async {
            Ok(w.tp.send_as(&inbox, None, "user_collaborator", None).await?.1["data"]["unread"] == 1)
        })
        .await?;
        ensure!(
            w.tp.send_as(&inbox, None, "user_collaborator", None).await?.1["data"]["items"][0]["run"] == last.as_str(),
            "member notice"
        );
        let read = new_id();
        let command = json!({"requestId": read, "id": read});
        ensure!(w.tp.send(&format!("{inbox}/{}/read", runs[0]), Some(command.clone())).await?.0 == 200, "read");
        ensure!(w.tp.send(&format!("{inbox}/{last}/read"), Some(command)).await?.0 == 409, "reused request");
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

fn page_items(data: &Value) -> impl Iterator<Item = String> + '_ {
    data["items"].as_array().into_iter().flatten().map(|item| text(&item["run"]))
}

#[tokio::test]
async fn workflow_sync_publishes_metadata_and_revisioned_run_transitions_once_across_retry_and_restart(
) -> Result<()> {
    let mut w = WorkflowHome::start().await?;
    let result = async {
        let before = w.tp.data(&sync(&w.project)).await?;
        let create = json!({"requestId": w.workflow, "id": w.workflow, "revision": 0,
            "name": "Observed workflow", "graph": approval_graph("Private approval canary")});
        let (status, saved) = w.tp.send(&w.list, Some(create.clone())).await?;
        ensure!(status == 201, "create: {status}");
        ensure!(w.tp.send(&w.list, Some(create)).await?.1["receipt"] == saved["receipt"], "create replay");
        let created = w.tp.data(&delta(&w.project, &before["nextCursor"])).await?;
        let rows: Vec<Value> = page(&created)
            .iter()
            .map(|event| json!([event["recordKind"], event["recordId"], event["revision"]]))
            .collect();
        ensure!(rows == vec![json!(["workflow", w.workflow, 1])], "created: {rows:?}");
        ensure!(page(&created)[0]["body"]["detailRef"] == w.detail.as_str(), "detailRef");
        let run = new_id();
        let path = format!("/api/projects/{}/workflow-runs/{run}", w.project);
        let command = json!({"requestId": run, "id": run, "revision": 1,
            "input": "Private workflow input canary"});
        let (status, accepted) = w.tp.send(&format!("{}/run", w.detail), Some(command.clone())).await?;
        ensure!(status == 201, "run: {status}");
        ensure!(
            w.tp.send(&format!("{}/run", w.detail), Some(command)).await?.1["receipt"] == accepted["receipt"],
            "run replay"
        );
        wait_run(&w, &path, |run| run["status"] == "approval").await?;
        let approve_id = new_id();
        let approve = json!({"requestId": approve_id, "id": approve_id, "revision": 2, "accepted": true});
        ensure!(w.tp.send(&format!("{path}/approve"), Some(approve.clone())).await?.0 == 200, "approve");
        wait_run(&w, &path, |run| run["status"] == "done").await?;
        w.tp.restart().await?;
        ensure!(w.tp.send(&format!("{path}/approve"), Some(approve)).await?.0 == 200, "approve replay");
        let changes = w.tp.data(&delta(&w.project, &created["nextCursor"])).await?;
        let rows: Vec<Value> = page(&changes)
            .iter()
            .map(|event| json!([event["recordKind"], event["revision"], event["body"]["status"]]))
            .collect();
        ensure!(
            rows == vec![
                json!(["workflow_run", 1, "ready"]),
                json!(["workflow_run", 2, "approval"]),
                json!(["workflow_run", 3, "ready"]),
                json!(["workflow_notification", 1, null]),
                json!(["workflow_run", 4, "done"]),
            ],
            "delta: {rows:?}"
        );
        for event in page(&changes) {
            ensure!(event["recordId"] == run.as_str(), "record id");
            ensure!(event["body"]["detailRef"] == path.as_str(), "run detailRef");
        }
        let snapshot = w.tp.data(&sync(&w.project)).await?;
        ensure!(
            page(&snapshot)
                .iter()
                .find(|event| event["recordKind"] == "workflow_run")
                .is_some_and(|event| event["revision"] == 4),
            "snapshot revision"
        );
        ensure!(!json!([created, changes, snapshot]).to_string().contains("canary"), "canary synced");
        ensure!(!changes.to_string().contains("graph"), "graph synced");
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn workflow_sync_includes_protected_wait_activation_delivery_and_expiry_without_secret_references(
) -> Result<()> {
    let f = workflow_wait("user_owner", 60).await?;
    let result = async {
        let w = &f.w;
        let snapshot = w.tp.data(&sync(&w.project)).await?;
        let initial = page(&snapshot)
            .into_iter()
            .find(|event| event["recordKind"] == "workflow_run")
            .ok_or("initial run event")?;
        ensure!(initial["revision"] == 2, "initial revision");
        ensure!(initial["body"]["status"] == "waiting", "initial status");
        let secret = w.secret(None)?;
        let time = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs()).to_string();
        let delivery = new_id();
        let body = r#"{"event":"ready","payload":"Private event canary"}"#;
        let events = format!("{}/events", f.wait_path);
        let send = || signed_event(&w.tp, &events, &secret, body, &delivery, &time, &[]);
        let db = w.tp.write()?;
        db.execute_batch("CREATE TRIGGER fail_workflow_sync BEFORE INSERT ON sync_events WHEN NEW.record_kind='workflow_run' BEGIN SELECT RAISE(ABORT,'synthetic workflow event failure'); END")?;
        ensure!(send().await?.0 == 500, "event failure");
        ensure!(w.tp.data(&f.wait_path).await?["status"] == "waiting", "wait kept");
        ensure!(w.tp.data(&f.path).await?["revision"] == 2, "run kept");
        db.execute_batch("DROP TRIGGER fail_workflow_sync")?;
        let (status, accepted) = send().await?;
        ensure!(status == 200, "event: {status} {accepted}");
        wait_run(w, &f.path, |run| run["status"] == "done").await?;
        ensure!(send().await?.1["receipt"] == accepted["receipt"], "event replay");
        let changes = w.tp.data(&delta(&w.project, &snapshot["nextCursor"])).await?;
        let rows: Vec<Value> = page(&changes)
            .iter()
            .map(|event| json!([event["recordKind"], event["revision"], event["body"]["status"]]))
            .collect();
        ensure!(
            rows == vec![
                json!(["workflow_run", 3, "ready"]),
                json!(["workflow_notification", 1, null]),
                json!(["workflow_run", 4, "done"]),
            ],
            "delta: {rows:?}"
        );
        let observations = json!([snapshot, changes]).to_string();
        for value in [secret.as_str(), "secret_ref", "secretRef", "secret_generation", "Private event canary"] {
            ensure!(!observations.contains(value), "{value} synced");
        }
        let edit = new_id();
        let graph = json!({"entry": "event", "maxSteps": 2, "nodes": [
            {"id": "event", "kind": "wait", "event": "ready", "timeoutSeconds": 2, "next": "out"},
            {"id": "out", "kind": "output"}]});
        ensure!(
            w.tp.send(&format!("{}/edit", w.detail), Some(json!({"requestId": edit, "id": edit, "revision": 1,
                "name": "Expiry", "graph": graph})))
                .await?
                .0
                == 200,
            "edit"
        );
        let next = new_id();
        let path = format!("/api/projects/{}/workflow-runs/{next}", w.project);
        let before = w.tp.data(&sync(&w.project)).await?;
        ensure!(
            w.tp.send(&format!("{}/run", w.detail), Some(json!({"requestId": next, "id": next, "revision": 2, "input": ""})))
                .await?
                .0
                == 201,
            "expiring run"
        );
        wait_run(w, &path, |run| run["status"] == "waiting").await?;
        wait_run(w, &path, |run| run["status"] == "failed").await?;
        let expired = w.tp.data(&delta(&w.project, &before["nextCursor"])).await?;
        let rows: Vec<Value> = page(&expired)
            .iter()
            .map(|event| json!([event["revision"], event["body"]["status"]]))
            .collect();
        ensure!(
            rows == vec![json!([1, "ready"]), json!([2, "waiting"]), json!([3, "failed"])],
            "expired: {rows:?}"
        );
        Ok(())
    }
    .await;
    f.w.tp.close().await?;
    result
}

#[tokio::test]
async fn workflow_sync_rolls_back_commands_with_event_failures_and_retains_deletion_tombstones(
) -> Result<()> {
    let w = WorkflowHome::start().await?;
    let result = async {
        let before = w.tp.data(&sync(&w.project)).await?;
        let create = json!({"requestId": w.workflow, "id": w.workflow, "revision": 0,
            "name": "Original", "graph": approval_graph("Approve")});
        let db = w.tp.write()?;
        db.execute_batch("CREATE TRIGGER fail_workflow_sync BEFORE INSERT ON sync_events WHEN NEW.record_kind='workflow' BEGIN SELECT RAISE(ABORT,'synthetic workflow sync failure'); END")?;
        ensure!(w.tp.send(&w.list, Some(create.clone())).await?.0 == 500, "create failure");
        ensure!(w.tp.send(&w.detail, None).await?.0 == 404, "partial workflow");
        ensure!(page(&w.tp.data(&delta(&w.project, &before["nextCursor"])).await?).is_empty(), "events after failure");
        db.execute_batch("DROP TRIGGER fail_workflow_sync")?;
        ensure!(w.tp.send(&w.list, Some(create)).await?.0 == 201, "create");
        for (operation, revision, fields) in [
            ("edit", 1, json!({"name": "Edited", "graph": approval_graph("Approve")})),
            ("restore", 2, json!({"version": 1})),
        ] {
            let id = new_id();
            let mut command = json!({"requestId": id, "id": id, "revision": revision});
            for (key, value) in fields.as_object().into_iter().flatten() {
                command[key] = value.clone();
            }
            let path = format!("{}/{operation}", w.detail);
            let (status, accepted) = w.tp.send(&path, Some(command.clone())).await?;
            ensure!(status == 200, "{operation}: {status}");
            ensure!(w.tp.send(&path, Some(command)).await?.1["receipt"] == accepted["receipt"], "{operation} replay");
        }
        let run = new_id();
        let path = format!("/api/projects/{}/workflow-runs/{run}", w.project);
        let command = json!({"requestId": run, "id": run, "revision": 3, "input": ""});
        let fail_run_sync = "CREATE TRIGGER fail_run_sync BEFORE INSERT ON sync_events WHEN NEW.record_kind='workflow_run' BEGIN SELECT RAISE(ABORT,'synthetic run sync failure'); END";
        db.execute_batch(fail_run_sync)?;
        ensure!(w.tp.send(&format!("{}/run", w.detail), Some(command.clone())).await?.0 == 500, "run failure");
        ensure!(w.tp.send(&path, None).await?.0 == 404, "partial run");
        db.execute_batch("DROP TRIGGER fail_run_sync")?;
        ensure!(w.tp.send(&format!("{}/run", w.detail), Some(command)).await?.0 == 201, "run");
        wait_run(&w, &path, |run| run["status"] == "approval").await?;
        let cancel_id = new_id();
        let cancellation = json!({"requestId": cancel_id, "id": cancel_id, "revision": 2});
        db.execute_batch(fail_run_sync)?;
        ensure!(w.tp.send(&format!("{path}/cancel"), Some(cancellation.clone())).await?.0 == 500, "cancel failure");
        ensure!(w.tp.data(&path).await?["status"] == "approval", "cancel rolled back");
        db.execute_batch("DROP TRIGGER fail_run_sync")?;
        let (status, cancelled) = w.tp.send(&format!("{path}/cancel"), Some(cancellation.clone())).await?;
        ensure!(status == 200, "cancel: {status}");
        ensure!(
            w.tp.send(&format!("{path}/cancel"), Some(cancellation)).await?.1["receipt"] == cancelled["receipt"],
            "cancel replay"
        );
        let delete_id = new_id();
        let deletion = json!({"requestId": delete_id, "id": delete_id, "revision": 3});
        ensure!(w.tp.send(&format!("{}/delete", w.detail), Some(deletion.clone())).await?.0 == 200, "delete");
        ensure!(w.tp.send(&format!("{}/delete", w.detail), Some(deletion)).await?.0 == 200, "delete replay");
        let changes = w.tp.data(&delta(&w.project, &before["nextCursor"])).await?;
        let rows: Vec<Value> = page(&changes)
            .iter()
            .map(|event| json!([event["recordKind"], event["revision"], event["operation"]]))
            .collect();
        ensure!(
            rows == vec![
                json!(["workflow", 1, "upsert"]),
                json!(["workflow", 2, "upsert"]),
                json!(["workflow", 3, "upsert"]),
                json!(["workflow_run", 1, "upsert"]),
                json!(["workflow_run", 2, "upsert"]),
                json!(["workflow_run", 3, "upsert"]),
                json!(["workflow", 4, "delete"]),
            ],
            "delta: {rows:?}"
        );
        let snapshot = w.tp.data(&sync(&w.project)).await?;
        let find = |kind: &str| page(&snapshot).into_iter().find(|event| event["recordKind"] == kind);
        ensure!(find("workflow").is_some_and(|event| event["operation"] == "delete"), "workflow tombstone");
        ensure!(find("workflow_run").is_some_and(|event| event["body"]["status"] == "cancelled"), "run status");
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn workflow_sync_rechecks_assignment_project_and_private_root_version_and_run_scopes(
) -> Result<()> {
    let w = WorkflowHome::start().await?;
    let result = async {
        let collaborator = account(&w.tp, "user_collaborator").await?;
        let db = w.tp.write()?;
        db.execute("INSERT INTO project_members VALUES (?,?,'member',1,0)", [&w.project, &collaborator])?;
        create(&w, "Scoped", &approval_graph("Approve")).await?;
        let run = new_id();
        let run_path = format!("/api/projects/{}/workflow-runs/{run}", w.project);
        ensure!(
            w.tp.send(&format!("{}/run", w.detail), Some(json!({"requestId": run, "id": run, "revision": 1, "input": ""})))
                .await?
                .0
                == 201,
            "run"
        );
        wait_run(&w, &run_path, |run| run["status"] == "approval").await?;
        let read = |subject: &'static str| {
            let tp = &w.tp;
            let path = sync(&w.project);
            async move { tp.send_as(&path, None, subject, None).await.map(|(_, body)| body["data"].clone()) }
        };
        let kinds = |data: &Value| -> Vec<String> {
            page(data)
                .iter()
                .map(|event| text(&event["recordKind"]))
                .filter(|kind| kind.starts_with("workflow"))
                .collect()
        };
        ensure!(kinds(&read("user_collaborator").await?).is_empty(), "unassigned kinds");
        db.execute("INSERT INTO collaborator_agents VALUES (?,?,?,1,1)", [&w.project, &collaborator, &w.agent])?;
        db.execute("INSERT INTO response_policies VALUES (?,?,?,0,1)", [&w.project, &collaborator, &w.agent])?;
        ensure!(kinds(&read("user_collaborator").await?) == ["workflow", "workflow_run"], "assigned kinds");
        ensure!(kinds(&w.tp.data(&sync(&w.other)).await?).is_empty(), "other project kinds");
        for (table, key, expected) in [
            ("workflows", json!([w.workflow]), vec![]),
            ("workflow_versions", json!([w.workflow, 1]), vec![]),
            ("workflow_runs", json!([run]), vec!["workflow".to_owned()]),
        ] {
            let before = read("user_collaborator").await?;
            db.execute(
                "UPDATE record_scopes SET visibility='owner' WHERE table_name=? AND key_json=?",
                [table, &key.to_string()],
            )?;
            let (status, denied) = w
                .tp
                .send_as(&delta(&w.project, &before["nextCursor"]), None, "user_collaborator", None)
                .await?;
            ensure!(status == 410, "{table} cursor: {status}");
            ensure!(denied["error"]["code"] == "resync_required", "{table} code");
            ensure!(kinds(&read("user_collaborator").await?) == expected, "{table} kinds");
            let detail = if table == "workflow_runs" { run_path.clone() } else { w.detail.clone() };
            ensure!(w.tp.send_as(&detail, None, "user_collaborator", None).await?.0 == 404, "{table} detail");
            db.execute(
                "UPDATE record_scopes SET visibility='project' WHERE table_name=? AND key_json=?",
                [table, &key.to_string()],
            )?;
        }
        let root = json!([w.workflow]).to_string();
        db.execute(
            "UPDATE record_scopes SET visibility='owner',account=? WHERE table_name='workflows' AND key_json=?",
            [&collaborator, &root],
        )?;
        ensure!(
            kinds(&read("user_owner").await?).is_empty(),
            "Project owner cannot read another human private root"
        );
        ensure!(kinds(&read("user_collaborator").await?) == ["workflow", "workflow_run"], "private owner kinds");
        db.execute("UPDATE record_scopes SET visibility='project' WHERE table_name='workflows' AND key_json=?", [&root])?;
        let before = read("user_collaborator").await?;
        db.execute(
            "UPDATE collaborator_agents SET enabled=0,revision=revision+1 WHERE project=? AND account=? AND agent=?",
            [&w.project, &collaborator, &w.agent],
        )?;
        ensure!(
            w.tp.send_as(&delta(&w.project, &before["nextCursor"]), None, "user_collaborator", None).await?.0 == 410,
            "revoked cursor"
        );
        ensure!(kinds(&read("user_collaborator").await?).is_empty(), "revoked kinds");
        ensure!(w.tp.send_as(&sync(&w.project), None, "user_stranger", None).await?.0 == 403, "stranger");
        Ok(())
    }
    .await;
    w.tp.close().await?;
    result
}

#[tokio::test]
async fn signed_human_sync_preserves_delta_cursors_through_canonical_restart_and_rejects_foreign_or_revoked_reads(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let project = new_id();
        tp.project(&project, "Sync project").await?;
        let path = sync(&project);
        let (status, first) = tp.send(&path, None).await?;
        ensure!(status == 200, "first: {status}");
        ensure!(!text(&first["data"]["nextCursor"]).is_empty(), "first cursor");
        let edit = json!({"requestId": new_id(), "expectedRevision": 1, "name": "Persisted sync value"});
        let project_path = format!("/api/projects/{project}");
        let (status, saved) = tp.send_as(&project_path, Some(edit.clone()), "user_owner", Some(Method::PATCH)).await?;
        ensure!(status == 200, "edit: {status}");
        ensure!(
            tp.send_as(&project_path, Some(edit), "user_owner", Some(Method::PATCH)).await?.1["receipt"] == saved["receipt"],
            "edit replay"
        );
        tp.restart().await?;
        let delta_path = delta(&project, &first["data"]["nextCursor"]);
        let (status, changes) = tp.send(&delta_path, None).await?;
        ensure!(status == 200, "delta: {status}");
        ensure!(page(&changes["data"]).len() == 1, "delta events");
        ensure!(page(&changes["data"])[0]["body"]["name"] == "Persisted sync value", "delta name");
        ensure!(tp.send_as(&delta_path, None, "user_stranger", None).await?.0 == 403, "stranger");
        for suffix in ["&cursor=".to_owned(), "&page=broken".into(), "&repo=private".into(), format!("&projectId={project}")] {
            ensure!(tp.send(&format!("{path}{suffix}"), None).await?.0 == 400, "{suffix}");
        }
        let (status, _) = tp
            .send_as(
                &project_path,
                Some(json!({"requestId": new_id(), "expectedRevision": 2, "deleted": true})),
                "user_owner",
                Some(Method::PATCH),
            )
            .await?;
        ensure!(status == 200, "delete: {status}");
        let (status, revoked) = tp.send(&delta_path, None).await?;
        ensure!(status == 403, "revoked: {status}");
        ensure!(revoked["ok"] == false, "revoked ok");
        ensure!(revoked.get("data").is_none(), "revoked data");
        Ok(())
    }
    .await;
    tp.close().await?;
    result
}

#[tokio::test]
async fn signed_task_and_device_mutations_publish_atomic_scoped_observations_across_retry_and_restart(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let runtime = tp.device("sync-home.sqlite")?;
        let project = new_id();
        tp.project(&project, "Device sync").await?;
        let path = sync(&project);
        let first = tp.data(&path).await?;
        runtime.enroll(&tp.jwt("user_owner")?, "new").await?;
        let connection = runtime.connect().await?;
        until("home", SETTLE, || async { Ok(connection.phase() == "connected") }).await?;
        let home = tp.data("/api/devices").await?[0].clone();
        let device_delta = tp.data(&delta(&project, &first["nextCursor"])).await?;
        let rows: Vec<Value> = page(&device_delta)
            .iter()
            .map(|event| json!([event["recordKind"], event["body"]["status"]]))
            .collect();
        ensure!(
            rows == vec![json!(["device", "offline"]), json!(["device", "connecting"]), json!(["device", "ready"])],
            "device delta: {rows:?}"
        );
        ensure!(!device_delta.to_string().contains("signingPublicKey"), "signing key synced");
        ensure!(!device_delta.to_string().contains("deviceSession"), "device session synced");
        let agent = new_id();
        ensure!(
            tp.send("/api/agents", Some(agent_body(&agent, "Sync agent", "Private profile canary", &project, &home)))
                .await?
                .0
                == 201,
            "agent"
        );
        let before = tp.data(&path).await?;
        let task = new_id();
        let payload = direct("Private prompt canary");
        let revisions = fresh_revisions();
        let payload_digest = digest(&json!(["minarbot-task-payload-v1", task, project, agent, home["id"], payload, revisions]));
        let command = json!({"requestId": task, "projectId": project, "agentId": agent,
            "homeDeviceId": home["id"], "payload": payload, "expectedRevisions": revisions,
            "payloadDigest": payload_digest});
        let (status, accepted) = tp.send("/api/tasks", Some(command.clone())).await?;
        ensure!(status == 202, "task: {status}");
        ensure!(tp.send("/api/tasks", Some(command)).await?.1["receipt"] == accepted["receipt"], "task replay");
        let control = format!("/api/tasks/{task}/control");
        ensure!(
            tp.send(&control, Some(json!({"requestId": new_id(), "expectedRevision": 1, "operation": "pause"})))
                .await?
                .0
                == 200,
            "pause"
        );
        let cancellation = json!({"requestId": new_id(), "expectedRevision": 2, "operation": "cancel"});
        let db = tp.write()?;
        db.execute_batch("CREATE TRIGGER fail_task_sync BEFORE INSERT ON sync_events WHEN NEW.record_kind='task' BEGIN SELECT RAISE(ABORT,'synthetic event failure'); END")?;
        ensure!(tp.send(&control, Some(cancellation.clone())).await?.0 == 500, "cancel failure");
        ensure!(tp.task_detail(&task).await?["state"] == "paused", "cancel rolled back");
        db.execute_batch("DROP TRIGGER fail_task_sync")?;
        let (status, cancelled) = tp.send(&control, Some(cancellation.clone())).await?;
        ensure!(status == 200, "cancel: {status}");
        ensure!(tp.send(&control, Some(cancellation)).await?.1["receipt"] == cancelled["receipt"], "cancel replay");
        let delta_path = delta(&project, &before["nextCursor"]);
        let task_delta = tp.data(&delta_path).await?;
        let rows: Vec<Value> = page(&task_delta)
            .iter()
            .map(|event| json!([event["recordId"], event["revision"], event["body"]["state"]]))
            .collect();
        ensure!(
            rows == vec![json!([task, 1, "queued"]), json!([task, 2, "paused"]), json!([task, 3, "cancelled"])],
            "task delta: {rows:?}"
        );
        ensure!(!task_delta.to_string().contains("canary"), "canary synced");
        ensure!(!task_delta.to_string().contains(&payload_digest), "digest synced");
        let completed = loop_task(&tp, &runtime, false, Binding::New).await?;
        let completed_path = sync(&completed.project);
        let queued = tp.data(&completed_path).await?;
        runtime
            .run_task(&completed.task, |_| async { Ok(json!({"text": "Private result canary"})) })
            .await?;
        let completion = tp.data(&delta(&completed.project, &queued["nextCursor"])).await?;
        let events = page(&completion);
        ensure!(events.last().is_some_and(|event| event["body"]["state"] == "completed"), "completed event");
        ensure!(events.iter().any(|event| event["body"]["state"] == "running"), "running event");
        ensure!(!completion.to_string().contains("Private result canary"), "result synced");
        db.execute_batch("CREATE TRIGGER fail_device_sync BEFORE INSERT ON sync_events WHEN NEW.record_kind='device' BEGIN SELECT RAISE(ABORT,'synthetic offline event failure'); END")?;
        connection.stop().await?;
        ensure!(tp.send("/api/session", None).await?.0 == 200, "session");
        db.execute_batch("DROP TRIGGER fail_device_sync")?;
        // A failed disconnect write is retried within seconds, but the
        // disconnect itself can wait for the 30 s sweep.
        until("home offline", DEVICE_RECONNECT, || async {
            Ok(tp.data("/api/devices").await?[0]["status"] == "offline")
        })
        .await?;
        drop(db);
        tp.restart().await?;
        until("delta readable", SETTLE, || async {
            Ok(tp.send(&delta_path, None).await.is_ok_and(|(status, _)| status == 200))
        })
        .await?;
        let tasks: Vec<Value> = page(&tp.data(&delta_path).await?)
            .into_iter()
            .filter(|event| event["recordKind"] == "task")
            .collect();
        ensure!(tasks == page(&task_delta), "task delta after restart");
        ensure!(tp.send_as(&delta_path, None, "user_stranger", None).await?.0 == 403, "stranger");
        Ok(())
    }
    .await;
    tp.close().await?;
    result
}
