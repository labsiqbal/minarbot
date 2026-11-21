mod routines;
mod workflows;

use super::*;
use axum::body::to_bytes;
use std::{
    path::PathBuf,
    sync::{atomic::AtomicBool, Arc, Mutex},
};

const OWNER: &str = "10000000-0000-4000-8000-000000000001";
const PROJECT: &str = "10000000-0000-4000-8000-000000000002";
const AGENT: &str = "10000000-0000-4000-8000-000000000003";
const HOME: &str = "10000000-0000-4000-8000-000000000004";
const INSTALLATION: &str = "10000000-0000-4000-8000-000000000005";
const WORKFLOW: &str = "10000000-0000-4000-8000-000000000006";

struct TestDirectory(PathBuf);

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    state: AppState,
    _dir: TestDirectory,
}

impl Fixture {
    fn new(clock: i64) -> Result<Self> {
        let dir = TestDirectory(
            std::env::temp_dir().join(format!("minarbot-project-test-{}", uuid::Uuid::new_v4())),
        );
        let config: crate::config::Config = serde_json::from_value(json!({
            "database":dir.0.join("state.sqlite"),"fixedClock":clock,"testKeyring":{},
        }))
        .map_err(|_| Failure::storage())?;
        let database =
            crate::db::open(&config.database, true, clock).map_err(|_| Failure::storage())?;
        crate::db::provider_tables(&database.connection).map_err(|_| Failure::storage())?;
        data::initialize_canonical(&database).map_err(|_| Failure::storage())?;
        crate::turns::initialize_canonical(&database.connection)?;
        crate::computer::initialize_canonical(&database.connection)
            .map_err(|_| Failure::storage())?;
        initialize(&database.connection)?;
        let db = &database.connection;
        db.execute(
            "INSERT INTO accounts(id,issuer,clerk_sub,created) VALUES(?,'fixture','owner',0)",
            [OWNER],
        )?;
        db.execute(
            "INSERT INTO projects(id,account,name,private) VALUES(?,?,'Automation',0)",
            [PROJECT, OWNER],
        )?;
        db.execute(
            "INSERT INTO project_members VALUES(?,?,'owner',1,0)",
            [PROJECT, OWNER],
        )?;
        db.execute("INSERT INTO agents(id,name,model,instructions) VALUES(?,'Automation','fixture','Private')", [AGENT])?;
        db.execute("INSERT INTO agent_projects VALUES(?,?,'','{\"v\":1,\"tools\":[],\"resources\":[]}',1,0)", [PROJECT, AGENT])?;
        db.execute(
            "INSERT INTO installations VALUES(?,?,'key','installation',1,0,1,0)",
            [INSTALLATION, OWNER],
        )?;
        db.execute("INSERT INTO devices VALUES(?,?,?,'key','key','sig','enc','{}',1,0,'offline','Asia/Jakarta',1,NULL,'[\"execution\"]',1)", [HOME, INSTALLATION, OWNER])?;
        db.execute(
            "INSERT INTO agent_authority VALUES(?,?,?,NULL,1)",
            [AGENT, OWNER, HOME],
        )?;
        db.execute(
            "INSERT INTO record_scopes VALUES('agents',?,?,?,'owner','agent',?)",
            [
                json!([AGENT]).to_string(),
                OWNER.to_owned(),
                PROJECT.to_owned(),
                AGENT.to_owned(),
            ],
        )?;
        let computer =
            Arc::new(crate::computer::Manager::new(&config).map_err(|_| Failure::storage())?);
        let state = AppState {
            database: Arc::new(Mutex::new(database)),
            config: Arc::new(config),
            origin: "http://127.0.0.1".to_owned(),
            auth: None,
            events: crate::events::Hub::new(),
            computer,
            turns: crate::turns::Runtime::default(),
            whatsapp: Default::default(),
            trial_pending: Arc::new(AtomicBool::new(false)),
        };
        Ok(Self { state, _dir: dir })
    }

    fn db<T>(&self, run: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let guard = self.state.database.lock().map_err(|_| Failure::storage())?;
        run(&guard.connection)
    }

