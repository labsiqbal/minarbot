// Other goal cases moved to crates/minarbot-e2e/tests/e2e/goals.rs (#89).
// This case stays until bug B9 in docs/test-ledger.md is fixed: a goal's
// maxTokens is not enforced before a harness turn.
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { setTimeout } from 'node:timers/promises';
import { startApp } from './test-bridge.ts';

type Request = { model: string; messages: { role: string; content: string; tool_calls?: { function: { name: string } }[] }[]; tools?: { function: { name: string } }[] };
const baseTime = Date.UTC(2026, 10, 22, 10);

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
const agentOptions = { name: 'Goal fixture', model: 'work', instructions: 'Run synthetic tasks.', runtime: 'api' };

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
