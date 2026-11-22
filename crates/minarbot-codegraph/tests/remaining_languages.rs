#![cfg(feature = "all-languages")]

use minarbot_codegraph::Index;
use rusqlite::Connection;
use std::{error::Error, fs};

#[test]
fn indexes_every_kernel_language_and_resolves_calls() -> Result<(), Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    let root = scratch.path().join("source");
    fs::create_dir(&root)?;
    let cases = [
        ("App.java", "java", "package demo;\nclass App { void helper() {} void entry() { helper(); } }\n"),
        ("demo.c", "c", "static __init void helper(void) {}\nvoid entry(void) { helper(); }\n"),
        ("demo.h", "cpp", "class DEMO_API Program {\npublic:\n  void helper() {}\n  void entry() { helper(); }\n};\n"),
        ("App.cs", "csharp", "class App {\n#if BUILD // café\nvoid helper() {}\n#endif\nvoid entry() { helper(); }\n}\n"),
        ("demo.rb", "ruby", "def helper\nend\ndef entry\n  helper()\nend\n"),
        ("demo.php", "php", "<p>Hello</p>\n<?php\nfunction helper() {}\nfunction entry() { helper(); }\n"),
        ("demo.swift", "swift", "class Demo {\n  typealias Both = First\n    & Second\n  func helper() {}\n  func entry() { helper() }\n}\n"),
        ("demo.kt", "kotlin", "fun helper() {}\nfun entry() {\n  helper()\n  val numbers = 1..<4\n}\n"),
        ("demo.R", "r", "helper <- function() 1\nentry <- function() helper()\n"),
        ("demo.lua", "lua", "local function helper() end\nlocal function entry() helper() end\n"),
        ("demo.luau", "luau", "local function helper(): number return 1 end\nlocal function entry(): number return helper() end\n"),
        ("demo.scala", "scala", "object Demo {\n  def helper(): Int = 1\n  def entry(): Int = helper()\n}\n"),
        ("demo.dart", "dart", "int helper() => 1;\nint entry() => helper();\n"),
    ];
    for (path, _, source) in cases {
        fs::write(root.join(path), source)?;
    }
    fs::write(
        root.join("excluded.vb"),
        "Public Class Ignored\nEnd Class\n",
    )?;
    let database = scratch.path().join("graph.db");
    let mut index = Index::open(root, &database)?;
    let stats = index.sync()?;
    assert_eq!(stats.added, cases.len());
    assert!(stats.issues.is_empty(), "{:?}", stats.issues);
    assert_eq!(index.sync()?.unchanged, cases.len());
    let db = Connection::open(database)?;
    for (path, lang, _) in cases {
        let got: String =
            db.query_row("SELECT language FROM files WHERE path=?", [path], |row| {
                row.get(0)
            })?;
        assert_eq!(got, lang, "{path}");
        let helpers: usize = db.query_row(
            "SELECT COUNT(*) FROM nodes WHERE file_path=? AND name='helper'",
            [path],
            |row| row.get(0),
        )?;
        assert_eq!(helpers, 1, "{path}");
        let calls: usize = db.query_row(
            "SELECT COUNT(*) FROM edges e JOIN nodes a ON a.id=e.source JOIN nodes b ON b.id=e.target
             WHERE a.file_path=?1 AND b.file_path=?1 AND a.name='entry' AND b.name='helper' AND e.kind='calls'",
            [path], |row| row.get(0)
        )?;
        assert!(calls > 0, "{path} must link entry to its helper");
    }
    Ok(())
}

#[test]
fn resolves_local_includes_and_jvm_imports() -> Result<(), Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    let root = scratch.path().join("source");
    fs::create_dir(&root)?;
    for (name, source) in [
        ("lib.h", "#ifdef __cplusplus\nextern \"C\" {\n#endif\nvoid greet(void);\n#ifdef __cplusplus\n}\n#endif\n"),
        ("entry.c", "#include \"lib.h\"\nvoid entry(void) { greet(); }\n"),
        ("Core.kt", "package api\nfun greet() {}\n"),
        ("Consumer.kt", "package client\nimport api.greet as hello\nfun invoke() { hello() }\n"),
    ] {
        fs::write(root.join(name), source)?;
    }
    let database = scratch.path().join("graph.db");
    Index::open(root, &database)?.sync()?;
    let db = Connection::open(database)?;
    for (from, to, kind) in [
        ("entry.c", "lib.h", "imports"),
        ("Consumer.kt", "Core.kt", "imports"),
        ("Consumer.kt", "Core.kt", "calls"),
    ] {
        let found: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM edges e JOIN nodes a ON a.id=e.source JOIN nodes b ON b.id=e.target
              WHERE a.file_path=? AND b.file_path=? AND e.kind=?)",
            [from,to,kind], |row| row.get(0),
        )?;
        assert!(found, "{from} {kind} {to}");
    }
    Ok(())
}
