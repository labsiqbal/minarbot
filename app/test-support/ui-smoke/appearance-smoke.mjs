import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
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
const general = '/settings/general';
const key = 'minarbot-presentation-v1';
assert(Number.isSafeInteger(samples) && samples >= 0);
const wasmName = (await readdir(join(dist, '_leptos/assets'))).find(name => /^appearance\..*\.wasm$/.test(name));
assert.match(wasmName ?? '', /^appearance\.[a-f0-9]{16}\.wasm$/);
const wasm = '/_leptos/assets/' + wasmName;
const data = await mkdtemp(join(tmpdir(), 'minarbot-appearance-'));
let server, browser;

async function startup(client, port = 0, config = {}) {
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
    database: join(data, 'state.sqlite'), dist, port, appearanceClient: client, ...config,
  }));
  return Promise.race([ready, wait(60000, undefined, { ref: false }).then(() => {
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
    if (client === 'leptos') {
      const modules = await page.evaluate(() => performance.getEntriesByType('resource')
        .map(entry => new URL(entry.name).pathname).filter(path => path.endsWith('.wasm')));
      assert.deepEqual(modules, [wasm]);
    }
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
    await page.waitForFunction(() => getComputedStyle(document.documentElement).fontSize === '18px');
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
  const documentStart = await page.evaluate(() => performance.timeOrigin);
  await button(page, 'Appearance').click();
  await page.waitForURL(url);
  await ready(page, 'leptos');
  assert.equal(await page.locator('#root').count(), 0);
  assert.equal(await page.evaluate(() => performance.timeOrigin), documentStart);
  await page.goBack({ waitUntil: 'commit' });
  await panel(page).getByRole('heading', { name: 'General' }).waitFor();
  assert.equal(await page.evaluate(() => performance.timeOrigin), documentStart);
  await page.goForward({ waitUntil: 'commit' });
  await ready(page, 'leptos');
  await page.keyboard.press('Escape');
  await page.waitForURL(origin + '/routines');
  const trigger = page.getByRole('link', { name: 'Settings', exact: true });
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
  await page.goto(origin + general + '?ui=react');
  await panel(page).getByRole('heading', { name: 'General' }).waitFor();
  await button(page, 'Appearance').click();
  await ready(page, 'react');
  assert.equal(new URL(page.url()).searchParams.get('ui'), 'react');
  await page.reload({ waitUntil: 'commit' });
  await ready(page, 'react');
  await open(page, origin, 'leptos', url.slice(origin.length));
  const settingsStart = await page.evaluate(() => performance.timeOrigin);
  await button(page, 'Vault').click();
  await page.waitForURL(origin + '/settings/vault?from=%2Froutines');
  await panel(page).getByRole('heading', { name: 'Vault', exact: true }).waitFor();
  assert.equal(await page.evaluate(() => performance.timeOrigin), settingsStart);
  const [response] = await Promise.all([
    page.waitForResponse(response => response.url() === origin + '/routines'),
    button(page, 'Close settings').click(),
  ]);
  assert.equal(response.status(), 200);
  assert.equal(response.headers()['content-security-policy'].includes('wasm-unsafe-eval'), true);
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
  const generalResponse = await fetch(origin + general);
  assert.equal(generalResponse.headers.get('content-security-policy').includes('wasm-unsafe-eval'), true);
  assert.match(await generalResponse.text(), /Open standard General/);
  for (const path of ['/settings/general?ui=react', '/calendar?ui=react']) {
    const other = await fetch(origin + path);
    assert.equal(other.headers.get('content-security-policy').includes('wasm-unsafe-eval'), false);
  }
  for (const path of ['/routines', '/workflows', '/calendar']) {
    const owned = await fetch(origin + path);
    assert.equal(owned.headers.get('content-security-policy').includes('wasm-unsafe-eval'), true);
    assert.match(await owned.text(), /id="leptos-root"/);
  }
  const outside = await fetch(origin + route + '?from=' + encodeURIComponent('//example.invalid'));
  assert.match(await outside.text(), /data-return-to="\/"/);
}

async function generalParity(context, origin, client) {
  const page = await context.newPage();
  const errors = [], consoleErrors = [], calls = [];
  const focus = () => page.waitForFunction(() => document.activeElement?.textContent === 'General');
  const archive = () => panel(page).getByRole('switch', { name: 'Auto-archive agent deliveries' });
  const search = () => panel(page).getByRole('textbox', { name: 'SearXNG URL' });
  const docs = panel(page).getByRole('region', { name: 'Library docs', exact: true });
  let docsStatus = { reachable: true, keySet: false };
  await page.route('**/api/settings/library-docs', route => {
    assert.equal(route.request().method(), 'GET');
    return route.fulfill({ json: docsStatus });
  });
  const waitArchive = value => page.waitForFunction(value => {
    const control = document.querySelector('[role="switch"][aria-label="Auto-archive agent deliveries"]');
    return control && !control.disabled && control.getAttribute('aria-checked') === value;
  }, value);
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', msg => {
    if (msg.type() === 'error') consoleErrors.push(msg.text());
  });
  page.on('request', request => {
    const path = new URL(request.url()).pathname;
    if (path === '/api/conversations/settings' || path.startsWith('/api/settings/web-search')) calls.push([request.method(), path]);
  });
  // Hold the initial read so the disabled state can be checked in both clients.
  let release;
  const pending = new Promise(resolve => { release = resolve; });
  const chats = '**/api/conversations/settings';
  const initial = async route => {
    await pending;
    if (!page.isClosed()) await route.continue();
  };
  await page.route(chats, initial);
  try {
    const response = await page.goto(origin + general, { waitUntil: 'commit' });
    assert.equal(response.status(), 200);
    assert.equal((await response.headerValue('content-security-policy')).includes("'wasm-unsafe-eval'"), client === 'leptos');
    await focus();
    assert.equal(await archive().isDisabled(), true);
    release();
    await waitArchive('true');
    await page.unroute(chats, initial);
    await archive().press('Space');
    await waitArchive('false');
    await docs.getByRole('status').filter({ hasText: 'Reachable' }).waitFor();
    await docs.getByText('Optional', { exact: true }).waitFor();
    docsStatus = { reachable: false, keySet: true };
    await docs.getByRole('button', { name: 'Check status' }).press('Enter');
    await docs.getByRole('status').filter({ hasText: 'Unavailable' }).waitFor();
    await docs.getByText('Set in Vault', { exact: true }).waitFor();
    docsStatus.keySet = false;
    await search().waitFor();
    assert.equal(await button(page, 'Save').isDisabled(), true);
    assert.equal(await button(page, 'Test').isDisabled(), true);
    await search().fill('https://example.invalid/search');
    await search().press('/');
    assert.equal(await search().evaluate(element => element === document.activeElement), true);
    await search().fill('https://example.invalid/search');
    // Fixture response keeps this test offline while checking the original test endpoint and payload.
    await page.route('**/api/settings/web-search/test', route => {
      const data = route.request().postDataJSON();
      assert.equal(data.url, 'https://example.invalid/search');
      assert.match(data.id, /^[a-f0-9-]{36}$/i);
      return route.fulfill({ json: { count: 2 } });
    });
    await button(page, 'Test').press('Enter');
    await panel(page).getByRole('status').filter({ hasText: '2 results' }).waitFor();
    const saved = panel(page).getByRole('status').filter({ hasText: 'Web search settings saved.' });
    await button(page, 'Save').press('Enter');
    await saved.waitFor();
    await page.evaluate(key => localStorage.setItem(key, JSON.stringify({ version: 1, theme: 'light', textSize: 'standard' })), key);
    await page.reload({ waitUntil: 'commit' });
    await focus();
    await waitArchive('false');
    await archive().press('Space');
    await waitArchive('true');
    await search().waitFor();
    assert.equal(await search().inputValue(), 'https://example.invalid/search');
    await search().fill('');
    await button(page, 'Save').press('Enter');
    await saved.waitFor();
    assert.equal(await page.locator('html').getAttribute('data-theme'), 'light');
    assert.equal(await page.locator('html').getAttribute('data-color-theme'), 'minarbot');
    if (screenshots) {
      for (const [label, viewport] of [['desktop', { width: 1440, height: 900 }], ['narrow', { width: 430, height: 850 }]]) {
        await page.setViewportSize(viewport);
        for (const mode of ['Dark', 'Light']) {
          await button(page, 'Appearance').click();
          await ready(page, client);
          await radio(page, mode).click();
          await button(page, 'General').click();
          await focus();
          await search().waitFor();
          await waitArchive('true');
          await docs.getByText('Optional', { exact: true }).waitFor();
          await docs.getByRole('button', { name: 'Check status', disabled: false }).waitFor();
          await page.evaluate(() => document.fonts.ready);
          await page.screenshot({ path: join(screenshots, `general-${client}-${label}-${mode.toLowerCase()}.png`), animations: 'disabled' });
        }
      }
    }
    assert.deepEqual(consoleErrors, []);
    await page.route(chats, route => route.fulfill({ status: 503, json: { error: 'Fixture load failure' } }));
    await page.reload({ waitUntil: 'commit' });
    await panel(page).getByRole('alert').filter({ hasText: 'Conversation settings could not be loaded' }).waitFor();
    assert.equal(await archive().isDisabled(), true);
    assert.deepEqual(errors, []);
    assert(consoleErrors.every(message => !/content security policy|unreachable|webassembly/i.test(message)));
    return [...new Set(calls.map(row => row.join(' ')))].sort();
  } finally { await page.close(); release(); }
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
    const ready = performance.getEntriesByName('appearance-heading-focus').at(-1)?.startTime;
    if (ready === undefined || ready < start) throw new Error('Appearance heading did not receive focus');
    return {
      milliseconds: ready - start,
      transferredBytes: performance.getEntries()
        .filter(entry => entry.startTime >= start && entry.responseEnd <= ready && new URL(entry.name, location.href).origin === location.origin)
        .reduce((total, entry) => total + (entry.transferSize ?? 0), 0),
    };
  }, fromClick);
}
async function benchmark(origin, client) {
  const cold = [], warmReload = [], warmNavigation = [];
  for (let index = 0; index < samples; index += 1) {
    const context = await browser.newContext();
    try {
      await context.addInitScript(() => document.addEventListener('focusin', event => {
        if (event.target instanceof HTMLHeadingElement && event.target.textContent === 'Appearance') {
          performance.mark('appearance-heading-focus');
        }
      }));
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

async function setupAndDirectory(origin, client) {
  const context = await browser.newContext({ reducedMotion: 'reduce' });
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', message => {
    if (message.type() === 'error') errors.push(message.text());
  });
  const heading = name => page.getByRole('heading', { name, level: 1, exact: true });
  const setup = page.locator('.first-run').getByRole('heading', { name: 'Set up Minarbot', level: 1 });
  const project = 'Directory fixture';
  const id = randomUUID();
  const projectPath = `/projects/${id}`;
  const post = async (path, body) => {
    const response = await context.request.post(origin + path, { headers: { Origin: origin }, data: body });
    assert.equal(response.ok(), true, `${path} returned ${response.status()}`);
    return response.json();
  };
  const capture = async (routeName, location, ready) => {
    if (!screenshots) return;
    for (const [width, height] of [[1440, 900], [430, 850]]) {
      await page.setViewportSize({ width, height });
      for (const theme of ['dark', 'light']) {
        await page.emulateMedia({ colorScheme: theme });
        await page.goto(origin + location);
        await page.evaluate(({ key, theme }) => localStorage.setItem(key, JSON.stringify({
          version: 1, colorTheme: 'minarbot', theme, textSize: 'standard',
        })), { key, theme });
        await page.reload();
        await ready();
        await page.evaluate(() => document.fonts.ready);
        await page.screenshot({ path: join(screenshots, `${routeName}-${client}-${width}-${theme}.png`), animations: 'disabled' });
        assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
      }
    }
  };
  try {
    await page.goto(origin);
    await setup.waitFor();
    assert.equal(await setup.evaluate(element => document.activeElement === element), true);
    await page.getByRole('button', { name: 'Check again', exact: true }).waitFor();
    assert.equal(await page.locator('#leptos-root .first-run').count(), client === 'leptos' ? 1 : 0);
    await capture('setup', '/', async () => {
      await setup.waitFor();
      await page.getByRole('button', { name: 'Check again', exact: true }).waitFor();
    });
    await page.setViewportSize({ width: 1440, height: 900 });
    await post('/api/projects', { id, name: project, members: [] });
    await page.reload();
    const projectsTab = page.getByRole('tab', { name: /^Projects\s*1$/ });
    await projectsTab.waitFor();
    if ((await projectsTab.getAttribute('aria-selected')) !== 'true') {
      await page.getByRole('tab', { name: /^Agents/, selected: true }).press('ArrowRight');
    }
    await page.getByRole('tab', { name: /^Projects/, selected: true }).waitFor();
    const nav = page.getByRole('navigation', { name: 'Projects', exact: true });
    const projectEntry = nav.getByText(project, { exact: true });
    await projectEntry.click();
    await page.waitForURL(origin + projectPath);
    await heading(project).waitFor();
    await page.getByText('No members yet', { exact: true }).waitFor();
    await page.getByText('No conversations', { exact: true }).waitFor();
    assert.equal(await page.locator('#leptos-root .project-home').count(), client === 'leptos' ? 1 : 0);
    await page.goBack();
    await setup.waitFor();
    await page.goForward();
    await heading(project).waitFor();
    await page.reload();
    await heading(project).waitFor();
    await capture('project', projectPath, () => heading(project).waitFor());
    const agent = await post('/api/agents', {
      id: randomUUID(), name: 'Directory agent', model: 'fixture', instructions: 'Test only.', runtime: 'api',
    });
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.reload();
    await heading(project).waitFor();
    await page.getByRole('tab', { name: /^Projects/ }).press('ArrowLeft');
    const sidebar = page.getByRole('complementary', { name: 'Workspace navigation' });
    const find = page.getByRole('searchbox', { name: 'Search agents' }).or(page.getByRole('textbox', { name: 'Search agents' }));
    await find.fill('Directory agent');
    await sidebar.getByText('Directory agent', { exact: true }).click();
    await page.waitForURL(origin + `/agents/${agent.id}`);
    await page.getByRole('main', { name: 'Conversation Directory agent' }).waitFor();
    assert.equal(await page.locator('#leptos-root').count(), 0);
    assert.deepEqual(errors, []);
  } finally { await context.close(); }
}

async function pairing(client) {
  const origin = await startup(client, 0, { testRemotePairing: true });
  const context = await browser.newContext();
  try {
    const issued = await context.request.post(origin + '/api/device-pairing/code', { headers: { Origin: origin }, data: {} });
    assert.equal(issued.ok(), true);
    const { code } = await issued.json();
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    page.on('console', message => {
      if (message.type() === 'error') errors.push(message.text());
    });
    const pairingForm = page.getByRole('heading', { name: 'Pair this device', exact: true });
    for (const [width, height] of [[1440, 900], [430, 850]]) {
      await page.setViewportSize({ width, height });
      for (const theme of ['dark', 'light']) {
        await page.emulateMedia({ colorScheme: theme });
        await page.goto(origin + '/pair');
        if (client === 'leptos') await page.locator('#leptos-root form').waitFor();
        await pairingForm.waitFor();
        if (screenshots) await page.screenshot({ path: join(screenshots, `pairing-${client}-${width}-${theme}.png`) });
      }
    }
    await page.keyboard.press('Tab');
    assert.equal(await page.getByRole('textbox', { name: 'Pairing code', exact: true }).evaluate(element => element === document.activeElement), true);
    await page.keyboard.type(code);
    await page.keyboard.press('Tab');
    await page.keyboard.type('Parity browser');
    await page.keyboard.press('Tab');
    await page.keyboard.press('Enter');
    await page.waitForURL(origin + '/');
    assert.equal(await pairingForm.count(), 0);
    // Use the browser's Secure cookie handling on loopback.
    const result = await page.evaluate(async () => {
      const response = await fetch('/api/device-pairing');
      return response.json();
    });
    assert.equal(result.devices.find(device => device.id === result.current)?.name, 'Parity browser');
    assert.deepEqual(errors, []);
  } finally { await context.close(); await stop(); }
}

try {
  if (screenshots) await mkdir(screenshots, { recursive: true });
  browser = await chromium.launch();
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, reducedMotion: 'reduce' });
  const page = await context.newPage();
  page.setDefaultTimeout(12000);
  let origin = await startup();
  const port = Number(new URL(origin).port);
  const reactCalls = await generalParity(context, origin, 'react');
  const reactTokens = await exercise(page, origin, 'react');
  const react = samples && await benchmark(origin, 'react');
  await stop();
  origin = await startup('leptos', port);
  const leptosCalls = await generalParity(context, origin, 'leptos');
  assert.deepEqual(leptosCalls, reactCalls);
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
  if (samples) {
    console.log(JSON.stringify({ chromium: browser.version(), samples, unit: 'ms', react, leptos }, null, 2));
    if (samples >= 20) {
      assert(leptos.cold.tti.p95 <= react.cold.tti.p95 * 1.2, 'Leptos cold settings start exceeds React p95 by over 20%');
      assert(leptos.warmNavigation.tti.p95 <= react.warmNavigation.tti.p95 * 1.1, 'Leptos warm settings navigation exceeds React p95 by over 10%');
    }
  }
  console.log('Preferences parity passed (React and Leptos, API, focus, tokens, browser history, gzip/CSP, failed WASM, and selector rollback).');
  for (const client of ['react', 'leptos']) {
    await stop();
    origin = await startup(client, 0, { database: join(data, `${client}.sqlite`) });
    await setupAndDirectory(origin, client);
    await stop();
    await pairing(client);
  }
  console.log('First-run, project navigation and browser pairing passed in both clients.');
} finally {
  await browser?.close();
  await stop();
  await rm(data, { recursive: true });
}
