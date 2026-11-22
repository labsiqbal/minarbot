import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { createServer } from 'node:http';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as wait } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import sharp from 'sharp';
import { startRustServer } from '../../rust-test-server.ts';

const output = resolve(process.argv[2] ?? '/tmp/minarbot-75-screenshots');
const work = await mkdtemp(join(tmpdir(), 'minarbot-conversation-controls-'));
const dist = fileURLToPath(new URL('../../dist', import.meta.url));
const provider = createServer(async (request, response) => {
  for await (const chunk of request) void chunk;
  response.setHeader('Content-Type', 'application/json');
  response.end(JSON.stringify(request.method === 'GET' ? { data: [{ id: 'fixture' }] } : {
    choices: [{ finish_reason: 'stop', message: { content: 'Fixture answer.' } }],
  }));
});
let browser, app;
const evidence = [], faults = [], navigation = {}, cold = {};
const stats = times => {
  const n = times.length;
  times.sort((a, b) => a - b);
  return { median: (times[Math.floor((n - 1) / 2)] + times[Math.floor(n / 2)]) / 2, p95: times[Math.ceil(n * .95) - 1], samples: n };
};
try {
  await mkdir(output, { recursive: true });
  await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
  app = await startRustServer({
    database: join(work, 'state.sqlite'), dist, appearanceClient: 'leptos', stream: false,
    baseURL: 'http://127.0.0.1:' + provider.address().port + '/v1', apiKey: 'synthetic', defaultModel: 'fixture',
  });
  const api = async (path, data) => {
    const response = await fetch(app.origin + path, data === undefined ? undefined : {
      method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(data),
    });
    assert(response.ok, `${path} returned ${response.status}`);
    return response.json();
  };
  const doc = async (path, expected) => {
    const response = await fetch(app.origin + path);
    assert(response.ok, path);
    const html = await response.text();
    assert.equal(/data-view="conversation"/.test(html), expected === 'conversation', path);
    if (expected === 'conversation') assert.match(response.headers.get('content-security-policy'), /wasm-unsafe-eval/);
    if (expected === 'setup') assert.match(html, /Set up Minarbot/);
  };
  await doc('/', 'setup');
  async function addAgent(name) {
    return api('/api/agents', { id: randomUUID(), name, runtime: 'api', model: 'fixture', instructions: 'Synthetic conversation fixture.' });
  }
  const agent = await addAgent('Chat fixture');
  const helper = await addAgent('Room helper');
  const main = '/agents/' + agent.id;
  const mainApi = '/api' + main;
  const thread = await api(mainApi + '/threads', { id: randomUUID(), name: 'Named chat' });
  const named = main + '/conversations/' + thread.id;
  const project = await api('/api/projects', { id: randomUUID(), name: 'Room fixture', members: [agent, helper].map(item => ({ agent: item.id, write: false })) });
  const room = '/rooms/' + project.sections[0].id;
  const projectThread = await api(`/api/projects/${project.id}/thread`, {
    id: randomUUID(), revision: project.revision, agent: agent.id, name: 'Project chat', folder: '',
  });
  const projectChat = main + '/conversations/' + projectThread.threads.find(thread => thread.name === 'Project chat').id;
  const realTurn = randomUUID();
  await api(mainApi + '/turns', { id: realTurn, text: 'Fork source message' });
  let complete = false;
  for (let i = 0; i < 100 && !complete; i++) {
    complete = (await api(mainApi + '/turns')).some(turn => turn.id === realTurn && turn.status === 'done');
    if (!complete) await wait(100);
  }
  assert(complete, 'Fork source finished');
  const fork = await api(mainApi + '/turns/' + realTurn + '/fork', { id: randomUUID() });
  const forkRoute = main + '/conversations/' + fork.id;
  for (const path of ['/', main, named, projectChat, room]) {
    await doc(path, 'conversation');
    await doc(path + '?ui=react', 'react');
  }
  await doc(main + '/inbox', 'react');
  await doc(main + '?result=fixture', 'react');
  if (process.env.MINARBOT_CONVERSATION_ROUTES_ONLY === '1') { console.log('PASS fresh setup, ordinary routes and React rollback'); process.exitCode = 0; }

  if (process.env.MINARBOT_CONVERSATION_ROUTES_ONLY !== '1') {
    const time = '2026-11-22T08:05:00.000Z';
    const date = Date.parse(time);
    const mk = (seq, text, reply = 'Fixture answer.') => ({
      id: randomUUID(), seq, created: time, revision: 1, agent: agent.id, agent_name: agent.name,
      text, reply, paused: 0, no_update: 0, status: 'done', error: null,
    });
    const historical = Array.from({ length: 6 }, (_, n) => mk(n + 1, n === 0 ? 'Archived needle' : 'Older message ' + (n + 1),
      'Earlier answer. ' + 'History fixture text. '.repeat(16)));
    const recent = Array.from({ length: 100 }, (_, n) => mk(n + 7, 'Message ' + (n + 1),
      '**Answer ' + (n + 1) + '**. ' + 'Timeline fixture text. '.repeat(12)));
    const live = { ...mk(107, 'Running task', 'Current partial.'), reply: null, status: 'running',
      partial: { seq: 1, text: 'Current partial.', status: 'pending', model: 'fixture' },
    };
    const queue = { ...mk(108, 'Queued task'), reply: null, status: 'queued', dispatch_mode: 'queue' };
    const eventId = randomUUID();
    const waiting = { ...mk(109, 'Wait for signed event'), reply: null, status: 'queued',
      events: [{ id: eventId, event: 'fixture.ready', deadline: date + 100000, status: 'waiting' }],
    };
    const checkpoint = { ...mk(110, 'Workspace change'), checkpoint: { restorable: true } };
    const child = { ...mk(111, 'Child task'), parent: checkpoint.id };
    checkpoint.coordination = [{ kind: 'delegate_task', child: child.id, prompt: child.text, owner_name: agent.name, target_name: helper.name }];
    const digest = '1'.repeat(64);
    const projectScope = '/api/channels/' + project.sections[0].id;
    const roomRoot = { ...mk(201, 'Room task'), reply: null, status: 'running', history_page: 1 };
    const roomChild = { ...mk(202, 'Another member'), agent: helper.id, agent_name: helper.name, parent: roomRoot.id, reply: null, status: 'queued', paused: 1, history_page: 1 };
    roomRoot.room = { root: roomRoot.id, generation: roomRoot.id, active: true, round: 0, stopped: false, silent: null, note: null, quiet: [], messages: [] };
    roomChild.room = { ...roomRoot.room, round: 1 };
    browser = await chromium.launch({ headless: true });
    const selectClient = (path, client) => app.origin + path + (client === 'react' ? '?ui=react' : '');
    for (const [size, width, height] of [['desktop', 1440, 900], ['narrow', 430, 850]]) {
      for (const theme of ['dark', 'light']) {
        const images = new Map();
        for (const client of ['react', 'leptos']) {
          const context = await browser.newContext({ viewport: { width, height }, reducedMotion: 'reduce', colorScheme: theme });
          try {
            await context.addInitScript(theme => {
              localStorage.setItem('minarbot-presentation-v1', JSON.stringify({ version: 1, theme, textSize: 'standard', colorTheme: 'minarbot' }));
              localStorage.setItem('minarbot:details-open', '0');
              localStorage.setItem('minarbot.speech.enabled.v1', 'true');
              localStorage.setItem('minarbot.speech.voice.v1', JSON.stringify(['local-fixture', 'Fixture voice', 'en-US']));
              const engine = new EventTarget();
              engine.getVoices = () => [{ voiceURI: 'local-fixture', name: 'Fixture voice', lang: 'en-US', localService: true }];
              Object.defineProperty(window, 'speechSynthesis', { value: engine, configurable: true });
              window.SpeechSynthesisUtterance = class { constructor(text) { this.text = text; } };
              engine.speak = utterance => { window.fixtureSpoken = utterance.text; };
              engine.cancel = () => { window.fixtureSpoken = ''; };
              window.fixtureCsp = [];
              document.addEventListener('securitypolicyviolation', event => window.fixtureCsp.push(event.effectiveDirective));
            }, theme);
            const page = await context.newPage();
            page.setDefaultTimeout(15000);
            page.on('pageerror', error => faults.push({ client, error: error.message }));
            page.on('console', message => { if (message.type() === 'error') faults.push({ client, error: message.text() }); });
            await page.clock.setFixedTime(new Date(date + 10_000));
            const actions = [];
            let rows = recent;
            let roomRows = [roomRoot, roomChild];
            await page.route(url => url.origin === app.origin && (
              url.pathname === mainApi + '/turns' || url.pathname === mainApi + '/dispatch' || url.pathname === mainApi + '/queue' ||
              url.pathname.startsWith(mainApi + '/git/checkpoints/') || url.pathname.startsWith(mainApi + '/turns/' + queue.id + '/') ||
              url.pathname === `/api/turn-waits/${eventId}` || url.pathname === projectScope + '/turns' ||
              url.pathname === projectScope + '/dispatch' || url.pathname === projectScope + '/discussions/' + roomRoot.id + '/stop'
            ), route => {
              const url = new URL(route.request().url());
              const path = url.pathname;
              if (route.request().method() === 'POST') {
                actions.push({ path, body: route.request().postDataJSON() });
                if (path.endsWith('/stop')) roomRows = roomRows.map(turn => ({ ...turn, room: { ...turn.room, active: false }, status: 'cancelled', paused: 0 }));
                if (path.endsWith('/restore')) return route.fulfill({ json: { restored: true } });
                return route.fulfill({ json: {} });
              }
              if (path === mainApi + '/turns') {
                if (url.searchParams.has('before')) return route.fulfill({ json: historical });
                if (url.searchParams.has('focus')) {
                  const id = url.searchParams.get('focus');
                  return id === realTurn ? route.continue() : route.fulfill({ json: rows.filter(turn => turn.id === id) });
                }
                return route.fulfill({ json: rows });
              }
              if (path === mainApi + '/dispatch') return route.fulfill({ json: rows.includes(live) ? [
                { id: live.id, agent: agent.id, capabilities: { steer: true, queue: true, restart: true } },
              ] : [] });
              if (path === mainApi + '/queue') return route.fulfill({ json: {
                turns: [queue].map(turn => ({ ...turn, editing: false, previous: null, next: null })), next_after: null, more: false, snapshot: digest,
              } });
              if (path.startsWith(mainApi + '/git/checkpoints/')) return route.fulfill({ json: {
                turn: checkpoint.id, digest, files: [{ path: 'fixture.txt', action: 'restore' }],
              } });
              if (path.startsWith('/api/turn-waits/')) return route.fulfill({ json: {
                event: 'fixture.ready', deadline: waiting.events[0].deadline,
                url: app.origin + path + '/events', secret: 'synthetic-fixture',
              } });
              if (path === projectScope + '/dispatch') return route.fulfill({ json: roomRows.some(turn => turn.status === 'running') ? [
                { id: roomRoot.id, agent: agent.id, capabilities: { steer: true, queue: true, restart: true } },
              ] : [] });
              if (path === projectScope + '/turns') return route.fulfill({ json: roomRows });
              return route.continue();
            });
            const capture = async (name, mask = []) => {
              assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), name + ' overflows');
              assert.deepEqual(await page.evaluate(() => window.fixtureCsp), [], name);
              await page.evaluate(async () => { await document.fonts.ready; await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))); });
              const file = join(output, `${size}-${theme}-${name}-${client}.png`);
              const image = await page.screenshot({ path: file, animations: 'disabled', mask });
              if (client === 'react') {
                images.set(name, image);
                return;
              }
              const react = images.get(name);
              assert(react, name);
              const pair = join(output, `${size}-${theme}-${name}-pair.png`);
              await sharp({ create: { width: 2 * width, height, channels: 3, background: '#000' } })
                .composite([{ input: react, left: 0, top: 0 }, { input: image, left: width, top: 0 }]).png().toFile(pair);
              const [left, right] = await Promise.all([react, image].map(input => sharp(input).removeAlpha().raw().toBuffer()));
              let different = 0;
              for (let i = 0; i < left.length; i += 3) {
                if (Math.max(Math.abs(left[i] - right[i]), Math.abs(left[i + 1] - right[i + 1]), Math.abs(left[i + 2] - right[i + 2])) > 16) different++;
              }
              evidence.push({ size, theme, name, pair, different, pixels: width * height });
              console.log(`${size} ${theme} ${name} ${different}/${width * height} pixels above channel delta 16`);
            };
            const open = async path => {
              await page.goto(selectClient(path, client));
              await page.getByRole('region', { name: 'Conversation history', exact: true }).waitFor();
            };
            const message = (id) => page.locator('#turn-' + id);
            await open(main);
            await message(recent.at(-1).id).waitFor();
            if (size === 'desktop') {
              const rail = page.locator('.timeline-minimap__hit');
              await rail.focus();
              await rail.press('Home');
              await rail.press('Enter');
              await page.locator('.timeline-minimap__preview').waitFor();
              await capture('timeline-minimap');
              await rail.press('Escape');
            }
            await page.getByRole('button', { name: 'Older messages', exact: true }).click();
            await message(historical[0].id).waitFor();
            await capture('older-history');
            await page.getByRole('button', { name: 'Newer messages', exact: true }).click();
            await message(recent.at(-1).id).waitFor();
            await page.keyboard.press('Control+f');
            const find = page.getByRole('search', { name: 'Find in conversation' });
            await find.getByRole('searchbox').fill('Archived needle');
            await find.getByText('1 of 1', { exact: true }).waitFor();
            await capture('find');
            await page.keyboard.press('Escape');
            assert.equal(await page.evaluate(() => CSS.highlights.has('message-find')), false);

            await open(forkRoute);
            await page.getByRole('region', { name: 'Inherited messages' }).getByText('Fork source message').waitFor();
            await capture('fork-history');
            await page.getByLabel('Fork origin').getByRole('link', { name: 'Main chat', exact: true }).click();
            await message(realTurn).waitFor();
            await capture('focused-message');

            rows = [live, queue, waiting, checkpoint, child];
            await open(main);
            const reply = message(child.id);
            await reply.getByRole('button', { name: 'Listen', exact: true }).focus();
            await reply.getByRole('button', { name: 'Listen', exact: true }).press('Enter');
            assert.equal(await page.evaluate(() => window.fixtureSpoken), child.reply);
            await capture('speech-playback');
            await reply.getByRole('button', { name: 'Stop', exact: true }).press('Enter');
            assert.equal(await page.evaluate(() => window.fixtureSpoken), '');

            await message(checkpoint.id).getByRole('button', { name: 'Restore to before this turn' }).focus();
            await message(checkpoint.id).getByRole('button', { name: 'Restore to before this turn' }).press('Enter');
            const restore = page.getByRole('dialog', { name: 'Restore to before this turn?' });
            await restore.getByRole('list', { name: 'Files to restore' }).getByText('fixture.txt').waitFor();
            await capture('checkpoint-restore');
            await restore.getByRole('button', { name: 'Restore files', exact: true }).click();
            await restore.waitFor({ state: 'hidden' });
            const restoration = actions.find(item => item.path.endsWith('/restore'));
            assert.equal(restoration.body.checkpointId, checkpoint.id);
            assert.equal(restoration.body.digest, digest);

            await message(waiting.id).getByRole('button', { name: 'Show event connection' }).click();
            const secret = message(waiting.id).getByLabel('Signing secret');
            await secret.waitFor();
            await secret.evaluate(input => input.scrollIntoView({ block: 'center' }));
            await capture('signed-event', [secret]);
            await message(waiting.id).getByRole('button', { name: 'Hide connection' }).click();
            assert.equal(await secret.count(), 0);

            const queued = page.getByRole('region', { name: 'Queued messages' });
            await queued.getByRole('button', { name: 'Order', exact: true }).click();
            const order = page.getByRole('dialog', { name: 'Queue order', exact: true });
            const expand = order.getByRole('button', { name: /^Queue [0-9]/ });
            if (await expand.count()) await expand.click();
            await order.getByRole('button', { name: 'Open message' }).waitFor();
            await capture('queue-order');
            await page.keyboard.press('Escape');
            await queued.getByRole('button', { name: 'Send now', exact: true }).click();
            const dispatch = actions.find(item => item.path.endsWith('/dispatch'));
            assert.equal(dispatch.body.targetTurn, live.id);
            assert.equal(dispatch.body.mode, 'restart');
            assert.equal(dispatch.body.revision, queue.revision);

            await open(room);
            const recipient = page.getByRole('combobox', { name: 'Addressed member' });
            await recipient.waitFor();
            await recipient.press('Enter');
            await page.getByRole('option', { name: agent.name, exact: true }).click();
            await capture('room');
            const resumeTarget = page.getByRole('region', { name: 'Reply from ' + helper.name });
            await resumeTarget.getByRole('button', { name: 'Resume', exact: true }).focus();
            await resumeTarget.getByRole('button', { name: 'Resume', exact: true }).press('Enter');
            const resume = page.getByRole('dialog', { name: 'Resume or reassign' });
            assert(await resume.getByRole('button', { name: 'Resume task' }).isDisabled());
            await resume.getByRole('checkbox', { name: 'I have checked previous results and effects' }).check();
            await capture('reassign');
            await page.keyboard.press('Escape');
            const stopping = page.waitForRequest(request => request.method() === 'POST' && request.url().endsWith('/stop'));
            await page.locator('.composer-area').getByRole('button', { name: 'Stop', exact: true }).click();
            await stopping;
            assert.equal(actions.find(item => item.path.endsWith('/stop')).body.generation, roomRoot.id);
            await open(projectChat);
            await page.getByRole('main', { name: 'Conversation Project chat', exact: true }).waitFor();
            await capture('project-chat');
            await open(named);
            await page.getByRole('main', { name: 'Conversation Named chat', exact: true }).waitFor();
            await capture('named-chat');

            if (client === 'leptos') {
              await page.locator('.chat__pill').click();
              await page.getByRole('button', { name: 'Search chat history', exact: true }).click();
              const all = page.getByRole('dialog', { name: 'Search chat history', exact: true });
              await all.getByRole('searchbox').fill('Fork source message');
              await all.getByRole('searchbox').press('Enter');
              await all.getByRole('link', { name: 'Fork source message', exact: true }).click();
              await message(realTurn).waitFor();
              await page.goBack();
              await page.getByRole('main', { name: 'Conversation Named chat', exact: true }).waitFor();
            }
          } finally { await context.close(); }
        }
        assert.equal(evidence.filter(item => item.size === size && item.theme === theme).length, images.size);
      }
    }
    const samples = Number(process.env.MINARBOT_CONVERSATION_NAV_BENCH ?? 20);
    const clients = ['react', 'leptos'];
    if (samples) {
      const options = { viewport: { width: 1440, height: 900 }, reducedMotion: 'reduce' };
      const coldTimes = { react: [], leptos: [] };
      for (let n = 0; n < samples; n++) {
        for (const client of n % 2 === 0 ? clients : [...clients].reverse()) {
          const page = await browser.newPage(options);
          try {
            const start = performance.now();
            await page.goto(selectClient(main, client), { waitUntil: 'commit' });
            await page.waitForFunction(id => document.querySelector('#message:enabled') && document.getElementById('turn-' + id), realTurn);
            coldTimes[client].push(performance.now() - start);
          } finally { await page.close(); }
        }
      }
      const pages = {};
      const warmTimes = { react: [], leptos: [] };
      for (const client of clients) {
        cold[client] = stats(coldTimes[client]);
        const page = pages[client] = await browser.newPage(options);
        await page.addInitScript(() => localStorage.setItem('minarbot:details-open', '1'));
        await page.goto(selectClient(main, client));
        await page.getByRole('tab', { name: 'Chats', exact: true }).waitFor();
      }
      try {
        for (let n = -2; n < samples; n++) {
          for (const client of n % 2 === 0 ? clients : [...clients].reverse()) {
            const page = pages[client];
            const label = n % 2 === 0 ? 'Named chat' : 'Main chat';
            const target = page.getByRole('complementary', { name: 'Conversation details' }).locator('.panel-row').filter({ hasText: label });
            const title = n % 2 === 0 ? 'Named chat' : agent.name;
            await target.evaluate((link, title) => {
              link.addEventListener('click', () => {
                const start = performance.now();
                window.fixtureRouteReady = null;
                const ready = () => {
                  if (document.querySelector('.chat__pill-name')?.textContent !== title || !document.querySelector('#message')) return;
                  window.fixtureRouteReady = performance.now() - start;
                  observer.disconnect();
                };
                const observer = new MutationObserver(ready);
                observer.observe(document, { subtree: true, childList: true, characterData: true });
                queueMicrotask(ready);
              }, { once: true });
            }, title);
            await target.click();
            await page.waitForFunction(() => typeof window.fixtureRouteReady === 'number');
            if (n >= 0) warmTimes[client].push(await page.evaluate(() => window.fixtureRouteReady));
          }
        }
      } finally { await Promise.all(Object.values(pages).map(page => page.close())); }
      for (const client of clients) navigation[client] = stats(warmTimes[client]);
    }
    console.log('Cold conversation readiness (ms)', JSON.stringify(cold));
    console.log('Warm conversation navigation (ms)', JSON.stringify(navigation));
    if (samples >= 20) {
      assert(cold.leptos.p95 <= cold.react.p95 * 1.2, 'Cold readiness exceeds the 20% budget.');
      assert(navigation.leptos.p95 <= navigation.react.p95 * 1.1, 'Warm navigation exceeds the 10% budget.');
    }
    assert.deepEqual(faults, []);
    console.log(`PASS route selection, controls, API payloads and CSP. ${evidence.length} screenshot pairs.`);
  }
} finally {
  if (browser) await writeFile(join(output, 'receipt.json'), JSON.stringify({ browser: browser.version(), evidence, faults, cold, navigation }, null, 2));
  await browser?.close();
  await app?.close();
  provider.closeAllConnections();
  if (provider.listening) await new Promise(resolve => provider.close(resolve));
  await rm(work, { recursive: true });
}
