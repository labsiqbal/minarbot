import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { DatabaseSync } from 'node:sqlite';
import { testDatabase } from './test-support/database.ts';
import { startApp } from './test-bridge.ts';
import { restoreChatCommand } from './src/conversation-command.ts';

async function until<T>(read: () => T | Promise<T>, predicate: (value: T) => boolean): Promise<T> {
  let last: T | undefined;
  for (let i = 0; i < 400; i++) { last = await read(); if (predicate(last)) return last; await new Promise(resolve => setTimeout(resolve, 15)); }
  assert.fail(`Fixture did not settle: ${JSON.stringify(last)}`);
}
async function fixture(stream = false) {
  const directory = mkdtempSync(join(import.meta.dirname, 'node_modules/.dispatch-'));
  const calls: any[] = [], held = new Map<string, () => void>();
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const body = JSON.parse(raw); calls.push(body);
    const last = body.messages.at(-1)?.content;
    const source = body.messages.find((message: any, index: number) => message.role === 'user' && typeof message.content === 'string' && /^(LOOP|HOLD|PARTIAL):/.test(message.content) && !(body.messages[index + 1]?.role === 'assistant' && typeof body.messages[index + 1].content === 'string'))?.content;
    const tools = body.messages.filter((row: any) => row.role === 'tool');
    const send = (delta: unknown, finish_reason: string | null = null) => res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta, finish_reason }] })}\n\n`);
    function answer(content: string) {
      if (body.stream) { res.writeHead(200, { 'Content-Type': 'text/event-stream' }); send({ content }); send({}, 'stop'); res.end('data: [DONE]\n\n'); }
      else res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content } }] }));
    }
    function tool(name: string, args: unknown) {
      const call = { id: randomUUID(), type: 'function', function: { name, arguments: JSON.stringify(args) } };
      if (body.stream) { res.writeHead(200, { 'Content-Type': 'text/event-stream' }); send({ tool_calls: [{ index: 0, ...call }] }); send({}, 'tool_calls'); res.end('data: [DONE]\n\n'); }
      else res.end(JSON.stringify({ choices: [{ finish_reason: 'tool_calls', message: { tool_calls: [call] } }] }));
    }
    if (typeof last === 'string' && last.startsWith('NEW:')) return answer('Restart finished');
    if (typeof source === 'string' && source.startsWith('LOOP:')) {
      if (!tools.length) return held.set(source, () => tool('create_text_file', { name: 'saved.txt', text: 'Saved once' }));
      if (tools.length === 1) return tool('read_provider_settings', {});
      return answer('Steered loop finished');
    }
    if (typeof source === 'string' && source.startsWith('PARTIAL:')) {
      if (!tools.length) return held.set(source, () => tool('create_text_file', { name: 'effect.txt', text: 'Do not repeat' }));
      res.writeHead(200, { 'Content-Type': 'text/event-stream' }); send({ content: 'Partial answer before restart' });
      return held.set(source, () => res.end());
    }
    if (typeof last === 'string' && last.startsWith('HOLD:')) return held.set(last, () => answer('Held answer completed'));
    answer(`Answer: ${last}`);
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const options = { database: join(directory, 'state.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1`, stream };
  let app = await startApp(options, { directory, servers: [provider] });
  const close = async () => {
    try { await app.close(); }
    finally {
      provider.closeAllConnections();
      await new Promise<void>(resolve => provider.close(() => resolve()));
      rmSync(directory, { recursive: true, force: true });
    }
  };
  async function request(path: string, body?: unknown, status?: number): Promise<any> {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const result = await response.json(); if (status) assert.equal(response.status, status, JSON.stringify(result)); else assert(response.ok, JSON.stringify(result)); return result;
  }
  const agent = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Dispatch agent', model: 'fake', instructions: 'Help.' }).catch(async error => {
    await close();
    throw error;
  });
  const path = `/api/agents/${agent.id}/turns`;
  async function turn(id: string, predicate: (row: any) => boolean, at = path) { return until(async () => (await request(at)).find((row: any) => row.id === id), row => !!row && predicate(row)); }
  return { request, turn, agent, path, calls, held, options, directory,
    offline: async (change: (db: DatabaseSync) => void) => { await app.close(); const db = testDatabase(options.database); try { change(db); } finally { db.close(); } app = await startApp(options); },
    restart: async () => { await app.close(); app = await startApp(options); },
    close };
}

