use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sha2::{Digest, Sha256};
pub(super) fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn secret() -> Result<String> {
    let mut bytes = [0; 32];
    getrandom::getrandom(&mut bytes).map_err(|_| Failure::storage())?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
const SCOPES: [&str; 21] = [
    "agents:read",
    "agents:profile",
    "history:read",
    "conversations:read",
    "projects:read",
    "routines:read",
    "workflows:read",
    "machines:read",
    "settings:read",
    "computer:read",
    "usage:read",
    "skills:read",
    "agents:write",
    "conversations:write",
    "projects:write",
    "routines:write",
    "agents:message",
    "workflows:run",
    "computer:run",
    "orchestration",
    "machine:update",
];
fn scopes(value: &Value) -> Result<Vec<String>> {
    let raw = value
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= SCOPES.len())
        .ok_or_else(|| error(400, "Choose at least one valid scope."))?;
    let mut items = Vec::new();
    for value in raw {
        let s = value
            .as_str()
            .filter(|s| SCOPES.contains(s))
            .ok_or_else(|| error(400, "Choose at least one valid scope."))?;
        if items.contains(&s.to_owned()) {
            return Err(error(400, "Choose at least one valid scope."));
        }
        items.push(s.to_owned());
    }
    items.sort();
    Ok(items)
}
fn metadata(row: &Value) -> Result<Value> {
    Ok(
        json!({"id":row["id"],"name":row["name"],"scopes":data::json(&row["scopes"])?,"created":row["created"],"lastUsed":row["last_used"],"revoked":row["revoked"]}),
    )
}
pub fn peers(state: &AppState, db: &Connection, v: &Value, get: bool) -> Result<Value> {
    if get {
        return Ok(
            json!({"items":all(db,"SELECT * FROM mcp_server_tokens WHERE kind='peer' ORDER BY created DESC,id",[])?.iter().map(metadata).collect::<Result<Vec<_>>>()?}),
        );
    }
    if !closed(v, &["id", "name", "scopes"]) || !uuid(string(v, "id")) {
        return Err(error(400, "Invalid peer token request."));
    }
    let name = field(v, "name", 80)?;
    if name.chars().any(char::is_control) {
        return Err(error(400, "Invalid peer token request."));
    }
    let scopes = scopes(&v["scopes"])?;
    let digest = hash(&json!({"name":name,"scopes":scopes}).to_string());
    if let Some(row) = one(
        db,
        "SELECT * FROM mcp_server_tokens WHERE id=?",
        [string(v, "id")],
    )? {
        if row["kind"] != "peer" || row["request_digest"] != digest {
            return Err(error(409, "ID was already used for a different change."));
        }
        let mut out = metadata(&row)?;
        out["alreadyCreated"] = json!(true);
        return Ok(out);
    }
    let token = secret()?;
    db.execute("INSERT INTO mcp_server_tokens(id,token_hash,kind,name,scopes,created,request_digest) VALUES(?,?,'peer',?,?,?,?)",params![string(v,"id"),hash(&token),name,json!(scopes).to_string(),state.config.now(),digest])?;
    let mut out = metadata(
        &one(
            db,
            "SELECT * FROM mcp_server_tokens WHERE id=?",
            [string(v, "id")],
        )?
        .ok_or_else(Failure::storage)?,
    )?;
    out["token"] = json!(token);
    out["alreadyCreated"] = json!(false);
    Ok(out)
}
pub fn revoke_peer(state: &AppState, db: &Connection, id: &str, v: &Value) -> Result<Value> {
    if !uuid(id) || !closed(v, &["id"]) {
        return Err(error(400, "Invalid revocation request."));
    }
    let digest = hash(&json!({"revoke":id}).to_string());
    if let Some(row) = one(
        db,
        "SELECT * FROM mcp_server_commands WHERE id=?",
        [string(v, "id")],
    )? {
        if row["digest"] != digest {
            return Err(error(409, "ID was already used for a different change."));
        }
        return data::json(&row["result"]);
    }
    let row = one(
        db,
        "SELECT * FROM mcp_server_tokens WHERE id=? AND kind='peer'",
        [id],
    )?
    .ok_or_else(|| error(404, "Peer token not found."))?;
    let value = json!({"id":id,"revoked":row["revoked"].as_i64().unwrap_or(state.config.now())});
    transaction(db, || {
        db.execute(
            "UPDATE mcp_server_tokens SET revoked=? WHERE id=?",
            params![value["revoked"].as_i64(), id],
        )?;
        db.execute(
            "INSERT INTO mcp_server_commands VALUES(?,?,?)",
            params![string(v, "id"), digest, value.to_string()],
        )?;
        Ok(value)
    })
}
pub fn mint(state: &AppState, agent: &str, turn: &str) -> Result<(String, String)> {
    let id = uuid::Uuid::new_v4().to_string();
    let token = secret()?;
    let db = state.database.lock().map_err(|_| Failure::storage())?;
    // The live turn owns this lease and revokes it on exit. Startup revokes orphaned sessions.
    db.connection.execute("INSERT INTO mcp_server_tokens(id,token_hash,kind,name,agent,turn,scopes,created,request_digest) VALUES(?,?,'session','',?,?,?,?,'')",params![id,hash(&token),agent,turn,json!(["agents:read","agents:profile","agents:message","history:read","conversations:read","conversations:write","projects:read","projects:write","routines:read","routines:write","workflows:read","workflows:run","machines:read","settings:read","usage:read","skills:read","computer:read","computer:run","orchestration"]).to_string(),state.config.now()])?;
    Ok((id, token))
}
pub fn register_external(
    state: &AppState,
    id: &str,
    token: &str,
    agent: &str,
    turn: &str,
) -> Result<()> {
    let db = state.database.lock().map_err(|_| Failure::storage())?;
    db.connection.execute(
        "INSERT INTO mcp_server_tokens(id,token_hash,kind,name,agent,turn,scopes,created,request_digest) VALUES(?,?,'session','',?,?,?,?,'') ON CONFLICT(token_hash) DO UPDATE SET id=excluded.id,agent=excluded.agent,turn=excluded.turn,revoked=NULL,created=excluded.created",
        params![id, hash(token), agent, turn, json!(["agents:read","agents:profile","agents:message","history:read","conversations:read","conversations:write","projects:read","projects:write","routines:read","routines:write","workflows:read","workflows:run","machines:read","settings:read","usage:read","skills:read","computer:read","computer:run","orchestration"]).to_string(), state.config.now()],
    )?;
    Ok(())
}
pub fn verify(state: &AppState, token: &str) -> Result<Value> {
    if token.is_empty()
        || token.len() > 4096
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err(error(401, "MCP authorization failed."));
    }
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    let row=one(db,"SELECT * FROM mcp_server_tokens WHERE token_hash=? AND revoked IS NULL AND (expires IS NULL OR expires>?)",params![hash(token),state.config.now()])?.ok_or_else(||error(401,"MCP authorization failed."))?;
    if row["kind"] == "session"
        && row["turn"].is_string()
        && !data::exists(
            db,
            // Native harnesses pause their turn while a blocking MCP call is
            // outstanding. Keep its session lease valid until that call has
            // produced a durable result, then revoke it when the turn settles.
            "SELECT 1 FROM turns WHERE id=? AND agent=? AND (status='running' OR (status='queued' AND (paused=1 OR waiting_for IS NOT NULL OR EXISTS(SELECT 1 FROM provider_calls WHERE turn=turns.id AND status='pending'))))",
            params![row["turn"].as_str(), row["agent"].as_str()],
        )?
    {
        db.execute(
            "UPDATE mcp_server_tokens SET revoked=? WHERE id=?",
            params![state.config.now(), row["id"].as_str()],
        )?;
        return Err(error(401, "MCP authorization failed."));
    }
    db.execute(
        "UPDATE mcp_server_tokens SET last_used=? WHERE id=?",
        params![state.config.now(), row["id"].as_str()],
    )?;
    Ok(row)
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
