import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { setTimeout } from 'node:timers/promises';
import { startApp } from './test-bridge.ts';

type Request = { model: string; messages: { role: string; content: string; tool_calls?: { function: { name: string } }[] }[]; tools?: { function: { name: string } }[] };
const baseTime = Date.UTC(2026, 10, 22, 10);
const answer = (content: string) => ({ choices: [{ finish_reason: 'stop', message: { content } }], usage: { prompt_tokens: 20, completion_tokens: 10, total_tokens: 30 } });
const tool = (name: string, args: object) => ({ choices: [{ finish_reason: 'tool_calls', message: { content: null, tool_calls: [{ id: name, type: 'function', function: { name, arguments: JSON.stringify(args) } }] } }] });
const judge = (met: boolean, reason: string) => answer(JSON.stringify({ met, reason, evidence: met ? ['answer'] : [] }));
const judging = (body: Request) => body.messages[0]?.content.startsWith('Judge whether the target is met.');

async function fixture(reply: (body: Request) => object | undefined) {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-goal-test-'));
  const calls: Request[] = [];
  const provider = createServer(async (request, response) => {
    if (request.method === 'GET') {
      response.writeHead(200, { 'Content-Type': 'application/json' });
      response.end(JSON.stringify({ data: [{ id: 'work' }, { id: 'judge' }] }));
      return;
    }
    let raw = '';
    for await (const chunk of request) raw += chunk;
    const body: Request = JSON.parse(raw);
    calls.push(body);
    const result = reply(body);
    if (!result) return;
    response.writeHead(200, { 'Content-Type': 'application/json' });
    response.end(JSON.stringify(result));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address();
  if (!address || typeof address === 'string') throw new Error('Missing fixture address.');
  const options = { database: join(directory, 'state.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1`, defaultModel: 'work', stream: false, fixedClock: baseTime };
  let app = await startApp(options, { directory, servers: [provider] });
  return {
    calls,
    async request<T = Record<string, unknown>>(path: string, body?: object, status?: number): Promise<T> {
      const response = await fetch(app.origin + path, body ? { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body) } : {});
      const value = await response.json();
      assert(status ? response.status === status : response.ok, `${path} ${response.status} ${JSON.stringify(value)}`);
      return value;
    },
    async restart(at: number, crash = false) {
      if (crash) await app.crash();
      else await app.close();
      app = await startApp({ ...options, fixedClock: at });
    },
    async close() {
      try { await app.close(); }
      finally {
        provider.closeAllConnections();
        await new Promise<void>(resolve => provider.close(() => resolve()));
        rmSync(directory, { recursive: true, force: true });
      }
    },
  };
}

async function until<T>(read: () => Promise<T>, done: (value: T) => boolean) {
  for (let attempt = 0; attempt < 150; attempt++) {
    const value = await read();
    if (done(value)) return value;
    await setTimeout(60);
  }
  assert.fail('Goal or loop did not settle: ' + JSON.stringify(await read()));
}

type Agent = { id: string; revision: number };
type Goal = { id: string; current_turn: string; status: string; turn_count: number; reason: string; max_tokens: number };
type Turn = { id: string; status: string; reply: string; questions: { id: string }[] };
const agentOptions = { name: 'Goal fixture', model: 'work', instructions: 'Run synthetic tasks.', runtime: 'api' };

test('goal judges work separately, uses the configured model and keeps ordinary requests unchanged', async t => {
  let judgments = 0;
  const app = await fixture(body => {
    if (judging(body)) return ++judgments === 1 ? judge(false, 'Include exactly FIXTURE_READY.') : judge(true, 'The answer is FIXTURE_READY.');
    return answer(body.messages.at(-1)?.content.includes('Continue the goal.') ? 'FIXTURE_READY' : 'First answer.');
  });
  t.after(() => app.close());
  const { id, revision } = await app.request<Agent>('/api/agents', { ...agentOptions, id: randomUUID() });
  const path = `/api/agents/${id}`;
  await app.request(`${path}/edit`, { id: randomUUID(), revision, name: agentOptions.name, model: 'work', instructions: agentOptions.instructions, goalJudgeModel: 'judge' });
  const ordinary = randomUUID();
  await app.request(`${path}/turns`, { id: ordinary, text: 'Say hello.' });
  await until(() => app.request<Turn[]>(`${path}/turns`), rows => rows.some(row => row.id === ordinary && row.status === 'done'));
  const ordinaryCalls = app.calls.length;
  assert.equal(ordinaryCalls, 1);
  assert(!app.calls[0].tools?.some(spec => spec.function.name === 'set_goal' || spec.function.name === 'set_loop'));

  const command = { id: randomUUID(), target: 'Reply with exactly FIXTURE_READY.', maxTurns: 3, maxTokens: 20000 };
  await app.request(`${path}/goal`, command);
  const goal = await until(() => app.request<Goal>(`${path}/goal`), goal => goal.status === 'met');
  assert.equal(goal.turn_count, 2);
  assert.equal(goal.max_tokens, 20000);
  assert.equal(goal.reason, 'The answer is FIXTURE_READY.');
  assert.equal((await app.request<Goal>(`${path}/goal`, command)).id, command.id);
  await app.request(`${path}/goal`, { ...command, maxTurns: 2 }, 409);
  assert.deepEqual(app.calls.slice(ordinaryCalls).map(call => call.model), ['work', 'judge', 'work', 'judge']);
  const result = await app.request<{ judgments: { number: number; met: number; reason: string }[] }>(`/api/turns/${goal.current_turn}/inspect`);
  assert.deepEqual(result.judgments.map(row => [row.number, row.met, row.reason]), [
    [1, 0, 'Include exactly FIXTURE_READY.'], [2, 1, 'The answer is FIXTURE_READY.'],
  ]);
});

test('goal can be set by an agent, pauses for you and resumes on answer', async t => {
  const app = await fixture(body => {
    if (judging(body)) return judge(true, 'The requested answer is present.');
    if (body.messages.some(m => m.tool_calls?.some(c => c.function.name === 'ask_user'))) return answer('Blue');
    if (body.messages.some(m => m.tool_calls?.some(c => c.function.name === 'set_goal'))) return tool('ask_user', { question: 'Which color?', choices: ['Blue', 'Green'] });
    return tool('set_goal', { target: 'Reply with the selected color.' });
  });
  t.after(() => app.close());
  const { id } = await app.request<Agent>('/api/agents', { ...agentOptions, id: randomUUID() });
  const path = `/api/agents/${id}`;
  const turn = randomUUID();
  await app.request(`${path}/turns`, { id: turn, text: 'Set a goal to reply with my selected color.' });
  await until(() => app.request<Goal>(`${path}/goal`), value => value?.status === 'needs_you');
  const rows = await app.request<Turn[]>(`${path}/turns`);
  const question = rows.find(row => row.id === turn)?.questions[0];
  assert(question);
  assert.equal(app.calls.filter(judging).length, 0);
  await app.restart(baseTime + 1000);
  assert.equal((await app.request<Goal>(`${path}/goal`)).status, 'needs_you');
  await app.request(`${path}/turns/${turn}/questions/${question.id}/answer`, { id: randomUUID(), text: 'Blue' });
  await until(() => app.request<Goal>(`${path}/goal`), value => value.status === 'met');
  assert.equal(app.calls.filter(judging).length, 1);
});

test('stop, token limits and restart fence prevent later work', async t => {
  const app = await fixture(() => undefined);
  t.after(() => app.close());
  const { id } = await app.request<Agent>('/api/agents', { ...agentOptions, id: randomUUID() });
  const path = `/api/agents/${id}`;
  const getGoal = () => app.request<Goal>(`${path}/goal`);
  const start = (target: string, options = {}) => app.request<Goal>(`${path}/goal`, { id: randomUUID(), target, ...options });
  const first = await start('Wait for stop.');
  await until(async () => app.calls.length, count => count === 1);
  assert.equal((await app.request<Goal>(`${path}/goal/stop`, { id: randomUUID(), goalId: first.id })).status, 'stopped');
  await start('Exhaust the budget.', { maxTokens: 1 });
  await until(getGoal, goal => goal.status === 'token_limit');
  assert.equal(app.calls.length, 1);
  await start('Wait for restart.');
  await until(async () => app.calls.length, count => count === 2);
  await app.restart(baseTime, true);
  assert.equal((await getGoal()).status, 'interrupted');
});

test('loops use routine occurrences after restart and stop on until or max runs', async t => {
  let judgments = 0;
  const app = await fixture(body => judging(body) ? judge(++judgments === 2, judgments === 2 ? 'Condition met.' : 'Keep checking.') : answer('SYNTHETIC_READY'));
  t.after(() => app.close());
  const { id } = await app.request<Agent>('/api/agents', { ...agentOptions, id: randomUUID() });
  const path = `/api/agents/${id}`;
  const command = { id: randomUUID(), prompt: 'Check synthetic readiness.', intervalSeconds: 60, until: 'Reply contains SYNTHETIC_READY', maxRuns: 4 };
  await app.request(`${path}/loop`, command, 201);
  await app.request(`${path}/loop`, command, 201);
  const second = { id: randomUUID(), prompt: 'Repeat a synthetic status.', intervalSeconds: 60, maxRuns: 1 };
  await app.request(`${path}/loop`, second, 201);
  assert.equal((await app.request(`/api/routines/${command.id}`)).next_at, baseTime + 60000);
  await app.restart(baseTime + 60000);
  const runs = (routine: string) => app.request<Turn[]>(`/api/routines/${routine}/runs`);
  await until(() => runs(command.id), value => value[0]?.status === 'done');
  await until(() => runs(second.id), value => value[0]?.status === 'done');
  assert.equal((await app.request(`/api/routines/${command.id}`)).enabled, 1);
  assert.equal((await app.request(`/api/routines/${second.id}`)).enabled, 0);
  await app.restart(baseTime + 120000);
  await until(() => app.request(`/api/routines/${command.id}`), routine => routine.enabled === 0);
  assert.equal((await runs(command.id)).length, 2);
  assert.equal((await runs(second.id)).length, 1);
  assert.equal(judgments, 2);
});

test('ACP work and judge use distinct sessions with tools disabled for the judge', async t => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-acp-goal-'));
  let app: Awaited<ReturnType<typeof startApp>> | undefined;
  t.after(async () => { try { await app?.close(); } finally { rmSync(dir, { recursive: true, force: true }); } });
  const agent = randomUUID();
  const executable = join(dir, 'fixture-acp');
  writeFileSync(executable, `#!${process.execPath}
const readline = require('node:readline');
if (process.argv.includes('--version')) { console.log('1.0.0'); process.exit(0); }
const input = readline.createInterface({ input: process.stdin });
let judged = false;
const send = value => console.log(JSON.stringify(value));
input.on('line', raw => {
  const { id, method, params } = JSON.parse(raw);
  let result = {};
  if (method === 'initialize') {
    judged = params.clientCapabilities.fs.readTextFile === false;
    result = { protocolVersion: 1 };
  } else if (method === 'session/new') {
    if (judged !== (params.mcpServers.length === 0)) throw Error('Judge exposed MCP');
    result = { sessionId: judged ? 'judge' : 'work', modes: { availableModes: [{ id: 'default' }] } };
  } else if (method === 'session/prompt') {
    if (params.sessionId !== (judged ? 'judge' : 'work')) throw Error('Judge reused work session');
    const text = judged
      ? JSON.stringify({ met: true, reason: 'Answer matches.', evidence: ['answer'] })
      : 'ACP_READY';
    send({ jsonrpc: '2.0', method: 'session/update', params: {
      sessionId: params.sessionId, update: { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text } }
    } });
    result = { stopReason: 'end_turn' };
  }
  if (id !== undefined) send({ jsonrpc: '2.0', id, result });
});
`, { mode: 0o700 });
  const instance = app = await startApp({ database: join(dir, 'data', 'state.sqlite'), harness: { executables: { opencode: executable } },
    defaultModel: 'work', baseURL: 'http://127.0.0.1:9/v1', workspaces: [{ agent, directory: dir, write: false }] });
  async function request<T>(path: string, body?: object): Promise<T> {
    const response = await fetch(instance.origin + path, body ? { method: 'POST', headers: { Origin: instance.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body) } : {});
    const value = await response.json();
    assert(response.ok, JSON.stringify(value));
    return value;
  }
  await request('/api/agents', { ...agentOptions, id: agent, runtime: 'opencode' });
  const path = `/api/agents/${agent}`;
  const turn = randomUUID();
  await request(`${path}/goal`, { id: turn, target: 'Reply with ACP_READY.' });
  const goal = await until(() => request<Goal>(`${path}/goal`), item => !['active', 'judging'].includes(item.status));
  assert.equal(goal.status, 'met', goal.reason);
  const rows = await request<Turn[]>(`${path}/turns`);
  assert.equal(rows.find(row => row.id === turn)?.reply, 'ACP_READY');
});
