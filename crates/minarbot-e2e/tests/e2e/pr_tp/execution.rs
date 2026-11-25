//! Home execution loop: provider answers, setup tools, uncertainty and
//! completion recovery through signed canonical HTTP.

use super::{
    fixture::{digest, direct, fresh_revisions, locked, task_body, until, Fault, Tp, DEVICE_RECONNECT, SETTLE},
    routines::{raw_as, text},
};
use minarbot_testkit::{
    device_host::{crash_task, enroll, export_recovery, is_code, provider, run_task_process, DeviceHost, HostOptions},
    http::Method,
    prelude::*,
    protocol::{canonical_json, sha256_hex},
};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

pub struct LoopTask {
    pub task: String,
    pub agent: String,
    pub project: String,
    pub binding: Option<String>,
}

/// Binding to create for a loop task (`loopTask`'s `existingBinding`).
pub enum Binding {
    New,
    None,
    Existing(String),
}

/// `loopTask`: a project, an agent on `runtime`'s home and one admitted
/// direct task (or a setup agent whose task is the agent id).
pub async fn loop_task(tp: &Tp, device: &str, setup: bool, binding: Binding) -> Result<LoopTask> {
    let home = tp
        .data("/api/devices")
        .await?
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == device).cloned())
        .ok_or("loop home missing")?;
    let (project, agent) = (new_id(), new_id());
    tp.project(&project, "Execution loop").await?;
    let binding = match binding {
        Binding::New => {
            let binding = new_id();
            let seed = tp.write()?;
            let account: String = seed.scalar("SELECT account FROM devices WHERE id=?", [device])?;
            seed.execute(
                "INSERT INTO provider_bindings VALUES (?,?,?,'openai-compatible',?,'Synthetic','ready',1)",
                [binding.as_str(), device, &account, &binding],
            )?;
            Some(binding)
        }
        Binding::None => None,
        Binding::Existing(binding) => Some(binding),
    };
    let body = json!({"requestId": agent, "id": agent, "name": "Loop agent", "model": "fixture",
        "backupModel": "", "instructions": "Answer carefully", "threadLimit": 2,
        "projectId": project, "homeDeviceId": home["id"], "expectedProjectRevision": 1,
        "expectedHomeDeviceRevision": home["revision"],
        "providerBinding": binding.as_ref().map(|binding| json!({"bindingId": binding, "expectedRevision": 1})),
        "setup": setup});
    let (status, created) = tp.send("/api/agents", Some(body)).await?;
    ensure!(status == 201, "loop agent: {status} {created}");
    let task = if setup { agent.clone() } else { new_id() };
    if !setup {
        let payload = direct("Answer once");
        let revisions = fresh_revisions();
        let home_id = text(&home["id"]);
        let (status, admitted) = tp
            .send(
                "/api/tasks",
                Some(json!({"requestId": task, "projectId": project, "agentId": agent,
                    "homeDeviceId": home_id, "payload": payload, "expectedRevisions": revisions,
                    "payloadDigest": digest(&json!(["minarbot-task-payload-v1", task, project, agent,
                        home_id, payload, revisions]))})),
            )
            .await?;
        ensure!(status == 202, "loop task: {status} {admitted}");
    }
    Ok(LoopTask { task, agent, project, binding })
}

type Provided = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send>>;

