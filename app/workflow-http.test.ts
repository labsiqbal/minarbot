import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHmac, randomUUID } from 'node:crypto';
import { createServer } from 'node:http';
import { testDatabase } from './test-support/database.ts';
import { workflowAutosaveDraft } from './src/workflow-editor-state.ts';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { startApp } from './test-bridge.ts';

test('legacy unsupported workflow steps fail once without producing output', async t => {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-workflow-legacy-kind-'));
  const database = join(directory, 'state.sqlite');
  let calls = 0;
  const provider = createServer((_req, res) => { calls++; res.writeHead(500).end(); });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address();
  assert(address && typeof address !== 'string');
  const app = await startApp({ database, baseURL: `http://127.0.0.1:${address.port}/v1` }, { directory, servers: [provider] });
  t.after(async () => {
    await app.close();
    provider.closeAllConnections();
    await new Promise<void>(resolve => provider.close(() => resolve()));
    rmSync(directory, { recursive: true, force: true });
  });
  async function request(path: string, body?: unknown, status = 200) {
    const response = await fetch(app.origin + path, {
      method: body === undefined ? 'GET' : 'POST',
      headers: { Origin: app.origin, 'Content-Type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const result = await response.json();
    assert.equal(response.status, status, JSON.stringify(result));
    return result;
  }

  const agent = await request('/api/agents', { id: randomUUID(), runtime: 'api', name: 'Legacy', model: 'fixture', instructions: 'Test.' }, 201);
  const list = `/api/agents/${agent.id}/workflows`;
  for (const kind of ['image', 'asset', 'file-output', 'message-trigger']) {
    const id = randomUUID();
    const graph = { entry: 'legacy', maxSteps: 2, nodes: [{ id: 'legacy', kind, next: 'out' }, { id: 'out', kind: 'output' }] };
    assert.deepEqual(await request(list, { id, name: kind, revision: 0, graph }, 400), { error: 'Unsupported workflow step type.' });

    // Simulate a saved version predating current validation.
    const db = testDatabase(database);
    try {
      db.prepare('INSERT INTO workflows(id,agent,name,revision) VALUES(?,?,?,1)').run(id, agent.id, kind);
      db.prepare('INSERT INTO workflow_versions(workflow,revision,graph) VALUES(?,1,?)').run(id, JSON.stringify(graph));
    } finally { db.close(); }
    const path = `/api/workflows/${id}`;
    assert.deepEqual((await request(path)).graph, graph);
    const launch = { id: randomUUID(), revision: 1, input: 'Preserve this input' };
    await request(`${path}/run`, launch);
    const run = await request(`/api/workflow-runs/${launch.id}`);
    const error = `Unsupported workflow step type: ${kind}.`;
    assert.equal(run.status, 'failed');
    assert.deepEqual(run.steps.map((step: { node: string; status: string; detail: string }) => [step.node, step.status, step.detail]), [['legacy', 'failed', error]]);
    assert.equal(run.error, error);
    assert.equal(run.last, launch.input);
  }
  assert.equal(calls, 0);
  assert.deepEqual((await request('/api/workflow-notifications')).notifications, []);
});

test('model conditions choose one branch without tools and reject ambiguous answers', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-workflow-condition-'));
  let answer = 'yes'; const payloads: any[] = [];
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    payloads.push(JSON.parse(raw));
    res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: answer } }] }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const options = { database: join(dir, 'state.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1` };
  let app = await startApp(options, { directory: dir, servers: [provider] });
  async function request(path: string, body?: unknown) {
    const res = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const data = await res.json(); assert(res.ok, JSON.stringify(data)); return data;
  }
  try {
    const agent = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Condition', model: 'owner', instructions: 'Evaluate.' });
    const graph = { entry: 'choose', maxSteps: 4, nodes: [
      { id: 'choose', kind: 'condition', operator: 'model', model: 'decision-model', value: 'Is the input acceptable?', yes: 'accepted', no: 'declined' },
      { id: 'accepted', kind: 'output' }, { id: 'declined', kind: 'output' },
    ] };
    const workflow = await request(`/api/agents/${agent.id}/workflows`, { id: randomUUID(), revision: 0, name: 'Condition', graph });
    let failedRun: any;
    const decisions = [
      [' yes\n', 'accepted'], ['NO', 'declined'], ['maybe', null],
      [' YES\n', 'accepted'], ['no', 'declined'], ['yes because it matches', null],
    ] as const;
    for (const [value, expected] of decisions) {
      answer = value;
      const run = await request(`/api/workflows/${workflow.id}/run`, { id: randomUUID(), revision: 1, input: 'ORIGINAL_DATA' });
      let current;
      for (let attempt = 0; attempt < 100; attempt++) {
        current = await request(`/api/workflow-runs/${run.id}`);
        if (['done', 'failed'].includes(current.status)) break;
        await new Promise(resolve => setTimeout(resolve, 20));
      }
      assert.equal(current.last, 'ORIGINAL_DATA');
      assert.equal(current.status, expected ? 'done' : 'failed');
      assert.equal(current.steps.length, expected ? 2 : 1);
      assert.equal(current.steps[0].status, expected ? 'done' : 'failed');
      if (expected) assert.equal(current.cursor, expected);
      else {
        assert.equal(current.error, 'Model condition must answer yes or no.');
        failedRun = current;
      }
    }
    assert.equal(payloads.length, decisions.length);
    for (const payload of payloads) { assert.equal(payload.model, 'decision-model'); assert.equal(payload.tools, undefined); }
    const edited = { ...graph, nodes: [{ ...graph.nodes[0], model: 'fixed-model', value: 'Fixed question', yes: 'declined', no: 'accepted' }, ...graph.nodes.slice(1)] };
    await request(`/api/workflows/${workflow.id}/edit`, { id: randomUUID(), revision: 1, name: 'Fixed', graph: edited });
    answer = 'yes';
    const retry = { id: randomUUID(), revision: failedRun.revision }, retryPath = `/api/workflow-runs/${failedRun.id}/retry`;
    await request(retryPath, retry); await request(retryPath, retry);
    let recovered;
    for (let attempt = 0; attempt < 100; attempt++) {
      recovered = await request(`/api/workflow-runs/${failedRun.id}`);
      if (recovered.status === 'done') break;
      await new Promise(resolve => setTimeout(resolve, 20));
    }
    assert.equal(recovered.status, 'done'); assert.equal(recovered.version, 1); assert.deepEqual(recovered.graph, graph);
    assert.equal(recovered.cursor, 'accepted'); assert.equal(recovered.steps.length, 3); assert.equal(recovered.steps[0].status, 'failed');
    assert.equal(recovered.last, 'ORIGINAL_DATA'); assert.equal(payloads.length, decisions.length + 1);
    const retryPayload = payloads[decisions.length];
    assert.equal(retryPayload.model, 'fixed-model'); assert.equal(retryPayload.tools, undefined);
    assert(JSON.stringify(retryPayload.messages).includes('Fixed question'));
    assert.equal(JSON.parse(recovered.steps[1].settings).model, 'fixed-model');
    await app.close(); app = await startApp(options);
    await request(retryPath, retry);
    assert.deepEqual((await request(`/api/workflow-runs/${failedRun.id}`)).steps, recovered.steps);
    assert.equal(payloads.length, decisions.length + 1);
    const stale = await fetch(app.origin + retryPath, { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify({ ...retry, id: randomUUID() }) });
    assert.equal(stale.status, 409); await stale.body?.cancel();
    assert.equal((await request(`/api/agents/${agent.id}/turns`)).length, 0);
  } finally { await app.close(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(dir, { recursive: true, force: true }); }
});

test('workflow HTTP versions, model steps, approval receipts and cancellation survive restart', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-workflow-http-')), models: string[] = [], toolNames: string[][] = [];
  let hold = false, forceTool = false, answer = 'RESULT';
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const data = JSON.parse(raw); models.push(data.model);
    toolNames.push((data.tools ?? []).map((tool: any) => tool.function.name));
    if (hold) return;
    if (forceTool) return res.end(JSON.stringify({ choices: [{ finish_reason: 'tool_calls', message: { content: null, tool_calls: [{ id: 'forced-file', type: 'function', function: { name: 'create_text_file', arguments: JSON.stringify({ name: 'forbidden.txt', text: 'Must not be saved' }) } }] } }] }));
    res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: answer } }] }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const options = { database: join(dir, 'state.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1` };
  let app = await startApp(options, { directory: dir, servers: [provider] });
  async function request(path: string, data?: unknown, status = 200) {
    const res = await fetch(app.origin + path, { method: data ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: data ? JSON.stringify(data) : undefined });
    const result = await res.json(); assert.equal(res.status, status, JSON.stringify(result)); return result;
  }
  async function until(path: string, ready: (data: any) => boolean) {
    for (let i = 0; i < 150; i++) { const data = await request(path); if (ready(data)) return data; await new Promise(resolve => setTimeout(resolve, 20)); }
    assert.fail('Workflow did not settle');
  }
  try {
    const agent = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Owner', model: 'owner-model', instructions: 'Help.' }, 201);
    const graph = { entry: 'model', maxSteps: 4, positions: { model: { x: 501, y: 702 } }, nodes: [
      { id: 'model', kind: 'model', model: 'override-model', prompt: 'Process', next: 'approve' },
      { id: 'approve', kind: 'approval', prompt: 'Accept?', yes: 'out', no: 'model' }, { id: 'out', kind: 'output' },
    ] };
    const create = { id: randomUUID(), revision: 0, name: 'Workflow', graph }, list = `/api/agents/${agent.id}/workflows`;
    const workflow = await request(list, create); assert.deepEqual(await request(list, create), workflow);
    const launch = { id: randomUUID(), revision: 1, input: 'INPUT' }, runPath = `/api/workflow-runs/${launch.id}`;
    await request(`/api/workflows/${workflow.id}/run`, launch);
    let run = await until(runPath, data => data.status === 'approval');
    assert.deepEqual(models, ['override-model']); assert.equal(run.last, 'RESULT');
    assert.equal(toolNames.length, 1); assert.equal(toolNames[0].length, 0, 'Model workflow steps must not receive agent tools');
    await request(`/api/workflows/${workflow.id}/edit`, { ...create, id: randomUUID(), revision: 0 }, 409);
    await request(`/api/workflows/${workflow.id}/edit`, { ...create, id: randomUUID(), revision: 1, graph: { entry: 'out', maxSteps: 1, nodes: [{ id: 'out', kind: 'output' }] } });
    const latest = await request(`/api/workflows/${workflow.id}`);
    assert.equal(latest.revision, 2); assert.equal(latest.graph.entry, 'out');
    await request(`/api/workflows/${randomUUID()}`, undefined, 404);
    const versionsPath = `/api/workflows/${workflow.id}/versions`;
    assert.deepEqual((await request(versionsPath)).versions.map((row: any) => row.revision), [2, 1]);
    assert.deepEqual((await request(`${versionsPath}?before=2`)).versions[0].graph, graph);
    for (const cursor of ['0', '-1', '1.5', 'NaN', '9007199254740992']) await request(`${versionsPath}?before=${cursor}`, undefined, 400);
    await request(`${versionsPath}?unknown=1`, undefined, 400);
    await app.close(); app = await startApp(options);
    assert.equal((await request(runPath)).status, 'approval');
    await request(`/api/workflows/${workflow.id}/run`, launch); assert.equal((await request(`/api/workflows/${workflow.id}/runs`)).length, 1);
    const approve = { id: randomUUID(), revision: run.revision, accepted: true };
    await request(`${runPath}/approve`, { ...approve, accepted: 'yes' }, 400);
    await request(`${runPath}/approve`, { ...approve, accepted: undefined }, 400);
    await request(`${runPath}/approve`, approve);
    const done = await until(runPath, data => data.status === 'done');
    assert.deepEqual(done.steps.map((step: { node: string }) => step.node), ['model', 'approve', 'out']);
    assert.equal(done.cursor, 'out');
    await request(`${runPath}/approve`, approve);
    await request(`${runPath}/approve`, { ...approve, id: randomUUID() }, 409);
    assert.equal((await request(runPath)).version, 1); assert.equal(models.length, 1);
    await request(`/api/workflows/${workflow.id}/edit`, { ...create, id: randomUUID(), revision: 2 });
    forceTool = true;
    const forced = await request(`/api/workflows/${workflow.id}/run`, { id: randomUUID(), revision: 3, input: 'FORCE TOOL' });
    const failed = await until(`/api/workflow-runs/${forced.id}`, data => data.status === 'failed');
    assert.equal(failed.steps[0].status, 'failed');
    assert.equal((await request(`/api/threads/${failed.steps[0].thread}/files`)).length, 0);
    forceTool = false;
    hold = true; const second = { id: randomUUID(), revision: 3, input: 'HOLD' };
    await request(`/api/workflows/${workflow.id}/run`, second);
    const secondPath = `/api/workflow-runs/${second.id}`;
    run = await until(secondPath, data => data.steps[0]?.task_status === 'running');
    await app.close(); app = await startApp(options);
    run = await request(secondPath); assert.equal(run.steps[0].task_status, 'interrupted'); assert.equal(run.status, 'waiting');
    const count = models.length;
    const cancel = { id: randomUUID(), revision: run.revision };
    await request(`${secondPath}/cancel`, cancel); await request(`${secondPath}/cancel`, cancel);
    assert.equal((await request(secondPath)).status, 'cancelled'); assert.equal((await request(secondPath)).steps[0].status, 'cancelled'); assert.equal(models.length, count);
    hold = false;
    await request(`/api/workflows/${workflow.id}/edit`, { ...create, id: randomUUID(), revision: 3, graph: { entry: 'work', maxSteps: 2, nodes: [
      { id: 'work', kind: 'agent', agent: agent.id, prompt: 'Agent tools remain available.', next: 'out' }, { id: 'out', kind: 'output' },
    ] } });
    const agentRun = await request(`/api/workflows/${workflow.id}/run`, { id: randomUUID(), revision: 4, input: 'AGENT' });
    const agentResult = await until(`/api/workflow-runs/${agentRun.id}`, data => data.status === 'done');
    assert.equal(agentResult.last, 'RESULT');
    assert.deepEqual(agentResult.steps.map((step: { status: string }) => step.status), ['done', 'done']);
    assert(toolNames.at(-1)!.includes('create_text_file')); assert(toolNames.at(-1)!.includes('set_routine_flow'));
    const restorePath = `/api/workflows/${workflow.id}/restore`, restore = { id: randomUUID(), revision: 4, version: 1 };
    await request(restorePath, { ...restore, version: '1' }, 400);
    await request(restorePath, { ...restore, version: 999 }, 404);
    await request(restorePath, { ...restore, revision: 3 }, 409);
    const restored = await request(restorePath, restore);
    assert.equal(restored.revision, 5); assert.deepEqual(restored.graph, graph);
    assert.equal((await request(`/api/workflow-runs/${agentRun.id}`)).version, 4);
    assert.equal((await request(`/api/workflow-runs/${agentRun.id}`)).graph.entry, 'work');
    await app.close(); app = await startApp(options);
    assert.deepEqual(await request(restorePath, restore), restored);
    assert.equal((await request(versionsPath)).versions.length, 5);
    await request(restorePath, { ...restore, version: 2 }, 409);
    const rejectedRun = await request(`/api/workflows/${workflow.id}/run`, { id: randomUUID(), revision: 5, input: 'REJECT' });
    const rejectedPath = `/api/workflow-runs/${rejectedRun.id}`;
    const awaiting = await until(rejectedPath, data => data.status === 'approval');
    const reject = { id: randomUUID(), revision: awaiting.revision, accepted: false }, callsBeforeReject = models.length;
    await request(`${rejectedPath}/approve`, reject);
    let rejected = await request(rejectedPath);
    assert.equal(rejected.status, 'failed'); assert.equal(rejected.error, 'Workflow approval denied.');
    assert.equal(rejected.steps.length, 2); assert.equal(rejected.steps[1].detail, 'rejected'); assert.equal(rejected.last, 'RESULT');
    await app.close(); app = await startApp(options);
    await request(`${rejectedPath}/approve`, reject);
    await request(`${rejectedPath}/approve`, { ...reject, id: randomUUID(), accepted: true }, 409);
    await new Promise(resolve => setTimeout(resolve, 1100));
    rejected = await request(rejectedPath);
    assert.equal(rejected.status, 'failed'); assert.equal(rejected.steps.length, 2); assert.equal(models.length, callsBeforeReject);
    assert.equal((await request(`/api/agents/${agent.id}/turns`)).length, 0);

    const draft = { id: 'draft', kind: 'agent', agent: agent.id, prompt: 'Draft', next: 'approve' };
    const retryGraph = { entry: 'draft', maxSteps: 8, nodes: [draft, { id: 'approve', kind: 'approval', prompt: 'Accept?', yes: 'out', no: 'draft' }, { id: 'out', kind: 'output' }] };
    const retryable = await request(list, { id: randomUUID(), revision: 0, name: 'Retry evidence', graph: retryGraph });
    const retryWorkflow = `/api/workflows/${retryable.id}`;
    answer = 'x'.repeat(256 * 1024 + 1);
    const oversized = await request(retryWorkflow + '/run', { id: randomUUID(), revision: 1, input: 'INPUT' });
    const failedPath = `/api/workflow-runs/${oversized.id}`;
    const tooLarge = await until(failedPath, data => data.status === 'failed');
    assert.match(tooLarge.error, /Step result exceeds 256 KiB/);
    assert.equal(tooLarge.steps[0].status, 'failed');
    await request(retryWorkflow + '/edit', { id: randomUUID(), revision: 1, name: 'Changed kind', graph: {
      entry: 'draft', maxSteps: 2, nodes: [{ id: 'draft', kind: 'condition', operator: 'equals', value: 'INPUT', yes: 'out', no: 'out' }, { id: 'out', kind: 'output' }],
    } });
    await request(failedPath + '/retry', { id: randomUUID(), revision: tooLarge.revision });
    const failedAgain = await until(failedPath, data => data.status === 'failed' && data.steps.length === 2);
    assert.deepEqual(JSON.parse(failedAgain.steps[1].settings), draft);
    await request(failedPath + '/retry', { id: randomUUID(), revision: tooLarge.revision }, 409);

    await request(retryWorkflow + '/edit', { id: randomUUID(), revision: 2, name: 'Removed node', graph: { entry: 'out', maxSteps: 1, nodes: [{ id: 'out', kind: 'output' }] } });
    answer = 'RECOVERED';
    await request(failedPath + '/retry', { id: randomUUID(), revision: failedAgain.revision });
    const recovered = await until(failedPath, data => data.status === 'approval');
    assert.equal(recovered.version, 1);
    assert.equal(recovered.last, 'RECOVERED');
    assert.deepEqual(recovered.graph, retryGraph);
    assert.deepEqual(JSON.parse(recovered.steps[2].settings), draft);
    assert.deepEqual(recovered.steps.map((step: { status: string }) => step.status), ['failed', 'failed', 'done', 'approval']);
    const current = await request(retryWorkflow + '/run', { id: randomUUID(), revision: 3, input: 'LATEST' });
    assert.equal((await until(`/api/workflow-runs/${current.id}`, data => data.status === 'done')).last, 'LATEST');

  } finally { await app.close(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(dir, { recursive: true, force: true }); }
});

test('provider-free workflows continue beyond one batch and configuration listing is bounded', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-workflow-local-'));
  const options = { database: join(dir, 'state.sqlite') };
  let app = await startApp(options);
  async function request(path: string, body?: unknown) {
    const res = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    assert(res.ok); return res.json();
  }
  try {
    const agent = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Local', model: 'unused', instructions: 'Help.' });
    const list = `/api/agents/${agent.id}/workflows`, ids: string[] = [];
    for (let i = 0; i < 21; i++) {
      const saved = await request(list, { id: randomUUID(), revision: 0, name: `Loop ${i}`, graph: { entry: 'loop', maxSteps: 40, nodes: [
        { id: 'loop', kind: 'condition', operator: 'equals', value: 'repeat', yes: 'loop', no: 'out' }, { id: 'out', kind: 'output' },
      ] } }); ids.push(saved.id);
    }
    const first = await request(list), second = await request(`${list}?after=${first.next_after}`);
    assert.equal(first.workflows.length, 20); assert.equal(second.workflows.length, 1); assert.equal(second.next_after, null);
    assert.deepEqual([...first.workflows, ...second.workflows].map(row => row.id), ids.toSorted());
    const versioned = await request(`/api/workflows/${ids[0]}`);
    for (let revision = 1; revision <= 20; revision++) await request(`/api/workflows/${ids[0]}/edit`, { id: randomUUID(), revision, name: versioned.name, graph: versioned.graph });
    const versions = await request(`/api/workflows/${ids[0]}/versions`);
    const older = await request(`/api/workflows/${ids[0]}/versions?before=${versions.next_before}`);
    assert.equal(versions.versions.length, 20); assert.equal(older.versions.length, 1); assert.equal(older.next_before, null);
    assert.deepEqual([...versions.versions, ...older.versions].map(row => row.revision), Array.from({ length: 21 }, (_, index) => 21 - index));
    const run = await request(`/api/workflows/${ids[0]}/run`, { id: randomUUID(), revision: 21, input: 'repeat' });
    let current;
    for (let i = 0; i < 150; i++) {
      current = await request(`/api/workflow-runs/${run.id}`);
      if (current.status === 'failed') break;
      await new Promise(resolve => setTimeout(resolve, 20));
    }
    assert.equal(current.status, 'failed'); assert.equal(current.steps.length, 40);
    const retry = await fetch(app.origin + `/api/workflow-runs/${run.id}/retry`, {
      method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' },
      body: JSON.stringify({ id: randomUUID(), revision: current.revision }),
    });
    assert.equal(retry.status, 409);
    const schedulePath = `/api/workflows/${ids[1]}/schedule`;
    assert.equal((await request(schedulePath)).schedule, null);
    const scheduleCommand = { id: randomUUID(), revision: 0, nextAt: Date.now() - 1000, intervalSeconds: null, enabled: true, input: 'finish' };
    const savedSchedule = await request(schedulePath, scheduleCommand);
    for (let i = 0; i < 150; i++) {
      current = await request(schedulePath);
      if (current.occurrences.length) break;
      await new Promise(resolve => setTimeout(resolve, 20));
    }
    assert.equal(current.occurrences.length, 1); assert.equal(current.schedule.enabled, 0);
    const scheduledRun = await request(`/api/workflow-runs/${current.occurrences[0].run}`);
    assert.equal(scheduledRun.status, 'done'); assert.equal(scheduledRun.input, 'finish');
    await app.close(); app = await startApp(options);
    assert.deepEqual(await request(schedulePath, scheduleCommand), savedSchedule);
    await new Promise(resolve => setTimeout(resolve, 1100));
    assert.equal((await request(schedulePath)).occurrences.length, 1);
    assert.equal((await request(`/api/workflows/${ids[1]}/runs`)).length, 1);
  } finally { await app.close(); rmSync(dir, { recursive: true, force: true }); }
});


