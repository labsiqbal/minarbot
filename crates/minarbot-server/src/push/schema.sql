CREATE TABLE IF NOT EXISTS push_devices(id TEXT PRIMARY KEY,platform TEXT NOT NULL CHECK(platform IN('ios','android')),token TEXT UNIQUE NOT NULL,name TEXT NOT NULL,app_id TEXT NOT NULL,environment TEXT NOT NULL CHECK(environment IN('production','development')),created TEXT NOT NULL,updated TEXT NOT NULL,last_sent TEXT,last_error TEXT) STRICT;
CREATE TABLE IF NOT EXISTS push_events(key TEXT PRIMARY KEY,created INTEGER NOT NULL) STRICT;
CREATE INDEX IF NOT EXISTS push_events_created ON push_events(created);
CREATE TABLE IF NOT EXISTS push_outbox(seq INTEGER PRIMARY KEY AUTOINCREMENT,event TEXT NOT NULL,device TEXT NOT NULL REFERENCES push_devices(id) ON DELETE CASCADE,payload TEXT NOT NULL,attempts INTEGER NOT NULL DEFAULT 0,next_at INTEGER NOT NULL,UNIQUE(event,device)) STRICT;
CREATE INDEX IF NOT EXISTS push_outbox_device ON push_outbox(device);
CREATE TABLE IF NOT EXISTS push_workflow_changes(seq INTEGER PRIMARY KEY AUTOINCREMENT,run TEXT NOT NULL,status TEXT NOT NULL,steps INTEGER NOT NULL) STRICT;
CREATE TRIGGER IF NOT EXISTS push_workflow_status AFTER UPDATE OF status ON workflow_runs
WHEN NEW.status IN('approval','failed','done') AND OLD.status IS NOT NEW.status
BEGIN
    INSERT INTO push_workflow_changes(run,status,steps) VALUES(NEW.id,NEW.status,NEW.steps);
END;