/// Wrap a provider so a provider failure fails the run with its own error,
/// as `withDeviceProvider` rethrew the provider's exception.
fn guard<F, Fut>(provide: F) -> (impl Fn(Value) -> Provided + Send + Sync + 'static, Arc<Mutex<Option<String>>>)
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value>> + Send + 'static,
{
    let failure: Arc<Mutex<Option<String>>> = Arc::default();
    let seen = failure.clone();
    let wrapped = move |context: Value| -> Provided {
        let (seen, next) = (seen.clone(), provide(context));
        Box::pin(async move {
            let result = next.await;
            if let Err(error) = &result {
                locked(&seen).get_or_insert_with(|| error.to_string());
            }
            result
        })
    };
    (wrapped, failure)
}

fn guarded(result: Result<Value>, failure: &Mutex<Option<String>>) -> Result<Value> {
    if let Some(failure) = locked(failure).clone() {
        return Err(failure.into());
    }
    result
}

/// Run a task on the test-control host with a guarded provider.
pub async fn run_on<F, Fut>(host: &DeviceHost, task: &str, provide: F) -> Result<Value>
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value>> + Send + 'static,
{
    let (provide, failure) = guard(provide);
    let result = host.run_task(task, provide).await;
    guarded(result, &failure)
}

/// Run an action on the test-control host with a guarded effect.
pub async fn act_on<F, Fut>(host: &DeviceHost, input: &Value, effect: F) -> Result<Value>
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value>> + Send + 'static,
{
    let (effect, failure) = guard(effect);
    let result = host.action(input, effect).await;
    guarded(result, &failure)
}

/// `rustTaskFixture`: an enrolled Rust device whose tasks run as their own
/// `run-task` processes.
pub struct RustTask {
    pub options: HostOptions,
    pub device: String,
}

impl RustTask {
    pub async fn start(tp: &Tp, name: &str, server: &str) -> Result<Self> {
        let options = HostOptions::new(&tp.dir().join(name), server)?;
        enroll(&options, &tp.jwt("user_owner")?, "new").await?;
        let device = options.device_id()?;
        Ok(Self { options, device })
    }

    pub async fn run_task<F, Fut>(&self, task: &str, provide: F) -> Result<Value>
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Value>> + Send + 'static,
    {
        let (provide, failure) = guard(provide);
        let relay = provider(provide).await?;
        let result = run_task_process(&self.options, task, &relay.origin()).await;
        drop(relay);
        guarded(result, &failure)
    }

    pub async fn crash_task<F, Fut>(&self, task: &str, provide: F, checkpoint: &str) -> Result<()>
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Value>> + Send + 'static,
    {
        let (provide, failure) = guard(provide);
        let relay = provider(provide).await?;
        let result = crash_task(&self.options, task, &relay.origin(), checkpoint).await;
        drop(relay);
        guarded(result.map(|()| Value::Null), &failure).map(|_| ())
    }
}

fn contents(detail: &Value) -> Vec<Value> {
    detail["results"].as_array().into_iter().flatten().map(|row| row["content"].clone()).collect()
}

fn failed(label: &str) -> Result<Value> {
    Err(label.to_owned().into())
}

#[tokio::test]
async fn home_execution_loop_publishes_one_provider_answer_through_signed_canonical_http() -> Result<()> {
    let tp = Tp::start().await?;
    let result = async {
        let runtime = RustTask::start(&tp, "loop.sqlite", &tp.origin()).await?;
        let task = loop_task(&tp, &runtime.device, false, Binding::New).await?.task;
        let calls = Arc::new(AtomicUsize::new(0));
        let provide = |calls: Arc<AtomicUsize>| {
            move |context: Value| {
                calls.fetch_add(1, Ordering::SeqCst);
                let digest = sha256_hex(canonical_json(&context["snapshot"]));
                let forbidden = context["snapshot"]["tools"].as_array().into_iter().flatten().any(|tool| {
                    ["read_attachment", "create_text_file", "publish_site", "delegate_task"]
                        .contains(&tool["function"]["name"].as_str().unwrap_or_default())
                });
                async move {
                    if context["snapshotRef"]["digest"] != digest.as_str() {
                        return failed("snapshot digest");
                    }
                    if forbidden {
                        return failed("forbidden tool offered");
                    }
                    Ok(json!({"text": "Canonical loop answer"}))
                }
            }
        };
        ensure!(runtime.run_task(&task, provide(calls.clone())).await?["state"] == "completed", "first run");
        ensure!(runtime.run_task(&task, provide(calls.clone())).await?["state"] == "completed", "second run");
        let detail = tp.task_detail(&task).await?;
        ensure!(detail["state"] == "completed", "state");
        ensure!(contents(&detail) == vec![json!({"text": "Canonical loop answer"})], "results: {detail}");
        ensure!(calls.load(Ordering::SeqCst) == 1, "provider calls");
        Ok(())
    }
    .await;
    tp.close().await?;
    result
}

#[tokio::test]
async fn home_execution_loop_saves_setup_tools_pauses_for_answers_and_continues_without_repeating_saved_provider_output(
) -> Result<()> {
    let tp = Tp::start().await?;
    let result = async {
        let runtime = RustTask::start(&tp, "setup-loop.sqlite", &tp.origin()).await?;
        let task = loop_task(&tp, &runtime.device, true, Binding::New).await?.task;
        let calls = Arc::new(AtomicUsize::new(0));
        let provide = |calls: Arc<AtomicUsize>| {
            move |context: Value| {
                let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
                let snapshot = context["snapshot"].clone();
                async move {
                    if call == 1 {
                        return Ok(json!({"toolCall": {"id": "question", "function": {"name": "ask_user",
                            "arguments": json!({"question": "Research or writing?",
                                "choices": ["Research", "Writing"]}).to_string()}}}));
                    }
                    if !snapshot["conversation"].to_string().contains("Research") {
                        return failed("answer in conversation");
                    }
                    if call == 2 {
                        return Ok(json!({"toolCall": {"id": "profile", "function": {"name": "edit_profile",
                            "arguments": json!({"revision": 1, "name": "Researcher", "role": "Research agent",
                                "instructions": "Research carefully"}).to_string()}}}));
                    }
                    if snapshot["profile"]["name"] != "Loop agent" {
                        return failed("profile name");
                    }
                    if !snapshot["toolJournal"].to_string().contains("Researcher") {
                        return failed("saved profile in tool journal");
                    }
                    Ok(json!({"text": "Research profile saved"}))
                }
            }
        };
        ensure!(runtime.run_task(&task, provide(calls.clone())).await?["state"] == "paused", "paused");
        ensure!(calls.load(Ordering::SeqCst) == 1, "calls before answer");
        let paused = tp.task_detail(&task).await?;
        let (status, _) = tp
            .send(
                &format!("/api/tasks/{task}/questions/{}/answer", text(&paused["questions"][0]["id"])),
                Some(json!({"requestId": new_id(), "expectedRevision": paused["revision"], "text": "Research"})),
            )
            .await?;
        ensure!(status == 200, "answer: {status}");
        ensure!(runtime.run_task(&task, provide(calls.clone())).await?["state"] == "completed", "completed");
        ensure!(calls.load(Ordering::SeqCst) == 3, "calls after answer");
        let detail = tp.task_detail(&task).await?;
        ensure!(detail["results"].as_array().map(Vec::len) == Some(5), "results: {detail}");
        ensure!(
            detail["results"][3]["content"]
                == json!({"name": "Researcher", "role": "Research agent", "instructions": "Research carefully", "revision": 2}),
            "profile result: {}",
            detail["results"][3]
        );
        Ok(())
    }
    .await;
    tp.close().await?;
    result
}

#[tokio::test]
async fn home_execution_loop_rejects_malformed_or_unadvertised_provider_outputs_and_never_retries_uncertain_effects(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let runtime = RustTask::start(&tp, "invalid-loop.sqlite", &tp.origin()).await?;
        let call = json!({"id": "bad", "function": {"name": "not_advertised", "arguments": "{}"}});
        let outputs = [
            json!({"text": ""}),
            json!({"text": "Mixed", "toolCall": call}),
            json!({"toolCall": call}),
            json!({"toolCall": {"id": "bad", "function": {"name": "ask_user", "arguments": "not json"}}}),
            json!({"unexpected": "bad"}),
        ];
        for (index, output) in outputs.into_iter().enumerate() {
            if index != 0 && index % 2 == 0 {
                until("home not ready", DEVICE_RECONNECT, || async {
                    Ok(tp.data("/api/devices").await?[0]["status"] != "ready")
                })
                .await?;
                tp.restart().await?;
            }
            let task = loop_task(&tp, &runtime.device, false, Binding::New).await?.task;
            let calls = Arc::new(AtomicUsize::new(0));
            let provide = |calls: Arc<AtomicUsize>, output: Value| {
                move |_| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let output = output.clone();
                    async move { Ok(output) }
                }
            };
            let rejected = runtime.run_task(&task, provide(calls.clone(), output.clone())).await;
            ensure!(
                rejected.as_ref().is_err_and(|error| is_code(error, "provider_result_invalid")),
                "output {index}: {rejected:?}"
            );
            ensure!(
                runtime.run_task(&task, provide(calls.clone(), output.clone())).await?["state"] == "uncertain",
                "output {index} retry"
            );
            ensure!(calls.load(Ordering::SeqCst) == 1, "output {index} calls");
            let detail = tp.task_detail(&task).await?;
            ensure!(detail["state"] == "uncertain", "output {index} state");
            ensure!(detail["results"] == json!([]), "output {index} results");
        }
        Ok(())
    }
    .await;
    tp.close().await?;
    result
}

