//! Agent script CLI (`minarbot-server script ...`): grants, retries with
//! stable identity across lost replies and restarts, waits, routines,
//! workflows, approvals and answers (port of `app/cli.test.ts`).

use crate::support::{after_question, after_question_reply, work_calls};
use minarbot_testkit::{
    cli::{Run, Script},
    http::{Method, SplitRequest},
    model::{Reply, ScriptedModel},
    prelude::*,
    proxy::LossyProxy,
    Server,
};
use std::time::Duration;

/// `--timeout` for CLI waits that should settle; the Node suite used 3-5 s,
/// which a loaded shared host can exceed. Expiry itself is tested with 1 s.
const WAIT_SECONDS: &str = "30";

/// Owned CLI arguments from mixed `&str`, `String` and numbers.
macro_rules! args {
    ($($arg:expr),* $(,)?) => {
        Vec::<String>::from([$($arg.to_string()),*])
    };
}

/// Root, scripts directory and config for a CLI test.
fn setup(model: Option<&ScriptedModel>) -> Result<(TestRoot, Script, ServerConfig)> {
    let root = TestRoot::new()?;
    let directory = root.path().join("scripts");
    let mut config = ServerConfig::new().script_directory(&directory);
    if let Some(model) = model {
        config = config.model(model);
    }
    Ok((root, Script::new(directory)?, config))
}

async fn agent(app: &Server, name: &str, model: &str) -> Result<Value> {
    app.post(
        "/api/agents",
        json!({"runtime": "api", "id": new_id(), "name": name, "model": model, "instructions": "Help"}),
    )
    .await
}

fn id(value: &Value) -> Result<String> {
    Ok(value["id"].as_str().ok_or("missing id")?.to_owned())
}

async fn scripts(app: &Server, agent: &str, revision: i64, enabled: bool) -> Result<Value> {
    app.post(
        &format!("/api/agents/{agent}/scripts"),
        json!({"id": new_id(), "revision": revision, "enabled": enabled}),
    )
    .await
}

fn code(run: &Run, expected: i32, label: &str) -> Result<()> {
    ensure!(
        run.code == expected,
        "{label}: exit {} (expected {expected}): {}",
        run.code,
        run.json
    );
    Ok(())
}

