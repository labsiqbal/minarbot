use crate::db::{self, Result};
use rusqlite::{Connection, OptionalExtension};

const BASELINE_VERSION: i64 = 3;
const MIGRATIONS: &[&str] = &[
    // Append SQL steps in user_version order starting at v4.
];

pub(super) fn initialize(db: &mut Connection) -> Result<()> {
    if db.query_row(
        "SELECT 1 FROM sqlite_schema WHERE type='table' AND name IN ('agents','accounts','schema_migrations')",
        [],
        |_| Ok(()),
    ).optional()?.is_some() {
        return Err("Canonical database cannot be used as a device database".into());
    }
    if !minarbot_launcher::data_format::adopt(db)? {
        return Err(minarbot_launcher::data_format::UNMARKED.into());
    }
    db.execute_batch(
        "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000",
    )?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == 0 && db::empty_schema(&tx)? {
        tx.execute_batch(include_str!("current.sql"))?;
        tx.pragma_update(None, "user_version", BASELINE_VERSION)?;
    }
    db::migrate_user_version(
        &tx,
        BASELINE_VERSION,
        MIGRATIONS,
        "Unsupported device database version",
    )?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn columns(db: &Connection, table: &str) -> Result<Vec<String>> {
        let mut statement = db.prepare("SELECT name FROM pragma_table_info(?)")?;
        let rows = statement.query_map([table], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn snapshot(db: &Connection) -> Result<(Vec<SchemaEntry>, i64, i64)> {
        let mut stmt =
            db.prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name")?;
        let rows = stmt.query_map([], |row| {
            let sql: Option<String> = row.get(3)?;
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                sql.map(|sql| sql.split_whitespace().collect::<Vec<_>>().join(" ")),
            ))
        })?;
        Ok((
            rows.collect::<rusqlite::Result<_>>()?,
            db.pragma_query_value(None, "user_version", |row| row.get(0))?,
            db.pragma_query_value(None, "application_id", |row| row.get(0))?,
        ))
    }

    type SchemaEntry = (String, String, String, Option<String>);

    #[test]
    fn direct_device_schema_matches_the_pre_collapse_schema() -> Result<()> {
        let old = Connection::open_in_memory()?;
        old.execute_batch(include_str!("fixtures/pre-collapse.sql"))?;
        let mut db = Connection::open_in_memory()?;
        initialize(&mut db)?;
        let before = snapshot(&db)?;
        assert_eq!(before, snapshot(&old)?);
        db.execute(
            "INSERT INTO device_state VALUES (1,'host','authority',8,4,5,'[]')",
            [],
        )?;
        let cookie: i64 = db.pragma_query_value(None, "schema_version", |row| row.get(0))?;
        initialize(&mut db)?;
        assert_eq!(snapshot(&db)?, before);
        assert_eq!(
            db.pragma_query_value(None, "schema_version", |row| row.get::<_, i64>(0))?,
            cookie
        );
        assert_eq!(
            db.query_row("SELECT journal_sequence FROM device_state", [], |row| row
                .get::<_, i64>(
                0
            ))?,
            5
        );
        Ok(())
    }

    #[test]
    fn device_schema_stays_separate_and_idempotent() -> Result<()> {
        let mut db = Connection::open_in_memory()?;
        initialize(&mut db)?;
        initialize(&mut db)?;
        assert_eq!(
            db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))?,
            3
        );
        for table in [
            "device_state",
            "device_resource_bindings",
            "device_provider_bindings",
            "device_journal",
            "device_outbox",
            "device_wait_secrets",
        ] {
            assert!(!columns(&db, table)?.is_empty(), "{table}");
        }
        assert!(columns(&db, "projects")?.is_empty());
        Ok(())
    }

    #[test]
    fn marks_new_device_data_and_refuses_unmarked_data() -> Result<()> {
        let mut fresh = Connection::open_in_memory()?;
        initialize(&mut fresh)?;
        assert!(minarbot_launcher::data_format::is_marked(&fresh)?);
        initialize(&mut fresh)?;
        let mut old = Connection::open_in_memory()?;
        old.execute_batch("CREATE TABLE device_state(id INTEGER PRIMARY KEY)")?;
        old.pragma_update(None, "user_version", 2)?;
        let error = initialize(&mut old)
            .err()
            .ok_or("Unmarked device data must be refused")?;
        assert_eq!(error.to_string(), minarbot_launcher::data_format::UNMARKED);
        assert_eq!(
            old.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))?,
            2
        );
        assert!(!minarbot_launcher::data_format::is_marked(&old)?);
        Ok(())
    }

    #[test]
    fn rejects_canonical_and_unknown_versions() -> Result<()> {
        let mut db = Connection::open_in_memory()?;
        db.execute_batch("CREATE TABLE schema_migrations(version INTEGER)")?;
        assert!(
            initialize(&mut db).is_err_and(|err| err.to_string().contains("Canonical database"))
        );
        db.execute_batch("DROP TABLE schema_migrations; PRAGMA user_version=4")?;
        minarbot_launcher::data_format::mark(&db)?;
        assert!(
            initialize(&mut db).is_err_and(|err| err.to_string().contains("Unsupported device"))
        );
        Ok(())
    }
}
