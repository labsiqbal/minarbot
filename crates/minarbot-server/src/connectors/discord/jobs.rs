use super::*;
use crate::{automation::workflows, data, files::attachments, turns::admission};
use futures_util::{stream::FuturesUnordered, StreamExt};
use reqwest::multipart::{Form, Part};
use std::collections::HashSet;

fn route(db: &Connection, id: &str) -> Result<Value> {
    let route = one(db, "SELECT * FROM discord_routes WHERE id=?", [id])?
        .ok_or_else(|| error(409, "Discord route was removed."))?;
    target(db, &route)?;
    Ok(route)
}

fn user_allowed(route: &Value, user: &str) -> Result<bool> {
    Ok(data::json(&route["users"])?
        .as_array()
        .is_some_and(|users| users.iter().any(|u| u == user)))
}

fn conversation(route: &Value) -> (&'static str, &str) {
    if route["kind"] == "agent" {
        ("threads", string(route, "thread"))
    } else {
        ("channels", string(route, "target"))
    }
}

fn workflow_allowed(db: &Connection, route: &Value, agent: &str) -> Result<bool> {
    if route["kind"] == "agent" {
        Ok(route["target"] == agent)
    } else {
        workflows::runtime::channel(db, string(route, "target"), agent)
    }
}

pub(super) fn record(state: &AppState, bot: &Bot, message: &Value) -> Result<()> {
    let (id, channel, sender) = (
        string(message, "id"),
        string(message, "channel_id"),
        string(&message["author"], "id"),
    );
    if [id, channel, sender].into_iter().any(|id| !snowflake(id))
        || sender == bot.id
        || message["author"]["bot"] == true
        || !message["webhook_id"].is_null()
        || !matches!(message["type"].as_u64(), Some(0 | 19))
    {
        return Ok(());
    }
    let guard = lock(state)?;
    let db = &guard.connection;
    if !current(db, bot)? {
        return Ok(());
    }
    let Some(route) = one(
        db,
        "SELECT * FROM discord_routes WHERE channel=?",
        [channel],
    )?
    else {
        return Ok(());
    };
    if !user_allowed(&route, sender)? {
        return Ok(());
    }
    let content = string(message, "content").replace(&bot.token, "[redacted]");
    let files = message["attachments"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    if content.trim().is_empty() && files.is_empty() {
        return Ok(());
    }
    if let Err(error) = target(db, &route) {
        return if error.status == 409 {
            Ok(())
        } else {
            Err(error)
        };
    }
    transaction(db, || {
        if !content.trim().is_empty()
            && files.is_empty()
            && text_answer(state, db, &route, sender, message, &content)?
        {
            return Ok(());
        }
        let clean = if files.len() > 4 {
            Value::Null
        } else {
            json!(files
                .iter()
                .map(|f| json!({
                    "id": f["id"], "size": f["size"],
                    "name":string(f,"filename").replace(&bot.token, "[redacted]"),
                    "url":f["url"]
                }))
                .collect::<Vec<_>>())
        };
        db.execute("INSERT OR IGNORE INTO discord_events(message,route,sender,text,attachments) VALUES(?,?,?,?,?)",
            params![id,string(&route,"id"),sender,content,clean.to_string()])?;
        Ok(())
    })
}

fn outbox(
    db: &Connection,
    destination: &Value,
    text: &str,
    files: &[Value],
    options: Option<&Value>,
) -> Result<()> {
    let parts = super::super::media::chunks(text, 2000, false).len() + files.len();
    db.execute("INSERT OR IGNORE INTO discord_outbox(id,route,reply_to,text,files,buttons,total,workflow_output) VALUES(?,?,?,?,?,?,?,?)",
        params![string(destination,"id"),string(destination,"route"),destination["reply_to"].as_str(),
          text,json!(files).to_string(),options.map(Value::to_string),parts,destination["workflow_output"].as_i64().unwrap_or(0)])?;
    Ok(())
}

fn reply(
    db: &Connection,
    event: &Value,
    id: &str,
    text: &str,
    files: &[Value],
    options: Option<&Value>,
) -> Result<()> {
    outbox(
        db,
        &json!({"id":id,"route":event["route"],"reply_to":event["message"]}),
        text,
        files,
        options,
    )
}

fn note_event(db: &Connection, event: &Value, text: &str) -> Result<()> {
    reply(db, event, &runner::new_id(), text, &[], None)?;
    finish_event(db, event)
}

fn finish_event(db: &Connection, event: &Value) -> Result<()> {
    db.execute(
        "UPDATE discord_events SET status='done',text='',attachments='[]' WHERE message=?",
        [string(event, "message")],
    )?;
    Ok(())
}

fn workflow_files(db: &Connection, run: &str) -> Result<Vec<Value>> {
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    for step in all(
        db,
        "SELECT turn FROM workflow_steps WHERE run=? AND turn IS NOT NULL ORDER BY seq",
        [run],
    )? {
        for file in turn_files(db, string(&step, "turn"))? {
            if seen.insert(string(&file, "id").to_owned()) {
                files.push(file);
            }
        }
    }
    Ok(files)
}

fn turn_files(db: &Connection, turn: &str) -> Result<Vec<Value>> {
    let mut files = crate::turns::requested_files(db, turn)?;
    // Section turns are not in side threads, so include their direct generated results.
    files.extend(all(db, "SELECT DISTINCT f.id,f.name,length(f.bytes) AS size,f.deleted FROM (SELECT turn,file FROM generated_files UNION ALL SELECT turn,file FROM mcp_files UNION ALL SELECT turn,file FROM image_generations WHERE file IS NOT NULL) o JOIN turns t ON t.id=o.turn JOIN files f ON f.id=o.file WHERE o.turn=? AND t.channel IS NOT NULL ORDER BY f.seq", [turn])?);
    files.retain(|f| !matches!(f["deleted"], Value::Bool(true)) && f["deleted"] != 1);
    Ok(files)
}

pub(crate) fn workflow_route(db: &Connection, channel: &str, agent: &str) -> Result<Option<Value>> {
    let Some(route) = one(
        db,
        "SELECT * FROM discord_routes WHERE channel=?",
        [channel],
    )?
    else {
        return Ok(None);
    };
    if let Err(error) = target(db, &route) {
        return if error.status == 409 {
            Ok(None)
        } else {
            Err(error)
        };
    }
    Ok(workflow_allowed(db, &route, agent)?.then_some(route))
}

pub(crate) fn workflow_output(db: &Connection, run: &Value, channel: &str) -> Result<bool> {
    let Some(route) = workflow_route(db, channel, string(run, "agent"))? else {
        return Ok(false);
    };
    let id = string(run, "id");
    let source = one(db, "SELECT e.* FROM discord_jobs j JOIN discord_events e ON e.message=j.message WHERE j.id=? AND j.kind='workflow' AND j.status='running'", [id])?;
    if let Some(source) = &source {
        // A source route revoked while an approval or step was running must not export results.
        if !exists(
            db,
            "SELECT 1 FROM discord_routes WHERE id=?",
            [string(source, "route")],
        )? {
            return Ok(false);
        }
    } else if exists(
        db,
        "SELECT 1 FROM discord_jobs WHERE id=? AND status='revoked'",
        [id],
    )? {
        return Ok(false);
    }
    let reply_to = source
        .as_ref()
        .filter(|e| e["route"] == route["id"])
        .and_then(|e| e["message"].as_str());
    let files = workflow_files(db, id)?;
    let text = string(run, "last");
    outbox(
        db,
        &json!({"id":id,"route":route["id"],"reply_to":reply_to,"workflow_output":1}),
        if text.trim().is_empty() {
            "Workflow finished."
        } else {
            text
        },
        &files,
        None,
    )?;
    Ok(true)
}

pub(crate) fn workflow_delivery(db: &Connection, id: &str) -> Result<(String, String)> {
    let delivery = one(
        db,
        "SELECT status,part,total,error FROM discord_outbox WHERE id=? AND workflow_output=1",
        [id],
    )?
    .ok_or_else(Failure::storage)?;
    let detail = format!(
        "Discord delivery: {} of {} parts confirmed. {}",
        delivery["part"],
        delivery["total"],
        string(&delivery, "error")
    );
    Ok((string(&delivery, "status").into(), detail))
}

async fn upload(
    state: &AppState,
    bot: &Bot,
    api: &Api,
    event: &Value,
    route: &Value,
) -> Result<Vec<String>> {
    let data = data::json(&event["attachments"])?;
    let files = data
        .as_array()
        .ok_or_else(|| error(413, "Send at most four Discord attachments."))?;
    let mut seen = HashSet::new();
    let (kind, owner) = conversation(route);
    let mut ids = Vec::new();
    for file in files {
        let remote = string(file, "id");
        if !snowflake(remote)
            || !seen.insert(remote)
            || file["size"].as_u64().is_none_or(|n| n > MAX_FILE as u64)
        {
            return Err(error(413, "Discord attachment is invalid or too large."));
        }
        if !lock(state).and_then(|g| current(&g.connection, bot))? {
            return Err(error(409, "Discord connection changed."));
        }
        let bytes = api.attachment(string(file, "url")).await?;
        super::super::media::validate(&bytes)?;
        let id = runner::new_id();
        let uri = format!("/api/{kind}/{owner}/files/{id}");
        let request = Request::builder()
            .method(Method::POST)
            .uri(&uri)
            .header("origin", &state.origin)
            .header("content-type", "application/octet-stream")
            .header(
                "x-file-name",
                attachments::encode_name(string(file, "name")),
            )
            .body(axum::body::Body::from(bytes))
            .map_err(|_| Failure::storage())?;
        let response =
            attachments::handle(state, request, &["api", kind, owner, "files", &id]).await?;
        if !response.status().is_success() {
            return Err(error(
                response.status().as_u16(),
                "Discord attachment could not be saved.",
            ));
        }
        ids.push(id);
    }
    Ok(ids)
}

async fn process(state: &AppState, bot: &Bot, api: &Api, event: &Value) -> Result<()> {
    let route = {
        let guard = lock(state)?;
        let db = &guard.connection;
        if !current(db, bot)? {
            return Ok(());
        }
        let route = route(db, string(event, "route"))?;
        if !user_allowed(&route, string(event, "sender"))? {
            return Err(error(403, "Discord sender no longer has access."));
        }
        let text = string(event, "text");
        if event["attachments"] == "[]" && !text.trim().is_empty() {
            let cmd = text.splitn(4, ' ').collect::<Vec<_>>();
            let workflows = if cmd.first() == Some(&"!workflow") {
                if cmd.len() != 4 || !uuid(cmd[1]) || cmd[3].trim().is_empty() {
                    return note_event(db, event, "Use !workflow ID REVISION message.");
                }
                all(db,"SELECT id,agent,revision FROM workflows WHERE id=? AND deleted=0 AND revision=?", [cmd[1],cmd[2]])?
            } else {
                all(db,"SELECT w.id,w.agent,w.revision FROM workflows w JOIN workflow_versions v ON v.workflow=w.id AND v.revision=w.revision WHERE w.deleted=0 AND json_extract(v.graph,'$.trigger.kind')='discord' AND json_extract(v.graph,'$.trigger.channel')=? ORDER BY w.id", [string(&route,"channel")])?
            };
            let matches = workflows
                .into_iter()
                .map(|w| {
                    workflow_allowed(db, &route, string(&w, "agent"))
                        .map(|allowed| allowed.then_some(w))
                })
                .collect::<Result<Vec<_>>>()?;
            let matching = matches.into_iter().flatten().collect::<Vec<_>>();
            if !matching.is_empty() {
                return transaction(db, || {
                    for workflow in matching {
                        let id = runner::new_id();
                        let input = if cmd.first() == Some(&"!workflow") {
                            cmd[3]
                        } else {
                            text
                        };
                        let path = format!("/api/workflows/{}/run", string(&workflow, "id"));
                        workflows::route(
                            state,
                            db,
                            &path,
                            &["api", "workflows", string(&workflow, "id"), "run"],
                            &Method::POST,
                            &[],
                            &Default::default(),
                            &json!({"id":id,"revision":workflow["revision"],"input":input}),
                        )?;
                        db.execute(
                            "INSERT INTO discord_jobs(id,message,kind) VALUES(?,?,'workflow')",
                            [&id, string(event, "message")],
                        )?;
                    }
                    finish_event(db, event)
                });
            }
            if cmd.first() == Some(&"!workflow") {
                return note_event(
                    db,
                    event,
                    "Workflow unavailable for this Discord conversation.",
                );
            }
        }
        if route["kind"] == "agent"
            && !exists(
                db,
                "SELECT 1 FROM threads WHERE id=?",
                [string(&route, "thread")],
            )?
        {
            transaction(db, || {
                data::conversations::threads(
                    db,
                    string(&route, "target"),
                    &json!({"id":route["thread"],"name":format!("Discord {}",route["channel"].as_str().unwrap_or(""))}),
                    false,
                )
            })?;
        }
        route
    };
    let files = upload(state, bot, api, event, &route).await?;
    let (kind, owner) = conversation(&route);
    let id = runner::new_id();
    let input = string(event, "text");
    let mut body = json!({"id":id,"text":if input.trim().is_empty() {String::new()} else {format!("Discord user {}: {input}",string(event,"sender"))},"files":files});
    let parts = ["api", kind, owner, "turns"];
    let path = format!("/api/{kind}/{owner}/turns");
    admission::prepare(state, &parts, &Method::POST, &body).await?;
    let guard = lock(state)?;
    let db = &guard.connection;
    if !current(db, bot)?
        || !exists(
            db,
            "SELECT 1 FROM discord_events WHERE message=? AND status='queued'",
            [string(event, "message")],
        )?
    {
        return Ok(());
    }
    target(db, &route)?;
    if kind == "channels" {
        // Use the section's normal recipient resolution with a single reply per Discord request.
        body["recipient"] = json!(admission::conversation(db, kind, owner, &body)?.0);
    }
    transaction(db, || {
        admission::route(state, db, &path, &parts, &Method::POST, &[], &body)?;
        db.execute(
            "INSERT INTO discord_jobs(id,message,kind) VALUES(?,?,'turn')",
            [&id, string(event, "message")],
        )?;
        finish_event(db, event)
    })
}

fn remember_choice(
    db: &Connection,
    job: &Value,
    subject: &str,
    revision: i64,
    kind: &str,
    prompt: &str,
    choices: &Value,
) -> Result<()> {
    if exists(
        db,
        "SELECT 1 FROM discord_choices WHERE subject=? AND revision=? AND kind=?",
        params![subject, revision, kind],
    )? {
        return Ok(());
    }
    let notice = runner::new_id();
    db.execute(
        "INSERT INTO discord_choices(notice,job,subject,revision,kind,options) VALUES(?,?,?,?,?,?)",
        params![
            notice,
            string(job, "id"),
            subject,
            revision,
            kind,
            choices.to_string()
        ],
    )?;
    let text = if choices.as_array().is_none_or(Vec::is_empty) {
        format!("{prompt}\n\nReply to this message to answer.")
    } else {
        prompt.to_owned()
    };
    reply(db, job, &notice, &text, &[], Some(choices))
}

fn settle(state: &AppState, bot: &Bot) -> Result<()> {
    let guard = lock(state)?;
    let db = &guard.connection;
    if !current(db, bot)? {
        return Ok(());
    }
    for job in all(db, "SELECT j.*,e.route,e.sender,e.message FROM discord_jobs j JOIN discord_events e ON e.message=j.message WHERE j.status='running' ORDER BY j.rowid LIMIT 100", [])? {
        transaction(db, || {
            let id = string(&job,"id");
            let destination = route(db,string(&job,"route"));
            if destination.is_err() {
                db.execute("UPDATE discord_jobs SET status='revoked' WHERE id=?", [id])?;
                return Ok(());
            }
            let workflow = job["kind"]=="workflow";
            let result = if workflow {
                one(db,"SELECT * FROM workflow_runs WHERE id=?", [id])?
            } else {
                one(db,"SELECT id,status,reply,paused FROM turns WHERE id=?", [id])?
            };
            let Some(result) = result else {
                db.execute("UPDATE discord_jobs SET status='revoked' WHERE id=?", [id])?;
                return Ok(());
            };
            if workflow && result["status"]=="approval" {
                let node = workflows::runtime::node(&result)?;
                remember_choice(db,&job,id,result["revision"].as_i64().ok_or_else(Failure::storage)?,
                    "workflow",string(&node,"prompt"),&json!(["Approve","Reject"]))?;
            }
            if !workflow {
                for question in all(db,"SELECT id,prompt,choices FROM questions WHERE turn=? AND answer IS NULL ORDER BY rowid", [id])? {
                    let choices = data::json(&question["choices"])?;
                    remember_choice(db,&job,string(&question,"id"),0,"question",string(&question,"prompt"),&choices)?;
                }
            }
            let status = string(&result,"status");
            if !["done","failed","interrupted","cancelled"].contains(&status) || result["paused"]==1 { return Ok(()); }
            let delivered = workflow && exists(db,"SELECT 1 FROM discord_outbox WHERE id=? AND workflow_output=1", [id])?;
            if !delivered {
                let files = if workflow { workflow_files(db,id)? } else { turn_files(db,id)? };
                let text = match status {
                    "done" => {
                        let text = string(&result,if workflow {"last"} else {"reply"});
                        if text.trim().is_empty() { if files.is_empty() {"Task finished without a text reply."} else {""} } else {text}
                    }
                    "cancelled" => "Task cancelled. Check Minarbot for saved results.",
                    _ => "Task failed. Check Minarbot for details.",
                };
                reply(db,&job,id,text,&files,None)?;
            }
            db.execute("UPDATE discord_jobs SET status='done' WHERE id=?", [id])?;
            Ok(())
        })?;
    }
    Ok(())
}

const CHOICE: &str = "SELECT c.*,o.status,o.remote,j.kind AS job_kind,j.id AS job_id,e.sender,e.route FROM discord_choices c JOIN discord_outbox o ON o.id=c.notice JOIN discord_jobs j ON j.id=c.job JOIN discord_events e ON e.message=j.message";
fn decide(
    state: &AppState,
    db: &Connection,
    route: &Value,
    choice: &Value,
    index: Option<usize>,
    text: &str,
) -> Result<()> {
    let options = data::json(&choice["options"])?;
    let id = runner::new_id();
    let job = string(choice, "job_id");
    if choice["kind"] == "workflow" {
        let path = format!("/api/workflow-runs/{job}/approve");
        workflows::route(
            state,
            db,
            &path,
            &["api", "workflow-runs", job, "approve"],
            &Method::POST,
            &[],
            &Default::default(),
            &json!({"id":id,"revision":choice["revision"],"accepted":index==Some(0)}),
        )?;
    } else {
        let answer = if let Some(index) = index {
            options[index].as_str().ok_or_else(Failure::storage)?
        } else {
            text
        };
        let (kind, owner) = conversation(route);
        let question = string(choice, "subject");
        let path = format!("/api/{kind}/{owner}/turns/{job}/questions/{question}/answer");
        admission::route(
            state,
            db,
            &path,
            &[
                "api",
                kind,
                owner,
                "turns",
                job,
                "questions",
                question,
                "answer",
            ],
            &Method::POST,
            &[],
            &json!({"id":id,"text":answer}),
        )?;
    }
    db.execute(
        "UPDATE discord_choices SET decided=1 WHERE notice=?",
        [string(choice, "notice")],
    )?;
    Ok(())
}

fn text_answer(
    state: &AppState,
    db: &Connection,
    route: &Value,
    sender: &str,
    message: &Value,
    text: &str,
) -> Result<bool> {
    let Some(referenced) = message["message_reference"]["message_id"]
        .as_str()
        .filter(|s| snowflake(s))
    else {
        return Ok(false);
    };
    let Some(choice) = one(
        db,
        &format!("{CHOICE} WHERE o.remote=? AND e.route=?"),
        [referenced, string(route, "id")],
    )?
    else {
        return Ok(false);
    };
    if choice["sender"] == sender
        && choice["decided"] == 0
        && choice["kind"] == "question"
        && choice["options"] == "[]"
    {
        if let Err(error) = decide(state, db, route, &choice, None, text) {
            if ![400, 404, 409].contains(&error.status) {
                return Err(error);
            }
        }
    }
    Ok(true)
}

pub(super) struct Callback {
    id: String,
    token: String,
    content: &'static str,
}
impl Callback {
    pub(super) async fn respond(self, api: &Api) {
        let path = format!("/interactions/{}/{}/callback", self.id, self.token);
        let body = json!({"type":4,"data":{"content":self.content,"flags":64,"allowed_mentions":{"parse":[]}}});
        let _ = tokio::time::timeout(
            Duration::from_millis(2500),
            api.request("interaction", &self.id, &path, reqwest::Method::POST, |r| {
                Ok(r.json(&body))
            }),
        )
        .await;
    }
}

pub(super) fn interaction(state: &AppState, bot: &Bot, value: &Value) -> Result<Option<Callback>> {
    let id = string(value, "id");
    let token = string(value, "token");
    let custom = string(&value["data"], "custom_id");
    let Some((notice, index)) = custom
        .strip_prefix("minarbot:")
        .and_then(|s| s.split_once(':'))
    else {
        return Ok(None);
    };
    let Some(index) = index.parse::<usize>().ok() else {
        return Ok(None);
    };
    if value["type"] != 3
        || value["data"]["component_type"] != 2
        || value["message"]["author"]["id"] != bot.id
        || !snowflake(id)
        || !snowflake(string(&value["message"], "id"))
        || !uuid(notice)
        || token.is_empty()
        || token.len() > 512
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
    {
        return Ok(None);
    }
    let sender = value["member"]["user"]["id"]
        .as_str()
        .unwrap_or_else(|| string(&value["user"], "id"));
    let guard = lock(state)?;
    let db = &guard.connection;
    if !current(db, bot)? {
        return Ok(None);
    }
    let mut content = "This approval is unavailable.";
    if let Some(choice) = one(db, &format!("{CHOICE} WHERE c.notice=?"), [notice])? {
        let recorded = choice["remote"].as_str();
        if (choice["status"] == "sending" || choice["status"] == "sent")
            && recorded.is_none_or(|id| value["message"]["id"] == id)
        {
            if let Ok(route) = route(db, string(&choice, "route")) {
                if value["channel_id"] == route["channel"]
                    && value["message"]["channel_id"] == route["channel"]
                {
                    if choice["sender"] != sender || !user_allowed(&route, sender)? {
                        content = "Only the original requester can choose.";
                    } else if choice["decided"] == 1 {
                        content = "A choice was already recorded.";
                    } else if data::json(&choice["options"])?[index].is_string() {
                        content = match transaction(db, || {
                            decide(state, db, &route, &choice, Some(index), "")
                        }) {
                            Ok(()) => "Choice recorded.",
                            Err(e) if (400..500).contains(&e.status) => {
                                "This approval is no longer active."
                            }
                            Err(e) => return Err(e),
                        };
                    }
                }
            }
        }
    }
    Ok(Some(Callback {
        id: id.into(),
        token: token.into(),
        content,
    }))
}

async fn deliver(state: &AppState, bot: &Bot, api: &Api) -> Result<()> {
    let (row, destination, payload, file) = {
        let guard = lock(state)?;
        let db = &guard.connection;
        if !current(db, bot)? {
            return Ok(());
        }
        let Some(row) = one(
            db,
            "SELECT o.* FROM discord_outbox o WHERE o.status='queued' AND NOT EXISTS (SELECT 1 FROM discord_outbox active WHERE active.route=o.route AND active.status='sending') ORDER BY o.rowid LIMIT 1",
            [],
        )?
        else {
            return Ok(());
        };
        let id = string(&row, "id");
        if row["workflow_output"] == 1
            && !exists(
                db,
                "SELECT 1 FROM workflow_runs WHERE id=? AND status='waiting'",
                [id],
            )?
        {
            return delivery_error(db, id, "revoked", "Workflow stopped before delivery.");
        }
        let destination = match route(db, string(&row, "route")) {
            Ok(route) => route,
            Err(e) if e.status == 409 => return delivery_error(db, id, "revoked", &e.message),
            Err(e) => return Err(e),
        };
        let parts = super::super::media::chunks(string(&row, "text"), 2000, false);
        let part = row["part"].as_u64().ok_or_else(Failure::storage)? as usize;
        let mut payload = json!({"allowed_mentions":{"parse":[],"replied_user":false}});
        if let Some(reply) = row["reply_to"].as_str() {
            payload["message_reference"] = json!({"message_id":reply,"fail_if_not_exists":false});
        }
        let file = if let Some(text) = parts.get(part) {
            payload["content"] = json!(text.replace(&bot.token, "[redacted]"));
            if part + 1 == parts.len() && row["buttons"].is_string() {
                let choices = data::json(&row["buttons"])?;
                let buttons = choices.as_array().into_iter().flatten().take(25).enumerate().map(|(index,label)| {
                    let label = super::super::media::chunks(label.as_str().unwrap_or("Choice"),80,false).into_iter().next().unwrap_or_else(||"Choice".into());
                    json!({"type":2,"style":2,"label":label,"custom_id":format!("minarbot:{id}:{index}")})
                }).collect::<Vec<_>>();
                if !buttons.is_empty() {
                    payload["components"] = json!(buttons
                        .chunks(5)
                        .map(|row| json!({"type":1,"components":row}))
                        .collect::<Vec<_>>());
                }
            }
            None
        } else {
            let files = data::json(&row["files"])?;
            let f = &files[part - parts.len()];
            let file_id = string(f, "id");
            let size = f["size"].as_u64().unwrap_or(u64::MAX);
            if size > MAX_FILE as u64 {
                return delivery_error(
                    db,
                    id,
                    "failed",
                    "Generated file exceeds the 10 MiB limit.",
                );
            }
            let source = one(
                db,
                "SELECT scope FROM files WHERE id=? AND deleted=0",
                [file_id],
            )?;
            let Some(source) = source else {
                return delivery_error(db, id, "failed", "Generated file was removed.");
            };
            let file = attachments::file(db, file_id, string(&source, "scope"))?;
            if file.bytes.len() > MAX_FILE {
                return delivery_error(
                    db,
                    id,
                    "failed",
                    "Generated file exceeds the 10 MiB limit.",
                );
            }
            let name = string(&file.meta, "name").replace(&bot.token, "[redacted]");
            payload["attachments"] = json!([{"id":0,"filename":name}]);
            Some((name, file.bytes))
        };
        db.execute(
            "UPDATE discord_outbox SET status='sending' WHERE id=?",
            [id],
        )?;
        (row, destination, payload, file)
    };
    let channel = string(&destination, "channel");
    let result = api.request("messages",channel,&format!("/channels/{channel}/messages"),reqwest::Method::POST,|r| {
        let guard = lock(state)?;
        let db = &guard.connection;
        if !current(db,bot)? {
            return Err(error(409,"Discord connection changed."));
        }
        let current = route(db,string(&row,"route"))?;
        if row["workflow_output"]==1 {
            let admissible=exists(db,"SELECT 1 FROM workflow_runs r WHERE id=? AND status='waiting' AND NOT EXISTS(SELECT 1 FROM discord_jobs j WHERE j.id=r.id AND j.status='revoked')",[string(&row,"id")])?;
            let agent=one(db,"SELECT agent FROM workflow_runs WHERE id=?",[string(&row,"id")])?.ok_or_else(Failure::storage)?;
            if !admissible || !workflow_allowed(db,&current,string(&agent,"agent"))? {
                return Err(error(409,"Discord workflow destination is no longer available."));
            }
        }
        if let Some((name,bytes)) = &file {
            Ok(r.multipart(Form::new().text("payload_json",payload.to_string()).part("files[0]",Part::bytes(bytes.clone()).file_name(name.clone()))))
        } else { Ok(r.json(&payload)) }
    }).await;
    let guard = lock(state)?;
    let db = &guard.connection;
    let id = string(&row, "id");
    match result {
        Ok(message) if snowflake(string(&message, "id")) => {
            db.execute("UPDATE discord_outbox SET part=part+1,status=CASE WHEN part+1=total THEN 'sent' WHEN NOT EXISTS(SELECT 1 FROM discord_routes r WHERE r.id=discord_outbox.route) THEN 'revoked' ELSE 'queued' END,remote=? WHERE id=?",
                [string(&message,"id"),id])?;
            Ok(())
        }
        Ok(_) => delivery_error(
            db,
            id,
            "uncertain",
            "Discord response did not identify the sent message.",
        ),
        Err(e) => delivery_error(
            db,
            id,
            if (400..500).contains(&e.status) {
                "failed"
            } else {
                "uncertain"
            },
            &e.message,
        ),
    }
}

fn delivery_error(db: &Connection, id: &str, status: &str, detail: &str) -> Result<()> {
    db.execute(
        "UPDATE discord_outbox SET status=?,error=? WHERE id=?",
        [status, detail, id],
    )?;
    Ok(())
}

pub(super) async fn work(state: &AppState, bot: &Bot, api: &Api) {
    let jobs = async {
        loop {
            let event = lock(state).and_then(|g| {
                one(
                    &g.connection,
                    "SELECT * FROM discord_events WHERE status='queued' ORDER BY rowid LIMIT 1",
                    [],
                )
            });
            if let Ok(Some(event)) = event {
                if let Err(e) = process(state, bot, api, &event).await {
                    if let Ok(guard) = lock(state) {
                        let db = &guard.connection;
                        let _ = transaction(db, || {
                            if !current(db,bot)? || !exists(db,"SELECT 1 FROM discord_events WHERE message=? AND status='queued'",[string(&event,"message")])? { return Ok(()); }
                            let message = if e.status == 413 || e.status == 415 {
                                "Discord attachment could not be used. Send up to four files: UTF-8 text or PDF (32 KiB total extracted text), or PNG/JPEG/WebP (4 MiB total)."
                            } else {
                                "Minarbot could not accept this Discord message. Check the route and agent in Settings."
                            };
                            note_event(db, &event, message)
                        });
                    }
                }
            }
            let _ = settle(state, bot);
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    };
    let sends = async {
        let mut pending = FuturesUnordered::new();
        let mut tick = tokio::time::interval(Duration::from_millis(150));
        loop {
            tokio::select! {
                _ = tick.tick(), if pending.len() < 4 => pending.push(deliver(state,bot,api)),
                _ = pending.next(), if !pending.is_empty() => (),
            }
        }
    };
    tokio::join!(jobs, sends);
}
