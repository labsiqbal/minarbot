mod gateway;
mod jobs;
mod rest;

#[cfg(test)]
mod tests;

use super::{runner, *};
use jobs::Callback;
use rest::Api;
use std::{
    sync::{Arc, MutexGuard},
    time::Duration,
};

const MAX_FILE: usize = 10 * 1024 * 1024;
const SECRET: &str = ".discord-key";
const CONTEXT: &[u8] = b"discord-bot-token";
const SETTINGS: &str = "/api/connected-apps/discord";

struct Bot {
    generation: String,
    id: String,
    token: String,
}

pub(super) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(include_str!("schema.sql"))?;
    Ok(())
}

pub(crate) fn snowflake(id: &str) -> bool {
    (1..=20).contains(&id.len())
        && !id.starts_with('0')
        && id.bytes().all(|b| b.is_ascii_digit())
        && id.parse::<u64>().is_ok()
}

fn lock(state: &AppState) -> Result<MutexGuard<'_, crate::db::Database>> {
    state.database.lock().map_err(|_| Failure::storage())
}

fn target(db: &Connection, route: &Value) -> Result<()> {
    let target = string(route, "target");
    if !uuid(target) {
        return Err(error(400, "Choose a valid Discord destination."));
    }
    let active = match string(route, "kind") {
        "agent" => exists(db, "SELECT 1 FROM agents a LEFT JOIN agent_visibility v ON v.agent=a.id WHERE a.id=? AND coalesce(v.hidden,0)=0", [target])?,
        "section" => exists(db, "SELECT 1 FROM local_project_sections s JOIN local_projects p ON p.id=s.project JOIN channels c ON c.id=s.channel WHERE s.channel=? AND s.archived=0 AND p.archived=0 AND p.deleted=0 AND c.archived=0", [target])?,
        _ => false,
    };
    if !active {
        return Err(error(409, "Discord destination is unavailable."));
    }
    Ok(())
}

fn revoke(db: &Connection, route: Option<&str>) -> Result<()> {
    db.execute("UPDATE discord_outbox SET status='revoked',error='Discord route was removed.' WHERE status='queued' AND (?1 IS NULL OR route=?1)", [route])?;
    db.execute("UPDATE discord_jobs SET status='revoked' WHERE status='running' AND message IN(SELECT message FROM discord_events WHERE ?1 IS NULL OR route=?1)", [route])?;
    db.execute("UPDATE discord_events SET status='revoked',text='',attachments='[]' WHERE status='queued' AND (?1 IS NULL OR route=?1)", [route])?;
    db.execute(
        "DELETE FROM discord_routes WHERE ?1 IS NULL OR id=?1",
        [route],
    )?;
    Ok(())
}

fn view(db: &Connection) -> Result<Value> {
    let bot = one(
        db,
        "SELECT generation,bot,status FROM discord_bot WHERE singleton=1",
        [],
    )?
    .ok_or_else(Failure::storage)?;
    let mut routes = all(db, "SELECT r.id,r.channel,r.kind,r.target,r.users,coalesce(a.name,p.name||' / '||s.name) AS label FROM discord_routes r LEFT JOIN agents a ON r.kind='agent' AND a.id=r.target LEFT JOIN local_project_sections s ON r.kind='section' AND s.channel=r.target LEFT JOIN local_projects p ON p.id=s.project ORDER BY r.channel", [])?;
    for route in &mut routes {
        route["users"] = crate::data::json(&route["users"])?;
    }
    let deliveries = all(db, "SELECT o.id,r.channel,o.status,o.part,o.total,o.error FROM discord_outbox o LEFT JOIN discord_routes r ON r.id=o.route WHERE o.status IN('failed','uncertain','revoked') ORDER BY o.rowid DESC LIMIT 10", [])?;
    Ok(
        json!({"generation":bot["generation"],"bot":bot["bot"],"status":bot["status"],"routes":routes,"deliveries":deliveries}),
    )
}

