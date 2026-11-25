// Ported to crates/minarbot-e2e/tests/e2e/cli.rs (#89). These cases stay
// for the assertions that depend on bugs B1-B3 in docs/test-ledger.md: the
// strict model-call counts around questions (B1), `script provider` (B2) and
// `script models` (B3).
import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { createServer, request as httpRequest } from 'node:http';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
import { testDatabase } from './test-support/database.ts';
import { rustBinary } from './rust-binary.ts';

const binary = () => rustBinary('minarbot-server', process.env.MINARBOT_RUST_BINARY);
function cli(args: string[], directory: string) {
  return new Promise<{ exitCode: number; id?: string; agents?: { id: string }[]; threads?: { id: string }[]; conversation?: { name: string }; turn?: any }>((resolve, reject) => {
    const child = spawn(binary(), ['script', ...args], { env: { ...process.env, MINARBOT_SCRIPT_DIRECTORY: directory }, stdio: ['ignore', 'pipe', 'pipe'] });
    let output = ''; child.stdout.on('data', chunk => { output += chunk; }); child.stderr.resume(); child.on('error', reject);
    child.on('close', code => { try { const result = JSON.parse(output); assert.equal(result.exitCode, code); resolve(result); } catch (error) { reject(error); } });
  });
}
import { startApp } from './test-bridge.ts';

