# Contributing to Minarbot

Thanks for helping. This guide covers setup, the checks every change must pass, and how to send a pull request.

## Find something to work on

- Browse the [open issues](https://github.com/labsiqbal/minarbot/issues). Issues labeled `good first issue` are a gentle start, and `help wanted` marks work that is ready for anyone.
- Before you start, comment on the issue that you are taking it. One issue can have only one owner at a time, so this avoids duplicate work.
- For a new feature or a large change, open an issue first so the approach can be agreed before you write code.

## Set up

You need Linux x86_64, Node.js 22.23.4 or later, npm, Git, and a Rust toolchain with `rustfmt` and `clippy`.

```sh
git clone https://github.com/labsiqbal/minarbot.git
cd minarbot
cargo build --workspace
npm ci --prefix app
npm run build --prefix app
cargo xtask leptos
```

To run your build, start `cargo run --release -p minarbot-server` from the repository root and open <http://127.0.0.1:4317>. Use `PORT` and `MINARBOT_DATA_DIR` to keep a development instance apart from one you use day to day.

## Checks

Run these before you open a pull request. Continuous integration runs the same checks once it lands ([#13](https://github.com/labsiqbal/minarbot/issues/13)).

From the repository root:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```

From `app/`:

```sh
npm run lint
npm run build
```

`cargo test --workspace` includes the end-to-end suite in `crates/minarbot-e2e`, which starts the built server against the kit's fake peers. Pick the layer for a new test from [docs/testing.md](docs/testing.md).

All tests are Rust ([#89](https://github.com/labsiqbal/minarbot/issues/89)). Do not add Node tests.

## Code rules

- **Rust:** tests return `Result` and propagate errors with `?`. Do not use `unwrap` or `expect`.
- **TypeScript:** lint runs with zero warnings allowed. The rules in `app/lint/` also check the client and server boundary and other project conventions.
- **Tests:** every new behavior comes with a Rust test at the lowest layer that proves it. A bug fix starts with a test that fails without the fix.
- **Honest states:** the UI and API never report success for work that did not happen. When an outcome is unknown, show it as unknown.
- **Copy:** all user-visible text is English.
- **Third-party code:** you may adapt code under compatible licenses (for example MIT or Apache-2.0). Keep the original license next to it and record the source in the nearby `provenance.json`. Do not copy code from projects with non-commercial or unclear licenses.
- **Generated files:** change them only through their generator.

## Pull requests

1. Fork the repository and create a branch from `main`.
2. Keep each pull request to one issue or one coherent change, with commits that build and pass the checks.
3. Write commit messages in the imperative mood, for example "Show why Computer is unavailable".
4. In the pull request description, link the issue (`Fixes #123`), describe what changed and why, and list the checks you ran. For UI changes, add a screenshot.
5. A maintainer reviews it. Expect questions and requested changes; they are part of the process.

By contributing, you agree that your contribution is licensed under the [Apache License 2.0](LICENSE).

## Reporting bugs and ideas

Use the issue templates. For a bug, include the Minarbot version (shown in Settings > Updates), what you did, what you expected, and what happened. Security problems go through [SECURITY.md](SECURITY.md), not public issues.
