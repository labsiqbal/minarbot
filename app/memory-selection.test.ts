import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { memoryCommand, memoryDraft } from './src/memory-command.ts';

// The server part of this case is
// crates/minarbot-e2e/tests/e2e/sa_memory_selection.rs. Only the assertions on
// the TypeScript module app/src/memory-command.ts stay here until app/ is
// removed (docs/test-ledger.md).
test('automatic memory selection is atomic, user-protected, scoped and durable across resume', () => {
  const first = { id: randomUUID(), entry: randomUUID(), revision: 0, text: 'AUTOMATIC_CONTEXT', essential: false, remove: false, automatic: true };
  const old: Record<string, unknown> = { id: randomUUID(), entry: randomUUID(), revision: 0, text: 'LEGACY', essential: false, remove: false };
  const legacyDraft = { entry: { id: first.entry, text: first.text, essential: 0, revision: 1 }, text: 'Draft', essential: false };
  assert.deepEqual(memoryDraft(legacyDraft), legacyDraft);
  const draft = { ...legacyDraft, entry: { ...legacyDraft.entry, selection_locked: 0 }, automatic: true };
  assert.deepEqual(memoryDraft(JSON.parse(JSON.stringify(draft))), draft);
  assert.deepEqual(memoryCommand(old), old);
  assert.throws(() => memoryCommand({ ...old, automatic: 'yes' })); assert.throws(() => memoryDraft({ ...draft, automatic: 1 }));
});