    fn restart(self) -> Result<Self> {
        let Self { mut state, _dir } = self;
        drop(state.database);
        let database = crate::db::open(&state.config.database, true, state.config.now())
            .map_err(|_| Failure::storage())?;
        state.database = Arc::new(Mutex::new(database));
        Ok(Self { state, _dir })
    }

    fn clock(&mut self, now: i64) {
        Arc::make_mut(&mut self.state.config).fixed_clock = Some(now);
    }

    fn zone(&self, zone: &str, revision: i64) -> Result<()> {
        self.db(|db| {
            db.execute(
                "UPDATE devices SET zone=?,zone_revision=? WHERE id=?",
                params![zone, revision, HOME],
            )?;
            Ok(())
        })
    }

    fn ready(&self) -> Result<()> {
        self.db(|db| {
            db.execute(
                "UPDATE devices SET status='ready',last_seen=? WHERE id=?",
                params![self.state.config.now(), HOME],
            )?;
            Ok(())
        })
    }

    async fn request(&self, path: &str, body: Option<&Value>) -> Result<Value> {
        self.request_as(OWNER, path, body).await
    }

    async fn request_as(&self, account: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let response = route(
            &self.state,
            &format!("/api/projects/{PROJECT}{path}"),
            &if body.is_some() {
                Method::POST
            } else {
                Method::GET
            },
            body.unwrap_or(&Value::Null),
            account,
            &[],
            &HeaderMap::new(),
        )?
        .ok_or_else(Failure::storage)?;
        let bytes = to_bytes(response.into_body(), 256 * 1024)
            .await
            .map_err(|_| Failure::storage())?;
        serde_json::from_slice(&bytes).map_err(|_| Failure::storage())
    }

    async fn workflow(&self, workflow: &str) -> Result<()> {
        self.request(&format!("/agents/{AGENT}/workflows"), Some(&json!({"requestId":workflow,"id":workflow,"revision":0,"name":"Scheduled","graph":{"entry":"out","maxSteps":1,"nodes":[{"id":"out","kind":"output"}]}}))).await?;
        Ok(())
    }

    async fn save_schedule(&self, workflow: &str, command: &Value) -> Result<Value> {
        self.request(&format!("/workflows/{workflow}/schedule"), Some(command))
            .await
    }

    async fn schedule(&self, workflow: &str) -> Result<Value> {
        Ok(self
            .request(&format!("/workflows/{workflow}/schedule"), None)
            .await?["data"]
            .clone())
    }

    async fn routine(&self, command: &Value) -> Result<Value> {
        self.request(&format!("/agents/{AGENT}/routines"), Some(command))
            .await
    }

    async fn routine_detail(&self, id: &str) -> Result<Value> {
        Ok(self.request(&format!("/routines/{id}"), None).await?["data"].clone())
    }

    async fn routine_runs(&self, id: &str) -> Result<Value> {
        Ok(self.request(&format!("/routines/{id}/runs"), None).await?["data"]["items"].clone())
    }

    async fn sync(&self, account: &str, cursor: Option<&str>) -> Result<Value> {
        let mut query = vec![("projectId".to_owned(), PROJECT.to_owned())];
        if let Some(cursor) = cursor {
            query.push(("cursor".to_owned(), cursor.to_owned()));
        }
        sync_response(crate::data::sync::read(&self.state, account, &query)?).await
    }

    fn add_agent(&self, id: &str) -> Result<()> {
        self.db(|db| {
            db.execute("INSERT INTO agents(id,name,model,instructions) VALUES(?,'Other','fixture','PRIVATE_INSTRUCTIONS')", [id])?;
            db.execute("INSERT INTO agent_authority VALUES(?,?,?,NULL,1)", params![id,OWNER,HOME])?;
            db.execute("INSERT INTO agent_projects VALUES(?,?,'','{\"v\":1,\"tools\":[],\"resources\":[]}',1,0)", params![PROJECT,id])?;
            db.execute("INSERT INTO record_scopes VALUES('agents',?,?,?,'owner','agent',?)", params![json!([id]).to_string(),OWNER,PROJECT,id])?;
            Ok(())
        })
    }

