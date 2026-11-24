import assert from 'node:assert/strict';
import { parityShot } from './parity-shot.mjs';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createInterface } from 'node:readline';
import { setTimeout as wait } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const binary = resolve(process.env.MINARBOT_RUST_BINARY ?? join(root, 'target/debug/minarbot-server'));
const output = process.env.MINARBOT_CONNECTION_SCREENSHOTS;
const dir = await mkdtemp(join(tmpdir(), 'minarbot-connections-'));
const dist = join(root, 'app/dist');
const server = spawn(binary, ['--test-config'], {
  env: { ...process.env, MINARBOT_TEST_SERVER: 'rust' },
  stdio: ['pipe', 'pipe', 'inherit'],
});
const ready = new Promise((resolve, reject) => {
  const lines = createInterface({ input: server.stdout });
  lines.on('line', line => {
    if (line.startsWith('Minarbot: ')) resolve(line.slice('Minarbot: '.length));
  });
  server.once('error', reject);
  server.once('exit', code => reject(new Error('Minarbot stopped before readiness (' + code + ')')));
  server.stdin.once('error', reject);
});
server.stdin.end(JSON.stringify({ database: join(dir, 'state.sqlite'), dist, port: 0, }));

const agent = '10000000-0000-4000-8000-000000000001';
const connection = '40000000-0000-4000-8000-000000000001';
const entry = { name: 'fixture-login', kind: 'login', origin: 'https://example.test', agents: [], revision: 1, hasTotp: true, hasNotes: true };
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const proof = 'synthetic-value-only';
const routes = [
  ['providers', 'Providers', 'Example provider'],
  ['skills', 'Skills', 'No skills'],
  ['connected-apps', 'Connected apps', 'Connect apps'],
  ['hub', 'minarbot.com', 'Not linked'],
  ['email', 'Email', 'Endpoint path'],
  ['mcp', 'MCP connections', 'Fixture connection'],
  ['vault', 'Vault', 'fixture-login'],
];

function showFor(client, slug) {
  return '/settings/' + slug;
}

function fixtureRequests() {
  const posts = [];
  let email = { domain: null, revision: 0, hasSecret: false, endpoint: '/api/email/inbound' };
  let skills = [];
  let connectionEnabled = 1;
  const fixtures = {
    '/api/agents': { agents: [{ id: agent, name: 'Fixture agent', role: 'Fixture', runtime: 'api' }], guide: null, hubAvailable: true },
    '/api/projects': { projects: [] },
    '/api/channels': [],
    '/api/provider-instances': { providers: [{ id: 'fixture-codex', kind: 'codex', displayName: 'Fixture Codex', enabled: true, installed: true, version: '1.2.3', status: 'installed', auth: { status: 'authenticated', method: 'ChatGPT' }, binaryPath: '/usr/bin/codex', homePath: '/tmp/codex', shadowHomePath: '', launchArgs: [], environment: [], revision: 1, models: [] }] },
    '/api/skill-packs': { packs: [], next_after: null },
    ['/api/agents/' + agent + '/skills']: { items: [], next_after: null },
    '/api/connected-apps/key': { source: 'none', revision: 0 },
    '/api/connected-apps': { configured: false, items: [], categories: [], next_after: null },
    '/api/connected-apps/accounts': { items: [] },
    ['/api/agents/' + agent + '/connected-apps']: { items: [] },
    ['/api/agents/' + agent + '/connected-app-policies']: { items: [] },
    '/api/connected-apps/discord': { generation: 'fixture', bot: null, status: 'disconnected', routes: [], deliveries: [] },
    '/api/whatsapp': { state: 'disconnected', connected: false, qr: null, chats: [], deliveryProblems: [], transcription: null },
    '/api/hub-link': { enrolled: false, connected: false, disabled: false, nodeId: null, accountId: null, approvals: [] },
    '/api/vault': [entry],
  };
  const mcp = () => ({ id: connection, name: 'Fixture connection', url: 'https://mcp.example.test', hasToken: true, enabled: connectionEnabled, revision: connectionEnabled ? 1 : 2 });
  const route = async http => {
    const request = http.request();
    const path = new URL(request.url()).pathname;
    if (request.method() === 'GET') {
      if (path === '/api/email/settings') return http.fulfill({ json: email });
      if (path === '/api/skills') return http.fulfill({ json: { skills, next_after: null } });
      if (path === '/api/mcp') return http.fulfill({ json: { items: [mcp()], next_after: null } });
      if (path === '/api/mcp/' + connection) return http.fulfill({ json: { ...mcp(), tools: [], grant: { revision: 0, tools: [] } } });
      if (Object.hasOwn(fixtures, path)) return http.fulfill({ json: fixtures[path] });
      return http.continue();
    }
    const body = request.postDataJSON();
    // Retain only synthetic assertions. No credentials in receipts or failure text.
    const { id, apiKey, token, key, password, ...fields } = body;
    if (id) assert.match(id, uuid);
    const privateValue = apiKey ?? token ?? key ?? password;
    if (privateValue) assert(privateValue === proof, 'Unexpected secret in test request.');
    posts.push([path, fields]);
    if (path.startsWith('/api/provider-instances/')) return http.fulfill({ json: fixtures['/api/provider-instances'] });
    if (path === '/api/skills') {
      const created = body.files
        ? { ...body, name: 'fixture-import', description: 'Review a fixture', content: 'Check the folder.', revision: 1, latest: 1 }
        : { ...body, revision: 1, latest: 1 };
      skills.push(created);
      return http.fulfill({ json: created });
    }
    if (path === '/api/email/settings') {
      email = { ...email, domain: body.domain ?? email.domain, revision: email.revision + 1, hasSecret: body.rotateSecret || email.hasSecret };
      return http.fulfill({ json: body.rotateSecret ? { ...email, secret: proof } : email });
    }
    if (path === '/api/mcp/' + connection) {
      connectionEnabled = body.enabled ? 1 : 0;
      return http.fulfill({ json: mcp() });
    }
    return http.fulfill({ status: 400, json: { error: 'Unexpected fixture mutation.' } });
  };
  return { posts, route };
}

