import assert from 'node:assert/strict';
import { assertP95 } from './timing-budgets.mjs';
import { fixDate, fixtureClock, parityShot, withFixtureText } from './parity-shot.mjs';
import { randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { chromium } from 'playwright-core';
import { startApp } from '../../test-bridge.ts';
import { issue1Smoke } from './issue1-smoke.mjs';
import { graphLayout } from './workflow-graph-smoke.mjs';

const scratch = await mkdtemp(join(tmpdir(), 'minarbot-automation-parity-'));
const output = resolve(process.argv[2] ?? 'test-support/ui-smoke/artifacts/automation-parity');
await mkdir(output, { recursive: true });
const provider = createServer(async (request, response) => {
  for await (const chunk of request) void chunk;
  response.setHeader('Content-Type', 'application/json');
  response.end(JSON.stringify(request.method === 'GET'
    ? { data: [{ id: 'fixture' }] }
    : { choices: [{ finish_reason: 'stop', message: { content: '**Automation fixture completed.**' } }] }));
});
await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
let browser;
const receipts = {};
const sizes = { desktop: { width: 1440, height: 900 }, narrow: { width: 390, height: 844 } };
const clients = ['leptos'];
const warmSamples = Number(process.env.MINARBOT_AUTOMATION_WARM_BENCH ?? 80);
assert(Number.isSafeInteger(warmSamples) && warmSamples >= 0);
const coldSamples = Number(process.env.MINARBOT_AUTOMATION_COLD_BENCH ?? 0);
assert(Number.isSafeInteger(coldSamples) && coldSamples >= 0);
const routes = [
  { path: '/routines', title: 'Routines', ready: 'main .routine-table tbody tr' },
  { path: '/workflows', title: 'Workflows', ready: 'main .k-row__title' },
  { path: '/calendar', title: 'Automation calendar', ready: 'main .calendar-grid .calendar-event' },
];
const fixtureIds = { agent: randomUUID(), other: randomUUID(), workflow: randomUUID(), routine: randomUUID(), webhookRoutine: 'a3e4eaab-0929-4305-9b99-67c0155f199d' };

async function exercise(client) {
  const app = await startApp({ database: join(scratch, client + '.sqlite'), dist: resolve('dist'), port: 0, fixedClock: fixtureClock,
    baseURL: 'http://127.0.0.1:' + provider.address().port + '/v1', stream: false });
  const context = await browser.newContext({ viewport: sizes.desktop, timezoneId: 'UTC' });
  const errors = [], failedExpected = [];
  await context.addInitScript(() => {
    window.automationCsp = [];
    addEventListener('securitypolicyviolation', event => window.automationCsp.push(event.effectiveDirective));
  });
  const page = await context.newPage();
  await fixDate(page);
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', event => {
    if (event.type() !== 'error') return;
    if (event.text().includes('503 (Service Unavailable)')) failedExpected.push(event.text());
    else errors.push(event.text());
  });
  const { origin } = app;
  const button = (name, scope = page) => scope.getByRole('button', { name, exact: true });
  const heading = name => page.getByRole('heading', { name, exact: true });
  const open = async (path, title) => {
    const response = await page.goto(origin + path);
    assert.equal(response.status(), 200);
    assert.equal((await response.headerValue('content-security-policy')).includes('wasm-unsafe-eval'), true);
    await heading(title).waitFor();
    assert.equal(await page.locator('#leptos-root').count(), 1);
    {
      assert.equal(await page.locator('script[src*="/workflows-start."]').count(), 1);
      assert.equal(await page.locator('script[src*="/appearance-start."], script[src*="/conversation-start."]').count(), 0);
    }
  };
  const mutations = [];
  const inspectGraph = async label => graphLayout(page, { graphLayouts: [], cspViolations: await page.evaluate(() => window.automationCsp) }, label);
  async function api(path, body) {
    const response = await fetch(origin + path, body === undefined ? {} : { method: 'POST', headers: { Origin: origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
    assert(response.ok, `${path} returned ${response.status}`);
    return response.json();
  }
  const capture = async (name, anchor) => {
    for (const [viewport, size] of Object.entries(sizes)) {
      await page.setViewportSize(size);
      if (anchor) await anchor.evaluate(node => node.scrollIntoView({ block: 'center', inline: 'nearest' }));
      for (const theme of ['dark', 'light']) {
        await page.evaluate(value => { document.documentElement.dataset.theme = value; }, theme);
        assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), `${client} ${name} overflows ${viewport}`);
        await parityShot(page, join(output, `${client}-${name}-${viewport}-${theme}.png`));
      }
    }
    await page.setViewportSize(sizes.desktop);
  };
  const choose = async (label, name, scope = page) => {
    const select = scope.getByRole('combobox', { name: label, exact: true });
    if (await select.evaluate(element => element instanceof HTMLSelectElement)) await select.selectOption({ label: name });
    else {
      await select.press('Enter');
      await page.getByRole('option', { name, exact: true }).click();
    }
  };

  try {
    const agent = await api('/api/agents', { id: fixtureIds.agent, providerInstance: 'codex-test-gateway', name: 'Automation owner', model: 'fixture', instructions: 'Synthetic fixture.' });
    await api('/api/agents', { id: fixtureIds.other, providerInstance: 'codex-test-gateway', name: 'Automation helper', model: 'fixture', instructions: 'Synthetic fixture.' });
    const routine = await api(`/api/agents/${agent.id}/routines`, {
      id: fixtureIds.routine, trigger: 'schedule', enabled: true, name: 'Morning digest', prompt: 'Prepare the digest.',
      zone: 'UTC', schedule: { kind: 'cron', expression: '0 9 * * 1-5' }, reportTo: 'direct', project: null,
    });
    const workflow = await api(`/api/agents/${agent.id}/workflows`, {
      id: fixtureIds.workflow, revision: 0, name: 'Daily summary',
      graph: { entry: 'work', maxSteps: 8, nodes: [
        { id: 'work', kind: 'agent', agent: agent.id, prompt: 'Summarize the fixture.', next: 'out' },
        { id: 'out', kind: 'output' },
      ], positions: { work: { x: 64, y: 64 }, out: { x: 344, y: 64 } } },
    });

    await open(`/routines?agent=${agent.id}`, 'Routines');
    await button('Morning digest').waitFor();
    assert.equal((await page.getByRole('combobox', { name: 'Agent', exact: true }).textContent()).trim(), agent.name);
    await open('/routines', 'Routines');
    await button('Morning digest').waitFor();
    await capture('routines');
    await page.getByRole('searchbox', { name: 'Search routines' }).fill('unmatched');
    await page.getByText('No matching routines').waitFor();
    await button('Clear filters').click();
    await button('Morning digest').click();
    await heading('Morning digest').waitFor();
    await capture('routine-overview');
    await button('Edit routine').press('Enter');
    let dialog = page.getByRole('dialog', { name: 'Edit routine' });
    await dialog.getByLabel('Name', { exact: true }).fill('Morning digest edited');
    await dialog.locator('.routine-preview li').nth(2).waitFor();
    await capture('routine-editor');
    const edited = page.waitForRequest(request => request.url().endsWith(`/api/routines/${routine.id}/edit`) && request.method() === 'POST');
    await button('Save routine', dialog).click();
    mutations.push((await edited).postDataJSON());
    await heading('Morning digest edited').waitFor();
    await page.getByRole('tab', { name: 'Flow' }).press('Enter');
    await heading('Agent flow').waitFor();
    await choose('To agent', 'Automation helper');
    await button('Connect').click();
    await button('Save flow').click();
    await page.waitForFunction(() => !Object.keys(sessionStorage).some(key => key.startsWith('minarbot:flow:')));
    await page.getByText('Automation owner to Automation helper', { exact: true }).waitFor();
    await capture('routine-flow');
    await page.getByRole('tab', { name: 'Overview' }).press('Enter');
    await button('Run now').click();
    await page.getByText('Done', { exact: true }).first().waitFor();
    await page.waitForFunction(async agent => {
      const response = await fetch('/api/sidebar/summary');
      if (!response.ok) return false;
      const { summaries } = await response.json();
      return summaries.some(row => row.agent === agent && row.preview.startsWith('Morning digest edited · Done'));
    }, agent.id, { polling: 100 });
    await button('All routines').click();
    const createdRoutine = page.waitForResponse(response => response.request().method() === 'POST'
      && new URL(response.url()).pathname === `/api/agents/${agent.id}/routines`);
    // Keep the time zone lookup pending until the manual create is sent.
    await page.route('**/api/routine-schedule', async route => {
      await createdRoutine;
      await route.continue();
    }, { times: 1 });
    await button('New routine').first().click();
    dialog = page.getByRole('dialog', { name: 'New routine' });
    await dialog.getByLabel('Name', { exact: true }).fill('Manual review');
    await dialog.getByLabel('Instructions', { exact: true }).fill('Review the fixture.');
    await choose('Trigger', 'Manual', dialog);
    await button('Save routine', dialog).click();
    const created = await createdRoutine;
    assert.equal(created.status(), 200);
    await button('Manual review').waitFor();
    await button('Manual review').click();
    await button('Routine actions').click();
    await page.getByRole('menuitem', { name: 'Delete routine', exact: true }).click();
    await button('Delete routine', page.getByRole('alertdialog')).click();
    await heading('Routines').waitFor();
    await button('Morning digest edited').waitFor();

    await open('/workflows', 'Workflows');
    await page.getByText('Daily summary', { exact: true }).waitFor();
    await capture('workflows');
    await page.getByText('Daily summary', { exact: true }).click();
    await heading('Daily summary').waitFor();
    await page.getByLabel('Workflow start time', { exact: true }).waitFor();
    await capture('workflow-detail');
    await button('Edit workflow').press('Enter');
    await heading('Edit workflow').waitFor();
    const node = page.getByRole('region', { name: 'Workflow graph, scroll to see all steps' }).getByRole('button', { name: /work Agent/ });
    await node.focus();
    const before = await node.evaluate(element => element.offsetLeft);
    await node.press('Alt+ArrowRight');
    await page.waitForFunction(x => document.querySelector('.workflow-graph-node')?.offsetLeft === x, before + 20);
    await node.press('Enter');
    assert.match(await page.evaluate(() => document.activeElement?.querySelector('legend')?.textContent ?? ''), /Step work/);
    await button('Back to graph work').click();
    assert(await node.evaluate(element => document.activeElement === element));
    await inspectGraph('editor');
    await page.getByText(/^Saved, version \d+\.$/).waitFor();
    await capture('workflow-editor');
    {
      const download = page.waitForEvent('download');
      await button('Export graph').click();
      const exported = JSON.parse(await readFile(await (await download).path(), 'utf8'));
      assert.equal(typeof exported.positions.work.x, 'number');
      await button('Import graph').click();
      const importer = page.getByRole('dialog', { name: 'Import workflow graph' });
      await importer.getByLabel('Graph JSON').fill(JSON.stringify({ ...exported, positions: [] }));
      await button('Import graph', importer).click();
      await importer.getByRole('alert').getByText(/uniquely named steps/).waitFor();
      await importer.getByLabel('Graph JSON').fill(JSON.stringify(exported));
      await button('Import graph', importer).click();
      await importer.waitFor({ state: 'hidden' });
    }
    await button('Save workflow').click();
    await heading('Daily summary').waitFor();
    const webhook = page.waitForRequest(request => request.url().endsWith(`/api/workflows/${workflow.id}/webhook`) && request.method() === 'POST');
    await page.getByRole('checkbox', { name: 'Workflow webhook active' }).check();
    await button('Save workflow webhook').click();
    mutations.push((await webhook).postDataJSON());
    await page.getByLabel('Workflow webhook URL', { exact: true }).waitFor();
    await page.waitForFunction(() => !Object.values(sessionStorage).some(value => value.includes('signingSecret')));
    await page.getByLabel('Workflow input', { exact: true }).fill('Run the fixture.');
    const version = (await api(`/api/workflows/${workflow.id}`)).revision;
    const runBody = [];
    await page.route(`**/api/workflows/${workflow.id}/run`, async route => {
      runBody.push(route.request().postDataJSON());
      if (runBody.length === 1) {
        const accepted = await route.fetch();
        assert(accepted.ok());
        await route.fulfill({ status: 503, json: { error: 'Synthetic response loss.' } });
      } else await route.continue();
    });
    await button(`Run version ${version}`).click();
    await page.getByText('Request is not confirmed yet', { exact: true }).waitFor();
    await page.reload();
    await heading('Workflows').waitFor();
    await page.getByText('Daily summary', { exact: true }).click();
    await heading('Daily summary').waitFor();
    const replay = page.waitForResponse(response => response.url().endsWith(`/api/workflows/${workflow.id}/run`)
      && response.request().method() === 'POST');
    await button('Retry request').click();
    assert((await replay).ok());
    await page.waitForFunction(async id => {
      const response = await fetch('/api/workflow-runs/' + id);
      return response.ok && (await response.json()).status === 'done';
    }, runBody[0].id);
    assert.deepEqual(runBody[0], runBody[1]);
    assert.equal((await api(`/api/workflows/${workflow.id}/runs`)).length, 1);
    await page.waitForFunction(key => !sessionStorage.getItem(key), `minarbot:workflow-command:${agent.id}`);
    // Replay selects the run itself. Open it separately from a fresh history view.
    await open('/workflows', 'Workflows');
    await page.getByText('Daily summary', { exact: true }).click();
    await button('Open run').click();
    await page.locator('.workflow-run-node').filter({ hasText: 'Completed' }).first().waitFor();
    await page.locator('main .markdown strong').filter({ hasText: 'Automation fixture completed.' }).first().waitFor();
    await inspectGraph('completed run');
    await capture('workflow-run', heading('Run history'));
    {
      const step = page.getByRole('link', { name: 'Open step work', exact: true });
      await step.press('Enter');
      await page.waitForURL('**/conversations/*');
      await page.goBack();
      await page.waitForFunction(() => document.activeElement?.textContent === 'Open step work');
    }
    await page.unroute(`**/api/workflows/${workflow.id}/run`);
    await open('/calendar', 'Automation calendar');
    await page.getByRole('button', { name: /^Morning digest edited, Routine, Scheduled/ }).first().waitFor();
    await capture('calendar');

    const automation = page.getByRole('navigation', { name: 'Automation', exact: true });
    {
      await automation.getByLabel('Workflows', { exact: true }).click();
      await heading('Workflows').waitFor();
      await page.getByRole('button', { name: /^Daily summary\b/ }).click();
      await heading('Daily summary').waitFor();
      await automation.getByLabel('Automation calendar', { exact: true }).click();
      await heading('Automation calendar').waitFor();
      await page.goBack();
      await heading('Workflows').waitFor();
      await page.goForward();
      await heading('Automation calendar').waitFor();
    }
    await issue1Smoke({ page, routineId: fixtureIds.webhookRoutine,
      check: async (name, run) => { if (name.startsWith('Routine templates')) await run(); },
      screenshot: name => withFixtureText(page, [[origin, 'http://127.0.0.1:33797']],
        () => parityShot(page, join(output, `${client}-${name}.png`))),
    });
    await open('/workflows', 'Workflows');
    await button('Workflow options').click();
    await page.getByRole('menuitem', { name: 'Results inbox', exact: true }).click();
    await heading('Workflow inbox').waitFor();
    await button('Enable browser notifications').waitFor();
    await page.getByText('Daily summary', { exact: true }).click();
    await heading('Daily summary').waitFor();
    await page.locator('main .markdown strong').filter({ hasText: 'Automation fixture completed.' }).first().waitFor();
    await button('Back to workflows').click();
    await button('New workflow').click();
    dialog = page.getByRole('dialog', { name: 'New workflow' });
    await choose('Starting agent', 'Automation owner', dialog);
    await button('Continue', dialog).click();
    await heading('New workflow').waitFor();
    await page.getByLabel('Workflow name', { exact: true }).fill('New fixture workflow');
    await page.getByLabel('Step instructions', { exact: true }).fill('Prepare a result.');
    await button('Save workflow').click();
    await page.getByText('New fixture workflow', { exact: true }).waitFor();
    assert.deepEqual(errors, []);
    assert.equal(failedExpected.length, 1);
    assert.deepEqual(await page.evaluate(() => window.automationCsp), []);
    console.log(`PASS ${client}: routines, flow, webhook templates, workflow keyboard and layout checks, exact run replay, Markdown and calendar.`);
    const cold = coldSamples ? await coldNavigation(origin) : {};
    const timings = warmSamples ? await warmNavigation(origin) : {};
    return { mutations: mutations.map(({ id, ...body }) => body), replayed: runBody.length, consoleErrors: errors.length, timings, cold };
  } finally {
    await context.close();
    await app.close();
  }
}

