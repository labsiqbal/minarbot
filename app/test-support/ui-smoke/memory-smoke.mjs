import assert from 'node:assert/strict';
import { assertP95 } from './timing-budgets.mjs';
import { fixDate, parityChromiumArgs, parityShot } from './parity-shot.mjs';
import { createServer } from 'node:http';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from 'node:sqlite';
import { chromium } from 'playwright-core';
import { startApp } from '../../test-bridge.ts';

const scratch = await mkdtemp(join(tmpdir(), 'minarbot-memory-ui-'));
const output = resolve(process.argv[2] ?? await mkdtemp(join(tmpdir(), 'minarbot-memory-shots-')));
const database = join(scratch, 'fixture.sqlite');
const agent = '40000000-0000-4000-8000-000000000050', turn = '40000000-0000-4000-8000-000000000051', thread = '40000000-0000-4000-8000-000000000052', channel = '40000000-0000-4000-8000-000000000053';
const main = `/agents/${agent}`, named = `${main}/conversations/${thread}`, room = `/rooms/${channel}`;
const note = 'Start the greenhouse inventory on Mondays.';
const shared = 'Water the seedlings before noon.';
const summary = 'Keep orchid labels in the north cabinet.';
const observationText = 'The north cabinet holds orchid tray labels and inventory sheets.';
const clients = ['leptos'];
const sizes = { desktop: { width: 1440, height: 900 }, narrow: { width: 390, height: 844 } };
const samples = Number(process.env.MINARBOT_MEMORY_BENCH ?? 0);
// Source IDs visible in the three frozen summary captures.
const sourceIds = [
  [
    '4d742fe9-fdfc-4e9e-b4af-85b9f99d9114',
    '18cfc3df-8518-43aa-9dd9-c65e58bd8c7e',
    'da2aae6a-19a9-4245-8b3d-f8302b52161a',
    'fdec64dd-f588-4c41-9803-50ed179c292d',
    '40c68213-e839-4509-b667-248927a7bb14',
    '042f9629-ef1f-4816-9aab-15d3cec9b85d',
    'c35b2b78-74e2-442a-9f95-f902d762f173',
    '302c5d9a-5b2c-4ed5-ba83-947791907cc0',
    '56e0ec58-0f37-45a7-9488-57b924a88229',
    'e95c60a7-d498-46f0-ac41-3ed2fd6a822a',
    'e004464b-0c1d-4919-b69f-4a4a09d49912',
  ],
  [
    '17172bd4-b9cb-44a2-96b9-81355f1e6fe6',
    '61f9ba53-7969-4d0a-988e-5d0e8b4584bb',
    '67e2e4b2-b7e6-4295-84cb-6284f523f618',
    '6759cff7-e75a-4439-b8fb-6fc70012965f',
    '673a9b39-854f-4a93-950b-a304a2a2445c',
    'e7fb18b8-284e-4249-a71f-3c2de3d199c5',
    'd719f4af-ea46-4cc1-a45b-108362f46526',
    '7c1f15fd-fd29-426d-a01e-cc8bcd4a48d8',
    'bcacc281-0f4a-43da-aca7-d018c07f7a1e',
    '9df68dcd-c3b1-46ee-b208-a82dc112ea93',
    '5f5ee39d-c40e-471f-bb98-6ce801a87f28',
  ],
  [
    '457022c1-8000-495e-ba6e-d3c5d5161c19',
    '663c61c0-124a-4ff7-b9a6-f367e9706441',
    '0fe09060-9a4a-476c-aa2f-dae2d4db8751',
    '46909f5b-28f2-4287-8eee-aa946e46f358',
    'c72fad74-8f52-43f7-9b1d-1a6b0fe639c4',
    'ee700044-3e73-45ea-91d9-71dbc24f955d',
    '36e00da3-c235-426a-bee0-e0f5c2a930a5',
    '9566ec45-7a35-418a-9309-5e8f9d3660a2',
    'fc3b87bd-8f5d-45c1-9228-c235e3276792',
    'd1ce30c6-f39c-4238-b51d-245d501c25c5',
    '908cdbac-58f4-4b13-b713-5829b4ef05a0',
  ],
];
const errors = [], timings = {};
let nextId = 60;
const stableId = () => `40000000-0000-4000-8000-${String(nextId++).padStart(12, '0')}`;
const bank = `agents:${agent}`, memory = `/api/agents/${agent}/memory`;
const provider = createServer((request, response) => {
  response.setHeader('Content-Type', 'application/json');
  if (request.method === 'GET') response.end(JSON.stringify({ data: [{ id: 'fixture' }] }));
  else response.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: 'Fixture reply' } }] }));
});
let app, browser;
try {
  await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
  app = await startApp({ database, dist: fileURLToPath(new URL('../../dist', import.meta.url)), defaultModel: 'fixture', baseURL: `http://127.0.0.1:${provider.address().port}/v1`, apiKey: 'synthetic' });
  async function api(path, data) {
    const response = await fetch(app.origin + path, data ? { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(data) } : {});
    assert(response.ok, `${path}: ${response.status} ${await response.clone().text()}`);
    return response.json();
  }
  await api('/api/agents', { id: agent, name: 'Garden planner', model: 'fixture', instructions: 'Plan garden work.', runtime: 'api' });
  await api(`/api/agents/${agent}/threads`, { id: thread, name: 'Seedling planning' });
  await api('/api/channels', { id: channel, name: 'Greenhouse', instructions: 'Plan planting.', lead: agent, members: [agent] });
  await api(`/api/channels/${channel}/memory`, { id: stableId(), entry: stableId(), text: shared, revision: 0, essential: false, remove: false });
  await api(memory, { id: stableId(), entry: stableId(), text: 'Start the greenhouse inventory on Mondays.', revision: 0, essential: true, remove: false });
  const db = new DatabaseSync(database);
  try {
    const source = { kind: 'turn', id: turn, agent, thread, role: 'prompt', created: '2026-11-19T10:30:00Z' };
    const raw = db.prepare("INSERT INTO memory_units(bank,origin,source_id,kind,text,source) VALUES(?,'prompt',?,'experience',?,?) RETURNING id")
      .get(bank, turn, 'The north shelf has unlabelled orchid trays.', JSON.stringify(source)).id;
    const retained = db.prepare("INSERT INTO memory_units(bank,origin,source_id,kind,text,source) VALUES(?,'retained',?,?,?,?) RETURNING id");
    const fact = retained.get(bank, stableId(), 'experience', 'Orchid tray labels are in the north cabinet.', JSON.stringify({ kind: 'fact', raw, message: source })).id;
    const observation = retained.get(bank, stableId(), 'observation', 'The north cabinet holds orchid tray labels and inventory sheets.', JSON.stringify({ kind: 'observation' })).id;
    db.prepare('INSERT INTO memory_facts(unit,raw,offset,consolidated) VALUES(?,?,0,1)').run(fact, raw);
    db.prepare('INSERT INTO memory_observations VALUES(?,?,?,?)').run(observation, bank, agent, 'cabinet');
    db.prepare('INSERT INTO memory_observation_evidence VALUES(?,?)').run(observation, fact);
    const addTurn = db.prepare("INSERT INTO turns(id,agent,channel,thread,text,reply,status,created) VALUES (?,?,?,?,?,?,'done',?) RETURNING seq,revision");
    for (const [source, [scope, roomId, threadId]] of [[bank, null, null], [`threads:${thread}`, null, thread], [`channels:${channel}`, channel, null]].entries()) {
      let through = 0;
      for (let index = 1; index <= 21; index++) {
        const row = addTurn.get(sourceIds[source][index - 1] ?? stableId(), agent, roomId, threadId, `Fixture message ${index}`, 'Fixture reply', '2026-11-19T10:30:00Z');
        through = row.seq;
        db.prepare("INSERT INTO compaction_sources(scope,seq,revision,status) VALUES(?,?,?,'done')").run(scope, row.seq, row.revision);
      }
      db.prepare('INSERT INTO compactions(scope,version,through,summary) VALUES(?,?,?,?)').run(scope, stableId(), through, summary);
    }
    db.prepare('INSERT INTO memory_mental_models(bank,id,question,answer,evidence,generation) SELECT ?,?,?,?,?,generation FROM memory_bank_generations WHERE bank=?')
      .run(bank, stableId(), 'Where are the orchid supplies?', 'Labels and inventory sheets are in the north cabinet.', JSON.stringify([{ id: observation, kind: 'observation', text: 'North cabinet', source: [source] }]), bank);
  } finally { db.close(); }
  await mkdir(output, { recursive: true });
  browser = await chromium.launch({ executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE ?? '/usr/bin/google-chrome', args: parityChromiumArgs });
  const role = (root, kind, name) => root.getByRole(kind, { name, exact: true });
  const button = (root, name) => role(root, 'button', name);
  const dialog = (page, name) => role(page, 'dialog', name);

  async function makePage(client, theme, size) {
    const page = await browser.newPage({ viewport: sizes[size], reducedMotion: 'reduce', timezoneId: 'UTC' });
    await fixDate(page);
    page.on('response', response => {
      if (response.status() >= 400) errors.push(`${client}: HTTP ${response.status()} ${new URL(response.url()).pathname}`);
    });
    page.on('pageerror', error => errors.push(`${client}: ${error.message}`));
    page.on('console', entry => {
      if (entry.type() === 'error') errors.push(`${client}: ${entry.text()}`);
    });
    await page.addInitScript(theme => {
      localStorage.setItem('minarbot-presentation-v1', JSON.stringify({ version: 1, theme, textSize: 'standard' }));
      localStorage.setItem('minarbot:details-open', '0');
      globalThis.memoryCsp = [];
      addEventListener('securitypolicyviolation', event => globalThis.memoryCsp.push(event.effectiveDirective));
    }, theme);
    return page;
  }

  async function go(page, client, path) {
    await page.goto(`${app.origin}${path}`);
    await page.locator('.chat__pill').waitFor();
    assert.equal(await page.locator('#leptos-root').count(), Number(true));
  }

  async function menu(page, name) {
    await page.locator('.chat__pill').click();
    await role(page, 'menuitem', name).or(button(page, name)).first().click();
  }

  async function profile(page, client, fromSidebar = false) {
    const profile = dialog(page, 'Agent profile').or(dialog(page, 'Edit profile'));
    if (fromSidebar) {
      const expand = button(page, 'Expand sidebar');
      if (await expand.isVisible()) await expand.click();
      await page.getByRole('button', { name: /^Garden planner/ }).first().click({ button: 'right' });
      await role(page, 'menuitem', 'Edit').click();
    } else {
      await button(page, 'Show details').click();
      await button(page.getByRole('complementary', { name: 'Conversation details' }), 'Edit profile').click();
    }
    await profile.waitFor();
    const advanced = role(profile, 'tab', 'Advanced');
    if (await advanced.count()) await advanced.click();
    return profile;
  }

  async function openMemory(page, parent) {
    await button(parent, 'Agent memory').press('Enter');
    const memory = dialog(page, 'Minarbot Memory');
    await memory.getByRole('heading', { name: 'Minarbot Memory', level: 1, exact: true }).waitFor();
    await memory.getByText(note, { exact: true }).waitFor();
    return memory;
  }

  async function openSummary(page, parent) {
    await button(parent, 'History summary Up to date').click();
    const history = dialog(page, 'History summary');
    await history.getByText(summary, { exact: true }).waitFor();
    return history;
  }

  for (const theme of ['dark', 'light']) for (const size of Object.keys(sizes)) for (const client of clients) {
    const page = await makePage(client, theme, size);
    async function capture(name, region) {
      assert(await region.evaluate(el => el.scrollWidth <= el.clientWidth + 1), `${name} overflow`);
      const key = `${name}-${theme}-${size}`;
      const file = join(output, `${client}-${key}.png`);
      // Launch controls can leave the pointer over different rows in the opened panel.
      await page.mouse.move(0, 0);
      await parityShot(page, file, region);
    }
    try {
      await go(page, client, main);
      const parent = await profile(page, client, false);
      await parent.locator('.summary-row').scrollIntoViewIfNeeded();
      await button(parent, 'History summary Up to date').waitFor();
      await button(parent, 'Agent memory').scrollIntoViewIfNeeded();
      const memory = await openMemory(page, parent);
      assert.equal(await page.locator('html').getAttribute('data-theme'), theme);
      await capture('agent-notes', memory);
      await role(memory, 'tab', 'Notes').press('ArrowRight');
      const observation = memory.getByRole('listitem').filter({ hasText: observationText }).first();
      await observation.waitFor();
      await observation.locator('summary').press('Enter');
      assert.equal(await role(observation, 'link', 'User message').getAttribute('href'), named);
      await capture('learned', memory);
      await role(memory, 'tab', 'Standing questions').press('Enter');
      await memory.getByText('Labels and inventory sheets are in the north cabinet.', { exact: true }).waitFor();
      await capture('questions', memory);
      await page.keyboard.press('Escape');
      await memory.waitFor({ state: 'hidden' });
      await page.waitForFunction(el => document.activeElement === el, await button(parent, 'Agent memory').elementHandle());

      const history = await openSummary(page, parent);
      await capture('profile-summary', history);
      await button(history, 'Next sources').click();
      await role(history, 'heading', 'Source list').and(page.locator(':focus')).waitFor();
      assert.equal(await history.locator('.k-row').count(), 1);
      await button(history, 'Previous sources').click();
      await page.keyboard.press('Escape');

      await go(page, client, named);
      await menu(page, 'Conversation settings');
      const settings = dialog(page, 'Conversation settings');
      await button(settings, 'Agent memory').press('Enter');
      const view = page.locator('.page-view');
      await view.getByText(note, { exact: true }).waitFor();
      await capture('chat-memory', view.locator('.k-page'));
      await button(view, 'Back').press('Enter');
      await page.waitForFunction(() => document.activeElement?.matches('.chat__pill'));
      await menu(page, 'Conversation settings');
      await button(settings, 'History summary').press('Enter');
      await capture('chat-summary', await openSummary(page, dialog(page, 'Conversation context')));

      await go(page, client, room);
      await menu(page, 'Section memory');
      await view.getByText(shared, { exact: true }).waitFor();
      await capture('section-notes', view.locator('.k-page'));
      await button(view, 'Back').press('Enter');
      await menu(page, 'History summary');
      await capture('section-summary', await openSummary(page, dialog(page, 'Conversation context')));
      assert.deepEqual(await page.evaluate(() => globalThis.memoryCsp), []);
    } finally { await page.close(); }
  }

  for (const client of clients) {
    const page = await makePage(client, 'dark', 'desktop');
    try {
      await go(page, client, main);
      let panel = await openMemory(page, await profile(page, client));
      const key = `minarbot:memory:${memory}`;
      const command = { id: stableId(), entry: stableId(), revision: 0, text: 'Recovery fixture note.', essential: true, remove: false };
      await api(memory, command);
      await page.evaluate(([key, command]) => sessionStorage.setItem(key, JSON.stringify(command)), [key, command]);
      await page.reload();
      panel = await openMemory(page, await profile(page, client));
      await button(panel, 'Retry save').press('Enter');
      await panel.getByText('Note saved.', { exact: true }).waitFor();
      assert.equal(await page.evaluate(key => sessionStorage.getItem(key), key), null);
      const search = role(panel, 'textbox', 'Search memory');
      await search.fill('Recovery fixture');
      await search.press('Enter');
      await panel.getByText(note, { exact: true }).waitFor({ state: 'hidden' });
      await button(panel, 'Edit note').press('Enter');
      await button(panel, 'Note actions').click();
      await role(page, 'menuitem', 'Forget note').press('Enter');
      await button(role(page, 'alertdialog', 'Forget this note?'), 'Forget note').press('Enter');
      await panel.getByText('Note forgotten.', { exact: true }).waitFor();
      await button(panel, 'Undo').press('Enter');
      await panel.getByText('Note restored.', { exact: true }).waitFor();
      await api(memory, { ...command, id: stableId(), revision: 3, text: '', essential: false, remove: true });

      const memoryPanel = panel;
      await role(memoryPanel, 'tab', 'Learned').press('Enter');
      const observation = memoryPanel.getByRole('listitem').filter({ hasText: observationText }).first();
      for (const [action, visible] of [['Pin to context', true], ['Forget', false]]) {
        await button(observation, 'Memory actions').click();
        await role(page, 'menuitem', action).press('Enter');
        await observation.getByText('Pinned', { exact: true }).waitFor({ state: visible ? 'visible' : 'hidden' });
      }
      await button(memoryPanel, 'Undo').press('Enter');
      await observation.getByText('Pinned', { exact: true }).waitFor();
      await button(observation, 'Memory actions').click();
      await role(page, 'menuitem', 'Unpin').press('Enter');
      await observation.getByText('Pinned', { exact: true }).waitFor({ state: 'hidden' });
      await button(observation, 'Memory actions').click();
      await role(page, 'menuitem', 'Edit').press('Enter');
      const edit = dialog(page, 'Edit memory');
      await role(edit, 'textbox', 'Memory contents').fill(observationText);
      await button(edit, 'Save').press('Enter');
      await edit.waitFor({ state: 'hidden' });
      await role(memoryPanel, 'tab', 'Standing questions').press('Enter');
      await memoryPanel.getByText('Awaiting refresh', { exact: true }).waitFor();
      await button(memoryPanel, 'Add question').press('Enter');
      const question = dialog(page, 'Add standing question');
      await role(question, 'textbox', 'Question').fill('Who waters the orchids?');
      await button(question, 'Save').press('Enter');
      const added = memoryPanel.getByRole('listitem').filter({ hasText: 'Who waters the orchids?' });
      await button(added, 'Question actions').click();
      await role(page, 'menuitem', 'Forget').press('Enter');
      await added.waitFor({ state: 'hidden' });
      await button(memoryPanel, 'Undo').press('Enter');
      await added.waitFor();
      await button(added, 'Question actions').click();
      await role(page, 'menuitem', 'Forget').press('Enter');
      await added.waitFor({ state: 'hidden' });
      assert.deepEqual(await page.evaluate(() => globalThis.memoryCsp), []);
    } finally { await page.close(); }
  }

  function stats(values) {
    values.sort((a, b) => a - b);
    const percentile = p => Number(values[Math.ceil(values.length * p) - 1].toFixed(1));
    return { samples: values.length, median: percentile(0.5), p95: percentile(0.95) };
  }
  if (samples) for (const client of clients) {
    const cold = [], warm = [];
    for (let sample = 0; sample < samples; sample++) {
      const page = await makePage(client, 'dark', 'desktop');
      try {
        const start = performance.now();
        await go(page, client, main);
        const parent = await profile(page, client, false);
        const memory = await openMemory(page, parent);
        cold.push(performance.now() - start);
        await button(memory, 'Back').press('Enter');
        await memory.waitFor({ state: 'hidden' });
        const warmStart = performance.now();
        await openMemory(page, parent);
        warm.push(performance.now() - warmStart);
      } finally { await page.close(); }
    }
    timings[client] = { cold: stats(cold), warm: stats(warm) };
  }
  await writeFile(join(output, 'timings.json'), JSON.stringify(timings, null, 2) + '\n');
  assert.deepEqual(errors, []);

  if (samples) {
    assertP95('memory', 'cold', timings.leptos.cold.p95);
    assertP95('memory', 'warm', timings.leptos.warm.p95);
  }
  console.log(`Memory, summary, sources, pin, forget and undo passed (${clients.join(', ')}). Screenshots: ${output}`);
} finally {
  await browser?.close();
  await app?.close();
  provider.closeAllConnections();
  await new Promise(resolve => provider.close(resolve));
  await rm(scratch, { recursive: true });
}
