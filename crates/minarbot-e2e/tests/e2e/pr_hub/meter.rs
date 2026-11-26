use super::fixture::{call, until, Hub, Node};
use minarbot_testkit::{
    db::Inspector,
    fakes::{self, Fake},
    model::{Reply, ScriptedModel},
    prelude::*,
    providers,
    server::Gateway,
};

async fn approve_tool(node: &Node) -> Result<()> {
    until("hub tool approval", || async {
        Ok(node.approvals()?.len() == 1)
    })
    .await?;
    let action = node
        .approvals()?
        .into_iter()
        .next()
        .ok_or("missing approval")?;
    call(
        &node.app,
        "/api/hub-link/approvals",
        Some(
            json!({"actionId": action["actionId"], "actionDigest": action["actionDigest"],
            "approved": true}),
        ),
        200,
    )
    .await?;
    Ok(())
}

async fn capped(hub: &Hub, node: &Node, max_cost: i64) -> Result<String> {
    let mut policy = hub.policy(node, json!({"mode": "owner"}), None).await?;
    policy["maxCostMicros"] = json!(max_cost);
    let grant = hub.grant(node, &policy, 100, None).await?;
    hub.post(&hub.alice, &[&grant.participation_id])
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| "missing hub turn".into())
}

fn spent(node: &Node, turn: &str) -> Result<i64> {
    Inspector::open(node.app.database())?
        .scalar("SELECT spent FROM hub_link_turns WHERE turn_id=?", [turn])
}

fn file_step() -> Reply {
    Reply::tool_call(
        "budget-tool",
        "create_text_file",
        json!({"name": "budget.txt", "text": "One local file"}),
    )
}

#[tokio::test]
async fn local_provider_spend_ceiling_refuses_next_request_before_dispatch() -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|request| {
        if request
            .messages()
            .iter()
            .any(|message| message["role"] == "tool")
        {
            Reply::text("Second paid call")
        } else {
            file_step()
        }
    });
    let hub = Hub::start(1, None, false).await?;
    let node = hub
        .node_with("Budget", &hub.alice, |config| config.model(&model))
        .await?;
    let turn = capped(&hub, &node, 100).await?;
    approve_tool(&node).await?;
    until("hub turn failed", || async {
        Ok(hub.turn_state(&turn)? == "failed")
    })
    .await?;
    ensure!(model.calls() == 1, "provider calls: {}", model.calls());
    ensure!(spent(&node, &turn)? == 100, "wrong provider spend");
    let db = Inspector::open(node.app.database())?;
    ensure!(
        db.scalar::<String>(
            "SELECT error FROM turns WHERE id=? AND status='failed'",
            [&turn]
        )? == "Local provider budget reached.",
        "wrong local failure"
    );
    let files = db.rows(
        "SELECT name, CAST(bytes AS TEXT) AS content FROM generated_files g JOIN files f ON f.id=g.file WHERE g.turn=?",
        [&turn],
    )?;
    ensure!(
        files == vec![json!({"name": "budget.txt", "content": "One local file"})],
        "unexpected files: {files:?}"
    );
    Ok(())
}

#[tokio::test]
async fn local_provider_spend_ceiling_reserves_each_streamed_request() -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.push(Reply::tool("list_skills", json!({})));
    model.push(Reply::deltas(["Budget ", "available."]));
    let hub = Hub::start(1, None, true).await?;
    let node = hub
        .node_with("BudgetStream", &hub.alice, |config| {
            config.gateway(Gateway {
                stream: Some(true),
                ..Gateway::new(model.base_url())
            })
        })
        .await?;
    let turn = capped(&hub, &node, 300).await?;
    approve_tool(&node).await?;
    let frames = hub.done(&turn, None).await?;
    ensure!(model.calls() == 2, "provider calls: {}", model.calls());
    ensure!(spent(&node, &turn)? == 200, "wrong provider spend");
    ensure!(
        model
            .requests()
            .iter()
            .all(|request| request.body["stream"] == true),
        "fixture used non-streaming requests"
    );
    ensure!(
        frames.iter().any(|frame| frame["kind"] == "turn.final"
            && frame["payload"]["text"] == "Budget available."),
        "streamed reply missing from hub frames"
    );
    Ok(())
}