test('CLI routine lists and histories stay within the selected granted agent', async t => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-cli-routines-')), directory = join(dir, 'scripts');
  let catalogFails = false;
  const prompts: string[] = []; let answer = 'Routine result';
  const provider = createServer(async (req, res) => { if (req.method === 'GET') { res.statusCode = catalogFails ? 502 : 200; res.end(JSON.stringify({ data: [{ id: 'fixture' }, { id: 'fixture-alt' }] })); return; } let raw=''; for await (const chunk of req) raw+=chunk; prompts.push(JSON.parse(raw).messages.at(-1).content); res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: answer } }] })); });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const options = { database: join(dir, 'db.sqlite'), scriptDirectory: directory, baseURL: `http://127.0.0.1:${address.port}/v1`, apiKey: randomUUID() };
  let app = await startApp(options, { directory: dir, servers: [provider] }), drop = true;
  const requests: string[] = [];
  const proxy = createServer(async (req, res) => {
    let body = ''; for await (const chunk of req) body += chunk;
    const response = await fetch(app.origin + req.url, { method: req.method, headers: { Authorization: req.headers.authorization!, 'Content-Type': 'application/json' }, body: body || undefined });
    const bytes = Buffer.from(await response.arrayBuffer());
    if ((req.url?.endsWith('/run') || req.url?.endsWith('/retry') || req.url?.endsWith('/provider')) && req.method === 'POST') { requests.push(body); if (drop && response.ok) { drop = false; res.destroy(); return; } }
    res.writeHead(response.status, { 'Content-Type': 'application/json' }); res.end(bytes);
  });
  await new Promise<void>(resolve => proxy.listen(0, '127.0.0.1', resolve));
  const proxyAddress = proxy.address(); assert(proxyAddress && typeof proxyAddress !== 'string');
  const useProxy = () => { const file = join(directory, 'connection.json'), saved = JSON.parse(readFileSync(file, 'utf8')); writeFileSync(file, JSON.stringify({ ...saved, url: `http://127.0.0.1:${proxyAddress.port}` })); };
  t.after(async () => { await app.close(); proxy.closeAllConnections(); await new Promise<void>(resolve => proxy.close(() => resolve())); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(dir, { recursive: true, force: true }); });
  const ui = async (path: string, body: unknown) => { const response = await fetch(app.origin + path, { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body) }); const value = await response.json(); assert(response.ok, JSON.stringify(value)); return value; };
  const terminal = (args: string[]) => new Promise<any>((resolve, reject) => {
    const child = spawn(binary(), ['script', ...args], { cwd: import.meta.dirname, env: { ...process.env, MINARBOT_SCRIPT_DIRECTORY: directory }, stdio: ['ignore', 'pipe', 'pipe'] });
    let output = ''; child.stdout.on('data', chunk => { output += chunk; }); child.stderr.resume(); child.on('error', reject);
    child.on('close', code => { try { const value = JSON.parse(output); assert.equal(value.exitCode, code); resolve(value); } catch (error) { reject(error); } });
  });
  const a = await ui('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Routine owner', model: 'fixture', instructions: 'Help' });
  const b = await ui('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Other', model: 'fixture', instructions: 'Help' });
  for (const agent of [a,b]) await ui(`/api/agents/${agent.id}/scripts`, { id: randomUUID(), revision: 0, enabled: true });
  const settings = await terminal(['provider', a.id]);
  assert.equal(settings.exitCode, 0); assert.equal(settings.provider.model, 'fixture'); assert.equal(settings.provider.providerAccount, '');
  assert.equal(settings.provider.accounts[0].configured, true); assert(!JSON.stringify(settings).includes(options.baseURL)); assert(!JSON.stringify(settings).includes(options.apiKey));
  assert.equal((await terminal(['provider', a.id, '--wait'])).exitCode, 2);
  assert.deepEqual((await terminal(['models', a.id])).models, ['fixture', 'fixture-alt']);
  assert.equal((await terminal(['models', a.id, '--wait'])).exitCode, 2);
  catalogFails = true; assert.equal((await terminal(['models', a.id])).exitCode, 1); catalogFails = false;
  const routine = await ui(`/api/agents/${a.id}/routines`, { id: randomUUID(), name: 'Routine', prompt: 'Task', nextAt: Date.now() + 3600000, intervalSeconds: null, enabled: false });
  const list = await terminal(['routines', a.id]); assert.equal(list.exitCode, 0); assert.deepEqual(list.routines.map((row: any) => row.id), [routine.id]);
  assert.deepEqual((await terminal(['routines', b.id])).routines, []);
  assert.deepEqual((await terminal(['routine-runs', a.id, routine.id])).runs, []);
  assert.equal((await terminal(['routine-runs', b.id, routine.id])).exitCode, 1);
  const command = ['routine-run', a.id, routine.id, String(routine.revision), 'CLI input', '--id', randomUUID(), '--wait', '--timeout', '5'];
  assert.equal((await terminal(['routine-run', b.id, ...command.slice(2)])).exitCode, 1);
  useProxy();
  const uncertain = await terminal(command);
  assert.equal(uncertain.exitCode, 5); assert.equal(uncertain.requestId, command[6]); assert.equal(uncertain.routine, routine.id); assert.equal(requests.length, 1);
  const run = await terminal(command); assert.equal(run.exitCode, 0); assert.equal(run.routine, routine.id); assert(run.thread); assert.equal(run.turn.reply, 'Routine result');
  assert.equal(requests.length, 2); assert.equal(requests[0], requests[1]);
  assert.equal((await terminal(command)).id, run.id); assert.deepEqual(prompts, ['Task\n\nScript input:\nCLI input']);
  let history: any;
  for (let n = 0; n < 30; n++) { history = await terminal(['routine-runs', a.id, routine.id]); if (history.runs?.[0]?.status === 'done') break; await new Promise(resolve => setTimeout(resolve, 20)); }
  assert.equal(history.exitCode, 0); assert.equal(history.runs.length, 1); assert.equal(history.runs[0].turn, run.id); assert.equal(history.runs[0].reply, 'Routine result');
  await app.close(); app = await startApp(options);
  useProxy();
  assert.deepEqual((await terminal(['routine-runs', a.id, routine.id])).runs, history.runs);
  assert.equal((await terminal(command)).id, run.id); assert.equal(prompts.length, 1);
  const accepted = await terminal(command.slice(0, 7)); assert.equal(accepted.exitCode, 0); assert.equal(accepted.id, run.id); assert.equal(accepted.thread, run.thread); assert.equal(accepted.turn, undefined);
  assert.equal((await terminal(command.map(value=>value==='CLI input'?'Changed':value))).exitCode, 1);
  for (const bad of [['routines', a.id, '--wait'], ['routine-runs', a.id], ['routine-runs', a.id, '../bad'], ['routine-runs', a.id, routine.id, '--thread', randomUUID()], ['routine-run', a.id, routine.id, '0', 'CLI input', '--id', randomUUID()], ['routine-run', a.id, routine.id, String(routine.revision), 'CLI input']]) assert.equal((await terminal(bad)).exitCode, 2);
  const workflow = await ui(`/api/agents/${a.id}/workflows`, { id: randomUUID(), revision: 0, name: 'Retry condition', graph: { entry: 'check', maxSteps: 4, nodes: [{ id: 'check', kind: 'condition', operator: 'model', model: 'fixture', value: 'Yes?', yes: 'out', no: 'out' }, { id: 'out', kind: 'output' }] } });
  const failed = await terminal(['workflow-run', a.id, workflow.id, '1', 'Input', '--id', randomUUID(), '--wait', '--timeout', '5']);
  assert.equal(failed.exitCode, 1); assert.equal(failed.turn.status, 'failed');
  const retry = ['workflow-retry', a.id, failed.id, String(failed.turn.revision), '--id', randomUUID()];
  assert.equal((await terminal(['workflow-retry', b.id, ...retry.slice(2)])).exitCode, 1);
  answer = 'yes'; drop = true;
  const lostRetry = await terminal(retry); assert.equal(lostRetry.exitCode, 5); assert.equal(lostRetry.requestId, retry[5]);
  let recovered: any;
  for (let n = 0; n < 30; n++) { recovered = await terminal(['workflow-status', a.id, failed.id]); if (recovered.turn?.status === 'done') break; await new Promise(resolve => setTimeout(resolve, 20)); }
  assert.equal(recovered.turn.status, 'done'); const calls = prompts.length; assert.equal(calls, 3);
  await app.close(); app = await startApp(options); useProxy();
  assert.equal((await terminal(retry)).exitCode, 0); assert.equal(requests.at(-1), requests.at(-2)); assert.equal(prompts.length, calls);
  const profileBefore = (await terminal(['provider', a.id])).provider;
  const providerPayload = JSON.stringify({ revision: profileBefore.revision, model: 'fixture-alt', backupModel: '', providerAccount: '' });
  const saveProvider = ['provider-save', a.id, providerPayload, '--id', randomUUID()];
  drop = true;
  const lostProvider = await terminal(saveProvider); assert.equal(lostProvider.exitCode, 5); assert.equal(lostProvider.requestId, saveProvider[4]);
  await app.close(); app = await startApp(options); useProxy();
  const updatedProvider = await terminal(saveProvider); assert.equal(updatedProvider.exitCode, 0); assert.equal(updatedProvider.id, a.id); assert.equal(updatedProvider.provider.model, 'fixture-alt'); assert.equal(updatedProvider.provider.name, a.name); assert.equal(updatedProvider.provider.instructions, a.instructions);
  assert.equal(requests.at(-1), requests.at(-2));
  assert.equal((await terminal(['provider-save', a.id, providerPayload, '--id', randomUUID()])).exitCode, 1);
  assert.equal((await terminal(['provider-save', a.id, '{"apiKey":"forbidden"}', '--id', randomUUID()])).exitCode, 2);
  await ui(`/api/agents/${a.id}/scripts`, { id: randomUUID(), revision: 1, enabled: false });
  assert.equal((await terminal(['routine-runs', a.id, routine.id])).exitCode, 5);
  assert.equal((await terminal(command)).exitCode, 5);
  assert.equal((await terminal(retry)).exitCode, 5);
  assert.equal((await terminal(['models', a.id])).exitCode, 5);
  assert.equal((await terminal(['provider', a.id])).exitCode, 5);
  assert.equal((await terminal(saveProvider)).exitCode, 5);
});

