import assert from 'node:assert/strict';
import test from 'node:test';
import { randomUUID } from 'node:crypto';
import { parseDraft, parseJoin, parseGrant } from '../src/hub/client.ts';

test('pilot drafts retain exact command and requester selections through renewal, reject corrupt recovery', () => {
  const pending = { requestId: randomUUID(), sectionId: randomUUID(), text: 'Keep draft', mentions: [randomUUID()] };
  const value = { text: pending.text, mentions: pending.mentions, pending };
  assert.deepEqual(parseDraft(JSON.stringify(value)), value);
  const join = { nodeId: randomUUID(), agentId: randomUUID(), name: 'Agent', localPolicyDigest: 'a'.repeat(64), maxCostMicros: 100, providerMaxCostMicros: 10 };
  assert.deepEqual(parseJoin(JSON.stringify(join)), join);
  for (const bad of [{ ...join, name: {} }, { ...join, providerMaxCostMicros: 101 }, null]) assert.throws(() => parseJoin(JSON.stringify(bad)));
  const grant = { projectId: randomUUID(), participationId: randomUUID(), agentId: join.agentId, localAgentId: join.agentId, revision: 1, providerMaxCostMicros: 10, policy: { sectionIds: [randomUUID()], invocation: { mode: 'owner' }, scopes: ['read','invoke'], localPolicyDigest: join.localPolicyDigest, payerBindingRef: join.agentId, maxCostMicros: 100 } };
  assert.deepEqual(parseGrant(JSON.stringify(grant)), grant);
  assert.throws(() => parseGrant(JSON.stringify({ ...grant, policy: { ...grant.policy, invocation: { mode: 'members' } } })));
  assert.throws(() => parseGrant(JSON.stringify({ ...grant, providerMaxCostMicros: 101 })));

  assert.deepEqual(parseDraft(null), { text: '', mentions: [], pending: null });
  for (const raw of ['broken', '{}', '{"text":"x","mentions":[],"pending":{}}', JSON.stringify({ ...value, mentions: ['not-an-id'] })]) assert.throws(() => parseDraft(raw));
});
