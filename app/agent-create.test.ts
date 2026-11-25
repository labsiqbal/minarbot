// Server cases moved to crates/minarbot-e2e/tests/e2e/agent_create.rs (#89).
// Kept: two TypeScript-module cases, which leave with app/ in #89 stage 5.
import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { restoreAgentCreation } from './shared/agent-create.ts';

test('creation recovery keeps exact command and rejects malformed authority', () => {
  const command = { id: randomUUID(), name: 'Agent baru', model: 'fixture', backupModel: '', instructions: 'Help with research', threadLimit: 2, providerAccount: '', setup: true };
  assert.deepEqual(restoreAgentCreation(JSON.stringify(command)), command); assert.equal(restoreAgentCreation(null), null);
  for (const value of [{ ...command, permissions: 'admin' }, { ...command, setup: 1 }, { ...command, id: 'bad' }, { ...command, threadLimit: 0 }, { ...command, instructions: '' }, { ...command, providerAccount: null }, { ...command, backupModel: 'fixture' }]) assert.throws(() => restoreAgentCreation(JSON.stringify(value)));
  assert.throws(() => restoreAgentCreation('x'.repeat(64001)));
});

import { parseCanonicalAgentCreation, restoreCanonicalAgentCreation } from './test-support/canonical-agent-creation.ts';
import { canonicalHttp } from './test-support/canonical-http.ts';

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
  return { ...f, member, foreign, home, project, command, enroll };
}

test('canonical draft carries project/home authority and rejects legacy or malformed provenance', async () => {
  const f = canonicalFixture();
  try {
    const command = f.command(); assert.deepEqual(restoreCanonicalAgentCreation(JSON.stringify(command)), command);
    assert.equal(restoreCanonicalAgentCreation(null), null);
    for (const change of [{ homeDeviceId: 'bad' }, { requestId: randomUUID() }, { expectedProjectRevision: 0 }, { expectedHomeDeviceRevision: 1.5 }, { providerBinding: {} }, { providerAccount: '' }, { setup: 'yes' }, { toolBudget: 9 }, { toolBudget: null }, { toolBudget: 201 }]) assert.throws(() => parseCanonicalAgentCreation({ ...command, ...change }));
    assert.throws(() => restoreCanonicalAgentCreation(JSON.stringify({ id: randomUUID(), name: 'Old draft' })));
  } finally { await f.close(); }
});
