//! Workspace backups through `POST /api/backup` and restores through
//! `minarbot-server restore workspace` (the former
//! `app/test-support/backup.ts` `restoreWorkspace`).

use crate::{server::server_binary, Result, Server, TestRoot};
use serde_json::{json, Value};
use std::path::Path;
use tokio::process::Command;

/// Download a confirmed workspace backup (an SQLite image).
pub async fn download(app: &Server) -> Result<Vec<u8>> {
    let response = app
        .request(reqwest::Method::POST, "/api/backup")?
        .header("Content-Type", "application/json")
        .body(json!({"id": crate::new_id(), "confirmed": true}).to_string())
        .send()
        .await?;
    let status = response.status().as_u16();
    let bytes = response.bytes().await?.to_vec();
    if status != 200 {
        return Err(format!(
            "POST /api/backup: HTTP {status}: {}",
            String::from_utf8_lossy(&bytes)
        )
        .into());
    }
    Ok(bytes)
}

/// `minarbot-server restore workspace <backup> --db <destination>`; the JSON
/// result's `exitCode` must equal the process status and be 0.
pub async fn restore_workspace(backup: &Path, destination: &Path) -> Result<Value> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let output = Command::new(server_binary(None)?)
        .arg("restore")
        .arg("workspace")
        .arg(backup)
        .arg("--db")
        .arg(destination)
        .env_clear()
        .env("PATH", crate::server::HERMETIC_PATH)
        .env("LANG", "C.UTF-8")
        .output()
        .await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(stdout.trim()).map_err(|error| {
        format!(
            "restore printed no JSON ({error}): {stdout} {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })?;
    let code = output.status.code().unwrap_or(-1);
    if value["exitCode"] != code || code != 0 {
        return Err(format!("restore failed with status {code}: {value}").into());
    }
    Ok(value)
}

/// Back up `app` and restore the image into the database of a fresh root.
pub async fn restore_into_new_root(app: &Server) -> Result<TestRoot> {
    let bytes = download(app).await?;
    let root = TestRoot::new()?;
    let file = root.path().join("backup.sqlite");
    std::fs::write(&file, bytes)?;
    restore_workspace(&file, &root.database()).await?;
    Ok(root)
}

/// Snapshot a stopped server's database into `file` the way the former
/// `workspaceSnapshot` did: an SQLite copy stamped as a workspace backup
/// (canonical schemas also record their authority generation and owner).
pub fn workspace_snapshot(source: &Path, file: &Path) -> Result<()> {
    let database = crate::db::Fixture::open(source)?;
    database.execute("VACUUM INTO ?", [file.to_string_lossy()])?;
    drop(database);
    let snapshot = crate::db::Fixture::open(file)?;
    snapshot.execute_batch(
        "CREATE TABLE IF NOT EXISTS minarbot_backup (format TEXT NOT NULL,version INTEGER NOT NULL,created TEXT NOT NULL) STRICT;
        DELETE FROM minarbot_backup;",
    )?;
    snapshot.execute(
        "INSERT INTO minarbot_backup VALUES ('minarbot-workspace',1,'2026-11-25T00:00:00.000Z')",
        [],
    )?;
    let migrations: i64 = snapshot.scalar(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='schema_migrations'",
        [],
    )?;
    let version: i64 = if migrations == 0 {
        0
    } else {
        snapshot.scalar("SELECT coalesce(max(version),0) FROM schema_migrations", [])?
    };
    if version > 0 {
        let generation: String =
            snapshot.scalar("SELECT generation FROM authority_state WHERE id=1", [])?;
        snapshot.execute_batch(
            "UPDATE minarbot_backup SET format='minarbot-canonical-v6',version=6;
            CREATE TABLE IF NOT EXISTS minarbot_backup_authority(generation TEXT NOT NULL,owner_account TEXT REFERENCES accounts(id)) STRICT;",
        )?;
        let owners = snapshot.rows("SELECT DISTINCT account FROM projects", [])?;
        let owner = match owners.as_slice() {
            [one] => one["account"].as_str().map(str::to_owned),
            [] => None,
            _ => return Err("Canonical backup requires its recorded owner".into()),
        };
        snapshot.execute_batch("DELETE FROM minarbot_backup_authority")?;
        snapshot.execute(
            "INSERT INTO minarbot_backup_authority VALUES (?,?)",
            (generation, owner),
        )?;
    }
    Ok(())
}
