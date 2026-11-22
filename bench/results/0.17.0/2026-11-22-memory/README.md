# Minarbot Memory LongMemEval-S receipt

Dataset revision `98d7416c24c778c2fee6e6f3006e7a073259d48f`.

The local Hindsight 0.9.2 container at `127.0.0.1:8888` was probed with fresh banks named `minarbot-48-*`. Bank deletion returned HTTP 200 after each probe. Retain and bank creation returned HTTP 500 because PostgreSQL could not resize its shared-memory segment. A retry with the container's configured bearer token returned the same error. No existing Hindsight bank was opened or changed.

On the same six cases and timestamped chunks selected by `bench/memory-longmemeval.py`, Minarbot's BM25 query returned all 10 gold sessions in the first five chunks, with a relevant chunk first in all six cases. The legacy substring query found 0 of 10. [`keyword.json`](keyword.json) records the rankings from a direct SQLite probe using the branch schema and FTS query. These scores measure retrieval on reduced banks.

The release example later measured the Rust paths. Hybrid recall found all 10 gold sessions and ranked a relevant chunk first in all six cases. Median per-case latency was 3,382 ms for hybrid recall and 1.35 ms for keyword recall. [`comparison.json`](comparison.json) records the six cases, rankings and timings.

## Benchmark #2 after merging main

Both sides ran the 16 fixed tasks three times with Claude Code 2.1.328 and `claude-sonnet-4-6`. The pinned main checkout (`638e451`) used the same runner and instrumentation as the branch (`c3a455a`). Both used debug server binaries to compare the two revisions on the shared host.

| Revision | Repeat 1 | Repeat 2 | Repeat 3 | Pass rate | Reported tokens per run, mean ± sample SD | Token coverage |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Main | 10/16 | 10/15 | 9/16 | 29/47 (61.7%) | 20,959 ± 2,464 | 42/48 |
| Branch | 10/16 | 11/16 | 12/16 | 33/48 (68.8%) | 25,284 ± 1,944 | 48/48 |

Main skipped one Computer attempt (HTTP 503). The table totals sum known complete task usage and have different coverage. On the 13 tasks with usage in all six runs, main reported 20,984, 20,071 and 17,641 tokens (19,565 ± 1,728). Branch reported 19,650, 19,803 and 18,652 (19,368 ± 625). The paired change was −197 ± 1,174 tokens per run. Claude's answer ledger reports input and output tokens without cache creation or cache reads. [Main receipt](benchmark-2-main.json) · [Branch receipt](benchmark-2-branch.json)

The benchmark uses fresh banks. Auto-recall added zero context in all 53 branch turns. Three seeded pairs on the branch switched automatic inclusion of the same short note on and off. Each enabled turn added one item (87 UTF-8 bytes, used as a conservative token bound). All six turns reported 3 input and 4 output tokens in the answer ledger. The 512-token auto-recall cap stayed in place. [Seeded recall receipt](auto-recall-probe.json)

The optional external Hindsight URL backend was omitted. [Hindsight comparison receipt](hindsight.json)
