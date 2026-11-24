#![allow(dead_code)]

use super::{store, Result};
use crate::{data::string, turns::memory::completion_agent, AppState};
use serde_json::Value;

pub async fn call(state: &AppState, turn: &str, messages: &[Value], phase: &str) -> Result<String> {
    let model = {
        let guard = state
            .database
            .lock()
            .map_err(|_| crate::web::Failure::storage())?;
        let agent = completion_agent(&guard.connection, turn, phase)?;
        let configured = string(&agent, "memory_model");
        if configured.is_empty() {
            string(&agent, "model").to_owned()
        } else {
            configured.to_owned()
        }
    };
    store::run(state, turn, &model, messages, phase).await
}
