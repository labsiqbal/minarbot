import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { cp, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout } from 'node:timers/promises';
import { chromium } from 'playwright-core';
import { freePort, stop } from '../test-process.ts';
import { parityShot } from './parity-shot.mjs';

assert(process.argv.length === 4, 'Pass a staged release directory and a screenshot directory.');
const release = resolve(process.argv[2]);
const output = resolve(process.argv[3]);
const { id: releaseId } = JSON.parse(await readFile(join(release, 'release.json'), 'utf8'));
assert.match(releaseId, /^\d+\.\d+\.\d+-[a-f0-9]{8}$/);
const scratch = await mkdtemp(join(tmpdir(), 'minarbot-67-rollback-'));
const releasesDir = join(scratch, 'releases');
const envFile = join(scratch, 'launcher.env');
const config = join(scratch, 'launcher.json');
const port = freePort();
assert.notEqual(port, 4317);
const origin = `http://127.0.0.1:${port}`;
let held = 0, calls = 0;
const provider = createServer(async (request, response) => {
  let raw = '';
  for await (const chunk of request) raw += chunk;
  response.setHeader('Content-Type', 'application/json');
  if (request.method === 'GET') return response.end(JSON.stringify({ data: [{ id: 'fixture' }] }));
  calls++;
  if (raw.includes('Hold the inspection open.')) { held++; return; }
  const judge = JSON.parse(raw).messages[0]?.content.startsWith('Judge whether');
  const content = judge ? JSON.stringify({ met: true, reason: 'Inspection report exists.', evidence: ['answer'] }) : 'Inspection report complete.';
  response.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content } }], usage: { prompt_tokens: 20, completion_tokens: 10, total_tokens: 30 } }));
});
let child, closed, browser;

async function until(check, label) {
  for (let count = 0; count < 150; count++) {
    const result = await check();
    if (result) return result;
    await setTimeout(100);
  }
  assert.fail(label);
}

async function start(client) {
  const providerEnv = `OPENAI_BASE_URL=http://127.0.0.1:${provider.address().port}/v1\nOPENAI_API_KEY=synthetic\nOPENAI_MODEL=fixture\nMINARBOT_AGENT_DESKTOP=0\n`;
  await writeFile(envFile, providerEnv + (client ? `MINARBOT_APPEARANCE_CLIENT=${client}\n` : ''), { mode: 0o600 });
  child = spawn(join(releasesDir, releaseId, 'bin/minarbot-launcher'), ['--config', config], {
    env: { PATH: '/usr/bin:/bin', HOME: scratch, TMPDIR: scratch }, stdio: ['ignore', 'ignore', 'pipe'],
  });
  closed = once(child, 'close');
  let stderr = '';
  child.stderr.on('data', chunk => { stderr = (stderr + chunk).slice(-4000); });
  await until(async () => {
    assert.equal(child.exitCode, null, stderr);
    return (await fetch(origin + '/api/environment').catch(() => null))?.ok;
  }, 'Launcher did not start the staged release.');
}

async function api(path, payload) {
  const response = await fetch(origin + path, payload === undefined ? {} : {
    method: 'POST', headers: { Origin: origin, 'Content-Type': 'application/json' }, body: JSON.stringify(payload),
  });
  assert(response.ok, `${path} ${response.status}: ${await response.clone().text()}`);
  return response.json();
}

