#![allow(dead_code)]

use super::Result;
use super::{features, guide_infra, validate};
use crate::{
    data::{self, all, closed, error, exists, one, string, transaction, uuid},
    web::Failure,
    AppState,
};
use axum::{extract::Request, http::Method, response::Response, routing::any, Router};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

pub const ID: &str = "00000000-0000-4000-8000-000000000005";
pub const NAME: &str = "Minarbot Guide";
pub const PROFILE: &str = "Setup and help";
const TTL_MS: i64 = 15 * 60 * 1000;
const PAGES: &str = concat!(
    include_str!("../../../../app/guide/agents.md"),
    "\n",
    include_str!("../../../../app/guide/connections.md"),
    "\n",
    include_str!("../../../../app/guide/infrastructure.md"),
    "\n",
    include_str!("../../../../app/guide/work.md"),
    "\n",
    include_str!("../../../../app/guide/settings.md"),
);
const SCREENS: &str = include_str!("../../../web/assets/guide-screens.json");
const WRITES: &[&str] = &[
    "create_agent",
    "update_agent",
    "create_routine",
    "update_routine",
    "pause_routine",
    "resume_routine",
    "connect_app",
    "set_connected_app_access",
];

pub fn initialize(db: &Connection) -> Result<()> {
    transaction(db, || {
        db.execute(
            "INSERT INTO agents(id,name,model,instructions,provider_instance,model_options,web_access) VALUES(?,?,'',?,'','{}',0)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name",
            [ID, NAME, PROFILE],
        )?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS guide_changes(
                id TEXT PRIMARY KEY, turn TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
                request TEXT NOT NULL, plan TEXT NOT NULL, digest TEXT NOT NULL,
                expires INTEGER NOT NULL, status TEXT NOT NULL DEFAULT 'pending'
                    CHECK(status IN ('pending','applying','applied','cancelled','expired','failed','unknown')),
                UNIQUE(turn,request)
            ) STRICT;
            CREATE TABLE IF NOT EXISTS guide_screens(
                turn TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE, screen TEXT NOT NULL,
                PRIMARY KEY(turn,screen)
            ) STRICT;
            UPDATE guide_changes SET status='unknown' WHERE status='applying';",
        )?;
        Ok(())
    })
}