test('startup failure reports child output and releases the provider and directory', async t => {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-startup-failure-'));
  const binary = join(directory, 'failing-server');
  writeFileSync(binary, '#!/bin/sh\nprintf "fixture stdout\\n"\nprintf "fixture stderr\\n" >&2\nexit 17\n', { mode: 0o700 });
  const provider = createServer();
  await new Promise<void>((resolve, reject) => provider.once('error', reject).listen(0, '127.0.0.1', resolve));
  const original = process.env.MINARBOT_RUST_BINARY;
  t.after(async () => {
    if (original === undefined) delete process.env.MINARBOT_RUST_BINARY;
    else process.env.MINARBOT_RUST_BINARY = original;
    if (provider.listening) await new Promise<void>(resolve => provider.close(() => resolve()));
    rmSync(directory, { recursive: true, force: true });
  });
  process.env.MINARBOT_RUST_BINARY = binary;
  await assert.rejects(startApp({ database: join(directory, 'state.sqlite') }, { directory, servers: [provider] }),
    /exited \(17\)\.[\s\S]*stdout tail:\nfixture stdout\nstderr tail:\nfixture stderr/);
  assert.equal(provider.listening, false);
  assert.equal(existsSync(directory), false);
});

test('steer is injected once at next tool-loop safe point, durable and idempotent', async () => {
  const f = await fixture(); try {
    const root = randomUUID(), text = `LOOP:${root}`;
    await f.request(f.path, { id: root, text }); await until(() => f.held.has(text), Boolean);
    const guidance = { id: randomUUID(), text: 'Use the new constraints', dispatchMode: 'steer', targetTurn: root };
    const receipt = await f.request(f.path, guidance);
    assert.equal(receipt.dispatch_mode, 'steer'); assert.deepEqual(await f.request(f.path, guidance), receipt);
    await f.request(f.path, { ...guidance, dispatchMode: 'restart' }, 409);
    await f.request(`${f.path}/${guidance.id}/edit`, { id: randomUUID(), editor: randomUUID(), action: 'hold', revision: 1 }, 409);
    assert.equal(f.calls.length, 1); f.held.get(text)!();
    const done = await f.turn(root, row => row.status === 'done'); assert.equal(done.reply, 'Steered loop finished');
    assert.match((await f.turn(guidance.id, row => row.status === 'done')).dispatch_label, /Delivered/);
    assert.equal(done.generatedFiles.length, 1); assert.equal(f.calls.length, 4);
    assert.match(f.calls.at(-1).messages[0].content, /One verification reminder/);
    for (const body of f.calls.slice(1)) assert.equal(body.messages.filter((message: any) => message.role === 'user' && message.content === guidance.text).length, 1);
    const before = f.calls.length; await f.restart(); await f.request(f.path, guidance); assert.equal(f.calls.length, before);
    const after = randomUUID(); await f.request(f.path, { id: after, text: 'Next request' }); await f.turn(after, row => row.status === 'done');
    assert(JSON.stringify(f.calls.at(-1).messages).includes(guidance.text));
    const fork = await f.request(`${f.path}/${root}/fork`, { id: randomUUID() });
    const side = `/api/threads/${fork.id}/turns`, follow = randomUUID();
    await f.request(side, { id: follow, text: 'Continue steered answer' }); await f.turn(follow, row => row.status === 'done', side);
    assert(JSON.stringify(f.calls.at(-1).messages).includes(guidance.text));
    assert(!JSON.stringify(f.calls.at(-1).messages).includes('Next request'));
  } finally { await f.close(); }
});

test('steer before final answer and steer after completion each become one queued turn', async () => {
  const f = await fixture(); try {
    const root = randomUUID(), text = `HOLD:${root}`;
    await f.request(f.path, { id: root, text }); await until(() => f.held.has(text), Boolean);
    const command = { id: randomUUID(), text: 'Late safe point guidance', dispatchMode: 'steer', targetTurn: root };
    await f.request(f.path, command); f.held.get(text)!(); await f.turn(command.id, row => row.status === 'done');
    assert.equal(f.calls.filter(body => body.messages.at(-1).content === command.text).length, 1);
    const late = { ...command, id: randomUUID(), text: 'Already finished guidance' };
    assert.equal((await f.request(f.path, late)).dispatch_mode, 'queue');
    await f.turn(late.id, row => row.status === 'done'); await f.request(f.path, late);
    assert.equal(f.calls.filter(body => body.messages.at(-1).content === late.text).length, 1);
    const fork = await f.request(`${f.path}/${root}/fork`, { id: randomUUID() });
    const side = `/api/threads/${fork.id}/turns`, follow = randomUUID();
    await f.request(side, { id: follow, text: 'Continue original answer' }); await f.turn(follow, row => row.status === 'done', side);
    assert(!JSON.stringify(f.calls.at(-1).messages).includes(command.text));
    assert(!JSON.stringify(f.calls.at(-1).messages).includes(late.text));
  } finally { await f.close(); }
});

