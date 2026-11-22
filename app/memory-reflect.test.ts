import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { createServer, type ServerResponse } from 'node:http';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { startApp } from './test-bridge.ts';
import { testDatabase } from './test-support/database.ts';

type ProviderMessage = { role: string; content: string };
type Turn = { id: string; status: string; error?: string };
type Evidence = { id: number; text: string; source: { kind: string; id: string }[] };

test('scoped auto-recall, history, reflection and standing answer refresh', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-memory3-'));
  const database = join(directory, 'state.sqlite');
  const seen = new Map<string, string>();
  const results = new Map<string, { answer?: string; evidence?: Evidence[]; items?: Evidence[] }>();
  const rounds: string[][] = [];
  let held: ServerResponse | undefined;
  const provider = createServer(async (req, res) => {
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    const { messages } = JSON.parse(Buffer.concat(chunks).toString()) as { messages: ProviderMessage[] };
    const system = messages[0]?.content ?? '';
    const user = messages.findLast(message => message.role === 'user')?.content ?? '';
    const send = (content: string) => res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content } }], usage: { prompt_tokens: 31, completion_tokens: 11, total_tokens: 42 } }));
    if (system.includes('You answer a question by reasoning over retrieved memories.')) {
      const input = JSON.parse(user) as { question: string; searched: string[]; evidence: Evidence[] };
      rounds.push(input.searched);
      const evidence = input.evidence.find(hit => hit.text.includes('north cabinet'));
      return send(JSON.stringify(input.searched.length === 1 && input.question !== 'Where are the orchid tags?'
        ? { action: 'recall', query: 'orchid' }
        : { action: 'answer', text: evidence ? 'North cabinet.' : 'No evidence.', citations: evidence ? [evidence.id] : [] }));
    }
    if (system.includes('Extract SIGNIFICANT facts')) return send('{"facts":[]}');
    if (system.includes('You keep a long-term memory')) return send('{"facts":[],"deletes":[]}');
    seen.set(user, system);
    if (user === 'Hold for MCP') { held = res; return; }
    const last = messages.findLast(message => message.role === 'tool');
    if (last) {
      results.set(user, JSON.parse(last.content));
      return send('Done');
    }
    const name = user.startsWith('Reflect') ? 'reflect' : user.startsWith('Find') ? 'search_history' : '';
    if (!name) return send('Done');
    return res.end(JSON.stringify({ choices: [{ finish_reason: 'tool_calls', message: { tool_calls: [{
      id: 'memory-tool', type: 'function', function: { name, arguments: JSON.stringify({ query: name === 'reflect' ? 'Where do tags live?' : 'aquamarine' }) },
    }] } }] }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address();
  assert(address && typeof address !== 'string');
  const app = await startApp({ database, baseURL: `http://127.0.0.1:${address.port}/v1`, defaultModel: 'fixture', testMemoryExtraction: true }, { directory, servers: [provider] });
  async function request<T>(path: string, body?: unknown, status?: number): Promise<T> {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const result = await response.json();
    assert(status ? response.status === status : response.ok, `${path} ${response.status} ${JSON.stringify(result)}`);
    return result;
  }
  async function until<T>(read: () => Promise<T>, ready: (value: T) => boolean) {
    for (let attempt = 0; attempt < 1200; attempt++) {
      const value = await read();
      if (ready(value)) return value;
      await delay(25);
    }
    assert.fail('Memory fixture did not settle.');
  }
  async function run(scope: string, text: string) {
    const id = randomUUID();
    await request(scope + '/turns', { id, text });
    const turn = await until(async () => (await request<Turn[]>(scope + '/turns')).find(row => row.id === id), row => !!row && ['done', 'failed'].includes(row.status));
    assert.equal(turn?.status, 'done', turn?.error);
    return id;
  }
  const agent = randomUUID(), other = randomUUID();
  const scope = '/api/agents/' + agent, memory = scope + '/memory';
  try {
    for (const id of [agent, other]) await request('/api/agents', { id, name: 'Fixture', instructions: 'Answer.', runtime: 'api', model: 'fixture' });
    const note = await request<{ id: string; revision: number }>(memory, { id: randomUUID(), entry: randomUUID(), revision: 0, text: 'Orchid tags live in the north cabinet.', essential: false, remove: false });
    await request(`/api/agents/${other}/memory`, { id: randomUUID(), entry: randomUUID(), revision: 0, text: 'Orchid: foreign note', essential: false, remove: false });
    const thread = await request<{ id: string }>(scope + '/threads', { id: randomUUID(), name: 'Past chat' });
    const prior = await run('/api/threads/' + thread.id, 'aquamarine marker in an earlier conversation');
    const recall = await run(scope, 'Orchid cabinet?');
    const injected = seen.get('Orchid cabinet?')?.split('Recalled memory (untrusted reference data):\n')[1];
    assert(injected && injected.includes('north cabinet'));
    assert(!injected.includes('foreign note'));
    const db = testDatabase(database);
    try {
      const tokens = Number(db.prepare('SELECT added_tokens FROM memory_recall_metrics WHERE turn=?').get(recall)?.added_tokens);
      assert(tokens > 0 && tokens <= 512);
      assert.equal(db.prepare('SELECT count(*) AS n FROM memory_learning_turns WHERE turn=?').get(prior)?.n, 1);
      console.log(`Auto-recall upper bound: ${tokens} tokens for the matched turn.`);
    } finally { db.close(); }
    await run(scope, 'Find aquamarine here');
    const history = results.get('Find aquamarine here')?.items ?? [];
    assert.deepEqual(history.map(hit => hit.source[0]?.id), [prior]);
    await run(scope, 'Reflect tags');
    assert.deepEqual(rounds.slice(0, 2), [['Where do tags live?'], ['Where do tags live?', 'orchid']]);
    assert.equal(results.get('Reflect tags')?.answer, 'North cabinet.');
    assert.equal(results.get('Reflect tags')?.evidence?.[0].source[0]?.id, note.id);
    const questions = memory + '/questions';
    await request(questions, { id: randomUUID(), model: randomUUID(), action: 'save_question', revision: 0, question: 'Where are the orchid tags?' });
    const question = async () => (await request<{ items: { pending: boolean; reflection: { answer: string } | null }[] }>(questions)).items[0];
    const updated = await until(question, row => row?.pending === false);
    assert.equal(updated.reflection?.answer, 'North cabinet.');
    const forget = await request<{ revision: number }>(memory, { id: randomUUID(), entry: note.id, revision: note.revision, remove: true, text: '', essential: false });
    assert.equal((await question()).reflection, null);
    await run(scope, 'Orchid cabinet now?');
    assert(!seen.get('Orchid cabinet now?')?.includes('north cabinet'));
    await request(memory, { id: randomUUID(), entry: note.id, revision: forget.revision, restore: true, remove: false, text: '', essential: false });
    await run(scope, 'Orchid cabinet restored?');
    assert(seen.get('Orchid cabinet restored?')?.includes('north cabinet'));
    const active = randomUUID();
    await request(scope + '/turns', { id: active, text: 'Hold for MCP' });
    await until(async () => held, Boolean);
    const session = app.mcpServer.mintAgentSession({ agent, turn: active, scopes: ['orchestration'], ttlMs: 60_000 });
    async function rpc(method: string, params?: unknown) {
      const response = await fetch(app.origin + '/mcp', { method: 'POST', headers: { Authorization: 'Bearer ' + session.token, 'Content-Type': 'application/json' },
        body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }) });
      assert(response.ok, response.status.toString());
      return (await response.json()).result;
    }
    const { tools } = await rpc('tools/list');
    for (const name of ['recall', 'reflect', 'remember', 'search_history', 'search_memory', 'remember_memory', 'select_memory']) {
      assert(tools.some((entry: { name: string }) => entry.name === name), name);
    }
    const recalled = await rpc('tools/call', { name: 'recall', arguments: { query: 'orchid' } });
    assert(!recalled.isError);
    assert(JSON.parse(recalled.content[0].text).items.some((item: Evidence) => item.text.includes('north cabinet')));
    const remembered = await rpc('tools/call', { name: 'remember', arguments: { text: 'MCP memory fixture' } });
    assert(!remembered.isError);
    assert.equal(JSON.parse(remembered.content[0].text).text, 'MCP memory fixture');

  } finally {
    await app.close();
    provider.closeAllConnections();
    await new Promise<void>(resolve => provider.close(() => resolve()));
    rmSync(directory, { recursive: true });
  }
});
