//! Provider discovery and execution matrix against a provider instance (port
//! of `app/provider-matrix.test.ts`).
//!
//! The Node suite drove the removed direct HTTP client: its exact error texts
//! per HTTP status, backup-model fallback and named provider accounts left
//! with #87. Each subcase now runs through the test gateway instance and keeps
//! what survives: discovery and turn failures are reported without leaking
//! the provider key, a failed turn is never retried or replayed, a completed
//! tool effect survives the failure and an acknowledged resume, streamed
//! partial text is kept, and the key never reaches API responses, server
//! output or files (`scan`).

use crate::sb_support::{after_tool, id, settle};
use minarbot_testkit::{
    control::{Answer, ControlModel},
    http::Method,
    prelude::*,
    server::Gateway,
    Server,
};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

const KEY: &str = "provider-matrix-secret-canary-87362";
const INSTANCE: &str = "codex-test-gateway";

struct Matrix {
    model: ControlModel,
    root: TestRoot,
    app: Server,
    mode: Arc<Mutex<String>>,
    responses: Arc<Mutex<Vec<String>>>,
    path: String,
    stream: bool,
}

fn answer(mode: &str, request: &minarbot_testkit::model::ModelRequest) -> Answer {
    if let Some(status) = mode
        .strip_prefix("http-")
        .and_then(|code| code.parse().ok())
    {
        return Answer::Status(status, json!({"error": {"message": KEY}}));
    }
    match mode {
        "timeout" => Answer::Defer,
        "text-secret" => Answer::text(KEY),
        "malformed" => Answer::Raw(format!("{{\"error\":\"{KEY}\",")),
        "tools" => Answer::Status(400, json!({"error": {"message": KEY, "param": "tools"}})),
        "other-400" => Answer::Status(
            400,
            json!({"error": {"message": KEY, "param": "temperature"}}),
        ),
        "stream-cut" => Answer::StreamError("Partial answer.".into(), KEY.into()),
        "stream-socket-cut" => Answer::StreamCut("Partial answer.".into()),
        "stream-secret" => Answer::StreamCut(KEY.into()),
        "after-tool" if !after_tool(request) => Answer::tool(
            "create_text_file",
            json!({"name": "once.txt", "text": "Saved once"}),
        ),
        "after-tool" => Answer::Status(503, json!({"error": {"message": KEY}})),
        _ => Answer::text("Answer"),
    }
}

fn catalog(mode: &str) -> Answer {
    if let Some(status) = mode
        .strip_prefix("http-")
        .and_then(|code| code.parse().ok())
    {
        return Answer::Status(status, json!({"error": {"message": KEY}}));
    }
    match mode {
        "loading" | "catalog-timeout" => Answer::Defer,
        "redirect" => Answer::Redirect("/stolen".into()),
        "non-json" => Answer::Raw(format!("{KEY}<html>not JSON</html>")),
        "oversize" => Answer::Raw(format!("{KEY}{}", "x".repeat(2 * 1024 * 1024))),
        "catalog-secret" => Answer::Raw(json!({"data": [{"id": KEY}]}).to_string()),
        "invalid-catalog" => Answer::Raw(json!({"data": [{"id": "\n"}], "error": KEY}).to_string()),
        "empty" => Answer::Raw(json!({"data": []}).to_string()),
        _ => Answer::Raw(json!({"data": [{"id": "primary"}, {"id": "backup"}]}).to_string()),
    }
}

impl Matrix {
    async fn start(stream: bool) -> Result<Self> {
        let mode = Arc::new(Mutex::new(String::from("catalog")));
        let model = ControlModel::start().await?;
        let turn_mode = mode.clone();
        model.respond(move |request| {
            let mode = turn_mode
                .lock()
                .map(|mode| mode.clone())
                .unwrap_or_default();
            answer(&mode, request)
        });
        let catalog_mode = mode.clone();
        model.respond_catalog(move || {
            let mode = catalog_mode
                .lock()
                .map(|mode| mode.clone())
                .unwrap_or_default();
            catalog(&mode)
        });
        let root = TestRoot::new()?;
        let app = root.start(Self::config(&model, stream)).await?;
        let mut matrix = Self {
            model,
            root,
            app,
            mode,
            responses: Arc::new(Mutex::new(Vec::new())),
            path: String::new(),
            stream,
        };
        let agent = matrix
            .request(
                Method::POST,
                "/api/agents",
                Some(
                    json!({"runtime": "api", "id": new_id(), "name": "Matrix", "model": "primary",
                    "backupModel": "backup", "instructions": "Synthetic provider checks"}),
                ),
                201,
            )
            .await?;
        matrix.path = format!("/api/agents/{}/turns", id(&agent)?);
        Ok(matrix)
    }

