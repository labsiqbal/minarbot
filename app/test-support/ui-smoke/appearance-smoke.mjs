import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createInterface } from 'node:readline';
import { setTimeout as wait } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const dist = join(root, 'app/dist');
const binary = resolve(process.env.MINARBOT_RUST_BINARY ?? join(root, 'target/debug/minarbot-server'));
const screenshots = process.env.MINARBOT_APPEARANCE_SCREENSHOTS;
const samples = Number(process.env.MINARBOT_APPEARANCE_BENCH ?? 0);
const route = '/settings/appearance';
const key = 'minarbot-presentation-v1';
assert(Number.isSafeInteger(samples) && samples >= 0);
const wasmName = (await readdir(join(dist, '_leptos/assets'))).find(name => name.endsWith('.wasm'));
assert.match(wasmName ?? '', /^appearance\.[a-f0-9]{16}\.wasm$/);
const wasm = '/_leptos/assets/' + wasmName;
const data = await mkdtemp(join(tmpdir(), 'minarbot-appearance-'));
let server, browser;

async function startup(client, port = 0) {
  server = spawn(binary, ['--test-config'], {
    env: { ...process.env, MINARBOT_TEST_SERVER: 'rust' },
    stdio: ['pipe', 'pipe', 'inherit'],
  });
  const output = createInterface({ input: server.stdout });
  const ready = new Promise((resolve, reject) => {
    output.on('line', line => {
      if (line.startsWith('Minarbot: ')) resolve(line.slice('Minarbot: '.length));
    });
    server.once('error', reject);
    server.stdin.once('error', reject);
    server.once('exit', code => reject(new Error('Minarbot stopped before readiness (' + code + ')')));
  });
  server.stdin.end(JSON.stringify({
    database: join(data, 'state.sqlite'), dist, port, appearanceClient: client,
  }));
  return Promise.race([ready, wait(15000, undefined, { ref: false }).then(() => {
    throw new Error('Minarbot did not report readiness');
  })]);
}

async function stop() {
  if (server?.pid && server.exitCode === null && server.signalCode === null) {
    const exited = once(server, 'exit');
    server.kill();
    if (!await Promise.race([exited, wait(2500, false, { ref: false })])) {
      server.kill('SIGKILL');
      await exited;
    }
  }
  server = undefined;
}

function panel(page) {
  return page.getByRole('dialog', { name: 'Settings', exact: true });
}
function button(page, name) {
  return panel(page).getByRole('button', { name, exact: true });
}
function radio(page, name) {
  return panel(page).getByRole('radio', { name, exact: true });
}
async function ready(page, client) {
  const heading = panel(page).getByRole('heading', { name: 'Appearance', exact: true });
  await heading.waitFor();
  await page.waitForFunction(() => document.activeElement?.textContent === 'Appearance');
  assert.equal(await page.locator('#appearance-dialog[open]').count(), client === 'leptos' ? 1 : 0);
  return heading;
}
async function open(page, origin, client, path = route) {
  const response = await page.goto(origin + path, { waitUntil: 'commit' });
  assert.equal(response.status(), 200);
  assert.equal((await response.headerValue('content-security-policy')).includes("'wasm-unsafe-eval'"), client === 'leptos');
  return ready(page, client);
}

