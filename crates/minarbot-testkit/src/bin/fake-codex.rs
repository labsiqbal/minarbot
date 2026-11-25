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
                json!({"id":id,"result":{"userAgent":"minarbot-testkit-codex","capabilities":{}}}),
            )?,
            Some("thread/start") | Some("thread/resume") => {
                send(json!({"id":id,"result":{"thread":{"id":value["params"]["threadId"]}}}))?
            }
            Some("turn/start") => {
                send(
                    json!({"id":id,"result":{"turn":{"id":"fixture-turn","status":"inProgress"}}}),
                )?;
                let thread = value["params"]["threadId"].clone();
                send(
                    json!({"method":"item/agentMessage/delta","params":{"threadId":thread,"itemId":"fixture-message","delta":"fixture reply"}}),
                )?;
                send(
                    json!({"method":"item/completed","params":{"threadId":value["params"]["threadId"],"item":{"id":"fixture-message","type":"agentMessage","text":"fixture reply"}}}),
                )?;
                send(
                    json!({"method":"turn/completed","params":{"threadId":value["params"]["threadId"],"turn":{"id":"fixture-turn","status":"completed"}}}),
                )?;
            }
            Some("turn/interrupt") => send(json!({"id":id,"result":{}}))?,
            Some(_) if !id.is_null() => send(json!({"id":id,"result":{}}))?,
            _ => {}
        }
    }
    Ok(())
}
