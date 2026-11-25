// Server cases moved to crates/minarbot-e2e/tests/e2e/questions.rs (#89).
// This TypeScript-module case leaves with app/src in #89 stage 5.
import { parseQuestionAnswer } from './src/question-answer.ts';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';

test('answer recovery rejects malformed storage and preserves exact retry identity', () => {
  const valid = { id: randomUUID(), text: 'Yes', acknowledgeUncertain: false };
  assert.deepEqual(parseQuestionAnswer(JSON.parse(JSON.stringify(valid)), ['Yes', 'No']), valid);
  for (const value of [false, 0, '', null, [], {}, { ...valid, id: 1 }, { ...valid, text: 'Other' }, { ...valid, acknowledgeUncertain: 'yes' }, { ...valid, extra: true }]) assert.throws(() => parseQuestionAnswer(value, ['Yes', 'No']));
});
