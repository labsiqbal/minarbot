import assert from 'node:assert/strict';
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
const clients = process.env.MINARBOT_SMOKE_UI ? [process.env.MINARBOT_SMOKE_UI] : ['react', 'leptos'];
const coldSamples = Number(process.env.MINARBOT_AUTOMATION_COLD_BENCH ?? 0);
assert(Number.isSafeInteger(coldSamples) && coldSamples >= 0);
const routes = [
  { path: '/routines', title: 'Routines', ready: 'main .routine-table tbody tr' },
  { path: '/workflows', title: 'Workflows', ready: 'main .k-row__title' },
  { path: '/calendar', title: 'Automation calendar', ready: 'main .calendar-grid .calendar-event' },
];
const fixtureIds = { agent: randomUUID(), other: randomUUID(), workflow: randomUUID(), routine: randomUUID() };

async function exercise(client) {
  const app = await startApp({ database: join(scratch, client + '.sqlite'), dist: resolve('dist'), port: 0, appearanceClient: client,
    defaultModel: 'fixture', baseURL: 'http://127.0.0.1:' + provider.address().port + '/v1', stream: false });
  const context = await browser.newContext({ viewport: sizes.desktop, timezoneId: 'UTC' });
  const errors = [], failedExpected = [];
  await context.addInitScript(() => {
    window.automationCsp = [];
    addEventListener('securitypolicyviolation', event => window.automationCsp.push(event.effectiveDirective));
  });
  const page = await context.newPage();
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
    assert.equal((await response.headerValue('content-security-policy')).includes('wasm-unsafe-eval'), client === 'leptos');
    await heading(title).waitFor();
    assert.equal(await page.locator('#leptos-root').count(), client === 'leptos' ? 1 : 0);
    if (client === 'leptos') {
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
  const capture = async name => {
    for (const [viewport, size] of Object.entries(sizes)) {
      await page.setViewportSize(size);
      for (const theme of ['dark', 'light']) {
        await page.evaluate(value => { document.documentElement.dataset.theme = value; }, theme);
        assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), `${client} ${name} overflows ${viewport}`);
        await page.screenshot({ path: join(output, `${client}-${name}-${viewport}-${theme}.png`), animations: 'disabled', mask: [page.locator('input[type=password]')] });
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
    const agent = await api('/api/agents', { id: fixtureIds.agent, runtime: 'api', name: 'Automation owner', model: 'fixture', instructions: 'Synthetic fixture.' });
    await api('/api/agents', { id: fixtureIds.other, runtime: 'api', name: 'Automation helper', model: 'fixture', instructions: 'Synthetic fixture.' });
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
    await button('All routines').click();
    await button('New routine').first().click();
    dialog = page.getByRole('dialog', { name: 'New routine' });
    await dialog.getByLabel('Name', { exact: true }).fill('Manual review');
    await dialog.getByLabel('Instructions', { exact: true }).fill('Review the fixture.');
    await choose('Trigger', 'Manual', dialog);
    await button('Save routine', dialog).click();
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
    if (client === 'leptos') {
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
    await inspectGraph('editor');
    await capture('workflow-editor');
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
    await button('Retry request').click();
    await page.waitForFunction(async id => {
      const response = await fetch('/api/workflow-runs/' + id);
      return response.ok && (await response.json()).status === 'done';
    }, runBody[0].id);
    assert.deepEqual(runBody[0], runBody[1]);
    assert.equal((await api(`/api/workflows/${workflow.id}/runs`)).length, 1);
    await button('Open run').first().click();
    await page.locator('.workflow-run-node').filter({ hasText: 'Completed' }).first().waitFor();
    await page.locator('main .markdown strong').filter({ hasText: 'Automation fixture completed.' }).first().waitFor();
    await inspectGraph('completed run');
    await capture('workflow-run');
    if (client === 'leptos') {
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

    const timings = await warmNavigation(page);
    if (client === 'leptos') {
      await page.goBack();
      await heading('Workflows').waitFor();
      await page.goForward();
      await heading('Automation calendar').waitFor();
    }
    await issue1Smoke({ page,
      check: async (name, run) => { if (name.startsWith('Routine templates')) await run(); },
      screenshot: name => page.screenshot({ path: join(output, `${client}-${name}.png`),
        animations: 'disabled', mask: [page.locator('input[type=password]')] }),
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
        const response = await page.goto(origin + path, { waitUntil: 'commit' });
        assert.equal(response.status(), 200);
        await page.locator(ready).first().waitFor({ state: 'visible' });
        samples[title].push(await page.evaluate(() => performance.now()));
      } finally { await context.close(); }
    }
  }
  return Object.fromEntries(Object.entries(samples).map(([title, values]) => [title, summarize(values)]));
}

async function warmNavigation(page) {
  const titles = routes.map(({ title }) => title);
  await page.getByRole('navigation', { name: 'Automation', exact: true }).waitFor();
  const samples = Object.fromEntries(titles.map(title => [title, []]));
  for (let i = 0; i < 45; i += 1) {
    for (const title of titles) {
      const elapsed = await page.evaluate(title => new Promise((resolve, reject) => {
        const control = document.querySelector('[aria-label="Automation"] [aria-label="' + title + '"]');
        const check = () => {
          const heading = document.querySelector('main:not([hidden]) h1');
          if (heading?.textContent !== title || !heading.getClientRects().length) return;
          observer.disconnect();
          clearTimeout(timeout);
          resolve(performance.now() - start);
        };
        const observer = new MutationObserver(check);
        observer.observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ['hidden'] });
        const timeout = setTimeout(() => { observer.disconnect(); reject(new Error('Timed out waiting for ' + title)); }, 5000);
        const start = performance.now();
        control.click();
        check();
      }), title);
      if (i >= 5) samples[title].push(elapsed);
    }
  }
  return Object.fromEntries(Object.entries(samples).map(([title, values]) => [title, summarize(values)]));
}

try {
  browser = await chromium.launch({ headless: true });
  for (const client of clients) receipts[client] = await exercise(client);
  await writeFile(join(output, 'receipt.json'), JSON.stringify(receipts, null, 2) + '\n');
  if (receipts.react && receipts.leptos) {
    const { timings: reactTiming, cold: reactCold, ...react } = receipts.react;
    const { timings: leptosTiming, cold: leptosCold, ...leptos } = receipts.leptos;
    assert.deepEqual(leptos, react);
    for (const key of Object.keys(reactTiming)) {
      console.log(`${key} warm p95: React ${reactTiming[key].p95.toFixed(1)} ms, Leptos ${leptosTiming[key].p95.toFixed(1)} ms`);
    }
    for (const key of Object.keys(reactCold)) {
      console.log(`${key} cold p95: React ${reactCold[key].p95.toFixed(1)} ms, Leptos ${leptosCold[key].p95.toFixed(1)} ms`);
    }
    for (const key of Object.keys(reactTiming)) {
      assert(leptosTiming[key].p95 <= reactTiming[key].p95 * 1.1, `${key} exceeded the warm p95 regression budget`);
    }
    for (const key of Object.keys(reactCold)) {
      assert(leptosCold[key].p95 <= reactCold[key].p95 * 1.2, `${key} exceeded the cold p95 regression budget`);
    }
  }
} finally {
  await browser?.close();
  provider.closeAllConnections();
  await new Promise(resolve => provider.close(resolve));
  await rm(scratch, { recursive: true });
}
