import { testDatabase } from './test-support/database.ts';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { createServer, type ServerResponse } from 'node:http';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { startApp } from './test-bridge.ts';

test('queue edit holds survive restart, preserve order and original receipt, reject competing editors and completed cancellations', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-queue-edit-'));
  const calls: string[] = []; let held: ServerResponse | undefined;
  const finish = (res: ServerResponse) => res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: 'Done' } }] }));
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const text = JSON.parse(raw).messages.at(-1).content; calls.push(text);
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
  async function until(check: () => Promise<boolean>) { for (let n=0; n<200; n++) { if (await check()) return; await new Promise(resolve => setTimeout(resolve, 10)); } assert.fail('Timed out'); }
  try {
    const a = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'A', model: 'fixture', instructions: 'Help' });
    const b = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'B', model: 'fixture', instructions: 'Help' });
    const side = await request(`/api/agents/${a.id}/threads`, { id: randomUUID(), name: 'Side' });
    const channel = await request('/api/channels', { id: randomUUID(), name: 'Channel', lead: a.id, members: [a.id], instructions: 'Help' });
    for (const path of [`/api/agents/${a.id}/turns`, `/api/threads/${side.id}/turns`, `/api/channels/${channel.id}/turns`]) {
      held = undefined; const blocker = randomUUID(); await request(path, { id: blocker, text: 'BLOCK' }); await until(async () => !!held);
      const fileId = randomUUID(), filePath = path.replace(/\/turns$/, '/files') + '/' + fileId;
      const upload = await fetch(app.origin + filePath, { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/octet-stream', 'X-File-Name': 'note.txt' }, body: 'Saved attachment' }); assert.equal(upload.status, 200);
      const paginated = path.startsWith('/api/agents/');
      if (paginated) {
        for (let n = 0; n < 100; n++) {
          const response = await fetch(app.origin + path.replace(/\/turns$/, '/files') + '/' + randomUUID(), { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/octet-stream', 'X-File-Name': `newer-${n}.txt` }, body: 'newer' }); assert.equal(response.status, 200);
        }
        const firstPage = await request(path.replace(/\/turns$/, '/files'));
        assert.equal(firstPage.length, 100); assert(!firstPage.some((file: any) => file.id === fileId));
        const secondPage = await request(path.replace(/\/turns$/, '/files') + '?before=' + firstPage.at(-1).seq);
        assert.deepEqual(secondPage.map((file: any) => file.id), [fileId]);
      }
      const originalFiles = paginated ? [] : [fileId], editedFiles = paginated ? [fileId] : [];
      const command = { id: randomUUID(), text: 'ORIGINAL', files: originalFiles }, tail = randomUUID();
      const turn = await request(path, command); await request(path, { id: tail, text: 'TAIL' });
      const editPath = `${path}/${turn.id}/edit`, hold = { id: randomUUID(), editor: randomUUID(), revision: turn.revision, action: 'hold' };
      const accepted = await request(editPath, hold); assert.deepEqual(accepted.files, originalFiles); assert.deepEqual(await request(editPath, hold), accepted);
      const renew = { ...hold, id: randomUUID(), revision: accepted.revision, action: 'renew' };
      assert.equal((await request(editPath, renew)).revision, accepted.revision);
      assert.deepEqual(await request(editPath, renew), await request(editPath, renew));
      await request(editPath, { ...renew, id: randomUUID(), editor: randomUUID() }, 409);
      await request(editPath, { ...renew, id: randomUUID(), revision: turn.revision }, 409);
      await request(`/api/agents/${b.id}/turns/${turn.id}/edit`, hold, 404);
      await request(editPath, { ...hold, id: randomUUID(), editor: randomUUID(), revision: accepted.revision }, 409);
      await request(`${path}/${turn.id}/pause`, { id: randomUUID(), revision: accepted.revision }, 409);
      const publicRows = await request(path); assert.equal(publicRows.find((row: any) => row.id === turn.id).editing, true); assert(!JSON.stringify(publicRows).includes(hold.editor));
      if (path.includes('/channels/')) {
        await request(`/api/agents/${a.id}/visibility`, { id: randomUUID(), revision: 0, hidden: true });
        assert.equal((await request(path)).find((row: any) => row.id === turn.id).revision, accepted.revision);
        await request(`/api/agents/${a.id}/visibility`, { id: randomUUID(), revision: 1, hidden: false });
      }
      finish(held!); await until(async () => (await request(path)).find((row: any) => row.id === blocker)?.status === 'done');
      await app.close(); app = await startApp(options);
      assert.equal((await request(path)).find((row: any) => row.id === tail).status, 'queued');
      await request(editPath, { id: randomUUID(), editor: hold.editor, revision: accepted.revision, action: 'save', text: 'WRONG', files: [randomUUID()] }, 404);
      const save = { id: randomUUID(), editor: hold.editor, revision: accepted.revision, action: 'save', text: 'CHANGED', files: editedFiles };
      const result = await request(editPath, save); assert.deepEqual(await request(editPath, save), result);
      await until(async () => (await request(path)).find((row: any) => row.id === tail)?.status === 'done');
      assert.equal(calls.at(-3), 'BLOCK'); assert.equal(calls.at(-1), 'TAIL');
      if (paginated) { assert.match(calls.at(-2)!, /CHANGED/); assert.match(calls.at(-2)!, /Saved attachment/); } else assert.equal(calls.at(-2), 'CHANGED');
      const edited = await request(path, command); assert.equal(edited.text, 'CHANGED'); assert.deepEqual(JSON.parse(edited.attachments), editedFiles);
      assert.equal(await (await fetch(app.origin + filePath)).text(), 'Saved attachment');
      await request(path, { ...command, text: 'CHANGED' }, 409);
      await request(editPath, { ...save, id: randomUUID(), action: 'cancel', text: undefined, files: undefined }, 409);
    }
    const path = `/api/agents/${a.id}/turns`; held = undefined;
    await request(path, { id: randomUUID(), text: 'BLOCK' }); await until(async () => !!held);
    const turn = await request(path, { id: randomUUID(), text: 'UNCHANGED' });
    const editPath = `${path}/${turn.id}/edit`, editor = randomUUID();
    const hold = await request(editPath, { id: randomUUID(), editor, revision: turn.revision, action: 'hold' });
    const cancel = { id: randomUUID(), editor, revision: hold.revision, action: 'cancel' };
    const result = await request(editPath, cancel); assert.deepEqual(await request(editPath, cancel), result);
    await request(editPath, { id: randomUUID(), editor, revision: result.revision, action: 'save', text: 'NEVER', files: [] }, 409);
    finish(held!); await until(async () => (await request(path)).find((row: any) => row.id === turn.id)?.status === 'done');
    assert.equal(calls.at(-1), 'UNCHANGED');
  } finally { await app.close(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(dir, { recursive: true, force: true }); }
});

test('abandoned edit lease expires and drains its message and following queue in order', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-abandoned-edit-'));
  let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  const calls: string[] = [];
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const text = JSON.parse(raw).messages.at(-1).content; calls.push(text);
    if (text === 'BLOCK') await gate;
    res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: 'Done' } }] }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const database = join(dir, 'db.sqlite');
  const options = { database, baseURL: `http://127.0.0.1:${address.port}/v1` };
  let app = await startApp(options, { directory: dir, servers: [provider] });
  const request = async (path: string, data?: unknown) => {
    const response = await fetch(app.origin + path, { method: data ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: data ? JSON.stringify(data) : undefined });
    assert(response.ok, await response.clone().text()); return response.json();
  };
  try {
    const agent = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Abandoned', model: 'fixture', instructions: 'Synthetic' });
    const path = `/api/agents/${agent.id}/turns`;
    await request(path, { id: randomUUID(), text: 'BLOCK' });
    const turn = await request(path, { id: randomUUID(), text: 'ABANDONED' });
    const tail = await request(path, { id: randomUUID(), text: 'TAIL' });
    const hold = { id: randomUUID(), editor: randomUUID(), revision: turn.revision, action: 'hold' };
    await request(`${path}/${turn.id}/edit`, hold);
    release();
    for (let n = 0; n < 100 && (await request(path))[0]?.status !== 'done'; n++) await new Promise(resolve => setTimeout(resolve, 10));
    await app.close();
    const db = testDatabase(database);
    db.prepare('UPDATE queue_edits SET expires_at=0 WHERE editor=?').run(hold.editor);
    db.close(); app = await startApp(options);
    let rows: any[] = [];
    for (let n = 0; n < 100; n++) {
      rows = await request(path);
      if (rows.find(row => row.id === tail.id)?.status === 'done') break;
      await new Promise(resolve => setTimeout(resolve, 25));
    }
    assert.equal(rows.find(row => row.id === turn.id)?.editing, false, 'Abandoned editor must release its queue hold');
    assert.equal(rows.find(row => row.id === tail.id)?.status, 'done', 'Queue must drain after abandoned edit');
    assert.deepEqual(calls, ['BLOCK', 'ABANDONED', 'TAIL']);
    const stale = await fetch(`${app.origin}${path}/${turn.id}/edit`, { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify({ ...hold, id: randomUUID(), revision: 2, action: 'save', text: 'STALE', files: [] }) });
    assert.equal(stale.status, 409);
  } finally { release(); await app.close(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(dir, { recursive: true, force: true }); }
});

