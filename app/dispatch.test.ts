import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
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

// Other cases moved to crates/minarbot-e2e/tests/e2e/fx_dispatch.rs (#89).
// The steer cases stay here for B7, the composer case tests a TS module.
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

test('composer recovery validates dispatch mode and target', () => {
  const command = { id: randomUUID(), text: 'Guidance', dispatchMode: 'steer', targetTurn: randomUUID() };
  assert.deepEqual(restoreChatCommand(JSON.stringify(command), false), command);
  assert.throws(() => restoreChatCommand(JSON.stringify({ ...command, dispatchMode: 'auto' }), false));
  assert.throws(() => restoreChatCommand(JSON.stringify({ ...command, targetTurn: 'bad' }), false));
});
