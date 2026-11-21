CREATE TABLE IF NOT EXISTS whatsapp_link (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    generation TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('disconnected','pairing','linked','needs_relink')),
    started INTEGER NOT NULL,
    next_send INTEGER NOT NULL DEFAULT 0
) STRICT;
INSERT OR IGNORE INTO whatsapp_link VALUES (1,'','disconnected',0,0);

CREATE TABLE IF NOT EXISTS whatsapp_session (
    bucket TEXT NOT NULL,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    expires INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(bucket,key)
) STRICT;
CREATE INDEX IF NOT EXISTS whatsapp_session_expiry ON whatsapp_session(bucket,expires);

CREATE TABLE IF NOT EXISTS whatsapp_chats (
    chat TEXT PRIMARY KEY,
    label TEXT NOT NULL,
    seen INTEGER NOT NULL,
    kind TEXT CHECK(kind IN ('agents','channels')),
    target TEXT,
    revision TEXT,
    transcribe_provider TEXT
) STRICT;

CREATE TABLE IF NOT EXISTS whatsapp_inbox (
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
CREATE INDEX IF NOT EXISTS whatsapp_inbox_work ON whatsapp_inbox(status,created);

CREATE TABLE IF NOT EXISTS whatsapp_outbox (
    id TEXT PRIMARY KEY,
    input TEXT NOT NULL REFERENCES whatsapp_inbox(id),
    slot TEXT NOT NULL,
    text TEXT NOT NULL,
    file TEXT,
    status TEXT NOT NULL DEFAULT 'queued',
    error TEXT,
    UNIQUE(input,slot)
) STRICT;
CREATE INDEX IF NOT EXISTS whatsapp_outbox_work ON whatsapp_outbox(status);

CREATE TABLE IF NOT EXISTS whatsapp_prompts (
    notice TEXT PRIMARY KEY REFERENCES whatsapp_outbox(id),
    prompt_type TEXT NOT NULL,
    subject TEXT NOT NULL,
    revision INTEGER,
    choices TEXT NOT NULL,
    answered INTEGER NOT NULL DEFAULT 0
) STRICT;
