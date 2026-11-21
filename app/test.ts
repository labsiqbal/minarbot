import { test } from 'node:test';
import { testDatabase } from './test-support/database.ts';
import assert from 'node:assert/strict';
import { createServer, get, request as httpRequest } from 'node:http';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { startApp } from './test-bridge.ts';

test('HTTP: persistence, idempotent FIFO turns, isolation, failure and interrupted recovery', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-check-'));
  const calls: string[] = [];
  const profilesSeen: { text: string; model: string; instructions: string }[] = [];
  let releaseRouting: (() => void) | undefined;
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const data = JSON.parse(raw); const text = data.messages.at(-1).content;
    calls.push(text);
    profilesSeen.push({ text, model: data.model, instructions: data.messages[0].content });
    if (data.messages[0].content.startsWith('Choose exactly one channel member')) {
      const input = JSON.parse(text);
      if (input.request === 'route-hang') return;
      if (input.request === 'route-busy-lead') await new Promise<void>(resolve => { releaseRouting = resolve; });
      if (input.request === 'route-cancel') await new Promise(resolve => setTimeout(resolve, 200));
      if (input.request === 'route-malformed') { res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: 'not JSON' } }] })); return; }
      const decision = input.request === 'route-invalid' ? { action: 'assign', recipient: randomUUID() }
        : input.request === 'route-clarify' ? { action: 'clarify', message: 'Which topic?' }
        : input.request === 'route-none' ? { action: 'none', message: 'No work requested.' }
        : { action: 'assign', recipient: input.members.find((member: { name: string }) => member.name === 'Agent 0').id };
      res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: JSON.stringify(decision) } }] }));
      return;
    }
    if (text === 'hang') return;
    await new Promise(resolve => setTimeout(resolve, 30));
    res.setHeader('Content-Type', 'application/json');
    if (text === 'fail') { res.writeHead(429); res.end('{"error":"private upstream detail"}'); return; }
    res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: `reply:${text};history:${data.messages.length}` } }] }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const options = { database: join(dir, 'state.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1` };
  let app = await startApp(options, { directory: dir, servers: [provider] });
  const request = async (path: string, body?: unknown, origin = app.origin) => {
    const response = await fetch(app.origin + path, body ? { method: 'POST', headers: { Origin: origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body) } : undefined);
    return { status: response.status, body: await response.json() };
  };
  async function until(path: string, status: string) {
    for (let i = 0; i < 100; i++) {
      const response = await request(path);
      if (response.body.at(-1)?.status === status) return response.body;
      await new Promise(resolve => setTimeout(resolve, 20));
    }
    assert.fail(`Timed out waiting for ${status}`);
  }
  try {
    const profile = { runtime: 'api', id: randomUUID(), name: 'Research', model: 'test-model', instructions: 'Answer precisely.' };
    assert.equal((await request('/api/agents', { ...profile, runtime: 'api' }, 'https://foreign.example')).status, 403);
    assert.equal((await request('/api/agents', { ...profile, name: '' })).status, 400);
    assert.equal((await request('/api/agents', { ...profile, runtime: 'api' })).status, 201);
    assert.equal((await request('/api/agents', { ...profile, runtime: 'api' })).status, 200);
    assert.equal((await request('/api/agents', { ...profile, name: 'changed' })).status, 409);
    for (let i = 0; i < 4; i++) assert.equal((await request('/api/agents', { ...profile, id: randomUUID(), name: `Agent ${i}` })).status, 201);
    assert.equal((await request('/api/agents')).body.agents.length, 5);
    const path = `/api/agents/${profile.id}/turns`;
    const first = { id: randomUUID(), text: 'first' };
    assert.equal((await request(path, first)).status, 202);
    assert.equal((await request(path, first)).status, 202);
    assert.equal((await request(path, { ...first, text: 'different' })).status, 409);
    const second = { id: randomUUID(), text: 'second' };
    assert.equal((await request(path, second)).status, 202);
    const turns = await until(path, 'done');
    assert.equal(turns.length, 2);
    assert.equal(turns[0].reply, 'reply:first;history:2');
    assert.equal(turns[1].reply, 'reply:second;history:4');
    assert.deepEqual(calls, ['first', 'second']);
    const other = (await request('/api/agents')).body.agents[1];
    assert.deepEqual((await request(`/api/agents/${other.id}/turns`)).body, []);
    assert.equal((await request(`/api/agents/${other.id}/turns`, first)).status, 409);
    await request(path, { id: randomUUID(), text: 'fail' });
    const failed = (await until(path, 'failed')).at(-1);
    assert.match(failed.error, /HTTP 429/); assert(!failed.error.includes('private'));
    await request(path, { id: randomUUID(), text: 'hang' });
    await until(path, 'running');
    assert.equal((await request(`/api/agents/${profile.id}/edit`, { ...profile, id: randomUUID(), revision: 1 })).status, 409);
    await app.close();
    app = await startApp(options);
    const recovered = (await request(path)).body;
    assert.equal(recovered.at(-1).status, 'interrupted');
    assert.equal(recovered[0].reply, 'reply:first;history:2');
    assert.equal((await request('/api/agents')).body.agents.length, 5);
    await new Promise(resolve => setTimeout(resolve, 80));
    assert.equal(calls.filter(text => text === 'hang').length, 1);
    assert.equal((await request(path, first)).status, 202);
    assert.equal((await request(path)).body.length, 4);
    const forbiddenHost = await new Promise<number | undefined>((resolve, reject) => get(app.origin + '/api/agents', { headers: { Host: 'attacker.example' } }, res => { res.resume(); resolve(res.statusCode); }).on('error', reject));
    assert.equal(forbiddenHost, 403);
    const staticPage = await fetch(app.origin); assert.equal(staticPage.status, 200);
    assert.match(staticPage.headers.get('content-security-policy')!, /frame-ancestors 'none'/);
    const channel = { id: randomUUID(), name: 'Research room', instructions: 'Compare findings.', members: [profile.id, other.id], lead: profile.id };
    assert.equal((await request('/api/channels', { ...channel, lead: randomUUID() })).status, 400);
    assert.equal((await request('/api/channels', channel)).status, 201);
    assert.equal((await request('/api/channels', { ...channel, members: [...channel.members].reverse() })).status, 200);
    assert.equal((await request('/api/channels', { ...channel, name: 'changed' })).status, 409);
    const channelPath = `/api/channels/${channel.id}/turns`;
    const channelMessage = { id: randomUUID(), text: 'channel-first', recipient: profile.id };
    assert.equal((await request(channelPath, channelMessage)).status, 202);
    assert.equal((await request(channelPath, channelMessage)).status, 202);
    const channelTurns = await until(channelPath, 'done');
    assert.equal(channelTurns.length, 1);
    assert.equal(channelTurns[0].agent, profile.id);
    assert.equal(channelTurns[0].reply, 'reply:channel-first;history:2');
    assert.equal((await request(path)).body.length, 4);
    assert.equal((await request(channelPath, { ...channelMessage, recipient: other.id })).status, 409);
    assert.equal((await request(channelPath, { id: randomUUID(), text: 'reject', recipient: randomUUID() })).status, 400);
    await request(channelPath, { id: randomUUID(), text: 'channel-second', recipient: other.id });
    const channelSecond = (await until(channelPath, 'done')).at(-1);
    assert.equal(channelSecond.agent, other.id);
    assert.equal(channelSecond.reply, 'reply:channel-second;history:4');
    assert.deepEqual((await request(`/api/agents/${other.id}/turns`)).body, []);
    const noLead = { ...channel, id: randomUUID(), lead: null };
    await request('/api/channels', noLead);
    assert.equal((await request(`/api/channels/${noLead.id}/turns`, { id: randomUUID(), text: 'ambiguous', automatic: true })).status, 409);
    assert.deepEqual((await request(`/api/channels/${noLead.id}/turns`)).body, []);
    const single = { ...channel, id: randomUUID(), lead: null, members: [other.id] };
    await request('/api/channels', single);
    const singlePath = `/api/channels/${single.id}/turns`;
    await request(singlePath, { id: randomUUID(), text: 'single' });
    assert.equal((await until(singlePath, 'done'))[0].agent, other.id);
    const hanging = { id: randomUUID(), text: 'hang', recipient: profile.id }, queued = { id: randomUUID(), text: 'never-sent', recipient: profile.id };
    await request(channelPath, hanging);
    await until(channelPath, 'running');
    await request(channelPath, queued);
    assert.equal((await request(`${path}/${hanging.id}/cancel`, { id: randomUUID() })).status, 404);
    assert.equal((await request(`${channelPath}/${queued.id}/cancel`, { id: randomUUID() })).body.status, 'cancelled');
    assert.equal((await request(`${channelPath}/${hanging.id}/cancel`, { id: randomUUID() })).body.status, 'cancelled');
    assert.equal((await request(`${channelPath}/${hanging.id}/cancel`, { id: randomUUID() })).body.status, 'cancelled');
    assert.equal((await request(`${channelPath}/${channelMessage.id}/cancel`, { id: randomUUID() })).status, 409);
    await request(channelPath, { id: randomUUID(), text: 'after-cancel', recipient: profile.id });
    assert.equal((await until(channelPath, 'done')).at(-1).reply, 'reply:after-cancel;history:6');
    assert(!calls.includes('never-sent'));
    const channelEditPath = `/api/channels/${channel.id}/edit`;
    const editedChannel = { ...channel, id: randomUUID(), revision: 1, archived: false, name: 'Renamed room' };
    const changedChannel = await request(channelEditPath, editedChannel);
    assert.equal(changedChannel.status, 200); assert.equal(changedChannel.body.revision, 2);
    assert.deepEqual((await request(channelEditPath, editedChannel)).body, changedChannel.body);
    assert.equal((await request(channelEditPath, { ...editedChannel, id: randomUUID() })).status, 409);
    assert.equal((await request('/api/channels', channel)).body.name, 'Renamed room');
    const blockedTurn = { id: randomUUID(), text: 'hang', recipient: profile.id };
    await request(channelPath, blockedTurn); await until(channelPath, 'running');
    const heldTurn = { id: randomUUID(), text: 'held-task', recipient: profile.id };
    await request(channelPath, heldTurn);
    const pause = { id: randomUUID(), revision: 1 };
    const heldPath = `${channelPath}/${heldTurn.id}`;
    const paused = await request(`${heldPath}/pause`, pause);
    assert.equal(paused.body.paused, 1); assert.equal(paused.body.revision, 2);
    assert.deepEqual((await request(`${heldPath}/pause`, pause)).body, paused.body);
    assert.equal((await request(`${heldPath}/resume`, { id: randomUUID(), revision: 1 })).status, 409);
    await app.close(); app = await startApp(options);
    assert(!calls.includes('held-task'));
    assert.equal((await request(`${heldPath}/reassign`, { id: randomUUID(), revision: 2, recipient: other.id })).status, 200);
    assert.equal((await until(channelPath, 'done')).at(-1).agent, other.id);
    assert.equal(calls.filter(text => text === 'held-task').length, 1);
    assert.deepEqual((await request(`${heldPath}/pause`, pause)).body, paused.body);
    assert.equal((await request(channelPath)).body.find((turn: { id: string }) => turn.id === heldTurn.id).status, 'done');
    const interruptedPath = `${channelPath}/${blockedTurn.id}`;
    assert.equal((await request(`${interruptedPath}/resume`, { id: randomUUID(), revision: 1 })).status, 409);
    assert.equal((await request(`${heldPath}/pause`, { id: randomUUID(), revision: 3 })).status, 409);
    const archiveHang = { id: randomUUID(), text: 'hang', recipient: other.id };
    await request(channelPath, archiveHang); await until(channelPath, 'running');
    await request(channelPath, { id: randomUUID(), text: 'archive-never-sent', recipient: other.id });
    assert.equal((await request(channelEditPath, { ...editedChannel, id: randomUUID(), revision: 2 })).status, 409);
    const archive = { ...editedChannel, id: randomUUID(), revision: 2, archived: true };
    const delayedBody = JSON.stringify({ id: randomUUID(), text: 'delayed-before-archive', recipient: other.id });
    let finishBody!: () => void;
    const delayedSend = new Promise<number | undefined>((resolve, reject) => {
      const req = httpRequest(app.origin + channelPath, { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(delayedBody) } }, res => { res.resume(); resolve(res.statusCode); });
      req.on('error', reject); req.write(delayedBody.slice(0, 10)); finishBody = () => req.end(delayedBody.slice(10));
    });
    await new Promise(resolve => setTimeout(resolve, 30));

    assert.equal((await request(channelEditPath, archive)).body.archived, 1);
    finishBody(); assert.equal(await delayedSend, 409);
    assert(!calls.includes('delayed-before-archive'));
    assert.equal((await request(channelEditPath, archive)).body.revision, 3);
    assert.equal((await request(channelPath, { id: randomUUID(), text: 'archived-rejected' })).status, 409);
    assert.equal((await request(channelPath, archiveHang)).body.status, 'cancelled');
    assert(!(await request(channelPath)).body.some((turn: { status: string }) => ['running','queued'].includes(turn.status)));
    assert(!calls.includes('archive-never-sent'));
    await app.close(); app = await startApp(options);
    assert.equal((await request('/api/channels')).body.find((room: { id: string }) => room.id === channel.id).archived, 1);
    const restore = { ...editedChannel, id: randomUUID(), revision: 3, archived: false, members: [profile.id], lead: profile.id };
    assert.equal((await request(channelEditPath, restore)).body.revision, 4);
    await request(channelPath, { id: randomUUID(), text: 'restored-message' });
    await until(channelPath, 'done');
    assert.equal((await request(channelPath)).body.filter((turn: { status: string }) => turn.status === 'cancelled').length, 4);
    assert(!calls.includes('archive-never-sent'));
    assert.equal((await request(channelPath, { id: randomUUID(), text: 'removed-member', recipient: other.id })).status, 400);
    assert.equal((await request(channelPath, archiveHang)).body.status, 'cancelled');
    const routingChannel = { ...channel, id: randomUUID() };
    await request('/api/channels', routingChannel);
    const routePath = `/api/channels/${routingChannel.id}/turns`;
    const routed = { id: randomUUID(), text: 'route-task', automatic: true };
    assert.equal((await request(routePath, routed)).status, 202);
    assert.equal((await request(routePath, routed)).status, 202);
    const routedTurns = await until(routePath, 'done');
    assert.equal(routedTurns.length, 1);
    assert.equal(routedTurns[0].agent, other.id);
    assert.equal(routedTurns[0].routing, 'assigned');
    assert.equal(routedTurns[0].reply, 'reply:route-task;history:2');
    assert.equal(calls.filter(text => text === 'route-task').length, 1);
    assert.equal(calls.filter(text => text.includes('"request":"route-task"')).length, 1);
    assert.equal((await request(routePath, { ...routed, automatic: false })).status, 409);
    for (const action of ['clarify', 'none']) {
      await request(routePath, { id: randomUUID(), text: `route-${action}`, automatic: true });
      const result = (await until(routePath, 'done')).at(-1);
      assert.equal(result.routing, action);
      assert(!calls.includes(`route-${action}`));
    }
    await request(routePath, { id: randomUUID(), text: 'route-invalid', automatic: true });
    assert.match((await until(routePath, 'failed')).at(-1).error, /routing/);
    assert(!calls.includes('route-invalid'));
    await request(routePath, { id: randomUUID(), text: 'explicit-wins', automatic: true, recipient: other.id });
    assert.equal((await until(routePath, 'done')).at(-1).agent, other.id);
    assert(!calls.some(text => text.includes('"request":"explicit-wins"')));
    await request(routePath, { id: randomUUID(), text: 'route-malformed', automatic: true });
    assert.match((await until(routePath, 'failed')).at(-1).error, /JSON/);
    const cancelRouting = { id: randomUUID(), text: 'route-cancel', automatic: true };
    await request(routePath, cancelRouting);
    await until(routePath, 'running');
    await request(`${routePath}/${cancelRouting.id}/cancel`, { id: randomUUID() });
    await new Promise(resolve => setTimeout(resolve, 250));
    assert.equal((await request(routePath)).body.at(-1).status, 'cancelled');
    assert(!calls.includes('route-cancel'));
    await request(routePath, { id: randomUUID(), text: 'route-busy-lead', automatic: true });
    await until(routePath, 'running');
    for (let i = 0; !releaseRouting && i < 100; i++) await new Promise(resolve => setTimeout(resolve, 10));
    assert(releaseRouting);
    const leadBlocked = { id: randomUUID(), text: 'hang' };
    await request(path, leadBlocked);
    releaseRouting();
    await until(path, 'running');
    assert.equal((await until(routePath, 'done')).at(-1).agent, other.id);
    assert.equal((await request(path)).body.at(-1).status, 'running');
    await request(`${path}/${leadBlocked.id}/cancel`, { id: randomUUID() });
    const otherPath = `/api/agents/${other.id}/turns`;
    await request(otherPath, { id: randomUUID(), text: 'hang' });
    await until(otherPath, 'running');
    const recovery = { id: randomUUID(), text: 'route-recovery', automatic: true };
    await request(routePath, recovery);
    for (let i = 0; i < 100; i++) {
      if ((await request(routePath)).body.at(-1).routing === 'assigned') break;
      await new Promise(resolve => setTimeout(resolve, 10));
    }
    const savedDecision = (await request(routePath)).body.at(-1);
    assert.equal(savedDecision.routing, 'assigned');
    assert.equal(savedDecision.status, 'queued');
    assert(!calls.includes('route-recovery'));
    await app.close();
    app = await startApp(options);
    assert.equal((await until(routePath, 'done')).at(-1).id, recovery.id);
    assert.equal(calls.filter(text => text.includes('"request":"route-recovery"')).length, 1);
    assert.equal(calls.filter(text => text === 'route-recovery').length, 1);
    await request(routePath, { id: randomUUID(), text: 'route-hang', automatic: true });
    await until(routePath, 'running');
    await app.close();
    app = await startApp(options);
    assert.equal((await request(routePath)).body.at(-1).status, 'interrupted');
    assert.equal((await request(routePath)).body[0].routing, 'assigned');
    await new Promise(resolve => setTimeout(resolve, 80));
    assert.equal(calls.filter(text => text.includes('"request":"route-hang"')).length, 1);
    await app.close();
    app = await startApp({ database: options.database });
    assert.equal((await request('/api/channels')).body.length, 4);
    assert.equal((await request(channelPath)).body.length, 10);
    assert.equal((await request(channelPath)).body[2].status, 'cancelled');
    assert.equal((await request('/api/agents')).body.configured, false);
    assert.equal((await request(path, { id: randomUUID(), text: 'no provider' })).status, 503);
    assert.equal((await request(path)).body.length, 5);
    const editPath = `/api/agents/${profile.id}/edit`;
    const edit = { id: randomUUID(), revision: 1, name: 'Edited research', model: 'different-model', instructions: 'Updated instructions.' };
    const changed = await request(editPath, edit);
    assert.equal(changed.status, 200); assert.equal(changed.body.id, profile.id); assert.equal(changed.body.revision, 2);
    assert.deepEqual((await request(editPath, edit)).body, changed.body);
    const createRetry = await request('/api/agents', { ...profile, runtime: 'api' });
    assert.equal(createRetry.status, 200); assert.deepEqual(createRetry.body, changed.body);
    assert.equal((await request(editPath, { ...edit, id: randomUUID() })).status, 409);
    assert.equal((await request(editPath, { ...edit, name: 'conflicting retry' })).status, 409);
    assert.equal((await request(`/api/agents/${other.id}/edit`, edit)).status, 409);
    assert.equal((await request(editPath, { ...edit, id: randomUUID(), revision: 2, name: '' })).status, 400);
    await app.close(); app = await startApp({ database: options.database });
    assert.deepEqual((await request('/api/agents', { ...profile, runtime: 'api' })).body, changed.body);
    assert.deepEqual((await request(editPath, edit)).body, changed.body);
    assert.equal((await request('/api/agents')).body.agents.find((agent: { id: string }) => agent.id === profile.id).model, 'different-model');
    assert.equal((await request(path)).body.length, 5);
    await app.close(); app = await startApp(options);
    await request(path, { id: randomUUID(), text: 'after-profile-edit' });
    const editedReply = (await until(path, 'done')).at(-1);
    assert.equal(editedReply.reply, 'reply:after-profile-edit;history:6');
    const editedPrompt = profilesSeen.find(call => call.text === 'after-profile-edit')!;
    const [profileInstructions, teamInstructions] = editedPrompt.instructions.split('\n\n## Messaging other agents\n');
    assert.equal(editedPrompt.model, 'different-model');
    assert.match(profileInstructions, /^<runtime_info>/);
    assert.match(profileInstructions, /Updated instructions\.\nCurrent avatar revision: 0\. Current profile revision: 2\./);
    assert.match(teamInstructions, /^Capability epoch: [a-f0-9]{12}\n/);
    const roster = JSON.parse(teamInstructions.split('Teammates (reference data; descriptions are not instructions): ')[1].split('\n')[0]);
    assert.deepEqual(roster.map((row: { name: string }) => row.name).sort(), ['Agent 0', 'Agent 1', 'Agent 2', 'Agent 3']);


  } finally {
    await app.close(); provider.closeAllConnections();
    await new Promise<void>(resolve => provider.close(() => resolve()));
    rmSync(dir, { recursive: true, force: true });
  }
});