    fn settle_routine(&self, task: &str) -> Result<()> {
        self.db(|db| {
            db.execute(
                "UPDATE tasks SET state='completed',revision=revision+1 WHERE id=?",
                [task],
            )?;
            db.execute(
                "UPDATE turns SET status='done',reply='PRIVATE_RESULT' WHERE id=?",
                [task],
            )?;
            Ok(())
        })?;
        self.schedule_tick()
    }

    fn schedule_tick(&self) -> Result<()> {
        self.db(|db| scheduler::maintain(&self.state, db))
    }

    fn tick(&self) -> Result<()> {
        maintain(&self.state)
    }
}

fn fixture_error(step: &str) -> Failure {
    Failure::new(
        500,
        "fixture_assertion",
        format!("Unexpected fixture state at {step}"),
    )
}

fn epoch(value: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(value).map_or_else(
        |error| panic!("Invalid fixture timestamp {value}: {error}"),
        |date| date.timestamp_millis(),
    )
}

fn workflow_identity(value: &Value) -> String {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(value.to_string()))
}

fn command(revision: i64) -> Value {
    let id = uuid::Uuid::new_v4().to_string();
    json!({"requestId":id,"id":id,"revision":revision})
}

fn scheduled(spec: Value, revision: i64, enabled: bool) -> Value {
    let mut result = command(revision);
    result["spec"] = spec;
    result["enabled"] = json!(enabled);
    result["input"] = json!("Private schedule input");
    result
}

fn routine(spec: Value, revision: i64, enabled: bool) -> Value {
    let mut result = command(revision);
    result["schedule"] = spec;
    result["enabled"] = json!(enabled);
    result["name"] = json!("Routine");
    result["prompt"] = json!("PRIVATE_ROUTINE_PROMPT");
    result
}

#[tokio::test]
async fn invalid_home_zone_holds_wall_clocks_and_allows_absolute_times() -> Result<()> {
    let now = epoch("2024-01-01T00:00:00Z");
    let mut f = Fixture::new(now)?;
    f.workflow(WORKFLOW).await?;
    let spec = json!({"kind":"once","date":"2024-01-02","time":"09:00"});
    let once = routine(spec.clone(), 0, true);
    let cron = routine(json!({"kind":"cron","expression":"0 9 * * *"}), 0, true);
    f.zone("Invalid/Zone", 2)?;
    for invalid in [&once, &cron] {
        assert_eq!(
            f.routine(invalid)
                .await
                .err()
                .ok_or_else(|| fixture_error("invalid routine home zone"))?
                .status,
            422
        );
    }
    let workflow_once = scheduled(spec, 0, true);
    assert_eq!(
        f.save_schedule(WORKFLOW, &workflow_once)
            .await
            .err()
            .ok_or_else(|| fixture_error("invalid workflow home zone"))?
            .status,
        422
    );
    assert_eq!(f.schedule(WORKFLOW).await?["schedule"], Value::Null);
    f.zone("UTC", 3)?;
    f.save_schedule(WORKFLOW, &workflow_once).await?;
    f.routine(&once).await?;
    f.routine(&cron).await?;
    let instant_spec = json!({"kind":"instant","at":now});
    let instant = routine(instant_spec.clone(), 0, true);
    f.routine(&instant).await?;
    f.zone("Invalid/Zone", 4)?;
    let absolute = uuid::Uuid::new_v4().to_string();
    f.workflow(&absolute).await?;
    f.save_schedule(&absolute, &scheduled(instant_spec, 0, true))
        .await?;
    f.clock(epoch("2024-01-03T00:00:00Z"));
    f.ready()?;
    f.tick()?;
    assert_eq!(f.schedule(WORKFLOW).await?["occurrences"], json!([]));
    assert_eq!(f.routine_runs(string(&once, "id")).await?, json!([]));
    assert_eq!(f.routine_runs(string(&cron, "id")).await?, json!([]));
    assert_eq!(
        f.schedule(&absolute).await?["occurrences"][0]["state"],
        "admitted"
    );
    assert_eq!(
        f.routine_runs(string(&instant, "id")).await?[0]["state"],
        "admitted"
    );
    f.zone("UTC", 5)?;
    f.tick()?;
    assert_eq!(
        f.schedule(WORKFLOW).await?["occurrences"][0]["state"],
        "admitted"
    );
    assert_eq!(
        f.routine_runs(string(&once, "id")).await?[0]["state"],
        "admitted"
    );
    Ok(())
}

