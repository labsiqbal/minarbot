-- New local conversation messages enter the queue when the turn settles.
-- Both trigger orders are valid: new reply units can arrive before or after
-- the completion trigger. Phase 1 import runs before these triggers exist.
CREATE VIEW IF NOT EXISTS memory_extractable_turns AS
SELECT t.id FROM turns t WHERE t.status='done'
  AND t.parent IS NULL AND t.agent<>'00000000-0000-4000-8000-000000000005'
  AND t.id<>t.agent
  AND coalesce(t.routing,'') NOT IN ('clarify','none')
  AND NOT EXISTS(SELECT 1 FROM hub_link_turns WHERE turn_id=t.id);

CREATE TRIGGER IF NOT EXISTS memory_extract_insert AFTER INSERT ON memory_units
WHEN NEW.origin IN ('prompt','reply') AND EXISTS(SELECT 1 FROM memory_extractable_turns WHERE id=NEW.source_id)
BEGIN
    INSERT INTO memory_extraction(raw) VALUES(NEW.id) ON CONFLICT DO NOTHING;
END;
CREATE TRIGGER IF NOT EXISTS memory_extract_turn AFTER UPDATE OF status ON turns
WHEN OLD.status<>'done' AND NEW.status='done'
  AND EXISTS(SELECT 1 FROM memory_extractable_turns WHERE id=NEW.id)
BEGIN
    INSERT INTO memory_extraction(raw) SELECT id FROM memory_units
    WHERE origin IN ('prompt','reply') AND source_id=NEW.id AND removed=0
    ON CONFLICT DO NOTHING;
END;
