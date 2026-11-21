CREATE TABLE IF NOT EXISTS agent_computers(agent TEXT PRIMARY KEY REFERENCES agents(id),
      enabled INTEGER NOT NULL CHECK(enabled IN(0,1)),revision INTEGER NOT NULL) STRICT;
    CREATE TABLE IF NOT EXISTS computer_always_on(agent TEXT PRIMARY KEY REFERENCES agent_computers(agent) ON DELETE CASCADE) STRICT;
    CREATE TABLE IF NOT EXISTS computer_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
    CREATE TABLE IF NOT EXISTS computer_actions(turn TEXT NOT NULL REFERENCES turns(id),call_id TEXT NOT NULL,
      state TEXT NOT NULL CHECK(state IN('uncertain','done')),image BLOB,PRIMARY KEY(turn,call_id)) STRICT;
    CREATE TABLE IF NOT EXISTS computer_control(agent TEXT PRIMARY KEY REFERENCES agents(id),
      mode TEXT NOT NULL CHECK(mode IN('agent','user')),who TEXT NOT NULL,since TEXT NOT NULL,revision INTEGER NOT NULL) STRICT;
    CREATE TABLE IF NOT EXISTS computer_waits(turn TEXT PRIMARY KEY REFERENCES turns(id),agent TEXT NOT NULL REFERENCES agents(id),
      call_id TEXT NOT NULL,reason TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN('waiting','returned','declined')),pause_revision INTEGER NOT NULL) STRICT;
