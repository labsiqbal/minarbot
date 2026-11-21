use super::*;

#[tokio::test]
async fn workflow_cron_blocks_impossible_and_tightly_spaced_expressions_before_save() -> Result<()>
{
    let f = Fixture::new(epoch("2024-01-01T03:00:00Z"))?;
    f.workflow(WORKFLOW).await?;
    let long = " ".repeat(257);
    for expression in [
        "",
        "0 9 * *",
        "0 0 9 * * *",
        "@daily",
        "0 9 * JAN MON",
        "60 9 * * *",
        "0 24 * * *",
        "0 9 0 * *",
        "0 9 * 13 *",
        "0 9 * * 7",
        "*/0 * * * *",
        "*/60 * * * *",
        "0 9 2-1 * *",
        "0,,5 9 * * *",
        "0-5/1 9 * * *",
        "0,59 * * * *",
        &long,
        "0 9 31 2 *",
    ] {
        let invalid = scheduled(json!({"kind":"cron","expression":expression}), 0, true);
        assert!(
            f.save_schedule(WORKFLOW, &invalid).await.is_err(),
            "{expression}"
        );
        assert_eq!(f.schedule(WORKFLOW).await?["schedule"], Value::Null);
    }
    for (expression, expected) in [
        ("0 9 1 * 1", "2024-01-08T02:00:00Z"),
        ("0 9 1-31 * 1", "2024-01-02T02:00:00Z"),
        ("5,20-50/15 9-11/2 * * *", "2024-01-01T04:05:00Z"),
        ("0 0 29 2 *", "2024-02-28T17:00:00Z"),
    ] {
        let workflow = uuid::Uuid::new_v4().to_string();
        f.workflow(&workflow).await?;
        let command = scheduled(json!({"kind":"cron","expression":expression}), 0, true);
        assert_eq!(
            f.save_schedule(&workflow, &command).await?["data"]["nextAt"],
            epoch(expected),
            "{expression}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn workflow_schedule_validates_closed_inputs_before_writes() -> Result<()> {
    let now = epoch("2024-01-01T00:00:00Z");
    let f = Fixture::new(now)?;
    f.workflow(WORKFLOW).await?;
    for spec in [
        Value::Null,
        json!([]),
        json!({}),
        json!({"kind":"instant","at":-1}),
        json!({"kind":"instant","at":1.1}),
        json!({"kind":"instant","at":null}),
        json!({"kind":"instant","at":253370764800000_i64}),
        json!({"kind":"instant","at":1,"zone":"UTC"}),
        json!({"kind":"interval","amount":1,"unit":"minute","anchorAt":1}),
        json!({"kind":"interval","amount":2,"unit":"minute","anchorAt":1}),
        json!({"kind":"interval","amount":4,"unit":"minute","anchorAt":1}),
        json!({"kind":"interval","amount":9007199254740991_i64,"unit":"day","anchorAt":1}),
        json!({"kind":"interval","amount":5,"unit":"months","anchorAt":1}),
        json!({"kind":"once","date":"2024-01-01","time":"09:00","zone":"UTC"}),
        json!({"kind":"legacy_interval","intervalMs":60000,"anchorAt":1}),
        json!({"kind":"interval","amount":5,"unit":"minute"}),
        json!({"kind":"cron","expression":"* * * * *"}),
    ]
    .into_iter()
    .chain(
        [
            "2024-02-30",
            "2023-02-29",
            "2024-13-01",
            "1969-12-31",
            "9999-01-01",
            "2024-1-01",
        ]
        .into_iter()
        .map(|date| json!({"kind":"once","date":date,"time":"09:00"})),
    )
    .chain(
        [
            json!("24:00"),
            json!("09:60"),
            json!("9:00"),
            json!("09:00:00"),
            Value::Null,
        ]
        .into_iter()
        .map(|time| json!({"kind":"once","date":"2024-01-01","time":time})),
    ) {
        assert!(
            f.save_schedule(WORKFLOW, &scheduled(spec.clone(), 0, true))
                .await
                .is_err(),
            "{spec}"
        );
        assert_eq!(f.schedule(WORKFLOW).await?["schedule"], Value::Null);
    }
    let at = 1234567890123_i64;
    let valid = scheduled(json!({"kind":"instant","at":at}), 0, true);
    assert_eq!(
        f.save_schedule(WORKFLOW, &valid).await?["data"]["nextAt"],
        at
    );
    assert_eq!(f.schedule(WORKFLOW).await?["spec"], valid["spec"]);
    let mut extra = valid;
    extra["projectId"] = json!(PROJECT);
    assert!(f.save_schedule(WORKFLOW, &extra).await.is_err());
    for (key, value) in [("before", "0"), ("zone", "UTC")] {
        assert!(route(
            &f.state,
            &format!("/api/projects/{PROJECT}/workflows/{WORKFLOW}/schedule"),
            &Method::GET,
            &Value::Null,
            OWNER,
            &[(key.to_owned(), value.to_owned())],
            &HeaderMap::new(),
        )
        .is_err());
    }
    Ok(())
}

#[tokio::test]
async fn workflow_once_clocks_resolve_gaps_folds_and_skipped_dates() -> Result<()> {
    let f = Fixture::new(epoch("2024-01-01T00:00:00Z"))?;
    for (zone, date, time, expected) in [
        (
            "America/New_York",
            "2024-03-10",
            "02:30",
            "2024-03-10T07:00:00Z",
        ),
        (
            "America/New_York",
            "2024-11-03",
            "01:30",
            "2024-11-03T05:30:00Z",
        ),
        (
            "Australia/Lord_Howe",
            "2024-10-06",
            "02:15",
            "2024-10-05T15:30:00Z",
        ),
        (
            "Pacific/Apia",
            "2011-12-30",
            "09:00",
            "2011-12-30T10:00:00Z",
        ),
        (
            "Australia/Lord_Howe",
            "2024-04-07",
            "01:45",
            "2024-04-06T14:45:00Z",
        ),
        (
            "Asia/Kathmandu",
            "1986-01-01",
            "00:00",
            "1985-12-31T18:30:00Z",
        ),
    ] {
        f.zone(zone, 2)?;
        let workflow = uuid::Uuid::new_v4().to_string();
        f.workflow(&workflow).await?;
        let spec = json!({"kind":"once","date":date,"time":time});
        let accepted = f
            .save_schedule(&workflow, &scheduled(spec.clone(), 0, true))
            .await?;
        assert_eq!(accepted["data"]["nextAt"], epoch(expected));
        assert_eq!(accepted["data"]["spec"], spec);
    }
    Ok(())
}

#[tokio::test]
async fn canonical_parallel_graph_rejection_preserves_versions_receipts_and_sync() -> Result<()> {
    let f = Fixture::new(epoch("2024-01-01T00:00:00Z"))?;
    let mut body = command(0);
    body["name"] = json!("Parallel");
    body["graph"] = json!({"entry":"fan","maxSteps":4,"nodes":[
        {"id":"fan","kind":"parallel","branches":[{"kind":"agent","agent":AGENT,"prompt":"A"},{"kind":"model","model":"fixture","prompt":"B"}],"next":"out"},
        {"id":"out","kind":"output"}
    ]});
    let before = f.db(|db| all(db, "SELECT * FROM record_scopes", []))?;
    let error = f
        .request(&format!("/agents/{AGENT}/workflows"), Some(&body))
        .await
        .err()
        .ok_or_else(|| fixture_error("canonical parallel graph"))?;
    assert_eq!(error.code, "invalid_workflow");
    assert_eq!(
        error.message,
        "Parallel steps are not supported by canonical execution."
    );
    f.db(|db| {
        assert_eq!(all(db, "SELECT * FROM record_scopes", [])?, before);
        for table in [
            "workflows",
            "workflow_versions",
            "workflow_runs",
            "workflow_steps",
            "receipts",
            "sync_events",
        ] {
            assert!(
                !exists(db, &format!("SELECT 1 FROM {table}"), [])?,
                "{table}"
            );
        }
        Ok(())
    })
}

#[tokio::test]
async fn canonical_saved_run_reports_unsupported_step_as_failed() -> Result<()> {
    let f = Fixture::new(epoch("2024-01-01T00:00:00Z"))?;
    f.workflow(WORKFLOW).await?;
    f.ready()?;
    for kind in [
        "image",
        "asset",
        "file-output",
        "message-trigger",
        "parallel",
    ] {
        let run = uuid::Uuid::new_v4().to_string();
        f.request(
            &format!("/workflows/{WORKFLOW}/run"),
            Some(&json!({"requestId":run,"id":run,"revision":1,"input":"Keep input"})),
        )
        .await?;
        let graph = json!({"entry":"out","maxSteps":1,"nodes":[{"id":"out","kind":kind}]});
        f.db(|db| {
            db.execute(
                "UPDATE workflow_runs SET graph=? WHERE id=?",
                params![graph.to_string(), run],
            )?;
            Ok(())
        })?;
        f.tick()?;
        let data = f.request(&format!("/workflow-runs/{run}"), None).await?["data"].clone();
        let reason = format!("Unsupported workflow step type: {kind}.");
        assert_eq!(data["status"], "failed");
        assert_eq!(data["error"], reason);
        assert_eq!(data["steps"][0]["status"], "failed");
        assert_eq!(data["steps"][0]["detail"], reason);
    }
    Ok(())
}

#[tokio::test]
async fn workflow_schedule_rotates_batches_past_offline_homes() -> Result<()> {
    let now = epoch("2024-01-01T00:00:00Z");
    let f = Fixture::new(now)?;
    for index in 0..101 {
        let workflow = format!("10000000-0000-4000-9000-{index:012}");
        f.workflow(&workflow).await?;
        f.save_schedule(
            &workflow,
            &scheduled(json!({"kind":"instant","at":now}), 0, true),
        )
        .await?;
    }
    let occurrences = || {
        f.db(|db| {
            all(
                db,
                "SELECT occurrence_id,due_at,state FROM occurrences ORDER BY occurrence_id",
                [],
            )
        })
    };
    f.tick()?;
    let first = occurrences()?;
    assert_eq!(first.len(), 100);
    assert!(first
        .iter()
        .all(|row| row["due_at"] == now && row["state"] == "pending"));
    let f = f.restart()?;
    let occurrences = || {
        f.db(|db| {
            all(
                db,
                "SELECT occurrence_id,due_at,state FROM occurrences ORDER BY occurrence_id",
                [],
            )
        })
    };
    f.tick()?;
    assert_eq!(occurrences()?.len(), 101);
    f.tick()?;
    let final_rows = occurrences()?;
    assert_eq!(final_rows.len(), 101);
    assert!(first.iter().all(|row| final_rows.contains(row)));
    Ok(())
}

#[tokio::test]
async fn workflow_instants_and_once_cannot_replay_after_metadata_or_timing_changes() -> Result<()> {
    let now = epoch("2024-01-02T02:00:00Z");
    for spec in [
        json!({"kind":"instant","at":now}),
        json!({"kind":"once","date":"2024-01-02","time":"09:00"}),
    ] {
        let f = Fixture::new(now)?;
        f.workflow(WORKFLOW).await?;
        let body = scheduled(spec.clone(), 0, true);
        let accepted = f.save_schedule(WORKFLOW, &body).await?;
        f.ready()?;
        f.tick()?;
        assert_eq!(
            f.save_schedule(WORKFLOW, &body).await?["receipt"],
            accepted["receipt"]
        );
        assert_eq!(
            f.save_schedule(WORKFLOW, &scheduled(spec.clone(), 1, true))
                .await
                .err()
                .ok_or_else(|| fixture_error("consumed schedule"))?
                .code,
            "schedule_processed"
        );
        f.save_schedule(WORKFLOW, &scheduled(spec.clone(), 1, false))
            .await?;
        assert_eq!(
            f.save_schedule(WORKFLOW, &scheduled(spec.clone(), 2, true))
                .await
                .err()
                .ok_or_else(|| fixture_error("disabled consumed schedule"))?
                .code,
            "schedule_processed"
        );
        f.save_schedule(
            WORKFLOW,
            &scheduled(json!({"kind":"instant","at":now+600000}), 2, false),
        )
        .await?;
        assert_eq!(
            f.save_schedule(WORKFLOW, &scheduled(spec, 3, true))
                .await
                .err()
                .ok_or_else(|| fixture_error("superseded consumed schedule"))?
                .code,
            "schedule_processed"
        );
        f.tick()?;
        assert_eq!(
            f.schedule(WORKFLOW).await?["occurrences"]
                .as_array()
                .ok_or_else(|| fixture_error("occurrences"))?
                .len(),
            1
        );
    }
    Ok(())
}

#[tokio::test]
async fn workflow_schedule_receipts_and_occurrence_observations_roll_back_atomically() -> Result<()>
{
    let now = epoch("2024-01-01T00:00:00Z");
    let f = Fixture::new(now)?;
    f.workflow(WORKFLOW).await?;
    let command = scheduled(
        json!({"kind":"interval","amount":5,"unit":"minute","anchorAt":now-960000}),
        0,
        true,
    );
    f.db(|db| {
        db.execute_batch("CREATE TRIGGER fail_schedule BEFORE INSERT ON receipts WHEN NEW.scope LIKE '%/schedule' BEGIN SELECT RAISE(ABORT,'fixture receipt error'); END")?;
        Ok(())
    })?;
    assert_eq!(
        f.save_schedule(WORKFLOW, &command)
            .await
            .err()
            .ok_or_else(|| fixture_error("receipt fault"))?
            .code,
        "storage_failed"
    );
    assert_eq!(f.schedule(WORKFLOW).await?["schedule"], Value::Null);
    f.db(|db| {
        assert!(!exists(
            db,
            "SELECT 1 FROM sync_events WHERE record_kind='workflow_schedule'",
            []
        )?);
        db.execute_batch("DROP TRIGGER fail_schedule")?;
        Ok(())
    })?;
    let saved = f.save_schedule(WORKFLOW, &command).await?;
    f.tick()?;
    let before = f.schedule(WORKFLOW).await?;
    let pending = &before["occurrences"][0];
    assert_eq!(pending["missedCount"], 3);
    assert_eq!(pending["state"], "pending");
    let identity = workflow_identity(&json!([WORKFLOW, 1, 1, now - 960000, 0]));
    assert_eq!(pending["occurrenceId"], identity);
    f.ready()?;
    let events = || f.db(|db| all(db, "SELECT * FROM sync_events ORDER BY position", []));
    let before_events = events()?;
    for kind in ["workflow_run", "workflow_schedule"] {
        f.db(|db| {
            db.execute_batch(&format!("CREATE TRIGGER fail_run BEFORE INSERT ON sync_events WHEN NEW.record_kind='{kind}' BEGIN SELECT RAISE(ABORT,'fixture event fault'); END"))?;
            Ok(())
        })?;
        assert_eq!(
            f.tick()
                .err()
                .ok_or_else(|| fixture_error("event fault"))?
                .code,
            "storage_failed"
        );
        assert_eq!(f.schedule(WORKFLOW).await?, before);
        assert_eq!(events()?, before_events);
        f.db(|db| {
            assert!(!exists(db, "SELECT 1 FROM workflow_runs", [])?);
            db.execute_batch("DROP TRIGGER fail_run")?;
            Ok(())
        })?;
    }
    f.tick()?;
    f.tick()?;
    let current = f.schedule(WORKFLOW).await?;
    assert_eq!(current["occurrences"][0]["occurrenceId"], identity);
    assert_eq!(current["occurrences"][0]["state"], "admitted");
    assert_eq!(current["nextAt"], now + 240000);
    assert_eq!(current["revision"], 1);
    let path = format!(
        "/workflow-runs/{}",
        string(&current["occurrences"][0], "runId")
    );
    assert_eq!(f.request(&path, None).await?["data"]["status"], "done");
    assert_eq!(
        f.save_schedule(WORKFLOW, &command).await?["receipt"],
        saved["receipt"]
    );
    let after_events = events()?;
    f.tick()?;
    assert_eq!(events()?, after_events);
    Ok(())
}

#[tokio::test]
async fn workflow_weekday_schedule_uses_home_zone() -> Result<()> {
    let f = Fixture::new(epoch("2024-01-01T00:00:00Z"))?;
    f.workflow(WORKFLOW).await?;
    let spec = json!({"kind":"cron","expression":"0 9 * * 1-5"});
    let accepted = f
        .save_schedule(WORKFLOW, &scheduled(spec.clone(), 0, true))
        .await?;
    assert_eq!(accepted["data"]["nextAt"], epoch("2024-01-01T02:00:00Z"));
    assert_eq!(accepted["data"]["spec"], spec);
    assert_eq!(accepted["data"]["zone"], "Asia/Jakarta");
    assert_eq!(f.schedule(WORKFLOW).await?["occurrences"], json!([]));
    Ok(())
}

#[tokio::test]
async fn workflow_zone_edit_retains_pending_elapsed_identity_and_consumed_instant() -> Result<()> {
    let now = epoch("2024-01-01T00:00:00Z");
    let mut f = Fixture::new(now)?;
    f.workflow(WORKFLOW).await?;
    f.save_schedule(
        WORKFLOW,
        &scheduled(json!({"kind":"instant","at":now}), 0, true),
    )
    .await?;
    f.tick()?;
    let pending = f.schedule(WORKFLOW).await?["occurrences"].clone();
    f.zone("Europe/London", 2)?;
    f.tick()?;
    let zoned = f.schedule(WORKFLOW).await?;
    assert_eq!(zoned["zoneRevision"], 2);
    assert_eq!(zoned["occurrences"], pending);
    f.ready()?;
    f.tick()?;
    let admitted = f.schedule(WORKFLOW).await?;
    assert_eq!(
        admitted["occurrences"][0]["occurrenceId"],
        pending[0]["occurrenceId"]
    );
    assert_eq!(admitted["occurrences"][0]["zoneRevision"], 1);
    assert_eq!(admitted["occurrences"][0]["state"], "admitted");
    assert_eq!(admitted["enabled"], false);
    assert_eq!(admitted["nextAt"], Value::Null);
    f.clock(now + 3600000);
    f.ready()?;
    f.tick()?;
    assert_eq!(f.schedule(WORKFLOW).await?, admitted);
    Ok(())
}

#[tokio::test]
async fn workflow_cron_catches_up_once_after_offline_ticks_and_failed_admission() -> Result<()> {
    let mut f = Fixture::new(epoch("2024-01-01T00:00:00Z"))?;
    f.workflow(WORKFLOW).await?;
    let command = scheduled(json!({"kind":"cron","expression":"*/5 * * * *"}), 0, true);
    let saved = f.save_schedule(WORKFLOW, &command).await?;
    f.clock(epoch("2024-01-01T00:16:00Z"));
    f.tick()?;
    let first = f.schedule(WORKFLOW).await?["occurrences"].clone();
    assert_eq!(first[0]["missedCount"], 2);
    f.tick()?;
    assert_eq!(f.schedule(WORKFLOW).await?["occurrences"], first);
    assert_eq!(first[0]["dueAt"], epoch("2024-01-01T00:05:00Z"));
    assert_eq!(
        first[0]["occurrenceId"],
        workflow_identity(&json!([WORKFLOW, 1, 1, "2024-01-01T07:05", 0]))
    );
    let mut f = f.restart()?;
    f.clock(epoch("2024-01-01T00:26:00Z"));
    f.tick()?;
    let pending = f.schedule(WORKFLOW).await?["occurrences"].clone();
    assert_eq!(pending[0]["missedCount"], 4);
    assert_eq!(pending[0]["occurrenceId"], first[0]["occurrenceId"]);
    f.ready()?;
    f.db(|db| {
        db.execute_batch("CREATE TRIGGER fail_cron BEFORE INSERT ON sync_events WHEN NEW.record_kind='workflow_run' BEGIN SELECT RAISE(ABORT,'fixture event fault'); END")?;
        Ok(())
    })?;
    assert_eq!(
        f.tick()
            .err()
            .ok_or_else(|| fixture_error("failed admission"))?
            .code,
        "storage_failed"
    );
    assert_eq!(f.schedule(WORKFLOW).await?["occurrences"], pending);
    f.db(|db| {
        db.execute_batch("DROP TRIGGER fail_cron")?;
        Ok(())
    })?;
    f.tick()?;
    f.tick()?;
    let after = f.schedule(WORKFLOW).await?;
    assert_eq!(after["nextAt"], epoch("2024-01-01T00:30:00Z"));
    assert_eq!(
        after["occurrences"]
            .as_array()
            .ok_or_else(|| fixture_error("occurrences"))?
            .len(),
        1
    );
    assert_eq!(
        after["occurrences"][0]["occurrenceId"],
        pending[0]["occurrenceId"]
    );
    assert_eq!(after["occurrences"][0]["state"], "admitted");
    assert_eq!(
        f.save_schedule(WORKFLOW, &command).await?["receipt"],
        saved["receipt"]
    );
    Ok(())
}

#[tokio::test]
async fn workflow_cron_admits_both_fold_instants_with_distinct_identities() -> Result<()> {
    let mut f = Fixture::new(epoch("2024-04-06T13:00:00Z"))?;
    f.zone("Australia/Lord_Howe", 2)?;
    f.workflow(WORKFLOW).await?;
    assert_eq!(
        f.save_schedule(
            WORKFLOW,
            &scheduled(json!({"kind":"cron","expression":"31,58 1 * * *"}), 0, true)
        )
        .await
        .err()
        .ok_or_else(|| fixture_error("three-minute fold interval"))?
        .code,
        "invalid_schedule"
    );
    for (zone, expression, start, first, second, local) in [
        (
            "America/New_York",
            "30 2 * * *",
            "2024-03-09T08:00:00Z",
            "2024-03-11T06:30:00Z",
            "2024-03-12T06:30:00Z",
            None,
        ),
        (
            "America/New_York",
            "30 1 * * *",
            "2024-11-03T04:00:00Z",
            "2024-11-03T05:30:00Z",
            "2024-11-03T06:30:00Z",
            Some("2024-11-03T01:30"),
        ),
        (
            "Australia/Lord_Howe",
            "45 1 * * *",
            "2024-04-06T13:00:00Z",
            "2024-04-06T14:45:00Z",
            "2024-04-06T15:15:00Z",
            Some("2024-04-07T01:45"),
        ),
        (
            "Pacific/Apia",
            "0 9 * * *",
            "2011-12-29T20:00:00Z",
            "2011-12-30T19:00:00Z",
            "2011-12-31T19:00:00Z",
            None,
        ),
    ] {
        f.clock(epoch(start));
        f.zone(zone, 3)?;
        let workflow = uuid::Uuid::new_v4().to_string();
        f.workflow(&workflow).await?;
        let spec = json!({"kind":"cron","expression":expression});
        let saved = f
            .save_schedule(&workflow, &scheduled(spec.clone(), 0, true))
            .await?;
        assert_eq!(saved["data"]["nextAt"], epoch(first));
        f.clock(epoch(first));
        f.ready()?;
        f.tick()?;
        assert_eq!(f.schedule(&workflow).await?["nextAt"], epoch(second));
        if let Some(local) = local {
            let from_second_fold = uuid::Uuid::new_v4().to_string();
            f.workflow(&from_second_fold).await?;
            let created = f
                .save_schedule(&from_second_fold, &scheduled(spec, 0, false))
                .await?;
            assert_eq!(created["data"]["nextAt"], epoch(second));
            f.clock(epoch(second));
            f.ready()?;
            f.tick()?;
            let data = f.schedule(&workflow).await?;
            let rows = data["occurrences"]
                .as_array()
                .ok_or_else(|| fixture_error("occurrences"))?;
            assert_eq!(rows.len(), 2);
            for (index, expected) in [first, second].into_iter().enumerate() {
                assert_eq!(rows[1 - index]["dueAt"], epoch(expected));
                assert_eq!(
                    rows[1 - index]["occurrenceId"],
                    workflow_identity(&json!([
                        workflow,
                        1,
                        saved["data"]["zoneRevision"],
                        local,
                        index
                    ]))
                );
            }
        }
        f.db(|db| {
            db.execute("UPDATE schedules SET enabled=0 WHERE id=?", [&workflow])?;
            Ok(())
        })?;
    }
    Ok(())
}

#[tokio::test]
async fn workflow_interval_metadata_keeps_elapsed_progress() -> Result<()> {
    let now = epoch("2024-01-01T00:00:00Z");
    let f = Fixture::new(now)?;
    f.workflow(WORKFLOW).await?;
    let spec = json!({"kind":"interval","amount":5,"unit":"minute","anchorAt":now-600000});
    f.save_schedule(WORKFLOW, &scheduled(spec.clone(), 0, true))
        .await?;
    f.ready()?;
    f.tick()?;
    let advanced = f.schedule(WORKFLOW).await?;
    assert_eq!(advanced["nextAt"], now + 300000);
    let mut edit = scheduled(spec.clone(), 1, false);
    edit["input"] = json!("Changed metadata");
    f.save_schedule(WORKFLOW, &edit).await?;
    assert_eq!(f.schedule(WORKFLOW).await?["nextAt"], advanced["nextAt"]);
    f.save_schedule(WORKFLOW, &scheduled(spec, 2, true)).await?;
    f.tick()?;
    let current = f.schedule(WORKFLOW).await?;
    assert_eq!(current["occurrences"], advanced["occurrences"]);
    assert_eq!(current["nextAt"], advanced["nextAt"]);
    Ok(())
}

#[tokio::test]
async fn workflow_once_zone_reanchor_preserves_pending_identity_after_restart() -> Result<()> {
    let mut f = Fixture::new(epoch("2024-01-01T00:00:00Z"))?;
    f.workflow(WORKFLOW).await?;
    let spec = json!({"kind":"once","date":"2024-01-02","time":"09:00"});
    f.save_schedule(WORKFLOW, &scheduled(spec.clone(), 0, true))
        .await?;
    f.zone("America/New_York", 2)?;
    let edited = f.save_schedule(WORKFLOW, &scheduled(spec, 1, true)).await?;
    assert_eq!(edited["data"]["nextAt"], epoch("2024-01-02T14:00:00Z"));
    f.zone("Europe/London", 3)?;
    f.tick()?;
    assert_eq!(
        f.schedule(WORKFLOW).await?["nextAt"],
        epoch("2024-01-02T09:00:00Z")
    );
    f.clock(epoch("2024-01-02T10:00:00Z"));
    f.tick()?;
    let pending = f.schedule(WORKFLOW).await?["occurrences"].clone();
    assert_eq!(pending[0]["state"], "pending");
    assert_eq!(
        pending[0]["occurrenceId"],
        workflow_identity(&json!([WORKFLOW, 2, 3, "2024-01-02T09:00", 0]))
    );
    f.zone("Pacific/Honolulu", 4)?;
    let f = f.restart()?;
    f.tick()?;
    assert_eq!(f.schedule(WORKFLOW).await?["occurrences"], pending);
    f.ready()?;
    f.tick()?;
    f.tick()?;
    let consumed = f.schedule(WORKFLOW).await?;
    assert_eq!(consumed["nextAt"], Value::Null);
    assert_eq!(consumed["enabled"], false);
    let rows = consumed["occurrences"]
        .as_array()
        .ok_or_else(|| fixture_error("occurrences"))?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["occurrenceId"], pending[0]["occurrenceId"]);
    assert_eq!(rows[0]["dueAt"], pending[0]["dueAt"]);
    assert_eq!(rows[0]["zoneRevision"], 3);
    Ok(())
}

#[tokio::test]
async fn workflow_scheduler_holds_stale_busy_and_revoked_homes_without_moving_anchor() -> Result<()>
{
    let now = epoch("2024-01-01T00:00:00Z");
    let mut f = Fixture::new(now)?;
    f.workflow(WORKFLOW).await?;
    f.save_schedule(
        WORKFLOW,
        &scheduled(
            json!({"kind":"interval","amount":5,"unit":"minute","anchorAt":now}),
            0,
            true,
        ),
    )
    .await?;
    f.ready()?;
    f.clock(now + 31000);
    f.tick()?;
    let pending = f.schedule(WORKFLOW).await?;
    assert_eq!(pending["nextAt"], now);
    assert_eq!(pending["occurrences"][0]["state"], "pending");
    let mut run = command(1);
    run["input"] = json!("Manual");
    f.request(&format!("/workflows/{WORKFLOW}/run"), Some(&run))
        .await?;
    f.ready()?;
    f.tick()?;
    assert_eq!(f.schedule(WORKFLOW).await?, pending);
    f.db(|db| {
        db.execute("UPDATE devices SET status='revoked' WHERE id=?", [HOME])?;
        Ok(())
    })?;
    f.tick()?;
    assert_eq!(f.schedule(WORKFLOW).await?, pending);
    f.clock(now + 931000);
    f.ready()?;
    f.tick()?;
    let current = f.schedule(WORKFLOW).await?;
    assert_eq!(
        current["occurrences"]
            .as_array()
            .ok_or_else(|| fixture_error("occurrences"))?
            .len(),
        1
    );
    assert_eq!(
        current["occurrences"][0]["occurrenceId"],
        pending["occurrences"][0]["occurrenceId"]
    );
    assert_eq!(current["occurrences"][0]["missedCount"], 3);
    assert_eq!(current["nextAt"], now + 1200000);
    Ok(())
}

#[tokio::test]
async fn workflow_assignment_revision_fences_admission_but_logout_keeps_saved_actor() -> Result<()>
{
    let now = epoch("2024-01-01T00:00:00Z");
    let f = Fixture::new(now)?;
    f.workflow(WORKFLOW).await?;
    let actor = crate::auth::Actor {
        account: uuid::Uuid::new_v4().to_string(),
        subject: "collaborator".to_owned(),
        session: "member".to_owned(),
        expires: now + 60000,
    };
    let member = actor.account.as_str();
    f.db(|db| {
        db.execute("INSERT INTO accounts(id,issuer,clerk_sub,created) VALUES(?,'fixture','collaborator',0)", [member])?;
        db.execute("INSERT INTO project_members VALUES(?,?,'member',1,0)", [PROJECT, member])?;
        db.execute("INSERT INTO collaborator_agents VALUES(?,?,?,1,1)", [PROJECT,member,AGENT])?;
        db.execute("INSERT INTO response_policies VALUES(?,?,?,0,1)", [PROJECT,member,AGENT])?;
        db.execute("UPDATE record_scopes SET visibility='project' WHERE root_kind='workflow'", [])?;
        Ok(())
    })?;
    let path = format!("/workflows/{WORKFLOW}/schedule");
    let spec = json!({"kind":"instant","at":now});
    let initial = f
        .request_as(member, &path, Some(&scheduled(spec.clone(), 0, true)))
        .await?;
    assert_eq!(initial["receipt"]["actorId"], member);
    f.tick()?;
    f.ready()?;
    f.db(|db| {
        db.execute(
            "UPDATE collaborator_agents SET enabled=0,revision=2 WHERE account=?",
            [member],
        )?;
        Ok(())
    })?;
    f.tick()?;
    assert_eq!(
        f.schedule(WORKFLOW).await?["occurrences"][0]["state"],
        "pending"
    );
    assert_eq!(
        f.request_as(member, &path, None)
            .await
            .err()
            .ok_or_else(|| fixture_error("revoked schedule reader"))?
            .status,
        404
    );
    f.db(|db| {
        db.execute(
            "UPDATE collaborator_agents SET enabled=1 WHERE account=?",
            [member],
        )?;
        Ok(())
    })?;
    f.tick()?;
    assert_eq!(
        f.schedule(WORKFLOW).await?["occurrences"][0]["state"],
        "pending"
    );
    f.request_as(member, &path, Some(&scheduled(spec, 1, true)))
        .await?;
    f.db(|db| {
        db.execute(
            "INSERT INTO session_denials VALUES('fixture',?,?,?,'logout_complete',?)",
            params![actor.session, member, actor.expires, now],
        )?;
        Ok(())
    })?;
    let auth = crate::auth::Auth::new(
        crate::config::AuthConfig {
            secret_key: "fixture".to_owned(),
            publishable_key: "fixture".to_owned(),
            issuer: "fixture".to_owned(),
            origin: f.state.origin.clone(),
            audience: None,
            development: true,
            jwt_key: None,
            api_url: None,
        },
        None,
    )
    .map_err(|_| Failure::storage())?;
    assert_eq!(
        auth.require_current(&f.state.database, &actor, now)
            .err()
            .ok_or_else(|| fixture_error("logged-out schedule reader"))?
            .code,
        "session_invalid"
    );
    f.tick()?;
    let data = f.schedule(WORKFLOW).await?;
    let rows = data["occurrences"]
        .as_array()
        .ok_or_else(|| fixture_error("schedule occurrences"))?;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["state"], "admitted");
    assert_eq!(rows[0]["assignmentRevision"], 2);
    assert_eq!(rows[0]["initiatorAccountId"], member);
    assert_eq!(rows[1]["state"], "blocked");
    assert_eq!(rows[1]["assignmentRevision"], 1);
    f.db(|db| {
        assert_eq!(
            one(
                db,
                "SELECT actor FROM receipts WHERE request_id=?",
                [string(&rows[0], "runId")]
            )?
            .ok_or_else(|| fixture_error("admission receipt"))?["actor"],
            member
        );
        Ok(())
    })
}

