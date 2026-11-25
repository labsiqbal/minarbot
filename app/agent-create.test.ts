import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { testDatabase } from './test-support/database.ts';
import { startApp } from './test-bridge.ts';
import { restoreAgentCreation } from './shared/agent-create.ts';

test('creation recovery keeps exact command and rejects malformed authority', () => {
  const command = { id: randomUUID(), name: 'Agent baru', model: 'fixture', backupModel: '', instructions: 'Help with research', threadLimit: 2, providerAccount: '', setup: true };
  assert.deepEqual(restoreAgentCreation(JSON.stringify(command)), command); assert.equal(restoreAgentCreation(null), null);
  for (const value of [{ ...command, permissions: 'admin' }, { ...command, setup: 1 }, { ...command, id: 'bad' }, { ...command, threadLimit: 0 }, { ...command, instructions: '' }, { ...command, providerAccount: null }, { ...command, backupModel: 'fixture' }]) assert.throws(() => restoreAgentCreation(JSON.stringify(value)));
  assert.throws(() => restoreAgentCreation('x'.repeat(64001)));
});

test('prompt creation atomically queues one restricted setup and recovers without duplicate work', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-create-'));
  let mode = 'normal', calls = 0, edits = 0, continued = false;
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const body = JSON.parse(raw); calls++;
    assert.deepEqual(body.tools.map((tool: any) => tool.function.name).sort(), ['ask_user', 'edit_profile']);
    const result = body.messages.findLast((message: any) => message.role === 'tool' && message.tool_call_id === 'profile');
    const system = body.messages[0].content, revision = Number(system.match(/Current profile revision: (\d+)/)[1]);
    let tool;
    if (!result && mode !== 'no-save') {
      if (mode === 'escape') tool = { id: 'escape', type: 'function', function: { name: 'create_text_file', arguments: JSON.stringify({ name: 'no.txt', text: 'Forbidden' }) } };
      else if (mode === 'ask' && !JSON.stringify(body.messages).includes('Research')) tool = { id: 'question', type: 'function', function: { name: 'ask_user', arguments: JSON.stringify({ question: 'Which role?', choices: ['Research', 'Writing'] }) } };
      else { edits++; tool = { id: 'profile', type: 'function', function: { name: 'edit_profile', arguments: JSON.stringify({ revision, name: 'Research partner', role: 'Research agent', ...(mode === 'name-only' ? {} : { instructions: 'Compare sources and cite evidence.' }) }) } }; }
    }
    if (result && mode === 'hold') { continued = true; return; }
    res.end(JSON.stringify({ choices: [{ finish_reason: tool ? 'tool_calls' : 'stop', message: tool ? { tool_calls: [tool] } : { content: 'Profile setup checked.' } }] }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const options = { database: join(dir, 'state.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1` };
  let app = await startApp(options, { directory: dir, servers: [provider] });
  async function request(path: string, body?: unknown, status = 200) {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const data = await response.json(); assert.equal(response.status, status, JSON.stringify(data)); return data;
  }
  const command = () => ({ runtime: 'api', id: randomUUID(), name: 'Agent baru', model: 'fixture', instructions: 'I need an evidence-focused assistant.', setup: true });
  async function until(id: string, status: string) { for (let n = 0; n < 300; n++) { const rows = await request(`/api/agents/${id}/turns`); if (status === 'waiting' ? rows[0]?.paused === 1 && rows[0]?.questions?.length : rows[0]?.status === status) return rows[0]; await new Promise(resolve => setTimeout(resolve, 10)); } assert.fail(`Missing ${status}`); }
  try {
    const input = command(); await request('/api/agents', { ...input, runtime: 'api' }, 201); await until(input.id, 'done'); const before = calls;
    assert.equal((await request('/api/agents', { ...input, runtime: 'api' })).name, 'Research partner'); assert.equal((await request('/api/agents', { ...input, runtime: 'api' })).role, 'Research agent'); assert.equal(calls, before); assert.equal((await request(`/api/agents/${input.id}/turns`)).length, 1); assert((await request(`/api/agents/${input.id}/turns`))[0].text.includes(input.instructions));
    await request('/api/agents', { ...input, setup: false }, 409); await request('/api/agents', { ...input, instructions: 'Different' }, 409);
    const manual = { ...command(), setup: false }; await request('/api/agents', { ...manual, runtime: 'api' }, 201); assert.equal((await request(`/api/agents/${manual.id}/turns`)).length, 0);
    const malformed = { ...command(), setup: 'yes' }; await request('/api/agents', malformed, 400);
    mode = 'escape'; const escaped = command(); await request('/api/agents', { ...escaped, runtime: 'api' }, 201); const failed = await until(escaped.id, 'failed'); assert.equal(failed.generatedFiles.length, 0);
    mode = 'name-only'; const partial = command(); await request('/api/agents', { ...partial, runtime: 'api' }, 201); await until(partial.id, 'failed');
    mode = 'no-save'; const empty = command(); await request('/api/agents', { ...empty, runtime: 'api' }, 201); await until(empty.id, 'failed');
    mode = 'ask'; const question = command(); await request('/api/agents', { ...question, runtime: 'api' }, 201); const waiting = await until(question.id, 'waiting');
    await request(`/api/agents/${question.id}/turns/${question.id}/questions/${waiting.questions[0].id}/answer`, { id: randomUUID(), text: 'Research' }); await until(question.id, 'done');
    mode = 'hold'; const held = command(); await request('/api/agents', { ...held, runtime: 'api' }, 201);
    for (let n = 0; n < 300 && !continued; n++) await new Promise(resolve => setTimeout(resolve, 10)); assert(continued);
    await app.close(); app = await startApp(options); const count = edits; mode = 'normal';
    const stopped = await until(held.id, 'interrupted'); await request('/api/agents', { ...held, runtime: 'api' }); assert.equal(edits, count);
    await request(`/api/agents/${held.id}/turns/${held.id}/resume`, { id: randomUUID(), revision: stopped.revision, acknowledgeUncertain: true }); await until(held.id, 'done'); assert.equal(edits, count);
    await app.close(); app = await startApp({ database: options.database });
    const offline = command(); await request('/api/agents', { ...offline, runtime: 'api' }, 503); assert(!(await request('/api/agents')).agents.some((a: any) => a.id === offline.id));
    await request('/api/agents', { ...input, runtime: 'api' }); // Historical receipt survives provider removal.
    await app.close(); const db = testDatabase(options.database); db.exec("CREATE TRIGGER reject_setup BEFORE INSERT ON turns BEGIN SELECT RAISE(ABORT, 'synthetic setup fault'); END"); db.close();
    app = await startApp(options); const rollback = command(); await request('/api/agents', { ...rollback, runtime: 'api' }, 500); assert(!(await request('/api/agents')).agents.some((a: any) => a.id === rollback.id));
  } finally { await app.close(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(dir, { recursive: true, force: true }); }
});

import { ProtocolError, digestCommand, sha256 } from './shared/protocol.ts';
import { parseCanonicalAgentCreation, restoreCanonicalAgentCreation } from './test-support/canonical-agent-creation.ts';
import { retainedToolNames } from './test-bridge.ts';
import { canonicalHttp } from './test-support/canonical-http.ts';
import sharp from 'sharp';

function canonicalFixture() {
  const f = canonicalHttp('minarbot-agent-create-');
  const { db, owner } = f;
  const member = f.addAccount('user_member'), foreign = f.addAccount('user_foreign');
  function enroll(actor = owner, capabilities = ['execution']) {
    const installation = randomUUID(), device = randomUUID();
    db.prepare("INSERT INTO installations VALUES (?,?,'fixture',?,1,0,1,0)").run(installation, actor.accountId, installation);
    db.prepare("INSERT INTO devices VALUES (?,?,?,'fixture','fixture',?,?,'{}',1,0,'offline','Asia/Jakarta',1,NULL,?,1)").run(device, installation, actor.accountId, device, device, JSON.stringify(capabilities));
    return device;
  }
  const home = enroll(), project = randomUUID();
  db.prepare("INSERT INTO projects(id,account,name,private) VALUES (?,?,'A',0)").run(project, owner.accountId);
  db.prepare("INSERT INTO project_members VALUES (?,?,'owner',1,0),(?,?,'member',1,0)").run(project, owner.accountId, project, member.accountId);
  const command = (change = {}) => { const id = randomUUID(); return { requestId: id, id, name: 'Agent', model: 'fixture', backupModel: '', instructions: 'Help with research', threadLimit: 2, projectId: project, homeDeviceId: home, expectedProjectRevision: 1, expectedHomeDeviceRevision: 1, providerBinding: null, setup: false, ...change }; };
  const agentPath = (id: string, agent: string) => `/api/projects/${id}/agents/${agent}`;
  const access = {
    createAgent: (actor: typeof owner, body: Record<string, unknown>) => f.request('/api/agents', 'POST', body, actor),
    createProject: (actor: typeof owner, body: Record<string, unknown>) => f.request('/api/projects', 'POST', body, actor),
    agentParticipation: (actor: typeof owner, project: string, agent: string, body: Record<string, unknown>) => f.request(agentPath(project, agent), 'PUT', body, actor),
    editAgentRules: (actor: typeof owner, project: string, agent: string, body: Record<string, unknown>) => f.request(agentPath(project, agent) + '/rules', 'PUT', body, actor),
    editAgentCapabilities: (actor: typeof owner, project: string, agent: string, body: Record<string, unknown>) => f.request(agentPath(project, agent) + '/capabilities', 'PUT', body, actor),
    requireAgentExecution: (actor: typeof owner, project: string, agent: string) => {
      const requestId = randomUUID(), payload = { v: 1, kind: 'direct', message: { text: 'Check', files: [] } };
      const expectedRevisions = { project: 1, agent: 1, homeBinding: 1, membership: 1, assignment: 1, capability: 4 };
      return f.request('/api/tasks', 'POST', {
        requestId, projectId: project, agentId: agent, homeDeviceId: home, payload, expectedRevisions,
        payloadDigest: digestCommand(['minarbot-task-payload-v1', requestId, project, agent, home, payload, expectedRevisions]),
      }, actor);
    },
  };
  return { ...f, access, member, foreign, home, project, command, enroll };
}
const code = (expected: string) => (error: unknown) => error instanceof ProtocolError && error.code === expected;

test('canonical draft carries project/home authority and rejects legacy or malformed provenance', async () => {
  const f = canonicalFixture();
  try {
    const command = f.command(); assert.deepEqual(restoreCanonicalAgentCreation(JSON.stringify(command)), command);
    assert.equal(restoreCanonicalAgentCreation(null), null);
    for (const change of [{ homeDeviceId: 'bad' }, { requestId: randomUUID() }, { expectedProjectRevision: 0 }, { expectedHomeDeviceRevision: 1.5 }, { providerBinding: {} }, { providerAccount: '' }, { setup: 'yes' }, { toolBudget: 9 }, { toolBudget: null }, { toolBudget: 201 }]) assert.throws(() => parseCanonicalAgentCreation({ ...command, ...change }));
    assert.throws(() => restoreCanonicalAgentCreation(JSON.stringify({ id: randomUUID(), name: 'Old draft' })));
  } finally { await f.close(); }
});

test('canonical creation is owner/home scoped, offline-safe, exact and fully atomic', async () => {
  const f = canonicalFixture(), { db, access, owner, member, foreign, home, project } = f;
  try {
    await f.start();
    await assert.rejects(access.createAgent(member, f.command()), code('owner_required'));
    await assert.rejects(access.createAgent(foreign, f.command()), code('resource_not_found'));
    await assert.rejects(access.createAgent(owner, f.command({ expectedProjectRevision: 2 })), code('stale_revision'));
    await assert.rejects(access.createAgent(owner, f.command({ expectedHomeDeviceRevision: 2 })), code('stale_revision'));
    await assert.rejects(access.createAgent(owner, f.command({ homeDeviceId: f.enroll(foreign) })), code('resource_not_found'));
    await assert.rejects(access.createAgent(owner, f.command({ homeDeviceId: f.enroll(owner, []) })), code('capability_missing'));
    await assert.rejects(access.createAgent(owner, f.command({ setup: true })), code('setup_unavailable'));
    db.prepare("UPDATE devices SET status='blocked' WHERE id=?").run(home);
    await assert.rejects(access.createAgent(owner, f.command()), code('resource_not_found'));
    db.prepare("UPDATE devices SET status='offline' WHERE id=?").run(home);
    const command = f.command({ role: ' Orchestration agent ', toolBudget: 30 }), result = await access.createAgent(owner, command);
    assert.equal(db.prepare('SELECT role FROM agents WHERE id=?').get(command.id)!.role, 'Orchestration agent');
    assert.equal(db.prepare('SELECT tool_budget FROM agents WHERE id=?').get(command.id)!.tool_budget, 30);
    await assert.rejects(access.createAgent(owner, { ...command, role: 'Personal agent' }), code('payload_conflict'));
    await assert.rejects(access.createAgent(owner, f.command({ role: 'x'.repeat(61) })), code('invalid_agent'));
    assert.equal(result.status, 201); assert.equal(result.data.homeDeviceId, home);
    assert.equal(db.prepare('SELECT count(*) AS n FROM turns').get()!.n, 0);
    assert.deepEqual(JSON.parse(String(db.prepare('SELECT capabilities_json FROM agent_projects WHERE agent=?').get(command.id)!.capabilities_json)), { v: 1, tools: retainedToolNames, resources: [] });
    assert.equal(db.prepare('SELECT rules FROM agent_projects WHERE agent=?').get(command.id)!.rules, '');
    assert.equal(db.prepare('SELECT count(*) AS n FROM collaborator_agents').get()!.n, 0);
    db.prepare("UPDATE devices SET status='revoked',revision=revision+1 WHERE id=?").run(home);
    assert.deepEqual((await access.createAgent(owner, command)).data, result.data);
    await assert.rejects(access.createAgent(owner, { ...command, instructions: 'Changed' }), code('payload_conflict'));
    assert.throws(() => db.prepare('UPDATE agent_authority SET home_device=? WHERE agent=?').run(f.enroll(), command.id), /agent_home_immutable/);
    db.prepare("UPDATE devices SET status='offline',revision=1 WHERE id=?").run(home);
    for (const table of ['agents', 'agent_authority', 'agent_projects', 'record_scopes', 'agent_creations', 'receipts']) {
      const failed = f.command(); db.exec(`CREATE TRIGGER reject_insert BEFORE INSERT ON ${table} BEGIN SELECT RAISE(ABORT,'synthetic insertion fault'); END`);
      await assert.rejects(access.createAgent(owner, failed), code('storage_failed'));
      db.exec('DROP TRIGGER reject_insert');
      for (const [table, key] of [['agents', 'id'], ['agent_authority', 'agent'], ['agent_projects', 'agent'], ['agent_creations', 'id'], ['receipts', 'request_id']]) assert.equal(db.prepare(`SELECT count(*) AS n FROM ${table} WHERE ${key}=?`).get(failed.id)!.n, 0);
    }
    assert.equal(db.prepare('SELECT count(*) AS n FROM agents').get()!.n, 1);
    assert.equal(db.prepare('SELECT revision FROM projects WHERE id=?').get(project)!.revision, 1);
  } finally { await f.close(); }
});

test('provider binding stays explicit and avatar receipt hashes original bytes', async () => {
  const f = canonicalFixture(), { db, access, owner, home } = f;
  try {
    await f.start();
    const binding = randomUUID(), otherHome = f.enroll();
    db.prepare("INSERT INTO provider_bindings VALUES (?, ?, ?, 'openai-compatible', 'owner-provider', 'Owner', 'missing', 1)").run(binding, otherHome, owner.accountId);
    const command = f.command({ providerBinding: { bindingId: binding, expectedRevision: 1 } });
    await assert.rejects(access.createAgent(owner, command), code('resource_not_found'));
    db.prepare('UPDATE provider_bindings SET device=? WHERE id=?').run(home, binding);
    await assert.rejects(access.createAgent(owner, { ...command, providerBinding: { bindingId: binding, expectedRevision: 2 } }), code('stale_revision'));
    const original = await sharp({ create: { width: 12, height: 20, channels: 4, background: '#c53e20' } }).png().toBuffer();
    const withAvatar = { ...command, avatar: original.toString('base64') };
    const result = await access.createAgent(owner, withAvatar);
    assert.equal(db.prepare('SELECT provider_account FROM agents').get()!.provider_account, 'owner-provider');
    const image = db.prepare('SELECT image,revision FROM agent_avatars').get()!, metadata = await sharp(image.image as Uint8Array).metadata();
    assert.equal(metadata.width, 256); assert.equal(metadata.height, 256); assert.equal(image.revision, 1);
    assert.equal(result.receipt.payloadDigest, digestCommand(['minarbot-agent-create-v1', { ...command, avatar: sha256(original) }]));
    assert(!String(db.prepare('SELECT request FROM agent_creations').get()!.request).includes(withAvatar.avatar));
    db.prepare("UPDATE provider_bindings SET readiness='revoked',revision=2").run();
    assert.equal((await access.createAgent(owner, withAvatar)).status, 200);
    await assert.rejects(access.createAgent(owner, f.command({ avatar: 'iVBORw0KGgo=' })), code('invalid_avatar'));
  } finally { await f.close(); }
});

test('canonical setup admission rolls back every row and requires a ready explicit home binding', async () => {
  const f=canonicalFixture(),{db,access,owner,home}=f,binding=randomUUID();
  try{
    await f.start();
    db.prepare("INSERT INTO provider_bindings VALUES (?,?,?,'openai-compatible','setup','Setup','missing',1)").run(binding,home,owner.accountId);
    const command=()=>f.command({setup:true,providerBinding:{bindingId:binding,expectedRevision:1}});
    for(const readiness of ['missing','unreadable','unavailable']){
      db.prepare('UPDATE provider_bindings SET readiness=? WHERE id=?').run(readiness,binding);
      await assert.rejects(access.createAgent(owner,command()),code('provider_unavailable'));
      assert.equal(db.prepare('SELECT count(*) n FROM agents').get()!.n,0);
    }
    db.prepare("UPDATE provider_bindings SET readiness='ready' WHERE id=?").run(binding);
    const tables=['agents','agent_authority','agent_projects','agent_creations','turns','tasks','receipts','record_scopes'],counts=()=>tables.map(table=>db.prepare(`SELECT count(*) n FROM ${table}`).get()!.n),before=counts();
    for(const table of ['turns','tasks','receipts']){
      const failed=command();db.exec(`CREATE TRIGGER reject_setup BEFORE INSERT ON ${table} BEGIN SELECT RAISE(ABORT,'setup rollback'); END`);
      await assert.rejects(access.createAgent(owner,failed),code('storage_failed'));db.exec('DROP TRIGGER reject_setup');
      assert.deepEqual(counts(),before);
    }
    db.prepare('UPDATE project_members SET grant_revision=3 WHERE project=? AND account=?').run(f.project,owner.accountId);
    const input=command(),created=await access.createAgent(owner,input),task=db.prepare('SELECT * FROM tasks WHERE id=?').get(input.id)!;
    assert.equal(created.status,201);assert.equal(task.state,'waiting_device');assert.equal(task.assignment_revision,3);assert.equal(task.provider_binding_id,binding);
    assert.equal(db.prepare('SELECT profile_setup FROM turns WHERE id=?').get(input.id)!.profile_setup,1);
    db.prepare("UPDATE provider_bindings SET readiness='unavailable',revision=2 WHERE id=?").run(binding);
    db.prepare("UPDATE devices SET status='revoked',revision=2 WHERE id=?").run(home);
    assert.deepEqual((await access.createAgent(owner,input)).data,created.data);
    assert.equal(db.prepare('SELECT count(*) n FROM tasks').get()!.n,1);
  }finally{await f.close();}
});

test('second project participation and shared CAS isolate rules/capabilities and stale grants', async () => {
  const f = canonicalFixture(), { db, access, owner, member, foreign, project } = f;
  try {
    await f.start();
    const command = f.command(); await access.createAgent(owner, command);
    const second = randomUUID(); await access.createProject(owner, { requestId: randomUUID(), id: second, name: 'B' });
    const participation = { requestId: randomUUID(), expectedProjectRevision: 1, expectedAgentRevision: 1, expectedAuthorityRevision: 1, expectedParticipationRevision: 0, rules: 'Only B', capabilities: { v: 1, tools: ['read_history'], resources: [] } };
    const result = await access.agentParticipation(owner, second, command.id, participation); assert.equal(result.status, 201);
    assert.equal((await access.agentParticipation(owner, second, command.id, participation)).status, 200);
    await assert.rejects(() => access.agentParticipation(owner, second, command.id, { ...participation, requestId: randomUUID() }), code('participation_exists'));
    await assert.rejects(() => access.agentParticipation(foreign, second, command.id, participation), code('resource_not_found'));
    await assert.rejects(() => access.requireAgentExecution(member, second, command.id), code('resource_not_found'));
    const rules = { requestId: randomUUID(), expectedParticipationRevision: 1, rules: 'B only, revised' };
    await access.editAgentRules(owner, second, command.id, rules);
    assert.equal((await access.editAgentRules(owner, second, command.id, rules)).data.revision, 2);
    await assert.rejects(() => access.editAgentRules(owner, second, command.id, { ...rules, rules: 'changed' }), code('payload_conflict'));
    const caps = { requestId: randomUUID(), expectedRevision: 1, capabilities: { v: 1, tools: [], resources: [] } };
    await assert.rejects(() => access.editAgentCapabilities(owner, second, command.id, caps), code('stale_revision'));
    const update = { ...caps, expectedRevision: 2 }; await access.editAgentCapabilities(owner, second, command.id, update);
    assert.equal((await access.editAgentCapabilities(owner, second, command.id, update)).data.revision, 3);
    await assert.rejects(() => access.editAgentCapabilities(owner, second, command.id, { ...update, capabilities: { v: 1, tools: ['unknown'], resources: [] } }), code('capability_missing'));
    await assert.rejects(() => access.editAgentCapabilities(owner, second, command.id, { ...update, capabilities: { v: 1, tools: [], resources: [], role: 'owner' } }), code('invalid_capabilities'));
    assert.equal(db.prepare('SELECT rules FROM agent_projects WHERE project=?').get(project)!.rules, '');
    db.prepare("INSERT INTO project_members VALUES (?,?,'member',1,0)").run(second, member.accountId);
    db.prepare('INSERT INTO collaborator_agents VALUES (?,?,?,1,1)').run(second, member.accountId, command.id);
    db.prepare('INSERT INTO response_policies VALUES (?,?,?,1,1)').run(second, member.accountId, command.id);
    db.prepare('UPDATE agent_projects SET deleted=1 WHERE project=?').run(second);
    await assert.rejects(() => access.editAgentRules(owner, second, command.id, rules), code('resource_not_found'));
    const reactivate = { ...participation, requestId: randomUUID(), expectedParticipationRevision: 3 };
    await assert.rejects(() => access.agentParticipation(owner, second, command.id, { ...reactivate, expectedParticipationRevision: 0 }), code('stale_revision'));
    assert.equal((await access.agentParticipation(owner, second, command.id, reactivate)).data.participationRevision, 4);
    assert.equal(db.prepare('SELECT enabled FROM collaborator_agents').get()!.enabled, 0);
    assert.equal(db.prepare('SELECT respond FROM response_policies').get()!.respond, 0);
    await assert.rejects(() => access.requireAgentExecution(member, second, command.id), code('execution_denied'));
    assert.equal(db.prepare('SELECT count(*) AS n FROM agent_authority').get()!.n, 1);
  } finally { await f.close(); }
});


test('local role create/edit receipts preserve normalized profile, omissions and revision conflicts across restart', async () => {
  const dir = mkdtempSync(join(process.cwd(), 'node_modules/.agent-role-'));
  const options = { database: join(dir, 'state.sqlite') };
  let app = await startApp(options, { directory: dir });
  async function request(path: string, body?: unknown, status = 200) {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const data = await response.json(); assert.equal(response.status, status, JSON.stringify(data)); return data;
  }
  try {
    const input = { id: randomUUID(), name: 'Odd name', role: '  Orchestration agent  ', model: 'fixture', instructions: 'Coordinate.' };
    const created = await request('/api/agents', { ...input, runtime: 'api' }, 201);
    assert.equal(created.role, 'Orchestration agent');
    assert.deepEqual(await request('/api/agents', { ...input, runtime: 'api' }), created);
    await request('/api/agents', { ...input, role: 'Personal agent' }, 409);
    for (const role of [null, 1, 'x'.repeat(61), 'bad\nrole', 'bad\u0085role', 'bad\u2028role']) await request('/api/agents', { ...input, id: randomUUID(), role }, 400);
    const path = `/api/agents/${created.id}/edit`, edit = { id: randomUUID(), revision: 1, name: created.name, role: 'x'.repeat(60), model: created.model, instructions: created.instructions };
    const updated = await request(path, edit); assert.equal(updated.role, edit.role); assert.equal(updated.revision, 2);
    assert.deepEqual(await request(path, edit), updated);
    await request(path, { ...edit, role: 'Different' }, 409);
    await request(path, { ...edit, id: randomUUID() }, 409);
    for (const role of [null, 1, 'x'.repeat(61), 'bad\rrole']) await request(path, { ...edit, id: randomUUID(), revision: 2, role }, 400);
    const { role, ...withoutRole } = edit;
    const preserved = await request(path, { ...withoutRole, id: randomUUID(), revision: 2 }); assert.equal(preserved.role, edit.role);
    const cleared = await request(path, { ...edit, id: randomUUID(), revision: 3, role: '   ' }); assert.equal(cleared.role, '');
    await app.close(); app = await startApp(options);
    assert.deepEqual(await request(path, edit), updated);
    assert.equal((await request('/api/agents')).agents[0].role, '');
    assert.equal((await request('/api/agents', { ...input, runtime: 'api' })).role, '');
  } finally { await app.close(); rmSync(dir, { recursive: true, force: true }); }
});