#[tokio::test]
async fn script_cli_discovers_only_grants_sends_once_waits_preserves_retry_id_and_reports_attention(
) -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|request| match request.last_text().as_str() {
        _ if after_question(request) => after_question_reply(),
        "HOLD" => Reply::hold(),
        "ASK" => Reply::tool_call(
            "question",
            "ask_user",
            json!({"question": "Choose", "choices": ["Yes", "No"]}),
        ),
        _ => Reply::text("Answer"),
    });
    let (root, cli, config) = setup(Some(&model))?;
    let mut app = root.start(config).await?;
    let a = id(&agent(&app, "Alpha", "test").await?)?;
    let b = id(&agent(&app, "alpha", "test").await?)?;
    scripts(&app, &a, 0, true).await?;
    let list = cli.run(&["list"]).await?;
    code(&list, 0, "list")?;
    ensure!(
        list["agents"]
            .as_array()
            .map(|rows| rows.iter().map(|row| row["id"].clone()).collect::<Vec<_>>())
            == Some(vec![json!(a)]),
        "granted agents: {}",
        list.json
    );
    let turn = new_id();
    let args = [
        "run",
        "ALPHA",
        "Hello",
        "--id",
        &turn,
        "--wait",
        "--timeout",
        "3",
    ];
    let done = cli.run(&args).await?;
    code(&done, 0, "run")?;
    ensure!(
        done["turn"]["reply"] == "Answer",
        "run reply: {}",
        done.json
    );
    code(&cli.run(&args).await?, 0, "run replay")?;
    ensure!(
        work_calls(&model) == 1,
        "run replay called the model again ({})",
        work_calls(&model)
    );
    ensure!(
        cli.run(&["status", &a, &turn]).await?["turn"]["id"] == json!(turn),
        "status by id"
    );
    scripts(&app, &b, 0, true).await?;
    code(
        &cli.run(&["run", "Alpha", "Ambiguous"]).await?,
        2,
        "ambiguous name",
    )?;
    ensure!(work_calls(&model) == 1, "ambiguous run reached the model");
    let ask = cli
        .run(&["run", &a, "ASK", "--wait", "--timeout", WAIT_SECONDS])
        .await?;
    code(&ask, 3, "question needs attention")?;
    let ask_id = id(&ask.json)?;
    code(
        &cli.run(&["cancel", &a, &ask_id]).await?,
        0,
        "cancel question",
    )?;
    code(
        &cli.run(&["status", &a, &ask_id]).await?,
        1,
        "cancelled status",
    )?;
    let held = cli
        .run(&["run", &a, "HOLD", "--wait", "--timeout", "1"])
        .await?;
    code(&held, 4, "wait timeout")?;
    let held_id = id(&held.json)?;
    cli.wait(&["status", &a, &held_id], |run| {
        run["turn"]["status"] == "running"
    })
    .await?;
    code(
        &cli.run(&["cancel", &b, &held_id]).await?,
        1,
        "cancel from other agent",
    )?;
    ensure!(
        cli.run(&["status", &a, &held_id]).await?["turn"]["status"] == "running",
        "foreign cancel stopped work"
    );
    code(
        &cli.run(&["cancel", &a, &held_id.to_uppercase()]).await?,
        1,
        "uppercase id",
    )?;
    let cancelled = cli.run(&["cancel", &a, &held_id]).await?;
    code(&cancelled, 0, "cancel")?;
    ensure!(
        cancelled["turn"]["status"] == "cancelled",
        "cancel: {}",
        cancelled.json
    );
    code(
        &cli.run(&["cancel", &a, &turn]).await?,
        1,
        "cancel finished turn",
    )?;
    code(
        &cli.run(&["status", &a, &held_id]).await?,
        1,
        "cancelled status",
    )?;
    let (status, output) = cli.raw(&["list"]).await?;
    ensure!(status == 0, "raw list exit {status}");
    let token = cli.connection()?["token"]
        .as_str()
        .ok_or("token")?
        .to_owned();
    ensure!(
        !output.contains(&token),
        "list printed the connection token"
    );
    let parsed: Value = serde_json::from_str(&output)?;
    ensure!(
        parsed["agents"].as_array().map(Vec::len) == Some(2),
        "list after second grant: {parsed}"
    );
    let unicode = cli
        .run_chunks(
            &["run", &a, "-", "--wait", "--timeout", WAIT_SECONDS],
            &[
                (&[0x63, 0x61, 0x66, 0xc3], Duration::ZERO),
                (&[0xa9], Duration::from_millis(500)),
            ],
        )
        .await?;
    code(&unicode, 0, "stdin run")?;
    ensure!(
        unicode["turn"]["text"] == "café",
        "split UTF-8 stdin: {}",
        unicode.json
    );

    let proxy = LossyProxy::start(app.origin()).await?;
    proxy.drop_next(|request| request.method == "POST");
    cli.use_url(proxy.origin())?;
    let lost = cli.run(&["run", &a, "Lost response"]).await?;
    code(&lost, 5, "lost reply")?;
    let lost_id = id(&lost.json)?;
    let recovered = cli
        .run(&[
            "run",
            &a,
            "Lost response",
            "--id",
            &lost_id,
            "--wait",
            "--timeout",
            "3",
        ])
        .await?;
    code(&recovered, 0, "retry after lost reply")?;
    ensure!(
        recovered["id"] == json!(lost_id),
        "retry id: {}",
        recovered.json
    );
    ensure!(
        work_calls(&model) == 5,
        "model calls after lost reply: {}",
        work_calls(&model)
    );
    let pending = cli.run(&["run", &a, "HOLD"]).await?;
    code(&pending, 0, "pending run")?;
    let pending_id = id(&pending.json)?;
    // Cancel an in-flight provider request, as the Node case assumed.
    until("pending turn reaches the model", || async {
        Ok((work_calls(&model) == 6).then_some(()))
    })
    .await?;
    proxy.drop_next(|request| request.method == "POST");
    let lost_cancel = cli.run(&["cancel", &a, &pending_id]).await?;
    code(&lost_cancel, 5, "lost cancel")?;
    ensure!(lost_cancel["id"] == json!(pending_id), "lost cancel id");
    let retry = cli.run(&["cancel", &a, &pending_id]).await?;
    code(&retry, 0, "cancel retry")?;
    ensure!(
        retry["turn"]["status"] == "cancelled",
        "cancel retry: {}",
        retry.json
    );
    let revision = retry["turn"]["revision"].clone();
    ensure!(
        cli.run(&["cancel", &a, &pending_id]).await?["turn"]["revision"] == revision,
        "cancel replay changed revision"
    );
    let restart_pending = cli.run(&["run", &a, "HOLD"]).await?;
    code(&restart_pending, 0, "run before restart")?;
    let restart_id = id(&restart_pending.json)?;
    let waiting = {
        let cli = cli.clone();
        let restart_id = restart_id.clone();
        let a = a.clone();
        tokio::spawn(async move {
            cli.run(&[
                "status",
                &a,
                &restart_id,
                "--wait",
                "--timeout",
                WAIT_SECONDS,
            ])
            .await
        })
    };
    until("waiting CLI polls status", || async {
        let focus = format!("?focus={restart_id}");
        Ok(proxy
            .requests()
            .iter()
            .any(|request| request.method == "GET" && request.path.ends_with(&focus))
            .then_some(()))
    })
    .await?;
    // The held turn must be in flight at the model before the restart, so
    // the final count proves the interrupted turn is not replayed.
    until("held turn reaches the model", || async {
        Ok((work_calls(&model) == 7).then_some(()))
    })
    .await?;
    // Make the server unreachable before stopping it, so no poll can land
    // in the graceful-shutdown window (the turn is already interrupted
    // there) and the CLI sees a pure discovery gap.
    proxy.set_offline(true);
    app.shutdown().await?;
    tokio::time::sleep(Duration::from_millis(650)).await;
    if waiting.is_finished() {
        let result = waiting.await??;
        return Err(format!("discovery gap ended an established wait: {}", result.json).into());
    }
    app.start_stopped(app.config().clone()).await?;
    proxy.set_target(app.origin());
    proxy.set_offline(false);
    let interrupted = waiting.await??;
    code(&interrupted, 1, "wait across restart")?;
    ensure!(
        interrupted["turn"]["status"] == "interrupted",
        "wait result: {}",
        interrupted.json
    );
    ensure!(
        work_calls(&model) == 7,
        "model calls after restart wait: {}",
        work_calls(&model)
    );
    drop(proxy);

    let collision = id(&agent(&app, &a, "test").await?)?;
    scripts(&app, &collision, 0, true).await?;
    code(
        &cli.run(&["status", &a, &turn]).await?,
        0,
        "id wins over a colliding name",
    )?;
    code(
        &cli.run(&["status", &a.to_uppercase(), &turn]).await?,
        0,
        "uppercase agent id",
    )?;
    app.restart().await?;
    ensure!(
        cli.run(&["status", &a, &turn]).await?["turn"]["reply"] == "Answer",
        "status after restart"
    );
    ensure!(work_calls(&model) == 7, "status replayed work");
    scripts(&app, &a, 1, false).await?;
    code(&cli.run(&["status", &a, &turn]).await?, 5, "revoked status")?;
    code(
        &cli.run(&["cancel", &a, &held_id]).await?,
        5,
        "revoked cancel",
    )?;
    for n in 0..99 {
        let extra = id(&agent(&app, &format!("Extra {n}"), "test").await?)?;
        scripts(&app, &extra, 0, true).await?;
    }
    let catalog = cli.run(&["list"]).await?;
    let ids: Vec<&str> = catalog["agents"]
        .as_array()
        .ok_or("agents")?
        .iter()
        .filter_map(|row| row["id"].as_str())
        .collect();
    ensure!(ids.len() == 101, "catalog size {}", ids.len());
    ensure!(
        ids.iter().collect::<std::collections::HashSet<_>>().len() == 101,
        "duplicate catalog entries"
    );
    ensure!(!ids.contains(&a.as_str()), "revoked agent listed");
    let token = cli.connection()?["token"]
        .as_str()
        .ok_or("token")?
        .to_owned();
    let client = app.client()?;
    let page = |path: String| {
        let request = client.get(app.url(&path)).bearer_auth(&token);
        async move {
            let response = request.send().await?;
            let status = response.status().as_u16();
            let body: Value = response.json().await.unwrap_or(Value::Null);
            Ok::<_, minarbot_testkit::Error>((status, body))
        }
    };
    let (_, first) = page("/api/scripts/agents".into()).await?;
    ensure!(
        first["agents"].as_array().map(Vec::len) == Some(100) && first["next_after"].is_string(),
        "first page: {first}"
    );
    let (_, second) = page(format!(
        "/api/scripts/agents?after={}",
        first["next_after"].as_str().unwrap_or_default()
    ))
    .await?;
    ensure!(
        second["agents"].as_array().map(Vec::len) == Some(1) && second["next_after"].is_null(),
        "second page: {second}"
    );
    for suffix in ["?after=invalid", "?unexpected=1"] {
        let (status, _) = page(format!("/api/scripts/agents{suffix}")).await?;
        ensure!(status == 400, "{suffix}: {status}");
    }
    let denied = client
        .get(app.url("/api/scripts/agents"))
        .bearer_auth(&token)
        .header("Origin", app.origin())
        .send()
        .await?;
    ensure!(
        denied.status().as_u16() == 403,
        "browser origin accepted: {}",
        denied.status()
    );

    let side = id(&app
        .post(
            &format!("/api/agents/{b}/threads"),
            json!({"id": new_id(), "name": "Cancellation scope"}),
        )
        .await?)?;
    let sides = cli.run(&["threads", &b]).await?;
    code(&sides, 0, "threads")?;
    ensure!(
        sides["threads"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"] == json!(side))),
        "threads: {}",
        sides.json
    );
    let created = cli.run(&["thread", &b, "CLI side"]).await?;
    code(&created, 0, "thread create")?;
    ensure!(
        created["conversation"]["name"] == "CLI side",
        "thread create: {}",
        created.json
    );
    let side_reply = cli
        .run(&[
            "run",
            &b,
            "Hello side",
            "--thread",
            &side,
            "--wait",
            "--timeout",
            "3",
        ])
        .await?;
    code(&side_reply, 0, "side run")?;
    ensure!(
        side_reply["turn"]["thread"] == json!(side) && side_reply["turn"]["reply"] == "Answer",
        "side run: {}",
        side_reply.json
    );
    let side_turn = id(&app
        .post(
            &format!("/api/threads/{side}/turns"),
            json!({"id": new_id(), "text": "HOLD"}),
        )
        .await?)?;
    code(
        &cli.run(&["cancel", &b, &side_turn]).await?,
        1,
        "cancel side turn without --thread",
    )?;
    app.post(
        &format!("/api/threads/{side}/turns/{side_turn}/cancel"),
        json!({"id": new_id()}),
    )
    .await?;
    let channel = id(&app
        .post("/api/channels", json!({"id": new_id(), "name": "Cancellation scope", "instructions": "Help", "members": [b], "lead": b}))
        .await?)?;
    let channel_turn = id(&app
        .post(
            &format!("/api/channels/{channel}/turns"),
            json!({"id": new_id(), "text": "HOLD", "agent": b}),
        )
        .await?)?;
    code(
        &cli.run(&["cancel", &b, &channel_turn]).await?,
        1,
        "cancel channel turn",
    )?;
    app.post(
        &format!("/api/channels/{channel}/turns/{channel_turn}/cancel"),
        json!({"id": new_id()}),
    )
    .await?;
    let revoke_target = cli.run(&["run", &b, "HOLD"]).await?;
    code(&revoke_target, 0, "run before revoke")?;
    let target = id(&revoke_target.json)?;
    let cancel_path = format!("/api/scripts/agents/{b}/turns/{target}/cancel");
    for method in [Method::GET, Method::PUT] {
        let response = client
            .request(method.clone(), app.url(&cancel_path))
            .bearer_auth(&token)
            .send()
            .await?;
        ensure!(
            response.status().as_u16() == 405,
            "{method} {cancel_path}: {}",
            response.status()
        );
    }
    let authorization = format!("Bearer {token}");
    let body = json!({"id": target}).to_string();
    let delayed = SplitRequest::start(
        app.origin(),
        "POST",
        &cancel_path,
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
        ],
        body.as_bytes(),
        1,
    )
    .await?;
    scripts(&app, &b, 1, false).await?;
    ensure!(
        delayed.finish().await? == 403,
        "cancel finished after revocation was accepted"
    );
    let retained = app
        .get(&format!("/api/agents/{b}/turns?focus={target}"))
        .await?;
    ensure!(
        ["queued", "running"].contains(&retained[0]["status"].as_str().unwrap_or_default()),
        "revoked cancel stopped work: {}",
        retained[0]
    );
    app.post(
        &format!("/api/agents/{b}/turns/{target}/cancel"),
        json!({"id": new_id()}),
    )
    .await?;
    let original = cli.connection()?;
    cli.write_connection(&json!({"url": "https://example.com", "token": "x".repeat(32)}))?;
    code(&cli.run(&["list"]).await?, 5, "foreign discovery URL")?;
    cli.write_connection(&original)?;
    code(
        &cli.run(&["run", &a, "Hello", "--id"]).await?,
        2,
        "missing --id value",
    )?;
    code(&cli.run(&["cancel", &a]).await?, 2, "cancel without turn")?;
    code(
        &cli.run(&["cancel", &a, &turn, "--wait"]).await?,
        2,
        "cancel --wait",
    )?;
    Ok(())
}