function summarize(values) {
  values.sort((a, b) => a - b);
  const middle = (values.length - 1) / 2;
  return { samples: values.length, median: (values[Math.floor(middle)] + values[Math.ceil(middle)]) / 2,
    p95: values[Math.ceil(values.length * .95) - 1] };
}

async function coldNavigation(origin) {
  const samples = Object.fromEntries(routes.map(({ title }) => [title, []]));
  for (let attempt = 0; attempt < coldSamples; attempt++) {
    for (const { path, title, ready } of routes) {
      const context = await browser.newContext({ viewport: sizes.desktop, timezoneId: 'UTC' });
      try {
        const page = await context.newPage();
        await fixDate(page);
        const response = await page.goto(origin + path, { waitUntil: 'commit' });
        assert.equal(response.status(), 200);
        await page.locator(ready).first().waitFor({ state: 'visible' });
        samples[title].push(await page.evaluate(() => performance.now()));
      } finally { await context.close(); }
    }
  }
  return Object.fromEntries(Object.entries(samples).map(([title, values]) => [title, summarize(values)]));
}

function navigate(page, title) {
  return page.evaluate(title => new Promise((resolve, reject) => {
    const control = document.querySelector('[aria-label="Automation"] [aria-label="' + title + '"]');
    let start, startFrame;
    const check = () => {
      if (start === undefined) return;
      const heading = document.querySelector('main h1:not([hidden], [hidden] *)');
      if (heading?.textContent !== title) return;
      observer.disconnect();
      const commit = performance.now() - start;
      // Let Chromium lay out and paint normally before sampling the visible heading.
      requestAnimationFrame(frame => {
        clearTimeout(timeout);
        if (!heading.getClientRects().length) reject(new Error('Hidden heading for ' + title));
        else resolve({ commit, frame: frame - startFrame });
      });
    };
    const observer = new MutationObserver(check);
    observer.observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ['hidden'] });
    const timeout = setTimeout(() => { observer.disconnect(); reject(new Error('Timed out waiting for ' + title)); }, 5000);
    // Compare from the same point in the frame cycle .
    requestAnimationFrame(frame => {
      startFrame = frame;
      start = performance.now();
      control.click();
      check();
    });
  }), title);
}

