# Leptos workspace and settings

Axum serves React by default. Set `MINARBOT_APPEARANCE_CLIENT=leptos` in the local server environment and restart to select Leptos for the routes below. Set it to `react` or remove it and restart to roll back. The launcher passes this setting from its environment file. A missing Leptos build selects React automatically. The fallback page links to React for that visit with `?ui=react`.

Both clients use `minarbot-presentation-v1` in the browser's local storage. The same origin is required to retain preferences. Appearance uses `GET /api/agents` to determine hub-link visibility. It makes no settings API writes.

## Build

From the repository root, install the app dependencies and run its normal build. Then run:

```sh
cargo xtask leptos
```

The command builds the separate Cargo workspace with the Rust toolchain pinned in `rust-toolchain.toml`. It validates the pinned wasm-bindgen and Binaryen CLI archives against upstream digests, applies the `wasm-release` profile and `wasm-opt -Oz`, and publishes the route-group documents and hashed assets to `app/dist/_leptos`. Release packaging includes both clients.

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

General, Appearance, Providers, Engines, Skills, minarbot.com, Connected apps,
Email, MCP connections, Vault, Craft Anything, Machines, Devices, Computer,
Updates, Usage, Shared data, Backup & restore, and External access use Leptos
when `MINARBOT_APPEARANCE_CLIENT=leptos`. First-run setup, project overview,
the Rust-rendered `/pair` form, `/routines`, `/workflows`, `/calendar`, and the
project Calendar tab also use Leptos. The three automation routes share a mounted
workspace shell and load the `workflows` bundle. The project Calendar link loads
the same bundle. Setup and Settings use `appearance`, and the #63 agent, named
conversation and room pilot uses `conversation` when its URL includes
`?ui=leptos`. It includes Files, Changes, workspace browsing, result links,
and Computer. Standalone Computer uses the same bundle when Leptos is enabled
and keeps `ui=react` as a rollback. Settings return links and browser history retain the selected route.
React remains the default for the staged rollout in #67.

## React inventory for #67

Inventory against main `865acc8` plus #64, with the server's Leptos selector
enabled. Source paths are relative to `app/src/`. Each row is a port or cutover
issue.

