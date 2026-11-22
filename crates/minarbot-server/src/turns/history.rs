use super::*;

pub fn slice(s: &str, offset: usize, length: usize) -> String {
    String::from_utf16_lossy(
        &s.encode_utf16()
            .skip(offset)
            .take(length)
            .collect::<Vec<_>>(),
    )
}
fn scope(turn: &Value) -> (&'static str, Vec<rusqlite::types::Value>) {
    if let Some(channel) = turn["channel"].as_str() {
        ("channel=?", vec![channel.to_owned().into()])
    } else {
        (
            "agent=? AND channel IS NULL AND thread IS ?",
            vec![
                string(turn, "agent").to_owned().into(),
                turn["thread"]
                    .as_str()
                    .map(|s| s.to_owned().into())
                    .unwrap_or(rusqlite::types::Value::Null),
            ],
        )
    }
}
pub fn position(db: &Connection, turn: &Value) -> Result<i64> {
    Ok(db.query_row("SELECT coalesce((SELECT position FROM queue_positions WHERE turn=t.id),seq) FROM turns t WHERE id=?",[string(turn,"id")],|r|r.get(0))?)
}
pub fn context_position(db: &Connection, turn: &Value) -> Result<i64> {
    match one(
        db,
        "SELECT t.* FROM turns t JOIN room_turns r ON r.root=t.id WHERE r.turn=?",
        [string(turn, "id")],
    )? {
        Some(root) => position(db, &root),
        None => position(db, turn),
    }
}
pub fn requested_files(db: &Connection, turn: &str) -> Result<Vec<Value>> {
    let mut files = all(db,"WITH RECURSIVE results(id) AS(SELECT t.id FROM turns t JOIN threads th ON th.id=t.thread AND th.deleted=0 WHERE t.id=? UNION SELECT t.id FROM results JOIN agent_requests r ON r.turn=results.id AND r.expects_reply=1 JOIN agent_deliveries d ON d.request=r.id JOIN turns t ON t.id=d.turn JOIN threads th ON th.id=t.thread AND th.deleted=0) SELECT DISTINCT f.id,f.name,length(f.bytes) AS size,substr(f.scope,9) AS thread,f.deleted FROM(SELECT turn,file FROM generated_files UNION ALL SELECT turn,file FROM mcp_files UNION ALL SELECT turn,file FROM image_generations WHERE file IS NOT NULL) outputs JOIN files f ON f.id=outputs.file WHERE outputs.turn IN(SELECT id FROM results) AND f.scope LIKE 'threads:%' ORDER BY f.seq",[turn])?;
    for file in &mut files {
        if file["deleted"] == 1 {
            file["deleted"] = json!(true);
        } else if let Some(file) = file.as_object_mut() {
            file.remove("deleted");
        }
    }
    Ok(files)
}
pub fn readable_file(
    db: &Connection,
    turn: &Value,
    id: &str,
) -> Result<Option<crate::files::attachments::File>> {
    let (clause, mut params) = scope(turn);
    params.extend([position(db, turn)?.into(), id.to_owned().into()]);
    let sent=exists(db,&format!("SELECT 1 FROM turns t JOIN json_each(t.attachments) attachment WHERE {clause} AND coalesce((SELECT position FROM queue_positions WHERE turn=t.id),seq)<=? AND attachment.value=?"),rusqlite::params_from_iter(params))?;
    let (clause, mut params) = scope(turn);
    params.extend([position(db, turn)?.into(), id.to_owned().into()]);
    let generated=exists(db,&format!("SELECT 1 FROM (SELECT turn,file FROM generated_files UNION ALL SELECT turn,file FROM mcp_files) g JOIN turns t ON t.id=g.turn WHERE {clause} AND coalesce((SELECT position FROM queue_positions WHERE turn=t.id),t.seq)<=? AND file=?"),rusqlite::params_from_iter(params))?;
    let (clause, mut params) = scope(turn);
    params.extend([position(db, turn)?.into(), id.to_owned().into()]);
    let image=exists(db,&format!("SELECT 1 FROM image_generations g JOIN turns t ON t.id=g.turn WHERE {clause} AND coalesce((SELECT position FROM queue_positions WHERE turn=t.id),t.seq)<=? AND file=?"),rusqlite::params_from_iter(params))?;
    if !sent && !generated && !image {
        for row in all(
            db,
            "SELECT delivery,snapshot FROM agent_reply_claims WHERE consumer=?",
            [string(turn, "id")],
        )? {
            let reply = data::json(&row["snapshot"])?;
            if reply["generatedFiles"]
                .as_array()
                .is_some_and(|files| files.iter().any(|file| file["id"] == id))
                && requested_files(db, string(&row, "delivery"))?
                    .iter()
                    .any(|file| file["id"] == id && file["deleted"] != true)
            {
                if let Some(source) = one(db, "SELECT scope FROM files WHERE id=?", [id])? {
                    return crate::files::attachments::file(db, id, string(&source, "scope"))
                        .map(Some);
                }
            }
        }
        for reply in all(db,"SELECT reply.value FROM flow_tool_calls c,json_each(c.result,'$.replies') reply WHERE c.turn=? AND json_extract(c.call,'$.function.name')='send_message'",[string(turn,"id")])? {
            let reply = data::json(&reply["value"])?;
            if reply["generatedFiles"].as_array().is_some_and(|files| files.iter().any(|file| file["id"]==id)) && requested_files(db,string(&reply,"id"))?.iter().any(|file| file["id"]==id && file["deleted"]!=true) {
                if let Some(source) = one(db,"SELECT scope FROM files WHERE id=?",[id])? {
                    return crate::files::attachments::file(db,id,string(&source,"scope")).map(Some);
                }
            }
        }
        return Ok(None);
    }
    match crate::files::attachments::file(db, id, &execution::scope(turn)) {
        Ok(f) => Ok(Some(f)),
        Err(e) if e.status == 404 => Ok(None),
        Err(e) => Err(e),
    }
}
pub fn read(db: &Connection, turn: &Value, name: &str, args: &Value) -> Result<Value> {
    let (clause, mut params) = scope(turn);
    let position = position(db, turn)?;
    if name == "read_history" {
        let before = args
            .get("before")
            .cloned()
            .unwrap_or(json!(9_007_199_254_740_991i64));
        if !closed(args, &["before"]) || !integer(&before, 1, 9_007_199_254_740_991) {
            return Err(error(400, "Invalid history position."));
        }
        params.extend([
            before.as_i64().ok_or_else(Failure::storage)?.into(),
            position.into(),
        ]);
        let mut rows=all(db,&format!("SELECT id,seq,agent,text,reply,status,created FROM turns t WHERE {clause} AND seq<? AND coalesce((SELECT position FROM queue_positions WHERE turn=t.id),seq)<? ORDER BY seq DESC LIMIT 6"),rusqlite::params_from_iter(params))?;
        let next = if rows.len() > 5 {
            rows.get(4).map(|r| r["seq"].clone())
        } else {
            None
        };
        rows.truncate(5);
        for row in &mut rows {
            row["truncated"] = json!(
                string(row, "text").encode_utf16().count() > 500
                    || string(row, "reply").encode_utf16().count() > 500
            );
            row["text"] = json!(slice(string(row, "text"), 0, 500));
            if !row["reply"].is_null() {
                row["reply"] = json!(slice(string(row, "reply"), 0, 500));
            }
        }
        return Ok(json!({"messages":rows,"next_before":next}));
    }
    let offset = args.get("offset").cloned().unwrap_or(json!(0));
    if !closed(args, &["id", "offset"])
        || !uuid(string(args, "id"))
        || !integer(&offset, 0, 9_007_199_254_740_991)
    {
        return Err(error(400, "Invalid message ID or position."));
    }
    let offset = offset.as_u64().unwrap_or(0) as usize;
    if name == "read_attachment" {
        let Some(file) = readable_file(db, turn, string(args, "id"))? else {
            return Ok(
                json!({"error":"Attachment not found in sent messages in this conversation."}),
            );
        };
        if let Some(mime) = crate::files::attachments::image_type(&file.bytes) {
            return Ok(if offset > 0 {
                json!({"error":"Image offset must be zero."})
            } else {
                json!({"id":args["id"],"name":file.meta["name"],"media_type":mime,"note":"Image pixels follow this tool result in a user message."})
            });
        }
        if let Some(format) = super::transcription::audio(&file.bytes) {
            return Ok(
                json!({"id":args["id"],"name":file.meta["name"],"text":"","source":"audio","format":format,"note":"Audio file attached. Its contents have not been transcribed. Ask the user to transcribe it through the file preview before interpreting it."}),
            );
        }
        let text = std::str::from_utf8(&file.bytes)
            .map_err(|_| error(415, "Message attachment must be UTF-8 text."))?;
        return Ok(
            json!({"id":args["id"],"name":file.meta["name"],"text":slice(text,offset,8000),"offset":offset,"next_offset":if text.encode_utf16().count()>offset+8000 {Some(offset+8000)}else{None}}),
        );
    }
    params.extend([string(args, "id").to_owned().into(), position.into()]);
    let Some(mut row)=one(db,&format!("SELECT id,seq,agent,text,reply,status,created,attachments FROM turns t WHERE {clause} AND id=? AND coalesce((SELECT position FROM queue_positions WHERE turn=t.id),seq)<?"),rusqlite::params_from_iter(params))? else{return Ok(json!({"error":"Message not found in this conversation before the current request."}));};
    let length = string(&row, "text")
        .encode_utf16()
        .count()
        .max(string(&row, "reply").encode_utf16().count());
    row["text"] = json!(slice(string(&row, "text"), offset, 8000));
    row["reply"] = json!(slice(string(&row, "reply"), offset, 8000));
    row["offset"] = json!(offset);
    row["next_offset"] = json!(if length > offset + 8000 {
        Some(offset + 8000)
    } else {
        None
    });
    row["attachments"] = json!([]);
    row["generatedFiles"] = json!([]);
    row["approvals"] = json!([]);
    row["harnessApprovals"] = json!([]);
    row["questions"] = json!([]);
    Ok(row)
}

