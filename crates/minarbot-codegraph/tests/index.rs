#![cfg(feature = "rust")]

use minarbot_codegraph::Index;
use rusqlite::Connection;
use std::{error::Error, fs, io, path::Path, process::Command};

type Test = Result<(), Box<dyn Error>>;

fn git(root: &Path, args: &[&str]) -> Test {
    let result = Command::new("git")
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .current_dir(root)
        .output()?;
    if !result.status.success() {
        return Err(io::Error::other(String::from_utf8_lossy(&result.stderr)).into());
    }
    Ok(())
}

#[test]
fn sync_tracks_hashes_and_replaces_graph_atomically() -> Test {
    let scratch = tempfile::tempdir()?;
    let root = scratch.path().join("source");
    fs::create_dir(&root)?;
    fs::create_dir(root.join("src"))?;
    git(&root, &["init", "--quiet"])?;
    fs::write(root.join("src/.gitignore"), "ignored*.rs\n")?;
    fs::write(root.join(".git/info/exclude"), "/local.rs\n")?;
    fs::write(root.join("src/ignored-tracked.rs"), "fn tracked() {}\n")?;
    git(&root, &["add", "-f", "src/ignored-tracked.rs"])?;
    fs::write(root.join("src/ignored-untracked.rs"), "fn hidden() {}\n")?;
    fs::write(root.join("local.rs"), "fn excluded() {}\n")?;
    fs::write(
        root.join("src/lib.rs"),
        "pub fn FindValue() { missing(); }\n",
    )?;
    fs::write(root.join("src/output.generated.rs"), "fn output() {}\n")?;
    let database = scratch.path().join("graph.db");
    let mut index = Index::open(&root, &database)?;
    assert_eq!(index.sync()?.added, 3);
    assert_eq!(index.sync()?.unchanged, 3);

    let db = Connection::open(&database)?;
    let files: String = db.query_row(
        "SELECT group_concat(path, '|') FROM (SELECT path FROM files ORDER BY path)",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        files,
        "src/ignored-tracked.rs|src/lib.rs|src/output.generated.rs"
    );
    let result: (bool, String, String) = db.query_row(
        "SELECT files.generated, unresolved_refs.reference_name, unresolved_refs.status
         FROM files, unresolved_refs
         WHERE files.path = 'src/output.generated.rs' AND unresolved_refs.reference_kind = 'calls'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(result, (true, "missing".to_owned(), "pending".to_owned()));
    let vocabulary: String = db.query_row(
        "SELECT group_concat(segment, ' ') FROM
         (SELECT segment FROM name_segment_vocab WHERE name = 'FindValue' ORDER BY segment)",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(vocabulary, "find value");

    db.execute_batch(
        "CREATE TRIGGER reject_replacement BEFORE INSERT ON files
         WHEN NEW.path = 'src/lib.rs' BEGIN SELECT RAISE(ABORT, 'blocked'); END;",
    )?;
    fs::write(
        root.join("src/lib.rs"),
        "pub fn ReadValue() { another(); }\n",
    )?;
    fs::remove_file(root.join("src/output.generated.rs"))?;
    assert!(index.sync().is_err());
    assert_eq!(fts_count(&db, "FindValue")?, 1);
    assert_eq!(fts_count(&db, "output")?, 2);

    db.execute_batch("DROP TRIGGER reject_replacement")?;
    drop(index);
    let mut index = Index::open(&root, database)?;
    let changes = index.sync()?;
    assert_eq!(
        (changes.updated, changes.deleted, changes.unchanged),
        (1, 1, 1)
    );
    assert_eq!(fts_count(&db, "FindValue")?, 0);
    assert_eq!(fts_count(&db, "ReadValue")?, 1);
    assert_eq!(fts_count(&db, "output")?, 0);
    let stale: i64 = db.query_row(
        "SELECT count(*) FROM unresolved_refs WHERE reference_name = 'missing'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(stale, 0);
    let integrity: i64 =
        db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    assert_eq!(integrity, 0);
    Ok(())
}

fn fts_count(db: &Connection, query: &str) -> rusqlite::Result<i64> {
    db.query_row(
        "SELECT count(*) FROM nodes_fts WHERE nodes_fts MATCH ?",
        [query],
        |row| row.get(0),
    )
}

#[test]
fn parse_errors_stay_visible_and_recover_on_edit() -> Test {
    let scratch = tempfile::tempdir()?;
    let root = scratch.path().join("source");
    fs::create_dir(&root)?;
    fs::write(root.join(".gitignore"), "skip.rs\n")?;
    fs::write(root.join("skip.rs"), "fn excluded() {}")?;
    fs::write(root.join("main.rs"), "fn incomplete(")?;
    let database = scratch.path().join("graph.db");
    let mut index = Index::open(&root, &database)?;
    let result = index.sync()?;
    assert_eq!(result.added, 1);
    assert_eq!(result.issues.len(), 1);
    assert_eq!(result.issues[0].code, "parse_error");
    let db = Connection::open(&database)?;
    let error: String = db.query_row(
        "SELECT json_extract(errors, '$[0].code') FROM files WHERE path = 'main.rs'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(error, "parse_error");
    assert_eq!(index.sync()?.unchanged, 1);

    fs::write(root.join("main.rs"), "fn complete() {}")?;
    let result = index.sync()?;
    assert_eq!(result.updated, 1);
    assert!(result.issues.is_empty());
    assert_eq!(fts_count(&db, "complete")?, 1);
    Ok(())
}

#[cfg(all(
    feature = "typescript",
    feature = "javascript",
    feature = "python",
    feature = "go"
))]
#[test]
fn indexes_every_enabled_dialect_and_generated_headers() -> Test {
    let scratch = tempfile::tempdir()?;
    let root = scratch.path().join("source");
    fs::create_dir(&root)?;
    for (path, source) in [
        ("native.rs", "fn native() {}"),
        ("typed.ts", "export function typed(): number { return 2; }"),
        (
            "typed-view.tsx",
            "export function TypedView() { return <div />; }",
        ),
        ("script.js", "export function script() { return 2; }"),
        (
            "script-view.jsx",
            "export function ScriptView() { return <div />; }",
        ),
        ("python.py", "def python():\n    return 2\n"),
        (
            "service.go",
            "// Code generated by tool. DO NOT EDIT.\npackage service\nfunc Service() {}\n",
        ),
    ] {
        fs::write(root.join(path), source)?;
    }
    let database = scratch.path().join("graph.db");
    let stats = Index::open(&root, &database)?.sync()?;
    assert_eq!(stats.added, 7);
    assert!(stats.issues.is_empty());
    let db = Connection::open(database)?;
    let dialects: i64 = db.query_row(
        "SELECT count(DISTINCT language) FROM nodes WHERE kind <> 'file'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(dialects, 7);
    let generated: bool = db.query_row(
        "SELECT generated FROM files WHERE path = 'service.go'",
        [],
        |row| row.get(0),
    )?;
    assert!(generated);
    Ok(())
}
