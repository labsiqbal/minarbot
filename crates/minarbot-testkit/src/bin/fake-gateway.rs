use reqwest::Client;
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

async fn completion(
    client: &Client,
    messages: &[Value],
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let base = std::env::var("MINARBOT_GATEWAY_BASE_URL")?;
    let mut request = client.post(format!("{}/chat/completions", base.trim_end_matches('/'))).json(&json!({"model":std::env::var("MINARBOT_GATEWAY_MODEL").unwrap_or_else(|_| "fixture".to_owned()),"messages":messages,"stream":false}));
    if let Ok(key) = std::env::var("MINARBOT_PROVIDER_KEY") {
        request = request.bearer_auth(key);
    }
    Ok(request.send().await?.error_for_status()?.json().await?)
}

async fn tool_call(
    client: &Client,
    name: &str,
    arguments: Value,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let endpoint = std::env::var("MINARBOT_MCP_URL")?;
    let token = std::env::var("MINARBOT_MCP_TOKEN").unwrap_or_default();
    Ok(client.post(endpoint).bearer_auth(token).json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}})).send().await?.error_for_status()?.json().await?)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if std::env::args().any(|arg| arg == "--version") {
        println!("fake-gateway 1.0.0");
        return Ok(());
    }
    let client = Client::new();
    let mut messages = Vec::new();
    for line in io::stdin().lock().lines() {
        let value: Value = serde_json::from_str(&line?)?;
        let id = value.get("id").cloned().unwrap_or(Value::Null);
        match value.get("method").and_then(Value::as_str) {
            Some("initialize") => {
                send(json!({"id":id,"result":{"userAgent":"minarbot-testkit-gateway"}}))?
            }
            Some("thread/start") | Some("thread/resume") => {
                send(json!({"id":id,"result":{"thread":{"id":value["params"]["threadId"]}}}))?
            }
            Some("turn/start") => {
                send(
                    json!({"id":id,"result":{"turn":{"id":"fixture-turn","status":"inProgress"}}}),
                )?;
                let prompt = value["params"]["input"][0]["text"].as_str().unwrap_or("");
                messages.push(json!({"role":"user","content":prompt}));
                let answer = completion(&client, &messages).await?;
                let message = answer["choices"][0]["message"].clone();
                let text = if let Some(calls) = message["tool_calls"].as_array() {
                    messages.push(message.clone());
                    for call in calls {
                        let name = call["function"]["name"].as_str().unwrap_or("");
                        let args = call["function"]["arguments"].as_str().unwrap_or("{}");
                        let args = serde_json::from_str(args).unwrap_or_else(|_| json!({}));
                        let result = tool_call(&client, name, args).await?;
                        messages.push(json!({"role":"tool","tool_call_id":call["id"],"content":result.to_string()}));
                    }
                    completion(&client, &messages).await?["choices"][0]["message"]["content"]
                        .as_str()
                        .unwrap_or("")
                        .to_owned()
                } else {
                    message["content"].as_str().unwrap_or("").to_owned()
                };
                send(
                    json!({"method":"item/agentMessage/delta","params":{"threadId":value["params"]["threadId"],"itemId":"fixture-message","delta":text}}),
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
