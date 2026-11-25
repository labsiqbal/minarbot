//! Canonical shared data, avatars, sites and questions through a home.

use super::fixture::{locked, until, Home, Owned, Tp, DEVICE_RECONNECT};
use minarbot_testkit::{device_host::is_code, http::Method, prelude::*};

/// Run a canonical action; with `lose`, lose its `/context-mutations` reply,
/// restart and wait for recovery to acknowledge it (the Node `run`).
pub async fn run(
    tp: &mut Tp,
    home: &Home,
    owned: &mut Owned,
    name: &str,
    args: Value,
    lose: bool,
) -> Result<Value> {
    let input = home.canonical_input(tp, owned, name, &args).await?;
    let action = input["actionId"].as_str().unwrap_or_default().to_owned();
    if lose {
        let seen = tp.lose_next("/context-mutations");
        let lost = home.execute(&input).await;
        tp.fault(None);
        ensure!(
            lost.as_ref()
                .is_err_and(|error| is_code(error, "device_offline")),
            "lost reply: {lost:?}"
        );
        ensure!(
            *locked(&seen) == Some(200),
            "lost reply status {:?}",
            *locked(&seen)
        );
        tp.restart_home(home).await?;
        home.acknowledged(&action).await?;
    } else {
        home.execute(&input).await?;
    }
    home.content(tp, &owned.task, &action).await
}

