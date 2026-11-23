# Test migration ledger

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
| `app/hub/web.test.ts` adapter case | The gateway suite checks the original outsider reads, mailbox proof, secret-free invitation lists, owner node/agent binding, exact registration retries and consent roles through Rust HTTP. The React draft/join/grant parser case stays in `web.test.ts`. |
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

The route ownership ledger lists each public surface and its default document.
The golden gate runs Leptos against disposable servers and committed screenshots.
The larger Node suite retains the React tests during stage B part 1.
[Route ownership](../crates/web/README.md#route-ownership)

| Gate group | Required pairs | Functional coverage |
| --- | ---: | --- |
| appearance | 20 | General and Appearance, settings focus and history, presentation storage, CSP and compressed Wasm, startup failures, setup, pairing |
| automation | 40 | Routine schedules, workflow editor and graph, versioning, webhooks, run inspector |
| calendar | 12 | Automation and project calendars, dates and timezone, run detail and downloads |
| connections | 36 | Provider, engine, Hub, email, Vault, MCP and connected app screens and mutation contracts |
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

The golden baseline run passed all 16 groups and 580 pairs. All pairs use the
same pixel thresholds, including the ten narrow Hub Leptos baselines.

See the screenshot parity guide for the gate command and failure thresholds.
Retained Node and Rust API cases below still cover the shared contracts.
[Parity guide](../app/test-support/ui-smoke/PARITY.md)

### React client tests retained for cutover

Each row names current Leptos, Rust, or shared-code coverage. Keep the Node
files until the React removal part. Timing sources and ceilings are recorded
in the [golden gate guide](../app/test-support/ui-smoke/PARITY.md).

| Node test | Coverage decision |
| --- | --- |
| `attachments.test.ts` | Shared attachment validation in the Leptos adapter and files/controls journeys for file selection and removal. |
| `audio-recording.test.ts` | Shared `startAudioRecording` runs under the Leptos adapter. Existing lifecycle tests still exercise production code. |
| `clipboard.test.ts` | Shared Clipboard API and fallbacks run under Leptos. Conversation and Computer journeys exercise browser clipboard. |
| `conversation-command.test.ts` | Rust conversation `protocol::restore`, Leptos response-loss retry and room/section controls cover scope and command identity. |
| `diff-lines.test.ts` | Leptos files smoke exercises inline and split diff rendering. Rust file/git tests exercise path and edit contracts. |
| `goal-command.test.ts` | Leptos `goals-smoke.mjs` and Rust conversation input tests cover mode commands and replay. |
| `guide.test.ts` | Guide and shell smokes navigate the route map while Rust imports the same guide screen specification. |
| `http.test.ts` | `readJson` is still called by the Leptos browser adapter. Existing response parsing tests still exercise production code. |
| `markdown.test.ts` | Ported renderer checks to `leptos-conversation-smoke.mjs`: structure, unsafe markup, ordinary/result links, attachment mentions and deleted attachments. |
| `markdown-stream-blocks.test.ts` | `splitMarkdownBlocks` is shared. Ported rendered reference-link and retained-prefix checks to `leptos-conversation-smoke.mjs`. |
| `message-find.test.ts` | `matchOffsets` is shared by the Leptos browser adapter. Controls smoke checks UI matching and highlight cleanup. |
| `navigation.test.ts` | Conversation deep links, startup route and history assertions run against Leptos. Result target parsing is shared. |
| `needs-you-notifications.test.ts` | Ported to `needs-you-notifications-smoke.mjs`. Leptos distinguishes requests on one turn from completion and suppresses duplicate notifications after reload. |
| `next-frame.test.ts` | The tested frame queue is shared by the Leptos HTML reply and MCP app bridges. |
| `optimistic-turns.test.ts` | Leptos conversation retry checks cover an in-flight send, stale history, reload and exact receipt reconciliation. |
| `polling.test.ts` | Leptos stream/reconnect and profile deletion smokes check cleanup and stale updates. Download helper is still shared. React-specific polling backoff and visibility scheduling use a different implementation and need a product contract before an equivalent port. |
| `presentation.test.ts` | Appearance smoke covers local storage, palettes, mode, text size and tabs. Theme values are shared with the Leptos build. Conversation files checks dock and narrow layouts. |
| `query-cache.test.ts` | Leptos uses route-local caches and generation checks. Model catalog smoke ports the stale-response assertion by releasing a Codex response after switching to API. React LRU and identity-clear policies have no Leptos equivalent. |
| `right-panel-state.test.ts` | Leptos controls/files smokes exercise docking, tabs, close/reopen, history navigation and narrow overlays. |
| `run-inspector.test.ts` | Leptos renders work logs through the shared `turnActivitySteps` adapter. Files and rich cover inspector display and API errors. |
| `timeline-minimap.test.ts` | Shared preview utility and Leptos `timeline-minimap-smoke.mjs` cover previews, keyboard movement, pointer bounds and narrow behavior. |
| `updateEverything.logic.test.ts` | System and shell smokes cover restart state, remote receipts, exact payload recovery and update submission. Native launcher tests cover persisted restart/rollback. |
| `workflows.test.ts` | Automation and scoped smokes cover keyboard moves, undo/save, version confirmation, route readiness, run results and journal replay. Rust server validates graph paths and step configuration. |

`csp.test.ts` still checks the legacy built SPA during part 1. The Leptos
appearance smoke checks CSP, gzip, asset allowlists, retry and failed Wasm.
Model catalog and Discord setup use disposable Leptos browser fixtures.
Hub-link tests are owned by the parallel Hub task.

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
| `csp.test.ts` | 1 | Actual built asset compression and matching style nonce in the shipped SPA. |
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

- **Client and reducer**. React rendering, browser state, parsing or client models move with their Leptos call sites.
  `attachments.test.ts`, `audio-recording.test.ts`, `clipboard.test.ts`,
  `conversation-command.test.ts`, `diff-lines.test.ts`, `goal-command.test.ts`, `guide.test.ts`,
  `markdown-stream-blocks.test.ts`, `markdown.test.ts`, `message-find.test.ts`,
  `navigation.test.ts`, `needs-you-notifications.test.ts`, `next-frame.test.ts`,
  `optimistic-turns.test.ts`, `polling.test.ts`, `presentation.test.ts`, `query-cache.test.ts`,
  `right-panel-state.test.ts`, `run-inspector.test.ts`, `timeline-minimap.test.ts`,
  `updateEverything.logic.test.ts`, `workflows.test.ts`.

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

- **Local API, CLI and automation**. Keep these feature contracts while their Rust replacements lack assertion-by-assertion parity. Mixed React and server cases also cover the current client.
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
