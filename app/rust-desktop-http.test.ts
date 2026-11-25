import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { EventEmitter } from 'node:events';
import type { ChildProcess } from 'node:child_process';
import { testDatabase } from './test-support/database.ts';
import sharp from 'sharp';

// The other cases of this file moved to crates/minarbot-e2e/tests/e2e/pd_rust_desktop_http.rs.
// This case stays for its strict provider-call counts until bug B1 is fixed (docs/test-ledger.md).
async function desktopFixture() {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-desktop-test-')), launches: { env: NodeJS.ProcessEnv; profile: string; child: ChildProcess }[] = [], commands: { args: string[]; env: NodeJS.ProcessEnv }[] = [];
  const png = await sharp({ create: { width: 1280, height: 800, channels: 3, background: '#fff' } }).png().toBuffer();
  let now = 0, fail = false;
  const runtime = {
    launch(env: NodeJS.ProcessEnv, profile: string) {
      const child = Object.assign(new EventEmitter(), { exitCode: null, stdin: { end() { queueMicrotask(() => { Object.defineProperty(child, 'exitCode', { value: 0 }); child.emit('exit', 0); }); } } }) as unknown as ChildProcess;
      launches.push({ env, profile, child }); return { child, ready: Promise.resolve() };
    },
    async run(args: string[], env: NodeJS.ProcessEnv) { commands.push({ args, env }); if (fail) throw new Error('synthetic process failure'); return Buffer.alloc(0); },
    async capture() { return png; },
  };
  return { dir, runtime, launches, commands, time: (value: number) => { now = value; }, fail: () => { fail = true; }, options: { dataDir: dir, leaseDir: join(dir, 'leases'), enabled: true, runtime, idleMs: 1000, now: () => now }, cleanup() { rmSync(dir, { recursive: true, force: true }); } };
}

