import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { startApp } from './test-bridge.ts';

test('core serves exact static bytes, strict asset CSP, HTML 404 and local boundary errors', async () => {
  const root = mkdtempSync(join(tmpdir(), 'minarbot-core-static-'));
  const app = await startApp({ database: join(root, 'db.sqlite') });
  try {
    const page = await fetch(app.origin); const html = await page.text();
    assert.equal(page.status, 200); assert.equal(page.headers.get('referrer-policy'), 'no-referrer');
    const asset = html.match(/(?:src|href)="(\/assets\/[^" ]+)"/)?.[1]; assert(asset);
    const response = await fetch(app.origin + asset);
    const expected = await import('node:fs/promises').then(fs => fs.readFile(new URL('./dist' + asset, import.meta.url)));
    assert.deepEqual(Buffer.from(await response.arrayBuffer()), expected);
    assert.equal(response.headers.get('cache-control'), 'public, max-age=31536000, immutable');
    assert(!response.headers.get('content-security-policy')!.includes('nonce-'));
    for (const path of ['/missing/path', '/rooms/deleted', '/settings/nope']) {
      const missing = await fetch(app.origin + path); assert.equal(missing.status, 404); assert.match(missing.headers.get('content-type')!, /text\/html/); await missing.text();
    }
    const denied = await fetch(app.origin + '/api/agents', { headers: { 'Sec-Fetch-Site': 'cross-site' } });
    assert.equal(denied.status, 403); assert.deepEqual(await denied.json(), { error: 'Access is only allowed through the local Minarbot address.' });
    const cookies = await fetch(app.origin + '/api/agents', { headers: { Cookie: '__session=untrusted' } });
    assert.equal(cookies.headers.get('set-cookie'), null); assert.equal(cookies.status, 200); await cookies.text();
  } finally { await app.close(); rmSync(root, { recursive: true, force: true }); }
});

test('Rust test bridge rejects unported options before starting a process', async () => {
  const root = mkdtempSync(join(tmpdir(), 'minarbot-core-unported-'));
  try {
    await assert.rejects(startApp({ database: join(root, 'db.sqlite'), ...{ unported: true } }), /not ported: option unported/);
    await assert.rejects(startApp({ database: join(root, 'db.sqlite'), hubLink: { origin: 'https://hub.example.test', hubSigningPublicKey: 'fixture', ...{ unported: true } } }), /not ported: option hubLink.unported/);
    for (const concurrency of [NaN, Infinity, 0, 33]) await assert.rejects(startApp({ database: join(root, `db-${String(concurrency)}.sqlite`), concurrency }), /MINARBOT_CONCURRENCY must be 1\.\.32/);
    assert(!existsSync(join(root, 'db.sqlite')));
  }
  finally { rmSync(root, { recursive: true, force: true }); }
});
