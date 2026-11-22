import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from 'node:sqlite';
import { chromium } from 'playwright-core';
import { startApp } from '../../test-bridge.ts';

const scratch = await mkdtemp(join(tmpdir(), 'minarbot-memory-ui-'));
const output = resolve(process.argv[2] ?? await mkdtemp(join(tmpdir(), 'minarbot-memory-shots-')));
const database = join(scratch, 'fixture.sqlite');
const agent = randomUUID(), turn = randomUUID(), thread = randomUUID();
const bank = `agents:${agent}`, memory = `/api/agents/${agent}/memory`;
let app, browser;
try {
  app = await startApp({ database, dist: fileURLToPath(new URL('../../dist', import.meta.url)), defaultModel: 'fixture' });
  async function api(path, data) {
    const response = await fetch(app.origin + path, data ? { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(data) } : {});
    assert(response.ok, `${path}: ${response.status} ${await response.clone().text()}`);
    return response.json();
  }
  await api('/api/agents', { id: agent, name: 'Garden planner', model: 'fixture', instructions: 'Plan garden work.', runtime: 'api' });
  await api(`/api/agents/${agent}/threads`, { id: thread, name: 'Seedling planning' });
  await api(memory, { id: randomUUID(), entry: randomUUID(), text: 'Start the greenhouse inventory on Mondays.', revision: 0, essential: true, remove: false });
  const db = new DatabaseSync(database);
  try {
    const source = { kind: 'turn', id: turn, agent, thread, role: 'prompt', created: '2026-11-19T10:30:00Z' };
    const raw = db.prepare("INSERT INTO memory_units(bank,origin,source_id,kind,text,source) VALUES(?,'prompt',?,'experience',?,?) RETURNING id")
      .get(bank, turn, 'The north shelf has unlabelled orchid trays.', JSON.stringify(source)).id;
    const retained = db.prepare("INSERT INTO memory_units(bank,origin,source_id,kind,text,source) VALUES(?,'retained',?,?,?,?) RETURNING id");
    const fact = retained.get(bank, randomUUID(), 'experience', 'Orchid tray labels are in the north cabinet.', JSON.stringify({ kind: 'fact', raw, message: source })).id;
    const observation = retained.get(bank, randomUUID(), 'observation', 'The north cabinet holds orchid tray labels and inventory sheets.', JSON.stringify({ kind: 'observation' })).id;
    db.prepare('INSERT INTO memory_facts(unit,raw,offset,consolidated) VALUES(?,?,0,1)').run(fact, raw);
    db.prepare('INSERT INTO memory_observations VALUES(?,?,?,?)').run(observation, bank, agent, 'cabinet');
    db.prepare('INSERT INTO memory_observation_evidence VALUES(?,?)').run(observation, fact);
    db.prepare('INSERT INTO memory_mental_models(bank,id,question,answer,evidence,generation) SELECT ?,?,?,?,?,generation FROM memory_bank_generations WHERE bank=?')
      .run(bank, randomUUID(), 'Where are the orchid supplies?', 'Labels and inventory sheets are in the north cabinet.', JSON.stringify([{ id: observation, kind: 'observation', text: 'North cabinet', source: [source] }]), bank);
  } finally { db.close(); }
  await mkdir(output, { recursive: true });
  browser = await chromium.launch({ executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE ?? '/usr/bin/google-chrome' });
  for (const theme of ['dark', 'light']) {
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.addInitScript(theme => localStorage.setItem('minarbot-presentation-v1', JSON.stringify({ version: 1, theme, textSize: 'standard' })), theme);
    await page.goto(app.origin + '/agents/' + agent);
    const profile = page.getByRole('dialog', { name: 'Agent profile', exact: true });
    await page.getByRole('button', { name: /^Garden planner/ }).first().click({ button: 'right' });
    await page.getByRole('menuitem', { name: 'Edit', exact: true }).click();
    await profile.getByRole('tab', { name: 'Advanced', exact: true }).click();
    const resource = profile.getByRole('button', { name: 'Agent memory', exact: true });
    await resource.press('Enter');
    const dialog = page.getByRole('dialog', { name: 'Minarbot Memory', exact: true });
    await dialog.getByRole('heading', { name: 'Minarbot Memory', level: 1, exact: true }).waitFor();
    await dialog.getByText('Start the greenhouse inventory on Mondays.', { exact: true }).waitFor();
    const learned = dialog.getByRole('tab', { name: 'Learned', exact: true });
    await learned.press('Enter');
    const observation = dialog.getByRole('listitem').filter({ hasText: 'The north cabinet holds orchid tray labels and inventory sheets.' }).first();
    await observation.waitFor();
    await observation.locator('summary').press('Enter');
    const source = observation.getByRole('link', { name: 'User message', exact: true });
    assert.equal(await source.getAttribute('href'), `/agents/${agent}/conversations/${thread}`);
    assert.equal(await page.locator('html').getAttribute('data-theme'), theme);
    await page.screenshot({ path: join(output, `memory-${theme}.png`), animations: 'disabled' });
    await page.setViewportSize({ width: 390, height: 844 });
    assert(await dialog.evaluate(el => el.scrollWidth <= el.clientWidth + 1));
    await page.screenshot({ path: join(output, `memory-${theme}-narrow.png`), animations: 'disabled' });
    await dialog.getByRole('tab', { name: 'Standing questions', exact: true }).press('Enter');
    await dialog.getByText('Labels and inventory sheets are in the north cabinet.', { exact: true }).waitFor();
    await dialog.getByRole('tab', { name: 'Learned', exact: true }).press('Enter');
    await observation.getByRole('button', { name: 'Memory actions' }).click();
    await page.getByRole('menuitem', { name: 'Pin to context', exact: true }).press('Enter');
    await observation.getByText('Pinned', { exact: true }).waitFor();
    await observation.getByRole('button', { name: 'Memory actions' }).click();
    await page.getByRole('menuitem', { name: 'Forget', exact: true }).press('Enter');
    await observation.waitFor({ state: 'hidden' });
    await dialog.getByRole('button', { name: 'Undo', exact: true }).press('Enter');
    await observation.getByText('Pinned', { exact: true }).waitFor();
    await observation.getByRole('button', { name: 'Memory actions' }).click();
    await page.getByRole('menuitem', { name: 'Unpin', exact: true }).press('Enter');
    await page.getByRole('menu', { includeHidden: true }).waitFor({ state: 'detached' });
    await observation.getByText('Pinned', { exact: true }).waitFor({ state: 'hidden' });
    await page.keyboard.press('Escape');
    await dialog.waitFor({ state: 'hidden' });
    await page.waitForFunction(el => document.activeElement === el, await resource.elementHandle());
    assert.deepEqual(errors, []);
    await page.close();
  }
  console.log(`Memory dark/light and narrow layouts, sources, pin, forget and undo passed. Screenshots: ${output}`);
} finally {
  await browser?.close();
  await app?.close();
  await rm(scratch, { recursive: true });
}
