use super::*;
use crate::{data::providers::format_timeout, harness::model_catalog};
use reqwest::Response;
use std::{future::Future, time::Duration};

pub const MALFORMED: &str =
    "The provider returned a malformed response. Check provider compatibility before retrying.";
pub const INCOMPLETE: &str = "Provider did not return a complete text answer. Check model support.";
pub const STREAM_ERROR:&str="The provider stream ended before completion. Check the provider before retrying because the result may be incomplete.";
pub enum Answer {
    Text(String),
    Tool(Value),
}
pub async fn call(
    state: &AppState,
    turn: &str,
    model: &str,
    messages: &[Value],
    tools: &[Value],
    phase: &str,
) -> Result<Answer> {
    let memory = phase.starts_with("memory_");
    let foreground_memory = phase == "memory_reflect";
    let private = memory || phase == "compaction" || phase == goals::JUDGE;
    let allowance = if memory {
        None
    } else {
        goals::allowance(state, turn, messages)?
    };
    let hub_turn = crate::computer::hub::check(state, turn)?.is_some();
    if memory && (hub_turn || pending(state)) {
        return Err(error(409, "Memory provider is unavailable."));
    }
    let native = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        one(
            &guard.connection,
            "SELECT a.runtime,a.provider_account FROM turns t JOIN agents a ON a.id=t.agent
             WHERE t.id=? AND a.runtime<>'api'",
            [turn],
        )?
    };
    if native.is_some() && hub_turn {
        return Err(error(
            403,
            "This runtime cannot fence every native effect for hub turns. Use an API runtime.",
        ));
    }
    if let Some(agent) = native.as_ref().filter(|_| memory) {
        let runtime = string(agent, "runtime");
        let binding = string(agent, "provider_account");
        if crate::data::logins::device_runtime(runtime)
            && (binding == crate::data::logins::DEVICE
                || binding.starts_with(crate::data::logins::PREFIX))
        {
            return crate::harness::memory::call(state, turn, messages, phase)
                .await
                .map(Answer::Text);
        }
        if !crate::data::logins::device_runtime(runtime) {
            let model = {
                let db = state.database.lock().map_err(|_| Failure::storage())?;
                let agent = super::memory::completion_agent(&db.connection, turn, phase)?;
                string(&agent, "learning_model").to_owned()
            };
            return Box::pin(crate::harness::run(state, turn, &model, messages, phase))
                .await
                .map(Answer::Text);
        }
    }
    if native.is_some() && !memory {
        state.turns.mark_started(turn);
        return crate::harness::run(state, turn, model, messages, phase)
            .await
            .map(Answer::Text);
    }
    if pending(state) {
        return Err(error(503, "Update trial is pending."));
    }
    let memory_selection = if memory {
        let (agent, account) = {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            let db = &guard.connection;
            let agent = super::memory::completion_agent(db, turn, phase)?;
            let account = admission::account(state, db, string(&agent, "id"))?;
            (agent, account)
        };
        let configured = string(&agent, "memory_model");
        let selected = if !configured.is_empty() {
            if configured == "default" {
                account.model.clone()
            } else {
                configured.to_owned()
            }
        } else {
            let main = string(&agent, "model");
            let default = if main.is_empty() || main == "default" {
                &account.model
            } else {
                main
            };
            model_catalog::api_models(state, &account)
                .await
                .ok()
                .and_then(|catalog| model_catalog::memory_default(&catalog, default))
                .unwrap_or_else(|| default.to_owned())
        };
        Some((agent["revision"].clone(), account, selected))
    } else {
        None
    };
    let model = memory_selection
        .as_ref()
        .map_or(model, |(_, _, model)| model.as_str());
    let mut messages = messages.to_vec();
    let (account, seq, vault_secrets) = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        let row = one(
            db,
            "SELECT agent,status,paused FROM turns WHERE id=?",
            [turn],
        )?
        .filter(|r| {
            if memory && !foreground_memory {
                r["status"] == "done"
            } else {
                (r["status"] == "running" || r["status"] == "queued") && r["paused"] == 0
            }
        })
        .ok_or_else(|| error(409, "Task has stopped."))?;
        let account = admission::account(state, db, string(&row, "agent"))?;
        if let Some((revision, discovered_account, _)) = &memory_selection {
            let current = super::memory::completion_agent(db, turn, phase)?;
            if pending(state) || &account != discovered_account || &current["revision"] != revision
            {
                return Err(error(409, "Conversation learning settings changed."));
            }
        }
        if !private
            && phase != "routing"
            && !super::profile::setup(db, &json!({"id":turn,"agent":row["agent"]}))?
            && super::messaging::allowed(db, turn)?
        {
            let turn_row =
                one(db, "SELECT * FROM turns WHERE id=?", [turn])?.ok_or_else(Failure::storage)?;
            if let Some(first) = messages.first_mut().filter(|m| m["role"] == "system") {
                first["content"] = json!(format!(
                    "{}\n\n{}",
                    string(first, "content"),
                    super::messaging::awareness(db, &turn_row)?
                ));
            }
        }

        let mut vault_secrets = crate::computer::vault::redactions(state, db)?;
        if memory {
            vault_secrets.extend(account.key.iter().cloned());
        }
        for message in &mut messages {
            crate::mcp::secrets::redact_content(message, &vault_secrets);
        }
        let origin = url::Url::parse(&account.base)
            .map_err(|_| error(502, MALFORMED))?
            .origin()
            .ascii_serialization();
        let purpose = if phase == "routing" {
            "routing"
        } else {
            "completion"
        };
        db.execute("INSERT INTO provider_calls(turn,agent,model,origin,purpose,phase,family,account,status,created) VALUES (?,?,?,?,?,?,?,?, 'pending',?)",params![turn,row["agent"].as_str(),model,origin,purpose,phase,account.family,account.id,iso(db,state.config.now())?])?;
        let seq = db.last_insert_rowid();
        if !memory {
            state.events.turn(db, turn)?;
        }
        (account, seq, vault_secrets)
    };
    let streamed = account.stream;
    let max_tokens = if memory {
        account
            .max_tokens
            .min(if matches!(phase, "memory_reflect" | "memory_refresh") {
                1024
            } else {
                4096
            })
    } else {
        allowance.map_or(account.max_tokens, |(_, limit)| {
            account.max_tokens.min(limit as u64)
        })
    };
    let endpoint = match account.family.as_str() {
        "anthropic" => format!("{}/messages", account.base.trim_end_matches('/')),
        "gemini" => {
            let id = gemini_model(model)?;
            format!(
                "{}/models/{}:{}{}",
                account.base.trim_end_matches('/'),
                id,
                if streamed {
                    "streamGenerateContent"
                } else {
                    "generateContent"
                },
                if streamed { "?alt=sse" } else { "" }
            )
        }
        _ => format!("{}/chat/completions", account.base.trim_end_matches('/')),
    };
    let mut body = translate(
        &account.family,
        model,
        &messages,
        tools,
        max_tokens,
        streamed,
    )?;
    if memory && account.family == "openai-compatible" {
        let limit = if model_catalog::api_memory_default(&account).is_some() {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        body[limit] = json!(max_tokens);
    }
    let mut request = state.turns.client.post(endpoint).json(&body);
    if account.family == "anthropic" {
        request = request.header("anthropic-version", "2023-06-01");
    }
    if let Some(key) = &account.key {
        request = match account.family.as_str() {
            "anthropic" => request.header("x-api-key", key),
            "gemini" => request.header("x-goog-api-key", key),
            _ => request.bearer_auth(key),
        };
    }
    let effect = if memory {
        None
    } else {
        crate::computer::hub::before_effect(state, turn, "provider", json!({"model":model,"purpose":if phase=="routing"{"routing"}else{"completion"},"phase":phase})).await?
    };
    let mut tokens = [None; 3];
    let limit = state.config.data.turn_idle_timeout();
    let explicit_timeout = if memory {
        Some(Duration::from_millis(
            state.config.data.timeout.unwrap_or(90_000).min(90_000),
        ))
    } else {
        state.config.data.timeout.map(|ms| {
            Duration::from_millis(if phase == "compaction" {
                ms.min(15000)
            } else {
                ms
            })
        })
    };
    let stream_idle = (streamed && explicit_timeout.is_none()).then_some(limit);
    let operation = async {
        if !memory {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            if pending(state) || guard.connection.execute("UPDATE turns SET status='running',started_at=coalesce(started_at,?) WHERE id=? AND status IN('queued','running') AND paused=0",params![iso(&guard.connection,state.config.now())?,turn])? == 0 {
                return Err(error(409,"Task has stopped."));
            }
            state.events.turn(&guard.connection, turn)?;
            state.turns.mark_started(turn);
        }
        let response = wait_for_stream_data(stream_idle, request.send()).await?
        .map_err(|_| error(502, "The provider could not be reached. Check the provider URL and network before retrying; completion may be uncertain."))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let unsupported = if matches!(status, 400 | 422) && !tools.is_empty() {
                let bytes = wait_for_stream_data(stream_idle, bounded(response, 8192))
                    .await?
                    .unwrap_or_default();
                let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                v["error"]["param"] == "tools"
                    || matches!(
                        v["error"]["code"].as_str(),
                        Some("unsupported_tools" | "tool_use_not_supported")
                    )
                    || v["error"]["message"].as_str().is_some_and(|s| {
                        let s = s.to_ascii_lowercase();
                        (s.contains("tool") || s.contains("function calling"))
                            && (s.contains("not supported")
                                || s.contains("unsupported")
                                || s.contains("not allowed"))
                    })
            } else {
                false
            };
            let mut e = error(502, provider_error(status, unsupported));
            if matches!(status, 429 | 503) || account.family == "anthropic" && status == 529 {
                e.code = "provider_unavailable";
            }
            return Err(e);
        }
        if streamed
            && !response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| {
                    v.to_ascii_lowercase().split(';').next() == Some("text/event-stream")
                })
        {
            return Err(error(502,"The provider did not send the requested SSE stream. Check provider streaming support."));
        }
        let v = if streamed {
            stream::read(
                response,
                &account.family,
                &account.termination,
                stream_idle,
                |text| {
                    if account
                        .key
                        .as_ref()
                        .is_some_and(|k| !k.is_empty() && text.contains(k))
                    {
                        return Err(error(502, MALFORMED));
                    }
                    if private {
                        Ok(())
                    } else {
                        let text = crate::mcp::secrets::redact(text, &vault_secrets, true);
                        stream::text(state, seq, &text, false)
                    }
                },
            )
            .await?
        } else {
            let bytes = bounded(response, 2 * 1024 * 1024).await?;
            serde_json::from_slice::<Value>(&bytes).map_err(|_| error(502, MALFORMED))?
        };
        if account
            .key
            .as_ref()
            .filter(|k| !k.is_empty())
            .is_some_and(|k| {
                let k = json!(k).to_string();
                v.to_string().contains(&k[1..k.len() - 1])
            })
        {
            return Err(error(502, MALFORMED));
        }
        tokens = usage(&account.family, &v);
        let choice = choice(&account.family, &v)?;
        if choice["finish_reason"] == "tool_calls" {
            let calls = choice["message"]["tool_calls"]
                .as_array()
                .filter(|v| v.len() == 1)
                .ok_or_else(|| error(502, "Invalid coordination request."))?;
            let call = &calls[0];
            if call["type"] != "function"
                || call["id"]
                    .as_str()
                    .is_none_or(|s| s.is_empty() || s.encode_utf16().count() > 200)
                || !tools
                    .iter()
                    .any(|t| t["function"]["name"] == call["function"]["name"])
                || call["function"]["arguments"]
                    .as_str()
                    .is_none_or(|s| s.encode_utf16().count() > 20000)
            {
                return Err(error(502, "Invalid coordination request."));
            }
            stream::tool(
                state,
                seq,
                string(call, "id"),
                string(&call["function"], "name"),
            )?;
            let mut call = call.clone();
            if !vault_secrets.is_empty() {
                let raw = string(&call["function"], "arguments");
                let mut args: Value =
                    serde_json::from_str(raw).map_err(|_| error(502, MALFORMED))?;
                crate::mcp::secrets::redact_content(&mut args, &vault_secrets);
                call["function"]["arguments"] = json!(args.to_string());
            }
            return Ok(Answer::Tool(call));
        }
        let text = choice["message"]["content"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| error(502, INCOMPLETE))?;
        if choice["finish_reason"] != "stop"
            || choice["message"]["tool_calls"]
                .as_array()
                .is_some_and(|a| !a.is_empty())
        {
            return Err(error(502, INCOMPLETE));
        }
        let text = crate::mcp::secrets::redact(text, &vault_secrets, false);
        if !private {
            stream::text(state, seq, &text, true)?;
        }
        Ok(Answer::Text(text))
    };
    let operation = crate::computer::hub::run_effect(&effect, operation);
    let result = if let Some(timeout) = explicit_timeout {
        tokio::time::timeout(timeout, operation)
            .await
            .unwrap_or_else(|_| Err(error(502, provider_error(408, false))))
    } else if streamed {
        operation.await
    } else {
        tokio::time::timeout(limit, operation)
            .await
            .unwrap_or_else(|_| Err(timeout_failure(limit, false)))
    };
    crate::computer::hub::finish_effect(
        state,
        effect,
        if result.is_ok() {
            "succeeded"
        } else {
            "failed"
        },
    )
    .await?;
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    db.execute("UPDATE provider_calls SET status=?,finished=?,input_tokens=?,output_tokens=?,total_tokens=? WHERE seq=? AND status='pending'",params![if result.is_ok(){"received"}else{"failed"},iso(db,state.config.now())?,tokens[0],tokens[1],tokens[2],seq])?;
    if let Some((estimated_input, _)) = allowance {
        let output = match &result {
            Ok(Answer::Text(text)) => text.len(),
            Ok(Answer::Tool(call)) => call.to_string().len(),
            Err(_) => 0,
        };
        goals::account_unmetered(db, turn, seq, estimated_input + (output as i64 + 3) / 4)?;
    }
    result
}
pub(super) async fn wait_for_stream_data<T>(
    idle: Option<Duration>,
    operation: impl Future<Output = T>,
) -> Result<T> {
    if let Some(limit) = idle {
        tokio::time::timeout(limit, operation)
            .await
            .map_err(|_| timeout_failure(limit, true))
    } else {
        Ok(operation.await)
    }
}

