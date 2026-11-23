import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import { startRustServer } from '../../rust-test-server.ts';

const output = await mkdtemp(join(tmpdir(), 'minarbot-discord-smoke-'));
const data = await mkdtemp(join(tmpdir(), 'minarbot-discord-state-'));
const agent = '40000000-0000-4000-8000-000000000080';
const project = '40000000-0000-4000-8000-000000000081';
const section = '40000000-0000-4000-8000-000000000082';
const disconnected = { generation: 'fixture', bot: null, status: 'disconnected', routes: [], deliveries: [] };
const discordPath = '/api/connected-apps/discord';
let app, browser;
try {
  app = await startRustServer({ database: join(data, 'state.sqlite'),
    dist: fileURLToPath(new URL('../../dist', import.meta.url)), defaultModel: 'fixture' });
  const { origin } = app;
  browser = await chromium.launch(process.env.CHROME_PATH ? { executablePath: process.env.CHROME_PATH } : {});
  const page = await browser.newPage({ viewport: { width: 980, height: 820 }, colorScheme: 'dark' });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.route('**/*', route => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
  const seed = await fetch(origin + '/api/agents', { method: 'POST', headers: { Origin: origin, 'Content-Type': 'application/json' }, body: JSON.stringify({ id: agent, name: 'Smoke agent', runtime: 'api', model: 'fixture', instructions: 'Fixture.' }) });
  assert.equal(seed.status, 201);
  let state = disconnected;
  await page.route('**/api/**', route => {
    const { pathname } = new URL(route.request().url());
    if (pathname === '/api/agents') return route.fulfill({ json: { agents: [{ id: agent, name: 'Smoke agent', runtime: 'api' }] } });
    if (pathname === '/api/projects') return route.fulfill({ json: { projects: [{ id: project, name: 'Smoke project', archived: false, sections: [{ id: section, name: 'General', archived: false }] }] } });
    if (pathname === discordPath) return route.fulfill({ json: state });
    if (!pathname.startsWith(discordPath + '/')) return route.continue();
    const body = route.request().postDataJSON();
    switch (pathname.slice(discordPath.length + 1)) {
      case 'connect':
        if (body.token === 'invalid') return route.fulfill({ status: 400, json: { error: 'Token rejected' } });
        assert(body.token === 'synthetic.discord.fixture-key', 'Unexpected fixture token');
        state = { ...state, generation: body.id, bot: '1234', status: 'connected' };
        break;
      case 'add-route':
        assert.deepEqual(body.users, ['9002', '9003']);
        assert.equal(body.target, body.channel === '10001' ? agent : section);
        state = { ...state, routes: [...state.routes, body] };
        break;
      case 'remove-route':
        state = { ...state, routes: state.routes.filter(row => row.id !== body.route) };
        break;
      case 'disconnect':
        state = { ...disconnected, generation: body.id };
        break;
      default:
        assert.fail('Unexpected Discord fixture mutation');
    }
    return route.fulfill({ json: state });
  });
  await page.goto(origin + '/settings/connected-apps');
  await page.locator('#leptos-root').waitFor({ state: 'attached' });
  const discord = page.locator('section.k-section').filter({ has: page.getByRole('heading', { name: 'Discord', exact: true }) });
  const token = discord.getByLabel('Bot token');
  await token.waitFor();
  assert.equal(await token.getAttribute('type'), 'password');
  await token.fill('invalid');
  await token.press('Enter');
  await discord.getByRole('alert').filter({ hasText: 'Discord could not connect.' }).waitFor();
  assert.equal(await token.inputValue(), '');
  await token.fill('synthetic.discord.fixture-key');
  await token.press('Enter');
  await discord.getByText('Connected', { exact: true }).waitFor();
  assert.equal(await token.count(), 0);
  assert.equal(await page.evaluate(() => JSON.stringify({ ...localStorage, ...sessionStorage }).includes('synthetic.discord.fixture-key')), false);

  async function add(channel, label) {
    await discord.getByRole('button', { name: 'Add route' }).click();
    await discord.getByLabel('Discord channel, thread, or DM ID').fill(channel);
    await discord.getByLabel('Allowed Discord user IDs').fill('9002, 9003');
    await discord.getByRole('combobox', { name: 'Destination' }).selectOption({ label });
    await discord.getByRole('button', { name: 'Save route' }).click();
    await discord.getByText(channel + ' · 2 allowed users').waitFor();
  }
  await add('10001', 'Smoke agent');
  await add('10002', 'Smoke project / General');
  await discord.getByRole('button', { name: 'Add route' }).click();
  await page.screenshot({ path: join(output, 'discord-settings-dark.png'), fullPage: true });
  await page.evaluate(() => { document.documentElement.dataset.theme = 'light'; });
  await page.screenshot({ path: join(output, 'discord-settings-light.png'), fullPage: true });
  await discord.getByRole('button', { name: 'Cancel', exact: true }).click();
  await discord.getByRole('button', { name: 'Remove route 10001' }).click();
  await discord.getByText('10001 · 2 allowed users').waitFor({ state: 'detached' });
  await discord.getByRole('button', { name: 'Disconnect', exact: true }).click();
  await page.getByRole('alertdialog').getByRole('button', { name: 'Disconnect' }).click();
  await discord.getByText('Disconnected', { exact: true }).waitFor();
  assert.deepEqual(errors, []);
  console.log('Discord Settings smoke passed. Screenshots:', output);
} finally {
  await browser?.close();
  await app?.close();
  await rm(data, { recursive: true });
}