test('workflow webhooks authenticate, deduplicate, ignore busy runs and recover rotation', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-workflow-hook-'));
  const options = { database: join(dir, 'state.sqlite') };
  let app = await startApp(options);
  async function request(path: string, body?: unknown, status = 200) {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const value = await response.json(); assert.equal(response.status, status, JSON.stringify(value)); return value;
  }
  async function deliver(route: string, secret: string, id: string, payload: unknown = { marker: 'external' }, status = 202, browser = false) {
    const body = JSON.stringify({ event: 'build.done', payload }), time = String(Math.floor(Date.now() / 1000));
    const signature = createHmac('sha256', secret).update(`${time}\n${id}\n`).update(body).digest('hex');
    const response = await fetch(`${app.origin}/api/workflow-webhooks/${route}`, { method: 'POST', headers: { 'Content-Type': 'application/json', 'x-minarbot-time': time, 'x-minarbot-id': id, 'x-minarbot-signature': signature, ...(browser ? { Origin: app.origin } : {}) }, body });
    const value = await response.json(); assert.equal(response.status, status, JSON.stringify(value)); return value;
  }
  try {
    const owner = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Hook', model: 'fixture', instructions: 'Test' }, 201);
    const workflow = await request(`/api/agents/${owner.id}/workflows`, { id: randomUUID(), revision: 0, name: 'Hook', graph: { entry: 'review', maxSteps: 3, nodes: [
      { id: 'review', kind: 'approval', prompt: 'Accept?', yes: 'out' }, { id: 'out', kind: 'output' },
    ] } });
    const path = `/api/workflows/${workflow.id}/webhook`;
    assert.equal(await request(path), null);
    const setup = { id: randomUUID(), revision: 0, rotate: true, enabled: true };
    const hook = await request(path, setup);
    assert.equal((await request(path, setup)).secret, hook.secret);
    assert.equal((await request(path)).secret, undefined);
    await deliver(hook.route, 'wrong', 'bad', {}, 401);
    await deliver(hook.route, hook.secret, 'oversize', 'x'.repeat(64000), 413);
    await deliver(hook.route, hook.secret, 'browser', {}, 403, true);
    const event = await deliver(hook.route, hook.secret, 'first');
    assert.equal(event.outcome, 'queued');
    assert.deepEqual(await deliver(hook.route, hook.secret, 'first', undefined, 200), event);
    await deliver(hook.route, hook.secret, 'first', { changed: true }, 409);
    assert.deepEqual(await deliver(hook.route, hook.secret, 'busy'), { outcome: 'ignored', reason: 'busy' });
    let run = await request(`/api/workflow-runs/${event.id}`);
    assert.match(run.input, /not instructions or permissions/); assert.match(run.input, /external/);
    await request(`/api/workflow-runs/${event.id}/cancel`, { id: randomUUID(), revision: run.revision });
    assert.deepEqual(await deliver(hook.route, hook.secret, 'busy', undefined, 200), { outcome: 'ignored', reason: 'busy' });
    const rotation = { id: randomUUID(), revision: 1, rotate: true, enabled: false };
    const rotated = await request(path, rotation);
    await request(path, setup, 409);
    await deliver(hook.route, hook.secret, 'old', {}, 401);
    assert.equal((await deliver(hook.route, rotated.secret, 'disabled')).reason, 'disabled');
    await app.close();
    const db = testDatabase(options.database);
    db.prepare('UPDATE workflow_webhook_receipts SET received=? WHERE delivery=?').run(Date.now() - 8 * 86400000, 'disabled'); db.close();
    app = await startApp(options);
    assert.equal((await request(path, rotation)).secret, rotated.secret);
    const rows = await request(`/api/workflows/${workflow.id}/webhook-events`);
    assert.equal(rows.length, 3);
    assert.equal((await request(`/api/workflows/${workflow.id}/runs`)).length, 1);
    await request(path, { id: randomUUID(), revision: 2, rotate: false, enabled: true });
    const active = await deliver(hook.route, rotated.secret, 'disabled');
    assert.equal(active.outcome, 'queued');
    const removal = { id: randomUUID(), revision: 3, remove: true, route: hook.route };
    assert.deepEqual(await request(path, removal), { removed: true });
    assert.deepEqual(await request(path), { route: null, revision: 4, enabled: 0 });
    await deliver(hook.route, rotated.secret, 'disabled', undefined, 404);
    assert.equal((await request(`/api/workflow-runs/${active.id}`)).id, active.id);
    await app.close(); app = await startApp(options);
    assert.deepEqual(await request(path, removal), { removed: true });
    await request(path, rotation, 409);
    await request(path, { id: randomUUID(), revision: 0, rotate: true, enabled: true }, 409);
    await request(path, { id: randomUUID(), revision: 3, rotate: true, enabled: true }, 409);
    const replacement = await request(path, { id: randomUUID(), revision: 4, rotate: true, enabled: true });
    assert.notEqual(replacement.route, hook.route);
    assert.equal(replacement.revision, 5);
    assert.deepEqual(await request(path, removal), { removed: true });
    assert.equal((await request(path)).route, replacement.route);
    await request(path, { ...removal, id: randomUUID() }, 409);
    await request(path, { id: randomUUID(), revision: 3, rotate: false, enabled: false }, 409);
    await deliver(hook.route, rotated.secret, 'old-after-recreate', undefined, 404);
    assert.equal((await deliver(replacement.route, replacement.secret, 'disabled')).reason, 'busy');
    const events = await request(`/api/workflows/${workflow.id}/webhook-events`);
    assert.equal(events.filter((row: any) => row.delivery === 'disabled').length, 2);
    assert.equal(new Set(events.filter((row: any) => row.delivery === 'disabled').map((row: any) => row.route)).size, 2);
  } finally { await app.close(); rmSync(dir, { recursive: true, force: true }); }
});