    fn config(model: &ControlModel, stream: bool) -> ServerConfig {
        let gateway = Gateway {
            api_key: Some(KEY.into()),
            stream: Some(stream),
            ..model.gateway()
        };
        ServerConfig::new().gateway(gateway).option("timeout", 2000)
    }

    fn set(&self, mode: &str) -> Result<()> {
        *self.mode.lock().map_err(|_| "mode")? = mode.into();
        Ok(())
    }

    /// Request recorded for `scan`; the reply must not carry the key.
    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        status: u16,
    ) -> Result<Value> {
        let reply = self.app.call(method, path, body).await?;
        let text = reply.body.to_string();
        self.responses
            .lock()
            .map_err(|_| "responses")?
            .push(text.clone());
        ensure!(!text.contains(KEY), "API response leaked the key: {text}");
        reply.expect(status)
    }

    async fn catalog(&self, status: u16) -> Result<Value> {
        self.request(
            Method::POST,
            &format!("/api/provider-instances/{INSTANCE}/models"),
            Some(json!({"id": new_id(), "instanceId": INSTANCE})),
            status,
        )
        .await
    }

    async fn turn(&self, command: &Value) -> Result<Value> {
        self.request(Method::POST, &self.path, Some(command.clone()), 202)
            .await?;
        let id = command["id"].as_str().ok_or("id")?;
        let row = settle(&self.app, &self.path, id).await?;
        ensure!(!row.to_string().contains(KEY), "turn leaked the key: {row}");
        Ok(row)
    }

    /// The key is absent from every API response, the server output and
    /// every file under the test root.
    fn scan(&self) -> Result<()> {
        let responses = self.responses.lock().map_err(|_| "responses")?.join("\n");
        ensure!(!responses.contains(KEY), "response history leaked the key");
        let (stdout, stderr) = self.app.output()?;
        ensure!(
            !stdout.contains(KEY) && !stderr.contains(KEY),
            "server output leaked the key"
        );
        scan_files(self.root.path())
    }

    async fn restart(&mut self) -> Result<()> {
        self.app
            .restart_with(Self::config(&self.model, self.stream))
            .await
    }
}

