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
~/.cache/minarbot-cargo-slot.sh cargo build --locked --release -p minarbot-codegraph --all-features --example index
~/.cache/minarbot-node-slot.sh python3 crates/minarbot-codegraph/parity/run.py target/release/examples/index
```

The harness uses Git, Python 3.10+, bubblewrap, and the installed CodeGraph
1.6.2 CLI. It verifies full commit IDs, clones into its own temporary directory,
and runs Node one-shot indexing with a read-only repository, writable
disposable index directory, and network disabled. Databases are read with
SQLite in read-only mode. Pass `--artifacts /tmp/codegraph-parity` to keep
sorted natural-key dumps, error records, and Node logs. Pass `--repo NAME` to
select one corpus, or repeat it to select several. Graph differences yield a
nonzero exit.

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

## Phase 3 query gate

The query comparison exercises the MCP tool responses. It reads the 12 pinned
upstream cases from `__tests__/evaluation/test-cases.ts` and runs 24 named and
prose queries on Minarbot. Recall is the fraction of expected symbols in source
sections or search results. File overlap is intersection divided by union
against the Node CLI. Both engines query the Node database first. A second
native run over the native index measures extraction and resolution effects.

Use CodeGraph 1.6.2 sources at `6560052`, Elasticsearch v8.17.0
(`2b6a7fed`, sparse paths `server/src/main/java` and `libs/core/src/main/java`),
and a disposable Minarbot checkout. Elasticsearch exercises query parity
through the Node-built database. Phase 3 ran before the native Java grammar
was added.

```sh
~/.cache/minarbot-cargo-slot.sh cargo build --locked -p minarbot-server
~/.cache/minarbot-node-slot.sh python3 crates/minarbot-codegraph/parity/queries.py \
  target/debug/minarbot-server --upstream-sources /path/to/codegraph-v1.6.2 \
  --minarbot /path/to/minarbot-clone --elasticsearch /path/to/elasticsearch \
  --artifacts /tmp/minarbot-query-parity
