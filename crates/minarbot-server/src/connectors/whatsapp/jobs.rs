use super::{
    protocol::{Attachment, Input, MediaKind, Session},
    *,
};
use crate::{data, files::attachments, media::transcription};
use axum::{body::Body, extract::Request};

const SEND_INTERVAL_MS: i64 = 3_000;
const OUTPUT_CHUNK: usize = 3_800;

pub(super) fn target_agent(db: &Connection, kind: &str, target: &str) -> Result<String> {
    match kind {
        "agents" if exists(db, "SELECT 1 FROM agents WHERE id=?", [target])?
            && !data::agents::hidden(db, target)? => Ok(target.into()),
        "channels" => one(db, "SELECT m.agent FROM channels c JOIN channel_members m ON m.channel=c.id LEFT JOIN agent_visibility v ON v.agent=m.agent WHERE c.id=? AND c.archived=0 AND coalesce(v.hidden,0)=0 ORDER BY m.agent IS NOT c.lead,m.agent LIMIT 1", [target])?
            .map(|r| string(&r,"agent").to_owned())
            .ok_or_else(|| error(409,"Project section needs an available agent.")),
        _ => Err(error(404,"WhatsApp destination is unavailable.")),
    }
}

pub(super) fn record(state: &AppState, db: &Connection, input: Input) -> Result<()> {
    let now = state.config.now();
    db.execute("INSERT INTO whatsapp_chats(chat,label,seen) VALUES(?,?,?) ON CONFLICT(chat) DO UPDATE SET label=excluded.label,seen=excluded.seen",
        params![input.chat,input.label,now])?;
    db.execute("DELETE FROM whatsapp_chats WHERE chat IN(SELECT chat FROM whatsapp_chats WHERE kind IS NULL ORDER BY seen DESC LIMIT -1 OFFSET 100)",[])?;
    let Some(route) = one(
        db,
        "SELECT kind,target,revision FROM whatsapp_chats WHERE chat=? AND kind IS NOT NULL",
        [&input.chat],
    )?
    else {
        return Ok(());
    };
    if exists(
        db,
        "SELECT 1 FROM whatsapp_inbox WHERE chat=? AND sender=? AND message=?",
        params![input.chat, input.sender, input.message],
    )? {
        return Ok(());
    }
    let pending: i64 = db.query_row(
        "SELECT count(*) FROM whatsapp_inbox WHERE chat=? AND status IN ('queued','running')",
        [&input.chat],
        |r| r.get(0),
    )?;
    let total: i64 = db.query_row(
        "SELECT count(*) FROM whatsapp_inbox WHERE status IN ('queued','running')",
        [],
        |r| r.get(0),
    )?;
    if pending >= 20 || total >= 100 {
        return Ok(());
    }
    let id = super::super::runner::new_id();
    let media = input
        .media
        .as_ref()
        .map(|media| store::encode(state, &format!("attachment:{id}"), media))
        .transpose()?;
    db.execute("INSERT INTO whatsapp_inbox(chat,sender,message,id,route,kind,target,text,quoted,media,upload,created) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
        params![input.chat,input.sender,input.message,id,route["revision"].as_str(),route["kind"].as_str(),route["target"].as_str(),input.text,input.quoted,media,super::super::runner::new_id(),now])?;
    Ok(())
}

fn allowed(db: &Connection, generation: &str, row: &Value) -> Result<bool> {
    Ok(epoch_active(db, generation)?
        && exists(
            db,
            "SELECT 1 FROM whatsapp_chats WHERE chat=? AND revision=?",
            [string(row, "chat"), string(row, "route")],
        )?)
}

fn path(row: &Value) -> String {
    if row["kind"] == "agents" {
        format!("/api/threads/{}", string(row, "route"))
    } else {
        format!("/api/channels/{}", string(row, "target"))
    }
}

