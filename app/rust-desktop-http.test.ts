import test from 'node:test';
import assert from 'node:assert/strict';
import fs, { mkdtempSync, rmSync, readFileSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { EventEmitter } from 'node:events';
import type { ChildProcess } from 'node:child_process';
import { testDatabase } from './test-support/database.ts';
import sharp from 'sharp';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import { connectVnc } from './test-support/agent-desktop-test-client.ts';
import { createServer as createNetServer } from 'node:net';

const computerToolNames = [
  'computer_screenshot', 'computer_click', 'computer_type', 'computer_key',
  'computer_scroll', 'computer_open_url', 'computer_request_user',
  'computer_list_vault', 'computer_fill_secret', 'computer_request_secret',
];

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

test('harness stdio MCP bridge delivers Computer images, input, scoped access and human-control pause', async () => {
  const { startApp } = await import('./test-bridge.ts');
  const f = await desktopFixture(), binary = join(f.dir, 'codex'), agent = randomUUID();
  writeFileSync(binary, `#!${process.execPath}\n` + readFileSync(new URL('./fixtures/fake-harness.mjs', import.meta.url), 'utf8').split('\n').slice(1).join('\n')); fs.chmodSync(binary, 0o700);
  const app = await startApp({ database: join(f.dir, 'mcp.sqlite'), baseURL: 'http://127.0.0.1:1/v1', desktop: f.options, harness: { executables: { codex: binary } } });
  const clients: Client[] = [];
  const request = async (path: string, body?: unknown) => { const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined }); const result = await response.json(); assert(response.ok, JSON.stringify(result)); return result; };
  const connect = async (token: string) => { const client = new Client({ name: 'harness-computer-test', version: '1' }); clients.push(client); await client.connect(new StdioClientTransport({ command: process.execPath, args: [new URL('./t3/harness-mcp-bridge.mjs', import.meta.url).pathname], env: { MINARBOT_MCP_URL: app.origin + '/mcp', MINARBOT_MCP_TOKEN: token }, stderr: 'ignore' })); return client; };
  try {
    await request('/api/agents', { id: agent, name: 'Default coder', model: 'fixture', instructions: 'Help.' });
    await request(`/api/agents/${agent}/computer`, { id: randomUUID(), revision: 0, enabled: true });
    const turn = randomUUID(); await request(`/api/agents/${agent}/turns`, { id: turn, text: 'HOLD' });
    for (let n = 0; n < 200; n++) { if ((await request(`/api/agents/${agent}/turns`))[0]?.status === 'running') break; await new Promise(resolve => setTimeout(resolve, 10)); }
    const session = app.mcpServer.mintAgentSession({ agent, turn, ttlMs: 60000, scopes: ['computer:read', 'computer:run'] });
    const client = await connect(session.token);
    assert.deepEqual((await client.listTools()).tools.filter(tool => tool.name.startsWith('computer_')).map(tool => tool.name), computerToolNames);
    const screenshot = await client.callTool({ name: 'computer_screenshot', arguments: {} }); assert(!screenshot.isError);
    const content = screenshot.content as { type: string; mimeType?: string; data?: string }[];
    assert.equal(content[1].type, 'image'); assert.equal(content[1].mimeType, 'image/png'); assert.equal(Buffer.from(content[1].data!, 'base64').subarray(1, 4).toString(), 'PNG');
    for (const [name, args] of [['computer_click', { x: 32, y: 64 }], ['computer_type', { text: 'Hello from MCP' }], ['computer_key', { keys: ['Control_L', 'a'] }], ['computer_scroll', { dx: 0, dy: 3 }], ['computer_open_url', { url: 'https://example.com' }]] as const) { const result = await client.callTool({ name, arguments: args }); assert(!result.isError, JSON.stringify(result)); }
    assert(f.commands.some(command => command.args.includes('Hello from MCP')));
    const limited = await connect(app.mcpServer.mintAgentSession({ agent, turn, ttlMs: 60000, scopes: ['computer:read'] }).token);
    await assert.rejects(() => limited.callTool({ name: 'computer_click', arguments: { x: 1, y: 1 } }), /Minarbot MCP request failed/);
    const status = await request(`/api/agents/${agent}/computer`);
    await request(`/api/agents/${agent}/computer/control`, { operation: 'take', id: randomUUID(), revision: status.control.revision });
    const count = f.commands.length;
    await client.callTool({ name: 'computer_click', arguments: { x: 10, y: 10 } }).catch(() => {});
    assert.equal(f.commands.length, count);
    const paused = (await request(`/api/agents/${agent}/turns`))[0]; assert.equal(paused.paused, 1); assert(paused.computerWait);
    const state = await request(`/api/agents/${agent}/computer`);
    await request(`/api/agents/${agent}/computer/control`, { operation: 'hand-back', id: randomUUID(), revision: state.control.revision });
    for (let n = 0; n < 300; n++) { if ((await request(`/api/agents/${agent}/turns`))[0]?.status === 'running') break; await new Promise(resolve => setTimeout(resolve, 10)); }
    const resumed = await connect(app.mcpServer.mintAgentSession({ agent, turn, ttlMs: 60000, scopes: ['computer:run'] }).token);
    await resumed.callTool({ name: 'computer_request_user', arguments: { reason: 'Sign in to fixture' } }).catch(() => {});
    const requested = (await request(`/api/agents/${agent}/turns`))[0]; assert.equal(requested.paused, 1); assert.equal(requested.computerWait.reason, 'Sign in to fixture');
  } finally { await Promise.allSettled(clients.map(client => client.close())); await app.close(); f.cleanup(); }
});