async function focused(page, locator, client) {
  await locator.waitFor();
  {
    await page.waitForFunction(() => document.activeElement === document.querySelector('#appearance-dialog .app-settings__page h1'));
  }
}

async function exercise(page, slug) {
  const button = name => page.getByRole('button', { name, exact: true, disabled: false });
  const dialog = name => page.getByRole('dialog', { name, exact: true });
  switch (slug) {
    case 'providers': {
      const opener = button('Add provider');
      await opener.press('Enter');
      const editor = dialog('Add provider');
      await editor.getByRole('button', { name: 'Continue', exact: true }).press('Enter');
      await editor.getByLabel('Display name', { exact: true }).fill('Fixture draft');
      await editor.getByRole('button', { name: 'Continue', exact: true }).press('Enter');
      await editor.getByRole('button', { name: 'Configure manually', exact: true }).press('Enter');
      await page.keyboard.press('Escape');
      await editor.waitFor({ state: 'hidden' });
      assert(await opener.evaluate(element => document.activeElement === element), 'Provider dialog lost opener focus.');
      await page.getByRole('heading', { name: 'Providers', exact: true }).waitFor();
      break;
    }
    case 'skills': {
      await button('Create skill').press('Enter');
      const editor = dialog('New skill');
      await editor.getByLabel('Name', { exact: true }).fill('fixture-skill');
      await editor.getByLabel('When to use', { exact: true }).fill('When testing settings');
      await editor.getByLabel('Instructions', { exact: true }).fill('Write a short fixture answer.');
      await editor.getByRole('button', { name: 'Save version', exact: true }).press('Enter');
      await page.getByRole('button', { name: /fixture-skill/ }).first().waitFor();
      await page.locator('input[type=file][accept=".md,.json,.zip"]').setInputFiles({
        name: 'fixture-folder.json',
        mimeType: 'application/json',
        buffer: Buffer.from(JSON.stringify({
          format: 'minarbot-skill-folder',
          version: 1,
          files: [
            { path: 'SKILL.md', content: '---\nname: fixture-import\ndescription: Review a fixture\n---\nCheck the folder.' },
            { path: 'asset.bin', content: 'AAECAw==', encoding: 'base64' },
          ],
        })),
      });
      const imported = dialog('Import fixture-import');
      await imported.getByText('2 files from folder').waitFor();
      const exported = page.waitForEvent('download');
      await imported.getByRole('button', { name: 'Export ZIP' }).click();
      const zip = await exported;
      assert.equal(zip.suggestedFilename(), 'fixture-import.zip');
      await imported.getByRole('button', { name: 'Save version' }).click();
      await imported.waitFor({ state: 'hidden' });
      await page.getByRole('tab', { name: 'Installed' }).press('Enter');
      await page.getByText('No installed skills').waitFor();
      break;
    }
    case 'connected-apps': {
      await button('Set up').press('Enter');
      const editor = dialog('Set up Composio');
      const key = editor.getByLabel('Composio API key', { exact: true });
      await key.waitFor();
      assert.equal(await key.getAttribute('type'), 'password');
      await key.press('Escape');
      await editor.waitFor({ state: 'hidden' });
      await page.getByRole('dialog', { name: 'Settings', exact: true }).getByRole('button')
        .filter({ has: page.getByText('WhatsApp', { exact: true }) }).click();
      await dialog('WhatsApp').getByRole('button', { name: 'Link WhatsApp' }).waitFor();
      await page.keyboard.press('Escape');
      break;
    }
    case 'hub':
      await button('Link machine').waitFor();
      assert.equal(await button('Link machine').isEnabled(), true);
      break;
    case 'email':
      await page.getByRole('textbox', { name: 'Receiving domain' }).fill('agents.example.test');
      await button('Save domain').press('Enter');
      await button('Generate secret').press('Enter');
      const secret = dialog('Save endpoint secret');
      await secret.waitFor();
      assert.equal(await page.locator('[data-private-token]').inputValue(), proof);
      await button('Done').press('Enter');
      await secret.waitFor({ state: 'hidden' });
      assert.equal(await page.locator('[data-private-token]').count(), 0);
      await page.reload();
      await button('Rotate secret').waitFor();
      assert.equal(await page.locator('[data-private-token]').count(), 0);
      break;
    case 'mcp':
      await button('Menu Fixture connection').press('Enter');
      await page.getByRole('menuitem', { name: 'Disable Fixture connection' }).press('Enter');
      await page.getByText('Inactive', { exact: true }).waitFor();
      break;
    case 'vault': {
      await page.locator('.k-row__main').filter({ hasText: 'fixture-login' }).press('Enter');
      const editor = dialog('Edit fixture-login');
      const password = editor.getByLabel('Password', { exact: true });
      assert.equal(await password.inputValue(), '');
      assert.equal(await password.getAttribute('type'), 'password');
      await page.keyboard.press('Escape');
      await editor.waitFor({ state: 'hidden' });
      break;
    }
  }
}