fn request(state: &AppState, path: &str, body: Body, content_type: &str) -> Result<Request> {
    let host = state.origin.split_once("://").map_or("", |(_, host)| host);
    Request::builder()
        .uri(path)
        .method(Method::POST)
        .header("host", host)
        .header("origin", &state.origin)
        .header("content-type", content_type)
        .body(body)
        .map_err(|_| Failure::storage())
}

async fn json_response(response: Response) -> Result<Value> {
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(error(status, "Minarbot rejected the WhatsApp request."));
    }
    let bytes = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .map_err(|_| Failure::storage())?;
    serde_json::from_slice(&bytes).map_err(|_| Failure::storage())
}

async fn turn_command(state: &AppState, path: &str, body: Value) -> Result<Value> {
    let req = request(
        state,
        path,
        Body::from(body.to_string()),
        "application/json",
    )?;
    json_response(crate::turns::local(state, req).await?).await
}

use crate::automation::connector_workflow_command as workflow_command;

async fn media_input(
    state: &AppState,
    session: &dyn Session,
    generation: &str,
    row: &Value,
) -> Result<(String, Option<String>)> {
    let Some(ciphertext) = row["media"].as_str() else {
        return Ok((string(row, "text").into(), None));
    };
    let id = string(row, "id");
    let upload = string(row, "upload");
    let base = path(row);
    let parts: Vec<_> = base.split('/').skip(1).collect();
    let (kind, owner) = (parts[1], parts[2]);
    let scope = format!("{kind}:{owner}");
    let prior = {
        let db = lock(state)?;
        exists(
            &db.connection,
            "SELECT 1 FROM files WHERE id=? AND scope=? AND deleted=0",
            [upload, &scope],
        )?
    };
    if !prior {
        let descriptor: Vec<u8> = store::decode(state, &format!("attachment:{id}"), ciphertext)?;
        let file = session.download(&descriptor).await?;
        if file.bytes.len() > protocol::media_limit(file.kind) {
            return Err(error(413, "WhatsApp attachment exceeds its size limit."));
        }
        if file.kind == MediaKind::Audio {
            if transcription::audio(&file.bytes).is_none() {
                return Err(error(415, "WhatsApp voice note has an unsupported format."));
            }
            transcription_provider(state, row)?;
        } else {
            super::super::media::validate(&file.bytes)?;
        }
        let upload_path = format!("{base}/files/{upload}");
        let mut req = request(
            state,
            &upload_path,
            Body::from(file.bytes),
            "application/octet-stream",
        )?;
        req.headers_mut().insert(
            "x-file-name",
            attachments::encode_name(&file.name)
                .parse()
                .map_err(|_| Failure::storage())?,
        );
        {
            let db = lock(state)?;
            if !allowed(&db.connection, generation, row)? {
                return Err(error(409, "WhatsApp mapping changed."));
            }
        }
        json_response(
            attachments::handle(state, req, &["api", kind, owner, "files", upload]).await?,
        )
        .await?;
    }
    let is_audio = {
        let db = lock(state)?;
        if !allowed(&db.connection, generation, row)? {
            return Err(error(409, "WhatsApp mapping changed."));
        }
        transcription::audio(&attachments::file(&db.connection, upload, &scope)?.bytes).is_some()
    };
    if !is_audio {
        return Ok((string(row, "text").into(), Some(upload.into())));
    }
    let provider = transcription_provider(state, row)?;
    let input = json!({"id":id,"consent":true,"provider":provider});
    let req = request(
        state,
        &format!("{base}/files/{upload}?transcription=1"),
        Body::from(input.to_string()),
        "application/json",
    )?;
    let operation = json_response(
        transcription::handle(state, req, &["api", kind, owner, "files", upload]).await?,
    )
    .await?;
    if operation["status"] != "done" {
        return Err(error(422, "Voice transcription could not be confirmed."));
    }
    let transcript = string(&operation["result"], "id");
    let db = lock(state)?;
    let file = attachments::file(&db.connection, transcript, &scope)?;
    let text = std::str::from_utf8(&file.bytes).map_err(|_| Failure::storage())?;
    // Large transcripts stay attached so the user can recover the full result in Minarbot.
    Ok((
        if text.encode_utf16().count() <= 16_000 {
            text.into()
        } else {
            "Read this voice note transcript.".into()
        },
        Some(transcript.into()),
    ))
}

