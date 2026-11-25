// Server cases moved to crates/minarbot-e2e/tests/e2e/threads.rs (#89).
// This TypeScript-module case leaves with app/src in #89 stage 5.
import { restoreThreadCommand } from './src/thread-command.ts';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';

test('conversation recovery retains exact request and refuses corrupt or foreign routes', () => {
  const agent = randomUUID(), thread = randomUUID();
  const command = { path: `/api/threads/${thread}/edit`, data: { id: randomUUID(), revision: 2, name: 'Saved name', archived: true } };
  assert.deepEqual(restoreThreadCommand(JSON.stringify(command), agent), command); assert.equal(restoreThreadCommand(null, agent), null);
  for (const raw of ['{', 'null', '[]', JSON.stringify({ ...command, path: '/api/agents/other/threads' }), JSON.stringify({ ...command, data: { ...command.data, revision: 0 } }), JSON.stringify({ ...command, data: { ...command.data, name: '' } }), JSON.stringify({ ...command, data: { ...command.data, extra: true } })]) assert.throws(() => restoreThreadCommand(raw, agent));
});