#[tokio::test]
async fn workflow_revoked_step_agent_does_not_starve_later_schedules() -> Result<()> {
    let now = epoch("2024-01-01T00:00:00Z");
    let f = Fixture::new(now)?;
    let bad = "10000000-0000-4000-8000-000000000000";
    let good = WORKFLOW;
    let target = uuid::Uuid::new_v4().to_string();
    f.workflow(bad).await?;
    f.workflow(good).await?;
    f.db(|db| {
        db.execute("INSERT INTO agents(id,name,model,instructions) VALUES(?,'Other','fixture','Help')", [&target])?;
        db.execute("INSERT INTO agent_authority VALUES(?,?,?,NULL,1)", params![target,OWNER,HOME])?;
        db.execute("INSERT INTO agent_projects VALUES(?,?,'','{\"v\":1,\"tools\":[],\"resources\":[]}',1,0)", params![PROJECT,target])?;
        db.execute("INSERT INTO record_scopes VALUES('agents',?,?,?,'owner','agent',?)", params![json!([target]).to_string(),OWNER,PROJECT,target])?;
        Ok(())
    })?;
    let mut update = command(1);
    update["name"] = json!("Unavailable target");
    update["graph"] = json!({"entry":"agent","maxSteps":2,"nodes":[{"id":"agent","kind":"agent","agent":target,"prompt":"Help","next":"out"},{"id":"out","kind":"output"}]});
    f.request(&format!("/workflows/{bad}/edit"), Some(&update))
        .await?;
    for id in [bad, good] {
        f.save_schedule(id, &scheduled(json!({"kind":"instant","at":now}), 0, true))
            .await?;
    }
    f.db(|db| {
        db.execute(
            "UPDATE agent_projects SET deleted=1 WHERE agent=?",
            [&target],
        )?;
        Ok(())
    })?;
    f.ready()?;
    f.tick()?;
    assert_eq!(
        f.schedule(good).await?["occurrences"][0]["state"],
        "admitted"
    );
    assert_eq!(
        f.request(&format!("/workflows/{bad}/runs"), None).await?["data"]["items"],
        json!([])
    );
    Ok(())
}

