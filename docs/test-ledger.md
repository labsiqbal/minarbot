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

<!-- stale-a start -->
## #89 stale-a slice

The Node suites below failed after #87 removed the direct API runtime. Each
runtime case is now one Rust test in `crates/minarbot-e2e/tests/e2e/sa_*.rs`
(`sa_` = stale-a), or a server unit test where noted. Agents use
`runtime: "api"` in test mode, which binds them to the kit's `fake-gateway`
Codex instance, so every model step is a harness turn against the
`ScriptedModel`.

Assertion counts are static call sites: Node `assert*(` calls in the case
body, Rust `ensure!`, `*_expect(` and `wait_status(` calls plus calls of
module helpers that assert (named per file). A Node assertion inside a loop
counts once, as does its Rust port.

Recurring #87 differences, stated once and referenced per row:

- **H1 (harness tool errors).** A harness gets an MCP error for a tool it is
  not offered, a refused call or a storage failure and decides itself how to
  continue. The API runtime failed the turn instead. Tests assert the exact
  MCP error the model saw, that the tool was not offered and that the call
  had no effect, and that the turn finishes.
- **H2 (done check).** After a turn creates a deliverable, the server sends one
  "One verification reminder" user message in the same turn. Scripted models
  answer it with text.
- **H3 (peer files).** `fake-gateway` keeps `fake-gateway-log.jsonl` and
  `.fake-gateway-<thread>.json` in its working directory, which is the agent
  workspace. Workspace listings ignore exactly these names.
