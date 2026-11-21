import assert from 'node:assert/strict';
import { mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import { createServer } from 'vite';

const root = fileURLToPath(new URL('../../', import.meta.url));
const output = await mkdtemp(join(tmpdir(), 'minarbot-discord-smoke-'));
const fixture = `<!doctype html>
<html lang="en" data-theme="dark" data-color-theme="minarbot">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Connected apps</title>
<div id="root"></div>
<script type="module">
  import { createElement } from 'react';
  import { createRoot } from 'react-dom/client';
  import { ConnectedAppsPanel } from '/src/ConnectedAppsPanel.tsx';
  import { ConfirmProvider } from '/src/ui/kit.tsx';
  import '/src/ui/tokens.css';
  import '/src/ui/base.css';
  import '/src/ui/kit.css';
  import '@fontsource-variable/inter';

  let state = { generation: '', bot: null, status: 'disconnected', routes: [], deliveries: [] };
  async function request(path, data) {
    switch (path) {
      case '/api/connected-apps/discord': return state;
      case '/api/connected-apps/discord/connect':
        if (data?.token?.length < 24) throw new Error('Token rejected');
        state = { ...state, generation: data.id, bot: '1234', status: 'connected' };
        return state;
      case '/api/connected-apps/discord/add-route':
        state = { ...state, routes: [...state.routes, { id: data.id, channel: data.channel, kind: data.kind, target: data.target, users: data.users }] };
        return state;
      case '/api/connected-apps/discord/remove-route':
        state = { ...state, routes: state.routes.filter(route => route.id !== data.route) };
        return state;
      case '/api/connected-apps/discord/disconnect':
        state = { generation: data.id, bot: null, status: 'disconnected', routes: [], deliveries: [] };
        return state;
      case '/api/projects':
        return { projects: [{ id: 'project', name: 'Smoke project', sections: [{ id: 'section', name: 'General' }] }] };
      case '/api/connected-apps/key': return { source: 'none', revision: 0 };
      default: return path.includes('?') ? { configured: false, items: [], categories: [] } : { items: [] };
    }
  }
  createRoot(document.getElementById('root')).render(createElement(ConfirmProvider, null,
    createElement(ConnectedAppsPanel, { agents: [{ id: 'agent', name: 'Smoke agent' }], request })));
</script>`;
const server = await createServer({
  root,
  server: { host: '127.0.0.1', port: 0, hmr: false },
  plugins: [{
    name: 'discord-settings-fixture',
    configureServer(server) {
      server.middlewares.use('/discord-settings.html', async (_request, response) => {
        response.setHeader('Content-Type', 'text/html');
        response.end(await server.transformIndexHtml('/discord-settings.html', fixture));
      });
    },
  }],
});
let browser;
try {
  await server.listen(0);
  const address = server.httpServer.address();
  assert(address && typeof address === 'object');
  const origin = 'http://127.0.0.1:' + address.port;
  browser = await chromium.launch(process.env.CHROME_PATH ? { executablePath: process.env.CHROME_PATH } : {});
  const page = await browser.newPage({ viewport: { width: 980, height: 820 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.route('**/*', route => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
  await page.goto(origin + '/discord-settings.html');
  const discord = page.getByRole('region', { name: 'Discord', exact: true });
  const token = discord.getByLabel('Bot token');
  await token.waitFor();
  assert.equal(await token.getAttribute('type'), 'password');
  await token.fill('invalid');
  await token.press('Enter');
  await discord.getByRole('alert').getByText('Discord could not connect.', { exact: false }).waitFor();
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
    await discord.getByRole('combobox', { name: 'Destination' }).press('Enter');
    await page.getByRole('option', { name: label }).click();
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
  await server.close();
}