#[tokio::test]
async fn workflow_schedule_delete_and_restore_disable_admission_and_retain_occurrences(
) -> Result<()> {
    let now = epoch("2024-01-01T00:00:00Z");
    let f = Fixture::new(now)?;
    f.workflow(WORKFLOW).await?;
    let command = scheduled(json!({"kind":"instant","at":now}), 0, true);
    f.save_schedule(WORKFLOW, &command).await?;
    f.tick()?;
    let pending = f.schedule(WORKFLOW).await?["occurrences"][0].clone();
    let archive = f._dir.0.join("backup.sqlite");
    f.db(|db| {
        db.execute("VACUUM INTO ?", [archive.to_string_lossy().as_ref()])?;
        Ok(())
    })?;
    let backup = Connection::open(&archive)?;
    backup.execute_batch("CREATE TABLE minarbot_backup(format TEXT,version INTEGER,created TEXT); INSERT INTO minarbot_backup VALUES('minarbot-canonical-v6',6,'fixture'); CREATE TABLE minarbot_backup_authority(generation TEXT,owner_account TEXT)")?;
    backup.execute(
        "INSERT INTO minarbot_backup_authority SELECT generation,? FROM authority_state",
        [OWNER],
    )?;
    drop(backup);
    let restored_path = f._dir.0.join("restored.sqlite");
    crate::restore::read_only(&archive, &restored_path).map_err(|error| {
        Failure::new(
            500,
            "fixture_assertion",
            format!("Restore schedule archive: {error}"),
        )
    })?;
    let restored = Connection::open(restored_path)?;
    assert_eq!(
        one(
            &restored,
            "SELECT enabled FROM schedules WHERE id=?",
            [WORKFLOW]
        )?
        .ok_or_else(|| fixture_error("retained schedule"))?["enabled"],
        0
    );
    assert_eq!(
        one(
            &restored,
            "SELECT occurrence_id FROM occurrences WHERE schedule=?",
            [WORKFLOW]
        )?
        .ok_or_else(|| fixture_error("retained occurrence"))?["occurrence_id"],
        pending["occurrenceId"]
    );
    f.request(
        &format!("/workflows/{WORKFLOW}/delete"),
        Some(&self::command(1)),
    )
    .await?;
    assert_eq!(f.schedule(WORKFLOW).await?["enabled"], false);
    assert_eq!(
        f.save_schedule(WORKFLOW, &command)
            .await
            .err()
            .ok_or_else(|| fixture_error("deleted schedule cannot replay"))?
            .status,
        404
    );
    f.ready()?;
    f.tick()?;
    assert_eq!(
        f.schedule(WORKFLOW).await?["occurrences"][0]["runId"],
        Value::Null
    );
    Ok(())
}

