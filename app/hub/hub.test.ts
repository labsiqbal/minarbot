import test from 'node:test';
import assert from 'node:assert/strict';
import { readdirSync, readFileSync } from 'node:fs';

test('hosted Hub has no Node runtime modules or executable entrypoint', () => {
  const files = readdirSync(new URL('.', import.meta.url), { withFileTypes: true });
  for (const entry of files) assert(entry.isFile() && entry.name.endsWith('.test.ts'), entry.name);
  const manifest = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8'));
  assert.equal(manifest.scripts.hub, undefined);
});
