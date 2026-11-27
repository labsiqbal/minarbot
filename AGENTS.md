# Minarbot

Weigh every feature, surface or dependency against the Vision in [STRATEGY.md](STRATEGY.md) before building it.

## Working on a change

Work on one change per branch, branched from `main` in its own worktree.
Try it on a running install with `cargo xtask preview` (`--port` runs a second preview beside it); `cargo xtask leptos --dev` rebuilds only the web client, in about a minute.
Merge through a pull request once the owner accepts the preview, then remove the worktree.

## App

From the repository root, install dependencies, lint, type-check, and build the web client.

```sh
npm ci --prefix app
npm run lint --prefix app
npm run build --prefix app
cargo xtask leptos
```

Write integration tests with `minarbot-testkit` as described in `docs/testing.md`. Build the server, the launcher, the kit's fake peers and, for the UI smokes, the Leptos client (`cargo xtask leptos`) before running them:

```sh
cargo build -p minarbot-server -p minarbot-testkit -p minarbot-launcher
cargo test -p minarbot-testkit -p minarbot-e2e
```

All tests are Rust; add new ones only to the Rust suites.
Add a JavaScript library only as a pinned, prebuilt, vendored file with recorded provenance, as noVNC is.
Background model work (titles, summaries, memory extraction) runs through a harness on a light model; the server calls no model API itself.

## Rust

CI runs `cargo build --workspace` and `cargo test --workspace` from the repository root.
Check formatting with `cargo fmt --check`.
Run `cargo clippy --workspace --all-targets -- -D warnings`.
Rust tests return `Result` and propagate errors with `?`. Avoid `unwrap` and `expect`.
On shared hosts, run Cargo and heavy test commands through `~/.cache/minarbot-cargo-slot.sh`.

## Tests

Before writing or changing a test, pick its layer, seam and commands from [docs/testing.md](docs/testing.md).
Run the tests for the area you changed; the full workspace suite belongs to CI and the release gate.

## Release tooling

Run `cargo xtask release` from the repository root. Run `npm ci --prefix app` before the release gate. For `--desktop`, follow the [Tauri build prerequisites](desktop/tauri/README.md#build-and-release). Pass `--issues <numbers>`; the ancestry gate checks each claimed branch locally, or through the head of its merged pull request once the branch is removed.

For artifact verification, run `cargo xtask leptos` and build the release binaries first. Run `cargo xtask release --dry-run --releases <temporary-directory> --notes <notes>`. Pass `--desktop` to include the fixture-signed Tauri AppImage and updater metadata. See the README for retry, CI and smoke commands.

## Issues

Open an issue for each feature and claim it before editing with `Claimed by <agent>, branch t3/<slug>`; a small fix needs no issue.

Product context, history, and decisions live in local notes under `notes/` in the primary checkout listed in `~/workspace/INDEX.md`.