fn lossy_post(request: &minarbot_testkit::proxy::Proxied, suffixes: &[&str]) -> bool {
    request.method == "POST" && suffixes.iter().any(|suffix| request.path.ends_with(suffix))
}

#[tokio::test]
async fn cli_routine_lists_and_histories_stay_within_the_selected_granted_agent() -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.catalog(["fixture", "fixture-alt"]);
    let answer = std::sync::Arc::new(std::sync::Mutex::new("Routine result".to_owned()));
    let current = answer.clone();
    model
        .respond(move |_| Reply::text(current.lock().map(|text| text.clone()).unwrap_or_default()));
    let root = TestRoot::new()?;
    let directory = root.path().join("scripts");
    let api_key = new_id();
    let config = ServerConfig::new().script_directory(&directory).gateway(
        minarbot_testkit::server::Gateway {
            api_key: Some(api_key.clone()),
            ..minarbot_testkit::server::Gateway::new(model.base_url())
        },
    );
    let cli = Script::new(&directory)?;
    let mut app = root.start(config).await?;
    let suffixes = ["/run", "/retry", "/provider"];
    let proxy = LossyProxy::start(app.origin()).await?;
    let use_proxy = |app: &Server| -> Result<()> {
        proxy.set_target(app.origin());
        cli.use_url(proxy.origin())
    };
    let posts = |proxy: &LossyProxy| -> Vec<String> {
        proxy
            .requests()
            .into_iter()
            .filter(|request| lossy_post(request, &suffixes))
            .map(|request| request.body)
            .collect()
    };
    let drop_next = |proxy: &LossyProxy| {
        proxy.drop_next(move |request| {
            lossy_post(request, &["/run", "/retry", "/provider"]) && request.ok()
        });
    };
    let prompts = |model: &ScriptedModel| -> Vec<String> {
        model
            .requests()
            .iter()
            .map(|request| request.last_text())
            .collect()
    };
    let a = agent(&app, "Routine owner", "fixture").await?;
    let a_id = id(&a)?;
    let b = id(&agent(&app, "Other", "fixture").await?)?;
    for agent in [&a_id, &b] {
        scripts(&app, agent, 0, true).await?;
    }
    // `script provider <agent>` deadlocks the server (bug B2 in
    // docs/test-ledger.md); its read assertions stay in the Node case.
    // The argument check runs before any request.
    code(
        &cli.run(&["provider", &a_id, "--wait"]).await?,
        2,
        "provider --wait",
    )?;
    // `script models` always fails (bug B3); its catalog assertions stay in
    // the Node case.
    code(
        &cli.run(&["models", &a_id, "--wait"]).await?,
        2,
        "models --wait",
    )?;
    let clock = app.config().clock().ok_or("fixed clock")?;
    let routine = app
        .post(
            &format!("/api/agents/{a_id}/routines"),
            json!({"id": new_id(), "name": "Routine", "prompt": "Task", "nextAt": clock + 3_600_000,
                "intervalSeconds": null, "enabled": false}),
        )
        .await?;
    let routine_id = id(&routine)?;
    let list = cli.run(&["routines", &a_id]).await?;
    code(&list, 0, "routines")?;
    ensure!(
        list["routines"]
            .as_array()
            .map(|rows| rows.iter().map(|row| row["id"].clone()).collect::<Vec<_>>())
            == Some(vec![json!(routine_id)]),
        "routines: {}",
        list.json
    );
    ensure!(
        cli.run(&["routines", &b]).await?["routines"] == json!([]),
        "foreign routines listed"
    );
    ensure!(
        cli.run(&["routine-runs", &a_id, &routine_id]).await?["runs"] == json!([]),
        "runs before run"
    );
    code(
        &cli.run(&["routine-runs", &b, &routine_id]).await?,
        1,
        "foreign routine runs",
    )?;
    let request_id = new_id();
    let revision = routine["revision"].to_string();
    let command = [
        "routine-run",
        &a_id,
        &routine_id,
        &revision,
        "CLI input",
        "--id",
        &request_id,
        "--wait",
        "--timeout",
        "5",
    ];
    let mut foreign = command;
    foreign[1] = &b;
    code(&cli.run(&foreign).await?, 1, "foreign routine run")?;
    use_proxy(&app)?;
    drop_next(&proxy);
    let uncertain = cli.run(&command).await?;
    code(&uncertain, 5, "lost routine run reply")?;
    ensure!(
        uncertain["requestId"] == json!(request_id) && uncertain["routine"] == json!(routine_id),
        "uncertain: {}",
        uncertain.json
    );
    ensure!(posts(&proxy).len() == 1, "posts: {:?}", posts(&proxy));
    let run = cli.run(&command).await?;
    code(&run, 0, "routine run retry")?;
    ensure!(
        run["routine"] == json!(routine_id) && run["thread"].is_string(),
        "run: {}",
        run.json
    );
    ensure!(
        run["turn"]["reply"] == "Routine result",
        "run reply: {}",
        run.json
    );
    let sent = posts(&proxy);
    ensure!(
        sent.len() == 2 && sent[0] == sent[1],
        "retry changed the request: {sent:?}"
    );
    ensure!(
        cli.run(&command).await?["id"] == run["id"],
        "routine run replay id"
    );
    ensure!(
        prompts(&model) == ["Task\n\nScript input:\nCLI input"],
        "prompts: {:?}",
        prompts(&model)
    );
    let bearer = format!("Bearer {api_key}");
    ensure!(
        model.requests()[0].authorization.as_deref() == Some(bearer.as_str()),
        "the instance key did not reach the provider"
    );
    let history = cli
        .wait(&["routine-runs", &a_id, &routine_id], |history| {
            history["runs"][0]["status"] == "done"
        })
        .await?;
    code(&history, 0, "routine-runs")?;
    ensure!(
        history["runs"].as_array().map(Vec::len) == Some(1),
        "runs: {}",
        history.json
    );
    ensure!(
        history["runs"][0]["turn"] == run["id"] && history["runs"][0]["reply"] == "Routine result",
        "run history: {}",
        history.json
    );
    app.restart().await?;
    use_proxy(&app)?;
    ensure!(
        cli.run(&["routine-runs", &a_id, &routine_id]).await?["runs"] == history["runs"],
        "history after restart"
    );
    ensure!(
        cli.run(&command).await?["id"] == run["id"],
        "replay after restart"
    );
    ensure!(
        model.calls() == 1,
        "routine replay after restart called the model"
    );
    let accepted = cli.run(&command[..7]).await?;
    code(&accepted, 0, "routine run without wait")?;
    ensure!(
        accepted["id"] == run["id"]
            && accepted["thread"] == run["thread"]
            && accepted["turn"].is_null(),
        "accepted: {}",
        accepted.json
    );
    let mut changed = command;
    changed[4] = "Changed";
    code(&cli.run(&changed).await?, 1, "changed routine payload")?;
    let zero = [
        "routine-run",
        &a_id,
        &routine_id,
        "0",
        "CLI input",
        "--id",
        &request_id,
    ];
    let missing_id = ["routine-run", &a_id, &routine_id, &revision, "CLI input"];
    let thread = new_id();
    for bad in [
        &["routines", &a_id, "--wait"][..],
        &["routine-runs", &a_id],
        &["routine-runs", &a_id, "../bad"],
        &["routine-runs", &a_id, &routine_id, "--thread", &thread],
        &zero,
        &missing_id,
    ] {
        code(&cli.run(bad).await?, 2, &format!("{bad:?}"))?;
    }
    let workflow = app
        .post(
            &format!("/api/agents/{a_id}/workflows"),
            json!({"id": new_id(), "revision": 0, "name": "Retry condition", "graph": {"entry": "check", "maxSteps": 4, "nodes": [
                {"id": "check", "kind": "condition", "operator": "model", "model": "fixture", "value": "Yes?", "yes": "out", "no": "out"},
                {"id": "out", "kind": "output"}]}}),
        )
        .await?;
    let workflow_id = id(&workflow)?;
    let failed = cli
        .run(&[
            "workflow-run",
            &a_id,
            &workflow_id,
            "1",
            "Input",
            "--id",
            &new_id(),
            "--wait",
            "--timeout",
            "5",
        ])
        .await?;
    code(&failed, 1, "failing workflow condition")?;
    ensure!(
        failed["turn"]["status"] == "failed",
        "workflow: {}",
        failed.json
    );
    let failed_id = id(&failed.json)?;
    let failed_revision = failed["turn"]["revision"].to_string();
    let retry_id = new_id();
    let retry = [
        "workflow-retry",
        &a_id,
        &failed_id,
        &failed_revision,
        "--id",
        &retry_id,
    ];
    let mut foreign_retry = retry;
    foreign_retry[1] = &b;
    code(&cli.run(&foreign_retry).await?, 1, "foreign workflow retry")?;
    *answer.lock().map_err(|_| "poisoned")? = "yes".into();
    drop_next(&proxy);
    let lost_retry = cli.run(&retry).await?;
    code(&lost_retry, 5, "lost retry reply")?;
    ensure!(
        lost_retry["requestId"] == json!(retry_id),
        "lost retry: {}",
        lost_retry.json
    );
    cli.wait(&["workflow-status", &a_id, &failed_id], |status| {
        status["turn"]["status"] == "done"
    })
    .await?;
    let calls = model.calls();
    ensure!(
        calls == 3,
        "model calls after workflow retry: {:?}",
        prompts(&model)
    );
    app.restart().await?;
    use_proxy(&app)?;
    code(&cli.run(&retry).await?, 0, "retry replay after restart")?;
    let sent = posts(&proxy);
    ensure!(
        sent[sent.len() - 1] == sent[sent.len() - 2],
        "retry replay changed the request"
    );
    ensure!(model.calls() == calls, "retry replay reran the workflow");
    let agents = app.get("/api/agents").await?;
    let before = agents["agents"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == json!(a_id)))
        .ok_or("agent missing")?
        .clone();
    let payload = json!({"revision": before["revision"], "model": "fixture-alt", "backupModel": "", "providerAccount": ""}).to_string();
    let save_id = new_id();
    let save = ["provider-save", &a_id, &payload, "--id", &save_id];
    drop_next(&proxy);
    let lost_save = cli.run(&save).await?;
    code(&lost_save, 5, "lost provider save")?;
    ensure!(
        lost_save["requestId"] == json!(save_id),
        "lost save: {}",
        lost_save.json
    );
    app.restart().await?;
    use_proxy(&app)?;
    let updated = cli.run(&save).await?;
    code(&updated, 0, "provider save retry")?;
    ensure!(
        updated["id"] == json!(a_id) && updated["provider"]["model"] == "fixture-alt",
        "provider save: {}",
        updated.json
    );
    ensure!(
        updated["provider"]["name"] == a["name"]
            && updated["provider"]["instructions"] == a["instructions"],
        "provider save kept profile: {}",
        updated.json
    );
    let sent = posts(&proxy);
    ensure!(
        sent[sent.len() - 1] == sent[sent.len() - 2],
        "save retry changed the request"
    );
    code(
        &cli.run(&["provider-save", &a_id, &payload, "--id", &new_id()])
            .await?,
        1,
        "stale provider save",
    )?;
    code(
        &cli.run(&[
            "provider-save",
            &a_id,
            "{\"apiKey\":\"forbidden\"}",
            "--id",
            &new_id(),
        ])
        .await?,
        2,
        "provider secret field",
    )?;
    scripts(&app, &a_id, 1, false).await?;
    code(
        &cli.run(&["routine-runs", &a_id, &routine_id]).await?,
        5,
        "revoked routine-runs",
    )?;
    code(&cli.run(&command).await?, 5, "revoked routine-run")?;
    code(&cli.run(&retry).await?, 5, "revoked workflow-retry")?;
    code(&cli.run(&["models", &a_id]).await?, 5, "revoked models")?;
    code(&cli.run(&save).await?, 5, "revoked provider-save")?;
    Ok(())
}

