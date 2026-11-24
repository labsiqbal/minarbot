use super::{id, json, until};
use crate::server::{Result, Server, TestRoot};
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
