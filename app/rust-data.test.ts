import test from 'node:test';
import assert from 'node:assert/strict';
import { testDatabase } from './test-support/database.ts';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { randomUUID } from 'node:crypto';
import { startRustServer } from './rust-test-server.ts';
import { retainedToolNames } from './test-bridge.ts';
import { canonicalHttp } from './test-support/canonical-http.ts';
import { canonicalJson, digestCommand } from './shared/protocol.ts';

test('Rust data reads preserve seeded history and local CRUD receipts across restart', { timeout: 20000 }, async () => {
  const root = mkdtempSync(join(tmpdir(), 'minarbot-rust-data-')), database = join(root, 'db.sqlite');
  let initial: Awaited<ReturnType<typeof startRustServer>> | undefined;
  let rust: Awaited<ReturnType<typeof startRustServer>> | undefined;
  try {
    initial = await startRustServer({ database });
    const agent = randomUUID(), turn = randomUUID();
    await initial.close(); initial = undefined;
    const db = testDatabase(database);
    db.prepare('INSERT INTO agents(id,name,model,instructions) VALUES(?,?,?,?)').run(agent, 'Saved agent', 'fixture', 'Help');
    db.prepare("INSERT INTO turns(id,agent,text,status,reply,created) VALUES(?,?,?,'done',?,?)").run(turn, agent, 'Saved question', 'Saved answer', '2026-01-02T03:04:05.000Z');
    const call = db.prepare("INSERT INTO provider_calls(turn,agent,model,origin,purpose,status,created) VALUES(?,?,?,'test','completion','received',?)").run(turn, agent, 'fixture', '2026-01-02T03:04:05.000Z').lastInsertRowid;
    db.prepare('INSERT INTO provider_streams VALUES(?,?)').run(call, 'Saved answer');
    db.prepare("INSERT INTO provider_stream_segments VALUES(?,0,'text-0','text',?,'',1,3)").run(call, 'Saved answer');
    db.prepare('INSERT INTO harness_approvals(id,turn,agent,runtime,workspace,kind,description,input,state) VALUES(?,?,?,?,?,?,?,?,?)').run(randomUUID(), turn, agent, 'codex', '/fixture', 'command', 'Saved approval', '{"command":"pwd"}', 'decided');
    db.prepare('INSERT INTO composio_calls VALUES(?,?,?,?,?,?,?,?,?,?,?)').run(turn, 'saved-tool', agent, JSON.stringify({ function: { name: 'fixture', arguments: '{"query":"saved"}' } }), JSON.stringify({ toolkit: 'fixture', name: 'Lookup', slug: 'LOOKUP', account: 'saved' }), 'done', 'approved', '{}', 0, 0, 1);
    const thread = randomUUID(), delivery = randomUUID(), request = randomUUID();
    db.prepare('INSERT INTO threads(id,agent,name) VALUES(?,?,?)').run(thread, agent, 'Saved delivery');
    db.prepare("INSERT INTO turns(id,agent,thread,text,status,reply,created) VALUES(?,?,?,?,'done',?,?)").run(delivery, agent, thread, 'Teammate task', 'Teammate result', '2026-01-02T03:04:06.000Z');
    db.prepare('INSERT INTO agent_requests VALUES(?,?,?,1,1)').run(request, turn, 'saved-request');
    db.prepare('INSERT INTO agent_deliveries VALUES(?,?)').run(request, delivery);
    const channel = randomUUID(), room = randomUUID();
    db.prepare("INSERT INTO channels(id,name,instructions,lead) VALUES(?,'Saved room','Help',?)").run(channel, agent);
    db.prepare('INSERT INTO channel_members VALUES(?,?)').run(channel, agent);
    db.prepare("INSERT INTO turns(id,agent,channel,text,status,created) VALUES(?,?,?,'Room question','interrupted','2026-01-02T03:04:07.000Z')").run(room, agent, channel);
    db.prepare('INSERT INTO room_discussions(root,members,lead,mentioned) VALUES(?,?,?,0)').run(room, JSON.stringify([agent]), agent);
    db.prepare('INSERT INTO room_turns(turn,root,round,speaker) VALUES(?,?,0,?)').run(room, room, agent);
    const roomCall = db.prepare("INSERT INTO provider_calls(turn,agent,model,origin,purpose,status,created) VALUES(?,?,?,'test','completion','interrupted',?)").run(room, agent, 'fixture', '2026-01-02T03:04:07.000Z').lastInsertRowid;
    db.prepare('INSERT INTO provider_streams VALUES(?,?)').run(roomCall, '“[SILENT]”');
    db.prepare("INSERT INTO provider_stream_segments VALUES(?,0,'text-0','text',?,'',0,1)").run(roomCall, '“[SILENT]”');
    db.close();
    const paths = [`/api/agents/${agent}/turns`, '/api/usage', '/api/conversations/settings', `/api/agents/${agent}/threads`, '/api/channels', `/api/channels/${channel}/turns`, `/api/agents/${agent}/panel`, `/api/threads/${thread}/panel/files`];
    initial = await startRustServer({ database });
    const expected = [];
    for (const path of paths) { const res: Response = await fetch(initial.origin + path); assert.equal(res.status, 200); expected.push(await res.json()); }
    await initial.close(); initial = undefined;
      rust = await startRustServer({ database });
    for (let i = 0; i < paths.length; i++) { const res: Response = await fetch(rust.origin + paths[i]); assert.equal(res.status, 200, paths[i] + ' ' + await (res.status === 200 ? Promise.resolve('') : res.text())); assert.deepEqual(await res.json(), expected[i], paths[i]); }
    const post = async (path: string, body: unknown, status = 200) => { const res: Response = await fetch(rust!.origin + path, { method: 'POST', headers: { Origin: rust!.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body) }); const result = await res.json(); assert.equal(res.status, status, JSON.stringify(result)); return result; };
    const command = { id: randomUUID(), name: 'Side' }, path = `/api/agents/${agent}/threads`;
    const created = await post(path, command);
    assert.deepEqual(await post(path, command), created);
    await post(path, { ...command, name: 'Conflict' }, 409);
    const deletion = { id: randomUUID(), revision: 1 };
    assert.equal((await post(`/api/threads/${created.id}/delete`, deletion)).deleted, 1);
    assert.equal((await post(`/api/threads/${created.id}/delete`, deletion)).deleted, 1);
  } finally { await initial?.close(); await rust?.close(); rmSync(root, { recursive: true, force: true }); }
});

