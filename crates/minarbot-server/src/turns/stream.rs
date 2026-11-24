#![allow(dead_code)]

use super::*;
use reqwest::Response;

async fn wait_for_chunk<T>(
    idle: Option<std::time::Duration>,
    operation: impl std::future::Future<Output = T>,
) -> Result<T> {
    if let Some(limit) = idle {
        tokio::time::timeout(limit, operation)
            .await
            .map_err(|_| error(502, provider::STREAM_ERROR))
    } else {
        Ok(operation.await)
    }
}
pub fn text(state: &AppState, call: i64, text: &str, final_text: bool) -> Result<()> {
    if text.len() > 2 * 1024 * 1024 {
        return Err(error(502, provider::MALFORMED));
    }
    if text.is_empty() && !final_text {
        return Ok(());
    }
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    transaction(db, || {
        let rev = one(
            db,
            "SELECT coalesce(max(revision),0)+1 AS rev FROM provider_stream_segments WHERE call=?",
            [call],
        )?
        .ok_or_else(Failure::storage)?;
        db.execute("INSERT INTO provider_stream_segments VALUES(?,0,'text-0','',?,'',?,?) ON CONFLICT(call,ordinal) DO UPDATE SET text=excluded.text,final=excluded.final,revision=excluded.revision",params![call,text,i64::from(final_text),rev["rev"].as_i64()])?;
        db.execute(
            "UPDATE provider_stream_segments SET kind='text' WHERE call=? AND ordinal=0",
            [call],
        )?;
        db.execute("INSERT INTO provider_streams VALUES(?,?) ON CONFLICT(call) DO UPDATE SET text=excluded.text",params![call,text])?;
        Ok(())
    })?;
    if let Some(turn) = one(db, "SELECT turn FROM provider_calls WHERE seq=?", [call])? {
        state.events.turn(db, string(&turn, "turn"))?;
    }
    Ok(())
}
pub fn tool(state: &AppState, call: i64, id: &str, name: &str) -> Result<()> {
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    transaction(db, || {
        let ordinal: i64 = db.query_row(
            "SELECT coalesce(max(ordinal),-1)+1 FROM provider_stream_segments WHERE call=?",
            [call],
            |r| r.get(0),
        )?;
        let rev: i64 = db.query_row(
            "SELECT coalesce(max(revision),0)+1 FROM provider_stream_segments WHERE call=?",
            [call],
            |r| r.get(0),
        )?;
        db.execute(
            "INSERT INTO provider_stream_segments VALUES(?,?,?,'tool',?,'requested',0,?)",
            params![call, ordinal, format!("tool-{id}"), name, rev],
        )?;
        db.execute(
            "INSERT INTO provider_streams VALUES(?,'') ON CONFLICT(call) DO UPDATE SET text=''",
            [call],
        )?;
        Ok(())
    })?;
    if let Some(turn) = one(db, "SELECT turn FROM provider_calls WHERE seq=?", [call])? {
        state.events.turn(db, string(&turn, "turn"))?;
    }
    Ok(())
}
#[derive(Default)]
struct Decoder {
    text: String,
    reason: Option<String>,
    usage: Value,
    call: Option<Value>,
    blocks: Vec<Value>,
    started: bool,
    active: Option<usize>,
    tool_json: String,
    stopped: bool,
    done: bool,
}
impl Decoder {
    fn event(
        &mut self,
        raw: &str,
        family: &str,
        mut publish: impl FnMut(&str) -> Result<()>,
    ) -> Result<()> {
        let invalid = || error(502, provider::STREAM_ERROR);
        if family == "openai-compatible" && raw == "[DONE]" {
            if self.reason.is_none() {
                return Err(invalid());
            }
            self.done = true;
            return Ok(());
        }
        let v: Value = serde_json::from_str(raw).map_err(|_| invalid())?;
        if !v.is_object() || !v["error"].is_null() || v["type"] == "error" {
            return Err(invalid());
        }
        if family == "openai-compatible" {
            let choices = v["choices"]
                .as_array()
                .filter(|a| a.len() <= 1)
                .ok_or_else(invalid)?;
            if !v["usage"].is_null() {
                if !v["usage"].is_object() {
                    return Err(invalid());
                }
                self.usage = v["usage"].clone();
            }
            if choices.is_empty() {
                return Ok(());
            }
            let c = &choices[0];
            let d = &c["delta"];
            if c["index"] != 0
                || self.reason.is_some()
                || !d.is_object()
                || d.get("role").is_some_and(|r| r != "assistant")
            {
                return Err(invalid());
            }
            if !d["content"].is_null() {
                let s = d["content"].as_str().ok_or_else(invalid)?;
                self.text.push_str(s);
                publish(&self.text)?;
            }
            if let Some(parts) = d.get("tool_calls") {
                let parts = parts
                    .as_array()
                    .filter(|a| a.len() == 1)
                    .ok_or_else(invalid)?;
                let p = &parts[0];
                if p["index"] != 0 || p.get("type").is_some_and(|t| t != "function") {
                    return Err(invalid());
                }
                let call = self.call.get_or_insert_with(
                    || json!({"id":"","type":"function","function":{"name":"","arguments":""}}),
                );
                if let Some(extra) = p.get("extra_content") {
                    if !extra.is_object() || call.get("extra_content").is_some_and(|v| v != extra) {
                        return Err(invalid());
                    }
                    call["extra_content"] = extra.clone();
                }
                if let Some(id) = p.get("id") {
                    let s = id.as_str().ok_or_else(invalid)?;
                    if !string(call, "id").is_empty() && call["id"] != s {
                        return Err(invalid());
                    }
                    call["id"] = json!(s);
                }
                if let Some(f) = p.get("function") {
                    if !f.is_object() {
                        return Err(invalid());
                    }
                    for k in ["name", "arguments"] {
                        if let Some(s) = f.get(k) {
                            let s = s.as_str().ok_or_else(invalid)?;
                            let mut text = string(&call["function"], k).to_owned();
                            text.push_str(s);
                            call["function"][k] = json!(text);
                        }
                    }
                }
                if string(call, "id").encode_utf16().count() > 200
                    || string(&call["function"], "name").encode_utf16().count() > 200
                    || string(&call["function"], "arguments")
                        .encode_utf16()
                        .count()
                        > 20000
                {
                    return Err(invalid());
                }
            }
            if !c["finish_reason"].is_null() {
                self.reason = Some(c["finish_reason"].as_str().ok_or_else(invalid)?.to_owned());
            }
            return Ok(());
        }
        if family == "gemini" {
            if !v["promptFeedback"]["blockReason"].is_null() {
                return Err(invalid());
            }
            if !v["usageMetadata"].is_null() {
                if !v["usageMetadata"].is_object() {
                    return Err(invalid());
                }
                self.usage = v["usageMetadata"].clone();
            }
            if v.get("candidates").is_none()
                && v["promptFeedback"].is_object()
                && self.blocks.is_empty()
                && self.reason.is_none()
            {
                return Ok(());
            }
            let candidates = v["candidates"]
                .as_array()
                .filter(|a| a.len() == 1)
                .ok_or_else(invalid)?;
            let c = &candidates[0];
            if self.reason.is_some() || c.get("index").is_some_and(|i| i != 0) {
                return Err(invalid());
            }
            if let Some(content) = c.get("content") {
                if content.get("role").is_some_and(|r| r != "" && r != "model") {
                    return Err(invalid());
                }
                for part in content["parts"].as_array().ok_or_else(invalid)? {
                    if !part.is_object() {
                        return Err(invalid());
                    }
                    self.blocks.push(part.clone());
                    if let Some(t) = part.get("text") {
                        if part["thought"] != true {
                            self.text.push_str(t.as_str().ok_or_else(invalid)?);
                            publish(&self.text)?;
                        }
                    }
                }
            }
            if let Some(r) = c.get("finishReason") {
                self.reason = Some(r.as_str().ok_or_else(invalid)?.to_owned());
            }
            return Ok(());
        }
        match v["type"].as_str() {
            Some("ping") => (),
            Some("message_start") => {
                let m = &v["message"];
                if self.started
                    || m["type"] != "message"
                    || m["role"] != "assistant"
                    || !m["content"].as_array().is_some_and(Vec::is_empty)
                {
                    return Err(invalid());
                }
                self.usage = m["usage"].clone();
                self.started = true;
            }
            _ if !self.started => return Err(invalid()),
            Some("content_block_start") => {
                if self.active.is_some()
                    || self.reason.is_some()
                    || v["index"].as_u64() != Some(self.blocks.len() as u64)
                    || self.blocks.len() >= 100
                {
                    return Err(invalid());
                }
                let block = v["content_block"].clone();
                match block["type"].as_str() {
                    Some("text") => {
                        if self.blocks.iter().any(|b| b["type"] == "text") {
                            self.text.push('\n');
                        }
                        self.text
                            .push_str(block["text"].as_str().ok_or_else(invalid)?);
                        publish(&self.text)?;
                    }
                    Some("tool_use") => {
                        if self.blocks.iter().any(|b| b["type"] == "tool_use")
                            || !block["id"].is_string()
                            || !block["name"].is_string()
                            || !block["input"].is_object()
                        {
                            return Err(invalid());
                        }
                        self.tool_json.clear();
                    }
                    _ => return Err(invalid()),
                }
                self.blocks.push(block);
                self.active = Some(self.blocks.len() - 1);
            }
            Some("content_block_delta") => {
                let index = self
                    .active
                    .filter(|i| v["index"].as_u64() == Some(*i as u64) && self.reason.is_none())
                    .ok_or_else(invalid)?;
                let d = &v["delta"];
                let block = &mut self.blocks[index];
                match d["type"].as_str() {
                    Some("text_delta") if block["type"] == "text" => {
                        let s = d["text"].as_str().ok_or_else(invalid)?;
                        self.text.push_str(s);
                        publish(&self.text)?;
                        block["text"] = json!(format!("{}{}", string(block, "text"), s));
                    }
                    Some("input_json_delta") if block["type"] == "tool_use" => {
                        self.tool_json
                            .push_str(d["partial_json"].as_str().ok_or_else(invalid)?);
                        if self.tool_json.encode_utf16().count() > 20000 {
                            return Err(invalid());
                        }
                    }
                    _ => return Err(invalid()),
                }
            }
            Some("content_block_stop") => {
                let index = self
                    .active
                    .filter(|i| v["index"].as_u64() == Some(*i as u64))
                    .ok_or_else(invalid)?;
                if self.blocks[index]["type"] == "tool_use" && !self.tool_json.is_empty() {
                    let input: Value =
                        serde_json::from_str(&self.tool_json).map_err(|_| invalid())?;
                    if !input.is_object() {
                        return Err(invalid());
                    }
                    self.blocks[index]["input"] = input;
                }
                self.active = None;
            }
            Some("message_delta") => {
                if self.active.is_some() || self.reason.is_some() {
                    return Err(invalid());
                }
                self.reason = Some(
                    v["delta"]["stop_reason"]
                        .as_str()
                        .ok_or_else(invalid)?
                        .to_owned(),
                );
                if self.usage.is_null() {
                    self.usage = json!({});
                }
                if !self.usage.is_object() {
                    return Err(invalid());
                }
                self.usage["output_tokens"] = Value::Null;
                if let Some(usage) = v.get("usage") {
                    for (k, value) in usage.as_object().ok_or_else(invalid)? {
                        if !value.is_null() {
                            self.usage[k] = value.clone();
                        }
                    }
                }
                self.stopped = true;
            }
            Some("message_stop") => {
                if !self.stopped || self.active.is_some() {
                    return Err(invalid());
                }
                self.done = true;
            }
            _ => return Err(invalid()),
        }
        Ok(())
    }
    fn finish(self, family: &str, termination: &str) -> Result<Value> {
        let reason = self
            .reason
            .ok_or_else(|| error(502, provider::STREAM_ERROR))?;
        if family == "openai-compatible" {
            if !self.done && termination != "finish-reason" {
                return Err(error(502, provider::STREAM_ERROR));
            }
            return Ok(
                json!({"usage":self.usage,"choices":[{"finish_reason":reason,"message":{"content":self.text,"tool_calls":self.call.into_iter().collect::<Vec<_>>()}}]}),
            );
        }
        if family == "gemini" {
            return Ok(
                json!({"usageMetadata":self.usage,"candidates":[{"finishReason":reason,"content":{"role":"model","parts":self.blocks}}]}),
            );
        }
        if !self.done {
            return Err(error(502, provider::STREAM_ERROR));
        }
        Ok(
            json!({"type":"message","role":"assistant","stop_reason":reason,"content":self.blocks,"usage":self.usage}),
        )
    }
}
pub async fn read(
    mut response: Response,
    family: &str,
    termination: &str,
    idle: Option<std::time::Duration>,
    mut publish: impl FnMut(&str) -> Result<()>,
) -> Result<Value> {
    let mut decoder = Decoder::default();
    let mut bytes = Vec::new();
    let mut data = Vec::new();
    let mut name = String::new();
    let mut size = 0;
    let mut count = 0;
    while let Some(chunk) = wait_for_chunk(idle, response.chunk())
        .await?
        .map_err(|_| error(502, provider::STREAM_ERROR))?
    {
        size += chunk.len();
        if size > 2 * 1024 * 1024 {
            return Err(error(502, provider::STREAM_ERROR));
        }
        bytes.extend_from_slice(&chunk);
        while let Some(index) = bytes.iter().position(|b| matches!(b, b'\r' | b'\n')) {
            if bytes[index] == b'\r' && index + 1 == bytes.len() {
                break;
            }
            let consumed = index
                + if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
            let line = std::str::from_utf8(&bytes[..index])
                .map_err(|_| error(502, provider::STREAM_ERROR))?
                .to_owned();
            bytes.drain(..consumed);
            if line.is_empty() {
                if name == "error" {
                    return Err(error(502, provider::STREAM_ERROR));
                }
                if !data.is_empty() {
                    count += 1;
                    if count > 20000 {
                        return Err(error(502, provider::STREAM_ERROR));
                    }
                    decoder.event(&data.join("\n"), family, &mut publish)?;
                    data.clear();
                    if decoder.done {
                        return decoder.finish(family, termination);
                    }
                }
                name.clear();
            } else if line == "data" || line.starts_with("data:") {
                data.push(
                    line.get(5..)
                        .unwrap_or("")
                        .strip_prefix(' ')
                        .unwrap_or(line.get(5..).unwrap_or(""))
                        .to_owned(),
                );
            } else if let Some(s) = line.strip_prefix("event:") {
                name = s.strip_prefix(' ').unwrap_or(s).to_owned();
            }
        }
    }
    if bytes == b"\r" {
        if name == "error" {
            return Err(error(502, provider::STREAM_ERROR));
        }
        if !data.is_empty() {
            decoder.event(&data.join("\n"), family, &mut publish)?;
        }
        data.clear();
        name.clear();
        bytes.clear();
    }
    if !data.is_empty() || !name.is_empty() || !bytes.is_empty() && !bytes.starts_with(b":") {
        return Err(error(502, provider::STREAM_ERROR));
    }
    decoder.finish(family, termination)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unfinished_tool_and_keeps_metadata() -> Result<()> {
        let mut d = Decoder::default();
        d.event(r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"read","arguments":"{}"},"extra_content":{"vendor":{"version":1}}}]},"finish_reason":null}]}"#,"openai-compatible",|_|Ok(()))?;
        assert!(d.finish("openai-compatible", "marker").is_err());
        let mut d = Decoder::default();
        d.event(
            r#"{"choices":[{"index":0,"delta":{"content":"Hello 世界"},"finish_reason":"stop"}]}"#,
            "openai-compatible",
            |_| Ok(()),
        )?;
        assert_eq!(
            d.finish("openai-compatible", "finish-reason")?["choices"][0]["message"]["content"],
            "Hello 世界"
        );
        Ok(())
    }
}