#[tokio::test]
async fn workflow_schedule_projection_is_atomic_and_omits_private_input() -> Result<()> {
    let now = epoch("2024-01-01T00:00:00Z");
    let f = Fixture::new(now)?;
    f.workflow(WORKFLOW).await?;
    let command = scheduled(json!({"kind":"instant","at":now}), 0, true);
    let before = f.sync(OWNER, None).await?;
    let cursor = string(&before, "nextCursor");
    let events = || {
        f.db(|db| all(db,"SELECT record_id,revision,body_json FROM sync_events WHERE record_kind='workflow_schedule'",[]))
    };
    f.db(|db| {
        db.execute_batch("CREATE TRIGGER fail_schedule_event BEFORE INSERT ON sync_events WHEN NEW.record_kind='workflow_schedule' BEGIN SELECT RAISE(ABORT,'fixture schedule event fault'); END")?;
        Ok(())
    })?;
    assert_eq!(
        f.save_schedule(WORKFLOW, &command)
            .await
            .err()
            .ok_or_else(|| fixture_error("schedule observation rollback"))?
            .code,
        "storage_failed"
    );
    assert_eq!(f.schedule(WORKFLOW).await?["schedule"], Value::Null);
    assert!(events()?.is_empty());
    assert!(sync_events(&f.sync(OWNER, Some(cursor)).await?).is_empty());
    f.db(|db| {
        db.execute_batch("DROP TRIGGER fail_schedule_event")?;
        Ok(())
    })?;
    let accepted = f.save_schedule(WORKFLOW, &command).await?;
    assert_eq!(
        f.save_schedule(WORKFLOW, &command).await?["receipt"],
        accepted["receipt"]
    );
    let rows = events()?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["record_id"], WORKFLOW);
    assert_eq!(rows[0]["revision"], 1);
    let body = data::json(&rows[0]["body_json"])?;
    assert_eq!(
        body["detailRef"],
        format!("/api/projects/{PROJECT}/workflows/{WORKFLOW}/schedule")
    );
    assert_eq!(body["scheduleRevision"], 1);
    assert_eq!(body["homeDeviceId"], HOME);
    assert_eq!(body["nextAt"], now);
    assert!(!body.to_string().contains("Private schedule input"));
    assert!(body.get("spec").is_none());
    let delta = f.sync(OWNER, Some(cursor)).await?;
    assert_eq!(sync_events(&delta).len(), 1);
    assert_eq!(sync_events(&delta)[0]["body"], body);
    let snapshot = f.sync(OWNER, None).await?;
    let event = sync_events(&snapshot)
        .iter()
        .find(|event| event["recordKind"] == "workflow_schedule")
        .ok_or_else(|| fixture_error("schedule snapshot"))?;
    assert_eq!(event["body"], body);
    Ok(())
}

