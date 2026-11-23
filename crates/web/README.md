# Leptos workspace and settings

Axum serves Leptos by default for every route group. Set `MINARBOT_APPEARANCE_CLIENT=react` in the launcher environment file and restart to roll back. `MINARBOT_APPEARANCE_CLIENT=leptos` remains accepted. A missing or invalid Leptos document selects React automatically. The recovery page links to React for that visit with `?ui=react`. The launcher reads the selector from its environment file.

Both clients use `minarbot-presentation-v1` in the browser's local storage. The same origin is required to retain preferences. Appearance uses `GET /api/agents` to determine hub-link visibility. It makes no settings API writes.

## Build

From the repository root, install the app dependencies and run its normal build. Then run:

```sh
cargo xtask leptos
```

The command builds the separate Cargo workspace with the Rust toolchain pinned in `rust-toolchain.toml`. It validates the pinned wasm-bindgen and Binaryen CLI archives against upstream digests, applies the `wasm-release` profile and `wasm-opt -Oz`, and publishes the route-group documents and hashed assets to `app/dist/_leptos`. Release packaging includes both clients.

Sources for tokens, kit styles, palette CSS and settings navigation live under `app/src`. Runtime theme and text size changes run in Rust. The JavaScript module loads the Wasm with two bounded fetch attempts. The HTML exposes the React fallback and return link throughout startup.

The build splits WebAssembly before generating wasm-bindgen glue. Opened settings
sections, profile and project dialogs, Memory, and route content share the
primary module's runtime and document. The receipt lists split assets and their
compressed sizes. The build fails before publishing if primary WASM exceeds
960,000 gzip bytes for shell/settings, 430,000 for conversation, 610,000 for
workflows, or 240,000 for the hosted hub.

## Bootstrap and browser glue

Each Leptos document contains a small hashed ES module that fetches its matching
WASM with two bounded attempts, initializes `wasm-bindgen`, mounts the route, and
hides the server-rendered recovery page only after mount succeeds. Axum serves
WASM with `application/wasm`, immutable hashed assets, and the scoped CSP
`wasm-unsafe-eval` permission. The same recovery page remains usable when an
asset, glue module, or WASM fetch fails.

The browser gate uses external Playwright dependencies. From `app/`, install
the pinned smoke package and Chromium before running browser checks:

```sh
npm ci --prefix test-support/ui-smoke
(cd test-support/ui-smoke && npx playwright-core install --with-deps chromium)
```

Set `PLAYWRIGHT_CHROMIUM_EXECUTABLE`, `CHROME_PATH`, or `CHROME_BIN` when the
browser is installed outside Playwright's cache. These dependencies stay in the
test harness and are not shipped in either client.

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

| Group | Public paths and surfaces | Default Leptos document |
| --- | --- | --- |
| First run and shell | Fresh install at `/`, sidebar and Quick switcher | `appearance.html` on first run, then the document for the active route |
| Settings | `/settings/:section` including General, Appearance, Providers, Engines, Skills, minarbot.com, Connected apps, Email, MCP connections, Vault, Machines, Devices, Computer, Updates, Usage, Shared data, Backup & restore, External access | `appearance.html` |
| Pairing | `/pair` on a secure origin | Axum pairing form with the Leptos appearance bootstrap |
| Conversation | Configured workspace at `/`, `/agents/:id`, `/agents/:id/conversations/:thread`, `/rooms/:id`, Guide, Files, Changes, workspace browser, results and standalone Computer | `conversation.html` |
| Inbox and remote agents | `/agents/:id/inbox` and `/machines/:id/agents/:id` | `conversation.html` |
| Agent lifecycle and Memory | Agent creation, profiles, templates, conversation and Advanced profile Memory and History summary | Active workspace document, with Memory loaded on demand |
| Projects | `/projects/:id`, Overview, Connected apps, Environment and MCP | `appearance.html` |
| Automation | `/routines`, `/workflows`, `/calendar`, project Calendar tab, direct scoped Workflows | `workflows.html` |
| Embedded automation | Upcoming routines, inbound reports, agent-scoped Workflows, and inline run steps | `conversation.html` with workflow content loaded on demand |
| Hosted Hub | Accounts and project hosts, `/`, `/login`, `/invite`, `/join`, `/nodes/link` | `hub.html` |

