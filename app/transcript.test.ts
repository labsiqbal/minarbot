import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { startApp } from './test-bridge.ts';

test('turn list exposes scoped read-only activity metadata in direct, thread and channel views', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-transcript-'));
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const part of req) raw += part;
    const body = JSON.parse(raw);
    if (body.model === 'primary') { res.writeHead(503); res.end('private upstream detail'); return; }
    const afterTool = body.messages.at(-1).role === 'tool';
    res.setHeader('Content-Type', 'application/json');
    res.end(JSON.stringify({ choices: [{ finish_reason: afterTool ? 'stop' : 'tool_calls', message: afterTool ? { content: '## Finished\n\nSafe answer.' } : { tool_calls: [{ id: randomUUID(), type: 'function', function: { name: 'remember_memory', arguments: JSON.stringify({ text: `Synthetic transcript check ${randomUUID()}` }) } }] } }] }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const app = await startApp({ database: join(directory, 'db.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1`, apiKey: 'private-provider-key' }, { directory, servers: [provider] });
  async function request(path: string, body?: unknown) {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const value = await response.json(); assert.equal(response.ok, true, JSON.stringify(value)); return value;
  }
  try {
    const agent = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Transcript', model: 'primary', backupModel: 'backup', instructions: 'Test' });
    const foreign = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Other', model: 'backup', instructions: 'Test' });
    const thread = await request(`/api/agents/${agent.id}/threads`, { id: randomUUID(), name: 'Side' });
    const channel = await request('/api/channels', { id: randomUUID(), name: 'Room', instructions: 'Test', members: [agent.id], lead: agent.id });
    for (const path of [`/api/agents/${agent.id}/turns`, `/api/threads/${thread.id}/turns`, `/api/channels/${channel.id}/turns`]) {
      const id = randomUUID(); await request(path, { id, text: 'Test activity', ...(path.includes('/channels/') ? { recipient: agent.id } : {}) });
      let row;
      for (let attempt = 0; attempt < 200; attempt++) {
        row = (await request(path)).find((row: { id: string }) => row.id === id);
        if (['done', 'failed'].includes(row?.status)) break;
        await new Promise(resolve => setTimeout(resolve, 15));
      }
      assert.equal(row.status, 'done');
      assert.equal(row.activity.calls.length, 3);
      assert.equal(row.activity.fallback_model, 'backup');
      assert.deepEqual(row.activity.tools.map((tool: { name: string }) => tool.name), ['remember_memory']);
      assert.equal(row.activity.tools[0].failed, 0);
      assert.equal(row.activity.tools[0].after_action, 0);
      assert.deepEqual(row.coordination, []);
      for (const call of row.activity.calls) { assert(Number.isFinite(Date.parse(call.created))); assert(Number.isFinite(Date.parse(call.finished))); assert(!('account' in call)); assert(!('origin' in call)); }
      assert.equal(JSON.stringify(row.activity).includes('private'), false);
      const focused = await request(`${path}?focus=${id}`); assert.deepEqual(focused[0].activity, row.activity);
    }
    assert.deepEqual(await request(`/api/agents/${foreign.id}/turns`), []);
  } finally { await app.close(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(directory, { recursive: true, force: true }); }
});

test('Stop timestamps survive repeated cancellation and server restart', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-stop-'));
  const provider = createServer((_req, res) => {
    res.writeHead(200, { 'Content-Type': 'text/event-stream' });
    res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { content: 'Synthetic partial reply.' }, finish_reason: null }] })}\n\n`);
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const options = { database: join(directory, 'db.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1`, stream: true, apiKey: 'synthetic-key' };
  let app = await startApp(options, { directory, servers: [provider] });
  async function request(path: string, body?: unknown) {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    assert(response.ok, await response.text().then(text => text || response.statusText));
  }
  try {
    const agent = randomUUID(), turn = randomUUID();
    await request('/api/agents', { runtime: 'api', id: agent, name: 'Stop check', model: 'synthetic', instructions: 'Test' });
    const path = `/api/agents/${agent}/turns`;
    await request(path, { id: turn, text: 'Hold reply' });
    let running;
    for (let attempt = 0; attempt < 100; attempt++) {
      [running] = await (await fetch(app.origin + path)).json();
      if (running.partial?.text) break;
      await delay(15);
    }
    assert(running.partial?.text, JSON.stringify(running)); assert(running.activity.started_at);
    assert.equal(running.activity.stopped_at, null);
    await delay(1100);
    await request(`${path}/${turn}/cancel`, { id: randomUUID() });
    const [stopped] = await (await fetch(app.origin + path)).json();
    assert.equal(stopped.activity.started_at, running.activity.started_at);
    assert(Date.parse(stopped.activity.stopped_at) - Date.parse(stopped.activity.started_at) >= 1100);
    await request(`${path}/${turn}/cancel`, { id: randomUUID() });
    await app.close(); app = await startApp(options);
    const [reloaded] = await (await fetch(app.origin + path)).json();
    assert.equal(reloaded.activity.started_at, stopped.activity.started_at);
    assert.equal(reloaded.activity.stopped_at, stopped.activity.stopped_at);
  } finally { await app.close(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(directory, { recursive: true, force: true }); }
});