fn ids(rows: &Value, key: &str) -> Vec<String> {
    let mut ids: Vec<String> = rows
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row[key].as_str().map(str::to_owned))
        .collect();
    ids.sort();
    ids
}

#[tokio::test]
async fn cli_workflow_discovery_paginates_and_explicit_approvals_retain_identity_after_lost_responses(
) -> Result<()> {
    let (root, cli, config) = setup(None)?;
    let mut app = root.start(config).await?;
    let proxy = LossyProxy::start(app.origin()).await?;
    let use_proxy = |app: &Server| -> Result<()> {
        proxy.set_target(app.origin());
        cli.use_url(proxy.origin())
    };
    let action = |path: &str| {
        ["/run", "/cancel", "/schedule", "/webhook"]
            .iter()
            .any(|suffix| path.ends_with(suffix))
    };
    let actions = |proxy: &LossyProxy| -> Vec<(String, String)> {
        proxy
            .requests()
            .into_iter()
            .filter(|request| request.method == "POST" && action(&request.path))
            .map(|request| (request.path, request.body))
            .collect()
    };
    let decisions = |proxy: &LossyProxy| -> Vec<String> {
        proxy
            .requests()
            .into_iter()
            .filter(|request| request.path.ends_with("/approve"))
            .map(|request| request.body)
            .collect()
    };
    let drop_action = |proxy: &LossyProxy, suffix: &'static str| {
        proxy.drop_next(move |request| {
            request.method == "POST" && request.path.ends_with(suffix) && request.ok()
        });
    };
    let last_two_equal = |items: &[(String, String)]| {
        items.len() >= 2 && items[items.len() - 1] == items[items.len() - 2]
    };
    let owner = id(&agent(&app, "Approver", "fixture").await?)?;
    let other = id(&agent(&app, "Other", "fixture").await?)?;
    for agent in [&owner, &other] {
        scripts(&app, agent, 0, true).await?;
    }
    let mut workflows = Vec::new();
    let mut runs = Vec::new();
    for n in 0..51 {
        let workflow = id(&app
            .post(
                &format!("/api/agents/{owner}/workflows"),
                json!({"id": new_id(), "revision": 0, "name": format!("Approval {n}"), "graph": {"entry": "ask", "maxSteps": 2, "nodes": [
                    {"id": "ask", "kind": "approval", "prompt": format!("Approve {n}?"), "yes": "out"},
                    {"id": "out", "kind": "output"}]}}),
            )
            .await?)?;
        runs.push(id(&app
            .post(
                &format!("/api/workflows/{workflow}/run"),
                json!({"id": new_id(), "revision": 1, "input": format!("Input {n}")}),
            )
            .await?)?);
        workflows.push(workflow);
    }
    use_proxy(&app)?;
    let list = cli.run(&args!["workflows", owner]).await?;
    code(&list, 0, "workflows")?;
    let mut sorted = workflows.clone();
    sorted.sort();
    ensure!(
        ids(&list["workflows"], "id") == sorted,
        "paginated workflows: {}",
        list.json
    );
    let read_hook = args!["workflow-webhook", owner, workflows[0]];
    ensure!(
        cli.run(&read_hook).await?["webhook"].is_null(),
        "webhook before save"
    );
    let hook_command = args![
        "workflow-webhook-save",
        owner,
        workflows[0],
        json!({"revision": 0, "rotate": true, "enabled": true}),
        "--id",
        new_id()
    ];
    drop_action(&proxy, "/webhook");
    let lost_hook = cli.run(&hook_command).await?;
    code(&lost_hook, 5, "lost webhook save")?;
    ensure!(
        lost_hook["requestId"] == json!(hook_command[5]),
        "lost webhook: {}",
        lost_hook.json
    );
    app.restart().await?;
    use_proxy(&app)?;
    let hook_result = cli.run(&hook_command).await?;
    code(&hook_result, 0, "webhook save retry")?;
    let hook = hook_result["webhook"].clone();
    let secret = hook["secret"].as_str().ok_or("webhook secret")?.to_owned();
    ensure!(
        last_two_equal(&actions(&proxy)),
        "webhook retry changed the request"
    );
    ensure!(
        cli.run(&hook_command).await?["webhook"]["secret"] == json!(secret),
        "webhook replay secret"
    );
    let mut foreign = hook_command.clone();
    foreign[1] = other.clone();
    code(&cli.run(&foreign).await?, 1, "foreign webhook save")?;
    proxy.clear_log();
    let inspected = cli.run(&read_hook).await?;
    code(&inspected, 0, "webhook read")?;
    ensure!(
        inspected["webhook"] == json!({"route": hook["route"], "revision": 1, "enabled": 1}),
        "webhook: {}",
        inspected.json
    );
    ensure!(
        !inspected.stdout.contains(&secret),
        "webhook read printed the secret"
    );
    code(
        &cli.run(&args!["workflow-webhook", other, workflows[0]])
            .await?,
        1,
        "foreign webhook read",
    )?;
    code(
        &cli.run(&[&read_hook[..], &args!["--wait"]].concat())
            .await?,
        2,
        "webhook --wait",
    )?;
    let disabled = cli
        .run(&args![
            "workflow-webhook-save",
            owner,
            workflows[0],
            json!({"revision": 1, "enabled": false, "rotate": false}),
            "--id",
            new_id()
        ])
        .await?;
    code(&disabled, 0, "disable webhook")?;
    ensure!(
        disabled["webhook"]["secret"].is_null() && !disabled.stdout.contains(&secret),
        "disabled webhook: {}",
        disabled.json
    );
    code(&cli.run(&hook_command).await?, 1, "stale webhook replay")?;
    let rotated = cli
        .run(&args![
            "workflow-webhook-save",
            owner,
            workflows[0],
            json!({"revision": 2, "enabled": true, "rotate": true}),
            "--id",
            new_id()
        ])
        .await?;
    code(&rotated, 0, "rotate webhook")?;
    let rotated_secret = rotated["webhook"]["secret"].as_str().unwrap_or_default();
    ensure!(
        !rotated_secret.is_empty() && rotated_secret != secret,
        "rotated secret: {}",
        rotated.json
    );
    code(
        &cli.run(&hook_command).await?,
        1,
        "stale webhook replay after rotation",
    )?;
    code(
        &cli.run(&args![
            "workflow-webhook-save",
            owner,
            workflows[0],
            json!({"secret": "forbidden"}),
            "--id",
            new_id()
        ])
        .await?,
        2,
        "webhook secret field",
    )?;
    let remove_hook = args![
        "workflow-webhook-save",
        owner,
        workflows[0],
        json!({"revision": 3, "remove": true, "route": hook["route"]}),
        "--id",
        new_id()
    ];
    code(&cli.run(&remove_hook).await?, 0, "remove webhook")?;
    code(&cli.run(&remove_hook).await?, 0, "remove webhook replay")?;
    ensure!(
        cli.run(&read_hook).await?["webhook"]
            == json!({"route": null, "revision": 4, "enabled": 0}),
        "removed webhook"
    );
    let recreated = cli
        .run(&args![
            "workflow-webhook-save",
            owner,
            workflows[0],
            json!({"revision": 4, "enabled": true, "rotate": true}),
            "--id",
            new_id()
        ])
        .await?;
    code(&recreated, 0, "recreate webhook")?;
    ensure!(
        recreated["webhook"]["route"] != hook["route"],
        "recreated webhook reused the route"
    );
    code(&cli.run(&remove_hook).await?, 0, "old remove replay")?;
    ensure!(
        cli.run(&read_hook).await?["webhook"]
            == json!({"route": recreated["webhook"]["route"], "revision": 5, "enabled": 1}),
        "old remove replay removed the new webhook"
    );
    proxy.clear_log();

    let pending = cli
        .wait(&args!["approvals", owner], |run| {
            run["approvals"].as_array().map(Vec::len) == Some(51)
        })
        .await?;
    code(&pending, 0, "approvals")?;
    let mut expected_runs = runs.clone();
    expected_runs.sort();
    ensure!(
        ids(&pending["approvals"], "run") == expected_runs,
        "approvals: {}",
        pending.json
    );
    ensure!(
        cli.run(&args!["approvals", other]).await?["approvals"] == json!([]),
        "foreign approvals listed"
    );
    let first = pending["approvals"][0].clone();
    let first_run = first["run"].as_str().ok_or("run")?.to_owned();
    let first_revision = first["revision"].as_i64().ok_or("revision")?;
    let request_id = new_id();
    let command = args![
        "approve",
        owner,
        first_run,
        first_revision,
        "yes",
        "--id",
        request_id
    ];
    let mut foreign = command.clone();
    foreign[1] = other.clone();
    code(&cli.run(&foreign).await?, 1, "foreign approval")?;
    code(
        &cli.run(&args![
            "approve",
            owner,
            first_run,
            first_revision + 1,
            "yes",
            "--id",
            new_id()
        ])
        .await?,
        1,
        "stale approval",
    )?;
    proxy.drop_next(|request| request.path.ends_with("/approve"));
    let lost = cli.run(&command).await?;
    code(&lost, 5, "lost approval")?;
    ensure!(
        lost["id"] == json!(first_run) && lost["requestId"] == json!(request_id),
        "lost approval: {}",
        lost.json
    );
    app.restart().await?;
    use_proxy(&app)?;
    code(&cli.run(&command).await?, 0, "approval retry")?;
    let sent = decisions(&proxy);
    ensure!(
        sent.len() >= 2 && sent[sent.len() - 1] == sent[sent.len() - 2],
        "approval retry changed the decision"
    );
    ensure!(
        app.get(&format!("/api/workflow-runs/{first_run}")).await?["status"] == "done",
        "approved run"
    );
    let mut changed = command.clone();
    changed[4] = "no".into();
    code(&cli.run(&changed).await?, 1, "changed decision")?;
    let second = pending["approvals"][1].clone();
    let second_run = second["run"].as_str().ok_or("run")?;
    code(
        &cli.run(&args![
            "approve",
            owner,
            second_run,
            second["revision"],
            "no",
            "--id",
            new_id()
        ])
        .await?,
        0,
        "reject",
    )?;
    ensure!(
        app.get(&format!("/api/workflow-runs/{second_run}")).await?["status"] == "failed",
        "rejected run"
    );

    let start = args![
        "workflow-run",
        owner,
        workflows[0],
        "1",
        "CLI workflow",
        "--id",
        new_id(),
        "--wait",
        "--timeout",
        "5"
    ];
    let mut uppercase = start.clone();
    uppercase[6] = "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA".into();
    code(&cli.run(&uppercase).await?, 2, "uppercase id")?;
    drop_action(&proxy, "/run");
    let lost_start = cli.run(&start).await?;
    code(&lost_start, 5, "lost workflow start")?;
    ensure!(
        lost_start["requestId"] == json!(start[6]),
        "lost start: {}",
        lost_start.json
    );
    ensure!(actions(&proxy).len() == 1, "actions: {:?}", actions(&proxy));
    app.restart().await?;
    use_proxy(&app)?;
    let started = cli.run(&start).await?;
    code(&started, 3, "started approval workflow")?;
    ensure!(
        started["id"] == json!(start[6]) && started["turn"]["status"] == "approval",
        "started: {}",
        started.json
    );
    let sent = actions(&proxy);
    ensure!(
        sent.len() == 2 && sent[0] == sent[1],
        "start retry changed the request: {sent:?}"
    );
    let saved_runs = app
        .get(&format!("/api/workflows/{}/runs", workflows[0]))
        .await?;
    ensure!(
        saved_runs.as_array().map(Vec::len) == Some(2),
        "runs: {saved_runs}"
    );
    ensure!(
        saved_runs
            .as_array()
            .into_iter()
            .flatten()
            .filter(|run| run["id"] == json!(start[6]))
            .count()
            == 1,
        "duplicate run"
    );
    ensure!(
        cli.run(&start).await?["id"] == json!(start[6]),
        "start replay id"
    );
    let started_id = id(&started.json)?;
    ensure!(
        cli.run(&args!["workflow-status", owner, started_id])
            .await?["turn"]["status"]
            == "approval",
        "status"
    );
    code(
        &cli.run(&args!["workflow-status", other, started_id])
            .await?,
        1,
        "foreign status",
    )?;
    code(
        &cli.run(&args![
            "approve",
            owner,
            started_id,
            started["turn"]["revision"],
            "yes",
            "--id",
            new_id()
        ])
        .await?,
        0,
        "approve started",
    )?;
    ensure!(
        cli.run(&start).await?["turn"]["status"] == "done",
        "approved workflow status"
    );
    let mut changed = start.clone();
    changed[4] = "Changed".into();
    code(&cli.run(&changed).await?, 1, "changed workflow input")?;
    let mut next = start.clone();
    next[6] = new_id();
    let waiting = cli.run(&next).await?;
    code(&waiting, 3, "second workflow")?;
    let waiting_id = id(&waiting.json)?;
    let cancel = args![
        "workflow-cancel",
        owner,
        waiting_id,
        waiting["turn"]["revision"],
        "--id",
        new_id()
    ];
    drop_action(&proxy, "/cancel");
    let lost_cancel = cli.run(&cancel).await?;
    code(&lost_cancel, 5, "lost cancel")?;
    ensure!(
        lost_cancel["id"] == json!(waiting_id) && lost_cancel["requestId"] == json!(cancel[5]),
        "lost cancel: {}",
        lost_cancel.json
    );
    let count = actions(&proxy).len();
    app.restart().await?;
    use_proxy(&app)?;
    code(&cli.run(&cancel).await?, 0, "cancel retry")?;
    let sent = actions(&proxy);
    ensure!(
        sent.len() == count + 1 && last_two_equal(&sent),
        "cancel retry changed the request"
    );
    code(&cli.run(&cancel).await?, 0, "cancel replay")?;
    code(&cli.run(&cancel).await?, 0, "cancel replay")?;
    let cancelled = cli
        .run(&args!["workflow-status", owner, waiting_id])
        .await?;
    code(&cancelled, 1, "cancelled status")?;
    ensure!(
        cancelled["turn"]["status"] == "cancelled",
        "cancelled: {}",
        cancelled.json
    );

    ensure!(
        cli.run(&args!["workflow-schedule", owner, workflows[0]])
            .await?["schedule"]["schedule"]
            .is_null(),
        "schedule before save"
    );
    let clock = app.config().clock().ok_or("fixed clock")?;
    let schedule = json!({"revision": 0, "nextAt": clock + 3_600_000, "intervalSeconds": 60, "input": "Scheduled", "enabled": true});
    let save_schedule = args![
        "workflow-schedule-save",
        owner,
        workflows[0],
        schedule,
        "--id",
        new_id()
    ];
    let mut foreign = save_schedule.clone();
    foreign[1] = other.clone();
    code(&cli.run(&foreign).await?, 1, "foreign schedule")?;
    drop_action(&proxy, "/schedule");
    let lost_schedule = cli.run(&save_schedule).await?;
    code(&lost_schedule, 5, "lost schedule save")?;
    ensure!(
        lost_schedule["requestId"] == json!(save_schedule[5]),
        "lost schedule: {}",
        lost_schedule.json
    );
    app.restart().await?;
    use_proxy(&app)?;
    code(&cli.run(&save_schedule).await?, 0, "schedule retry")?;
    ensure!(
        last_two_equal(&actions(&proxy)),
        "schedule retry changed the request"
    );
    let saved = cli
        .run(&args!["workflow-schedule", owner, workflows[0]])
        .await?;
    ensure!(
        saved["schedule"]["schedule"]["revision"] == 1
            && saved["schedule"]["schedule"]["enabled"] == 1,
        "schedule: {}",
        saved.json
    );
    ensure!(
        saved["schedule"]["occurrences"] == json!([]),
        "occurrences: {}",
        saved.json
    );
    let mut disabled = schedule.clone();
    disabled["revision"] = json!(1);
    disabled["enabled"] = json!(false);
    code(
        &cli.run(&args![
            "workflow-schedule-save",
            owner,
            workflows[0],
            disabled,
            "--id",
            new_id()
        ])
        .await?,
        0,
        "disable schedule",
    )?;
    code(
        &cli.run(&args![
            "workflow-schedule-save",
            owner,
            workflows[0],
            schedule,
            "--id",
            new_id()
        ])
        .await?,
        1,
        "stale schedule",
    )?;
    ensure!(
        cli.run(&args!["workflow-schedule", owner, workflows[0]])
            .await?["schedule"]["schedule"]["enabled"]
            == 0,
        "schedule disabled"
    );
    let mut boundary = schedule.clone();
    boundary["revision"] = json!(2);
    boundary["enabled"] = json!(false);
    boundary["input"] = json!("\"".repeat(16000));
    code(
        &cli.run(&args![
            "workflow-schedule-save",
            owner,
            workflows[0],
            boundary,
            "--id",
            new_id()
        ])
        .await?,
        0,
        "16000-character input",
    )?;
    ensure!(
        cli.run(&args!["workflow-schedule", owner, workflows[0]])
            .await?["schedule"]["schedule"]["input"]
            == boundary["input"],
        "boundary input"
    );
    let mut over = boundary.clone();
    over["revision"] = json!(3);
    over["input"] = json!(format!("{}x", "\"".repeat(16000)));
    code(
        &cli.run(&args![
            "workflow-schedule-save",
            owner,
            workflows[0],
            over,
            "--id",
            new_id()
        ])
        .await?,
        1,
        "oversized input",
    )?;
    for bad in [
        args!["workflows", owner, "--wait"],
        args!["approvals", owner, "--thread", new_id()],
        command[..5].to_vec(),
        args!["approve", owner, first_run, "1", "maybe", "--id", new_id()],
        args!["workflow-retry", owner, first_run, "0", "--id", new_id()],
    ] {
        code(&cli.run(&bad).await?, 2, &format!("{bad:?}"))?;
    }
    scripts(&app, &owner, 1, false).await?;
    code(&cli.run(&command).await?, 5, "revoked approve")?;
    code(&cli.run(&save_schedule).await?, 5, "revoked schedule")?;
    code(&cli.run(&read_hook).await?, 5, "revoked webhook read")?;
    code(&cli.run(&hook_command).await?, 5, "revoked webhook save")?;
    Ok(())
}

