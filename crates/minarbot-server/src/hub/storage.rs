use super::*;
use minarbot_launcher::data_format;
use rusqlite::{OpenFlags, TransactionBehavior};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

const MIGRATION: &str = include_str!("../../../../app/hub/migrations/001-hub.sql");
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
