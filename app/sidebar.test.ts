import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createServer } from 'node:http';
import { testDatabase } from './test-support/database.ts';
import { startApp } from './test-bridge.ts';
import { restoreWorkspace } from './test-support/backup.ts';

const guideId = '00000000-0000-4000-8000-000000000005';
const isUserSummary = (row: { agent: string }) => row.agent !== guideId;

test('sidebar validates unique membership, names, IDs and bounded settings', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-sidebar-valid-'));
  const app = await startApp({ database: join(directory, 'state.sqlite') });
  const post = (path: string, body: unknown) => fetch(app.origin + path, {
    method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body),
  });
  try {
    const agent = randomUUID(), section = { id: randomUUID(), name: ' Work ', agents: [agent] };
    assert((await post('/api/agents', { id: agent, runtime: 'api', name: 'Fixture', model: 'fixture', instructions: 'Help' })).ok);
    const save = (sections: unknown, revision = 0) => post('/api/sidebar', { id: randomUUID(), revision, sections });
    const updated = await save([section]);
    assert.equal(updated.status, 200);
    assert.equal((await updated.json()).sections[0].name, 'Work');
    for (const sections of [
      null, {}, [null], [{ ...section, name: ' ' }], [{ ...section, name: 'a'.repeat(81) }],
      [{ ...section, name: 'a\nb' }], [{ ...section, id: '__proto__' }], [section, section],
      [{ ...section, agents: [agent, agent] }], [section, { ...section, id: randomUUID() }],
      [{ ...section, agents: ['missing'] }], [{ ...section, privilege: 'admin' }], Array(101).fill(section),
    ]) assert.equal((await save(sections, 1)).status, 400, JSON.stringify(sections));
  } finally { await app.close(); rmSync(directory, { recursive: true, force: true }); }
});

