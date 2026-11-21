use super::*;
use sha2::{Digest, Sha256};

pub fn migrate(db: &Connection) -> Result<()> {
    if !all(db, "PRAGMA table_info(verification_artifacts)", [])?
        .iter()
        .any(|column| column["name"] == "file" && column["notnull"] == 1)
    {
        return Ok(());
    }
    db.execute_batch("SAVEPOINT verification_files")?;
    let result = db.execute_batch(
        "ALTER TABLE verification_artifacts RENAME TO verification_artifacts_old;
         CREATE TABLE verification_artifacts(turn TEXT NOT NULL REFERENCES turns(id),
             source TEXT NOT NULL,file TEXT REFERENCES files(id),kind TEXT NOT NULL,
             stamp TEXT NOT NULL,PRIMARY KEY(turn,source)) STRICT;
         INSERT INTO verification_artifacts SELECT * FROM verification_artifacts_old;
         DROP TABLE verification_artifacts_old;",
    );
    if result.is_err() {
        db.execute_batch("ROLLBACK TO verification_files")?;
    }
    db.execute_batch("RELEASE verification_files")?;
    Ok(result?)
}

fn stamp(db: &Connection, turn: &str) -> Result<String> {
    let changes:i64=db.query_row("SELECT count(*) FROM flow_tool_calls WHERE turn=? AND json_extract(call,'$.function.name') IN ('replace_workspace_file','create_workspace_file','delete_workspace_file','create_routine','update_routine','computer_open_url','computer_click','computer_type','computer_key','computer_scroll','browser_click','browser_type','browser_select','browser_hover','browser_press','browser_navigate','browser_back','browser_tabs') AND json_extract(result,'$.status') IS NOT 'error'",[turn],|r|r.get(0))?;
    let writes:i64=db.query_row("SELECT count(*) FROM harness_steps WHERE turn=? AND (name IN ('Write','Edit','MultiEdit','fileChange') OR name LIKE '%apply_patch%')",[turn],|r|r.get(0))?;
    let files: i64 = db.query_row(
        "SELECT count(*) FROM generated_files WHERE turn=?",
        [turn],
        |r| r.get(0),
    )?;
    Ok(json!([changes, writes, files]).to_string())
}
fn artifact(
    db: &Connection,
    turn: &str,
    source: &str,
    kind: &str,
    name: &str,
    bytes: &[u8],
) -> Result<()> {
    if exists(
        db,
        "SELECT 1 FROM verification_artifacts WHERE turn=? AND source=?",
        [turn, source],
    )? {
        return Ok(());
    }
    let row = one(db, "SELECT * FROM turns WHERE id=?", [turn])?.ok_or_else(Failure::storage)?;
    let file = uuid::Uuid::new_v4().to_string();
    db.execute_batch("SAVEPOINT verification_artifact")?;
    let result = (|| {
        db.execute(
            "INSERT INTO files(id,scope,name,digest,bytes,created) VALUES(?,?,?,?,?,?)",
            params![
                file,
                execution::scope(&row),
                name,
                format!("{:x}", Sha256::digest(bytes)),
                bytes,
                iso(db, crate::config::now())?
            ],
        )?;
        db.execute(
            "INSERT INTO verification_artifacts VALUES(?,?,?,?,?)",
            params![turn, source, file, kind, stamp(db, turn)?],
        )?;
        Ok(())
    })();
    if result.is_err() {
        let _ = db.execute_batch("ROLLBACK TO verification_artifact");
    }
    db.execute_batch("RELEASE verification_artifact")?;
    result
}
fn shell_command(command: &str) -> Result<String> {
    let wrapper = regex::Regex::new(r#"^/(?:usr/)?bin/(?:bash|sh) -[^\s]+ ['"]([\s\S]*)['"]$"#)
        .map_err(|_| Failure::storage())?;
    Ok(wrapper
        .captures(command)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
        .unwrap_or(command)
        .to_owned())
}
pub fn check_command(command: &str) -> Result<bool> {
    Ok(regex::Regex::new(r"(?:^|[;&|])\s*(?:(?:npm|pnpm|yarn|bun)\s+(?:run\s+)?(?:test(?::[\w-]+)?|build|lint|check)|pytest|vitest|jest|node\s+(?:(?:--[^\s]+\s+)*--test\b|[\w./-]+\.(?:test|spec)\.[cm]?js\b)|cargo\s+(?:test|check|build)|go\s+(?:test|build)|python3?\s+-m\s+(?:unittest|pytest)|make\s+(?:test|check|build))\b").map_err(|_|Failure::storage())?.is_match(&shell_command(command)?))
}
pub fn command(
    db: &Connection,
    turn: &str,
    id: &str,
    command: &str,
    exit_code: Option<i64>,
    explicit_check: bool,
) -> Result<()> {
    let check = explicit_check || check_command(command)?;
    let read = regex::Regex::new(r"(?:^|[;&|]\s*)(?:cat|head|tail|sed|wc|stat)\s")
        .map_err(|_| Failure::storage())?
        .is_match(&shell_command(command)?);
    if check || read && exit_code == Some(0) {
        // Shell output belongs to its step. Save the check receipt without a synthetic file.
        db.execute(
            "INSERT OR IGNORE INTO verification_artifacts(turn,source,kind,stamp) VALUES(?,?,?,?)",
            params![
                turn,
                format!("check:{id}"),
                if check { "check" } else { "file-check" },
                stamp(db, turn)?
            ],
        )?;
    }
    Ok(())
}
pub fn capture(db: &Connection, turn: &str) -> Result<()> {
    let mut statement=db.prepare("SELECT f.call_id,f.call,f.result,c.image FROM flow_tool_calls f LEFT JOIN computer_actions c ON c.turn=f.turn AND c.call_id=f.call_id WHERE f.turn=?")?;
    let rows = statement
        .query_map([turn], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<Vec<u8>>>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (id, call, result, image) in rows {
        let call: Value = serde_json::from_str(&call).map_err(|_| Failure::storage())?;
        let result: Value = serde_json::from_str(&result).map_err(|_| Failure::storage())?;
        if call["function"]["name"] == "run_shell" && result.get("error").is_none() {
            let args: Value = serde_json::from_str(string(&call["function"], "arguments"))
                .map_err(|_| Failure::storage())?;
            command(
                db,
                turn,
                &id,
                string(&args, "command"),
                result["exitCode"].as_i64(),
                args["check"] == true,
            )?;
        }
        if let Some(image) = image {
            artifact(
                db,
                turn,
                &format!("screenshot:{id}"),
                "screenshot",
                "verification-screenshot.png",
                &image,
            )?;
        }
    }
    Ok(())
}
fn evidence(db: &Connection, turn: &str, kind: &str) -> Result<bool> {
    match kind {
        "code"|"ui"=>exists(db,"SELECT 1 FROM verification_artifacts WHERE turn=? AND kind=? AND stamp=?",[turn,if kind=="code" {"check"}else{"screenshot"},&stamp(db,turn)?]),
        "routine"=>exists(db,"SELECT 1 FROM flow_tool_calls WHERE turn=? AND json_extract(call,'$.function.name')='read_routine' AND json_type(result,'$.error') IS NULL AND coalesce(json_extract(result,'$.routine.id'),json_extract(result,'$.id')) IS NOT NULL",[turn]),
        _=>Ok(exists(db,"SELECT 1 FROM verification_artifacts WHERE turn=? AND kind='file-check' AND stamp=?",[turn,&stamp(db,turn)?])? || exists(db,"SELECT 1 FROM flow_tool_calls WHERE turn=? AND json_extract(call,'$.function.name') IN ('read_attachment','read_workspace_file') AND json_type(result,'$.text')='text' AND after_output=(SELECT count(*) FROM generated_files WHERE turn=?)",[turn,turn])?),
    }
}
pub fn review(db: &Connection, turn: &Value, args: &Value) -> Result<Value> {
    if !closed(args, &["kind", "review"])
        || !["code", "ui", "file", "routine"].contains(&string(args, "kind"))
        || string(args, "review").trim().is_empty()
        || string(args, "review").encode_utf16().count() > 2000
    {
        return Ok(json!({"verified":false,"error":"Invalid verification review."}));
    }
    let id = string(turn, "id");
    capture(db, id)?;
    if !evidence(db, id, string(args, "kind"))? {
        return Ok(
            json!({"verified":false,"missing":format!("Run the relevant {} check before recording the review.",string(args,"kind"))}),
        );
    }
    db.execute("INSERT INTO turn_verification(turn,review,kind,stamp) VALUES(?,?,?,?) ON CONFLICT(turn) DO UPDATE SET review=excluded.review,kind=excluded.kind,stamp=excluded.stamp",params![id,string(args,"review"),string(args,"kind"),stamp(db,id)?])?;
    Ok(json!({"verified":true,"evidenceInWorkLog":true}))
}
pub fn nudge(db: &Connection, id: &str, turn: &Value, profile: &Value) -> Result<bool> {
    capture(db, id)?;
    let stamp = stamp(db, id)?;
    let used = one(db, "SELECT tool_budget_used FROM turns WHERE id=?", [id])?
        .ok_or_else(Failure::storage)?;
    let budget = profile["tool_budget"].as_i64().unwrap_or(90);
    if exists(db,"SELECT 1 FROM turn_verification WHERE turn=? AND (nudged=1 OR (review IS NOT NULL AND stamp=?))",[id,&stamp])? || exists(db,"SELECT 1 FROM verification_artifacts WHERE turn=? AND stamp=?",[id,&stamp])? || evidence(db,id,"routine")? || used["tool_budget_used"].as_i64().unwrap_or(0)>=budget || db.query_row("SELECT count(*) FROM harness_steps WHERE turn=?",[id],|r|r.get::<_,i64>(0))?>=budget {return Ok(false);}
    let generated = exists(db, "SELECT 1 FROM generated_files WHERE turn=?", [id])?;
    if generated && evidence(db, id, "file")? {
        return Ok(false);
    }
    let deliverable=generated || exists(db,"SELECT 1 FROM harness_steps WHERE turn=? AND name IN ('Write','Edit','MultiEdit','fileChange')",[id])? || regex::Regex::new(r"(?i)\b(build|implement|fix|create|save|set up|deliver|write|edit|update|change|make)\s").map_err(|_|Failure::storage())?.is_match(string(turn,"text"));
    if !deliverable {
        return Ok(false);
    }
    db.execute("INSERT INTO turn_verification(turn,nudged) VALUES(?,1) ON CONFLICT(turn) DO UPDATE SET nudged=1",[id])?;
    Ok(true)
}
pub fn receipt(db: &Connection, turn: &str, call: &str) -> Result<Option<Value>> {
    Ok(exists(
        db,
        "SELECT 1 FROM verification_artifacts WHERE turn=? AND source=? AND stamp=?",
        [turn, &format!("check:{call}"), &stamp(db, turn)?],
    )?
    .then(|| json!({"recorded":true,"evidenceInWorkLog":true})))
}
#[cfg(test)]
mod tests {
    #[test]
    fn check_runners() {
        for command in [
            "npm test",
            "npm run build",
            "pnpm test:unit",
            "node --test app.test.mjs",
            "node cart.test.mjs",
            "node tests/cart.test.js",
            "python3 -m unittest",
            "cargo test",
            "/bin/sh -lc 'cargo test'",
        ] {
            assert!(
                super::check_command(command).is_ok_and(|value| value),
                "{command}"
            );
        }
        for command in [
            "echo done",
            "cat test.js",
            "touch result.txt",
            "echo pytest",
            "cat jest.js",
            "echo node --test",
            "echo cargo test",
        ] {
            assert!(
                super::check_command(command).is_ok_and(|value| !value),
                "{command}"
            );
        }
    }

    #[test]
    fn evidence_stamps_invalidate_after_edits_and_distinguish_review_kinds() -> super::Result<()> {
        use super::*;
        let db = Connection::open_in_memory()?;
        db.execute_batch(
            "CREATE TABLE turns(id TEXT, agent TEXT, channel TEXT, thread TEXT, tool_budget_used INTEGER);
             INSERT INTO turns VALUES('turn','agent',NULL,NULL,0);
             CREATE TABLE files(id TEXT, scope TEXT, name TEXT, digest TEXT, bytes BLOB, created TEXT);
             CREATE TABLE flow_tool_calls(turn TEXT, call_id TEXT, call TEXT, result TEXT, after_output INTEGER DEFAULT 0);
             CREATE TABLE harness_steps(turn TEXT, name TEXT);
             CREATE TABLE generated_files(turn TEXT);
             CREATE TABLE computer_actions(turn TEXT, call_id TEXT, image BLOB);
             CREATE TABLE turn_verification(turn TEXT PRIMARY KEY, review TEXT, kind TEXT, stamp TEXT, nudged INTEGER);
             CREATE TABLE verification_artifacts(turn TEXT, source TEXT, file TEXT, kind TEXT, stamp TEXT, PRIMARY KEY(turn,source));",
        )?;
        let turn = json!({"id":"turn","text":"Create a result"});
        let profile = json!({});
        let review_kind = |kind: &str| -> Result<Value> {
            review(
                &db,
                &turn,
                &json!({"kind":kind,"review":"Compared with the request"}),
            )
        };
        let tool = |id: &str, name: &str, args: Value, result: Value| -> Result<()> {
            let call = json!({"function":{"name":name,"arguments":args.to_string()}});
            db.execute(
                "INSERT INTO flow_tool_calls(turn,call_id,call,result) VALUES('turn',?,?,?)",
                params![id, call.to_string(), result.to_string()],
            )?;
            Ok(())
        };
        command(
            &db,
            "turn",
            "native-check",
            "node result.test.mjs",
            Some(0),
            false,
        )?;
        let recorded = Some(json!({"recorded":true,"evidenceInWorkLog":true}));
        assert_eq!(receipt(&db, "turn", "native-check")?, recorded);
        assert!(!nudge(&db, "turn", &turn, &profile)?);
        tool(
            "readback",
            "run_shell",
            json!({"command":"cat result.txt"}),
            json!({"output":"Requested result","exitCode":0}),
        )?;
        assert_eq!(review_kind("code")?["verified"], true);
        assert!(!nudge(&db, "turn", &turn, &profile)?);
        db.execute("INSERT INTO harness_steps VALUES('turn','fileChange')", [])?;
        assert_eq!(review_kind("code")?["verified"], false);
        assert_eq!(receipt(&db, "turn", "native-check")?, None);
        assert!(nudge(&db, "turn", &turn, &profile)?);
        assert!(!nudge(&db, "turn", &turn, &profile)?);
        assert!(!exists(&db, "SELECT 1 FROM files", [])?);
        tool("capture", "computer_screenshot", json!({}), json!({}))?;
        db.execute(
            "INSERT INTO computer_actions VALUES('turn','capture',?)",
            [b"screenshot fixture".as_slice()],
        )?;
        assert_eq!(review_kind("ui")?["verified"], true);
        assert_eq!(review_kind("routine")?["verified"], false);
        db.execute("DELETE FROM turn_verification", [])?;
        db.execute("INSERT INTO harness_steps VALUES('turn','fileChange')", [])?;
        tool(
            "routine",
            "read_routine",
            json!({}),
            json!({"routine":{"id":"routine"}}),
        )?;
        assert!(!nudge(&db, "turn", &turn, &profile)?);
        assert_eq!(review_kind("routine")?["verified"], true);
        tool(
            "custom-check",
            "run_shell",
            json!({"command":"python3 custom_check.py","check":true}),
            json!({"output":"","exitCode":0}),
        )?;
        capture(&db, "turn")?;
        assert_eq!(receipt(&db, "turn", "custom-check")?, recorded);
        assert_eq!(review_kind("code")?["verified"], true);
        assert_eq!(
            db.query_row("SELECT count(*) FROM verification_artifacts", [], |r| r
                .get::<_, u32>(0))?,
            4
        );
        Ok(())
    }
}
