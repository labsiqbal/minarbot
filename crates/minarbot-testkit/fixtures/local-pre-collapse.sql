-- Captured from fresh local startup at 85373b3 before collapse.
PRAGMA application_id=1296978481;
PRAGMA user_version=0;
CREATE TABLE agent_avatars(agent TEXT PRIMARY KEY REFERENCES agents(id),revision INTEGER NOT NULL,image BLOB) STRICT;
CREATE TABLE agent_computers(agent TEXT PRIMARY KEY REFERENCES agents(id),
      enabled INTEGER NOT NULL CHECK(enabled IN(0,1)),revision INTEGER NOT NULL) STRICT;
CREATE TABLE agent_creations(id TEXT PRIMARY KEY REFERENCES agents(id),request TEXT NOT NULL) STRICT;
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
CREATE TABLE agent_reply_captures(turn TEXT PRIMARY KEY REFERENCES turns(id)) STRICT;
CREATE TABLE agent_reply_claims(delivery TEXT PRIMARY KEY REFERENCES turns(id),consumer TEXT NOT NULL REFERENCES turns(id),snapshot TEXT NOT NULL) STRICT;
CREATE TABLE agent_requests(id TEXT PRIMARY KEY,turn TEXT NOT NULL REFERENCES turns(id),call_id TEXT NOT NULL,expects_reply INTEGER NOT NULL,settled INTEGER NOT NULL DEFAULT 0,UNIQUE(turn,call_id)) STRICT;
CREATE TABLE agent_skills(agent TEXT NOT NULL REFERENCES agents(id),skill TEXT NOT NULL REFERENCES skills(id),version INTEGER NOT NULL,enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),revision INTEGER NOT NULL, removed INTEGER NOT NULL DEFAULT 0 CHECK(removed IN (0,1)), pack TEXT REFERENCES skill_packs(id),PRIMARY KEY(agent,skill),FOREIGN KEY(skill,version) REFERENCES skill_versions(skill,revision)) STRICT;
CREATE TABLE agent_visibility(agent TEXT PRIMARY KEY REFERENCES agents(id),hidden INTEGER NOT NULL DEFAULT 0,revision INTEGER NOT NULL DEFAULT 0) STRICT;
CREATE TABLE agent_visibility_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE agent_visibility_skips(agent TEXT NOT NULL REFERENCES agents(id),routine TEXT NOT NULL,scheduled_at INTEGER NOT NULL,missed INTEGER NOT NULL,PRIMARY KEY(agent,routine,scheduled_at)) STRICT;
CREATE TABLE agents(id TEXT PRIMARY KEY,name TEXT NOT NULL,model TEXT NOT NULL,instructions TEXT NOT NULL, role TEXT NOT NULL DEFAULT '', revision INTEGER NOT NULL DEFAULT 1, thread_limit INTEGER NOT NULL DEFAULT 2, tool_budget INTEGER NOT NULL DEFAULT 90, backup_model TEXT NOT NULL DEFAULT '', provider_account TEXT NOT NULL DEFAULT '', runtime TEXT NOT NULL DEFAULT 'api', web_access INTEGER NOT NULL DEFAULT 1 CHECK(web_access IN(0,1))) STRICT;
CREATE TABLE avatar_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE browser_devices (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    last_seen INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT;
CREATE TABLE browser_pairing_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    required INTEGER NOT NULL CHECK (required IN (0, 1))
) STRICT;
CREATE TABLE channel_creations(id TEXT PRIMARY KEY REFERENCES channels(id),request TEXT NOT NULL) STRICT;
CREATE TABLE channel_edits(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE channel_members(channel TEXT NOT NULL REFERENCES channels(id),agent TEXT NOT NULL REFERENCES agents(id),PRIMARY KEY(channel,agent)) STRICT;
CREATE TABLE channels(id TEXT PRIMARY KEY,name TEXT NOT NULL,instructions TEXT NOT NULL,lead TEXT REFERENCES agents(id),revision INTEGER NOT NULL DEFAULT 1,archived INTEGER NOT NULL DEFAULT 0, activity INTEGER NOT NULL DEFAULT 0, read_position INTEGER NOT NULL DEFAULT 0, project TEXT REFERENCES local_projects(id)) STRICT;
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
CREATE TABLE engine_logins(id TEXT PRIMARY KEY,runtime TEXT NOT NULL CHECK(runtime IN('claude','codex')),name TEXT NOT NULL,created TEXT NOT NULL,checked_at TEXT,signed_in INTEGER CHECK(signed_in IN(0,1))) STRICT;
CREATE TABLE environment_identity(singleton INTEGER PRIMARY KEY CHECK(singleton=1),environment_id TEXT UNIQUE NOT NULL,name TEXT NOT NULL) STRICT;
CREATE TABLE files (seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL,
      scope TEXT NOT NULL, name TEXT NOT NULL, digest TEXT NOT NULL, bytes BLOB NOT NULL, created TEXT NOT NULL, deleted INTEGER NOT NULL DEFAULT 0) STRICT;
CREATE TABLE flow_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE flow_runs(root_turn TEXT PRIMARY KEY REFERENCES turns(id),routine TEXT NOT NULL REFERENCES routines(id),graph TEXT NOT NULL) STRICT;
CREATE TABLE flow_steps(root_turn TEXT NOT NULL REFERENCES flow_runs(root_turn),agent TEXT NOT NULL REFERENCES agents(id),turn TEXT UNIQUE REFERENCES turns(id),status TEXT NOT NULL CHECK(status IN('waiting','queued','done','failed','skipped')),result TEXT NOT NULL,PRIMARY KEY(root_turn,agent)) STRICT;
CREATE TABLE flow_tool_calls(seq INTEGER PRIMARY KEY AUTOINCREMENT,turn TEXT NOT NULL REFERENCES turns(id),call_id TEXT NOT NULL,call TEXT NOT NULL,result TEXT NOT NULL,after_output INTEGER NOT NULL,after_action INTEGER,UNIQUE(turn,call_id)) STRICT;
CREATE TABLE fork_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE generated_files (seq INTEGER PRIMARY KEY AUTOINCREMENT,
      turn TEXT NOT NULL REFERENCES turns(id), file TEXT NOT NULL REFERENCES files(id),
      call_id TEXT NOT NULL, call TEXT NOT NULL, after_action INTEGER NOT NULL, workspace TEXT,
      UNIQUE(turn,call_id)) STRICT;
CREATE TABLE guide_changes(
                id TEXT PRIMARY KEY, turn TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
                request TEXT NOT NULL, plan TEXT NOT NULL, digest TEXT NOT NULL,
                expires INTEGER NOT NULL, status TEXT NOT NULL DEFAULT 'pending'
                    CHECK(status IN ('pending','applying','applied','cancelled','expired','failed','unknown')),
                UNIQUE(turn,request)
            ) STRICT;
CREATE TABLE guide_screens(
                turn TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE, screen TEXT NOT NULL,
                PRIMARY KEY(turn,screen)
            ) STRICT;
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
CREATE TABLE install_rate_limit(bucket INTEGER PRIMARY KEY,attempts INTEGER NOT NULL) STRICT;
CREATE TABLE install_tokens(digest BLOB PRIMARY KEY,expires INTEGER NOT NULL,used INTEGER NOT NULL DEFAULT 0,created INTEGER NOT NULL,request_id TEXT UNIQUE NOT NULL,release TEXT NOT NULL) STRICT;
CREATE TABLE local_project_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE local_project_folder_intents(command TEXT PRIMARY KEY,request TEXT NOT NULL,folders TEXT NOT NULL) STRICT;
CREATE TABLE local_project_folders(project TEXT NOT NULL REFERENCES local_projects(id),directory TEXT NOT NULL,PRIMARY KEY(project,directory)) STRICT;
CREATE TABLE local_project_legacy_channels(channel TEXT PRIMARY KEY REFERENCES channels(id)) STRICT;
CREATE TABLE local_project_members(project TEXT NOT NULL REFERENCES local_projects(id),agent TEXT NOT NULL REFERENCES agents(id),write INTEGER NOT NULL CHECK(write IN(0,1)),PRIMARY KEY(project,agent)) STRICT;
CREATE TABLE local_project_sections(project TEXT NOT NULL REFERENCES local_projects(id),channel TEXT PRIMARY KEY REFERENCES channels(id),name TEXT NOT NULL,position INTEGER NOT NULL,archived INTEGER NOT NULL DEFAULT 0,revision INTEGER NOT NULL DEFAULT 1,UNIQUE(project,name)) STRICT;
CREATE TABLE local_project_threads(thread TEXT PRIMARY KEY REFERENCES threads(id),project TEXT NOT NULL REFERENCES local_projects(id),folder TEXT NOT NULL) STRICT;
CREATE TABLE local_projects(id TEXT PRIMARY KEY,name TEXT NOT NULL,description TEXT NOT NULL,revision INTEGER NOT NULL DEFAULT 1,deleted INTEGER NOT NULL DEFAULT 0,created INTEGER NOT NULL,updated INTEGER NOT NULL,channel TEXT NOT NULL, archived INTEGER NOT NULL DEFAULT 0, codegraph_enabled INTEGER NOT NULL DEFAULT 0 CHECK(codegraph_enabled IN(0,1)), allow_installation_apps INTEGER NOT NULL DEFAULT 0) STRICT;
CREATE TABLE local_routine_deletions(routine TEXT PRIMARY KEY REFERENCES routines(id)) STRICT;
CREATE TABLE local_routine_reports(run TEXT PRIMARY KEY REFERENCES turns(id),name TEXT NOT NULL,agent TEXT NOT NULL REFERENCES agents(id),thread TEXT NOT NULL REFERENCES threads(id),destination TEXT NOT NULL,posted INTEGER NOT NULL DEFAULT 0,message TEXT UNIQUE REFERENCES turns(id),status TEXT) STRICT;
CREATE TABLE local_sites (
    id TEXT PRIMARY KEY, agent TEXT NOT NULL REFERENCES agents(id), name TEXT NOT NULL,
    source TEXT NOT NULL, html TEXT NOT NULL, revision INTEGER NOT NULL, deleted INTEGER NOT NULL DEFAULT 0,
    updated TEXT NOT NULL
  ) STRICT;
CREATE TABLE local_workflow_run_projects(
  run TEXT PRIMARY KEY REFERENCES workflow_runs(id) ON DELETE CASCADE,
  project TEXT NOT NULL REFERENCES local_projects(id)
) STRICT;
CREATE TABLE local_workflow_schedule_projects(
  workflow TEXT PRIMARY KEY REFERENCES workflow_schedules(workflow) ON DELETE CASCADE,
  project TEXT NOT NULL REFERENCES local_projects(id)
) STRICT;
CREATE TABLE machine_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,environment_id TEXT,result TEXT) STRICT;
CREATE TABLE machine_provider_reports(
    environment_id TEXT PRIMARY KEY REFERENCES paired_machines(environment_id) ON DELETE CASCADE,
    report TEXT NOT NULL CHECK(json_valid(report))
) STRICT;
CREATE TABLE machine_update_policy(environment_id TEXT PRIMARY KEY,allowed INTEGER NOT NULL DEFAULT 0) STRICT;
CREATE TABLE mcp_calls(turn TEXT NOT NULL REFERENCES turns(id),call TEXT NOT NULL,uncertain INTEGER NOT NULL,PRIMARY KEY(turn,call)) STRICT;
CREATE TABLE mcp_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE mcp_connections(id TEXT PRIMARY KEY,name TEXT NOT NULL,url TEXT NOT NULL,token TEXT NOT NULL,tools TEXT NOT NULL,enabled INTEGER NOT NULL,revision INTEGER NOT NULL) STRICT;
CREATE TABLE mcp_feature_commands(caller TEXT NOT NULL,request_id TEXT NOT NULL,digest TEXT NOT NULL,plan TEXT,result TEXT,PRIMARY KEY(caller,request_id)) STRICT;
CREATE TABLE mcp_files(turn TEXT NOT NULL REFERENCES turns(id),file TEXT NOT NULL REFERENCES files(id),PRIMARY KEY(turn,file)) STRICT;
CREATE TABLE mcp_grants(agent TEXT NOT NULL REFERENCES agents(id),connection TEXT NOT NULL REFERENCES mcp_connections(id),revision INTEGER NOT NULL,tools TEXT NOT NULL,PRIMARY KEY(agent,connection)) STRICT;
CREATE TABLE mcp_projects(connection TEXT PRIMARY KEY REFERENCES mcp_connections(id),project TEXT NOT NULL) STRICT;
CREATE TABLE mcp_server_commands(id TEXT PRIMARY KEY,digest TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE mcp_server_messages(caller TEXT NOT NULL REFERENCES mcp_server_tokens(id),request_id TEXT NOT NULL,digest TEXT NOT NULL,turn TEXT NOT NULL UNIQUE REFERENCES turns(id),result TEXT NOT NULL,PRIMARY KEY(caller,request_id)) STRICT;
CREATE TABLE mcp_server_tokens(id TEXT PRIMARY KEY,token_hash TEXT NOT NULL UNIQUE,kind TEXT NOT NULL CHECK(kind IN ('session','peer')),name TEXT NOT NULL,agent TEXT REFERENCES agents(id),turn TEXT REFERENCES turns(id),scopes TEXT NOT NULL,created INTEGER NOT NULL,last_used INTEGER,revoked INTEGER,expires INTEGER,request_digest TEXT NOT NULL) STRICT;
CREATE TABLE mcp_test_projects(test TEXT PRIMARY KEY,project TEXT NOT NULL) STRICT;
CREATE TABLE mcp_tests(id TEXT PRIMARY KEY,digest TEXT NOT NULL,name TEXT NOT NULL,url TEXT NOT NULL,token TEXT NOT NULL,tools TEXT NOT NULL,expires INTEGER NOT NULL,connection TEXT,revision INTEGER) STRICT;
CREATE TABLE memories(scope TEXT NOT NULL,id TEXT NOT NULL,text TEXT NOT NULL,essential INTEGER NOT NULL,revision INTEGER NOT NULL,removed INTEGER NOT NULL, selection_locked INTEGER NOT NULL DEFAULT 1 CHECK(selection_locked IN(0,1)),PRIMARY KEY(scope,id)) STRICT;
CREATE TABLE memory_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE model_fallbacks(turn TEXT PRIMARY KEY REFERENCES turns(id),agent TEXT NOT NULL REFERENCES agents(id),model TEXT NOT NULL) STRICT;
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
CREATE TABLE provider_calls(seq INTEGER PRIMARY KEY AUTOINCREMENT,turn TEXT NOT NULL REFERENCES turns(id),agent TEXT NOT NULL REFERENCES agents(id),model TEXT NOT NULL,origin TEXT NOT NULL,purpose TEXT NOT NULL CHECK(purpose IN('routing','completion')),status TEXT NOT NULL CHECK(status IN('pending','received','failed','interrupted')),created TEXT NOT NULL,finished TEXT,input_tokens INTEGER,output_tokens INTEGER,total_tokens INTEGER, phase TEXT NOT NULL DEFAULT 'answer', family TEXT NOT NULL DEFAULT 'openai-compatible', account TEXT NOT NULL DEFAULT '') STRICT;
CREATE TABLE provider_stream_segments(call INTEGER NOT NULL REFERENCES provider_calls(seq),ordinal INTEGER NOT NULL,id TEXT NOT NULL,kind TEXT NOT NULL,text TEXT NOT NULL,state TEXT NOT NULL,final INTEGER NOT NULL DEFAULT 0,revision INTEGER NOT NULL,PRIMARY KEY(call,ordinal),UNIQUE(call,id)) STRICT;
CREATE TABLE provider_streams(call INTEGER PRIMARY KEY REFERENCES provider_calls(seq),text TEXT NOT NULL) STRICT;
CREATE TABLE question_answers(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE questions(id TEXT PRIMARY KEY,turn TEXT NOT NULL REFERENCES turns(id),call_id TEXT NOT NULL,call TEXT NOT NULL,prompt TEXT NOT NULL,choices TEXT NOT NULL,answer TEXT,after_output INTEGER NOT NULL,after_action INTEGER NOT NULL,UNIQUE(turn,call_id)) STRICT;
CREATE TABLE queue_edit_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE queue_edits(editor TEXT PRIMARY KEY,turn TEXT NOT NULL REFERENCES turns(id),state TEXT NOT NULL,original TEXT NOT NULL,expires_at INTEGER NOT NULL DEFAULT 0) STRICT;
CREATE TABLE queue_order_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE queue_positions(turn TEXT PRIMARY KEY REFERENCES turns(id),position INTEGER NOT NULL) STRICT;
CREATE TABLE remote_agent_calls(
    id TEXT PRIMARY KEY, machine TEXT NOT NULL, agent TEXT NOT NULL, name TEXT NOT NULL, text TEXT NOT NULL,
    depth INTEGER NOT NULL, delivery TEXT UNIQUE REFERENCES turns(id), state TEXT NOT NULL CHECK(state IN ('intent','sending','sent','result','uncertain')),
    remote_turn TEXT, next_poll INTEGER NOT NULL DEFAULT 0, attempts INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL,
    cancel_requested INTEGER NOT NULL DEFAULT 0, cancel_sent INTEGER NOT NULL DEFAULT 0, error TEXT, status TEXT NOT NULL DEFAULT 'queued', reply TEXT
  ) STRICT;
CREATE TABLE room_admissions(turn TEXT PRIMARY KEY REFERENCES turns(id),watermark INTEGER NOT NULL,cutoff INTEGER NOT NULL,context TEXT NOT NULL) STRICT;
CREATE TABLE room_discussions(root TEXT PRIMARY KEY REFERENCES turns(id),members TEXT NOT NULL,lead TEXT,mentioned INTEGER NOT NULL,fallback INTEGER NOT NULL DEFAULT 0,note TEXT, stopped INTEGER NOT NULL DEFAULT 0) STRICT;
CREATE TABLE room_dispatches(turn TEXT PRIMARY KEY REFERENCES turns(id),root TEXT NOT NULL REFERENCES room_discussions(root)) STRICT;
CREATE TABLE room_invitations(source TEXT NOT NULL REFERENCES turns(id),target TEXT NOT NULL,root TEXT NOT NULL REFERENCES room_discussions(root),seq INTEGER NOT NULL,round INTEGER NOT NULL,resolved TEXT REFERENCES turns(id),PRIMARY KEY(source,target)) STRICT;
CREATE TABLE room_messages(seq INTEGER PRIMARY KEY AUTOINCREMENT,root TEXT NOT NULL REFERENCES room_discussions(root),turn TEXT NOT NULL REFERENCES turns(id),kind TEXT NOT NULL,agent TEXT NOT NULL,target TEXT,text TEXT NOT NULL,UNIQUE(turn,kind,target)) STRICT;
CREATE TABLE room_stops(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE room_turns(turn TEXT PRIMARY KEY REFERENCES turns(id),root TEXT NOT NULL REFERENCES room_discussions(root),round INTEGER NOT NULL,speaker TEXT,silent TEXT,processed INTEGER NOT NULL DEFAULT 0,fallback INTEGER NOT NULL DEFAULT 0) STRICT;
CREATE TABLE room_watermarks(root TEXT NOT NULL REFERENCES room_discussions(root),agent TEXT NOT NULL,through INTEGER NOT NULL,PRIMARY KEY(root,agent)) STRICT;
CREATE TABLE routine_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE routine_flows(routine TEXT PRIMARY KEY REFERENCES routines(id),graph TEXT NOT NULL,revision INTEGER NOT NULL) STRICT;
CREATE TABLE routine_runs(routine TEXT NOT NULL REFERENCES routines(id),scheduled_at INTEGER,turn TEXT PRIMARY KEY REFERENCES turns(id),missed INTEGER NOT NULL,source TEXT NOT NULL DEFAULT 'scheduled',UNIQUE(routine,scheduled_at)) STRICT;
CREATE TABLE routines(id TEXT PRIMARY KEY,agent TEXT NOT NULL REFERENCES agents(id),name TEXT NOT NULL,prompt TEXT NOT NULL,next_at INTEGER NOT NULL,interval_ms INTEGER,enabled INTEGER NOT NULL,revision INTEGER NOT NULL DEFAULT 1,trigger TEXT NOT NULL DEFAULT 'schedule',observation_revision INTEGER NOT NULL DEFAULT 1 CHECK(observation_revision>0), project TEXT, report_to TEXT NOT NULL DEFAULT 'run', schedule_json TEXT, schedule_zone TEXT, webhook_source TEXT) STRICT;
CREATE TABLE script_admissions(id TEXT PRIMARY KEY,agent TEXT NOT NULL REFERENCES agents(id),created INTEGER NOT NULL) STRICT;
CREATE TABLE script_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE script_permissions(agent TEXT PRIMARY KEY REFERENCES agents(id),enabled INTEGER NOT NULL,revision INTEGER NOT NULL) STRICT;
CREATE TABLE shared_data(id INTEGER PRIMARY KEY CHECK(id=1),revision INTEGER NOT NULL,image BLOB) STRICT;
CREATE TABLE shared_data_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
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
CREATE TABLE task_controls(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE thread_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE threads(id TEXT PRIMARY KEY,agent TEXT NOT NULL REFERENCES agents(id),name TEXT NOT NULL,revision INTEGER NOT NULL DEFAULT 1,archived INTEGER NOT NULL DEFAULT 0,deleted INTEGER NOT NULL DEFAULT 0) STRICT;
CREATE TABLE transcriptions(id TEXT PRIMARY KEY,file TEXT NOT NULL REFERENCES files(id),request TEXT NOT NULL,model TEXT NOT NULL,origin TEXT NOT NULL,status TEXT NOT NULL,result TEXT,created TEXT NOT NULL,usage TEXT) STRICT;
CREATE TABLE turn_step_diagnostics(
    turn TEXT NOT NULL REFERENCES turns(id), id TEXT NOT NULL, started TEXT, finished TEXT,
    arguments TEXT, result TEXT, failed INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(turn,id)
) STRICT;
CREATE TABLE turn_verification(turn TEXT PRIMARY KEY REFERENCES turns(id),nudged INTEGER NOT NULL DEFAULT 0,review TEXT,kind TEXT,stamp TEXT);
CREATE TABLE turn_waits(id TEXT PRIMARY KEY,turn TEXT NOT NULL REFERENCES turns(id),call_id TEXT NOT NULL,call TEXT NOT NULL,event TEXT NOT NULL,secret TEXT NOT NULL,deadline INTEGER NOT NULL,status TEXT NOT NULL CHECK(status IN('waiting','received','expired','closed')),delivery TEXT,digest TEXT,result TEXT,after_output INTEGER NOT NULL,after_action INTEGER NOT NULL,UNIQUE(turn,call_id)) STRICT;
CREATE TABLE "turns"(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,agent TEXT REFERENCES agents(id),text TEXT NOT NULL,reply TEXT,status TEXT NOT NULL CHECK(status IN('queued','running','done','failed','interrupted','cancelled')),error TEXT,created TEXT NOT NULL,tool_budget_used INTEGER NOT NULL DEFAULT 0,tool_budget_stopped INTEGER NOT NULL DEFAULT 0, thread TEXT REFERENCES threads(id), channel TEXT REFERENCES channels(id), attachments TEXT NOT NULL DEFAULT '[]', parent TEXT REFERENCES turns(id), recipient TEXT REFERENCES agents(id), automatic INTEGER NOT NULL DEFAULT 0, routing TEXT, paused INTEGER NOT NULL DEFAULT 0, revision INTEGER NOT NULL DEFAULT 1, no_update INTEGER NOT NULL DEFAULT 0, waiting_for TEXT REFERENCES turns(id), origin TEXT, message_depth INTEGER NOT NULL DEFAULT 0, started_at TEXT, stopped_at TEXT, dispatch_mode TEXT NOT NULL DEFAULT 'queue', dispatch_target TEXT, dispatch_label TEXT, attention_finished_at INTEGER, workflow_run TEXT) STRICT;
CREATE TABLE update_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE update_fan_out_attempts(
            id TEXT NOT NULL REFERENCES update_commands(id),
            machine TEXT NOT NULL,
            release TEXT NOT NULL,
            version TEXT NOT NULL,
            state TEXT NOT NULL,
            bytes INTEGER NOT NULL DEFAULT 0,
            total INTEGER,
            source TEXT,
            started INTEGER NOT NULL,
            finished INTEGER,
            error_code TEXT,
            error_message TEXT,
            PRIMARY KEY(id, machine)
        ) STRICT;
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
CREATE TABLE web_search_settings(id INTEGER PRIMARY KEY CHECK(id=1),url TEXT) STRICT;
CREATE TABLE webhook_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE webhook_receipts(routine TEXT NOT NULL REFERENCES routines(id),route TEXT NOT NULL,delivery TEXT NOT NULL,digest TEXT NOT NULL,received INTEGER NOT NULL,result TEXT NOT NULL,PRIMARY KEY(route,delivery)) STRICT;
CREATE TABLE webhooks(routine TEXT PRIMARY KEY REFERENCES routines(id),route TEXT UNIQUE NOT NULL,secret TEXT NOT NULL,event_name TEXT NOT NULL,filters TEXT NOT NULL,revision INTEGER NOT NULL) STRICT;
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
CREATE TABLE workflow_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE workflow_notifications(seq INTEGER PRIMARY KEY AUTOINCREMENT,run TEXT UNIQUE NOT NULL REFERENCES workflow_runs(id),name TEXT NOT NULL,created TEXT NOT NULL,read INTEGER NOT NULL DEFAULT 0 CHECK(read IN(0,1))) STRICT;
CREATE TABLE workflow_occurrences(workflow TEXT NOT NULL REFERENCES workflows(id),scheduled_at INTEGER NOT NULL,run TEXT REFERENCES workflow_runs(id),missed INTEGER NOT NULL,reason TEXT,PRIMARY KEY(workflow,scheduled_at)) STRICT;
CREATE TABLE workflow_parallel_members(run TEXT NOT NULL,group_seq INTEGER NOT NULL,branch INTEGER NOT NULL,step_seq INTEGER NOT NULL,PRIMARY KEY(run,group_seq,branch),FOREIGN KEY(run,group_seq) REFERENCES workflow_steps(run,seq),FOREIGN KEY(run,step_seq) REFERENCES workflow_steps(run,seq)) STRICT;
CREATE TABLE workflow_runs(id TEXT PRIMARY KEY,workflow TEXT NOT NULL REFERENCES workflows(id),version INTEGER NOT NULL,agent TEXT NOT NULL REFERENCES agents(id),graph TEXT NOT NULL,input TEXT NOT NULL,last TEXT NOT NULL,cursor TEXT NOT NULL,steps INTEGER NOT NULL DEFAULT 0,revision INTEGER NOT NULL DEFAULT 1,status TEXT NOT NULL CHECK(status IN('ready','waiting','approval','done','failed','cancelled')),error TEXT,created TEXT NOT NULL,retry_settings TEXT) STRICT;
CREATE TABLE workflow_schedules(workflow TEXT PRIMARY KEY REFERENCES workflows(id),revision INTEGER NOT NULL,next_at INTEGER NOT NULL,interval_ms INTEGER,input TEXT NOT NULL,enabled INTEGER NOT NULL CHECK(enabled IN(0,1))) STRICT;
CREATE TABLE workflow_steps(run TEXT NOT NULL REFERENCES workflow_runs(id),seq INTEGER NOT NULL,node TEXT NOT NULL,turn TEXT UNIQUE REFERENCES turns(id),model TEXT,status TEXT NOT NULL CHECK(status IN('waiting','approval','done','failed','cancelled')),detail TEXT,settings TEXT,parallel_group INTEGER,parallel_branch INTEGER,PRIMARY KEY(run,seq)) STRICT;
CREATE TABLE workflow_versions(workflow TEXT NOT NULL REFERENCES workflows(id),revision INTEGER NOT NULL,graph TEXT NOT NULL,PRIMARY KEY(workflow,revision)) STRICT;
CREATE TABLE workflow_waits(run TEXT NOT NULL,seq INTEGER NOT NULL,event TEXT NOT NULL,secret TEXT NOT NULL,deadline INTEGER NOT NULL,delivery TEXT,digest TEXT,result TEXT,PRIMARY KEY(run,seq),FOREIGN KEY(run,seq) REFERENCES workflow_steps(run,seq)) STRICT;
CREATE TABLE workflow_webhook_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE workflow_webhook_receipts(route TEXT NOT NULL,delivery TEXT NOT NULL,digest TEXT NOT NULL,workflow TEXT NOT NULL REFERENCES workflows(id),received INTEGER NOT NULL,result TEXT NOT NULL,PRIMARY KEY(route,delivery)) STRICT;
CREATE TABLE workflow_webhook_revocations(route TEXT PRIMARY KEY,workflow TEXT NOT NULL REFERENCES workflows(id),revision INTEGER NOT NULL) STRICT;
CREATE TABLE workflow_webhooks(workflow TEXT PRIMARY KEY REFERENCES workflows(id),route TEXT UNIQUE NOT NULL,secret TEXT NOT NULL,revision INTEGER NOT NULL,enabled INTEGER NOT NULL CHECK(enabled IN(0,1))) STRICT;
CREATE TABLE workflows(id TEXT PRIMARY KEY,agent TEXT NOT NULL REFERENCES agents(id),name TEXT NOT NULL,revision INTEGER NOT NULL,deleted INTEGER NOT NULL DEFAULT 0 CHECK(deleted IN(0,1))) STRICT;
CREATE TABLE workspace_checkpoints(turn TEXT NOT NULL,agent TEXT NOT NULL,directory TEXT NOT NULL,ref TEXT NOT NULL,hash TEXT,state TEXT NOT NULL CHECK(state IN('prepared','done','uncertain','pruned')),created INTEGER NOT NULL,PRIMARY KEY(agent,turn)) STRICT;
CREATE TABLE workspace_git_operations(turn TEXT NOT NULL,call_id TEXT NOT NULL,request TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN('prepared','done','uncertain')),result TEXT,call TEXT,after_output INTEGER NOT NULL,after_action INTEGER NOT NULL,PRIMARY KEY(turn,call_id)) STRICT;
CREATE TABLE workspace_writes(turn TEXT NOT NULL REFERENCES turns(id),call_id TEXT NOT NULL,call TEXT NOT NULL,after_output INTEGER NOT NULL,after_action INTEGER NOT NULL,PRIMARY KEY(turn,call_id)) STRICT;
CREATE INDEX agent_deliveries_request ON agent_deliveries(request);
CREATE INDEX agent_email_inbox ON agent_email_messages(agent,seq);
CREATE INDEX agent_reply_claims_consumer ON agent_reply_claims(consumer);
CREATE INDEX attention_app_approvals ON composio_calls(turn,call_id) WHERE state='approval';
CREATE INDEX attention_computer_waits ON computer_waits(turn,call_id) WHERE state='waiting';
CREATE INDEX attention_finished ON turns(attention_finished_at DESC)
WHERE attention_finished_at IS NOT NULL;
CREATE INDEX attention_harness_approvals ON harness_approvals(turn,id) WHERE state='approval';
CREATE INDEX attention_questions ON questions(turn,id) WHERE answer IS NULL;
CREATE INDEX delegation_tasks_creator ON delegation_tasks(creator_turn);
CREATE INDEX discord_event_queue ON discord_events(status);
CREATE INDEX discord_job_queue ON discord_jobs(status);
CREATE INDEX discord_outbox_queue ON discord_outbox(status);
CREATE INDEX files_scope_seq ON files(scope,seq);
CREATE INDEX provider_calls_turn_seq ON provider_calls(turn,seq);
CREATE UNIQUE INDEX queue_edit_active ON queue_edits(turn) WHERE state='held';
CREATE INDEX remote_agent_calls_latest ON remote_agent_calls(machine,agent,created DESC,id DESC) WHERE delivery IS NULL;
CREATE INDEX room_messages_root ON room_messages(root);
CREATE INDEX room_turns_root ON room_turns(root);
CREATE INDEX room_unresolved_invitations ON room_invitations(root) WHERE resolved IS NULL;
CREATE INDEX script_admissions_agent_created ON script_admissions(agent,created);
CREATE INDEX turn_waits_pending ON turn_waits(turn,status);
CREATE INDEX turns_agent_seq ON turns(agent,seq);
CREATE INDEX turns_agent_status ON turns(agent,status);
CREATE INDEX turns_parent ON turns(parent);
CREATE INDEX turns_queued_steer ON turns(dispatch_target) WHERE status='queued' AND dispatch_mode='steer';
CREATE INDEX turns_released_routing ON turns(routing) WHERE status='queued' AND routing='released';
CREATE INDEX turns_waiting_parent ON turns(waiting_for) WHERE status='queued' AND waiting_for IS NOT NULL;
CREATE INDEX update_fan_out_by_machine
            ON update_fan_out_attempts(machine, started DESC);
CREATE INDEX webhook_receipts_digest ON webhook_receipts(route,digest);
CREATE INDEX whatsapp_inbox_work ON whatsapp_inbox(status,created);
CREATE INDEX whatsapp_outbox_work ON whatsapp_outbox(status);
CREATE INDEX whatsapp_session_expiry ON whatsapp_session(bucket,expires);
CREATE TRIGGER attention_turn_finished AFTER UPDATE OF status ON turns
WHEN NEW.status IN('done','failed') AND OLD.status NOT IN('done','failed')
BEGIN
    UPDATE turns SET attention_finished_at=unixepoch() WHERE seq=NEW.seq;
END;
CREATE TRIGGER channel_turn_changed AFTER UPDATE ON turns WHEN NEW.channel IS NOT NULL AND(NEW.status IS NOT OLD.status OR NEW.reply IS NOT OLD.reply OR NEW.agent IS NOT OLD.agent OR NEW.revision IS NOT OLD.revision) BEGIN UPDATE channels SET activity=activity+1 WHERE id=NEW.channel; END;
CREATE TRIGGER channel_turn_created AFTER INSERT ON turns WHEN NEW.channel IS NOT NULL BEGIN UPDATE channels SET activity=activity+1 WHERE id=NEW.channel; END;
CREATE TRIGGER delivery_activity_insert AFTER INSERT ON turns WHEN NEW.thread IS NOT NULL BEGIN
      UPDATE delivery_conversations SET activity=CAST(unixepoch('subsec')*1000 AS INTEGER),settled_at=NULL WHERE thread=NEW.thread;
    END;
CREATE TRIGGER delivery_activity_update AFTER UPDATE OF status,reply,paused,text ON turns WHEN NEW.thread IS NOT NULL BEGIN
      UPDATE delivery_conversations SET activity=CAST(unixepoch('subsec')*1000 AS INTEGER),settled_at=NULL WHERE thread=NEW.thread;
    END;
CREATE TRIGGER turn_stopped AFTER UPDATE OF status ON turns
WHEN NEW.status='cancelled' AND OLD.status<>'cancelled' AND NEW.stopped_at IS NULL
BEGIN UPDATE turns SET stopped_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=NEW.id; END;
CREATE VIEW conversation_attention AS
WITH requests(turn,kind,request) AS (
    SELECT turn,'question',id FROM questions WHERE answer IS NULL
    UNION ALL
    SELECT turn,'app',call_id FROM composio_calls WHERE state='approval'
    UNION ALL
    SELECT turn,'harness',id FROM harness_approvals WHERE state='approval'
    UNION ALL
    SELECT turn,'computer',call_id FROM computer_waits WHERE state='waiting'
)
SELECT coalesce(th.agent,t.agent) AS agent,t.thread,t.channel,t.seq,t.id AS turn,
       coalesce(r.root,t.id) AS focus,requests.kind,requests.request
FROM requests
JOIN turns t ON t.id=requests.turn
LEFT JOIN threads th ON th.id=t.thread
LEFT JOIN channels c ON c.id=t.channel
LEFT JOIN room_turns r ON r.turn=t.id
LEFT JOIN agent_visibility v ON v.agent=coalesce(th.agent,t.agent)
WHERE coalesce(th.archived,0)=0 AND coalesce(th.deleted,0)=0 AND coalesce(c.archived,0)=0
  AND (t.channel IS NOT NULL OR coalesce(v.hidden,0)=0)
  AND CASE requests.kind
    WHEN 'question' THEN t.paused=1 AND t.status IN('queued','interrupted')
    WHEN 'app' THEN t.status='queued'
    WHEN 'harness' THEN t.status='running'
    WHEN 'computer' THEN t.status NOT IN('done','cancelled')
  END;
