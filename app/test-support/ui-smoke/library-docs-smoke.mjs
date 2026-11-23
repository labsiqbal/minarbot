import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import { startApp } from '../../test-bridge.ts';

const directory = await mkdtemp(join(tmpdir(), 'minarbot-docs-ui-'));
const output = process.env.MINARBOT_DOCS_SCREENSHOTS ?? await mkdtemp(join(tmpdir(), 'minarbot-docs-shots-'));
const agent = randomUUID();
const appearanceClient = process.env.MINARBOT_SMOKE_UI === 'leptos' ? 'leptos' : 'react';
const agentQuery = appearanceClient === 'leptos' ? '?ui=leptos' : '';
let app, browser;
try {
  app = await startApp({ database: join(directory, 'fixture.sqlite'), dist: fileURLToPath(new URL('../../dist', import.meta.url)), defaultModel: 'fixture', appearanceClient });
  const create = await fetch(app.origin + '/api/agents', {
    method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' },
    body: JSON.stringify({ id: agent, name: 'Docs agent', instructions: 'Help with library APIs.', model: 'fixture', runtime: 'api' }),
  });
  assert.equal(create.status, 201, await create.text());
  await mkdir(output, { recursive: true });
  browser = await chromium.launch({ executablePath: process.env.CHROME_PATH ?? '/usr/bin/google-chrome' });
  for (const theme of ['dark', 'light']) {
    const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
    await page.addInitScript(theme => localStorage.setItem('minarbot-presentation-v1', JSON.stringify({ version: 1, theme, textSize: 'standard' })), theme);
    let status = { reachable: true, keySet: false };
    await page.route('**/api/settings/library-docs', route => route.fulfill({ json: status }));
    await page.goto(app.origin + '/settings/general');
    const docs = page.getByRole('region', { name: 'Library docs', exact: true });
    await docs.getByText('Reachable', { exact: true }).waitFor();
    assert.equal(await docs.getByText('Optional', { exact: true }).count(), 1);
    assert.equal(await page.locator('html').getAttribute('data-theme'), theme);
    await docs.scrollIntoViewIfNeeded();
    await page.screenshot({ path: join(output, `library-docs-${theme}.png`), animations: 'disabled' });
    status = { reachable: false, keySet: true };
    const check = docs.getByRole('button', { name: 'Check status' });
    await check.focus();
    await page.keyboard.press('Enter');
    await docs.getByText('Unavailable', { exact: true }).waitFor();
    assert.equal(await docs.getByText('Set in Vault', { exact: true }).count(), 1);
    await page.close();
  }
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  await page.goto(`${app.origin}/agents/${agent}${agentQuery}`);
  const showDetails = page.getByRole('button', { name: 'Show details', exact: true });
  if (await showDetails.isVisible()) await showDetails.click();
  await page.getByRole('button', { name: 'Edit profile', exact: true }).first().click();
  const profile = page.getByRole('dialog', { name: 'Agent profile', exact: true });
  await profile.getByRole('tab', { name: 'Advanced', exact: true }).click();
  const docs = profile.getByRole('switch', { name: 'Library docs', exact: true });
  assert.equal(await docs.getAttribute('aria-checked'), 'true');
  await docs.focus();
  await page.keyboard.press('Space');
  assert.equal(await docs.getAttribute('aria-checked'), 'false');
  await profile.getByRole('button', { name: 'Save changes', exact: true }).click();
  await profile.waitFor({ state: 'hidden' });
  const directoryResponse = await (await fetch(app.origin + '/api/agents')).json();
  assert.equal(directoryResponse.agents.find(item => item.id === agent).library_docs_access, 0);
  console.log(`Library docs Settings dark/light and agent switch passed. Screenshots at ${output}`);
} finally {
  await browser?.close();
  await app?.close();
  await rm(directory, { recursive: true });
}
