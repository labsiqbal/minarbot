CREATE TABLE IF NOT EXISTS discord_bot(
    singleton INTEGER PRIMARY KEY CHECK(singleton=1), generation TEXT NOT NULL,
    bot TEXT, ciphertext TEXT, status TEXT NOT NULL
) STRICT;
INSERT OR IGNORE INTO discord_bot VALUES (1,'',NULL,NULL,'disconnected');

CREATE TABLE IF NOT EXISTS discord_routes(
    id TEXT PRIMARY KEY, channel TEXT NOT NULL UNIQUE, kind TEXT NOT NULL,
    target TEXT NOT NULL, users TEXT NOT NULL, thread TEXT NOT NULL UNIQUE
) STRICT;
CREATE TABLE IF NOT EXISTS discord_events(
    message TEXT PRIMARY KEY, route TEXT NOT NULL, sender TEXT NOT NULL,
    text TEXT NOT NULL, attachments TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'queued'
) STRICT;
CREATE INDEX IF NOT EXISTS discord_event_queue ON discord_events(status);
CREATE TABLE IF NOT EXISTS discord_jobs(
    id TEXT PRIMARY KEY, message TEXT NOT NULL REFERENCES discord_events(message),
    kind TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'running'
) STRICT;
CREATE INDEX IF NOT EXISTS discord_job_queue ON discord_jobs(status);
CREATE TABLE IF NOT EXISTS discord_outbox(
    id TEXT PRIMARY KEY, route TEXT NOT NULL, reply_to TEXT, text TEXT NOT NULL,
    files TEXT NOT NULL, buttons TEXT, status TEXT NOT NULL DEFAULT 'queued',
    part INTEGER NOT NULL DEFAULT 0, total INTEGER NOT NULL, remote TEXT, error TEXT,
    workflow_output INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX IF NOT EXISTS discord_outbox_queue ON discord_outbox(status);
CREATE TABLE IF NOT EXISTS discord_choices(
    notice TEXT PRIMARY KEY, job TEXT NOT NULL REFERENCES discord_jobs(id),
    subject TEXT NOT NULL, revision INTEGER NOT NULL, kind TEXT NOT NULL,
    options TEXT NOT NULL, decided INTEGER NOT NULL DEFAULT 0,
    UNIQUE(subject, revision, kind)
) STRICT;
UPDATE discord_outbox SET status='uncertain',error='Send interrupted. Last part may have reached Discord.' WHERE status='sending';
UPDATE discord_bot SET status='connecting' WHERE ciphertext IS NOT NULL;
