import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import { startApp } from '../../test-bridge.ts';

const directory = await mkdtemp(join(tmpdir(), 'minarbot-craft-ui-'));
const output = process.env.MINARBOT_CRAFT_SCREENSHOTS ?? await mkdtemp(join(tmpdir(), 'minarbot-craft-shots-'));
const endpoint = '/api/settings/craft-anything';
const agent = randomUUID();
const project = randomUUID();
let app, browser;
try {
  app = await startApp({ appearanceClient: 'leptos', database: join(directory, 'fixture.sqlite'), dist: fileURLToPath(new URL('../../dist', import.meta.url)), defaultModel: 'fixture' });
  const create = await fetch(app.origin + '/api/agents', {
    method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' },
    body: JSON.stringify({ id: agent, name: 'Website agent', instructions: 'Study public pages.', model: 'fixture', runtime: 'api' }),
  });
  assert.equal(create.status, 201, await create.text());
  await mkdir(output, { recursive: true });
  browser = await chromium.launch({ executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE ?? '/usr/bin/google-chrome' });
  for (const theme of ['dark', 'light']) {
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    await page.addInitScript(theme => localStorage.setItem('minarbot-presentation-v1', JSON.stringify({ version: 1, theme, textSize: 'standard' })), theme);
    await page.route('**/api/projects', route => route.fulfill({ json: { root: directory, projects: [{ id: project, name: 'Public website', archived: 0, members: [], threads: [], sections: [], folders: [] }] } }));
    const status = { version: '0.6.1', available: true, installed: false, computerReady: true, agents: [], deepProjects: [] };
    const actions = [];
    await page.route(`**${endpoint}**`, route => {
      const action = new URL(route.request().url()).pathname.slice(endpoint.length + 1);
      if (action) {
        actions.push(action);
        const body = route.request().postDataJSON();
        if (action === 'install' || action === 'remove') status.installed = action === 'install';
        if (action === 'grant') { assert.deepEqual(body, { agent, enabled: true }); status.agents = [agent]; }
        if (action === 'deep') { assert.deepEqual(body, { project, confirmedOwnership: true }); status.deepProjects = [project]; }
      }
      return route.fulfill({ json: status });
    });
    await page.goto(app.origin + '/settings/craft-anything');
    const settings = page.getByRole('dialog', { name: 'Settings', exact: true });
    await settings.getByRole('heading', { name: 'Craft Anything', exact: true }).waitFor();
    await settings.getByText('Not installed', { exact: true }).waitFor();
    const install = settings.getByRole('button', { name: 'Install', exact: true });
    await install.focus();
    await page.keyboard.press('Enter');
    await settings.getByText('Installed', { exact: true }).waitFor();
    await settings.getByRole('switch', { name: 'Craft Anything access for Website agent' }).click();
    await settings.getByRole('switch', { name: 'Craft Anything access for Website agent', checked: true }).waitFor();
    const deep = settings.getByRole('switch', { name: 'Deep mode for Public website' });
    await deep.click();
    const confirmation = page.getByRole('alertdialog', { name: 'Allow Deep mode for Public website?' });
    await confirmation.waitFor();
    assert.deepEqual(actions, ['install', 'grant']);
    await page.keyboard.press('Escape');
    await confirmation.waitFor({ state: 'hidden' });
    assert.equal(await deep.getAttribute('aria-checked'), 'false');
    await deep.click();
    await confirmation.getByRole('button', { name: 'Confirm permission' }).click();
    await settings.getByText('Deep mode allowed', { exact: true }).waitFor();
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
    await page.screenshot({ path: join(output, `settings-${theme}.png`), animations: 'disabled' });
    await settings.getByRole('button', { name: 'Remove', exact: true }).click();
    await settings.getByText('Not installed', { exact: true }).waitFor();
    assert.deepEqual(actions, ['install', 'grant', 'deep', 'remove']);
    await page.close();
  }
  console.log(`Craft Anything Settings dark/light passed: ${output}`);
} finally {
  await browser?.close();
  await app?.close();
  await rm(directory, { recursive: true, force: true });
}
