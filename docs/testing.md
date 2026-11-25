# Testing

Test each behaviour at the lowest layer that proves it. A good test goes red
when its behaviour breaks and stays green through unrelated internal changes.

## Layers

| Layer | Where | Use it for |
| --- | --- | --- |
| Rust unit | `#[cfg(test)]` modules and `crates/*/tests/` | Logic, validation, migrations, permissions, encryption, error mapping. Milliseconds. |
| Node integration | `app/*.test.ts` with `startApp` from `app/test-bridge.ts` | HTTP contracts, storage, multi-step server flows, restarts. Seconds. |
| Agent turn | Node integration plus the fake harness `app/fixtures/fake-harness.mjs` | Only behaviour that depends on an agent reply or tool call: tool loops, waits and resume, delegation, streaming, approvals. |
| UI smoke | Playwright in `app/test-support/ui-smoke/` | User journeys in the browser, with dark and light, desktop and narrow screenshots. |
| Gates | [PARITY.md](../app/test-support/ui-smoke/PARITY.md) | Golden screenshots and timing budgets before a release. |

## New or changed feature

1. Cover logic and edge cases with Rust unit tests.
2. Cover each new route or stored shape with one Node integration test.
3. Cover the main user journey end to end once or twice: a UI smoke when the
   feature has UI, an agent-turn test when an agent drives it.
4. For UI, capture screenshots in the smoke. Update golden baselines only for
   the screens the change is meant to alter.

## Bug fix

1. Reproduce the bug from the user's side, through the browser or HTTP, until
   the test is red.
2. Find and fix the root cause.
3. Keep the regression test at the lowest layer that goes red on the bug. Keep
   the end-to-end reproduction only when no lower layer catches it.

## Seams

- Agent replies always come from the fake harness through the test server's
  options. Tests never call a model provider. When the way Minarbot calls
  models changes, update the fixture rather than each test.
- Tests for threads, memory, shared data, settings, files and other stored
  features call their API directly, with no agent turn.
- Time comes from the test server's `fixedClock` option. Keep wall-clock dates
  out of assertions and screenshots.
- Tests depend only on their own temporary directories and ports: never on
  real free disk space, the real home directory, the network, or the live
  service.

## Running

- While working, run only the files you touched: from `app/`,
  `MINARBOT_RUST_BINARY=$PWD/../target/debug/minarbot-server node --experimental-strip-types --test <file>`,
  and `cargo test -p <crate> <name>`.
- Before handing off, run `cargo test --workspace` and the full `npm test` once,
  through the slot scripts named in [AGENTS.md](../AGENTS.md).

## Honest tests

A failing test means one of two things:
- **A bug.** Fix the product.
- **An intentionally removed behaviour.** Update or delete the test, naming the decision that removed the behaviour.

Every other test stays as it is. Skipped, ignored or loosened tests count as failures.