test('server advertises gated tools, sends model image pixels, protects viewer routes and refuses uncertain resume', async () => {
  const { createServer } = await import('node:http'), { startApp } = await import('./test-bridge.ts');
  const f = await desktopFixture();
  let mode = 'text', exposed: string[] = [], pixels = false, providerCalls = 0;
  let advertisedTools: { name: string; description: string; parameters: { properties: Record<string, unknown> } }[] = [];
  let duringProvider: (() => Promise<void>) | undefined;
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const body = JSON.parse(raw); providerCalls++;
    advertisedTools = body.tools.map((tool: { function: (typeof advertisedTools)[number] }) => tool.function).filter((tool: (typeof advertisedTools)[number]) => tool.name.startsWith('computer_'));
    exposed = advertisedTools.map(tool => tool.name);
    if (duringProvider) { const check = duringProvider; duringProvider = undefined; await check(); }
    pixels ||= body.messages.some((message: any) => Array.isArray(message.content) && message.content.some((part: any) => part.type === 'image_url' && part.image_url.url.startsWith('data:image/png;base64,')));
    const done = mode === 'text' || body.messages.some((message: any) => message.role === 'tool');
    res.end(JSON.stringify({ choices: [{ message: done ? { content: 'Checked.' } : { tool_calls: [{ id: 'computer-call', type: 'function', function: { name: mode === 'screenshot' ? 'computer_screenshot' : 'computer_click', arguments: mode === 'screenshot' ? '{}' : '{"x":12,"y":24}' } }] }, finish_reason: done ? 'stop' : 'tool_calls' }] }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve)); const address = provider.address(); assert(address && typeof address !== 'string');
  const options = { database: join(f.dir, 'server.sqlite'), desktop: { ...f.options, geometry: '1920x1200', idleMs: 30_000 }, baseURL: `http://127.0.0.1:${address.port}/v1` };
  let app = await startApp(options, { directory: f.dir, servers: [provider] }), closed = false;
  async function request(path: string, body?: unknown, origin = app.origin) {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    assert(response.ok, await response.clone().text()); return response.json();
  }
  async function waitTurn(agent: string, id: string, state: string) {
    for (let n = 0; n < 300; n++) { const turn = (await request(`/api/agents/${agent}/turns`)).find((row: any) => row.id === id && row.status === state); if (turn) return turn; await new Promise(resolve => setTimeout(resolve, 10)); }
    assert.fail(`Timed out waiting for ${state}`);
  }
  async function send(agent: string, state = 'done') { const id = randomUUID(); await request(`/api/agents/${agent}/turns`, { id, text: 'Use computer if permitted.' }); return waitTurn(agent, id, state); }
  try {
    const id = randomUUID(); await request('/api/agents', { runtime: 'api', id, name: 'Desktop', model: 'fixture', instructions: 'Help.' }); const path = `/api/agents/${id}/computer`;
    assert.equal((await request(path)).enabled, false); await send(id); assert.deepEqual(exposed, []); assert.equal(f.launches.length, 0);
    const enable = { id: randomUUID(), revision: 0, enabled: true };
    assert.deepEqual(await request(path, enable), await request(path, enable));
    assert.equal((await request(path)).revision, 1);
    const stale = await fetch(app.origin + path, { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify({ ...enable, id: randomUUID() }) });
    assert.equal(stale.status, 409);
    let deniedWhileBusy: { status: number; body: string; nativeCalls: number } | undefined;
    duringProvider = async () => {
      const before = f.commands.length;
      const response = await fetch(app.origin + path + '/open-url', { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify({ url: 'https://example.com/busy' }) });
      deniedWhileBusy = { status: response.status, body: await response.text(), nativeCalls: f.commands.length - before };
    };
    mode = 'screenshot'; const checked = await send(id); assert.equal(checked.activity.artifacts.length, 1); assert.equal(checked.activity.artifacts[0].kind, 'screenshot'); assert.equal(checked.generatedFiles.length, 0); assert.deepEqual(exposed, computerToolNames); assert(pixels); assert.equal(f.launches.length, 1);
    assert.equal(deniedWhileBusy!.status, 409);
    assert.match(deniedWhileBusy!.body, /Computer is busy/);
    assert.equal(deniedWhileBusy!.nativeCalls, 0);
    assert.match(advertisedTools.find(tool => tool.name === 'computer_screenshot')!.description, /1920x1200/);
    assert.deepEqual(advertisedTools.find(tool => tool.name === 'computer_click')!.parameters.properties.x, { type: 'integer', minimum: 0, maximum: 1919 });
    const screenshot = await fetch(app.origin + path + '/screenshot', { headers: { Origin: app.origin } }); assert.equal(screenshot.status, 200); assert.equal(screenshot.headers.get('content-type'), 'image/jpeg');
    await request(path + '/open-url', { url: 'https://example.com/result' });
    assert(f.commands.some(command => JSON.stringify(command).includes('https://example.com/result')));
    assert.equal((await request(path)).control.mode, 'agent');
    assert.equal((await fetch(app.origin + path + '/open-url', { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify({ url: 'javascript:alert(1)' }) })).status, 400);
    const ticket = await request(path + '/vnc-ticket', { mode: 'watch', revision: 0 }); assert.equal(typeof ticket.ticket, 'string');
    assert.equal((await fetch(app.origin + path + '/stream')).status, 404);
    for (const suffix of ['', '/status', '/screenshot', '/navigate', '/back', '/forward', '/reload', '/stop', '/switch', '/close', '/new', '/input']) {
      const route = `${app.origin}/api/agents/${id}/browser${suffix}`;
      assert.equal((await fetch(route)).status, 404);
      assert.equal((await fetch(route, { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify({ id: randomUUID() }) })).status, 404);
    }
    for (const suffix of ['', '/screenshot']) assert.equal((await fetch(app.origin + path + suffix, { headers: { Origin: 'http://evil.invalid' } })).status, 403);
    assert.equal((await fetch(app.origin + path + '/start', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}' })).status, 403);
    assert.equal((await fetch(app.origin + path + '?display=0')).status, 400);
    const other = randomUUID(); await request('/api/agents', { runtime: 'api', id: other, name: 'Other', model: 'fixture', instructions: 'Help.' }); assert.equal((await fetch(`${app.origin}/api/agents/${other}/computer/screenshot`)).status, 403);
    mode = 'click'; f.fail(); const failed = await send(id, 'failed'); const count = f.commands.length;
    await app.close(); closed = true;
    const db = testDatabase(options.database);
    try {
      const saved = db.prepare('SELECT state, length(image) AS size FROM computer_actions WHERE turn=?').get(checked.id)!;
      assert.equal(saved.state, 'done'); assert(Number(saved.size) > 0);
      assert.equal(db.prepare('SELECT state FROM computer_actions WHERE turn=?').get(failed.id)!.state, 'uncertain');
      assert.match(String(db.prepare('SELECT result FROM flow_tool_calls WHERE turn=?').get(failed.id)!.result), /uncertain/);
    } finally { db.close(); }
    app = await startApp(options); closed = false; const beforeResume = providerCalls;
    const current = await waitTurn(id, failed.id, 'failed');
    await request(`/api/agents/${id}/turns/${failed.id}/resume`, { id: randomUUID(), revision: current.revision, acknowledgeUncertain: true });
    const resumed = await waitTurn(id, failed.id, 'failed'); assert.match(resumed.error, /Computer action outcome is uncertain/); assert.equal(f.commands.length, count); assert.equal(providerCalls, beforeResume);
    await app.close(); closed = true;
    app = await startApp({ ...options, desktop: { ...options.desktop, enabled: false } }); closed = false;
    mode = 'text'; await send(id); assert.deepEqual(exposed, []);
    await request(path, { id: randomUUID(), revision: 1, enabled: false });
  } finally { if (!closed) await app.close(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); f.cleanup(); }
});

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

test('HTTP VNC relay enforces owner Host/Origin, tickets, viewer cap, live control fencing and stop cleanup', { timeout: 30000 }, async () => {
  const { startApp } = await import('./test-bridge.ts'); const f = await desktopFixture(), agent = randomUUID();
  const app = await startApp({ database: join(f.dir, 'vnc.sqlite'), desktop: f.options }), path = `/api/agents/${agent}/computer`;
  const received: Buffer[] = [];
  const backend = createNetServer(socket => {
    let state = 0; socket.write('RFB 003.008\n'); socket.on('error', () => {});
    socket.on('data', data => { if (state === 0) { state++; socket.write(Buffer.from([1, 1])); } else if (state === 1) { state++; socket.write(Buffer.alloc(4)); } else if (state === 2) { state++; const init = Buffer.alloc(24); init.writeUInt16BE(1280); init.writeUInt16BE(800, 2); socket.write(init); } else received.push(Buffer.from(data)); });
  });
  const clients: Awaited<ReturnType<typeof connectVnc>>[] = [];
  let socketDirectory: number | undefined;
  async function raw(suffix: string, body: unknown, headers = {}) { return fetch(app.origin + suffix, { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json', ...headers }, body: JSON.stringify(body) }); }
  async function post(suffix: string, body: unknown) { const res = await raw(suffix, body); assert(res.ok, await res.clone().text()); return res.json(); }
  const mint = (mode = 'watch', revision = 0) => post(path + '/vnc-ticket', { mode, revision });
  const url = (ticket: string) => app.origin + path + '/vnc?ticket=' + ticket;
  try {
    await post('/api/agents', { runtime: 'api', id: agent, name: 'VNC fixture', model: 'fixture', instructions: 'Help.' }); await post(path, { id: randomUUID(), revision: 0, enabled: true }); await post(path + '/start', {});
    socketDirectory = fs.openSync(join(f.dir, 'agent-desktops', agent, 'x11'), 'r');
    await new Promise<void>(resolve => backend.listen(`/proc/self/fd/${socketDirectory}/vnc.sock`, resolve));
    assert.equal((await raw(path + '/vnc-ticket', { mode: 'watch', revision: 0 }, { Origin: 'https://evil.invalid' })).status, 403);
    assert.equal((await raw(path + '/vnc-ticket', { mode: 'watch', revision: 0 }, { Authorization: 'Bearer peer-token' })).status, 403);
    assert.equal((await raw(path + '/vnc-ticket', { mode: ['control'], revision: 0 })).status, 400);
    assert.equal((await raw(path + '/vnc-ticket', { mode: 'control', revision: 0 })).status, 409);
    const ticket = await mint();
    await assert.rejects(connectVnc(url(ticket.ticket), 'https://evil.invalid'), /403/); await assert.rejects(connectVnc(url(ticket.ticket), app.origin, { Host: 'evil.invalid' }), /403/); await assert.rejects(connectVnc(url(ticket.ticket), app.origin, { Authorization: 'Bearer script-token' }), /403/);
    const vncPath = join(f.dir, 'agent-desktops', agent, 'x11', 'vnc.sock');
    fs.renameSync(vncPath, vncPath + '.real'); symlinkSync(vncPath + '.real', vncPath);
    await assert.rejects(connectVnc(url((await mint()).ticket), app.origin), /503/);
    fs.unlinkSync(vncPath); fs.renameSync(vncPath + '.real', vncPath);
    const one = await connectVnc(url(ticket.ticket), app.origin); clients.push(one); await one.handshake();
    await assert.rejects(connectVnc(url(ticket.ticket), app.origin), /401/);
    for (let i = 0; i < 2; i++) { const next = await connectVnc(url((await mint()).ticket), app.origin); clients.push(next); await next.handshake(); }
    assert.equal((await raw(path + '/vnc-ticket', { mode: 'watch', revision: 0 })).status, 429);
    one.key(97, true); one.pointer(4, 8, 1); await new Promise(resolve => setTimeout(resolve, 30)); assert(!Buffer.concat(received).includes(Buffer.from([4, 1, 0, 0, 0, 0, 0, 97])));
    await post(path + '/control', { id: randomUUID(), revision: 0, operation: 'take' }); await new Promise(resolve => setTimeout(resolve, 30)); assert(clients.every(client => client.closed));
    const control = await connectVnc(url((await mint('control', 1)).ticket), app.origin); clients.push(control); await control.handshake(); control.key(97, true); control.pointer(10, 20, 1);
    await new Promise(resolve => setTimeout(resolve, 30)); assert(Buffer.concat(received).includes(Buffer.from([4, 1, 0, 0, 0, 0, 0, 97])));
    await post(path + '/control', { id: randomUUID(), revision: 1, operation: 'hand-back' }); await new Promise(resolve => setTimeout(resolve, 30)); assert(control.closed); assert(Buffer.concat(received).includes(Buffer.from([4, 0, 0, 0, 0, 0, 0, 97])));
    const last = await connectVnc(url((await mint('watch', 2)).ticket), app.origin); clients.push(last); await last.handshake(); await post(path + '/stop', {}); await new Promise(resolve => setTimeout(resolve, 30)); assert(last.closed);
  } finally { for (const client of clients) client.close(); await app.close(); await new Promise<void>(resolve => backend.close(() => resolve())); if (socketDirectory !== undefined) fs.closeSync(socketDirectory); f.cleanup(); }
});

test('Always-on Computer uses a desktop slot, survives idle and restores on server restart', async () => {
  const { startApp } = await import('./test-bridge.ts');
  const f = await desktopFixture();
  const options = { database: join(f.dir, 'always-on.sqlite'), desktop: { ...f.options, max: 1 } };
  let app = await startApp(options, { directory: f.dir });
  async function request(path: string, body?: unknown, status = 200) {
    const response = await fetch(app.origin + path, {
      method: body ? 'POST' : 'GET',
      headers: { Origin: app.origin, 'Content-Type': 'application/json' },
      body: body ? JSON.stringify(body) : undefined,
    });
    const data = await response.json();
    assert.equal(response.status, status, JSON.stringify(data));
    return data;
  }
  async function computer(name: string) {
    const id = randomUUID(), path = `/api/agents/${id}/computer`;
    await request('/api/agents', { id, name, runtime: 'api', model: 'fixture', instructions: 'Help.' }, 201);
    await request(path, { id: randomUUID(), revision: 0, enabled: true });
    return path;
  }
  async function waitFor(path: string, state: string, slotAvailable = false) {
    for (let attempt = 0; attempt < 140; attempt++) {
      const status = await request(path);
      if ((status.state === state || state === 'running' && status.state === 'idle') &&
          (!slotAvailable || !status.alwaysOnLimitReached)) return status;
      await new Promise(resolve => setTimeout(resolve, 25));
    }
    assert.fail(`Computer did not reach ${state}`);
  }
  try {
    const first = await computer('Keep on'), second = await computer('On demand');
    await request(second + '/start', {});
    assert.equal((await request(first)).alwaysOnLimitReached, true);
    const enable = { id: randomUUID(), revision: 1, alwaysOn: true };
    await request(first, enable, 429);
    await request(second + '/stop', {});
    const running = await request(first, enable);
    assert.equal(running.alwaysOn, true);
    assert.equal(running.stopsInMs, null);
    assert.equal((await request(first, enable)).revision, running.revision);
    await request(second + '/start', {}, 429);
    await new Promise(resolve => setTimeout(resolve, 2150));
    await waitFor(first, 'running');
    assert.equal(f.launches.length, 2);
    await app.close();
    app = await startApp(options);
    const restored = await waitFor(first, 'running');
    assert.equal(restored.alwaysOn, true);
    assert.equal(f.launches.length, 3);
    assert.equal(f.launches[2].profile, f.launches[1].profile);
    assert.equal((await request(second)).state, 'off');
    await request(first, { id: randomUUID(), revision: restored.revision, alwaysOn: false });
    await waitFor(first, 'off');
    await waitFor(second, 'off', true);
  } finally {
    await app.close();
    f.cleanup();
  }
});
