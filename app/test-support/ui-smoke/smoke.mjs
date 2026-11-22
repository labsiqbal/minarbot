import { instantMenusSmoke } from './instant-menus-smoke.mjs';
import { selfVerifyReply, selfVerifySmoke } from './self-verify-smoke.mjs';
import { providersHiddenSmoke } from './providers-hidden-smoke.mjs';
import { fileOutputPanelSmoke, fileOutputReply } from './file-output-panel-smoke.mjs';
import { acpRuntimeSmoke } from './acp-runtime-smoke.mjs';
import { r5PanelSmoke } from './r5-panel-smoke.mjs';
import { r4SettingsSmoke } from './r4-settings-smoke.mjs';
import assert from 'node:assert/strict';
import { polishSmoke } from './polish-smoke.mjs';
import { copySettingsSmoke } from './copy-settings-smoke.mjs';
import { whatsappSmoke } from './whatsapp-smoke.mjs';
import { rightPanelUXSmoke } from './right-panel-ux-smoke.mjs';
import { deleteAgentSmoke } from './delete-agent-smoke.mjs';
import { typographySmoke } from './typography-smoke.mjs';
import { detailsDeclutterSmoke } from './details-declutter-smoke.mjs';
import { sidebarSpacingSmoke } from './sidebar-spacing-smoke.mjs';
import { attachmentSmoke } from './attachment-smoke.mjs';
import { widePanelsSmoke } from './rs1-smoke.mjs';
import { desktopImage, installVncFixture } from './computer-smoke.mjs';
import { desktopViewSmoke } from './desktop-view-smoke.mjs';
import { issue1Smoke } from './issue1-smoke.mjs';
import { roomControlsSmoke } from './room-controls-smoke.mjs';
import { runControlsSmoke } from './run-controls-smoke.mjs';
import { streamActionsSmoke } from './stream-actions-smoke.mjs';
import { timelineMinimapSmoke } from './timeline-minimap-smoke.mjs';
import { spawn, execFileSync } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { access, mkdir, mkdtemp, open, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const require = createRequire(import.meta.url);
const output = await mkdtemp(join(root, 'app/node_modules/.ui-smoke-'));
const gitAgent = randomUUID(), gitName = 'Source Control Fixture';
let gitWorkspace;
const started = Date.now();
const reply = 'Smoke reply: **ready**.\n\n- Local provider completed the turn.';
const models = ['smoke-primary', 'smoke-secondary'];
const quiet = 'Smoke Reviewer';
const lead = 'Smoke Lead', helper = 'Smoke Helper', projectName = 'Smoke Channel', channel = 'Smoke Channel / general', skill = 'smoke-skill';
const longName = 'Iris Editorial and Knowledge Management with a Deliberately Long Agent Name';
const pendingMessage = 'Hold this reply for the pending header layout check';
const receipt = {
  startedAt: new Date(started).toISOString(), output, viewport: { width: 1440, height: 900 },
  versions: { node: process.version, playwright: require('playwright-core/package.json').version },
  checks: [], consoleErrors: [], pageErrors: [], failedRequests: [], providerRequests: [],
  screenshots: [], timings: {}, cleanupErrors: [],
  roomDiscussion: [],
  cspViolations: [], nativeSelectViolations: [], graphLayouts: [], headerLayouts: [],
};
let provider, mcpFixture, app, browser, browserScratch, page, data, origin, appLog = '', activeCheck = 'startup';
let workflow, releasePending;
const pendingReply = new Promise(resolve => { releasePending = resolve; });
const hrcGates = new Map();
let releaseStream, releaseRoom, releaseRoomDelivery;
const streamGate = new Promise(resolve => { releaseStream = resolve; });
const roomGate = new Promise(resolve => { releaseRoom = resolve; });
const roomDeliveryGate = new Promise(resolve => { releaseRoomDelivery = resolve; });
const streamReply = 'Stable first block.\n\n' + Array.from({ length: 30 }, (_, i) => `Paragraph ${i + 1}: streaming text stays in place.\n\n`).join('') + 'Final streamed answer.';

let interrupted;
for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => {
  interrupted = signal;
  app?.kill('SIGTERM');
  void browser?.close().catch(() => {});
});

async function listen(server) {
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
  return server.address().port;
}

function fakeProvider(req, res) {
  // Long waits between checks can overlap the fixture's idle socket close.
  res.setHeader('Connection', 'close');
  void (async () => {
    const chunks = [];
    for await (const chunk of req) {
      chunks.push(chunk);
      assert(chunks.reduce((size, value) => size + value.length, 0) <= 2 * 1024 * 1024, 'Provider request too large');
    }
    const body = chunks.length ? JSON.parse(Buffer.concat(chunks).toString()) : {};
    receipt.providerRequests.push({ method: req.method, path: req.url, model: body.model, stream: !!body.stream, tools: body.tools?.length ?? 0, atMs: Date.now() - started });
    const json = (status, value) => { res.writeHead(status, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(value)); };
    if (req.method === 'GET' && req.url === '/v1/models') return json(200, { object: 'list', data: models.map(id => ({ id, object: 'model', owned_by: 'ui-smoke' })) });
    if (req.method !== 'POST' || req.url !== '/v1/chat/completions') return json(404, { error: { message: 'Unknown fake-provider route' } });
    // Hold only this synthetic turn; ordinary chat and workflow replies still complete.
    if (JSON.stringify(body.messages).includes(pendingMessage)) await pendingReply;
    const latestUser = [...(body.messages ?? [])].reverse().find(message => message.role === 'user')?.content;
    const fileReply = selfVerifyReply(body) ?? await fileOutputReply(body); if (fileReply) return json(200, fileReply);
    if (latestUser === 'HRA ordered room') {
      const system = body.messages[0].content, ownId = system.match(/Your member ID: ([^.]+)\./)?.[1];
      const name = system.match(new RegExp(`"id":"${ownId}","name":"([^"]+)`))?.[1];
      res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache' });
      const emit = (delta, finish_reason = null) => res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta, finish_reason }] })}\n\n`);
      const finish = reason => { emit({}, reason); res.end('data: [DONE]\n\n'); };
      if (name === 'ST1 Reviewer') { await roomDeliveryGate; emit({ content: 'NO_REPLY' }); finish('stop'); return; }
      if (!body.messages.some(message => message.role === 'tool')) {
        emit({ content: `Before tool for ${name}.` }); await delay(name === 'ST1 Helper' ? 80 : 45);
        emit({ tool_calls: [{ index: 0, id: 'hra-file', type: 'function', function: { name: 'create_text_file', arguments: JSON.stringify({ name: 'hra.txt', text: 'Exactly once' }) } }] }); finish('tool_calls'); return;
      }
      emit({ content: `Answer from ${name}: ` }); await delay(name === 'ST1 Helper' ? 130 : 70); emit({ content: 'growing' });
      await roomDeliveryGate; emit({ content: '. Complete.' }); finish('stop'); return;
    }
    if (body.stream && typeof latestUser === 'string' && latestUser.includes('HRC APPROVE')) {
      res.writeHead(200, { 'Content-Type': 'text/event-stream' });
      res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { content: 'Room control preview.\n\n' }, finish_reason: null }] })}\n\n`);
      await new Promise(resolve => hrcGates.set(body.model, resolve));
      const call = { index: 0, id: randomUUID(), type: 'function', function: { name: 'read_provider_settings', arguments: '{}' } };
      res.end(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { tool_calls: [call] }, finish_reason: 'tool_calls' }] })}\n\ndata: [DONE]\n\n`); return;
    }
    if (body.stream && ['ST1 progressive stream', 'ST1 room stream'].includes(latestUser)) {
      res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache' });
      const emit = text => res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { content: text }, finish_reason: null }] })}\n\n`);
      const finish = () => res.end(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })}\n\ndata: [DONE]\n\n`);
      emit('Stable first block.\n\n'); await delay(150); emit('Growing second block');
      if (latestUser === 'ST1 room stream') { await roomGate; emit('. Room stream finished.'); finish(); return; }
      await streamGate;
      // Extend into a long transcript so follow and free-scroll both get exercised.
      const rest = streamReply.slice('Stable first block.\n\n'.length);
      // Replace the synthetic incomplete second block with its final content by continuing it.
      for (const part of rest.match(/.{1,35}(?:\n|$)|[\s\S]{1,35}/g) ?? []) { emit(part); await delay(45); }
      finish(); return;
    }
    if (latestUser === 'R3C room stop') {
      res.writeHead(200, { 'Content-Type': 'text/event-stream' });
      res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { content: 'Synthetic polish room reply.' }, finish_reason: null }] })}\n\n`);
      return;
    }
    if (latestUser === 'UXA held stream') {
      res.writeHead(200, { 'Content-Type': 'text/event-stream' });
      res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { content: 'Synthetic active reply.' }, finish_reason: null }] })}\n\n`);
      res.once('close', () => {}); return;
    }
    if (latestUser === 'UXA fail') return json(400, { error: { message: 'UXA synthetic failure' } });
    if (latestUser === 'ST1 failed reply') return json(400, { error: { message: 'Synthetic failure' } });
    if (latestUser === 'ST1 fallback reply' && body.model === models[0]) return json(503, { error: { message: 'Synthetic unavailable' } });
    if (latestUser === 'R3C tool output' && !body.messages.some(message => message.role === 'tool')) {
      const call = { id: randomUUID(), type: 'function', function: { name: 'create_text_file', arguments: JSON.stringify({ name: 'notes.txt', text: 'Synthetic polish notes' }) } };
      if (!body.stream) return json(200, { choices: [{ finish_reason: 'tool_calls', message: { tool_calls: [call] } }] });
      res.writeHead(200, { 'Content-Type': 'text/event-stream' });
      res.end(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { tool_calls: [{ index: 0, ...call }] }, finish_reason: 'tool_calls' }] })}\n\ndata: [DONE]\n\n`); return;
    }
    const childDelivery = body.messages?.[0]?.content?.includes('Message from teammate');
    if (latestUser === 'Delegated smoke' && !childDelivery && !body.messages.some(message => message.role === 'tool') && body.tools?.some(tool => tool.function?.name === 'delegate_task')) {
      return json(200, { choices: [{ finish_reason: 'tool_calls', message: { tool_calls: [{ id: 'smoke-delegate', type: 'function', function: { name: 'delegate_task', arguments: JSON.stringify({ agent: helper, task: 'Delegated smoke', wait: true }) } }] } }] });
    }
    if (latestUser === 'Delegated smoke' && childDelivery) {
      return json(200, { choices: [{ finish_reason: 'stop', message: { content: 'Child smoke result' } }] });
    }
    const system = body.messages?.[0]?.content ?? '';
    const ownId = system.match(/Your member ID: ([^.]+)\./)?.[1];
    const ownName = ownId && system.match(new RegExp(`\\"id\\":\\"${ownId}\\",\\"name\\":\\"([^\\"]+)`))?.[1];
    if (system.includes('Group chat.') && system.includes('Original human brief: "Room discussion smoke"\n')) {
      const invitation = '@Smoke Helper, please check the plan.';
      const delta = system.match(/Committed room replies after watermark [^\n]*\n([\s\S]*?)\nGroup chat\./)?.[1] ?? '';
      const invited = delta.split('\n').some(line => line.includes(`"name":"${lead}"`) && line.endsWith(`: ${JSON.stringify(invitation)}`));
      const content = ownName === lead ? invitation : ownName === helper && invited ? 'Plan checked. Ready for the next step.' : 'NO_REPLY';
      receipt.roomDiscussion.push({ agent: ownName, invited, latestUser, content });
      return json(200, { choices: [{ finish_reason: 'stop', message: { content } }] });
    }
    await delay(300);
    const base = { id: 'chatcmpl-smoke', created: Math.floor(Date.now() / 1000), model: body.model ?? models[0] };
    const usage = { prompt_tokens: 12, completion_tokens: 10, total_tokens: 22 };
    // Accept offered tools without calling them: a final text answer is valid.
    if (!body.stream) return json(200, { ...base, object: 'chat.completion', choices: [{ index: 0, message: { role: 'assistant', content: reply }, finish_reason: 'stop' }], usage });
    res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache' });
    for (const chunk of [
      { choices: [{ index: 0, delta: { role: 'assistant', content: reply }, finish_reason: null }] },
      { choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] },
      { choices: [], usage },
    ]) res.write(`data: ${JSON.stringify({ ...base, object: 'chat.completion.chunk', ...chunk })}\n\n`);
    res.end('data: [DONE]\n\n');
  })().catch(error => {
    receipt.providerRequests.push({ error: error.message, atMs: Date.now() - started });
    if (!res.headersSent) res.writeHead(400, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ error: { message: error.message } }));
  });
}

async function screenshot(label, target = page) {
  if (!target || target.isClosed()) return null;
  const path = join(output, `${String(receipt.screenshots.length + 1).padStart(2, '0')}-${label.replace(/[^a-z0-9]+/gi, '-').toLowerCase()}.png`);
  await target.screenshot({ path, fullPage: true, animations: 'disabled', timeout: 10_000, mask: [target.locator('input[type=password], [data-private-token]')] });
  receipt.screenshots.push(path);
  return path;
}

async function captureComputerThemes(label) {
  for (const [palette, theme] of [['minarbot', 'dark'], ['minarbot', 'light'], ['catppuccin', 'dark']]) {
    await page.evaluate(({ palette, theme }) => { document.documentElement.dataset.colorTheme = palette; document.documentElement.dataset.theme = theme; }, { palette, theme });
    await screenshot(`${label}-${palette}-${theme}`);
  }
  await page.evaluate(() => { document.documentElement.dataset.colorTheme = 'minarbot'; document.documentElement.dataset.theme = 'dark'; });
}

async function check(name, run) {
  if (interrupted) throw new Error(`Interrupted by ${interrupted}`);
  activeCheck = name;
  const start = Date.now();
  const item = { name, status: 'pass', message: '', screenshot: null, startedAt: new Date(start).toISOString() };
  try { item.message = await run() ?? 'Expected UI state confirmed.'; }
  catch (error) { item.status = 'fail'; item.message = error.stack ?? String(error); }
  try { item.screenshot = await screenshot(name); }
  catch (error) { item.status = 'fail'; item.message += `\nScreenshot failed: ${error.message}`; }
  try { await page.evaluate(() => window.__smokeGuardFlush()); }
  catch (error) { item.status = 'fail'; item.message += `\nPage guard failed: ${error.message}`; }
  const violations = [...receipt.cspViolations, ...receipt.nativeSelectViolations].filter(event => event.check === name);
  if (violations.length) { item.status = 'fail'; item.message += `\nPage guard: ${JSON.stringify(violations)}`; }
  item.durationMs = Date.now() - start;
  receipt.checks.push(item);
  console.log(`${item.status.toUpperCase()} ${name}${item.status === 'fail' ? `\n${item.message}` : ''}`);
  if (process.env.MINARBOT_SMOKE_SET3_ONLY === '1' && item.status === 'fail') throw new Error('SMOKE_STOP_AFTER');
  if (process.env.MINARBOT_SMOKE_STOP_AFTER === name) throw new Error('SMOKE_STOP_AFTER');
}

async function settingsDataSmoke() {
  const json = (route, value) => route.fulfill({ contentType: 'application/json', body: JSON.stringify(value) });
  const broken = route => route.fulfill({ contentType: 'application/json', body: '{' });
  const close = async dialog => { await dialog.getByRole('button', { name: 'Close', exact: true }).first().click(); await hidden(dialog); };
  const setup = async name => { const panel = await settings('Machines'); await panel.getByRole('button', { name: 'Add machine', exact: true }).click(); await page.getByRole('menuitem', { name, exact: true }).click(); const dialog = page.getByRole('dialog', { name, exact: true }); await visible(dialog); return dialog; };
  async function contain(scope) {
    assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), 'Document overflow');
    assert(await scope.evaluate(el => el.scrollWidth <= el.clientWidth + 1), 'Content overflow');
    assert(await scope.locator('.k-btn, .k-icon-btn').evaluateAll(els => els.filter(el => el.getClientRects().length).every(el => { return el.offsetWidth >= 40 && el.offsetHeight >= (el.closest('.app-settings__page .k-page-head') ? 32 : 40); })), 'Action below 32px in page header or 40px elsewhere');
    assert(await scope.locator('.k-table-wrap').evaluateAll(els => els.every(el => el.getBoundingClientRect().right <= innerWidth + 1 && el.scrollWidth >= el.clientWidth)), 'Table overflow escaped wrapper');
  }
  async function matrix(label, scope) {
    for (const viewport of [{ width: 1440, height: 900 }, { width: 1024, height: 768 }]) {
      await page.setViewportSize(viewport);
      for (const [palette, theme] of [['minarbot', 'dark'], ['minarbot', 'light'], ['catppuccin', 'dark']]) {
        await page.evaluate(({ palette, theme }) => { document.documentElement.dataset.colorTheme = palette; document.documentElement.dataset.theme = theme; }, { palette, theme });
        await scope.evaluate(el => { if (el.matches('.k-dialog')) el.querySelector('.k-dialog__body').scrollTop=0; if (el.matches('.set4-page')) el.closest('.app-settings__main').scrollTop=0; });
        if(label==='transcribe')await scope.scrollIntoViewIfNeeded();
        if(label==='install-minarbot'){await page.keyboard.press('Tab');await scope.getByText('Linux and macOS requirements',{exact:true}).focus();const tooltip=page.getByRole('tooltip').filter({hasText:'Requires curl, Python 3, and OpenSSL 3.'});await visible(tooltip);assert(await tooltip.evaluate(el=>el.getBoundingClientRect().width<=340),'Tooltip exceeds readable width');}
        await contain(scope); await screenshot(`set4-${label}-${palette}-${theme}-${viewport.width}`);
      }
    }
    await page.setViewportSize(receipt.viewport); await page.evaluate(() => { document.documentElement.dataset.colorTheme = 'minarbot'; document.documentElement.dataset.theme = 'dark'; });
  }
  await check('SET4 Machines list, directional switches and setup dialogs', async () => {
    const panel = await settings('Machines');
    await visible(panel.getByRole('button', { name: 'Open Studio workstation', exact: true }));
    assert.deepEqual(await panel.locator('.set4-page').getByRole('heading', { level: 2 }).allTextContents(), ['Paired machines', 'This device']);
    await hidden(panel.getByRole('textbox'));
    await matrix('machines', panel.locator('.set4-page'));
    const requests=[]; const record=req=>{if(req.method()==='POST' && /\/api\/machines\/(agent-messaging|updates)$/.test(new URL(req.url()).pathname)) requests.push({path:new URL(req.url()).pathname,body:req.postDataJSON()});};page.on('request',record);
    try {
      await panel.getByRole('button', { name: 'Open Studio workstation', exact: true }).click();
      const detail=page.getByRole('dialog',{name:'Studio workstation',exact:true});await visible(detail);
      for(const label of ['Incoming messages','Remote updates']) { const control=detail.getByRole('switch',{name:label,exact:true});const before=await control.isChecked();await control.click();await page.waitForFunction(({label,before})=>document.querySelector(`[role="switch"][aria-label="${label}"]`)?.getAttribute('aria-checked')===String(!before),{label,before});await control.click();await page.waitForFunction(({label,before})=>document.querySelector(`[role="switch"][aria-label="${label}"]`)?.getAttribute('aria-checked')===String(before),{label,before}); }
      assert.equal(requests.length,4);for(const req of requests){assert.deepEqual(Object.keys(req.body).sort(),['allowed','environmentId','id']);assert.match(req.body.id,/^[0-9a-f-]{36}$/);}
      await matrix('machine-detail',detail);assert.equal(await detail.getByRole('button').last().innerText(),'Unpair machine');await close(detail);
    } finally {page.off('request',record);}
    for(const name of ['Install Minarbot','Pair machine','Show pairing code']) {const dialog=await setup(name);assert.equal(await page.locator('.k-dialog[role="dialog"]').count(),1);await matrix(name.toLowerCase().replaceAll(' ','-'),dialog);await close(dialog);await focusRestored(panel.getByRole('button',{name:'Add machine',exact:true}));}
    await cleanPage(panel);
  });
  await check('SET4 Pairing code generates on request, copies, expires and clears',async()=>{
    await page.context().grantPermissions(['clipboard-read','clipboard-write'],{origin});let created=0;const record=req=>{if(req.method()==='POST'&&new URL(req.url()).pathname==='/api/pairing-codes')created++;};page.on('request',record);
    try{let dialog=await setup('Show pairing code');assert.equal(created,0);await dialog.getByRole('button',{name:'Create code',exact:true}).click();const region=dialog.getByRole('region',{name:'One-time pairing code',exact:true});await visible(region);assert.equal(created,1);const code=await region.locator('.machines-code__value').innerText();await region.getByRole('button',{name:'Copy pairing code',exact:true}).click();await visible(region.getByRole('button',{name:'Copied',exact:true}));assert.equal(await page.evaluate(()=>navigator.clipboard.readText()),code);await page.evaluate(()=>{window.__set4Now=Date.now;Date.now=()=>window.__set4Now()+11*60_000;});try{await hidden(region);await visible(dialog.getByRole('button',{name:'Create code',exact:true}));}finally{await page.evaluate(()=>{Date.now=window.__set4Now;delete window.__set4Now;});}await close(dialog);dialog=await setup('Show pairing code');await hidden(dialog.getByRole('region'));assert.equal(created,1);await close(dialog);}finally{page.off('request',record);}
  });
  await check('SET4 Pair machine preserves exact retry and input limits',async()=>{
    const commands=[];let fail=true;const route='**/api/machines';await page.route(route,r=>{if(r.request().method()!=='POST')return r.continue();commands.push(r.request().postDataJSON());return fail?broken(r):json(r,{});});
    try {const dialog=await setup('Pair machine');const address=dialog.getByLabel('Address',{exact:true}),code=dialog.getByLabel('Pairing code',{exact:true});assert.equal(await address.getAttribute('maxlength'),'2048');assert.equal(await code.getAttribute('minlength'),'12');assert.equal(await code.getAttribute('maxlength'),'12');await address.fill('https://fixture.example.test:4317');await code.fill('abcdefghjkmn');await dialog.getByRole('button',{name:'Pair machine',exact:true}).click();await visible(dialog.getByRole('alert'));fail=false;await dialog.getByRole('button',{name:'Pair machine',exact:true}).click();await hidden(dialog);assert.deepEqual(commands[0],commands[1]);assert.deepEqual(Object.keys(commands[0]).sort(),['code','id','origin']);assert.equal(commands[0].code,'ABCDEFGHJKMN');}
    finally{await page.unroute(route);await workspace();}
  });
  await check('SET4 This device identity, metric groups, unknowns and low disk',async()=>{
    const metrics=await fetch(origin+'/api/host-metrics').then(r=>r.json());metrics.cpu.percent=null;metrics.cpu.intervalMs=null;metrics.memory.available=null;metrics.filesystem={...metrics.filesystem,pressure:'critical'};
    const route='**/api/host-metrics';await page.route(route,r=>json(r,metrics));
    try {const panel=await settings('Machines');await panel.getByRole('button',{name:'Resources',exact:true}).click();const detail=page.getByRole('dialog',{name:'This device',exact:true});await visible(detail.getByLabel('Name',{exact:true}));await visible(detail.getByText(/Storage space is critically low/));await visible(detail.getByText('Waiting for two readings',{exact:true}).first());
      for(const group of ['Memory','Storage','Database'])await detail.getByRole('button',{name:group,exact:true}).click();
      await visible(detail.getByText(/^Unavailable of/));await matrix('this-computer',detail);
      const before=await detail.getByLabel('Name',{exact:true}).inputValue();const response=page.waitForRequest(req=>req.method()==='POST'&&new URL(req.url()).pathname==='/api/environment');await detail.getByLabel('Name',{exact:true}).fill('Fixture computer');await detail.getByRole('button',{name:'Save name',exact:true}).click();const request=await response;assert.deepEqual(Object.keys(request.postDataJSON()).sort(),['id','name']);await page.waitForFunction(()=>document.querySelector('button[aria-busy]')===null);
      await detail.getByLabel('Name',{exact:true}).fill(before);await detail.getByRole('button',{name:'Save name',exact:true}).click();await close(detail);
    }finally{await page.unroute(route);}
  });
  await check('SET4 Machines empty, error and reload recovery',async()=>{
    let fail=false;const route='**/api/machines';await page.route(route,r=>fail?broken(r):json(r,{machines:[]}));
    try {await workspace();let panel=await settings('Machines');await visible(panel.getByText('No paired machines',{exact:true}));await screenshot('set4-machines-empty');fail=true;await page.evaluate(() => window.dispatchEvent(new Event('focus')));await visible(panel.getByRole('alert'));await screenshot('set4-machines-error');fail=false;await page.evaluate(() => window.dispatchEvent(new Event('focus')));await hidden(panel.getByRole('alert'));}
    finally{await page.unroute(route);await workspace();}
  });
  await check('SET4 Usage tabs, paged history, unknowns and containment',async()=>{
    const summary={calls:51,pending:1,failed:1,interrupted:1,input_tokens:'9007199254740993',output_tokens:'0',total_tokens:null,unknown_input:1,unknown_output:2,unknown_total:3};const transcription={calls:1,pending:0,uncertain:1,cancelled:0,unknown:1,input_tokens:null,output_tokens:null,total_tokens:null,seconds:null};
    const calls=Array.from({length:3},(_,i)=>({seq:i+2,agent_name:'Synthetic reporting agent',model:'fixture-model',origin:'http://127.0.0.1:1234/v1',family:'openai-compatible',purpose:'completion',phase:'answer',status:'received',created:'2026-11-15T00:00:00Z',input_tokens:null,output_tokens:0,total_tokens:null}));let empty=false,fail=false;const seen=[];const route='**/api/usage*';await page.route(route,r=>{seen.push(new URL(r.request().url()).search);return fail?broken(r):json(r,{summary,transcription,calls:empty?[]:calls,next_before:new URL(r.request().url()).search?null:2});});
    try {let panel=await settings('Usage');await visible(panel.getByRole('table'));assert.equal(await panel.getByRole('heading',{name:'System resources',exact:true}).count(),0);await matrix('usage-calls',panel.locator('.set4-page'));await panel.getByRole('button',{name:'Older calls',exact:true}).click();await visible(panel.getByRole('table'));assert(seen.includes('?before=2'));await panel.getByRole('button',{name:'Newer calls',exact:true}).click();await panel.getByRole('tab',{name:'Calls',exact:true}).focus();await page.keyboard.press('ArrowRight');await visible(panel.getByRole('tabpanel',{name:'Transcription',exact:true}));await matrix('usage-transcription',panel.locator('.set4-page'));
      await panel.getByRole('tab',{name:'Calls',exact:true}).click();empty=true;await page.evaluate(() => window.dispatchEvent(new Event('focus')));await visible(panel.getByText('No calls',{exact:true}));await screenshot('set4-usage-empty');fail=true;await page.evaluate(() => window.dispatchEvent(new Event('focus')));await visible(panel.getByRole('alert'));fail=false;await page.evaluate(() => window.dispatchEvent(new Event('focus')));await hidden(panel.getByRole('alert'));
    }finally{await page.unroute(route);await workspace();}
  });
  await check('SET4 Shared data list, schema, SQL, delete recovery and containment',async()=>{
    const panel=await settings('Shared data');await visible(panel.getByText('settings_fixture',{exact:true}));assert.equal(await panel.getByRole('button',{name:'SQL query',exact:true}).getAttribute('aria-expanded'),'false');await panel.getByRole('button',{name:'Schema',exact:true}).click();await visible(panel.getByRole('region',{name:'Columns in settings_fixture',exact:true}));await matrix('shared-data',panel.locator('.set4-page'));await panel.getByRole('button',{name:'SQL query',exact:true}).click();await panel.getByRole('textbox',{name:'SQL query',exact:true}).fill('SELECT * FROM settings_fixture');const pending=page.waitForRequest(req=>new URL(req.url()).pathname==='/api/data/query');await panel.getByRole('button',{name:'Run query',exact:true}).click();assert.deepEqual(Object.keys((await pending).postDataJSON()).sort(),['id','revision','sql']);await visible(panel.getByText('Synthetic shared row',{exact:true}));await matrix('shared-query',panel.locator('.set4-page'));
    const route='**/api/data/drop',commands=[];let fail=true;await page.route(route,r=>{commands.push(r.request().postDataJSON());return fail?broken(r):r.continue();});
    try {await panel.getByRole('button',{name:'Menu settings_fixture',exact:true}).click();await page.getByRole('menuitem',{name:'Delete table settings_fixture',exact:true}).click();const confirm=page.getByRole('alertdialog');await visible(confirm);await confirm.getByRole('button',{name:'Cancel',exact:true}).click();assert.equal(commands.length,0);await panel.getByRole('button',{name:'Menu settings_fixture',exact:true}).click();await page.getByRole('menuitem',{name:'Delete table settings_fixture',exact:true}).click();await page.getByRole('alertdialog').getByRole('button',{name:'Delete table',exact:true}).click();await visible(panel.getByText('Not confirmed: delete settings_fixture',{exact:true}));await screenshot('set4-shared-delete-recovery');const saved=await page.evaluate(()=>JSON.parse(sessionStorage.getItem('minarbot:shared-data-drop')));assert.deepEqual(saved,commands[0]);fail=false;await panel.getByRole('button',{name:'Try again',exact:true}).click();await visible(panel.getByText('No tables',{exact:true}));assert.deepEqual(commands[0],commands[1]);assert.equal(await page.evaluate(()=>sessionStorage.getItem('minarbot:shared-data-drop')),null);await screenshot('set4-shared-empty');}
    finally{await page.unroute(route);}
  });
  await check('SET4 Shared data and host metrics error recovery',async()=>{
    let fail=true;await page.route('**/api/data?*',r=>fail?broken(r):json(r,{tables:[],next_after:null,revision:2}));
    try{const panel=await settings('Shared data');await page.evaluate(() => window.dispatchEvent(new Event('focus')));await visible(panel.getByRole('alert'));await screenshot('set4-shared-list-error');fail=false;await page.evaluate(() => window.dispatchEvent(new Event('focus')));await visible(panel.getByText('No tables',{exact:true}));await hidden(panel.getByRole('alert'));}finally{await page.unroute('**/api/data?*');}
    fail=true;const metrics=await fetch(origin+'/api/host-metrics').then(r=>r.json());await page.route('**/api/host-metrics',r=>fail?broken(r):json(r,metrics));
    try{const panel=await settings('Machines');await panel.getByRole('button',{name:'Resources',exact:true}).click();const detail=page.getByRole('dialog',{name:'This device',exact:true});await visible(detail.getByRole('alert'));await screenshot('set4-metrics-error');fail=false;await detail.getByRole('button',{name:'Reload metrics',exact:true}).click();await visible(detail.getByText('Host CPU',{exact:true}));await hidden(detail.getByRole('alert'));await close(detail);}finally{await page.unroute('**/api/host-metrics');}
  });
  await check('SET4 Backup download consent, restore CLI dialog and themes',async()=>{
    const panel=await settings('Backup & restore');await matrix('backup',panel.locator('.set4-page'));await panel.getByRole('button',{name:'Open',exact:true}).click();const restore=page.getByRole('dialog',{name:'Restore backup',exact:true});await visible(restore.getByRole('region',{name:'Backup restore command',exact:true}));assert.equal(await restore.getByRole('region',{name:'Backup restore command',exact:true}).innerText(),'./bin/minarbot-server restore workspace BACKUP.sqlite --db /new-directory/minarbot.sqlite');await matrix('restore',restore);await close(restore);
    await panel.getByRole('button',{name:'Download',exact:true}).click();const consent=page.getByRole('alertdialog',{name:'Download workspace backup?',exact:true});await visible(consent.getByText(/webhook secrets.*not encrypted/));await consent.getByRole('button',{name:'Cancel',exact:true}).click();await panel.getByRole('button',{name:'Download',exact:true}).click();const download=page.waitForEvent('download'),request=page.waitForRequest(req=>new URL(req.url()).pathname==='/api/backup');await page.getByRole('alertdialog').getByRole('button',{name:'Download backup',exact:true}).click();const req=await request;assert.equal(req.method(),'POST');assert.equal(req.postDataJSON().confirmed,true);assert.deepEqual(Object.keys(req.postDataJSON()).sort(),['confirmed','id']);assert.equal((await download).suggestedFilename(),'minarbot-workspace.sqlite');await visible(panel.getByText('Download started. Check the saved file.',{exact:true}));
  });
  await check('SET4 Backup error and retry recover without closing Settings',async()=>{
    const probe=await page.context().newPage(),errors=[];probe.on('pageerror',e=>errors.push(e.message));probe.on('console',message=>{if(message.type()!=='error')return;const detail={message:message.text(),url:message.location().url};if(detail.url===origin+'/api/backup'&&/409/.test(detail.message)){(receipt.expectedErrors??=[]).push(detail);}else errors.push(detail);});
    try{await probe.goto(origin);await probe.getByRole('button',{name:'Settings',exact:true}).click();const panel=probe.getByRole('dialog',{name:'Settings',exact:true});await panel.getByRole('button',{name:'Backup & restore',exact:true}).click();await probe.route('**/api/backup',r=>r.fulfill({status:409,json:{error:'Synthetic backup failure. Try downloading again.'}}));await panel.getByRole('button',{name:'Download',exact:true}).click();await probe.getByRole('alertdialog').getByRole('button',{name:'Download backup',exact:true}).click();await visible(panel.getByText('Synthetic backup failure. Try downloading again.',{exact:true}));await screenshot('set4-backup-error',probe);await probe.unroute('**/api/backup');await panel.getByRole('button',{name:'Download',exact:true}).click();const download=probe.waitForEvent('download');await probe.getByRole('alertdialog').getByRole('button',{name:'Download backup',exact:true}).click();await download;await hidden(panel.getByRole('alert'));assert.deepEqual(errors,[]);}
    finally{await probe.close();}
  });
  await check('SET4 Updates apply exact retry, reconnect, completion and rollback', async () => {
    const available = { id: '0.2.0-12345678', version: '0.2.0', commit: '12345678'.repeat(5), created: '2026-11-15T00:00:00Z', notes: 'Synthetic release. Automatic rollback available.' };
    const current = { ...available, id: '0.1.0-87654321', version: '0.1.0' };
    const state = { current, feed: available, available, launcherAvailable: true, runningTurns: 2 };
    const commands = [];
    let offline = false;
    await page.route('**/api/updates', route => offline ? route.abort('aborted') : json(route, state));
    await page.route('**/api/updates/apply', route => {
      const command = route.request().postDataJSON();
      commands.push(command);
      return commands.length === 1 ? route.abort('aborted') : json(route, { id: command.id });
    });
    try {
      const panel = await settings('Updates');
      await visible(panel.getByText('Minarbot 0.2.0', { exact: true }));
      await matrix('updates', panel.locator('.set4-page'));
      const offer = panel.getByRole('button', { name: 'Update now', exact: true });
      const confirm = page.getByRole('dialog', { name: 'Update 1 machine?', exact: true });
      await offer.click();
      await visible(confirm.getByText('2 running turns will be interrupted and can resume after the restart.', { exact: true }));
      await confirm.getByRole('button', { name: 'Cancel', exact: true }).click();
      await hidden(confirm);
      assert.deepEqual(commands, []);
      await offer.click();
      await confirm.getByRole('button', { name: 'Update', exact: true }).click();
      const progress = page.getByRole('dialog', { name: 'Updating 1 machine', exact: true });
      await visible(progress.getByText('Preparing update', { exact: true }));
      await page.waitForFunction(() => JSON.parse(sessionStorage.getItem('minarbot:update-everything')).commands[0].submitted);
      assert.equal(commands.length, 2);
      assert.deepEqual(commands[1], commands[0]);
      assert.deepEqual(await page.evaluate(() => JSON.parse(sessionStorage.getItem('minarbot:update'))), commands[0]);
      offline = true;
      await visible(progress.locator('p.k-field__hint').getByText('Reconnecting…', { exact: true }));
      await screenshot('set4-update-reconnect');
      offline = false;
      state.current = available;
      delete state.available;
      state.lastAttempt = { ...commands[0], status: 'done', target: available.id, started: available.created };
      const complete = page.getByRole('dialog', { name: 'Update complete', exact: true });
      await visible(complete.getByText('Updated', { exact: true }));
      await close(complete);
      assert.equal(await page.evaluate(() => sessionStorage.getItem('minarbot:update')), null);

      const next = { ...available, id: '0.3.0-23456789', version: '0.3.0' };
      state.feed = state.available = next;
      await visible(panel.getByText('Minarbot 0.3.0', { exact: true }));
      await offer.click();
      await confirm.getByRole('button', { name: 'Update', exact: true }).click();
      await page.waitForFunction(() => JSON.parse(sessionStorage.getItem('minarbot:update-everything')).commands[0].submitted);
      assert.equal(commands.length, 3);
      assert.notEqual(commands[2].id, commands[0].id);
      assert.equal(commands[2].version, next.version);
      state.lastAttempt = { ...commands[2], status: 'rolled-back', target: next.id, started: next.created, reason: 'Synthetic health check failure' };
      const result = page.getByRole('dialog', { name: 'Update results', exact: true });
      await visible(result.getByText('Rolled back', { exact: true }));
      await visible(result.getByText(state.lastAttempt.reason, { exact: true }));
      await screenshot('set4-update-rollback');
      await close(result);
    } finally {
      await page.unroute('**/api/updates');
      await page.unroute('**/api/updates/apply');
      await page.evaluate(() => { sessionStorage.removeItem('minarbot:update'); sessionStorage.removeItem('minarbot:update-everything'); });
      await workspace();
    }
  });
  await check('SET4 Updates fan-out review, exact retry and disabled reasons', async () => {
    const list = await fetch(origin + '/api/machines').then(response => response.json());
    const machine = { ...list.machines[0], allowUpdates: true, status: 'online' };
    const offline = { ...machine, id: randomUUID(), name: 'Offline fixture', status: 'offline' };
    const disabled = { ...machine, id: randomUUID(), name: 'Updates off fixture', allowUpdates: false };
    const release = { id: '0.2.0-12345678', version: '0.2.0', commit: '12345678'.repeat(5), created: '2026-11-15T00:00:00Z', notes: 'Fixture' };
    const commands = [];
    const peer = { machine: machine.id, available: true, launcherAvailable: true, runningTurns: 3, current: { version: '0.1.0' } };
    await page.route('**/api/updates', route => json(route, { current: release, feed: release, launcherAvailable: true, runningTurns: 0 }));
    await page.route('**/api/machines', route => json(route, { machines: [machine, offline, disabled] }));
    await page.route('**/api/updates/machines', route => json(route, { machines: [peer] }));
    await page.route('**/api/updates/fan-out', route => {
      const command = route.request().postDataJSON();
      commands.push(command);
      if (commands.length === 1) {
        peer.available = false;
        return route.abort('aborted');
      }
      return json(route, { machines: [{ machine: machine.id, status: 'accepted', id: command.id }] });
    });
    try {
      const panel = await settings('Updates');
      const toggle = panel.getByRole('switch', { name: `Also update ${machine.name}`, exact: true });
      await visible(toggle);
      for (const [row, reason] of [[offline, 'Offline'], [disabled, 'Remote updates off']]) {
        await visible(panel.locator('.k-row').filter({ hasText: row.name }).locator('.k-row__meta').getByText(reason, { exact: false }));
        assert.equal(await panel.getByRole('switch', { name: `Also update ${row.name}`, exact: true }).count(), 0);
      }
      const selected = panel.getByRole('button', { name: 'Update selected (1)', exact: true });
      assert.equal(await selected.count(), 0);
      await toggle.check();
      await selected.click();
      const review = page.getByRole('dialog', { name: 'Update 1 machine?', exact: true });
      await visible(review.getByText('3 running turns will be interrupted and can resume after the restart.', { exact: true }));
      await matrix('update-selected', review);
      await review.getByRole('button', { name: 'Update', exact: true }).click();
      const progress = page.getByRole('dialog', { name: 'Updating 1 machine', exact: true });
      await visible(progress.getByText('Preparing update', { exact: true }));
      await page.waitForFunction(() => JSON.parse(sessionStorage.getItem('minarbot:update-everything')).commands[0].submitted);
      assert.equal(commands.length, 2);
      assert.deepEqual(commands[0], commands[1]);
      assert.deepEqual(Object.keys(commands[0]).sort(), ['id', 'machines', 'version']);
      assert.deepEqual(commands[0].machines, [machine.id]);
      peer.current.version = release.version;
      const complete = page.getByRole('dialog', { name: 'Update complete', exact: true });
      await visible(complete.getByText('Updated', { exact: true }));
      await close(complete);
    } finally {
      for (const route of ['**/api/updates', '**/api/machines', '**/api/updates/machines', '**/api/updates/fan-out']) await page.unroute(route);
      await page.evaluate(() => sessionStorage.removeItem('minarbot:update-everything'));
      await workspace();
    }
  });
  await check('SET4 File transcription consent, whole video, retry charge and recovery',async()=>{
    await agentChat();const agents=await fetch(origin+'/api/agents').then(r=>r.json()),agent=agents.agents.find(row=>row.name===lead),id=randomUUID(),path=`/api/agents/${agent.id}/files/${id}`;const filesRoute=`**/api/agents/${agent.id}/panel/files`;await page.route(filesRoute,r=>json(r,[{id,name:'set4-video.mp4',size:22}]));
    let info={provider:{id:'fixture',origin:'http://127.0.0.1:1234',model:'fixture-transcribe'},format:'mp4',operation:null},fail=false;const commands=[],route=`**${path}?transcription=1`;
    await page.route(route,r=>{if(r.request().method()==='GET')return fail?broken(r):json(r,info);const body=r.request().postDataJSON();commands.push(body);if(body.action==='cancel')info.operation={...info.operation,status:'cancelled'};else info.operation={id:body.id,status:'pending',model:info.provider.model,origin:info.provider.origin,result:null,usage:null};return json(r,info.operation);});
    try {await page.reload();const details=page.getByRole('complementary',{name:'Conversation details',exact:true});const show=page.getByRole('button',{name:'Show details',exact:true});if(await show.isVisible())await show.click();await details.getByRole('tab',{name:'Files',exact:true}).click();await visible(details.getByText('set4-video.mp4',{exact:true}));await details.getByRole('button',{name:'File actions: set4-video.mp4',exact:true}).click();await page.getByRole('menuitem',{name:'Transcribe',exact:true}).click();const actions=details;await actions.getByRole('button',{name:'Transcribe',exact:true}).click();const controls=actions.locator('.set4-transcription');await visible(controls.getByText(/Whole file is sent/));const send=controls.getByRole('button',{name:'Transcribe',exact:true});assert(await send.isDisabled());assert(!(await controls.getByRole('checkbox').isChecked()));await matrix('transcribe',controls);await controls.getByRole('checkbox').check();await send.click();await visible(controls.getByText(/Processing/));assert.equal(commands[0].consent,true);assert.deepEqual(Object.keys(commands[0]).sort(),['consent','id','provider']);await controls.getByRole('button',{name:'Cancel transcription',exact:true}).click();await visible(controls.getByText(/Cancelled in Minarbot/));assert.deepEqual(commands[1],{action:'cancel',id:commands[0].id});await visible(controls.getByRole('checkbox',{name:/additional charge/}));assert(!(await controls.getByRole('checkbox').isChecked()));await controls.getByRole('checkbox').check();await controls.getByRole('button',{name:'Transcribe',exact:true}).click();assert.equal(commands[2].retryOf,commands[0].id);info.operation={...info.operation,status:'uncertain'};await controls.getByRole('button',{name:'Reload transcription status',exact:true}).click();await visible(controls.getByText(/previous result is uncertain/));await screenshot('set4-transcription-uncertain');fail=true;await controls.getByRole('button',{name:'Reload transcription status',exact:true}).click();await visible(controls.getByRole('alert'));fail=false;info.provider=null;await controls.getByRole('button',{name:'Reload transcription status',exact:true}).click();await visible(controls.getByText('Transcription unavailable. No provider configured.',{exact:true}));}
    finally{await page.unroute(route);await page.unroute(filesRoute);await workspace();}
  });
}

