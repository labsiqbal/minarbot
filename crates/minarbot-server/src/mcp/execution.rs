use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
const UNCERTAIN:&str="MCP result is uncertain. Check effects on the target server. This call is not repeated; MCP tools are blocked for this task. Start a new message only after checking the result.";
fn project(db: &Connection, turn: &Value) -> Result<String> {
    Ok(one(db,"SELECT project FROM local_project_threads WHERE thread=? UNION SELECT project FROM channels WHERE id=?",params![turn["thread"].as_str(),turn["channel"].as_str()])?.map(|v|string(&v,"project").to_owned()).unwrap_or_default())
}
pub fn specs() -> Result<Vec<Value>> {
    serde_json::from_str(include_str!("client-tools.json")).map_err(|_| Failure::storage())
}
pub async fn run(state: &AppState, turn: &Value, call: &Value, args: &Value) -> Result<Value> {
    if call["function"]["name"] == "list_mcp_tools" {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        let project = project(db, turn)?;
        connections::member(db, &project, string(turn, "agent"))?;
        if !closed(args, &["after"]) {
            return Err(error(400, "Unknown MCP field."));
        }
        let after = string(args, "after");
        if !after.is_empty() && !uuid(after) {
            return Err(error(400, "Invalid MCP position."));
        }
        let rows=all(db,"SELECT c.*,g.tools AS permitted FROM mcp_connections c JOIN mcp_grants g ON g.connection=c.id WHERE c.enabled=1 AND g.agent=? AND coalesce((SELECT project FROM mcp_projects WHERE connection=c.id),'')=? AND c.id>? AND g.tools<>'[]' ORDER BY c.id LIMIT 2",[string(turn,"agent"),&project,after])?;
        let mut out = vec![];
        for row in rows.iter().take(1) {
            let grant = data::json(&row["permitted"])?;
            let tools = data::json(&row["tools"])?;
            out.push(json!({"id":row["id"],"name":row["name"],"url":row["url"],"enabled":row["enabled"],"revision":row["revision"],"hasToken":!string(row,"token").is_empty(),"tools":tools.as_array().ok_or_else(Failure::storage)?.iter().filter(|t|super::apps::visible(t, "model") && grant.as_array().is_some_and(|g|g.contains(&t["name"]))).collect::<Vec<_>>()}));
        }
        return Ok(
            json!({"connections":out,"next_after":if rows.len()>1{rows[0]["id"].clone()}else{Value::Null}}),
        );
    }
    if !closed(args, &["connection", "name", "arguments"])
        || !uuid(string(args, "connection"))
        || !args["name"].is_string()
        || !args["arguments"].is_object()
        || args["arguments"].to_string().len() > 32000
    {
        return Err(error(400, "Invalid MCP tool arguments."));
    }
    let row = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        if data::exists(
            db,
            "SELECT 1 FROM mcp_calls WHERE turn=? AND uncertain=1",
            [string(turn, "id")],
        )? {
            return Err(error(409, UNCERTAIN));
        }
        permitted(db, turn, args)?
    };
    let url = string(&row, "url");
    let token = client::decrypt(state, string(&row, "token"), url)?;
    let mut session = client::Session::open(url, &token).await?;
    let name = string(args, "name");
    let tools = connections::clean_tools(&session.catalogue().await?, &token)?;
    let expected = data::json(&row["tools"])?;
    let tool=tools.as_array().and_then(|a|a.iter().find(|v|v["name"]==name)).ok_or_else(||error(409,"MCP tool definition changed. Test a connection update before granting access again."))?;
    if expected
        .as_array()
        .and_then(|a| a.iter().find(|v| v["name"] == name))
        != Some(tool)
    {
        return Err(error(
            409,
            "MCP tool definition changed. Test a connection update before granting access again.",
        ));
    }
    if !super::validate::valid(&tool["inputSchema"], &args["arguments"])
        || tool["execution"]["taskSupport"] == "required"
    {
        return Err(error(
            400,
            "Arguments do not match the MCP tool schema. Call has not been sent.",
        ));
    }
    {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        let current = permitted(db, turn, args)?;
        if current["revision"] != row["revision"]
            || current["grant_revision"] != row["grant_revision"]
        {
            return Err(error(
                409,
                "MCP permissions or task changed before sending.",
            ));
        }
        transaction(db, || {
            db.execute(
                "INSERT INTO mcp_calls VALUES(?,?,1)",
                [string(turn, "id"), string(call, "id")],
            )?;
            crate::turns::harness::journal(
                db,
                turn,
                call,
                &json!({"error":UNCERTAIN,"uncertain":true}),
            )
        })?;
    }
    let response = {
        let future = session.call(tool, &args["arguments"]);
        tokio::pin!(future);
        loop {
            tokio::select! {
             result=&mut future=>break result.map_err(|_|error(502,UNCERTAIN))?,
             _=tokio::time::sleep(std::time::Duration::from_millis(100))=>{let guard=state.database.lock().map_err(|_|Failure::storage())?;let current=permitted(&guard.connection,turn,args).map_err(|_|error(502,UNCERTAIN))?;if current["revision"]!=row["revision"]||current["grant_revision"]!=row["grant_revision"]{return Err(error(502,UNCERTAIN));}}
            }
        }
    };
    let snapshot = super::apps::discover(
        state,
        turn,
        &mut session,
        string(&row, "name"),
        tool,
        &args["arguments"],
        &response,
    )
    .await;
    let (mut result, files) = content(&response, &token).map_err(|_| error(502, UNCERTAIN))?;
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    let current = permitted(db, turn, args).map_err(|_| error(502, UNCERTAIN))?;
    if current["revision"] != row["revision"] || current["grant_revision"] != row["grant_revision"]
    {
        return Err(error(502, UNCERTAIN));
    }
    for secret in secrets::project(state, db, Some(&project(db, turn)?))? {
        if files
            .iter()
            .any(|(_, bytes, _, _, _)| secrets::reflected(bytes, &secret))
        {
            return Err(error(502, UNCERTAIN));
        }
        result = connections::redact(&result, &secret);
    }
    transaction(db, || {
        let count:i64=db.query_row("SELECT (SELECT count(*) FROM generated_files WHERE turn=?)+(SELECT count(*) FROM mcp_files WHERE turn=?)+(SELECT count(*) FROM image_generations WHERE turn=? AND file IS NOT NULL)",[string(turn,"id");3],|r|r.get(0))?;
        if count + files.len() as i64 > 8 {
            return Err(error(502, UNCERTAIN));
        }
        let mut metas = vec![];
        for (index, bytes, name, mime, uri) in files {
            let id = uuid::Uuid::new_v4().to_string();
            let scope = if let Some(thread) = turn["thread"].as_str() {
                format!("threads:{thread}")
            } else if let Some(channel) = turn["channel"].as_str() {
                format!("channels:{channel}")
            } else {
                format!("agents:{}", string(turn, "agent"))
            };
            db.execute(
                "INSERT INTO files(id,scope,name,digest,bytes,created) VALUES(?,?,?,?,?,?)",
                params![
                    id,
                    scope,
                    name,
                    registry::hash_bytes(&bytes),
                    bytes,
                    data::iso(db, state.config.now())?
                ],
            )?;
            db.execute(
                "INSERT INTO mcp_files VALUES(?,?)",
                [string(turn, "id"), &id],
            )?;
            let mut meta = json!({"id":id,"contentIndex":index,"name":name,"size":bytes.len(),"mimeType":mime});
            if let Some(uri) = uri {
                meta["uri"] = json!(uri);
            }
            metas.push(meta);
        }
        if let Some(snapshot) = snapshot {
            let displayed =
                super::apps::save(state, db, string(turn, "id"), &row, &token, snapshot)?;
            result["appDisplayed"] = json!(displayed);
        }
        result["files"] = json!(metas);
        result = connections::redact(&result, &token);
        for secret in secrets::project(state, db, Some(&project(db, turn)?))? {
            result = connections::redact(&result, &secret);
        }
        if result.to_string().len() > 128000 {
            return Err(error(502, UNCERTAIN));
        }
        db.execute(
            "UPDATE flow_tool_calls SET result=? WHERE turn=? AND call_id=?",
            [
                result.to_string(),
                string(turn, "id").to_owned(),
                string(call, "id").to_owned(),
            ],
        )?;
        db.execute(
            "UPDATE mcp_calls SET uncertain=0 WHERE turn=? AND call=?",
            [string(turn, "id"), string(call, "id")],
        )?;
        Ok(result)
    })
}
fn permitted(db: &Connection, turn: &Value, args: &Value) -> Result<Value> {
    let row = granted(db, turn, string(args, "connection"), string(args, "name"))?;
    if !data::json(&row["tools"])?.as_array().is_some_and(|tools| {
        tools
            .iter()
            .any(|tool| tool["name"] == args["name"] && super::apps::visible(tool, "model"))
    }) {
        return Err(error(403, "MCP tool is unavailable to the model."));
    }
    if !data::exists(
        db,
        "SELECT 1 FROM turns WHERE id=? AND agent=? AND status='running'",
        [string(turn, "id"), string(turn, "agent")],
    )? {
        return Err(error(
            409,
            "MCP permissions or task changed before sending.",
        ));
    }
    Ok(row)
}
pub(super) fn granted(db: &Connection, turn: &Value, id: &str, name: &str) -> Result<Value> {
    let scope = project(db, turn)?;
    connections::member(db, &scope, string(turn, "agent"))?;
    let row=one(db,"SELECT c.*,g.revision grant_revision,g.tools permitted FROM mcp_connections c JOIN mcp_grants g ON g.connection=c.id WHERE c.id=? AND g.agent=? AND c.enabled=1 AND coalesce((SELECT project FROM mcp_projects WHERE connection=c.id),'')=?",[id,string(turn,"agent"),&scope])?.filter(|v|data::json(&v["permitted"]).ok().and_then(|v|v.as_array().cloned()).is_some_and(|a|a.iter().any(|v| v == name))).ok_or_else(||error(403,"MCP tool is not authorized for this agent yet."))?;
    Ok(row)
}
type File = (usize, Vec<u8>, String, String, Option<String>);
fn content(v: &Value, token: &str) -> Result<(Value, Vec<File>)> {
    let mut text = vec![];
    let mut files = vec![];
    let mut omitted = 0;
    let mut total = 0;
    for (index, item) in v["content"]
        .as_array()
        .ok_or_else(Failure::storage)?
        .iter()
        .enumerate()
    {
        if super::apps::is_ui_resource(item) {
            continue;
        }
        if item["type"] == "text" {
            text.push(json!({"type":"text","contentIndex":index,"text":connections::redact(&item["text"],token)}));
            continue;
        }
        let resource = &item["resource"];
        if item["type"] == "resource" && resource["text"].is_string() {
            text.push(json!({"type":"text","contentIndex":index,"text":connections::redact(resource,token).to_string()}));
            continue;
        }
        let data = if item["type"] == "resource" {
            resource["blob"].as_str()
        } else if item["type"] == "image" || item["type"] == "audio" {
            item["data"].as_str()
        } else {
            None
        };
        let Some(data) = data else {
            omitted += 1;
            continue;
        };
        let bytes = STANDARD.decode(data).map_err(|_| Failure::storage())?;
        total += bytes.len();
        if STANDARD.encode(&bytes) != data || total > 128000 || files.len() >= 8 {
            return Err(Failure::storage());
        }
        if !token.is_empty() {
            let utf16 = token
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>();
            if [
                token.as_bytes(),
                utf16.as_slice(),
                STANDARD.encode(token).as_bytes(),
            ]
            .iter()
            .any(|s| !s.is_empty() && bytes.windows(s.len()).any(|v| v == *s))
            {
                return Err(Failure::storage());
            }
        }
        let mime = resource["mimeType"]
            .as_str()
            .or(item["mimeType"].as_str())
            .unwrap_or("application/octet-stream");
        let ext = match mime {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            "audio/wav" => "wav",
            "audio/mpeg" => "mp3",
            "application/pdf" => "pdf",
            "text/plain" => "txt",
            _ => "bin",
        };
        files.push((
            index,
            bytes,
            format!("mcp-{}.{ext}", files.len() + 1),
            mime.into(),
            resource["uri"].as_str().map(str::to_owned),
        ));
    }
    Ok((
        json!({"content":text,"files":[],"structuredContent":connections::redact(&v["structuredContent"],token),"isError":v["isError"]==true,"omitted":omitted}),
        files,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_preserves_bounded_files_and_never_resolves_resource_uris() -> Result<()> {
        let bytes = [0, 255, 128];
        let encoded = STANDARD.encode(bytes);
        let (result, files) = content(
            &json!({"content":[
                {"type":"resource","resource":{"uri":"file:///fixture/never-opened","blob":encoded}},
                {"type":"audio","mimeType":"audio/wav","data":encoded},
                {"type":"resource","resource":{"uri":"fixture://text","text":"untrusted"}},
                {"type":"resource_link","uri":"https://example.test/file","name":"link"}
            ]}),
            "secret",
        )?;
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].1, bytes);
        assert_eq!(files[0].2, "mcp-1.bin");
        assert_eq!(files[0].4.as_deref(), Some("file:///fixture/never-opened"));
        assert_eq!(files[1].2, "mcp-2.wav");
        assert_eq!(result["omitted"], 1);
        let resource: Value = serde_json::from_str(
            result["content"][0]["text"]
                .as_str()
                .ok_or_else(Failure::storage)?,
        )
        .map_err(|_| Failure::storage())?;
        assert_eq!(resource["text"], "untrusted");
        for data in [
            "not base64".into(),
            "AB==".into(),
            "AA".into(),
            STANDARD.encode(vec![0; 128001]),
        ] {
            assert!(content(
                &json!({"content":[{"type":"image","mimeType":"image/png","data":data}]}),
                ""
            )
            .is_err());
        }
        for secret in [
            b"secret".to_vec(),
            "secret".encode_utf16().flat_map(u16::to_le_bytes).collect(),
            STANDARD.encode("secret").into_bytes(),
        ] {
            assert!(content(&json!({"content":[{"type":"resource","resource":{"uri":"fixture://private","blob":STANDARD.encode(secret)}}]}), "secret").is_err());
        }
        let image = json!({"type":"image","mimeType":"image/png","data":encoded});
        assert!(content(&json!({"content":vec![image; 9]}), "").is_err());
        for mime in ["__proto__", "constructor", "toString"] {
            let (_, files) = content(
                &json!({"content":[{"type":"image","mimeType":mime,"data":encoded}]}),
                "",
            )?;
            assert_eq!(files[0].2, "mcp-1.bin");
        }
        Ok(())
    }
}
