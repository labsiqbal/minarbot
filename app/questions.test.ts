import { parseQuestionAnswer } from './src/question-answer.ts';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { createServer } from 'node:http';
import { testDatabase } from './test-support/database.ts';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { startApp } from './test-bridge.ts';

test('question cards persist, answer once in scope, and resume without repeating effects', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-questions-'));
  let free = false, invalid = false, asks = 0;
  const replies: string[] = [], prompts: string[] = [];
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const data = JSON.parse(raw); prompts.push(JSON.stringify(data.messages)); const calls = data.messages.flatMap((m: any) => m.tool_calls ?? []);
    const answers = data.messages.filter((m: any) => m.role === 'tool');
    let call;
    if (!calls.length) call = { id: 'effect', name: 'remember_memory', args: { text: 'Saved before question ' + data.messages.findLast((m: any) => m.role === 'user').content } };
    else if (!calls.some((c: any) => c.function.name === 'ask_user')) { asks++; call = { id: 'question', name: 'ask_user', args: { question: '<script>Which path?</script>', choices: invalid ? ['duplicate', 'duplicate'] : free ? [] : ['Yes', 'No'] } }; }
    else { assert.equal(calls[0].function.name, 'remember_memory'); replies.push(JSON.parse(answers.at(-1).content).answer); }
    res.end(JSON.stringify({ choices: [{ finish_reason: call ? 'tool_calls' : 'stop', message: call ? { tool_calls: [{ id: call.id, type: 'function', function: { name: call.name, arguments: JSON.stringify(call.args) } }] } : { content: 'User chose ' + replies.at(-1) } }] }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const options = { database: join(dir, 'db.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1` };
  let app = await startApp(options, { directory: dir, servers: [provider] });
  async function request(path: string, body?: unknown, expected?: number) {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const value = await response.json(); if (expected) assert.equal(response.status, expected, JSON.stringify(value)); else assert(response.ok, JSON.stringify(value)); return value;
  }
  async function until(path: string, id: string, predicate: (row: any) => boolean) {
    for (let n = 0; n < 200; n++) { const row = (await request(path)).find((r: any) => r.id === id); if (predicate(row)) return row; await new Promise(resolve => setTimeout(resolve, 10)); }
    assert.fail('Question task did not settle');
  }
  async function ask(path: string) {
    const id = randomUUID(); await request(path, { id, text: id + (path.startsWith('/api/channels/') ? ' @A' : '') });
    const turn = await until(path, id, row => row.questions?.length); assert.equal(turn.status, 'queued'); assert.equal(turn.paused, 1); return turn;
  }
  const answerPath = (path: string, turn: any) => `${path}/${turn.id}/questions/${turn.questions[0].id}/answer`;
  try {
    const a = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'A', model: 'fixture', instructions: 'Help' });
    const b = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'B', model: 'fixture', instructions: 'Help' });
    const c = await request('/api/channels', { id: randomUUID(), name: 'C', instructions: 'Help', lead: a.id, members: [a.id, b.id] });
    const side = await request(`/api/agents/${a.id}/threads`, { id: randomUUID(), name: 'Side' });
    const direct = `/api/agents/${a.id}/turns`, channel = `/api/channels/${c.id}/turns`, thread = `/api/threads/${side.id}/turns`;
    const first = await ask(direct), originalAsks = asks;
    await request(`${direct}/${first.id}/resume`, { id: randomUUID(), revision: first.revision, acknowledgeUncertain: true }, 409);
    await request(answerPath(`/api/agents/${b.id}/turns`, first), { id: randomUUID(), text: 'Yes' }, 404);
    await request(answerPath(channel, first), { id: randomUUID(), text: 'Yes' }, 404);
    await request(answerPath(direct, first), { id: randomUUID(), text: 'Wrong choice' }, 400);
    await app.close(); app = await startApp(options);
    const waiting = (await request(direct)).find((r: any) => r.id === first.id); assert.equal(waiting.questions[0].answer, null); assert.equal(asks, originalAsks);
    const command = { id: randomUUID(), text: 'Yes' }, endpoint = answerPath(direct, first), receipt = await request(endpoint, command);
    assert.deepEqual(await request(endpoint, command), receipt);
    await until(direct, first.id, row => row.status === 'done'); assert.equal(replies.at(-1), 'Yes'); assert.equal(asks, originalAsks);
    assert.equal((await request(`/api/agents/${a.id}/memory`)).items.length, 1);
    await request(endpoint, { id: randomUUID(), text: 'No' }, 409);
    await request(endpoint, { ...command, text: 'No' }, 409);
    await app.close(); app = await startApp(options); assert.deepEqual(await request(endpoint, command), receipt);
    for (const path of [channel, thread]) { const turn = await ask(path); await request(answerPath(direct, turn), { id: randomUUID(), text: 'No' }, 404); await request(answerPath(path, turn), { id: randomUUID(), text: 'No' }); await until(path, turn.id, row => row.status === 'done'); }
    free = true; const textTurn = await ask(direct); assert(prompts.at(-1)!.includes('User question cards (reference data)')); assert(prompts.at(-1)!.includes('Yes')); await request(answerPath(direct, textTurn), { id: randomUUID(), text: 'Custom answer' }); await until(direct, textTurn.id, row => row.status === 'done'); assert.equal(replies.at(-1), 'Custom answer');
    const cancelled = await ask(direct); await request(`${direct}/${cancelled.id}/cancel`, { id: randomUUID() }); await request(answerPath(direct, cancelled), { id: randomUUID(), text: 'Too late' }, 409);
    const archived = await ask(channel); await request(`/api/channels/${c.id}/edit`, { id: randomUUID(), revision: 1, name: 'C', instructions: 'Help', members: [a.id, b.id], lead: a.id, archived: true }); await request(answerPath(channel, archived), { id: randomUUID(), text: 'Late' }, 409);
    const restored = await ask(direct); await app.close(); const db = testDatabase(options.database); db.prepare("UPDATE turns SET status='interrupted',revision=revision+1 WHERE id=?").run(restored.id); db.close(); app = await startApp(options);
    await request(answerPath(direct, restored), { id: randomUUID(), text: 'Restored answer' }, 409);
    await request(answerPath(direct, restored), { id: randomUUID(), text: 'Restored answer', acknowledgeUncertain: true }); await until(direct, restored.id, row => row.status === 'done');
    const atomic = await ask(direct);
    await app.close(); const fault = testDatabase(options.database); fault.exec("CREATE TRIGGER reject_answer BEFORE INSERT ON question_answers BEGIN SELECT RAISE(ABORT, 'synthetic answer receipt failure'); END"); fault.close(); app = await startApp(options);
    const retry = { id: randomUUID(), text: 'Atomic answer' };
    await request(answerPath(direct, atomic), retry, 500);
    const untouched = (await request(direct)).find((r: any) => r.id === atomic.id); assert.equal(untouched.paused, 1); assert.equal(untouched.questions[0].answer, null);
    await app.close(); const recovered = testDatabase(options.database); recovered.exec('DROP TRIGGER reject_answer'); recovered.close(); app = await startApp(options);
    await request(answerPath(direct, atomic), retry); await until(direct, atomic.id, row => row.status === 'done'); assert.equal(replies.at(-1), 'Atomic answer');
    invalid = true; const bad = randomUUID(); await request(direct, { id: bad, text: bad }); const failed = await until(direct, bad, row => row.status === 'failed'); assert.match(failed.error, /question/); assert.equal(failed.questions.length, 0);
  } finally { await app.close(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(dir, { recursive: true, force: true }); }
});

test('answer recovery rejects malformed storage and preserves exact retry identity', () => {
  const valid = { id: randomUUID(), text: 'Yes', acknowledgeUncertain: false };
  assert.deepEqual(parseQuestionAnswer(JSON.parse(JSON.stringify(valid)), ['Yes', 'No']), valid);
  for (const value of [false, 0, '', null, [], {}, { ...valid, id: 1 }, { ...valid, text: 'Other' }, { ...valid, acknowledgeUncertain: 'yes' }, { ...valid, extra: true }]) assert.throws(() => parseQuestionAnswer(value, ['Yes', 'No']));
});
