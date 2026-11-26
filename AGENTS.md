# Minarbot

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

The remaining Node suite is legacy coverage during the #89 migration. Do not add Node tests.

## Rust

From the repository root, run `cargo build --workspace` and `cargo test --workspace`.
Check formatting with `cargo fmt --check`.
Run `cargo clippy --workspace --all-targets -- -D warnings`.
Rust tests return `Result` and propagate errors with `?`. Avoid `unwrap` and `expect`.
On shared hosts, run Cargo and heavy test commands through `~/.cache/minarbot-cargo-slot.sh`.

## Tests

Before writing or changing a test, pick its layer, seam and commands from [docs/testing.md](docs/testing.md).

## Release tooling

Run `cargo xtask release` from the repository root. Use `npm ci` in `app/` and, for `--desktop`, in `desktop/` before running the release gate. Pass `--issues <numbers>` and keep all locally claimed branches until release verification finishes.

For artifact verification, run `cargo xtask leptos` and build the release binaries first. Run `cargo xtask release --dry-run --releases <temporary-directory> --notes <notes>`. Pass `--desktop` to include the AppImage. See the README for retry, CI and smoke commands.

## Issues

Claim the issue before editing with `Claimed by <agent>, branch t3/<slug>`.

Product context, history, and decisions live in local notes under `notes/` in the primary checkout listed in `~/workspace/INDEX.md`.
