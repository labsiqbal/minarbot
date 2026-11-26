//! Built-in Craft Anything tools over an agent session and their approval
//! cards (port of `app/craft-anything.test.ts`; the opt-in live release
//! case is not a default runtime case).

use crate::{
    fx_support::gated,
    rg_support::{mint_session, McpClient},
};
use minarbot_testkit::{
    gated::{Answer, Gate, GatedModel},
    prelude::*,
    wait::until_within,
    Server,
};
use std::time::Duration;

const SETTINGS: &str = "/api/settings/craft-anything";

/// `waitForTurn` (20 s in Node): poll the turn until `check` holds; a
/// failed turn fails the test at once.
async fn wait_turn(
    app: &Server,
    path: &str,
    turn: &str,
    check: impl Fn(&Value) -> bool,
) -> Result<Value> {
    until_within(
        "Craft Anything turn did not reach the expected state",
        Duration::from_secs(20),
        || async {
            let row = app.row(path, turn).await?;
            if let Some(row) = &row {
                ensure!(row["status"] != "failed", "turn failed: {}", row["error"]);
            }
            Ok(row.filter(|row| check(row)))
        },
    )
    .await
}

fn pending_card(row: &Value) -> Option<Value> {
    row["harnessApprovals"]
        .as_array()?
        .iter()
        .find(|card| card["state"] == "approval")
        .cloned()
}

/// `decide`: the pending card is a Craft card without a prefix grant,
/// refuses `always`, and takes `decision`. Returns the card input.
async fn decide(app: &Server, path: &str, turn: &str, decision: &str) -> Result<Value> {
    let row = wait_turn(app, path, turn, |row| pending_card(row).is_some()).await?;
    let card = pending_card(&row).ok_or("approval card")?;
    ensure!(card["kind"] == "craft", "card kind: {card}");
    ensure!(card["prefix"].is_null(), "card prefix: {card}");
    let url = format!(
        "{path}/{turn}/harness-approvals/{}",
        card["id"].as_str().ok_or("card id")?
    );
    app.post_expect(
        &url,
        json!({"id": new_id(), "revision": card["revision"], "decision": "always"}),
        400,
    )
    .await?;
    app.post(
        &url,
        json!({"id": new_id(), "revision": card["revision"], "decision": decision}),
    )
    .await?;
    Ok(card["input"].clone())
}

#[tokio::test]
async fn craft_anything_is_built_in_and_maps_its_approvals_to_allow_once_cards() -> Result<()> {
    let model = GatedModel::start().await?;
    let finish = Gate::new();
    let held = finish.clone();
    model.respond(move |_| Answer::after(&held, Answer::text("Done.")));
    let root = TestRoot::new()?;
    let desktop = json!({"enabled": true, "dataDir": root.dir("desktop")?,
        "leaseDir": root.dir("leases")?});
    let app = root.start(gated(&model).option("desktop", desktop)).await?;
    let (agent, turn) = (new_id(), new_id());
    let path = format!("/api/agents/{agent}/turns");

    let initial = app.get(SETTINGS).await?;
    ensure!(initial["installed"] == false, "initial: {initial}");
    // The fixture install: the pinned version directory beside the database.
    let version = app
        .database()
        .parent()
        .ok_or("data directory")?
        .join("craft-anything")
        .join(initial["version"].as_str().ok_or("version")?);
    std::fs::create_dir_all(&version)?;
    std::fs::write(version.join("craft-anything-cli"), "fixture")?;
    app.post_expect(
        "/api/agents",
        json!({"runtime": "api", "id": agent, "name": "Craft fixture", "model": "fixture",
            "instructions": "Study websites."}),
        201,
    )
    .await?;
    app.post_expect(
        &path,
        json!({"id": turn, "text": "Check Craft Anything."}),
        202,
    )
    .await?;
    wait_turn(&app, &path, &turn, |row| row["status"] == "running").await?;
    let session = mint_session(&app, &agent, Some(&turn), &["orchestration"], 180_000)?;
    let client = McpClient::connect(&app, &session.token).await?;

    let names = client.list_tools().await?;
    ensure!(
        names.iter().any(|name| name == "list_craft_tools"),
        "tools: {names:?}"
    );
    ensure!(
        names.iter().any(|name| name == "call_craft_tool"),
        "tools: {names:?}"
    );
    for enabled in [true, false] {
        let granted = app
            .post(
                &format!("{SETTINGS}/grant"),
                json!({"agent": agent, "enabled": enabled}),
            )
            .await?;
        ensure!(
            granted["agents"]
                .as_array()
                .is_some_and(|agents| agents.iter().any(|id| id == agent.as_str())),
            "grant {enabled}: {granted}"
        );
    }
    ensure!(
        client.list_tools().await? == names,
        "the legacy grant changed the tools"
    );
    // Since #87 (5528da5e) a refused native tool reaches the caller as an
    // MCP tool error carrying the reason, not as HTTP 403.
    let deep = client
        .call_tool("list_craft_tools", json!({"deep": true}))
        .await?;
    ensure!(deep["isError"] == true, "deep without permission: {deep}");
    ensure!(
        crate::rg_support::tool_error(&deep)
            == json!({"error": "Deep mode requires prior owner permission for this project.",
                "status": 403}),
        "deep refusal: {deep}"
    );

    let input = json!({"file_path": "draft.txt"});
    for decision in ["deny", "allow"] {
        let pending = client.call(
            "call_craft_tool",
            json!({"name": "core_action_approve",
                "arguments": {"tool_name": "Write", "input": input}}),
        );
        let (result, card) = tokio::try_join!(pending, decide(&app, &path, &turn, decision))?;
        ensure!(
            card == json!({"tool": "Write", "arguments": input}),
            "{decision} card input: {card}"
        );
        ensure!(
            result["behavior"] == decision,
            "{decision} result: {result}"
        );
        if decision == "allow" {
            ensure!(result["updatedInput"] == input, "updated input: {result}");
        }
    }
    let removed = app.post(&format!("{SETTINGS}/remove"), json!({})).await?;
    ensure!(removed["installed"] == false, "removed: {removed}");
    ensure!(
        client.list_tools().await? == names,
        "removing the release changed the tools"
    );
    finish.open();
    Ok(())
}