async function warmNavigation(origin) {
  const titles = routes.map(({ title }) => title);
  const runs = [];
  try {
    for (const client of clients) {
      const context = await browser.newContext({ viewport: sizes.desktop, timezoneId: 'UTC' });
      const page = await context.newPage();
      runs.push({ client, context, page, samples: Object.fromEntries(titles.map(title => [title, []])) });
      await fixDate(page);
      await page.goto(origin + `/calendar`);
      await page.locator(routes.at(-1).ready).first().waitFor();
      assert.equal(await page.locator('#leptos-root').count(), Number(true));
    }
    for (let i = 0; i < warmSamples + 5; i++) {
      for (const { page, samples } of i % 2 ? runs.toReversed() : runs) {
        await page.bringToFront();
        for (const { title, ready } of routes) {
          const elapsed = await navigate(page, title);
          if (i >= 5) samples[title].push(elapsed);
          // Finish loading the destination before timing the next navigation.
          await page.locator(ready).filter({ visible: true }).first().waitFor();
        }
      }
    }
    return Object.fromEntries(runs.map(({ client, samples }) => [client,
      Object.fromEntries(Object.entries(samples).map(([title, values]) => [title, {
        ...summarize(values.map(value => value.frame)), commit: summarize(values.map(value => value.commit)),
      }]))]));
  } finally {
    await Promise.all(runs.map(({ context }) => context.close()));
  }
}

