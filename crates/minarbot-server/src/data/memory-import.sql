-- Minarbot adaptation of Hindsight v1.2.3, bd230e8bb40e768bf69f266f1dd3c9ae03e22fe3.
-- Keep existing write paths and receipts. Mirror into the recall store in the same transaction.
CREATE VIEW IF NOT EXISTS memory_notes AS
SELECT bank AS scope,source_id AS id,text,removed,
       json_extract(metadata,'$.essential') AS essential,
       json_extract(metadata,'$.revision') AS revision,
       json_extract(metadata,'$.selection_locked') AS selection_locked
FROM memory_units WHERE origin='legacy';

CREATE VIEW IF NOT EXISTS memory_import AS
SELECT bank,origin,source_id,kind,text,occurred_at,mentioned_at,source,metadata,removed
FROM memory_units WHERE false;

CREATE TRIGGER IF NOT EXISTS memory_import_write INSTEAD OF INSERT ON memory_import BEGIN
    INSERT INTO memory_banks VALUES (NEW.bank) ON CONFLICT DO NOTHING;
    INSERT INTO memory_units(bank,origin,source_id,kind,text,occurred_at,mentioned_at,source,metadata,removed)
    VALUES(NEW.bank,NEW.origin,NEW.source_id,NEW.kind,NEW.text,NEW.occurred_at,NEW.mentioned_at,NEW.source,NEW.metadata,NEW.removed)
    ON CONFLICT(bank,origin,source_id) DO UPDATE SET
        embedding=CASE WHEN excluded.text IS memory_units.text AND excluded.removed=0 THEN memory_units.embedding END,
        text=excluded.text,occurred_at=excluded.occurred_at,mentioned_at=excluded.mentioned_at,
        source=excluded.source,metadata=excluded.metadata,removed=excluded.removed;
END;

CREATE VIEW IF NOT EXISTS memory_legacy_rows AS
SELECT scope AS bank,'legacy' AS origin,id AS source_id,'world' AS kind,text,
       NULL AS occurred_at,NULL AS mentioned_at,json_object('kind','note','id',id) AS source,
       json_object('essential',essential,'revision',revision,'selection_locked',selection_locked) AS metadata,
       removed
FROM memories;

CREATE VIEW IF NOT EXISTS memory_history_rows AS
SELECT scopes.bank,role.origin,t.id AS source_id,'experience' AS kind,
       CASE role.origin WHEN 'prompt' THEN t.text ELSE t.reply END AS text,
       CAST(unixepoch(t.created,'subsec')*1000 AS INTEGER) AS occurred_at,
       CAST(unixepoch(t.created,'subsec')*1000 AS INTEGER) AS mentioned_at,
       json_object('kind','turn','id',t.id,'role',role.origin,'agent',t.agent,
                   'thread',t.thread,'channel',t.channel,'created',t.created) AS source,
       '{}' AS metadata,0 AS removed
FROM turns t JOIN memory_history_scope scopes ON scopes.id=t.id
LEFT JOIN threads th ON th.id=t.thread CROSS JOIN (SELECT 'prompt' AS origin UNION ALL SELECT 'reply') role
WHERE coalesce(th.deleted,0)=0 AND trim(CASE role.origin WHEN 'prompt' THEN t.text ELSE t.reply END)<>'';

CREATE TRIGGER IF NOT EXISTS memory_note_insert AFTER INSERT ON memories BEGIN
    INSERT INTO memory_import SELECT * FROM memory_legacy_rows WHERE bank=NEW.scope AND source_id=NEW.id;
END;
CREATE TRIGGER IF NOT EXISTS memory_note_update AFTER UPDATE ON memories BEGIN
    DELETE FROM memory_units WHERE bank=OLD.scope AND origin='legacy' AND source_id=OLD.id
      AND (OLD.scope IS NOT NEW.scope OR OLD.id IS NOT NEW.id);
    INSERT INTO memory_import SELECT * FROM memory_legacy_rows WHERE bank=NEW.scope AND source_id=NEW.id;
END;
CREATE TRIGGER IF NOT EXISTS memory_note_delete AFTER DELETE ON memories BEGIN
    DELETE FROM memory_units WHERE bank=OLD.scope AND origin='legacy' AND source_id=OLD.id;
END;

CREATE TRIGGER IF NOT EXISTS memory_turn_insert AFTER INSERT ON turns BEGIN
    INSERT INTO memory_import SELECT * FROM memory_history_rows WHERE source_id=NEW.id;
END;
CREATE TRIGGER IF NOT EXISTS memory_turn_update AFTER UPDATE OF text,reply,agent,thread,channel,created ON turns
WHEN OLD.text IS NOT NEW.text OR OLD.reply IS NOT NEW.reply OR OLD.agent IS NOT NEW.agent
  OR OLD.thread IS NOT NEW.thread OR OLD.channel IS NOT NEW.channel OR OLD.created IS NOT NEW.created BEGIN
    DELETE FROM memory_units WHERE origin IN ('prompt','reply') AND source_id=OLD.id
      AND NOT EXISTS (SELECT 1 FROM memory_history_rows h WHERE h.source_id=NEW.id
                      AND h.bank=memory_units.bank AND h.origin=memory_units.origin);
    INSERT INTO memory_import SELECT * FROM memory_history_rows WHERE source_id=NEW.id;
END;
CREATE TRIGGER IF NOT EXISTS memory_turn_delete AFTER DELETE ON turns BEGIN
    DELETE FROM memory_units WHERE origin IN ('prompt','reply') AND source_id=OLD.id;
END;
CREATE TRIGGER IF NOT EXISTS memory_thread_visibility AFTER UPDATE OF deleted,agent ON threads
WHEN NEW.deleted IS NOT OLD.deleted OR NEW.agent IS NOT OLD.agent BEGIN
    DELETE FROM memory_units WHERE origin IN ('prompt','reply')
      AND source_id IN (SELECT id FROM turns WHERE thread=OLD.id);
    INSERT INTO memory_import SELECT * FROM memory_history_rows
      WHERE source_id IN (SELECT id FROM turns WHERE thread=NEW.id);
END;
