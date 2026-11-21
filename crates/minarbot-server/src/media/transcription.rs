use super::*;
use sha2::{Digest, Sha256};

pub(crate) fn audio(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 16 && bytes.get(4..8) == Some(b"ftyp") {
        let size = u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?) as usize;
        return (size >= 16
            && size <= bytes.len()
            && size.is_multiple_of(4)
            && [
                b"M4A ", b"M4B ", b"isom", b"mp41", b"mp42", b"iso2", b"iso5", b"iso6",
            ]
            .contains(&bytes.get(8..12)?.try_into().ok()?))
        .then_some("mp4");
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE") {
        return Some("wav");
    }
    if bytes.starts_with(b"ID3")
        || bytes.first() == Some(&255) && bytes.get(1).is_some_and(|b| b & 0xe0 == 0xe0)
    {
        return Some("mp3");
    }
    if bytes.starts_with(b"fLaC") {
        return Some("flac");
    }
    if bytes.starts_with(b"OggS") {
        return Some("ogg");
    }
    if bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        let (size, width) = vint(bytes, 4, false)?;
        let end = 4 + width + size;
        if size > 4096 || end > bytes.len() {
            return None;
        }
        let mut offset = 4 + width;
        let mut webm = false;
        while offset < end {
            let (id, width) = vint(bytes, offset, true)?;
            offset += width;
            let (size, width) = vint(bytes, offset, false)?;
            offset += width;
            if offset + size > end {
                return None;
            }
            if id == 0x4282 {
                if webm || bytes.get(offset..offset + size) != Some(b"webm") {
                    return None;
                }
                webm = true;
            }
            offset += size;
        }
        return webm.then_some("webm");
    }
    None
}
fn vint(bytes: &[u8], offset: usize, marker: bool) -> Option<(usize, usize)> {
    let first = *bytes.get(offset)?;
    let width = first.leading_zeros() as usize + 1;
    if width > 4 || offset + width > bytes.len() {
        return None;
    }
    let mut value = if marker {
        first as usize
    } else {
        (first & ((1 << (8 - width)) - 1)) as usize
    };
    for byte in bytes.get(offset + 1..offset + width)? {
        value = value * 256 + *byte as usize;
    }
    if !marker && value == (1usize << (7 * width)) - 1 {
        return None;
    }
    Some((value, width))
}
fn visible(row: Option<Value>) -> Result<Value> {
    let Some(mut row) = row else {
        return Ok(Value::Null);
    };
    row["result"] = row["result"]
        .as_str()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| Failure::storage())?
        .unwrap_or(Value::Null);
    row["usage"] = row["usage"]
        .as_str()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| Failure::storage())?
        .unwrap_or(Value::Null);
    Ok(row)
}
fn operation(db: &Connection, id: &str) -> Result<Value> {
    visible(one(
        db,
        "SELECT id,model,origin,status,created,result,usage FROM transcriptions WHERE id=?",
        [id],
    )?)
}
fn writable(db: &Connection, kind: &str, owner: &str) -> Result<()> {
    let row = crate::files::context(db, kind, owner)?;
    if row["archived"] == 1 || kind == "agents" && data::agents::hidden(db, owner)? {
        return Err(error(409, "Restore the conversation before transcribing."));
    }
    Ok(())
}
pub(crate) fn destination(state: &AppState) -> Result<Value> {
    let Some(config) = &state.config.media.transcription_provider else {
        return Ok(Value::Null);
    };
    let url = config
        .endpoint("/audio/transcriptions")
        .map_err(|_| Failure::storage())?;
    let id = format!(
        "{:x}",
        Sha256::digest(json!([url.as_str(), config.model]).to_string().as_bytes())
    );
    Ok(json!({"id":id,"origin":url.origin().ascii_serialization(),"model":config.model}))
}

