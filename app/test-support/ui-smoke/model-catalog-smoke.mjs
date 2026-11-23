import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as wait } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import { startRustServer } from '../../rust-test-server.ts';

const data = await mkdtemp(join(tmpdir(), 'minarbot-model-catalog-'));
const runtime = (runtime, label, extra = {}) => ({ runtime, label, available: true, enabled: true, signedIn: true, models: [], ...extra });
const codex = runtime('codex', 'Codex');
const claude = runtime('claude', 'Claude Code');
const signedOut = { ...codex, signedIn: false };
const hermes = runtime('hermes', 'Hermes', { models: [{ id: 'host-model' }] });
const login = { runtime: 'codex', id: 'fixture-login', name: 'Work login', signedIn: true };
const api = { id: 'fixture-api', name: 'Work API', configured: true, defaultModel: 'saved-api-model', provider: 'anthropic' };
const openai = { ...api, id: 'fixture-openai', name: 'Work OpenAI', provider: 'openai-compatible' };
const creation = (id, runtime, providerAccount, model) => ({ id, providerAccount, name: 'Fixture agent', role: '', model, backupModel: '',
  instructions: 'Help the user complete their tasks.', threadLimit: 2, toolBudget: 90, runtime });
let app, browser;
try {
  app = await startRustServer({ appearanceClient: 'leptos', database: join(data, 'state.sqlite'),
    dist: fileURLToPath(new URL('../../dist', import.meta.url)), defaultModel: 'fixture' });
  browser = await chromium.launch(process.env.CHROME_PATH ? { executablePath: process.env.CHROME_PATH } : {});
  const page = await browser.newPage();
  const name = page.getByRole('textbox', { name: 'Name', exact: true });
  const modelInput = page.getByLabel('Model', { exact: true });
  const picker = page.getByRole('combobox', { name: 'Run with', exact: true });
  const status = page.locator('.first-run__status');
  let fixture, created, saved, catalogPath;
  const oldCatalog = Promise.withResolvers();
  await page.route('**/*', route => new URL(route.request().url()).origin === app.origin ? route.continue() : route.abort());
  await page.route('**/api/**', async route => {
    const request = route.request(), url = new URL(request.url());
    switch (url.pathname) {
      case '/api/agents':
        if (request.method() === 'GET') return route.fulfill({ json: { agents: [], guide: null } });
        created = request.postDataJSON();
        return route.fulfill({ status: 503, json: { error: 'Fixture retains the first-run surface for retries.' } });
      case '/api/runtimes': return route.fulfill({ json: { choices: fixture.choices } });
      case '/api/engine-logins': return route.fulfill({ json: fixture.logins ?? [] });
      case '/api/provider-accounts/test': return route.fulfill({ json: { models: ['tested-model'] } });
      case '/api/provider-accounts':
        if (request.method() === 'GET') return route.fulfill({ json: fixture.accounts ?? [] });
        const { id, name, provider, defaultModel } = request.postDataJSON();
        saved = id;
        return route.fulfill({ json: { id, name, provider, defaultModel, configured: true } });
      case '/api/models': {
        catalogPath = url.pathname + '?' + url.searchParams.toString();
        const selected = url.searchParams.get('runtime');
        const preferred = selected === 'hermes' ? 'host-model' : selected + '-preferred';
        if (fixture.holdCodex && selected === 'codex') await oldCatalog.promise;
        return fixture.unavailable ? route.fulfill({ status: 503, json: { error: 'Catalog unavailable' } })
          : route.fulfill({ json: { models: [preferred, selected + '-other'] } });
      }
      default: return route.continue();
    }
  });
  async function mount(value) {
    fixture = value;
    created = saved = catalogPath = undefined;
    if (page.url() !== 'about:blank') await page.evaluate(pending => {
      sessionStorage.removeItem('minarbot-agent-creation');
      if (pending) sessionStorage.setItem('minarbot-agent-creation', JSON.stringify(pending));
    }, fixture.pending);
    await page.goto(app.origin);
    await page.getByRole('heading', { name: 'Set up Minarbot' }).waitFor();
  }
  async function expectModel(runtime, account, model) {
    const expected = '/api/models?' + new URLSearchParams({ runtime, account });
    await modelInput.waitFor();
    for (let i = 0; catalogPath !== expected && i < 100; i++) await wait(20);
    assert.equal(catalogPath, expected);
    await page.waitForFunction(model => document.querySelector('[placeholder="Enter model ID"]')?.value === model, model);
  }
  async function submit(runtime, account, model, pending) {
    if (!pending) await name.fill('Fixture agent');
    const button = page.getByRole('button', { name: pending ? 'Retry creation' : 'Create agent', exact: true });
    await Promise.all([page.waitForResponse(response => response.url().endsWith('/api/agents') && response.request().method() === 'POST'), button.click()]);
    assert.match(created.id, /^[0-9a-f-]{36}$/);
    assert.deepEqual(created, pending ?? creation(created.id, runtime, account, model));
  }

  for (const [value, runtime, account, model] of [
    [{ choices: [claude, codex, hermes], accounts: [api], logins: [login] }, 'codex', 'device', 'default'],
    [{ choices: [signedOut, claude, hermes], accounts: [api], logins: [login] }, 'claude', 'device', 'default'],
    [{ choices: [signedOut, hermes], logins: [login] }, 'codex', 'login:fixture-login', 'default'],
    [{ choices: [hermes] }, 'hermes', '', 'host-model'],
    [{ choices: [signedOut, hermes], accounts: [api, openai], logins: [login] }, 'codex', openai.id, 'codex-preferred'],
    [{ choices: [hermes], accounts: [openai] }, 'api', openai.id, 'api-preferred'],
  ]) {
    await mount(value);
    await expectModel(runtime, account, model);
    await submit(runtime, account, model);
  }

  for (const [choices, runtime] of [[[codex, claude], 'codex'], [[claude], 'api']]) {
    await mount({ choices });
    await page.getByRole('button', { name: 'Use an API key', exact: true }).click();
    await page.getByRole('textbox', { name: 'Name', exact: true }).fill('Saved provider');
    await page.getByLabel('API key', { exact: true }).fill('fixture-key');
    await page.getByRole('button', { name: 'Test', exact: true }).click();
    await page.getByRole('button', { name: 'Save', exact: true }).click();
    await expectModel(runtime, saved, runtime + '-preferred');
    await submit(runtime, saved, runtime + '-preferred');
  }

  await mount({ choices: [codex] });
  await expectModel('codex', 'device', 'default');
  assert.equal(await picker.isVisible(), false);
  assert.equal(await status.textContent(), 'Codex');

  const three = { choices: [codex, claude], accounts: [api] };
  await mount(three);
  await expectModel('codex', 'device', 'default');
  assert.deepEqual(await picker.locator('option').allTextContents(), ['Codex', 'Claude Code', 'Work API']);
  await name.fill('Fixture agent');
  await modelInput.fill('codex-override');
  await picker.press('ArrowDown');
  await expectModel('claude', 'device', 'default');
  assert.equal(await status.textContent(), 'Claude Code');
  assert.equal(await name.inputValue(), 'Fixture agent');
  await submit('claude', 'device', 'default');

  await mount(three);
  await picker.selectOption({ label: api.name });
  await expectModel('api', api.id, 'api-preferred');
  const suggested = await modelInput.getAttribute('list');
  assert.deepEqual(await page.locator('#' + suggested + ' option').evaluateAll(rows => rows.map(row => row.value)), ['api-preferred', 'api-other']);
  await modelInput.fill('api-other');
  await submit('api', api.id, 'api-other');

  await mount({ ...three, holdCodex: true });
  await expectModel('codex', 'device', 'default');
  await picker.selectOption({ label: api.name });
  await expectModel('api', api.id, 'api-preferred');
  await Promise.all([
    page.waitForResponse(response => new URL(response.url()).searchParams.get('runtime') === 'codex'),
    oldCatalog.resolve(),
  ]);
  assert.equal(await modelInput.inputValue(), 'api-preferred');
  assert.deepEqual(await page.locator('#' + suggested + ' option').evaluateAll(rows => rows.map(row => row.value)), ['api-preferred', 'api-other']);

  await mount({ ...three, unavailable: true });
  await expectModel('codex', 'device', 'default');
  assert.equal(await modelInput.getAttribute('placeholder'), 'Enter model ID');
  assert.equal(await page.locator('#' + suggested + ' option').count(), 0);
  await modelInput.fill('custom/model');
  await submit('codex', 'device', 'custom/model');

  await mount({ choices: [codex, claude, hermes, runtime('grok', 'Grok', { enabled: false })],
    accounts: [api, { ...openai, configured: false }], logins: [{ ...login, name: api.name }, { ...login, id: 'signed-out', signedIn: false }] });
  await expectModel('codex', 'device', 'default');
  assert.deepEqual(await picker.locator('option').allTextContents(), ['Codex', 'Claude Code', 'Work API', 'Work API', 'Hermes']);
  assert.equal(await picker.locator('option').filter({ hasText: 'Grok' }).count(), 0);

  const pending = creation('9b3d8d00-a5cf-45d6-9130-2b883971cdb1', 'claude', 'device', 'recovered-model');
  await mount({ ...three, pending });
  await modelInput.waitFor();
  assert.equal(await modelInput.inputValue(), pending.model);
  assert(await picker.isDisabled());
  await submit('claude', 'device', pending.model, pending);
  if (process.env.FIRST_RUN_SCREENSHOT) await page.screenshot({ path: process.env.FIRST_RUN_SCREENSHOT, animations: 'disabled' });
  console.log('Leptos setup verified runtime and account defaults, model options, editable fallback and exact recovery retry.');
} finally {
  await browser?.close();
  await app?.close();
  await rm(data, { recursive: true });
}