fn journal_intent(tp: &Tp, name: &str, task: &str) -> Result<Value> {
    let db = minarbot_testkit::db::Inspector::open(tp.dir().join(name))?;
    let intent: String = db.scalar("SELECT intent_json FROM device_journal WHERE task_id=?", [task])?;
    Ok(serde_json::from_str(&intent)?)
}

#[tokio::test]
async fn home_execution_loop_resumes_saved_provider_output_after_completion_reply_loss_without_another_provider_call(
) -> Result<()> {
    let tp = Tp::start().await?;
    let result = async {
        let name = "loop-restart.sqlite";
        let runtime = RustTask::start(&tp, name, &tp.origin()).await?;
        let task = loop_task(&tp, &runtime.device, false, Binding::New).await?.task;
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        runtime
            .crash_task(
                &task,
                move |_| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    async { Ok(json!({"text": "Restart-safe answer"})) }
                },
                "completion-reply",
            )
            .await?;
        let intent = journal_intent(&tp, name, &task)?;
        ensure!(intent["completeRequestId"].is_string(), "complete request id: {intent}");
        ensure!(intent.get("completionReceipt").is_none(), "receipt before reply");
        ensure!(tp.task_detail(&task).await?["state"] == "completed", "server completed");
        let counter = calls.clone();
        let rerun = runtime
            .run_task(&task, move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                async { Ok(json!({"text": "WRONG"})) }
            })
            .await?;
        ensure!(rerun["state"] == "completed", "rerun: {rerun}");
        ensure!(calls.load(Ordering::SeqCst) == 1, "provider calls");
        ensure!(
            contents(&tp.task_detail(&task).await?) == vec![json!({"text": "Restart-safe answer"})],
            "results"
        );
        let intent = journal_intent(&tp, name, &task)?;
        ensure!(intent["completionReceipt"]["requestId"] == intent["completeRequestId"], "receipt: {intent}");
        Ok(())
    }
    .await;
    tp.close().await?;
    result
}

