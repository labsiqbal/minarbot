import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { createServer } from 'node:http';
import { markMinarbotData, testDatabase } from './test-support/database.ts';
import { chmodSync, linkSync, mkdtempSync, readFileSync, writeFileSync, rmSync, statSync, symlinkSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { startApp, startCanonicalApp } from './test-bridge.ts';
import { rustBinary } from './rust-binary.ts';
import { checkedRestore, restoreWorkspace, workspaceSnapshot } from './test-support/backup.ts';
import { installCanonicalFixture } from './test-support/canonical-schema.ts';
import { canonicalHttp } from './test-support/canonical-http.ts';

test('canonical server guard fences aliases and processes, releases on SIGKILL and rejects unsafe files', async () => {
  const dir=mkdtempSync(join(tmpdir(),'minarbot-server-lock-')),database=join(dir,'canonical.sqlite'),guardPath=database+'.server-lock.sqlite';
  const source = testDatabase(database);
  installCanonicalFixture(source);
  source.exec("UPDATE authority_state SET serving_state='read_only_recovery'");
  source.close();
  chmodSync(database, 0o600);
  const auth = { origin: 'http://127.0.0.1', development: true, issuer: 'https://fixture.example', secretKey: 'fixture-secret', publishableKey: 'fixture-key' };
  const fence = () => checkedRestore(['fence-request', '--db', database, '--session', join(dir, 'missing.json')]);
  const binary = rustBinary('minarbot-server', process.env.MINARBOT_RUST_BINARY);
  const probe = (path: string) => spawnSync(binary, ['--test-config'], {
    env: { ...process.env, MINARBOT_TEST_SERVER: 'rust' },
    input: JSON.stringify({ database: path, port: 0, auth }), encoding: 'utf8', timeout: 3000,
  });
  const app = await startCanonicalApp({ database, auth });
  try {
    assert.throws(fence, /Another canonical server/);
    const alias = join(dir, 'alias.sqlite'); symlinkSync(database, alias);
    const aliasResult = probe(alias);
    assert.equal(aliasResult.status, 1, aliasResult.stderr); assert.match(aliasResult.stderr, /Another canonical server/);
    assert.throws(fence, /Another canonical server/);
    const retained = statSync(guardPath);
    await app.crash();
    assert.throws(fence, /No such file/);
    assert.equal(statSync(guardPath).mode & 0o777, 0o600);
    assert.equal(statSync(guardPath).ino, retained.ino);
    const hard = join(dir, 'hard.sqlite'); linkSync(database, hard);
    assert.match(probe(hard).stderr, /hard link/); rmSync(hard);
    const initial = readFileSync(guardPath); rmSync(guardPath);
    symlinkSync(join(dir,'missing'), guardPath); assert.throws(fence, /private/); rmSync(guardPath);
    writeFileSync(guardPath, initial, { mode: 0o644 }); assert.throws(fence, /private/); chmodSync(guardPath, 0o600);
    const linked=join(dir,'guard-link'); linkSync(guardPath, linked); assert.throws(fence, /private/); rmSync(linked);
    writeFileSync(guardPath, 'not sqlite'); assert.throws(fence, /Another canonical server/);
    assert.equal(readFileSync(guardPath, 'utf8'), 'not sqlite');
    writeFileSync(guardPath, initial); assert.throws(fence, /No such file/);
  } finally {
    await app.close();
    rmSync(dir, { recursive: true, force: true });
  }
});

test('v4 backup retains sync floors, monotonic heads, tombstones and receipts while restored authority stays fenced', async () => {
  const f = canonicalHttp('minarbot-sync-backup-');
  const { db, dir, owner } = f;
  const now = Date.now(), project = randomUUID();
  let backup: Awaited<ReturnType<typeof workspaceSnapshot>> | undefined;
  try {
    await f.start({ fixedClock: now - 90 * 86400_000 - 1 });
    await f.request('/api/projects', 'POST', { requestId: randomUUID(), id: project, name: 'Retained tombstone' });
    db.prepare("INSERT INTO record_scopes VALUES ('projects',?,?,?,'owner','project',?)").run(JSON.stringify([project]), owner.accountId, project, project);
    db.prepare("UPDATE record_scopes SET visibility='project' WHERE project=?").run(project);
    await f.request(`/api/projects/${project}`, 'PATCH', { requestId: randomUUID(), expectedRevision: 1, deleted: true });
    assert.equal(db.prepare('SELECT count(*) n FROM sync_events').get()!.n, 2);
    await f.start({ fixedClock: now });
    const deadline = Date.now() + 10000;
    while (Number(db.prepare('SELECT count(*) n FROM sync_events').get()!.n) && Date.now() < deadline) {
      await new Promise(resolve => setTimeout(resolve, 20));
    }
    assert.equal(db.prepare('SELECT count(*) n FROM sync_events').get()!.n, 0);
    const retained = Object.fromEntries(['sync_heads','retention_floors','sync_scope_revisions','receipts','projects'].map(table => [table, db.prepare('SELECT * FROM ' + table).all()]));
    backup = await workspaceSnapshot(db, owner.accountId);
    const destination = join(dir, 'restored.sqlite'); restoreWorkspace(backup.file, destination, { canonicalOnly: true });
    const restored = testDatabase(destination);
    try {
      for (const [table, rows] of Object.entries(retained)) assert.deepEqual(restored.prepare('SELECT * FROM ' + table).all(), rows);
      assert.equal(restored.prepare('SELECT serving_state FROM authority_state').get()!.serving_state, 'read_only_recovery');
    } finally { restored.close(); }
    const live = randomUUID(), recovery = testDatabase(destination);
    try {
      recovery.prepare("INSERT INTO projects VALUES(?,?,'Visible before restore',0,0,1)").run(live, owner.accountId);
      recovery.prepare("INSERT INTO project_members VALUES(?,?,'owner',1,0)").run(live, owner.accountId);
    } finally { recovery.close(); }
    await f.start({ database: destination });
    await assert.rejects(f.request(`/api/sync?projectId=${project}`), { code: 'sync_scope_denied' });
    await assert.rejects(f.request(`/api/sync?projectId=${live}`), { code: 'authority_unavailable' });
  } finally { backup?.cleanup(); await f.close(); }
});

test('live backup restores history while holding pending work and disabling automation', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-backup-test-'));
  let accepted!: () => void, calls = 0;
  const receipt = new Promise<void>(resolve => { accepted = resolve; });
  const provider = createServer(async req => { for await (const _chunk of req) {} calls++; accepted(); });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const options = { database: join(dir, 'source.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1` };
  let app = await startApp(options, { directory: dir, servers: [provider] }), restored: Awaited<ReturnType<typeof startApp>> | undefined;
  async function request(path: string, body?: unknown) {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const data = await response.json(); assert(response.ok, JSON.stringify(data)); return data;
  }
  try {
    const agent = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Backup', role: 'Personal agent', model: 'fixture', instructions: 'Help.' });
    await app.close();
    const seed = testDatabase(options.database);
    seed.prepare('INSERT INTO routines(id,agent,name,prompt,next_at,enabled,revision) VALUES (?,?,?,?,?,1,1)').run(randomUUID(), agent.id, 'Future', 'Help', Date.now() + 3600000);
    seed.prepare('INSERT INTO script_permissions VALUES (?,1,1)').run(agent.id);
    seed.prepare('INSERT INTO files(id,scope,name,digest,bytes,created) VALUES (?,?,?,?,?,?)').run(randomUUID(), `agents:${agent.id}`, 'binary.bin', 'fixture', Buffer.from([0, 255, 1]), new Date().toISOString());
    seed.close();
    app = await startApp(options);
    const turn = randomUUID(); await request(`/api/agents/${agent.id}/turns`, { id: turn, text: 'Pending provider result' }); await receipt;
    const workflow = await request(`/api/agents/${agent.id}/workflows`, { id: randomUUID(), revision: 0, name: 'Waiting approval', graph: { entry: 'approval', maxSteps: 2, nodes: [{ id: 'approval', kind: 'approval', prompt: 'Continue?', yes: 'out' }, { id: 'out', kind: 'output' }] } });
    const hook = await request(`/api/workflows/${workflow.id}/webhook`, { id: randomUUID(), revision: 0, rotate: true, enabled: true });
    await request(`/api/workflows/${workflow.id}/run`, { id: randomUUID(), revision: 1, input: 'Retain' });
    await request(`/api/workflows/${workflow.id}/schedule`, { id: randomUUID(), revision: 0, nextAt: Date.now() + 3600000, intervalSeconds: null, input: 'Future', enabled: true });
    const denied = await fetch(app.origin + '/api/backup', { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify({ id: randomUUID(), confirmed: false }) });
    assert.equal(denied.status, 400);
    const response = await fetch(app.origin + '/api/backup', { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify({ id: randomUUID(), confirmed: true }) });
    assert.equal(response.status, 200); assert.match(response.headers.get('content-disposition')!, /attachment/);
    const file = join(dir, 'backup.sqlite'), bytes = Buffer.from(await response.arrayBuffer()); writeFileSync(file, bytes, { mode: 0o600 });
    assert.equal(bytes.subarray(0, 15).toString(), 'SQLite format 3');
    const destination = join(dir, 'restored.sqlite'); restoreWorkspace(file, destination);
    assert.equal(statSync(destination).mode & 0o777, 0o600);
    assert.deepEqual(readFileSync(file), bytes);
    const db = testDatabase(destination);
    assert.equal(db.prepare('SELECT enabled FROM routines').get()!.enabled, 0);
    assert.equal(db.prepare('SELECT enabled FROM workflow_webhooks').get()!.enabled, 0);
    assert.equal(db.prepare('SELECT secret FROM workflow_webhooks').get()!.secret, hook.secret);
    assert.deepEqual(Buffer.from(db.prepare('SELECT bytes FROM files').get()!.bytes as Uint8Array), Buffer.from([0, 255, 1]));
    assert.equal(db.prepare('SELECT count(*) AS n FROM agent_creations').get()!.n, 1);
    assert.equal(db.prepare('SELECT enabled FROM script_permissions').get()!.enabled, 0);
    assert.equal(db.prepare('SELECT enabled FROM workflow_schedules').get()!.enabled, 0);
    assert.equal(db.prepare('SELECT status FROM workflow_runs').get()!.status, 'failed');
    assert.equal(db.prepare('SELECT paused FROM turns WHERE id=?').get(turn)!.paused, 1); db.close();
    restored = await startApp({ ...options, database: destination });
    assert.equal((await (await fetch(restored.origin + '/api/agents')).json()).agents[0].role, 'Personal agent');
    const turns = await (await fetch(restored.origin + `/api/agents/${agent.id}/turns`)).json();
    assert.equal(turns[0].text, 'Pending provider result'); assert.equal(turns[0].status, 'interrupted');
    await new Promise(resolve => setTimeout(resolve, 1100)); assert.equal(calls, 1);
    assert.throws(() => restoreWorkspace(file, destination), /new database/);
    const symlink = join(dir, 'symlink.sqlite'); symlinkSync(destination, symlink); assert.throws(() => restoreWorkspace(file, symlink), /new database/);
    const dangling = join(dir, 'dangling.sqlite'); symlinkSync(join(dir, 'missing'), dangling + '-wal');
    assert.throws(() => restoreWorkspace(file, dangling), /new database/);
    const unknown = join(dir, 'unknown.sqlite'), unknownDb = testDatabase(unknown); markMinarbotData(unknownDb); unknownDb.exec("CREATE TABLE minarbot_backup(format TEXT,version INTEGER); INSERT INTO minarbot_backup VALUES ('minarbot-workspace',99)"); unknownDb.close();
    assert.throws(() => restoreWorkspace(unknown, join(dir, 'unknown-target.sqlite')), /backup format/);
    const corrupt = join(dir, 'corrupt'); writeFileSync(corrupt, 'not sqlite'); assert.throws(() => restoreWorkspace(corrupt, join(dir, 'invalid.sqlite')));
  } finally { await restored?.close(); await app.close(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(dir, { recursive: true, force: true }); }
});
