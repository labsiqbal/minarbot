# Leptos workspace and settings

Axum serves React by default. Set `MINARBOT_APPEARANCE_CLIENT=leptos` in the local server environment and restart to select Leptos for the routes below. Set it to `react` or remove it and restart to roll back. The launcher passes this setting from its environment file. A missing Leptos build selects React automatically. The fallback page links to React for that visit with `?ui=react`.

Both clients use `minarbot-presentation-v1` in the browser's local storage. The same origin is required to retain preferences. Appearance uses `GET /api/agents` to determine hub-link visibility. It makes no settings API writes.

## Build

From the repository root, install the app dependencies and run its normal build. Then run:

```sh
cargo xtask leptos
```

The command builds the separate Cargo workspace with the Rust toolchain pinned in `rust-toolchain.toml`. It validates the pinned wasm-bindgen CLI archive against its upstream digest, uses the app's Vite CSS transforms and publishes the generated files to `app/dist/_leptos`. CI and the local release gate build Appearance after the React build. Release packaging includes both clients.

Sources for tokens, kit styles, palette CSS and settings navigation live under `app/src`. Runtime theme and text size changes run in Rust. The JavaScript module loads the Wasm with two bounded fetch attempts. The HTML exposes the React fallback and return link throughout startup.

## Mount route groups

Use `shell::WorkspaceShell` once per Leptos document and pass the current workspace
URL, generated icons, and presentation palettes. The `children` slot accepts a
Leptos route outlet or the settings dialog. Pass `header` for route-group controls
in the shared top bar. The shell provides a
`ReadSignal<Option<Result<shell::Directory, String>>>` context for the local agent list
and Guide. Sections owned by another client use ordinary same-origin links so Axum
can select the correct document. Preserve `from` on settings links with
`settings::section_url`. For navigation inside a route group, update history on
selection and render the matching route on `popstate`.

The settings dialog renders the selected page in `settings::shell`. Register a
ported settings page in `routes::SettingsRoute` and use the same URL in the React
handoff. Axum uses the shared route definition to select the document. Keep each
settings section in its own Rust module. Extend the browser smoke before moving
route ownership.

## Route ownership

Leptos owns General, Appearance, Machines, Devices, Computer, Updates, Usage,
Shared data, Backup & restore, and External access when selected. First-run,
project overview, and `/pair` also use Leptos. Agent conversation routes and
project tabs transfer to React, along with Providers, Engines, Skills,
minarbot.com, Connected apps, Email, MCP connections, Vault, and Craft Anything.
The separate canonical hub
continues to use its own session and client.

Conversation and agent/room deep links stay with #63. Routines, workflows and
calendar stay with #64. Port Memory (#48), Guide infrastructure cards (#54) and
Docs (#69) after their changes land on main.

## Verify

Install the UI smoke dependencies under `app/test-support/ui-smoke` and install the pinned Playwright Chromium. Run `node app/test-support/ui-smoke/appearance-smoke.mjs` with `MINARBOT_RUST_BINARY` pointing at the built server. It restarts a disposable loopback server on the same origin to verify both selectors, General API and keyboard parity, appearance persistence, and rollback.

Set `MINARBOT_APPEARANCE_SCREENSHOTS` to a directory for dark and light desktop and narrow screenshots. Set `MINARBOT_APPEARANCE_BENCH=20` for 20 cold, warm reload and warmed settings-navigation samples per client. Readiness means Appearance has rendered and its heading has received focus.

Run `node --experimental-strip-types app/test-support/ui-smoke/settings-system-smoke.mjs`
with the built server for the system settings parity suite. Set
`MINARBOT_SYSTEM_SCREENSHOTS` to capture both clients at desktop and narrow
widths in dark and light. Update commands use browser fixtures on the
disposable server. The persisted journal keys and JSON formats are shared
with React for updates, table deletion, and access token recovery.

The system settings use `js-sys` (MIT OR Apache-2.0), already pinned through
wasm-bindgen, directly for browser date and URI formatting.

## Appearance pilot baseline from #59

NUC, Chromium 156.0.8078.4, optimized Axum build, loopback, no throttling. Twenty interleaved runs per client measured time to heading focus. Cold runs used fresh browser contexts. Warm navigation opened Appearance from a warmed General settings page.

| Time to interactive | React median / p95 | Leptos median / p95 |
| --- | ---: | ---: |
| Cold document | 662.5 / 809.3 ms | 88.8 / 151.8 ms |
| Warm reload | 457.3 / 498.7 ms | 43.3 / 67.1 ms |
| Warm settings navigation | 18.4 / 51.1 ms | 47.3 / 62.6 ms |

The original warm-navigation p95 exceeded the 10% regression budget. General-to-Appearance navigation now stays within the Leptos document, preserving the settings rail and workspace queries.

Packaged WASM plus startup glue measured 349,332 bytes raw and 129,079 bytes gzip. Release build wall time was 1,365.09 s for native Cargo, 38.83 s for the React build, 87.57 s for the incremental WASM build and 15.49 s for asset packaging. Build slot wait and network setup are outside those measurements.
