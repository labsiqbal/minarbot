import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { mkdtemp } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import { startRustServer } from '../../rust-test-server.ts';
import { rustBinary } from '../../rust-binary.ts';

const work = await mkdtemp(join(tmpdir(), 'minarbot-components-'));
const model = 'component-fixture';
const prompt = 'Show the October report and ask what to compare.';
const held = 'Hold the turn for MCP checks.';
const stopped = 'Wait for form, then stop.';
const children = [
  { type: 'bar', title: 'Weekly signups', data: 'signups' },
  { type: 'line', title: 'Net change', data: 'change' },
  { type: 'pie', title: 'Signup channels', data: 'channels' },
  { type: 'table', title: 'October totals', data: 'totals' },
  { type: 'metric', title: 'New accounts', data: 'accounts' },
  { type: 'timeline', title: 'Launch milestones', data: 'milestones' },
  { type: 'mermaid', title: 'Signup journey', data: 'journey' },
];
const catalog = { tree: { type: 'stack', children }, data: {
  signups: [{ label: 'Week 1', value: 26 }, { label: 'Week 2', value: 40 }, { label: 'Week 3', value: 51 }, { label: 'Week 4', value: 67 }],
  change: [{ label: 'Week 1', value: -3 }, { label: 'Week 2', value: 14 }, { label: 'Week 3', value: 11 }, { label: 'Week 4', value: 16 }],
  channels: [{ label: 'Organic', value: 90 }, { label: 'Referrals', value: 60 }, { label: 'Direct', value: 34 }],
  totals: { columns: ['Channel', 'Accounts'], rows: [['Organic', 90], ['Referrals', 60], ['Direct', 34]] },
  accounts: { value: 184, detail: 'Across all signup channels' },
  milestones: [{ label: 'Launch', when: 'October 1', detail: 'Invites opened.' }, { label: 'Follow-up', when: 'October 22' }],
  journey: { source: 'graph LR\n  A[Visit] --> B[Sign up]\n  B --> C[Activate]', description: 'People visit the site, sign up, then activate their accounts.' },
}, actions: [] };
const form = { tree: { type: 'stack', children: [{ type: 'form', title: 'Compare periods', data: 'fields', action: 'compare' }] },
  data: { fields: [
    { name: 'label', label: 'Comparison name', type: 'text', required: true },
    { name: 'weeks', label: 'Weeks to compare', type: 'number', required: true },
    { name: 'channel', label: 'Signup channel', type: 'select', options: ['Organic', 'Referrals', 'Direct'], required: true },
    { name: 'includeDraft', label: 'Include draft data', type: 'boolean' },
  ] },
  actions: [{ id: 'compare', type: 'submit', label: 'Run comparison', context: { report: 'october', version: 1 } }],
};
const values = { label: 'October review', weeks: 2, channel: 'Organic', includeDraft: true };
const scriptURL = 'https://cdn.jsdelivr.net/npm/mermaid@10.9.5/dist/mermaid.min.js';
const libraryResponse = await fetch(scriptURL, { signal: AbortSignal.timeout(30_000) });
assert.equal(libraryResponse.status, 200);
const library = Buffer.from(await libraryResponse.arrayBuffer());
assert.equal(createHash('sha256').update(library).digest('hex'), '616a109f19cd186842e11d45b35ac07456b3a75513310f6ea075351aa430b1e2');

let releaseHold;
const hold = new Promise(resolve => { releaseHold = resolve; });
let apiFormResult;
let declared = false;
const provider = createServer(async (req, res) => {
  res.setHeader('Content-Type', 'application/json');
  if (req.method === 'GET') return res.end(JSON.stringify({ data: [{ id: model }] }));
  try {
    let raw = '';
    for await (const chunk of req) raw += chunk;
    const { messages, tools } = JSON.parse(raw);
    const user = messages.findLast(message => message.role === 'user')?.content;
    const result = messages.at(-1)?.role === 'tool' ? JSON.parse(messages.at(-1).content) : null;
    let args;
    if (user === held) await hold;
    if (user === prompt) {
      declared ||= tools.some(tool => tool.function.name === 'render_component' && tool.function.parameters.$defs?.form);
      if (!result) args = catalog;
      else if (result.status === 'rendered') args = form;
      else apiFormResult = result;
    } else if (user === stopped && !result) args = form;
    const calls = args && [{ type: 'function', id: randomUUID(), function: {
      name: 'render_component', arguments: JSON.stringify(args),
    } }];
    res.end(JSON.stringify({ choices: [{ finish_reason: calls ? 'tool_calls' : 'stop', message: calls
      ? { content: null, tool_calls: calls } : { content: 'Comparison ready.' } }] }));
  } catch {
    res.writeHead(500).end();
  }
});

