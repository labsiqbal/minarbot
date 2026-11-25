import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { newestPartial, parseTurnSnapshot, type StreamPartial } from './shared/turn-stream.ts';

test('snapshot merge rejects stale calls, duplicate reconnects and late polling regressions', () => {
  const partial: StreamPartial = { seq: 1, text: 'Hello', model: 'fixture', status: 'pending' };
  assert.deepEqual(newestPartial(partial, { ...partial, text: 'He' }), partial);
  assert.deepEqual(newestPartial(partial, partial), partial);
  const resumed = { ...partial, seq: 2, text: 'New' };
  assert.deepEqual(newestPartial(resumed, partial), resumed);
  const received = { ...partial, status: 'received' };
  assert.deepEqual(newestPartial(received, partial), received);
  assert.deepEqual(newestPartial(partial, received), received);
  const preTool = { ...partial, segment: 'before', revision: 2 };
  const afterTool = { ...partial, segment: 'after', text: 'OK', revision: 3 };
  assert.deepEqual(newestPartial(preTool, afterTool), afterTool);
  assert.deepEqual(newestPartial(afterTool, preTool), afterTool);
  const corrected = { ...afterTool, text: 'Y', revision: 4, final: true };
  assert.deepEqual(newestPartial(afterTool, corrected), corrected);
  assert.deepEqual(newestPartial(corrected, afterTool), corrected);
  assert.deepEqual(newestPartial(preTool, { ...afterTool, seq: 2, text: '' }), { ...afterTool, seq: 2, text: '' });
  for (const value of [null, {}, { id: randomUUID(), partial: { ...partial, text: 42 } }, { id: randomUUID(), partial: { ...partial, seq: -1 } }]) assert.equal(parseTurnSnapshot(value), null);
});
