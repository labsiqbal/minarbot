import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { startApp } from './test-bridge.ts';

test('web routes recover from a missing build and ignore the retired client query', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'minarbot-web-recovery-'));
  const dist = join(directory, 'dist');
  const start = '/_leptos/assets/appearance-start.0123456789abcdef.js';
  const route = '/settings/appearance?ui=react&from=%2Froutines';
  const app = await startApp({ database: join(directory, 'state.sqlite'), dist });
  try {
    await mkdir(join(dist, '_leptos/assets'), { recursive: true });
    await writeFile(join(dist, 'index.html'), '<title>Retired build</title>');
    const document = join(dist, '_leptos/appearance.html');
    const recover = async () => {
      const response = await fetch(app.origin + route);
      assert.equal(response.status, 503);
      assert.equal(response.headers.get('cache-control'), 'no-store');
      assert.match(response.headers.get('content-type') ?? '', /^text\/html/);
      assert.match(await response.text(), /Minarbot web client unavailable.*cargo xtask leptos/s);
    };
    await recover();
    await writeFile(document, '<title>Incomplete build</title>');
    await recover();
    await writeFile(document, `<title>__SETTINGS_TITLE__</title><script type="module" src="${start}"></script><div id="leptos-root" data-return-to="__RETURN_TO__"></div>`);
    await recover();
    await writeFile(join(dist, start.slice(1)), 'export {};');
    const page = await fetch(app.origin + route);
    assert.equal(page.status, 200);
    assert.match(await page.text(), /<title>Appearance<\/title>.*data-return-to="\/routines"/s);
    for (const path of ['/index.html', '/assets/missing.js', '/_leptos/appearance.html', '/settings/missing', '/api/missing']) {
      assert.equal((await fetch(app.origin + path)).status, 404, path);
    }
  } finally {
    await app.close();
    await rm(directory, { recursive: true });
  }
});
