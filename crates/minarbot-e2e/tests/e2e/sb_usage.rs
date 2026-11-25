//! Usage totals and per-call provider accounting (port of
//! `app/usage.test.ts`).

use crate::sb_support::{after_tool, id, settle};
use minarbot_testkit::{
    control::{Answer, ControlModel},
    prelude::*,
    providers::AgentSpec,
    server::Gateway,
};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

#[tokio::test]
async fn rust_usage_returns_exact_decimal_totals_unknown_counts_and_stable_pages() -> Result<()> {
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new()).await?;
    let agent = id(&AgentSpec::new("Usage")
        .field("instructions", "Test")
        .create(&app)
        .await?)?;
    let turn = new_id();
    app.edit_database(|db| {
        db.execute(
            "INSERT INTO turns(id,agent,text,status,created) VALUES (?,?,'Usage fixture','done','2026-01-01')",
            (&turn, &agent),
        )?;
        let mut tokens: Vec<Option<i64>> = vec![Some(9_007_199_254_740_991), Some(2)];
        tokens.extend(std::iter::repeat_n(None, 50));
        for value in tokens {
            db.execute(
                "INSERT INTO provider_calls(turn,agent,model,origin,purpose,status,created,input_tokens)
                 VALUES (?,?,'fixture','http://localhost','completion','received','2026-01-01',?)",
                (&turn, &agent, value),
            )?;
        }
        Ok(())
    })
    .await?;
    let first = app.get("/api/usage").await?;
    ensure!(
        first["summary"]["input_tokens"] == "9007199254740993",
        "exact total: {}",
        first["summary"]
    );
    ensure!(
        first["summary"]["total_tokens"].is_null(),
        "total: {}",
        first["summary"]
    );
    ensure!(
        first["summary"]["unknown_input"] == 50,
        "unknown input: {}",
        first["summary"]
    );
    ensure!(
        first["summary"]["unknown_total"] == 52,
        "unknown total: {}",
        first["summary"]
    );
    let calls = first["calls"].as_array().ok_or("calls")?;
    ensure!(calls.len() == 50, "first page: {}", calls.len());
    let second = app
        .get(&format!("/api/usage?before={}", first["next_before"]))
        .await?;
    let more = second["calls"].as_array().ok_or("calls")?;
    ensure!(more.len() == 2, "second page: {}", more.len());
    ensure!(
        second["next_before"].is_null(),
        "next page: {}",
        second["next_before"]
    );
    let seqs: HashSet<String> = calls
        .iter()
        .chain(more)
        .map(|row| row["seq"].to_string())
        .collect();
    ensure!(seqs.len() == 52, "pages overlap: {}", seqs.len());
    Ok(())
}

