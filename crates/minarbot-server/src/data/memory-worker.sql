-- Replace the earlier worker triggers from unreleased builds.
DROP TRIGGER IF EXISTS memory_extract_insert;
DROP TRIGGER IF EXISTS memory_extract_turn;
DROP VIEW IF EXISTS memory_extractable_turns;

-- Mark completion events after installation. Reindexing an old reply or
-- restoring an archived thread does not enroll any historical turn.
CREATE TABLE IF NOT EXISTS memory_learning_turns (
    turn TEXT PRIMARY KEY REFERENCES turns(id) ON DELETE CASCADE,
    agent TEXT NOT NULL
) STRICT;
CREATE VIEW IF NOT EXISTS memory_learning_candidates AS
SELECT t.id,t.agent,coalesce(nullif(a.memory_model,''),a.model) AS model
FROM turns t JOIN agents a ON a.id=t.agent
WHERE t.status='done' AND a.learn_from_conversations=1
  AND t.parent IS NULL AND t.agent<>'00000000-0000-4000-8000-000000000005'
  AND t.id<>t.agent
  AND coalesce(t.routing,'') NOT IN ('clarify','none')
  AND NOT EXISTS(SELECT 1 FROM hub_link_turns WHERE turn_id=t.id);
CREATE VIEW IF NOT EXISTS memory_learning_ready AS
SELECT c.id,c.agent,c.model FROM memory_learning_candidates c
JOIN memory_learning_turns e ON e.turn=c.id AND e.agent=c.agent;

CREATE TABLE IF NOT EXISTS memory_recall_metrics (
    turn TEXT PRIMARY KEY REFERENCES turns(id) ON DELETE CASCADE,
    bank TEXT NOT NULL,
    added_tokens INTEGER NOT NULL CHECK(added_tokens >= 0),
    units INTEGER NOT NULL CHECK(units >= 0)
) STRICT;

CREATE TRIGGER IF NOT EXISTS memory_learning_raw AFTER INSERT ON memory_units
WHEN NEW.origin IN ('prompt','reply')
  AND EXISTS(SELECT 1 FROM memory_learning_ready
    WHERE id=NEW.source_id AND agent=json_extract(NEW.source,'$.agent'))
BEGIN
    INSERT INTO memory_extraction(raw) VALUES(NEW.id) ON CONFLICT DO NOTHING;
END;
CREATE TRIGGER IF NOT EXISTS memory_learning_done AFTER UPDATE OF status ON turns
WHEN OLD.status<>'done' AND NEW.status='done'
  AND EXISTS(SELECT 1 FROM memory_learning_candidates WHERE id=NEW.id)
BEGIN
    INSERT INTO memory_learning_turns VALUES(NEW.id,NEW.agent) ON CONFLICT DO NOTHING;
    INSERT INTO memory_extraction(raw) SELECT id FROM memory_units
      WHERE origin IN ('prompt','reply') AND source_id=NEW.id AND removed=0
        AND json_extract(source,'$.agent')=NEW.agent
      ON CONFLICT DO NOTHING;
END;