test('Rust deletion rolls back FK closure, preserves room history and retries external cleanup', { timeout: 20000 }, async () => {
  const { mkdirSync, writeFileSync, existsSync } = await import('node:fs');
  const { execFileSync } = await import('node:child_process');
  const root = mkdtempSync(join(tmpdir(), 'minarbot-rust-delete-')), database = join(root, 'db.sqlite');
  let app: Awaited<ReturnType<typeof startRustServer>> | undefined;
  try {
    const initial = await startRustServer({ database }); await initial.close();
    const db = testDatabase(database), agent = randomUUID(), other = randomUUID(), direct = randomUUID(), room = randomUUID(), channel = randomUUID();
    const desktop = join(root, 'agent-desktops', agent), workspace = join(root, 'workspace');
    mkdirSync(desktop, { recursive: true }); writeFileSync(join(desktop, 'saved'), 'saved'); mkdirSync(workspace);
    execFileSync('git', ['init', '-q', workspace]);
    execFileSync('git', ['-C', workspace, '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.test', 'commit', '-qm', 'Fixture', '--allow-empty']);
    const hash = execFileSync('git', ['-C', workspace, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim(), ref = `refs/minarbot/checkpoints/${direct}`;
    execFileSync('git', ['-C', workspace, 'update-ref', ref, hash]);
    for (const id of [agent, other]) db.prepare("INSERT INTO agents(id,name,model,instructions) VALUES(?,'Fixture','fixture','Help')").run(id);
    db.prepare("INSERT INTO channels(id,name,instructions,lead) VALUES(?,'Room','Help',?)").run(channel, agent);
    db.prepare('INSERT INTO channel_members VALUES(?,?)').run(channel, agent);
    db.prepare("INSERT INTO turns(id,agent,text,status,created) VALUES(?,?,'Private','done','fixture')").run(direct, agent);
    db.prepare("INSERT INTO turns(id,agent,channel,text,reply,status,created) VALUES(?,?,?,'Shared','Saved reply','done','fixture')").run(room, agent, channel);
    db.prepare('INSERT INTO room_discussions(root,members,lead,mentioned) VALUES(?,?,?,0)').run(room, JSON.stringify([agent, other]), agent);
    db.prepare('INSERT INTO room_turns(turn,root,round,speaker) VALUES(?,?,0,?)').run(room, room, agent);
    db.prepare("INSERT INTO room_messages(root,turn,kind,agent,text) VALUES(?,?,'reply',?,'Saved reply')").run(room, room, agent);
    db.prepare("INSERT INTO workspace_checkpoints VALUES(?,?,?,?,?,'done',0)").run(direct, agent, workspace, ref, hash);
    db.exec("CREATE TRIGGER fail_delete BEFORE DELETE ON agents BEGIN SELECT RAISE(ABORT,'fixture'); END;"); db.close();
    app = await startRustServer({ database });
    const command = { id: randomUUID() }, path = `/api/agents/${agent}/delete`;
    const remove = () => fetch(app!.origin + path, { method: 'POST', headers: { Origin: app!.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(command) });
    assert.equal((await remove()).status, 500); assert(existsSync(desktop));
    const retry = testDatabase(database);
    assert(retry.prepare('SELECT 1 FROM agents WHERE id=?').get(agent)); assert(retry.prepare('SELECT 1 FROM turns WHERE id=?').get(direct));
    assert.equal(retry.prepare('SELECT agent FROM turns WHERE id=?').get(room)!.agent, agent);
    retry.exec('DROP TRIGGER fail_delete'); retry.close();
    assert.equal((await remove()).status, 200); assert(!existsSync(desktop));
    assert.equal((await remove()).status, 200);
    assert.equal(execFileSync('git', ['-C', workspace, 'for-each-ref', ref], { encoding: 'utf8' }), '');
    const history = await (await fetch(app.origin + `/api/channels/${channel}/turns`)).json();
    assert.equal(history[0].agent, null); assert.equal(history[0].reply, 'Saved reply'); assert.equal(history[0].agent_name, 'Deleted agent');
    const saved = testDatabase(database); assert.deepEqual(saved.prepare('PRAGMA foreign_key_check').all(), []);
    assert.equal(saved.prepare('SELECT count(*) AS n FROM agent_delete_checkpoints').get()!.n, 0); saved.close();
  } finally { await app?.close(); rmSync(root, { recursive: true, force: true }); }
});

test('Rust canonical projects preserve ownership, CAS and durable command receipts', { timeout: 20000 }, async () => {
  const f = canonicalHttp('minarbot-rust-projects-'), { db, owner } = f;
  const account = owner.accountId, project = randomUUID();
  const projectPath = `/api/projects/${project}`;
  const send = f.send;
  try {
    await f.start();
    assert.equal((await send('/api/agents')).status, 404);
    const command = { requestId: randomUUID(), id: project, name: ' Shared project ' };
    const created = await send('/api/projects', 'POST', command);
    assert.equal(created.status, 201);
    assert.deepEqual(created.body.data, { projectId: project, name: 'Shared project', private: false, revision: 1 });
    const { committedAt, ...receipt } = created.body.receipt;
    assert(Number.isSafeInteger(committedAt));
    assert.deepEqual(receipt, {
      requestId: command.requestId, actorId: account, projectId: project,
      payloadDigest: digestCommand(['minarbot-project-create-v1', { ...command, name: 'Shared project' }]),
      status: 201, recordId: project, recordRevision: 1,
    });
    assert.deepEqual((await send('/api/projects', 'POST', command)).body, created.body);
    const row = { id: project, name: 'Shared project', private: 0, revision: 1, role: 'owner', grant_revision: 1 };
    assert.deepEqual((await send('/api/projects')).body.data, [row]);
    assert.deepEqual((await send(projectPath)).body.data, { ...row, account, deleted: 0 });

    const home = randomUUID(), installation = randomUUID(), binding = randomUUID();
    db.prepare("INSERT INTO installations(id,account,signing_public_key,signing_thumbprint,revision,created) VALUES(?,?,'fixture',?,1,0)").run(installation, account, installation);
    db.prepare(`INSERT INTO devices(id,installation,account,signing_public_key,encryption_public_key,signing_thumbprint,encryption_thumbprint,descriptor_json,generation,connection_generation,status,zone,zone_revision,capabilities_json,revision) VALUES(?,?,?,'fixture','fixture','fixture','fixture','{}',1,0,'offline','UTC',1,'["execution"]',1)`).run(home, installation, account);
    db.prepare("INSERT INTO provider_bindings VALUES(?,?,?,'openai-compatible','fixture','Fixture','ready',1)").run(binding, home, account);
    const aid = randomUUID(), creation = { requestId: aid, id: aid, name: ' Agent ', role: ' Helper ', model: 'fixture', backupModel: '', instructions: 'Help', threadLimit: 2, projectId: project, homeDeviceId: home, expectedProjectRevision: 1, expectedHomeDeviceRevision: 1, providerBinding: null, setup: false };
    const agentData = { agentId: aid, agentRevision: 1, authorityRevision: 1, projectId: project, participationRevision: 1, homeDeviceId: home, avatarRevision: 0 };
    const createdAgent = await send('/api/agents', 'POST', creation);
    assert.equal(createdAgent.status, 201, JSON.stringify(createdAgent));
    assert.deepEqual(createdAgent.body.data, agentData);
    assert.equal(createdAgent.body.receipt.payloadDigest, digestCommand(['minarbot-agent-create-v1', { ...creation, name: 'Agent', role: 'Helper' }]));
    assert.deepEqual((await send('/api/agents', 'POST', creation)).body, createdAgent.body);

    const setupId = randomUUID(), setup = { ...creation, requestId: setupId, id: setupId, providerBinding: { bindingId: binding, expectedRevision: 1 }, setup: true };
    const createdSetup = await send('/api/agents', 'POST', setup);
    assert.equal(createdSetup.status, 201, JSON.stringify(createdSetup));
    assert.deepEqual(createdSetup.body.data, { ...agentData, agentId: setupId });
    const taskFields = 'id,logical_turn,project,agent,initiator_account,home_device,assignment_revision,capability_revision,capability_digest,capability_snapshot_json,provider_binding_id,provider_binding_revision,provider_binding_owner,state,payload_digest,payload_json,revision';
    const caps = { v: 1, tools: retainedToolNames, resources: [] };
    const payload = { v: 1, kind: 'direct', message: { text: `Initial name: "Agent". Draft this agent's profile from the needs in the initial instructions. Use edit_profile to save a fitting name, a short role and clear working instructions. Do not start the actual work or change the account, permissions or model. If the needs are unclear, ask with ask_user. After saving the profile, briefly explain what the agent can and cannot do.

Initial needs:
Help`, files: [] } };
    const revisions = { project: 1, agent: 1, homeBinding: 1, membership: 1, assignment: 1, capability: 1 };
    assert.deepEqual({ ...db.prepare(`SELECT ${taskFields} FROM tasks WHERE id=?`).get(setupId) }, {
      id: setupId, logical_turn: setupId, project, agent: setupId, initiator_account: account, home_device: home,
      assignment_revision: 1, capability_revision: 1, capability_digest: digestCommand(['minarbot-capabilities-v1', caps]),
      capability_snapshot_json: canonicalJson(caps), provider_binding_id: binding, provider_binding_revision: 1,
      provider_binding_owner: account, state: 'waiting_device', payload_digest: digestCommand(['minarbot-task-payload-v1', setupId, project, setupId, home, payload, revisions]),
      payload_json: canonicalJson(payload), revision: 1,
    });

    const second = randomUUID(), projectCommand = { requestId: randomUUID(), id: second, name: 'Second' };
    assert.equal((await send('/api/projects', 'POST', projectCommand)).status, 201);
    const agentPath = `/api/projects/${second}/agents/${aid}`;
    const participation = { requestId: randomUUID(), expectedProjectRevision: 1, expectedAgentRevision: 1, expectedAuthorityRevision: 1, expectedParticipationRevision: 0, rules: 'Project rules\nSecond line', capabilities: { v: 1, tools: ['read_history'], resources: [] } };
    const attached = await send(agentPath, 'PUT', participation);
    assert.equal(attached.status, 201, JSON.stringify(attached));
    assert.deepEqual(attached.body.data, { projectId: second, agentId: aid, participationRevision: 1, homeDeviceId: home });
    assert.equal(attached.body.receipt.payloadDigest, digestCommand(['minarbot-agent-participation-v1', second, aid, participation]));
    const rules = { requestId: randomUUID(), expectedParticipationRevision: 1, rules: 'Changed rules' };
    const capabilities = { requestId: randomUUID(), expectedRevision: 2, capabilities: { v: 1, tools: ['ask_user', 'read_history'], resources: [] } };
    for (const [kind, change, fields, revision] of [['rules', rules, { rules: rules.rules }, 2], ['capabilities', capabilities, { capabilities: capabilities.capabilities }, 3]] as const) {
      const changed = await send(agentPath + '/' + kind, 'PUT', change);
      assert.equal(changed.status, 200, JSON.stringify(changed));
      assert.deepEqual(changed.body.data, { projectId: second, agentId: aid, ...fields, revision });
      assert.equal(changed.body.receipt.payloadDigest, digestCommand([`minarbot-agent-${kind}-v1`, second, aid, change]));
    }
    const collaborator = f.addAccount('user_member').accountId;
    db.prepare("INSERT INTO project_members VALUES(?,?,'member',1,0)").run(project, collaborator);
    const revocation = { requestId: randomUUID(), expectedRevision: 1, deleted: true };
    const memberPath = projectPath + '/members/' + collaborator;
    const revoked = await send(memberPath, 'PATCH', revocation);
    assert.equal(revoked.status, 200);
    assert.deepEqual(revoked.body.data, { membershipId: JSON.stringify([project, collaborator]), grantRevision: 2 });
    assert.deepEqual((await send(memberPath, 'PATCH', revocation)).body, revoked.body);
    const edit = { requestId: randomUUID(), expectedRevision: 1, name: 'Renamed' };
    assert.deepEqual((await send(projectPath, 'PATCH', edit)).body.data, { projectId: project, name: 'Renamed', deleted: false, revision: 2 });
    assert.equal((await send(projectPath, 'PATCH', { ...edit, requestId: randomUUID() })).body.error.code, 'stale_revision');
    assert.equal((await send('/api/projects', 'POST', { ...command, name: 'Changed' })).body.error.code, 'payload_conflict');
    await f.start();
    const reopened = await send('/api/projects', 'POST', command);
    assert.equal(reopened.status, 200);
    assert.deepEqual(reopened.body, created.body);
  } finally { await f.close(); }
});
