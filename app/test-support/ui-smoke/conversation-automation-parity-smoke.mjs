import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium } from 'playwright-core';
import { startApp } from '../../test-bridge.ts';

const output = resolve(process.argv[2] ?? 'test-support/ui-smoke/artifacts/conversation-automation');
const scratch = await mkdtemp(join(tmpdir(), 'minarbot-conversation-automation-'));
const folder = await mkdtemp(join(await realpath('node_modules'), '.conversation-automation-project-'));
await mkdir(output, { recursive: true });
const ids = Object.fromEntries(['agent', 'other', 'project', 'section', 'routine', 'workflow', 'foreign', 'machine', 'remote', 'mail'].map(key => [key, randomUUID()]));
const provider = createServer(async (req, res) => {
  for await (const chunk of req) void chunk;
  res.setHeader('Content-Type', 'application/json');
  res.end(JSON.stringify(req.method === 'GET' ? { data: [{ id: 'fixture' }] }
    : { choices: [{ finish_reason: 'stop', message: { content: '**Fixture result ready.**' } }] }));
});
await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));

const sizes = { desktop: { width: 1440, height: 900 }, narrow: { width: 390, height: 844 } };
const now = Date.UTC(2026, 10, 20, 12, 0);
const clients = process.env.MINARBOT_SMOKE_UI ? [process.env.MINARBOT_SMOKE_UI] : ['react', 'leptos'];
const samples = Number(process.env.MINARBOT_CONVERSATION_AUTOMATION_BENCH ?? 0);
assert(Number.isSafeInteger(samples) && samples >= 0);
const fixture = { id: ids.mail, subject: 'Fixture receipt', from: 'sender@example.test', to: 'owner@example.test', text: 'The fixture arrived. <b>Plain text.</b>', received: now };
const digest = 'Morning digest';
const wfName = 'Owner workflow';
let browser;

