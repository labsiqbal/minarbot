import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const binary = resolve(process.env.MINARBOT_RUST_BINARY ?? join(root, 'target/debug/minarbot-server'));
const output = process.env.MINARBOT_PROVIDER_SCREENSHOTS ?? await mkdtemp(join(tmpdir(), 'minarbot-provider-shots-'));
await mkdir(output, { recursive: true });
const data = await mkdtemp(join(tmpdir(), 'minarbot-provider-data-'));
const server = spawn(binary, ['--test-config'], { env: { ...process.env, MINARBOT_TEST_SERVER: 'rust' }, stdio: ['pipe', 'pipe', 'inherit'] });
const ready = new Promise((resolveReady, reject) => {
  const lines = createInterface({ input: server.stdout });
  lines.on('line', line => { if (line.startsWith('Minarbot: ')) resolveReady(line.slice('Minarbot: '.length)); });
  server.once('error', reject);
  server.once('exit', code => reject(new Error(`Minarbot stopped before readiness (${code})`)));
});
server.stdin.end(JSON.stringify({ database: join(data, 'state.sqlite'), dist: join(root, 'app/dist'), port: 0 }));

const codex = { id: 'codex-smoke', kind: 'codex', displayName: 'Smoke Codex', enabled: true, installed: true, version: '1.2.3', status: 'installed', auth: { status: 'authenticated', method: 'ChatGPT' }, binaryPath: '/usr/bin/codex', homePath: '/tmp/codex-home', shadowHomePath: '/tmp/codex-shadow', launchArgs: ['--serve'], environment: [{ name: 'TOKEN', value: '', secret: true, valueRedacted: true }, { name: 'MODE', value: 'fast', secret: false, valueRedacted: false }], revision: 1, models: [] };
const claude = { id: 'claude-smoke', kind: 'claude', displayName: 'Smoke Claude', enabled: false, installed: true, version: '2.0.0', status: 'disabled', auth: { status: 'unauthenticated', reason: 'Disabled' }, binaryPath: '/usr/bin/claude', homePath: '/tmp/claude-home', shadowHomePath: '', launchArgs: [], environment: [], revision: 1, models: [] };
const providers = [codex, claude];
const requests = [];

function response(route, body, status = 200) { return route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) }); }
function envelope() { return { providers: providers.map(provider => ({ ...provider, environment: provider.environment.map(value => value.secret ? { ...value, value: '', valueRedacted: true } : value) })) }; }

