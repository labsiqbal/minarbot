//! Helpers shared by e2e modules.

use minarbot_testkit::model::{ModelRequest, Reply};

/// A model step after a question means the server ignored the paused turn.
pub fn after_question(request: &ModelRequest) -> bool {
    request
        .messages()
        .last()
        .is_some_and(|message| message["role"] == "tool" && message["content"] == "null")
        && request.called("ask_user")
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
