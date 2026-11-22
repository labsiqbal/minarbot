CREATE TABLE IF NOT EXISTS goals(
  id TEXT PRIMARY KEY REFERENCES turns(id) ON DELETE CASCADE,
  scope TEXT NOT NULL,
  target TEXT NOT NULL,
  request TEXT NOT NULL,
  max_turns INTEGER NOT NULL,
  max_minutes INTEGER NOT NULL,
  max_tokens INTEGER NOT NULL,
  deadline INTEGER NOT NULL,
  current_turn TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
  turn_count INTEGER NOT NULL DEFAULT 1,
  status TEXT NOT NULL DEFAULT 'active',
  reason TEXT
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS active_goal_scope ON goals(scope) WHERE status='active';
CREATE INDEX IF NOT EXISTS goals_by_scope ON goals(scope);
CREATE UNIQUE INDEX IF NOT EXISTS goals_current_turn ON goals(current_turn);

CREATE TABLE IF NOT EXISTS goal_turns(
  turn TEXT PRIMARY KEY REFERENCES turns(id) ON DELETE CASCADE,
  goal TEXT NOT NULL REFERENCES goals(id) ON DELETE CASCADE,
  number INTEGER NOT NULL,
  estimated_tokens INTEGER NOT NULL DEFAULT 0,
  UNIQUE(goal,number)
) STRICT;
CREATE TABLE IF NOT EXISTS goal_verdicts(
  turn TEXT PRIMARY KEY REFERENCES turns(id) ON DELETE CASCADE,
  met INTEGER NOT NULL,
  reason TEXT NOT NULL,
  evidence TEXT NOT NULL,
  signature TEXT NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS goal_judge_models(
  agent TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
  runtime TEXT NOT NULL,
  model TEXT NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS routine_loops(
  routine TEXT PRIMARY KEY REFERENCES routines(id) ON DELETE CASCADE,
  request TEXT NOT NULL,
  until_condition TEXT,
  max_runs INTEGER NOT NULL,
  reason TEXT
) STRICT;
