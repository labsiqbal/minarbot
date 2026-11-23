import assert from 'node:assert/strict';
import { test } from 'node:test';
import { downloadBlob } from './src/hooks/downloadBlob.ts';

test('download attaches, clicks and removes link, then revokes URL after one second, including failed clicks', t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const saved = Object.getOwnPropertyDescriptor(globalThis, 'document');
  const events: string[] = [], blob = new Blob(['data']); let fail = false;
  const link = { href: '', download: '', click() { events.push('click'); if (fail) throw new Error('click failed'); }, remove() { events.push('remove'); } };
  Object.defineProperty(globalThis, 'document', { configurable: true, value: { createElement(tag: string) { assert.equal(tag, 'a'); return link; }, body: { append(node: unknown) { assert.equal(node, link); events.push('append'); } } } });
  t.mock.method(URL, 'createObjectURL', (value: Blob) => { assert.equal(value, blob); return 'blob:download'; });
  t.mock.method(URL, 'revokeObjectURL', (url: string) => { assert.equal(url, 'blob:download'); events.push('revoke'); });
  try {
    downloadBlob(blob, 'file.zip'); assert.equal(link.href, 'blob:download'); assert.equal(link.download, 'file.zip');
    assert.deepEqual(events, ['append', 'click', 'remove']);
    t.mock.timers.tick(999); assert.equal(events.length, 3); t.mock.timers.tick(1); assert.equal(events.at(-1), 'revoke');
    events.length = 0; fail = true; assert.throws(() => downloadBlob(blob, 'file.zip'), /click failed/);
    t.mock.timers.tick(1000); assert.deepEqual(events, ['append', 'click', 'remove', 'revoke']);
  } finally { if (saved) Object.defineProperty(globalThis, 'document', saved); else Reflect.deleteProperty(globalThis, 'document'); }
});
