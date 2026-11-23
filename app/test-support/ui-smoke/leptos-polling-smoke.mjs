import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as wait } from 'node:timers/promises';
import { chromium } from 'playwright-core';
import { startRustServer } from '../../rust-test-server.ts';

const directory = await mkdtemp(join(tmpdir(), 'minarbot-leptos-poll-'));
let app, browser;
try {
  app = await startRustServer({
    appearanceClient: 'leptos',
    database: join(directory, 'state.sqlite'),
    dist: resolve(import.meta.dirname, '../../dist'),
  });
  browser = await chromium.launch();
  const page = await browser.newPage();
  const start = Date.now();
  await page.clock.install({ time: start });
  await page.clock.pauseAt(start + 1000);
  await page.addInitScript(() => {
    let hidden = false;
    Object.defineProperty(document, 'hidden', { get: () => hidden });
    window.fixtureHidden = value => {
      hidden = value;
      document.dispatchEvent(new Event('visibilitychange'));
    };
  });

  let reads = 0, failing = true;
  await page.route('**/api/usage', async route => {
    reads++;
    if (failing) {
      await route.fulfill({ status: 503, json: { error: 'Fixture poll failure ' + reads } });
    } else {
      await route.fulfill({ json: { transcription: {}, summary: {}, calls: [], next_before: null } });
    }
  });
  async function rendered(text) {
    const deadline = performance.now() + 10000;
    while (!await page.evaluate(value => document.body.textContent.includes(value), text)) {
      assert(performance.now() < deadline, 'Missing result from polling: ' + text);
      await wait(20);
    }
  }
  await page.goto(app.origin + '/settings/usage', { waitUntil: 'commit' });
  await rendered('Fixture poll failure 1');
  for (const delay of [10000, 20000, 40000, 60000, 60000]) {
    const previous = reads;
    await page.clock.runFor(delay - 1);
    assert.equal(reads, previous, 'Read before its backoff deadline');
    await page.clock.runFor(1);
    await rendered('Fixture poll failure ' + (previous + 1));
    assert.equal(reads, previous + 1);
  }

  failing = false;
  await page.clock.runFor(60000);
  await rendered('No calls');
  const recovered = reads;
  const normal = page.waitForResponse(response => new URL(response.url()).pathname === '/api/usage');
  await page.clock.runFor(5000);
  await normal;
  assert.equal(reads, recovered + 1, 'Successful read resets cadence');

  await page.evaluate(() => window.fixtureHidden(true));
  const hidden = reads;
  await page.clock.runFor(120000);
  assert.equal(reads, hidden, 'Hidden tab must issue no polling requests');
  const visible = page.waitForResponse(response => new URL(response.url()).pathname === '/api/usage');
  await page.evaluate(() => window.fixtureHidden(false));
  await visible;
  assert.equal(reads, hidden + 1, 'Visible tab refreshes once');
  console.log('PASS Leptos polling pauses and resumes, doubles failures to 60s, and resets after success');
} finally {
  await browser?.close();
  await app?.close();
  await rm(directory, { recursive: true });
}