pub fn inherit_connection(_state: &AppState, db: &Connection) -> Result<()> {
    if !exists(db, "SELECT 1 FROM agents WHERE id=? AND model=''", [ID])? {
        return Ok(());
    }
    let profiles = all(db, "SELECT model,provider_instance FROM agents a
        WHERE id<>? AND model<>'' AND NOT EXISTS(SELECT 1 FROM agent_visibility v WHERE v.agent=a.id AND v.hidden=1)
        ORDER BY rowid", [ID])?;
    for profile in profiles {
        let instance = string(&profile, "provider_instance");
        if db
            .query_row(
                "SELECT 1 FROM provider_instances WHERE id=? AND enabled=1",
                [instance],
                |_| Ok(()),
            )
            .is_ok()
        {
            db.execute("UPDATE agents SET model=?,provider_instance=?,revision=revision+1 WHERE id=? AND model=''",
                params![string(&profile,"model"),instance,ID])?;
            break;
        }
    }
    Ok(())
}

pub fn detach_connection(db: &Connection, binding: &str) -> Result<()> {
    db.execute(
        "UPDATE agents SET model='',provider_instance='',revision=revision+1 WHERE id=? AND provider_instance=?",
        [ID, binding],
    )?;
    Ok(())
}

pub fn eligible(turn: &Value) -> bool {
    turn["agent"] == ID
        && ["channel", "thread", "parent", "origin", "workflow_run"]
            .iter()
            .all(|key| turn[key].is_null())
}

pub fn instructions() -> String {
    format!(
        "You are {NAME}, the built-in setup and help assistant for Minarbot. \
         In onboarding, ask what work the user wants to do, what output they need and when. \
         Suggest a fitting starter template, instructions, available model and runtime, connected apps, and routines or triggers as needed. \
         Use list_guide_connections for available provider account IDs and your current connection. \
         Use feature tools to propose only the requested changes. Each write returns a proposed change set. \
         Tell the user to use Apply or Cancel in Minarbot. Text agreement cannot apply it. \
         Propose prerequisites first. Use list_guide_agents to check that an agent was created before proposing its routines or app access. \
         Direct users to Vault for SSH passwords. Ask for the Vault entry name only. Keep credentials and pairing codes out of chat. \
         Use a status tool to verify setup after Apply. Browser-only install commands and QR codes are available through the setup cards or Settings. \
         For sign-ins or settings without a tool, open the appropriate screen. \
         For help, use this guide and open_guide_screen to attach a screen link when useful. \
         Report proposed, applied and cancelled changes accurately.\n\n{PAGES}"
    )
}

fn screens() -> Result<Value> {
    serde_json::from_str(SCREENS).map_err(|_| Failure::storage())
}

pub fn declarations() -> Result<Vec<Value>> {
    let mut specs: Vec<Value> =
        serde_json::from_str(include_str!("feature-tools.json")).map_err(|_| Failure::storage())?;
    specs.retain(|spec| {
        let name = string(spec, "name");
        WRITES.contains(&name)
            || ["read_agent", "list_routines", "list_connected_apps"].contains(&name)
    });
    let uuid = json!({"type":"string","pattern":"^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$"});
    let toolkit = json!({"type":"string","pattern":"^[a-z0-9_]{1,64}$"});
    let catalog = screens()?;
    let screen_ids = catalog
        .as_object()
        .ok_or_else(Failure::storage)?
        .keys()
        .collect::<Vec<_>>();
    specs.extend([
        json!({"name":"list_guide_agents","description":"List up to 30 visible agents after an optional ID.","inputSchema":{"type":"object","properties":{"after":uuid},"additionalProperties":false}}),
        json!({"name":"list_guide_connections","description":"List configured provider accounts and the connection running this conversation.","inputSchema":{"type":"object","properties":{},"additionalProperties":false}}),
        json!({"name":"open_guide_screen","description":"Attach a user-clickable link to a screen in the product guide.","inputSchema":{"type":"object","properties":{"screen":{"enum":screen_ids}},"required":["screen"],"additionalProperties":false}}),
        json!({"name":"connect_app","description":"Propose starting sign-in to a Composio app. After Apply the user finishes sign-in in their browser.","inputSchema":{"type":"object","properties":{"requestId":uuid,"toolkit":toolkit},"required":["requestId","toolkit"],"additionalProperties":false}}),
        json!({"name":"set_connected_app_access","description":"Propose enabling or disabling an existing connected app for an agent. Read its current revision using list_connected_apps first (0 if absent).","inputSchema":{"type":"object","properties":{"requestId":uuid,"agent":uuid,"toolkit":toolkit,"revision":{"type":"integer","minimum":0},"enabled":{"type":"boolean"}},"required":["requestId","agent","toolkit","revision","enabled"],"additionalProperties":false}}),
        json!({"name":"add_guide_machine","description":"Propose adding a machine by name and its exact HTTPS origin. Apply reveals a one-time install command in the card and tracks pairing health.","inputSchema":{"type":"object","properties":{"requestId":uuid,"name":{"type":"string","minLength":1,"maxLength":120},"origin":{"type":"string","maxLength":2048}},"required":["requestId","name","origin"],"additionalProperties":false}}),
        json!({"name":"install_guide_vps","description":"Propose a fresh Linux Minarbot install over SSH. Ask for SSH host, user, trusted Ed25519 host public key, and the VPS HTTPS origin. Reference the Vault secret by name. Password goes only in Vault. Default ports are SSH 22 and Minarbot 4317.","inputSchema":{"type":"object","properties":{"requestId":uuid,"host":{"type":"string","minLength":1,"maxLength":253},"user":{"type":"string","minLength":1,"maxLength":64},"port":{"type":"integer","minimum":1,"maximum":65535},"servicePort":{"type":"integer","minimum":1,"maximum":65535},"origin":{"type":"string","maxLength":2048},"hostKey":{"type":"string","maxLength":100},"vaultEntry":{"type":"string","pattern":"^[a-z0-9-]{1,64}$"}},"required":["requestId","host","user","origin","hostKey","vaultEntry"],"additionalProperties":false}}),
        json!({"name":"configure_guide_remote_access","description":"Propose adding the exact origin found by inspect_guide_remote_access to launcher.env. Verify after restart and offer Devices pairing QR.","inputSchema":{"type":"object","properties":{"requestId":uuid,"origin":{"type":"string","maxLength":2048}},"required":["requestId","origin"],"additionalProperties":false}}),
        json!({"name":"setup_guide_computer","description":"Propose starting Computer setup. Apply runs the existing setup flow and reports missing programs.","inputSchema":{"type":"object","properties":{"requestId":uuid},"required":["requestId"],"additionalProperties":false}}),
        json!({"name":"list_guide_machines","description":"Check pairing and health. Filter by the target HTTPS origin when adding a machine.","inputSchema":{"type":"object","properties":{"origin":{"type":"string","maxLength":2048}},"additionalProperties":false}}),
        json!({"name":"inspect_guide_remote_access","description":"Inspect Tailscale sign-in, MagicDNS, HTTPS Serve for Minarbot and whether its pairing page is reachable from this server.","inputSchema":{"type":"object","properties":{},"additionalProperties":false}}),
        json!({"name":"read_guide_computer_setup","description":"Read Computer setup state and missing programs.","inputSchema":{"type":"object","properties":{},"additionalProperties":false}}),
    ]);
    for spec in &mut specs {
        let name = string(spec, "name").to_owned();
        if ["create_agent", "update_agent"].contains(&name.as_str()) {
            let props = &mut spec["inputSchema"]["properties"];
            props["providerInstance"] = json!({"type":"string","maxLength":64});
            props["model"] = json!({"type":"string","minLength":1,"maxLength":160});
        }
        if name == "create_routine" {
            spec["inputSchema"]["required"]
                .as_array_mut()
                .ok_or_else(Failure::storage)?
                .push(json!("agent"));
            spec["description"] = json!(
                "Propose a routine owned by an existing agent. Manual triggers need no schedule."
            );
        }
        if WRITES.contains(&name.as_str()) || guide_infra::WRITES.contains(&name.as_str()) {
            spec["description"] = json!(format!(
                "{} Requires Apply in Minarbot before any settings change.",
                string(spec, "description")
            ));
        }
    }
    Ok(specs)
}

fn preview(plan: &Value) -> Result<Value> {
    if plan.get("guideOperation").is_some() {
        return Ok(
            json!({"tool":plan["tool"],"target":plan["target"],"settings":plan["settings"]}),
        );
    }
    let mut settings = plan["body"]
        .as_object()
        .cloned()
        .ok_or_else(Failure::storage)?;
    settings.remove("id");
    Ok(json!({"tool":plan["tool"],"target":plan["target"],"settings":settings}))
}

fn plan(db: &Connection, name: &str, args: &Value, id: &str) -> Result<Value> {
    if args["agent"] == ID {
        return Err(error(400, "Choose a regular agent."));
    }
    let agent = args["agent"]
        .as_str()
        .map(|id| data::agents::read(db, id))
        .transpose()?;
    let mut plan = match name {
        "connect_app" => {
            json!({"path":format!("/api/connected-apps/connect/{}",string(args,"toolkit")),"body":{"id":id}})
        }
        "set_connected_app_access" => {
            json!({"path":format!("/api/agents/{}/connected-apps/{}",string(args,"agent"),string(args,"toolkit")),"body":{"id":id,"revision":args["revision"],"enabled":args["enabled"]}})
        }
        _ => features::plan(db, &json!({"kind":"peer"}), name, args, id)?,
    };
    plan["tool"] = json!(name);
    plan["target"] = match name {
        "connect_app" => json!({"app":args["toolkit"]}),
        "set_connected_app_access" => {
            json!({"agent":args["agent"],"name":agent.as_ref().map(|a|&a["name"]),"app":args["toolkit"]})
        }
        "create_agent" => Value::Null,
        "update_agent" | "create_routine" => {
            json!({"agent":args["agent"],"name":agent.as_ref().map(|a|&a["name"])})
        }
        _ => {
            let routine = one(
                db,
                "SELECT id,agent,name FROM routines WHERE id=?",
                [string(args, "routine")],
            )?
            .ok_or_else(|| error(404, "Routine not found."))?;
            if routine["agent"] == ID {
                return Err(error(400, "Choose a routine owned by a regular agent."));
            }
            routine
        }
    };
    if name == "create_agent" {
        let guide = data::agents::read(db, ID)?;
        plan["body"]["providerInstance"] = args
            .get("providerInstance")
            .unwrap_or(&guide["providerInstance"])
            .clone();
    } else if name == "create_routine" {
        if plan["body"]["trigger"].is_null() {
            plan["body"]["trigger"] = json!("schedule");
        }
        if plan["body"]["reportTo"].is_null() {
            plan["body"]["reportTo"] = json!("run");
        }
        if matches!(plan["body"]["trigger"].as_str(), Some("webhook" | "script"))
            && plan["body"]["schedule"].is_null()
        {
            plan["body"]["nextAt"] = json!(1);
        }
    } else if name == "update_agent" {
        for key in ["providerInstance"] {
            if let Some(value) = args.get(key) {
                plan["body"][key] = value.clone();
            }
        }
        if let Some(agent) = agent {
            let _ = agent;
        }
    }
    Ok(plan)
}

pub async fn call(state: &AppState, turn: &Value, name: &str, args: &Value) -> Result<Value> {
    if crate::updates::pending(state) || !eligible(turn) {
        return Err(error(403, "Guide tools require a direct conversation."));
    }
    let spec = declarations()?
        .into_iter()
        .find(|spec| spec["name"] == name)
        .ok_or_else(|| error(403, "Guide tool is unavailable."))?;
    if !validate::valid(&spec["inputSchema"], args) {
        return Err(error(400, "Invalid guide tool arguments."));
    }
    let turn_id = string(turn, "id");
    {
        let lock = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &lock.connection;
        if !exists(
            db,
            "SELECT 1 FROM turns WHERE id=? AND agent=? AND status='running'",
            [turn_id, ID],
        )? || data::agents::hidden(db, ID)?
        {
            return Err(error(409, "Guide conversation is unavailable."));
        }
        if WRITES.contains(&name) || guide_infra::WRITES.contains(&name) {
            let request = string(args, "requestId");
            let fingerprint = crate::db::digest(&json!([name, args]).to_string());
            let receipt = one(
                db,
                "SELECT status,expires,plan FROM guide_changes WHERE turn=? AND request=?",
                [turn_id, request],
            )?;
            if let Some(receipt) = receipt {
                let saved = data::json(&receipt["plan"])?;
                if saved["requestDigest"] != fingerprint {
                    return Err(error(
                        409,
                        "Request ID was already used for a different change.",
                    ));
                }
                let status = if receipt["status"] == "pending" {
                    if receipt["expires"]
                        .as_i64()
                        .is_some_and(|at| at <= state.config.now())
                    {
                        "expired"
                    } else {
                        "proposed"
                    }
                } else {
                    string(&receipt, "status")
                };
                return Ok(json!({"status":status,"change":preview(&saved)?}));
            }
            if exists(
                db,
                "SELECT 1 FROM guide_changes WHERE turn=? GROUP BY turn HAVING count(*)>=12",
                [turn_id],
            )? {
                return Err(error(
                    409,
                    "Review the current changes before proposing more.",
                ));
            }
            let id = ::uuid::Uuid::new_v4().to_string();
            let mut plan = if guide_infra::WRITES.contains(&name) {
                let mut plan = guide_infra::plan(state, db, name, args)?;
                plan["tool"] = json!(name);
                plan
            } else {
                plan(db, name, args, &id)?
            };
            plan["requestDigest"] = json!(fingerprint);
            let vault_reflected = if guide_infra::WRITES.contains(&name) {
                let bytes = plan.to_string();
                crate::computer::vault::redactions(state, db)?
                    .iter()
                    .any(|secret| super::secrets::reflected(bytes.as_bytes(), secret))
            } else {
                false
            };
            if vault_reflected || plan != features::clean(state, db, &plan)? {
                return Err(error(400, "Remove credentials from the proposed change."));
            }
            let change = preview(&plan)?;
            let digest = crate::db::digest(&plan.to_string());
            db.execute(
                "INSERT INTO guide_changes(id,turn,request,plan,digest,expires) VALUES(?,?,?,?,?,?)",
                params![id,turn_id,request,plan.to_string(),digest,state.config.now()+TTL_MS],
            )?;
            return Ok(json!({"status":"proposed","change":change}));
        }
        match name {
            "open_guide_screen" => {
                let screen = string(args, "screen");
                db.execute(
                    "INSERT OR IGNORE INTO guide_screens VALUES(?,?)",
                    [turn_id, screen],
                )?;
                return Ok(json!({"screen":screen,"link":screens()?[screen]}));
            }
            "list_guide_connections" => {
                let guide = data::agents::read(db, ID)?;
                let accounts = data::provider_instances::list(state)?["providers"].clone();
                return features::clean(
                    state,
                    db,
                    &json!({"providers":accounts,"current":{"providerInstance":guide["providerInstance"],"model":guide["model"]}}),
                );
            }
            "list_guide_agents" => {
                let mut rows = all(db,"SELECT a.id,a.name,a.revision,a.model,a.provider_instance AS providerInstance FROM agents a LEFT JOIN agent_visibility v ON v.agent=a.id WHERE a.id<>? AND a.id>? AND coalesce(v.hidden,0)=0 ORDER BY a.id LIMIT 31",[ID,string(args,"after")])?;
                let next = (rows.len() > 30).then(|| rows[29]["id"].clone());
                rows.truncate(30);
                return features::clean(state, db, &json!({"items":rows,"next_after":next}));
            }
            _ => {}
        }
    }
    let result = if guide_infra::READS.contains(&name) {
        guide_infra::read(state, name, args).await?
    } else {
        features::read(state, &json!({"kind":"peer"}), name, args).await?
    };
    let lock = state.database.lock().map_err(|_| Failure::storage())?;
    features::clean(state, &lock.connection, &result)
}

pub fn actions(db: &Connection, turn: &str, now: i64) -> Result<Value> {
    let changes = all(
        db,
        "SELECT id,plan,expires,status FROM guide_changes WHERE turn=? ORDER BY rowid",
        [turn],
    )?
    .into_iter()
    .map(|row| {
        let status = if row["status"] == "pending"
            && row["expires"]
                .as_i64()
                .is_some_and(|expires| expires <= now)
        {
            json!("expired")
        } else {
            row["status"].clone()
        };
        Ok(json!({"id":row["id"],"change":preview(&data::json(&row["plan"])?)?,"status":status}))
    })
    .collect::<Result<Vec<_>>>()?;
    let screens = all(
        db,
        "SELECT screen FROM guide_screens WHERE turn=? ORDER BY rowid",
        [turn],
    )?
    .into_iter()
    .map(|row| row["screen"].clone())
    .collect::<Vec<_>>();
    Ok(json!({"changes":changes,"screens":screens}))
}

fn claim(db: &Connection, id: &str, now: i64, action: &str, change: &Value) -> Result<Value> {
    let row = one(db, "SELECT * FROM guide_changes WHERE id=?", [id])?
        .ok_or_else(|| error(404, "Proposed change not found."))?;
    let plan = data::json(&row["plan"])?;
    if preview(&plan)? != *change || row["digest"] != crate::db::digest(&plan.to_string()) {
        return Err(error(409, "Proposed change does not match."));
    }
    let status = if action == "apply" {
        "applying"
    } else {
        "cancelled"
    };
    if db.execute(
        "UPDATE guide_changes SET status=? WHERE id=? AND status='pending' AND expires>?",
        params![status, id, now],
    )? != 1
    {
        return Err(error(409, "This change expired or was already reviewed."));
    }
    Ok(plan)
}

pub fn handles(path: &str) -> bool {
    path.starts_with("/api/guide/changes/")
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/guide/changes/{id}", any(crate::dispatch))
}

pub async fn local(state: &AppState, request: Request) -> Result<Response> {
    let id = request
        .uri()
        .path()
        .trim_start_matches("/api/guide/changes/")
        .to_owned();
    if request.method() != Method::POST || !uuid(&id) || request.uri().query().is_some() {
        return Err(error(400, "Invalid change request."));
    }
    let headers = request.headers();
    let remote = request
        .extensions()
        .get::<crate::auth::pairing::BrowserAccess>()
        .cloned();
    if headers.contains_key("authorization")
        || !data::origin(state, headers)
        || crate::text_header(headers, "sec-fetch-site") != Some("same-origin")
        || crate::text_header(headers, "content-type").and_then(|v| v.split(';').next())
            != Some("application/json")
    {
        return Err(error(403, "Review changes in Minarbot."));
    }
    let bytes = axum::body::to_bytes(request.into_body(), 64000)
        .await
        .map_err(|_| error(413, "Change request is too large."))?;
    let body = crate::web::parse_json(&bytes).map_err(|_| error(400, "Invalid change request."))?;
    let action = string(&body, "action");
    if !closed(&body, &["action", "change"])
        || !["apply", "cancel"].contains(&action)
        || !body["change"].is_object()
    {
        return Err(error(400, "Invalid change request."));
    }
    let plan = {
        let lock = state.database.lock().map_err(|_| Failure::storage())?;
        if data::agents::hidden(&lock.connection, ID)? {
            return Err(error(409, "Restore the guide before reviewing changes."));
        }
        if guide_infra::WRITES.contains(&string(&body["change"], "tool"))
            && remote
                .as_ref()
                .map(|access| access.paired(&lock.connection, state.config.now()))
                .transpose()?
                == Some(false)
        {
            return Err(error(
                403,
                "Pair this browser before reviewing infrastructure changes.",
            ));
        }
        claim(
            &lock.connection,
            &id,
            state.config.now(),
            action,
            &body["change"],
        )?
    };
    if action == "cancel" {
        return Ok(crate::json_response(200, json!({"status":"cancelled"})));
    }
    if plan.get("guideOperation").is_some() {
        let state = state.clone();
        return tokio::spawn(async move {
            let result = guide_infra::apply(&state, &id, &plan).await;
            let status = record_apply(&state, &id, &result)?;
            let mut result = result?;
            result["status"] = json!(status);
            Ok(crate::json_response(200, result))
        })
        .await
        .map_err(|_| Failure::storage())?;
    }
    // An interrupted Apply remains consumed. Existing feature commands keep their own receipts.
    let result = features::api(state, string(&plan, "path"), plan.get("body")).await;
    let status = record_apply(state, &id, &result)?;
    Ok(crate::json_response(
        200,
        json!({"status":status,"redirect_url":result?["redirect_url"]}),
    ))
}

fn record_apply(state: &AppState, id: &str, result: &Result<Value>) -> Result<&'static str> {
    let status = match result {
        Ok(_) => "applied",
        Err(error) if error.status < 500 => "failed",
        Err(_) => "unknown",
    };
    let lock = state.database.lock().map_err(|_| Failure::storage())?;
    lock.connection
        .execute("UPDATE guide_changes SET status=? WHERE id=?", [status, id])?;
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex},
    };

    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn review(
        state: &AppState,
        proposal: &Value,
        action: &str,
        change: Value,
    ) -> crate::db::Result<u16> {
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("/api/guide/changes/{}", string(proposal, "id")))
            .header("origin", &state.origin)
            .header("sec-fetch-site", "same-origin")
            .header("content-type", "application/json")
            .body(json!({"action":action,"change":change}).to_string().into())?;
        Ok(match local(state, request).await {
            Ok(response) => response.status().as_u16(),
            Err(error) => error.status,
        })
    }

    #[tokio::test]
    #[ignore = "legacy guide fixture uses removed API runtime setup"]
    async fn settings_change_only_after_exact_unexpired_confirmation() -> crate::db::Result<()> {
        let scratch = Scratch(
            std::env::temp_dir().join(format!("minarbot-guide-test-{}", ::uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&scratch.0)?;
        let mut config: crate::config::Config = serde_json::from_value(json!({
            "database":scratch.0.join("state.sqlite"),"fixedClock":1000,"defaultModel":"fixture",
        }))?;
        config.data.validate()?;
        let database = crate::db::open(&config.database, false, config.now())?;
        let db = &database.connection;
        crate::files::init(db)?;
        data::initialize(&database)?;
        crate::turns::initialize(db).map_err(|error| error.message)?;
        crate::connectors::initialize(db).map_err(|error| error.message)?;
        super::super::initialize(db).map_err(|error| error.message)?;
        initialize(db).map_err(|error| error.message)?;
        let turn_id = ::uuid::Uuid::new_v4().to_string();
        db.execute("INSERT INTO turns(id,agent,text,status,created) VALUES(?,?,'Set up','running','fixture')", [turn_id.as_str(),ID])?;
        let state = AppState {
            whatsapp: Default::default(),
            computer: Arc::new(crate::computer::Manager::new(&config)?),
            database: Arc::new(Mutex::new(database)),
            config: Arc::new(config),
            origin: "http://127.0.0.1".into(),
            auth: None,
            events: crate::events::Hub::new(),
            turns: crate::turns::Runtime::default(),
            trial_pending: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let turn = json!({"agent":ID,"id":turn_id});
        for name in ["Researcher", "Expired", "Cancelled"] {
            let args = json!({"requestId": ::uuid::Uuid::new_v4().to_string(), "name":name,
                "instructions":"Cite sources.", "model":"fixture"});
            let result = call(&state, &turn, "create_agent", &args)
                .await
                .map_err(|e| e.message)?;
            assert_eq!(result["status"], "proposed");
            assert!(result["id"].is_null());
        }
        let proposals = {
            let guard = state
                .database
                .lock()
                .map_err(|_| "Database lock poisoned")?;
            let db = &guard.connection;
            db.execute("UPDATE guide_changes SET expires=1000 WHERE json_extract(plan,'$.body.name')='Expired'",[])?;
            actions(db, &turn_id, state.config.now()).map_err(|e| e.message)?
        };
        let pending = &proposals["changes"][0];
        let change = pending["change"].clone();
        let mut edited = change.clone();
        edited["settings"]["instructions"] = json!("Different instructions");
        assert_eq!(review(&state, pending, "apply", Value::Null).await?, 400);
        assert_eq!(review(&state, pending, "apply", edited).await?, 409);
        let expired = &proposals["changes"][1];
        assert_eq!(
            review(&state, expired, "apply", expired["change"].clone()).await?,
            409
        );
        let cancelled = &proposals["changes"][2];
        assert_eq!(
            review(&state, cancelled, "cancel", cancelled["change"].clone()).await?,
            200
        );
        assert_eq!(
            review(&state, cancelled, "apply", cancelled["change"].clone()).await?,
            409
        );
        assert_eq!(
            features::api(&state, "/api/agents", None)
                .await
                .map_err(|e| e.message)?["agents"],
            json!([])
        );

        assert_eq!(review(&state, pending, "apply", change.clone()).await?, 200);
        assert_eq!(review(&state, pending, "apply", change).await?, 409);
        let directory = features::api(&state, "/api/agents", None)
            .await
            .map_err(|e| e.message)?;
        assert_eq!(directory["agents"].as_array().map(Vec::len), Some(1));
        assert_eq!(directory["agents"][0]["name"], "Researcher");
        assert_eq!(directory["agents"][0]["instructions"], "Cite sources.");
        Ok(())
    }
}
