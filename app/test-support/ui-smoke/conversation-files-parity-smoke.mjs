import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import sharp from 'sharp';
import { startRustServer } from '../../rust-test-server.ts';
import { desktopImage, preloadVncFixture } from './computer-smoke.mjs';

const output = resolve(process.argv[2] ?? '/tmp/minarbot-79-evidence');
const work = await mkdtemp(join(tmpdir(), 'minarbot-79-'));
const workspace = join(work, 'workspace');
const dist = fileURLToPath(new URL('../../dist', import.meta.url));
const agent = randomUUID(), turn = randomUUID(), file = randomUUID(), textFile = randomUUID(), failedTurn = randomUUID(), outputTurn = randomUUID();
const api = `/api/agents/${agent}`, panel = `${api}/panel`, chat = `/agents/${agent}`;
const name = 'File and Computer fixture';
const wait = { turn, call_id: 'fixture-call', reason: '', state: 'waiting',
  secret_entry: 'Example sign-in', secret_origin: 'https://example.test' };
const html = '<h1>Fixture artifact</h1><script>throw new Error("Preview must be inert");</script>';
const shots = new Map(), report = { screenshots: {}, timing: {} };
const samples = Number(process.env.MINARBOT_FILES_BENCH ?? 20);
assert(Number.isSafeInteger(samples) && samples >= 0);
let app, browser;

