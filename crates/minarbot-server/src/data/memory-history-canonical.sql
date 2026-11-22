-- Minarbot adaptation of Hindsight v1.2.3, bd230e8bb40e768bf69f266f1dd3c9ae03e22fe3.
-- Canonical history is banked by verified record scope.
CREATE VIEW IF NOT EXISTS memory_history_scope AS
SELECT t.id, CASE
    WHEN s.root_kind='channel' AND s.visibility='project'
      THEN printf('project:%s/channels:%s',s.project,s.root_id)
    ELSE printf('project:%s/agents:%s/human:%s',s.project,coalesce(th.agent,t.agent),s.account)
    END AS bank
FROM turns t JOIN record_scopes s ON s.table_name='turns' AND s.key_json=json_array(t.id)
LEFT JOIN threads th ON t.thread=th.id
WHERE t.profile_setup=0 AND (s.visibility='project' AND s.root_kind='channel' OR coalesce(th.agent,t.agent) IS NOT NULL);

CREATE TRIGGER IF NOT EXISTS memory_scope_insert AFTER INSERT ON record_scopes WHEN NEW.table_name='turns' BEGIN
    DELETE FROM memory_units WHERE origin IN ('prompt','reply') AND source_id=json_extract(NEW.key_json,'$[0]')
      AND NOT EXISTS (SELECT 1 FROM memory_history_rows h WHERE h.source_id=memory_units.source_id
                      AND h.bank=memory_units.bank AND h.origin=memory_units.origin);
    INSERT INTO memory_import SELECT * FROM memory_history_rows WHERE source_id=json_extract(NEW.key_json,'$[0]');
END;
CREATE TRIGGER IF NOT EXISTS memory_scope_update AFTER UPDATE ON record_scopes
WHEN OLD.table_name='turns' OR NEW.table_name='turns' BEGIN
    DELETE FROM memory_units WHERE origin IN ('prompt','reply') AND source_id=json_extract(OLD.key_json,'$[0]')
      AND OLD.table_name='turns';
    INSERT INTO memory_import SELECT * FROM memory_history_rows WHERE source_id=json_extract(NEW.key_json,'$[0]')
      AND NEW.table_name='turns';
END;
CREATE TRIGGER IF NOT EXISTS memory_scope_delete AFTER DELETE ON record_scopes WHEN OLD.table_name='turns' BEGIN
    DELETE FROM memory_units WHERE origin IN ('prompt','reply') AND source_id=json_extract(OLD.key_json,'$[0]');
END;