#[tokio::test]
async fn cli_answers_retain_receipt_identity_across_lost_replies_and_restart() -> Result<()> {
    let model = ScriptedModel::start().await?;
    let answers = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = answers.clone();
    model.respond(move |request| {
        if after_question(request) {
            return after_question_reply();
        }
        if request.last_text() == "ASK" {
            return Reply::tool_call(
                "ask",
                "ask_user",
                json!({"question": "Proceed?", "choices": ["Yes", "No"]}),
            );
        }
        let answer = serde_json::from_str::<Value>(&request.last_text())
            .ok()
            .and_then(|content| content["answer"].as_str().map(str::to_owned));
        match answer {
            Some(answer) => {
                if let Ok(mut answers) = seen.lock() {
                    answers.push(answer);
                }
                Reply::text("Answered")
            }
            None => Reply::status(
                500,
                json!({"error": "expected an answer", "last": request.last_text()}),
            ),
        }
    });
    let answered = |answers: &std::sync::Arc<std::sync::Mutex<Vec<String>>>| {
        answers.lock().map(|rows| rows.clone()).unwrap_or_default()
    };
    let (root, cli, config) = setup(Some(&model))?;
    let mut app = root.start(config).await?;
    let proxy = LossyProxy::start(app.origin()).await?;
    let use_proxy = |app: &Server| -> Result<()> {
        proxy.set_target(app.origin());
        cli.use_url(proxy.origin())
    };
    let posts = |proxy: &LossyProxy| -> Vec<String> {
        proxy
            .requests()
            .into_iter()
            .filter(|request| request.path.ends_with("/answer"))
            .map(|request| request.body)
            .collect()
    };
    let drop_answer =
        |proxy: &LossyProxy| proxy.drop_next(|request| request.path.ends_with("/answer"));
    let a = id(&agent(&app, "Answer agent", "fixture").await?)?;
    let b = id(&agent(&app, "Other", "fixture").await?)?;
    for agent in [&a, &b] {
        scripts(&app, agent, 0, true).await?;
    }
    use_proxy(&app)?;
    let ask = cli
        .run(&args!["run", a, "ASK", "--wait", "--timeout", WAIT_SECONDS])
        .await?;
    code(&ask, 3, "question")?;
    let ask_id = id(&ask.json)?;
    let question = ask["turn"]["questions"][0]["id"]
        .as_str()
        .ok_or("question")?
        .to_owned();
    let request_id = new_id();
    let answer = args!["answer", a, ask_id, question, "Yes", "--id", request_id];
    code(
        &cli.run(&args!["answer", a, ask_id, question, "Yes"])
            .await?,
        2,
        "answer without --id",
    )?;
    code(
        &cli.run(&args![
            "answer",
            a,
            ask_id,
            question,
            "x".repeat(4001),
            "--id",
            new_id()
        ])
        .await?,
        2,
        "oversized answer",
    )?;
    code(
        &cli.run(&args![
            "answer",
            a,
            ask_id,
            question,
            "Wrong",
            "--id",
            new_id()
        ])
        .await?,
        1,
        "unknown choice",
    )?;
    code(
        &cli.run(&args![
            "answer",
            b,
            ask_id,
            question,
            "Yes",
            "--id",
            new_id()
        ])
        .await?,
        1,
        "foreign answer",
    )?;
    ensure!(
        work_calls(&model) == 1,
        "rejected answers reached the model"
    );
    drop_answer(&proxy);
    let lost = cli.run(&answer).await?;
    code(&lost, 5, "lost answer reply")?;
    ensure!(
        lost["id"] == json!(ask_id)
            && lost["requestId"] == json!(request_id)
            && lost["question"] == json!(question),
        "lost: {}",
        lost.json
    );
    let done = cli
        .run(&[&answer[..], &args!["--wait", "--timeout", WAIT_SECONDS]].concat())
        .await?;
    code(&done, 0, "answer retry")?;
    ensure!(
        done["turn"]["reply"] == "Answered" && done["requestId"] == json!(request_id),
        "answer retry: {}",
        done.json
    );
    let sent = posts(&proxy);
    ensure!(
        sent.len() >= 2 && sent[sent.len() - 1] == sent[sent.len() - 2],
        "answer retry changed the request"
    );
    ensure!(
        answered(&answers) == ["Yes"] && work_calls(&model) == 2,
        "answers {:?}, calls {}",
        answered(&answers),
        work_calls(&model)
    );
    app.restart().await?;
    use_proxy(&app)?;
    let replay = cli.run(&answer).await?;
    code(&replay, 0, "answer replay after restart")?;
    ensure!(
        replay["answer"]["answer"] == "Yes" && work_calls(&model) == 2,
        "replay: {}",
        replay.json
    );
    code(
        &cli.run(&args![
            "answer", a, ask_id, question, "No", "--id", request_id
        ])
        .await?,
        1,
        "changed answer",
    )?;
    let next = cli
        .run(&args![
            "run",
            a,
            "ASK",
            "--id",
            new_id().to_uppercase(),
            "--wait",
            "--timeout",
            "5"
        ])
        .await?;
    code(&next, 3, "second question")?;
    proxy.drop_next(|request| request.path.contains("?focus="));
    let post_count = posts(&proxy).len();
    let stdin = cli
        .run_input(
            &args![
                "answer",
                a,
                id(&next.json)?,
                next["turn"]["questions"][0]["id"]
                    .as_str()
                    .ok_or("question")?,
                "-",
                "--id",
                new_id(),
                "--wait",
                "--timeout",
                "5"
            ],
            b"No",
        )
        .await?;
    code(&stdin, 0, "stdin answer with lost status reply")?;
    ensure!(
        answered(&answers) == ["Yes", "No"],
        "answers: {:?}",
        answered(&answers)
    );
    ensure!(
        proxy.pending_rules() == 0 && posts(&proxy).len() == post_count + 1,
        "status loss resent the answer"
    );
    let uncertain = cli
        .run(&args!["run", a, "ASK", "--wait", "--timeout", WAIT_SECONDS])
        .await?;
    code(&uncertain, 3, "third question")?;
    let uncertain_id = id(&uncertain.json)?;
    app.edit_database(|db| {
        db.execute(
            "UPDATE turns SET status='interrupted',revision=revision+1 WHERE id=?",
            [&uncertain_id],
        )?;
        Ok(())
    })
    .await?;
    use_proxy(&app)?;
    let uncertain_answer = args![
        "answer",
        a,
        uncertain_id,
        uncertain["turn"]["questions"][0]["id"]
            .as_str()
            .ok_or("question")?,
        "Yes",
        "--id",
        new_id()
    ];
    let before = work_calls(&model);
    code(
        &cli.run(&uncertain_answer).await?,
        1,
        "answer to interrupted turn",
    )?;
    ensure!(
        work_calls(&model) == before,
        "interrupted answer reached the model"
    );
    code(
        &cli.run(
            &[
                &uncertain_answer[..],
                &args![
                    "--acknowledge-uncertain",
                    "--wait",
                    "--timeout",
                    WAIT_SECONDS
                ],
            ]
            .concat(),
        )
        .await?,
        0,
        "acknowledged answer",
    )?;
    ensure!(
        work_calls(&model) == before + 1,
        "acknowledged answer calls: {}",
        work_calls(&model)
    );
    let lower_id = "abcdef12-3456-4789-abcd-0123456789ab";
    let upper_id = lower_id.to_uppercase();
    code(
        &cli.run(&args![
            "run",
            a,
            "ASK",
            "--id",
            lower_id,
            "--wait",
            "--timeout",
            "5"
        ])
        .await?,
        3,
        "lowercase id",
    )?;
    code(
        &cli.run(&args![
            "run",
            a,
            "ASK",
            "--id",
            upper_id,
            "--wait",
            "--timeout",
            "5"
        ])
        .await?,
        3,
        "uppercase id",
    )?;
    let cancelled_upper = cli.run(&args!["cancel", a, upper_id]).await?;
    code(&cancelled_upper, 0, "cancel uppercase")?;
    ensure!(
        cancelled_upper["turn"]["id"] == json!(upper_id),
        "cancelled: {}",
        cancelled_upper.json
    );
    let untouched = cli.run(&args!["status", a, lower_id]).await?;
    code(&untouched, 3, "lowercase untouched")?;
    ensure!(
        untouched["turn"]["status"] == "queued",
        "lowercase turn: {}",
        untouched.json
    );
    code(
        &cli.run(&args!["cancel", a, lower_id]).await?,
        0,
        "cancel lowercase",
    )?;
    scripts(&app, &a, 1, false).await?;
    code(&cli.run(&answer).await?, 5, "revoked answer")?;
    ensure!(
        work_calls(&model) == before + 3,
        "calls after id-case checks: {}",
        work_calls(&model)
    );

    let side_id = new_id();
    let create = args!["thread", b, "Terminal side", "--id", side_id];
    proxy.drop_next(|request| request.method == "POST" && request.path.ends_with("/threads"));
    let lost_create = cli.run(&create).await?;
    code(&lost_create, 5, "lost thread create")?;
    ensure!(
        lost_create["id"] == json!(side_id),
        "lost create: {}",
        lost_create.json
    );
    let side = cli.run(&create).await?;
    code(&side, 0, "thread create retry")?;
    ensure!(
        side["conversation"]["id"] == json!(side_id),
        "side: {}",
        side.json
    );
    ensure!(
        cli.run(&create).await?["conversation"]["id"] == json!(side_id),
        "thread create replay"
    );
    let threads = cli.run(&args!["threads", b]).await?;
    ensure!(
        threads["threads"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|row| row["id"] == json!(side_id))
            .count()
            == 1,
        "threads: {}",
        threads.json
    );
    let side_args = args![
        "run",
        b,
        "ASK",
        "--thread",
        side_id,
        "--id",
        new_id().to_uppercase(),
        "--wait",
        "--timeout",
        "5"
    ];
    let before_side = work_calls(&model);
    let thread_turns = format!("/threads/{side_id}/turns");
    proxy.drop_next(move |request| {
        request.method == "POST" && request.path.ends_with(&thread_turns)
    });
    let lost_send = cli.run(&side_args).await?;
    code(&lost_send, 5, "lost side send")?;
    ensure!(
        lost_send["thread"] == json!(side_id) && lost_send["id"] == json!(side_args[6]),
        "lost side send: {}",
        lost_send.json
    );
    let side_ask = cli.run(&side_args).await?;
    code(&side_ask, 3, "side question")?;
    ensure!(
        side_ask["thread"] == json!(side_id),
        "side ask: {}",
        side_ask.json
    );
    let side_calls = work_calls(&model);
    ensure!(
        side_calls == before_side + 1,
        "side send calls: {side_calls}"
    );
    ensure!(
        cli.run(&side_args).await?["id"] == side_ask["id"] && work_calls(&model) == side_calls,
        "side replay"
    );
    let side_ask_id = id(&side_ask.json)?;
    code(
        &cli.run(&args!["status", b, side_ask_id]).await?,
        1,
        "side status without --thread",
    )?;
    code(
        &cli.run(&args!["cancel", b, side_ask_id]).await?,
        1,
        "side cancel without --thread",
    )?;
    code(
        &cli.run(&args!["status", b, side_ask_id, "--thread", side_id])
            .await?,
        3,
        "side status",
    )?;
    let side_answer = args![
        "answer",
        b,
        side_ask_id,
        side_ask["turn"]["questions"][0]["id"]
            .as_str()
            .ok_or("question")?,
        "Yes",
        "--thread",
        side_id,
        "--id",
        new_id()
    ];
    drop_answer(&proxy);
    let side_lost = cli.run(&side_answer).await?;
    code(&side_lost, 5, "lost side answer")?;
    ensure!(
        side_lost["thread"] == json!(side_id),
        "side lost: {}",
        side_lost.json
    );
    let side_done = cli
        .run(
            &[
                &side_answer[..],
                &args!["--wait", "--timeout", WAIT_SECONDS],
            ]
            .concat(),
        )
        .await?;
    code(&side_done, 0, "side answer retry")?;
    ensure!(
        side_done["turn"]["thread"] == json!(side_id) && work_calls(&model) == side_calls + 1,
        "side done: {}",
        side_done.json
    );
    app.restart().await?;
    use_proxy(&app)?;
    code(&cli.run(&side_answer).await?, 0, "side answer replay")?;
    ensure!(
        work_calls(&model) == side_calls + 1,
        "side answer replay reached the model"
    );
    let side_next = cli
        .run(&args![
            "run",
            b,
            "ASK",
            "--thread",
            side_id,
            "--wait",
            "--timeout",
            "5"
        ])
        .await?;
    code(&side_next, 3, "next side question")?;
    let side_next_id = id(&side_next.json)?;
    code(
        &cli.run(&args!["cancel", b, side_next_id, "--thread", side_id])
            .await?,
        0,
        "side cancel",
    )?;
    ensure!(
        cli.run(&args!["status", b, side_next_id, "--thread", side_id])
            .await?["turn"]["status"]
            == "cancelled",
        "side cancelled"
    );
    for bad in [
        args!["threads", b, "extra"],
        args!["thread", b, "name", "--wait"],
        args!["run", b, "ASK", "--thread", "bad"],
        args!["run", b, "ASK", "--thread", side_id, "--thread", side_id],
    ] {
        code(&cli.run(&bad).await?, 2, &format!("{bad:?}"))?;
    }
    Ok(())
}
