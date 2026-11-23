CREATE TABLE IF NOT EXISTS hub_browser_sessions (
  digest TEXT PRIMARY KEY, host TEXT NOT NULL, project_id TEXT, account_id TEXT NOT NULL REFERENCES accounts(id),
  identity_json TEXT NOT NULL, csrf TEXT NOT NULL, expires_at INTEGER NOT NULL,
  parent_digest TEXT, generation TEXT NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS hub_browser_starts (
  state_digest TEXT PRIMARY KEY, browser_digest TEXT NOT NULL, host TEXT NOT NULL, project_id TEXT NOT NULL,
  generation TEXT NOT NULL, expires_at INTEGER NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS hub_browser_codes (
  digest TEXT PRIMARY KEY, state_digest TEXT NOT NULL UNIQUE REFERENCES hub_browser_starts(state_digest) ON DELETE CASCADE,
  parent_digest TEXT NOT NULL REFERENCES hub_browser_sessions(digest) ON DELETE CASCADE, expires_at INTEGER NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS hub_browser_denials (
  issuer TEXT NOT NULL, sid TEXT NOT NULL, PRIMARY KEY(issuer,sid)
) STRICT;
CREATE TABLE IF NOT EXISTS hub_web_requests (
  participation_id TEXT PRIMARY KEY, revision INTEGER NOT NULL, requester_account_id TEXT NOT NULL,
  provider_max_cost_micros INTEGER NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS node_link_codes (
  digest TEXT PRIMARY KEY,human_digest TEXT UNIQUE NOT NULL,candidate TEXT NOT NULL,account_id TEXT,
  expires_at INTEGER NOT NULL,next_poll INTEGER NOT NULL,consumed INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE TABLE IF NOT EXISTS node_link_sessions (
  digest TEXT PRIMARY KEY,node_id TEXT NOT NULL,account_id TEXT NOT NULL,node_generation INTEGER NOT NULL,
  authority_generation TEXT NOT NULL,expires_at INTEGER NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS node_link_profiles (account_id TEXT PRIMARY KEY,name TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS node_link_replays (digest TEXT PRIMARY KEY,expires_at INTEGER NOT NULL) STRICT;