const browser = await chromium.launch();
try {
  const origin = await ready;
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.route('**/api/**', async route => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (request.method() === 'GET' && path === '/api/provider-instances') return response(route, envelope());
    if (request.method() === 'GET' && path.endsWith('/models')) return response(route, { models: ['gpt-smoke'], catalog: [{ id: 'gpt-smoke', label: 'Smoke model', options: [{ id: 'reasoningEffort', type: 'string', values: ['low', 'high'] }, { id: 'serviceTier', type: 'string', values: ['auto', 'fast'] }] }] });
    if (request.method() !== 'POST') return route.continue();
    const body = request.postDataJSON() ?? {};
    requests.push({ path, body });
    if (path === '/api/provider-instances') {
      const created = { ...codex, ...body, id: body.instanceId, revision: 1, auth: { status: 'unauthenticated', reason: 'No sign-in found in this instance home.' } };
      providers.push(created);
      return response(route, created, 201);
    }
    const match = path.match(/^\/api\/provider-instances\/([^/]+)\/(update|refresh|delete|sign-in)$/);
    if (!match) return response(route, { error: 'Unexpected provider mutation.' }, 400);
    const provider = providers.find(item => item.id === match[1]);
    if (!provider) return response(route, { error: 'Provider instance not found.' }, 404);
    if (match[2] === 'delete') {
      providers.splice(providers.indexOf(provider), 1);
      return response(route, { deleted: true });
    }
    if (match[2] === 'update') {
      Object.assign(provider, body, { revision: provider.revision + 1 });
      if (body.environment) provider.environment = body.environment.map(value => value.secret && value.valueRedacted ? { ...value, value: 'encrypted-secret' } : value);
      return response(route, provider);
    }
    if (match[2] === 'sign-in') return response(route, { started: true, provider: provider.id }, 202);
    return response(route, envelope());
  });

  await page.goto(`${origin}/settings/providers`);
  await page.getByRole('heading', { name: 'Providers', exact: true }).waitFor();
  await page.evaluate(() => { document.documentElement.dataset.theme = 'light'; });
  await page.screenshot({ path: join(output, 'providers-light.png'), animations: 'disabled', fullPage: true });
  await page.evaluate(() => { document.documentElement.dataset.theme = 'dark'; });
  await page.screenshot({ path: join(output, 'providers-dark.png'), animations: 'disabled', fullPage: true });
  await page.getByRole('button', { name: /Smoke Claude/ }).click();
  await page.getByRole('switch', { name: 'Enable Smoke Claude', exact: true }).click();
  await page.getByLabel('Display name', { exact: true }).fill('Smoke Claude Updated');
  await page.getByRole('button', { name: 'Add variable', exact: true }).click();
  const names = page.getByLabel('Variable name');
  await names.last().fill('API_TOKEN');
  await page.getByLabel('Value for API_TOKEN', { exact: true }).fill('fixture-secret');
  await page.getByLabel('Secret', { exact: true }).last().check();
  const updateRequest = page.waitForRequest(request => request.url().endsWith('/api/provider-instances/claude-smoke/update'));
  await page.getByRole('button', { name: 'Save changes', exact: true }).click();
  await updateRequest;
  const save = requests.findLast(item => item.path.endsWith('/update'));
  assert(save, 'provider update was not sent');
  assert(!JSON.stringify(envelope()).includes('encrypted-secret'), 'secret echoed in provider response');
  await page.getByRole('button', { name: 'Remove variable', exact: true }).last().click();
  await page.waitForTimeout(250);
  await page.getByRole('button', { name: 'Delete instance', exact: true }).last().click();
  await page.getByRole('alertdialog').getByRole('button', { name: 'Delete instance', exact: true }).click();
  await page.getByRole('button', { name: 'Add provider', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Add provider', exact: true });
  await dialog.getByText('1').waitFor();
  await page.screenshot({ path: join(output, 'provider-wizard-step-1-dark.png'), animations: 'disabled' });
  await page.evaluate(() => { document.documentElement.dataset.theme = 'light'; });
  await page.screenshot({ path: join(output, 'provider-wizard-step-1-light.png'), animations: 'disabled' });
  await page.evaluate(() => { document.documentElement.dataset.theme = 'dark'; });
  await dialog.getByRole('button', { name: 'Continue', exact: true }).click();
  await page.screenshot({ path: join(output, 'provider-wizard-step-2-dark.png'), animations: 'disabled' });
  await page.evaluate(() => { document.documentElement.dataset.theme = 'light'; });
  await page.screenshot({ path: join(output, 'provider-wizard-step-2-light.png'), animations: 'disabled' });
  await page.evaluate(() => { document.documentElement.dataset.theme = 'dark'; });
  await dialog.getByLabel('Display name', { exact: true }).fill('Smoke New Codex');
  await dialog.getByRole('button', { name: 'Continue', exact: true }).click();
  await page.screenshot({ path: join(output, 'provider-wizard-step-3-dark.png'), animations: 'disabled' });
  await page.evaluate(() => { document.documentElement.dataset.theme = 'light'; });
  await page.screenshot({ path: join(output, 'provider-wizard-step-3-light.png'), animations: 'disabled' });
  await page.evaluate(() => { document.documentElement.dataset.theme = 'dark'; });
  await dialog.getByRole('button', { name: 'Configure manually', exact: true }).click();
  await dialog.waitFor({ state: 'hidden' });
  await page.getByRole('button', { name: 'Add provider', exact: true }).click();
  const signInDialog = page.getByRole('dialog', { name: 'Add provider', exact: true });
  await signInDialog.getByRole('button', { name: 'Continue', exact: true }).click();
  await signInDialog.getByLabel('Display name', { exact: true }).fill('Smoke Sign In');
  await signInDialog.getByRole('button', { name: 'Continue', exact: true }).click();
  await signInDialog.getByRole('button', { name: 'Continue with ChatGPT', exact: true }).click();
  await signInDialog.waitFor({ state: 'hidden' });
  assert(requests.some(item => item.path.endsWith('/sign-in')), 'sign-in request was not sent');
} finally {
  await browser.close();
  server.kill();
}