test('restart cancels and links a new turn, carries partial text and saved results, no effect replay', async () => {
  const f = await fixture(true); try {
    const root = randomUUID(), text = `PARTIAL:${root}`;
    await f.request(f.path, { id: root, text }); await until(() => f.held.has(text), Boolean);
    const guidance = { id: randomUUID(), text: 'Keep the delivered constraint', dispatchMode: 'steer', targetTurn: root };
    await f.request(f.path, guidance); f.held.get(text)!(); await f.turn(root, row => !!row.partial?.text);
    const queued = randomUUID(); await f.request(f.path, { id: queued, text: 'Ordinary queued message' });
    const command = { id: randomUUID(), text: `NEW:${root}`, dispatchMode: 'restart', targetTurn: root };
    const receipt = await f.request(f.path, command); assert.equal(receipt.dispatch_target, root);
    const done = await f.turn(command.id, row => row.status === 'done');
    assert.equal(done.reply, 'Restart finished'); assert.equal(done.activity.dispatch[0].source_turn, root);
    assert.equal((await f.turn(root, row => row.status === 'cancelled')).generatedFiles.length, 1);
    const newCall = f.calls.find(body => body.messages.at(-1).content === command.text);
    assert(JSON.stringify(newCall.messages).includes('Partial answer before restart'));
    assert(JSON.stringify(newCall.messages).includes('Do not repeat'));
    assert(JSON.stringify(newCall.messages).includes(guidance.text));
    assert.match(JSON.stringify(newCall.messages), /Never replay uncertain operations/);
    assert.equal(newCall.messages.some((row: any) => row.role === 'tool'), false);
    await f.turn(queued, row => row.status === 'done');
    assert(f.calls.indexOf(newCall) < f.calls.findIndex(body => body.messages.at(-1).content === 'Ordinary queued message'));
    const count = f.calls.length; await f.restart(); assert.deepEqual(await f.request(f.path, command), receipt); assert.equal(f.calls.length, count);
  } finally { await f.close(); }
});

test('server restart mid-steer releases guidance once and never replays uncertain source', async () => {
  const f = await fixture(); try {
    const root = randomUUID(), text = `HOLD:${root}`;
    await f.request(f.path, { id: root, text }); await until(() => f.held.has(text), Boolean);
    const command = { id: randomUUID(), text: 'Survive server restart', dispatchMode: 'steer', targetTurn: root };
    await f.request(f.path, command); await f.restart(); await f.turn(command.id, row => row.status === 'done');
    await f.turn(root, row => row.status === 'interrupted'); await f.request(f.path, command); await f.restart();
    assert.equal(f.calls.filter(body => body.messages.at(-1).content === command.text).length, 1);
    assert.equal(f.calls.filter(body => body.messages.at(-1).content === text).length, 1);
  } finally { await f.close(); }
});