#[cfg(test)]
mod gemini_tests {
    use super::*;

    #[test]
    fn streams_visible_text_and_requires_gemini_finish() -> Result<()> {
        let mut decoder = Decoder::default();
        let mut visible = String::new();
        let mut publish = |text: &str| {
            visible = text.to_owned();
            Ok(())
        };
        for event in [
            json!({"promptFeedback":{"safetyRatings":[]}}),
            json!({"candidates":[{"content":{"parts":[{"text":"hidden","thought":true},{"text":"Hello "}]}}]}),
            json!({"candidates":[{"content":{"role":"model","parts":[{"text":"world"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":1}}),
        ] {
            decoder.event(&event.to_string(), "gemini", &mut publish)?;
        }
        assert_eq!(visible, "Hello world");
        let result = decoder.finish("gemini", "marker")?;
        assert_eq!(
            provider::choice("gemini", &result)?["message"]["content"],
            "Hello world"
        );

        let mut incomplete = Decoder::default();
        incomplete.event(
            r#"{"candidates":[{"content":{"parts":[{"text":"partial"}]}}]}"#,
            "gemini",
            |_| Ok(()),
        )?;
        assert!(incomplete.finish("gemini", "marker").is_err());
        Ok(())
    }
}

#[cfg(test)]
mod sse_tests {
    use super::*;
    use axum::body::Body;
    use tokio::net::TcpListener;

    async fn fixture(
        text: impl Into<String>,
        width: usize,
        interrupted: bool,
        family: &str,
        termination: &str,
        publish: impl FnMut(&str) -> Result<()>,
    ) -> Result<Value> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| error(500, e.to_string()))?;
        let origin = format!(
            "http://{}",
            listener
                .local_addr()
                .map_err(|e| error(500, e.to_string()))?
        );
        let text = text.into();
        let server = tokio::spawn(async move {
            let app = Router::new().fallback(move || async move {
                let chunks = text
                    .as_bytes()
                    .chunks(width)
                    .map(axum::body::Bytes::copy_from_slice)
                    .collect::<Vec<_>>();
                Body::from_stream(async_stream::stream! {
                    for chunk in chunks {
                        yield Ok(chunk);
                    }
                    if interrupted {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        yield Err(std::io::Error::other("simulated broken connection"));
                    }
                })
            });
            axum::serve(listener, app).await
        });
        let parsed = match reqwest::get(origin).await {
            Ok(response) => read(response, family, termination, None, publish).await,
            Err(error) => Err(super::error(500, error.to_string())),
        };
        server.abort();
        parsed
    }

    fn sse(v: &Value) -> String {
        format!("data: {v}\n\n")
    }
    fn chunk(delta: Value, reason: Value) -> String {
        sse(&json!({"choices":[{"index":0,"delta":delta,"finish_reason":reason}]}))
    }
    const DONE: &str = "data: [DONE]\n\n";

    #[tokio::test]
    async fn compatible_eof_requires_finish_and_clean_transport() -> Result<()> {
        let partial = chunk(json!({"content":"Result"}), Value::Null);
        let complete = partial.clone() + &chunk(json!({}), json!("stop"));
        let check = |text, interrupted, mode| {
            fixture(text, 64, interrupted, "openai-compatible", mode, |_| Ok(()))
        };
        let result = check(complete.clone(), false, "finish-reason").await?;
        assert_eq!(result["choices"][0]["message"]["content"], "Result");
        assert!(check(complete.clone(), false, "marker").await.is_err());
        assert!(check(partial, false, "finish-reason").await.is_err());
        assert!(check(
            complete.clone() + "data: {\"choices\":",
            false,
            "finish-reason"
        )
        .await
        .is_err());
        assert!(check(complete, true, "finish-reason").await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn openai_sse_handles_framing_metadata_and_invalid_events() -> Result<()> {
        let usage = json!({"prompt_tokens":2,"completion_tokens":4,"total_tokens":6});
        let stream = String::from(": comment\n\n")
            + &chunk(json!({"role":"assistant","content":"Halo "}), Value::Null)
            + &chunk(json!({"content":"世界"}), Value::Null)
            + &chunk(json!({}), json!("stop"))
            + &sse(&json!({"choices":[],"usage":usage}))
            + DONE;
        for newline in ["\r\n", "\r"] {
            let mut latest = String::new();
            let parsed = fixture(
                stream.replace('\n', newline),
                1,
                false,
                "openai-compatible",
                "marker",
                |text| {
                    latest = text.to_owned();
                    Ok(())
                },
            )
            .await?;
            assert_eq!(latest, "Halo 世界");
            assert_eq!(parsed["usage"], usage);
        }
        let check = |data, width| {
            fixture(
                data,
                width,
                false,
                "openai-compatible",
                "marker",
                |_| Ok(()),
            )
        };
        let multiline = "data: {\"choices\":\ndata: [{\"index\":0,\"delta\":{\"content\":\"Yes\"},\"finish_reason\":\"stop\"}]}\n\n";
        assert!(check(multiline.to_owned() + DONE, 15).await.is_ok());

        let extra =
            json!({"google":{"thought_signature":"opaque-signature"},"vendor":{"version":1}});
        let metadata = |v| {
            chunk(
                json!({"tool_calls":[{"index":0,"extra_content":v}]}),
                Value::Null,
            )
        };
        let start = chunk(
            json!({"tool_calls":[{"index":0,"id":"call","type":"function","function":{"name":"read_history","arguments":"{"}}]}),
            Value::Null,
        );
        let end = chunk(
            json!({"tool_calls":[{"index":0,"function":{"arguments":"}"}}]}),
            Value::Null,
        );
        let finish = chunk(json!({}), json!("tool_calls")) + DONE;
        for tool in [
            metadata(extra.clone()) + &start + &end + &finish,
            start.clone()
                + &end
                + &metadata(extra.clone())
                + &metadata(json!({"vendor":{"version":1},"google":extra["google"]}))
                + &finish,
        ] {
            let result = check(tool, 17).await?;
            let call = &result["choices"][0]["message"]["tool_calls"][0];
            assert_eq!(call["function"]["arguments"], "{}");
            assert_eq!(call["extra_content"], extra);
        }
        for invalid in [Value::Null, json!([]), json!(false), json!("signature")] {
            assert!(check(metadata(invalid) + &start + &end + &finish, 100)
                .await
                .is_err());
        }
        for malformed in [
            String::from("event: error\n") + multiline + DONE,
            chunk(json!({"content":"Partial"}), Value::Null),
            chunk(json!({}), json!("stop")),
            DONE.to_owned(),
            chunk(json!({"tool_calls":[{"index":1}]}), Value::Null),
            chunk(json!({"content":42}), Value::Null),
            sse(&json!({"error":{"message":"fixture error"}})),
            start.replace("read_history", &"x".repeat(201)) + &end + &finish,
            metadata(extra)
                + &metadata(json!({"google":{"thought_signature":"changed"}}))
                + &start
                + &end
                + &finish,
            "x".repeat(2 * 1024 * 1024 + 1),
        ] {
            assert!(check(malformed, 65536).await.is_err());
        }
        Ok(())
    }

    #[tokio::test]
    async fn anthropic_sse_requires_order_and_keeps_unknown_usage() -> Result<()> {
        let start = json!({"type":"message_start","message":{"type":"message","role":"assistant","content":[],"usage":{"input_tokens":2,"output_tokens":0,"cache_read_input_tokens":3}}});
        let block = json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}});
        let delta = json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Native"}});
        let stop = json!({"type":"content_block_stop","index":0});
        let done = json!({"type":"message_stop"});
        let finish = json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}});
        let check = |events: &[Value]| {
            fixture(
                events.iter().map(sse).collect::<String>(),
                3,
                false,
                "anthropic",
                "marker",
                |_| Ok(()),
            )
        };
        let mut reported = finish.clone();
        reported["usage"] = json!({"output_tokens":4});
        let result = check(&[
            start.clone(),
            json!({"type":"ping"}),
            block.clone(),
            delta.clone(),
            stop.clone(),
            reported,
            done.clone(),
        ])
        .await?;
        assert_eq!(result["content"][0]["text"], "Native");
        assert_eq!(
            result["usage"],
            json!({"input_tokens":2,"output_tokens":4,"cache_read_input_tokens":3})
        );
        let unknown = check(&[
            start.clone(),
            block.clone(),
            delta.clone(),
            stop.clone(),
            finish.clone(),
            done.clone(),
        ])
        .await?;
        assert_eq!(unknown["usage"]["output_tokens"], Value::Null);
        for malformed in [
            vec![start.clone(), delta],
            vec![start.clone(), block.clone(), finish, done.clone()],
            vec![start.clone(), block, stop, done],
            vec![start.clone(), start.clone()],
            vec![
                start,
                json!({"type":"error","error":{"message":"fixture error"}}),
            ],
        ] {
            assert!(check(&malformed).await.is_err());
        }
        Ok(())
    }
}
