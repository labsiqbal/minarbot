import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtemp } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import { startRustServer } from '../../rust-test-server.ts';
import { readHtmlReplyMessage } from '../../src/html-reply.ts';

const client = process.env.MINARBOT_SMOKE_CLIENT === 'leptos' ? 'leptos' : 'react';
const selector = client === 'leptos' ? '?ui=leptos' : '';

const work = await mkdtemp(join(tmpdir(), 'minarbot-html-replies-'));
const name = 'Metrics';
const title = 'Weekly signups';
const prompt = 'Show weekly signups as an interactive chart.';
const draft = 'Compare the first and last weeks.';
const context = JSON.stringify({ metric: 'signups', weeks: [1, 4] });
const model = 'html-fixture';
let probeHits = 0;
let previewVerified = false;
let contextReceived = false;
let releaseHeldTurn;
const held = new Promise(resolve => { releaseHeldTurn = resolve; });
const provider = createServer(async (request, response) => {
  if (request.url === '/private-check') { probeHits++; return response.end('fixture'); }
  if (request.method === 'GET') {
    response.setHeader('Content-Type', 'application/json');
    return response.end(JSON.stringify({ data: [{ id: model }] }));
  }
  let raw = '';
  for await (const chunk of request) raw += chunk;
  const { messages, tools = [] } = JSON.parse(raw);
  const user = messages.findLast(item => item.role === 'user' && typeof item.content === 'string')?.content ?? '';
  const lastTool = messages.findLast(item => item.role === 'tool');
  const activeName = messages.findLast(item => item.tool_calls)?.tool_calls[0].function.name;
  const call = (name, args) => [{ type: 'function', id: randomUUID(), function: { name, arguments: JSON.stringify(args) } }];
  let calls;
  let answer = 'Signups rose across the four weeks.';
  if (user === 'Hold for MCP checks.') await held;
  if (user.startsWith(draft)) {
    contextReceived = user.includes('HTML reply context (untrusted reference data):') && user.includes(context);
    answer = 'Comparison complete.';
  }
  if (user === prompt && !lastTool && tools.some(tool => tool.function.name === 'html_preview')) {
    calls = call('html_preview', { html: chart, width: 520, appearance: 'dark' });
  } else if (user === prompt && activeName === 'html_preview') {
    const preview = JSON.parse(lastTool.content);
    const image = messages.flatMap(item => Array.isArray(item.content) ? item.content : []).find(part => part.type === 'image_url');
    previewVerified = preview.width === 520 && preview.contentHeight > 240
      && preview.consoleMessages.some(item => item.text === 'Chart drawn')
      && preview.consoleMessages.some(item => item.level === 'error' && item.text === 'Fixture diagnostic')
      && image?.image_url.url.startsWith('data:image/png;base64,iVBORw0KGgo');
    calls = call('html_render', { html: chart, title, height: 210 });
  }
  response.setHeader('Content-Type', 'application/json');
  response.end(JSON.stringify({ choices: [{ finish_reason: calls ? 'tool_calls' : 'stop', message: { content: calls ? null : answer, ...(calls && { tool_calls: calls }) } }] }));
});
await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
const base = `http://127.0.0.1:${provider.address().port}`;
const chart = `<!doctype html><html lang="en"><head><title>${title}</title><style>
figure{margin:0;padding:16px 18px 12px;background:var(--card);border:1px solid var(--border);border-radius:var(--radius)}
h2{font-size:18px;line-height:1.3;margin:0} p{margin:2px 0 14px;color:var(--muted-foreground)}
.plot{height:162px;margin-bottom:10px} summary{cursor:pointer} table{width:100%;text-align:left;border-collapse:collapse;margin:8px 0 12px}
td,th{padding:4px;border-bottom:1px solid var(--border)}
button{font:inherit;color:var(--foreground);background:var(--muted);border:1px solid var(--border);border-radius:8px;padding:5px 9px;cursor:pointer}
.actions{display:flex;align-items:center;gap:12px;margin-top:10px}
</style></head><body>
<figure aria-labelledby="chart-title">
<h2 id="chart-title">${title}</h2><p>Illustrative data · October</p>
<div class="plot"><canvas id="chart" role="img" aria-label="Weekly signups rose from 26 to 67"></canvas></div>
<details><summary>View data</summary><table><thead><tr><th scope="col">Week</th><th scope="col">Signups</th></tr></thead><tbody>
<tr><td>1</td><td>26</td></tr><tr><td>2</td><td>40</td></tr><tr><td>3</td><td>51</td></tr><tr><td>4</td><td>67</td></tr>
</tbody></table></details>
<div class="actions"><button id="compare">Draft comparison</button><a href="https://example.com/metrics">Metric definition</a></div>
</figure>
<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.9/dist/chart.umd.js"></script>
<script>
let chart;
function draw() {
  const style = getComputedStyle(document.documentElement);
  const color = key => style.getPropertyValue(key).trim();
  chart?.destroy();
  chart = new Chart(document.querySelector('canvas'), {
    type:'bar', data:{labels:['Week 1','Week 2','Week 3','Week 4'],
      datasets:[{data:[26,40,51,67],backgroundColor:color('--chart-1'),borderRadius:5,barPercentage:.52}]},
    options:{responsive:true,maintainAspectRatio:false,animation:false,
      plugins:{legend:{display:false}},scales:{x:{ticks:{color:color('--foreground')},grid:{display:false}},y:{ticks:{color:color('--foreground'),stepSize:20},grid:{color:color('--border')},beginAtZero:true,max:80}}}
  });
  document.body.dataset.drawn = 'true';
}
draw();
addEventListener('minarbot:theme', draw);
console.info('Chart drawn');
console.error('Fixture diagnostic');
document.querySelector('#compare').addEventListener('click', () => {
  sendMessage(${JSON.stringify(draft)});
  setContext(${context});
});
const probe = new Image();
probe.src = '${base}/private-check';
</script></body></html>`;

