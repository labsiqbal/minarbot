# Leptos workspace and settings

Axum serves React by default. Set `MINARBOT_APPEARANCE_CLIENT=leptos` in the local server environment and restart to select Leptos for the routes below. Set it to `react` or remove it and restart to roll back. The launcher passes this setting from its environment file. A missing Leptos build selects React automatically. The fallback page links to React for that visit with `?ui=react`.

Both clients use `minarbot-presentation-v1` in the browser's local storage. The same origin is required to retain preferences. Appearance uses `GET /api/agents` to determine hub-link visibility. It makes no settings API writes.

## Build

From the repository root, install the app dependencies and run its normal build. Then run:

```sh
cargo xtask leptos
```

The command builds the separate Cargo workspace with the Rust toolchain pinned in `rust-toolchain.toml`. It uses the workspace-root `wasm-release` profile and Leptos's `erase_components` option to reduce duplicated render code. It validates the pinned wasm-bindgen and Binaryen 133 CLI archives against their upstream digests and runs `wasm-opt -Oz` per route group. It uses the app's Vite CSS transforms and publishes the generated files to `app/dist/_leptos`. CI and the local release gate build Leptos after the React build. Release packaging includes both clients.

Binaryen 133 is Apache-2.0 licensed and comes from the [upstream release archive](https://github.com/WebAssembly/binaryen/releases/tag/version_133). Its archive SHA-256 and version check live in `crates/xtask/src/leptos.rs`; the staged release includes this notice.

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

Each HTML document loads one bootstrap and its route's configuration. `appearance` owns shell and settings. `conversation` owns agent conversations. Links across groups select the next document through Axum on navigation. When #64 lands, add `workflows` to `ROUTE_GROUPS` in `crates/xtask/src/leptos.rs` with its feature, HTML, CSS and configuration entry in the Vite build.

General, Appearance, Providers, Engines, Skills, minarbot.com, Connected apps,
Email, MCP connections, Vault, Craft Anything, Machines, Devices, Computer,
Updates, Usage, Shared data, Backup & restore, and External access use Leptos
when `MINARBOT_APPEARANCE_CLIENT=leptos`. First-run setup, project overview, and
the Rust-rendered `/pair` form also use Leptos. Project tabs transfer to React.
The separate canonical hub uses its own session and client.

Conversation and agent/room deep links use the #63 pilot with `?ui=leptos`.
Routines, workflows and calendar stay with #64. Port Memory (#48), Guide infrastructure cards (#54) and
Docs (#69) after their changes land on main.

## Verify

Install the UI smoke dependencies under `app/test-support/ui-smoke` and install the pinned Playwright Chromium. Run `node app/test-support/ui-smoke/appearance-smoke.mjs` with `MINARBOT_RUST_BINARY` pointing at the built server. It restarts a disposable loopback server on the same origin to verify both selectors, General API and keyboard parity, appearance persistence, and rollback.

Set `MINARBOT_APPEARANCE_SCREENSHOTS` to a directory for dark and light desktop and narrow screenshots. Set `MINARBOT_APPEARANCE_BENCH=20` for 20 cold, warm reload and warmed settings-navigation samples per client. Readiness means Appearance has rendered and its heading has received focus.

Run `node app/test-support/ui-smoke/settings-connections-smoke.mjs` to compare
connection settings on the same server using synthetic API fixtures. The test
compares mutation paths and redacted payloads. Set `MINARBOT_CONNECTION_SCREENSHOTS`
to capture both themes at desktop and narrow widths.

Run `node --experimental-strip-types app/test-support/ui-smoke/settings-system-smoke.mjs`
with the built server for the system settings parity suite. Set
`MINARBOT_SYSTEM_SCREENSHOTS` to capture both clients at desktop and narrow
widths in dark and light. Update commands use browser fixtures on the
disposable server. The persisted journal keys and JSON formats are shared
with React for updates, table deletion, and access token recovery.

The system settings use `js-sys` (MIT OR Apache-2.0), already pinned through
wasm-bindgen, directly for browser date and URI formatting.

## Warm navigation in #62

On the NUC, Playwright Chromium 156.0.8078.4 measured 20 warmed General-to-Appearance switches per client against the same local server. Times are from click to heading focus.

| Client | Median | p95 |
| --- | ---: | ---: |
| React | 13.0 ms | 18.0 ms |
| Leptos | 12.4 ms | 14.1 ms |

The settings rail stays mounted and navigation reuses the document. Each section gets its own scroll container. This removes the `scrollTop` reset that triggered 38 ms of synchronous layout in the pre-fix Chrome trace.

## Appearance pilot baseline from #59

NUC, Chromium 156.0.8078.4, optimized Axum build, loopback, no throttling. Twenty interleaved runs per client measured time to heading focus. Cold runs used fresh browser contexts. Warm navigation opened Appearance from a warmed General settings page.

| Time to interactive | React median / p95 | Leptos median / p95 |
| --- | ---: | ---: |
| Cold document | 662.5 / 809.3 ms | 88.8 / 151.8 ms |
| Warm reload | 457.3 / 498.7 ms | 43.3 / 67.1 ms |
| Warm settings navigation | 18.4 / 51.1 ms | 47.3 / 62.6 ms |

The original warm-navigation p95 exceeded the 10% regression budget. General-to-Appearance navigation now stays within the Leptos document, preserving the settings rail and workspace queries.

Packaged WASM plus startup glue measured 349,332 bytes raw and 129,079 bytes gzip. Release build wall time was 1,365.09 s for native Cargo, 38.83 s for the React build, 87.57 s for the incremental WASM build and 15.49 s for asset packaging. Build slot wait and network setup are outside those measurements.

## Conversation pilot

With the Leptos selector enabled, append `?ui=leptos` to an agent, side conversation, or room URL. The same route with `?ui=react` opens standard Minarbot. The standalone document resolves the existing API scope and mounts `conversation::shell(ConversationConfig)`. A shell can mount that component keyed by scope and own the surrounding navigation.

Use the node slot script for `app/test-support/ui-smoke/leptos-conversation-smoke.mjs`. Set `MINARBOT_CONVERSATION_SCREENSHOTS` for four theme and viewport captures, and `MINARBOT_CONVERSATION_BENCH=20` for cold editable-composer and warm draft-persistence samples per client. The goal, HTML reply, component reply and MCP Apps smokes accept `MINARBOT_SMOKE_CLIENT=leptos` and retain the same assertions.

## WASM size before the default flip (#74)

Pinned Rust 1.95.0, wasm-bindgen 0.2.130 and Binaryen 133 on Linux x86-64. The baseline uses the packaged `cd8aec9` assets. Sizes are bytes. Gzip uses `gzip -n -9`.

| Route group | `cd8aec9` raw / gzip | Type erasure, before `wasm-opt` | Packaged raw / gzip | Gzip reduction |
| --- | ---: | ---: | ---: | ---: |
| Shell and settings | 3,516,193 / 1,256,411 | 2,786,024 / 953,459 | 2,544,753 / 951,962 | 24.2% |
| Conversation | 1,401,856 / 531,242 | 1,221,664 / 424,012 | 1,105,492 / 421,263 | 20.7% |

Cargo metadata identified `crates/web` as its own workspace root. Baseline rustc arguments confirmed its existing `release` size flags already applied. The dedicated profile scopes those flags to `cargo xtask leptos`.

Twiggy 0.8.0 `top` and `monos` on diagnostic bindgen output with function names showed the most duplication in Tachys reactive render closures. Twiggy estimated 267 KB for settings and 99 KB for conversation. Leptos's type erasure reduces view and attribute specializations in both groups. Symbol names were already stripped from the shipped baseline. `wasm-opt -Oz` reduced raw code further. Route-specific configuration also reduced staged HTML from 58,334 to 47,448 bytes for settings and from 58,036 to 21,736 bytes for conversation.
