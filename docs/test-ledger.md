# Test migration ledger

## #89 Rust test kit stage 1

`crates/minarbot-testkit` replaces `app/test-bridge.ts`,
`app/rust-test-server.ts` and the `app/fixtures/*.mjs` peers. The six suites
below were ported case by case to `crates/minarbot-e2e/tests/e2e/<area>.rs`.
Four cases remain in Node. They exercise TypeScript modules scheduled
for removal with `app/` in stage 5.

| Node file | Node runtime cases | Rust tests | Node cases kept |
| --- | ---: | ---: | ---: |
| `app/threads.test.ts` | 4 | 3 | 1 (TS module) |
| `app/questions.test.ts` | 2 | 1 | 1 (TS module) |
| `app/goals.test.ts` | 5 | 5 | 0 |
| `app/cli.test.ts` | 4 | 4 + 1 regression | 0 |
| `app/agent-create.test.ts` | 8 | 6 + 2 regressions | 2 (TS module) |
| `app/harness.test.ts` | 17 declarations / 32 runtime cases | 32 + 1 regression | 0 |
| Total | 55 | 51 + 4 regressions | 4 |

`crates/minarbot-e2e/tests/e2e/kit.rs` (3 tests) and
`crates/minarbot-testkit/tests/{fakes,kit}.rs` (18 tests) cover the kit itself.

### Case map

Counts are assertion sites per case body. Rust counts include explicit
status and exit-code helpers. One site can check multiple fields or run
for each item in a loop. Counts for Codex and Claude apply to each runtime.

