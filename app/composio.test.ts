import assert from 'node:assert/strict';
import test from 'node:test';
import { randomUUID } from 'node:crypto';
import { restoreConnectedAppCommand } from './src/connected-app-command.ts';
const canary='COMPOSIO_CANARY_6vR9pX8m7Q4b';

// Runtime cases moved to crates/minarbot-e2e/tests/e2e/fx_composio.rs (#89).
// This case keeps the TypeScript command recovery checks of the former
// "Rust key replacement and removal" case, which test src/connected-app-command.ts.
test('connected app command recovery accepts grant and key commands and rejects malformed or secret-bearing input', () => {
  const grant = `/api/agents/${randomUUID()}/connected-apps/gmail`;
  assert(restoreConnectedAppCommand(JSON.stringify({ path: grant, body: { id: randomUUID(), revision: 0, enabled: true } })));
  assert.throws(() => restoreConnectedAppCommand('{bad'));
  assert.throws(() => restoreConnectedAppCommand(JSON.stringify({ path: '/api/agents', body: { id: randomUUID() } })));
  for (const path of ['/api/connected-apps/key', '/api/connected-apps/key/remove']) {
    const command = { path, body: { id: randomUUID(), revision: 3 } };
    assert.deepEqual(restoreConnectedAppCommand(JSON.stringify(command)), command);
    assert.throws(() => restoreConnectedAppCommand(JSON.stringify({ ...command, body: { ...command.body, key: canary } })), /invalid/);
  }
});
