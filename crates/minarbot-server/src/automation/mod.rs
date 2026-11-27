mod calendar;
pub(crate) mod flows;
pub mod options;
pub(crate) mod project;
mod routines;
pub(crate) mod schedule;
pub mod scripts;
mod webhooks;
pub(crate) mod workflows;

use crate::{
    data::{self, all, error, field, integer, iso, one, string, transaction, uuid},
    web::Failure,
    AppState,
};
use axum::{
    extract::{Request, State},
    http::{HeaderMap, Method},
    response::Response,
    routing::any,
    Router,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
type Result<T> = std::result::Result<T, Failure>;

pub fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(include_str!("schema.sql"))?;
    let follows = all(db, "PRAGMA table_info(routines)", [])?
        .iter()
        .any(|r| r["name"] == "zone_follows");
    for (table, name, ty) in [
        ("routines", "project", "TEXT"),
        ("routines", "report_to", "TEXT NOT NULL DEFAULT 'run'"),
        ("routines", "schedule_json", "TEXT"),
        ("routines", "schedule_zone", "TEXT"),
        ("routines", "webhook_source", "TEXT"),
        ("workflow_runs", "retry_settings", "TEXT"),
        ("workflow_steps", "parallel_group", "INTEGER"),
        ("workflow_steps", "parallel_branch", "INTEGER"),
        ("turns", "workflow_run", "TEXT"),
        ("routines", "zone_follows", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !all(db, &format!("PRAGMA table_info({table})"), [])?
            .iter()
            .any(|r| r["name"] == name)
        {
            db.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {name} {ty}"))?;
        }
    }
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS time_zone_settings(id INTEGER PRIMARY KEY CHECK(id=1),zone TEXT)",
    )?;
    if !follows {
        // Routines on the machine's clock follow the new General time zone.
        db.execute(
            "UPDATE routines SET zone_follows=1 WHERE schedule_zone=? OR schedule_zone IS NULL",
            [schedule::machine_zone()],
        )?;
    }
    let columns = all(db, "PRAGMA table_info(routine_runs)", [])?;
    if columns
        .iter()
        .any(|r| r["name"] == "scheduled_at" && r["notnull"] == 1)
    {
        db.execute_batch("ALTER TABLE routine_runs RENAME TO routine_runs_old; CREATE TABLE routine_runs(routine TEXT NOT NULL REFERENCES routines(id),scheduled_at INTEGER,turn TEXT PRIMARY KEY REFERENCES turns(id),missed INTEGER NOT NULL,UNIQUE(routine,scheduled_at)); INSERT INTO routine_runs SELECT routine,scheduled_at,turn,missed FROM routine_runs_old; DROP TABLE routine_runs_old;")?;
    }
    if !all(db, "PRAGMA table_info(routine_runs)", [])?
        .iter()
        .any(|r| r["name"] == "source")
    {
        db.execute_batch("ALTER TABLE routine_runs ADD COLUMN source TEXT NOT NULL DEFAULT 'scheduled'; UPDATE routine_runs SET source='test' WHERE scheduled_at IS NULL;")?;
    }
    db.execute_batch("WITH missing AS MATERIALIZED (SELECT DISTINCT run,parallel_group FROM workflow_steps s WHERE parallel_group IS NOT NULL AND NOT EXISTS(SELECT 1 FROM workflow_parallel_members m WHERE m.run=s.run AND m.group_seq=s.parallel_group)) INSERT INTO workflow_parallel_members SELECT s.run,s.parallel_group,row_number() OVER(PARTITION BY s.run,s.parallel_group ORDER BY s.seq),s.seq FROM workflow_steps s JOIN missing ON missing.run=s.run AND missing.parallel_group=s.parallel_group;")?;
    Ok(())
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .merge(scripts::routes())
        .route("/api/automation-calendar", any(entry))
        .route("/api/automation-calendar/routines.ics", any(entry))
        .route("/api/routines", any(entry))
        .route("/api/routines/{id}", any(entry))
        .route("/api/routine-schedule", any(entry))
        .route("/api/settings/time-zone", any(entry))
        .route("/api/agents/{id}/routines", any(entry))
        .route("/api/routines/{id}/edit", any(entry))
        .route("/api/routines/{id}/delete", any(entry))
        .route("/api/routines/{id}/test", any(entry))
        .route("/api/routines/{id}/runs", any(entry))
        .route("/api/routines/{id}/flow", any(entry))
        .route("/api/routines/{id}/flow-runs", any(entry))
        .route("/api/routines/{id}/webhook", any(entry))
        .route("/api/routines/{id}/webhook-events", any(entry))
        .route("/api/webhooks/{id}", any(entry))
        .route("/api/workflows/{id}/webhook", any(entry))
        .route("/api/workflows/{id}/webhook-events", any(entry))
        .route("/api/workflow-webhooks/{id}", any(entry))
        .route("/api/workflow-waits/{id}/{seq}", any(entry))
        .route("/api/workflow-waits/{id}/{seq}/events", any(entry))
        .route("/api/flows", any(entry))
        .route("/api/workflows", any(entry))
        .route("/api/agents/{id}/workflows", any(entry))
        .route("/api/workflows/{id}", any(entry))
        .route("/api/workflows/{id}/edit", any(entry))
        .route("/api/workflows/{id}/delete", any(entry))
        .route("/api/workflows/{id}/restore", any(entry))
        .route("/api/workflows/{id}/versions", any(entry))
        .route("/api/workflows/{id}/run", any(entry))
        .route("/api/workflows/{id}/runs", any(entry))
        .route("/api/workflows/{id}/schedule", any(entry))
        .route("/api/workflow-runs/{id}", any(entry))
        .route("/api/workflow-runs/{id}/approve", any(entry))
        .route("/api/workflow-runs/{id}/cancel", any(entry))
        .route("/api/workflow-runs/{id}/retry", any(entry))
        .route("/api/workflow-notifications", any(entry))
        .route("/api/workflow-notifications/{id}/read", any(entry))
        .route("/api/projects/{project}/routines", any(entry))
        .route("/api/projects/{project}/routines/{id}", any(entry))
        .route("/api/projects/{project}/routines/{id}/edit", any(entry))
        .route("/api/projects/{project}/routines/{id}/flow", any(entry))
        .route("/api/projects/{project}/routines/{id}/runs", any(entry))
        .route("/api/projects/{project}/routines/{id}/run", any(entry))
        .route("/api/projects/{project}/routines/{id}/webhook", any(entry))
        .route(
            "/api/projects/{project}/routines/{id}/webhook-events",
            any(entry),
        )
        .route(
            "/api/projects/{project}/agents/{agent}/routines",
            any(entry),
        )
        .route(
            "/api/projects/{project}/agents/{agent}/routines/calendar.ics",
            any(entry),
        )
        .route("/api/projects/{project}/workflows", any(entry))
        .route("/api/projects/{project}/workflows/{id}", any(entry))
        .route(
            "/api/projects/{project}/workflows/{id}/versions",
            any(entry),
        )
        .route("/api/projects/{project}/workflows/{id}/edit", any(entry))
        .route("/api/projects/{project}/workflows/{id}/restore", any(entry))
        .route("/api/projects/{project}/workflows/{id}/delete", any(entry))
        .route("/api/projects/{project}/workflows/{id}/run", any(entry))
        .route("/api/projects/{project}/workflows/{id}/runs", any(entry))
        .route(
            "/api/projects/{project}/workflows/{id}/schedule",
            any(entry),
        )
        .route(
            "/api/projects/{project}/agents/{agent}/workflows",
            any(entry),
        )
        .route("/api/projects/{project}/workflow-runs/{id}", any(entry))
        .route(
            "/api/projects/{project}/workflow-runs/{id}/approve",
            any(entry),
        )
        .route(
            "/api/projects/{project}/workflow-runs/{id}/cancel",
            any(entry),
        )
        .route(
            "/api/projects/{project}/workflow-runs/{id}/retry",
            any(entry),
        )
        .route("/api/projects/{project}/workflow-notifications", any(entry))
        .route(
            "/api/projects/{project}/workflow-notifications/{id}/read",
            any(entry),
        )
}
async fn entry(State(state): State<AppState>, request: Request) -> Response {
    let result = local(&state, request).await;
    let mut response = result.unwrap_or_else(|e| crate::local_error(e.status, &e.message));
    crate::security_headers(&mut response);
    response
}
pub(crate) async fn local(state: &AppState, request: Request) -> Result<Response> {
    if state.auth.is_some() {
        return Ok(crate::canonical(state, request).await);
    }
    let host = crate::text_header(request.headers(), "host").unwrap_or("");
    if format!("http://{host}") != state.origin
        && !state
            .config
            .allowed_origins
            .contains(&format!("https://{host}"))
        || crate::text_header(request.headers(), "sec-fetch-site") == Some("cross-site")
    {
        return Err(error(
            403,
            "Access is only allowed through the local Minarbot address.",
        ));
    }
    if crate::updates::pending(state) {
        return Err(error(503, "Update is being verified. Retry after restart."));
    }
    let path = request.uri().path().to_owned();
    if path.starts_with("/api/webhooks/") || path.starts_with("/api/workflow-webhooks/") {
        return webhooks::incoming(state, request).await;
    }
    if path.starts_with("/api/workflow-waits/") {
        return webhooks::wait(state, request).await;
    }
    let q = data::query(&request);
    let method = request.method().clone();
    let headers = request.headers().clone();
    let v = if method == Method::POST {
        if !data::origin(state, &headers)
            || crate::text_header(&headers, "content-type")
                .unwrap_or("")
                .split(';')
                .next()
                != Some("application/json")
        {
            return Err(error(403, "Request must originate from Minarbot."));
        }
        let bytes = axum::body::to_bytes(request.into_body(), 64000)
            .await
            .map_err(|_| error(413, "Request is too large."))?;
        let v: Value = serde_json::from_slice(&bytes).map_err(|_| error(400, "Invalid JSON."))?;
        if !v.is_object() {
            return Err(error(400, "JSON object is required."));
        }
        if !v["id"].as_str().is_some_and(uuid) {
            return Err(error(400, "Invalid request ID."));
        }
        v
    } else {
        json!({})
    };
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    if path == "/api/automation-calendar" || path == "/api/automation-calendar/routines.ics" {
        if method != Method::GET {
            return Err(error(405, "Method not allowed."));
        }
        let now = state.config.now();
        return if path.ends_with(".ics") {
            calendar::export(db, &q, now)
        } else {
            Ok(crate::json_response(200, calendar::read(db, &q, now)?))
        };
    }
    let p: Vec<_> = path.split('/').skip(1).collect();
    if method == Method::GET && path == "/api/routine-schedule" {
        return Ok(crate::json_response(
            200,
            schedule::preview(db, &q, state.config.now())?,
        ));
    }
    if path == "/api/settings/time-zone" {
        let now = state.config.now();
        let value = match method {
            Method::GET => schedule::settings(db, &v, true, now)?,
            Method::POST => transaction(db, || schedule::settings(db, &v, false, now))?,
            _ => return Err(error(405, "Method not allowed.")),
        };
        return Ok(crate::json_response(200, value));
    }
    let run = || {
        if matches!(p.last(), Some(&"webhook") | Some(&"webhook-events")) {
            return webhooks::settings(state, db, &path, &p, &method, &v);
        }
        if p.contains(&"routines") || p.get(1) == Some(&"flows") {
            routines::route(state, db, &path, &p, &method, &q, &headers, &v)
        } else {
            workflows::route(state, db, &path, &p, &method, &q, &headers, &v)
        }
    };
    let (status, value) = if method == Method::POST {
        let value = transaction(db, run)?;
        if let Err(e) = transaction(db, || workflows::settle(state, db)) {
            eprintln!("Workflow scheduling failed: {}", e.message);
        }
        value
    } else {
        run()?
    };
    Ok(crate::json_response(status, value))
}
pub(crate) fn connector_workflow_command(
    state: &AppState,
    path: &str,
    value: &Value,
) -> Result<()> {
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    let parts: Vec<_> = path.split('/').skip(1).collect();
    transaction(db, || {
        workflows::route(
            state,
            db,
            path,
            &parts,
            &Method::POST,
            &[],
            &HeaderMap::new(),
            value,
        )?;
        Ok(())
    })
}