async function exercise(page, origin, client) {
  const errors = [];
  const violations = [];
  const recordError = error => errors.push(error.message);
  const recordConsole = message => {
    if (/content security policy|webassembly.*failed/i.test(message.text())) violations.push(message.text());
  };
  page.on('pageerror', recordError);
  page.on('console', recordConsole);
  try {
    await open(page, origin, client);
    assert.equal(await panel(page).getByRole('group', { name: 'Color theme', exact: true }).getByRole('button').count(), 6);
    assert.equal(await panel(page).getByRole('radiogroup', { name: 'Mode', exact: true }).getByRole('radio').count(), 3);
    assert.equal(await panel(page).getByRole('radiogroup', { name: 'Text size', exact: true }).getByRole('radio').count(), 4);
    await page.keyboard.press('Tab');
    assert.equal(await page.evaluate(() => document.activeElement?.getAttribute('aria-pressed')), await button(page, 'Minarbot').getAttribute('aria-pressed'));
    assert.equal(await button(page, 'Minarbot').evaluate(element => element === document.activeElement && getComputedStyle(element).outlineStyle === 'solid'), true);
    await button(page, 'GitHub').focus();
    await page.keyboard.press('Space');
    assert.equal(await button(page, 'GitHub').getAttribute('aria-pressed'), 'true');
    await radio(page, 'Light').focus();
    await page.keyboard.press('Space');
    assert.equal(await radio(page, 'Light').getAttribute('aria-checked'), 'true');
    await radio(page, 'Light').press('ArrowRight');
    assert.equal(await radio(page, 'Dark').evaluate(element => element === document.activeElement && element.getAttribute('aria-checked') === 'true'), true);
    await radio(page, 'Large').focus();
    await page.keyboard.press('Space');
    assert.equal(await radio(page, 'Large').getAttribute('aria-checked'), 'true');
    assert.equal(await panel(page).getByRole('status').filter({ hasText: 'Appearance saved in this browser.' }).count(), 1);
    await page.keyboard.press('/');
    const search = panel(page).getByRole('searchbox', { name: 'Search settings' });
    assert.equal(await search.evaluate(element => element === document.activeElement), true);
    await search.fill('pref appear');
    await search.press('ArrowDown');
    assert.equal(await button(page, 'Appearance').evaluate(element => element === document.activeElement), true);
    await page.keyboard.press('Enter');
    assert.equal(await panel(page).getByRole('heading', { name: 'Appearance' }).evaluate(element => element === document.activeElement), true);
    await page.keyboard.press('/');
    await page.keyboard.press('Escape');
    assert.equal(await search.inputValue(), '');
    assert.equal(await panel(page).isVisible(), true);

    await button(page, 'Minarbot').click();
    await radio(page, 'Default').click();
    if (screenshots) {
      for (const [label, viewport] of [['desktop', { width: 1440, height: 900 }], ['narrow', { width: 430, height: 850 }]]) {
        await page.setViewportSize(viewport);
        for (const mode of ['Dark', 'Light']) {
          await radio(page, mode).click();
          await page.evaluate(() => document.fonts.ready.then(() => document.activeElement?.blur()));
          await page.screenshot({ path: join(screenshots, [client, label, mode.toLowerCase()].join('-') + '.png'), animations: 'disabled' });
          assert.equal(await page.locator('.app-settings__main').evaluate(element => element.scrollWidth === element.clientWidth), true);
        }
      }
      await page.setViewportSize({ width: 1440, height: 900 });
    }
    await button(page, 'GitHub').click();
    await radio(page, 'Large').click();
    await radio(page, 'Light').click();
    const saved = await page.evaluate(key => JSON.parse(localStorage.getItem(key)), key);
    assert.deepEqual(saved, { version: 1, colorTheme: 'github', theme: 'light', textSize: 'large' });
    const tokens = await page.locator('html').evaluate(element => {
      const style = getComputedStyle(element);
      return ['--color-bg-chrome', '--color-text-secondary', '--color-border-focus', 'font-size']
        .map(name => style.getPropertyValue(name).trim());
    });
    await page.reload({ waitUntil: 'commit' });
    await ready(page, client);
    assert.equal(await radio(page, 'Light').getAttribute('aria-checked'), 'true');
    assert.equal(await radio(page, 'Large').getAttribute('aria-checked'), 'true');
    assert.equal(await button(page, 'GitHub').getAttribute('aria-pressed'), 'true');
    assert.deepEqual(errors, []);
    assert.deepEqual(violations, []);
    return tokens;
  } finally {
    page.off('pageerror', recordError);
    page.off('console', recordConsole);
  }
}

