import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { memoryCommand, memoryDraft } from './src/memory-command.ts';

// The server part of this case is crates/minarbot-e2e/tests/e2e/sa_memory.rs.
// Only the assertions on the TypeScript module app/src/memory-command.ts stay
// here until app/ is removed (docs/test-ledger.md).
test('memory CRUD, exact retry, isolated provider search and bounded context survive restart', () => {
  const original = { id: randomUUID(), entry: randomUUID(), revision: 0, text: 'PRIVATE_A', essential: true, remove: false };
  const draft = { entry: { id: original.entry, revision: 1, text: original.text, essential: 1 }, text: 'Unsaved edit', essential: false };
  assert.deepEqual(memoryDraft(JSON.parse(JSON.stringify(draft))), draft);
  for (const bad of [null, {}, { ...draft, entry: { ...draft.entry, revision: 0 } }, { ...draft, text: 123 }]) assert.throws(() => memoryDraft(bad));
  for (const bad of [null, [], {}, { ...original, text: '' }, { ...original, revision: -1 }, { ...original, essential: 1 }]) assert.throws(() => memoryCommand(bad));
});
