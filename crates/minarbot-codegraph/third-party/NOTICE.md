# Minarbot CodeGraph third-party notices

The port carries upstream CodeGraph's MIT license in [LICENSE-codegraph](../LICENSE-codegraph).
The schema is byte-for-byte `src/db/schema.sql` from `colbymchenry/codegraph`
v1.6.2, commit `6560052a6f856855d3f71eee838fd66ccfa4285d`, MIT,
copyright (c) 2026 Colby Mchenry. Rust port files carry their original paths
in their headers. [Upstream source](https://github.com/colbymchenry/codegraph/tree/6560052a6f856855d3f71eee838fd66ccfa4285d)

Tree-sitter runtime and grammar versions match the upstream kernel's
`Cargo.lock`. Source revisions come from crates.io package metadata.

| Package | Version | Source revision | License |
|---|---|---|---|
| tree-sitter | 0.25.10 | [da6fe9b](https://github.com/tree-sitter/tree-sitter/tree/da6fe9beb4f7f67beb75914ca8e0d48ae48d6406) | [MIT](LICENSE-tree-sitter-0.25.10) |
| tree-sitter-go | 0.23.4 | [3c3775f](https://github.com/tree-sitter/tree-sitter-go/tree/3c3775faa968158a8b4ac190a7fda867fd5fb748) | [MIT](LICENSE-tree-sitter-go-0.23.4) |
| tree-sitter-javascript | 0.25.0 | [44c892e](https://github.com/tree-sitter/tree-sitter-javascript/tree/44c892e0be055ac465d5eeddae6d3e194424e7de) | [MIT](LICENSE-tree-sitter-javascript-0.25.0) |
| tree-sitter-python | 0.23.6 | [bffb65a](https://github.com/tree-sitter/tree-sitter-python/tree/bffb65a8cfe4e46290331dfef0dbf0ef3679de11) | [MIT](LICENSE-tree-sitter-python-0.23.6) |
| tree-sitter-rust | 0.24.2 | [e2bee85](https://github.com/tree-sitter/tree-sitter-rust/tree/e2bee853694a1d3e0f6ef308fe3674542fec95d7) | [MIT](LICENSE-tree-sitter-rust-0.24.2) |
| tree-sitter-typescript | 0.23.2 | [f975a62](https://github.com/tree-sitter/tree-sitter-typescript/tree/f975a621f4e7f532fe322e13c4f79495e0a7b2e7) | [MIT](LICENSE-tree-sitter-typescript-0.23.2) |
| tree-sitter-language | 0.1.9 | [10eb5ac](https://github.com/tree-sitter/tree-sitter/tree/10eb5ac119eec9cb4267a8e035066473983f5f3a) | [MIT](LICENSE-tree-sitter-language-0.1.9) |

Other new direct dependencies:

| Package | Version | Purpose | License and source |
|---|---|---|---|
| ignore | 0.4.34 | Gitignore matching and fallback file walk | [Unlicense OR MIT](https://crates.io/crates/ignore/0.4.34) |
| rayon | 1.12.0 | Parallel extraction of changed files | [MIT OR Apache-2.0](https://crates.io/crates/rayon/1.12.0) |

The crate reuses workspace dependencies for hashing, SQLite with FTS5,
regex, error types, JSON, and test temporary directories.
