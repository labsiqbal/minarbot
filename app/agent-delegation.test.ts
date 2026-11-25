import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { startApp } from './test-bridge.ts';

// The other cases moved to crates/minarbot-e2e/tests/e2e/fx_delegation.rs
// (#89). restart-wait stays here for B1: its child call count includes no
// model step after ask_user, which the harness takes today.
import type { AgentLimits } from './test-support/agent-delegation.ts';

type Script = (body: any) => Promise<{ name: string; args: unknown } | string> | { name: string; args: unknown } | string;
async function fixture(script: Script, agentLimits?: Partial<AgentLimits>) {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-delegation-')), calls: any[] = [];
  const provider = createServer({ keepAliveTimeout: 0 }, async (req, res) => {
    let raw = ''; for await (const part of req) raw += part;
    const body = JSON.parse(raw); calls.push(body);
    try {
      const result = await script(body), message = typeof result === 'string' ? { content: result } : { tool_calls: [{ id: randomUUID(), type: 'function', function: { name: result.name, arguments: JSON.stringify(result.args) } }] };
      res.setHeader('Content-Type', 'application/json'); res.end(JSON.stringify({ choices: [{ finish_reason: typeof result === 'string' ? 'stop' : 'tool_calls', message }] }));
    } catch (error) { res.writeHead(500); res.end(JSON.stringify({ error: String(error) })); }
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const options = { database: join(dir, 'db.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1`, concurrency: 12, agentLimits };
  let app = await startApp(options, { directory: dir, servers: [provider] }), stopped = false;
  const stop = async () => { if (!stopped) { await app.close(); stopped = true; } };
  const api = async (path: string, body?: unknown) => {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json', Connection: 'close' }, body: body ? JSON.stringify(body) : undefined });
    const value = await response.json(); assert(response.ok, JSON.stringify(value)); return value;
  };
  const until = async (path: string, predicate: (rows: any[]) => boolean): Promise<any[]> => {
    const deadline = Date.now() + 20_000;
    while (Date.now() < deadline) { const rows = await api(path); if (predicate(rows)) return rows; await new Promise(resolve => setTimeout(resolve, 20)); }
    assert.fail(`Timed out: ${path} ${JSON.stringify(await api(path))}`);
  };
  const agent = async (name: string) => api('/api/agents', { runtime: 'api', id: randomUUID(), name, model: name, instructions: `${name} role` });
  return { api, until, agent, calls, database: options.database, stop, restart: async () => { await stop(); app = await startApp(options); stopped = false; }, close: async () => { await stop(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(dir, { recursive: true, force: true }); } };
}
const results = (body: any) => body.messages.filter((row: any) => row.role === 'tool').map((row: any) => JSON.parse(row.content));
const gate = () => { let release = () => {}; const promise = new Promise<void>(resolve => { release = resolve; }); return { promise, release }; };

for (const mode of ['restart-wait']) test(`durable task wait: ${mode}`, async () => {
  let target = ''; const child = gate(); let childCalls = 0;
  const f = await fixture(async body => {
    if (body.model === 'Child') {
      childCalls++;
      if (mode === 'restart-wait' && !results(body).length) return { name: 'ask_user', args: { question: 'Finish child?', choices: ['Yes'] } };
      await child.promise; return 'Child result';
    }
    if (body.messages.filter((row: any) => row.role === 'user').at(-1)?.content.startsWith('Delegated task completed')) return 'Late result received';
    const rs = results(body);
    if (!rs.length) return { name: 'delegate_task', args: { agent: target, task: 'Child work', wait: mode === 'delegate-wait' } };
    if (rs.at(-1).taskId && rs.at(-1).status === 'queued') return { name: 'wait_task', args: { taskId: rs.at(-1).taskId, timeoutSeconds: mode === 'timeout' ? 1 : 600 } };
    return JSON.stringify(rs.at(-1));
  });
  try {
    target = (await f.agent('Child')).id; const parent = await f.agent('Parent'), path = `/api/agents/${parent.id}/turns`, id = randomUUID();
    await f.api(path, { id, text: 'Start task' });
    const waiting = (await f.until(path, rows => rows.some(row => row.id === id && row.status === 'queued' && row.agentMessages.length))).find(row => row.id === id);
    assert.equal(waiting.reply, null);
    if (mode === 'timeout') {
      const done = (await f.until(path, rows => rows.some(row => row.id === id && row.status === 'done'))).find(row => row.id === id);
      assert.equal(JSON.parse(done.reply).timedOut, true); child.release();
      await f.until(path, rows => rows.some(row => row.text.startsWith('Delegated task completed') && row.status === 'done'));
    } else {
      if (mode === 'restart-wait') {
        const item = waiting.agentMessages[0], childPath = `/api/threads/${item.thread}/turns`;
        const row = (await f.until(childPath, rows => rows[0]?.questions?.length))[0];
        assert.equal((await f.api(path)).find((row: any) => row.id === id).agentMessages[0].status, 'waiting');
        await f.restart(); child.release();
        await f.api(`${childPath}/${item.id}/questions/${row.questions[0].id}/answer`, { id: randomUUID(), text: 'Yes' });
      } else { if (mode === 'restart-child') { await f.until(`/api/threads/${waiting.agentMessages[0].thread}/turns`, rows => rows[0]?.status === 'running'); await f.restart(); } child.release(); }
      const done = (await f.until(path, rows => rows.some(row => row.id === id && row.status === 'done'))).find(row => row.id === id), result = JSON.parse(done.reply);
      assert.equal(result.status, mode === 'restart-child' ? 'uncertain' : 'done');
      if (mode !== 'restart-child') assert.equal(result.result, 'Child result');
      assert.equal(childCalls, mode === 'restart-wait' ? 2 : 1);
      await f.restart(); assert.equal((await f.api(path)).filter((row: any) => row.text.startsWith('Delegated task completed')).length, 0);
    }
  } finally { child.release(); await f.close(); }
});