| Node case | Rust test | Node / Rust assertion sites | Notes |
| --- | --- | --- | --- |
| threads: side threads isolate queues, history, controls and archive recovery | `threads::side_threads_isolate_queues_history_controls_and_archive_recovery` | 28 / 70 | Full. |
| threads: configured concurrency limits side threads and validates before opening storage | `threads::configured_concurrency_limits_side_threads_and_validates_before_opening_storage` | 17 / 14 | Full. `NaN` and `Infinity` have no JSON form, so the test uses `"NaN"` and `1e300`. |
| threads: conversation commands protect Main chat, revisions, queued work and deleted history across restart | `threads::conversation_commands_protect_main_chat_revisions_queued_work_and_deleted_history` | 21 / 37 | Full. |
| threads: conversation recovery retains exact request and refuses corrupt or foreign routes | Kept in Node | 3 / 0 | Tests `app/src/thread-command.ts`. |
| questions: question cards persist, answer once in scope, and resume without repeating effects | `questions::question_cards_persist_answer_once_in_scope_and_resume_without_repeating_effects` | 22 / 28 | The last step changed with #87: harness turns get an invalid question as an MCP tool error, not a failed turn. The test asserts the rejection reaches the model and no card is stored. |
| questions: answer recovery rejects malformed storage and preserves exact retry identity | Kept in Node | 2 / 0 | Tests `app/src/question-answer.ts`. |
| goals: goal judges work separately, uses the configured model and keeps ordinary requests unchanged | `goals::goal_judges_work_separately_uses_the_configured_model_and_keeps_ordinary_requests_unchanged` | 8 / 10 | Full. |
| goals: goal can be set by an agent, pauses for you and resumes on answer | `goals::goal_can_be_set_by_an_agent_pauses_for_you_and_resumes_on_answer` | 4 / 3 | Full. |
| goals: stop, token limits and restart fence prevent later work | `goals::stop_and_restart_fence_prevent_later_goal_work` | 3 / 3 | Full, including the pre-dispatch `maxTokens` fence. |
| goals: loops use routine occurrences after restart and stop on until or max runs | `goals::loops_use_routine_occurrences_after_restart_and_stop_on_until_or_max_runs` | 6 / 9 | Full. |
| goals: ACP work and judge use distinct sessions with tools disabled for the judge | `goals::acp_work_and_judge_use_distinct_sessions_with_tools_disabled_for_the_judge` | 3 / 4 | Full. The inline ACP script is now the judge mode of `fake-acp`. The agent is bound through `/api/provider-instances`, because #87 ignores `runtime` on create. |
| cli: CLI routine lists and histories stay within the selected granted agent | `cli::cli_routine_lists_and_histories_stay_within_the_selected_granted_agent` | 71 / 58 | Provider settings use instance metadata. Catalog checks also run in `cli::cli_models_reads_the_granted_provider_instance_catalog`. |
| cli: script model catalog reads the granted provider instance | `cli::cli_models_reads_the_granted_provider_instance_catalog` | supplemental B3 regression | The Node catalog assertions are included in the routine case. |
| cli: CLI workflow discovery paginates and explicit approvals retain identity after lost responses | `cli::cli_workflow_discovery_paginates_and_explicit_approvals_retain_identity_after_lost_responses` | 97 / 84 | Full. |
| cli: script CLI discovers only grants, sends once, waits, preserves retry ID and reports attention/timeouts | `cli::script_cli_discovers_only_grants_sends_once_waits_preserves_retry_id_and_reports_attention` | 76 / 68 | Full model-call counts after questions. A deterministic offline window replaces the racy server-stop gap. |
| cli: CLI answers retain receipt identity across lost replies and restart | `cli::cli_answers_retain_receipt_identity_across_lost_replies_and_restart` | 71 / 58 | Full model-call counts after questions. |
| agent-create: creation recovery keeps exact command and rejects malformed authority | Kept in Node | 4 / 0 | Tests `app/shared/agent-create.ts`. |
| agent-create: prompt creation atomically queues one restricted setup and recovers without duplicate work | `agent_create::prompt_creation_atomically_queues_one_restricted_setup_and_recovers_without_duplicate_work` | 16 / 33 | B4 is covered in the setup flow. B5 and B6 use dedicated regressions below. Resumed transcripts use server call IDs, so saves are matched by tool name. |
| agent-create: setup creation rejects missing provider before storing an agent | `agent_create::setup_creation_requires_a_ready_provider` | supplemental B5 regression | The provider readiness guard returns 503 and leaves no agent row. |
| agent-create: creation receipt replays after provider removal | `agent_create::creation_replay_survives_provider_removal` | supplemental B6 regression | Both omitted and explicit provider requests replay without another setup turn. |
| agent-create: canonical draft carries project/home authority and rejects legacy or malformed provenance | Kept in Node | 4 / 0 | Tests `app/test-support/canonical-agent-creation.ts`. |
| agent-create: canonical creation is owner/home scoped, offline-safe, exact and fully atomic | `agent_create::canonical_creation_is_owner_home_scoped_offline_safe_exact_and_fully_atomic` | 25 / 12 | Full. |
| agent-create: provider binding stays explicit and avatar receipt hashes original bytes | `agent_create::provider_binding_stays_explicit_and_avatar_receipt_hashes_original_bytes` | 10 / 6 | #87 dropped `agents.provider_account`; the test asserts `agent_authority.provider_binding`. |
| agent-create: canonical setup admission rolls back every row and requires a ready explicit home binding | `agent_create::canonical_setup_admission_rolls_back_every_row_and_requires_a_ready_explicit_home_binding` | 11 / 8 | Full. |
| agent-create: second project participation and shared CAS isolate rules/capabilities and stale grants | `agent_create::second_project_participation_and_shared_cas_isolate_rules_capabilities_and_stale_grants` | 19 / 9 | Full. |
| agent-create: local role create/edit receipts preserve normalized profile, omissions and revision conflicts across restart | `agent_create::local_role_create_edit_receipts_preserve_normalized_profile_omissions_and_revision_conflicts` | 11 / 22 | Full. |
| harness: claude: workspace shell inspection and checks finish without approval | `harness::claude_workspace_shell_inspection_and_checks_finish_without_approval` | 4 / 3 | Full, including no approval in test mode. |
| harness (codex, claude): native check receipt prevents a second verification turn | `harness::native_check_receipt_prevents_a_second_verification_turn::{codex,claude}` | 7 / 8 | Full. |
| harness (codex, claude): gated segments replace only final answer, coalesce writes and restore without replay | `harness::gated_segments_replace_only_final_answer_coalesce_writes_and_restore_without_replay::{codex,claude}` | 17 / 14 | Full. |
| harness (codex, claude): abort flush preserves final pending segment and stays unfinished after restart | `harness::abort_flush_preserves_final_pending_segment_and_stays_unfinished_after_restart::{codex,claude}` | 5 / 3 | Full. |
| harness (codex, claude): error flush commits buffered tail without declaring a final answer | `harness::error_flush_commits_buffered_tail_without_declaring_a_final_answer::{codex,claude}` | 7 / 5 | Full. |
| harness (codex, claude): authoritative snapshot and reply apply identical session-token redaction | `harness::authoritative_snapshot_and_reply_apply_identical_session_token_redaction::{codex,claude}` | 3 / 3 | Full. |
| harness (codex, claude): stream, tool activity, MCP lifetime, follow-up and restart resume | `harness::stream_tool_activity_mcp_lifetime_follow_up_and_restart_resume::{codex,claude}` | 21 / 14 | Full. |
| harness (codex, claude): Steer degrades to linked Restart with partial context and fresh native session | `harness::steer_degrades_to_linked_restart_with_partial_context_and_fresh_native_session::{codex,claude}` | 15 / 13 | Full B7 coverage. |
| harness: queued Steer promotion restarts the linked turn | `harness::queued_steer_promotion_restarts_the_linked_turn` | supplemental B7 regression | Exercises the `/dispatch` promotion path after admission already queued the message. |
| harness (codex, claude): addressed room Steer restarts only selected member; room Stop expires approval and fences restart | `harness::addressed_room_steer_restarts_only_selected_member_and_room_stop_fences_restart::{codex,claude}` | 11 / 10 | Full B7 coverage. |
| harness (codex, claude): durable approval allow, deny, prefix grant and scoped receipt | `harness::durable_approval_allow_deny_prefix_grant_and_scoped_receipt::{codex,claude}` | 15 / 16 | Full. |
| harness (codex, claude): workspace write policy, outside-file and network consent | `harness::workspace_write_policy_outside_file_and_network_consent::{codex,claude}` | 9 / 10 | Full. |
| harness (codex, claude): crash fails once, preserves unfinished text and never replays | `harness::crash_fails_once_preserves_unfinished_text_and_never_replays::{codex,claude}` | 4 / 3 | Full. |
| harness (codex, claude): cancel kills descendants and revokes MCP; mid-turn runtime switch refused | `harness::cancel_kills_descendants_and_revokes_mcp_mid_turn_runtime_switch_refused::{codex,claude}` | 3 / 2 | Full. |
| harness (codex, claude): pending approval expires on shutdown/restart, no auto approval | `harness::pending_approval_expires_on_shutdown_restart_no_auto_approval::{codex,claude}` | 3 / 4 | Full. |
| harness (codex, claude): bounded output fails without replay | `harness::bounded_output_fails_without_replay::{codex,claude}` | 3 / 2 | Full. |
| harness: runtime changes require confirmation and revision; missing workspaces are provisioned | `harness::runtime_changes_require_confirmation_and_revision_missing_workspaces_are_provisioned` | 5 / 4 | Removed by #87: the unconfirmed runtime switch (409) and `agents[0].runtime == "api"`. Revisions, replay, runtime choices and a missing executable remain. |
| harness (codex, claude): done check nudges once and preserves native session | `harness::done_check_nudges_once_and_preserves_native_session::{codex,claude}` | 5 / 5 | Full. |

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