async function exercise(client) {
  const leptos = client === 'leptos';
  const app = await startApp({
    database: join(scratch, client + '.sqlite'), dist: resolve('dist'), port: 0, appearanceClient: client,
    defaultModel: 'fixture', baseURL: `http://127.0.0.1:${provider.address().port}/v1`,
    stream: false,
  });
  const { origin } = app;
  const errors = [], captureNames = [];
  const machine = {
    id: ids.machine, environmentId: ids.machine, name: 'Studio workstation', origin: 'https://studio.example.test:4317',
    status: 'online', remoteAgentMessaging: true, lastSeen: now, agentCount: 1, pairingState: 'paired',
    agents: [{ id: ids.remote, name: 'Remote Iris', role: 'Reviewer', model: 'fixture', status: 'idle' }],
  };
  const messages = new Map();
  const attempts = [];
  let loseReply = false;
  async function context(viewport = sizes.desktop) {
    const context = await browser.newContext({ viewport, timezoneId: 'UTC', locale: 'en-US' });
    await context.addInitScript(() => {
      localStorage.setItem('minarbot:details-open', '1');
      localStorage.setItem('minarbot:sidebar-tab', 'agents');
      window.violations = [];
      addEventListener('securitypolicyviolation', event => window.violations.push(event.effectiveDirective));
    });
    await context.route('**/api/machines', route => route.fulfill({ json: { machines: [machine] } }));
    await context.route(`**/api/agents/${ids.agent}/email**`, route => {
      const url = new URL(route.request().url());
      if (url.pathname.endsWith('/email')) {
        const after = url.searchParams.has('before');
        return route.fulfill({ json: { address: fixture.to, items: after ? [] : [fixture], nextBefore: after ? null : 1 } });
      }
      assert(url.pathname.endsWith('/messages/' + fixture.id));
      return route.fulfill({ json: fixture });
    });
    await context.route('**/api/remote-agents/messages**', async route => {
      if (route.request().method() === 'GET') {
        assert.deepEqual(Object.fromEntries(new URL(route.request().url()).searchParams), { machine: ids.machine, agent: ids.remote });
        return route.fulfill({ json: { messages: [...messages.values()] } });
      }
      const command = route.request().postDataJSON();
      attempts.push(command);
      assert.deepEqual({ machine: command.machine, agent: command.agent }, { machine: ids.machine, agent: ids.remote });
      if (!messages.has(command.id)) messages.set(command.id, { id: command.id, text: command.text, status: 'done', state: 'done', reply: '**Remote reply ready.**', error: null, created: now });
      if (loseReply) return route.fulfill({ status: 200, contentType: 'application/json', body: '{lost acknowledgement' });
      return route.fulfill({ status: 202, json: { id: command.id } });
    });
    return context;
  }
  const primary = await context();
  const page = await primary.newPage();
  page.setDefaultTimeout(15_000);
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', event => { if (event.type() === 'error') errors.push(event.text()); });
  const button = (name, scope = page) => scope.getByRole('button', { name, exact: true });
  const linkOrButton = (name, scope = page) => scope.getByRole('link', { name, exact: true }).or(button(name, scope));
  const heading = name => page.getByRole('heading', { name, exact: true });
  const chat = `/agents/${ids.agent}`;
  const inbox = chat + '/inbox';
  const remote = `/machines/${ids.machine}/agents/${ids.remote}`;
  const pilot = path => path + (leptos && !path.includes('?') ? '?ui=leptos' : '');
  const details = page.getByRole('complementary', { name: 'Conversation details' });

  async function api(path, body) {
    const response = await fetch(origin + path, body === undefined ? {} : {
      method: 'POST', headers: { Origin: origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body),
    });
    assert(response.ok, `${path}: ${response.status} ${response.ok ? "" : await response.text()}`);
    return response.json();
  }
  async function open(path, tab = page, expected = leptos) {
    const response = await tab.goto(origin + path);
    assert.equal(response.status(), 200);
    assert.equal((await response.headerValue('content-security-policy')).includes('wasm-unsafe-eval'), expected);
  }
  async function capture(name, setup = async () => {}, reset = async () => {}) {
    captureNames.push(name);
    await page.mouse.move(1, 1);
    for (const [size, viewport] of Object.entries(sizes)) {
      await page.setViewportSize(viewport);
      await setup(size);
      for (const theme of ['dark', 'light']) {
        await page.evaluate(value => { document.documentElement.dataset.theme = value; }, theme);
        assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), `${client} ${name} overflows ${size}`);
        await page.screenshot({ path: join(output, `${client}-${name}-${size}-${theme}.png`), animations: 'disabled',
          mask: [page.locator('[data-private-token]'), page.locator('input[type=password]')] });
      }
      await reset(size);
    }
    await page.setViewportSize(sizes.desktop);
  }
  async function waitForApi(path, accept) {
    for (let i = 0; i < 100; i++) {
      const value = await api(path);
      if (accept(value)) return value;
      await delay(50);
    }
    assert.fail(`Timeout waiting for ${path}`);
  }
  async function workflowMenu(tab = page) {
    await tab.locator('.chat__pill').press('Enter');
    await tab.getByRole('navigation', { name: /^Menu / }).getByText('Workflows', { exact: true })
      .or(tab.getByRole('menu', { name: /^Menu / }).getByText('Workflows', { exact: true })).click();
    await tab.getByRole('heading', { name: 'Workflow library', exact: true }).waitFor();
  }
  const reveal = async () => {
    const trigger = button('Show details');
    if (await trigger.isVisible()) await trigger.click();
  };
  const capturePanel = async (name, upcoming) => {
    await capture(name, async size => {
      if (size === 'narrow') await reveal();
      const tab = details.getByRole('tab', { name: name.startsWith('section') ? 'Members' : 'Chats', exact: true });
      await tab.click();
      if (!await upcoming.getByText(digest, { exact: true }).isVisible()) {
        await upcoming.locator('summary').or(upcoming.getByRole('button', { name: /^Upcoming/ })).first().press('Enter');
      }
      await upcoming.getByText(digest, { exact: true }).waitFor();
      if (name === 'agent-upcoming') {
        const timestamps = await upcoming.locator('time').evaluateAll(nodes =>
          nodes.map(node => [node.dateTime, node.textContent]));
        const [short, long] = timestamps;
        assert.equal(short[1], new Date(short[0]).toLocaleString('en-US', {
          timeZone: 'UTC', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit',
        }));
        assert.equal(long[1], new Date(long[0]).toLocaleString('en-US', { timeZone: 'UTC' }));
      }
    }, async size => {
      if (size === 'narrow' && await details.isVisible()) await button('Close details', details).click();
    });
  };

  try {
    for (const [id, name] of [[ids.agent, 'Routine owner'], [ids.other, 'Other workflow agent']]) {
      await api('/api/agents', { id, runtime: 'api', name, model: 'fixture', instructions: 'Synthetic fixture.' });
    }
    let project = await api('/api/projects', { id: ids.project, name: 'Fixture project', folders: [{ mode: 'existing', path: folder }], members: [{ agent: ids.agent, write: false }] });
    project = await api(`/api/projects/${project.id}/sections`, { id: ids.section, revision: project.revision, name: 'Reports' });
    const room = project.sections.find(section => section.name === 'Reports').id;
    const routine = await api(`${chat.replace(/^/, '/api')}/routines`, {
      id: ids.routine, trigger: 'schedule', enabled: true, name: digest, prompt: 'Prepare fixture digest.',
      zone: 'UTC', schedule: { kind: 'cron', expression: '0 9 * * 1-5' }, reportTo: room, project: ids.project,
    });
    const run = await api(`/api/routines/${routine.id}/test`, { id: randomUUID(), revision: routine.revision });
    await waitForApi(`/api/channels/${room}/turns`, rows => rows.some(row => row.routineReport?.run === run.turn));

    for (const [agent, id, name] of [[ids.agent, ids.workflow, wfName], [ids.other, ids.foreign, 'Other agent workflow']]) {
      await api(`/api/agents/${agent}/workflows`, {
        id, revision: 0, name, graph: { entry: 'work', maxSteps: 5, nodes: [
          { id: 'work', kind: 'agent', agent, prompt: 'Prepare fixture workflow.', next: 'out' },
          { id: 'out', kind: 'output' },
        ], positions: { work: { x: 64, y: 64 }, out: { x: 344, y: 64 } } },
      });
    }

    await open(pilot(chat));
    await page.locator('.chat__pill').waitFor();
    await reveal();
    assert((await api(`/api/agents/${ids.agent}/routines`)).some(row => row.name === digest && row.enabled && row.trigger === 'schedule'));
    await capturePanel('agent-upcoming', details.getByRole('region', { name: 'Upcoming routines' }));
    await reveal();
    await linkOrButton('Manage', details).click();
    await heading('Routines').waitFor();
    assert.equal(new URL(page.url()).searchParams.get('agent'), ids.agent);

    const roomPath = `/rooms/${room}`;
    await open(pilot(roomPath));
    const report = page.getByRole('region', { name: 'Routine report: ' + digest });
    await report.getByText('Fixture result ready.', { exact: false }).waitFor();
    await reveal();
    await capturePanel('section-upcoming-report', details.getByRole('region', { name: 'Routines reporting here' }));
    await report.getByRole('link', { name: 'Open full run' }).click();
    await page.getByRole('region', { name: 'Conversation history' }).getByText('Prepare fixture digest.', { exact: true }).waitFor();

    await open(pilot(chat));
    const documentStart = await page.evaluate(() => performance.timeOrigin);
    if (leptos) {
      assert(!(await page.evaluate(() => performance.getEntriesByType('resource')
        .some(entry => /\/workflows\.[\da-f]+\.wasm$/.test(entry.name)))), 'Workflows WASM loaded with chat');
    }
    await workflowMenu();
    await page.getByText(wfName, { exact: true }).waitFor();
    assert.equal(await page.getByText('Other agent workflow', { exact: true }).count(), 0);
    assert.equal(await page.evaluate(() => performance.timeOrigin), documentStart);
    if (leptos) {
      assert.equal(new URL(page.url()).searchParams.get('agent'), ids.agent);
      await page.goBack();
      await page.waitForFunction(() => document.activeElement?.classList.contains('chat__pill'));
      const listPath = `**/api/agents/${ids.agent}/workflows`;
      const pending = Promise.withResolvers();
      await page.route(listPath, route => pending.resolve(route), { times: 1 });
      await page.goForward();
      const revalidation = await pending.promise;
      try {
        await page.getByText(wfName, { exact: true }).waitFor();
      } finally {
        await revalidation.continue();
      }
      await page.locator('button[aria-label="Owner workflow actions"]:enabled').waitFor();
      assert.equal(await page.evaluate(() => performance.timeOrigin), documentStart);
    }
    await capture('workflow-library');
    await button(wfName + ' actions').press('Enter');
    await page.getByRole('menuitem', { name: 'Edit ' + wfName }).press('Enter');
    await heading('Edit workflow').waitFor();
    const node = page.getByRole('region', { name: /Workflow graph/ }).getByRole('button', { name: /work Agent/ });
    await node.press('Alt+ArrowRight');
    await capture('workflow-editor');
    await linkOrButton('Back').click();
    const leave = page.getByRole('alertdialog');
    if (await leave.isVisible()) await leave.getByRole('button', { name: 'Leave', exact: true }).click();
    await page.getByText(wfName, { exact: true }).click();
    await heading(wfName).waitFor();
    await page.getByLabel('Workflow start time', { exact: true }).waitFor();
    await capture('workflow-forms');
    await page.getByRole('button', { name: /^Run version / }).click();
    const step = linkOrButton('Open step work');
    await step.waitFor();
    await page.locator('.workflow-run-node').filter({ hasText: /work.*Completed/s }).waitFor();
    await capture('workflow-inspector', () => step.scrollIntoViewIfNeeded());
    await step.press('Enter');
    const stepBack = button('Back to run ' + wfName);
    await stepBack.waitFor();
    await page.getByRole('region', { name: 'Conversation history' }).getByText(/Prepare fixture workflow\./).waitFor();
    await page.waitForFunction(() => document.activeElement?.textContent === 'Back to run Owner workflow');
    await capture('workflow-step', async size => {
      if (size === 'narrow') {
        assert.equal(await page.locator('dialog:modal').count(), 0, 'Step details covered the narrow transcript');
        assert(await page.getByRole('region', { name: 'Conversation history' }).isVisible());
      }
    });
    await button('Back to run ' + wfName).press('Enter');
    await page.waitForFunction(() => document.activeElement?.textContent === 'Open step work');

    await open(inbox);
    await heading('Inbox').waitFor();
    const message = page.getByRole('button', { name: /^Fixture receipt/ });
    await message.waitFor();
    await capture('inbox');
    await message.press('Enter');
    await heading(fixture.subject).waitFor();
    await page.getByText(fixture.text, { exact: true }).waitFor();
    assert(await heading(fixture.subject).evaluate(el => document.activeElement === el));
    await capture('inbox-message');
    await button('Back to message list').press('Enter');
    await page.waitForFunction(() => document.activeElement?.id.startsWith('inbox-message-'));
    await button('Older messages').press('Enter');
    await page.getByText('No messages', { exact: true }).waitFor();
    await button('Newer messages').press('Enter');
    await message.waitFor();

    await open(remote);
    const composer = page.getByRole('textbox', { name: 'Message Remote Iris on Studio workstation' });
    await composer.waitFor();
    await capture('remote-empty');
    await composer.fill('Saved fixture draft');
    await page.reload();
    await composer.waitFor();
    assert.equal(await composer.inputValue(), 'Saved fixture draft');
    loseReply = true;
    await composer.press('Enter');
    await page.getByText('Message submission is not confirmed', { exact: true }).waitFor();
    await page.reload();
    await button('Retry send').waitFor();
    assert(await composer.isDisabled());
    await capture('remote-recovery');
    loseReply = false;
    await button('Retry send').press('Enter');
    await page.locator('.message-body .markdown strong').getByText('Remote reply ready.').waitFor();
    assert.deepEqual(attempts[0], attempts[1]);
    assert.equal(await page.locator('.human-bubble').count(), 1);
    await capture('remote-reply');
    machine.status = 'offline';
    await page.getByText('Offline. Reconnect the machine before sending.', { exact: true }).waitFor();
    await page.getByText('Offline · Last known agents', { exact: true }).waitFor();
    assert(await button('Send message').isDisabled());
    await capture('remote-offline');
    machine.status = 'online';

    assert.deepEqual(await page.evaluate(() => window.violations), []);
    assert.deepEqual(errors, []);
    await open(inbox + '?ui=react', page, false);
    await heading('Inbox').waitFor();

    const timings = {};
    const boundary = {
      inbox: [inbox, '.agent-inbox__list li button'],
      remote: [remote, '#remote-message'],
      upcoming: [pilot(chat), '[aria-label="Upcoming routines"] summary, [aria-label="Upcoming routines"] button'],
    };
    for (const [name, [path, selector]] of Object.entries(boundary)) {
      const cold = [], warm = [];
      for (let run = 0; run < samples; run++) {
        const bench = await context();
        const tab = await bench.newPage();
        try {
          let start = performance.now();
          await open(path, tab);
          await tab.locator(selector).first().waitFor();
          cold.push(performance.now() - start);
          start = performance.now();
          await tab.reload();
          await tab.locator(selector).first().waitFor();
          warm.push(performance.now() - start);
        } finally { await bench.close(); }
      }
      timings[name] = { cold: stats(cold), warm: stats(warm) };
    }
    const workflowCold = [], workflowWarm = [];
    for (let run = 0; run < samples; run++) {
      const bench = await context();
      const tab = await bench.newPage();
      try {
        await open(pilot(chat), tab);
        for (const result of [workflowCold, workflowWarm]) {
          const began = performance.now();
          await workflowMenu(tab);
          await tab.getByText(wfName, { exact: true }).waitFor();
          result.push(performance.now() - began);
          await linkOrButton('Back', tab).click();
          await tab.locator('.chat__pill').waitFor();
        }
      } finally { await bench.close(); }
    }
    timings.workflow = { cold: stats(workflowCold), warm: stats(workflowWarm) };
    return { captureNames, timings, consoleErrors: errors.length, cspErrors: 0 };
  } catch (error) {
    await page.screenshot({ path: join(output, `${client}-failure.png`), animations: 'disabled',
      mask: [page.locator('[data-private-token]'), page.locator('input[type=password]')] });
    throw error;
  } finally {
    await primary.close();
    await app.close();
  }
}

