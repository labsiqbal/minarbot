//! SQLite access for tests.
//!
//! [`Inspector`] is read-only and is the default way to assert stored state.
//! [`Fixture`] is writable and exists only for seeding rows the API cannot
//! create and for fault injection (for example a trigger that makes one
//! insert fail); prefer the HTTP API whenever it can set the state.

use crate::Result;
pub use rusqlite::Params;
use rusqlite::{types::ValueRef, Connection, OpenFlags};
use serde_json::{json, Map, Value};
use std::{path::Path, time::Duration};

fn to_json(value: ValueRef<'_>) -> Value {
    use base64::Engine;
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(value) => json!(value),
        ValueRef::Real(value) => json!(value),
        ValueRef::Text(value) => json!(String::from_utf8_lossy(value)),
        ValueRef::Blob(value) => json!(base64::engine::general_purpose::STANDARD.encode(value)),
    }
}

fn rows(connection: &Connection, sql: &str, params: impl Params) -> Result<Vec<Value>> {
    let mut statement = connection.prepare(sql)?;
    let names: Vec<String> = statement
        .column_names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let mut rows = statement.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let mut object = Map::new();
        for (index, name) in names.iter().enumerate() {
            object.insert(name.clone(), to_json(row.get_ref(index)?));
        }
        out.push(Value::Object(object));
    }
    Ok(out)
}

/// Read-only view of a server database. Safe while the server runs.
pub struct Inspector {
    connection: Connection,
}

impl Inspector {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        Ok(Self { connection })
    }

    /// Every row as a JSON object keyed by column name (blobs as base64).
    pub fn rows(&self, sql: &str, params: impl Params) -> Result<Vec<Value>> {
        rows(&self.connection, sql, params)
    }

    /// The single row `sql` returns.
    pub fn row(&self, sql: &str, params: impl Params) -> Result<Value> {
        let mut rows = self.rows(sql, params)?;
        match rows.len() {
            1 => Ok(rows.remove(0)),
            count => Err(format!("expected one row from `{sql}`, got {count}").into()),
        }
    }

    /// First column of the single row `sql` returns.
    pub fn scalar<T: rusqlite::types::FromSql>(&self, sql: &str, params: impl Params) -> Result<T> {
        Ok(self.connection.query_row(sql, params, |row| row.get(0))?)
    }

    /// `SELECT count(*) FROM table`.
    pub fn count(&self, table: &str) -> Result<i64> {
        let quoted = table.replace('"', "\"\"");
        self.scalar(&format!("SELECT count(*) FROM \"{quoted}\""), [])
    }

    /// Raw bytes of a blob column.
    pub fn blob(&self, sql: &str, params: impl Params) -> Result<Vec<u8>> {
        Ok(self.connection.query_row(sql, params, |row| row.get(0))?)
    }
}

/// Writable connection for seeding and fault injection. Stop the server
/// first ([`crate::Server::edit_database`]) unless the test deliberately
/// changes state under a running server (canonical fixtures do).
pub struct Fixture {
    connection: Connection,
}

impl Fixture {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        Ok(Self { connection })
    }

    pub fn execute(&self, sql: &str, params: impl Params) -> rusqlite::Result<usize> {
        self.connection.execute(sql, params)
    }

    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        Ok(self.connection.execute_batch(sql)?)
    }

    pub fn rows(&self, sql: &str, params: impl Params) -> Result<Vec<Value>> {
        rows(&self.connection, sql, params)
    }

    pub fn row(&self, sql: &str, params: impl Params) -> Result<Value> {
        let mut rows = self.rows(sql, params)?;
        match rows.len() {
            1 => Ok(rows.remove(0)),
            count => Err(format!("expected one row from `{sql}`, got {count}").into()),
        }
    }

    pub fn scalar<T: rusqlite::types::FromSql>(&self, sql: &str, params: impl Params) -> Result<T> {
        Ok(self.connection.query_row(sql, params, |row| row.get(0))?)
    }

    /// Raw bytes of a blob column.
    pub fn blob(&self, sql: &str, params: impl Params) -> Result<Vec<u8>> {
        Ok(self.connection.query_row(sql, params, |row| row.get(0))?)
    }

    /// The underlying connection, for registering SQL functions.
    pub fn connection(&self) -> &Connection {
        &self.connection
    }
}