#[tokio::test]
async fn workflow_occurrence_events_commit_with_admission_and_unchanged_ticks_are_quiet(
) -> Result<()> {
    let now = epoch("2024-01-01T00:00:00Z");
    let f = Fixture::new(now)?;
    f.workflow(WORKFLOW).await?;
    let spec = json!({"kind":"interval","amount":5,"unit":"minute","anchorAt":now-600000});
    f.save_schedule(WORKFLOW, &scheduled(spec, 0, true)).await?;
    let before = event_head(&f)?;
    f.db(|db| { db.execute_batch("CREATE TRIGGER fail_occurrence BEFORE INSERT ON sync_events WHEN NEW.record_kind='workflow_schedule_occurrence' BEGIN SELECT RAISE(ABORT,'fixture occurrence fault'); END")?; Ok(()) })?;
    assert_eq!(
        f.schedule_tick()
            .err()
            .ok_or_else(|| fixture_error("occurrence observation rollback"))?
            .code,
        "storage_failed"
    );
    assert_eq!(f.schedule(WORKFLOW).await?["occurrences"], json!([]));
    assert!(events_after(&f, before)?.is_empty());
    f.db(|db| {
        db.execute_batch("DROP TRIGGER fail_occurrence")?;
        Ok(())
    })?;
    f.schedule_tick()?;
    let saved = f.schedule(WORKFLOW).await?["occurrences"].clone();
    let pending = events_after(&f, before)?;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["record_kind"], "workflow_schedule_occurrence");
    assert_eq!(pending[0]["revision"], 1);
    assert_eq!(pending[0]["body"]["state"], "pending");
    assert_eq!(
        pending[0]["record_id"],
        json!([WORKFLOW, saved[0]["occurrenceId"]]).to_string()
    );
    let pending_head = event_head(&f)?;
    f.schedule_tick()?;
    assert!(events_after(&f, pending_head)?.is_empty());
    f.ready()?;
    f.db(|db| { db.execute_batch("CREATE TRIGGER fail_schedule BEFORE INSERT ON sync_events WHEN NEW.record_kind='workflow_schedule' BEGIN SELECT RAISE(ABORT,'fixture admission fault'); END")?; Ok(()) })?;
    assert_eq!(
        f.schedule_tick()
            .err()
            .ok_or_else(|| fixture_error("schedule observation rollback"))?
            .code,
        "storage_failed"
    );
    assert_eq!(f.schedule(WORKFLOW).await?["occurrences"], saved);
    assert_eq!(
        f.request(&format!("/workflows/{WORKFLOW}/runs"), None)
            .await?["data"]["items"],
        json!([])
    );
    assert!(events_after(&f, pending_head)?.is_empty());
    f.db(|db| {
        db.execute_batch("DROP TRIGGER fail_schedule")?;
        Ok(())
    })?;
    f.schedule_tick()?;
    let events = events_after(&f, pending_head)?;
    assert_eq!(
        events
            .iter()
            .map(|e| json!([e["record_kind"], e["revision"]]))
            .collect::<Vec<_>>(),
        [
            json!(["workflow_run", 1]),
            json!(["workflow_schedule", 2]),
            json!(["workflow_schedule_occurrence", 2])
        ]
    );
    let occurrence = &f.schedule(WORKFLOW).await?["occurrences"][0];
    assert_eq!(events[2]["body"]["runId"], occurrence["runId"]);
    assert_eq!(events[2]["body"]["state"], "admitted");
    assert_eq!(events[1]["body"]["scheduleRevision"], 1);
    let admitted = event_head(&f)?;
    f.schedule_tick()?;
    assert!(events_after(&f, admitted)?.is_empty());
    Ok(())
}