test('sidebar HTTP edits retain agents/history through delete, conflicts, retry, restart and backup', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-sidebar-'));
  const provider = createServer(async (request, response) => { for await (const chunk of request) {} response.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: 'History retained.' } }] })); });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const options = { database: join(directory, 'state.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1` };
  let app = await startApp(options, { directory, servers: [provider] }), restored: Awaited<ReturnType<typeof startApp>> | undefined;
  async function request(path: string, body?: unknown, expected = 200) {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const result = await response.json(); assert.equal(response.status, expected, JSON.stringify(result)); return result;
  }
  try {
    assert.deepEqual(await request('/api/sidebar'), { revision: 0, sections: [] });
    const initialSummary = await request('/api/sidebar/summary');
    assert.deepEqual({ ...initialSummary, summaries: initialSummary.summaries.filter(isUserSummary) }, { summaries: [] });
    await request('/api/sidebar/summary?limit=1', undefined, 400);
    assert.equal((await fetch(app.origin + '/api/sidebar/summary', { headers: { Origin: 'https://foreign.test' } })).status, 403);
    const a = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'A', model: 'fixture', instructions: 'Help' }, 201);
    const b = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'B', model: 'fixture', instructions: 'Help' }, 201);
    const historyPath = `/api/agents/${a.id}/turns`;
    await request(historyPath, { id: randomUUID(), text: 'Retain this conversation' }, 202);
    let history;
    for (let attempt = 0; attempt < 100; attempt++) { history = await request(historyPath); if (history[0]?.status === 'done') break; await new Promise(resolve => setTimeout(resolve, 10)); }
    assert.equal(history[0].status, 'done');
    const summary = await request('/api/sidebar/summary');
    assert.deepEqual(summary.summaries.find((row: any) => row.agent === a.id), { agent: a.id, preview: 'History retained.', time: history[0].created, status: 'idle', needs_you: 0 });
    assert.equal((await fetch(app.origin + '/api/sidebar/summary')).headers.get('cache-control'), 'no-store');
    const first = { id: randomUUID(), name: 'Projects', agents: [a.id, b.id] }, second = { id: randomUUID(), name: 'Personal', agents: [] as string[] };
    const create = { id: randomUUID(), revision: 0, sections: [first, second] };
    const initial = await request('/api/sidebar', create);
    assert.equal(initial.revision, 1);
    assert.deepEqual(await request('/api/sidebar', create), initial);
    await request('/api/sidebar', { ...create, sections: [] }, 409);
    const edited = await request('/api/sidebar', { id: randomUUID(), revision: 1, sections: [{ ...second, name: 'Reading', agents: [b.id] }, { ...first, agents: [a.id] }] });
    assert.equal(edited.revision, 2); assert.equal(edited.sections[0].name, 'Reading');
    await request('/api/sidebar', { id: randomUUID(), revision: 1, sections: [] }, 409);
    await request('/api/sidebar', { id: randomUUID(), revision: 2, sections: [{ ...first, agents: [randomUUID()] }] }, 404);
    await request('/api/sidebar', { id: randomUUID(), revision: 2, sections: [{ ...first, agents: [a.id, a.id] }] }, 400);
    await request('/api/sidebar?path=other', undefined, 400);
    const foreign = await fetch(app.origin + '/api/sidebar', { method: 'POST', headers: { Origin: 'https://foreign.test', 'Content-Type': 'application/json' }, body: JSON.stringify({ id: randomUUID(), revision: 2, sections: [] }) });
    assert.equal(foreign.status, 403);
    await app.close();
    const seed = testDatabase(options.database);
    seed.exec("CREATE TRIGGER reject_sidebar_receipt BEFORE INSERT ON sidebar_commands BEGIN SELECT RAISE(ABORT,'fixture'); END");
    seed.close(); app = await startApp(options);
    const remove = { id: randomUUID(), revision: 2, sections: [] };
    await request('/api/sidebar', remove, 500);
    assert.deepEqual(await request('/api/sidebar'), edited);
    await app.close();
    const cleanup = testDatabase(options.database); cleanup.exec('DROP TRIGGER reject_sidebar_receipt'); cleanup.close(); app = await startApp(options);
    const empty = await request('/api/sidebar', remove); assert.equal(empty.revision, 3);
    assert.deepEqual(await request('/api/sidebar', create), initial);
    assert.deepEqual(await request('/api/sidebar'), empty);
    assert.deepEqual(await request(historyPath), history);
    assert.equal((await request('/api/agents')).agents.length, 2);
    const [one, two] = await Promise.all([randomUUID(), randomUUID()].map(id => fetch(app.origin + '/api/sidebar', { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify({ id, revision: 3, sections: [first] }) })));
    assert.deepEqual([one.status, two.status].sort(), [200, 409]);
    const latest = await request('/api/sidebar');
    await app.close(); app = await startApp(options);
    assert.deepEqual(await request('/api/sidebar'), latest);
    assert.deepEqual(await request('/api/sidebar', create), initial);
    assert.deepEqual(await request(historyPath), history);
    assert.deepEqual(await request('/api/sidebar/summary'), summary);
    const backup = await fetch(app.origin + '/api/backup', { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify({ id: randomUUID(), confirmed: true }) });
    assert.equal(backup.status, 200);
    const backupPath = join(directory, 'backup.sqlite'); writeFileSync(backupPath, Buffer.from(await backup.arrayBuffer()));
    const restoredPath = restoreWorkspace(backupPath, join(directory, 'restored.sqlite'));
    restored = await startApp({ database: restoredPath });
    assert.deepEqual(await (await fetch(restored.origin + '/api/sidebar')).json(), latest);
  } finally { await restored?.close(); await app.close(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(directory, { recursive: true, force: true }); }
});