The Rust regressions for B1 through B8 fail against `af83887` and pass
against the patched server. B9 passes on both builds.

| Bug | Rust regression test | Root cause and fix |
| --- | --- | --- |
| B1 | `cli::script_cli_discovers_only_grants_sends_once_waits_preserves_retry_id_and_reports_attention`, `cli::cli_answers_retain_receipt_identity_across_lost_replies_and_restart` | `ask_user` paused the turn but returned a normal tool result to the native gateway. The server commits the question, signals cancellation and holds the MCP reply until the worker releases its slot. |
| B2 | `cli::cli_routine_lists_and_histories_stay_within_the_selected_granted_agent` | Script provider read held the database guard while calling the state-locking provider list. The route reads instance metadata using the current connection and omits connection details. |
| B3 | `cli::cli_models_reads_the_granted_provider_instance_catalog` | Script models called a POST-only provider-instance route with GET. The script route invokes the instance catalog outside the DB lock and rechecks the grant before replying. |
| B4 | `agent_create::prompt_creation_atomically_queues_one_restricted_setup_and_recovers_without_duplicate_work` | Profile setup validation checked a transient column instead of `profile::setup()`. Both tool validation and final setup completion now use the setup predicate. |
| B5 | `agent_create::setup_creation_requires_a_ready_provider` | Setup creation lacked a ready provider guard. Setup now returns 503 before storing rows when the provider readiness check fails. |
| B6 | `agent_create::creation_replay_survives_provider_removal` | Replay validation rejected a stored provider that had since been deleted. Replay compares the normalized request using the original provider choice before checking live providers. |
| B7 | `harness::steer_degrades_to_linked_restart_with_partial_context_and_fresh_native_session::{codex,claude}`, `harness::addressed_room_steer_restarts_only_selected_member_and_room_stop_fences_restart::{codex,claude}` | Turn admission and dispatch promotion now convert unsupported steer to restart, preserve the cannot-steer label, cancel the selected active turn, clear native session state, and retain room stop fencing. Queued promotion is covered by `harness::queued_steer_promotion_restarts_the_linked_turn`. |
| B8 | `harness::claude_workspace_shell_inspection_and_checks_finish_without_approval` | Test mode forced approval for every shell command. Approval now follows the same writable-workspace and sensitive-command policy as production. |
| B9 | `goals::stop_and_restart_fence_prevent_later_goal_work` | `6ae2a68` restored `goals::allowance` in `execution::run` before this slice. The port asserts that `maxTokens: 1` stops the goal without dispatching. |



