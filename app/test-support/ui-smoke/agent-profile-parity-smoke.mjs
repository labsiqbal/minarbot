import assert from 'node:assert/strict';
import { assertP95 } from './timing-budgets.mjs';
import { randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir, homedir } from 'node:os';
import { join, resolve } from 'node:path';
import { chromium } from 'playwright-core';
import { startApp } from '../../test-bridge.ts';
import { testDatabase } from '../database.ts';
import { deleteAgentSmoke } from './delete-agent-smoke.mjs';
import { fixDate, fixtureClock, parityChromiumArgs, parityShot, withFixtureText } from './parity-shot.mjs';

const output = resolve(process.argv[2] ?? 'test-support/ui-smoke/artifacts/agent-profile');
const scratch = await mkdtemp(join(tmpdir(), 'minarbot-agent-profile-'));
const folder = await mkdtemp(join(homedir(), '.minarbot-agent-profile-'));
await mkdir(output, { recursive: true });
const sizes = { desktop: { width: 1440, height: 900 }, narrow: { width: 390, height: 844 } };
const clients = ['leptos'];
const samples = Number(process.env.MINARBOT_AGENT_BENCH ?? 0);
assert(Number.isSafeInteger(samples) && samples >= 0);
const agentId = '0035128a-b125-450c-a39c-1b678657d2ec';
const templateFile = {
  name: 'fixture.json', mimeType: 'application/json', buffer: Buffer.from(JSON.stringify({
    format: 'minarbot-agent', version: 1, name: 'Imported fixture', model: 'fixture',
    instructions: 'Help the user.', threadLimit: 2, libraryDocsAccess: false,
  })),
};
const provider = createServer(async (request, response) => {
  for await (const chunk of request) void chunk;
  response.setHeader('Content-Type', 'application/json');
  response.end(JSON.stringify(request.method === 'GET' ? { data: [
    { id: 'fixture', context_length: 128_000, pricing: { prompt: '0.000003', completion: '0.000015' } },
    { id: 'alternate', context_length: 128_000, pricing: { prompt: '0.000001', completion: '0.000005' } },
  ] }
    : { choices: [{ finish_reason: 'stop', message: { content: 'Drafted fixture profile.' } }] }));
});
await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));

const summary = values => {
  const sorted = [...values].sort((a, b) => a - b);
  const at = fraction => sorted[Math.ceil(sorted.length * fraction) - 1];
  return { median: at(0.5), p95: at(0.95), samples: sorted.length };
};
const button = (scope, name) => scope.getByRole('button', { name, exact: true });
const dialog = (page, name) => page.getByRole('dialog', { name, exact: true });
const menu = (page, name) => page.getByRole('menuitem', { name, exact: true });
const agentRow = (page, name) => page.locator('.sidebar-agent > .bot-row').filter({ has: page.locator('.bot-row__name', { hasText: name }) });

async function openSidebar(page) {
  const expand = button(page, 'Expand sidebar');
  await page.locator('.sidebar[aria-label="Workspace navigation"]:visible').or(expand).first().waitFor();
  if (await expand.isVisible()) await expand.click();
}
async function createMenu(page, label) {
  await openSidebar(page);
  await page.getByLabel('Create new', { exact: true }).click();
  const option = menu(page, label);
  if (await option.count()) await option.click();
  else await button(page, label).click();
}
async function agentMenu(page, name, action) {
  await openSidebar(page);
  await agentRow(page, name).waitFor();
  await agentRow(page, name).hover();
  await button(page, `Actions for ${name}`).click();
  await menu(page, action).click();
}
async function profile(page, name = 'Lifecycle fixture') {
  await agentMenu(page, name, 'Edit');
  const panel = dialog(page, 'Agent profile');
  await panel.waitFor();
  return panel;
}
async function close(page) { await page.keyboard.press('Escape'); }
function settingsFixture(page) {
  let status = { available: true, enabled: true, alwaysOn: false, sharedProfile: false, state: 'off', revision: 1, requests: [] };
  return page.route(`**/api/agents/${agentId}/computer`, async route => {
    if (route.request().method() === 'POST') {
      const { id, revision, ...change } = route.request().postDataJSON();
      assert.match(id, /^[a-f0-9-]{36}$/);
      assert.equal(revision, status.revision);
      status = { ...status, ...change, revision: revision + 1 };
    }
    await route.fulfill({ json: status });
  });
}

