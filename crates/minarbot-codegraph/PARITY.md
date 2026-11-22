# Minarbot CodeGraph extraction parity

Phase 1 matched Node CodeGraph 1.6.2 on **912 clean files, 18,012 nodes, and
20,170 extraction edges**. Counts are Node/Rust. Differences are rows found
only in Node/only in Rust.

| Pinned repository | Language | Clean files | Nodes | Extraction edges | File, node, edge diffs |
|---|---|---:|---:|---:|---|
| ripgrep `4649aa9` | Rust | 98/98 | 3,787/3,787 | 5,767/5,767 | 0/0, 0/0, 0/0 |
| Express `1faf228` | JavaScript | 152/152 | 941/941 | 848/848 | 0/0, 0/0, 0/0 |
| Flask `ab81496` | Python | 83/83 | 2,390/2,390 | 2,395/2,395 | 0/0, 0/0, 0/0 |
| Cobra `40b5bc1` | Go | 36/36 | 900/900 | 1,078/1,078 | 0/0, 0/0, 0/0 |
| Excalidraw `817d8c5` | TypeScript, TSX, JavaScript | 543/543 | 9,994/9,994 | 10,082/10,082 | 0/0, 0/0, 0/0 |

JSX also runs through the Rust integration test alongside the other six
dialects. The pinned repos contain JavaScript files with JSX syntax and no
standalone `.jsx` files.

## Parse errors

Excalidraw's `setupTests.ts` is the sole parse-error file across the five repos
(Node 0, Rust 1). The native parser reported syntax errors. Minarbot stored the
error in `files.errors` and emitted zero symbols for that file. Node's WASM
fallback recovered 17 symbols. The harness lists parse errors and excludes
their files and incident edges from the clean-file comparison.

## Reproduce

```sh
~/.cache/minarbot-cargo-slot.sh cargo build --locked --release -p minarbot-codegraph --example index
python3 crates/minarbot-codegraph/parity/run.py target/release/examples/index
```

The harness uses Git, Python 3.10+, bubblewrap, and the installed CodeGraph
1.6.2 CLI. It verifies full commit IDs, clones into its own temporary directory,
and runs Node one-shot indexing with a read-only repository, writable
disposable index directory, and network disabled. Databases are read with
SQLite in read-only mode. Pass `--artifacts /tmp/codegraph-parity` to keep
sorted natural-key dumps, error records, and Node logs. Any clean-file
difference yields a nonzero exit.

Node natural keys use path, kind, qualified name, line, and column. Compared
records retain names, spans, flags, signatures, docstrings, and other stable
attributes. Edge endpoints use those same keys. The Node extraction projection
includes same-file containment and value references. Node resolution and
framework synthesis produced additional routes and edges on the clean files.

| Repository | Downstream Node rows (nodes / edges) |
|---|---:|
| ripgrep | 0 / 8,044 |
| Express | 302 / 1,598 |
| Flask | 303 / 2,723 |
| Cobra | 0 / 2,925 |
| Excalidraw | 2 / 28,740 |

## Binary size

Release build of the `index` example on x86_64 Linux, with symbols stripped.
Both builds include the SQLite/FTS5-backed index. Phase 1 adds 0 bytes to the
shipped server because the server has no dependency on this crate yet.

| Example build | Bytes |
|---|---:|
| Default, all five grammar crates | 10,584,216 |
| `--no-default-features` | 4,905,240 |
| Default feature delta | 5,678,976 |
