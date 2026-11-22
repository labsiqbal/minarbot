-- Captured from fresh canonical startup at 85373b3 before collapse.
PRAGMA application_id=1296978481;
PRAGMA user_version=0;
CREATE TABLE accounts(id TEXT PRIMARY KEY, issuer TEXT NOT NULL, clerk_sub TEXT NOT NULL, blocked INTEGER NOT NULL DEFAULT 0 CHECK(blocked IN(0,1)), revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0), created INTEGER NOT NULL, UNIQUE(issuer,clerk_sub)) STRICT;
CREATE TABLE agent_authority(agent TEXT PRIMARY KEY REFERENCES agents(id), owner_account TEXT NOT NULL REFERENCES accounts(id), home_device TEXT NOT NULL REFERENCES devices(id), provider_binding TEXT REFERENCES provider_bindings(id), revision INTEGER NOT NULL CHECK(revision>0)) STRICT;
CREATE TABLE agent_avatars(agent TEXT PRIMARY KEY REFERENCES agents(id),revision INTEGER NOT NULL,image BLOB) STRICT;
CREATE TABLE agent_computers(agent TEXT PRIMARY KEY REFERENCES agents(id),
      enabled INTEGER NOT NULL CHECK(enabled IN(0,1)),revision INTEGER NOT NULL) STRICT;
CREATE TABLE agent_creations(id TEXT PRIMARY KEY REFERENCES agents(id), request TEXT NOT NULL) STRICT;
CREATE TABLE agent_delete_checkpoints(agent TEXT NOT NULL,directory TEXT NOT NULL,ref TEXT NOT NULL,hash TEXT NOT NULL,PRIMARY KEY(agent,directory,ref)) STRICT;
CREATE TABLE agent_deliveries(request TEXT NOT NULL REFERENCES agent_requests(id),turn TEXT PRIMARY KEY REFERENCES turns(id)) STRICT;
CREATE TABLE agent_edits(id TEXT PRIMARY KEY,agent TEXT NOT NULL REFERENCES agents(id),request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE agent_email_addresses(
                agent TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE, slug TEXT NOT NULL UNIQUE
             ) STRICT;
CREATE TABLE agent_email_drafts(
                id TEXT PRIMARY KEY, agent TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
                turn TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE, call_id TEXT NOT NULL,
                ciphertext TEXT NOT NULL, created INTEGER NOT NULL, UNIQUE(turn,call_id)
             ) STRICT;
CREATE TABLE agent_email_messages(
                seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE,
                agent TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
                received INTEGER NOT NULL, digest TEXT NOT NULL, ciphertext TEXT NOT NULL,
                UNIQUE(agent,digest)
             ) STRICT;
CREATE TABLE agent_email_settings(
                id INTEGER PRIMARY KEY CHECK(id=1), domain TEXT, secret TEXT, revision INTEGER NOT NULL
             ) STRICT;
CREATE TABLE agent_folder_commands(id TEXT PRIMARY KEY,agent TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE agent_folders(agent TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,directory TEXT,write INTEGER NOT NULL CHECK(write IN(0,1)),revision INTEGER NOT NULL CHECK(revision>0)) STRICT;
CREATE TABLE agent_message_cooldowns(agent TEXT PRIMARY KEY REFERENCES agents(id),until INTEGER NOT NULL) STRICT;
CREATE TABLE agent_message_events(id TEXT PRIMARY KEY,agent TEXT NOT NULL REFERENCES agents(id),created INTEGER NOT NULL) STRICT;
CREATE TABLE agent_packs(agent TEXT NOT NULL REFERENCES agents(id),pack TEXT NOT NULL REFERENCES skill_packs(id),version INTEGER NOT NULL,enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),removed INTEGER NOT NULL CHECK(removed IN (0,1)),revision INTEGER NOT NULL,PRIMARY KEY(agent,pack),FOREIGN KEY(pack,version) REFERENCES skill_pack_versions(pack,revision)) STRICT;
CREATE TABLE agent_projects(project TEXT NOT NULL REFERENCES projects(id), agent TEXT NOT NULL REFERENCES agents(id), rules TEXT NOT NULL, capabilities_json TEXT NOT NULL CHECK(json_valid(capabilities_json)), revision INTEGER NOT NULL CHECK(revision>0), deleted INTEGER NOT NULL DEFAULT 0 CHECK(deleted IN(0,1)), PRIMARY KEY(project,agent)) STRICT;
CREATE TABLE agent_reply_captures(turn TEXT PRIMARY KEY REFERENCES turns(id)) STRICT;
CREATE TABLE agent_reply_claims(delivery TEXT PRIMARY KEY REFERENCES turns(id),consumer TEXT NOT NULL REFERENCES turns(id),snapshot TEXT NOT NULL) STRICT;
CREATE TABLE agent_requests(id TEXT PRIMARY KEY,turn TEXT NOT NULL REFERENCES turns(id),call_id TEXT NOT NULL,expects_reply INTEGER NOT NULL,settled INTEGER NOT NULL DEFAULT 0,UNIQUE(turn,call_id)) STRICT;
CREATE TABLE agent_skills(agent TEXT NOT NULL REFERENCES agents(id),skill TEXT NOT NULL REFERENCES skills(id),version INTEGER NOT NULL,enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),revision INTEGER NOT NULL, removed INTEGER NOT NULL DEFAULT 0 CHECK(removed IN (0,1)), pack TEXT REFERENCES skill_packs(id),PRIMARY KEY(agent,skill),FOREIGN KEY(skill,version) REFERENCES skill_versions(skill,revision)) STRICT;
CREATE TABLE agent_visibility(agent TEXT PRIMARY KEY REFERENCES agents(id),hidden INTEGER NOT NULL DEFAULT 0,revision INTEGER NOT NULL DEFAULT 0) STRICT;
CREATE TABLE agent_visibility_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE agent_visibility_skips(agent TEXT NOT NULL REFERENCES agents(id),routine TEXT NOT NULL,scheduled_at INTEGER NOT NULL,missed INTEGER NOT NULL,PRIMARY KEY(agent,routine,scheduled_at)) STRICT;
CREATE TABLE agents(id TEXT PRIMARY KEY, name TEXT NOT NULL, model TEXT NOT NULL, instructions TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 1, role TEXT NOT NULL DEFAULT '', tool_budget INTEGER NOT NULL DEFAULT 90, thread_limit INTEGER NOT NULL DEFAULT 2, backup_model TEXT NOT NULL DEFAULT '', provider_account TEXT NOT NULL DEFAULT '', web_access INTEGER NOT NULL DEFAULT 1 CHECK(web_access IN(0,1))) STRICT;
CREATE TABLE authority_state(id INTEGER PRIMARY KEY CHECK(id=1), generation TEXT NOT NULL, serving_state TEXT NOT NULL CHECK(serving_state IN('offline','read_only_recovery','active')), cutover_version INTEGER NOT NULL, recovery_session_hash TEXT, recovery_expires_at INTEGER, revision INTEGER NOT NULL CHECK(revision>0)) STRICT;
CREATE TABLE avatar_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE channel_creations(id TEXT PRIMARY KEY REFERENCES channels(id),request TEXT NOT NULL) STRICT;
CREATE TABLE channel_edits(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE channel_members(channel TEXT NOT NULL REFERENCES channels(id),agent TEXT NOT NULL REFERENCES agents(id),PRIMARY KEY(channel,agent)) STRICT;
CREATE TABLE channels(id TEXT PRIMARY KEY,name TEXT NOT NULL,instructions TEXT NOT NULL,lead TEXT REFERENCES agents(id),revision INTEGER NOT NULL DEFAULT 1,archived INTEGER NOT NULL DEFAULT 0, activity INTEGER NOT NULL DEFAULT 0, read_position INTEGER NOT NULL DEFAULT 0, project TEXT REFERENCES local_projects(id)) STRICT;
CREATE TABLE collaborator_agents(project TEXT NOT NULL, account TEXT NOT NULL, agent TEXT NOT NULL, enabled INTEGER NOT NULL CHECK(enabled IN(0,1)), revision INTEGER NOT NULL CHECK(revision>0), PRIMARY KEY(project,account,agent), FOREIGN KEY(project,account) REFERENCES project_members(project,account), FOREIGN KEY(project,agent) REFERENCES agent_projects(project,agent)) STRICT;
CREATE TABLE compaction_attempts(turn TEXT PRIMARY KEY REFERENCES turns(id),status TEXT NOT NULL,created TEXT NOT NULL) STRICT;
CREATE TABLE compaction_sources(scope TEXT NOT NULL,seq INTEGER NOT NULL REFERENCES turns(seq),revision INTEGER NOT NULL,status TEXT NOT NULL,PRIMARY KEY(scope,seq)) STRICT;
CREATE TABLE compactions(scope TEXT PRIMARY KEY,version TEXT NOT NULL,through INTEGER NOT NULL,summary TEXT NOT NULL) STRICT;
CREATE TABLE composio_auth_configs(toolkit TEXT PRIMARY KEY,config TEXT NOT NULL) STRICT;
CREATE TABLE composio_calls(turn TEXT NOT NULL REFERENCES turns(id),call_id TEXT NOT NULL,agent TEXT NOT NULL REFERENCES agents(id),call TEXT NOT NULL,tool TEXT NOT NULL,state TEXT NOT NULL,decision TEXT,result TEXT,after_output INTEGER NOT NULL,after_action INTEGER NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(turn,call_id)) STRICT;
CREATE TABLE composio_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT) STRICT;
CREATE TABLE composio_grants(agent TEXT NOT NULL REFERENCES agents(id),toolkit TEXT NOT NULL,enabled INTEGER NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(agent,toolkit)) STRICT;
CREATE TABLE composio_installation(id INTEGER PRIMARY KEY CHECK(id=1),user_id TEXT NOT NULL) STRICT;
CREATE TABLE composio_policies(agent TEXT NOT NULL REFERENCES agents(id),fingerprint TEXT NOT NULL,tool TEXT NOT NULL,account TEXT NOT NULL,toolkit TEXT NOT NULL,PRIMARY KEY(agent,fingerprint)) STRICT;
CREATE TABLE composio_policy_projects(fingerprint TEXT PRIMARY KEY,project TEXT NOT NULL) STRICT;
CREATE TABLE composio_project_accounts(project TEXT NOT NULL,account TEXT NOT NULL,toolkit TEXT NOT NULL,removed INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(project,account)) STRICT;
CREATE TABLE composio_project_grants(project TEXT NOT NULL,agent TEXT NOT NULL REFERENCES agents(id),toolkit TEXT NOT NULL,enabled INTEGER NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(project,agent,toolkit)) STRICT;
CREATE TABLE composio_project_identities(project TEXT PRIMARY KEY,user_id TEXT NOT NULL) STRICT;
CREATE TABLE composio_settings(id INTEGER PRIMARY KEY CHECK(id=1),revision INTEGER NOT NULL,ciphertext TEXT NOT NULL) STRICT;
CREATE TABLE computer_actions(turn TEXT NOT NULL REFERENCES turns(id),call_id TEXT NOT NULL,
      state TEXT NOT NULL CHECK(state IN('uncertain','done')),image BLOB,PRIMARY KEY(turn,call_id)) STRICT;
