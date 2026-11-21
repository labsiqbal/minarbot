CREATE TABLE IF NOT EXISTS local_routine_deletions(routine TEXT PRIMARY KEY REFERENCES routines(id)) STRICT;
CREATE TABLE IF NOT EXISTS routine_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS flow_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS routine_flows(routine TEXT PRIMARY KEY REFERENCES routines(id),graph TEXT NOT NULL,revision INTEGER NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS workflow_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS workflow_waits(run TEXT NOT NULL,seq INTEGER NOT NULL,event TEXT NOT NULL,secret TEXT NOT NULL,deadline INTEGER NOT NULL,delivery TEXT,digest TEXT,result TEXT,PRIMARY KEY(run,seq),FOREIGN KEY(run,seq) REFERENCES workflow_steps(run,seq)) STRICT;
CREATE TABLE IF NOT EXISTS workflow_notifications(seq INTEGER PRIMARY KEY AUTOINCREMENT,run TEXT UNIQUE NOT NULL REFERENCES workflow_runs(id),name TEXT NOT NULL,created TEXT NOT NULL,read INTEGER NOT NULL DEFAULT 0 CHECK(read IN(0,1))) STRICT;
CREATE TABLE IF NOT EXISTS workflow_parallel_members(run TEXT NOT NULL,group_seq INTEGER NOT NULL,branch INTEGER NOT NULL,step_seq INTEGER NOT NULL,PRIMARY KEY(run,group_seq,branch),FOREIGN KEY(run,group_seq) REFERENCES workflow_steps(run,seq),FOREIGN KEY(run,step_seq) REFERENCES workflow_steps(run,seq)) STRICT;
CREATE TABLE IF NOT EXISTS webhooks(routine TEXT PRIMARY KEY REFERENCES routines(id),route TEXT UNIQUE NOT NULL,secret TEXT NOT NULL,event_name TEXT NOT NULL,filters TEXT NOT NULL,revision INTEGER NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS webhook_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS webhook_receipts(routine TEXT NOT NULL REFERENCES routines(id),route TEXT NOT NULL,delivery TEXT NOT NULL,digest TEXT NOT NULL,received INTEGER NOT NULL,result TEXT NOT NULL,PRIMARY KEY(route,delivery)) STRICT;
CREATE INDEX IF NOT EXISTS webhook_receipts_digest ON webhook_receipts(route,digest);
CREATE TABLE IF NOT EXISTS workflow_webhook_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS workflow_webhook_revocations(route TEXT PRIMARY KEY,workflow TEXT NOT NULL REFERENCES workflows(id),revision INTEGER NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS workflow_webhook_receipts(route TEXT NOT NULL,delivery TEXT NOT NULL,digest TEXT NOT NULL,workflow TEXT NOT NULL REFERENCES workflows(id),received INTEGER NOT NULL,result TEXT NOT NULL,PRIMARY KEY(route,delivery)) STRICT;
CREATE TABLE IF NOT EXISTS local_routine_reports(run TEXT PRIMARY KEY REFERENCES turns(id),name TEXT NOT NULL,agent TEXT NOT NULL REFERENCES agents(id),thread TEXT NOT NULL REFERENCES threads(id),destination TEXT NOT NULL,posted INTEGER NOT NULL DEFAULT 0,message TEXT UNIQUE REFERENCES turns(id),status TEXT) STRICT;
CREATE TABLE IF NOT EXISTS script_admissions(id TEXT PRIMARY KEY,agent TEXT NOT NULL REFERENCES agents(id),created INTEGER NOT NULL) STRICT;
CREATE INDEX IF NOT EXISTS script_admissions_agent_created ON script_admissions(agent,created);
CREATE TABLE IF NOT EXISTS local_workflow_schedule_projects(
  workflow TEXT PRIMARY KEY REFERENCES workflow_schedules(workflow) ON DELETE CASCADE,
  project TEXT NOT NULL REFERENCES local_projects(id)
) STRICT;
CREATE TABLE IF NOT EXISTS local_workflow_run_projects(
  run TEXT PRIMARY KEY REFERENCES workflow_runs(id) ON DELETE CASCADE,
  project TEXT NOT NULL REFERENCES local_projects(id)
) STRICT;