let browser;
try {
  if (output) await mkdir(output, { recursive: true });
  const origin = await Promise.race([ready, wait(20000, undefined, { ref: false }).then(() => { throw new Error('Server readiness timed out'); })]);
  assert.equal((await fetch(origin + '/settings/craft-anything')).status, 404);
  browser = await chromium.launch();
  const receipts = {};
  for (const client of ['leptos']) {
    receipts[client] = {};
    for (const [slug, title, content] of routes) {
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, reducedMotion: 'reduce' });
      const page = await context.newPage();
      page.setDefaultTimeout(12000);
      const errors = [];
      page.on('pageerror', error => errors.push(error.message));
      page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
      await page.addInitScript(() => {
        window.settingsCsp = [];
        document.addEventListener('securitypolicyviolation', event => window.settingsCsp.push(event.violatedDirective));
      });
      const fixture = fixtureRequests();
      await page.route('**/api/**', fixture.route);
      const path = showFor(client, slug);
      const response = await page.goto(origin + path);
      assert.equal(response.status(), 200);
      assert.equal((await response.headerValue('content-security-policy')).includes('wasm-unsafe-eval'), true);
      const panel = page.getByRole('dialog', { name: 'Settings', exact: true });
      const heading = panel.getByRole('heading', { level: 1, name: title, exact: true });
      await focused(page, heading, client);
      assert.equal(await panel.getByRole('button', { name: 'Craft Anything', exact: true }).count(), 0);
      await panel.getByText(content, { exact: true }).first().waitFor();
      for (const [size, viewport] of [['desktop', { width: 1440, height: 900 }], ['narrow', { width: 430, height: 850 }]]) {
        await page.setViewportSize(viewport);
        for (const theme of ['dark', 'light']) {
          await page.evaluate(async theme => {
            document.documentElement.dataset.theme = theme;
            await document.fonts.ready;
          }, theme);
          assert(await page.locator('.app-settings__main').evaluate(main => main.scrollWidth <= main.clientWidth + 1), 'Settings content overflows.');
          if (output) await parityShot(page, join(output, [slug, client, size, theme].join('-') + '.png'));
        }
      }
      await page.setViewportSize({ width: 1440, height: 900 });
      try {
        await exercise(page, slug);
      } catch (error) {
        assert.deepEqual(errors, [], client + ' ' + slug + ' console');
        throw new Error(client + ' ' + slug + ' interaction failed', { cause: error });
      }
      receipts[client][slug] = fixture.posts;
      assert.deepEqual(await page.evaluate(() => window.settingsCsp), [], client + ' ' + slug + ' CSP');
      assert.deepEqual(errors, [], client + ' ' + slug + ' console');
      console.log(client + ' ' + slug + ' passed');
      await context.close();
    }
  }
  console.log('Eight settings pages passed with four viewport/theme captures per page.');
} finally {
  await browser?.close();
  if (server.pid && server.exitCode === null && server.signalCode === null) {
    const exited = once(server, 'exit');
    server.kill();
    if (!await Promise.race([exited, wait(2500, false, { ref: false })])) {
      server.kill('SIGKILL');
      await exited;
    }
  }
  await rm(dir, { recursive: true });
}
