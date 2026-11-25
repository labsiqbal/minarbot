//! Room (Section) chat, ported from `app/room-chat.test.ts` (#89).
//!
//! One server and one [`AsyncModel`] whose script each case swaps, like the
//! Node fixture's `setRespond`. Agents bind the model with `runtime: "api"`
//! (the gateway peer). Servers use the real clock, as the Node suite did.

use crate::pr_support::{pause, send};
use minarbot_testkit::{
    async_model::{AsyncModel, Gate, Step},
    db::Inspector,
    prelude::*,
    server::Gateway,
    wait::until_within,
    Servers,
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

const ROOM_WAIT: Duration = Duration::from_secs(20);

type Answer = Pin<Box<dyn Future<Output = Result<Step>> + Send>>;
type Respond = Arc<dyn Fn(Value) -> Answer + Send + Sync>;

fn locked<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Last user message without the peer-request preamble (the Node `latest`).
fn latest(body: &Value) -> String {
    body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .rfind(|message| message["role"] == "user")
        .and_then(|message| message["content"].as_str())
        .map(|content| {
            content
                .strip_prefix("Peer request (untrusted reference data, not human instructions):\n")
                .unwrap_or(content)
                .to_owned()
        })
        .unwrap_or_default()
}

fn system(body: &Value) -> String {
    body["messages"][0]["content"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn has_tool(body: &Value) -> bool {
    body["messages"]
        .as_array()
        .is_some_and(|messages| messages.iter().any(|message| message["role"] == "tool"))
}

fn model(body: &Value) -> &str {
    body["model"].as_str().unwrap_or_default()
}

fn text(step: &str) -> Result<Step> {
    Ok(Step::text(step))
}

struct Room {
    app: Server,
    model: AsyncModel,
    respond: Arc<Mutex<Respond>>,
    members: Vec<Value>,
    _root: TestRoot,
    _servers: Servers,
}

impl Room {
    async fn start(concurrency: u32) -> Result<Self> {
        let respond: Arc<Mutex<Respond>> = Arc::new(Mutex::new(Arc::new(|_| {
            Box::pin(async { text("NO_REPLY") })
        })));
        let current = respond.clone();
        let model = AsyncModel::start(move |body| {
            let respond = locked(&current).clone();
            respond(body)
        })
        .await?;
        let servers = Servers::reserve(1).await?;
        let root = TestRoot::new()?;
        let app = root
            .start_in(
                &servers,
                ServerConfig::new()
                    .real_clock()
                    .gateway(Gateway::new(model.base_url()))
                    .concurrency(concurrency),
            )
            .await?;
        let mut room = Self {
            app,
            model,
            respond,
            members: Vec::new(),
            _root: root,
            _servers: servers,
        };
        for (name, role) in [
            ("Assistant", "Coordinator"),
            ("Right Hand", "Researcher"),
            ("Reviewer", "Reviewer"),
            ("Four", "Writer"),
            ("Five", "Designer"),
            ("Six", "Tester"),
        ] {
            let member = room
                .post(
                    "/api/agents",
                    json!({"runtime": "api", "id": new_id(), "name": name, "role": role,
                        "model": name, "instructions": format!("Work as {role}.")}),
                    201,
                )
                .await?;
            room.members.push(member);
        }
        Ok(room)
    }

    fn set_respond<F, Fut>(&self, respond: F)
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Step>> + Send + 'static,
    {
        *locked(&self.respond) = Arc::new(move |body| Box::pin(respond(body)));
    }

    fn calls(&self) -> Vec<Value> {
        self.model.requests()
    }

    fn member(&self, index: usize) -> String {
        self.members[index]["id"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    async fn post(&self, path: &str, body: Value, status: u16) -> Result<Value> {
        let (got, value) = send(&self.app, path, Some(&body), None, true).await?;
        ensure!(
            got == status,
            "{path}: expected {status}, got {got}: {value}"
        );
        Ok(value)
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let (got, value) = send(&self.app, path, None, None, true).await?;
        ensure!(got == 200, "{path}: expected 200, got {got}: {value}");
        Ok(value)
    }

    async fn rows(&self, path: &str) -> Result<Vec<Value>> {
        Ok(self
            .get(path)
            .await?
            .as_array()
            .cloned()
            .unwrap_or_default())
    }

    /// Create a room with the first `count` members and `lead`.
    async fn room(&self, count: usize, lead: Option<usize>) -> Result<String> {
        let members: Vec<String> = (0..count).map(|index| self.member(index)).collect();
        let channel = self
            .post(
                "/api/channels",
                json!({"id": new_id(), "name": "Team", "instructions": "Collaborate.",
                    "members": members, "lead": lead.map(|index| self.member(index))}),
                201,
            )
            .await?;
        Ok(format!(
            "/api/channels/{}/turns",
            channel["id"].as_str().unwrap_or_default()
        ))
    }

    /// Poll `path` until the row `id` exists and `done(rows)` holds.
    async fn wait_for(
        &self,
        path: &str,
        id: &str,
        done: impl Fn(&[Value]) -> bool,
    ) -> Result<Vec<Value>> {
        until_within(format!("room {path} around {id}"), ROOM_WAIT, || async {
            let rows = self.rows(path).await?;
            Ok((rows.iter().any(|row| row["id"] == id) && done(&rows)).then_some(rows))
        })
        .await
    }

    /// Default wait: every row of the root's discussion settled.
    async fn wait(&self, path: &str, id: &str) -> Result<Vec<Value>> {
        let root = id.to_owned();
        self.wait_for(path, id, move |rows| {
            rows.iter()
                .filter(|row| row["room"]["root"] == root.as_str())
                .all(|row| row["status"] != "running" && row["status"] != "queued")
        })
        .await
    }

    async fn restart(&mut self) -> Result<()> {
        self.app.restart().await
    }

    async fn stop(&mut self) -> Result<()> {
        self.app.shutdown().await?;
        Ok(())
    }

    async fn start_again(&mut self) -> Result<()> {
        let config = self.app.config().clone();
        self.app.start_stopped(config).await
    }

    fn db(&self) -> Result<Inspector> {
        Inspector::open(self.app.database())
    }
}

fn compact(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .map(|row| {
            format!(
                "{} {} {} silent={} msgs={}",
                row["agent_name"],
                row["status"],
                row["reply"],
                row["room"]["silent"],
                texts(&row["room"]["messages"]).join("|")
            )
        })
        .collect()
}

fn find<'a>(rows: &'a [Value], id: &str) -> Result<&'a Value> {
    rows.iter()
        .find(|row| row["id"] == id)
        .ok_or_else(|| format!("row {id} missing").into())
}

fn texts(messages: &Value) -> Vec<String> {
    messages
        .as_array()
        .into_iter()
        .flatten()
        .map(|message| message["text"].as_str().unwrap_or_default().to_owned())
        .collect()
}

fn tool(name: &str, args: Value) -> Result<Step> {
    Ok(Step::tool(name, args))
}

fn stop_path(path: &str, root: &str) -> String {
    path.replace("/turns", &format!("/discussions/{root}/stop"))
}

#[tokio::test]
async fn section_lane_waits_for_committed_speaker_reply_and_admits_a_labelled_shared_delta(
) -> Result<()> {
    let f = Room::start(4).await?;
    let held = Gate::new();
    let gate = held.clone();
    f.set_respond(move |body| {
        let gate = gate.clone();
        async move {
            if model(&body) == "Assistant" {
                gate.wait().await;
                return text("First committed finding.");
            }
            text("Building on First committed finding.")
        }
    });
    let path = f.room(3, Some(0)).await?;
    let root = new_id();
    f.post(&path, json!({"id": root, "text": "Shared brief"}), 202)
        .await?;
    let early = async {
        f.wait_for(&path, &root, |rows| {
            rows.iter().any(|row| row["status"] == "running")
        })
        .await?;
        // The held speaker's call starts after its turn is running.
        until_within("held speaker call", ROOM_WAIT, || async {
            Ok((!f.calls().is_empty()).then_some(()))
        })
        .await?;
        pause(100).await;
        ensure!(
            f.calls().len() == 1,
            "queued peers must not call provider: {}",
            f.calls().len()
        );
        Ok::<(), minarbot_testkit::Error>(())
    }
    .await;
    held.open();
    early?;
    let rows = f.wait(&path, &root).await?;
    let calls = f.calls();
    ensure!(calls.len() == 3, "calls: {}", calls.len());
    for call in &calls {
        let member = f
            .members
            .iter()
            .find(|member| member["name"] == call["model"])
            .ok_or("unknown model")?;
        ensure!(
            system(call).contains(&format!(
                "Your member ID: {}.",
                member["id"].as_str().unwrap_or_default()
            )),
            "member ID missing for {}",
            call["model"]
        );
    }
    ensure!(
        system(&calls[1]).contains("First committed finding."),
        "shared delta missing"
    );
    ensure!(
        system(&calls[1]).contains(&f.member(0)),
        "speaker label missing"
    );
    let first = rows
        .iter()
        .find(|row| row["agent"] == f.member(0).as_str())
        .ok_or("lead row missing")?;
    ensure!(
        first["reply"] == "First committed finding.",
        "lead reply: {first}"
    );
    Ok(())
}

#[tokio::test]
async fn room_serializes_broadcast_records_quiet_agents_emits_named_agent_mention_replies_and_survives_exact_retry_restart(
) -> Result<()> {
    let mut f = Room::start(4).await?;
    f.set_respond(|body| async move {
        text(match model(&body) {
            "Assistant" => "@Right Hand, check the schedule.",
            "Right Hand" => "The schedule is clear.",
            _ => "**NO_REPLY**",
        })
    });
    let path = f.room(3, Some(0)).await?;
    let id = new_id();
    let command = json!({"id": id, "text": "Plan today"});
    f.post(&path, command.clone(), 202).await?;
    let rows = f.wait(&path, &id).await?;
    let calls = f.calls();
    ensure!(calls.len() == 3, "calls: {}", calls.len());
    let mut models: Vec<&str> = calls.iter().take(3).map(model).collect();
    models.sort();
    ensure!(
        models == ["Assistant", "Reviewer", "Right Hand"],
        "models: {models:?}"
    );
    for call in &calls {
        let prompt = system(call);
        for member in f.members.iter().take(3) {
            ensure!(
                prompt.contains(member["name"].as_str().unwrap_or_default()),
                "member name missing"
            );
            ensure!(
                prompt.contains(member["role"].as_str().unwrap_or_default()),
                "member role missing"
            );
        }
        ensure!(prompt.contains("exactly NO_REPLY"), "silence rule missing");
        ensure!(
            !prompt.starts_with("Choose exactly"),
            "prompt starts with the silence rule"
        );
    }
    let root = find(&rows, &id)?;
    ensure!(
        texts(&root["room"]["messages"])
            == ["@Right Hand, check the schedule.", "The schedule is clear."],
        "messages: {}",
        root["room"]
    );
    ensure!(
        root["room"]["messages"][1]["agent"] == f.member(1).as_str(),
        "reply agent: {}",
        root["room"]
    );
    ensure!(
        root["room"]["quiet"].as_array().map(Vec::len) == Some(1),
        "quiet: {}",
        root["room"]
    );
    ensure!(
        rows.iter()
            .filter(|row| row["room"]["silent"].as_bool() != Some(false)
                && !row["room"]["silent"].is_null())
            .all(|row| row["reply"].is_null()),
        "silent rows have replies"
    );
    ensure!(
        calls
            .last()
            .is_some_and(|call| system(call).contains("\"name\":\"Assistant\"")),
        "last prompt lacks the committed speaker"
    );
    f.post(&path, command.clone(), 202).await?;
    f.restart().await?;
    f.post(&path, command.clone(), 202).await?;
    ensure!(f.calls().len() == 3, "retry replayed: {}", f.calls().len());
    ensure!(f.rows(&path).await?.len() == 3, "rows after retry");
    let mut changed = command;
    changed["text"] = json!("changed");
    f.post(&path, changed, 409).await?;
    Ok(())
}

#[tokio::test]
async fn mentions_skip_provider_calls_for_unmentioned_members_including_lead() -> Result<()> {
    let f = Room::start(4).await?;
    f.set_respond(|_| async { text("A relevant answer.") });
    let path = f.room(3, Some(0)).await?;
    let id = new_id();
    f.post(
        &path,
        json!({"id": id, "text": "@Right Hand, research this."}),
        202,
    )
    .await?;
    let rows = f.wait(&path, &id).await?;
    let models: Vec<String> = f
        .calls()
        .iter()
        .map(|call| model(call).to_owned())
        .collect();
    ensure!(models == ["Right Hand"], "models: {models:?}");
    ensure!(
        rows.iter()
            .filter(|row| row["room"]["silent"] == "not_mentioned")
            .count()
            == 2,
        "not mentioned rows"
    );
    ensure!(
        find(&rows, &id)?["room"]["messages"][0]["agent"] == f.member(1).as_str(),
        "mentioned reply agent"
    );
    Ok(())
}

#[tokio::test]
async fn optional_lead_breaks_all_silent_tie_once_mention_requests_never_fall_back_to_unmentioned_lead(
) -> Result<()> {
    let f = Room::start(4).await?;
    f.set_respond(|body| async move {
        text(if system(&body).contains("Nobody answered.") {
            "What outcome would help?"
        } else {
            "[SILENT]"
        })
    });
    let path = f.room(3, Some(0)).await?;
    let id = new_id();
    f.post(&path, json!({"id": id, "text": "Help me choose."}), 202)
        .await?;
    let rows = f
        .wait_for(&path, &id, |rows| {
            rows.len() == 4 && rows.iter().all(|row| row["status"] == "done")
        })
        .await?;
    let calls = f.calls();
    ensure!(calls.len() == 4, "calls: {}", calls.len());
    ensure!(calls.last().map(model) == Some("Assistant"), "tie breaker");
    ensure!(
        find(&rows, &id)?["room"]["messages"][0]["text"] == "What outcome would help?",
        "lead answer"
    );
    let next = new_id();
    f.post(
        &path,
        json!({"id": next, "text": "@Right Hand, decide."}),
        202,
    )
    .await?;
    f.wait(&path, &next).await?;
    ensure!(f.calls().len() == 5, "calls: {}", f.calls().len());
    Ok(())
}

async fn room_creates_visible_request_reply_before_parent_answer(name: &'static str) -> Result<()> {
    let f = Room::start(4).await?;
    let helper = f.member(1);
    f.set_respond(move |body| {
        let helper = helper.clone();
        async move {
            if model(&body) != "Assistant" {
                return text(if latest(&body) == "Check facts" {
                    "Facts checked."
                } else {
                    "NO_REPLY"
                });
            }
            if has_tool(&body) {
                return text("Ready to proceed.");
            }
            tool(
                name,
                if name == "send_message" {
                    json!({"recipients": [helper], "text": "Check facts", "expects_reply": true})
                } else {
                    json!({"agent": helper, "task": "Check facts", "wait": true})
                },
            )
        }
    });
    let path = f.room(3, Some(0)).await?;
    let id = new_id();
    f.post(
        &path,
        json!({"id": id, "text": "@Assistant, coordinate."}),
        202,
    )
    .await?;
    let rows = f.wait(&path, &id).await?;
    let root = find(&rows, &id)?;
    ensure!(
        texts(&root["room"]["messages"]) == ["Check facts", "Facts checked.", "Ready to proceed."],
        "messages: {}",
        root["room"]
    );
    let agents: Vec<String> = root["room"]["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|message| message["agent"].as_str().unwrap_or_default().to_owned())
        .collect();
    ensure!(
        agents == [f.member(0), f.member(1), f.member(0)],
        "message agents: {agents:?}"
    );
    ensure!(
        root["agentMessages"][0]["thread"] == "",
        "agent message thread: {root}"
    );
    ensure!(
        root["agentMessages"][0]["reply"] == "Facts checked.",
        "agent message reply: {root}"
    );
    Ok(())
}

#[tokio::test]
async fn room_send_message_creates_visible_request_reply_before_parent_answer() -> Result<()> {
    room_creates_visible_request_reply_before_parent_answer("send_message").await
}

#[tokio::test]
async fn room_delegate_task_creates_visible_request_reply_before_parent_answer() -> Result<()> {
    room_creates_visible_request_reply_before_parent_answer("delegate_task").await
}

#[tokio::test]
async fn room_rounds_stop_at_3_and_reserve_at_most_10_visible_agent_messages() -> Result<()> {
    let f = Room::start(4).await?;
    let sequence = Arc::new(AtomicUsize::new(0));
    let counter = sequence.clone();
    f.set_respond(move |body| {
        let next = counter.fetch_add(1, Ordering::SeqCst) + 1;
        async move {
            let other = if model(&body) == "Assistant" {
                "Right Hand"
            } else {
                "Assistant"
            };
            text(&format!("{next} @{other}, continue."))
        }
    });
    let path = f.room(2, None).await?;
    let id = new_id();
    f.post(&path, json!({"id": id, "text": "@Assistant, start."}), 202)
        .await?;
    let rows = f.wait(&path, &id).await?;
    let round = rows
        .iter()
        .filter_map(|row| row["room"]["round"].as_i64())
        .max()
        .unwrap_or(0);
    ensure!(round <= 3, "rounds: {round}");
    ensure!(
        rows[0]["room"]["note"]
            .as_str()
            .unwrap_or_default()
            .contains("3 agent rounds"),
        "round note: {}",
        rows[0]["room"]
    );
    let counter = sequence.clone();
    f.set_respond(move |_| {
        let next = counter.fetch_add(1, Ordering::SeqCst) + 1;
        async move {
            text(&format!(
                "Update {next}. @Assistant @Right Hand @Reviewer @Four @Five @Six"
            ))
        }
    });
    let many = f.room(6, None).await?;
    let next = new_id();
    f.post(&many, json!({"id": next, "text": "Discuss."}), 202)
        .await?;
    let capped = f.wait(&many, &next).await?;
    ensure!(
        capped[0]["room"]["messages"].as_array().map_or(0, Vec::len) <= 10,
        "messages: {}",
        capped[0]["room"]
    );
    ensure!(
        capped[0]["room"]["note"]
            .as_str()
            .unwrap_or_default()
            .contains("10 agent message"),
        "message note: {}",
        capped[0]["room"]
    );
    Ok(())
}

#[tokio::test]
async fn per_agent_loop_guard_stops_repeated_replies_without_an_unbounded_conversation(
) -> Result<()> {
    let f = Room::start(4).await?;
    f.set_respond(|body| async move {
        let other = if model(&body) == "Assistant" {
            "Right Hand"
        } else {
            "Assistant"
        };
        text(&format!("@{other}, continue."))
    });
    let path = f.room(2, None).await?;
    let id = new_id();
    f.post(&path, json!({"id": id, "text": "Discuss."}), 202)
        .await?;
    let rows = f.wait(&path, &id).await?;
    ensure!(
        rows[0]["room"]["note"]
            .as_str()
            .unwrap_or_default()
            .contains("loop guard"),
        "note: {}",
        rows[0]["room"]
    );
    ensure!(f.calls().len() <= 6, "calls: {}", f.calls().len());
    Ok(())
}

#[tokio::test]
async fn room_limit_preserves_the_original_delegation_call_and_bounded_receipt_across_restart(
) -> Result<()> {
    let mut f = Room::start(4).await?;
    let helper = f.member(1);
    f.set_respond(move |body| {
        let helper = helper.clone();
        async move {
            if latest(&body) == "@Assistant, begin." {
                return text("@Right Hand, continue.");
            }
            if model(&body) == "Right Hand" {
                return text("@Assistant, delegate now.");
            }
            if has_tool(&body) {
                return text("Bounded safely.");
            }
            tool(
                "delegate_task",
                json!({"agent": helper, "task": "One more round", "wait": true}),
            )
        }
    });
    let path = f.room(2, None).await?;
    let id = new_id();
    let command = json!({"id": id, "text": "@Assistant, begin."});
    f.post(&path, command.clone(), 202).await?;
    let rows = f.wait(&path, &id).await?;
    ensure!(
        rows[0]["room"]["note"]
            .as_str()
            .unwrap_or_default()
            .contains("3 agent rounds"),
        "note: {}",
        rows[0]["room"]
    );
    ensure!(
        rows[0]["room"]["messages"]
            .as_array()
            .and_then(|messages| messages.last())
            .is_some_and(|message| message["text"] == "Bounded safely."),
        "last message: {}",
        rows[0]["room"]
    );
    f.stop().await?;
    let saved = f.db()?.row(
        "SELECT call,result FROM flow_tool_calls ORDER BY seq DESC LIMIT 1",
        [],
    )?;
    let call: Value = serde_json::from_str(saved["call"].as_str().unwrap_or("null"))?;
    ensure!(
        call["function"]["name"] == "delegate_task",
        "saved call: {call}"
    );
    let result: Value = serde_json::from_str(saved["result"].as_str().unwrap_or("null"))?;
    ensure!(
        result == json!({"status": "bounded"}),
        "saved result: {result}"
    );
    let count = f.calls().len();
    f.start_again().await?;
    f.post(&path, command, 202).await?;
    ensure!(f.calls().len() == count, "retry replayed a provider call");
    Ok(())
}

#[tokio::test]
async fn restart_resumes_queued_room_members_without_replaying_interrupted_provider_calls(
) -> Result<()> {
    let mut f = Room::start(1).await?;
    let held = Gate::new();
    let first = Arc::new(AtomicBool::new(true));
    let (gate, once) = (held.clone(), first.clone());
    f.set_respond(move |_| {
        let (gate, once) = (gate.clone(), once.clone());
        async move {
            if once.swap(false, Ordering::SeqCst) {
                gate.wait().await;
            }
            text("A useful reply.")
        }
    });
    let path = f.room(2, None).await?;
    let id = new_id();
    let command = json!({"id": id, "text": "Recover room"});
    f.post(&path, command.clone(), 202).await?;
    f.wait_for(&path, &id, |rows| {
        rows.iter().any(|row| row["status"] == "running")
    })
    .await?;
    // A harness turn is running before its provider call starts; the Node
    // case assumed the held call was in flight.
    until_within("held provider call", ROOM_WAIT, || async {
        Ok((f.calls().len() == 1).then_some(()))
    })
    .await?;
    f.restart().await?;
    held.open();
    let rows = f.wait(&path, &id).await?;
    ensure!(
        f.calls().len() == 2,
        "calls: {} {:?} rows {:?}",
        f.calls().len(),
        f.calls()
            .iter()
            .map(|c| (model(c).to_owned(), latest(c)))
            .collect::<Vec<_>>(),
        compact(&rows)
    );
    ensure!(
        rows.iter()
            .filter(|row| row["status"] == "interrupted")
            .count()
            == 1,
        "interrupted rows"
    );
    f.post(&path, command, 202).await?;
    ensure!(f.calls().len() == 2, "retry replayed: {}", f.calls().len());
    Ok(())
}

#[tokio::test]
async fn direct_delegation_exposes_inline_transcript_request_named_reply_machine_and_activity_without_replacing_final_answer(
) -> Result<()> {
    let f = Room::start(4).await?;
    let helper = f.member(1);
    f.set_respond(move |body| {
        let helper = helper.clone();
        async move {
            if model(&body) == "Right Hand" {
                return text("Research result.");
            }
            if has_tool(&body) {
                return text("Final answer.");
            }
            tool(
                "delegate_task",
                json!({"agent": helper, "task": "Research request.", "wait": true}),
            )
        }
    });
    let path = format!("/api/agents/{}/turns", f.member(0));
    let id = new_id();
    f.post(&path, json!({"id": id, "text": "Delegate."}), 202)
        .await?;
    let turn = until_within("direct delegation", ROOM_WAIT, || async {
        let rows = f.rows(&path).await?;
        Ok(rows.first().filter(|row| row["status"] == "done").cloned())
    })
    .await?;
    ensure!(turn["reply"] == "Final answer.", "reply: {turn}");
    let message = &turn["agentMessages"][0];
    ensure!(message["task"] == "Research request.", "task: {message}");
    ensure!(message["name"] == "Right Hand", "name: {message}");
    ensure!(message["reply"] == "Research result.", "reply: {message}");
    ensure!(
        message["activity"]["calls"]
            .as_array()
            .is_some_and(|calls| !calls.is_empty()),
        "activity: {message}"
    );
    Ok(())
}

#[tokio::test]
async fn new_project_section_serializes_with_h2_silence_and_agent_mentions_then_follows_project_membership_edits(
) -> Result<()> {
    let f = Room::start(4).await?;
    f.set_respond(|body| async move {
        if latest(&body) != "Plan section work" {
            return text("Plan checked.");
        }
        text(match model(&body) {
            "Assistant" => "@Right Hand, inspect the plan.",
            "Right Hand" if system(&body).contains("inspect the plan.") => "Plan checked.",
            _ => "NO_REPLY",
        })
    });
    let members: Vec<Value> = (0..3)
        .map(|index| json!({"agent": f.member(index), "write": false}))
        .collect();
    let project = f
        .post(
            "/api/projects",
            json!({"id": new_id(), "name": "Section team", "members": members}),
            200,
        )
        .await?;
    let project_id = project["id"].as_str().unwrap_or_default().to_owned();
    let with_section = f
        .post(
            &format!("/api/projects/{project_id}/sections"),
            json!({"id": new_id(), "revision": 1, "name": "design"}),
            201,
        )
        .await?;
    let section = with_section["sections"]
        .as_array()
        .and_then(|sections| sections.iter().find(|section| section["name"] == "design"))
        .cloned()
        .ok_or("design section missing")?;
    let path = format!(
        "/api/channels/{}/turns",
        section["id"].as_str().unwrap_or_default()
    );
    let id = new_id();
    f.post(&path, json!({"id": id, "text": "Plan section work"}), 202)
        .await?;
    let rows = f.wait(&path, &id).await?;
    let calls = f.calls();
    let mut models: Vec<&str> = calls.iter().take(3).map(model).collect();
    models.sort();
    ensure!(
        models == ["Assistant", "Reviewer", "Right Hand"],
        "models: {models:?}"
    );
    ensure!(
        calls.len() == 3 || calls.len() == 4,
        "calls: {}",
        calls.len()
    );
    let call_count = calls.len();
    let root = find(&rows, &id)?;
    ensure!(
        texts(&root["room"]["messages"]) == ["@Right Hand, inspect the plan.", "Plan checked."],
        "messages: {}",
        root["room"]
    );
    ensure!(
        root["room"]["quiet"].as_array().map(Vec::len) == Some(call_count - 2),
        "quiet: {}",
        root["room"]
    );
    let changed = f
        .post(
            &format!("/api/projects/{project_id}/edit"),
            json!({"id": new_id(), "revision": 2, "members": [
                {"agent": f.member(0), "write": false}, {"agent": f.member(3), "write": false}]}),
            200,
        )
        .await?;
    let channels = f.get("/api/channels").await?;
    let mut expected = vec![f.member(0), f.member(3)];
    expected.sort();
    for section in changed["sections"].as_array().into_iter().flatten() {
        let channel = channels
            .as_array()
            .and_then(|channels| {
                channels
                    .iter()
                    .find(|channel| channel["id"] == section["id"])
            })
            .ok_or("section channel missing")?;
        ensure!(
            channel["members"] == json!(expected),
            "section members: {channel}"
        );
    }
    let next = new_id();
    f.post(&path, json!({"id": next, "text": "@Four, inspect."}), 202)
        .await?;
    f.wait(&path, &next).await?;
    let later: Vec<String> = f.calls()[call_count..]
        .iter()
        .map(|call| model(call).to_owned())
        .collect();
    ensure!(later == ["Four"], "later models: {later:?}");
    Ok(())
}

#[tokio::test]
async fn room_stop_fences_a_completed_root_with_live_queued_invited_members_exact_lost_response_retry_survives_restart_and_new_generation(
) -> Result<()> {
    let mut f = Room::start(4).await?;
    let held = Gate::new();
    let gate = held.clone();
    f.set_respond(move |body| {
        let gate = gate.clone();
        async move {
            let last = latest(&body);
            if model(&body) == "Right Hand"
                && (last == "Old generation" || last == "@Right Hand, investigate.")
            {
                gate.wait().await;
                return text("@Reviewer, late invitation must not publish.");
            }
            if last == "Old generation" {
                return text(if model(&body) == "Assistant" {
                    "@Right Hand, investigate."
                } else {
                    "NO_REPLY"
                });
            }
            text("Fresh reply.")
        }
    });
    let path = f.room(3, Some(0)).await?;
    let other = f.room(1, None).await?;
    let root = new_id();
    let result = async {
        f.post(&path, json!({"id": root, "text": "Old generation"}), 202)
            .await?;
        let parent = root.clone();
        f.wait_for(&path, &root, move |rows| {
            rows.iter()
                .any(|row| row["id"] == parent.as_str() && row["status"] == "done")
                && rows
                    .iter()
                    .any(|row| row["parent"] == parent.as_str() && row["status"] == "running")
        })
        .await?;
        let stop = stop_path(&path, &root);
        let command = json!({"id": new_id(), "generation": root});
        // Treat the first response as lost.
        let receipt = f.post(&stop, command.clone(), 200).await?;
        ensure!(
            receipt["cancelled"]
                .as_array()
                .is_some_and(|cancelled| !cancelled.is_empty()),
            "receipt: {receipt}"
        );
        ensure!(
            f.post(&stop, command.clone(), 200).await? == receipt,
            "stop retry differs"
        );
        let mut wrong = command.clone();
        wrong["generation"] = json!(new_id());
        f.post(&stop, wrong, 409).await?;
        f.post(
            &stop_path(&other, &root),
            json!({"id": new_id(), "generation": root}),
            404,
        )
        .await?;
        let stopped = f.rows(&path).await?;
        let saved = find(&stopped, &root)?.clone();
        ensure!(saved["status"] == "done", "root: {saved}");
        ensure!(saved["room"]["stopped"] == true, "stopped: {saved}");
        ensure!(saved["room"]["active"] == false, "active: {saved}");
        ensure!(
            texts(&saved["room"]["messages"]) == ["@Right Hand, investigate."],
            "messages: {}",
            saved["room"]
        );
        let cancelled = receipt["cancelled"].as_array().cloned().unwrap_or_default();
        let child = stopped
            .iter()
            .find(|row| cancelled.contains(&row["id"]))
            .ok_or("cancelled child missing")?;
        f.post(
            &path,
            json!({"id": new_id(), "text": "Stale steer", "recipient": child["agent"],
                "dispatchMode": "steer", "targetTurn": child["id"]}),
            409,
        )
        .await?;
        let next = new_id();
        let elsewhere = new_id();
        f.post(&path, json!({"id": next, "text": "Fresh generation"}), 202)
            .await?;
        f.post(
            &other,
            json!({"id": elsewhere, "text": "Unrelated root"}),
            202,
        )
        .await?;
        held.open();
        f.wait(&path, &next).await?;
        f.wait(&other, &elsewhere).await?;
        let after = f.rows(&path).await?;
        ensure!(
            find(&after, &next)?["room"]["generation"] == next.as_str(),
            "new generation"
        );
        ensure!(
            find(&after, &root)?["room"]["messages"] == saved["room"]["messages"],
            "stopped root changed"
        );
        ensure!(
            !f.calls()
                .iter()
                .any(|call| latest(call) == "@Reviewer, late invitation must not publish."),
            "late invitation reached a provider"
        );
        f.restart().await?;
        ensure!(
            f.post(&stop, command, 200).await? == receipt,
            "stop receipt changed after restart"
        );
        ensure!(
            find(&f.rows(&path).await?, &root)?["room"]["stopped"] == true,
            "stop lost on restart"
        );
        Ok(())
    }
    .await;
    held.open();
    result
}

#[tokio::test]
async fn room_stop_cancels_fan_out_delegated_descendants_and_pending_addressed_guidance_other_roots_and_direct_tasks_continue(
) -> Result<()> {
    let f = Room::start(4).await?;
    let held = Gate::new();
    let delegated = Arc::new(AtomicBool::new(false));
    let (gate, once, helper) = (held.clone(), delegated.clone(), f.member(1));
    f.set_respond(move |body| {
        let (gate, once, helper) = (gate.clone(), once.clone(), helper.clone());
        async move {
            if latest(&body) == "@Assistant, delegate." && !once.swap(true, Ordering::SeqCst) {
                return tool(
                    "delegate_task",
                    json!({"agent": helper, "task": "Held child", "wait": true}),
                );
            }
            if latest(&body) == "Held child" {
                gate.wait().await;
                return text("Late child result");
            }
            // B7: the steer restarts the child; hold that run until Stop.
            if latest(&body) == "Pending guidance" {
                gate.wait().await;
                return text("Late guidance result");
            }
            text("Other work finished")
        }
    });
    let path = f.room(3, Some(0)).await?;
    let root = new_id();
    let result = async {
        f.post(
            &path,
            json!({"id": root, "text": "@Assistant, delegate."}),
            202,
        )
        .await?;
        let rows = f
            .wait_for(&path, &root, |rows| {
                rows.iter()
                    .any(|row| row["text"] == "Held child" && row["status"] == "running")
            })
            .await?;
        let child = rows
            .iter()
            .find(|row| row["text"] == "Held child")
            .cloned()
            .ok_or("child missing")?;
        // B7: a harness child cannot steer, so the guidance is a linked
        // restart that cancels the child at once and runs in its place.
        let guidance = new_id();
        let steer = f
            .post(
                &path,
                json!({"id": guidance, "text": "Pending guidance", "recipient": child["agent"],
                    "targetTurn": child["id"], "dispatchMode": "steer"}),
                202,
            )
            .await?;
        ensure!(
            steer["dispatch_mode"] == "restart" && steer["dispatch_target"] == child["id"],
            "steer receipt: {steer}"
        );
        let running = guidance.clone();
        let rows = f
            .wait_for(&path, &root, move |rows| {
                rows.iter()
                    .any(|row| row["id"] == running.as_str() && row["status"] == "running")
            })
            .await?;
        let restarted = rows
            .iter()
            .find(|row| row["id"] == child["id"])
            .cloned()
            .ok_or("child row missing")?;
        ensure!(
            restarted["status"] == "cancelled"
                && restarted["error"]
                    .as_str()
                    .is_some_and(|error| error.starts_with("Stopped for restart.")),
            "child not restarted: {restarted}"
        );
        let stopped = f
            .post(
                &stop_path(&path, &root),
                json!({"id": new_id(), "generation": root}),
                200,
            )
            .await?;
        let cancelled = stopped["cancelled"].as_array().cloned().unwrap_or_default();
        ensure!(
            cancelled.contains(&json!(root)),
            "root not cancelled: {stopped}"
        );
        ensure!(
            cancelled.contains(&json!(guidance)),
            "pending guidance not cancelled: {stopped}"
        );
        ensure!(cancelled.len() == 2, "cancelled: {stopped}");
        held.open();
        let direct = format!("/api/agents/{}/turns", f.member(2));
        let id = new_id();
        f.post(&direct, json!({"id": id, "text": "Unrelated direct"}), 202)
            .await?;
        let done = id.clone();
        f.wait_for(&direct, &id, move |rows| {
            rows.iter()
                .any(|row| row["id"] == done.as_str() && row["status"] == "done")
        })
        .await?;
        ensure!(
            f.rows(&path)
                .await?
                .iter()
                .filter(|row| cancelled.contains(&row["id"]) || row["id"] == child["id"])
                .all(|row| row["status"] == "cancelled" && row["reply"].is_null()),
            "cancelled rows ran"
        );
        Ok(())
    }
    .await;
    held.open();
    result
}

#[tokio::test]
async fn addressed_api_room_steer_consumes_once_at_safe_point_and_completion_winner_queues_once_cross_room_targets_fail(
) -> Result<()> {
    let f = Room::start(4).await?;
    let held = Gate::new();
    let steps = Arc::new(AtomicUsize::new(0));
    let (gate, counter) = (held.clone(), steps.clone());
    f.set_respond(move |body| {
        let (gate, counter) = (gate.clone(), counter.clone());
        async move {
            if model(&body) != "Assistant" {
                gate.wait().await;
                return text("Peer finished");
            }
            if latest(&body) == "Safe point" && counter.fetch_add(1, Ordering::SeqCst) == 0 {
                gate.wait().await;
                return tool("read_provider_settings", json!({}));
            }
            text("Addressed answer")
        }
    });
    let path = f.room(3, Some(0)).await?;
    let other = f.room(1, None).await?;
    let root = new_id();
    let result = async {
        f.post(&path, json!({"id": root, "text": "Safe point"}), 202)
            .await?;
        f.wait_for(&path, &root, |rows| {
            rows.iter().filter(|row| row["status"] == "running").count() == 1
        })
        .await?;
        let guidance = json!({"id": new_id(), "text": "One constraint", "recipient": f.member(0),
            "targetTurn": root, "dispatchMode": "steer"});
        f.post(&other, guidance.clone(), 404).await?;
        let mut wrong = guidance.clone();
        wrong["recipient"] = json!(f.member(1));
        f.post(&path, wrong, 404).await?;
        f.post(
            &path,
            json!({"id": new_id(), "text": "No explicit target", "dispatchMode": "steer"}),
            400,
        )
        .await?;
        let receipt = f.post(&path, guidance.clone(), 202).await?;
        ensure!(
            f.post(&path, guidance.clone(), 202).await? == receipt,
            "steer retry differs"
        );
        held.open();
        f.wait(&path, &root).await?;
        let guidance_id = guidance["id"].as_str().unwrap_or_default().to_owned();
        let target = guidance_id.clone();
        f.wait_for(&path, &guidance_id, move |rows| {
            rows.iter()
                .any(|row| row["id"] == target.as_str() && row["status"] == "done")
        })
        .await?;
        let consumed: Vec<Value> = f
            .calls()
            .into_iter()
            .filter(|call| {
                call["messages"].as_array().is_some_and(|messages| {
                    messages.iter().any(|message| {
                        message["role"] == "user" && message["content"] == guidance["text"]
                    })
                })
            })
            .collect();
        ensure!(
            consumed.len() == 1,
            "guidance consumed {} times",
            consumed.len()
        );
        ensure!(model(&consumed[0]) == "Assistant", "guidance consumer");
        let mut late = guidance.clone();
        late["id"] = json!(new_id());
        late["text"] = json!("Late guidance");
        f.post(&path, late.clone(), 202).await?;
        let late_id = late["id"].as_str().unwrap_or_default().to_owned();
        let target = late_id.clone();
        f.wait_for(&path, &late_id, move |rows| {
            rows.iter()
                .any(|row| row["id"] == target.as_str() && row["status"] == "done")
        })
        .await?;
        f.post(&path, late, 202).await?;
        ensure!(
            f.calls()
                .iter()
                .filter(|call| latest(call) == "Late guidance")
                .count()
                == 1,
            "late guidance replayed"
        );
        Ok(())
    }
    .await;
    held.open();
    result
}

#[tokio::test]
async fn addressed_root_restart_retains_peers_stop_before_completion_prevents_final_and_queued_mentions(
) -> Result<()> {
    let f = Room::start(4).await?;
    let held = Gate::new();
    let gate = held.clone();
    f.set_respond(move |body| {
        let gate = gate.clone();
        async move {
            if latest(&body) == "Hold all" {
                gate.wait().await;
                return text("@Reviewer, too late.");
            }
            text("Restarted branch")
        }
    });
    let path = f.room(3, Some(0)).await?;
    let root = new_id();
    let result = async {
        f.post(&path, json!({"id": root, "text": "Hold all"}), 202)
            .await?;
        f.wait_for(&path, &root, |rows| {
            rows.iter().filter(|row| row["status"] == "running").count() == 1
        })
        .await?;
        let restart = json!({"id": new_id(), "text": "Restart only Assistant",
            "recipient": f.member(0), "targetTurn": root, "dispatchMode": "restart"});
        f.post(&path, restart.clone(), 202).await?;
        let restart_id = restart["id"].as_str().unwrap_or_default().to_owned();
        let target = restart_id.clone();
        f.wait_for(&path, &restart_id, move |rows| {
            rows.iter()
                .any(|row| row["id"] == target.as_str() && row["status"] == "done")
        })
        .await?;
        let before = f.rows(&path).await?;
        ensure!(
            before
                .iter()
                .filter(|row| row["status"] == "running" || row["status"] == "queued")
                .count()
                == 2,
            "live rows before stop"
        );
        let receipt = f
            .post(
                &stop_path(&path, &root),
                json!({"id": new_id(), "generation": root}),
                200,
            )
            .await?;
        ensure!(
            receipt["cancelled"].as_array().map(Vec::len) == Some(2),
            "receipt: {receipt}"
        );
        held.open();
        ensure!(
            find(&f.rows(&path).await?, &restart_id)?["reply"] == "Restarted branch",
            "restarted branch reply"
        );
        ensure!(
            !f.calls()
                .iter()
                .any(|call| latest(call) == "@Reviewer, too late."),
            "stopped mention reached a provider"
        );
        Ok(())
    }
    .await;
    held.open();
    result
}

#[tokio::test]
async fn room_stop_cancels_queued_mention_invitations_before_provider_admission() -> Result<()> {
    let f = Room::start(1).await?;
    let held = Gate::new();
    let gate = held.clone();
    f.set_respond(move |body| {
        let gate = gate.clone();
        async move {
            if model(&body) == "Assistant" {
                return text("@Right Hand, invited work. @Reviewer");
            }
            gate.wait().await;
            text("Late peer")
        }
    });
    let path = f.room(3, Some(0)).await?;
    let root = new_id();
    let result = async {
        f.post(
            &path,
            json!({"id": root, "text": "@Assistant, discuss"}),
            202,
        )
        .await?;
        f.wait_for(&path, &root, |rows| {
            rows.iter().any(|row| {
                row["text"] == "@Right Hand, invited work. @Reviewer" && row["status"] == "queued"
            }) && rows.iter().any(|row| row["status"] == "running")
        })
        .await?;
        // The running invitation holds its provider call, as the Node
        // case assumed before stopping.
        until_within(
            "running invitation reached its provider",
            ROOM_WAIT,
            || async {
                Ok(f.calls()
                    .iter()
                    .any(|call| latest(call) == "@Right Hand, invited work. @Reviewer")
                    .then_some(()))
            },
        )
        .await?;
        let stopped = f
            .post(
                &stop_path(&path, &root),
                json!({"id": new_id(), "generation": root}),
                200,
            )
            .await?;
        ensure!(
            stopped["cancelled"].as_array().map(Vec::len) == Some(2),
            "cancelled: {stopped}"
        );
        held.open();
        f.wait(&path, &root).await?;
        ensure!(
            f.calls()
                .iter()
                .filter(|call| latest(call) == "@Right Hand, invited work. @Reviewer")
                .count()
                == 1,
            "queued invitation reached a provider: {:?}",
            f.calls()
                .iter()
                .map(|c| (model(c).to_owned(), latest(c)))
                .collect::<Vec<_>>()
        );
        Ok(())
    }
    .await;
    held.open();
    result
}

#[tokio::test]
async fn room_stop_rejects_foreign_origins_bearer_callers_payload_changes_wrong_generation_and_archived_scope(
) -> Result<()> {
    let f = Room::start(4).await?;
    let path = f.room(1, Some(0)).await?;
    let root = new_id();
    f.post(&path, json!({"id": root, "text": "Complete first"}), 202)
        .await?;
    f.wait(&path, &root).await?;
    let stop = stop_path(&path, &root);
    let command = json!({"id": new_id(), "generation": root});
    for bearer in [false, true] {
        let origin = if bearer {
            f.app.origin().to_owned()
        } else {
            "https://foreign.invalid".to_owned()
        };
        let mut request = f
            .app
            .client()?
            .post(f.app.url(&stop))
            .header("Content-Type", "application/json")
            .header("Origin", origin)
            .body(command.to_string());
        if bearer {
            request = request.header("Authorization", "Bearer untrusted");
        }
        let status = request.send().await?.status().as_u16();
        ensure!(status == 403, "bearer={bearer}: {status}");
    }
    let mut extra = command.clone();
    extra["extra"] = json!(true);
    f.post(&stop, extra, 400).await?;
    let mut wrong = command.clone();
    wrong["generation"] = json!(new_id());
    f.post(&stop, wrong, 409).await?;
    let channels = f.get("/api/channels").await?;
    let channel = channels
        .as_array()
        .and_then(|channels| {
            channels
                .iter()
                .find(|channel| path.contains(channel["id"].as_str().unwrap_or("missing")))
        })
        .cloned()
        .ok_or("channel missing")?;
    f.post(
        &format!(
            "/api/channels/{}/edit",
            channel["id"].as_str().unwrap_or_default()
        ),
        json!({"id": new_id(), "revision": channel["revision"], "name": channel["name"],
            "instructions": channel["instructions"], "members": channel["members"],
            "lead": channel["lead"], "archived": true}),
        200,
    )
    .await?;
    f.post(&stop, command, 409).await?;
    ensure!(
        find(&f.rows(&path).await?, &root)?["room"]["stopped"] == false,
        "archived room was stopped"
    );
    Ok(())
}

async fn serialized_broadcast_yields_to_parallel_children_and_resumes_once(
    name: &'static str,
) -> Result<()> {
    let mut f = Room::start(4).await?;
    let held = Gate::new();
    let (gate, helpers) = (held.clone(), vec![f.member(1), f.member(2)]);
    f.set_respond(move |body| {
        let (gate, helpers) = (gate.clone(), helpers.clone());
        async move {
            if latest(&body) == "Child work" {
                gate.wait().await;
                return text("Child committed result.");
            }
            if model(&body) == "Assistant" && !has_tool(&body) {
                return tool(
                    name,
                    if name == "send_message" {
                        json!({"recipients": helpers, "text": "Child work", "expects_reply": true})
                    } else {
                        json!({"agent": helpers[0], "task": "Child work", "wait": true})
                    },
                );
            }
            text(if model(&body) == "Assistant" {
                "Parent final once."
            } else {
                "NO_REPLY"
            })
        }
    });
    let path = f.room(3, Some(0)).await?;
    let root = new_id();
    f.post(
        &path,
        json!({"id": root, "text": "Broadcast and delegate"}),
        202,
    )
    .await?;
    let children = if name == "send_message" { 2 } else { 1 };
    let started = f
        .wait_for(&path, &root, move |rows| {
            rows.iter()
                .filter(|row| row["text"] == "Child work" && row["status"] == "running")
                .count()
                == children
        })
        .await;
    held.open();
    started?;
    let rows = f.wait(&path, &root).await?;
    let parent = find(&rows, &root)?;
    ensure!(parent["reply"] == "Parent final once.", "parent: {parent}");
    ensure!(
        f.calls()
            .iter()
            .filter(|call| model(call) == "Assistant")
            .count()
            == 2,
        "Assistant calls: {:?}",
        f.calls()
            .iter()
            .map(|c| (model(c).to_owned(), latest(c), has_tool(c)))
            .collect::<Vec<_>>()
    );
    ensure!(
        texts(&parent["room"]["messages"])
            .iter()
            .filter(|text| *text == "Parent final once.")
            .count()
            == 1,
        "parent final published twice"
    );
    f.stop().await?;
    // A paused `delegate_task {wait: true}` parks the parent before the
    // gateway's own `wait_task` (kit G13) could run, so each variant journals
    // its one call.
    let journal: Vec<String> = f
        .db()?
        .rows(
            "SELECT json_extract(call,'$.function.name') AS name FROM flow_tool_calls WHERE turn=? ORDER BY seq",
            [root.as_str()],
        )?
        .iter()
        .map(|row| row["name"].as_str().unwrap_or_default().to_owned())
        .collect();
    ensure!(journal == [name], "tool journal: {journal:?}");
    let calls = f.calls().len();
    f.start_again().await?;
    ensure!(f.calls().len() == calls, "restart replayed");
    Ok(())
}

#[tokio::test]
async fn serialized_broadcast_yields_send_message_to_parallel_children_and_resumes_once(
) -> Result<()> {
    serialized_broadcast_yields_to_parallel_children_and_resumes_once("send_message").await
}

#[tokio::test]
async fn serialized_broadcast_yields_delegate_task_to_parallel_children_and_resumes_once(
) -> Result<()> {
    serialized_broadcast_yields_to_parallel_children_and_resumes_once("delegate_task").await
}

#[tokio::test]
async fn separate_sections_and_direct_chats_progress_while_root_order_holds_within_one_section(
) -> Result<()> {
    let f = Room::start(4).await?;
    let held = Gate::new();
    let gate = held.clone();
    f.set_respond(move |body| {
        let gate = gate.clone();
        async move {
            if latest(&body) == "First root" {
                gate.wait().await;
                return text("First root done.");
            }
            text("Other work done.")
        }
    });
    let first = f.room(1, Some(0)).await?;
    let second = f
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Other section", "instructions": "Separate.",
                "members": [f.member(1)], "lead": null}),
            201,
        )
        .await?;
    let second = format!(
        "/api/channels/{}/turns",
        second["id"].as_str().unwrap_or_default()
    );
    let direct_path = format!("/api/agents/{}/turns", f.member(2));
    let (root, next, other, direct) = (new_id(), new_id(), new_id(), new_id());
    f.post(&first, json!({"id": root, "text": "First root"}), 202)
        .await?;
    f.post(&first, json!({"id": next, "text": "Second root"}), 202)
        .await?;
    f.post(&second, json!({"id": other, "text": "Other Section"}), 202)
        .await?;
    f.post(&direct_path, json!({"id": direct, "text": "Direct"}), 202)
        .await?;
    let progress = async {
        f.wait(&second, &other).await?;
        let target = direct.clone();
        f.wait_for(&direct_path, &direct, move |rows| {
            rows.iter()
                .any(|row| row["id"] == target.as_str() && row["status"] == "done")
        })
        .await?;
        ensure!(
            !f.calls().iter().any(|call| latest(call) == "Second root"),
            "second root ran before the first finished"
        );
        Ok::<(), minarbot_testkit::Error>(())
    }
    .await;
    held.open();
    progress?;
    f.wait(&first, &next).await?;
    let order: Vec<String> = f
        .calls()
        .iter()
        .filter(|call| model(call) == "Assistant")
        .map(latest)
        .collect();
    ensure!(order == ["First root", "Second root"], "order: {order:?}");
    Ok(())
}

#[tokio::test]
async fn mention_storms_deduplicate_sources_settle_pass_obligations_and_never_invite_self(
) -> Result<()> {
    let mut f = Room::start(4).await?;
    f.set_respond(|body| async move {
        if model(&body) == "Right Hand" {
            return text("NO_REPLY");
        }
        text(&format!(
            "{}{}",
            "@Assistant @Right Hand @Right Hand ".repeat(50),
            model(&body)
        ))
    });
    let path = f.room(3, None).await?;
    let root = new_id();
    f.post(
        &path,
        json!({"id": root, "text": "@Assistant @Reviewer discuss"}),
        202,
    )
    .await?;
    let rows = f.wait(&path, &root).await?;
    ensure!(
        f.calls()
            .iter()
            .filter(|call| model(call) == "Right Hand")
            .count()
            == 1,
        "Right Hand invited more than once"
    );
    ensure!(
        find(&rows, &root)?["room"]["messages"]
            .as_array()
            .map_or(0, Vec::len)
            <= 10,
        "message cap"
    );
    f.stop().await?;
    let db = f.db()?;
    let own: i64 = db.scalar(
        "SELECT count(*) FROM room_invitations i JOIN turns t ON t.id=i.source WHERE i.target=t.agent",
        [],
    )?;
    ensure!(own == 0, "self invitations: {own}");
    let open: i64 = db.scalar(
        "SELECT count(*) FROM room_invitations WHERE target=(SELECT id FROM agents WHERE name=?) AND resolved IS NULL",
        ["Right Hand"],
    )?;
    ensure!(open == 0, "unresolved invitations: {open}");
    let total: i64 = db.scalar(
        "SELECT count(*) FROM room_invitations WHERE target=(SELECT id FROM agents WHERE name=?)",
        ["Right Hand"],
    )?;
    ensure!(total == 2, "invitations: {total}");
    Ok(())
}

#[tokio::test]
async fn same_name_unicode_speakers_retain_distinct_ids_in_bounded_committed_context_user_brief_survives_truncation(
) -> Result<()> {
    let f = Room::start(4).await?;
    let mut peers = Vec::new();
    for index in 0..2 {
        peers.push(
            f.post(
                "/api/agents",
                json!({"runtime": "api", "id": new_id(), "name": "林 Smith", "role": "Researcher",
                    "model": format!("unicode-{index}"), "instructions": "Help."}),
                201,
            )
            .await?,
        );
    }
    let section = f
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "Unicode", "instructions": "Collaborate.",
                "members": [peers[0]["id"], peers[1]["id"]], "lead": peers[0]["id"]}),
            201,
        )
        .await?;
    let path = format!(
        "/api/channels/{}/turns",
        section["id"].as_str().unwrap_or_default()
    );
    let root = new_id();
    f.set_respond(|body| async move {
        if model(&body) == "unicode-0" {
            return text(&format!("{} @林 Smith", "知".repeat(20000)));
        }
        text("NO_REPLY")
    });
    f.post(
        &path,
        json!({"id": root, "text": "@林 Smith Preserve this human brief."}),
        202,
    )
    .await?;
    f.wait(&path, &root).await?;
    let calls = f.calls();
    ensure!(calls.len() == 2, "calls: {}", calls.len());
    let prompt = system(&calls[1]);
    for peer in &peers {
        ensure!(
            prompt.contains(peer["id"].as_str().unwrap_or_default()),
            "speaker id missing"
        );
    }
    ensure!(
        prompt.contains("Preserve this human brief."),
        "brief truncated away"
    );
    ensure!(prompt.contains("[Reply truncated.]"), "truncation marker");
    ensure!(!prompt.contains('\u{fffd}'), "broken UTF-8 in prompt");
    ensure!(prompt.len() < 40 * 1024, "prompt bytes: {}", prompt.len());
    Ok(())
}