test('fork stores bounded immutable cutoff, same agent, idempotency and channel lineage', async () => {
  const f = await fixture(); try {
    const first = randomUUID(), second = randomUUID();
    await f.request(f.path, { id: first, text: 'Fork source' }); await f.turn(first, row => row.status === 'done');
    await f.request(f.path, { id: second, text: 'Beyond cutoff' }); await f.turn(second, row => row.status === 'done');
    const forkPath = `${f.path}/${first}/fork`, command = { id: randomUUID() };
    const fork = await f.request(forkPath, command); assert.equal(fork.agent, f.agent.id); assert.equal(fork.fork.cutoff_turn, first);
    assert.equal(fork.name, 'New conversation'); assert.equal(fork.fork.source_name, 'Main chat');
    const inherited = await f.request(`/api/threads/${fork.id}/fork-context`);
    assert.deepEqual(inherited.turns.map((turn: { id: string }) => turn.id), [first]);
    assert.equal(inherited.turns[0].text, 'Fork source');
    await f.request(`/api/threads/${randomUUID()}/fork-context`, undefined, 404);
    assert.deepEqual(await f.request(forkPath, command), fork); await f.request(`${f.path}/${second}/fork`, command, 409);
    const side = `/api/threads/${fork.id}/turns`, follow = randomUUID(); await f.request(side, { id: follow, text: 'Continue fork' }); await f.turn(follow, row => row.status === 'done', side);
    assert(JSON.stringify(f.calls.at(-1).messages).includes('Answer: Fork source')); assert(!JSON.stringify(f.calls.at(-1).messages).includes('Beyond cutoff'));
    assert.equal((await f.request(`/api/agents/${f.agent.id}/threads`)).find((thread: { id: string }) => thread.id === fork.id).name, 'Continue fork');
    const nested = await f.request(`${side}/${follow}/fork`, { id: randomUUID() }); assert.equal(nested.fork.source_name, 'Continue fork');
    await f.restart(); assert.deepEqual(await f.request(forkPath, command), fork);
    assert.deepEqual(await f.request(`/api/threads/${fork.id}/fork-context`), inherited);
    const channel = await f.request('/api/channels', { id: randomUUID(), name: 'Channel fork', instructions: 'Coordinate.', members: [f.agent.id], lead: f.agent.id });
    const at = `/api/channels/${channel.id}/turns`, channelTurn = randomUUID(); await f.request(at, { id: channelTurn, text: 'Channel answer', recipient: f.agent.id }); await f.turn(channelTurn, row => row.status === 'done', at);
    const channelCommand = { id: randomUUID() }, channelFork = await f.request(`${at}/${channelTurn}/fork`, channelCommand);
    assert.equal(channelFork.fork.source_name, 'Channel fork'); assert.equal(channelFork.agent, f.agent.id); assert.equal(channelFork.fork.source_kind, 'channel'); assert.equal(channelFork.fork.source_id, channel.id);
    assert.deepEqual(await f.request(`${at}/${channelTurn}/fork`, channelCommand), channelFork);
    await f.request(`${side}/${channelTurn}/fork`, { id: randomUUID() }, 404);
    const cutoff = randomUUID();
    await f.offline(db => { for (let i = 0; i < 45; i++) db.prepare("INSERT INTO turns(id,agent,text,reply,status,created) VALUES (?,?,?,?,'done',?)").run(i === 44 ? cutoff : randomUUID(), f.agent.id, `Older ${i}` + '漢'.repeat(5000), '漢'.repeat(30000), new Date().toISOString()); });
    const bounded = await f.request(`${f.path}/${cutoff}/fork`, { id: randomUUID() });
    await f.offline(db => { const transfer = db.prepare('SELECT transcript FROM context_transfers WHERE target_thread=?').get(bounded.id)!; assert(Buffer.byteLength(String(transfer.transcript)) <= 128 * 1024); const snapshot = JSON.parse(String(transfer.transcript)); assert.equal(snapshot.turns.at(-1).id, cutoff); assert(snapshot.turns.length <= 40); assert(snapshot.truncated); });
  } finally { await f.close(); }
});

test('composer recovery validates dispatch mode and target', () => {
  const command = { id: randomUUID(), text: 'Guidance', dispatchMode: 'steer', targetTurn: randomUUID() };
  assert.deepEqual(restoreChatCommand(JSON.stringify(command), false), command);
  assert.throws(() => restoreChatCommand(JSON.stringify({ ...command, dispatchMode: 'auto' }), false));
  assert.throws(() => restoreChatCommand(JSON.stringify({ ...command, targetTurn: 'bad' }), false));
});