#[tokio::test]
async fn home_execution_loop_survives_sigkill_across_provider_and_tool_results_without_replaying_effects(
) -> Result<()> {
    let mut tp = Tp::start().await?;
    let result = async {
        let calls: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
        let counter = calls.clone();
        let relay = provider(move |context| {
            let snapshot = &context["snapshot"];
            *locked(&counter).entry(text(&snapshot["taskId"])).or_default() += 1;
            let saved = snapshot["toolJournal"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|row| row["result"]["name"] == "Saved by loop");
            async move {
                Ok(if saved {
                    json!({"text": "Saved profile once"})
                } else {
                    json!({"toolCall": {"id": "profile", "function": {"name": "edit_profile",
                        "arguments": json!({"revision": 1, "name": "Saved by loop", "role": "Personal agent",
                            "instructions": "Retain saved changes"}).to_string()}}})
                })
            }
        })
        .await?;
        let runtime = RustTask::start(&tp, "killed-loop.sqlite", &tp.origin()).await?;
        for checkpoint in ["provider-not-sent", "canonical-not-sent", "provider-saved", "tool-reply", "tool-report", "provider-in-flight"] {
            let task = loop_task(&tp, &runtime.device, false, Binding::New).await?.task;
            crash_task(&runtime.options, &task, &relay.origin(), checkpoint).await?;
            tp.restart().await?;
            let in_flight = checkpoint == "provider-in-flight";
            for _ in 0..2 {
                let recovered = run_task_process(&runtime.options, &task, &relay.origin()).await?;
                ensure!(
                    recovered["state"] == if in_flight { "uncertain" } else { "completed" },
                    "{checkpoint}: {recovered}"
                );
                ensure!(
                    locked(&calls).get(&task).copied() == Some(if in_flight { 1 } else { 2 }),
                    "{checkpoint} calls: {:?}",
                    locked(&calls).get(&task)
                );
            }
            let detail = tp.task_detail(&task).await?;
            ensure!(
                detail["results"].as_array().map(Vec::len) == Some(if in_flight { 0 } else { 3 }),
                "{checkpoint} results: {detail}"
            );
            if !in_flight {
                ensure!(
                    detail["results"][1]["content"]
                        == json!({"name": "Saved by loop", "role": "Personal agent",
                            "instructions": "Retain saved changes", "revision": 2}),
                    "{checkpoint} profile: {}",
                    detail["results"][1]
                );
            }
        }
        drop(relay);
        Ok(())
    }
    .await;
    tp.close().await?;
    result
}

