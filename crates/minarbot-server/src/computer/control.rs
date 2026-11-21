use super::{error, Result};
use crate::{
    files::{keys, one, rows, text, timestamp},
    web::Failure,
    AppState,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
fn db<T>(s: &AppState, work: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let g = s.database.lock().map_err(|_| Failure::storage())?;
    work(&g.connection)
}
fn tx<T>(s: &AppState, work: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    db(s, |db| super::transaction(db, work))
}
fn permission(c: &Connection, agent: &str) -> Result<Value> {
    Ok(one(
        c,
        "SELECT enabled,revision FROM agent_computers WHERE agent=?",
        [agent],
    )?
    .unwrap_or(json!({"enabled":0,"revision":0})))
}
fn control(c: &Connection, agent: &str, now: i64) -> Result<Value> {
    c.execute(
        "INSERT OR IGNORE INTO computer_control VALUES(?,'agent',?,?,0)",
        params![agent, agent, timestamp(c, now)?],
    )?;
    one(
        c,
        "SELECT mode,who,since,revision FROM computer_control WHERE agent=?",
        [agent],
    )?
    .ok_or_else(Failure::storage)
}
fn waits(c: &Connection, agent: &str) -> Result<Vec<Value>> {
    rows(c,"SELECT w.* FROM computer_waits w JOIN turns t ON t.id=w.turn WHERE w.agent=? AND w.state='waiting' AND t.status NOT IN('done','cancelled')",[agent])
}
pub fn enabled(s: &AppState, agent: &str) -> Result<bool> {
    Ok(s.computer.enabled() && db(s, |c| permission(c, agent))?["enabled"] == 1)
}
pub fn assert_enabled(s: &AppState, agent: &str) -> Result<()> {
    if !enabled(s, agent)? {
        return Err(error(403, "Computer tools are disabled for this agent."));
    }
    Ok(())
}
pub fn assert_recoverable(c: &Connection, turn: &str) -> Result<()> {
    if one(
        c,
        "SELECT 1 FROM computer_actions WHERE turn=? AND state='uncertain'",
        [turn],
    )?
    .is_some()
    {
        return Err(error(409,"Computer action outcome is uncertain. Inspect computer and send a new message; this task cannot replay input."));
    }
    Ok(())
}
pub fn turn_wait(c: &Connection, turn: &str) -> Result<Option<Value>> {
    one(c,"SELECT w.* FROM computer_waits w JOIN turns t ON t.id=w.turn WHERE w.turn=? AND w.state='waiting' AND t.status NOT IN('done','cancelled')",[turn])
}
fn read_db(s: &AppState, c: &Connection, agent: &str) -> Result<Value> {
    let mut value = s.computer.status(agent, s.config.now())?;
    let p = permission(c, agent)?;
    value["enabled"] = json!(p["enabled"] == 1);
    value["revision"] = p["revision"].clone();
    value["control"] = control(c, agent, s.config.now())?;
    value["requests"] = json!(waits(c, agent)?);
    Ok(value)
}
pub fn read(s: &AppState, agent: &str) -> Result<Value> {
    db(s, |c| read_db(s, c, agent))
}
fn command_id(v: &Value) -> bool {
    v["id"]
        .as_str()
        .is_some_and(|v| crate::web::id(v).is_some())
        && v["revision"]
            .as_i64()
            .is_some_and(|v| (0..=9007199254740991).contains(&v))
}
fn prior(c: &Connection, v: &Value, request: &str) -> Result<Option<Value>> {
    if let Some(p) = one(
        c,
        "SELECT * FROM computer_commands WHERE id=?",
        [text(v, "id")],
    )? {
        if p["request"] != request {
            return Err(error(409, "Request ID was already used."));
        }
        return serde_json::from_str(text(&p, "result"))
            .map(Some)
            .map_err(|_| Failure::storage());
    }
    Ok(None)
}
pub async fn configure(s: &AppState, agent: &str, v: &Value) -> Result<Value> {
    if !keys(v, &["id", "revision", "enabled"]) || !command_id(v) || !v["enabled"].is_boolean() {
        return Err(error(400, "Invalid computer setting."));
    }
    let request =
        json!({"agent":agent,"id":v["id"],"revision":v["revision"],"enabled":v["enabled"]})
            .to_string();
    if v["enabled"] == true && !s.computer.enabled() {
        return Err(error(403, "Set up Computer on this device first."));
    }
    let gate = s.computer.control_gate(agent)?;
    let _guard = gate.lock().await;
    if let Some(p) = db(s, |c| prior(c, v, &request))? {
        return Ok(p);
    }
    tx(s, |c| {
        let ctrl = control(c, agent, s.config.now())?;
        if v["enabled"] == false && (ctrl["mode"] == "user" || !waits(c, agent)?.is_empty()) {
            return Err(error(
                409,
                "Hand control back or dismiss the request before disabling Computer.",
            ));
        }
        if permission(c, agent)?["revision"] != v["revision"] {
            return Err(error(409, "Computer setting changed. Load latest status."));
        }
        c.execute("INSERT INTO agent_computers VALUES(?,?,?) ON CONFLICT(agent) DO UPDATE SET enabled=excluded.enabled,revision=excluded.revision",params![agent,if v["enabled"]==true{1}else{0},v["revision"].as_i64().unwrap_or(0)+1])?;
        let result = read_db(s, c, agent)?;
        c.execute(
            "INSERT INTO computer_commands VALUES(?,?,?)",
            params![text(v, "id"), request, result.to_string()],
        )?;
        Ok(())
    })?;
    if v["enabled"] == false {
        s.computer.stop(agent).await?;
    }
    read(s, agent)
}
pub async fn transition(s: &AppState, agent: &str, v: &Value) -> Result<Value> {
    let op = text(v, "operation");
    if !["take", "hand-back", "dismiss"].contains(&op) {
        return Err(error(400, "Invalid control operation."));
    }
    if !keys(v, &["id", "revision", "turn", "operation"])
        || !command_id(v)
        || if op == "dismiss" {
            crate::web::id(text(v, "turn")).is_none()
        } else {
            v.get("turn").is_some()
        }
    {
        return Err(error(400, "Invalid control request."));
    }
    let gate = s.computer.control_gate(agent)?;
    let _guard = gate.lock().await;
    assert_enabled(s, agent)?;
    let request=json!({"agent":agent,"operation":op,"id":v["id"],"revision":v["revision"],"turn":v["turn"]}).to_string();
    if db(s, |c| prior(c, v, &request))?.is_some() {
        return read(s, agent);
    }
    tx(s, |c| {
        let current = control(c, agent, s.config.now())?;
        if current["revision"] != v["revision"] {
            return Err(error(409, "Control changed. Load latest status."));
        }
        let waits = waits(c, agent)?;
        if op == "take" && current["mode"] != "agent"
            || op == "hand-back" && current["mode"] != "user"
            || op == "dismiss"
                && (current["mode"] != "agent" || !waits.iter().any(|w| w["turn"] == v["turn"]))
        {
            return Err(error(409, "Control request is no longer available."));
        }
        let mode = if op == "take" { "user" } else { "agent" };
        c.execute(
            "UPDATE computer_control SET mode=?,who=?,since=?,revision=revision+1 WHERE agent=?",
            params![
                mode,
                if mode == "user" { "owner" } else { agent },
                timestamp(c, s.config.now())?,
                agent
            ],
        )?;
        if op != "take" {
            for wait in waits
                .into_iter()
                .filter(|w| op != "dismiss" || w["turn"] == v["turn"])
            {
                let note = if op == "dismiss" {
                    "The user declined to take control. Continue without the human-only step or explain what is blocked."
                } else {
                    "The user handed control back. The screen may have changed; take a screenshot first."
                };
                c.execute(
                    "UPDATE computer_waits SET state=? WHERE turn=? AND state='waiting'",
                    params![
                        if op == "dismiss" {
                            "declined"
                        } else {
                            "returned"
                        },
                        text(&wait, "turn")
                    ],
                )?;
                c.execute("UPDATE flow_tool_calls SET result=? WHERE turn=? AND call_id=?",params![json!({"status":if op=="dismiss"{"user_declined"}else{"control_returned"},"message":note}).to_string(),text(&wait,"turn"),text(&wait,"call_id")])?;
                c.execute("UPDATE turns SET paused=0,revision=revision+1,error=NULL WHERE id=? AND status='queued' AND paused=1 AND revision=(SELECT pause_revision FROM computer_waits WHERE turn=?) AND NOT EXISTS(SELECT 1 FROM computer_actions WHERE turn=? AND state='uncertain')",params![text(&wait,"turn"),text(&wait,"turn"),text(&wait,"turn")])?;
            }
        }
        let value = read_db(s, c, agent)?;
        c.execute(
            "INSERT INTO computer_commands VALUES(?,?,?)",
            params![text(v, "id"), request, value.to_string()],
        )?;
        Ok(())
    })?;
    s.computer.viewers.revoke(agent)?;
    if let Some(d) = s.computer.get(agent)? {
        d.disconnected.send_modify(|n| *n += 1);
    }
    read(s, agent)
}
pub async fn input(s: &AppState, agent: &str, v: &Value) -> Result<Value> {
    let name = text(v, "name");
    if !keys(v, &["revision", "name", "args"])
        || v["revision"].as_i64().is_none_or(|n| n < 0)
        || ![
            "computer_click",
            "computer_type",
            "computer_key",
            "computer_scroll",
            "computer_open_url",
        ]
        .contains(&name)
    {
        return Err(error(400, "Invalid computer input."));
    }
    super::actions::validate(name, &v["args"], s.computer.width, s.computer.height)?;
    let _input = s.computer.admit_input(agent, s.config.now())?;
    let gate = s.computer.control_gate(agent)?;
    let _guard = gate.lock().await;
    assert_enabled(s, agent)?;
    let current = db(s, |c| control(c, agent, s.config.now()))?;
    if current["mode"] != "user" || current["revision"] != v["revision"] {
        return Err(error(409, "Take control before sending computer input."));
    }
    s.computer
        .action(agent, name, &v["args"], s.config.now())
        .await?;
    Ok(json!({"status":"done"}))
}
pub async fn open_result(s: &AppState, agent: &str, v: &Value) -> Result<Value> {
    super::actions::validate("computer_open_url", v, s.computer.width, s.computer.height)?;
    let gate = s.computer.control_gate(agent)?;
    let _guard = gate.lock().await;
    assert_enabled(s, agent)?;
    if db(s, |c| {
        one(
            c,
            "SELECT 1 FROM turns WHERE agent=? AND status IN('queued','running') AND paused=0",
            [agent],
        )
    })?
    .is_some()
    {
        return Err(error(
            409,
            "Computer is busy. Wait for the agent to finish before opening the result.",
        ));
    }
    s.computer
        .action(agent, "computer_open_url", v, s.config.now())
        .await?;
    Ok(json!({"status":"done"}))
}
pub struct Outcome {
    pub result: Value,
    pub image: Option<Vec<u8>>,
    pub paused: bool,
}
pub async fn action(
    s: &AppState,
    agent: &str,
    turn: &str,
    call: &Value,
    args: &Value,
    after_output: i64,
    after_action: i64,
) -> Result<Outcome> {
    let gate = s.computer.control_gate(agent)?;
    let guard = gate.lock_owned().await;
    assert_enabled(s, agent)?;
    db(s, |c| assert_recoverable(c, turn))?;
    let name = text(&call["function"], "name");
    super::actions::validate(name, args, s.computer.width, s.computer.height)?;
    let current = db(s, |c| control(c, agent, s.config.now()))?;
    if current["mode"] == "user" || name == "computer_request_user" {
        let result = json!({"status":"waiting_for_user","message":if name=="computer_request_user"{"The user must take control before this human-only step can continue."}else{"The user is controlling your computer. Wait until they hand it back."}});
        let wait = tx(s, |c| {
            let paused=one(c,"UPDATE turns SET status='queued',paused=1,revision=revision+1 WHERE id=? AND status='running' RETURNING revision",[turn])?.ok_or_else(||error(409,"Computer task stopped before waiting."))?;
            c.execute("INSERT INTO computer_waits VALUES(?,?,?,?,'waiting',?) ON CONFLICT(turn) DO UPDATE SET call_id=excluded.call_id,reason=excluded.reason,state='waiting',pause_revision=excluded.pause_revision",params![turn,agent,text(call,"id"),if name=="computer_request_user"{text(args,"reason")}else{""},paused["revision"].as_i64()])?;
            c.execute("INSERT INTO flow_tool_calls(turn,call_id,call,result,after_output,after_action) VALUES(?,?,?,?,?,?)",params![turn,text(call,"id"),call.to_string(),result.to_string(),after_output,after_action])?;
            crate::harness::config::CardWait::checkpoint(s, c, turn)
        })?;
        if let Some(wait) = wait {
            wait.keep_paused();
        }
        return Ok(Outcome {
            result,
            image: None,
            paused: true,
        });
    }
    let environment = db(s, |c| {
        crate::data::environment::computer_values(s, c, turn, agent)
    })?;
    tx(s, |c| {
        c.execute(
            "INSERT INTO computer_actions VALUES(?,?,'uncertain',NULL)",
            params![turn, text(call, "id")],
        )?;
        c.execute("INSERT INTO flow_tool_calls(turn,call_id,call,result,after_output,after_action) VALUES(?,?,?,?,?,?)",params![turn,text(call,"id"),call.to_string(),json!({"error":"Computer action outcome is uncertain. Do not repeat this input."}).to_string(),after_output,after_action])?;
        Ok(())
    })?;
    let now = s.config.now();
    let ((image, title), _guard) = if environment.is_empty() {
        let result = s.computer.action(agent, name, args, now).await?;
        (result, guard)
    } else {
        let computer = s.computer.clone();
        let (agent, name, args) = (agent.to_owned(), name.to_owned(), args.clone());
        let (_in_progress, cancelled) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let result = computer
                .action_with_environment(&agent, &name, &args, now, &environment, cancelled)
                .await?;
            Ok::<_, Failure>((result, guard))
        })
        .await
        .map_err(|_| Failure::storage())??
    };
    let mut result = json!({"status":"done"});
    if let Some(title) = title {
        result["pageTitle"] = title.into();
        result["note"] = "Page finished loading. Take a screenshot to see it.".into();
    }
    if image.is_some() {
        result["note"] =
            "Desktop screenshot follows as an image. Coordinates use original desktop pixels."
                .into();
    }
    tx(s, |c| {
        c.execute(
            "UPDATE computer_actions SET state='done',image=? WHERE turn=? AND call_id=?",
            params![image, turn, text(call, "id")],
        )?;
        c.execute(
            "UPDATE flow_tool_calls SET result=? WHERE turn=? AND call_id=?",
            params![result.to_string(), turn, text(call, "id")],
        )?;
        Ok(())
    })?;
    Ok(Outcome {
        result,
        image,
        paused: false,
    })
}