fn transcription_provider(state: &AppState, row: &Value) -> Result<String> {
    let db = lock(state)?;
    let current = transcription::destination(state)?;
    let consent = one(
        &db.connection,
        "SELECT transcribe_provider FROM whatsapp_chats WHERE chat=? AND revision=?",
        [string(row, "chat"), string(row, "route")],
    )?;
    if !current["id"].is_string()
        || consent.is_none_or(|value| value["transcribe_provider"] != current["id"])
    {
        return Err(error(
            422,
            "Enable voice transcription for this chat in Minarbot.",
        ));
    }
    Ok(string(&current, "id").into())
}

fn queue(
    db: &Connection,
    input: &str,
    slot: &str,
    text: &str,
    file: Option<&str>,
) -> Result<String> {
    use whatsapp_rust::wacore::{request::RequestUtils, time::now_secs_u64};
    let id = RequestUtils::message_id_at(None, now_secs_u64());
    db.execute(
        "INSERT INTO whatsapp_outbox(id,input,slot,text,file) VALUES(?,?,?,?,?) ON CONFLICT(input,slot) DO NOTHING",
        params![id, input, slot, text, file],
    )?;
    Ok(id)
}

fn finish(db: &Connection, input: &str, text: &str, files: &[Value]) -> Result<()> {
    transaction(db, || {
        for (part, text) in super::super::media::chunks(text, OUTPUT_CHUNK, false)
            .iter()
            .enumerate()
        {
            queue(db, input, &format!("reply:{part}"), text, None)?;
        }
        for file in files.iter().filter(|file| file["deleted"] != true) {
            let id = string(file, "id");
            let name = string(file, "name");
            if file["size"].as_u64().is_none_or(|n| n > MAX_MEDIA as u64) {
                queue(
                    db,
                    input,
                    &format!("file:{id}"),
                    &format!("File too large for WhatsApp. Open {name} in Minarbot."),
                    None,
                )?;
            } else {
                queue(db, input, &format!("file:{id}"), name, Some(id))?;
            }
        }
        db.execute(
            "UPDATE whatsapp_inbox SET status='done',media=NULL WHERE id=?",
            [input],
        )?;
        Ok(())
    })
}

fn prompt(
    db: &Connection,
    row: &Value,
    kind: &str,
    subject: &str,
    revision: Option<i64>,
    text: &str,
    choices: &[Value],
) -> Result<()> {
    let input = string(row, "id");
    let slot = format!("prompt:{kind}:{subject}:{}", revision.unwrap_or(0));
    if exists(
        db,
        "SELECT 1 FROM whatsapp_outbox WHERE input=? AND slot=?",
        [input, &slot],
    )? {
        return Ok(());
    }
    let mut notice: String = text.chars().take(1700).collect();
    if choices.is_empty() {
        notice.push_str("\n\nQuote this message to reply.");
    } else {
        for (index, choice) in choices.iter().enumerate() {
            let choice: String = choice.as_str().unwrap_or("").chars().take(120).collect();
            notice.push_str(&format!("\n{}. {choice}", index + 1));
        }
        notice.push_str("\n\nQuote this message and reply with the number.");
    }
    transaction(db, || {
        let out = queue(db, input, &slot, &notice, None)?;
        db.execute("INSERT INTO whatsapp_prompts(notice,prompt_type,subject,revision,choices) VALUES(?,?,?,?,?)",
            params![out,kind,subject,revision,json!(choices).to_string()])?;
        Ok(())
    })
}

