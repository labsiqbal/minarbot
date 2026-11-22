import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createInterface } from 'node:readline';
import { setTimeout as wait } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import sharp from 'sharp';
import { desktopImage, installVncFixture } from './computer-smoke.mjs';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const binary = resolve(process.env.MINARBOT_RUST_BINARY ?? join(root, 'target/debug/minarbot-server'));
const output = process.env.MINARBOT_CONVERSATION_SCREENSHOTS;
const data = await mkdtemp(join(tmpdir(), 'minarbot-leptos-chat-'));
const calls = [];
const agent = randomUUID();
const path = '/agents/' + agent;
const turns = '/api' + path + '/turns';
let releaseAnswer;
const finishAnswer = new Promise(resolve => { releaseAnswer = resolve; });
const provider = createServer(async (req, res) => {
  if (req.method !== 'POST') {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    return res.end(JSON.stringify({ data: [{ id: 'fixture' }] }));
  }
  let raw = '';
  for await (const chunk of req) raw += chunk;
  const body = JSON.parse(raw);
  const prompt = body.messages.findLast(row => row.role === 'user')?.content;
  calls.push(prompt);
  const frame = (content, finish_reason = null) => 'data: ' + JSON.stringify({ choices: [{ index: 0, delta: { content }, finish_reason }] }) + '\n\n';
  res.writeHead(200, { 'Content-Type': 'text/event-stream' });
  res.write(frame('First **café**.\n\nFinal'));
  await Promise.race([finishAnswer, once(res, 'close')]);
  res.end(frame(' answer.', 'stop') + 'data: [DONE]\n\n');
});
const child = spawn(binary, ['--test-config'], { env: { ...process.env, MINARBOT_TEST_SERVER: 'rust' }, stdio: ['pipe', 'pipe', 'inherit'] });
let browser;
try {
  await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
  const lines = createInterface({ input: child.stdout });
  const listening = new Promise((resolve, reject) => {
    lines.on('line', line => { if (line.startsWith('Minarbot: ')) resolve(line.slice(10)); });
    child.once('error', reject);
    child.once('exit', code => reject(new Error('Fixture exited ' + code)));
  });
  child.stdin.end(JSON.stringify({
    database: join(data, 'state.sqlite'), port: 0, dist: join(root, 'app/dist'), appearanceClient: 'leptos',
    baseURL: 'http://127.0.0.1:' + provider.address().port + '/v1', apiKey: 'synthetic', defaultModel: 'fixture', stream: true,
  }));
  const origin = await Promise.race([listening, wait(90_000, null, { ref: false })]);
  assert(origin, 'Fixture readiness timed out');
  const headers = { Origin: origin, 'Content-Type': 'application/json' };
  const create = await fetch(origin + '/api/agents', { method: 'POST', headers,
    body: JSON.stringify({ id: agent, runtime: 'api', name: 'Conversation fixture', model: 'fixture', instructions: 'Reply to fixture prompts.' }),
  });
  assert(create.ok, await create.text());

  browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({
    viewport: { width: 1440, height: 900 }, reducedMotion: 'reduce', permissions: ['clipboard-read', 'clipboard-write'],
  });
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  const route = origin + path + '?ui=leptos';
  const input = page.getByRole('textbox', { name: 'Message Conversation fixture' });
  const response = await page.goto(route);
  assert.equal(response.status(), 200);
  assert.match(await response.headerValue('content-security-policy'), /wasm-unsafe-eval/);
  await input.fill('Replay fixture');
  await page.reload();
  assert.equal(await input.inputValue(), 'Replay fixture');
  let original, replay;
  await page.route(url => url.origin === origin && url.pathname === turns, async requestRoute => {
    const request = requestRoute.request();
    if (request.method() === 'GET' && original !== undefined && replay === undefined) {
      // Hold history recovery until the retry reaches the network.
      return requestRoute.fulfill({ json: [] });
    }
    if (request.method() === 'POST') {
      const command = request.postData();
      if (original === undefined) {
        original = command;
        const accepted = await requestRoute.fetch();
        assert(accepted.ok(), await accepted.text());
        return requestRoute.abort('connectionreset');
      }
      replay = command;
    }
    return requestRoute.continue();
  });
  await input.press('Enter');
  await page.getByRole('button', { name: 'Retry send', exact: true }).waitFor();
  await page.reload();
  await page.getByRole('button', { name: 'Retry send', exact: true }).click();
  const partial = page.locator('[aria-label="Streaming answer"]');
  await partial.getByText('café', { exact: true }).waitFor();
  assert.equal(original, replay);
  assert.equal(calls.length, 1);
  await page.reload();
  await partial.getByText('café', { exact: true }).waitFor();
  const firstBlock = await partial.locator('.markdown').first().elementHandle();
  await firstBlock.evaluate(node => { node.dataset.retained = 'true'; });
  releaseAnswer();
  await page.getByText('Final answer.', { exact: false }).waitFor();
  assert(await firstBlock.evaluate(node => node.isConnected && node.dataset.retained === 'true'));
  await firstBlock.dispose();
  const rows = await page.request.get(origin + turns).then(response => response.json());
  assert.equal(rows.filter(row => row.id === JSON.parse(original).id).length, 1);
  assert.equal(calls.length, 1);
  console.log('Accepted response loss + reload: exact replay, 1 persisted turn, 1 provider call, stable Markdown block');

  await page.getByRole('button', { name: 'Inspect run' }).click();
  await page.getByRole('region', { name: 'Run inspector' }).waitFor();
  await page.getByRole('tab', { name: 'Chats' }).focus();
  await page.getByRole('tab', { name: 'Chats' }).press('ArrowRight');
  await page.getByRole('tabpanel', { name: 'files' }).getByRole('heading', { name: 'Conversation files' }).waitFor();
  await page.getByRole('button', { name: 'Close details' }).click();

  if (output) {
    await mkdir(output, { recursive: true });
    await page.mouse.move(0, 0);
    for (const theme of ['dark', 'light']) {
      await page.evaluate(theme => localStorage.setItem(
        'minarbot-presentation-v1', JSON.stringify({ version: 1, theme, colorTheme: 'minarbot', textSize: 'standard' }),
      ), theme);
      for (const [label, width, height] of [['desktop', 1440, 900], ['narrow', 430, 850]]) {
        await page.setViewportSize({ width, height });
        const images = [];
        for (const client of ['react', 'leptos']) {
          await page.goto(origin + path + '?ui=' + client);
          await page.getByText('Final answer.', { exact: true }).waitFor();
          await page.evaluate(() => document.fonts.ready);
          assert.equal(await page.locator('html').getAttribute('data-theme'), theme);
          assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
          const box = page.getByRole('textbox', { name: 'Message Conversation fixture' });
          assert.equal(await box.getAttribute('placeholder'), 'Message Conversation fixture');
          images.push(await page.screenshot({ path: join(output, `${label}-${theme}-${client}.png`), animations: 'disabled' }));
        }
        await sharp({ create: { width: 2 * width, height, channels: 3, background: '#000' } })
          .composite(images.map((input, i) => ({ input, left: i * width, top: 0 }))).png()
          .toFile(join(output, `${label}-${theme}-pair.png`));
        const [react, leptos] = await Promise.all(images.map(image => sharp(image).removeAlpha().raw().toBuffer()));
        const diff = Buffer.alloc(react.length);
        let pixels = 0;
        for (let i = 0; i < diff.length; i += 3) {
          const error = Math.max(...[0, 1, 2].map(channel => Math.abs(react[i + channel] - leptos[i + channel])));
          if (error > 16) pixels++;
          diff.fill(Math.min(255, error * 4), i, i + 3);
        }
        await sharp(diff, { raw: { width, height, channels: 3 } }).png()
          .toFile(join(output, `${label}-${theme}-diff.png`));
        console.log('Visual diff', label, theme, `${pixels}/${width * height} pixels differ by >16`);
      }
    }
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto(route);
    await input.waitFor();
  }

  const computer = { available: true, enabled: true, state: 'running', control: { mode: 'agent', revision: 0 } };
  let clipboardInput;
  const controlCommands = [];
  await page.route(new RegExp('/api/agents/' + agent + '/computer(?:/[^?]+)?$'), route => {
    const url = route.request().url();
    if (url.endsWith('/vnc-ticket')) return route.fulfill({ json: { ticket: 'fixture-viewer' } });
    if (url.endsWith('/control')) {
      const body = route.request().postDataJSON();
      controlCommands.push(route.request().postData());
      if (controlCommands.length === 1) {
        assert.equal(body.revision, 0);
        computer.control = { mode: 'user', revision: 1 };
        return route.abort('connectionreset');
      }
      assert.equal(controlCommands[1], controlCommands[0]);
    }
    if (url.endsWith('/input')) {
      clipboardInput = route.request().postDataJSON();
      return route.fulfill({ json: {} });
    }
    return route.fulfill({ json: computer });
  });
  await installVncFixture(page, 'data:image/png;base64,' + (await desktopImage).toString('base64'));
  await page.getByRole('button', { name: 'Show details', exact: true }).click();
  await page.getByRole('tab', { name: 'Computer', exact: true }).click();
  await page.locator('.computer-viewer canvas').waitFor();
  await page.getByRole('button', { name: 'Take control', exact: true }).click();
  await page.getByRole('dialog', { name: 'Take control?' }).getByRole('button', { name: 'Take control' }).click();
  await page.getByRole('button', { name: 'Retry saved request' }).click();
  await page.getByRole('button', { name: 'Paste clipboard', exact: true }).waitFor();
  await page.evaluate(() => navigator.clipboard.writeText('Fixture clipboard'));
  const sent = page.waitForResponse(response => response.url().endsWith('/computer/input'));
  await page.getByRole('button', { name: 'Paste clipboard', exact: true }).click();
  await sent;
  assert.deepEqual(clipboardInput, { revision: 1, name: 'computer_type', args: { text: 'Fixture clipboard' } });
  await page.waitForFunction(() => window.__vncFake.connections >= 2 && window.__vncFake.sockets[0]?._stage === 3);
  await page.locator('.computer-viewer canvas').press('Shift+Escape');
  assert(await page.getByRole('button', { name: 'Hand back to agent' }).evaluate(node => node === document.activeElement));
  await page.evaluate(() => {
    const text = new TextEncoder().encode('Copied from fixture');
    const packet = new Uint8Array(8 + text.length);
    packet[0] = 3;
    new DataView(packet.buffer).setUint32(4, text.length);
    packet.set(text, 8);
    window.__vncFake.sockets[0]._emit(packet);
  });
  await page.getByRole('button', { name: 'Copy clipboard', exact: true }).click();
  assert.equal(await page.evaluate(() => navigator.clipboard.readText()), 'Copied from fixture');
  await page.getByRole('tab', { name: 'Chats', exact: true }).click();
  await page.waitForFunction(() => window.__vncFake.sockets.length === 0);
  console.log('Computer: noVNC canvas, exact control replay, clipboard roundtrip, focus return and socket teardown passed');

  const samples = Number(process.env.MINARBOT_CONVERSATION_BENCH ?? 0);
  const statistics = {};
  const role = page.getByRole('textbox', { name: 'Message Conversation fixture' });
  for (const client of ['react', 'leptos']) {
    const timings = [];
    await page.goto(origin + path + '?ui=' + client);
    await role.waitFor();
    for (let i = 0; i < samples; i++) {
      const started = performance.now();
      await role.fill('Latency receipt ' + i);
      await page.waitForFunction((key) => sessionStorage.getItem(key)?.startsWith('Latency receipt '), 'minarbot:draft:' + agent);
      timings.push(performance.now() - started);
      await role.fill('');
    }
    timings.sort((a, b) => a - b);
    if (timings.length) statistics[client] = { samples, median: timings[Math.ceil(samples / 2) - 1], p95: timings[Math.ceil(samples * 0.95) - 1] };
  }
  assert.deepEqual(errors, []);
  if (samples) {
    console.log('Warm composer latency (ms)', JSON.stringify(statistics));
    assert(statistics.leptos.p95 <= statistics.react.p95 * 1.1, 'Warm Leptos composer p95 exceeds the 10% budget');
  }
  console.log('Conversation pilot passed');
} finally {
  await browser?.close();
  releaseAnswer();
  provider.closeAllConnections();
  if (provider.listening) await new Promise(resolve => provider.close(resolve));
  if (child.pid && child.exitCode === null && child.signalCode === null) {
    const exited = once(child, 'exit');
    child.kill();
    if (!await Promise.race([exited, wait(5000, false, { ref: false })])) {
      child.kill('SIGKILL');
      await exited;
    }
  }
  await rm(data, { recursive: true });
}
