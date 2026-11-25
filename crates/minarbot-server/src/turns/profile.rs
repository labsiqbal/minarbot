use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};

pub fn setup(db: &Connection, turn: &Value) -> Result<bool> {
    Ok(turn["profile_setup"] == 1
        || turn["id"] == turn["agent"]
            && exists(
                db,
                "SELECT 1 FROM agent_creations WHERE id=? AND json_extract(request,'$.setup')=1",
                [string(turn, "agent")],
            )?)
}

pub fn execute(
    state: &AppState,
    db: &Connection,
    turn: &Value,
    name: &str,
    args: &Value,
    avatar: &mut data::avatar::Prepared,
) -> Result<Value> {
    if !turn["channel"].is_null() {
        return Err(error(
            403,
            "Profile and site tools require a private conversation.",
        ));
    }
    let agent = string(turn, "agent");
    match name {
        "edit_profile" => {
            if !closed(args, &["revision", "name", "role", "instructions"])
                || !integer(&args["revision"], 1, 9_007_199_254_740_991)
                || !["name", "role", "instructions"]
                    .iter()
                    .any(|k| args.get(k).is_some())
            {
                return Err(error(400, "Invalid profile change."));
            }
            let name = args
                .get("name")
                .map(|_| field(args, "name", 80))
                .transpose()?;
            let instructions = args
                .get("instructions")
                .map(|_| field(args, "instructions", 8000))
                .transpose()?;
            let role = args
                .get("role")
                .map(|v| {
                    v.as_str()
                        .filter(|v| {
                            v.encode_utf16().count() <= 60
                                && !v
                                    .chars()
                                    .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
                        })
                        .map(|v| v.trim().to_owned())
                        .ok_or_else(|| error(400, "Invalid agent role."))
                })
                .transpose()?;
            if setup(db, turn)? && (name.is_none() || instructions.is_none()) {
                return Err(error(
                    400,
                    "Profile setup must save the name and instructions together.",
                ));
            }
            if data::agents::read(db, agent)?["revision"] != args["revision"] {
                return Err(error(
                    409,
                    "Profile changed. Ask the user for guidance before changing it again.",
                ));
            }
            db.execute("UPDATE agents SET name=coalesce(?,name),role=coalesce(?,role),instructions=coalesce(?,instructions),revision=revision+1 WHERE id=?",params![name,role,instructions,agent])?;
            let updated = data::agents::read(db, agent)?;
            Ok(
                json!({"name":updated["name"],"role":updated["role"],"instructions":updated["instructions"],"revision":updated["revision"]}),
            )
        }
        "set_avatar" => {
            if !closed(args, &["image", "revision"])
                || !integer(&args["revision"], 0, 9_007_199_254_740_991)
                || args.get("image").is_none()
            {
                return Err(error(400, "Invalid avatar request."));
            }
            let image = if args["image"].is_null() {
                Value::Null
            } else {
                let file =
                    history::readable_file(db, turn, string(args, "image"))?.ok_or_else(|| {
                        error(
                            404,
                            "Image has not been sent or is unavailable in this conversation.",
                        )
                    })?;
                if file.bytes.len() > 4 * 1024 * 1024 {
                    return Err(error(413, "Avatar limit: 4 MiB."));
                }
                json!(STANDARD.encode(avatar.get(Some(file.bytes))?.ok_or_else(Failure::storage)?))
            };
            data::agents::avatar_save(
                db,
                agent,
                &json!({"id":uuid::Uuid::new_v4().to_string(),"revision":args["revision"],"image":image}),
                avatar,
            )
        }
        "list_sites" => {
            if !closed(args, &["after"])
                || args
                    .get("after")
                    .is_some_and(|v| !v.as_str().is_some_and(uuid))
            {
                return Err(error(400, "Invalid site list position."));
            }
            let mut rows = all(db,"SELECT id,name,revision,updated FROM local_sites WHERE agent=? AND deleted=0 AND id>? ORDER BY id LIMIT 11",[agent,string(args,"after")])?;
            let next = if rows.len() > 10 {
                rows[9]["id"].clone()
            } else {
                Value::Null
            };
            rows.truncate(10);
            for row in &mut rows {
                row["path"] = json!(format!("/sites/{}", string(row, "id")));
            }
            Ok(json!({"sites":rows,"next_after":next}))
        }
        _ => {
            if !closed(
                args,
                if name == "publish_site" {
                    &["file", "name", "site", "revision"]
                } else {
                    &["site", "revision"]
                },
            ) {
                return Err(error(400, "Invalid site request."));
            }
            let mut input = json!({"id":uuid::Uuid::new_v4().to_string()});
            for key in ["site", "revision", "name"] {
                if let Some(value) = args.get(key) {
                    input[key] = value.clone();
                }
            }
            if name == "publish_site" {
                let file =
                    history::readable_file(db, turn, string(args, "file"))?.ok_or_else(|| {
                        error(
                            404,
                            "HTML file has not been sent or is unavailable in this conversation.",
                        )
                    })?;
                if !(string(&file.meta, "name").to_lowercase().ends_with(".html")
                    || string(&file.meta, "name").to_lowercase().ends_with(".htm"))
                    || file.bytes.len() > 262144
                {
                    return Err(error(400, "Site requires a UTF-8 HTML file up to 256 KiB."));
                }
                input["source"] = json!(std::str::from_utf8(&file.bytes)
                    .map_err(|_| error(400, "HTML file must use UTF-8."))?);
            } else {
                input["delete"] = json!(true);
            }
            let row = crate::files::sites::save(db, agent, &input, state.config.now())?;
            Ok(
                json!({"id":row["id"],"name":row["name"],"revision":row["revision"],"deleted":row["deleted"],"path":if row["deleted"] == 1 {Value::Null}else{json!(format!("/sites/{}",string(&row,"id")))}}),
            )
        }
    }
}
