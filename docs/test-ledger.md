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
| `app/rust-desktop-http.test.ts` | 5 | 5 | 123 -> 112 | 0 |
| `app/sidebar.test.ts` | 3 | 3 | 42 -> 48 | 0 |
| `app/channel-context.test.ts` | 1 declaration / 2 runtime cases | 2 | 15 -> 13 per case | 0 |
| `app/agent-reply-capture.test.ts` | 1 | 1 | 35 -> 37 | 0 |
| `app/guide-infra.test.ts` | 1 | 1 | 26 -> 25 | 0 |
| `app/local-projects.test.ts` | 13 | 13 | 166 -> 164 | 0 |
| `app/local-routines.test.ts` | 4 | 4 | 44 -> 55 | 0 |
| Total | 42 | 42 | 802 -> 858 | 0 |

Every Node file is deleted. `app/rust-desktop-http.test.ts` kept one case
for B1 until the #89 integration below fixed it.

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
| workspace-shell: Rust shell confines cwd, filesystem and environment and enforces read-only and approval | `pd_workspace_shell::rust_shell_confines_cwd_filesystem_and_environment_and_enforces_read_only_and_approval` | 12 -> 12 | Full. Since B8, plain commands on a writable binding run without a card: the check asserts no approval, and the denied command adds `chmod` so it still needs one (#89 integration). |
| workspace-shell: Rust shell caps output, times out and stops descendants after exit and cancellation | `pd_workspace_shell::rust_shell_caps_output_times_out_and_stops_descendants_after_exit_and_cancellation` | 6 -> 5 | Full; the 2.1 s absence check is `wait::never`. Since B8 the commands run without a card, which each step asserts (#89 integration). |
| mcp-isolation: a hostile MCP output schema cannot block HTTP, exceed the worker deadline or resist cancellation | `pd_mcp_isolation::a_hostile_mcp_output_schema_cannot_block_http_exceed_the_worker_deadline_or_resist_cancellation` | 7 -> 4 | Full. |
| rust-desktop-http: harness stdio MCP bridge delivers Computer images, input, scoped access and human-control pause | `pd_rust_desktop_http::harness_stdio_mcp_bridge_delivers_computer_images_input_scoped_access_and_human_control_pause` | 14 -> 13 | Sessions are seeded in `mcp_server_tokens` (the Node test called `mcpServer.mintAgentSession`); the bridge is `minarbot-server --mcp-stdio`, whose refusal text is "MCP request failed." (JSON-RPC -32603) instead of the Node bridge's "Minarbot MCP request failed.". |
| rust-desktop-http: server advertises gated tools, sends model image pixels, protects viewer routes and refuses uncertain resume | `pd_rust_desktop_http::server_advertises_gated_tools_sends_model_image_pixels_protects_viewer_routes_and_refuses_uncertain_resume` | 41 -> 38 | Full after the uncertain-action fix below. |
| rust-desktop-http: HTTP control and user input enforce Origin, revision, validation and rate limits; paused turn resumes once | `pd_rust_desktop_http::http_control_and_user_input_enforce_origin_revision_validation_and_rate_limits_paused_turn_resumes_once` | 44 -> 39 | Full. Strict model-call counts (1, 2, 4) since the #89 integration fixed the B1 step after a pausing Computer tool. |
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

### Product bugs kept (fixed in the #89 integration)

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

## #89 fixture slice

Twenty-one Node suites that drove agent turns through the removed direct API
runtime, ported case by case to `crates/minarbot-e2e/tests/e2e/fx_*.rs`.
Agent replies come from `minarbot_testkit::gated::GatedModel` behind the
gateway peer (`runtime: "api"` in test mode). Assertion counts are
`assert*` calls plus explicit status checks in the Node case, and `ensure!`,
`*_expect` and status checks in the Rust test; parameterized Node loops count
each iteration as a case.

| Node file | Node runtime cases | Rust tests | Dropped | Node cases kept |
| --- | ---: | ---: | ---: | --- |
| `app/queue-edits.test.ts` | 3 | 3 | 0 | 0 (deleted) |
| `app/queue-order.test.ts` | 1 | 1 | 0 | 0 (deleted) |
| `app/agent-delete.test.ts` | 6 | 6 | 0 | 0 (deleted) |
| `app/agent-visibility.test.ts` | 1 | 1 | 0 | 1 (TS module, split out) |
| `app/script-discovery.test.ts` | 3 | 3 | 0 | 0 (deleted) |
| `app/script-messages.test.ts` | 1 | 1 | 0 | 0 (deleted) |
| `app/flow-tools.test.ts` | 1 | 1 | 0 | 0 (deleted) |
| `app/routine-tools.test.ts` | 1 | 1 | 0 | 0 (deleted) |
| `app/artifacts.test.ts` | 1 | 1 | 0 | 0 (deleted) |
| `app/avatar-tools.test.ts` | 1 | 1 | 0 | 0 (deleted) |
| `app/site-tools.test.ts` | 1 | 1 | 0 | 0 (deleted) |
| `app/base-instructions-scenario.test.ts` | 1 | 1 | 0 | 0 (deleted) |
| `app/turn-waits.test.ts` | 5 | 5 | 0 | 0 (deleted) |
| `app/acp-harness.test.ts` | 17 | 17 | 0 | 0 (deleted) |
| `app/agent-delegation.test.ts` | 19 | 19 | 0 | 0 (deleted) |
| `app/composio.test.ts` | 3 | 3 | 0 | 1 (TS module, split out) |
| `app/dispatch.test.ts` | 11 | 10 | 0 | 1 (TS module) |
| `app/turn-stream.test.ts` | 2 | 1 | 0 | 1 (TS module) |
| `app/connector-task-files.test.ts` | 6 | 6 | 0 | 0 (deleted) |
| `app/workflow-files.test.ts` | 8 | 8 | 0 | 0 (deleted) |
| `app/rust-connectors.test.ts` | 13 | 13 | 0 | 0 (deleted) |
| Total | 105 | 103 | 0 | 4 TS-module cases (the B1, B2 and B7 cases were ported in the #89 integration) |

"Dropped" counts whole cases; assertions dropped for removed behaviour are
named per case below. Partly dropped behaviour: the malformed-JSON tool
arguments iteration (routine-tools), the per-request system prompt roster
refresh and capability epoch (team awareness), and turn failure on refused or
failed MCP tool calls (several cases), all removed with the #87 API runtime.

### `app/queue-edits.test.ts` (deleted)

| Node case | Rust test | Asserts | Notes |
| --- | --- | ---: | --- |
| queue edit holds survive restart, preserve order and original receipt, reject competing editors and completed cancellations | `fx_queues::queue_edit_holds_survive_restart_preserve_order_and_original_receipt_reject_competing_editors_and_completed_cancellations` | 37 -> 32 | Full. The Node count includes three helper asserts. |
| abandoned edit lease expires and drains its message and following queue in order | `fx_queues::abandoned_edit_lease_expires_and_drains_its_message_and_following_queue_in_order` | 7 -> 4 | Full; the Node count includes the request helper. |
| Rust expires migrated permanent queue holds and fences the prior revision once | `fx_queues::rust_expires_migrated_permanent_queue_holds_and_fences_the_prior_revision_once` | 7 -> 7 | Full. Lease deadlines use SQLite's wall clock, so the seeded deadline does too. |

### `app/queue-order.test.ts` (deleted)

| Node case | Rust test | Asserts | Notes |
| --- | --- | ---: | --- |
| queue order survives restart, preserves transcript order, blocks edits and scopes, supplies reordered context | `fx_queues::queue_order_survives_restart_preserves_transcript_order_blocks_edits_and_scopes_supplies_reordered_context` | 30 -> 24 | Full. The two provider-side Node asserts (summary before `EARLY LAST`, `SECOND` before `FIRST`) fail the model step instead, which fails the turn the test waits on. The Node count includes three helper asserts. |

### `app/agent-delete.test.ts` (deleted)

| Node case | Rust test | Asserts | Notes |
| --- | --- | ---: | --- |
| delete cascades every agent FK, scoped data, workflows, desktop and journals, preserving room history and shared resources | `fx_agents::delete_cascades_every_agent_fk_scoped_data_workflows_desktop_and_journals_preserving_room_history_and_shared_resources` | 21 -> 21 | Full. |
| delete guards last agent, active/paused/queued work, unknown IDs, cross origin, bearer/peer/script views and hub grant ownership | `fx_agents::delete_guards_last_agent_active_paused_queued_work_unknown_ids_cross_origin_bearer_peer_script_views_and_hub_grant_ownership` | 10 -> 10 | Full. |
| delete preserves coordination journals until another agent finishes its dependent work | `fx_agents::delete_preserves_coordination_journals_until_another_agent_finishes_its_dependent_work` | 5 -> 5 | Full. |
| running provider work must settle before delete; transactional failure preserves agent and desktop for retry | `fx_agents::running_provider_work_must_settle_before_delete_transactional_failure_preserves_agent_and_desktop_for_retry` | 11 -> 8 | Full; `assert(started)` is the parked model request, the Node count includes helper asserts. |
| paired peer cannot delete owner agents; deletion on owner refreshes peer roster | `fx_agents::paired_peer_cannot_delete_owner_agents_deletion_on_owner_refreshes_peer_roster` | 6 -> 4 | Full: issued token, 403, 404, refreshed roster. Both servers use `peer::PeerBridge` (token issuer and loopback `peer-http`) and start through `server::start_pair`. |
| owned Git checkpoint refs clean up durably without removing project files; failed cleanup retries | `fx_agents::owned_git_checkpoint_refs_clean_up_durably_without_removing_project_files_failed_cleanup_retries` | 6 -> 6 | Full. |

### `app/agent-visibility.test.ts` (kept: TS module)

| Node case | Rust test | Asserts | Notes |
| --- | --- | ---: | --- |
| hidden agents finish active work, hold queue across restart, reject new work and skip missed schedules | `fx_agents::hidden_agents_finish_active_work_hold_queue_across_restart_reject_new_work_and_skip_missed_schedules` | 33 -> 27 | Full, with the strict `calls.length === 6` since the #89 integration. The 7 `visibilityCommand` asserts test `app/shared/agent-visibility.ts` and stay in Node as their own TS-module case ("visibility command accepts an exact hide command and rejects malformed input"). |

### `app/script-discovery.test.ts` (deleted)

| Node case | Rust test | Asserts | Notes |
| --- | --- | ---: | --- |
| duplicate discovery startup cannot interrupt the owning server task | `fx_scripts::duplicate_discovery_startup_cannot_interrupt_the_owning_server_task` | 4 -> 2 | Full: the duplicate start fails and the owning turn keeps running. The duplicate start shares the running server's slot (`TestRoot::start_error_beside`). |
| private script discovery follows live grants, rotates on restart and preserves foreign files | `fx_scripts::private_script_discovery_follows_live_grants_rotates_on_restart_and_preserves_foreign_files` | 27 -> 22 | Full: the fresh token is proven on `/api/scripts/agents/{id}/provider` since B2 is fixed (#89 integration). |
| discovery rejects unsafe directories and symlink destinations without changing targets | `fx_scripts::discovery_rejects_unsafe_directories_and_symlink_destinations_without_changing_targets` | 7 -> 6 | Full. |

### `app/script-messages.test.ts` (deleted)

| Node case | Rust test | Asserts | Notes |
| --- | --- | ---: | --- |
| script messages and answers share grants, scope, receipts, redaction and rate admission | `fx_scripts::script_messages_and_answers_share_grants_scope_receipts_redaction_and_rate_admission` | 45 -> 50 | Full, with the strict `calls === 2` since the #89 integration. The fixed clock replaces the mocked `Date`. The 58 admitted messages each run one harness turn, so the drain before the restart waits 2 s per queued turn. |

### `app/flow-tools.test.ts` (deleted)

| Node case | Rust test | Asserts | Notes |
| --- | --- | ---: | --- |
| agent flow edits preserve ownership, revisions and ordered receipts across restart | `fx_tools::agent_flow_edits_preserve_ownership_revisions_and_ordered_receipts_across_restart` | 16 -> 13 | Full. The server's done check asks once more ("One verification reminder"); the Node API runtime kept the tool results in that request, so the model answers it with the final text. The Node count includes two helper asserts. |

### `app/routine-tools.test.ts` (deleted)

| Node case | Rust test | Asserts | Notes |
| --- | --- | ---: | --- |
| routine tools recover from invalid arguments, preserve owner scope and keep runtime faults fatal | `fx_tools::routine_tools_recover_from_invalid_arguments_preserve_owner_scope_and_keep_runtime_faults_fatal` | 28 -> 22 | Found F1 (fixed): channel turns were offered `list_routines` and `read_routine` again through MCP feature tools. Changed by #87: the malformed-JSON iteration `{` is dropped, because the harness rejects arguments that are not JSON before Minarbot sees the call (the API runtime parsed them itself); `null`, `[]` and `"cursor"` still reach Minarbot and get the 400. An unoffered `set_routine_flow` in a channel is refused over MCP (403) instead of failing the turn, and the model stops after the refusal. MCP refusals carry `status`, so the foreign-routine result is `{error, status: 400}`. |

### `app/artifacts.test.ts` (deleted)

| Node case | Rust test | Asserts | Notes |
| --- | --- | ---: | --- |
| generated text artifacts persist across interruption without duplicate creation | `fx_tools::generated_text_artifacts_persist_across_interruption_without_duplicate_creation` | 23 -> 20 | Full. Held provider responses are parked `GatedModel` requests; the Node count includes helper asserts. |

### `app/avatar-tools.test.ts` (deleted)

| Node case | Rust test | Asserts | Notes |
| --- | --- | ---: | --- |
| avatar tool scopes files, journals atomic changes and preserves user changes on restart | `fx_tools::avatar_tool_scopes_files_journals_atomic_changes_and_preserves_user_changes_on_restart` | 33 -> 27 | Found F2 (fixed): the refused (409) `set_avatar` call was not journaled. Changed by #87: an unoffered `set_avatar` in a channel and the journal-trigger storage failure no longer fail the turn; over MCP the harness gets the refusal (403) or the error (500) and the turn ends `done`. The test asserts those results and that the avatar is unchanged. The 512x512 generated image is `noise::noise_png`. |

### `app/site-tools.test.ts` (deleted)

| Node case | Rust test | Asserts | Notes |
| --- | --- | ---: | --- |
| site tools bind sent files and owner, journal results and never replay old mutation on restart | `fx_tools::site_tools_bind_sent_files_and_owner_journal_results_and_never_replay_old_mutation_on_restart` | 44 -> 33 | Changed by #87: an unoffered `publish_site` in a channel is refused over MCP (403) and the turn ends `done`; the test asserts the refusal. Everything else is full; the Node count includes three helper asserts. |

### `app/base-instructions-scenario.test.ts` (deleted)

| Node case | Rust test | Asserts | Notes |
| --- | --- | ---: | --- |
| synthetic scenarios record panel without duplicate writes, inline links, workspace context and approved shell | `fx_tools::synthetic_scenarios_record_panel_without_duplicate_writes_inline_links_workspace_context_and_approved_shell` | 29 -> 27 | Changed by #87: `<runtime_info>` names the Codex instance (`through the codex runtime, model fixture`) instead of `api`. The agent session for the native MCP call is minted by inserting a hashed token, as the Node MCP facade did. Since B8 a plain check on a writable workspace needs no approval, so the scripted check adds `chmod` to keep the approval step (#89 integration). |

### `app/turn-waits.test.ts` (deleted)

| Node case | Rust test | Asserts | Notes |
| --- | --- | ---: | --- |
| agent event waits release workers, survive restart, honor pause and accept once | `fx_waits::agent_event_waits_release_workers_survive_restart_honor_pause_and_accept_once` | 37 -> 25 | Full since the #89 integration: `wait_for_event` parks the turn without another model step, so `continuations` counts strictly. Changed by #87: `timeoutSeconds: 0` returns the 400 to the model instead of failing the turn. The backup restore runs `minarbot-server restore workspace` into a second root. Signatures use `signing::hmac_sha256_hex` and the fixed clock. A resume right after the pause retries only on "Previous request has not stopped yet" while the worker stops the parked run. |
| routine waits preserve artifacts and reject events cancelled before upload completes | `fx_waits::routine_waits_preserve_artifacts_and_reject_events_cancelled_before_upload_completes` | 26 -> 19 | Full since the #89 integration: both continuations count (the event and the one-time verification reminder), as in Node. The half-sent event is cancelled mid-upload (`SplitRequest`). |
| workflow cancellation closes waiting descendants of completed steps | `fx_waits::workflow_cancellation_closes_waiting_descendants_of_completed_steps` | 17 -> 10 | Full (shared helper `cancellation_closes_waiting_descendants`). |
| channel cancellation closes waiting descendants of completed steps | `fx_waits::channel_cancellation_closes_waiting_descendants_of_completed_steps` | 17 -> 10 | Full. |
| thread cancellation closes waiting descendants of completed steps | `fx_waits::thread_cancellation_closes_waiting_descendants_of_completed_steps` | 17 -> 10 | Full. |

### `app/acp-harness.test.ts` (deleted)

| Node case | Rust test | Asserts | Notes |
| --- | --- | ---: | --- |
| hermes: initialize, supervised session, stream segments, tool, one-use permission and done | `fx_acp::hermes_initialize_supervised_session_stream_segments_tool_one_use_permission_and_done` | 18 -> 18 | Full. The MCP token is the kit issuer's `SESSION_TOKEN`. The prompt now carries the transcript as `<minarbot-message>` tags, so the last tag is checked to be the user's `hello` instead of a trailing `user: hello` line. |
| antigravity: initialize, supervised session, stream segments, tool, one-use permission and done | `fx_acp::antigravity_initialize_supervised_session_stream_segments_tool_one_use_permission_and_done` | 18 -> 18 | Full. The MCP token is the kit issuer's `SESSION_TOKEN`. The prompt now carries the transcript as `<minarbot-message>` tags, so the last tag is checked to be the user's `hello` instead of a trailing `user: hello` line. |
| grok: initialize, supervised session, stream segments, tool, one-use permission and done | `fx_acp::grok_initialize_supervised_session_stream_segments_tool_one_use_permission_and_done` | 18 -> 18 | Full. The MCP token is the kit issuer's `SESSION_TOKEN`. The prompt now carries the transcript as `<minarbot-message>` tags, so the last tag is checked to be the user's `hello` instead of a trailing `user: hello` line. |
| opencode: initialize, supervised session, stream segments, tool, one-use permission and done | `fx_acp::opencode_initialize_supervised_session_stream_segments_tool_one_use_permission_and_done` | 18 -> 18 | Full. The MCP token is the kit issuer's `SESSION_TOKEN`. The prompt now carries the transcript as `<minarbot-message>` tags, so the last tag is checked to be the user's `hello` instead of a trailing `user: hello` line. |
| gemini: initialize, supervised session, stream segments, tool, one-use permission and done | `fx_acp::gemini_initialize_supervised_session_stream_segments_tool_one_use_permission_and_done` | 18 -> 18 | Full. The MCP token is the kit issuer's `SESSION_TOKEN`. The prompt now carries the transcript as `<minarbot-message>` tags, so the last tag is checked to be the user's `hello` instead of a trailing `user: hello` line. |
| kiro: initialize, supervised session, stream segments, tool, one-use permission and done | `fx_acp::kiro_initialize_supervised_session_stream_segments_tool_one_use_permission_and_done` | 18 -> 18 | Full. The MCP token is the kit issuer's `SESSION_TOKEN`. The prompt now carries the transcript as `<minarbot-message>` tags, so the last tag is checked to be the user's `hello` instead of a trailing `user: hello` line. |
| ACP resume suppresses replay and keeps cwd and credentials scoped | `fx_acp::acp_resume_suppresses_replay_and_keeps_cwd_and_credentials_scoped` | 6 -> 6 | Full. |
| ACP file requests reject outside paths and symlinks, require write approval | `fx_acp::acp_file_requests_reject_outside_paths_and_symlinks_require_write_approval` | 5 -> 5 | Full. |
| ACP read-only workspace rejects file writes without approval | `fx_acp::acp_read_only_workspace_rejects_file_writes_without_approval` | 3 -> 3 | Full. |
| Pi RPC streams, requests tool approval, selects model, and cancels | `fx_acp::pi_rpc_streams_requests_tool_approval_selects_model_and_cancels` | 7 -> 7 | Full. |
| ACP denied permission never grants persistent option | `fx_acp::acp_denied_permission_never_grants_persistent_option` | 3 -> 3 | Full. |
| ACP CRASH: fail bounded, no replay | `fx_acp::acp_crash_fail_bounded_no_replay` | 3 -> 3 | Full (shared helper `bounded_failure`). |
| ACP INVALID: fail bounded, no replay | `fx_acp::acp_invalid_fail_bounded_no_replay` | 3 -> 3 | Full. |
| ACP WAIT: fail bounded, no replay | `fx_acp::acp_wait_fail_bounded_no_replay` | 3 -> 3 | Full. |
| ACP cancel reaches child, kills detached tool process and preserves inspect diagnostics | `fx_acp::acp_cancel_reaches_child_kills_detached_tool_process_and_preserves_inspect_diagnostics` | 13 -> 12 | Full; the Node `assert(start)` is the `ok_or` on the process start time. |
| ACP runtimes work through server: approvals, segments and runtime picker | `fx_acp::acp_runtimes_work_through_server_approvals_segments_and_runtime_picker` | 6 -> 4 | Full; the Node count includes two helper asserts. |
| ACP room queue, checkpoints and stop preserve peer approvals | `fx_acp::acp_room_queue_checkpoints_and_stop_preserve_peer_approvals` | 6 -> 4 | Full; project folders must sit in the server's home, so the Git workspace is under `home/`. The Node count includes two helper asserts. |

### `app/agent-delegation.test.ts` (deleted)

| Node case | Rust test | Asserts | Notes |
| --- | --- | ---: | --- |
| delegation isolates child context and durably wakes idle parent exactly once | `fx_delegation::delegation_isolates_child_context_and_durably_wakes_idle_parent_exactly_once` | 12 -> 6 | Full; found F3 (fixed). The Node count includes four fixture helper asserts. |
| durable task wait: complete | `fx_delegation::durable_task_wait_complete` | 7 -> 7 | Full; found F4 (fixed). |
| durable task wait: timeout | `fx_delegation::durable_task_wait_timeout` | 7 -> 7 | Full. |
| durable task wait: restart-wait | `fx_delegation::durable_task_wait_restart_wait` | 7 -> 7 | Full, with the strict `childCalls === 2` since the #89 integration. |
| durable task wait: restart-child | `fx_delegation::durable_task_wait_restart_child` | 7 -> 7 | Full; the restart waits for the child's model request (the harness sends it just after the turn starts). |
| durable task wait: delegate-wait | `fx_delegation::durable_task_wait_delegate_wait` | 7 -> 7 | Full. |
| backend delegation limit: depth | `fx_delegation::backend_delegation_limit_depth` | 6 -> 6 | Full. |
| backend delegation limit: children | `fx_delegation::backend_delegation_limit_children` | 6 -> 6 | Changed by #87: the 409 returns to the model and the turn ends `done` with it instead of `failed`. |
| backend delegation limit: size | `fx_delegation::backend_delegation_limit_size` | 6 -> 6 | Full. |
| delegated task authority denies unrelated caller; cancel stops entire mixed-message tree | `fx_delegation::delegated_task_authority_denies_unrelated_caller_cancel_stops_entire_mixed_message_tree` | 5 -> 4 | Full; the Node count includes a helper assert. |
| team awareness refreshes role and bounded roster on next provider request | `fx_delegation::team_awareness_refreshes_role_and_bounded_roster_on_next_provider_request` | 5 -> 4 | Changed by #87: the provider client that rebuilt the system prompt (roster and capability epoch) before each request was removed; harness turns read the roster with `list_agents`. The test asserts the refreshed role and the bound through two `list_agents` calls in one turn; the epoch assertion is dropped. |
| delegate_task receives actionable rate error across conversations, restart and cooldown | `fx_delegation::delegate_task_receives_actionable_rate_error_across_conversations_restart_and_cooldown` | 3 -> 3 | Full. |
| send_message receives actionable rate error across conversations, restart and cooldown | `fx_delegation::send_message_receives_actionable_rate_error_across_conversations_restart_and_cooldown` | 3 -> 3 | Full. |
| nested async children: wait includes completion follow-up | `fx_delegation::nested_async_children_wait_includes_completion_follow_up` | 6 -> 6 | Full. |
| nested async children: cancel completed root with running descendants | `fx_delegation::nested_async_children_cancel_completed_root_with_running_descendants` | 6 -> 6 | Full. |
| owner later turns can inspect and wait without a duplicate completion wake | `fx_delegation::owner_later_turns_can_inspect_and_wait_without_a_duplicate_completion_wake` | 3 -> 3 | Full. |
| completion wakes a parent waiting for user input and preserves its pending question | `fx_delegation::completion_wakes_a_parent_waiting_for_user_input_and_preserves_its_pending_question` | 3 -> 3 | Full. |
| concurrent child limit counts tasks still waiting on their async descendants | `fx_delegation::concurrent_child_limit_counts_tasks_still_waiting_on_their_async_descendants` | 4 -> 4 | Full. |
| Rust counts active children of an owner completion turn against its inherited budget | `fx_delegation::rust_counts_active_children_of_an_owner_completion_turn_against_its_inherited_budget` | 3 -> 3 | Changed by #87: the limit refusal returns to the model, which ends the continuation `done` with it instead of `failed`. |

### `app/composio.test.ts` (kept: TS module)

| Node case | Rust test | Asserts | Notes |
| --- | --- | ---: | --- |
| connected apps end to end: catalog, accounts, grants, tools, approvals, receipts and no secrets | `fx_composio::connected_apps_end_to_end_catalog_accounts_grants_tools_approvals_receipts_and_no_secrets` | 70 -> 61 | Changed by #87: the unconfigured and ungranted turns no longer fail; the unoffered call is refused over MCP (403) and the turn ends `done`. The test asserts the refusal and that no Composio tool was offered. The `COMPOSIO_API_KEY` environment variable is the kit's key bridge (`Bridge::composio_key_cell`); server output replaces captured console logs. |
| installation API key: validation, encrypted persistence, receipts, runtime precedence and restart | `fx_composio::installation_api_key_validation_encrypted_persistence_receipts_runtime_precedence_and_restart` | 39 -> 38 | Full; the environment key changes through the key cell between requests, as `process.env` did. |
| Rust key replacement and removal reset identity, grants, policies and pending calls atomically | `fx_composio::rust_key_replacement_and_removal_reset_identity_grants_policies_and_pending_calls_atomically` | 21 -> 21 (+6 TS) | Changed by #87: the Node API runtime kept the tool list of the turn's start, so the call held across the reset was refused ("was not offered or access was revoked"). A harness call resolves tools again over MCP, so after the reset and the new grant the write waits for approval; the test denies it and asserts nothing was sent. Every pending-call and reset assertion is full. The 6 `restoreConnectedAppCommand` asserts test `app/src/connected-app-command.ts` and stay in Node as their own TS-module case. |

### `app/dispatch.test.ts` (kept: TS module)

| Node case | Rust test | Asserts | Notes |
| --- | --- | ---: | --- |
| startup failure reports child output and releases the provider and directory | `fx_dispatch::startup_failure_reports_child_output_and_releases_the_provider_and_directory` | 3 -> 4 | Ported to the kit's startup error: exit status, stdout and stderr tails, no provider call, and the test root is removed when it drops. |
| steer is injected once at next tool-loop safe point, durable and idempotent | `fx_dispatch::steer_is_injected_once_at_next_tool_loop_safe_point_durable_and_idempotent` | 15 -> 19 | Ported in the #89 integration with B7 behaviour: a steer is a linked restart (`dispatch_mode: restart`, "cannot steer" label) that cancels the loop at once, so the loop runs in the steer turn. Ported: receipt, replay, conflicting mode, held editor (409 once the restart runs), call count before release, the linked source, reply, label, saved file, the reminder as last call, one guidance copy per later request, receipt replay without a model call, guidance in later context and in a fork of the steer turn without later requests. Changed: 5 calls instead of 4 (the cancelled root keeps its first call) and the guidance is the restart's own request instead of a delivered steer. |
| steer before final answer and steer after completion each become one queued turn | `fx_dispatch::steer_before_final_answer_and_steer_after_completion_each_become_one_queued_turn` | 5 -> 5 | Full. |
| restart cancels and links a new turn, carries partial text and saved results, no effect replay | `fx_dispatch::restart_cancels_and_links_a_new_turn_carries_partial_text_and_saved_results_no_effect_replay` | 12 -> 14 | Ported in the #89 integration with B7 behaviour: the steer restarts the partial turn at once, so the steer turn saves `effect.txt` and streams the partial before the explicit restart (targeting the steer turn) interrupts it. Everything else is as in Node: receipt target, reply, link, saved file on the cancelled turn, partial text, saved result, steer text and replay warning in the restart context, no tool replay, order before the queued message, receipt replay without a model call. Added: the steer receipt is a restart, and the root saved no file. |
| server restart mid-steer releases guidance once and never replays uncertain source | `fx_dispatch::server_restart_mid_steer_releases_guidance_once_and_never_replays_uncertain_source` | 2 -> 2 | Full. Since B7 the steer restarts the held source at once, so it is `cancelled` ("Stopped for restart.") instead of `interrupted` (#89 integration). |
| fork stores bounded immutable cutoff, same agent, idempotency and channel lineage | `fx_dispatch::fork_stores_bounded_immutable_cutoff_same_agent_idempotency_and_channel_lineage` | 25 -> 25 | Full. |
| composer recovery validates dispatch mode and target | Kept in Node | 3 -> 0 | Tests `app/src/conversation-command.ts`. |
| restart during uncertain MCP effect keeps its receipt and never replays external call | `fx_dispatch::restart_during_uncertain_mcp_effect_keeps_its_receipt_and_never_replays_external_call` | 6 -> 4 | Full; the external MCP server is `fx_mcp::LosingMcp`. The provider-side check counts as the last assert. |
| queued dispatch rejects stale revision, held editor, cross-conversation target and started message | `fx_dispatch::queued_dispatch_rejects_stale_revision_held_editor_cross_conversation_target_and_started_message` | 5 -> 5 | Full. |
| queued restart cancels captured generation; late steering stays queued without duplicate turn | `fx_dispatch::queued_restart_cancels_captured_generation_late_steering_stays_queued_without_duplicate_turn` | 4 -> 4 | Full. |
| terminal forks preserve partial answers; untitled chats name from first text and keep manual names | `fx_dispatch::terminal_forks_preserve_partial_answers_untitled_chats_name_from_first_text_and_keep_manual_names` | 3 -> 3 | Full. |

### `app/turn-stream.test.ts` (kept: TS module)

| Node case | Rust test | Asserts | Notes |
| --- | --- | ---: | --- |
| snapshot merge rejects stale calls, duplicate reconnects and late polling regressions | Kept in Node | 14 -> 0 | Tests `app/shared/turn-stream.ts`. |
| room polling retains active members beyond full history and orders prose/tool/final by attempt | `fx_stream::room_polling_retains_active_members_beyond_full_history_and_orders_prose_tool_final_by_attempt` | 31 -> 29 | Changed by #87: a harness turn is one provider call record, so the second model step is a new stream segment of the same `seq` (ordinals 0-3) instead of a new attempt with its own `seq`, and its revision grows instead of restarting at 0. `streamWork` is reimplemented in the test. Needs the gateway's live stream forwarding (G4). |

### `app/connector-task-files.test.ts` (deleted)

| Node case | Rust test | Asserts | Notes |
| --- | --- | ---: | --- |
| slack ordinary chat collects only requested teammate files (reply) | `fx_connectors::slack_ordinary_chat_collects_only_requested_teammate_files_reply` | 24 -> 23 | Full (shared helper `task_files`). The connector fixture is `platform::ConnectorFixture` (fake Slack API with Socket Mode, fault-injecting script proxy, `minarbot-server --connector-config`). |
| slack ordinary chat collects only requested teammate files (notice) | `fx_connectors::slack_ordinary_chat_collects_only_requested_teammate_files_notice` | 24 -> 23 | Full. |
| slack ordinary chat collects only requested teammate files (nested) | `fx_connectors::slack_ordinary_chat_collects_only_requested_teammate_files_nested` | 24 -> 23 | Full. |
| telegram ordinary chat collects only requested teammate files (reply) | `fx_connectors::telegram_ordinary_chat_collects_only_requested_teammate_files_reply` | 24 -> 23 | Full. |
| telegram ordinary chat collects only requested teammate files (notice) | `fx_connectors::telegram_ordinary_chat_collects_only_requested_teammate_files_notice` | 24 -> 23 | Full. |
| telegram ordinary chat collects only requested teammate files (nested) | `fx_connectors::telegram_ordinary_chat_collects_only_requested_teammate_files_nested` | 24 -> 23 | Full. |

### `app/workflow-files.test.ts` (deleted)

| Node case | Rust test | Asserts | Notes |
| --- | --- | ---: | --- |
| slack step workflow artifacts are scoped to owned run and retain partial bytes | `fx_connectors::slack_step_workflow_artifacts_are_scoped_to_owned_run_and_retain_partial_bytes` | 19 -> 21 | Full (shared helper `workflow_files`). |
| slack reply workflow artifacts are scoped to owned run and retain partial bytes | `fx_connectors::slack_reply_workflow_artifacts_are_scoped_to_owned_run_and_retain_partial_bytes` | 19 -> 21 | Full (shared helper `workflow_files`). |
| slack notice workflow artifacts are scoped to owned run and retain partial bytes | `fx_connectors::slack_notice_workflow_artifacts_are_scoped_to_owned_run_and_retain_partial_bytes` | 19 -> 21 | Full (shared helper `workflow_files`). |
| slack nested workflow artifacts are scoped to owned run and retain partial bytes | `fx_connectors::slack_nested_workflow_artifacts_are_scoped_to_owned_run_and_retain_partial_bytes` | 19 -> 21 | Full (shared helper `workflow_files`). |
| telegram step workflow artifacts are scoped to owned run and retain partial bytes | `fx_connectors::telegram_step_workflow_artifacts_are_scoped_to_owned_run_and_retain_partial_bytes` | 19 -> 21 | Full (shared helper `workflow_files`). |
| telegram reply workflow artifacts are scoped to owned run and retain partial bytes | `fx_connectors::telegram_reply_workflow_artifacts_are_scoped_to_owned_run_and_retain_partial_bytes` | 19 -> 21 | Full (shared helper `workflow_files`). |
| telegram notice workflow artifacts are scoped to owned run and retain partial bytes | `fx_connectors::telegram_notice_workflow_artifacts_are_scoped_to_owned_run_and_retain_partial_bytes` | 19 -> 21 | Full (shared helper `workflow_files`). |
| telegram nested workflow artifacts are scoped to owned run and retain partial bytes | `fx_connectors::telegram_nested_workflow_artifacts_are_scoped_to_owned_run_and_retain_partial_bytes` | 19 -> 21 | Full (shared helper `workflow_files`). |

### `app/rust-connectors.test.ts` (deleted)

| Node case | Rust test | Asserts | Notes |
| --- | --- | ---: | --- |
| Rust Composio approval recovery, revocation, schema checks and uncertain writes remain fenced | `fx_composio::rust_composio_approval_recovery_revocation_schema_checks_and_uncertain_writes_remain_fenced` | 21 -> 19 | Full; the remote that accepts and holds or drops the write is a `stub::Stub` (`Hang`, `Drop`). |
| Rust slack offline recovery preserves confirmed delivery and requires confirmation | `fx_connectors::rust_slack_offline_recovery_preserves_confirmed_delivery_and_requires_confirmation` | 10 -> 11 | Full; journal commands run through `connector::journal`. |
| Rust telegram offline recovery preserves confirmed delivery and requires confirmation | `fx_connectors::rust_telegram_offline_recovery_preserves_confirmed_delivery_and_requires_confirmation` | 10 -> 11 | Full; journal commands run through `connector::journal`. |
| Rust telegram runner reply through native turns | `fx_connectors::rust_telegram_runner_reply_through_native_turns` | 20 -> 22 | Full (shared helper `runner`; in file mode the connector stops once both the document and the reply arrived, where Node stopped 100 ms after the document): fake platform with Socket Mode reconnect, script proxy that maps script routes to UI routes and answers workflow calls itself. |
| Rust telegram runner question through native turns | `fx_connectors::rust_telegram_runner_question_through_native_turns` | 20 -> 22 | Full, with the strict `calls === 2` since the #89 integration. |
| Rust telegram runner file through native turns | `fx_connectors::rust_telegram_runner_file_through_native_turns` | 20 -> 22 | Full (shared helper `runner`; in file mode the connector stops once both the document and the reply arrived, where Node stopped 100 ms after the document): fake platform with Socket Mode reconnect, script proxy that maps script routes to UI routes and answers workflow calls itself. |
| Rust telegram runner workflow through native turns | `fx_connectors::rust_telegram_runner_workflow_through_native_turns` | 20 -> 22 | Full (shared helper `runner`; in file mode the connector stops once both the document and the reply arrived, where Node stopped 100 ms after the document): fake platform with Socket Mode reconnect, script proxy that maps script routes to UI routes and answers workflow calls itself. |
| Rust telegram runner media through native turns | `fx_connectors::rust_telegram_runner_media_through_native_turns` | 20 -> 22 | Full (shared helper `runner`; in file mode the connector stops once both the document and the reply arrived, where Node stopped 100 ms after the document): fake platform with Socket Mode reconnect, script proxy that maps script routes to UI routes and answers workflow calls itself. |
| Rust slack runner reply through native turns | `fx_connectors::rust_slack_runner_reply_through_native_turns` | 20 -> 22 | Full (shared helper `runner`; in file mode the connector stops once both the document and the reply arrived, where Node stopped 100 ms after the document): fake platform with Socket Mode reconnect, script proxy that maps script routes to UI routes and answers workflow calls itself. |
| Rust slack runner question through native turns | `fx_connectors::rust_slack_runner_question_through_native_turns` | 20 -> 22 | Full, with the strict `calls === 2` since the #89 integration. |
| Rust slack runner file through native turns | `fx_connectors::rust_slack_runner_file_through_native_turns` | 20 -> 22 | Full (shared helper `runner`; in file mode the connector stops once both the document and the reply arrived, where Node stopped 100 ms after the document): fake platform with Socket Mode reconnect, script proxy that maps script routes to UI routes and answers workflow calls itself. |
| Rust slack runner workflow through native turns | `fx_connectors::rust_slack_runner_workflow_through_native_turns` | 20 -> 22 | Full (shared helper `runner`; in file mode the connector stops once both the document and the reply arrived, where Node stopped 100 ms after the document): fake platform with Socket Mode reconnect, script proxy that maps script routes to UI routes and answers workflow calls itself. |
| Rust slack runner media through native turns | `fx_connectors::rust_slack_runner_media_through_native_turns` | 20 -> 22 | Full (shared helper `runner`; in file mode the connector stops once both the document and the reply arrived, where Node stopped 100 ms after the document): fake platform with Socket Mode reconnect, script proxy that maps script routes to UI routes and answers workflow calls itself. |


### Product bugs found and fixed in the fixture slice

| Bug | Fix | Found by |
| --- | --- | --- |
| F1 | MCP feature tools re-offered private-only tools (`list_routines`, `read_routine`, `list_skills`) to shared-room turns after the native filter withheld them. `turns::tools::PRIVATE_TOOLS` now drives both filters. | `fx_tools::routine_tools...` |
| F2 | Refused (4xx) native tool calls over MCP were not journaled, unlike the agent loop in `turns/execution.rs`; `harness::journal_refusal` records them. | `fx_tools::avatar_tool...` |
| F3 | MCP added `requestId` and `turn` to every session `delegate_task`; the native handler rejects unknown fields, so every harness delegation failed with 400. Only the feature tool gets them now. | `fx_delegation::*` |
| F4 | Harness sessions got the native `wait_task`, which marks the turn paused and waiting but is never resumed. Sessions now use the blocking feature tool, which records a durable delegation wait. | `fx_delegation::durable_task_wait_*` |

B1 also covered `wait_for_event`: it returned `null` at once and the harness took one more model step. The #89 integration fixed it (see there); `fx_support::after_pause` and `pause_reply_for` are gone and every count is strict.

Open finding (not fixed, #87 behaviour change): a Composio call held across a key reset is offered again for the new installation over MCP, because harness calls resolve tools per request; the Node API runtime refused it with its turn-start tool list. The Rust test asserts that the call waits for approval and is never dispatched.

## #89 stale-b ports

The Node suites of the stale-b slice broke when #87 removed the direct API
runtime. They were rewritten case by case in `crates/minarbot-e2e/tests/e2e/sb_*.rs`.
Agent turns run through the gateway instance (`ServerConfig::gateway`), whose
model is the kit's `ControlModel` (`crates/minarbot-testkit/src/control.rs`):
it can hold a request and answer it later with a chosen body, as the Node
provider servers did. Assertion counts are static assertion sites (Node
`assert*` calls and expected-status requests; Rust `ensure!`, `*_expect`,
`wait_status`); assertions in loops count once.

Behaviour that changed with #87, and how the ports treat it:

- One harness call is one usage row and one `activity.calls` entry; a tool
  loop inside it is not a separate provider request.
- A tool the turn is not offered, or a tool error, reaches the model as an MCP
  tool error and the turn finishes; the direct client failed the turn.
- A harness CLI always exposes its MCP tools, also to summary and routing
  requests.
- Backup-model fallback, provider accounts, direct provider HTTP error
  mapping, `/api/models` discovery and provider families are removed.

### Kit changes

- New `control.rs`: `ControlModel` (deferred answers, raw bodies, HTTP
  statuses, verbatim tool arguments, a stalled SSE stream, per-answer usage).
- New `stub.rs`: `HttpStub`, a closure-backed HTTP server for remote MCP
  servers and similar external fakes, with a lost-reply response.
- New `connector_fixture.rs`: `SlackFixture` (fake Slack Web API and Socket
  Mode WebSocket, `--connector-config` runner) and `journal` for
  `--connector-journal-config`. The kit's `axum` dependency gains the `ws`
  feature the server already uses.
- `control.rs` also answers streamed requests with SSE, scripts
  `GET /v1/models` (`respond_catalog`, redirects, held catalog requests) and
  has stream error and cut answers; `fake/gateway.rs` fails on an SSE `error`
  event.
- `fake/native.rs`: `ENV_PRESENT` / `ENV_ABSENT` check `PROJECT_TEST_TOKEN`
  in the harness process (the Node `project-integrations` fixture patch).
- `fake/gateway.rs`: streamed content deltas are forwarded as soon as their
  SSE line arrives (the Node fixture buffered the whole body, so a stalled
  provider never showed partial text), an answer without `usage` reports
  no token usage instead of zeros, and an MCP result with a null
  `structuredContent` but Minarbot fields (`files`, `omitted`, `uncertain`)
  reaches the model as its JSON instead of the text `null`.

### Product bugs fixed

| Bug | Fix |
| --- | --- |
| S1 | `crates/shared/skill_files.rs` accepted duplicate `SKILL.md` frontmatter keys (`name: x` then `name: y` kept `y`), because serde_json maps keep the last key. The TypeScript parser rejected them. The frontmatter is now parsed as a YAML mapping first, which rejects duplicates. |
| S2 | a8cfcac removed the local `read_provider_settings` executor with provider accounts but kept offering the tool in private conversations, so every call failed. `turns/tools.rs` executes it again with the instance binding (`revision`, `providerInstance`, `model`, `configured`), the shape of the canonical executor, private-only and without arguments. |
| S3 | `turns/harness.rs` (`specs`) offered `edit_profile` to harness turns in channels, where the executor refuses it with 403; the direct client offered it only in private conversations. It is now private-only, like the skill tools (ef64cd9). |
| S4 | a8cfcac deleted the direct-provider path that appended the team roster (`## Messaging other agents`, capability epoch, teammates) to answer prompts, leaving `messaging::awareness` unused; harness agents no longer saw their teammates. `turns/provider.rs` (`call`) appends it again under the old conditions: answer calls only (not memory, compaction, goal judge or routing), not profile setup, and only when the turn may message teammates. |
| S5 | `harness/store.rs` (`run`, f89f751) treated a turn that is not `running` as paused and returned an empty reply. Background memory calls run on finished turns, so every extraction and consolidation got an empty answer and failed after retries. The check now skips memory phases, whose turn lookup already allows finished turns. |
| S6 | Harness turns run without an isolated home, so a mise shim launch got the owner's `HOME` and `configure_mise` never set `MISE_STATE_DIR`; mise would record trust in the owner's state dir, which the probe path avoids. Mise launches without a home now get a private one per binding under the harness homes root. |
| S7 | A paused turn whose provider instance was disabled still ran on it when its question was answered (`ready()` checks only at admission). `harness/store.rs` refuses a disabled instance before launch. |
| S8 | Instance model discovery returned a catalog that contained the instance's secret (the provider key as a model id) to the browser. `model_catalog::models_for_instance` rejects such a catalog as invalid (502), as the direct client did; `provider_instances::secret_values` lists the decrypted secrets. |

### `app/compaction.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| incremental compaction keeps source scopes, originals, provenance and failure fallback | `sb_compaction::incremental_compaction_keeps_source_scopes_originals_provenance_and_failure_fallback` | 24 -> 20 | Dropped: `data.tools === undefined` on the summary request (a harness CLI always exposes MCP tools). The 1 s `timeout` still bounds each harness call, so a stalled summary falls back to history. |
| Rust compaction fences stale responses, changed sources, cancelled turns and future watermarks | `sb_compaction::rust_compaction_fences_stale_responses_changed_sources_cancelled_turns_and_future_watermarks` | 14 -> 17 | Full. |

### `app/crash.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| workflow SIGKILL preserves uncertain step without replay across two restarts | `sb_crash::workflow_sigkill_preserves_uncertain_step_without_replay_across_two_restarts` | 16 -> 13 | Full; `Server::crash` replaces the killed bridge child process. |
| SIGKILL after delegated provider acceptance does not replay parent or child | `sb_crash::sigkill_after_delegated_provider_acceptance_does_not_replay_parent_or_child` | 11 -> 8 | Full: 2 requests are in flight at the crash, as in Node, since the #89 integration parks a delegating parent without another model step; none is repeated after restart. |

### `app/flows.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| routine flow graph rejects cycles, foreign agents and disconnected branches | `sb_flows::routine_flow_graph_rejects_cycles_foreign_agents_and_disconnected_branches` | 4 -> 6 | The Node case called `app/src/flows.ts`; the same graphs go through `POST /api/routines/{id}/flow` (Rust `automation/flows.rs`). |
| routine flow fan-in waits for every input and settles all-failed branches | `sb_flows::routine_flow_fan_in_waits_for_every_input_and_settles_all_failed_branches` | 18 -> 14 | Full. Step model counts exclude the harness step after a `report_failure` result. |

### `app/skill-files.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| skill folders preserve exact files and reject invalid or unsupported inputs as a whole | `sb_skills::skill_folders_preserve_exact_files_and_reject_invalid_or_unsupported_inputs_as_a_whole` | 11 -> 7 | The parser checks go through `POST /api/skills` (`crates/shared/skill_files.rs`); found S1. Dropped: the export-bundle and draft-restore checks of the browser modules `exportedSkillFiles`/`restoreSkillCommand`, replaced by the Leptos importer. |
| folder revisions, receipts and pinned file tools survive reload without foreign or revoked access | `sb_skills::folder_revisions_receipts_and_pinned_file_tools_survive_reload_without_foreign_or_revoked_access` | 33 -> 27 | In a channel `read_skill_file` is not offered and a call returns no content; the turn finishes instead of failing. |

### `app/transcript.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| turn list exposes scoped read-only activity metadata in direct, thread and channel views | `sb_transcript::turn_list_exposes_scoped_read_only_activity_metadata_in_direct_thread_and_channel_views` | 16 -> 11 | Dropped: `activity.fallback_model` (#87). One harness call per turn, so `activity.calls` has 1 entry (Node: 3 HTTP requests). |
| Stop timestamps survive repeated cancellation and server restart | `sb_transcript::stop_timestamps_survive_repeated_cancellation_and_server_restart` | 9 -> 5 | Full, on the real clock. Needed the gateway streaming fix (the Node case failed on the buffering fixture). |

### `app/usage.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| Rust usage returns exact decimal totals, unknown counts and stable pages | `sb_usage::rust_usage_returns_exact_decimal_totals_unknown_counts_and_stable_pages` | 9 -> 8 | Full. |
| usage records each provider call including tool loops, missing counts and failure | `sb_usage::usage_records_each_provider_call_including_tool_loops_missing_counts_and_failure` | 22 -> 20 | One row per harness call (the tool loop is one row). Dropped: the `finish_reason: length` mode, a direct-client rule. |

### `app/mcp-context.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| MCP resources and prompts require exact grants and remain untrusted journaled data | `sb_mcp_context::mcp_resources_and_prompts_require_exact_grants_and_remain_untrusted_journaled_data` | 66 -> 63 | Refused calls (no grant, wrong agent, target override, bad prompt arguments, drift, revoked template) reach the model as MCP errors and the turn finishes; each step asserts the error result and that the remote saw no read. A lost MCP reply still fails the turn and resumes as uncertain without a second read. The remote MCP server is `stub::HttpStub`. |

### `app/profile-tools.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| profile tool preserves authority, revision conflicts, execution snapshot and durable results | `sb_profile::profile_tool_preserves_authority_revision_conflicts_execution_snapshot_and_durable_results` | 26 -> 36 | Found S3 (`edit_profile` offered in channels). In a channel and after the journal-failure trigger the refusal reaches the model as an MCP error and the turn finishes (Node: failed); the profile stays unchanged in both. The model answers the harness verification reminder without repeating the tool. |

### `app/provider-settings-tools.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| provider settings read stays private and excludes endpoint and credentials | `sb_provider_settings::provider_settings_read_stays_private_and_excludes_endpoint_and_credentials` | 11 -> 11 | Found S2. The result is `{revision, providerInstance, model, configured}`; dropped `providerAccount` and `backupModel` (#87). |
| model changes preserve execution snapshot, scope and journal across restart | `sb_provider_settings::model_changes_preserve_execution_snapshot_scope_and_journal_across_restart` | 29 -> 16 | Dropped: the `set_provider_model` tool, its validation matrix, backup-model fallback and its journal trigger (all removed by #87). Ported: a held turn keeps its model across a profile edit, the acknowledged resume uses the current model, cancellation before the effect, other agents unchanged, restart. |

### `app/scripts.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| local scripts require token and current agent grant, keep payload and retry identity | `sb_scripts::local_scripts_require_token_and_current_agent_grant_keep_payload_and_retry_identity` | 57 -> 47 | Full. |

### `app/test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| HTTP: persistence, idempotent FIFO turns, isolation, failure and interrupted recovery | `sb_http::http_persistence_idempotent_fifo_turns_isolation_failure_and_interrupted_recovery` | 184 -> 132 | Full since the #89 integration: both create replays while the instance is disabled return the edited receipt (B6), before and after a restart. "No provider" is a disabled instance (the seeded test instance keeps its binary); `configured: false` became `runnable: false` with a `blocked_reason`. Found S4 (teammate roster missing from prompts). The static page is a minimal Leptos shell in a test `dist`. |

### `app/rust-workspace-writes-http.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| restored incomplete workspace intent resumes with uncertainty without replaying the write | `sb_workspace_writes::restored_incomplete_workspace_intent_resumes_with_uncertainty_without_replaying_the_write` | 14 -> 11 | Full. The backup is `VACUUM INTO` plus the `minarbot_backup` row, restored with `minarbot-server restore workspace`; the restored database replaces the root database because the kit always starts on it. |
| write grant creates file once through provider and result journal survives restart | `sb_workspace_writes::write_grant_creates_file_once_through_provider_and_result_journal_survives_restart` | 26 -> 22 (shared `write_grant`, `check_effect`) | Changed by #87 on purpose: legacy channels get the agent's managed workspace (dcaab7f), so the write is offered there and the journal refuses the duplicate; read-only bindings offer the write tools and refuse the call (5528da5). Both replace the Node "not offered, turn failed" checks; bytes stay unchanged. |
| write grant creates directory once through provider and result journal survives restart | `sb_workspace_writes::write_grant_creates_directory_once_through_provider_and_result_journal_survives_restart` | 26 -> 22 | As above. |
| write grant creates skill once through provider and result journal survives restart | `sb_workspace_writes::write_grant_creates_skill_once_through_provider_and_result_journal_survives_restart` | 26 -> 22 | As above; the disabled skill's copy is refused. |
| write grant creates delete once through provider and result journal survives restart | `sb_workspace_writes::write_grant_creates_delete_once_through_provider_and_result_journal_survives_restart` | 26 -> 22 | As above. |
| write grant creates restore once through provider and result journal survives restart | `sb_workspace_writes::write_grant_creates_restore_once_through_provider_and_result_journal_survives_restart` | 26 -> 22 | As above, with the 2048-byte recovery input. |
| write grant creates replace once through provider and result journal survives restart | `sb_workspace_writes::write_grant_creates_replace_once_through_provider_and_result_journal_survives_restart` | 26 -> 22 | As above. |
| provider workspace writes reject malformed Unicode without changing bytes or path | `sb_workspace_writes::provider_workspace_writes_reject_malformed_unicode_without_changing_bytes_or_path` | 8 -> 6 | Full; the lone-surrogate arguments are sent verbatim (`Answer::RawTool`). |

### `app/gemini.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| Gemini HTTP streaming, signed tool recovery, models and cancellation | `sb_gemini::gemini_http_streaming_signed_tool_recovery_models_and_cancellation` | 18 -> 18 | Dropped with the Gemini API client (#87): `x-goog-api-key`, `generateContent` payloads, `thoughtSignature` replay, `maxOutputTokens`, `MAX_TOKENS` failure, catalog paging. Ported against the instance: catalog, text and tool turns, the held tool step resumed after restart without a second tool call, cancel, streamed and unary requests. |

### `app/provider.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| native Anthropic messages, catalog, tools, resume, cancellation and fallback use one explicit provider | `sb_provider::native_anthropic_messages_catalog_tools_resume_cancellation_and_fallback_use_one_explicit_provider` | 39 -> 20 | Dropped with the Anthropic client (#87): `/v1/messages` shape, headers, `tool_choice`, catalog pagination/cycle/redirect errors, `max_tokens` partial failure, parallel tool rejection, redirect, `backupModel` fallback, `family: anthropic`. An unknown tool reaches the model as an MCP error (turn done). Ported: replies, artifact tool result pairing, provider failures without fallback, receipts, question across restart, empty workflow output, routing, delegate/transfer coordination, cancel, usage redaction. |

### `app/provider-accounts.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| agents bind isolated provider accounts across channels, side threads, edit and restart | `sb_provider_accounts::agents_bind_isolated_provider_accounts_across_channels_side_threads_edit_and_restart` | 36 -> 29 | Accounts became provider instances created through `/api/provider-instances` (gateway binary, own model, own secret key). Dropped: provider families, `/api/models?account=`, `usage.account`. Removing an account is deleting or disabling the instance; found S7. |

### `app/provider-account-change.test.ts`

All nine cases are dropped: they test the agent tools `set_provider_account`, `list_provider_accounts`, `list_provider_models` and the browser consent flow for account switches, which #87 removed with provider accounts (a8cfcac). The surviving parts are ported elsewhere: carrying history to a new endpoint without foreign history or keys is `sb_handoff`; private provider settings are `sb_provider_settings`; instance model discovery is `sb_provider_matrix`.

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| provider account switch requires scoped browser consent and survives restart without replay | - | 34 -> 0 | Dropped: `set_provider_account` consent flow (#87). |
| provider account metadata tool redacts credentials and paths | - | 11 -> 0 | Dropped: `list_provider_accounts` (#87). |
| private model discovery uses selected account, pages results and cancels without continuation | - | 21 -> 0 | Dropped: `list_provider_models` (#87). |
| confirmed account switch openai-compatible to anthropic retains consent across restart | - | 29 -> 0 | Dropped: account switch consent and families (#87); handoff in `sb_handoff`. |
| confirmed account switch openai-compatible to gemini retains consent across restart | - | 29 -> 0 | Dropped: account switch consent and families (#87); handoff in `sb_handoff`. |
| confirmed account switch anthropic to openai-compatible retains consent across restart | - | 29 -> 0 | Dropped: account switch consent and families (#87); handoff in `sb_handoff`. |
| confirmed account switch anthropic to gemini retains consent across restart | - | 29 -> 0 | Dropped: account switch consent and families (#87); handoff in `sb_handoff`. |
| confirmed account switch gemini to openai-compatible retains consent across restart | - | 29 -> 0 | Dropped: account switch consent and families (#87); handoff in `sb_handoff`. |
| confirmed account switch gemini to anthropic retains consent across restart | - | 29 -> 0 | Dropped: account switch consent and families (#87); handoff in `sb_handoff`. |

### `app/provider-handoff.test.ts`

Provider families and their payload conversions left with #87, so each family pair collapses onto its scope's test: the instance is re-pointed at a different model endpoint across a restart while the question waits.

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| provider handoff openai-compatible to openai-compatible in direct retains completed effects and question recovery | `sb_handoff::provider_handoff_in_direct_retains_completed_effects_and_question_recovery` | 23 -> 19 | Family pair dropped (#87); the handoff, artifact, history, foreign-scope isolation and answer receipt are ported. Gemini signature checks dropped. |
| provider handoff openai-compatible to gemini in direct retains completed effects and question recovery | `sb_handoff::provider_handoff_in_direct_retains_completed_effects_and_question_recovery` | 23 -> 19 | Family pair dropped (#87); the handoff, artifact, history, foreign-scope isolation and answer receipt are ported. Gemini signature checks dropped. |
| provider handoff anthropic to gemini in direct retains completed effects and question recovery | `sb_handoff::provider_handoff_in_direct_retains_completed_effects_and_question_recovery` | 23 -> 19 | Family pair dropped (#87); the handoff, artifact, history, foreign-scope isolation and answer receipt are ported. Gemini signature checks dropped. |
| provider handoff gemini to openai-compatible in direct retains completed effects and question recovery | `sb_handoff::provider_handoff_in_direct_retains_completed_effects_and_question_recovery` | 23 -> 19 | Family pair dropped (#87); the handoff, artifact, history, foreign-scope isolation and answer receipt are ported. Gemini signature checks dropped. |
| provider handoff gemini to anthropic in direct retains completed effects and question recovery | `sb_handoff::provider_handoff_in_direct_retains_completed_effects_and_question_recovery` | 23 -> 19 | Family pair dropped (#87); the handoff, artifact, history, foreign-scope isolation and answer receipt are ported. Gemini signature checks dropped. |
| provider handoff openai-compatible to anthropic in direct retains completed effects and question recovery | `sb_handoff::provider_handoff_in_direct_retains_completed_effects_and_question_recovery` | 23 -> 19 | Family pair dropped (#87); the handoff, artifact, history, foreign-scope isolation and answer receipt are ported. Gemini signature checks dropped. |
| provider handoff anthropic to openai-compatible in direct retains completed effects and question recovery | `sb_handoff::provider_handoff_in_direct_retains_completed_effects_and_question_recovery` | 23 -> 19 | Family pair dropped (#87); the handoff, artifact, history, foreign-scope isolation and answer receipt are ported. Gemini signature checks dropped. |
| provider handoff openai-compatible to openai-compatible in channel retains completed effects and question recovery | `sb_handoff::provider_handoff_in_channel_retains_completed_effects_and_question_recovery` | 23 -> 19 | Family pair dropped (#87); the handoff, artifact, history, foreign-scope isolation and answer receipt are ported. Gemini signature checks dropped. |
| provider handoff openai-compatible to gemini in channel retains completed effects and question recovery | `sb_handoff::provider_handoff_in_channel_retains_completed_effects_and_question_recovery` | 23 -> 19 | Family pair dropped (#87); the handoff, artifact, history, foreign-scope isolation and answer receipt are ported. Gemini signature checks dropped. |
| provider handoff anthropic to gemini in channel retains completed effects and question recovery | `sb_handoff::provider_handoff_in_channel_retains_completed_effects_and_question_recovery` | 23 -> 19 | Family pair dropped (#87); the handoff, artifact, history, foreign-scope isolation and answer receipt are ported. Gemini signature checks dropped. |
| provider handoff gemini to openai-compatible in channel retains completed effects and question recovery | `sb_handoff::provider_handoff_in_channel_retains_completed_effects_and_question_recovery` | 23 -> 19 | Family pair dropped (#87); the handoff, artifact, history, foreign-scope isolation and answer receipt are ported. Gemini signature checks dropped. |
| provider handoff gemini to anthropic in channel retains completed effects and question recovery | `sb_handoff::provider_handoff_in_channel_retains_completed_effects_and_question_recovery` | 23 -> 19 | Family pair dropped (#87); the handoff, artifact, history, foreign-scope isolation and answer receipt are ported. Gemini signature checks dropped. |
| provider handoff openai-compatible to anthropic in channel retains completed effects and question recovery | `sb_handoff::provider_handoff_in_channel_retains_completed_effects_and_question_recovery` | 23 -> 19 | Family pair dropped (#87); the handoff, artifact, history, foreign-scope isolation and answer receipt are ported. Gemini signature checks dropped. |
| provider handoff anthropic to openai-compatible in channel retains completed effects and question recovery | `sb_handoff::provider_handoff_in_channel_retains_completed_effects_and_question_recovery` | 23 -> 19 | Family pair dropped (#87); the handoff, artifact, history, foreign-scope isolation and answer receipt are ported. Gemini signature checks dropped. |
| provider handoff openai-compatible to openai-compatible in side retains completed effects and question recovery | `sb_handoff::provider_handoff_in_side_retains_completed_effects_and_question_recovery` | 23 -> 19 | Family pair dropped (#87); the handoff, artifact, history, foreign-scope isolation and answer receipt are ported. Gemini signature checks dropped. |
| provider handoff openai-compatible to gemini in side retains completed effects and question recovery | `sb_handoff::provider_handoff_in_side_retains_completed_effects_and_question_recovery` | 23 -> 19 | Family pair dropped (#87); the handoff, artifact, history, foreign-scope isolation and answer receipt are ported. Gemini signature checks dropped. |
| provider handoff anthropic to gemini in side retains completed effects and question recovery | `sb_handoff::provider_handoff_in_side_retains_completed_effects_and_question_recovery` | 23 -> 19 | Family pair dropped (#87); the handoff, artifact, history, foreign-scope isolation and answer receipt are ported. Gemini signature checks dropped. |
| provider handoff gemini to openai-compatible in side retains completed effects and question recovery | `sb_handoff::provider_handoff_in_side_retains_completed_effects_and_question_recovery` | 23 -> 19 | Family pair dropped (#87); the handoff, artifact, history, foreign-scope isolation and answer receipt are ported. Gemini signature checks dropped. |
| provider handoff gemini to anthropic in side retains completed effects and question recovery | `sb_handoff::provider_handoff_in_side_retains_completed_effects_and_question_recovery` | 23 -> 19 | Family pair dropped (#87); the handoff, artifact, history, foreign-scope isolation and answer receipt are ported. Gemini signature checks dropped. |
| provider handoff openai-compatible to anthropic in side retains completed effects and question recovery | `sb_handoff::provider_handoff_in_side_retains_completed_effects_and_question_recovery` | 23 -> 19 | Family pair dropped (#87); the handoff, artifact, history, foreign-scope isolation and answer receipt are ported. Gemini signature checks dropped. |
| provider handoff anthropic to openai-compatible in side retains completed effects and question recovery | `sb_handoff::provider_handoff_in_side_retains_completed_effects_and_question_recovery` | 23 -> 19 | Family pair dropped (#87); the handoff, artifact, history, foreign-scope isolation and answer receipt are ported. Gemini signature checks dropped. |

### `app/provider-matrix.test.ts`

One parent case with 34 subtests; each subtest is its own Rust test against the test gateway instance. Every test ends with `scan` (the key absent from responses, server output and files). The exact direct-client error texts per status, backup-model fallback and named accounts left with #87; what remains is a key-free failure, one request and no replay.

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| loading remains pending; empty catalog succeeds without inventing models | `sb_provider_matrix::loading_remains_pending_empty_catalog_succeeds_without_inventing_models` | 6 -> 8 | Instance catalog (`/api/provider-instances/{id}/models`). |
| discovery http-401 | `sb_provider_matrix::discovery_http_401` | 5 -> 8 | Exact error text dropped (#87 direct client). |
| discovery http-403 | `sb_provider_matrix::discovery_http_403` | 5 -> 8 | Exact error text dropped (#87 direct client). |
| discovery catalog-secret | `sb_provider_matrix::discovery_catalog_secret` | 5 -> 8 | Found S8 (the key came back as a model id). |
| discovery non-json | `sb_provider_matrix::discovery_non_json` | 5 -> 8 | Exact error text dropped (#87 direct client). |
| discovery oversize | `sb_provider_matrix::discovery_oversize` | 5 -> 8 | Exact error text dropped (#87 direct client). |
| discovery invalid-catalog | `sb_provider_matrix::discovery_invalid_catalog` | 5 -> 8 | Exact error text dropped (#87 direct client). |
| discovery redirect | `sb_provider_matrix::discovery_redirect` | 5 -> 8 | Exact error text dropped (#87 direct client). |
| discovery http-408 | `sb_provider_matrix::discovery_http_408` | 5 -> 8 | Exact error text dropped (#87 direct client). |
| discovery http-429 | `sb_provider_matrix::discovery_http_429` | 5 -> 8 | Exact error text dropped (#87 direct client). |
| discovery http-503 | `sb_provider_matrix::discovery_http_503` | 5 -> 8 | Exact error text dropped (#87 direct client). |
| discovery catalog-timeout | `sb_provider_matrix::discovery_catalog_timeout` | 5 -> 8 | Exact error text dropped (#87 direct client). |
| missing named account cannot use configured default | `sb_provider_matrix::missing_named_account_cannot_use_configured_default` | 3 -> 5 | A missing instance id (404), no provider request. |
| turn http-401 does not fall back or replay | `sb_provider_matrix::turn_http_401_does_not_fall_back_or_replay` | 7 -> 10 | Exact error text dropped (#87 direct client). |
| turn http-403 does not fall back or replay | `sb_provider_matrix::turn_http_403_does_not_fall_back_or_replay` | 7 -> 10 | Exact error text dropped (#87 direct client). |
| turn http-404 does not fall back or replay | `sb_provider_matrix::turn_http_404_does_not_fall_back_or_replay` | 7 -> 10 | Exact error text dropped (#87 direct client). |
| turn http-408 does not fall back or replay | `sb_provider_matrix::turn_http_408_does_not_fall_back_or_replay` | 7 -> 10 | Exact error text dropped (#87 direct client). |
| turn timeout does not fall back or replay | `sb_provider_matrix::turn_timeout_does_not_fall_back_or_replay` | 7 -> 10 | Exact error text dropped (#87 direct client). |
| turn http-500 does not fall back or replay | `sb_provider_matrix::turn_http_500_does_not_fall_back_or_replay` | 7 -> 10 | Exact error text dropped (#87 direct client). |
| turn http-502 does not fall back or replay | `sb_provider_matrix::turn_http_502_does_not_fall_back_or_replay` | 7 -> 10 | Exact error text dropped (#87 direct client). |
| turn text-secret does not fall back or replay | `sb_provider_matrix::turn_text_secret_does_not_fall_back_or_replay` | 7 -> 10 | An echoed key is redacted. |
| turn malformed does not fall back or replay | `sb_provider_matrix::turn_malformed_does_not_fall_back_or_replay` | 7 -> 10 | Exact error text dropped (#87 direct client). |
| turn tools does not fall back or replay | `sb_provider_matrix::turn_tools_does_not_fall_back_or_replay` | 7 -> 10 | Exact error text dropped (#87 direct client). |
| turn other-400 does not fall back or replay | `sb_provider_matrix::turn_other_400_does_not_fall_back_or_replay` | 7 -> 10 | Exact error text dropped (#87 direct client). |
| turn 429 falls back once on same account | `sb_provider_matrix::turn_429_falls_back_once_on_same_account` | 6 -> 10 | Fallback removed (#87): one request, turn failed. |
| turn 503 falls back once on same account | `sb_provider_matrix::turn_503_falls_back_once_on_same_account` | 6 -> 10 | As above. |
| fallback failure ends after two requests | `sb_provider_matrix::fallback_failure_ends_after_two_requests` | 4 -> 10 | As above: one request. |
| rate limit without fallback stays failed | `sb_provider_matrix::rate_limit_without_fallback_stays_failed` | 4 -> 7 | Exact error text dropped. |
| completed tool effect blocks automatic fallback; acknowledged resume retains artifact | `sb_provider_matrix::completed_tool_effect_blocks_automatic_fallback_acknowledged_resume_retains_artifact` | 9 -> 10 | Full apart from the error text. |
| turn stream-cut retains partial text without fallback or replay on restart | `sb_provider_matrix::turn_stream_cut_retains_partial_text_without_fallback_or_replay_on_restart` | 9 -> 12 | Exact error text dropped. |
| turn stream-socket-cut retains partial text without fallback or replay on restart | `sb_provider_matrix::turn_stream_socket_cut_retains_partial_text_without_fallback_or_replay_on_restart` | 9 -> 12 | Exact error text dropped. |
| streamed credential echo never reaches partial storage | `sb_provider_matrix::streamed_credential_echo_never_reaches_partial_storage` | 7 -> 6 | Exact error text dropped. |
| unconfigured default is distinct from missing named account | `sb_provider_matrix::unconfigured_default_is_distinct_from_missing_named_account` | 4 -> 7 | A disabled instance: turns 503, no provider request. |
| unreachable loopback host reports connection problem | `sb_provider_matrix::unreachable_loopback_host_reports_connection_problem` | 4 -> 7 | Exact error text dropped. |

### `app/memory-reflect.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| scoped auto-recall, history, reflection and standing answer refresh | `sb_memory::scoped_auto_recall_history_reflection_and_standing_answer_refresh` | 22 -> 25 | Found S5 (background memory calls always got an empty answer). The Node case minted an MCP session and called `recall`/`remember` directly; here the turn's own MCP session runs them. |
| background learning uses the binding catalog unless Memory model is set | `sb_memory::background_learning_uses_the_binding_catalog_unless_memory_model_is_set` | 10 -> 12 | Dropped: the cheapest-model default from the API account catalog (`/api/models?runtime=api`, `api_memory_default`), removed by #87; learning now uses the Memory model, else the agent's model. Per-phase calls, models and usage rows carry over. |

### `app/slack-output.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| Slack artifacts retain exact bytes and upload stages across uncertain completion and grant recovery | `sb_slack_output::slack_artifacts_retain_exact_bytes_and_upload_stages_across_uncertain_completion_and_grant_recovery` | 24 -> 21 | Full; the fake Slack Web API and Socket Mode server is `connector_fixture::SlackFixture`. |

### `app/rust-harness.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| mise shim probes and launches with trusted mise paths and an isolated home | `sb_rust_harness::mise_shim_probes_and_launches_with_trusted_mise_paths_and_an_isolated_home` | 5 -> 3 | Full (the stand-in `mise` script checks the environment itself). Found S6. |
| Rust ACP and Pi execute fake protocols with model selection and approvals | `sb_rust_harness::rust_acp_and_pi_execute_fake_protocols_with_model_selection_and_approvals` | 6 -> 6 | Full. |
| Rust MCP feature receipts, scopes, cursors and stdio forwarding | `sb_rust_harness::rust_mcp_feature_receipts_scopes_cursors_and_stdio_forwarding` | 17 -> 15 | Full; peer tokens come from `/api/mcp-server/peer-tokens`. |

### `app/harness-default.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| new agents default to Codex with a managed workspace; API agents keep runtime and switch in one saved command | `sb_harness_default::new_agents_default_to_codex_with_a_managed_workspace_api_agents_keep_runtime_and_switch_in_one_saved_command` | 11 -> 9 | Dropped (#87): `runtime` on create and the implicit default binding; a new agent is unbound until an instance is chosen. Ported: runtime choices, the confirmed one-command switch with its receipt, the managed workspace, restart, `defaultRuntime`. |
| agents on this machine's Codex sign-in use the CLI folder without writing to it | `sb_harness_default::agents_on_this_machines_codex_sign_in_use_the_cli_folder_without_writing_to_it` | 11 -> 6 | The `device` account became an instance with `homePath` (sets `CODEX_HOME`). Dropped (#87, a8cfcac): the isolated `HOME` for device turns and the API-runtime 400. |
| agents on this machine's Claude Code sign-in run with the real HOME and add nothing to ~/.claude | `sb_harness_default::agents_on_this_machines_claude_code_sign_in_run_with_the_real_home_and_add_nothing_to_claude` | 6 -> 4 | Full; the agent is bound through the runtime edit. |
| provider isolation, account home rotation and MCP shell approval boundaries | `sb_harness_default::provider_isolation_account_home_rotation_and_mcp_shell_approval_boundaries` | 24 -> 14 | Full apart from #87 drops, since the #89 integration: workspace checks (`echo`, the custom `python3` check) run without approval and record verification, the turn has no approval card, and the sensitive `rm` waits for a card, is denied and leaves the file. Accounts became instances with secret keys. Dropped (#87): per-binding home rotation and `config.toml` (a8cfcac), the "not offered" tool list (5528da5 offers the full set). |
| default harness setup saves its profile through MCP and cannot use unrelated tools (codex) | `sb_harness_default::default_harness_setup_saves_its_profile_through_mcp_and_cannot_use_unrelated_tools_codex` | 6 -> 6 | Full; bound to an explicit instance. |
| default harness setup saves its profile through MCP and cannot use unrelated tools (claude) | `sb_harness_default::default_harness_setup_saves_its_profile_through_mcp_and_cannot_use_unrelated_tools_claude` | 6 -> 6 | Full. |
| new agents fall back to the API runtime when Codex is not installed | `sb_harness_default::new_agents_fall_back_to_the_api_runtime_when_codex_is_not_installed` | 2 -> 3 | `runtime: api` left with #87: the agent stays unbound and not runnable. |

### `app/project-integrations.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| Rust project environment validates names and UTF-8 byte bounds | `sb_project_integrations::rust_project_environment_validates_names_and_utf_8_byte_bounds` | 3 -> 4 | Full. |
| project environment settings and removal preserve consent, replay and stale-write fences | `sb_project_integrations::project_environment_settings_and_removal_preserve_consent_replay_and_stale_write_fences` | 11 -> 11 | Full. |
| codex: project environment encryption, harness shell, routine and direct-chat isolation | `sb_project_integrations::codex_project_environment_encryption_harness_shell_routine_and_direct_chat_isolation` | 24 -> 22 (shared `environment_isolation`) | The fake's `ENV_PRESENT`/`ENV_ABSENT` keywords replace the Node fixture patch. A harness reply joins its agent messages, so the reply ends with `finished` and carries the redacted shell output (the Node `reply === 'finished'` was never reached against Rust). |
| claude: project environment encryption, harness shell, routine and direct-chat isolation | `sb_project_integrations::claude_project_environment_encryption_harness_shell_routine_and_direct_chat_isolation` | 24 -> 22 | As above. |
| project apps and MCP: separate OAuth identities, offered tools, approvals, restart and cleanup | `sb_project_integrations::project_apps_and_mcp_separate_oauth_identities_offered_tools_approvals_restart_and_cleanup` | 39 -> 48 | Composio and the MCP server are `HttpStub`s; the catalog check uses the instance catalog (`/api/models` left with #87). Projects get a folder because harness turns need a workspace. A foreign project's MCP call is refused as an MCP error (Node: failed turn). |

### `app/script-provider.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| script provider settings keep identity and credentials isolated, reject busy/revoked changes and preserve receipts | `sb_script_provider::script_provider_settings_keep_identity_and_credentials_isolated_reject_busy_revoked_changes_and_preserve_receipts` | 40 -> 33 | Full apart from #87 drops, since the #89 integration: the provider read after the grant (model, the bound instance listed as enabled, no connection details; B2), `script models` (instance and `["new","old"]`, missing instance 404, foreign parameter 400) and a catalog read held across the revocation (403, and the held catalog request carried the instance key; B3). Accounts became instances: `?account=missing` (503) is `?instance=missing` (404). Dropped (#87): the `providerAccount: 'missing'` refusal and `provider_account` in the result. |

### `app/task-recovery.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | ---: | --- |
| reassign creates one immutable successor, keeps original receipt and fences source controls | `sb_task_recovery::reassign_creates_one_immutable_successor_keeps_original_receipt_and_fences_source_controls` | 18 -> 18 |  |
| canonical channel context names current visible members and lead across edits | `sb_task_recovery::canonical_channel_context_names_current_visible_members_and_lead_across_edits` | 5 -> 5 |  |
| canonical routine outcomes validate reasons and atomically retain result receipt and task state | `sb_task_recovery::canonical_routine_outcomes_validate_reasons_and_atomically_retain_result_receipt_and_task_state` | 18 -> 14 | Same checks; some Node checks are combined in one Rust check. |
| canonical routine outcomes deny forced ordinary chat and descendant calls | `sb_task_recovery::canonical_routine_outcomes_deny_forced_ordinary_chat_and_descendant_calls` | 3 -> 3 |  |
| canonical routine outcomes reject late results after pause cancel revoke or context change | `sb_task_recovery::canonical_routine_outcomes_reject_late_results_after_pause_cancel_revoke_or_context_change` | 4 -> 4 |  |
| canonical turn wait reserves one deadline and activates only after matching home secret result | `sb_task_recovery::canonical_turn_wait_reserves_one_deadline_and_activates_only_after_matching_home_secret_result` | 17 -> 17 |  |
| pending turn wait closes after cancellation, stale target or failed keyring result and rolls back failed activation | `sb_task_recovery::pending_turn_wait_closes_after_cancellation_stale_target_or_failed_keyring_result_and_rolls_back_failed_activation` | 8 -> 8 |  |
| canonical maintenance expires pending and activated waits once without reviving closed keys | `sb_task_recovery::canonical_maintenance_expires_pending_and_activated_waits_once_without_reviving_closed_keys` | 10 -> 9 | Same checks; some Node checks are combined in one Rust check. |
| canonical wait maintenance survives SQLite failure and retries after storage recovers | `sb_task_recovery::canonical_wait_maintenance_survives_sqlite_failure_and_retries_after_storage_recovers` | 4 -> 4 |  |
| disaster restore closes pending provisions and activated wait references while keeping receipts | `sb_task_recovery::disaster_restore_closes_pending_provisions_and_activated_wait_references_while_keeping_receipts` | 6 -> 7 |  |
| canonical shared data keeps schema ownership, collaborator scope, validation and atomic receipts | `sb_task_recovery::canonical_shared_data_keeps_schema_ownership_collaborator_scope_validation_and_atomic_receipts` | 15 -> 14 | Same checks; some Node checks are combined in one Rust check. |
| canonical shared data rechecks cancellation and authority across worker I/O and preserves concurrent conflicts | `sb_task_recovery::canonical_shared_data_rechecks_cancellation_and_authority_across_worker_io_and_preserves_concurrent_conflicts` | 9 -> 9 |  |
| canonical cancellation kills SQL workers and releases query capacity | `sb_task_recovery::canonical_cancellation_kills_sql_workers_and_releases_query_capacity` | 3 -> 3 |  |
| canonical questions validate limits and atomically pause, answer and retain cancellation | `sb_task_recovery::canonical_questions_validate_limits_and_atomically_pause_answer_and_retain_cancellation` | 20 -> 19 | Same checks; some Node checks are combined in one Rust check. |
| canonical question answers enforce private human, conversation, project and current collaborator access | `sb_task_recovery::canonical_question_answers_enforce_private_human_conversation_project_and_current_collaborator_access` | 10 -> 10 |  |
| canonical avatar removal in direct preserves CAS, rollback, scope and exact results | `sb_task_recovery::canonical_avatar_removal_in_direct_preserves_cas_rollback_scope_and_exact_results` | 19 -> 19 |  |
| canonical avatar removal in side preserves CAS, rollback, scope and exact results | `sb_task_recovery::canonical_avatar_removal_in_side_preserves_cas_rollback_scope_and_exact_results` | 19 -> 19 |  |
| canonical sites in direct page scoped metadata and retain atomic deletion | `sb_task_recovery::canonical_sites_in_direct_page_scoped_metadata_and_retain_atomic_deletion` | 21 -> 21 |  |
| canonical sites in side page scoped metadata and retain atomic deletion | `sb_task_recovery::canonical_sites_in_side_page_scoped_metadata_and_retain_atomic_deletion` | 21 -> 21 |  |
| reassign denies unresolved effects, revoked initiators and invalid successor provenance atomically | `sb_task_recovery::reassign_denies_unresolved_effects_revoked_initiators_and_invalid_successor_provenance_atomically` | 6 -> 6 |  |
| committed result survives lost ACK, revocation and offline history without granting dispatch | `sb_task_recovery::committed_result_survives_lost_ack_revocation_and_offline_history_without_granting_dispatch` | 7 -> 7 |  |
| not-sent and uncertain reports require verifiable evidence and cannot resurrect cancelled work | `sb_task_recovery::not_sent_and_uncertain_reports_require_verifiable_evidence_and_cannot_resurrect_cancelled_work` | 5 -> 5 |  |
| signed no-marker evidence reconciles original owner before acquire and altered proofs fail closed | `sb_task_recovery::signed_no_marker_evidence_reconciles_original_owner_before_acquire_and_altered_proofs_fail_closed` | 8 -> 8 |  |
| progress reports advance monotonically, immutable result publishes once and prepare sees changed context | `sb_task_recovery::progress_reports_advance_monotonically_immutable_result_publishes_once_and_prepare_sees_changed_context` | 7 -> 7 |  |
| task controls use current scope and revisions, keep cancellation terminal and resolve no effects implicitly | `sb_task_recovery::task_controls_use_current_scope_and_revisions_keep_cancellation_terminal_and_resolve_no_effects_implicitly` | 6 -> 6 |  |
| owner risk resolution reserves one replacement and cannot erase unknown original effect | `sb_task_recovery::owner_risk_resolution_reserves_one_replacement_and_cannot_erase_unknown_original_effect` | 6 -> 6 |  |
| late known result rejects risk retry and historical accepted receipt stays valid after a later ACK | `sb_task_recovery::late_known_result_rejects_risk_retry_and_historical_accepted_receipt_stays_valid_after_a_later_ack` | 3 -> 3 |  |
| offline task admission is durable, scoped, immutable and exact without provider binding | `sb_task_recovery::offline_task_admission_is_durable_scoped_immutable_and_exact_without_provider_binding` | 15 -> 15 |  |
| assigned collaborator executes owner home/binding and revoked scope blocks receipt replay | `sb_task_recovery::assigned_collaborator_executes_owner_home_binding_and_revoked_scope_blocks_receipt_replay` | 3 -> 3 |  |
| task messages require files, preserve line breaks, and closed union rejects mixed routing | `sb_task_recovery::task_messages_require_files_preserve_line_breaks_and_closed_union_rejects_mixed_routing` | 5 -> 5 |  |
| task admission requires scoped conversation, matching revisions and exact attachment conversation | `sb_task_recovery::task_admission_requires_scoped_conversation_matching_revisions_and_exact_attachment_conversation` | 7 -> 7 |  |
| provider provenance is captured from owner same-home binding and never caller fields | `sb_task_recovery::provider_provenance_is_captured_from_owner_same_home_binding_and_never_caller_fields` | 5 -> 5 |  |
| acquire requires stopped/reconciled owner, exact token and fenced generation | `sb_task_recovery::acquire_requires_stopped_reconciled_owner_exact_token_and_fenced_generation` | 7 -> 7 |  |
| context snapshot is immutable, exact, project scoped and requires current executor token | `sb_task_recovery::context_snapshot_is_immutable_exact_project_scoped_and_requires_current_executor_token` | 6 -> 6 |  |
| action prepare grants no dispatch; commit has one live grant and stale context blocks new work | `sb_task_recovery::action_prepare_grants_no_dispatch_commit_has_one_live_grant_and_stale_context_blocks_new_work` | 9 -> 9 |  |
| canonical transfer commits one successor and immutable result, fences old executor and retries without mutation | `sb_task_recovery::canonical_transfer_commits_one_successor_and_immutable_result_fences_old_executor_and_retries_without_mutation` | 15 -> 14 | Same checks; some Node checks are combined in one Rust check. |
| canonical RPC rejects changed input, stale grants and fabricated results without partial transfer | `sb_task_recovery::canonical_rpc_rejects_changed_input_stale_grants_and_fabricated_results_without_partial_transfer` | 8 -> 8 |  |
| canonical transfer keeps channel-only tool boundary for private tasks | `sb_task_recovery::canonical_transfer_keeps_channel_only_tool_boundary_for_private_tasks` | 2 -> 2 |  |
| context projects authorized memory, pinned skills and byte-free asset refs, invalidating changed sources | `sb_task_recovery::context_projects_authorized_memory_pinned_skills_and_byte_free_asset_refs_invalidating_changed_sources` | 11 -> 11 | Memory and skill rows are seeded with SQL equal to `rememberMemory` and `seedSkillVersion`. |
| context preserves scoped history, questions, compaction and child evidence without foreign records or file bytes | `sb_task_recovery::context_preserves_scoped_history_questions_compaction_and_child_evidence_without_foreign_records_or_file_bytes` | 18 -> 17 | Same checks; some Node checks are combined in one Rust check. |
| canonical history retains sequence cursors and excludes unmapped, foreign, private and future originals | `sb_task_recovery::canonical_history_retains_sequence_cursors_and_excludes_unmapped_foreign_private_and_future_originals` | 13 -> 13 |  |
| canonical skills retain installed pins, owner mappings, private boundaries and immutable read results | `sb_task_recovery::canonical_skills_retain_installed_pins_owner_mappings_private_boundaries_and_immutable_read_results` | 25 -> 25 |  |
| canonical routine tools keep private project mappings, pagination and flow scope | `sb_task_recovery::canonical_routine_tools_keep_private_project_mappings_pagination_and_flow_scope` | 18 -> 18 |  |
| canonical routine edits require project agents and assignment, preserve CAS and roll back with result failure | `sb_task_recovery::canonical_routine_edits_require_project_agents_and_assignment_preserve_cas_and_roll_back_with_result_failure` | 16 -> 16 |  |
| canonical provider settings read binding metadata only in direct and stays private | `sb_task_recovery::canonical_provider_settings_read_binding_metadata_only_in_direct_and_stays_private` | 8 -> 5 | Dropped (#87): the provider-binding cases (another home, readiness, foreign owner) and the `providerAccount`/`backupModel` fields. The canonical executor reads the agent's provider instance: `{revision, providerInstance, model, configured}`, checked unbound and bound. |
| canonical provider settings read binding metadata only in side and stays private | `sb_task_recovery::canonical_provider_settings_read_binding_metadata_only_in_side_and_stays_private` | 8 -> 5 | Dropped (#87): the provider-binding cases (another home, readiness, foreign owner) and the `providerAccount`/`backupModel` fields. The canonical executor reads the agent's provider instance: `{revision, providerInstance, model, configured}`, checked unbound and bound. |
| canonical setup restricts host actions and forced tools, and requires a complete profile edit | `sb_task_recovery::canonical_setup_restricts_host_actions_and_forced_tools_and_requires_a_complete_profile_edit` | 5 -> 5 |  |
| canonical profile edit in direct keeps atomic CAS errors and channel privacy | `sb_task_recovery::canonical_profile_edit_in_direct_keeps_atomic_cas_errors_and_channel_privacy` | 12 -> 12 |  |
| canonical profile edit in side keeps atomic CAS errors and channel privacy | `sb_task_recovery::canonical_profile_edit_in_side_keeps_atomic_cas_errors_and_channel_privacy` | 12 -> 12 |  |
| canonical agent directory pages authorized teammates and scopes activity to project, human and channel | `sb_task_recovery::canonical_agent_directory_pages_authorized_teammates_and_scopes_activity_to_project_human_and_channel` | 22 -> 22 |  |
| canonical interruption isolates sender, human and project while preserving terminal outcomes and atomic notice rollback | `sb_task_recovery::canonical_interruption_isolates_sender_human_and_project_while_preserving_terminal_outcomes_and_atomic_notice_rollback` | 27 -> 27 |  |
| canonical messages retain full assignment, isolated children, group waits and atomic cancellation | `sb_task_recovery::canonical_messages_retain_full_assignment_isolated_children_group_waits_and_atomic_cancellation` | 38 -> 38 | A paused child still gets its context; the refusal comes at prepare (`task_not_runnable`), which is where the Node `stage` call failed. |
| context counts projected result bytes once, including escaped JSON content | `sb_task_recovery::context_counts_projected_result_bytes_once_including_escaped_json_content` | 2 -> 2 |  |
| disaster mixed batches reconcile known evidence and privately quarantine only unknown actions atomically | `sb_task_recovery::disaster_mixed_batches_reconcile_known_evidence_and_privately_quarantine_only_unknown_actions_atomically` | 12 -> 12 | The restore session comes from `POST /api/restore/reconcile` with a bootstrap set in the database (the Node `prepareRestoreBootstrap`), not the TS `issueRestoreSession` helper. |
| owner recovery session is scoped, nonsecret on retry, rotates and requires revoked original source | `sb_task_recovery::owner_recovery_session_is_scoped_nonsecret_on_retry_rotates_and_requires_revoked_original_source` | 9 -> 9 |  |
| revoked-source recovery retains signed evidence then publishes exact content without granting execution | `sb_task_recovery::revoked_source_recovery_retains_signed_evidence_then_publishes_exact_content_without_granting_execution` | 11 -> 11 |  |
| recovery rejects replacement keys, expired proof, changed content and non-advancing watermarks atomically | `sb_task_recovery::recovery_rejects_replacement_keys_expired_proof_changed_content_and_non_advancing_watermarks_atomically` | 11 -> 11 |  |
| recovery stop proof preserves original journal signature and rejects altered runner evidence | `sb_task_recovery::recovery_stop_proof_preserves_original_journal_signature_and_rejects_altered_runner_evidence` | 4 -> 4 |  |
| late content completes a saved recovery batch without lowering newer evidence watermark | `sb_task_recovery::late_content_completes_a_saved_recovery_batch_without_lowering_newer_evidence_watermark` | 6 -> 6 |  |

59 cases, 59 Rust tests, assertions 648 -> 634. `app/rust-task-test-bridge.ts` is removed with it (the kit's `task_bridge` replaces it).

<!-- stale-b files -->

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
- **B1x (null coordination results).** `delegate_task` and `transfer_task`
  returned null like `ask_user` (B1), and the harness took one more model
  step before the server parked or stopped the turn. The #89 integration
  fixed it: the tests no longer special-case that step and count strictly.
- **D1 (harness context).** The harness keeps its own conversation: the
  server cannot prune older screenshots in it, and a resumed run starts a new
  harness session that sees the transcript text, not earlier tool results.
- **M1 (harness parent waits).** Written against `af83887`, where a harness
  turn's `send_message` with `expects_reply` held the MCP call, and the
  parent's run and slot, until the group settled. The product slice
  (1018261c) queues the parent and returns the waiting receipt at once, and
  the #89 integration stops the parked run, as the API runtime did. A later
  user turn on that thread runs at once and receives ready replies
  (`delivered_earlier`) again; `sa_agent_interrupt` keeps its side-thread
  setup, which still holds.
- **W1 (channel workspaces).** 7098b86 (#87) gives a channel without a project
  its lead agent's workspace, so workspace and Git tools are offered there.

Kit additions: `minarbot_testkit::connector` (new file: port of `rust-connector-fixture.ts`, `rust-connectors-runner.ts` and `connector-journal-command.ts`: fake Telegram/Slack API with a Socket Mode WebSocket and a `Remote` hook, a lossy script-API proxy with `Fault`s, the connector runner and journal command; enables axum's `ws` feature, already in the lock through the server), `minarbot_testkit::restore` (new file: runs `minarbot-server restore ...` and checks `exitCode` against the process status, Clerk identity env and stdin token from the test-only key, Ed25519 `FenceKey`; adds the `ring` dependency the server already locks), `minarbot_testkit::hmac` (new file), `fixtures/local-pre-collapse.sql` (copied from `app/test-support/fixtures/`), `minarbot_testkit::gate::ModelGate` (new file `gate.rs`) holds
one scripted reply so a test can act between "the model chose a tool" and
"the server ran it", which the Node providers did by awaiting before they
answered. `ServerConfig::to_json` now also rejects unknown `hubLink` fields
(`not ported: option hubLink.<name>`), as the Node bridge did.

Kit fixes found while porting: `fake-gateway` forwards `turn/start`
`input_image` items and treats `MINARBOT_TOOL_BUDGET=0` as an exhausted
budget; a finished `fake-gateway` run no longer removes the run that
replaced it on the same thread (a later `turn/interrupt` could not abort
it, and the peer never exited: the flaky `g8_g9_g11...` self-test);
`ModelGate` holds by predicate (`hold_when`) and releases replies gated
before a release but answered after it; the scripted model and the gate
accept bodies up to 64 MiB.

Product bugs fixed in this slice:

| Bug | Found by | Cause and fix |
| --- | --- | --- |
| P2 | image-attachment | Codex prompts repeated each attached image as a base64 data URL in the text prompt as well as an `input_image` item, and harness stdin accepted at most 2 MiB per message, so valid images above about 1.5 MiB failed the turn ("Coding harness context exceeds 2 MiB", then "input exceeded its buffer limit"). The prompt now carries a note instead of the data URL, the stdin limit covers the 4 MiB image budget plus the prompt (8 MiB), and `read_attachment` of an image already attached to the turn returns the "already included" note again. |
| P3 | tool-budget | Harness continuations (question answer, transfer to another owner, restart) started with the full `MINARBOT_TOOL_BUDGET` again; `ask_user` and `transfer_task` were never counted because the run stops before reporting them; a transfer left the old owner's run spending the budget on 401 calls; and a run that failed after the budget ran out lost "Stopped after N tool steps. Resume to continue.". The server now passes the remaining budget, counts both tools, stops the old owner's run on transfer (keeping the turn queued for the new owner) and maps the failure. Resume and reassign keep their fresh budget (they reset `tool_budget_used`). |
| P4 | agent-messages | MCP `tools/list` for a harness session listed private tools (skills, routines, profile) in channel deliveries and `send_message`/`list_agents`/`interrupt_agent` in no-reply notices, unlike `turns::tools::offered`. The session listing now applies the same filters. |
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
| `app/telegram-output.test.ts` | 1 | 1 | 0 | 0 |
| `app/transcription.test.ts` | 6 | 6 | 0 | 0 |
| `app/mcp.test.ts` | 1 | 1 | 0 | 0 |
| `app/slack.test.ts` | 8 | 8 | 0 | 0 |
| `app/image-attachment.test.ts` | 3 | 1 | 2 | 0 |
| `app/agent-result-files.test.ts` | 2 | 2 | 0 | 0 |
| `app/tool-budget.test.ts` | 13 | 11 | 2 | 1 (TS module part) |
| `app/fallback.test.ts` | 1 | 1 | 0 | 0 |
| `app/coordination.test.ts` | 1 | 1 | 0 | 0 |
| `app/agent-messages.test.ts` | 1 | 1 | 0 | 0 |
| `app/agent-interrupt.test.ts` | 1 | 1 | 0 | 0 |

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
`ask_user` followed stage 1's B1 handling until the #89 integration removed
`support.rs`; their counts are strict now.

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| Telegram question buttons answer the owning task once across restart and lost responses | `sa_telegram::telegram_question_buttons_answer_the_owning_task_once_across_restart_and_lost_responses` | 19 -> 18 | Full (the Node case failed on B1's null step). |
| Telegram offline recovery preserves sent parts and rejects repeated operator decisions | `sa_telegram::telegram_offline_recovery_preserves_sent_parts_and_rejects_repeated_operator_decisions` | 26 -> 26 | Full. |
| Telegram bridge submits once through granted side-thread API and isolates main history | `sa_telegram::telegram_bridge_submits_once_through_granted_side_thread_api_and_isolates_main_history` | 12 -> 11 | Full. |
| Rust Telegram runner rejects a mismatched bot identity and releases its journal lock | `sa_telegram::rust_telegram_runner_rejects_a_mismatched_bot_identity_and_releases_its_journal_lock` | 3 -> 2 | Full. |
| Telegram runner replies directly and releases owned journal on shutdown | `sa_telegram::telegram_runner_replies_directly_and_releases_owned_journal_on_shutdown` | 9 -> 6 (shared helper) | Full; the fake's own checks are collected and asserted. |
| Telegram runner answers a question and releases owned journal on shutdown | `sa_telegram::telegram_runner_answers_a_question_and_releases_owned_journal_on_shutdown` | 9 -> 6 (shared helper) | Same helper; strict model calls (2, as in Node) since the #89 integration. |

### `app/telegram-output.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| Telegram sends exact generated artifacts, retains uncertain delivery and rechecks grant before recovery | `sa_telegram_output::telegram_sends_exact_generated_artifacts_retains_uncertain_delivery_and_rechecks_grant_before_recovery` | 22 -> 20 | Full. The three model steps are the file, the answer and the done-check reminder (H2), counted strictly since the #89 integration. |

### `app/transcription.test.ts`

Transcription is a direct HTTP integration (`transcriptionProvider`), not a
harness, so these cases port one to one; the provider is `sa_support::RawProvider`.

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| HTTP transcription requires consent, isolates files and never silently replays uncertain requests | `sa_transcription::http_transcription_requires_consent_isolates_files_and_never_silently_replays_uncertain_requests` | 35 -> 29 | Full; the provider's own checks are collected and asserted once. |
| in-flight transcription deduplicates and cannot publish after archive or shutdown | `sa_transcription::in_flight_transcription_deduplicates_and_cannot_publish_after_archive_or_shutdown` | 11 -> 8 | Full. |
| transcription is opt-in and validates request bodies | `sa_transcription::transcription_is_opt_in_and_validates_request_bodies` | 5 -> 5 | Full. |
| transcription rejects redirects and oversized output, and script grants cannot replace consent | `sa_transcription::transcription_rejects_redirects_and_oversized_output_and_script_grants_cannot_replace_consent` | 18 -> 17 | Full. |
| Rust upgrades legacy transcription operations and reports unknown and duration usage | `sa_transcription::rust_upgrades_legacy_transcription_operations_and_reports_unknown_and_duration_usage` | 6 -> 6 | Full. |
| cancelling streamed transcription persists without publication or replay | `sa_transcription::cancelling_streamed_transcription_persists_without_publication_or_replay` | 17 -> 16 | Full. |

### `app/mcp.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| MCP tests before save, protects credentials/grants and journals success or uncertainty without replay | `sa_mcp::mcp_tests_before_save_protects_credentials_grants_and_journals_success_or_uncertainty_without_replay` | 43 -> 47 | H1: the ungranted, other-agent, channel, drifted-schema, revoked and missing-host-key calls reach the model as MCP errors and the turn finishes (the Node case expected failed turns); each still asserts the server was not called. Lost and invalid-output calls still fail the turn as uncertain. The backup is restored with `restore workspace` through `minarbot_testkit::restore`. |

### `app/slack.test.ts`

`minarbot_testkit::connector` gained `SocketScript` (rate-limit-once methods,
envelopes on connect, disconnect after the first ack, an ack hook), mutable
channels and a record of `Authorization` headers for these cases.

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| Slack actual agent API retries one task and isolates requester question answers (buttons=false) | `sa_slack::slack_actual_agent_api_retries_one_task_and_isolates_requester_question_answers_buttons_false` | 20 -> 19 (shared helper) | Full, with strict model calls since the #89 integration. |
| Slack actual agent API retries one task and isolates requester question answers (buttons=true) | `sa_slack::slack_actual_agent_api_retries_one_task_and_isolates_requester_question_answers_buttons_true` | 20 -> 19 (shared helper) | Same. |
| Slack Socket Mode runner acknowledges journaled event and delivers once after reconnect | `sa_slack::slack_socket_mode_runner_acknowledges_journaled_event_and_delivers_once_after_reconnect` | 16 -> 8 | Full; the fake's checks (429 Retry-After, ack only after journaling, delivery fields) are collected and asserted once. The connector reaches the server through the kit proxy instead of the server's own `connection.json`. |
| Slack status and stop scope to requester root and preserve cancellation identity after lost responses | `sa_slack::slack_status_and_stop_scope_to_requester_root_and_preserve_cancellation_identity_after_lost_responses` | 17 -> 16 | Full, with strict model calls since the #89 integration. |
| Slack offline recovery pins revisions, retains confirmed chunks and requires explicit CLI confirmation | `sa_slack::slack_offline_recovery_pins_revisions_retains_confirmed_chunks_and_requires_explicit_cli_confirmation` | 27 -> 27 | Full. |
| Slack startup cancellation releases lock and status retains authentication cooldown | `sa_slack::slack_startup_cancellation_releases_lock_and_status_retains_authentication_cooldown` | 8 -> 7 | Full. |
| Slack interactive Socket Mode withholds early ACK and commits one decision before redelivery ACK | `sa_slack::slack_interactive_socket_mode_withholds_early_ack_and_commits_one_decision_before_redelivery_ack` | 6 -> 5 | Full. |
| Slack shared channels require opt-in mention and retain one scoped multi-user thread | `sa_slack::slack_shared_channels_require_opt_in_mention_and_retain_one_scoped_multi_user_thread` | 18 -> 17 | Full. |

### `app/image-attachment.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| openai-compatible image attachments retain scope, bytes, retry identity and bounded rereading | `sa_image_attachment::openai_compatible_image_attachments_retain_scope_bytes_retry_identity_and_bounded_rereading` | 30 -> 32 | Full; found P2. The kit's `fake-gateway` now forwards `input_image` items, and the scripted model and gate accept bodies up to 64 MiB. |
| anthropic image attachments retain scope, bytes, retry identity and bounded rereading | Dropped | 30 -> 0 | #87 removed the Anthropic wire format (direct provider clients); every family-neutral assertion is in the openai-compatible test. |
| gemini image attachments retain scope, bytes, retry identity and bounded rereading | Dropped | 30 -> 0 | #87 removed the Gemini API; as above. |

### `app/agent-result-files.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| requester reads returned teammate text artifacts but not private inputs or no-reply outputs | `sa_agent_result_files::requester_reads_returned_teammate_text_artifacts_but_not_private_inputs_or_no_reply_outputs` | 24 -> 20 (shared helper) | Full. `ModelGate::hold_when` replaces the provider's held continuation; H2. |
| requester reads returned teammate PDF artifacts but not private inputs or no-reply outputs | `sa_agent_result_files::requester_reads_returned_teammate_pdf_artifacts_but_not_private_inputs_or_no_reply_outputs` | 24 -> 20 (shared helper) | Full; the PDF fixture is `sa_support::pdf_fixture` and the extraction hold is the kit PDF bridge (multi-thread runtime, because the bridge blocks). |

### `app/tool-budget.test.ts`

The fake desktop runtime is the kit computer bridge (`desktop-*` operations)
with `desktop.testRuntime`; its lease directory is kept short so the proxy
socket path fits `sun_path`.

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| nine distinct tools complete before per-task budget requests a final answer without tools | `sa_tool_budget::nine_distinct_tools_complete_before_per_task_budget_requests_a_final_answer_without_tools` | 12 -> 6 | Full (the Node count includes the provider's per-request checks). |
| budget setting rejects malformed values, survives restart and template recovery | `sa_tool_budget::budget_setting_rejects_malformed_values_survives_restart_and_template_recovery` | 9 -> 11 | Server half; `templateCommand` is built inline. `parseAgentTemplate` and `restoreAgentCreation` stay in the trimmed Node case (TS modules). |
| resume gets fresh budget without replay; desktop images retain latest three across restart | `sa_tool_budget::resume_gets_fresh_budget_without_replay_desktop_images_retain_latest_three_across_restart` | 17 -> 11 | Kept: stop message, 10 captures, summary without tools, resume once (replayed command), 10 fresh captures, 11 requests, 20 distinct tool ids and screenshots, budget 10. D1: the "latest three images", "omitted" note, 10 earlier results in the resumed context and the per-attempt `budget_summary` call phase are API-runtime context management. |
| legacy channel transfers share attempt budget beyond eight coordination steps | `sa_tool_budget::legacy_channel_transfers_share_attempt_budget_beyond_eight_coordination_steps` | 5 -> 5 | Full after P3; strict counts since the #89 integration (B1x). |
| duplicate provider IDs have distinct protocol message (current attempt) | Dropped | 4 -> 0 | Provider tool-call IDs stay inside the harness protocol since #87 (the server's provider client was removed); the server never sees them. |
| duplicate provider IDs have distinct protocol message (saved call) | Dropped | 4 -> 0 | Same. |
| workflow uses shared per-task budget and final summary | `sa_tool_budget::workflow_uses_shared_per_task_budget_and_final_summary` | 6 -> 6 (shared helper) | Full. |
| routine uses shared per-task budget and final summary | `sa_tool_budget::routine_uses_shared_per_task_budget_and_final_summary` | 6 -> 6 (shared helper) | Full. |
| routine flow uses shared per-task budget and final summary | `sa_tool_budget::routine_flow_uses_shared_per_task_budget_and_final_summary` | 6 -> 6 (shared helper) | Full. |
| invalid final summary fails clearly and never executes tools (string) | `sa_tool_budget::invalid_final_summary_fails_clearly_and_never_executes_tools_string` | 5 -> 5 (shared helper) | Full after P3. |
| invalid final summary fails clearly and never executes tools (object) | `sa_tool_budget::invalid_final_summary_fails_clearly_and_never_executes_tools_object` | 5 -> 5 (shared helper) | Full after P3. |
| reassign gets fresh budget while retaining recorded computer calls | `sa_tool_budget::reassign_gets_fresh_budget_while_retaining_recorded_computer_calls` | 6 -> 6 | Full. |
| automatic question continuation retains attempt budget across restart | `sa_tool_budget::automatic_question_continuation_retains_attempt_budget_across_restart` | 5 -> 5 | Full after P3; strict count (11 requests) since the #89 integration. |

### `app/fallback.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| backup model is opt-in, bounded, durable and never replays after activity or uncertainty | `sa_fallback::backup_model_is_opt_in_bounded_durable_and_never_replays_after_activity_or_uncertainty` | 37 -> 25 | **Owner decision needed.** The brief records that #87 accepts and ignores `backupModel` (the upgrade drops `agents.backup_model`), so no turn can fall back. The test asserts that: `backupModel` is accepted and not stored; every failure class (429, 503, 403, malformed, lost reply, after a tool) makes exactly one attempt with the agent's model and is never replayed by the same command; usage rows exist and never contain the upstream detail; scoped and workflow turns make one attempt; cancel, uncertain resume and edits behave as before. Dropped: the backup-success branches (`answer_model == "backup"`, `["primary","backup"]` call lists, the question and restart-during-backup flows that only the backup model reached), the template `backupModel` parse (TS module, needs a stored backup), the client-side `timeout` scenario and `truncated` (finish reasons are read by the harness since #87). |

### `app/coordination.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| coordination: durable delegation, transfer, cancellation, rejection and recovery | `sa_coordination::coordination_durable_delegation_transfer_cancellation_rejection_and_recovery` | 55 -> 47 | Full; strict call log since the #89 integration (B1x). Holds use `Reply::hold` and `ModelGate::hold_when`. Children are paused, cancelled or restarted after their first model call (the harness reaches the model shortly after the child starts running). |

### `app/agent-messages.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| teammate messages correlate groups, isolate context, persist receipts and bound delivery | `sa_agent_messages::teammate_messages_correlate_groups_isolate_context_persist_receipts_and_bound_delivery` | 48 -> 57 | Full since the #89 integration (M1 no longer applies): the bypass runs while the parent waits, the group result carries the gamma reply and one `delivered_earlier` entry for the bypass, and the bypass received the beta reply. The resumed parent reads its completed group result from its transcript, which ends with the note on replies delivered earlier. |

### `app/agent-interrupt.test.ts`

| Node case | Rust test | Assertions Node -> Rust | Notes |
| --- | --- | --- | --- |
| caller interruption cancels only owned deliveries, journals notice and preserves other work | `sa_agent_interrupt::caller_interruption_cancels_only_owned_deliveries_journals_notice_and_preserves_other_work` | 30 -> 31 | Full. M1: Alpha's own requests and its `inspect`/`stop` turns run in side threads (a waiting parent, including Alpha's channel turn, holds its conversation), Alpha may run 8 threads and the server 10 turns. H1: "no active work", "You can only stop" (self and ancestor) and the channel call (where `interrupt_agent` is not offered) reach the model as errors and the turns end `done` with the error in the answer; the ancestor delivery is `done` with the refusal instead of `failed`. The notice trigger is seeded with `edit_database`; holds use `ModelGate::hold_when` (the kit gate now releases replies that were gated before a release but answered after it). |
<!-- stale-a end -->

## #89 integration

`t3/89-integrate` starts from `t3/87-89-base` (185b92a), which merged the B1-B9
fixes (`t3/89-port-bugs`) with the product, fixture, stale-b and stale-a
slices, all written on `af83887`. The integration fixes the merged failures,
removes every B1 workaround from the ports and ports the Node cases the
slices kept for B1, B2, B3, B6, B7 and B8.

### Merged failures

At 15eacf5 the e2e run had 7 failures; with stale-a merged (185b92a) the e2e
crate did not compile and 4 more tests failed.

| Test | Cause | Fix |
| --- | --- | --- |
| `fx_dispatch::steer_is_injected_once_at_next_tool_loop_safe_point_durable_and_idempotent` | Test: asserted pre-B7 behaviour (a stored, undelivered steer). | Asserts the B7 linked restart (see the dispatch rows above). |
| `fx_dispatch::restart_cancels_and_links_a_new_turn_carries_partial_text_and_saved_results_no_effect_replay` | Test: the steer now restarts the partial turn before it streams, so the partial never came. | The steer turn streams the partial and the explicit restart targets it. |
| `fx_dispatch::server_restart_mid_steer_releases_guidance_once_and_never_replays_uncertain_source` | Test: the source is `cancelled` by the steer's restart, not `interrupted` by the server restart. | Asserts `cancelled` with "Stopped for restart.". |
| `fx_tools::synthetic_scenarios_record_panel_without_duplicate_writes_inline_links_workspace_context_and_approved_shell` | Test: waited for an approval card that B8 no longer shows for a plain check. | The scripted check adds `chmod`, which still needs approval. |
| `pd_workspace_shell::rust_shell_confines_cwd_filesystem_and_environment_and_enforces_read_only_and_approval` | Test: pre-B8 approvals for plain commands. | Plain checks assert no card; denial uses a sensitive command; `env` and the read-only binding keep their cards. |
| `pd_workspace_shell::rust_shell_caps_output_times_out_and_stops_descendants_after_exit_and_cancellation` | Test: pre-B8 approvals for plain commands. | Each step asserts it ran without a card. |
| `sb_harness_default::provider_isolation_account_home_rotation_and_mcp_shell_approval_boundaries` | Test: allowed the pre-B8 card. | Ported the kept B8 assertions (no card, the custom check). |
| e2e build (`sa_slack`, `sa_telegram`) | Test: imported `support::work_calls`, which the B1 fix removed. | Strict `model.calls()`. |
| `sa_agent_messages::teammate_messages_correlate_groups_isolate_context_persist_receipts_and_bound_delivery` | Product: after 1018261c a reply-expecting `send_message` queues the turn but replies at once, so the harness took a third model step (B1 pattern). Test: asserted M1. | Product fix below; the test asserts the Node bypass and `delivered_earlier` values. |
| `sa_agent_result_files::requester_reads_returned_teammate_text_artifacts_but_not_private_inputs_or_no_reply_outputs` and the PDF case | Product: the same extra step after `send_message` re-created the file and changed the returned result on resume. | Product fix below. |
| `sb_crash::sigkill_after_delegated_provider_acceptance_does_not_replay_parent_or_child` | Test: expected the B1x extra parent step (3 calls) once the product fix removed it. | Node's 2 calls. |
| `cli::script_cli_discovers_only_grants_sends_once_waits_preserves_retry_id_and_reports_attention` | Test: a settle wait kept Node's `--timeout 3` and expired on a loaded host. | `WAIT_SECONDS` for every settle wait, as stage 1 did. |

### Product fix

| Bug | Cause and fix |
| --- | --- |
| I1 | B1 was fixed for `ask_user` only. `wait_for_event`, a delegation or transfer that parks the turn, a reply-expecting `send_message` and a Computer hand-off returned to the harness at once, so it took one more model step before the server stopped or resumed the turn. `turns/harness.rs` (`tool`) now stops the run for each of them and holds the MCP reply until the worker has stopped, and `turns/mod.rs` (`run_reserved`) leaves a stopped turn that a tool left `queued` for its resume instead of failing it. The tests that answered or skipped that step (`support.rs`, `fx_support::after_pause`, `pause_reply_for`, `work_calls`, the desktop and stale-a variants) are removed, and every model-call count is strict. |

### Kept cases ported

| Node file | Node case | Rust test | Assertions Node -> Rust |
| --- | --- | --- | ---: |
| `app/rust-desktop-http.test.ts` | HTTP control and user input enforce Origin, revision, validation and rate limits; paused turn resumes once (B1) | `pd_rust_desktop_http::http_control_and_user_input_enforce_origin_revision_validation_and_rate_limits_paused_turn_resumes_once` | 44 -> 39 |
| `app/agent-visibility.test.ts` | hidden agents finish active work, hold queue across restart, reject new work and skip missed schedules (B1) | `fx_agents::hidden_agents_finish_active_work_hold_queue_across_restart_reject_new_work_and_skip_missed_schedules` | 33 -> 27 (+7 TS) |
| `app/script-discovery.test.ts` | private script discovery follows live grants, rotates on restart and preserves foreign files (B2) | `fx_scripts::private_script_discovery_follows_live_grants_rotates_on_restart_and_preserves_foreign_files` | 27 -> 22 |
| `app/script-messages.test.ts` | script messages and answers share grants, scope, receipts, redaction and rate admission (B1) | `fx_scripts::script_messages_and_answers_share_grants_scope_receipts_redaction_and_rate_admission` | 45 -> 50 |
| `app/turn-waits.test.ts` | agent event waits release workers, survive restart, honor pause and accept once (B1) | `fx_waits::agent_event_waits_release_workers_survive_restart_honor_pause_and_accept_once` | 37 -> 25 |
| `app/turn-waits.test.ts` | routine waits preserve artifacts and reject events cancelled before upload completes (B1) | `fx_waits::routine_waits_preserve_artifacts_and_reject_events_cancelled_before_upload_completes` | 26 -> 19 |
| `app/agent-delegation.test.ts` | durable task wait: restart-wait (B1) | `fx_delegation::durable_task_wait_restart_wait` | 7 -> 7 |
| `app/dispatch.test.ts` | steer is injected once at next tool-loop safe point, durable and idempotent (B7) | `fx_dispatch::steer_is_injected_once_at_next_tool_loop_safe_point_durable_and_idempotent` | 15 -> 19 |
| `app/dispatch.test.ts` | restart cancels and links a new turn, carries partial text and saved results, no effect replay (B7) | `fx_dispatch::restart_cancels_and_links_a_new_turn_carries_partial_text_and_saved_results_no_effect_replay` | 12 -> 14 |
| `app/rust-connectors.test.ts` | Rust telegram runner question through native turns (B1) | `fx_connectors::rust_telegram_runner_question_through_native_turns` | 20 -> 22 |
| `app/rust-connectors.test.ts` | Rust slack runner question through native turns (B1) | `fx_connectors::rust_slack_runner_question_through_native_turns` | 20 -> 22 |
| `app/test.ts` | HTTP: persistence, idempotent FIFO turns, isolation, failure and interrupted recovery (B6) | `sb_http::http_persistence_idempotent_fifo_turns_isolation_failure_and_interrupted_recovery` | 184 -> 132 |
| `app/harness-default.test.ts` | provider isolation, account home rotation and MCP shell approval boundaries (B8) | `sb_harness_default::provider_isolation_account_home_rotation_and_mcp_shell_approval_boundaries` | 24 -> 14 |
| `app/script-provider.test.ts` | script provider settings keep identity and credentials isolated, reject busy/revoked changes and preserve receipts (B2, B3) | `sb_script_provider::script_provider_settings_keep_identity_and_credentials_isolated_reject_busy_revoked_changes_and_preserve_receipts` | 40 -> 33 |

Counts are static assertion sites, as in each slice. The B1 rows kept their
Rust sites; their counts became strict. The Node files above are deleted,
except `app/agent-visibility.test.ts` and `app/dispatch.test.ts`, which keep
only their TypeScript-module cases.

### Node cases left from #89

Every remaining case tests a TypeScript module and stays until #89 S3:
`threads` (1), `questions` (1), `agent-create` (2), `agent-visibility` (1),
`composio` (1), `dispatch` (1), `turn-stream` (1), `memory` (1),
`memory-selection` (1), `workflow-http` (1) and `tool-budget` (1), all
`app/<name>.test.ts`.

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
