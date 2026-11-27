use super::*;

pub fn elicitation(params: &Value) -> Value {
    if params["serverName"] != "minarbot" || params["mode"] != "form" {
        return json!({"action":"decline"});
    }
    let once_pattern = regex::Regex::new(
        r"(?i)^(?:once|accept|approve|allow)(?:[_\s-]*(?:once(?:[_\s-]*only)?|this[_\s-]*time))?$",
    );
    let persistent = regex::Regex::new("(?i)persist|always|remember");
    let schema = &params["requestedSchema"];
    let mut content = serde_json::Map::new();
    if let Some(properties) = schema["properties"].as_object() {
        for (key, field) in properties {
            let options: Vec<&str> = if let Some(values) = field["enum"].as_array() {
                values.iter().filter_map(Value::as_str).collect()
            } else {
                field["oneOf"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v["const"].as_str())
                    .collect()
            };
            let once = options
                .into_iter()
                .find(|s| once_pattern.as_ref().is_ok_and(|r| r.is_match(s.trim())));
            if let Some(value) = once {
                content.insert(key.clone(), json!(value));
            } else if field["type"] == "boolean"
                && persistent.as_ref().is_ok_and(|r| r.is_match(key))
            {
                content.insert(key.clone(), json!(false));
            } else if let Some(value) = field.get("default").filter(|v| !v.is_null()) {
                content.insert(key.clone(), value.clone());
            }
        }
    }
    if schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|key| key.as_str().is_none_or(|key| !content.contains_key(key)))
    {
        return json!({"action":"decline"});
    }
    json!({"action":"accept","content":content})
}

pub(crate) fn models(value: &Value) -> Value {
    let mut result = Vec::new();
    let state = value
        .get("models")
        .filter(|v| !v.is_null())
        .unwrap_or(&value["_meta"]["modelState"]);
    for model in state["availableModels"].as_array().into_iter().flatten() {
        let id = string(model, "modelId");
        result.push(json!({"id":id,"label":if string(model,"name").is_empty(){id}else{string(model,"name")}}));
    }
    for option in value["configOptions"].as_array().into_iter().flatten() {
        if string(option, "category") != "model" && string(option, "id") != "model" {
            continue;
        }
        for item in option["options"].as_array().into_iter().flatten() {
            let options = item["options"].as_array();
            for model in options
                .into_iter()
                .flatten()
                .chain(std::iter::once(item).filter(|_| options.is_none()))
            {
                let id = string(model, "value");
                result.push(json!({"id":id,"label":if string(model,"name").is_empty(){id}else{string(model,"name")}}));
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    Value::Array(
        result
            .into_iter()
            .filter(|v| {
                let id = string(v, "id");
                !id.is_empty() && id.encode_utf16().count() <= 256 && seen.insert(id.to_owned())
            })
            .take(256)
            .collect(),
    )
}

pub async fn probe(
    path: &std::path::Path,
    runtime: &str,
    hydrated_path: &str,
) -> Option<(bool, Value, Value)> {
    let home = process::ProbeHome::new().ok()?;
    let mut process = home.launch(
        path,
        &if runtime == "pi" {
            vec![
                "--mode".into(),
                "rpc".into(),
                "--offline".into(),
                "--no-extensions".into(),
                "--no-session".into(),
            ]
        } else {
            store::acp_args(runtime, path)
        },
        runtime,
        hydrated_path,
    )?;
    if runtime == "pi" {
        process
            .send(json!({"id":1,"type":"get_state"}))
            .await
            .ok()?;
        let state = tokio::time::timeout(Duration::from_secs(2), process.next())
            .await
            .ok()?
            .ok()?;
        if state["success"] != true {
            return None;
        }
        process
            .send(json!({"id":2,"type":"get_available_models"}))
            .await
            .ok()?;
        let models = tokio::time::timeout(Duration::from_secs(2), process.next())
            .await
            .ok()?
            .ok()?;
        return Some((true,Value::Null,json!(models["data"]["models"].as_array().into_iter().flatten().map(|m|json!({"id":format!("{}/{}",string(m,"provider"),string(m,"id")),"label":m["name"]})).collect::<Vec<_>>())));
    }
    process.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientInfo":{"name":"minarbot","version":"0.1.0"},"clientCapabilities":{"terminal":false}}})).await.ok()?;
    let budget = if runtime == "hermes" {
        process::HERMES_STARTUP
    } else {
        Duration::from_secs(15)
    };
    let value = tokio::time::timeout(budget, process.next())
        .await
        .ok()?
        .ok()?;
    Some((
        value["result"]["protocolVersion"] == 1,
        if runtime == "hermes" {
            json!(value["result"]["authMethods"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|m| m["type"] == "agent"))
        } else {
            Value::Null
        },
        models(&value["result"]),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn elicitations_only_accept_minarbot_once() {
        let form = json!({"serverName":"minarbot","mode":"form","requestedSchema":{"properties":{"decision":{"enum":["acceptAlways","accept","decline"]},"remember":{"type":"boolean"}},"required":["decision"]}});
        assert_eq!(
            elicitation(&form),
            json!({"action":"accept","content":{"decision":"accept","remember":false}})
        );
        let mut unknown = form.clone();
        unknown["serverName"] = json!("foreign");
        assert_eq!(elicitation(&unknown), json!({"action":"decline"}));
        unknown = form.clone();
        unknown["requestedSchema"]["required"] = json!(["missing"]);
        assert_eq!(elicitation(&unknown), json!({"action":"decline"}));
        let mut empty = form.clone();
        empty["requestedSchema"] = json!({"type":"object","properties":{}});
        assert_eq!(elicitation(&empty), json!({"action":"accept","content":{}}));
        let mut boolean = empty.clone();
        boolean["requestedSchema"]["properties"]["persist"] = json!({"type":"boolean"});
        assert_eq!(
            elicitation(&boolean),
            json!({"action":"accept","content":{"persist":false}})
        );
        empty["mode"] = json!("url");
        empty["url"] = json!("https://example.com");
        assert_eq!(elicitation(&empty), json!({"action":"decline"}));
    }
}