test('restart during uncertain MCP effect keeps its receipt and never replays external call', async () => {
  const directory = mkdtempSync(join(import.meta.dirname, 'node_modules/.dispatch-effect-'));
  let connection = '', effects = 0;
  const external = createServer(async (req, res) => {
    if (req.method === 'GET') { res.writeHead(405).end(); return; }
    let raw = ''; for await (const chunk of req) raw += chunk; const input = JSON.parse(raw);
    if (input.id === undefined) { res.writeHead(202).end(); return; }
    let result;
    if (input.method === 'initialize') result = { protocolVersion: '2025-03-26', capabilities: { tools: {} }, serverInfo: { name: 'uncertain-effect', version: '1' } };
    else if (input.method === 'tools/list') result = { tools: [{ name: 'effect', inputSchema: { type: 'object' } }] };
    else { effects++; return; } // Effect accepted; deliberately lose the result.
    res.setHeader('Content-Type', 'application/json'); res.end(JSON.stringify({ jsonrpc: '2.0', id: input.id, result }));
  });
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk; const body = JSON.parse(raw);
    const restart = body.messages.at(-1).content === 'Inspect uncertain result';
    if (restart) assert.match(JSON.stringify(body.messages), /uncertain|not confirmed/i);
    res.end(JSON.stringify({ choices: [{ finish_reason: restart ? 'stop' : 'tool_calls', message: restart ? { content: 'Stopped without replay' } : { tool_calls: [{ id: 'uncertain-effect', type: 'function', function: { name: 'call_mcp_tool', arguments: JSON.stringify({ connection, name: 'effect', arguments: {} }) } }] } }] }));
  });
  await Promise.all([new Promise<void>(resolve => external.listen(0, '127.0.0.1', resolve)), new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve))]);
  const ea = external.address(), pa = provider.address(); assert(ea && typeof ea !== 'string' && pa && typeof pa !== 'string');
  const options = { database: join(directory, 'state.sqlite'), baseURL: `http://127.0.0.1:${pa.port}/v1` };
  let app = await startApp(options, { directory, servers: [external, provider] });
  async function request(path: string, body?: unknown): Promise<any> { const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined }); const result = await response.json(); assert(response.ok, JSON.stringify(result)); return result; }
  try {
    const agent = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Uncertain restart', model: 'fake', instructions: 'Help.' });
    const tested = await request('/api/mcp/test', { id: randomUUID(), name: 'Effect fixture', url: `http://127.0.0.1:${ea.port}/mcp`, token: '' });
    connection = (await request('/api/mcp', { id: randomUUID(), test: tested.id })).id;
    await request(`/api/agents/${agent.id}/mcp/${connection}`, { id: randomUUID(), revision: 0, connectionRevision: 1, tools: ['effect'] });
    const path = `/api/agents/${agent.id}/turns`, root = randomUUID();
    await request(path, { id: root, text: 'Begin effect' }); await until(() => effects, value => value === 1);
    const command = { id: randomUUID(), text: 'Inspect uncertain result', dispatchMode: 'restart', targetTurn: root };
    await request(path, command);
    await until(() => request(path), rows => rows.some((row: any) => row.id === command.id && row.status === 'done'));
    await app.close(); app = await startApp(options); await request(path, command); assert.equal(effects, 1);
    await app.close();
    const db = testDatabase(options.database);
    try { assert.equal(db.prepare('SELECT uncertain FROM mcp_calls WHERE turn=?').get(root)?.uncertain, 1); assert.match(String(db.prepare("SELECT context FROM dispatch_links WHERE target_turn=?").get(command.id)?.context), /uncertain/); } finally { db.close(); }
    app = await startApp(options);
  } finally {
    await app.close(); external.closeAllConnections(); provider.closeAllConnections();
    await Promise.all([new Promise<void>(resolve => external.close(() => resolve())), new Promise<void>(resolve => provider.close(() => resolve()))]); rmSync(directory, { recursive: true, force: true });
  }
});

test('queued dispatch rejects stale revision, held editor, cross-conversation target and started message', async () => {
  const f = await fixture(); try {
    const root = randomUUID(), text = `HOLD:${root}`;
    await f.request(f.path, { id: root, text }); await until(() => f.held.has(text), Boolean);
    const queued = randomUUID(); await f.request(f.path, { id: queued, text: 'Queue remains unchanged' });
    const command = () => ({ id: randomUUID(), revision: 1, mode: 'steer', targetTurn: root });
    await f.request(`${f.path}/${queued}/dispatch`, { ...command(), revision: 99 }, 409);
    const other = await f.request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Other', instructions: 'Help.', model: 'fake' });
    const wrong = randomUUID(); await f.request(`/api/agents/${other.id}/turns`, { id: wrong, text: `HOLD:${wrong}` });
    await f.request(`${f.path}/${queued}/dispatch`, { ...command(), targetTurn: wrong }, 404);
    const edit = { id: randomUUID(), editor: randomUUID(), action: 'hold', revision: 1 };
    const held = await f.request(`${f.path}/${queued}/edit`, edit);
    await f.request(`${f.path}/${queued}/dispatch`, { ...command(), revision: held.revision }, 409);
    await f.request(`${f.path}/${root}/dispatch`, command(), 409);
    assert.equal((await f.request(f.path)).find((row: any) => row.id === queued).dispatch_mode, 'queue');
  } finally { await f.close(); }
});