#[tokio::test]
async fn workflow_schedule_observation_revisions_cover_block_zone_and_deletion() -> Result<()> {
    let now = epoch("2024-01-01T00:00:00Z");
    let f = Fixture::new(now)?;
    f.workflow(WORKFLOW).await?;
    let spec = json!({"kind":"instant","at":now});
    f.save_schedule(WORKFLOW, &scheduled(spec.clone(), 0, true))
        .await?;
    f.schedule_tick()?;
    let old = f.schedule(WORKFLOW).await?["occurrences"][0].clone();
    let before = event_head(&f)?;
    let mut edit = scheduled(spec, 1, true);
    edit["input"] = json!("Revised input");
    f.save_schedule(WORKFLOW, &edit).await?;
    let events = events_after(&f, before)?;
    assert_eq!(
        events
            .iter()
            .map(|e| json!([e["record_kind"], e["revision"]]))
            .collect::<Vec<_>>(),
        [
            json!(["workflow_schedule", 2]),
            json!(["workflow_schedule_occurrence", 2])
        ]
    );
    assert_eq!(events[1]["body"]["occurrenceId"], old["occurrenceId"]);
    assert_eq!(events[1]["body"]["state"], "blocked");
    let edited = event_head(&f)?;
    f.zone("America/New_York", 2)?;
    f.schedule_tick()?;
    let zoned = events_after(&f, edited)?;
    assert!(zoned.iter().any(
        |e| e["record_kind"] == "workflow_schedule" && e["body"]["zone"] == "America/New_York"
    ));
    let before_delete = event_head(&f)?;
    f.request(&format!("/workflows/{WORKFLOW}/delete"), Some(&command(1)))
        .await?;
    let deleted = events_after(&f, before_delete)?;
    let event = deleted
        .iter()
        .find(|e| e["record_kind"] == "workflow_schedule")
        .ok_or_else(|| fixture_error("schedule tombstone"))?;
    assert_eq!(event["operation"], "delete");
    assert_eq!(event["body"]["enabled"], false);
    assert_eq!(event["body"]["scheduleRevision"], 3);
    assert_eq!(event["body"]["revision"], 4);
    assert_eq!(
        f.schedule(WORKFLOW).await?["occurrences"]
            .as_array()
            .ok_or_else(|| fixture_error("occurrences"))?
            .len(),
        2
    );
    let snapshot = f.sync(OWNER, None).await?;
    assert_eq!(
        sync_events(&snapshot)
            .iter()
            .filter(|event| event["recordKind"] == "workflow_schedule_occurrence")
            .count(),
        2
    );
    Ok(())
}

