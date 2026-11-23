import assert from 'node:assert/strict';
import { fixDate, fixtureClock, parityShot } from './parity-shot.mjs';
import { randomUUID } from 'node:crypto';
import { mkdirSync, mkdtempSync, rmSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { chromium } from 'playwright-core';
import { startApp } from '../../test-bridge.ts';
import { testDatabase } from '../database.ts';
import { addDays, dateLabel, wallTime } from '../../src/automation-calendar.ts';

const output = process.argv[2] ? resolve(process.argv[2]) : mkdtempSync(join(tmpdir(), 'minarbot-calendar-screenshots-'));
mkdirSync(output, { recursive: true });
const directory = mkdtempSync(join(tmpdir(), 'minarbot-calendar-smoke-'));
const database = join(directory, 'state.sqlite');
const client = process.env.MINARBOT_SMOKE_UI ?? 'react';
assert(['react', 'leptos'].includes(client));
const zone = 'Asia/Jakarta';
process.env.TZ = zone;
const today = wallTime(fixtureClock, zone).date;
const tomorrow = addDays(today, 1);
const future = addDays(today, 2);
const fixtureId = number => `00000000-0000-4000-8000-${String(number).padStart(12, '0')}`;
let app, browser;
let release;
const blocked = new Promise(resolve => { release = resolve; });
const provider = createServer(async (request, response) => {
  response.setHeader('Content-Type', 'application/json');
  if (request.method === 'GET') {
    response.end(JSON.stringify({ data: [{ id: 'fixture' }] })); return;
  }
  let raw = '';
  for await (const chunk of request) raw += chunk;
  if (raw.includes('Wait for the calendar check.')) await blocked;
  response.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: 'Fixture report complete.' } }],
    usage: { prompt_tokens: 20, completion_tokens: 10, total_tokens: 30 } }));
});
await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
try {
  app = await startApp({ database, port: 0, dist: resolve('dist'), appearanceClient: client, fixedClock: fixtureClock + 1000, baseURL: 'http://127.0.0.1:' + provider.address().port + '/v1', defaultModel: 'fixture', stream: false });
  const { origin } = app;
  async function api(path, body) {
    const response = await fetch(origin + path, body === undefined ? {} : { method: 'POST', headers: { Origin: origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
    const value = await response.json();
    assert(response.ok, path + ': ' + JSON.stringify(value));
    return value;
  }
  const agent = await api('/api/agents', { id: fixtureId(12), runtime: 'api', name: 'Calendar agent', model: 'fixture', instructions: 'Synthetic automation fixture.' });
  const other = await api('/api/agents', { id: fixtureId(13), runtime: 'api', name: 'Other calendar agent', model: 'fixture', instructions: 'Synthetic automation fixture.' });
  const project = await api('/api/projects', { id: fixtureId(14), name: 'Field Operations', members: [{ agent: agent.id, write: false }] });
  const scope = { project: project.id, zone, trigger: 'schedule', enabled: true };
  const once = (date, time) => ({ kind: 'once', date, time });
  const seeded = await api('/api/agents/' + agent.id + '/routines', { ...scope, id: fixtureId(15), name: 'Morning digest', prompt: 'Wait for the calendar check.', schedule: once(tomorrow, '09:00') });
  const finished = await api('/api/agents/' + agent.id + '/routines', { ...scope, id: fixtureId(16), name: 'Inventory recap', prompt: 'Finish the recap.', schedule: once(future, '10:30'), enabled: false, reportTo: project.channel });
  const recap = await api('/api/routines/' + finished.id + '/test', { id: randomUUID(), revision: finished.revision });
  // Give the prior run its earlier start time even though the server clock is frozen.
  const db = testDatabase(database);
  try {
    assert.equal(db.prepare('UPDATE turns SET created = ? WHERE id = ?')
      .run(new Date(fixtureClock).toISOString(), recap.turn).changes, 1);
  } finally {
    db.close();
  }
  await api('/api/agents/' + agent.id + '/workflows', { id: fixtureId(17), revision: 0, name: 'Daily summary', graph: { entry: 'out', maxSteps: 8, nodes: [{ id: 'out', kind: 'output' }] } });

  browser = await chromium.launch({ headless: true, ...(process.env.CHROMIUM_EXECUTABLE ? { executablePath: process.env.CHROMIUM_EXECUTABLE } : {}) });
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, timezoneId: 'America/Los_Angeles' });
  const page = await context.newPage();
  await fixDate(page);
  const errors = [], consoleErrors = [];
  function inspect(target) {
    target.on('pageerror', error => errors.push(error.message));
    target.on('console', message => {
      if (message.type() === 'error' && !message.text().includes('409 (Conflict)')) consoleErrors.push(message.text());
    });
  }
  await context.addInitScript(() => {
    window.calendarCspViolations = [];
    document.addEventListener('securitypolicyviolation', event => window.calendarCspViolations.push(event.effectiveDirective));
  });
  inspect(page);
  const show = locator => locator.waitFor({ state: 'visible', timeout: 10_000 });
  const button = (name, root = page) => root.getByRole('button', { name, exact: typeof name === 'string' });
  const dialog = () => page.getByRole('dialog');
  const event = (name, status) => page.getByRole('button', { name: new RegExp('^' + name + ', (Routine|Workflow), ' + status) });
  const cell = day => page.getByRole('region', { name: dateLabel(day), exact: true }).and(page.locator('.calendar-cell'));
  const shot = async name => {
    assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), 'Calendar page overflows viewport');
    await parityShot(page, join(output, name + '.png'));
  };
  async function openProject() {
    await page.goto(origin + '/projects/' + project.id);
    const overview = page.getByRole('tab', { name: 'Overview', exact: true });
    const calendarTab = page.getByRole('tab', { name: 'Calendar', exact: true });
    await overview.press('ArrowRight');
    await show(calendarTab.and(page.locator('[aria-selected=true]')));
    assert.equal(await page.locator('#leptos-root .automation-calendar').count(), client === 'leptos' ? 1 : 0);
    const ics = page.getByRole('link', { name: 'Download routines' });
    const calendar = await page.request.get(new URL(await ics.getAttribute('href'), origin).href);
    assert.equal(calendar.status(), 200);
    assert.match(await calendar.text(), /BEGIN:VCALENDAR/);
  }
  await openProject();
  await show(event('Morning digest', 'Scheduled'));
  await show(event('Inventory recap', 'Done'));
  for (const [viewport, size] of Object.entries({ desktop: { width: 1440, height: 1000 }, narrow: { width: 390, height: 844 } })) {
    await page.setViewportSize(size);
    for (const theme of ['light', 'dark']) {
      await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
      await shot(`project-${viewport}-${theme}`);
    }
  }
  await page.setViewportSize({ width: 1440, height: 1000 });
  if (client === 'leptos') {
    await page.waitForURL('**?projectTab=calendar');
    const settings = await page.getByRole('link', { name: 'Settings', exact: true }).getAttribute('href');
    assert(new URL(settings, origin).searchParams.get('from').endsWith('?projectTab=calendar'));
    await page.goBack();
    await show(page.getByRole('tab', { name: 'Overview', exact: true, selected: true }));
    await page.goForward();
    await show(page.getByRole('tab', { name: 'Calendar', exact: true, selected: true }));
  }
  await button('Add automation on ' + dateLabel(future), cell(future)).click();
  await dialog().getByLabel('Name', { exact: true }).fill('Calendar maintenance');
  await dialog().getByLabel('Prompt', { exact: true }).fill('Check the calendar.');
  assert.equal(await dialog().getByLabel('Date', { exact: true }).inputValue(), future);
  await button('Save schedule', dialog()).click();
  await show(event('Calendar maintenance', 'Scheduled'));
  const { routines } = await api('/api/routines?project=' + project.id);
  const routine = routines.find(row => row.name === 'Calendar maintenance');
  assert(routine);

  await event('Calendar maintenance', 'Scheduled').focus();
  await page.keyboard.press('Enter');
  await button('Edit schedule', dialog()).click();
  await dialog().getByLabel('Name', { exact: true }).fill('Stale maintenance draft');
  await api('/api/routines/' + routine.id + '/edit', { id: randomUUID(), revision: routine.revision, name: routine.name, prompt: 'External update.', schedule: routine.schedule, zone, enabled: true });
  await button('Save schedule', dialog()).click();
  await show(dialog().getByText('Routine changed. Reload before saving.'));
  assert.equal(await dialog().getByLabel('Name', { exact: true }).inputValue(), 'Stale maintenance draft');
  await button('Reload saved schedule', dialog()).click();
  await dialog().getByLabel('Name', { exact: true }).fill('Edited maintenance');
  await button('Save schedule', dialog()).click();
  await show(event('Edited maintenance', 'Scheduled'));
  await event('Edited maintenance', 'Scheduled').dragTo(cell(tomorrow));
  await show(dialog().getByRole('heading', { name: 'Move occurrence' }));
  assert.equal(await dialog().getByLabel('Date', { exact: true }).inputValue(), tomorrow);
  await button('Move occurrence', dialog()).click();
  await show(cell(tomorrow).getByRole('button', { name: /^Edited maintenance, Routine, Scheduled/ }));
  await event('Edited maintenance', 'Scheduled').click();
  await button('Pause schedule', dialog()).click();
  await show(event('Edited maintenance', 'Paused'));
  await event('Edited maintenance', 'Paused').click();
  await button('Enable schedule', dialog()).click();
  await show(event('Edited maintenance', 'Scheduled'));

  await button('Add automation on ' + dateLabel(future), cell(future)).click();
  await dialog().getByRole('combobox', { name: 'Automation' }).click();
  await page.getByRole('option', { name: 'Schedule workflow' }).click();
  await dialog().getByRole('combobox', { name: 'Workflow', exact: true }).click();
  await page.getByRole('option', { name: 'Daily summary' }).click();
  await dialog().getByLabel('Repeat every (minutes)').fill('720');
  await button('Save schedule', dialog()).click();
  await show(event('Daily summary', 'Scheduled').first());
  const workflowEvent = cell(future).getByRole('button', { name: /^Daily summary, Workflow, Scheduled/ });
  const later = addDays(future, 1);
  await workflowEvent.dragTo(cell(later));
  await show(dialog().getByText('All future occurrences in this series will move.'));
  assert.equal(await dialog().getByLabel('Date', { exact: true }).inputValue(), later);
  await button('Move series', dialog()).click();
  await show(cell(later).getByRole('button', { name: /^Daily summary, Workflow, Scheduled/ }));
  await workflowEvent.waitFor({ state: 'hidden' });

  const stream = page.waitForResponse(response => response.url() === origin + '/api/automation-calendar/stream');
  await button('Automation calendar').click();
  await stream;
  await show(event('Morning digest', 'Scheduled'));
  await page.getByRole('combobox', { name: 'Filter by agent' }).click();
  await page.getByRole('option', { name: other.name, exact: true }).click();
  await button('Add automation').click();
  assert.equal((await dialog().getByRole('combobox', { name: 'Agent', exact: true }).textContent()).trim(), other.name);
  await button('Cancel', dialog()).click();
  await page.getByRole('combobox', { name: 'Filter by agent' }).click();
  await page.getByRole('option', { name: 'All agents', exact: true }).click();
  const projectPage = await context.newPage();
  await fixDate(projectPage);
  inspect(projectPage);
  await projectPage.goto(origin + '/projects/' + project.id);
  await projectPage.getByRole('tab', { name: 'Calendar', exact: true }).click();
  await show(projectPage.getByRole('button', { name: /^Morning digest, Routine, Scheduled/ }));

  // The ten-second check is shorter than the connected stream's fallback interval.
  let navigations = 0;
  page.on('framenavigated', frame => { if (frame === page.mainFrame()) navigations++; });
  await page.bringToFront();
  await event('Morning digest', 'Scheduled').click();
  await button('Run now', dialog()).click();
  await show(event('Morning digest', 'Running'));
  await show(event('Inventory recap', 'Done'));
  await show(event('Daily summary', 'Scheduled').first());
  await projectPage.bringToFront();
  await show(projectPage.getByRole('button', { name: /^Morning digest, Routine, Running/ }));
  await show(projectPage.getByRole('button', { name: /^Inventory recap, Routine, Done/ }));
  await show(projectPage.getByRole('button', { name: /^Daily summary, Workflow, Scheduled/ }).first());
  await parityShot(projectPage, join(output, 'project-month-running-dark.png'));

  await page.bringToFront();
  await shot('overall-month-running-dark');
  await event('Morning digest', 'Running').click();
  await show(dialog().getByText('Running', { exact: true }));
  release();
  await show(dialog().getByText('Done', { exact: true }));
  assert.equal(navigations, 0, 'Run finished after page navigation');
  await button('Open Run inspector', dialog()).click();
  await show(dialog().getByRole('heading', { name: 'Run inspector' }));
  await show(dialog().getByRole('region', { name: 'Provider calls' }).getByText(/^Completed · /));
  const providers = dialog().getByRole('region', { name: 'Provider calls' });
  await providers.locator('summary').filter({ hasText: 'fixture' }).first().click();
  await show(providers.getByText('Total tokens', { exact: true }).first());
  assert.equal(await providers.getByText('30', { exact: true }).first().textContent(), '30');
  await shot('routine-run-usage-dark');
  await button('Close', dialog()).click();
  await page.evaluate(() => { document.documentElement.dataset.theme = 'light'; });
  await shot('overall-month-light');

  await button('View ' + dateLabel(tomorrow), cell(tomorrow)).click();
  await show(page.getByRole('list', { name: 'Hourly calendar' }));
  await button('Add automation on ' + dateLabel(tomorrow) + ' at 13:00').press('Enter');
  assert.equal(await dialog().getByLabel('Time', { exact: true }).inputValue(), '13:00');
  await button('Cancel', dialog()).click();
  await button('Week').click();
  await show(event('Edited maintenance', 'Scheduled'));
  await shot('overall-week-light');
  await page.evaluate(() => { document.documentElement.dataset.theme = 'dark'; });
  await shot('overall-week-dark');

  await page.setViewportSize({ width: 390, height: 844 });
  await show(event('Edited maintenance', 'Scheduled'));
  await show(event('Edited maintenance', 'Scheduled').getByText(/Field Operations/));
  assert.equal(await page.locator('.calendar-grid').isVisible(), false);
  await shot('overall-agenda-dark');
  await page.evaluate(() => { document.documentElement.dataset.theme = 'light'; });
  await shot('overall-agenda-light');
  await projectPage.getByRole('button', { name: /^Inventory recap, Routine, Done/ }).click();
  await projectPage.getByRole(client === 'leptos' ? 'link' : 'button', { name: 'Open report', exact: true }).click();
  await projectPage.waitForURL(origin + '/rooms/' + project.channel);
  await show(projectPage.getByRole('region', { name: 'Conversation history' }).getByText('Fixture report complete.', { exact: true }));
  assert.deepEqual(errors, []);
  assert.deepEqual(consoleErrors, []);
  assert.deepEqual(await page.evaluate(() => window.calendarCspViolations), []);
  assert.deepEqual(await projectPage.evaluate(() => window.calendarCspViolations), []);
  console.log('PASS: project + overall calendar, create/edit/conflict/drag/pause/enable, workflow scheduling, live Running to Done, month/week/day and narrow agenda.');
  console.log('Screenshots: ' + output);
} finally {
  release();
  await browser?.close();
  await app?.close();
  provider.closeAllConnections();
  await new Promise(resolve => provider.close(resolve));
  rmSync(directory, { recursive: true, force: true });
}
