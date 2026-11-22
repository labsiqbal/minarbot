import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdirSync, mkdtempSync, rmSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { chromium } from 'playwright-core';
import { startApp } from '../../test-bridge.ts';

const scratch = mkdtempSync(join(tmpdir(), 'minarbot-goal-ui-'));
const screenshots = resolve(process.argv[2] ?? join(tmpdir(), 'minarbot-goal-screenshots'));
mkdirSync(screenshots, { recursive: true });
const answer = content => JSON.stringify({
  choices: [{ finish_reason: 'stop', message: { content } }],
  usage: { prompt_tokens: 20, completion_tokens: 10, total_tokens: 30 },
});
const provider = createServer(async (request, response) => {
  let raw = '';
  for await (const chunk of request) raw += chunk;
  if (request.method === 'GET') {
    response.setHeader('Content-Type', 'application/json');
    response.end(JSON.stringify({ data: [{ id: 'fixture' }] }));
    return;
  }
  const body = JSON.parse(raw);
  const last = body.messages.at(-1)?.content ?? '';
  if (last.includes('Continue the goal.')) return;
  response.setHeader('Content-Type', 'application/json');
  response.end(answer(body.messages[0].content.startsWith('Judge whether')
    ? JSON.stringify({ met: false, reason: 'Still need the verification result.', evidence: [] }) : 'First check complete.'));
});
await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
let app, browser;
try {
  app = await startApp({
    database: join(scratch, 'test.sqlite'), dist: resolve('dist'), baseURL: `http://127.0.0.1:${provider.address().port}/v1`,
    defaultModel: 'fixture', stream: false,
  });
  async function api(path, body) {
    const response = await fetch(app.origin + path, body ? { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body) } : {});
    assert(response.ok, `${path} ${response.status} ${await response.clone().text()}`);
    return response.json();
  }
  browser = await chromium.launch({ headless: true, ...(process.env.CHROMIUM_EXECUTABLE ? { executablePath: process.env.CHROMIUM_EXECUTABLE } : {}) });
  for (const theme of ['dark', 'light']) {
    const agent = await api('/api/agents', { id: randomUUID(), name: 'Goal fixture', model: 'fixture', instructions: 'Synthetic only.', runtime: 'api' });
    const context = await browser.newContext({ viewport: { width: 1440, height: 960 } });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await context.addInitScript(theme => localStorage.setItem('minarbot-presentation-v1', JSON.stringify({ version: 1, theme, textSize: 'standard' })), theme);
    await page.goto(`${app.origin}/agents/${agent.id}`);
    const composer = page.getByRole('textbox', { name: 'Message Goal fixture' });
    await composer.fill('/goal Confirm fixture readiness');
    await composer.press('Enter');
    const dialog = page.getByRole('dialog', { name: 'Start goal' });
    assert.equal(await dialog.getByLabel('Target', { exact: true }).inputValue(), 'Confirm fixture readiness');
    await dialog.getByLabel('Max turns', { exact: true }).fill('5');
    await dialog.getByRole('button', { name: 'Start goal', exact: true }).click();
    const chip = page.locator('.goal-chip');
    await chip.getByText('2/5 turns').waitFor();
    await page.getByText('Goal continued', { exact: true }).waitFor();
    assert(await composer.evaluate(element => element === document.activeElement));
    await chip.getByRole('button', { name: 'Confirm fixture readiness' }).click();
    const judge = page.getByRole('region', { name: 'Goal judge' });
    await judge.getByText('More work').waitFor();
    await judge.getByText('Turn 1', { exact: true }).click();
    await judge.getByText('Still need the verification result.').waitFor();
    assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
    await page.screenshot({ path: join(screenshots, `goal-${theme}.png`), animations: 'disabled' });
    await chip.getByRole('button', { name: 'Stop goal' }).click();
    await chip.getByText('Stopped', { exact: true }).waitFor();
    await page.screenshot({ path: join(screenshots, `goal-stopped-${theme}.png`), animations: 'disabled' });
    await page.reload();
    await chip.getByText('Stopped', { exact: true }).waitFor();
    await composer.fill(`/loop 5m Check ${theme} fixture readiness`);
    await composer.press('Enter');
    const loop = page.getByRole('dialog', { name: 'Schedule loop' });
    await loop.getByLabel(/^Until/).fill('Fixture is ready');
    await loop.getByRole('button', { name: 'Schedule loop', exact: true }).click();
    await page.getByRole('link', { name: 'View routines' }).click();
    await page.getByText(`Loop · Check ${theme} fixture readiness`, { exact: true }).first().waitFor();
    await page.goto(`${app.origin}/calendar`);
    await page.getByRole('button', { name: new RegExp(`^Loop · Check ${theme} fixture readiness, Routine, Scheduled`) }).first().waitFor();
    assert.deepEqual(errors, []);
    await context.close();
  }
  console.log('Goal composer, inspector, Stop, restart readback and loop creation passed in dark and light.');
  console.log(screenshots);
} finally {
  await browser?.close();
  await app?.close();
  provider.closeAllConnections();
  await new Promise(resolve => provider.close(resolve));
  rmSync(scratch, { recursive: true, force: true });
}
