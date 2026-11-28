CREATE TABLE IF NOT EXISTS agent_folders(agent TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,directory TEXT,write INTEGER NOT NULL CHECK(write IN(0,1)),revision INTEGER NOT NULL CHECK(revision>0)) STRICT;
CREATE TABLE IF NOT EXISTS agent_folder_commands(id TEXT PRIMARY KEY,agent TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS agent_visibility(agent TEXT PRIMARY KEY REFERENCES agents(id),hidden INTEGER NOT NULL DEFAULT 0,revision INTEGER NOT NULL DEFAULT 0) STRICT;
    CREATE TABLE IF NOT EXISTS agent_visibility_skips(agent TEXT NOT NULL REFERENCES agents(id),routine TEXT NOT NULL,scheduled_at INTEGER NOT NULL,missed INTEGER NOT NULL,PRIMARY KEY(agent,routine,scheduled_at)) STRICT;
    CREATE TABLE IF NOT EXISTS agent_visibility_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS agent_avatars(agent TEXT PRIMARY KEY REFERENCES agents(id),revision INTEGER NOT NULL,image BLOB) STRICT;
    CREATE TABLE IF NOT EXISTS avatar_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS delivery_conversations(
    thread TEXT PRIMARY KEY REFERENCES threads(id),sender TEXT NOT NULL,machine TEXT,activity INTEGER NOT NULL,settled_at INTEGER) STRICT;
    CREATE TABLE IF NOT EXISTS conversation_settings(id INTEGER PRIMARY KEY CHECK(id=1),auto_archive INTEGER NOT NULL CHECK(auto_archive IN (0,1))) STRICT;
    INSERT OR IGNORE INTO conversation_settings VALUES (1,1);
    CREATE TRIGGER IF NOT EXISTS delivery_activity_insert AFTER INSERT ON turns WHEN NEW.thread IS NOT NULL BEGIN
      UPDATE delivery_conversations SET activity=CAST(unixepoch('subsec')*1000 AS INTEGER),settled_at=NULL WHERE thread=NEW.thread;
    END;
    CREATE TRIGGER IF NOT EXISTS delivery_activity_update AFTER UPDATE OF status,reply,paused,text ON turns WHEN NEW.thread IS NOT NULL BEGIN
      UPDATE delivery_conversations SET activity=CAST(unixepoch('subsec')*1000 AS INTEGER),settled_at=NULL WHERE thread=NEW.thread;
    END;
CREATE TABLE IF NOT EXISTS memories(scope TEXT NOT NULL,id TEXT NOT NULL,text TEXT NOT NULL,essential INTEGER NOT NULL,revision INTEGER NOT NULL,removed INTEGER NOT NULL,PRIMARY KEY(scope,id)) STRICT;
    CREATE TABLE IF NOT EXISTS memory_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS sidebar (id INTEGER PRIMARY KEY CHECK(id=1),revision INTEGER NOT NULL,sections TEXT NOT NULL) STRICT;
    INSERT OR IGNORE INTO sidebar VALUES(1,0,'[]');
    CREATE TABLE IF NOT EXISTS sidebar_commands (id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS dispatch_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
    CREATE TABLE IF NOT EXISTS dispatch_links(id TEXT PRIMARY KEY,source_turn TEXT NOT NULL REFERENCES turns(id),target_turn TEXT NOT NULL REFERENCES turns(id),mode TEXT NOT NULL CHECK(mode IN ('steer','restart')),context TEXT NOT NULL,created TEXT NOT NULL,UNIQUE(source_turn,target_turn)) STRICT;
    CREATE TABLE IF NOT EXISTS context_transfers(id TEXT PRIMARY KEY,source_kind TEXT NOT NULL,source_id TEXT NOT NULL,cutoff_turn TEXT NOT NULL REFERENCES turns(id),target_thread TEXT NOT NULL REFERENCES threads(id),agent TEXT NOT NULL REFERENCES agents(id),transcript TEXT NOT NULL,created TEXT NOT NULL,request_id TEXT NOT NULL UNIQUE) STRICT;
    CREATE TABLE IF NOT EXISTS fork_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS agent_requests(id TEXT PRIMARY KEY,turn TEXT NOT NULL REFERENCES turns(id),call_id TEXT NOT NULL,expects_reply INTEGER NOT NULL,settled INTEGER NOT NULL DEFAULT 0,UNIQUE(turn,call_id)) STRICT;
    CREATE TABLE IF NOT EXISTS agent_deliveries(request TEXT NOT NULL REFERENCES agent_requests(id),turn TEXT PRIMARY KEY REFERENCES turns(id)) STRICT;
    CREATE TABLE IF NOT EXISTS agent_reply_captures(turn TEXT PRIMARY KEY REFERENCES turns(id)) STRICT;
    CREATE TABLE IF NOT EXISTS agent_reply_claims(delivery TEXT PRIMARY KEY REFERENCES turns(id),consumer TEXT NOT NULL REFERENCES turns(id),snapshot TEXT NOT NULL) STRICT;
    CREATE INDEX IF NOT EXISTS agent_reply_claims_consumer ON agent_reply_claims(consumer);
    CREATE INDEX IF NOT EXISTS agent_deliveries_request ON agent_deliveries(request);
    CREATE INDEX IF NOT EXISTS turns_parent ON turns(parent);
    CREATE INDEX IF NOT EXISTS turns_agent_status ON turns(agent,status);
CREATE TABLE IF NOT EXISTS delegation_tasks(id TEXT PRIMARY KEY,creator_turn TEXT NOT NULL REFERENCES turns(id),parent_task TEXT REFERENCES delegation_tasks(id),request TEXT NOT NULL REFERENCES agent_requests(id),delivery TEXT NOT NULL REFERENCES turns(id),wait INTEGER NOT NULL DEFAULT 0,wake_delivered INTEGER NOT NULL DEFAULT 0,created INTEGER NOT NULL) STRICT;
    CREATE INDEX IF NOT EXISTS delegation_tasks_creator ON delegation_tasks(creator_turn);
    CREATE TABLE IF NOT EXISTS delegation_waits(id TEXT PRIMARY KEY,turn TEXT NOT NULL REFERENCES turns(id),call_id TEXT NOT NULL,call TEXT NOT NULL,task TEXT NOT NULL REFERENCES delegation_tasks(id),deadline INTEGER NOT NULL,status TEXT NOT NULL CHECK(status IN ('waiting','received','expired','closed')),after_output INTEGER NOT NULL,after_action INTEGER NOT NULL,result TEXT,UNIQUE(turn,call_id)) STRICT;
    CREATE TABLE IF NOT EXISTS agent_message_events(id TEXT PRIMARY KEY,agent TEXT NOT NULL REFERENCES agents(id),created INTEGER NOT NULL) STRICT;
    CREATE TABLE IF NOT EXISTS agent_message_cooldowns(agent TEXT PRIMARY KEY REFERENCES agents(id),until INTEGER NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS remote_agent_calls(
    id TEXT PRIMARY KEY, machine TEXT NOT NULL, agent TEXT NOT NULL, name TEXT NOT NULL, text TEXT NOT NULL,
    depth INTEGER NOT NULL, delivery TEXT UNIQUE REFERENCES turns(id), state TEXT NOT NULL CHECK(state IN ('intent','sending','sent','result','uncertain')),
    remote_turn TEXT, next_poll INTEGER NOT NULL DEFAULT 0, attempts INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL,
    cancel_requested INTEGER NOT NULL DEFAULT 0, cancel_sent INTEGER NOT NULL DEFAULT 0, error TEXT, status TEXT NOT NULL DEFAULT 'queued', reply TEXT
  ) STRICT;
  CREATE INDEX IF NOT EXISTS remote_agent_calls_latest ON remote_agent_calls(machine,agent,created DESC,id DESC) WHERE delivery IS NULL;
  CREATE TABLE IF NOT EXISTS peer_message_events(id TEXT PRIMARY KEY,peer TEXT NOT NULL,created INTEGER NOT NULL) STRICT;
  CREATE TABLE IF NOT EXISTS peer_message_cooldowns(peer TEXT PRIMARY KEY,until INTEGER NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS room_discussions(root TEXT PRIMARY KEY REFERENCES turns(id),members TEXT NOT NULL,lead TEXT,mentioned INTEGER NOT NULL,fallback INTEGER NOT NULL DEFAULT 0,note TEXT) STRICT;
    CREATE TABLE IF NOT EXISTS room_turns(turn TEXT PRIMARY KEY REFERENCES turns(id),root TEXT NOT NULL REFERENCES room_discussions(root),round INTEGER NOT NULL,speaker TEXT,silent TEXT,processed INTEGER NOT NULL DEFAULT 0,fallback INTEGER NOT NULL DEFAULT 0) STRICT;
    CREATE TABLE IF NOT EXISTS room_stops(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
    CREATE TABLE IF NOT EXISTS room_dispatches(turn TEXT PRIMARY KEY REFERENCES turns(id),root TEXT NOT NULL REFERENCES room_discussions(root)) STRICT;
    CREATE INDEX IF NOT EXISTS room_turns_root ON room_turns(root);
    CREATE TABLE IF NOT EXISTS room_admissions(turn TEXT PRIMARY KEY REFERENCES turns(id),watermark INTEGER NOT NULL,cutoff INTEGER NOT NULL,context TEXT NOT NULL) STRICT;
    CREATE TABLE IF NOT EXISTS room_watermarks(root TEXT NOT NULL REFERENCES room_discussions(root),agent TEXT NOT NULL,through INTEGER NOT NULL,PRIMARY KEY(root,agent)) STRICT;
    CREATE TABLE IF NOT EXISTS room_invitations(source TEXT NOT NULL REFERENCES turns(id),target TEXT NOT NULL,root TEXT NOT NULL REFERENCES room_discussions(root),seq INTEGER NOT NULL,round INTEGER NOT NULL,resolved TEXT REFERENCES turns(id),PRIMARY KEY(source,target)) STRICT;
    CREATE TABLE IF NOT EXISTS room_messages(seq INTEGER PRIMARY KEY AUTOINCREMENT,root TEXT NOT NULL REFERENCES room_discussions(root),turn TEXT NOT NULL REFERENCES turns(id),kind TEXT NOT NULL,agent TEXT NOT NULL,target TEXT,text TEXT NOT NULL,UNIQUE(turn,kind,target)) STRICT;
CREATE TABLE IF NOT EXISTS local_projects(id TEXT PRIMARY KEY,name TEXT NOT NULL,description TEXT NOT NULL,revision INTEGER NOT NULL DEFAULT 1,deleted INTEGER NOT NULL DEFAULT 0,created INTEGER NOT NULL,updated INTEGER NOT NULL,channel TEXT NOT NULL) STRICT;
  CREATE TABLE IF NOT EXISTS local_project_folders(project TEXT NOT NULL REFERENCES local_projects(id),directory TEXT NOT NULL,PRIMARY KEY(project,directory)) STRICT;
  CREATE TABLE IF NOT EXISTS local_project_members(project TEXT NOT NULL REFERENCES local_projects(id),agent TEXT NOT NULL REFERENCES agents(id),write INTEGER NOT NULL CHECK(write IN(0,1)),PRIMARY KEY(project,agent)) STRICT;
  CREATE TABLE IF NOT EXISTS local_project_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
  CREATE TABLE IF NOT EXISTS local_project_folder_intents(command TEXT PRIMARY KEY,request TEXT NOT NULL,folders TEXT NOT NULL) STRICT;
  CREATE TABLE IF NOT EXISTS local_project_threads(thread TEXT PRIMARY KEY REFERENCES threads(id),project TEXT NOT NULL REFERENCES local_projects(id),folder TEXT NOT NULL) STRICT;