pub async fn handle(state: &AppState, request: Request, parts: &[&str]) -> Result<Response> {
    let kind = *parts.get(1).ok_or_else(Failure::storage)?;
    let owner = *parts.get(2).ok_or_else(Failure::storage)?;
    let file = *parts.get(4).ok_or_else(Failure::storage)?;
    let scope = format!("{kind}:{owner}");
    let bytes = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        crate::files::context(&guard.connection, kind, owner)?;
        crate::files::attachments::file(&guard.connection, file, &scope)?.bytes
    };
    let config = state.config.media.transcription_provider.clone();
    let provider = destination(state)?;
    if request.method() == Method::GET {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let last=visible(one(&guard.connection,"SELECT id,model,origin,status,created,result,usage FROM transcriptions WHERE file=? ORDER BY rowid DESC LIMIT 1",[file])?)?;
        return Ok(crate::json_response(
            200,
            json!({"provider":provider,"format":audio(&bytes),"operation":last}),
        ));
    }
    if request.method() != Method::POST {
        return Err(error(405, "Transcription method is not supported."));
    }
    if request.headers().contains_key("authorization") {
        return Err(error(
            403,
            "Transcription requires user approval in Minarbot.",
        ));
    }
    if crate::text_header(request.headers(), "origin") != Some(state.origin.as_str())
        || crate::text_header(request.headers(), "content-type") != Some("application/json")
    {
        return Err(error(
            403,
            "Confirm transcription must berasal from Minarbot.",
        ));
    }
    let input = crate::files::body(request, 4096)
        .await
        .map_err(|_| error(400, "Invalid transcription request."))?;
    if !input.is_object() {
        return Err(error(400, "Invalid transcription request."));
    }
    let id = string(&input, "id");
    if input["action"] == "cancel" {
        if !closed(&input, &["id", "action"]) || !input["id"].is_string() {
            return Err(error(400, "Invalid transcription cancellation request."));
        }
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let row = one(
            &guard.connection,
            "SELECT status FROM transcriptions WHERE id=? AND file=?",
            [id, file],
        )?
        .ok_or_else(|| error(404, "Transcription not found."))?;
        if !["pending", "cancelled"].contains(&string(&row, "status")) {
            return Err(error(409, "Transcription has ended. Reload status."));
        }
        guard.connection.execute(
            "UPDATE transcriptions SET status='cancelled' WHERE id=?",
            [id],
        )?;
        if let Ok(active) = state.turns.media.lock() {
            if let Some(cancel) = active.get(id) {
                cancel.send_replace(true);
            }
        }
        return Ok(crate::json_response(200, operation(&guard.connection, id)?));
    }
    if !closed(&input, &["id", "provider", "consent", "retryOf"])
        || !uuid(id)
        || !input["provider"].is_string()
        || input["consent"] != true
        || input.get("retryOf").is_some_and(|v| !v.is_string())
    {
        return Err(error(
            400,
            "File-send confirmation and request ID are required.",
        ));
    }
    let encoded=json!({"file":file,"scope":scope,"provider":input["provider"],"consent":true,"retryOf":input["retryOf"]}).to_string();
    let (cancel, mut cancelled) = watch::channel(false);
    {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        if let Some(old) = one(db, "SELECT request FROM transcriptions WHERE id=?", [id])? {
            if old["request"] != encoded {
                return Err(error(
                    409,
                    "Transcription ID is already used for a different request.",
                ));
            }
            writable(db, kind, owner)?;
            return Ok(crate::json_response(200, operation(db, id)?));
        }
        if provider.is_null() {
            return Err(error(503, "Transcription provider is not configured."));
        }
        let provider = &provider;
        if input["provider"] != provider["id"] {
            return Err(error(
                409,
                "Transcription provider changed. Reload and confirm the new destination.",
            ));
        }
        if audio(&bytes).is_none() || bytes.len() > 10 * 1024 * 1024 {
            return Err(error(
                415,
                "Use a WAV, MP3, FLAC, OGG, WebM, or M4A/MP4 file up to 10 MiB.",
            ));
        }
        let mut active = state.turns.media.lock().map_err(|_| Failure::storage())?;
        if active.len() >= 2 {
            return Err(error(
                503,
                "Transcription is unavailable. Wait and try again.",
            ));
        }
        writable(db, kind, owner)?;
        let last = one(
            db,
            "SELECT id,status FROM transcriptions WHERE file=? ORDER BY rowid DESC LIMIT 1",
            [file],
        )?;
        if last.as_ref().is_some_and(|r| {
            !["uncertain", "cancelled"].contains(&string(r, "status"))
                || input["retryOf"] != r["id"]
        }) || last.is_none() && input.get("retryOf").is_some()
        {
            return Err(error(
                409,
                "Check the previous transcription before creating a new request.",
            ));
        }
        db.execute("INSERT INTO transcriptions(id,file,request,model,origin,status,created) VALUES(?,?,?,?,?,'pending',?)",params![id,file,encoded,string(provider,"model"),string(provider,"origin"),iso(db,state.config.now())?])?;
        active.insert(id.into(), cancel);
    }
    let config = config.ok_or_else(Failure::storage)?;
    let work = async {
        let format = audio(&bytes).ok_or_else(Failure::storage)?;
        let boundary = format!("minarbot{}", uuid::Uuid::new_v4().simple());
        let mut body=format!("--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\n{}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"response_format\"\r\n\r\njson\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.{format}\"\r\nContent-Type: audio/{format}\r\n\r\n",config.model).into_bytes();
        body.extend(&bytes);
        body.extend(format!("\r\n--{boundary}--\r\n").as_bytes());
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Failure::storage())?;
        let mut req = client
            .post(
                config
                    .endpoint("/audio/transcriptions")
                    .map_err(|_| Failure::storage())?,
            )
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body);
        if let Some(key) = &config.api_key {
            req = req.bearer_auth(key);
        }
        let bytes = super::images::response(
            req.send().await.map_err(|_| Failure::storage())?,
            512 * 1024,
        )
        .await?;
        #[derive(serde::Deserialize)]
        struct Receipt {
            usage: Option<Value>,
        }
        let receipt: Receipt = serde_json::from_slice(&bytes).map_err(|_| Failure::storage())?;
        let usage = usage(receipt.usage.as_ref().unwrap_or(&Value::Null));
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        db.execute(
            "UPDATE transcriptions SET usage=? WHERE id=?",
            [usage.to_string(), id.into()],
        )?;
        let data: Value = serde_json::from_slice(&bytes).map_err(|_| Failure::storage())?;
        let text = data["text"]
            .as_str()
            .filter(|s| {
                !s.trim().is_empty()
                    && s.len() <= 256 * 1024
                    && !s.chars().any(|c| {
                        c <= '\x08' || c == '\x0b' || c == '\x0c' || ('\x0e'..='\x1f').contains(&c)
                    })
                    && !config
                        .api_key
                        .as_ref()
                        .is_some_and(|key| !key.is_empty() && s.contains(key))
            })
            .ok_or_else(Failure::storage)?;
        transaction(db, || {
            writable(db, kind, owner)?;
            crate::files::attachments::file(db, file, &scope)?;
            if !exists(
                db,
                "SELECT 1 FROM transcriptions WHERE id=? AND status='pending'",
                [id],
            )? {
                return Err(error(409, "Stopped"));
            }
            let output = uuid::Uuid::new_v4().to_string();
            let created = iso(db, state.config.now())?;
            db.execute("INSERT INTO files(id,scope,name,digest,bytes,created) VALUES(?,?,'transcript.txt',?,?,?)",params![output,scope,format!("{:x}",Sha256::digest(text.as_bytes())),text.as_bytes(),created])?;
            db.execute("UPDATE transcriptions SET status='done',result=? WHERE id=?",[json!({"id":output,"name":"transcript.txt","size":text.len(),"created":created}).to_string(),id.into()])?;
            Ok(())
        })
    };
    tokio::select! {_=tokio::time::timeout(std::time::Duration::from_secs(120),work)=>(),_=cancelled.changed()=>()}
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    guard.connection.execute(
        "UPDATE transcriptions SET status='uncertain' WHERE id=? AND status='pending'",
        [id],
    )?;
    if let Ok(mut active) = state.turns.media.lock() {
        active.remove(id);
    }
    crate::files::context(&guard.connection, kind, owner)?;
    Ok(crate::json_response(200, operation(&guard.connection, id)?))
}
fn usage(v: &Value) -> Value {
    if v["type"] == "duration"
        && v["seconds"]
            .as_f64()
            .is_some_and(|n| (0.0..=1e9).contains(&n))
    {
        return json!({"type":"duration","seconds":v["seconds"]});
    }
    if v["type"] == "tokens" {
        let tokens = ["input_tokens", "output_tokens", "total_tokens"].map(|k| {
            v[k].as_i64()
                .filter(|n| (0..=9_007_199_254_740_991).contains(n))
        });
        if tokens.iter().any(Option::is_some) {
            return json!({"type":"tokens","input_tokens":tokens[0],"output_tokens":tokens[1],"total_tokens":tokens[2]});
        }
    }
    Value::Null
}
#[cfg(test)]
mod tests {
    use super::{audio, usage};
    use serde_json::{json, Value};

