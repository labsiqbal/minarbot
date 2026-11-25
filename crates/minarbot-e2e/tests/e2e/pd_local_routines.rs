//! Project-scoped local routines (port of `app/local-routines.test.ts`).

use minarbot_testkit::{
    db::Inspector,
    model::{Reply, ScriptedModel},
    prelude::*,
    wait::never,
    FIXED_CLOCK,
};
use std::time::Duration;

fn id_of(value: &Value) -> Result<String> {
    Ok(value["id"].as_str().ok_or("missing id")?.to_owned())
}

#[tokio::test]
async fn local_routines_require_project_membership_and_a_report_section_belonging_to_that_project(
) -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let agent = app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Owner", "model": "test",
                "instructions": "Help."}),
            201,
        )
        .await?;
    let agent_id = id_of(&agent)?;
    let project = app
        .post_expect(
            "/api/projects",
            json!({"id": new_id(), "name": "Work", "members": []}),
            200,
        )
        .await?;
    let project_id = id_of(&project)?;
    let routines = format!("/api/agents/{agent_id}/routines");
    let input = json!({"id": new_id(), "name": "Briefing", "prompt": "Report.",
        "nextAt": FIXED_CLOCK + 3_600_000, "enabled": false, "project": project_id,
        "reportTo": project["channel"]});
    app.post_expect(&routines, input.clone(), 403).await?;
    app.post_expect(
        &format!("/api/projects/{project_id}/edit"),
        json!({"id": new_id(), "revision": project["revision"],
            "members": [{"agent": agent_id, "write": false}]}),
        200,
    )
    .await?;
    let saved = app.post_expect(&routines, input.clone(), 200).await?;
    ensure!(saved["project"] == project_id.as_str(), "saved: {saved}");
    ensure!(saved["report_to"] == project["channel"], "saved: {saved}");
    let mut unbound_input = input.clone();
    unbound_input["id"] = json!(new_id());
    unbound_input["project"] = Value::Null;
    if let Some(fields) = unbound_input.as_object_mut() {
        fields.remove("reportTo");
    }
    let unbound = app.post_expect(&routines, unbound_input, 200).await?;
    ensure!(unbound["report_to"] == "run", "unbound: {unbound}");
    let mut stray = input.clone();
    stray["id"] = json!(new_id());
    stray["project"] = Value::Null;
    app.post_expect(&routines, stray, 400).await?;
    let mut foreign = input.clone();
    foreign["id"] = json!(new_id());
    foreign["reportTo"] = json!(new_id());
    app.post_expect(&routines, foreign, 400).await?;
    Ok(())
}

#[tokio::test]
async fn rust_upgrades_legacy_routine_timing_without_publishing_earlier_runs() -> Result<()> {
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new()).await?;
    app.shutdown().await?;
    {
        let db = minarbot_testkit::db::Fixture::open(app.database())?;
        db.execute_batch(
            "INSERT INTO agents(id,name,model,instructions) VALUES ('legacy-agent','Legacy','fixture','Help');
            ALTER TABLE routines DROP COLUMN project;
            ALTER TABLE routines DROP COLUMN report_to;
            ALTER TABLE routines DROP COLUMN schedule_json;
            ALTER TABLE routines DROP COLUMN schedule_zone;
            DROP TABLE local_routine_reports;
            INSERT INTO routines(id,agent,name,prompt,next_at,interval_ms,enabled)
              VALUES ('legacy','legacy-agent','Legacy','Report',1794758400123,65000,0);
            INSERT INTO turns(id,agent,text,reply,status,created)
              VALUES ('old-run','legacy-agent','Report','Earlier outcome','done','2026-01-01');
            INSERT INTO routine_runs(routine,scheduled_at,turn,missed) VALUES ('legacy',1794758335123,'old-run',0);",
        )?;
    }
    for restart in 0..2 {
        let config = app.config().clone();
        app.start_stopped(config).await?;
        let migrated = Inspector::open(app.database())?;
        let row = migrated.row(
            "SELECT next_at,interval_ms,project,report_to,schedule_json,schedule_zone FROM routines",
            [],
        )?;
        ensure!(
            row == json!({"next_at": 1_794_758_400_123_i64, "interval_ms": 65000, "project": null,
                "report_to": "run", "schedule_json": null, "schedule_zone": null}),
            "restart {restart}: {row}"
        );
        ensure!(
            migrated
                .scalar::<String>("SELECT turn FROM routine_runs WHERE routine=?", ["legacy"])?
                == "old-run",
            "restart {restart}: run lost"
        );
        ensure!(
            migrated.scalar::<i64>("SELECT count(*) FROM local_routine_reports", [])? == 0,
            "restart {restart}: earlier run published"
        );
        drop(migrated);
        app.shutdown().await?;
    }
    Ok(())
}

