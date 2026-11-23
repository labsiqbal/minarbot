import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import { startApp } from '../../test-bridge.ts';

const scratch = await mkdtemp(join(tmpdir(), 'minarbot-needs-you-'));
const agent = '7e270641-2a83-4602-bde2-7dd162ee025d';
const turn = '3b869c34-a857-4447-a59c-69cb9eb78f7e';
let app, browser;
try {
  app = await startApp({ database: join(scratch, 'state.sqlite'),
    dist: fileURLToPath(new URL('../../dist', import.meta.url)), defaultModel: 'fixture' });
  browser = await chromium.launch();
  const page = await browser.newPage();
  await page.route('**/api/sidebar/stream', route => route.fulfill({ status: 204 }));
  await page.addInitScript(() => {
    const tags = window.fixtureNotifications = [];
    window.Notification = class {
      static permission = 'granted';
      constructor(_title, { tag }) { tags.push(tag); }
    };
    Object.defineProperty(document, 'hidden', { get: () => true });
    window.EventSource = class extends EventSource {
      constructor(url, options) {
        super(url, options);
        if (url === '/api/sidebar/stream') window.fixtureSidebar = this;
      }
    };
  });
  const send = (pending = [], finished = []) => page.evaluate(snapshot => {
    window.fixtureSidebar.dispatchEvent(new MessageEvent('message', { data: JSON.stringify(snapshot) }));
    return window.fixtureNotifications;
  }, { pending, finished });
  const first = { agent, turn, focus: turn, kind: 'question', request: 'request-1' };
  const second = { ...first, request: 'request-2' };
  const finished = { agent, turn, focus: turn, status: 'done' };
  const firstTag = `minarbot:question:${turn}:request-1`;
  const secondTag = `minarbot:question:${turn}:request-2`;
  const ready = () => page.waitForFunction(() => typeof window.fixtureSidebar?.onmessage === 'function');

  await page.goto(app.origin + '/settings/general');
  await ready();
  assert.deepEqual(await send(), []);
  assert.deepEqual(await send([first]), [firstTag]);
  assert.deepEqual(await send(), [firstTag]);
  assert.deepEqual(await send([first]), [firstTag]);
  assert.deepEqual(await send([second]), [firstTag, secondTag]);
  assert.deepEqual(await send([], [finished]), [firstTag, secondTag, `minarbot:finished:${turn}`]);

  await page.reload();
  await ready();
  assert.deepEqual(await send(), []);
  assert.deepEqual(await send([first, second], [finished]), []);
  console.log('Leptos Needs you notifications retain distinct delivery claims across reload.');
} finally {
  await browser?.close();
  await app?.close();
  await rm(scratch, { recursive: true });
}
