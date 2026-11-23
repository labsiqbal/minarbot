import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdirSync, mkdtempSync, rmSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout } from 'node:timers/promises';
import { chromium } from 'playwright-core';
import { startApp } from '../../test-bridge.ts';

const client = process.env.MINARBOT_SMOKE_CLIENT === 'leptos' ? 'leptos' : 'react';
const selector = client === 'leptos' ? '?ui=leptos' : '';

const scratch = mkdtempSync(join(tmpdir(), 'minarbot-goal-ui-'));
const screenshots = resolve(process.argv[2] ?? join(tmpdir(), 'minarbot-goal-screenshots'));
mkdirSync(screenshots, { recursive: true });
const answer = content => JSON.stringify({
  choices: [{ finish_reason: 'stop', message: { content } }],
  usage: { prompt_tokens: 20, completion_tokens: 10, total_tokens: 30 },
});
const requests = [];
let releaseContinuation;
const provider = createServer(async (request, response) => {
  let raw = '';
  for await (const chunk of request) raw += chunk;
  if (request.method === 'GET') {
    response.setHeader('Content-Type', 'application/json');
    response.end(JSON.stringify({ data: [{ id: 'fixture' }] }));
    return;
  }
  const body = JSON.parse(raw);
  const judge = body.messages[0].content.startsWith('Judge whether');
  const continuing = body.messages.at(-1)?.content?.includes('Continue the goal.');
  requests.push(judge ? 'judge' : continuing ? 'continuation' : 'turn');
  if (continuing) {
    releaseContinuation = () => {
      if (response.destroyed) return;
      response.setHeader('Content-Type', 'application/json');
      response.end(answer('Late continuation reply.'));
    };
    return;
  }
  response.setHeader('Content-Type', 'application/json');
  response.end(answer(judge
    ? JSON.stringify({ met: false, reason: 'Still need the verification result.', evidence: [] }) : 'First check complete.'));
});
await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
let app, browser;
try {
  app = await startApp({
    appearanceClient: client, database: join(scratch, 'test.sqlite'), dist: resolve('dist'), baseURL: `http://127.0.0.1:${provider.address().port}/v1`,
    defaultModel: 'fixture', stream: false,
  });
  async function api(path, body) {
    const response = await fetch(app.origin + path, body ? { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body) } : {});
    assert(response.ok, `${path} ${response.status} ${await response.clone().text()}`);
    return response.json();
  }
  browser = await chromium.launch({ headless: true, ...(process.env.CHROMIUM_EXECUTABLE ? { executablePath: process.env.CHROMIUM_EXECUTABLE } : {}) });
  for (const theme of ['dark', 'light']) {
    const requestStart = requests.length;
    const agent = await api('/api/agents', { id: randomUUID(), name: 'Goal fixture', model: 'fixture', instructions: 'Synthetic only.', runtime: 'api' });
    const context = await browser.newContext({ viewport: { width: 1440, height: 960 } });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await context.addInitScript(theme => localStorage.setItem('minarbot-presentation-v1', JSON.stringify({ version: 1, theme, textSize: 'standard' })), theme);
    await page.goto(`${app.origin}/agents/${agent.id}${selector}`);
    const composer = page.getByRole('textbox', { name: 'Message Goal fixture' });
    await composer.fill('/goal Confirm fixture readiness');
    await composer.press('Enter');
    const dialog = page.getByRole('dialog', { name: 'Start goal' });
    assert.equal(await dialog.getByLabel('Target', { exact: true }).inputValue(), 'Confirm fixture readiness');
    await dialog.getByLabel('Max turns', { exact: true }).fill('5');
    await dialog.getByRole('button', { name: 'Start goal', exact: true }).click();
    const chip = page.locator('.goal-chip');
    await chip.getByText('2/5 turns').waitFor();
    const continuation = page.getByRole('article').filter({ has: page.getByText('Goal continued', { exact: true }) });
    await continuation.locator('.activity-label').filter({ hasText: /^Working/ }).waitFor();
    assert(await composer.evaluate(element => element === document.activeElement));
    await chip.getByRole('button', { name: 'Confirm fixture readiness' }).click();
    const judge = page.getByRole('region', { name: 'Goal judge' });
    await judge.getByText('More work').waitFor();
    await judge.getByText('Turn 1', { exact: true }).click();
    await judge.getByText('Still need the verification result.').waitFor();
    const providerCalls = page.getByRole('region', { name: 'Provider calls' });
    await providerCalls.getByText('Pending', { exact: true }).waitFor();
    assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
    await page.screenshot({ path: join(screenshots, `goal-${theme}.png`), animations: 'disabled' });
    assert.deepEqual(requests.slice(requestStart), ['turn', 'judge', 'continuation']);
    const { current_turn: turn } = await api(`/api/agents/${agent.id}/goal`);
    await chip.getByRole('button', { name: 'Stop goal' }).click();
    await chip.getByText('Stopped', { exact: true }).waitFor();
    releaseContinuation();
    async function assertStopped() {
      await setTimeout(3000);
      assert.match(await continuation.locator('.activity-label').textContent(), /^Stopped/);
      assert.equal(await page.getByRole('article').locator('.activity-label').filter({ hasText: /^(Working|Queued)/ }).count(), 0);
      assert.equal(await continuation.getByRole('button', { name: 'Task actions' }).count(), 0);
      const conversation = await api(`/api/agents/${agent.id}/turns`);
      assert(conversation.every(turn => !['running', 'queued'].includes(turn.status)));
      const inspection = await api(`/api/turns/${turn}/inspect`);
      assert.equal(inspection.status, 'cancelled');
      assert.equal(inspection.providers.length, 1);
      assert.equal(inspection.providers[0].status, 'interrupted');
      assert(inspection.providers[0].finished);
      await providerCalls.locator('.run-inspector__state').filter({ hasText: /^Stopped/ }).waitFor();
      await page.locator('.run-inspector').getByText('Run stopped', { exact: true }).waitFor();
      assert.deepEqual(requests.slice(requestStart), ['turn', 'judge', 'continuation']);
    }
    await assertStopped();
    await page.screenshot({ path: join(screenshots, `goal-stopped-${theme}.png`), animations: 'disabled' });
    await page.reload();
    await chip.getByText('Stopped', { exact: true }).waitFor();
    await chip.getByRole('button', { name: 'Confirm fixture readiness' }).click();
    await assertStopped();
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
