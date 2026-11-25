//! Agent skill tools (port of `app/skill-tools.test.ts`).

use minarbot_testkit::{
    model::ModelRequest,
    parked::{Answer, ParkedModel, Route},
    prelude::*,
    server::Gateway,
};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Plan {
    mode: &'static str,
    target: String,
    /// Park the first request of the next turn under `revoke`.
    revoke: bool,
}

fn tool_json(request: &ModelRequest) -> Option<Value> {
    request
        .tool_results()
        .last()
        .and_then(|result| serde_json::from_str(result["content"].as_str()?).ok())
}

fn answer(plan: &Plan, request: &ModelRequest) -> Answer {
    let reads = request.tool_results().len();
    let after = tool_json(request).map(|result| result["next_after"].clone());
    let call = plan.mode != "channel"
        && (reads == 0
            || plan.mode == "list" && after.as_ref().is_some_and(|after| !after.is_null()));
    if !call {
        return Answer::text("Done");
    }
    match plan.mode {
        "list" => {
            let arguments = match after {
                Some(after) if !after.is_null() => json!({"after": after}),
                _ => json!({}),
            };
            Answer::tool("list_skills", arguments)
        }
        "invalid" => Answer::tool(
            "read_skill",
            json!({"id": plan.target, "agent": plan.target}),
        ),
        _ => Answer::tool("read_skill", json!({"id": plan.target})),
    }
}

struct Fixture {
    app: Server,
    model: ParkedModel,
    plan: Arc<Mutex<Plan>>,
}

impl Fixture {
    fn change(&self, change: impl FnOnce(&mut Plan)) {
        if let Ok(mut plan) = self.plan.lock() {
            change(&mut plan);
        }
    }

    fn mode(&self) -> &'static str {
        self.plan.lock().map(|plan| plan.mode).unwrap_or("")
    }

    /// Run one turn; returns the parsed tool results the model saw.
    async fn run(&self, path: &str, during: Option<Value>) -> Result<Vec<Value>> {
        let first = self.model.calls();
        let id = new_id();
        self.app
            .post(path, json!({"id": id, "text": "Inspect installed skills"}))
            .await?;
        if let Some(revoke) = during {
            self.model.wait_parked(&["revoke"]).await?;
            let (agent, skill) = (revoke["agent"].clone(), revoke["skill"].clone());
            self.app
                .post(
                    &format!(
                        "/api/agents/{}/skills/{}",
                        agent.as_str().unwrap_or(""),
                        skill.as_str().unwrap_or("")
                    ),
                    revoke["body"].clone(),
                )
                .await?;
            let plan = self
                .plan
                .lock()
                .map(|plan| plan.clone())
                .unwrap_or_default();
            let parked = self
                .model
                .parked_request("revoke")
                .ok_or("revoke request")?;
            self.model.finish("revoke", answer(&plan, &parked))?;
        }
        let turn = self
            .app
            .wait_row(path, &id, |row| {
                ["done", "failed"].contains(&row["status"].as_str().unwrap_or(""))
            })
            .await?;
        ensure!(turn["status"] == "done", "turn: {}", turn["error"]);
        let requests = self.model.requests().split_off(first);
        let channel = self.mode() == "channel";
        for request in &requests {
            let offered = request.tool_names().iter().any(|name| name == "read_skill");
            if channel {
                ensure!(
                    request.body["tools"].is_array() && !offered,
                    "channel offered read_skill: {:?}",
                    request.tool_names()
                );
            } else {
                ensure!(
                    offered,
                    "read_skill not offered: {:?}",
                    request.tool_names()
                );
            }
        }
        Ok(requests
            .iter()
            .filter(|request| !request.tool_results().is_empty())
            .filter_map(tool_json)
            .collect())
    }
}

fn not_found(results: &[Value]) -> bool {
    results
        .first()
        .and_then(|result| result["error"].as_str())
        .is_some_and(|error| error.contains("not found"))
}