test('Rust expires migrated permanent queue holds and fences the prior revision once', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-queue-migration-'));
  const database = join(directory, 'state.sqlite');
  let app = await startApp({ database });
  const legacy = randomUUID(), current = randomUUID();
  try {
    await app.close();
    const seed = testDatabase(database);
    try {
      seed.exec(`INSERT INTO agents(id,name,model,instructions) VALUES ('fixture','Fixture','fixture','Help');
        ALTER TABLE queue_edits DROP COLUMN expires_at;`);
      seed.prepare("INSERT INTO turns(id,agent,text,status,created,paused) VALUES (?,'fixture','Legacy','queued','2026-01-01',1)").run(legacy);
      seed.prepare("INSERT INTO queue_edits(editor,turn,state,original) VALUES ('legacy-editor',?,'held','{}')").run(legacy);
    } finally { seed.close(); }

    app = await startApp({ database });
    const db = testDatabase(database);
    try {
      const edit = db.prepare('SELECT state,expires_at FROM queue_edits WHERE turn=?');
      const revision = db.prepare('SELECT revision FROM turns WHERE id=?');
      assert.deepEqual({ ...edit.get(legacy) }, { state: 'cancelled', expires_at: 0 });
      assert.equal(revision.get(legacy)?.revision, 2);

      db.prepare("INSERT INTO turns(id,agent,text,status,created,paused) VALUES (?,'fixture','Current','queued','2026-01-01',1)").run(current);
      const deadline = Date.now() + 30000;
      db.prepare("INSERT INTO queue_edits(editor,turn,state,original,expires_at) VALUES ('current-editor',?,'held','{}',?)").run(current, deadline);
      await app.close(); app = await startApp({ database });
      assert.equal(revision.get(legacy)?.revision, 2);
      assert.deepEqual({ ...edit.get(current) }, { state: 'held', expires_at: deadline });
      assert.equal(revision.get(current)?.revision, 1);

      await app.close();
      db.prepare('UPDATE queue_edits SET expires_at=0 WHERE turn=?').run(current);
      app = await startApp({ database });
      assert.equal(edit.get(current)?.state, 'cancelled');
      assert.equal(revision.get(current)?.revision, 2);
    } finally { db.close(); }
  } finally { await app.close(); rmSync(directory, { recursive: true, force: true }); }
});