CREATE TABLE computer_always_on(agent TEXT PRIMARY KEY REFERENCES agent_computers(agent) ON DELETE CASCADE) STRICT;
CREATE TABLE computer_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE computer_control(agent TEXT PRIMARY KEY REFERENCES agents(id),
      mode TEXT NOT NULL CHECK(mode IN('agent','user')),who TEXT NOT NULL,since TEXT NOT NULL,revision INTEGER NOT NULL) STRICT;
CREATE TABLE computer_peer_messaging(token_id TEXT PRIMARY KEY,allowed INTEGER NOT NULL) STRICT;
CREATE TABLE computer_settings(id INTEGER PRIMARY KEY CHECK(id=1), enabled INTEGER NOT NULL CHECK(enabled IN(0,1))) STRICT;
CREATE TABLE computer_shared_profiles(
    agent TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE
) STRICT;
CREATE TABLE computer_waits(turn TEXT PRIMARY KEY REFERENCES turns(id),agent TEXT NOT NULL REFERENCES agents(id),
      call_id TEXT NOT NULL,reason TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN('waiting','returned','declined')),pause_revision INTEGER NOT NULL) STRICT;
CREATE TABLE context_transfers(id TEXT PRIMARY KEY,source_kind TEXT NOT NULL,source_id TEXT NOT NULL,cutoff_turn TEXT NOT NULL REFERENCES turns(id),target_thread TEXT NOT NULL REFERENCES threads(id),agent TEXT NOT NULL REFERENCES agents(id),transcript TEXT NOT NULL,created TEXT NOT NULL,request_id TEXT NOT NULL UNIQUE) STRICT;
CREATE TABLE conversation_settings(id INTEGER PRIMARY KEY CHECK(id=1),auto_archive INTEGER NOT NULL CHECK(auto_archive IN (0,1))) STRICT;
CREATE TABLE coordination(seq INTEGER PRIMARY KEY AUTOINCREMENT,turn TEXT NOT NULL REFERENCES turns(id),owner TEXT NOT NULL REFERENCES agents(id),target TEXT NOT NULL REFERENCES agents(id),kind TEXT NOT NULL,prompt TEXT NOT NULL,call TEXT NOT NULL,child TEXT REFERENCES turns(id)) STRICT;
CREATE TABLE delegation_tasks(id TEXT PRIMARY KEY,creator_turn TEXT NOT NULL REFERENCES turns(id),parent_task TEXT REFERENCES delegation_tasks(id),request TEXT NOT NULL REFERENCES agent_requests(id),delivery TEXT NOT NULL REFERENCES turns(id),wait INTEGER NOT NULL DEFAULT 0,wake_delivered INTEGER NOT NULL DEFAULT 0,created INTEGER NOT NULL) STRICT;
CREATE TABLE delegation_waits(id TEXT PRIMARY KEY,turn TEXT NOT NULL REFERENCES turns(id),call_id TEXT NOT NULL,call TEXT NOT NULL,task TEXT NOT NULL REFERENCES delegation_tasks(id),deadline INTEGER NOT NULL,status TEXT NOT NULL CHECK(status IN ('waiting','received','expired','closed')),after_output INTEGER NOT NULL,after_action INTEGER NOT NULL,result TEXT,UNIQUE(turn,call_id)) STRICT;
CREATE TABLE deleted_agents(id TEXT PRIMARY KEY) STRICT;
CREATE TABLE delivery_conversations(
    thread TEXT PRIMARY KEY REFERENCES threads(id),sender TEXT NOT NULL,machine TEXT,activity INTEGER NOT NULL,settled_at INTEGER) STRICT;