async function captures(page, prefix) {
  const shot = async (name, panel) => {
    // The mouse position from opening a layer can overlap a field in the new layer.
    await page.mouse.move(0, 0);
    const displayed = [scratch, folder].map(path => [path, path.replace(/-[a-zA-Z0-9]{6}$/, '-ZuiYSz')]);
    await withFixtureText(page, displayed, () => parityShot(page, join(output, `${prefix}-${name}.png`), panel));
  };
  await createMenu(page, 'New agent');
  const create = dialog(page, 'New agent');
  await shot('new', create);
  await create.getByRole('radio', { name: 'Draft with AI', exact: true }).click();
  await shot('draft', create);
  await button(create, 'Change').click();
  const avatar = dialog(page, 'Avatar');
  await avatar.getByRole('radio', { name: 'Bunny', exact: true }).click();
  await avatar.getByRole('radio', { name: 'Teal', exact: true }).click();
  await shot('look', avatar);
  await close(page);
  await close(page);
  await createMenu(page, 'Import template');
  const importPanel = dialog(page, 'Import template');
  await shot('import', importPanel);
  const [fileChooser] = await Promise.all([
    page.waitForEvent('filechooser'),
    button(importPanel, 'Choose file').click(),
  ]);
  await fileChooser.setFiles(templateFile);
  await importPanel.getByRole('heading', { name: 'New profile preview' }).waitFor();
  // Focus the editable preview after the browser returns from the file picker.
  await importPanel.getByRole('textbox', { name: 'Name', exact: true }).click();
  await shot('import-preview', importPanel);
  await close(page);

  const edit = await profile(page);
  await page.waitForFunction(() => {
    const field = document.activeElement;
    return field instanceof HTMLInputElement && field.name === 'name'
      && field.selectionStart === 0 && field.selectionEnd === field.value.length;
  });
  await shot('profile', edit);
  const change = button(edit, 'Change');
  await change.click();
  await avatar.getByText('Current avatar', { exact: true }).waitFor();
  await shot('avatar', avatar);
  await close(page);
  await page.waitForFunction(node => document.activeElement === node, await change.elementHandle());
  await edit.getByRole('tab', { name: 'Profile', exact: true }).press('ArrowRight');
  await edit.getByRole('tab', { name: 'Model', exact: true, selected: true }).waitFor();
  const provider = edit.getByRole('combobox', { name: 'Provider instance', exact: true });
  await provider.waitFor();
  await provider.scrollIntoViewIfNeeded();
  await shot('model', edit);
  await edit.getByRole('tab', { name: 'Advanced', exact: true }).click();
  await edit.getByRole('switch', { name: 'Computer', exact: true }).waitFor();
  await edit.getByText('Working folder', { exact: true }).waitFor();
  await shot('advanced', edit);
  await button(edit, 'Agent sites').scrollIntoViewIfNeeded();
  await shot('advanced-resources', edit);
  await button(edit, 'Choose').click();
  await shot('folder', dialog(page, 'Choose working folder'));
  await close(page);
  await button(edit, 'Agent sites').click();
  const sites = dialog(page, 'Agent sites');
  await sites.getByText('No sites', { exact: true }).waitFor();
  await shot('sites', sites);
  await button(sites, 'Create site').click();
  await shot('site-editor', dialog(page, 'New site'));
  await close(page);
  await close(page);
  await button(edit, 'Agent profile actions').click();
  await menu(page, 'Export template').click();
  const template = dialog(page, 'Export template');
  await button(template, 'Preview export template').click();
  await template.getByRole('region', { name: 'File contents to download' }).waitFor();
  await shot('export', template);
  await close(page);
  await button(edit, 'Agent profile actions').click();
  await menu(page, 'Hide agent').click();
  await shot('availability', dialog(page, 'Availability'));
  await close(page);
  await button(edit, 'Agent profile actions').click();
  await menu(page, 'Delete agent').click();
  await shot('delete', page.getByRole('alertdialog'));
  await close(page);
  await close(page);
}

