use minarbot_launcher::data_format;
use rusqlite::{
    functions::FunctionFlags, Connection, OptionalExtension, Transaction, TransactionBehavior,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

// Keep released ledger rows readable by existing servers and backups.
const BASELINE_LEDGER: &[(&str, &str)] = &[
    (
        "v1_owner_project",
        "61c06b2162d013123bf8901c1f4e72c33deb6cf60b7018a8c6ac65aa0b2e417b",
    ),
    (
        "v2_device_authority",
        "8be7de84c6c87fb68b0afedf22bfa59ff8e88c08aeda262eded348216e86dc7f",
    ),
    (
        "v3_task_fencing",
        "f933c4c0663fa9680a4b6227e578052b31afa9c8d377bd753e1e8f0ffd0c5b1a",
    ),
    (
        "v4_sync_retention",
        "9c7b5684504ef6d121413585b4b78322e9149188af0ebb1d7e7e4a354cae0a7e",
    ),
    (
        "v5_canonical_indexes",
        "57a220046ffb095a17a6c987070f59f518b72d80a8f605d5e8c309fdd2dd7b52",
    ),
    (
        "v6_provider_accounts",
        "e5228a7b235e9cf8176187f1313b106c1b7ae704713e4ca2ff90e4e0c99dec6f",
    ),
];
pub(crate) const BASELINE_VERSION: i64 = BASELINE_LEDGER.len() as i64;
const PROVIDER_INSTANCE_MIGRATION: &str = concat!(
    include_str!("data/provider-instances.sql"),
    "\nALTER TABLE agents ADD COLUMN provider_instance TEXT NOT NULL DEFAULT '';\n",
    "ALTER TABLE agents ADD COLUMN model_options TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(model_options));\n",
);
const CANONICAL_MIGRATIONS: &[(&str, &str)] =
    &[("v7_provider_instances", PROVIDER_INSTANCE_MIGRATION)];
pub const SCHEMA_VERSION: i64 = BASELINE_VERSION + CANONICAL_MIGRATIONS.len() as i64;
const LOCAL_MIGRATIONS: &[&str] = &[PROVIDER_INSTANCE_MIGRATION];

const PROVIDER_ACCOUNTS: &str = include_str!("data/provider-accounts.sql");
const PROVIDER_INSTANCES: &str = include_str!("data/provider-instances.sql");
const CANONICAL_SCHEMA: &str = concat!(
    include_str!("data/canonical-current.sql"),
    include_str!("data/protected-waits.sql"),
    include_str!("data/task-successors.sql"),
    include_str!("data/sync-scopes.sql"),
    include_str!("turns/agent-visibility.sql"),
    include_str!("turns/agent-messages.sql"),
    include_str!("computer/remote-agents.sql"),
    include_str!("turns/agent-delegation.sql"),
    include_str!("turns/rooms.sql"),
);
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
    if table(&connection, "hub_migrations")? {
        return Err("Hosted hub database requires --hub mode.".into());
    }
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
pub(crate) fn empty_schema(db: &Connection) -> Result<bool> {
    Ok(!db.prepare("SELECT 1 FROM sqlite_schema")?.exists([])?)
}

pub(crate) fn migrate_user_version(
    db: &Transaction<'_>,
    baseline: i64,
    steps: &[&str],
    unsupported: &str,
) -> Result<()> {
    let version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if !(baseline..=baseline + steps.len() as i64).contains(&version) {
        return Err(unsupported.into());
    }
    for (index, sql) in steps.iter().enumerate().skip((version - baseline) as usize) {
        db.execute_batch(sql)?;
        db.pragma_update(None, "user_version", baseline + index as i64 + 1)?;
    }
    Ok(())
}

fn create_canonical(db: &Connection, now: i64) -> Result<()> {
    db.execute_batch(CANONICAL_SCHEMA)?;
    provider_tables(db)?;
    db.execute_batch(PROVIDER_ACCOUNTS)?;
    db.execute_batch(PROVIDER_INSTANCES)?;
    crate::turns::initialize_canonical(db).map_err(|error| error.message)?;
    crate::files::init_canonical_schema(db)?;
    crate::automation::project::initialize(db).map_err(|error| error.message)?;
    db.execute_batch(CANONICAL_INDEXES)?;
    db.execute(
        "INSERT INTO authority_state VALUES (1,?,'active',0,NULL,NULL,1)",
        [uuid::Uuid::new_v4().to_string()],
    )?;
    for (index, (name, checksum)) in BASELINE_LEDGER.iter().enumerate() {
        db.execute(
            "INSERT INTO schema_migrations VALUES (?,?,?,?)",
            rusqlite::params![index as i64 + 1, name, checksum, now],
        )?;
    }
    Ok(())
}

pub fn migrate(db: &Connection, now: i64, through: i64) -> Result<()> {
    migrate_canonical(db, now, through, CANONICAL_MIGRATIONS)
}

fn migrate_canonical(
    db: &Connection,
    now: i64,
    through: i64,
    steps: &[(&str, &str)],
) -> Result<()> {
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
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate)?;
    let version = if empty_schema(&tx)? {
        create_canonical(&tx, now)?;
        BASELINE_VERSION
    } else {
        if !table(&tx, "schema_migrations")? {
            return Err("Database does not have a canonical schema".into());
        }
        tx.query_row(
            "SELECT coalesce(max(version),0) FROM schema_migrations",
            [],
            |row| row.get::<_, i64>(0),
        )?
    };
    if version < BASELINE_VERSION {
        return Err("Unsupported canonical database version".into());
    }
    let latest = BASELINE_VERSION + steps.len() as i64;
    if version > latest || through > latest {
        return Err("Database schema is newer than this server".into());
    }
    if version > through {
        return Err("Schema downgrade is not supported".into());
    }
    let validate = |version, name: &str, checksum: &str| -> Result<()> {
        let valid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=? AND name=? AND checksum=?)",
            rusqlite::params![version, name, checksum],
            |row| row.get(0),
        )?;
        if !valid {
            return Err("Schema migration checksum mismatch".into());
        }
        Ok(())
    };
    for (index, (name, checksum)) in BASELINE_LEDGER.iter().enumerate() {
        validate(index as i64 + 1, name, checksum)?;
    }
    for (index, (name, sql)) in steps.iter().enumerate() {
        let next = BASELINE_VERSION + index as i64 + 1;
        if next > through {
            break;
        }
        let checksum = digest(sql);
        if next <= version {
            validate(next, name, &checksum)?;
        } else {
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO schema_migrations VALUES (?,?,?,?)",
                rusqlite::params![next, name, checksum, now],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn local(db: &Connection) -> Result<()> {
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate)?;
    tx.execute_batch(include_str!("data/local-current.sql"))?;
    provider_tables(&tx)?;
    tx.execute_batch(PROVIDER_ACCOUNTS)?;
    tx.execute_batch(PROVIDER_INSTANCES)?;
    tx.execute_batch(include_str!("provider-indexes.sql"))?;
    migrate_user_version(
        &tx,
        0,
        LOCAL_MIGRATIONS,
        "Unsupported local database version",
    )?;
    tx.commit()?;
    Ok(())
}

pub fn provider_tables(db: &Connection) -> Result<()> {
    db.execute_batch(include_str!("data/provider-calls.sql"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_pending_canonical_migrations_preserve_ledger_and_schema() -> Result<()> {
        let db = Connection::open_in_memory()?;
        migrate(&db, 1, SCHEMA_VERSION)?;
        let changes: i64 = db.query_row("SELECT total_changes()", [], |row| row.get(0))?;
        let cookie: i64 = db.pragma_query_value(None, "schema_version", |row| row.get(0))?;
        migrate(&db, 2, SCHEMA_VERSION)?;
        assert_eq!(
            db.query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))?,
            changes
        );
        assert_eq!(
            db.pragma_query_value(None, "schema_version", |row| row.get::<_, i64>(0))?,
            cookie
        );
        Ok(())
    }

    #[test]
    fn numbered_steps_run_once_and_validate_their_checksum() -> Result<()> {
        let db = Connection::open_in_memory()?;
        migrate_canonical(&db, 1, BASELINE_VERSION, &[])?;
        let next = BASELINE_VERSION + 1;
        let sql = "ALTER TABLE agents ADD COLUMN fixture_note TEXT";
        let steps = [("v7_fixture", sql)];
        let two_steps = [steps[0], ("v8_fixture", "INVALID SQL")];
        assert!(migrate_canonical(&db, 2, next + 1, &two_steps).is_err());

        migrate_canonical(&db, 3, next, &steps)?;
        migrate_canonical(&db, 4, next, &steps)?;
        let saved = db.query_row(
            "SELECT name, checksum, applied_at FROM schema_migrations WHERE version=?",
            [next],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?;
        assert_eq!(saved, (steps[0].0.to_owned(), digest(sql), 3));
        let edited = [(
            steps[0].0,
            "ALTER TABLE agents ADD COLUMN fixture_note INTEGER",
        )];
        assert!(migrate_canonical(&db, 5, next, &edited)
            .is_err_and(|error| error.to_string() == "Schema migration checksum mismatch"));
        assert!(db.is_autocommit());
        Ok(())
    }

    #[test]
    fn rejects_unknown_canonical_versions_and_checksum_drift() -> Result<()> {
        let db = Connection::open_in_memory()?;
        migrate(&db, 1, SCHEMA_VERSION)?;
        db.execute(
            "INSERT INTO schema_migrations VALUES(?,'future','future',1)",
            [SCHEMA_VERSION + 1],
        )?;
        let error = migrate(&db, 2, SCHEMA_VERSION)
            .err()
            .ok_or("Unknown migration must be refused")?;
        assert_eq!(
            error.to_string(),
            "Database schema is newer than this server"
        );
        assert!(db.is_autocommit());
        db.execute(
            "DELETE FROM schema_migrations WHERE version>?",
            [SCHEMA_VERSION],
        )?;
        db.execute(
            "UPDATE schema_migrations SET checksum='bad' WHERE version=?",
            [BASELINE_VERSION],
        )?;
        assert!(migrate(&db, 2, SCHEMA_VERSION)
            .is_err_and(|error| error.to_string() == "Schema migration checksum mismatch"));
        assert!(db.is_autocommit());
        db.execute(
            "DELETE FROM schema_migrations WHERE version>=?",
            [BASELINE_VERSION],
        )?;
        assert!(migrate(&db, 2, SCHEMA_VERSION)
            .is_err_and(|error| error.to_string() == "Unsupported canonical database version"));
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