async function visible(locator) { await locator.waitFor({ state: 'visible' }); }
async function hidden(locator) { await locator.waitFor({ state: 'hidden' }); }
async function cleanPage(scope = page) {
  assert.deepEqual(await scope.getByRole('alert').allTextContents(), [], 'Unexpected error banner');
  assert.equal(await scope.locator('.legacy').evaluateAll(elements => elements.filter(element => !element.parentElement?.closest('.bs-root')).length), 0, 'Legacy content lacks a bs-root ancestor');
  await hidden(scope.getByRole('heading', { name: 'Panel could not be loaded', exact: true }));
}
async function workspace() {
  await page.goto(origin);
  await page.locator('.sidebar [aria-label="Create new"], .sidebar-reveal').first().waitFor({ state: 'visible' });
  const reveal = page.getByRole('button', { name: 'Expand sidebar', exact: true });
  if (await reveal.isVisible()) await reveal.click();
  await page.locator('.sidebar [aria-label="Create new"]').waitFor({ state: 'visible' });
  await hidden(page.getByText('Loading workspace...', { exact: true }));
  await closePanelSheet();
  await visible(page.getByRole('button', { name: 'Create new', exact: true }));
  await hidden(page.getByText('Loading workspace...', { exact: true }));
}
async function closePanelSheet() {
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  const sheet = page.getByRole('dialog', { name: 'Conversation panel', exact: true });
  if (await sheet.isVisible()) await sheet.getByRole('button', { name: 'Close details', exact: true }).click();
}
async function sidebarTab(name) {
  await page.locator('.sidebar [aria-label="Create new"]').waitFor({ state: 'visible' });
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  await closePanelSheet();
  await page.getByRole('complementary', { name: 'Workspace navigation', exact: true }).getByRole('tab', { name: new RegExp(`^${name}`) }).click();
}
async function openNew(name, keepDefault = false) {
  await closePanelSheet();
  await page.getByRole('complementary', { name: 'Workspace navigation' }).getByRole('button', { name: 'Create new', exact: true }).click();
  await page.getByRole('menuitem', { name, exact: true }).click();
  const dialog = page.getByRole('dialog', { name, exact: true });
  await visible(dialog);
  if (name === 'New agent' && !keepDefault) {
    const advanced = dialog.getByRole('button', { name: 'Advanced settings', exact: true }); await advanced.click();
    await dialog.getByRole('combobox', { name: 'Run with', exact: true }).click(); await page.getByRole('option', { name: 'API model', exact: true }).click(); await advanced.click(); await dialog.getByLabel('Name', { exact: true }).focus();
  }
  return dialog;
}
async function createAgent(name) {
  await workspace();
  const dialog = await openNew('New agent');
  await dialog.getByLabel('Name', { exact: true }).fill(name);
  await dialog.getByRole('textbox', { name: /^Role/ }).fill('Personal agent');
  await dialog.getByRole('button', { name: 'Advanced settings', exact: true }).click();
  const model = dialog.getByRole('combobox', { name: 'Model', exact: true });
  await model.fill(models[0]);
  await model.press('Tab');
  await dialog.getByLabel(/^Instructions/, { exact: false }).fill('Reply briefly to the supplied task.');
  await dialog.getByRole('button', { name: 'Create agent', exact: true }).click();
  await hidden(dialog);
  await visible(page.getByRole('main', { name: `Conversation ${name}`, exact: true }));
  await visible(page.getByRole('textbox', { name: `Message ${name}`, exact: true }));
}
async function agentChat() {
  await workspace();
  await sidebarTab('Agents');
  await page.getByRole('navigation', { name: 'Choose agent', exact: true }).getByRole('button', { name: new RegExp(`^${lead} `) }).click();
  await visible(page.getByRole('main', { name: `Conversation ${lead}`, exact: true }));
}
async function send(name, text) {
  const chat = page.getByRole('main', { name: `Conversation ${name}`, exact: true });
  const message = chat.locator('.composer__input');
  await message.fill(text);
  await message.press('Enter');
  await visible(chat.getByRole('region', { name: 'Conversation history', exact: true }).getByText(text, { exact: true }));
  await screenshot(`${name}-user-message`);
  if (name.includes(' / ')) {
    await visible(chat.locator('.room-conversation'));
    await visible(chat.locator('.room-conversation').getByText('Smoke reply:', { exact: false }).first());
    await page.waitForFunction(() => !document.querySelector('.room-pending'));
    assert.equal(await chat.locator('.recipient-picker').count(), 0, 'Room composer still exposes recipient picker');
  } else {
    await visible(chat.getByRole('button', { name: 'Copy reply', exact: true }).last());
    await visible(chat.getByText('Smoke reply:', { exact: false }));
    await visible(chat.getByRole('strong').filter({ hasText: 'ready' }));
  }
  await cleanPage(chat);
}
async function projectHome(name) {
  await page.getByRole('button', { name: `Menu project ${name}`, exact: true }).click();
  await page.getByRole('menuitem', { name: 'Project settings', exact: true }).click();
}
async function settings(section, heading = section) {
  if (section === 'MCP server') { section = 'External access'; if (heading === 'MCP server') heading = 'External access'; }
  if (section === 'Backup') section = 'Backup & restore';
  let dialog = page.getByRole('dialog', { name: 'Settings', exact: true });
  if (!await dialog.isVisible()) {
    await workspace();
    await page.getByRole('button', { name: /^Settings(?: - Update available)?$/ }).click();
  }
  await dialog.getByRole('button', { name: section, exact: true }).click();
  await visible(dialog.getByRole('heading', { name: heading, level: 1, exact: true }));
  return dialog;
}

