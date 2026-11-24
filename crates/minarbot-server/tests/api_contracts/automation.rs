use super::{id, json, until};
use crate::{
    loopback::Loopback,
    server::{Result, Server, TestRoot},
};
use axum::{routing::post, Json, Router};
use rusqlite::{params, Connection};
use rustix::process::Signal;
use serde_json::{json, Value};

const CLOCK: i64 = 1_790_000_000_000;

async fn owner(app: &Server<'_>) -> Result<String> {
    let agent = id();
    json(app.post("/api/agents").json(&json!({"id":agent,"runtime":"api","name":"Schedule owner","model":"fixture","instructions":"Help."})), 201).await?;
    Ok(agent)
}

async fn workflow(app: &Server<'_>, owner: &str, name: &str, graph: Value) -> Result<String> {
    let workflow = id();
    let created = json(
        app.post(&format!("/api/agents/{owner}/workflows"))
            .json(&json!({"id":workflow,"revision":0,"name":name,"graph":graph})),
        200,
    )
    .await?;
    assert_eq!(created["id"], workflow);
    Ok(workflow)
}

async fn wait_for(app: &Server<'_>, path: &str, ready: impl Fn(&Value) -> bool) -> Result<Value> {
    until(async || {
        let value = json(app.get(path), 200).await?;
        Ok(ready(&value).then_some(value))
    })
    .await
    .map_err(|error| format!("workflow state did not settle at {path}: {error}").into())
}

