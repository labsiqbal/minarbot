import assert from 'node:assert/strict';
import { test } from 'node:test';
import { uploadLimitError } from './src/attachments.ts';

test('upload boundaries keep four attachments and 10 MiB per file', () => {
  assert.equal(uploadLimitError([{ size: 10 * 1024 * 1024 }], 3), '');
  assert.match(uploadLimitError([{ size: 1 }], 4), /four/);
  assert.match(uploadLimitError([{ size: 10 * 1024 * 1024 + 1 }], 0), /10 MiB/);
});
