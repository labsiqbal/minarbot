use super::*;
use minarbot_launcher::data_format;
use rusqlite::{OpenFlags, TransactionBehavior};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

const MIGRATION: &str = include_str!("migrations/001-hub.sql");
const EXTENSIONS: &str = include_str!("extensions.sql");

pub(super) struct Writer {
    _file: File,
    path: PathBuf,
}
impl Drop for Writer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub(super) fn open(path: &Path) -> crate::db::Result<(Connection, Writer)> {
    let directory = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(directory)?;
    let name = path
        .file_name()
        .filter(|n| *n != ":memory:")
        .ok_or("Persistent hub path required.")?;
    let path = directory.canonicalize()?.join(name);
    let lock = minarbot_launcher::files::sidecar(&path, ".writer.lock");
    let writer = Writer {
        _file: OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&lock)?,
        path: lock,
    };
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let file = minarbot_launcher::files::sidecar(&path, suffix);
        match fs::symlink_metadata(&file) {
            Ok(meta)
                if meta.is_file()
                    && meta.nlink() == 1
                    && meta.mode() & 0o077 == 0
                    && meta.uid() == rustix::process::geteuid().as_raw() => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            _ => {
                return Err(
                    "Hub database and sidecars must be private files without aliases.".into(),
                )
            }
        }
    }
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&path)?;
    let mut db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF; PRAGMA busy_timeout=5000; PRAGMA synchronous=FULL")?;
    initialize(&mut db)?;
    db.pragma_update(None, "journal_mode", "WAL")?;
    Ok((db, writer))
}

fn initialize(db: &mut Connection) -> crate::db::Result<()> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !data_format::adopt(&tx)? {
        return Err(data_format::UNMARKED.into());
    }
    let fresh = crate::db::empty_schema(&tx)?;
    let checksum = digest(MIGRATION);
    if fresh {
        tx.execute_batch("CREATE TABLE hub_migrations(version INTEGER PRIMARY KEY, checksum TEXT NOT NULL) STRICT;")?;
        tx.execute_batch(MIGRATION)?;
        tx.execute("INSERT INTO hub_migrations VALUES(1,?)", [&checksum])?;
        tx.execute(
            "INSERT INTO hub_state(id,authority_generation,state) VALUES(1,?,'active')",
            [new_id()],
        )?;
    } else {
        let mut stmt = tx
            .prepare("SELECT version,checksum FROM hub_migrations ORDER BY version")
            .map_err(|_| "Refusing non-hub database.")?;
        let found = stmt
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if found != [(1, checksum)] {
            return Err("Incompatible hub migration version/checksum.".into());
        }
    }
    tx.execute_batch(EXTENSIONS)?;
    if tx.prepare("PRAGMA foreign_key_check")?.exists([])? {
        return Err("Hub foreign key check failed.".into());
    }
    tx.commit()?;
    Ok(())
}

pub(super) fn fence_restart(db: &mut Connection) -> crate::db::Result<()> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // A process restart cannot prove an admitted turn or lease completed.
    tx.execute_batch(
        "
        UPDATE nodes SET status='offline' WHERE status='ready';
        UPDATE turns SET state='unknown' WHERE state IN('pending','admitted');
        DELETE FROM outbox WHERE kind NOT IN('grant.revoked','turn.cancel');
    ",
    )?;
    tx.commit()?;
    Ok(())
}

pub(super) fn command(path: &Path, args: &[std::ffi::OsString]) -> crate::db::Result<()> {
    let usage = "Use --hub [backup PATH | fence-restore | activate GENERATION | admit ISSUER SUBJECT | account-status ID active|blocked].";
    let verb = args.first().and_then(|arg| arg.to_str()).ok_or(usage)?;
    if !matches!(
        (verb, args.len()),
        ("backup" | "activate", 2) | ("fence-restore", 1) | ("admit" | "account-status", 3)
    ) {
        return Err(usage.into());
    }
    let (_writer, mut db);
    (db, _writer) = open(path)?;
    if verb == "backup" {
        return backup(&db, Path::new(&args[1]));
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let output = match verb {
        "fence-restore" => {
            let generation = new_id();
            tx.execute("UPDATE hub_state SET authority_generation=?,state='recovery',revision=revision+1 WHERE id=1", [&generation])?;
            tx.execute_batch("
                UPDATE invitations SET state='held',revision=revision+1 WHERE state='pending';
                UPDATE nodes SET status=CASE WHEN status='revoked' THEN status ELSE 'offline' END,generation=generation+1,connection_generation=connection_generation+1;
                UPDATE turns SET state='unknown' WHERE state IN('pending','admitted');
                DELETE FROM outbox;
                DELETE FROM action_leases;
                DELETE FROM node_link_codes;
                DELETE FROM node_link_sessions;
            ")?;
            json!({"authorityGeneration":generation,"state":"recovery"})
        }
        "activate" => {
            let generation = args[1].to_str().and_then(crate::web::id).ok_or(usage)?;
            let updated = tx.execute("UPDATE hub_state SET state='active',revision=revision+1 WHERE id=1 AND state='recovery' AND authority_generation=?", [&generation])?;
            if updated != 1 {
                return Err("Recovery generation changed.".into());
            }
            json!({"authorityGeneration":generation,"state":"active"})
        }
        "admit" => {
            let issuer = args[1]
                .to_str()
                .filter(|v| !v.is_empty() && v.len() <= 1024)
                .ok_or(usage)?;
            let subject = args[2]
                .to_str()
                .filter(|v| !v.trim().is_empty() && v.len() <= 256)
                .ok_or(usage)?;
            generation(&tx).map_err(|error| error.message)?;
            tx.execute("INSERT INTO accounts(id,issuer,subject,status,pilot_admitted) VALUES(?,?,?,'active',1)
                ON CONFLICT(issuer,subject) DO UPDATE SET pilot_admitted=1 WHERE pilot_admitted=0",
                params![new_id(), issuer, subject])?;
            let account: String = tx.query_row(
                "SELECT id FROM accounts WHERE issuer=? AND subject=?",
                [issuer, subject],
                |r| r.get(0),
            )?;
            json!({"accountId":account})
        }
        "account-status" => {
            let account = args[1].to_str().and_then(crate::web::id).ok_or(usage)?;
            let status = args[2]
                .to_str()
                .filter(|v| matches!(*v, "active" | "blocked"))
                .ok_or(usage)?;
            authority::account_status(&tx, &account, status).map_err(|error| error.message)?;
            json!({"accountId":account,"status":status})
        }
        _ => return Err(usage.into()),
    };
    tx.commit()?;
    println!("{output}");
    Ok(())
}

fn backup(db: &Connection, path: &Path) -> crate::db::Result<()> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    let result = (|| -> crate::db::Result<()> {
        let mut copy = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        rusqlite::backup::Backup::new(db, &mut copy)?.run_to_completion(
            128,
            Duration::from_millis(10),
            None,
        )?;
        file.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        fs::remove_file(path)?;
    }
    result
}
