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

Agents start with **Learn from conversations** on. Turning it off pauses
queued work and excludes turns completed while off. A call already started
can finish. Existing memory remains searchable. Re-enabling resumes eligible
work. **Memory model** defaults to the agent model and can select another
model through the same provider binding.
Upgrade imports historical turns for search. Only turns that finish after
upgrade can enter the learning queue.

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

The real Claude Haiku smoke used a temporary data directory and random-port
server. One finished message made three background calls. These are the
recorded usage rows for the prompt extraction, consolidation, and reply
extraction stages.

| Source | Calls per message | Input tokens | Output tokens | Total tokens |
| --- | ---: | ---: | ---: | ---: |
| Prompt extraction | 1 | 6,812 | 915 | 7,727 |
| Consolidation | 1 | 8,735 | 599 | 9,334 |
| Reply extraction | 1 | 6,770 | 82 | 6,852 |
| Full message | 3 | 22,317 | 1,596 | 23,913 |

## Turn memory and standing questions

Minarbot adds relevant saved notes and learned memory to interactive local turns across API models, Codex, Claude Code, and ACP. Automatic lookup uses the SQLite keyword index so starting a turn never waits for model weights or another LLM call. The inserted reference block fits within 512 UTF-8 bytes, a conservative token upper bound for byte-level tokenizers. Pinned entries keep the existing 8 KiB budget. Run inspection reports the automatic lookup budget used and how many entries were included.

- `recall` searches durable notes, facts, and observations in the current bank.
- `reflect` answers a question using up to three searches with the agent's memory model. It returns only cited evidence IDs and their sources. A matching fresh standing answer can be returned directly.
- `remember` saves a note. The existing `remember_memory`, `search_memory`, and `select_memory` contracts remain available for compatibility.
- `search_history` searches completed conversations in the current bank across sessions, including the source chat and message ID.

Private chats use the agent bank. Section chats use the section bank. The **Learn from conversations** switch controls background fact capture, consolidation, and standing-question refresh. Minarbot Memory in agent settings and the section menu lists each bank's notes, learned facts, observations, and standing questions. Pinning uses the same context budget for notes and learned entries. Forgetting takes an entry out of memory lookup. Undo restores the bank entry at its recorded revision. Its original conversation remains in history.

Standing answers are refreshed after background capture settles. Each answer records the cited evidence from the bank generation it was generated against. Edits, forgets, and capture changes invalidate earlier answers until refresh succeeds. The SQLite implementation adapts Hindsight's retrieval loop and refresh concepts. It rewrites bounded answers instead of applying document deltas.

The hybrid recall index remains available through the existing history search API. See the [memory benchmark instructions](../../bench/README.md) for the reduced LongMemEval-S comparison.
