use super::{approval, error, session, Access, Result};
use crate::{data, web::Failure, AppState};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

pub const EXPORT: &str = "export_craft_artifact";
const FAL_ENTRY: &str = "craft-anything-fal";

pub fn asset_tool(name: &str) -> bool {
    matches!(name, "assets_image_generate" | "assets_model_generate")
}

pub fn specs() -> [Value; 3] {
    [
        json!({"name":EXPORT,"title":"Attach a Craft Anything artifact","inputSchema":{"type":"object","properties":{"path":{"type":"string","maxLength":240}},"required":["path"],"additionalProperties":false}}),
        json!({"name":"assets_image_generate","title":"Generate a picture with fal.ai","inputSchema":{"type":"object","properties":{"prompt":{"type":"string","minLength":1,"maxLength":2000},"save_as":{"type":"string","maxLength":240},"shape":{"type":"string","enum":["square","wide","tall"]}},"required":["prompt","save_as"],"additionalProperties":false}}),
        json!({"name":"assets_model_generate","title":"Generate an untextured 3D model with fal.ai","inputSchema":{"type":"object","properties":{"prompt":{"type":"string","minLength":1,"maxLength":2000},"save_as":{"type":"string","maxLength":240},"faces":{"type":"integer","minimum":48,"maximum":20000}},"required":["prompt","save_as"],"additionalProperties":false}}),
    ]
}

fn save(
    db: &Connection,
    state: &AppState,
    turn: &Value,
    folder: &Path,
    path: &str,
    secrets: &[String],
) -> Result<Value> {
    let relative = path.strip_prefix("/work/").unwrap_or(path);
    let (name, bytes) = crate::files::workspace::download(folder, relative).map_err(|_| {
        error(
            400,
            "Artifact must be a regular file in /work, at most 10 MiB.",
        )
    })?;
    let mime = match name.rsplit_once('.').map(|(_, ext)| ext) {
        Some("png") if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => "image/png",
        Some("glb") if bytes.starts_with(b"glTF") => "model/gltf-binary",
        Some("json" | "md" | "txt") if std::str::from_utf8(&bytes).is_ok() => "text/plain",
        _ => {
            return Err(error(
                400,
                "Attach PNG, GLB, JSON, Markdown, or text artifacts.",
            ))
        }
    };
    if secrets.iter().any(|secret| {
        crate::mcp::secrets::reflected(&bytes, secret)
            || crate::mcp::secrets::reflected(name.as_bytes(), secret)
    }) {
        return Err(error(403, "Artifact contains a protected value."));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let turn_id = data::string(turn, "id");
    super::super::transaction(db, |db| {
        let count: i64 = db.query_row(
            "SELECT (SELECT count(*) FROM mcp_files WHERE turn=?)+(SELECT count(*) FROM generated_files WHERE turn=?)",
            [turn_id; 2], |row| row.get(0))?;
        if count >= 8 {
            return Err(error(409, "This turn already has eight result files."));
        }
        db.execute(
            "INSERT INTO files(id,scope,name,digest,bytes,created) VALUES(?,?,?,?,?,?)",
            params![
                id,
                crate::turns::execution::scope(turn),
                name,
                format!("{:x}", Sha256::digest(&bytes)),
                bytes,
                data::iso(db, state.config.now())?
            ],
        )?;
        db.execute("INSERT INTO mcp_files VALUES(?,?)", [turn_id, &id])?;
        Ok(json!({"id":id,"name":name,"mimeType":mime,"size":bytes.len()}))
    })
}

pub fn export(state: &AppState, turn: &Value, mcp: &session::Mcp, path: &str) -> Result<Value> {
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    save(
        db,
        state,
        turn,
        &mcp.work.0,
        path,
        &super::super::vault::redactions(state, db)?,
    )
}

pub async fn generate(
    state: &AppState,
    turn: &Value,
    authority: &Access,
    name: &str,
    input: &Value,
) -> Result<Value> {
    let body = specs()
        .into_iter()
        .find(|tool| tool["name"] == name)
        .ok_or_else(Failure::storage)?;
    if !crate::mcp::validate::valid(&body["inputSchema"], input) {
        return Err(error(400, "Invalid asset request."));
    }
    let path = data::string(input, "save_as");
    let path = path.strip_prefix("app/").unwrap_or(path);
    let suffix = if name == "assets_image_generate" {
        ".png"
    } else {
        ".glb"
    };
    if !path.ends_with(suffix)
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || path
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | ':'))
    {
        return Err(error(400, "Choose a relative asset path inside app/."));
    }
    let agent = &authority.agent;
    {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        if !data::exists(
            &guard.connection,
            "SELECT 1 FROM vault_grants WHERE entry=? AND agent=?",
            [FAL_ENTRY, agent],
        )? {
            return Err(error(
                403,
                "Grant this agent the craft-anything-fal secret in Vault.",
            ));
        }
    }
    if !approval(state, turn, name, input).await? {
        return Err(error(403, "Craft Anything asset generation denied."));
    }
    let (slot, mut secret) = {
        let _changes = state.computer.craft.changes.lock().await;
        super::access(state, turn, authority.deep)?;
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let (origin, secret) = super::super::vault::resolve(
            state,
            &guard.connection,
            agent,
            FAL_ENTRY,
            "value",
            state.config.now(),
        )?;
        if origin != "https://fal.ai" {
            return Err(error(
                403,
                "Use https://fal.ai as the Craft Anything Vault secret URL.",
            ));
        }
        (
            state
                .computer
                .craft
                .session(data::string(turn, "id"), authority)?,
            secret,
        )
    };
    let response = session::until_stopped(&slot, async {
        let mut mcp = session::Mcp::open(state, authority, Some(&secret)).await?;
        let mut input = input.clone();
        if name == "assets_model_generate" {
            input["textured"] = json!(false);
        }
        let result = mcp.call(name, &input).await?;
        if result["isError"] == true {
            return Err(error(
                502,
                "Craft Anything asset generation failed. Check fal.ai before retrying.",
            ));
        }
        super::access(state, turn, authority.deep)?;
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let mut secrets = super::super::vault::redactions(state, &guard.connection)?;
        secrets.push(std::mem::take(&mut secret));
        save(
            &guard.connection,
            state,
            turn,
            &mcp.work.0,
            &format!("app/{path}"),
            &secrets,
        )
    })
    .await?;
    Ok(json!({"file":response}))
}