async fn answer(state: &AppState, row: &Value) -> Result<bool> {
    let Some(quoted) = row["quoted"].as_str() else {
        return Ok(false);
    };
    let decision = {
        let db = lock(state)?;
        one(&db.connection,"SELECT p.*,i.id AS original,i.sender,i.route,i.chat,o.status FROM whatsapp_prompts p JOIN whatsapp_outbox o ON o.id=p.notice JOIN whatsapp_inbox i ON i.id=o.input WHERE p.notice=? AND i.chat=?",
            [quoted,string(row,"chat")])?
    };
    let Some(decision) = decision else {
        return Ok(false);
    };
    if decision["route"] != row["route"]
        || decision["sender"] != row["sender"]
        || decision["answered"] != 0
        || !matches!(string(&decision, "status"), "sent" | "uncertain")
        || row["media"].is_string()
    {
        return Ok(true);
    }
    let choices = data::json(&decision["choices"])?;
    let text = string(row, "text");
    let index = text.parse::<usize>().ok().and_then(|n| n.checked_sub(1));
    let selection = if choices.as_array().is_some_and(Vec::is_empty) {
        Some(text)
    } else {
        index.and_then(|i| choices.get(i)).and_then(Value::as_str)
    };
    let Some(selection) = selection else {
        return Ok(true);
    };
    let original = string(&decision, "original");
    let id = string(row, "id");
    if decision["prompt_type"] == "workflow" {
        workflow_command(
            state,
            &format!("/api/workflow-runs/{original}/approve"),
            &json!({"id":id,"revision":decision["revision"],"accepted":index == Some(0)}),
        )?;
    } else {
        turn_command(
            state,
            &format!(
                "{}/turns/{original}/questions/{}/answer",
                path(row),
                string(&decision, "subject")
            ),
            json!({"id":id,"text":selection}),
        )
        .await?;
    }
    let db = lock(state)?;
    db.connection.execute(
        "UPDATE whatsapp_prompts SET answered=1 WHERE notice=? AND answered=0",
        [quoted],
    )?;
    finish(&db.connection, id, "Decision received.", &[])?;
    Ok(true)
}

