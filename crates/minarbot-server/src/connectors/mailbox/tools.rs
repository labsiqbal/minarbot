use super::*;
use crate::turns;
use std::sync::LazyLock;
use tokio::time::timeout;

pub fn is_tool(name: &str) -> bool {
    matches!(
        name,
        "email_address" | "email_list" | "email_read" | "email_wait_for_code" | "email_send"
    )
}

pub fn specs() -> Result<Vec<Value>> {
    serde_json::from_str(include_str!("tools.json")).map_err(|_| Failure::storage())
}

pub fn available(state: &AppState, db: &Connection, turn: &Value) -> Result<bool> {
    if state.auth.is_some()
        || !turn["channel"].is_null()
        || !turn["parent"].is_null()
        || !turn["origin"].is_null()
        || turn["profile_setup"] == 1
        || turns::harness::restricted(db, turn)?
        || exists(
            db,
            "SELECT 1 FROM hub_link_turns WHERE turn_id=?",
            [string(turn, "id")],
        )?
    {
        return Ok(false);
    }
    let row = settings(db)?;
    Ok(row["domain"].is_string() && row["secret"].is_string())
}

fn active(state: &AppState, db: &Connection, turn: &Value) -> Result<()> {
    if !exists(
        db,
        "SELECT 1 FROM turns WHERE id=? AND status='running'",
        [string(turn, "id")],
    )? {
        return Err(error(409, "Task has stopped."));
    }
    if !available(state, db, turn)? {
        return Err(error(
            403,
            "Email tools require a private local turn and a configured relay.",
        ));
    }
    Ok(())
}

fn take_code(input: &str) -> Option<&str> {
    static LABEL: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?i)\b(?:code|otp)\s*(?:is\b\s*)?[:\-]?\s*([0-9]{4,8})\b")
            .unwrap_or_else(|_| unreachable!())
    });
    LABEL
        .captures(input)
        .and_then(|capture| capture.get(1))
        .map(|value| value.as_str())
}

fn code(mail: &Value) -> Option<&str> {
    take_code(string(mail, "subject"))
        .or_else(|| take_code(string(mail, "text")))
        .or_else(|| {
            let mut lines = string(mail, "text").lines().map(str::trim).filter(|line| {
                (4..=8).contains(&line.len()) && line.bytes().all(|b| b.is_ascii_digit())
            });
            let first = lines.next()?;
            lines.next().is_none().then_some(first)
        })
}

pub(super) async fn wait(state: &AppState, turn: &Value, args: &Value) -> Result<Value> {
    if !closed(
        args,
        &["from", "subject", "timeoutSeconds", "receivedAfter"],
    ) || !crate::data::integer(&args["timeoutSeconds"], 1, 120)
        || args.get("subject").is_some_and(|v| {
            v.as_str()
                .is_none_or(|s| s.trim().is_empty() || s.chars().count() > 320)
        })
        || args
            .get("receivedAfter")
            .is_some_and(|v| !crate::data::integer(v, 0, 9_007_199_254_740_991))
    {
        return Err(error(
            400,
            "Invalid email wait filters. Use a timeout of 1–120 seconds.",
        ));
    }
    let from = crate::data::normalize_email(&args["from"])?;
    let subject = string(args, "subject").to_lowercase();
    let since = args["receivedAfter"].as_i64().unwrap_or_else(|| {
        chrono::DateTime::parse_from_rfc3339(string(turn, "created"))
            .map_or_else(|_| state.config.now(), |date| date.timestamp_millis())
    });
    let agent = string(turn, "agent");
    let seconds = args["timeoutSeconds"]
        .as_u64()
        .ok_or_else(Failure::storage)?;
    let mut changes = state.events.subscribe();
    let mut after = 0;
    let key = key(state, false)?;
    let wait = async {
        loop {
            let more = {
                let guard = state.database.lock().map_err(|_| Failure::storage())?;
                let db = &guard.connection;
                active(state, db, turn)?;
                let rows = all(db,
                    "SELECT * FROM agent_email_messages WHERE agent=? AND seq>? AND received>=? ORDER BY seq LIMIT 25",
                    params![agent,after,since],
                )?;
                for row in &rows {
                    after = row["seq"].as_i64().ok_or_else(Failure::storage)?;
                    let mail = message(&key, agent, row)?;
                    if mail["from"] == from
                        && string(&mail, "subject").to_lowercase().contains(&subject)
                    {
                        if let Some(code) = code(&mail) {
                            return Ok(json!({"status":"received","id":mail["id"],"code":code}));
                        }
                    }
                }
                rows.len() == PAGE
            };
            if more {
                tokio::task::yield_now().await;
            } else {
                let _ = changes.recv().await;
            }
        }
    };
    timeout(Duration::from_secs(seconds), wait)
        .await
        .unwrap_or_else(|_| Ok(json!({"status":"timeout"})))
}

fn valid_text(value: &Value, max: usize, multiline: bool) -> bool {
    value.as_str().is_some_and(|s| {
        !s.trim().is_empty()
            && s.chars().count() <= max
            && !s
                .chars()
                .any(|c| c.is_control() && !(multiline && matches!(c, '\n' | '\r' | '\t')))
    })
}