let app, browser, stdio;
try {
  await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
  app = await startRustServer({ database: join(work, 'state.sqlite'), apiKey: 'fixture-key', defaultModel: model, stream: false,
    dist: fileURLToPath(new URL('../../dist', import.meta.url)), baseURL: `http://127.0.0.1:${provider.address().port}/v1` });
  async function api(path, data, status = 200) {
    const response = await fetch(app.origin + path, { method: data ? 'POST' : 'GET', headers: {
      Origin: app.origin, 'Sec-Fetch-Site': 'same-origin', 'Content-Type': 'application/json',
    }, body: data ? JSON.stringify(data) : undefined });
    const body = await response.json();
    assert.equal(response.status, status, JSON.stringify(body));
    return body;
  }
  const agent = await api('/api/agents', { id: randomUUID(), runtime: 'api', model, name: 'Reports', instructions: 'Summarize fixture data.' }, 201);
  const thread = await api(`/api/agents/${agent.id}/threads`, { id: randomUUID(), name: 'October report' });
  const scope = `/api/threads/${thread.id}`;
  const path = `${scope}/turns`;
  const conversation = `/agents/${agent.id}/conversations/${thread.id}`;
  const send = async text => {
    const id = randomUUID();
    await api(path, { id, text }, 202);
    return id;
  };
  const read = async id => (await api(path)).find(turn => turn.id === id);
  async function until(id, predicate) {
    const deadline = Date.now() + 25_000;
    let turn;
    do {
      turn = await read(id);
      if (predicate(turn)) return turn;
      if (['failed', 'cancelled', 'interrupted'].includes(turn?.status)) break;
      await delay(50);
    } while (Date.now() < deadline);
    assert.fail(`Turn state did not settle: ${turn?.status} ${turn?.error ?? ''}`);
  }
  const pendingCard = turn => turn?.harnessApprovals.find(card => card.kind === 'form' && card.state === 'approval');
  const decidePath = (id, card) => `${path}/${id}/harness-approvals/${card.id}`;
  const id = await send(prompt);
  const waiting = await until(id, pendingCard);
  assert(declared && !apiFormResult);
  assert.equal(waiting.status, 'running');
  assert.equal(waiting.htmlReplies, undefined);
  const card = pendingCard(waiting);
  const decision = { id: randomUUID(), revision: card.revision, decision: 'allow', values };
  const invalidValues = [{ ...values, weeks: '2' }, { ...values, extra: true }, { ...values, channel: 'Unknown' }, { ...values, label: 'fixture-key' }];
  for (const invalid of invalidValues) await api(decidePath(id, card), { ...decision, id: randomUUID(), values: invalid }, 400);
  await api(decidePath(id, card), { id: randomUUID(), revision: card.revision, decision: 'allow' }, 400);
  await api(decidePath(id, card).replace(scope, `/api/agents/${agent.id}`), decision, 404);

  browser = await chromium.launch({ executablePath: process.env.CHROME_PATH ?? '/usr/bin/google-chrome' });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1020 }, colorScheme: 'dark' });
  await page.route(scriptURL, route => route.fulfill({
    contentType: 'text/javascript', headers: { 'Access-Control-Allow-Origin': '*' }, body: library,
  }));
  await page.goto(app.origin + conversation);
  const chart = page.getByRole('heading', { name: 'Weekly signups' });
  const disclosure = page.getByText('View data', { exact: false }).first();
  const chartTable = page.getByRole('table', { name: 'Weekly signups data' });
  const formRegion = page.getByRole('form', { name: 'Compare periods' });
  await chart.waitFor();
  await formRegion.waitFor();
  await page.getByText('Waiting for your input', { exact: true }).waitFor();
  await disclosure.press('Enter');
  assert.match(await chartTable.ariaSnapshot(), /columnheader "Value"[\s\S]*rowheader "Week 4"[\s\S]*cell "67"/);
  await page.getByRole('table', { name: 'October totals' }).waitFor();
  await page.getByRole('heading', { name: 'Net change' }).waitFor();
  await page.getByRole('heading', { name: 'Signup channels' }).waitFor();
  await page.getByRole('heading', { name: 'New accounts' }).waitFor();
  await page.getByRole('heading', { name: 'Launch milestones' }).waitFor();
  const frame = page.locator('iframe[title="Signup journey"]');
  const diagram = frame.contentFrame().locator('#diagram svg');
  await frame.scrollIntoViewIfNeeded();
  await diagram.waitFor();
  assert.equal(await frame.getAttribute('sandbox'), 'allow-scripts allow-forms');
  const frameSrc = (await frame.getAttribute('src')).split('#')[0];
  const csp = (await page.request.get(app.origin + frameSrc)).headers()['content-security-policy'];
  assert.match(csp, /sandbox allow-scripts allow-forms/);
  assert.doesNotMatch(csp, /allow-same-origin/);

  for (const mode of ['dark', 'light']) {
    await page.evaluate(mode => { document.documentElement.dataset.theme = mode; }, mode);
    await chart.scrollIntoViewIfNeeded();
    await page.screenshot({ path: join(work, `catalog-${mode}.png`), animations: 'disabled' });
    await formRegion.scrollIntoViewIfNeeded();
    await page.screenshot({ path: join(work, `form-${mode}.png`), animations: 'disabled' });
  }
  await page.reload();
  await formRegion.waitFor();
  const label = formRegion.getByRole('textbox', { name: 'Comparison name' });
  await formRegion.getByRole('button', { name: 'Run comparison' }).press('Enter');
  assert.equal(await label.evaluate(input => input === document.activeElement && input.validity.valueMissing), true);
  await page.keyboard.type(values.label);
  await page.keyboard.press('Tab');
  assert.equal(await formRegion.getByRole('spinbutton').evaluate(input => input === document.activeElement), true);
  await page.keyboard.type(String(values.weeks));
  await page.keyboard.press('Tab');
  assert.equal(await formRegion.getByRole('combobox').evaluate(input => input === document.activeElement), true);
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('Tab');
  assert.equal(await formRegion.getByRole('checkbox').evaluate(input => input === document.activeElement), true);
  await page.keyboard.press('Space');
  await page.keyboard.press('Tab');
  await page.keyboard.press('Tab');
  await page.keyboard.press('Enter');
  await page.getByText('Comparison ready.').waitFor();
  assert.deepEqual(apiFormResult, { status: 'submitted', id: card.id, action: 'compare', context: { report: 'october', version: 1 }, values });
  await page.reload();
  await page.getByText('Form submitted', { exact: true }).waitFor();

  const stopId = await send(stopped);
  const stopTurn = await until(stopId, pendingCard);
  await api(`${path}/${stopId}/cancel`, { id: randomUUID(), revision: stopTurn.revision });
  const oldCard = pendingCard(stopTurn);
  await api(decidePath(stopId, oldCard), { ...decision, id: randomUUID(), revision: oldCard.revision }, 409);

  const mcpId = await send(held);
  await until(mcpId, turn => turn.status === 'running');
  const session = app.mcpServer.mintAgentSession({ agent: agent.id, turn: mcpId, scopes: ['orchestration'], ttlMs: 120_000 });
  const peer = app.mcpServer.createPeerToken({ id: randomUUID(), name: 'Peer', scopes: ['orchestration'] });
  const rpcMessage = (method, params) => ({ jsonrpc: '2.0', id: randomUUID(), method, params });
  const call = arguments_ => rpcMessage('tools/call', { name: 'render_component', arguments: arguments_ });
  async function rpc(token, message) {
    const response = await fetch(app.origin + '/mcp', { method: 'POST', headers: {
      Authorization: `Bearer ${token}`, 'Content-Type': 'application/json',
    }, body: JSON.stringify(message) });
    return { status: response.status, ...(await response.json()) };
  }
  const list = rpcMessage('tools/list', {});
  assert((await rpc(session.token, list)).result.tools.some(tool => tool.name === 'render_component'));
  assert(!(await rpc(peer.token, list)).result.tools.some(tool => tool.name === 'render_component'));
  assert.equal((await rpc(session.token, call({ ...form, extra: true }))).status, 400);
  const wrongBinding = structuredClone(catalog);
  wrongBinding.data.accounts = catalog.data.signups;
  assert.equal((await rpc(session.token, call(wrongBinding))).result.isError, true);
  const protectedValue = structuredClone(catalog);
  protectedValue.data.accounts.value = session.token;
  assert.equal((await rpc(session.token, call(protectedValue))).status, 400);

  let settled = false;
  const outstanding = rpc(session.token, call(form)).then(result => { settled = true; return result; });
  const mcpCard = pendingCard(await until(mcpId, pendingCard));
  assert(!settled);
  const mcpDecision = { ...decision, id: randomUUID(), revision: mcpCard.revision };
  const saved = await api(decidePath(mcpId, mcpCard), mcpDecision);
  assert.deepEqual(await api(decidePath(mcpId, mcpCard), mcpDecision), saved);
  await api(decidePath(mcpId, mcpCard), { ...mcpDecision, values: { ...values, weeks: 3 } }, 409);
  assert.deepEqual((await outstanding).result.structuredContent,
    { status: 'submitted', id: mcpCard.id, action: 'compare', context: { report: 'october', version: 1 }, values });

  stdio = spawn(rustBinary('minarbot-server', process.env.MINARBOT_RUST_BINARY), ['--mcp-stdio'], {
    env: { ...process.env, MINARBOT_MCP_URL: app.origin + '/mcp', MINARBOT_MCP_TOKEN: session.token },
    stdio: ['pipe', 'pipe', 'inherit'],
  });
  const lines = createInterface({ input: stdio.stdout })[Symbol.asyncIterator]();
  stdio.stdin.end(JSON.stringify(call(form)) + '\n');
  const stdioCard = pendingCard(await until(mcpId, pendingCard));
  await api(decidePath(mcpId, stdioCard), { id: randomUUID(), revision: stdioCard.revision, decision: 'deny' });
  const line = await lines.next();
  assert.equal(JSON.parse(line.value).result.structuredContent.status, 'cancelled');
  console.log('PASS catalog, Mermaid sandbox, both themes, keyboard form, accessibility tree, API resume, reload, validation, stop, HTTP and stdio MCP');
  console.log(`Screenshots: ${work}`);
} finally {
  releaseHold();
  if (stdio?.exitCode === null) stdio.kill('SIGTERM');
  await browser?.close();
  await app?.close();
  provider.closeAllConnections();
  await new Promise(resolve => provider.close(resolve));
}