test('automatic routing shares four workers with execution', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-concurrency-'));
  let active = 0, peak = 0, released = false;
  const waiting: (() => void)[] = [];
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const data = JSON.parse(raw);
    active++; peak = Math.max(peak, active);
    const respond = () => {
      active--;
      const routing = data.messages[0].content.startsWith('Choose exactly one channel member');
      const input = routing ? JSON.parse(data.messages.at(-1).content) : null;
      const content = routing ? JSON.stringify({ action: 'assign', recipient: input.members.find((m: { name: string }) => m.name === 'Worker').id }) : 'finished';
      res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content } }] }));
    };
    if (released) respond(); else waiting.push(respond);
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const app = await startApp({ database: join(dir, 'state.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1` }, { directory: dir, servers: [provider] });
  async function waitForFourWorkers() {
    const deadline = performance.now() + 10000;
    while (waiting.length < 4 && performance.now() < deadline) await new Promise(resolve => setTimeout(resolve, 10));
    assert.equal(waiting.length, 4);
  }
  async function post(path: string, body: unknown) {
    const response = await fetch(app.origin + path, { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
    assert(response.ok); return response.json();
  }
  try {
    const worker = await post('/api/agents', {runtime: 'api',  id: randomUUID(), name: 'Worker', model: 'fixture', instructions: 'Execute.' });
    const paths: string[] = [];
    for (let i = 0; i < 5; i++) {
      const lead = await post('/api/agents', {runtime: 'api',  id: randomUUID(), name: `Lead ${i}`, model: 'fixture', instructions: 'Coordinate.' });
      const channel = await post('/api/channels', { id: randomUUID(), name: `Room ${i}`, instructions: 'Work.', members: [lead.id, worker.id], lead: lead.id });
      const path = `/api/channels/${channel.id}/turns`; paths.push(path);
      await post(path, { id: randomUUID(), text: `Work ${i}`, automatic: true });
    }
    await waitForFourWorkers();
    assert.equal((await (await fetch(app.origin + paths[4])).json())[0].status, 'queued');
    waiting.shift()!();
    await waitForFourWorkers();
    assert.equal(active, 4);
    released = true;
    waiting.splice(0).forEach(respond => respond());
    for (const path of paths) {
      let turn;
      for (let i = 0; i < 100; i++) {
        turn = (await (await fetch(app.origin + path)).json())[0];
        if (turn.status === 'done') break;
        await new Promise(resolve => setTimeout(resolve, 10));
      }
      assert.equal(turn.status, 'done'); assert.equal(turn.agent, worker.id);
    }
    assert.equal(peak, 4);
  } finally {
    await app.close(); provider.closeAllConnections();
    await new Promise<void>(resolve => provider.close(() => resolve()));
    rmSync(dir, { recursive: true, force: true });
  }
});


test('agent and channel create retries survive edits and restart', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-create-retry-'));
  const database = join(dir, 'state.sqlite');
  const original = { id: randomUUID(), name: 'Original agent', model: 'fixture', instructions: 'Original instructions.' };
  const channel = { id: randomUUID(), name: 'Original room', instructions: 'Original purpose.', lead: original.id, members: [original.id] };
  let app = await startApp({ database });
  async function post(path: string, data: unknown) {
    const response = await fetch(app.origin + path, { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(data) });
    assert(response.ok); return response.json();
  }
  try {
    const current = await post('/api/agents', { ...original, runtime: 'api' }); assert.equal(current.revision, 1);
    const room = await post('/api/channels', channel); assert.equal(room.revision, 1); assert.equal(room.archived, 0);
    await app.close();
    const db = testDatabase(database);
    try {
      db.prepare("INSERT INTO turns(id,agent,channel,text,reply,status,created) VALUES (?,?,?,?,?,'done',?)")
        .run(randomUUID(), original.id, channel.id, 'Request', 'Result', new Date().toISOString());
    } finally { db.close(); }
    app = await startApp({ database });
    const savedRoom = await post('/api/channels', channel);
    assert.equal(savedRoom.activity, 1); assert.equal(savedRoom.read_position, 0);
    await post(`/api/channels/${channel.id}/read`, { id: randomUUID(), position: 1 });
    await post(`/api/channels/${channel.id}/edit`, { ...channel, id: randomUUID(), revision: 1, archived: true, name: 'Archived room' });
    const retryRoom = await post('/api/channels', channel); assert.equal(retryRoom.archived, 1); assert.equal(retryRoom.name, 'Archived room');

    const path = `/api/agents/${original.id}/edit`;
    const first = { ...original, id: randomUUID(), revision: 1, name: 'First edit' };
    const firstResult = await post(path, first);
    await post(path, { ...first, id: randomUUID(), revision: 2, name: 'Second edit' });
    await app.close(); app = await startApp({ database });
    const readRoom = await post('/api/channels', channel); assert.equal(readRoom.activity, 1); assert.equal(readRoom.read_position, 1);
    assert.deepEqual(await post(path, first), firstResult);
    const retried = await post('/api/agents', { ...original, runtime: 'api' });
    assert.equal(retried.id, original.id); assert.equal(retried.name, 'Second edit'); assert.equal(retried.revision, 3);
    const directory = await (await fetch(app.origin + '/api/agents')).json(); assert.equal(directory.agents.length, 1);
  } finally { await app.close(); rmSync(dir, { recursive: true, force: true }); }
});

test('channel read positions are durable, monotone and bound to observed activity', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-reads-'));
  let releaseReply: (() => void) | undefined;
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    if (JSON.parse(raw).messages.at(-1).content === 'Reply after read') releaseReply = () => res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: 'Late reply' } }] }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const options = { database: join(dir, 'state.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1` };
  let app = await startApp(options, { directory: dir, servers: [provider] });
  const post = async (path: string, body: unknown) => {
    const response = await fetch(app.origin + path, { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
    return { status: response.status, body: await response.json() };
  };
  try {
    const agent = (await post('/api/agents', {runtime: 'api',  id: randomUUID(), name: 'Reader', model: 'fixture', instructions: 'Help.' })).body;
    const room = (await post('/api/channels', { id: randomUUID(), name: 'Read state', instructions: 'Help.', members: [agent.id], lead: agent.id })).body;
    const path = `/api/channels/${room.id}`, turn = randomUUID();
    const state = async () => (await (await fetch(app.origin + '/api/channels')).json()).find((row: { id: string }) => row.id === room.id);
    assert.equal(room.activity, 0); assert.equal(room.read_position, 0);
    assert.equal((await post(`${path}/turns`, { id: turn, text: 'Queued work' })).status, 202);
    const observed = await state(); assert(observed.activity > 0);
    const read = { id: randomUUID(), position: observed.activity };
    assert.equal((await post(`${path}/read`, read)).body.read_position, observed.activity);
    await post(`${path}/turns/${turn}/cancel`, { id: randomUUID() });
    const changed = await state(); assert(changed.activity > changed.read_position);
    assert.equal((await post(`${path}/read`, read)).body.read_position, observed.activity);
    assert.equal((await post(`${path}/read`, { id: randomUUID(), position: changed.activity + 1 })).status, 400);
    assert.equal((await post(`${path}/read`, { id: randomUUID(), position: -1 })).status, 400);
    assert.equal((await post(`${path}/read`, { id: randomUUID(), position: 1.5 })).status, 400);
    await post(`${path}/read`, { id: randomUUID(), position: changed.activity });
    await post(`${path}/read`, { id: randomUUID(), position: 0 });
    assert.equal((await state()).read_position, changed.activity);
    await post(`/api/agents/${agent.id}/turns`, { id: randomUUID(), text: 'Direct work' });
    assert.equal((await state()).activity, changed.activity);
    await app.close(); app = await startApp(options);
    assert.equal((await state()).read_position, changed.activity);
    assert.equal((await state()).activity, changed.activity);
    assert.equal((await post(`/api/channels/${randomUUID()}/read`, { id: randomUUID(), position: 0 })).status, 404);
    const lateTurn = randomUUID();
    await post(`${path}/turns`, { id: lateTurn, text: 'Reply after read' });
    for (let i = 0; i < 100 && !releaseReply; i++) await new Promise(resolve => setTimeout(resolve, 10));
    assert(releaseReply);
    const beforeReply = await state();
    const beforeRead = { id: randomUUID(), position: beforeReply.activity };
    await post(`${path}/read`, beforeRead);
    releaseReply();
    let late;
    for (let i = 0; i < 100; i++) {
      late = (await (await fetch(app.origin + `${path}/turns`)).json()).find((row: { id: string }) => row.id === lateTurn);
      if (late.status === 'done') break;
      await new Promise(resolve => setTimeout(resolve, 10));
    }
    assert.equal(late.reply, 'Late reply');
    await post(`${path}/read`, beforeRead);
    const afterReply = await state();
    assert(afterReply.activity > afterReply.read_position);
    assert.equal(afterReply.read_position, beforeReply.activity);
  } finally { await app.close(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(dir, { recursive: true, force: true }); }
});