async fn send(state: &AppState, turn: &Value, call: &Value, args: &Value) -> Result<Value> {
    if !closed(args, &["to", "subject", "text"])
        || !valid_text(&args["subject"], 320, false)
        || !valid_text(&args["text"], 8_000, true)
    {
        return Err(error(400, "Enter a recipient, subject and email text."));
    }
    let to = crate::data::normalize_email(&args["to"])?;
    let agent = string(turn, "agent");
    let turn_id = string(turn, "id");
    let call_id = string(call, "id");
    let id = uuid::Uuid::new_v4().to_string();
    let mut changes = state.events.subscribe();
    let _wait = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        active(state, db, turn)?;
        if let Some(draft) = one(
            db,
            "SELECT id FROM agent_email_drafts WHERE turn=? AND call_id=?",
            [turn_id, call_id],
        )? {
            return Ok(json!({"status":"draft_saved","id":draft["id"],"sent":false}));
        }
        let draft = json!({"from":configured_address(db, agent)?,"to":to,"subject":args["subject"],"text":args["text"]});
        let ciphertext = seal(
            &key(state, false)?,
            &json!(["agent-mail-draft-v1", agent, id]),
            &draft,
        )?;
        let input = json!({"ciphertext":ciphertext,"callId":call_id,"created":state.config.now()});
        let card = crate::harness::config::CardWait::start(state, turn_id)?;
        db.execute("INSERT INTO harness_approvals(id,turn,agent,runtime,workspace,kind,description,input,state) VALUES(?,?,?,'api','','email','Save this email draft?',?,'approval')",
            params![id,turn_id,agent,input.to_string()])?;
        card
    };
    loop {
        {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            let row = one(
                &guard.connection,
                "SELECT state,decision FROM harness_approvals WHERE id=?",
                [&id],
            )?
            .ok_or_else(Failure::storage)?;
            match string(&row, "state") {
                "decided" if row["decision"] == "allow" => {
                    return Ok(json!({"status":"draft_saved","id":id,"sent":false}))
                }
                "decided" => return Ok(json!({"status":"denied","sent":false})),
                "approval" => (),
                _ => return Err(error(409, "Email approval expired.")),
            }
        }
        let _ = changes.recv().await;
    }
}

pub fn draft_preview(state: &AppState, agent: &str, approval: &Value) -> Result<Value> {
    if approval["state"] != "approval" {
        return Ok(Value::Null);
    }
    let input = crate::data::json(&approval["input"])?;
    open(
        &key(state, false)?,
        &json!(["agent-mail-draft-v1", agent, approval["id"]]),
        string(&input, "ciphertext"),
    )
}

pub fn decide_draft(db: &Connection, approval: &Value, allowed: bool) -> Result<()> {
    let id = string(approval, "id");
    if allowed {
        // ponytail: approval records a draft. A future outbound transport can consume approved drafts.
        let input = crate::data::json(&approval["input"])?;
        db.execute(
            "INSERT INTO agent_email_drafts VALUES(?,?,?,?,?,?)",
            params![
                id,
                string(approval, "agent"),
                string(approval, "turn"),
                string(&input, "callId"),
                string(&input, "ciphertext"),
                input["created"].as_i64()
            ],
        )?;
    }
    db.execute("UPDATE harness_approvals SET input='null' WHERE id=?", [id])?;
    Ok(())
}

pub fn journal(
    state: &AppState,
    db: &Connection,
    turn: &Value,
    call: &Value,
    result: &Value,
) -> Result<()> {
    let redacted = json!({"id":call["id"],"type":"function","function":{"name":call["function"]["name"],"arguments":"{}"}});
    let context = json!([
        "agent-mail-tool-v1",
        turn["id"],
        call["id"],
        call["function"]["name"]
    ]);
    let payload = json!({"call":call,"result":result});
    let encrypted = seal(&key(state, false)?, &context, &payload)?;
    turns::harness::journal(db, turn, &redacted, &json!({"mailbox":encrypted}))
}

pub fn replay(state: &AppState, turn: &str, call: &mut Value, stored: &Value) -> Result<Value> {
    let stored = crate::data::json(stored)?;
    let context = json!([
        "agent-mail-tool-v1",
        turn,
        call["id"],
        call["function"]["name"]
    ]);
    let result = open(&key(state, false)?, &context, string(&stored, "mailbox"))?;
    *call = result["call"].clone();
    Ok(json!(result["result"].to_string()))
}

pub async fn run(state: &AppState, turn: &Value, call: &Value) -> Result<Value> {
    let name = string(&call["function"], "name");
    let args: Value = serde_json::from_str(string(&call["function"], "arguments"))
        .map_err(|_| error(400, "Invalid email tool arguments."))?;
    let result = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        active(state, db, turn)?;
        let agent = string(turn, "agent");
        match name {
            "email_address" if closed(&args, &[]) => {
                Some(json!({"address":configured_address(db, agent)?}))
            }
            "email_list" if closed(&args, &["before"]) => {
                Some(list(state, db, agent, cursor(&args["before"])?)?)
            }
            "email_read" if closed(&args, &["id"]) => {
                Some(read(state, db, agent, string(&args, "id"))?)
            }
            "email_wait_for_code" | "email_send" => None,
            _ => return Err(error(400, "Invalid email tool arguments.")),
        }
    };
    let result = match result {
        Some(value) => value,
        None if name == "email_wait_for_code" => wait(state, turn, &args).await?,
        None => send(state, turn, call, &args).await?,
    };
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    journal(state, &guard.connection, turn, call, &result)?;
    Ok(result)
}
