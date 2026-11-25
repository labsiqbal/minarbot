# Test migration ledger

## #89 Rust test kit stage 1

`crates/minarbot-testkit` replaces `app/test-bridge.ts`,
`app/rust-test-server.ts` and the `app/fixtures/*.mjs` peers. The six suites
below were ported case by case to `crates/minarbot-e2e/tests/e2e/<area>.rs`.
A Node case remains only when it tests a TypeScript module, which leaves with
`app/` in stage 5, or when one of its assertions waits on a product bug
listed below. Retained cases keep their original code.

| Node file | Node runtime cases | Rust tests | Node cases kept |
| --- | ---: | ---: | ---: |
| `app/threads.test.ts` | 4 | 3 | 1 (TS module) |
| `app/questions.test.ts` | 2 | 1 | 1 (TS module) |
| `app/goals.test.ts` | 5 | 5 | 1 (B9) |
| `app/cli.test.ts` | 4 | 4 | 3 (B1, B2, B3) |
| `app/agent-create.test.ts` | 8 | 6 | 3 (2 TS module, B4-B6) |
| `app/harness.test.ts` | 17 declarations / 32 runtime cases | 28 | 5 (B7, B8) |
| Total | 55 | 47 | 14 |

`crates/minarbot-e2e/tests/e2e/kit.rs` (2 tests) and
`crates/minarbot-testkit/tests/{fakes,kit}.rs` (18 tests) cover the kit itself.

### Case map

| Node case | Rust test | Notes |
| --- | --- | --- |
| threads: side threads isolate queues, history, controls and archive recovery | `threads::side_threads_isolate_queues_history_controls_and_archive_recovery` | Full. |
| threads: configured concurrency limits side threads and validates before opening storage | `threads::configured_concurrency_limits_side_threads_and_validates_before_opening_storage` | Full. `NaN` and `Infinity` have no JSON form, so the test uses `"NaN"` and `1e300`. |
| threads: conversation commands protect Main chat, revisions, queued work and deleted history across restart | `threads::conversation_commands_protect_main_chat_revisions_queued_work_and_deleted_history` | Full. |
| threads: conversation recovery retains exact request and refuses corrupt or foreign routes | Kept in Node | Tests `app/src/thread-command.ts`. |
| questions: question cards persist, answer once in scope, and resume without repeating effects | `questions::question_cards_persist_answer_once_in_scope_and_resume_without_repeating_effects` | The last step changed with #87: harness turns get an invalid question as an MCP tool error, not a failed turn. The test asserts the rejection reaches the model and no card is stored. |
| questions: answer recovery rejects malformed storage and preserves exact retry identity | Kept in Node | Tests `app/src/question-answer.ts`. |
| goals: goal judges work separately, uses the configured model and keeps ordinary requests unchanged | `goals::goal_judges_work_separately_uses_the_configured_model_and_keeps_ordinary_requests_unchanged` | Full. |
| goals: goal can be set by an agent, pauses for you and resumes on answer | `goals::goal_can_be_set_by_an_agent_pauses_for_you_and_resumes_on_answer` | Full. |
| goals: stop, token limits and restart fence prevent later work | `goals::stop_and_restart_fence_prevent_later_goal_work` | The token-limit step is kept in Node (B9). |
| goals: loops use routine occurrences after restart and stop on until or max runs | `goals::loops_use_routine_occurrences_after_restart_and_stop_on_until_or_max_runs` | Full. |
| goals: ACP work and judge use distinct sessions with tools disabled for the judge | `goals::acp_work_and_judge_use_distinct_sessions_with_tools_disabled_for_the_judge` | Full. The inline ACP script is now the judge mode of `fake-acp`. The agent is bound through `/api/provider-instances`, because #87 ignores `runtime` on create. |
| cli: CLI routine lists and histories stay within the selected granted agent | `cli::cli_routine_lists_and_histories_stay_within_the_selected_granted_agent` | `script provider` reads (B2) and `script models` (B3) are kept in Node. #87 replaced `providerAccount`/`accounts` with `providerInstance`/`providers`. |
| cli: CLI workflow discovery paginates and explicit approvals retain identity after lost responses | `cli::cli_workflow_discovery_paginates_and_explicit_approvals_retain_identity_after_lost_responses` | Full. |
| cli: script CLI discovers only grants, sends once, waits, preserves retry ID and reports attention/timeouts | `cli::script_cli_discovers_only_grants_sends_once_waits_preserves_retry_id_and_reports_attention` | Model-call counts exclude the step after a question (B1); the strict count stays in Node. A deterministic offline window replaces the racy server-stop gap. |
| cli: CLI answers retain receipt identity across lost replies and restart | `cli::cli_answers_retain_receipt_identity_across_lost_replies_and_restart` | Same B1 counting. |
| agent-create: creation recovery keeps exact command and rejects malformed authority | Kept in Node | Tests `app/shared/agent-create.ts`. |
| agent-create: prompt creation atomically queues one restricted setup and recovers without duplicate work | `agent_create::prompt_creation_atomically_queues_one_restricted_setup_and_recovers_without_duplicate_work` | Name-only setup (B4), offline creation (B5) and receipt replay after provider removal (B6) are kept in Node. The saved profile is matched by tool name, since resumed transcripts use server call IDs. |
| agent-create: canonical draft carries project/home authority and rejects legacy or malformed provenance | Kept in Node | Tests `app/test-support/canonical-agent-creation.ts`. |
| agent-create: canonical creation is owner/home scoped, offline-safe, exact and fully atomic | `agent_create::canonical_creation_is_owner_home_scoped_offline_safe_exact_and_fully_atomic` | Full. |
| agent-create: provider binding stays explicit and avatar receipt hashes original bytes | `agent_create::provider_binding_stays_explicit_and_avatar_receipt_hashes_original_bytes` | #87 dropped `agents.provider_account`; the test asserts `agent_authority.provider_binding`. |
| agent-create: canonical setup admission rolls back every row and requires a ready explicit home binding | `agent_create::canonical_setup_admission_rolls_back_every_row_and_requires_a_ready_explicit_home_binding` | Full. |
| agent-create: second project participation and shared CAS isolate rules/capabilities and stale grants | `agent_create::second_project_participation_and_shared_cas_isolate_rules_capabilities_and_stale_grants` | Full. |
| agent-create: local role create/edit receipts preserve normalized profile, omissions and revision conflicts across restart | `agent_create::local_role_create_edit_receipts_preserve_normalized_profile_omissions_and_revision_conflicts` | Full. |
| harness: claude: workspace shell inspection and checks finish without approval | `harness::claude_workspace_shell_inspection_and_checks_finish_without_approval` | Sandbox checks ported with a shell script. The "no approval" assertion is kept in Node (B8). |
| harness (codex, claude): native check receipt prevents a second verification turn | `harness::native_check_receipt_prevents_a_second_verification_turn::{codex,claude}` | Full. |
| harness (codex, claude): gated segments replace only final answer, coalesce writes and restore without replay | `harness::gated_segments_replace_only_final_answer_coalesce_writes_and_restore_without_replay::{codex,claude}` | Full. |
| harness (codex, claude): abort flush preserves final pending segment and stays unfinished after restart | `harness::abort_flush_preserves_final_pending_segment_and_stays_unfinished_after_restart::{codex,claude}` | Full. |
| harness (codex, claude): error flush commits buffered tail without declaring a final answer | `harness::error_flush_commits_buffered_tail_without_declaring_a_final_answer::{codex,claude}` | Full. |
| harness (codex, claude): authoritative snapshot and reply apply identical session-token redaction | `harness::authoritative_snapshot_and_reply_apply_identical_session_token_redaction::{codex,claude}` | Full. |
| harness (codex, claude): stream, tool activity, MCP lifetime, follow-up and restart resume | `harness::stream_tool_activity_mcp_lifetime_follow_up_and_restart_resume::{codex,claude}` | Full. |
| harness (codex, claude): Steer degrades to linked Restart with partial context and fresh native session | Kept in Node | B7. |
| harness (codex, claude): addressed room Steer restarts only selected member; room Stop expires approval and fences restart | Kept in Node | B7. |
| harness (codex, claude): durable approval allow, deny, prefix grant and scoped receipt | `harness::durable_approval_allow_deny_prefix_grant_and_scoped_receipt::{codex,claude}` | Full. |
| harness (codex, claude): workspace write policy, outside-file and network consent | `harness::workspace_write_policy_outside_file_and_network_consent::{codex,claude}` | Full. |
| harness (codex, claude): crash fails once, preserves unfinished text and never replays | `harness::crash_fails_once_preserves_unfinished_text_and_never_replays::{codex,claude}` | Full. |
| harness (codex, claude): cancel kills descendants and revokes MCP; mid-turn runtime switch refused | `harness::cancel_kills_descendants_and_revokes_mcp_mid_turn_runtime_switch_refused::{codex,claude}` | Full. |
| harness (codex, claude): pending approval expires on shutdown/restart, no auto approval | `harness::pending_approval_expires_on_shutdown_restart_no_auto_approval::{codex,claude}` | Full. |
| harness (codex, claude): bounded output fails without replay | `harness::bounded_output_fails_without_replay::{codex,claude}` | Full. |
| harness: runtime changes require confirmation and revision; missing workspaces are provisioned | `harness::runtime_changes_require_confirmation_and_revision_missing_workspaces_are_provisioned` | Removed by #87: the unconfirmed runtime switch (409) and `agents[0].runtime == "api"`. Revisions, replay, runtime choices and a missing executable remain. |
| harness (codex, claude): done check nudges once and preserves native session | `harness::done_check_nudges_once_and_preserves_native_session::{codex,claude}` | Full. |

### Node fixture gaps fixed by the kit

At 7c6bcad the six suites ran 24 of 55 cases green. Most failures came from
the Node bridge and fixtures, not the product:

- With `baseURL` set, the bridge replaced every Codex executable with the
  gateway fixture. Every Codex harness case therefore ran against an
  unreachable model.
- Goal judges have no MCP session, and the gateway fixture called `tools/list`
  on an empty URL, so every goal judgment failed.
- The gateway fixture had no `model/list`, so the Codex model catalog could
  not load.
- Scripted providers assumed that every request offers tools; a request after
  the tool budget ran out crashed them.
- Scripts matched saved tool calls by model call ID, but resumed transcripts
  use server IDs.
- Two CLI steps raced the scheduler. The tests now wait for the in-flight
  precondition the Node case assumed.

### Product bugs found

Each bug keeps its Node case. The Rust test leaves that assertion out until
the bug is fixed.

| Bug | Kept Node assertion | Cause |
| --- | --- | --- |
| B1 | `app/cli.test.ts` "script CLI ..." `assert.equal(calls, 5)` and "CLI answers ..." `assert.equal(calls, 2)` | `ask_user` (`crates/minarbot-server/src/turns/tools.rs`, `"ask_user"` arm) pauses the turn and returns `null` at once. The harness then takes one more model step after every question; a real Codex run could also run more tools while the question waits. If that stray step is held or answered with text, it races the pause, and later turns of the conversation sometimes stalled for over 30 s. Rust tests fail that step at once (`support::after_question_reply`) and leave it out of model-call counts. |
| B2 | `app/cli.test.ts` "CLI routine ..." `assert.equal(settings.exitCode, 0)` | `GET /api/scripts/agents/{id}/provider` holds the `state.database` guard (`automation/scripts.rs:387`) and calls `provider_instances::list` (`:533`), which locks it again. The server deadlocks and every later request hangs. |
| B3 | `app/cli.test.ts` "CLI routine ..." `assert.deepEqual((await terminal(['models', a.id])).models, ['fixture', 'fixture-alt'])` | `script models` forwards a GET to `/api/provider-instances/{id}/models`, which accepts only POST with an `instanceId` body. It always fails with "Use GET or POST for provider instances." |
| B4 | `app/agent-create.test.ts` "prompt creation ..." name-only step `await until(partial.id, 'failed')` | `edit_profile` (`turns/profile.rs:61`) checks the `profile_setup` column. Local setup turns are recognized only through `profile::setup()` (`agent_creations.setup`), so over MCP a setup save without instructions is accepted and the turn ends `done`. |
| B5 | `app/agent-create.test.ts` "prompt creation ..." `await request('/api/agents', { ...offline, runtime: 'api' }, 503)` | a8cfcac removed the "no provider configured" guard for setup creation and added no instance check. With no usable instance the agent is stored (`providerInstance: ""`) with a setup turn that cannot run. |
| B6 | `app/agent-create.test.ts` "prompt creation ..." `await request('/api/agents', { ...input, runtime: 'api' })` after provider removal | Replaying a stored creation after its provider instance is deleted returns 400 "Choose a provider instance." instead of the stored receipt. |
| B7 | `app/harness.test.ts` Steer cases `assert.equal(receipt.dispatch_mode, 'restart')` | a8cfcac removed the "Runtime cannot steer" restart in turn creation and `turns/dispatch.rs`, because it keyed on `agents.runtime`. Every runtime still reports `steer: false` (`data/agents.rs` `capabilities`), but a steer is now stored as "Waiting for steering safe point". |
| B8 | `app/harness.test.ts` "claude: workspace shell ..." `assert.deepEqual(row.harnessApprovals, [])` | 5528da5 made `turns/shell.rs` (`approval`) ask approval for every `run_shell` command in test mode, so tests cannot exercise the production auto-allow for workspace checks. |
| B9 | `app/goals.test.ts` "stop, token limits ..." `await until(getGoal, goal => goal.status === 'token_limit'); assert.equal(app.calls.length, 1)` | `turns::goals::allowance` (`turns/goals.rs:399`) lost its only caller in a8cfcac, so a goal's `maxTokens` is no longer checked before a harness turn. |

No product hook was added; `crates/minarbot-server/src` is unchanged.

## #67 hosted Hub test migration

The app test runner discovers Hub Link once and starts the Rust hosted listener.
Hub Link keeps all 41 cases. The Rust operator command admits accounts, and
browser routes create projects, invite members, post turns, propose and consent
to grants, and withdraw access. Durable row checks read SQLite opened by Rust.
The fixture clock and atomic queue admission preserve timeout boundaries.
The signed withdrawal remains an on-wire check. Two additional scope and
revision assertions call the same Rust withdrawal handler via the isolated
fixture. Fault injection still intercepts the member transport.

