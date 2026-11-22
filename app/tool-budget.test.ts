import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { createServer } from 'node:http';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { startApp } from './test-bridge.ts';

test('nine distinct tools complete before per-task budget requests a final answer without tools', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-tool-budget-'));
  const requests: any[] = [];
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const body = JSON.parse(raw); requests.push(body);
    const count = body.messages.filter((row: any) => row.role === 'tool').length;
    res.end(JSON.stringify({ choices: [{ finish_reason: body.tools?.length ? 'tool_calls' : 'stop', message: body.tools?.length
      ? { tool_calls: [{ id: `read-${count}`, type: 'function', function: { name: 'read_history', arguments: '{}' } }] }
      : { content: 'Checked history. More work remains. Reply "continue".' } }] }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const app = await startApp({ database: join(dir, 'state.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1` }, { directory: dir, servers: [provider] });
  const request = async (path: string, body?: unknown) => {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const data = await response.json(); assert(response.ok, JSON.stringify(data)); return data;
  };
  try {
    const agent = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Budget', model: 'fixture', instructions: 'Read.', toolBudget: 10 });
    const path = `/api/agents/${agent.id}/turns`, id = randomUUID();
    await request(path, { id, text: 'Perform more than nine steps.' });
    let turn: any;
    for (let n = 0; n < 400; n++) { turn = (await request(path)).find((row: any) => row.id === id); if (['done', 'failed'].includes(turn.status)) break; await delay(10); }
    assert.equal(turn.status, 'done', turn.error);
    assert.equal(requests.length, 11);
    assert.equal(requests.at(-1).tools, undefined);
    assert.equal(turn.activity.budget_stopped, 10);
    assert.match(turn.reply, /continue/);
    assert.match(JSON.stringify(requests.at(-1).messages), /what.*left|remains/i);
  } finally { await app.close(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(dir, { recursive: true, force: true }); }
});

import { EventEmitter } from 'node:events';
import type { ChildProcess } from 'node:child_process';
import sharp from 'sharp';
import { parseAgentTemplate, templateCommand } from './shared/agent-template.ts';
import { restoreAgentCreation } from './shared/agent-create.ts';

type ProviderReply = string | { id: string; name: string; args?: object } | number;
async function fixture(answer: (body: any) => ProviderReply) {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-budget-')), requests: any[] = [];
  let captures = 0;
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const body = JSON.parse(raw); requests.push(body);
    const value = answer(body);
    if (typeof value === 'number') { res.writeHead(value); res.end('{}'); return; }
    res.end(JSON.stringify({ choices: [{ finish_reason: typeof value === 'string' ? 'stop' : 'tool_calls', message: typeof value === 'string' ? { content: value } : { tool_calls: [{ id: value.id, type: 'function', function: { name: value.name, arguments: JSON.stringify(value.args ?? {}) } }] } }] }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const options = { database: join(dir, 'state.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1`, desktop: { dataDir: dir, leaseDir: join(dir, 'leases'), enabled: true, runtime: {
    launch() { const child = Object.assign(new EventEmitter(), { exitCode: null, stdin: { end() { queueMicrotask(() => { Object.defineProperty(child, 'exitCode', { value: 0 }); child.emit('exit', 0); }); } } }) as unknown as ChildProcess; return { child, ready: Promise.resolve() }; },
    async run() { return Buffer.alloc(0); }, async capture() { captures++; return sharp({ create: { width: 1280, height: 800, channels: 3, background: { r: captures * 47 % 256, g: captures * 83 % 256, b: captures * 131 % 256 } } }).png().toBuffer(); },
  } } };
  let app = await startApp(options, { directory: dir, servers: [provider] });
  const request = async (path: string, body?: unknown, status?: number) => {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const data = await response.json(); if (status) assert.equal(response.status, status, JSON.stringify(data)); else assert(response.ok, JSON.stringify(data)); return data;
  };
  const until = async (read: () => Promise<any>, ready: (row: any) => boolean) => { for (let n = 0; n < 600; n++) { const row = await read(); if (ready(row)) return row; await delay(10); } assert.fail('Budget task did not settle'); };
  const turn = (path: string, id: string) => until(async () => (await request(path)).find((row: any) => row.id === id), row => ['done','failed'].includes(row?.status));
  const agent = await request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Budget', model: 'fixture', instructions: 'Help.', toolBudget: 10 });
  return { agent, options, requests, request, until, turn, captures: () => captures, async restart() { await app.close(); app = await startApp(options); }, async close() { await app.close(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(dir, { recursive: true, force: true }); } };
}
const reads = (body: any) => body.messages.filter((row: any) => row.role === 'tool');
const terminal = (row: any) => ['done','failed'].includes(row.status);

test('budget setting rejects malformed values, survives restart and template recovery', async () => {
  const f = await fixture(() => 'Done.');
  try {
    const profile = { name: 'Budget', model: 'fixture', instructions: 'Help.' };
    for (const toolBudget of [null, true, '90', 9, 201, 10.5]) {
      await f.request('/api/agents', { runtime: 'api', id: randomUUID(), ...profile, toolBudget }, 400);
      await f.request(`/api/agents/${f.agent.id}/edit`, { id: randomUUID(), revision: 1, ...profile, toolBudget }, 400);
      assert.throws(() => parseAgentTemplate({ format: 'minarbot-agent', version: 1, ...profile, threadLimit: 2, toolBudget }));
    }
    const create = { id: randomUUID(), ...profile, toolBudget: 200 };
    const agent = await f.request('/api/agents', { ...create, runtime: 'api' }); assert.equal(agent.tool_budget, 200);
    assert.deepEqual(await f.request('/api/agents', { ...create, runtime: 'api' }), agent);
    await f.request('/api/agents', { runtime: 'api', ...create, toolBudget: 199 }, 409);
    const edit = { id: randomUUID(), revision: 1, ...profile, toolBudget: 15 };
    const changed = await f.request(`/api/agents/${agent.id}/edit`, edit); assert.equal(changed.tool_budget, 15);
    await f.request(`/api/agents/${agent.id}/edit`, { ...edit, id: randomUUID() }, 409);
    const exported = await f.request(`/api/agents/${agent.id}/template`); assert.equal(exported.toolBudget, 15);
    const imported = await f.request('/api/agents', { ...templateCommand(exported, randomUUID()), runtime: 'api' }); assert.equal(imported.tool_budget, 15);
    const recovery = { id: randomUUID(), ...profile, role: '', backupModel: '', threadLimit: 2, providerAccount: '', toolBudget: 35 };
    assert.equal(restoreAgentCreation(JSON.stringify(recovery))!.toolBudget, 35);
    await f.restart();
    assert.deepEqual(await f.request(`/api/agents/${agent.id}/edit`, edit), changed);
    assert.equal((await f.request('/api/agents')).agents.find((row: any) => row.id === agent.id).tool_budget, 15);
  } finally { await f.close(); }
});

test('resume gets fresh budget without replay; desktop images retain latest three across restart', async () => {
  let summaries = 0;
  const f = await fixture(body => !body.tools?.length ? ++summaries === 1 ? 503 : 'Captured more. Reply "continue".' : { id: `shot-${reads(body).length}`, name: 'computer_screenshot' });
  try {
    await f.request(`/api/agents/${f.agent.id}/computer`, { id: randomUUID(), revision: 0, enabled: true });
    const path = `/api/agents/${f.agent.id}/turns`, id = randomUUID();
    await f.request(path, { id, text: 'Inspect desktop.' });
    const failed = await f.turn(path,id); assert.equal(failed.status,'failed'); assert.equal(failed.error,'Stopped after 10 tool steps. Resume to continue.'); assert.equal(f.captures(),10);
    for (const body of f.requests) {
      const images = body.messages.flatMap((row: any) => Array.isArray(row.content) ? row.content.filter((part: any) => part.type === 'image_url') : []);
      assert.equal(images.length,Math.min(3,reads(body).length));
      if (reads(body).length > 3) assert.match(JSON.stringify(body.messages), /Earlier agent desktop screenshot omitted/);
    }
    assert.equal(f.requests.at(-1).tools,undefined);
    await f.restart();
    const before = f.requests.length;
    const resume = { id: randomUUID(), revision: failed.revision, acknowledgeUncertain: true };
    await f.request(`${path}/${id}/resume`,resume);
    await f.request(`${path}/${id}/resume`,resume);
    const done = await f.turn(path,id); assert.equal(done.status,'done',done.error); assert.equal(f.captures(),20);
    assert.equal(f.requests.length-before,11); assert.equal(reads(f.requests[before]).length,10);
    assert.equal(f.requests[before].messages.flatMap((row: any) => Array.isArray(row.content) ? row.content.filter((part: any) => part.type === 'image_url') : []).length,3);
    assert.equal(done.activity.tools.length,20); assert.equal(new Set(done.activity.tools.map((row: any) => row.id)).size,20);
    assert.equal(done.activity.budget_stopped,10); assert.equal(done.activity.calls.filter((row: any) => row.phase==='budget_summary').length,2);
    const captured: string[] = [];
    for (const body of f.requests) {
      const images = body.messages.flatMap((row: any) => Array.isArray(row.content) ? row.content.filter((part: any) => part.type === 'image_url').map((part: any) => part.image_url.url) : []);
      if (reads(body).length > captured.length) captured.push(images.at(-1));
      assert.deepEqual(images, captured.slice(-3), 'Request retains newest screenshots in capture order');
    }
    assert.equal(new Set(captured).size, 20);
  } finally { await f.close(); }
});

test('legacy channel transfers share attempt budget beyond eight coordination steps', async () => {
  const f = await fixture(body => !body.tools?.length ? 'Transferred work. Reply "continue".' : { id: `transfer-${reads(body).length}`, name: 'transfer_task', args: { recipient: body.tools.find((tool: any) => tool.function.name === 'transfer_task').function.parameters.properties.recipient.enum[0], task: 'Continue work.' } });
  try {
    const next = await f.request('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Next', model: 'fixture', instructions: 'Help.', toolBudget: 10 });
    const room = await f.request('/api/channels', { id: randomUUID(), name: 'Transfers', instructions: 'Help.', lead: f.agent.id, members: [f.agent.id, next.id] });
    const path = `/api/channels/${room.id}/turns`, id = randomUUID();
    await f.request(path, { id, text: 'Transfer work.', recipient: f.agent.id });
    const done = await f.turn(path,id);
    assert.equal(done.status, 'done', done.error); assert.equal(done.coordination.length, 10);
    assert.equal(done.activity.budget_stopped, 10); assert.equal(f.requests.length, 11); assert.equal(f.requests.at(-1).tools, undefined);
  } finally { await f.close(); }
});

for (const persisted of [false,true]) test(`duplicate provider IDs have distinct protocol message (${persisted ? 'saved call' : 'current attempt'})`, async () => {
  const f = await fixture(() => ({ id: 'same-id', name: persisted ? 'computer_screenshot' : 'read_history' }));
  try {
    if (persisted) await f.request(`/api/agents/${f.agent.id}/computer`, { id: randomUUID(), revision: 0, enabled: true });
    const path = `/api/agents/${f.agent.id}/turns`, id = randomUUID(); await f.request(path,{id,text:'Read.'});
    const failed = await f.turn(path,id); assert.equal(failed.error,'Provider reused a tool call ID. Resume to retry.'); assert.equal(f.requests.length,2);
    if (persisted) { await f.restart(); await f.request(`${path}/${id}/resume`,{id:randomUUID(),revision:failed.revision,acknowledgeUncertain:true}); assert.equal((await f.turn(path,id)).error,failed.error); assert.equal(f.captures(),1); }
  } finally { await f.close(); }
});

for (const kind of ['workflow','routine','routine flow'] as const) test(`${kind} uses shared per-task budget and final summary`, async () => {
  const f = await fixture(body => body.tools?.length ? { id:`read-${reads(body).length}`,name:'read_history' } : 'Checked. More remains. Reply "continue".');
  try {
    let path: string, id: string;
    if (kind==='workflow') {
      const graph={entry:'work',maxSteps:3,nodes:[{id:'work',kind:'agent',agent:f.agent.id,prompt:'Read.',next:'out'},{id:'out',kind:'output'}]};
      const workflow=await f.request(`/api/agents/${f.agent.id}/workflows`,{id:randomUUID(),revision:0,name:'Budget',graph});
      const run=await f.request(`/api/workflows/${workflow.id}/run`,{id:randomUUID(),revision:1,input:'Read.'});
      const result=await f.until(()=>f.request(`/api/workflow-runs/${run.id}`),terminal);assert.equal(result.status,'done');
      const step=result.steps.find((step:any)=>step.turn);path=`/api/threads/${step.thread}/turns`;id=step.turn;
    } else {
      const routine=await f.request(`/api/agents/${f.agent.id}/routines`,{id:randomUUID(),name:'Budget',prompt:'Read.',nextAt:Date.now()+3600000,intervalSeconds:60,enabled:false});
      if(kind==='routine flow')await f.request(`/api/routines/${routine.id}/flow`,{id:randomUUID(),revision:0,links:[]});
      const run=await f.request(`/api/routines/${routine.id}/test`,{id:randomUUID(),revision:routine.revision});path=`/api/threads/${run.thread}/turns`;id=run.turn;
      const result=await f.until(()=>f.request(`/api/routines/${routine.id}/runs`).then(rows=>rows[0]),terminal);assert.equal(result.status,'done');
    }
    const row=await f.turn(path,id);assert.equal(row.status,'done',row.error);assert.equal(row.activity.budget_stopped,10);assert.equal(f.requests.length,11);assert.equal(f.requests.at(-1).tools,undefined);
  } finally { await f.close(); }
});

for (const summary of ['', { id:'forbidden-summary-tool',name:'read_history' }]) test(`invalid final summary fails clearly and never executes tools (${typeof summary})`, async () => {
  const f=await fixture(body=>body.tools?.length?{id:`read-${reads(body).length}`,name:'read_history'}:summary);
  try { const path=`/api/agents/${f.agent.id}/turns`,id=randomUUID();await f.request(path,{id,text:'Read.'});const row=await f.turn(path,id);assert.equal(row.status,'failed');assert.equal(row.error,'Stopped after 10 tool steps. Resume to continue.');assert.equal(f.requests.length,11);assert.equal(f.requests.at(-1).tools,undefined);assert.equal(row.activity.budget_stopped,10); }
  finally { await f.close(); }
});

test('reassign gets fresh budget while retaining recorded computer calls', async () => {
  let summaries=0;
  const f=await fixture(body=>!body.tools?.length?++summaries===1?503:'Checked. Reply "continue".':{id:`shot-${reads(body).length}`,name:'computer_screenshot'});
  try {
    const next=await f.request('/api/agents',{runtime:'api', id:randomUUID(),name:'Next',model:'fixture',instructions:'Help.',toolBudget:10});
    for(const agent of [f.agent,next])await f.request(`/api/agents/${agent.id}/computer`,{id:randomUUID(),revision:0,enabled:true});
    const room=await f.request('/api/channels',{id:randomUUID(),name:'Budget room',instructions:'Help.',lead:f.agent.id,members:[f.agent.id,next.id]});
    const path=`/api/channels/${room.id}/turns`,id=randomUUID();await f.request(path,{id,text:'Inspect.',recipient:f.agent.id});
    const failed=await f.turn(path,id);assert.equal(failed.error,'Stopped after 10 tool steps. Resume to continue.');assert.equal(f.captures(),10);
    const command={id:randomUUID(),revision:failed.revision,recipient:next.id,acknowledgeUncertain:true};await f.request(`${path}/${id}/reassign`,command);await f.request(`${path}/${id}/reassign`,command);
    const done=await f.turn(path,id);assert.equal(done.status,'done',done.error);assert.equal(done.agent,next.id);assert.equal(f.captures(),20);assert.equal(done.activity.tools.length,20);
  } finally { await f.close(); }
});

test('automatic question continuation retains attempt budget across restart', async () => {
  let calls=0;
  const f=await fixture(body=>!body.tools?.length?'Progress saved. Reply "continue".':++calls===10?{id:'ask',name:'ask_user',args:{question:'Ready?',choices:[]}}:{id:`read-${calls}`,name:'read_history'});
  try {
    const path=`/api/agents/${f.agent.id}/turns`,id=randomUUID();await f.request(path,{id,text:'Read then ask.'});
    const waiting=await f.until(()=>f.request(path).then(rows=>rows.find((row:any)=>row.id===id)),row=>row.questions?.length===1);
    assert.equal(calls,10);await f.restart();
    await f.request(`${path}/${id}/questions/${waiting.questions[0].id}/answer`,{id:randomUUID(),text:'Ready.'});
    const done=await f.turn(path,id);assert.equal(done.status,'done',done.error);assert.equal(done.activity.budget_stopped,10);assert.equal(f.requests.length,11);assert.equal(f.requests.at(-1).tools,undefined);
  } finally { await f.close(); }
});
