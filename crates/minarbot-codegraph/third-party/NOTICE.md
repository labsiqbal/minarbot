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
| tree-sitter-java | 0.23.5 | [94703d5](https://github.com/tree-sitter/tree-sitter-java/tree/94703d5a6bed02b98e438d7cad1136c01a60ba2c) | [MIT](LICENSE-tree-sitter-java-0.23.5) |
| tree-sitter-c | 0.24.2 | [b780e47](https://github.com/tree-sitter/tree-sitter-c/tree/b780e47fc780ddc8da13afa35a3f4ed5c157823d) | [MIT](LICENSE-tree-sitter-c-0.24.2) |
| tree-sitter-cpp | 0.23.4 | [f41e1a0](https://github.com/tree-sitter/tree-sitter-cpp/tree/f41e1a044c8a84ea9fa8577fdd2eab92ec96de02) | [MIT](LICENSE-tree-sitter-cpp-0.23.4) |
| tree-sitter-c-sharp | 0.23.5 | [cac6d5f](https://github.com/tree-sitter/tree-sitter-c-sharp/tree/cac6d5fb595f5811a076336682d5d595ac1c9e85) | [MIT](LICENSE-tree-sitter-c-sharp-0.23.5) |
| tree-sitter-ruby | 0.23.1 | [71bd32f](https://github.com/tree-sitter/tree-sitter-ruby/tree/71bd32fb7607035768799732addba884a37a6210) | [MIT](LICENSE-tree-sitter-ruby-0.23.1) |
| tree-sitter-php | 0.24.2 | [5b5627f](https://github.com/tree-sitter/tree-sitter-php/tree/5b5627faaa290d89eb3d01b9bf47c3bb9e797dea) | [MIT](LICENSE-tree-sitter-php-0.24.2) |
| tree-sitter-swift | 0.7.3 | registry tarball (VCS metadata records a dirty tree) | [MIT](LICENSE-tree-sitter-swift-0.7.3) |
| tree-sitter-r | 1.2.0 | [e9944e9](https://github.com/r-lib/tree-sitter-r/tree/e9944e9801595ad484f49be492daf0c4c81547ef) | [MIT](LICENSE-tree-sitter-r-1.2.0) |
| tree-sitter-luau | 1.2.0 | [a8914d6](https://github.com/tree-sitter-grammars/tree-sitter-luau/tree/a8914d6c1fc5131f8e1c13f769fa704c9f5eb02f) | [MIT](LICENSE-tree-sitter-luau-1.2.0) |
| tree-sitter-kotlin (vendored C) | 0.3.8 + CodeGraph scanner patch | [fwcd 0.3.8](https://github.com/fwcd/tree-sitter-kotlin/tree/0.3.8) | [MIT](../vendor/kotlin/LICENSE) |
| tree-sitter-lua | 0.4.1 | [816840c](https://github.com/tree-sitter-grammars/tree-sitter-lua/tree/816840c592ab973500ae9750763c707b447e7fef) | [MIT](LICENSE-tree-sitter-lua-0.4.1) |
| tree-sitter-scala | 0.26.2 | [b931fcc](https://github.com/tree-sitter/tree-sitter-scala/tree/b931fcc338390925eb893d70ad070033f5856ccf) | [MIT](LICENSE-tree-sitter-scala-0.26.2) |
| tree-sitter-dart (vendored C) | d4d8f3e337d8 | [d4d8f3e337d8](https://github.com/UserNobody14/tree-sitter-dart/tree/d4d8f3e337d8) | [MIT](../vendor/dart/LICENSE) |

Generated C and scanner files are byte-for-byte copies from the pinned upstream
CodeGraph kernel. Kotlin uses its patched scanner. Scala and Lua registry
artifacts match the upstream kernel's generated C and scanner SHA-256 hashes.
PHP uses the HTML-interleaving
grammar. The Swift artifact matches the crate tarball.

Other new direct dependencies:

| Package | Version | Purpose | License and source |
|---|---|---|---|
| cc | 1.11.2 | Build the two vendored C grammars | [MIT OR Apache-2.0](https://crates.io/crates/cc/1.11.2) |
| ignore | 0.4.34 | Gitignore matching and fallback file walk | [Unlicense OR MIT](https://crates.io/crates/ignore/0.4.34) |
| rayon | 1.12.0 | Parallel extraction of changed files | [MIT OR Apache-2.0](https://crates.io/crates/rayon/1.12.0) |
| globset | 0.4.21 | Cargo and package workspace glob matching | [Unlicense OR MIT](https://crates.io/crates/globset/0.4.21) |
| toml_edit | 0.25.19 | Cargo manifest parsing | [MIT OR Apache-2.0](https://crates.io/crates/toml_edit/0.25.19) |

The crate reuses workspace dependencies for hashing, SQLite with FTS5,
regex, error types, JSON, and test temporary directories.
