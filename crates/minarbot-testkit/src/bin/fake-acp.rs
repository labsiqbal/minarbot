use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

fn send(value: Value) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(
        output,
        "{}",
        serde_json::to_string(&value).map_err(io::Error::other)?
    )?;
    output.flush()
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    for line in io::stdin().lock().lines() {
        let value: Value = serde_json::from_str(&line?)?;
        let id = value.get("id").cloned().unwrap_or(Value::Null);
        match value.get("method").and_then(Value::as_str) {
            Some("initialize") => send(
                json!({"id":id,"result":{"protocolVersion":1,"agentInfo":{"name":"minarbot-testkit","version":"1"},"agentCapabilities":{"loadSession":true}}}),
            )?,
            Some("session/new") | Some("session/load") => send(
                json!({"id":id,"result":{"sessionId":"fixture-session","modes":{"availableModes":[{"id":"default","name":"Default"}]}}}),
            )?,
            Some("session/prompt") => {
                send(json!({"id":id,"result":{"stopReason":"end_turn"}}))?;
                send(
                    json!({"method":"session/update","params":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"fixture reply"}}}),
                )?;
            }
            Some("session/cancel") => send(json!({"id":id,"result":{}}))?,
            Some(_) if !id.is_null() => send(json!({"id":id,"result":{}}))?,
            _ => {}
        }
    }
    Ok(())
}