fn timeout_failure(limit: Duration, streamed: bool) -> Failure {
    let duration = format_timeout(limit);
    let reason = if streamed {
        format!("No activity for {duration}.")
    } else {
        format!("Reached the {duration} provider request limit.")
    };
    error(
        502,
        format!("{reason} Check the provider before retrying. Completion may be uncertain."),
    )
}

async fn bounded(mut r: Response, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    while let Some(chunk) = r.chunk().await.map_err(|_| error(502, MALFORMED))? {
        if bytes.len() + chunk.len() > limit {
            return Err(error(413,"The provider returned an oversized response. Check provider compatibility and response limits."));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn provider_error(status: u16, unsupported: bool) -> String {
    match status{401=>"The provider rejected the API key. Check the server's provider configuration.".into(),403=>"The provider denied this request. Check the API key permissions and model access.".into(),404=>"The provider does not know this model. Check the model ID or choose one from model discovery.".into(),408=>"The provider request timed out. Check the provider before retrying because completion may be uncertain.".into(),429=>"The provider rate limit was reached (HTTP 429). Wait for quota to reset or choose another model.".into(),_ if unsupported=>"The provider rejected the requested tools. Choose a model and provider that support tools.".into(),500..=599=>format!("The provider failed while processing the request (HTTP {status}). Check provider status and retry later."),_=>format!("The provider rejected the request (HTTP {status}). Check model and provider compatibility.")}
}
pub(crate) fn translate(
    family: &str,
    model: &str,
    messages: &[Value],
    tools: &[Value],
    max: u64,
    stream: bool,
) -> Result<Value> {
    if family == "openai-compatible" {
        let mut clean = messages.to_vec();
        for message in &mut clean {
            if let Some(calls) = message["tool_calls"].as_array_mut() {
                for call in calls {
                    if let Some(object) = call.as_object_mut() {
                        object.remove("geminiParts");
                    }
                }
            }
        }
        let mut v = json!({"model":model,"stream":stream,"messages":clean});
        if stream {
            v["stream_options"] = json!({"include_usage":true});
        }
        if !tools.is_empty() {
            v["parallel_tool_calls"] = json!(false);
            v["tools"] = json!(tools);
        }
        return Ok(v);
    }
    let mut system = Vec::new();
    let mut rows: Vec<Value> = Vec::new();
    let mut calls: HashMap<String, Value> = HashMap::new();
    for m in messages {
        let role = string(m, "role");
        if role == "system" {
            system.push(if family == "anthropic" {
                json!({"type":"text","text":m["content"]})
            } else {
                json!({"text":m["content"]})
            });
            continue;
        }
        let native_role = if role == "assistant" {
            if family == "anthropic" {
                "assistant"
            } else {
                "model"
            }
        } else {
            "user"
        };
        let mut content = Vec::new();
        if role == "tool" {
            if family == "anthropic" {
                content.push(json!({"type":"tool_result","tool_use_id":m["tool_call_id"],"content":m["content"]}));
            } else {
                let call = calls
                    .get(string(m, "tool_call_id"))
                    .ok_or_else(|| error(502, MALFORMED))?;
                let mut response =
                    json!({"name":call["function"]["name"],"response":{"result":m["content"]}});
                if let Some(id) = call["geminiParts"]
                    .as_array()
                    .and_then(|p| p.iter().find_map(|v| v["functionCall"]["id"].as_str()))
                {
                    response["id"] = json!(id);
                }
                content.push(json!({"functionResponse":response}));
            }
        } else if let Some(tc) = m["tool_calls"].as_array() {
            for call in tc {
                calls.insert(string(call, "id").to_owned(), call.clone());
                let args: Value = serde_json::from_str(string(&call["function"], "arguments"))
                    .map_err(|_| error(502, MALFORMED))?;
                if family == "anthropic" {
                    content.push(json!({"type":"tool_use","id":call["id"],"name":call["function"]["name"],"input":args}));
                } else if let Some(parts) = call["geminiParts"]
                    .as_array()
                    .filter(|parts| !parts.is_empty())
                {
                    content.extend(parts.clone());
                } else {
                    content.push(json!({"functionCall":{"name":call["function"]["name"],"args":args},"thoughtSignature":"context_engineering_is_the_way to_go"}));
                }
            }
        } else if let Some(parts) = m["content"].as_array() {
            for p in parts {
                if p["type"] == "text" {
                    content.push(if family == "anthropic" {
                        json!({"type":"text","text":p["text"]})
                    } else {
                        json!({"text":p["text"]})
                    });
                } else {
                    let image = p["image_url"]["url"]
                        .as_str()
                        .and_then(|s| s.strip_prefix("data:"))
                        .and_then(|s| s.split_once(";base64,"))
                        .filter(|(mime, data)| {
                            let payload = data.trim_end_matches('=');
                            role == "user"
                                && p["type"] == "image_url"
                                && ["image/png", "image/jpeg", "image/webp"].contains(mime)
                                && !payload.is_empty()
                                && data.len() - payload.len() <= 2
                                && payload.bytes().all(|byte| {
                                    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/')
                                })
                        })
                        .ok_or_else(|| error(502, MALFORMED))?;
                    content.push(if family=="anthropic"{json!({"type":"image","source":{"type":"base64","media_type":image.0,"data":image.1}})}else{json!({"inlineData":{"mimeType":image.0,"data":image.1}})});
                }
            }
        } else if let Some(text) = m["content"].as_str().filter(|s| !s.is_empty()) {
            content.push(if family == "anthropic" {
                json!({"type":"text","text":text})
            } else {
                json!({"text":text})
            });
        }
        if content.is_empty() {
            if role == "assistant" {
                continue;
            }
            return Err(error(502, MALFORMED));
        }
        if family == "anthropic" {
            if let Some(prev) = rows.last_mut().filter(|p| p["role"] == native_role) {
                prev["content"]
                    .as_array_mut()
                    .ok_or_else(Failure::storage)?
                    .extend(content);
            } else {
                rows.push(json!({"role":native_role,"content":content}));
            }
        } else {
            rows.push(json!({"role":native_role,"parts":content}));
        }
    }
    if family == "anthropic" {
        let mut v =
            json!({"model":model,"max_tokens":max,"stream":stream,"system":system,"messages":rows});
        if !tools.is_empty() {
            v["tools"]=json!(tools.iter().map(|t|json!({"name":t["function"]["name"],"description":t["function"]["description"],"input_schema":t["function"]["parameters"]})).collect::<Vec<_>>());
            v["tool_choice"] = json!({"type":"auto","disable_parallel_tool_use":true});
        }
        Ok(v)
    } else {
        let mut v =
            json!({"contents":rows,"generationConfig":{"maxOutputTokens":max,"candidateCount":1}});
        if !system.is_empty() {
            v["systemInstruction"] = json!({"parts":system});
        }
        if !tools.is_empty() {
            v["tools"] = json!([{"functionDeclarations":tools.iter().map(|t|json!({"name":t["function"]["name"],"description":t["function"]["description"],"parametersJsonSchema":t["function"]["parameters"]})).collect::<Vec<_>>()}]);
        }
        Ok(v)
    }
}
pub(crate) fn gemini_model(model: &str) -> Result<&str> {
    let id = model.strip_prefix("models/").unwrap_or(model);
    if !id.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        || id.len() > 151
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(error(502, MALFORMED));
    }
    Ok(id)
}

pub fn choice(family: &str, v: &Value) -> Result<Value> {
    if family == "openai-compatible" {
        return v["choices"][0]
            .as_object()
            .map(|_| v["choices"][0].clone())
            .ok_or_else(|| error(502, INCOMPLETE));
    }
    let mut text = Vec::new();
    let mut calls = Vec::new();
    if family == "anthropic" {
        if v["type"] != "message" || v["role"] != "assistant" {
            return Err(error(502, MALFORMED));
        }
        for b in v["content"]
            .as_array()
            .ok_or_else(|| error(502, MALFORMED))?
        {
            match b["type"].as_str(){Some("text") if b["text"].is_string()=>text.push(string(b,"text").to_owned()),Some("tool_use") if b["id"].is_string()&&b["name"].is_string()&&b["input"].is_object()=>calls.push(json!({"type":"function","id":b["id"],"function":{"name":b["name"],"arguments":b["input"].to_string()}})),_=>return Err(error(502,MALFORMED))}
        }
        let reason = match v["stop_reason"].as_str() {
            Some("end_turn") => "stop",
            Some("tool_use") => "tool_calls",
            _ => "incomplete",
        };
        return Ok(
            json!({"finish_reason":reason,"message":{"content":text.join("\n"),"tool_calls":calls}}),
        );
    }
    let candidates = v["candidates"]
        .as_array()
        .filter(|a| a.len() == 1)
        .ok_or_else(|| error(502, MALFORMED))?;
    let c = &candidates[0];
    if c["finishReason"] != "STOP"
        || v.get("error").is_some()
        || !v["promptFeedback"]["blockReason"].is_null()
        || c["content"]
            .get("role")
            .is_some_and(|role| role != "" && role != "model")
    {
        return Err(error(502, MALFORMED));
    }
    let parts = c["content"]["parts"]
        .as_array()
        .ok_or_else(|| error(502, MALFORMED))?;
    for p in parts {
        if !closed(p, &["text", "functionCall", "thought", "thoughtSignature"])
            || p.get("text").is_some() == p.get("functionCall").is_some()
            || p.get("thought").is_some_and(|v| !v.is_boolean())
            || p.get("thoughtSignature").is_some_and(|v| !v.is_string())
        {
            return Err(error(502, MALFORMED));
        }
        if let Some(s) = p["text"].as_str() {
            if p["thought"] != true {
                text.push(s.to_owned());
            }
        } else {
            let call = &p["functionCall"];
            if !closed(call, &["name", "args", "id"])
                || !call["name"].is_string()
                || !call.get("args").is_none_or(Value::is_object)
                || !call.get("id").is_none_or(Value::is_string)
            {
                return Err(error(502, MALFORMED));
            }
            calls.push(json!({"id":uuid::Uuid::new_v4().to_string(),"type":"function","function":{"name":call["name"],"arguments":call.get("args").cloned().unwrap_or(json!({})).to_string()},"geminiParts":parts}));
        }
    }
    if calls.len() > 1 {
        return Err(error(502, MALFORMED));
    }
    Ok(
        json!({"finish_reason":if calls.is_empty(){"stop"}else{"tool_calls"},"message":{"content":text.join(""),"tool_calls":calls}}),
    )
}
fn count(v: &Value) -> Option<i64> {
    v.as_i64()
        .filter(|n| *n >= 0 && *n <= 9_007_199_254_740_991)
}
fn sum(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    a.zip(b)
        .and_then(|(a, b)| a.checked_add(b))
        .filter(|n| *n <= 9_007_199_254_740_991)
}
fn usage(family: &str, v: &Value) -> [Option<i64>; 3] {
    let (input, output, total) = match family {
        "anthropic" => {
            let u = &v["usage"];
            let i = sum(
                sum(
                    count(&u["input_tokens"]),
                    count(u.get("cache_creation_input_tokens").unwrap_or(&json!(0))),
                ),
                count(u.get("cache_read_input_tokens").unwrap_or(&json!(0))),
            );
            let o = count(&u["output_tokens"]);
            (i, o, sum(i, o))
        }
        "gemini" => {
            let u = &v["usageMetadata"];
            (
                count(&u["promptTokenCount"]),
                sum(
                    count(&u["candidatesTokenCount"]),
                    count(u.get("thoughtsTokenCount").unwrap_or(&json!(0))),
                ),
                count(&u["totalTokenCount"]),
            )
        }
        _ => {
            let u = &v["usage"];
            let count = |value: &Value| count(value).filter(|n| *n <= 1_000_000_000);
            (
                count(&u["prompt_tokens"]),
                count(&u["completion_tokens"]),
                count(&u["total_tokens"]),
            )
        }
    };
    [input, output, total]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatible_usage_preserves_zero_and_ignores_invalid_counts() {
        for value in [
            Value::Null,
            json!({"usage": []}),
            json!({"usage": {"prompt_tokens": "1", "completion_tokens": -1, "total_tokens": 1.2}}),
            json!({"usage": {"prompt_tokens": 1e30, "completion_tokens": 1_000_000_001}}),
        ] {
            assert_eq!(usage("openai-compatible", &value), [None, None, None]);
        }
        assert_eq!(
            usage(
                "openai-compatible",
                &json!({"usage": {"prompt_tokens": 0, "completion_tokens": 3, "total_tokens": 3}})
            ),
            [Some(0), Some(3), Some(3)]
        );
    }

    #[test]
    fn anthropic_translation_preserves_roles_tools_and_usage() -> Result<()> {
        let body = translate(
            "anthropic",
            "model",
            &[
                json!({"role":"system","content":"System"}),
                json!({"role":"system","content":"Summary"}),
                json!({"role":"user","content":"Question"}),
                json!({"role":"user","content":"Other participant"}),
                json!({"role":"assistant","content":null,"tool_calls":[{"id":"call","function":{"name":"lookup","arguments":r#"{"query":"x"}"#}}]}),
                json!({"role":"tool","tool_call_id":"call","content":"Result"}),
            ],
            &[
                json!({"function":{"name":"lookup","description":"Read","parameters":{"type":"object"}}}),
            ],
            2048,
            false,
        )?;
        assert_eq!(body["max_tokens"], 2048);
        assert_eq!(body["stream"], false);
        assert_eq!(
            body["system"],
            json!([{"type":"text","text":"System"},{"type":"text","text":"Summary"}])
        );
        let messages = &body["messages"];
        assert_eq!(messages.as_array().map(Vec::len), Some(3));
        assert_eq!(messages[0]["content"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            messages[1]["content"],
            json!([{"type":"tool_use","id":"call","name":"lookup","input":{"query":"x"}}])
        );
        assert_eq!(
            messages[2]["content"],
            json!([{"type":"tool_result","tool_use_id":"call","content":"Result"}])
        );
        assert_eq!(
            body["tool_choice"],
            json!({"type":"auto","disable_parallel_tool_use":true})
        );
        for (counts, expected) in [
            (
                json!({"input_tokens":3,"output_tokens":4,"cache_creation_input_tokens":5,"cache_read_input_tokens":6}),
                [Some(14), Some(4), Some(18)],
            ),
            (json!({"input_tokens":0,"output_tokens":0}), [Some(0); 3]),
            (
                json!({"input_tokens":3,"output_tokens":4,"cache_read_input_tokens":"6"}),
                [None, Some(4), None],
            ),
            (Value::Null, [None; 3]),
        ] {
            assert_eq!(usage("anthropic", &json!({"usage":counts})), expected);
        }
        for invalid in [
            json!(false),
            json!({}),
            json!({"type":"message","role":"assistant","content":[{"type":"thinking","thinking":"hidden"}]}),
            json!({"type":"message","role":"assistant","content":[{"type":"tool_use","id":"x","name":"read","input":[]}]}),
        ] {
            assert!(choice("anthropic", &invalid).is_err());
        }
        Ok(())
    }

    #[test]
    fn native_attachment_translation_accepts_only_inline_raster_bytes() -> Result<()> {
        let translate_image = |family, url| {
            translate(
                family,
                "vision",
                &[
                    json!({"role":"user","content":[{"type":"text","text":"Look"},{"type":"image_url","image_url":{"url":url}}]}),
                ],
                &[],
                100,
                false,
            )
        };
        let body = translate_image("anthropic", "data:image/png;base64,AA==")?;
        assert_eq!(
            body["messages"][0]["content"][1],
            json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"AA=="}})
        );
        for family in ["anthropic", "gemini"] {
            for url in [
                "https://example.com/private",
                "data:image/svg+xml;base64,PHN2Zz4=",
                "file:///tmp/private",
            ] {
                assert!(translate_image(family, url).is_err());
            }
        }
        Ok(())
    }

    #[test]
    fn gemini_preserves_signed_content_and_validates_effects() -> Result<()> {
        let parts = json!([
            {"text":"Reasoning","thought":true},
            {"functionCall":{"name":"read_history","args":{}},"thoughtSignature":"signed"}
        ]);
        let candidate = |parts: Value, finish: &str| json!({"candidates":[{"content":{"role":"model","parts":parts},"finishReason":finish}]});
        let result = choice("gemini", &candidate(parts.clone(), "STOP"))?;
        let message = &result["message"];
        assert_eq!(message["content"], "");
        let call = &message["tool_calls"][0];
        let body = translate(
            "gemini",
            "fixture",
            &[
                json!({"role":"system","content":"System"}),
                json!({"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,AA=="}}]}),
                json!({"role":"assistant","content":null,"tool_calls":[call]}),
                json!({"role":"tool","tool_call_id":call["id"],"content":"{}"}),
            ],
            &[],
            100,
            false,
        )?;
        assert_eq!(body["contents"][1]["parts"], parts);
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["name"],
            "read_history"
        );
        assert_eq!(
            body["contents"][0]["parts"][0]["inlineData"]["mimeType"],
            "image/png"
        );

        let no_args = candidate(json!([{"functionCall":{"name":"list_agents"}}]), "STOP");
        assert_eq!(
            choice("gemini", &no_args)?["message"]["tool_calls"][0]["function"]["arguments"],
            "{}"
        );
        for invalid in [
            json!({"text":"hidden call","functionCall":{"name":"read_history","args":{}}}),
            json!({"text":"bad flag","thought":"true"}),
            json!({"functionCall":{"name":"read_history","args":null}}),
            json!({"functionCall":{"name":"read_history","id":{}}}),
        ] {
            assert!(choice("gemini", &candidate(json!([invalid, parts[1]]), "STOP")).is_err());
        }
        for finish in ["MAX_TOKENS", "SAFETY", "MALFORMED_FUNCTION_CALL"] {
            assert!(choice("gemini", &candidate(parts.clone(), finish)).is_err());
        }
        for model in ["../key", "bad?key=x", "x/y", ".model"] {
            assert!(gemini_model(model).is_err());
        }
        assert_eq!(gemini_model("models/model-1")?, "model-1");
        assert_eq!(
            usage(
                "gemini",
                &json!({"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":4,"thoughtsTokenCount":2,"totalTokenCount":9}})
            ),
            [Some(3), Some(6), Some(9)]
        );
        assert_eq!(usage("gemini", &json!({})), [None; 3]);
        Ok(())
    }
}
