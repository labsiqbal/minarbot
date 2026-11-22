# Minarbot CodeGraph

Call `Index::open(project_root, database_path)` and `Index::sync()` to build a
SQLite source graph. Sync hashes visible sources, replaces changed extractions,
removes deleted files, and resolves references in one transaction. A file with
a parser error retains a `files.errors` row so the caller can report it and
retry after an edit.

`Index::query()` exposes the read-only MCP query facade. Explore combines
FTS5, identifier segments, and graph traversal to return numbered source,
call paths, and blast radius within a per-file budget. Each MCP connection
deduplicates unchanged source spans it has already returned.

Minarbot indexes enabled project folders when attached and refreshes the
selected folder before a project turn. The toggle starts on for new projects.
The server mounts the workspace read-only in bubblewrap and keeps the index
under its data directory. Codex, Claude Code, ACP, and API turns share this
engine.

Git supplies tracked paths and unignored worktree files. Outside Git, the
walker applies local `.gitignore` rules. Both paths exclude dependency,
build, and index directories by default. Generated files remain searchable
and carry their classification in `files.generated`.

Rust, TypeScript/TSX, JavaScript/JSX, Python, and Go are enabled by default.
Disable default features and select `rust`, `typescript`, `javascript`,
`python`, and/or `go` to build a smaller indexer. Sync reconciles the
database to the enabled languages.

Resolution follows local import paths, named and default bindings, barrel
exports, JSONC `tsconfig` and `jsconfig` aliases, Go module paths, and Cargo
workspace crate names. The name matcher uses scope, receiver types and
inheritance edges before applying the ambiguity ceiling to heuristic matches.
Function-as-value references persist as `references` edges with `fnRef`
metadata. Dispatch heuristics record their provenance.

An unresolved reference stays in `unresolved_refs` with its status and name
tail. Changed definitions trigger a retry. When an indexed target disappears,
sync restores the original reference from the edge metadata and resolves it
again. Use a dedicated database path for the index.

- [Parity and binary size](PARITY.md)
- [Third-party notices](third-party/NOTICE.md)