test('parallel webhook waits for provider before creating a receipt', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-parallel-hook-'));
  let app = await startApp({ database: join(dir, 'state.sqlite') });
  const provider = createServer((_req, res) => res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: 'ok' } }] })));
  try {
    const request = async (path: string, body: unknown, status = 200) => {
      const response = await fetch(app.origin + path, { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
      const value = await response.json(); assert.equal(response.status, status, JSON.stringify(value)); return value;
    };
    const owner = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Parallel hook', model: 'fixture', instructions: 'Test' }, 201);
    const workflow = await request(`/api/agents/${owner.id}/workflows`, { id: randomUUID(), revision: 0, name: 'Parallel hook', graph: { entry: 'fan', maxSteps: 4, nodes: [
      { id: 'fan', kind: 'parallel', branches: [{ kind: 'agent', agent: owner.id, prompt: 'A' }, { kind: 'model', model: 'fixture', prompt: 'B' }], next: 'out' }, { id: 'out', kind: 'output' },
    ] } });
    const hook = await request(`/api/workflows/${workflow.id}/webhook`, { id: randomUUID(), revision: 0, enabled: true, rotate: true });
    const body = JSON.stringify({ event: 'parallel', payload: {} }), time = String(Math.floor(Date.now() / 1000)), delivery = randomUUID();
    const signature = createHmac('sha256', hook.secret).update(`${time}\n${delivery}\n`).update(body).digest('hex');
    const response = await fetch(`${app.origin}/api/workflow-webhooks/${hook.route}`, { method: 'POST', headers: { 'x-minarbot-time': time, 'x-minarbot-id': delivery, 'x-minarbot-signature': signature, 'Content-Type': 'application/json' }, body });
    assert.equal(response.status, 503); await response.body?.cancel();
    const runs = await fetch(app.origin + `/api/workflows/${workflow.id}/runs`);
    assert.equal(runs.status, 200); assert.deepEqual(await runs.json(), []);
    const events = await fetch(app.origin + `/api/workflows/${workflow.id}/webhook-events`);
    assert.equal(events.status, 200); assert.deepEqual(await events.json(), []);
    await app.close();
    await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
    const address = provider.address(); assert(address && typeof address !== 'string');
    app = await startApp({ database: join(dir, 'state.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1` });
    const retry = await fetch(`${app.origin}/api/workflow-webhooks/${hook.route}`, { method: 'POST', headers: { 'x-minarbot-time': time, 'x-minarbot-id': delivery, 'x-minarbot-signature': signature, 'Content-Type': 'application/json' }, body });
    assert.equal(retry.status, 202);
    const receipt = await retry.json();
    assert.equal(receipt.outcome, 'queued');
    const duplicate = await fetch(`${app.origin}/api/workflow-webhooks/${hook.route}`, { method: 'POST', headers: { 'x-minarbot-time': time, 'x-minarbot-id': delivery, 'x-minarbot-signature': signature, 'Content-Type': 'application/json' }, body });
    assert.equal(duplicate.status, 200); await duplicate.body?.cancel();
    assert.equal((await (await fetch(app.origin + `/api/workflows/${workflow.id}/runs`)).json()).length, 1);
    let run;
    for (let attempt = 0; attempt < 150; attempt++) {
      const response = await fetch(`${app.origin}/api/workflow-runs/${receipt.id}`);
      assert(response.ok);
      run = await response.json();
      if (['done', 'failed'].includes(run.status)) break;
      await new Promise(resolve => setTimeout(resolve, 20));
    }
    assert.equal(run.status, 'done');
    assert.match(run.input, /"event":"parallel"/);
    assert.deepEqual(JSON.parse(run.last), [{ branch: 1, result: 'ok' }, { branch: 2, result: 'ok' }]);
  } finally { await app.close(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(dir, { recursive: true, force: true }); }
});


test('workspace workflow directory pages across owners and keeps per-agent lists scoped', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-workflow-directory-'));
  const options = { database: join(dir, 'state.sqlite') };
  let app = await startApp(options);
  async function request(path: string, body?: unknown, status = 200) {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const data = await response.json(); assert.equal(response.status, status, JSON.stringify(data)); return data;
  }
  try {
    assert.deepEqual(await request('/api/workflows'), { workflows: [], next_after: null });
    const owners = [];
    for (const name of ['Alpha', 'Beta']) owners.push(await request('/api/agents', { runtime: 'api', id: randomUUID(), name, model: 'fixture', instructions: 'Test' }, 201));
    const workflows = [];
    for (let index = 0; index < 21; index++) {
      const agent = owners[index % 2];
      workflows.push(await request(`/api/agents/${agent.id}/workflows`, { id: randomUUID(), revision: 0, name: `Workflow ${index}`, graph: { entry: 'out', maxSteps: 1, nodes: [{ id: 'out', kind: 'output' }], positions: { out: { x: 100 + index, y: 200 + index } } } }));
    }
    const first = await request('/api/workflows'); assert.equal(first.workflows.length, 20); assert(first.next_after);
    const second = await request(`/api/workflows?after=${first.next_after}`); assert.equal(second.workflows.length, 1); assert.equal(second.next_after, null);
    const rows = [...first.workflows, ...second.workflows];
    assert.deepEqual(rows.map(row => row.id), workflows.map(row => row.id).sort());
    for (const row of rows) {
      assert.equal(row.owner_name, owners.find(owner => owner.id === row.agent)?.name);
      assert.deepEqual(row.graph.positions, workflows.find(workflow => workflow.id === row.id)?.graph.positions);
      assert.deepEqual((await request(`/api/workflows/${row.id}`)).graph.positions, row.graph.positions);
    }
    for (const owner of owners) {
      const scoped = await request(`/api/agents/${owner.id}/workflows`);
      assert.deepEqual(scoped.workflows.map((row: any) => row.id), rows.filter(row => row.agent === owner.id).map(row => row.id));
    }
    const workflow = rows[0];
    await request(`/api/workflows/${workflow.id}/edit`, { id: randomUUID(), revision: 1, name: 'Changed from directory', graph: workflow.graph });
    assert.equal((await request('/api/workflows')).workflows[0].revision, 2);
    const run = await request(`/api/workflows/${workflow.id}/run`, { id: randomUUID(), revision: 2, input: '' });
    await request(`/api/workflows/${workflow.id}/edit`, { id: randomUUID(), revision: 2, name: 'Moved', graph: { ...workflow.graph, positions: { out: { x: 800, y: 900 } } } });
    assert.deepEqual((await request(`/api/workflow-runs/${run.id}`)).graph.positions, workflow.graph.positions);
    await app.close(); app = await startApp(options);
    assert.deepEqual((await request(`/api/workflows/${workflow.id}`)).graph.positions, { out: { x: 800, y: 900 } });
    const restored = await request(`/api/workflows/${workflow.id}/restore`, { id: randomUUID(), revision: 3, version: 1 });
    assert.deepEqual(restored.graph.positions, workflow.graph.positions);
    for (const query of ['after=bad', 'before=1', 'owner=anything']) await request(`/api/workflows?${query}`, undefined, 400);
  } finally { await app.close(); rmSync(dir, { recursive: true, force: true }); }
});


test('autosave stops after server canonicalizes condition property order', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-autosave-'));
  const app = await startApp({ database: join(dir, 'state.sqlite') });
  async function post(path: string, body: unknown) {
    const response = await fetch(app.origin + path, { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
    const value = await response.json(); assert(response.ok, JSON.stringify(value)); return value;
  }
  try {
    const agent = await post('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Autosave', model: 'fixture', instructions: 'Test' });
    const graph = { entry: 'check', maxSteps: 2, nodes: [
      { id: 'check', kind: 'condition' as const, operator: 'equals' as const, value: 'yes', yes: 'out', no: 'out' }, { id: 'out', kind: 'output' as const },
    ] };
    const saved = await post(`/api/agents/${agent.id}/workflows`, { id: randomUUID(), revision: 0, name: 'Autosave', graph });
    const local = { ...graph, nodes: [{ id: 'check', kind: 'condition' as const, value: 'yes', yes: 'out', no: 'out', operator: 'contains' as const }, graph.nodes[1]] };
    const draft = workflowAutosaveDraft(saved.name, local, saved, new Set([agent.id])); assert(draft);
    const ack = await post(`/api/workflows/${saved.id}/edit`, { id: randomUUID(), ...draft });
    assert.equal(ack.revision, 2);
    assert.equal(workflowAutosaveDraft(ack.name, local, ack, new Set([agent.id])), null);
  } finally { await app.close(); rmSync(dir, { recursive: true, force: true }); }
});


test('workflow deletion revokes triggers, rejects stale commands and retains terminal results', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-workflow-delete-'));
  const options = { database: join(dir, 'state.sqlite') }; let app = await startApp(options);
  async function request(path: string, body?: unknown, status = 200) {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const value = await response.json(); assert.equal(response.status, status, JSON.stringify(value)); return value;
  }
  try {
    const agent = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Delete', model: 'fixture', instructions: 'Test' }, 201);
    const list = `/api/agents/${agent.id}/workflows`;
    const create = { id: randomUUID(), revision: 0, name: 'Delete me', graph: { entry: 'review', maxSteps: 2, nodes: [
      { id: 'review', kind: 'approval', prompt: 'Accept?', yes: 'out' }, { id: 'out', kind: 'output' },
    ] } };
    const workflow = await request(list, create), path = `/api/workflows/${workflow.id}`;
    const schedule = { id: randomUUID(), revision: 0, enabled: true, nextAt: Date.now() + 3600000, intervalSeconds: 60, input: 'scheduled' };
    await request(`${path}/schedule`, schedule);
    const hookCommand = { id: randomUUID(), revision: 0, rotate: true, enabled: true };
    const hook = await request(`${path}/webhook`, hookCommand);
    const runCommand = { id: randomUUID(), revision: 1, input: 'retained marker' };
    const run = await request(`${path}/run`, runCommand);
    await request(`${path}/delete`, { id: randomUUID(), revision: 1 }, 409);
    const current = await request(`/api/workflow-runs/${run.id}`);
    await request(`/api/workflow-runs/${run.id}/cancel`, { id: randomUUID(), revision: current.revision });
    const deletion = { id: randomUUID(), revision: 1 };
    await request(`${path}/delete`, { ...deletion, revision: 99 }, 409);
    assert.deepEqual(await request(`${path}/delete`, deletion), { id: workflow.id, deleted: 1 });
    assert.equal((await request(path)).deleted, 1);
    assert.equal((await request('/api/workflows')).workflows.length, 0);
    assert.equal((await request(list)).workflows.length, 0);
    assert.equal((await request('/api/workflows?deleted=1')).workflows[0].id, workflow.id);
    assert.equal((await request(`${path}/schedule`)).schedule.enabled, 0);
    for (const [endpoint, command] of [[list, create], [`${path}/run`, runCommand], [`${path}/schedule`, schedule], [`${path}/webhook`, hookCommand],
      [`${path}/edit`, { id: randomUUID(), revision: 1, name: 'Resurrect', graph: create.graph }],
      [`${path}/restore`, { id: randomUUID(), revision: 1, version: 1 }], [`/api/workflow-runs/${run.id}/retry`, { id: randomUUID(), revision: current.revision + 1 }]]) {
      await request(endpoint as string, command, 404);
    }
    const response = await fetch(`${app.origin}/api/workflow-webhooks/${hook.route}`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}' }); assert.equal(response.status, 404);
    await app.close(); app = await startApp(options);
    assert.deepEqual(await request(`${path}/delete`, deletion), { id: workflow.id, deleted: 1 });
    const preserved = await request(`/api/workflow-runs/${run.id}`);
    assert.equal(preserved.input, 'retained marker'); assert.equal(preserved.status, 'cancelled'); assert.deepEqual(preserved.graph, create.graph);
    assert.equal((await request(`${path}/runs`)).length, 1);
    await request(`${path}/delete`, { id: randomUUID(), revision: 1 }, 404);
    await request('/api/workflows?deleted=invalid', undefined, 400);
  } finally { await app.close(); rmSync(dir, { recursive: true, force: true }); }
});

test('workflow output inbox survives restart with stable paging and idempotent read', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-workflow-inbox-'));
  const options = { database: join(dir, 'state.sqlite') };
  let app = await startApp(options);
  async function request(path: string, body?: unknown, status = 200) {
    const res = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const data = await res.json(); assert.equal(res.status, status, JSON.stringify(data)); return data;
  }
  try {
    const agent = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Inbox', model: 'fixture', instructions: 'Test' }, 201);
    const workflow = await request(`/api/agents/${agent.id}/workflows`, { id: randomUUID(), name: 'Output', revision: 0, graph: { entry: 'out', maxSteps: 1, nodes: [{ id: 'out', kind: 'output' }] } });
    for (let index = 0; index < 21; index++) {
      const body = { id: randomUUID(), revision: 1, input: `Result ${index}` };
      const run = await request(`/api/workflows/${workflow.id}/run`, body);
      assert.deepEqual(await request(`/api/workflows/${workflow.id}/run`, body), run);
    }
    let first;
    for (let attempt = 0; attempt < 100; attempt++) {
      first = await request('/api/workflow-notifications');
      if (first.unread === 21) break;
      await new Promise(resolve => setTimeout(resolve, 20));
    }
    assert.equal(first.unread, 21); assert.equal(first.notifications.length, 20);
    const second = await request(`/api/workflow-notifications?before=${first.next_before}`);
    assert.equal(second.notifications.length, 1); assert.equal(second.next_before, null);
    assert.equal(new Set([...first.notifications, ...second.notifications].map(item => item.run)).size, 21);
    const selected = first.notifications[0];
    assert.equal((await request(`/api/workflow-runs/${selected.run}`)).last, 'Result 20');
    const body = { id: randomUUID() };
    await request(`/api/workflow-notifications/${selected.run}/read`, body);
    await request(`/api/workflow-notifications/${selected.run}/read`, body);
    await request('/api/workflow-notifications?before=0', undefined, 400);
    await request('/api/workflow-notifications?before=1e2', undefined, 400);
    await request(`/api/workflow-notifications/${randomUUID()}/read`, body, 404);
    await app.close(); app = await startApp(options);
    const reopened = await request('/api/workflow-notifications');
    assert.equal(reopened.unread, 20); assert.equal(reopened.notifications[0].read, 1);
    assert.deepEqual(reopened.notifications.map((item: any) => item.run), first.notifications.map((item: any) => item.run));
  } finally { await app.close(); rmSync(dir, { recursive: true, force: true }); }
});

test('workflow channel outputs deliver once and recheck channel availability at completion', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-workflow-channel-output-'));
  const options = { database: join(dir, 'state.sqlite') };
  let app = await startApp(options);
  async function request(path: string, body?: unknown, status = 200) {
    const res = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const data = await res.json(); assert.equal(res.status, status, JSON.stringify(data)); return data;
  }
  async function settled(id: string, status: string) {
    for (let attempt = 0; attempt < 100; attempt++) {
      const run = await request(`/api/workflow-runs/${id}`);
      if (run.status === status) return run;
      await new Promise(resolve => setTimeout(resolve, 20));
    }
    assert.fail(`Run did not reach ${status}`);
  }
  try {
    const owner = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Owner', model: 'unused', instructions: 'Test' }, 201);
    const other = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Other', model: 'unused', instructions: 'Test' }, 201);
    let room = await request('/api/channels', { id: randomUUID(), name: 'Results', instructions: 'Results', members: [owner.id, other.id], lead: owner.id }, 201);
    const graph = { entry: 'review', maxSteps: 2, nodes: [{ id: 'review', kind: 'approval', prompt: 'Publish?', yes: 'out' }, { id: 'out', kind: 'output', channel: room.id }] };
    const workflow = await request(`/api/agents/${owner.id}/workflows`, { id: randomUUID(), revision: 0, name: 'Publish', graph });
    await request(`/api/agents/${other.id}/workflows`, { id: randomUUID(), revision: 0, name: 'Invalid', graph: { ...graph, nodes: [graph.nodes[0], { ...graph.nodes[1], channel: randomUUID() }] } }, 409);
    await request(`/api/agents/${owner.id}/workflows`, { id: randomUUID(), revision: 0, name: 'Invalid', graph: { ...graph, nodes: [graph.nodes[0], { ...graph.nodes[1], channel: '' }] } }, 400);
    const launch = { id: randomUUID(), revision: 1, input: '<b>Exact result</b>' };
    const run = await request(`/api/workflows/${workflow.id}/run`, launch);
    const approval = await settled(run.id, 'approval');
    const accept = { id: randomUUID(), revision: approval.revision, accepted: true };
    const db = testDatabase(options.database);
    try {
      db.exec("CREATE TRIGGER fail_notice_fixture BEFORE INSERT ON workflow_notifications BEGIN SELECT RAISE(ABORT,'injected notification failure'); END");
      await request(`/api/workflow-runs/${run.id}/approve`, accept);
      const rolledBack = await request(`/api/workflow-runs/${run.id}`);
      assert.equal(rolledBack.status, 'ready');
      assert.equal(rolledBack.steps.length, 1);
      assert.deepEqual((await request('/api/workflow-notifications')).notifications, []);
      assert.deepEqual(await request(`/api/channels/${room.id}/turns`), []);
    } finally {
      db.exec('DROP TRIGGER IF EXISTS fail_notice_fixture');
      db.close();
    }
    await settled(run.id, 'done');
    await request(`/api/workflow-runs/${run.id}/approve`, accept);
    await request(`/api/workflows/${workflow.id}/run`, launch);
    const messages = await request(`/api/channels/${room.id}/turns`);
    assert.equal(messages.length, 1); assert.equal(messages[0].reply, launch.input);
    assert.equal(messages[0].workflow_run, run.id); assert.equal(messages[0].agent, owner.id);
    assert.equal((await request(`/api/agents/${owner.id}/turns`)).length, 0);
    assert.equal((await request('/api/channels'))[0].activity, 1);
    await app.close(); app = await startApp(options);
    assert.deepEqual(await request(`/api/channels/${room.id}/turns`), messages);
    for (const mode of ['archive', 'remove']) {
      const next = await request(`/api/workflows/${workflow.id}/run`, { id: randomUUID(), revision: 1, input: mode });
      const pending = await settled(next.id, 'approval');
      room = await request(`/api/channels/${room.id}/edit`, { id: randomUUID(), revision: room.revision, name: room.name, instructions: room.instructions, archived: mode === 'archive', members: mode === 'remove' ? [other.id] : [owner.id, other.id], lead: other.id });
      await request(`/api/workflow-runs/${next.id}/approve`, { id: randomUUID(), revision: pending.revision, accepted: true });
      const failed = await settled(next.id, 'failed');
      assert.equal(failed.last, mode); assert.equal(failed.steps.at(-1).status, 'failed');
      assert.match(failed.error, /Output channel/);
      assert.equal((await request(`/api/channels/${room.id}/turns`)).length, 1);
      await request(`/api/workflows/${workflow.id}/edit`, { id: randomUUID(), revision: 1, name: 'Publish', graph }, 409);
      room = await request(`/api/channels/${room.id}/edit`, { id: randomUUID(), revision: room.revision, name: room.name, instructions: room.instructions, archived: false, members: [owner.id, other.id], lead: owner.id });
    }
    assert.equal((await request('/api/workflow-notifications')).notifications.length, 1);
    const snapshotRun = await request(`/api/workflows/${workflow.id}/run`, { id: randomUUID(), revision: 1, input: 'Snapshot output' });
    const snapshotApproval = await settled(snapshotRun.id, 'approval');
    await request(`/api/workflows/${workflow.id}/edit`, { id: randomUUID(), revision: 1, name: 'Inbox only', graph: { ...graph, nodes: [graph.nodes[0], { id: 'out', kind: 'output' }] } });
    await request(`/api/workflow-runs/${snapshotRun.id}/approve`, { id: randomUUID(), revision: snapshotApproval.revision, accepted: true });
    await settled(snapshotRun.id, 'done');
    const snapshotMessages = await request(`/api/channels/${room.id}/turns`);
    assert.equal(snapshotMessages.length, 2); assert.equal(snapshotMessages[1].reply, 'Snapshot output');
    assert.equal((await request(`/api/workflow-runs/${snapshotRun.id}`)).version, 1);
  } finally { await app.close(); rmSync(dir, { recursive: true, force: true }); }
});