async function settingsConnectionsSmoke(mcpPort) {
  await workspace();
  const post = async (path, body) => {
    const response = await fetch(origin + path, { method: 'POST', headers: { Origin: origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
    assert(response.ok, `Fixture request failed: ${path} (${response.status})`);
    return response.json();
  };
  const directory = await fetch(origin + '/api/agents').then(response => response.json());
  const agent = directory.agents[0]; assert(agent, 'SET3 needs one fixture agent');
  const project = await post('/api/projects', { id: randomUUID(), name: 'SET3 project', members: [{ agent: agent.id, write: false }] });
  const projectTab = async name => {
    await workspace(); await sidebarTab('Projects');
    await projectHome('SET3 project');
    await page.getByRole('tablist', { name: 'Project sections', exact: true }).getByRole('tab', { name, exact: true }).click();
    await visible(page.getByRole('heading', { name: name === 'MCP' ? 'MCP connections' : name, level: 1, exact: true }));
    return page.locator('.project-home');
  };
  const matrix = async label => {
    for (const viewport of [{ width: 1440, height: 900 }, { width: 1024, height: 768 }]) {
      await page.setViewportSize(viewport);
      for (const [palette, theme] of [['minarbot', 'dark'], ['minarbot', 'light'], ['catppuccin', 'dark']]) {
        await page.evaluate(({ palette, theme }) => { document.documentElement.dataset.colorTheme = palette; document.documentElement.dataset.theme = theme; }, { palette, theme });
        await screenshot(`set3-${label}-${palette}-${theme}-${viewport.width}x${viewport.height}`);
        assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), 'SET3 page overflow');
        const overflow = await page.locator('.app-settings__page, .k-dialog__body').evaluateAll(nodes => nodes.filter(node => node.getBoundingClientRect().height > 0 && node.scrollWidth > node.clientWidth + 1).length);
        assert.equal(overflow, 0, 'SET3 content overflow');
      }
    }
    await page.setViewportSize(receipt.viewport);
    await page.evaluate(() => { document.documentElement.dataset.colorTheme = 'minarbot'; document.documentElement.dataset.theme = 'dark'; });
  };
  await check('SET3 Skills tabs and pack saved-version installation', async () => {
    let dialog = await settings('Skills');
    assert.deepEqual(await dialog.getByRole('tablist', { name: 'Skills', exact: true }).getByRole('tab').allTextContents(), ['Library', 'Installed', 'Packs']);
    await visible(dialog.getByRole('tabpanel', { name: 'Library', exact: true })); await matrix('skills-library');
    await dialog.getByRole('tab', { name: 'Library', exact: true }).focus(); await page.keyboard.press('ArrowRight');
    await visible(dialog.getByRole('tabpanel', { name: 'Installed', exact: true })); await matrix('skills-installed');
    await page.keyboard.press('ArrowRight'); await visible(dialog.getByRole('tabpanel', { name: 'Packs', exact: true })); await visible(dialog.getByText('No packs', { exact: true })); await matrix('packs-empty');
    const saved = await post('/api/skills', { id: randomUUID(), name: 'set3-skill', description: 'Fixture requests', content: 'Keep fixture answers brief.', revision: 0 });
    const refreshed = page.waitForResponse(response => new URL(response.url()).pathname === '/api/skills');
    await page.evaluate(() => window.dispatchEvent(new Event('focus'))); await refreshed;
    await dialog.getByRole('button', { name: 'Create pack', exact: true }).click();
    let editor = page.getByRole('dialog', { name: 'New pack', exact: true });
    await editor.getByLabel('Name', { exact: true }).fill('SET3 pack');
    await editor.getByRole('combobox', { name: 'Skill from library', exact: true }).click(); await page.getByRole('option', { name: 'set3-skill', exact: true }).click();
    await editor.getByRole('button', { name: 'Add skill', exact: true }).click(); await visible(editor.locator('.k-row__title').filter({ hasText: /^set3-skill$/ }));
    await matrix('pack-editor');
    await editor.getByRole('button', { name: 'Save version', exact: true }).click(); await hidden(editor);
    await dialog.getByRole('button', { name: /^SET3 pack/ }).click(); editor = page.getByRole('dialog', { name: 'SET3 pack', exact: true });
    await editor.getByLabel('Name', { exact: true }).fill('Unsaved pack name'); editor = page.getByRole('dialog', { name: 'Unsaved pack name', exact: true });
    await editor.getByRole('button', { name: 'Pack installation actions', exact: true }).click(); await page.getByRole('menuitem', { name: 'Install v1', exact: true }).click();
    const confirm = page.getByRole('alertdialog'); await confirm.getByRole('button', { name: 'Apply pack', exact: true }).click(); await hidden(confirm);
    await visible(editor.getByText('Installed version 1', { exact: true }));
    const packs = await fetch(origin + '/api/skill-packs').then(response => response.json()); assert(packs.packs.some(row => row.name === 'SET3 pack'), 'Unsaved draft changed library');
    await editor.getByRole('button', { name: 'Close', exact: true }).last().click(); await page.getByRole('alertdialog').getByRole('button', { name: 'Discard changes', exact: true }).click(); await hidden(editor);
    await dialog.getByRole('tab', { name: 'Library', exact: true }).click(); await page.evaluate(() => window.dispatchEvent(new Event('focus'))); await visible(dialog.getByRole('button', { name: /^set3-skill/ }));
    await matrix('skills-populated');
    await dialog.getByRole('button', { name: 'Import', exact: true }).click(); await visible(page.getByRole('menuitem', { name: 'Folder', exact: true }));
    const chooserPromise = page.waitForEvent('filechooser'); await page.getByRole('menuitem', { name: 'File', exact: true }).click(); const chooser = await chooserPromise;
    await chooser.setFiles({ name: 'SKILL.md', mimeType: 'text/markdown', buffer: Buffer.from('---\nname: set3-import\ndescription: Fixture import\n---\nImported fixture instructions.') });
    editor = page.getByRole('dialog', { name: 'Import set3-import', exact: true }); await visible(editor); await editor.getByRole('button', { name: 'Close', exact: true }).last().click();
    if (await page.getByRole('alertdialog').isVisible()) await page.getByRole('alertdialog').getByRole('button', { name: 'Discard changes', exact: true }).click();
    await hidden(editor);
  });
  await check('SET3 Connected apps setup, list, detail and grants', async () => {
    let dialog = await settings('Connected apps'); await visible(dialog.getByText('Connect apps', { exact: true })); await matrix('apps-empty');
    await dialog.getByRole('button', { name: 'Set up', exact: true }).click(); const setup = page.getByRole('dialog', { name: 'Set up Composio', exact: true }); await visible(setup); await matrix('apps-setup'); await page.keyboard.press('Escape'); await hidden(setup);
    const pattern = /\/api\/(?:connected-apps(?:\?|\/(?:key|accounts)$)|agents\/[^/]+\/connected-app)/;
    let enabled = false, keySaves = 0; const commands = [];
    await page.route(pattern, async route => {
      const req = route.request(), url = new URL(req.url());
      if (req.method() === 'POST') { const body = req.postDataJSON(); const { key, ...metadata } = body; commands.push({ path: url.pathname, body: metadata }); if (url.pathname.endsWith('/key') && ++keySaves === 1) return route.fulfill({ status: 200, contentType: 'application/json', body: '{lost key acknowledgement' }); if ('enabled' in body) enabled = body.enabled; return route.fulfill({ json: {} }); }
      const value = url.pathname.endsWith('/key') ? { source: 'settings', revision: 1 } : url.pathname.endsWith('/accounts') ? { items: [{ id: 'set3-account', toolkit: 'github', status: 'ACTIVE' }] } : url.pathname.endsWith('/connected-app-policies') ? { items: [{ toolkit: 'github', tool: 'GITHUB_LIST_REPOS', account: 'set3-account', fingerprint: 'a'.repeat(64) }] } : url.pathname.startsWith('/api/agents/') ? { items: [{ toolkit: 'github', enabled, revision: 1 }] } : { configured: true, items: [{ slug: 'github', name: 'GitHub', description: 'Fixture', managedAuth: true }], categories: [{ id: 'dev', name: 'Development' }], next_after: null };
      await route.fulfill({ json: value });
    });
    try {
      await settings('Skills');
      dialog = await settings('Connected apps');
      await visible(dialog.getByRole('button', { name: /^GitHub set3-account/ })); await matrix('apps-list');
      await dialog.getByRole('button', { name: /^GitHub set3-account/ }).click(); await visible(dialog.getByRole('heading', { name: 'GitHub', exact: true })); await matrix('apps-detail');
      await dialog.getByRole('switch').click(); await page.waitForFunction(() => document.querySelector('.app-settings [role=switch]')?.getAttribute('aria-checked') === 'true');
      assert.match(commands[0].path, /\/api\/agents\/[^/]+\/connected-apps\/github$/); assert.deepEqual(Object.keys(commands[0].body).sort(), ['enabled', 'id', 'revision']);
      const revoked = page.waitForResponse(response => new URL(response.url()).pathname.endsWith('/connected-app-policies') && response.request().method() === 'POST'); await dialog.getByRole('button', { name: 'Ask next time', exact: true }).click(); await revoked; assert.equal(commands.at(-1).body.fingerprint, 'a'.repeat(64));
      await dialog.getByRole('button', { name: 'Integration setup', exact: true }).click(); await page.getByRole('menuitem', { name: 'Replace key', exact: true }).click(); await page.getByRole('alertdialog').getByRole('button', { name: 'Cancel', exact: true }).click();
      await dialog.getByRole('button', { name: 'Integration setup', exact: true }).click(); await page.getByRole('menuitem', { name: 'Replace key', exact: true }).click(); await page.getByRole('alertdialog').getByRole('button', { name: 'Replace key', exact: true }).click();
      const replacement = page.getByRole('dialog', { name: 'Replace key', exact: true }); await replacement.getByLabel('Composio API key', { exact: true }).fill('set3-fixture-composio-key'); await replacement.getByRole('button', { name: 'Save key', exact: true }).click(); await visible(replacement.getByText('Not confirmed', { exact: true }));
      const keyCommand = commands.at(-1); assert.deepEqual(Object.keys(keyCommand.body).sort(), ['id', 'revision']); assert(!(await page.evaluate(() => sessionStorage.getItem('minarbot:connected-app-command'))).includes('set3-fixture-composio-key'));
      await page.keyboard.press('Escape'); await hidden(replacement); await settings('Skills'); dialog = await settings('Connected apps'); await dialog.getByRole('button', { name: 'Enter key', exact: true }).click();
      const retry = page.getByRole('dialog', { name: 'Set up Composio', exact: true }); assert.equal(await retry.getByLabel('Composio API key', { exact: true }).inputValue(), ''); assert(await retry.getByRole('button', { name: 'Try again', exact: true }).last().isDisabled()); await retry.getByLabel('Composio API key', { exact: true }).fill('set3-fixture-composio-key'); await retry.getByRole('button', { name: 'Try again', exact: true }).last().click(); await hidden(retry); assert.deepEqual(commands.at(-1), keyCommand);
      await projectTab('Connected apps'); await visible(page.getByRole('heading', { name: 'SET3 project', exact: true })); await matrix('project-apps');
      const inherited = page.getByRole('switch', { name: 'Allow global apps', exact: true }); await inherited.click(); await visible(page.getByRole('alertdialog')); await page.getByRole('alertdialog').getByRole('button', { name: 'Cancel', exact: true }).click(); assert.equal(await inherited.getAttribute('aria-checked'), 'false');
    } finally { await page.unroute(pattern); }
  });
  await check('SET3 MCP tested connection, detail and staged permissions', async () => {
    const dialog = await settings('MCP connections'); await visible(dialog.getByText('No connections', { exact: true })); await matrix('mcp-empty');
    await dialog.getByRole('button', { name: 'Add connection', exact: true }).click(); const editor = page.getByRole('dialog', { name: 'New connection', exact: true });
    await editor.getByLabel('Name', { exact: true }).fill('SET3 service'); await editor.getByLabel('URL', { exact: true }).fill(`http://127.0.0.1:${mcpPort}/mcp`); await editor.getByLabel(/^Token/).fill('set3-fixture-bearer');
    assert.equal(await editor.getByRole('button', { name: 'Save connection', exact: true }).count(), 0);
    await editor.getByRole('button', { name: 'Test connection', exact: true }).click(); await visible(editor.getByText('Tested', { exact: true })); await matrix('mcp-tested');
    const save = page.waitForRequest(req => new URL(req.url()).pathname === '/api/mcp' && req.method() === 'POST'); await editor.getByRole('button', { name: 'Save connection', exact: true }).click(); const command = (await save).postDataJSON(); assert.deepEqual(Object.keys(command).sort(), ['id', 'test']); await hidden(editor);
    await matrix('mcp-list'); await dialog.getByRole('button', { name: /^SET3 service/ }).click(); await visible(dialog.getByRole('checkbox', { name: 'Tool: echo', exact: true })); assert.equal(await dialog.getByRole('checkbox').count(), 4); await matrix('mcp-detail'); await dialog.getByRole('button', { name: 'Details: Fixture template', exact: true }).click(); await visible(dialog.getByText(/Permissions cover all URIs/)); await screenshot('set3-mcp-template-details');
    let changed = 0; const count = req => { if (/\/api\/agents\/[^/]+\/mcp\//.test(req.url()) && req.method() === 'POST') changed++; }; page.on('request', count);
    try {
      await dialog.getByRole('checkbox', { name: 'Tool: echo', exact: true }).check(); assert.equal(changed, 0, 'Checkbox submitted before Save');
      await dialog.getByRole('button', { name: 'Save permissions', exact: true }).click(); await page.getByRole('alertdialog').getByRole('button', { name: 'Save permissions', exact: true }).click(); await hidden(page.getByRole('alertdialog'));
      await page.waitForFunction(() => !document.querySelector('.app-settings button[aria-label="Reload"]')?.disabled); assert.equal(changed, 1);
    } finally { page.off('request', count); }
    await dialog.getByRole('button', { name: 'Menu SET3 service', exact: true }).click(); await page.getByRole('menuitem', { name: 'Edit connection', exact: true }).click();
    const edit = page.getByRole('dialog', { name: 'Edit SET3 service', exact: true }); assert(await edit.getByLabel('URL', { exact: true }).getAttribute('readonly') !== null); await visible(edit.getByRole('checkbox', { name: 'Replace token', exact: true })); await edit.getByRole('checkbox', { name: 'Replace token', exact: true }).check(); await edit.getByLabel(/^Token/).fill('set3-replacement-fixture'); await edit.getByRole('button', { name: 'Test connection', exact: true }).click(); await visible(edit.getByText('Tested', { exact: true })); await matrix('mcp-edit-tested');
    await edit.getByRole('button', { name: 'Save changes', exact: true }).click(); const reset = page.getByRole('alertdialog'); await visible(reset.getByText(/revoke all access permissions/)); await reset.getByRole('button', { name: 'Cancel', exact: true }).click();
    await edit.getByRole('button', { name: 'Save changes', exact: true }).click(); const update = page.waitForRequest(req => /\/api\/mcp\/[^/]+$/.test(new URL(req.url()).pathname) && req.method() === 'POST'); await reset.getByRole('button', { name: 'Save changes', exact: true }).click(); assert.deepEqual(Object.keys((await update).postDataJSON()).sort(), ['id', 'revision', 'test']); await hidden(edit); await visible(dialog.getByRole('checkbox', { name: 'Tool: echo', exact: true })); assert.equal(await dialog.getByRole('checkbox', { checked: true }).count(), 0);
    await projectTab('MCP'); await visible(page.getByText('No connections', { exact: true })); await matrix('project-mcp');
  });
  await check('SET3 External access groups, one-time result and token detail', async () => {
    const dialog = await settings('External access'); await hidden(dialog.locator('.k-skeleton').first()); await matrix('external-list');
    await dialog.getByRole('button', { name: 'Create token', exact: true }).click(); const create = page.getByRole('dialog', { name: 'Create token', exact: true });
    await create.getByLabel('Name', { exact: true }).fill('SET3 client'); assert.equal(await create.getByRole('checkbox').count(), 21); assert.equal(await create.getByRole('checkbox', { checked: true }).count(), 1); await create.getByRole('checkbox', { name: 'Read token conversations', exact: true }).check();
    await matrix('external-permissions'); await create.getByRole('button', { name: 'Create token', exact: true }).click(); await hidden(create);
    const result = page.getByRole('dialog', { name: 'Copy token now', exact: true }); await visible(result.getByRole('heading', { name: 'Copy token now', exact: true }).last()); await hidden(page.getByRole('tooltip')); await visible(result.getByText('Shown once', { exact: true }));
    const token = await result.getByLabel('Token', { exact: true }).inputValue(); assert(token.length > 20, 'Fixture token missing');
    await matrix('external-result-masked'); await result.getByRole('button', { name: 'Copy token', exact: true }).click(); await result.getByRole('button', { name: 'I saved the token', exact: true }).click(); await hidden(result);
    assert(!(await page.evaluate(() => JSON.stringify(sessionStorage))).includes(token), 'Token entered recovery storage');
    const metadata = await fetch(origin + '/api/mcp-server/peer-tokens').then(response => response.text()); assert(!metadata.includes(token), 'Token entered metadata');
    await dialog.getByRole('button', { name: /^SET3 client/ }).click(); const detail = page.getByRole('dialog', { name: 'SET3 client', exact: true }); await visible(detail.getByText('Read token conversations', { exact: true })); assert(!(await detail.textContent()).includes('history:read')); await matrix('external-detail'); await page.keyboard.press('Escape'); await hidden(detail);
    await dialog.getByRole('button', { name: 'Token actions for SET3 client', exact: true }).click(); await page.getByRole('menuitem', { name: 'Revoke token', exact: true }).click(); await page.getByRole('alertdialog').getByRole('button', { name: 'Revoke token', exact: true }).click(); await visible(dialog.getByRole('button', { name: /^SET3 client.*Revoked/ }));
  });
  await check('SET3 Project environment dialog and scope', async () => {
    await projectTab('Environment'); await visible(page.getByText('No environment variables', { exact: true })); await matrix('environment-empty');
    await page.getByRole('button', { name: 'Add variable', exact: true }).click(); let editor = page.getByRole('dialog', { name: 'Add variable', exact: true }); await editor.getByLabel('Variable name', { exact: true }).fill('SET3_TOKEN'); await editor.getByLabel('Secret value', { exact: true }).fill('set3-fixture-secret'); await matrix('environment-editor-masked');
    const save = page.waitForRequest(req => new URL(req.url()).pathname === `/api/projects/${project.id}/environment` && req.method() === 'POST'); await editor.getByRole('button', { name: 'Add variable', exact: true }).click(); const command = (await save).postDataJSON(); assert.deepEqual(Object.keys(command).sort(), ['id', 'name', 'updated', 'value']); await hidden(editor); await visible(page.getByText('SET3_TOKEN', { exact: true })); await matrix('environment-populated');
    await page.getByRole('button', { name: 'Variable actions for SET3_TOKEN', exact: true }).click(); await page.getByRole('menuitem', { name: 'Replace', exact: true }).click(); editor = page.getByRole('dialog', { name: 'Replace SET3_TOKEN', exact: true }); assert.equal(await editor.getByLabel('Secret value', { exact: true }).inputValue(), ''); await page.keyboard.press('Escape'); await hidden(editor);
    await page.getByRole('switch', { name: 'Expose environment to the agent computer', exact: true }).click(); await visible(page.getByRole('alertdialog')); await page.getByRole('alertdialog').getByRole('button', { name: 'Cancel', exact: true }).click();
  });
  for (const [section, path, storage, packs] of [
    ['Skills', '/api/skills', 'minarbot:skill-command', false],
    ['Skills', '/api/skill-packs', 'minarbot:pack-command', true],
    ['Connected apps', '/api/connected-apps?search=&category=&after=', 'minarbot:connected-app-command', false],
    ['MCP connections', '/api/mcp', 'minarbot:mcp-command', false],
    ['External access', '/api/mcp-server/peer-tokens', 'minarbot:mcp-server-command', false],
    ['Environment', `/api/projects/${project.id}/environment`, `minarbot:project-environment:${project.id}`, false],
  ]) await check(`SET3 ${packs ? 'Packs' : section} loading error reload corrupt recovery`, async () => {
    await workspace();
    let release; const gate = new Promise(resolve => { release = resolve; }); const pattern = origin + path;
    await page.route(pattern, async route => { if (route.request().method() !== 'GET') return route.continue(); await gate; await route.fulfill({ status: 200, contentType: 'application/json', body: '{fixture read failure' }); });
    try {
      const dialog = section === 'Environment' ? (await projectTab(section), page) : await settings(section);
      if (packs) await dialog.getByRole('tab', { name: 'Packs', exact: true }).click();
      await visible(dialog.locator('.k-skeleton').first()); await screenshot(`set3-${packs ? 'packs' : section}-loading`); release(); await visible(dialog.getByRole('alert').first()); await screenshot(`set3-${packs ? 'packs' : section}-error`);
    } finally { release(); await page.unroute(pattern); }
    let dialog = section === 'Environment' ? (await projectTab(section), page) : await settings(section === 'Skills' ? 'Appearance' : 'Skills');
    await page.evaluate(storage => sessionStorage.setItem(storage, '{invalid fixture recovery'), storage);
    dialog = section === 'Environment' ? (await projectTab(section), page) : await settings(section);
    if (packs) await dialog.getByRole('tab', { name: 'Packs', exact: true }).click();
    await visible(dialog.getByText('Recovery unavailable', { exact: true })); await dialog.getByRole('button', { name: 'Discard request', exact: true }).click();
    const confirm = page.getByRole('alertdialog'); await visible(confirm); await confirm.getByRole('button', { name: 'Cancel', exact: true }).click(); assert.equal(await page.evaluate(storage => sessionStorage.getItem(storage), storage), '{invalid fixture recovery');
    await dialog.getByRole('button', { name: 'Discard request', exact: true }).click(); await confirm.getByRole('button').last().click(); await hidden(confirm);
    await page.waitForFunction(storage => sessionStorage.getItem(storage) === null, storage); await hidden(dialog.getByText('Recovery unavailable', { exact: true })); await screenshot(`set3-${packs ? 'packs' : section}-recovered`);
  });
  await check('SET3 restored pack MCP token and environment commands keep identity', async () => {
    const skillRows = await fetch(origin + '/api/skills').then(response => response.json());
    const source = skillRows.skills.find(row => row.name === 'set3-skill');
    const connections = await fetch(origin + '/api/mcp').then(response => response.json()); const connection = connections.items.find(row => row.name === 'SET3 service');
    for (const [section, storage, command, packs] of [
      ['Skills', 'minarbot:pack-command', { path: '/api/skill-packs', body: { id: randomUUID(), name: 'SET3 recovered pack', revision: 0, members: [{ skill: source.id, version: 1 }] } }, true],
      ['MCP connections', 'minarbot:mcp-command', { path: `/api/mcp/${connection.id}`, body: { id: randomUUID(), revision: connection.revision, enabled: false } }, false],
      ['External access', 'minarbot:mcp-server-command', { path: '/api/mcp-server/peer-tokens', body: { id: randomUUID(), name: 'SET3 lost token', scopes: ['agents:read'] } }, false],
    ]) {
      // Commit on fixture server, then restore metadata as if its acknowledgement was lost.
      await post(command.path, command.body); await workspace(); await page.evaluate(({ storage, command }) => sessionStorage.setItem(storage, JSON.stringify(command)), { storage, command });
      const dialog = await settings(section); if (packs) await dialog.getByRole('tab', { name: 'Packs', exact: true }).click(); await visible(dialog.getByText('Not confirmed', { exact: true }));
      const sent = page.waitForRequest(req => new URL(req.url()).pathname === command.path && req.method() === 'POST'); const settled = page.waitForResponse(response => new URL(response.url()).pathname === command.path && response.request().method() === 'POST');
      await dialog.getByRole('button', { name: 'Try again', exact: true }).click(); assert.deepEqual((await sent).postDataJSON(), command.body); assert.equal((await settled).status(), 200);
      await page.waitForFunction(storage => sessionStorage.getItem(storage) === null, storage);
      if (section === 'External access') { await visible(dialog.getByText(/Its secret cannot be recovered/)); assert.equal(await page.getByRole('dialog', { name: 'Copy token now', exact: true }).count(), 0); await screenshot('set3-token-irrecoverable'); }
    }
    const command = { path: `/api/projects/${project.id}/environment`, body: { id: randomUUID(), name: 'SET3_RECOVERY', updated: null } };
    await post(command.path, { ...command.body, value: 'set3-recovery-fixture' }); await workspace(); const storage = `minarbot:project-environment:${project.id}`;
    await page.evaluate(({ storage, command }) => sessionStorage.setItem(storage, JSON.stringify(command)), { storage, command }); await projectTab('Environment'); await visible(page.getByText('Not confirmed', { exact: true }));
    await page.getByRole('button', { name: 'Enter value', exact: true }).click(); const editor = page.getByRole('dialog', { name: 'Add variable', exact: true }); assert(await editor.getByRole('button', { name: 'Try again', exact: true }).last().isDisabled()); await editor.getByLabel('Secret value', { exact: true }).fill('set3-recovery-fixture');
    const sent = page.waitForRequest(req => new URL(req.url()).pathname === command.path && req.method() === 'POST'); await editor.getByRole('button', { name: 'Try again', exact: true }).last().click(); const body = (await sent).postDataJSON(); const { value, ...metadata } = body; assert.equal(typeof value, 'string'); assert.deepEqual(metadata, command.body); await hidden(editor);
    assert.equal(await page.evaluate(storage => sessionStorage.getItem(storage), storage), null);
  });
  await check('SET3 skill uncertain save retries identical command', async () => {
    const dialog = await settings('Skills'); await dialog.getByRole('button', { name: 'Create skill', exact: true }).click(); const editor = page.getByRole('dialog', { name: 'New skill', exact: true });
    await editor.getByLabel('Name', { exact: true }).fill('set3-retry'); await editor.getByLabel('When to use', { exact: true }).fill('Fixture recovery'); await editor.getByLabel(/^Instructions/, { exact: false }).fill('Retry fixture.');
    const commands = [], pattern = origin + '/api/skills';
    await page.route(pattern, async route => {
      if (route.request().method() !== 'POST') return route.continue();
      commands.push(route.request().postDataJSON());
      if (commands.length > 1) return route.continue();
      const response = await route.fetch();
      assert(response.ok(), 'Fixture skill save failed');
      await route.fulfill({ status: 200, contentType: 'application/json', body: '{lost acknowledgement' });
    });
    try {
      await editor.getByRole('button', { name: 'Save version', exact: true }).click(); await visible(editor.getByRole('alert'));
      // Exact retry remains on the page after closing the failed editor.
      await editor.getByRole('button', { name: 'Close', exact: true }).last().click(); await page.getByRole('alertdialog').getByRole('button', { name: 'Discard changes', exact: true }).click(); await hidden(editor);
      await dialog.getByRole('button', { name: 'Try again', exact: true }).click(); await visible(dialog.getByRole('button', { name: /^set3-retry/ })); await page.waitForFunction(() => sessionStorage.getItem('minarbot:skill-command') === null); assert.equal(commands.length, 2); assert.deepEqual(commands[1], commands[0]);
    } finally { await page.unroute(pattern); }
  });
}

async function focusContained(dialog) {
  await dialog.getByRole('searchbox', { name: 'Search settings', exact: true }).focus();
  // Start at the first control and cross both ends repeatedly, including reverse wrap.
  for (const key of ['Shift+Tab', ...Array(40).fill('Tab'), ...Array(40).fill('Shift+Tab')]) {
    await page.keyboard.press(key);
    assert(await dialog.evaluate(element => element.contains(document.activeElement)), `${key} escaped Settings`);
  }
  // A tooltip consumes its own Escape before the modal. Test modal close from a plain control.
  await dialog.getByRole('searchbox', { name: 'Search settings', exact: true }).focus();
  await page.waitForFunction(() => [...document.querySelectorAll('[role="tooltip"]')].every(el => el.getClientRects().length === 0 || getComputedStyle(el).visibility === 'hidden'));
}

async function focusRestored(trigger) {
  // Closing visibility changes before Radix's deferred focus restoration runs.
  await page.waitForFunction(element => element === document.activeElement, await trigger.elementHandle(), { timeout: 1000 });
}

async function graphLayout(label) {
  const graph = page.getByRole('region', { name: 'Workflow graph, scroll to see all steps', exact: true });
  await visible(graph);
  await graph.scrollIntoViewIfNeeded();
  const layout = await graph.evaluate(element => {
    const content = element.querySelector('.workflow-graph-content');
    const box = content.getBoundingClientRect();
    return {
      content: { width: box.width, height: box.height, position: getComputedStyle(content).position },
      background: getComputedStyle(element).backgroundColor,
      nodes: [...element.querySelectorAll('.workflow-graph-node')].map(node => {
        const rect = node.getBoundingClientRect(), style = getComputedStyle(node);
        return { id: node.title, x: rect.x - box.x, y: rect.y - box.y, width: rect.width, height: rect.height,
          position: style.position, border: parseFloat(style.borderTopWidth), borderStyle: style.borderTopStyle,
          left: parseFloat(node.style.left), top: parseFloat(node.style.top) };
      }),
      edges: [...element.querySelectorAll('svg g text')].map(text => ({ text: text.textContent, fill: getComputedStyle(text).fill, color: getComputedStyle(text).color })),
    };
  });
  receipt.graphLayouts.push({ label, ...layout });
  assert.equal(layout.content.position, 'relative', 'Graph must anchor positioned cards');
  assert.equal(layout.nodes.length, 2, 'Connected fixture must render both steps');
  for (const node of layout.nodes) {
    assert.equal(node.position, 'absolute', `${node.id}: graph stylesheet missing`);
    assert(node.width > 100 && node.height > 40 && node.border > 0 && node.borderStyle !== 'none', `${node.id}: not a sized card`);
    assert(Math.abs(node.x - node.left) < 1 && Math.abs(node.y - node.top) < 1, `${node.id}: card ignores saved position`);
    assert(node.y >= 0 && node.y + node.height < layout.content.height, `${node.id}: card stacked below graph`);
  }
  const [first, second] = layout.nodes;
  assert(first.x + first.width <= second.x || second.x + second.width <= first.x || first.y + first.height <= second.y || second.y + second.height <= first.y, 'Graph cards overlap');
  assert.equal(layout.edges.length, 1, 'Fixture edge label missing');
  for (const edge of layout.edges) {
    assert(edge.fill !== layout.background && !['none', 'transparent', 'rgba(0, 0, 0, 0)'].includes(edge.fill), 'Edge label paint matches graph background');
    assert.notEqual(edge.color, layout.background, 'Edge label color matches graph background');
  }
  assert.deepEqual(receipt.cspViolations, [], 'Graph emitted CSP violations');
}

async function computerViewerSmoke() {
  await check('Computer VNC watch, takeover, clipboard, hand-back and reconnect', async () => {
    const route = /\/api\/agents\/[^/]+\/computer(?:\/[^?]+)?$/;
    const status = { available: true, enabled: true, state: 'running', revision: 1, width: 1280, height: 800, stopsInMs: 900000, requests: [], control: { mode: 'agent', who: 'fixture', since: new Date().toISOString(), revision: 0 } };
    const errors = [], badTickets = [];
    const r5Evidence = join(root, 'app/test-support/ui-smoke/artifacts/ux-r5-panel/after'); await mkdir(r5Evidence, { recursive: true });
    const consoleError = event => { if (event.type() === 'error') errors.push(event.text()); };
    page.on('console', consoleError);
    await page.route(route, async request => {
      const url = new URL(request.request().url()), raw = request.request().postData(), body = raw ? request.request().postDataJSON() : {};
      if (url.pathname.endsWith('/screenshot')) { await request.fulfill({ status: 200, contentType: 'image/png', body: await desktopImage }); return; }
      if (url.pathname.endsWith('/stop')) status.state = 'off';
      if (url.pathname.endsWith('/vnc-ticket') && (status.state !== 'running' || body.revision !== status.control.revision || body.mode === 'control' && status.control.mode !== 'user')) { badTickets.push(body); return request.fulfill({ status: 409, json: { error: 'Computer is not ready for this ticket.' } }); }
      if (url.pathname.endsWith('/control')) status.control = { ...status.control, mode: body.operation === 'take' ? 'user' : 'agent', revision: status.control.revision + 1 };
      await request.fulfill({ json: url.pathname.endsWith('/vnc-ticket') ? { ticket: 'smoke-single-use' } : status });
    });
    await installVncFixture(page, `data:image/png;base64,${(await desktopImage).toString('base64')}`);
    try {
      const panel = page.getByRole('region', { name: 'Computer', exact: true });
      await visible(panel.getByRole('button', { name: 'Open', exact: true }));
      await panel.getByRole('button', { name: 'Open', exact: true }).click();
      const viewer = page.locator('.computer-viewer');
      const canvas = viewer.locator('canvas'); await visible(canvas);
      await page.waitForFunction(() => document.querySelector('.computer-viewer canvas')?.width === 1280);
      assert.equal(await viewer.locator('.computer-viewer__toolbar .k-badge').count(), 1);
      assert.equal(await viewer.getByText('Agent in control', { exact: true }).count(), 1);
      await viewer.getByRole('button', { name: 'More computer actions', exact: true }).click();
      await visible(page.getByRole('menuitem', { name: 'Stop computer', exact: true }));
      assert(await page.getByRole('menuitem', { name: 'Stop computer', exact: true }).evaluate(el => el.classList.contains('is-danger')));
      await page.getByRole('menuitem', { name: 'Stop computer', exact: true }).click();
      await visible(page.getByRole('alertdialog', { name: 'Stop computer?', exact: true }));
      await page.getByRole('alertdialog').getByRole('button', { name: 'Cancel', exact: true }).click();
      async function fillsDesktop(label) {
        for (const viewport of [{ width: 1440, height: 900 }, { width: 1024, height: 768 }]) {
          await page.setViewportSize(viewport);
          await page.waitForTimeout(100);
          const geometry = await viewer.evaluate(el => {
            const screen = el.querySelector('.computer-viewer__screen').getBoundingClientRect(), body = el, box = el.getBoundingClientRect(), canvas = el.querySelector('canvas').getBoundingClientRect();
            return { width: screen.width, height: screen.height, screenLeft: screen.left, bodyLeft: body.getBoundingClientRect().left, bodyWidth: body.clientWidth, padding: parseFloat(getComputedStyle(body).paddingLeft), dialogHeight: box.height, top: box.top, right: box.right, canvasWidth: canvas.width, canvasHeight: canvas.height, overflow: el.scrollWidth > el.clientWidth + 1 };
          });
          assert(Math.abs(geometry.canvasWidth / geometry.canvasHeight - 1.6) < .01, JSON.stringify(geometry));
          assert(Math.abs(geometry.width - (geometry.bodyWidth - geometry.padding * 2)) <= 1, 'Screen leaves horizontal gutters');
          assert(geometry.canvasWidth <= geometry.width + 2 && geometry.canvasHeight <= geometry.height + 2 && (Math.abs(geometry.canvasWidth - geometry.width) <= 2 || Math.abs(geometry.canvasHeight - geometry.height) <= 2), `VNC canvas must fit screen without distortion: ${JSON.stringify(geometry)}`);
          assert(geometry.top >= 0 && geometry.dialogHeight <= viewport.height && geometry.right <= viewport.width && !geometry.overflow, JSON.stringify(geometry));
          await screenshot(`${label}-${viewport.width}x${viewport.height}`);
        }
        await page.setViewportSize(receipt.viewport);
      }
      await fillsDesktop('computer-agent-control');
      await page.screenshot({ path: join(r5Evidence, 'computer.png'), animations: 'disabled' });
      const inset = await viewer.evaluate(el => { const screen = el.querySelector('.computer-viewer__screen').getBoundingClientRect(), element = el.closest('.right-panel'), panel = element.getBoundingClientRect(), style = getComputedStyle(element); return { left: screen.left - panel.left - parseFloat(style.borderLeftWidth), right: panel.right - screen.right - parseFloat(style.borderRightWidth) }; });
      assert(Math.abs(inset.left - 16) < 1 && Math.abs(inset.right - 16) < 1, JSON.stringify(inset));
      assert.equal(await viewer.getByRole('button', { name: 'Pop out', exact: true }).innerText(), '');
      assert((await page.evaluate(() => window.__vncFake.frames)) > 0); await canvas.click(); await page.keyboard.press('a'); assert.equal(await page.evaluate(() => window.__vncFake.input.length), 0, 'Watch view must not send input');
      const originalPixel = await canvas.evaluate(element => [...element.getContext('2d').getImageData(0, 0, 1, 1).data]);
      for (const palette of ['minarbot', 'github', 'catppuccin', 'everforest', 'solarized', 'midnight']) for (const theme of ['dark', 'light']) {
        await page.evaluate(({ palette, theme }) => { document.documentElement.dataset.colorTheme = palette; document.documentElement.dataset.theme = theme; }, { palette, theme });
        assert.deepEqual(await canvas.evaluate(element => [...element.getContext('2d').getImageData(0, 0, 1, 1).data]), originalPixel, 'Palette recolored VNC pixels');
        await screenshot(`vnc-watch-${palette}-${theme}`);
      }
      await page.evaluate(() => { document.documentElement.dataset.colorTheme = 'minarbot'; });
      await viewer.getByRole('button', { name: 'Take control', exact: true }).click();
      await page.getByRole('alertdialog').getByRole('button', { name: 'Take control', exact: true }).click();
      await visible(viewer.getByRole('button', { name: 'Hand back', exact: true })); await visible(canvas);
      await page.waitForFunction(() => document.querySelector('.computer-viewer canvas')?.width === 1280);
      await fillsDesktop('computer-user-control');
      assert.equal(await viewer.getByText("You're in control", { exact: true }).count(), 1);
      const r6Evidence = join(root, 'app/test-support/ui-smoke/artifacts/ux-r6/after'); await mkdir(r6Evidence, { recursive: true });
      const actionLayout = await viewer.locator('.computer-viewer__actions').evaluate(el => [...el.children].filter(e => e.getClientRects().length).map(e => e.getBoundingClientRect().top));
      assert(Math.max(...actionLayout) - Math.min(...actionLayout) < 5, 'Computer actions wrap while in control');
      assert.equal(await viewer.locator('.computer-viewer__screen').evaluate(el => getComputedStyle(el).outlineOffset), '-2px');
      for (const name of ['Paste text', 'Pop out']) {
        const action = viewer.getByRole('button', { name, exact: true });
        assert.equal(await action.innerText(), ''); await action.hover(); await visible(page.getByRole('tooltip', { name, exact: true }));
      }
      await page.screenshot({ path: join(r6Evidence, '08-computer.png'), animations: 'disabled' });
      await captureComputerThemes('computer-user-control');
      await canvas.click(); await page.keyboard.press('a');
      await hidden(page.getByLabel('Type or paste text', { exact: true }));
      await viewer.getByRole('button', { name: 'Paste text', exact: true }).click();
      await page.getByLabel('Type or paste text', { exact: true }).fill('VNC clipboard fixture');
      await page.getByRole('button', { name: 'Set clipboard', exact: true }).click();
      await page.waitForTimeout(100);
      assert(await page.evaluate(() => window.__vncFake.input.some(message => message[0] === 4))); assert(await page.evaluate(() => window.__vncFake.input.some(message => message[0] === 5))); assert(await page.evaluate(() => window.__vncFake.input.some(message => message[0] === 6)));
      await captureComputerThemes('computer-paste-popover');
      await page.keyboard.press('Escape');
      await canvas.click(); await page.keyboard.press('Shift+Escape'); assert(await viewer.getByRole('button', { name: 'Hand back', exact: true }).evaluate(element => element === document.activeElement));
      await viewer.getByRole('button', { name: 'Hand back', exact: true }).click();
      await hidden(viewer.getByRole('button', { name: 'Hand back', exact: true }));
      await visible(canvas); await page.waitForFunction(() => document.querySelector('.computer-viewer canvas')?.width === 1280);
      await page.evaluate(() => window.__vncFake.closeAll());
      await visible(viewer.getByRole('button', { name: 'Reconnect', exact: true })); await viewer.getByRole('button', { name: 'Reconnect', exact: true }).click();
      await hidden(viewer.getByRole('button', { name: 'Reconnect', exact: true })); await visible(canvas);
      await viewer.getByRole('button', { name: 'More computer actions', exact: true }).click();
      await page.getByRole('menuitem', { name: 'Stop computer', exact: true }).click();
      await page.getByRole('alertdialog').getByRole('button', { name: 'Stop computer', exact: true }).click();
      await visible(viewer.getByRole('button', { name: 'Turn on computer', exact: true }));
      assert.deepEqual(badTickets, [], 'No VNC requests with stale revision or stopped desktop');
      assert.deepEqual(errors, [], 'No console errors after Hand back and Stop');
      await writeFile(join(r5Evidence, 'computer-flow.json'), JSON.stringify({ badTickets, consoleErrors: errors, state: status.state, control: status.control }, null, 2) + '\n');
      await page.screenshot({ path: join(r5Evidence, 'computer-stopped.png'), animations: 'disabled' });
      status.state = 'running';
      await page.evaluate(() => window.dispatchEvent(new Event('focus')));
      status.requests = [{ turn: 'smoke-takeover', reason: 'Sign in to continue.' }];
      await visible(viewer.getByText(`${lead} needs you to take over: Sign in to continue.`, { exact: true }));
      assert.equal(await viewer.locator('.computer-viewer__requests').count(), 1);
      await captureComputerThemes('computer-takeover-banner'); status.requests = [];
      for (const state of ['starting', 'off']) {
        status.state = state;
        if (state === 'starting') await visible(viewer.getByText('Starting computer...', { exact: true })); else await visible(viewer.getByRole('button', { name: 'Turn on computer', exact: true }));
        if (state === 'starting') assert.equal(await viewer.locator('.computer-viewer__empty .k-spinner').count(), 1);
        await captureComputerThemes(`computer-${state}`);
      }
      status.state = 'running'; await visible(canvas);
      await page.getByRole('tab', { name: 'Chats', exact: true }).click();
      await visible(panel.locator('.computer-compact-row > svg')); assert.equal(await panel.locator('img').count(), 0);
      await captureComputerThemes('computer-compact-running');
      await page.waitForTimeout(100); assert.equal(await page.evaluate(() => window.__vncFake.sockets.length), 0); assert((await page.evaluate(() => window.__vncFake.connections)) >= 4);
    } finally { await page.evaluate(() => { window.__vncFake?.closeAll(); window.WebSocket = window.__nativeWebSocket; }); page.off('console', consoleError); await page.unroute(route); await page.reload(); }
  });
}

try {
  try { await access(join(root, 'app/dist/index.html')); }
  catch { throw new Error('Missing app/dist/index.html. Run `npm ci` then `npm run build` in app/ before `node smoke.mjs`. This harness does not build the app.'); }
  const dist = await readFile(join(root, 'app/dist/index.html'));
  receipt.versions.distIndexSha256 = createHash('sha256').update(dist).digest('hex');
  receipt.versions.app = JSON.parse(await readFile(join(root, 'app/package.json'), 'utf8')).version;
  receipt.versions.sourceCommit = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();
  provider = createServer(fakeProvider);
  const providerPort = await listen(provider);
  mcpFixture = createServer(async (req, res) => {
    if (req.method === 'GET') { res.writeHead(405); return res.end(); }
    if (req.method === 'DELETE') { res.writeHead(200); return res.end(); }
    let raw = ''; for await (const chunk of req) raw += chunk;
    const call = raw ? JSON.parse(raw) : {};
    if(call.id === undefined) { res.writeHead(202); return res.end(); }
    const result = call.method === 'initialize'
      ? { protocolVersion: '2025-03-26', capabilities: { tools: {}, resources: {}, prompts: {} }, serverInfo: { name: 'ui-smoke', version: '1' } }
      : call.method === 'tools/list' ? { tools: [{ name: 'echo', description: 'Smoke echo', inputSchema: { type: 'object', properties: {}, additionalProperties: false } }] }
      : call.method === 'resources/list' ? { resources: [{ uri: 'fixture://notes', name: 'Fixture notes', mimeType: 'text/plain' }] }
      : call.method === 'resources/templates/list' ? { resourceTemplates: [{ uriTemplate: 'fixture://notes/{name}', name: 'Fixture template' }] }
      : call.method === 'prompts/list' ? { prompts: [{ name: 'brief', description: 'Fixture prompt', arguments: [] }] }
      : {};
    res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(JSON.stringify({ jsonrpc: '2.0', id: call.id, result }));
  });
  const mcpPort = await listen(mcpFixture);
  // PORT rejects zero; reserve an OS-assigned port, then release it for the app.
  const reservation = createServer();
  const port = await listen(reservation);
  await new Promise(resolve => reservation.close(resolve));
  data = await mkdtemp(join(process.env.MINARBOT_SMOKE_DATA_ROOT ?? join(root, 'app/node_modules'), '.ui-smoke-data-'));
  gitWorkspace = join(data, 'workspace'); await mkdir(gitWorkspace);
  const localGit = (...args) => execFileSync('git', ['-c', 'core.hooksPath=/dev/null', ...args], { cwd: gitWorkspace, env: { PATH: '/usr/bin:/bin', GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: '/dev/null', GIT_TERMINAL_PROMPT: '0' } });
  localGit('init', '-q', '--initial-branch=main'); localGit('config', 'user.name', 'UI Smoke'); localGit('config', 'user.email', 'ui-smoke@example.test');
  await writeFile(join(gitWorkspace, 'notes.txt'), 'Original first line\nShared context\n--Old dash line\n++Old plus line\n'); localGit('add', '--', 'notes.txt'); localGit('commit', '-q', '-m', 'Fixture baseline');
  await writeFile(join(gitWorkspace, 'notes.txt'), 'Updated first line\nShared context\n--New dash line\n++New plus line\n' + 'Long diff line '.repeat(100) + '\n');
  await writeFile(join(gitWorkspace, 'draft.txt'), 'Untracked draft\n');
  await writeFile(join(gitWorkspace, 'fixture.html'), '<!doctype html><title>Preview fixture</title><h1>Synthetic preview</h1>');
  const env = Object.fromEntries(['PATH', 'HOME', 'LANG', 'TZ', 'TMPDIR', 'MINARBOT_TEST_SERVER', 'MINARBOT_RUST_BINARY'].filter(key => process.env[key] !== undefined).map(key => [key, process.env[key]]));
  if (interrupted) throw new Error(`Interrupted by ${interrupted}`);
  app = spawn(process.execPath, ['--experimental-strip-types', 'app/test-support/ui-smoke/machines-fixture.mjs'], {
    cwd: root, env: { ...env, PORT: String(port), MINARBOT_DATA_DIR: data, MINARBOT_PROJECT_ROOT: join(output, 'project-folders'), MINARBOT_WORKSPACES: JSON.stringify([{ agent: gitAgent, directory: gitWorkspace, write: true }]), MINARBOT_PROVIDER: 'openai-compatible', OPENAI_BASE_URL: `http://127.0.0.1:${providerPort}/v1`, OPENAI_API_KEY: 'ui-smoke-fake-key', OPENAI_MODEL: models[0] },
    stdio: ['ignore', 'pipe', 'pipe', 'ipc'],
  });
  app.stderr.on('data', chunk => { appLog += chunk; });
  origin = await new Promise((resolve, reject) => {
    const startupTimeout = Number(process.env.MINARBOT_SMOKE_STARTUP_TIMEOUT_MS ?? 30_000);
    const timer = setTimeout(() => reject(new Error(`App did not print its origin within ${startupTimeout}ms.\n${appLog}`)), startupTimeout);
    const done = (error, value) => { clearTimeout(timer); error ? reject(error) : resolve(value); };
    app.once('error', error => done(error));
    app.once('exit', (code, signal) => done(new Error(`App exited before startup (${code ?? signal}).\n${appLog}`)));
    app.stdout.on('data', chunk => {
      appLog += chunk;
      const match = /^Minarbot: (http:\/\/127\.0\.0\.1:\d+)$/m.exec(appLog);
      if (match) done(null, match[1]);
    });
  });
  assert.equal(origin, `http://127.0.0.1:${port}`, 'App printed an unexpected origin');
  receipt.origin = origin;
  if (process.env.MINARBOT_SMOKE_PROFILE_ONLY === '1' || process.env.MINARBOT_SMOKE_SB1_ONLY === '1') console.log(`Fixture: ${origin}`);
  // Short descriptor path keeps Chrome Unix sockets below 108 bytes, inside this worktree.
  browserScratch = await open(output, 'r');
  process.env.TMPDIR = `/proc/${process.pid}/fd/${browserScratch.fd}`;
  browser = await chromium.launch({ channel: 'chrome', headless: true });
  receipt.versions.chrome = browser.version();
  const context = await browser.newContext({ viewport: receipt.viewport, colorScheme: 'dark' });
  await context.exposeBinding('__smokeGuardReport', ({ frame }, kind, detail) => {
    receipt[kind === 'csp' ? 'cspViolations' : 'nativeSelectViolations'].push({ check: activeCheck, url: frame.url(), ...detail, atMs: Date.now() - started });
  });
  // Synthetic local/cloud voices make speech UI checks deterministic without device audio.
  // Keep instrumentation out of script-disabled preview frames.
  await context.addInitScript(() => {
    if (window.top !== window) return;
    const local = { voiceURI: 'smoke-local', name: 'Smoke local voice', lang: 'en-US', localService: true, default: false };
    const cloud = { voiceURI: 'smoke-cloud', name: 'Smoke cloud voice', lang: 'en-US', localService: false, default: false };
    const engine = new EventTarget();
    const fixture = { voices: [local, cloud], spoken: [], cancelled: 0, fail: false };
    engine.getVoices = () => fixture.voices;
    engine.speak = utterance => { fixture.spoken.push(utterance); if (fixture.fail) utterance.onerror?.(new Event('error')); };
    engine.cancel = () => { fixture.cancelled++; };
    Object.defineProperty(window, 'speechSynthesis', { configurable: true, value: engine });
    window.SpeechSynthesisUtterance = class { constructor(text) { this.text = text; } };
    window.__speechSmoke = fixture;
  });
  await context.addInitScript(() => {
    if (window.top !== window) return;
    const pending = new Set(), reported = new WeakSet();
    const report = (kind, detail) => {
      const promise = window.__smokeGuardReport(kind, detail);
      pending.add(promise);
      void promise.finally(() => pending.delete(promise));
    };
    document.addEventListener('securitypolicyviolation', event => report('csp', {
      directive: event.effectiveDirective, blockedURI: event.blockedURI, disposition: event.disposition,
      sourceFile: event.sourceFile, lineNumber: event.lineNumber, sample: event.sample,
    }));
    // RequiredSelect keeps native missing-value validation until kit Select supports
    // required + empty values. Limit exception to known fields in this exact form.
    const requiredSelects = ['Starting step', 'Continue to', 'If approved', 'If matched', 'If not matched', 'Destination section']
      .map(label => `#workflow-editor select.k-input[required][aria-label="${label}"]`).join(',');
    const scan = () => {
      for (const select of document.querySelectorAll('.bs-root select')) {
        // Radix's hidden form mirror is not an interactive native dropdown.
        const style = getComputedStyle(select);
        const mirror = select.matches('select[aria-hidden="true"][tabindex="-1"]') && style.position === 'absolute' && style.clip === 'rect(0px, 0px, 0px, 0px)';
        if (mirror || select.matches(requiredSelects) || reported.has(select)) continue;
        reported.add(select);
        report('select', { html: select.outerHTML.slice(0, 2000) });
      }
    };
    new MutationObserver(scan).observe(document, { subtree: true, childList: true, attributes: true });
    window.__smokeGuardFlush = async () => { scan(); await Promise.all([...pending]); };
  });
  // Fail closed if a future UI introduces external browser requests.
  await context.route('**/*', async route => {
    try { await (new URL(route.request().url()).origin === origin ? route.continue() : route.abort('blockedbyclient')); }
    catch (error) {
      // Polling can finish a request while Playwright hands routing across a reload.
      if (new URL(route.request().url()).origin !== origin || !error.message.includes('Route is already handled!')) throw error;
      (receipt.routeRaces ??= []).push({ check: activeCheck, url: route.request().url() });
    }
  });
  page = await context.newPage();
  page.setDefaultTimeout(10_000);
  page.on('console', message => {
    const detail = { check: activeCheck, message: message.text(), location: message.location(), atMs: Date.now() - started };
    const expectedRefusal = activeCheck === 'UXC Computer enable and start, recovery and server refusal' && /\/computer\/start$/.test(detail.location.url) && /status of 409/.test(detail.message) || activeCheck === 'NC1 unavailable result targets show not found' && /status of 404/.test(detail.message) && /\/(?:sites|files)\//.test(detail.location.url);
    if (message.type() === 'error') (expectedRefusal ? receipt.expectedErrors ??= [] : receipt.consoleErrors).push(detail);
    if (/content.security.policy|\bCSP\b|violat(?:es|ion|ing).*directive/i.test(message.text())) receipt.cspViolations.push({ source: 'console', ...detail });
  });
  page.on('pageerror', error => receipt.pageErrors.push({ check: activeCheck, message: error.stack ?? error.message, atMs: Date.now() - started }));
  page.on('response', response => { if (new URL(response.url()).pathname.startsWith('/api/') && response.status() >= 500) receipt.failedRequests.push({ check: activeCheck, url: response.url(), method: response.request().method(), status: response.status(), atMs: Date.now() - started }); });
  receipt.timings.startupMs = Date.now() - started;

  if (process.env.MINARBOT_SMOKE_WHATSAPP_ONLY === '1') {
    await whatsappSmoke({ check, page, origin, settings, screenshot });
    throw new Error('SMOKE_STOP_AFTER');
  }
  if (process.env.MINARBOT_SMOKE_INSTANT_MENUS_ONLY === '1') {
    const agent = await page.request.post(origin + '/api/agents', { headers: { Origin: origin }, data: { runtime: 'api', id: gitAgent, name: lead, model: models[0], instructions: 'Synthetic menus fixture.' } });
    assert(agent.ok(), await agent.text());
    const backToChat = async () => {
      if (page.url() === 'about:blank') await workspace();
      await page.getByRole('navigation', { name: 'Choose agent', exact: true }).getByRole('button', { name: new RegExp('^' + lead + ' ') }).click();
      await visible(page.getByRole('main', { name: 'Conversation ' + lead, exact: true }));
    };
    await instantMenusSmoke({ page, check, backToChat, origin, agentId: gitAgent });
    throw new Error('SMOKE_STOP_AFTER');
  }
  if (process.env.MINARBOT_SMOKE_NC1_ONLY === '1') {
    await page.request.post(origin + '/api/agents', { headers: { Origin: origin }, data: { runtime: 'api', id: gitAgent, name: gitName, model: 'smoke-primary', instructions: 'Synthetic source control.' } });
    await fileOutputPanelSmoke({ page, origin, root, check, gitAgent, gitName, agentChat: async () => {} });
    await selfVerifySmoke({ page, origin, root, check, gitAgent, gitName, gitWorkspace });
    throw new Error('SMOKE_STOP_AFTER');
  }
  if (process.env.MINARBOT_SMOKE_SET3_ONLY === '1') {
    await createAgent(lead); await settingsConnectionsSmoke(mcpPort); throw new Error('SMOKE_STOP_AFTER');
  }
  if (process.env.MINARBOT_SMOKE_ISSUE1_ONLY === '1') {
    await createAgent(lead);
    await issue1Smoke({ page, check, agentChat, screenshot });
    throw new Error('SMOKE_STOP_AFTER');
  }
  await check('Empty workspace', async () => {
    await workspace();
    await visible(page.getByRole('button', { name: 'New agent', exact: true }));
    const evidence = join(root, 'app/test-support/ui-smoke/artifacts/ux-copy-settings/after');
    await mkdir(evidence, { recursive: true });
    for (const finding of ['UX-16', 'UX-32']) await page.screenshot({ path: join(evidence, `${finding}.png`), animations: 'disabled' });
    await sidebarTab('Projects');
    const projects = page.getByRole('navigation', { name: 'Projects', exact: true });
    await visible(projects.getByText('No projects yet', { exact: true }));
    assert.equal(await projects.locator('p').count(), 0);
    await projects.getByRole('button', { name: 'New project', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: 'New project', exact: true }); await visible(dialog);
    await page.keyboard.press('Escape'); await hidden(dialog); await sidebarTab('Agents');
  });
  let releaseRuntime;
  const runtimeGate = new Promise(resolve => { releaseRuntime = resolve; });
  await page.route('**/api/runtimes', async route => { const response = await route.fetch(); await runtimeGate; await route.fulfill({ response }); });
  await check('New agent dialog focus and collapsed Advanced settings', async () => {
    const dialog = await openNew('New agent', true);
    assert(await dialog.getByLabel('Name', { exact: true }).evaluate(element => element === document.activeElement), 'Opening New agent must focus Name');
    assert.equal(await dialog.getByRole('button', { name: 'Advanced settings', exact: true }).getAttribute('aria-expanded'), 'false');
    await hidden(dialog.getByLabel('Fallback model', { exact: false }));
    await hidden(dialog.getByLabel('Parallel chats', { exact: true }));
    await hidden(dialog.getByLabel('Tool step limit', { exact: true }));
  });
  await check('Runtime selector loads the Codex default and keeps API available', async () => {
    const dialog = page.getByRole('dialog', { name: 'New agent', exact: true });
    await dialog.getByRole('button', { name: 'Advanced settings', exact: true }).click();
    const selector = dialog.getByRole('combobox', { name: 'Run with', exact: true });
    await visible(selector.filter({ hasText: 'API model' }));
    await selector.click();
    assert.equal(await page.getByRole('option', { name: /^Codex/ }).count(), 0);
    assert.equal(await page.getByRole('option', { name: /^Claude Code/ }).count(), 0);
    await page.getByRole('option', { name: 'API model', exact: true }).click();
    const runtimes = page.waitForResponse(response => response.url().endsWith('/api/runtimes'));
    releaseRuntime();
    await runtimes;
    await page.unroute('**/api/runtimes');
    await visible(selector.filter({ hasText: 'Codex' }));
    await selector.click();
    await page.getByRole('option', { name: 'API model', exact: true }).click();
    assert.match(await selector.textContent(), /API model/);
  });
  await check('Create first agent and open chat', () => createAgent(lead));
  await providersHiddenSmoke({ page, check, origin, root, settings, workspace, lead });
  await whatsappSmoke({ check, page, origin, settings, screenshot });
  await copySettingsSmoke({ check, page, origin, root, openNew, settings, workspace, agentChat });
  if (process.env.MINARBOT_SMOKE_UXD_ONLY === '1') throw new Error('SMOKE_STOP_AFTER');
  if (process.env.MINARBOT_SMOKE_DELETE_ONLY === '1') { await deleteAgentSmoke({ page, check, origin, root, receipt }); throw new Error('SMOKE_STOP_AFTER'); }
  if (process.env.MINARBOT_SMOKE_HRC_ONLY === '1') { await roomControlsSmoke({ check, page, origin, screenshot, visible, hidden, agentChat, release: model => hrcGates.get(model)?.() }); throw new Error('SMOKE_STOP_AFTER'); }
  if (process.env.MINARBOT_SMOKE_MM1_ONLY === '1') { await timelineMinimapSmoke({ check, page, origin, root, agentChat }); throw new Error('SMOKE_STOP_AFTER'); }
  if (process.env.MINARBOT_SMOKE_TY1_ONLY === '1') {
    console.log(`Fixture: ${origin}`);
    await typographySmoke({ page, check, origin, agentChat, sidebarTab, send, root, baseline: process.env.MINARBOT_SMOKE_TY1_BASELINE === '1' });
    throw new Error('SMOKE_STOP_AFTER');
  }
  if (process.env.MINARBOT_SMOKE_R5B_ONLY === '1') { await r5PanelSmoke({ check, page, origin, root, agentChat, settings, openNew, gitAgent, gitName }); throw new Error('SMOKE_STOP_AFTER'); }
  if (process.env.MINARBOT_SMOKE_UXC_ONLY === '1') { await rightPanelUXSmoke({ check, page, origin, root, agentChat }); await widePanelsSmoke({ check, page, agentChat, settings, visible, hidden, screenshot, viewport: receipt.viewport }); throw new Error('SMOKE_STOP_AFTER'); }
  if (process.env.MINARBOT_SMOKE_LB1_ONLY === '1') {
    await detailsDeclutterSmoke({ page, check, origin, screenshot, gitAgent, gitName, models, baseline: process.env.MINARBOT_SMOKE_LB1_BASELINE === '1' });
    throw new Error('SMOKE_STOP_AFTER');
  }
  if (process.env.MINARBOT_SMOKE_SB1_ONLY === '1') {
    await createAgent(helper);
    const response = await page.request.post(origin + '/api/projects', { headers: { Origin: origin }, data: { id: randomUUID(), name: projectName, members: [] } });
    assert(response.ok());
    await page.reload(); await workspace();
    await sidebarSpacingSmoke({ page, check, sidebarTab, screenshot, projectName, baseline: process.env.MINARBOT_SMOKE_SB1_BASELINE === '1' });
    throw new Error('SMOKE_STOP_AFTER');
  }
  if (process.env.MINARBOT_SMOKE_ST1_ONLY === '1') {
    await streamActionsSmoke({ check, page, origin, screenshot, releaseStream, releaseRoom, releaseRoomDelivery, streamReply: 'Stable first block.\n\nGrowing second block' + streamReply.slice('Stable first block.\n\n'.length), agentChat, visible, hidden });
    throw new Error('SMOKE_STOP_AFTER');
  }
  await attachmentSmoke({ check, page, context, origin, agentChat, screenshot, viewport: receipt.viewport });
  if (process.env.MINARBOT_SMOKE_AT1_ONLY === '1') throw new Error('SMOKE_STOP_AFTER');
  if (process.env.MINARBOT_SMOKE_RS1_ONLY === '1') { await widePanelsSmoke({ check, page, agentChat, settings, visible, hidden, screenshot, viewport: receipt.viewport }); throw new Error('SMOKE_STOP_AFTER'); }
  if (process.env.MINARBOT_SMOKE_SETTINGS_DATA_ONLY === '1') { await settingsDataSmoke(); throw new Error('SMOKE_STOP_AFTER'); }
  if (process.env.MINARBOT_SMOKE_COMPUTER_ONLY === '1') { await computerViewerSmoke(); throw new Error('SMOKE_STOP_AFTER'); }
  if (process.env.MINARBOT_SMOKE_DESKTOP_ONLY === '1') {
    await desktopViewSmoke({ check, context, page, lead, agentChat, visible, hidden, screenshot, captureComputerThemes, viewport: receipt.viewport });
    throw new Error('SMOKE_STOP_AFTER');
  }
  await check('SET2 tabs, mounted payloads, coding runtime and validation', async () => {
    await agentChat();
    const directory = await page.evaluate(async () => (await fetch('/api/agents')).json());
    const agent = directory.agents.find(agent => agent.name === 'Smoke Lead');
    assert(agent, 'Fixture agent missing');
    const path = `/api/agents/${agent.id}/edit`, choices = `/api/agents/${agent.id}/runtimes`;
    const commands = [];
    await page.route(`**${choices}`, route => route.fulfill({ json: { choices: [{ runtime: 'codex', label: 'Codex', available: true, enabled: true, version: 'fixture', reason: null }, { runtime: 'claude', enabled: false, version: null, reason: 'Not installed' }] } }));
    await page.route(`**${path}`, async route => {
      const command = route.request().postDataJSON(); commands.push(command);
      assert.equal(route.request().method(), 'POST');
      return route.fulfill({ json: agent });
    });
    try {
      await page.getByRole('button', { name: 'Edit profile', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: 'Agent profile', exact: true });
      const baseline = await dialog.locator('form').evaluate(form => Object.fromEntries(new FormData(form)));
      assert.deepEqual(baseline, { name: agent.name, role: agent.role, instructions: agent.instructions, model: agent.model, backupModel: agent.backup_model ?? '', threadLimit: String(agent.thread_limit), toolBudget: String(agent.tool_budget ?? 90) });
      await dialog.getByRole('tab', { name: 'Model', exact: true }).click();
      const fallback = dialog.getByRole('button', { name: 'Fallback model', exact: true });
      assert.equal(await fallback.getAttribute('aria-expanded'), 'false');
      await fallback.click(); await dialog.getByLabel('Fallback model', { exact: false }).fill('smoke-secondary'); await page.keyboard.press('Escape'); await fallback.click();
      await dialog.getByRole('combobox', { name: 'Model', exact: true }).fill('smoke-primary'); await page.keyboard.press('Escape');
      await dialog.getByRole('tab', { name: 'Advanced', exact: true }).click();
      await dialog.getByLabel('Parallel chats', { exact: true }).fill('7');
      await dialog.getByLabel('Tool step limit', { exact: true }).fill('131');
      await dialog.getByRole('tab', { name: 'Advanced', exact: true }).click();
      await visible(dialog.getByRole('button', { name: 'Save changes', exact: true }));
      await dialog.getByRole('button', { name: 'Save changes', exact: true }).click(); await hidden(dialog);
      const { id, ...payload } = commands[0]; assert.match(id, /^[a-f0-9-]{36}$/i);
      assert.deepEqual(payload, { revision: agent.revision, webAccess: agent.web_access !== 0, providerAccount: agent.provider_account ?? '', name: agent.name, role: agent.role, instructions: agent.instructions, model: 'smoke-primary', backupModel: 'smoke-secondary', threadLimit: 7, toolBudget: 131 });
      // API-only fields stay mounted when switching runtime.
      await page.getByRole('button', { name: 'Edit profile', exact: true }).click();
      await dialog.getByRole('tab', { name: 'Model', exact: true }).click();
      await dialog.getByRole('combobox', { name: 'Model', exact: true }).fill('staged-switch-model'); await page.keyboard.press('Escape');
      await screenshot('harness-default-profile-switch');
      await dialog.getByRole('button', { name: 'Use Codex', exact: true }).click();
      await hidden(dialog);
      const { id: codingId, ...coding } = commands[1]; assert.match(codingId, /^[a-f0-9-]{36}$/i);
      assert.deepEqual(coding, { revision: agent.revision, webAccess: agent.web_access !== 0, providerAccount: agent.provider_account ?? '', name: agent.name, role: agent.role, instructions: agent.instructions, model: 'staged-switch-model', backupModel: agent.backup_model ?? '', threadLimit: agent.thread_limit, toolBudget: agent.tool_budget ?? 90, runtime: 'codex', confirmRuntime: true });
      await page.getByRole('button', { name: 'Edit profile', exact: true }).click();
      await dialog.getByLabel(/^Instructions/, { exact: false }).fill('');
      await dialog.getByRole('tab', { name: 'Model', exact: true }).click(); await dialog.getByRole('button', { name: 'Save changes', exact: true }).click();
      assert.equal(await dialog.getByRole('tab', { name: 'Profile', exact: true }).getAttribute('aria-selected'), 'true');
      assert(await dialog.getByLabel(/^Instructions/, { exact: false }).evaluate(input => input === document.activeElement));
      await dialog.getByLabel(/^Instructions/, { exact: false }).fill(agent.instructions);
      await dialog.getByRole('tab', { name: 'Advanced', exact: true }).click(); await dialog.getByLabel('Parallel chats', { exact: true }).fill('0');
      await dialog.getByRole('tab', { name: 'Profile', exact: true }).click(); await dialog.getByRole('button', { name: 'Save changes', exact: true }).click();
      assert.equal(await dialog.getByRole('tab', { name: 'Advanced', exact: true }).getAttribute('aria-selected'), 'true');
      await dialog.getByRole('tab', { name: 'Profile', exact: true }).click(); await dialog.getByLabel('Name', { exact: true }).fill('');
      await dialog.getByRole('button', { name: 'Save changes', exact: true }).click();
      assert.equal(await dialog.getByRole('tab', { name: 'Profile', exact: true }).getAttribute('aria-selected'), 'true');
      assert(await dialog.getByLabel('Name', { exact: true }).evaluate(input => input === document.activeElement), 'Multiple invalid tabs must reveal first invalid field');
      assert.equal(commands.length, 2, 'Invalid hidden fields submitted');
      await dialog.getByRole('button', { name: 'Cancel', exact: true }).click(); await hidden(dialog);
    } finally { await page.unroute(`**${path}`); await page.unroute(`**${choices}`); }
  });
  await check('SET2 profile tab matrix, keyboard tabs, avatar and focus return', async () => {
    await agentChat();
    const trigger = page.getByRole('button', { name: 'Edit profile', exact: true }); await trigger.click();
    const dialog = page.getByRole('dialog', { name: 'Agent profile', exact: true });
    await visible(dialog.getByLabel('Name', { exact: true }));
    assert.equal(await dialog.locator('.k-dialog__head').getByText(lead, { exact: true }).count(), 0);
    assert.equal(await dialog.getByRole('heading', { name: 'Agent sites', exact: true }).count(), 0);
    await dialog.getByRole('tab', { name: 'Profile', exact: true }).focus(); await page.keyboard.press('ArrowRight');
    await dialog.locator('[role=tab][aria-selected=true]').filter({ hasText: /^Model$/ }).waitFor();
    for (const viewport of [{ width: 1440, height: 900 }, { width: 1024, height: 768 }]) {
      await page.setViewportSize(viewport);
      for (const [palette, theme] of [['minarbot', 'dark'], ['minarbot', 'light'], ['catppuccin', 'dark']]) {
        await page.evaluate(({ palette, theme }) => { document.documentElement.dataset.colorTheme = palette; document.documentElement.dataset.theme = theme; }, { palette, theme });
        for (const name of ['Profile', 'Model', 'Advanced']) {
          await dialog.getByRole('tab', { name, exact: true }).click();
          await screenshot(`set2-${name}-${palette}-${theme}-${viewport.width}x${viewport.height}`);
          assert(await dialog.evaluate(element => element.scrollWidth <= element.clientWidth + 1));
          assert(await dialog.evaluate(element => { const box = element.getBoundingClientRect(); return box.y >= 0 && box.bottom <= innerHeight + 1; }));
          if (name === 'Profile') assert(await dialog.getByLabel(/^Instructions/, { exact: false }).evaluate(element => { const box = element.getBoundingClientRect(), body = element.closest('.k-dialog__body').getBoundingClientRect(); return box.bottom <= body.bottom && box.y >= body.y; }), 'Profile essentials below fold');
          await visible(dialog.getByRole('button', { name: 'Cancel', exact: true })); await visible(dialog.getByRole('button', { name: 'Save changes', exact: true }));
        }
      }
    }
    for (const textSize of ['large', 'larger']) {
      await page.evaluate(value => { document.documentElement.dataset.textSize = value; }, textSize);
      for (const name of ['Profile', 'Model', 'Advanced']) {
        await dialog.getByRole('tab', { name, exact: true }).click(); await screenshot(`set2-${name}-${textSize}-1024x768`);
        assert(await dialog.evaluate(element => element.scrollWidth <= element.clientWidth + 1), `${name} ${textSize}: overflow`);
        assert(await dialog.locator('.k-dialog__foot').evaluate(element => element.getBoundingClientRect().bottom <= innerHeight), `${name} ${textSize}: footer clipped`);
      }
    }
    await page.evaluate(() => { document.documentElement.dataset.textSize = 'standard'; });
    await page.setViewportSize(receipt.viewport); await page.evaluate(() => { document.documentElement.dataset.theme = 'dark'; document.documentElement.dataset.colorTheme = 'minarbot'; });
    await dialog.getByRole('tab', { name: 'Profile', exact: true }).click();
    await dialog.getByLabel('Role', { exact: false }).fill('Staged role');
    await dialog.getByRole('button', { name: 'Change', exact: true }).click();
    const avatar = page.getByRole('dialog', { name: 'Avatar', exact: true }); await visible(avatar); await visible(avatar.getByRole('button', { name: 'Save', exact: true }));
    await avatar.getByRole('button', { name: 'Done', exact: true }).focus(); await hidden(page.getByRole('tooltip'));
    await page.keyboard.press('Escape'); await hidden(avatar);
    await dialog.getByRole('button', { name: 'Change', exact: true }).waitFor();
    await focusRestored(dialog.getByRole('button', { name: 'Change', exact: true }));
    assert.equal(await dialog.getByLabel('Role', { exact: false }).inputValue(), 'Staged role');
    const exportMenu = dialog.getByRole('button', { name: 'Agent profile actions', exact: true }); await exportMenu.click(); await page.getByRole('menuitem', { name: 'Export template', exact: true }).click();
    const exported = page.getByRole('dialog', { name: 'Export template', exact: true }); await visible(exported); await exported.getByRole('button', { name: 'Preview export template', exact: true }).click(); await visible(exported.getByRole('button', { name: 'Download reviewed template', exact: true }));
    await exported.getByRole('button', { name: 'Done', exact: true }).click(); await hidden(exported); await focusRestored(exportMenu); assert.equal(await dialog.getByLabel('Role', { exact: false }).inputValue(), 'Staged role');

    await dialog.getByRole('tab', { name: 'Advanced', exact: true }).click(); await visible(dialog.getByRole('button', { name: 'Save changes', exact: true }));
    await dialog.getByRole('tab', { name: 'Profile', exact: true }).click(); await dialog.getByLabel('Role', { exact: false }).fill('Personal agent');
    await dialog.getByRole('tab', { name: 'Advanced', exact: true }).click(); await visible(dialog.getByRole('button', { name: 'Save changes', exact: true }));
    await dialog.getByRole('button', { name: 'Cancel', exact: true }).click(); await hidden(dialog);
    await focusRestored(trigger);
  });
  await check('SET2 availability confirmation, exact retry and immediate save state', async () => {
    await agentChat();
    const commands = [], pattern = /\/api\/agents\/[^/]+\/visibility$/;
    await page.route(pattern, async route => { commands.push(route.request().postDataJSON()); const response = await route.fetch(); return route.fulfill(commands.length === 1 ? { status: 200, contentType: 'application/json', body: '{lost acknowledgement' } : { response }); });
    try {
      await page.getByRole('button', { name: 'Edit profile', exact: true }).click();
      const profile = page.getByRole('dialog', { name: 'Agent profile', exact: true });
      await profile.getByRole('button', { name: 'Agent profile actions', exact: true }).click();
      await page.getByRole('menuitem', { name: 'Hide agent', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: 'Availability', exact: true });
      const hide = dialog.getByRole('button', { name: 'Hide agent', exact: true }); await hide.click();
      let confirmation = page.getByRole('alertdialog', { name: `Hide ${lead}?`, exact: true }); await visible(confirmation);
      await confirmation.getByRole('button', { name: 'Cancel', exact: true }).click(); await hidden(confirmation);
      assert.equal(commands.length, 0); await focusRestored(hide);
      await hide.click(); await confirmation.getByRole('button', { name: 'Hide agent', exact: true }).click();
      const retry = dialog.getByRole('button', { name: 'Try again', exact: true }); await visible(retry);
      const stored = await page.evaluate(() => Object.entries(sessionStorage).find(([key]) => key.startsWith('minarbot-visibility:')));
      assert.deepEqual(JSON.parse(stored[1]), commands[0]); assert.equal(commands[0].hidden, true);
      await retry.click(); const restore = dialog.getByRole('button', { name: 'Restore agent', exact: true }); await visible(restore); assert.deepEqual(commands[1], commands[0]);
      await visible(dialog.getByText('Hidden · queue paused', { exact: true }));
      await dialog.getByRole('button', { name: 'Done', exact: true }).click(); await hidden(dialog);
      await profile.getByRole('button', { name: 'Agent profile actions', exact: true }).click();
      await page.getByRole('menuitem', { name: 'Restore agent', exact: true }).click(); await visible(dialog);
      await restore.click(); confirmation = page.getByRole('alertdialog', { name: `Restore ${lead}?`, exact: true }); await visible(confirmation);
      await confirmation.getByRole('button', { name: 'Cancel', exact: true }).click(); await hidden(confirmation);
      assert.equal(commands.length, 2); await focusRestored(restore);
      await restore.click(); await confirmation.getByRole('button', { name: 'Restore agent', exact: true }).click(); await visible(hide);
      assert.equal(commands[2].hidden, false); assert.equal(commands[2].revision, commands[0].revision + 1);
      await visible(dialog.getByRole('button', { name: 'Done', exact: true })); await dialog.getByRole('button', { name: 'Done', exact: true }).click(); await hidden(dialog);
      assert.equal(await page.evaluate(key => sessionStorage.getItem(key), stored[0]), null);
      await profile.getByRole('button', { name: 'Cancel', exact: true }).click(); await hidden(profile);
    } finally { await page.unroute(pattern); }
  });
  await check('Advanced tool steps persist and validate in dark and light', async () => {
    await agentChat();
    await page.getByRole('button', { name: 'Edit profile', exact: true }).click();
    let dialog = page.getByRole('dialog', { name: 'Agent profile', exact: true });
    await dialog.getByRole('tab', { name: 'Advanced', exact: true }).click();
    const field = dialog.getByLabel('Tool step limit', { exact: true });
    await visible(field); assert.equal(await field.inputValue(), '90');
    assert.equal(await field.getAttribute('min'), '10'); assert.equal(await field.getAttribute('max'), '200');
    await field.fill('9'); assert.equal(await field.evaluate(element => element.validity.valid), false);
    await field.fill('10.5'); assert.equal(await field.evaluate(element => element.validity.valid), false);
    await field.fill('120'); await dialog.getByRole('button', { name: 'Save changes', exact: true }).click();
    await hidden(dialog); await page.reload(); await agentChat();
    await page.getByRole('button', { name: 'Edit profile', exact: true }).click();
    dialog = page.getByRole('dialog', { name: 'Agent profile', exact: true });
    await dialog.getByRole('tab', { name: 'Advanced', exact: true }).click();
    assert.equal(await dialog.getByLabel('Tool step limit', { exact: true }).inputValue(), '120');
    for (const theme of ['dark', 'light']) {
      await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
      await dialog.getByLabel('Tool step limit', { exact: true }).scrollIntoViewIfNeeded();
      await screenshot(`tool-budget-advanced-${theme}`);
    }
    await page.evaluate(() => { document.documentElement.dataset.theme = 'dark'; });
    await dialog.getByRole('button', { name: 'Cancel', exact: true }).click(); await hidden(dialog); await cleanPage();
  });
  await check('Computer settings distinguish slow progress from recovery and retry the same command', async () => {
    const pattern = /\/api\/agents\/[^/]+\/computer(?:\/screenshot)?$/;
    const status = { available: true, enabled: true, state: 'running', revision: 7, width: 1280, height: 800, requests: [], control: { mode: 'agent', since: new Date().toISOString(), revision: 0 } };
    const commands = []; let release, received, finished;
    const settled = new Promise(resolve => { finished = resolve; });
    const held = new Promise(resolve => { release = resolve; });
    const arrived = new Promise(resolve => { received = resolve; });
    await page.route(pattern, async route => {
      if (new URL(route.request().url()).pathname.endsWith('/screenshot')) return route.fulfill({ status: 200, contentType: 'image/png', body: await desktopImage });
      if (route.request().method() === 'GET') return route.fulfill({ json: status });
      commands.push(route.request().postDataJSON());
      if (commands.length === 1) { received(); await held; try { await route.fulfill({ status: 200, contentType: 'application/json', body: '{lost acknowledgement' }); } finally { finished(); } return; }
      status.enabled = commands.at(-1).enabled; status.revision++; status.state = 'off';
      return route.fulfill({ json: status });
    });
    try {
      await agentChat(); await page.getByRole('button', { name: 'Edit profile', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: 'Agent profile', exact: true });
      await dialog.getByRole('tab', { name: 'Advanced', exact: true }).click();
      const toggle = dialog.getByRole('switch', { name: 'Computer', exact: true });
      await visible(toggle); await visible(dialog.getByRole('button', { name: 'Save changes', exact: true })); await toggle.click(); await arrived;
      assert.equal(await dialog.getByRole('button', { name: 'Save changes', exact: true }).count(), 1);
      await captureComputerThemes('computer-setting-in-flight');
      assert.equal(await dialog.getByText(/not confirmed|Couldn.t confirm/i).count(), 0, 'Recovery warning appeared while request was still in flight');
      assert.equal(await toggle.getAttribute('aria-checked'), 'false');
      await visible(dialog.getByRole('status').filter({ hasText: 'Stopping computer...' }));
      release(); await visible(dialog.getByRole('button', { name: 'Try again', exact: true }));
      await captureComputerThemes('computer-setting-failed');
      await page.reload(); await page.getByRole('button', { name: 'Edit profile', exact: true }).click();
      await dialog.getByRole('tab', { name: 'Advanced', exact: true }).click();
      await visible(dialog.getByRole('button', { name: 'Try again', exact: true }));
      assert.equal(commands.length, 1, 'Reload replayed a saved command automatically');
      await dialog.getByRole('button', { name: 'Try again', exact: true }).click();
      await hidden(dialog.getByRole('button', { name: 'Try again', exact: true }));
      assert.deepEqual(commands[1], commands[0], 'Retry changed the saved command ID or revision');
      await cleanPage(dialog);
    } finally { release(); if (commands.length) await settled; await page.unroute(pattern); await page.reload(); }
  });
  await check('Role saves and reloads in edit profile, dark and light', async () => {
    await agentChat();
    await page.getByRole('button', { name: 'Edit profile', exact: true }).click();
    let dialog = page.getByRole('dialog', { name: 'Agent profile', exact: true });
    await visible(dialog);
    assert.equal(await dialog.getByRole('textbox', { name: /^Role/ }).inputValue(), 'Personal agent');
    assert.equal(await dialog.getByRole('textbox', { name: /^Role/ }).getAttribute('maxlength'), '60');
    await dialog.getByRole('textbox', { name: /^Role/ }).fill('Orchestration agent');
    await dialog.getByRole('button', { name: 'Save changes', exact: true }).click();
    await hidden(dialog);
    await page.reload(); await agentChat();
    await page.getByRole('button', { name: 'Edit profile', exact: true }).click();
    dialog = page.getByRole('dialog', { name: 'Agent profile', exact: true });
    await visible(dialog);
    assert.equal(await dialog.getByRole('textbox', { name: /^Role/ }).inputValue(), 'Orchestration agent');
    await dialog.evaluate(element => Promise.all(element.getAnimations().map(animation => animation.finished)));
    const role = await dialog.getByRole('textbox', { name: /^Role/ }).boundingBox();
    const name = await dialog.getByLabel('Name', { exact: true }).boundingBox();
    assert(role && name && role.y > name.y + name.height && Math.abs(role.x - name.x) < 1 && Math.abs(role.width - name.width) < 1, JSON.stringify({ role, name }));
    for (const theme of ['dark', 'light']) {
      await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
      await dialog.getByRole('textbox', { name: /^Role/ }).scrollIntoViewIfNeeded();
      await screenshot(`agent-role-edit-${theme}`);
    }
    await page.evaluate(() => { document.documentElement.dataset.theme = 'dark'; });
    await dialog.getByRole('button', { name: 'Cancel', exact: true }).click();
    await hidden(dialog); await cleanPage();
  });
  await check('SET2 profile failure stays visible and retries original payload across tabs', async () => {
    await agentChat(); const commands = [], pattern = /\/api\/agents\/[^/]+\/edit$/;
    await page.route(pattern, async route => { commands.push(route.request().postDataJSON()); const response = await route.fetch(); return route.fulfill(commands.length === 1 ? { status: 200, contentType: 'application/json', body: '{lost acknowledgement' } : { response }); });
    try {
      await page.getByRole('button', { name: 'Edit profile', exact: true }).click(); const dialog = page.getByRole('dialog', { name: 'Agent profile', exact: true });
      const instructions = dialog.getByLabel(/^Instructions/, { exact: false }); await instructions.fill((await instructions.inputValue()) + '\nRetain retry fixture.');
      await dialog.getByRole('button', { name: 'Save changes', exact: true }).click(); const error = dialog.getByRole('alert'); await visible(error);
      assert(await error.evaluate(element => { const box = element.getBoundingClientRect(), body = element.closest('.k-dialog__body').getBoundingClientRect(); return box.top >= body.top && box.bottom <= body.bottom + 1; }), 'Profile error clipped by tab container');
      assert.equal(await instructions.getAttribute('readonly'), ''); await screenshot('set2-profile-uncertain');
      await dialog.getByRole('tab', { name: 'Advanced', exact: true }).click(); await visible(dialog.getByRole('button', { name: 'Save changes', exact: true }));
      await dialog.getByRole('button', { name: 'Save changes', exact: true }).click(); await hidden(dialog); assert.deepEqual(commands[1], commands[0]);
    } finally { await page.unroute(pattern); await page.reload(); }
  });
  await check('SET2 computer loading and direct Computer entry', async () => {
    const pattern = /\/api\/agents\/[^/]+\/computer$/;
    let hold = false, release; const gate = new Promise(resolve => { release = resolve; });
    const status = { available: true, enabled: false, state: 'off', revision: 0, requests: [], width: 1280, height: 800, control: { mode: 'agent', revision: 0, since: new Date().toISOString() } };
    await page.route(pattern, async route => { if (hold) await gate; return route.fulfill({ json: status }); });
    try {
      await agentChat(); const direct = page.getByRole('region', { name: 'Computer', exact: true }).getByRole('button', { name: 'Open', exact: true }); await visible(direct);
      hold = true; await page.getByRole('button', { name: 'Edit profile', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: 'Agent profile', exact: true }); await dialog.getByRole('tab', { name: 'Advanced', exact: true }).click();
      await visible(dialog.getByText('Loading computer status...', { exact: true })); assert.equal(await dialog.getByRole('switch', { name: 'Computer', exact: true }).count(), 0);
      assert.equal(await dialog.getByText('Unavailable', { exact: true }).count(), 0, 'Unknown capability shown unavailable'); release();
      await visible(dialog.getByRole('switch', { name: 'Computer', exact: true })); await dialog.getByRole('button', { name: 'Cancel', exact: true }).click(); await hidden(dialog);
      await direct.click();
      const panel = page.getByRole('complementary', { name: 'Conversation details', exact: true });
      await visible(panel.getByRole('button', { name: 'Turn on computer', exact: true }));
      assert.equal(await panel.getByRole('tab', { name: 'Computer', exact: true }).getAttribute('aria-selected'), 'true');
      await panel.getByRole('tab', { name: 'Chats', exact: true }).click();
    } finally { release(); await page.unroute(pattern); await page.reload(); }
  });
  await check('SET2 unavailable bound account/runtime and unlisted models stay intact', async () => {
    const directory = '**/api/agents', runtimes = /\/api\/agents\/[^/]+\/runtimes$/, catalog = '**/api/models?runtime=*&account=missing-fixture-account';
    await page.route(catalog, route => route.fulfill({ json: { models: [] } }));
    await page.route(directory, async route => {
      if (route.request().method() !== 'GET') return route.continue();
      const response = await route.fetch(), data = await response.json();
      data.agents = data.agents.map(agent => agent.name === lead ? { ...agent, runtime: 'codex', provider_account: 'missing-fixture-account', model: 'unlisted-primary', backup_model: 'unlisted-fallback' } : agent);
      return route.fulfill({ response, json: data });
    });
    await page.route(runtimes, route => route.fulfill({ json: { choices: [{ runtime: 'codex', label: 'Codex', available: false, enabled: false, version: null, reason: 'Not installed' }, { runtime: 'claude', label: 'Claude Code', available: false, enabled: false, version: null, reason: 'Not installed' }] } }));
    try {
      await agentChat(); await page.getByRole('button', { name: 'Edit profile', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: 'Agent profile', exact: true }); await dialog.getByRole('tab', { name: 'Model', exact: true }).click();
      const runtime = dialog.getByRole('combobox', { name: 'Run with', exact: true }); assert.match(await runtime.textContent(), /Codex/);
      const model = dialog.getByRole('textbox', { name: 'Model', exact: true }); assert.equal(await model.inputValue(), 'unlisted-primary'); assert.equal(await model.getAttribute('readonly'), null);
      assert.equal(await model.getAttribute('placeholder'), 'Enter model ID');
      await runtime.click(); assert.equal(await page.getByRole('option', { name: /^Codex/ }).getAttribute('aria-disabled'), 'true'); await page.getByRole('option', { name: 'API model', exact: true }).click();
      const provider = dialog.getByRole('combobox', { name: 'Provider', exact: true }); assert.match(await provider.textContent(), /Unavailable account/); await provider.click(); await visible(page.getByText('missing-fixture-account', { exact: true })); await page.keyboard.press('Escape');
      assert.equal(await model.inputValue(), 'unlisted-primary'); assert.equal(await model.getAttribute('readonly'), null);
      const values = await dialog.locator('form').evaluate(form => Object.fromEntries(new FormData(form))); assert.equal(values.backupModel, 'unlisted-fallback');
      await screenshot('set2-unavailable-bound-model'); await dialog.getByRole('button', { name: 'Cancel', exact: true }).click(); await hidden(dialog);
    } finally { await page.unrouteAll({ behavior: 'wait' }); await page.reload(); }
  });
  await check('SET2 avatar saves independently, exact retry and guarded removal', async () => {
    await agentChat();
    const commands = [], pattern = /\/api\/agents\/[^/]+\/avatar$/;
    await page.route(pattern, async route => {
      if (route.request().method() === 'GET') return route.continue();
      commands.push(route.request().postDataJSON()); const response = await route.fetch();
      return route.fulfill(commands.length === 1 ? { status: 200, contentType: 'application/json', body: '{lost acknowledgement' } : { response });
    });
    try {
      await page.getByRole('button', { name: 'Edit profile', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: 'Agent profile', exact: true }); await dialog.getByLabel('Name', { exact: true }).fill('Unsaved name');
      await dialog.getByRole('button', { name: 'Change', exact: true }).click();
      const avatar = page.getByRole('dialog', { name: 'Avatar', exact: true }); await visible(avatar);
      await avatar.getByRole('radio', { name: 'Pebble', exact: true }).click(); await avatar.getByRole('button', { name: 'Save', exact: true }).click();
      const retry = avatar.getByRole('button', { name: 'Try again', exact: true }); await visible(retry);
      assert.equal(commands[0].revision, 0); assert.match(commands[0].image, /^iVBOR/); assert.deepEqual(Object.keys(commands[0]).sort(), ['id', 'image', 'revision']);
      const stored = await page.evaluate(() => Object.entries(sessionStorage).find(([key]) => key.startsWith('minarbot-avatar:'))); assert.deepEqual(JSON.parse(stored[1]), commands[0]);
      await screenshot('set2-avatar-recovery'); await retry.click(); await visible(avatar.getByText('Avatar updated.', { exact: true })); assert.deepEqual(commands[1], commands[0]);
      const remove = avatar.getByRole('button', { name: 'Remove avatar', exact: true }); await remove.click();
      const confirmation = page.getByRole('alertdialog', { name: 'Remove avatar?', exact: true }); await visible(confirmation); await confirmation.getByRole('button', { name: 'Cancel', exact: true }).click(); await hidden(confirmation); await focusRestored(remove); assert.equal(commands.length, 2);
      await remove.click(); await confirmation.getByRole('button', { name: 'Remove avatar', exact: true }).click(); await page.waitForFunction(() => !Object.keys(sessionStorage).some(key => key.startsWith('minarbot-avatar:')));
      assert.equal(commands[2].revision, 1); assert.equal(commands[2].image, null);
      await avatar.getByRole('button', { name: 'Done', exact: true }).click(); await hidden(avatar); await focusRestored(dialog.getByRole('button', { name: 'Change', exact: true }));
      assert.equal(await dialog.getByLabel('Name', { exact: true }).inputValue(), 'Unsaved name');
      await dialog.getByRole('button', { name: 'Cancel', exact: true }).click(); await hidden(dialog); await page.reload(); await agentChat();
    } finally { await page.unroute(pattern); }
  });
  await check('SET2 independent sites, exact commands, delete confirmation and editor focus', async () => {
    await agentChat();
    const details = page.getByRole('complementary', { name: 'Conversation details', exact: true });
    const show = page.getByRole('button', { name: 'Show details', exact: true }); if (await show.isVisible()) await show.click();
    await details.getByRole('button', { name: 'Edit profile', exact: true }).click();
    const profile = page.getByRole('dialog', { name: 'Agent profile', exact: true });
    await profile.getByRole('tab', { name: 'Advanced', exact: true }).click();
    const trigger = profile.getByRole('button', { name: 'Agent sites', exact: true }); await trigger.click();
    const manager = page.getByRole('dialog', { name: 'Agent sites', exact: true });
    await visible(manager.getByText('No sites', { exact: true }));
    const add = manager.getByRole('button', { name: 'Create site', exact: true }); await add.click();
    const editor = page.getByRole('dialog', { name: 'New site', exact: true }); await visible(editor);
    await editor.getByLabel('Name', { exact: true }).fill('Profile fixture'); await editor.getByLabel('HTML', { exact: true }).fill('<p>Profile fixture</p>');
    const created = page.waitForRequest(request => /\/api\/agents\/[^/]+\/sites$/.test(new URL(request.url()).pathname) && request.method() === 'POST');
    await editor.getByRole('button', { name: 'Create site', exact: true }).click();
    const command = (await created).postDataJSON(); const { id, ...payload } = command; assert.match(id, /^[a-f0-9-]{36}$/i); assert.deepEqual(payload, { name: 'Profile fixture', source: '<p>Profile fixture</p>' });
    await page.waitForFunction(() => document.querySelector('#site-editor input').value === '');
    await editor.locator('.k-dialog__foot').getByRole('button', { name: 'Close', exact: true }).click(); await hidden(editor); await focusRestored(add);
    const edit = manager.getByRole('button', { name: 'Edit Profile fixture', exact: true }); await edit.click();
    const replacement = page.getByRole('dialog', { name: 'Replace site contents', exact: true }); await visible(replacement);
    const remove = replacement.getByRole('button', { name: 'Delete site', exact: true }); await remove.click();
    const confirmation = page.getByRole('alertdialog', { name: 'Delete Profile fixture?', exact: true }); await visible(confirmation);
    await confirmation.getByRole('button', { name: 'Cancel', exact: true }).click(); await hidden(confirmation); await focusRestored(remove);
    assert.equal(await replacement.getByRole('checkbox').count(), 0, 'Deletion still needs standing consent checkbox');
    await remove.click(); const deleted = page.waitForRequest(request => /\/api\/agents\/[^/]+\/sites$/.test(new URL(request.url()).pathname) && request.method() === 'POST');
    await confirmation.getByRole('button', { name: 'Delete site', exact: true }).click(); const deletion = (await deleted).postDataJSON(); assert.equal(deletion.delete, true); assert.equal(deletion.revision, 1); assert.deepEqual(Object.keys(deletion).sort(), ['delete', 'id', 'revision', 'site']);
    await page.waitForFunction(() => document.querySelector('#site-editor input').value === ''); await page.getByRole('dialog', { name: 'New site', exact: true }).locator('.k-dialog__foot').getByRole('button', { name: 'Close', exact: true }).click(); await hidden(page.getByRole('dialog', { name: 'New site', exact: true })); await visible(manager.getByText('No sites', { exact: true }));
    await manager.getByRole('button', { name: 'Create site', exact: true }).focus(); await hidden(page.getByRole('tooltip')); await page.keyboard.press('Escape'); await hidden(manager); await focusRestored(trigger);
    await profile.getByRole('button', { name: 'Cancel', exact: true }).click(); await hidden(profile);
    await page.getByRole('main', { name: `Conversation ${lead}`, exact: true }).locator('.chat__pill').click(); await page.getByRole('menuitem', { name: 'Export template', exact: true }).click();
    const exported = page.getByRole('dialog', { name: `${lead} · Export template`, exact: true }); await visible(exported);
    await exported.getByRole('button', { name: 'Preview export template', exact: true }).click(); await visible(exported.getByRole('button', { name: 'Download reviewed template', exact: true }));
    await page.keyboard.press('Escape'); await hidden(exported);
    await details.getByRole('tab', { name: 'Chats', exact: true }).click();
  });
  if (process.env.MINARBOT_SMOKE_PROFILE_ONLY === '1') throw new Error('SMOKE_STOP_AFTER');
  await check('Enter sends direct message and completes Markdown reply', async () => { await agentChat(); await send(lead, 'Direct smoke message'); });
  await timelineMinimapSmoke({ check, page, origin, root, agentChat });
  await acpRuntimeSmoke({ page, check, origin, root, agentChat });
  await runControlsSmoke({ check, page, origin, visible, hidden, agentChat });
  await streamActionsSmoke({ check, page, origin, screenshot, releaseStream, releaseRoom, releaseRoomDelivery, streamReply: 'Stable first block.\n\nGrowing second block' + streamReply.slice('Stable first block.\n\n'.length), agentChat, visible, hidden });
  await deleteAgentSmoke({ page, check, origin, root, receipt });
  await roomControlsSmoke({ check, page, origin, screenshot, visible, hidden, agentChat, release: model => hrcGates.get(model)?.() });
  await r4SettingsSmoke({ check, page, origin, root, agentChat, settings, openNew });
  await polishSmoke({ check, page, origin, root, agentChat });
  await agentChat();
  await check('Voice defaults off: no reply controls or inline picker', async () => {
    await agentChat();
    assert.equal(await page.evaluate(() => localStorage.getItem('minarbot.speech.enabled.v1')), null);
    assert.equal(await page.getByRole('button', { name: 'Listen', exact: true }).count(), 0);
    assert.equal(await page.getByRole('button', { name: 'Listen to answer', exact: true }).count(), 0);
    assert.equal(await page.getByRole('combobox', { name: 'Voice', exact: true }).count(), 0);
  });
  await check('Voice opt-in: saved local voice, test, live reply action, dark and light', async () => {
    const sibling = await page.context().newPage();
    try {
      await sibling.goto(origin);
      await sibling.getByRole('navigation', { name: 'Choose agent', exact: true }).getByRole('button', { name: new RegExp(`^${lead} `) }).click();
      await visible(sibling.getByRole('main', { name: `Conversation ${lead}`, exact: true }).getByText('Smoke reply:', { exact: false }));
      assert.equal(await sibling.getByRole('button', { name: 'Listen', exact: true }).count(), 0);
      // Keep original saved-voice format; enabling must retain it.
      await sibling.evaluate(() => localStorage.setItem('minarbot.speech.voice.v1', JSON.stringify(['smoke-local', 'Smoke local voice', 'en-US'])));
      const panel = await settings('General');
      const toggle = panel.getByRole('switch', { name: 'Read aloud', exact: true });
      assert.equal(await toggle.getAttribute('aria-checked'), 'false');
      assert.equal(await panel.getByRole('combobox', { name: 'Voice', exact: true }).count(), 0);
      await toggle.click();
      await visible(sibling.getByRole('button', { name: 'Listen', exact: true }));
      const picker = panel.getByRole('combobox', { name: 'Voice', exact: true });
      await visible(picker); assert.match(await picker.textContent(), /Smoke local voice/);
      await picker.click(); await visible(page.getByRole('option', { name: 'Smoke local voice (en-US)', exact: true }));
      assert.equal(await page.getByRole('option', { name: /cloud/ }).count(), 0); await page.keyboard.press('Escape');
      assert.equal(await page.evaluate(() => window.__speechSmoke.spoken.length), 0, 'Enabling voice must not autoplay');
      await panel.getByRole('button', { name: 'Test voice', exact: true }).click();
      await visible(panel.getByRole('button', { name: 'Stop voice test', exact: true }));
      assert.equal(await page.evaluate(() => window.__speechSmoke.spoken.at(-1).voice.localService), true);
      await panel.getByRole('button', { name: 'Stop voice test', exact: true }).click();
      for (const mode of ['dark', 'light']) {
        await page.evaluate(mode => { document.documentElement.dataset.theme = mode; }, mode);
        await panel.getByRole('heading', { name: 'Voice', exact: true }).scrollIntoViewIfNeeded();
        await screenshot(`voice-settings-${mode}`);
        const section = panel.locator('section[aria-labelledby="voice-settings"]');
        const path = join(output, `voice-settings-${mode}.png`); await section.screenshot({ path }); receipt.screenshots.push(path);
      }
      await panel.getByRole('button', { name: 'Close settings', exact: true }).click();
      const listen = page.getByRole('button', { name: 'Listen', exact: true });
      await visible(listen);
      assert.equal(await listen.locator('xpath=ancestor::*[contains(@class,"turn-controls")]').count(), 1);
      for (const mode of ['dark', 'light']) {
        await page.evaluate(mode => { document.documentElement.dataset.theme = mode; }, mode);
        await listen.scrollIntoViewIfNeeded(); await screenshot(`voice-reply-${mode}`);
      }
      await listen.hover(); await visible(page.getByRole('tooltip', { name: 'Listen to answer', exact: true }));
      await listen.click(); await visible(page.getByRole('button', { name: 'Stop', exact: true }));
      await page.getByRole('button', { name: 'Stop', exact: true }).click(); await visible(listen);
      const appearance = await settings('General'); await appearance.getByRole('switch', { name: 'Read aloud', exact: true }).click();
      await hidden(sibling.getByRole('button', { name: 'Listen', exact: true }));
      await appearance.getByRole('button', { name: 'Close settings', exact: true }).click();
      assert.equal(await listen.count(), 0); assert.equal(await page.locator('.speech-playback').count(), 0);
      await cleanPage();
    } finally { await sibling.close(); }
  });
  await check('Voice missing selection and speech errors stay inline; opt-out stops playback', async () => {
    const panel = await settings('General');
    await panel.getByRole('switch', { name: 'Read aloud', exact: true }).click();
    await panel.getByRole('button', { name: 'Close settings', exact: true }).click();
    const sibling = await page.context().newPage();
    try {
      await sibling.goto(origin);
      await sibling.evaluate(() => localStorage.removeItem('minarbot.speech.voice.v1'));
      await page.waitForFunction(() => localStorage.getItem('minarbot.speech.voice.v1') === null);
      const listen = page.getByRole('button', { name: 'Listen', exact: true }); await listen.click();
      await visible(page.getByText('Pick a voice in Settings > General.', { exact: true }));
      assert.equal(await page.getByRole('combobox', { name: 'Voice', exact: true }).count(), 0);
      await sibling.evaluate(() => localStorage.setItem('minarbot.speech.voice.v1', JSON.stringify(['smoke-local', 'Smoke local voice', 'en-US'])));
      await hidden(page.getByText('Pick a voice in Settings > General.', { exact: true }));
      await page.evaluate(() => { window.__speechSmoke.fail = true; }); await listen.click();
      await visible(page.getByText('Reading stopped. Check device audio and try again.', { exact: true }));
      assert.equal(await page.getByRole('alert').count(), 0);
      await page.evaluate(() => { window.__speechSmoke.fail = false; }); await listen.click();
      await visible(page.getByRole('button', { name: 'Stop', exact: true }));
      const cancels = await page.evaluate(() => window.__speechSmoke.cancelled);
      await sibling.evaluate(() => localStorage.setItem('minarbot.speech.enabled.v1', 'false'));
      await hidden(page.locator('.speech-playback'));
      assert(await page.evaluate(previous => window.__speechSmoke.cancelled > previous, cancels));
    } finally { await sibling.close(); }
  });
  await check('Voice unavailable: missing local voices or speech API hides row', async () => {
    const panel = await settings('General');
    await page.evaluate(() => { window.__speechSmoke.voices = window.__speechSmoke.voices.filter(voice => !voice.localService); speechSynthesis.dispatchEvent(new Event('voiceschanged')); });
    await hidden(panel.getByRole('switch', { name: 'Read aloud', exact: true }));
    assert.equal(await panel.getByText('No local voices', { exact: true }).count(), 0);
    await page.evaluate(() => { window.__speechSmoke.voices.push({ voiceURI: 'smoke-local', name: 'Smoke local voice', lang: 'en-US', localService: true, default: false }); speechSynthesis.dispatchEvent(new Event('voiceschanged')); });
    await page.waitForFunction(() => !document.querySelector('[aria-label="Read aloud"]').disabled);
    const unsupported = await page.context().newPage();
    try {
      await unsupported.addInitScript(() => { delete window.speechSynthesis; delete window.SpeechSynthesisUtterance; });
      await unsupported.goto(origin);
      await unsupported.getByRole('button', { name: /^Settings(?: - Update available)?$/ }).click();
      await unsupported.getByRole('button', { name: 'General', exact: true }).click();
      assert.equal(await unsupported.getByRole('switch', { name: 'Read aloud', exact: true }).count(), 0);
      assert.equal(await unsupported.getByText('Unavailable', { exact: true }).count(), 0);
    } finally { await unsupported.close(); }
    await page.evaluate(() => { document.documentElement.dataset.theme = 'dark'; });
    await panel.getByRole('button', { name: 'Close settings', exact: true }).click();
  });
  await check('Create second agent', async () => {
    const path = '**/api/agents';
    const setHelperId = route => {
      const request = route.request();
      if (request.method() !== 'POST') return route.fallback();
      // Keep the delegation recipient past the first roster page.
      return route.fallback({ postData: { ...request.postDataJSON(), id: 'ffffffff-ffff-4fff-bfff-ffffffffffff' } });
    };
    await page.route(path, setHelperId);
    try { await createAgent(helper); }
    finally { await page.unroute(path, setHelperId); }
  });
  await check('Create quiet reviewer', () => createAgent(quiet));
  await check('Migrated chatroom appears as project with original history', async () => {
    const agents=await fetch(origin+'/api/agents').then(r=>r.json());
    await page.goto('about:blank');
    const ready=once(app,'message');app.send({kind:'seed-legacy-room',agent:agents.agents.find(a=>a.name===lead).id});
    const [result]=await ready;assert.equal(result.kind,'legacy-room-ready',JSON.stringify(result));
    await workspace();
    await sidebarTab('Projects');
    await visible(page.getByRole('button',{name:'Migrated chatroom',exact:true}));
    await page.getByRole('button',{name:/^# general in Migrated chatroom/}).click();
    await visible(page.getByText('Original room history stays here.',{exact:true}));
    const projects=await fetch(origin+'/api/projects').then(r=>r.json());
    const project=projects.projects.find(p=>p.name==='Migrated chatroom');
    assert.equal(project.sections[0].id,result.room);
    const history=await fetch(origin+`/api/channels/${result.room}/turns`).then(r=>r.json());assert.equal(history[0].id,result.turn);
  });
  await check('Create project without folder and three members', async () => {
    await workspace();
    const dialog=await openNew('New project');
    await dialog.getByLabel('Project name',{exact:true}).fill(projectName);
    await dialog.getByRole('textbox',{name:/^Description/}).fill('Local smoke collaboration.');
    // The first member owns the root: helper passes before the lead can invite it.
    for(const name of [helper,lead,quiet]){await dialog.getByRole('combobox',{name:'Add project member',exact:true}).click();await page.getByRole('option',{name,exact:true}).click();}
    await screenshot('project-dialog-without-folders');
    await dialog.getByRole('button',{name:'Create project',exact:true}).click();await hidden(dialog);
    await visible(page.getByRole('main',{name:`Conversation ${channel}`,exact:true}));
    const project=(await fetch(origin+'/api/projects').then(r=>r.json())).projects.find(p=>p.name===projectName);
    assert.equal(project.folders.length,0);assert.equal(project.members.length,3);assert.equal(project.sections[0].name,'general');
    await visible(page.getByRole('textbox',{name:'Message #general',exact:true}));
  });
  await check('Channel room delivery completes reply', async () => { await workspace(); await sidebarTab('Projects'); await page.getByRole('button', { name: new RegExp(`^# general in ${projectName}`) }).click(); await send(channel, 'Channel smoke message'); });
  await check('Room mention autocomplete is keyboard accessible', async () => {
    const chat = page.getByRole('main', { name: `Conversation ${channel}`, exact: true });
    const box = chat.getByRole('textbox', { name: 'Message #general', exact: true });
    await box.fill('@Smoke');
    await visible(chat.getByRole('listbox', { name: 'Mention a section member', exact: true }));
    await box.press('ArrowDown'); await box.press('Enter');
    assert((await box.inputValue()).startsWith('@Smoke ')); await box.fill('');
  });
  await check('Room agents converse inline and silent reviewer stays in activity only', async () => {
    const chat = page.getByRole('main', { name: `Conversation ${channel}`, exact: true });
    const box = chat.getByRole('textbox', { name: 'Message #general', exact: true });
    await box.fill('Room discussion smoke'); await box.press('Enter');
    const turn = chat.locator('.turn-block').filter({ hasText: 'Room discussion smoke' });
    await visible(turn.getByText('Plan checked. Ready for the next step.', { exact: true }));
    await page.waitForFunction(() => !document.querySelector('.room-pending'));
    assert.deepEqual(receipt.roomDiscussion.filter(call => call.agent === helper).map(call => [call.invited, call.content]), [[false, 'NO_REPLY'], [true, 'Plan checked. Ready for the next step.']]);
    assert(receipt.roomDiscussion.filter(call => call.agent === quiet).every(call => call.content === 'NO_REPLY'));
    assert.equal(await turn.locator('.room-conversation .message-name').allTextContents().then(names => names.join(',')), `${lead},${helper}`);
    assert.equal(await turn.locator('.room-conversation').getByText('NO_REPLY', { exact: true }).count(), 0);
    await turn.getByLabel(`Reply from ${helper}`, { exact: true }).locator('.room-agent__status').click();
    await visible(turn.getByText(`${quiet} stayed quiet`, { exact: true }).first());
    await turn.locator('.room-conversation .room-agent').last().scrollIntoViewIfNeeded();
    for (const theme of ['dark', 'light']) {
      await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
      await screenshot(`room-agent-conversation-${theme}`);
    }
    await cleanPage(chat);
  });
  await check('Details direct and channel screenshots, dark and light', async () => {
    for (const kind of ['direct', 'channel']) {
      await workspace();
      if (kind === 'direct') await agentChat();
      else { await sidebarTab('Projects'); await page.getByRole('button', { name: new RegExp(`^# general in ${projectName}`) }).click(); }
      const show = page.getByRole('button', { name: 'Show details', exact: true });
      if (await show.isVisible()) await show.click();
      const details = page.getByRole('complementary', { name: 'Conversation details', exact: true });
      await visible(details);
      const label = kind === 'direct' ? 'Chats' : 'Members';
      const tabs = details.getByRole('tab', { name: label, exact: true });
      const phase = 'after';
      await visible(tabs); await tabs.click();
      assert.equal(await tabs.getAttribute('aria-selected'), 'true');
      assert.deepEqual(await details.getByRole('tablist', { name: 'Conversation panel', exact: true }).getByRole('tab').allTextContents(), [label, 'Files']);
      assert.equal(await details.getByRole('tabpanel').count(), 1);

      await hidden(details.getByRole('button', { name: 'History summary', exact: true }));
      await hidden(details.getByRole('button', { name: 'Message queue', exact: true }));
      await hidden(details.getByRole('button', { name: 'Start computer', exact: true }));
      await hidden(details.getByRole('button', { name: 'Stop computer', exact: true }));
      if (kind === 'direct') {
        await visible(details.locator('.details__who-sub'));
        assert.notEqual((await details.locator('.details__who-sub').textContent())?.trim(), '', 'Direct identity role is empty');
        await hidden(details.getByRole('region', { name: 'Computer', exact: true }));
      } else {
        assert.equal(await details.getByRole('list', { name: 'Section members', exact: true }).getByRole('listitem').count(), 3);
        await hidden(details.getByRole('region', { name: 'Computer', exact: true }));
        assert.equal(await details.getByRole('button', { name: 'Mark as read', exact: true }).count(), 0);
      }
      for (const theme of ['dark', 'light']) {
        await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
        await page.evaluate(() => { document.querySelector('.details__scroll, .right-panel__members')?.scrollTo(0, 0); });
        await screenshot(`details-${phase}-${kind}-${theme}`);
        const path = join(output, `details-${phase}-${kind}-${theme}.png`);
        await details.screenshot({ path, animations: 'disabled' });
        receipt.screenshots.push(path);
      }
    }
  });
  await check('Color themes: six palettes, two modes, System updates, cross-tab sync and screenshots', async () => {
    const themes = ['Minarbot', 'GitHub', 'Catppuccin', 'Everforest', 'Solarized', 'Midnight'];
    receipt.themeTokens = [];
    for (const mode of ['dark', 'light']) {
      for (const theme of themes) {
        const appearance = await settings('Appearance');
        await appearance.getByRole('radio', { name: mode === 'dark' ? 'Dark' : 'Light', exact: true }).click();
        await appearance.getByRole('button', { name: theme, exact: true }).click();
        await page.waitForFunction(({ theme, mode }) => document.documentElement.dataset.colorTheme === theme.toLowerCase() && document.documentElement.dataset.theme === mode, { theme, mode });
        assert.equal(await appearance.getByRole('button', { name: theme, exact: true }).getAttribute('aria-pressed'), 'true');
        if (theme === 'Midnight') await screenshot(`theme-picker-${mode}`);
        await appearance.getByRole('button', { name: 'Close settings', exact: true }).click();
        await agentChat();
        const show = page.getByRole('button', { name: 'Show details', exact: true });
        if (await show.isVisible()) await show.click();
        await visible(page.getByRole('complementary', { name: 'Conversation details', exact: true }));
        const tokens = await page.evaluate(() => {
          const css = getComputedStyle(document.documentElement);
          return Object.fromEntries(['--color-bg-chrome', '--color-bg-sidebar', '--color-bg-pane', '--color-brand', '--color-text-primary', '--color-border-focus'].map(name => [name, css.getPropertyValue(name).trim()]));
        });
        receipt.themeTokens.push({ theme, mode, tokens });
        assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), 'Theme introduced horizontal overflow');
        await screenshot(`theme-${theme}-${mode}`);
        await cleanPage();
      }
    }
    assert.equal(new Set(receipt.themeTokens.map(value => value.tokens['--color-brand'])).size, 12, 'Palette accents did not change across themes/modes');
    const appearance = await settings('Appearance');
    await appearance.getByRole('radio', { name: 'System', exact: true }).click();
    for (const theme of themes) {
      await appearance.getByRole('button', { name: theme, exact: true }).click();
      const backgrounds = [];
      for (const colorScheme of ['light', 'dark']) {
        await page.emulateMedia({ colorScheme });
        const expected = receipt.themeTokens.find(item => item.theme === theme && item.mode === colorScheme).tokens['--color-bg-pane'];
        await page.waitForFunction(expected => getComputedStyle(document.documentElement).getPropertyValue('--color-bg-pane').trim() === expected, expected);
        backgrounds.push(expected);
      }
      assert.notEqual(backgrounds[0], backgrounds[1], `${theme}: System did not follow live device appearance`);
    }
    const sibling = await page.context().newPage();
    try {
      await sibling.goto(origin);
      await sibling.waitForFunction(() => document.documentElement.dataset.colorTheme === 'midnight');
      await appearance.getByRole('button', { name: 'Minarbot', exact: true }).click();
      await sibling.waitForFunction(() => document.documentElement.dataset.colorTheme === 'minarbot');
    } finally { await sibling.close(); }
    await appearance.getByRole('radio', { name: 'Dark', exact: true }).click();
    await appearance.getByRole('button', { name: 'Close settings', exact: true }).click();
  });
  await check('SET1 grouped settings and preferences', async () => {
    await workspace();
    await page.keyboard.press('Control+,');
    let dialog = page.getByRole('dialog', { name: 'Settings', exact: true });
    await visible(dialog.getByRole('heading', { name: 'General', exact: true }));
    const groups = { Preferences: ['General', 'Appearance'], Agents: ['Skills'], Connections: ['Providers', 'Engines', 'Connected apps', 'Email', 'MCP connections', 'Vault'], System: ['Machines', 'Devices', 'Computer', 'Updates'], Data: ['Usage', 'Shared data', 'Backup & restore'], Advanced: ['External access'] };
    const nav = dialog.getByRole('navigation', { name: 'Settings', exact: true });
    assert.deepEqual(await nav.getByRole('heading').allTextContents(), Object.keys(groups));
    for (const [group, labels] of Object.entries(groups)) assert.deepEqual(await nav.getByRole('group', { name: group, exact: true }).getByRole('button').allTextContents(), labels);
    for (const name of ['General', 'Appearance']) {
      await nav.getByRole('button', { name, exact: true }).focus(); await page.keyboard.press('Enter');
      await page.waitForFunction(name => document.activeElement?.tagName === 'H1' && document.activeElement.textContent === name, name);
      assert.equal(await nav.getByRole('button', { name, exact: true }).getAttribute('aria-current'), 'page');
      const content = dialog.locator('.app-settings__page');
      assert.equal(await content.locator('.k-page-head p').count(), 0);
      assert.equal(await content.locator('p, .k-row__meta, .k-field__hint').filter({ hasText: /.{100}/ }).count(), 0);
      if (name === 'General') {
        assert.deepEqual(await content.getByRole('heading', { level: 2 }).allTextContents(), ['Notifications', 'Voice', 'Conversations', 'Web search']);
        for (const label of ['Agent and workflow notifications', 'Read aloud', 'Auto-archive agent deliveries']) await visible(content.getByRole('switch', { name: label, exact: true }));
        const archive = content.getByRole('switch', { name: 'Auto-archive agent deliveries', exact: true });
        const previous = await archive.getAttribute('aria-checked') === 'true';
        for (const expected of [!previous, previous]) {
          const request = page.waitForRequest(request => request.url() === origin + '/api/conversations/settings' && request.method() === 'POST');
          await archive.click(); const body = (await request).postDataJSON(); assert.deepEqual(Object.keys(body).sort(), ['autoArchive', 'id']); assert.match(body.id, /^[0-9a-f-]{36}$/); assert.equal(body.autoArchive, expected);
          await page.waitForFunction(expected => document.querySelector('[aria-label="Auto-archive agent deliveries"]').getAttribute('aria-checked') === String(expected) && !document.querySelector('[aria-label="Auto-archive agent deliveries"]').disabled, expected);
        }
        await page.keyboard.press('Tab'); await content.getByRole('button', { name: 'About auto-archive', exact: true }).focus(); await visible(page.getByRole('tooltip').first());
      } else { assert.equal(await content.getByRole('switch').count(), 0); assert.equal(await content.locator('.color-theme-card').count(), 6); }
      for (const viewport of [{ width: 1440, height: 900 }, { width: 1024, height: 768 }]) {
        await page.setViewportSize(viewport);
        for (const [palette, theme] of [['minarbot', 'dark'], ['minarbot', 'light'], ['catppuccin', 'dark']]) {
          await page.evaluate(({ palette, theme }) => { document.documentElement.dataset.colorTheme = palette; document.documentElement.dataset.theme = theme; }, { palette, theme });
          const bounds = await dialog.locator('.app-settings__nav').boundingBox(); assert.equal(bounds.width, 240);
          assert(await content.evaluate(el => el.scrollWidth <= el.clientWidth + 1));
          for (const option of await content.getByRole('radio').all()) assert(await option.evaluate(el => el.scrollWidth <= el.clientWidth + 1), 'Preference option text overflows');
          assert(await content.evaluate(el => el.getBoundingClientRect().bottom <= innerHeight), `${name} exceeds first viewport`);
          await screenshot(`set1-${name}-${palette}-${theme}-${viewport.width}x${viewport.height}`);
        }
      }
    }
    await page.setViewportSize(receipt.viewport);
    for (const size of ['Compact', 'Default', 'Large', 'Larger']) { await dialog.getByRole('radio', { name: size, exact: true }).click(); assert(await dialog.evaluate(el => el.scrollWidth <= el.clientWidth + 1)); }
    await dialog.getByRole('radio', { name: 'Default', exact: true }).click(); await dialog.getByRole('radio', { name: 'Dark', exact: true }).click();
    await focusContained(dialog); await page.keyboard.press('Escape'); await hidden(dialog);
    const imported = await openNew('Import template'); await visible(imported.getByRole('button', { name: 'Choose file', exact: true })); assert.equal(await imported.getByRole('heading', { name: 'Import template', exact: true }).count(), 1); await page.waitForFunction(() => document.activeElement?.textContent === 'Choose file'); await page.keyboard.press('Escape'); await hidden(imported);
    await page.getByRole('button', { name: /^Settings(?: - Update available)?$/ }).click(); dialog = page.getByRole('dialog', { name: 'Settings', exact: true }); await visible(dialog.getByRole('heading', { name: 'General', exact: true }));
    await cleanPage(dialog);
  });
  await check('SET1 failed lazy page retains keyboard focus through refresh', async () => {
    const failed = await page.context().newPage();
    try {
      await failed.route('**/GeneralSettings-*.js', route => route.fulfill({ contentType: 'application/javascript', body: 'throw new Error("Synthetic preference module failure");' }));
      await failed.goto(origin); await visible(failed.getByRole('button', { name: /^Settings(?: - Update available)?$/ }));
      await failed.getByRole('button', { name: /^Settings(?: - Update available)?$/ }).click();
      await failed.waitForFunction(() => document.activeElement?.textContent === 'Panel could not be loaded');
      const reload = failed.getByRole('button', { name: 'Reload app', exact: true }); await reload.focus();
      await failed.waitForResponse(response => new URL(response.url()).pathname === '/api/agents' && response.ok());
      await failed.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      assert(await reload.evaluate(el => el === document.activeElement), 'Background refresh stole recovery-action focus');
      await failed.keyboard.press('Escape'); await hidden(failed.getByRole('dialog', { name: 'Settings', exact: true }));
    } finally { await failed.close(); }
  });
  for (const [section, heading] of [
    ['Machines', 'Machines'], ['Updates', 'Updates'], ['Skills', 'Skills'], ['Connected apps', 'Connected apps'], ['MCP connections', 'MCP connections'], ['External access', 'External access'], ['Shared data', 'Shared data'], ['Usage', 'Usage'],
    ['General', 'General'], ['Backup & restore', 'Backup & restore'], ['Appearance', 'Appearance'],
  ]) await check(`Settings: ${section}`, async () => {
    const consoleStart = receipt.consoleErrors.length, pageStart = receipt.pageErrors.length;
    const dialog = await settings(section, heading);
    if (section === 'Connected apps') {
      await visible(dialog.getByText('Connect apps', { exact: true }));
      await visible(dialog.getByRole('button', { name: 'Set up', exact: true }));
    }
    await cleanPage(dialog);
    assert.equal(receipt.consoleErrors.length, consoleStart, 'Settings emitted console errors');
    assert.equal(receipt.pageErrors.length, pageStart, 'Settings emitted uncaught errors');
  });
  await check('Sidebar tabs follow routes, persist on neutral routes and keep independent search', async () => {
    await workspace(); await agentChat(); await sidebarTab('Projects');
    await page.reload(); await workspace();
    const sidebar = page.getByRole('complementary', { name: 'Workspace navigation', exact: true });
    assert.equal(await sidebar.getByRole('tab', { name: /^Agents/ }).getAttribute('aria-selected'), 'true');
    await page.getByRole('button', { name: 'Routines', exact: true }).click();
    await page.waitForURL('**/routines');
    await sidebarTab('Projects'); await page.reload();
    await page.getByRole('heading', { name: 'Routines', exact: true }).waitFor();
    assert.equal(await sidebar.getByRole('tab', { name: /^Projects/ }).getAttribute('aria-selected'), 'true');
    assert.equal(await page.evaluate(() => localStorage.getItem('minarbot:sidebar-tab')), 'projects');
    await hidden(sidebar.getByRole('button', { name: 'Filter sidebar', exact: true }));
    await page.getByRole('textbox', { name: 'Search projects', exact: true }).fill('Migrated');
    await visible(sidebar.getByRole('button', { name: 'Migrated chatroom', exact: true }));
    await hidden(sidebar.getByRole('button', { name: projectName, exact: true }));
    for (const modifier of ['Control', 'Meta']) {
      await page.keyboard.press(`${modifier}+1`);
      assert.equal(await sidebar.getByRole('tab', { name: /^Agents/ }).getAttribute('aria-selected'), 'true');
      assert.equal(await page.getByRole('textbox', { name: 'Search agents', exact: true }).inputValue(), '');
      await visible(sidebar.getByRole('button', { name: new RegExp(`^${lead} `) }));
      await page.getByRole('textbox', { name: 'Search agents', exact: true }).fill('Personal agent');
      await hidden(sidebar.getByRole('button', { name: new RegExp(`^${lead} `) }));
      await page.keyboard.press(`${modifier}+2`);
      assert.equal(await page.getByRole('textbox', { name: 'Search projects', exact: true }).inputValue(), 'Migrated');
      await visible(sidebar.getByRole('button', { name: 'Migrated chatroom', exact: true }));
      await sidebar.getByRole('tab', { name: /^Projects/ }).focus();
      await page.keyboard.press('ArrowLeft');
      await page.waitForFunction(() => document.querySelector('.sidebar__tabs [role="tab"][data-state="active"]')?.textContent?.startsWith('Agents'));
      assert.equal(await sidebar.getByRole('tab', { name: /^Agents/ }).getAttribute('aria-selected'), 'true');
      assert.equal(await page.getByRole('textbox', { name: 'Search agents', exact: true }).inputValue(), 'Personal agent');
      await page.getByRole('textbox', { name: 'Search agents', exact: true }).fill('');
      await page.keyboard.press('Tab');
    }
    await sidebarTab('Projects'); await page.getByRole('textbox', { name: 'Search projects', exact: true }).fill('');
    await sidebarTab('Agents');
    const local = sidebar.locator('.machine-group__header').first();
    await local.focus(); await page.keyboard.press('Enter');
    await hidden(page.getByRole('navigation', { name: 'Choose agent', exact: true }));
    await page.reload(); await workspace(); assert.equal(await local.getAttribute('aria-expanded'), 'false');
    await local.click(); await visible(page.getByRole('navigation', { name: 'Choose agent', exact: true }));
  });
  await check('Project sections create, rename, reorder and archive from sidebar', async () => {
    await workspace(); await sidebarTab('Projects');
    const projectMenu=page.getByRole('button',{name:`Menu project ${projectName}`,exact:true});
    await projectMenu.click();await page.getByRole('menuitem',{name:'New section',exact:true}).click();
    const dialog=page.getByRole('dialog',{name:'New section',exact:true});
    await dialog.getByLabel('Section name',{exact:true}).fill('design');await dialog.getByRole('button',{name:'Create section',exact:true}).click();await hidden(dialog);
    const section=page.getByRole('button',{name:new RegExp(`^# design in ${projectName}`)});await visible(section);await section.click();
    await send(`${projectName} / design`,'Section smoke message');
    const menu=()=>page.getByRole('button',{name:`Menu section design in ${projectName}`,exact:true});
    await menu().click();await page.getByRole('menuitem',{name:'Rename section',exact:true}).click();
    const rename=page.getByRole('dialog',{name:'Rename section',exact:true});await rename.getByLabel('Section name',{exact:true}).fill('backend');await rename.getByRole('button',{name:'Save name',exact:true}).click();await hidden(rename);
    const search=page.getByRole('textbox',{name:'Search projects',exact:true});
    await search.fill('backend');
    await hidden(page.getByRole('button',{name:new RegExp(`^# general in ${projectName}`)}));
    await page.getByRole('button',{name:`Menu section backend in ${projectName}`,exact:true}).click();await page.getByRole('menuitem',{name:'Move section up',exact:true}).click();
    await search.fill('');
    await page.waitForFunction(name=>[...document.querySelectorAll('.project-navigation__project')].find(el=>el.querySelector('.project-navigation__name')?.textContent===name)?.querySelector('.project-navigation__room')?.textContent==='backend',projectName);
    await page.getByRole('button',{name:`Menu section backend in ${projectName}`,exact:true}).click();await page.getByRole('menuitem',{name:'Archive section',exact:true}).click();
    const archive=page.getByRole('alertdialog',{name:'Archive section backend?',exact:true});await archive.getByRole('button',{name:'Archive section',exact:true}).click();await hidden(archive);await hidden(page.getByRole('button',{name:new RegExp(`^# backend in ${projectName}`)}));
    await projectHome(projectName);await visible(page.getByRole('button',{name:/^backend\s*Archived$/}));
    await page.getByRole('button',{name:/^backend\s*Archived$/}).click();await visible(page.getByText('Section smoke message',{exact:true}));
    await cleanPage();
  });
  await check('Projects: new managed folder, member, project home and conversations in both themes', async () => {
    await workspace();
    const dialog=await openNew('New project');await dialog.getByLabel('Project name',{exact:true}).fill('Release notes');await dialog.getByRole('textbox',{name:/^Description/}).fill('Plan and write the next release together.');
    await dialog.getByRole('combobox',{name:'Add project member',exact:true}).click();await page.getByRole('option',{name:lead,exact:true}).click();
    await dialog.getByRole('button',{name:'Add folder',exact:true}).click();
    for(const theme of ['dark','light']) {await page.evaluate(theme=>document.documentElement.dataset.theme=theme,theme);await screenshot(`create-project-${theme}`);}
    await dialog.getByRole('button',{name:'Create project',exact:true}).click();await hidden(dialog);await visible(page.getByRole('main',{name:'Conversation Release notes / general',exact:true}));await projectHome('Release notes');await visible(page.getByRole('heading',{name:'Release notes',level:1,exact:true}));
    const result=await fetch(origin+'/api/projects').then(response=>response.json());const project=result.projects.find(project=>project.name==='Release notes');assert.equal(project.members.length,1);assert.equal(project.folders.length,1);await access(project.folders[0]);
    await visible(page.locator('section').filter({hasText:'Source control'}).getByText(/No Git repository in this folder|Working tree clean|changed/).first());
    await page.getByRole('button',{name:'New section',exact:true}).click();
    const sectionDialog=page.getByRole('dialog',{name:'New section',exact:true});
    await sectionDialog.getByLabel('Section name',{exact:true}).fill('writing');
    await sectionDialog.getByRole('button',{name:'Create section',exact:true}).click();await hidden(sectionDialog);
    await visible(page.getByRole('button',{name:/^# writing in Release notes/}));
    for(const theme of ['dark','light']) {await page.evaluate(theme=>document.documentElement.dataset.theme=theme,theme);await screenshot(`project-home-${theme}`);await screenshot(`sidebar-sections-projects-${theme}`);}
    await page.getByRole('button',{name:'general',exact:true}).first().click();await visible(page.getByRole('main',{name:'Conversation Release notes / general',exact:true}));
    await projectHome('Release notes');await visible(page.getByRole('heading',{name:'Release notes',level:1,exact:true}));
    await page.getByRole('button',{name:'Manage members & folders',exact:true}).click();await page.getByRole('combobox',{name:'Add project member',exact:true}).click();await page.getByRole('option',{name:helper,exact:true}).click();await page.getByRole('button',{name:'Save project',exact:true}).click();await visible(page.getByRole('button',{name:'Manage members & folders',exact:true}));
    await page.getByRole('button',{name:'New conversation',exact:true}).click();await page.getByRole('menuitem',{name:lead,exact:true}).click();await visible(page.getByRole('main',{name:'Conversation Project conversation',exact:true}));await cleanPage();
  });
  await check('Sidebar keyboard resize preserves width', async () => {
    await agentChat();
    const sidebar = page.getByRole('complementary', { name: 'Workspace navigation', exact: true });
    const resize = sidebar.getByRole('separator');
    const initial = (await sidebar.boundingBox()).width;
    await resize.focus(); await page.keyboard.press('ArrowLeft');
    await page.waitForFunction(initial => document.querySelector('.sidebar').getBoundingClientRect().width < initial, initial);
    await page.keyboard.press('ArrowRight');
    await page.waitForFunction(initial => Math.abs(document.querySelector('.sidebar').getBoundingClientRect().width - initial) < 1, initial);
    await page.setViewportSize(receipt.viewport); await sidebarTab('Agents');
  });
  await sidebarSpacingSmoke({ page, check, sidebarTab, screenshot, projectName });
  await check('Project connected-apps: not configured, installation opt-in and themes', async () => {
    await workspace();await sidebarTab('Projects');await projectHome('Release notes');
    const tabs=page.getByRole('tablist',{name:'Project sections',exact:true});
    await tabs.getByRole('tab',{name:'Connected apps',exact:true}).click(); await visible(page.getByText('Connect apps',{exact:true})); await screenshot('project-connected-apps-not-configured');
    for(const theme of ['dark','light']){await page.evaluate(theme=>document.documentElement.dataset.theme=theme,theme);await screenshot(`project-connected-apps-${theme}`);}
    await cleanPage();
  });
  await check('Project environment: add, replace, delete, values withheld and themes', async () => {
    await workspace();await sidebarTab('Projects');await projectHome('Release notes');
    const tabs=page.getByRole('tablist',{name:'Project sections',exact:true});
    await tabs.getByRole('tab',{name:'Environment',exact:true}).click(); await visible(page.getByRole('heading',{name:'Environment',exact:true}));
    await page.getByRole('button',{name:'Add variable',exact:true}).click(); const envSecret='UI_PROJECT_SECRET_7x'; await page.getByLabel('Variable name',{exact:true}).fill('PROJECT_UI_TOKEN'); await page.getByLabel('Secret value',{exact:true}).fill(envSecret); await page.getByRole('dialog',{name:'Add variable',exact:true}).getByRole('button',{name:'Add variable',exact:true}).click(); await visible(page.getByText('PROJECT_UI_TOKEN',{exact:true})); assert(!await page.locator('body').textContent().then(text=>text.includes(envSecret))); for(const theme of ['dark','light']){await page.evaluate(theme=>document.documentElement.dataset.theme=theme,theme);await screenshot(`project-environment-set-${theme}`);}
    await page.getByRole('button',{name:'Variable actions for PROJECT_UI_TOKEN',exact:true}).click(); await page.getByRole('menuitem',{name:'Replace',exact:true}).click(); await page.getByLabel('Secret value',{exact:true}).fill('UI_PROJECT_SECRET_REPLACED'); await page.getByRole('button',{name:'Replace value',exact:true}).click(); const replaceConfirm=page.getByRole('alertdialog'); await visible(replaceConfirm); await replaceConfirm.getByRole('button',{name:'Replace value',exact:true}).click(); await visible(page.getByText('Environment value saved.',{exact:true}));
    await page.getByRole('button',{name:'Variable actions for PROJECT_UI_TOKEN',exact:true}).click(); await page.getByRole('menuitem',{name:'Delete',exact:true}).click(); const deleteConfirm=page.getByRole('alertdialog'); await visible(deleteConfirm); await deleteConfirm.getByRole('button',{name:'Delete variable',exact:true}).click(); await visible(page.getByText('No environment variables',{exact:true})); await screenshot('project-environment-empty');
    for(const theme of ['dark','light']){await page.evaluate(theme=>document.documentElement.dataset.theme=theme,theme);await screenshot(`project-environment-${theme}`);}
    await cleanPage();
  });
  await check('Project mcp: test, add, remove project HTTP connection and themes', async () => {
    await workspace();await sidebarTab('Projects');await projectHome('Release notes');
    const tabs=page.getByRole('tablist',{name:'Project sections',exact:true});
    await tabs.getByRole('tab',{name:'MCP',exact:true}).click(); await visible(page.getByRole('heading',{name:'MCP connections',exact:true})); await page.getByRole('button',{name:'Add connection',exact:true}).click(); const mcpDialog=page.getByRole('dialog',{name:'New connection',exact:true}); await mcpDialog.getByLabel('Name',{exact:true}).fill('Smoke project MCP'); await mcpDialog.getByLabel('URL',{exact:true}).fill(`http://127.0.0.1:${mcpPort}/mcp`); await mcpDialog.getByLabel(/^Token/).fill('ui-smoke-token'); await mcpDialog.getByRole('button',{name:'Test connection',exact:true}).click(); await visible(mcpDialog.getByRole('button',{name:'Save connection',exact:true})); await mcpDialog.getByRole('button',{name:'Save connection',exact:true}).click(); await hidden(mcpDialog); await visible(page.getByText('Smoke project MCP',{exact:true})); for(const theme of ['dark','light']){await page.evaluate(theme=>document.documentElement.dataset.theme=theme,theme);await screenshot(`project-mcp-connected-${theme}`);} await page.getByRole('button',{name:'Menu Smoke project MCP',exact:true}).click(); await page.getByRole('menuitem',{name:'Remove Smoke project MCP',exact:true}).click(); const mcpConfirm=page.getByRole('alertdialog'); await visible(mcpConfirm); await mcpConfirm.getByRole('button',{name:'Remove connection',exact:true}).click(); await visible(page.getByText('No connections',{exact:true})); await screenshot('project-mcp-empty');
    for(const theme of ['dark','light']){await page.evaluate(theme=>document.documentElement.dataset.theme=theme,theme);await screenshot(`project-mcp-${theme}`);}
    await cleanPage();
  });
  await check('Help: keyboard shortcuts and both themes', async () => {
    await workspace(); await page.getByRole('button', { name: 'Help', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: 'Keyboard shortcuts', exact: true }); await visible(dialog.getByRole('heading',{name:'Keyboard shortcuts',exact:true}));
    for(const theme of ['dark','light']) {await page.evaluate(theme=>document.documentElement.dataset.theme=theme,theme);await screenshot(`help-${theme}`);}await page.keyboard.press('Escape'); await hidden(dialog);
  });
  await check('SET1 agent sites and export entry points', async () => {
    await workspace(); await agentChat();
    const details = page.getByRole('complementary', { name: 'Conversation details', exact: true });
    const show = page.getByRole('button', { name: 'Show details', exact: true }); if (await show.isVisible()) await show.click();
    await details.getByRole('button', { name: 'Edit profile', exact: true }).click();
    const profile = page.getByRole('dialog', { name: 'Agent profile', exact: true }); await profile.getByRole('tab', { name: 'Advanced', exact: true }).click();
    await profile.getByRole('button', { name: 'Agent sites', exact: true }).click();
    const sites = page.getByRole('dialog', { name: 'Agent sites', exact: true }); await visible(sites); await visible(sites.getByRole('button', { name: 'Create site', exact: true }));
    await page.keyboard.press('Escape'); await hidden(sites); await profile.getByRole('button', { name: 'Cancel', exact: true }).click();
    await page.getByRole('main', { name: `Conversation ${lead}`, exact: true }).locator('.chat__pill').click();
    await page.getByRole('menuitem', { name: 'Export template', exact: true }).click();
    const exported = page.getByRole('dialog', { name: `${lead} · Export template`, exact: true }); await visible(exported);
    await exported.getByRole('button', { name: 'Preview export template', exact: true }).click(); await visible(exported.getByRole('button', { name: 'Download reviewed template', exact: true }));
    await page.keyboard.press('Escape'); await hidden(exported); await cleanPage();
  });
  await settingsDataSmoke();
  await check('Updates: unmanaged instance shows version without update controls', async () => {
    const dialog = await settings('Updates');
    await visible(dialog.getByText('Current version', { exact: true }));
    assert.equal(await dialog.getByRole('button', { name: 'Update now', exact: true }).count(), 0);
    assert.equal(await page.getByRole('button', { name: 'Update available', exact: true }).count(), 0);
    await cleanPage(dialog);
  });
  await check('Updates: available release, consent, machine dialog and themes', async () => {
    const list = await fetch(origin + '/api/machines').then(response => response.json());
    const machine = { ...list.machines[0], version: '0.1.0', allowUpdates: true, status: 'online' };
    const available = { id: '0.2.0-12345678', version: '0.2.0', commit: '12345678'.repeat(5), notes: 'Workspace updates now verify health before activation.\nAutomatic rollback keeps the previous release ready.', created: '2026-11-14T08:00:00.000Z' };
    const fixture = { current: { ...available, id: '0.1.0-87654321', version: '0.1.0' }, previous: { ...available, version: '0.0.9' }, feed: available, available, launcherAvailable: true, runningTurns: 2 };
    const statusRoute = '**/api/updates', machinesRoute = '**/api/machines', peersRoute = '**/api/updates/machines';
    await page.route(statusRoute, route => { const url = new URL(route.request().url()); if (route.request().method() === 'GET' && url.pathname === '/api/updates') return route.fulfill({ contentType: 'application/json', body: JSON.stringify(fixture) }); return route.continue(); });
    await page.route(machinesRoute, route => { const url = new URL(route.request().url()); if (route.request().method() === 'GET' && url.pathname === '/api/machines') return route.fulfill({ contentType: 'application/json', body: JSON.stringify({ machines: [machine] }) }); return route.continue(); });
    await page.route(peersRoute, route => { if (route.request().method() === 'GET') return route.fulfill({ contentType: 'application/json', body: JSON.stringify({ machines: [{ machine: machine.id, available: true, launcherAvailable: true, runningTurns: 3, current: { version: '0.1.0' } }] }) }); return route.continue(); });
    try {
      await workspace();
      for (const theme of ['dark', 'light']) {
        const appearance = await settings('Appearance'); await appearance.getByRole('radio', { name: theme === 'dark' ? 'Dark' : 'Light', exact: true }).click();
        const settingsDialog = await settings('Updates');
        await visible(settingsDialog.getByText('Minarbot 0.2.0', { exact: true }));
        await visible(settingsDialog.getByText('2 running turns', { exact: true }));
        await screenshot(`settings-updates-${theme}`);
        await settingsDialog.getByRole('button', { name: 'Update now', exact: true }).click();
        const confirmation = page.getByRole('dialog', { name: 'Update 1 machine?', exact: true }); await visible(confirmation);
        await visible(confirmation.getByRole('row').filter({ hasText: 'Connected server' }).getByRole('cell', { name: '2', exact: true }));
        await visible(confirmation.getByText('2 running turns will be interrupted and can resume after the restart.', { exact: true }));
        await confirmation.getByRole('button', { name: 'Release notes', exact: true }).click();
        await visible(confirmation.getByText(/Automatic rollback keeps the previous release ready/));
        await confirmation.getByRole('button', { name: 'Cancel', exact: true }).click(); await hidden(confirmation);
        await settingsDialog.getByRole('switch', { name: `Also update ${machine.name}`, exact: true }).check();
        const open = settingsDialog.getByRole('button', { name: 'Update selected (1)', exact: true }); await open.click();
        const update = page.getByRole('dialog', { name: 'Update 1 machine?', exact: true }); await visible(update);
        await visible(update.getByText(machine.name, { exact: true })); await visible(update.getByText('3 running turns will be interrupted', { exact: false }));
        assert(await update.evaluate(el => el.scrollWidth <= el.clientWidth + 1), 'Machine table overflow escaped dialog');
        await screenshot(`update-machines-dialog-${theme}`);
        await update.getByRole('button', { name: 'Cancel', exact: true }).click(); await hidden(update); await focusRestored(open);
      }
      fixture.current = available; delete fixture.available;
      const updatedOrigin = await settings('Updates');
      await visible(updatedOrigin.getByText('Up to date', { exact: true }));
      assert(await updatedOrigin.getByRole('switch', { name: `Also update ${machine.name}`, exact: true }).isEnabled(), 'Up-to-date origin must still distribute release to older machines');
      await settings('Appearance'); await page.getByRole('radio', { name: 'Dark', exact: true }).click();
    } finally { await page.unroute(statusRoute); await page.unroute(machinesRoute); await page.unroute(peersRoute); }
    await workspace();
  });
  await check('Machines: Add a machine install command, copy, expiry and dialog focus in dark and light', async () => {
    await page.context().grantPermissions(['clipboard-read','clipboard-write'], {origin});
    for (const theme of ['dark','light']) {
      await settings('Appearance'); await page.getByRole('radio',{name:theme === 'dark'?'Dark':'Light',exact:true}).click();
      const panel=await settings('Machines'), trigger=panel.getByRole('button',{name:'Add machine',exact:true});
      await trigger.click(); await page.getByRole('menuitem',{name:'Install Minarbot',exact:true}).click(); const dialog=page.getByRole('dialog',{name:'Install Minarbot',exact:true}); await visible(dialog);
      await visible(dialog.getByText('Linux and macOS requirements',{exact:true}));
      await dialog.getByRole('button',{name:'Create install command',exact:true}).click();
      const command=dialog.getByRole('textbox',{name:'Install command',exact:true});await visible(command);
      const text=await command.inputValue(); assert(text.startsWith(`curl -fsSL '${origin}/api/install/script' | sh -s -- --source '${origin}' --token 'bsi_`)); assert(/bsi_[A-Za-z0-9_-]{43}'$/.test(text));
      await visible(dialog.getByText('Private, single use',{exact:true}));
      await dialog.getByRole('button',{name:'Copy command',exact:true}).click();await visible(dialog.getByRole('button',{name:'Copied',exact:true}));assert.equal(await page.evaluate(()=>navigator.clipboard.readText()),text);
      for (const key of ['Tab','Shift+Tab']) {await page.keyboard.press(key);assert(await dialog.evaluate(el=>el.contains(document.activeElement)), 'Install dialog must trap focus');}
      await screenshot(`add-machine-${theme}`); await cleanPage(dialog);
      await page.keyboard.press('Escape'); await hidden(dialog); await focusRestored(trigger);
      await trigger.click();await page.getByRole('menuitem',{name:'Install Minarbot',exact:true}).click();await hidden(page.getByRole('textbox',{name:'Install command',exact:true}));await page.keyboard.press('Escape');
    }
    // Drive the 30-minute boundary without waiting in real time.
    const panel=await settings('Machines');await panel.getByRole('button',{name:'Add machine',exact:true}).click();await page.getByRole('menuitem',{name:'Install Minarbot',exact:true}).click();
    const dialog=page.getByRole('dialog',{name:'Install Minarbot',exact:true});
    await dialog.getByRole('button',{name:'Create install command',exact:true}).click();await visible(dialog.getByRole('textbox',{name:'Install command',exact:true}));
    await page.evaluate(()=>{window.__installerDateNow=Date.now;Date.now=()=>window.__installerDateNow()+31*60_000;});
    try {await visible(dialog.getByText('This install command expired. Create a new one to continue.',{exact:true}));await hidden(dialog.getByRole('textbox',{name:'Install command',exact:true}));}
    finally {await page.evaluate(()=>{Date.now=window.__installerDateNow;delete window.__installerDateNow;});}
    await page.keyboard.press('Escape');await workspace();
  });
  await check('Machines: code shown once, paired agents, light and dark', async () => {
    for (const theme of ['dark', 'light']) {
      const appearance = await settings('Appearance');
      await appearance.getByRole('radio', { name: theme === 'dark' ? 'Dark' : 'Light', exact: true }).click();
      await page.waitForFunction(theme => document.documentElement.dataset.theme === theme, theme);
      const dialog = await settings('Machines');
      await visible(dialog.getByRole('button', { name: 'Resources', exact: true }));
      await visible(dialog.locator('.k-row__title').filter({ hasText: 'Studio workstation' }));
      await dialog.getByRole('button', { name: 'Add machine', exact: true }).click();await page.getByRole('menuitem',{name:'Show pairing code',exact:true}).click();
      const setup=page.getByRole('dialog',{name:'Show pairing code',exact:true});await setup.getByRole('button',{name:'Create code',exact:true}).click();
      const code = setup.getByRole('region', { name: 'One-time pairing code', exact: true });
      await visible(code);
      assert.match(await code.locator('.machines-code__value').innerText(), /^[23456789ABCDEFGHJKLMNPQRSTUVWXYZ]{12}$/);
      assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), theme);
      await screenshot(`settings-machines-${theme}`);
      await setup.getByRole('button',{name:'Close',exact:true}).first().click();
      await dialog.getByRole('button', { name: 'Close settings', exact: true }).click();
      await settings('Machines');
      await hidden(page.getByRole('region', { name: 'One-time pairing code', exact: true }));
      await page.getByRole('button', { name: 'Close settings', exact: true }).click();
      await sidebarTab('Agents');
      const machines = page.getByRole('navigation', { name: 'Machines', exact: true });
      const trigger = machines.locator('.machine-group__header').filter({ hasText: 'Studio workstation' });
      if (await trigger.getAttribute('aria-expanded') !== 'true') await trigger.click();
      const agent = machines.getByRole('button', { name: 'Remote Iris on Studio workstation', exact: true });
      await visible(agent); assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), theme); await screenshot(`sidebar-machines-${theme}`);
      await agent.click();
      await visible(page.getByRole('main', { name: 'Conversation Remote Iris on Studio workstation', exact: true }));
      assert.equal(await page.getByText('Back to workspace', { exact: true }).count(), 0);
      assert.equal(await page.getByText(/127\.0\.0\.1/, { exact: false }).count(), 0);
      const remoteComposer = page.getByRole('textbox', { name: /Message Remote Iris on Studio workstation/ });
      await visible(remoteComposer);
      if (theme === 'dark') {
        const initialHeight = await remoteComposer.evaluate(element => element.clientHeight);
        await remoteComposer.fill('First line'); await remoteComposer.press('Shift+Enter'); await remoteComposer.type('Second line');
        await remoteComposer.press('Shift+Enter'); await remoteComposer.type('Third line');
        assert.equal(await remoteComposer.inputValue(), 'First line\nSecond line\nThird line');
        await page.waitForFunction(height => document.querySelector('#remote-message').clientHeight > height, initialHeight);
        for (const text of ['Summarize the current plan.', 'List the next steps.']) {
          await remoteComposer.fill(text); await remoteComposer.press('Enter');
          const remoteTurn = page.locator('.turn-block').filter({ has: page.getByText(text, { exact: true }) });
          await visible(remoteTurn.locator('.agent-message .message-body strong').filter({ hasText: 'ready' }));
          await visible(remoteTurn.locator('.turn-status').getByText('Done', { exact: true }));
        }
      }
      await page.waitForFunction(() => document.querySelectorAll('.human-bubble').length === 2);
      assert.equal(await page.locator('.human-bubble').count(), 2);
      assert.equal(await page.locator('.agent-message .message-body .markdown').count(), 2);
      await screenshot(`remote-conversation-${theme}`);
      assert.equal(await page.getByRole('button', { name: 'Send message', exact: true }).count(), 1);
      await page.getByRole('button', { name: 'Collapse sidebar', exact: true }).click();
      await hidden(page.getByRole('navigation', { name: 'Machines', exact: true }));
      await page.getByRole('button', { name: 'Expand sidebar', exact: true }).click();
      await page.getByRole('button', { name: /^Smoke Lead / }).click();
      const localChat = page.getByRole('main', { name: 'Conversation Smoke Lead', exact: true });
      await visible(localChat.getByRole('textbox', { name: 'Message Smoke Lead', exact: true }));
      assert.equal(await localChat.locator('.composer').count(), 1, 'Local chat composer changed');
      assert.equal(await localChat.locator('.chat__header').count(), 1, 'Local chat header changed');
      await cleanPage();
    }
  });
  await check('Chat: shared local and remote layout, keyboard behavior and dark/light comparison', async () => {
    await createAgent('Chat Layout');
    const local = page.getByRole('main', { name: 'Conversation Chat Layout', exact: true });
    const input = local.getByRole('textbox', { name: 'Message Chat Layout', exact: true });
    await visible(local.locator('.empty-chat'));
    await input.fill('First line'); await input.press('Shift+Enter'); await input.type('Second line');
    assert.equal(await input.inputValue(), 'First line\nSecond line');
    for (const text of ['Summarize the current plan.', 'List the next steps.']) {
      await input.fill(text); await input.press('Enter');
      const turn = local.locator('.turn-block').filter({ has: page.getByText(text, { exact: true }) });
      await visible(turn.locator('.agent-message .message-body strong').filter({ hasText: 'ready' }));
      await visible(turn.getByRole('button', { name: 'Copy reply', exact: true }));
      assert.equal(await turn.locator('.turn-status, .turn-model').count(), 0);
    }
    if (await page.getByRole('button', { name: 'Close details', exact: true }).isVisible()) await page.getByRole('button', { name: 'Close details', exact: true }).click();
    const layout = async chat => {
      await chat.evaluate(element => Promise.all(element.getAnimations({ subtree: true }).filter(animation => animation.effect?.getTiming().iterations !== Infinity).map(animation => animation.finished.catch(() => {}))));
      return chat.evaluate(element => {
      const style = selector => { const node = element.querySelector(selector), css = getComputedStyle(node); return { fontSize: css.fontSize, lineHeight: css.lineHeight, padding: css.padding, borderRadius: css.borderRadius, background: css.backgroundColor, border: css.border, width: node.getBoundingClientRect().width }; };
      const human = element.querySelector('.human-message').getBoundingClientRect(), turn = element.querySelector('.turn-block').getBoundingClientRect();
      // Recording now lives in T3's separate toolbar and never narrows the prompt.
      const input = style('.composer__input');
      return { header: style('.chat__header'), pill: { ...style('.chat__pill'), width: undefined }, composer: style('.composer'), input, bubble: { ...style('.human-bubble'), width: undefined }, answer: style('.message-body'), rightAligned: Math.abs(human.right - turn.right) < 1 };
      });
    };
    for (const theme of ['dark', 'light']) {
      await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
      await page.getByRole('button', { name: /^Chat Layout / }).click();
      await visible(local.locator('.turn-block').nth(1));
      await input.focus(); await page.mouse.move(30, 890);
      const localLayout = await layout(local);
      assert(localLayout.rightAligned);
      assert.equal(await local.getByRole('button', { name: 'Attach conversation file', exact: true }).isDisabled(), false);
      await screenshot(`local-chat-${theme}`);
      await page.getByRole('button', { name: 'Remote Iris on Studio workstation', exact: true }).click();
      const remote = page.getByRole('main', { name: 'Conversation Remote Iris on Studio workstation', exact: true });
      await visible(remote.locator('.turn-block').nth(1));
      await remote.getByRole('textbox', { name: 'Message Remote Iris on Studio workstation', exact: true }).focus();
      const remoteLayout = await layout(remote);
      assert.deepEqual(remoteLayout, localLayout, 'Shared chat typography, spacing, colors and composer differ');
      assert.equal(await remote.getByRole('button', { name: 'Attach conversation file', exact: true }).isDisabled(), true);
      await screenshot(`remote-chat-${theme}`);
    }
    await cleanPage();
  });
  await check('Sidebar: machine groups, preview times, search, filters and persisted collapse', async () => {
    await workspace();
    await page.context().grantPermissions(['notifications'], { origin });
    const appearance = await settings('General');
    const notifications = appearance.getByRole('switch', { name: 'Agent and workflow notifications', exact: true });
    assert(await notifications.isChecked());
    await notifications.click(); assert.equal(await notifications.getAttribute('aria-checked'), 'false');
    await workspace(); await settings('General');
    assert.equal(await notifications.getAttribute('aria-checked'), 'false');
    await notifications.click(); assert(await notifications.isChecked());
    await workspace(); await sidebarTab('Agents');
    const agents = await fetch(origin + '/api/agents').then(response => response.json());
    const owner = agents.agents.find(agent => agent.name === lead);
    const layout = await fetch(origin + '/api/sidebar').then(response => response.json());
    const saved = await page.request.post(origin + '/api/sidebar', { headers: { Origin: origin }, data: { id: randomUUID(), revision: layout.revision, sections: [{ id: randomUUID(), name: 'Editorial', agents: [owner.id] }] } });
    assert(saved.ok());
    await agentChat();
    const machines = page.getByRole('navigation', { name: 'Machines', exact: true });
    const local = machines.getByRole('group', { name: 'This device contents', exact: true });
    const headers = machines.locator('.machine-group__header');
    assert.match(await headers.first().innerText(), /THIS DEVICE/);
    assert.match(await headers.nth(1).innerText(), /STUDIO WORKSTATION/);
    await visible(local.getByRole('navigation', { name: 'Choose agent', exact: true }));
    await hidden(local.getByText('Editorial',{exact:true}));
    assert.deepEqual(await fetch(origin+'/api/sidebar').then(r=>r.json()),await saved.json());
    await hidden(page.getByRole('navigation', { name: 'Projects', exact: true }));
    await hidden(local.getByText('Chatrooms', { exact: true }));
    await page.getByRole('textbox', { name: 'Search agents', exact: true }).focus(); await page.mouse.move(700, 700);
    const localRow = local.locator('.bot-row[aria-current="page"]');
    // The agent's role or plain-text preview is the subtitle. The latest reply stays in its tooltip.
    const subtitle = localRow.locator('.bot-row__preview'); await visible(subtitle);
    assert.ok((await subtitle.innerText()).trim().length > 0);
    assert.match(await subtitle.getAttribute('title') ?? '', /^(Smoke reply: ready\.|@Smoke Helper, please check the plan\.)/);
    await localRow.hover(); await hidden(localRow.locator('time'));
    await page.mouse.move(1000, 10);
    await page.keyboard.press('F6'); await page.mouse.move(800, 100);
    await visible(localRow.locator('time'));
    assert.match(await localRow.locator('time').innerText(), /^\d+[mhd]$/);
    const remote = machines.getByRole('navigation', { name: 'Agents on Studio workstation', exact: true });
    await visible(remote.getByRole('button', { name: 'Remote Iris on Studio workstation', exact: true }));
    await visible(remote.locator('time'));
    const localTrigger = headers.first();
    await localTrigger.focus(); await page.keyboard.press('Enter');
    await hidden(local);
    await workspace();
    assert.equal(await headers.first().getAttribute('aria-expanded'), 'false');
    await headers.first().focus(); await page.keyboard.press('Space');
    await visible(local);
    const search = page.getByRole('textbox', { name: 'Search agents', exact: true });
    await search.fill('Remote Iris');
    await hidden(local.getByRole('button', { name: new RegExp(`^${lead} `) }));
    await hidden(local.getByRole('button',{name:projectName,exact:true}));
    await visible(remote.getByRole('button', { name: 'Remote Iris on Studio workstation', exact: true }));
    await localTrigger.click();
    await hidden(local);
    await search.fill(projectName);
    await hidden(page.getByRole('button',{name:projectName,exact:true}));
    await hidden(remote.getByRole('button', { name: 'Remote Iris on Studio workstation', exact: true }));
    await search.fill('');
    await visible(local.getByRole('button', { name: new RegExp(`^${lead} `) }));
    await page.getByRole('button', { name: 'Filter sidebar', exact: true }).click();
    await page.getByRole('menuitemcheckbox', { name: 'Only online', exact: true }).click();
    await visible(remote.getByRole('button', { name: 'Remote Iris on Studio workstation', exact: true }));
    await page.getByRole('button', { name: 'Filter sidebar', exact: true }).click();
    const online = page.getByRole('menuitemcheckbox', { name: 'Only online', exact: true });
    assert.equal(await online.getAttribute('aria-checked'), 'true');
    await online.click();
    const hiddenResponse = await page.request.post(origin + '/api/agents', { headers: { Origin: origin }, data: { runtime: 'api', id: randomUUID(), name: 'Hidden sidebar fixture', model: models[0], instructions: 'Synthetic hidden fixture.' } });
    assert(hiddenResponse.ok());
    const hiddenAgent = await hiddenResponse.json();
    const hide = await page.request.post(origin + `/api/agents/${hiddenAgent.id}/visibility`, { headers: { Origin: origin }, data: { id: randomUUID(), revision: 0, hidden: true } });
    assert(hide.ok());
    await workspace();
    const hiddenRow = local.getByRole('button', { name: /^Hidden sidebar fixture / });
    await hidden(hiddenRow);
    const hiddenGroup = page.locator('.hidden-agents');
    const toggle = hiddenGroup.getByRole('button', { name: /^Hidden/ });
    assert.equal(await toggle.getAttribute('aria-expanded'), 'false');
    await toggle.click();
    const hiddenAgents = page.getByRole('navigation', { name: 'Hidden agents', exact: true });
    await visible(hiddenAgents.getByRole('button', { name: 'Open hidden agent Hidden sidebar fixture', exact: true }));
    await hiddenAgents.getByRole('button', { name: 'Restore Hidden sidebar fixture', exact: true }).click();
    await visible(hiddenRow);
    await hidden(hiddenGroup);
    // Cached remote agents remain clickable offline; Only online removes that machine.
    const machineList = await fetch(origin + '/api/machines').then(response => response.json());
    const machinesRoute = '**/api/machines';
    await page.route(machinesRoute, route => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ machines: machineList.machines.map(machine => ({ ...machine, status: 'offline' })) }) }));
    try {
      await visible(machines.getByRole('img', { name: 'Offline', exact: true }).first());
      await visible(remote.getByRole('button', { name: 'Remote Iris on Studio workstation', exact: true }));
      await page.getByRole('button', { name: 'Filter sidebar', exact: true }).click();
      await page.getByRole('menuitemcheckbox', { name: 'Only online', exact: true }).click();
      await hidden(remote);
      await visible(local);
      await page.getByRole('button', { name: 'Filter sidebar', exact: true }).click();
      await page.getByRole('menuitemcheckbox', { name: 'Only online', exact: true }).click();
    } finally { await page.unroute(machinesRoute); }
    await visible(machines.getByRole('img', { name: 'Online', exact: true }).first());
    await page.getByRole('textbox', { name: 'Search agents', exact: true }).focus();
    await page.mouse.move(700, 700);
    for (const theme of ['dark', 'light']) {
      await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
      const sidebar = page.getByRole('complementary', { name: 'Workspace navigation', exact: true });
      const layout = await sidebar.locator('.sidebar__scroll').evaluate(element => ({ scrollWidth: element.scrollWidth, width: element.clientWidth, rows: [...element.querySelectorAll('.bot-row')].map(row => ({ width: row.clientWidth, scroll: row.scrollWidth })), avatars: [...element.querySelectorAll('.bot-row__avatar .bs-avatar')].map(avatar => avatar.getBoundingClientRect().width) }));
      assert(layout.scrollWidth <= layout.width + 1, 'Sidebar overflows horizontally');
      assert(layout.rows.every(row => row.scroll <= row.width + 1), 'Sidebar row overflows');
      assert(layout.avatars.every(width => width === 36), 'Agent avatars must stay36px');
      await screenshot(`sidebar-by-machine-${theme}`);
    }
    // Restore remote conversation expected by next check.
    await remote.getByRole('button', { name: 'Remote Iris on Studio workstation', exact: true }).click();
  });
  await check('Machines: remote composer restores draft and uncertain request without duplicate delivery', async () => {
    await workspace();
    const agent=page.getByRole('navigation',{name:'Machines',exact:true}).getByRole('button',{name:'Remote Iris on Studio workstation',exact:true}); await agent.click();
    const composer=page.getByRole('textbox',{name:/Message Remote Iris on Studio workstation/});
    async function reopen(){const trigger=page.getByRole('navigation',{name:'Machines',exact:true}).locator('.machine-group__header').filter({hasText:'Studio workstation'});if(await trigger.getAttribute('aria-expanded')!=='true')await trigger.click();await agent.click();}
    await composer.fill('Recover remote request');
    await page.getByRole('button', { name: /^Smoke Lead / }).click(); await reopen();
    assert.equal(await composer.inputValue(),'Recover remote request');
    const submissions=[];
    const pattern='**/api/remote-agents/messages';
    await page.route(pattern,async route=>{if(route.request().method()!=='POST')return route.continue();submissions.push(route.request().postDataJSON());await route.fetch();await route.fulfill({status:200,contentType:'application/json',body:'{lost acknowledgement'});});
    try{
      await composer.press('Enter');await visible(page.getByText('Message submission is not confirmed',{exact:true}));
      await page.getByRole('button', { name: /^Smoke Lead / }).click(); await reopen();
      await visible(page.getByRole('button',{name:'Retry send',exact:true}));assert.equal(await composer.isDisabled(),true);
    }finally{await page.unroute(pattern);}
    await page.route(pattern,async route=>{if(route.request().method()==='POST')submissions.push(route.request().postDataJSON());await route.continue();});
    try{await page.getByRole('button',{name:'Retry send',exact:true}).click();await visible(page.getByRole('button',{name:'Send message',exact:true}));}finally{await page.unroute(pattern);}
    assert.equal(submissions.length,2);assert.deepEqual(submissions[0],submissions[1]);
    const turn=page.locator('.turn-block').filter({has:page.getByText('Recover remote request',{exact:true})});
    await visible(turn.getByText('Done',{exact:true}));assert.equal(await turn.count(),1);await cleanPage();
  });
  await check('Machines: offline and remote kill switch disable composer without losing draft', async () => {
    const paired = await fetch(origin + '/api/machines').then(response => response.json());
    const machine = paired.machines.find(item => item.name === 'Studio workstation');
    const remote = page.getByRole('main', { name: 'Conversation Remote Iris on Studio workstation', exact: true });
    const composer = remote.getByRole('textbox', { name: 'Message Remote Iris on Studio workstation', exact: true });
    await composer.fill('Keep this draft while unavailable');
    const machinesRoute = '**/api/machines';
    await page.route(machinesRoute, route => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ machines: paired.machines.map(item => ({ ...item, status: 'offline' })) }) }));
    try {
      await visible(remote.getByText('Offline', { exact: true }));
      assert.equal(await composer.isDisabled(), true);
      assert.equal(await remote.getByRole('button', { name: 'Send message', exact: true }).isDisabled(), true);
      await visible(remote.getByText('Offline. Reconnect the machine before sending.', { exact: true }));
      await screenshot('remote-chat-offline');
    } finally { await page.unroute(machinesRoute); }
    const reciprocal = await fetch(machine.origin + '/api/machines').then(response => response.json());
    const policy = async allowed => {
      const response = await fetch(machine.origin + '/api/machines/agent-messaging', { method: 'POST', headers: { Origin: machine.origin, 'Content-Type': 'application/json' }, body: JSON.stringify({ id: randomUUID(), environmentId: reciprocal.machines[0].id, allowed }) });
      assert(response.ok);
    };
    try {
      await policy(false);
      await visible(remote.getByText('Messaging is turned off on Studio workstation. Enable it in Settings > Machines on that machine.', { exact: true }));
      assert.equal(await composer.isDisabled(), true);
      assert.equal(await remote.getByRole('button', { name: 'Send message', exact: true }).isDisabled(), true);
      await screenshot('remote-chat-messaging-off');
    } finally { await policy(true); }
    await page.waitForFunction(() => !document.querySelector('#remote-message')?.disabled);
    assert.equal(await composer.inputValue(), 'Keep this draft while unavailable');
    await composer.fill('');
    await cleanPage();
  });
  await check('Machines: unpair confirmation and add form restore mutual pairing', async () => {
    const dialog = await settings('Machines');
    const paired = await fetch(origin + '/api/machines').then(response => response.json());
    const remote = paired.machines.find(machine => machine.name === 'Studio workstation');
    assert(remote);
    await dialog.getByRole('button', { name: 'Machine actions: Studio workstation', exact: true }).click();
    await page.getByRole('menuitem', { name: 'Unpair Studio workstation', exact: true }).click();
    const confirm = page.getByRole('alertdialog');
    await visible(confirm);
    await confirm.getByRole('button', { name: 'Cancel', exact: true }).click();
    await hidden(confirm);
    await dialog.getByRole('button', { name: 'Machine actions: Studio workstation', exact: true }).click();
    await page.getByRole('menuitem', { name: 'Unpair Studio workstation', exact: true }).click();
    await confirm.getByRole('button', { name: 'Unpair machine', exact: true }).click();
    await visible(dialog.getByText('No paired machines', { exact: true }));
    const response = await fetch(remote.origin + '/api/pairing-codes', { method: 'POST', headers: { Origin: remote.origin, 'Content-Type': 'application/json' }, body: JSON.stringify({ id: crypto.randomUUID() }) });
    assert(response.ok);
    const pairing = await response.json();
    await dialog.getByRole('button',{name:'Add machine',exact:true}).click();await page.getByRole('menuitem',{name:'Pair machine',exact:true}).click();
    const setup=page.getByRole('dialog',{name:'Pair machine',exact:true});
    await setup.getByLabel('Address', { exact: true }).fill(remote.origin);
    await setup.getByLabel('Pairing code', { exact: true }).fill(pairing.code);
    await setup.getByRole('button', { name: 'Pair machine', exact: true }).click();await hidden(setup);
    await visible(dialog.locator('.k-row__title').filter({ hasText: 'Studio workstation' }));
    const reciprocal = await fetch(remote.origin + '/api/machines').then(response => response.json());
    assert.equal(reciprocal.machines.length, 1);
    await cleanPage(dialog);
  });
  await check('MCP server: create token once, copy, revoke and theme layouts', async () => {
    let dialog = await settings('MCP server');
    await dialog.getByRole('button', { name: 'Create token', exact: true }).click();
    const create = page.getByRole('dialog', { name: 'Create token', exact: true });
    await create.getByRole('textbox', { name: 'Name', exact: true }).fill('Smoke laptop');
    assert.equal(await create.getByRole('group', {name:'Permissions',exact:true}).getByRole('checkbox').count(), 21);
    for (const label of ['Read','Write','Run']) await visible(create.locator('legend').filter({hasText:new RegExp('^'+label+'$')}));
    await create.getByRole('checkbox', { name: /^Read projects/ }).check();
    for (const theme of ['dark','light']) { await page.evaluate(theme => document.documentElement.dataset.theme=theme,theme); await screenshot(`mcp-scope-picker-${theme}`); }
    await page.evaluate(() => document.documentElement.dataset.theme='dark');
    await create.getByRole('checkbox', { name: /^Send messages/ }).check();
    await create.getByRole('checkbox', { name: /^Read token conversations/ }).check();
    await create.getByRole('button', { name: 'Create token', exact: true }).click();
    await hidden(create);
    const result = page.getByRole('dialog', { name: 'Copy token now', exact: true });
    const secret = result.getByRole('textbox', { name: 'Token', exact: true });
    await visible(secret);
    const token = await secret.inputValue(); assert(/^[A-Za-z0-9_-]{43}$/.test(token), 'Fixture token has unexpected format');
    await visible(result.getByText('Shown once', { exact: true }));
    await page.context().grantPermissions(['clipboard-read', 'clipboard-write'], { origin });
    await page.evaluate(() => Object.defineProperty(navigator, 'clipboard', { configurable: true, value: undefined }));
    try {
      const copyToken = result.getByRole('button', { name: 'Copy token', exact: true });
      await copyToken.focus(); await copyToken.click(); await visible(result.getByText('Copied.', { exact: true }));
      assert.equal(await copyToken.evaluate(el => document.activeElement === el), true, 'Fallback lost dialog focus');
    } finally { await page.evaluate(() => { delete navigator.clipboard; }); }
    assert.equal(await page.evaluate(() => navigator.clipboard.readText()), token);
    assert.equal(await result.locator('textarea[aria-hidden="true"]').count(), 0, 'Fallback retained token in DOM');
    await screenshot('mcp-server-token-once-dark');
    await result.getByRole('button', { name: 'I saved the token', exact: true }).click();
    assert.equal(await secret.count(), 0);
    await screenshot('mcp-server-dark');
    await settings('Skills'); dialog = await settings('MCP server');
    assert.equal(await secret.count(), 0);
    const metadata = await page.evaluate(async () => (await fetch('/api/mcp-server/peer-tokens')).text()); assert(!metadata.includes(token));
    let appearance = await settings('Appearance'); await appearance.getByRole('radio', { name: 'Light', exact: true }).click();
    await page.waitForFunction(() => document.documentElement.dataset.theme === 'light');
    dialog = await settings('MCP server'); await screenshot('mcp-server-light');
    await dialog.getByRole('button', { name: 'Token actions for Smoke laptop', exact: true }).click();
    await page.getByRole('menuitem', { name: 'Revoke token', exact: true }).click();
    const confirm = page.getByRole('alertdialog'); await visible(confirm); await confirm.getByRole('button', { name: 'Revoke token', exact: true }).click();
    await visible(dialog.getByRole('button', { name: /^Smoke laptop.*Revoked/ }));
    appearance = await settings('Appearance'); await appearance.getByRole('radio', { name: 'Dark', exact: true }).click();
    await page.waitForFunction(() => document.documentElement.dataset.theme === 'dark');
    return 'Token shown once, metadata-only listing, revoked with confirmation; dark/light captured.';
  });
  await check('Connected apps: installation key not configured', async () => {
    const dialog = await settings('Connected apps');
    await dialog.getByRole('button', { name: 'Set up', exact: true }).click();
    const setup = page.getByRole('dialog', { name: 'Set up Composio', exact: true });
    const input = setup.getByLabel('Composio API key', { exact: true });
    await visible(input);
    assert.equal(await input.getAttribute('type'), 'password');
    assert.equal(await input.inputValue(), '');
    assert(await setup.getByRole('button', { name: 'Save key', exact: true }).isDisabled());
    assert.equal(await setup.getByRole('link', { name: 'Get API key', exact: true }).getAttribute('href'), 'https://platform.composio.dev/');
    await page.keyboard.press('Escape'); await hidden(setup); await cleanPage(dialog);
  });
  await check('Skills editor creates Library entry', async () => {
    const dialog = await settings('Skills');
    await dialog.getByRole('button', { name: 'Create skill', exact: true }).first().click();
    const editor = page.getByRole('dialog', { name: 'New skill', exact: true });
    await editor.getByLabel('Name', { exact: true }).fill(skill);
    await editor.getByLabel('When to use', { exact: true }).fill('Use for local smoke requests.');
    await editor.getByLabel(/^Instructions/, { exact: false }).fill('Keep the answer brief.');
    await screenshot('skill-editor-filled');
    await editor.getByRole('button', { name: 'Save version', exact: true }).click();
    await hidden(editor);
    await visible(dialog.getByRole('tabpanel', { name: 'Library', exact: true }).getByRole('button', { name: new RegExp(`^${skill}`) }));
  });
  await check('Install skill from editor and toggle off and on', async () => {
    const dialog = await settings('Skills');
    await dialog.getByRole('tabpanel', { name: 'Library', exact: true }).getByRole('button', { name: new RegExp(`^${skill}`) }).click();
    const editor = page.getByRole('dialog', { name: skill, exact: true });
    await editor.getByRole('combobox', { name: 'Install on agent', exact: true }).click();
    await page.getByRole('option', { name: lead, exact: true }).click();
    await editor.getByRole('button', { name: 'Install v1', exact: true }).click();
    await page.getByRole('alertdialog').getByRole('button', { name: 'Install', exact: true }).click();
    await hidden(page.getByRole('alertdialog'));
    // Close is disabled while saving; its click waits for acknowledgement.
    await editor.locator('.k-dialog__foot').getByRole('button', { name: 'Close', exact: true }).last().click();
    await hidden(editor);
    await dialog.getByRole('tab', { name: 'Installed', exact: true }).click();
    const installed = dialog.getByRole('tabpanel', { name: 'Installed', exact: true });
    await visible(installed.getByText(skill, { exact: true }));
    await visible(installed.getByRole('switch', { name: `Disable ${skill}`, checked: true, exact: true }));
    await screenshot('skill-installed-enabled');
    await installed.getByRole('switch', { name: `Disable ${skill}`, exact: true }).click();
    await visible(installed.getByRole('switch', { name: `Enable ${skill}`, checked: false, exact: true }));
    await screenshot('skill-disabled');
    await installed.getByRole('switch', { name: `Enable ${skill}`, exact: true }).click();
    await visible(installed.getByRole('switch', { name: `Disable ${skill}`, checked: true, exact: true }));
    await cleanPage(dialog);
  });
  await settingsConnectionsSmoke(mcpPort);
  await check('Appearance Light changes theme and background', async () => {
    const dialog = await settings('Appearance');
    await dialog.getByRole('radio', { name: 'Dark', exact: true }).click();
    await page.waitForFunction(() => document.documentElement.dataset.theme === 'dark');
    const background = await page.evaluate(() => getComputedStyle(document.body).backgroundColor);
    await dialog.getByRole('radio', { name: 'Light', exact: true }).click();
    await page.waitForFunction(before => document.documentElement.dataset.theme === 'light' && getComputedStyle(document.body).backgroundColor !== before, background);
    const github = dialog.getByRole('button', { name: /^GitHub/ });
    const beforePalette = await page.evaluate(() => ({ theme: document.documentElement.dataset.colorTheme, brand: getComputedStyle(document.documentElement).getPropertyValue('--color-brand'), background: getComputedStyle(document.body).backgroundColor }));
    await github.click();
    await page.waitForFunction(() => document.documentElement.dataset.colorTheme === 'github');
    const afterPalette = await page.evaluate(() => ({ theme: document.documentElement.dataset.colorTheme, brand: getComputedStyle(document.documentElement).getPropertyValue('--color-brand'), background: getComputedStyle(document.body).backgroundColor }));
    assert.notDeepEqual(afterPalette, beforePalette, 'Theme picker did not change semantic tokens');
    await page.reload();
    await visible(page.getByRole('dialog', { name: 'Settings', exact: true }));
    await page.waitForFunction(() => document.documentElement.dataset.colorTheme === 'github');
    const persistedPalette = await page.evaluate(() => ({ theme: document.documentElement.dataset.colorTheme, brand: getComputedStyle(document.documentElement).getPropertyValue('--color-brand') }));
    assert.equal(persistedPalette.theme, 'github', 'Theme choice did not persist after reload');
    assert.equal(persistedPalette.brand, afterPalette.brand, 'Persisted palette token changed after reload');
    const restored = await settings('Appearance'); await restored.getByRole('button', { name: 'Minarbot', exact: true }).click();
    return `Theme light; palette ${beforePalette.theme} -> ${afterPalette.theme}; persisted after reload.`;
  });
  await check('Sidebar: Routines', async () => {
    await workspace();
    await page.getByRole('navigation', { name: 'Automation', exact: true }).getByRole('button', { name: 'Routines', exact: true }).click();
    await visible(page.getByRole('heading', { name: 'Routines', level: 1, exact: true }));
    await cleanPage();
  });
  await check('Sidebar: Workflows', async () => {
    await workspace();
    await page.getByRole('navigation', { name: 'Automation', exact: true }).getByRole('button', { name: 'Workflows', exact: true }).click();
    await visible(page.getByRole('heading', { name: 'Workflows', level: 1, exact: true }));
    await cleanPage();
  });
  await check('Sidebar header New menu opens agent, project and machine flows', async () => {
    await workspace();
    for (const name of ['New agent', 'New project']) {
      const dialog = await openNew(name);
      const tab = await page.locator('.sidebar__tabs [role="tab"][aria-selected="true"]').textContent();
      await page.keyboard.press('Control+2');
      assert.equal(await page.locator('.sidebar__tabs [role="tab"][aria-selected="true"]').textContent(), tab, 'Sidebar shortcut escaped modal');
      await page.keyboard.press('Escape'); await hidden(dialog);
    }
    await page.getByRole('button', { name: 'Create new', exact: true }).click();
    assert.deepEqual(await page.getByRole('menuitem').allTextContents(), ['New conversationCtrl/Cmd+Shift+O', 'New agent', 'Import template', 'New project', 'Add a machine']);
    await page.getByRole('menuitem', { name: 'Add a machine', exact: true }).click();
    await visible(page.getByRole('heading', { name: 'Machines', exact: true }));
    await workspace();
  });
  await check('Delegated task row renders status', async () => {
    await agentChat();
    const message = page.getByRole('textbox', { name: `Message ${lead}`, exact: true });
    await message.fill('Delegated smoke');
    await message.press('Enter');
    const row = page.locator('.inline-delegation').filter({ hasText: helper });
    await row.waitFor({ state: 'attached' });
    await visible(row);
    await visible(row.getByText('Delegated smoke', { exact: true }));
    await visible(row.getByText('Child smoke result', { exact: true }));
    assert.equal(await page.getByText(/Inter-agent messages/).count(), 0);
    for (const theme of ['dark', 'light']) {
      await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
      await screenshot(`delegated-task-row-${theme}`);
    }
    await cleanPage();
  });
  await check('Chats row menus rename, archive, restore and delete; local and peer deliveries stay grouped', async () => {
    const post = async (path, body) => { const response = await fetch(origin + path, { method: 'POST', headers: { Origin: origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body) }); const value = await response.json(); assert(response.ok, JSON.stringify(value)); return value; };
    const directory = await fetch(origin + '/api/agents').then(response => response.json());
    const target = directory.agents.find(agent => agent.name === helper);
    const peer = await post('/api/mcp-server/peer-tokens', { id: randomUUID(), name: 'Minarbot: omarchy', scopes: ['agents:message'] });
    const response = await fetch(origin + '/mcp', { method: 'POST', headers: { 'Content-Type': 'application/json', Accept: 'application/json, text/event-stream', Authorization: `Bearer ${peer.token}` }, body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'send_message', arguments: { requestId: randomUUID(), recipient: target.id, text: 'Inspect cross-machine release notes' } } }) });
    const delivery = await response.json(); assert(response.ok && !delivery.result?.isError, JSON.stringify(delivery));
    await workspace(); await page.getByRole('navigation', { name: 'Choose agent', exact: true }).getByRole('button', { name: new RegExp(`^${helper} `) }).click();
    await visible(page.getByRole('main', { name: `Conversation ${helper}`, exact: true }));
    const show = page.getByRole('button', { name: 'Show details', exact: true }); if (await show.isVisible()) await show.click();
    const details = page.getByRole('complementary', { name: 'Conversation details', exact: true }); await details.getByRole('tab', { name: 'Chats', exact: true }).click();
    const group = details.getByRole('button', { name: /^From other agents/ }); await visible(group); assert.equal(await group.getAttribute('aria-expanded'), 'false'); await group.click();
    await visible(details.getByRole('button', { name: `${lead} asked: Delegated smoke`, exact: true }));
    const peerName = 'Minarbot on omarchy: Inspect cross-machine release notes'; await visible(details.getByRole('button', { name: peerName, exact: true }));
    const peerMenu = details.getByRole('button', { name: `Menu conversation ${peerName}`, exact: true });
    for (const theme of ['dark', 'light']) {
      await page.evaluate(theme => document.documentElement.dataset.theme = theme, theme); await peerMenu.click();
      await visible(page.getByRole('menuitem', { name: 'Rename', exact: true })); await screenshot(`chats-agent-group-row-menu-${theme}`); await page.keyboard.press('Escape');
    }
    await details.getByRole('button', { name: 'Menu conversation Main chat', exact: true }).click(); assert.equal(await page.getByRole('menuitem', { name: 'Delete', exact: true }).count(), 0); await page.keyboard.press('Escape');
    await details.getByRole('button', { name: 'New conversation', exact: true }).click();
    await visible(page.getByRole('main', { name: 'Conversation New conversation', exact: true }));
    assert.equal(await page.getByRole('dialog', { name: 'New conversation', exact: true }).count(), 0);
    let menu = details.getByRole('button', { name: 'Menu conversation New conversation', exact: true }); await visible(menu); await menu.focus(); await page.keyboard.press('Enter'); await page.keyboard.press('Enter');
    const rename = page.getByRole('textbox', { name: 'Conversation name', exact: true }); await visible(rename); await rename.fill('Renamed tidy work'); await rename.press('Enter'); await hidden(rename);
    let dialog;
    await details.getByRole('button', { name: 'Menu conversation Renamed tidy work', exact: true }).click(); await page.getByRole('menuitem', { name: 'Archive', exact: true }).click();
    dialog = page.getByRole('alertdialog', { name: 'Archive conversation?', exact: true }); await dialog.getByRole('button', { name: 'Archive conversation', exact: true }).click(); await hidden(dialog); await hidden(details.getByRole('button', { name: 'Renamed tidy work', exact: true }));
    await details.getByRole('button', { name: 'Conversation filters', exact: true }).click(); await page.getByRole('menuitemcheckbox', { name: 'Show archived', exact: true }).click(); await visible(details.getByRole('button', { name: 'Renamed tidy work Archived', exact: true }));
    await details.getByRole('button', { name: 'Menu conversation Renamed tidy work', exact: true }).click(); await page.getByRole('menuitem', { name: 'Restore', exact: true }).click(); await visible(details.getByRole('button', { name: 'Renamed tidy work', exact: true }));
    await details.getByRole('button', { name: 'Renamed tidy work', exact: true }).click();
    await page.locator('.chat__pill').click(); await visible(page.getByRole('menuitem', { name: 'Archive', exact: true })); await visible(page.getByRole('menuitem', { name: 'Delete', exact: true })); await page.keyboard.press('Escape');
    await details.getByRole('button', { name: 'Menu conversation Renamed tidy work', exact: true }).click(); await page.getByRole('menuitem', { name: 'Delete', exact: true }).click();
    dialog = page.getByRole('alertdialog', { name: 'Delete conversation?', exact: true }); await dialog.getByRole('button', { name: 'Cancel', exact: true }).click(); await hidden(dialog); await visible(details.getByRole('button', { name: 'Renamed tidy work', exact: true }));
    await details.getByRole('button', { name: 'Menu conversation Renamed tidy work', exact: true }).click(); await page.getByRole('menuitem', { name: 'Delete', exact: true }).click();
    dialog = page.getByRole('alertdialog', { name: 'Delete conversation?', exact: true }); await dialog.getByRole('button', { name: 'Delete conversation', exact: true }).click(); await hidden(dialog); await hidden(details.getByRole('button', { name: 'Renamed tidy work', exact: true })); await cleanPage();
    await agentChat();
  });
  await check('Details tabs support arrows, remember selection, and reject invalid storage', async () => {
    await agentChat();
    const show = page.getByRole('button', { name: 'Show details', exact: true }); if (await show.isVisible()) await show.click();
    const details = page.getByRole('complementary', { name: 'Conversation details', exact: true });
    const chats = details.getByRole('tab', { name: 'Chats', exact: true });
    await chats.focus(); await page.keyboard.press('ArrowRight');
    await page.waitForFunction(() => document.querySelector('.right-panel > .k-tabs [role=tab][data-state=active]')?.textContent === 'Files');
    await hidden(details.getByRole('button', { name: /^Queue / }));
    await hidden(details.getByRole('region', { name: 'Upcoming routines', exact: true }));
    await hidden(details.getByRole('button', { name: /^Delegated smoke/ }));
    await page.reload();
    await visible(details.getByRole('tabpanel', { name: 'Files', exact: true }));
    await details.getByRole('tab', { name: 'Files', exact: true }).click();
    assert.equal(await details.getByRole('button', { name: 'Agent memory', exact: true }).count(), 0);
    await hidden(details.getByRole('button', { name: 'Workspace files', exact: true }));
    await hidden(details.getByRole('region', { name: 'Source control', exact: true }));
    const directory = await fetch(origin + '/api/agents').then(response => response.json());
    const key = `minarbot:right-panel:${directory.agents.find(agent => agent.name === lead).id}`;
    assert.equal(await page.evaluate(key => localStorage.getItem(key), key), '"files"', 'Selected tab was not persisted');
    await page.evaluate(key => localStorage.setItem(key, 'invalid'), key);
    await page.reload();
    await visible(details.getByRole('tabpanel', { name: 'Chats', exact: true }));
    await chats.focus(); await page.keyboard.press('End');
    await page.waitForFunction(() => document.querySelector('.right-panel > .k-tabs [role=tab][data-state=active]')?.textContent === 'Files');
    await page.keyboard.press('Home');
    await page.waitForFunction(() => document.querySelector('.right-panel > .k-tabs [role=tab][data-state=active]')?.textContent === 'Chats');
  });
  await check('Details panel closes and reopens', async () => {
    await agentChat();
    if (await page.getByRole('button', { name: 'Show details', exact: true }).isVisible()) await page.getByRole('button', { name: 'Show details', exact: true }).click();
    await visible(page.getByRole('complementary', { name: 'Conversation details', exact: true }));
    await page.getByRole('button', { name: 'Close details', exact: true }).click();
    await hidden(page.getByRole('complementary', { name: 'Conversation details', exact: true }));
    await page.getByRole('button', { name: 'Show details', exact: true }).click();
    await visible(page.getByRole('complementary', { name: 'Conversation details', exact: true }));
    const computer = page.getByRole('region', { name: 'Computer', exact: true });
    await hidden(computer);
    assert.equal(await computer.getByRole('button').count(), 0);

  });
  await check('Details resize by keyboard, retain width, and keep compact row within bounds', async () => {
    const details = page.getByRole('complementary', { name: 'Conversation details', exact: true });
    const handle = details.getByRole('separator', { name: 'Resize details', exact: true });
    await handle.focus(); await page.keyboard.press('Home');
    await page.waitForFunction(() => document.querySelector('.details')?.getBoundingClientRect().width === 280);
    assert(await details.evaluate(el => el.scrollWidth <= el.clientWidth + 1), 'Details overflow at minimum width');
    assert.equal(await details.getByRole('tab', { name: 'Computer', exact: true }).count(), 0);
    for (const theme of ['dark', 'light']) {
      await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
      await screenshot(`details-min-width-${theme}`);
    }
    await page.keyboard.press('ArrowLeft');
    assert.equal(await handle.getAttribute('aria-valuenow'), '296');
    await page.reload();
    await visible(details);
    assert.equal(await details.getByRole('separator', { name: 'Resize details', exact: true }).getAttribute('aria-valuenow'), '296');
    await page.evaluate(() => { for (const key of Object.keys(localStorage)) if (key.startsWith('minarbot:right-panel-width:')) localStorage.setItem(key, '360'); }); await page.reload();
  });
  await widePanelsSmoke({ check, page, agentChat, settings, visible, hidden, screenshot, viewport: receipt.viewport });
  await check('Chats search, Files attachment shortcut, and section memory', async () => {
    await agentChat();
    const details = page.getByRole('complementary', { name: 'Conversation details', exact: true });
    await details.getByRole('tab', { name: 'Chats', exact: true }).click();
    assert.equal(await details.getByRole('searchbox').count(), 0);
    await page.keyboard.press('Control+f');
    await page.getByRole('searchbox', { name: 'Find in conversation', exact: true }).fill('Direct smoke message');
    const find = page.getByRole('search', { name: 'Find in conversation', exact: true });
    await visible(find);
    await visible(find.getByRole('status').filter({ hasText: /^[1-9][0-9]* of [1-9][0-9]*$/ }));
    assert(await page.evaluate(() => CSS.highlights.get('message-find').size > 0));
    await page.screenshot({ path: join(root, 'app/test-support/ui-smoke/artifacts/ux-right-panel/integration-find.png'), animations: 'disabled' });
    await find.getByRole('button', { name: 'Next match', exact: true }).click();
    await find.getByRole('searchbox').press('Escape');
    await hidden(find);
    assert.equal(await page.evaluate(() => CSS.highlights.has('message-find')), false);
    const chooser = page.waitForEvent('filechooser');
    await page.getByRole('button', { name: 'Attach conversation file', exact: true }).click();
    await (await chooser).setFiles([]);
    await details.getByRole('tab', { name: 'Files', exact: true }).click();
    await visible(details.getByRole('tabpanel', { name: 'Files', exact: true }));
    await visible(details.getByText('Directory is empty on this page.', { exact: true }));
    try {
      await workspace();
      await sidebarTab('Projects');
      await page.getByRole('button', { name: new RegExp(`^# general in ${projectName}`) }).click();
      assert.equal(await details.getByRole('button', { name: 'Mark as read', exact: true }).count(), 0);
      await page.waitForFunction(async name => { const channels=await (await fetch('/api/channels')).json(); const projects=await (await fetch('/api/projects')).json(); const project=projects.projects.find(item=>item.name===name); const room=channels.find(item=>item.id===project.channel); return room.activity === room.read_position; }, projectName);
      await details.getByRole('tab', { name: 'Members', exact: true }).click();
      await hidden(details.getByRole('button', { name: /^Queue / }));
      await hidden(details.getByRole('region', { name: 'Upcoming routines', exact: true }));
      await hidden(details.getByRole('button', { name: 'Automation', exact: true }));
      await page.locator('.chat__pill').click();
      await visible(page.getByRole('menuitem', { name: 'Section memory', exact: true }));
      await hidden(page.getByRole('menuitem', { name: 'Agent memory', exact: true }));
      await page.getByRole('menuitem', { name: 'Section memory', exact: true }).click();
      await visible(page.getByRole('heading', { name: 'Memory for this section', exact: true }));
      await page.getByRole('button', { name: 'Back', exact: true }).click();
      await page.waitForFunction(() => document.activeElement?.classList.contains('chat__pill'));
    } finally { await agentChat(); }
  });
  await desktopViewSmoke({ check, context, page, lead, agentChat, visible, hidden, screenshot, captureComputerThemes, viewport: receipt.viewport });
  await computerViewerSmoke();
  await check('History summary in profile updates on focus without reload', async () => {
    const details = page.getByRole('complementary', { name: 'Conversation details', exact: true });
    await details.getByRole('tab', { name: 'Chats', exact: true }).click();
    await details.getByRole('button', { name: 'Edit profile', exact: true }).click();
    const profile = page.getByRole('dialog', { name: 'Agent profile', exact: true }); await profile.getByRole('tab', { name: 'Advanced', exact: true }).click();
    await profile.locator('.summary-row').scrollIntoViewIfNeeded(); await visible(profile.getByText('None yet', { exact: true }));
    const loaded = page.waitForResponse(response => response.url().includes('/compaction?after=0'));
    await page.evaluate(() => window.dispatchEvent(new Event('focus'))); await loaded;
    assert.equal(await profile.getByRole('button', { name: 'Reload summary', exact: true }).count(), 0);
    await profile.getByRole('button', { name: 'Cancel', exact: true }).click();
  });
  await check('Inline recording cancel releases capture and unlocks composer upload', async () => {
    await page.evaluate(() => {
      window.__recordingTrackStops = 0;
      navigator.mediaDevices.getUserMedia = async () => ({ getTracks: () => [{ stop: () => window.__recordingTrackStops++ }] });
      window.MediaRecorder = class {
        static isTypeSupported() { return true; }
        state = 'inactive';
        start() { this.state = 'recording'; }
        stop() { this.state = 'inactive'; queueMicrotask(() => this.onstop?.()); }
      };
    });
    try {
      await page.getByRole('button', { name: 'Record audio', exact: true }).click();
      const recorder = page.getByRole('group', { name: 'Audio recording', exact: true });
      await visible(recorder.getByRole('timer', { name: 'Recording duration', exact: true }));
      assert(await page.getByRole('button', { name: 'Attach conversation file', exact: true }).isDisabled());
      await recorder.getByRole('button', { name: 'Cancel recording', exact: true }).click();
      await hidden(recorder);
      assert((await page.evaluate(() => window.__recordingTrackStops)) > 0);
      assert(!(await page.getByRole('button', { name: 'Attach conversation file', exact: true }).isDisabled()));
    } finally { await page.reload(); }
  });
  async function routineThemes(label) {
    for (const theme of ['dark', 'light']) {
      await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
      assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), theme);
      await screenshot(`routine-${label}-${theme}`);
    }
  }
  async function routineSelect(scope, label, choice) {
    await scope.getByRole('combobox', { name: label, exact: true }).click();
    await page.getByRole('option', { name: choice, exact: true }).click();
  }
  async function routineHub() {
    await page.getByRole('navigation', { name: 'Automation', exact: true }).getByRole('button', { name: 'Routines', exact: true }).click();
    await visible(page.getByRole('heading', { name: 'Routines', exact: true }));
  }
  await check('Routines: Manage filters owner, scheduled dialog saves and survives reload', async () => {
    await agentChat();
    if (await page.getByRole('button', { name: 'Show details', exact: true }).isVisible()) await page.getByRole('button', { name: 'Show details', exact: true }).click();
    const details = page.getByRole('complementary', { name: 'Conversation details', exact: true });
    await details.getByRole('tab', { name: 'Chats', exact: true }).click(); await page.locator('.chat__pill').click(); await page.getByRole('menuitem', { name: 'Routines', exact: true }).click();
    await visible(page.getByRole('heading', { name: 'Routines', exact: true }));
    assert.equal((await page.getByRole('combobox', { name: 'Agent', exact: true }).textContent()).trim(), lead);
    await page.getByRole('button', { name: 'New routine', exact: true }).first().click();
    const dialog = page.getByRole('dialog', { name: 'New routine', exact: true }); await visible(dialog);
    assert.equal((await dialog.getByRole('combobox', { name: 'Owner agent', exact: true }).textContent()).trim(), lead);
    await dialog.getByLabel('Name', { exact: true }).fill('Morning briefing'); await dialog.getByLabel(/^Instructions/, { exact: false }).fill('Review the workspace and give a short morning summary.');
    await routineSelect(dialog, 'Schedule', 'Weekdays');
    await dialog.getByLabel('Time', { exact: true }).fill('09:00');
    await dialog.getByLabel('Time zone', { exact: true }).fill('Asia/Jakarta');
    await routineSelect(dialog, 'Project', projectName);
    await routineSelect(dialog, 'Report to', `${projectName} / # general`);
    await page.waitForFunction(() => document.querySelectorAll('.routine-preview li').length === 3);
    assert.equal(await dialog.getByRole('region', { name:'Next runs', exact:true }).locator('li').count(), 3);
    const dates=await dialog.locator('.routine-preview time').evaluateAll(nodes=>nodes.map(node=>node.dateTime));
    assert(dates.every(date=>{const day=new Date(date).getUTCDay();return day!==0&&day!==6;}));
    await routineThemes('new-dialog');
    assert(await dialog.evaluate(el => el.scrollWidth <= el.clientWidth + 1), 'Routine dialog overflows');
    await dialog.getByRole('button', { name: 'Save routine', exact: true }).click(); await hidden(dialog);
    await visible(page.getByRole('button', { name: 'Morning briefing', exact: true }));
    await page.reload(); await agentChat(); await details.getByRole('tab', { name: 'Chats', exact: true }).click();
    await details.getByRole('button', { name: /^Upcoming / }).click(); await visible(details.getByRole('region', { name: 'Upcoming routines', exact: true }).getByText('Morning briefing', { exact: true }));
    await details.getByRole('button', { name: 'Manage', exact: true }).click();
    await visible(page.getByRole('button', { name: 'Morning briefing', exact: true })); await cleanPage();
  });
  await check('Routines: two-agent list, owner picker, search and trigger/status filters', async () => {
    await routineHub(); await page.getByRole('button', { name: 'New routine', exact: true }).first().click();
    const dialog = page.getByRole('dialog', { name: 'New routine', exact: true });
    await routineSelect(dialog, 'Owner agent', helper); await dialog.getByLabel('Name', { exact: true }).fill('Release review');
    await dialog.getByLabel(/^Instructions/, { exact: false }).fill('Check the release notes and list anything still missing.');
    await routineSelect(dialog, 'Trigger', 'Manual'); await dialog.getByRole('button', { name: 'Save routine', exact: true }).click(); await hidden(dialog);
    await visible(page.getByRole('button', { name: 'Release review', exact: true }));
    const leadRow = page.locator('tbody tr').filter({ hasText: 'Morning briefing' }), helperRow = page.locator('tbody tr').filter({ hasText: 'Release review' });
    assert.equal((await leadRow.locator('td').nth(1).locator('.routine-owner > span:last-child').textContent()).trim(), lead); assert.equal((await helperRow.locator('td').nth(1).locator('.routine-owner > span:last-child').textContent()).trim(), helper);
    await routineThemes('list');
    assert(await page.locator('.routines-hub').evaluate(el => el.scrollWidth <= el.clientWidth + 1), 'Routines page overflows');
    assert(await page.locator('.routine-table').evaluate(el => el.getBoundingClientRect().right <= innerWidth), 'Routine columns must fit the viewport');
    await routineSelect(page, 'Project', projectName); await visible(leadRow); await hidden(helperRow); await routineSelect(page, 'Project', 'All projects');
    await routineSelect(page, 'Agent', helper); await hidden(leadRow); await visible(helperRow); await routineSelect(page, 'Agent', 'All agents');
    await routineSelect(page, 'Trigger', 'Manual'); await hidden(leadRow); await visible(helperRow); await routineSelect(page, 'Trigger', 'All triggers');
    await page.getByRole('searchbox', { name: 'Search routines', exact: true }).fill('Morning'); await visible(leadRow); await hidden(helperRow);
    await page.getByRole('searchbox', { name: 'Search routines', exact: true }).fill(''); await visible(helperRow); await cleanPage();
  });
  await check('Routines: enabled switch and detail Pause/Resume update revision', async () => {
    const row = page.locator('tbody tr').filter({ hasText: 'Morning briefing' });
    await row.getByRole('switch', { name: 'Enable Morning briefing', exact: true }).click();
    await page.waitForFunction(() => document.querySelector('button[aria-label="Enable Morning briefing"]')?.getAttribute('aria-checked') === 'false');
    await routineSelect(page, 'Status', 'Disabled'); await visible(row); await hidden(page.getByRole('button', { name: 'Release review', exact: true }));
    await routineSelect(page, 'Status', 'All statuses'); await row.getByRole('button', { name: 'Morning briefing', exact: true }).click();
    await visible(page.getByRole('tab', { name: 'Overview', exact: true })); await visible(page.getByRole('button', { name: 'Resume', exact: true }));
    await page.getByRole('button', { name: 'Resume', exact: true }).click(); await visible(page.getByRole('button', { name: 'Pause', exact: true }));
    await routineThemes('overview'); await cleanPage();
  });
  await check('Routines: Flow tab connects an agent, persists graph, and shows badge', async () => {
    await page.getByRole('tab', { name: 'Flow', exact: true }).click(); await visible(page.getByRole('heading', { name: 'Agent flow', exact: true }));
    await routineSelect(page, 'To agent', helper); await page.getByRole('button', { name: 'Connect', exact: true }).click();
    await page.getByRole('button', { name: 'Save flow', exact: true }).click();
    await page.waitForFunction(() => !Object.keys(sessionStorage).some(key => key.startsWith('minarbot:flow:')));
    await page.getByRole('button', { name: 'Reload flow', exact: true }).click();
    await visible(page.getByText(`${lead} to ${helper}`, { exact: true })); await routineThemes('flow');
    await page.getByRole('button', { name: 'All routines', exact: true }).click();
    await visible(page.locator('tbody tr').filter({ hasText: 'Morning briefing' }).getByText('2 agents', { exact: true })); await cleanPage();
  });
  await check('Routines: Run now records a real run and opens its conversation', async () => {
    await page.getByRole('button', { name: 'Morning briefing', exact: true }).click(); await page.getByRole('button', { name: 'Run now', exact: true }).click();
    await visible(page.getByText('Done', { exact: true }).first()); await visible(page.getByRole('button', { name: 'Open run', exact: true }));
    await routineThemes('overview');
    await page.getByRole('tab', { name: 'Flow', exact: true }).click(); await visible(page.getByRole('button', { name: `Open step ${helper}`, exact: true })); await routineThemes('flow');
    await page.getByRole('tab', { name: 'Overview', exact: true }).click(); await page.getByRole('button', { name: 'Open run', exact: true }).click();
    await visible(page.getByRole('textbox', { name: `Message ${lead}`, exact: true })); await visible(page.getByRole('region', { name: 'Conversation history', exact: true }).getByText('Review the workspace and give a short morning summary.', { exact: true }));
    await routineHub(); await visible(page.locator('tbody tr').filter({ hasText: 'Morning briefing' }).getByRole('button', { name: /Done/ })); await cleanPage();
  });
  await check('Routines: section receives one report without fan-out, Members lists it and full-run link works', async () => {
    const project=(await fetch(origin+'/api/projects').then(response=>response.json())).projects.find(item=>item.name===projectName);
    const routine=(await fetch(origin+'/api/routines?'+new URLSearchParams({search:'Morning briefing'})).then(response=>response.json())).routines[0];
    assert.equal(routine.project,project.id);assert.equal(routine.report_to,project.channel);assert.deepEqual(routine.schedule,{kind:'cron',expression:'0 9 * * 1-5'});assert.equal(routine.schedule_zone,'Asia/Jakarta');
    await sidebarTab('Projects'); await page.getByRole('button',{name:new RegExp(`^# general in ${projectName}`)}).click();
    const report=page.getByRole('region',{name:'Routine report: Morning briefing',exact:true});await visible(report);
    assert.match(await report.textContent(),/Done/);assert.match(await report.textContent(),/Smoke reply/);
    if(await page.getByRole('button',{name:'Show details',exact:true}).isVisible())await page.getByRole('button',{name:'Show details',exact:true}).click();
    const details=page.getByRole('complementary',{name:'Conversation details',exact:true});await details.getByRole('tab',{name:'Members',exact:true}).click();
    const upcoming = details.getByRole('region',{name:'Routines reporting here',exact:true});
    await upcoming.getByRole('button',{name:/^Upcoming [1-9]/}).click();
    await visible(upcoming.getByText('Morning briefing',{exact:true}));
    await routineThemes('section-report');
    const messages=await fetch(origin+`/api/channels/${project.channel}/turns`).then(response=>response.json());
    const reports=messages.filter(row=>row.routineReport?.name==='Morning briefing');assert.equal(reports.length,1);assert.equal(reports[0].room,null);assert.equal(reports[0].routing,'report');
    await report.getByRole('link',{name:'Open full run',exact:true}).click();await visible(page.getByRole('region',{name:'Conversation history',exact:true}).getByText('Review the workspace and give a short morning summary.',{exact:true}));
    await routineHub();await cleanPage();
  });
  await check('Routines: edit dialog and confirmed delete retain other routines', async () => {
    await page.getByRole('button', { name: 'Release review', exact: true }).click(); await page.getByRole('button', { name: 'Edit routine', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: 'Edit routine', exact: true }); await visible(dialog);
    assert(await dialog.getByRole('combobox', { name: 'Owner agent', exact: true }).isDisabled()); await dialog.getByLabel('Name', { exact: true }).fill('Release checklist');
    await dialog.getByRole('button', { name: 'Save routine', exact: true }).click(); await hidden(dialog); await visible(page.getByRole('heading', { name: 'Release checklist', exact: true }));
    await page.getByRole('button', { name: 'Routine actions', exact: true }).click(); await page.getByRole('menuitem', { name: 'Delete routine', exact: true }).click();
    const confirmation = page.getByRole('alertdialog'); await visible(confirmation); await confirmation.getByRole('button', { name: 'Cancel', exact: true }).click();
    await visible(page.getByRole('heading', { name: 'Release checklist', exact: true }));
    await page.getByRole('button', { name: 'Routine actions', exact: true }).click(); await page.getByRole('menuitem', { name: 'Delete routine', exact: true }).click();
    await confirmation.getByRole('button', { name: 'Delete routine', exact: true }).click(); await hidden(confirmation);
    await visible(page.getByRole('heading', { name: 'Routines', exact: true })); await hidden(page.getByRole('button', { name: 'Release checklist', exact: true }));
    await visible(page.getByRole('button', { name: 'Morning briefing', exact: true })); await cleanPage();
    await sidebarTab('Agents'); await page.getByRole('navigation', { name: 'Choose agent', exact: true }).getByRole('button', { name: new RegExp(`^${lead} `) }).click();
    if (await page.getByRole('button', { name: 'Show details', exact: true }).isVisible()) await page.getByRole('button', { name: 'Show details', exact: true }).click();
    const details = page.getByRole('complementary', { name: 'Conversation details', exact: true });
    await details.getByRole('tab', { name: 'Chats', exact: true }).click(); await details.getByRole('button', { name: 'Main chat', exact: true }).click();
    await visible(page.getByRole('main', { name: `Conversation ${lead}`, exact: true }));
  });
  for (const entry of ['keyboard shortcut', 'sidebar button']) await check(`Settings focus containment and restoration: ${entry}`, async () => {
    await agentChat();
    const trigger = page.getByRole('button', { name: /^Settings(?: - Update available)?$/ });
    if (entry === 'keyboard shortcut') {
      await page.getByRole('textbox', { name: `Message ${lead}`, exact: true }).focus();
      await page.keyboard.press('Control+,');
    } else await trigger.click();
    const dialog = page.getByRole('dialog', { name: 'Settings', exact: true });
    await visible(dialog);
    for (const section of ['Skills', 'Backup']) {
      await settings(section, section === 'Backup' ? 'Backup & restore' : section);
      await focusContained(dialog);
    }
    await page.keyboard.press('Escape');
    await hidden(dialog);
    await focusRestored(trigger);
  });
  await check('Nested Settings skill confirm restores editor focus', async () => {
    const dialog = await settings('Skills');
    await dialog.getByRole('button', { name: 'Create skill', exact: true }).first().click();
    const editor = page.getByRole('dialog', { name: 'New skill', exact: true });
    await visible(editor);
    await editor.getByLabel('Name', { exact: true }).fill('unsaved-focus-check');
    await page.keyboard.press('Escape');
    const discard = page.getByRole('alertdialog', { name: 'Discard skill changes?', exact: true });
    await visible(discard);
    for (const key of [...Array(4).fill('Tab'), ...Array(4).fill('Shift+Tab')]) {
      await page.keyboard.press(key);
      assert(await discard.evaluate(element => element.contains(document.activeElement)), 'Focus escaped discard confirm');
    }
    // Modal ancestors leave the accessibility tree while a child owns focus.
    // Check mounted Settings separately, then its accessible role after dismissal.
    assert.equal(await page.locator('.app-settings').count(), 1, 'Escape removed Settings below editor');
    await page.keyboard.press('Escape');
    await hidden(discard);
    await visible(editor);
    assert.equal(await page.locator('.app-settings').count(), 1, 'Escape removed Settings below confirm');
    // Radix restores focus after the closing layer unmounts, on the next task.
    try {
      await page.waitForFunction(() => document.activeElement?.closest('.k-dialog[role="dialog"]'), null, { timeout: 1000 });
    } catch {
      const state = await page.evaluate(() => ({
        active: { tag: document.activeElement?.tagName, id: document.activeElement?.id, role: document.activeElement?.getAttribute('role') },
        settingsMounted: !!document.querySelector('.app-settings'),
        dialogs: [...document.querySelectorAll('[role="dialog"], [role="alertdialog"]')].map(element => ({ role: element.role, hidden: element.getAttribute('aria-hidden'), title: element.querySelector('h2, .app-settings__heading')?.textContent })),
      }));
      assert.fail(`Escape closed skill confirm but focus did not return inside editor: ${JSON.stringify(state)}`);
    }
    await editor.locator('.k-dialog__foot').getByRole('button', { name: 'Close', exact: true }).first().click();
    await discard.getByRole('button', { name: 'Discard changes', exact: true }).click();
    await hidden(editor);
    await visible(dialog);
    assert(await dialog.evaluate(element => element.contains(document.activeElement)), 'Focus left Settings after nested dialog closed');
    await page.keyboard.press('Escape');
    await hidden(dialog);
  });
  await check('Nested Settings backup confirm consumes Escape first', async () => {
    await workspace();
    const dialog = await settings('Backup', 'Backup & restore');
    await dialog.getByRole('button', { name: 'Download', exact: true }).click();
    const confirm = page.getByRole('alertdialog', { name: 'Download workspace backup?', exact: true });
    await visible(confirm);
    await focusRestored(confirm.getByRole('button', { name: 'Cancel', exact: true }));
    await page.keyboard.press('Tab');
    await page.keyboard.press('Shift+Tab');
    assert(await confirm.evaluate(element => element.contains(document.activeElement)), 'Focus escaped backup confirm');
    await page.keyboard.press('Escape');
    await hidden(confirm);
    await visible(dialog);
    await page.keyboard.press('Escape');
    await hidden(dialog);
    await focusRestored(page.getByRole('button', { name: /^Settings(?: - Update available)?$/ }));
  });
  await check('Workflow editor graph layout and contrast', async () => {
    await workspace();
    workflow = await page.evaluate(async name => {
      const directory = await (await fetch('/api/agents')).json();
      const owner = directory.agents.find(agent => agent.name === name);
      if (!owner) throw new Error('Workflow owner missing');
      const response = await fetch(`/api/agents/${owner.id}/workflows`, {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ id: crypto.randomUUID(), revision: 0, name: 'Smoke connected graph', graph: {
          entry: 'work', maxSteps: 4, nodes: [
            { id: 'work', kind: 'agent', agent: owner.id, prompt: 'Reply briefly to {{input}}.', next: 'output' },
            { id: 'output', kind: 'output' },
          ], positions: { work: { x: 64, y: 64 }, output: { x: 344, y: 64 } },
        } }),
      });
      const body = await response.json();
      if (!response.ok) throw new Error(`Workflow create HTTP ${response.status}: ${JSON.stringify(body)}`);
      return body;
    }, lead);
    await page.getByRole('navigation', { name: 'Automation', exact: true }).getByRole('button', { name: 'Workflows', exact: true }).click();
    await page.getByText(workflow.name, { exact: true }).click();
    await page.getByRole('button', { name: 'Edit workflow', exact: true }).click();
    await visible(page.getByRole('heading', { name: 'Edit workflow', exact: true }));
    for (const viewport of [{ width: 1280, height: 800 }, receipt.viewport]) for (const theme of ['dark', 'light']) {
      await page.setViewportSize(viewport);
      await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
      await graphLayout(`editor-${theme}-${viewport.width}`);
      await screenshot(`editor-graph-${theme}-${viewport.width}`);
    }
    await cleanPage();
  });
  await check('Completed workflow run graph layout and contrast', async () => {
    assert(workflow, 'Workflow fixture was not created');
    // Reopen from the directory so editor assertion failures do not block run coverage.
    await workspace();
    await page.getByRole('navigation', { name: 'Automation', exact: true }).getByRole('button', { name: 'Workflows', exact: true }).click();
    await page.getByText(workflow.name, { exact: true }).click();
    await page.getByLabel('Workflow input', { exact: true }).fill('Complete the graph smoke run');
    const response = page.waitForResponse(response => new URL(response.url()).pathname === `/api/workflows/${workflow.id}/run` && response.request().method() === 'POST');
    await page.getByRole('button', { name: `Run version ${workflow.revision}`, exact: true }).click();
    const started = await response;
    assert(started.ok(), `Run start HTTP ${started.status()}`);
    const run = await started.json();
    await page.waitForFunction(async id => {
      const response = await fetch(`/api/workflow-runs/${id}`);
      const run = await response.json();
      if (['failed', 'cancelled'].includes(run.status)) throw new Error(`Workflow ${run.status}: ${run.error}`);
      return run.status === 'done';
    }, run.id, { timeout: 20_000 });
    const open = page.getByRole('button', { name: 'Open run', exact: true });
    await visible(open);
    await open.click();
    await visible(page.locator('.workflow-run-node').filter({ hasText: 'Completed' }).first());
    for (const viewport of [{ width: 1280, height: 800 }, receipt.viewport]) for (const theme of ['dark', 'light']) {
      await page.setViewportSize(viewport);
      await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
      await graphLayout(`completed-run-${theme}-${viewport.width}`);
      await screenshot(`completed-run-graph-${theme}-${viewport.width}`);
    }
    await cleanPage();
  });
  await check('Long-name agent enters pending turn', async () => {
    await createAgent(longName);
    const message = page.getByRole('textbox', { name: `Message ${longName}`, exact: true });
    await message.fill(pendingMessage);
    await message.press('Enter');
    await hidden(page.locator('.activity-chip'));
    await visible(page.locator('.composer').getByRole('button', { name: 'Stop', exact: true }));
  });
  await check('Composer queues on Enter without mode select and keeps Stop visible', async () => {
    assert.equal(await page.getByRole('combobox', { name: 'Message mode', exact: true }).count(), 0);
    await visible(page.locator('.composer').getByRole('button', { name: 'Stop', exact: true }));
    const box = page.getByRole('textbox', { name: `Message ${longName}`, exact: true });
    await box.fill('Unsent follow-up');
    await visible(page.getByRole('button', { name: 'Queue message', exact: true }));
    await visible(page.locator('.composer').getByRole('button', { name: 'Stop', exact: true }));
    for (const theme of ['dark', 'light']) {
      await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
      await screenshot(`composer-dispatch-mode-${theme}`);
    }
    await box.fill('');
  });
  for (const viewport of [{ width: 1280, height: 800 }, receipt.viewport]) await check(`Pending chat header avoids overlap: ${viewport.width}x${viewport.height}`, async () => {
    await page.setViewportSize(viewport);
    for (const theme of ['dark', 'light']) {
      await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
      for (const detailsOpen of [true, false]) {
        const show = page.getByRole('button', { name: 'Show details', exact: true });
        const close = page.getByRole('button', { name: 'Close details', exact: true });
        if (detailsOpen && await show.isVisible()) await show.click();
        if (!detailsOpen && await close.isVisible()) await close.click();
        const details = page.getByRole('complementary', { name: 'Conversation details', exact: true });
        if (detailsOpen) await visible(details); else await hidden(details);
        await hidden(page.locator('.activity-chip'));
        const header = page.locator('.chat-row > .chat').locator('.chat__header');
        const title = await header.locator('.chat__pill-name').boundingBox();
        const pill = await header.locator('.chat__pill').boundingBox();
        const chip = null;
        const button = detailsOpen ? null : await show.boundingBox();
        const bounds = await header.boundingBox();
        receipt.headerLayouts.push({ viewport, theme, detailsOpen, title, pill, chip, button, bounds });
        assert(title?.width > 0 && title.height > 0, 'Header title vanished');
        const intersects = (a, b) => a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
        for (const [name, box] of (!detailsOpen ? [['Show details', button]] : [])) {
          assert(box?.width > 0 && box.height > 0, `${name} vanished`);
          assert(!intersects(title, box) && !intersects(pill, box), `Title overlaps ${name}`);
          assert(box.x >= bounds.x && box.x + box.width <= bounds.x + bounds.width + 1, `${name} overflows header`);
        }
        await screenshot(`pending-header-${theme}-${viewport.width}-details-${detailsOpen ? 'open' : 'closed'}`);
      }
    }
  });
  await check('Fork from here creates same-agent side thread and source link, dark and light', async () => {
    releasePending();
    await hidden(page.locator('.activity-chip').filter({ hasText: /^1 message processing$/ }));
    // A slow initial thread refresh must not lose navigation to a newer polling response.
    const threadLists = '**/api/agents/*/threads'; let delayed = false;
    const pendingRoutes = new Set();
    await page.route(threadLists, route => {
      const work = (async () => {
        const response = await route.fetch();
        if (!delayed && (await response.json()).some(thread => thread.fork)) { delayed = true; await delay(6000); }
        await route.fulfill({ response });
      })();
      pendingRoutes.add(work);
      return work.finally(() => pendingRoutes.delete(work));
    });
    const forked = page.waitForResponse(response => /\/turns\/[^/]+\/fork$/.test(new URL(response.url()).pathname) && response.request().method() === 'POST');
    await page.getByRole('button', { name: 'Fork from here', exact: true }).click();
    await hidden(page.getByRole('alertdialog', { name: 'Fork from here?', exact: true }));
    assert((await forked).ok(), 'Fork request failed');
    const header = page.getByLabel('Fork origin', { exact: true });
    await visible(header);
    await visible(page.getByRole('region', { name: 'Inherited messages', exact: true }));
    assert.match(await header.textContent(), /Forked from Main chat/);
    for (const theme of ['dark', 'light']) {
      await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
      await screenshot(`forked-thread-header-${theme}`);
    }
    await header.getByRole('link', { name: 'Main chat', exact: true }).click();
    await visible(page.getByRole('main', { name: `Conversation ${longName}`, exact: true }));
    await hidden(header);
    await Promise.all([...pendingRoutes]);
    await page.unroute(threadLists);
  });
  await check('Source control section, diff dialog and checkpoints in dark and light', async () => {
    releasePending(); await hidden(page.locator('.activity-chip').filter({ hasText: /^1 message processing$/ }));
    const agents = await page.request.get(origin + '/api/agents').then(response => response.json());
    if (!agents.agents.some(agent => agent.id === gitAgent)) {
      const created = await page.request.post(origin + '/api/agents', { headers: { Origin: origin }, data: { runtime: 'api', id: gitAgent, name: gitName, model: models[0], instructions: 'Help with the local Git workspace.' } });
      assert(created.ok(), await created.text());
    }
    await workspace();
    await sidebarTab('Agents');
    await page.getByRole('navigation', { name: 'Choose agent', exact: true }).getByRole('button', { name: new RegExp(`^${gitName} `) }).click();
    await visible(page.getByRole('main', { name: `Conversation ${gitName}`, exact: true }));
    const show = page.getByRole('button', { name: 'Show details', exact: true }); if (await show.isVisible()) await show.click();
    await page.getByRole('complementary', { name: 'Conversation details', exact: true }).getByRole('tab', { name: 'Files', exact: true }).click();
    await visible(page.getByRole('navigation', { name: 'Workspace path', exact: true }));
    const details = page.getByRole('complementary', { name: 'Conversation details', exact: true });
    for (const theme of ['dark', 'light']) {
      await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
      await details.getByRole('tab', { name: 'Files', exact: true }).click();
      await details.getByRole('button', { name: /^Source control/ }).click();
      const tabs = page.getByRole('tablist', { name: 'Conversation panel', exact: true });
      assert.equal(await tabs.getByRole('tab', { name: 'Files', exact: true }).getAttribute('aria-selected'), 'true');
      await details.locator('.right-panel__changed-files').getByRole('button', { name: /notes\.txt/ }).click();
      const diff = details.getByRole('region', { name: 'Unified diff', exact: true });
      await visible(diff.getByText('-Original first line', { exact: true })); await visible(diff.getByText('+Updated first line', { exact: true }));
      assert(await diff.getByLabel('Old line 1', { exact: true }).isVisible()); assert(await diff.getByLabel('New line 1', { exact: true }).isVisible());
      await visible(diff.locator('.is-remove').getByText('---Old dash line', { exact: true })); await visible(diff.locator('.is-add').getByText('+++New plus line', { exact: true }));
      assert(await diff.getByLabel('Old line 3', { exact: true }).isVisible()); assert(await diff.getByLabel('New line 4', { exact: true }).isVisible());
      assert(await diff.evaluate(el => el.scrollWidth <= el.clientWidth + 1), 'Wrapped diff overflowed');
      await details.getByRole('button', { name: 'Wrap lines', exact: true }).click();
      assert(await diff.evaluate(el => el.scrollWidth > el.clientWidth), 'Unwrapped code lines must scroll inside diff');
      await details.getByRole('button', { name: 'Wrap lines', exact: true }).click();
      assert(await details.evaluate(el => el.scrollWidth <= el.clientWidth + 1), 'Diff overflow escaped panel');
      for (const palette of ['minarbot', 'github', 'catppuccin', 'everforest', 'solarized', 'midnight']) {
        await page.evaluate(palette => { document.documentElement.dataset.colorTheme = palette; }, palette); await screenshot(`source-control-diff-${palette}-${theme}`);
      }
      await page.evaluate(() => { document.documentElement.dataset.colorTheme = 'minarbot'; });
      await screenshot(`source-control-diff-${theme}`);
      await tabs.getByRole('tab', { name: 'Chats', exact: true }).click();
      await page.getByRole('complementary', { name: 'Conversation details', exact: true }).getByRole('tab', { name: 'Files', exact: true }).click();
    }
    await send(gitName, 'Capture a checkpoint for this turn');
    await writeFile(join(gitWorkspace, 'notes.txt'), 'Changed after turn\n');
    const restore = page.getByRole('button', { name: 'Restore to before this turn', exact: true }); await visible(restore); await restore.click();
    const confirmation = page.getByRole('dialog', { name: 'Restore to before this turn?', exact: true }); await visible(confirmation);
    await visible(confirmation.getByRole('list', { name: 'Files to restore' }).getByText('notes.txt', { exact: true }));
    await confirmation.getByRole('button', { name: 'Restore files', exact: true }).click(); await hidden(confirmation);
    assert((await readFile(join(gitWorkspace, 'notes.txt'), 'utf8')).startsWith('Updated first line'));
    await cleanPage();
  });
  await detailsDeclutterSmoke({ page, check, origin, screenshot, gitAgent, gitName, models, existing: true });
  await rightPanelUXSmoke({ check, page, origin, root, agentChat });
  await check('RP1 files, source control, preview and scoped preferences in one row', async () => {
    const directory = await page.request.get(origin + '/api/agents').then(r => r.json()), owner = directory.agents.find(agent => agent.name === lead);
    async function post(path, body) { const r = await page.request.post(origin + path, { headers: { Origin: origin }, data: body }); assert(r.ok(), await r.text()); return r.json(); }
    let project = await post('/api/projects', { id: randomUUID(), name: 'RP1 Project', folders: [{ mode: 'new' }], members: [{ agent: owner.id, write: false }] });
    await post(`/api/projects/${project.id}/thread`, { id: randomUUID(), revision: project.revision, agent: owner.id, name: 'RP1 thread' });
    project = await page.request.get(origin + `/api/projects/${project.id}`).then(r => r.json());
    await writeFile(join(project.folders[0], 'panel.txt'), 'RP1 working tree change\n');
    const base = `/api/threads/${project.threads[0].id}`;
    for (const [name, text] of [['panel.html', '<h1>RP1 preview</h1><script>window.BAD=true</script>'], ['panel.md', '# RP1 markdown\nRead in panel.']]) {
      const r = await page.request.post(origin + `${base}/files/${randomUUID()}`, { headers: { Origin: origin, 'Content-Type': 'application/octet-stream', 'X-File-Name': name }, data: text }); assert(r.ok(), await r.text());
    }
    const details = page.getByRole('complementary', { name: 'Conversation details', exact: true }), tabs = details.getByRole('tablist', { name: 'Conversation panel', exact: true });
    async function openPanel() { const show = page.getByRole('button', { name: 'Show details', exact: true }); if (await show.isVisible()) await show.click(); await visible(details); }
    async function openThread() { await sidebarTab('Projects'); await projectHome('RP1 Project'); await page.getByRole('button', { name: /^RP1 thread/ }).click(); await openPanel(); }
    await agentChat(); await openPanel(); await tabs.getByRole('tab', { name: 'Files', exact: true }).click();
    const handle = details.getByRole('separator', { name: 'Resize details', exact: true }); await handle.focus(); await page.keyboard.press('Home'); await page.keyboard.press('ArrowLeft'); await page.keyboard.press('ArrowLeft');
    assert.equal(await handle.getAttribute('aria-valuenow'), '312');
    await openThread(); await tabs.getByRole('tab', { name: 'Files', exact: true }).click();
    await details.getByRole('button', { name: 'Preview panel.md', exact: true }).click(); await visible(details.getByRole('heading', { name: 'RP1 markdown', exact: true })); await details.getByRole('button', { name: 'Close file', exact: true }).click();
    await details.getByRole('button', { name: /^panel.txt/ }).click(); await visible(details.getByRole('region', { name: 'Contents of panel.txt', exact: true })); await details.getByRole('button', { name: 'Close file', exact: true }).click();
    async function surface(name) { await tabs.getByRole('tab', { name: 'Files', exact: true }).click(); const back = details.getByRole('button', { name: 'Back to files', exact: true }); if (await back.isVisible()) await back.click(); await details.getByRole('button', { name }).click(); }
    await surface(/^Source control/); await visible(details.getByText('+RP1 working tree change', { exact: true }));
    await details.getByRole('combobox', { name: 'Diff layout', exact: true }).click(); await page.getByRole('option', { name: 'Split', exact: true }).click(); await visible(details.getByRole('region', { name: 'Split diff', exact: true })); await screenshot('rp1-split-diff');
    await agentChat(); await openPanel(); assert.equal(await handle.getAttribute('aria-valuenow'), '312'); assert.equal(await tabs.getByRole('tab', { name: 'Files', exact: true }).getAttribute('aria-selected'), 'true');
    await openThread(); await visible(details.getByRole('region', { name: 'Unified diff', exact: true }));
    await page.reload(); await openThread(); await visible(details.getByRole('region', { name: 'Unified diff', exact: true }));
    for (const viewport of [{ width: 1440, height: 900 }, { width: 1024, height: 768 }]) {
      await page.setViewportSize(viewport);
      for (const [palette, theme] of [['minarbot', 'dark'], ['minarbot', 'light'], ['catppuccin', 'dark']]) {
        await page.evaluate(({ palette, theme }) => Object.assign(document.documentElement.dataset, { colorTheme: palette, theme }), { palette, theme });
        for (const name of [/^Source control/, 'Preview artifacts']) { await surface(name); if (name === 'Preview artifacts') await visible(details.frameLocator('iframe').getByRole('heading', { name: 'RP1 preview', exact: true })); else await visible(details.getByRole('region', { name: 'Unified diff', exact: true })); assert.equal(await tabs.getByRole('tab').count(), 2); assert(await details.evaluate(el => el.scrollWidth <= el.clientWidth + 1)); await screenshot(`rp1-${String(name)}-${palette}-${theme}-${viewport.width}`); }
      }
    }
    await page.setViewportSize(receipt.viewport); await page.evaluate(() => Object.assign(document.documentElement.dataset, { colorTheme: 'minarbot', theme: 'dark' }));
    await details.getByRole('tab', { name: 'Chats', exact: true }).click(); await cleanPage();
  });
  await r5PanelSmoke({ check, page, origin, root, agentChat, settings, openNew, gitAgent, gitName, reviewer: quiet });
  await fileOutputPanelSmoke({ page, origin, root, check, gitAgent, gitName, agentChat });
  await selfVerifySmoke({ page, origin, root, check, gitAgent, gitName, gitWorkspace });
  releasePending();
  await check('Every visited page rejects CSP violations and unapproved native selects', async () => {
    await page.evaluate(() => window.__smokeGuardFlush());
    assert.deepEqual(receipt.cspViolations, [], 'CSP violations found; see receipt.json');
    assert.deepEqual(receipt.nativeSelectViolations, [], 'Unapproved native selects found; see receipt.json');
  });
  await typographySmoke({ page, check, origin, agentChat, sidebarTab, send, root });
  await check('Whole-run console and API health', async () => {
    assert(receipt.providerRequests.some(request => request.path === '/v1/models'), 'Model catalog was not requested');
    assert(receipt.providerRequests.filter(request => request.path === '/v1/chat/completions').length >= 2, 'Direct and channel provider calls were not observed');
    assert.deepEqual(
      { consoleErrors: receipt.consoleErrors.length, uncaughtErrors: receipt.pageErrors.length, apiFailures: receipt.failedRequests.length },
      { consoleErrors: 0, uncaughtErrors: 0, apiFailures: 0 },
      'Browser health failed; full messages and request details are in receipt.json',
    );
  });
} catch (error) {
  if (error.message !== 'SMOKE_STOP_AFTER') {
  receipt.checks.push({ name: 'Harness setup or interrupted run', status: 'fail', message: error.stack ?? String(error), screenshot: null, durationMs: Date.now() - started });
  console.error(error.message);
  }
} finally {
  releasePending(); releaseStream(); releaseRoom(); releaseRoomDelivery(); for (const release of hrcGates.values()) release();
  const cleanupStart = Date.now();
  for (const [name, close] of [
    ['browser', async () => { await browser?.close(); }],
    ['browser scratch descriptor', async () => { await browserScratch?.close(); }],
    ['app', async () => {
      if (!app || app.exitCode !== null || app.signalCode !== null) return;
      const exited = once(app, 'exit');
      app.kill('SIGTERM');
      const timer = setTimeout(() => app.kill('SIGKILL'), 5000);
      try { await exited; } finally { clearTimeout(timer); }
    }],
    ['provider', async () => { if (provider?.listening) { provider.closeAllConnections(); await new Promise((resolve, reject) => provider.close(error => error ? reject(error) : resolve())); } }],
    ['mcp fixture', async () => { if (mcpFixture?.listening) { mcpFixture.closeAllConnections(); await new Promise((resolve, reject) => mcpFixture.close(error => error ? reject(error) : resolve())); } }],
    ['temp data', async () => { if (data) await rm(data, { recursive: true, force: true }); }],
  ]) {
    try { await close(); } catch (error) { receipt.cleanupErrors.push({ resource: name, message: error.message }); }
  }
  receipt.timings.cleanupMs = Date.now() - cleanupStart;
  receipt.timings.totalMs = Date.now() - started;
  receipt.finishedAt = new Date().toISOString();
  receipt.interrupted = interrupted ?? null;
  receipt.appLog = join(output, 'app.log');
  receipt.status = receipt.checks.some(check => check.status === 'fail') || receipt.cleanupErrors.length || receipt.consoleErrors.length || receipt.pageErrors.length || receipt.failedRequests.length || receipt.cspViolations.length || receipt.nativeSelectViolations.length || interrupted ? 'fail' : 'pass';
  await writeFile(receipt.appLog, appLog);
  await writeFile(join(output, 'receipt.json'), `${JSON.stringify(receipt, null, 2)}\n`);
  console.log(`\n${receipt.status.toUpperCase()}: ${receipt.checks.filter(check => check.status === 'pass').length}/${receipt.checks.length} checks passed.\nOutput: ${output}\nReceipt: ${join(output, 'receipt.json')}`);
  process.exitCode = receipt.status === 'pass' ? 0 : 1;
}
