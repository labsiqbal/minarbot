import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { createServer } from 'node:http';
import { testDatabase } from './test-support/database.ts';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { startApp } from './test-bridge.ts';


test('Rust workflow schedules retain offline work and coalesce provider-free occurrences', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-rust-schedule-'));
  const now = Date.now();
  const options = { database: join(directory, 'state.sqlite'), fixedClock: now };
  const app = await startApp(options);
  async function api(path: string, body?: unknown) {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const value = await response.json(); assert(response.ok, JSON.stringify(value)); return value;
  }
  try {
    const owner = await api('/api/agents', { id: randomUUID(), runtime: 'api', name: 'Schedule owner', model: 'fixture', instructions: 'Help.' });
    const graphs = [
      { entry: 'out', maxSteps: 2, nodes: [{ id: 'out', kind: 'output' }] },
      { entry: 'work', maxSteps: 3, nodes: [{ id: 'work', kind: 'model', model: 'fixture', prompt: 'Work', next: 'out' }, { id: 'out', kind: 'output' }] },
      { entry: 'work', maxSteps: 4, nodes: [{ id: 'work', kind: 'parallel', branches: [{ kind: 'model', model: 'fixture', prompt: 'One' }, { kind: 'model', model: 'fixture', prompt: 'Two' }], next: 'out' }, { id: 'out', kind: 'output' }] },
    ];
    const workflows = [];
    for (const [index, graph] of graphs.entries()) {
      const workflow = await api(`/api/agents/${owner.id}/workflows`, { id: randomUUID(), revision: 0, name: `Workflow ${index}`, graph });
      workflows.push(workflow);
      await api(`/api/workflows/${workflow.id}/schedule`, { id: randomUUID(), revision: 0, nextAt: now - 180000, intervalSeconds: 60, input: 'Saved input', enabled: true });
    }
    let schedule;
    for (let n = 0; n < 100; n++) {
      schedule = await api(`/api/workflows/${workflows[0].id}/schedule`);
      if (schedule.occurrences.length) break;
      await new Promise(resolve => setTimeout(resolve, 20));
    }
    assert.equal(schedule.occurrences.length, 1);
    assert.equal(schedule.occurrences[0].missed, 3);
    assert.equal(schedule.schedule.next_at, now + 60000);
    for (const workflow of workflows.slice(1)) {
      const pending = await api(`/api/workflows/${workflow.id}/schedule`);
      assert.deepEqual(pending.occurrences, []);
      assert.equal(pending.schedule.next_at, now - 180000);
      assert.deepEqual(await api(`/api/workflows/${workflow.id}/runs`), []);
    }
  } finally { await app.close(); rmSync(directory, { recursive: true, force: true }); }
});

