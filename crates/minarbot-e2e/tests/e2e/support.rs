//! Helpers shared by e2e modules.

use minarbot_testkit::model::{ModelRequest, Reply, ScriptedModel};

/// Product bug B1 (`docs/test-ledger.md`): `ask_user` pauses the turn and
/// returns `null` at once, so the harness takes one more model step after
/// every question. Tests that count model work exclude that step with this
/// predicate; the strict count stays in the retained Node case until B1 is
/// fixed.
pub fn after_question(request: &ModelRequest) -> bool {
    request
        .messages()
        .last()
        .is_some_and(|message| message["role"] == "tool" && message["content"] == "null")
        && request.called("ask_user")
}

/// Model requests that are real work (every request except the B1 step).
pub fn work_calls(model: &ScriptedModel) -> usize {
    model
        .requests()
        .iter()
        .filter(|request| !after_question(request))
        .count()
}

/// Answer for the B1 step: fail it at once, so the harness turn behind the
/// paused Minarbot turn ends immediately. Holding it or answering with text
/// lets that stray step race the server's pause handling.
pub fn after_question_reply() -> Reply {
    Reply::status(
        500,
        serde_json::json!({"error": "no model step after a question (B1)"}),
    )
}
