import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { createServer, type ServerResponse } from 'node:http';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { startApp } from './test-bridge.ts';

test('queue order survives restart, preserves transcript order, blocks edits and scopes, supplies reordered context', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-queue-edit-'));
  const calls: string[] = []; const summaries: any[] = []; let held: ServerResponse | undefined;
  const finish = (res: ServerResponse) => res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: 'Done' } }] }));
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const messages = JSON.parse(raw).messages;
    if (messages[0].content.startsWith('Summarize conversation history')) { summaries.push(JSON.parse(messages[1].content)); return res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: 'REORDERED SUMMARY' } }] })); }
    const text = messages.at(-1).content; calls.push(text);
    if (text === 'EARLY LAST') assert(messages.some((message: any) => typeof message.content === 'string' && message.content.includes('REORDERED SUMMARY')));
    if (text === 'FIRST') assert(messages.some((message: any) => message.role === 'user' && message.content === 'SECOND'));
    if (text === 'BLOCK') held = res; else finish(res);
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const options = { database: join(dir, 'db.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1`, concurrency: 1 };
  let app = await startApp(options, { directory: dir, servers: [provider] });
  async function request(path: string, body?: unknown, status?: number): Promise<any> {
    const res = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const result = await res.json(); if (status) assert.equal(res.status, status, JSON.stringify(result)); else assert(res.ok, JSON.stringify(result)); return result;
  }
  async function until(check: () => Promise<boolean>, label = 'queue progress') { for (let n=0; n<200; n++) { if (await check()) return; await new Promise(resolve => setTimeout(resolve, 10)); } assert.fail(`Timed out waiting for ${label}`); }
  try {
    const a = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'A', model: 'fixture', instructions: 'Help' });
    const side = await request(`/api/agents/${a.id}/threads`, { id: randomUUID(), name: 'Side' });
    const channel = await request('/api/channels', { id: randomUUID(), name: 'Channel', lead: a.id, members: [a.id], instructions: 'Help' });
    for (const path of [`/api/agents/${a.id}/turns`, `/api/threads/${side.id}/turns`, `/api/channels/${channel.id}/turns`]) {
      held = undefined; const blocker = randomUUID(); await request(path, { id: blocker, text: 'BLOCK' }); await until(async () => !!held);
      const first = await request(path, { id: randomUUID(), text: 'FIRST' }), second = await request(path, { id: randomUUID(), text: 'SECOND' });
      const endpoint = path.replace(/\/turns$/, '/queue');
      const swap = { id: randomUUID(), target: first.id, other: second.id, revision: first.revision, otherRevision: second.revision };
      const editor = randomUUID(), hold = await request(`${path}/${first.id}/edit`, { id: randomUUID(), editor, revision: first.revision, action: 'hold' });
      await request(endpoint, { ...swap, revision: hold.revision }, 409);
      const cancelled = await request(`${path}/${first.id}/edit`, { id: randomUUID(), editor, revision: hold.revision, action: 'cancel' });
      swap.revision = cancelled.revision;
      const result = await request(endpoint, swap); assert.deepEqual(await request(endpoint, swap), result);
      assert.deepEqual((await request(endpoint)).turns.map((row: any) => row.id), [second.id, first.id]);
      await request(endpoint, { ...swap, id: randomUUID() }, 409);
      await request(`/api/agents/${randomUUID()}/queue`, { ...swap, id: randomUUID() }, 404);
      await app.close(); app = await startApp(options);
      await until(async () => (await request(path)).find((row: any) => row.id === first.id)?.status === 'done');
      assert.deepEqual(calls.slice(-3), ['BLOCK', 'SECOND', 'FIRST']);
      const history = await request(path); assert(history.findIndex((row: any) => row.id === first.id) < history.findIndex((row: any) => row.id === second.id));
      assert.deepEqual((await request(endpoint)).turns, []);
      assert.deepEqual(await request(endpoint, swap), result);
    }
    const path = `/api/agents/${a.id}/turns`; held = undefined;
    await request(path, { id: randomUUID(), text: 'BLOCK' }); await until(async () => !!held);
    const early = await request(path, { id: randomUUID(), text: 'EARLY LAST' }); let last: any;
    for (let n = 0; n < 45; n++) last = await request(path, { id: randomUUID(), text: `LATE ${n}` });
    await request(path.replace(/\/turns$/, '/queue'), { id: randomUUID(), target: early.id, other: last.id, revision: early.revision, otherRevision: last.revision });
    finish(held!);
    const offset = calls.length;
    const order = ['LATE 44', ...Array.from({ length: 44 }, (_, n) => `LATE ${n}`), 'EARLY LAST'];
    // Wait on provider progress so repeated full-history reads do not contend
    // with every turn's completion and scheduling transactions.
    for (const [index, text] of order.entries()) {
      await until(async () => calls.length > offset + index, text);
      assert.equal(calls[offset + index], text);
    }
    await until(async () => (await request(path)).find((row: any) => row.id === early.id)?.status === 'done', 'final commit');
    assert(summaries.some(summary => summary.messages.some((message: any) => message.id === last.id)));
    held = undefined; await request(path, { id: randomUUID(), text: 'BLOCK' }); await until(async () => !!held);
    const b = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'B', model: 'fixture', instructions: 'Help' });
    const group = await request('/api/channels', { id: randomUUID(), name: 'Large queue', lead: a.id, members: [a.id, b.id], instructions: 'Help' });
    const groupPath = `/api/channels/${group.id}/turns`, groupQueue = `/api/channels/${group.id}/queue`;
    const ids: string[] = [];
    for (let n=0; n<101; n++) { const id = randomUUID(); ids.push(id); await request(groupPath, { id, text: `QUEUED ${n}`, recipient: n < 98 ? a.id : b.id }); }
    const firstPage = await request(groupQueue); assert.equal(firstPage.turns.length,100); assert.equal(firstPage.more,true);
    const cursor = `?after=${firstPage.next_after}&snapshot=${firstPage.snapshot}`;
    const secondPage = await request(groupQueue + cursor); assert.equal(secondPage.turns.length,1); assert.equal(secondPage.next_after,null);
    assert.deepEqual([...firstPage.turns, ...secondPage.turns].map((row: any) => row.id), ids);
    assert.equal(firstPage.turns.at(-1).next.id, secondPage.turns[0].id);
    assert.equal(secondPage.turns[0].previous.id, firstPage.turns.at(-1).id);
    await request(groupQueue + '?after=NaN&snapshot=' + firstPage.snapshot, undefined,400);
    await request(groupQueue + '?after=1', undefined,400);
    await request(`/api/agents/${b.id}/queue` + cursor, undefined,409);
    const left=firstPage.turns[0], right=firstPage.turns[1];
    await request(groupQueue, { id: randomUUID(), target:left.id, other:right.id, revision:left.revision, otherRevision:right.revision });
    await request(groupQueue + cursor, undefined,409);
    const boundary = secondPage.turns[0], neighbor = boundary.previous;
    await request(groupQueue, { id: randomUUID(), target: boundary.id, other: neighbor.id, revision: boundary.revision, otherRevision: neighbor.revision });
    const changed = await request(groupQueue); assert.notEqual(changed.snapshot,firstPage.snapshot); assert.equal(changed.turns[0].id,ids[1]);
  } finally { await app.close(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(dir, { recursive: true, force: true }); }
});
