# Minarbot Memory, based on Hindsight

Upstream release [Hindsight v1.2.3](https://github.com/vectorize-io/hindsight/tree/bd230e8bb40e768bf69f266f1dd3c9ae03e22fe3) at commit `bd230e8bb40e768bf69f266f1dd3c9ae03e22fe3`. The upstream MIT notice is in [LICENSE-hindsight](LICENSE-hindsight). Ported files cite their upstream paths.

## Phase 1 behavior

- `Store::retain` writes raw world, experience, or observation units with embeddings, source metadata, optional event time, and caller-supplied entities. Banks can represent an agent, a channel, or an explicitly shared scope. `Store::link` creates typed edges within a bank.
- `Store::recall` fuses exact cosine, FTS5 BM25, a two-hop recursive graph walk over typed links and shared entities, and temporal results when a date window is supplied. It uses Hindsight's reciprocal rank fusion with k=60, followed by MiniLM cross-encoder reranking. Budget is measured in result units. Date bounds are inclusive UTC Unix milliseconds.
- SQLite stores normalized 384-dimensional little-endian float32 vectors. Exact bank-scoped cosine keeps deployment on bundled SQLite across Minarbot's Linux and macOS targets and avoids native SQLite extension loading. Measure the exact scan before adding an ANN index for larger banks.
- Legacy notes stay in `memories` for existing writes and receipts. The same transaction updates their recall units, including original IDs, scope, text, tombstones, revisions, essential flags, and selection locks. Existing tool search retains substring matching and ID pagination through the new store.
- On upgrade, existing turns become separate prompt and reply units with conversation and timestamp sources. Triggers keep searches current through edits and deletes. Canonical history requires a verified record scope and uses project/owner bank names. The local owner can call `GET /api/agents/{id}/history/search?q=...` or `GET /api/channels/{id}/history/search?q=...` across sessions. The default is offline BM25. `mode=hybrid` loads models and backfills missing vectors in the requested bank before returning ranked results. The first hybrid request for a large bank can take substantially longer than warm recall.
- Phase 2 can retain extracted units and link them to source turns and evidence. Phase 3 can use the same recall API for automatic context and tools.

## Models

Candle performs inference on CPU in Rust. The embedding model is [BAAI bge-small-en-v1.5](https://huggingface.co/BAAI/bge-small-en-v1.5) (MIT). The smaller [MiniLM-L6-v2 cross-encoder](https://huggingface.co/cross-encoder/ms-marco-MiniLM-L6-v2) (Apache-2.0) limits reranking cost. The model artifacts, tokenizers, and configs total **225,761,231 bytes** per installation. Revision-pinned URLs, lengths, and SHA-256 hashes are in [src/models.rs](src/models.rs). Downloads use HTTPS and atomic cache writes. Cached assets are reverified at load. The cache lives beside the database as `<database>.memory-models/`. Model inputs are capped at 256 wordpieces. SQLite stores and searches full text.

| Direct dependency | License | Use |
| --- | --- | --- |
| `candle-core` | MIT OR Apache-2.0 | CPU tensors and safetensors weights |
| `candle-nn` | MIT OR Apache-2.0 | Weight loading and cross-encoder classifier layers |
| `candle-transformers` | MIT OR Apache-2.0 | BERT encoders for BGE and MiniLM |
| `tokenizers` | Apache-2.0 | Checkpoint-compatible WordPiece and paired-input encoding |
| `tempfile` | MIT OR Apache-2.0 | Unique temporary file for atomic cache publication |
| `reqwest` | MIT OR Apache-2.0 | Existing HTTPS client with blocking support on the model worker |
| `rusqlite` | MIT | Existing SQLite with FTS5 and recursive CTEs |
| `serde`, `serde_json` | MIT OR Apache-2.0 | Existing JSON metadata, API responses, and model config |
| `sha2` | MIT OR Apache-2.0 | Existing SHA-256 implementation for artifact checks |
| `uuid` | MIT OR Apache-2.0 | Existing source ID generator for raw retains |
| `chrono` | MIT OR Apache-2.0 | ISO-8601 event date parsing for extracted facts |

No Python, Postgres, or model server is needed at runtime.

## Checks

```sh
~/.cache/minarbot-cargo-slot.sh cargo test --workspace
RAYON_NUM_THREADS=4 ~/.cache/minarbot-cargo-slot.sh cargo test --release -p minarbot-memory --test model_benchmark -- --ignored --nocapture
```

## Phase 1 measurements

Release build on Linux x86-64, Intel Core i5-1340P, four inference threads.
The fixed fixture has 12 notes and four queries with one relevant note each.
For latency, the test fills a file-backed SQLite database on tmpfs with
10,000 units by duplicating one distractor and its BGE vector. Warm recall
includes the query embedding, vector scan, fusion and MiniLM reranking.

| Measurement | Result |
| --- | --- |
| Fixed-fixture precision@3 | 0.333 hybrid vs 0.083 substring |
| Relevant notes at rank 1 | 4 of 4 hybrid vs 1 of 4 substring |
| Warm recall, 20 runs at 10k units | 320.5 ms p50, 441.0 ms p95 |
| Cold model download, verification and load | 8.37 s |
| 10k SQLite fixture | 22,392,832 bytes |
| First-use model download and cache | 225,761,231 bytes |
| Server release binary | 79,577,040 bytes, up 6,057,136 bytes from the v0.16.4 baseline artifact |

## Phase 2 extraction and consolidation

New completed local conversations enqueue prompt and reply units for extraction. Each
request is capped at 4096 UTF-8 bytes and can resume at its byte offset. A model
returns world facts, assistant experiences, entities, event dates, and causal
links in JSON. Raw prompt and reply units stay searchable when extraction or
consolidation fails.

The consolidation queue folds facts into one-facet observations. It uses
Hindsight's fact-by-fact temporal rules, preserves earlier states in
contradictions, deduplicates normalized text, and records evidence links and
proof counts. Source edits invalidate derived facts and observations, then
requeue extraction. Three attempts with backoff bound each job.

`src/prompts/extraction.txt` and `src/prompts/consolidation.txt` carry
provenance headers naming Hindsight v1.2.3 and the upstream paths. Server
provider calls use phase names `memory_extract_*` and `memory_consolidate_*`,
so `/api/usage` reports model calls and input/output/total tokens per source
turn. API agents use their configured account. Codex and Claude Code agents use
their existing login binding in a tool-free, ephemeral run. Old turns, explicit
notes, and turns that run under remote authority keep their Phase 1 recall units.
