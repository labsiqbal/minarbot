//! Turn activity metadata and Stop timestamps (port of
//! `app/transcript.test.ts`).

use minarbot_testkit::{
    control::{Answer, ControlModel},
    prelude::*,
    server::Gateway,
};

use crate::sb_support::{is_time as timestamp, iso_millis};

/// #87 removed the backup-model fallback (`activity.fallback_model`) and the
/// direct provider client, so the first step no longer fails over: each turn
/// is a `remember_memory` tool step and a final answer through the instance,
/// recorded as one harness provider call (the Node case counted three HTTP
/// requests: the failed primary, the backup tool step and the answer).
#[tokio::test]
async fn turn_list_exposes_scoped_read_only_activity_metadata_in_direct_thread_and_channel_views(
) -> Result<()> {
    let model = ControlModel::start().await?;
    model.respond(|request| {
        if request
            .messages()
            .last()
            .is_some_and(|message| message["role"] == "tool")
        {
            Answer::text("## Finished\n\nSafe answer.")
        } else {
            Answer::tool(
                "remember_memory",
                json!({"text": format!("Synthetic transcript check {}", new_id())}),
            )
        }
    });
    let root = TestRoot::new()?;
    let gateway = Gateway {
        api_key: Some("private-provider-key".into()),
        ..model.gateway()
    };
    let app = root.start(ServerConfig::new().gateway(gateway)).await?;
    let agent = app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Transcript", "model": "primary",
                "backupModel": "backup", "instructions": "Test"}),
        )
        .await?;
    let agent_id = agent["id"].as_str().ok_or("agent")?;
    let foreign = app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Other", "model": "backup",
                "instructions": "Test"}),
        )
        .await?;
    let thread = app
        .post(
            &format!("/api/agents/{agent_id}/threads"),
            json!({"id": new_id(), "name": "Side"}),
        )
        .await?;
    let channel = app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Room", "instructions": "Test",
                "members": [agent_id], "lead": agent_id}),
        )
        .await?;
    for path in [
        format!("/api/agents/{agent_id}/turns"),
        format!(
            "/api/threads/{}/turns",
            thread["id"].as_str().ok_or("thread")?
        ),
        format!(
            "/api/channels/{}/turns",
            channel["id"].as_str().ok_or("channel")?
        ),
    ] {
        let id = new_id();
        let mut body = json!({"id": id, "text": "Test activity"});
        if path.contains("/channels/") {
            body["recipient"] = json!(agent_id);
        }
        app.post(&path, body).await?;
        let row = app
            .wait_row(&path, &id, |row| {
                row["status"] == "done" || row["status"] == "failed"
            })
            .await?;
        ensure!(row["status"] == "done", "{path}: {row}");
        let activity = &row["activity"];
        let calls = activity["calls"].as_array().ok_or("calls")?;
        ensure!(calls.len() == 1, "{path} calls: {activity}");
        let tools: Vec<&Value> = activity["tools"]
            .as_array()
            .ok_or("tools")?
            .iter()
            .map(|tool| &tool["name"])
            .collect();
        ensure!(
            tools == [&json!("remember_memory")],
            "{path} tools: {activity}"
        );
        ensure!(
            activity["tools"][0]["failed"] == 0,
            "tool failed: {activity}"
        );
        ensure!(
            activity["tools"][0]["after_action"] == 0,
            "after_action: {activity}"
        );
        ensure!(row["coordination"] == json!([]), "coordination: {row}");
        for call in calls {
            ensure!(
                timestamp(&call["created"]) && timestamp(&call["finished"]),
                "call times: {call}"
            );
            ensure!(
                call.get("account").is_none() && call.get("origin").is_none(),
                "call exposes provider identity: {call}"
            );
        }
        ensure!(
            !activity.to_string().contains("private"),
            "activity leaked provider detail: {activity}"
        );
        let focused = app.get(&format!("{path}?focus={id}")).await?;
        ensure!(focused[0]["activity"] == *activity, "focus view: {focused}");
    }
    ensure!(
        app.get(&format!(
            "/api/agents/{}/turns",
            foreign["id"].as_str().ok_or("foreign")?
        ))
        .await?
            == json!([]),
        "foreign agent saw turns"
    );
    Ok(())
}

/// Stop timestamps need server time to advance, so this case runs on the
/// real clock like the Node case did.
#[tokio::test]
async fn stop_timestamps_survive_repeated_cancellation_and_server_restart() -> Result<()> {
    let model = ControlModel::start().await?;
    model.respond(|_| Answer::Stall("Synthetic partial reply.".into()));
    let root = TestRoot::new()?;
    let gateway = Gateway {
        api_key: Some("synthetic-key".into()),
        stream: Some(true),
        ..model.gateway()
    };
    let config = ServerConfig::new().real_clock().gateway(gateway);
    let mut app = root.start(config).await?;
    let agent = new_id();
    let turn = new_id();
    app.post(
        "/api/agents",
        json!({"runtime": "api", "id": agent, "name": "Stop check", "model": "synthetic",
            "instructions": "Test"}),
    )
    .await?;
    let path = format!("/api/agents/{agent}/turns");
    app.post(&path, json!({"id": turn, "text": "Hold reply"}))
        .await?;
    let running = app
        .wait_row(&path, &turn, |row| {
            row["partial"]["text"]
                .as_str()
                .is_some_and(|text| !text.is_empty())
        })
        .await?;
    ensure!(
        timestamp(&running["activity"]["started_at"]),
        "started_at: {running}"
    );
    ensure!(
        running["activity"]["stopped_at"].is_null(),
        "stopped early: {running}"
    );
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    app.post(&format!("{path}/{turn}/cancel"), json!({"id": new_id()}))
        .await?;
    let stopped = app.row(&path, &turn).await?.ok_or("stopped turn")?;
    ensure!(
        stopped["activity"]["started_at"] == running["activity"]["started_at"],
        "start changed: {stopped}"
    );
    let millis = iso_millis;
    ensure!(
        millis(&stopped["activity"]["stopped_at"])? - millis(&stopped["activity"]["started_at"])?
            >= 1100,
        "stop time: {}",
        stopped["activity"]
    );
    app.post(&format!("{path}/{turn}/cancel"), json!({"id": new_id()}))
        .await?;
    app.restart().await?;
    let reloaded = app.row(&path, &turn).await?.ok_or("reloaded turn")?;
    ensure!(
        reloaded["activity"]["started_at"] == stopped["activity"]["started_at"]
            && reloaded["activity"]["stopped_at"] == stopped["activity"]["stopped_at"],
        "restart changed stop times: {} vs {}",
        reloaded["activity"],
        stopped["activity"]
    );
    Ok(())
}