async function rollout(page, origin) {
  await page.goto(origin + '/settings/general?from=%2Froutines');
  const url = origin + route + '?from=%2Froutines';
  await button(page, 'Appearance').click();
  await page.waitForURL(url);
  await ready(page, 'leptos');
  assert.equal(await page.locator('#root').count(), 0);
  await page.goBack({ waitUntil: 'commit' });
  await panel(page).getByRole('heading', { name: 'General' }).waitFor();
  await page.goForward({ waitUntil: 'commit' });
  await ready(page, 'leptos');
  await page.keyboard.press('Escape');
  await page.waitForURL(origin + '/routines');
  const trigger = page.getByRole('button', { name: 'Settings', exact: true });
  await trigger.waitFor();
  await page.getByRole('heading', { name: 'Routines', exact: true }).waitFor();
  await page.waitForFunction(() => document.activeElement?.classList.contains('sidebar-settings'));
  await page.setViewportSize({ width: 430, height: 850 });
  await open(page, origin, 'leptos', url.slice(origin.length));
  await button(page, 'Close settings').press('Enter');
  await page.waitForURL(origin + '/routines');
  await page.getByRole('heading', { name: 'Routines', exact: true }).waitFor();
  await page.waitForFunction(() => document.activeElement?.classList.contains('sidebar-reveal'));
  await page.setViewportSize({ width: 1440, height: 900 });
  await open(page, origin, 'leptos', route + '?from=%2Fworkflows');
  await button(page, 'Close settings').press('Enter');
  await page.waitForURL(origin + '/workflows');
  await page.getByRole('heading', { name: 'Workflows', exact: true }).waitFor();
  await page.waitForFunction(() => document.activeElement?.classList.contains('sidebar-settings'));
  await open(page, origin, 'leptos', url.slice(origin.length));
  const [response] = await Promise.all([
    page.waitForResponse(response => response.url().startsWith(origin + '/settings/vault')),
    button(page, 'Vault').click(),
  ]);
  assert.equal(response.status(), 200);
}

async function faults(context, origin) {
  const page = await context.newPage();
  try {
    let requests = 0;
    const intercept = request => ++requests === 1 ? request.abort('failed') : request.continue();
    await page.route('**/*.wasm', intercept);
    await open(page, origin, 'leptos');
    assert.equal(requests, 2);
    await page.unroute('**/*.wasm', intercept);
    await page.route('**/*.wasm', request => {
      requests += 1;
      return request.fulfill({ status: 503, body: 'Intentional fixture failure' });
    });
    requests = 0;
    await page.goto(origin + route + '?from=%2Fworkflows');
    await page.getByRole('status').filter({ hasText: 'Appearance could not load' }).waitFor();
    assert.equal(requests, 2);
    const fallback = page.getByRole('link', { name: 'Open standard Appearance' });
    if (screenshots) await page.screenshot({ path: join(screenshots, 'failed-wasm-fallback.png') });
    await fallback.focus();
    await page.keyboard.press('Enter');
    await ready(page, 'react');
    assert.equal(new URL(page.url()).searchParams.get('from'), '/workflows');
    assert.equal(new URL(page.url()).searchParams.get('ui'), 'react');
    assert.equal(await button(page, 'GitHub').getAttribute('aria-pressed'), 'true');
  } finally {
    await page.close();
  }
}

async function crossTab(context, page, origin) {
  await open(page, origin, 'leptos');
  const peer = await context.newPage();
  try {
    await open(peer, origin, 'react', route + '?ui=react');
    await radio(peer, 'Dark').click();
    await page.waitForFunction(() => document.documentElement.dataset.theme === 'dark');
    assert.equal(await panel(page).getByRole('status').filter({ hasText: 'Appearance updated from another tab.' }).count(), 1);
    await radio(page, 'Light').click();
    await peer.waitForFunction(() => document.documentElement.dataset.theme === 'light');
  } finally {
    await peer.close();
  }
}

async function headers(origin) {
  const response = await fetch(origin + wasm, { headers: { 'accept-encoding': 'gzip' } });
  assert.equal(response.status, 200);
  assert.equal(response.headers.get('content-type'), 'application/wasm');
  assert.equal(response.headers.get('content-encoding'), 'gzip');
  assert.equal(response.headers.get('cache-control'), 'public, max-age=31536000, immutable');
  assert.match(response.headers.get('vary'), /accept-encoding/i);
  assert.equal(response.headers.get('content-security-policy').includes('wasm-unsafe-eval'), false);
  assert.equal(Buffer.from(await response.arrayBuffer()).subarray(0, 4).toString('hex'), '0061736d');
  const raw = await fetch(origin + wasm, { headers: { 'accept-encoding': 'gzip;q=0' } });
  assert.equal(raw.headers.has('content-encoding'), false);
  await raw.arrayBuffer();
  for (const path of ['/_leptos/appearance.html', wasm + '.gz', '/_leptos/assets/%2e%2e%2fappearance.html']) {
    const blocked = await fetch(origin + path);
    assert.equal(blocked.status, 404);
    assert.equal(blocked.headers.get('content-security-policy').includes('wasm-unsafe-eval'), false);
  }
  const other = await fetch(origin + '/settings/general');
  assert.equal(other.headers.get('content-security-policy').includes('wasm-unsafe-eval'), false);
  const outside = await fetch(origin + route + '?from=' + encodeURIComponent('//example.invalid'));
  assert.match(await outside.text(), /data-return-to="\/"/);
}