test('workflow run pages retain older approvals after restart and validate cursors', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-run-pages-'));
  const options = { database: join(dir, 'state.sqlite') };
  let app = await startApp(options);
  async function request(path: string, body?: unknown, expected = 200) {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const data = await response.json(); assert.equal(response.status, expected, JSON.stringify(data)); return data;
  }
  try {
    const agent = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Approvals', model: 'fixture', instructions: 'Help.' }, 201);
    const workflow = await request(`/api/agents/${agent.id}/workflows`, { id: randomUUID(), revision: 0, name: 'Review', graph: { entry: 'review', maxSteps: 2, nodes: [{ id: 'review', kind: 'approval', prompt: 'Accept?', yes: 'out', no: 'out' }, { id: 'out', kind: 'output' }] } });
    const ids: string[] = [];
    for (let i = 0; i < 22; i++) ids.push((await request(`/api/workflows/${workflow.id}/run`, { id: randomUUID(), revision: 1, input: String(i) })).id);
    await app.close(); app = await startApp(options);
    const path = `/api/workflows/${workflow.id}/runs`, latest = await request(path);
    assert.equal(latest.length, 20); assert.deepEqual(latest.map((row: any) => row.id), ids.slice(2).reverse());
    const older = await request(`${path}?before=${latest.at(-1).seq}`);
    assert.deepEqual(older.map((row: any) => row.id), ids.slice(0, 2).reverse());
    assert.equal((await request(`/api/workflow-runs/${older.at(-1).id}`)).status, 'approval');
    assert.equal((await request(`${path}?before=${older.at(-1).seq}`)).length, 0);
    for (const query of ['before=0', 'before=-1', 'before=no', 'before=1.5', 'before=9007199254740992', 'before=', 'before=1&before=2', 'other=1']) await request(`${path}?${query}`, undefined, 400);
  } finally { await app.close(); rmSync(dir, { recursive: true, force: true }); }
});