fn command(state: &AppState, row: &Value) -> Result<bool> {
    if row["media"].is_string() {
        return Ok(false);
    }
    let text = string(row, "text");
    let parts: Vec<_> = text.splitn(4, ' ').collect();
    let verb = parts[0];
    if !matches!(verb, "/workflows" | "/workflow" | "/stop") {
        return Ok(false);
    }
    let id = string(row, "id");
    let agent = {
        let db = lock(state)?;
        target_agent(&db.connection, string(row, "kind"), string(row, "target"))?
    };
    match parts.as_slice() {
        ["/workflows"] | ["/workflows", _] => {
            let cursor = parts.get(1).copied().unwrap_or("");
            if !cursor.is_empty() && !uuid(cursor) {
                return Err(error(400, "Use /workflows or /workflows CURSOR."));
            }
            let db = lock(state)?;
            let entries = all(&db.connection, "SELECT id,name,revision FROM workflows WHERE agent=? AND deleted=0 AND id>? ORDER BY id LIMIT 21", [&agent,cursor])?;
            let mut menu = entries
                .iter()
                .take(20)
                .map(|w| {
                    format!(
                        "{}\n/workflow {} {} input",
                        string(w, "name"),
                        string(w, "id"),
                        w["revision"]
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            if menu.is_empty() {
                menu = "No workflows available.".into();
            }
            if entries.len() > 20 {
                menu.push_str(&format!(
                    "\n\nNext page: /workflows {}",
                    string(&entries[19], "id")
                ));
            }
            finish(&db.connection, id, &menu, &[])?;
        }
        ["/workflow", workflow, revision, input] if uuid(workflow) && !input.is_empty() => {
            let revision: i64 = revision
                .parse()
                .ok()
                .filter(|n| (1..=9_007_199_254_740_991).contains(n))
                .ok_or_else(|| error(400, "Choose a workflow with /workflows."))?;
            {
                let db = lock(state)?;
                if !exists(
                    &db.connection,
                    "SELECT 1 FROM workflows WHERE id=? AND agent=? AND deleted=0",
                    [*workflow, &agent],
                )? {
                    return Err(error(404, "Choose a workflow with /workflows."));
                }
            }
            workflow_command(
                state,
                &format!("/api/workflows/{workflow}/run"),
                &json!({"id":id,"revision":revision,"input":input}),
            )?;
            let db = lock(state)?;
            transaction(&db.connection, || {
                db.connection.execute(
                    "UPDATE whatsapp_inbox SET status='running',workflow=1 WHERE id=?",
                    [id],
                )?;
                queue(
                    &db.connection,
                    id,
                    "workflow-start",
                    &format!("Workflow started. /stop {id}"),
                    None,
                )?;
                Ok(())
            })?;
        }
        ["/stop", run] if uuid(run) => {
            let status = {
                let db = lock(state)?;
                one(&db.connection,"SELECT w.revision,w.status FROM workflow_runs w JOIN whatsapp_inbox i ON i.id=w.id WHERE i.id=? AND i.chat=? AND i.sender=? AND i.route=?",
                    [*run,string(row,"chat"),string(row,"sender"),string(row,"route")])?
            };
            let status =
                status.ok_or_else(|| error(404, "Workflow run not found for this requester."))?;
            workflow_command(
                state,
                &format!("/api/workflow-runs/{run}/cancel"),
                &json!({"id":id,"revision":status["revision"]}),
            )?;
            let db = lock(state)?;
            finish(&db.connection, id, "Workflow cancelled.", &[])?;
        }
        _ => return Err(error(400, "Use /workflows to find a workflow.")),
    }
    Ok(true)
}

async fn submit(
    state: &AppState,
    generation: &str,
    row: &Value,
    session: &dyn Session,
) -> Result<()> {
    let id = string(row, "id");
    if answer(state, row).await? || command(state, row)? {
        let db = lock(state)?;
        db.connection.execute(
            "UPDATE whatsapp_inbox SET status='done',media=NULL WHERE id=? AND status='queued'",
            [id],
        )?;
        return Ok(());
    }
    {
        let db = lock(state)?;
        target_agent(&db.connection, string(row, "kind"), string(row, "target"))?;
        if row["kind"] == "agents" {
            let thread = string(row, "route");
            if !exists(&db.connection, "SELECT 1 FROM threads WHERE id=?", [thread])? {
                let conversation = json!({"id":thread,"name":"WhatsApp"});
                transaction(&db.connection, || {
                    data::conversations::threads(
                        &db.connection,
                        string(row, "target"),
                        &conversation,
                        false,
                    )
                })?;
            }
        }
    }
    let (text, file) = media_input(state, session, generation, row).await?;
    {
        let db = lock(state)?;
        if !allowed(&db.connection, generation, row)? {
            return Ok(());
        }
    }
    let files: Vec<_> = file.into_iter().collect();
    turn_command(
        state,
        &format!("{}/turns", path(row)),
        json!({"id":id,"text":text,"files":files}),
    )
    .await?;
    let db = lock(state)?;
    db.connection.execute(
        "UPDATE whatsapp_inbox SET status='running',media=NULL WHERE id=?",
        [id],
    )?;
    Ok(())
}

fn settle(db: &Connection, row: &Value) -> Result<()> {
    let id = string(row, "id");
    if row["workflow"] == 1 {
        let Some(run) = one(
            db,
            "SELECT id,status,last,graph,cursor,revision FROM workflow_runs WHERE id=?",
            [id],
        )?
        else {
            return Ok(());
        };
        if run["status"] == "approval" {
            let graph = data::json(&run["graph"])?;
            let node = graph["nodes"]
                .as_array()
                .and_then(|nodes| nodes.iter().find(|n| n["id"] == run["cursor"]))
                .ok_or_else(Failure::storage)?;
            prompt(
                db,
                row,
                "workflow",
                id,
                run["revision"].as_i64(),
                string(node, "prompt"),
                &[json!("Approve"), json!("Reject")],
            )?;
        }
        if matches!(string(&run, "status"), "done" | "failed" | "cancelled") {
            let mut files = Vec::new();
            for step in all(
                db,
                "SELECT turn FROM workflow_steps WHERE run=? AND turn IS NOT NULL ORDER BY seq",
                [id],
            )? {
                for file in crate::turns::requested_files(db, string(&step, "turn"))? {
                    if !files.iter().any(|f: &Value| f["id"] == file["id"]) {
                        files.push(file);
                    }
                }
            }
            return finish(
                db,
                id,
                &result_text(&run, "last", "Workflow", &files),
                &files,
            );
        }
    } else {
        let Some(turn) = one(db, "SELECT id,status,reply FROM turns WHERE id=?", [id])? else {
            return Ok(());
        };
        for question in all(
            db,
            "SELECT id,prompt,choices FROM questions WHERE turn=? AND answer IS NULL",
            [id],
        )? {
            let choices = data::json(&question["choices"])?;
            prompt(
                db,
                row,
                "question",
                string(&question, "id"),
                None,
                string(&question, "prompt"),
                choices.as_array().ok_or_else(Failure::storage)?,
            )?;
        }
        if matches!(string(&turn, "status"), "done" | "failed" | "cancelled") {
            let files = if row["kind"] == "agents" {
                crate::turns::requested_files(db, id)?
            } else {
                data::history::generated(db, id)?
                    .as_array()
                    .cloned()
                    .ok_or_else(Failure::storage)?
            };
            return finish(db, id, &result_text(&turn, "reply", "Task", &files), &files);
        }
    }
    Ok(())
}

fn result_text(row: &Value, field: &str, label: &str, files: &[Value]) -> String {
    match string(row, "status") {
        "done" if !string(row, field).is_empty() => string(row, field).into(),
        "done" => format!("{label} finished without a text result."),
        "cancelled" => format!("{label} cancelled."),
        _ if !files.is_empty() => {
            format!("{label} failed. Any available files are partial results.")
        }
        _ => format!("{label} failed. Check Minarbot for details."),
    }
}

fn output_file(db: &Connection, id: &str) -> Result<Attachment> {
    let (name, bytes): (String, Vec<u8>) = db.query_row(
        "SELECT name,bytes FROM files WHERE id=? AND deleted=0 AND length(bytes)<=?",
        params![id, MAX_MEDIA],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let (kind, mime) = if let Some(mime) = attachments::image_type(&bytes) {
        if bytes.len() > protocol::media_limit(MediaKind::Image) {
            return Err(error(413, "Result image exceeds the 4 MiB limit."));
        }
        (MediaKind::Image, mime)
    } else if let Some(format) = transcription::audio(&bytes) {
        let mime = match format {
            "ogg"
                if bytes[..bytes.len().min(64)]
                    .windows(8)
                    .any(|field| field == b"OpusHead") =>
            {
                "audio/ogg; codecs=opus"
            }
            "ogg" => "audio/ogg",
            "mp3" => "audio/mpeg",
            "mp4" => "audio/mp4",
            "wav" => "audio/wav",
            "flac" => "audio/flac",
            _ => "audio/webm",
        };
        (MediaKind::Audio, mime)
    } else {
        (MediaKind::Document, "application/octet-stream")
    };
    Ok(Attachment {
        name,
        kind,
        mime: mime.into(),
        bytes,
    })
}

async fn deliver(state: &AppState, generation: &str, session: &dyn Session) -> Result<()> {
    let (row, file) = {
        let db = lock(state)?;
        let db = &db.connection;
        let now = state.config.now();
        if !epoch_active(db, generation)?
            || !exists(db, "SELECT 1 FROM whatsapp_link WHERE next_send<=?", [now])?
        {
            return Ok(());
        }
        let Some(row) = one(db,"SELECT o.id,o.file,o.text,i.chat,i.route,i.kind,i.target FROM whatsapp_outbox o JOIN whatsapp_inbox i ON i.id=o.input JOIN whatsapp_chats c ON c.chat=i.chat AND c.revision=i.route WHERE o.status='queued' ORDER BY o.rowid LIMIT 1",[])? else { return Ok(()); };
        let id = string(&row, "id");
        if target_agent(db, string(&row, "kind"), string(&row, "target")).is_err() {
            db.execute("UPDATE whatsapp_outbox SET status='failed',error='Minarbot destination is unavailable.' WHERE id=?",[id])?;
            return Ok(());
        }
        let file = match row["file"]
            .as_str()
            .map(|id| output_file(db, id))
            .transpose()
        {
            Ok(file) => file,
            Err(failure) => {
                let message = if failure.status == 413 {
                    failure.message.as_str()
                } else {
                    "Result file is unavailable."
                };
                db.execute(
                    "UPDATE whatsapp_outbox SET status='failed',error=? WHERE id=?",
                    params![message, id],
                )?;
                return Ok(());
            }
        };
        transaction(db, || {
            db.execute(
                "UPDATE whatsapp_link SET next_send=?",
                [now.saturating_add(SEND_INTERVAL_MS)],
            )?;
            db.execute(
                "UPDATE whatsapp_outbox SET status='sending' WHERE id=?",
                [id],
            )?;
            Ok(())
        })?;
        (row, file)
    };
    let result = session
        .send(
            string(&row, "chat"),
            string(&row, "id"),
            string(&row, "text"),
            file,
        )
        .await;
    let (status, error) = match result {
        Ok(()) => ("sent", None),
        Err(failure) if failure.status == 422 => ("failed", Some(failure.message)),
        Err(_) => (
            "uncertain",
            Some("Send outcome could not be confirmed. Check WhatsApp.".into()),
        ),
    };
    let db = lock(state)?;
    db.connection.execute(
        "UPDATE whatsapp_outbox SET status=?,error=? WHERE id=? AND status='sending'",
        params![status, error, string(&row, "id")],
    )?;
    Ok(())
}

pub(super) async fn tick(state: &AppState, generation: &str, session: &dyn Session) -> Result<()> {
    let inputs = {
        let db = lock(state)?;
        if !epoch_active(&db.connection, generation)? {
            return Ok(());
        }
        db.connection.execute("UPDATE whatsapp_inbox SET status='revoked',media=NULL WHERE status IN ('queued','running') AND NOT EXISTS(SELECT 1 FROM whatsapp_chats c WHERE c.chat=whatsapp_inbox.chat AND c.revision=whatsapp_inbox.route)",[])?;
        for row in all(
            &db.connection,
            "SELECT * FROM whatsapp_inbox WHERE status='running' ORDER BY rowid LIMIT 100",
            [],
        )? {
            settle(&db.connection, &row)?;
        }
        all(
            &db.connection,
            "SELECT * FROM whatsapp_inbox WHERE status='queued' ORDER BY rowid LIMIT 1",
            [],
        )?
    };
    if let Some(row) = inputs.first() {
        if let Err(failure) = submit(state, generation, row, session).await {
            let db = lock(state)?;
            if allowed(&db.connection, generation, row)? {
                let text = match failure.status {
                    413 | 415 => "WhatsApp attachment could not be processed.",
                    422 => &failure.message,
                    409 => "Request could not proceed. Check the destination or latest approval in Minarbot.",
                    _ => "Request could not be completed. Check Minarbot for details.",
                };
                finish(&db.connection, string(row, "id"), text, &[])?;
            }
        }
    }
    deliver(state, generation, session).await
}
