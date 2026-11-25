import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { restoreChatCommand } from './src/conversation-command.ts';

// The runtime cases moved to crates/minarbot-e2e/tests/e2e/fx_dispatch.rs (#89).
// This case tests the TypeScript module src/conversation-command.ts.
test('composer recovery validates dispatch mode and target', () => {
  const command = { id: randomUUID(), text: 'Guidance', dispatchMode: 'steer', targetTurn: randomUUID() };
  assert.deepEqual(restoreChatCommand(JSON.stringify(command), false), command);
  assert.throws(() => restoreChatCommand(JSON.stringify({ ...command, dispatchMode: 'auto' }), false));
  assert.throws(() => restoreChatCommand(JSON.stringify({ ...command, targetTurn: 'bad' }), false));
});
