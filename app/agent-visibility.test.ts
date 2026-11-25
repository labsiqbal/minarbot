import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { visibilityCommand } from './shared/agent-visibility.ts';

// The runtime case moved to crates/minarbot-e2e/tests/e2e/fx_agents.rs (#89).
// This case keeps the visibility command checks of the former "hidden agents
// finish active work ..." case, which test shared/agent-visibility.ts.
test('visibility command accepts an exact hide command and rejects malformed input', () => {
  const hide = { id: randomUUID(), revision: 0, hidden: true };
  assert.deepEqual(visibilityCommand(hide), hide);
  for (const bad of [null, [], {}, { ...hide, revision: -1 }, { ...hide, hidden: 1 }, { ...hide, extra: true }]) assert.throws(() => visibilityCommand(bad));
});