#[tokio::test]
async fn home_execution_loop_uses_explicit_uncertainty_retry_reservation_and_never_retries_before_that_decision(
) -> Result<()> {
    let tp = Tp::start().await?;
    let result = async {
        let name = "risk-loop.sqlite";
        let runtime = RustTask::start(&tp, name, &tp.origin()).await?;
        let task = loop_task(&tp, &runtime.device, false, Binding::New).await?.task;
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = |calls: Arc<AtomicUsize>, answer: Option<&'static str>| {
            move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    match answer {
                        Some(text) => Ok(json!({"text": text})),
                        None => failed("Provider reply lost"),
                    }
                }
            }
        };
        let lost = runtime.run_task(&task, counted(calls.clone(), None)).await;
        ensure!(
            lost.as_ref().is_err_and(|error| error.to_string().contains("Provider reply lost")),
            "lost reply: {lost:?}"
        );
        ensure!(
            runtime.run_task(&task, counted(calls.clone(), Some("MUST NOT CALL"))).await?["state"] == "uncertain",
            "uncertain"
        );
        ensure!(calls.load(Ordering::SeqCst) == 1, "calls before decision");
        let recovered = minarbot_testkit::db::Inspector::open(tp.dir().join(name))?.row(
            "SELECT j.action_id,j.state,o.state AS ack FROM device_journal j JOIN device_outbox o USING(action_id) WHERE j.task_id=?",
            [&task],
        )?;
        ensure!(recovered["state"] == "uncertain", "journal state");
        ensure!(recovered["ack"] == "acknowledged", "outbox ack");
        let action = text(&recovered["action_id"]);
        let detail = tp.task_detail(&task).await?;
        let request = new_id();
        let (status, decision) = tp
            .send(
                &format!("/api/tasks/{task}/control"),
                Some(json!({"requestId": request, "expectedRevision": detail["revision"],
                    "operation": "resolve_uncertain", "decision": {"kind": "retry_with_risk",
                        "actionId": action, "acknowledgement": "effect_may_have_happened",
                        "reason": "Synthetic provider output lost; retry explicitly approved"}})),
            )
            .await?;
        ensure!(status == 200, "decision: {status} {decision}");
        ensure!(decision["data"]["replacementActionId"] == request.as_str(), "replacement");
        ensure!(
            runtime.run_task(&task, counted(calls.clone(), Some("Approved replacement answer"))).await?["state"]
                == "completed",
            "replacement run"
        );
        ensure!(calls.load(Ordering::SeqCst) == 2, "calls after decision");
        let completed = tp.task_detail(&task).await?;
        ensure!(completed["results"][0]["actionId"] == request.as_str(), "result action");
        ensure!(completed["results"][0]["content"] == json!({"text": "Approved replacement answer"}), "result");
        Ok(())
    }
    .await;
    tp.close().await?;
    result
}