function stats(list) {
  list.sort((left, right) => left - right);
  const mid = Math.floor(list.length / 2);
  return {
    median: Math.round((list[mid] + list[Math.floor((list.length - 1) / 2)]) / 2 * 10) / 10,
    p95: Math.round(list[Math.ceil(list.length * 0.95) - 1] * 10) / 10,
  };
}
async function sample(page, fromClick = false) {
  return page.locator('body').evaluate((_, fromClick) => {
    const start = fromClick
      ? Number(sessionStorage.getItem('appearance-bench-start')) - performance.timeOrigin
      : 0;
    return {
      milliseconds: performance.now() - start,
      transferredBytes: performance.getEntries()
        .filter(entry => entry.startTime >= start && new URL(entry.name, location.href).origin === location.origin)
        .reduce((total, entry) => total + (entry.transferSize ?? 0), 0),
    };
  }, fromClick);
}
async function benchmark(origin, client) {
  const cold = [], warmReload = [], warmNavigation = [];
  for (let index = 0; index < samples; index += 1) {
    const context = await browser.newContext();
    try {
      const page = await context.newPage();
      await open(page, origin, client);
      cold.push(await sample(page));
      await page.reload({ waitUntil: 'commit' });
      await ready(page, client);
      warmReload.push(await sample(page));
      await page.goto(origin + '/settings/general', { waitUntil: 'commit' });
      await panel(page).getByRole('heading', { name: 'General' }).waitFor();
      await button(page, 'Appearance').evaluate(element => {
        setTimeout(() => {
          sessionStorage.setItem('appearance-bench-start', String(performance.timeOrigin + performance.now()));
          element.click();
        }, 0);
      });
      await ready(page, client);
      warmNavigation.push(await sample(page, true));
    } finally { await context.close(); }
  }
  return Object.fromEntries(Object.entries({ cold, warmReload, warmNavigation }).map(([mode, results]) => [
    mode, {
      tti: stats(results.map(row => row.milliseconds)),
      transferredBytes: stats(results.map(row => row.transferredBytes)),
    },
  ]));
}

try {
  if (screenshots) await mkdir(screenshots, { recursive: true });
  browser = await chromium.launch();
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, reducedMotion: 'reduce' });
  const page = await context.newPage();
  page.setDefaultTimeout(12000);
  let origin = await startup();
  const port = Number(new URL(origin).port);
  const reactTokens = await exercise(page, origin, 'react');
  const react = samples && await benchmark(origin, 'react');
  await stop();
  origin = await startup('leptos', port);
  const leptosTokens = await exercise(page, origin, 'leptos');
  assert.deepEqual(leptosTokens, reactTokens);
  await headers(origin);
  await crossTab(context, page, origin);
  await rollout(page, origin);
  await faults(context, origin);
  const leptos = samples && await benchmark(origin, 'leptos');
  await stop();
  origin = await startup('react', port);
  await open(page, origin, 'react');
  const value = await page.evaluate(key => JSON.parse(localStorage.getItem(key)), key);
  assert.deepEqual(value, { version: 1, colorTheme: 'github', theme: 'light', textSize: 'large' });
  await context.close();
  if (samples) console.log(JSON.stringify({ chromium: browser.version(), samples, unit: 'ms', react, leptos }, null, 2));
  console.log('Appearance parity passed (React and Leptos, focus, tokens, gzip/CSP, failed WASM, and selector rollback).');
} finally {
  await browser?.close();
  await stop();
  await rm(data, { recursive: true });
}