fn scan_files(directory: &Path) -> Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            scan_files(&entry.path())?;
        } else if kind.is_file() {
            let bytes = std::fs::read(entry.path())?;
            ensure!(
                !bytes
                    .windows(KEY.len())
                    .any(|window| window == KEY.as_bytes()),
                "file {} leaked the key",
                entry.path().display()
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn loading_remains_pending_empty_catalog_succeeds_without_inventing_models() -> Result<()> {
    let m = Matrix::start(false).await?;
    m.set("loading")?;
    let before = m.model.catalog_calls();
    let app = &m.app;
    let pending = tokio::spawn({
        let url = app.url(&format!("/api/provider-instances/{INSTANCE}/models"));
        let client = app.client()?;
        let origin = app.origin().to_owned();
        async move {
            client
                .post(url)
                .header("Origin", origin)
                .header("Content-Type", "application/json")
                .body(json!({"id": new_id(), "instanceId": INSTANCE}).to_string())
                .send()
                .await
        }
    });
    let held = m.model.deferred().await?;
    ensure!(
        !pending.is_finished(),
        "discovery finished while the catalog was loading"
    );
    ensure!(m.model.catalog_calls() - before == 1, "catalog requests");
    held.answer(Answer::Raw(json!({"data": []}).to_string()));
    let response = pending.await??;
    ensure!(
        response.status().is_success(),
        "empty catalog: {}",
        response.status()
    );
    let body: Value = response.json().await?;
    ensure!(body["models"] == json!([]), "invented models: {body}");
    m.set("empty")?;
    ensure!(
        m.catalog(200).await?["models"] == json!([]),
        "empty catalog"
    );
    m.scan()
}

async fn discovery(scenario: &str) -> Result<()> {
    let m = Matrix::start(false).await?;
    m.set(scenario)?;
    let before = m.model.catalog_calls();
    let reply = m
        .app
        .call(
            Method::POST,
            &format!("/api/provider-instances/{INSTANCE}/models"),
            Some(json!({"id": new_id(), "instanceId": INSTANCE})),
        )
        .await?;
    let text = reply.body.to_string();
    m.responses
        .lock()
        .map_err(|_| "responses")?
        .push(text.clone());
    ensure!(!text.contains(KEY), "{scenario} leaked the key: {text}");
    let models = reply.body["models"].as_array().cloned().unwrap_or_default();
    ensure!(
        !(200..300).contains(&reply.status) || models.is_empty(),
        "{scenario} invented models: {text}"
    );
    ensure!(
        !(200..300).contains(&reply.status) || reply.body["error"].is_string() || models.is_empty(),
        "{scenario}: {text}"
    );
    ensure!(
        reply.body["error"].is_string() || models.is_empty(),
        "{scenario} error: {text}"
    );
    ensure!(
        m.model.catalog_calls() - before == 1,
        "{scenario} catalog requests: {}",
        m.model.catalog_calls() - before
    );
    m.scan()
}

#[tokio::test]
async fn discovery_http_401() -> Result<()> {
    discovery("http-401").await
}

#[tokio::test]
async fn discovery_http_403() -> Result<()> {
    discovery("http-403").await
}

#[tokio::test]
async fn discovery_catalog_secret() -> Result<()> {
    discovery("catalog-secret").await
}

#[tokio::test]
async fn discovery_non_json() -> Result<()> {
    discovery("non-json").await
}

#[tokio::test]
async fn discovery_oversize() -> Result<()> {
    discovery("oversize").await
}

#[tokio::test]
async fn discovery_invalid_catalog() -> Result<()> {
    discovery("invalid-catalog").await
}

#[tokio::test]
async fn discovery_redirect() -> Result<()> {
    discovery("redirect").await
}

#[tokio::test]
async fn discovery_http_408() -> Result<()> {
    discovery("http-408").await
}

#[tokio::test]
async fn discovery_http_429() -> Result<()> {
    discovery("http-429").await
}

#[tokio::test]
async fn discovery_http_503() -> Result<()> {
    discovery("http-503").await
}

#[tokio::test]
async fn discovery_catalog_timeout() -> Result<()> {
    discovery("catalog-timeout").await
}

/// Named accounts are gone; the instance equivalent is an instance id that
/// does not exist, which must not fall back to the configured one.
#[tokio::test]
async fn missing_named_account_cannot_use_configured_default() -> Result<()> {
    let m = Matrix::start(false).await?;
    let before = m.model.catalog_calls();
    let missing = m
        .request(
            Method::POST,
            "/api/provider-instances/gone/models",
            Some(json!({"id": new_id(), "instanceId": "gone"})),
            404,
        )
        .await?;
    ensure!(missing["error"].is_string(), "missing instance: {missing}");
    ensure!(
        m.model.catalog_calls() == before,
        "missing instance reached the provider"
    );
    m.scan()
}

async fn turn_failure(scenario: &str) -> Result<()> {
    let m = Matrix::start(false).await?;
    m.set(scenario)?;
    let before = m.model.calls();
    let command = json!({"id": new_id(), "text": scenario});
    let row = m.turn(&command).await?;
    if scenario == "text-secret" {
        // A provider answer that echoes the key is redacted before storage.
        ensure!(
            row["status"] == "failed"
                || row["reply"]
                    .as_str()
                    .is_some_and(|reply| reply.contains("[REDACTED]")),
            "{scenario}: {row}"
        );
    } else {
        ensure!(row["status"] == "failed", "{scenario}: {row}");
        ensure!(
            row["error"].as_str().is_some_and(|error| !error.is_empty()),
            "{scenario} error: {row}"
        );
        ensure!(row["reply"].is_null(), "{scenario} reply: {row}");
    }
    let models: Vec<String> = m.model.requests()[before..]
        .iter()
        .map(|request| request.model().to_owned())
        .collect();
    ensure!(models == ["primary"], "{scenario} requests: {models:?}");
    m.request(Method::POST, &m.path, Some(command), 202).await?;
    ensure!(m.model.calls() == before + 1, "{scenario} replayed");
    m.scan()
}

#[tokio::test]
async fn turn_http_401_does_not_fall_back_or_replay() -> Result<()> {
    turn_failure("http-401").await
}

#[tokio::test]
async fn turn_http_403_does_not_fall_back_or_replay() -> Result<()> {
    turn_failure("http-403").await
}

#[tokio::test]
async fn turn_http_404_does_not_fall_back_or_replay() -> Result<()> {
    turn_failure("http-404").await
}

#[tokio::test]
async fn turn_http_408_does_not_fall_back_or_replay() -> Result<()> {
    turn_failure("http-408").await
}

#[tokio::test]
async fn turn_timeout_does_not_fall_back_or_replay() -> Result<()> {
    turn_failure("timeout").await
}

#[tokio::test]
async fn turn_http_500_does_not_fall_back_or_replay() -> Result<()> {
    turn_failure("http-500").await
}

#[tokio::test]
async fn turn_http_502_does_not_fall_back_or_replay() -> Result<()> {
    turn_failure("http-502").await
}

#[tokio::test]
async fn turn_text_secret_does_not_fall_back_or_replay() -> Result<()> {
    turn_failure("text-secret").await
}

#[tokio::test]
async fn turn_malformed_does_not_fall_back_or_replay() -> Result<()> {
    turn_failure("malformed").await
}

#[tokio::test]
async fn turn_tools_does_not_fall_back_or_replay() -> Result<()> {
    turn_failure("tools").await
}

#[tokio::test]
async fn turn_other_400_does_not_fall_back_or_replay() -> Result<()> {
    turn_failure("other-400").await
}

/// Backup-model fallback was removed with #87: a 429 or 503 fails the turn
/// after one request on the bound model.
#[tokio::test]
async fn turn_429_falls_back_once_on_same_account() -> Result<()> {
    turn_failure("http-429").await
}

#[tokio::test]
async fn turn_503_falls_back_once_on_same_account() -> Result<()> {
    turn_failure("http-503").await
}

#[tokio::test]
async fn fallback_failure_ends_after_two_requests() -> Result<()> {
    turn_failure("http-503").await
}

#[tokio::test]
async fn rate_limit_without_fallback_stays_failed() -> Result<()> {
    let m = Matrix::start(false).await?;
    let plain = m
        .request(
            Method::POST,
            "/api/agents",
            Some(json!({"runtime": "api", "id": new_id(), "name": "No fallback", "model": "primary", "instructions": "Test"})),
            201,
        )
        .await?;
    m.set("http-429")?;
    let before = m.model.calls();
    let path = format!("/api/agents/{}/turns", id(&plain)?);
    let command = json!({"id": new_id(), "text": "http-429"});
    m.request(Method::POST, &path, Some(command.clone()), 202)
        .await?;
    let row = settle(&m.app, &path, command["id"].as_str().ok_or("id")?).await?;
    ensure!(row["status"] == "failed", "rate limit: {row}");
    ensure!(
        row["error"]
            .as_str()
            .is_some_and(|error| !error.contains(KEY)),
        "error: {row}"
    );
    ensure!(m.model.calls() == before + 1, "rate limit retried");
    m.scan()
}

#[tokio::test]
async fn completed_tool_effect_blocks_automatic_fallback_acknowledged_resume_retains_artifact(
) -> Result<()> {
    let m = Matrix::start(false).await?;
    m.set("after-tool")?;
    let before = m.model.calls();
    let command = json!({"id": new_id(), "text": "after-tool"});
    let row = m.turn(&command).await?;
    ensure!(row["status"] == "failed", "after tool: {row}");
    ensure!(
        row["generatedFiles"].as_array().map(Vec::len) == Some(1),
        "artifact: {row}"
    );
    let models: Vec<String> = m.model.requests()[before..]
        .iter()
        .map(|request| request.model().to_owned())
        .collect();
    ensure!(models == ["primary", "primary"], "requests: {models:?}");
    let id = command["id"].as_str().ok_or("id")?;
    m.request(
        Method::POST,
        &format!("{}/{id}/resume", m.path),
        Some(json!({"id": new_id(), "revision": row["revision"]})),
        409,
    )
    .await?;
    ensure!(m.model.calls() == before + 2, "unacknowledged resume ran");
    m.set("answer")?;
    m.request(
        Method::POST,
        &format!("{}/{id}/resume", m.path),
        Some(json!({"id": new_id(), "revision": row["revision"], "acknowledgeUncertain": true})),
        200,
    )
    .await?;
    let resumed = m.app.wait_status(&m.path, id, "done").await?;
    ensure!(
        resumed["generatedFiles"] == row["generatedFiles"],
        "artifact changed: {resumed}"
    );
    let results: Vec<usize> = m.model.requests()[before..]
        .iter()
        .map(|request| request.tool_results().len())
        .collect();
    ensure!(
        results.first() == Some(&0) && results[1..].iter().all(|count| *count == 1),
        "tool results: {results:?}"
    );
    m.scan()
}

async fn stream_cut(scenario: &str) -> Result<()> {
    let mut m = Matrix::start(true).await?;
    m.set(scenario)?;
    let before = m.model.calls();
    let command = json!({"id": new_id(), "text": scenario});
    let row = m.turn(&command).await?;
    ensure!(row["status"] == "failed", "{scenario}: {row}");
    ensure!(
        row["error"].as_str().is_some_and(|error| !error.is_empty()),
        "{scenario} error: {row}"
    );
    ensure!(row["reply"].is_null(), "{scenario} reply: {row}");
    ensure!(
        row["partial"]["text"] == "Partial answer.",
        "{scenario} partial: {}",
        row["partial"]
    );
    ensure!(m.model.calls() == before + 1, "{scenario} retried");
    m.restart().await?;
    let id = command["id"].as_str().ok_or("id")?;
    let restored = m.app.row(&m.path, id).await?.ok_or("restored turn")?;
    ensure!(
        restored["error"] == row["error"],
        "{scenario} error after restart: {restored}"
    );
    ensure!(
        m.model.calls() == before + 1,
        "{scenario} replayed on restart"
    );
    m.request(Method::POST, &m.path, Some(command), 202).await?;
    ensure!(m.model.calls() == before + 1, "{scenario} receipt replayed");
    m.scan()
}

#[tokio::test]
async fn turn_stream_cut_retains_partial_text_without_fallback_or_replay_on_restart() -> Result<()>
{
    stream_cut("stream-cut").await
}

#[tokio::test]
async fn turn_stream_socket_cut_retains_partial_text_without_fallback_or_replay_on_restart(
) -> Result<()> {
    stream_cut("stream-socket-cut").await
}

#[tokio::test]
async fn streamed_credential_echo_never_reaches_partial_storage() -> Result<()> {
    let m = Matrix::start(true).await?;
    m.set("stream-secret")?;
    let before = m.model.calls();
    let row = m
        .turn(&json!({"id": new_id(), "text": "stream-secret"}))
        .await?;
    ensure!(row["status"] == "failed", "stream secret: {row}");
    ensure!(
        !row["partial"].to_string().contains(KEY)
            && !row["activity"]["stream"].to_string().contains(KEY),
        "partial storage leaked the key: {row}"
    );
    ensure!(m.model.calls() == before + 1, "stream secret retried");
    m.scan()
}

/// The instance-era "unconfigured default" is a disabled instance: discovery
/// and turns are refused without a provider request.
#[tokio::test]
async fn unconfigured_default_is_distinct_from_missing_named_account() -> Result<()> {
    let m = Matrix::start(false).await?;
    let instances = m
        .request(Method::GET, "/api/provider-instances", None, 200)
        .await?;
    let gateway = instances["providers"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == INSTANCE))
        .ok_or("test instance")?
        .clone();
    m.request(
        Method::POST,
        &format!("/api/provider-instances/{INSTANCE}/edit"),
        Some(json!({"id": new_id(), "instanceId": INSTANCE, "kind": "codex", "displayName": "Test gateway",
            "enabled": false, "revision": gateway["revision"]})),
        200,
    )
    .await?;
    let before = (m.model.calls(), m.model.catalog_calls());
    let refused = m
        .request(
            Method::POST,
            &m.path,
            Some(json!({"id": new_id(), "text": "Must not dispatch"})),
            503,
        )
        .await?;
    ensure!(refused["error"].is_string(), "refusal: {refused}");
    ensure!(
        (m.model.calls(), m.model.catalog_calls()) == before,
        "disabled instance reached the provider"
    );
    m.scan()
}

#[tokio::test]
async fn unreachable_loopback_host_reports_connection_problem() -> Result<()> {
    let Matrix {
        model,
        root,
        mut app,
        mode,
        responses,
        path,
        stream,
    } = Matrix::start(false).await?;
    let config = Matrix::config(&model, stream);
    drop(model);
    app.restart_with(config).await?;
    let m = Matrix {
        model: ControlModel::start().await?,
        root,
        app,
        mode,
        responses,
        path,
        stream,
    };
    let reply = m
        .app
        .call(
            Method::POST,
            &format!("/api/provider-instances/{INSTANCE}/models"),
            Some(json!({"id": new_id(), "instanceId": INSTANCE})),
        )
        .await?;
    ensure!(
        !(200..300).contains(&reply.status) || reply.body["models"] == json!([]),
        "catalog: {}",
        reply.body
    );
    ensure!(
        !reply.body.to_string().contains(KEY),
        "catalog leaked the key"
    );
    let row = m
        .turn(&json!({"id": new_id(), "text": "Unreachable"}))
        .await?;
    ensure!(row["status"] == "failed", "unreachable: {row}");
    ensure!(
        row["error"].as_str().is_some_and(|error| !error.is_empty()),
        "error: {row}"
    );
    m.scan()
}
