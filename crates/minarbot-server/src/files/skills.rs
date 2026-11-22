use super::*;

fn revision(v: &Value, min: i64) -> bool {
    v.as_i64()
        .is_some_and(|v| (min..=9007199254740991).contains(&v))
}
fn field(v: &Value, key: &str, max: usize) -> Result<String> {
    let value = v
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty() && s.encode_utf16().count() <= max)
        .ok_or_else(|| error(400, "Invalid skill name, description, or content."))?;
    Ok(value.trim().to_owned())
}
#[path = "../../../shared/skill_files.rs"]
mod skill_files;

pub fn parse_files(value: &Value) -> Result<Value> {
    skill_files::parse_files(value).map_err(|message| error(400, message))
}
pub fn context(db: &Connection, agent: &str) -> Result<String> {
    let rows=rows(db,"SELECT v.* FROM agent_skills a JOIN skill_versions v ON a.skill=v.skill AND a.version=v.revision WHERE a.agent=? AND a.enabled=1 AND a.removed=0 ORDER BY a.skill",[agent])?;
    Ok(rows
        .iter()
        .map(|r| {
            format!(
                "\n\nInstalled skill {} (revision {}):\n{}\n{}",
                r["name"],
                r["revision"],
                text(r, "description"),
                text(r, "content")
            )
        })
        .collect())
}
pub fn save_skill(db: &Connection, id: &str, v: &Value) -> Result<Value> {
    let imported = v.get("files").map(parse_files).transpose()?;
    if !keys(
        v,
        if imported.is_some() {
            &["id", "revision", "files"]
        } else {
            &["id", "revision", "name", "description", "content"]
        },
    ) || !revision(&v["revision"], 0)
    {
        return Err(error(400, "Invalid skill revision."));
    }
    let source = imported.as_ref().unwrap_or(v);
    let name = field(source, "name", 80)?;
    let description = field(source, "description", 1024)?;
    let content = field(source, "content", 16000)?;
    let current = one(db, "SELECT revision FROM skills WHERE id=?", [id])?;
    if v["revision"]
        != current
            .as_ref()
            .map(|c| c["revision"].clone())
            .unwrap_or(json!(0))
    {
        return Err(error(409, "Skill changed. Reload before saving."));
    }
    if imported.is_none()
        && one(
            db,
            "SELECT skill FROM skill_files WHERE skill=? AND revision=?",
            rusqlite::params![id, v["revision"].as_i64()],
        )?
        .is_some()
    {
        return Err(error(
            409,
            "This skill came from a folder. Import the complete folder to create a new revision.",
        ));
    }
    let revision = v["revision"].as_i64().ok_or_else(Failure::storage)? + 1;
    db.execute("INSERT INTO skills VALUES(?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,revision=excluded.revision",rusqlite::params![id,name,revision])?;
    db.execute(
        "INSERT INTO skill_versions VALUES(?,?,?,?,?)",
        rusqlite::params![id, revision, name, description, content],
    )?;
    if let Some(imported) = imported {
        db.execute(
            "INSERT INTO skill_files VALUES(?,?,?)",
            rusqlite::params![id, revision, imported["files"].to_string()],
        )?;
    }
    Ok(json!({"id":id,"name":name,"description":description,"content":content,"revision":revision}))
}
fn install_input(v: &Value, pack: bool) -> Result<()> {
    if !keys(v, &["id", "revision", "version", "enabled", "remove"])
        || !revision(&v["revision"], 0)
        || !revision(&v["version"], 1)
        || !v["enabled"].is_boolean()
        || v.get("remove")
            .is_some_and(|r| r != true || v["enabled"] == true)
    {
        return Err(error(
            400,
            if pack {
                "Invalid skill pack installation."
            } else {
                "Invalid skill installation."
            },
        ));
    }
    Ok(())
}
fn agent_exists(db: &Connection, agent: &str) -> Result<()> {
    if one(db, "SELECT id FROM agents WHERE id=?", [agent])?.is_none() {
        return Err(error(404, "Agent not found."));
    }
    Ok(())
}
pub fn install_skill(db: &Connection, agent: &str, skill: &str, v: &Value) -> Result<Value> {
    agent_exists(db, agent)?;
    install_input(v, false)?;
    if one(
        db,
        "SELECT skill FROM skill_versions WHERE skill=? AND revision=?",
        rusqlite::params![skill, v["version"].as_i64()],
    )?
    .is_none()
    {
        return Err(error(404, "Skill version not found."));
    }
    let prior = one(
        db,
        "SELECT * FROM agent_skills WHERE agent=? AND skill=?",
        [agent, skill],
    )?;
    if v["revision"]
        != prior
            .as_ref()
            .map(|c| c["revision"].clone())
            .unwrap_or(json!(0))
    {
        return Err(error(
            409,
            "Skill installation changed. Reload before saving.",
        ));
    }
    if prior
        .as_ref()
        .is_some_and(|p| !p["pack"].is_null() && p["removed"] == 0)
    {
        return Err(error(
            409,
            "Skill is managed by a skill pack. Change the installation through the skill pack.",
        ));
    }
    if v["remove"] == true
        && !prior
            .as_ref()
            .is_some_and(|p| p["removed"] == 0 && p["version"] == v["version"])
    {
        return Err(error(
            409,
            "Skill is not installed at that version. Reload installations.",
        ));
    }
    db.execute("INSERT INTO agent_skills(agent,skill,version,enabled,revision,removed) VALUES(?,?,?,?,?,?) ON CONFLICT(agent,skill) DO UPDATE SET version=excluded.version,enabled=excluded.enabled,revision=excluded.revision,removed=excluded.removed,pack=NULL",rusqlite::params![agent,skill,v["version"].as_i64(),i32::from(v["enabled"]==true),v["revision"].as_i64().ok_or_else(Failure::storage)?+1,i32::from(v["remove"]==true)])?;
    if context(db, agent)?.len() > 64 * 1024 {
        return Err(error(
            413,
            "Total active skill context limit is 64 KiB. Disable another skill first.",
        ));
    }
    one(
        db,
        "SELECT * FROM agent_skills WHERE agent=? AND skill=?",
        [agent, skill],
    )?
    .ok_or_else(Failure::storage)
}
fn members(v: &Value) -> Result<Vec<Value>> {
    let a = v
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 64)
        .ok_or_else(|| error(400, "Skill pack must contain 1 to 64 skills."))?;
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for m in a {
        if !keys(m, &["skill", "version"])
            || m["skill"]
                .as_str()
                .is_none_or(|s| crate::web::id(s).is_none())
            || !revision(&m["version"], 1)
        {
            return Err(error(400, "Invalid skill pack member."));
        }
        if !seen.insert(text(m, "skill")) {
            return Err(error(400, "Skills must not repeat within a skill pack."));
        }
        result.push(json!({"skill":m["skill"],"version":m["version"]}));
    }
    Ok(result)
}
pub fn save_pack(db: &Connection, id: &str, v: &Value) -> Result<Value> {
    let name = text(v, "name");
    if !keys(v, &["id", "revision", "name", "members"])
        || !revision(&v["revision"], 0)
        || name.trim().is_empty()
        || name.encode_utf16().count() > 80
    {
        return Err(error(400, "Invalid skill pack name or revision."));
    }
    let members = members(&v["members"])?;
    let current = one(db, "SELECT revision FROM skill_packs WHERE id=?", [id])?;
    if v["revision"]
        != current
            .as_ref()
            .map(|c| c["revision"].clone())
            .unwrap_or(json!(0))
    {
        return Err(error(409, "Skill pack changed. Reload before saving."));
    }
    for m in &members {
        if one(
            db,
            "SELECT skill FROM skill_versions WHERE skill=? AND revision=?",
            rusqlite::params![text(m, "skill"), m["version"].as_i64()],
        )?
        .is_none()
        {
            return Err(error(404, "Member skill version not found."));
        }
    }
    let revision = v["revision"].as_i64().ok_or_else(Failure::storage)? + 1;
    let name = name.trim();
    db.execute("INSERT INTO skill_packs VALUES(?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,revision=excluded.revision",rusqlite::params![id,name,revision])?;
    db.execute(
        "INSERT INTO skill_pack_versions VALUES(?,?,?,?)",
        rusqlite::params![id, revision, name, json!(members).to_string()],
    )?;
    Ok(json!({"id":id,"revision":revision,"name":name,"members":members}))
}
pub fn install_pack(db: &Connection, agent: &str, pack: &str, v: &Value) -> Result<Value> {
    agent_exists(db, agent)?;
    install_input(v, true)?;
    let saved = one(
        db,
        "SELECT members FROM skill_pack_versions WHERE pack=? AND revision=?",
        rusqlite::params![pack, v["version"].as_i64()],
    )?
    .ok_or_else(|| error(404, "Skill pack version not found."))?;
    let prior = one(
        db,
        "SELECT * FROM agent_packs WHERE agent=? AND pack=?",
        [agent, pack],
    )?;
    if v["revision"]
        != prior
            .as_ref()
            .map(|c| c["revision"].clone())
            .unwrap_or(json!(0))
    {
        return Err(error(
            409,
            "Skill pack installation changed. Reload before saving.",
        ));
    }
    if v["remove"] == true
        && !prior
            .as_ref()
            .is_some_and(|p| p["removed"] == 0 && p["version"] == v["version"])
    {
        return Err(error(409, "Skill pack is not installed at that version."));
    }
    let members: Vec<Value> =
        serde_json::from_str(text(&saved, "members")).map_err(|_| Failure::storage())?;
    if v["remove"] != true {
        for m in &members {
            if one(
                db,
                "SELECT pack,removed FROM agent_skills WHERE agent=? AND skill=?",
                [agent, text(m, "skill")],
            )?
            .is_some_and(|p| p["removed"] == 0 && p["pack"] != pack)
            {
                return Err(error(409,"Member skill was installed manually or is managed by another skill pack. Remove that installation first."));
            }
        }
    }
    db.execute("UPDATE agent_skills SET removed=1,enabled=0,revision=revision+1 WHERE agent=? AND pack=? AND removed=0",[agent,pack])?;
    if v["remove"] != true {
        for m in &members {
            db.execute("INSERT INTO agent_skills(agent,skill,version,enabled,revision,removed,pack) VALUES(?,?,?,?,1,0,?) ON CONFLICT(agent,skill) DO UPDATE SET version=excluded.version,enabled=excluded.enabled,revision=agent_skills.revision+1,removed=0,pack=excluded.pack",rusqlite::params![agent,text(m,"skill"),m["version"].as_i64(),i32::from(v["enabled"]==true),pack])?;
        }
    }
    if context(db, agent)?.len() > 64 * 1024 {
        return Err(error(413, "Total active skill context limit is 64 KiB."));
    }
    db.execute("INSERT INTO agent_packs VALUES(?,?,?,?,?,?) ON CONFLICT(agent,pack) DO UPDATE SET version=excluded.version,enabled=excluded.enabled,removed=excluded.removed,revision=excluded.revision",rusqlite::params![agent,pack,v["version"].as_i64(),i32::from(v["enabled"]==true),i32::from(v["remove"]==true),v["revision"].as_i64().ok_or_else(Failure::storage)?+1])?;
    one(
        db,
        "SELECT * FROM agent_packs WHERE agent=? AND pack=?",
        [agent, pack],
    )?
    .ok_or_else(Failure::storage)
}
pub async fn handle(state: &AppState, req: Request, parts: &[&str]) -> Result<Response> {
    let install = parts.get(1) == Some(&"agents");
    let pack = parts.get(if install { 3 } else { 1 }) == Some(&"skill-packs");
    let entity = parts.get(if install { 4 } else { 2 }).copied();
    let agent = if install { parts[2] } else { "" };
    if entity.is_some_and(|s| !valid_id(s)) || install && !valid_id(agent) {
        return Err(error(
            400,
            if pack {
                "Invalid skill pack ID."
            } else {
                "Invalid skill or agent ID."
            },
        ));
    }
    let q = query(&req);
    if req.method() == Method::GET {
        let db = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &db.connection;
        if install {
            let exists = one(
                db,
                if pack {
                    "SELECT id FROM skill_packs WHERE id=?"
                } else {
                    "SELECT id FROM skills WHERE id=?"
                },
                [entity.unwrap_or("")],
            )?;
            if entity.is_some() {
                if one(db, "SELECT id FROM agents WHERE id=?", [agent])?.is_none()
                    || exists.is_none()
                {
                    return Err(error(
                        404,
                        if pack {
                            "Agent or skill pack not found."
                        } else {
                            "Agent or skill not found."
                        },
                    ));
                }
                return Ok(json_response(
                    200,
                    one(
                        db,
                        if pack {
                            "SELECT * FROM agent_packs WHERE agent=? AND pack=?"
                        } else {
                            "SELECT * FROM agent_skills WHERE agent=? AND skill=?"
                        },
                        [agent, entity.unwrap_or("")],
                    )?
                    .unwrap_or(Value::Null),
                ));
            }
            agent_exists(db, agent)?;
            let after = parameter(&q, "after").unwrap_or("");
            if q.iter().any(|(k, _)| k != "after") || !after.is_empty() && !valid_id(after) {
                return Err(error(400, "Invalid installation position."));
            }
            return Ok(json_response(200,paginate(rows(db,"SELECT a.*,v.name,s.revision AS latest FROM agent_skills a JOIN skill_versions v ON v.skill=a.skill AND v.revision=a.version JOIN skills s ON s.id=a.skill WHERE a.agent=? AND a.removed=0 AND a.skill>? ORDER BY a.skill LIMIT 21",[agent,after])?,"items","skill")));
        }
        if let Some(id) = entity {
            let current = one(
                db,
                if pack {
                    "SELECT * FROM skill_packs WHERE id=?"
                } else {
                    "SELECT * FROM skills WHERE id=?"
                },
                [id],
            )?
            .ok_or_else(|| {
                error(
                    404,
                    if pack {
                        "Skill pack not found."
                    } else {
                        "Skill not found."
                    },
                )
            })?;
            let revision = parameter(&q, "version")
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or_else(|| current["revision"].as_i64().unwrap_or(0));
            if q.iter().any(|(k, _)| k != "version")
                || revision < 1
                || parameter(&q, "version").is_some_and(|v| v.parse::<i64>().is_err())
            {
                return Err(error(
                    400,
                    if pack {
                        "Invalid skill pack version."
                    } else {
                        "Invalid skill version."
                    },
                ));
            }
            let mut row = one(
                db,
                if pack {
                    "SELECT * FROM skill_pack_versions WHERE pack=? AND revision=?"
                } else {
                    "SELECT * FROM skill_versions WHERE skill=? AND revision=?"
                },
                rusqlite::params![id, revision],
            )?
            .ok_or_else(|| {
                error(
                    404,
                    if pack {
                        "Skill pack version not found."
                    } else {
                        "Skill version not found."
                    },
                )
            })?;
            row["id"] = json!(id);
            row["latest"] = current["revision"].clone();
            if pack {
                let mut members: Vec<Value> =
                    serde_json::from_str(text(&row, "members")).map_err(|_| Failure::storage())?;
                for member in &mut members {
                    if let Some(version)=one(db,"SELECT name,description,content FROM skill_versions WHERE skill=? AND revision=?",rusqlite::params![text(member,"skill"),member["version"].as_i64()])?{member.as_object_mut().ok_or_else(Failure::storage)?.extend(version.as_object().ok_or_else(Failure::storage)?.clone());}
                }
                row["members"] = json!(members);
            } else if let Some(files) = one(
                db,
                "SELECT files FROM skill_files WHERE skill=? AND revision=?",
                rusqlite::params![id, revision],
            )? {
                row["files"] =
                    serde_json::from_str(text(&files, "files")).map_err(|_| Failure::storage())?;
            }
            return Ok(json_response(200, row));
        }
        let after = parameter(&q, "after").unwrap_or("");
        if q.iter().any(|(k, _)| k != "after") || !after.is_empty() && !valid_id(after) {
            return Err(error(
                400,
                if pack {
                    "Invalid skill pack position."
                } else {
                    "Invalid library position."
                },
            ));
        }
        return Ok(json_response(
            200,
            paginate(
                rows(
                    db,
                    if pack {
                        "SELECT * FROM skill_packs WHERE id>? ORDER BY id LIMIT 21"
                    } else {
                        "SELECT * FROM skills WHERE id>? ORDER BY id LIMIT 21"
                    },
                    [after],
                )?,
                if pack { "packs" } else { "skills" },
                "id",
            ),
        ));
    }
    if req.method() != Method::POST {
        return Err(error(404, "Route not found."));
    }
    mutation(state, &req)?;
    let path = req.uri().path().to_owned();
    let v = body(
        req,
        if !install && !pack {
            4 * 1024 * 1024
        } else {
            64000
        },
    )
    .await?;
    if !valid_id(text(&v, "id")) {
        return Err(error(400, "Invalid request ID."));
    }
    let mut db = state.database.lock().map_err(|_| Failure::storage())?;
    if !install {
        if let Some(id) = entity {
            if one(
                &db.connection,
                if pack {
                    "SELECT id FROM skill_packs WHERE id=?"
                } else {
                    "SELECT id FROM skills WHERE id=?"
                },
                [id],
            )?
            .is_none()
            {
                return Err(error(
                    404,
                    if pack {
                        "Skill pack not found."
                    } else {
                        "Skill not found."
                    },
                ));
            }
        }
    }
    let result = transaction(&mut db.connection, |db| {
        let request = json!({"path":path,"data":v}).to_string();
        let id = text(&v, "id");
        if let Some(prior) = one(
            db,
            "SELECT request,result FROM skill_commands WHERE id=?",
            [id],
        )? {
            if prior["request"] != request {
                return Err(error(409, "ID was already used for a different change."));
            }
            return serde_json::from_str(text(&prior, "result")).map_err(|_| Failure::storage());
        }
        let entity = entity.unwrap_or(id);
        let result = if install {
            if pack {
                install_pack(db, agent, entity, &v)?
            } else {
                install_skill(db, agent, entity, &v)?
            }
        } else if pack {
            save_pack(db, entity, &v)?
        } else {
            save_skill(db, entity, &v)?
        };
        db.execute(
            "INSERT INTO skill_commands VALUES(?,?,?)",
            [id, &request, &result.to_string()],
        )?;
        Ok(result)
    })?;
    Ok(json_response(200, result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_migration_preserves_manual_skill_bindings() -> crate::db::Result<()> {
        let db = Connection::open_in_memory()?;
        db.execute_batch(
            "CREATE TABLE agents(id TEXT PRIMARY KEY); INSERT INTO agents VALUES ('a');
             CREATE TABLE skills(id TEXT PRIMARY KEY,name TEXT NOT NULL,revision INTEGER NOT NULL) STRICT;
             CREATE TABLE skill_versions(skill TEXT,revision INTEGER,name TEXT,description TEXT,content TEXT,PRIMARY KEY(skill,revision)) STRICT;
             CREATE TABLE agent_skills(agent TEXT,skill TEXT,version INTEGER,enabled INTEGER,revision INTEGER,removed INTEGER,PRIMARY KEY(agent,skill)) STRICT;
             INSERT INTO skills VALUES ('s','Manual',1);
             INSERT INTO skill_versions VALUES ('s',1,'Manual','Description','Content');
             INSERT INTO agent_skills VALUES ('a','s',1,1,4,0);",
        )?;

        super::super::init_schema(&db, false)?;
        super::super::init_schema(&db, false)?;
        let binding = one(&db, "SELECT * FROM agent_skills", []).map_err(|error| error.message)?;
        assert_eq!(
            binding,
            Some(
                json!({"agent":"a","skill":"s","version":1,"enabled":1,"revision":4,"removed":0,"pack":null})
            )
        );
        Ok(())
    }
}
