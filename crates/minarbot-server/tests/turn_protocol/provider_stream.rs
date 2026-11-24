use super::*;
use std::{future::pending, os::unix::process::ExitStatusExt};
use tokio::sync::watch;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    OpenAi,
    Anthropic,
}

impl Family {
    fn name(self) -> &'static str {
        match self {
            Self::OpenAi => "openai-compatible",
            Self::Anthropic => "anthropic",
        }
    }

    fn text(self, text: &str) -> Bytes {
        match self {
            Self::OpenAi => text_delta(text),
            Self::Anthropic => event(
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
            ),
        }
    }

    fn finish(self, tool: bool, marker: bool) -> Vec<Bytes> {
        let mut frames = match self {
            Self::OpenAi => vec![delta(
                json!({}),
                json!(if tool { "tool_calls" } else { "stop" }),
            )],
            Self::Anthropic => vec![
                event(json!({"type":"content_block_stop","index":0})),
                event(
                    json!({"type":"message_delta","delta":{"stop_reason":if tool { "tool_use" } else { "end_turn" }},"usage":{"output_tokens":4}}),
                ),
            ],
        };
        if self == Self::OpenAi && !tool {
            frames.push(event(json!({"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":4,"total_tokens":9}})));
        }
        if marker {
            frames.push(match self {
                Self::OpenAi => event("[DONE]"),
                Self::Anthropic => event(json!({"type":"message_stop"})),
            });
        }
        frames
    }

    fn tool(self) -> Vec<Bytes> {
        let args = json!({"name":"stream.txt","text":"Exactly once"}).to_string();
        let (first, rest) = args.split_at(10);
        match self {
            Self::OpenAi => vec![
                delta(
                    json!({"tool_calls":[{"index":0,"id":"stream-tool","type":"function","function":{"name":"create_text_file","arguments":first}}]}),
                    Value::Null,
                ),
                delta(
                    json!({"tool_calls":[{"index":0,"function":{"arguments":rest}}]}),
                    Value::Null,
                ),
                delta(
                    json!({"tool_calls":[{"index":0,"extra_content":{"google":{"thought_signature":"stream-signature"}}}]}),
                    Value::Null,
                ),
            ],
            Self::Anthropic => vec![
                event(
                    json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"stream-tool","name":"create_text_file","input":{}}}),
                ),
                event(
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":first}}),
                ),
                event(
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":rest}}),
                ),
            ],
        }
    }

    fn tool_result(self, body: &Value) -> bool {
        let Some(last) = messages(body).last() else {
            return false;
        };
        match self {
            Self::OpenAi => last["role"] == "tool",
            Self::Anthropic => last["content"]
                .as_array()
                .is_some_and(|blocks| blocks.iter().any(|block| block["type"] == "tool_result")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    Text,
    Broken,
    Tool,
    BrokenTool,
    WrongType,
    ToolHold,
    Hold,
}

#[derive(Clone)]
struct StreamingProvider {
    family: Family,
    scenario: watch::Receiver<Scenario>,
    calls: Calls,
    release: Arc<Notify>,
    wrong_type_closed: Arc<Notify>,
}

struct NotifyOnDrop(Arc<Notify>);

impl Drop for NotifyOnDrop {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

async fn stream_provider(
    axum::extract::State(state): axum::extract::State<StreamingProvider>,
    Json(body): Json<Value>,
) -> Response {
    state.calls.record(&body);
    let scenario = *state.scenario.borrow();
    if scenario == Scenario::WrongType {
        let closed = NotifyOnDrop(state.wrong_type_closed);
        return (
            [(header::CONTENT_TYPE, "application/json")],
            Body::from_stream(async_stream::stream! {
                let _closed = closed;
                yield Ok::<_, Infallible>("{}");
                pending::<()>().await;
            }),
        )
            .into_response();
    }

    upstream_sse(async_stream::stream! {
        let family = state.family;
        if family == Family::Anthropic {
            yield event(json!({"type":"message_start","message":{"type":"message","role":"assistant","content":[],"usage":{"input_tokens":2,"output_tokens":0,"cache_read_input_tokens":3}}}));
        }
        if matches!(scenario, Scenario::Tool | Scenario::ToolHold | Scenario::BrokenTool) && !family.tool_result(&body) {
            for frame in family.tool().into_iter().chain(family.finish(true, scenario != Scenario::BrokenTool)) {
                yield frame;
            }
            return;
        }

        if family == Family::Anthropic {
            yield event(json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}));
        }
        yield family.text("Partial ");
        match scenario {
            Scenario::Hold | Scenario::ToolHold => state.release.notified().await,
            Scenario::Broken => return,
            _ => yield family.text("complete"),
        }
        for frame in family.finish(false, true) {
            yield frame;
        }
    })
}

async fn settled(app: &Server<'_>, path: &str, turn: &str) -> Result<Value> {
    wait_turn(app, path, turn, |row| {
        matches!(
            row["status"].as_str(),
            Some("done" | "failed" | "cancelled")
        )
    })
    .await
}

async fn resume(app: &Server<'_>, path: &str, turn: &str, revision: &Value) -> Result<()> {
    let resume = json!({"id":id(),"revision":revision,"acknowledgeUncertain":true});
    let endpoint = format!("{path}/{turn}/resume");
    let receipt = post_json(app, &endpoint, &resume, 200).await?;
    assert_eq!(receipt["id"], turn);
    assert_eq!(post_json(app, &endpoint, &resume, 200).await?, receipt);
    Ok(())
}

async fn exercise_provider_stream(family: Family) -> Result<()> {
    let (mode, scenario) = watch::channel(Scenario::Text);
    let state = StreamingProvider {
        family,
        scenario,
        calls: Calls::default(),
        release: Arc::new(Notify::new()),
        wrong_type_closed: Arc::new(Notify::new()),
    };
    let endpoint = match family {
        Family::OpenAi => "/v1/chat/completions",
        Family::Anthropic => "/v1/messages",
    };
    let provider = Loopback::start(
        Router::new()
            .route(endpoint, post(stream_provider))
            .with_state(state.clone()),
    )
    .await?;
    let root = TestRoot::new()?;
    let config = json!({"baseURL":provider.base_url(),"provider":family.name(),"stream":true});
    let mut app = root.start(config.clone()).await?;
    let owner = id();
    post_json(&app, "/api/agents", &json!({"id":owner,"runtime":"api","name":"Stream","model":"primary","backupModel":"backup","instructions":"Help"}), 201).await?;
    let path = format!("/api/agents/{owner}/turns");
    for (scenario, status, num_calls, files) in [
        (Scenario::Text, "done", 1, 0),
        (Scenario::Broken, "failed", 1, 0),
        (Scenario::Tool, "done", 3, 1),
        (Scenario::BrokenTool, "failed", 1, 0),
        (Scenario::WrongType, "failed", 1, 0),
    ] {
        mode.send_replace(scenario);
        let before = state.calls.read()?.len();
        let turn = submit(&app, &path, &format!("{scenario:?}")).await?;
        let row = settled(&app, &path, &turn).await?;
        assert_eq!(row["status"], status, "{scenario:?}: {row}");
        assert_eq!(
            state.calls.read()?.len() - before,
            num_calls,
            "{scenario:?}"
        );
        assert_eq!(
            row["generatedFiles"].as_array().map(Vec::len),
            Some(files),
            "{scenario:?}"
        );
        if scenario == Scenario::Broken {
            assert_eq!(row["reply"], Value::Null);
            assert_eq!(row["partial"]["text"], "Partial ");
        }
        if status == "done" {
            assert_eq!(row["reply"], "Partial complete");
        }
        let usage = get_json(&app, "/api/usage").await?;
        let call = rows(&usage["calls"])?
            .iter()
            .find(|call| call["turn"] == turn)
            .ok_or("missing usage call")?;
        if scenario == Scenario::Text {
            assert_eq!(call["total_tokens"], 9);
        }
        if scenario == Scenario::Broken {
            assert_eq!(call["total_tokens"], Value::Null);
        }
        if scenario == Scenario::WrongType {
            timeout(WAIT, state.wrong_type_closed.notified()).await?;
        }
    }

    mode.send_replace(Scenario::ToolHold);
    let signed = submit(&app, &path, "Resume signed tool").await?;
    let live = wait_turn(&app, &path, &signed, |row| {
        row["generatedFiles"].as_array().map(Vec::len) == Some(1)
            && row["partial"]["text"] == "Partial "
    })
    .await?;
    assert!(app.terminate(Signal::TERM).await?.success());
    mode.send_replace(Scenario::Text);
    app = root.start(config.clone()).await?;
    let restored = wait_status(&app, &path, &signed, "interrupted").await?;
    let before = state.calls.read()?.len();
    resume(&app, &path, &signed, &restored["revision"]).await?;
    let completed = settled(&app, &path, &signed).await?;
    assert_eq!(completed["status"], "done");
    assert_eq!(completed["generatedFiles"], live["generatedFiles"]);
    assert_eq!(state.calls.read()?.len() - before, 2);

    mode.send_replace(Scenario::Hold);
    let cancelled = submit(&app, &path, "Cancel stream").await?;
    wait_turn(&app, &path, &cancelled, |row| {
        row["partial"]["text"] == "Partial "
    })
    .await?;
    post_json(
        &app,
        &format!("{path}/{cancelled}/cancel"),
        &json!({"id":id()}),
        200,
    )
    .await?;
    let row = settled(&app, &path, &cancelled).await?;
    assert_eq!(row["status"], "cancelled");
    assert_eq!(row["partial"]["text"], "Partial ");

    let interrupted = submit(&app, &path, "Restart stream").await?;
    wait_turn(&app, &path, &interrupted, |row| {
        row["partial"]["text"] == "Partial "
    })
    .await?;
    assert!(app.terminate(Signal::TERM).await?.success());
    app = root.start(config.clone()).await?;
    let restored = wait_turn(&app, &path, &interrupted, |row| {
        matches!(row["status"].as_str(), Some("failed" | "interrupted"))
    })
    .await?;
    assert_eq!(restored["partial"]["text"], "Partial ");
    post_json(
        &app,
        &format!("{path}/{interrupted}/resume"),
        &json!({"id":id(),"revision":restored["revision"]}),
        409,
    )
    .await?;
    mode.send_replace(Scenario::Text);
    let before = state.calls.read()?.len();
    resume(&app, &path, &interrupted, &restored["revision"]).await?;
    assert_eq!(
        settled(&app, &path, &interrupted).await?["reply"],
        "Partial complete"
    );
    assert_eq!(state.calls.read()?.len() - before, 1);

    mode.send_replace(Scenario::Hold);
    let turn = submit(&app, &path, "Visible while streaming").await?;
    let live = wait_turn(&app, &path, &turn, |row| {
        row["partial"]["text"] == "Partial "
    })
    .await?;
    assert_eq!(live["status"], "running");
    assert_eq!(live["reply"], Value::Null);
    state.release.notify_one();
    assert_eq!(settled(&app, &path, &turn).await?["reply"], "Partial ");

    if family == Family::OpenAi {
        assert!(app.terminate(Signal::TERM).await?.success());
        let mut compatible = config;
        compatible["streamTermination"] = json!("finish-reason");
        app = root.start(compatible).await?;
        mode.send_replace(Scenario::BrokenTool);
        let before = state.calls.read()?.len();
        let turn = submit(&app, &path, "Tool with finish-reason EOF").await?;
        let result = settled(&app, &path, &turn).await?;
        assert_eq!(result["status"], "done");
        assert_eq!(result["generatedFiles"].as_array().map(Vec::len), Some(1));
        assert_eq!(state.calls.read()?.len() - before, 3);
    }
    for body in state.calls.read()? {
        assert_eq!(body["stream"], true);
        if family == Family::OpenAi {
            assert_eq!(body["stream_options"], json!({"include_usage":true}));
            for call in messages(&body)
                .iter()
                .filter_map(|message| message["tool_calls"].as_array())
                .flatten()
            {
                assert_eq!(
                    call["extra_content"]["google"]["thought_signature"],
                    "stream-signature"
                );
            }
        }
    }
    assert!(app.terminate(Signal::TERM).await?.success());
    Ok(())
}

#[tokio::test]
async fn rejects_invalid_provider_stream_termination() -> Result<()> {
    for (config, diagnostic) in [
        (
            json!({"streamTermination":"unknown"}),
            "MINARBOT_STREAM_TERMINATION",
        ),
        (
            json!({"provider":"anthropic","streamTermination":"finish-reason"}),
            "only supported",
        ),
    ] {
        let root = TestRoot::new()?;
        let error = root
            .start(config)
            .await
            .err()
            .ok_or("invalid stream config started")?;
        assert!(error.to_string().contains(diagnostic), "{error}");
    }
    Ok(())
}
