# Leptos web client

Axum selects a Leptos document for each workspace route. If the document or
startup module is missing, Axum serves an HTTP 503 recovery page. Browser
startup failures leave recovery instructions visible on the same URL. Rebuild
from source or reinstall the current release and reload. Restart hosted Hub
after rebuilding its assets. Pairing can proceed with the native form.

Old launcher files may still contain `MINARBOT_APPEARANCE_CLIENT`. The server
logs once at startup that it ignores the setting. The `ui` query has no effect.
The browser still reads preferences from `minarbot-presentation-v1` on the same
origin. Conversation, workflow, and Memory journals keep their existing formats.

## Build

From the repository root:

```sh
npm ci --prefix app
cargo xtask leptos
```

The command builds the separate workspace with its pinned Rust toolchain and
verifies the wasm-bindgen and Binaryen CLI archives against upstream digests.
It splits WebAssembly, runs `wasm-opt -Oz`, and emits route documents, hashed
assets, and the Hub asset allowlist in `app/dist/_leptos`. It also copies
pairing CSS, the web manifest, and its icons from `public/` to `app/dist`.
The app's TypeScript check leaves these files intact.

`assets/` holds tokens, kit CSS, palettes, settings navigation, icons, Inter
fonts, and notices. Vite still compiles route CSS and bundles browser adapters
under `app/src/`. Runtime theme and text size changes run in Rust.

The build receipt reports raw and compressed sizes for primary and split WASM.
Gzip budgets are 960,000 bytes for shell/settings, 430,000 for conversation,
610,000 for workflows, and 240,000 for hosted Hub.

## Bootstrap and navigation

The hashed ES module loads wasm-bindgen glue, fetches WASM with two bounded
attempts, and hides the recovery view after a successful mount. Axum serves
WASM with `application/wasm` and immutable hashed assets. Only client documents
allow `wasm-unsafe-eval` in CSP.

Mount `shell::WorkspaceShell` once per document. Pass the workspace URL, icons,
palettes, and the route outlet or settings dialog as children. Use the header
slot for route controls. Settings pages register in `routes::SettingsRoute`.
Use `settings::section_url` to preserve `from`. Use same-origin links across
document groups and history updates with `popstate` handling within each group.

## Route ownership

| Group | Public paths and surfaces | Leptos document |
| --- | --- | --- |
| First run and shell | Fresh install at `/`, sidebar and Quick switcher | `appearance.html` on first run, then the document for the active route |
| Settings | `/settings/:section` including General, Appearance, Providers, Skills, minarbot.com, Connected apps, Email, MCP connections, Vault, Machines, Devices, Computer, Updates, Usage, Shared data, Backup & restore, External access | `appearance.html` |
| Pairing | `/pair` on a secure origin | Axum pairing form with the Leptos appearance bootstrap |
| Conversation | Configured workspace at `/`, `/agents/:id`, `/agents/:id/conversations/:thread`, `/rooms/:id`, Guide, Files, Changes, workspace browser, results and standalone Computer | `conversation.html` |
| Inbox and remote agents | `/agents/:id/inbox` and `/machines/:id/agents/:id` | `conversation.html` |
| Agent lifecycle and Memory | Agent creation, profiles, templates, conversation and Advanced profile Memory and History summary | Active workspace document, with Memory loaded on demand |
| Projects | `/projects/:id`, Overview, Connected apps, Environment and MCP | `appearance.html` |
| Automation | `/routines`, `/workflows`, `/calendar`, project Calendar tab, direct scoped Workflows | `workflows.html` |
| Embedded automation | Upcoming routines, inbound reports, agent-scoped Workflows, and inline run steps | `conversation.html` with workflow content loaded on demand |
| Hosted Hub | Accounts and project hosts, `/`, `/login`, `/invite`, `/join`, `/nodes/link` | `hub.html` |

Unknown workspace paths return HTTP 404. The hosted listener reads
`web.distPath` from its configuration. See the [Hub boundary](../../docs/hub-boundary.md)
and [test ledger](../../docs/test-ledger.md#67-ui-coverage-ledger).

## Verify

Install the pinned Playwright dependencies and Chromium:

```sh
npm ci --prefix app/test-support/ui-smoke
(cd app/test-support/ui-smoke && npx playwright-core install --with-deps chromium)
```

Set `PLAYWRIGHT_CHROMIUM_EXECUTABLE`, `CHROME_PATH`, or `CHROME_BIN` when the
smoke runner needs an external Chromium executable. See the
[golden gate guide](../../app/test-support/ui-smoke/PARITY.md) for commands,
timing ceilings, and screenshot thresholds.

The release browser smoke expects an extracted dry-run runtime archive and a
screenshot directory. It launches on loopback with an isolated installation
and a synthetic provider. It opens main routes with both empty and copied
synthetic data, checks journal recovery, and checks for duplicate sends.

```sh
cd app
node --experimental-strip-types test-support/ui-smoke/release-browser-smoke.mjs <extracted-runtime> <screenshot-directory>
```
