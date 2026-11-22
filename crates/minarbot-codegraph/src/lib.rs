//! Minarbot CodeGraph stores per-project source extractions in SQLite.
//!
//! Stack guard adapted from colbymchenry/codegraph v1.6.2 (6560052),
//! codegraph-kernel/src/lib.rs. License: MIT (Copyright (c) 2026 Colby Mchenry).

macro_rules! stack_guard {
    () => {
        if crate::stack::exhausted() {
            return ::core::default::Default::default();
        }
    };
}

mod buffers;
mod docstring;
mod generated;
mod go;
mod ids;
mod langs;
mod python;
mod query;
mod resolution;
mod rustlang;
mod scan;
mod stack;
mod store;
mod textutil;
mod tsjs;

use rayon::prelude::*;
use rusqlite::{params, Connection, OpenFlags, TransactionBehavior};
use std::path::{Path, PathBuf};

const SCHEMA: &str = include_str!("../schema.sql");

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path:?}: {source}")]
    Source {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("source walk: {0}")]
    Walk(#[from] ignore::Error),
    #[error("SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

type Result<T> = std::result::Result<T, Error>;

fn literal_regex(pattern: &str) -> regex::Regex {
    match regex::Regex::new(pattern) {
        Ok(regex) => regex,
        Err(error) => panic!("invalid static pattern {pattern:?}: {error}"),
    }
}

/// One project's graph. The caller chooses the location of its SQLite database.
pub struct Index {
    root: PathBuf,
    db: Connection,
}

pub use query::{mcp_tools, ExploreSession, QueryError, QueryIndex};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncStats {
    pub added: usize,
    pub updated: usize,
    pub deleted: usize,
    pub unchanged: usize,
    pub issues: Vec<FileIssue>,
}

/// A source file that was recorded without symbols.
#[derive(Debug, PartialEq, Eq)]
pub struct FileIssue {
    pub path: String,
    pub code: &'static str,
    pub message: String,
}

impl Index {
    /// Open or initialize a database. Its parent directory must already exist.
    pub fn open(root: impl AsRef<Path>, database: impl AsRef<Path>) -> Result<Self> {
        let root = std::fs::canonicalize(root.as_ref()).map_err(|source| Error::Source {
            path: root.as_ref().to_path_buf(),
            source,
        })?;
        if !root.is_dir() {
            return Err(Error::Source {
                path: root,
                source: std::io::Error::other("project root is not a directory"),
            });
        }
        let mut db = Connection::open(database)?;
        db.busy_timeout(std::time::Duration::from_secs(30))?;
        db.pragma_update(None, "foreign_keys", true)?;

        let transaction = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'schema_versions')",
            [],
            |row| row.get::<_, bool>(0),
        )? {
            transaction.execute_batch(SCHEMA)?;
        }
        transaction.commit()?;
        Ok(Self { root, db })
    }

    /// Open an existing Minarbot or CodeGraph v1.6.2 database for queries only.
    pub fn open_read_only(root: impl AsRef<Path>, database: impl AsRef<Path>) -> Result<Self> {
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(|source| Error::Source {
                path: root.as_ref().to_owned(),
                source,
            })?;
        let db = Connection::open_with_flags(
            database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.pragma_update(None, "trusted_schema", false)?;
        Ok(Self { root, db })
    }

    /// Hash visible sources, extract changed files in parallel, and commit the
    /// new graph as one transaction. I/O or database failures leave the previous
    /// graph intact. Parse failures are stored in `files.errors`.
    pub fn sync(&mut self) -> Result<SyncStats> {
        // Acquire the writer before reading the previous snapshot. Two handles
        // opening the same database cannot reconcile against a stale snapshot.
        let transaction = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut previous = store::file_hashes(&transaction)?;
        let mut stats = SyncStats::default();
        let mut changes = resolution::Changes::default();
        let now = store::timestamp(std::time::SystemTime::now());
        let paths = scan::discover(&self.root)?;
        // ponytail: bounded batches keep extraction memory independent of repo
        // size. Use a bounded queue if overlapping parse and writes is needed.
        for batch in paths.chunks(32) {
            let results: Result<Vec<_>> = batch
                .par_iter()
                .map(|file| scan::extract(&self.root, file, previous.get(&file.path)))
                .collect();
            for (file, result) in batch.iter().zip(results?) {
                match result {
                    scan::Outcome::Missing => continue,
                    scan::Outcome::Unchanged => stats.unchanged += 1,
                    scan::Outcome::Changed(source) => {
                        let added = !previous.contains_key(&file.path);
                        if !added {
                            changes.removing(&transaction, &file.path)?;
                            store::delete_file(&transaction, &file.path)?;
                            stats.updated += 1;
                        } else {
                            stats.added += 1;
                        }
                        store::insert_file(&transaction, file, &source, now)?;
                        changes.inserted(&source, added);
                        stats.issues.extend(source.issue);
                    }
                }
                previous.remove(&file.path);
            }
        }
        stats.deleted = previous.len();
        for path in previous.keys() {
            changes.removing(&transaction, path)?;
            store::delete_file(&transaction, path)?;
        }
        resolution::resolve(&transaction, &self.root, &changes)?;
        transaction.execute(
            "INSERT INTO project_metadata (key,value,updated_at) VALUES ('minarbot_last_sync',?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
            params![serde_json::json!({"added":stats.added,"modified":stats.updated,"removed":stats.deleted}).to_string(), now],
        )?;
        transaction.commit()?;
        Ok(stats)
    }

    /// Read-only query view over this index.
    pub fn query(&self) -> QueryIndex<'_> {
        QueryIndex::new(&self.root, &self.db)
    }
}
