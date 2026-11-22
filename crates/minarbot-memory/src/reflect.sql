-- Hindsight v1.2.3, bd230e8bb40e768bf69f266f1dd3c9ae03e22fe3.
-- Adapted from hindsight-api-slim/hindsight_api/engine/mental_model_refresh.py.
CREATE TABLE IF NOT EXISTS memory_mental_models (
    bank TEXT NOT NULL REFERENCES memory_banks(id),
    id TEXT NOT NULL,
    question TEXT NOT NULL,
    answer TEXT NOT NULL DEFAULT '',
    evidence TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(evidence)),
    revision INTEGER NOT NULL DEFAULT 1,
    removed INTEGER NOT NULL DEFAULT 0,
    generation INTEGER NOT NULL DEFAULT -1,
    retry_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(bank,id)
) STRICT;

CREATE TABLE IF NOT EXISTS memory_bank_generations (
    bank TEXT PRIMARY KEY REFERENCES memory_banks(id),
    generation INTEGER NOT NULL DEFAULT 1
) STRICT;

-- Track changes to durable knowledge. History remains searchable separately.
CREATE TRIGGER IF NOT EXISTS memory_model_unit_insert AFTER INSERT ON memory_units
WHEN NEW.origin IN ('legacy','retained') AND NEW.removed=0 BEGIN
    INSERT INTO memory_bank_generations(bank) VALUES(NEW.bank)
    ON CONFLICT(bank) DO UPDATE SET generation=generation+1;
END;
CREATE TRIGGER IF NOT EXISTS memory_model_unit_update AFTER UPDATE OF text,removed ON memory_units
WHEN NEW.origin IN ('legacy','retained') AND (NEW.text IS NOT OLD.text OR NEW.removed IS NOT OLD.removed) BEGIN
    INSERT INTO memory_bank_generations(bank) VALUES(NEW.bank)
    ON CONFLICT(bank) DO UPDATE SET generation=generation+1;
END;
CREATE TRIGGER IF NOT EXISTS memory_model_unit_delete AFTER DELETE ON memory_units
WHEN OLD.origin IN ('legacy','retained') AND OLD.removed=0 BEGIN
    INSERT INTO memory_bank_generations(bank) VALUES(OLD.bank)
    ON CONFLICT(bank) DO UPDATE SET generation=generation+1;
END;

CREATE TABLE IF NOT EXISTS memory_unit_controls (
    unit INTEGER PRIMARY KEY REFERENCES memory_units(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL,
    essential INTEGER NOT NULL DEFAULT 0 CHECK(essential IN (0,1)),
    edited INTEGER NOT NULL DEFAULT 0 CHECK(edited IN (0,1))
) STRICT;

CREATE TRIGGER IF NOT EXISTS memory_unit_revision AFTER UPDATE OF text,removed ON memory_units
WHEN NEW.origin='retained' AND (NEW.text IS NOT OLD.text OR NEW.removed IS NOT OLD.removed) BEGIN
    INSERT INTO memory_unit_controls(unit,revision) VALUES(NEW.id,2)
    ON CONFLICT(unit) DO UPDATE SET revision=revision+1;
END;

-- Reject old derived observations when a user changes their supporting fact.
CREATE TRIGGER IF NOT EXISTS memory_fact_edit AFTER UPDATE OF text,removed ON memory_units
WHEN (NEW.text IS NOT OLD.text OR NEW.removed IS NOT OLD.removed)
  AND EXISTS(SELECT 1 FROM memory_facts WHERE unit=NEW.id) BEGIN
    UPDATE memory_facts SET consolidated=0,attempts=0,ready_at=0
    WHERE unit IN (
        SELECT e.fact FROM memory_observation_evidence e
        JOIN memory_observation_evidence old ON old.observation=e.observation WHERE old.fact=NEW.id
    ) OR unit=NEW.id;
    DELETE FROM memory_units WHERE id IN (
        SELECT observation FROM memory_observation_evidence WHERE fact=NEW.id
    );
END;