async function mutations(page) {
  const requests = [];
  page.on('request', request => {
    const path = new URL(request.url()).pathname;
    if (request.method() === 'POST' && path.startsWith('/api/agents')) {
      const { id, ...payload } = request.postDataJSON();
      assert.match(id, /^[a-f0-9-]{36}$/);
      requests.push({ path, payload });
    }
  });
  const edit = await profile(page);
  await edit.getByRole('textbox', { name: 'Name', exact: true }).fill('Lifecycle updated');
  await edit.getByRole('tab', { name: 'Model', exact: true }).click();
  await edit.getByLabel('Provider instance', { exact: true }).selectOption('codex-test-gateway');
  await edit.locator('input[name=model]').fill('fixture');
  await edit.getByRole('switch', { name: 'Learn from conversations' }).click();
  await edit.getByText('Goal judge model', { exact: true }).click();
  await edit.getByLabel('Goal judge model').fill('alternate');
  await edit.getByRole('tab', { name: 'Advanced', exact: true }).click();
  assert.equal(await edit.getByRole('switch', { name: 'Library docs', exact: true }).count(), 0);
  await button(edit, 'Save changes').click();
  await edit.waitFor({ state: 'hidden' });
  await agentRow(page, 'Lifecycle updated').waitFor();

  const updated = await profile(page, 'Lifecycle updated');
  await updated.getByRole('tab', { name: 'Advanced', exact: true }).click();
  await button(updated, 'Choose').click();
  const choose = dialog(page, 'Choose working folder');
  await choose.getByLabel('Absolute folder path').fill(folder);
  await button(choose, 'Save folder').click();
  await choose.waitFor({ state: 'hidden' });
  await updated.getByText(folder, { exact: true }).waitFor();
  await button(updated, 'Reset').click();
  await updated.getByText(folder, { exact: true }).waitFor({ state: 'hidden' });

  const computer = updated.getByRole('switch', { name: 'Always-on Computer', exact: true });
  await computer.click();
  await updated.getByRole('switch', { name: 'Always-on Computer', exact: true, checked: true }).waitFor();
  await button(updated, 'Agent sites').click();
  const sites = dialog(page, 'Agent sites');
  await button(sites, 'Create site').click();
  const editor = dialog(page, 'New site');
  await editor.getByLabel('Name').fill('Fixture site');
  await editor.getByLabel('HTML').fill('<h1>Fixture site</h1>');
  const published = page.waitForResponse(response => response.url().endsWith(`/api/agents/${agentId}/sites`) && response.request().method() === 'POST');
  await button(editor, 'Create site').click();
  assert((await published).ok());
  await button(editor, 'Create site').waitFor();
  await close(page);
  await sites.getByRole('link', { name: 'Open Fixture site', exact: true }).waitFor();
  await close(page);

  await updated.getByRole('tab', { name: 'Profile', exact: true }).click();
  await button(updated, 'Change').click();
  const avatar = dialog(page, 'Avatar');
  await avatar.getByRole('radio', { name: 'Bunny', exact: true }).click();
  await avatar.getByRole('radio', { name: 'Teal', exact: true }).click();
  await button(avatar, 'Save').click();
  await avatar.getByRole('img', { name: 'Current agent avatar' }).waitFor();
  await close(page);
  await close(page);

  await agentMenu(page, 'Lifecycle updated', 'Hide');
  await agentRow(page, 'Lifecycle updated').getByText('Hidden', { exact: true }).waitFor();
  const hidden = page.locator('.hidden-agents');
  await hidden.locator('.machine-group__header').click();
  await button(hidden, 'Restore Lifecycle updated').click();
  await agentRow(page, 'Lifecycle updated').getByText('Hidden', { exact: true }).waitFor({ state: 'hidden' });

  const [created] = await Promise.all([
    page.waitForResponse(response => response.url().endsWith('/api/agents') && response.request().method() === 'POST'),
    (async () => {
      await createMenu(page, 'New agent');
      const create = dialog(page, 'New agent');
      await create.getByRole('radio', { name: 'Draft with AI', exact: true }).click();
      await create.getByLabel(/Temporary name/).fill('Drafted agent');
      await create.getByLabel(/Brief/).fill('Draft a profile for scheduling.');
      await create.getByLabel('Provider instance', { exact: true }).selectOption('codex-test-gateway');
      await create.locator('input[name=model]').fill('fixture');
      await button(create, 'Create and draft').click();
    })(),
  ]);
  assert.equal(created.status(), 201);
  const { id: newAgent } = created.request().postDataJSON();
  await page.waitForURL(url => url.pathname === `/agents/${newAgent}`);
  await createMenu(page, 'Import template');
  const template = dialog(page, 'Import template');
  await template.locator('input[type=file]').setInputFiles(templateFile);
  await template.getByRole('heading', { name: 'New profile preview' }).waitFor();
  const imported = page.waitForResponse(response => response.url().endsWith('/api/agents') && response.request().method() === 'POST');
  await button(template, 'Create agent').click();
  const importResponse = await imported;
  assert.equal(importResponse.status(), 201);
  const { id: importedId } = importResponse.request().postDataJSON();
  await page.waitForURL(url => url.pathname === `/agents/${importedId}`);
  await template.waitFor({ state: 'hidden' });
  await agentMenu(page, 'Imported fixture', 'Delete');
  const deleted = page.waitForResponse(response => response.url().endsWith(`/api/agents/${importedId}/delete`));
  await button(page.getByRole('alertdialog'), 'Delete agent').click();
  assert.equal((await deleted).status(), 200);
  await agentRow(page, 'Imported fixture').waitFor({ state: 'hidden' });

  assert(requests.some(({ payload }) => payload.setup && payload.name === 'Drafted agent'));
  assert(requests.some(({ payload }) => payload.name === 'Imported fixture'));
  return requests.map(({ path, payload }) => ({
    path: path.replace(/\/api\/agents\/[a-f0-9-]{36}(?=\/)/g, '/api/agents/:id'),
    payload: { ...payload, ...(payload.image ? { image: 'PNG' } : {}) },
  }));
}

