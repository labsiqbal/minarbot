import { test } from 'node:test';
import assert from 'node:assert/strict';
import { resultTarget } from './src/navigation.ts';
const agent = '00000000-0000-4000-8000-000000000001';
const other = '00000000-0000-4000-8000-000000000002';

test('result targets retain safe file, URL and Computer destinations', () => {
  for (const result of [
    { view: 'file', path: `/api/agents/${agent}/panel/files/${other}`, name: 'Report.html' },
    { view: 'computer', agent, path: 'https://example.test/', name: 'Browser result' },
    { view: 'url', path: 'https://example.test/?a=1&b=2#part', name: 'Report' },
  ]) {
    assert.deepEqual(resultTarget(result), result);
  }
  for (const result of [
    { view: 'file', path: '/api/backup', name: 'Backup' },
    { view: 'url', path: 'javascript:alert(1)', name: 'Bad' },
    { view: 'url', path: 'https://user:pass@example.test/', name: 'Bad' },
  ]) {
    assert.equal(resultTarget(result), null);
  }
});