CREATE TABLE device_resource_bindings(binding_id TEXT PRIMARY KEY, project_id TEXT NOT NULL, directory TEXT NOT NULL, write INTEGER NOT NULL CHECK(write IN(0,1)), revision INTEGER NOT NULL CHECK(revision>0), deleted INTEGER NOT NULL CHECK(deleted IN(0,1))) STRICT;
CREATE TABLE device_sessions(id_hash TEXT PRIMARY KEY, device TEXT NOT NULL REFERENCES devices(id), generation INTEGER NOT NULL, authority_generation TEXT NOT NULL, expires_at INTEGER NOT NULL, revoked INTEGER NOT NULL CHECK(revoked IN(0,1))) STRICT;
CREATE TABLE devices(id TEXT PRIMARY KEY, installation TEXT NOT NULL REFERENCES installations(id), account TEXT NOT NULL REFERENCES accounts(id), signing_public_key TEXT NOT NULL, encryption_public_key TEXT NOT NULL, signing_thumbprint TEXT NOT NULL, encryption_thumbprint TEXT NOT NULL, descriptor_json TEXT NOT NULL CHECK(json_valid(descriptor_json)), generation INTEGER NOT NULL CHECK(generation>0), connection_generation INTEGER NOT NULL CHECK(connection_generation>=0), status TEXT NOT NULL CHECK(status IN('offline','connecting','ready','blocked','revoked')), zone TEXT NOT NULL, zone_revision INTEGER NOT NULL CHECK(zone_revision>0), last_seen INTEGER, capabilities_json TEXT NOT NULL CHECK(json_valid(capabilities_json)), revision INTEGER NOT NULL CHECK(revision>0)) STRICT;
CREATE TABLE discord_bot(
    singleton INTEGER PRIMARY KEY CHECK(singleton=1), generation TEXT NOT NULL,
    bot TEXT, ciphertext TEXT, status TEXT NOT NULL
) STRICT;
CREATE TABLE discord_choices(
    notice TEXT PRIMARY KEY, job TEXT NOT NULL REFERENCES discord_jobs(id),
    subject TEXT NOT NULL, revision INTEGER NOT NULL, kind TEXT NOT NULL,
    options TEXT NOT NULL, decided INTEGER NOT NULL DEFAULT 0,
    UNIQUE(subject, revision, kind)
) STRICT;
CREATE TABLE discord_events(
    message TEXT PRIMARY KEY, route TEXT NOT NULL, sender TEXT NOT NULL,
    text TEXT NOT NULL, attachments TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'queued'
) STRICT;
CREATE TABLE discord_jobs(
    id TEXT PRIMARY KEY, message TEXT NOT NULL REFERENCES discord_events(message),
    kind TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'running'
) STRICT;
CREATE TABLE discord_outbox(
    id TEXT PRIMARY KEY, route TEXT NOT NULL, reply_to TEXT, text TEXT NOT NULL,
    files TEXT NOT NULL, buttons TEXT, status TEXT NOT NULL DEFAULT 'queued',
    part INTEGER NOT NULL DEFAULT 0, total INTEGER NOT NULL, remote TEXT, error TEXT,
    workflow_output INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE TABLE discord_routes(
    id TEXT PRIMARY KEY, channel TEXT NOT NULL UNIQUE, kind TEXT NOT NULL,
    target TEXT NOT NULL, users TEXT NOT NULL, thread TEXT NOT NULL UNIQUE
) STRICT;
CREATE TABLE dispatch_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE dispatch_links(id TEXT PRIMARY KEY,source_turn TEXT NOT NULL REFERENCES turns(id),target_turn TEXT NOT NULL REFERENCES turns(id),mode TEXT NOT NULL CHECK(mode IN ('steer','restart')),context TEXT NOT NULL,created TEXT NOT NULL,UNIQUE(source_turn,target_turn)) STRICT;
CREATE TABLE environment_identity(singleton INTEGER PRIMARY KEY CHECK(singleton=1),environment_id TEXT UNIQUE NOT NULL,name TEXT NOT NULL) STRICT;
CREATE TABLE execution_owners(task TEXT PRIMARY KEY REFERENCES tasks(id), device TEXT NOT NULL REFERENCES devices(id), authority_generation TEXT NOT NULL, device_generation INTEGER NOT NULL, executor_generation INTEGER NOT NULL CHECK(executor_generation>0), token_hash TEXT NOT NULL, lock_proof_json TEXT NOT NULL CHECK(json_valid(lock_proof_json)), reconciled_through INTEGER NOT NULL CHECK(reconciled_through>=0), last_seen INTEGER NOT NULL, stopped INTEGER NOT NULL CHECK(stopped IN(0,1))) STRICT;
CREATE TABLE files (seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL,
      scope TEXT NOT NULL, name TEXT NOT NULL, digest TEXT NOT NULL, bytes BLOB NOT NULL, created TEXT NOT NULL, deleted INTEGER NOT NULL DEFAULT 0) STRICT;
CREATE TABLE flow_tool_calls(seq INTEGER PRIMARY KEY AUTOINCREMENT,turn TEXT NOT NULL REFERENCES turns(id),call_id TEXT NOT NULL,call TEXT NOT NULL,result TEXT NOT NULL,after_output INTEGER NOT NULL,after_action INTEGER,UNIQUE(turn,call_id)) STRICT;
CREATE TABLE fork_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE generated_files (seq INTEGER PRIMARY KEY AUTOINCREMENT,
      turn TEXT NOT NULL REFERENCES turns(id), file TEXT NOT NULL REFERENCES files(id),
      call_id TEXT NOT NULL, call TEXT NOT NULL, after_action INTEGER NOT NULL, workspace TEXT,
      UNIQUE(turn,call_id)) STRICT;
CREATE TABLE harness_approvals(id TEXT PRIMARY KEY,turn TEXT NOT NULL REFERENCES turns(id),agent TEXT NOT NULL REFERENCES agents(id),runtime TEXT NOT NULL,workspace TEXT NOT NULL,kind TEXT NOT NULL,description TEXT NOT NULL,command TEXT,prefix TEXT,input TEXT NOT NULL,state TEXT NOT NULL,decision TEXT,revision INTEGER NOT NULL DEFAULT 1) STRICT;
CREATE TABLE harness_card_time(turn TEXT PRIMARY KEY REFERENCES turns(id) ON DELETE CASCADE,active_ms INTEGER NOT NULL CHECK(active_ms>=0),idle_remaining_ms INTEGER NOT NULL CHECK(idle_remaining_ms>=0)) STRICT;
CREATE TABLE harness_catalog(runtime TEXT PRIMARY KEY,models TEXT NOT NULL) STRICT;
CREATE TABLE harness_decisions(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE harness_models(agent TEXT NOT NULL REFERENCES agents(id),runtime TEXT NOT NULL,model TEXT NOT NULL,PRIMARY KEY(agent,runtime)) STRICT;
CREATE TABLE harness_policies(agent TEXT NOT NULL REFERENCES agents(id),runtime TEXT NOT NULL,workspace TEXT NOT NULL,prefix TEXT NOT NULL,PRIMARY KEY(agent,runtime,workspace,prefix)) STRICT;
CREATE TABLE harness_sessions(agent TEXT NOT NULL REFERENCES agents(id),conversation TEXT NOT NULL,runtime TEXT NOT NULL,workspace TEXT NOT NULL,session TEXT NOT NULL,PRIMARY KEY(agent,conversation,runtime,workspace)) STRICT;
CREATE TABLE harness_stderr(
    call INTEGER PRIMARY KEY REFERENCES provider_calls(seq), tail TEXT NOT NULL
) STRICT;
CREATE TABLE harness_steps(turn TEXT NOT NULL REFERENCES turns(id),id TEXT NOT NULL,name TEXT NOT NULL,state TEXT NOT NULL,PRIMARY KEY(turn,id)) STRICT;
CREATE TABLE hub_link_actions (action_id TEXT PRIMARY KEY,turn_id TEXT NOT NULL,digest TEXT NOT NULL,kind TEXT NOT NULL,state TEXT NOT NULL,approved INTEGER NOT NULL DEFAULT 0,lease TEXT,payload TEXT NOT NULL) STRICT;
CREATE TABLE hub_link_events (event_id TEXT PRIMARY KEY,project_id TEXT NOT NULL,section_id TEXT NOT NULL,participation_id TEXT NOT NULL,digest TEXT NOT NULL,frame TEXT NOT NULL) STRICT;
CREATE TABLE hub_link_grants (project_id TEXT NOT NULL,participation_id TEXT NOT NULL,value TEXT NOT NULL,enabled INTEGER NOT NULL DEFAULT 1,PRIMARY KEY(project_id,participation_id)) STRICT;
CREATE TABLE hub_link_identity (id INTEGER PRIMARY KEY CHECK(id=1),installation_id TEXT NOT NULL,node_id TEXT NOT NULL,origin TEXT NOT NULL,hub_key TEXT NOT NULL,account_id TEXT,node_generation INTEGER,authority_generation TEXT,disabled INTEGER NOT NULL DEFAULT 0) STRICT;
CREATE TABLE hub_link_profile (id INTEGER PRIMARY KEY CHECK(id=1),name TEXT NOT NULL) STRICT;
CREATE TABLE hub_link_requests (project_id TEXT NOT NULL,participation_id TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(project_id,participation_id)) STRICT;
CREATE TABLE hub_link_results (turn_id TEXT NOT NULL,sequence INTEGER NOT NULL,frame TEXT NOT NULL,acked INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(turn_id,sequence)) STRICT;
CREATE TABLE hub_link_runtime(id INTEGER PRIMARY KEY CHECK(id=1),value TEXT NOT NULL) STRICT;
CREATE TABLE hub_link_threads (project_id TEXT NOT NULL,section_id TEXT NOT NULL,agent_id TEXT NOT NULL,thread TEXT NOT NULL,PRIMARY KEY(project_id,section_id,agent_id)) STRICT;
CREATE TABLE hub_link_turns (turn_id TEXT PRIMARY KEY,event_id TEXT UNIQUE NOT NULL,digest TEXT NOT NULL,frame TEXT NOT NULL,state TEXT NOT NULL,sequence INTEGER NOT NULL DEFAULT 0,published TEXT NOT NULL DEFAULT '',spent INTEGER NOT NULL DEFAULT 0) STRICT;
CREATE TABLE hub_link_withdrawals (project_id TEXT NOT NULL,participation_id TEXT NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(project_id,participation_id)) STRICT;
CREATE TABLE image_generations(turn TEXT NOT NULL REFERENCES turns(id),call TEXT NOT NULL,file TEXT REFERENCES files(id),uncertain INTEGER NOT NULL,PRIMARY KEY(turn,call)) STRICT;
CREATE TABLE installations(id TEXT PRIMARY KEY, account TEXT NOT NULL REFERENCES accounts(id), signing_public_key TEXT NOT NULL, signing_thumbprint TEXT NOT NULL UNIQUE, proof_revision INTEGER NOT NULL DEFAULT 1 CHECK(proof_revision>0), revoked INTEGER NOT NULL DEFAULT 0 CHECK(revoked IN(0,1)), revision INTEGER NOT NULL CHECK(revision>0), created INTEGER NOT NULL) STRICT;
CREATE TABLE invitations(id TEXT PRIMARY KEY, project TEXT NOT NULL REFERENCES projects(id), authority_generation TEXT NOT NULL, email_hash TEXT NOT NULL, target_clerk_sub TEXT, state TEXT NOT NULL CHECK(state IN('pending_delivery','sent','delivery_unknown','accepted','revoked','expired','failed')), token_digest TEXT NOT NULL UNIQUE, assignments_json TEXT NOT NULL CHECK(json_valid(assignments_json)), expires_at INTEGER NOT NULL, used_at INTEGER, revision INTEGER NOT NULL CHECK(revision>0)) STRICT;
CREATE TABLE link_challenges(id TEXT PRIMARY KEY, account TEXT NOT NULL REFERENCES accounts(id), sid TEXT NOT NULL, installation TEXT NOT NULL, device TEXT NOT NULL, installation_thumbprint TEXT NOT NULL, signing_thumbprint TEXT NOT NULL, encryption_thumbprint TEXT NOT NULL, candidate_json TEXT NOT NULL CHECK(json_valid(candidate_json)), nonce_digest TEXT NOT NULL UNIQUE, expires_at INTEGER NOT NULL, consumed INTEGER NOT NULL CHECK(consumed IN(0,1))) STRICT;
CREATE TABLE local_project_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE local_project_folder_intents(command TEXT PRIMARY KEY,request TEXT NOT NULL,folders TEXT NOT NULL) STRICT;
CREATE TABLE local_project_folders(project TEXT NOT NULL REFERENCES local_projects(id),directory TEXT NOT NULL,PRIMARY KEY(project,directory)) STRICT;
CREATE TABLE local_project_legacy_channels(channel TEXT PRIMARY KEY REFERENCES channels(id)) STRICT;
CREATE TABLE local_project_members(project TEXT NOT NULL REFERENCES local_projects(id),agent TEXT NOT NULL REFERENCES agents(id),write INTEGER NOT NULL CHECK(write IN(0,1)),PRIMARY KEY(project,agent)) STRICT;
CREATE TABLE local_project_sections(project TEXT NOT NULL REFERENCES local_projects(id),channel TEXT PRIMARY KEY REFERENCES channels(id),name TEXT NOT NULL,position INTEGER NOT NULL,archived INTEGER NOT NULL DEFAULT 0,revision INTEGER NOT NULL DEFAULT 1,UNIQUE(project,name)) STRICT;
CREATE TABLE local_project_threads(thread TEXT PRIMARY KEY REFERENCES threads(id),project TEXT NOT NULL REFERENCES local_projects(id),folder TEXT NOT NULL) STRICT;
CREATE TABLE local_projects(id TEXT PRIMARY KEY,name TEXT NOT NULL,description TEXT NOT NULL,revision INTEGER NOT NULL DEFAULT 1,deleted INTEGER NOT NULL DEFAULT 0,created INTEGER NOT NULL,updated INTEGER NOT NULL,channel TEXT NOT NULL, archived INTEGER NOT NULL DEFAULT 0, codegraph_enabled INTEGER NOT NULL DEFAULT 0 CHECK(codegraph_enabled IN(0,1)), allow_installation_apps INTEGER NOT NULL DEFAULT 0) STRICT;
CREATE TABLE local_sites (
    id TEXT PRIMARY KEY, agent TEXT NOT NULL REFERENCES agents(id), name TEXT NOT NULL,
    source TEXT NOT NULL, html TEXT NOT NULL, revision INTEGER NOT NULL, deleted INTEGER NOT NULL DEFAULT 0,
    updated TEXT NOT NULL
  ) STRICT;
CREATE TABLE machine_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,environment_id TEXT,result TEXT) STRICT;
CREATE TABLE machine_provider_reports(
    environment_id TEXT PRIMARY KEY REFERENCES paired_machines(environment_id) ON DELETE CASCADE,
    report TEXT NOT NULL CHECK(json_valid(report))
) STRICT;
CREATE TABLE machine_update_policy(environment_id TEXT PRIMARY KEY,allowed INTEGER NOT NULL DEFAULT 0) STRICT;
CREATE TABLE mail_outbox(id TEXT PRIMARY KEY REFERENCES invitations(id), revision INTEGER NOT NULL, ciphertext BLOB NOT NULL, state TEXT NOT NULL CHECK(state IN('queued','sending','sent','unknown','failed','revoked')), first_attempt INTEGER, next_attempt INTEGER, attempts INTEGER NOT NULL CHECK(attempts>=0), remote_id TEXT, payload_digest TEXT NOT NULL) STRICT;
CREATE TABLE mcp_calls(turn TEXT NOT NULL REFERENCES turns(id),call TEXT NOT NULL,uncertain INTEGER NOT NULL,PRIMARY KEY(turn,call)) STRICT;
CREATE TABLE mcp_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE mcp_connections(id TEXT PRIMARY KEY,name TEXT NOT NULL,url TEXT NOT NULL,token TEXT NOT NULL,tools TEXT NOT NULL,enabled INTEGER NOT NULL,revision INTEGER NOT NULL) STRICT;
CREATE TABLE mcp_feature_commands(caller TEXT NOT NULL,request_id TEXT NOT NULL,digest TEXT NOT NULL,plan TEXT,result TEXT,PRIMARY KEY(caller,request_id)) STRICT;
CREATE TABLE mcp_files(turn TEXT NOT NULL REFERENCES turns(id),file TEXT NOT NULL REFERENCES files(id),PRIMARY KEY(turn,file)) STRICT;
CREATE TABLE mcp_grants(agent TEXT NOT NULL REFERENCES agents(id),connection TEXT NOT NULL REFERENCES mcp_connections(id),revision INTEGER NOT NULL,tools TEXT NOT NULL,PRIMARY KEY(agent,connection)) STRICT;
CREATE TABLE mcp_projects(connection TEXT PRIMARY KEY REFERENCES mcp_connections(id),project TEXT NOT NULL) STRICT;
CREATE TABLE mcp_server_commands(id TEXT PRIMARY KEY,digest TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE mcp_server_messages (
    caller TEXT NOT NULL REFERENCES mcp_server_tokens(id), request_id TEXT NOT NULL, digest TEXT NOT NULL,
    turn TEXT NOT NULL UNIQUE REFERENCES turns(id), result TEXT NOT NULL, PRIMARY KEY(caller,request_id)
  ) STRICT;
CREATE TABLE mcp_server_tokens (
    id TEXT PRIMARY KEY, token_hash TEXT NOT NULL UNIQUE, kind TEXT NOT NULL CHECK(kind IN ('session','peer')),
    name TEXT NOT NULL, agent TEXT REFERENCES agents(id), turn TEXT REFERENCES turns(id), scopes TEXT NOT NULL,
    created INTEGER NOT NULL, last_used INTEGER, revoked INTEGER, expires INTEGER, request_digest TEXT NOT NULL
  ) STRICT;
CREATE TABLE mcp_test_projects(test TEXT PRIMARY KEY,project TEXT NOT NULL) STRICT;
CREATE TABLE mcp_tests(id TEXT PRIMARY KEY,digest TEXT NOT NULL,name TEXT NOT NULL,url TEXT NOT NULL,token TEXT NOT NULL,tools TEXT NOT NULL,expires INTEGER NOT NULL,connection TEXT,revision INTEGER) STRICT;
CREATE TABLE memories(scope TEXT NOT NULL,id TEXT NOT NULL,text TEXT NOT NULL,essential INTEGER NOT NULL,revision INTEGER NOT NULL,removed INTEGER NOT NULL, selection_locked INTEGER NOT NULL DEFAULT 1 CHECK(selection_locked IN(0,1)),PRIMARY KEY(scope,id)) STRICT;
CREATE TABLE memory_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE model_fallbacks(turn TEXT PRIMARY KEY REFERENCES turns(id),agent TEXT NOT NULL REFERENCES agents(id),model TEXT NOT NULL) STRICT;
CREATE TABLE occurrences(schedule TEXT NOT NULL REFERENCES schedules(id),occurrence_id TEXT NOT NULL,due_at INTEGER NOT NULL,schedule_revision INTEGER NOT NULL,zone_revision INTEGER NOT NULL,initiator_account TEXT NOT NULL REFERENCES accounts(id),assignment_revision INTEGER NOT NULL CHECK(assignment_revision>0),authority_generation TEXT NOT NULL,task TEXT REFERENCES tasks(id),state TEXT NOT NULL CHECK(state IN('pending','admitted','consumed','blocked')),missed_count INTEGER NOT NULL CHECK(missed_count>=0),run TEXT UNIQUE REFERENCES workflow_runs(id),admission_id TEXT NOT NULL UNIQUE,observation_revision INTEGER NOT NULL DEFAULT 1 CHECK(observation_revision>0),PRIMARY KEY(schedule,occurrence_id)) STRICT;
CREATE TABLE paired_machines(environment_id TEXT PRIMARY KEY,name TEXT NOT NULL,origin TEXT UNIQUE NOT NULL,token TEXT NOT NULL,local_token_id TEXT NOT NULL,return_token TEXT,state TEXT NOT NULL,status TEXT NOT NULL,version TEXT NOT NULL,last_seen INTEGER,agents TEXT NOT NULL,allow_agent_messaging INTEGER NOT NULL DEFAULT 1) STRICT;
CREATE TABLE pairing_codes(id TEXT PRIMARY KEY,digest TEXT UNIQUE NOT NULL,expires INTEGER NOT NULL,used INTEGER NOT NULL DEFAULT 0) STRICT;
CREATE TABLE pairing_rate_limit(bucket INTEGER PRIMARY KEY,attempts INTEGER NOT NULL) STRICT;
CREATE TABLE pdf_text(file TEXT PRIMARY KEY REFERENCES files(id),text TEXT NOT NULL) STRICT;
CREATE TABLE peer_message_cooldowns(peer TEXT PRIMARY KEY,until INTEGER NOT NULL) STRICT;
CREATE TABLE peer_message_events(id TEXT PRIMARY KEY,peer TEXT NOT NULL,created INTEGER NOT NULL) STRICT;
CREATE TABLE peer_pairing_pending(token_id TEXT PRIMARY KEY,environment_id TEXT UNIQUE NOT NULL,name TEXT NOT NULL,origin TEXT NOT NULL,version TEXT NOT NULL,expires INTEGER NOT NULL) STRICT;
CREATE TABLE peer_revocations(token_id TEXT PRIMARY KEY) STRICT;
CREATE TABLE project_environment(project TEXT NOT NULL,name TEXT NOT NULL,ciphertext TEXT NOT NULL,updated INTEGER NOT NULL,PRIMARY KEY(project,name)) STRICT;
CREATE TABLE project_environment_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE project_environment_redactions(project TEXT NOT NULL,name TEXT NOT NULL,ciphertext TEXT NOT NULL,PRIMARY KEY(project,name,ciphertext)) STRICT;
CREATE TABLE project_environment_settings(project TEXT PRIMARY KEY,expose_computer INTEGER NOT NULL DEFAULT 0,revision INTEGER NOT NULL DEFAULT 1) STRICT;
CREATE TABLE project_members(project TEXT NOT NULL REFERENCES projects(id), account TEXT NOT NULL REFERENCES accounts(id), role TEXT NOT NULL CHECK(role IN('owner','member')), grant_revision INTEGER NOT NULL CHECK(grant_revision>0), deleted INTEGER NOT NULL DEFAULT 0 CHECK(deleted IN(0,1)), PRIMARY KEY(project,account)) STRICT;
CREATE TABLE project_workflow_notifications(seq INTEGER PRIMARY KEY AUTOINCREMENT,project TEXT NOT NULL REFERENCES projects(id),account TEXT NOT NULL REFERENCES accounts(id),run TEXT NOT NULL REFERENCES workflow_runs(id),name TEXT NOT NULL,created INTEGER NOT NULL,read INTEGER NOT NULL DEFAULT 0 CHECK(read IN(0,1)),UNIQUE(project,account,run)) STRICT;
CREATE TABLE projects(id TEXT PRIMARY KEY, account TEXT NOT NULL REFERENCES accounts(id), name TEXT NOT NULL, private INTEGER NOT NULL CHECK(private IN(0,1)), deleted INTEGER NOT NULL DEFAULT 0 CHECK(deleted IN(0,1)), revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0)) STRICT;
CREATE TABLE proof_replays(device TEXT NOT NULL REFERENCES devices(id), nonce_digest TEXT NOT NULL, expires_at INTEGER NOT NULL, PRIMARY KEY(device,nonce_digest)) STRICT;
CREATE TABLE provider_account_changes(question TEXT PRIMARY KEY REFERENCES questions(id),agent TEXT NOT NULL REFERENCES agents(id),revision INTEGER NOT NULL,account TEXT NOT NULL,model TEXT NOT NULL,binding TEXT NOT NULL) STRICT;
CREATE TABLE provider_accounts(
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    family TEXT NOT NULL CHECK(family IN('openai-compatible','anthropic','gemini')),
    base_url TEXT NOT NULL,
    key_ciphertext TEXT NOT NULL,
    default_model TEXT NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1
) STRICT;
CREATE TABLE provider_bindings(id TEXT PRIMARY KEY, device TEXT NOT NULL REFERENCES devices(id), owner_account TEXT NOT NULL REFERENCES accounts(id), provider_id TEXT NOT NULL CHECK(provider_id IN('openai-compatible','anthropic','gemini')), provider_account_id TEXT NOT NULL, label TEXT NOT NULL, readiness TEXT NOT NULL CHECK(readiness IN('missing','ready','unreadable','unavailable','revoked')), revision INTEGER NOT NULL CHECK(revision>0), UNIQUE(device,provider_id,provider_account_id)) STRICT;
CREATE TABLE provider_calls(seq INTEGER PRIMARY KEY AUTOINCREMENT,turn TEXT NOT NULL REFERENCES turns(id),agent TEXT NOT NULL REFERENCES agents(id),model TEXT NOT NULL,origin TEXT NOT NULL,purpose TEXT NOT NULL CHECK(purpose IN('routing','completion')),status TEXT NOT NULL CHECK(status IN('pending','received','failed','interrupted')),created TEXT NOT NULL,finished TEXT,input_tokens INTEGER,output_tokens INTEGER,total_tokens INTEGER, phase TEXT NOT NULL DEFAULT 'answer', family TEXT NOT NULL DEFAULT 'openai-compatible', account TEXT NOT NULL DEFAULT '') STRICT;
CREATE TABLE provider_stream_segments(call INTEGER NOT NULL REFERENCES provider_calls(seq),ordinal INTEGER NOT NULL,id TEXT NOT NULL,kind TEXT NOT NULL,text TEXT NOT NULL,state TEXT NOT NULL,final INTEGER NOT NULL DEFAULT 0,revision INTEGER NOT NULL,PRIMARY KEY(call,ordinal),UNIQUE(call,id)) STRICT;
CREATE TABLE provider_streams(call INTEGER PRIMARY KEY REFERENCES provider_calls(seq),text TEXT NOT NULL) STRICT;
CREATE TABLE question_answers(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE questions(id TEXT PRIMARY KEY,turn TEXT NOT NULL REFERENCES turns(id),call_id TEXT NOT NULL,call TEXT NOT NULL,prompt TEXT NOT NULL,choices TEXT NOT NULL,answer TEXT,after_output INTEGER NOT NULL,after_action INTEGER NOT NULL,UNIQUE(turn,call_id)) STRICT;
CREATE TABLE queue_edit_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE queue_edits(editor TEXT PRIMARY KEY,turn TEXT NOT NULL REFERENCES turns(id),state TEXT NOT NULL,original TEXT NOT NULL,expires_at INTEGER NOT NULL DEFAULT 0) STRICT;
CREATE TABLE queue_order_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE queue_positions(turn TEXT PRIMARY KEY REFERENCES turns(id),position INTEGER NOT NULL) STRICT;
CREATE TABLE receipts(request_id TEXT PRIMARY KEY, actor TEXT NOT NULL, project TEXT REFERENCES projects(id), scope TEXT NOT NULL, payload_digest TEXT NOT NULL, status INTEGER NOT NULL, result_json TEXT NOT NULL CHECK(json_valid(result_json)), created INTEGER NOT NULL) STRICT;
CREATE TABLE record_scopes(table_name TEXT NOT NULL, key_json TEXT NOT NULL CHECK(json_valid(key_json)), account TEXT NOT NULL REFERENCES accounts(id), project TEXT NOT NULL REFERENCES projects(id), visibility TEXT NOT NULL CHECK(visibility IN('owner','project')), root_kind TEXT NOT NULL, root_id TEXT NOT NULL, PRIMARY KEY(table_name,key_json)) STRICT;
CREATE TABLE recovery_evidence_sessions(id_hash TEXT PRIMARY KEY, request_id TEXT NOT NULL UNIQUE REFERENCES receipts(request_id), recovery_generation TEXT NOT NULL UNIQUE, authority_generation TEXT NOT NULL, owner_account TEXT NOT NULL REFERENCES accounts(id), task TEXT NOT NULL REFERENCES tasks(id), source_device TEXT NOT NULL REFERENCES devices(id), source_generation INTEGER NOT NULL CHECK(source_generation>0), expires_at INTEGER NOT NULL, revoked INTEGER NOT NULL CHECK(revoked IN(0,1))) STRICT;
CREATE TABLE recovery_imports(request_id TEXT PRIMARY KEY REFERENCES receipts(request_id), source_device TEXT NOT NULL REFERENCES devices(id), source_authority TEXT NOT NULL, watermark INTEGER NOT NULL CHECK(watermark>=0), recovery_generation TEXT NOT NULL, envelope_digest TEXT NOT NULL, envelope_json TEXT NOT NULL CHECK(json_valid(envelope_json)), UNIQUE(recovery_generation,source_device,source_authority,watermark)) STRICT;
CREATE TABLE recovery_quarantine(request_id TEXT NOT NULL REFERENCES receipts(request_id), owner_account TEXT NOT NULL REFERENCES accounts(id), task TEXT NOT NULL, action TEXT NOT NULL, source_device TEXT NOT NULL REFERENCES devices(id), source_authority TEXT NOT NULL, recovery_generation TEXT NOT NULL, watermark INTEGER NOT NULL CHECK(watermark>=0), envelope_digest TEXT NOT NULL, envelope_json TEXT NOT NULL CHECK(json_valid(envelope_json)), reason TEXT NOT NULL, created INTEGER NOT NULL, PRIMARY KEY(request_id,action)) STRICT;
CREATE TABLE remote_agent_calls(
    id TEXT PRIMARY KEY, machine TEXT NOT NULL, agent TEXT NOT NULL, name TEXT NOT NULL, text TEXT NOT NULL,
    depth INTEGER NOT NULL, delivery TEXT UNIQUE REFERENCES turns(id), state TEXT NOT NULL CHECK(state IN ('intent','sending','sent','result','uncertain')),
    remote_turn TEXT, next_poll INTEGER NOT NULL DEFAULT 0, attempts INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL,
    cancel_requested INTEGER NOT NULL DEFAULT 0, cancel_sent INTEGER NOT NULL DEFAULT 0, error TEXT, status TEXT NOT NULL DEFAULT 'queued', reply TEXT
  ) STRICT;
CREATE TABLE response_policies(project TEXT NOT NULL, account TEXT NOT NULL, agent TEXT NOT NULL, respond INTEGER NOT NULL CHECK(respond IN(0,1)), revision INTEGER NOT NULL CHECK(revision>0), PRIMARY KEY(project,account,agent), FOREIGN KEY(project,account,agent) REFERENCES collaborator_agents(project,account,agent)) STRICT;
CREATE TABLE retention_floors(
  account TEXT NOT NULL REFERENCES accounts(id),
  project TEXT NOT NULL REFERENCES projects(id),
  event_position INTEGER NOT NULL CHECK(event_position>=0),
  updated_at INTEGER NOT NULL,
  PRIMARY KEY(account,project)
) STRICT;
CREATE TABLE room_admissions(turn TEXT PRIMARY KEY REFERENCES turns(id),watermark INTEGER NOT NULL,cutoff INTEGER NOT NULL,context TEXT NOT NULL) STRICT;
CREATE TABLE room_discussions(root TEXT PRIMARY KEY REFERENCES turns(id),members TEXT NOT NULL,lead TEXT,mentioned INTEGER NOT NULL,fallback INTEGER NOT NULL DEFAULT 0,note TEXT, stopped INTEGER NOT NULL DEFAULT 0) STRICT;
CREATE TABLE room_dispatches(turn TEXT PRIMARY KEY REFERENCES turns(id),root TEXT NOT NULL REFERENCES room_discussions(root)) STRICT;
CREATE TABLE room_invitations(source TEXT NOT NULL REFERENCES turns(id),target TEXT NOT NULL,root TEXT NOT NULL REFERENCES room_discussions(root),seq INTEGER NOT NULL,round INTEGER NOT NULL,resolved TEXT REFERENCES turns(id),PRIMARY KEY(source,target)) STRICT;
CREATE TABLE room_messages(seq INTEGER PRIMARY KEY AUTOINCREMENT,root TEXT NOT NULL REFERENCES room_discussions(root),turn TEXT NOT NULL REFERENCES turns(id),kind TEXT NOT NULL,agent TEXT NOT NULL,target TEXT,text TEXT NOT NULL,UNIQUE(turn,kind,target)) STRICT;
CREATE TABLE room_stops(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE room_turns(turn TEXT PRIMARY KEY REFERENCES turns(id),root TEXT NOT NULL REFERENCES room_discussions(root),round INTEGER NOT NULL,speaker TEXT,silent TEXT,processed INTEGER NOT NULL DEFAULT 0,fallback INTEGER NOT NULL DEFAULT 0) STRICT;
CREATE TABLE room_watermarks(root TEXT NOT NULL REFERENCES room_discussions(root),agent TEXT NOT NULL,through INTEGER NOT NULL,PRIMARY KEY(root,agent)) STRICT;
CREATE TABLE routine_cron_cursors(routine TEXT PRIMARY KEY REFERENCES routines(id),occurrence_id TEXT NOT NULL,counted_at INTEGER NOT NULL) STRICT;
CREATE TABLE routine_flows(routine TEXT PRIMARY KEY REFERENCES routines(id),graph TEXT NOT NULL CHECK(json_valid(graph)),revision INTEGER NOT NULL) STRICT;
CREATE TABLE routine_occurrences(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,routine TEXT NOT NULL REFERENCES routines(id),project TEXT NOT NULL REFERENCES projects(id),due_at INTEGER NOT NULL,schedule_revision INTEGER NOT NULL,initiator_account TEXT NOT NULL REFERENCES accounts(id),assignment_revision INTEGER NOT NULL CHECK(assignment_revision>0),authority_generation TEXT NOT NULL,home_device TEXT NOT NULL REFERENCES devices(id),zone TEXT NOT NULL,zone_revision INTEGER NOT NULL,missed_count INTEGER NOT NULL CHECK(missed_count>=0),state TEXT NOT NULL CHECK(state IN('pending','admitted','done','failed','blocked')),graph_json TEXT CHECK(graph_json IS NULL OR json_valid(graph_json)),task TEXT UNIQUE REFERENCES tasks(id),observation_revision INTEGER NOT NULL DEFAULT 1 CHECK(observation_revision>0),source TEXT NOT NULL DEFAULT 'schedule' CHECK(source IN('schedule','manual','webhook')),schedule_kind TEXT NOT NULL DEFAULT 'instant' CHECK(schedule_kind IN('instant','interval','once','cron'))) STRICT;
CREATE TABLE routine_schedule_bindings(routine TEXT PRIMARY KEY REFERENCES routines(id),project TEXT NOT NULL REFERENCES projects(id),initiator_account TEXT NOT NULL REFERENCES accounts(id),assignment_revision INTEGER NOT NULL CHECK(assignment_revision>0),authority_generation TEXT NOT NULL,home_device TEXT NOT NULL REFERENCES devices(id),zone TEXT NOT NULL,zone_revision INTEGER NOT NULL CHECK(zone_revision>0),spec_json TEXT CHECK(spec_json IS NULL OR json_valid(spec_json))) STRICT;
CREATE TABLE routine_scheduler_cursors(kind TEXT PRIMARY KEY,after_id TEXT NOT NULL) STRICT;
CREATE TABLE routine_steps(occurrence TEXT NOT NULL REFERENCES routine_occurrences(id),agent TEXT NOT NULL REFERENCES agents(id),turn TEXT UNIQUE REFERENCES turns(id),state TEXT NOT NULL CHECK(state IN('waiting','queued','done','failed','skipped')),result TEXT NOT NULL,PRIMARY KEY(occurrence,agent)) STRICT;
CREATE TABLE routine_webhook_receipts(id TEXT PRIMARY KEY,routine TEXT NOT NULL REFERENCES routines(id),event_id TEXT NOT NULL,payload_digest TEXT NOT NULL CHECK(length(payload_digest)=64),received INTEGER NOT NULL,result TEXT NOT NULL CHECK(json_valid(result)),UNIQUE(routine,event_id)) STRICT;
CREATE TABLE routine_webhooks(routine TEXT PRIMARY KEY REFERENCES routines(id),revision INTEGER NOT NULL CHECK(revision>0),generation INTEGER NOT NULL CHECK(generation>0),secret_ref TEXT NOT NULL,filters TEXT NOT NULL CHECK(json_valid(filters))) STRICT;
CREATE TABLE routines(id TEXT PRIMARY KEY,agent TEXT NOT NULL REFERENCES agents(id),name TEXT NOT NULL,prompt TEXT NOT NULL,next_at INTEGER NOT NULL,interval_ms INTEGER,enabled INTEGER NOT NULL,revision INTEGER NOT NULL DEFAULT 1,trigger TEXT NOT NULL DEFAULT 'schedule',observation_revision INTEGER NOT NULL DEFAULT 1 CHECK(observation_revision>0)) STRICT;
CREATE TABLE schedules(id TEXT PRIMARY KEY,project TEXT NOT NULL,agent TEXT NOT NULL,home_device TEXT NOT NULL REFERENCES devices(id),initiator_account TEXT NOT NULL REFERENCES accounts(id),assignment_revision INTEGER NOT NULL CHECK(assignment_revision>0),authority_generation TEXT NOT NULL,kind TEXT NOT NULL CHECK(kind IN('instant','once','interval','cron','legacy_interval')),zone TEXT NOT NULL,zone_revision INTEGER NOT NULL CHECK(zone_revision>0),spec_json TEXT NOT NULL CHECK(json_valid(spec_json)),next_at INTEGER,enabled INTEGER NOT NULL CHECK(enabled IN(0,1)),revision INTEGER NOT NULL CHECK(revision>0),observation_revision INTEGER NOT NULL DEFAULT 1 CHECK(observation_revision>0),FOREIGN KEY(project,agent) REFERENCES agent_projects(project,agent)) STRICT;
CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, checksum TEXT NOT NULL, applied_at INTEGER NOT NULL) STRICT;
CREATE TABLE session_denials(issuer TEXT NOT NULL, sid TEXT NOT NULL, account TEXT NOT NULL REFERENCES accounts(id), expires_at INTEGER NOT NULL, reason TEXT NOT NULL, created INTEGER NOT NULL, PRIMARY KEY(issuer,sid)) STRICT;
CREATE TABLE shared_data(project TEXT NOT NULL REFERENCES projects(id),id INTEGER NOT NULL CHECK(id=1),revision INTEGER NOT NULL CHECK(revision>=0),image BLOB,PRIMARY KEY(project,id)) STRICT;
CREATE TABLE sidebar (id INTEGER PRIMARY KEY CHECK(id=1),revision INTEGER NOT NULL,sections TEXT NOT NULL) STRICT;
CREATE TABLE sidebar_commands (id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE site_commands (
    id TEXT PRIMARY KEY, agent TEXT NOT NULL REFERENCES agents(id), digest TEXT NOT NULL, site TEXT NOT NULL REFERENCES local_sites(id)
  ) STRICT;
CREATE TABLE skill_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE skill_files(skill TEXT NOT NULL,revision INTEGER NOT NULL,files TEXT NOT NULL,PRIMARY KEY(skill,revision),FOREIGN KEY(skill,revision) REFERENCES skill_versions(skill,revision)) STRICT;
CREATE TABLE skill_pack_versions(pack TEXT NOT NULL REFERENCES skill_packs(id),revision INTEGER NOT NULL,name TEXT NOT NULL,members TEXT NOT NULL,PRIMARY KEY(pack,revision)) STRICT;
CREATE TABLE skill_packs(id TEXT PRIMARY KEY,name TEXT NOT NULL,revision INTEGER NOT NULL) STRICT;
CREATE TABLE skill_versions(skill TEXT NOT NULL REFERENCES skills(id),revision INTEGER NOT NULL,name TEXT NOT NULL,description TEXT NOT NULL,content TEXT NOT NULL,PRIMARY KEY(skill,revision)) STRICT;
CREATE TABLE skills(id TEXT PRIMARY KEY,name TEXT NOT NULL,revision INTEGER NOT NULL) STRICT;
CREATE TABLE sync_events(
  position INTEGER PRIMARY KEY AUTOINCREMENT,
  account TEXT NOT NULL REFERENCES accounts(id),
  project TEXT NOT NULL REFERENCES projects(id),
  authorization_generation TEXT NOT NULL,
  previous_position INTEGER NOT NULL CHECK(previous_position>=0),
  record_kind TEXT NOT NULL,
  record_id TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision>0),
  operation TEXT NOT NULL CHECK(operation IN('upsert','delete','revoke')),
  body_json TEXT CHECK(body_json IS NULL OR json_valid(body_json)),
  committed_at INTEGER NOT NULL
) STRICT;
CREATE TABLE sync_heads(account TEXT NOT NULL REFERENCES accounts(id),project TEXT NOT NULL REFERENCES projects(id),event_position INTEGER NOT NULL CHECK(event_position>=0),PRIMARY KEY(account,project)) STRICT;
CREATE TABLE sync_scope_revisions(project TEXT PRIMARY KEY REFERENCES projects(id),revision INTEGER NOT NULL CHECK(revision>0)) STRICT;
CREATE TABLE task_action_resolutions(action TEXT PRIMARY KEY, task TEXT NOT NULL REFERENCES tasks(id), request_id TEXT NOT NULL UNIQUE REFERENCES receipts(request_id), actor TEXT NOT NULL REFERENCES accounts(id), resolution TEXT NOT NULL CHECK(resolution IN('known_result','not_sent','retry_with_risk')), evidence_request_id TEXT REFERENCES receipts(request_id), replacement_action_id TEXT UNIQUE, created INTEGER NOT NULL, FOREIGN KEY(action,task) REFERENCES task_actions(id,task), CHECK((resolution='known_result' AND evidence_request_id IS NOT NULL AND replacement_action_id IS NULL) OR (resolution='not_sent' AND evidence_request_id IS NOT NULL AND replacement_action_id IS NOT NULL) OR (resolution='retry_with_risk' AND evidence_request_id IS NULL AND replacement_action_id IS NOT NULL)), CHECK(replacement_action_id IS NULL OR replacement_action_id<>action)) STRICT;
CREATE TABLE task_actions(id TEXT PRIMARY KEY, task TEXT NOT NULL REFERENCES tasks(id), sequence INTEGER NOT NULL CHECK(sequence>=0), action_kind TEXT NOT NULL CHECK(action_kind IN('provider','canonical','resource','read','compute','local','browser')), authority_generation TEXT NOT NULL, device_generation INTEGER NOT NULL, executor_generation INTEGER NOT NULL, intent_digest TEXT NOT NULL, context_snapshot_id TEXT NOT NULL REFERENCES task_context_snapshots(snapshot_id), context_digest TEXT NOT NULL CHECK(length(context_digest)=64), binding_id TEXT REFERENCES provider_bindings(id), binding_revision INTEGER, grant_revision INTEGER NOT NULL, resource_intent_json TEXT CHECK(resource_intent_json IS NULL OR json_valid(resource_intent_json)), mcp_intent_json TEXT CHECK(mcp_intent_json IS NULL OR (json_valid(mcp_intent_json) AND action_kind='local')), state TEXT NOT NULL CHECK(state IN('prepared','committed','in_flight','reported','acknowledged','not_sent','uncertain')), committed_at INTEGER, result_digest TEXT, effect_status TEXT CHECK(effect_status IN('none','known','unknown')), uncertain_reason TEXT, CHECK((action_kind='provider' AND binding_id IS NOT NULL AND binding_revision IS NOT NULL AND binding_revision>0) OR (action_kind<>'provider' AND binding_id IS NULL AND binding_revision IS NULL)), UNIQUE(task,sequence), UNIQUE(id,task)) STRICT;
CREATE TABLE task_context_snapshots(task TEXT NOT NULL REFERENCES tasks(id), context_revision INTEGER NOT NULL CHECK(context_revision>0), snapshot_id TEXT NOT NULL UNIQUE, authority_generation TEXT NOT NULL, project_revision INTEGER NOT NULL, agent_revision INTEGER NOT NULL, assignment_revision INTEGER NOT NULL, capability_revision INTEGER NOT NULL, context_digest TEXT NOT NULL CHECK(length(context_digest)=64), snapshot_json TEXT NOT NULL CHECK(json_valid(snapshot_json) AND length(CAST(snapshot_json AS BLOB))<=8388608), receipt_id TEXT NOT NULL REFERENCES receipts(request_id), created INTEGER NOT NULL, PRIMARY KEY(task,context_revision)) STRICT;
CREATE TABLE task_controls(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE task_reports(action TEXT PRIMARY KEY REFERENCES task_actions(id), report_digest TEXT NOT NULL, device TEXT NOT NULL REFERENCES devices(id), journal_sequence INTEGER NOT NULL, payload_json TEXT NOT NULL CHECK(json_valid(payload_json)), result_ref TEXT REFERENCES task_results(action), receipt_id TEXT NOT NULL REFERENCES receipts(request_id), CHECK(result_ref IS NULL OR result_ref=action)) STRICT;
CREATE TABLE task_results(action TEXT PRIMARY KEY, task TEXT NOT NULL REFERENCES tasks(id), digest TEXT NOT NULL CHECK(length(digest)=64 AND digest NOT GLOB '*[^0-9a-f]*'), size INTEGER NOT NULL CHECK(size>=0 AND size<=8388608), encoding TEXT NOT NULL CHECK(encoding='canonical-json-v1'), content_json TEXT NOT NULL CHECK(json_valid(content_json)), created INTEGER NOT NULL, FOREIGN KEY(action,task) REFERENCES task_actions(id,task), CHECK(length(CAST(content_json AS BLOB))=size)) STRICT;
CREATE TABLE tasks(id TEXT PRIMARY KEY, logical_turn TEXT NOT NULL REFERENCES turns(id), predecessor_task TEXT UNIQUE REFERENCES tasks(id), project TEXT NOT NULL, agent TEXT NOT NULL, initiator_account TEXT NOT NULL REFERENCES accounts(id), home_device TEXT NOT NULL REFERENCES devices(id), assignment_revision INTEGER NOT NULL CHECK(assignment_revision>0), capability_revision INTEGER NOT NULL CHECK(capability_revision>0), capability_digest TEXT NOT NULL CHECK(length(capability_digest)=64), capability_snapshot_json TEXT NOT NULL CHECK(json_valid(capability_snapshot_json)), provider_binding_id TEXT REFERENCES provider_bindings(id), provider_binding_revision INTEGER, provider_binding_owner TEXT REFERENCES accounts(id), state TEXT NOT NULL CHECK(state IN('queued','waiting_device','waiting_resource','running','paused','approval','uncertain','completed','failed','cancelled','transferred')), payload_digest TEXT NOT NULL, payload_json TEXT NOT NULL CHECK(json_valid(payload_json)), revision INTEGER NOT NULL CHECK(revision>0), created INTEGER NOT NULL, CHECK((provider_binding_id IS NULL AND provider_binding_revision IS NULL AND provider_binding_owner IS NULL) OR (provider_binding_id IS NOT NULL AND provider_binding_revision IS NOT NULL AND provider_binding_revision>0 AND provider_binding_owner IS NOT NULL)), FOREIGN KEY(project,agent) REFERENCES agent_projects(project,agent), CHECK(predecessor_task IS NULL OR predecessor_task<>id), UNIQUE(id,logical_turn,project), FOREIGN KEY(predecessor_task,logical_turn,project) REFERENCES tasks(id,logical_turn,project)) STRICT;
CREATE TABLE thread_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE threads(id TEXT PRIMARY KEY,agent TEXT NOT NULL REFERENCES agents(id),name TEXT NOT NULL,revision INTEGER NOT NULL DEFAULT 1,archived INTEGER NOT NULL DEFAULT 0,deleted INTEGER NOT NULL DEFAULT 0) STRICT;
CREATE TABLE transcriptions(id TEXT PRIMARY KEY,file TEXT NOT NULL REFERENCES files(id),request TEXT NOT NULL,model TEXT NOT NULL,origin TEXT NOT NULL,status TEXT NOT NULL,result TEXT,created TEXT NOT NULL,usage TEXT) STRICT;
CREATE TABLE turn_step_diagnostics(
    turn TEXT NOT NULL REFERENCES turns(id), id TEXT NOT NULL, started TEXT, finished TEXT,
    arguments TEXT, result TEXT, failed INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(turn,id)
) STRICT;
CREATE TABLE turn_verification(turn TEXT PRIMARY KEY REFERENCES turns(id),nudged INTEGER NOT NULL DEFAULT 0,review TEXT,kind TEXT,stamp TEXT);
CREATE TABLE turn_waits(
  id TEXT PRIMARY KEY,turn TEXT NOT NULL REFERENCES turns(id),call_id TEXT NOT NULL,call TEXT NOT NULL,event TEXT NOT NULL,
  secret_ref TEXT NOT NULL,secret_generation INTEGER NOT NULL CHECK(secret_generation>0),source_device_id TEXT NOT NULL REFERENCES devices(id),
  deadline INTEGER NOT NULL,status TEXT NOT NULL CHECK(status IN('waiting','received','expired','closed')),
  delivery TEXT,digest TEXT,result TEXT,after_output INTEGER NOT NULL,after_action INTEGER NOT NULL,UNIQUE(turn,call_id)
) STRICT;
CREATE TABLE "turns"(seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL, agent TEXT REFERENCES agents(id), text TEXT NOT NULL, reply TEXT, status TEXT NOT NULL CHECK(status IN('queued','running','done','failed','interrupted','cancelled')), error TEXT, created TEXT NOT NULL, thread TEXT REFERENCES threads(id), channel TEXT REFERENCES channels(id), attachments TEXT NOT NULL DEFAULT '[]', recipient TEXT REFERENCES agents(id), automatic INTEGER NOT NULL DEFAULT 0, routing TEXT, paused INTEGER NOT NULL DEFAULT 0, revision INTEGER NOT NULL DEFAULT 1, profile_setup INTEGER NOT NULL DEFAULT 0, parent TEXT REFERENCES turns(id), no_update INTEGER NOT NULL DEFAULT 0 CHECK(no_update IN(0,1)), origin TEXT, message_depth INTEGER NOT NULL DEFAULT 0, waiting_for TEXT REFERENCES turns(id), started_at TEXT, stopped_at TEXT, dispatch_mode TEXT NOT NULL DEFAULT 'queue', dispatch_target TEXT, dispatch_label TEXT) STRICT;
CREATE TABLE vault_entries(
    name TEXT PRIMARY KEY, kind TEXT NOT NULL CHECK(kind IN('login','secret')),
    revision INTEGER NOT NULL, payload BLOB NOT NULL
) STRICT;
CREATE TABLE vault_grants(
    entry TEXT NOT NULL REFERENCES vault_entries(name) ON DELETE CASCADE,
    agent TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    PRIMARY KEY(entry, agent)
) STRICT;
CREATE TABLE vault_key(id INTEGER PRIMARY KEY CHECK(id=1), name TEXT NOT NULL) STRICT;
CREATE TABLE vault_requests(
    turn TEXT PRIMARY KEY REFERENCES computer_waits(turn) ON DELETE CASCADE,
    call_id TEXT NOT NULL, entry TEXT NOT NULL, origin TEXT NOT NULL, desktop TEXT NOT NULL, target TEXT NOT NULL
) STRICT;
CREATE TABLE verification_artifacts(turn TEXT NOT NULL REFERENCES turns(id),source TEXT NOT NULL,file TEXT REFERENCES files(id),kind TEXT NOT NULL,stamp TEXT NOT NULL,PRIMARY KEY(turn,source)) STRICT;
CREATE TABLE wait_provisions(
  id TEXT PRIMARY KEY,
  project TEXT NOT NULL,
  agent TEXT NOT NULL,
  source_device TEXT NOT NULL REFERENCES devices(id),
  source_generation INTEGER NOT NULL CHECK(source_generation>0),
  authority_generation TEXT NOT NULL,
  target_key TEXT NOT NULL,
  secret_generation INTEGER NOT NULL CHECK(secret_generation>0),
  payload_digest TEXT NOT NULL CHECK(length(payload_digest)=64),
  request_json TEXT NOT NULL CHECK(json_valid(request_json) AND length(CAST(request_json AS BLOB))<=65536),
  deadline INTEGER NOT NULL,
  state TEXT NOT NULL CHECK(state IN('pending','activated','closed','failed')),
  result_request_id TEXT UNIQUE REFERENCES receipts(request_id),
  UNIQUE(target_key,secret_generation),
  FOREIGN KEY(project,agent) REFERENCES agent_projects(project,agent)
) STRICT;
CREATE TABLE web_search_settings(id INTEGER PRIMARY KEY CHECK(id=1),url TEXT) STRICT;
CREATE TABLE whatsapp_chats (
    chat TEXT PRIMARY KEY,
    label TEXT NOT NULL,
    seen INTEGER NOT NULL,
    kind TEXT CHECK(kind IN ('agents','channels')),
    target TEXT,
    revision TEXT,
    transcribe_provider TEXT
) STRICT;
CREATE TABLE whatsapp_inbox (
    chat TEXT NOT NULL,
    sender TEXT NOT NULL,
    message TEXT NOT NULL,
    id TEXT NOT NULL UNIQUE,
    route TEXT NOT NULL,
    kind TEXT NOT NULL,
    target TEXT NOT NULL,
    text TEXT NOT NULL,
    quoted TEXT,
    media TEXT,
    upload TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued',
    workflow INTEGER NOT NULL DEFAULT 0,
    created INTEGER NOT NULL,
    PRIMARY KEY(chat,sender,message)
) STRICT;
CREATE TABLE whatsapp_link (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    generation TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('disconnected','pairing','linked','needs_relink')),
    started INTEGER NOT NULL,
    next_send INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE TABLE whatsapp_outbox (
    id TEXT PRIMARY KEY,
    input TEXT NOT NULL REFERENCES whatsapp_inbox(id),
    slot TEXT NOT NULL,
    text TEXT NOT NULL,
    file TEXT,
    status TEXT NOT NULL DEFAULT 'queued',
    error TEXT,
    UNIQUE(input,slot)
) STRICT;
CREATE TABLE whatsapp_prompts (
    notice TEXT PRIMARY KEY REFERENCES whatsapp_outbox(id),
    prompt_type TEXT NOT NULL,
    subject TEXT NOT NULL,
    revision INTEGER,
    choices TEXT NOT NULL,
    answered INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE TABLE whatsapp_session (
    bucket TEXT NOT NULL,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    expires INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(bucket,key)
) STRICT;
CREATE TABLE workflow_runs(id TEXT PRIMARY KEY,workflow TEXT NOT NULL REFERENCES workflows(id),version INTEGER NOT NULL,agent TEXT NOT NULL REFERENCES agents(id),graph TEXT NOT NULL,input TEXT NOT NULL,last TEXT NOT NULL,cursor TEXT NOT NULL,steps INTEGER NOT NULL DEFAULT 0,revision INTEGER NOT NULL DEFAULT 1,status TEXT NOT NULL CHECK(status IN('ready','waiting','approval','done','failed','cancelled')),error TEXT,created TEXT NOT NULL,retry_settings TEXT) STRICT;
CREATE TABLE workflow_schedule_cron_cursors(schedule TEXT PRIMARY KEY REFERENCES schedules(id),occurrence_id TEXT NOT NULL,counted_at INTEGER NOT NULL) STRICT;
CREATE TABLE workflow_schedule_targets(schedule TEXT PRIMARY KEY REFERENCES schedules(id),workflow TEXT NOT NULL UNIQUE REFERENCES workflows(id),input TEXT NOT NULL) STRICT;
CREATE TABLE workflow_steps(run TEXT NOT NULL REFERENCES workflow_runs(id),seq INTEGER NOT NULL,node TEXT NOT NULL,turn TEXT UNIQUE REFERENCES turns(id),model TEXT,status TEXT NOT NULL CHECK(status IN('waiting','approval','done','failed','cancelled')),detail TEXT,settings TEXT,PRIMARY KEY(run,seq)) STRICT;
CREATE TABLE workflow_versions(workflow TEXT NOT NULL REFERENCES workflows(id),revision INTEGER NOT NULL,graph TEXT NOT NULL,PRIMARY KEY(workflow,revision)) STRICT;
CREATE TABLE workflow_waits(run TEXT NOT NULL,seq INTEGER NOT NULL,event TEXT NOT NULL,secret_ref TEXT NOT NULL,secret_generation INTEGER NOT NULL,source_device_id TEXT NOT NULL REFERENCES devices(id),deadline INTEGER NOT NULL,status TEXT NOT NULL CHECK(status IN('waiting','received','expired','closed')),delivery TEXT,digest TEXT,result TEXT,PRIMARY KEY(run,seq),FOREIGN KEY(run,seq) REFERENCES workflow_steps(run,seq)) STRICT;
CREATE TABLE workflows(id TEXT PRIMARY KEY,agent TEXT NOT NULL REFERENCES agents(id),name TEXT NOT NULL,revision INTEGER NOT NULL,deleted INTEGER NOT NULL DEFAULT 0 CHECK(deleted IN(0,1))) STRICT;
CREATE TABLE workspace_checkpoints(turn TEXT NOT NULL,agent TEXT NOT NULL,directory TEXT NOT NULL,ref TEXT NOT NULL,hash TEXT,state TEXT NOT NULL CHECK(state IN('prepared','done','uncertain','pruned')),created INTEGER NOT NULL,PRIMARY KEY(agent,turn)) STRICT;
CREATE TABLE workspace_git_operations(turn TEXT NOT NULL,call_id TEXT NOT NULL,request TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN('prepared','done','uncertain')),result TEXT,call TEXT,after_output INTEGER NOT NULL,after_action INTEGER NOT NULL,PRIMARY KEY(turn,call_id)) STRICT;
CREATE TABLE workspace_writes(turn TEXT NOT NULL REFERENCES turns(id),call_id TEXT NOT NULL,call TEXT NOT NULL,after_output INTEGER NOT NULL,after_action INTEGER NOT NULL,PRIMARY KEY(turn,call_id)) STRICT;
CREATE INDEX agent_deliveries_request ON agent_deliveries(request);
CREATE INDEX agent_email_inbox ON agent_email_messages(agent,seq);
CREATE INDEX agent_reply_claims_consumer ON agent_reply_claims(consumer);
CREATE INDEX delegation_tasks_creator ON delegation_tasks(creator_turn);
CREATE INDEX discord_event_queue ON discord_events(status);
CREATE INDEX discord_job_queue ON discord_jobs(status);
CREATE INDEX discord_outbox_queue ON discord_outbox(status);
CREATE INDEX files_scope_seq ON files(scope,seq);
CREATE UNIQUE INDEX invitations_pending ON invitations(project,email_hash) WHERE state IN('pending_delivery','sent');
CREATE INDEX link_challenges_expiry ON link_challenges(expires_at);
CREATE UNIQUE INDEX occurrence_pending ON occurrences(schedule,schedule_revision) WHERE state IN('pending','blocked');
CREATE INDEX project_workflow_notifications_reader ON project_workflow_notifications(project,account,seq);
CREATE INDEX proof_replays_expiry ON proof_replays(expires_at);
CREATE INDEX provider_calls_turn_seq ON provider_calls(turn,seq);
CREATE UNIQUE INDEX queue_edit_active ON queue_edits(turn) WHERE state='held';
CREATE INDEX record_scope_lookup ON record_scopes(project,root_kind,root_id);
CREATE INDEX recovery_evidence_expiry ON recovery_evidence_sessions(expires_at);
CREATE INDEX recovery_quarantine_scope ON recovery_quarantine(owner_account,task,created);
CREATE INDEX remote_agent_calls_latest ON remote_agent_calls(machine,agent,created DESC,id DESC) WHERE delivery IS NULL;
CREATE INDEX room_turns_root ON room_turns(root);
CREATE INDEX routine_occurrences_active ON routine_occurrences(state,routine);
CREATE UNIQUE INDEX routine_occurrences_schedule ON routine_occurrences(routine,schedule_revision,due_at) WHERE source='schedule';
CREATE INDEX sync_events_account_project ON sync_events(account,project,position);
CREATE INDEX sync_events_project_position ON sync_events(project,position);
CREATE INDEX tasks_queue ON tasks(home_device,project,state,created);
CREATE UNIQUE INDEX tasks_root_turn ON tasks(logical_turn) WHERE predecessor_task IS NULL;
CREATE INDEX tasks_turn ON tasks(logical_turn,created,id);
CREATE INDEX turn_waits_pending ON turn_waits(turn,status);
CREATE INDEX turns_agent_status ON turns(agent,status);
CREATE INDEX turns_parent ON turns(parent);
CREATE INDEX wait_provisions_pending ON wait_provisions(state,deadline,id);
CREATE INDEX whatsapp_inbox_work ON whatsapp_inbox(status,created);
CREATE INDEX whatsapp_outbox_work ON whatsapp_outbox(status);
CREATE INDEX whatsapp_session_expiry ON whatsapp_session(bucket,expires);
CREATE TRIGGER agent_home_immutable BEFORE UPDATE OF agent,owner_account,home_device ON agent_authority WHEN NEW.agent<>OLD.agent OR NEW.owner_account<>OLD.owner_account OR NEW.home_device<>OLD.home_device BEGIN SELECT RAISE(ABORT,'agent_home_immutable'); END;
CREATE TRIGGER channel_turn_changed AFTER UPDATE ON turns WHEN NEW.channel IS NOT NULL AND(NEW.status IS NOT OLD.status OR NEW.reply IS NOT OLD.reply OR NEW.agent IS NOT OLD.agent OR NEW.revision IS NOT OLD.revision) BEGIN UPDATE channels SET activity=activity+1 WHERE id=NEW.channel; END;
CREATE TRIGGER channel_turn_created AFTER INSERT ON turns WHEN NEW.channel IS NOT NULL BEGIN UPDATE channels SET activity=activity+1 WHERE id=NEW.channel; END;
CREATE TRIGGER delivery_activity_insert AFTER INSERT ON turns WHEN NEW.thread IS NOT NULL BEGIN
      UPDATE delivery_conversations SET activity=CAST(unixepoch('subsec')*1000 AS INTEGER),settled_at=NULL WHERE thread=NEW.thread;
    END;
CREATE TRIGGER delivery_activity_update AFTER UPDATE OF status,reply,paused,text ON turns WHEN NEW.thread IS NOT NULL BEGIN
      UPDATE delivery_conversations SET activity=CAST(unixepoch('subsec')*1000 AS INTEGER),settled_at=NULL WHERE thread=NEW.thread;
    END;
CREATE TRIGGER occurrence_observation_revision AFTER UPDATE OF state,missed_count,task,run ON occurrences WHEN OLD.state IS NOT NEW.state OR OLD.missed_count IS NOT NEW.missed_count OR OLD.task IS NOT NEW.task OR OLD.run IS NOT NEW.run BEGIN UPDATE occurrences SET observation_revision=OLD.observation_revision+1 WHERE schedule=NEW.schedule AND occurrence_id=NEW.occurrence_id; END;
CREATE TRIGGER routine_observation_revision AFTER UPDATE OF revision ON routines WHEN OLD.revision IS NOT NEW.revision BEGIN UPDATE routines SET observation_revision=OLD.observation_revision+1 WHERE id=NEW.id; END;
CREATE TRIGGER routine_occurrence_observation_revision AFTER UPDATE OF task,state,missed_count ON routine_occurrences WHEN OLD.task IS NOT NEW.task OR OLD.state IS NOT NEW.state OR OLD.missed_count IS NOT NEW.missed_count BEGIN UPDATE routine_occurrences SET observation_revision=OLD.observation_revision+1 WHERE id=NEW.id; END;
CREATE TRIGGER routine_zone_observation_revision AFTER UPDATE OF zone,zone_revision ON routine_schedule_bindings WHEN OLD.zone IS NOT NEW.zone OR OLD.zone_revision IS NOT NEW.zone_revision BEGIN UPDATE routines SET observation_revision=observation_revision+1 WHERE id=NEW.routine; END;
CREATE TRIGGER schedule_observation_revision AFTER UPDATE OF revision,zone,zone_revision,next_at,enabled ON schedules WHEN OLD.revision IS NOT NEW.revision OR OLD.zone IS NOT NEW.zone OR OLD.zone_revision IS NOT NEW.zone_revision OR OLD.next_at IS NOT NEW.next_at OR OLD.enabled IS NOT NEW.enabled BEGIN UPDATE schedules SET observation_revision=OLD.observation_revision+1 WHERE id=NEW.id; END;
CREATE TRIGGER sync_scope_delete AFTER DELETE ON record_scopes BEGIN
  INSERT INTO sync_scope_revisions VALUES (OLD.project,1) ON CONFLICT(project) DO UPDATE SET revision=revision+1;
END;
CREATE TRIGGER sync_scope_update AFTER UPDATE ON record_scopes
WHEN NEW.project<>OLD.project OR NEW.account<>OLD.account OR NEW.visibility<>OLD.visibility OR NEW.root_kind<>OLD.root_kind OR NEW.root_id<>OLD.root_id OR NEW.table_name<>OLD.table_name OR NEW.key_json<>OLD.key_json
BEGIN
  INSERT INTO sync_scope_revisions VALUES (OLD.project,1) ON CONFLICT(project) DO UPDATE SET revision=revision+1;
  INSERT INTO sync_scope_revisions SELECT NEW.project,1 WHERE NEW.project<>OLD.project ON CONFLICT(project) DO UPDATE SET revision=revision+1;
END;
CREATE TRIGGER task_admission_immutable BEFORE UPDATE OF logical_turn,predecessor_task,project,agent,initiator_account,home_device,assignment_revision,capability_revision,capability_digest,capability_snapshot_json,provider_binding_id,provider_binding_revision,provider_binding_owner,payload_digest,payload_json,created ON tasks BEGIN SELECT RAISE(ABORT,'immutable task admission'); END;
CREATE TRIGGER task_context_immutable_delete BEFORE DELETE ON task_context_snapshots BEGIN SELECT RAISE(ABORT,'immutable task context'); END;
CREATE TRIGGER task_context_immutable_update BEFORE UPDATE ON task_context_snapshots BEGIN SELECT RAISE(ABORT,'immutable task context'); END;
CREATE TRIGGER task_resolution_immutable_delete BEFORE DELETE ON task_action_resolutions BEGIN SELECT RAISE(ABORT,'immutable task resolution'); END;
CREATE TRIGGER task_resolution_immutable_update BEFORE UPDATE ON task_action_resolutions BEGIN SELECT RAISE(ABORT,'immutable task resolution'); END;
CREATE TRIGGER task_results_immutable_delete BEFORE DELETE ON task_results BEGIN SELECT RAISE(ABORT,'immutable task result'); END;
CREATE TRIGGER task_results_immutable_update BEFORE UPDATE ON task_results BEGIN SELECT RAISE(ABORT,'immutable task result'); END;
CREATE TRIGGER task_successor_provenance BEFORE INSERT ON tasks WHEN NEW.predecessor_task IS NOT NULL AND NOT EXISTS(
  SELECT 1 FROM tasks previous JOIN projects project ON project.id=previous.project
  JOIN agent_authority home ON home.agent=NEW.agent AND home.owner_account=project.account
  JOIN agent_projects participation ON participation.project=project.id AND participation.agent=NEW.agent
  JOIN project_members member ON member.project=project.id AND member.account=NEW.initiator_account
  LEFT JOIN collaborator_agents assignment ON assignment.project=project.id AND assignment.account=member.account AND assignment.agent=NEW.agent
  LEFT JOIN provider_bindings binding ON binding.id=home.provider_binding
  WHERE previous.id=NEW.predecessor_task AND previous.state='transferred'
  AND previous.logical_turn=NEW.logical_turn AND previous.project=NEW.project AND previous.initiator_account=NEW.initiator_account
  AND home.home_device=NEW.home_device AND participation.deleted=0 AND member.deleted=0
  AND NEW.capability_revision=participation.revision
  AND minarbot_capability_digest(NEW.capability_snapshot_json)=minarbot_capability_digest(participation.capabilities_json)
  AND NEW.capability_digest=minarbot_capability_digest(participation.capabilities_json)
  AND NEW.assignment_revision=CASE WHEN member.role='owner' THEN member.grant_revision ELSE assignment.revision END
  AND (member.role='owner' OR assignment.enabled=1)
  AND NEW.provider_binding_id IS home.provider_binding
  AND (home.provider_binding IS NULL OR (binding.device=NEW.home_device AND binding.owner_account=project.account AND binding.revision=NEW.provider_binding_revision AND binding.owner_account=NEW.provider_binding_owner AND binding.readiness='ready'))
  AND (previous.provider_binding_owner IS NULL OR previous.provider_binding_owner=NEW.provider_binding_owner)
) BEGIN SELECT RAISE(ABORT,'invalid task successor provenance'); END;
CREATE TRIGGER turn_stopped AFTER UPDATE OF status ON turns
WHEN NEW.status='cancelled' AND OLD.status<>'cancelled' AND NEW.stopped_at IS NULL
BEGIN UPDATE turns SET stopped_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=NEW.id; END;
INSERT OR IGNORE INTO routine_scheduler_cursors VALUES('schedule',''),('flow',''),('workflow_schedule','');
INSERT INTO schema_migrations VALUES (1,'v1_owner_project','61c06b2162d013123bf8901c1f4e72c33deb6cf60b7018a8c6ac65aa0b2e417b',0);
INSERT INTO schema_migrations VALUES (2,'v2_device_authority','8be7de84c6c87fb68b0afedf22bfa59ff8e88c08aeda262eded348216e86dc7f',0);
INSERT INTO schema_migrations VALUES (3,'v3_task_fencing','f933c4c0663fa9680a4b6227e578052b31afa9c8d377bd753e1e8f0ffd0c5b1a',0);
INSERT INTO schema_migrations VALUES (4,'v4_sync_retention','9c7b5684504ef6d121413585b4b78322e9149188af0ebb1d7e7e4a354cae0a7e',0);
INSERT INTO schema_migrations VALUES (5,'v5_canonical_indexes','57a220046ffb095a17a6c987070f59f518b72d80a8f605d5e8c309fdd2dd7b52',0);
INSERT INTO schema_migrations VALUES (6,'v6_provider_accounts','e5228a7b235e9cf8176187f1313b106c1b7ae704713e4ca2ff90e4e0c99dec6f',0);
