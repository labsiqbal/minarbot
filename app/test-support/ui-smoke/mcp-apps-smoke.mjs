import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtemp, readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import { startRustServer } from '../../rust-test-server.ts';
import { appContext, appLink, appTool, readMcpAppMessage } from '../../src/mcp-app-host.ts';

const work = await mkdtemp(join(tmpdir(), 'minarbot-mcp-apps-'));
const html = await readFile(new URL('./mcp-app-fixture.html', import.meta.url), 'utf8');
const model = 'mcp-app-fixture';
const uri = 'ui://warehouse/shipments';
const secret = 'mcp-apps-fixture-token-canary';
const title = 'Weekly shipments';
const contents = { type: 'object', additionalProperties: false };
const tools = [
  { name: 'chart', title, inputSchema: { ...contents, properties: { warehouse: { type: 'string' } } }, _meta: { ui: { resourceUri: uri } } },
  { name: 'lookup', inputSchema: contents, annotations: { readOnlyHint: true }, _meta: { ui: { visibility: ['app'] } } },
  { name: 'save', inputSchema: { ...contents, properties: { week: { type: 'integer' } }, required: ['week'] } },
  { name: 'model_only', inputSchema: contents, _meta: { ui: { visibility: ['model'] } } },
  { name: 'ungranted', inputSchema: contents, annotations: { readOnlyHint: true } },
];
const effects = { chart: 0, lookup: 0, save: 0 };
let app;
let connection;
let reads = 0;
let followup = '';
const fixture = createServer(async (req, res) => {
  let raw = '';
  for await (const chunk of req) raw += chunk;
  res.setHeader('Content-Type', 'application/json');
  if (req.url === '/v1/models') return res.end(JSON.stringify({ data: [{ id: model }] }));
  if (req.url === '/v1/chat/completions') {
    const messages = JSON.parse(raw).messages;
    const user = messages.findLast(message => message.role === 'user')?.content ?? '';
    if (user.startsWith('Compare all three weeks.')) followup = user;
    const shouldCall = user === 'Show shipments.' && messages.at(-1).role !== 'tool';
    if (messages.at(-1).role === 'tool') {
      assert(!messages.at(-1).content.includes('<!doctype'));
      assert(!messages.at(-1).content.includes(secret));
    }
    const message = shouldCall ? { tool_calls: [{
      type: 'function', id: randomUUID(), function: { name: 'call_mcp_tool',
        arguments: JSON.stringify({ connection, name: 'chart', arguments: { warehouse: 'North' } }) },
    }] } : { content: followup ? 'Comparison complete.' : 'Shipments increased each week.' };
    return res.end(JSON.stringify({ choices: [{ finish_reason: shouldCall ? 'tool_calls' : 'stop', message }] }));
  }

  assert.equal(req.url, '/mcp');
  assert(req.headers.authorization === `Bearer ${secret}`, 'MCP fixture received matching authorization');
  if (req.method === 'DELETE') { res.writeHead(200); return res.end(); }
  const { id, method, params } = JSON.parse(raw);
  if (id === undefined) { res.writeHead(202); return res.end(); }
  const content = text => ({ content: [{ type: 'text', text }] });
  let result;
  switch (method) {
    case 'initialize':
      result = { protocolVersion: '2025-03-26', capabilities: { tools: {}, resources: {} },
        serverInfo: { name: 'Warehouse', version: '1' } };
      break;
    case 'tools/list': result = { tools }; break;
    case 'resources/list': result = { resources: [] }; break;
    case 'resources/templates/list': result = { resourceTemplates: [] }; break;
    case 'resources/read':
      reads++;
      assert.equal(params.uri, uri);
      result = { contents: [{ uri, mimeType: 'text/html; profile=mcp-app', blob: Buffer.from(html).toString('base64'),
        _meta: { ui: { csp: {
          connectDomains: ['https://metrics.example.test', app?.origin],
          resourceDomains: ['https://assets.example.test', "https://invalid.example.test; script-src *"],
          frameDomains: ['https://video.example.test'],
          baseUriDomains: ['https://assets.example.test'],
        } } },
      }] };
      break;
    case 'tools/call':
      assert(Object.hasOwn(effects, params.name), 'Only granted App tools should run');
      effects[params.name]++;
      result = params.name === 'chart' ? { ...content('Inventory chart ready.'), structuredContent: { total: 139 } }
        : params.name === 'lookup' ? content('139 shipments') : content(`Saved week ${params.arguments.week}`);
      break;
    default: assert.fail('Unexpected MCP method');
  }
  res.end(JSON.stringify({ jsonrpc: '2.0', id, result }));
});
await new Promise(resolve => fixture.listen(0, '127.0.0.1', resolve));
const base = `http://127.0.0.1:${fixture.address().port}`;

