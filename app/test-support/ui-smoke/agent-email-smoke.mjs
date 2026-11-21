import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import { startApp } from '../../test-bridge.ts';
import { testDatabase } from '../database.ts';
import relay from '../../../deploy/email-routing/worker.mjs';

const output = fileURLToPath(new URL('./artifacts/agent-email/', import.meta.url));
const temp = await mkdtemp(join(tmpdir(), 'minarbot-email-smoke-'));
const database = join(temp, 'state.sqlite');
const fixtureCode = '0' + String(36419);
const otpPrompt = 'Wait for the fixture verification email.';
const draftPrompt = 'Draft the fixture email.';
const from = 'verify@example.test';
let sawWait = false;
const provider = createServer((req, res) => {
  void (async () => {
    if (req.method !== 'POST') { res.writeHead(404); res.end(); return; }
    let raw = '';
    for await (const chunk of req) raw += chunk;
    const { messages, tools } = JSON.parse(raw);
    const prompt = messages.findLast(item => item.role === 'user')?.content;
    const result = messages.at(-1)?.role === 'tool' ? JSON.parse(messages.at(-1).content) : null;
    let choice;
    if (result) {
      const ready = prompt === otpPrompt ? result.code === fixtureCode && result.status === 'received' : result.status === 'draft_saved' && result.sent === false;
      choice = { finish_reason: 'stop', message: { content: ready ? prompt === otpPrompt ? 'Fixture verification received.' : 'Fixture draft saved.' : 'Unexpected mail tool result.' } };
    } else {
      const wait = prompt === otpPrompt;
      const name = wait ? 'email_wait_for_code' : 'email_send';
      assert(tools.some(spec => spec.function.name === name), 'Email tool missing from the provider request.');
      if (wait) sawWait = true;
      const args = wait ? { from, subject: 'Account verification', timeoutSeconds: 10 } : { to: 'reply@example.test', subject: 'Fixture draft', text: 'Synthetic draft text.' };
      choice = { finish_reason: 'tool_calls', message: { tool_calls: [{ id: randomUUID(), type: 'function', function: { name, arguments: JSON.stringify(args) } }] } };
    }
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ choices: [choice] }));
  })().catch(() => { res.writeHead(500); res.end(); });
});

