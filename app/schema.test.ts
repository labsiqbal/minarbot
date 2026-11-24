import test from 'node:test';
import assert from 'node:assert/strict';
import { copyFileSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { testDatabase } from './test-support/database.ts';
import { installCanonicalFixture } from './test-support/canonical-schema.ts';
import { startRustServer } from './rust-test-server.ts';
import { rustBinary } from './rust-binary.ts';
import { startRustHub } from './test-support/hosted-hub.ts';

const auth = { secretKey: 'sk_test_fixture', publishableKey: 'pk_test_fixture', issuer: 'https://fixture.clerk.accounts.dev', origin: 'https://minarbot.example', development: true };

function snapshot(db: ReturnType<typeof testDatabase>) {
  const objects = db.prepare('SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name').all()
    .filter(({ name }) => name !== 'provider_instances' && name !== 'sqlite_autoindex_provider_instances_1')
    .map(({ type, name, tbl_name, sql }) => ({ type, name, tbl_name, sql: name === 'agents' ? null : typeof sql === 'string' ? sql.trim().replace(/\s+/g, ' ') : sql }));
  return {
    objects,
    userVersion: 0,
    applicationId: db.prepare('PRAGMA application_id').get()?.application_id,
  };
}

const ledger = 'SELECT version, name, checksum FROM schema_migrations ORDER BY version';

function fixture(name: string) {
  return readFileSync(new URL(`./test-support/fixtures/${name}-pre-collapse.sql`, import.meta.url), 'utf8');
}

function addMemorySchema(db: ReturnType<typeof testDatabase>, mode: 'canonical' | 'local', phase2 = true) {
  for (const path of phase2 ? ['minarbot-memory/src/schema.sql', 'minarbot-memory/src/retain.sql', 'minarbot-memory/src/reflect.sql'] : ['minarbot-memory/src/schema.sql']) {
    db.exec(readFileSync(new URL(`../crates/${path}`, import.meta.url), 'utf8'));
  }
  if (mode === 'canonical') {
    db.exec(readFileSync(new URL('../crates/minarbot-server/src/data/memory-history-canonical.sql', import.meta.url), 'utf8'));
  } else {
    db.exec(`CREATE VIEW memory_history_scope AS
      SELECT t.id, CASE WHEN t.channel IS NOT NULL THEN 'channels:'||t.channel
                        ELSE 'agents:'||coalesce(th.agent,t.agent) END AS bank
      FROM turns t LEFT JOIN threads th ON th.id=t.thread
      WHERE t.channel IS NOT NULL OR coalesce(th.agent,t.agent) IS NOT NULL`);
  }
  db.exec(readFileSync(new URL('../crates/minarbot-server/src/data/memory-import.sql', import.meta.url), 'utf8'));
  db.exec('CREATE TABLE memory_imports(version INTEGER PRIMARY KEY) STRICT');
  if (mode === 'local' && phase2) {
    db.exec("ALTER TABLE agents ADD COLUMN learn_from_conversations INTEGER NOT NULL DEFAULT 1 CHECK(learn_from_conversations IN(0,1)); ALTER TABLE agents ADD COLUMN memory_model TEXT NOT NULL DEFAULT '';");
    db.exec(readFileSync(new URL('../crates/minarbot-server/src/data/memory-worker.sql', import.meta.url), 'utf8'));
  }
}

async function startAndClose(database: string, mode: 'canonical' | 'local') {
  const app = await startRustServer({ database, auth: mode === 'canonical' ? auth : undefined });
  await app.close();
}

async function startBaseline(database: string, mode: 'canonical' | 'local', binary: string) {
  const current = process.env.MINARBOT_RUST_BINARY;
  process.env.MINARBOT_RUST_BINARY = binary;
  try { await startAndClose(database, mode); }
  finally {
    if (current === undefined) delete process.env.MINARBOT_RUST_BINARY;
    else process.env.MINARBOT_RUST_BINARY = current;
  }
}

const retainedData = `SELECT a.*, c.request, t.reply, t.status, p.enabled, o.agent AS always_on
  FROM agents a JOIN agent_creations c ON c.id=a.id JOIN turns t ON t.agent=a.id
  JOIN agent_computers p ON p.agent=a.id JOIN computer_always_on o ON o.agent=a.id
  WHERE a.id='fixture-agent'`;

for (const mode of ['canonical', 'local'] as const) {
  test(`fresh ${mode} startup extends the 85373b3 schema and reopens its data unchanged`, async () => {
    const directory = mkdtempSync(join(tmpdir(), 'minarbot-schema-'));
    const old = join(directory, 'pre-collapse.sqlite');
    const fresh = join(directory, 'fresh.sqlite');
    const expected = testDatabase(':memory:');
    try {
      expected.exec(fixture(mode));
      expected.exec(readFileSync(new URL('../crates/minarbot-server/src/data/provider-instances.sql', import.meta.url), 'utf8'));
      expected.exec("ALTER TABLE agents ADD COLUMN provider_instance TEXT NOT NULL DEFAULT ''; ALTER TABLE agents ADD COLUMN model_options TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(model_options));");
      const frozen = snapshot(expected);
      if (mode === 'local') {
        expected.exec(readFileSync(new URL('../crates/minarbot-server/src/turns/goals.sql', import.meta.url), 'utf8'));
        expected.exec(`CREATE TABLE craft_deep(project TEXT PRIMARY KEY REFERENCES local_projects(id) ON DELETE CASCADE) STRICT;
          CREATE TABLE install_token_targets(
            token BLOB PRIMARY KEY REFERENCES install_tokens(digest) ON DELETE CASCADE,
            origin TEXT NOT NULL
          ) STRICT;`);
      }
      expected.exec(`CREATE TABLE pairing_code_targets(id TEXT PRIMARY KEY REFERENCES pairing_codes(id) ON DELETE CASCADE,origin TEXT NOT NULL) STRICT;
        CREATE TABLE mcp_apps(id TEXT PRIMARY KEY,turn TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,connection TEXT NOT NULL REFERENCES mcp_connections(id) ON DELETE CASCADE,revision INTEGER NOT NULL,title TEXT NOT NULL,html TEXT NOT NULL,bootstrap TEXT NOT NULL) STRICT;
        CREATE INDEX mcp_apps_turn ON mcp_apps(turn);
        CREATE TABLE mcp_app_approvals(app TEXT PRIMARY KEY REFERENCES mcp_apps(id) ON DELETE CASCADE,id TEXT NOT NULL,digest TEXT NOT NULL,expires INTEGER NOT NULL) STRICT;`);
      expected.exec(readFileSync(new URL('../crates/minarbot-server/src/files/schema.sql', import.meta.url), 'utf8'));
      expected.exec('ALTER TABLE agents ADD COLUMN library_docs_access INTEGER NOT NULL DEFAULT 1 CHECK(library_docs_access IN(0,1))');
      expected.exec(readFileSync(new URL('../crates/minarbot-server/src/turns/library_docs/schema.sql', import.meta.url), 'utf8'));
      addMemorySchema(expected, mode);
      const current = snapshot(expected);
      await startAndClose(fresh, mode);
      const actual = testDatabase(fresh, { readOnly: true });
      try {
        assert.deepEqual(snapshot(actual), current);
        if (mode === 'canonical') {
          assert.equal(actual.prepare('SELECT max(version) AS version FROM schema_migrations').get()?.version, 7);
          assert.equal(actual.prepare('SELECT serving_state FROM authority_state').get()?.serving_state, 'active');
        }
      } finally { actual.close(); }

      // Set this to a binary built from 85373b3 to exercise real pre-collapse startup.
      const baseline = process.env.MINARBOT_SCHEMA_BASELINE_BINARY;
      if (baseline) await startBaseline(old, mode, baseline);
      const existing = testDatabase(old);
      try {
        if (!baseline) {
          if (mode === 'canonical') installCanonicalFixture(existing);
          else existing.exec(fixture(mode));
        }
        assert.deepEqual(snapshot(existing), frozen);
        existing.exec(`INSERT INTO agents(id,name,model,instructions) VALUES('fixture-agent','Saved agent','fixture','Keep data');
          INSERT INTO agent_creations VALUES('fixture-agent','{}');
          INSERT INTO turns(id,agent,text,reply,status,created) VALUES('fixture-turn','fixture-agent','Question','Saved answer','done','2026-11-22');
          INSERT INTO agent_computers VALUES('fixture-agent',1,2);
          INSERT INTO computer_always_on VALUES('fixture-agent');`);
        const saved = existing.prepare(retainedData).get();
        assert.equal(saved?.reply, 'Saved answer');
        const beforeVersion = existing.prepare('PRAGMA schema_version').get();
        await startAndClose(old, mode);
        assert.deepEqual(snapshot(existing), current);
        const upgradedVersion = existing.prepare('PRAGMA schema_version').get();
        assert.notDeepEqual(upgradedVersion, beforeVersion);
        await startAndClose(old, mode);
        assert.deepEqual(snapshot(existing), current);
        assert.deepEqual(existing.prepare('PRAGMA schema_version').get(), upgradedVersion);
        const retained = { ...existing.prepare(retainedData).get() };
        delete retained.provider_instance;
        delete retained.model_options;
        assert.deepEqual(retained, mode === 'local'
          ? { ...saved, learn_from_conversations: 1, memory_model: '', library_docs_access: 1 } : { ...saved, library_docs_access: 1 });
        if (mode === 'canonical') assert.equal(existing.prepare('SELECT max(version) AS version FROM schema_migrations').get()?.version, 7);
      } finally { existing.close(); }
    } finally {
      expected.close();
      rmSync(directory, { recursive: true, force: true });
    }
  });
}

test('fresh Rust hub startup preserves the 85373b3 schema and applies extensions', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-hub-schema-'));
  const expected = testDatabase(':memory:');
  try {
    expected.exec(fixture('hub'));
    const dataPath = join(directory, 'hub.sqlite');
    const mailboxKeyPath = join(directory, 'mailbox.key');
    const tokensPath = join(directory, 'tokens.json');
    expected.exec(readFileSync(new URL('../crates/minarbot-server/src/hub/extensions.sql', import.meta.url), 'utf8'));
    writeFileSync(mailboxKeyPath, Buffer.alloc(32, 8), { mode: 0o600 });
    writeFileSync(tokensPath, '{}', { mode: 0o600 });
    const binary = rustBinary('minarbot-server', process.env.MINARBOT_HOSTED_HUB_BINARY ?? process.env.MINARBOT_RUST_BINARY);
    const hub = await startRustHub(binary, directory, {
      domain: 'minarbot.test', accountsHost: 'accounts.minarbot.test', dataPath, mailboxKeyPath,
      identity: { mode: 'stub', tokensPath },
    });
    await hub.close();
    const db = testDatabase(dataPath, { readOnly: true });
    try {
      assert.deepEqual(snapshot(db), snapshot(expected));
      assert.deepEqual(db.prepare('SELECT * FROM hub_migrations').all(), expected.prepare('SELECT * FROM hub_migrations').all());
    } finally { db.close(); }
  } finally {
    expected.close();
    rmSync(directory, { recursive: true, force: true });
  }
});

