# Real-task benchmark

16 fixed tasks measure whether Minarbot completes useful work. Each task lives under `bench/tasks/<id>/` with a prompt, seed inputs, time/token budgets, and a reference to its deterministic checker. Categories cover web apps with tests, seeded bug fixes and features, CSV cleanup, source-backed research, Computer browser flows, documents, delegated room work, HTML output, and routines.

## Commands

```sh
# Validate every checker with known-good fixture outputs. No server or model.
node bench/run.mjs --dry-run
node --test bench/bench.test.mjs

# Build from the checkout you want to benchmark. On NUC, use the shared Cargo slot.
npm --prefix app ci
npm --prefix app run build
~/.cache/minarbot-cargo-slot.sh cargo build --workspace --release --locked

# Select tasks, runtimes, repeats. Omit filters for the full suite.
node bench/run.mjs --tasks counter-app,cart-bug --runtime codex,claude --repeat 2
```

`--runtime` accepts `api`, installed harness runtimes, and ACP runtimes advertised by the disposable server. API needs `MINARBOT_BENCH_API_URL`, `MINARBOT_BENCH_API_MODEL`, and optional `MINARBOT_BENCH_API_KEY`. Without them, API rows say `skipped: no provider`. Codex and Claude use `providerAccount: "device"` and the host CLI sign-ins. Put harness executables on PATH. To use sign-ins stored outside the standard home paths, set `MINARBOT_BENCH_CODEX_HOME` or `MINARBOT_BENCH_CLAUDE_HOME` to an absolute directory.

Use `--model` with a single native runtime to pin the model across benchmark runs.

Requires Linux, Node matching `app/package.json`, `bwrap`, Git, and Chrome for HTML checks. Build the app and release binaries before a live run. Pass a release binary with `--binary`, defaulting to `target/release/minarbot-server`. Each attempt uses a random port, private temporary data, disposable workspace and disposable harness home. The release server uses its fixture configuration to expose the synthetic site to Computer with the normal sandbox and approval policies. It records status, turns, known and complete token counts, wall time, interventions, checker result, and binary/suite hashes in `bench/results/<version>/<timestamp>/`. Approval, question, Computer takeover, and recognized prose clarification requests fail a row. The runner stops at the time limit and checks reported usage as it arrives. A harness can exceed its token budget before reporting usage. Unknown totals cannot pass the token gate. Missing Computer support records a skip with its reason. Unknown provider billing stays unavailable.

Research inputs use frozen source extracts, so checkers verify exact claims and attributed URLs without live web calls. Submitted outputs are checked through no-network `bwrap` sandboxes. Only live runs consume provider usage. Use `--results /path/to/output` for development receipts.

For CodeGraph comparisons, add `--project-codegraph` to run seeded coding
tasks in project conversations. Both binaries get the same prompt, seed, and
checker. For a pre-native baseline, put CodeGraph 1.6.2 on PATH so the runner
can enable the project's external engine. The native server uses its built-in
engine even when that CLI is installed.

## Memory comparison

The reduced LongMemEval-S benchmark uses six fixed question IDs, one from each question type. For each it retains the gold sessions and seven distractors selected by a stable hash. Both engines receive the same timestamped message chunks. The legacy comparison uses its case-insensitive substring query on the same chunks. The output measures distinct gold sessions retrieved and the first relevant rank among five returned chunks. It does not measure generated-answer accuracy.

Download `longmemeval_s_cleaned.json` at the pinned [dataset revision](https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned/tree/98d7416c24c778c2fee6e6f3006e7a073259d48f). Build the Rust example and run the comparison on NUC.

```sh
~/.cache/minarbot-cargo-slot.sh cargo build --release -p minarbot-memory --example longmemeval
~/.cache/minarbot-node-slot.sh python3 bench/memory-longmemeval.py /path/to/longmemeval_s_cleaned.json /path/to/results.json \
  --minarbot-example target/release/examples/longmemeval --models /path/to/model-cache
```

Set `MINARBOT_HINDSIGHT_BENCH_TOKEN` if the local Hindsight API requires a bearer token. The runner creates and deletes only its own throwaway banks. With `--hindsight-results /path/to/receipt.json` it reuses an earlier Hindsight run without opening a bank. The real-task JSON receipts include per-turn provider usage and the auto-recall token upper bound.
