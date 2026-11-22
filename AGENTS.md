# Minarbot

## App

From the repository root, install dependencies, lint, and build.

```sh
cd app
npm ci
npm run lint
npm run build
```

Run app tests with `npm test` from `app/`.

## Rust

From the repository root, run `cargo build --workspace` and `cargo test --workspace`.
Check formatting with `cargo fmt --check`.
Run `cargo clippy --workspace --all-targets -- -D warnings`.
Rust tests return `Result` and propagate errors with `?`. Avoid `unwrap` and `expect`.
On shared hosts, run Cargo and heavy test commands through `~/.cache/minarbot-cargo-slot.sh`.

## Release tooling

Run `cargo xtask release` from the repository root. Use `npm ci` in `app/` and, for `--desktop`, in `desktop/` before running the release gate. Pass `--issues <numbers>` and keep all locally claimed branches until release verification finishes.

For artifact verification, build the app and release binaries first. Run `cargo xtask release --dry-run --releases <temporary-directory> --notes <notes>`. Pass `--desktop` to include the AppImage. See the README for retry, CI and smoke commands.

## Issues

Claim the issue before editing with `Claimed by <agent>, branch t3/<slug>`.

Product context, history, and decisions live in local notes under `notes/` in the primary checkout listed in `~/workspace/INDEX.md`.
