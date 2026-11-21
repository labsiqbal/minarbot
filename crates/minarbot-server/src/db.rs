use minarbot_launcher::data_format;
use rusqlite::{functions::FunctionFlags, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};

mod automation;

mod sql {
    include!(concat!(env!("OUT_DIR"), "/schema.rs"));
}
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub const SCHEMA_VERSION: i64 = 6;
const PROVIDER_ACCOUNTS: &str = include_str!("data/provider-accounts.sql");
const CANONICAL_INDEXES: &str = concat!(
    include_str!("files/indexes.sql"),
    include_str!("turns/indexes.sql"),
    include_str!("provider-indexes.sql"),
    include_str!("automation/project-indexes.sql"),
);

pub fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
pub fn canonical(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|key| format!("{}:{}", json!(key), canonical(&map[key])))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        _ => value.to_string(),
    }
}

pub struct Database {
    pub connection: Connection,
    _guard: Option<Connection>,
}
pub fn open(path: &Path, canonical_mode: bool, now: i64) -> Result<Database> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    let connection = Connection::open(path)?;
    connection.set_prepared_statement_cache_capacity(128);
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    let guard = server_guard(path)?;
    if !data_format::adopt(&connection)? {
        return Err(data_format::UNMARKED.into());
    }
    connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000")?;
    if canonical_mode {
        migrate(&connection, now, SCHEMA_VERSION)?;
    } else {
        if table(&connection, "authority_state")? || table(&connection, "schema_migrations")? {
            return Err(
                "Canonical database requires canonical mode; legacy dispatcher is disabled.".into(),
            );
        }
        local(&connection)?;
    }
    Ok(Database {
        connection,
        _guard: Some(guard),
    })
}
fn server_guard(path: &Path) -> Result<Connection> {
    let source = fs::canonicalize(path)?;
    if fs::metadata(&source)?.nlink() != 1 {
        return Err("Canonical database must be a file without hard links.".into());
    }
    let guard_path = minarbot_launcher::files::sidecar(&source, ".server-lock.sqlite");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(&guard_path)
        .map_err(|error| {
            if error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "Server guard must be a private file owned by the user.",
                )
            } else {
                error
            }
        })?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err("Server guard must be a private file owned by the user.".into());
    }
    drop(file);
    let guard = Connection::open(&guard_path)?;
    guard
        .execute_batch("PRAGMA busy_timeout=0; PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE")
        .map_err(|_| "Another canonical server is still running.")?;
    Ok(guard)
}

pub(crate) fn open_recovery(path: &Path, exclusive: bool) -> Result<Database> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let companion = minarbot_launcher::files::sidecar(path, suffix);
        match fs::symlink_metadata(companion) {
            Ok(meta)
                if meta.is_file()
                    && meta.nlink() == 1
                    && meta.uid() == rustix::process::getuid().as_raw()
                    && meta.mode() & 0o077 == 0 => {}
            Err(error) if !suffix.is_empty() && error.kind() == std::io::ErrorKind::NotFound => {}
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "Recovery database must be a private file owned by the user.",
                )
                .into())
            }
        }
    }
    let guard = exclusive.then(|| server_guard(path)).transpose()?;
    let connection = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    connection.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000")?;
    Ok(Database {
        connection,
        _guard: guard,
    })
}

fn table(db: &Connection, name: &str) -> Result<bool> {
    Ok(db
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?",
            [name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}
fn column(db: &Connection, table: &str, name: &str, definition: &str) -> Result<()> {
    let mut query = db.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = query
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if !columns.iter().any(|value| value == name) {
        db.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {name} {definition}"
        ))?;
    }
    Ok(())
}
fn messages(db: &Connection) -> Result<()> {
    db.execute_batch(include_str!("turns/agent-visibility.sql"))?;
    column(db, "turns", "origin", "TEXT")?;
    db.execute_batch(include_str!("turns/agent-messages.sql"))?;
    db.execute_batch(sql::REMOTE)?;
    column(db, "turns", "message_depth", "INTEGER NOT NULL DEFAULT 0")?;
    db.execute_batch(include_str!("turns/agent-delegation.sql"))?;
    db.execute_batch(include_str!("turns/rooms.sql"))?;
    column(
        db,
        "room_discussions",
        "stopped",
        "INTEGER NOT NULL DEFAULT 0",
    )
}