test('script CLI discovers only grants, sends once, waits, preserves retry ID and reports attention/timeouts', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-cli-')); let calls = 0;
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk; calls++;
    const text = JSON.parse(raw).messages.at(-1).content;
    if (text === 'HOLD') return;
    const ask = text === 'ASK';
    res.end(JSON.stringify({ choices: [{ finish_reason: ask ? 'tool_calls' : 'stop', message: ask ? { tool_calls: [{ id: 'question', type: 'function', function: { name: 'ask_user', arguments: JSON.stringify({ question: 'Choose', choices: ['Yes','No'] }) } }] } : { content: 'Answer' } }] }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const directory = join(dir, 'scripts'), options = { database: join(dir, 'db.sqlite'), scriptDirectory: directory, baseURL: `http://127.0.0.1:${address.port}/v1` };
  let app = await startApp(options, { directory: dir, servers: [provider] });
  async function request(path: string, body?: unknown) {
    const res = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const value = await res.json(); assert(res.ok, JSON.stringify(value)); return value;
  }
  try {
    const a = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Alpha', model: 'test', instructions: 'Help' });
    const b = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'alpha', model: 'test', instructions: 'Help' });
    await request(`/api/agents/${a.id}/scripts`, { id: randomUUID(), revision: 0, enabled: true });
    const list = await cli(['list'], directory); assert.equal(list.exitCode, 0); assert.deepEqual(list.agents?.map(row => row.id), [a.id]);
    const id = randomUUID(), args = ['run','ALPHA','Hello','--id',id,'--wait','--timeout','3'];
    const done = await cli(args, directory); assert.equal(done.exitCode, 0); assert.equal(done.turn.reply, 'Answer');
    assert.equal((await cli(args, directory)).exitCode, 0); assert.equal(calls, 1);
    assert.equal((await cli(['status',a.id,id], directory)).turn.id, id);
    await request(`/api/agents/${b.id}/scripts`, { id: randomUUID(), revision: 0, enabled: true });
    assert.equal((await cli(['run','Alpha','Ambiguous'], directory)).exitCode, 2); assert.equal(calls, 1);
    const ask = await cli(['run',a.id,'ASK','--wait','--timeout','3'], directory); assert.equal(ask.exitCode, 3);
    assert.equal((await cli(['cancel',a.id,ask.id!], directory)).exitCode, 0);
    assert.equal((await cli(['status',a.id,ask.id!], directory)).exitCode, 1);
    const held = await cli(['run',a.id,'HOLD','--wait','--timeout','1'], directory); assert.equal(held.exitCode, 4); assert(held.id);
    assert.equal((await cli(['status',a.id,held.id], directory)).turn.status, 'running');
    assert.equal((await cli(['cancel',b.id,held.id], directory)).exitCode, 1);
    assert.equal((await cli(['status',a.id,held.id], directory)).turn.status, 'running');
    assert.equal((await cli(['cancel',a.id,held.id.toUpperCase()], directory)).exitCode, 1);
    const cancelled = await cli(['cancel',a.id,held.id], directory);
    assert.equal(cancelled.exitCode, 0); assert.equal(cancelled.turn.status, 'cancelled');
    assert.equal((await cli(['cancel',a.id,id], directory)).exitCode, 1);
    assert.equal((await cli(['status',a.id,held.id], directory)).exitCode, 1);
    const output = await new Promise<string>((resolve, reject) => {
      const child = spawn(binary(), ['script','list'], { cwd: import.meta.dirname, env: { ...process.env, MINARBOT_SCRIPT_DIRECTORY: directory }, stdio: ['ignore','pipe','pipe'] });
      let text = ''; child.stdout.on('data', chunk => { text += chunk; }); child.stderr.resume(); child.on('error', reject); child.on('close', code => { if (code !== 0) reject(new Error(`CLI exit ${code}`)); else resolve(text); });
    });
    assert.equal(JSON.parse(output).agents.length, 2); assert(!output.includes(JSON.parse(readFileSync(join(directory,'connection.json'),'utf8')).token));
    const unicode = await new Promise<string>((resolve, reject) => {
      const child = spawn(binary(), ['script','run',a.id,'-','--wait','--timeout','3'], { cwd: import.meta.dirname, env: { ...process.env, MINARBOT_SCRIPT_DIRECTORY: directory }, stdio: ['pipe','pipe','pipe'] });
      let text = ''; child.stdout.on('data', chunk => { text += chunk; }); child.stderr.resume(); child.on('error', reject); child.on('close', code => { if (code !== 0) reject(new Error(`CLI exit ${code}: ${text}`)); else resolve(text); });
      child.stdin.write(Buffer.from([0x63,0x61,0x66,0xc3]));
      setTimeout(() => child.stdin.end(Buffer.from([0xa9])), 500);
    });
    assert.equal(JSON.parse(unicode).turn.text, 'café');
    const connectionFile = join(directory, 'connection.json'), saved = readFileSync(connectionFile, 'utf8');
    let drop = true, observed: (() => void) | undefined;
    const proxy = createServer(async (req, res) => {
      let body = ''; for await (const chunk of req) body += chunk;
      const response = await fetch(app.origin + req.url, { method: req.method, headers: { Authorization: req.headers.authorization!, 'Content-Type': 'application/json' }, body: body || undefined });
      const bytes = Buffer.from(await response.arrayBuffer());
      if (req.method === 'POST' && drop) { drop = false; res.destroy(); return; }
      res.writeHead(response.status, { 'Content-Type': 'application/json' }); res.end(bytes);
      if (req.method === 'GET' && req.url?.includes('?focus=')) observed?.();
    });
    await new Promise<void>(resolve => proxy.listen(0, '127.0.0.1', resolve));
    const proxyAddress = proxy.address(); assert(proxyAddress && typeof proxyAddress !== 'string');
    try {
      writeFileSync(connectionFile, JSON.stringify({ ...JSON.parse(saved), url: `http://127.0.0.1:${proxyAddress.port}` }));
      const lost = await cli(['run',a.id,'Lost response'], directory); assert.equal(lost.exitCode, 5); assert(lost.id);
      const recovered = await cli(['run',a.id,'Lost response','--id',lost.id,'--wait','--timeout','3'], directory);
      assert.equal(recovered.exitCode, 0); assert.equal(recovered.id, lost.id); assert.equal(calls, 5);
      const pending = await cli(['run',a.id,'HOLD'], directory); assert.equal(pending.exitCode, 0);
      drop = true;
      const lostCancel = await cli(['cancel',a.id,pending.id!], directory);
      assert.equal(lostCancel.exitCode, 5); assert.equal(lostCancel.id, pending.id);
      const cancelRetry = await cli(['cancel',a.id,pending.id!], directory);
      assert.equal(cancelRetry.exitCode, 0); assert.equal(cancelRetry.turn.status, 'cancelled');
      const revision = cancelRetry.turn.revision;
      assert.equal((await cli(['cancel',a.id,pending.id!], directory)).turn.revision, revision);
      const restartPending = await cli(['run',a.id,'HOLD'], directory); assert.equal(restartPending.exitCode, 0);
      const seen = new Promise<void>(resolve => { observed = resolve; }); let settled = false;
      const waiting = cli(['status',a.id,restartPending.id!,'--wait','--timeout','5'], directory).then(result => { settled = true; return result; });
      await seen; observed = undefined;
      await app.close();
      await new Promise(resolve => setTimeout(resolve, 650)); assert.equal(settled, false, 'discovery gap must not end an established wait');
      app = await startApp(options);
      const interrupted = await waiting; assert.equal(interrupted.exitCode, 1); assert.equal(interrupted.turn.status, 'interrupted'); assert.equal(calls, 7);
    } finally { proxy.closeAllConnections(); await new Promise<void>(resolve => proxy.close(() => resolve())); }
    const collision = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: a.id, model: 'test', instructions: 'Help' });
    await request(`/api/agents/${collision.id}/scripts`, { id: randomUUID(), revision: 0, enabled: true });
    assert.equal((await cli(['status',a.id,id], directory)).exitCode, 0);
    assert.equal((await cli(['status',a.id.toUpperCase(),id], directory)).exitCode, 0);
    await app.close(); app = await startApp(options);
    assert.equal((await cli(['status',a.id,id], directory)).turn.reply, 'Answer'); assert.equal(calls, 7);
    await request(`/api/agents/${a.id}/scripts`, { id: randomUUID(), revision: 1, enabled: false });
    assert.equal((await cli(['status',a.id,id], directory)).exitCode, 5);
    assert.equal((await cli(['cancel',a.id,held.id], directory)).exitCode, 5);
    for (let n = 0; n < 99; n++) {
      const extra = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: `Extra ${n}`, model: 'test', instructions: 'Help' });
      await request(`/api/agents/${extra.id}/scripts`, { id: randomUUID(), revision: 0, enabled: true });
    }
    const catalog = await cli(['list'], directory); assert.equal(catalog.agents?.length, 101);
    assert.equal(new Set(catalog.agents?.map(agent => agent.id)).size, 101);
    assert(!catalog.agents?.some(agent => agent.id === a.id));
    const credentials = JSON.parse(readFileSync(connectionFile, 'utf8'));
    const headers = { Authorization: `Bearer ${credentials.token}` };
    const first = await (await fetch(app.origin + '/api/scripts/agents', { headers })).json();
    assert.equal(first.agents.length, 100); assert(first.next_after);
    const second = await (await fetch(app.origin + '/api/scripts/agents?after=' + first.next_after, { headers })).json();
    assert.equal(second.agents.length, 1); assert.equal(second.next_after, null);
    for (const suffix of ['?after=invalid', '?unexpected=1']) {
      const rejected = await fetch(app.origin + '/api/scripts/agents' + suffix, { headers }); assert.equal(rejected.status, 400); await rejected.arrayBuffer();
    }
    const denied = await fetch(app.origin + '/api/scripts/agents', { headers: { ...headers, Origin: app.origin } }); assert.equal(denied.status, 403); await denied.arrayBuffer();
    const side = await request(`/api/agents/${b.id}/threads`, { id: randomUUID(), name: 'Cancellation scope' });
    const listedSides = await cli(['threads', b.id], directory); assert.equal(listedSides.exitCode, 0); assert(listedSides.threads?.some(row => row.id === side.id));
    const createdSide = await cli(['thread', b.id, 'CLI side'], directory); assert.equal(createdSide.exitCode, 0); assert.equal(createdSide.conversation?.name, 'CLI side');
    const sideReply = await cli(['run', b.id, 'Hello side', '--thread', side.id, '--wait', '--timeout', '3'], directory);
    assert.equal(sideReply.exitCode, 0); assert.equal(sideReply.turn.thread, side.id); assert.equal(sideReply.turn.reply, 'Answer');
    const sideTurn = await request(`/api/threads/${side.id}/turns`, { id: randomUUID(), text: 'HOLD' });
    assert.equal((await cli(['cancel',b.id,sideTurn.id], directory)).exitCode, 1);
    await request(`/api/threads/${side.id}/turns/${sideTurn.id}/cancel`, { id: randomUUID() });
    const channel = await request('/api/channels', { id: randomUUID(), name: 'Cancellation scope', instructions: 'Help', members: [b.id], lead: b.id });
    const channelTurn = await request(`/api/channels/${channel.id}/turns`, { id: randomUUID(), text: 'HOLD', agent: b.id });
    assert.equal((await cli(['cancel',b.id,channelTurn.id], directory)).exitCode, 1);
    await request(`/api/channels/${channel.id}/turns/${channelTurn.id}/cancel`, { id: randomUUID() });
    const revokeTarget = await cli(['run',b.id,'HOLD'], directory);
    assert.equal(revokeTarget.exitCode, 0);
    const cancelPath = `/api/scripts/agents/${b.id}/turns/${revokeTarget.id}/cancel`;
    for (const method of ['GET', 'PUT']) {
      const response = await fetch(app.origin + cancelPath, { method, headers });
      assert.equal(response.status, 405); await response.arrayBuffer();
    }
    let finishBody!: () => void;
    const delayed = new Promise<number | undefined>((resolve, reject) => {
      const raw = JSON.stringify({ id: revokeTarget.id });
      const req = httpRequest(app.origin + cancelPath, { method: 'POST', headers: { ...headers, 'Content-Type': 'application/json' } }, res => { res.resume(); res.on('end', () => resolve(res.statusCode)); });
      req.on('error', reject); req.write(raw.slice(0, -1)); finishBody = () => req.end(raw.slice(-1));
    });
    await request(`/api/agents/${b.id}/scripts`, { id: randomUUID(), revision: 1, enabled: false });
    finishBody(); assert.equal(await delayed, 403);
    const retained = await request(`/api/agents/${b.id}/turns?focus=${revokeTarget.id}`);
    assert(['queued','running'].includes(retained[0].status));
    await request(`/api/agents/${b.id}/turns/${revokeTarget.id}/cancel`, { id: randomUUID() });
    const file = join(directory, 'connection.json'), original = readFileSync(file);
    writeFileSync(file, JSON.stringify({ url: 'https://example.com', token: 'x'.repeat(32) }));
    assert.equal((await cli(['list'], directory)).exitCode, 5); writeFileSync(file, original);
    assert.equal((await cli(['run',a.id,'Hello','--id'], directory)).exitCode, 2);
    assert.equal((await cli(['cancel',a.id], directory)).exitCode, 2);
    assert.equal((await cli(['cancel',a.id,id,'--wait'], directory)).exitCode, 2);
  } finally { await app.close(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(dir, { recursive: true, force: true }); }
});