try {
  await mkdir(output, { recursive: true });
  await mkdir(join(workspace, 'folder'), { recursive: true });
  const git = (...args) => execFileSync('git', ['-c', 'core.hooksPath=/dev/null', ...args], { cwd: workspace, env: { ...process.env, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: '/dev/null', GIT_TERMINAL_PROMPT: '0' } });
  git('init', '-q', '-b', 'main');
  await writeFile(join(workspace, 'folder', 'notes.txt'), 'Old line\nContext\n');
  git('add', '--', 'folder/notes.txt');
  git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.test', 'commit', '-q', '-m', 'Fixture baseline');
  await writeFile(join(workspace, 'folder', 'notes.txt'), 'New line\nContext\n');
  app = await startRustServer({
    database: join(work, 'minarbot.sqlite'), dist, appearanceClient: 'leptos',
    baseURL: 'http://127.0.0.1:1/v1', apiKey: 'fixture-only', defaultModel: 'fixture',
    workspaces: [{ agent, directory: workspace, write: true }],
  });
  async function post(path, body, headers = {}) {
    const response = await fetch(app.origin + path, { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json', ...headers }, body });
    assert(response.ok, `${path} HTTP ${response.status}`);
    return response.json();
  }
  await post('/api/agents', JSON.stringify({ id: agent, name, instructions: 'Fixture.', model: 'fixture', runtime: 'api' }));
  await post(`${api}/files/${file}`, html, { 'Content-Type': 'application/octet-stream', 'X-File-Name': 'report.html' });
  await post(`${api}/files/${textFile}`, 'Text fixture\n', { 'Content-Type': 'application/octet-stream', 'X-File-Name': 'notes.txt' });
  const site = await post(`${api}/sites`, JSON.stringify({ id: randomUUID(), name: 'Local report', source: '<h1>Fixture site</h1>' }));
  const resultUrl = app.origin + '/sites/' + site.id;
  const screen = 'data:image/png;base64,' + (await desktopImage).toString('base64');
  browser = await chromium.launch({ headless: true });
  const route = (client, target) => {
    const location = new URL(chat, app.origin);
    location.searchParams.set('ui', client);
    if (target) location.searchParams.set('result', JSON.stringify(target));
    return location.href;
  };
  const fileResult = { view: 'file', path: `${panel}/files/${file}`, name: 'report.html' };
  const urlResult = { view: 'url', path: resultUrl, name: 'Fixture site' };

  async function setup(client, viewport, theme, fixtureComputer = true) {
    const context = await browser.newContext({ viewport, colorScheme: theme, reducedMotion: 'reduce' });
    const errors = [];
    const requests = [];
    await context.addInitScript(theme => {
      if (window.top !== window) return;
      localStorage.setItem('minarbot-presentation-v1', JSON.stringify({ version: 1, theme, colorTheme: 'minarbot', textSize: 'standard' }));
      localStorage.setItem('minarbot:details-open', '0');
      document.addEventListener('securitypolicyviolation', event => window.__policyErrors.push(event.violatedDirective));
      window.__policyErrors = [];
    }, theme);
    if (fixtureComputer) await context.route(url => url.pathname.startsWith(`${api}/computer`) || url.pathname === '/api/vault/respond', async request => {
      const pathname = new URL(request.request().url()).pathname;
      if (request.request().method() === 'POST') {
        const body = request.request().postDataJSON();
        if (pathname === '/api/vault/respond') {
          // Record the contract without retaining or printing the password.
          assert.deepEqual(Object.keys(body).sort(), ['agent', 'call', 'turn', 'value']);
          assert(body.agent === agent && body.turn === turn && body.call === wait.call_id && body.value === 'synthetic-entry');
          requests.push({ pathname });
          return request.fulfill({ json: { ok: true } });
        }
        requests.push({ pathname, body });
      }
      if (pathname.endsWith('/vnc-ticket')) return request.fulfill({ json: { ticket: 'fixture-viewer' } });
      if (pathname.endsWith('/open-url')) return request.fulfill({ json: { status: 'done' } });
      return request.fulfill({ json: { available: true, enabled: true, state: 'running', revision: 2,
        control: { mode: 'agent', revision: 1 }, requests: [wait], stopsInMs: null } });
    });
    await context.route(url => url.pathname === `${api}/turns` || url.pathname === `/api/turns/${failedTurn}/inspect`, request => {
      if (request.request().method() !== 'GET') return request.continue();
      if (request.request().url().endsWith('/inspect')) return request.fulfill({ json: {
        status: 'failed', error: 'Fixture run failed.', tools: [], harness: [], engineStderr: null,
        providers: [{ seq: 1, model: 'fixture', status: 'failed', created: '2026-11-22T09:00:00Z', finished: '2026-11-22T09:00:00.100Z',
          duration_ms: 100, input_tokens: null, output_tokens: null, total_tokens: null }],
      } });
      const common = { agent, created: '2026-11-22T09:00:00Z', paused: 0, no_update: 0, revision: 1, parent: null, files: [] };
      return request.fulfill({ json: [
        { ...common, id: failedTurn, seq: 1, status: 'failed', text: 'Inspect fixture', reply: null, error: 'Fixture run failed.' },
        { ...common, id: outputTurn, seq: 2, status: 'done', text: 'Report fixture', reply: 'report.html is ready.', error: null,
          generatedFiles: [{ id: file, name: 'report.html', size: html.length, image: false }] },
      ] });
    });
    const page = await context.newPage();
    await preloadVncFixture(page, screen);
    page.on('pageerror', error => errors.push(error.message));
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
    page.setDefaultTimeout(10_000);
    return { client, context, page, errors, requests };
  }

  async function capture(page, client, key, surface) {
    await surface.waitFor({ state: 'visible' });
    await page.evaluate(() => document.fonts.ready);
    await page.mouse.move(1, 1);
    const png = await page.screenshot({ path: join(output, `${key}-${client}.png`), animations: 'disabled' });
    const pair = shots.get(key) ?? {};
    pair[client] = png;
    shots.set(key, pair);
  }

  for (const viewport of [{ width: 1440, height: 900 }, { width: 430, height: 850 }]) {
    for (const theme of ['dark', 'light']) {
      for (const client of ['react', 'leptos']) {
        const { context, page, errors, requests } = await setup(client, viewport, theme);
        const key = surface => `${viewport.width}-${theme}-${surface}`;
        const details = page.getByRole('complementary', { name: 'Conversation details', exact: true });
        const composer = page.getByRole('textbox', { name: `Message ${name}`, exact: true });
        const tab = tab => details.getByRole('tab', { name: tab, exact: true });
        const close = details.getByRole('button', { name: 'Close details', exact: true });
        const reveal = async () => {
          const show = page.getByRole('button', { name: 'Show details', exact: true });
          if (await show.isVisible()) await show.click();
        };
        try {
          let response = await page.goto(route(client));
          assert.equal(response.status(), 200);
          const policy = await response.headerValue('content-security-policy');
          if (client === 'leptos') assert.match(policy, /wasm-unsafe-eval/);
          else assert.doesNotMatch(policy, /wasm-unsafe-eval/);
          await composer.waitFor();
          const inspect = page.getByRole('button', { name: /^(Inspect|Inspect run)$/ }).filter({ visible: true }).first();
          await inspect.click();
          await capture(page, client, key('run-inspector'), details.getByText('Run error', { exact: true }));
          await details.getByText('fixture', { exact: true }).click();
          const started = details.locator('time[datetime="2026-11-22T09:00:00Z"]');
          assert.equal(await started.innerText(), await started.evaluate(time =>
            new Intl.DateTimeFormat(undefined, { dateStyle: 'short', timeStyle: 'medium' }).format(new Date(time.dateTime))));
          await close.click();
          await reveal();
          await tab('Files').press('Enter');
          const fileRow = details.getByRole('button', { name: 'Preview report.html', exact: true });
          await capture(page, client, key('files'), fileRow);
          const menu = details.getByRole('button', { name: 'File actions: notes.txt', exact: true });
          await menu.click();
          await page.getByRole('menuitem', { name: 'Attach to message', exact: true }).click();
          await page.locator('button[aria-label="Remove notes.txt"]').waitFor({ state: 'attached' });
          await menu.click();
          await page.getByRole('menuitem', { name: 'Remove from message', exact: true }).click();
          await fileRow.click();
          const heading = details.frameLocator('iframe').getByRole('heading', { name: 'Fixture artifact', exact: true });
          await heading.waitFor();
          await capture(page, client, key('file-preview'), details.locator('iframe'));
          const iframe = details.locator('iframe');
          assert.equal(await iframe.getAttribute('sandbox'), '');
          assert.equal(await iframe.getAttribute('referrerpolicy'), 'no-referrer');
          assert.equal(await details.frameLocator('iframe').locator('script').count(), 0);
          assert(new URL(page.url()).searchParams.has('result'));
          await page.reload();
          await heading.waitFor();
          await details.getByRole('button', { name: 'Close file', exact: true }).click();
          await reveal();
          await tab('Files').click();

          await details.getByRole('button', { name: 'Preview artifacts', exact: true }).click();
          await heading.waitFor();
          await capture(page, client, key('artifacts'), iframe);
          await details.getByRole('button', { name: 'Back to files', exact: true }).click();
          await details.getByRole('button', { name: /^Source control/ }).click();
          const diff = details.getByRole('region', { name: 'Unified diff', exact: true });
          await diff.getByText('+New line', { exact: true }).waitFor();
          await capture(page, client, key('changes'), diff);
          const layout = details.getByRole('combobox', { name: 'Diff layout' });
          if (await layout.evaluate(element => element instanceof HTMLSelectElement)) await layout.selectOption('split');
          else { await layout.click(); await page.getByRole('option', { name: 'Split', exact: true }).click(); }
          const split = details.getByRole('region', { name: 'Split diff', exact: true });
          await split.focus();
          await capture(page, client, key('split-diff'), split);
          await details.getByRole('button', { name: 'Wrap lines', exact: true }).click();
          await details.getByRole('button', { name: 'Back to files', exact: true }).click();
          const files = details.getByRole('navigation', { name: 'Workspace path' });
          await files.getByRole('button', { name: 'Workspace', exact: true }).waitFor();
          await details.getByRole('button', { name: 'folder', exact: true }).click();
          await details.getByRole('button', { name: 'notes.txt', exact: true }).click();
          const text = details.getByRole('region', { name: 'Contents of notes.txt' });
          await text.getByText('New line', { exact: false }).waitFor();
          await capture(page, client, key('workspace-file'), text);
          await close.click();
          await page.getByLabel('Agent reply', { exact: true }).getByRole('link', { name: 'report.html', exact: true }).click();
          await heading.waitFor();
          await close.click();
          await page.locator('.chat__pill').click();
          await page.getByText('Workspace files', { exact: true }).click();
          await page.getByRole('button', { name: /folder.*Directory/ }).waitFor();
          await capture(page, client, key('workspace-browser'), page.getByRole('heading', { name: `Workspace files for ${name}` }));
          await page.getByRole('button', { name: /folder.*Directory/ }).click();
          await page.getByRole('button', { name: /notes\.txt.*File/ }).click();
          await page.getByRole('region', { name: 'Contents of folder/notes.txt' }).waitFor();
          await capture(page, client, key('workspace-browser-file'), page.getByRole('region', { name: 'Contents of folder/notes.txt' }));
          await page.getByRole('button', { name: 'Back', exact: true }).click();

          await page.goto(route(client, urlResult));
          const siteFrame = details.frameLocator('iframe[title="URL preview"]');
          await siteFrame.getByRole('heading', { name: 'Fixture site', exact: true }).waitFor();
          await capture(page, client, key('url-preview'), details.locator('iframe[title="URL preview"]'));
          await details.getByRole('button', { name: 'Open in Computer', exact: true }).click();
          await page.locator('.computer-viewer canvas').waitFor();
          await page.waitForFunction(() => window.__vncFake?.frames > 0);
          assert.deepEqual(requests.find(request => request.pathname.endsWith('/open-url'))?.body, { url: resultUrl });
          await capture(page, client, key('computer-result'), page.locator('.computer-viewer canvas'));
          await page.getByRole('button', { name: 'Enter value', exact: true }).click();
          const secure = page.getByRole('dialog', { name: 'Enter a secure value' });
          await capture(page, client, key('secure-request'), secure);
          const value = secure.getByLabel(wait.secret_entry, { exact: true });
          assert.equal(await value.getAttribute('type'), 'password');
          await value.fill('synthetic-entry');
          await secure.getByRole('button', { name: 'Save and fill' }).click();
          await secure.waitFor({ state: 'hidden' });
          assert.equal(requests.filter(request => request.pathname === '/api/vault/respond').length, 1);
          assert(await page.evaluate(() => !Object.values(sessionStorage).some(value => value.includes('synthetic-entry'))));
          await page.goBack();
          await siteFrame.getByRole('heading', { name: 'Fixture site', exact: true }).waitFor();

          response = await page.goto(`${app.origin}/?computer=${agent}&ui=${client}`);
          assert.equal(response.status(), 200);
          if (client === 'leptos') assert.match(await response.headerValue('content-security-policy'), /wasm-unsafe-eval/);
          else assert.doesNotMatch(await response.headerValue('content-security-policy'), /wasm-unsafe-eval/);
          await page.waitForFunction(() => window.__vncFake?.frames > 0);
          await capture(page, client, key('computer-window'), page.locator('.computer-window canvas'));
          assert.equal(await page.getByRole('button', { name: 'Pop out', exact: true }).count(), 0);
          assert.deepEqual(await page.evaluate(() => window.__policyErrors), []);
          assert.deepEqual(errors, []);
          console.log(client, key('workspace, files, diff, URL, Computer and secure fill passed'));
        } finally { await context.close(); }
      }
    }
  }

  for (const [name, images] of shots) {
    if (!images.react || !images.leptos) continue;
    const { width, height } = await sharp(images.react).metadata();
    const [left, right] = await Promise.all([images.react, images.leptos].map(image => sharp(image).removeAlpha().raw().toBuffer()));
    let different = 0;
    for (let n = 0; n < left.length; n += 3) {
      if (Math.max(...[0, 1, 2].map(c => Math.abs(left[n + c] - right[n + c]))) > 16) different++;
    }
    const path = join(output, `${name}-pair.png`);
    await sharp({ create: { width: width * 2, height, channels: 3, background: '#000' } })
      .composite([{ input: images.react, left: 0, top: 0 }, { input: images.leptos, left: width, top: 0 }]).png().toFile(path);
    report.screenshots[name] = { path, different, pixels: width * height };
  }

  const summary = data => {
    const sorted = [...data].sort((a, b) => a - b);
    return { n: sorted.length, median: sorted[Math.floor(sorted.length / 2)], p95: sorted[Math.ceil(sorted.length * .95) - 1] };
  };
  const cold = { react: [], leptos: [] }, warm = { react: [], leptos: [] };
  for (let sample = 0; sample < samples; sample++) {
    for (const client of sample % 2 ? ['leptos', 'react'] : ['react', 'leptos']) {
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, reducedMotion: 'reduce' });
      const page = await context.newPage();
      try {
        await page.addInitScript(() => {
          document.addEventListener('load', event => {
            if (event.target.matches?.('iframe[title="Visual preview report.html"]')) performance.mark('file-ready');
          }, true);
        });
        await page.goto(route(client, fileResult), { waitUntil: 'commit' });
        await page.waitForFunction(() => performance.getEntriesByName('file-ready').length > 0, undefined, { polling: 20 });
        cold[client].push(await page.evaluate(() => performance.getEntriesByName('file-ready')[0].startTime));
        await page.getByRole('button', { name: 'Close file', exact: true }).click();
        const show = page.getByRole('button', { name: 'Show details', exact: true });
        if (await show.isVisible()) await show.click();
        await page.getByRole('tab', { name: 'Files', exact: true }).click();
        const trigger = page.getByRole('button', { name: 'Preview report.html', exact: true });
        await trigger.evaluate(button => button.addEventListener('click', () => {
          performance.clearMarks('file-ready');
          performance.mark('open-file');
        }, { once: true }));
        await trigger.click();
        await page.waitForFunction(() => performance.getEntriesByName('file-ready').length > 0, undefined, { polling: 20, timeout: 5000 });
        warm[client].push(await page.evaluate(() => performance.getEntriesByName('file-ready')[0].startTime - performance.getEntriesByName('open-file')[0].startTime));
      } finally { await context.close(); }
    }
  }
  if (samples) report.timing = { cold: { react: summary(cold.react), leptos: summary(cold.leptos) },
    warm: { react: summary(warm.react), leptos: summary(warm.leptos) } };
  await writeFile(join(output, 'report.json'), JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify(report.timing));
  console.log(`Compared ${shots.size} screenshot pairs in ${output}`);
  if (samples) {
    assert(report.timing.cold.leptos.p95 <= report.timing.cold.react.p95 * 1.2, 'Cold file preview p95 exceeds React +20%');
    assert(report.timing.warm.leptos.p95 <= report.timing.warm.react.p95 * 1.1, 'Warm file preview p95 exceeds React +10%');
  }
} finally {
  await browser?.close();
  await app?.close();
  await rm(work, { recursive: true });
}
