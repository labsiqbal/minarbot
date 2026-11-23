import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { startApp } from '../../test-bridge.ts';
import { chromium } from 'playwright-core';

const scratch = await mkdtemp(join(tmpdir(), 'minarbot-computer-ui-'));
const output = process.env.MINARBOT_COMPUTER_SCREENSHOTS ?? await mkdtemp(join(tmpdir(), 'minarbot-computer-shots-'));
await mkdir(output, { recursive: true });
const appearanceClient = process.env.MINARBOT_SMOKE_UI === 'leptos' ? 'leptos' : 'react';
const agentQuery = appearanceClient === 'leptos' ? '?ui=leptos' : '';
let app, browser;
try {
  app = await startApp({ database: join(scratch, 'fixture.sqlite'), dist: fileURLToPath(new URL('../../dist', import.meta.url)), defaultModel: 'fixture', appearanceClient });
  browser = await chromium.launch({ executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE });
  const agent = randomUUID();
  let status;
  const reset = state => ({
    state, runtime: state === 'no_runtime' ? null : 'Podman',
    missingPrograms: state === 'no_runtime' ? ['Xvnc', 'xfwm4', 'scrot'] : [],
    canSetup: state !== 'ready', message: state === 'failed' ? 'Podman could not download the Computer image.' : null,
  });
  for (const theme of ['dark', 'light']) {
    status = reset('no_runtime');
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    await page.route('**/api/runtimes', route => route.fulfill({ json: { choices: [] } }));
    await page.addInitScript(theme => localStorage.setItem('minarbot-presentation-v1', JSON.stringify({ version: 1, theme, textSize: 'standard' })), theme);
    await page.route('**/api/settings/computer', route => {
      if (route.request().method() === 'POST') {
        assert.deepEqual(route.request().postDataJSON(), {});
        status = reset('downloading');
      }
      return route.fulfill({ status: route.request().method() === 'POST' ? 202 : 200, json: status });
    });
    await page.goto(app.origin);
    await page.getByRole('heading', { name: 'Set up Minarbot' }).waitFor();
    await page.getByText('No container runtime', { exact: true }).waitFor();
    await page.getByText('Missing programs - Xvnc, xfwm4, scrot').waitFor();
    assert(await page.getByRole('button', { name: 'Set up Computer' }).evaluate(button => button.getBoundingClientRect().bottom <= innerHeight));
    await page.screenshot({ path: join(output, `first-run-${theme}.png`), animations: 'disabled' });
    await page.goto(app.origin + '/settings/computer');
    const dialog = page.getByRole('dialog', { name: 'Settings', exact: true });
    await dialog.getByRole('heading', { name: 'Computer', exact: true }).waitFor();
    async function settle(state, label) {
      status = reset(state);
      await page.evaluate(() => window.dispatchEvent(new Event('focus')));
      await dialog.getByText(label, { exact: true }).waitFor();
    }
    await settle('image_missing', 'Image missing');
    const setup = dialog.getByRole('button', { name: 'Set up Computer' });
    await setup.focus();
    await page.keyboard.press('Enter');
    await dialog.getByText('Downloading image', { exact: true }).waitFor();
    assert(await setup.isDisabled());
    await settle('failed', 'Setup failed');
    await dialog.getByRole('alert').getByText('Podman could not download the Computer image.').waitFor();
    assert(await setup.isEnabled());
    await page.screenshot({ path: join(output, `settings-${theme}.png`), animations: 'disabled' });
    await setup.click();
    await dialog.getByText('Downloading image', { exact: true }).waitFor();
    await settle('ready', 'Ready · Podman');
    assert.equal(await setup.count(), 0);
    await page.close();
  }
  const create = await fetch(app.origin + '/api/agents', {
    method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' },
    body: JSON.stringify({ id: agent, name: 'Computer fixture', instructions: 'Help.', model: 'fixture', runtime: 'api' }),
  });
  assert.equal(create.status, 201, await create.text());
  const page = await browser.newPage();
  await page.route('**/api/runtimes', route => route.fulfill({ json: { choices: [] } }));
  status = reset('image_missing');
  await page.route('**/api/settings/computer', route => route.fulfill({ json: status }));
  const agentPath = `/api/agents/${agent}/computer`;
  let sharedProfile = false;
  const commands = [];
  await page.route(`**${agentPath}`, route => {
    if (route.request().method() === 'POST') {
      commands.push(route.request().postDataJSON());
      if (commands.length === 1) return route.fulfill({ contentType: 'application/json', body: '{lost acknowledgement' });
      sharedProfile = true;
    }
    const ready = status.state === 'ready';
    return route.fulfill({
      json: { available: ready, enabled: ready, sharedProfile, alwaysOn: false, alwaysOnLimitReached: false, state: 'off', width: 1280, height: 800, revision: Number(sharedProfile), control: {}, requests: [], setup: status },
    });
  });
  const profile = page.getByRole('dialog', { name: 'Agent profile' });
  async function openProfile() {
    const show = page.getByRole('button', { name: 'Show details', exact: true });
    if (await show.isVisible()) await show.click();
    await page.getByRole('button', { name: 'Edit profile', exact: true }).click();
    await profile.getByRole('tab', { name: 'Advanced' }).click();
  }
  await page.goto(app.origin + `/agents/${agent}${agentQuery}`);
  await openProfile();
  const toggle = profile.getByRole('switch', { name: 'Computer', exact: true });
  await toggle.waitFor();
  assert(await toggle.isDisabled());
  await profile.getByText('Image missing', { exact: true }).waitFor();
  await profile.getByRole('link', { name: 'Computer setup', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Settings', exact: true });
  await dialog.getByRole('heading', { name: 'Computer', exact: true }).waitFor();
  await dialog.getByRole('button', { name: 'Vault', exact: true }).click();
  await dialog.getByRole('heading', { name: 'Vault', exact: true }).waitFor();
  await dialog.getByRole('button', { name: 'Close settings', exact: true }).click();

  status = reset('ready');
  await openProfile();
  await profile.getByRole('switch', { name: 'Always-on Computer' }).waitFor();
  const shared = profile.getByRole('switch', { name: 'Shared browser profile' });
  await shared.click();
  const retry = profile.getByRole('button', { name: 'Try again', exact: true });
  await retry.waitFor();
  await page.reload();
  await openProfile();
  await retry.click();
  await profile.getByRole('switch', { name: 'Shared browser profile', checked: true }).waitFor();
  assert.deepEqual(commands[1], commands[0]);
  assert.equal(commands[0].sharedProfile, true);
  assert.equal(commands[0].enabled, true);
  for (const theme of ['dark', 'light']) {
    await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
    await page.screenshot({ path: join(output, `agent-settings-${theme}.png`), animations: 'disabled' });
  }
  console.log(`Computer setup smoke passed. Screenshots: ${output}`);
} finally {
  await browser?.close();
  await app?.close();
  await rm(scratch, { recursive: true, force: true });
}