#[tokio::test]
async fn hub_meter_rejects_wrong_missing_and_finished_turn_tokens() -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.push(Reply::tool("list_skills", json!({})));
    model.push(Reply::text("Done."));
    let hub = Hub::start(1, None, false).await?;
    let node = hub
        .node_with("BudgetToken", &hub.alice, |config| config.model(&model))
        .await?;
    let turn = capped(&hub, &node, 300).await?;
    until("active metered turn awaiting approval", || async {
        Ok(node.approvals()?.len() == 1)
    })
    .await?;
    let wrong = format!("/api/hub-meter/{turn}/{}/chat/completions", "0".repeat(64));
    let missing = format!("/api/hub-meter/{turn}");
    node.app.post_expect(&wrong, json!({}), 403).await?;
    node.app.post_expect(&missing, json!({}), 403).await?;
    ensure!(model.calls() == 1, "rejected request reached provider");
    approve_tool(&node).await?;
    hub.done(&turn, None).await?;
    node.app.post_expect(&wrong, json!({}), 403).await?;
    ensure!(model.calls() == 2, "finished turn request reached provider");
    Ok(())
}

#[tokio::test]
async fn non_hub_turn_uses_provider_without_meter() -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.push(Reply::text("Local reply."));
    let hub = Hub::start(1, None, false).await?;
    let node = hub
        .node_with("BudgetLocal", &hub.alice, |config| config.model(&model))
        .await?;
    let path = format!("/api/agents/{}/turns", node.local_agent());
    let turn = new_id();
    node.app
        .post_expect(&path, json!({"id": turn, "text": "Hello"}), 202)
        .await?;
    let done = node.app.wait_status(&path, &turn, "done").await?;
    ensure!(done["reply"] == "Local reply.", "local reply: {done}");
    let db = Inspector::open(node.app.database())?;
    let workspace: String = db.scalar(
        "SELECT workspace FROM harness_sessions WHERE agent=?",
        [node.local_agent()],
    )?;
    let log = fakes::gateway_log(workspace)?;
    let dispatches: Vec<_> = log
        .iter()
        .filter(|entry| entry["stage"] == "provider")
        .collect();
    ensure!(
        dispatches.len() == 1 && dispatches[0]["hubMeter"] == false,
        "expected one direct provider dispatch"
    );
    ensure!(model.calls() == 1, "provider calls: {}", model.calls());
    Ok(())
}

#[tokio::test]
async fn capped_hub_turn_refuses_unsupported_provider_before_launch() -> Result<()> {
    let model = ScriptedModel::start().await?;
    let hub = Hub::start(1, None, false).await?;
    let node = hub
        .node_with("BudgetUnsupported", &hub.alice, |config| {
            config.model(&model)
        })
        .await?;
    providers::create_instance(&node.app, "unsupported", "hermes", fakes::path(Fake::Acp)?).await?;
    node.app
        .post(
            &format!("/api/agents/{}/edit", node.local_agent()),
            json!({"id": new_id(), "revision": node.agent["revision"], "name": node.agent["name"],
            "model": node.agent["model"], "instructions": node.agent["instructions"],
            "providerInstance": "unsupported", "confirmRuntime": true}),
        )
        .await?;
    let turn = capped(&hub, &node, 100).await?;
    until("unsupported provider failed", || async {
        Ok(hub.turn_state(&turn)? == "failed")
    })
    .await?;
    let db = Inspector::open(node.app.database())?;
    let error: String = db.scalar(
        "SELECT error FROM turns WHERE id=? AND status='failed'",
        [&turn],
    )?;
    ensure!(
        error == "This provider cannot enforce the hub spend limit.",
        "{error}"
    );
    ensure!(model.calls() == 0, "unsupported provider dispatched");
    ensure!(
        db.scalar::<i64>("SELECT count(*) FROM provider_calls WHERE turn=?", [&turn])? == 0
            && spent(&node, &turn)? == 0,
        "unsupported provider reached launch or reservation"
    );
    Ok(())
}