pub fn start(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut events = state.events.subscribe();
        let canonical = state.auth.is_some();
        let period = std::time::Duration::from_secs(1);
        let start = tokio::time::Instant::now() + period;
        let mut timer = tokio::time::interval_at(start, period);
        loop {
            tokio::select! {
                _ = events.recv(), if canonical => (),
                _ = timer.tick() => (),
            }
            if state.auth.is_some() {
                if crate::tasks::maintain(&state).is_err() {
                    eprintln!("Task wait maintenance failed");
                }
                if let Err(e) = project::maintain(&state) {
                    eprintln!("Project automation failed: {}", e.message);
                }
                continue;
            }
            if crate::turns::pending(&state) {
                continue;
            }
            let result = (|| {
                let guard = state.database.lock().map_err(|_| Failure::storage())?;
                let db = &guard.connection;
                let transaction = rusqlite::Transaction::new_unchecked(
                    db,
                    rusqlite::TransactionBehavior::Deferred,
                )?;
                let before: i64 = db.query_row("SELECT total_changes()", [], |r| r.get(0))?;
                routines::tick(&state, db)?;
                flows::settle(&state, db)?;
                workflows::settle(&state, db)?;
                routines::post_reports(&state, db)?;
                let after: i64 = db.query_row("SELECT total_changes()", [], |r| r.get(0))?;
                if after == before {
                    transaction.rollback()?;
                } else {
                    transaction.commit()?;
                }
                Ok::<_, Failure>(())
            })();
            if let Err(e) = result {
                eprintln!("Automation scheduling failed: {}", e.message);
            }
            tokio::task::yield_now().await;
        }
    })
}
fn cursor(q: &[(String, String)], key: &str, message: &str) -> Result<i64> {
    if q.iter().any(|(k, _)| k != key) || q.iter().filter(|(k, _)| k == key).count() > 1 {
        return Err(error(400, message));
    }
    match data::param(q, key) {
        None => Ok(9007199254740991),
        Some(v) => v
            .parse::<i64>()
            .ok()
            .filter(|n| *n > 0 && *n <= 9007199254740991 && n.to_string() == v)
            .ok_or_else(|| error(400, message)),
    }
}
fn parse_graph(row: &mut Value) -> Result<()> {
    row["graph"] = data::json(&row["graph"])?;
    Ok(())
}
fn command(
    db: &Connection,
    table: &str,
    path: &str,
    v: &Value,
    run: impl FnOnce() -> Result<Value>,
) -> Result<Value> {
    let request = json!({"path":path,"data":v});
    let id = string(v, "id");
    if let Some(old) = data::receipt(
        db,
        table,
        id,
        &request,
        "ID was already used for a different change.",
    )? {
        return Ok(old);
    }
    let result = run()?;
    data::save_receipt(db, table, id, &request, &result)?;
    Ok(result)
}
pub(crate) fn loop_project(
    db: &Connection,
    thread: Option<&str>,
    channel: Option<&str>,
) -> Result<Value> {
    Ok(one(
        db,
        "SELECT project FROM local_project_threads WHERE thread=?1
        UNION ALL SELECT project FROM local_project_sections WHERE channel=?2 LIMIT 1",
        params![thread, channel],
    )?
    .map(|row| row["project"].clone())
    .unwrap_or(Value::Null))
}