#[tokio::test]
async fn offline_schedules_coalesce_only_provider_free_work() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(json!({"fixedClock":CLOCK})).await?;
    let agent = owner(&app).await?;
    let graphs = [
        json!({"entry":"out","maxSteps":2,"nodes":[{"id":"out","kind":"output"}]}),
        json!({"entry":"work","maxSteps":3,"nodes":[{"id":"work","kind":"model","model":"fixture","prompt":"Work","next":"out"},{"id":"out","kind":"output"}]}),
        json!({"entry":"work","maxSteps":4,"nodes":[{"id":"work","kind":"parallel","branches":[{"kind":"model","model":"fixture","prompt":"One"},{"kind":"model","model":"fixture","prompt":"Two"}],"next":"out"},{"id":"out","kind":"output"}]}),
    ];
    let mut workflows = Vec::new();
    for (index, graph) in graphs.into_iter().enumerate() {
        let workflow = workflow(&app, &agent, &format!("Workflow {index}"), graph).await?;
        let path = format!("/api/workflows/{workflow}/schedule");
        json(app.post(&path).json(&json!({"id":id(),"revision":0,"nextAt":CLOCK-180000,"intervalSeconds":60,"input":"Saved input","enabled":true})), 200).await?;
        workflows.push(workflow);
    }
    let path = format!("/api/workflows/{}/schedule", workflows[0]);
    let schedule = wait_for(&app, &path, |data| {
        data["occurrences"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    })
    .await?;
    assert_eq!(schedule["occurrences"].as_array().map(Vec::len), Some(1));
    assert_eq!(schedule["occurrences"][0]["missed"], 3);
    assert_eq!(schedule["schedule"]["next_at"], CLOCK + 60000);
    for workflow in &workflows[1..] {
        let pending = json(app.get(&format!("/api/workflows/{workflow}/schedule")), 200).await?;
        assert_eq!(pending["occurrences"], json!([]));
        assert_eq!(pending["schedule"]["next_at"], CLOCK - 180000);
        assert_eq!(
            json(app.get(&format!("/api/workflows/{workflow}/runs")), 200).await?,
            json!([])
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "legacy direct provider HTTP runtime removed"]
async fn busy_approval_skips_once_and_offline_work_runs_after_restart() -> Result<()> {
    let root = TestRoot::new()?;
    let provider = Loopback::start(Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            Json(json!({"choices":[{"finish_reason":"stop","message":{"content":"yes"}}]}))
        }),
    ))
    .await?;
    let app = root.start(json!({"fixedClock":CLOCK})).await?;
    let agent = owner(&app).await?;
    let out = json!({"id":"out","kind":"output"});
    let approval = workflow(&app, &agent, "Approval", json!({"entry":"review","maxSteps":2,"nodes":[{"id":"review","kind":"approval","prompt":"Accept?","yes":"out","no":"out"},out]})).await?;
    let condition = workflow(&app, &agent, "Provider condition", json!({"entry":"check","maxSteps":2,"nodes":[{"id":"check","kind":"condition","operator":"model","model":"fixture","value":"Accept?","yes":"out","no":"out"},out]})).await?;
    let parallel = workflow(&app, &agent, "Parallel", json!({"entry":"work","maxSteps":4,"nodes":[{"id":"work","kind":"parallel","branches":[{"kind":"model","model":"fixture","prompt":"A"},{"kind":"model","model":"fixture","prompt":"B"}],"next":"out"},out]})).await?;
    let approval_path = format!("/api/workflows/{approval}/schedule");
    let settings = |revision, next: i64, interval: Option<i64>| json!({"id":id(),"revision":revision,"nextAt":next,"intervalSeconds":interval,"input":"Scheduled input","enabled":true});
    json(
        app.post(&approval_path)
            .json(&settings(0, CLOCK - 180000, Some(0))),
        400,
    )
    .await?;
    for workflow in [&condition, &parallel] {
        let path = format!("/api/workflows/{workflow}/schedule");
        json(
            app.post(&path).json(&settings(0, CLOCK - 240000, None)),
            200,
        )
        .await?;
        assert_eq!(json(app.get(&path), 200).await?["occurrences"], json!([]));
    }
    {
        let mut db = Connection::open(root.database())?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for _ in 0..100 {
            let workflow = id();
            tx.execute(
                "INSERT INTO workflows(id,agent,name,revision) VALUES (?,?,'Offline',1)",
                params![workflow, agent],
            )?;
            tx.execute("INSERT INTO workflow_versions SELECT ?,revision,graph FROM workflow_versions WHERE workflow=?", params![workflow, condition])?;
            tx.execute(
                "INSERT INTO workflow_schedules VALUES (?,1,?,NULL,'Offline input',1)",
                params![workflow, CLOCK - 300000],
            )?;
        }
        tx.commit()?;
    }
    json(
        app.post(&approval_path)
            .json(&settings(0, CLOCK - 180000, Some(60))),
        200,
    )
    .await?;
    json(
        app.post(&approval_path)
            .json(&settings(0, CLOCK - 180000, Some(60))),
        409,
    )
    .await?;
    let occurrences =
        |count| move |data: &Value| data["occurrences"].as_array().map(Vec::len) == Some(count);
    let first = wait_for(&app, &approval_path, occurrences(1)).await?;
    assert_eq!(first["occurrences"][0]["missed"], 3);
    assert!(first["occurrences"][0]["reason"].is_null());
    assert_eq!(first["schedule"]["next_at"], CLOCK + 60000);
    let run = first["occurrences"][0]["run"]
        .as_str()
        .ok_or("missing approval run")?;
    let run_path = format!("/api/workflow-runs/{run}");
    let pending = wait_for(&app, &run_path, |run| run["status"] == "approval").await?;
    assert_eq!(pending["input"], "Scheduled input");

    assert!(app.terminate(Signal::TERM).await?.success());
    let app = root.start(json!({"fixedClock":CLOCK+60000})).await?;
    let busy = wait_for(&app, &approval_path, occurrences(2)).await?;
    assert_eq!(busy["occurrences"][0]["scheduled_at"], CLOCK + 60000);
    assert_eq!(busy["occurrences"][0]["reason"], "busy");
    json(
        app.post(&format!("{run_path}/approve"))
            .json(&json!({"id":id(),"revision":pending["revision"],"accepted":true})),
        200,
    )
    .await?;
    wait_for(&app, &run_path, |run| run["status"] == "done").await?;
    let revision = busy["schedule"]["revision"]
        .as_i64()
        .ok_or("missing schedule revision")?;
    json(
        app.post(&approval_path)
            .json(&settings(revision, CLOCK + 60000, Some(60))),
        409,
    )
    .await?;
    json(
        app.post(&approval_path)
            .json(&settings(revision, CLOCK + 120000, None)),
        200,
    )
    .await?;
    assert!(app.terminate(Signal::TERM).await?.success());
    Connection::open(root.database())?.execute_batch(
        "DELETE FROM workflow_schedules WHERE workflow IN (SELECT id FROM workflows WHERE name='Offline');
        DELETE FROM workflow_versions WHERE workflow IN (SELECT id FROM workflows WHERE name='Offline');
        DELETE FROM workflows WHERE name='Offline';",
    )?;
    let app = root
        .start(json!({"fixedClock":CLOCK + 120000,"baseURL":provider.base_url()}))
        .await?;
    let once = wait_for(&app, &approval_path, occurrences(3)).await?;
    assert_eq!(once["schedule"]["enabled"], 0);
    assert_eq!(
        json(app.get(&format!("/api/workflows/{approval}/runs")), 200)
            .await?
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    for workflow in [&condition, &parallel] {
        let path = format!("/api/workflows/{workflow}/schedule");
        let admitted = wait_for(&app, &path, occurrences(1)).await?;
        let run = admitted["occurrences"][0]["run"]
            .as_str()
            .ok_or("missing provider run")?;
        assert_eq!(admitted["schedule"]["enabled"], 0);
        assert_eq!(
            json(app.get(&format!("/api/workflows/{workflow}/runs")), 200)
                .await?
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        let run = wait_for(&app, &format!("/api/workflow-runs/{run}"), |run| {
            run["status"] == "done"
        })
        .await?;
        assert_eq!(run["input"], "Scheduled input");
        assert_eq!(run["cursor"], "out");
        if workflow == &condition {
            assert_eq!(run["last"], "Scheduled input");
        } else {
            let last: Value =
                serde_json::from_str(run["last"].as_str().ok_or("missing parallel output")?)?;
            assert_eq!(
                last,
                json!([{"branch":1,"result":"yes"},{"branch":2,"result":"yes"}])
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn cron_preview_keeps_utc_order_and_rejects_dense_schedules() -> Result<()> {
    let root = TestRoot::new()?;
    let now = "2026-11-01T00:00:00Z"
        .parse::<chrono::DateTime<chrono::Utc>>()?
        .timestamp_millis();
    let app = root.start(json!({"fixedClock":now})).await?;
    let preview = |expression| {
        app.get("/api/routine-schedule").query(&[
            ("zone", "America/Havana".into()),
            (
                "schedule",
                json!({"kind":"cron","expression":expression}).to_string(),
            ),
        ])
    };
    let expected = [
        "2026-11-01T04:00:00Z",
        "2026-11-01T04:30:00Z",
        "2026-11-02T05:00:00Z",
    ]
    .map(str::parse::<chrono::DateTime<chrono::Utc>>)
    .into_iter()
    .map(|instant| instant.map(|instant| instant.timestamp_millis()))
    .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(
        json(preview("0,30 0 * * *"), 200).await?["times"],
        json!(expected)
    );
    assert_eq!(
        json(preview("* * * * *"), 400).await?,
        json!({"error":"Cron interval must be at least five minutes."})
    );
    Ok(())
}
