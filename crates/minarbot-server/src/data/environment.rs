use super::*;
use crate::mcp::client;
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn initialize(db: &Connection) -> std::result::Result<(), rusqlite::Error> {
    db.execute_batch("PRAGMA secure_delete=ON; CREATE TABLE IF NOT EXISTS project_environment(project TEXT NOT NULL,name TEXT NOT NULL,ciphertext TEXT NOT NULL,updated INTEGER NOT NULL,PRIMARY KEY(project,name)) STRICT; CREATE TABLE IF NOT EXISTS project_environment_redactions(project TEXT NOT NULL,name TEXT NOT NULL,ciphertext TEXT NOT NULL,PRIMARY KEY(project,name,ciphertext)) STRICT; CREATE TABLE IF NOT EXISTS project_environment_settings(project TEXT PRIMARY KEY,expose_computer INTEGER NOT NULL DEFAULT 0,revision INTEGER NOT NULL DEFAULT 1) STRICT; CREATE TABLE IF NOT EXISTS project_environment_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;")?;
    db.execute("INSERT OR IGNORE INTO project_environment_redactions SELECT project,name,ciphertext FROM project_environment", [])?;
    Ok(())
}
fn name(v: &Value) -> Result<&str> {
    let value = v
        .as_str()
        .filter(|s| {
            s.len() <= 64
                && s.bytes()
                    .all(|b| b == b'_' || b.is_ascii_uppercase() || b.is_ascii_digit())
                && s.as_bytes()
                    .first()
                    .is_some_and(|b| b == &b'_' || b.is_ascii_uppercase())
        })
        .ok_or_else(|| {
            error(
                400,
                "Environment name is reserved or invalid. Use A-Z, 0-9, and _.",
            )
        })?;
    if [
        "PATH",
        "HOME",
        "SHELL",
        "USER",
        "LOGNAME",
        "PWD",
        "OLDPWD",
        "TMPDIR",
        "TMP",
        "TEMP",
        "ENV",
        "BASH_ENV",
        "SHELLOPTS",
        "BASHOPTS",
        "IFS",
        "CDPATH",
        "GLOBIGNORE",
    ]
    .contains(&value)
        || ["LD_", "DYLD_", "NODE_", "MINARBOT_", "PYTHON"]
            .iter()
            .any(|prefix| value.starts_with(prefix))
    {
        return Err(error(
            400,
            "Environment name is reserved or invalid. Use A-Z, 0-9, and _.",
        ));
    }
    Ok(value)
}
fn check_project(db: &Connection, id: &str) -> Result<()> {
    if !uuid(id)
        || !exists(
            db,
            "SELECT 1 FROM local_projects WHERE id=? AND deleted=0 AND archived=0",
            [id],
        )?
    {
        return Err(error(404, "Active project not found."));
    }
    Ok(())
}
fn command(
    db: &Connection,
    id: &str,
    request: &Value,
    run: impl FnOnce() -> Result<Value>,
) -> Result<Value> {
    transaction(db, || {
        if let Some(old) = one(
            db,
            "SELECT request,result FROM project_environment_commands WHERE id=?",
            [id],
        )? {
            if crate::data::json(&old["request"])? != *request {
                return Err(error(
                    409,
                    "Request ID already used for another environment change.",
                ));
            }
            return crate::data::json(&old["result"]);
        }
        let result = run()?;
        db.execute(
            "INSERT INTO project_environment_commands VALUES(?,?,?)",
            rusqlite::params![id, request.to_string(), result.to_string()],
        )?;
        Ok(result)
    })
}
pub fn route(
    state: &AppState,
    db: &Connection,
    project: &str,
    op: &str,
    v: &Value,
    headers: &HeaderMap,
    get: bool,
) -> Result<Value> {
    if headers.contains_key("authorization")
        || headers.contains_key("origin") && !origin(state, headers)
    {
        return Err(error(
            403,
            "Project integrations require the local Owner in Minarbot.",
        ));
    }
    let current = projects::read(db, project)?;
    if current["archived"] == true {
        return Err(error(409, "Project is archived."));
    }
    check_project(db, project)?;
    let settings = one(
        db,
        "SELECT expose_computer,revision FROM project_environment_settings WHERE project=?",
        [project],
    )?
    .unwrap_or(json!({"expose_computer":0,"revision":1}));
    if get {
        if op != "environment" {
            return Err(error(404, "Route not found."));
        }
        return Ok(
            json!({"items":all(db,"SELECT name,updated FROM project_environment WHERE project=? ORDER BY name",[project])?.into_iter().map(|mut row|{row["set"]=json!(true);row}).collect::<Vec<_>>(),"exposeComputer":settings["expose_computer"]==1,"revision":settings["revision"]}),
        );
    }
    let id = string(v, "id");
    if !uuid(id) {
        return Err(error(400, "Invalid environment request ID."));
    }
    match op {
        "remove" => {
            if !closed(v, &["id", "name", "updated", "confirmed"]) || v["confirmed"] != true {
                return Err(error(400, "Confirm environment deletion."));
            }
            let key = name(&v["name"])?;
            command(db, id, &json!({"project":project,"request":v}), || {
                let old = one(
                    db,
                    "SELECT updated FROM project_environment WHERE project=? AND name=?",
                    [project, key],
                )?
                .ok_or_else(|| error(409, "Environment changed. Reload before deleting."))?;
                if old["updated"] != v["updated"] {
                    return Err(error(409, "Environment changed. Reload before deleting."));
                }
                db.execute(
                    "DELETE FROM project_environment WHERE project=? AND name=?",
                    [project, key],
                )?;
                Ok(json!({"deleted":true}))
            })
        }
        "settings" => {
            if !closed(v, &["id", "revision", "exposeComputer", "confirmed"])
                || !v["exposeComputer"].is_boolean()
                || v["exposeComputer"] == true && v["confirmed"] != true
            {
                return Err(error(
                    400,
                    "Confirm exposing environment to the agent computer.",
                ));
            }
            command(db, id, &json!({"project":project,"request":v}), || {
                if settings["revision"] != v["revision"] {
                    return Err(error(
                        409,
                        "Environment setting changed. Reload before saving.",
                    ));
                }
                let rev = settings["revision"].as_i64().unwrap_or(1) + 1;
                db.execute("INSERT INTO project_environment_settings VALUES(?,?,?) ON CONFLICT(project) DO UPDATE SET expose_computer=excluded.expose_computer,revision=excluded.revision",rusqlite::params![project,v["exposeComputer"]==true,rev])?;
                Ok(json!({"exposeComputer":v["exposeComputer"],"revision":rev}))
            })
        }
        "environment" => {
            if !closed(v, &["id", "name", "value", "updated"]) || !v["value"].is_string() {
                return Err(error(400, "Invalid environment fields."));
            }
            let key = name(&v["name"])?;
            let value = v["value"].as_str().unwrap_or("");
            if value.len() > 16384 || value.contains('\0') {
                return Err(error(
                    400,
                    "Environment value must be text up to 16 KiB without null bytes.",
                ));
            }
            let mac = ring::hmac::sign(
                &ring::hmac::Key::new(
                    ring::hmac::HMAC_SHA256,
                    &client::key(state, true, "project-env-key")?,
                ),
                value.as_bytes(),
            );
            let digest = mac
                .as_ref()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            command(
                db,
                id,
                &json!({"project":project,"request":{"name":key,"digest":digest,"updated":v.get("updated")}}),
                || {
                    let old = one(
                        db,
                        "SELECT updated FROM project_environment WHERE project=? AND name=?",
                        [project, key],
                    )?;
                    if old
                        .as_ref()
                        .map(|r| r["updated"].clone())
                        .unwrap_or(Value::Null)
                        != v.get("updated").cloned().unwrap_or(Value::Null)
                    {
                        return Err(error(409, "Environment changed. Reload before replacing."));
                    }
                    if old.is_none()
                        && one(
                            db,
                            "SELECT count(*) AS n FROM project_environment WHERE project=?",
                            [project],
                        )?
                        .is_some_and(|r| r["n"].as_i64().unwrap_or(0) >= 128)
                    {
                        return Err(error(400, "Project environment limit is 128 values."));
                    }
                    let updated = state.config.now().max(
                        old.as_ref()
                            .and_then(|r| r["updated"].as_i64())
                            .unwrap_or(0)
                            + 1,
                    );
                    let encrypted = client::encrypt_with(
                        state,
                        value,
                        &format!("{project}:{key}"),
                        "project-env-key",
                    )?;
                    db.execute("INSERT INTO project_environment VALUES(?,?,?,?) ON CONFLICT(project,name) DO UPDATE SET ciphertext=excluded.ciphertext,updated=excluded.updated",rusqlite::params![project,key,encrypted,updated])?;
                    db.execute(
                        "INSERT OR REPLACE INTO project_environment_redactions VALUES(?,?,?)",
                        rusqlite::params![project, key, encrypted],
                    )?;
                    Ok(json!({"name":key,"updated":updated,"set":true}))
                },
            )
        }
        _ => Err(error(404, "Route not found.")),
    }
}