function stats(values) {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const at = percentile => +sorted[Math.ceil(sorted.length * percentile) - 1].toFixed(1);
  return { samples: values.length, medianMs: at(0.5), p95Ms: at(0.95) };
}

try {
  browser = await chromium.launch(process.env.CHROME_PATH ? { executablePath: process.env.CHROME_PATH } : {});
  const receipt = {};
  for (const client of clients) {
    receipt[client] = await exercise(client);
    await writeFile(join(output, 'receipt.json'), JSON.stringify(receipt, null, 2) + '\n');
    console.log(`${client} conversation automation parity passed (${receipt[client].captureNames.length * 4} captures).`);
  }
  if (samples && receipt.react && receipt.leptos) {
    const react = receipt.react.timings.workflow.warm.p95Ms;
    const leptos = receipt.leptos.timings.workflow.warm.p95Ms;
    console.log(`Scoped Workflows warm p95 ${leptos} ms (Leptos), ${react} ms (React).`);
    assert(leptos <= react * 1.1, 'Scoped Workflows exceeded the warm p95 budget');
  }
} finally {
  await browser?.close();
  provider.closeAllConnections();
  await new Promise(resolve => provider.close(resolve));
  await Promise.all([rm(scratch, { recursive: true }), rm(folder, { recursive: true })]);
}
