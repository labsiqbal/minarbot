import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, symlink } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import { startRustServer } from '../../rust-test-server.ts';

const work = await mkdtemp(join(tmpdir(), 'minarbot-guide-'));
const output = process.env.MINARBOT_GUIDE_SCREENSHOTS && resolve(process.env.MINARBOT_GUIDE_SCREENSHOTS);
const client = process.env.MINARBOT_SMOKE_UI ?? 'react';
const turns = [
  ['Help me set up my workspace.', 'What work do you want to do, and when do you need the output?'],
  ['I need a Researcher that cites sources.', 'Review the Researcher profile and select Apply.', 'create_agent'],
  ['Add an on-demand Source review routine.', 'Review the manual routine and select Apply.', 'create_routine'],
  ['How do I change AI providers?', 'Open Providers in Settings and add a provider account.', 'open_guide_screen'],
  ['Add my test machine.', 'Review the machine and select Apply.', 'add_guide_machine'],
];
let agentId = '';
let guidePrompt = false;
let toolIsolation = false;
const model = 'guide-fixture';
const provider = createServer(async (req, res) => {
  res.setHeader('Content-Type', 'application/json');
  if (req.method === 'GET') return res.end(JSON.stringify({ data: [{ id: model }] }));
  let raw = '';
  for await (const chunk of req) raw += chunk;
  const request = JSON.parse(raw);
  guidePrompt ||= request.messages.some(item => item.role === 'system' && item.content.includes('[[settings.providers]]'));
  const offered = request.tools?.map(tool => tool.function.name) ?? [];
  toolIsolation ||= offered.includes('create_agent') && !offered.includes('run_shell') && !offered.includes('edit_profile');
  const last = request.messages.at(-1);
  const user = request.messages.findLast(item => item.role === 'user');
  const [, reply, tool] = turns.find(([prompt]) => prompt === user?.content) ?? [];
  const args = {
    create_agent: { requestId: randomUUID(), name: 'Researcher', model, instructions: 'Cite primary sources and flag gaps.' },
    create_routine: { requestId: randomUUID(), agent: agentId, name: 'Source review', prompt: 'Check the sources in the latest brief.', trigger: 'manual', enabled: true },
    open_guide_screen: { screen: 'settings.providers' },
    add_guide_machine: { requestId: randomUUID(), name: 'Test machine', origin: 'https://machine.example' },
  };
  const calls = tool && last.role !== 'tool' ? [{ id: randomUUID(), type: 'function', function: { name: tool, arguments: JSON.stringify(args[tool]) } }] : null;
  res.end(JSON.stringify({ choices: [{ finish_reason: calls ? 'tool_calls' : 'stop', message: { content: calls ? null : reply ?? 'Fixture reply', ...(calls && { tool_calls: calls }) } }] }));
});