test('HTTP control and user input enforce Origin, revision, validation and rate limits; paused turn resumes once', { timeout: 90000 }, async () => {
  const { createServer } = await import('node:http'), { startApp } = await import('./test-bridge.ts');
  const f = await desktopFixture(); let providerCalls = 0, lastTool = '', action = 'computer_request_user';
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const body = JSON.parse(raw); providerCalls++;
    const result = body.messages.findLast((message: any) => message.role === 'tool');
    if (result) lastTool = result.content;
    res.end(JSON.stringify({ choices: [{ message: result ? { content: 'Continued.' } : { tool_calls: [{ id: 'request-control', type: 'function', function: { name: action, arguments: action === 'computer_click' ? '{"x":1,"y":1}' : '{"reason":"Sign in to fixture"}' } }] }, finish_reason: result ? 'stop' : 'tool_calls' }] }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve)); const address = provider.address(); assert(address && typeof address !== 'string');
  const options = { database: join(f.dir, 'control-http.sqlite'), desktop: f.options, baseURL: `http://127.0.0.1:${address.port}/v1` };
  let app = await startApp(options, { directory: f.dir, servers: [provider] }), closed = false;
  const agent = randomUUID(), path = `/api/agents/${agent}/computer`;
  async function raw(path: string, body?: unknown, origin = app.origin, content = 'application/json') { return fetch(app.origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { Origin: origin, 'Content-Type': content }, body: body === undefined ? undefined : JSON.stringify(body) }); }
  async function request(path: string, body?: unknown) { const res = await raw(path, body); assert(res.ok, await res.clone().text()); return res.json(); }
  async function wait(id: string, status: string) { for (let n = 0; n < 600; n++) { const turn = (await request(`/api/agents/${agent}/turns`)).find((turn: any) => turn.id === id); if (turn?.status === status && (status !== 'queued' || turn.paused === 1)) return turn; await new Promise(resolve => setTimeout(resolve, 10)); } assert.fail('Control HTTP turn did not settle.'); }
  try {
    await request('/api/agents', { runtime: 'api', id: agent, name: 'Control HTTP', model: 'fixture', instructions: 'Help.' });
    await request(path, { id: randomUUID(), revision: 0, enabled: true });
    const input = { revision: 0, name: 'computer_click', args: { x: 4, y: 8 } };
    assert.equal((await raw(path + '/input', input)).status, 409);
    const turn = randomUUID(); await request(`/api/agents/${agent}/turns`, { id: turn, text: 'Request human sign in' });
    const paused = await wait(turn, 'queued'); assert.equal(paused.computerWait.reason, 'Sign in to fixture'); assert.equal(providerCalls, 1);
    assert((await request('/api/agents')).agents.find((item: any) => item.id === agent).computer_waiting);
    assert.equal((await raw(`/api/agents/${agent}/turns/${turn}/resume`, { id: randomUUID(), revision: paused.revision })).status, 409);
    const take = { id: randomUUID(), revision: 0, operation: 'take' };
    assert.equal((await raw(path + '/control', take, 'https://evil.invalid')).status, 403);
    assert.equal((await raw(path + '/control', take, app.origin, 'text/plain')).status, 403);
    const taken = await request(path + '/control', take);
    assert.equal(taken.control.mode, 'user'); assert.equal(taken.control.who, 'owner'); assert(taken.control.since);
    await request(path + '/control', take); assert.equal((await request(path)).control.revision, 1);
    assert.equal((await raw(path + '/control', { id: randomUUID(), revision: 0, operation: 'hand-back' })).status, 409);
    assert.equal((await raw(path + '/input', { revision: 1, name: 'computer_open_url', args: { url: 'ftp://example.test/' } })).status, 400);
    assert.equal((await raw(path + '/input', { ...input, revision: 1 }, 'https://evil.invalid')).status, 403);
    assert.equal((await raw(path + '/input', { ...input, revision: 1, args: { x: 1280, y: 0 } })).status, 400);
    assert.equal((await raw(path + '/input', { ...input, revision: 1, args: { x: 1, y: 1, display: ':0' } })).status, 400);
    await request(path + '/input', { ...input, revision: 1 }); assert.equal(f.commands.length, 1);
    await app.close(); closed = true; app = await startApp(options); closed = false;
    assert.deepEqual((await request(path)).control, taken.control); assert.equal(providerCalls, 1);
    const back = { id: randomUUID(), revision: 1, operation: 'hand-back' }; await request(path + '/control', back); await wait(turn, 'done');
    assert.match(lastTool, /screen may have changed; take a screenshot first/); assert.equal(providerCalls, 2);
    await request(path + '/control', back); assert.equal(providerCalls, 2); assert.equal(f.commands.length, 1);
    assert.equal((await raw(path + '/input', { ...input, revision: 1 })).status, 409);
    await app.close(); closed = true; app = await startApp(options); closed = false; await request(path + '/control', back); assert.equal(providerCalls, 2);
    const next = randomUUID(); await request(`/api/agents/${agent}/turns`, { id: next, text: 'Request and decline' }); await wait(next, 'queued');
    const dismiss = { id: randomUUID(), revision: 2, operation: 'dismiss', turn: next }; await request(path + '/control', dismiss); await wait(next, 'done'); assert.match(lastTool, /user_declined/); assert.equal(providerCalls, 4);
    await request(path + '/control', dismiss); assert.equal(providerCalls, 4);
    assert.equal((await raw(path + '/control', { ...dismiss, revision: 3, id: randomUUID() })).status, 409);
    action = 'computer_click';
    let releaseAction!: () => void, admitted!: () => void;
    const actionGate = new Promise<void>(resolve => { releaseAction = resolve; });
    const actionStarted = new Promise<void>(resolve => { admitted = resolve; });
    const nativeRun = f.runtime.run;
    f.runtime.run = async (args, env) => {
      admitted();
      await actionGate;
      return nativeRun(args, env);
    };
    const admittedTurn = randomUUID(), beforeAction = f.commands.length;
    try {
      await request(`/api/agents/${agent}/turns`, { id: admittedTurn, text: 'Admit model click' });
      await new Promise<void>((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error('Model click did not reach the desktop fixture')), 10000);
        void actionStarted.then(() => { clearTimeout(timeout); resolve(); });
      });
      const taking = request(path + '/control', { id: randomUUID(), revision: 3, operation: 'take' });
      assert.equal((await request(path)).control.mode, 'agent');
      releaseAction();
      assert.equal((await taking).control.mode, 'user');
      await wait(admittedTurn, 'done'); assert.equal(f.commands.length, beforeAction + 1);
    } finally { releaseAction(); f.runtime.run = nativeRun; }

    const blocked = randomUUID();
    await request(`/api/agents/${agent}/turns`, { id: blocked, text: 'Click while owner controls' });
    await wait(blocked, 'queued');
    assert.equal(f.commands.length, beforeAction + 1);
    const saved = testDatabase(options.database);
    try {
      assert.equal(saved.prepare('SELECT count(*) AS n FROM computer_actions WHERE turn=?').get(blocked)!.n, 0);
      assert.match(String(saved.prepare('SELECT result FROM flow_tool_calls WHERE turn=?').get(blocked)!.result), /waiting_for_user/);
    } finally { saved.close(); }
    await request(path + '/input', { revision: 4, name: 'computer_open_url', args: { url: 'https://example.com/fixture' } });
    assert(f.commands.some(command => command.args.includes('https://example.com/fixture')));
    const count = f.commands.length;
    await request(path + '/control', { id: randomUUID(), revision: 4, operation: 'hand-back' });
    assert.equal((await raw(path + '/input', { ...input, revision: 4 })).status, 409);
    await wait(blocked, 'done'); assert.equal(f.commands.length, count);
    await request(path + '/control', { id: randomUUID(), revision: 5, operation: 'take' });
    let release!: () => void; const gate = new Promise<void>(resolve => { release = resolve; });
    f.runtime.run = async () => { await gate; return Buffer.alloc(0); };
    const inputs = Array.from({ length: 40 }, () => raw(path + '/input', { ...input, revision: 6 }));
    // Hold native work so queue-pressure rejection is deterministic without assuming machine speed.
    const rejected = await Promise.race(inputs.map(async response => { const res = await response; if (res.status === 429) return res; return new Promise<never>(() => {}); })); assert.equal(rejected.status, 429);
    release(); const statuses = await Promise.all(inputs.map(async response => (await response).status)); assert(statuses.includes(429)); assert(statuses.every(status => status === 200 || status === 429));
  } finally { if (!closed) await app.close(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); f.cleanup(); }
});
