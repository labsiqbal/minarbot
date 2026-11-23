import assert from 'node:assert/strict';
import { assertP95 } from './timing-budgets.mjs';
import { fixDate, fixtureClock, parityShot } from './parity-shot.mjs';
import { createHash, randomUUID } from 'node:crypto';
import { chmod, mkdir, mkdtemp, writeFile } from 'node:fs/promises';
import { createServer as socketServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import { startRustServer } from '../../rust-test-server.ts';
import { testDatabase } from '../database.ts';

const scratch = await mkdtemp(join(tmpdir(), 'mb78-'));
const output = resolve(process.argv[2] ?? scratch);
const dist = fileURLToPath(new URL('../../dist', import.meta.url));
const samples = Number(process.env.MINARBOT_PROMPTS_BENCH ?? 60);
assert(Number.isInteger(samples) && samples >= 0);
await mkdir(output, { recursive: true });

const shapes = { desktop: { width: 1440, height: 900 }, narrow: { width: 390, height: 844 } };
const themes = ['dark', 'light'];
const notes = 'Fixture release. Restart and health verification use a disposable launcher socket.';
const version = '99.0.1';
const commit = '01234567'.repeat(5);
const release = `${version}-${commit.slice(0, 8)}`;
const pkg = JSON.stringify({ version });
const meta = { id: release, version, commit, node: process.version, notes, created: '2026-11-22T12:00:00.000Z',
  files: { 'package.json': createHash('sha256').update(pkg).digest('hex') } };
const ids = { owner: randomUUID(), hidden: randomUUID(), thread: randomUUID(), turn: randomUUID(), question: randomUUID(),
  approvalTurn: randomUUID(), approval: 'fixture-app-action', harnessTurn: randomUUID(), harness: randomUUID() };

async function launcher(client) {
  const root = join(scratch, client), releasesDir = join(root, 'rel'), launcherSocket = join(root, 'ctl');
  await mkdir(join(releasesDir, release), { recursive: true });
  await mkdir(join(releasesDir, 'feed'), { recursive: true });
  await writeFile(join(releasesDir, release, 'package.json'), pkg);
  for (const path of [join(releasesDir, release, 'release.json'), join(releasesDir, 'feed/latest.json')]) {
    await writeFile(path, JSON.stringify(meta));
  }
  const commands = [];
  let attempt;
  const socket = socketServer(connection => {
    let body = '';
    connection.on('data', chunk => {
      body += chunk;
      if (!body.endsWith('\n')) return;
      const command = JSON.parse(body);
      if (command.type === 'status') {
        connection.end(JSON.stringify({ protocol: 1, current: 'development', ...(attempt && { lastAttempt: attempt }) }) + '\n');
      } else {
        commands.push(command);
        attempt = { id: command.id, version: command.version, status: 'starting' };
        connection.end(JSON.stringify({ accepted: true, id: command.id }) + '\n');
      }
    });
  });
  await new Promise((resolve, reject) => {
    socket.once('error', reject);
    socket.listen(launcherSocket, resolve);
  });
  await chmod(launcherSocket, 0o600);
  return { root, updates: { releasesDir, launcherSocket }, commands, finish: () => { attempt.status = 'done'; },
    close: () => new Promise(resolve => socket.close(resolve)) };
}

function stats(values) {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const percentile = ratio => Number(sorted[Math.max(0, Math.ceil(sorted.length * ratio) - 1)].toFixed(1));
  return { samples: sorted.length, median: percentile(.5), p95: percentile(.95) };
}
function observer(page, errors) {
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', event => { if (event.type() === 'error') errors.push(event.text()); });
  return page;
}

async function* exercise(browser, client) {
  const ctl = await launcher(client);
  let app, context;
  try {
    const database = join(ctl.root, 'state.sqlite');
    app = await startRustServer({ database, dist, fixedClock: fixtureClock, defaultModel: 'fixture',
      baseURL: 'http://127.0.0.1:9/v1', apiKey: 'fixture',
      updates: ctl.updates, harness: { executables: Object.fromEntries(
        ['codex', 'claude', 'hermes', 'antigravity', 'grok', 'pi', 'opencode', 'gemini', 'kiro'].map(runtime => [runtime, join(ctl.root, runtime)]),
      ) } });
    async function api(path, data) {
      const response = await fetch(app.origin + path, data ? {
        method: 'POST', headers: { 'Content-Type': 'application/json', Origin: app.origin }, body: JSON.stringify(data),
      } : {});
      assert(response.ok, `${path} returned ${response.status}`);
      return response.json();
    }
    const { guide } = await api('/api/agents');
    for (const [id, name] of [[ids.owner, 'Review agent'], [ids.hidden, 'Archived helper']]) {
      await api('/api/agents', { id, runtime: 'api', name, model: 'fixture', instructions: 'Fixture profile.' });
    }
    await api(`/api/agents/${ids.hidden}/visibility`, { id: randomUUID(), revision: 0, hidden: true });
    await api(`/api/agents/${ids.owner}/threads`, { id: ids.thread, name: 'Daily follow-up' });
    const db = testDatabase(database);
    try {
      db.prepare("INSERT INTO turns(id,agent,text,status,created,thread,paused) VALUES(?,?,?,'queued',?,?,1)")
        .run(ids.turn, ids.owner, 'Check draft before continuing.', '2026-11-22T12:00:00.000Z', ids.thread);
      db.prepare("INSERT INTO questions(id,turn,call_id,call,prompt,choices,after_output,after_action) VALUES(?,?,?,'{}',?,? ,0,0)")
        .run(ids.question, ids.turn, 'fixture-question', 'Which version should be reviewed?', JSON.stringify(['Draft', 'Final']));
      const turn = db.prepare('INSERT INTO turns(id,agent,text,status,created,thread,paused) VALUES(?,?,?,?,?,?,?)');
      turn.run(ids.approvalTurn, ids.owner, 'Review the connected app action.', 'queued', '2026-11-22T12:01:00.000Z', ids.thread, 1);
      turn.run(ids.harnessTurn, ids.owner, 'Review the local task.', 'running', '2026-11-22T12:02:00.000Z', ids.thread, 0);
      db.prepare("INSERT INTO composio_calls VALUES(?,?,?,?,?,'approval','pending',NULL,0,0,1)")
        .run(ids.approvalTurn, ids.approval, ids.owner, JSON.stringify({ function: { arguments: JSON.stringify({ subject: 'Fixture draft' }) } }), JSON.stringify({
          name: 'Create draft', toolkit: 'gmail', slug: 'GMAIL_CREATE_EMAIL_DRAFT',
          account: 'Fixture account', arguments: { subject: 'Fixture draft' },
        }));
      db.prepare("INSERT INTO harness_approvals(id,turn,agent,runtime,workspace,kind,description,command,prefix,input,state) VALUES(?,?,?,'api','fixture','command',?,?,?,'{}','approval')")
        .run(ids.harness, ids.harnessTurn, ids.owner, 'Review workspace task', 'git status', 'git status');
    } finally { db.close(); }
    assert.equal((await api('/api/updates')).available.version, version);
    const errors = [], warm = [], cold = [];
    context = await browser.newContext({ viewport: shapes.desktop, timezoneId: 'UTC', reducedMotion: 'reduce' });
    await context.addInitScript(() => {
      localStorage.setItem('minarbot:details-open', '0');
      window.promptViolations = [];
      addEventListener('securitypolicyviolation', event => window.promptViolations.push(event.effectiveDirective));
    });
    const page = observer(await context.newPage(), errors);
    // Update polling measures elapsed wall time.
    await fixDate(page, fixtureClock, { tick: true });
    const guidePath = `/agents/${guide.id}`;
    const action = label => page.getByRole('button', { name: label, exact: true });
    const dialog = label => page.getByRole('dialog', { name: label, exact: true });
    const capture = async (name, region, { needsSidebar = false, center = false } = {}) => {
      for (const [layout, viewport] of Object.entries(shapes)) {
        await page.setViewportSize(viewport);
        if (needsSidebar && layout === 'narrow') await action('Expand sidebar').click();
        for (const theme of themes) {
          await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
          assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
          if (center) await region.evaluate(async node => {
            await document.fonts.ready;
            await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
            node.scrollIntoView({ block: 'center', behavior: 'instant' });
            const transcript = node.closest('.transcript');
            const visible = () => {
              const card = node.getBoundingClientRect();
              const viewport = transcript.getBoundingClientRect();
              return card.top >= viewport.top && card.bottom <= viewport.bottom;
            };
            for (let frame = 0; frame < 8; frame++) {
              const before = transcript.scrollTop;
              await new Promise(resolve => requestAnimationFrame(resolve));
              if (transcript.scrollTop === before && visible()) return;
              node.scrollIntoView({ block: 'center', behavior: 'instant' });
            }
            throw new Error('Approval card did not settle inside the transcript');
          });
          await parityShot(page, join(output, `${client}-${name}-${layout}-${theme}.png`), region);
        }
        if (needsSidebar && layout === 'narrow') await page.keyboard.press('Escape');
      }
      await page.setViewportSize(shapes.desktop);
    };
    const compose = page.getByRole('textbox', { name: `Message ${guide.name}`, exact: true });
    await page.goto(app.origin + guidePath);
    await compose.waitFor();
    assert.equal(await page.locator('#leptos-root').count(), 1);
    await capture('guide-chat', page.locator('main.chat'));
    const menu = page.locator('.chat__pill');
    await menu.click();
    await page.getByRole('button', { name: 'AI connection', exact: true })
      .or(page.getByRole('menuitem', { name: 'AI connection', exact: true })).click();
    const connection = dialog(`AI connection for ${guide.name}`);
    await connection.getByRole('combobox', { name: 'Run with' }).waitFor();
    await capture('guide-connection', connection);
    await page.keyboard.press('Escape');
    await connection.waitFor({ state: 'hidden' });

    const quick = dialog('Quick switcher');
    const search = quick.getByRole('combobox', { name: 'Jump to', exact: true });
    const openQuick = async () => {
      await page.keyboard.press('Control+k');
      await search.waitFor();
      assert(await search.evaluate(node => node === document.activeElement));
    };
    await action('Quick switcher (Ctrl/Cmd+K)').click();
    await search.fill('follow daily');
    await quick.getByRole('option', { name: /Daily follow-up/ }).waitFor();
    await search.press('Home');
    assert.equal(await search.evaluate(node => document.getElementById(node.getAttribute('aria-activedescendant')).getAttribute('aria-selected')), 'true');
    await page.keyboard.press('Escape');
    await quick.waitFor({ state: 'hidden' });
    await page.waitForFunction(() => document.activeElement?.getAttribute('aria-label') === 'Quick switcher (Ctrl/Cmd+K)');
    await openQuick();
    await search.fill('follow daily');
    await quick.getByRole('option', { name: /Daily follow-up/ }).waitFor();
    await capture('quick-switcher', quick);
    await page.keyboard.press('Escape');
    await quick.waitFor({ state: 'hidden' });
    await openQuick();
    await search.fill('Archived helper');
    await quick.getByRole('option', { name: 'Restore Archived helper' }).waitFor();
    await search.press('Enter');
    await page.getByText('Archived helper', { exact: true }).waitFor();
    assert.equal((await api('/api/agents')).agents.find(agent => agent.id === ids.hidden).hidden, false);

    await action('Help').click();
    const help = dialog('Keyboard shortcuts');
    await help.getByText('Focus message composer', { exact: true }).waitFor();
    await capture('help', help);
    await page.keyboard.press('Escape');
    await help.waitFor({ state: 'hidden' });
    const capsule = page.locator('.sidebar-update__capsule');
    await capsule.waitFor();
    await capture('update-capsule', capsule, { needsSidebar: true });
    await capsule.click();
    const update = dialog('Update 1 machine?');
    await update.getByRole('button', { name: 'Release notes', exact: true }).click();
    await update.getByText(notes, { exact: true }).waitFor();
    await capture('update-confirmation', update);
    await update.getByRole('button', { name: 'Update', exact: true }).click();
    await page.getByText('Restarting', { exact: true }).waitFor();
    assert.equal(ctl.commands.length, 1);
    assert.equal(ctl.commands[0].version, version);
    await page.reload();
    const progress = dialog('Updating 1 machine');
    await progress.getByText('Restarting', { exact: true }).waitFor();
    assert.equal(ctl.commands.length, 1);
    ctl.finish();
    await dialog('Update complete').getByRole('button', { name: 'Close', exact: true }).last().click();
    await page.waitForFunction(() => !sessionStorage.getItem('minarbot:update-everything'));

    const row = page.locator('.bot-row').filter({ hasText: 'Review agent' }).first();
    await row.getByText('Needs you', { exact: true }).last().waitFor();
    await row.click();
    await page.waitForURL(url => url.pathname === `/agents/${ids.owner}/conversations/${ids.thread}`);
    const cardId = `question:${ids.turn}:${ids.question}`;
    const card = page.locator(`[id="${cardId}"]`);
    await page.waitForFunction(id => document.activeElement?.id === id, cardId);
    await capture('question', card);
    await card.getByRole('radio', { name: 'Draft' }).check();
    await page.reload();
    await card.getByRole('radio', { name: 'Draft' }).waitFor();
    assert(await card.getByRole('radio', { name: 'Draft' }).isChecked());
    await card.getByRole('radio', { name: 'Final' }).check();
    const sent = page.waitForResponse(response => response.url().includes(`/questions/${ids.question}/answer`) && response.request().method() === 'POST');
    await card.getByRole('button', { name: 'Answer', exact: true }).click();
    const receipt = await sent;
    assert(receipt.ok(), `answer returned ${receipt.status()}`);
    const command = receipt.request().postDataJSON();
    assert.deepEqual(Object.keys(command).sort(), ['acknowledgeUncertain', 'id', 'text']);
    assert.equal(command.text, 'Final');
    const history = () => api(`/api/threads/${ids.thread}/turns`);
    assert.equal((await history()).find(turn => turn.id === ids.turn).questions[0].answer, 'Final');
    await card.getByRole('radio', { name: 'Final', exact: true }).waitFor({ state: 'hidden' });
    await page.reload();
    for (const [kind, turn, approval] of [
      ['app', ids.approvalTurn, ids.approval], ['harness', ids.harnessTurn, ids.harness],
    ]) {
      const region = page.locator(`[id="${kind}:${turn}:${approval}"]`);
      await page.mouse.move(0, 0);
      await capture(`${kind}-approval`, region, { center: true });
      const endpoint = `/api/threads/${ids.thread}/turns/${turn}/${kind}-approvals/${approval}`;
      const key = `minarbot:${kind}-approval:/api/threads/${ids.thread}/turns/${turn}:${approval}`;
      const command = { id: randomUUID(), revision: 1, decision: 'deny' };
      await page.evaluate(({ key, entry }) => sessionStorage.setItem(key, JSON.stringify(entry)),
        { key, entry: kind === 'app' ? { path: endpoint, body: command } : command });
      await page.reload();
      const receipt = page.waitForResponse(response => response.url().endsWith(endpoint) && response.request().method() === 'POST');
      const label = 'Retry saved decision';
      await region.getByRole('button', { name: label, exact: true }).click();
      const response = await receipt;
      assert(response.ok(), `${kind} decision returned ${response.status()}`);
      assert.deepEqual(response.request().postDataJSON(), command);
      const saved = (await history()).find(row => row.id === turn);
      assert.equal(saved[kind === 'app' ? 'approvals' : 'harnessApprovals'][0].decision, 'deny');
    }
    assert.deepEqual(await page.evaluate(() => window.promptViolations), []);
    assert.deepEqual(errors, []);

    await page.goto(app.origin + guidePath);
    await compose.waitFor();
    await openQuick();
    await page.keyboard.press('Escape');
    await quick.waitFor({ state: 'hidden' });
    yield;

    for (let i = 0; i < samples; i++) {
      await page.bringToFront();
      const start = performance.now();
      await openQuick();
      warm.push(performance.now() - start);
      await page.keyboard.press('Escape');
      await quick.waitFor({ state: 'hidden' });
      const coldContext = await browser.newContext({ viewport: shapes.desktop });
      try {
        const page = observer(await coldContext.newPage(), errors);
        const began = performance.now();
        await page.goto(app.origin + guidePath, { waitUntil: 'commit' });
        await page.getByRole('textbox', { name: `Message ${guide.name}`, exact: true }).waitFor();
        cold.push(performance.now() - began);
      } finally { await coldContext.close(); }
      yield;
    }
    assert.deepEqual(errors, []);
    return { coldGuideMs: stats(cold), warmSwitcherMs: stats(warm), updateSubmissions: ctl.commands.length,
      consoleErrors: errors.length };
  } finally {
    await context?.close();
    await app?.close();
    await ctl.close();
  }
}

const browser = await chromium.launch({ executablePath: process.env.CHROME ?? '/usr/bin/google-chrome', args: ['--no-sandbox'] });
const run = exercise(browser, 'leptos');
try {
  await run.next();
  for (let i = 0; i < samples; i++) await run.next();
  const { value: result } = await run.next();
  await writeFile(join(output, 'receipt.json'), JSON.stringify(result, null, 2));
  console.log(JSON.stringify({ ...result, output }));
  if (samples) {
    assertP95('shell', 'cold', result.coldGuideMs.p95);
    assertP95('shell', 'warm', result.warmSwitcherMs.p95);
  }
} finally {
  await run.return();
  await browser.close();
}