try {
  await mkdir(output, { recursive: true });
  await mkdir(releasesDir, { mode: 0o700 });
  await cp(release, join(releasesDir, releaseId), { recursive: true });
  await symlink(releaseId, join(releasesDir, 'current'));
  await writeFile(config, JSON.stringify({ releasesDir, dataDir: join(scratch, 'data'), envFile, port }), { mode: 0o600 });
  provider.listen(0, '127.0.0.1');
  await once(provider, 'listening');
  await start();

  const workspace = join(scratch, 'work');
  await mkdir(workspace);
  const names = ['Field planner', 'Inventory reviewer'];
  const [planner, reviewer] = await Promise.all(names.map(async name => (await api('/api/agents', {
    id: randomUUID(), name, runtime: 'api', model: 'fixture', instructions: 'Summarize the synthetic greenhouse inventory.',
  })).id));
  const agent = `/api/agents/${planner}`, second = `/api/agents/${reviewer}`;
  const project = await api('/api/projects', { id: randomUUID(), name: 'Greenhouse operations', description: 'Track inventory and maintenance.', folders: [{ mode: 'existing', path: workspace }], members: [{ agent: planner, write: false }, { agent: reviewer, write: false }] });
  const thread = randomUUID();
  await api(agent + '/threads', { id: thread, name: 'Spring inventory' });
  const turn = randomUUID();
  await api(`/api/threads/${thread}/turns`, { id: turn, text: 'Summarize inventory for the north greenhouse.' });
  const finished = await api(agent + '/goal', { id: randomUUID(), target: 'Finish the inspection report.' });
  await until(async () => (await api(agent + '/goal'))?.status === 'met', 'Finished goal did not settle.');
  await api(second + '/goal', { id: randomUUID(), target: 'Hold the inspection open.' });
  await until(() => held === 1, 'Active goal did not reach the provider.');
  assert.equal((await api(second + '/goal')).status, 'active');
  const loop = randomUUID();
  await api(agent + '/loop', { id: loop, prompt: 'Check greenhouse humidity.', intervalSeconds: 3600, maxRuns: 4 });
  const routine = await api(agent + '/routines', { id: randomUUID(), name: 'Maintenance digest', prompt: 'Summarize open maintenance work.', zone: 'UTC', schedule: { kind: 'cron', expression: '30 9 * * 1' }, enabled: false });
  await api(agent + '/workflows', { id: randomUUID(), revision: 0, name: 'Inventory report', graph: { entry: 'out', maxSteps: 8, nodes: [{ id: 'out', kind: 'output' }] } });
  await api(agent + '/memory', { id: randomUUID(), entry: randomUUID(), text: 'Inventory labels are in the north cabinet.', revision: 0, essential: true, remove: false });

  const persisted = ['/api/agents', `/api/projects/${project.id}`, agent + '/threads', `/api/threads/${thread}/turns`, agent + '/goal', `/api/routines/${loop}`, `/api/routines/${routine.id}`, agent + '/workflows', agent + '/memory'];
  const snapshot = await Promise.all(persisted.map(path => api(path)));
  assert.equal(snapshot[3].find(row => row.id === turn)?.status, 'done');
  assert.equal(snapshot[4].id, finished.id);
  const previousCalls = calls;
  browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1280, height: 860 } });
  const postAttempts = [];
  page.on('request', request => {
    if (request.method() === 'POST' && request.url().startsWith(origin + '/api/')) postAttempts.push(request.url());
  });
  const conversation = `/agents/${planner}`, named = `${conversation}/conversations/${thread}`;
  const composer = page.getByRole('textbox', { name: `Message ${names[0]}`, exact: true });
  const retry = page.getByRole('button', { name: 'Retry send', exact: true });
  const pendingKey = `minarbot:pending:${planner}`, draftKey = `minarbot:draft:thread:${thread}`;
  const draft = 'Draft: compare supplies before Friday.';
  const uncertain = 'Check the north cabinet before ordering labels.';
  const storage = key => page.evaluate(key => sessionStorage.getItem(key), key);
  async function open(path, client) {
    const response = await page.goto(origin + path);
    assert(response.ok());
    const html = await response.text();
    assert.equal(html.includes('id="leptos-root"'), client === 'leptos', path);
    await composer.waitFor();
  }

  await open(named, 'leptos');
  await composer.fill(draft);
  assert.equal(await storage(draftKey), draft);
  await open(conversation, 'leptos');
  await page.route(origin + agent + '/turns', route => route.request().method() === 'POST' ? route.abort('connectionreset') : route.continue());
  await composer.fill(uncertain);
  await composer.press('Enter');
  await retry.waitFor();
  const journal = await storage(pendingKey);
  assert.equal(JSON.parse(journal).text, uncertain);
  assert.equal(postAttempts.length, 1);

  for (const [label, selector, query] of [['visit-react', null, '?ui=react'], ['restart-react', 'react', ''], ['restart-leptos', '', '']]) {
    if (selector !== null) {
      await stop(child, closed);
      await start(selector);
      assert.equal((await api(second + '/goal')).status, 'interrupted');
    }
    const client = label.endsWith('react') ? 'react' : 'leptos';
    await open(named + query, client);
    assert.equal(await composer.inputValue(), draft);
    await open(conversation + query, client);
    await retry.waitFor();
    assert.equal(await composer.inputValue(), uncertain);
    assert.equal(await storage(pendingKey), journal);
    await parityShot(page, join(output, label + '.png'));
    const reread = await Promise.all(persisted.map(path => api(path)));
    assert(reread[0].agents.some(row => row.id === planner));
    assert.equal(reread[1].id, project.id);
    assert(reread[2].some(row => row.id === thread));
    assert.equal(reread[3].find(row => row.id === turn)?.text, 'Summarize inventory for the north greenhouse.');
    assert.equal(reread[4].id, finished.id);
    assert.equal(reread[5].some(row => row.id === loop), true);
    assert.equal(reread[6].id, routine.id);
    assert.equal(reread[7].workflows.some(row => row.id), true);
    assert.equal(reread[8].items[0].text, 'Inventory labels are in the north cabinet.');
    assert.equal((await api(agent + '/turns')).some(row => row.id === JSON.parse(journal).id), false);
    assert.equal(postAttempts.length, 1);
    assert.equal(calls, previousCalls);
  }
  console.log(JSON.stringify({ releaseId, clients: ['leptos', 'visit-react', 'restart-react', 'restart-leptos'], persistedEndpoints: persisted.length, draftRetained: true, journalRetained: true, providerCalls: calls, automaticReplay: 0, runningGoalAfterRestart: 'interrupted' }));
} finally {
  await browser?.close();
  if (child) await stop(child, closed);
  provider.closeAllConnections();
  await new Promise(resolve => provider.close(resolve));
  await rm(scratch, { recursive: true, force: true });
}