test('Rust workflow schedules skip busy approvals once and leave offline work due until a provider returns', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-schedule-busy-'));
  const now = Date.now(), database = join(directory, 'state.sqlite');
  const provider = createServer((_req, res) => res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: 'yes' } }] })));
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address();
  assert(address && typeof address !== 'string');
  let app = await startApp({ database, fixedClock: now }, { directory, servers: [provider] });
  async function api(path: string, body?: unknown, status = 200) {
    const response = await fetch(app.origin + path, {
      method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' },
      body: body ? JSON.stringify(body) : undefined,
    });
    const value = await response.json();
    assert.equal(response.status, status, JSON.stringify(value));
    return value;
  }
  async function until(path: string, ready: (value: any) => boolean) {
    for (let attempt = 0; attempt < 200; attempt++) {
      const value = await api(path);
      if (ready(value)) return value;
      await new Promise(resolve => setTimeout(resolve, 20));
    }
    assert.fail(`Schedule state did not settle: ${path}`);
  }
  try {
    const agent = await api('/api/agents', { id: randomUUID(), runtime: 'api', name: 'Owner', model: 'fixture', instructions: 'Help.' }, 201);
    const list = `/api/agents/${agent.id}/workflows`;
    const out = { id: 'out', kind: 'output' };
    const approval = await api(list, { id: randomUUID(), revision: 0, name: 'Approval', graph: { entry: 'review', maxSteps: 2, nodes: [{ id: 'review', kind: 'approval', prompt: 'Accept?', yes: 'out', no: 'out' }, out] } });
    const onlineOnly = await api(list, { id: randomUUID(), revision: 0, name: 'Provider condition', graph: { entry: 'check', maxSteps: 2, nodes: [{ id: 'check', kind: 'condition', operator: 'model', model: 'fixture', value: 'Accept?', yes: 'out', no: 'out' }, out] } });
    const parallel = await api(list, { id: randomUUID(), revision: 0, name: 'Parallel', graph: { entry: 'work', maxSteps: 4, nodes: [{ id: 'work', kind: 'parallel', branches: [{ kind: 'model', model: 'fixture', prompt: 'A' }, { kind: 'model', model: 'fixture', prompt: 'B' }], next: 'out' }, out] } });
    const approvalPath = `/api/workflows/${approval.id}/schedule`;
    const settings = { id: randomUUID(), revision: 0, nextAt: now - 180000, intervalSeconds: 60, input: 'Scheduled input', enabled: true };
    await api(approvalPath, { ...settings, id: randomUUID(), intervalSeconds: 0 }, 400);
    for (const workflow of [onlineOnly, parallel]) {
      const path = `/api/workflows/${workflow.id}/schedule`;
      await api(path, { ...settings, id: randomUUID(), nextAt: now - 240000, intervalSeconds: null });
      assert.deepEqual((await api(path)).occurrences, []);
    }
    const db = testDatabase(database);
    try {
      db.exec('BEGIN IMMEDIATE');
      for (let index = 0; index < 100; index++) {
        const id = randomUUID();
        db.prepare("INSERT INTO workflows(id,agent,name,revision) VALUES (?,?,'Offline',1)").run(id, agent.id);
        db.prepare('INSERT INTO workflow_versions SELECT ?,revision,graph FROM workflow_versions WHERE workflow=?').run(id, onlineOnly.id);
        db.prepare("INSERT INTO workflow_schedules VALUES (?,1,?,NULL,'Offline input',1)").run(id, now - 300000);
      }
      db.exec('COMMIT');
    } finally { db.close(); }

    await api(approvalPath, settings);
    await api(approvalPath, { ...settings, id: randomUUID() }, 409);
    const first = await until(approvalPath, data => data.occurrences.length === 1);
    assert.equal(first.occurrences[0].missed, 3);
    assert.equal(first.occurrences[0].reason, null);
    assert.equal(first.schedule.next_at, now + 60000);
    const runPath = `/api/workflow-runs/${first.occurrences[0].run}`;
    const pending = await until(runPath, run => run.status === 'approval');
    assert.equal(pending.input, 'Scheduled input');

    await app.close(); app = await startApp({ database, fixedClock: now + 60000 });
    const busy = await until(approvalPath, data => data.occurrences.length === 2);
    assert.equal(busy.occurrences[0].scheduled_at, now + 60000);
    assert.equal(busy.occurrences[0].reason, 'busy');
    await api(runPath + '/approve', { id: randomUUID(), revision: pending.revision, accepted: true });
    await until(runPath, run => run.status === 'done');
    await api(approvalPath, { ...settings, id: randomUUID(), revision: busy.schedule.revision, nextAt: now + 60000 }, 409);
    await api(approvalPath, { ...settings, id: randomUUID(), revision: busy.schedule.revision, nextAt: now + 120000, intervalSeconds: null });

    await app.close();
    const cleanup = testDatabase(database);
    try {
      cleanup.exec(`DELETE FROM workflow_schedules WHERE workflow IN (SELECT id FROM workflows WHERE name='Offline');
        DELETE FROM workflow_versions WHERE workflow IN (SELECT id FROM workflows WHERE name='Offline');
        DELETE FROM workflows WHERE name='Offline';`);
    } finally { cleanup.close(); }
    app = await startApp({ database, fixedClock: now + 120000, baseURL: `http://127.0.0.1:${address.port}/v1` });
    const once = await until(approvalPath, data => data.occurrences.length === 3);
    assert.equal(once.schedule.enabled, 0);
    assert.equal((await api(`/api/workflows/${approval.id}/runs`)).length, 2);
    for (const workflow of [onlineOnly, parallel]) {
      const path = `/api/workflows/${workflow.id}/schedule`;
      const admitted = await until(path, data => data.occurrences.length === 1);
      assert(admitted.occurrences[0].run);
      assert.equal(admitted.schedule.enabled, 0);
      assert.equal((await api(`/api/workflows/${workflow.id}/runs`)).length, 1);
      const run = await until(`/api/workflow-runs/${admitted.occurrences[0].run}`, data => data.status === 'done');
      assert.equal(run.input, settings.input);
      assert.equal(run.cursor, 'out');
      if (workflow.id === onlineOnly.id) assert.equal(run.last, settings.input);
      else assert.deepEqual(JSON.parse(run.last), [{ branch: 1, result: 'yes' }, { branch: 2, result: 'yes' }]);
    }
  } finally {
    await app.close();
    provider.closeAllConnections();
    await new Promise<void>(resolve => provider.close(() => resolve()));
    rmSync(directory, { recursive: true, force: true });
  }
});

test('Rust cron preview preserves UTC ordering through midnight folds and rejects dense schedules', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-rust-cron-'));
  const now = Date.parse('2026-11-01T00:00:00Z');
  const options = { database: join(directory, 'state.sqlite'), fixedClock: now };
  const app = await startApp(options);
  try {
    const preview = async (expression: string) => fetch(`${app.origin}/api/routine-schedule?zone=America%2FHavana&schedule=${encodeURIComponent(JSON.stringify({ kind: 'cron', expression }))}`);
    const response = await preview('0,30 0 * * *'); assert.equal(response.status, 200);
    const value = await response.json();
    assert.deepEqual(value.times, [Date.parse('2026-11-01T04:00:00Z'), Date.parse('2026-11-01T04:30:00Z'), Date.parse('2026-11-02T05:00:00Z')]);
    const dense = await preview('* * * * *'); assert.equal(dense.status, 400);
    assert.deepEqual(await dense.json(), { error: 'Cron interval must be at least five minutes.' });
  } finally { await app.close(); rmSync(directory, { recursive: true, force: true }); }
});
