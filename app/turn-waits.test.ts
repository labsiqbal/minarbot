import test from 'node:test';
import assert from 'node:assert/strict';
import { createHmac, randomUUID } from 'node:crypto';
import { createServer } from 'node:http';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { testDatabase } from './test-support/database.ts';
import { startApp } from './test-bridge.ts';

test('routine waits preserve artifacts and reject events cancelled before upload completes', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-routine-events-'));
  let filesRequested = 0, completed = 0;
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const { messages, tools } = JSON.parse(raw);
    assert(tools.some((tool: any) => tool.function.name === 'wait_for_event'));
    const calls = messages.flatMap((m: any) => m.tool_calls ?? []);
    const received = calls.some((call: any) => call.function.name === 'wait_for_event');
    let name = 'wait_for_event', id = 'wait', args: unknown = { event: 'ready', timeoutSeconds: 3600 };
    if (!calls.length) { filesRequested++; name = 'create_text_file'; id = 'artifact'; args = { name: 'before.txt', text: 'Evidence before event.' }; }
    if (received) {
      completed++;
      assert.deepEqual(calls.map((call: any) => call.function.name), ['create_text_file', 'wait_for_event']);
      assert.equal(JSON.parse(messages.filter((m: any) => m.role === 'tool').at(-1).content).payload, 'ready-data');
    }
    res.end(JSON.stringify({ choices: [{ finish_reason: received ? 'stop' : 'tool_calls', message: received ? { content: 'Routine resumed.' } : { tool_calls: [{ id, type: 'function', function: { name, arguments: JSON.stringify(args) } }] } }] }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const options = { database: join(dir, 'state.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1` };
  let app = await startApp(options, { directory: dir, servers: [provider] });
  async function request(path: string, body?: unknown, expected?: number) {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const value = await response.json(); if (expected) assert.equal(response.status, expected, JSON.stringify(value)); else assert(response.ok, JSON.stringify(value)); return value;
  }
  async function until(path: string, predicate: (rows: any[]) => boolean) {
    for (let n = 0; n < 200; n++) { const rows = await request(path); if (predicate(rows)) return rows; await new Promise(resolve => setTimeout(resolve, 10)); }
    assert.fail('Routine event state did not settle');
  }
  const body = JSON.stringify({ event: 'ready', payload: 'ready-data' });
  function headers(secret: string, id: string) {
    const time = String(Math.floor(Date.now() / 1000));
    return { 'Content-Type': 'application/json', 'X-Minarbot-Time': time, 'X-Minarbot-Id': id, 'X-Minarbot-Signature': createHmac('sha256', secret).update(`${time}\n${id}\n${body}`).digest('hex') };
  }
  try {
    const agent = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Routine', model: 'fixture', instructions: 'Help' });
    const routine = await request(`/api/agents/${agent.id}/routines`, { id: randomUUID(), name: 'Event routine', prompt: 'Wait for event', nextAt: Date.now() + 3600000, intervalSeconds: null, enabled: false });
    await request(`/api/routines/${routine.id}/test`, { id: randomUUID(), revision: routine.revision });
    const runsPath = `/api/routines/${routine.id}/runs`;
    const [run] = await until(runsPath, rows => rows.length === 1);
    const turnsPath = `/api/threads/${run.thread}/turns`;
    const [waiting] = await until(turnsPath, rows => rows[0]?.events?.length === 1);
    assert.equal(waiting.generatedFiles.length, 1); assert.equal(filesRequested, 1);
    const connectionPath = `/api/turn-waits/${waiting.events[0].id}`, connection = await request(connectionPath);
    await request(`/api/routines/${routine.id}/test`, { id: randomUUID(), revision: routine.revision }, 409);
    assert.equal((await request(`/api/agents/${agent.id}/turns`)).length, 0);
    await app.close(); app = await startApp(options);
    const delivery = randomUUID();
    const responses = await Promise.all([1, 2].map(() => fetch(app.origin + connectionPath, { method: 'POST', headers: headers(connection.secret, delivery), body })));
    assert(responses.every(response => response.status === 200)); assert.deepEqual(await responses[0].json(), await responses[1].json());
    const [done] = await until(turnsPath, rows => rows[0]?.status === 'done');
    assert.equal(completed, 2); assert.equal(filesRequested, 1); assert.deepEqual(done.generatedFiles, waiting.generatedFiles);
    const download = await fetch(`${app.origin}/api/threads/${run.thread}/files/${done.generatedFiles[0].id}`); assert.equal(await download.text(), 'Evidence before event.');
    assert.equal((await request(runsPath))[0].status, 'done');
    await request(`/api/routines/${routine.id}/test`, { id: randomUUID(), revision: routine.revision });
    const [next] = await until(runsPath, rows => rows.length === 2);
    const nextPath = `/api/threads/${next.thread}/turns`, [nextWaiting] = await until(nextPath, rows => rows[0]?.events?.length === 1);
    const nextConnection = await request(`/api/turn-waits/${nextWaiting.events[0].id}`);
    const { request: httpRequest } = await import('node:http');
    let upload!: ReturnType<typeof httpRequest>;
    const response = new Promise<number | undefined>((resolve, reject) => {
      upload = httpRequest(nextConnection.url, { method: 'POST', headers: { ...headers(nextConnection.secret, randomUUID()), 'Content-Length': Buffer.byteLength(body) } }, res => { res.resume(); res.on('end', () => resolve(res.statusCode)); });
      upload.on('error', reject);
    });
    void response.catch(() => {});
    try {
      await new Promise<void>((resolve, reject) => upload.write(body.slice(0, 10), error => error ? reject(error) : resolve()));
      await request(`${nextPath}/${nextWaiting.id}/cancel`, { id: randomUUID() });
      upload.end(body.slice(10)); assert.equal(await response, 409);
    } finally { upload.destroy(); }
    assert.equal(completed, 2); assert.equal((await request(runsPath))[0].status, 'cancelled');
    assert.equal((await request(nextPath))[0].events[0].status, 'closed');
    const scheduled = await request(`/api/agents/${agent.id}/routines`, { id: randomUUID(), name: 'Scheduled wait', prompt: 'Wait from schedule', nextAt: Date.now() - 1, intervalSeconds: null, enabled: true });
    const scheduledRuns = `/api/routines/${scheduled.id}/runs`, [scheduledRun] = await until(scheduledRuns, rows => rows.length === 1);
    const scheduledTurns = `/api/threads/${scheduledRun.thread}/turns`;
    const [scheduledWait] = await until(scheduledTurns, rows => rows[0]?.events?.length === 1);
    await app.close(); const db = testDatabase(options.database); db.prepare('UPDATE turn_waits SET deadline=0 WHERE turn=?').run(scheduledWait.id); db.close();
    app = await startApp(options);
    const [expiredRun] = await until(scheduledRuns, rows => rows[0]?.status === 'failed');
    assert.match(expiredRun.error, /Event wait timed out/); assert.equal(completed, 2);
    assert.equal((await request(scheduledTurns))[0].generatedFiles.length, 1);

  } finally { await app.close(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(dir, { recursive: true, force: true }); }
});
