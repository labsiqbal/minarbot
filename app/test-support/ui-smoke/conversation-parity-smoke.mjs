import assert from 'node:assert/strict';
import { fixDate, parityShot } from './parity-shot.mjs';
import { randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as wait } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import { startRustServer } from '../../rust-test-server.ts';
import { desktopImage, installVncFixture } from './computer-smoke.mjs';

const out = resolve(process.argv[2] ?? '/tmp/minarbot-conversation-parity');
const work = await mkdtemp(join(tmpdir(), 'minarbot-conversation-parity-'));
const dist = fileURLToPath(new URL('../../dist', import.meta.url));
const model = 'parity-fixture';
const ids = { agent: '40000000-0000-4000-8000-000000000010', completed: '40000000-0000-4000-8000-000000000011', failed: '40000000-0000-4000-8000-000000000012', rich: '40000000-0000-4000-8000-000000000013' };
const chart = {
  tree: { type: 'stack', children: [{ type: 'bar', title: 'Weekly signups', data: 'signups' }] },
  data: { signups: [{ label: 'Week 1', value: 26 }, { label: 'Week 2', value: 40 }, { label: 'Week 3', value: 51 }, { label: 'Week 4', value: 67 }] },
  actions: [],
};
const form = {
  tree: { type: 'stack', children: [{ type: 'form', title: 'Compare periods', data: 'fields', action: 'compare' }] },
  data: { fields: [{ name: 'label', label: 'Comparison name', type: 'text', required: true }, { name: 'weeks', label: 'Weeks to compare', type: 'number', required: true }] },
  actions: [{ id: 'compare', type: 'submit', label: 'Run comparison', context: { report: 'fixture' } }],
};
const provider = createServer(async (request, response) => {
  response.setHeader('content-type', 'application/json');
  if (request.method === 'GET') return response.end(JSON.stringify({ data: [{ id: model }] }));
  let raw = '';
  for await (const chunk of request) raw += chunk;
  const { messages } = JSON.parse(raw);
  const prompt = messages.findLast(message => message.role === 'user')?.content;
  if (prompt === 'Inspect failed run') {
    response.statusCode = 400;
    return response.end(JSON.stringify({ error: { message: 'Fixture provider rejected the request.' } }));
  }
  const result = messages.at(-1)?.role === 'tool' ? JSON.parse(messages.at(-1).content) : null;
  const reply = prompt === 'Show the report' ? (!result ? chart : result.status === 'rendered' ? form : null) : null;
  const message = reply ? { content: null, tool_calls: [{ type: 'function', id: randomUUID(), function: { name: 'render_component', arguments: JSON.stringify(reply) } }] }
    : { content: 'First **café**.\n\nFinal answer.' };
  response.end(JSON.stringify({ choices: [{ finish_reason: reply ? 'tool_calls' : 'stop', message }] }));
});