#[tokio::test]
async fn home_execution_loop_consumes_resolved_late_known_output_without_repeating_its_provider_call(
) -> Result<()> {
    let tp = Tp::start().await?;
    let result = async {
        let runtime = tp.device("known-loop.sqlite")?;
        runtime.enroll(&tp.jwt("user_owner")?, "new").await?;
        let connection = runtime.connect().await?;
        until("home", SETTLE, || async { Ok(connection.phase() == "connected") }).await?;
        let task = loop_task(&tp, &runtime.device_id()?, false, Binding::New).await?.task;
        let calls = Arc::new(AtomicUsize::new(0));
        let prepared: Arc<Mutex<Value>> = Arc::new(Mutex::new(json!({})));
        let last_report: Arc<Mutex<Value>> = Arc::new(Mutex::new(json!({})));
        let (seen_prepare, seen_report) = (prepared.clone(), last_report.clone());
        tp.fault(Some(Arc::new(move |exchange| {
            if exchange.path.ends_with("/prepare") {
                let mut body = exchange.json();
                let parts: Vec<&str> = exchange.path.split('/').collect();
                body["actionId"] = json!(parts[parts.len().saturating_sub(2)]);
                *locked(&seen_prepare) = body;
            }
            if exchange.path.ends_with("/reconcile") {
                *locked(&seen_report) = exchange.json();
            }
            Fault::Pass
        })));
        let counter = calls.clone();
        let lost = run_on(&runtime, &task, move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { failed("Reply lost") }
        })
        .await;
        ensure!(lost.as_ref().is_err_and(|error| error.to_string().contains("Reply lost")), "lost: {lost:?}");
        runtime.recover().await?;
        let prepared = locked(&prepared).clone();
        let action = text(&prepared["actionId"]);
        let result_content = json!({"toolCall": {"id": "settings", "function": {"name": "read_provider_settings", "arguments": "{}"}}});
        let encoded = canonical_json(&result_content);
        let result_ref = json!({"v": 1, "taskId": task, "actionId": action, "digest": sha256_hex(&encoded),
            "size": encoded.len(), "encoding": "canonical-json-v1"});
        let evidence = new_id();
        let sequence = locked(&last_report)["journalSequence"].as_i64().unwrap_or(0) + 1;
        runtime
            .call(
                "reconcile",
                json!({"taskId": task, "body": {"requestId": evidence, "actionId": action,
                    "ownership": prepared["ownership"], "payloadDigest": prepared["payloadDigest"],
                    "state": "reported", "effectStatus": "known", "journalSequence": sequence,
                    "resultDigest": result_ref["digest"], "resultRef": result_ref,
                    "resultContent": result_content}}),
            )
            .await?;
        let detail = tp.task_detail(&task).await?;
        let (status, _) = tp
            .send(
                &format!("/api/tasks/{task}/control"),
                Some(json!({"requestId": new_id(), "expectedRevision": detail["revision"],
                    "operation": "resolve_uncertain", "decision": {"kind": "known_result",
                        "actionId": action, "evidenceRequestId": evidence, "resultRef": result_ref}})),
            )
            .await?;
        ensure!(status == 200, "known result: {status}");
        tp.fault(None);
        let counter = calls.clone();
        let ran = run_on(&runtime, &task, move |context| {
            counter.fetch_add(1, Ordering::SeqCst);
            let configured = context["snapshot"]["toolJournal"].to_string().contains("configured");
            async move {
                if !configured {
                    return failed("known tool result in journal");
                }
                Ok(json!({"text": "Known tool choice retained"}))
            }
        })
        .await?;
        ensure!(ran["state"] == "completed", "run: {ran}");
        ensure!(calls.load(Ordering::SeqCst) == 2, "provider calls");
        ensure!(tp.task_detail(&task).await?["results"].as_array().map(Vec::len) == Some(3), "results");
        let device = runtime.device_id()?;
        let home = tp
            .data("/api/devices")
            .await?
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["id"] == device.as_str()).cloned())
            .ok_or("home missing")?;
        ensure!(
            tp.send(&format!("/api/devices/{device}/revoke"), Some(json!({"requestId": new_id(), "expectedRevision": home["revision"]})))
                .await?
                .0
                == 200,
            "revoke"
        );
        connection.stop().await?;
        runtime.close().await?;
        let (status, recovery) = tp
            .send(
                &format!("/api/tasks/{task}/recovery-session"),
                Some(json!({"requestId": new_id(),
                    "expectedAuthorityGeneration": prepared["ownership"]["authorityGeneration"],
                    "sourceDeviceId": device,
                    "sourceDeviceGeneration": prepared["ownership"]["deviceGeneration"]})),
            )
            .await?;
        ensure!(status == 201, "recovery session: {status} {recovery}");
        let exported = export_recovery(
            &runtime.options,
            &json!({"taskId": task, "actionIds": [action], "requestId": new_id(),
                "recoveryGeneration": recovery["data"]["recoveryGeneration"], "withContent": true}),
        )
        .await?;
        let request = text(&exported["requestId"]);
        let session = text(&recovery["data"]["session"]);
        let (status, _, body) = raw_as(
            &tp,
            "user_owner",
            Method::POST,
            &format!("/api/tasks/{task}/recovery-results"),
            &[("X-Minarbot-Request-Id", &request), ("X-Minarbot-Recovery", &session)],
            Some(exported.to_string()),
        )
        .await?;
        ensure!(status == 200, "recovery results: {status} {body}");
        ensure!(exported["envelope"]["entries"][0]["effectStatus"] == "known", "effect status");
        ensure!(exported["envelope"]["entries"][0]["resultRef"] == result_ref, "result ref");
        ensure!(exported["resultContent"] == result_content, "result content");
        ensure!(calls.load(Ordering::SeqCst) == 2, "calls after recovery");
        Ok(())
    }
    .await;
    tp.fault(None);
    tp.close().await?;
    result
}