async function benchmark(browser, origin, client) {
  const route = `${origin}/agents/${agentId}`;
  const cold = [], warm = [];
  for (let index = 0; index < samples; index++) {
    const ctx = await browser.newContext({ viewport: sizes.desktop, reducedMotion: 'reduce' });
    try {
      const page = await ctx.newPage();
      const start = performance.now();
      await page.goto(route);
      await profile(page);
      cold.push(performance.now() - start);
      await close(page);
      for (let repeat = 0; repeat < 2; repeat++) {
        const row = agentRow(page, 'Lifecycle fixture');
        await row.hover();
        await button(page, 'Actions for Lifecycle fixture').click();
        const option = menu(page, 'Edit');
        const elapsed = await option.evaluate(control => new Promise((resolve, reject) => {
          const find = () => [...document.querySelectorAll('[role=dialog], dialog[open]')].some(node =>
            node.querySelector('h2')?.textContent === 'Agent profile' && node.getBoundingClientRect().height > 0);
          const began = performance.now();
          const observer = new MutationObserver(() => {
            if (find()) { observer.disconnect(); clearTimeout(timeout); resolve(performance.now() - began); }
          });
          observer.observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ['open'] });
          const timeout = setTimeout(() => { observer.disconnect(); reject(new Error('Profile did not render')); }, 5000);
          control.click();
        }));
        if (repeat === 1) warm.push(elapsed);
        await close(page);
      }
    } finally { await ctx.close(); }
  }
  return samples ? { cold: summary(cold), warm: summary(warm) } : {};
}

async function deletion(browser, origin, client) {
  const lead = randomUUID();
  const response = await fetch(origin + '/api/agents', {
    method: 'POST', headers: { Origin: origin, 'Content-Type': 'application/json' },
    body: JSON.stringify({ id: lead, providerInstance: 'codex-test-gateway', name: 'Smoke Lead', model: 'fixture', instructions: 'Help.' }),
  });
  assert.equal(response.status, 201, await response.text());
  const context = await browser.newContext({ viewport: sizes.desktop, reducedMotion: 'reduce' });
  try {
    const page = await context.newPage();
    await page.goto(`${origin}/agents/${lead}`);
    await openSidebar(page);
    await deleteAgentSmoke({ page, origin, client, evidence: join(output, `${client}-deletion`),
      check: async (name, run) => { await run(); console.log(client, name); },
    });
  } finally { await context.close(); }
}