/// Since #87 every call goes through a provider instance, and the harness
/// writes one usage row per harness call (`origin: local-harness`) with the
/// usage the CLI reports, not one row per direct HTTP request. A tool loop is
/// therefore one row, and the Node `finish_reason: length` mode, a direct
/// client rule, has no instance equivalent. Missing counts, provider
/// failures, receipts, restart stability, routing rows, cancellation and
/// secret redaction carry over.
#[tokio::test]
async fn usage_records_each_provider_call_including_tool_loops_missing_counts_and_failure(
) -> Result<()> {
    let mode = Arc::new(Mutex::new(String::from("known")));
    let model = ControlModel::start().await?;
    let script = mode.clone();
    model.respond(move |request| {
        let mode = script.lock().map(|mode| mode.clone()).unwrap_or_default();
        if request
            .system()
            .starts_with("Choose exactly one channel member")
        {
            let input: Value = serde_json::from_str(&request.last_user_text()).unwrap_or_default();
            return Answer::text(
                json!({"action": "assign", "recipient": input["members"][1]["id"]}).to_string(),
            );
        }
        match mode.as_str() {
            "hold" => Answer::Defer,
            "reject" => Answer::Status(429, json!({"error": "private error"})),
            "malformed" => Answer::Raw("{".into()),
            "tools" if !after_tool(request) => Answer::tool("read_history", json!({})),
            _ => Answer::text("Result"),
        }
    });
    let root = TestRoot::new()?;
    let gateway = Gateway {
        api_key: Some("fixture-secret".into()),
        ..model.gateway()
    };
    let config = ServerConfig::new().gateway(gateway);
    let mut app = root.start(config).await?;
    let agent = id(&app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Usage", "model": "fixture", "instructions": "Test"}),
        )
        .await?)?;
    let path = format!("/api/agents/{agent}/turns");
    for value in ["known", "missing", "tools", "reject", "malformed"] {
        *mode.lock().map_err(|_| "mode")? = value.into();
        model.set_usage((value != "missing").then_some((10, 2)));
        let id = new_id();
        let body = json!({"id": id, "text": value});
        app.post_expect(&path, body.clone(), 202).await?;
        let row = settle(&app, &path, &id).await?;
        let expected = if ["reject", "malformed"].contains(&value) {
            "failed"
        } else {
            "done"
        };
        ensure!(row["status"] == expected, "{value}: {row}");
        let before = model.calls();
        app.post_expect(&path, body, 202).await?;
        ensure!(model.calls() == before, "{value} replayed");
    }
    let report = app.get("/api/usage").await?;
    let summary = &report["summary"];
    ensure!(model.calls() == 6, "model calls: {}", model.calls());
    ensure!(summary["calls"] == 5, "usage calls: {summary}");
    ensure!(summary["failed"] == 2, "failed: {summary}");
    ensure!(summary["pending"] == 0, "pending: {summary}");
    ensure!(
        summary["input_tokens"] == "20"
            && summary["output_tokens"] == "4"
            && summary["total_tokens"] == "24",
        "tokens: {summary}"
    );
    ensure!(summary["unknown_total"] == 3, "unknown counts: {summary}");
    let text = report.to_string();
    ensure!(
        !text.contains("fixture-secret") && !text.contains("private error"),
        "usage leaked provider detail: {report}"
    );
    app.get_expect("/api/usage?before=0", 400).await?;
    app.restart().await?;
    ensure!(
        app.get("/api/usage").await? == report,
        "usage changed across restart"
    );

    let other = id(&app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Other", "model": "other-model", "instructions": "Test"}),
        )
        .await?)?;
    let room = id(&app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Routing", "instructions": "Route", "lead": agent,
                "members": [agent, other]}),
        )
        .await?)?;
    *mode.lock().map_err(|_| "mode")? = "known".into();
    model.set_usage(Some((1, 1)));
    let routed = new_id();
    app.post_expect(
        &format!("/api/channels/{room}/turns"),
        json!({"id": routed, "text": "Route this", "automatic": true}),
        202,
    )
    .await?;
    let routed_usage = until_value(
        "routed usage",
        || async {
            let usage = app.get("/api/usage").await?;
            Ok(usage["calls"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter(|row| row["turn"] == routed.as_str())
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default())
        },
        |rows| rows.len() == 2 && rows.iter().all(|row| row["status"] == "received"),
    )
    .await?;
    ensure!(
        routed_usage[1]["purpose"] == "routing",
        "routing row: {routed_usage:?}"
    );
    ensure!(
        routed_usage[1]["agent"] == agent.as_str(),
        "routing agent: {routed_usage:?}"
    );
    ensure!(
        routed_usage[0]["purpose"] == "completion",
        "completion row: {routed_usage:?}"
    );
    let expected_model = if routed_usage[0]["agent"] == agent.as_str() {
        "fixture"
    } else {
        "other-model"
    };
    ensure!(
        routed_usage[0]["model"] == expected_model,
        "model: {routed_usage:?}"
    );

    *mode.lock().map_err(|_| "mode")? = "hold".into();
    let cancelled = new_id();
    app.post_expect(&path, json!({"id": cancelled, "text": "Cancel"}), 202)
        .await?;
    let held = model.deferred().await?;
    app.post(
        &format!("{path}/{cancelled}/cancel"),
        json!({"id": new_id()}),
    )
    .await?;
    let row = until_value(
        "cancelled usage",
        || async {
            let usage = app.get("/api/usage").await?;
            Ok(usage["calls"]
                .as_array()
                .and_then(|rows| {
                    rows.iter()
                        .find(|row| row["turn"] == cancelled.as_str())
                        .cloned()
                })
                .unwrap_or(Value::Null))
        },
        |row| row["status"] == "failed",
    )
    .await?;
    ensure!(row["total_tokens"].is_null(), "cancelled tokens: {row}");
    drop(held);
    Ok(())
}