| Issue group | React screens and dialogs | React sources |
| --- | --- | --- |
| Conversation routing and remaining chat controls | Post-setup `/`, agent Main chat, named and project conversations, and rooms use React on ordinary links. The explicit #63 pilot covers the core transcript, composer, rich replies, queue, approvals, Goals/Loops, chat list and Computer panel. Still on the React path are timeline minimap, historical paging and task-family navigation, speech playback, chat history search across conversations, fork history/origin, checkpoint restore, signed event connection, room management and the remaining dispatch controls. | `App.tsx`, `navigation.ts`, `hooks/useAppRoute.ts`, `ui/AgentConversation.tsx`, `ui/ChatPrimitives.tsx`, `ui/MessageFind.tsx`, `ui/TimelineMinimap.tsx`, `ui/TurnActivity.tsx`, `ui/RunControls.tsx`, `SourceControl.tsx`, `SpeechPlayback.tsx`, `EventConnection.tsx` |
| Agent lifecycle and profile | New agent, Draft with AI, full Profile, Model and Advanced tabs, avatar and look editor, availability/hidden agent restore and deletion, model/provider/runtime and access controls, working folder and Computer settings, Sites and template import/export. The #63 pilot has a basic profile form. | `App.tsx`, `AgentProfileDialog.tsx`, `AgentActions.tsx`, `AgentSidebar.tsx`, `AgentAvatar.tsx`, `AgentVisibility.tsx`, `AgentFolderSettings.tsx`, `AgentComputerSettings.tsx`, `SitesPanel.tsx`, `AgentTemplateImport.tsx` |
| Memory and history summary | Agent and section Memory, including Notes, Learned and Standing questions, search, source links, edit, pin, forget and undo. History summary and compaction controls from chat and profile. | `App.tsx`, `AgentProfileDialog.tsx`, `MemoryPanel.tsx`, `MemoryKnowledge.tsx`, `CompactionPanel.tsx` |
| Workspace search and Needs you | Quick switcher search over conversations, hidden agents and remote agents, plus restore and create actions. Needs you routes pending questions/approvals to exact conversations and focuses their input. Leptos has basic directory search, a quick switcher, keyboard help and sidebar badges. | `App.tsx`, `ui/QuickSwitcher.tsx`, `hooks/useQuickSwitcher.ts`, `hooks/useNeedsYouNotifications.ts`, `NeedsYou.tsx`, `QuestionCard.tsx`, `ApprovalCard.tsx`, `HarnessApprovalCard.tsx`, `HelpPage.tsx` |
| Guide and update prompts | Guide chat on ordinary links, AI connection/preferences dialog and Guide change cards for routine/agent proposals, infrastructure setup, machine install/pairing status and approval. Sidebar update capsule, release notes and Update everything dialog. | `App.tsx`, `GuideActions.tsx`, `FirstRun.tsx`, `SidebarUpdateCapsule.tsx`, `SidebarUpdateReleaseNotes.tsx`, `UpdateEverythingDialog.tsx`, `hooks/useUpdateEverything.ts` |
| Agent inbox and remote agent detail | Agent email inbox and message content at `/agents/:id/inbox`, and paired-machine agent detail at `/machines/:id/agents/:id`. | `App.tsx`, `AgentEmail.tsx`, `MachinesPanel.tsx`, `AgentActions.tsx` |
| Conversation files, results and Computer | Local agent, named conversation and room file panels, workspace browser, Changes, artifact and result previews, run inspector, secure requests and standalone Computer use the Leptos conversation bundle. Remote agent detail follows its own route group. | `RightPanel.tsx`, `RightPanelFiles.tsx`, `RightPanelDiff.tsx`, `WorkspacePanel.tsx`, `HtmlPreview.tsx`, `ResultURLPreview.tsx`, `ComputerViewer.tsx`, `SecureComputerRequest.tsx`, `RunInspector.tsx` |
| Automation embedded in conversations | Agent/section Upcoming routines, inbound routine reports and deep links, and the agent-scoped workflow library, editor, versions, webhook/schedule forms and run inspector with inline step conversations. | `App.tsx`, `WorkflowPanel.tsx`, `WorkflowEditor.tsx`, `WorkflowGraph.tsx`, `WorkflowSchedule.tsx`, `WorkflowWebhook.tsx`, `WorkflowInbox.tsx` |
| Project management and integrations | New project, project-scoped Connected apps, Environment and MCP tabs, project source control, sidebar rename/archive/restore and section settings. Overview, its member/folder forms, section creation and Calendar have Leptos entry points. | `App.tsx`, `Projects.tsx`, `ConnectedAppsPanel.tsx`, `DiscordSettings.tsx`, `WhatsAppSettings.tsx`, `ProjectEnvironmentPanel.tsx`, `McpPanel.tsx`, `SourceControl.tsx` |
| Canonical hub | Clerk sign-in/session handoff, project directory and creation, invitation acceptance, account/machine enrollment, join agent, project members/roles/invitations, participating agents/policies/consent, sections and room chat. | `hub/main.tsx`, `hub/HubApp.tsx`, `hub/client.ts` |
| Fallback and retirement | Workspace loading/error/404, then the full React workspace, setup, Settings, Project and Automation when explicitly selected or when a Leptos document is unavailable. Keep rollback available for the staged releases. | `App.tsx`, `navigation.ts`, `hooks/useAppRoute.ts`, `FirstRun.tsx`, `settings-groups.ts`, `lazyPanels.tsx`, `Projects.tsx`, `AutomationCalendar.tsx`, `AutomationCalendarDialog.tsx`, `RoutineSchedule.tsx`, `RoutineTemplates.tsx`, `WorkflowDirectory.tsx` |

Workflow step conversations open the existing conversation route. Browser Back
restores the run and focuses the step link. The embedded step conversation in
React's workflow panel remains listed under Agent management.

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

Run automation parity from `app/` after packaging Leptos. Set
`MINARBOT_RUST_BINARY` to the freshly built server. The smokes use disposable
loopback fixtures. Workflow Markdown loads the existing `marked` and link
validator in a separate asset when the run inspector needs it.

```sh
node --experimental-strip-types test-support/ui-smoke/automation-parity-smoke.mjs /tmp/minarbot-automation-parity
MINARBOT_SMOKE_UI=leptos node --experimental-strip-types test-support/ui-smoke/automation-calendar-smoke.mjs /tmp/minarbot-calendar-leptos
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
