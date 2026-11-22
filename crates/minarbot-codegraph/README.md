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

Default builds enable Rust, TypeScript/TSX, JavaScript/JSX, Python, Go, Java,
and C. Additional Cargo features enable C++ (`cpp`), C# (`csharp`), Ruby,
PHP, Swift, Kotlin, R, Lua, Luau, Scala, and Dart. Select `all-languages`
for every native grammar. Use `--no-default-features` and individual
language features for a smaller standalone indexer. Sync reconciles the
database to the enabled languages.

The server builds with the default grammars. Build it with
`--features codegraph-all-languages` to enable all native grammars for every
runtime. C++ includes CUDA and Metal sources. VB.NET and WASM-only inputs
are outside the native index. Kotlin and Dart compile the pinned parser and
scanner through `cc`.

Resolution follows local import paths, named and default bindings, barrel
exports, JSONC `tsconfig` and `jsconfig` aliases, Go module paths, Cargo
workspace crate names, JVM qualified names, and local C/C++, PHP, Ruby, Dart,
and Lua/Luau imports. The name matcher uses scope, receiver types and
inheritance edges before applying the ambiguity ceiling to heuristic matches.
Function-as-value references persist as `references` edges with `fnRef`
metadata. Dispatch heuristics record their provenance.

Framework synthesis adds HTTP routes and handler edges for Express, NestJS,
Django, and Flask. React Router and Next.js pages link to their components
and navigation sites. CodeGraph rebuilds framework routes transactionally
when indexed sources change.

An unresolved reference stays in `unresolved_refs` with its status and name
tail. Changed definitions trigger a retry. When an indexed target disappears,
sync restores the original reference from the edge metadata and resolves it
again. Use a dedicated database path for the index.

- [Parity and binary size](PARITY.md)
- [Third-party notices](third-party/NOTICE.md)
