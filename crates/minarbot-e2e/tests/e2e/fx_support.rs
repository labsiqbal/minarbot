//! Helpers shared by the `fx_*` modules (ports of the fixture-slice Node
//! suites, `docs/test-ledger.md`).

use minarbot_testkit::{gated::GatedModel, http::Method, prelude::*, server::Gateway, Server};

/// Server config whose gateway peer asks `model` for every step.
pub fn gated(model: &GatedModel) -> ServerConfig {
    ServerConfig::new().gateway(Gateway::new(model.base_url()))
}

/// String field of a JSON value, or an error naming the field.
pub fn text(value: &Value, field: &str) -> Result<String> {
    value[field]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("missing {field} in {value}").into())
}

/// `id` of a created record.
pub fn id(value: &Value) -> Result<String> {
    text(value, "id")
}

/// `POST /api/agents` with the test-mode `runtime: "api"` shortcut, which
/// binds the agent to the gateway peer and so to the configured model.
pub async fn api_agent(app: &Server, name: &str, model: &str) -> Result<Value> {
    app.post_expect(
        "/api/agents",
        json!({"runtime": "api", "id": new_id(), "name": name, "model": model, "instructions": "Help"}),
        201,
    )
    .await
}

/// Rows of a list endpoint as an array.
pub async fn rows(app: &Server, path: &str) -> Result<Vec<Value>> {
    let value = app.get(path).await?;
    value
        .as_array()
        .cloned()
        .ok_or_else(|| format!("GET {path} is not an array: {value}").into())
}

/// The row with `id` in `path`.
pub async fn row(app: &Server, path: &str, id: &str) -> Result<Value> {
    app.row(path, id)
        .await?
        .ok_or_else(|| format!("row {id} missing from {path}").into())
}

/// Wait until the row with `id` in `path` reaches `status`.
pub async fn settle(app: &Server, path: &str, id: &str, status: &str) -> Result<Value> {
    app.wait_status(path, id, status).await
}

/// Upload raw bytes as a conversation file (`POST <scope>/files/<id>`).
pub async fn upload(
    app: &Server,
    path: &str,
    name: &str,
    bytes: impl Into<Vec<u8>>,
) -> Result<u16> {
    let response = app
        .request(Method::POST, path)?
        .header("Content-Type", "application/octet-stream")
        .header("X-File-Name", name)
        .body(bytes.into())
        .send()
        .await?;
    Ok(response.status().as_u16())
}

/// Raw GET returning status and body bytes.
pub async fn fetch(app: &Server, path: &str) -> Result<(u16, Vec<u8>)> {
    let response = app.request(Method::GET, path)?.send().await?;
    let status = response.status().as_u16();
    Ok((status, response.bytes().await?.to_vec()))
}

/// JSON body of a tool result message (`role: tool`).
pub fn tool_json(message: &Value) -> Value {
    message["content"]
        .as_str()
        .and_then(|content| serde_json::from_str(content).ok())
        .unwrap_or(Value::Null)
}

/// B1 (`docs/test-ledger.md`): `ask_user` and `wait_for_event` pause the
/// turn and return `null` at once, so the harness takes one more model step
/// right after them. Tests answer that step with [`pause_reply`] and leave it
/// out of model-call counts; the strict counts stay in the kept Node case.
pub fn after_pause(request: &minarbot_testkit::model::ModelRequest) -> bool {
    paused_by(request).is_some()
}

fn paused_by(request: &minarbot_testkit::model::ModelRequest) -> Option<String> {
    let last_tool = request
        .tool_calls()
        .last()
        .and_then(|call| call["function"]["name"].as_str().map(str::to_owned))?;
    let null_result = request
        .messages()
        .last()
        .is_some_and(|message| message["role"] == "tool" && message["content"] == "null");
    (null_result && matches!(last_tool.as_str(), "ask_user" | "wait_for_event"))
        .then_some(last_tool)
}

/// Answer for the B1 step. After `ask_user` it fails at once (the server
/// already stopped that turn for the question). After `wait_for_event` it
/// ends the harness turn with text: a failed harness turn would fail the
/// waiting Minarbot turn, while a finished one leaves it waiting.
pub fn pause_reply_for(
    request: &minarbot_testkit::model::ModelRequest,
) -> minarbot_testkit::gated::Answer {
    match paused_by(request).as_deref() {
        Some("wait_for_event") => minarbot_testkit::gated::Answer::text("Waiting for the event."),
        _ => pause_reply(),
    }
}

/// Failing answer for the B1 step after `ask_user`.
pub fn pause_reply() -> minarbot_testkit::gated::Answer {
    minarbot_testkit::gated::Answer::Status(
        500,
        json!({"error": "no model step after a pausing tool (B1)"}),
    )
}

/// Model requests that are real work (every request except B1 steps).
pub fn work_calls(model: &GatedModel) -> Vec<minarbot_testkit::model::ModelRequest> {
    model
        .requests()
        .into_iter()
        .filter(|request| !after_pause(request))
        .collect()
}

/// Statuses of every row, for waits whose timeout should say what is left.
pub async fn statuses(app: &Server, path: &str) -> Result<Vec<(String, String)>> {
    Ok(rows(app, path)
        .await?
        .iter()
        .map(|row| {
            (
                row["text"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(40)
                    .collect(),
                row["status"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect())
}

/// Whether a status is terminal.
pub fn terminal(status: &str) -> bool {
    ["done", "failed", "cancelled", "interrupted"].contains(&status)
}
