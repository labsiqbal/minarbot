# Minarbot CodeGraph parity

## Extraction baseline

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
The following table records the phase-1 downstream output.

| Repository | Downstream Node rows (nodes / edges) |
|---|---:|
| ripgrep | 0 / 8,044 |
| Express | 302 / 1,598 |
| Flask | 303 / 2,723 |
| Cobra | 0 / 2,925 |
| Excalidraw | 2 / 28,740 |

## Phase 1 binary size

Release build of the `index` example on x86_64 Linux, with symbols stripped.
Both builds include the SQLite/FTS5-backed index. Phase 1 adds 0 bytes to the
shipped server because the server has no dependency on this crate yet.

| Example build | Bytes |
|---|---:|
| Default, all five grammar crates | 10,584,216 |
| `--no-default-features` | 4,905,240 |
| Default feature delta | 5,678,976 |

## Resolution parity

The phase-2 harness compares calls, imports, extends, implements, references,
and instantiates edges. The last kind captures promoted constructor calls.
Each edge retains coordinates, metadata, and provenance. Failed rows retain
the source symbol, name, kind, coordinates, candidates, path, language, status,
and name tail. The comparison excludes framework routes and incident edges.
Extraction matches on all 912 clean files.

Totals are Node/Rust. Differences are Node-only/Rust-only.

| Repository | Resolved edges | Unresolved refs | Resolved diff | Unresolved diff |
|---|---:|---:|---:|---:|
| ripgrep | 8,044/8,143 | 9,048/8,949 | 398/497 | 201/102 |
| Express | 1,574/1,574 | 9,059/9,059 | 6/6 | 6/6 |
| Flask | 2,372/2,356 | 3,404/3,420 | 222/206 | 22/38 |
| Cobra | 2,876/2,876 | 2,998/2,998 | 0/0 | 0/0 |
| Excalidraw | 28,736/28,815 | 23,736/23,657 | 66/145 | 114/35 |
| Total | 43,602/43,764 | 48,245/48,083 | 692/854 | 343/181 |

The harness exits 1 on this run. It pairs divergent edges by endpoint and
original reference site. Every divergent failed row is a site the other side
resolved. The 460 metadata differences retain the same edge identity.

| Repository | Same edge, different metadata | Other target or kind | Only one side resolves (Node/Rust) |
|---|---:|---:|---:|
| ripgrep | 256 | 40 | 102/201 |
| Express | 0 | 0 | 6/6 |
| Flask | 182 | 2 | 38/22 |
| Cobra | 0 | 0 | 0/0 |
| Excalidraw | 22 | 9 | 35/114 |
| Total | 460 | 51 | 181/343 |

- **ripgrep** - Crate-alias imports, Rust scope guards, receiver inference, and
  scoring account for the differences. Minarbot links workspace declarations
  such as grep_matcher::Matcher that Node leaves failed. Node's qualified
  suffix rule connects Path::new to PrinterPath::new. Minarbot declines that
  external receiver. Receiver and enum-member ranking account for retargets
  such as builder.build.
- **Express** - Node resolves six calls written through this in the CommonJS
  response object (send, vary, cookie). Minarbot resolves six test-local calls
  named fn or router which Node leaves failed. CommonJS imports and the
  event-emitter edge match.
- **Flask** - Confidence and strategy differences remain for declared receivers,
  inherited class methods, and imported globals. Minarbot follows test-local
  fixtures and enclosing `conftest.py` declarations. Node also follows plugins
  and star-imported fixtures. The remaining divergent sites include Jinja
  registry lookups, callback values, and calls through imported receivers.
- **Cobra** - Edges, metadata, and failed rows match.
- **Excalidraw** - JavaScript and TypeScript receiver and scope heuristics
  account for most differences. Examples include InteractiveCanvas callback
  values and calls on scene or app. Node also has a few React-specific
  bindings. JSX child and React render synthesis match on the clean files.

Pass `--artifacts` to write every discrepant row to `*-diff.jsonl`, with its
side, edge or reference keys, source coordinates, and metadata. The harness
prints the reason counts for each repository.
