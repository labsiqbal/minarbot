import assert from 'node:assert/strict';
import { parityShot } from './parity-shot.mjs';
import { randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import { startRustServer } from '../../rust-test-server.ts';

const data = await mkdtemp(join(tmpdir(), 'minarbot-system-settings-'));
const dist = fileURLToPath(new URL('../../dist', import.meta.url));
const output = process.env.MINARBOT_SYSTEM_SCREENSHOTS;
if (output) await mkdir(output, { recursive: true });
let origin;

const peerId = 'fixture-peer';
const agentId = '40000000-0000-4000-8000-000000000040';
const accountId = '40000000-0000-4000-8000-000000000041';
const deviceId = '40000000-0000-4000-8000-000000000042';
const tokenId = '40000000-0000-4000-8000-000000000043';
const stamp = Date.parse('2026-11-15T00:00:00Z');
const created = new Date(stamp).toISOString();
const release = { id: '0.2.0-12345678', version: '0.2.0', commit: '12345678'.repeat(5), created, notes: 'Synthetic release notes.', source: 'Local feed' };
const current = { ...release, id: '0.1.0-87654321', version: '0.1.0', commit: '87654321'.repeat(5) };
const pathFor = (route, client) => `/settings/${route}`;
const settings = page => page.getByRole('dialog', { name: 'Settings', exact: true });
const button = (scope, name) => scope.getByRole('button', { name, exact: true });
const modal = (page, name) => page.getByRole('dialog', { name, exact: true });
const consent = page => page.getByRole('alertdialog');
const visible = locator => locator.waitFor({ state: 'visible' });
const hidden = locator => locator.waitFor({ state: 'hidden' });
const exact = (scope, text) => scope.getByText(text, { exact: true });
const uuid = id => assert.match(id, /^[0-9a-f-]{36}$/i);
const names = {
  machines: ['Machines', 'Studio workstation'],
  devices: ['Devices', 'Test phone'],
  computer: ['Computer', 'Image missing'],
  updates: ['Updates', 'Minarbot 0.2.0'],
  usage: ['Usage', 'Synthetic reporting agent'],
  data: ['Shared data', 'settings_fixture'],
  backup: ['Backup & restore', 'Workspace backup'],
  'mcp-server': ['External access', 'Fixture token'],
};

async function fixtures(page, commands) {
  const environment = { id: 'fixture-local', name: 'Fixture server', version: current.version, origin: 'https://minarbot.example.test' };
  const machine = { id: peerId, environmentId: peerId, name: 'Studio workstation', origin: 'https://studio.example.test', version: current.version,
    status: 'online', lastSeen: stamp, agentCount: 0, agents: [], pairingState: 'paired', allowAgentMessaging: false, allowUpdates: true,
    providerAccounts: [{ id: accountId, name: 'Fixture account', provider: 'openai-compatible', host: 'https://provider.example.test', defaultModel: 'fixture-model' }] };
  const device = { id: deviceId, name: 'Test phone', lastSeen: stamp };
  const computer = { state: 'image_missing', runtime: 'Podman', missingPrograms: [], canSetup: true, message: null };
  const update = { current, available: release, feed: release, launcherAvailable: true, runningTurns: 2 };
  const peer = { machine: peerId, available: true, current: { version: current.version }, launcherAvailable: true, runningTurns: 1 };
  const table = { name: 'settings_fixture', owner: agentId, columns: [{ name: 'message', type: 'TEXT' }] };
  const listing = { tables: [table], next_after: null, revision: 4 };
  const queryResult = { columns: ['message'], rows: [['Synthetic shared row']], changes: 0, truncated: false, revision: 4 };
  const scope = { id: tokenId, name: 'Fixture token', scopes: ['agents:read'], created: stamp, revoked: null, lastUsed: null };
  const tokens = [scope];
  const deviceSettings = { required: false, canRequire: true, current: null, pairingUrl: 'https://minarbot.example.test/pair', devices: [device] };
  const summary = { calls: 51, pending: 1, failed: 1, interrupted: 1, input_tokens: '9007199254740993', output_tokens: '0', total_tokens: null };
  const transcription = { calls: 1, pending: 0, uncertain: 1, cancelled: 0, unknown: 1, input_tokens: null, output_tokens: null, total_tokens: null, seconds: null };
  const call = { seq: 3, agent_name: 'Synthetic reporting agent', model: 'fixture-model', origin: 'https://provider.example.test/v1',
    family: 'openai-compatible', purpose: 'completion', phase: 'answer', status: 'received', created, input_tokens: null, output_tokens: 0, total_tokens: null };
  const path = '/api/mcp-server/peer-tokens';
  const stub = async (endpoint, read, write) => page.route(`**${endpoint}${endpoint === '/api/usage' || endpoint === '/api/data' ? '*' : ''}`, async route => {
    const req = route.request();
    if (req.method() === 'GET') return read === undefined ? route.fallback() : route.fulfill({ json: typeof read === 'function' ? read(new URL(req.url())) : read });
    if (!write) return route.fallback();
    const body = req.postDataJSON();
    commands.push({ endpoint, body });
    const result = write(body);
    if (result === undefined) return route.abort('aborted');
    return route.fulfill({ json: result });
  });
  await Promise.all([
    stub('/api/agents', { agents: [{ id: agentId, name: 'Fixture agent', role: 'Assistant', model: 'fixture-model', runtime: 'api' }], hubAvailable: false, configured: true, defaultModel: 'fixture-model' }),
    stub('/api/projects', { projects: [] }),
    stub('/api/channels', []),
    stub('/api/sidebar/summary', { summaries: [] }),
    stub('/api/environment', environment, body => Object.assign(environment, { name: body.name })),
    stub('/api/machines', () => ({ machines: [machine] }), () => ({})),
    stub('/api/machines/agent-messaging', undefined, body => { machine.allowAgentMessaging = body.allowed; return {}; }),
    stub('/api/machines/updates', undefined, body => { machine.allowUpdates = body.allowed; return {}; }),
    stub('/api/pairing-codes', undefined, () => ({ code: 'ABCDEFGHJKMN', expiresAt: Date.now() + 120_000 })),
    stub('/api/install/tokens', undefined, () => ({ source: 'https://minarbot.example.test', token: 'fixture-install', expiresAt: Date.now() + 120_000 })),
    stub('/api/device-pairing', deviceSettings, body => { deviceSettings.required = body.required; return {}; }),
    stub('/api/device-pairing/code', undefined, () => ({ code: '472951', url: 'https://minarbot.example.test/pair', qr: 'data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciLz4=' })),
    stub('/api/device-pairing/revoke', undefined, () => { deviceSettings.devices = []; return {}; }),
    stub('/api/settings/computer', computer, () => Object.assign(computer, { state: 'downloading' })),
    stub('/api/updates', update),
    stub('/api/updates/machines', { machines: [peer] }),
    stub('/api/updates/apply', undefined, body => {
      if (commands.filter(command => command.endpoint === '/api/updates/apply').length === 1) return undefined;
      update.lastAttempt = { id: body.id, version: release.version, status: 'downloading', totalBytes: 100, bytesTransferred: 37 };
      return { id: body.id };
    }),
    stub('/api/updates/fan-out', undefined, body => {
      peer.lastAttempt = { id: body.id, version: release.version, status: 'done' };
      return { machines: [{ machine: peerId, status: 'accepted', id: body.id }] };
    }),
    stub('/api/usage', url => ({ summary, transcription, calls: [call], next_before: url.search ? null : 3 })),
    stub('/api/data', () => listing),
    stub('/api/data/query', undefined, () => queryResult),
    stub('/api/data/drop', undefined, () => {
      if (commands.filter(command => command.endpoint === '/api/data/drop').length === 1) return undefined;
      listing.tables = [];
      return { ...queryResult, columns: [], rows: [], changes: 1, revision: 5 };
    }),
    stub(path, () => ({ items: tokens, endpoint: '/mcp' }), body => {
      const item = { ...scope, id: randomUUID(), name: body.name, scopes: body.scopes };
      tokens.push(item);
      return { ...item, token: 'fixture-one-time-value' };
    }),
    stub(`${path}/${tokenId}/revoke`, undefined, () => { scope.revoked = stamp; return scope; }),
  ]);
  return { update, computer, peer };
}

async function open(page, client, route) {
  const [name, populated] = names[route];
  const response = await page.goto(origin + pathFor(route, client));
  assert.equal(response.status(), 200);
  assert.equal((await response.headerValue('content-security-policy')).includes("'wasm-unsafe-eval'"), true);
  const panel = settings(page);
  await visible(panel.getByRole('heading', { name, exact: true }));
  await visible(exact(panel, populated).first());
  assert.equal(await page.locator('#appearance-dialog[open]').count(), 1);
  return panel;
}

async function exercise(page, client, route, panel, state, commands) {
  const posts = endpoint => commands.filter(command => command.endpoint === endpoint).map(command => command.body);
  switch (route) {
    case 'machines': {
      const add = button(panel, 'Add machine');
      await add.press('ArrowDown');
      await page.getByRole('menuitem', { name: 'Show pairing code', exact: true }).click();
      const pairing = modal(page, 'Show pairing code');
      await button(pairing, 'Create code').click();
      await visible(pairing.getByRole('region', { name: 'One-time pairing code' }));
      await page.keyboard.press('Escape');
      await hidden(pairing);
      await page.waitForFunction(() => document.activeElement?.getAttribute('aria-label') === 'Add machine');
      assert.equal(posts('/api/pairing-codes').length, 1);
      await add.click();
      await page.getByRole('menuitem', { name: 'Install Minarbot', exact: true }).click();
      const install = modal(page, 'Install Minarbot');
      await button(install, 'Create install command').click();
      const command = install.getByLabel('Install command', { exact: true });
      await visible(command);
      assert.match(await command.inputValue(), /^curl -fsSL 'https:\/\/minarbot\.example\.test\/api\/install\/script'/);
      await page.keyboard.press('Escape');
      await hidden(install);
      await add.click();
      await page.getByRole('menuitem', { name: 'Pair machine', exact: true }).click();
      const pair = modal(page, 'Pair machine');
      await pair.getByLabel('Address', { exact: true }).fill('https://example.test');
      await pair.getByLabel('Pairing code', { exact: true }).fill('abcdefghjkmn');
      await button(pair, 'Pair machine').click();
      await hidden(pair);
      assert.equal(posts('/api/machines')[0].code, 'ABCDEFGHJKMN');
      uuid(posts('/api/machines')[0].id);
      await button(panel, 'Open Studio workstation').click();
      const detail = modal(page, 'Studio workstation');
      const policy = detail.getByRole('switch', { name: 'Incoming messages' });
      await policy.press('Space');
      await page.waitForFunction(() => document.querySelector('[role=switch][aria-label="Incoming messages"]')?.getAttribute('aria-checked') === 'true');
      assert.equal(posts('/api/machines/agent-messaging')[0].allowed, true);
      assert.equal(posts('/api/machines/agent-messaging')[0].environmentId, peerId);
      await page.keyboard.press('Escape');
      await hidden(detail);
      break;
    }
    case 'devices': {
      const pair = button(panel, 'Pair a device');
      await pair.press('Enter');
      const dialog = modal(page, 'Pair a device');
      await visible(dialog.getByRole('img', { name: /QR code/ }));
      assert.deepEqual(posts('/api/device-pairing/code'), [{}]);
      await button(dialog, 'Done').click();
      await hidden(dialog);
      await button(panel, 'Revoke Test phone').click();
      await button(consent(page), 'Cancel').click();
      assert.deepEqual(posts('/api/device-pairing/revoke'), []);
      await button(panel, 'Revoke Test phone').click();
      await button(consent(page), 'Revoke').click();
      await visible(exact(panel, 'No paired devices'));
      assert.deepEqual(posts('/api/device-pairing/revoke'), [{ id: deviceId }]);
      break;
    }
    case 'computer': {
      const setup = button(panel, 'Set up Computer');
      await setup.press('Enter');
      await visible(exact(panel, 'Downloading image'));
      assert.equal(await setup.isDisabled(), true);
      assert.deepEqual(posts('/api/settings/computer'), [{}]);
      Object.assign(state.computer, { state: 'failed', message: 'Synthetic setup failure.' });
      await page.evaluate(() => window.dispatchEvent(new Event('focus')));
      await visible(exact(panel.getByRole('alert'), 'Synthetic setup failure.'));
      Object.assign(state.computer, { state: 'ready', message: null, canSetup: false });
      await page.evaluate(() => window.dispatchEvent(new Event('focus')));
      await visible(exact(panel, 'Ready · Podman'));
      {
        assert.equal(await panel.getByRole('link', { name: /computer viewer/ }).getAttribute('href'), `/?computer=${agentId}`);
      }
      break;
    }
    case 'updates': {
      await button(panel, 'Update now').press('Enter');
      const plan = modal(page, 'Update 1 machine?');
      await visible(exact(plan, '2 running turns will be interrupted and can resume after the restart.'));
      await button(plan, 'Cancel').click();
      assert.deepEqual(posts('/api/updates/apply'), []);
      await button(panel, 'Update now').click();
      await button(plan, 'Update').click();
      const progress = modal(page, 'Updating 1 machine');
      await visible(exact(progress, 'Downloading 37%'));
      assert.equal(await page.locator('progress[aria-label="Download progress"]').getAttribute('value'), '37');
      const attempts = posts('/api/updates/apply');
      assert.deepEqual(attempts[0], attempts[1]);
      assert.equal(attempts[0].version, release.version);
      uuid(attempts[0].id);
      // React's update modal sits above workspace.
      // Reloading with both Radix portals open hides both from assistive technology.

      await page.reload();
      await visible(modal(page, 'Updating 1 machine'));
      state.update.lastAttempt.status = 'done';
      const completed = modal(page, 'Update complete');
      await visible(completed);
      await button(completed, 'Close').last().click();
      assert.deepEqual(posts('/api/updates/apply'), attempts);

      await panel.getByRole('switch', { name: 'Also update Studio workstation' }).press('Space');
      await button(panel, 'Update selected (1)').click();
      await button(plan, 'Update').click();
      await visible(completed);
      assert.deepEqual(posts('/api/updates/fan-out')[0].machines, [peerId]);
      await button(completed, 'Close').last().click();
      break;
    }
    case 'usage': {
      await visible(exact(panel, '9,007,199,254,740,993'));
      await panel.getByRole('tab', { name: 'Calls', exact: true }).press('ArrowRight');
      await visible(panel.getByRole('tabpanel', { name: 'Transcription', exact: true }));
      await panel.getByRole('tab', { name: 'Calls', exact: true }).click();
      await button(panel, 'Older calls').click();
      await page.waitForFunction(() => document.activeElement?.textContent === 'Usage');
      await button(panel, 'Newer calls').click();
      break;
    }
    case 'data': {
      await button(panel, 'Schema').press('Enter');
      await visible(panel.getByRole('region', { name: 'Columns in settings_fixture' }));
      await button(panel, 'SQL query').click();
      await panel.getByRole('textbox', { name: 'SQL query', exact: true }).fill('SELECT * FROM settings_fixture');
      await button(panel, 'Run query').click();
      await visible(exact(panel, 'Synthetic shared row'));
      assert.equal(posts('/api/data/query')[0].revision, 4);
      await button(panel, 'Menu settings_fixture').click();
      await page.getByRole('menuitem', { name: 'Delete table settings_fixture', exact: true }).click();
      await button(consent(page), 'Delete table').click();
      await visible(exact(panel, 'Not confirmed: delete settings_fixture'));
      await page.reload();
      await button(panel, 'Try again').click();
      await visible(exact(panel, 'No tables'));
      assert.deepEqual(posts('/api/data/drop')[0], posts('/api/data/drop')[1]);
      break;
    }
    case 'backup': {
      await button(panel, 'Open').press('Enter');
      const restore = modal(page, 'Restore backup');
      await visible(restore.getByRole('region', { name: 'Backup restore command' }));
      await page.waitForFunction(element => element.contains(document.activeElement), await restore.elementHandle());
      await page.keyboard.press('Escape');
      await hidden(restore);
      await button(panel, 'Download').click();
      await button(consent(page), 'Cancel').click();
      await button(panel, 'Download').click();
      const requested = page.waitForRequest(req => new URL(req.url()).pathname === '/api/backup');
      const download = page.waitForEvent('download');
      await button(consent(page), 'Download backup').click();
      const body = (await requested).postDataJSON();
      assert.equal(body.confirmed, true);
      uuid(body.id);
      assert.equal((await download).suggestedFilename(), 'minarbot-workspace.sqlite');
      break;
    }
    case 'mcp-server': {
      await button(panel, 'Create token').press('Enter');
      const create = modal(page, 'Create token');
      await create.getByLabel('Name', { exact: true }).fill('New fixture token');
      await create.getByRole('checkbox', { name: 'Read token conversations', exact: true }).check();
      await button(create, 'Create token').click();
      const secret = modal(page, 'Copy token now');
      await visible(secret.getByLabel('Token', { exact: true }));
      assert.equal(await secret.getByLabel('Token', { exact: true }).inputValue() === 'fixture-one-time-value', true);
      await button(secret, 'I saved the token').click();
      await hidden(secret);
      assert.equal(await page.locator('input[data-private-token]').evaluateAll(inputs => inputs.every(input => input.value === '')), true);
      assert.equal(await page.evaluate(() => Object.values(sessionStorage).every(value => !value.includes('fixture-one-time-value'))), true);
      assert.deepEqual(posts('/api/mcp-server/peer-tokens')[0].scopes, ['agents:read', 'history:read']);
      await button(panel, 'Token actions for Fixture token').click();
      await page.getByRole('menuitem', { name: 'Revoke token', exact: true }).click();
      await button(consent(page), 'Revoke token').click();
      await visible(exact(panel, 'Revoked'));
      break;
    }
  }
}

const clients = ['leptos'];
const routes = process.env.MINARBOT_SYSTEM_ROUTE ? [process.env.MINARBOT_SYSTEM_ROUTE] : Object.keys(names);
const failures = [];
let app, browser;
try {
  app = await startRustServer({ appearanceClient: 'leptos', database: join(data, 'state.sqlite'), defaultModel: 'fixture-model', dist, port: 0, });
  origin = app.origin;
  assert.match(origin, /^http:\/\/127\.0\.0\.1:\d+$/);
  assert.notEqual(new URL(origin).port, '4317');
  const seed = await fetch(origin + '/api/agents', {
    method: 'POST', headers: { Origin: origin, 'Content-Type': 'application/json' },
    body: JSON.stringify({ id: agentId, name: 'Fixture agent', instructions: 'Help.', model: 'fixture-model', runtime: 'api' }),
  });
  assert.equal(seed.status, 201);
  browser = await chromium.launch({ executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE });
  assert(routes.every(route => Object.hasOwn(names, route)));
  for (const client of clients) {
    for (const route of routes) {
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, reducedMotion: 'reduce' });
      try {
        const page = await context.newPage();
        page.setDefaultTimeout(12_000);
        const commands = [], errors = [];
        page.on('pageerror', error => errors.push(error.message));
        page.on('console', message => {
          if (message.type() === 'error' || /content security policy/i.test(message.text())) {
            const url = message.location().url;
            errors.push(`${url ? new URL(url).pathname + ' ' : ''}${message.text()}`);
          }
        });
        await page.addInitScript(() => {
          document.addEventListener('securitypolicyviolation', event => console.error('CSP violation', event.violatedDirective));
        });
        const state = await fixtures(page, commands);
        for (const [viewport, size] of [['desktop', { width: 1440, height: 900 }], ['narrow', { width: 430, height: 850 }]]) {
          await page.setViewportSize(size);
          for (const theme of ['dark', 'light']) {
            await page.emulateMedia({ colorScheme: theme, reducedMotion: 'reduce' });
            if (page.url().startsWith(origin + '/')) {
              await page.evaluate(theme => localStorage.setItem('minarbot-presentation-v1', JSON.stringify({
                version: 1, theme, colorTheme: 'minarbot', textSize: 'standard',
              })), theme);
            }
            await open(page, client, route);
            assert.equal(await page.locator('html').getAttribute('data-theme'), theme);
            await page.evaluate(() => document.fonts.ready);
            assert.equal(await page.locator('.app-settings__main').evaluate(element => element.scrollWidth <= element.clientWidth), true);
            if (output) await parityShot(page, join(output, `${route}-${viewport}-${theme}-${client}.png`));
          }
        }
        await page.setViewportSize({ width: 1440, height: 900 });
        const panel = await open(page, client, route);
        await exercise(page, client, route, panel, state, commands);
        if (true && route === 'backup') {
          const start = await page.evaluate(() => performance.timeOrigin);
          await button(panel, 'Usage').click();
          await visible(panel.getByRole('heading', { name: 'Usage', exact: true }));
          await page.waitForFunction(() => document.activeElement?.textContent === 'Usage');
          await page.goBack();
          await visible(panel.getByRole('heading', { name: 'Backup & restore', exact: true }));
          assert.equal(await page.evaluate(() => performance.timeOrigin), start);
        }
        assert.deepEqual(errors, [], `${client} ${route} browser errors`);
        console.log(`${client} ${route} passed`);
      } catch (error) {
        console.error(`${client} ${route} failed`, error.message);
        failures.push(new Error(`${client} ${route}`, { cause: error }));
      } finally { await context.close(); }
    }
  }
} finally {
  try { await browser?.close(); } finally {
    try { await app?.close(); } finally { await rm(data, { recursive: true }); }
  }
}
if (failures.length) throw new AggregateError(failures, 'System settings parity failed');