/// A `POST` with the protocol headers that owns everything it needs, for
/// fault hooks that run while the device proxy holds a request.
fn owned_post(tp: &Tp, path: &str) -> Result<impl Fn(Value) -> Provided + Send + Sync + 'static> {
    let client = tp.app.server.client()?;
    let (origin, token, path) = (tp.origin(), tp.jwt("user_owner")?, path.to_owned());
    Ok(move |body: Value| -> Provided {
        let (client, origin, token, path) = (client.clone(), origin.clone(), token.clone(), path.clone());
        Box::pin(async move {
            let request = text(&body["requestId"]);
            let response = client
                .post(format!("{origin}{path}"))
                .header("Authorization", format!("Bearer {token}"))
                .header("Origin", origin.clone())
                .header("Content-Type", "application/json")
                .header("X-Minarbot-Protocol", "1")
                .header("X-Minarbot-Request-Id", request)
                .body(body.to_string())
                .send()
                .await?;
            let status = response.status().as_u16();
            Ok(json!({"status": status, "body": response.json::<Value>().await.unwrap_or(Value::Null)}))
        })
    })
}

#[tokio::test]
async fn home_execution_loop_persists_provider_wait_across_reply_loss_and_preserves_concurrent_pause(
) -> Result<()> {
    let tp = Tp::start().await?;
    let result = async {
        let runtime = RustTask::start(&tp, "wait-loop.sqlite", &tp.proxy.origin()).await?;
        let missing = loop_task(&tp, &runtime.device, false, Binding::None).await?;
        let seen = tp.lose_next("/wait-provider");
        let lost = runtime.run_task(&missing.task, |_| async { failed("No provider binding") }).await;
        ensure!(lost.as_ref().is_err_and(|error| is_code(error, "device_offline")), "lost wait: {lost:?}");
        ensure!(*locked(&seen) == Some(200), "Lost reply must follow a durable provider wait");
        tp.fault(None);
        let waiting = tp.task_detail(&missing.task).await?;
        ensure!(waiting["state"] == "waiting_resource", "waiting state");
        ensure!(
            runtime.run_task(&missing.task, |_| async { failed("No provider binding") }).await?["state"] == "waiting_resource",
            "rerun waits"
        );
        ensure!(tp.task_detail(&missing.task).await?["revision"] == waiting["revision"], "revision kept");
        let paused = loop_task(&tp, &runtime.device, false, Binding::None).await?;
        let path = format!("/api/tasks/{}", paused.task);
        let control = owned_post(&tp, &format!("{path}/control"))?;
        let (database, task) = (tp.database(), paused.task.clone());
        let pause_status: Arc<Mutex<Option<u16>>> = Arc::default();
        let recorded = pause_status.clone();
        let control = Arc::new(control);
        let suffix = format!("{path}/wait-provider");
        tp.fault(Some(Arc::new(move |exchange| {
            if !exchange.path.ends_with(&suffix) {
                return Fault::Pass;
            }
            let (control, database, task, recorded) = (control.clone(), database.clone(), task.clone(), recorded.clone());
            Fault::Hold(Box::pin(async move {
                let revision = minarbot_testkit::db::Inspector::open(&database)
                    .and_then(|db| db.scalar::<i64>("SELECT revision FROM tasks WHERE id=?", [&task]))
                    .unwrap_or(0);
                let response = control(json!({"requestId": new_id(), "expectedRevision": revision, "operation": "pause"})).await;
                *locked(&recorded) = response.ok().and_then(|value| value["status"].as_u64()).map(|status| status as u16);
            }))
        })));
        let stale = runtime.run_task(&paused.task, |_| async { failed("Paused task must not dispatch") }).await;
        tp.fault(None);
        ensure!(*locked(&pause_status) == Some(200), "pause: {:?}", locked(&pause_status));
        ensure!(stale.as_ref().is_err_and(|error| is_code(error, "stale_revision")), "stale: {stale:?}");
        ensure!(tp.task_detail(&paused.task).await?["state"] == "paused", "paused");
        Ok(())
    }
    .await;
    tp.fault(None);
    tp.close().await?;
    result
}