pub fn migrate(db: &Connection, now: i64, through: i64) -> Result<()> {
    db.create_scalar_function(
        "minarbot_capability_digest",
        1,
        FunctionFlags::SQLITE_DETERMINISTIC | FunctionFlags::SQLITE_UTF8,
        |context| {
            let raw: String = context.get(0)?;
            let value: Value = serde_json::from_str(&raw)
                .map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))?;
            Ok(digest(&canonical(&json!([
                "minarbot-capabilities-v1",
                value
            ]))))
        },
    )?;
    db.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<()> {
        let mut version: i64 = if table(db, "schema_migrations")? {
            db.query_row(
                "SELECT coalesce(max(version),0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )?
        } else {
            0
        };
        if version > SCHEMA_VERSION {
            return Err("Database schema is newer than this server".into());
        }
        if version > through {
            return Err("Schema downgrade is not supported".into());
        }
        let migrations = [
            (sql::MIGRATION, "v1_owner_project"),
            (sql::MIGRATIONV2, "v2_device_authority"),
            (sql::MIGRATIONV3, "v3_task_fencing"),
            (sql::MIGRATIONV4, "v4_sync_retention"),
            (CANONICAL_INDEXES, "v5_canonical_indexes"),
            (PROVIDER_ACCOUNTS, "v6_provider_accounts"),
        ];
        for (index, (sql, name)) in migrations.iter().enumerate() {
            let next = index as i64 + 1;
            if next > through {
                break;
            }
            if version < next {
                if next == 3 {
                    db.execute_batch("CREATE TABLE IF NOT EXISTS threads(id TEXT PRIMARY KEY,agent TEXT NOT NULL REFERENCES agents(id),name TEXT NOT NULL,revision INTEGER NOT NULL DEFAULT 1,archived INTEGER NOT NULL DEFAULT 0,deleted INTEGER NOT NULL DEFAULT 0) STRICT; CREATE TABLE IF NOT EXISTS channels(id TEXT PRIMARY KEY,name TEXT NOT NULL,instructions TEXT NOT NULL,lead TEXT REFERENCES agents(id),revision INTEGER NOT NULL DEFAULT 1,archived INTEGER NOT NULL DEFAULT 0) STRICT; CREATE TABLE IF NOT EXISTS channel_members(channel TEXT NOT NULL REFERENCES channels(id),agent TEXT NOT NULL REFERENCES agents(id),PRIMARY KEY(channel,agent)) STRICT;")?;
                    for (name, definition) in [
                        ("thread", "TEXT REFERENCES threads(id)"),
                        ("channel", "TEXT REFERENCES channels(id)"),
                        ("attachments", "TEXT NOT NULL DEFAULT '[]'"),
                        ("recipient", "TEXT REFERENCES agents(id)"),
                        ("automatic", "INTEGER NOT NULL DEFAULT 0"),
                        ("routing", "TEXT"),
                        ("paused", "INTEGER NOT NULL DEFAULT 0"),
                        ("revision", "INTEGER NOT NULL DEFAULT 1"),
                        ("profile_setup", "INTEGER NOT NULL DEFAULT 0"),
                    ] {
                        column(db, "turns", name, definition)?;
                    }
                }
                if next == 5 {
                    provider_tables(db)?;
                    crate::turns::initialize_canonical(db).map_err(|error| error.message)?;
                    crate::files::init_canonical_schema(db)?;
                    automation::upgrade(db)?;
                }
                db.execute_batch(sql)?;
                if next == 1 {
                    column(db, "agents", "revision", "INTEGER NOT NULL DEFAULT 1")?;
                    db.execute(
                        "INSERT INTO authority_state VALUES (1,?,'active',0,NULL,NULL,1)",
                        [uuid::Uuid::new_v4().to_string()],
                    )?;
                }
                if next == 2 {
                    for (name, definition) in [
                        ("thread_limit", "INTEGER NOT NULL DEFAULT 2"),
                        ("backup_model", "TEXT NOT NULL DEFAULT ''"),
                        ("provider_account", "TEXT NOT NULL DEFAULT ''"),
                    ] {
                        column(db, "agents", name, definition)?;
                    }
                }
                db.execute(
                    "INSERT INTO schema_migrations VALUES (?,?,?,?)",
                    rusqlite::params![next, name, digest(sql), now],
                )?;
                version = next;
            } else {
                let saved: Option<(String, String)> = db
                    .query_row(
                        "SELECT name,checksum FROM schema_migrations WHERE version=?",
                        [next],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                if saved != Some((name.to_string(), digest(sql))) {
                    return Err("Schema migration checksum mismatch".into());
                }
                if next == 5 {
                    // Legacy schedule mappings may become eligible after the schema is installed.
                    automation::upgrade(db)?;
                }
            }
            if next == 1 {
                column(db, "agents", "role", "TEXT NOT NULL DEFAULT ''")?;
                column(db, "agents", "tool_budget", "INTEGER NOT NULL DEFAULT 90")?;
            }
            if next == 3 {
                db.execute_batch("CREATE TABLE IF NOT EXISTS recovery_quarantine(request_id TEXT NOT NULL REFERENCES receipts(request_id), owner_account TEXT NOT NULL REFERENCES accounts(id), task TEXT NOT NULL, action TEXT NOT NULL, source_device TEXT NOT NULL REFERENCES devices(id), source_authority TEXT NOT NULL, recovery_generation TEXT NOT NULL, watermark INTEGER NOT NULL CHECK(watermark>=0), envelope_digest TEXT NOT NULL, envelope_json TEXT NOT NULL CHECK(json_valid(envelope_json)), reason TEXT NOT NULL, created INTEGER NOT NULL, PRIMARY KEY(request_id,action)) STRICT; CREATE INDEX IF NOT EXISTS recovery_quarantine_scope ON recovery_quarantine(owner_account,task,created);")?;
                db.execute_batch(sql::TASKSUCCESSORCONSTRAINT)?;
                column(db, "turns", "parent", "TEXT REFERENCES turns(id)")?;
                column(
                    db,
                    "turns",
                    "no_update",
                    "INTEGER NOT NULL DEFAULT 0 CHECK(no_update IN(0,1))",
                )?;
                if through == 3 {
                    messages(db)?;
                }
            }
            if next == 4 {
                db.execute_batch(sql::PROTECTEDWAITSCHEMA)?;
                db.execute_batch(sql::SYNCSCOPESCHEMA)?;
                messages(db)?;
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            db.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            db.execute_batch("ROLLBACK")?;
            Err(error)
        }
    }
}

fn local(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS agents(id TEXT PRIMARY KEY,name TEXT NOT NULL,model TEXT NOT NULL,instructions TEXT NOT NULL) STRICT; CREATE TABLE IF NOT EXISTS turns(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,agent TEXT NOT NULL REFERENCES agents(id),text TEXT NOT NULL,reply TEXT,status TEXT NOT NULL CHECK(status IN('queued','running','done','failed','interrupted','cancelled')),error TEXT,created TEXT NOT NULL,tool_budget_used INTEGER NOT NULL DEFAULT 0,tool_budget_stopped INTEGER NOT NULL DEFAULT 0) STRICT; CREATE INDEX IF NOT EXISTS turns_agent_seq ON turns(agent,seq);")?;
    for (name, definition) in [
        ("role", "TEXT NOT NULL DEFAULT ''"),
        ("revision", "INTEGER NOT NULL DEFAULT 1"),
        ("thread_limit", "INTEGER NOT NULL DEFAULT 2"),
        ("tool_budget", "INTEGER NOT NULL DEFAULT 90"),
        ("backup_model", "TEXT NOT NULL DEFAULT ''"),
        ("provider_account", "TEXT NOT NULL DEFAULT ''"),
        ("runtime", "TEXT NOT NULL DEFAULT 'api'"),
    ] {
        column(db, "agents", name, definition)?;
    }
    db.execute_batch("CREATE TABLE IF NOT EXISTS threads(id TEXT PRIMARY KEY,agent TEXT NOT NULL REFERENCES agents(id),name TEXT NOT NULL,revision INTEGER NOT NULL DEFAULT 1,archived INTEGER NOT NULL DEFAULT 0,deleted INTEGER NOT NULL DEFAULT 0) STRICT; CREATE TABLE IF NOT EXISTS channels(id TEXT PRIMARY KEY,name TEXT NOT NULL,instructions TEXT NOT NULL,lead TEXT REFERENCES agents(id),revision INTEGER NOT NULL DEFAULT 1,archived INTEGER NOT NULL DEFAULT 0) STRICT;")?;
    for (name, definition) in [
        ("thread", "TEXT REFERENCES threads(id)"),
        ("channel", "TEXT REFERENCES channels(id)"),
        ("attachments", "TEXT NOT NULL DEFAULT '[]'"),
        ("parent", "TEXT REFERENCES turns(id)"),
    ] {
        column(db, "turns", name, definition)?;
    }
    provider_tables(db)?;
    db.execute_batch(PROVIDER_ACCOUNTS)?;
    db.execute_batch(include_str!("provider-indexes.sql"))?;
    Ok(())
}

pub fn provider_tables(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS provider_calls(seq INTEGER PRIMARY KEY AUTOINCREMENT,turn TEXT NOT NULL REFERENCES turns(id),agent TEXT NOT NULL REFERENCES agents(id),model TEXT NOT NULL,origin TEXT NOT NULL,purpose TEXT NOT NULL CHECK(purpose IN('routing','completion')),status TEXT NOT NULL CHECK(status IN('pending','received','failed','interrupted')),created TEXT NOT NULL,finished TEXT,input_tokens INTEGER,output_tokens INTEGER,total_tokens INTEGER) STRICT;")?;
    for (name, definition) in [
        ("phase", "TEXT NOT NULL DEFAULT 'answer'"),
        ("family", "TEXT NOT NULL DEFAULT 'openai-compatible'"),
        ("account", "TEXT NOT NULL DEFAULT ''"),
    ] {
        column(db, "provider_calls", name, definition)?;
    }
    db.execute_batch("CREATE TABLE IF NOT EXISTS provider_streams(call INTEGER PRIMARY KEY REFERENCES provider_calls(seq),text TEXT NOT NULL) STRICT; CREATE TABLE IF NOT EXISTS provider_stream_segments(call INTEGER NOT NULL REFERENCES provider_calls(seq),ordinal INTEGER NOT NULL,id TEXT NOT NULL,kind TEXT NOT NULL,text TEXT NOT NULL,state TEXT NOT NULL,final INTEGER NOT NULL DEFAULT 0,revision INTEGER NOT NULL,PRIMARY KEY(call,ordinal),UNIQUE(call,id)) STRICT;")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migrations_upgrade_and_refuse_drift_atomically() -> Result<()> {
        for version in 1..SCHEMA_VERSION {
            let db = Connection::open_in_memory()?;
            migrate(&db, 123, version)?;
            migrate(&db, 124, SCHEMA_VERSION)?;
            migrate(&db, 125, SCHEMA_VERSION)?;
            assert_eq!(
                db.query_row("SELECT max(version) FROM schema_migrations", [], |row| row
                    .get::<_, i64>(
                    0
                ))?,
                SCHEMA_VERSION
            );
            db.execute(
                "UPDATE schema_migrations SET checksum='bad' WHERE version=1",
                [],
            )?;
            assert!(migrate(&db, 126, SCHEMA_VERSION).is_err());
            assert!(db.is_autocommit());
        }
        let db = Connection::open_in_memory()?;
        migrate(&db, 1, SCHEMA_VERSION)?;
        db.execute(
            "INSERT INTO schema_migrations VALUES(?,'future','future',1)",
            [SCHEMA_VERSION + 1],
        )?;
        assert!(migrate(&db, 2, SCHEMA_VERSION).is_err());
        assert!(db.is_autocommit());
        Ok(())
    }

    #[test]
    fn rejects_unmarked_data_and_accepts_fresh_data() -> Result<()> {
        let directory =
            std::env::temp_dir().join(format!("minarbot-format-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory)?;
        let old = directory.join("old.sqlite");
        Connection::open(&old)?.execute_batch("CREATE TABLE old_data(value TEXT)")?;
        let before = fs::read(&old)?;
        let error = open(&old, true, 0)
            .err()
            .ok_or("Unmarked database must be rejected")?;
        assert_eq!(error.to_string(), data_format::UNMARKED);
        assert_eq!(fs::read(&old)?, before);
        for canonical in [false, true] {
            let fresh = directory.join(format!("fresh-{canonical}.sqlite"));
            drop(open(&fresh, canonical, 0)?);
            drop(open(&fresh, canonical, 0)?);
        }
        fs::remove_dir_all(directory)?;
        Ok(())
    }
}