pub fn values(
    state: &AppState,
    db: &Connection,
    turn: &str,
    agent: &str,
) -> Result<std::collections::HashMap<String, String>> {
    turn_values(state, db, turn, agent, false)
}

pub fn computer_values(
    state: &AppState,
    db: &Connection,
    turn: &str,
    agent: &str,
) -> Result<std::collections::HashMap<String, String>> {
    turn_values(state, db, turn, agent, true)
}

fn turn_values(
    state: &AppState,
    db: &Connection,
    turn: &str,
    agent: &str,
    computer: bool,
) -> Result<std::collections::HashMap<String, String>> {
    let Some(project) = turn_project(db, turn, agent, computer)? else {
        return Ok(Default::default());
    };
    all(
        db,
        "SELECT name,ciphertext FROM project_environment WHERE project=?",
        [&project],
    )?
    .into_iter()
    .map(|r| Ok((string(&r, "name").to_owned(), decrypt(state, &r, &project)?)))
    .collect()
}

pub(crate) fn authorize_computer(db: &Connection, turn: &str, agent: &str) -> Result<()> {
    turn_project(db, turn, agent, true).map(drop)
}

fn turn_project(
    db: &Connection,
    turn: &str,
    agent: &str,
    computer: bool,
) -> Result<Option<String>> {
    let scope=one(db,"SELECT coalesce(p.project,c.project) AS project FROM turns t LEFT JOIN local_project_threads p ON p.thread=t.thread LEFT JOIN channels c ON c.id=t.channel WHERE t.id=?",[turn])?;
    let Some(project) = scope.as_ref().and_then(|r| r["project"].as_str()) else {
        return Ok(None);
    };
    if computer
        && !exists(
            db,
            "SELECT 1 FROM project_environment_settings WHERE project=? AND expose_computer=1",
            [project],
        )?
    {
        return Ok(None);
    }
    check_project(db, project)?;
    if !exists(
        db,
        "SELECT 1 FROM local_project_members WHERE project=? AND agent=?",
        [project, agent],
    )? {
        return Err(error(403, "Agent is not an active project member."));
    }
    Ok(Some(project.to_owned()))
}

pub fn decrypt(state: &AppState, row: &Value, project: &str) -> Result<String> {
    client::decrypt_with(
        state,
        string(row, "ciphertext"),
        &format!("{project}:{}", string(row, "name")),
        "project-env-key",
    )
    .map_err(|_| {
        error(
            409,
            "Project environment key is unavailable. Save values again on this host.",
        )
    })
}