#[tokio::test]
async fn home_execution_loop_holds_missing_bindings_and_enforces_provider_slots_before_dispatch(
) -> Result<()> {
    let tp = Tp::start().await?;
    let releases: Arc<Mutex<Vec<tokio::sync::oneshot::Sender<()>>>> = Arc::default();
    let result = async {
        let runtime = tp.device("capacity-loop.sqlite")?;
        runtime.enroll(&tp.jwt("user_owner")?, "new").await?;
        let connection = runtime.connect().await?;
        until("home", SETTLE, || async { Ok(connection.phase() == "connected") }).await?;
        let device = runtime.device_id()?;
        let hold = {
            let releases = releases.clone();
            move || {
                let releases = releases.clone();
                move |_: Value| {
                    let (release, held) = tokio::sync::oneshot::channel();
                    locked(&releases).push(release);
                    async move {
                        let _ = held.await;
                        Ok(json!({"text": "Released answer"}))
                    }
                }
            }
        };
        let held = |count: usize| {
            let releases = releases.clone();
            async move { until("held provider calls", SETTLE, || async { Ok(locked(&releases).len() == count) }).await }
        };
        let mut running = Vec::new();
        let missing = loop_task(&tp, &device, false, Binding::None).await?;
        ensure!(
            run_on(&runtime, &missing.task, |_| async { failed("Missing binding must not dispatch") }).await?["state"]
                == "waiting_resource",
            "missing binding"
        );
        ensure!(tp.task_detail(&missing.task).await?["state"] == "waiting_resource", "missing state");
        let first = loop_task(&tp, &device, false, Binding::New).await?;
        let spawn = |task: String| {
            let (runtime, provide) = (runtime.clone(), hold());
            tokio::spawn(async move { run_on(&runtime, &task, provide).await })
        };
        running.push(spawn(first.task.clone()));
        held(1).await?;
        let busy = run_on(&runtime, &first.task, hold()).await;
        ensure!(busy.as_ref().is_err_and(|error| is_code(error, "executor_busy")), "busy: {busy:?}");
        let same = new_id();
        let (status, _) = tp
            .send("/api/tasks", Some(task_body(&same, &first.project, &first.agent, &device, &direct("Same agent/project"))))
            .await?;
        ensure!(status == 202, "same task: {status}");
        ensure!(
            run_on(&runtime, &same, |_| async { failed("Same agent/project must wait") }).await?["state"] == "waiting_capacity",
            "same agent waits"
        );
        let binding = first.binding.clone().ok_or("first binding")?;
        let second = loop_task(&tp, &device, false, Binding::Existing(binding.clone())).await?;
        running.push(spawn(second.task.clone()));
        held(2).await?;
        let third = loop_task(&tp, &device, false, Binding::Existing(binding)).await?;
        ensure!(
            run_on(&runtime, &third.task, |_| async { failed("Third binding call must wait") }).await?["state"] == "waiting_capacity",
            "third binding call waits"
        );
        for count in 2..4 {
            let task = loop_task(&tp, &device, false, Binding::New).await?;
            running.push(spawn(task.task));
            held(count + 1).await?;
        }
        let fifth = loop_task(&tp, &device, false, Binding::New).await?;
        ensure!(
            run_on(&runtime, &fifth.task, |_| async { failed("Fifth host call must wait") }).await?["state"] == "waiting_capacity",
            "fifth host call waits"
        );
        // Refill the real 40-proof/2s device budget before releasing four held calls.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        for release in locked(&releases).drain(..) {
            let _ = release.send(());
        }
        for run in running {
            run.await??;
        }
        ensure!(
            run_on(&runtime, &same, |_| async { Ok(json!({"text": "Later same-agent answer"})) }).await?["state"] == "completed",
            "same agent later"
        );
        connection.stop().await?;
        Ok(())
    }
    .await;
    for release in locked(&releases).drain(..) {
        let _ = release.send(());
    }
    tp.close().await?;
    result
}
