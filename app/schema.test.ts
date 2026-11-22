import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { testDatabase } from './test-support/database.ts';
import { installCanonicalFixture } from './test-support/canonical-schema.ts';
import { startRustServer } from './rust-test-server.ts';
import { openHubDatabase } from './hub/schema.ts';

const auth = { secretKey: 'sk_test_fixture', publishableKey: 'pk_test_fixture', issuer: 'https://fixture.clerk.accounts.dev', origin: 'https://minarbot.example', development: true };

function snapshot(db: ReturnType<typeof testDatabase>) {
  const objects = db.prepare('SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name').all()
    .map(({ type, name, tbl_name, sql }) => ({ type, name, tbl_name, sql: typeof sql === 'string' ? sql.trim().replace(/\s+/g, ' ') : sql }));
  return {
    objects,
    userVersion: db.prepare('PRAGMA user_version').get()?.user_version,
    applicationId: db.prepare('PRAGMA application_id').get()?.application_id,
  };
}

const ledger = 'SELECT version, name, checksum FROM schema_migrations ORDER BY version';

function fixture(name: string) {
  return readFileSync(new URL(`./test-support/fixtures/${name}-pre-collapse.sql`, import.meta.url), 'utf8');
}

function addMemorySchema(db: ReturnType<typeof testDatabase>, mode: 'canonical' | 'local') {
  for (const path of ['minarbot-memory/src/schema.sql', 'minarbot-memory/src/retain.sql']) {
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
  test(`fresh ${mode} startup adds memory schema to 85373b3 and reopens its data unchanged`, async () => {
    const directory = mkdtempSync(join(tmpdir(), 'minarbot-schema-'));
    const old = join(directory, 'pre-collapse.sqlite');
    const fresh = join(directory, 'fresh.sqlite');
    const expected = testDatabase(':memory:');
    try {
      expected.exec(fixture(mode));
      const frozen = snapshot(expected);
      const frozenLedger = mode === 'canonical' ? expected.prepare(ledger).all() : [];
      addMemorySchema(expected, mode);
      const current = snapshot(expected);
      await startAndClose(fresh, mode);
      const actual = testDatabase(fresh, { readOnly: true });
      try {
        assert.deepEqual(snapshot(actual), current);
        if (mode === 'canonical') {
          assert.deepEqual(actual.prepare(ledger).all(), frozenLedger);
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
        const beforeLedger = mode === 'canonical' ? existing.prepare('SELECT * FROM schema_migrations ORDER BY version').all() : [];
        await startAndClose(old, mode);
        assert.deepEqual(snapshot(existing), current);
        const upgradedVersion = existing.prepare('PRAGMA schema_version').get();
        assert.notDeepEqual(upgradedVersion, beforeVersion);
        await startAndClose(old, mode);
        assert.deepEqual(snapshot(existing), current);
        assert.deepEqual(existing.prepare('PRAGMA schema_version').get(), upgradedVersion);
        assert.deepEqual(existing.prepare(retainedData).get(), saved);
        if (mode === 'canonical') assert.deepEqual(existing.prepare('SELECT * FROM schema_migrations ORDER BY version').all(), beforeLedger);
      } finally { existing.close(); }
    } finally {
      expected.close();
      rmSync(directory, { recursive: true, force: true });
    }
  });
}

test('fresh hub startup matches 85373b3', () => {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-hub-schema-'));
  const expected = testDatabase(':memory:');
  try {
    expected.exec(fixture('hub'));
    const hub = openHubDatabase(join(directory, 'hub.sqlite'));
    try {
      assert.deepEqual(snapshot(hub.db), snapshot(expected));
      assert.deepEqual(hub.db.prepare('SELECT * FROM hub_migrations').all(), expected.prepare('SELECT * FROM hub_migrations').all());
    } finally { hub.close(); }
  } finally {
    expected.close();
    rmSync(directory, { recursive: true, force: true });
  }
});
