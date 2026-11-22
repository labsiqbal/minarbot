import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { createServer } from 'node:http';
import { testDatabase } from './test-support/database.ts';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { startApp } from './test-bridge.ts';
import { memoryCommand, memoryDraft } from './src/memory-command.ts';

test('automatic memory selection is atomic, user-protected, scoped and durable across resume', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-selection-'));
  let args: unknown = {}, hold = false, held = false, result: any, initial = '';
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const data = JSON.parse(raw), results = data.messages.filter((m: any) => m.role === 'tool');
    assert(data.tools.some((t: any) => t.function.name === 'select_memory'));
    if (results.length) {
      result = JSON.parse(results.at(-1).content);
      if (hold) { held = true; return; }
      return res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: 'Done' } }] }));
    }
    initial = data.messages[0].content;
    res.end(JSON.stringify({ choices: [{ finish_reason: 'tool_calls', message: { tool_calls: [{ id: 'selection', type: 'function', function: { name: 'select_memory', arguments: JSON.stringify(args) } }] } }] }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const options = { database: join(dir, 'db.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1` };
  let app = await startApp(options, { directory: dir, servers: [provider] });
  async function request(path: string, body?: unknown) {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const value = await response.json(); assert(response.ok, JSON.stringify(value)); return value;
  }
  async function until(read: () => Promise<any>, predicate: (value: any) => boolean) {
    for (let n = 0; n < 150; n++) { const value = await read(); if (predicate(value)) return value; await new Promise(resolve => setTimeout(resolve, 10)); }
    assert.fail('Selection did not settle');
  }
  async function run(path: string) {
    const id = randomUUID(); await request(path, { id, text: 'Select saved context' });
    const rows = await until(() => request(path), rows => ['done', 'failed'].includes(rows.find((r: any) => r.id === id)?.status));
    assert.equal(rows.find((r: any) => r.id === id).status, 'done');
  }
  const note = (text: string, automatic = true) => ({ id: randomUUID(), entry: randomUUID(), revision: 0, text, essential: false, remove: false, automatic });
  const choose = (id: string, revision: number, essential = true) => ({ id, revision, essential });
  try {
    const a = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'A', model: 'fixture', instructions: 'Help' });
    const c = await request('/api/channels', { id: randomUUID(), name: 'C', instructions: 'Help', lead: a.id, members: [a.id] });
    const path = `/api/agents/${a.id}/turns`, memory = `/api/agents/${a.id}/memory`;
    const first = note('AUTOMATIC_CONTEXT'), second = note('MANUAL_CONTEXT', false), third = note('OTHER_AUTOMATIC');
    await request(memory, first); await request(memory, second); await request(memory, third);
    args = { items: [choose(first.entry, 1), choose(second.entry, 1)] }; await run(path); assert.equal(result.status, 409);
    assert((await request(memory)).items.every((r: any) => r.essential === 0 && r.revision === 1));
    for (const [items, status] of [
      [[choose(first.entry, 1), choose(first.entry, 1)], 400],
      [[choose(first.entry, 2)], 409],
      [[choose(first.entry, 1), choose(randomUUID(), 1)], 404],
      [[], 400],
    ] as const) { args = { items }; await run(path); assert.equal(result.status, status); }
    args = { items: [choose(first.entry, 1)] }; await run(`/api/channels/${c.id}/turns`); assert.equal(result.status, 404);
    args = { items: [choose(first.entry, 1), choose(third.entry, 1)] }; hold = true;
    const task = randomUUID(); await request(path, { id: task, text: 'Select and interrupt' }); await until(async () => held, Boolean);
    assert.doesNotMatch(initial, /AUTOMATIC_CONTEXT/); assert.equal(result.items[0].revision, 2);
    await app.close(); app = await startApp(options); hold = false;
    await request(memory, { ...first, id: randomUUID(), revision: 2, automatic: false, essential: false });
    const stopped = (await request(path)).find((r: any) => r.id === task);
    await request(`${path}/${task}/resume`, { id: randomUUID(), revision: stopped.revision, acknowledgeUncertain: true });
    await until(() => request(path), rows => rows.find((r: any) => r.id === task)?.status === 'done');
    assert.equal(result.items[0].revision, 2);
    let current = (await request(memory)).items.find((r: any) => r.id === first.entry); assert.equal(current.essential, 0); assert.equal(current.selection_locked, 1); assert.equal(current.revision, 3);
    args = { items: [choose(first.entry, 3)] }; await run(path); assert.equal(result.status, 409); assert.match(initial, /OTHER_AUTOMATIC/); assert.doesNotMatch(initial, /AUTOMATIC_CONTEXT/);
    await request(memory, { ...first, id: randomUUID(), revision: 3, automatic: true });
    args = { items: [choose(first.entry, 4)] }; await run(path); assert.equal(result.items[0].essential, 1);
    const largeA = note('界'.repeat(2000)), largeB = note('文'.repeat(2000)); await request(memory, largeA); await request(memory, largeB);
    args = { items: [choose(first.entry, 5, false), choose(largeA.entry, 1), choose(largeB.entry, 1)] }; await run(path); assert.equal(result.status, 413);
    const rows = (await request(memory)).items; assert.equal(rows.find((r: any) => r.id === first.entry).essential, 1); assert.equal(rows.find((r: any) => r.id === largeA.entry).revision, 1);
    await request(memory, { ...third, id: randomUUID(), revision: 2, text: '', essential: false, remove: true });
    args = { items: [choose(third.entry, 3)] }; await run(path); assert.equal(result.status, 404);
    const old = note('LEGACY'); delete (old as { automatic?: boolean }).automatic; const receipt = await request(memory, old);
    await app.close();
    const db = testDatabase(options.database);
    try {
      // Remove Phase 1's mirrors before reconstructing the legacy note schema.
      db.exec([
        'DROP TRIGGER memory_note_insert',
        'DROP TRIGGER memory_note_update',
        'DROP TRIGGER memory_note_delete',
        'DROP VIEW memory_legacy_rows',
        "DELETE FROM memory_units WHERE origin='legacy'",
        'DELETE FROM memory_imports WHERE version=1',
        'ALTER TABLE memories DROP COLUMN selection_locked',
      ].join('; '));
    } finally { db.close(); }
    app = await startApp(options);
    assert.deepEqual(await request(memory, old), receipt); assert((await request(memory)).items.every((r: any) => r.selection_locked === 1));
    const legacyDraft = { entry: { id: first.entry, text: first.text, essential: 0, revision: 1 }, text: 'Draft', essential: false };
    assert.deepEqual(memoryDraft(legacyDraft), legacyDraft);
    const draft = { ...legacyDraft, entry: { ...legacyDraft.entry, selection_locked: 0 }, automatic: true };
    assert.deepEqual(memoryDraft(JSON.parse(JSON.stringify(draft))), draft);
    assert.deepEqual(memoryCommand(old), old);
    assert.throws(() => memoryCommand({ ...old, automatic: 'yes' })); assert.throws(() => memoryDraft({ ...draft, automatic: 1 }));
  } finally { await app.close(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(dir, { recursive: true, force: true }); }
});