test('sidebar summary bounds entries and text, redacts credentials, and selects latest explicit remote messages', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-sidebar-summary-')), database = join(directory, 'state.sqlite');
  const app = await startApp({ database, baseURL: 'http://127.0.0.1:1/v1', apiKey: 'saved-provider-credential' });
  const db = testDatabase(database);
  try {
    const add = db.prepare("INSERT INTO agents(id,name,model,instructions) VALUES(?,?,'fixture','PRIVATE_INSTRUCTIONS')");
    const [first, second, room, third] = Array.from({ length: 4 }, () => randomUUID());
    const agents = [first, second, third, ...Array.from({ length: 498 }, () => randomUUID())];
    for (const agent of agents) add.run(agent, agent);
    db.prepare("INSERT INTO channels(id,name,instructions) VALUES(?,'Room','Help')").run(room);
    const turn = db.prepare('INSERT INTO turns(id,agent,text,reply,created,status,error,channel) VALUES(?,?,?,?,?,?,?,?)');
    turn.run(randomUUID(), first, 'old', null, '2026-11-14T00:00:00.000Z', 'running', 'PRIVATE_ERROR', null);
    turn.run(randomUUID(), first, 'unused prompt', 'saved-provider-credential Bearer abcdefghijklmnop token="hide-me" sk-abcdefghijklmnopqrstuv ' + 'x'.repeat(500), '2026-11-14T01:00:00.000Z', 'done', 'PRIVATE_ERROR', null);
    turn.run(randomUUID(), second, 'Room-only secret', null, '2026-11-14T02:00:00.000Z', 'running', null, room);
    turn.run(randomUUID(), first, 'Room newer than direct', null, '2026-11-14T03:00:00.000Z', 'done', null, room);
    const delivery = randomUUID();
    turn.run(delivery, third, 'Delivery', null, '2026-11-14T03:00:00.000Z', 'done', null, room);
    db.exec("INSERT INTO paired_machines(environment_id,name,origin,token,local_token_id,state,status,version,agents) VALUES('machine','Peer','http://127.0.0.1:1','PRIVATE_PAIRING_TOKEN','fixture','paired','offline','fixture','[]')");
    const remote = db.prepare("INSERT INTO remote_agent_calls(id,machine,agent,name,text,reply,created,status,delivery,depth,state) VALUES(?,?,?,'Fixture',?,?,?,'done',?,0,'result')");
    remote.run('a','machine','remote','old',null,1000,null);
    remote.run('b','machine','remote','latest user message','Latest reply',2000,null);
    remote.run('c','machine','remote','AGENT_DELIVERY_SECRET',null,3000,delivery);
    remote.run('d','removed-machine','remote','UNPAIRED_SECRET',null,3000,null);
    const summary = async () => {
      const response = await fetch(app.origin + '/api/sidebar/summary');
      assert.equal(response.status, 200);
      return response.json();
    };
    const result = await summary();
    const userSummaries = result.summaries.filter(isUserSummary);
    assert.equal(userSummaries.length, 501);
    assert.deepEqual(userSummaries.slice(0, 500).map((row: { agent: string }) => row.agent), agents.slice(0, 500));
    assert.deepEqual(result.summaries[500], { agent: guideId, preview: '', time: null, status: 'idle', needs_you: 0 });
    assert.deepEqual(userSummaries[1], { agent: second, preview: '', time: null, status: 'idle', needs_you: 0 });
    assert.equal(userSummaries[0].status, 'running');
    assert.equal(userSummaries[0].preview.length, 180);
    assert.equal(userSummaries[0].time, '2026-11-14T01:00:00.000Z');
    assert.deepEqual(result.summaries.at(-1), { agent: 'remote@machine', preview: 'Latest reply', time: '1970-01-01T00:00:02.000Z', status: 'done', needs_you: 0 });
    assert.deepEqual(Object.keys(userSummaries[0]).sort(), ['agent','needs_you','preview','status','time']);
    assert.doesNotMatch(JSON.stringify(result), /PRIVATE_|hide-me|abcdefghijklmnop|saved-provider-credential|AGENT_DELIVERY_SECRET|UNPAIRED_SECRET|unused prompt/);
    for (let i = 0; i < 501; i++) remote.run(`remote-${i}`, 'machine', `r-${i}`, 'hello', null, 4000, null);
    assert.equal((await summary()).summaries.filter(isUserSummary).length, 1000);
  } finally { db.close(); await app.close(); rmSync(directory, { recursive: true, force: true }); }
});