try {
  browser = await chromium.launch({ headless: true });
  for (const client of clients) receipts[client] = await exercise(client);
  receipts.leptos.timings = receipts.leptos.timings.leptos ?? {};
  await writeFile(join(output, 'receipt.json'), JSON.stringify(receipts, null, 2) + '\n');
  const { timings, cold, ...functional } = receipts.leptos;
  assert.deepEqual(functional, {
    mutations: [
      { revision: 1, name: 'Morning digest edited', prompt: 'Prepare the digest.', trigger: 'schedule',
        schedule: { kind: 'cron', expression: '0 9 * * 1-5' }, zone: 'UTC', project: null, reportTo: 'direct', enabled: true },
      { revision: 0, enabled: true, rotate: true },
    ],
    replayed: 2,
    consoleErrors: 0,
  });
  const route = { Routines: 'automation routines', Workflows: 'automation workflows', 'Automation calendar': 'automation calendar' };
  for (const [name, timing] of Object.entries(cold)) assertP95(route[name], 'cold', timing.p95);
  for (const [name, timing] of Object.entries(timings)) assertP95(route[name], 'warm', timing.p95);
} finally {
  await browser?.close();
  provider.closeAllConnections();
  await new Promise(resolve => provider.close(resolve));
  await rm(scratch, { recursive: true });
}