#[tokio::test]
async fn workflow_cron_reanchors_future_and_pending_times_without_changing_identity() -> Result<()>
{
    let mut f = Fixture::new(epoch("2024-01-01T00:00:00Z"))?;
    f.workflow(WORKFLOW).await?;
    let spec = json!({"kind":"cron","expression":"0 9 * * *"});
    f.save_schedule(WORKFLOW, &scheduled(spec.clone(), 0, true))
        .await?;
    f.zone("America/New_York", 2)?;
    f.schedule_tick()?;
    assert_eq!(
        f.schedule(WORKFLOW).await?["nextAt"],
        epoch("2024-01-01T14:00:00Z")
    );
    f.clock(epoch("2024-01-01T14:00:00Z"));
    f.schedule_tick()?;
    let pending = f.schedule(WORKFLOW).await?["occurrences"].clone();
    f.zone("Invalid/Zone", 3)?;
    f.ready()?;
    f.schedule_tick()?;
    assert_eq!(f.schedule(WORKFLOW).await?["occurrences"], pending);
    f.zone("Asia/Jakarta", 4)?;
    let mut f = f.restart()?;
    f.clock(epoch("2024-01-03T00:00:00Z"));
    f.ready()?;
    f.tick()?;
    let admitted = f.schedule(WORKFLOW).await?;
    assert_eq!(admitted["nextAt"], epoch("2024-01-03T02:00:00Z"));
    assert_eq!(admitted["zoneRevision"], 4);
    assert_eq!(admitted["occurrences"][0]["zoneRevision"], 2);
    assert_eq!(
        admitted["occurrences"][0]["occurrenceId"],
        pending[0]["occurrenceId"]
    );
    assert_eq!(admitted["occurrences"][0]["missedCount"], 1);
    let mut edit = scheduled(spec, 1, false);
    edit["input"] = json!("Metadata only");
    f.save_schedule(WORKFLOW, &edit).await?;
    assert_eq!(f.schedule(WORKFLOW).await?["nextAt"], admitted["nextAt"]);
    Ok(())
}