test('CLI answers retain receipt identity across lost replies and restart', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-cli-answer-')); let calls = 0, drop = false, dropStatus = false, dropThread = false, dropSide = false;
  const posts: string[] = [], answers: string[] = [];
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk; calls++;
    const message = JSON.parse(raw).messages.at(-1), ask = message.content === 'ASK';
    if (!ask) answers.push(JSON.parse(message.content).answer);
    res.end(JSON.stringify({ choices: [{ finish_reason: ask ? 'tool_calls' : 'stop', message: ask ? { tool_calls: [{ id: 'ask', type: 'function', function: { name: 'ask_user', arguments: JSON.stringify({ question: 'Proceed?', choices: ['Yes','No'] }) } }] } : { content: 'Answered' } }] }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const directory = join(dir, 'scripts'), options = { database: join(dir, 'db.sqlite'), scriptDirectory: directory, baseURL: `http://127.0.0.1:${address.port}/v1` };
  let app = await startApp(options, { directory: dir, servers: [provider] });
  const proxy = createServer(async (req, res) => {
    let body = ''; for await (const chunk of req) body += chunk;
    const response = await fetch(app.origin + req.url, { method: req.method, headers: { Authorization: req.headers.authorization!, 'Content-Type': 'application/json' }, body: body || undefined });
    const bytes = Buffer.from(await response.arrayBuffer());
    if (req.method === 'POST' && dropThread && req.url?.endsWith('/threads')) { dropThread = false; res.destroy(); return; }
    if (req.method === 'POST' && dropSide && req.url?.includes('/threads/') && req.url.endsWith('/turns')) { dropSide = false; res.destroy(); return; }
    if (req.url?.endsWith('/answer')) { posts.push(body); if (drop) { drop = false; res.destroy(); return; } }
    if (dropStatus && req.url?.includes('?focus=')) { dropStatus = false; res.destroy(); return; }
    res.writeHead(response.status, { 'Content-Type': 'application/json' }); res.end(bytes);
  });
  await new Promise<void>(resolve => proxy.listen(0, '127.0.0.1', resolve));
  const proxyAddress = proxy.address(); assert(proxyAddress && typeof proxyAddress !== 'string');
  const proxyURL = `http://127.0.0.1:${proxyAddress.port}`;
  function useProxy() { const file = join(directory, 'connection.json'), value = JSON.parse(readFileSync(file, 'utf8')); writeFileSync(file, JSON.stringify({ ...value, url: proxyURL })); }
  async function ui(path: string, body: unknown) {
    const response = await fetch(app.origin + path, { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
    const value = await response.json(); assert(response.ok, JSON.stringify(value)); return value;
  }
  async function terminal(args: string[], input?: string) {
    return await new Promise<any>((resolve, reject) => {
      const child = spawn(binary(), ['script',...args], { cwd: import.meta.dirname, env: { ...process.env, MINARBOT_SCRIPT_DIRECTORY: directory }, stdio: ['pipe','pipe','pipe'] });
      let output = ''; child.stdout.on('data', chunk => { output += chunk; }); child.stderr.resume(); child.on('error', reject);
      child.on('close', code => { try { const result = JSON.parse(output); assert.equal(result.exitCode, code); resolve(result); } catch (error) { reject(error); } }); child.stdin.end(input);
    });
  }
  try {
    const a = await ui('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Answer agent', model: 'fixture', instructions: 'Help' });
    const b = await ui('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Other', model: 'fixture', instructions: 'Help' });
    for (const agent of [a,b]) await ui(`/api/agents/${agent.id}/scripts`, { id: randomUUID(), revision: 0, enabled: true });
    useProxy();
    const ask = await terminal(['run',a.id,'ASK','--wait','--timeout','5']); assert.equal(ask.exitCode, 3);
    const question = ask.turn.questions[0].id, requestId = randomUUID(), args = ['answer',a.id,ask.id,question,'Yes','--id',requestId];
    assert.equal((await terminal(['answer',a.id,ask.id,question,'Yes'])).exitCode, 2);
    assert.equal((await terminal(['answer',a.id,ask.id,question,'x'.repeat(4001),'--id',randomUUID()])).exitCode, 2);
    assert.equal((await terminal(['answer',a.id,ask.id,question,'Wrong','--id',randomUUID()])).exitCode, 1);
    assert.equal((await terminal(['answer',b.id,ask.id,question,'Yes','--id',randomUUID()])).exitCode, 1); assert.equal(calls, 1);
    drop = true;
    const lost = await terminal(args); assert.equal(lost.exitCode, 5); assert.equal(lost.id, ask.id); assert.equal(lost.requestId, requestId); assert.equal(lost.question, question);
    const done = await terminal([...args,'--wait','--timeout','5']); assert.equal(done.exitCode, 0); assert.equal(done.turn.reply, 'Answered'); assert.equal(done.requestId, requestId);
    assert.equal(posts.at(-1), posts.at(-2)); assert.deepEqual(answers, ['Yes']); assert.equal(calls, 2);
    await app.close(); app = await startApp(options); useProxy();
    const replay = await terminal(args); assert.equal(replay.exitCode, 0); assert.equal(replay.answer.answer, 'Yes'); assert.equal(calls, 2);
    assert.equal((await terminal(['answer',a.id,ask.id,question,'No','--id',requestId])).exitCode, 1);
    const next = await terminal(['run',a.id,'ASK','--id',randomUUID().toUpperCase(),'--wait','--timeout','5']); assert.equal(next.exitCode, 3);
    dropStatus = true; const postCount = posts.length;
    const stdin = await terminal(['answer',a.id,next.id,next.turn.questions[0].id,'-','--id',randomUUID(),'--wait','--timeout','5'], 'No');
    assert.equal(stdin.exitCode, 0); assert.deepEqual(answers, ['Yes','No']);
    assert.equal(dropStatus, false); assert.equal(posts.length, postCount + 1);
    const uncertain = await terminal(['run',a.id,'ASK','--wait','--timeout','5']); assert.equal(uncertain.exitCode, 3);
    await app.close();
    const db = testDatabase(options.database); db.prepare("UPDATE turns SET status='interrupted',revision=revision+1 WHERE id=?").run(uncertain.id); db.close();
    app = await startApp(options); useProxy();
    const uncertainArgs = ['answer',a.id,uncertain.id,uncertain.turn.questions[0].id,'Yes','--id',randomUUID()], before = calls;
    assert.equal((await terminal(uncertainArgs)).exitCode, 1); assert.equal(calls, before);
    assert.equal((await terminal([...uncertainArgs,'--acknowledge-uncertain','--wait','--timeout','5'])).exitCode, 0); assert.equal(calls, before + 1);
    const lowerId = 'abcdef12-3456-4789-abcd-0123456789ab', upperId = lowerId.toUpperCase();
    const lower = await terminal(['run',a.id,'ASK','--id',lowerId,'--wait','--timeout','5']); assert.equal(lower.exitCode, 3);
    const upper = await terminal(['run',a.id,'ASK','--id',upperId,'--wait','--timeout','5']); assert.equal(upper.exitCode, 3);
    const cancelledUpper = await terminal(['cancel',a.id,upperId]); assert.equal(cancelledUpper.exitCode, 0);
    assert.equal(cancelledUpper.turn.id, upperId);
    const untouchedLower = await terminal(['status',a.id,lowerId]); assert.equal(untouchedLower.exitCode, 3); assert.equal(untouchedLower.turn.status, 'queued');
    assert.equal((await terminal(['cancel',a.id,lowerId])).exitCode, 0);
    await ui(`/api/agents/${a.id}/scripts`, { id: randomUUID(), revision: 1, enabled: false });
    assert.equal((await terminal(args)).exitCode, 5); assert.equal(calls, before + 3);
    const sideId = randomUUID(), create = ['thread', b.id, 'Terminal side', '--id', sideId];
    dropThread = true;
    const lostCreate = await terminal(create); assert.equal(lostCreate.exitCode, 5); assert.equal(lostCreate.id, sideId);
    const side = await terminal(create); assert.equal(side.exitCode, 0); assert.equal(side.conversation.id, sideId);
    assert.equal((await terminal(create)).conversation.id, sideId);
    assert.equal((await terminal(['threads', b.id])).threads.filter((row: { id: string }) => row.id === sideId).length, 1);
    const sideArgs = ['run', b.id, 'ASK', '--thread', sideId, '--id', randomUUID().toUpperCase(), '--wait', '--timeout', '5'];
    dropSide = true; const beforeSide = calls;
    const lostSend = await terminal(sideArgs); assert.equal(lostSend.exitCode, 5); assert.equal(lostSend.thread, sideId); assert.equal(lostSend.id, sideArgs[6]);
    const sideAsk = await terminal(sideArgs); assert.equal(sideAsk.exitCode, 3); assert.equal(sideAsk.thread, sideId);
    const sideCalls = calls; assert.equal(sideCalls, beforeSide + 1);
    assert.equal((await terminal(sideArgs)).id, sideAsk.id); assert.equal(calls, sideCalls);
    assert.equal((await terminal(['status', b.id, sideAsk.id])).exitCode, 1);
    assert.equal((await terminal(['cancel', b.id, sideAsk.id])).exitCode, 1);
    assert.equal((await terminal(['status', b.id, sideAsk.id, '--thread', sideId])).exitCode, 3);
    const sideAnswer = ['answer', b.id, sideAsk.id, sideAsk.turn.questions[0].id, 'Yes', '--thread', sideId, '--id', randomUUID()];
    drop = true;
    const sideLost = await terminal(sideAnswer); assert.equal(sideLost.exitCode, 5); assert.equal(sideLost.thread, sideId);
    const sideDone = await terminal([...sideAnswer, '--wait', '--timeout', '5']); assert.equal(sideDone.exitCode, 0); assert.equal(sideDone.turn.thread, sideId); assert.equal(calls, sideCalls + 1);
    await app.close(); app = await startApp(options); useProxy();
    assert.equal((await terminal(sideAnswer)).exitCode, 0); assert.equal(calls, sideCalls + 1);
    const sideNext = await terminal(['run', b.id, 'ASK', '--thread', sideId, '--wait', '--timeout', '5']); assert.equal(sideNext.exitCode, 3);
    assert.equal((await terminal(['cancel', b.id, sideNext.id, '--thread', sideId])).exitCode, 0);
    assert.equal((await terminal(['status', b.id, sideNext.id, '--thread', sideId])).turn.status, 'cancelled');
    for (const bad of [['threads', b.id, 'extra'], ['thread', b.id, 'name', '--wait'], ['run', b.id, 'ASK', '--thread', 'bad'], ['run', b.id, 'ASK', '--thread', sideId, '--thread', sideId]]) assert.equal((await terminal(bad)).exitCode, 2);
  } finally { await app.close(); proxy.closeAllConnections(); provider.closeAllConnections(); await Promise.all([new Promise<void>(resolve => proxy.close(() => resolve())), new Promise<void>(resolve => provider.close(() => resolve()))]); rmSync(dir, { recursive: true, force: true }); }
});