test('local 0.16.4 memory import does not backfill learning jobs', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-memory-upgrade-'));
  const database = join(directory, 'legacy.sqlite');
  const baseline = join(directory, 'baseline.sqlite');
  const released = process.env.MINARBOT_V0164_BINARY;
  try {
    if (released) await startBaseline(baseline, 'local', released);
    const original = testDatabase(baseline);
    try {
      if (!released) original.exec(fixture('local'));
      original.exec(`INSERT INTO agents(id,name,model,instructions) VALUES('learning-agent','Learning agent','agent-model','');
        INSERT INTO turns(id,agent,text,reply,status,created) VALUES('historical-turn','learning-agent','Old question','Old answer','done','2026-11-22');`);
      addMemorySchema(original, 'local', false);
      original.exec('INSERT INTO memory_imports VALUES(1); INSERT INTO memory_import SELECT * FROM memory_history_rows;');
      assert.equal(original.prepare('SELECT count(*) AS n FROM memory_units').get()?.n, 2);
    } finally { original.close(); }
    copyFileSync(baseline, database);

    const server = await startRustServer({ database });
    await server.close();
    const reopened = testDatabase(database);
    try {
      assert.equal(reopened.prepare('SELECT count(*) AS n FROM memory_learning_turns').get()?.n, 0);
      assert.equal(reopened.prepare('SELECT count(*) AS n FROM memory_extraction').get()?.n, 0);
      reopened.exec(`UPDATE turns SET reply='Edited answer' WHERE id='historical-turn';
        INSERT INTO turns(id,agent,text,status,created) VALUES('new-turn','learning-agent','New question','queued','2026-11-22');
        UPDATE turns SET status='done',reply='New answer' WHERE id='new-turn';`);
      assert.equal(reopened.prepare('SELECT count(*) AS n FROM memory_learning_turns').get()?.n, 1);
      const queued = reopened.prepare('SELECT source_id,origin FROM memory_extraction JOIN memory_units ON raw=id ORDER BY origin').all();
      assert.deepEqual(queued.map(row => [row.source_id, row.origin]), [['new-turn', 'prompt'], ['new-turn', 'reply']]);
      assert.equal(reopened.prepare("SELECT count(*) AS n FROM memory_units JOIN memory_fts ON memory_fts.rowid=id WHERE source_id='historical-turn' AND memory_fts MATCH 'question'").get()?.n, 1);

      reopened.exec(`INSERT INTO agents(id,name,model,instructions,learn_from_conversations) VALUES('paused-agent','Paused agent','agent-model','',0);
        INSERT INTO turns(id,agent,text,reply,status,created) VALUES('paused-turn','paused-agent','No learn','No learn','queued','2026-11-22');
        UPDATE turns SET status='done' WHERE id='paused-turn';`);
      assert.equal(reopened.prepare("SELECT count(*) AS n FROM memory_learning_turns WHERE agent='paused-agent'").get()?.n, 0);
      assert.equal(reopened.prepare("SELECT count(*) AS n FROM memory_units WHERE source_id='paused-turn'").get()?.n, 2);
    } finally {
      reopened.close();
    }
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