```

The Minarbot snapshot exposed four TypeScript-family parser errors in valid
source, including bare `&` in JSX and inline `import(...).Type` annotations.
The native index now keeps recovered declarations and flags those files as
`parse_recovered`. The extraction baseline above records Phase 1 behavior.

| Corpus | Queries | Query engine / index | Mean symbol recall | File overlap vs. Node |
|---|---:|---|---:|---:|
| Elasticsearch | 12 | Node / Node | 68.8% | 100% |
| Elasticsearch | 12 | Rust / Node | 79.2% | 75.5% |
| Minarbot `c707e98` | 24 | Node / Node | 81.2% | 100% |
| Minarbot `c707e98` | 24 | Rust / Node | 83.3% | 66.8% |
| Minarbot `c707e98` | 24 | Rust / Rust | 83.3% | 66.5% |

The six Elasticsearch search cases returned identical file sets on the shared
database. Across the Minarbot queries, Rust returned the same expected
symbols on either database, with 95.5% file overlap between those runs. Rust
explore showed 7.9 files per Minarbot query on average, versus Node's 7.0.
Explore downranks `test-support/` and standalone `tests.rs` files for
production questions. The Chrome query now includes the browser supervisor
source and returns zero smoke-fixture files, down from seven.
This corpus shows differences in ranking and breadth, with unchanged recall
when substituting the native extraction and resolution.

### Remaining misses

- **Generic lifecycle language**. “Where does a paused turn resume work?”
  surfaces turn controls and the harness configuration. The action is a
  string in `turns/admission.rs` and the scheduler entry is
  `promote` in `turns/dispatch.rs`. Missing that handoff matters when
  changing resume behavior. Use both file paths to trace it.
- **Protocol names and symbol names**. “How does browser page evaluation
  reach Chrome?” expects `evaluate`, which occurs as the CDP method
  `Runtime.evaluate`. The indexed transport symbol is `Cdp::call`.
  The browser supervisor file gives agents a starting point. The oracle's
  missing name overstates the retrieval gap for that question.
- **Broad search crowded by nearby symbols**. “How does search execution work
  from request to shard?” surfaces cluster-shard requests and aggregators.
  The eight-file budget drops the three expected request and shard symbols
  on the shared Node database. That matters for tracing the flow. Naming
  `ShardSearchRequest` anchors a follow-up query.

## Phase 4 extraction parity

The complete run covered the five MVP corpora and the 13 additional language
corpora. The MVP results match the extraction and resolution baselines above.
For each new corpus the harness compares only its named language while each
engine indexes the whole checkout to retain cross-file context. Full commit
IDs and tags are pinned in [run.py](parity/run.py).

Totals are Node/Rust. Differences are Node-only/Rust-only. Files with reported
errors in either engine and their incident edges are excluded from clean rows.

| Language | Repository | Clean files | Nodes | Extraction edges | Error files | File, node, edge diffs |
|---|---|---:|---:|---:|---:|---|
| Java | JUnit 4 `05fe2a6` | 470/471 | 10,921/10,927 | 10,692/10,697 | 0/0 | 0/1, 0/6, 0/5 |
| C | cJSON `acc7623` | 26/66 | 513/956 | 505/958 | 0/33 | 0/40, 0/443, 0/453 |
| C++ | fmt `1239137` | 43/43 | 934/934 | 893/893 | 0/27 | 0/0, 0/0, 0/0 |
| C# | Dapper `360367c` | 141/141 | 3,432/3,432 | 3,355/3,355 | 0/11 | 0/0, 0/0, 0/0 |
| Ruby | Sinatra `7b50a1b` | 147/147 | 1,798/1,798 | 1,724/1,724 | 0/0 | 0/0, 0/0, 0/0 |
| PHP | Monolog `aef6ee7` | 216/216 | 3,072/3,072 | 2,935/2,935 | 0/0 | 0/0, 0/0, 0/0 |
| Swift | Swift Argument Parser `41982a3` | 119/119 | 2,562/2,562 | 2,477/2,477 | 0/27 | 0/0, 0/0, 0/0 |
| Kotlin | kotlinx-cli `85737e0` | 20/20 | 355/355 | 355/355 | 0/1 | 0/0, 0/0, 0/0 |
| R | glue `48688f8` | 24/24 | 87/87 | 63/63 | 0/0 | 0/0, 0/0, 0/0 |
| Lua | luassert `8d8dc8a` | 37/37 | 643/643 | 606/606 | 0/0 | 0/0, 0/0, 0/0 |
| Luau | Lune `010cd36` | 184/184 | 1,452/1,452 | 1,268/1,268 | 0/2 | 0/0, 0/0, 0/0 |
| Scala | scalaj-http `afd7379` | 10/10 | 307/307 | 304/304 | 0/0 | 0/0, 0/0, 0/0 |
| Dart | Dart args `5c83bc9` | 25/25 | 336/336 | 326/326 | 0/0 | 0/0, 0/0, 0/0 |

Extraction records agree across all shared clean files. The Java-only file is
JUnit 4's `src/main/java/org/junit/runners/JUnit4.java`. The extra C files are
tracked sources from cJSON's bundled Unity test code. Minarbot flags native
parser recovery explicitly. Node can recover through WASM without recording
a file error. The clean count excludes that recovery output. Dart's same-ID
re-emissions keep the upstream last value and a single SQLite and FTS row.

All 1,462 shared clean files across the 13 new corpora match on extraction.

## Phase 4 resolution parity

The projection covers the six edge kinds from Phase 2 and excludes edges
incident to framework route nodes. The harness completed all repos and
exited 1 on the reported differences.

| Language | Resolved edges | Unresolved refs | Resolved diff | Unresolved diff |
|---|---:|---:|---:|---:|
| Java | 11,540/11,221 | 8,847/8,533 | 4,353/4,034 | 1,226/912 |
| C | 68/595 | 210/192 | 6/533 | 64/46 |
| C++ | 709/647 | 1,923/1,811 | 243/181 | 1,161/1,049 |
| C# | 2,237/2,879 | 7,119/6,427 | 776/1,418 | 879/187 |
| Ruby | 922/1,166 | 2,561/2,247 | 248/492 | 447/133 |
| PHP | 4,603/4,779 | 5,202/5,026 | 1,201/1,377 | 213/37 |
| Swift | 2,349/2,648 | 2,838/2,465 | 550/849 | 420/47 |
| Kotlin | 584/504 | 553/614 | 85/5 | 4/65 |
| R | 395/395 | 922/922 | 0/0 | 0/0 |
| Lua | 534/774 | 2,831/2,591 | 1/241 | 241/1 |
| Luau | 954/976 | 2,037/2,016 | 28/50 | 23/2 |
| Scala | 378/386 | 998/989 | 75/83 | 16/7 |
| Dart | 1,344/1,348 | 1,300/1,296 | 89/93 | 12/8 |

- **Java and Kotlin** - Qualified imports, inherited methods, overload and
  receiver ranking, and Node's interface-dispatch synthesis account for the
  main gaps. Node also synthesizes expect/actual dispatch in Kotlin.
- **C and C++** - Most C edges unique to Rust have an endpoint in the extra
  Unity sources. Include confidence and macro and standard-library references
  account for shared-corpus differences. C++ also differs on template and
  receiver lookup.
- **C#** - Most differences involve overloads and constructor, attribute, and
  generic type references. Node adds interface call edges.
- **Ruby and PHP** - Require and include paths, inheritance, aliases and
  receiver heuristics differ. Top-level Ruby requires can map to a lib file
  before Node's synthesized path reference resolves.
- **Swift** - Protocol/extension ranking and calls through inferred receivers
  account for retargets. Node also synthesizes interface dispatch.
- **R** - Extraction, resolution metadata and failed rows agree.
- **Lua and Luau** - Require path and local table-member lookup differ.
  Minarbot links relative Luau requires to module files. Lua's extra matches
  include locally bound functions and callable table values.
- **Scala** - Receiver and member ranking cause both metadata and target
  differences. Scala Parser Combinators was replaced as the Scala corpus
  after CodeGraph 1.6.2 failed to resolve its symbolic operator references.
- **Dart** - Constructor chains and import paths account for the differences,
  including self-package URI lookup in Node.

### Framework synthesis

[Framework integration test](tests/frameworks.rs) covers named and inline
Express handlers, NestJS controller prefixes, Django URL patterns, Flask
decorators, React Router routes, Next App Router page and handler files, and
navigation edges. Reindexing removes obsolete routes and navigation edges in
the same transaction. Framework logic resolves static declarations, paths and
handler bindings. Gaps include computed React Router paths, nonliteral
navigation destinations and advanced NestJS bootstrap and routing options.

Route and incident-edge counts below include recovered files. The comparison
uses file, name, line, and edge kind, with counts ordered Node/Rust/common.
Flask emits one route per declared HTTP method. Differences in Express include
chained registrations and inline and mounted handler bindings.

| Repository | Routes | Incident edges |
|---|---:|---:|
| Express | 302/299/296 | 24/15/7 |
| Flask | 303/368/303 | 297/341/295 |
| Excalidraw | 2/2/2 | 4/4/4 |

## Phase 4 binary size

Measured on x86_64 Linux with Rust 1.95.0 using the release `index` example
and `CARGO_PROFILE_RELEASE_STRIP=symbols`. Each single-feature build starts
with `--no-default-features`. The reported delta includes the walker, parser,
scanner, and any code linked for that feature. The linked default and
all-language sizes are measured directly.

| Build | Bytes | Delta from no grammars |
|---|---:|---:|
| No grammars | 6,084,336 | 0 |
| Five MVP grammars | 11,742,480 | 5,658,144 |
| Seven default grammars | 13,128,848 | 7,044,512 |
| All languages | 38,931,088 | 32,846,752 |

| Feature | Bytes | Delta from no grammars |
|---|---:|---:|
| `rust` | 7,444,784 | 1,360,448 |
| `typescript` | 9,252,368 | 3,168,032 |
| `javascript` | 6,796,720 | 712,384 |
| `python` | 6,778,000 | 693,664 |
| `go` | 6,528,624 | 444,288 |
| `java` | 6,745,360 | 661,024 |
| `c` | 7,069,296 | 984,960 |
| `cpp` | 10,023,088 | 3,938,752 |
| `csharp` | 11,620,432 | 5,536,096 |
| `ruby` | 8,376,528 | 2,292,192 |
| `php` | 7,532,528 | 1,448,192 |
| `swift` | 10,006,960 | 3,922,624 |
| `kotlin` | 10,360,816 | 4,276,480 |
| `r` | 6,710,608 | 626,272 |
| `lua` | 6,497,968 | 413,632 |
| `luau` | 6,393,712 | 309,376 |
| `scala` | 10,221,616 | 4,137,280 |
| `dart` | 7,318,608 | 1,234,272 |

Java and C join the defaults to cover common JVM and systems repositories.
Together they add 1,386,368 bytes to the five-language build. Opting into all
remaining grammars adds 25,802,240 bytes beyond the new default. Keep the
existing five plus Java and C as a compact build with broad language coverage.
The server's `codegraph-all-languages` feature opts into the full set.

To measure a single feature, substitute its name for `java` and run the same
build and file-size command. Omit both feature flags for the default or use
the `all-languages` feature for the complete set.

```sh
CARGO_PROFILE_RELEASE_STRIP=symbols ~/.cache/minarbot-cargo-slot.sh cargo build \
  --locked --release -p minarbot-codegraph --example index --no-default-features --features java
stat -c %s target/release/examples/index
```
