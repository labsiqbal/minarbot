# Leptos web client

Axum serves one Leptos document, `app.html`, for every workspace route and
the pairing form, and `hub.html` for hosted Hub. If the document or startup
module is missing, Axum serves an HTTP 503 recovery page. Browser
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
It splits WebAssembly, runs `wasm-opt -Oz`, and emits the app and Hub
documents, hashed assets, and the Hub asset allowlist in `app/dist/_leptos`. It also copies
pairing CSS, the web manifest, and its icons from `public/` to `app/dist`.
The app's TypeScript check leaves these files intact.

`assets/` holds tokens, kit CSS, palettes, settings navigation, icons, Inter
fonts, and notices. Vite still compiles `app.css` and `hub.css` and bundles browser adapters
under `app/src/`. The app's startup module defines the workflow Markdown
adapter and loads the conversation adapter, at once on chat routes and on
demand elsewhere. Runtime theme and text size changes run in Rust.

The build receipt reports raw and compressed sizes for primary and split WASM.
Gzip budgets cover the primary module: 430,000 bytes for the app and 240,000
for hosted Hub. Route code loads as split chunks.

## Bootstrap and navigation

The hashed ES module loads wasm-bindgen glue, fetches WASM with two bounded
attempts, and hides the recovery view after a successful mount. Axum serves
WASM with `application/wasm` and immutable hashed assets. Only client documents
allow `wasm-unsafe-eval` in CSP.

`app::mount_app` mounts `shell::WorkspaceShell` once and keeps it across every
route change. Its outlet renders one lazy chunk per route kind
(`router::RouteKind`): chat, automation, first run, a project, and Settings
over the page it opened from. Routines, Workflows and the calendar share one
kind, so each keeps its state while the others show.

`router` owns history: `router::open` changes route in the app (or loads a
document for paths outside it), `Router::replace` rewrites the current entry,
one `popstate` listener follows Back and Forward, and one interceptor turns
same-origin links to app routes into in-app navigation. Settings pages
register in `routes::SettingsRoute`; use `settings::section_url` to preserve
`from`. A lazy chunk's view builds once: reads while it builds do not rebuild
it. Do not write signals in `on_cleanup`; routes now unmount in place.

## Route ownership

| Route kind | Public paths and surfaces | Chunk |
| --- | --- | --- |
| First run | Fresh install at `/` (`data-initial="setup"`), until an agent can chat | `app_setup` |
| Settings | `/settings/:section` including General, Appearance, Providers, Skills, minarbot.com, Connected apps, Email, MCP connections, Vault, Machines, Devices, Computer, Updates, Usage, Shared data, Backup & restore, External access | `app_settings`, over the page it opened from |
| Pairing | `/pair` on a secure origin | Axum pairing form with the app startup module, `app_pairing` |
| Conversation | Configured workspace at `/`, `/agents/:id`, `/agents/:id/conversations/:thread`, `/rooms/:id`, Guide, Files, Changes, workspace browser and results | `app_conversation` |
| Inbox and remote agents | `/agents/:id/inbox` and `/machines/:id/agents/:id` | `app_conversation` |
| Agent-scoped Workflows | `/workflows?agent=:id&from=:chat`, over the chat | `app_scoped_workflows` |
| Standalone Computer | `/?computer=:agent` without the shell | `app_computer` |
| Agent lifecycle and Memory | Agent creation, profiles, templates, conversation and Advanced profile Memory and History summary | Shell, with each dialog loaded on demand |
| Projects | `/projects/:id`, Overview, Calendar, Connected apps, Environment and MCP | `app_project` |
| Automation | `/routines`, `/workflows`, `/calendar` | `app_automation` |
| Embedded automation | Upcoming routines, inbound reports and inline run steps (`app_workflow_step`) | Loaded on demand inside chat and automation |
| Hosted Hub | Accounts and project hosts, `/`, `/login`, `/invite`, `/join`, `/nodes/link` | `hub.html` |

Unknown workspace paths return HTTP 404. The hosted listener reads
`web.distPath` from its configuration. See the [Hub boundary](../../docs/hub-boundary.md)
and [test ledger](../../docs/test-ledger.md#67-ui-coverage-ledger).

## Verify

The Rust browser smokes in `crates/minarbot-e2e/tests/e2e/browser_smokes.rs`
drive this client in headless Chrome. Build the client first, then run them
with the e2e suite (see [testing](../../docs/testing.md#browser-smokes)):

```sh
~/.cache/minarbot-cargo-slot.sh cargo xtask leptos
~/.cache/minarbot-cargo-slot.sh cargo build -p minarbot-server -p minarbot-testkit -p minarbot-launcher
~/.cache/minarbot-cargo-slot.sh cargo test -p minarbot-e2e browser_smokes
```