test('queued restart cancels captured generation; late steering stays queued without duplicate turn', async () => {
  const f = await fixture(); try {
    const root = randomUUID(), text = `HOLD:${root}`;
    await f.request(f.path, { id: root, text }); await until(() => f.held.has(text), Boolean);
    const queued = randomUUID(); await f.request(f.path, { id: queued, text: 'NEW:promoted restart' });
    const command = { id: randomUUID(), revision: 1, mode: 'restart', targetTurn: root };
    await f.request(`${f.path}/${queued}/dispatch`, command);
    await f.turn(root, row => row.status === 'cancelled'); await f.turn(queued, row => row.status === 'done');
    await f.request(`${f.path}/${queued}/dispatch`, command);
    assert.equal((await f.request(f.path)).length, 2);
    const next = randomUUID(), hold = `HOLD:${next}`;
    await f.request(f.path, { id: next, text: hold }); await until(() => f.held.has(hold), Boolean);
    f.held.get(hold)!(); await f.turn(next, row => row.status === 'done');
    const barrier = randomUUID(), barrierText = `HOLD:${barrier}`;
    await f.request(f.path, { id: barrier, text: barrierText }); await until(() => f.held.has(barrierText), Boolean);
    const late = randomUUID(); await f.request(f.path, { id: late, text: 'Late guidance' });
    const response = await f.request(`${f.path}/${late}/dispatch`, { id: randomUUID(), revision: 1, mode: 'steer', targetTurn: next });
    assert.equal(response.dispatch_mode, 'queue'); assert.equal(response.dispatch_label, 'Queued after active turn');
    f.held.get(barrierText)!();
    await f.turn(late, row => row.status === 'done');
    assert.equal(f.calls.filter(body => body.messages.at(-1)?.content === 'Late guidance').length, 1);
  } finally { await f.close(); }
});

test('terminal forks preserve partial answers; untitled chats name from first text and keep manual names', async () => {
  const f = await fixture();
  try {
    for (const status of ['cancelled', 'failed', 'interrupted']) {
      const turn = randomUUID(), text = `Partial ${status}`;
      await f.offline(db => {
        db.prepare('INSERT INTO turns(id,agent,text,status,created) VALUES (?,?,?,?,?)').run(turn, f.agent.id, 'Synthetic stopped task', status, new Date().toISOString());
        const call = db.prepare("INSERT INTO provider_calls(turn,agent,model,origin,purpose,phase,status,created) VALUES (?,?,?,'http://synthetic.test','completion','answer',?,?)").run(turn, f.agent.id, 'fake', status === 'cancelled' ? 'interrupted' : status, new Date().toISOString()).lastInsertRowid;
        db.prepare('INSERT INTO provider_streams(call,text) VALUES (?,?)').run(call, text);
      });
      const fork = await f.request(`${f.path}/${turn}/fork`, { id: randomUUID() });
      const context = await f.request(`/api/threads/${fork.id}/fork-context`);
      assert.equal(context.turns.at(-1).reply, text);
    }
    const thread = await f.request(`/api/agents/${f.agent.id}/threads`, { id: randomUUID(), name: 'New conversation' });
    const path = `/api/threads/${thread.id}/turns`, first = randomUUID();
    await f.request(path, { id: first, text: 'Plan launch' }); await f.turn(first, row => row.status === 'done', path);
    const named = await f.request(`/api/threads/${thread.id}`); assert.equal(named.name, 'Plan launch');
    await f.request(`/api/threads/${thread.id}/edit`, { id: randomUUID(), revision: named.revision, archived: false, name: 'Manual name' });
    const second = randomUUID(); await f.request(path, { id: second, text: 'Another message' }); await f.turn(second, row => row.status === 'done', path);
    assert.equal((await f.request(`/api/threads/${thread.id}`)).name, 'Manual name');
  } finally { await f.close(); }
});
