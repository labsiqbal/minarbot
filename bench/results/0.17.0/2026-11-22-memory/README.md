# Minarbot Memory LongMemEval-S receipt

Dataset revision `98d7416c24c778c2fee6e6f3006e7a073259d48f`.

The local Hindsight 0.9.2 container at `127.0.0.1:8888` was probed with fresh banks named `minarbot-48-*`. Bank deletion returned HTTP 200 after each probe. Retain and bank creation returned HTTP 500 because PostgreSQL could not resize its shared-memory segment. A retry with the container's configured bearer token returned the same error. No existing Hindsight bank was opened or changed.

The committed runner remains `bench/memory-longmemeval.py`. Local Minarbot-versus-legacy measurements require the release example and model cache. The shared Cargo queue did not finish that build during this run.