#[tokio::test]
async fn skill_tools_page_own_enabled_pins_reject_foreign_access_and_observe_revocation(
) -> Result<()> {
    let plan = Arc::new(Mutex::new(Plan {
        mode: "list",
        ..Plan::default()
    }));
    let routed = plan.clone();
    let model = ParkedModel::start(move |request| {
        let Ok(mut plan) = routed.lock() else {
            return Route::Now(Answer::fail("poisoned"));
        };
        if plan.revoke && request.tool_results().is_empty() {
            plan.revoke = false;
            return Route::Park("revoke".into());
        }
        Route::Now(answer(&plan, request))
    })
    .await?;
    let root = TestRoot::new()?;
    let app = root
        .start(ServerConfig::new().gateway(Gateway::new(model.base_url())))
        .await?;
    let fixture = Fixture { app, model, plan };
    let app = &fixture.app;
    let a = app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "A", "model": "fixture",
                "instructions": "Help"}),
        )
        .await?;
    let b = app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "B", "model": "fixture",
                "instructions": "Help"}),
        )
        .await?;
    let a_id = a["id"].as_str().ok_or("a id")?.to_owned();
    let b_id = b["id"].as_str().ok_or("b id")?.to_owned();
    let mut ids = Vec::new();
    for index in 0..13 {
        let skill = app
            .post(
                "/api/skills",
                json!({"id": new_id(), "revision": 0, "name": format!("Skill {index}"),
                    "description": "Description", "content": format!("Pinned {index}")}),
            )
            .await?;
        let skill_id = skill["id"].as_str().ok_or("skill id")?.to_owned();
        let owner = if index == 12 { &b_id } else { &a_id };
        app.post(
            &format!("/api/agents/{owner}/skills/{skill_id}"),
            json!({"id": new_id(), "revision": 0, "version": 1, "enabled": index != 11}),
        )
        .await?;
        ids.push(skill_id);
    }
    let path = format!("/api/agents/{a_id}/turns");

    // Listing pages through exactly the agent's enabled pins.
    let results = fixture.run(&path, None).await?;
    ensure!(results.len() == 2, "list pages: {results:?}");
    let mut listed: Vec<String> = results
        .iter()
        .flat_map(|result| result["items"].as_array().cloned().unwrap_or_default())
        .map(|item| item["id"].as_str().unwrap_or("").to_owned())
        .collect();
    let mut expected = ids[..11].to_vec();
    expected.sort();
    listed.sort();
    ensure!(listed == expected, "listed: {listed:?}");

    // Reads return the pinned version, not the newer installed one.
    fixture.change(|plan| {
        plan.mode = "read";
        plan.target.clone_from(&ids[0]);
    });
    app.post(
        &format!("/api/skills/{}", ids[0]),
        json!({"id": new_id(), "revision": 1, "name": "New", "description": "New",
            "content": "UNINSTALLED VERSION"}),
    )
    .await?;
    let results = fixture.run(&path, None).await?;
    ensure!(results[0]["content"] == "Pinned 0", "read: {results:?}");
    ensure!(results[0]["version"] == 1, "read: {results:?}");
    for target in [ids[11].clone(), ids[12].clone(), new_id()] {
        fixture.change(|plan| plan.target.clone_from(&target));
        let results = fixture.run(&path, None).await?;
        ensure!(not_found(&results), "{target}: {results:?}");
    }

    // A pin disabled during the turn is no longer readable, nor once removed.
    fixture.change(|plan| {
        plan.target.clone_from(&ids[0]);
        plan.revoke = true;
    });
    let revoke = json!({"agent": a_id, "skill": ids[0],
        "body": {"id": new_id(), "revision": 1, "version": 1, "enabled": false}});
    let results = fixture.run(&path, Some(revoke)).await?;
    ensure!(not_found(&results), "revoked: {results:?}");
    app.post(
        &format!("/api/agents/{a_id}/skills/{}", ids[0]),
        json!({"id": new_id(), "revision": 2, "version": 1, "enabled": false, "remove": true}),
    )
    .await?;
    let results = fixture.run(&path, None).await?;
    ensure!(not_found(&results), "removed: {results:?}");

    // Side threads read the agent's pins; invalid requests are rejected.
    let side = app
        .post(
            &format!("/api/agents/{a_id}/threads"),
            json!({"id": new_id(), "name": "Private side"}),
        )
        .await?;
    fixture.change(|plan| plan.target.clone_from(&ids[1]));
    let results = fixture
        .run(
            &format!(
                "/api/threads/{}/turns",
                side["id"].as_str().ok_or("side id")?
            ),
            None,
        )
        .await?;
    ensure!(results[0]["content"] == "Pinned 1", "side: {results:?}");
    fixture.change(|plan| plan.mode = "invalid");
    let results = fixture.run(&path, None).await?;
    ensure!(
        results == vec![json!({"error": "Invalid skill request.", "status": 400})],
        "invalid: {results:?}"
    );

    // Channel turns are not offered skill tools.
    fixture.change(|plan| plan.mode = "channel");
    let channel = app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Room", "instructions": "Help", "lead": a_id,
                "members": [a_id]}),
        )
        .await?;
    fixture
        .run(
            &format!(
                "/api/channels/{}/turns",
                channel["id"].as_str().ok_or("channel id")?
            ),
            None,
        )
        .await?;
    Ok(())
}