Each group has a React rollback at the same public URL. Pairing falls back
to the native form. Unknown workspace paths still return HTTP 404. See the
[coverage ledger](../../docs/test-ledger.md#67-ui-coverage-ledger).

Project tabs preserve their URL and browser history. Restoring a project
reopens the sections and conversations that were active when it was archived.
Restoring projects archived before #81 keeps their sections and conversations
archived. Discord and WhatsApp reuse installation settings for project routing.
Settings return links retain the selected workspace route. Both clients share
the conversation, workflow and Memory recovery formats.

## Hosted hub

The hosted listener serves Leptos by default. Build the hub document with
`cargo xtask leptos`. Set `MINARBOT_APPEARANCE_CLIENT=react` on that
listener to select React for accounts, sign-in, machine linking, joining an
agent, project sections, members, invitations and room chat. The hub reads its
own asset allowlist from `app/dist/_leptos/hub-assets.json`. Open the same route
with `?ui=react` for rollback. Both clients use the same sessions, API and
browser draft storage.

## Rollback rehearsal

Use the versioned directory produced by the release dry run, such as
`<releases>/0.17.1-<commit>`. The rehearsal uses a private data directory and a
loopback port.

```sh
MINARBOT_XTASK_BINARY="$PWD/target/debug/xtask" \
  node --experimental-strip-types app/test-support/ui-smoke/client-rollback-smoke.mjs \
  <staged-release-directory> <screenshot-directory>
```

The rehearsal seeds agents, a project, a named conversation, drafts, a running
and finished goal, a loop, a routine, a workflow, and Memory. It restarts through
Leptos, React, and Leptos, checks draft and uncertain-command journals, confirms
the running goal becomes interrupted, and asserts no mutation replay.

The November 23 Stage A run used the optimized native server and both web
builds in staged runtime `0.17.1-62725ce2`. On one loopback origin and one
browser tab, the unset selector opened Leptos, `?ui=react` opened React,
a restart with `react` kept React, and a restart with the selector removed
returned to Leptos.

- Nine API readbacks matched across switches, including both agents, project,
  named conversation and history, completed goal, scheduled loop, routine,
  workflow and Memory note.
- The named conversation draft and the uncertain send's original journal bytes
  survived both restarts. The browser made one send attempt, intentionally
  interrupted by the fixture. It attempted zero replays and the server added
  zero copies. Synthetic provider calls stayed at four throughout the switch.
- The running goal became interrupted on restart. The completed goal stayed met
  and the loop stayed scheduled.
- The signed installer fixture verified artifact acceptance, Leptos default,
  React query override and a synthetic turn. Linux Electron and Tauri AppImages
  passed local packaging and application smoke checks at `0.17.1-1cfb2d0c`.
  The exact `--issues 67` dry run stopped at the ancestry gate because #67's
  claim is embedded in a bullet. Artifact verification used a separate local
  dry run without that flag.

## React inventory for #67

Inventory against main `865acc8` plus #64, with the server's Leptos selector
enabled. Source paths are relative to `app/src/`. Each row is a port or cutover
issue.

| Issue group | React screens and dialogs | React sources |
| --- | --- | --- |
| Memory and history summary | Agent and section Memory and History summary use Leptos in the selected conversation routes and Advanced profile. React provides the same Notes, Learned, Standing questions, and compaction views on rollback. | `App.tsx`, `AgentProfileDialog.tsx`, `MemoryPanel.tsx`, `MemoryKnowledge.tsx`, `CompactionPanel.tsx` |
| Workspace search and Needs you | Leptos shell owns Quick switcher, hidden-agent restore, Help and Needs you. Quick switcher opens the Leptos New project dialog and remote agent detail. | `App.tsx`, `ui/QuickSwitcher.tsx`, `hooks/useNeedsYouNotifications.ts`, `NeedsYou.tsx` |
| Agent inbox and remote agent detail | Agent email inbox and message content at `/agents/:id/inbox`, and paired-machine agent detail at `/machines/:id/agents/:id`. | `App.tsx`, `AgentEmail.tsx`, `MachinesPanel.tsx`, `AgentActions.tsx` |
| Conversation files, results and Computer | Local agent, named conversation and room file panels, workspace browser, Changes, artifact and result previews, run inspector, secure requests and standalone Computer use the Leptos conversation bundle. Remote agent detail follows its own route group. | `RightPanel.tsx`, `RightPanelFiles.tsx`, `RightPanelDiff.tsx`, `WorkspacePanel.tsx`, `HtmlPreview.tsx`, `ResultURLPreview.tsx`, `ComputerViewer.tsx`, `SecureComputerRequest.tsx`, `RunInspector.tsx` |
| Automation embedded in conversations | Agent/section Upcoming routines, inbound routine reports and deep links, and the agent-scoped workflow library, editor, versions, webhook/schedule forms and run inspector with inline step conversations. | `App.tsx`, `WorkflowPanel.tsx`, `WorkflowEditor.tsx`, `WorkflowGraph.tsx`, `WorkflowSchedule.tsx`, `WorkflowWebhook.tsx`, `WorkflowInbox.tsx` |
| Project management and integrations (#81) | Ported creation, project-scoped Connected apps, Environment and MCP, source control, sidebar actions and section settings. | `App.tsx`, `Projects.tsx`, `ConnectedAppsPanel.tsx`, `DiscordSettings.tsx`, `WhatsAppSettings.tsx`, `ProjectEnvironmentPanel.tsx`, `McpPanel.tsx`, `SourceControl.tsx` |
| Canonical hub fallback | Project hosts and central accounts use Leptos by default. Sign-in uses the hosted identity contract and the existing development token fixture. React remains the rollback client. | `hub/main.tsx`, `hub/HubApp.tsx`, `hub/client.ts` |
| Fallback and retirement | Workspace loading/error/404, then the full React workspace, setup, Settings, Project and Automation when explicitly selected or when a Leptos document is unavailable. Keep rollback available for the staged releases. | `App.tsx`, `navigation.ts`, `hooks/useAppRoute.ts`, `FirstRun.tsx`, `settings-groups.ts`, `lazyPanels.tsx`, `Projects.tsx`, `AutomationCalendar.tsx`, `AutomationCalendarDialog.tsx`, `RoutineSchedule.tsx`, `RoutineTemplates.tsx`, `WorkflowDirectory.tsx` |

Conversation details show upcoming agent and section routines. Inbound reports
link to the full run. The Workflows menu opens the agent's library in the
workflows bundle. It reuses the shared editor, graph, versions, schedule,
webhook and run inspector. Back from an inline step restores focus to the step
button. Global workflow step links open the conversation route.

## Verify

Run `node --experimental-strip-types app/test-support/ui-smoke/conversation-controls-parity-smoke.mjs <output-directory>`
after packaging Leptos, with `MINARBOT_RUST_BINARY` pointing at the built server.
The smoke compares navigation, message history, and chat controls in both
themes at desktop and narrow widths against the same disposable server.
Run `MINARBOT_SMOKE_CONVERSATION_LEPTOS=1 node --experimental-strip-types app/test-support/ui-smoke/smoke.mjs`
to run the existing minimap, room, streaming, and dispatch checks on Leptos.

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

Run automation parity from `app/` after packaging Leptos. Set
`MINARBOT_RUST_BINARY` to the freshly built server. The smokes use disposable
loopback fixtures. Workflow Markdown loads the existing `marked` and link
validator in a separate asset when the run inspector needs it.

```sh
node --experimental-strip-types test-support/ui-smoke/automation-parity-smoke.mjs /tmp/minarbot-automation-parity
MINARBOT_SMOKE_UI=leptos node --experimental-strip-types test-support/ui-smoke/automation-calendar-smoke.mjs /tmp/minarbot-calendar-leptos
MINARBOT_CONVERSATION_AUTOMATION_BENCH=20 node --experimental-strip-types test-support/ui-smoke/conversation-automation-parity-smoke.mjs /tmp/minarbot-conversation-automation
MINARBOT_SMOKE_UI=leptos node --experimental-strip-types test-support/ui-smoke/agent-email-smoke.mjs
```

Set `MINARBOT_AUTOMATION_COLD_BENCH=20` to record cold readiness for all three
automation routes in fresh Chromium contexts. The receipt also records 40
warmed switches per route and client, measured from clicking an automation
control to layout of the visible route heading.

Run Files and Computer parity after packaging Leptos. The smoke uses a
disposable Git workspace, synthetic Computer and secure-form fixtures, and
writes dark and light desktop and narrow screenshot pairs and timing receipts.

```sh
MINARBOT_RUST_BINARY="$PWD/target/debug/minarbot-server" node --experimental-strip-types app/test-support/ui-smoke/conversation-files-parity-smoke.mjs /tmp/minarbot-files-parity
```

Run the #78 workspace and Guide fixture smokes from `app/` after packaging. The
workspace smoke starts isolated loopback servers and a private fixture launcher,
then writes dark/light desktop/narrow comparison images and a timing receipt.

```sh
node --experimental-strip-types test-support/ui-smoke/workspace-prompts-smoke.mjs /tmp/minarbot-workspace-prompts
MINARBOT_SMOKE_UI=leptos node --experimental-strip-types test-support/ui-smoke/guide-smoke.mjs
```

Run Memory and History summary parity after packaging. The smoke seeds private
and section banks on a disposable server, checks note recovery and source
paging, and compares desktop and narrow screenshots in both themes.

```sh
MINARBOT_MEMORY_BENCH=20 node --experimental-strip-types test-support/ui-smoke/memory-smoke.mjs /tmp/minarbot-memory-parity
```

Run project parity from `app/` after packaging Leptos. Set
`MINARBOT_RUST_BINARY` to the freshly built server.

```sh
node --experimental-strip-types test-support/ui-smoke/project-smoke.mjs /tmp/minarbot-project-parity
```

The smoke uses a disposable server and synthetic connection responses. It
captures dark and light desktop and narrow screens and compares project
mutation paths and payloads with secrets omitted. `MINARBOT_PROJECT_BENCH`
sets the sample count for cold loads and warmed tab switches.

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

## Verify agent lifecycle

Package Leptos, then run from `app/` with `MINARBOT_RUST_BINARY` pointing to
the built server. Use `MINARBOT_AGENT_BENCH=20` for cold and warm profile
timings, and pass a screenshot directory.

```sh
node --experimental-strip-types test-support/ui-smoke/agent-profile-parity-smoke.mjs /tmp/minarbot-agent-profile
```
