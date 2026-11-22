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
mod rustlang;
mod scan;
mod stack;
mod store;
mod textutil;
mod tsjs;

use rayon::prelude::*;
use rusqlite::{Connection, TransactionBehavior};
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
                        if previous.contains_key(&file.path) {
                            store::delete_file(&transaction, &file.path)?;
                            stats.updated += 1;
                        } else {
                            stats.added += 1;
                        }
                        store::insert_file(&transaction, file, &source, now)?;
                        stats.issues.extend(source.issue);
                    }
                }
                previous.remove(&file.path);
            }
        }
        stats.deleted = previous.len();
        for path in previous.keys() {
            store::delete_file(&transaction, path)?;
        }
        transaction.commit()?;
        Ok(stats)
    }
}
