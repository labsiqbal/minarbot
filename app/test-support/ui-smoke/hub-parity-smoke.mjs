import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { once } from 'node:events';
import { request } from 'node:http';
import { createServer } from 'node:https';
import { mkdtemp, readFile, rm, mkdir } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { tmpdir } from 'node:os';
import { DatabaseSync } from 'node:sqlite';
import { chromium } from 'playwright-core';
import { createHubServer } from '../../hub/server.ts';
import { createHubAuthority } from '../../hub/project-authority.ts';
import { initializeHubSchema } from '../../hub/schema.ts';
import { createStubVerifier } from '../../hub/sessions.ts';

const domain = 'hub-fixture.test';
const central = 'accounts.' + domain;
const scratch = await mkdtemp(join(tmpdir(), 'minarbot-hub-ui-'));
const screenshots = process.env.MINARBOT_HUB_SCREENSHOTS;
const db = new DatabaseSync(':memory:');
initializeHubSchema(db);
const authority = createHubAuthority(db, { domain, mailboxKey: Buffer.alloc(32, 3) });
const identities = new Map(['Alice', 'Bob'].map(subject => [subject.toLowerCase(), {
  issuer: 'https://identity.test', subject, verifiedMailboxes: [subject.toLowerCase() + '@example.test'],
  sessionId: subject + '-fixture', expiresAt: Date.now() + 600000,
}]));
const account = authority.provisionAccount(identities.get('alice'), true);
authority.provisionAccount(identities.get('bob'), true);
const project = authority.createProject(account, 'pilot', 'Pilot project');
if (process.env.MINARBOT_HUB_SMOKE_CLIENT !== 'react') process.env.MINARBOT_APPEARANCE_CLIENT = 'leptos';
const hub = createHubServer({
  authority, accountsHost: central, domain, verifier: createStubVerifier(identities),
  web: { distPath: resolve(import.meta.dirname, '../../dist/hub'), developmentIdentity: true },
});
let proxy, browser, dropMessageResponse = false;

async function listen(server) {
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  return server.address().port;
}

try {
  const port = await listen(hub.server);
  execFileSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1', '-subj', '/CN=' + central,
    '-addext', 'subjectAltName=DNS:*.' + domain, '-keyout', join(scratch, 'tls.key'), '-out', join(scratch, 'tls.crt')], { stdio: 'ignore' });
  proxy = createServer({ key: await readFile(join(scratch, 'tls.key')), cert: await readFile(join(scratch, 'tls.crt')) }, (incoming, response) => {
    const connection = request({ host: '127.0.0.1', port, path: incoming.url, method: incoming.method, headers: incoming.headers });
    incoming.pipe(connection);
    connection.on('response', upstream => {
      if (dropMessageResponse && incoming.url === '/api/messages' && upstream.statusCode === 200) {
        dropMessageResponse = false;
        upstream.resume();
        response.writeHead(503, { 'Content-Type': 'application/json' });
        response.end(JSON.stringify({ error: { code: 'fixture_lost_response', message: 'Success response lost in fixture.' } }));
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
  for (const client of process.env.MINARBOT_HUB_SMOKE_CLIENT ? [process.env.MINARBOT_HUB_SMOKE_CLIENT] : ['react','leptos']) {
    const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1280, height: 860 } });
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
      const text = 'Hub message from ' + client;
      await message.fill(text);
      await page.reload();
      await message.waitFor();
      assert.equal(await message.inputValue(), text);
      dropMessageResponse = true;
      await message.press('Enter');
      await page.getByText('Message not confirmed', { exact: true }).waitFor();
      await page.getByRole('button', { name: 'Retry message', exact: true }).first().click();
      await page.locator('.human-bubble').getByText(text, { exact: true }).waitFor();
      await page.waitForFunction(() => document.querySelector('textarea[placeholder="Message general"]')?.value === '');
      assert.equal(await page.getByText(text, { exact: true }).count(), 1);
      if (screenshots) {
        assert.equal(await page.locator('html').getAttribute('data-theme'), 'dark');
        await page.screenshot({ path: join(screenshots, client + '-room-dark.png') });
        await page.evaluate(() => localStorage.setItem('minarbot-presentation-v1', JSON.stringify({
          version: 1, theme: 'light', textSize: 'standard', colorTheme: 'minarbot',
        })));
        await page.reload();
        await page.getByText(text, { exact: true }).waitFor();
        assert.equal(await page.locator('html').getAttribute('data-theme'), 'light');
        await page.screenshot({ path: join(screenshots, client + '-room-light.png') });
      }
      await page.getByRole('link', { name: 'Members', exact: true }).click();
      await page.getByRole('button', { name: 'Invite member', exact: true }).click();
      const invite = page.getByRole('dialog', { name: 'Invite member' });
      await invite.getByLabel('Email', { exact: true }).fill('bob@example.test');
      await invite.getByRole('button', { name: 'Create invitation' }).click();
      await page.getByRole('heading', { name: 'Invitation ready' }).waitFor();
      await page.getByLabel('Private invitation link', { exact: true }).waitFor();
      assert.equal(errors.length, 0, errors.join('\n'));
      console.log(client + ': HTTPS handoff, durable draft, exact replay after response loss, room SSE, invitation form, dark/light');
    } finally {
      await context.close();
    }
  }
} finally {
  await browser?.close();
  if (proxy) {
    proxy.closeAllConnections();
    await new Promise(resolve => proxy.close(resolve));
  }
  await hub.close();
  db.close();
  await rm(scratch, { recursive: true });
}