let app, browser;
try {
  await mkdir(out, { recursive: true });
  await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
  app = await startRustServer({ appearanceClient: 'leptos', appearanceClient: 'leptos', database: join(work, 'state.sqlite'), dist, fixedClock: Date.UTC(2026, 10, 23, 7), baseURL: `http://127.0.0.1:${provider.address().port}/v1`, apiKey: 'synthetic', defaultModel: model, stream: false });
  async function api(path, payload) {
    const res = await fetch(app.origin + path, payload === undefined ? undefined : { method: 'POST', headers: { origin: app.origin, 'content-type': 'application/json' }, body: JSON.stringify(payload) });
    assert(res.ok, `${path} ${res.status} ${await res.clone().text()}`);
    return res.json();
  }
  const agent = await api('/api/agents', { id: ids.agent, name: 'Conversation fixture', instructions: 'Reply to fixture prompts.', model, runtime: 'api' });
  const main = `/agents/${agent.id}`;
  async function seed(scope, text, ready) {
    const id = text === 'Replay fixture' ? ids.completed : text === 'Inspect failed run' ? ids.failed : ids.rich;
    await api(`${scope}/turns`, { id, text });
    for (let i = 0; i < 200; i++) {
      const turn = (await api(`${scope}/turns`)).find(row => row.id === id);
      if (turn && ready(turn)) return turn;
      if (turn?.status === 'failed' || turn?.status === 'interrupted') assert.fail(`${text}: ${turn.error}`);
      await wait(100);
    }
    assert.fail(`${text} did not reach requested state`);
  }
  async function thread(name) {
    const data = await api(`/api${main}/threads`, { id: randomUUID(), name });
    return { path: `${main}/conversations/${data.id}`, scope: `/api/threads/${data.id}` };
  }
  const completed = await seed(`/api${main}`, 'Replay fixture', turn => turn.status === 'done');
  const failed = await thread('Failed run');
  await seed(failed.scope, 'Inspect failed run', turn => turn.status === 'failed');
  const rich = await thread('October report');
  const richTurn = await seed(rich.scope, 'Show the report', turn => turn.harnessApprovals?.some(item => item.kind === 'form' && item.state === 'approval'));
  const screen = 'data:image/png;base64,' + (await desktopImage).toString('base64');
  const fixed = Math.max(Date.now(), Date.parse(completed.created) + 5_000);
  const selector = (path, client) => app.origin + path;
  browser = await chromium.launch({ headless: true });

  for (const [viewport, width, height] of [['desktop', 1440, 900], ['narrow', 430, 850]]) {
    for (const theme of ['dark', 'light']) {

      for (const client of ['leptos']) {
        const context = await browser.newContext({ viewport: { width, height }, colorScheme: theme, reducedMotion: 'reduce' });
        try {
          const page = await context.newPage();
          await fixDate(page, fixed);
          await page.addInitScript(theme => {
            localStorage.setItem('minarbot-presentation-v1', JSON.stringify({ version: 1, theme, textSize: 'standard', colorTheme: 'minarbot' }));
            localStorage.setItem('minarbot:details-open', '0');
          }, theme);
          async function capture(name, anchor) {
            await page.evaluate(async () => {
              await document.fonts.ready;
              await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
            });
            if (anchor) await anchor.scrollIntoViewIfNeeded();
            await parityShot(page, join(out, `${viewport}-${theme}-${name}-${client}.png`));
          }
          const details = page.getByRole('complementary', { name: 'Conversation details' });
          const transcript = page.getByRole('region', { name: 'Conversation history' });
          await page.goto(selector(main, client));
          await transcript.getByText('Final answer.', { exact: true }).waitFor();
          await page.getByRole('textbox', { name: 'Message Conversation fixture' }).waitFor();
          const bubble = transcript.getByText('Replay fixture', { exact: true });
          await bubble.hover();
          await capture('conversation');
          await page.getByRole('button', { name: 'Show details' }).click();
          await details.getByRole('tab', { name: 'Chats', exact: true }).waitFor();
          await details.getByRole('button', { name: 'Edit profile', exact: true }).waitFor();
          await details.getByLabel('Menu conversation Main chat', { exact: true }).waitFor();
          await details.getByLabel('Menu conversation October report', { exact: true }).waitFor();
          await capture('chats');
          await details.getByRole('tab', { name: 'Files', exact: true }).click();
          await details.getByText('Directory is empty on this page.', { exact: true }).waitFor();
          await capture('files');

          await page.goto(selector(failed.path, client));
          await transcript.getByText('Inspect failed run', { exact: true }).waitFor();
          await transcript.getByRole('button', { name: /^Inspect( run)?$/ }).click();
          await details.getByRole('tab', { name: 'Run', exact: true }).waitFor();
          await details.locator('.run-inspector').waitFor();
          await details.getByText('Provider calls', { exact: true }).waitFor();
          await capture('run');

          await page.goto(selector(rich.path, client));
          const chartHeading = transcript.getByRole('heading', { name: 'Weekly signups', exact: true });
          const formRegion = transcript.getByRole('form', { name: 'Compare periods' });
          await chartHeading.waitFor();
          await formRegion.waitFor();
          await capture('chart', chartHeading.locator('..'));
          await capture('form', formRegion);
          await page.route(url => url.origin === app.origin && url.pathname === rich.scope + '/turns', async route => {
            if (route.request().method() !== 'GET') return route.continue();
            const original = await route.fetch();
            const rows = await original.json();
            const approval = { id: '11111111-1111-4111-8111-111111111111', kind: 'command', description: 'Review the requested fixture action.', command: 'printf fixture', prefix: null, input: { command: 'printf fixture' }, state: 'approval', decision: null, revision: 1 };
            await route.fulfill({ response: original, json: rows.map(turn => ({ ...turn, harnessApprovals: [approval] })) });
          });
          await page.reload();
          const approve = transcript.getByRole('button', { name: 'Allow once', exact: true });
          await approve.waitFor();
          await capture('approval', approve);

          await page.route(url => url.origin === app.origin && url.pathname.startsWith(`/api${main}/computer`), route => route.fulfill({
            json: route.request().url().endsWith('/vnc-ticket') ? { ticket: 'fixture-viewer' } : {
              available: true, enabled: true, state: 'running', control: { mode: 'agent', revision: 0 }, requests: [],
            },
          }));
          await page.goto(selector(main, client));
          await transcript.getByText('Final answer.', { exact: true }).waitFor();
          await installVncFixture(page, screen);
          await page.getByRole('button', { name: 'Show details' }).click();
          await details.getByRole('tab', { name: 'Computer', exact: true }).click();
          await details.locator('canvas').waitFor();
          await page.waitForFunction(() => window.__vncFake?.frames > 0);
          await capture('computer');

        } finally { await context.close(); }
      }
    }
  }
  await api(`${rich.scope}/turns/${richTurn.id}/cancel`, { id: randomUUID() });
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.addInitScript(() => localStorage.setItem('minarbot:details-open', '0'));
  await page.goto(selector(main, 'leptos'));
  await page.getByRole('button', { name: 'Help', exact: true }).click();
  await page.getByRole('dialog', { name: 'Keyboard shortcuts' }).waitFor();
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: /Quick switcher/ }).click();
  const search = page.getByRole('dialog', { name: 'Quick switcher' });
  await search.getByLabel('Jump to').fill('Conversation fixture');
  await search.getByRole('option', { name: /^Conversation fixture/ }).first().waitFor();
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: 'Show details' }).click();
  const details = page.getByRole('complementary', { name: 'Conversation details' });
  const editor = page.getByRole('dialog', { name: 'Agent profile' });
  await details.getByRole('button', { name: 'Edit profile', exact: true }).click();
  await editor.getByRole('button', { name: 'Cancel' }).click();
  await editor.waitFor({ state: 'hidden' });
  await details.getByLabel('Menu conversation Main chat').click();
  await details.locator('.chat-list-menu').getByRole('button', { name: 'Edit profile', exact: true }).click();
  assert.equal(await editor.getByRole('textbox', { name: 'Name', exact: true }).inputValue(), agent.name);
  const saved = page.waitForResponse(response => response.request().method() === 'POST' && response.url().endsWith(`/api${main}/edit`));
  const reloaded = page.waitForEvent('load');
  await editor.getByRole('button', { name: 'Save changes', exact: true }).click();
  const response = await saved;
  assert(response.ok(), `Profile edit returned ${response.status()}`);
  await reloaded;
  const updated = (await api('/api/agents')).agents.find(item => item.id === agent.id);
  assert.equal(updated.revision, agent.revision + 1);
  assert.equal(updated.name, agent.name);
  console.log('PASS quick switcher, keyboard help, panel gear, Main chat menu, and profile save');
} finally {
  await browser?.close();
  await app?.close();
  provider.closeAllConnections();
  if (provider.listening) await new Promise(resolve => provider.close(resolve));
  await rm(work, { recursive: true });
}
