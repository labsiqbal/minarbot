-- Hindsight v1.2.3, bd230e8bb40e768bf69f266f1dd3c9ae03e22fe3.
-- SQLite adaptation of hindsight-api-slim/hindsight_api/alembic/versions/5a366d414dce_initial_schema.py.
CREATE TABLE IF NOT EXISTS memory_banks (id TEXT PRIMARY KEY) STRICT;
CREATE TABLE IF NOT EXISTS memory_units (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    bank TEXT NOT NULL REFERENCES memory_banks(id),
    origin TEXT NOT NULL CHECK (origin IN ('retained','legacy','prompt','reply')),
    source_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('world','experience','observation')),
    text TEXT NOT NULL,
    occurred_at INTEGER,
    mentioned_at INTEGER,
    source TEXT NOT NULL CHECK (json_valid(source)),
    metadata TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata)),
    removed INTEGER NOT NULL DEFAULT 0,
    embedding BLOB CHECK (embedding IS NULL OR length(embedding)=1536),
    UNIQUE (bank,origin,source_id),
    UNIQUE (bank,id)
) STRICT;
CREATE INDEX IF NOT EXISTS memory_units_time
    ON memory_units(bank,occurred_at DESC,id) WHERE removed=0;
CREATE INDEX IF NOT EXISTS memory_pending
    ON memory_units(bank,id) WHERE removed=0 AND embedding IS NULL;
CREATE INDEX IF NOT EXISTS memory_source ON memory_units(origin,source_id);

CREATE TABLE IF NOT EXISTS memory_unit_entities (
    bank TEXT NOT NULL,
    unit INTEGER NOT NULL,
    entity TEXT NOT NULL,
    PRIMARY KEY (bank,unit,entity),
    FOREIGN KEY (bank,unit) REFERENCES memory_units(bank,id) ON DELETE CASCADE
) STRICT;
CREATE INDEX IF NOT EXISTS memory_entity_units ON memory_unit_entities(bank,entity,unit);

CREATE TABLE IF NOT EXISTS memory_links (
    bank TEXT NOT NULL,
    source INTEGER NOT NULL,
    target INTEGER NOT NULL CHECK (target<>source),
    kind TEXT NOT NULL CHECK (kind IN ('entity','semantic','temporal')),
    PRIMARY KEY (bank,source,target,kind),
    FOREIGN KEY (bank,source) REFERENCES memory_units(bank,id) ON DELETE CASCADE,
    FOREIGN KEY (bank,target) REFERENCES memory_units(bank,id) ON DELETE CASCADE
) STRICT;
CREATE INDEX IF NOT EXISTS memory_links_target ON memory_links(bank,target,source);

CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(text, content='', tokenize='unicode61');
CREATE TRIGGER IF NOT EXISTS memory_fts_insert AFTER INSERT ON memory_units WHEN NEW.removed=0 BEGIN
    INSERT INTO memory_fts(rowid,text) VALUES(NEW.id,NEW.text);
END;
CREATE TRIGGER IF NOT EXISTS memory_fts_delete AFTER DELETE ON memory_units WHEN OLD.removed=0 BEGIN
    INSERT INTO memory_fts(memory_fts,rowid,text) VALUES('delete',OLD.id,OLD.text);
END;
CREATE TRIGGER IF NOT EXISTS memory_fts_update AFTER UPDATE OF text,removed ON memory_units
WHEN NEW.text IS NOT OLD.text OR NEW.removed IS NOT OLD.removed BEGIN
    INSERT INTO memory_fts(memory_fts,rowid,text) SELECT 'delete',OLD.id,OLD.text WHERE OLD.removed=0;
    INSERT INTO memory_fts(rowid,text) SELECT NEW.id,NEW.text WHERE NEW.removed=0;
END;
