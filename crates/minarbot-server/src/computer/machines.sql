CREATE TABLE IF NOT EXISTS environment_identity(singleton INTEGER PRIMARY KEY CHECK(singleton=1),environment_id TEXT UNIQUE NOT NULL,name TEXT NOT NULL) STRICT;
    CREATE TABLE IF NOT EXISTS pairing_codes(id TEXT PRIMARY KEY,digest TEXT UNIQUE NOT NULL,expires INTEGER NOT NULL,used INTEGER NOT NULL DEFAULT 0) STRICT;
    CREATE TABLE IF NOT EXISTS pairing_code_targets(id TEXT PRIMARY KEY REFERENCES pairing_codes(id) ON DELETE CASCADE,origin TEXT NOT NULL) STRICT;
    CREATE TABLE IF NOT EXISTS pairing_rate_limit(bucket INTEGER PRIMARY KEY,attempts INTEGER NOT NULL) STRICT;
    CREATE TABLE IF NOT EXISTS peer_pairing_pending(token_id TEXT PRIMARY KEY,environment_id TEXT UNIQUE NOT NULL,name TEXT NOT NULL,origin TEXT NOT NULL,version TEXT NOT NULL,expires INTEGER NOT NULL) STRICT;
    CREATE TABLE IF NOT EXISTS paired_machines(environment_id TEXT PRIMARY KEY,name TEXT NOT NULL,origin TEXT UNIQUE NOT NULL,token TEXT NOT NULL,local_token_id TEXT NOT NULL,return_token TEXT,state TEXT NOT NULL,status TEXT NOT NULL,version TEXT NOT NULL,last_seen INTEGER,agents TEXT NOT NULL,allow_agent_messaging INTEGER NOT NULL DEFAULT 1) STRICT;
    CREATE TABLE IF NOT EXISTS machine_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,environment_id TEXT,result TEXT) STRICT;
    CREATE TABLE IF NOT EXISTS peer_revocations(token_id TEXT PRIMARY KEY) STRICT;
CREATE TABLE IF NOT EXISTS machine_update_policy(environment_id TEXT PRIMARY KEY,allowed INTEGER NOT NULL DEFAULT 0) STRICT;
CREATE TABLE IF NOT EXISTS computer_peer_messaging(token_id TEXT PRIMARY KEY,allowed INTEGER NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS mcp_server_tokens (
    id TEXT PRIMARY KEY, token_hash TEXT NOT NULL UNIQUE, kind TEXT NOT NULL CHECK(kind IN ('session','peer')),
    name TEXT NOT NULL, agent TEXT REFERENCES agents(id), turn TEXT REFERENCES turns(id), scopes TEXT NOT NULL,
    created INTEGER NOT NULL, last_used INTEGER, revoked INTEGER, expires INTEGER, request_digest TEXT NOT NULL
  ) STRICT;
  CREATE TABLE IF NOT EXISTS mcp_server_commands(id TEXT PRIMARY KEY,digest TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS mcp_server_messages (
    caller TEXT NOT NULL REFERENCES mcp_server_tokens(id), request_id TEXT NOT NULL, digest TEXT NOT NULL,
    turn TEXT NOT NULL UNIQUE REFERENCES turns(id), result TEXT NOT NULL, PRIMARY KEY(caller,request_id)
  ) STRICT;
CREATE TABLE IF NOT EXISTS machine_provider_reports(
    environment_id TEXT PRIMARY KEY REFERENCES paired_machines(environment_id) ON DELETE CASCADE,
    report TEXT NOT NULL CHECK(json_valid(report))
) STRICT;
