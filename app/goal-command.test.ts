import { strict as assert } from 'node:assert';
import { test } from 'node:test';
import { parseModeDraft, restoreModeCommand } from './src/goal-command.ts';

test('composer recognizes exact goal and loop commands with multiline prompts', () => {
  assert.deepEqual(parseModeDraft('/goal Ship\nRun checks'), { mode: 'goal', text: 'Ship\nRun checks' });
  assert.deepEqual(parseModeDraft('/loop 2h Check status'), { mode: 'loop', text: 'Check status', intervalSeconds: 7200 });
  assert.equal(parseModeDraft('/goalpost'), null);
  assert.throws(() => parseModeDraft('/loop 0m Check status'), /interval/);
  assert.throws(() => parseModeDraft('/loop every day Check status'), /interval/);
});

test('goal retries retain the original ID, target, files and limits', () => {
  const id = '2de63528-fe50-49ed-b435-b7d653c14b99';
  const saved = { mode: 'goal', body: { id, target: 'Ship', maxTurns: 4, maxMinutes: 120, maxTokens: 2000, files: [id] } };
  assert.deepEqual(restoreModeCommand(JSON.stringify(saved)), saved);
  assert.throws(() => restoreModeCommand(JSON.stringify({ ...saved, body: { ...saved.body, maxTurns: 0 } })), /Invalid/);
  assert.throws(() => restoreModeCommand(JSON.stringify({ ...saved, body: { ...saved.body, id: 'other' } })), /Invalid/);
});