/// The Node provider: the first user message picks the step.
fn routine_model(model: &ScriptedModel) {
    model.respond(|request| {
        let messages = request.messages();
        let user = messages
            .iter()
            .find(|message| message["role"] == "user")
            .map(|message| message["content"].as_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        let tool = messages.iter().find(|message| message["role"] == "tool");
        match (user.as_str(), tool) {
            ("READ", None) => {
                Reply::tool_call("read", "read_workspace_file", json!({"path": "scope.txt"}))
            }
            ("QUIET", _) if tool.is_none() => Reply::tool_call(
                "outcome",
                "no_update",
                json!({"reason": "Checked; nothing changed."}),
            ),
            ("FAIL", _) if tool.is_none() => Reply::tool_call(
                "outcome",
                "report_failure",
                json!({"reason": "Missing service."}),
            ),
            (_, Some(tool)) => Reply::text(tool["content"].as_str().unwrap_or("").to_owned()),
            _ => Reply::text("Final answer"),
        }
    });
}

#[tokio::test]
async fn project_routine_runs_use_project_folders_and_post_one_system_report_per_outcome_across_retry_and_restart(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    routine_model(&model);
    let root = TestRoot::new()?;
    // Project folders must be inside the Owner home (the kit's `HOME`).
    let folder = root.dir("home/work")?;
    let host = root.dir("home/host")?;
    std::fs::write(folder.join("scope.txt"), "project bytes")?;
    std::fs::write(host.join("scope.txt"), "HOST MUST NOT LEAK")?;
    let agent = new_id();
    let mut app = root
        .start(
            ServerConfig::new()
                .model(&model)
                .workspace(&agent, &host, true),
        )
        .await?;
    app.post_expect(
        "/api/agents",
        json!({"runtime": "api", "id": agent, "name": "Owner", "model": "fixture",
            "instructions": "Help."}),
        201,
    )
    .await?;
    let other = app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Teammate", "model": "fixture",
                "instructions": "Help."}),
            201,
        )
        .await?;
    let project = app
        .post_expect(
            "/api/projects",
            json!({"id": new_id(), "name": "Acme", "folders": [{"mode": "existing", "path": folder}],
                "members": [{"agent": agent, "write": false}, {"agent": other["id"], "write": false}]}),
            200,
        )
        .await?;
    let project_id = id_of(&project)?;
    let project = app
        .post_expect(
            &format!("/api/projects/{project_id}/sections"),
            json!({"id": new_id(), "revision": project["revision"], "name": "reports"}),
            201,
        )
        .await?;
    let section = project["sections"]
        .as_array()
        .and_then(|sections| sections.iter().find(|section| section["name"] == "reports"))
        .and_then(|section| section["id"].as_str())
        .ok_or("reports section")?
        .to_owned();
    let folder_text = folder.to_string_lossy().into_owned();
    for (prompt, destination, expected) in [
        ("READ", section.as_str(), "done"),
        ("QUIET", section.as_str(), "no_update"),
        ("FAIL", "direct", "failed"),
        ("READ", "run", "done"),
    ] {
        let failing_delivery = prompt == "READ" && destination == section;
        if failing_delivery {
            app.edit_database(|db| {
                db.execute_batch(
                    "CREATE TRIGGER report_failure_fixture BEFORE UPDATE OF posted ON local_routine_reports WHEN NEW.posted=1 BEGIN SELECT RAISE(ABORT,'Synthetic delivery failure'); END",
                )
            })
            .await?;
        }
        let routine = app
            .post_expect(
                &format!("/api/agents/{agent}/routines"),
                json!({"id": new_id(), "name": format!("Routine {prompt} {destination}"),
                    "prompt": prompt, "project": project_id, "reportTo": destination,
                    "trigger": "manual", "nextAt": FIXED_CLOCK + 3_600_000, "enabled": false}),
                200,
            )
            .await?;
        let routine_id = id_of(&routine)?;
        let command = json!({"id": new_id(), "revision": routine["revision"]});
        let run_path = format!("/api/routines/{routine_id}/test");
        let run = app.post_expect(&run_path, command.clone(), 200).await?;
        ensure!(
            app.post_expect(&run_path, command.clone(), 200).await? == run,
            "{prompt}: run replay differs"
        );
        let run_turn = run["turn"].as_str().ok_or("run turn")?.to_owned();
        let run_thread = run["thread"].as_str().ok_or("run thread")?.to_owned();
        let results = app
            .wait_get(&format!("/api/routines/{routine_id}/runs"), |rows| {
                ["done", "failed"].contains(&rows[0]["status"].as_str().unwrap_or(""))
            })
            .await?;
        if prompt == "READ" {
            let reply = results[0]["reply"].as_str().unwrap_or("");
            ensure!(reply.contains("project bytes"), "{prompt}: reply {reply}");
            ensure!(
                !reply.contains("HOST MUST NOT LEAK"),
                "{prompt}: host leaked"
            );
        }
        if failing_delivery {
            ensure!(
                app.get(&format!("/api/channels/{section}/turns"))
                    .await?
                    .as_array()
                    .is_some_and(Vec::is_empty),
                "Delivery receipt failure must roll back the message too"
            );
            let before = model.calls();
            app.edit_database(|db| {
                let posted: i64 = db.scalar(
                    "SELECT posted FROM local_routine_reports WHERE run=?",
                    [&run_turn],
                )?;
                if posted != 0 {
                    return Err(format!("report marked posted: {posted}").into());
                }
                db.execute_batch("DROP TRIGGER report_failure_fixture")
            })
            .await?;
            ensure!(
                model.calls() == before,
                "Restart retries delivery without running the routine"
            );
        }
        let fresh = app.get(&format!("/api/projects/{project_id}")).await?;
        ensure!(
            fresh["threads"].as_array().is_some_and(|threads| threads
                .iter()
                .any(|thread| thread["id"] == run_thread.as_str()
                    && thread["folder"] == folder_text.as_str())),
            "{prompt}: project threads {}",
            fresh["threads"]
        );
        let conversation = app.get(&format!("/api/threads/{run_thread}")).await?;
        ensure!(
            conversation["project"] == project_id.as_str(),
            "{prompt}: {conversation}"
        );
        ensure!(
            conversation["folder"] == folder_text.as_str(),
            "{prompt}: {conversation}"
        );
        let dest_path = if destination == "direct" {
            format!("/api/agents/{agent}/turns")
        } else {
            format!("/api/channels/{section}/turns")
        };
        let reports_of = |rows: &Value| -> Vec<Value> {
            rows.as_array()
                .map(|rows| {
                    rows.iter()
                        .filter(|row| row["routineReport"]["run"] == run_turn.as_str())
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        };
        let reports = app
            .wait_get(&dest_path, |rows| {
                destination == "run" || !reports_of(rows).is_empty()
            })
            .await?;
        let found = reports_of(&reports);
        let expected_count = usize::from(destination != "run");
        ensure!(found.len() == expected_count, "{prompt}: reports {found:?}");
        if let Some(report) = found.first() {
            ensure!(report["routing"] == "report", "{prompt}: {report}");
            ensure!(report["status"] == "done", "{prompt}: {report}");
            ensure!(report["room"].is_null(), "{prompt}: {report}");
            ensure!(
                report["routineReport"]["status"] == expected,
                "{prompt}: {report}"
            );
            ensure!(
                report["routineReport"]["thread"] == run_thread.as_str(),
                "{prompt}: {report}"
            );
            let needle = match prompt {
                "FAIL" => "Missing service",
                "QUIET" => "nothing changed",
                _ => "project bytes",
            };
            ensure!(
                report["reply"]
                    .as_str()
                    .is_some_and(|reply| reply.contains(needle)),
                "{prompt}: report reply {}",
                report["reply"]
            );
        }
        let before = model.calls();
        app.restart().await?;
        ensure!(
            app.post_expect(&run_path, command, 200).await? == run,
            "{prompt}: replay after restart differs"
        );
        ensure!(
            reports_of(&app.get(&dest_path).await?).len() == found.len(),
            "{prompt}: reports changed after restart"
        );
        never(
            "reports fan out or replay providers",
            Duration::from_millis(50),
            || async { Ok(model.calls() != before) },
        )
        .await?;
        ensure!(
            model.calls() == before,
            "Reports must not fan out or replay providers"
        );
    }
    let rows = app
        .get(&format!(
            "/api/routines?project={project_id}&reportTo={section}"
        ))
        .await?;
    ensure!(
        rows["routines"].as_array().map(Vec::len) == Some(2),
        "section routines: {rows}"
    );
    ensure!(
        app.get("/api/routines?project=none").await?["routines"]
            .as_array()
            .map(Vec::len)
            == Some(0),
        "project=none"
    );
    // Revoking membership rejects new runs without falling back to the host grant.
    let current = app.get(&format!("/api/projects/{project_id}")).await?;
    app.post_expect(
        &format!("/api/projects/{project_id}/edit"),
        json!({"id": new_id(), "revision": current["revision"], "members": []}),
        200,
    )
    .await?;
    let routine = rows["routines"][0].clone();
    app.post_expect(
        &format!(
            "/api/routines/{}/test",
            routine["id"].as_str().ok_or("routine id")?
        ),
        json!({"id": new_id(), "revision": routine["revision"]}),
        403,
    )
    .await?;
    Ok(())
}

/// First `0 9 * * *` in Asia/Jakarta (UTC+7, no daylight saving time)
/// strictly after `now`: 02:00 UTC.
fn next_nine_in_jakarta(now: i64) -> i64 {
    const DAY: i64 = 86_400_000;
    let candidate = now.div_euclid(DAY) * DAY + 2 * 3_600_000;
    if candidate > now {
        candidate
    } else {
        candidate + DAY
    }
}

#[tokio::test]
async fn scheduled_cron_catches_up_once_and_unavailable_project_folders_do_not_starve_other_routines(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|_| Reply::text("Scheduled result"));
    let root = TestRoot::new()?;
    let folder = root.dir("home/work")?;
    let mut app = root.start(ServerConfig::new().model(&model)).await?;
    let agent = app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Scheduler", "model": "fixture",
                "instructions": "Help."}),
        )
        .await?;
    let agent_id = id_of(&agent)?;
    let project = app
        .post(
            "/api/projects",
            json!({"id": new_id(), "name": "Unavailable",
                "folders": [{"mode": "existing", "path": folder}],
                "members": [{"agent": agent_id, "write": false}]}),
        )
        .await?;
    let input = json!({"name": "Daily", "prompt": "Run.",
        "schedule": {"kind": "cron", "expression": "0 9 * * *"}, "zone": "Asia/Jakarta",
        "enabled": true});
    let routines = format!("/api/agents/{agent_id}/routines");
    for _ in 0..11 {
        let mut body = input.clone();
        body["id"] = json!(new_id());
        body["project"] = project["id"].clone();
        body["reportTo"] = json!("run");
        app.post(&routines, body).await?;
    }
    let mut body = input.clone();
    body["id"] = json!(new_id());
    let healthy = app.post(&routines, body).await?;
    let healthy_id = id_of(&healthy)?;
    let now = FIXED_CLOCK;
    app.shutdown().await?;
    {
        let db = minarbot_testkit::db::Fixture::open(app.database())?;
        db.execute(
            "UPDATE routines SET next_at=? WHERE project IS NOT NULL",
            [now - 3 * 86_400_000],
        )?;
        db.execute(
            "UPDATE routines SET next_at=? WHERE id=?",
            (now - 2 * 86_400_000, &healthy_id),
        )?;
    }
    std::fs::remove_dir_all(&folder)?;
    let config = app.config().clone();
    app.start_stopped(config).await?;
    let turns = format!("/api/agents/{agent_id}/turns");
    let reports = app
        .wait_get(&turns, |rows| {
            rows.as_array().is_some_and(|rows| !rows.is_empty())
        })
        .await?;
    ensure!(
        reports.as_array().map(Vec::len) == Some(1),
        "reports: {reports}"
    );
    ensure!(reports[0]["routing"] == "report", "report: {}", reports[0]);
    ensure!(
        reports[0]["reply"]
            .as_str()
            .is_some_and(|reply| reply.contains("Scheduled result")),
        "report: {}",
        reports[0]
    );
    let runs = app.get(&format!("/api/routines/{healthy_id}/runs")).await?;
    ensure!(runs.as_array().map(Vec::len) == Some(1), "runs: {runs}");
    ensure!(
        runs[0]["missed"].as_i64().is_some_and(|missed| missed >= 1),
        "missed: {}",
        runs[0]
    );
    ensure!(model.calls() == 1, "calls: {}", model.calls());
    let listed = app.get(&routines).await?;
    let current = listed
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == healthy_id.as_str()))
        .cloned()
        .ok_or("healthy routine")?;
    ensure!(
        current["next_at"] == next_nine_in_jakarta(now),
        "next_at: {} (expected {})",
        current["next_at"],
        next_nine_in_jakarta(now)
    );
    ensure!(current["enabled"] == 1, "enabled: {current}");
    for row in listed.as_array().cloned().unwrap_or_default() {
        if row["project"].is_string() {
            let project_runs = app
                .get(&format!(
                    "/api/routines/{}/runs",
                    row["id"].as_str().unwrap_or("")
                ))
                .await?;
            ensure!(
                project_runs.as_array().is_some_and(Vec::is_empty),
                "unavailable folder ran: {project_runs}"
            );
        }
    }
    app.restart().await?;
    ensure!(
        app.get(&turns).await?.as_array().map(Vec::len) == Some(1),
        "restart added reports"
    );
    ensure!(model.calls() == 1, "restart calls: {}", model.calls());
    Ok(())
}