pub(super) async fn settings(
    state: &AppState,
    path: &str,
    method: &Method,
    input: &Value,
) -> Result<Value> {
    if path == SETTINGS && method == Method::GET {
        return view(&lock(state)?.connection);
    }
    if method != Method::POST {
        return Err(error(405, "Method not allowed."));
    }
    let generation = string(input, "generation");
    let id = string(input, "id");
    if path == format!("{SETTINGS}/connect") {
        if !closed(input, &["id", "generation", "token"]) {
            return Err(error(400, "Invalid Discord setup request."));
        }
        let token = string(input, "token").trim();
        if !(20..=256).contains(&token.len())
            || !token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        {
            return Err(error(400, "Enter a Discord bot token."));
        }
        let profile = Api::new(state, token.to_owned())?
            .request("identity", "", "/users/@me", reqwest::Method::GET, Ok)
            .await?;
        let bot = string(&profile, "id");
        if !snowflake(bot) || profile["bot"] != true {
            return Err(error(400, "Token does not belong to a Discord bot."));
        }
        let guard = lock(state)?;
        let db = &guard.connection;
        let old = one(
            db,
            "SELECT generation,bot FROM discord_bot WHERE singleton=1",
            [],
        )?
        .ok_or_else(Failure::storage)?;
        if old["generation"] == id && old["bot"] == bot {
            return view(db);
        }
        if old["generation"] != generation
            || old["bot"].as_str().is_some_and(|previous| previous != bot)
        {
            return Err(error(
                409,
                "Discord connection changed. Reload Settings or disconnect the current bot.",
            ));
        }
        let encrypted = super::secrets::encrypt(state, SECRET, CONTEXT, token)?;
        db.execute("UPDATE discord_bot SET generation=?,bot=?,ciphertext=?,status='connecting' WHERE singleton=1", [id, bot, &encrypted])?;
        return view(db);
    }
    let guard = lock(state)?;
    let db = &guard.connection;
    transaction(db, || {
        let bot = one(
            db,
            "SELECT generation,bot FROM discord_bot WHERE singleton=1",
            [],
        )?
        .ok_or_else(Failure::storage)?;
        let action = path.rsplit('/').next().unwrap_or("");
        if action == "disconnect" && bot["generation"] == id && bot["bot"].is_null() {
            return view(db);
        }
        if bot["generation"] != generation || bot["bot"].is_null() {
            return Err(error(409, "Discord connection changed. Reload Settings."));
        }
        match action {
            "disconnect" if closed(input, &["id", "generation"]) => {
                revoke(db, None)?;
                db.execute("UPDATE discord_bot SET generation=?,bot=NULL,ciphertext=NULL,status='disconnected' WHERE singleton=1", [id])?;
            }
            "add-route"
                if closed(
                    input,
                    &["id", "generation", "channel", "users", "kind", "target"],
                ) =>
            {
                if !snowflake(string(input, "channel")) {
                    return Err(error(400, "Enter a Discord channel, thread, or DM ID."));
                }
                let mut seen = std::collections::HashSet::new();
                let users = input["users"]
                    .as_array()
                    .filter(|users| {
                        (1..=100).contains(&users.len())
                            && users
                                .iter()
                                .all(|u| u.as_str().is_some_and(|u| snowflake(u) && seen.insert(u)))
                    })
                    .ok_or_else(|| error(400, "Enter one to 100 different Discord user IDs."))?;
                target(db, input)?;
                let prior = one(db, "SELECT id,channel,kind,target,users FROM discord_routes WHERE channel=? OR id=?", [string(input,"channel"), id])?;
                if let Some(prior) = prior {
                    if ["id", "channel", "kind", "target"]
                        .iter()
                        .all(|k| prior[*k] == input[*k])
                        && crate::data::json(&prior["users"])? == input["users"]
                    {
                        return view(db);
                    }
                    return Err(error(409, "This Discord conversation already has a route."));
                }
                if db.query_row("SELECT count(*) FROM discord_routes", [], |r| {
                    r.get::<_, i64>(0)
                })? >= 100
                {
                    return Err(error(409, "Discord supports up to 100 routes."));
                }
                db.execute(
                    "INSERT INTO discord_routes VALUES(?,?,?,?,?,?)",
                    params![
                        id,
                        string(input, "channel"),
                        string(input, "kind"),
                        string(input, "target"),
                        json!(users).to_string(),
                        runner::new_id()
                    ],
                )?;
            }
            "remove-route"
                if closed(input, &["id", "generation", "route"])
                    && input["route"].as_str().is_some_and(uuid) =>
            {
                revoke(db, input["route"].as_str())?;
            }
            _ => return Err(error(400, "Invalid Discord settings request.")),
        }
        view(db)
    })
}

fn connection_status(state: &AppState, generation: &str, status: &str) -> Result<()> {
    lock(state)?.connection.execute(
        "UPDATE discord_bot SET status=? WHERE generation=? AND ciphertext IS NOT NULL",
        [status, generation],
    )?;
    Ok(())
}

fn current(db: &Connection, bot: &Bot) -> Result<bool> {
    exists(
        db,
        "SELECT 1 FROM discord_bot WHERE generation=? AND bot=?",
        [&bot.generation, &bot.id],
    )
}

pub(crate) fn start(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if state.auth.is_some() {
            return;
        }
        loop {
            if !crate::turns::pending(&state) {
                let row = lock(&state).and_then(|guard| one(&guard.connection, "SELECT generation,bot,ciphertext FROM discord_bot WHERE ciphertext IS NOT NULL", []));
                if let Ok(Some(row)) = row {
                    let generation = string(&row, "generation").to_owned();
                    match super::secrets::decrypt(
                        &state,
                        SECRET,
                        CONTEXT,
                        string(&row, "ciphertext"),
                    ) {
                        Ok(token) => {
                            let bot = Bot {
                                generation,
                                id: string(&row, "bot").to_owned(),
                                token,
                            };
                            if let Ok(api) = Api::new(&state, bot.token.clone()) {
                                let api = Arc::new(api);
                                tokio::select! {
                                    _ = async { tokio::join!(gateway::listen(&state,&bot,&api), jobs::work(&state,&bot,&api)); } => (),
                                    _ = async {
                                        loop {
                                            tokio::time::sleep(Duration::from_millis(250)).await;
                                            if crate::turns::pending(&state) || !lock(&state).and_then(|g| current(&g.connection,&bot)).unwrap_or(false) { break; }
                                        }
                                    } => (),
                                }
                                // A cancelled HTTP future may already have reached Discord.
                                if let Ok(guard) = lock(&state) {
                                    let _ = guard.connection.execute("UPDATE discord_outbox SET status='uncertain',error='Send interrupted. Last part may have reached Discord.' WHERE status='sending'", []);
                                }
                            }
                        }
                        Err(_) => {
                            let _ = connection_status(&state, &generation, "token_unavailable");
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
}

pub(crate) use jobs::{workflow_delivery, workflow_output, workflow_route};