- **L1 (user-selected notes).** 89b7f4e (#87, on the base) makes a note the user
  selected read-only to agents (409 "Memory note was selected by the user").
  Agent edits of such notes now assert that 409; revisions that followed
  them shift down by one.
- **W1 (channel workspaces).** 7098b86 (#87) gives a channel without a project
  its lead agent's workspace, so workspace and Git tools are offered there.

Kit additions: `minarbot_testkit::connector` (new file: port of `rust-connector-fixture.ts`, `rust-connectors-runner.ts` and `connector-journal-command.ts`: fake Telegram/Slack API with a Socket Mode WebSocket and a `Remote` hook, a lossy script-API proxy with `Fault`s, the connector runner and journal command; enables axum's `ws` feature, already in the lock through the server), `minarbot_testkit::restore` (new file: runs `minarbot-server restore ...` and checks `exitCode` against the process status, Clerk identity env and stdin token from the test-only key, Ed25519 `FenceKey`; adds the `ring` dependency the server already locks), `minarbot_testkit::hmac` (new file), `fixtures/local-pre-collapse.sql` (copied from `app/test-support/fixtures/`), `minarbot_testkit::gate::ModelGate` (new file `gate.rs`) holds
one scripted reply so a test can act between "the model chose a tool" and
"the server ran it", which the Node providers did by awaiting before they
answered. `ServerConfig::to_json` now also rejects unknown `hubLink` fields
(`not ported: option hubLink.<name>`), as the Node bridge did.

Product bugs fixed in this slice:

| Bug | Found by | Cause and fix |
| --- | --- | --- |
| P1 | workflow-http "parallel webhook ..." | A parallel branch that could not start (here an agent without a provider instance) returned its error out of `automation::workflows::runtime::settle`. That rolled back the whole automation tick every second, so the run stayed `ready` and every other routine, flow and workflow stalled. The branches now start inside a savepoint; a failure rolls them back and fails only that run, as a single step does. |

| Node file | Node runtime cases | Rust tests | Dropped | Kept in Node |
| --- | ---: | ---: | ---: | ---: |
| `app/memory.test.ts` | 2 | 2 | 0 | 1 (TS module part) |
| `app/memory-selection.test.ts` | 1 | 1 | 0 | 1 (TS module part) |
| `app/memory-tools.test.ts` | 2 | 2 | 0 | 0 |
| `app/shared-data.test.ts` | 1 | 1 | 0 | 0 |
| `app/rust-file-output-panel.test.ts` | 1 | 1 | 0 | 0 |
| `app/rust-core.test.ts` | 2 | 2 | 0 | 0 |
| `app/rust-workspace-git-http.test.ts` | 2 | 2 | 0 | 0 |
| `app/routines.test.ts` | 4 | 4 | 0 | 0 |
| `app/models.test.ts` | 1 | 1 | 0 | 0 |
| `app/workflow-http.test.ts` | 12 | 12 (+1 regression) | 0 | 1 (TS module) |
| `app/schema.test.ts` | 4 | 4 (1 server unit test) | 0 | 0 |
| `app/restore-fence.test.ts` | 7 | 7 | 0 | 0 |
| `app/telegram.test.ts` | 6 | 6 | 0 | 0 |

### `app/memory.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| memory CRUD, exact retry, isolated provider search and bounded context survive restart | `sa_memory::memory_crud_exact_retry_isolated_provider_search_and_bounded_context_survive_restart` | 33 -> 34 | The six `memoryCommand` rejections are also asserted as server 400s. The `memoryDraft`/`memoryCommand` assertions stay in the trimmed Node case (TS module `app/src/memory-command.ts`). |
| Rust memory permits edits at capacity and rolls back edits over the context budget | `sa_memory::rust_memory_permits_edits_at_capacity_and_rolls_back_edits_over_the_context_budget` | 7 -> 15 | Full. |

### `app/memory-selection.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| automatic memory selection is atomic, user-protected, scoped and durable across resume | `sa_memory_selection::automatic_memory_selection_is_atomic_user_protected_scoped_and_durable_across_resume` | 30 -> 23 (+5 kept) | The provider's "`select_memory` offered" assertion is counted per request. `memoryCommand({automatic: 'yes'})` is also asserted as a server 400; the other `memoryDraft`/`memoryCommand` assertions stay in the trimmed Node case (TS module). |

### `app/memory-tools.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| agent memory writes preserve scope, user selection, atomic errors and resume receipts | `sa_memory_tools::agent_memory_writes_preserve_scope_user_selection_atomic_errors_and_resume_receipts` | 32 -> 32 | L1: "Agent updated" and "Side update" assert 409. The context-budget note is saved with `automatic: true` so the agent edit still reaches the 413 check. `ModelGate` replaces the provider's `beforeWrite` await. H1: the journal fault is a 500 tool error and the write rolls back. |
| memory and artifact order survives repeated delegation when parent continues | `sa_memory_tools::memory_and_artifact_order_survives_repeated_delegation_when_parent_continues` | 5 -> 3 | Full (the Node count includes the provider's own checks). |

### `app/shared-data.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| Rust shared SQL keeps schema ownership, host restrictions and bounded results | `sa_shared_data::rust_shared_sql_keeps_schema_ownership_host_restrictions_and_bounded_results` | 27 -> 38 (counting `Sql::ok`/`ok_with`/`rejects`) | The runaway-query bound (5 s) is measured beyond an ordinary harness turn, measured right before it. |

### `app/rust-file-output-panel.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| conversation output updates, panel requests and owner deletion preserve reference identity | `sa_file_output_panel::conversation_output_updates_panel_requests_and_owner_deletion_preserve_reference_identity` | 35 -> 30 | Full; the provider's two checks are collected and asserted at the end. H2. |

### `app/rust-core.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| core serves exact static bytes, strict asset CSP, HTML 404 and local boundary errors | `sa_core::core_serves_exact_static_bytes_strict_asset_csp_html_404_and_local_boundary_errors` | 12 -> 12 | Serves a minimal packaged build (`sa_support::web_dist`) instead of `app/dist`, so no web build is needed. |
| Rust test bridge rejects unported options before starting a process | `sa_core::rust_test_bridge_rejects_unported_options_before_starting_a_process` | 4 -> 4 | `NaN` and `Infinity` are sent as `"NaN"` and `1e300`. |

### `app/rust-workspace-git-http.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| Git log returns last 50 commits and no more | `sa_workspace_git::git_log_returns_last_50_commits_and_no_more` | 4 -> 5 | Full. |
| HTTP Git reads/tools/checkpoints use workspace gating and reject foreign requests, restart and restore through API | `sa_workspace_git::http_git_reads_tools_checkpoints_use_workspace_gating_and_reject_foreign_requests_restart_and_restore_through_api` | 22 -> 25 | H1: the read-only `git_commit` call gets MCP 403 and no commit appears. W1: the channel turn now runs `git_status`. H3. |

### `app/routines.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| manual routine runs keep schedule, deduplicate across restart and enforce revision and overlap | `sa_routines::manual_routine_runs_keep_schedule_deduplicate_across_restart_and_enforce_revision_and_overlap` | 36 -> 58 | H1: a direct `report_failure` is refused over MCP and records no outcome (the Node case expected a failed turn). Dropped: the 503 for a new run after restarting without `baseURL`; #87 removed the global provider and the agent's instance persists. |
| routine occurrences persist once and isolate each run from main history | `sa_routines::routine_occurrences_persist_once_and_isolate_each_run_from_main_history` | 38 -> 49 | Real clock (scheduling). Icon from `sa_support::web_dist`. H1: a direct `no_update` is refused over MCP. |
| routine hub pages visible local owners, filters safely and never returns secrets | `sa_routines::routine_hub_pages_visible_local_owners_filters_safely_and_never_returns_secrets` | 22 -> 30 | Full. |
| routine hub delete is revisioned and recoverable, retains runs, revokes webhook and prevents resurrection | `sa_routines::routine_hub_delete_is_revisioned_and_recoverable_retains_runs_revokes_webhook_and_prevents_resurrection` | 15 -> 22 | Full. |

### `app/models.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| model discovery validates catalog and never follows credential redirects | `sa_models::model_discovery_validates_catalog_and_never_follows_credential_redirects` | 13 -> 11 | #87 removed `GET /api/models` (now 404 "Use a provider instance model catalog.") and the server's own provider HTTP client. The test asks the test-gateway instance (`POST /api/provider-instances/codex-test-gateway/models`); `fake-gateway` reads a raw loopback provider (`sa_support::RawProvider`). Kept: bearer key and path on every provider call, deduplicated ids, empty catalog, 502 without the provider detail or key for 401, oversized, malformed, `"\n"` and `" model "` ids. Changed: ids keep the instance's order (`["z","a"]`; the API catalog sorted them) and the default comes from the harness. Dropped (#87 removed the server's provider client and API settings): the redirect mode (the harness fetches models now), `?runtime=api&account=`, startup rejection of an invalid `baseURL`, and 503 without a provider. |

### `app/workflow-http.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| legacy unsupported workflow steps fail once without producing output | `sa_workflows::legacy_unsupported_workflow_steps_fail_once_without_producing_output` | 10 -> 9 | Full. |
| model conditions choose one branch without tools and reject ambiguous answers | `sa_workflows::model_conditions_choose_one_branch_without_tools_and_reject_ambiguous_answers` | 27 -> 25 | Full; "no tools" is asserted on the tools the harness offered the model. |
| workflow HTTP versions, model steps, approval receipts and cancellation survive restart | `sa_workflows::workflow_http_versions_model_steps_approval_receipts_and_cancellation_survive_restart` | 53 -> 93 | Full. |
| provider-free workflows continue beyond one batch and configuration listing is bounded | `sa_workflows::provider_free_workflows_continue_beyond_one_batch_and_configuration_listing_is_bounded` | 20 -> 19 | Full (`nextAt` relative to the fixed clock). |
| workflow webhooks authenticate, deduplicate, ignore busy runs and recover rotation | `sa_workflows::workflow_webhooks_authenticate_deduplicate_ignore_busy_runs_and_recover_rotation` | 27 -> 51 | Full; real clock for signed timestamps and the 8-day receipt window. HMAC from `minarbot_testkit::hmac`. |
| parallel webhook waits for provider before creating a receipt | `sa_workflows::parallel_webhook_waits_for_provider_before_creating_a_receipt` | 15 -> 12 | Full. After the restart with a provider the agent is bound to the new instance, because #87 agents bind instances instead of using a global provider. Found bug P1 (below); regression test `sa_workflows::parallel_branch_start_failure_fails_only_its_run`. |
| workspace workflow directory pages across owners and keeps per-agent lists scoped | `sa_workflows::workspace_workflow_directory_pages_across_owners_and_keeps_per_agent_lists_scoped` | 15 -> 27 | Full. |
| autosave stops after server canonicalizes condition property order | `sa_workflows::autosave_stops_after_server_canonicalizes_condition_property_order` | 4 -> 3 | Server half: the edit ack returns the draft's graph at revision 2. The case stays in Node for `workflowAutosaveDraft` (TS module `app/src/workflow-editor-state.ts`). |
| workflow deletion revokes triggers, rejects stale commands and retains terminal results | `sa_workflows::workflow_deletion_revokes_triggers_rejects_stale_commands_and_retains_terminal_results` | 13 -> 32 | Full. |
| workflow output inbox survives restart with stable paging and idempotent read | `sa_workflows::workflow_output_inbox_survives_restart_with_stable_paging_and_idempotent_read` | 11 -> 21 | Full. |
| workflow channel outputs deliver once and recheck channel availability at completion | `sa_workflows::workflow_channel_outputs_deliver_once_and_recheck_channel_availability_at_completion` | 21 -> 47 | Full. |
| workflow run pages retain older approvals after restart and validate cursors | `sa_workflows::workflow_run_pages_retain_older_approvals_after_restart_and_validate_cursors` | 6 -> 12 | Full. |

### `app/schema.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| fresh canonical startup extends the 85373b3 schema and reopens its data unchanged | `sa_schema::fresh_canonical_startup_extends_the_85373b3_schema_and_reopens_its_data_unchanged` | 11 -> 13 (shared helper) | #87 removed legacy provider storage: a fresh server no longer creates `provider_accounts`, `provider_account_changes`, `model_fallbacks` (and, locally, `engine_logins`); an upgrade drops `provider_accounts`, `engine_logins` and the `agents` columns `runtime`, `provider_account`, `backup_model`, and keeps the other two tables so old turns keep their history. The expected snapshots and the retained agent row leave exactly these out. The optional baseline-binary branch (`MINARBOT_SCHEMA_BASELINE_BINARY`) is not ported; the frozen fixture is seeded as in the default Node path. |
| fresh local startup extends the 85373b3 schema and reopens its data unchanged | `sa_schema::fresh_local_startup_extends_the_85373b3_schema_and_reopens_its_data_unchanged` | 11 -> 13 (shared helper) | Same as above. |
| fresh Rust hub startup preserves the 85373b3 schema and applies extensions | `minarbot-server` unit test `hub::schema_tests::fresh_rust_hub_startup_preserves_the_85373b3_schema_and_applies_extensions` | 2 -> 2 | Opens the hub store (`hub::storage::open`) directly; fixture copied to `crates/minarbot-server/src/hub/testdata/`. |
| local 0.16.4 memory import does not backfill learning jobs | `sa_schema::local_0_16_4_memory_import_does_not_backfill_learning_jobs` | 8 -> 8 | Full (the optional `MINARBOT_V0164_BINARY` branch is not ported). |

### `app/restore-fence.test.ts`

All cases drive `minarbot-server restore ...` through `minarbot_testkit::restore`.

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| signed infrastructure fence preflight survives restart and exact retry without activating restore | `sa_restore_fence::signed_infrastructure_fence_preflight_survives_restart_and_exact_retry_without_activating_restore` | 8 -> 8 | Full. |
| restore fence rejects forged, stale, mismatched, self-supplied and replaced-session proofs | `sa_restore_fence::restore_fence_rejects_forged_stale_mismatched_self_supplied_and_replaced_session_proofs` | 7 -> 7 | Full (each of the 18 evidence variants and 3 pins is checked in a loop). |
| operator CLI creates and verifies bounded private fence files while keeping recovery read-only | `sa_restore_fence::operator_cli_creates_and_verifies_bounded_private_fence_files_while_keeping_recovery_read_only` | 25 -> 25 | Full. |
| activation plan commits one fenced recovery receipt and exact retry | `sa_restore_fence::activation_plan_commits_one_fenced_recovery_receipt_and_exact_retry` | 9 -> 9 | The evidence lives 5 s instead of 30 s, because the case waits for it to expire. |
| activation rolls back on receipt failure and rejects changed plans | `sa_restore_fence::activation_rolls_back_on_receipt_failure_and_rejects_changed_plans` | 9 -> 9 | Full. |
| activation retains populated revoked grants and held tasks, refusing unsafe recovery state | `sa_restore_fence::activation_retains_populated_revoked_grants_and_held_tasks_refusing_unsafe_recovery_state` | 12 -> 9 | Full; the four revoked-list counts are one loop. |
| activation requires current schema, owner session and fresh infrastructure proof | `sa_restore_fence::activation_requires_current_schema_owner_session_and_fresh_infrastructure_proof` | 6 -> 7 | Recovery accepts schema v6 (baseline) to v7 (#87 added `v7_provider_instances`); the Node case deleted v6, then the newest, and failed after #87. The test removes v6 and v7. Evidence lives 5 s. |

### `app/telegram.test.ts`

The connector cases use `minarbot_testkit::connector`. Model steps after
`ask_user` follow stage 1's B1 handling (`support::after_question_reply`,
left out of counts).

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| Telegram question buttons answer the owning task once across restart and lost responses | `sa_telegram::telegram_question_buttons_answer_the_owning_task_once_across_restart_and_lost_responses` | 19 -> 18 | Full (the Node case failed on B1's null step). |
| Telegram offline recovery preserves sent parts and rejects repeated operator decisions | `sa_telegram::telegram_offline_recovery_preserves_sent_parts_and_rejects_repeated_operator_decisions` | 26 -> 26 | Full. |
| Telegram bridge submits once through granted side-thread API and isolates main history | `sa_telegram::telegram_bridge_submits_once_through_granted_side_thread_api_and_isolates_main_history` | 12 -> 11 | Full. |
| Rust Telegram runner rejects a mismatched bot identity and releases its journal lock | `sa_telegram::rust_telegram_runner_rejects_a_mismatched_bot_identity_and_releases_its_journal_lock` | 3 -> 2 | Full. |
| Telegram runner replies directly and releases owned journal on shutdown | `sa_telegram::telegram_runner_replies_directly_and_releases_owned_journal_on_shutdown` | 9 -> 6 (shared helper) | Full; the fake's own checks are collected and asserted. |
| Telegram runner answers a question and releases owned journal on shutdown | `sa_telegram::telegram_runner_answers_a_question_and_releases_owned_journal_on_shutdown` | 9 -> 6 (shared helper) | Same helper; model calls exclude the B1 step (the Node case saw 4). |
<!-- stale-a end -->

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
