-- Hindsight v1.2.3, bd230e8bb40e768bf69f266f1dd3c9ae03e22fe3.
-- SQLite adaptation of engine/retain/fact_storage.py and engine/memories/pg/consolidation.py.
CREATE TABLE IF NOT EXISTS memory_extraction (
    raw INTEGER PRIMARY KEY REFERENCES memory_units(id) ON DELETE CASCADE,
    generation INTEGER NOT NULL DEFAULT 0,
    offset INTEGER NOT NULL DEFAULT 0,
    done INTEGER NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0,
    ready_at INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX IF NOT EXISTS memory_extraction_ready ON memory_extraction(ready_at,raw)
    WHERE done=0 AND attempts<3;

CREATE TABLE IF NOT EXISTS memory_facts (
    unit INTEGER PRIMARY KEY REFERENCES memory_units(id) ON DELETE CASCADE,
    raw INTEGER NOT NULL REFERENCES memory_units(id) ON DELETE CASCADE,
    offset INTEGER NOT NULL,
    consolidated INTEGER NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0,
    ready_at INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX IF NOT EXISTS memory_facts_raw ON memory_facts(raw,offset,unit);
CREATE INDEX IF NOT EXISTS memory_facts_ready ON memory_facts(ready_at,unit)
    WHERE consolidated=0 AND attempts<3;

CREATE TABLE IF NOT EXISTS memory_observations (
    unit INTEGER PRIMARY KEY REFERENCES memory_units(id) ON DELETE CASCADE,
    bank TEXT NOT NULL,
    agent TEXT NOT NULL,
    normalized TEXT NOT NULL,
    UNIQUE (bank,agent,normalized),
    FOREIGN KEY (bank,unit) REFERENCES memory_units(bank,id) ON DELETE CASCADE
) STRICT;
CREATE INDEX IF NOT EXISTS memory_observations_recent ON memory_observations(bank,agent,unit DESC);
CREATE TABLE IF NOT EXISTS memory_observation_evidence (
    observation INTEGER NOT NULL REFERENCES memory_observations(unit) ON DELETE CASCADE,
    fact INTEGER NOT NULL REFERENCES memory_facts(unit) ON DELETE CASCADE,
    PRIMARY KEY (observation,fact)
) STRICT;
CREATE INDEX IF NOT EXISTS memory_evidence_fact ON memory_observation_evidence(fact,observation);

-- Rebuild affected observations from surviving evidence on a source edit or deletion.
CREATE TRIGGER IF NOT EXISTS memory_fact_delete BEFORE DELETE ON memory_units
WHEN EXISTS (SELECT 1 FROM memory_facts WHERE unit=OLD.id) BEGIN
    UPDATE memory_facts SET consolidated=0,attempts=0,ready_at=0
    WHERE unit<>OLD.id AND unit IN (
        SELECT e.fact FROM memory_observation_evidence e
        JOIN memory_observation_evidence old ON old.observation=e.observation WHERE old.fact=OLD.id
    );
    DELETE FROM memory_units WHERE id IN (
        SELECT observation FROM memory_observation_evidence WHERE fact=OLD.id
    );
END;
CREATE TRIGGER IF NOT EXISTS memory_raw_delete BEFORE DELETE ON memory_units
WHEN EXISTS (SELECT 1 FROM memory_extraction WHERE raw=OLD.id) BEGIN
    DELETE FROM memory_units WHERE id IN (SELECT unit FROM memory_facts WHERE raw=OLD.id);
END;
CREATE TRIGGER IF NOT EXISTS memory_raw_change AFTER UPDATE OF text,source,occurred_at,mentioned_at,removed ON memory_units
WHEN (NEW.text IS NOT OLD.text OR NEW.source IS NOT OLD.source OR NEW.removed IS NOT OLD.removed
      OR NEW.occurred_at IS NOT OLD.occurred_at OR NEW.mentioned_at IS NOT OLD.mentioned_at)
  AND EXISTS (SELECT 1 FROM memory_extraction WHERE raw=OLD.id) BEGIN
    DELETE FROM memory_units WHERE id IN (SELECT unit FROM memory_facts WHERE raw=OLD.id);
    UPDATE memory_extraction SET generation=generation+1,offset=0,done=0,attempts=0,ready_at=0
      WHERE raw=OLD.id;
END;
