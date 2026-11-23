// From repo root: xvfb-run -a node --experimental-strip-types app/test-support/ui-smoke/copy-paste-smoke.mjs
import assert from 'node:assert/strict';
import { chromium, _electron as electron } from 'playwright-core';
import { mkdtemp, rm, writeFile, mkdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { startApp } from '../../test-bridge.ts';

const scratch = await mkdtemp(join(tmpdir(), 'minarbot-cp1-'));
const baseline = process.env.CP1_BASELINE === '1';
const receipt = { baseline, checks: [], errors: [] };
const output = resolve('app/test-support/ui-smoke/artifacts/copy-paste'); await mkdir(output, { recursive: true });
let browser, desktop, app;
const reply = 'Clipboard fixture **ready**.\n\n```js\nconst answer = 42;\n```';
const provider = createServer(async (req, res) => {
  for await (const chunk of req) { /* Drain synthetic request. */ }
  res.setHeader('Content-Type', 'application/json');
  res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: reply } }] }));
});
async function check(name, run) {
  try { await run(); receipt.checks.push({ name, status: 'pass' }); console.log('PASS', name); }
  catch (error) { receipt.checks.push({ name, status: 'fail', error: error.message }); console.log('FAIL', name, error.message); }
}
try {
  await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
  app = await startApp({ appearanceClient: 'leptos', database: join(scratch, 'app.sqlite'), port: Number(process.env.CP1_PORT ?? 0), provider: 'openai-compatible', baseURL: `http://127.0.0.1:${provider.address().port}/v1`, apiKey: 'synthetic', defaultModel: 'fixture' });
  console.log('Disposable server', app.origin);
  const agent = await fetch(app.origin + '/api/agents', { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify({ runtime: 'api', id: randomUUID(), name: 'Clipboard fixture', instructions: 'Synthetic clipboard test.', model: 'fixture' }) }).then(r => r.json());
  assert(agent.id, JSON.stringify(agent));
  browser = await chromium.launch({ channel: 'chrome', headless: true });
  const context = await browser.newContext({ permissions: ['clipboard-read', 'clipboard-write'], viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  async function open(page) {
    page.on('pageerror', error => receipt.errors.push(error.message));
    page.setDefaultTimeout(4000);
    await page.goto(app.origin);
    await page.getByRole('tab', { name: /^Agents/ }).click();
    await page.getByRole('navigation', { name: 'Choose agent', exact: true }).getByRole('button', { name: /^Clipboard fixture / }).click();
    await page.getByRole('textbox', { name: 'Message Clipboard fixture', exact: true }).waitFor();
  }
  await open(page);
  await page.getByRole('textbox', { name: 'Message Clipboard fixture', exact: true }).fill('Copy this reply');
  await page.getByRole('textbox', { name: 'Message Clipboard fixture', exact: true }).press('Enter');
  await page.locator('.markdown pre code').waitFor();
  const copyReply = async page => {
    await page.waitForFunction(() => ![...document.querySelectorAll('[role="status"]')].some(el => el.textContent === 'Copied'));
    await page.getByRole('button', { name: 'Copy reply', exact: true }).click();
    await page.waitForFunction(() => document.querySelector('[role="status"]')?.textContent === 'Copied' || [...document.querySelectorAll('[role="status"]')].some(el => el.textContent === 'Copied'));
  };
  await check('web reply copies original Markdown', async () => { await copyReply(page); assert.equal(await page.evaluate(() => navigator.clipboard.readText()), reply); });
  await check('web code block copy', async () => { assert.equal(await page.getByRole('button', { name: 'Copy code', exact: true }).count(), 1); await page.getByRole('button', { name: 'Copy code', exact: true }).click(); assert.equal(await page.evaluate(() => navigator.clipboard.readText()), 'const answer = 42;'); });
  await check('web missing Clipboard API fallback', async () => {
    await page.evaluate(() => { window.__clipboard = navigator.clipboard; Object.defineProperty(navigator, 'clipboard', { configurable: true, value: undefined }); });
    try { await copyReply(page); } finally { await page.evaluate(() => { delete navigator.clipboard; }); }
    assert.equal(await page.evaluate(() => navigator.clipboard.readText()), reply);
  });
  await check('web denied Clipboard API fallback', async () => {
    await page.evaluate(() => { window.__write = navigator.clipboard.writeText; navigator.clipboard.writeText = async () => { throw new DOMException('Denied', 'NotAllowedError'); }; });
    try { await copyReply(page); } finally { await page.evaluate(() => { navigator.clipboard.writeText = window.__write; delete window.__write; }); }
    assert.equal(await page.evaluate(() => navigator.clipboard.readText()), reply);
  });
  await check('web copy failure feedback', async () => {
    await page.waitForFunction(() => ![...document.querySelectorAll('[role="status"]')].some(el => el.textContent === 'Copied'));
    await page.evaluate(() => { window.__write = navigator.clipboard.writeText; window.__exec = document.execCommand; navigator.clipboard.writeText = async () => { throw new Error('denied'); }; document.execCommand = () => false; });
    try { await page.getByRole('button', { name: 'Copy reply', exact: true }).click(); await page.getByRole('alert').filter({ hasText: 'Text could not be copied' }).waitFor(); }
    finally { await page.evaluate(() => { navigator.clipboard.writeText = window.__write; document.execCommand = window.__exec; delete window.__write; delete window.__exec; }); }
    await copyReply(page);
  });
  await check('web transcript selection and keyboard Copy', async () => {
    const text = await page.locator('.markdown p').first().evaluate(el => { const range = document.createRange(); range.selectNodeContents(el); const selection = getSelection(); selection.removeAllRanges(); selection.addRange(range); return selection.toString(); });
    await page.keyboard.press('Control+c'); assert.equal(await page.evaluate(() => navigator.clipboard.readText()), text);
  });
  for (const [name, types] of [['plain', ['text/plain']], ['rich', ['text/plain', 'text/html']]]) await check(`web composer ${name} text paste`, async () => {
    const input = page.getByRole('textbox', { name: 'Message Clipboard fixture', exact: true }); await input.fill(''); await input.focus();
    await page.evaluate(types => navigator.clipboard.write([new ClipboardItem(Object.fromEntries(types.map(type => [type, new Blob([type === 'text/html' ? '<b>pasted text</b>' : 'pasted text'], { type })])))]), types);
    await input.press('Control+v'); assert.equal(await input.inputValue(), 'pasted text');
  });
  await check('web composer pasted image attaches', async () => {
    const input = page.getByRole('textbox', { name: 'Message Clipboard fixture', exact: true }); await input.fill('Keep draft'); await input.focus();
    await page.evaluate(async () => { const canvas = document.createElement('canvas'); canvas.width = canvas.height = 8; const blob = await new Promise(resolve => canvas.toBlob(resolve)); await navigator.clipboard.write([new ClipboardItem({ 'image/png': blob })]); });
    await input.press('Control+v');
    await page.locator('.composer-area .attachment__image').waitFor();
    assert.equal(await input.inputValue(), 'Keep draft');
  });
  await check('web mixed text/files paste keeps text default and attaches files', async () => {
    const input = page.getByRole('textbox', { name: 'Message Clipboard fixture', exact: true });
    const allowed = await input.evaluate(el => { const data = new DataTransfer(); data.items.add(new File(['first'], 'first.txt', { type: 'text/plain' })); data.items.add(new File(['second'], 'second.txt', { type: 'text/plain' })); data.setData('text/plain', 'mixed text'); return el.dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true })); });
    assert.equal(allowed, true, 'Text paste default was swallowed');
    await page.waitForFunction(() => document.querySelectorAll('.composer-area .attachment').length === 3, null, { timeout: 4000 });
    assert.equal(await input.inputValue(), 'Keep draft');
  });
  await check('web pasted upload recovers lost response using same ID', async () => {
    const uploads = []; let loseReply = true;
    const pattern = '**/api/agents/*/files/*';
    await page.route(pattern, async route => {
      if (route.request().method() !== 'POST') return route.continue();
      uploads.push(new URL(route.request().url()).pathname);
      const response = await route.fetch();
      if (loseReply) { loseReply = false; return route.abort('failed'); }
      return route.fulfill({ response });
    });
    try {
      const input = page.getByRole('textbox', { name: 'Message Clipboard fixture', exact: true });
      await input.evaluate(el => { const data = new DataTransfer(); data.items.add(new File(['recover'], 'recover.txt', { type: 'text/plain' })); el.dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true })); });
      await page.getByRole('button', { name: 'Retry upload', exact: true }).waitFor();
      await page.getByRole('button', { name: 'Retry upload', exact: true }).click();
      await page.waitForFunction(() => document.querySelectorAll('.composer-area .attachment').length === 4);
      assert.equal(uploads.length, 2); assert.equal(uploads[0], uploads[1]);
      assert.equal(await input.inputValue(), 'Keep draft');
    } finally { await page.unroute(pattern); }
  });
  await check('web pasted upload enforces attachment limit', async () => {
    const input = page.getByRole('textbox', { name: 'Message Clipboard fixture', exact: true });
    await input.evaluate(el => { const data = new DataTransfer(); data.items.add(new File(['extra'], 'extra.txt', { type: 'text/plain' })); el.dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true })); });
    await page.getByRole('alert').filter({ hasText: 'Up to four attachments per message.' }).waitFor();
    assert.equal(await input.inputValue(), 'Keep draft');
  });
  desktop = await electron.launch({ executablePath: resolve('desktop/node_modules/electron/dist/electron'), args: [resolve('desktop'), `--user-data-dir=${join(scratch, 'electron')}`], env: { ...process.env, MINARBOT_SERVER_URL: app.origin } });
  const desktopPage = await desktop.firstWindow(); await desktopPage.waitForURL(app.origin + '/'); await open(desktopPage);
  assert.deepEqual(await desktopPage.evaluate(() => Object.keys(window.minarbotDesktop).sort()), ['appVersion', 'checkForUpdates', 'openExternal']);
  await check('desktop clipboard API write/read', async () => {
    await desktopPage.bringToFront();
    assert.equal(await desktopPage.evaluate(async () => { try { await navigator.clipboard.writeText('desktop fixture'); return await navigator.clipboard.readText(); } catch (e) { return e.name + ': ' + e.message; } }), 'desktop fixture');
  });
  await check('desktop denies clipboard in same-origin subframe', async () => {
    const framePromise = desktopPage.waitForEvent('framenavigated', frame => frame !== desktopPage.mainFrame());
    await desktopPage.evaluate(origin => { const frame = document.createElement('iframe'); frame.srcdoc = '<title>Clipboard subframe</title>'; document.body.append(frame); }, app.origin);
    const frame = await framePromise;
    assert.equal(await frame.evaluate(async () => { try { await navigator.clipboard.writeText('untrusted subframe'); return 'allowed'; } catch (e) { return e.name; } }), 'NotAllowedError');
    await desktopPage.evaluate(() => document.querySelector('iframe')?.remove());
  });
  await check('desktop denies clipboard in another WebContents', async () => {
    const result = await desktop.evaluate(async ({ BrowserWindow }, origin) => {
      const window = new BrowserWindow({ show: false, webPreferences: { sandbox: true, contextIsolation: true, nodeIntegration: false } });
      try { await window.loadURL(origin); return await window.webContents.executeJavaScript("navigator.clipboard.writeText('untrusted window').then(() => 'allowed', error => error.name)", true); }
      finally { window.destroy(); }
    }, app.origin);
    assert.equal(result, 'NotAllowedError');
    await desktopPage.bringToFront();
  });
  await check('desktop reply copy', async () => { await copyReply(desktopPage); assert.equal(await desktop.evaluate(({ clipboard }) => clipboard.readText()), reply); });
  await check('desktop composer text paste', async () => {
    const input = desktopPage.getByRole('textbox', { name: 'Message Clipboard fixture', exact: true });
    await input.fill(''); await desktop.evaluate(({ clipboard }) => clipboard.writeText('desktop pasted text')); await input.press('Control+v'); assert.equal(await input.inputValue(), 'desktop pasted text');
  });
  await check('desktop composer image paste', async () => {
    await desktopPage.evaluate(async () => { const canvas = document.createElement('canvas'); canvas.width = canvas.height = 8; const blob = await new Promise(resolve => canvas.toBlob(resolve)); await navigator.clipboard.write([new ClipboardItem({ 'image/png': blob })]); });
    const input = desktopPage.getByRole('textbox', { name: 'Message Clipboard fixture', exact: true });
    await input.press('Control+v'); await desktopPage.locator('.composer-area .attachment__image').waitFor();
    assert.equal(await input.inputValue(), 'desktop pasted text');
  });
  await check('desktop native context menu',  async () => {
    await desktop.evaluate(({ BrowserWindow, Menu }) => { globalThis.__menus = []; const popup = Menu.prototype.popup; Menu.prototype.popup = function(options) { globalThis.__menus.push(this); }; globalThis.__restorePopup = () => { Menu.prototype.popup = popup; }; });
    await desktopPage.getByRole('textbox', { name: 'Message Clipboard fixture', exact: true }).click({ button: 'right' });
    const menus = await desktop.evaluate(() => globalThis.__menus.map(menu => menu.items.map(item => item.role ?? item.label)));
    assert(menus.some(items => ['cut', 'copy', 'paste', 'selectall'].every(role => items.includes(role))), JSON.stringify(menus));
    const input = desktopPage.getByRole('textbox', { name: 'Message Clipboard fixture', exact: true });
    const action = role => desktop.evaluate(({ BrowserWindow }, role) => { const menu = globalThis.__menus.at(-1); const item = menu.items.find(item => item.role === role); if (!item?.enabled) throw new Error('Menu role unavailable: ' + role); item.click(undefined, BrowserWindow.getAllWindows()[0], BrowserWindow.getAllWindows()[0].webContents); }, role);
    await input.fill('native menu text'); await input.press('Control+a'); await input.click({ button: 'right' });
    await action('copy'); assert.equal(await desktop.evaluate(({ clipboard }) => clipboard.readText()), 'native menu text');
    await action('cut'); assert.equal(await input.inputValue(), '');
    await desktop.evaluate(({ clipboard }) => clipboard.writeText('menu paste'));
    await input.click({ button: 'right' }); await action('paste'); assert.equal(await input.inputValue(), 'menu paste');
    await input.click({ button: 'right' }); await action('selectall'); assert.equal(await input.evaluate(el => el.selectionEnd - el.selectionStart), 'menu paste'.length);
    await desktopPage.evaluate(() => { const link = document.createElement('a'); link.id = 'cp1-link'; link.href = 'https://example.test/copy'; link.textContent = 'Test link'; document.querySelector('.composer-area').prepend(link); });
    await desktopPage.locator('#cp1-link').click({ button: 'right' });
    await desktop.evaluate(() => globalThis.__menus.at(-1).items.find(item => item.label === 'Copy Link').click());
    assert.equal(await desktop.evaluate(({ clipboard }) => clipboard.readText()), 'https://example.test/copy');
    await desktopPage.locator('#cp1-link').evaluate(el => el.remove());
    await desktopPage.evaluate(async () => { const canvas = document.createElement('canvas'); canvas.width = canvas.height = 8; const image = new Image(); image.id = 'cp1-image'; image.src = canvas.toDataURL(); await image.decode(); document.querySelector('.composer-area').prepend(image); });
    await desktopPage.locator('#cp1-image').click({ button: 'right' });
    await desktop.evaluate(() => globalThis.__menus.at(-1).items.find(item => item.label === 'Copy Image').click());
    await desktopPage.waitForTimeout(100);
    assert.equal(await desktop.evaluate(({ clipboard }) => clipboard.has('image/png')), true);
    await desktopPage.locator('#cp1-image').evaluate(el => el.remove());
    await desktop.evaluate(() => globalThis.__restorePopup());
  });
  await page.screenshot({ path: join(output, 'web.png') });
  await desktopPage.screenshot({ path: join(output, 'desktop.png') });
} finally {
  await desktop?.close(); await browser?.close(); await app?.close(); await new Promise(resolve => provider.close(resolve));
  await writeFile(join(output, baseline ? 'before.json' : 'after.json'), JSON.stringify(receipt, null, 2) + '\n');
  await rm(scratch, { recursive: true, force: true });
}
assert(baseline || receipt.checks.every(item => item.status === 'pass'), 'Clipboard smoke failed');
assert.equal(receipt.errors.length, 0, JSON.stringify(receipt.errors));
