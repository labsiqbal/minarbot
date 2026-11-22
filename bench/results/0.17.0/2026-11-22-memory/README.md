# Minarbot Memory LongMemEval-S receipt

Dataset revision `98d7416c24c778c2fee6e6f3006e7a073259d48f`.

The local Hindsight 0.9.2 container at `127.0.0.1:8888` was probed with fresh banks named `minarbot-48-*`. Bank deletion returned HTTP 200 after each probe. Retain and bank creation returned HTTP 500 because PostgreSQL could not resize its shared-memory segment. A retry with the container's configured bearer token returned the same error. No existing Hindsight bank was opened or changed.

On the same six cases and timestamped chunks selected by `bench/memory-longmemeval.py`, Minarbot's BM25 query returned all 10 gold sessions in the first five chunks, with a relevant chunk first in all six cases. The legacy substring query found 0 of 10. [`keyword.json`](keyword.json) records the rankings from a direct SQLite probe using the branch schema and FTS query. These scores measure retrieval on reduced banks.

The release example measures the Rust paths and adds hybrid recall. That build was still queued at the time of the SQLite probe.
