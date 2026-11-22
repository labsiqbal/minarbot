# Minarbot CodeGraph

Phase 1 provides a SQLite source index. Call `Index::open(project_root, database_path)`
and `Index::sync()`. Repeat sync to hash visible sources, replace changed
extractions, and remove deleted files. A file with a parser error retains a
`files.errors` row so the caller can report it and retry after an edit.

Git supplies tracked paths and unignored worktree files. Outside Git, the
walker applies local `.gitignore` rules. Both paths exclude dependency,
build, and index directories by default. Generated files remain searchable
and carry their classification in `files.generated`.

Rust, TypeScript/TSX, JavaScript/JSX, Python, and Go are enabled by default.
Disable default features and select `rust`, `typescript`, `javascript`,
`python`, and/or `go` to build a smaller indexer. Sync reconciles the
database to the enabled languages.

The schema includes extraction edges, unresolved references, and FTS5.
Resolution, framework synthesis, and Minarbot integration land in later
phases. Use a dedicated database path for the extraction index.

- [Parity and binary size](PARITY.md)
- [Third-party notices](third-party/NOTICE.md)