fn names(rows: &Value) -> Vec<String> {
    rows.as_array()
        .into_iter()
        .flatten()
        .map(|row| row["name"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[tokio::test]
async fn canonical_shared_data_isolates_project_images_and_recovers_one_write_after_lost_reply_and_restart(
) -> Result<()> {
    let mut f = Tp::start().await?;
    let result = async {
        let home = f.home("home.sqlite").await?;
        let (project, other, agent) = (new_id(), new_id(), new_id());
        for id in [&project, &other] {
            f.project(id, id).await?;
        }
        f.agent(&agent, "Data agent", "Use project data", &project, &home).await?;
        let (status, _) = f
            .send_as(
                &format!("/api/projects/{other}/agents/{agent}"),
                Some(json!({"requestId": new_id(), "expectedProjectRevision": 1,
                    "expectedAgentRevision": 1, "expectedAuthorityRevision": 1,
                    "expectedParticipationRevision": 0, "rules": "",
                    "capabilities": {"v": 1, "tools": ["list_data_tables", "query_data"], "resources": []}})),
                "user_owner",
                Some(Method::PUT),
            )
            .await?;
        ensure!(status == 201, "participation: {status}");
        let mut first = {
            let task = new_id();
            f.task(&task, &project, &agent, &home, "Use data").await?;
            home.start(&task).await?
        };
        let created = run(&mut f, &home, &mut first, "query_data", json!({"sql": "CREATE TABLE notes(text TEXT)"}), false).await?;
        ensure!(created["revision"] == 1, "create: {created}");
        let inserted = run(
            &mut f,
            &home,
            &mut first,
            "query_data",
            json!({"sql": "INSERT INTO notes VALUES (?)", "parameters": ["Project A only"]}),
            true,
        )
        .await?;
        ensure!(inserted["revision"] == 2, "insert: {inserted}");
        let rows = run(&mut f, &home, &mut first, "query_data", json!({"sql": "SELECT text FROM notes"}), false).await?;
        ensure!(rows["rows"] == json!([["Project A only"]]), "rows: {rows}");
        let tables = run(&mut f, &home, &mut first, "list_data_tables", json!({}), false).await?;
        ensure!(names(&tables["tables"]) == ["notes"], "tables: {tables}");
        let mut second = {
            let task = new_id();
            f.task(&task, &other, &agent, &home, "Use data").await?;
            home.start(&task).await?
        };
        let empty = run(&mut f, &home, &mut second, "list_data_tables", json!({}), false).await?;
        ensure!(empty["tables"] == json!([]), "other project tables: {empty}");
        let missing = run(&mut f, &home, &mut second, "query_data", json!({"sql": "SELECT * FROM notes"}), false).await?;
        ensure!(
            missing["error"].as_str().unwrap_or_default().contains("no such table"),
            "other project read: {missing}"
        );
        let own = run(&mut f, &home, &mut second, "query_data", json!({"sql": "CREATE TABLE notes(text TEXT)"}), false).await?;
        ensure!(own["revision"] == 1, "other create: {own}");
        let none = run(&mut f, &home, &mut second, "query_data", json!({"sql": "SELECT * FROM notes"}), false).await?;
        ensure!(none["rows"] == json!([]), "other rows: {none}");
        let db = f.inspect()?;
        ensure!(
            db.scalar::<i64>("SELECT revision FROM shared_data WHERE project=?", [&project])? == 2,
            "project revision"
        );
        ensure!(
            db.scalar::<i64>("SELECT revision FROM shared_data WHERE project=?", [&other])? == 1,
            "other revision"
        );
        Ok(())
    }
    .await;
    f.fault(None);
    f.close().await?;
    result
}

#[tokio::test]
async fn canonical_avatar_removal_recovers_its_original_result_after_a_lost_reply_later_image_and_restart(
) -> Result<()> {
    let mut f = Tp::start().await?;
    let result =
        async {
            let home = f.home("home.sqlite").await?;
            let (project, agent) = (new_id(), new_id());
            let image = minarbot_testkit::image::png(256, 256, [0x20, 0x60, 0x60, 0xff])?;
            f.project(&project, "Avatar project").await?;
            f.agent(&agent, "Avatar owner", "Remove avatar", &project, &home)
                .await?;
            f.write()?.execute(
                "INSERT INTO agent_avatars VALUES (?,1,?)",
                minarbot_testkit::hosted::params![agent, image],
            )?;
            let task = new_id();
            f.task(&task, &project, &agent, &home, "Remove my avatar")
                .await?;
            let mut owned = home.start(&task).await?;
            let context = home
                .host
                .context(&task, &owned.ownership, &owned.revision)
                .await?;
            ensure!(
                context["snapshot"]["profile"]["avatarRevision"] == 1,
                "avatar revision: {}",
                context["snapshot"]["profile"]
            );
            let input = home
                .canonical_input(
                    &f,
                    &mut owned,
                    "set_avatar",
                    &json!({"image": null, "revision": 1}),
                )
                .await?;
            let action = input["actionId"].as_str().unwrap_or_default().to_owned();
            let seen = f.lose_next("/context-mutations");
            let lost = home.execute(&input).await;
            f.fault(None);
            ensure!(
                lost.as_ref()
                    .is_err_and(|error| is_code(error, "device_offline")),
                "lost removal: {lost:?}"
            );
            ensure!(
                *locked(&seen) == Some(200),
                "removal status {:?}",
                *locked(&seen)
            );
            let db = f.write()?;
            ensure!(
                db.row("SELECT image FROM agent_avatars WHERE agent=?", [&agent])?["image"]
                    .is_null(),
                "avatar not removed"
            );
            db.execute(
                "UPDATE agent_avatars SET revision=3,image=? WHERE agent=?",
                minarbot_testkit::hosted::params![image, agent],
            )?;
            drop(db);
            f.restart_home(&home).await?;
            home.acknowledged(&action).await?;
            let detail = f.task_detail(&task).await?;
            ensure!(
                detail["results"].as_array().map(Vec::len) == Some(1),
                "results: {detail}"
            );
            ensure!(
                detail["results"][0]["content"] == json!({"revision": 2, "present": false}),
                "original result: {}",
                detail["results"][0]["content"]
            );
            let row = f.inspect()?.row(
                "SELECT revision,image FROM agent_avatars WHERE agent=?",
                [&agent],
            )?;
            ensure!(row["revision"] == 3, "later revision kept: {row}");
            use minarbot_testkit::base64::Engine;
            ensure!(
                row["image"]
                    == json!(
                        minarbot_testkit::base64::engine::general_purpose::STANDARD.encode(&image)
                    ),
                "later image replaced"
            );
            let latest = home
                .host
                .context(&task, &owned.ownership, &detail["revision"])
                .await?;
            ensure!(
                latest["snapshot"]["profile"]["avatarRevision"] == 3,
                "context avatar revision"
            );
            Ok(())
        }
        .await;
    f.fault(None);
    f.close().await?;
    result
}

#[tokio::test]
async fn canonical_site_management_keeps_project_scope_and_recovers_one_deletion_after_a_lost_reply_and_restart(
) -> Result<()> {
    let mut f = Tp::start().await?;
    let result = async {
        let home = f.home("home.sqlite").await?;
        let (project, other, agent, site, foreign) = (new_id(), new_id(), new_id(), new_id(), new_id());
        for id in [&project, &other] {
            f.project(id, id).await?;
        }
        f.agent(&agent, "Site owner", "Manage sites", &project, &home).await?;
        {
            let db = f.write()?;
            let owner: String =
                db.scalar("SELECT id FROM accounts WHERE clerk_sub='user_owner'", [])?;
            let source = "<p>PRIVATE_SITE_SOURCE</p>";
            for (id, scope) in [(&site, &project), (&foreign, &other)] {
                db.execute(
                    "INSERT INTO local_sites(id,agent,name,source,html,revision,updated) VALUES (?,?,'Scoped site',?,?,1,?)",
                    [id.as_str(), agent.as_str(), source, source, "2026-11-25T00:00:00.000Z"],
                )?;
                db.execute(
                    "INSERT INTO site_commands VALUES (?,?,?,?)",
                    [
                        id.as_str(),
                        agent.as_str(),
                        &minarbot_testkit::protocol::digest_command(
                            &json!({"id": id, "name": "Scoped site", "source": source}),
                        ),
                        id.as_str(),
                    ],
                )?;
                db.execute(
                    "INSERT INTO record_scopes VALUES ('local_sites',?,?,?,'owner','agent',?)",
                    [&json!([id]).to_string(), &owner, scope.as_str(), agent.as_str()],
                )?;
            }
        }
        let task = new_id();
        f.task(&task, &project, &agent, &home, "List and delete my site").await?;
        let mut owned = home.start(&task).await?;
        let listing_input = home.canonical_input(&f, &mut owned, "list_sites", &json!({})).await?;
        let listing = home.execute(&listing_input).await?;
        let listed: Value = serde_json::from_str(listing["result_json"].as_str().unwrap_or("null"))?;
        let ids: Vec<&str> = listed["sites"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| row["id"].as_str())
            .collect();
        ensure!(ids == [site.as_str()], "sites: {listed}");
        ensure!(
            !listed.to_string().contains("PRIVATE_SITE_SOURCE"),
            "site source listed"
        );
        let deletion = home
            .canonical_input(&f, &mut owned, "delete_site", &json!({"site": site, "revision": 1}))
            .await?;
        let action = deletion["actionId"].as_str().unwrap_or_default().to_owned();
        let seen = f.lose_next("/context-mutations");
        let lost = home.execute(&deletion).await;
        f.fault(None);
        ensure!(
            lost.as_ref().is_err_and(|error| is_code(error, "device_offline")),
            "lost deletion: {lost:?}"
        );
        ensure!(*locked(&seen) == Some(200), "deletion status");
        f.restart_home(&home).await?;
        home.acknowledged(&action).await?;
        let detail = f.task_detail(&task).await?;
        ensure!(detail["results"].as_array().map(Vec::len) == Some(2), "results: {detail}");
        ensure!(
            detail["results"][1]["content"]
                == json!({"id": site, "name": "Scoped site", "revision": 2, "deleted": 1, "path": null}),
            "deletion result: {}",
            detail["results"][1]["content"]
        );
        let db = f.inspect()?;
        ensure!(
            db.row("SELECT revision,deleted,source,html FROM local_sites WHERE id=?", [&site])?
                == json!({"revision": 2, "deleted": 1, "source": "", "html": ""}),
            "deleted site row"
        );
        ensure!(
            db.scalar::<i64>("SELECT deleted FROM local_sites WHERE id=?", [&foreign])? == 0,
            "foreign site deleted"
        );
        ensure!(
            db.scalar::<i64>("SELECT count(*) FROM site_commands WHERE site=?", [&site])? == 2,
            "site commands"
        );
        Ok(())
    }
    .await;
    f.fault(None);
    f.close().await?;
    result
}

#[tokio::test]
async fn canonical_questions_survive_lost_rpc_and_answer_replies_restart_and_home_outage_without_duplicate_work(
) -> Result<()> {
    let mut f = Tp::start().await?;
    let home = f.home("home.sqlite").await?;
    let result = async {
        let (project, agent) = (new_id(), new_id());
        f.project(&project, "Questions").await?;
        f.agent(&agent, "Reader", "Ask first", &project, &home).await?;
        let task = new_id();
        f.task(&task, &project, &agent, &home, "Ask before continuing").await?;
        let mut owned = home.start(&task).await?;
        let input = home
            .canonical_input(&f, &mut owned, "ask_user", &json!({"question": "Continue?", "choices": ["Yes", "No"]}))
            .await?;
        let action = input["actionId"].as_str().unwrap_or_default().to_owned();
        let seen = f.lose_next("/context-mutations");
        let lost = home.execute(&input).await;
        f.fault(None);
        ensure!(
            lost.as_ref().is_err_and(|error| is_code(error, "device_offline")),
            "lost question: {lost:?}"
        );
        ensure!(*locked(&seen) == Some(200), "question status");
        let detail = f.task_detail(&task).await?;
        ensure!(detail["state"] == "paused", "state: {detail}");
        ensure!(detail["questions"].as_array().map(Vec::len) == Some(1), "questions");
        ensure!(detail["questions"][0]["answer"].is_null(), "answered early");
        f.restart_home(&home).await?;
        home.acknowledged(&action).await?;
        let detail = f.task_detail(&task).await?;
        ensure!(detail["questions"].as_array().map(Vec::len) == Some(1), "questions after restart");
        ensure!(detail["results"].as_array().map(Vec::len) == Some(1), "results after restart");
        let path = format!(
            "/api/tasks/{task}/questions/{}/answer",
            detail["questions"][0]["id"].as_str().unwrap_or_default()
        );
        let answer = json!({"requestId": new_id(), "expectedRevision": detail["revision"], "text": "Yes"});
        let with = |field: &str, value: Value| {
            let mut body = answer.clone();
            body[field] = value;
            body
        };
        ensure!(f.send(&path, Some(with("text", json!("Maybe")))).await?.0 == 400, "invalid choice");
        ensure!(
            f.send_as(&path, Some(answer.clone()), "user_foreign", None).await?.0 == 404,
            "foreign answer"
        );
        let resume = f
            .send(
                &format!("/api/tasks/{task}/control"),
                Some(json!({"requestId": new_id(), "expectedRevision": detail["revision"],
                    "operation": "resume"})),
            )
            .await?;
        ensure!(resume.1["error"]["code"] == "question_pending", "resume: {resume:?}");
        let stale = f.send(&path, Some(with("expectedRevision", json!(1)))).await?;
        ensure!(stale.1["error"]["code"] == "stale_revision", "stale: {stale:?}");
        // The owner's answer reply is lost after the server answered 200.
        let lost = f.send(&path, Some(answer.clone())).await?;
        ensure!(lost.0 == 200, "answer: {lost:?}");
        f.restart_home(&home).await?;
        let replay = f.send(&path, Some(answer.clone())).await?;
        ensure!(replay.0 == 200, "replay: {replay:?}");
        ensure!(replay.1["data"]["answer"] == "Yes", "replay: {replay:?}");
        let conflict = f.send(&path, Some(with("text", json!("No")))).await?;
        ensure!(conflict.1["error"]["code"] == "payload_conflict", "conflict: {conflict:?}");
        let detail = f.task_detail(&task).await?;
        ensure!(detail["state"] == "running", "state after answer: {detail}");
        ensure!(detail["questions"][0]["answer"] == "Yes", "answer stored");
        let continued = home
            .host
            .context(&task, &owned.ownership, &detail["revision"])
            .await?;
        ensure!(
            continued["snapshot"]["conversation"]["questions"][0]["answer"] == "Yes",
            "context answer"
        );
        home.host
            .action(
                &json!({"taskId": task, "actionId": new_id(), "sequence": 1, "actionKind": "read",
                    "ownership": owned.ownership, "payload": {"operation": "after answer"},
                    "context": continued, "grantRevision": 1}),
                |_| async { Ok(json!({"text": "Continued once"})) },
            )
            .await?;
        let detail = f.task_detail(&task).await?;
        ensure!(detail["results"].as_array().map(Vec::len) == Some(2), "results after read");
        let next_context = home
            .host
            .context(&task, &owned.ownership, &detail["revision"])
            .await?;
        let next = new_id();
        home.execute(&json!({"taskId": task, "actionId": next, "sequence": 2,
            "actionKind": "canonical", "ownership": owned.ownership, "payload": {"toolCall": {
                "id": next, "function": {"name": "ask_user", "arguments":
                    json!({"question": "More input?", "choices": []}).to_string()}},
                "expectedRevision": next_context["snapshot"]["taskRevision"].as_i64().unwrap_or(0) + 1},
            "context": next_context, "grantRevision": 1}))
            .await?;
        home.host.close().await?;
        until("home offline", DEVICE_RECONNECT, || async {
            Ok(f.data("/api/devices").await?[0]["status"] != "ready")
        })
        .await?;
        let detail = f.task_detail(&task).await?;
        let offline = f
            .send(
                &format!(
                    "/api/tasks/{task}/questions/{}/answer",
                    detail["questions"][1]["id"].as_str().unwrap_or_default()
                ),
                Some(json!({"requestId": new_id(), "expectedRevision": detail["revision"],
                    "text": "Saved while offline"})),
            )
            .await?;
        ensure!(offline.0 == 200, "offline answer: {offline:?}");
        ensure!(offline.1["data"]["state"] == "waiting_device", "offline: {offline:?}");
        ensure!(
            f.send(&path, Some(answer)).await?.1["receipt"] == replay.1["receipt"],
            "answer receipt changed"
        );
        let _ = owned.sequence;
        Ok(())
    }
    .await;
    f.fault(None);
    f.close().await?;
    result
}
