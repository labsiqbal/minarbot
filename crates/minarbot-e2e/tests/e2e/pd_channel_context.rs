//! Channel context in routing and execution prompts (port of
//! `app/channel-context.test.ts`, one test per loop iteration).

use minarbot_testkit::{
    model::{ModelRequest, Reply, ScriptedModel},
    prelude::*,
};
use std::sync::{Arc, Mutex};

const ROUTING: &str = "Choose exactly one channel member";

fn routing(request: &ModelRequest) -> bool {
    request.system().starts_with(ROUTING)
}

fn text(message: &Value) -> String {
    match &message["content"] {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

struct Fixture {
    app: Server,
    model: ScriptedModel,
    recipient: Arc<Mutex<String>>,
    path: String,
    assistant: String,
}

impl Fixture {
    fn set_recipient(&self, id: &str) {
        if let Ok(mut recipient) = self.recipient.lock() {
            id.clone_into(&mut recipient);
        }
    }

    fn recipient(&self) -> String {
        self.recipient
            .lock()
            .map(|recipient| recipient.clone())
            .unwrap_or_default()
    }

    async fn send(&self, lead: &str, members: &[&str], route: bool) -> Result<()> {
        let first = self.model.calls();
        let id = new_id();
        let recipient = self.recipient();
        let mut body = json!({"id": id, "text": "Describe this channel."});
        if route {
            body["automatic"] = json!(true);
        } else {
            body["recipient"] = json!(recipient);
        }
        self.app.post_expect(&self.path, body, 202).await?;
        let turn = self
            .app
            .wait_row(&self.path, &id, |row| {
                ["done", "failed", "cancelled", "interrupted"]
                    .contains(&row["status"].as_str().unwrap_or(""))
            })
            .await?;
        ensure!(turn["status"] == "done", "Channel turn completed: {turn}");
        ensure!(turn["agent"] == recipient.as_str(), "turn agent: {turn}");
        let captured = self.model.requests().split_off(first);
        ensure!(
            captured.len() == if route { 2 } else { 1 },
            "captured {} calls",
            captured.len()
        );
        for call in &captured {
            let is_routing = routing(call);
            let expected_model = if is_routing {
                if lead == "Assistant" {
                    "assistant"
                } else {
                    "right-hand"
                }
            } else if recipient == self.assistant {
                "assistant"
            } else {
                "right-hand"
            };
            ensure!(
                call.model() == expected_model,
                "model {} (routing {is_routing})",
                call.model()
            );
            let context = call
                .messages()
                .iter()
                .filter(|message| {
                    message["role"] == "system" || (is_routing && message["role"] == "user")
                })
                .map(text)
                .collect::<Vec<_>>()
                .join("\n");
            ensure!(context.contains("Owner team"), "{context}");
            ensure!(context.contains("Discuss channel work."), "{context}");
            for member in members {
                ensure!(context.contains(member), "{member} missing: {context}");
            }
            if is_routing {
                let user = call
                    .messages()
                    .iter()
                    .find(|message| message["role"] == "user")
                    .map(text)
                    .ok_or("routing user message")?;
                let data: Value = serde_json::from_str(&user)?;
                ensure!(data["lead"] == lead, "routing lead: {data}");
            } else {
                ensure!(context.contains(&format!("Lead: {lead}")), "{context}");
            }
            if !members.contains(&"Right Hand") {
                ensure!(!context.contains("Right Hand"), "{context}");
            }
        }
        Ok(())
    }
}

async fn channel_context_names_lead(automatic: bool) -> Result<()> {
    let model = ScriptedModel::start().await?;
    let recipient = Arc::new(Mutex::new(String::new()));
    let chosen = recipient.clone();
    model.respond(move |request| {
        if routing(request) {
            let recipient = chosen.lock().map(|id| id.clone()).unwrap_or_default();
            Reply::text(json!({"action": "assign", "recipient": recipient}).to_string())
        } else {
            Reply::text("Channel understood.")
        }
    });
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new().model(&model)).await?;
    let assistant = app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Assistant", "model": "assistant",
                "instructions": "Help."}),
        )
        .await?;
    let right_hand = app
        .post(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Right Hand", "model": "right-hand",
                "instructions": "Coordinate."}),
        )
        .await?;
    let assistant_id = assistant["id"].as_str().ok_or("assistant id")?.to_owned();
    let right_id = right_hand["id"].as_str().ok_or("right hand id")?.to_owned();
    let mut channel = app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Owner team", "instructions": "Discuss channel work.",
                "members": [assistant_id, right_id], "lead": right_id}),
        )
        .await?;
    let channel_id = channel["id"].as_str().ok_or("channel id")?.to_owned();
    let edit = format!("/api/channels/{channel_id}/edit");
    let fixture = Fixture {
        path: format!("/api/channels/{channel_id}/turns"),
        app,
        model,
        recipient,
        assistant: assistant_id.clone(),
    };
    let both = ["Assistant", "Right Hand"];
    fixture.set_recipient(&right_id);
    fixture.send("Right Hand", &both, automatic).await?;
    fixture.set_recipient(&assistant_id);
    fixture.send("Right Hand", &both, automatic).await?;
    let changed = |channel: &Value, changes: Value| {
        let mut body = channel.clone();
        body["id"] = json!(new_id());
        body["archived"] = json!(false);
        if let (Some(body), Some(changes)) = (body.as_object_mut(), changes.as_object()) {
            for (key, value) in changes {
                body.insert(key.clone(), value.clone());
            }
        }
        body
    };
    channel = fixture
        .app
        .post(&edit, changed(&channel, json!({"lead": assistant_id})))
        .await?;
    fixture.set_recipient(&assistant_id);
    fixture.send("Assistant", &both, automatic).await?;
    channel = fixture
        .app
        .post(&edit, changed(&channel, json!({"lead": right_id})))
        .await?;
    fixture
        .app
        .post(
            &format!("/api/agents/{right_id}/visibility"),
            json!({"id": new_id(), "revision": 0, "hidden": true}),
        )
        .await?;
    let before = fixture.model.calls();
    fixture
        .app
        .post_expect(
            &fixture.path,
            json!({"id": new_id(), "text": "Hidden lead cannot route.", "automatic": true}),
            409,
        )
        .await?;
    ensure!(fixture.model.calls() == before, "hidden lead routed");
    fixture.send("No lead", &["Assistant"], false).await?;
    fixture
        .app
        .post(
            &edit,
            changed(&channel, json!({"members": [assistant_id], "lead": null})),
        )
        .await?;
    fixture.send("No lead", &["Assistant"], false).await?;
    Ok(())
}

#[tokio::test]
async fn channel_context_names_lead_for_automatic_routing_and_execution() -> Result<()> {
    channel_context_names_lead(true).await
}

#[tokio::test]
async fn channel_context_names_lead_for_explicit_execution() -> Result<()> {
    channel_context_names_lead(false).await
}