pub(crate) fn create_loop(state: &AppState, db: &Connection, agent: &str, v: &Value) -> Result<()> {
    let path = format!("/api/agents/{agent}/routines");
    routines::route(
        state,
        db,
        &path,
        &["api", "agents", agent, "routines"],
        &Method::POST,
        &[],
        &HeaderMap::new(),
        v,
    )?;
    Ok(())
}

pub(crate) fn finish_loop(db: &Connection, routine: &str, turn: &str, reason: &str) -> Result<()> {
    db.execute(
        "UPDATE routine_loops SET reason=? WHERE routine=?",
        params![reason, routine],
    )?;
    db.execute(
        "UPDATE routines SET enabled=0,next_at=coalesce(
        (SELECT scheduled_at FROM routine_runs WHERE turn=? AND routine=routines.id),
        next_at),revision=revision+1 WHERE id=?",
        params![turn, routine],
    )?;
    Ok(())
}

pub(crate) fn enqueue(
    state: &AppState,
    db: &Connection,
    agent: &str,
    name: &str,
    text: &str,
) -> Result<(String, String)> {
    let thread = uuid::Uuid::new_v4().to_string();
    let turn = uuid::Uuid::new_v4().to_string();
    db.execute(
        "INSERT INTO threads(id,agent,name) VALUES(?,?,?)",
        params![thread, agent, name],
    )?;
    crate::turns::enqueue_automation(state, db, agent, &thread, &turn, text)?;
    Ok((turn, thread))
}

