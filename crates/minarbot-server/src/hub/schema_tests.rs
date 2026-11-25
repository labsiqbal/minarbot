//! Port of `app/schema.test.ts` "fresh Rust hub startup preserves the 85373b3
//! schema and applies extensions".

use super::*;
use std::{fs, os::unix::fs::DirBuilderExt};

type Check<T = ()> = crate::db::Result<T>;

const FROZEN: &str = include_str!("testdata/hub-pre-collapse.sql");

struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One schema object: type, name, table and whitespace-normalized SQL.
type SchemaObject = (String, String, String, Option<String>);

/// Schema objects with whitespace-normalized SQL, ordered by type and name.
fn snapshot(db: &Connection) -> Check<Vec<SchemaObject>> {
    let mut statement =
        db.prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name")?;
    let rows = statement.query_map([], |row| {
        let sql: Option<String> = row.get(3)?;
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            sql.map(|sql| sql.split_whitespace().collect::<Vec<_>>().join(" ")),
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn migrations(db: &Connection) -> Check<Vec<Vec<String>>> {
    let mut statement = db.prepare("SELECT * FROM hub_migrations ORDER BY 1")?;
    let columns = statement.column_count();
    let rows = statement.query_map([], |row| {
        (0..columns)
            .map(|index| row.get_ref(index).map(|value| format!("{value:?}")))
            .collect::<rusqlite::Result<Vec<_>>>()
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[test]
fn fresh_rust_hub_startup_preserves_the_85373b3_schema_and_applies_extensions() -> Check {
    let dir = std::env::temp_dir().join(format!("minarbot-hub-schema-{}", new_id()));
    fs::DirBuilder::new().mode(0o700).create(&dir)?;
    let temp = Temp(dir);
    let path = temp.0.join("hub.sqlite");
    {
        let (_db, _writer) = storage::open(&path)?;
    }
    let actual = Connection::open(&path)?;
    let expected = Connection::open_in_memory()?;
    expected.execute_batch(FROZEN)?;
    expected.execute_batch(include_str!("extensions.sql"))?;
    let (left, right) = (snapshot(&actual)?, snapshot(&expected)?);
    if left != right {
        let extra: Vec<_> = left.iter().filter(|row| !right.contains(row)).collect();
        let missing: Vec<_> = right.iter().filter(|row| !left.contains(row)).collect();
        return Err(format!("hub schema: unexpected {extra:?}; missing {missing:?}").into());
    }
    let (left, right) = (migrations(&actual)?, migrations(&expected)?);
    if left != right {
        return Err(format!("hub migrations: {left:?} != {right:?}").into());
    }
    Ok(())
}