| Previous suite | Replacement and matched assertions |
| --- | --- |
| `app/hub-link.test.ts` | Same 41 case names. Public Hub HTTP, Rust clock, and read-only DB observations replace Node authority, routing, web adapters and direct writes. Enrollment, attribution, key custody, dispatch limits, exact replay, expiry, revocation, result order, and fault injection remain. |
| `app/hub/gateway-sessions.test.ts` | All 20 cases use Rust HTTP, pinned JWKS, private tokens, files and ephemeral listeners. Coverage includes duplicate and missing Host, trusted forwarding, browser-bound single-use handoff, session expiry and rotation, exact Origin, CSRF, duplicate JSON/cookies, membership changes during partial body reads, SSE no-store and authorization fences, logout and restart denial, callback CSP, local-only route denial, node credential and path fences, JWT claim/signature rejection, and oversized JSON. Rust configuration with and without a signer replaces the removed Node mount hook. |
| `app/hub/hosted-rust.test.ts` | Seed the frozen pre-collapse Hub schema with typed SQL. Pinned identity, concurrent single-use handoff, session reopen, signed nonce replay, global logout, private backup, and restore/fence/activate assertions remain. |
| `app/hub/web.test.ts` | The gateway suite checks outsider reads, mailbox proof, secret-free invitation lists, owner node/agent binding, exact registration retries and consent roles. The Hub browser smoke covers draft and send recovery. React's standalone join and grant parser cases retired with those parsers. |
| `app/schema.test.ts` fresh Hub startup | Rust creates a fresh database. Its schema, data marker, user version and migration ledger match the frozen `85373b3` snapshot with the Rust Hub extensions. |
| `app/release-runtime-imports.test.ts` | Staging excludes Node Hub source and npm shims. Native `skills list` and `restore` preserve result and diagnostic assertions. |

### Former Node authority and routing cases

Rust unit coverage lives in `crates/minarbot-server/src/hub/tests.rs`. Tests
call the Rust authority and routing functions with private databases. The
schema initializes through Rust storage and the routing writes use the shared
transaction boundary.

| Original case or cases | Rust checks |
| --- | --- |
| Role permission matrix and administrator limits | `roles_fence_reads_posts_and_membership_changes` checks role gates, outsider reads, viewer writes and admin edits. |
| Immutable UUID/host/slug and ownership transfer | `exact_hosts_and_immutable_project_ownership` covers exact hosts, reserved slugs, foreign sections, immutable IDs and tombstones, invalid owner edits, and the transactional one-owner database constraint. Transfer and delete remain absent from the HTTP API. |
| Identity pluggability, mailbox identity and admission | Gateway pinned-JWKS and stub cases authenticate into admitted account IDs, check verified mailbox evidence, reject absent subjects and bad proof, and refuse implicit provisioning. |
| Invite proof scope, expiry, hash storage, replay and issuer status | `invitation_binding_hashes_expiry_replay_and_issuer_membership` checks mailbox, account, project, hash redaction, expiry, replay, revocation and demotion. |
| Fixed home and exact dual consent | `fixed_home_and_exact_dual_consent` and the adapter case check immutable node and agent identity, exact registration, participation revision, policy digest and authenticated owner-node and project approvals. |
| Invocation modes, foreign IDs and idempotency | `invocation_scope_idempotency_and_tenant_isolation` checks owner default, member selection, viewer denial, cross-project section/turn and participation IDs, exact replay, changed input, and requester/project receipt isolation. |
| Section events and payer attribution | `section_event_and_attribution_stay_scoped_to_active_participants` checks delivery only to active matching nodes, independent requester/payer, and scoped grant snapshots. |
| Ordered result receipts and per-effect leases | `ordered_results_preserve_receipts_and_reject_conflicts` and `action_lease_and_result_receipts_recheck_authority` cover frame scope, admission ordering, replay, digest and sequence, immutable intent, lease expiry, exact action identity, and fabricated receipt rejection. |
| Member/requester removal, viewer demotion, archive, node revoke and account block | Rust revocation cases preserve unaffected projects and requester/owner separation, deny fresh invocation and late results, and filter queued deliveries. Archived state is a DB fixture because the old archive helper had no mounted route. |
| Offline admission and outbox ACK | Rust offline and ACK cases check atomic refusal, durable uncertainty, connection generations, exact event and digest, revocation before retry, and empty stale delivery. |
| Invalid policy, malformed keys/frames, expiry, publication bounds and lost approval ACK | Rust policy, node signature, and frame tests cover invalid section/requester, untrusted payer references, keys, extra fields, oversized UTF-16 payloads, forged fences, outbound rollback, publication budget, and exact expired-approval receipt replay. |
| Data marker, migrations, exclusive writer and restore | Rust storage cases cover fresh marker, unmarked/foreign/future/checksum/partial refusal, v0 ledger and reopen, WAL and foreign keys, writer exclusivity and filesystem aliases, private snapshots, restore generation and activation, held invites, unknown work and stale principal. |
| Node production import graph | The app boundary case checks that the Node Hub has no executable modules or npm entrypoint. Staging checks exclude Node Hub source. Rust gateway cases preserve local-owner endpoint and privilege isolation. |

## #67 UI coverage ledger