#[tokio::test]
async fn workflow_cron_counts_old_zone_then_only_future_candidates_in_new_zone() -> Result<()> {
    let mut f = Fixture::new(epoch("2024-01-01T00:00:00Z"))?;
    f.workflow(WORKFLOW).await?;
    let spec = json!({"kind":"cron","expression":"0 9 * * *"});
    f.save_schedule(WORKFLOW, &scheduled(spec, 0, true)).await?;
    f.clock(epoch("2024-01-03T22:00:00Z"));
    f.schedule_tick()?;
    let pending = f.schedule(WORKFLOW).await?["occurrences"].clone();
    assert_eq!(pending[0]["missedCount"], 2);
    f.zone("Pacific/Honolulu", 2)?;
    f.schedule_tick()?;
    assert_eq!(f.schedule(WORKFLOW).await?["occurrences"], pending);
    let mut f = f.restart()?;
    f.schedule_tick()?;
    assert_eq!(f.schedule(WORKFLOW).await?["occurrences"], pending);
    f.clock(epoch("2024-01-04T18:00:00Z"));
    f.schedule_tick()?;
    assert_eq!(f.schedule(WORKFLOW).await?["occurrences"], pending);
    f.clock(epoch("2024-01-04T19:00:00Z"));
    f.schedule_tick()?;
    assert_eq!(
        f.schedule(WORKFLOW).await?["occurrences"][0]["missedCount"],
        3
    );
    f.zone("Asia/Jakarta", 3)?;
    f.schedule_tick()?;
    assert_eq!(
        f.schedule(WORKFLOW).await?["occurrences"][0]["missedCount"],
        3
    );
    f.ready()?;
    f.tick()?;
    let admitted = f.schedule(WORKFLOW).await?;
    assert_eq!(admitted["nextAt"], epoch("2024-01-05T02:00:00Z"));
    assert_eq!(
        admitted["occurrences"][0]["occurrenceId"],
        pending[0]["occurrenceId"]
    );
    assert_eq!(admitted["occurrences"][0]["missedCount"], 3);
    Ok(())
}
