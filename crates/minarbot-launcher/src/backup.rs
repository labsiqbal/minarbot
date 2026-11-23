use crate::{files::*, timestamp, Result};
use minarbot_launcher::data_format;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::{fs, os::unix::fs::MetadataExt, path::Path, time::Duration};

pub fn safe_error(error: Box<dyn std::error::Error>) -> Box<dyn std::error::Error> {
    match error.downcast::<rusqlite::Error>() {
        // SQLite's free-form messages can include database values or names.
        Ok(error) => match error.sqlite_error() {
            Some(code) => format!("SQLite {code}").into(),
            None => "SQLite could not read or prepare backup metadata.".into(),
        },
        Err(error) => error,
    }
}

pub fn run(command: &str, database: &Path, destination: &Path) -> Result<()> {
    let source = fs::canonicalize(database)?;
    let metadata = fs::symlink_metadata(&source)?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err("Canonical database must be a file without hard links.".into());
    }
    let _guard = lease(
        &sidecar(&source, ".server-lock.sqlite"),
        "Another canonical server is still running.",
    )?;
    match command {
        "snapshot" => {
            mkdir(destination.parent().ok_or("Invalid snapshot path")?)?;
            if destination.exists() {
                return Ok(());
            }
            let temp = destination.with_extension("tmp.sqlite");
            let result =
                snapshot(&source, &temp).and_then(|()| durable(destination, &fs::read(&temp)?));
            let _ = fs::remove_file(temp);
            result
        }
        "restore" => {
            let snapshot =
                Connection::open_with_flags(destination, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let (format, version): (String, i64) =
                snapshot.query_row("SELECT format,version FROM minarbot_backup", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            let integrity: String =
                snapshot.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
            // The launcher took this snapshot from the live database itself, so it
            // carries whatever marker that database had; refusing it here would only
            // block the rollback. Backups from elsewhere are checked by the server.
            if !data_format::is_backup(&format, version)
                || integrity != "ok"
                || snapshot.prepare("PRAGMA foreign_key_check")?.exists([])?
            {
                return Err("Invalid update snapshot.".into());
            }
            drop(snapshot);
            let staged = source
                .parent()
                .ok_or("Invalid database path")?
                .join(".update-restore.sqlite");
            durable(&staged, &fs::read(destination)?)?;
            for suffix in ["-wal", "-shm", "-journal"] {
                remove(&sidecar(&source, suffix))?;
            }
            fs::rename(staged, &source)?;
            sync_dir(source.parent().ok_or("Invalid database path")?)
        }
        _ => Err("Unknown update backup operation.".into()),
    }
}
fn snapshot(source: &Path, target: &Path) -> Result<()> {
    let source = Connection::open(source)?;
    let mut db = Connection::open(target)?;
    {
        let backup = rusqlite::backup::Backup::new(&source, &mut db)?;
        // With the server stopped and its lease held, only contention needs a pause.
        backup.run_to_completion(i32::MAX, Duration::from_millis(10), None)?;
    }
    let db = db.transaction()?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS minarbot_backup (format TEXT NOT NULL,version INTEGER NOT NULL,created TEXT NOT NULL) STRICT; DELETE FROM minarbot_backup;")?;
    let (format, version) = data_format::WORKSPACE_BACKUP;
    db.execute(
        "INSERT INTO minarbot_backup VALUES (?,?,?)",
        rusqlite::params![format, version, timestamp()?],
    )?;
    let migrations = db
        .prepare("SELECT 1 FROM sqlite_schema WHERE type='table' AND name='schema_migrations'")?
        .exists([])?;
    if migrations
        && db.query_row(
            "SELECT coalesce(max(version),0) FROM schema_migrations",
            [],
            |r| r.get::<_, i64>(0),
        )? != 0
    {
        let generation: String = db.query_row(
            "SELECT generation FROM authority_state WHERE id=1",
            [],
            |r| r.get(0),
        )?;
        let (format, version) = data_format::CANONICAL_BACKUP;
        db.execute(
            "UPDATE minarbot_backup SET format=?,version=?",
            rusqlite::params![format, version],
        )?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS minarbot_backup_authority(generation TEXT NOT NULL,owner_account TEXT REFERENCES accounts(id)) STRICT;")?;
        let columns = db
            .prepare("PRAGMA table_info(minarbot_backup_authority)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if !columns.iter().any(|c| c == "owner_account") {
            db.execute_batch("ALTER TABLE minarbot_backup_authority ADD COLUMN owner_account TEXT REFERENCES accounts(id)")?;
        }
        let previous: Option<String> = db
            .query_row(
                "SELECT owner_account FROM minarbot_backup_authority",
                [],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let owners = db
            .prepare("SELECT DISTINCT account FROM projects")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let owner = previous.or_else(|| {
            if owners.len() == 1 {
                owners.first().cloned()
            } else {
                None
            }
        });
        if !owners.is_empty() && !owner.as_ref().is_some_and(|o| owners.contains(o)) {
            return Err("Canonical backup requires its recorded owner".into());
        }
        db.execute("DELETE FROM minarbot_backup_authority", [])?;
        db.execute(
            "INSERT INTO minarbot_backup_authority VALUES (?,?)",
            rusqlite::params![generation, owner],
        )?;
    }
    db.commit()?;
    Ok(())
}