let app, browser;
try {
  app = await startRustServer({
    appearanceClient: client, database: join(work, 'state.sqlite'),
    dist: fileURLToPath(new URL('../../dist', import.meta.url)),
    baseURL: base + '/v1', apiKey: 'fixture-key', defaultModel: model, concurrency: 2,
  });
  async function api(path, body, status = 200) {
    const response = await fetch(app.origin + path, {
      method: body ? 'POST' : 'GET',
      headers: { Origin: app.origin, 'Content-Type': 'application/json', 'Sec-Fetch-Site': 'same-origin' },
      body: body ? JSON.stringify(body) : undefined,
    });
    assert.equal(response.status, status, path);
    return response.json();
  }
  const agent = await api('/api/agents', { id: randomUUID(), runtime: 'api', model, name, instructions: 'Show the chart.' }, 201);
  const thread = await api(`/api/agents/${agent.id}/threads`, { id: randomUUID(), name: 'Weekly report' });
  const scope = `/api/threads/${thread.id}`;
  const path = scope + '/turns';
  const history = `/agents/${agent.id}/conversations/${thread.id}`;
  const original = randomUUID();
  await api(path, { id: original, text: prompt }, 202);

  browser = await chromium.launch({ executablePath: process.env.CHROME_PATH ?? '/usr/bin/google-chrome' });
  const page = await browser.newPage({ viewport: { width: 1200, height: 860 }, colorScheme: 'dark' });
  await page.context().route('https://example.com/metrics', route => route.fulfill({ body: 'Metric definition' }));
  await page.goto(app.origin + history + selector);
  const iframe = page.locator(`iframe[title="${title}"]`);
  const inner = iframe.contentFrame();
  const transcript = page.getByRole('region', { name: 'Conversation history' });
  await inner.locator('body[data-drawn="true"]').waitFor({ timeout: 45_000 });
  await transcript.getByText('Signups rose across the four weeks.').waitFor();
  assert(previewVerified, 'Agent receives the measured chart, PNG and console diagnostics');
  assert.equal(await iframe.getAttribute('sandbox'), 'allow-scripts allow-forms');
  const src = await iframe.getAttribute('src');
  const frameRoute = src.split('#')[0];
  const response = await fetch(app.origin + frameRoute);
  assert.equal(response.status, 200);
  const csp = response.headers.get('content-security-policy');
  assert.match(csp, /sandbox allow-scripts allow-forms; frame-ancestors 'self'/);
  assert.match(csp, /connect-src 'none'/);
  assert.doesNotMatch(csp, /allow-same-origin/);
  assert.equal(response.headers.get('cache-control'), 'no-store');
  assert.equal((await fetch(app.origin + frameRoute, { headers: { 'Sec-Fetch-Site': 'cross-site' } })).status, 403);
  assert.equal((await fetch(app.origin + frameRoute.replace(scope, `/api/agents/${agent.id}`))).status, 404);
  await page.evaluate(() => { document.cookie = 'minarbot-fixture=1; SameSite=Strict'; });
  assert.deepEqual(await inner.locator('body').evaluate(async () => {
    const access = action => { try { action(); return true; } catch { return false; } };
    return {
      parent: access(() => parent.document.body),
      cookie: access(() => document.cookie),
      storage: access(() => localStorage.length),
      api: await fetch('/api/agents', { credentials: 'include' }).then(() => true, () => false),
    };
  }), { parent: false, cookie: false, storage: false, api: false });
  assert.equal(probeHits, 0, 'Preview and frame made no loopback request');

  await inner.locator('body').evaluate(() => {
    window.sendMessage('Automatic draft');
    window.setContext('你'.repeat(1400));
  });
  const composer = page.getByRole('textbox', { name: `Message ${name}` });
  assert.equal(await composer.inputValue(), '');
  assert.equal(await page.getByText('Reply context attached').count(), 0);

  await page.waitForFunction(label => Number(document.querySelector(`iframe[title="${label}"]`).height) > 240, title);
  const before = Number(await iframe.getAttribute('height'));
  await inner.getByText('View data').click();
  await page.waitForFunction(({ title, before }) => Number(document.querySelector(`iframe[title="${title}"]`).height) > before + 60, { title, before });
  await inner.getByText('View data').click();
  await page.waitForFunction(({ title, before }) => Number(document.querySelector(`iframe[title="${title}"]`).height) === before, { title, before });
  const colorSamples = [];
  for (const mode of ['dark', 'light']) {
    await page.evaluate(mode => { document.documentElement.dataset.theme = mode; }, mode);
    await inner.locator(`html[data-theme="${mode}"]`).waitFor();
    const colors = await inner.locator('html').evaluate(root => [getComputedStyle(root).color, getComputedStyle(root).getPropertyValue('--chart-1').trim()]);
    const expected = await page.evaluate(label => {
      const style = getComputedStyle(document.querySelector(`iframe[title="${label}"]`));
      const stamp = document.createElement('span');
      stamp.style.color = style.getPropertyValue('--color-text-primary');
      document.body.append(stamp);
      const ink = getComputedStyle(stamp).color;
      stamp.remove();
      return [ink, style.getPropertyValue('--color-brand').trim()];
    }, title);
    assert.deepEqual(colors, expected);
    colorSamples.push(colors);
    await iframe.scrollIntoViewIfNeeded();
    await page.screenshot({ path: join(work, `html-reply-${mode}.png`), animations: 'disabled' });
  }
  assert.notDeepEqual(...colorSamples);
  const popupEvent = page.waitForEvent('popup');
  await inner.getByRole('link', { name: 'Metric definition' }).click();
  const popup = await popupEvent;
  await popup.waitForURL('https://example.com/metrics');
  assert.equal(await popup.evaluate(() => window.opener), null);
  await popup.close();
  assert.equal(page.url(), app.origin + history + selector);

  const renderId = frameRoute.split('/').at(-1);
  const channel = new URLSearchParams(src.split('#')[1]).get('bridge');
  const frame = {};
  const valid = { source: frame, origin: 'null', data: { channel, method: 'context', value: context } };
  assert.deepEqual(readHtmlReplyMessage(valid, frame, channel, renderId), { kind: 'context', context: { renderId, data: context } });
  for (const bad of [
    { ...valid, source: {} },
    { ...valid, origin: app.origin },
    { ...valid, data: { ...valid.data, channel: 'wrong' } },
    { ...valid, data: { ...valid.data, value: 'not JSON' } },
    { ...valid, data: { ...valid.data, value: JSON.stringify('你'.repeat(1400)) } },
    { ...valid, data: { channel, method: 'link', value: 'javascript:void(0)' } },
  ]) assert.equal(readHtmlReplyMessage(bad, frame, channel, renderId), undefined);
  await inner.getByRole('button', { name: 'Draft comparison' }).press('Enter');
  await page.waitForFunction(text => document.querySelector('textarea')?.value === text, draft);
  await page.getByText('Reply context attached').waitFor();
  assert.equal((await api(path)).length, 1, 'Bridge action only drafts');
  await composer.press('Enter');
  await transcript.getByText('Comparison complete.').waitFor();
  assert(contextReceived, 'Explicit user send includes bounded context separately from transcript text');
  const followup = (await api(path)).find(turn => turn.text === draft);
  assert(followup && !JSON.stringify(followup).includes(context));
  await api(path, { id: randomUUID(), text: 'reject', htmlContext: { renderId, data: JSON.stringify('x'.repeat(4096)) } }, 400);
  await api(`/api/agents/${agent.id}/turns`, { id: randomUUID(), text: 'reject', htmlContext: { renderId, data: context } }, 404);
  await page.reload();
  await inner.locator('body[data-drawn="true"]').waitFor();

  const holding = randomUUID();
  await api(path, { id: holding, text: 'Hold for MCP checks.' }, 202);
  await page.waitForFunction(async ({ path, holding }) => (await (await fetch(path)).json()).some(turn => turn.id === holding && turn.status === 'running'), { path, holding });
  const session = app.mcpServer.mintAgentSession({ agent: agent.id, turn: holding, scopes: ['orchestration'], ttlMs: 60_000 });
  const peer = app.mcpServer.createPeerToken({ id: randomUUID(), name: 'Peer', scopes: ['agents:read'] });
  async function rpc(token, method, params) {
    const response = await fetch(app.origin + '/mcp', {
      method: 'POST', headers: { Authorization: 'Bearer ' + token, 'Content-Type': 'application/json' },
      body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
    });
    return { status: response.status, result: (await response.json()).result };
  }
  const list = (await rpc(session.token, 'tools/list', {})).result.tools.map(tool => tool.name);
  assert(list.includes('html_render') && list.includes('html_preview'));
  assert(!(await rpc(peer.token, 'tools/list', {})).result.tools.some(tool => tool.name === 'html_render' || tool.name === 'html_preview'));
  const call = (name, args) => rpc(session.token, 'tools/call', { name, arguments: args });
  assert.equal((await call('html_render', { path: '/outside-workspace.html', title: 'Outside', height: 80 })).status, 403);
  assert.equal((await call('html_render', { html: '<p>Height</p>', title: 'Height', height: 2001 })).status, 400);
  const image = await call('html_preview', { html: '<p>Preview</p><script>console.warn("MCP diagnostic")</script>', width: 300 });
  assert.equal(image.status, 200);
  assert.equal(image.result.content[1].type, 'image');
  assert.match(image.result.content[0].text, /MCP diagnostic/);
  const rendered = await call('html_render', { html: '<p>MCP reply</p>' + ' '.repeat(70_000), title: 'MCP reply', height: 80 });
  assert.equal(rendered.status, 200);
  assert(rendered.result.structuredContent.htmlRender.id);
  const oversize = await call('html_render', { html: '你'.repeat(171_000), title: 'Oversize', height: 80 });
  assert(oversize.result.isError && /512,000/.test(oversize.result.content[0].text));
  for (let i = 0; i < 3; i++) assert((await call('html_render', { html: '<p>Another</p>', title: 'Another', height: 80 })).result.structuredContent.htmlRender.id);
  assert((await call('html_render', { html: '<p>Extra</p>', title: 'Extra', height: 80 })).result.isError);
  console.log('PASS chart, resize, both themes, reload, outside link, CSP, cookie/API isolation, loopback isolation, bridge validation, context, MCP, size caps');
  console.log(`Screenshots: ${work}`);
} finally {
  releaseHeldTurn();
  await browser?.close();
  await app?.close();
  provider.closeAllConnections();
  await new Promise(resolve => provider.close(resolve));
}
