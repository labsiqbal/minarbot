use super::*;

const EPOCH: i64 = 1_795_334_400_000;
type TestResult = crate::db::Result<()>;

fn checked<T>(value: Result<T>) -> crate::db::Result<T> {
    value.map_err(|failure| failure.message.into())
}

struct Fixture {
    state: AppState,
    _scratch: Scratch,
}

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Fixture {
    fn open() -> crate::db::Result<Self> {
        let scratch =
            Scratch(std::env::temp_dir().join(format!("minarbot-goals-{}", uuid::Uuid::new_v4())));
        let config: crate::Config = serde_json::from_value(
            json!({"database":scratch.0.join("test.sqlite"),"fixedClock":EPOCH}),
        )?;
        let database = crate::db::open(&config.database, false, EPOCH)?;
        let db = &database.connection;
        crate::files::init(db)?;
        data::initialize(&database)?;
        checked(super::super::initialize(db))?;
        checked(crate::connectors::initialize(db))?;
        checked(crate::automation::initialize(db))?;
        crate::computer::initialize(db)?;
        db.execute_batch(include_str!("../../data/attention.sql"))?;
        db.execute_batch("INSERT INTO agents(id,name,model,instructions) VALUES('agent','Agent','model','');
             INSERT INTO turns(id,agent,text,status,created) VALUES('root','agent','Goal target','queued','2026-11-22');")?;
        let computer = Arc::new(crate::computer::Manager::new(&config)?);
        Ok(Self {
            state: AppState {
                database: Arc::new(Mutex::new(database)),
                config: Arc::new(config),
                origin: "http://127.0.0.1:42141".into(),
                auth: None,
                events: crate::events::Hub::new(),
                turns: Runtime::default(),
                whatsapp: Default::default(),
                trial_pending: Default::default(),
                computer,
            },
            _scratch: scratch,
        })
    }

    fn start(&self, db: &Connection, turns: i64, tokens: i64) -> crate::db::Result<()> {
        checked(attach(
            db,
            &json!({"id":"root","agent":"agent"}),
            &json!({
                "target":"Goal target","maxTurns":turns,"maxMinutes":2,"maxTokens":tokens
            }),
            EPOCH,
        ))
    }
}

fn settle(fixture: &Fixture, db: &Connection, met: bool, reason: &str) -> crate::db::Result<()> {
    let turn: String = db.query_row(
        "SELECT current_turn FROM goals WHERE id='root'",
        [],
        |row| row.get(0),
    )?;
    db.execute("UPDATE turns SET status='done' WHERE id=?", [&turn])?;
    checked(finish(
        &fixture.state,
        db,
        &json!({"id":turn,"agent":"agent"}),
        Judgment {
            goal: Some("root".into()),
            routine: None,
            verdict: Some(Verdict {
                met,
                reason: reason.into(),
                evidence: vec!["answer".into()],
            }),
            signature: reason.into(),
            skipped: None,
        },
    ))
}

fn status(db: &Connection) -> crate::db::Result<String> {
    Ok(
        db.query_row("SELECT status FROM goals WHERE id='root'", [], |row| {
            row.get(0)
        })?,
    )
}

#[test]
fn judge_requires_cited_evidence_and_reports_missing_work() -> TestResult {
    let evidence = vec![json!({"id":"check-1","output":"2 tests passed"})];
    let parse = |raw| Verdict::parse(raw, &evidence).ok_or("Missing verdict");
    assert!(parse(r#"{"met":true,"reason":"Both checks passed.","evidence":["check-1"]}"#)?.met);
    assert_eq!(
        parse(r#"{"met":false,"reason":"Still need the build.","evidence":[]}"#)?.reason,
        "Still need the build."
    );
    assert!(!parse(r#"{"met":true,"reason":"Looks done.","evidence":[]}"#)?.met);
    assert!(Verdict::parse(
        r#"{"met":true,"reason":"Done.","evidence":["invented"]}"#,
        &evidence
    )
    .is_none());
    Ok(())
}

#[test]
fn unmet_goals_continue_until_met_turn_limit_or_repeated_failure() -> TestResult {
    for (max, reasons, met, expected) in [
        (20, vec!["First missing", "Done"], true, "met"),
        (
            2,
            vec!["First missing", "Second missing"],
            false,
            "turn_limit",
        ),
        (20, vec!["Same", "Same", "Same"], false, "no_progress"),
    ] {
        let fixture = Fixture::open()?;
        let guard = fixture.state.database.lock().map_err(|_| "Database lock")?;
        let db = &guard.connection;
        fixture.start(db, max, 250_000)?;
        for (index, reason) in reasons.iter().enumerate() {
            settle(&fixture, db, met && index == reasons.len() - 1, reason)?;
        }
        assert_eq!(status(db)?, expected);
        let verdicts = checked(verdicts(db, "root"))?;
        assert_eq!(verdicts.len(), reasons.len());
        assert_eq!(
            verdicts.last().map(|v| v["reason"].as_str()),
            Some(reasons.last().copied())
        );
    }
    Ok(())
}

#[test]
fn goals_stop_at_time_and_token_limits_and_recover_interrupted_work() -> TestResult {
    for (change, expected) in [
        ("UPDATE goals SET deadline=0", "wall_limit"),
        ("UPDATE goal_turns SET estimated_tokens=99", "token_limit"),
        (
            "UPDATE turns SET status='interrupted',error='Server stopped' WHERE id='root'",
            "interrupted",
        ),
    ] {
        let fixture = Fixture::open()?;
        let guard = fixture.state.database.lock().map_err(|_| "Database lock")?;
        let db = &guard.connection;
        fixture.start(db, 20, 99)?;
        db.execute_batch(change)?;
        if expected == "token_limit" {
            settle(&fixture, db, false, "Remaining work")?;
        } else {
            checked(maintain(&fixture.state, db))?;
        }
        assert_eq!(status(db)?, expected);
        let count: i64 = db.query_row("SELECT count(*) FROM goal_turns", [], |row| row.get(0))?;
        assert_eq!(count, 1);
    }
    Ok(())
}

#[test]
fn idle_goal_maintenance_does_not_wake_the_scheduler() -> TestResult {
    let fixture = Fixture::open()?;
    let guard = fixture.state.database.lock().map_err(|_| "Database lock")?;
    let db = &guard.connection;
    fixture.start(db, 20, 250_000)?;
    let commits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = commits.clone();
    db.commit_hook(Some(move || {
        observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        false
    }));
    checked(maintain(&fixture.state, db))?;
    assert_eq!(commits.load(std::sync::atomic::Ordering::Relaxed), 0);
    Ok(())
}

#[test]
fn goal_reports_approval_pause_and_stop_cancels_the_current_turn() -> TestResult {
    let fixture = Fixture::open()?;
    let guard = fixture.state.database.lock().map_err(|_| "Database lock")?;
    let db = &guard.connection;
    fixture.start(db, 20, 250_000)?;
    db.execute_batch(
        "UPDATE turns SET status='running' WHERE id='root';
        INSERT INTO harness_approvals(id,turn,agent,runtime,workspace,kind,description,input,state)
        VALUES('approval','root','agent','codex','/tmp','command','Approve','{}','approval');",
    )?;
    assert_eq!(checked(read(db, "agents:agent"))?["status"], "needs_you");
    db.execute(
        "UPDATE harness_approvals SET state='decided' WHERE id='approval'",
        [],
    )?;
    assert_eq!(checked(read(db, "agents:agent"))?["status"], "active");
    let goal = checked(active(db, "root"))?.ok_or("Missing goal")?;
    checked(cancel(
        &fixture.state,
        db,
        &goal,
        "stopped",
        "Stopped by you.",
    ))?;
    assert_eq!(status(db)?, "stopped");
    let turn =
        checked(one(db, "SELECT status FROM turns WHERE id='root'", []))?.ok_or("Missing turn")?;
    assert_eq!(turn["status"], "cancelled");
    Ok(())
}

#[test]
fn loop_until_and_run_limit_disable_the_routine() -> TestResult {
    let fixture = Fixture::open()?;
    let guard = fixture.state.database.lock().map_err(|_| "Database lock")?;
    let db = &guard.connection;
    db.execute_batch("UPDATE turns SET status='done' WHERE id='root';
        INSERT INTO routines(id,agent,name,prompt,next_at,interval_ms,enabled) VALUES('loop','agent','Loop','Check',60000,60000,1);
        INSERT INTO routine_loops(routine,request,until_condition,max_runs) VALUES('loop','{}','Ready',3);
        INSERT INTO routine_runs(routine,scheduled_at,turn,missed) VALUES('loop',0,'root',0);")?;
    let settle_loop = |met| {
        checked(finish(
            &fixture.state,
            db,
            &json!({"id":"root","agent":"agent"}),
            Judgment {
                goal: None,
                routine: Some("loop".into()),
                verdict: Some(Verdict {
                    met,
                    reason: "Ready".into(),
                    evidence: vec!["answer".into()],
                }),
                signature: "fixture".into(),
                skipped: None,
            },
        ))
    };
    settle_loop(false)?;
    assert_eq!(
        db.query_row("SELECT enabled FROM routines WHERE id='loop'", [], |row| {
            row.get::<_, i64>(0)
        })?,
        1
    );
    db.execute("DELETE FROM goal_verdicts WHERE turn='root'", [])?;
    settle_loop(true)?;
    assert_eq!(
        db.query_row("SELECT enabled FROM routines WHERE id='loop'", [], |row| {
            row.get::<_, i64>(0)
        })?,
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT reason FROM routine_loops WHERE routine='loop'",
            [],
            |row| row.get::<_, String>(0)
        )?,
        "Ready"
    );
    Ok(())
}

#[test]
fn allowance_stops_a_goal_before_provider_dispatch() -> TestResult {
    let fixture = Fixture::open()?;
    {
        let guard = fixture.state.database.lock().map_err(|_| "Database lock")?;
        fixture.start(&guard.connection, 20, 8)?;
    }
    let messages = [json!({"role":"user","content":"This input exceeds the goal budget."})];
    let failure = allowance(&fixture.state, "root", &messages)
        .err()
        .ok_or("goal allowance unexpectedly passed")?;
    assert_eq!(failure.status, 409);
    let guard = fixture.state.database.lock().map_err(|_| "Database lock")?;
    assert_eq!(status(&guard.connection)?, "token_limit");
    Ok(())
}