let browser;
try {
  app = await startRustServer({
    database: join(work, 'state.sqlite'),
    dist: fileURLToPath(new URL('../../dist', import.meta.url)),
    baseURL: base + '/v1', apiKey: 'fixture-key', defaultModel: model,
  });
  async function api(path, body, status = 200, headers = {}) {
    const response = await fetch(app.origin + path, {
      method: body ? 'POST' : 'GET',
      headers: { Origin: app.origin, 'Content-Type': 'application/json', 'Sec-Fetch-Site': 'same-origin', ...headers },
      body: body ? JSON.stringify(body) : undefined,
    });
    const result = await response.json();
    assert.equal(response.status, status, `${path} ${JSON.stringify(result)}`);
    assert(!JSON.stringify(result).includes(secret));
    return result;
  }
  const agent = await api('/api/agents', { id: randomUUID(), runtime: 'api', model, name: 'Warehouse', instructions: 'Show the inventory.' }, 201);
  const thread = await api(`/api/agents/${agent.id}/threads`, { id: randomUUID(), name: 'Inventory' });
  const scope = `/api/threads/${thread.id}`;
  const path = scope + '/turns';
  const tested = await api('/api/mcp/test', { id: randomUUID(), name: 'Warehouse', url: base + '/mcp', token: secret });
  connection = (await api('/api/mcp', { id: randomUUID(), test: tested.id })).id;
  const grant = `/api/agents/${agent.id}/mcp/${connection}`;
  const granted = ['chart', 'lookup', 'save', 'model_only'];
  await api(grant, { id: randomUUID(), revision: 0, connectionRevision: 1, tools: granted });
  await api(path, { id: randomUUID(), text: 'Show shipments.' }, 202);

  browser = await chromium.launch({ executablePath: process.env.CHROME_PATH ?? '/usr/bin/google-chrome' });
  const page = await browser.newPage({ viewport: { width: 1200, height: 860 }, colorScheme: 'dark' });
  await page.context().route('https://assets.example.test/chart.js', route => route.fulfill({
    contentType: 'application/javascript', body: 'globalThis.fixtureLibrary = true;',
  }));
  await page.context().route('https://metrics.example.test/total', route => route.fulfill({
    headers: { 'Access-Control-Allow-Origin': '*' }, body: '139',
  }));
  await page.context().route('https://guide.example.test/inventory', route => route.fulfill({ body: 'Inventory guide' }));
  const history = `/agents/${agent.id}/conversations/${thread.id}`;
  await page.goto(app.origin + history);
  const outer = page.locator(`iframe[title="${title}"]`);
  const inner = outer.contentFrame().locator('iframe').contentFrame();
  await inner.locator('body[data-input="North"][data-total="139"][data-protocol="2026-01-26"][data-pre-init="-32000"]').waitFor({ timeout: 45_000 });
  await page.locator('p').filter({ hasText: 'Shipments increased each week.' }).waitFor();
  assert.deepEqual(effects, { chart: 1, lookup: 0, save: 0 });
  assert.equal(reads, 1);
  assert.equal(await outer.getAttribute('sandbox'), 'allow-scripts allow-forms');
  assert.equal(await outer.contentFrame().locator('iframe').getAttribute('sandbox'), 'allow-scripts allow-forms');
  const route = (await outer.getAttribute('src')).split('#')[0];
  const response = await fetch(app.origin + route);
  assert.equal(response.status, 200);
  assert.equal(response.headers.get('cache-control'), 'no-store');
  const csp = response.headers.get('content-security-policy');
  assert.match(csp, /sandbox allow-scripts allow-forms; frame-ancestors 'self'/);
  assert.match(csp, /connect-src https:\/\/metrics\.example\.test;/);
  assert.match(csp, /frame-src https:\/\/video\.example\.test;/);
  assert.match(csp, /base-uri https:\/\/assets\.example\.test;/);
  assert(!csp.includes(app.origin));
  assert.doesNotMatch(csp, /allow-same-origin|invalid\.example/);
  assert.equal((await fetch(app.origin + route, { headers: { 'Sec-Fetch-Site': 'cross-site' } })).status, 403);
  await api(route.replace(scope, `/api/agents/${agent.id}`) + '/data', null, 404);
  await api(route + '/data', null, 404, { 'Sec-Fetch-Site': 'same-site' });
  await api(route + '/call', { name: 'lookup' }, 403, { Origin: 'null' });
  await api(route + '/call', { name: 'model_only' }, 403);
  await api(route + '/call', { name: 'ungranted' }, 403);
  await api(route + '/call', { name: 'lookup', arguments: [] }, 400);
  await page.evaluate(() => { document.cookie = 'fixture-cookie=1; SameSite=Strict'; });
  assert.deepEqual(await inner.locator('body').evaluate(async (_, origin) => {
    const access = action => { try { action(); return true; } catch { return false; } };
    const get = url => fetch(url).then(response => response.text(), () => false);
    return {
      parent: access(() => parent.document.body),
      top: access(() => top.document.body),
      cookie: access(() => document.cookie),
      storage: access(() => localStorage.length),
      api: await get(origin + '/api/agents'),
      declared: await get('https://metrics.example.test/total'),
      undeclared: await get('https://undeclared.example.test/total'),
      library: globalThis.fixtureLibrary,
    };
  }, app.origin), { parent: false, top: false, cookie: false, storage: false, api: false, declared: '139', undeclared: false, library: true });
  assert.equal(await outer.contentFrame().locator('body').evaluate(() => {
    try { return !!parent.document.body; } catch { return false; }
  }), false);

  const frame = {};
  const event = { source: frame, origin: 'null', data: { channel: 'fixture', rpc: { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'lookup' } } } };
  assert.equal(readMcpAppMessage(event, frame, 'fixture').method, 'tools/call');
  for (const invalid of [
    { ...event, source: {} },
    { ...event, origin: app.origin },
    { ...event, data: { ...event.data, channel: 'other' } },
    { ...event, data: { ...event.data, rpc: { ...event.data.rpc, id: [] } } },
    { ...event, data: { ...event.data, rpc: { ...event.data.rpc, params: { name: '你'.repeat(22_000) } } } },
  ]) assert.equal(readMcpAppMessage(invalid, frame, 'fixture'), undefined);
  assert.equal(appTool({ name: 'lookup', arguments: [] }), undefined);
  assert.equal(appContext({ structuredContent: { text: '你'.repeat(1400) } }), undefined);
  assert.equal(appLink('javascript:void(0)'), undefined);
  await inner.locator('body').evaluate(() => {
    const { hash } = new URL(document.referrer || 'https://fixture.invalid');
    top.postMessage({ rpc: { jsonrpc: '2.0', id: 99, method: 'tools/call', params: { name: 'lookup' } }, channel: hash }, '*');
    parent.postMessage({ jsonrpc: '2.0', method: 'ui/notifications/sandbox-closed' }, '*');
  });

  await page.waitForFunction(title => Number(document.querySelector(`iframe[title="${title}"]`).height) !== 240, title);
  const before = Number(await outer.getAttribute('height'));
  await inner.getByText('View inventory notes').click();
  await page.waitForFunction(({ title, before }) => Number(document.querySelector(`iframe[title="${title}"]`).height) > before, { title, before });
  await inner.getByText('View inventory notes').click();
  await page.waitForFunction(({ title, before }) => Number(document.querySelector(`iframe[title="${title}"]`).height) === before, { title, before });
  for (const mode of ['dark', 'light']) {
    await page.evaluate(mode => { document.documentElement.dataset.theme = mode; }, mode);
    await inner.locator(`html[data-theme="${mode}"]`).waitFor();
    await outer.scrollIntoViewIfNeeded();
    await page.screenshot({ path: join(work, `mcp-app-${mode}.png`), animations: 'disabled' });
  }
  await inner.getByRole('button', { name: 'Refresh totals' }).click();
  await inner.getByText('139 shipments').waitFor();
  assert.equal(effects.lookup, 1);
  assert.equal(await page.getByRole('alertdialog').count(), 0);

  const dialog = page.getByRole('alertdialog');
  const save = inner.getByRole('button', { name: 'Save selection' });
  await save.press('Enter');
  await dialog.getByText('Allow Warehouse to run save?').waitFor();
  assert.equal(effects.save, 0);
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await inner.getByText('Request declined.').waitFor();
  assert.equal(effects.save, 0);
  await save.click();
  await dialog.getByRole('button', { name: 'Run tool' }).click();
  await inner.getByText('Saved week 3').waitFor();
  assert.equal(effects.save, 1);

  const intended = { name: 'save', arguments: { week: 2 } };
  const ticket = await api(route + '/call', intended);
  assert.equal(ticket.status, 'approval');
  await api(route + '/call', { ...intended, arguments: { week: 1 }, approval: ticket.id }, 403);
  const approval = { ...intended, approval: ticket.id };
  const results = await Promise.all([1, 2].map(() => fetch(app.origin + route + '/call', {
    method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(approval),
  })));
  assert.deepEqual(results.map(result => result.status).sort(), [200, 403]);
  assert.equal(effects.save, 2);

  await inner.getByRole('button', { name: 'Attach context' }).click();
  await page.getByText('Reply context attached').waitFor();
  const composer = page.getByRole('textbox', { name: 'Message Warehouse' });
  await composer.fill('Keep this draft.');
  await inner.getByRole('button', { name: 'Ask agent' }).click();
  await dialog.getByText('Compare all three weeks.').waitFor();
  assert.equal((await api(path)).length, 1);
  await dialog.getByRole('button', { name: 'Send message' }).click();
  await page.locator('p').filter({ hasText: 'Comparison complete.' }).waitFor();
  assert(followup.includes('HTML reply context (untrusted reference data):') && followup.includes('"week":3'));
  assert.equal(await composer.inputValue(), 'Keep this draft.');
  assert.equal((await api(path)).length, 2);

  const [popup] = await Promise.all([
    page.waitForEvent('popup'),
    inner.getByRole('button', { name: 'Inventory guide' }).click(),
  ]);
  await popup.waitForURL('https://guide.example.test/inventory');
  assert.equal(await popup.evaluate(() => window.opener), null);
  await popup.close();
  assert.equal(page.url(), app.origin + history);

  await page.reload();
  await inner.locator('body[data-total="139"]').waitFor();
  assert.equal(reads, 1, 'Reload should serve the snapshot');
  const pending = await api(route + '/call', intended);
  await api(grant, { id: randomUUID(), revision: 1, connectionRevision: 1, tools: granted.filter(name => name !== 'save') });
  await api(route + '/call', { ...intended, approval: pending.id }, 403);
  assert.equal(effects.save, 2);

  console.log('PASS MCP snapshot, protocol, CSP, opaque frames, validation, tool grants, single-use approvals, message approval, context, links, sizing, themes, reload');
  console.log(`Screenshots: ${work}`);
} finally {
  await browser?.close();
  await app?.close();
  fixture.closeAllConnections();
  await new Promise(resolve => fixture.close(resolve));
}