#[tokio::test]
async fn failure_releases_serialized_lane_without_lead_fallback_or_provider_replay() -> Result<()> {
    let mut f = Room::start(4).await?;
    f.set_respond(|body| async move {
        if model(&body) == "Assistant" {
            return Ok(Step::RawTool(json!({"bad": true})));
        }
        text("NO_REPLY")
    });
    let path = f.room(3, Some(0)).await?;
    let root = new_id();
    f.post(&path, json!({"id": root, "text": "Fail first"}), 202)
        .await?;
    let rows = f.wait(&path, &root).await?;
    ensure!(
        find(&rows, &root)?["status"] == "failed",
        "root: {}",
        find(&rows, &root)?
    );
    ensure!(f.calls().len() == 3, "calls: {}", f.calls().len());
    f.restart().await?;
    ensure!(
        f.calls().len() == 3,
        "restart replayed: {}",
        f.calls().len()
    );
    Ok(())
}

#[tokio::test]
async fn admission_cutoff_and_unpublished_watermarks_survive_restarts() -> Result<()> {
    let mut f = Room::start(4).await?;
    let held = Gate::new();
    let gated = Arc::new(AtomicBool::new(false));
    let (gate, once) = (held.clone(), gated.clone());
    f.set_respond(move |body| {
        let (gate, once) = (gate.clone(), once.clone());
        async move {
            if model(&body) == "Assistant" {
                return text("Committed A");
            }
            if !once.swap(true, Ordering::SeqCst) {
                gate.wait().await;
            }
            text("Committed peer")
        }
    });
    let path = f.room(3, Some(0)).await?;
    let root = new_id();
    f.post(&path, json!({"id": root, "text": "Restart cutoff"}), 202)
        .await?;
    let parent = root.clone();
    f.wait_for(&path, &root, move |rows| {
        rows.iter()
            .any(|row| row["id"] == parent.as_str() && row["status"] == "done")
            && rows
                .iter()
                .any(|row| row["id"] != parent.as_str() && row["status"] == "running")
    })
    .await?;
    // A harness turn is running before its provider call starts; the Node
    // case assumed the gated peer call was in flight.
    until_within("gated peer call", ROOM_WAIT, || async {
        Ok((f.calls().len() == 2).then_some(()))
    })
    .await?;
    f.stop().await?;
    let snapshot = f.db()?.row(
        "SELECT a.* FROM room_admissions a JOIN turns t ON t.id=a.turn WHERE t.status='interrupted'",
        [],
    )?;
    let turn = snapshot["turn"].as_str().unwrap_or_default().to_owned();
    ensure!(
        snapshot["context"]
            .as_str()
            .unwrap_or_default()
            .contains("Committed A"),
        "admission context: {snapshot}"
    );
    ensure!(
        snapshot["cutoff"].as_i64().unwrap_or(0) > 0,
        "cutoff: {snapshot}"
    );
    let reply = f
        .db()?
        .row("SELECT reply FROM turns WHERE id=?", [turn.as_str()])?;
    ensure!(reply["reply"].is_null(), "interrupted reply: {reply}");
    let messages: i64 = f.db()?.scalar(
        "SELECT count(*) FROM room_messages WHERE turn=?",
        [turn.as_str()],
    )?;
    ensure!(
        messages == 0,
        "interrupted turn published {messages} messages"
    );
    f.start_again().await?;
    held.open();
    let rows = f.wait(&path, &root).await?;
    ensure!(
        f.calls().len() == 3,
        "calls: {} {:?} rows {:?}",
        f.calls().len(),
        f.calls()
            .iter()
            .map(|c| (model(c).to_owned(), latest(c)))
            .collect::<Vec<_>>(),
        compact(&rows)
    );
    ensure!(
        find(&rows, &turn)?["status"] == "interrupted",
        "interrupted turn resumed"
    );
    f.stop().await?;
    let saved = f.db()?.row(
        "SELECT * FROM room_admissions WHERE turn=?",
        [turn.as_str()],
    )?;
    ensure!(
        saved == snapshot,
        "admission changed: {saved} vs {snapshot}"
    );
    let watermark = f.db()?.rows(
        "SELECT 1 FROM room_watermarks w JOIN turns t ON t.agent=w.agent WHERE t.id=? AND w.root=?",
        [turn.as_str(), root.as_str()],
    )?;
    ensure!(watermark.is_empty(), "unpublished watermark advanced");
    Ok(())
}
