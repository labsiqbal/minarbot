import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { once } from 'node:events';
import { request } from 'node:http';
import { createServer } from 'node:https';
import { copyFile, mkdtemp, readFile, rm, mkdir, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { tmpdir } from 'node:os';
import { DatabaseSync } from 'node:sqlite';
import { chromium } from 'playwright-core';
import { createHubServer } from '../../hub/server.ts';
import { createHubAuthority } from '../../hub/project-authority.ts';
import { initializeHubSchema } from '../../hub/schema.ts';
import { createStubVerifier } from '../../hub/sessions.ts';
import { startRustHub } from '../hosted-hub.ts';
import { routeReadiness } from './route-readiness.mjs';
import { fixtureClock, parityShot } from './parity-shot.mjs';

const domain = 'hub-fixture.test';
const central = 'accounts.' + domain;
const scratch = await mkdtemp(join(tmpdir(), 'minarbot-hub-ui-'));
const screenshots = process.env.MINARBOT_HUB_SCREENSHOTS;
const samples = Number(process.env.MINARBOT_HUB_BENCH ?? 0);
assert(Number.isSafeInteger(samples) && samples >= 0);
const rustBinary = process.env.MINARBOT_HOSTED_HUB_BINARY;
const seedPath = join(scratch, 'seed.sqlite');
const db = new DatabaseSync(seedPath);
initializeHubSchema(db);
const authority = createHubAuthority(db, { domain, mailboxKey: Buffer.alloc(32, 3) });
const identities = new Map(['Alice', 'Bob'].map(subject => [subject.toLowerCase(), {
  issuer: 'https://identity.test', subject, verifiedMailboxes: [subject.toLowerCase() + '@example.test'],
  sessionId: subject + '-fixture', expiresAt: Date.now() + 600000,
}]));
const account = authority.provisionAccount(identities.get('alice'), true);
authority.provisionAccount(identities.get('bob'), true);
const fixtureProject = authority.createProject(account, 'workspace-fixture', 'Pilot project');
db.close();
const web = { distPath: resolve(import.meta.dirname, '../../dist/hub'), developmentIdentity: true };
let rust, node, activeDatabase, port;
let proxy, browser, dropMessageResponse = false;
const inviteFixture = { invitationId: '10000000-0000-4000-8000-000000000082', token: 'synthetic-invitation-fixture' };

async function listen(server) {
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  return server.address().port;
}

try {
  const tokensPath = join(scratch, 'tokens.json');
  const mailboxKeyPath = join(scratch, 'mailbox.key');
  if (rustBinary) {
    await writeFile(mailboxKeyPath, Buffer.alloc(32, 3), { mode: 0o600 });
    await writeFile(tokensPath, JSON.stringify(Object.fromEntries(identities)), { mode: 0o600 });
  }
  execFileSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1', '-subj', '/CN=' + central,
    '-addext', 'subjectAltName=DNS:*.' + domain, '-keyout', join(scratch, 'tls.key'), '-out', join(scratch, 'tls.crt')], { stdio: 'ignore' });
  proxy = createServer({ key: await readFile(join(scratch, 'tls.key')), cert: await readFile(join(scratch, 'tls.crt')) }, (incoming, response) => {
    const connection = request({ host: '127.0.0.1', port, path: incoming.url, method: incoming.method, headers: incoming.headers });
    incoming.pipe(connection);
    connection.on('response', async upstream => {
      if (dropMessageResponse && incoming.url === '/api/messages' && upstream.statusCode === 200) {
        dropMessageResponse = false;
        upstream.resume();
        response.writeHead(503, { 'Content-Type': 'application/json' });
        response.end(JSON.stringify({ error: { code: 'fixture_lost_response', message: 'Success response lost in fixture.' } }));
        return;
      }
      if (screenshots && incoming.url === '/api/invitations' && upstream.statusCode === 200) {
        let body = '';
        for await (const chunk of upstream) body += chunk;
        const json = JSON.parse(body);
        const url = `https://${fixtureProject.host}/invite#${new URLSearchParams(inviteFixture)}`;
        const fixture = incoming.method === 'POST' ? { ...json, url } : json.map(row => ({
          ...row, id: inviteFixture.invitationId, expiresAt: fixtureClock + 86_400_000,
        }));
        const headers = { ...upstream.headers };
        delete headers['content-length'];
        response.writeHead(200, headers);
        response.end(JSON.stringify(fixture));
        return;
      }
      response.writeHead(upstream.statusCode, upstream.headers);
      upstream.pipe(response);
    });
    connection.on('error', () => response.destroy());
    response.on('close', () => connection.destroy());
  });
  const httpsPort = await listen(proxy);
  browser = await chromium.launch({
    executablePath: process.env.CHROME_BIN ?? '/usr/bin/google-chrome',
    args: ['--no-proxy-server', '--host-resolver-rules=MAP *.' + domain + ' 127.0.0.1:' + httpsPort],
  });
  if (screenshots) await mkdir(screenshots, { recursive: true });
  const timings = {};
  for (const client of process.env.MINARBOT_HUB_SMOKE_CLIENT ? [process.env.MINARBOT_HUB_SMOKE_CLIENT] : ['react','leptos']) {
    process.env.MINARBOT_APPEARANCE_CLIENT = client;
    const dataPath = join(scratch, client + '.sqlite');
    await copyFile(seedPath, dataPath);
    if (rustBinary) {
      rust = await startRustHub(rustBinary, scratch, { domain, accountsHost: central, dataPath,
        mailboxKeyPath, identity: { mode: 'stub', tokensPath }, web });
      port = rust.port;
    } else {
      activeDatabase = new DatabaseSync(dataPath);
      node = createHubServer({ authority: createHubAuthority(activeDatabase, { domain, mailboxKey: Buffer.alloc(32, 3) }),
        accountsHost: central, domain, verifier: createStubVerifier(identities), web });
      port = await listen(node.server);
    }
    const project = fixtureProject;
    const viewport = { width: 1280, height: 860 };
    const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport, reducedMotion: 'reduce', timezoneId: 'UTC', locale: 'en-US' });
    if (client === 'react') {
      await context.route(/^https:\/\/[^/]+\/(?:login|invite|join|nodes\/link)?(?:\?.*)?$/, async route => {
        if (!route.request().isNavigationRequest()) return route.continue();
        const url = new URL(route.request().url());
        if (url.searchParams.get('ui') === 'react') return route.continue();
        url.searchParams.set('ui', 'react');
        await route.continue({ url: url.href });
      });
    }
    const page = await context.newPage();
    async function capture(label) {
      if (!screenshots) return;
      for (const [layout, size] of Object.entries({ desktop: viewport, narrow: { width: 390, height: 844 } })) {
        await page.setViewportSize(size);
        for (const theme of ['dark', 'light']) {
          await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
          await parityShot(page, join(screenshots, `${client}-${label}-${layout}-${theme}.png`));
        }
      }
      await page.setViewportSize(viewport);
    }
    const errors = [];
    const wasm = [];
    page.on('response', response => {
      const path = new URL(response.url()).pathname;
      if (path.endsWith('.wasm')) wasm.push(response.headers());
      if (path.startsWith('/_leptos/assets/') && response.status() >= 400) errors.push('Unavailable Leptos asset ' + path);
    });
    page.on('pageerror', error => errors.push(error.message));
    page.on('console', message => {
      if (/violates.*content security policy|panicked at|unreachable/i.test(message.text())) errors.push(message.text());
    });
    try {
      await page.addInitScript(() => {
        if (!localStorage.getItem('minarbot-presentation-v1')) {
          localStorage.setItem('minarbot-presentation-v1', JSON.stringify({
            version: 1, theme: 'dark', textSize: 'standard', colorTheme: 'minarbot',
          }));
        }
      });
      await page.goto('https://' + project.host + '/');
      await page.getByRole('button', { name: 'Sign in', exact: true }).click();
      await page.getByLabel('Development identity token').waitFor();
      await capture('sign-in');
      await page.getByLabel('Development identity token').fill('alice');
      await page.getByRole('button', { name: 'Sign in', exact: true }).click();
      const message = page.getByRole('textbox', { name: 'Message general' });
      await message.waitFor();
      assert.equal(await page.locator('#leptos-root').count(), client === 'leptos' ? 1 : 0);
      if (client === 'leptos') {
        assert(wasm.length > 0);
        assert(wasm.every(headers => headers['content-type'] === 'application/wasm' && headers['content-encoding'] === 'gzip'));
      } else assert.equal(wasm.length, 0);
      await page.getByText('Live', { exact: true }).waitFor();
      const text = 'Hub message fixture';
      await message.fill(text);
      await page.reload();
      await message.waitFor();
      assert.equal(await message.inputValue(), text);
      dropMessageResponse = true;
      await message.press('Enter');
      await page.getByText('Message not confirmed', { exact: true }).waitFor();
      await page.reload();
      await page.getByText('Message not confirmed', { exact: true }).waitFor();
      await page.getByRole('button', { name: 'Retry message', exact: true }).first().click();
      await page.locator('.human-bubble').getByText(text, { exact: true }).waitFor();
      await page.waitForFunction(() => document.querySelector('textarea[placeholder="Message general"]')?.value === '');
      assert.equal(await page.getByText(text, { exact: true }).count(), 1);
      if (screenshots) {
        assert.equal(await page.locator('html').getAttribute('data-theme'), 'dark');
        await parityShot(page, join(screenshots, client + '-room-dark.png'));
        await page.evaluate(() => localStorage.setItem('minarbot-presentation-v1', JSON.stringify({
          version: 1, theme: 'light', textSize: 'standard', colorTheme: 'minarbot',
        })));
        await page.reload();
        await page.getByText(text, { exact: true }).waitFor();
        assert.equal(await page.locator('html').getAttribute('data-theme'), 'light');
        await parityShot(page, join(screenshots, client + '-room-light.png'));
        await capture('room');
      }
      if (samples) {
        timings[client] = await routeReadiness(browser, {
          ignoreHTTPSErrors: true, viewport, storageState: await context.storageState(),
        }, `https://${project.host}/?ui=${client}`, 'textarea[placeholder="Message general"]', samples);
      }
      await page.getByRole('link', { name: 'Members', exact: true }).click();
      await page.getByRole('button', { name: 'Invite member', exact: true }).waitFor();
      await page.getByText('No invitations', { exact: true }).waitFor();
      await capture('members');
      await page.getByRole('button', { name: 'Invite member', exact: true }).click();
      const invite = page.getByRole('dialog', { name: 'Invite member' });
      await invite.waitFor();
      await capture('invite');
      await invite.getByLabel('Email', { exact: true }).fill('bob@example.test');
      await invite.getByRole('button', { name: 'Create invitation' }).click();
      await page.getByRole('heading', { name: 'Invitation ready' }).waitFor();
      await page.getByLabel('Private invitation link', { exact: true }).waitFor();
      await page.getByText(/^Invitation [a-f0-9]{8}$/).waitFor();
      await capture('invitation');
      await page.goto('https://' + central + '/');
      await page.getByRole('button', { name: 'Create project', exact: true }).click();
      const create = page.getByRole('dialog', { name: 'Create project' });
      await create.waitFor();
      await capture('create-project');
      await create.getByLabel('Project name', { exact: true }).fill('New project ' + client);
      await create.getByLabel('Project address', { exact: true }).fill('pilot-' + client);
      await create.getByRole('button', { name: 'Create project', exact: true }).click();
      await page.waitForURL(url => url.hostname === 'pilot-' + client + '.' + domain);
      await page.getByRole('button', { name: 'Sign in', exact: true }).click();
      await page.getByRole('button', { name: 'Continue', exact: true }).click();
      await page.getByRole('heading', { name: 'New project ' + client, exact: true }).waitFor();
      assert.equal(errors.length, 0, errors.join('\n'));
      console.log(client + ': HTTPS handoff, durable draft, exact replay after response loss, room SSE, invitation and project creation, dark/light');
    } finally {
      await context.close();
      await node?.close();
      await rust?.close();
      activeDatabase?.close();
      node = rust = activeDatabase = undefined;
    }
  }
  if (samples) {
    console.log(JSON.stringify({ hub: timings }));
    if (screenshots) await writeFile(join(screenshots, 'timings.json'), JSON.stringify(timings, null, 2) + '\n');
    if (timings.react && timings.leptos) {
      assert(timings.leptos.cold.p95 <= timings.react.cold.p95 * 1.2, 'Hub exceeded the cold p95 budget');
      assert(timings.leptos.warm.p95 <= timings.react.warm.p95 * 1.1, 'Hub exceeded the warm p95 budget');
    }
  }
} finally {
  await browser?.close();
  if (proxy) {
    proxy.closeAllConnections();
    await new Promise(resolve => proxy.close(resolve));
  }
  await node?.close();
  await rust?.close();
  activeDatabase?.close();
  await rm(scratch, { recursive: true });
}
