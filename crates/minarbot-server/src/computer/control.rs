use super::{error, vault, Result};
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
        "SELECT enabled,revision,EXISTS(SELECT 1 FROM computer_shared_profiles WHERE agent=agent_computers.agent) shared_profile FROM agent_computers WHERE agent=?",
        [agent],
    )?
    .unwrap_or(json!({"enabled":0,"revision":0})))
}
fn pinned(c: &Connection, agent: &str) -> Result<bool> {
    Ok(one(c, "SELECT 1 FROM computer_always_on WHERE agent=?", [agent])?.is_some())
}
pub(super) fn always_on(s: &AppState, agent: &str) -> Result<bool> {
    db(s, |c| {
        Ok(pinned(c, agent)? && !crate::data::agents::hidden(c, agent)?)
    })
}
fn at_capacity(s: &AppState, c: &Connection, agent: &str) -> Result<bool> {
    let pinned = c.query_row("SELECT count(*) FROM computer_always_on", [], |r| r.get(0))?;
    s.computer.at_capacity(agent, pinned)
}
pub(super) async fn restore_always_on(s: &AppState) {
    let Ok(agents) = db(s, |c| {
        rows(c, "SELECT agent FROM computer_always_on ORDER BY agent", [])
    }) else {
        return;
    };
    for row in agents {
        let agent = text(&row, "agent");
        let result = async {
            let gate = s.computer.control_gate(agent)?;
            let _guard = gate.lock().await;
            if always_on(s, agent)? && enabled(s, agent)? {
                s.computer.start(agent, s.config.now()).await?;
            }
            Ok::<_, Failure>(())
        }
        .await;
        if let Err(error) = result {
            eprintln!(
                "Always-on Computer could not start for {agent}: {}",
                error.message
            );
        }
    }
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
    rows(c,"SELECT w.*,v.entry AS secret_entry,v.origin AS secret_origin FROM computer_waits w JOIN turns t ON t.id=w.turn LEFT JOIN vault_requests v ON v.turn=w.turn AND v.call_id=w.call_id WHERE w.agent=? AND w.state='waiting' AND t.status NOT IN('done','cancelled')",[agent])
}
pub fn enabled(s: &AppState, agent: &str) -> Result<bool> {
    Ok(s.computer.enabled && db(s, |c| permission(c, agent))?["enabled"] == 1)
}
pub fn assert_enabled(s: &AppState, agent: &str) -> Result<()> {
    if !crate::platform::LINUX_SANDBOX {
        return Err(error(503, crate::platform::COMPUTER_UNAVAILABLE));
    }
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
    one(c,"SELECT w.*,v.entry AS secret_entry,v.origin AS secret_origin FROM computer_waits w JOIN turns t ON t.id=w.turn LEFT JOIN vault_requests v ON v.turn=w.turn AND v.call_id=w.call_id WHERE w.turn=? AND w.state='waiting' AND t.status NOT IN('done','cancelled')",[turn])
}
fn read_db(s: &AppState, c: &Connection, agent: &str) -> Result<Value> {
    let mut value = s.computer.status(agent, s.config.now())?;
    let p = permission(c, agent)?;
    let always_on = pinned(c, agent)?;
    value["alwaysOn"] = json!(always_on);
    value["alwaysOnLimitReached"] =
        json!(!always_on && p["enabled"] == 1 && at_capacity(s, c, agent)?);
    if always_on {
        value["stopsInMs"] = Value::Null;
    }
    value["enabled"] = json!(p["enabled"] == 1);
    value["revision"] = p["revision"].clone();
    value["sharedProfile"] = json!(p["shared_profile"] == 1);
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
    if v.get("alwaysOn").is_some() {
        return configure_always_on(s, agent, v).await;
    }
    if !keys(v, &["id", "revision", "enabled", "sharedProfile"])
        || !command_id(v)
        || !v["enabled"].is_boolean()
        || v.get("sharedProfile")
            .is_some_and(|value| !value.is_boolean())
    {
        return Err(error(400, "Invalid computer setting."));
    }
    let mut request =
        json!({"agent":agent,"id":v["id"],"revision":v["revision"],"enabled":v["enabled"]});
    if let Some(shared) = v.get("sharedProfile") {
        request["sharedProfile"] = shared.clone();
    }
    let request = request.to_string();
    if v["enabled"] == true && !s.computer.enabled {
        if !crate::platform::LINUX_SANDBOX {
            return Err(error(503, crate::platform::COMPUTER_UNAVAILABLE));
        }
        return Err(error(
            403,
            "Enable MINARBOT_AGENT_DESKTOP=1 on the server first.",
        ));
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
        if let Some(shared) = v["sharedProfile"].as_bool() {
            if (permission(c, agent)?["shared_profile"] == 1) != shared
                && (s.computer.get(agent)?.is_some() || !waits(c, agent)?.is_empty())
            {
                return Err(error(
                    409,
                    "Stop Computer and resolve pending requests before switching profiles.",
                ));
            }
            if shared {
                c.execute(
                    "INSERT OR IGNORE INTO computer_shared_profiles VALUES(?)",
                    [agent],
                )?;
            } else {
                c.execute(
                    "DELETE FROM computer_shared_profiles WHERE agent=?",
                    [agent],
                )?;
            }
        }
        c.execute("INSERT INTO agent_computers VALUES(?,?,?) ON CONFLICT(agent) DO UPDATE SET enabled=excluded.enabled,revision=excluded.revision",params![agent,if v["enabled"]==true{1}else{0},v["revision"].as_i64().unwrap_or(0)+1])?;
        if v["enabled"] == false {
            c.execute("DELETE FROM computer_always_on WHERE agent=?", [agent])?;
        }
        let result = read_db(s, c, agent)?;
        c.execute(
            "INSERT INTO computer_commands VALUES(?,?,?)",
            params![text(v, "id"), request, result.to_string()],
        )?;
        Ok(())
    })?;
    if let Some(shared) = v["sharedProfile"].as_bool() {
        s.computer.set_shared_profile(agent, shared)?;
    }
    if v["enabled"] == false {
        s.computer.stop(agent).await?;
    }
    read(s, agent)
}
async fn configure_always_on(s: &AppState, agent: &str, v: &Value) -> Result<Value> {
    if !keys(v, &["id", "revision", "alwaysOn"]) || !command_id(v) || !v["alwaysOn"].is_boolean() {
        return Err(error(400, "Invalid always-on setting."));
    }
    let request =
        json!({"agent":agent,"id":v["id"],"revision":v["revision"],"alwaysOn":v["alwaysOn"]})
            .to_string();
    let _capacity = s.computer.always_on_gate.lock().await;
    let gate = s.computer.control_gate(agent)?;
    let _guard = gate.lock().await;
    if db(s, |c| prior(c, v, &request))?.is_some() {
        return read(s, agent);
    }
    let pin = v["alwaysOn"] == true;
    db(s, |c| {
        let permission = permission(c, agent)?;
        if permission["revision"] != v["revision"] {
            return Err(error(409, "Computer setting changed. Load latest status."));
        }
        if pin
            && (!s.computer.enabled
                || permission["enabled"] != 1
                || crate::data::agents::hidden(c, agent)?)
        {
            return Err(error(403, "Enable Computer for this agent first."));
        }
        if pin && !pinned(c, agent)? && at_capacity(s, c, agent)? {
            return Err(error(
                429,
                "Desktop limit reached. Stop another computer first.",
            ));
        }
        Ok(())
    })?;
    if pin {
        s.computer.start(agent, s.config.now()).await?;
    }
    tx(s, |c| {
        if pin {
            c.execute(
                "INSERT OR IGNORE INTO computer_always_on VALUES(?)",
                [agent],
            )?;
        } else {
            c.execute("DELETE FROM computer_always_on WHERE agent=?", [agent])?;
        }
        c.execute(
            "UPDATE agent_computers SET revision=revision+1 WHERE agent=?",
            [agent],
        )?;
        let value = read_db(s, c, agent)?;
        c.execute(
            "INSERT INTO computer_commands VALUES(?,?,?)",
            params![text(v, "id"), request, value.to_string()],
        )?;
        Ok(value)
    })
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
            for wait in waits.into_iter().filter(|w| {
                if op == "dismiss" {
                    w["turn"] == v["turn"]
                } else {
                    w["secret_entry"].is_null()
                }
            }) {
                let note = if op == "dismiss" && !wait["secret_entry"].is_null() {
                    "The owner declined the secure form. Continue without the value or explain what is blocked."
                } else if op == "dismiss" {
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
    let command = super::actions::validate(name, args, s.computer.width, s.computer.height)?;
    if name == "computer_list_vault" {
        let result = db(s, |c| vault::choices(s, c, agent))?;
        tx(s, |c| {
            journal(c, turn, call, &result, after_output, after_action)
        })?;
        return Ok(Outcome {
            result,
            image: None,
            paused: false,
        });
    }
    let current = db(s, |c| control(c, agent, s.config.now()))?;
    let secret_request = if name == "computer_request_secret" && current["mode"] == "agent" {
        let [entry, origin] = command.as_slice() else {
            return Err(Failure::storage());
        };
        if db(s, |c| {
            crate::data::exists(c, "SELECT 1 FROM vault_entries WHERE name=?", [entry])
        })? {
            return Err(error(409, "Choose an unused Vault entry name."));
        }
        let token = uuid::Uuid::new_v4().to_string();
        let desktop = s.computer.secure_target(agent, origin, &token).await?;
        Some((entry, origin, desktop, token))
    } else {
        None
    };
    if current["mode"] == "user" || name == "computer_request_user" || secret_request.is_some() {
        let message = if secret_request.is_some() {
            "Waiting for the owner to complete the secure form in Minarbot."
        } else if name == "computer_request_user" {
            "The user must take control before this human-only step can continue."
        } else {
            "The user is controlling your computer. Wait until they hand it back."
        };
        let result = json!({"status":"waiting_for_user","message":message});
        let wait = tx(s, |c| {
            let paused=one(c,"UPDATE turns SET status='queued',paused=1,revision=revision+1 WHERE id=? AND status='running' RETURNING revision",[turn])?.ok_or_else(||error(409,"Computer task stopped before waiting."))?;
            c.execute("INSERT INTO computer_waits VALUES(?,?,?,?,'waiting',?) ON CONFLICT(turn) DO UPDATE SET call_id=excluded.call_id,reason=excluded.reason,state='waiting',pause_revision=excluded.pause_revision",params![turn,agent,text(call,"id"),if name=="computer_request_user"{text(args,"reason")}else{""},paused["revision"].as_i64()])?;
            if let Some((entry, origin, desktop, token)) = secret_request {
                c.execute(
                    "INSERT OR REPLACE INTO vault_requests VALUES(?,?,?,?,?,?)",
                    params![turn, text(call, "id"), entry, origin, desktop, token],
                )?;
            }
            journal(c, turn, call, &result, after_output, after_action)?;
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
    if name == "computer_screenshot" && s.computer.screenshots_hidden(agent)? {
        return Err(error(409, "Screenshots are hidden after secure fill. Ask the owner to check Computer or restart it to start a new session."));
    }
    if name == "computer_fill_secret" {
        let [entry, field] = command.as_slice() else {
            return Err(Failure::storage());
        };
        let (origin, value) = db(s, |c| {
            let resolved = vault::resolve(s, c, agent, entry, field, s.config.now())?;
            super::transaction(c, |c| {
                mark_uncertain(c, turn, text(call, "id"))?;
                journal(
                    c,
                    turn,
                    call,
                    &json!({"error":"Secure fill outcome is uncertain. Do not repeat this input."}),
                    after_output,
                    after_action,
                )
            })?;
            Ok(resolved)
        })?;
        let filled = s.computer.secure_fill(agent, &origin, None, value).await?;
        let result = secret_result(entry, field, filled, false);
        tx(s, |c| complete_secret(c, turn, text(call, "id"), &result))?;
        return Ok(Outcome {
            result,
            image: None,
            paused: false,
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
        result["pageTitle"] = db(s, |c| {
            Ok(crate::mcp::secrets::redact(
                &title,
                &vault::redactions(s, c)?,
                false,
            ))
        })?
        .into();
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

fn journal(
    c: &Connection,
    turn: &str,
    call: &Value,
    result: &Value,
    after_output: i64,
    after_action: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO flow_tool_calls(turn,call_id,call,result,after_output,after_action) VALUES(?,?,?,?,?,?)",
        params![turn,text(call,"id"),call.to_string(),result.to_string(),after_output,after_action],
    )?;
    Ok(())
}

fn mark_uncertain(c: &Connection, turn: &str, call: &str) -> Result<()> {
    c.execute(
        "INSERT INTO computer_actions VALUES(?,?,'uncertain',NULL)",
        params![turn, call],
    )?;
    Ok(())
}

fn complete_secret(c: &Connection, turn: &str, call: &str, result: &Value) -> Result<()> {
    c.execute(
        "UPDATE computer_actions SET state='done' WHERE turn=? AND call_id=?",
        [turn, call],
    )?;
    c.execute(
        "UPDATE flow_tool_calls SET result=? WHERE turn=? AND call_id=?",
        params![result.to_string(), turn, call],
    )?;
    Ok(())
}

fn secret_result(entry: &str, field: &str, filled: bool, saved: bool) -> Value {
    let status = if filled {
        "done"
    } else if saved {
        "saved_not_filled"
    } else {
        "not_filled"
    };
    let message = if filled {
        "Filled the focused field. Screenshots are hidden for this desktop session."
    } else {
        "Focus an editable field on the saved origin before filling."
    };
    json!({"status":status,"placeholder":vault::placeholder(entry, field),"message":message})
}

pub(super) async fn respond_secret(s: &AppState, body: &Value) -> Result<()> {
    let (agent, turn, call, value) = (
        text(body, "agent"),
        text(body, "turn"),
        text(body, "call"),
        text(body, "value"),
    );
    if !keys(body, &["agent", "turn", "call", "value"])
        || crate::web::id(agent).is_none()
        || crate::web::id(turn).is_none()
        || !(1..=200).contains(&call.len())
        || !vault::value(value)
    {
        return Err(error(400, "Enter a valid value."));
    }
    let _gate = s.computer.control_gate(agent)?.lock_owned().await;
    assert_enabled(s, agent)?;
    let wait = db(s, |c| {
        let wait = one(
            c,
            "SELECT v.*,w.pause_revision FROM vault_requests v
            JOIN computer_waits w ON w.turn=v.turn AND w.call_id=v.call_id
            JOIN turns t ON t.id=w.turn
            WHERE w.turn=? AND w.agent=? AND w.call_id=? AND w.state='waiting'
            AND t.status='queued' AND t.paused=1 AND t.revision=w.pause_revision",
            [turn, agent, call],
        )?
        .ok_or_else(|| error(409, "This secure request is no longer active."))?;
        if control(c, agent, s.config.now())?["mode"] != "agent" {
            return Err(error(409, "Hand control back before filling this value."));
        }
        assert_recoverable(c, turn)?;
        let key = vault::key(s, c, true)?;
        super::transaction(c, |c| {
            vault::store_requested(
                c,
                &key,
                text(&wait, "entry"),
                agent,
                text(&wait, "origin"),
                value,
            )?;
            mark_uncertain(c, turn, call)?;
            c.execute(
                "UPDATE flow_tool_calls SET result=? WHERE turn=? AND call_id=?",
                params![
                    json!({"error":"Secure fill outcome is uncertain. Do not repeat this input."})
                        .to_string(),
                    turn,
                    call
                ],
            )?;
            Ok(())
        })?;
        Ok(wait)
    })?;
    let filled = s
        .computer
        .secure_fill(
            agent,
            text(&wait, "origin"),
            Some((text(&wait, "desktop"), text(&wait, "target"))),
            value.to_owned(),
        )
        .await;
    let uncertain = filled.is_err();
    tx(s, |c| {
        if let Ok(filled) = filled {
            complete_secret(
                c,
                turn,
                call,
                &secret_result(text(&wait, "entry"), "value", filled, true),
            )?;
        }
        c.execute("UPDATE computer_waits SET state='returned' WHERE turn=? AND call_id=? AND state='waiting'", [turn,call])?;
        if uncertain {
            c.execute("UPDATE turns SET status='interrupted',error='Value saved. Secure fill outcome is uncertain. Check Computer before starting a new task.',paused=0,revision=revision+1 WHERE id=? AND status='queued' AND paused=1 AND revision=?", params![turn,wait["pause_revision"].as_i64()])?;
        } else {
            c.execute("UPDATE turns SET paused=0,revision=revision+1,error=NULL WHERE id=? AND status='queued' AND paused=1 AND revision=?", params![turn,wait["pause_revision"].as_i64()])?;
        }
        Ok(())
    })?;
    if uncertain {
        return Err(error(
            503,
            "Value saved. Secure fill outcome is uncertain. Check Computer before trying again.",
        ));
    }
    Ok(())
}