let app, browser;
try {
  await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
  app = await startApp({ database, baseURL: `http://127.0.0.1:${provider.address().port}/v1`, defaultModel: 'email-fixture', stream: false });
  browser = await chromium.launch(process.env.CHROME_PATH ? { executablePath: process.env.CHROME_PATH } : {});
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const { origin } = app;
  await page.route('**/*', route => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
  const agent = randomUUID();
  const created = await page.request.post(origin + '/api/agents', {
    headers: { Origin: origin },
    data: { id: agent, runtime: 'api', name: 'Mail fixture', model: 'email-fixture', instructions: 'Help with fixture mail.' },
  });
  assert.equal(created.status(), 201);
  await page.goto(origin + `/agents/${agent}`);
  await page.getByRole('button', { name: 'Settings', exact: true }).click();
  await page.getByRole('button', { name: 'Email', exact: true }).click();
  await page.getByRole('textbox', { name: 'Receiving domain' }).fill('agents.example.test');
  await page.getByRole('button', { name: 'Save domain' }).click();
  await page.getByRole('button', { name: 'Generate secret' }).click();
  const reveal = page.getByRole('dialog', { name: 'Save endpoint secret' });
  const secret = await reveal.getByRole('textbox', { name: 'Endpoint secret' }).inputValue();
  assert(secret.length === 43, 'Secret was missing.');
  await reveal.getByRole('button', { name: 'Done', exact: true }).click();
  await page.reload();
  await page.getByRole('button', { name: 'Rotate secret' }).waitFor();
  assert.equal(await page.locator('[data-private-token]').count(), 0);
  const settings = await (await page.request.get(origin + '/api/email/settings')).json();
  assert(!('secret' in settings), 'Secret was revealed again.');
  await mkdir(output, { recursive: true });
  async function captureThemes(surface) {
    for (const theme of ['dark', 'light']) {
      await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
      await page.screenshot({ path: join(output, `${surface}-${theme}.png`), animations: 'disabled' });
    }
  }
  await captureThemes('settings');
  await page.getByRole('button', { name: 'Close settings' }).click();

  const fixtureAddress = `mail-fixture-${agent.replaceAll('-', '')}@agents.example.test`;
  const env = { MAIL_ENDPOINT: origin + settings.endpoint, MAIL_SECRET: secret };
  const receive = async (to, subject, text) => {
    const raw = new TextEncoder().encode(`From: ${from}\r\nTo: undisclosed-recipients:;\r\nSubject: ${subject}\r\n\r\n${text}\r\n`);
    let rejection;
    await relay.email({
      to, rawSize: raw.byteLength, raw: new Blob([raw]).stream(),
      setReject(reason) { rejection = reason; },
    }, env);
    return rejection;
  };
  assert.equal(await receive('unknown@agents.example.test', 'Rejected', 'Wrong recipient'), 'Unknown recipient.');
  assert.equal(await receive(fixtureAddress, 'Fixture receipt', 'Payment receipt for the smoke.'), undefined);
  assert.equal(await receive(fixtureAddress, 'Fixture confirmation', 'Second message.'), undefined);
  const agentRow = page.getByRole('button', { name: /^Mail fixture Idle/ });
  await agentRow.hover();
  await page.getByRole('button', { name: 'Actions for Mail fixture' }).press('Enter');
  await page.getByRole('menuitem', { name: 'Inbox' }).press('Enter');
  await page.getByRole('heading', { name: 'Inbox', exact: true }).waitFor();
  await page.getByText(fixtureAddress, { exact: true }).waitFor();
  await page.getByRole('button', { name: /^Fixture receipt/ }).press('Enter');
  await page.getByRole('heading', { name: 'Fixture receipt' }).waitFor();
  await page.getByText('Payment receipt for the smoke.', { exact: true }).waitFor();
  assert(await page.getByRole('heading', { name: 'Fixture receipt' }).evaluate(el => el === document.activeElement));
  await captureThemes('inbox');
  await page.setViewportSize({ width: 390, height: 844 });
  assert(await page.locator('main').evaluate(el => el.scrollWidth <= el.clientWidth));
  await page.screenshot({ path: join(output, 'inbox-mobile.png'), animations: 'disabled' });
  await page.getByRole('button', { name: 'Back to message list' }).click();
  await page.getByRole('button', { name: /^Fixture receipt/ }).waitFor();
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.getByRole('button', { name: 'Back to chat' }).click();

  const requestTurn = async text => {
    const id = randomUUID();
    const res = await page.request.post(origin + `/api/agents/${agent}/turns`, { headers: { Origin: origin }, data: { id, text } });
    assert.equal(res.status(), 202);
    return id;
  };
  const otpTurn = await requestTurn(otpPrompt);
  for (let attempt = 0; attempt < 80 && !sawWait; attempt++) await delay(50);
  assert(sawWait, 'Fake provider did not request the wait.');
  await receive(fixtureAddress, 'Account verification', 'Your code is ' + fixtureCode);
  await page.getByText('Fixture verification received.', { exact: true }).waitFor({ timeout: 15_000 });
  const inspection = await (await page.request.get(origin + `/api/turns/${otpTurn}/inspect`)).text();
  assert(!inspection.includes(fixtureCode), 'Run inspection leaked the fixture code.');

  await requestTurn(draftPrompt);
  const card = page.getByRole('region', { name: 'Email draft approval' });
  await card.getByText('Synthetic draft text.', { exact: false }).waitFor({ timeout: 15_000 });
  const draftCount = () => {
    const db = testDatabase(database, { readOnly: true });
    try { return db.prepare('SELECT count(*) AS count FROM agent_email_drafts').get().count; }
    finally { db.close(); }
  };
  assert.equal(draftCount(), 0);
  await card.getByRole('button', { name: 'Approve draft' }).click();
  await page.getByText('Fixture draft saved.', { exact: true }).waitFor({ timeout: 15_000 });
  assert.equal(draftCount(), 1);
  console.log('Agent email smoke passed: one-time secret, signed routing, keyboard inbox, OTP turn, approved draft.');
} finally {
  await browser?.close();
  await app?.close();
  provider.closeAllConnections();
  await new Promise(resolve => provider.close(resolve));
  await rm(temp, { force: true, recursive: true });
}