await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
let app, browser;
try {
  const releasesDir = join(work, 'releases');
  await mkdir(join(releasesDir, '0.17.0-8523c26b'), { recursive: true });
  await symlink('0.17.0-8523c26b', join(releasesDir, 'current'));
  app = await startRustServer({
    database: join(work, 'state.sqlite'),
    appearanceClient: client,
    dist: fileURLToPath(new URL('../../dist', import.meta.url)),
    baseURL: `http://127.0.0.1:${provider.address().port}/v1`,
    apiKey: 'fixture-key',
    defaultModel: model,
    updates: { releasesDir },
    harness: { executables: Object.fromEntries(
      ['codex', 'claude', 'hermes', 'antigravity', 'grok', 'pi', 'opencode', 'gemini', 'kiro'].map(runtime => [runtime, join(work, runtime)]),
    ) },
  });
  async function api(path, body, status = 200, headers = {}) {
    const response = await fetch(app.origin + path, {
      method: body ? 'POST' : 'GET',
      headers: { Origin: app.origin, 'Content-Type': 'application/json', 'Sec-Fetch-Site': 'same-origin', ...headers },
      body: body ? JSON.stringify(body) : undefined,
    });
    assert.equal(response.status, status, path.replace(/\/changes\/[^/]+$/, '/changes/:id'));
    return response.json();
  }
  const directory = await api('/api/agents');
  assert.equal(directory.agents.length, 0);
  const { guide } = directory;
  assert(guide.builtin);
  await api(`/api/agents/${guide.id}/delete`, { id: randomUUID() }, 409);
  const path = `/api/agents/${guide.id}/turns`;
  browser = await chromium.launch({ executablePath: process.env.CHROME ?? '/usr/bin/google-chrome', args: ['--no-sandbox'] });
  const viewport = { width: 1280, height: 820 };
  const page = await browser.newPage({ viewport, reducedMotion: 'reduce' });
  const failures = [];
  page.on('pageerror', error => failures.push(error.message));
  page.on('console', event => { if (event.type() === 'error') failures.push(event.text()); });
  async function capture(label, region, mask = []) {
    if (!output) return;
    await mkdir(output, { recursive: true });
    for (const [layout, size] of Object.entries({ desktop: viewport, narrow: { width: 390, height: 844 } })) {
      await page.setViewportSize(size);
      for (const theme of ['dark', 'light']) {
        await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
        await region.screenshot({ path: join(output, `${client}-${label}-${layout}-${theme}.png`), animations: 'disabled', mask });
      }
    }
    await page.setViewportSize(viewport);
  }
  await page.goto(app.origin);
  await page.getByRole('button', { name: `Set up with ${guide.name}` }).click();
  await page.waitForURL(url => url.pathname === `/agents/${guide.id}` && !url.search);
  assert.equal(await page.locator('#leptos-root').count(), client === 'leptos' ? 1 : 0);
  const composer = page.getByRole('textbox', { name: `Message ${guide.name}` });
  async function send(step) {
    const [prompt, reply] = turns[step];
    const before = await page.getByText(reply, { exact: true }).count();
    await composer.fill(prompt);
    await composer.press('Enter');
    await page.getByText(reply, { exact: true }).nth(before).waitFor();
  }
  await send(0);
  await send(1);
  const agentChange = page.getByRole('article', { name: 'Create agent' });
  await agentChange.getByRole('button', { name: 'Apply', exact: true }).waitFor();
  const proposal = (await api(path)).flatMap(turn => turn.guide?.changes ?? []).at(-1);
  const changed = structuredClone(proposal.change);
  changed.settings.instructions = 'Different instructions.';
  const confirm = `/api/guide/changes/${proposal.id}`;
  await api(confirm, { action: 'apply' }, 400);
  await api(confirm, { action: 'apply', change: changed }, 409);
  await api(confirm, { action: 'apply', change: proposal.change }, 403, { 'Sec-Fetch-Site': 'cross-site' });
  assert.equal((await api('/api/agents')).agents.length, 0);

  await page.getByLabel('Streaming answer').waitFor({ state: 'hidden' });
  await agentChange.scrollIntoViewIfNeeded();
  for (const theme of ['dark', 'light']) {
    await page.evaluate(value => { document.documentElement.dataset.theme = value; }, theme);
    await page.screenshot({ path: join(work, `guide-${theme}.png`), animations: 'disabled' });
  }
  await capture('guide-agent-proposal', agentChange);
  await agentChange.getByRole('button', { name: 'Apply', exact: true }).press('Enter');
  await agentChange.getByRole('status').filter({ hasText: 'Applied' }).waitFor();
  const agents = (await api('/api/agents')).agents;
  assert.equal(agents.length, 1);
  assert.equal(agents[0].instructions, 'Cite primary sources and flag gaps.');
  agentId = agents[0].id;
  await api(confirm, { action: 'apply', change: proposal.change }, 409);

  await send(2);
  const routineChange = page.getByRole('article', { name: 'Create routine' });
  assert.deepEqual(await api(`/api/agents/${agentId}/routines`), []);
  await capture('guide-routine-proposal', routineChange);
  await routineChange.getByRole('button', { name: 'Cancel', exact: true }).click();
  await routineChange.getByRole('status').filter({ hasText: 'Cancelled' }).waitFor();
  assert.deepEqual(await api(`/api/agents/${agentId}/routines`), []);
  await send(2);
  await routineChange.last().getByRole('button', { name: 'Apply', exact: true }).click();
  await routineChange.last().getByRole('status').filter({ hasText: 'Applied' }).waitFor();
  const routines = await api(`/api/agents/${agentId}/routines`);
  assert.equal(routines.length, 1);
  assert.equal(routines[0].trigger, 'manual');

  await send(3);
  await page.getByRole('link', { name: 'Open Providers', exact: true }).click();
  await page.waitForURL(url => url.pathname === '/settings/providers');
  await page.getByRole('heading', { name: 'Providers', exact: true }).waitFor();
  await page.goBack();
  await send(4);
  const machineChange = page.getByRole('article', { name: 'Add machine', exact: true });
  await machineChange.getByRole('button', { name: 'Apply', exact: true }).waitFor();
  await machineChange.scrollIntoViewIfNeeded();
  for (const theme of ['dark', 'light']) {
    await page.evaluate(value => { document.documentElement.dataset.theme = value; }, theme);
    await page.screenshot({ path: join(work, 'guide-infra-' + theme + '.png'), animations: 'disabled' });
  }
  await capture('guide-machine-proposal', machineChange);
  const installReceipt = page.waitForResponse(response => response.url().endsWith('/api/install/tokens') && response.request().method() === 'POST');
  await machineChange.getByRole('button', { name: 'Apply', exact: true }).press('Enter');
  const issued = await installReceipt;
  assert.equal(issued.status(), 201);
  assert(!/\bbsi_/.test(await issued.text()), 'Install credential leaked in response');
  await machineChange.getByRole('status').filter({ hasText: 'Waiting for the machine to pair.' }).waitFor();
  const command = await machineChange.getByRole('textbox', { name: 'Run on the new machine' }).inputValue();
  assert(command.includes('--name \'Test machine\'') && /\bbsi_[A-Za-z0-9_-]{43}\b/.test(command), 'Install command invalid');
  assert(!/\bbsi_[A-Za-z0-9_-]{43}\b/.test(JSON.stringify(await api(path))), 'Install credential leaked in transcript');
  await capture('guide-machine-install', machineChange, [machineChange.getByRole('textbox', { name: 'Run on the new machine' })]);
  assert.deepEqual(failures, []);
  assert(guidePrompt);
  assert(toolIsolation);
  await api(`/api/agents/${guide.id}/visibility`, { id: randomUUID(), revision: 0, hidden: true });
  assert.equal((await api('/api/agents')).guide.hidden, true);
  await api(`/api/agents/${guide.id}/delete`, { id: randomUUID() }, 409);
  console.log(`PASS: Guide setup, approval, private install command, pairing status, help and hide/delete. Screenshots: ${work}`);
} finally {
  await browser?.close();
  await app?.close();
  provider.closeAllConnections();
  await new Promise(resolve => provider.close(resolve));
}
