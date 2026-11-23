// Run from desktop/: xvfb-run -a node --experimental-strip-types scripts/smoke.mjs
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { startApp } from '../../app/test-bridge.ts';

const cache = path.resolve('.cache');
fs.mkdirSync(cache, { recursive: true });
const scratch = fs.mkdtempSync(path.join(cache, 'smoke-'));
const screenshotDir = path.resolve('.cache/verification');
fs.mkdirSync(screenshotDir, { recursive: true });
const appImage = path.resolve('dist', fs.readdirSync('dist').find((name) => name.endsWith('.AppImage')));
const privateOrigin = 'https://desktop-check.invalid:4317';
console.log('Starting isolated Minarbot server');
const server = await startApp({ database: path.join(scratch, 'minarbot.sqlite'), port: 0, allowedOrigins: [privateOrigin] });
let child;
let socket;
let closeBrowser = () => {};
const scratchFd = fs.openSync(scratch, 'r');
const shortScratch = `/proc/${process.pid}/fd/${scratchFd}`;
const timeout = setTimeout(() => { console.error('Smoke timed out'); if (child) process.kill(-child.pid, 'SIGTERM'); }, 90000);
function headersCheck(headers, method = 'GET') {
  return new Promise((resolve, reject) => {
    const request = http.request(`${server.origin}/api/agents`, { method, headers: { ...headers, ...(method === 'POST' ? { 'content-type': 'application/json' } : {}) } }, (response) => {
      response.resume(); response.on('end', () => resolve(response.statusCode));
    });
    request.on('error', reject); request.end(method === 'POST' ? '{}' : undefined);
  });
}
async function waitFor(callback, limit = 15000) {
  const start = Date.now();
  while (Date.now() - start < limit) {
    const value = await callback(); if (value) return value;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error('Timed out waiting for renderer');
}
try {
  assert.equal(await headersCheck({ host: new URL(privateOrigin).host, origin: privateOrigin }), 200);
  assert.equal(await headersCheck({ host: new URL(privateOrigin).host, origin: privateOrigin }, 'POST'), 400); // admission passed; empty payload rejected
  assert.equal(await headersCheck({ host: 'evil.invalid' }), 403);
  assert.equal(await headersCheck({ origin: 'https://evil.invalid' }, 'POST'), 403);
  console.log('Host/Origin admission checks passed');
  const configHome = path.join(scratch, 'config');
  fs.mkdirSync(configHome, { recursive: true });
  console.log('Launching built AppImage');
  const launchOptions = { env: {
    PATH: process.env.PATH, DISPLAY: process.env.DISPLAY, XAUTHORITY: process.env.XAUTHORITY,
    HOME: scratch, XDG_CONFIG_HOME: configHome, XDG_CACHE_HOME: path.join(scratch, 'cache'), XDG_DATA_HOME: path.join(scratch, 'data'),
    TMPDIR: shortScratch, APPIMAGE_EXTRACT_AND_RUN: '1', ELECTRON_ENABLE_LOGGING: '1',
  }, detached: true, stdio: ['ignore', 'pipe', 'pipe'] };
  child = spawn(appImage, ['--remote-debugging-port=0'], launchOptions);
  let output = '';
  const record = (chunk) => { output += chunk.toString(); fs.appendFileSync(path.join(screenshotDir, 'electron.log'), chunk.toString()); };
  child.stdout.on('data', record); child.stderr.on('data', record);
  child.on('error', (error) => { output += error.stack; });
  const browserUrl = await waitFor(() => {
    if (child.exitCode !== null) throw new Error(`AppImage exited ${child.exitCode}\n${output}`);
    return /DevTools listening on ws:\/\/127\.0\.0\.1:(\d+)/.exec(output)?.[1];
  }, 30000);
  const target = await waitFor(async () => (await (await fetch(`http://127.0.0.1:${browserUrl}/json/list`)).json()).find((entry) => entry.type === 'page'));
  socket = new WebSocket(target.webSocketDebuggerUrl); await once(socket, 'open');
  let seq = 0; const pending = new Map();
  closeBrowser = () => { if (socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify({ id: ++seq, method: 'Browser.close' })); };
  socket.addEventListener('message', ({ data }) => { const message = JSON.parse(data); if (message.id && pending.has(message.id)) { pending.get(message.id)(message); pending.delete(message.id); } });
  function send(method, params = {}) { const id = ++seq; return new Promise((resolve) => { pending.set(id, resolve); socket.send(JSON.stringify({ id, method, params })); }); }
  async function evaluate(expression) {
    const response = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true, userGesture: true });
    if (response.error || response.result.exceptionDetails) throw new Error(JSON.stringify(response));
    return response.result.result.value;
  }
  async function screenshot(name) {
    const response = await send('Page.captureScreenshot', { format: 'png' });
    fs.writeFileSync(path.join(screenshotDir, name), Buffer.from(response.result.data, 'base64'));
  }
  await waitFor(() => evaluate('document.querySelector("button")?.textContent === "Connect"'));
  assert.deepEqual(await evaluate('Object.keys(window.minarbotDesktop).sort()'), ['appVersion', 'checkForUpdates', 'openExternal']);
  assert.equal(await evaluate('typeof require'), 'undefined');
  await screenshot('connect.png');
  await evaluate('document.querySelector("input").value="http://example.invalid"; document.querySelector("form").requestSubmit()');
  await waitFor(() => evaluate('document.querySelector("#error")?.textContent.includes("Use HTTPS")'));
  await evaluate(`document.querySelector('input').value=${JSON.stringify(server.origin)}; document.querySelector('form').requestSubmit()`);
  await waitFor(() => evaluate(`location.origin===${JSON.stringify(server.origin)} && document.querySelector('#leptos-root')?.childElementCount > 0 && document.querySelector('#appearance-fallback')?.hidden`));
  const result = await evaluate(`(async () => ({ title: document.title, origin: location.origin, bridge: Object.keys(window.minarbotDesktop).sort(), node: typeof require, agentsStatus: (await fetch('/api/agents')).status, visibleText: document.body.innerText.slice(0, 300) }))()`);
  assert.match(result.title, /Minarbot$/); assert.equal(result.agentsStatus, 200); assert.equal(result.node, 'undefined');
  const writeStatus = await evaluate(`fetch('/api/agents', {method:'POST',headers:{'content-type':'application/json'},body:'{}'}).then(response => response.status)`);
  assert.equal(writeStatus, 400); // same-origin write admission reached payload validation
  await screenshot('app.png');
  const second = spawn(appImage, [], { ...launchOptions, stdio: 'ignore' });
  const secondExit = once(second, 'exit');
  const secondLimit = setTimeout(() => { if (second.exitCode === null && second.signalCode === null) process.kill(-second.pid, 'SIGTERM'); }, 15000);
  try { const [code, signal] = await secondExit; assert.equal(code, 0); assert.equal(signal, null); }
  finally { clearTimeout(secondLimit); }
  assert.equal(child.exitCode, null);
  assert.equal(await evaluate('document.hasFocus()'), true);

  const settingsFile = await waitFor(() => {
    const walk = (dir) => fs.readdirSync(dir, { withFileTypes: true }).flatMap((entry) => entry.isDirectory() ? walk(path.join(dir, entry.name)) : entry.name === 'settings.json' ? [path.join(dir, entry.name)] : []);
    return walk(configHome)[0];
  });
  assert.equal(JSON.parse(fs.readFileSync(settingsFile, 'utf8')).activeServer, `${server.origin}/`);
  console.log(JSON.stringify({ appImage, loaded: result, sameOriginWriteStatus: writeStatus, privateOriginAllowed: true, wrongHostRejected: true, wrongOriginRejected: true, settingsPersisted: true, secondInstanceFocused: true, screenshots: ['connect.png', 'app.png'], sandboxDisabled: false }, null, 2));
  fs.writeFileSync(path.join(screenshotDir, 'electron.log'), output);
} finally {
  clearTimeout(timeout); closeBrowser();
  if (child && child.exitCode === null && child.signalCode === null) await Promise.race([once(child, 'exit'), new Promise((resolve) => setTimeout(resolve, 5000))]);
  socket?.close();
  if (child && child.exitCode === null && child.signalCode === null) { const exited = once(child, 'exit'); process.kill(-child.pid, 'SIGTERM'); await Promise.race([exited, new Promise((resolve) => setTimeout(resolve, 5000))]); if (child.exitCode === null && child.signalCode === null) { process.kill(-child.pid, 'SIGKILL'); await exited; } }
  if (child) { try { process.kill(-child.pid, 'SIGTERM'); } catch (error) { if (error.code !== 'ESRCH') throw error; } }
  await server.close();
  fs.closeSync(scratchFd);
  await fs.promises.rm(scratch, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  console.log('Cleanup complete: AppImage and isolated Minarbot server stopped.');
}