The route ownership ledger lists each public surface and its Leptos document.
The golden gate runs Leptos against disposable servers and committed screenshots.
[Route ownership](../crates/web/README.md#route-ownership)

| Gate group | Required pairs | Functional coverage |
| --- | ---: | --- |
| appearance | 20 | General and Appearance, settings focus and history, presentation storage, CSP and compressed Wasm, startup failures, setup, pairing |
| automation | 40 | Routine schedules, workflow editor and graph, versioning, webhooks, run inspector |
| calendar | 12 | Automation and project calendars, dates and timezone, run detail and downloads |
| connections | 32 | Provider, engine, Hub, email, Vault, MCP and connected app screens and mutation contracts |
| controls | 58 | Conversation routing and composer, dispatch, attachments, Goals and Needs you |
| conversation | 4 | Named and room conversation navigation, history, sending, streaming and startup |
| files | 52 | Workspace browser, results, Changes, Computer and secure requests |
| guide | 16 | Guide entry, setup and connection behavior |
| hub | 26 | Hosted login, projects, rooms, invitations, members, draft and send recovery. Ten narrow Leptos baselines |
| memory | 32 | Personal and section notes, learned material, standing questions, history summary, journal recovery |
| profiles | 64 | Agent creation, templates, profile editing, lifecycle and avatar actions |
| projects | 68 | Creation, overview, source control, scoped integrations, sections, archives and restore |
| rich | 32 | Markdown, attachments, rich replies, tool/activity display |
| scoped | 52 | Upcoming routines, inbox deep links, agent Workflows, and inline run steps |
| shell | 36 | Workspace navigation, Quick switcher, prompts, command recovery, sidebar controls |
| system | 32 | Usage, updates, devices, shared data, backup and external access, mutation recovery |

The golden gate covers 16 groups and 576 pairs. All pairs use the same pixel
thresholds, including the ten narrow Hub Leptos baselines.

See the screenshot parity guide for the gate command and failure thresholds.
Retained Node and Rust API cases below still cover the shared contracts.
[Parity guide](../app/test-support/ui-smoke/PARITY.md)

### Stage B retirement and retained coverage

This inventory records matching coverage at cutover. The legacy polling and
query-cache cases retired with their React implementations, including the old
backoff, visibility, LRU and identity-clear policies. The shared download
case moved to download-blob.test.ts. The optimistic-turns and right-panel
state unit suites retired when Leptos took over their UI contracts.
Presentation keeps the palette contrast case. Shared tests exercise production
adapter code. Timing sources and ceilings are recorded
in the [golden gate guide](../app/test-support/ui-smoke/PARITY.md).

| Node test | Coverage decision |
| --- | --- |
| `attachments.test.ts` | Keep the upload-limit check for the shared adapter. Retire React presentation and removal helpers. Files/controls cover selection and removal. |
| `audio-recording.test.ts` | Shared `startAudioRecording` runs under the Leptos adapter. Existing lifecycle tests still exercise production code. |
| `clipboard.test.ts` | Shared Clipboard API and fallbacks run under Leptos. Conversation and Computer journeys exercise browser clipboard. |
| `conversation-command.test.ts` | Removed. Rust conversation `protocol::restore`, Leptos response-loss retry and room/section controls cover scope and command identity. |
| `diff-lines.test.ts` | Removed. Leptos files smoke exercises inline and split diff rendering. Rust file/git tests exercise path and edit contracts. |
| `goal-command.test.ts` | Removed. Leptos `goals-smoke.mjs` and Rust conversation input tests cover mode commands and replay. |
| `guide.test.ts` | Removed. Guide and shell smokes navigate routes while Rust reads the same guide screen specification. |
| `http.test.ts` | `readJson` is still called by the Leptos browser adapter. Existing response parsing tests still exercise production code. |
| `markdown.test.ts` | Removed. `leptos-conversation-smoke.mjs` covers structure, unsafe markup, ordinary/result links, attachment mentions and deleted attachments. |
| `markdown-stream-blocks.test.ts` | Keep the shared splitter checks. Remove the React renderer assertion. `leptos-conversation-smoke.mjs` covers rendered reference links and retained prefixes. |
| `message-find.test.ts` | `matchOffsets` is shared by the Leptos browser adapter. Controls smoke checks UI matching and highlight cleanup. |
| `navigation.test.ts` | Keep shared result target validation. Conversation deep links, startup route and history run against Leptos. |
| `needs-you-notifications.test.ts` | Removed. `needs-you-notifications-smoke.mjs` checks request/completion identity and notification deduplication across reload. |
| `next-frame.test.ts` | The tested frame queue is shared by the Leptos HTML reply and MCP app bridges. |
| `optimistic-turns.test.ts` | Leptos conversation retry checks cover an in-flight send, stale history, reload and exact receipt reconciliation. |
| `polling.test.ts` | Leptos Usage smoke uses Chromium fake time to check hidden-tab pause, foreground refresh, exponential backoff capped at 60 s, and recovery to the normal cadence. Stream and profile smokes cover cleanup and stale updates. Download helper is still shared. |
| `presentation.test.ts` | Appearance smoke covers local storage, palettes, mode, text size and tabs. Theme values are shared with the Leptos build. Conversation files checks dock and narrow layouts. |
| `query-cache.test.ts` | Leptos uses route-local caches and generation checks. Hosted Hub and paired-browser smokes exercise account and project switches, sign-out from another tab, and expired sessions. Session loss tears down the old document. The model catalog smoke releases a stale Codex response after switching to API. |
| `right-panel-state.test.ts` | Leptos controls/files smokes exercise docking, tabs, close/reopen, history navigation and narrow overlays. |
| `run-inspector.test.ts` | Leptos renders work logs through the shared `turnActivitySteps` adapter. Files and rich cover inspector display and API errors. |
| `timeline-minimap.test.ts` | Shared preview utility and Leptos `timeline-minimap-smoke.mjs` cover previews, keyboard movement, pointer bounds and narrow behavior. |
| `updateEverything.logic.test.ts` | Removed. System and shell smokes cover update submission, restart, remote receipts and exact payload recovery. Native launcher tests cover persisted restart and rollback. |
| `workflows.test.ts` | Removed. Automation and scoped smokes cover keyboard moves, undo/save, version confirmation, route readiness, run results and journal replay. Rust validates graph paths and step configuration. |

The retired `csp.test.ts` exercised React's style nonce. The appearance smoke
owns CSP, gzip, asset allowlists, retry and failed Wasm. The web recovery test
covers missing and invalid builds, 404s and the ignored selector. The queue
edit HTTP suite retains lease, revision and expiry checks. Its React-only
session-storage parser case retired with the hook. Leptos queue controls
keep the journal format. Leptos transcript and minimap checks cover the retired React status labels,
poll cadence, turn projection and rail geometry. Shared preview and run
inspector tests keep exercising their adapters. The conversation and rich
golden groups own the old chat-like-t3 harness coverage.

### Leptos polling

| Surface | Periodic reads or retries |
| --- | --- |
| Workspace shell | Paired browser session, machines, Quick switcher threads, local/peer update state and update receipt recovery |
| Chat | History and active dispatch, goal state, room read position, queue order and pending edit recovery, details context/files/Git/Computer, Computer control, Guide machine pairing |
| Agent and project screens | Agent folders, resources and sites, local project Git, Memory notes, learned material, standing questions and compaction |
| Automation | Routine list and run lists, workflow list and run inspector, calendar and routine inspector, automation notifications, scoped routine lists, agent inbox, remote agent directory and history |
| Settings and setup | Machines, environment, host metrics, devices, Usage, external access, Computer setup, Skills, Connected apps, Discord, WhatsApp, MCP and local Hub link |
| Hosted Hub | Browser session and project metadata, sections, members and participants |

Periodic requests pause while hidden and back off after failures. Chat uses
shorter intervals while work is live. Queue-edit renewal is a write heartbeat
with a 30 s lease. It slows from 10 s to about 20 s while hidden or after a
failed renewal. SSE subscriptions retain their push and browser reconnect
behavior. Recording, activity and token expiry timers read local clocks.

## Issue #60 turn protocol

Issue #60 runs production `minarbot-server` with stdin config, an OS-assigned IPv4 loopback port, and a private temporary SQLite and config root. Axum providers use their own loopback ports. The lifecycle test verifies startup rejection, child teardown, temporary root removal, and closure of both listeners.

The pre-change run used a clean snapshot of `13bfb19a5dbb1b9bb306521e67c19148f90f82f8` and a server built from the same production source. From `app/`, the Node slot ran:

```sh
~/.cache/minarbot-node-slot.sh node --experimental-strip-types --test --test-concurrency=2 provider-stream.test.ts turn-stream.test.ts dispatch.test.ts test.ts
```

**Before removal:** 24 passed, 0 failed, 0 skipped. The run counted 4 cases in `provider-stream.test.ts`, 3 in `turn-stream.test.ts`, 13 in `dispatch.test.ts`, and 4 in `test.ts`.

The Cargo slot ran `cargo test -p minarbot-server --test turn_protocol --test browser_pairing`. **Before removal:** 9 protocol tests and 1 pairing test passed, 0 failed. Rust paths below are relative to `crates/minarbot-server/tests/`.

| Node case passed before removal | Rust case passed before removal | Matched assertions |
| --- | --- | --- |
| `app/provider-stream.test.ts` - `Rust rejects invalid provider stream termination settings` | `turn_protocol/provider_stream.rs` - `rejects_invalid_provider_stream_termination` | Startup rejects unknown termination and Anthropic `finish-reason` with the same diagnostics. |
| `app/provider-stream.test.ts` - `openai-compatible streaming persists partial text, gates tools, cancels and resumes without replay` | `turn_protocol/provider_stream.rs` - `openai_streaming_gates_tools_and_resumes_without_replay` | Split tool arguments and thought signature round-trip. Broken EOF and wrong MIME fail once without files. Wrong MIME closes upstream. Complete text records 9 tokens. A valid tool creates 1 file in 3 calls. Stop and restart retain the partial. Signed-tool resume retains the file with 2 calls. Explicit text resume uses 1 call. Finish-reason EOF compatibility completes the tool in 3 calls. |
| `app/provider-stream.test.ts` - `anthropic streaming persists partial text, gates tools, cancels and resumes without replay` | `turn_protocol/provider_stream.rs` - `anthropic_streaming_gates_tools_and_resumes_without_replay` | Split tool blocks require the terminal marker. Failed text retains the partial and leaves uncertain usage unset. Complete text records 9 tokens. A valid tool creates 1 file in 3 calls. Wrong MIME drops upstream. Stop and restart retain the partial. Tool and text resumes use 2 and 1 calls respectively. |
| `app/provider-stream.test.ts` - `SIGKILL preserves committed stream text without automatic provider replay` | `turn_protocol/provider_stream.rs` - `sigkill_preserves_committed_text_without_automatic_provider_replay` | SIGKILL preserves the partial, marks the turn and provider usage interrupted, and leaves total tokens unset. Exact submit retry returns HTTP 202 for the same row. Provider calls stay at 1 through a scheduler interval. Rust signals the server directly. |
| `app/turn-stream.test.ts` - `SSE delivers ordered persisted snapshots; reconnect repairs missed text without provider replay` | `turn_protocol.rs` - `sse_delivers_ordered_persisted_snapshots_and_reconnects_without_replay` | HTTP 200 SSE emits empty revision 0, then ordered text under one call sequence. Reconnect returns the latest snapshot, including a packet sent while disconnected. Other agent, thread and channel scopes stay quiet. Query, foreign origin, bearer and POST yield 400, 403, 403 and 405. Final text matches history, provider calls stay at 1, and shutdown closes the stream. |
| `app/dispatch.test.ts` - `room Stop seals gated SSE partial and effects across lost response, restart and new generation` | `turn_protocol.rs` - `room_stop_seals_gated_partial_and_effects_across_lost_response_and_restart` | Stop seals the partial and 1 file, leaving reply null and room messages empty. The exact HTTP 200 receipt replays after an unread response and after restart. A changed generation returns 409. A new generation completes and the original never replays. |
| `app/dispatch.test.ts` - `queued item promotes to steer once, uses safe point and retains original turn ID` | `turn_protocol.rs` - `queued_dispatch_replays_its_receipt_and_steers_once_at_the_safe_point` | Promote keeps the queued turn ID and sets steer mode. Exact HTTP 200 replay returns the same receipt. Changed mode returns 409. Both rows complete and each subsequent provider request receives guidance once. Replay after restart leaves the two rows and call count unchanged. |

**Cases ported and removed:** 7, including all 4 cases in the deleted file `app/provider-stream.test.ts`. The remaining tests in `app/dispatch.test.ts` and `app/turn-stream.test.ts` stay in Node.

| Retained Node case | Rust coverage in this batch | Assertions still covered by Node |
| --- | --- | --- |
| `app/test.ts` - `HTTP: persistence, idempotent FIFO turns, isolation, failure and interrupted recovery` | `admission_and_concurrent_exact_retries_keep_fifo_and_scope` checks HTTP 202 concurrent duplicates, 400 malformed and 403 foreign-origin admission, 409 changed and cross-scope requests, FIFO history, upstream diagnostic redaction, and 503 provider-unavailable admission after restart. | Channel routing, archive races, profile receipts, and other mixed HTTP checks. |
| `app/turn-stream.test.ts` - `room polling retains active members beyond full history and orders prose/tool/final by attempt` | `room_stop_seals_gated_partial_and_effects_across_lost_response_and_restart` checks persisted prose, tool and draft segments by call sequence and ordinal. | Room paging, member handoff, and React transcript projection. |

## Issue #61 remaining HTTP migration

The pre-change Node inventory ran from `df79baf` with the shared Node slot and Rust binaries built from that commit. From the baseline checkout, build the xtask fixture and set `fixture_executable` to the absolute executable path printed by the second command.

```sh
~/.cache/minarbot-cargo-slot.sh cargo build --workspace
~/.cache/minarbot-cargo-slot.sh cargo test -p xtask --bin xtask --no-run
```

Run each listed file individually with its app-relative path in `file`.

```sh
cd app
npm ci
npm run build
```

```sh
MINARBOT_RUST_BINARY=$PWD/../target/debug/minarbot-server \
MINARBOT_RUST_LAUNCHER_BINARY=$PWD/../target/debug/minarbot-launcher \
MINARBOT_XTASK_BINARY=$PWD/../target/debug/xtask \
MINARBOT_XTASK_FIXTURE_BINARY="$fixture_executable" \
~/.cache/minarbot-node-slot.sh node --experimental-strip-types --test --test-concurrency=1 "$file"
```

Each listed file ran alone in one shared Node slot. The first pass counted **177 cases** across 23 files, with 174 passed, 2 failed, and 1 cancelled. The release fixture executable came from the unchanged #58 xtask test harness. The opt-in release build stayed off.

| Node file under `app/` | Cases | Passed | Failed | Cancelled |
| --- | ---: | ---: | ---: | ---: |
| `origins.test.ts` | 2 | 2 | 0 | 0 |
| `machine-origin.test.ts` | 1 | 1 | 0 | 0 |
| `auth.test.ts` | 5 | 5 | 0 | 0 |
| `rust-auth.test.ts` | 3 | 3 | 0 | 0 |
| `rust-automation.test.ts` | 3 | 3 | 0 | 0 |
| `rust-harness.test.ts` | 4 | 4 | 0 | 0 |
| `rust-connectors.test.ts` | 15 | 15 | 0 | 0 |
| `launcher.test.ts` | 8 | 6 | 2 | 0 |
| `updates.test.ts` | 14 | 13 | 0 | 1 |
| `host-metrics.test.ts` | 1 | 1 | 0 | 0 |
| `test.ts` | 4 | 4 | 0 | 0 |
| `rust-data.test.ts` | 4 | 4 | 0 | 0 |
| `csp.test.ts` | 1 | 1 | 0 | 0 |
| `rust-core.test.ts` | 3 | 3 | 0 | 0 |
| `automation-migrations.test.ts` | 3 | 3 | 0 | 0 |
| `rust-data-gaps.test.ts` | 5 | 5 | 0 | 0 |
| `rust-hermes-probe.test.ts` | 1 | 1 | 0 | 0 |
| `install.test.ts` | 6 | 6 | 0 | 0 |
| `hub-link.test.ts` | 41 | 41 | 0 | 0 |
| `hub/gateway-sessions.test.ts` | 20 | 20 | 0 | 0 |
| `hub/hub.test.ts` | 27 | 27 | 0 | 0 |
| `hub/web.test.ts` | 2 | 2 | 0 | 0 |
| `http.test.ts` | 4 | 4 | 0 | 0 |

Both baseline launcher timeouts reproduced with `launcher.test.ts` alone on main at `3c1908e`. The first-probe disconnect case also timed out there. `updates.test.ts` alone on main reproduced the native trial cancellation at its 30-second deadline. The retained native trial now strips the fixture binary once before copying it into three releases.

Before Node removal, the Cargo slot ran `cargo test --workspace`. All 23 API contract cases, 11 turn protocol cases and 13 launcher cases passed. The turn protocol count includes nine cases from #60. Launcher fixtures reserve their loopback port until launch and allow five seconds for candidate startup. The first-probe disconnect and interrupted-trial fixtures allow 15 seconds.

**38 ported and removed. 139 of the 177 baseline cases kept.** Seven Node files retired and six mixed files kept their remaining cases.

The final Node run discovered 179 default test files. It reported 995 passes,
7 failures and 1 existing loader-library skip across 1,003 cases. Isolated
reruns on the pinned `df79baf` baseline used the same production source.

| Retained file | Full Node failure | Isolated baseline result |
| --- | --- | --- |
| `agent-messages.test.ts` | Teammate delivery aborted. | 1 passed. |
| `memory-selection.test.ts` | Legacy view refers to the dropped selection column. | Same failure. |
| `routine-tools.test.ts` | Loopback provider could not be reached. | 1 passed. |
| `rust-routes.test.ts` | History search route absent from frozen inventory. | Same failure. |
| `schema.test.ts` | Canonical and local schema inventories differ from fixtures. | Same 2 failures, 1 passed. |
| `updates.test.ts` | Native trial lost its launcher socket with `ENOENT`. | Same `ENOENT`. The isolated branch rerun also failed with `ENOENT` and passed the other six cases. |

The native update diagnostic logged `Permission denied (os error 13)` for the
intentionally non-executable candidate, then `Invalid update snapshot.` and
`Snapshot restore failed. Server remains stopped.` The synthetic launcher
tests passed. #68 fixed the bundled SQLite read-only FTS5 check and moved
production-database rollback coverage to Rust.

### Batch A - server HTTP, auth, storage, automation, harness, connectors

| Node case | Rust case | Matched assertions |
| --- | --- | --- |
| `app/origins.test.ts` - `extra browser origins are denied unless explicitly configured` | `api_contracts/auth.rs` - `unconfigured_browser_origin_rejected` | Foreign Host and Origin return 403. Exact loopback Origin creates one agent with 201. |
| `app/origins.test.ts` - `a configured https origin is accepted for host and writes while other origins stay denied` | `api_contracts/auth.rs` - `configured_https_host_and_origin_are_exact` | Configured Host and Origin succeed. Evil scheme, host, cross-site fetch and foreign Origin fail. Loopback remains usable. |
| `app/machine-origin.test.ts` - `machine identity uses the first allowed origin so paired peers keep matching it` | `api_contracts/auth.rs` - `first_allowed_origin_identifies_machine` | `/api/environment` reports first configured origin. |
| `app/rust-auth.test.ts` - `Rust JWKS rotates keys, rejects unknown signatures and enforces audience` | `api_contracts/auth.rs` - `jwks_rotates_and_enforces_audience` | First key fetch, key rotation, account continuity, unknown kid and wrong audience rejection. |
| `app/rust-auth.test.ts` - `Rust reopens current canonical schema and rejects unknown versions and migration drift` | `api_contracts/auth.rs` - `canonical_schema_reopens_and_rejects_future_versions_and_checksum_drift` | Version 6 and frozen indexes survive reopen. Future schema and checksum drift refuse startup. |
| `app/rust-auth.test.ts` - `Rust canonical TLS serves protocol errors over a verified fixture certificate` | `api_contracts/auth.rs` - `canonical_tls_returns_protocol_errors_with_verification` | Fixture certificate validates. Missing protocol returns exact 426 envelope. |
| `app/auth.test.ts` - `Rust verifies signed human sessions before internal account mapping` | `api_contracts/auth.rs` - `signed_sessions_validate_before_account_mapping` | Account mapping, subject isolation, cookie rejection, origin rejection, claim validation, unknown key and blocked account. |
| `app/auth.test.ts` - `Rust logout denial survives disk reopen, old JWT expiry and upstream recovery` | `api_contracts/auth.rs` - `logout_denial_survives_restart_expiry_and_upstream_recovery` | Durable denial, expired local denial, upstream recovery and final denial state. |
| `app/auth.test.ts` - `canonical listen failure leaves no maintenance running on a closed database` | `api_contracts/auth.rs` - `occupied_canonical_port_releases_database_for_restart` | Bind failure leaves database startable. |
| `app/auth.test.ts` - `canonical HTTP verifies identity, isolates projects and cannot dispatch legacy routes` | `api_contracts/auth.rs` - `canonical_project_receipts_are_durable_and_tenant_scoped` | Exact replay, changed payload 409, private-field rejection, project isolation, invalid legacy route and duplicate JSON key rejection. |
| `app/auth.test.ts` - `canonical invitation creates durable outbox and worker delivers through configured provider` | `api_contracts/auth.rs` - `canonical_invitation_delivers_in_flight_outbox_after_restart` | Invitation creates durable outbox. Sending state resumes once after restart with member address and product sender. |
| `app/test.ts` - `agent and channel create retries survive edits and restart` | `api_contracts/storage.rs` - `create_retries_return_current_records_and_persist_edit_receipts` | Create replay, activity/read position, archive edit, edit replay and restart durability. |
| `app/test.ts` - `channel read positions are durable, monotone and bound to observed activity` | `api_contracts/storage.rs` - `channel_read_positions_survive_restart_and_do_not_hide_late_replies` | Monotone read position, invalid positions, restart persistence, late reply activity and room isolation. |
| `app/rust-data.test.ts` - `Rust forks preserve bounded persisted transcripts and restart receipts` | `api_contracts/storage.rs` - `fork_context_preserves_bounded_transcript_and_receipt_after_restart` | Six-entry bounded context, compaction summary, exact replay, restart replay and bearer denial. |
| `app/host-metrics.test.ts` - `Rust host metrics cache samples, classify storage files and reject path overrides` | `api_contracts/storage.rs` - `host_metrics_cache_and_redact_storage_paths` | CPU/process fields, cgroup-independent sizes, symlink classification, path redaction, cache interval and Origin/path fences. |
| `app/rust-automation.test.ts` - `Rust workflow schedules retain offline work and coalesce provider-free occurrences` | `api_contracts/automation.rs` - `offline_schedules_coalesce_only_provider_free_work` | Missed count, next anchor and provider-dependent work left due. |
| `app/rust-automation.test.ts` - `Rust workflow schedules skip busy approvals once and leave offline work due until a provider returns` | `api_contracts/automation.rs` - `busy_approval_skips_once_and_offline_work_runs_after_restart` | Invalid interval, busy occurrence, approval replay, restart, provider recovery and parallel output. |
| `app/rust-automation.test.ts` - `Rust cron preview preserves UTC ordering through midnight folds and rejects dense schedules` | `api_contracts/automation.rs` - `cron_preview_keeps_utc_order_and_rejects_dense_schedules` | Havana fold ordering and five-minute density rejection. |
| `app/rust-harness.test.ts` - `device sign-in check runs a mise shim without resolving back to it` | `api_contracts/harness.rs` - `device_sign_in_runs_mise_without_resolving_its_own_shim` | Isolated HOME, trusted PATH, version, signed-in state and no shim recursion. |
| `app/rust-connectors.test.ts` - `Rust calendar exports folded and escaped iCalendar with exact UTC intervals and IANA gap/fold times` | `api_contracts/connectors.rs` - `calendar_preserves_folded_escaped_text_and_exact_utc_times` | Escaping, line folding, exact UTC fold/gap values, omitted unsafe schedules and bearer denial. |
| `app/rust-connectors.test.ts` - `Rust mail retries exact envelope after lost response and fences expired, revoked, tampered and exhausted jobs` | `api_contracts/connectors.rs` - `mail_retries_exact_envelope_and_fences_unusable_jobs` | Exact encrypted envelope retry, idempotency key, expired/revoked/authority/tampered/exhausted fences and ciphertext clearing. |
| `app/rust-core.test.ts` - `core SSE wire format replays persisted snapshots, isolates scopes and closes on shutdown` | `turn_protocol.rs` - `seeded_sse_frames_wake_after_visibility_and_file_lane_commits` | Seeded frame, scope isolation, origin/bearer/query fences, visibility wake, file/site/skill wake, reconnect revision and clean shutdown. |
| `app/test.ts` - `automatic routing shares four workers with execution` | `turn_protocol.rs` - `automatic_routing_and_execution_share_four_workers` | Four active provider calls, fifth queued, safe release, all five done on worker and peak concurrency 4. |
| `app/updates.test.ts` - `update API uses owner Host/Origin/JSON rules, refuses unmanaged apply and preserves request identity` | `api_contracts/updates.rs` - `owner_update_api_validates_request_and_replays_launcher_id` | Local release metadata, one logical launcher command for exact retry, 400 invalid body/ID, 403 Host/Origin/MIME/bearer fences and 409 unmanaged apply. |
| `app/updates.test.ts` - `server side lock rejects a second process on the same DB` | `api_contracts/storage.rs` - `rejects_second_server_on_same_database` | Second process on same database fails with lock diagnostic. First caller remains available. |

The Rust cases above use `crates/minarbot-server/tests/support/server.rs` and `support/loopback.rs`. They spawn production server with private temporary root, loopback port, and real SQLite state.

### Batch B - launcher

Rust integration file `crates/minarbot-launcher/tests/launcher.rs` covers eight cases from `app/launcher.test.ts` and six from `app/updates.test.ts`.

| Node case | Rust case | Matched assertions |
| --- | --- | --- |
| `launcher.test.ts` - launcher starts and accepts update control with a long HOME | `long_home_uses_private_control_socket_and_cleans_it_up` | Uses private fallback socket and directory, reports current release and removes socket on shutdown. |
| `launcher.test.ts` - launcher rejects invalid config, public files, symlinks and hard links | `config_rejects_invalid_shapes_and_untrusted_files` | Rejects all nine malformed configurations, public config/env files, symlinks and hard links. |
| `launcher.test.ts` - launcher parses literal env, overrides routing and delivers native socket commit; receipts survive restart | `literal_environment_and_socket_commit_persist_receipts` | Literal multiline/single-quoted/empty values, no command substitution, routing overrides, private state/snapshot/socket, exact replay and changed receipt rejection. |
| `launcher.test.ts` - launcher rejects corrupt releases and strict socket requests without stopping current server | `corrupt_candidate_and_bad_control_messages_keep_current_server` | Native manifest verification, three malformed control requests, oversized frame closes and current release stays healthy. |
| `launcher.test.ts` - launcher rejects tampered startup and public persisted state | `startup_rejects_tampering_and_public_persisted_state` | Reports manifest mismatch and refuses public persisted state. |
| `launcher.test.ts` - launcher commits a native trial after its first health probe disconnects | `next_probe_commits_after_first_probe_disconnects` | Candidate finishes with matching version, request ID and committed trial. |
| `launcher.test.ts` - Rust launcher selects native release, checks chunked health, snapshots, and stops orphan on parent death | `native_chunked_candidate_stops_after_launcher_sigkill` | Chunked native response selects candidate and SIGKILL of launcher closes its server listener. |
| `launcher.test.ts` - launcher updates and snapshots after its own release binary was deleted | `launcher_updates_after_its_own_binary_is_unlinked` | Launcher survives unlink, commits a native candidate and writes snapshot. |
| `updates.test.ts` - launcher commits healthy trial, preserves data and request identity, and refuses competing launcher | `healthy_trial_commits_once_and_refuses_a_competing_launcher` | Receipt replay and conflict, current/previous selection, durable done status, persisted data, competing launcher exits nonzero. |
| `updates.test.ts` - launcher wrong candidate rolls back release and original DB snapshot | `wrong_version_candidate_rolls_back` | Health rejects candidate version, reselects original release and restores original data. |
| `updates.test.ts` - launcher exit candidate rolls back release and original DB snapshot | `exited_candidate_rolls_back` | Candidate exits after writing damage, reselects original release and restores original data. |
| `updates.test.ts` - launcher refuse candidate rolls back release and original DB snapshot | `refused_candidate_rolls_back_with_diagnostic` | Startup refusal restores data/current and preserves the full data-marker error. |
| `updates.test.ts` - interrupted trial recovers immutable snapshot before restarting previous release | `interrupted_trial_restores_snapshot_before_relaunch` | Verifies damage while trial runs, SIGKILL, closed listener and restored snapshot on restart. |
| `updates.test.ts` - Rust launcher trials native Rust server, commits over socket, and rolls back failed health | `native_trial_commits_and_restores_feature_data` | Checks native socket commit, clean foreign keys and restored feature data after a failed trial. Workspace tests build both binaries. |

### Retained Node coverage and reasons

| Node file | Default cases kept | Remaining assertions |
| --- | ---: | --- |
| `test.ts` | 1 | Composite HTTP lane still asserts channel routing, archive races, profile receipts and recovery across provider failure. |
| `rust-data.test.ts` | 3 | Full seeded read snapshots, FK rollback and external cleanup, canonical project ownership and receipts. |
| `automation-migrations.test.ts` | 3 | Legacy elapsed interval bytes, canonical schedule binding and frozen receipt/sync compatibility. |
| `rust-data-gaps.test.ts` | 5 | Cross-language invitation ciphertext, sync cursors, decoded pixels, UTF-16 ranges and checkpoint binding grants. |
| `updates.test.ts` | 6 | xtask release construction, peer archive/hash and token custody, and server trial admission gates. The real release build case remains opt-in via `MINARBOT_UPDATE_IT=1`. |
| `install.test.ts` | 6 | Signed installer handoff, token hashing and expiry, pairing admission and shell install rollback. |
| `rust-harness.test.ts` | 3 | Gemini mise trust on execution, ACP/Pi model and approval messages, MCP stdio and warm p95. |
| `rust-connectors.test.ts` | 13 | Composio approval/revocation and uncertain writes. Slack/Telegram journal recovery, WebSocket/polling, uploads, workflow delivery and receipts. |
| `rust-core.test.ts` | 2 | Bytes from the built app and validation of the still-used Node test bridge. |
| `web-recovery.test.ts` | 1 | Leptos build availability, recovery responses and routing with old query links. The appearance smoke checks CSP and compressed assets. |
| `http.test.ts` | 4 | TypeScript browser HTTP parsing and presentation errors. |
| `rust-hermes-probe.test.ts` | 1 | Fake Hermes inspection of a private install tree. The test needs a fixture that can prove host files stay unchanged. |
| `hub-link.test.ts` and `hub/*.test.ts` | 90 | Historical phase 6 inventory before the hosted Hub cutover. See the #67 migration above for the current Rust coverage. |

The retained bridge consumers include `test-bridge.ts` for canonical fixtures,
`rust-test-server.ts` for the harness, connector, hub and Computer bridges, and
`task-recovery.test.ts` for the task bridge. The launcher/release fixture helpers
still feed `updates.test.ts`, `github-updates.test.ts` and the xtask checks.

### Other retained suites

Paths below are relative to `app/`. Playwright remains in `test-support/ui-smoke/` for browser journeys and screenshots.

- **Opt-in external**. Real engines and desktop need opt-in fixtures and remain outside the default gate.
  `acp-harness.integration.test.ts`, `rust-desktop.integration.test.ts`.

- **Browser adapter and asset checks**. Exercise helpers used by Leptos and its build.
  `attachments.test.ts`, `audio-recording.test.ts`, `clipboard.test.ts`, `download-blob.test.ts`,
  `markdown-stream-blocks.test.ts`, `message-find.test.ts`, `navigation.test.ts`,
  `next-frame.test.ts`, `presentation.test.ts`, `run-inspector.test.ts`, `timeline-minimap.test.ts`.

- **Build, schema and release**. Keep packaging, frozen schema, route inventory, environment and download checks with their fixture and tooling consumers.
  `check-served.test.ts`, `fanout-older-peer.test.ts`, `github-updates.test.ts`,
  `path-hydration.test.ts`, `peer-release-transfer.test.ts`, `release-gates.test.ts`,
  `release-runtime-imports.test.ts`, `rust-production-env.test.ts`, `rust-routes.test.ts`,
  `schema.test.ts`, `scripts/test-discovery.test.ts`.

- **Canonical and device**. The remaining authority, sync, backup, restore, enrollment and executor cases need equivalent persisted seeds and signed device/tenant fixtures.
  `agent-create.test.ts`, `application-sync.test.ts`, `backup.test.ts`, `devices.test.ts`,
  `project-access.test.ts`, `protocol.test.ts`, `restore-fence.test.ts`, `restore.test.ts`,
  `rust-devices.test.ts`, `rust-projects.test.ts`, `rust-tasks.test.ts`,
  `task-recovery.test.ts`, `two-process.test.ts`.

- **Connector and MCP**. Retain adapter delivery, approval, remote-session and replay assertions until Rust fixtures cover the same interleavings and effects.
  `composio.test.ts`, `connector-task-files.test.ts`, `mail.test.ts`, `mcp-context.test.ts`,
  `mcp-feature-tools.test.ts`, `mcp-isolation.test.ts`, `mcp-server.test.ts`,
  `mcp-update.test.ts`, `mcp.test.ts`, `project-integrations.test.ts`, `slack-media.test.ts`,
  `slack-output.test.ts`, `slack-workflows.test.ts`, `slack.test.ts`, `telegram-media.test.ts`,
  `telegram-output.test.ts`, `telegram-workflows.test.ts`, `telegram.test.ts`,
  `transcription.test.ts`.

- **Harness and Computer**. Retain executable probes, interactive wire frames, sandbox, VNC and latency assertions until their test fixtures and platform capabilities match.
  `acp-harness.test.ts`, `agent-interrupt.test.ts`, `agent-reply-capture.test.ts`,
  `fallback.test.ts`, `gemini.test.ts`, `harness-default.test.ts`, `harness.test.ts`,
  `models.test.ts`, `provider-handoff.test.ts`, `provider-matrix.test.ts`,
  `rust-desktop-http.test.ts`, `rust-latency.test.ts`, `rust-rehearsal.test.ts`,
  `workspace-shell.test.ts`.

- **Local API, CLI and automation**. Keep these feature contracts while their Rust replacements lack assertion-by-assertion parity.
  `agent-delegation.test.ts`, `agent-delete.test.ts`, `agent-messages.test.ts`,
  `agent-result-files.test.ts`, `agent-template.test.ts`, `agent-visibility.test.ts`,
  `artifacts.test.ts`, `attachment-only.test.ts`, `automation-calendar.test.ts`,
  `avatar-tools.test.ts`, `avatars.test.ts`, `base-instructions-scenario.test.ts`,
  `channel-context.test.ts`, `cli.test.ts`, `compaction.test.ts`, `conversation-panel.test.ts`,
  `coordination.test.ts`, `crash.test.ts`, `dispatch.test.ts`, `files.test.ts`,
  `flow-tools.test.ts`, `flows-http.test.ts`, `flows.test.ts`, `goals.test.ts`,
  `history-tools.test.ts`, `history.test.ts`, `html-preview.test.ts`,
  `image-attachment.test.ts`, `image-generation.test.ts`, `local-projects.test.ts`,
  `local-routines.test.ts`, `machines.test.ts`, `memory-selection.test.ts`,
  `memory-tools.test.ts`, `memory.test.ts`, `pdf-text.test.ts`, `profile-tools.test.ts`,
  `provider-account-change.test.ts`, `provider-accounts.test.ts`,
  `provider-settings-tools.test.ts`, `provider.test.ts`, `questions.test.ts`,
  `queue-edits.test.ts`, `queue-order.test.ts`, `remote-agents.test.ts`, `room-chat.test.ts`,
  `routine-tools.test.ts`, `routines.test.ts`, `rust-file-output-panel.test.ts`,
  `rust-files.test.ts`, `rust-workspace-git-http.test.ts`, `rust-workspace-writes-http.test.ts`,
  `script-approvals.test.ts`, `script-discovery.test.ts`, `script-messages.test.ts`,
  `script-provider.test.ts`, `scripts.test.ts`, `self-verification.test.ts`,
  `shared-data-http.test.ts`, `shared-data.test.ts`, `sidebar.test.ts`, `site-tools.test.ts`,
  `sites.test.ts`, `skill-directory.test.ts`, `skill-files.test.ts`, `skill-packs.test.ts`,
  `skill-tools.test.ts`, `skill-zip.test.ts`, `skills.test.ts`, `speech-preference.test.ts`,
  `speech.test.ts`, `t3-connection.test.ts`, `threads.test.ts`, `tool-budget.test.ts`,
  `transcript.test.ts`, `turn-stream.test.ts`, `turn-waits.test.ts`, `usage.test.ts`,
  `webhooks.test.ts`, `workflow-files.test.ts`, `workflow-http.test.ts`,
  `workflow-parallel.test.ts`, `workflow-waits.test.ts`, `workspace-files.test.ts`.

## #89 peers slice: multi-server Node suites to Rust

Ported to `crates/minarbot-e2e/tests/e2e/pr_*.rs`, one Rust test per Node
runtime case. Assertion counts are `assert` calls in the Node case body
against `ensure!` calls in the Rust test; status codes checked inside the
`call`/`api` helpers are not counted on either side (the Rust helpers check
them too). Node servers always had a computer bridge; the Rust tests start
`machine_bridge::MachineBridge` when a case needs `refreshMachines`, a fake
peer token issuer or the manual machine clock, and otherwise let the
servers talk to each other directly. Peer suites use the real clock, as the
Node suites did. Multi-server tests reserve their server slots at once with
`Servers::reserve`, so two such tests cannot deadlock on the two-slot cap.

### `app/machines.test.ts` (9 cases, 9 Rust tests, file deleted)

| Node case | Rust test (`pr_machines::`) | Asserts | Notes |
| --- | --- | --- | --- |
| paired machine recovers after restart while roster sync times out on a shared link | `paired_machine_recovers_after_restart_while_roster_sync_times_out_on_a_shared_link` | 12 -> 12 | The throttled link is a `relay::Relay` with origin rewriting and a paced roster body. |
| peer transport accepts proxy host headers while owner machine routes retain the local host boundary | `peer_transport_accepts_proxy_host_headers_while_owner_machine_routes_retain_the_local_host_boundary` | 8 -> 8 | Raw HTTP/1.1 requests carry the foreign `Host`. |
| two real instances pair mutually, expose scoped cached agents, survive restarts, recover offline, and revoke on unpair without leaking credentials | `two_real_instances_pair_mutually_expose_scoped_cached_agents_survive_restarts_recover_offline_and_revoke_on_unpair_without_leaking_credentials` | 28 -> 28 | Node captured test-process logs; Rust scans the server logs, which the file scan also covers. |
| pairing uses uniform invalid-code errors, strict bodies, one-time expiry, persisted rate limits, and browser-only owner writes | `pairing_uses_uniform_invalid_code_errors_strict_bodies_one_time_expiry_persisted_rate_limits_and_browser_only_owner_writes` | 4 -> 4 | Manual machine clock through the bridge. |
| origins reject unsafe forms; remote pairing rejects redirects, oversized bodies, and credential reflection | `origins_reject_unsafe_forms_remote_pairing_rejects_redirects_oversized_bodies_and_credential_reflection` | 3 -> 3 | No bridge, so the product's own client meets the redirect and size limits. "No issued token" is read from `/api/mcp-server/peer-tokens`. |
| unpair removes first while remote is offline; missing wrapping key fails closed and cached agents remain readable | `unpair_removes_first_while_remote_is_offline_missing_wrapping_key_fails_closed_and_cached_agents_remain_readable` | 3 -> 3 | |
| background health records a different peer version as online and never sends bearer to a changed identity or redirected descriptor | `background_health_records_a_different_peer_version_as_online_and_never_sends_bearer_to_a_changed_identity_or_redirected_descriptor` | 14 -> 15 | Adds a check that only one token was issued. |
| lost reciprocal acknowledgement retries encrypted pending credentials after restart without minting another token | `lost_reciprocal_acknowledgement_retries_encrypted_pending_credentials_after_restart_without_minting_another_token` | 7 -> 7 | The Node in-handler `assert.equal(data.token, saved)` is a recorded mismatch flag. |
| default wiring issues pairing tokens from the Minarbot MCP token registry and revokes them on unpair | `default_wiring_issues_pairing_tokens_from_the_minarbot_mcp_token_registry_and_revokes_them_on_unpair` | 12 -> 12 | |

### `app/remote-agents.test.ts` (15 cases, 14 Rust tests, 2 Node cases kept)

Both servers ask one `async_model::AsyncModel`, the Rust form of the Node
fixture's async provider. The cases that patched `globalThis.fetch` hook the
same peer requests with a `MachineBridge` extra operation.

| Node case | Rust test (`pr_remote_agents::`) | Asserts | Notes |
| --- | --- | --- | --- |
| remote message recovery keeps exact target, ID and payload and rejects corrupt storage | Kept in Node | 6 -> - | Tests `app/src/remote-message-command.ts`. |
| paired real MCP remote agents: message | `paired_real_mcp_remote_agents_message` | 12 -> 12 | Shared helper `paired_real_mcp_remote_agents`. |
| paired real MCP remote agents: wait | `paired_real_mcp_remote_agents_wait` | 12 -> 12 | Failed on product bug P3 (fixed). |
| paired real MCP remote agents: completion | `paired_real_mcp_remote_agents_completion` | 12 -> 12 | Failed on product bug P1 (fixed). |
| paired real MCP remote agents: restart-a | `paired_real_mcp_remote_agents_restart_a` | 12 -> 12 | Failed on P1 (fixed). |
| paired real MCP remote agents: restart-b | `paired_real_mcp_remote_agents_restart_b` | 12 -> 12 | Failed on P1 (fixed). |
| remote cancel stops peer delivery, is peer-scoped, and unpaired addresses are unreachable | `remote_cancel_stops_peer_delivery_is_peer_scoped_and_unpaired_addresses_are_unreachable` | 1 -> 1 | Failed on P1 (fixed). |
| kill switch rejects immediately and loop limits count peer and agent across requests | `kill_switch_rejects_immediately_and_loop_limits_count_peer_and_agent_across_requests` | 12 -> 12 | |
| delegation depth travels across machines and stops ping-pong | `delegation_depth_travels_across_machines_and_stops_ping_pong` | 2 -> 2 | Failed on P1 (fixed). The count leaves out the extra model step after each paused `delegate_task {wait: true}` (B1); the strict `requests === 6` stays in Node. |
| offline send remains explicitly uncertain across caller restart without replay | `offline_send_remains_explicitly_uncertain_across_caller_restart_without_replay` | 2 -> 2 | |
| accepted remote send is never replayed: lost-ack | `accepted_remote_send_is_never_replayed_lost_ack` | 3 -> 3 | Shared helper `accepted_remote_send_is_never_replayed`. |
| accepted remote send is never replayed: restart-sending | `accepted_remote_send_is_never_replayed_restart_sending` | 3 -> 3 | SIGTERM reaches A before the held acknowledgement fails, as in Node. |
| peer tokens never enter database bytes, logs, or outbound message responses | `peer_tokens_never_enter_database_bytes_logs_or_outbound_message_responses` | 4 -> 4 | Tokens are captured from bridge `peer-http` traffic, as Node captured them from `fetch`. |
| kill switch cancels an active peer turn before any more remote work is admitted | `kill_switch_cancels_an_active_peer_turn_before_any_more_remote_work_is_admitted` | 1 -> 1 | |
| remote role reaches local discovery and team prompt through paired peer summaries | `remote_role_reaches_local_discovery_and_team_prompt_through_paired_peer_summaries` | 3 -> 3 | Failed on product bug P2 (fixed). |

### `app/room-chat.test.ts` (25 cases, 25 Rust tests, 1 Node case kept for B1)

| Node case | Rust test (`pr_room_chat::`) | Asserts | Notes |
| --- | --- | --- | --- |
| Section lane waits for committed speaker reply and admits a labelled shared delta | `section_lane_waits_for_committed_speaker_reply_and_admits_a_labelled_shared_delta` | 6 -> 6 | Waits for the held speaker's provider call before the 100 ms quiet window (a harness turn is running before its call starts). |
| room serializes broadcast, records quiet agents, emits named agent mention replies and survives exact retry/restart | `room_serializes_broadcast_records_quiet_agents_emits_named_agent_mention_replies_and_survives_exact_retry_restart` | 13 -> 13 |  |
| @mentions skip provider calls for unmentioned members, including lead | `mentions_skip_provider_calls_for_unmentioned_members_including_lead` | 3 -> 3 |  |
| optional lead breaks all-silent tie once; mention requests never fall back to unmentioned lead | `optional_lead_breaks_all_silent_tie_once_mention_requests_never_fall_back_to_unmentioned_lead` | 4 -> 4 |  |
| room send_message creates visible request/reply before parent answer | `room_send_message_creates_visible_request_reply_before_parent_answer` | 4 -> 4 | Shared helper. |
| room delegate_task creates visible request/reply before parent answer | `room_delegate_task_creates_visible_request_reply_before_parent_answer` | 4 -> 4 | Failed on P1 (fixed). |
| room rounds stop at 3 and reserve at most 10 visible agent messages | `room_rounds_stop_at_3_and_reserve_at_most_10_visible_agent_messages` | 4 -> 4 |  |
| per-agent loop guard stops repeated replies without an unbounded conversation | `per_agent_loop_guard_stops_repeated_replies_without_an_unbounded_conversation` | 2 -> 2 |  |
| room limit preserves the original delegation call and bounded receipt across restart | `room_limit_preserves_the_original_delegation_call_and_bounded_receipt_across_restart` | 5 -> 5 |  |
| restart resumes queued room members without replaying interrupted provider calls | `restart_resumes_queued_room_members_without_replaying_interrupted_provider_calls` | 3 -> 3 | Waits for the held provider call before restarting, the in-flight state the Node case assumed. |
| direct delegation exposes inline transcript request, named reply, machine and activity without replacing final answer | `direct_delegation_exposes_inline_transcript_request_named_reply_machine_and_activity_without_replacing_final_answer` | 5 -> 5 |  |
| new project section serializes with H2 silence and agent mentions, then follows project membership edits | `new_project_section_serializes_with_h2_silence_and_agent_mentions_then_follows_project_membership_edits` | 6 -> 6 | Failed on product bug P5 (fixed). |
| room Stop fences a completed root with live/queued invited members; exact lost-response retry survives restart and new generation | `room_stop_fences_a_completed_root_with_live_queued_invited_members_exact_lost_response_retry_survives_restart_and_new_generation` | 11 -> 11 |  |
| room Stop cancels fan-out, delegated descendants and pending addressed guidance; other roots and direct tasks continue | `room_stop_cancels_fan_out_delegated_descendants_and_pending_addressed_guidance_other_roots_and_direct_tasks_continue` | 4 -> 4 |  |
| addressed API room Steer consumes once at safe point and completion winner queues once; cross-room targets fail | `addressed_api_room_steer_consumes_once_at_safe_point_and_completion_winner_queues_once_cross_room_targets_fail` | 4 -> 4 |  |
| addressed root restart retains peers; Stop before completion prevents final and queued mentions | `addressed_root_restart_retains_peers_stop_before_completion_prevents_final_and_queued_mentions` | 4 -> 4 |  |
| room Stop cancels queued mention invitations before provider admission | `room_stop_cancels_queued_mention_invitations_before_provider_admission` | 2 -> 2 | Waits until the running invitation's provider call arrived before Stop. |
| room Stop rejects foreign origins, bearer callers, payload changes, wrong generation and archived scope | `room_stop_rejects_foreign_origins_bearer_callers_payload_changes_wrong_generation_and_archived_scope` | 2 -> 2 |  |
| serialized broadcast yields send_message to parallel children and resumes once | `serialized_broadcast_yields_send_message_to_parallel_children_and_resumes_once` | 5 -> 5 | Shared helper. |
| serialized broadcast yields delegate_task to parallel children and resumes once | `serialized_broadcast_yields_delegate_task_to_parallel_children_and_resumes_once` | 5 -> 5 | The Assistant count leaves out the step after the paused delegation (B1); the strict count stays in Node. The gateway peer waits a `delegate_task {wait: true}` with its own `wait_task` (G13), so the journal holds those two calls once each instead of one row. |
| separate Sections and direct chats progress while root order holds within one Section | `separate_sections_and_direct_chats_progress_while_root_order_holds_within_one_section` | 2 -> 2 |  |
| mention storms deduplicate sources, settle pass obligations and never invite self | `mention_storms_deduplicate_sources_settle_pass_obligations_and_never_invite_self` | 5 -> 5 |  |
| same-name Unicode speakers retain distinct IDs in bounded committed context; user brief survives truncation | `same_name_unicode_speakers_retain_distinct_ids_in_bounded_committed_context_user_brief_survives_truncation` | 7 -> 7 | Both speaker IDs are checked in one loop. |
| failure releases serialized lane without lead fallback or provider replay | `failure_releases_serialized_lane_without_lead_fallback_or_provider_replay` | 3 -> 3 | Kit fix: `fake-gateway` now fails the turn on a tool call without a function name, as Codex does; it used to send it to MCP and loop to the tool budget (93 calls). |
| admission cutoff and unpublished watermarks survive restarts | `admission_cutoff_and_unpublished_watermarks_survive_restarts` | 8 -> 8 | Waits for the gated peer's provider call before stopping. |

### `app/hub-link.test.ts` (41 cases, 39 Rust tests, 2 Node cases kept)

A real hosted Hub (`minarbot-server --hub test-fixture`) runs with a manual
clock. Node servers enroll through browser approval and reach it over real
WebSockets through `hosted_hub::HubTransport` (one socket set per node, so
stopping or crashing a node closes its links as the Node bridge did).
Received frames are rewritten with `receive_frames`, and sent frames are
held, dropped or followed by a close with `intercept_sends`, replacing the
Node `receive` hook and `WebSocket.prototype.send` patches. The shared
`node()` enrollment helper carries the Node fixture's 9 assertions.

| Node case | Rust test | Asserts | Notes |
| --- | --- | --- | --- |
| signed envelopes reject forged payloads, keys and directions | Kept in Node | 3 -> - | Tests the TypeScript helper `app/test-support/hub-node-signature.ts`. Forged hub signatures against the product are covered by `foreign_project_and_forged_signed_hub_frames_never_reach_local_engine`. |
| browser-approved enrollment, two real local engines, attribution and local key custody | `pr_hub::cases::browser_approved_enrollment_two_real_local_engines_attribution_and_local_key_custody` | 11 -> 11 |  |
| owner-only default, specific-member and all-member grants; offline refuses without hub execution | `pr_hub::cases::owner_only_default_specific_member_and_all_member_grants_offline_refuses_without_hub_execution` | 4 -> 4 |  |
| dangerous tool waits for exact local owner approval; hub/browser roles cannot approve it | `pr_hub::cases::dangerous_tool_waits_for_exact_local_owner_approval_hub_browser_roles_cannot_approve_it` | 8 -> 8 |  |
| duplicate delivery, final ACK loss and restart never execute twice | `pr_hub::cases::duplicate_delivery_final_ack_loss_and_restart_never_execute_twice` | 4 -> 4 |  |
| revocation mid-stream stops local work before another effect and does not execute offline | `pr_hub::cases::revocation_mid_stream_stops_local_work_before_another_effect_and_does_not_execute_offline` | 3 -> 3 |  |
| foreign project and forged signed hub frames never reach local engine | `pr_hub::cases::foreign_project_and_forged_signed_hub_frames_never_reach_local_engine` | 1 -> 1 |  |
| local owner endpoints require exact Origin and refuse bearer callers; unenrolled app stays idle | `pr_hub::cases::local_owner_endpoints_require_exact_origin_and_refuse_bearer_callers_unenrolled_app_stays_idle` | 4 -> 4 |  |
| node revocation drains its signed frame before closing transport | `pr_hub::cases::node_revocation_drains_its_signed_frame_before_closing_transport` | 3 -> 3 |  |
| SIGKILL after dispatch and before result ACK keeps durable uncertainty without replay | `pr_hub::cases::sigkill_after_dispatch_and_before_result_ack_keeps_durable_uncertainty_without_replay` | 2 -> 2 |  |
| provider partial snapshots stream over real WebSockets before final | `pr_hub::cases::provider_partial_snapshots_stream_over_real_websockets_before_final` | 4 -> 4 |  |
| browser code alone cannot redeem enrollment; node upgrade rejects browser credentials | `pr_hub::cases::browser_code_alone_cannot_redeem_enrollment_node_upgrade_rejects_browser_credentials` | 2 -> 2 |  |
| reconnect fences old generation; session expiry renews with node key and never replays completed work | `pr_hub::cases::reconnect_fences_old_generation_session_expiry_renews_with_node_key_and_never_replays_completed_work` | 3 -> 3 |  |
| local provider spend ceiling refuses next request before dispatch | Kept in Node | 1 -> - | Open product gap P6, needs an Owner decision. |
| lost enrollment redemption response recovers with signed node proof | `pr_hub::cases::lost_enrollment_redemption_response_recovers_with_signed_node_proof` | 2 -> 2 |  |
| changed local agent policy rejects an existing hub grant before provider dispatch | `pr_hub::cases::changed_local_agent_policy_rejects_an_existing_hub_grant_before_provider_dispatch` | 1 -> 1 |  |
| revoking alpha preserves simultaneous beta work on the same node | `pr_hub::cases::revoking_alpha_preserves_simultaneous_beta_work_on_the_same_node` | 5 -> 5 |  |
| offline local withdrawal survives restart and reaches hub before reconnect admits work | `pr_hub::cases::offline_local_withdrawal_survives_restart_and_reaches_hub_before_reconnect_admits_work` | 2 -> 2 |  |
| hub-side withdrawal waits offline beyond turn deadline and disables local grant before new work | `pr_hub::cases::hub_side_withdrawal_waits_offline_beyond_turn_deadline_and_disables_local_grant_before_new_work` | 2 -> 2 |  |
| withdrawal ACK loss retries same revision and cannot withdraw another node or newer consent | `pr_hub::cases::withdrawal_ack_loss_retries_same_revision_and_cannot_withdraw_another_node_or_newer_consent` | 5 -> 5 |  |
| accounts enrollment routes reject project cookies, bad CSRF, fingerprint mismatch and account overrides | `pr_hub::cases::accounts_enrollment_routes_reject_project_cookies_bad_csrf_fingerprint_mismatch_and_account_overrides` | 2 -> 2 |  |
| computer input requires local approval and action lease; revoke leaves uncertain input without replay | `pr_hub::effects::computer_input_requires_local_approval_and_action_lease_revoke_leaves_uncertain_input_without_replay` | 8 -> 8 | The lease directory sits in a short temporary folder: its proxy socket path must stay under the 108-byte Unix socket limit, as the Node suite's short folder did. |
| connected app calls require local approval and lease; revocation after dispatch is uncertain and never replayed | `pr_hub::effects::connected_app_calls_require_local_approval_and_lease_revocation_after_dispatch_is_uncertain_and_never_replayed` | 7 -> 6 | The Node fixture's `assert(address ...)` has no Rust counterpart. |
| MCP async discovery cannot dispatch after revoke; lost result remains uncertain across restart | `pr_hub::effects::mcp_async_discovery_cannot_dispatch_after_revoke_lost_result_remains_uncertain_across_restart` | 6 -> 5 | The Node fixture's `assert(address ...)` has no Rust counterpart. |
| codex: unsupported effect fencing never executes hub turns | `pr_hub::effects::codex_unsupported_effect_fencing_never_executes_hub_turns` | 6 -> 5 | #87 removed `agents.runtime`, so no agent can run a native runtime per hub turn. The surviving fence is the local engine refusal: the agent is hidden locally after its grant (policy digest unchanged). The advertised runtime fence, the failed turn code and zero provider calls (also after restart) are checked; the session-mint trap cannot fire without a native runtime. |
| claude: unsupported effect fencing never executes hub turns | `pr_hub::effects::claude_unsupported_effect_fencing_never_executes_hub_turns` | 6 -> 5 | #87 removed `agents.runtime`, so no agent can run a native runtime per hub turn. The surviving fence is the local engine refusal: the agent is hidden locally after its grant (policy digest unchanged). The advertised runtime fence, the failed turn code and zero provider calls (also after restart) are checked; the session-mint trap cannot fire without a native runtime. |
| hermes: unsupported effect fencing never executes hub turns | `pr_hub::effects::hermes_unsupported_effect_fencing_never_executes_hub_turns` | 6 -> 5 | #87 removed `agents.runtime`, so no agent can run a native runtime per hub turn. The surviving fence is the local engine refusal: the agent is hidden locally after its grant (policy digest unchanged). The advertised runtime fence, the failed turn code and zero provider calls (also after restart) are checked; the session-mint trap cannot fire without a native runtime. |
| antigravity: unsupported effect fencing never executes hub turns | `pr_hub::effects::antigravity_unsupported_effect_fencing_never_executes_hub_turns` | 6 -> 5 | #87 removed `agents.runtime`, so no agent can run a native runtime per hub turn. The surviving fence is the local engine refusal: the agent is hidden locally after its grant (policy digest unchanged). The advertised runtime fence, the failed turn code and zero provider calls (also after restart) are checked; the session-mint trap cannot fire without a native runtime. |
| grok: unsupported effect fencing never executes hub turns | `pr_hub::effects::grok_unsupported_effect_fencing_never_executes_hub_turns` | 6 -> 5 | #87 removed `agents.runtime`, so no agent can run a native runtime per hub turn. The surviving fence is the local engine refusal: the agent is hidden locally after its grant (policy digest unchanged). The advertised runtime fence, the failed turn code and zero provider calls (also after restart) are checked; the session-mint trap cannot fire without a native runtime. |
| pi: unsupported effect fencing never executes hub turns | `pr_hub::effects::pi_unsupported_effect_fencing_never_executes_hub_turns` | 6 -> 5 | #87 removed `agents.runtime`, so no agent can run a native runtime per hub turn. The surviving fence is the local engine refusal: the agent is hidden locally after its grant (policy digest unchanged). The advertised runtime fence, the failed turn code and zero provider calls (also after restart) are checked; the session-mint trap cannot fire without a native runtime. |
| opencode: unsupported effect fencing never executes hub turns | `pr_hub::effects::opencode_unsupported_effect_fencing_never_executes_hub_turns` | 6 -> 5 | #87 removed `agents.runtime`, so no agent can run a native runtime per hub turn. The surviving fence is the local engine refusal: the agent is hidden locally after its grant (policy digest unchanged). The advertised runtime fence, the failed turn code and zero provider calls (also after restart) are checked; the session-mint trap cannot fire without a native runtime. |
| gemini: unsupported effect fencing never executes hub turns | `pr_hub::effects::gemini_unsupported_effect_fencing_never_executes_hub_turns` | 6 -> 5 | #87 removed `agents.runtime`, so no agent can run a native runtime per hub turn. The surviving fence is the local engine refusal: the agent is hidden locally after its grant (policy digest unchanged). The advertised runtime fence, the failed turn code and zero provider calls (also after restart) are checked; the session-mint trap cannot fire without a native runtime. |
| kiro: unsupported effect fencing never executes hub turns | `pr_hub::effects::kiro_unsupported_effect_fencing_never_executes_hub_turns` | 6 -> 5 | #87 removed `agents.runtime`, so no agent can run a native runtime per hub turn. The surviving fence is the local engine refusal: the agent is hidden locally after its grant (policy digest unchanged). The advertised runtime fence, the failed turn code and zero provider calls (also after restart) are checked; the session-mint trap cannot fire without a native runtime. |
| two session-enrolled nodes answer browser mention; revoking one live stream preserves other node | `pr_hub::cases::two_session_enrolled_nodes_answer_browser_mention_revoking_one_live_stream_preserves_other_node` | 6 -> 5 | The Node `assert(revokedTurn && otherTurn)` is an error return. |
| offline withdrawal is acknowledged even after owner loses project membership | `pr_hub::cases::offline_withdrawal_is_acknowledged_even_after_owner_loses_project_membership` | 3 -> 3 |  |
| project queue admission is atomic, idempotent and expires without executing offline work | `pr_hub::cases::project_queue_admission_is_atomic_idempotent_and_expires_without_executing_offline_work` | 5 -> 5 |  |
| connected app resume requires exactly one ready action before issuing any lease | `pr_hub::cases::connected_app_resume_requires_exactly_one_ready_action_before_issuing_any_lease` | 3 -> 3 |  |
| human post rate leaves capacity for withdrawal controls | `pr_hub::cases::human_post_rate_leaves_capacity_for_withdrawal_controls` | 2 -> 2 |  |
| withdrawal drains its control frame before queued section delivery closes transport | `pr_hub::cases::withdrawal_drains_its_control_frame_before_queued_section_delivery_closes_transport` | 3 -> 3 |  |
| signed pending requests stay node-scoped; approval, stale revision, offline decline and reconnect preserve consent | `pr_hub::cases::signed_pending_requests_stay_node_scoped_approval_stale_revision_offline_decline_and_reconnect_preserve_consent` | 20 -> 20 |  |
| local owner deletes linked agent, withdraws hub participation and removes local transport journals | `pr_hub::cases::local_owner_deletes_linked_agent_withdraws_hub_participation_and_removes_local_transport_journals` | 4 -> 4 |  |

### `app/two-process.test.ts` (100 tests and 3 loops, 132 runtime cases, 132 Rust tests, file deleted)

`pr_tp::fixture::Tp` is the Node `fixture()`: a canonical-mode server with
Clerk session tokens (`hosted::HostedApp`) and a device fetch proxy
(`relay::Relay`) in front of it. The Node suite patched `globalThis.fetch`
to lose, hold, rewrite or delay device requests; those patches are
`Fault` hooks on the proxy (`Drop`, `LoseReply`, `Rewrite`, `Hold`,
`After`). Test-control hosts (`device_host::DeviceHost`), standalone
connections (`ConnectionProcess`) and `run-task` processes (`RustTask`)
run the same `minarbot-server --device-host` binary as the Node helpers.
Lost replies to the test's own requests are sent and their answer
ignored. Loop iterations and subtests are separate Rust tests; their Node
count is the shared case body. The Rust count adds the checks of the
module helpers a test calls, once per call site, and the status checks of
`Tp::project`, `Tp::agent` and `Tp::task`. Node `assert.fail` guards in
effects that must not run became effects that fail the call and are not
counted. `app/test-support/rust-device-host.ts` was used only by this
suite and is deleted with it.

| Node case | Rust test (`pr_tp::`) | Asserts | Notes |
| --- | --- | --- | --- |
| protected turn event verifies on home, saves one receipt and resumes with exact raw payload | `events::protected_turn_event_verifies_on_home_saves_one_receipt_and_resumes_with_exact_raw_payload` | 31 -> 41 |  |
| protected turn event rejects delayed verification after scope or connection changes: pause | `events::protected_turn_event_rejects_delayed_verification_after_scope_or_connection_changes_pause` | 8 -> 13 | Subtest; the Node count is the shared case body. |
| protected turn event rejects delayed verification after scope or connection changes: rules | `events::protected_turn_event_rejects_delayed_verification_after_scope_or_connection_changes_rules` | 8 -> 13 | Subtest; the Node count is the shared case body. |
| protected turn event rejects delayed verification after scope or connection changes: cancel | `events::protected_turn_event_rejects_delayed_verification_after_scope_or_connection_changes_cancel` | 8 -> 13 | Subtest; the Node count is the shared case body. |
| protected turn event rejects delayed verification after scope or connection changes: revoke | `events::protected_turn_event_rejects_delayed_verification_after_scope_or_connection_changes_revoke` | 8 -> 13 | Subtest; the Node count is the shared case body. |
| protected turn event rejects delayed verification after scope or connection changes: timeout | `events::protected_turn_event_rejects_delayed_verification_after_scope_or_connection_changes_timeout` | 8 -> 13 | Subtest; the Node count is the shared case body. |
| protected turn wait uses outbound stream and recovers lost host ACK without another key or wait | `events::protected_turn_wait_uses_outbound_stream_and_recovers_lost_host_ack_without_another_key_or_wait` | 42 -> 53 |  |
| canonical shared data isolates project images and recovers one write after lost reply and restart | `shared::canonical_shared_data_isolates_project_images_and_recovers_one_write_after_lost_reply_and_restart` | 18 -> 30 |  |
| canonical avatar removal recovers its original result after a lost reply, later image and restart | `shared::canonical_avatar_removal_recovers_its_original_result_after_a_lost_reply_later_image_and_restart` | 13 -> 12 |  |
| canonical site management keeps project scope and recovers one deletion after a lost reply and restart | `shared::canonical_site_management_keeps_project_scope_and_recovers_one_deletion_after_a_lost_reply_and_restart` | 14 -> 11 |  |
| canonical questions survive lost RPC and answer replies, restart and home outage without duplicate work | `shared::canonical_questions_survive_lost_rpc_and_answer_replies_restart_and_home_outage_without_duplicate_work` | 28 -> 26 |  |
| protected workflow connection reveals only ciphertext to its current owner browser | `workflow_waits::protected_workflow_connection_reveals_only_ciphertext_to_its_current_owner_browser` | 17 -> 20 |  |
| protected workflow event commits one transition and keeps exact retries after advancement restart and restore | `workflow_waits::protected_workflow_event_commits_one_transition_and_keeps_exact_retries_after_advancement_restart_and_restore` | 29 -> 35 |  |
| protected workflow wait discards delayed reveal and event replies after authority changes: reveal/revision | `workflow_waits::protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_reveal_revision` | 10 -> 15 | Subtest; the Node count is the shared case body. |
| protected workflow wait discards delayed reveal and event replies after authority changes: reveal/rules | `workflow_waits::protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_reveal_rules` | 10 -> 15 | Subtest; the Node count is the shared case body. |
| protected workflow wait discards delayed reveal and event replies after authority changes: reveal/assignment | `workflow_waits::protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_reveal_assignment` | 10 -> 15 | Subtest; the Node count is the shared case body. |
| protected workflow wait discards delayed reveal and event replies after authority changes: reveal/mapping | `workflow_waits::protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_reveal_mapping` | 10 -> 15 | Subtest; the Node count is the shared case body. |
| protected workflow wait discards delayed reveal and event replies after authority changes: reveal/cancel | `workflow_waits::protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_reveal_cancel` | 10 -> 15 | Subtest; the Node count is the shared case body. |
| protected workflow wait discards delayed reveal and event replies after authority changes: reveal/expiry | `workflow_waits::protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_reveal_expiry` | 10 -> 15 | Subtest; the Node count is the shared case body. |
| protected workflow wait discards delayed reveal and event replies after authority changes: reveal/revoke | `workflow_waits::protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_reveal_revoke` | 10 -> 15 | Subtest; the Node count is the shared case body. |
| protected workflow wait discards delayed reveal and event replies after authority changes: reveal/disconnect | `workflow_waits::protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_reveal_disconnect` | 10 -> 15 | Subtest; the Node count is the shared case body. |
| protected workflow wait discards delayed reveal and event replies after authority changes: event/revision | `workflow_waits::protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_event_revision` | 10 -> 15 | Subtest; the Node count is the shared case body. |
| protected workflow wait discards delayed reveal and event replies after authority changes: event/rules | `workflow_waits::protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_event_rules` | 10 -> 15 | Subtest; the Node count is the shared case body. |
| protected workflow wait discards delayed reveal and event replies after authority changes: event/assignment | `workflow_waits::protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_event_assignment` | 10 -> 15 | Subtest; the Node count is the shared case body. |
| protected workflow wait discards delayed reveal and event replies after authority changes: event/mapping | `workflow_waits::protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_event_mapping` | 10 -> 15 | Subtest; the Node count is the shared case body. |
| protected workflow wait discards delayed reveal and event replies after authority changes: event/cancel | `workflow_waits::protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_event_cancel` | 10 -> 15 | Subtest; the Node count is the shared case body. |
| protected workflow wait discards delayed reveal and event replies after authority changes: event/expiry | `workflow_waits::protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_event_expiry` | 10 -> 15 | Subtest; the Node count is the shared case body. |
| protected workflow wait discards delayed reveal and event replies after authority changes: event/revoke | `workflow_waits::protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_event_revoke` | 10 -> 15 | Subtest; the Node count is the shared case body. |
| protected workflow wait discards delayed reveal and event replies after authority changes: event/disconnect | `workflow_waits::protected_workflow_wait_discards_delayed_reveal_and_event_replies_after_authority_changes_event_disconnect` | 10 -> 15 | Subtest; the Node count is the shared case body. |
| canonical workflow schedule coalesces offline intervals with one durable identity across restart | `schedules::canonical_workflow_schedule_coalesces_offline_intervals_with_one_durable_identity_across_restart` | 18 -> 20 |  |
| canonical workflow once uses its home zone and consumes one catch-up after offline restart | `schedules::canonical_workflow_once_uses_its_home_zone_and_consumes_one_catch_up_after_offline_restart` | 16 -> 18 |  |
| canonical workflow cron validates HTTP input and admits one retained overdue occurrence after reconnect | `schedules::canonical_workflow_cron_validates_http_input_and_admits_one_retained_overdue_occurrence_after_reconnect` | 16 -> 18 |  |
| canonical workflow occurrence attaches its first recovery task atomically on the original home | `schedules::canonical_workflow_occurrence_attaches_its_first_recovery_task_atomically_on_the_original_home` | 11 -> 12 |  |
| workflow schedule sync retains scoped metadata and exact deltas across restart without replaying commands | `schedules::workflow_schedule_sync_retains_scoped_metadata_and_exact_deltas_across_restart_without_replaying_commands` | 13 -> 13 |  |
| workflow schedule sync rechecks current assignment private versions and root visibility for every human | `schedules::workflow_schedule_sync_rechecks_current_assignment_private_versions_and_root_visibility_for_every_human` | 13 -> 13 |  |
| canonical workflow schedule rejects foreign scope and rechecks current assignment before receipt replay | `schedules::canonical_workflow_schedule_rejects_foreign_scope_and_rechecks_current_assignment_before_receipt_replay` | 10 -> 10 |  |
| canonical workflow executes conditions and approval once across restart | `workflows::canonical_workflow_executes_conditions_and_approval_once_across_restart` | 10 -> 10 |  |
| canonical workflow admits isolated home tasks and keeps model nodes tool-free | `workflows::canonical_workflow_admits_isolated_home_tasks_and_keeps_model_nodes_tool_free` | 21 -> 21 |  |
| canonical workflow retry preserves topology and failed evidence while using current node settings once | `workflows::canonical_workflow_retry_preserves_topology_and_failed_evidence_while_using_current_node_settings_once` | 24 -> 25 |  |
| canonical workflow retry changes agent per attempt and falls back to the original node after removal | `workflows::canonical_workflow_retry_changes_agent_per_attempt_and_falls_back_to_the_original_node_after_removal` | 18 -> 20 |  |
| canonical workflow retry checks capacity scopes and original authority with atomic receipts | `workflows::canonical_workflow_retry_checks_capacity_scopes_and_original_authority_with_atomic_receipts` | 18 -> 18 |  |
| canonical workflow dispatch retains original collaborator and owner cancellation fences revoked work | `workflows::canonical_workflow_dispatch_retains_original_collaborator_and_owner_cancellation_fences_revoked_work` | 11 -> 13 |  |
| canonical workflow completion recovers a lost ACK after advancing to approval | `workflows::canonical_workflow_completion_recovers_a_lost_ack_after_advancing_to_approval` | 9 -> 12 |  |
| canonical workflow cancellation fences no-reply descendants of completed steps | `workflows::canonical_workflow_cancellation_fences_no_reply_descendants_of_completed_steps` | 10 -> 12 |  |
| canonical workflow oversized handoff fails once without starving later runs | `workflows::canonical_workflow_oversized_handoff_fails_once_without_starving_later_runs` | 7 -> 9 |  |
| canonical workflow capacity retries leave no empty threads before admission | `workflow_runs::canonical_workflow_capacity_retries_leave_no_empty_threads_before_admission` | 9 -> 15 |  |
| canonical workflow revoked assignment before host ACK closes the original run | `workflow_runs::canonical_workflow_revoked_assignment_before_host_ack_closes_the_original_run` | 7 -> 10 |  |
| canonical workflow wait reserves once offline and activates only after home keyring acknowledgement | `workflow_runs::canonical_workflow_wait_reserves_once_offline_and_activates_only_after_home_keyring_acknowledgement` | 18 -> 22 |  |
| canonical workflow timeout fails offline reservation without generating a second wait | `workflow_runs::canonical_workflow_timeout_fails_offline_reservation_without_generating_a_second_wait` | 11 -> 12 |  |
| canonical workflow channel output is atomic and does not duplicate after restart | `workflow_runs::canonical_workflow_channel_output_is_atomic_and_does_not_duplicate_after_restart` | 9 -> 9 |  |
| canonical workflow run admission keeps its graph and receipt across restart and revision changes | `workflow_runs::canonical_workflow_run_admission_keeps_its_graph_and_receipt_across_restart_and_revision_changes` | 22 -> 22 | Node failed at base (409 "Run has stopped."): the scheduler settles a run as soon as it is admitted. A `record_scopes` trigger (`HOLD_STEPS`) holds step execution. |
| canonical workflow runs deny revoked private and unmapped state and roll back controls with receipts | `workflow_runs::canonical_workflow_runs_deny_revoked_private_and_unmapped_state_and_roll_back_controls_with_receipts` | 26 -> 26 | Node failed at base (UNIQUE `workflow_steps`), for the same reason; held with `HOLD_STEPS`, released only around the manual step mapping. |
| canonical workflow run and step pages preserve old approvals and admission rolls back failed receipts | `workflow_runs::canonical_workflow_run_and_step_pages_preserve_old_approvals_and_admission_rolls_back_failed_receipts` | 19 -> 19 |  |
| canonical routine manual run waits for its home and recovers lost replies and restart without moving schedule | `routines::canonical_routine_manual_run_waits_for_its_home_and_recovers_lost_replies_and_restart_without_moving_schedule` | 25 -> 25 |  |
| canonical routine manual admission enforces current authority private scopes closed requests and receipt conflicts | `routines::canonical_routine_manual_admission_enforces_current_authority_private_scopes_closed_requests_and_receipt_conflicts` | 16 -> 16 |  |
| canonical routine configuration and flow stay scoped across projects restart and lost replies | `routines::canonical_routine_configuration_and_flow_stay_scoped_across_projects_restart_and_lost_replies` | 27 -> 27 |  |
| canonical routine runtime coalesces offline intervals once and retains task receipts across restart | `routines::canonical_routine_runtime_coalesces_offline_intervals_once_and_retains_task_receipts_across_restart` | 27 -> 28 |  |
| canonical routine runtime snapshots flows and hands off once using current home authority | `routines::canonical_routine_runtime_snapshots_flows_and_hands_off_once_using_current_home_authority` | 19 -> 21 |  |
| canonical routine no_update recovers terminal result after home SIGKILL and server restart without fan-out | `routines::canonical_routine_no_update_recovers_terminal_result_after_home_sigkill_and_server_restart_without_fan_out` | 17 -> 16 | Loop iteration; the Node count is the shared case body. |
| canonical routine report_failure recovers terminal result after home SIGKILL and server restart without fan-out | `routines::canonical_routine_report_failure_recovers_terminal_result_after_home_sigkill_and_server_restart_without_fan_out` | 17 -> 16 | Loop iteration; the Node count is the shared case body. |
| canonical routine mixed fan-in waits for success and keeps quiet and failed outcomes distinct | `routines::canonical_routine_mixed_fan_in_waits_for_success_and_keeps_quiet_and_failed_outcomes_distinct` | 16 -> 16 |  |
| canonical routine keeps no-reply descendants executable after completion and fences revoked root authority | `routines::canonical_routine_keeps_no_reply_descendants_executable_after_completion_and_fences_revoked_root_authority` | 5 -> 6 |  |
| canonical routine runtime rechecks grants and rolls back failed task admissions without consuming occurrence | `routines::canonical_routine_runtime_rechecks_grants_and_rolls_back_failed_task_admissions_without_consuming_occurrence` | 12 -> 14 |  |
| canonical routine edit cancels unstarted scheduled work and retains accepted evidence | `routines::canonical_routine_edit_cancels_unstarted_scheduled_work_and_retains_accepted_evidence` | 6 -> 7 |  |
| canonical routine runtime preserves started snapshots through edits and restore holds unfinished occurrences | `routines::canonical_routine_runtime_preserves_started_snapshots_through_edits_and_restore_holds_unfinished_occurrences` | 13 -> 14 |  |
| canonical routine cancellation settles terminal occurrence after task cancel | `routines::canonical_routine_cancellation_settles_terminal_occurrence_after_task_cancel` | 5 -> 6 |  |
| canonical routines recheck assignments private mappings flow agents and receipt rollback | `routines::canonical_routines_recheck_assignments_private_mappings_flow_agents_and_receipt_rollback` | 18 -> 18 |  |
| canonical routine wall-clock HTTP keeps exact retries and pending identities across restart and catch-up admission | `routines::canonical_routine_wall_clock_http_keeps_exact_retries_and_pending_identities_across_restart_and_catch_up_admission` | 33 -> 34 | Node used the ambient `TZ` and the TypeScript `onceCandidate`. The server and home run in `Asia/Jakarta` (fixed +07:00), so the candidate is computed directly. |
| canonical routine calendar exports only current project and reader schedules without prompts | `routines::canonical_routine_calendar_exports_only_current_project_and_reader_schedules_without_prompts` | 22 -> 22 |  |
| canonical workflow configuration stays project scoped with immutable versions and retry receipts | `workflow_config::canonical_workflow_configuration_stays_project_scoped_with_immutable_versions_and_retry_receipts` | 24 -> 24 |  |
| canonical workflows enforce current assignments private mappings selectors and atomic receipt storage | `workflow_config::canonical_workflows_enforce_current_assignments_private_mappings_selectors_and_atomic_receipt_storage` | 33 -> 33 |  |
| canonical workflow pages bound graph bytes and preserve list and version cursors | `workflow_config::canonical_workflow_pages_bound_graph_bytes_and_preserve_list_and_version_cursors` | 11 -> 11 |  |
| canonical workflow inbox keeps read state per authorized human across retry restart and deletion | `workflow_config::canonical_workflow_inbox_keeps_read_state_per_authorized_human_across_retry_restart_and_deletion` | 26 -> 26 |  |
| canonical workflow inbox hides foreign selectors and rechecks current reader scopes before receipt replay | `workflow_config::canonical_workflow_inbox_hides_foreign_selectors_and_rechecks_current_reader_scopes_before_receipt_replay` | 26 -> 26 |  |
| canonical workflow inbox pages twenty outputs without duplicate completion or recipient backfill | `sync::canonical_workflow_inbox_pages_twenty_outputs_without_duplicate_completion_or_recipient_backfill` | 18 -> 20 |  |
| workflow sync publishes metadata and revisioned run transitions once across retry and restart | `sync::workflow_sync_publishes_metadata_and_revisioned_run_transitions_once_across_retry_and_restart` | 14 -> 14 |  |
| workflow sync includes protected wait activation delivery and expiry without secret references | `sync::workflow_sync_includes_protected_wait_activation_delivery_and_expiry_without_secret_references` | 12 -> 15 |  |
| workflow sync rolls back commands with event failures and retains deletion tombstones | `sync::workflow_sync_rolls_back_commands_with_event_failures_and_retains_deletion_tombstones` | 18 -> 18 |  |
| workflow sync rechecks assignment project and private root version and run scopes | `sync::workflow_sync_rechecks_assignment_project_and_private_root_version_and_run_scopes` | 14 -> 14 |  |
| signed human sync preserves delta cursors through canonical restart and rejects foreign or revoked reads | `sync::signed_human_sync_preserves_delta_cursors_through_canonical_restart_and_rejects_foreign_or_revoked_reads` | 14 -> 14 |  |
| signed task and device mutations publish atomic scoped observations across retry and restart | `sync::signed_task_and_device_mutations_publish_atomic_scoped_observations_across_retry_and_restart` | 21 -> 36 |  |
| home execution loop publishes one provider answer through signed canonical HTTP | `execution::home_execution_loop_publishes_one_provider_answer_through_signed_canonical_http` | 7 -> 11 |  |
| home execution loop saves setup tools, pauses for answers and continues without repeating saved provider output | `execution::home_execution_loop_saves_setup_tools_pauses_for_answers_and_continues_without_repeating_saved_provider_output` | 10 -> 13 |  |
| home execution loop rejects malformed or unadvertised provider outputs and never retries uncertain effects | `execution::home_execution_loop_rejects_malformed_or_unadvertised_provider_outputs_and_never_retries_uncertain_effects` | 5 -> 12 |  |
| home execution loop resumes saved provider output after completion reply loss without another provider call | `execution::home_execution_loop_resumes_saved_provider_output_after_completion_reply_loss_without_another_provider_call` | 7 -> 13 |  |
| home execution loop survives SIGKILL across provider and tool results without replaying effects | `execution::home_execution_loop_survives_sigkill_across_provider_and_tool_results_without_replaying_effects` | 7 -> 10 |  |
| home execution loop uses explicit uncertainty retry reservation and never retries before that decision | `execution::home_execution_loop_uses_explicit_uncertainty_retry_reservation_and_never_retries_before_that_decision` | 11 -> 17 |  |
| home execution loop consumes resolved late-known output without repeating its provider call | `execution::home_execution_loop_consumes_resolved_late_known_output_without_repeating_its_provider_call` | 13 -> 19 |  |
| home execution loop persists provider wait across reply loss and preserves concurrent pause | `execution::home_execution_loop_persists_provider_wait_across_reply_loss_and_preserves_concurrent_pause` | 11 -> 20 |  |
| home execution loop holds missing bindings and enforces provider slots before dispatch | `execution::home_execution_loop_holds_missing_bindings_and_enforces_provider_slots_before_dispatch` | 12 -> 52 |  |
| canonical completion recovery handles request without another provider call | `completion::canonical_completion_recovery_handles_request_without_another_provider_call` | 19 -> 26 | Loop iteration; the Node count is the shared case body. |
| canonical completion recovery handles reply without another provider call | `completion::canonical_completion_recovery_handles_reply_without_another_provider_call` | 19 -> 26 | Loop iteration; the Node count is the shared case body. |
| canonical completion recovery handles receipt without another provider call | `completion::canonical_completion_recovery_handles_receipt_without_another_provider_call` | 19 -> 26 | Loop iteration; the Node count is the shared case body. |
| canonical completion recovery handles revoked without another provider call | `completion::canonical_completion_recovery_handles_revoked_without_another_provider_call` | 19 -> 26 | Loop iteration; the Node count is the shared case body. |
| canonical completion recovery handles storage without another provider call | `completion::canonical_completion_recovery_handles_storage_without_another_provider_call` | 19 -> 26 | Loop iteration; the Node count is the shared case body. |
| canonical completion enforces pause through signed HTTP | `completion::canonical_completion_enforces_pause_through_signed_http` | 20 -> 22 | Loop iteration; the Node count is the shared case body. |
| canonical completion enforces cancel through signed HTTP | `completion::canonical_completion_enforces_cancel_through_signed_http` | 20 -> 22 | Loop iteration; the Node count is the shared case body. |
| canonical completion enforces latest through signed HTTP | `completion::canonical_completion_enforces_latest_through_signed_http` | 20 -> 22 | Loop iteration; the Node count is the shared case body. |
| canonical completion enforces invalid through signed HTTP | `completion::canonical_completion_enforces_invalid_through_signed_http` | 20 -> 22 | Loop iteration; the Node count is the shared case body. |
| canonical completion enforces setup through signed HTTP | `completion::canonical_completion_enforces_setup_through_signed_http` | 20 -> 22 | Loop iteration; the Node count is the shared case body. |
| canonical completion enforces revoked through signed HTTP | `completion::canonical_completion_enforces_revoked_through_signed_http` | 20 -> 22 | Loop iteration; the Node count is the shared case body. |
| canonical listener rejects a second server on the same database and releases lock on close | `devices::canonical_listener_rejects_a_second_server_on_the_same_database_and_releases_lock_on_close` | 5 -> 5 | The Node launcher mapped "Another canonical server is still running." to `canonical_server_busy`; the Rust test reads that startup error. The alias is a symlinked database in a second test root. |
| cross-device reassign survives lost reply and restart, preserves old results and waits for fixed recipient home | `devices::cross_device_reassign_survives_lost_reply_and_restart_preserves_old_results_and_waits_for_fixed_recipient_home` | 17 -> 25 |  |
| local possession gate enrolls only configured server and retains identity across reconnect/revoke | `devices::local_possession_gate_enrolls_only_configured_server_and_retains_identity_across_reconnect_revoke` | 25 -> 28 | The large avatar is a 256x256 noise PNG (`noise_image::noise_png`) instead of `sharp` on random bytes. |
| device key loss fails closed and concurrent stream replacement fences old heartbeat | `devices::device_key_loss_fails_closed_and_concurrent_stream_replacement_fences_old_heartbeat` | 4 -> 8 |  |
| lost enrollment ACK resumes same request and explicit reenroll/rotation preserve home identity | `devices::lost_enrollment_ack_resumes_same_request_and_explicit_reenroll_rotation_preserve_home_identity` | 12 -> 14 |  |
| canonical task HTTP survives lost acceptance response/restart without dispatch and hides private task | `devices::canonical_task_http_survives_lost_acceptance_response_restart_without_dispatch_and_hides_private_task` | 9 -> 15 |  |
| signed host HTTP admission commits once and result ACK survives canonical restart | `devices::signed_host_http_admission_commits_once_and_result_ack_survives_canonical_restart` | 13 -> 22 |  |
| journal-backed host dispatches once, retries publication only and refuses lost or late commit grants | `journal::journal_backed_host_dispatches_once_retries_publication_only_and_refuses_lost_or_late_commit_grants` | 19 -> 22 |  |
| SIGKILL recovery proves no send before marker and never repeats marked effects or lost result ACKs | `journal::sigkill_recovery_proves_no_send_before_marker_and_never_repeats_marked_effects_or_lost_result_acks` | 12 -> 20 |  |
| Rust exports prepared, in-flight and reported device journal evidence offline | `journal::rust_exports_prepared_in_flight_and_reported_device_journal_evidence_offline` | 7 -> 7 | The frozen journal moved to `crates/minarbot-testkit/fixtures/device-journal-v3/`. |
| stalled normal requests cannot starve control reads and disconnected requests release capacity | `journal::stalled_normal_requests_cannot_starve_control_reads_and_disconnected_requests_release_capacity` | 4 -> 6 | The 64 held requests are raw TCP connections that send headers and one body byte. |
| canonical transfer survives lost RPC reply and canonical restart without duplicate successor or caller effects | `journal::canonical_transfer_survives_lost_rpc_reply_and_canonical_restart_without_duplicate_successor_or_caller_effects` | 14 -> 16 |  |
| owner recovers revoked-device journal through HTTP without device session or repeated effect | `journal::owner_recovers_revoked_device_journal_through_http_without_device_session_or_repeated_effect` | 25 -> 29 |  |
| disaster recovery imports original host result without original server and never enables dispatch | `journal::disaster_recovery_imports_original_host_result_without_original_server_and_never_enables_dispatch` | 20 -> 24 |  |
| disaster recovery quarantines signed post-backup actions and tasks without publishing or admitting them | `journal::disaster_recovery_quarantines_signed_post_backup_actions_and_tasks_without_publishing_or_admitting_them` | 21 -> 29 |  |
| canonical memory RPC isolates projects and retains committed memory after a lost reply | `rpc::canonical_memory_rpc_isolates_projects_and_retains_committed_memory_after_a_lost_reply` | 17 -> 25 |  |
| canonical history RPC pages scoped originals and retains exact reads after a lost reply | `rpc::canonical_history_rpc_pages_scoped_originals_and_retains_exact_reads_after_a_lost_reply` | 23 -> 35 |  |
| canonical skill reads recover exact pinned text after lost response, disable and server restart | `rpc::canonical_skill_reads_recover_exact_pinned_text_after_lost_response_disable_and_server_restart` | 16 -> 20 |  |
| canonical agent directory isolates projects and recovers exact roster after lost reply | `rpc::canonical_agent_directory_isolates_projects_and_recovers_exact_roster_after_lost_reply` | 13 -> 11 |  |
| canonical teammate sends retain one scoped delivery on its fixed home after lost reply and restart | `rpc::canonical_teammate_sends_retain_one_scoped_delivery_on_its_fixed_home_after_lost_reply_and_restart` | 25 -> 26 |  |
| canonical setup creation retains one restricted task and creation receipt after lost response and restart | `rpc::canonical_setup_creation_retains_one_restricted_task_and_creation_receipt_after_lost_response_and_restart` | 18 -> 17 |  |
| canonical caller interruption retains one notice and fences remote deliveries after lost result and restart | `rpc::canonical_caller_interruption_retains_one_notice_and_fences_remote_deliveries_after_lost_result_and_restart` | 17 -> 19 |  |
| canonical profile edits recover exact result without replacing later edits or current task instructions | `rpc::canonical_profile_edits_recover_exact_result_without_replacing_later_edits_or_current_task_instructions` | 13 -> 19 |  |
| canonical provider settings use home binding metadata and recover saved result after lost reply | `rpc::canonical_provider_settings_use_home_binding_metadata_and_recover_saved_result_after_lost_reply` | 8 -> 7 | Failed on product bug P7 (fixed). #87 dropped `agents.provider_account` and `agents.backup_model`: the test re-adds the legacy column before seeding it (as the routine-reads case adds `secret`), and expects `backupModel: ""` instead of the creation value `"backup"`. |
| canonical routine reads and edits isolate project configuration and recover original flow after lost reply | `rpc::canonical_routine_reads_and_edits_isolate_project_configuration_and_recover_original_flow_after_lost_reply` | 17 -> 20 |  |
| canonical routine webhook admits once offline and recovers lost response restart edits and rotation | `webhooks::canonical_routine_webhook_admits_once_offline_and_recovers_lost_response_restart_edits_and_rotation` | 47 -> 49 |  |
| canonical routine webhook conceals missing and inactive targets until signature verification | `webhooks::canonical_routine_webhook_conceals_missing_and_inactive_targets_until_signature_verification` | 13 -> 39 |  |
| canonical routine webhook performs one keyring read for configured and missing targets | `webhooks::canonical_routine_webhook_performs_one_keyring_read_for_configured_and_missing_targets` | 8 -> 8 |  |
| canonical routine webhook rejects invalid deliveries and records ignored receipts across filter changes | `webhooks::canonical_routine_webhook_rejects_invalid_deliveries_and_records_ignored_receipts_across_filter_changes` | 22 -> 22 |  |
| canonical routine webhook settings require current owner while assigned collaborator can edit | `webhooks::canonical_routine_webhook_settings_require_current_owner_while_assigned_collaborator_can_edit` | 15 -> 15 |  |
| canonical routine webhook keyring and DB failures leave no active reference or partial admission and exclude secrets | `webhooks::canonical_routine_webhook_keyring_and_db_failures_leave_no_active_reference_or_partial_admission_and_exclude_secrets` | 23 -> 23 |  |

### Product bugs fixed in the peers slice

| Bug | Symptom | Cause and fix |
| --- | --- | --- |
| P1 | Every harness agent's `delegate_task` over MCP answered 400 "Invalid delegation request". | `mcp/server.rs` added `requestId` and `turn` to session `delegate_task` arguments (aa2feb0), and the native handler (`turns/messaging.rs`) rejects unknown fields. Only the feature tool reads `turn`, so it is now added only when the declaration is not native. |
| P2 | Agents no longer saw their teammates ("Messaging other agents") in the system prompt, so remote roles never reached the team prompt. | `messaging::awareness` lost its only caller when a8cfcac removed the API runtime. `turns/execution.rs` appends it again under the same conditions (not a profile setup turn, messaging allowed). |
| P3 | A harness agent that called `wait_task` stayed paused forever. | The native `wait_task` path (`turns/harness.rs`, 93410e8) paused the turn without registering a `delegation_waits` row, so `messaging::settle` never stored the result or resumed the turn. It now registers the wait (and returns at once when the task already finished). |
| P4 | A peer update whose permission the receiver's owner revoked mid-transfer was still installed when the owner allowed updates again before the transfer ended, although the sender had reported it failed. | The receiver checked permission only at the end of the download. `updates/peers.rs` now counts revocations per machine (`computer/machines.rs` records each one); an apply that saw one never publishes or applies its release. |
| P5 | Every message to a Section of a project without folders failed with 503 "Coding runtime requires a workspace for this conversation." | #87 made every turn a harness turn, and harness turns need a working directory; `turns/tools.rs` returned none for folderless projects. Such projects now get an empty, read-only managed folder (`data/agent_folders.rs` `ensure_project`). |
| P7 | Every hosted agent's `read_provider_settings` reported `configured: false` and no provider account, although its home provider binding was ready and tasks ran on it. | a8cfcac (#87) switched the canonical tool (`tasks/mutations.rs`) to the local `agents.provider_instance`, which canonical agents never set. It reads the home binding again (`providerAccount`, `configured`, `binding_stale`). `backupModel` stays `""`, because #87 removed fallback models. |

### `app/updates.test.ts` (6 runtime cases, 6 Rust tests, file deleted)

| Node case | Rust test | Asserts | Notes |
| --- | --- | --- | --- |
| release builder publishes verified immutable files before feed metadata and refuses dirty trees | `crates/xtask/src/release_port.rs` `release_builder_publishes_verified_immutable_files_before_feed_metadata_and_refuses_dirty_trees` | 12 -> 12 | Calls `release::fixture_build`, which the Node `app_fixture` runner wrapped. |
| release lint failure stops build, tests and packaging; successful gates run in order | `release_port::release_lint_failure_stops_build_tests_and_packaging_successful_gates_run_in_order` | 6 -> 6 | The gates still run package scripts through `npm`, as the product does; the fixture scripts are shell commands instead of `node -e`. |
| desktop build follows dirty guard and packages release version without changing tracked desktop version | `release_port::desktop_build_follows_dirty_guard_and_packages_release_version_without_changing_tracked_desktop_version` | 5 -> 5 | Calls `release::command`. The product runs `node desktop/scripts/build.mjs`, so the fixture keeps that script. |
| peer fan-out needs both owners, grants no default update scope, streams verified release and keeps tokens secret | `pr_updates::peer_fan_out_needs_both_owners_grants_no_default_update_scope_streams_verified_release_and_keeps_tokens_secret` | 20 -> 20 | The release is published with `release_fixture::publish` instead of the xtask runner. Tokens come from bridge traffic and from decrypting both databases, as in Node. |
| peer SHA-256 mismatch rejects download before publication | `crates/minarbot-server/src/updates/tests/peer_sha_mismatch.rs` `peer_sha256_mismatch_rejects_download_before_publication` | 2 -> 2 | Node exercised a TypeScript stand-in (`app/rust-update-test.ts`); the Rust test drives the server's own `archive::receive`. |
| Rust trial gates owner work until matching private socket commit | `pr_updates::rust_trial_gates_owner_work_until_matching_private_socket_commit` | 0 -> 5 | Node checked statuses inside `api()`; the Rust test states each one. |
| real release build, HTTP update, broken trial rollback and intact workspace | Not ported | - | Opt-in (`MINARBOT_UPDATE_IT=1`), not a default runtime case. It patches `app/server.ts`, which the Rust server replaced, so it could no longer run. The native trial and rollback flow is covered by `crates/minarbot-launcher/tests/launcher.rs` (`native_trial_commits_and_restores_feature_data`, `exited_candidate_rolls_back`). |

### `app/fanout-older-peer.test.ts` (1 case, 1 Rust test, file deleted)

| Node case | Rust test (`pr_fanout::`) | Asserts | Notes |
| --- | --- | --- | --- |
| fan-out serves one verified executable archive to a peer without apply polling and waits beyond its former apply deadline | `fan_out_serves_one_verified_executable_archive_to_a_peer_without_apply_polling_and_waits_beyond_its_former_apply_deadline` | 16 -> 16 | A `relay::Relay` bound to the peer's paired port hides the apply-polling capability, holds the apply reply for 125 s and can replace its body. The slow call uses its own 170 s client budget. |

### `app/peer-release-transfer.test.ts` (5 default cases plus 2 opt-in, 6 Rust tests, file deleted)

As in Node, these servers run without a computer bridge. A `relay::Relay`
bound to the sender's paired port shapes each release download; another can
lose the receiver's apply reply.

| Node case | Rust test (`pr_peer_transfer::`) | Asserts | Notes |
| --- | --- | --- | --- |
| peer fan-out follows receiver progress beyond the apply request deadline | `peer_fan_out_follows_receiver_progress_beyond_the_apply_request_deadline` | 8 -> 8 | Default 1 s deadline and 90 KB payload (the `MINARBOT_PEER_CONTROL` slow values were opt-in). |
| peer fan-out succeeds while the receiver still lists the restarted sender as offline | `peer_fan_out_succeeds_while_the_receiver_still_lists_the_restarted_sender_as_offline` | 2 -> 2 | |
| peer fan-out resumes dropped downloads, including after receiver restart | `peer_fan_out_resumes_dropped_downloads_including_after_receiver_restart` | 21 -> 21 | |
| peer fan-out reports receiver restart and can resume its retained partial | `peer_fan_out_reports_receiver_restart_and_can_resume_its_retained_partial` | 8 -> 8 | |
| peer fan-out reports timeout, integrity, size, permission, and launcher failures | `peer_fan_out_reports_timeout_integrity_size_permission_and_launcher_failures` | 7 -> 7 | Intermittently failed on product bug P4 (fixed). |
| 25 KB/s control keeps fan-out open beyond 60 seconds | `kb_per_second_25_control_keeps_fan_out_open_beyond_60_seconds` | 3 -> 3 | Opt-in in Node (`MINARBOT_PEER_CONTROL=1`); runs every time in Rust (about 65 s). |
| 20.8 MB peer archive reaches launcher at N KB/s | Not ported | 5 -> - | A loop over `MINARBOT_PEER_TEST_RATES`, which is empty by default, so it produced no runtime case. It is a manual throughput benchmark (about 12 minutes at 30 KB/s), not a contract. |

### Open product gaps (Node case kept)

| Gap | Kept Node case | Detail |
| --- | --- | --- |
| P6 | `app/hub-link.test.ts` "local provider spend ceiling refuses next request before dispatch" | `harness/store.rs` reserves the hub provider budget (`before_effect(... "provider")`) once per harness pass. A CLI harness makes several model calls inside one pass (here: the call after an approved tool), so a grant with room for one paid request still makes two. The turn then fails on `spend_limit` at the next pass. Enforcing a per-request ceiling under native harnesses needs a design decision (count MCP tool steps, or refuse multi-step passes for capped grants). |
