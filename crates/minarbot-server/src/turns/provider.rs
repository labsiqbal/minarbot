#![allow(dead_code)]

use super::*;

pub const MALFORMED: &str =
    "The provider returned a malformed response. Check provider compatibility before retrying.";
pub const INCOMPLETE: &str = "Provider did not return a complete text answer. Check model support.";
pub const STREAM_ERROR: &str =
    "The provider stream ended before completion. Check the provider before retrying because the result may be incomplete.";

pub enum Answer {
    Text(String),
    Tool(Value),
}

pub(crate) fn choice(family: &str, value: &Value) -> Result<Value> {
    if family == "gemini" {
        let candidate = value["candidates"]
            .as_array()
            .filter(|candidates| candidates.len() == 1)
            .and_then(|candidates| candidates.first())
            .ok_or_else(|| error(502, MALFORMED))?;
        let parts = candidate["content"]["parts"]
            .as_array()
            .ok_or_else(|| error(502, MALFORMED))?;
        let content = parts
            .iter()
            .filter(|part| part["thought"] != true)
            .filter_map(|part| part["text"].as_str())
            .collect::<String>();
        return Ok(json!({
            "finish_reason": candidate["finishReason"].as_str().unwrap_or("STOP"),
            "message": {"content": content, "tool_calls": []}
        }));
    }
    if family == "anthropic" {
        return Err(error(502, MALFORMED));
    }
    value["choices"]
        .as_array()
        .filter(|choices| choices.len() == 1)
        .and_then(|choices| choices.first())
        .cloned()
        .ok_or_else(|| error(502, MALFORMED))
}

/// Run every model request through its configured provider-instance CLI.
///
/// `Answer::Tool` remains in the public shape for callers that still handle
/// old provider results. CLI harnesses execute tools themselves, so this path
/// always returns text.
pub async fn call(
    state: &AppState,
    turn: &str,
    model: &str,
    messages: &[Value],
    _tools: &[Value],
    phase: &str,
) -> Result<Answer> {
    #[cfg(test)]
    if let Some(base) = state.config.data.base_url.as_deref() {
        return test_fixture_call(state, base, model, messages, _tools).await;
    }
    crate::harness::run(state, turn, model, messages, phase)
        .await
        .map(Answer::Text)
}

#[cfg(test)]
async fn test_fixture_call(
    state: &AppState,
    base: &str,
    model: &str,
    messages: &[Value],
    tools: &[Value],
) -> Result<Answer> {
    let mut messages = messages.to_vec();
    let secrets = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        crate::computer::vault::redactions(state, &guard.connection)?
    };
    for message in &mut messages {
        crate::mcp::secrets::redact_content(message, &secrets);
    }
    let mut body = json!({"model": model, "messages": messages, "stream": false});
    if !tools.is_empty() {
        body["tools"] = json!(tools);
        body["parallel_tool_calls"] = json!(false);
    }
    let response = reqwest::Client::new()
        .post(format!("{}/chat/completions", base.trim_end_matches('/')))
        .json(&body)
        .send()
        .await
        .map_err(|_| error(502, MALFORMED))?;
    let value: Value = response.json().await.map_err(|_| error(502, MALFORMED))?;
    let choice = value["choices"]
        .as_array()
        .and_then(|choices| choices.first())
        .ok_or_else(|| error(502, MALFORMED))?;
    if choice["finish_reason"] == "tool_calls" {
        let call = choice["message"]["tool_calls"]
            .as_array()
            .and_then(|calls| calls.first())
            .ok_or_else(|| error(502, MALFORMED))?;
        let mut call = call.clone();
        crate::mcp::secrets::redact_content(&mut call, &secrets);
        return Ok(Answer::Tool(call));
    }
    let text = choice["message"]["content"]
        .as_str()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| error(502, MALFORMED))?
        .to_owned();
    Ok(Answer::Text(crate::mcp::secrets::redact(
        &text, &secrets, false,
    )))
}
