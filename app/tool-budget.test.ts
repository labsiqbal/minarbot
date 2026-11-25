import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { parseAgentTemplate } from './shared/agent-template.ts';
import { restoreAgentCreation } from './shared/agent-create.ts';

// The other cases and the server half of this one moved to
// crates/minarbot-e2e/tests/e2e/sa_tool_budget.rs. These assertions check the
// TypeScript modules app/shared/agent-template.ts and agent-create.ts
// (docs/test-ledger.md).
test('budget setting rejects malformed values, survives restart and template recovery', () => {
  const profile = { name: 'Budget', model: 'fixture', instructions: 'Help.' };
  for (const toolBudget of [null, true, '90', 9, 201, 10.5]) {
    assert.throws(() => parseAgentTemplate({ format: 'minarbot-agent', version: 1, ...profile, threadLimit: 2, toolBudget }));
  }
  const recovery = { id: randomUUID(), ...profile, role: '', backupModel: '', threadLimit: 2, providerAccount: '', toolBudget: 35 };
  assert.equal(restoreAgentCreation(JSON.stringify(recovery))!.toolBudget, 35);
});