pub(crate) fn tools(db: &Connection, turn: &Value) -> Result<Vec<Value>> {
    let mut tools = Vec::new();
    if data::exists(
        db,
        "SELECT 1 FROM routine_runs WHERE turn=? UNION SELECT 1 FROM flow_steps WHERE turn=?",
        [string(turn, "id"), string(turn, "id")],
    )? {
        tools.push(json!({"type":"function","function":{"name":"no_update","description":"Finish a routine quietly when all checks completed without updates.","parameters":{"type":"object","properties":{"reason":{"type":"string","maxLength":2000}},"required":["reason"],"additionalProperties":false}}}));
    }
    if !tools.is_empty() {
        tools.push(json!({"type":"function","function":{"name":"report_failure","description":"Report that the task cannot be completed. Describe the blocker.","parameters":{"type":"object","properties":{"reason":{"type":"string","maxLength":2000}},"required":["reason"],"additionalProperties":false}}}));
    }
    if tools.len()>1 && data::exists(db,"SELECT 1 FROM routine_loops l JOIN routine_runs r USING(routine) WHERE r.turn=? AND l.until_condition IS NOT NULL",
        [string(turn,"id")])? {
        tools.retain(|t|t["function"]["name"]!="no_update");
    }
    Ok(tools)
}
pub(crate) fn tool(db: &Connection, turn: &Value, name: &str, args: &Value) -> Result<Value> {
    match name {
        "list_routines" => {
            if !data::closed(args, &["after"])
                || args
                    .get("after")
                    .is_some_and(|v| !v.as_str().is_some_and(uuid))
            {
                return Err(error(400, "Invalid routine list position. Omit after for the first page, or pass the UUID from next_after; stop when next_after is null."));
            }
            let rows=all(db,"SELECT id,name,enabled,trigger,next_at,interval_ms,revision FROM routines WHERE agent=? AND id NOT IN(SELECT routine FROM local_routine_deletions) AND id>? ORDER BY id LIMIT 11",params![string(turn,"agent"),args["after"].as_str().unwrap_or("")])?;
            let next = if rows.len() > 10 {
                rows[9]["id"].clone()
            } else {
                Value::Null
            };
            Ok(json!({"routines":rows.into_iter().take(10).collect::<Vec<_>>(),"next_after":next}))
        }
        "read_routine" => {
            if !data::closed(args, &["id"]) || !args["id"].as_str().is_some_and(uuid) {
                return Err(error(400, "Invalid routine ID."));
            }
            let Some(r)=one(db,"SELECT id,name,prompt,enabled,trigger,next_at,interval_ms,revision FROM routines WHERE id=? AND agent=? AND id NOT IN(SELECT routine FROM local_routine_deletions)",params![string(args,"id"),string(turn,"agent")])? else {return Ok(json!({"error":"Routine not found for this agent."}));};
            let flow = one(
                db,
                "SELECT graph,revision FROM routine_flows WHERE routine=?",
                [string(args, "id")],
            )?;
            let flow = match flow {
                Some(mut f) => {
                    parse_graph(&mut f)?;
                    f
                }
                None => {
                    json!({"graph":{"root":turn["agent"],"nodes":[turn["agent"]],"links":[]},"revision":0})
                }
            };
            let mut result = r;
            result["flow"] = flow;
            Ok(result)
        }
        "set_routine_flow" => {
            if !data::closed(args, &["id", "revision", "links"])
                || !args["id"].as_str().is_some_and(uuid)
            {
                return Err(error(400, "Invalid flow request."));
            }
            flows::save(db, string(args, "id"), string(turn, "agent"), args)
        }
        "no_update" | "report_failure" => {
            if !data::closed(args, &["reason"]) {
                return Err(error(400, "Invalid task outcome."));
            }
            let reason = field(args, "reason", 2000)?;
            if name == "no_update" {
                if !data::exists(
                    db,
                    "SELECT 1 FROM routine_runs WHERE turn=? UNION SELECT 1 FROM flow_steps WHERE turn=?",
                    [string(turn, "id"),string(turn, "id")],
                )? {
                    return Err(error(400, "No-update is only available for routine runs."));
                }
                db.execute("UPDATE turns SET status='done',no_update=1,reply=?,revision=revision+1 WHERE id=?",params![reason,string(turn,"id")])?;
            } else {
                db.execute(
                    "UPDATE turns SET status='failed',error=?,revision=revision+1 WHERE id=?",
                    params![reason, string(turn, "id")],
                )?;
            }
            Ok(json!({"recorded":true}))
        }
        _ => Err(error(400, "Unknown automation tool.")),
    }
}
pub(crate) fn model(db: &Connection, turn: &str) -> Result<Option<String>> {
    Ok(one(
        db,
        "SELECT model FROM workflow_steps WHERE turn=? AND model IS NOT NULL",
        [turn],
    )?
    .map(|r| string(&r, "model").to_owned()))
}
