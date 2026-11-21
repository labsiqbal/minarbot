import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { testDatabase } from './test-support/database.ts';
import { installCanonicalFixture } from './test-support/canonical-schema.ts';
import { startRustServer } from './rust-test-server.ts';

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

for (const mode of ['canonical', 'local'] as const) {
  test(`direct ${mode} schema matches pre-collapse startup`, async () => {
    const directory = mkdtempSync(join(tmpdir(), 'minarbot-schema-'));
    const old = join(directory, 'pre-collapse.sqlite');
    const fresh = join(directory, 'fresh.sqlite');
    const expected = testDatabase(old);
    try {
      if (mode === 'canonical') installCanonicalFixture(expected);
      else expected.exec(readFileSync(new URL('./test-support/fixtures/local-pre-collapse.sql', import.meta.url), 'utf8'));
      const frozen = snapshot(expected);
      const frozenLedger = mode === 'canonical' ? expected.prepare(ledger).all() : [];
      for (const database of [old, fresh]) {
        const app = await startRustServer({ database, auth: mode === 'canonical' ? auth : undefined });
        await app.close();
      }
      const actual = testDatabase(fresh, { readOnly: true });
      try {
        const existing = snapshot(expected);
        assert.deepEqual(snapshot(actual), existing);
        assert.equal(existing.applicationId, frozen.applicationId);
        const existingLedger = mode === 'canonical' ? expected.prepare(ledger).all() : [];
        if (existing.userVersion === frozen.userVersion && existingLedger.length === frozenLedger.length) {
          // A later module can add objects at startup. Keep comparing every released object.
          const frozenNames = new Set(frozen.objects.map(row => row.name));
          assert.deepEqual(existing.objects.filter(row => frozenNames.has(row.name)), frozen.objects);
        }
        if (mode === 'canonical') {
          assert.deepEqual(actual.prepare(ledger).all(), existingLedger);
          assert.deepEqual(existingLedger.slice(0, frozenLedger.length), frozenLedger);
          assert.equal(actual.prepare('SELECT serving_state FROM authority_state').get()?.serving_state, 'active');
        }
      } finally { actual.close(); }
    } finally {
      expected.close();
      rmSync(directory, { recursive: true, force: true });
    }
  });
}
