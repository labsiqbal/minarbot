# Testing

Test each behaviour at lowest layer that proves it. Rust unit tests cover logic,
validation, migrations, permissions, encryption and error mapping. Rust
integration tests cover HTTP contracts, storage, restarts and multi-step
server flows. Agent-turn tests use kit fake peers and scripted model only for
agent replies, tool calls, waits, resume, streaming and approvals. UI smokes
cover browser journeys.

## Rust test kit

`crates/minarbot-testkit` owns disposable server processes, fixed-clock
configuration, same-origin `reqwest` helpers, SSE frames, timeout polling,
read-only SQLite inspection and temporary workspaces. `TestRoot::start` and
`Server::restart` reuse one data directory. `ScriptedModel` is an axum
OpenAI-compatible model fixture and records every request. `fake-codex`,
`fake-claude` and `fake-acp` are native line-protocol peers.

```sh
~/.cache/minarbot-cargo-slot.sh cargo build -p minarbot-server
MINARBOT_SERVER_BINARY=$PWD/target/debug/minarbot-server \
  ~/.cache/minarbot-cargo-slot.sh cargo test -p minarbot-e2e
```

The former Node integration and fixture layers are retired by #89. New tests
must not add `app/*.test.ts` or `app/fixtures/*.mjs`.

## Seams

Agent replies always come from fake peers through the test server's options.
Stored features call their API directly without an agent turn. Time comes from
the fixed clock. Tests use only temporary directories and loopback ports.

## Honest tests

A failing test means a product bug or a named removed behavior. Skipped,
ignored or loosened tests count as failures.