## #89 product slice

The 18 files of the product slice moved to `crates/minarbot-e2e/tests/e2e/pd_<file>.rs`,
one Rust test per Node case. Assertion counts are static counts: Node
`assert*(...)` calls in the case, Rust `ensure!`, `*_expect` and status
checks in the test and the helpers it calls. Where a Node fixture asserted
inside its HTTP handler (provider, image server, MCP peer), the Rust fake
records violations and the test checks them, so several Node assertions map
to one Rust check. Waits (`until`, `assert.fail('Timed out')`) count on the
Node side only.

| Node file | Node cases | Rust tests | Node asserts -> Rust | Node cases kept |
| --- | ---: | ---: | --- | --- |
| `app/workflow-parallel.test.ts` | 1 | 1 | 59 -> 71 | 0 |
| `app/flows-http.test.ts` | 1 | 1 | 36 -> 57 | 0 |
| `app/webhooks.test.ts` | 2 | 2 | 60 -> 76 | 0 |
| `app/shared-data-http.test.ts` | 1 | 1 | 17 -> 21 | 0 |
| `app/files.test.ts` | 2 | 2 | 60 -> 73 | 0 |
| `app/pdf-text.test.ts` | 1 | 1 | 28 -> 31 | 0 |
| `app/image-generation.test.ts` | 1 | 1 | 27 -> 18 | 0 |
| `app/history-tools.test.ts` | 1 | 1 | 24 -> 22 | 0 |
| `app/skill-tools.test.ts` | 1 | 1 | 15 -> 12 | 0 |
| `app/workspace-shell.test.ts` | 2 | 2 | 18 -> 19 | 0 |
| `app/mcp-isolation.test.ts` | 1 | 1 | 7 -> 4 | 0 |
| `app/rust-desktop-http.test.ts` | 5 | 5 | 123 -> 112 | 1 (B1) |
| `app/sidebar.test.ts` | 3 | 3 | 42 -> 48 | 0 |
| `app/channel-context.test.ts` | 1 declaration / 2 runtime cases | 2 | 15 -> 13 per case | 0 |
| `app/agent-reply-capture.test.ts` | 1 | 1 | 35 -> 37 | 0 |
| `app/guide-infra.test.ts` | 1 | 1 | 26 -> 25 | 0 |
| `app/local-projects.test.ts` | 13 | 13 | 166 -> 164 | 0 |
| `app/local-routines.test.ts` | 4 | 4 | 44 -> 55 | 0 |
| Total | 42 | 42 | 802 -> 858 | 1 |