    #[test]
    fn audio_headers() {
        assert_eq!(audio(b"RIFF0000WAVEdata"), Some("wav"));
        assert_eq!(audio(b"plain text"), None);
    }

    #[test]
    fn webm_requires_bounded_ebml_header_and_exact_doctype() {
        let valid = *b"\x1a\x45\xdf\xa3\x87\x42\x82\x84webm";
        assert_eq!(audio(&valid), Some("webm"));
        for len in 0..valid.len() {
            assert_eq!(audio(&valid[..len]), None);
        }
        for (offset, byte) in [(8, b'm'), (4, 0xff), (5, 0x43)] {
            let mut invalid = valid;
            invalid[offset] = byte;
            assert_eq!(audio(&invalid), None);
        }
    }

    #[test]
    fn mp4_checks_brand_and_ftyp_size() {
        let mut bytes = *b"\x00\x00\x00\x18ftypM4A \x00\x00\x00\x00isommp42";
        for brand in [
            b"M4A ", b"M4B ", b"isom", b"mp41", b"mp42", b"iso2", b"iso5", b"iso6",
        ] {
            bytes[8..12].copy_from_slice(brand);
            assert_eq!(audio(&bytes), Some("mp4"));
        }
        for brand in [b"avif", b"heic", b"mif1", b"xxxx"] {
            bytes[8..12].copy_from_slice(brand);
            assert_eq!(audio(&bytes), None);
        }
        bytes[8..12].copy_from_slice(b"M4A ");
        assert_eq!(audio(&bytes[..15]), None);
        for size in [0_u32, 1, 8, 15, 17, 25, u32::MAX] {
            bytes[..4].copy_from_slice(&size.to_be_bytes());
            assert_eq!(audio(&bytes), None);
        }
    }

    #[test]
    fn usage_preserves_zero_and_rejects_invalid_counts_and_durations() {
        for invalid in [
            Value::Null,
            json!([]),
            json!(1),
            json!({"type":"unknown"}),
            json!({"type":"duration","seconds":-1}),
            json!({"type":"duration","seconds":f64::INFINITY}),
            json!({"type":"duration","seconds":"3"}),
            json!({"type":"tokens","input_tokens":"2","output_tokens":-1,"total_tokens":1.1}),
        ] {
            assert_eq!(usage(&invalid), Value::Null);
        }
        for duration in [json!(1.25), json!(0)] {
            let value = json!({"type":"duration","seconds":duration});
            assert_eq!(usage(&value), value);
        }
        assert_eq!(
            usage(&json!({"type":"tokens","input_tokens":0}))["input_tokens"],
            0
        );
    }
}