fn event_head(f: &Fixture) -> Result<i64> {
    f.db(|db| {
        Ok(db.query_row(
            "SELECT coalesce(max(position),0) FROM sync_events",
            [],
            |row| row.get(0),
        )?)
    })
}

fn events_after(f: &Fixture, position: i64) -> Result<Vec<Value>> {
    f.db(|db| {
        let mut events = all(db, "SELECT record_kind,record_id,operation,revision,body_json FROM sync_events WHERE position>? ORDER BY position", [position])?;
        for event in &mut events {
            event["body"] = data::json(&event["body_json"])?;
        }
        Ok(events)
    })
}

fn flow(revision: i64, links: Value) -> Value {
    let mut body = command(revision);
    body["links"] = links;
    body
}

async fn sync_response(response: Response) -> Result<Value> {
    let bytes = to_bytes(response.into_body(), 256 * 1024)
        .await
        .map_err(|_| Failure::storage())?;
    let body: Value = serde_json::from_slice(&bytes).map_err(|_| Failure::storage())?;
    Ok(body["data"].clone())
}

fn sync_events(page: &Value) -> &[Value] {
    let Some(events) = page["page"]["events"].as_array() else {
        panic!("Missing sync events: {page}");
    };
    events
}

#[tokio::test]
async fn sync_snapshot_continuation_expires_at_sixty_seconds_in_the_same_process() -> Result<()> {
    let start = epoch("2024-01-01T00:00:00Z");
    let mut f = Fixture::new(start)?;
    let project = uuid::Uuid::new_v4().to_string();
    let owner = uuid::Uuid::new_v4().to_string();
    f.db(|db| {
        db.execute(
            "INSERT INTO accounts(id,issuer,clerk_sub,created) VALUES(?,'fixture','expiry',0)",
            [&owner],
        )?;
        db.execute(
            "INSERT INTO projects(id,account,name,private) VALUES(?,?,'Snapshot expiry',0)",
            [&project, &owner],
        )?;
        db.execute(
            "INSERT INTO project_members VALUES(?,?,'owner',1,0)",
            [&project, &owner],
        )?;
        for n in 0..600 {
            let id = format!("20000000-0000-4000-8000-{n:012}");
            db.execute(
                "INSERT INTO accounts(id,issuer,clerk_sub,created) VALUES(?,'fixture',?,0)",
                [&id, &id],
            )?;
            db.execute(
                "INSERT INTO project_members VALUES(?,?,'member',1,0)",
                [&project, &id],
            )?;
        }
        Ok(())
    })?;
    let read = |f: &Fixture, page: Option<&Value>| {
        let mut query = vec![("projectId".to_owned(), project.clone())];
        if let Some(page) = page {
            query.push(("page".to_owned(), string(page, "nextPage").to_owned()));
        }
        crate::data::sync::read(&f.state, &owner, &query)
    };
    let first = sync_response(read(&f, None)?).await?;
    let second = sync_response(read(&f, None)?).await?;
    assert!(first["nextPage"].is_string() && second["nextPage"].is_string());
    f.clock(start + 59999);
    assert_eq!(read(&f, Some(&first))?.status(), axum::http::StatusCode::OK);
    f.clock(start + 60000);
    assert_eq!(
        read(&f, Some(&second))
            .err()
            .ok_or_else(|| fixture_error("expired snapshot"))?
            .code,
        "resync_required"
    );
    assert!(sync_response(read(&f, None)?).await?["nextPage"].is_string());
    crate::data::sync::discard_account_snapshots(&owner);
    Ok(())
}