Every Node file except `app/rust-desktop-http.test.ts` is deleted. That
file keeps one case, unchanged, for B1.

### Case map

| Node case | Rust test | Asserts | Notes |
| --- | --- | --- | --- |
| workflow-parallel: parallel workflow joins once in branch order, retains failures and cancels all branches | `pd_workflow_parallel::parallel_workflow_joins_once_in_branch_order_retains_failures_and_cancels_all_branches` | 59 -> 71 | Full. `validateWorkflow` (TS) is asserted through the HTTP 400 for an empty block. The model-branch "no tools" check runs for every request at the end. |
| flows-http: routine flow dispatch joins once, snapshots edits and recovers interrupted steps | `pd_flows_http::routine_flow_dispatch_joins_once_snapshots_edits_and_recovers_interrupted_steps` | 36 -> 57 | Model-call counts leave out the harness step after `report_failure` / `no_update` (the model receives the tool result once more before the server stops the turn); the removed API runtime ended the turn inside its own provider loop. |
| webhooks: webhook receipts isolate, deduplicate, rotate and persist ignored events | `pd_webhooks::webhook_receipts_isolate_deduplicate_rotate_and_persist_ignored_events` | 35 -> 51 | Full. Signatures use the fixed clock. |
| webhooks: a signed GitHub issue starts one routine turn with untrusted reference data | `pd_webhooks::a_signed_github_issue_starts_one_routine_turn_with_untrusted_reference_data` | 25 -> 22 | Full; the per-response secret check runs on every browser reply. |
| shared-data-http: shared data agent tools journal once across resume, user reads/drop and restored workspace retain data | `pd_shared_data_http::shared_data_agent_tools_journal_once_across_resume_user_reads_drop_and_restored_workspace_retain_data` | 17 -> 21 | Full. Restore uses `minarbot-server restore workspace`. |
| files: files preserve bytes, scope, retry identity and archive boundaries | `pd_files::files_preserve_bytes_scope_retry_identity_and_archive_boundaries` | 42 -> 41 | Full. |
| files: text attachments reach only selected conversation and retain retry identity | `pd_files::text_attachments_reach_only_selected_conversation_and_retain_retry_identity` | 18 -> 32 | Full. |
| pdf-text: PDF HTTP preview and messages preserve source, scope, retry, routing, tools and restart | `pd_pdf_text::pdf_http_preview_and_messages_preserve_source_scope_retry_routing_tools_and_restart` | 28 -> 31 | Full. The PDF gate is the kit's async PDF bridge. |
| image-generation: image generation preserves scope, uncertain receipts and exact artifacts through restart | `pd_image_generation::image_generation_preserves_scope_uncertain_receipts_and_exact_artifacts_through_restart` | 27 -> 18 | Full; the image and chat contract checks (path, keys, body, tool offers) are violation lists asserted once each. |
| history-tools: agent history tools retrieve old originals and reject foreign conversations | `pd_history_tools::agent_history_tools_retrieve_old_originals_and_reject_foreign_conversations` | 24 -> 22 | Full. The undici network-error capture was Node diagnostics only. |
| skill-tools: skill tools page own enabled pins, reject foreign access and observe revocation | `pd_skill_tools::skill_tools_page_own_enabled_pins_reject_foreign_access_and_observe_revocation` | 15 -> 12 | Full. |
| workspace-shell: Rust shell confines cwd, filesystem and environment and enforces read-only and approval | `pd_workspace_shell::rust_shell_confines_cwd_filesystem_and_environment_and_enforces_read_only_and_approval` | 12 -> 12 | Full. |
| workspace-shell: Rust shell caps output, times out and stops descendants after exit and cancellation | `pd_workspace_shell::rust_shell_caps_output_times_out_and_stops_descendants_after_exit_and_cancellation` | 6 -> 5 | Full; the 2.1 s absence check is `wait::never`. |
| mcp-isolation: a hostile MCP output schema cannot block HTTP, exceed the worker deadline or resist cancellation | `pd_mcp_isolation::a_hostile_mcp_output_schema_cannot_block_http_exceed_the_worker_deadline_or_resist_cancellation` | 7 -> 4 | Full. |
| rust-desktop-http: harness stdio MCP bridge delivers Computer images, input, scoped access and human-control pause | `pd_rust_desktop_http::harness_stdio_mcp_bridge_delivers_computer_images_input_scoped_access_and_human_control_pause` | 14 -> 13 | Sessions are seeded in `mcp_server_tokens` (the Node test called `mcpServer.mintAgentSession`); the bridge is `minarbot-server --mcp-stdio`, whose refusal text is "MCP request failed." (JSON-RPC -32603) instead of the Node bridge's "Minarbot MCP request failed.". |
| rust-desktop-http: server advertises gated tools, sends model image pixels, protects viewer routes and refuses uncertain resume | `pd_rust_desktop_http::server_advertises_gated_tools_sends_model_image_pixels_protects_viewer_routes_and_refuses_uncertain_resume` | 41 -> 38 | Full after the uncertain-action fix below. |
| rust-desktop-http: HTTP control and user input enforce Origin, revision, validation and rate limits; paused turn resumes once | `pd_rust_desktop_http::http_control_and_user_input_enforce_origin_revision_validation_and_rate_limits_paused_turn_resumes_once` | 44 -> 39 | Model-call counts leave out the step after a pausing Computer tool (B1); the strict counts stay in Node. |
| rust-desktop-http: HTTP VNC relay enforces owner Host/Origin, tickets, viewer cap, live control fencing and stop cleanup | `pd_rust_desktop_http::http_vnc_relay_enforces_owner_host_origin_tickets_viewer_cap_live_control_fencing_and_stop_cleanup` | 13 -> 10 | Full, with the kit's WebSocket/RFB client. |
| rust-desktop-http: Always-on Computer uses a desktop slot, survives idle and restores on server restart | `pd_rust_desktop_http::always_on_computer_uses_a_desktop_slot_survives_idle_and_restores_on_server_restart` | 11 -> 12 | Real clock: the idle deadline must advance. |
| sidebar: sidebar validates unique membership, names, IDs and bounded settings | `pd_sidebar::sidebar_validates_unique_membership_names_ids_and_bounded_settings` | 4 -> 3 | Full (13 invalid layouts in a loop). |
| sidebar: sidebar HTTP edits retain agents/history through delete, conflicts, retry, restart and backup | `pd_sidebar::sidebar_http_edits_retain_agents_history_through_delete_conflicts_retry_restart_and_backup` | 26 -> 33 | Full. |
| sidebar: sidebar summary bounds entries and text, redacts credentials, and selects latest explicit remote messages | `pd_sidebar::sidebar_summary_bounds_entries_and_text_redacts_credentials_and_selects_latest_explicit_remote_messages` | 12 -> 12 | The provider credential is the gateway instance's secret launch variable; #87 removed the `apiKey` provider account. |
| channel-context (automatic): channel context names lead for automatic routing and execution | `pd_channel_context::channel_context_names_lead_for_automatic_routing_and_execution` | 15 -> 13 | Full. |
| channel-context (explicit): channel context names lead for explicit execution | `pd_channel_context::channel_context_names_lead_for_explicit_execution` | 15 -> 13 | Full. |
| agent-reply-capture: new user turns capture only ready same-scope group replies once across retries and restart | `pd_agent_reply_capture::new_user_turns_capture_only_ready_same_scope_group_replies_once_across_retries_and_restart` | 35 -> 37 | Full after the messaging fix below. The group receipt is found by content: the gateway peer replays earlier tool results as assistant messages. |
| guide-infra: Guide infrastructure requires approval and keeps credentials out of replies, storage and logs | `pd_guide_infra::guide_infrastructure_requires_approval_and_keeps_credentials_out_of_replies_storage_and_logs` | 26 -> 25 | #87 dropped `providerAccount`. The server gets the system `TMPDIR`: the askpass socket path under the kit's per-test TMPDIR exceeds the Unix socket limit. |
| local-projects: Rust project folders reject system, traversal, home, credential, data and symlink paths before creation | `pd_local_projects::rust_project_folders_reject_system_traversal_home_credential_data_and_symlink_paths_before_creation` | 3 -> 3 | `HOME` is the test root, so the data folder sits inside the Owner home as in Node. |
| local-projects: Rust project CRUD preserves folder ownership, revision fences, exact replay and files on delete | `pd_local_projects::rust_project_crud_preserves_folder_ownership_revision_fences_exact_replay_and_files_on_delete` | 22 -> 21 | Full. |
| local-projects: Rust project grants resolve by room or selected side thread folder and fail closed after revocation | `pd_local_projects::rust_project_grants_resolve_by_room_or_selected_side_thread_folder_and_fail_closed_after_revocation` | 8 -> 8 | A read-only write returns the tool's 403 to the model and the turn ends `done` (the API runtime failed the turn); revocation is refused at admission with 403 "not a project member" because every send now resolves its harness workspace first. |
| local-projects: built-in CodeGraph indexes projects and serves API turns without a CLI | `pd_local_projects::built_in_codegraph_indexes_projects_and_serves_api_turns_without_a_cli` | 16 -> 15 | The Node case skipped without bubblewrap; the Rust test requires `codegraphAvailable`. The gateway peer mounts the `codegraph` stdio server as Codex does. |
| local-projects: project HTTP Owner rules, direct-chat isolation, read/write enforcement, env fallback, per-folder Git and restart receipts | `pd_local_projects::project_http_owner_rules_direct_chat_isolation_read_write_enforcement_env_fallback_per_folder_git_and_restart_receipts` | 28 -> 32 | Read-only write as above. The peer's own log and session files are in `.git/info/exclude`. |
| local-projects: project harness cwd and read-only native escalation denial; direct chat uses the managed folder | `pd_local_projects::project_harness_cwd_and_read_only_native_escalation_denial_direct_chat_uses_the_managed_folder` | 7 -> 7 | Bound to explicit fake Codex/Claude instances: once a gateway was configured, `runtime: codex` keeps meaning the gateway instance. |
| local-projects: Rust managed folder retry recovers durable intent after transaction failure without deleting files | `pd_local_projects::rust_managed_folder_retry_recovers_durable_intent_after_transaction_failure_without_deleting_files` | 4 -> 4 | Full. |
| local-projects: standalone and P2 migration preserves channel IDs, history, read state, memory, files and legacy sidebar through restart and backup restore | `pd_local_projects::standalone_and_p2_migration_preserves_channel_ids_history_read_state_memory_files_and_legacy_sidebar_through_restart_and_backup_restore` | 28 -> 25 | Full; snapshots via `backup::workspace_snapshot`. |
| local-projects: section commands require revisions and request IDs, inherit members, reorder, archive and replay across restart | `pd_local_projects::section_commands_require_revisions_and_request_ids_inherit_members_reorder_archive_and_replay_across_restart` | 18 -> 19 | Full. |
| local-projects: folderless project and side threads isolate host grants, accept folders later, and archive retains configuration | `pd_local_projects::folderless_project_and_side_threads_isolate_host_grants_accept_folders_later_and_archive_retains_configuration` | 17 -> 14 | Folderless sends are refused with 503 "Coding runtime requires a workspace for this conversation." (before #87 only harness runtimes got this; now every agent is one). |
| local-projects: Rust project migration rolls back ownership and retries without touching standalone data | `pd_local_projects::rust_project_migration_rolls_back_ownership_and_retries_without_touching_standalone_data` | 7 -> 8 | Full. |
| local-projects: Rust migrates a standalone channel with a colliding ID into its own project | `pd_local_projects::rust_migrates_a_standalone_channel_with_a_colliding_id_into_its_own_project` | 6 -> 4 | Full. |
| local-projects: Rust project members retain added order through reload and edit | `pd_local_projects::rust_project_members_retain_added_order_through_reload_and_edit` | 2 -> 2 | Full. |
| local-routines: local routines require project membership and a report section belonging to that project | `pd_local_routines::local_routines_require_project_membership_and_a_report_section_belonging_to_that_project` | 4 -> 11 | Full. |
| local-routines: Rust upgrades legacy routine timing without publishing earlier runs | `pd_local_routines::rust_upgrades_legacy_routine_timing_without_publishing_earlier_runs` | 3 -> 3 | Full (two restarts). |
| local-routines: project routine runs use project folders and post one system report per outcome across retry and restart | `pd_local_routines::project_routine_runs_use_project_folders_and_post_one_system_report_per_outcome_across_retry_and_restart` | 24 -> 30 | Folders live in the kit's Owner home. |
| local-routines: scheduled cron catches up once and unavailable project folders do not starve other routines | `pd_local_routines::scheduled_cron_catches_up_once_and_unavailable_project_folders_do_not_starve_other_routines` | 13 -> 11 | `nextScheduleTimes` (TS) is computed in the test: 09:00 Asia/Jakarta is 02:00 UTC. |

### Product bugs fixed

| Bug | Case | Cause and fix |
| --- | --- | --- |
| Group requests held their conversation | agent-reply-capture | A session `send_message` with `expects_reply` polled up to 30 s inside its MCP call, so the harness turn stayed active and blocked the conversation; in test mode `messaging::run` also skipped queueing the source turn (c9a9e0f). New user turns could not run or capture ready replies. The source turn is now queued in every mode and the MCP call returns the waiting receipt at once, like `wait_task`; the scheduler resumes it when the replies settle (`turns/messaging.rs`, `mcp/server.rs`). |
| Failed Computer actions looked retryable | rust-desktop-http (uncertain resume) | A native action is journaled as uncertain before it runs, but a failure returned the raw error to the harness as an ordinary tool error, so the model could repeat the input and the turn ended `done`. Failures after the journal now return the uncertain outcome (502), as MCP tool calls do (`computer/control.rs`). |

### Product bugs kept

| Bug | Kept Node assertion | Cause |
| --- | --- | --- |
| B1 | `app/rust-desktop-http.test.ts` "HTTP control and user input ..." `assert.equal(providerCalls, 1)`, `2`, `4` | Same cause as B1: `computer_request_user` (and a click under human control) pauses the turn and answers `waiting_for_user` at once, and the harness takes one more model step. The Rust test fails that step at once and leaves it out of the counts. |

### Kit changes

New files: `parked` (requests held per key and answered later), `webhook`
(HMAC-SHA256, held deliveries), `upload` (chunked uploads), `backup`
(download, `restore workspace`, `workspace_snapshot`), `images` (image
provider on raw TCP), `mcp_server` (external MCP server with held calls),
`pdf` (PDF fixtures, async PDF gate), `desktop` (desktop fixture runtime),
`vnc` (WebSocket/RFB client), `fake/stdio_mcp` (stdio MCP client) and
`reap` (dropping a root kills the processes left in it). Edits:
`bridge` gained `pdf_async` and `operations_async`; the gateway peer mounts
extra stdio MCP servers from `thread/start` (G19); `Server` stops its
process before releasing its root.

### Notes

- Two Unix socket paths exceed the 108-byte limit under long `TMPDIR` or
  data paths: the Guide SSH askpass socket (`$TMPDIR/minarbot-ssh-<32
  hex>/password`) and the desktop proxy socket (`<leaseDir>/v-<32
  hex>/proxy.sock`). Tests use short directories; the product fails with
  "outcome is uncertain" or a storage error on such hosts.

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