let browser;
const receipt = {};
try {
  browser = await chromium.launch({ executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE, args: parityChromiumArgs });
  for (const client of clients) {
    const database = join(scratch, client + '.sqlite');
    const app = await startApp({
      database, dist: resolve('dist'), port: 0, defaultModel: 'fixture', baseURL: 'http://127.0.0.1:' + provider.address().port + '/v1', apiKey: 'synthetic', stream: false, projectRoot: scratch, fixedClock: fixtureClock,
    });
    try {
      const response = await fetch(app.origin + '/api/agents', {
        method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' },
        body: JSON.stringify({ id: agentId, providerInstance: 'codex-test-gateway', name: 'Lifecycle fixture', model: 'fixture', instructions: 'Help the user.' }),
      });
      assert.equal(response.status, 201, await response.text());
      // The frozen profile shows a migrated agent with no bound provider.
      const db = testDatabase(database);
      try {
        db.prepare("UPDATE agents SET provider_instance='' WHERE id=?").run(agentId);
      } finally { db.close(); }
      const timing = await benchmark(browser, app.origin, client);
      const errors = [];
      let commands;
      for (const [size, viewport] of Object.entries(sizes)) {
        for (const theme of ['dark', 'light']) {
          const context = await browser.newContext({ viewport, reducedMotion: 'reduce' });
          await context.addInitScript(theme => {
            localStorage.setItem('minarbot-presentation-v1', JSON.stringify({ version: 1, theme, textSize: 'standard' }));
            addEventListener('securitypolicyviolation', event => console.error('CSP', event.effectiveDirective, event.blockedURI));
          }, theme);
          const page = await context.newPage();
          await fixDate(page);
          page.on('pageerror', error => errors.push(error.message));
          page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
          await page.route('**/api/provider-instances*', async route => {
            if (route.request().method() !== 'GET') return route.continue();
            await route.fulfill({ json: { providers: [{ id: 'codex-test-gateway', instanceId: 'codex-test-gateway', kind: 'codex', displayName: 'Test gateway', enabled: true, installed: true, auth: { status: 'authenticated', method: 'ChatGPT' }, revision: 1, models: [{ id: 'fixture', label: 'fixture', options: [] }, { id: 'alternate', label: 'alternate', options: [] }] }] } });
          });
          await page.route('**/api/provider-instances/*/models', async route => {
            if (route.request().method() !== 'GET') return route.continue();
            await route.fulfill({ json: { models: ['fixture', 'alternate'], catalog: [{ id: 'fixture', label: 'fixture', options: [] }, { id: 'alternate', label: 'alternate', options: [] }] } });
          });
          await settingsFixture(page);
          const document = await page.goto(`${app.origin}/agents/${agentId}`);
          assert.equal((await document.headerValue('content-security-policy')).includes('wasm-unsafe-eval'), true);
          assert.equal(await page.locator('script[src*="/conversation-start."]').count(), 1);
          await openSidebar(page);
          await agentRow(page, 'Lifecycle fixture').waitFor();
          await captures(page, `${client}-${size}-${theme}`);
          if (size === 'narrow' && theme === 'light') {
            await page.setViewportSize(sizes.desktop);
            commands = await mutations(page);
          }
          await context.close();
        }
      }
      assert.deepEqual(errors, []);
      assert.deepEqual(commands, [
        {"path": "/api/agents/:id/edit", "payload": {"revision": 1, "webAccess": true, "learnFromConversations": false, "memoryModel": "", "goalJudgeModel": "alternate", "providerInstance": "codex-test-gateway", "modelOptions": {}, "name": "Lifecycle updated", "role": "", "model": "fixture", "instructions": "Help the user.", "threadLimit": 2, "toolBudget": 90}},
        {"path": "/api/agents/:id/folder", "payload": {"path": folder, "mode": "read-write", "revision": 0}},
        {"path": "/api/agents/:id/folder/reset", "payload": {"revision": 1}},
        {"path": "/api/agents/:id/computer", "payload": {"revision": 1, "alwaysOn": true}},
        {"path": "/api/agents/:id/sites", "payload": {"name": "Fixture site", "source": "<h1>Fixture site</h1>"}},
        {"path": "/api/agents/:id/avatar", "payload": {"revision": 0, "image": "PNG"}},
        {"path": "/api/agents/:id/visibility", "payload": {"revision": 0, "hidden": true}},
        {"path": "/api/agents/:id/visibility", "payload": {"revision": 1, "hidden": false}},
        {"path": "/api/agents", "payload": {"setup": true, "providerInstance": "codex-test-gateway", "modelOptions": {}, "name": "Drafted agent", "role": "", "model": "fixture", "instructions": "Draft a profile for scheduling.", "threadLimit": 2, "toolBudget": 90}},
        {"path": "/api/agents", "payload": {"name": "Imported fixture", "role": "", "model": "fixture", "instructions": "Help the user.", "threadLimit": 2}},
        {"path": "/api/agents/:id/delete", "payload": {}},
      ]);
      await deletion(browser, app.origin, client);
      receipt[client] = { timing, commands, errors };
      if (samples) {
        assertP95('profiles', 'cold', timing.cold.p95);
        assertP95('profiles', 'warm', timing.warm.p95);
      }
      console.log(`${client} browser screens, API and CSP checks passed`, timing);
    } finally { await app.close(); }
  }
  await writeFile(join(output, 'receipt.json'), JSON.stringify(receipt, null, 2) + '\n');

} finally {
  await browser?.close();
  provider.closeAllConnections();
  await new Promise(resolve => provider.close(resolve));
  await rm(scratch, { recursive: true });
  await rm(folder, { recursive: true });
}