fn inspect_step(mut row: Value, secrets: &[String]) -> Result<Value> {
    let name = string(&row, "name");
    row["name"] = json!(super::diagnostics::text(name, secrets));
    let call = row.get("call").and_then(|value| data::json(value).ok());
    let result = row
        .get("result")
        .map(|value| data::json(value).unwrap_or_else(|_| value.clone()));
    if let Some(call) = call {
        let raw = &call["function"]["arguments"];
        let args = data::json(raw).unwrap_or_else(|_| raw.clone());
        row["arguments"] = json!(super::diagnostics::display(&args, secrets));
    }
    if let Some(result) = result {
        let result = if result["mailbox"].is_string() {
            json!({"message":"Private email tool result."})
        } else {
            result
        };
        row["state"] = json!(if super::diagnostics::tool_failed(&result) {
            "failed"
        } else {
            result["status"].as_str().unwrap_or("completed")
        });
        row["result"] = json!(super::diagnostics::display(&result, secrets));
    }
    if let Some(title) = row["title"].as_str() {
        row["title"] = json!(super::diagnostics::text(title, secrets));
    }
    row.as_object_mut()
        .ok_or_else(Failure::storage)?
        .remove("call");
    Ok(row)
}

pub fn inspect(
    state: &AppState,
    id: &str,
    method: &Method,
    headers: &axum::http::HeaderMap,
    query: &[(String, String)],
) -> Result<Response> {
    if headers.contains_key("authorization")
        || headers.contains_key("origin") && !data::origin(state, headers)
    {
        return Err(error(
            403,
            "Turn inspection requires the local owner in Minarbot.",
        ));
    }
    if *method != Method::GET {
        return Err(error(405, "Use GET to inspect a turn."));
    }
    if !query.is_empty() {
        return Err(error(400, "Invalid inspection parameters."));
    }
    let missing = || error(404, "Turn not found.");
    if !uuid(id) {
        return Err(missing());
    }
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    let turn = one(
        db,
        "SELECT id,agent,channel,thread,status,error FROM turns WHERE id=?",
        [id],
    )?
    .ok_or_else(missing)?;
    let (kind, scope) = if let Some(channel) = turn["channel"].as_str() {
        ("channels", channel)
    } else if let Some(thread) = turn["thread"].as_str() {
        ("threads", thread)
    } else {
        ("agents", string(&turn, "agent"))
    };
    data::conversations::owner(db, kind, scope).map_err(|failure| {
        if failure.status == 404 {
            missing()
        } else {
            failure
        }
    })?;

    let secrets = super::diagnostics::secrets(state, db)?;
    let tools = all(db,
        "WITH journal AS (
           SELECT c.seq,c.call_id,c.call,c.result,c.after_action,'Created '||f.name AS title
           FROM flow_tool_calls c LEFT JOIN generated_files g USING(turn,call_id)
           LEFT JOIN files f ON f.id=g.file WHERE c.turn=?1
           UNION ALL
           SELECT g.seq,g.call_id,g.call,json_object('id',f.id,'name',f.name),g.after_action,'Created '||f.name
           FROM generated_files g JOIN files f ON f.id=g.file WHERE g.turn=?1
           AND NOT EXISTS(SELECT 1 FROM flow_tool_calls c WHERE c.turn=g.turn AND c.call_id=g.call_id)
         )
         SELECT j.call_id AS id,json_extract(j.call,'$.function.name') AS name,j.call,j.result,j.title,
         coalesce(j.after_action,0) AS after_action,s.started,s.finished
         FROM journal j LEFT JOIN turn_step_diagnostics s ON s.turn=?1 AND s.id=j.call_id
         ORDER BY after_action,j.seq",
        [id],
    )?.into_iter().map(|row| inspect_step(row, &secrets)).collect::<Result<Vec<_>>>()?;
    let harness = all(
        db,
        "SELECT h.id,h.name,h.state,s.started,s.finished,s.arguments,s.result,coalesce(s.failed,0) AS failed FROM harness_steps h
         LEFT JOIN turn_step_diagnostics s USING(turn,id) WHERE h.turn=? ORDER BY h.rowid",
        [id],
    )?
    .into_iter()
    .map(|mut row| {
        for field in ["name", "arguments", "result"] {
            if let Some(raw) = row[field].as_str() {
                row[field] = json!(super::diagnostics::bounded(&super::diagnostics::text(
                    raw, &secrets
                )));
            }
        }
        row
    })
    .collect::<Vec<_>>();
    let mut providers = all(
        db,
        "SELECT seq,model,status,created,finished,input_tokens,output_tokens,total_tokens,
         CAST(round((julianday(finished)-julianday(created))*86400000) AS INTEGER) AS duration_ms
         FROM provider_calls WHERE turn=? ORDER BY seq",
        [id],
    )?;
    for provider in &mut providers {
        provider["model"] = json!(super::diagnostics::text(
            string(provider, "model"),
            &secrets
        ));
    }
    let stderr = one(db,
        "SELECT tail FROM harness_stderr JOIN provider_calls ON provider_calls.seq=harness_stderr.call
         WHERE turn=? ORDER BY seq DESC LIMIT 1",
        [id],
    )?;
    Ok(crate::json_response(
        200,
        json!({
            "status": turn["status"],
            "error": super::diagnostics::display(&turn["error"], &secrets),
            "tools": tools,
            "harness": harness,
            "providers": providers,
            "judgments": if state.auth.is_none() {super::goals::verdicts(db,id)?} else {vec![]},
            "engineStderr": stderr.and_then(|row| super::diagnostics::display(&row["tail"], &secrets)),
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, extract::State, http::StatusCode};

    #[tokio::test]
    async fn inspect_route_checks_access_and_redacts_failed_runs() -> crate::db::Result<()> {
        let path = std::env::temp_dir().join(format!("minarbot-inspect-{}", uuid::Uuid::new_v4()));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(path.clone());
        let config: crate::Config = serde_json::from_value(
            json!({"database":path.join("inspect.sqlite"),"scriptToken":"fixture-configured-value"}),
        )?;
        let database = crate::db::open(&config.database, false, 0)?;
        crate::files::init(&database.connection)?;
        data::initialize(&database)?;
        super::super::initialize(&database.connection).map_err(|failure| failure.message)?;
        let db = &database.connection;
        let id = uuid::Uuid::new_v4().to_string();
        db.execute_batch(
            "INSERT INTO agents(id,name,model,instructions) VALUES('a','Agent','','');
             INSERT INTO threads(id,agent,name) VALUES('thread','a','Thread');",
        )?;
        db.execute(
            "INSERT INTO turns(id,agent,thread,text,status,error,created) VALUES(?,'a','thread','','failed',?,'2026-11-19T00:00:00.000Z')",
            params![id, "Engine stopped: fixture-configured-value"],
        )?;
        let args =
            json!({"password":"value-with spaces","command":"echo fixture-configured-value"});
        let call = json!({"function":{"name":"run_shell","arguments":args.to_string()}});
        let result =
            json!({"exitCode":127,"output":"missing command\nBearer fixture-unconfigured-value"});
        db.execute(
            "INSERT INTO flow_tool_calls(turn,call_id,call,result,after_output) VALUES(?,'shell',?,?,0)",
            params![id, call.to_string(), result.to_string()],
        )?;
        db.execute(
            "INSERT INTO harness_steps(turn,id,name,state) VALUES(?,'shell-native','run_shell','completed')",
            [&id],
        )?;
        db.execute(
            "INSERT INTO turn_step_diagnostics(turn,id,started,finished) VALUES(?,'shell-native','2026-11-19T00:00:00.100Z','2026-11-19T00:00:00.200Z')",
            [&id],
        )?;
        db.execute(
            "INSERT INTO provider_calls(turn,agent,model,origin,purpose,status,created,finished,input_tokens,output_tokens,total_tokens)
             VALUES(?,'a','fixture-model','local-harness','completion','failed','2026-11-19T00:00:00.000Z','2026-11-19T00:00:00.500Z',12,3,15)",
            [&id],
        )?;
        db.execute(
            "INSERT INTO harness_stderr VALUES(1,'Exit: fixture-configured-value')",
            [],
        )?;

        let computer = std::sync::Arc::new(crate::computer::Manager::new(&config)?);
        let state = AppState {
            database: std::sync::Arc::new(Mutex::new(database)),
            config: std::sync::Arc::new(config),
            origin: "http://127.0.0.1:4423".into(),
            auth: None,
            events: crate::events::Hub::new(),
            turns: super::super::Runtime::default(),
            whatsapp: Default::default(),
            trial_pending: Default::default(),
            computer,
        };
        let route = format!("/api/turns/{id}/inspect");
        let send = |method, header: Option<(&str, &str)>| -> axum::http::Result<_> {
            let mut request = Request::builder()
                .method(method)
                .uri(&route)
                .header("host", "127.0.0.1:4423");
            if let Some((name, value)) = header {
                request = request.header(name, value);
            }
            Ok(crate::dispatch(
                State(state.clone()),
                request.body(Body::empty())?,
            ))
        };
        let failed = send(Method::GET, Some(("origin", &state.origin)))?.await;
        assert_eq!(failed.status(), StatusCode::OK);
        assert_eq!(failed.headers()["cache-control"], "no-store");
        let bytes = axum::body::to_bytes(failed.into_body(), 64 * 1024).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["status"], "failed");
        assert_eq!(body["tools"][0]["state"], "failed");
        assert!(string(&body["tools"][0], "result").contains("missing command"));
        assert!(string(&body["tools"][0], "arguments").contains("[REDACTED]"));
        assert_eq!(body["harness"][0]["state"], "completed");
        assert_eq!(body["harness"][0]["finished"], "2026-11-19T00:00:00.200Z");
        assert_eq!(body["providers"][0]["duration_ms"], 500);
        assert_eq!(body["providers"][0]["total_tokens"], 15);
        assert!(string(&body, "engineStderr").contains("[REDACTED]"));
        let wire = String::from_utf8(bytes.to_vec())?;
        for secret in [
            "fixture-configured-value",
            "value-with spaces",
            "fixture-unconfigured-value",
        ] {
            assert!(!wire.contains(secret), "synthetic secret exposed: {secret}");
        }
        for header in [
            ("origin", "https://foreign.invalid"),
            ("authorization", "Bearer agent"),
        ] {
            assert_eq!(
                send(Method::GET, Some(header))?.await.status(),
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(
            send(Method::POST, None)?.await.status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
        {
            let guard = state
                .database
                .lock()
                .map_err(|_| "fixture database lock poisoned")?;
            guard
                .connection
                .execute("UPDATE threads SET deleted=1 WHERE id='thread'", [])?;
        }
        assert_eq!(
            send(Method::GET, None)?.await.status(),
            StatusCode::NOT_FOUND
        );
        Ok(())
    }
}
