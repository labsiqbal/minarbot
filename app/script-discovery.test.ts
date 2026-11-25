import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { existsSync, lstatSync, mkdtempSync, readFileSync, rmSync, unlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { startApp } from './test-bridge.ts';

test('private script discovery follows live grants, rotates on restart and preserves foreign files', async () => {
  const root = mkdtempSync(join(tmpdir(), 'minarbot-discovery-'));
  const directory = join(root, 'scripts'), file = join(directory, 'connection.json');
  const options = { database: join(root, 'db.sqlite'), scriptDirectory: directory };
  let app = await startApp(options);
  let closed = false;
  async function request(path: string, body?: unknown) {
    const res = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const value = await res.json(); assert(res.ok, JSON.stringify(value)); return value;
  }
  const read = () => JSON.parse(readFileSync(file, 'utf8'));
  try {
    assert.equal(lstatSync(directory).mode & 0o777, 0o700);
    assert(!existsSync(file));
    const a = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'A', model: 'test', instructions: 'Help' });
    const b = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'B', model: 'test', instructions: 'Help' });
    const path = `/api/agents/${a.id}/scripts`;
    assert.equal((await request(path)).configured, true);
    const enable = { id: randomUUID(), revision: 0, enabled: true };
    writeFileSync(file, 'preserve', { mode: 0o600 });
    const failed = await fetch(app.origin + path, { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(enable) });
    assert.equal(failed.status, 500); await failed.arrayBuffer();
    assert.equal(readFileSync(file, 'utf8'), 'preserve'); unlinkSync(file);
    await request(path, enable);
    const first = read(); assert.equal(first.url, app.origin); assert.match(first.token, /^[A-Za-z0-9_-]{43}$/);
    assert.equal(lstatSync(file).mode & 0o777, 0o600);
    const response = await fetch(first.url + `/api/scripts/agents/${a.id}/provider`, { headers: { Authorization: `Bearer ${first.token}` } });
    assert.equal(response.status, 200); assert(!(await response.text()).includes(first.token));
    await request(`/api/agents/${b.id}/scripts`, { id: randomUUID(), revision: 0, enabled: true });
    await request(path, { id: randomUUID(), revision: 1, enabled: false }); assert.deepEqual(read(), first);
    await request(`/api/agents/${b.id}/scripts`, { id: randomUUID(), revision: 1, enabled: false }); assert(!existsSync(file));
    await request(path, enable); assert(!existsSync(file), 'historical enabled receipt must not republish revoked access');
    await request(path, { id: randomUUID(), revision: 2, enabled: true });
    await app.close(); assert(!existsSync(file)); assert(!existsSync(join(directory, 'owner.lock')));
    app = await startApp(options);
    const second = read(); assert.notEqual(second.token, first.token); assert.equal(second.url, app.origin);
    const stale = await fetch(app.origin + `/api/scripts/agents/${a.id}/provider`, { headers: { Authorization: `Bearer ${first.token}` } });
    assert.equal(stale.status, 403); await stale.arrayBuffer();
    await assert.rejects(startApp({ ...options, database: join(root, 'competing.sqlite') }));
    assert.deepEqual(read(), second);
    await app.close();
    closed = true;
    writeFileSync(file, 'foreign', { mode: 0o600 });
    await assert.rejects(startApp(options)); assert.equal(readFileSync(file, 'utf8'), 'foreign');
    assert(!existsSync(join(directory, 'owner.lock')));
    unlinkSync(file);
    app = await startApp(options);
    closed = false;
    assert.equal(read().url, app.origin);
  } finally { if (!closed) await app.close(); rmSync(root, { recursive: true, force: true }); }
});
